//! Storage boundary for the local SQLite workspace and the held PostgreSQL pilot.
use super::{ApiResult, internal};
use serde_json::Value;
use sqlx::{PgConnection, PgPool, Row, SqlitePool, postgres::PgPoolOptions};
use std::{
    collections::{HashMap, HashSet},
    io::Write,
    path::{Path, PathBuf},
    time::Duration,
};

#[path="storage_pg_writer.rs"]
mod pg_writer;

const WORKSPACE: &str = "local-pilot";
const LEASE: i64 = 438772116;
#[path="storage_predecessor_recovery.rs"]
mod predecessor_recovery_storage;
pub(crate) use predecessor_recovery_storage::{capture_predecessor,import_predecessor};
#[path = "storage_reads.rs"]
mod reads;
pub(crate) use reads::KnowledgeHeadsQuery;
#[path = "storage_media_gate_context.rs"]
mod media_gate_context;
pub(crate) use media_gate_context::MediaGateReadBudget;
#[path = "storage_source_export.rs"]
mod source_export;
#[path = "storage_writes.rs"]
mod writes;
#[path = "storage_source_snapshot.rs"]
mod source_snapshot;
pub(crate) use source_snapshot::SourceReadIntent;
#[path = "storage_operation_evidence.rs"]
mod operation_evidence;
#[path = "storage_connection_gate.rs"]
mod connection_gate_storage;
pub(crate) use operation_evidence::OperationEvidenceUpdate;
#[path = "storage_operation_outcome.rs"]
mod operation_outcome;
#[path = "storage_readback.rs"]
mod readback_recovery_storage;
#[path = "storage_assistant.rs"]
mod assistant;
#[path = "storage_media.rs"]
mod storage_media;
#[path = "storage_media_analysis_read.rs"]
mod media_analysis_read;
#[path = "storage_media_validation.rs"]
mod media_validation;
pub(crate) use media_validation::{MediaValidationMode,MediaValidationScope};
#[path = "storage_media_status.rs"]
mod media_status;
#[path = "storage_preparation.rs"]
mod preparation;
#[cfg(test)]
pub(crate) use preparation::writer_v51_fixture_db;
#[path = "storage_local_admission.rs"]
mod local_admission;
#[path = "storage_rules.rs"]
mod rules;
#[path = "storage_bounded_review.rs"]
mod bounded_review;
#[path = "storage_operator_batch.rs"]
mod operator_batch;
#[path = "storage_hot_admission.rs"]
mod hot_admission;
pub(crate) use hot_admission::AdmissionScope;
#[path="storage_scope_context.rs"]
mod scope_context;
#[path="storage_proposal_edit.rs"]
mod proposal_edit;
#[path = "storage_conductor_admission.rs"]
mod conductor_admission;
#[path = "storage_conductor_bootstrap.rs"]
mod conductor_bootstrap;
#[path = "storage_runtime_lifecycle.rs"]
mod runtime_lifecycle;
#[path = "storage_bootstrap_ledger.rs"]
mod bootstrap_ledger;
pub(crate) use bootstrap_ledger::read_bootstrap_ledger_snapshot;
#[cfg(test)]
#[path = "storage_fixture_snapshot.rs"]
mod fixture_snapshot;
#[cfg(test)]
pub(crate) use fixture_snapshot::{read_native_fixture_workspace_snapshot,read_native_fixture_backend_cessation};
const TABLES: [&str; 13] = [
    "posts",
    "branches",
    "items",
    "conversations",
    "proposals",
    "approvals",
    "operations",
    "materials",
    "jobs",
    "audit",
    "knowledge_entries",
    "knowledge_versions",
    "feedback",
];

#[derive(Clone)]
pub(super) enum Database {
    Sqlite(SqlitePool),
    Postgres { writer: PgPool, reader: PgPool },
}

impl Database {
    pub(super) async fn postgres(url: &str) -> ApiResult<Self> {
        let writer = PgPoolOptions::new()
            .max_connections(1)
            .min_connections(1)
            .idle_timeout(None)
            .max_lifetime(None)
            // A background snapshot import shares this lease-owning connection.
            // Allow short transaction queues without introducing another writer.
            .acquire_timeout(Duration::from_secs(15))
            .after_connect(|connection, _| {
                Box::pin(async move {
                    let held: bool = sqlx::query_scalar("SELECT pg_try_advisory_lock($1)")
                        .bind(LEASE)
                        .fetch_one(connection)
                        .await?;
                    if !held {
                        return Err(sqlx::Error::Protocol(
                            "PostgreSQL pilot already has a server lease".into(),
                        ));
                    }
                    Ok(())
                })
            })
            .connect(url)
            .await?;
        if let Err(error) = crate::db_guards::require_schema(&writer).await {
            writer.close().await;
            return Err(error);
        }
        // Only the writer owns the server lease. Additional leased connections
        // would compete with that owner, so reads use their own bounded pool.
        // This is a product-query guard, not a separate database-role boundary.
        let reader = match PgPoolOptions::new()
            .max_connections(4)
            .min_connections(1)
            .acquire_timeout(Duration::from_secs(15))
            .after_connect(|connection, _| {
                Box::pin(async move {
                    sqlx::query("SET default_transaction_read_only = on")
                        .execute(connection)
                        .await?;
                    Ok(())
                })
            })
            .connect(url)
            .await
        {
            Ok(reader) => reader,
            Err(error) => {
                writer.close().await;
                return Err(error.into());
            }
        };
        let db = Self::Postgres { writer, reader };
        // Never bootstrap from the immutable migration archive or silently seed.
        if let Err(error) = db.read().await {
            db.close().await;
            return Err(error);
        }
        Ok(db)
    }

