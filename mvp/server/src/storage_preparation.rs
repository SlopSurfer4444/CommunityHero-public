//! Transactional projections for preparation claims and explicit scheduling.
//! Queue discovery sees complete source/authority collections. Job-bound stages
//! retain recipients and their author history, plus the complete shared media
//! and knowledge corpus; unrelated discussion and private history stay outside.
//! Explicit
//! scheduling retains all media and audio jobs and appends exactly one new job.
//! SQLite merges the narrow delta into its full document under the writer transaction.
use super::*;
use serde_json::json;
#[path = "storage_preparation_admission.rs"]
mod admission;
#[path = "storage_preparation_final.rs"]
mod final_settlement;
#[path = "storage_proposal.rs"]
mod proposal;
#[path = "storage_preparation_plan.rs"]
mod plan;

#[cfg(test)]
pub(crate) async fn writer_v51_fixture_db() -> Database {
    let url = std::env::var("COMMUNITYHERO_WRITER_V51_TEST_URL").expect("explicit isolated fixture URL");
    let expected = std::env::var("COMMUNITYHERO_WRITER_V51_TEST_DATABASE").expect("explicit isolated fixture name");
    writer_v51_fixture_db_with(&url,&expected).await
}

// Same pristine/local/test identity gate, with explicit arguments so paired
// ignored tests never mutate process-global fixture environment variables.
#[cfg(test)]
pub(crate) async fn writer_v51_fixture_db_with(url:&str,expected:&str) -> Database {
    writer_v51_fixture_db_for_profile_with(url,expected,crate::accounts::Profile::LikeAvto).await
}

#[cfg(test)]
pub(crate) async fn writer_v51_fixture_db_for_profile_with(url:&str,expected:&str,profile:crate::accounts::Profile) -> Database {
    assert!(expected.starts_with("communityhero_writer_v51_test_"), "refusing non-test database");
    let options = url.parse::<sqlx::postgres::PgConnectOptions>()
        .unwrap_or_else(|_| panic!("invalid fixture connection options"));
    assert_eq!(options.get_host(), "127.0.0.1", "fixture must be local");
    assert_eq!(options.get_database(), Some(expected), "fixture URL identity mismatch");
    let guard = PgPoolOptions::new().max_connections(1).connect_with(options).await
        .unwrap_or_else(|_| panic!("fixture guard connection failed"));
    let actual: String = sqlx::query_scalar("SELECT current_database()").fetch_one(&guard).await
        .unwrap_or_else(|_| panic!("fixture identity query failed"));
    assert!(actual == expected, "fixture identity mismatch");
    let exists: bool = sqlx::query_scalar("SELECT to_regnamespace('communityhero') IS NOT NULL")
        .fetch_one(&guard).await.unwrap();
    assert!(!exists, "fresh database without a communityhero schema required");
    let mut tx = guard.begin().await.unwrap();
    sqlx::raw_sql(include_str!("../migrations/0001_pilot.sql")).execute(&mut *tx).await.unwrap();
    sqlx::query("CREATE TABLE communityhero.schema_migrations(version integer PRIMARY KEY, applied_at timestamptz NOT NULL DEFAULT now())")
        .execute(&mut *tx).await.unwrap();
    sqlx::raw_sql(include_str!("../migrations/0002_knowledge.sql")).execute(&mut *tx).await.unwrap();
    sqlx::raw_sql(include_str!("../migrations/0003_history_guards.sql")).execute(&mut *tx).await.unwrap();
    let mut initial = crate::empty();
    normalize(&mut initial);
    // New BAW tests create a pristine BAW workspace directly. Existing test
    // fixtures retain their previous unbound LikeAvto placeholder semantics.
    if profile==crate::accounts::Profile::BawRussia{crate::accounts::initialize(&mut initial,profile).unwrap();}
    sqlx::query("INSERT INTO communityhero.migration_imports(id,source_sha256,payload) VALUES('writer-v51-fixture',$1,$2::jsonb)")
        .bind("0".repeat(64)).bind(initial.to_string()).execute(&mut *tx).await.unwrap();
    sqlx::query("INSERT INTO communityhero.workspaces(id,account,import_id,metadata) VALUES($1,$2,'writer-v51-fixture',$3::jsonb)")
        .bind(WORKSPACE).bind(profile.display()).bind(metadata(&initial).to_string()).execute(&mut *tx).await.unwrap();
    tx.commit().await.unwrap();
    guard.close().await;
    let db = Database::postgres(&url).await.unwrap_or_else(|_| panic!("fixture storage startup failed"));
    let initial = db.read().await.unwrap();
    assert_eq!(initial["account"], profile.display());
    assert!(TABLES.iter().all(|table| crate::list(&initial,table).is_empty()), "fresh empty fixture required for each test");
    db
}

const SOURCE: &[&str] = &["posts", "branches", "operations", "materials", "knowledge_entries", "knowledge_versions", "preparationResearch"];
const MUTABLE: &[&str] = &["items", "proposals", "jobs"];
const OMITTED: &[&str] = &["conversations", "audit", "approvals", "feedback"];
const ITEM_MUTABLE: &[&str] = &["autoPreparation", "autoRevalidation", "preparationMediaWait", "workflow", "decision", "reason", "revision"];
const PROPOSAL_MUTABLE: &[&str] = &["status", "staleReason", "staleAt", "revision"];
const MEDIA_JOB_MUTABLE: &[&str] = &["status", "finishedAt", "result", "error"];

// These are FULL canonical rows, including settled children. A selected parent
// cannot spend/reset a repair budget by forgetting an old child/paid receipt.
const MATERIAL_JOB_FIELDS:&[&str]=&["mandatoryMaterialContract","postContextBundle","materialReadiness",
    "materialArtifacts","modelMaterialReceipt","modelMaterialReceipts","answeringRepairPlan",
    "originatingAnsweringAttemptId","requestingPaidAttemptId","roundOrdinal","repairPaidIntent",
    "frameNeed","framePlan","frameResult","frameLease","videoFrameNeeds","frameNeedOutcome",
    "sourceOriginJobId","nativeSourceOriginJobId","sourceProofRef","manualFrameRequest","retainedSource","videoSpeechAssetPin","repairMergeReceipt","pendingWorkResume","sourceAssetPin","parentManualFrameRequestId"];
pub(super) fn retained_material_job(job:&Value)->bool {
    MATERIAL_JOB_FIELDS.iter().any(|field|job.get(*field).is_some())
        ||["materialAcquisition","answeringRepairs","repairBudget"].iter().any(|field|job["preparationStages"].get(*field).is_some())
}
pub(super) fn retained_material_job_sql()->String {
    format!("(payload ?| ARRAY[{}] OR (payload->'preparationStages') ?| ARRAY['materialAcquisition','answeringRepairs','repairBudget'])",
        MATERIAL_JOB_FIELDS.iter().map(|field|format!("'{field}'")).collect::<Vec<_>>().join(","))
}

