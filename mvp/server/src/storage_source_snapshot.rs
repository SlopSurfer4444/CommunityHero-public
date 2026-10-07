//! Source admission retains the complete semantic evidence corpus, but never
//! loads unrelated dialogue/feedback/audit history or embedded approval bodies.
//! This projection is not a replacement workspace: persistence is whitelist-only.
use super::*;
use crate::{bad, ApiError};
use serde_json::json;
#[path="storage_source_delta.rs"]
mod delta;
#[path="storage_source_jobs.rs"]
mod source_jobs;
#[path="storage_dependency_closure.rs"]
mod dependency_closure;
pub(crate) use dependency_closure::SourceReadIntent;
#[path="storage_guarded_delta.rs"]
mod guarded_delta;

/// Returned only after transaction settlement and awaited PG pool return.
/// Full fallback and SQLite retain their existing internal cleanup lifetime.
pub(crate) struct SourceWriteCompletion<T> {
    pub(crate) outcome: ApiResult<(T,bool)>,
    pub(crate) cleanup: SourceProjectionCleanup,
}

#[derive(Default)]
pub(crate) struct SourceProjectionCleanup {
    before: Option<Value>,
    after: Option<Value>,
}
#[cfg(test)]
type CleanupProbe=std::sync::Arc<dyn Fn()->std::pin::Pin<Box<dyn std::future::Future<Output=()>+Send>>+Send+Sync>;
#[cfg(test)]
tokio::task_local! {static CLEANUP_PROBE: CleanupProbe;}
#[cfg(test)]
async fn with_cleanup_probe<T>(probe:CleanupProbe,work:impl std::future::Future<Output=T>)->T {
    CLEANUP_PROBE.scope(probe,work).await
}
impl SourceProjectionCleanup {
    pub(crate) fn has_projections(&self)->bool {self.before.is_some()||self.after.is_some()}
    /// App calls this only after releasing its exact source writer permit.
    pub(crate) async fn dispose(self) {
        if !self.has_projections(){return;}
        #[cfg(test)]
        if let Ok(probe)=CLEANUP_PROBE.try_with(Clone::clone){probe().await;}
        let dropping=crate::performance::Span::new("source.snapshot.drop");
        drop(self.after);drop(self.before);drop(dropping);
    }
}

// A source-only semantic view. Canonical jobs contain FULL selected rows;
// separate complete controls are consumed only by legacy maintenance helpers.
fn project_scoped(workspace:&Value)->ApiResult<Value> {
    if workspace.get("sourceJobControls").is_some(){return Err(internal("Source controls cannot be canonical metadata"));}
    let mut view=Value::Object(workspace.as_object().ok_or_else(||internal("Invalid source workspace"))?
        .iter().filter(|(key,_)|!TABLES.contains(&key.as_str())&&key.as_str()!="preparationResearch")
        .map(|(key,value)|(key.clone(),value.clone())).collect());
    for table in TABLES {
        view[table]=if OMITTED.contains(&table)||table=="jobs" {json!([])}
            else if table=="approvals" {Value::Array(rows(workspace,table)?.iter().map(approval).collect())}
            else {workspace[table].clone()};
    }
    let controls=rows(workspace,"jobs")?.iter().map(source_jobs::source_control).collect::<Vec<_>>();
    let mut closure=dependency_closure::SourceClosure::from_controls(&view,&controls);
    loop {
        let mut expanded=closure.expand_controls(&controls);
        for job in rows(workspace,"jobs")?.iter().filter(|job|job["id"].as_str().is_some_and(|id|closure.full_jobs.contains(id))).collect::<Vec<_>>() {
            expanded|=closure.expand_jobs(job);
        }
        if !expanded {break;}
    }
    view["jobs"]=Value::Array(rows(workspace,"jobs")?.iter()
        .filter(|job|job["id"].as_str().is_some_and(|id|closure.full_jobs.contains(id))).cloned().collect());
    closure.collect_archives(&view["jobs"]);closure.collect_archives(&view["proposals"]);
    if let Some(research)=dependency_closure::research_projection(workspace.get("preparationResearch"),&closure) {
        view["preparationResearch"]=research;
    }
    view["sourceJobControls"]=json!({"version":1,"complete":true,"jobs":controls.into_iter().map(|(value,_)|value).collect::<Vec<_>>()});
    validate(&view)?;validate_source_controls(&view)?;Ok(view)
}

fn validate_source_controls(view:&Value)->ApiResult<()> {
    let controls=&view["sourceJobControls"];
    if controls["version"]!=1||controls["complete"]!=true{return Err(internal("Incomplete source maintenance controls"));}
    let controls=controls["jobs"].as_array().ok_or_else(||internal("Invalid source maintenance controls"))?;
    let mut seen=HashMap::new();
    for control in controls {
        if !control.is_object()||seen.insert(text(control,"id")?,control).is_some(){return Err(internal("Duplicate source maintenance control"));}
        for (_,key) in projection("jobs") {
            if !control[*key].is_null()&&!control[*key].is_string(){return Err(internal("Invalid source maintenance projection"));}
        }
    }
    for job in rows(view,"jobs")? {
        if seen.get(text(job,"id")?).copied()!=Some(&source_jobs::source_control(job).0){
            return Err(internal("Full source job disagrees with maintenance inventory"));
        }
    }
    Ok(())
}