    pub(super) fn is_postgres(&self) -> bool {
        matches!(self, Self::Postgres { .. })
    }

    pub(super) async fn read(&self) -> ApiResult<Value> {
        match self {
            Self::Sqlite(pool) => {
                let payload: String =
                    sqlx::query_scalar("SELECT payload FROM workspace WHERE id=1")
                        .fetch_one(pool)
                        .await?;
                let mut value = parse(&payload)?;
                normalize(&mut value);
                Ok(value)
            }
            Self::Postgres { reader, .. } => {
                let mut tx = reader.begin().await?;
                sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ READ ONLY")
                    .execute(&mut *tx)
                    .await?;
                let value = read_postgres(&mut tx).await?;
                tx.commit().await?;
                Ok(value)
            }
        }
    }

    pub(super) async fn change<T>(
        &self,
        f: impl FnOnce(&mut Value) -> ApiResult<T>,
    ) -> ApiResult<T> {
        self.change_observed(f).await.map(|(result, _)| result)
    }

    /// Keep the transaction and single-writer lease, but do not write unchanged state.
    pub(super) async fn change_observed<T>(
        &self,
        f: impl FnOnce(&mut Value) -> ApiResult<T>,
    ) -> ApiResult<(T, bool)> {
        self.change_observed_mode(f, false).await
    }

    /// The only storage path allowed to introduce retained recovery proofs.
    /// It invokes the typed owner-authorized reducer itself on the complete
    /// workspace inside the ordinary SQLite/PG transaction and history guards.
    pub(super) async fn commit_retained_paid_recovery_observed(
        &self, actor: &crate::operator_auth::Actor,
        installed: &crate::retained_paid_recovery::InstalledCapture, body: &Value,
        expected_runtime: &crate::runtime_lifecycle::RuntimeIdentity,
    ) -> ApiResult<(Value, bool)> {
        self.change_observed_mode(|d| {
            crate::runtime_lifecycle::current_owner(d,expected_runtime)?;
            crate::retained_paid_recovery::commit(d, actor, installed, body)
        }, true).await
    }

