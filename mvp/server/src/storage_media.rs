//! Scoped media queue transaction. Keep historical media attempts visible while
//! excluding branch text, assistant evidence and unrelated jobs from each tick.
use super::*;
use serde_json::json;

const ITEM_FIELDS: [&str; 10] = [
    "id", "objectId", "itemId", "connectorBinding", "postId", "postKey",
    "conversationKey", "providerStatus", "workflow", "branchId",
];
const COLLECTIONS: [&str; 5] = ["posts", "items", "jobs", "knowledge_entries", "knowledge_versions"];
const JOB_MUTABLE: [&str; 7] = [
    "status", "startedAt", "finishedAt", "error", "result", "refId", "sourceAttempts",
];

impl Database {
    /// Narrow read-only projection used to warm immutable visual proofs outside
    /// any workspace writer transaction. No branch/history/job payload loading.
    pub(crate) async fn read_media_visual_evidence(&self)->ApiResult<Vec<Value>> {
        match self {
            Self::Sqlite(_)=>Ok(self.read().await?["knowledge_versions"].as_array().into_iter().flatten().filter_map(|v|v.get("visualEvidence").filter(|e|e["schemaVersion"]==2).cloned()).collect()),
            Self::Postgres{reader,..}=>{
                let records=sqlx::query("SELECT (v.payload->'visualEvidence')::text AS evidence FROM communityhero.knowledge_versions v JOIN communityhero.knowledge_entries e ON e.workspace_id=v.workspace_id AND e.payload->>'currentVersionId'=v.id WHERE v.workspace_id=$1 AND v.payload->'visualEvidence'->>'schemaVersion'='2'").bind(WORKSPACE).fetch_all(reader).await?;
                records.into_iter().map(|r|parse(r.try_get::<&str,_>("evidence")?)).collect()
            }
        }
    }
    pub(crate) async fn change_media_observed<T>(
        &self,
        f: impl FnOnce(&mut Value) -> ApiResult<T>,
    ) -> ApiResult<(T, bool)> {
        match self {
            Self::Sqlite(_) => self.change_observed(|workspace| {
                let before = media_projection(workspace)?;
                let mut after = before.clone();
                let result = f(&mut after)?;
                validate_media_change(&before, &after)?;
                if before != after {
                    workspace["mediaQueue"] = after["mediaQueue"].clone();
                    apply_jobs(workspace, &before["jobs"], &after["jobs"])?;
                }
                Ok(result)
            }).await,
            Self::Postgres { writer, .. } => {
                let mut tx = writer.begin().await?;
                let record = sqlx::query("SELECT account, execution_enabled, (jsonb_build_object('account',metadata->'account') || CASE WHEN metadata ? 'connectorBinding' THEN jsonb_build_object('connectorBinding',metadata->'connectorBinding') ELSE '{}'::jsonb END || CASE WHEN metadata ? 'mediaQueue' THEN jsonb_build_object('mediaQueue',metadata->'mediaQueue') ELSE '{}'::jsonb END)::text AS media_metadata FROM communityhero.workspaces WHERE id=$1 FOR UPDATE")
                    .bind(WORKSPACE).fetch_one(&mut *tx).await?;
                if record.try_get::<bool, _>("execution_enabled")? {
                    return Err(internal("PostgreSQL pilot execution must remain disabled"));
                }
                let mut before = parse(record.try_get::<&str, _>("media_metadata")?)?;
                if record.try_get::<Option<String>, _>("account")?.as_deref() != before["account"].as_str() {
                    return Err(internal("Workspace identity mismatch"));
                }
                for table in COLLECTIONS { before[table] = json!([]); }
                before["posts"] = json!(media_rows(&mut tx,"posts",None).await?);
                before["items"] = json!(media_rows(&mut tx,"items",Some(&ITEM_FIELDS)).await?);
                before["jobs"] = json!(media_rows(&mut tx,"jobs",None).await?);
                before["knowledge_entries"] = json!(media_rows(&mut tx,"knowledge_entries",None).await?);
                before["knowledge_versions"] = json!(media_rows(&mut tx,"knowledge_versions",None).await?);
                let mut after = before.clone();
                let result = f(&mut after)?;
                validate_media_change(&before,&after)?;
                if after == before { tx.commit().await?; return Ok((result,false)); }
                let old = rows(&before,"jobs")?;
                let new = rows(&after,"jobs")?;
                for (position, job) in new.iter().enumerate() {
                    if old.get(position) == Some(job) { continue; }
                    if position < old.len() {
                        let updated = sqlx::query("UPDATE communityhero.jobs SET payload=$3::jsonb,status=$4,ref_id=$5 WHERE workspace_id=$1 AND id=$2")
                            .bind(WORKSPACE).bind(text(job,"id")?).bind(job.to_string())
                            .bind(job["status"].as_str()).bind(job["refId"].as_str())
                            .execute(&mut *tx).await?;
                        if updated.rows_affected() != 1 { return Err(internal("Media job disappeared")); }
                    } else {
                        sqlx::query("INSERT INTO communityhero.jobs(workspace_id,id,ordinal,payload,kind,status,ref_id) SELECT $1,$2,COALESCE(MAX(ordinal),-1)+1,$3::jsonb,$4,$5,$6 FROM communityhero.jobs WHERE workspace_id=$1")
                            .bind(WORKSPACE).bind(text(job,"id")?).bind(job.to_string())
                            .bind(job["kind"].as_str()).bind(job["status"].as_str())
                            .bind(job["refId"].as_str()).execute(&mut *tx).await?;
                    }
                }
                if after["mediaQueue"] != before["mediaQueue"] {
                    sqlx::query("UPDATE communityhero.workspaces SET metadata=jsonb_set(metadata,'{mediaQueue}',$2::jsonb,true) WHERE id=$1")
                        .bind(WORKSPACE).bind(after["mediaQueue"].to_string()).execute(&mut *tx).await?;
                }
                tx.commit().await?;
                Ok((result,true))
            }
        }
    }
}