pub(super) fn project_pinned_research(workspace:&Value,view:&mut Value) {
    let mut closure=dependency_closure::SourceClosure::default();
    closure.collect_archives(&view["jobs"]);closure.collect_archives(&view["proposals"]);
    if let Some(research)=dependency_closure::research_projection(workspace.get("preparationResearch"),&closure) {
        view["preparationResearch"]=research;
    }
}
// Existing-proposal authority needs exact paid/fact/native parents and every
// duplicate bundle body, even when its direct generation run was already read.
// A malformed/deep lineage falls back to the complete job corpus.
pub(super) fn scoped_job_dependencies(jobs:&[Value],proposals:&[Value])->(Vec<String>,Vec<String>,bool) {
    let mut closure=dependency_closure::SourceClosure::default();
    let mut bundles=HashSet::new();
    for job in jobs {
        if let Some(id)=job["id"].as_str(){closure.full_jobs.insert(id.to_owned());}
        if let Some(id)=job["prepareBundle"]["id"].as_str().filter(|id|!id.is_empty()){bundles.insert(id.to_owned());}
        closure.expand_jobs(job);
    }
    for proposal in proposals {closure.expand_jobs(proposal);}
    let mut ids=closure.full_jobs.into_iter().collect::<Vec<_>>();ids.sort();
    let mut bundles=bundles.into_iter().collect::<Vec<_>>();bundles.sort();
    (ids,bundles,closure.full_job_inventory)
}
pub(super) async fn load_pinned_research(connection:&mut PgConnection,view:&mut Value)->ApiResult<()> {
    let mut closure=dependency_closure::SourceClosure::default();
    closure.collect_archives(&view["jobs"]);closure.collect_archives(&view["proposals"]);
    let mut ids=closure.archive_ids.iter().cloned().collect::<Vec<_>>();ids.sort();
    let mut span=crate::performance::Span::new("source.snapshot.load.research.fetch");
    let research:Option<String>=sqlx::query_scalar("SELECT CASE WHEN NOT(metadata ? 'preparationResearch') THEN NULL ELSE \
        CASE WHEN jsonb_typeof(metadata->'preparationResearch')='array' AND NOT $3::boolean THEN \
          COALESCE((SELECT jsonb_agg(a.value ORDER BY a.ordinality) FROM jsonb_array_elements(metadata->'preparationResearch') WITH ORDINALITY a(value,ordinality) \
            WHERE a.value->>'id'=ANY($2::text[])),'[]'::jsonb) ELSE metadata->'preparationResearch' END::text END \
        FROM communityhero.workspaces WHERE id=$1")
        .bind(WORKSPACE).bind(ids).bind(closure.full_research).fetch_one(&mut *connection).await?;
    span.measurements(crate::performance::StorageMeasurements{payload_read:crate::performance::ReadMeasurements{
        rows:Some(1),bytes:Some(research.as_ref().map_or(0,|v|v.len()) as u64),statements:Some(1)},
        fallback_reason:closure.full_research.then_some("source_scope_ambiguous_research"),..Default::default()});drop(span);
    if let Some(research)=research{view["preparationResearch"]=parse(&research)?;}
    Ok(())
}

fn validate_scoped_change(before:&Value,after:&Value)->ApiResult<()> {
    validate(after)?;
    validate_source_controls(after)?;
    validate_source_metadata(before,after)?;
    if before["connectorBinding"]!=after["connectorBinding"]{crate::active_binding(after)?;}
    for table in TABLES {
        let old=rows(before,table)?;let new=rows(after,table)?;
        if MUTABLE.contains(&table) {
            if new.len()<old.len()||old.iter().zip(new).any(|(old,new)|old["id"]!=new["id"]) {
                return Err(internal("Scoped source deleted or reordered records"));
            }
        }else if table!="audit"&&old!=new {return Err(internal("Scoped source changed readonly evidence"));}
    }
    // Source cannot invent a proposal/paid provenance or rewrite saved text.
    // No full-history validator receives the filtered canonical job array.
    let old=rows(before,"proposals")?;let new=rows(after,"proposals")?;
    if old.len()!=new.len(){return Err(internal("Source admission cannot create proposals"));}
    for (old,new) in old.iter().zip(new) {
        if old!=new {
            // merge_snapshot only retires an unapproved automatic draft; it
            // never promotes or retires an approval/UNKNOWN publication record.
            let automatic=crate::answering_repair_plan::automatic_proposal_origin(before,old).is_some();
            let revision=old["revision"].as_u64().unwrap_or(0).checked_add(1);
            if old["status"]!="draft"||new["status"]!="stale"||!automatic||new["revision"].as_u64()!=revision
                ||new["staleReason"].as_str().is_none_or(|reason|reason.is_empty())
                ||new["staleAt"].as_str().is_none_or(|at|at.is_empty()) {
                return Err(internal("Source admission changed a protected proposal state"));
            }
        }
        const CHANGES:&[&str]=&["status","staleReason","staleAt","revision"];
        let old=old.as_object().ok_or_else(||internal("Invalid source proposal"))?;
        let new=new.as_object().ok_or_else(||internal("Invalid source proposal"))?;
        if !old.iter().filter(|(key,_)|!CHANGES.contains(&key.as_str()))
            .eq(new.iter().filter(|(key,_)|!CHANGES.contains(&key.as_str()))) {
            return Err(internal("Source admission changed proposal text or provenance"));
        }
    }
    for (old,new) in rows(before,"items")?.iter().zip(rows(after,"items")?) {
        for field in ["itemId","objectId","draft","draftEdited","draftOrigin","draftSessionId"] {
            if old[field]!=new[field]{return Err(internal("Source admission changed recipient or operator draft"));}
        }
        if old.get("connectorBinding").is_some()&&old["connectorBinding"]!=new["connectorBinding"] {
            return Err(internal("Source admission rebound a recipient"));
        }
    }
    let mut seen=HashSet::new();
    for audit in rows(after,"audit")? {
        if !seen.insert(text(audit,"id")?){return Err(internal("Duplicate scoped audit identity"));}
    }
    Ok(())
}

const OMITTED: &[&str] = &["conversations", "audit", "feedback"];
const MUTABLE: &[&str] = &["posts", "branches", "items", "proposals"];
const APPROVAL_FIELDS: &[&str] = &["id", "status", "proposals"];

// Only this source-admission view may omit unreferenced terminal payloads.
// Preserve every row/order and required outcome/control fields: legacy recovery
// still reads terminal job outcomes and bundle digests without a job pointer.
// Review/resume/recovery must obtain their own full job, never reuse this view.
fn protected_jobs(workspace: &Value) -> Option<HashSet<String>> {
    let mut protected = HashSet::new();
    let mut pending = Vec::new();
    for table in ["items", "proposals"] {
        for row in workspace.get(table)?.as_array()? {
            if !row.is_object() { return None; }
            pending.push((row, 0usize));
        }
    }
    while let Some((value, depth)) = pending.pop() {
        // Ambiguous or unexpectedly deep lineage keeps every job in full.
        if depth > 64 { return None; }
        match value {
            Value::Object(fields) => for (key, value) in fields {
                if matches!(key.as_str(), "prepareRunId" | "jobId" | "originalJobId") && !value.is_null() {
                    let id = value.as_str().filter(|id| !id.is_empty())?;
                    protected.insert(id.to_owned());
                }
                if matches!(key.as_str(), "origin" | "recovery" | "autoPreparation" | "autoRevalidation")
                    && !value.is_null() && !value.is_object() { return None; }
                if key == "history" && !value.is_null()
                    && value.as_array().is_none_or(|rows| rows.iter().any(|row| !row.is_object())) { return None; }
                pending.push((value, depth + 1));
            },
            Value::Array(values) => pending.extend(values.iter().map(|value| (value, depth + 1))),
            _ => (),
        }
    }
    Some(protected)
}

fn compactable_job(job: &Value, protected: Option<&HashSet<String>>) -> bool {
    let Some(protected) = protected else { return false; };
    let hash = |v: &Value| v.as_str().is_some_and(|s| s.len() == 64
        && s.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)));
    job.is_object() && job["kind"] == "assistant"
        && matches!(job["status"].as_str(), Some("completed" | "failed" | "cancelled"))
        && job["id"].as_str().is_some_and(|id| !id.is_empty() && !protected.contains(id))
        && job["prepareBundle"].is_object() && job["prepareBundle"]["request"].is_object()
        && job["prepareBundle"]["version"] == 1
        && hash(&job["prepareBundle"]["digest"]) && hash(&job["prepareBundle"]["dependencyDigest"])
        && job["prepareBundle"]["itemIds"].is_array()
}

