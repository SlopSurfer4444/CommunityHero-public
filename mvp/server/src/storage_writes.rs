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

impl Database {
    /// Source scheduling needs routing/status tokens, never comment text, draft,
    /// branch evidence, or historical job payloads. The item projection is readonly.
    pub(crate) async fn change_source_claim_observed<T>(
        &self,
        f: impl FnOnce(&mut Value) -> ApiResult<T>,
    ) -> ApiResult<(T, bool)> {
        self.change_scope(Scope::SourceClaim, f).await
    }

    pub(crate) async fn read_source_status(&self) -> ApiResult<Value> {
        self.change_scope(Scope::SourceClaim, |d| Ok(d.clone()))
            .await
            .map(|(value, _)| value)
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

    /// At most 800 explicit provider routes. Only their status/workflow clocks,
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
                let mut tx = pool.begin().await?;
                let record = sqlx::query("SELECT account,metadata::text,execution_enabled FROM communityhero.workspaces WHERE id=$1 FOR UPDATE")
                    .bind(WORKSPACE).fetch_one(&mut *tx).await?;
                if record.try_get::<bool, _>("execution_enabled")? {
                    return Err(internal("PostgreSQL pilot execution must remain disabled"));
                }
                let mut before = parse(record.try_get::<&str, _>("metadata")?)?;
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
                        before["jobs"] = json!(
                            load_records(
                                &mut tx,
                                "jobs",
                                "status IN ('running','queued')",
                                None,
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
                let mut after = before.clone();
                let result = f(&mut after)?;
                validate_scope(&before, &after, &scope)?;
                if after == before {
                    tx.commit().await?;
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
                tx.commit().await?;
                Ok((result, true))
            }
        }
    }
}

fn validate_routes(routes: &[Value]) -> ApiResult<()> {
    if routes.len() > 800 {
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
            projected["jobs"] = json!(
                rows(workspace, "jobs")?
                    .iter()
                    .filter(|j| matches!(j["status"].as_str(), Some("running" | "queued")))
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
    if matches!(scope, Scope::Status(_)) && rows(&projected, "items")?.len() > 800 {
        return Err(internal("Status scope has too many matching records"));
    }
    Ok(projected)
}

fn validate_scope(before: &Value, after: &Value, scope: &Scope<'_>) -> ApiResult<()> {
    let mut readonly_before = before.clone();
    let mut readonly_after = after.clone();
    for key in ["sync", "jobs", "items"] {
        readonly_before.as_object_mut().unwrap().remove(key);
        readonly_after
            .as_object_mut()
            .ok_or_else(|| internal("Invalid scoped workspace"))?
            .remove(key);
    }
    if readonly_before != readonly_after || !after["sync"].is_object() {
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
                let mut old = old.clone();
                let mut new = new.clone();
                for field in ["statusObservedAt", "providerStatus", "workflow", "revision"] {
                    old.as_object_mut().unwrap().remove(field);
                    new.as_object_mut().unwrap().remove(field);
                }
                if old != new {
                    return Err(internal("Status scope changed protected item content"));
                }
            }
        }
    }
    // History collections above are read-only empty placeholders, never histories
    // submitted to the full-workspace history validator.
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
    let statement = format!(
        "SELECT id,payload::text{} FROM communityhero.{table} WHERE workspace_id=$1 AND {predicate} ORDER BY ordinal{}",
        projection(table)
            .iter()
            .map(|(column, _)| format!(",{column}"))
            .collect::<String>(),
        if table == "items" { " LIMIT 801" } else { "" }
    );
    let mut query = sqlx::query(sqlx::AssertSqlSafe(statement.as_str())).bind(WORKSPACE);
    if let Some(key) = key {
        query = query.bind(*key);
    }
    if let Some(routes) = routes {
        query = query.bind(routes.to_string());
    }
    let records = query.fetch_all(connection).await?;
    if table == "items" && records.len() > 800 {
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
