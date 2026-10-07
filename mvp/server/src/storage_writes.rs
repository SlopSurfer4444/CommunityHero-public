//! Deliberately narrow transactional write projections. A scoped closure is not
//! a workspace closure: omitted collections are empty, and only the documented
//! fields may change. PostgreSQL holds the same workspace lock and leased pool.
//! SQLite retains its one-document storage and therefore still rewrites JSON.
use super::*;
use serde_json::json;

enum Scope<'a> {
    Job(&'a str),
    Schedule,
    SourceClaim,
    Status(&'a [Value]),
}
// Includes the complete bounded 12-object, two-page status head.
const STATUS_ROUTE_LIMIT:usize=2400;

impl Database {
    /// Read full workspace metadata for cursor/connector decisions without
    /// loading entity payloads or taking the single writer connection.
    pub(crate) async fn read_metadata(&self) -> ApiResult<Value> {
        match self {
            Self::Sqlite(_) => Ok(metadata(&self.read().await?)),
            Self::Postgres { reader, .. } => {
                let record = sqlx::query("SELECT account,metadata::text,execution_enabled FROM communityhero.workspaces WHERE id=$1")
                    .bind(WORKSPACE).fetch_one(reader).await?;
                #[cfg(test)] crate::performance::r3_sql_read();
                if record.try_get::<bool, _>("execution_enabled")? {
                    return Err(internal("PostgreSQL pilot execution must remain disabled"));
                }
                let value = parse(record.try_get::<&str, _>("metadata")?)?;
                if !value.is_object() || value["account"].as_str().is_none()
                    || record.try_get::<Option<String>, _>("account")?.as_deref() != value["account"].as_str() {
                    return Err(internal("Workspace identity mismatch"));
                }
                for table in TABLES {
                    if value.get(table).is_some() {
                        return Err(internal("Workspace metadata contains entity collections"));
                    }
                }
                Ok(value)
            }
        }
    }

    /// Source scheduling needs routing/status tokens, never comment text, draft,
    /// branch evidence, or historical job payloads. The item projection is readonly.
    pub(crate) async fn change_source_claim_observed<T>(
        &self,
        f: impl FnOnce(&mut Value) -> ApiResult<T>,
    ) -> ApiResult<(T, bool)> {
        self.change_scope(Scope::SourceClaim, f).await
    }

    pub(crate) async fn read_source_status(&self) -> ApiResult<Value> {
        match self {
            Self::Sqlite(_) => {
                let value = project(&self.read().await?, &Scope::SourceClaim)?;
                validate_scope(&value, &value, &Scope::SourceClaim)?;
                Ok(value)
            }
            Self::Postgres { reader, .. } => {
                // A single statement gives metadata, active jobs and source tokens
                // one MVCC snapshot, without borrowing the leased writer or locking
                // its workspace row. Atomic claim rechecks remain in change_scope.
                let record = sqlx::query(
                    "SELECT w.account,w.metadata::text,w.execution_enabled,
                     COALESCE((SELECT jsonb_agg(jsonb_build_object('id',j.id,'kind',j.kind,'status',j.status,'refId',j.ref_id,'payload',j.payload) ORDER BY j.ordinal) FROM communityhero.jobs j WHERE j.workspace_id=w.id AND j.status IN ('running','queued')),'[]'::jsonb)::text AS active_jobs,
                     COALESCE((SELECT jsonb_agg(COALESCE((SELECT jsonb_object_agg(e.key,e.value) FROM jsonb_each(i.payload) e WHERE e.key=ANY($2)),'{}'::jsonb) ORDER BY i.ordinal) FROM communityhero.items i WHERE i.workspace_id=w.id),'[]'::jsonb)::text AS source_items
                     FROM communityhero.workspaces w WHERE w.id=$1",
                ).bind(WORKSPACE).bind(SOURCE_FIELDS.to_vec()).fetch_one(reader).await?;
                if record.try_get::<bool, _>("execution_enabled")? {
                    return Err(internal("PostgreSQL pilot execution must remain disabled"));
                }
                let mut value = parse(record.try_get::<&str, _>("metadata")?)?;
                if !value.is_object() || value["account"].as_str().is_none()
                    || record.try_get::<Option<String>, _>("account")?.as_deref() != value["account"].as_str() {
                    return Err(internal("Workspace identity mismatch"));
                }
                for table in TABLES {
                    if value.get(table).is_some() {
                        return Err(internal("Workspace metadata contains entity collections"));
                    }
                    value[table] = json!([]);
                }
                let active = parse(record.try_get::<&str, _>("active_jobs")?)?;
                let mut jobs = Vec::new();
                for job in active.as_array().ok_or_else(|| internal("Invalid active jobs"))? {
                    let payload = &job["payload"];
                    if job["id"].as_str() != Some(text(payload, "id")?) {
                        return Err(internal("Record identity mismatch"));
                    }
                    for (_, key) in projection("jobs") {
                        if (!payload[*key].is_null() && !payload[*key].is_string())
                            || job[*key].as_str() != payload[*key].as_str() {
                            return Err(internal("Record relational projection mismatch"));
                        }
                    }
                    jobs.push(payload.clone());
                }
                value["jobs"] = json!(jobs);
                value["items"] = parse(record.try_get::<&str, _>("source_items")?)?;
                validate_scope(&value, &value, &Scope::SourceClaim)?;
                Ok(value)
            }
        }
    }

    /// One existing job plus metadata. Only that job and `sync` can change.
    pub(crate) async fn change_job_observed<T>(
        &self,
        key: &str,
        f: impl FnOnce(&mut Value) -> ApiResult<T>,
    ) -> ApiResult<(T, bool)> {
        self.change_scope(Scope::Job(key), f).await
    }

    /// Metadata and currently queued/running jobs. Existing jobs are read-only;
    /// permits appending jobs and changing sync deadlines/claim ownership.
    pub(crate) async fn change_schedule_observed<T>(
        &self,
        f: impl FnOnce(&mut Value) -> ApiResult<T>,
    ) -> ApiResult<(T, bool)> {
        self.change_scope(Scope::Schedule, f).await
    }

    /// At most 2400 explicit provider routes. Only their status/workflow clocks,
    /// revisions and sync metadata can change; drafts and routing are immutable.
    pub(crate) async fn change_status_observed<T>(
        &self,
        routes: &[Value],
        f: impl FnOnce(&mut Value) -> ApiResult<T>,
    ) -> ApiResult<(T, bool)> {
        validate_routes(routes)?;
        self.change_scope(Scope::Status(routes), f).await
    }

    async fn change_scope<T>(
        &self,
        scope: Scope<'_>,
        f: impl FnOnce(&mut Value) -> ApiResult<T>,
    ) -> ApiResult<(T, bool)> {
        match self {
            Self::Sqlite(_) => {
                self.change_observed(|workspace| {
                    let before = project(workspace, &scope)?;
                    let mut after = before.clone();
                    let result = f(&mut after)?;
                    validate_scope(&before, &after, &scope)?;
                    if after != before {
                        workspace["sync"] = after["sync"].clone();
                        for table in ["jobs", "items"] {
                            let original = rows(&before, table)?;
                            let changed = rows(&after, table)?;
                            for (position, record) in changed.iter().enumerate() {
                                if original.get(position) == Some(record) {
                                    continue;
                                }
                                if position >= original.len()
                                    && rows(workspace, table)?
                                        .iter()
                                        .any(|v| v["id"] == record["id"])
                                {
                                    return Err(internal(
                                        "Scoped append reused an existing identity",
                                    ));
                                }
                                if let Some(saved) = workspace[table]
                                    .as_array_mut()
                                    .unwrap()
                                    .iter_mut()
                                    .find(|v| v["id"] == record["id"])
                                {
                                    *saved = record.clone();
                                } else {
                                    workspace[table]
                                        .as_array_mut()
                                        .unwrap()
                                        .push(record.clone());
                                }
                            }
                        }
                    }
                    Ok(result)
                })
                .await
            }
            Self::Postgres { writer: pool, .. } => {
                let mut connection=pool.acquire().await?;
                let mut tx=sqlx::Connection::begin(&mut *connection).await?;
                let mut before=Value::Null;
                let mut after=Value::Null;
                let mut retained_record=None;
                let outcome:ApiResult<_>=async {
                retained_record=Some(sqlx::query("SELECT account,metadata::text,execution_enabled FROM communityhero.workspaces WHERE id=$1 FOR UPDATE")
                    .bind(WORKSPACE).fetch_one(&mut *tx).await?);
                let record=retained_record.as_ref().expect("metadata row retained");
                if record.try_get::<bool, _>("execution_enabled")? {
                    return Err(internal("PostgreSQL pilot execution must remain disabled"));
                }
                before = parse(record.try_get::<&str, _>("metadata")?)?;
                if !before.is_object()
                    || before["account"].as_str().is_none()
                    || record.try_get::<Option<String>, _>("account")?.as_deref()
                        != before["account"].as_str()
                {
                    return Err(internal("Workspace identity mismatch"));
                }
                for table in TABLES {
                    if before.get(table).is_some() {
                        return Err(internal("Workspace metadata contains entity collections"));
                    }
                    before[table] = json!([]);
                }
                match &scope {
                    Scope::Job(key) => {
                        before["jobs"] =
                            json!(load_records(&mut tx, "jobs", "id=$2", Some(key), None).await?);
                        if rows(&before, "jobs")?.len() != 1 {
                            return Err(internal("Job not found"));
                        }
                    }
                    Scope::Schedule | Scope::SourceClaim => {
                        // A conductor read admission must inspect its exact durable
                        // grant inside this transaction, including a paused or
                        // otherwise non-active job. Other scheduling keeps its
                        // established active-job projection and deduplication.
                        let conductor = matches!(&scope, Scope::Schedule)
                            .then(crate::conductor_authority::current_context).flatten();
                        let conductor_id=conductor.as_ref().map(|ctx|ctx.run_id.as_str());
                        before["jobs"] = json!(
                            load_records(
                                &mut tx,
                                "jobs",
                                if conductor.is_some(){"(status IN ('running','queued') OR id=$2)"}
                                else{"status IN ('running','queued')"},
                                conductor_id.as_ref(),
                                None
                            )
                            .await?
                        );
                        if matches!(&scope, Scope::SourceClaim) {
                            let records: Vec<String> = sqlx::query_scalar("SELECT COALESCE((SELECT jsonb_object_agg(e.key,e.value) FROM jsonb_each(payload) e WHERE e.key=ANY($2)), '{}'::jsonb)::text FROM communityhero.items WHERE workspace_id=$1 ORDER BY ordinal")
                                .bind(WORKSPACE).bind(SOURCE_FIELDS.to_vec()).fetch_all(&mut *tx).await?;
                            before["items"] = Value::Array(
                                records
                                    .iter()
                                    .map(|p| parse(p))
                                    .collect::<ApiResult<Vec<_>>>()?,
                            );
                        }
                    }
                    Scope::Status(routes) => {
                        before["items"] = json!(load_records(&mut tx, "items", "EXISTS (SELECT 1 FROM jsonb_array_elements($2::jsonb) r WHERE payload->>'objectId'=r->>'objectId' AND payload->>'itemId'=r->>'itemId')", None, Some(&json!(routes))).await?);
                    }
                }
                after = before.clone();
                let result = f(&mut after)?;
                validate_scope(&before, &after, &scope)?;
                if after == before {
                    return Ok((result, false));
                }
                for table in ["jobs", "items"] {
                    let old = rows(&before, table)?;
                    let new = rows(&after, table)?;
                    for (n, value) in new.iter().enumerate() {
                        if old.get(n) == Some(value) {
                            continue;
                        }
                        if n < old.len() {
                            update_record(&mut tx, table, value).await?;
                        } else {
                            append_job(&mut tx, value).await?;
                        }
                    }
                }
                if after["sync"] != before["sync"] {
                    sqlx::query("UPDATE communityhero.workspaces SET metadata=jsonb_set(metadata,'{sync}',$2::jsonb,true) WHERE id=$1")
                        .bind(WORKSPACE).bind(after["sync"].to_string()).execute(&mut *tx).await?;
                }
                Ok((result, true))
                }.await;
                let (outcome,completion)=super::pg_writer::settle(tx,outcome).await;
                super::pg_writer::release(&mut connection,pool,completion).await;
                drop(retained_record);
                outcome
            }
        }
    }
}

fn validate_routes(routes: &[Value]) -> ApiResult<()> {
    if routes.len() > STATUS_ROUTE_LIMIT {
        return Err(internal("Status scope exceeds bounded route limit"));
    }
    for route in routes {
        for field in ["objectId", "itemId"] {
            let v = text(route, field)?;
            if v.len() > 256 || v.chars().any(char::is_control) {
                return Err(internal("Invalid status route"));
            }
        }
    }
    Ok(())
}

fn project(workspace: &Value, scope: &Scope<'_>) -> ApiResult<Value> {
    let mut projected = metadata(workspace);
    for table in TABLES {
        projected[table] = json!([]);
    }
    match scope {
        Scope::Job(key) => {
            let job = rows(workspace, "jobs")?
                .iter()
                .find(|j| j["id"].as_str() == Some(key))
                .ok_or_else(|| internal("Job not found"))?;
            projected["jobs"] = json!([job]);
        }
        Scope::Schedule | Scope::SourceClaim => {
            let conductor = matches!(scope, Scope::Schedule)
                .then(crate::conductor_authority::current_context).flatten();
            projected["jobs"] = json!(
                rows(workspace, "jobs")?
                    .iter()
                    .filter(|j| matches!(j["status"].as_str(), Some("running" | "queued"))
                        || conductor.as_ref().is_some_and(|ctx|j["id"]==ctx.run_id))
                    .collect::<Vec<_>>()
            );
            if matches!(scope, Scope::SourceClaim) {
                projected["items"] = Value::Array(
                    rows(workspace, "items")?
                        .iter()
                        .map(|item| {
                            Value::Object(
                                item.as_object()
                                    .expect("validated item")
                                    .iter()
                                    .filter(|(key, _)| SOURCE_FIELDS.contains(&key.as_str()))
                                    .map(|(key, value)| (key.clone(), value.clone()))
                                    .collect(),
                            )
                        })
                        .collect(),
                );
            }
        }
        Scope::Status(routes) => {
            projected["items"] = json!(
                rows(workspace, "items")?
                    .iter()
                    .filter(|item| routes
                        .iter()
                        .any(|route| item["objectId"] == route["objectId"]
                            && item["itemId"] == route["itemId"]))
                    .collect::<Vec<_>>()
            )
        }
    }
    if matches!(scope, Scope::Status(_)) && rows(&projected, "items")?.len() > STATUS_ROUTE_LIMIT {
        return Err(internal("Status scope has too many matching records"));
    }
    Ok(projected)
}

// Compare the exact protected object members by borrowing their values. Missing
// and null remain distinct; no job, result or item body is copied then removed.
fn same_except(before: &Value, after: &Value, mutable: &[&str]) -> bool {
    let (Some(before), Some(after)) = (before.as_object(), after.as_object()) else {return false;};
    before.iter().filter(|(key, _)| !mutable.contains(&key.as_str()))
        .all(|(key, value)| after.get(key) == Some(value))
        && after.keys().filter(|key| !mutable.contains(&key.as_str()))
            .all(|key| before.contains_key(key))
}
fn validate_scope(before: &Value, after: &Value, scope: &Scope<'_>) -> ApiResult<()> {
    if !same_except(before, after, &["sync", "jobs", "items"]) || !after["sync"].is_object() {
        return Err(internal(
            "Scoped mutation changed read-only workspace state",
        ));
    }
    for table in ["jobs", "items"] {
        let mut ids = HashSet::new();
        for record in rows(after, table)? {
            if !record.is_object() || !ids.insert(text(record, "id")?) {
                return Err(internal("Invalid scoped record identity"));
            }
            for (_, key) in projection(table) {
                if !record[*key].is_null() && !record[*key].is_string() {
                    return Err(internal("Invalid scoped projection"));
                }
            }
        }
    }
    let old_jobs = rows(before, "jobs")?;
    let new_jobs = rows(after, "jobs")?;
    match scope {
        Scope::Job(_) => {
            if new_jobs.len() != 1 || before["items"] != after["items"] {
                return Err(internal("Job scope changed unrelated records"));
            }
            for key in ["id", "kind", "refId"] {
                if old_jobs[0][key] != new_jobs[0][key] {
                    return Err(internal("Job identity is immutable"));
                }
            }
        }
        Scope::Schedule | Scope::SourceClaim => {
            if !new_jobs.starts_with(old_jobs) || before["items"] != after["items"] {
                return Err(internal("Schedule scope changed existing records"));
            }
            for job in &new_jobs[old_jobs.len()..] {
                if !matches!(job["status"].as_str(), Some("running" | "queued")) {
                    return Err(internal("New claim must be active"));
                }
            }
        }
        Scope::Status(_) => {
            let old = rows(before, "items")?;
            let new = rows(after, "items")?;
            if old.len() != new.len() || old_jobs != new_jobs {
                return Err(internal("Status scope changed record membership"));
            }
            for (old, new) in old.iter().zip(new) {
                if !same_except(old, new, &["statusObservedAt", "providerStatus", "workflow", "revision"]) {
                    return Err(internal("Status scope changed protected item content"));
                }
            }
        }
    }
    // History collections above are read-only empty placeholders, never histories
    // submitted to the full-workspace history validator.
    crate::runtime_paid_result::validate_change(before, after)?;
    crate::model_material_receipt::validate_change(before, after)?;
    if matches!(scope, Scope::Job(_)) {
        crate::answering_repair_plan::validate_job_change(before, after)?;
    } else {
        crate::answering_repair_plan::validate_readonly_projection_change(before, after)?;
    }
    // Other scopes preserve all existing jobs exactly; scheduling above also
    // rejects creation of repair authority. Full closure checks belong to the
    // full-domain writer, not this explicitly incomplete projection.
    crate::conductor_authority::validate_change(before, after)?;
    crate::retained_paid_recovery_registry::validate_change(before, after, false)?;
    Ok(())
}

const SOURCE_FIELDS: [&str; 12] = [
    "id",
    "objectId",
    "itemId",
    "postKey",
    "conversationKey",
    "connectorBinding",
    "providerStatus",
    "statusObservedAt",
    "providerObservedAt",
    "workflow",
    "contextObservedAt",
    "revision",
];

async fn load_records(
    connection: &mut PgConnection,
    table: &str,
    predicate: &str,
    key: Option<&&str>,
    routes: Option<&Value>,
) -> ApiResult<Vec<Value>> {
    // table/predicate are module constants, parameters are always bound values.
    let statement = if table == "items" && routes.is_some() {
        // Deduplicate requested routes, never matching item identities. The
        // JSON text expressions intentionally preserve the existing PG route
        // comparison semantics; identity/projection validation remains below.
        // The existing items_provider_route_idx matches these equality keys.
        format!(
            "WITH requested_routes AS (SELECT DISTINCT r->>'objectId' AS object_id,r->>'itemId' AS provider_item_id FROM jsonb_array_elements($2::jsonb) r) SELECT i.id,i.payload::text{} FROM requested_routes r JOIN communityhero.items i ON i.workspace_id=$1 AND i.payload->>'objectId'=r.object_id AND i.payload->>'itemId'=r.provider_item_id ORDER BY i.ordinal LIMIT 2401",
            projection(table).iter().map(|(column, _)| format!(",i.{column}")).collect::<String>()
        )
    } else { format!(
        "SELECT id,payload::text{} FROM communityhero.{table} WHERE workspace_id=$1 AND {predicate} ORDER BY ordinal{}",
        projection(table)
            .iter()
            .map(|(column, _)| format!(",{column}"))
            .collect::<String>(),
        if table == "items" { " LIMIT 2401" } else { "" }
    ) };
    let mut query = sqlx::query(sqlx::AssertSqlSafe(statement.as_str())).bind(WORKSPACE);
    if let Some(key) = key {
        query = query.bind(*key);
    }
    if let Some(routes) = routes {
        query = query.bind(routes.to_string());
    }
    let records = query.fetch_all(connection).await?;
    if table == "items" && records.len() > STATUS_ROUTE_LIMIT {
        return Err(internal("Status scope has too many matching records"));
    }
    records
        .into_iter()
        .map(|record| {
            let payload = parse(record.try_get::<&str, _>("payload")?)?;
            if record.try_get::<&str, _>("id")? != text(&payload, "id")? {
                return Err(internal("Record identity mismatch"));
            }
            for (column, key) in projection(table) {
                if (!payload[*key].is_null() && !payload[*key].is_string())
                    || record.try_get::<Option<String>, _>(*column)?.as_deref()
                        != payload[*key].as_str()
                {
                    return Err(internal("Record relational projection mismatch"));
                }
            }
            Ok(payload)
        })
        .collect()
}

async fn update_record(connection: &mut PgConnection, table: &str, value: &Value) -> ApiResult<()> {
    let columns = projection(table);
    let statement = format!(
        "UPDATE communityhero.{table} SET payload=$3::jsonb{} WHERE workspace_id=$1 AND id=$2",
        columns
            .iter()
            .enumerate()
            .map(|(n, (column, _))| format!(",{column}=${}", n + 4))
            .collect::<String>()
    );
    let mut query = sqlx::query(sqlx::AssertSqlSafe(statement.as_str()))
        .bind(WORKSPACE)
        .bind(text(value, "id")?)
        .bind(value.to_string());
    for (_, field) in columns {
        query = query.bind(value[*field].as_str());
    }
    if query.execute(connection).await?.rows_affected() != 1 {
        return Err(internal("Scoped record disappeared"));
    }
    Ok(())
}

async fn append_job(connection: &mut PgConnection, value: &Value) -> ApiResult<()> {
    sqlx::query("INSERT INTO communityhero.jobs(workspace_id,id,ordinal,payload,kind,status,ref_id) SELECT $1,$2,COALESCE(MAX(ordinal),-1)+1,$3::jsonb,$4,$5,$6 FROM communityhero.jobs WHERE workspace_id=$1")
        .bind(WORKSPACE).bind(text(value,"id")?).bind(value.to_string()).bind(value["kind"].as_str()).bind(value["status"].as_str()).bind(value["refId"].as_str()).execute(connection).await?;
    Ok(())
}

#[cfg(test)]
#[path = "storage_writes_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "storage_writes_conductor_tests.rs"]
mod conductor_tests;

#[cfg(test)]
#[path = "storage_writes_query_candidate_tests.rs"]
mod query_candidate_tests;

#[cfg(test)]
#[path = "storage_writes_paid_history_tests.rs"]
mod paid_history_tests;