fn source_job(job: &Value, protected: Option<&HashSet<String>>) -> Value {
    if source_jobs::requires_full_material_job(job){return job.clone();}
    if let Some(view)=source_jobs::project_assistant(job,protected) { return view; }
    if compactable_job(job, protected) {
        // Omitted payloads must never be cloned just to delete them. The SQLite
        // view and in-memory parity fixtures use this path; PostgreSQL applies
        // the matching projection before text transfer.
        let omit_result = job["status"] == "completed" && job["result"].is_object();
        Value::Object(job.as_object().unwrap().iter()
            .filter(|(key,_)| !omit_result || key.as_str() != "result")
            .map(|(key,value)| {
                let value = if key == "prepareBundle" {
                    Value::Object(value.as_object().unwrap().iter()
                        .filter(|(field,_)| field.as_str() != "request")
                        .map(|(field,value)| (field.clone(),value.clone())).collect())
                } else { value.clone() };
                (key.clone(),value)
            }).collect())
    } else if compactable_media_result(job, protected) {
        let progress=job["result"]["visualProgress"].as_object().unwrap();
        let retained=progress.iter().filter(|(key,_)|MEDIA_PROGRESS_FIELDS.contains(&key.as_str()))
            .map(|(key,value)|(key.clone(),value.clone())).collect::<serde_json::Map<_,_>>();
        let mut view:serde_json::Map<_,_>=job.as_object().unwrap().iter()
            .filter(|(key,_)| key.as_str() != "result")
            .map(|(key,value)| (key.clone(),value.clone())).collect();
        view.insert("result".to_owned(),json!({"visualProgress":retained}));
        Value::Object(view)
    } else { job.clone() }
}

// Keep this predicate identical to compactable_job. Parameters are a conservative
// same-transaction reference set and its validity, not an authority to drop rows.
const JOB_PAYLOAD: &str = "CASE WHEN $3::boolean AND jsonb_typeof(payload)='object' \
    AND payload->>'kind'='assistant' AND payload->>'status' IN ('completed','failed','cancelled') \
    AND jsonb_typeof(payload->'id')='string' AND length(payload->>'id')>0 \
    AND NOT (payload->>'id'=ANY($2::text[])) \
    AND jsonb_typeof(payload->'prepareBundle')='object' \
    AND jsonb_typeof(payload#>'{prepareBundle,request}')='object' \
    AND jsonb_typeof(payload#>'{prepareBundle,version}')='number' \
    AND payload#>>'{prepareBundle,version}'='1' \
    AND jsonb_typeof(payload#>'{prepareBundle,digest}')='string' \
    AND payload#>>'{prepareBundle,digest}' ~ '^[a-f0-9]{64}$' \
    AND jsonb_typeof(payload#>'{prepareBundle,dependencyDigest}')='string' \
    AND payload#>>'{prepareBundle,dependencyDigest}' ~ '^[a-f0-9]{64}$' \
    AND jsonb_typeof(payload#>'{prepareBundle,itemIds}')='array' \
    THEN CASE WHEN payload->>'status'='completed' AND jsonb_typeof(payload->'result')='object' \
        THEN (payload#-'{prepareBundle,request}')-'result' \
        ELSE payload#-'{prepareBundle,request}' END ELSE payload END";

// Same duration-policy inputs as storage_dispatch, while preserving all root
// state here. Do not use its row filter: pointerless legacy assessments need history.
const MEDIA_PROGRESS_FIELDS:&[&str]=&["phase","resumePhase","schemaVersion","sourcePostId",
    "sourceVersion","account","connectorBinding","sourcePostKey","sourceIdentity","source"];
const MEDIA_ROOT_TEXT:&[&str]=&["id","account","refId","createdAt"];
const MEDIA_PROGRESS_TEXT:&[&str]=&["sourcePostId","sourceVersion","account","sourcePostKey"];
const MEDIA_IDENTITY_TEXT:&[&str]=&["account","postKey","mediaSha256"];

fn nonempty_fields(value:&Value,fields:&[&str])->bool {
    fields.iter().all(|key|value[*key].as_str().is_some_and(|v|!v.is_empty()))
}
fn compactable_media_result(job:&Value,protected:Option<&HashSet<String>>)->bool {
    let Some(protected)=protected else{return false;};
    let p=&job["result"]["visualProgress"];
    job.is_object() && job["kind"]=="media" && job["purpose"]=="auto_media"
        && job["status"]=="completed" && job["visualContractVersion"]==2
        && nonempty_fields(job,MEDIA_ROOT_TEXT) && !protected.contains(job["id"].as_str().unwrap())
        && job["connectorBinding"].is_object() && job["result"].is_object() && p.is_object()
        && p["schemaVersion"]==2 && p["phase"]=="complete"
        && nonempty_fields(p,MEDIA_PROGRESS_TEXT) && p["connectorBinding"].is_object()
        && p["sourceIdentity"].is_object() && nonempty_fields(&p["sourceIdentity"],MEDIA_IDENTITY_TEXT)
        && p["sourceIdentity"]["durationMs"].as_u64().is_some_and(|v|v>0)
        && crate::media_fullframes::reference(&p["source"]).is_ok()
}

