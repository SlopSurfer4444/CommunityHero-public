//! Complete selected diagnostic evidence in one read-only snapshot. This view
//! is never persisted and never falls back to the full workspace on ambiguity.
use super::*;
use crate::{ApiError,StatusCode};
use serde_json::json;
use sqlx::SqliteConnection;

#[derive(Clone,Copy,Debug)]
pub(crate) struct MediaGateReadBudget { pub max_rows:usize, pub max_bytes:usize }
impl Default for MediaGateReadBudget {
    fn default()->Self{Self{max_rows:3000,max_bytes:16*1024*1024}}
}
fn budget_error()->ApiError{ApiError(StatusCode::PAYLOAD_TOO_LARGE,"diagnostic_budget_exceeded:media_context; inspect complete evidence through offline workspace export".into())}
fn ambiguous()->ApiError{ApiError(StatusCode::CONFLICT,"scope_ambiguous:media_context; complete evidence required".into())}
impl MediaGateReadBudget {
    fn check(&self,count:usize,bytes:usize)->ApiResult<()> {
        if self.max_rows==0||self.max_bytes==0||count>self.max_rows||bytes>self.max_bytes{return Err(budget_error());}Ok(())
    }
    fn check_view(&self,view:&Value)->ApiResult<()> {
        let count=TABLES.iter().map(|t|view[*t].as_array().map_or(0,Vec::len)).sum();
        self.check(count,serde_json::to_vec(view).map_err(|_|internal("Media context serialization failed"))?.len())
    }
}
fn ids(view:&Value,table:&str,field:&str)->Vec<String>{
    let mut ids=crate::list(view,table).iter().filter_map(|v|v[field].as_str().filter(|s|!s.is_empty()).map(str::to_owned)).collect::<Vec<_>>();ids.sort();ids.dedup();ids
}
fn collect_refs(value:&Value,key:&str,output:&mut HashSet<String>,depth:usize)->ApiResult<()> {
    if depth>64{return Err(ambiguous());}
    match value {
        Value::Object(fields)=>for(name,value)in fields {
            if name==key {let id=value.as_str().filter(|s|!s.is_empty()).ok_or_else(ambiguous)?;output.insert(id.into());}
            // Arrays bind full immutable recipient groups, not only the current
            // proposal. Other receipt arrays retain their exact field pins.
            if key=="itemId"&&matches!(name.as_str(),"itemIds"|"selectedItemIds") {
                for id in value.as_array().ok_or_else(ambiguous)? {output.insert(id.as_str().filter(|s|!s.is_empty()).ok_or_else(ambiguous)?.into());}
            }
            collect_refs(value,key,output,depth+1)?;
        },
        Value::Array(values)=>for value in values{collect_refs(value,key,output,depth+1)?;},
        _=>(),
    }
    if output.len()>3000||output.iter().any(|id|id.len()>512||id.chars().any(char::is_control)){return Err(ambiguous());}Ok(())
}
fn references(view:&Value,key:&str)->ApiResult<Vec<String>> {
    let mut output=HashSet::new();
    for table in ["proposals","jobs"]{collect_refs(&view[table],key,&mut output,0)?;}
    let mut output=output.into_iter().collect::<Vec<_>>();output.sort();Ok(output)
}
fn root(raw:&str,budget:&MediaGateReadBudget)->ApiResult<Value> {
    budget.check(0,raw.len())?;let mut view=parse(raw)?;
    if !view.is_object()||TABLES.iter().any(|t|view.get(*t).is_some()){return Err(internal("Invalid media context metadata"));}
    crate::accounts::Profile::from_workspace(&view)?;crate::active_binding(&view)?;
    for table in TABLES{view[table]=json!([]);}Ok(view)
}
// Stats are read inside the same RR transaction before transferring ANY body.
// SQL predicates select both relational and JSON identities so corrupt carriers
// reach the identity/projection checks rather than becoming absence.
async fn pg_rows(connection:&mut PgConnection,table:&str,condition:&str,selected:&[String],context:&Value,budget:&MediaGateReadBudget)->ApiResult<Vec<Value>> {
    if !TABLES.contains(&table){return Err(internal("Invalid media table"));}
    let stats=format!("SELECT count(*)::bigint AS count,COALESCE(sum(octet_length(payload::text)),0)::bigint AS bytes FROM communityhero.{table} WHERE workspace_id=$1 AND {condition}");
    let stats=sqlx::query(sqlx::AssertSqlSafe(stats.as_str())).bind(WORKSPACE).bind(selected).bind(context.to_string()).fetch_one(&mut *connection).await?;
    let count=stats.try_get::<i64,_>("count")?;let bytes=stats.try_get::<i64,_>("bytes")?;
    if count<0||bytes<0{return Err(internal("Invalid media context budget measurement"));}budget.check(count as usize,bytes as usize)?;
    let statement=format!("SELECT id,ordinal,payload::text{} FROM communityhero.{table} WHERE workspace_id=$1 AND {condition} ORDER BY ordinal,id LIMIT $4",
        projection(table).iter().map(|(c,_)|format!(",{c}")).collect::<String>());
    let records=sqlx::query(sqlx::AssertSqlSafe(statement.as_str())).bind(WORKSPACE).bind(selected).bind(context.to_string()).bind((budget.max_rows+1) as i64).fetch_all(&mut *connection).await?;
    budget.check(records.len(),bytes as usize)?;if records.len()!=count as usize{return Err(internal("Media context selected set changed"));}
    let mut measurement=crate::performance::Span::new("media_context.selected.payload");measurement.counts(records.len(),bytes as usize,2);drop(measurement);
    let mut seen=HashSet::new();let mut previous=None;let mut values=Vec::with_capacity(records.len());
    for record in records {
        let value=parse(record.try_get("payload")?)?;let position=record.try_get::<i32,_>("ordinal")?;
        if record.try_get::<&str,_>("id")?!=text(&value,"id")?||!seen.insert(text(&value,"id")?.to_owned())||position<0||previous.is_some_and(|old|old>=position){return Err(internal("Media context identity/order mismatch"));}
        for(column,field)in projection(table){
            if (!value[*field].is_null()&&!value[*field].is_string())||record.try_get::<Option<String>,_>(*column)?.as_deref()!=value[*field].as_str(){return Err(internal("Media context relational projection mismatch"));}
        }
        previous=Some(position);values.push(value);
    }
    Ok(values)
}
async fn sqlite_rows(connection:&mut SqliteConnection,table:&str,condition:&str,selected:&[String],context:&Value,budget:&MediaGateReadBudget)->ApiResult<Vec<Value>> {
    if !TABLES.contains(&table){return Err(internal("Invalid media table"));}
    let base=format!("FROM workspace w,json_each(w.payload,'$.{table}') r WHERE w.id=1 AND {condition}");
    let stats=format!("SELECT count(*) AS count,COALESCE(sum(length(CAST(r.value AS BLOB))),0) AS bytes {base}");
    let stats=sqlx::query(sqlx::AssertSqlSafe(stats.as_str())).bind(json!(selected).to_string()).bind(context.to_string()).fetch_one(&mut *connection).await?;
    let count=stats.try_get::<i64,_>("count")?;let bytes=stats.try_get::<i64,_>("bytes")?;
    if count<0||bytes<0{return Err(internal("Invalid media context budget measurement"));}budget.check(count as usize,bytes as usize)?;
    let statement=format!("SELECT r.value AS payload,CAST(r.key AS INTEGER) AS ordinal {base} ORDER BY CAST(r.key AS INTEGER) LIMIT ?3");
    let records=sqlx::query(sqlx::AssertSqlSafe(statement.as_str())).bind(json!(selected).to_string()).bind(context.to_string()).bind((budget.max_rows+1) as i64).fetch_all(&mut *connection).await?;
    budget.check(records.len(),bytes as usize)?;if records.len()!=count as usize{return Err(internal("Media context selected set changed"));}
    let mut measurement=crate::performance::Span::new("media_context.selected.payload");measurement.counts(records.len(),bytes as usize,2);drop(measurement);
    let mut values=Vec::with_capacity(records.len());let mut seen=HashSet::new();
    for record in records{let value=parse(record.try_get("payload")?)?;if !seen.insert(text(&value,"id")?.to_owned()){return Err(internal("Duplicate media context identity"));}values.push(value);}Ok(values)
}
const PG_IDS:&str="(id=ANY($2::text[]) OR payload->>'id'=ANY($2::text[])) AND $3::jsonb IS NOT NULL";
const SQLITE_IDS:&str="json_extract(r.value,'$.id') IN (SELECT value FROM json_each(?1)) AND json(?2) IS NOT NULL";
const PG_ITEMS:&str=r#"(id=ANY($2::text[]) OR payload->>'id'=ANY($2::text[]) OR EXISTS(
 SELECT 1 FROM jsonb_array_elements($3::jsonb->'targets') target WHERE
 length(COALESCE(target->>'authorId',''))>0 AND payload->>'authorId'=target->>'authorId' AND payload->'platform'=target->'platform'))"#;