fn retain_job(job: &Value, references: &HashSet<String>) -> bool {
    retained_material_job(job)||matches!(job["kind"].as_str(),Some("media"|"media_analysis"|"media_analysis_applicability"))
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
        for field in [&proposal["prepareRunId"], &proposal["origin"]["prepareRunId"], &proposal["recovery"]["prepareRunId"]] {
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
    projection_for(workspace,None)
}
fn projection_for(workspace:&Value,first_job:Option<&str>)->ApiResult<Value> {
    projection_with_jobs(workspace,first_job,false)
}
fn schedule_projection(workspace:&Value)->ApiResult<Value> {
    projection_with_jobs(workspace,None,true)
}
fn prepare_receipt(row:&Value)->bool {
    row["action"]=="local_admission.committed" && row["kind"]=="prepare"
}

// Only a well-formed captured recipient set permits narrowing. Legacy or
// malformed jobs retain the previous projection so domain guards, not missing
// storage context, determine their disposition. Current author fields decide
// cross-post history: an author correction must not reuse the saved author's
// evidence. Shared posts/knowledge remain complete for media conflict checks.
fn preparation_recipients(job:&Value)->Option<Vec<String>> {
    recipient_ids(&job["prepareBundle"]["itemIds"])
}
fn recipient_ids(value:&Value)->Option<Vec<String>> {
    let ids=value.as_array().filter(|ids|!ids.is_empty()&&ids.len()<=100)?;
    let mut seen=HashSet::new();
    ids.iter().map(|id| {
        let id=id.as_str().filter(|id|!id.is_empty())?;
        seen.insert(id).then(||id.to_owned())
    }).collect()
}
fn recipient_scope(workspace:&Value,job_id:Option<&str>)->Option<Vec<String>> {
    let job_id=job_id.filter(|id|!id.is_empty())?;
    let recipients=preparation_recipients(crate::list(workspace,"jobs").iter().find(|job|job["id"]==job_id)?)?;
    recipient_union(workspace,recipients,crate::list(workspace,"jobs"))
}
// Fact workers remain in job-bound preparation contexts. Their currentness
// proof consumes their own current recipients and branches, so narrowing only
// to the ordinary job falsely turns exact disjoint work into an exclusive hold.
// This discriminator grants no independence: the worker's domain helper still
// validates the complete marker, paid parent/attempt and current source. Invalid
// marker/recipient shapes select the existing broad source fallback.
fn recipient_union(workspace:&Value,mut recipients:Vec<String>,jobs:&[Value])->Option<Vec<String>> {
    let mut seen:HashSet<String>=recipients.iter().cloned().collect();
    for job in jobs.iter().filter(|job| job["kind"]=="assistant" && job["purpose"]=="public_fact_followup"
        && matches!(job["status"].as_str(),Some("queued"|"running")) && job.get("factWorkerScope").is_some()) {
        let scope=&job["factWorkerScope"];
        let requested=recipient_ids(&job["requestedItemIds"])?;
        let captured=recipient_ids(&scope["itemIds"])?;
        let requested_set:HashSet<_>=requested.iter().cloned().collect();
        if !scope.is_object() || scope["version"]!=1 || scope["account"]!=workspace["account"]
            || scope["connectorBinding"]!=crate::active_binding(workspace).ok()?.to_json()
            || !job["id"].as_str().is_some_and(|id|!id.is_empty()) || scope["jobId"]!=job["id"]
            || !job["parentPrepareJobId"].as_str().is_some_and(|id|!id.is_empty())
            || scope["parentJobId"]!=job["parentPrepareJobId"]
            || captured.into_iter().collect::<HashSet<_>>()!=requested_set {return None;}
        for id in requested {
            if seen.insert(id.clone()){recipients.push(id);}
        }
    }
    Some(recipients)
}
fn scoped_item_ids(workspace:&Value,recipients:&[String])->HashSet<String> {
    let authors:HashSet<_>=crate::list(workspace,"items").iter()
        .filter(|item|item["id"].as_str().is_some_and(|id|recipients.iter().any(|r|r==id)))
        .filter_map(|item|Some((item["authorId"].as_str().filter(|s|!s.is_empty())?,
            item["platform"].as_str().filter(|s|!s.is_empty())?))).collect();
    crate::list(workspace,"items").iter().filter(|item|
        item["id"].as_str().is_some_and(|id|recipients.iter().any(|r|r==id))
        ||(["account","accountId"].iter().all(|field|item.get(*field).is_none_or(|value|value==&workspace["account"]))
            &&item["authorId"].as_str().zip(item["platform"].as_str()).is_some_and(|pair|authors.contains(&pair))))
        .filter_map(|item|item["id"].as_str().map(str::to_owned)).collect()
}
fn projection_with_jobs(workspace:&Value,first_job:Option<&str>,schedule:bool)->ApiResult<Value> {
    let mut view = claim_metadata(workspace)?;
    let conductor=crate::conductor_authority::current_context().map(|ctx|ctx.run_id);
    let refs = (first_job.is_none()&&!schedule).then(||references(workspace));
    let recipients=(!schedule).then(||recipient_scope(workspace,first_job)).flatten();
    let item_ids=recipients.as_ref().map(|ids|scoped_item_ids(workspace,ids));
    let branch_ids:HashSet<_>=crate::list(workspace,"items").iter()
        .filter(|item|item_ids.as_ref().is_some_and(|ids|item["id"].as_str().is_some_and(|id|ids.contains(id))))
        .filter_map(|item|item["branchId"].as_str()).collect();
    for table in TABLES {
        match table {
            "items" if recipients.is_some()=>view[table]=json!(rows(workspace,table)?.iter()
                .filter(|item|item["id"].as_str().is_some_and(|id|item_ids.as_ref().unwrap().contains(id))).collect::<Vec<_>>()),
            "branches" if recipients.is_some()=>view[table]=json!(rows(workspace,table)?.iter()
                .filter(|branch|branch["id"].as_str().is_some_and(|id|branch_ids.contains(id))).collect::<Vec<_>>()),
            "proposals" if recipients.is_some()=>view[table]=json!(rows(workspace,table)?.iter()
                .filter(|row|row["itemId"].as_str().is_some_and(|id|recipients.as_ref().unwrap().iter().any(|r|r==id))).collect::<Vec<_>>()),
            "jobs" => view[table] = json!(rows(workspace, table)?.iter().filter(|j| {
                if retained_material_job(j){return true;}
                if conductor.as_deref().is_some_and(|run|j["id"]==run){return true;}
                if schedule {matches!(j["kind"].as_str(),Some("media"|"media_audio"|"media_analysis"|"media_analysis_applicability"))||j["factFollowups"].as_array().is_some_and(|v|!v.is_empty())
                    ||(j["kind"]=="assistant"&&matches!(j["status"].as_str(),Some("running"|"queued")))}
                else if let Some(job_id)=first_job {matches!(j["kind"].as_str(),Some("media"|"media_audio"|"media_analysis"|"media_analysis_applicability"))||j["id"]==job_id
                    || j["factFollowups"].as_array().is_some_and(|v|!v.is_empty())
                    || (j["kind"]=="assistant"&&matches!(j["status"].as_str(),Some("running"|"queued")))}
                else {retain_job(j,refs.as_ref().expect("claim references"))}
            }).collect::<Vec<_>>()),
            "audit" if schedule => view[table]=json!(rows(workspace,table)?.iter()
                .filter(|row|prepare_receipt(row)).collect::<Vec<_>>()),
            name if OMITTED.contains(&name) => (),
            _ => view[table] = workspace[table].clone(),
        }
    }
    // Pure projections receive the complete workspace. PostgreSQL metadata
    // has no entity arrays; its controls are loaded separately after entities.
    super::scope_context::project(workspace,&mut view)?;
    Ok(view)
}

// Explicit prepare scheduling may append one job, and may not edit source,
// operator decisions, media state, or another job while the writer lock is held.
fn validate_schedule_change(before:&Value,after:&Value)->ApiResult<()> {
    crate::preparation_reservations::validate_change(before,after)?;
    crate::retained_paid_recovery_registry::validate_change(before,after,false)?;
    let (Some(old),Some(new))=(before.as_object(),after.as_object()) else {
        return Err(internal("Invalid preparation scheduling workspace"));
    };
    if old.len()!=new.len() || old.iter().any(|(key,value)|!matches!(key.as_str(),"jobs"|"audit")&&new.get(key)!=Some(value)) {
        return Err(internal("Preparation scheduling changed source or history"));
    }
    let prior=rows(before,"jobs")?;
    let next=rows(after,"jobs")?;
    let prior_audit=rows(before,"audit")?;
    let next_audit=rows(after,"audit")?;
    if before==after {return Ok(());}
    if next.len()!=prior.len()+1 {
        return Err(internal("Preparation scheduling changed existing jobs"));
    }
    let job=next.last().unwrap();
    crate::fact_followup::validate_automatic_schedule(before,after,job)?;
    let manifest=job["prepareBundle"].get("factFollowupManifest").cloned().unwrap_or(json!([]));
    let mut expected=before.clone();
    crate::fact_followup::current(before,&manifest,job["selectedItemIds"].as_array().ok_or_else(||internal("Missing fact recipients"))?,&crate::now()).map_err(internal)?;
    crate::fact_followup::consume(&mut expected,crate::required(job,"id")?,&manifest)?;
    if !rows(&expected,"jobs")?.iter().zip(next).all(|(a,b)|a==b){return Err(internal("Preparation scheduling changed protected fact evidence"));}
    if !next_audit.starts_with(prior_audit) || next_audit.len()>prior_audit.len()+1 {
        return Err(internal("Preparation scheduling changed receipt history"));
    }
    if let Some(receipt)=next_audit.get(prior_audit.len()) {
        super::local_admission::validate_prepare_receipt(receipt,job,after)?;
    }
    let fields=job.as_object().ok_or_else(||internal("Invalid engine preparation job"))?;
    let mut stages=job["preparationStages"].clone();
    let captured=stages.as_object_mut().and_then(|stages|stages.remove("groupAdmission"));
    if after.get("runtimeLifecycle").is_some() {
        validate_lifecycle_admission(after,job,"initialAdmission","scheduled","admittedAt")?;
        stages.as_object_mut().unwrap().remove("initialAdmission");
    }
    if stages!=json!({"first":null,"review":null}) {
        return Err(internal("Preparation scheduling appended invalid stages"));
    }
    if let Some(captured)=captured {
        let expected=if let Some(bundle)=job.get("prepareBundle") {
            crate::prepare_bundle::capture_groups(after,bundle).map_err(internal)?
        }else{json!([])};
        if captured!=expected {return Err(internal("Preparation scheduling group bindings changed"));}
    }
    let attribution=["conductorRunId","grantGeneration"];
    if let Some(ctx)=crate::conductor_authority::current_context(){
        if job["conductorRunId"]!=ctx.run_id||job["grantGeneration"].as_u64()!=Some(ctx.lease_generation){
            return Err(internal("Preparation scheduling changed conductor attribution"));
        }
        crate::conductor_authority::fence_admission(after,"prepare",
            job["selectedItemIds"].as_array().ok_or_else(||internal("Preparation recipients missing"))?)?;
    }else if attribution.iter().any(|field|job.get(*field).is_some()){
        return Err(internal("Preparation scheduling requires its conductor context"));
    }
    let allowed=["id","kind","refId","status","createdAt","purpose","requestedItemIds",
        "selectedItemIds","held","preparationStages","prepareBundle","scopeReservation","preparationWorkerScope","conductorRunId","grantGeneration","automaticFactContinuation"];
    if fields.keys().any(|key|!allowed.contains(&key.as_str()))
        || job["id"].as_str().is_none_or(str::is_empty)
        || prior.iter().any(|existing|existing["id"]==job["id"])
        || job["kind"]!="assistant" || job["refId"]!="engine_prepare"
        || job["purpose"]!="engine_prepare" || job["status"]!="running"
        || job["createdAt"].as_str().is_none_or(str::is_empty)
        || job["requestedItemIds"].as_array().is_none_or(Vec::is_empty)
        || !job["selectedItemIds"].is_array() || !job["held"].is_array()
        || (job["selectedItemIds"].as_array().unwrap().is_empty()!=job.get("prepareBundle").is_none())
        || job.get("prepareBundle").is_some_and(|bundle|bundle["itemIds"]!=job["selectedItemIds"]) {
        return Err(internal("Preparation scheduling appended an invalid job"));
    }
    if job.get("preparationWorkerScope").is_some_and(|saved|crate::preparation_workers::capture(after,job).as_ref()!=Some(saved)) {
        return Err(internal("Preparation scheduling family worker scope changed"));
    }
    crate::db_guards::validate_change(before,after)?;
    Ok(())
}
fn merge_schedule_delta(workspace:&mut Value,before:&Value,after:&Value)->ApiResult<()> {
    if before==after {return Ok(());}
    let job=rows(after,"jobs")?.last().ok_or_else(||internal("Preparation scheduling job missing"))?;
    let existing=rows(workspace,"jobs")?;
    if existing.iter().any(|row|row["id"]==job["id"]) {
        return Err(internal("Preparation scheduling reused a job identity"));
    }
    for (old,new) in rows(before,"jobs")?.iter().zip(rows(after,"jobs")?) {
        if old!=new {*crate::row_mut(workspace,"jobs",crate::required(new,"id")?)?=new.clone();}
    }
    workspace["jobs"].as_array_mut().ok_or_else(||internal("Workspace jobs missing"))?.push(job.clone());
    for receipt in &rows(after,"audit")?[rows(before,"audit")?.len()..] {
        if rows(workspace,"audit")?.iter().any(|row|row["id"]==receipt["id"]) {
            return Err(internal("Preparation scheduling reused a receipt identity"));
        }
        workspace["audit"].as_array_mut().ok_or_else(||internal("Workspace audit missing"))?.push(receipt.clone());
    }
    Ok(())
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
    crate::preparation_reservations::validate_change(before,after)?;
    crate::retained_paid_recovery_registry::validate_change(before,after,false)?;
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
        crate::fact_followup::validate_automatic_policy(after,new)?;
        if !((new["kind"] == "media" && new["purpose"] == "auto_media")
            || (new["kind"] == "assistant" && matches!(new["purpose"].as_str(), Some("auto_prepare" | "auto_revalidate"))
                && new["status"] == "running")) {
            return Err(internal("Preparation claim appended an unrelated job"));
        }
        if new["kind"]=="assistant" {
            let stages=new["preparationStages"].as_object().ok_or_else(||internal("New preparation job stages missing"))?;
            if !new["preparationStages"]["first"].is_null()||!new["preparationStages"]["review"].is_null()
                ||stages.keys().any(|key|!matches!(key.as_str(),"first"|"review"|"groupAdmission"|"initialAdmission")) {
                return Err(internal("New preparation job contains prepopulated paid stages"));
            }
        }
        if new["kind"]=="assistant" && new["preparationStages"].get("groupAdmission").is_some() {
            let expected=crate::prepare_bundle::capture_groups(after,&new["prepareBundle"]).map_err(internal)?;
            if new["preparationStages"]["groupAdmission"]!=expected
                || new["preparationStages"].as_object().is_none_or(|stages|stages.iter().any(|(key,value)|
                    !matches!(key.as_str(),"groupAdmission"|"initialAdmission")&&(!matches!(key.as_str(),"first"|"review")||!value.is_null()))) {
                return Err(internal("Preparation claim group bindings changed"));
            }
        }
        if new["kind"]=="assistant" && after.get("runtimeLifecycle").is_some() {
            validate_lifecycle_admission(after,new,"initialAdmission","scheduled","admittedAt")?;
        }
        if new.get("preparationWorkerScope").is_some_and(|saved|crate::preparation_workers::capture(after,new).as_ref()!=Some(saved)) {
            return Err(internal("Preparation claim family worker scope changed"));
        }
    }
    crate::db_guards::validate_change(before, after)?;
    Ok(())
}