fn sql_nonempty_fields(input:&str,fields:&[&str])->String {
    fields.iter().map(|key|format!("jsonb_typeof({input}->'{key}')='string' AND length({input}->>'{key}')>0"))
        .collect::<Vec<_>>().join(" AND ")
}
// No numeric casts: malformed values must select the full-payload fallback,
// including JSON-looking strings and integers outside serde_json's u64 range.
fn sql_u64(input:&str)->String {
    format!("jsonb_typeof({input})='number' AND ({input})::text ~ '^(0|[1-9][0-9]*)$' AND \
        (length(({input})::text)<20 OR (length(({input})::text)=20 AND ({input})::text<='18446744073709551615'))")
}
fn job_payload_sql()->String {
    let p="(payload#>'{result,visualProgress}')";
    let identity=format!("({p}->'sourceIdentity')");
    let source=format!("({p}->'source')");
    let fields=MEDIA_PROGRESS_FIELDS.iter().map(|key|format!("'{key}'")).collect::<Vec<_>>().join(",");
    let root_text=sql_nonempty_fields("payload",MEDIA_ROOT_TEXT);
    let progress_text=sql_nonempty_fields(p,MEDIA_PROGRESS_TEXT);
    let identity_text=sql_nonempty_fields(&identity,MEDIA_IDENTITY_TEXT);
    let duration=sql_u64(&format!("({identity}->'durationMs')"));
    let bytes=sql_u64(&format!("({source}->'bytes')"));
    let legacy=source_jobs::payload_sql(&format!("CASE WHEN $3::boolean AND jsonb_typeof(payload)='object' \
        AND payload->>'kind'='media' AND payload->>'purpose'='auto_media' AND payload->>'status'='completed' \
        AND jsonb_typeof(payload->'visualContractVersion')='number' AND payload->>'visualContractVersion'='2' \
        AND {root_text} AND NOT (payload->>'id'=ANY($2::text[])) \
        AND jsonb_typeof(payload->'connectorBinding')='object' AND jsonb_typeof(payload->'result')='object' \
        AND jsonb_typeof({p})='object' AND jsonb_typeof({p}->'schemaVersion')='number' \
        AND {p}->>'schemaVersion'='2' AND {p}->>'phase'='complete' AND {progress_text} \
        AND jsonb_typeof({p}->'connectorBinding')='object' AND jsonb_typeof({identity})='object' \
        AND {identity_text} AND {duration} AND {identity}->>'durationMs'<>'0' \
        AND jsonb_typeof({source})='object' AND {source} ?& ARRAY['sha256','bytes'] \
        AND CASE WHEN jsonb_typeof({source})='object' THEN ({source}-'sha256'-'bytes')='{{}}'::jsonb ELSE false END \
        AND jsonb_typeof({source}->'sha256')='string' AND {source}->>'sha256' ~ '^[a-f0-9]{{64}}$' AND {bytes} \
        THEN jsonb_set(payload,'{{result}}',jsonb_build_object('visualProgress', \
            (SELECT jsonb_object_agg(field.key,field.value) FROM jsonb_each({p}) field WHERE field.key IN ({fields})))) \
        ELSE ({JOB_PAYLOAD}) END"));
    format!("CASE WHEN payload ?| ARRAY[{}] THEN payload ELSE ({legacy}) END",source_jobs::material_fields_sql())
}

fn approval(value: &Value) -> Value {
    match value.as_object() {
        Some(fields) => Value::Object(fields.iter().filter(|(k,_)| APPROVAL_FIELDS.contains(&k.as_str())).map(|(k,v)|{
            let projected=if k=="proposals" { approval_references(v) } else { v.clone() };
            (k.clone(),projected)
        }).collect()),
        None => value.clone(),
    }
}

// Source admission needs exact approval references, not the embedded immutable
// consent snapshots. This view cannot write approvals and is never a dispatch
// or export input. Keep all reference fields/order and malformed values intact.
fn approval_references(value:&Value)->Value {
    let Some(entries)=value.as_array().filter(|entries|entries.iter().all(|entry|
        entry.is_object() && entry["id"].as_str().is_some_and(|id|!id.is_empty()) &&
        ["proposal","item"].iter().all(|key|entry.get(*key).is_none_or(Value::is_object)))) else{return value.clone();};
    Value::Array(entries.iter().map(|entry|Value::Object(entry.as_object().unwrap().iter()
        .filter(|(key,_)|!matches!(key.as_str(),"proposal"|"item"))
        .map(|(key,value)|(key.clone(),value.clone())).collect())).collect())
}

fn approval_payload_sql()->&'static str {
    "CASE WHEN jsonb_typeof(payload)='object' THEN \
        COALESCE((SELECT jsonb_object_agg(e.key,CASE WHEN e.key='proposals' AND jsonb_typeof(e.value)='array' THEN \
            CASE WHEN NOT EXISTS(SELECT 1 FROM jsonb_array_elements(e.value) a(value) WHERE \
                jsonb_typeof(a.value) IS DISTINCT FROM 'object' OR jsonb_typeof(a.value->'id') IS DISTINCT FROM 'string' \
                OR length(a.value->>'id')=0 OR (a.value ? 'proposal' AND jsonb_typeof(a.value->'proposal') IS DISTINCT FROM 'object') \
                OR (a.value ? 'item' AND jsonb_typeof(a.value->'item') IS DISTINCT FROM 'object')) \
            THEN COALESCE((SELECT jsonb_agg(a.value-'proposal'-'item' ORDER BY a.ordinality) \
                FROM jsonb_array_elements(e.value) WITH ORDINALITY a(value,ordinality)),'[]'::jsonb) \
            ELSE e.value END ELSE e.value END) \
        FROM jsonb_each(payload) e WHERE e.key IN ('id','status','proposals')),'{}'::jsonb) ELSE payload END"
}

fn project(workspace: &Value) -> ApiResult<Value> {
    if workspace.get("sourceJobControls").is_some(){return Err(internal("Source controls cannot be canonical metadata"));}
    let mut view = metadata(workspace);
    let protected = protected_jobs(workspace);
    for table in TABLES {
        view[table] = if OMITTED.contains(&table) { json!([]) }
            else if table == "approvals" { Value::Array(rows(workspace,table)?.iter().map(approval).collect()) }
            else if table == "jobs" { Value::Array(rows(workspace,table)?.iter().map(|job| source_job(job, protected.as_ref())).collect()) }
            else { workspace[table].clone() };
    }
    validate(&view)?;
    Ok(view)
}

fn validate_change(before: &Value, after: &Value) -> ApiResult<()> {
    let shape=crate::performance::Span::new("source.snapshot.validation.shape");
    validate(after)?;drop(shape);
    let guards=crate::performance::Span::new("source.snapshot.validation.history_guards");
    crate::db_guards::validate_change(before,after)?;drop(guards);
    let immutable=crate::performance::Span::new("source.snapshot.validation.immutable_versions");
    immutable_versions(before,after)?;drop(immutable);
    let metadata_guard=crate::performance::Span::new("source.snapshot.validation.metadata");
    validate_source_metadata(before,after)?;drop(metadata_guard);
    if after["connectorBinding"]!=before["connectorBinding"] {crate::active_binding(after)?;}
    let readonly=crate::performance::Span::new("source.snapshot.validation.collections");
    for table in TABLES {
        let old=rows(before,table)?;let new=rows(after,table)?;
        if MUTABLE.contains(&table) {
            if new.len()<old.len()||old.iter().zip(new).any(|(a,b)|a["id"]!=b["id"]) {
                return Err(internal("Source admission deleted or reordered records"));
            }
        } else if table!="audit" && old!=new {
            return Err(internal("Source admission changed a readonly collection"));
        }
    }
    drop(readonly);
    Ok(())
}