    async fn change_observed_mode<T>(
        &self,
        f: impl FnOnce(&mut Value) -> ApiResult<T>,
        allow_retained_recovery_delta: bool,
    ) -> ApiResult<(T, bool)> {
        match self {
            Self::Sqlite(pool) => {
                let mut tx = pool.begin().await?;
                let payload: String =
                    sqlx::query_scalar("SELECT payload FROM workspace WHERE id=1")
                        .fetch_one(&mut *tx)
                        .await?;
                let mut value = parse(&payload)?;
                normalize(&mut value);
                let before = value.clone();
                let result = f(&mut value)?;
                crate::db_guards::validate_change_mode(&before, &value, allow_retained_recovery_delta)?;
                immutable_versions(&before, &value)?;
                validate_knowledge(&value)?;
                if value["account"] != before["account"] {
                    // A fresh SQLite file starts with the legacy LikeAvto
                    // placeholder. Permit only the explicit pristine profile
                    // bootstrap; a populated or bound database keeps its owner.
                    let selected = crate::accounts::Profile::from_workspace(&value)?;
                    let mut bootstrapped = before.clone();
                    crate::accounts::initialize(&mut bootstrapped, selected)?;
                    if value["account"] != bootstrapped["account"]
                        || value["connectorBinding"] != bootstrapped["connectorBinding"]
                        || value["settings"]["provider"] != bootstrapped["settings"]["provider"]
                    {
                        return Err(internal("Workspace account is immutable"));
                    }
                }
                if value["connectorBinding"] != before["connectorBinding"] {
                    // A connector may be replaced within its company, but a
                    // foreign binding cannot become the workspace route.
                    crate::active_binding(&value)?;
                }
                if value == before {
                    tx.commit().await?;
                    return Ok((result, false));
                }
                sqlx::query("UPDATE workspace SET payload=? WHERE id=1")
                    .bind(value.to_string())
                    .execute(&mut *tx)
                    .await?;
                tx.commit().await?;
                Ok((result, true))
            }
            Self::Postgres { writer, .. } => {
                let pool_wait = crate::performance::Span::new("workspace.change.pool_wait");
                let mut connection=writer.acquire().await?;
                let mut tx=sqlx::Connection::begin(&mut *connection).await?;
                drop(pool_wait);
                let mut before=Value::Null;
                let mut after=Value::Null;
                let mut metadata_after=Value::Null;
                let mut persistence=None;
                let outcome:ApiResult<_>=async {
                let lock_wait = crate::performance::Span::new("workspace.change.row_lock_wait");
                sqlx::query("SELECT id FROM communityhero.workspaces WHERE id=$1 FOR UPDATE")
                    .bind(WORKSPACE)
                    .fetch_one(&mut *tx)
                    .await?;
                #[cfg(test)] crate::performance::r3_sql_read();
                drop(lock_wait);
                let load = crate::performance::Span::new("workspace.change.load");
                before = read_postgres(&mut tx).await?;
                drop(load);
                let domain = crate::performance::Span::new("workspace.change.clone_and_domain");
                after = before.clone();
                let result = f(&mut after)?;
                drop(domain);
                let validation = crate::performance::Span::new("workspace.change.validation");
                crate::db_guards::validate_change_mode(&before, &after, allow_retained_recovery_delta)?;
                if after == before {
                    return Ok((result, false));
                }
                validate(&after)?;
                immutable_versions(&before, &after)?;
                if after["account"] != before["account"] {
                    return Err(internal("Workspace account is immutable"));
                }
                if after["connectorBinding"] != before["connectorBinding"] {
                    // Provider replacement is allowed within the same account;
                    // an unrelated company's binding is never a valid route.
                    crate::active_binding(&after)?;
                }
                for table in TABLES {
                    let old = rows(&before, table)?;
                    let new = rows(&after, table)?;
                    if new.len() < old.len() || old.iter().zip(new).any(|(a, b)| a["id"] != b["id"])
                    {
                        return Err(internal(
                            "Storage does not support deleting or reordering records",
                        ));
                    }
                }
                drop(validation);
                persistence=Some(crate::performance::Span::new("workspace.change.persist_and_commit"));
                for table in TABLES {
                    let columns = projection(table);
                    // Every interpolated identifier comes from constants in this module.
                    let statement = format!(
                        "INSERT INTO communityhero.{table}(workspace_id,id,ordinal,payload{}) VALUES($1,$2,$3,$4::jsonb{}) ON CONFLICT(workspace_id,id) DO UPDATE SET payload=EXCLUDED.payload{}",
                        columns
                            .iter()
                            .map(|(col, _)| format!(",{col}"))
                            .collect::<String>(),
                        (0..columns.len())
                            .map(|n| format!(",${}", n + 5))
                            .collect::<String>(),
                        columns
                            .iter()
                            .map(|(col, _)| format!(",{col}=EXCLUDED.{col}"))
                            .collect::<String>()
                    );
                    let old = rows(&before, table)?;
                    for (n, value) in rows(&after, table)?.iter().enumerate() {
                        if old.get(n) == Some(value) {
                            continue;
                        }
                        let mut query = sqlx::query(sqlx::AssertSqlSafe(statement.as_str()))
                            .bind(WORKSPACE)
                            .bind(text(value, "id")?)
                            .bind(n as i32)
                            .bind(value.to_string());
                        for (_, key) in columns {
                            query = query.bind(value[*key].as_str());
                        }
                        query.execute(&mut *tx).await?;
                        #[cfg(test)] crate::performance::r3_sql_write();
                    }
                }
                metadata_after = metadata(&after);
                if metadata_after != self::metadata(&before) {
                    sqlx::query(
                        "UPDATE communityhero.workspaces SET metadata=$1::jsonb WHERE id=$2",
                    )
                    .bind(metadata_after.to_string())
                    .bind(WORKSPACE)
                    .execute(&mut *tx)
                    .await?;
                    #[cfg(test)] crate::performance::r3_sql_write();
                }
                Ok((result, true))
                }.await;
                let (outcome,completion)=pg_writer::settle(tx,outcome).await;
                pg_writer::release(&mut connection,writer,completion).await;
                drop(persistence);
                drop(metadata_after);
                outcome
            }
        }
    }

    /// `folder` is the destination backup directory, not the workspace data root.
    pub(super) async fn backup(&self, folder: &Path) -> ApiResult<PathBuf> {
        std::fs::create_dir_all(folder).map_err(|_| internal("Backup directory unavailable"))?;
        let extension = if self.is_postgres() { "json" } else { "sqlite" };
        let path = folder.join(format!("workspace-{}.{extension}", super::id()));
        match self {
            Self::Sqlite(pool) => {
                sqlx::query("VACUUM INTO ?")
                    .bind(path.to_string_lossy().as_ref())
                    .execute(pool)
                    .await?;
            }
            Self::Postgres { .. } => {
                let value = self.read().await?;
                let mut file = std::fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(&path)
                    .map_err(|_| internal("Backup file unavailable"))?;
                file.write_all(value.to_string().as_bytes())
                    .and_then(|_| file.sync_all())
                    .map_err(|_| internal("Backup write failed"))?;
            }
        }
        Ok(path)
    }

    pub(super) async fn close(&self) {
        match self {
            Self::Sqlite(pool) => pool.close().await,
            Self::Postgres { writer, reader } => {
                reader.close().await;
                writer.close().await;
            }
        }
    }
}

