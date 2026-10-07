//! Consistent, read-only planner evidence. Complete semantic source is required:
//! ownership settlement may inspect another captured recipient's fingerprint.
//! Unrelated paid requests and private history stay out of the reader result;
//! this projection is never a writable workspace.
use super::*;
use serde_json::json;

const COMPLETE: &[&str] = &["items", "branches", "posts", "operations", "materials", "knowledge_entries", "knowledge_versions"];
// Family discovery needs the complete media-conflict corpus, but no thread,
// paid request, proposal or operation payload. Exact capture loads those later.
const FAMILY: &[&str] = &["posts", "knowledge_entries", "knowledge_versions"];
fn family_project(workspace: &Value, recipients: &[String]) -> ApiResult<Value> {
    let mut view = claim_metadata(workspace)?;
    let selected: HashSet<_> = recipients.iter().map(String::as_str).collect();
    let current: HashSet<_> = rows(workspace,"knowledge_entries")?.iter()
        .filter_map(|entry|entry["currentVersionId"].as_str()).collect();
    for table in TABLES {
        view[table] = if table == "items" {
            json!(rows(workspace,table)?.iter().filter(|item|item["id"].as_str().is_some_and(|id|selected.contains(id))).collect::<Vec<_>>())
        } else if table == "knowledge_versions" {
            json!(rows(workspace,table)?.iter().filter(|version|version["id"].as_str().is_some_and(|id|current.contains(id))).collect::<Vec<_>>())
        } else if FAMILY.contains(&table) { workspace[table].clone() } else { json!([]) };
    }
    Ok(view)
}
fn media_job(job: &Value) -> bool {
    matches!(job["kind"].as_str(), Some("media" | "media_audio" | "media_analysis" | "media_analysis_applicability"))
}
// Planner media readiness consumes full ledger/applicability witnesses; scope
// controls below retain unrelated paid ownership without loading paid bodies.
const PG_MEDIA_JOBS:&str="(kind IN ('media','media_audio','media_analysis','media_analysis_applicability') OR payload->>'kind' IN ('media','media_audio','media_analysis','media_analysis_applicability'))";
fn project(workspace: &Value, recipients: &[String]) -> ApiResult<Value> {
    let mut view = claim_metadata(workspace)?;
    for table in TABLES {
        view[table] = match table {
            "proposals" => json!(rows(workspace, table)?.iter().filter(|proposal|
                proposal["itemId"].as_str().is_some_and(|id| recipients.iter().any(|r| r == id))).collect::<Vec<_>>()),
            "jobs" => json!(rows(workspace, table)?.iter().filter(|job| media_job(job)).collect::<Vec<_>>()),
            _ if COMPLETE.contains(&table) => workspace[table].clone(),
            _ => json!([]),
        };
    }
    super::super::scope_context::project(workspace, &mut view)?;
    Ok(view)
}

async fn load(connection: &mut PgConnection, recipients: &[String], family: bool) -> ApiResult<Value> {
    let record = sqlx::query("SELECT account, execution_enabled, metadata::text FROM communityhero.workspaces WHERE id=$1")
        .bind(WORKSPACE).fetch_one(&mut *connection).await?;
    if record.try_get::<bool, _>("execution_enabled")? {
        return Err(internal("PostgreSQL pilot execution must remain disabled"));
    }
    let metadata = parse(record.try_get::<&str, _>("metadata")?)?;
    if TABLES.iter().any(|table| metadata.get(*table).is_some()) {
        return Err(internal("Workspace metadata contains entity collections"));
    }
    let mut view = claim_metadata(&metadata)?;
    if record.try_get::<Option<String>, _>("account")?.as_deref() != view["account"].as_str() {
        return Err(internal("Workspace identity mismatch"));
    }
    for table in TABLES {
        view[table] = json!([]);
        if family {
            if !FAMILY.contains(&table) && table != "items" { continue; }
        } else if !COMPLETE.contains(&table) && !["items", "branches", "proposals", "jobs"].contains(&table) { continue; }
        let columns = projection(table);
        let mut binds_recipients = false;
        let condition = match table {
            "items" if family => { binds_recipients = true; "id=ANY($2)".to_owned() },
            "knowledge_versions" if family => "id IN (SELECT current_version_id FROM communityhero.knowledge_entries WHERE workspace_id=$1)".to_owned(),
            "proposals" => { binds_recipients = true; scoped_condition(table).unwrap() },
            "jobs" => PG_MEDIA_JOBS.to_owned(),
            _ => "TRUE".to_owned(),
        };
        let statement = format!("SELECT id,payload::text{} FROM communityhero.{table} WHERE workspace_id=$1 AND {condition} ORDER BY ordinal",
            columns.iter().map(|(column, _)| format!(",{column}")).collect::<String>());
        let mut query = sqlx::query(sqlx::AssertSqlSafe(statement.as_str())).bind(WORKSPACE);
        if binds_recipients { query = query.bind(recipients); }
        let fetch = crate::performance::Span::new("preparation.plan.entities.fetch");
        let records = query.fetch_all(&mut *connection).await?;
        drop(fetch);
        let decode = crate::performance::Span::new("preparation.plan.entities.decode");
        let mut values = Vec::with_capacity(records.len());
        let mut seen = HashSet::new();
        for record in records {
            let value = parse(record.try_get::<&str, _>("payload")?)?;
            let id = text(&value, "id")?;
            if record.try_get::<&str, _>("id")? != id || !seen.insert(id.to_owned()) {
                return Err(internal("Preparation plan record identity mismatch"));
            }
            for (column, field) in columns {
                if (!value[*field].is_null() && !value[*field].is_string())
                    || record.try_get::<Option<String>, _>(*column)?.as_deref() != value[*field].as_str() {
                    return Err(internal("Preparation plan record projection mismatch"));
                }
            }
            values.push(value);
        }
        view[table] = json!(values);
        drop(decode);
    }
    if !family { super::super::scope_context::load(connection, &mut view).await?; }
    Ok(view)
}

impl Database {
    pub(crate) async fn read_preparation_families(&self, recipients: &[String]) -> ApiResult<Value> {
        if recipients.is_empty() || recipients.len() > 5000 {
            return Err(crate::bad("Family selection requires 1 to 5000 recipients"));
        }
        match self {
            Self::Sqlite(_) => family_project(&self.read().await?,recipients),
            Self::Postgres { reader, .. } => {
                let mut tx=reader.begin().await?;
                sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ READ ONLY").execute(&mut *tx).await?;
                let view=load(&mut tx,recipients,true).await?;
                tx.commit().await?;
                Ok(view)
            }
        }
    }
    /// Advisory only. Scheduling recaptures under the writer and repeats every
    /// source/operation/ownership gate; this snapshot grants no spend or send.
    pub(crate) async fn read_preparation_plan(&self, recipients: &[String]) -> ApiResult<Value> {
        let _total = crate::performance::Span::new("preparation.plan.read");
        if recipients.is_empty() || recipients.len() > 100 {
            return Err(crate::bad("Preparation plan requires 1 to 100 recipients"));
        }
        match self {
            Self::Sqlite(_) => project(&self.read().await?, recipients),
            Self::Postgres { reader, .. } => {
                let mut tx = reader.begin().await?;
                sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ READ ONLY").execute(&mut *tx).await?;
                let view = load(&mut tx, recipients, false).await?;
                tx.commit().await?;
                Ok(view)
            }
        }
    }
}

#[cfg(test)]
#[path = "storage_preparation_plan_tests.rs"]
mod tests;

#[cfg(test)]
#[path="storage_preparation_media_projection_tests.rs"]
mod media_projection_tests;