const SQLITE_ITEMS:&str=r#"(json_extract(r.value,'$.id') IN (SELECT value FROM json_each(?1)) OR EXISTS(
 SELECT 1 FROM json_each(?2,'$.targets') target WHERE length(COALESCE(json_extract(target.value,'$.authorId'),''))>0
 AND json_extract(r.value,'$.authorId')=json_extract(target.value,'$.authorId') AND json_extract(r.value,'$.platform')=json_extract(target.value,'$.platform')))"#;
const PG_POSTS:&str=r#"(id=ANY($2::text[]) OR payload->>'id'=ANY($2::text[]) OR payload->>'postKey' IN
 (SELECT jsonb_array_elements_text($3::jsonb->'postKeys')))"#;
const SQLITE_POSTS:&str=r#"(json_extract(r.value,'$.id') IN (SELECT value FROM json_each(?1)) OR json_extract(r.value,'$.postKey') IN (SELECT value FROM json_each(?2,'$.postKeys')))"#;
// Keep every current media candidate: exact same-byte audio equivalence/native
// first-alias ranking is a domain contract. Scope unrelated fact/rule heads by
// their explicit post scope; malformed/current-head disagreements stay visible.
const PG_HEADS:&str=r#"($2::text[] IS NOT NULL AND (id IN (SELECT v.entry_id FROM communityhero.knowledge_versions v WHERE v.workspace_id=$1 AND v.id IN
 (SELECT jsonb_array_elements_text($3::jsonb->'versionIds'))) OR payload->>'kind' IN ('transcript','ocr','visual_context','customer_case') OR EXISTS(
 SELECT 1 FROM communityhero.knowledge_versions v WHERE v.workspace_id=$1 AND v.id=current_version_id AND
 (v.payload->>'kind' IN ('transcript','ocr','visual_context','customer_case') OR
 jsonb_typeof(v.payload#>'{scope,postKeys}') IS DISTINCT FROM 'array' OR jsonb_array_length(v.payload#>'{scope,postKeys}')=0 OR
 EXISTS(SELECT 1 FROM jsonb_array_elements_text(v.payload#>'{scope,postKeys}') k JOIN jsonb_array_elements_text($3::jsonb->'postKeys') p ON k=p)
 OR jsonb_typeof(v.payload#>'{companyImport,scope,postAliases}')='array') ) OR NOT EXISTS(
 SELECT 1 FROM communityhero.knowledge_versions v WHERE v.workspace_id=$1 AND v.id=current_version_id)))"#;
const SQLITE_HEADS:&str=r#"(json(?1) IS NOT NULL AND (json_extract(r.value,'$.id') IN
 (SELECT json_extract(v.value,'$.entryId') FROM json_each(w.payload,'$.knowledge_versions') v WHERE json_extract(v.value,'$.id') IN (SELECT value FROM json_each(?2,'$.versionIds')))
 OR json_extract(r.value,'$.kind') IN ('transcript','ocr','visual_context','customer_case') OR EXISTS(
 SELECT 1 FROM json_each(w.payload,'$.knowledge_versions') v WHERE json_extract(v.value,'$.id')=json_extract(r.value,'$.currentVersionId') AND
 (json_extract(v.value,'$.kind') IN ('transcript','ocr','visual_context','customer_case') OR COALESCE(json_type(v.value,'$.scope.postKeys'),'')!='array'
 OR json_array_length(v.value,'$.scope.postKeys')=0 OR EXISTS(SELECT 1 FROM json_each(v.value,'$.scope.postKeys') k JOIN json_each(?2,'$.postKeys') p ON k.value=p.value)
 OR json_type(v.value,'$.companyImport.scope.postAliases')='array')) OR NOT EXISTS(
 SELECT 1 FROM json_each(w.payload,'$.knowledge_versions') v WHERE json_extract(v.value,'$.id')=json_extract(r.value,'$.currentVersionId'))))"#;
const PG_HEAD_PEERS:&str=r#"(source_material_id=ANY($2::text[]) OR payload->>'sourceMaterialId'=ANY($2::text[]) OR id IN(SELECT jsonb_array_elements_text($3::jsonb->'entryIds')) OR payload->>'id' IN(SELECT jsonb_array_elements_text($3::jsonb->'entryIds')))"#;
const SQLITE_HEAD_PEERS:&str=r#"(json_extract(r.value,'$.sourceMaterialId') IN(SELECT value FROM json_each(?1)) OR json_extract(r.value,'$.id') IN(SELECT value FROM json_each(?2,'$.entryIds')))"#;
const PG_MATERIALS:&str=r#"(id=ANY($2::text[]) OR payload->>'id'=ANY($2::text[]) OR
 jsonb_typeof(payload->'postKey') IS DISTINCT FROM 'string' OR payload->>'postKey'='' OR payload->>'postKey' IN(SELECT jsonb_array_elements_text($3::jsonb->'postKeys')))"#;
