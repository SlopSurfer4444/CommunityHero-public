//! Fresh assistant projections and delta-only persistence. This is not a partial
//! workspace snapshot eligible for replacement: omitted history is never written.
//! Corpus callers retain all source/domain evidence. Dialogue callers may omit
//! comment evidence only while no attached, retained or tool-read roots exist.
use super::*;
use serde_json::json;

const READONLY: &[&str] = &["posts","branches","operations","materials","knowledge_entries","knowledge_versions","approvals"];
const WRITABLE: &[&str] = &["items","conversations","jobs","proposals","audit","feedback"];
const ACTIVE_PROPOSAL: &[&str] = &["draft","approved","dispatching","unknown"];
const EMPTY_DIALOGUE_TABLES: &[&str] = &["items","branches","proposals","operations","materials"];

#[derive(Clone,Copy)]
enum Scope<'a> { Corpus, Dialogue(&'a [Value]) }

// Nonempty or malformed retained evidence fails open to the complete corpus,
// never to a truncated source graph. A prepared multi-item proposal can depend
// on other recipients, cross-post author history and global media conflicts.
fn has_roots(value:&Value)->bool { !value.is_null() && !value.as_array().is_some_and(Vec::is_empty) }
fn empty_dialogue(d:&Value,job:Option<&str>,conversation:&str,scope:Scope<'_>)->bool {
    let Scope::Dialogue(extra)=scope else{return false;};
    if !extra.is_empty(){return false;}
    let chats=crate::list(d,"conversations");
    let Some(chat)=chats.iter().find(|c|c["id"]==conversation) else{return false;};
    if has_roots(&chat["itemIds"])||has_roots(&chat["actionReviews"]){return false;}
    !crate::list(d,"jobs").iter().filter(|j|job==j["id"].as_str()).any(|j|
        has_roots(&j["prepareBundle"]["itemIds"])||has_roots(&j["toolResults"])||has_roots(&j["prepareBundle"]["request"]["toolResults"]))
}

fn dependency_roots(d:&Value,job:Option<&str>,conversation:&str)->Vec<Value>{
    let mut roots=crate::list(d,"proposals").iter().filter(|p|ACTIVE_PROPOSAL.contains(&p["status"].as_str().unwrap_or(""))).cloned().collect::<Vec<_>>();
    for selected in crate::list(d,"jobs").iter().filter(|j|job==j["id"].as_str()||(j["kind"]=="assistant"&&matches!(j["status"].as_str(),Some("running"|"queued")))) {
        roots.push(json!({"prepareRunId":selected["id"]}));
    }
    for manual in crate::list(d,"jobs").iter().filter(|j|j["purpose"]=="manual_video_frames") {roots.push(json!({"prepareRunId":manual["id"]}));}
    if let Some(job)=job{roots.push(json!({"prepareRunId":job}));}
    // Preserve frozen reviewed proposals and retained operation proof roots as
    // well as live drafts. Traverse exact carriers, never retarget their IDs.
    for operation in crate::list(d,"operations") {roots.push(operation.clone());}
    for chat in crate::list(d,"conversations").iter().filter(|chat|chat["id"]==conversation) {if let Some(reviews)=chat.get("actionReviews"){roots.push(reviews.clone());}}
    roots
}
fn required_jobs(d:&Value,job:Option<&str>,conversation:&str)->HashSet<String>{
    let roots=dependency_roots(d,job,conversation);let mut jobs=Vec::new();
    loop{
        let(ids,bundles,full)=super::source_snapshot::scoped_job_dependencies(&jobs,&roots);
        let next=crate::list(d,"jobs").iter().filter(|j|full||j["id"].as_str().is_some_and(|id|ids.iter().any(|selected|selected==id))||j["prepareBundle"]["id"].as_str().is_some_and(|id|bundles.iter().any(|selected|selected==id))).cloned().collect::<Vec<_>>();
        if next==jobs{return next.iter().filter_map(|j|j["id"].as_str().map(str::to_owned)).collect();}
        jobs=next;
    }
}
fn projected(d:&Value,job:Option<&str>,conversation:&str)->ApiResult<Value>{
    projected_scope(d,job,conversation,Scope::Corpus)
}
fn projected_scope(d:&Value,job:Option<&str>,conversation:&str,scope:Scope<'_>)->ApiResult<Value>{
    let empty=empty_dialogue(d,job,conversation,scope);
    let selected=if empty{job.into_iter().map(str::to_owned).collect()}else{required_jobs(d,job,conversation)};
    let mut value=metadata(d);
    for table in TABLES{
        if empty&&EMPTY_DIALOGUE_TABLES.contains(&table){value[table]=json!([]);continue;}
        value[table]=match table {
            "jobs"=>json!(rows(d,table)?.iter().filter(|j|selected.contains(j["id"].as_str().unwrap_or(""))||(j["kind"]=="assistant"&&matches!(j["status"].as_str(),Some("running"|"queued")))).collect::<Vec<_>>()),
            "conversations"=>json!(rows(d,table)?.iter().filter(|c|c["id"]==conversation).collect::<Vec<_>>()),
            "audit"|"feedback"|"approvals"=>json!([]),
            _=>d[table].clone()
        };
    }
    validate_roots(&value,job,conversation)?;
    if !empty {super::scope_context::project(d,&mut value)?;}
    Ok(value)
}
fn validate_roots(value:&Value,job:Option<&str>,conversation:&str)->ApiResult<()> {
    if rows(value,"conversations")?.len()!=1||value["conversations"][0]["id"]!=conversation{return Err(internal("Assistant conversation not found"));}
    if let Some(id)=job{if !rows(value,"jobs")?.iter().any(|j|j["id"]==id){return Err(internal("Assistant job not found"));}}
    Ok(())
}