fn validate_lifecycle_admission(workspace:&Value,job:&Value,field:&str,status:&str,clock:&str)->ApiResult<()> {
    let receipt=&job["preparationStages"][field];
    if receipt.as_object().is_none_or(|o|o.len()!=5||!["version","status","requestSha256","owner",clock].iter().all(|k|o.contains_key(*k)))
        ||receipt["version"]!=1||receipt["status"]!=status||!receipt[clock].as_str().is_some_and(|v|!v.is_empty()) {
        return Err(internal("Preparation lifecycle reservation schema mismatch"));
    }
    let owner=crate::runtime_lifecycle::parse_token(&receipt["owner"])?;
    crate::runtime_lifecycle::require_admission(workspace,&owner,crate::runtime_lifecycle::AdmissionClass::Preparation)?;
    let request=&job["prepareBundle"]["request"];
    if request.is_null() {
        if field!="initialAdmission"||!receipt["requestSha256"].is_null(){return Err(internal("Preparation lifecycle request missing"));}
    }else{
        use sha2::{Digest,Sha256};let digest=format!("{:x}",Sha256::digest(request.to_string().as_bytes()));
        if receipt["requestSha256"]!=digest||job["prepareBundle"]["digest"]!=digest{return Err(internal("Preparation lifecycle request differs"));}
    }
    Ok(())
}
fn without_stage(job: &Value, stage: &str) -> ApiResult<Value> {
    let mut copy = job.clone();
    let fields = copy.as_object_mut().ok_or_else(|| internal("Invalid preparation job"))?;
    if let Some(stages) = fields.get_mut("preparationStages") {
        let stages = stages.as_object_mut().ok_or_else(|| internal("Invalid preparation stages"))?;
        stages.remove(stage);
        if stages.is_empty() { fields.remove("preparationStages"); }
    }
    Ok(copy)
}

