//! Transactional projection for the periodic automatic preparation claim only.
//! A claim sees complete source/authority collections, but unrelated discussion,
//! sync and private history never leave PostgreSQL. SQLite merges into its full
//! document under the ordinary writer transaction.
use super::*;
use serde_json::json;

const SOURCE: &[&str] = &["posts", "branches", "operations", "materials", "knowledge_entries", "knowledge_versions"];
const MUTABLE: &[&str] = &["items", "proposals", "jobs"];
const OMITTED: &[&str] = &["conversations", "audit", "approvals", "feedback"];
const ITEM_MUTABLE: &[&str] = &["autoPreparation", "autoRevalidation", "preparationMediaWait", "workflow", "decision", "reason", "revision"];
const PROPOSAL_MUTABLE: &[&str] = &["status", "staleReason", "staleAt", "revision"];
const MEDIA_JOB_MUTABLE: &[&str] = &["status", "finishedAt", "result", "error"];

fn retain_job(job: &Value, references: &HashSet<String>) -> bool {
    job["kind"] == "media"
        || matches!(job["purpose"].as_str(), Some("auto_prepare" | "auto_revalidate" | "auto_media"))
        || (job["kind"] == "assistant" && matches!(job["status"].as_str(), Some("running" | "queued")))
        || job["id"].as_str().is_some_and(|id| references.contains(id))
}

fn references(value: &Value) -> HashSet<String> {
    let mut refs = HashSet::new();
    for item in crate::list(value, "items") {
        for field in [&item["autoPreparation"]["jobId"], &item["autoRevalidation"]["jobId"]] {
            if let Some(id) = field.as_str() { refs.insert(id.to_owned()); }
        }
    }
    for proposal in crate::list(value, "proposals") {
        for field in [&proposal["prepareRunId"], &proposal["recovery"]["prepareRunId"]] {
            if let Some(id) = field.as_str() { refs.insert(id.to_owned()); }
        }
    }
    refs
}

fn claim_metadata(workspace: &Value) -> ApiResult<Value> {
    if !workspace.is_object() { return Err(internal("Workspace must be an object")); }
    let mut view = metadata(workspace);
    // A missing connectorBinding selects the legacy LikeAvto mapping; preserve
    // absence rather than substituting null. mediaQueue is always write-ready.
    if view.get("mediaQueue").is_none() { view["mediaQueue"] = Value::Null; }
    Ok(view)
}

fn projection_of(workspace: &Value) -> ApiResult<Value> {
    let mut view = claim_metadata(workspace)?;
    let refs = references(workspace);
    for table in TABLES {
        match table {
            "jobs" => view[table] = json!(rows(workspace, table)?.iter().filter(|j| retain_job(j, &refs)).collect::<Vec<_>>()),
            name if OMITTED.contains(&name) => (),
            _ => view[table] = workspace[table].clone(),
        }
    }
    Ok(view)
}