// SQLite still parses its document internally. JSON1 excludes unrelated payloads
// from the Rust result; writes merge deltas back into its complete document.
const SQLITE_ASSISTANT:&str=r#"
WITH w AS (SELECT payload, (?3 AND NOT EXISTS (
 SELECT 1 FROM json_each(payload,'$.conversations') c WHERE json_extract(c.value,'$.id')=?2 AND
 (COALESCE(json_type(c.value,'$.itemIds'),'null') NOT IN ('null','array') OR json_array_length(c.value,'$.itemIds')>0
 OR COALESCE(json_type(c.value,'$.actionReviews'),'null') NOT IN ('null','array') OR json_array_length(c.value,'$.actionReviews')>0))
 AND NOT EXISTS (SELECT 1 FROM json_each(payload,'$.jobs') j WHERE json_extract(j.value,'$.id')=?1 AND
 (COALESCE(json_type(j.value,'$.prepareBundle.itemIds'),'null') NOT IN ('null','array') OR json_array_length(j.value,'$.prepareBundle.itemIds')>0
 OR COALESCE(json_type(j.value,'$.toolResults'),'null') NOT IN ('null','array') OR json_array_length(j.value,'$.toolResults')>0
 OR COALESCE(json_type(j.value,'$.prepareBundle.request.toolResults'),'null') NOT IN ('null','array') OR json_array_length(j.value,'$.prepareBundle.request.toolResults')>0))) AS empty_dialogue FROM workspace WHERE id=1)
SELECT json_set(json_remove(w.payload,'$.jobs','$.conversations','$.audit','$.feedback','$.approvals','$.items','$.branches','$.proposals','$.operations','$.materials'),
 '$.items',json(CASE WHEN empty_dialogue THEN '[]' ELSE json_extract(w.payload,'$.items') END),
 '$.branches',json(CASE WHEN empty_dialogue THEN '[]' ELSE json_extract(w.payload,'$.branches') END),
 '$.proposals',json(CASE WHEN empty_dialogue THEN '[]' ELSE json_extract(w.payload,'$.proposals') END),
 '$.operations',json(CASE WHEN empty_dialogue THEN '[]' ELSE json_extract(w.payload,'$.operations') END),
 '$.materials',json(CASE WHEN empty_dialogue THEN '[]' ELSE json_extract(w.payload,'$.materials') END),
 '$.jobs',json(COALESCE((SELECT json_group_array(json(j.value)) FROM json_each(w.payload,'$.jobs') j
  WHERE json_extract(j.value,'$.id')=?1
     OR (NOT empty_dialogue AND json_extract(j.value,'$.purpose')='manual_video_frames')
     OR (json_extract(j.value,'$.kind')='assistant' AND json_extract(j.value,'$.status') IN ('running','queued'))
     OR (NOT empty_dialogue AND EXISTS(SELECT 1 FROM json_each(w.payload,'$.proposals') p
       WHERE json_extract(p.value,'$.status') IN ('draft','approved','dispatching','unknown')
         AND (json_extract(p.value,'$.prepareRunId')=json_extract(j.value,'$.id') OR json_extract(p.value,'$.recovery.prepareRunId')=json_extract(j.value,'$.id'))))), '[]')),
 '$.conversations',json(COALESCE((SELECT json_group_array(json(c.value)) FROM json_each(w.payload,'$.conversations') c WHERE json_extract(c.value,'$.id')=?2),'[]')),
 '$.audit',json('[]'),'$.feedback',json('[]'),'$.approvals',json('[]'))