fn media_projection(workspace:&Value)->ApiResult<Value>{
    let mut view=json!({"account":workspace["account"]});
    for key in ["connectorBinding","mediaQueue"] {
        if let Some(value)=workspace.get(key) {view[key]=value.clone();}
    }
    view["posts"]=workspace["posts"].clone();
    view["items"]=Value::Array(rows(workspace,"items")?.iter().map(|item| {
        Value::Object(item.as_object().expect("validated item").iter()
            .filter(|(key,_)| ITEM_FIELDS.contains(&key.as_str()))
            .map(|(key,value)|(key.clone(),value.clone())).collect())
    }).collect());
    view["jobs"]=json!(rows(workspace,"jobs")?.iter().filter(|job|job["kind"]=="media").collect::<Vec<_>>());
    view["knowledge_entries"]=workspace["knowledge_entries"].clone();
    view["knowledge_versions"]=workspace["knowledge_versions"].clone();
    Ok(view)
}

async fn media_rows(tx:&mut PgConnection,table:&str,fields:Option<&[&str]>)->ApiResult<Vec<Value>>{
    let predicate=if table=="jobs" {" AND kind='media'"} else {""};
    let columns=projection(table).iter().map(|(column,_)|format!(",{column}")).collect::<String>();
    let records=if let Some(fields)=fields {
        let statement=format!("SELECT id,COALESCE((SELECT jsonb_object_agg(e.key,e.value) FROM jsonb_each(payload) e WHERE e.key=ANY($2)),'{{}}'::jsonb)::text AS payload{columns} FROM communityhero.{table} WHERE workspace_id=$1{predicate} ORDER BY ordinal");
        sqlx::query(sqlx::AssertSqlSafe(statement.as_str()))
            .bind(WORKSPACE).bind(fields.to_vec()).fetch_all(&mut *tx).await?
    } else {
        let statement=format!("SELECT id,payload::text AS payload{columns} FROM communityhero.{table} WHERE workspace_id=$1{predicate} ORDER BY ordinal");
        sqlx::query(sqlx::AssertSqlSafe(statement.as_str()))
            .bind(WORKSPACE).fetch_all(&mut *tx).await?
    };
    records.into_iter().map(|record|{
        let value=parse(record.try_get::<&str,_>("payload")?)?;
        if !value.is_object() || record.try_get::<&str,_>("id")? != text(&value,"id")? {
            return Err(internal("Media record identity mismatch"));
        }
        for (column,key) in projection(table) {
            if record.try_get::<Option<String>,_>(*column)?.as_deref()!=value[*key].as_str() {
                return Err(internal("Media record relational projection mismatch"));
            }
        }
        Ok(value)
    }).collect()
}