fn parse(s: &str) -> ApiResult<Value> {
    serde_json::from_str(s).map_err(|_| internal("Database content invalid"))
}
fn normalize(value: &mut Value) {
    for key in ["knowledge_entries", "knowledge_versions", "feedback"] {
        if value.get(key).is_none() {
            value[key] = serde_json::json!([]);
        }
    }
}
fn immutable_versions(before: &Value, after: &Value) -> ApiResult<()> {
    for key in ["knowledge_versions", "feedback"] {
        let old = rows(before, key)?;
        let new = rows(after, key)?;
        if !new.starts_with(old) {
            return Err(internal("Immutable history cannot be changed or deleted"));
        }
    }
    Ok(())
}
fn validate_knowledge(value: &Value) -> ApiResult<()> {
    for table in ["knowledge_entries", "knowledge_versions", "feedback"] {
        let mut ids = HashSet::new();
        for record in rows(value, table)? {
            if !record.is_object() || !ids.insert(text(record, "id")?) {
                return Err(internal("Invalid knowledge record identity"));
            }
            let references: &[(&str, &str, bool)] = match table {
                "knowledge_entries" => &[
                    ("sourceMaterialId", "materials", false),
                    ("currentVersionId", "knowledge_versions", true),
                ],
                "knowledge_versions" => &[
                    ("sourceMaterialId", "materials", false),
                    ("entryId", "knowledge_entries", true),
                ],
                _ => &[("itemId", "items", true)],
            };
            for (key, target, required) in references {
                if !required && record[*key].is_null() {
                    continue;
                }
                let id = text(record, key)?;
                let linked = rows(value, target)?
                    .iter()
                    .find(|r| r["id"].as_str() == Some(id))
                    .ok_or_else(|| internal("Dangling knowledge reference"))?;
                if *key == "currentVersionId" && linked["entryId"] != record["id"] {
                    return Err(internal("Current version belongs to another entry"));
                }
            }
        }
    }
    Ok(())
}
fn text<'a>(value: &'a Value, key: &str) -> ApiResult<&'a str> {
    value[key]
        .as_str()
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| internal("Missing or invalid record identifier"))
}
fn rows<'a>(value: &'a Value, table: &str) -> ApiResult<&'a Vec<Value>> {
    value[table]
        .as_array()
        .ok_or_else(|| internal("Missing or invalid workspace collection"))
}
fn metadata(value: &Value) -> Value {
    // Entity arrays can hold tens of megabytes of branch/job evidence. They are
    // persisted separately; do not clone them only to immediately discard them.
    Value::Object(value.as_object().expect("validated workspace").iter()
        .filter(|(key, _)| !TABLES.contains(&key.as_str()))
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect())
}

#[cfg(test)]
mod metadata_projection_tests {
    use super::*;
    #[test]
    fn preserves_all_non_entity_state_exactly_without_mutating_the_workspace() {
        let expected=serde_json::json!({"account":"LikeAvto","schemaVersion":7,
            "settings":{"enabled":false},"sync":{"cursor":null,"frontiers":["one","two"]},
            "preparationResearch":[{"jobId":"j","sources":[{"url":"https://example.com","claim":"A claim"}]}],
            "preparationRuns":[{"itemIds":["i"]}],"unknownFutureField":{"nested":[0,true,null,"text"]}});
        let mut workspace=expected.clone();
        for table in TABLES {workspace[table]=serde_json::json!([{"id":format!("{table}-record"),"evidence":"retained only in entity storage"}]);}
        let before=workspace.clone();
        assert_eq!(metadata(&workspace),expected);
        assert_eq!(workspace,before);
        // Preserve historical clone-and-remove semantics for every table, even
        // a partial migration document with a missing collection.
        workspace.as_object_mut().unwrap().remove("branches");
        let mut legacy=workspace.clone();
        for table in TABLES {legacy.as_object_mut().unwrap().remove(table);}
        assert_eq!(metadata(&workspace),legacy);
    }
}
fn projection(table: &str) -> &'static [(&'static str, &'static str)] {
    match table {
        "branches" => &[("post_id", "postId")],
        "items" => &[("post_id", "postId"), ("branch_id", "branchId")],
        "proposals" => &[("item_id", "itemId"), ("status", "status")],
        "approvals" => &[("status", "status")],
        "operations" => &[
            ("item_id", "itemId"),
            ("proposal_id", "proposalId"),
            ("approval_id", "approvalId"),
            ("status", "status"),
        ],
        "materials" => &[("kind", "kind")],
        "jobs" => &[("kind", "kind"), ("status", "status"), ("ref_id", "refId")],
        "audit" => &[("action", "action"), ("ref_id", "refId")],
        "knowledge_entries" => &[
            ("source_material_id", "sourceMaterialId"),
            ("current_version_id", "currentVersionId"),
        ],
        "knowledge_versions" => &[
            ("entry_id", "entryId"),
            ("source_material_id", "sourceMaterialId"),
        ],
        "feedback" => &[("item_id", "itemId")],
        _ => &[],
    }
}