FROM w
"#;
// Build the proposal-reference set once. A correlated EXISTS here repeatedly
// decompresses proposal payloads for every historical job on large workspaces.
const PG_JOBS:&str=r#"(id=$2 OR (kind='assistant' AND status IN ('running','queued'))
 OR payload->>'purpose'='manual_video_frames'
 OR id IN (SELECT p.payload->>'prepareRunId' FROM communityhero.proposals p
   WHERE p.workspace_id=$1 AND p.status IN ('draft','approved','dispatching','unknown')
   UNION SELECT p.payload#>>'{recovery,prepareRunId}' FROM communityhero.proposals p
   WHERE p.workspace_id=$1 AND p.status IN ('draft','approved','dispatching','unknown')
   UNION SELECT p.payload#>>'{origin,prepareRunId}' FROM communityhero.proposals p
   WHERE p.workspace_id=$1 AND p.status IN ('draft','approved','dispatching','unknown')))"#;

async fn load_pg(connection:&mut PgConnection,job:Option<&str>,conversation:&str,write:bool,scope:Scope<'_>)->ApiResult<Value>{
    let _load=crate::performance::Span::new("assistant.load.total");
    let metadata_timing=crate::performance::Span::new("assistant.load.metadata");
    let statement=if write{"SELECT account,metadata::text,execution_enabled FROM communityhero.workspaces WHERE id=$1 FOR UPDATE"}else{"SELECT account,metadata::text,execution_enabled FROM communityhero.workspaces WHERE id=$1"};
    let record=sqlx::query(statement).bind(WORKSPACE).fetch_one(&mut *connection).await?;
    if record.try_get::<bool,_>("execution_enabled")?{return Err(internal("PostgreSQL pilot execution must remain disabled"));}
    let mut value=parse(record.try_get::<&str,_>("metadata")?)?;
    if !value.is_object()||record.try_get::<Option<String>,_>("account")?.as_deref()!=value["account"].as_str()||!value["account"].is_string(){return Err(internal("Workspace identity mismatch"));}
    drop(metadata_timing);
    let empty=if matches!(scope,Scope::Dialogue(ids) if ids.is_empty()) {
        sqlx::query_scalar::<_,bool>(r#"SELECT NOT EXISTS (
          SELECT 1 FROM communityhero.conversations WHERE workspace_id=$1 AND id=$3 AND
           (COALESCE(NULLIF(payload->'itemIds','null'::jsonb),'[]'::jsonb)<>'[]'::jsonb OR
            COALESCE(NULLIF(payload->'actionReviews','null'::jsonb),'[]'::jsonb)<>'[]'::jsonb))
          AND NOT EXISTS (SELECT 1 FROM communityhero.jobs WHERE workspace_id=$1 AND id=$2 AND
           (COALESCE(NULLIF(payload#>'{prepareBundle,itemIds}','null'::jsonb),'[]'::jsonb)<>'[]'::jsonb OR
            COALESCE(NULLIF(payload->'toolResults','null'::jsonb),'[]'::jsonb)<>'[]'::jsonb OR
            COALESCE(NULLIF(payload#>'{prepareBundle,request,toolResults}','null'::jsonb),'[]'::jsonb)<>'[]'::jsonb))"#)
            .bind(WORKSPACE).bind(job).bind(conversation).fetch_one(&mut *connection).await?
    }else{false};
    for table in TABLES{
        if value.get(table).is_some(){return Err(internal("Workspace metadata contains entity collections"));}
        if ["audit","feedback","approvals"].contains(&table)||(empty&&EMPTY_DIALOGUE_TABLES.contains(&table)){value[table]=json!([]);continue;}
        let condition=match table{"jobs" if empty=>"(id=$2 OR (kind='assistant' AND status IN ('running','queued')))","jobs"=>PG_JOBS,"conversations"=>"id=$2",_=>"TRUE"};
        // Identifiers/predicates are constants; all user-selected values are bound.
        let statement=format!("SELECT id,payload::text{} FROM communityhero.{table} WHERE workspace_id=$1 AND {condition} ORDER BY ordinal",projection(table).iter().map(|(column,_)|format!(",{column}")).collect::<String>());
        let mut query=sqlx::query(sqlx::AssertSqlSafe(statement.as_str())).bind(WORKSPACE);
        if table=="jobs"{query=query.bind(job);}else if table=="conversations"{query=query.bind(conversation);}
        let sql_timing=crate::performance::Span::new(match table {"jobs"=>"assistant.sql.jobs","items"=>"assistant.sql.items","posts"=>"assistant.sql.posts","branches"=>"assistant.sql.branches",_=>"assistant.sql.other"});
        let records=query.fetch_all(&mut *connection).await?;
        drop(sql_timing);
        let parse_timing=crate::performance::Span::new("assistant.parse.collection");
        let mut data=Vec::with_capacity(records.len());let mut seen=HashSet::new();
        for record in records{
            let payload=parse(record.try_get::<&str,_>("payload")?)?;
            if record.try_get::<&str,_>("id")?!=text(&payload,"id")?||!seen.insert(text(&payload,"id")?.to_owned()){return Err(internal("Record identity mismatch"));}
            for (column,key) in projection(table){
                if (!payload[*key].is_null()&&!payload[*key].is_string())||record.try_get::<Option<String>,_>(*column)?.as_deref()!=payload[*key].as_str(){return Err(internal("Record relational projection mismatch"));}
            }
            data.push(payload);
        }
        value[table]=Value::Array(data);
        drop(parse_timing);
    }
    validate_roots(&value,job,conversation)?;
    if !empty {
        let roots=dependency_roots(&value,job,conversation);
        value["jobs"]=json!(super::reads::dispatch::dependency_jobs_pg(connection,&roots).await?);
        super::scope_context::load(connection,&mut value).await?;
    }
    Ok(value)
}

impl Database {
    /// Caller holds the process writer gate. Append one server-owned record on
    /// the leased writer, without loading unrelated conversation/source history.
    pub(crate) async fn create_conversation(&self,actor_id:&str,title:&str,item_ids:&[Value],expected_runtime:&crate::runtime_lifecycle::RuntimeIdentity)->ApiResult<Value>{
        let record=||json!({"id":crate::id(),"title":title,"operatorId":actor_id,"itemIds":item_ids,"messages":[],"createdAt":crate::now()});
        match self{
            Self::Sqlite(_)=>self.change(|d|{
                crate::runtime_lifecycle::current_owner(d,expected_runtime)?;
                for key in item_ids {crate::row(d,"items",key.as_str().ok_or_else(||crate::bad("Invalid itemIds"))?)?;}
                let conversation=record();
                crate::list_mut(d,"conversations").push(conversation.clone());Ok(conversation)
            }).await,
            Self::Postgres {writer,..}=>{
                let _timing=crate::performance::Span::new("assistant.create_conversation.total");
                let mut tx=writer.begin().await?;
                let workspace=sqlx::query("SELECT account,metadata::text,execution_enabled FROM communityhero.workspaces WHERE id=$1 FOR UPDATE")
                    .bind(WORKSPACE).fetch_one(&mut *tx).await?;
                if workspace.try_get::<bool,_>("execution_enabled")?{return Err(internal("PostgreSQL pilot execution must remain disabled"));}
                let metadata=parse(workspace.try_get::<&str,_>("metadata")?)?;
                crate::runtime_lifecycle::current_owner(&metadata,expected_runtime)?;
                if !metadata.is_object()||!metadata["account"].is_string()
                    ||workspace.try_get::<Option<String>,_>("account")?.as_deref()!=metadata["account"].as_str(){return Err(internal("Workspace identity mismatch"));}
                text(&metadata,"account")?;
                if TABLES.iter().any(|table|metadata.get(*table).is_some()){return Err(internal("Workspace metadata contains entity collections"));}
                let keys:Vec<_>=item_ids.iter().filter_map(Value::as_str).collect();
                let mut attached=HashSet::new();
                if !keys.is_empty(){
                    // No source text or branch payload is needed to attach an
                    // exact existing identity. Keep the same projection checks.
                    let records=sqlx::query("SELECT id,post_id,branch_id,jsonb_build_object('id',payload->'id','postId',payload->'postId','branchId',payload->'branchId')::text AS payload FROM communityhero.items WHERE workspace_id=$1 AND id=ANY($2)")
                        .bind(WORKSPACE).bind(keys).fetch_all(&mut *tx).await?;
                    for row in records{
                        let payload=parse(row.try_get::<&str,_>("payload")?)?;
                        let key=text(&payload,"id")?;
                        if row.try_get::<&str,_>("id")?!=key||!attached.insert(key.to_owned()){return Err(internal("Record identity mismatch"));}
                        for (column,field) in projection("items"){
                            if (!payload[*field].is_null()&&!payload[*field].is_string())
                                ||row.try_get::<Option<String>,_>(*column)?.as_deref()!=payload[*field].as_str(){return Err(internal("Record relational projection mismatch"));}
                        }
                    }
                }
                // Preserve request order, duplicates, and the existing bad-ID
                // versus missing-record error behavior; attachment is not read admission.
                for value in item_ids{
                    let key=value.as_str().ok_or_else(||crate::bad("Invalid itemIds"))?;
                    if !attached.contains(key){return Err(crate::ApiError(axum::http::StatusCode::NOT_FOUND,"items record not found".into()));}
                }
                let conversation=record();
                persist_record(&mut tx,"conversations",&conversation,true).await?;
                tx.commit().await?;Ok(conversation)
            }
        }
    }
    /// Coherent current evidence, one conversation, and only relevant model jobs.
    /// No cache or source freshness relaxation. Other conversations never leave DB.
    pub(crate) async fn read_assistant_context(&self,job:Option<&str>,conversation:&str)->ApiResult<Value>{
        self.read_assistant_scope(job,conversation,Scope::Corpus).await
    }
    /// Empty discussion fast path, with an exact corpus fallback for any roots.
    /// Tools that search, count or discover recipients must use the corpus API.
    pub(crate) async fn read_assistant_dialogue(&self,job:Option<&str>,conversation:&str,extra_ids:&[Value])->ApiResult<Value>{
        self.read_assistant_scope(job,conversation,Scope::Dialogue(extra_ids)).await
    }
    async fn read_assistant_scope(&self,job:Option<&str>,conversation:&str,scope:Scope<'_>)->ApiResult<Value>{
        match self{
            Self::Sqlite(pool)=>{
                let mut tx=pool.begin().await?;
                let payload:String=sqlx::query_scalar(SQLITE_ASSISTANT).bind(job).bind(conversation).bind(matches!(scope,Scope::Dialogue(ids) if ids.is_empty())).fetch_one(&mut *tx).await?;
                let mut value=parse(&payload)?;validate_roots(&value,job,conversation)?;
                if !empty_dialogue(&value,job,conversation,scope){
                    let roots=dependency_roots(&value,job,conversation);
                    value["jobs"]=json!(super::reads::dispatch::dependency_jobs_sqlite(&mut tx,&roots).await?);
                    // Existing SQLite ownership projection requires the whole
                    // document. Keep it in this SAME reader snapshot; this is
                    // not a claim that SQLite wire transfer is now bounded.
                    let raw:String=sqlx::query_scalar("SELECT payload FROM workspace WHERE id=1").fetch_one(&mut *tx).await?;
                    let workspace=parse(&raw)?;super::scope_context::project(&workspace,&mut value)?;
                }
                tx.commit().await?;
                Ok(value)
            },
            Self::Postgres { reader:pool, .. }=>{
                let acquire=crate::performance::Span::new("assistant.read.pool_wait");
                let mut tx=pool.begin().await?;
                drop(acquire);
                sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ READ ONLY").execute(&mut *tx).await?;
                let value=load_pg(&mut tx,job,conversation,false,scope).await?;
                tx.commit().await?;Ok(value)
            }
        }
    }
    /// Caller holds the process writer gate; PG also locks the workspace row.
    /// Updates only loaded existing identities and INSERTs appended history.
    /// It cannot approve, dispatch, rewrite source evidence, or replace a table.
    pub(crate) async fn change_assistant_observed<T>(&self,job:Option<&str>,conversation:&str,f:impl FnOnce(&mut Value)->ApiResult<T>)->ApiResult<(T,bool)>{
        self.change_assistant_scope(job,conversation,Scope::Corpus,f).await
    }
    pub(crate) async fn change_assistant_dialogue_observed<T>(&self,job:Option<&str>,conversation:&str,extra_ids:&[Value],f:impl FnOnce(&mut Value)->ApiResult<T>)->ApiResult<(T,bool)>{
        self.change_assistant_scope(job,conversation,Scope::Dialogue(extra_ids),f).await
    }
    async fn change_assistant_scope<T>(&self,job:Option<&str>,conversation:&str,scope:Scope<'_>,f:impl FnOnce(&mut Value)->ApiResult<T>)->ApiResult<(T,bool)>{
        match self{
            Self::Sqlite(_)=>self.change_observed(|workspace|{
                let before=projected_scope(workspace,job,conversation,scope)?;let mut after=before.clone();let result=f(&mut after)?;
                validate_assistant_change(&before,&after,job,conversation)?;
                merge_delta(workspace,&before,&after)?;Ok(result)
            }).await,
            Self::Postgres { writer:pool, .. }=>{
                let acquire=crate::performance::Span::new("assistant.change.pool_wait");
                let mut tx=pool.begin().await?;
                drop(acquire);
                let before=load_pg(&mut tx,job,conversation,true,scope).await?;
                let preparation=crate::performance::Span::new("assistant.change.clone_and_domain");
                let mut after=before.clone();let result=f(&mut after)?;
                drop(preparation);
                let validation=crate::performance::Span::new("assistant.change.validation");
                validate_assistant_change(&before,&after,job,conversation)?;
                drop(validation);
                if before==after{tx.commit().await?;return Ok((result,false));}
                let persistence=crate::performance::Span::new("assistant.change.persist_and_commit");
                for table in WRITABLE{
                    let old=rows(&before,table)?;
                    for (index,value) in rows(&after,table)?.iter().enumerate(){
                        if old.get(index)==Some(value){continue;}
                        persist_record(&mut tx,table,value,index>=old.len()).await?;
                    }
                }
                tx.commit().await?;drop(persistence);Ok((result,true))
            }
        }
    }
}

fn without(v:&Value,keys:&[&str])->Value{
    let mut result=v.clone();if let Some(object)=result.as_object_mut(){for key in keys{object.remove(*key);}}result
}
fn validate_assistant_change(before:&Value,after:&Value,job:Option<&str>,conversation:&str)->ApiResult<()> {
    if !after.is_object()||metadata(before)!=metadata(after){return Err(internal("Assistant scope changed workspace metadata"));}
    validate_roots(after,job,conversation)?;
    for table in READONLY{if before[*table]!=after[*table]{return Err(internal("Assistant scope changed read-only source or authority"));}}
    for table in WRITABLE{
        let old=rows(before,table)?;let new=rows(after,table)?;
        if new.len()<old.len()||old.iter().zip(new).any(|(a,b)|a["id"]!=b["id"]){return Err(internal("Assistant scope cannot remove or reorder records"));}
        let mut seen=HashSet::new();
        for value in new{
            if !value.is_object()||!seen.insert(text(value,"id")?){return Err(internal("Invalid scoped identity"));}
            for (_,key) in projection(table){if !value[*key].is_null()&&!value[*key].is_string(){return Err(internal("Invalid scoped record projection"));}}
        }
    }
    let old_items=rows(before,"items")?;let new_items=rows(after,"items")?;
    if old_items.len()!=new_items.len(){return Err(internal("Assistant cannot add comments"));}
    for (old,new) in old_items.iter().zip(new_items){
        let mut old=without(old,&["workflow","revision","waitingReason","dueAt"]);
        let mut new=without(new,&["workflow","revision","waitingReason","dueAt"]);
        for item in [&mut old,&mut new]{if let Some(a)=item["autoPreparation"].as_object_mut(){a.remove("humanOverrideAt");}}
        if old!=new{return Err(internal("Assistant changed protected comment content or routing"));}
    }
    let old_chats=rows(before,"conversations")?;let new_chats=rows(after,"conversations")?;
    if old_chats.len()!=1||new_chats.len()!=1{return Err(internal("Assistant conversation membership changed"));}
    for field in ["id","operatorId","createdAt"]{if old_chats[0][field]!=new_chats[0][field]{return Err(internal("Assistant conversation identity changed"));}}
    let old_jobs=rows(before,"jobs")?;let new_jobs=rows(after,"jobs")?;
    for (old,new) in old_jobs.iter().zip(new_jobs){
        if job!=old["id"].as_str(){if old!=new{return Err(internal("Assistant changed another job"));}}
        else{for field in ["id","kind","refId","operatorId","sourceUserMessageId","createdAt"]{if old[field]!=new[field]{return Err(internal("Assistant job identity changed"));}}}
    }
    for added in &new_jobs[old_jobs.len()..]{
        if job.is_some()||added["kind"]!="assistant"||added["refId"]!=conversation||added["operatorId"].as_str()!=Some(new_chats[0]["operatorId"].as_str().unwrap_or("local-owner"))||added["status"]!="running"{return Err(internal("Invalid assistant job admission"));}
    }
    for table in ["proposals","audit","feedback"]{
        let old=rows(before,table)?;let new=rows(after,table)?;
        if !new.starts_with(old){return Err(internal("Assistant cannot rewrite historical records"));}
        if table!="audit"{for added in &new[old.len()..]{if !new_items.iter().any(|item|item["id"]==added["itemId"]){return Err(internal("Assistant record has an unknown recipient"));}}}
    }
    crate::db_guards::validate_change(before,after)?;
    Ok(())
}
fn merge_delta(workspace:&mut Value,before:&Value,after:&Value)->ApiResult<()> {
    for table in WRITABLE{
        let old=rows(before,table)?;
        for (index,record) in rows(after,table)?.iter().enumerate(){
            if old.get(index)==Some(record){continue;}
            let target=workspace[*table].as_array_mut().ok_or_else(||internal("Workspace collection missing"))?;
            if index<old.len(){
                let stored=target.iter_mut().find(|v|v["id"]==record["id"]).ok_or_else(||internal("Scoped record disappeared"))?;
                *stored=record.clone();
            }else{
                if target.iter().any(|v|v["id"]==record["id"]){return Err(internal("Scoped append reused an unloaded identity"));}
                target.push(record.clone());
            }
        }
    }Ok(())
}
async fn persist_record(connection:&mut PgConnection,table:&str,value:&Value,append:bool)->ApiResult<()> {
    let columns=projection(table);
    let statement=if append{
        format!("INSERT INTO communityhero.{table}(workspace_id,id,payload,ordinal{}) SELECT $1,$2,$3::jsonb,COALESCE(MAX(ordinal),-1)+1{} FROM communityhero.{table} WHERE workspace_id=$1",columns.iter().map(|(c,_)|format!(",{c}")).collect::<String>(),(0..columns.len()).map(|n|format!(",${}",n+4)).collect::<String>())
    }else{
        format!("UPDATE communityhero.{table} SET payload=$3::jsonb{} WHERE workspace_id=$1 AND id=$2",columns.iter().enumerate().map(|(n,(c,_))|format!(",{c}=${}",n+4)).collect::<String>())
    };
    let mut query=sqlx::query(sqlx::AssertSqlSafe(statement.as_str())).bind(WORKSPACE).bind(text(value,"id")?).bind(value.to_string());
    for (_,field) in columns{query=query.bind(value[*field].as_str());}
    if query.execute(connection).await?.rows_affected()!=1{return Err(internal("Scoped record disappeared"));}Ok(())
}

#[cfg(test)]
#[path="storage_assistant_tests.rs"]
mod tests;
