//! Read-only dispatch validation input. Keep the complete evidence corpus:
//! multi-recipient bundles, cross-post author history, media conflicts and
//! knowledge integrity cannot be reconstructed from only the target item.
//! Operation receipts and job output are projected before transfer/decoding;
//! routing, preparation bundles and media source evidence remain intact.
//! Omitted histories must never be persisted as a replacement workspace.
use super::*;
use serde_json::json;
use sqlx::{Acquire,SqliteConnection};

// Quarantine is an existence check over operation routing, not a workspace
// snapshot. Keep JSON binding values intact for the shared domain predicate.
const SQLITE_REPLY_BLOCKERS: &str = r#"
SELECT json_object(
 'sourceValid',json_type(w.payload,'$.account')='text' AND json_type(w.payload,'$.operations')='array',
 'operations',json(COALESCE((SELECT json_group_array(json_object(
   'id',json_extract(o.value,'$.id'),'status',json_extract(o.value,'$.status'),
   'itemId',json_extract(o.value,'$.itemId'),'proposalId',json_extract(o.value,'$.proposalId'),
   'approvalId',json_extract(o.value,'$.approvalId'),
   'target',json_object('connectorBinding',json_extract(o.value,'$.target.connectorBinding')),
   'action',json_object('action',json_extract(o.value,'$.action.action'),'conversationKey',json_extract(o.value,'$.action.conversationKey'))
 )) FROM json_each(w.payload,'$.operations') o
 WHERE json_extract(o.value,'$.status')='unknown'
   AND json_extract(o.value,'$.action.conversationKey')=?1),'[]')))