async fn read_postgres(connection: &mut PgConnection) -> ApiResult<Value> {
    let record = sqlx::query(
        "SELECT account,metadata::text,execution_enabled FROM communityhero.workspaces WHERE id=$1",
    )
    .bind(WORKSPACE)
    .fetch_one(&mut *connection)
    .await?;
    #[cfg(test)] crate::performance::r3_sql_read();
    #[cfg(test)] let mut materialized=crate::performance::Span::new("workspace.projection.materialized");
    #[cfg(test)] let mut materialized_rows=0usize;
    #[cfg(test)] let mut materialized_bytes=record.try_get::<&str,_>("metadata")?.len();
    if record.try_get::<bool, _>("execution_enabled")? {
        return Err(internal("PostgreSQL pilot execution must remain disabled"));
    }
    let mut value = parse(record.try_get::<&str, _>("metadata")?)?;
    if !value.is_object()
        || record.try_get::<Option<String>, _>("account")?.as_deref() != value["account"].as_str()
    {
        return Err(internal("Workspace identity mismatch"));
    }
    for table in TABLES {
        if value.get(table).is_some() {
            return Err(internal("Workspace metadata contains entity collections"));
        }
        let statement = format!(
            "SELECT id,ordinal,payload::text{} FROM communityhero.{table} WHERE workspace_id=$1 ORDER BY ordinal",
            projection(table)
                .iter()
                .map(|(col, _)| format!(",{col}"))
                .collect::<String>()
        );
        let records = sqlx::query(sqlx::AssertSqlSafe(statement.as_str()))
            .bind(WORKSPACE)
            .fetch_all(&mut *connection)
            .await?;
        #[cfg(test)] { crate::performance::r3_sql_read(); materialized_rows+=records.len(); }
        let mut values = Vec::with_capacity(records.len());
        for (n, record) in records.into_iter().enumerate() {
            #[cfg(test)] { materialized_bytes+=record.try_get::<&str,_>("payload")?.len(); }
            let payload = parse(record.try_get::<&str, _>("payload")?)?;
            if record.try_get::<i32, _>("ordinal")? != n as i32
                || record.try_get::<&str, _>("id")? != text(&payload, "id")?
            {
                return Err(internal("Record identity or order mismatch"));
            }
            for (col, key) in projection(table) {
                if record.try_get::<Option<String>, _>(*col)?.as_deref() != payload[*key].as_str() {
                    return Err(internal("Record relational projection mismatch"));
                }
            }
            values.push(payload);
        }
        value[table] = Value::Array(values);
    }
    validate(&value)?;
    #[cfg(test)] { materialized.counts(materialized_rows,materialized_bytes,TABLES.len()); }
    Ok(value)
}

fn validate(value: &Value) -> ApiResult<()> {
    if !value.is_object() {
        return Err(internal("Workspace must be an object"));
    }
    text(value, "account")?;
    let mut ids = HashMap::new();
    for table in TABLES {
        let records = rows(value, table)?;
        if records.len() > i32::MAX as usize {
            return Err(internal("Collection too large"));
        }
        let mut seen = HashSet::new();
        for record in records {
            if !record.is_object() || !seen.insert(text(record, "id")?) {
                return Err(internal("Invalid or duplicate record ID"));
            }
            for (_, key) in projection(table) {
                if !record[*key].is_null() && !record[*key].is_string() {
                    return Err(internal("Invalid projected field"));
                }
            }
        }
        ids.insert(table, seen);
    }
    let check = |record: &Value, key: &str, table: &str, required: bool| -> ApiResult<()> {
        if record[key].is_null() && !required {
            return Ok(());
        }
        if !ids[table].contains(text(record, key)?) {
            return Err(internal("Dangling record reference"));
        }
        Ok(())
    };
    for record in rows(value, "branches")? {
        check(record, "postId", "posts", false)?;
    }
    for record in rows(value, "items")? {
        check(record, "postId", "posts", false)?;
        check(record, "branchId", "branches", false)?;
    }
    for record in rows(value, "proposals")? {
        check(record, "itemId", "items", true)?;
    }
    for record in rows(value, "operations")? {
        check(record, "itemId", "items", false)?;
        check(record, "proposalId", "proposals", false)?;
        check(record, "approvalId", "approvals", false)?;
    }
    for record in rows(value, "conversations")? {
        if let Some(targets) = record.get("itemIds") {
            for target in targets
                .as_array()
                .ok_or_else(|| internal("Invalid conversation itemIds"))?
            {
                if !target.as_str().is_some_and(|id| ids["items"].contains(id)) {
                    return Err(internal("Dangling conversation itemId"));
                }
            }
        }
    }
    for record in rows(value, "approvals")? {
        for proposal in record["proposals"]
            .as_array()
            .ok_or_else(|| internal("Invalid approval proposals"))?
        {
            check(proposal, "id", "proposals", true)?;
        }
    }
    // IDs and duplicate checks above have already validated these keys. Build
    // the inverse lookup once rather than scanning every retained version for
    // each entry while holding the workspace writer.
    let knowledge_versions: HashMap<&str, &Value> = rows(value,"knowledge_versions")?
        .iter().map(|version|(version["id"].as_str().unwrap(),version)).collect();
    for record in rows(value, "knowledge_entries")? {
        check(record, "sourceMaterialId", "materials", false)?;
        check(record, "currentVersionId", "knowledge_versions", true)?;
        let version = knowledge_versions[record["currentVersionId"].as_str().unwrap()];
        if version["entryId"] != record["id"] {
            return Err(internal(
                "Knowledge current version belongs to another entry",
            ));
        }
    }
    for record in rows(value, "knowledge_versions")? {
        check(record, "entryId", "knowledge_entries", true)?;
        check(record, "sourceMaterialId", "materials", false)?;
    }
    for record in rows(value, "feedback")? {
        check(record, "itemId", "items", true)?;
    }
    Ok(())
}

#[cfg(test)]
mod performance_tests {
    use super::*;
    use serde_json::json;
    use std::sync::Arc;
    use tokio::sync::{Mutex, broadcast};