fn apply_jobs(workspace:&mut Value,before:&Value,after:&Value)->ApiResult<()> {
    let old=before.as_array().ok_or_else(||internal("Invalid media job projection"))?;
    let new=after.as_array().ok_or_else(||internal("Invalid media job projection"))?;
    for (position,job) in new.iter().enumerate() {
        if old.get(position)==Some(job) {continue;}
        if position>=old.len() && job_rows_mut(workspace)?.iter().any(|v|v["id"]==job["id"]) {
            return Err(internal("Media job identity already exists"));
        }
        if let Some(saved)=job_rows_mut(workspace)?.iter_mut().find(|v|v["id"]==job["id"]) {
            *saved=job.clone();
        } else {job_rows_mut(workspace)?.push(job.clone());}
    }
    Ok(())
}

fn job_rows_mut(workspace:&mut Value)->ApiResult<&mut Vec<Value>> {
    workspace["jobs"].as_array_mut().ok_or_else(||internal("Invalid workspace jobs"))
}

fn validate_media_change(before:&Value,after:&Value)->ApiResult<()> {
    let mut unchanged_before=before.clone();
    let mut unchanged_after=after.clone();
    for key in ["jobs","mediaQueue"] {
        unchanged_before.as_object_mut().unwrap().remove(key);
        unchanged_after.as_object_mut().ok_or_else(||internal("Invalid media projection"))?.remove(key);
    }
    let valid_digest=after["mediaQueue"]==before["mediaQueue"] || after["mediaQueue"].as_object().is_some_and(|queue| queue.len()==1
        && queue.get("inputDigest").and_then(Value::as_str).is_some_and(|digest|digest.len()==64
            && digest.bytes().all(|b|b.is_ascii_hexdigit())));
    if unchanged_before!=unchanged_after || !valid_digest {
        return Err(internal("Media transaction changed read-only workspace state"));
    }
    let old=rows(before,"jobs")?;
    let new=rows(after,"jobs")?;
    if new.len()<old.len() {return Err(internal("Media transaction removed a job"));}
    let mut ids=HashSet::new();
    for (position,job) in new.iter().enumerate() {
        if !job.is_object() || !ids.insert(text(job,"id")?) || job["kind"]!="media" {
            return Err(internal("Invalid media job identity"));
        }
        if let Some(prior)=old.get(position) {
            if job["id"]!=prior["id"] || job["kind"]!=prior["kind"] || job["purpose"]!=prior["purpose"] {
                return Err(internal("Media job identity changed"));
            }
            if job==prior { continue; }
            if job["purpose"]!="auto_media" {
                return Err(internal("Media transaction changed foreign job"));
            }
            let mut protected_before=prior.clone();
            let mut protected_after=job.clone();
            for key in JOB_MUTABLE {
                protected_before.as_object_mut().unwrap().remove(key);
                protected_after.as_object_mut().unwrap().remove(key);
            }
            if protected_before!=protected_after {
                return Err(internal("Media transaction changed protected job content"));
            }
            if job.get("sourceAttempts")!=prior.get("sourceAttempts") {
                if prior.get("sourceAttempts").is_some_and(|v|!v.is_null()&&!v.is_array()) {
                    return Err(internal("Invalid prior media attempt history"));
                }
                let prior_attempts=prior["sourceAttempts"].as_array().map(Vec::as_slice).unwrap_or(&[]);
                let attempts=rows(job,"sourceAttempts")?;
                if !attempts.starts_with(prior_attempts) || attempts.len()!=prior_attempts.len()+1 {
                    return Err(internal("Media transaction changed source attempt history"));
                }
                if attempts.last().unwrap()["status"]!="running" {
                    return Err(internal("Media transaction appended invalid source attempt"));
                }
            }
        } else if job["purpose"]!="auto_media" || !matches!(job["status"].as_str(),Some("queued"|"failed"|"running"))
            || (job["status"]=="running" && (rows(job,"sourceAttempts")?.len()!=1
                || job["sourceAttempts"][0]["status"]!="running"))
            || (job["status"]!="running" && !rows(job,"sourceAttempts")?.is_empty()) {
            return Err(internal("Invalid new media queue job"));
        }
    }
    Ok(())
}

#[cfg(test)]
#[path="storage_media_tests.rs"]
mod tests;