// Compare metadata by reference. The reusable research archive can be large:
// checking a sync cursor must not deep-clone it into two temporary documents.
// Missing keys remain different from explicit null on immutable surfaces.
fn validate_source_metadata(before:&Value,after:&Value)->ApiResult<()> {
    let old=before.as_object().ok_or_else(||internal("Invalid source metadata"))?;
    let new=after.as_object().ok_or_else(||internal("Invalid source metadata"))?;
    let fixed=|key:&str|!TABLES.contains(&key)&&!["sync","connectorBinding","settings"].contains(&key);
    if !old.iter().filter(|(key,_)|fixed(key)).eq(new.iter().filter(|(key,_)|fixed(key))) {
        return Err(internal("Source admission changed unrelated metadata"));
    }
    // The previous removal path indexed expected["settings"] mutably even
    // when no policy existed. serde_json inserted explicit null for a missing
    // settings key: preserve that exact legacy admission boundary without
    // materializing or copying any metadata on this borrowed path.
    if !old.contains_key("settings") && !new.contains_key("settings") {
        return Err(internal("Source admission changed unrelated metadata"));
    }
    if old.get("settings")==new.get("settings") {return Ok(());}
    const POLICY_FIELDS:&[&str]=&["postMediaPolicies","mediaAudioEquivalences"];
    match (old.get("settings"),new.get("settings")) {
        (None,Some(Value::Null))=>Ok(()),
        (Some(Value::Object(a)),Some(Value::Object(b))) if
            a.iter().filter(|(key,_)|!POLICY_FIELDS.contains(&key.as_str()))
                .eq(b.iter().filter(|(key,_)|!POLICY_FIELDS.contains(&key.as_str())))=>Ok(()),
        // Supplying a policy materialized absent/null settings as an object.
        (None|Some(Value::Null),Some(Value::Object(b))) if !b.is_empty()
            && b.keys().all(|key|POLICY_FIELDS.contains(&key.as_str()))=>Ok(()),
        _=>Err(internal("Source admission changed unrelated metadata")),
    }
}

// Call only after the complete native/readonly/metadata guards. Persist the
// permitted top-level delta in the same locked transaction, retaining every
// other stored key without cloning or serializing its body for the UPDATE.
fn source_metadata_patch(before:&Value,after:&Value)->(Vec<String>,Value) {
    let mut remove=Vec::new();
    let mut set=serde_json::Map::new();
    for field in ["sync","connectorBinding","settings"] {
        if before.get(field)==after.get(field) {continue;}
        match after.get(field) {
            Some(value)=>{set.insert(field.to_owned(),value.clone());},
            None=>remove.push(field.to_owned()),
        }
    }
    (remove,Value::Object(set))
}

// Call only after validate_change proves every readonly collection unchanged.
// Rechecking workspace equality would traverse retained jobs/operation evidence
// a second time. Compare the remaining mutation surfaces without cloning root
// metadata; key presence remains significant (missing is not explicit null).
fn mutable_source_changed(before: &Value, after: &Value) -> bool {
    if MUTABLE.iter().copied().chain(std::iter::once("audit"))
        .any(|table| before.get(table) != after.get(table)) {
        return true;
    }
    let old = before.as_object().expect("validated source workspace");
    let new = after.as_object().expect("validated source workspace");
    let metadata_field = |key: &&String| !TABLES.contains(&key.as_str());
    old.keys().filter(metadata_field).count() != new.keys().filter(metadata_field).count()
        || old.iter().filter(|(key,_)| !TABLES.contains(&key.as_str()))
            .any(|(key,value)| new.get(key) != Some(value))
}

fn apply(workspace:&mut Value,before:&Value,after:&Value)->ApiResult<()> {
    validate_change(before,after)?;
    let existing:HashSet<_>=rows(workspace,"audit")?.iter().map(|v|text(v,"id").map(str::to_owned)).collect::<ApiResult<_>>()?;
    if rows(after,"audit")?.iter().any(|v|v["id"].as_str().is_some_and(|id|existing.contains(id))) {
        return Err(internal("Source admission reused an audit identity"));
    }
    for table in MUTABLE {workspace[*table]=after[*table].clone();}
    for field in ["sync","connectorBinding","settings"] {
        match after.get(field) {Some(value)=>{workspace[field]=value.clone();},None=>{workspace.as_object_mut().unwrap().remove(field);}}
    }
    workspace["audit"].as_array_mut().unwrap().extend(rows(after,"audit")?.iter().cloned());
    Ok(())
}

// Fixed table labels only; never derive diagnostic names from stored values.
fn load_stages(table:&str)->(&'static str,&'static str) {
    match table {
        "posts"=>("source.snapshot.load.posts.fetch","source.snapshot.load.posts.decode"),
        "branches"=>("source.snapshot.load.branches.fetch","source.snapshot.load.branches.decode"),
        "items"=>("source.snapshot.load.items.fetch","source.snapshot.load.items.decode"),
        "proposals"=>("source.snapshot.load.proposals.fetch","source.snapshot.load.proposals.decode"),
        "approvals"=>("source.snapshot.load.approvals.fetch","source.snapshot.load.approvals.decode"),
        "operations"=>("source.snapshot.load.operations.fetch","source.snapshot.load.operations.decode"),
        "materials"=>("source.snapshot.load.materials.fetch","source.snapshot.load.materials.decode"),
        "jobs"=>("source.snapshot.load.jobs.fetch","source.snapshot.load.jobs.decode"),
        "knowledge_entries"=>("source.snapshot.load.knowledge_entries.fetch","source.snapshot.load.knowledge_entries.decode"),
        "knowledge_versions"=>("source.snapshot.load.knowledge_versions.fetch","source.snapshot.load.knowledge_versions.decode"),
        _=>("source.snapshot.load.other.fetch","source.snapshot.load.other.decode"),
    }
}