const SQLITE_MATERIALS:&str=r#"(json_extract(r.value,'$.id') IN(SELECT value FROM json_each(?1)) OR COALESCE(json_type(r.value,'$.postKey'),'')!='text' OR json_extract(r.value,'$.postKey')='' OR json_extract(r.value,'$.postKey') IN(SELECT value FROM json_each(?2,'$.postKeys')))"#;
// sourceAttempts intentionally do NOT use ref_id: one job can acquire several
// posts. Cached audio retains its complete immutable pin and current body.
const PG_MEDIA_JOBS:&str=r#"($2::text[] IS NOT NULL AND (kind IN ('media_analysis','media_analysis_applicability') OR payload->>'kind' IN ('media_analysis','media_analysis_applicability') OR
 ((kind IN ('media','media_audio') OR payload->>'kind' IN ('media','media_audio')) AND (
 ref_id IN(SELECT jsonb_array_elements_text($3::jsonb->'postIds')) OR payload->>'refId' IN(SELECT jsonb_array_elements_text($3::jsonb->'postIds')) OR
 payload#>>'{result,visualProgress,sourcePostId}' IN(SELECT jsonb_array_elements_text($3::jsonb->'postIds')) OR
 EXISTS(SELECT 1 FROM jsonb_array_elements(CASE WHEN jsonb_typeof(payload->'sourceAttempts')='array' THEN payload->'sourceAttempts' ELSE '[]'::jsonb END) attempt
 JOIN jsonb_array_elements($3::jsonb->'sources') source ON attempt->'postId'=source->'id' AND attempt->'sourceVersion'=source->'sourceVersion')))))"#;