    async fn app() -> (crate::App, tempfile::TempDir) {
        let folder = tempfile::tempdir().unwrap();
        let pool = crate::open_db(&folder.path().join("workspace.sqlite"))
            .await
            .unwrap();
        let (events, _) = broadcast::channel(32);
        let app = crate::App {lifecycle_task_count: Default::default(), lifecycle_admission: Arc::new(crate::runtime_lifecycle_startup::Admission::fixture(crate::accounts::Profile::LikeAvto)), lifecycle_owner: Arc::new(crate::runtime_lifecycle_startup::Admission::fixture(crate::accounts::Profile::LikeAvto).identity().clone()), lifecycle_provider_token: Default::default(), lifecycle_work: Default::default(), media_discovery: Default::default(),preparation_wake: Default::default(),provider_session: Default::default(),
                account:crate::accounts::Profile::LikeAvto,navigation:crate::account_navigation::Navigation::root(),
                db: Database::Sqlite(pool),
                gate: Arc::new(crate::writer_gate::WriterGate::default()), execution_gate: Arc::new(Mutex::new(())),
        preparation_workers: Default::default(),editorial_gate: Default::default(),assistant_gate: Arc::new(Mutex::new(())),
        assistant_chat_gate: Arc::new(Mutex::new(())),
                events,
                csrf: "test-csrf".into(),
                auth: None,
                public_origin: None, external_writes: false,
                port: 0,
                data: folder.path().to_owned(),
                bridge: PathBuf::new(),
                node: PathBuf::new(),
                tasks: Arc::new(Mutex::new(HashMap::new())),bootstrap_cache:Arc::new(crate::bootstrap_cache::Cache::default()),
            };
        crate::runtime_lifecycle_app::initialize_app_fixture(&app).await.unwrap();
        (app, folder)
    }

    #[tokio::test]
    async fn unchanged_transactions_do_not_write_or_broadcast() {
        let (app, _folder) = app().await;
        let Database::Sqlite(pool) = &app.db else {
            unreachable!()
        };
        sqlx::query("CREATE TABLE write_count(n INTEGER NOT NULL)")
            .execute(pool)
            .await
            .unwrap();
        sqlx::query("INSERT INTO write_count VALUES(0)")
            .execute(pool)
            .await
            .unwrap();
        sqlx::query("CREATE TRIGGER count_workspace_update AFTER UPDATE ON workspace BEGIN UPDATE write_count SET n=n+1; END").execute(pool).await.unwrap();
        let mut events = app.events.subscribe();
        assert_eq!(app.change(|_| Ok(17)).await.unwrap(), 17);
        assert!(matches!(
            events.try_recv(),
            Err(broadcast::error::TryRecvError::Empty)
        ));
        let count: i64 = sqlx::query_scalar("SELECT n FROM write_count")
            .fetch_one(pool)
            .await
            .unwrap();
        assert_eq!(count, 0);
        app.change(|d| {
            d["settings"]["test"] = json!(true);
            Ok(())
        })
        .await
        .unwrap();
        assert!(events.try_recv().is_ok());
        app.change(|d| {
            d["settings"]["test"] = json!(true);
            Ok(())
        })
        .await
        .unwrap();
        assert!(matches!(
            events.try_recv(),
            Err(broadcast::error::TryRecvError::Empty)
        ));
        let result: ApiResult<()> = app
            .change(|d| {
                d["settings"]["test"] = json!(false);
                Err(internal("Deliberate rejection"))
            })
            .await;
        assert!(result.is_err());
        assert!(matches!(
            events.try_recv(),
            Err(broadcast::error::TryRecvError::Empty)
        ));
        let count: i64 = sqlx::query_scalar("SELECT n FROM write_count")
            .fetch_one(pool)
            .await
            .unwrap();
        assert_eq!(count, 1);
        assert_eq!(app.read().await.unwrap()["settings"]["test"], true);
    }