async fn load(connection:&mut PgConnection)->ApiResult<Value> {
    let metadata_fetch=crate::performance::Span::new("source.snapshot.load.metadata.fetch");
    let record=sqlx::query("SELECT account,metadata::text,execution_enabled FROM communityhero.workspaces WHERE id=$1 FOR UPDATE")
        .bind(WORKSPACE).fetch_one(&mut *connection).await?;
    drop(metadata_fetch);
    let metadata_decode=crate::performance::Span::new("source.snapshot.load.metadata.decode");
    if record.try_get::<bool,_>("execution_enabled")? {return Err(internal("PostgreSQL pilot execution must remain disabled"));}
    let mut value=parse(record.try_get::<&str,_>("metadata")?)?;
    if !value.is_object()||record.try_get::<Option<String>,_>("account")?.as_deref()!=value["account"].as_str(){return Err(internal("Workspace identity mismatch"));}
    if value.get("sourceJobControls").is_some(){return Err(internal("Source controls cannot be canonical metadata"));}
    drop(metadata_decode);
    for table in TABLES {
        if value.get(table).is_some(){return Err(internal("Workspace metadata contains entity collections"));}
        value[table]=json!([]);
        if OMITTED.contains(&table){continue;}
        let payload=if table=="jobs" { job_payload_sql() } else if table=="approvals" {
            approval_payload_sql().to_owned()
        } else {"payload".to_owned()};
        let statement=format!("SELECT id,ordinal,({payload})::text AS payload{} FROM communityhero.{table} WHERE workspace_id=$1 ORDER BY ordinal",
            projection(table).iter().map(|(col,_)|format!(",{col}")).collect::<String>());
        let mut query=sqlx::query(sqlx::AssertSqlSafe(statement.as_str())).bind(WORKSPACE);
        if table=="jobs" {
            // TABLES loads items/proposals before jobs, within the locked writer
            // transaction. Missing or ambiguous references disable compaction.
            let _references=crate::performance::Span::new("source.snapshot.load.jobs.references");
            let protected=protected_jobs(&value);
            let mut ids=protected.as_ref().map(|ids|ids.iter().cloned().collect::<Vec<_>>()).unwrap_or_default();
            ids.sort();
            query=query.bind(ids).bind(protected.is_some());
        }
        let (fetch_stage,decode_stage)=load_stages(table);
        let fetch=crate::performance::Span::new(fetch_stage);
        let records=query.fetch_all(&mut *connection).await?;
        drop(fetch);
        let decode=crate::performance::Span::new(decode_stage);
        let mut values=Vec::with_capacity(records.len());
        for (index,record) in records.into_iter().enumerate() {
            let payload=parse(record.try_get::<&str,_>("payload")?)?;
            if record.try_get::<i32,_>("ordinal")?!=index as i32||record.try_get::<&str,_>("id")?!=text(&payload,"id")? {return Err(internal("Record identity or order mismatch"));}
            for (col,key) in projection(table) {
                if record.try_get::<Option<String>,_>(*col)?.as_deref()!=payload[*key].as_str(){return Err(internal("Record relational projection mismatch"));}
            }
            values.push(payload);
        }
        value[table]=Value::Array(values);
        drop(decode);
    }
    let _validation=crate::performance::Span::new("source.snapshot.load.validate");
    validate(&value)?;
    Ok(value)
}

async fn load_scoped(connection:&mut PgConnection)->ApiResult<(Value,guarded_delta::PhysicalOrdinals)> {
    let mut metadata_fetch=crate::performance::Span::new("source.snapshot.load.metadata.fetch");
    let record=sqlx::query("SELECT account,(metadata-'preparationResearch')::text AS metadata,execution_enabled FROM communityhero.workspaces WHERE id=$1 FOR UPDATE")
        .bind(WORKSPACE).fetch_one(&mut *connection).await?;
    let metadata_text=record.try_get::<&str,_>("metadata")?;
    metadata_fetch.measurements(crate::performance::StorageMeasurements{payload_read:crate::performance::ReadMeasurements{
        rows:Some(1),bytes:Some(metadata_text.len() as u64),statements:Some(1)},..Default::default()});
    drop(metadata_fetch);
    if record.try_get::<bool,_>("execution_enabled")?{return Err(internal("PostgreSQL pilot execution must remain disabled"));}
    let mut view=parse(metadata_text)?;
    if !view.is_object()||record.try_get::<Option<String>,_>("account")?.as_deref()!=view["account"].as_str()
        ||view.get("sourceJobControls").is_some()||TABLES.iter().any(|table|view.get(*table).is_some()){return Err(internal("Scoped source workspace identity mismatch"));}
    let mut ordinals=guarded_delta::PhysicalOrdinals::default();
    for table in TABLES {
        view[table]=json!([]);
        if OMITTED.contains(&table)||table=="jobs"{continue;}
        let payload=if table=="approvals"{approval_payload_sql()}else{"payload"};
        let statement=format!("SELECT id,ordinal,({payload})::text AS payload{} FROM communityhero.{table} WHERE workspace_id=$1 ORDER BY ordinal",
            projection(table).iter().map(|(col,_)|format!(",{col}")).collect::<String>());
        let (fetch_stage,decode_stage)=load_stages(table);
        let mut fetch=crate::performance::Span::new(fetch_stage);
        let records=sqlx::query(sqlx::AssertSqlSafe(statement.as_str())).bind(WORKSPACE).fetch_all(&mut *connection).await?;
        let mut bytes=0u64;
        for record in &records{bytes+=record.try_get::<&str,_>("payload")?.len() as u64;}
        fetch.measurements(crate::performance::StorageMeasurements{payload_read:crate::performance::ReadMeasurements{
            rows:Some(records.len() as u64),bytes:Some(bytes),statements:Some(1)},collections:Some(1),..Default::default()});drop(fetch);
        let decode=crate::performance::Span::new(decode_stage);
        let mut values=Vec::with_capacity(records.len());
        for (index,record) in records.into_iter().enumerate() {
            let value=parse(record.try_get::<&str,_>("payload")?)?;
            let id=text(&value,"id")?;
            let ordinal=record.try_get::<i32,_>("ordinal")?;
            if ordinal!=index as i32||record.try_get::<&str,_>("id")?!=id{return Err(internal("Scoped source record identity or order mismatch"));}
            for (col,key) in projection(table) {
                if record.try_get::<Option<String>,_>(*col)?.as_deref()!=value[*key].as_str(){return Err(internal("Scoped source relational projection mismatch"));}
            }
            if MUTABLE.contains(&table){ordinals.insert(table,id.to_owned(),ordinal)?;}
            values.push(value);
        }
        view[table]=Value::Array(values);drop(decode);
    }
    let control_sql=source_jobs::source_control_sql();
    let statement=format!("SELECT id,ordinal,kind,ref_id,status,({control_sql})::text AS control FROM communityhero.jobs WHERE workspace_id=$1 ORDER BY ordinal");
    let mut fetch=crate::performance::Span::new("source.snapshot.load.jobs.controls.fetch");
    let records=sqlx::query(sqlx::AssertSqlSafe(statement.as_str())).bind(WORKSPACE).bind(Vec::<String>::new()).bind(true)
        .fetch_all(&mut *connection).await?;
    let mut bytes=0u64;
    for record in &records{bytes+=record.try_get::<&str,_>("control")?.len() as u64;}
    fetch.measurements(crate::performance::StorageMeasurements{control_read:crate::performance::ReadMeasurements{
        rows:Some(records.len() as u64),bytes:Some(bytes),statements:Some(1)},..Default::default()});drop(fetch);
    let decode=crate::performance::Span::new("source.snapshot.load.jobs.controls.decode");
    let mut controls=Vec::with_capacity(records.len());
    for (index,record) in records.into_iter().enumerate() {
        let envelope=parse(record.try_get::<&str,_>("control")?)?;
        let control=&envelope["job"];
        if record.try_get::<i32,_>("ordinal")?!=index as i32||record.try_get::<&str,_>("id")?!=text(control,"id")?
            ||envelope["fullRequired"].as_bool().is_none(){return Err(internal("Source maintenance control identity mismatch"));}
        for (col,key) in projection("jobs") {
            if record.try_get::<Option<String>,_>(*col)?.as_deref()!=control[*key].as_str(){return Err(internal("Source maintenance control projection mismatch"));}
        }
        controls.push((control.clone(),envelope["fullRequired"]==true));
    }
    drop(decode);
    let mut closure=dependency_closure::SourceClosure::from_controls(&view,&controls);
    let mut loaded=HashMap::<String,Value>::new();
    let mut missing=HashSet::<String>::new();
    loop {
        closure.expand_controls(&controls);
        let mut ids=closure.full_jobs.iter().filter(|id|!loaded.contains_key(*id)&&!missing.contains(*id)).cloned().collect::<Vec<_>>();
        ids.sort();if ids.is_empty(){break;}
        let mut fetch=crate::performance::Span::new("source.snapshot.load.jobs.bodies.fetch");
        let records=sqlx::query("SELECT id,kind,ref_id,status,payload::text FROM communityhero.jobs WHERE workspace_id=$1 AND (id=ANY($2::text[]) OR payload->>'id'=ANY($2::text[])) ORDER BY ordinal")
            .bind(WORKSPACE).bind(&ids).fetch_all(&mut *connection).await?;
        let mut bytes=0u64;
        for record in &records{bytes+=record.try_get::<&str,_>("payload")?.len() as u64;}
        fetch.measurements(crate::performance::StorageMeasurements{payload_read:crate::performance::ReadMeasurements{
            rows:Some(records.len() as u64),bytes:Some(bytes),statements:Some(1)},
            fallback_reason:closure.full_job_inventory.then_some("source_scope_legacy_fallback"),..Default::default()});drop(fetch);
        let decode=crate::performance::Span::new("source.snapshot.load.jobs.bodies.decode");
        for record in records {
            let job=parse(record.try_get::<&str,_>("payload")?)?;
            let id=text(&job,"id")?;
            if record.try_get::<&str,_>("id")?!=id{return Err(internal("Source full-job identity mismatch"));}
            for (col,key) in projection("jobs") {
                if record.try_get::<Option<String>,_>(*col)?.as_deref()!=job[*key].as_str(){return Err(internal("Source full-job projection mismatch"));}
            }
            closure.expand_jobs(&job);loaded.insert(id.to_owned(),job);
        }
        // Missing historical refs retain the existing domain missing-job error;
        // mark only the read attempt, never invent a job or loop forever.
        for id in ids {if !loaded.contains_key(&id){missing.insert(id);}}
        drop(decode);
    }
    view["jobs"]=Value::Array(controls.iter().filter_map(|(control,_)|control["id"].as_str().and_then(|id|loaded.remove(id))).collect());
    closure.collect_archives(&view["jobs"]);closure.collect_archives(&view["proposals"]);
    let mut archive_ids=closure.archive_ids.iter().cloned().collect::<Vec<_>>();archive_ids.sort();
    let mut fetch=crate::performance::Span::new("source.snapshot.load.research.fetch");
    let research:Option<String>=sqlx::query_scalar("SELECT CASE WHEN NOT(metadata ? 'preparationResearch') THEN NULL ELSE \
        CASE WHEN jsonb_typeof(metadata->'preparationResearch')='array' AND NOT $3::boolean THEN \
          COALESCE((SELECT jsonb_agg(a.value ORDER BY a.ordinality) FROM jsonb_array_elements(metadata->'preparationResearch') WITH ORDINALITY a(value,ordinality) \
            WHERE a.value->>'id'=ANY($2::text[])),'[]'::jsonb) ELSE metadata->'preparationResearch' END::text END \
        FROM communityhero.workspaces WHERE id=$1")
        .bind(WORKSPACE).bind(archive_ids).bind(closure.full_research).fetch_one(&mut *connection).await?;
    fetch.measurements(crate::performance::StorageMeasurements{payload_read:crate::performance::ReadMeasurements{
        rows:Some(1),bytes:Some(research.as_ref().map_or(0,|v|v.len()) as u64),statements:Some(1)},
        fallback_reason:closure.full_research.then_some("source_scope_ambiguous_research"),..Default::default()});drop(fetch);
    let decode=crate::performance::Span::new("source.snapshot.load.research.decode");
    if let Some(research)=research{view["preparationResearch"]=parse(&research)?;}
    drop(decode);
    view["sourceJobControls"]=json!({"version":1,"complete":true,"jobs":controls.into_iter().map(|(value,_)|value).collect::<Vec<_>>()});
    validate(&view)?;validate_source_controls(&view)?;Ok((view,ordinals))
}