// Scope is deliberately strict: adding a new claim side effect requires a
// conscious storage review, not silent persistence of a partial workspace.
fn without(value: &Value, keys: &[&str]) -> Value {
    let mut copy = value.clone();
    if let Some(object) = copy.as_object_mut() {
        for key in keys { object.remove(*key); }
    }
    copy
}
fn stable_rows(before: &Value, after: &Value, table: &str) -> ApiResult<()> {
    let old = rows(before, table)?;
    let new = rows(after, table)?;
    if new.len() < old.len() || old.iter().zip(new).any(|(a, b)| a["id"] != b["id"]) {
        return Err(internal("Preparation claim removed or reordered records"));
    }
    let mut ids = HashSet::new();
    for value in new {
        if !value.is_object() || !ids.insert(text(value, "id")?) {
            return Err(internal("Invalid preparation claim identity"));
        }
        for (_, key) in projection(table) {
            if !value[*key].is_null() && !value[*key].is_string() {
                return Err(internal("Invalid preparation claim relational projection"));
            }
        }
    }
    Ok(())
}
fn validate_claim_change(before: &Value, after: &Value) -> ApiResult<()> {
    if !after.is_object() { return Err(internal("Invalid preparation claim workspace")); }
    for name in OMITTED {
        if before.get(*name) != after.get(*name) {
            return Err(internal("Preparation claim changed omitted history"));
        }
    }
    let before_keys: HashSet<_> = before.as_object().unwrap().keys().collect();
    let after_keys: HashSet<_> = after.as_object().unwrap().keys().collect();
    if before_keys != after_keys { return Err(internal("Preparation claim changed workspace shape")); }
    if without(&metadata(before), &["mediaQueue"]) != without(&metadata(after), &["mediaQueue"]) {
        return Err(internal("Preparation claim changed workspace metadata"));
    }
    for table in SOURCE {
        if before[*table] != after[*table] {
            return Err(internal("Preparation claim changed source or operation history"));
        }
    }
    for table in MUTABLE { stable_rows(before, after, table)?; }
    let old_items = rows(before, "items")?;
    let new_items = rows(after, "items")?;
    if old_items.len() != new_items.len() { return Err(internal("Preparation claim added an item")); }
    for (old, new) in old_items.iter().zip(new_items) {
        if without(old, ITEM_MUTABLE) != without(new, ITEM_MUTABLE) {
            return Err(internal("Preparation claim changed protected item data"));
        }
    }
    let old_proposals = rows(before, "proposals")?;
    let new_proposals = rows(after, "proposals")?;
    if old_proposals.len() != new_proposals.len() { return Err(internal("Preparation claim added a proposal")); }
    for (old, new) in old_proposals.iter().zip(new_proposals) {
        if without(old, PROPOSAL_MUTABLE) != without(new, PROPOSAL_MUTABLE)
            || (old != new && !(old["status"] == "draft" && new["status"] == "stale")) {
            return Err(internal("Preparation claim changed protected proposal"));
        }
    }
    let old_jobs = rows(before, "jobs")?;
    let new_jobs = rows(after, "jobs")?;
    for (old, new) in old_jobs.iter().zip(new_jobs) {
        if old != new && (old["purpose"] != "auto_media"
            || without(old, MEDIA_JOB_MUTABLE) != without(new, MEDIA_JOB_MUTABLE)) {
            return Err(internal("Preparation claim changed a protected job"));
        }
    }
    for new in &new_jobs[old_jobs.len()..] {
        if !((new["kind"] == "media" && new["purpose"] == "auto_media")
            || (new["kind"] == "assistant" && matches!(new["purpose"].as_str(), Some("auto_prepare" | "auto_revalidate"))
                && new["status"] == "running")) {
            return Err(internal("Preparation claim appended an unrelated job"));
        }
    }
    crate::db_guards::validate_change(before, after)?;
    Ok(())
}

fn merge_claim_delta(workspace: &mut Value, before: &Value, after: &Value) -> ApiResult<()> {
    if before["mediaQueue"] != after["mediaQueue"] { workspace["mediaQueue"] = after["mediaQueue"].clone(); }
    for table in MUTABLE {
        let old = rows(before, table)?;
        for (index, record) in rows(after, table)?.iter().enumerate() {
            if old.get(index) == Some(record) { continue; }
            let target = workspace[*table].as_array_mut().ok_or_else(|| internal("Workspace collection missing"))?;
            if index < old.len() {
                let stored = target.iter_mut().find(|v| v["id"] == record["id"])
                    .ok_or_else(|| internal("Preparation claim record disappeared"))?;
                *stored = record.clone();
            } else {
                if target.iter().any(|v| v["id"] == record["id"]) { return Err(internal("Preparation claim reused an unloaded identity")); }
                target.push(record.clone());
            }
        }
    }
    Ok(())
}