    #[tokio::test]
    async fn bootstrap_compacts_only_response_and_preserves_recovery_jobs() {
        let (app, _folder) = app().await;
        app.change(|d| {
            d["branches"] = json!([{"id":"branch","messages":[{"id":"message","text":"Visible"}],"observedMessages":[{"id":"message","text":"Observed"}]}]);
            d["items"] = json!([{"id":"item"}]);
            d["materials"] = json!([{"id":"material","text":"Keep"}]);
            let mut jobs = vec![];
            for (n, status) in ["running","queued","unknown","interrupted"].iter().enumerate() {
                // Explicit empty response fields distinguish wire shaping from
                // loss of the opaque recovery checkpoint retained by full-row equality.
                jobs.push(json!({"id":format!("active-{n}"),"kind":"assistant","purpose":"discussion","refId":"item","status":status,
                    "prepareBundle":null,"result":{"visualProgress":null,"recoveryCheckpoint":{"outputRef":format!("retained-{n}"),"receiptSha256":"a".repeat(64)}}}));
            }
            for n in 0..450 {
                jobs.push(json!({"id":format!("finished-{n}"),"status":"completed","prepareBundle":{"id":"bundle","digest":"digest","request":{"text":"x".repeat(1000)}}}));
            }
            d["jobs"] = json!(jobs);
            Ok(())
        }).await.unwrap();
        let before = app.read().await.unwrap();
        let view = crate::bootstrap(axum::extract::State(app.clone()))
            .await
            .unwrap()
            .0;
        assert_eq!(view["jobs"].as_array().unwrap().len(), 204);
        assert_eq!(view["jobs"][0]["status"], "running");
        assert_eq!(view["jobs"][3]["status"], "interrupted");
        assert_eq!(&view["jobs"].as_array().unwrap()[..4], &before["jobs"].as_array().unwrap()[..4]);
        assert_eq!(view["jobs"][4]["id"], "finished-250");
        assert_eq!(view["jobs"][203]["id"], "finished-449");
        assert_eq!(view["historyMetadata"]["jobs"]["omitted"], 250);
        assert_eq!(view["historyMetadata"]["jobs"]["historyTruncated"], true);
        assert!(view["jobs"][4]["prepareBundle"].get("request").is_none());
        assert_eq!(view["jobs"][4]["prepareBundle"]["digest"], "digest");
        assert!(view["branches"][0].get("observedMessages").is_none());
        assert_eq!(
            view["branches"][0]["messages"],
            before["branches"][0]["messages"]
        );
        assert_eq!(view["items"], before["items"]);
        assert_eq!(view["materials"], before["materials"]);
        assert_eq!(app.read().await.unwrap(), before);
        assert!(view.to_string().len() < before.to_string().len() / 2);
    }
}

impl Database {
    /// The closure sees one item and its related records. It may update that item
    /// (not its routing identity) and append feedback; all other state is read-only.
    pub(super) async fn change_item_observed<T>(
        &self,
        key: &str,
        f: impl FnOnce(&mut Value) -> ApiResult<T>,
    ) -> ApiResult<(T, bool)> {
        match self {
            Self::Sqlite(_) => {
                self.change_observed(|workspace| {
                    let before = item_scope(workspace, key)?;
                    let mut after = before.clone();
                    let result = f(&mut after)?;
                    validate_item_scope(&before, &after)?;
                    if before != after {
                        let item = workspace["items"]
                            .as_array_mut()
                            .unwrap()
                            .iter_mut()
                            .find(|v| v["id"].as_str() == Some(key))
                            .unwrap();
                        *item = after["items"][0].clone();
                        let old_len = before["feedback"].as_array().unwrap().len();
                        workspace["feedback"].as_array_mut().unwrap().extend(
                            after["feedback"].as_array().unwrap()[old_len..]
                                .iter()
                                .cloned(),
                        );
                    }
                    Ok(result)
                })
                .await
            }
            Self::Postgres { writer, .. } => {
                let mut tx = writer.begin().await?;
                let row=sqlx::query("SELECT account,execution_enabled,metadata ? 'runtimeLifecycle' AS has_runtime_lifecycle,(metadata->'runtimeLifecycle')::text AS runtime_lifecycle FROM communityhero.workspaces WHERE id=$1 FOR UPDATE").bind(WORKSPACE).fetch_one(&mut *tx).await?;
                if row.try_get::<bool, _>("execution_enabled")? {
                    return Err(internal("PostgreSQL pilot execution must remain disabled"));
                }
                let payload: Option<String>=sqlx::query_scalar("SELECT payload::text FROM communityhero.items WHERE workspace_id=$1 AND id=$2 FOR UPDATE").bind(WORKSPACE).bind(key).fetch_optional(&mut *tx).await?;
                let item = parse(&payload.ok_or_else(|| internal("Item not found"))?)?;
                let mut before = serde_json::json!({"account":row.try_get::<String,_>("account")?,"items":[item]});
                if row.try_get::<bool,_>("has_runtime_lifecycle")? {
                    before["runtimeLifecycle"]=parse(row.try_get::<&str,_>("runtime_lifecycle")?)?;
                }
                for table in ["operations", "proposals", "feedback"] {
                    let statement = format!(
                        "SELECT payload::text FROM communityhero.{table} WHERE workspace_id=$1 AND item_id=$2 ORDER BY ordinal"
                    );
                    let records: Vec<String> =
                        sqlx::query_scalar(sqlx::AssertSqlSafe(statement.as_str()))
                            .bind(WORKSPACE)
                            .bind(key)
                            .fetch_all(&mut *tx)
                            .await?;
                    before[table] = Value::Array(
                        records
                            .iter()
                            .map(|p| parse(p))
                            .collect::<ApiResult<Vec<_>>>()?,
                    );
                }
                let mut after = before.clone();
                let result = f(&mut after)?;
                validate_item_scope(&before, &after)?;
                if before == after {
                    tx.commit().await?;
                    return Ok((result, false));
                }
                sqlx::query("UPDATE communityhero.items SET payload=$1::jsonb WHERE workspace_id=$2 AND id=$3").bind(after["items"][0].to_string()).bind(WORKSPACE).bind(key).execute(&mut *tx).await?;
                let old_len = before["feedback"].as_array().unwrap().len();
                let ordinal:i64=sqlx::query_scalar("SELECT COALESCE(MAX(ordinal),-1)::bigint+1 FROM communityhero.feedback WHERE workspace_id=$1").bind(WORKSPACE).fetch_one(&mut *tx).await?;
                for (n, feedback) in after["feedback"].as_array().unwrap()[old_len..]
                    .iter()
                    .enumerate()
                {
                    let position = i32::try_from(ordinal + n as i64)
                        .map_err(|_| internal("Feedback history too large"))?;
                    sqlx::query("INSERT INTO communityhero.feedback(workspace_id,id,ordinal,payload,item_id) VALUES($1,$2,$3,$4::jsonb,$5)").bind(WORKSPACE).bind(text(feedback,"id")?).bind(position).bind(feedback.to_string()).bind(key).execute(&mut *tx).await?;
                }
                tx.commit().await?;
                Ok((result, true))
            }
        }
    }
}
fn item_scope(workspace: &Value, key: &str) -> ApiResult<Value> {
    let item = rows(workspace, "items")?
        .iter()
        .find(|v| v["id"].as_str() == Some(key))
        .ok_or_else(|| internal("Item not found"))?;
    let mut scope = serde_json::json!({"account":workspace["account"],"items":[item]});
    // Capture::with checks the fixed native owner against CURRENT locked
    // metadata. Retain full lifecycle state and exact key presence only.
    if let Some(lifecycle)=workspace.get("runtimeLifecycle") {
        scope["runtimeLifecycle"]=lifecycle.clone();
    }
    for table in ["operations", "proposals", "feedback"] {
        scope[table] = Value::Array(
            rows(workspace, table)?
                .iter()
                .filter(|v| v["itemId"].as_str() == Some(key))
                .cloned()
                .collect(),
        );
    }
    Ok(scope)
}
fn validate_item_scope(before: &Value, after: &Value) -> ApiResult<()> {
    if after.as_object().map(|v| v.len()) != Some(5+usize::from(before.get("runtimeLifecycle").is_some()))
        || after.get("runtimeLifecycle") != before.get("runtimeLifecycle")
        || after["account"] != before["account"]
        || after["operations"] != before["operations"]
        || after["proposals"] != before["proposals"]
    {
        return Err(internal("Scoped mutation changed read-only state"));
    }
    let items = rows(after, "items")?;
    if items.len() != 1 || !items[0].is_object() {
        return Err(internal("Scoped mutation must preserve one item"));
    }
    for field in ["id", "postId", "branchId"] {
        if items[0][field] != before["items"][0][field] {
            return Err(internal("Scoped mutation changed item identity"));
        }
    }
    let old = rows(before, "feedback")?;
    let new = rows(after, "feedback")?;
    if !new.starts_with(old) {
        return Err(internal("Feedback history is immutable"));
    }
    let mut ids = HashSet::new();
    for record in new {
        if !record.is_object()
            || !ids.insert(text(record, "id")?)
            || record["itemId"] != items[0]["id"]
        {
            return Err(internal("Invalid scoped feedback"));
        }
    }
    Ok(())
}