fn validate_first_stage_change(before: &Value, after: &Value, job_id: &str) -> ApiResult<()> {
    let old = crate::row(before, "jobs", job_id)?;
    let new = crate::row(after, "jobs", job_id)?;
    if old["kind"] != "assistant" || old["status"] != "running"
        || !matches!(old["purpose"].as_str(), Some("auto_prepare" | "auto_revalidate" | "engine_prepare")) {
        return Err(internal("First-pass scope requires one running preparation job"));
    }
    let mut reverted = after.clone();
    *crate::row_mut(&mut reverted, "jobs", job_id)? = old.clone();
    let mut old_without=without_stage(old,"first")?;let mut new_without=without_stage(new,"first")?;
    if old["preparationStages"].get("firstAdmission").is_none() && new["preparationStages"].get("firstAdmission").is_some() {
        if !old["preparationStages"]["first"].is_null()||!new["preparationStages"]["first"].is_null() {
            return Err(internal("Paid first reservation cannot rewrite settled output"));
        }
        validate_lifecycle_admission(after,new,"firstAdmission","reserved","reservedAt")?;
        let initial=&old["preparationStages"]["initialAdmission"];
        let reserved=&new["preparationStages"]["firstAdmission"];
        if initial["status"]!="scheduled" || initial["owner"]!=reserved["owner"] || initial["requestSha256"]!=reserved["requestSha256"] {
            return Err(internal("Paid first reservation differs from initial admitted run"));
        }
        new_without=without_stage(&new_without,"firstAdmission")?;
    }
    if old.get("factFollowups").is_none()&&new.get("factFollowups").is_some(){
        crate::fact_followup::validate_created(before,new)?;
        old_without.as_object_mut().unwrap().remove("factFollowups");new_without.as_object_mut().unwrap().remove("factFollowups");
    }
    if old.get("videoFrameNeeds").is_none()&&old.get("frameNeedOutcome").is_none()
        &&(new.get("videoFrameNeeds").is_some()||new.get("frameNeedOutcome").is_some()) {
        // These fields are exact native derivations of this immutable paid
        // first result/source snapshot, never an arbitrary first-stage whitelist.
        crate::video_frame_work::validate_created(before,new)?;
        for field in ["videoFrameNeeds","frameNeedOutcome"]{new_without.as_object_mut().unwrap().remove(field);}
    }
    if reverted != *before || old_without != new_without {
        return Err(internal("First-pass scope changed protected workspace state"));
    }
    if old["preparationStages"]["first"] != new["preparationStages"]["first"] {
        if !old["preparationStages"]["first"].is_null()
            || new["preparationStages"]["first"]["status"] != "completed"
            || !new["preparationStages"]["first"]["result"].is_object() {
            return Err(internal("First-pass evidence must be appended once"));
        }
    }
    crate::db_guards::validate_change(before, after)?;
    Ok(())
}

