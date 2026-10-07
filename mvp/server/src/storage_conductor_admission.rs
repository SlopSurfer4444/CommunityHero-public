//! Campaign start uses the established leased writer/workspace lock, without
//! hydrating source history, model bundles or operation evidence. This reducer
//! may append one grant job; all captured context and prior jobs are immutable.
use super::*;
use serde_json::json;
use crate::operator_auth::Actor;

fn scope_ids(body:&Value)->ApiResult<Vec<String>> {
    body["scope"]["itemIds"].as_array().filter(|ids|!ids.is_empty()&&ids.len()<=5000)
        .ok_or_else(||internal("Invalid conductor admission manifest"))?
        .iter().map(|id|id.as_str().map(str::to_owned).ok_or_else(||internal("Invalid conductor item identity"))).collect()
}
fn project(workspace:&Value,body:&Value)->ApiResult<Value> {
    let ids:HashSet<_>=scope_ids(body)?.into_iter().collect();
    let mut view=metadata(workspace);
    for table in TABLES {view[table]=json!([]);}
    view["items"]=json!(rows(workspace,"items")?.iter().filter(|item|item["id"].as_str().is_some_and(|id|ids.contains(id))).collect::<Vec<_>>());
    view["jobs"]=json!(rows(workspace,"jobs")?.iter().filter(|job|job["kind"]=="conductor").collect::<Vec<_>>());
    Ok(view)
}
fn validate_delta(before:&Value,after:&Value,body:&Value,actor:&Actor,hash:&str)->ApiResult<()> {
    if metadata(before)!=metadata(after) {return Err(internal("Conductor start changed workspace metadata"));}
    for table in TABLES {if table!="jobs"&&before[table]!=after[table]{return Err(internal("Conductor start changed read-only context"));}}
    let old=rows(before,"jobs")?;let new=rows(after,"jobs")?;
    if !new.starts_with(old)||new.len()>old.len()+1 {return Err(internal("Conductor start rewrote job history"));}
    crate::conductor_authority::validate_change(before,after)?;
    crate::retained_paid_recovery_registry::validate_change(before,after,false)?;
    if new.len()==old.len(){return Ok(());}
    if old.iter().any(|job|job["refId"]==body["requestId"]
        ||matches!(job["conductor"]["desiredState"].as_str(),Some("running"|"pausing"))){
        return Err(crate::conflict("Conductor start conflicts with its current admission"));
    }
    let job=new.last().unwrap();let key=text(job,"id")?;
    if key.is_empty()||old.iter().any(|old|old["id"]==key)
        ||job.as_object().is_none_or(|fields|fields.len()!=9||fields.keys().any(|key|!matches!(key.as_str(),"id"|"kind"|"refId"|"status"|"createdAt"|"purpose"|"account"|"connectorBinding"|"conductor")))
        ||job["kind"]!="conductor"||job["refId"]!=body["requestId"]||job["status"]!="running"
        ||job["purpose"]!="autonomous_conductor"||job["account"]!=before["account"]
        ||job["connectorBinding"]!=crate::active_binding(before)?.to_json()
        ||chrono::DateTime::parse_from_rfc3339(text(job,"createdAt")?).is_err(){
        return Err(internal("Conductor start appended an invalid grant job"));
    }
    let expected=json!({"version":1,"desiredState":"running","leaseGeneration":1,"mode":body["mode"],
        "scope":body["scope"],"limits":body["limits"],"grant":crate::conductor_authority::create_grant(before,actor,body)?,
        "startPayloadHash":hash,"childEverStarted":false,"checkpoint":{"relativePath":format!("conductor/{key}/queue.json")},
        "progress":null,"itemHolds":[]});
    if job["conductor"]!=expected {return Err(internal("Conductor start changed captured campaign authority"));}
    if let Some(cutoff)=body["scope"]["cutoffUtc"].as_str(){
        let cutoff=chrono::DateTime::parse_from_rfc3339(cutoff).map_err(|_|crate::bad("Invalid conductor cutoff"))?;
        for id in scope_ids(body)? {
            let item=crate::row(before,"items",&id)?;
            let created=chrono::DateTime::parse_from_rfc3339(crate::required(item,"createdAt")?).map_err(|_|crate::bad("Campaign item has no valid creation date"))?;
            if created>cutoff{return Err(crate::conflict("Campaign manifest exceeds its cutoff"));}
        }
    }
    Ok(())
}
fn decode(table:&str,records:Vec<sqlx::postgres::PgRow>)->ApiResult<Vec<Value>> {
    records.into_iter().map(|record|{
        let value=parse(record.try_get::<&str,_>("payload")?)?;
        if record.try_get::<&str,_>("id")?!=text(&value,"id")? {return Err(internal("Conductor admission record identity mismatch"));}
        for(column,field)in projection(table){
            if(!value[*field].is_null()&&!value[*field].is_string())
                ||record.try_get::<Option<String>,_>(*column)?.as_deref()!=value[*field].as_str(){return Err(internal("Conductor admission relational projection mismatch"));}
        }
        Ok(value)
    }).collect()
}
async fn load(connection:&mut PgConnection,body:&Value)->ApiResult<Value> {
    let record=sqlx::query("SELECT account,metadata::text,execution_enabled FROM communityhero.workspaces WHERE id=$1 FOR UPDATE")
        .bind(WORKSPACE).fetch_one(&mut *connection).await?;
    if record.try_get::<bool,_>("execution_enabled")? {return Err(internal("PostgreSQL pilot execution must remain disabled"));}
    let mut view=parse(record.try_get::<&str,_>("metadata")?)?;
    if !view.is_object()||view["account"].as_str().is_none()
        ||record.try_get::<Option<String>,_>("account")?.as_deref()!=view["account"].as_str(){return Err(internal("Workspace identity mismatch"));}
    for table in TABLES {if view.get(table).is_some(){return Err(internal("Workspace metadata contains entity collections"));}view[table]=json!([]);}
    let items=sqlx::query("SELECT id,post_id,branch_id,payload::text FROM communityhero.items WHERE workspace_id=$1 AND (id=ANY($2::text[]) OR payload->>'id'=ANY($2::text[])) ORDER BY ordinal")
        .bind(WORKSPACE).bind(scope_ids(body)?).fetch_all(&mut *connection).await?;
    view["items"]=json!(decode("items",items)?);
    // Include BOTH relational and payload discriminators. A corrupt projection
    // must be rejected rather than hide a replay or a current active grant.
    let jobs=sqlx::query("SELECT id,kind,status,ref_id,payload::text FROM communityhero.jobs WHERE workspace_id=$1 AND (kind='conductor' OR payload->>'kind'='conductor') ORDER BY ordinal")
        .bind(WORKSPACE).fetch_all(connection).await?;
    view["jobs"]=json!(decode("jobs",jobs)?);Ok(view)
}
impl Database {
    pub(crate) async fn change_conductor_start_observed<T>(&self,body:&Value,actor:&Actor,hash:&str,f:impl FnOnce(&mut Value)->ApiResult<T>)->ApiResult<(T,bool)> {
        let _total=crate::performance::Span::new("conductor.start.scoped.total");
        match self {
            Self::Sqlite(_)=>self.change_observed(|workspace|{
                let before=project(workspace,body)?;let mut after=before.clone();let result=f(&mut after)?;
                validate_delta(&before,&after,body,actor,hash)?;
                for job in &rows(&after,"jobs")?[rows(&before,"jobs")?.len()..]{
                    if rows(workspace,"jobs")?.iter().any(|old|old["id"]==job["id"]){return Err(internal("Conductor append reused job identity"));}
                    crate::list_mut(workspace,"jobs").push(job.clone());
                }
                Ok(result)
            }).await,
            Self::Postgres{writer,..}=>{
                let waiting=crate::performance::Span::new("conductor.start.pool_wait");let mut tx=writer.begin().await?;drop(waiting);
                let loading=crate::performance::Span::new("conductor.start.load");let before=load(&mut tx,body).await?;drop(loading);
                let domain=crate::performance::Span::new("conductor.start.domain");let mut after=before.clone();let result=f(&mut after)?;drop(domain);
                validate_delta(&before,&after,body,actor,hash)?;
                let changed=before!=after;
                for job in &rows(&after,"jobs")?[rows(&before,"jobs")?.len()..]{
                    // This ordinal belongs to the WHOLE durable jobs table,
                    // not the filtered conductor history loaded above.
                    sqlx::query("INSERT INTO communityhero.jobs(workspace_id,id,kind,status,ref_id,payload,ordinal) SELECT $1,$2,$3,$4,$5,$6::jsonb,COALESCE(MAX(ordinal),-1)+1 FROM communityhero.jobs WHERE workspace_id=$1")
                        .bind(WORKSPACE).bind(text(job,"id")?).bind(job["kind"].as_str()).bind(job["status"].as_str()).bind(job["refId"].as_str()).bind(job.to_string()).execute(&mut *tx).await?;
                }
                tx.commit().await?;Ok((result,changed))
            }
        }
    }
}
#[cfg(test)]
#[path="storage_conductor_admission_tests.rs"]
mod tests;