const SQLITE_MEDIA_JOBS:&str=r#"(json(?1) IS NOT NULL AND (json_extract(r.value,'$.kind') IN ('media_analysis','media_analysis_applicability') OR
 (json_extract(r.value,'$.kind') IN ('media','media_audio') AND (
 json_extract(r.value,'$.refId') IN(SELECT value FROM json_each(?2,'$.postIds')) OR json_extract(r.value,'$.result.visualProgress.sourcePostId') IN(SELECT value FROM json_each(?2,'$.postIds')) OR
 EXISTS(SELECT 1 FROM json_each(CASE WHEN json_type(r.value,'$.sourceAttempts')='array' THEN json_extract(r.value,'$.sourceAttempts') ELSE '[]' END) attempt JOIN json_each(?2,'$.sources') source
 ON json_extract(attempt.value,'$.postId')=json_extract(source.value,'$.id') AND json_extract(attempt.value,'$.sourceVersion')=json_extract(source.value,'$.sourceVersion'))))))"#;
fn merge_jobs(view:&mut Value,extra:Vec<Value>)->ApiResult<()> {
    let jobs=view["jobs"].as_array_mut().ok_or_else(ambiguous)?;
    for job in extra {
        if let Some(old)=jobs.iter().find(|old|old["id"]==job["id"]){if old!=&job{return Err(ambiguous());}}
        else{jobs.push(job);}
    }Ok(())
}
fn validate_selected(view:&Value,proposal_id:&str,budget:&MediaGateReadBudget)->ApiResult<()> {
    budget.check_view(view)?;let proposals=rows(view,"proposals")?;
    if proposals.len()!=1||proposals[0]["id"]!=proposal_id{return Err(ApiError(StatusCode::NOT_FOUND,"Selected proposal missing or duplicated".into()));}
    let selected=crate::row(view,"items",text(&proposals[0],"itemId")?)?;
    let binding=crate::active_binding(view)?;crate::bound_item(&binding,selected)?;
    for(table,edge,target)in [("items","postId","posts"),("items","branchId","branches"),("branches","postId","posts"),("knowledge_entries","currentVersionId","knowledge_versions"),("knowledge_versions","entryId","knowledge_entries"),("knowledge_entries","sourceMaterialId","materials"),("knowledge_versions","sourceMaterialId","materials")] {
        let mut seen=HashSet::new();for row in rows(view,table)?{
            if !seen.insert(text(row,"id")?){return Err(internal("Duplicate selected media source identity"));}
            if !row[edge].is_null() {let key=text(row,edge)?;if !crate::list(view,target).iter().any(|row|row["id"]==key){return Err(internal("Dangling selected media source reference"));}}
        }
    }
    crate::knowledge::validate_catalog(view).map_err(internal)?;
    Ok(())
}
fn source_post_ids(view:&Value)->ApiResult<Vec<String>> {
    let mut selected=ids(view,"items","postId");selected.extend(references(view,"postId")?);selected.extend(references(view,"sourcePostId")?);
    if let Some(records)=view["settings"].get("mediaAudioEquivalences") {
        let records=records.as_object().filter(|m|m.len()<=1000).ok_or_else(ambiguous)?;
        let targets=selected.iter().cloned().collect::<HashSet<_>>();
        for(target,record)in records {if targets.contains(target){if let Some(id)=record["sourcePostId"].as_str().filter(|s|!s.is_empty()){selected.push(id.into());}}}
    }
    selected.sort();selected.dedup();Ok(selected)
}
fn context(view:&Value)->ApiResult<Value> {
    let mut keys=ids(view,"items","postKey");keys.extend(ids(view,"posts","postKey"));keys.sort();keys.dedup();
    let mut post_ids=ids(view,"items","postId");post_ids.extend(ids(view,"posts","id"));post_ids.sort();post_ids.dedup();
    let account=text(view,"account")?;
    let sources=crate::list(view,"posts").iter().map(|post|json!({"id":post["id"],"sourceVersion":crate::media_fullframes::source_version(post,account)})).collect::<Vec<_>>();
    let mut versions=references(view,"versionId")?;versions.extend(references(view,"knowledgeVersionId")?);versions.sort();versions.dedup();
    Ok(json!({"account":view["account"],"connectorBinding":view["connectorBinding"],"targets":view["items"],"postKeys":keys,"postIds":post_ids,"sources":sources,"versionIds":versions}))
}
fn media_source_context(view:&Value)->ApiResult<Value> {
    let mut ctx=context(view)?;let mut keys=ctx["postKeys"].as_array().ok_or_else(ambiguous)?.iter().filter_map(|v|v.as_str().map(str::to_owned)).collect::<Vec<_>>();
    for version in crate::list(view,"knowledge_versions").iter().filter(|v|matches!(v["kind"].as_str(),Some("transcript"|"ocr"|"visual_context"))) {
        if let Some(key)=version["postKey"].as_str().filter(|s|!s.is_empty()){keys.push(key.into());}
    }
    keys.sort();keys.dedup();ctx["postKeys"]=json!(keys);Ok(ctx)
}
async fn media_sources_pg(connection:&mut PgConnection,view:&mut Value,budget:&MediaGateReadBudget)->ApiResult<()> {
    let ctx=media_source_context(view)?;view["posts"]=json!(pg_rows(connection,"posts",PG_POSTS,&source_post_ids(view)?,&ctx,budget).await?);
    let ctx=context(view)?;let extra=pg_rows(connection,"jobs",PG_MEDIA_JOBS,&[],&ctx,budget).await?;merge_jobs(view,extra)?;
    let roots=rows(view,"jobs")?.iter().chain(rows(view,"proposals")?.iter()).cloned().collect::<Vec<_>>();
    let jobs=super::reads::dispatch::dependency_jobs_pg_bounded(connection,&roots,budget.max_rows,budget.max_bytes).await.map_err(|e|if e.1.contains("ambiguous"){ambiguous()}else if e.1.contains("budget"){budget_error()}else{e})?;merge_jobs(view,jobs)?;
    view["jobs"]=json!(pg_rows(connection,"jobs",PG_IDS,&ids(view,"jobs","id"),&json!({}),budget).await?);Ok(())
}
async fn media_sources_sqlite(connection:&mut SqliteConnection,view:&mut Value,budget:&MediaGateReadBudget)->ApiResult<()> {
    let ctx=media_source_context(view)?;view["posts"]=json!(sqlite_rows(connection,"posts",SQLITE_POSTS,&source_post_ids(view)?,&ctx,budget).await?);
    let ctx=context(view)?;let extra=sqlite_rows(connection,"jobs",SQLITE_MEDIA_JOBS,&[],&ctx,budget).await?;merge_jobs(view,extra)?;
    let roots=rows(view,"jobs")?.iter().chain(rows(view,"proposals")?.iter()).cloned().collect::<Vec<_>>();
    let jobs=super::reads::dispatch::dependency_jobs_sqlite_bounded(connection,&roots,budget.max_rows,budget.max_bytes).await.map_err(|e|if e.1.contains("ambiguous"){ambiguous()}else if e.1.contains("budget"){budget_error()}else{e})?;merge_jobs(view,jobs)?;
    view["jobs"]=json!(sqlite_rows(connection,"jobs",SQLITE_IDS,&ids(view,"jobs","id"),&json!({}),budget).await?);Ok(())
}
async fn research_pg(connection:&mut PgConnection,view:&mut Value,budget:&MediaGateReadBudget)->ApiResult<()> {
    let kind:Option<String>=sqlx::query_scalar("SELECT jsonb_typeof(metadata->'preparationResearch') FROM communityhero.workspaces WHERE id=$1").bind(WORKSPACE).fetch_one(&mut *connection).await?;
    let Some(kind)=kind else{return Ok(());};if kind!="array"{return Err(ambiguous());}
    let selected=references(view,"archiveId")?;
    let stats=sqlx::query("SELECT count(*)::bigint AS count,COALESCE(sum(octet_length(a.value::text)),0)::bigint AS bytes FROM communityhero.workspaces w CROSS JOIN LATERAL jsonb_array_elements(w.metadata->'preparationResearch') a WHERE w.id=$1 AND a.value->>'id'=ANY($2::text[])").bind(WORKSPACE).bind(&selected).fetch_one(&mut *connection).await?;
    let count=stats.try_get::<i64,_>("count")?;let bytes=stats.try_get::<i64,_>("bytes")?;
    if count<0||bytes<0{return Err(internal("Invalid media research measurement"));}budget.check(count as usize,bytes as usize)?;
    let raw:String=sqlx::query_scalar("SELECT COALESCE(json_agg(a.value ORDER BY a.ordinality),'[]'::json)::text FROM communityhero.workspaces w CROSS JOIN LATERAL jsonb_array_elements(w.metadata->'preparationResearch') WITH ORDINALITY a WHERE w.id=$1 AND a.value->>'id'=ANY($2::text[])").bind(WORKSPACE).bind(&selected).fetch_one(&mut *connection).await?;
    let holder=json!({"preparationResearch":parse(&raw)?});super::source_snapshot::project_pinned_research(&holder,view);Ok(())
}
async fn research_sqlite(connection:&mut SqliteConnection,view:&mut Value,budget:&MediaGateReadBudget)->ApiResult<()> {
    let kind:Option<String>=sqlx::query_scalar("SELECT json_type(payload,'$.preparationResearch') FROM workspace WHERE id=1").fetch_one(&mut *connection).await?;
    let Some(kind)=kind else{return Ok(());};if kind!="array"{return Err(ambiguous());}
    let selected=json!(references(view,"archiveId")?).to_string();
    let stats=sqlx::query("SELECT count(*) AS count,COALESCE(sum(length(CAST(a.value AS BLOB))),0) AS bytes FROM workspace w,json_each(w.payload,'$.preparationResearch') a WHERE w.id=1 AND json_extract(a.value,'$.id') IN(SELECT value FROM json_each(?1))").bind(&selected).fetch_one(&mut *connection).await?;
    let count=stats.try_get::<i64,_>("count")?;let bytes=stats.try_get::<i64,_>("bytes")?;
    if count<0||bytes<0{return Err(internal("Invalid media research measurement"));}budget.check(count as usize,bytes as usize)?;
    let raw:String=sqlx::query_scalar("SELECT COALESCE(json_group_array(json(a.value)),'[]') FROM workspace w,json_each(w.payload,'$.preparationResearch') a WHERE w.id=1 AND json_extract(a.value,'$.id') IN(SELECT value FROM json_each(?1)) ORDER BY CAST(a.key AS INTEGER)").bind(&selected).fetch_one(&mut *connection).await?;
    let holder=json!({"preparationResearch":parse(&raw)?});super::source_snapshot::project_pinned_research(&holder,view);Ok(())
}
async fn selected_pg(connection:&mut PgConnection,proposal_id:&str,budget:&MediaGateReadBudget)->ApiResult<Value> {
    let size: i64=sqlx::query_scalar("SELECT octet_length((metadata-'preparationResearch'-'companyKnowledgeCoverage')::text)::bigint FROM communityhero.workspaces WHERE id=$1").bind(WORKSPACE).fetch_one(&mut *connection).await?;
    if size<0{return Err(internal("Invalid media metadata byte count"));}budget.check(0,size as usize)?;
    let record=sqlx::query("SELECT (metadata-'preparationResearch'-'companyKnowledgeCoverage')::text AS metadata,execution_enabled,(jsonb_typeof(metadata)='object' AND account=metadata->>'account') IS TRUE AS identity_valid FROM communityhero.workspaces WHERE id=$1").bind(WORKSPACE).fetch_one(&mut *connection).await?;
    if record.try_get::<bool,_>("execution_enabled")?||!record.try_get::<bool,_>("identity_valid")?{return Err(internal("Media workspace identity mismatch"));}
    let mut view=root(record.try_get("metadata")?,budget)?;
    view["proposals"]=json!(pg_rows(connection,"proposals",PG_IDS,&[proposal_id.into()],&json!({}),budget).await?);
    if crate::list(&view,"proposals").len()!=1{return Err(ApiError(StatusCode::NOT_FOUND,"Selected proposal missing or duplicated".into()));}
    let dependency=super::reads::dispatch::dependency_jobs_pg_bounded(connection,rows(&view,"proposals")?,budget.max_rows,budget.max_bytes).await.map_err(|e|if e.1.contains("ambiguous"){ambiguous()}else if e.1.contains("budget"){budget_error()}else{e})?;
    view["jobs"]=json!(dependency);
    let item_ids=references(&view,"itemId")?;
    view["items"]=json!(pg_rows(connection,"items",PG_IDS,&item_ids,&json!({}),budget).await?);
    let ctx=context(&view)?;
    view["items"]=json!(pg_rows(connection,"items",PG_ITEMS,&item_ids,&ctx,budget).await?);
    let ctx=context(&view)?;
    view["posts"]=json!(pg_rows(connection,"posts",PG_POSTS,&source_post_ids(&view)?,&ctx,budget).await?);
    view["branches"]=json!(pg_rows(connection,"branches",PG_IDS,&ids(&view,"items","branchId"),&json!({}),budget).await?);
    let ctx=context(&view)?;
    let extra=pg_rows(connection,"jobs",PG_MEDIA_JOBS,&[],&ctx,budget).await?;merge_jobs(&mut view,extra)?;
    let roots=rows(&view,"jobs")?.iter().chain(rows(&view,"proposals")?.iter()).cloned().collect::<Vec<_>>();
    let dependencies=super::reads::dispatch::dependency_jobs_pg_bounded(connection,&roots,budget.max_rows,budget.max_bytes).await.map_err(|e|if e.1.contains("ambiguous"){ambiguous()}else if e.1.contains("budget"){budget_error()}else{e})?;
    merge_jobs(&mut view,dependencies)?;
    view["jobs"]=json!(pg_rows(connection,"jobs",PG_IDS,&ids(&view,"jobs","id"),&json!({}),budget).await?);
    let item_ids=references(&view,"itemId")?;
    view["items"]=json!(pg_rows(connection,"items",PG_IDS,&item_ids,&json!({}),budget).await?);
    let ctx=context(&view)?;view["items"]=json!(pg_rows(connection,"items",PG_ITEMS,&item_ids,&ctx,budget).await?);
    let ctx=context(&view)?;view["posts"]=json!(pg_rows(connection,"posts",PG_POSTS,&source_post_ids(&view)?,&ctx,budget).await?);
    view["branches"]=json!(pg_rows(connection,"branches",PG_IDS,&ids(&view,"items","branchId"),&json!({}),budget).await?);
    let ctx=context(&view)?;
    view["knowledge_entries"]=json!(pg_rows(connection,"knowledge_entries",PG_HEADS,&[],&ctx,budget).await?);
    let peers=json!({"entryIds":ids(&view,"knowledge_entries","id")});view["knowledge_entries"]=json!(pg_rows(connection,"knowledge_entries",PG_HEAD_PEERS,&ids(&view,"knowledge_entries","sourceMaterialId"),&peers,budget).await?);
    let mut versions=ids(&view,"knowledge_entries","currentVersionId");versions.extend(ctx["versionIds"].as_array().unwrap().iter().filter_map(|v|v.as_str().map(str::to_owned)));versions.sort();versions.dedup();
    view["knowledge_versions"]=json!(pg_rows(connection,"knowledge_versions",PG_IDS,&versions,&json!({}),budget).await?);
    media_sources_pg(connection,&mut view,budget).await?;
    let ctx=context(&view)?;
    view["materials"]=json!(pg_rows(connection,"materials",PG_MATERIALS,&ids(&view,"knowledge_entries","sourceMaterialId"),&ctx,budget).await?);
    research_pg(connection,&mut view,budget).await?;
    let mut controls=view.clone();super::scope_context::project(&view,&mut controls)?;view=controls;
    validate_selected(&view,proposal_id,budget)?;Ok(view)
}
async fn selected_sqlite(connection:&mut SqliteConnection,proposal_id:&str,budget:&MediaGateReadBudget)->ApiResult<Value> {
    let metadata="json_remove(payload,'$.preparationResearch','$.companyKnowledgeCoverage','$.posts','$.branches','$.items','$.conversations','$.proposals','$.approvals','$.operations','$.materials','$.jobs','$.audit','$.knowledge_entries','$.knowledge_versions','$.feedback')";
    let stats=format!("SELECT length(CAST({metadata} AS BLOB)) FROM workspace WHERE id=1");let size:i64=sqlx::query_scalar(sqlx::AssertSqlSafe(stats.as_str())).fetch_one(&mut *connection).await?;
    if size<0{return Err(internal("Invalid media metadata byte count"));}budget.check(0,size as usize)?;
    let statement=format!("SELECT {metadata} FROM workspace WHERE id=1");let raw:String=sqlx::query_scalar(sqlx::AssertSqlSafe(statement.as_str())).fetch_one(&mut *connection).await?;
    let mut view=root(&raw,budget)?;
    view["proposals"]=json!(sqlite_rows(connection,"proposals",SQLITE_IDS,&[proposal_id.into()],&json!({}),budget).await?);
    if crate::list(&view,"proposals").len()!=1{return Err(ApiError(StatusCode::NOT_FOUND,"Selected proposal missing or duplicated".into()));}
    view["jobs"]=json!(super::reads::dispatch::dependency_jobs_sqlite_bounded(connection,rows(&view,"proposals")?,budget.max_rows,budget.max_bytes).await.map_err(|e|if e.1.contains("ambiguous"){ambiguous()}else if e.1.contains("budget"){budget_error()}else{e})?);
    let item_ids=references(&view,"itemId")?;
    view["items"]=json!(sqlite_rows(connection,"items",SQLITE_IDS,&item_ids,&json!({}),budget).await?);
    let ctx=context(&view)?;view["items"]=json!(sqlite_rows(connection,"items",SQLITE_ITEMS,&item_ids,&ctx,budget).await?);
    let ctx=context(&view)?;view["posts"]=json!(sqlite_rows(connection,"posts",SQLITE_POSTS,&source_post_ids(&view)?,&ctx,budget).await?);
    view["branches"]=json!(sqlite_rows(connection,"branches",SQLITE_IDS,&ids(&view,"items","branchId"),&json!({}),budget).await?);
    let ctx=context(&view)?;let extra=sqlite_rows(connection,"jobs",SQLITE_MEDIA_JOBS,&[],&ctx,budget).await?;merge_jobs(&mut view,extra)?;
    let roots=rows(&view,"jobs")?.iter().chain(rows(&view,"proposals")?.iter()).cloned().collect::<Vec<_>>();
    let dependencies=super::reads::dispatch::dependency_jobs_sqlite_bounded(connection,&roots,budget.max_rows,budget.max_bytes).await.map_err(|e|if e.1.contains("ambiguous"){ambiguous()}else if e.1.contains("budget"){budget_error()}else{e})?;merge_jobs(&mut view,dependencies)?;
    view["jobs"]=json!(sqlite_rows(connection,"jobs",SQLITE_IDS,&ids(&view,"jobs","id"),&json!({}),budget).await?);
    let item_ids=references(&view,"itemId")?;
    view["items"]=json!(sqlite_rows(connection,"items",SQLITE_IDS,&item_ids,&json!({}),budget).await?);
    let ctx=context(&view)?;view["items"]=json!(sqlite_rows(connection,"items",SQLITE_ITEMS,&item_ids,&ctx,budget).await?);
    let ctx=context(&view)?;view["posts"]=json!(sqlite_rows(connection,"posts",SQLITE_POSTS,&source_post_ids(&view)?,&ctx,budget).await?);
    view["branches"]=json!(sqlite_rows(connection,"branches",SQLITE_IDS,&ids(&view,"items","branchId"),&json!({}),budget).await?);
    let ctx=context(&view)?;view["knowledge_entries"]=json!(sqlite_rows(connection,"knowledge_entries",SQLITE_HEADS,&[],&ctx,budget).await?);
    let peers=json!({"entryIds":ids(&view,"knowledge_entries","id")});view["knowledge_entries"]=json!(sqlite_rows(connection,"knowledge_entries",SQLITE_HEAD_PEERS,&ids(&view,"knowledge_entries","sourceMaterialId"),&peers,budget).await?);
    let mut versions=ids(&view,"knowledge_entries","currentVersionId");versions.extend(ctx["versionIds"].as_array().unwrap().iter().filter_map(|v|v.as_str().map(str::to_owned)));versions.sort();versions.dedup();
    view["knowledge_versions"]=json!(sqlite_rows(connection,"knowledge_versions",SQLITE_IDS,&versions,&json!({}),budget).await?);
    media_sources_sqlite(connection,&mut view,budget).await?;
    let ctx=context(&view)?;
    view["materials"]=json!(sqlite_rows(connection,"materials",SQLITE_MATERIALS,&ids(&view,"knowledge_entries","sourceMaterialId"),&ctx,budget).await?);
    research_sqlite(connection,&mut view,budget).await?;
    let mut controls=view.clone();super::scope_context::project(&view,&mut controls)?;view=controls;
    validate_selected(&view,proposal_id,budget)?;Ok(view)
}
impl Database {
    pub(crate) async fn read_media_gate_context(&self,proposal_id:&str,budget:MediaGateReadBudget)->ApiResult<Value> {
        if proposal_id.is_empty()||proposal_id.len()>512||proposal_id.chars().any(char::is_control){return Err(crate::bad("Invalid media proposal ID"));}
        budget.check(0,0)?;
        let _span=crate::performance::Span::new("media_context.selected.read");
        match self {
            Self::Postgres{reader,..}=>{
                let mut tx=reader.begin().await?;sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ READ ONLY").execute(&mut *tx).await?;
                let view=selected_pg(&mut tx,proposal_id,&budget).await?;tx.commit().await?;Ok(view)
            }
            Self::Sqlite(pool)=>{let mut tx=pool.begin().await?;let view=selected_sqlite(&mut tx,proposal_id,&budget).await?;tx.commit().await?;Ok(view)}
        }
    }
}
#[cfg(test)]
#[path="storage_media_gate_context_tests.rs"]
mod tests;