fn validate_review_checkpoint_change(before:&Value,after:&Value,job_id:&str)->ApiResult<()> {
    let old=crate::row(before,"jobs",job_id)?;
    let new=crate::row(after,"jobs",job_id)?;
    if old["kind"]!="assistant"||old["status"]!="running"
        ||!matches!(old["purpose"].as_str(),Some("auto_prepare"|"auto_revalidate"|"engine_prepare"))
        ||old["preparationStages"]["first"]["status"]!="completed"
        ||old["preparationStages"]["first"]["reviewRequired"]!=true
        ||!old["preparationStages"]["review"].is_null() {
        return Err(internal("Review checkpoint scope requires one running reviewed preparation job"));
    }
    let mut reverted=after.clone();
    *crate::row_mut(&mut reverted,"jobs",job_id)?=old.clone();
    if reverted!=*before||without_stage(old,"reviewChunks")?!=without_stage(new,"reviewChunks")? {
        return Err(internal("Review checkpoint scope changed protected workspace state"));
    }
    let prior=&old["preparationStages"]["reviewChunks"];
    let saved=&new["preparationStages"]["reviewChunks"];
    if prior!=saved {
        let chunks=saved["chunks"].as_array().filter(|rows|!rows.is_empty()&&rows.len()<=4)
            .ok_or_else(||internal("Review checkpoint has invalid chunks"))?;
        crate::preparation_review::chunks::validate_state_policy(saved)?;
        let uncapped=saved["version"]==2;
        if saved["maxInputBytes"].as_u64().is_none_or(|n|n>2_400_000)
            ||saved["automaticResume"]!=false {
            return Err(internal("Review checkpoint budget or policy changed"));
        }
        let review_request=crate::preparation_review::plan_review_for_job(old).map_err(internal)?
            .ok_or_else(||internal("Review recipients missing"))?;
        let expected_ids=review_request["items"].as_array()
            .ok_or_else(||internal("Review recipients missing"))?;
        let mut seen_ids=Vec::new();let mut charged=0u64;let mut observed=0u64;
        let mut reserved=0u64;let mut input_bytes=0u64;let mut unknown=0u64;
        for (index,chunk) in chunks.iter().enumerate() {
            if chunk["id"]!=format!("chunk-{}",index+1) {return Err(internal("Review chunk identity changed"));}
            let ids=chunk["itemIds"].as_array().filter(|v|!v.is_empty()&&v.len()<=25)
                .ok_or_else(||internal("Review chunk recipients invalid"))?;
            seen_ids.extend(ids.iter().cloned());
            let attempts=chunk["attempts"].as_array().filter(|v|v.len()<=2)
                .ok_or_else(||internal("Review attempts invalid"))?;
            for attempt in attempts {
                let held=if uncapped {
                    if attempt.get("reservedWebCalls")!=Some(&Value::Null){return Err(internal("Uncapped review invented web reservation"));}
                    0
                }else{attempt["reservedWebCalls"].as_u64().filter(|n|*n>0&&*n<=saved["maxWebCalls"].as_u64().unwrap())
                    .ok_or_else(||internal("Review web reservation invalid"))?};
                let contract=crate::preparation_review::chunks::chunk_contract(&attempt["contract"])
                    .map_err(|_|internal("Review attempt contract invalid"))?;
                if (if uncapped {contract.get("maxWebCalls")!=Some(&Value::Null)}else{contract["maxWebCalls"]!=held})||contract["attemptId"]!=attempt["id"]
                    ||contract["chunkId"]!=chunk["id"]||contract["profileSha256"]!=saved["profile"]["profileSha256"]
                    ||contract["requestSha256"]!=attempt["requestDigest"] {
                    return Err(internal("Review web reservation differs from immutable contract"));
                }
                let bytes=attempt["inputBytes"].as_u64()
                    .ok_or_else(||internal("Review input reservation invalid"))?;
                input_bytes=input_bytes.checked_add(bytes).ok_or_else(||internal("Review input budget overflow"))?;
                match attempt["status"].as_str() {
                    Some("completed")=>{
                        let used=attempt["observedWebCalls"].as_u64().filter(|n|uncapped||*n<=held)
                            .ok_or_else(||internal("Review observed web use invalid"))?;
                        observed+=used;charged+=used;
                    },
                    Some("running")=>{reserved+=held;charged+=held;},
                    Some("unknown")=>{unknown+=1;reserved+=held;charged+=held;},
                    _=>return Err(internal("Review attempt status invalid")),
                }
            }
        }
        if seen_ids!=expected_ids.iter().map(|item|item["id"].clone()).collect::<Vec<_>>()
            ||(!uncapped&&charged>saved["maxWebCalls"].as_u64().unwrap())
            ||input_bytes>saved["maxInputBytes"].as_u64().unwrap() {
            return Err(internal("Review checkpoint recipients or budget changed"));
        }
        if charged>0||input_bytes>0 {
            if uncapped {crate::preparation_review::chunks::validate_usage(saved)?;} else {
            let usage=&saved["usage"];
            if usage["chargedWebCalls"]!=charged||usage["observedCompletedWebCalls"]!=observed
                ||usage["reservedUnconfirmedWebCalls"]!=reserved||usage["unknownAttemptCount"]!=unknown
                ||usage["inputBytes"]!=input_bytes||usage["webBudget"]!=saved["maxWebCalls"]
                ||usage["actualTotalWebCallsKnown"]!=(reserved==0) {
                return Err(internal("Review checkpoint usage differs from attempts"));
            }
            }
        }else if !saved["usage"].is_null() {
            return Err(internal("Review checkpoint has unearned usage"));
        }
        let mut settled_existing_attempt=false;
        if prior.is_object() {
            for key in ["version","bundleId","bundleDigest","requestDigest","connectorBinding","profile",
                "planDigest","maxWebCalls","maxInputBytes","automaticResume"] {
                if prior[key]!=saved[key] {return Err(internal("Review checkpoint binding changed"));}
            }
            let old_chunks=prior["chunks"].as_array().ok_or_else(||internal("Invalid prior review chunks"))?;
            if old_chunks.len()!=chunks.len() {return Err(internal("Review checkpoint recipients changed"));}
            let mut changed_chunks=0;
            for (old_chunk,new_chunk) in old_chunks.iter().zip(chunks) {
                if old_chunk!=new_chunk {changed_chunks+=1;}
                for key in ["id","itemIds"] {
                    if old_chunk[key]!=new_chunk[key] {return Err(internal("Review checkpoint recipients changed"));}
                }
                if !old_chunk["result"].is_null()&&old_chunk["result"]!=new_chunk["result"] {
                    return Err(internal("Completed review chunk is immutable"));
                }
                let old_attempts=old_chunk["attempts"].as_array().ok_or_else(||internal("Invalid prior review attempts"))?;
                let new_attempts=new_chunk["attempts"].as_array().filter(|rows|rows.len()<=2)
                    .ok_or_else(||internal("Invalid review attempts"))?;
                if new_attempts.len()<old_attempts.len()||new_attempts.len()>old_attempts.len()+1 {
                    return Err(internal("Review attempt history changed"));
                }
                for (index,prior_attempt) in old_attempts.iter().enumerate() {
                    let current=&new_attempts[index];
                    if prior_attempt!=current && (index+1!=old_attempts.len()
                        ||prior_attempt["status"]!="running"
                        ||!matches!(current["status"].as_str(),Some("completed"|"unknown"))
                        ||without(prior_attempt,&["status","finishedAt","observedWebCalls","resultDigest","errorCode","retryable"])
                            !=without(current,&["status","finishedAt","observedWebCalls","resultDigest","errorCode","retryable"])) {
                        return Err(internal("Review attempt evidence was rewritten"));
                    }
                    if prior_attempt!=current {
                        if new_attempts.len()!=old_attempts.len() {
                            return Err(internal("Review settlement cannot also reserve another attempt"));
                        }
                        settled_existing_attempt=true;
                    }
                }
            }
            if changed_chunks>1 {return Err(internal("Review checkpoint changed more than one chunk"));}
        }else if !prior.is_null() {
            return Err(internal("Invalid prior review checkpoint"));
        }else if saved["status"]!="pending"||chunks.iter().any(|c|!c["result"].is_null()
            ||c["attempts"].as_array().is_none_or(|v|!v.is_empty())) {
            return Err(internal("New review checkpoint must start empty"));
        }
        // Settlement of an existing reservation retains finished or uncertain
        // work even when its sources changed. Initialization and new spending
        // still require full currentness. No proposal or source can change here.
        if settled_existing_attempt {
            crate::preparation_review::chunks::validate_settlement(after,old,new)?;
        }else{
            crate::preparation_review::chunks::current(after,new)?;
        }
    }
    crate::db_guards::validate_change(before,after)?;
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

const PG_JOB_PREDICATE: &str = r#"(kind='media' OR kind IN ('media_analysis','media_analysis_applicability') OR payload->>'kind' IN ('media_analysis','media_analysis_applicability') OR payload->>'purpose' IN ('auto_prepare','auto_revalidate','auto_media')
 OR (kind='assistant' AND status IN ('running','queued'))
 OR id IN (SELECT i.payload#>>'{autoPreparation,jobId}' FROM communityhero.items i WHERE i.workspace_id=$1
           UNION SELECT i.payload#>>'{autoRevalidation,jobId}' FROM communityhero.items i WHERE i.workspace_id=$1
           UNION SELECT p.payload->>'prepareRunId' FROM communityhero.proposals p WHERE p.workspace_id=$1
           UNION SELECT p.payload#>>'{origin,prepareRunId}' FROM communityhero.proposals p WHERE p.workspace_id=$1
            UNION SELECT p.payload#>>'{recovery,prepareRunId}' FROM communityhero.proposals p WHERE p.workspace_id=$1))"#;
const PG_SCHEDULE_JOBS: &str = "(kind IN ('media','media_audio','media_analysis','media_analysis_applicability') OR payload->>'kind' IN ('media','media_audio','media_analysis','media_analysis_applicability') OR (jsonb_typeof(payload->'factFollowups')='array' AND jsonb_array_length(payload->'factFollowups')>0) OR ((kind='assistant' OR payload->>'kind'='assistant') AND (status IN ('running','queued') OR payload->>'status' IN ('running','queued'))))";
const PG_BOUND_JOBS: &str = "(kind IN ('media','media_audio','media_analysis','media_analysis_applicability') OR payload->>'kind' IN ('media','media_audio','media_analysis','media_analysis_applicability') OR id=$2 OR payload->>'id'=$2 OR (jsonb_typeof(payload->'factFollowups')='array' AND jsonb_array_length(payload->'factFollowups')>0) OR ((kind='assistant' OR payload->>'kind'='assistant') AND (status IN ('running','queued') OR payload->>'status' IN ('running','queued'))))";

// Match scoped_item_ids. The subquery always binds the same workspace; author
// history is deliberately not truncated before customer-case selection counts
// omissions and finds older contract requests. Relational branch/item IDs stay
// authoritative and payload projection validation still runs for loaded rows.
const PG_SCOPED_ITEMS:&str=r#"(id=ANY($2::text[]) OR
 ((NOT (payload ? 'account') OR payload->'account'=(SELECT metadata->'account' FROM communityhero.workspaces WHERE id=$1))
 AND (NOT (payload ? 'accountId') OR payload->'accountId'=(SELECT metadata->'account' FROM communityhero.workspaces WHERE id=$1))
 AND jsonb_typeof(payload->'authorId')='string' AND jsonb_typeof(payload->'platform')='string'
 AND (payload->>'authorId',payload->>'platform') IN
 (SELECT s.payload->>'authorId',s.payload->>'platform' FROM communityhero.items s
  WHERE s.workspace_id=$1 AND s.id=ANY($2::text[])
  AND jsonb_typeof(s.payload->'authorId')='string' AND jsonb_typeof(s.payload->'platform')='string'
  AND COALESCE(s.payload->>'authorId','')<>'' AND COALESCE(s.payload->>'platform','')<>'')))"#;

fn scoped_condition(table:&str)->Option<String> {
    match table {
        "items"=>Some(PG_SCOPED_ITEMS.to_owned()),
        "branches"=>Some(format!("id IN (SELECT branch_id FROM communityhero.items WHERE workspace_id=$1 AND {PG_SCOPED_ITEMS})")),
        // Load either representation so a stale relational discriminator cannot
        // hide a selected recipient's UNKNOWN operation from integrity checks.
        "proposals"=>Some("(item_id=ANY($2::text[]) OR payload->>'itemId'=ANY($2::text[]))".to_owned()),
        _=>None,
    }
}

async fn load_pg_claim(connection: &mut PgConnection, first_job:Option<&str>,schedule:bool) -> ApiResult<Value> {
    load_pg_preparation(connection,first_job,schedule,true).await
}
async fn load_pg_preparation(connection:&mut PgConnection,first_job:Option<&str>,schedule:bool,lock:bool)->ApiResult<Value> {
    let mut materialized=crate::performance::Span::new("preparation.projection.materialized");
    let mut materialized_rows=0usize;
    let mut materialized_collections=0usize;
    let conductor=crate::conductor_authority::current_context().map(|ctx|ctx.run_id);
    let statement=if lock {"SELECT account, execution_enabled, metadata::text AS claim_metadata FROM communityhero.workspaces WHERE id=$1 FOR UPDATE"}
        else {"SELECT account, execution_enabled, metadata::text AS claim_metadata FROM communityhero.workspaces WHERE id=$1"};
    let record = sqlx::query(statement)
        .bind(WORKSPACE).fetch_one(&mut *connection).await?;
    #[cfg(test)] crate::performance::r3_sql_read();
    if record.try_get::<bool, _>("execution_enabled")? { return Err(internal("PostgreSQL pilot execution must remain disabled")); }
    let metadata = parse(record.try_get::<&str, _>("claim_metadata")?)?;
    let mut materialized_bytes=record.try_get::<&str,_>("claim_metadata")?.len();
    if TABLES.iter().any(|table| metadata.get(*table).is_some()) {
        return Err(internal("Workspace metadata contains entity collections"));
    }
    let mut view = claim_metadata(&metadata)?;
    if record.try_get::<Option<String>, _>("account")?.as_deref() != view["account"].as_str() {
        return Err(internal("Workspace identity mismatch"));
    }
    let recipients=if let Some(job_id)=first_job.filter(|id|!id.is_empty()&&!schedule) {
        // Load only the small recipient discriminator here. The complete job is
        // still read below with relational identity validation in this snapshot.
        let ids:Option<String>=sqlx::query_scalar("SELECT (payload->'prepareBundle'->'itemIds')::text FROM communityhero.jobs WHERE workspace_id=$1 AND id=$2")
            .bind(WORKSPACE).bind(job_id).fetch_optional(&mut *connection).await?.flatten();
        #[cfg(test)] crate::performance::r3_sql_read();
        let recipients=ids.map(|ids|parse(&ids)).transpose()?.and_then(|ids|recipient_ids(&ids));
        if let Some(recipients)=recipients {
            // Same snapshot as the complete jobs below. Avoid fetching research
            // results/parent captures just to discover current source recipients.
            // Preserve a malformed marker's presence as null to disable narrowing.
            let _facts=crate::performance::Span::new("preparation.context.fact_scope");
            let records=sqlx::query("SELECT id,kind,status,jsonb_build_object('id',payload->'id','kind',payload->'kind', \
                'purpose',payload->'purpose','status',payload->'status','parentPrepareJobId',payload->'parentPrepareJobId', \
                'requestedItemIds',payload->'requestedItemIds','factWorkerScope', \
                CASE WHEN jsonb_typeof(payload->'factWorkerScope')='object' THEN jsonb_build_object( \
                    'version',payload#>'{factWorkerScope,version}','account',payload#>'{factWorkerScope,account}', \
                    'connectorBinding',payload#>'{factWorkerScope,connectorBinding}','jobId',payload#>'{factWorkerScope,jobId}', \
                    'parentJobId',payload#>'{factWorkerScope,parentJobId}','itemIds',payload#>'{factWorkerScope,itemIds}') \
                    ELSE 'null'::jsonb END)::text AS fact_scope \
                FROM communityhero.jobs WHERE workspace_id=$1 AND (kind='assistant' OR payload->>'kind'='assistant') \
                AND payload->>'purpose'='public_fact_followup' AND (status IN ('queued','running') OR payload->>'status' IN ('queued','running')) \
                AND payload ? 'factWorkerScope' ORDER BY ordinal")
                .bind(WORKSPACE).fetch_all(&mut *connection).await?;
            #[cfg(test)] crate::performance::r3_sql_read();
            let mut facts=Vec::with_capacity(records.len());
            for record in records {
                let fact=parse(record.try_get::<&str,_>("fact_scope")?)?;
                if record.try_get::<&str,_>("id")?!=text(&fact,"id")?
                    || record.try_get::<Option<String>,_>("kind")?.as_deref()!=fact["kind"].as_str()
                    || record.try_get::<Option<String>,_>("status")?.as_deref()!=fact["status"].as_str() {
                    return Err(internal("Fact context record identity or projection mismatch"));
                }
                facts.push(fact);
            }
            recipient_union(&view,recipients,&facts)
        }else{None}
    }else{None};
    for table in TABLES {
        if OMITTED.contains(&table) && !(schedule&&table=="audit") { continue; }
        materialized_collections+=1;
        let narrowed=recipients.as_ref().and_then(|_|scoped_condition(table));
        let condition = if let Some(condition)=narrowed.as_deref() {condition} else if table == "jobs" {
            if schedule {PG_SCHEDULE_JOBS}
            else if first_job.is_some() {PG_BOUND_JOBS}
            else {PG_JOB_PREDICATE}
        } else if table=="audit" {"action='local_admission.committed' AND payload->>'kind'='prepare'"}
        else { "TRUE" };
        let material_condition=(table=="jobs").then(||format!("({condition} OR {})",retained_material_job_sql()));
        let condition=material_condition.as_deref().unwrap_or(condition);
        // Bind the exact task-local grant inside this same snapshot. Default
        // projections retain their previous SQL and never load conductor history.
        let conductor_condition=if table=="jobs"&&conductor.is_some(){
            let parameter=if first_job.is_some(){3}else{2};
            Some(format!("({condition} OR id=${parameter} OR payload->>'id'=${parameter})"))
        }else{None};
        let condition=conductor_condition.as_deref().unwrap_or(condition);
        let statement = format!("SELECT id,payload::text{} FROM communityhero.{table} WHERE workspace_id=$1 AND {condition} ORDER BY ordinal",
            projection(table).iter().map(|(column, _)| format!(",{column}")).collect::<String>());
        let mut query=sqlx::query(sqlx::AssertSqlSafe(statement.as_str())).bind(WORKSPACE);
        if narrowed.is_some(){query=query.bind(recipients.as_ref().unwrap());}
        if table=="jobs" {if let Some(job_id)=first_job {query=query.bind(job_id);}}
        if table=="jobs" {if let Some(run)=conductor.as_deref(){query=query.bind(run);}}
        let records = query.fetch_all(&mut *connection).await?;
        #[cfg(test)] crate::performance::r3_sql_read();
        materialized_rows+=records.len();
        let mut data = Vec::with_capacity(records.len());
        let mut seen = HashSet::new();
        for record in records {
            materialized_bytes+=record.try_get::<&str,_>("payload")?.len();
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
    // Selected entity payloads plus metadata, excluding optional discriminator
    // probes and control rows loaded by scope_context below. No reserialization.
    materialized.counts(materialized_rows,materialized_bytes,materialized_collections);
    drop(materialized);
    super::scope_context::load(connection,&mut view).await?;
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
    let affected=query.execute(connection).await?.rows_affected();
    #[cfg(test)] crate::performance::r3_sql_write();
    if affected != 1 { return Err(internal("Preparation claim record disappeared")); }
    Ok(())
}

#[derive(Clone,Copy)]
enum PreparationChange<'a>{Claim,Schedule,First(&'a str),ReviewCheckpoint(&'a str)}
impl PreparationChange<'_>{
    fn job_id(&self)->Option<&str>{match self{Self::Claim|Self::Schedule=>None,Self::First(id)|Self::ReviewCheckpoint(id)=>Some(id)}}
    fn validate(&self,before:&Value,after:&Value)->ApiResult<()> {
        match self {
            Self::Claim=>validate_claim_change(before,after),
            Self::Schedule=>validate_schedule_change(before,after),
            Self::First(id)=>validate_first_stage_change(before,after,id),
            Self::ReviewCheckpoint(id)=>validate_review_checkpoint_change(before,after,id),
        }
    }
}

impl Database {
    /// Rolling automatic discovery uses the EXACT existing claim projection.
    /// Complete dependency/UNKNOWN/source/knowledge and retained job history
    /// remain; unrelated private discussion/audit/approval bodies are omitted.
    /// Read-only advisory capture; the writer recaptures and admits every claim.
    pub(crate) async fn read_preparation_discovery(&self) -> ApiResult<Value> {
        let _total=crate::performance::Span::new("preparation.discovery.read.total");
        match self {
            Self::Sqlite(_)=>projection_of(&self.read().await?),
            Self::Postgres{reader,..}=>{
                let wait=crate::performance::Span::new("preparation.discovery.reader.wait");
                let mut tx=reader.begin().await?;
                drop(wait);
                sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ READ ONLY")
                    .execute(&mut *tx).await?;
                let value=load_pg_preparation(&mut tx,None,false,false).await?;
                tx.commit().await?;
                Ok(value)
            }
        }
    }
    /// Capacity preview uses the same source/job projection as scheduling.
    /// It grants no admission: the writer transaction recaptures and compares.
    pub(crate) async fn read_preparation_schedule(&self) -> ApiResult<Value> {
        match self {
            Self::Sqlite(_)=>schedule_projection(&self.read().await?),
            Self::Postgres{reader,..}=>{
                let mut tx=reader.begin().await?;
                sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ READ ONLY").execute(&mut *tx).await?;
                let value=load_pg_preparation(&mut tx,None,true,false).await?;
                tx.commit().await?;
                Ok(value)
            }
        }
    }
    /// Consistent current evidence for one captured preparation job. This does
    /// not acquire writer ownership or authorize spending/admission; the caller
    /// must run its normal preflight and later transactional admission guards.
    pub(crate) async fn read_preparation_context(&self,job_id:&str)->ApiResult<Value> {
        let _total=crate::performance::Span::job("preparation.context.read",job_id);
        match self {
            Self::Sqlite(_)=>projection_for(&self.read().await?,Some(job_id)),
            Self::Postgres{reader,..}=>{
                let mut tx=reader.begin().await?;
                sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ READ ONLY").execute(&mut *tx).await?;
                let value=load_pg_preparation(&mut tx,Some(job_id),false,false).await?;
                tx.commit().await?;
                Ok(value)
            }
        }
    }
    /// Caller holds the App writer gate. PostgreSQL also uses the leased writer
    /// and workspace row lock. The closure may only perform claim/reconciliation.
    pub(crate) async fn change_preparation_claim_observed<T>(
        &self, f: impl FnOnce(&mut Value) -> ApiResult<T>,
    ) -> ApiResult<(T, bool)> {
        self.change_preparation_projection(PreparationChange::Claim, f).await
    }

    /// Build current explicit preparation evidence and append its single job
    /// under the existing writer lock, without loading unrelated job histories.
    pub(crate) async fn change_preparation_schedule_observed<T>(
        &self, f:impl FnOnce(&mut Value)->ApiResult<T>,
    )->ApiResult<(T,bool)> {
        let _total=crate::performance::Span::new("preparation.schedule.total");
        self.change_preparation_projection(PreparationChange::Schedule,f).await
    }

    /// Save only the immutable first-pass evidence for one running preparation
    /// job. The closure still sees current source, knowledge and item evidence
    /// under the same transaction, but unrelated histories are never loaded.
    pub(crate) async fn change_preparation_first_observed<T>(
        &self, job_id: &str, f: impl FnOnce(&mut Value) -> ApiResult<T>,
    ) -> ApiResult<(T, bool)> {
        let _total = crate::performance::Span::new("preparation.first.total");
        self.change_preparation_projection(PreparationChange::First(job_id), f).await
    }

    /// A stronger-review checkpoint changes only one running job's durable
    /// reviewChunks field. Explicit engine final admission and archive use their
    /// own atomic scope; other preparation completion paths remain full writes.
    pub(crate) async fn change_preparation_review_checkpoint_observed<T>(
        &self, job_id:&str, f:impl FnOnce(&mut Value)->ApiResult<T>,
    )->ApiResult<(T,bool)> {
        let _total=crate::performance::Span::new("preparation.review_checkpoint.total");
        self.change_preparation_projection(PreparationChange::ReviewCheckpoint(job_id),f).await
    }

    async fn change_preparation_projection<T>(
        &self, scope:PreparationChange<'_>, f: impl FnOnce(&mut Value) -> ApiResult<T>,
    ) -> ApiResult<(T, bool)> {
        match self {
            Self::Sqlite(_) => self.change_observed(|workspace| {
                let before = if matches!(scope,PreparationChange::Schedule){schedule_projection(workspace)?}
                    else{projection_for(workspace,scope.job_id())?};
                let mut after = before.clone();
                let result = f(&mut after)?;
                scope.validate(&before,&after)?;
                if matches!(scope,PreparationChange::Schedule){merge_schedule_delta(workspace,&before,&after)?;}
                else{merge_claim_delta(workspace, &before, &after)?;}
                Ok(result)
            }).await,
            Self::Postgres { writer, .. } => {
                let mut connection = writer.acquire().await?;
                let mut tx = sqlx::Connection::begin(&mut *connection).await?;
                // Keep the heavy source/delta alive until COMMIT/ROLLBACK and
                // awaited pool return finish, including every early failure.
                let mut before = Value::Null;
                let mut after = Value::Null;
                let outcome:ApiResult<(T,bool)> = async {
                    let loading = crate::performance::Span::new("preparation.projection.load");
                    before = load_pg_claim(&mut tx,scope.job_id(),matches!(scope,PreparationChange::Schedule)).await?;
                    drop(loading);
                    after = before.clone();
                    let result = f(&mut after)?;
                    scope.validate(&before,&after)?;
                    if before == after { return Ok((result, false)); }
                    if matches!(scope,PreparationChange::Schedule) {
                        for (old,new) in rows(&before,"jobs")?.iter().zip(rows(&after,"jobs")?) {
                            if old!=new {persist_claim_record(&mut tx,"jobs",new,false).await?;}
                        }
                        let job=rows(&after,"jobs")?.last().ok_or_else(||internal("Preparation scheduling job missing"))?;
                        persist_claim_record(&mut tx,"jobs",job,true).await?;
                        for receipt in &rows(&after,"audit")?[rows(&before,"audit")?.len()..] {
                            persist_claim_record(&mut tx,"audit",receipt,true).await?;
                        }
                        return Ok((result,true));
                    }
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
                    Ok((result, true))
                }.await;
                let (outcome,completion) = super::pg_writer::settle(tx,outcome).await;
                super::pg_writer::release(&mut connection,writer,completion).await;
                drop(after);
                drop(before);
                outcome
            }
        }
    }
}

#[cfg(test)]
#[path = "storage_preparation_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "storage_preparation_scope_tests.rs"]
mod scope_tests;

#[cfg(test)]
#[path="storage_preparation_conductor_tests.rs"]
mod conductor_tests;

#[cfg(test)]
#[path="storage_preparation_fact_context_tests.rs"]
mod fact_context_tests;