#[cfg(test)]
mod knowledge_storage_tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn history_is_append_only_and_legacy_arrays_normalize() {
        let mut old = json!({"account":"LikeAvto"});
        normalize(&mut old);
        let mut new = old.clone();
        new["knowledge_versions"] = json!([{"id":"v1","text":"fact"}]);
        assert!(immutable_versions(&old, &new).is_ok());
        let old = new.clone();
        new["knowledge_versions"][0]["text"] = json!("rewritten");
        assert!(immutable_versions(&old, &new).is_err());
        new["knowledge_versions"] = json!([]);
        assert!(immutable_versions(&old, &new).is_err());
    }
    #[tokio::test]
    async fn scoped_write_preserves_other_items_and_rejects_route_or_history_changes() {
        let dir = tempfile::tempdir().unwrap();
        let pool = crate::open_db(&dir.path().join("workspace.sqlite"))
            .await
            .unwrap();
        let db = Database::Sqlite(pool);
        db.change(|d| {
            d["items"] =
                json!([{"id":"one","postId":"p","draft":"old"},{"id":"two","draft":"keep"}]);
            Ok(())
        })
        .await
        .unwrap();
        let (_, changed) = db
            .change_item_observed("one", |d| {
                assert_eq!(d["items"].as_array().unwrap().len(), 1);
                d["items"][0]["draft"] = json!("new");
                d["feedback"]
                    .as_array_mut()
                    .unwrap()
                    .push(json!({"id":"f","itemId":"one","status":"pending_review"}));
                Ok(())
            })
            .await
            .unwrap();
        assert!(changed);
        let saved = db.read().await.unwrap();
        assert_eq!(saved["items"][1]["draft"], "keep");
        assert!(
            db.change_item_observed("one", |d| {
                d["items"][0]["postId"] = json!("other");
                Ok(())
            })
            .await
            .is_err()
        );
        assert!(
            db.change_item_observed("one", |d| {
                d["feedback"][0]["status"] = json!("approved");
                Ok(())
            })
            .await
            .is_err()
        );
        assert_eq!(saved, db.read().await.unwrap());
        assert!(!db.change_item_observed("one", |_| Ok(())).await.unwrap().1);
    }
}

#[cfg(test)]
#[path="post_network_transition_pg_tests.rs"]
mod post_network_transition_pg_tests;

#[cfg(test)]
#[path="storage_item_lifecycle_tests.rs"]
mod item_lifecycle_tests;