FROM workspace w WHERE w.id=1
"#;
const PG_REPLY_BLOCKERS: &str = r#"
SELECT w.execution_enabled,
 (jsonb_typeof(w.metadata)='object' AND jsonb_typeof(w.metadata->'account')='string'
   AND w.account=w.metadata->>'account') IS TRUE AS identity_valid,
 COALESCE((SELECT jsonb_agg(jsonb_build_object(
   'id',o.payload->'id','status',o.payload->'status',
   'itemId',o.payload->'itemId','proposalId',o.payload->'proposalId','approvalId',o.payload->'approvalId',
   'target',jsonb_build_object('connectorBinding',o.payload#>'{target,connectorBinding}'),
   'action',jsonb_build_object('action',o.payload#>'{action,action}','conversationKey',o.payload#>'{action,conversationKey}'),
   'relationalValid',(jsonb_typeof(o.payload)='object' AND jsonb_typeof(o.payload->'id')='string'
     AND o.id=o.payload->>'id' AND o.ordinal>=0
     AND o.status IS NOT DISTINCT FROM o.payload->>'status'
     AND o.item_id IS NOT DISTINCT FROM o.payload->>'itemId'
     AND o.proposal_id IS NOT DISTINCT FROM o.payload->>'proposalId'
     AND o.approval_id IS NOT DISTINCT FROM o.payload->>'approvalId') IS TRUE
 ) ORDER BY o.ordinal) FROM communityhero.operations o
 WHERE o.workspace_id=w.id AND (o.status='unknown' OR o.payload->>'status'='unknown')
   AND o.payload#>>'{action,conversationKey}'=$2),'[]'::jsonb)::text AS operations
FROM communityhero.workspaces w WHERE w.id=$1
"#;

fn reply_blocker_from_candidates(candidates:Value,operation:&Value)->ApiResult<Option<String>> {
    let records=candidates.as_array().ok_or_else(||internal("Invalid operation candidates"))?;
    let mut seen=HashSet::new();
    for record in records {
        if !record.is_object() || !seen.insert(text(record,"id")?)
            || record.get("relationalValid").is_some_and(|valid|valid!=true) {
            return Err(internal("Invalid operation candidate identity"));
        }
        for (_,field) in projection("operations") {
            if !record[*field].is_null()&&!record[*field].is_string() {
                return Err(internal("Invalid operation candidate projection"));
            }
        }
    }
    Ok(crate::dispatch_evidence::conversation_blocker(&serde_json::json!({"operations":candidates}),operation))
}

const OMITTED: &[&str] = &["conversations", "approvals", "audit", "feedback"];
// Durable asset ledgers (including UNKNOWN attempts) and exact applicability
// proofs are readonly authority input, independent of a proposal's prepare run.
// Preserve the whole carrier so domain validation can reject foreign/malformed
// company bindings. The surrounding reader still scopes every row to workspace.
const ANALYSIS_JOBS: &[&str] = &["media_analysis", "media_analysis_applicability"];
#[cfg(test)]
fn retained_analysis_job(value:&Value)->bool {
    value["kind"].as_str().is_some_and(|kind|ANALYSIS_JOBS.contains(&kind))
}
fn analysis_job_kinds_sql()->String {
    ANALYSIS_JOBS.iter().map(|kind|format!("'{kind}'")).collect::<Vec<_>>().join(",")
}
fn retained_analysis_job_sql(input:&str,postgres:bool)->String {
    let kinds=analysis_job_kinds_sql();
    if postgres {
        format!("(jsonb_typeof({input}->'kind')='string' AND {input}->>'kind' IN ({kinds}))")
    } else {
        format!("(json_type({input},'$.kind')='text' AND json_extract({input},'$.kind') IN ({kinds}))")
    }
}
// One contract drives both SQL dialects. Preserve missing versus explicit null,
// exact JSON binding equality, and malformed containers for normal validation.
// These are read-only views, never inputs to workspace persistence.
fn dispatch_fields(path: &str) -> &'static [&'static str] {
    match path {
        "operations" => &["id", "itemId", "proposalId", "approvalId", "status", "target", "action", "approvedEditorialReceiptSha256", "approvedPhotoAcquisitionProof"],
        "operations.target" => &["connectorBinding"],
        "operations.action" => &["action", "conversationKey"],
        "jobs" => &["id", "kind", "status", "refId", "prepareBundle", "purpose", "visualContractVersion",
            "account", "connectorBinding", "createdAt", "result"],
        "jobs.result" => &["visualProgress"],
        // Complete source/reference and identity objects are intentional: the
        // media validator owns their format. All candidate jobs retain ordering.
        "jobs.result.visualProgress" => &["phase", "resumePhase", "schemaVersion", "sourcePostId",
            "sourceVersion", "account", "connectorBinding", "sourcePostKey", "sourceIdentity", "source"],
        _ => &[],
    }
}

fn compact_payload_sql(path: &str, input: &str, postgres: bool, depth: usize) -> String {
    let fields = dispatch_fields(path);
    if fields.is_empty() { return input.to_owned(); }
    let alias = format!("dispatch_field_{depth}");
    let keys = fields.iter().map(|key| format!("'{key}'")).collect::<Vec<_>>().join(",");
    // json_each exposes SQL scalar values; re-encode by JSON type so booleans,
    // strings (including JSON-looking strings), arrays and null never coerce.
    let raw = if postgres { format!("{alias}.value") } else {
        format!("CASE {alias}.type WHEN 'text' THEN json_quote({alias}.value) WHEN 'true' THEN 'true' WHEN 'false' THEN 'false' WHEN 'null' THEN 'null' ELSE {alias}.value END")
    };
    let nested = fields.iter().filter_map(|key| {
        let child = format!("{path}.{key}");
        (!dispatch_fields(&child).is_empty()).then(|| format!(" WHEN {alias}.key='{key}' THEN {}",
            compact_payload_sql(&child, &raw, postgres, depth + 1)))
    }).collect::<String>();
    let value = if nested.is_empty() { raw } else { format!("CASE{nested} ELSE {raw} END") };
    let compact=if postgres {
        format!("CASE WHEN jsonb_typeof({input})='object' THEN COALESCE((SELECT jsonb_object_agg({alias}.key,{value}) FROM jsonb_each({input}) {alias} WHERE {alias}.key IN ({keys})),'{{}}'::jsonb) ELSE {input} END")
    } else {
        format!("CASE WHEN json_type({input})='object' THEN (SELECT json_group_object({alias}.key,json({value})) FROM json_each({input}) {alias} WHERE {alias}.key IN ({keys})) ELSE {input} END")
    };
    if path=="jobs" {
        format!("CASE WHEN {} THEN {input} ELSE ({compact}) END",retained_analysis_job_sql(input,postgres))
    } else { compact }
}

#[cfg(test)]
pub(super) fn compact_context_record(value: &Value, path: &str) -> Value {
    if path=="jobs" && retained_analysis_job(value) {return value.clone();}
    let fields = dispatch_fields(path);
    let Some(object) = value.as_object().filter(|_| !fields.is_empty()) else { return value.clone(); };
    Value::Object(object.iter().filter(|(key, _)| fields.contains(&key.as_str()))
        .map(|(key, value)| (key.clone(), compact_context_record(value, &format!("{path}.{key}"))))
        .collect())
}

const SQLITE_DISPATCH: &str = r#"
SELECT json_set(json_remove(w.payload,'$.conversations','$.approvals','$.audit','$.feedback','$.proposals','$.jobs','$.operations'),
 '$.conversations',json('[]'),'$.approvals',json('[]'),'$.audit',json('[]'),'$.feedback',json('[]'),
 '$.operations',json(CASE WHEN json_type(w.payload,'$.operations')='array' THEN
   (SELECT json_group_array(json({operations})) FROM json_each(w.payload,'$.operations') o)
   ELSE json_extract(w.payload,'$.operations') END),
 '$.proposals',json(COALESCE((SELECT json_group_array(json(p.value))
   FROM json_each(w.payload,'$.proposals') p WHERE json_extract(p.value,'$.id')=?1),'[]')),
 '$.jobs',json(COALESCE((SELECT json_group_array(json({jobs}))
   FROM json_each(w.payload,'$.jobs') j WHERE json_extract(j.value,'$.id') IN (
      SELECT json_extract(p.value,'$.prepareRunId') FROM json_each(w.payload,'$.proposals') p WHERE json_extract(p.value,'$.id')=?1
      UNION SELECT json_extract(p.value,'$.recovery.prepareRunId') FROM json_each(w.payload,'$.proposals') p WHERE json_extract(p.value,'$.id')=?1
   ) OR (json_extract(j.value,'$.kind')='media'
     AND json_extract(j.value,'$.purpose')='auto_media'
     AND json_type(j.value,'$.visualContractVersion')='integer'
     AND json_extract(j.value,'$.visualContractVersion')=2) OR {analysis_jobs}),'[]'))) AS projection
FROM workspace w WHERE w.id=1
"#;
fn sqlite_dispatch_statement() -> String {
    SQLITE_DISPATCH.replace("{operations}", &compact_payload_sql("operations", "o.value", false, 0))
        .replace("{jobs}", &compact_payload_sql("jobs", "j.value", false, 0))
        .replace("{analysis_jobs}", &retained_analysis_job_sql("j.value",false))
}
// Video source fingerprints can depend on ffprobe duration retained in a v2
// media job. Preserve every eligible job so the projection makes the same
// latest-source choice as the full workspace.
const PG_JOBS: &str = r#"(id IN (
 SELECT p.payload->>'prepareRunId' FROM communityhero.proposals p WHERE p.workspace_id=$1 AND p.id=$2
 UNION SELECT p.payload#>>'{recovery,prepareRunId}' FROM communityhero.proposals p WHERE p.workspace_id=$1 AND p.id=$2
 ) OR (payload->>'kind'='media' AND payload->>'purpose'='auto_media'
   AND payload->'visualContractVersion'='2'::jsonb) OR {analysis_jobs})"#;
fn pg_dispatch_jobs_statement()->String {
    // Include either identity signal. A corrupted carrier must reach parse_pg's
    // physical/payload comparison instead of disappearing from the authority view.
    let retained=format!("(kind IN ({}) OR {})",analysis_job_kinds_sql(),retained_analysis_job_sql("payload",true));
    PG_JOBS.replace("{analysis_jobs}",&retained)
}

struct MaterialJobRoots { ids:Vec<String>, full:bool }
fn needs_material_jobs(proposals:&[Value])->bool {
    proposals.iter().any(|p|["modelMaterialReceipt","editorialModelMaterialReceipt"]
        .iter().any(|key|p.get(*key).is_some()))
}
// Current proof validators consume the selected proposal's native pointer,
// not an operation's historical proposal snapshot. Operation review hashes
// remain intact; this resolver never retargets them or mints proof authority.
async fn material_roots_pg(connection:&mut PgConnection,proposals:&[Value])->ApiResult<Option<MaterialJobRoots>> {
    if !needs_material_jobs(proposals){return Ok(None);}
    let jobs=dependency_jobs_pg(connection,proposals).await?;
    let (ids,_,full)=super::source_snapshot::scoped_job_dependencies(&jobs,proposals);
    Ok(Some(MaterialJobRoots{ids,full}))
}
// Shared by authority readers: roots may also include an exact current job or
// retained proposal snapshot. Complete bodies and duplicate bundle families are
// discovered inside the caller's transaction; malformed lineage stays broad.
pub(in crate::storage) async fn dependency_jobs_pg(connection:&mut PgConnection,roots:&[Value])->ApiResult<Vec<Value>> {
    dependency_jobs_pg_limited(connection,roots,None).await
}
pub(in crate::storage) async fn dependency_jobs_pg_bounded(connection:&mut PgConnection,roots:&[Value],rows:usize,bytes:usize)->ApiResult<Vec<Value>> {
    dependency_jobs_pg_limited(connection,roots,Some((rows,bytes))).await
}
async fn dependency_jobs_pg_limited(connection:&mut PgConnection,roots:&[Value],limit:Option<(usize,usize)>)->ApiResult<Vec<Value>> {
    let mut bounded_iterations=0usize;
    let mut jobs=Vec::new();let mut previous=None;
    loop {
        if limit.is_some(){bounded_iterations+=1;if bounded_iterations>64{return Err(internal("Bounded native job dependency scope ambiguous: iteration limit"));}}
        let (ids,bundles,full)=super::source_snapshot::scoped_job_dependencies(&jobs,roots);
        if full&&limit.is_some(){return Err(internal("Bounded native job dependency scope ambiguous"));}
        let key=(ids.clone(),bundles.clone(),full);
        if previous.as_ref()==Some(&key){return Ok(jobs);}
        previous=Some(key);
        if let Some((max_rows,max_bytes))=limit {
            let stats=sqlx::query("SELECT count(*)::bigint AS count,COALESCE(sum(octet_length(payload::text)),0)::bigint AS bytes FROM communityhero.jobs WHERE workspace_id=$1 AND ($4::boolean OR id=ANY($2::text[]) OR payload->>'id'=ANY($2::text[]) OR payload#>>'{prepareBundle,id}'=ANY($3::text[]))")
                .bind(WORKSPACE).bind(&ids).bind(&bundles).bind(full).fetch_one(&mut *connection).await?;
            let count=stats.try_get::<i64,_>("count")?;let bytes=stats.try_get::<i64,_>("bytes")?;
            if count<0||bytes<0||count as usize>max_rows||bytes as usize>max_bytes{return Err(internal("Bounded native job dependency budget exceeded"));}
        }
        let mut fetch=crate::performance::Span::new("source.snapshot.load.jobs.bodies.fetch");
        let records=sqlx::query("SELECT id,ordinal,kind,ref_id,status,payload::text FROM communityhero.jobs WHERE workspace_id=$1 AND ($4::boolean OR id=ANY($2::text[]) OR payload->>'id'=ANY($2::text[]) OR payload#>>'{prepareBundle,id}'=ANY($3::text[])) ORDER BY ordinal LIMIT $5")
            .bind(WORKSPACE).bind(ids).bind(bundles).bind(full).bind(limit.map(|(rows,_)|rows as i64+1).unwrap_or(i64::MAX)).fetch_all(&mut *connection).await?;
        let bytes=records.iter().map(|row|row.try_get::<&str,_>("payload").map(str::len)).collect::<Result<Vec<_>,_>>()?.into_iter().sum::<usize>();
        fetch.measurements(crate::performance::StorageMeasurements{payload_read:crate::performance::ReadMeasurements{
            rows:Some(records.len() as u64),bytes:Some(bytes as u64),statements:Some(1)},
            fallback_reason:full.then_some("source_scope_legacy_fallback"),..Default::default()});drop(fetch);
        if limit.is_some_and(|(rows,max_bytes)|records.len()>rows||bytes>max_bytes){return Err(internal("Bounded native job dependency budget exceeded"));}
        let decode=crate::performance::Span::new("source.snapshot.load.jobs.bodies.decode");
        let mut next=Vec::with_capacity(records.len());let mut seen=HashSet::new();let mut ordinal=None;
        for record in records {
            let value=parse(record.try_get::<&str,_>("payload")?)?;let id=text(&value,"id")?;
            let position=record.try_get::<i32,_>("ordinal")?;
            if record.try_get::<&str,_>("id")?!=id||!seen.insert(id.to_owned())||position<0||ordinal.is_some_and(|old|old>=position){return Err(internal("Dispatch material job identity/order mismatch"));}
            for(column,field)in projection("jobs"){if record.try_get::<Option<String>,_>(*column)?.as_deref()!=value[*field].as_str(){return Err(internal("Dispatch material job projection mismatch"));}}
            ordinal=Some(position);next.push(value);
        }
        jobs=next;drop(decode);
    }
}
async fn material_roots_sqlite(connection:&mut SqliteConnection,proposal_id:&str)->ApiResult<Option<MaterialJobRoots>> {
    let proposals=sqlx::query_scalar::<_,String>("SELECT p.value FROM workspace w,json_each(w.payload,'$.proposals') p WHERE w.id=1 AND json_extract(p.value,'$.id')=?1")
        .bind(proposal_id).fetch_all(&mut *connection).await?.into_iter().map(|raw|parse(&raw)).collect::<ApiResult<Vec<_>>>()?;
    if !needs_material_jobs(&proposals){return Ok(None);}
    let jobs=dependency_jobs_sqlite(connection,&proposals).await?;
    let(ids,_,full)=super::source_snapshot::scoped_job_dependencies(&jobs,&proposals);
    Ok(Some(MaterialJobRoots{ids,full}))
}
pub(in crate::storage) async fn dependency_jobs_sqlite(connection:&mut SqliteConnection,roots:&[Value])->ApiResult<Vec<Value>> {
    dependency_jobs_sqlite_limited(connection,roots,None).await
}
pub(in crate::storage) async fn dependency_jobs_sqlite_bounded(connection:&mut SqliteConnection,roots:&[Value],rows:usize,bytes:usize)->ApiResult<Vec<Value>> {
    dependency_jobs_sqlite_limited(connection,roots,Some((rows,bytes))).await
}
async fn dependency_jobs_sqlite_limited(connection:&mut SqliteConnection,roots:&[Value],limit:Option<(usize,usize)>)->ApiResult<Vec<Value>> {
    let mut bounded_iterations=0usize;
    let mut jobs=Vec::new();let mut previous=None;
    loop {
        if limit.is_some(){bounded_iterations+=1;if bounded_iterations>64{return Err(internal("Bounded native job dependency scope ambiguous: iteration limit"));}}
        let(ids,bundles,full)=super::source_snapshot::scoped_job_dependencies(&jobs,roots);
        if full&&limit.is_some(){return Err(internal("Bounded native job dependency scope ambiguous"));}
        let key=(ids.clone(),bundles.clone(),full);
        if previous.as_ref()==Some(&key){return Ok(jobs);}
        previous=Some(key);
        if let Some((max_rows,max_bytes))=limit {
            let stats=sqlx::query("SELECT count(*) AS count,COALESCE(sum(length(CAST(j.value AS BLOB))),0) AS bytes FROM workspace w,json_each(w.payload,'$.jobs') j WHERE w.id=1 AND (?3=1 OR json_extract(j.value,'$.id') IN (SELECT value FROM json_each(?1)) OR json_extract(j.value,'$.prepareBundle.id') IN (SELECT value FROM json_each(?2)))")
                .bind(json!(ids).to_string()).bind(json!(bundles).to_string()).bind(full).fetch_one(&mut *connection).await?;
            let count=stats.try_get::<i64,_>("count")?;let bytes=stats.try_get::<i64,_>("bytes")?;
            if count<0||bytes<0||count as usize>max_rows||bytes as usize>max_bytes{return Err(internal("Bounded native job dependency budget exceeded"));}
        }
        let mut fetch=crate::performance::Span::new("source.snapshot.load.jobs.bodies.fetch");
        let records=sqlx::query_scalar::<_,String>("SELECT j.value FROM workspace w,json_each(w.payload,'$.jobs') j WHERE w.id=1 AND (?3=1 OR json_extract(j.value,'$.id') IN (SELECT value FROM json_each(?1)) OR json_extract(j.value,'$.prepareBundle.id') IN (SELECT value FROM json_each(?2))) ORDER BY CAST(j.key AS INTEGER) LIMIT ?4")
            .bind(json!(ids).to_string()).bind(json!(bundles).to_string()).bind(full).bind(limit.map(|(rows,_)|rows as i64+1).unwrap_or(i64::MAX)).fetch_all(&mut *connection).await?;
        fetch.measurements(crate::performance::StorageMeasurements{payload_read:crate::performance::ReadMeasurements{
            rows:Some(records.len() as u64),bytes:Some(records.iter().map(String::len).sum::<usize>() as u64),statements:Some(1)},
            fallback_reason:full.then_some("source_scope_legacy_fallback"),..Default::default()});drop(fetch);
        if limit.is_some_and(|(rows,bytes)|records.len()>rows||records.iter().map(String::len).sum::<usize>()>bytes){return Err(internal("Bounded native job dependency budget exceeded"));}
        let decode=crate::performance::Span::new("source.snapshot.load.jobs.bodies.decode");
        jobs=records.into_iter().map(|raw|parse(&raw)).collect::<ApiResult<Vec<_>>>()?;drop(decode);
    }
}
fn sqlite_material_dispatch_statement()->String {
    let selected="(?3=1 OR json_extract(j.value,'$.id') IN (SELECT value FROM json_each(?2)))";
    let jobs=format!("CASE WHEN {selected} THEN j.value ELSE ({}) END",compact_payload_sql("jobs","j.value",false,0));
    let retained=format!("({} OR {selected})",retained_analysis_job_sql("j.value",false));
    SQLITE_DISPATCH.replace("{operations}",&compact_payload_sql("operations","o.value",false,0))
        .replace("{jobs}",&jobs).replace("{analysis_jobs}",&retained)
}

fn validate_context(value: &mut Value) -> ApiResult<()> {
    // Keep normal retained-source identity/FK validation. Only operation links
    // into deliberately omitted approvals/other proposals are outside this view.
    // Check their types before omitting those two edges from validation.
    let mut operation_refs = Vec::new();
    for operation in rows(value, "operations")? {
        for (_, field) in projection("operations") {
            if !operation[*field].is_null() && !operation[*field].is_string() {
                return Err(internal("Invalid projected field"));
            }
        }
        operation_refs.push(serde_json::json!({"id":operation["id"],"itemId":operation["itemId"],"status":operation["status"]}));
    }
    let operations = std::mem::replace(&mut value["operations"], Value::Array(operation_refs));
    let result = validate(value);
    value["operations"] = operations;
    result
}

fn parse_pg(record: sqlx::postgres::PgRow, tables: Vec<(&str, Vec<sqlx::postgres::PgRow>)>) -> ApiResult<Value> {
    let decoding = crate::performance::Span::new("dispatch.context.decode");
    postgres_guard(&record)?;
    let mut value = parse(record.try_get::<&str, _>("metadata")?)?;
    if !value.is_object() || !value["account"].is_string() {
        return Err(internal("Workspace identity mismatch"));
    }
    for table in TABLES {
        if value.get(table).is_some() { return Err(internal("Workspace metadata contains entity collections")); }
        value[table] = Value::Array(vec![]);
    }
    for (table, records) in tables {
        let mut values = Vec::with_capacity(records.len());
        let mut seen = HashSet::new();
        let mut last_ordinal = None;
        for (index, record) in records.into_iter().enumerate() {
            let payload = parse(record.try_get::<&str, _>("payload")?)?;
            let ordinal = record.try_get::<i32, _>("ordinal")?;
            let partial = matches!(table, "proposals" | "jobs");
            if record.try_get::<&str, _>("id")? != text(&payload, "id")?
                || !seen.insert(text(&payload, "id")?.to_owned())
                || (!partial && ordinal != index as i32)
                || ordinal < 0 || last_ordinal.is_some_and(|previous| ordinal <= previous) {
                return Err(internal("Record identity or order mismatch"));
            }
            for (column, field) in projection(table) {
                if (!payload[*field].is_null() && !payload[*field].is_string())
                    || record.try_get::<Option<String>, _>(*column)?.as_deref() != payload[*field].as_str() {
                    return Err(internal("Record relational projection mismatch"));
                }
            }
            last_ordinal = Some(ordinal);
            values.push(payload);
        }
        value[table] = Value::Array(values);
    }
    drop(decoding);
    let _validation = crate::performance::Span::new("dispatch.context.validate");
    validate_context(&mut value)?;
    Ok(value)
}

impl Database {
    async fn owner_close_dispatch_context(&self,compact:Value,proposal_id:&str)->ApiResult<Value>{
        if rows(&compact,"proposals")?.iter().any(|p|p["id"]==proposal_id&&p.get(crate::retained_paid_recovery::FIELD).is_some()){
            // Actual dispatch rechecks the exact original paid owner, receipt,
            // approval and own operation. Compact rows omit that authority.
            return self.read().await;
        }
        if rows(&compact,"proposals")?.iter().any(|p|p["id"]==proposal_id&&!p["operatorCloseDecision"].is_null()){
            // Distinct owner closes bind immutable UNKNOWN receipts and the
            // current own operation. Reuse the consistent admission dependency
            // closure; compact records cannot authenticate those full hashes.
            return self.read_operator_editorial(&serde_json::json!({"proposals":[{"id":proposal_id}]})).await;
        }
        Ok(compact)
    }
    /// One statement snapshot; no histories, job payloads or receipt bodies are
    /// decoded. Errors propagate before external execution, never as no blocker.
    pub(crate) async fn read_reply_conversation_blocker(&self, operation:&Value)->ApiResult<Option<String>> {
        if operation["action"]["action"]!="reply_and_close" {return Ok(None);}
        text(operation,"id")?;
        let conversation=text(&operation["action"],"conversationKey")?;
        let _timing=crate::performance::Span::new("dispatch.reply_blocker.read");
        let candidates=match self {
            Self::Sqlite(pool)=>{
                let raw:String=sqlx::query_scalar(SQLITE_REPLY_BLOCKERS).bind(conversation).fetch_one(pool).await?;
                let value=parse(&raw)?;
                if value["sourceValid"]!=1 {return Err(internal("Invalid operation source"));}
                value["operations"].clone()
            }
            Self::Postgres {reader,..}=>{
                let record=sqlx::query(PG_REPLY_BLOCKERS).bind(WORKSPACE).bind(conversation).fetch_one(reader).await?;
                postgres_guard(&record)?;
                parse(record.try_get::<&str,_>("operations")?)?
            }
        };
        reply_blocker_from_candidates(candidates,operation)
    }
    pub(crate) async fn read_dispatch_context(&self, proposal_id: &str) -> ApiResult<Value> {
        let _timing=crate::performance::Span::new("dispatch.context.read");
        if let Some(value)=self.read_bounded_owner_close(proposal_id).await? {return Ok(value);}
        match self {
            Self::Sqlite(pool) => {
                let waiting = crate::performance::Span::new("dispatch.context.pool_wait");
                let mut connection = pool.acquire().await?;
                drop(waiting);
                let reading = crate::performance::Span::new("dispatch.context.sql");
                let mut tx=connection.begin().await?;
                let roots=material_roots_sqlite(&mut tx,proposal_id).await?;
                let statement=if roots.is_some(){sqlite_material_dispatch_statement()}else{sqlite_dispatch_statement()};
                let mut query=sqlx::query_scalar(sqlx::AssertSqlSafe(statement.as_str())).bind(proposal_id);
                if let Some(roots)=roots{query=query.bind(json!(roots.ids).to_string()).bind(roots.full);}
                let payload:String=query.fetch_one(&mut *tx).await?;
                tx.commit().await?;
                drop(connection);
                drop(reading);
                let decoding = crate::performance::Span::new("dispatch.context.decode");
                let mut value = parse(&payload)?;
                normalize(&mut value);
                drop(decoding);
                let _validation = crate::performance::Span::new("dispatch.context.validate");
                validate_context(&mut value)?;
                self.owner_close_dispatch_context(value,proposal_id).await
            }
            Self::Postgres { reader, .. } => {
                let waiting = crate::performance::Span::new("dispatch.context.pool_wait");
                let mut connection = reader.acquire().await?;
                drop(waiting);
                let reading = crate::performance::Span::new("dispatch.context.sql");
                let mut tx = connection.begin().await?;
                sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ READ ONLY").execute(&mut *tx).await?;
                let record = sqlx::query("SELECT metadata::text,execution_enabled,(jsonb_typeof(metadata)='object' AND jsonb_typeof(metadata->'account')='string' AND account=metadata->>'account') IS TRUE AS identity_valid FROM communityhero.workspaces WHERE id=$1")
                    .bind(WORKSPACE).fetch_one(&mut *tx).await?;
                postgres_guard(&record)?;
                let mut tables:Vec<(&str,Vec<sqlx::postgres::PgRow>)> = Vec::new();
                let jobs_condition=pg_dispatch_jobs_statement();
                for table in TABLES {
                    if OMITTED.contains(&table) { continue; }
                    let mut condition = match table { "proposals" => "id=$2", "jobs" => jobs_condition.as_str(), _ => "TRUE" }.to_owned();
                    // Table/projection identifiers and predicates are constants;
                    // workspace and selected proposal are always bound values.
                    let mut payload = compact_payload_sql(table, "payload", true, 0);
                    let roots=if table=="jobs" {
                        let selected=tables.iter().find(|(name,_)|*name=="proposals").ok_or_else(||internal("Dispatch proposal projection missing"))?;
                        let proposals=selected.1.iter().map(|row:&sqlx::postgres::PgRow|parse(row.try_get::<&str,_>("payload")?)).collect::<ApiResult<Vec<_>>>()?;
                        material_roots_pg(&mut tx,&proposals).await?
                    }else{None};
                    if roots.is_some(){
                        let selected="($4::boolean OR id=ANY($3::text[]) OR payload->>'id'=ANY($3::text[]))";
                        condition=format!("({condition} OR {selected})");payload=format!("CASE WHEN {selected} THEN payload ELSE ({payload}) END");
                    }
                    let statement = format!("SELECT id,ordinal,({payload})::text AS payload{} FROM communityhero.{table} WHERE workspace_id=$1 AND {condition} ORDER BY ordinal",
                        projection(table).iter().map(|(column, _)| format!(",{column}")).collect::<String>());
                    let mut query = sqlx::query(sqlx::AssertSqlSafe(statement.as_str())).bind(WORKSPACE);
                    if matches!(table, "proposals" | "jobs") { query = query.bind(proposal_id); }
                    if let Some(roots)=roots{query=query.bind(roots.ids).bind(roots.full);}
                    tables.push((table, query.fetch_all(&mut *tx).await?));
                }
                // All bytes come from one repeatable-read snapshot. Release the
                // scarce reader before JSON decoding and domain validation.
                tx.commit().await?;
                drop(connection);
                drop(reading);
                let value=parse_pg(record,tables)?;
                self.owner_close_dispatch_context(value,proposal_id).await
            }
        }
    }
}

#[cfg(test)]
#[path = "storage_dispatch_blocker_tests.rs"]
mod blocker_tests;

#[cfg(test)]
#[path = "storage_dispatch_compact_tests.rs"]
mod compact_tests;

#[cfg(test)]
#[path="storage_dispatch_media_analysis_tests.rs"]
mod media_analysis_tests;

#[path = "storage_owner_close.rs"]
mod owner_close;