impl Database {
    pub(crate) async fn change_source_snapshot_scoped_observed<T>(&self,intent:SourceReadIntent<'_>,f:impl FnOnce(&mut Value)->ApiResult<T>)->ApiResult<(T,bool)> {
        let SourceWriteCompletion{outcome,cleanup}=self.change_source_snapshot_scoped_completed(intent,f).await;
        cleanup.dispose().await;outcome
    }

    pub(crate) async fn change_source_snapshot_scoped_completed<T>(&self,intent:SourceReadIntent<'_>,f:impl FnOnce(&mut Value)->ApiResult<T>)->SourceWriteCompletion<T> {
        let mut cleanup=SourceProjectionCleanup::default();
        match intent {
            SourceReadIntent::Full{reason}=>{
                let mut fallback=crate::performance::Span::new("source.snapshot.load");
                fallback.measurements(crate::performance::StorageMeasurements{fallback_reason:Some(reason),..Default::default()});
                return SourceWriteCompletion{outcome:self.change_source_snapshot_observed(f).await,cleanup};
            },
            SourceReadIntent::Snapshot(snapshot) if !snapshot.is_object()=>{
                let mut fallback=crate::performance::Span::new("source.snapshot.load");
                fallback.measurements(crate::performance::StorageMeasurements{fallback_reason:Some("source_scope_legacy_fallback"),..Default::default()});
                return SourceWriteCompletion{outcome:self.change_source_snapshot_observed(f).await,cleanup};
            },
            SourceReadIntent::Snapshots(snapshots) if snapshots.is_empty()||snapshots.len()>2||snapshots.iter().any(|snapshot|!snapshot.is_object())=>
                return SourceWriteCompletion{outcome:Err(bad("Invalid ordered source snapshot batch")),cleanup},
            SourceReadIntent::Snapshots(_)=>(),
            SourceReadIntent::Snapshot(_)=>(),
        }
        let outcome=match self {
            Self::Sqlite(_)=>self.change_observed(|workspace|{
                validate(workspace)?;
                let before=project_scoped(workspace)?;let mut after=before.clone();let result=f(&mut after)?;
                validate_scoped_change(&before,&after)?;
                if mutable_source_changed(&before,&after) {
                    let existing:HashSet<_>=rows(workspace,"audit")?.iter().map(|row|text(row,"id").map(str::to_owned)).collect::<ApiResult<_>>()?;
                    if rows(&after,"audit")?.iter().any(|row|row["id"].as_str().is_some_and(|id|existing.contains(id))){return Err(internal("Source reused audit identity"));}
                    for table in MUTABLE {workspace[*table]=after[*table].clone();}
                    for field in ["sync","connectorBinding","settings"] {
                        match after.get(field){Some(value)=>workspace[field]=value.clone(),None=>{workspace.as_object_mut().unwrap().remove(field);}}
                    }
                    workspace["audit"].as_array_mut().unwrap().extend(rows(&after,"audit")?.iter().cloned());
                }
                Ok(result)
            }).await,
            Self::Postgres{writer,..}=>async {
                let _total=crate::performance::Span::new("source.snapshot.transaction");
                let acquire=crate::performance::Span::new("source.snapshot.writer.acquire");
                let mut connection=writer.acquire().await?;drop(acquire);
                let settled=async {
                let begin=crate::performance::Span::new("source.snapshot.begin");
                let mut tx=sqlx::Connection::begin(&mut *connection).await?;drop(begin);
                let loading=crate::performance::Span::new("source.snapshot.load");
                let loaded=load_scoped(&mut tx).await;drop(loading);
                // Retain projections until after explicit rollback/commit ACK and
                // awaited pool return, including rejected reducer paths.
                let outcome:ApiResult<(T,bool)>=async {
                    let (value,ordinals)=loaded?;cleanup.before=Some(value);
                    let cloning=crate::performance::Span::new("source.snapshot.clone");
                    cleanup.after=cleanup.before.clone();drop(cloning);
                    let reducing=crate::performance::Span::new("source.snapshot.domain");
                    let result=f(cleanup.after.as_mut().unwrap())?;drop(reducing);
                    let (before,after)=(cleanup.before.as_ref().unwrap(),cleanup.after.as_ref().unwrap());
                    let validation=crate::performance::Span::new("source.snapshot.validation");
                    validate_scoped_change(before,after)?;drop(validation);
                    let changed=mutable_source_changed(before,after);
                    if changed {
                        let persist=crate::performance::Span::new("source.snapshot.persist.entities");
                        guarded_delta::persist(&mut tx,before,after,&ordinals).await?;drop(persist);
                        let (remove,set)=source_metadata_patch(before,after);
                        if !remove.is_empty()||set.as_object().is_some_and(|fields|!fields.is_empty()) {
                            let result=sqlx::query("UPDATE communityhero.workspaces SET metadata=(metadata-$2::text[])||$3::jsonb WHERE id=$1")
                                .bind(WORKSPACE).bind(remove).bind(set.to_string()).execute(&mut *tx).await?;
                            if result.rows_affected()!=1{return Err(internal("Scoped source workspace disappeared"));}
                        }
                    }
                    Ok((result,changed))
                }.await;
                let commit=crate::performance::Span::new("source.snapshot.commit");
                let (outcome,completion)=super::pg_writer::settle(tx,outcome).await;drop(commit);
                Ok::<_,ApiError>((outcome,completion))
                }.await;
                match settled {
                    Ok((outcome,completion))=>{
                        super::pg_writer::release(&mut connection,writer,completion).await;outcome
                    },
                    Err(error)=>{
                        // Failed BEGIN still returns this acquired connection
                        // before App can receive its envelope. No rollback ACK
                        // or safe retry is inferred from a BEGIN error.
                        let mut returning=crate::performance::Span::new("pg.writer.return");
                        connection.return_to_pool().await;
                        returning.writer_pool_state(false,false,writer.num_idle(),writer.size());
                        Err(error)
                    },
                }
            }.await,
        };
        SourceWriteCompletion{outcome,cleanup}
    }