const PG_JOB_PREDICATE: &str = r#"(kind='media' OR payload->>'purpose' IN ('auto_prepare','auto_revalidate','auto_media')
 OR (kind='assistant' AND status IN ('running','queued'))
 OR id IN (SELECT i.payload#>>'{autoPreparation,jobId}' FROM communityhero.items i WHERE i.workspace_id=$1
           UNION SELECT i.payload#>>'{autoRevalidation,jobId}' FROM communityhero.items i WHERE i.workspace_id=$1
           UNION SELECT p.payload->>'prepareRunId' FROM communityhero.proposals p WHERE p.workspace_id=$1
           UNION SELECT p.payload#>>'{recovery,prepareRunId}' FROM communityhero.proposals p WHERE p.workspace_id=$1))"#;

async fn load_pg_claim(connection: &mut PgConnection) -> ApiResult<Value> {
    let record = sqlx::query("SELECT account, execution_enabled, metadata::text AS claim_metadata FROM communityhero.workspaces WHERE id=$1 FOR UPDATE")
        .bind(WORKSPACE).fetch_one(&mut *connection).await?;
    if record.try_get::<bool, _>("execution_enabled")? { return Err(internal("PostgreSQL pilot execution must remain disabled")); }
    let metadata = parse(record.try_get::<&str, _>("claim_metadata")?)?;
    if TABLES.iter().any(|table| metadata.get(*table).is_some()) {
        return Err(internal("Workspace metadata contains entity collections"));
    }
    let mut view = claim_metadata(&metadata)?;
    if record.try_get::<Option<String>, _>("account")?.as_deref() != view["account"].as_str() {
        return Err(internal("Workspace identity mismatch"));
    }
    for table in TABLES {
        if OMITTED.contains(&table) { continue; }
        let condition = if table == "jobs" { PG_JOB_PREDICATE } else { "TRUE" };
        let statement = format!("SELECT id,payload::text{} FROM communityhero.{table} WHERE workspace_id=$1 AND {condition} ORDER BY ordinal",
            projection(table).iter().map(|(column, _)| format!(",{column}")).collect::<String>());
        let records = sqlx::query(sqlx::AssertSqlSafe(statement.as_str())).bind(WORKSPACE)
            .fetch_all(&mut *connection).await?;
        let mut data = Vec::with_capacity(records.len());
        let mut seen = HashSet::new();
        for record in records {
            let payload = parse(record.try_get::<&str, _>("payload")?)?;
            let id = text(&payload, "id")?;
            if record.try_get::<&str, _>("id")? != id || !seen.insert(id.to_owned()) {
                return Err(internal("Preparation claim record identity mismatch"));
            }
            for (column, field) in projection(table) {
                if (!payload[*field].is_null() && !payload[*field].is_string())
                    || record.try_get::<Option<String>, _>(*column)?.as_deref() != payload[*field].as_str() {
                    return Err(internal("Preparation claim record projection mismatch"));
                }
            }
            data.push(payload);
        }
        view[table] = Value::Array(data);
    }
    Ok(view)
}

async fn persist_claim_record(connection: &mut PgConnection, table: &str, value: &Value, append: bool) -> ApiResult<()> {
    let columns = projection(table);
    let statement = if append {
        format!("INSERT INTO communityhero.{table}(workspace_id,id,payload,ordinal{}) SELECT $1,$2,$3::jsonb,COALESCE(MAX(ordinal),-1)+1{} FROM communityhero.{table} WHERE workspace_id=$1",
            columns.iter().map(|(c, _)| format!(",{c}")).collect::<String>(),
            (0..columns.len()).map(|n| format!(",${}", n+4)).collect::<String>())
    } else {
        format!("UPDATE communityhero.{table} SET payload=$3::jsonb{} WHERE workspace_id=$1 AND id=$2",
            columns.iter().enumerate().map(|(n, (c, _))| format!(",{c}=${}", n+4)).collect::<String>())
    };
    let mut query = sqlx::query(sqlx::AssertSqlSafe(statement.as_str())).bind(WORKSPACE)
        .bind(text(value, "id")?).bind(value.to_string());
    for (_, field) in columns { query = query.bind(value[*field].as_str()); }
    if query.execute(connection).await?.rows_affected() != 1 { return Err(internal("Preparation claim record disappeared")); }
    Ok(())
}

impl Database {
    /// Caller holds the App writer gate. PostgreSQL also uses the leased writer
    /// and workspace row lock. The closure may only perform claim/reconciliation.
    pub(crate) async fn change_preparation_claim_observed<T>(
        &self, f: impl FnOnce(&mut Value) -> ApiResult<T>,
    ) -> ApiResult<(T, bool)> {
        match self {
            Self::Sqlite(_) => self.change_observed(|workspace| {
                let before = projection_of(workspace)?;
                let mut after = before.clone();
                let result = f(&mut after)?;
                validate_claim_change(&before, &after)?;
                merge_claim_delta(workspace, &before, &after)?;
                Ok(result)
            }).await,
            Self::Postgres { writer, .. } => {
                let mut tx = writer.begin().await?;
                let before = load_pg_claim(&mut tx).await?;
                let mut after = before.clone();
                let result = f(&mut after)?;
                validate_claim_change(&before, &after)?;
                if before == after { tx.commit().await?; return Ok((result, false)); }
                for table in MUTABLE {
                    let old = rows(&before, table)?;
                    for (index, value) in rows(&after, table)?.iter().enumerate() {
                        if old.get(index) != Some(value) {
                            persist_claim_record(&mut tx, table, value, index >= old.len()).await?;
                        }
                    }
                }
                if before["mediaQueue"] != after["mediaQueue"] {
                    sqlx::query("UPDATE communityhero.workspaces SET metadata=jsonb_set(metadata,'{mediaQueue}',$2::jsonb,true) WHERE id=$1")
                        .bind(WORKSPACE).bind(after["mediaQueue"].to_string()).execute(&mut *tx).await?;
                }
                tx.commit().await?;
                Ok((result, true))
            }
        }
    }
}

#[cfg(test)]
#[path = "storage_preparation_tests.rs"]
mod tests;