    pub(crate) async fn change_source_snapshot_observed<T>(&self,f:impl FnOnce(&mut Value)->ApiResult<T>)->ApiResult<(T,bool)> {
        match self {
            Self::Sqlite(_)=>self.change_observed(|workspace|{
                let before=project(workspace)?;let mut after=before.clone();let result=f(&mut after)?;
                validate_change(&before,&after)?;
                if mutable_source_changed(&before,&after) {apply(workspace,&before,&after)?;}
                Ok(result)
            }).await,
            Self::Postgres{writer,..}=>{
                let _total=crate::performance::Span::new("source.snapshot.transaction");
                let begin=crate::performance::Span::new("source.snapshot.begin");
                let mut tx=writer.begin().await?;
                drop(begin);
                let loading=crate::performance::Span::new("source.snapshot.load");
                let before=load(&mut tx).await?;drop(loading);
                let domain=crate::performance::Span::new("source.snapshot.clone_and_domain");
                let cloning=crate::performance::Span::new("source.snapshot.clone");
                let mut after=before.clone();drop(cloning);
                let reducing=crate::performance::Span::new("source.snapshot.domain");
                let result=f(&mut after)?;drop(reducing);drop(domain);
                let validation=crate::performance::Span::new("source.snapshot.validation");
                validate_change(&before,&after)?;drop(validation);
                let comparison=crate::performance::Span::new("source.snapshot.change_detection");
                let changed=mutable_source_changed(&before,&after);drop(comparison);
                if !changed {
                    let commit=crate::performance::Span::new("source.snapshot.commit");
                    tx.commit().await?;drop(commit);return Ok((result,false));
                }
                let entities=crate::performance::Span::new("source.snapshot.persist.entities");
                delta::persist(&mut tx,&before,&after).await?;
                drop(entities);
                let delta=crate::performance::Span::new("source.snapshot.metadata.delta");
                let (remove,set)=source_metadata_patch(&before,&after);
                drop(delta);
                if !remove.is_empty() || set.as_object().is_some_and(|fields|!fields.is_empty()) {
                    let persist=crate::performance::Span::new("source.snapshot.persist.metadata");
                    let result=sqlx::query("UPDATE communityhero.workspaces SET metadata=(metadata-$2::text[])||$3::jsonb WHERE id=$1")
                        .bind(WORKSPACE).bind(remove).bind(set.to_string()).execute(&mut *tx).await?;
                    if result.rows_affected()!=1 {return Err(internal("Source admission workspace disappeared"));}
                    drop(persist);
                }
                let commit=crate::performance::Span::new("source.snapshot.commit");
                tx.commit().await?;drop(commit);Ok((result,true))
            }
        }
    }
}

#[cfg(test)]
#[path="storage_source_snapshot_tests.rs"]
mod tests;

#[cfg(test)]
#[path="storage_source_scoped_tests.rs"]
mod scoped_tests;

#[cfg(test)]
#[path = "storage_source_benchmark_tests.rs"]
mod benchmark_tests;

#[cfg(test)]
#[path="storage_source_completion_app_tests.rs"]
mod completion_app_tests;
