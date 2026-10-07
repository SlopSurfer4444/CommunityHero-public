//! Explicit, durable preparation for the headless engine API.
//!
//! This path creates reviewable drafts only. It never approves or dispatches
//! them, and media prerequisites hold only the affected items.
use axum::{Json, extract::State};
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};

const MAX_ITEMS: usize = 100;
const MAX_ITEM_ID: usize = 256;
const MAX_INSTRUCTION: usize = 12_000;
pub(crate) const MAX_REQUEST_BYTES: usize = 550_000;
const MAX_CAPTURE_BYTES:usize=2_400_000;
#[path="preparation_capacity.rs"]
pub(crate) mod capacity;
#[path="preparation_manual_wait.rs"]
pub(crate) mod manual_wait;
#[cfg(test)]
#[path="preparation_capture_context_tests.rs"]
mod capture_context_tests;
// Keep aligned with assistant-images.mjs. Shared post attachments occupy one
// slot per exact post/index; equal URLs on different posts are not shared proof.
pub(crate) const MAX_REQUEST_IMAGES: usize = 16;
pub(crate) const IMAGE_CAPACITY_ERROR: &str = "Selected assistant image evidence exceeds the 16-image budget; split preparation recipients";

#[derive(Clone)]
pub(crate) struct Input {
    pub(crate) item_ids: Vec<String>,
    pub(crate) instruction: Option<String>,
}

#[derive(Clone)]
pub(crate) struct Scheduled {
    pub(crate) job_id: String,
    request: Option<Value>,
    selected: Vec<String>,
    held: Vec<Value>,
}
impl Scheduled {
    pub(crate) fn request(&self)->Option<&Value>{self.request.as_ref()}
}

pub(crate) fn parse(body: &Value) -> super::ApiResult<Input> {
    let object = body
        .as_object()
        .ok_or_else(|| super::bad("Engine prepare body must be an object"))?;
    if object
        .keys()
        .any(|key| !matches!(key.as_str(), "itemIds" | "instruction" | "requestId" | "workflowMode"))
    {
        return Err(super::bad(
            "Engine prepare body contains unsupported fields",
        ));
    }
    if object.get("workflowMode").is_some_and(|mode|mode!=crate::continuous_preparation::MODE){
        return Err(super::bad("Unsupported engine preparation workflow mode"));
    }
    let values = object
        .get("itemIds")
        .and_then(Value::as_array)
        .filter(|values| !values.is_empty() && values.len() <= MAX_ITEMS)
        .ok_or_else(|| super::bad("Engine prepare requires 1 to 100 itemIds"))?;
    let mut seen = BTreeSet::new();
    let mut item_ids = Vec::with_capacity(values.len());
    for value in values {
        let item_id = value
            .as_str()
            .filter(|value| !value.trim().is_empty() && value.encode_utf16().count() <= MAX_ITEM_ID)
            .ok_or_else(|| super::bad("Engine prepare itemIds must be non-empty strings"))?;
        if !seen.insert(item_id.to_owned()) {
            return Err(super::bad("Engine prepare itemIds must be unique"));
        }
        item_ids.push(item_id.to_owned());
    }
    let instruction = match object.get("instruction") {
        None => None,
        Some(value) => Some(
            value
                .as_str()
                .filter(|value| value.encode_utf16().count() <= MAX_INSTRUCTION)
                .ok_or_else(|| {
                    super::bad("Engine prepare instruction must be at most 12000 characters")
                })?
                .to_owned(),
        ),
    };
    Ok(Input {
        item_ids,
        instruction,
    })
}

fn rehash(bundle: &mut Value) {
    let digest = format!(
        "{:x}",
        Sha256::digest(bundle["request"].to_string().as_bytes())
    );
    bundle["digest"] = json!(digest);
}

// The immutable bundle keeps every source byte. Count proven model omissions
// separately; unknown evidence stays conservative and neither limit is removed.
pub(crate) fn model_request_bytes(request:&Value)->usize {
    let mut compact=request.clone();
    if let Some(materials)=compact.get_mut("materials").and_then(Value::as_array_mut) {
        materials.retain(|material| {
            let evidence=&material["visualEvidence"];
            // The JS adapter removes validated video-frame material after its
            // own validation. Only warmed exact V2 proofs establish that same
            // modality here; never load artifacts or spawn an adapter to count.
            let omitted=material["kind"]=="visual_context"
                && evidence["schemaVersion"]==2
                && evidence["coverage"]["kind"]=="all_frames_fast_selected_neural"
                && evidence["source"]["durationMs"].as_u64().is_some_and(|duration|duration>0 && duration<=9_007_199_254_740_991)
                && material["postKey"].as_str().is_some_and(|key|!key.trim().is_empty() && evidence["source"]["postKey"]==key)
                && super::media_fullframes::validate_evidence(evidence).is_ok();
            !omitted
        });
    }
    for item in compact["items"].as_array_mut().into_iter().flatten() {
        if item["preview"].is_string() && item["preview"]==item["text"] {
            item.as_object_mut().unwrap().remove("preview");
        }
    }
    for post in compact["posts"].as_array_mut().into_iter().flatten() {
        if post["body"].is_string() && post["body"]==post["text"] {
            post.as_object_mut().unwrap().remove("body");
        }
    }
    compact.to_string().len()
}

pub(crate) fn image_count(request: &Value) -> usize {
    let selection=match super::prepare_bundle::visual::request_selection(request) {
        Ok(selection)=>selection,
        Err(_)=>return MAX_REQUEST_IMAGES+1,
    };
    let mut images = BTreeSet::new();
    for item in request["items"].as_array().into_iter().flatten() {
        let attachments = if item["attachments"].is_array() { &item["attachments"] } else { &item["commentAttachments"] };
        for (index, attachment) in attachments.as_array().into_iter().flatten().enumerate() {
            if matches!(attachment["type"].as_str(), Some("photo" | "image" | "sticker")) {
                images.insert(("comment", item["id"].as_str().unwrap_or(""), index));
            }
        }
        let post_id = item["postId"].as_str().filter(|id| !id.is_empty()).or_else(|| {
            request["branches"].as_array().into_iter().flatten().find(|branch| branch["id"] == item["branchId"])
                .and_then(|branch| branch["postId"].as_str())
        });
        if let Some(post) = request["posts"].as_array().into_iter().flatten().find(|post| post["id"].as_str() == post_id && post_id.is_some()) {
            for (index, attachment) in post["attachments"].as_array().into_iter().flatten().enumerate() {
                if matches!(attachment["type"].as_str(), Some("photo" | "image"))
                    &&selection.as_ref().is_none_or(|selected|super::prepare_bundle::visual::recipients(selected,post_id.unwrap(),index).contains(item["id"].as_str().unwrap_or(""))) {
                    images.insert(("post", post["id"].as_str().unwrap_or(""), index));
                }
            }
        }
    }
    images.len()
}

// Planning and scheduling measure the identical serialized request. The plan
// never carries approval authority; scheduling rebuilds current evidence.
pub(crate) fn build_request(d: &Value, ids: &[Value], instruction: Option<&str>) -> Result<Value, &'static str> {
    build_request_at(d,ids,instruction,None)
}
fn build_request_at(d: &Value, ids: &[Value], instruction: Option<&str>, fact_selected_at:Option<&str>) -> Result<Value, &'static str> {
    let mut context=super::prepare_bundle::EvidenceContext::new(d);
    let mut bundle = super::prepare_bundle::build_engine_capture_with_context(&context, ids, &[])?;
    bundle["request"]["purpose"] = json!("triage");
    // New work uses one complete generation, including factual and editorial
    // checks. Existing paid jobs retain their immutable captured request.
    bundle["request"]["preparationMode"] = json!("single_pass_v1");
    bundle["request"]["responseContract"] = json!("compact_decisions_v1");
    // Capture model presentation/research semantics only for newly scheduled
    // work. Recovery consumes its saved request without inserting defaults.
    bundle["request"]["modelContextContract"] = json!("shared_moderation_v1");
    bundle["request"]["researchPolicy"] = json!("context_sufficient_v1");
    bundle["request"]["researchLimitContract"] = json!("uncapped_evidence_v1");
    bundle["request"]["recoveryEvidenceContract"] = json!("held_candidates_v1");
    bundle["request"]["visualNeedContract"] = json!(super::prepare_bundle::visual::CONTRACT);
    bundle["request"]["visualSelection"] = super::prepare_bundle::visual::empty();
    bundle["request"]["factDependencyContract"] = json!(super::fact_followup::CONTRACT);
    bundle["factSourceFingerprints"]=json!({});
    for id in ids.iter().filter_map(Value::as_str){
        context.begin_fresh_selection();
        bundle["factSourceFingerprints"][id]=json!(context.review_fingerprint(id)?);
    }
    let facts=super::fact_followup::select(d,ids,fact_selected_at.unwrap_or(&super::now()))?;
    if let Some(materials)=bundle["request"]["materials"].as_array_mut(){
        materials.extend(facts["materials"].as_array().into_iter().flatten().cloned());
    }
    context.begin_fresh_selection();
    super::decision_media::attach_request_with_context(&context,&mut bundle["request"])?;
    super::preparation_unit::attach(d,&mut bundle,&super::now())?;
    bundle["factFollowupManifest"]=facts["manifest"].clone();
    if let Some(instruction) = instruction.filter(|value| !value.is_empty()) {
        let base = bundle["request"]["instruction"].as_str().unwrap_or("");
        bundle["request"]["instruction"] = json!(format!("{base}\n\nAdditional operator instruction:\n{instruction}"));
    }
    super::preparation_materials::attach_request(d,&mut bundle["request"])?;
    if bundle["request"].to_string().len() > MAX_CAPTURE_BYTES {
        return Err("Selected assistant evidence exceeds the 2400000-byte complete-capture budget; split recipients");
    }
    if model_request_bytes(&bundle["request"]) > MAX_REQUEST_BYTES {
        return Err("Selected assistant evidence exceeds the 550000-byte budget; reduce attachments or instruction");
    }
    if image_count(&bundle["request"]) > MAX_REQUEST_IMAGES {
        return Err(IMAGE_CAPACITY_ERROR);
    }
    rehash(&mut bundle);
    Ok(bundle)
}

/// The same exact-recipient operation guard is used by the advisory plan and
/// the scheduler. Neither path infers permission from a different comment.
pub(crate) fn operation_holds(d: &Value, item_ids: &[String]) -> BTreeMap<String, &'static str> {
    let requested: BTreeSet<&str> = item_ids.iter().map(String::as_str).collect();
    let mut holds = BTreeMap::new();
    for operation in super::list(d, "operations") {
        let Some(item_id) = operation["itemId"].as_str().filter(|id| requested.contains(id)) else { continue; };
        let reason = match operation["status"].as_str() {
            Some("unknown") => "operation_outcome_unknown",
            Some("dispatching") => "operation_dispatch_in_progress",
            Some("succeeded") => "operation_already_succeeded",
            _ => continue,
        };
        let priority = |reason| match reason {
            "operation_outcome_unknown" => 3,
            "operation_dispatch_in_progress" => 2,
            _ => 1,
        };
        let hold = holds.entry(item_id.to_owned()).or_insert(reason);
        if priority(reason) > priority(*hold) { *hold = reason; }
    }
    holds
}

pub(crate) fn schedule(d: &mut Value, input: Input) -> super::ApiResult<Scheduled> {
    schedule_at(d,input,None)
}
pub(crate) fn schedule_at(d: &mut Value, input: Input, fact_selected_at:Option<&str>) -> super::ApiResult<Scheduled> {
    let binding = super::active_binding(d)?;
    super::bridge_account(&binding)?;

    // A mixed selection must be split by the planner before any new job.
    let requested=input.item_ids.iter().map(|id|json!(id)).collect::<Vec<_>>();
    super::preparation_unit::capture(d,&requested,&super::now()).map_err(super::bad)?;
    let items: Vec<Value> = input
        .item_ids
        .iter()
        .map(|item_id| super::row(d, "items", item_id).cloned())
        .collect::<super::ApiResult<_>>()?;
    let media = super::media_queue::preparation_states(d, &items, &super::now())?;
    // Final group admission cannot create a new proposal over an operation
    // whose outcome is in flight, unknown, or already succeeded. Exclude those
    // exact recipients before the model call while retaining unrelated work.
    let operation_holds = operation_holds(d, &input.item_ids);
    let mut selected = Vec::new();
    let mut held = Vec::new();
    for item_id in &input.item_ids {
        if let Some(reason) = operation_holds.get(item_id) {
            held.push(json!({"itemId":item_id,"reason":reason}));
        } else if media.get(item_id).copied().flatten().is_some()
            && !super::decision_media::may_assess(d,super::row(d,"items",item_id)?).map_err(super::conflict)? {
            held.push(json!({"itemId":item_id,"reason":media[item_id]}));
        } else {
            // New captured requests assess the actual available text and name
            // their media dependencies in the same semantic pass. Publication
            // remains gated by an accepted exact decision receipt.
            selected.push(item_id.clone());
        }
    }

    let selected_values: Vec<Value> = selected.iter().map(|item_id| json!(item_id)).collect();
    let mut bundle = if selected.is_empty() {
        None
    } else {
        Some(build_request_at(d, &selected_values, input.instruction.as_deref(),fact_selected_at).map_err(super::bad)?)
    };

    let groups = bundle.as_ref().map(|bundle| super::prepare_bundle::capture_groups(d,bundle).map_err(super::bad)).transpose()?;
    let job_id = super::new_job(d, "assistant", "engine_prepare")?;
    let job = super::row_mut(d, "jobs", &job_id)?;
    job["purpose"] = json!("engine_prepare");
    job["requestedItemIds"] = json!(input.item_ids);
    job["selectedItemIds"] = json!(selected);
    job["held"] = json!(held);
    job["preparationStages"] = json!({"first":null,"review":null,"groupAdmission":groups.unwrap_or(json!([]))});
    let request = bundle.as_ref().map(|bundle| bundle["request"].clone());
    if let Some(bundle) = bundle.take() {
        job["prepareBundle"] = bundle;
    }
    let facts=super::row(d,"jobs",&job_id)?["prepareBundle"]["factFollowupManifest"].clone();
    if facts.is_array(){super::fact_followup::consume(d,&job_id,&facts)?;}
    if !selected.is_empty() {
        let reservation=super::preparation_reservations::capture(d,&job_id)?;
        super::preparation_reservations::check(d,&reservation,Some(&job_id))?;
        super::row_mut(d,"jobs",&job_id)?["scopeReservation"]=reservation;
        if let Some(scope)=super::preparation_workers::capture(d,super::row(d,"jobs",&job_id)?) {
            super::row_mut(d,"jobs",&job_id)?["preparationWorkerScope"]=scope;
        }
    }
    Ok(Scheduled {
        job_id,
        request,
        selected,
        held,
    })
}

fn preflight_capture(d: &Value, job_id: &str) -> super::ApiResult<()> {
    preflight_with_materials(d,job_id,false)
}
fn preflight(d: &Value, job_id: &str) -> super::ApiResult<()> {
    preflight_with_materials(d,job_id,true)
}
fn preflight_with_materials(d: &Value, job_id: &str,ready:bool) -> super::ApiResult<()> {
    let binding = super::active_binding(d)?;
    super::bridge_account(&binding)?;
    let job = super::row(d, "jobs", job_id)?;
    if job["status"] != "running"
        || job["kind"] != "assistant"
        || job["purpose"] != "engine_prepare"
    {
        return Err(super::conflict(
            "Engine preparation cancelled before model call",
        ));
    }
    let bundle = job
        .get("prepareBundle")
        .filter(|bundle| bundle.is_object())
        .ok_or_else(|| super::conflict("Engine preparation bundle is missing"))?;
    super::fact_followup::automatic_continuation_current(d,job)?;
    if job["preparationStages"]["first"].is_null()
        ||!job["preparationStages"]["groupAdmission"].is_array() {
        super::prepare_bundle::current(d, bundle).map_err(super::conflict)?;
        super::preparation_unit::current_bundle(d,bundle,&super::now()).map_err(super::conflict)?;
        if ready {
            if job["preparationStages"]["first"].is_null() {
                super::manual_frame_request::require_no_pending(d,job).map_err(super::conflict)?;
            }
            super::preparation_materials::require_request(d,&bundle["request"]).map_err(super::conflict)?;
        }
    }
    if job["preparationStages"]["first"]["status"]=="completed"
        &&job["preparationStages"]["first"]["reviewRequired"]!=true
        &&job["preparationStages"]["groupAdmission"].is_array(){return Ok(());}
    let pending:Vec<Value>=if job["preparationStages"]["first"].is_null()
        ||!job["preparationStages"]["groupAdmission"].is_array(){
        bundle["itemIds"].as_array().cloned().ok_or_else(||super::conflict("Engine preparation recipients are missing"))?
    }else{job["preparationStages"]["groupAdmission"].as_array().unwrap().iter()
        .filter(|g|g["status"]=="pending").flat_map(|g|g["itemIds"].as_array().cloned().unwrap_or_default()).collect()};
    let item_ids=&pending;
    if job.get("scopeReservation").is_some() {
        let ids=pending.iter().filter_map(|id|id.as_str().map(str::to_owned)).collect::<Vec<_>>();
        super::preparation_reservations::assert_available(d,&ids,Some(job_id))?;
    }
    let items: Vec<Value> = item_ids
        .iter()
        .map(|item_id| {
            item_id
                .as_str()
                .ok_or_else(|| super::conflict("Engine preparation recipient is invalid"))
                .and_then(|item_id| super::row(d, "items", item_id).cloned())
        })
        .collect::<super::ApiResult<_>>()?;
    let media = super::media_queue::preparation_states(d, &items, &super::now())?;
    if !super::decision_media::enabled(&bundle["request"]) && media.values().any(Option::is_some) {
        return Err(super::conflict(
            "Preparation media evidence changed before model call",
        ));
    }
    Ok(())
}

fn all_held_result(held: Vec<Value>) -> Value {
    json!({
        "conversationId":null,
        "prepareBundleId":null,
        "status":"held",
        "reason":null,
        "candidates":[],
        "held":held,
        "needsAttention":[],
        "selectedItemIds":[],
        "preparedItemIds":[]
    })
}

fn enrich(mut admission: Value, result: &Value, held: Vec<Value>, selected: Vec<String>) -> Value {
    let prepared: Vec<Value> = admission["candidates"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|candidate| candidate["status"] == "review")
        .filter_map(|candidate| candidate["itemId"].as_str().map(|item_id| json!(item_id)))
        .collect();
    // These are reviewed per-recipient holds. A hold has no proposal and must
    // not suppress a different recipient's reviewable draft in the same batch.
    let needs_attention: Vec<Value> = result["assessments"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|assessment| assessment["outcome"] == "needs_attention")
        .map(|assessment| json!({"itemId":assessment["itemId"],"reason":assessment["reason"]}))
        .collect();
    let object: &mut Map<String, Value> =
        admission.as_object_mut().expect("admission is an object");
    object.insert("held".into(), json!(held));
    object.insert("needsAttention".into(), json!(needs_attention));
    object.insert("selectedItemIds".into(), json!(selected));
    object.insert("preparedItemIds".into(), Value::Array(prepared));
    if object["status"] == "discussed" && !needs_attention.is_empty() {
        object.insert("status".into(), json!("held"));
    }
    admission
}

/// Acquisition observations can refresh an unpaid request only. Historical
/// first captures/results are immutable and never enter this path.
pub(crate) fn refresh_unpaid_materials(d:&mut Value,run:&str,original_request:&Value)->super::ApiResult<Value>{
    let binding=super::active_binding(d)?;super::bridge_account(&binding)?;
    let job=super::row(d,"jobs",run)?.clone();
    if job["kind"]!="assistant"||job["status"]!="running"
        ||!matches!(job["purpose"].as_str(),Some("engine_prepare"|"auto_prepare"|"auto_revalidate"))
        ||job["prepareBundle"]["request"]!=*original_request
        ||!job["preparationStages"]["first"].is_null()||job["preparationStages"].get("firstAdmission").is_some()
        ||job["retainedEvidence"].as_array().is_some_and(|refs|!refs.is_empty()) {
        return Err(super::conflict("Paid preparation capture cannot be refreshed"));
    }
    let mut bundle=job["prepareBundle"].clone();let old=bundle["request"].clone();
    super::prepare_bundle::current(d,&bundle).map_err(super::conflict)?;
    super::preparation_unit::current_bundle(d,&bundle,&super::now()).map_err(super::conflict)?;
    super::fact_followup::automatic_continuation_current(d,&job)?;
    super::preparation_materials::attach_request(d,&mut bundle["request"]).map_err(super::conflict)?;
    rehash(&mut bundle);
    super::preparation_materials::require_request(d,&bundle["request"]).map_err(super::conflict)?;
    let groups=super::prepare_bundle::capture_groups(d,&bundle).map_err(super::conflict)?;
    let request=bundle["request"].clone();
    let target=super::row_mut(d,"jobs",run)?;target["prepareBundle"]=bundle;
    target["preparationStages"]["groupAdmission"]=groups;
    super::preparation_review::refresh_initial_capture(d,run,&old,&request)?;
    super::preparation_reservations::refresh_unpaid_scope(d,run,&job)?;
    Ok(request)
}
async fn run(app: super::App, mut scheduled: Scheduled, mut resume_only:bool) -> super::ApiResult<Value> {
    if scheduled.request.is_none() {
        return Ok(all_held_result(scheduled.held));
    }
    // A paid response can outlive its ACK or the independent material receipt.
    // Recover and settle that ORIGINAL capture before source warming/acquisition.
    // No matching retained capture leaves the paid reservation intact below.
    if !resume_only {
        let original=scheduled.request.as_ref().unwrap().clone();
        if let Some(first)=super::preparation_review::recover_first_if_retained(&app,&scheduled.job_id,&original).await? {
            app.change_preparation_first(&scheduled.job_id,|d|{
                super::preparation_review::settle_first(d,&scheduled.job_id,&original,&first,&super::now())
            }).await?;
            resume_only=true;
        }
    }
    let first_lifecycle=if resume_only {None} else {
        Some(app.lifecycle_admission_token(super::runtime_lifecycle::AdmissionClass::Preparation).await?)
    };
    let mut material_wait=first_lifecycle.clone().map(manual_wait::Wait::new);
    loop {
        if let Some(wait)=material_wait.as_mut() {
            let original=scheduled.request.as_ref().unwrap();
            scheduled.request=Some(manual_wait::acquire(&app,&scheduled.job_id,original,wait,preflight_capture).await?);
        }
        // Retain the original epoch across warming and worker-slot waits.
        let result=async {
            super::media_fullframes::refresh(&app).await?;
            let state=app.db.read_preparation_context(&scheduled.job_id).await?;
            preflight(&state,&scheduled.job_id)?;
            let keys=super::preparation_workers::keys(&state,super::row(&state,"jobs",&scheduled.job_id)?);
            drop(state);
            let lease=app.preparation_workers.acquire(keys.clone()).await;
            let _assistant_guard=if lease.slot()==0 {Some(app.assistant_gate.clone().lock_owned().await)} else {None};
            lease.scope(run_owned(app.clone(),scheduled.clone(),resume_only,keys,first_lifecycle.clone())).await
        }.await;
        // The async scope above drops BOTH the lease and legacy gate first.
        // acquire rechecks the same unpaid capture; any admission or paid
        // evidence makes this a terminal error, never a second model attempt.
        if result.as_ref().is_err_and(|e|super::manual_frame_request::is_wait_reason(&e.1)) {
            if let Some(wait)=material_wait.as_mut() {
                wait.admission_race(scheduled.request.as_ref().unwrap());
                continue;
            }
        }
        return result;
    }
}

async fn run_owned(app:super::App,scheduled:Scheduled,resume_only:bool,keys:Option<BTreeSet<String>>,
    first_lifecycle:Option<super::runtime_lifecycle::OwnerToken>)->super::ApiResult<Value> {
    let run = scheduled.job_id.clone();
    app.db.read_preparation_context(&run).await.and_then(|d| {
        preflight(&d,&run)?;
        if keys.is_some()&&super::preparation_workers::keys(&d,super::row(&d,"jobs",&run)?)!=keys {
            return Err(super::conflict("Preparation family ownership changed while waiting; model not started"));
        }
        Ok(())
    })?;
    let (result,reviewed)=if resume_only {
        let saved=app.db.read_preparation_context(&run).await?;
        let job=super::row(&saved,"jobs",&run)?;
        let first=job["preparationStages"]["first"].clone();
        if first["status"]!="completed" {return Err(super::conflict("Preparation resume has no completed original first result"));}
        let grouped=job["preparationStages"]["groupAdmission"].is_array();
        drop(saved);
        if grouped {
            app.change_preparation_admission(&run, |d| {
                let first=super::row(d,"jobs",&run)?["preparationStages"]["first"]["result"].clone();
                admit_groups(d,&run,&first,false)
            }).await?;
        }
        if first["reviewRequired"]==true {
            (super::preparation_review::chunks::run(&app,&run,preflight).await?,true)
        }else{(first["result"].clone(),false)}
    }else{
    let request=scheduled.request.expect("checked above");
    let lifecycle=first_lifecycle.as_ref().ok_or_else(||super::conflict("First-pass lifecycle admission missing"))?;
    let first=super::preparation_review::dispatch_first_admitted(&app,&run,request.clone(),lifecycle,|d,run|{
        preflight(d,run)?;
        if keys.is_some()&&super::preparation_workers::keys(d,super::row(d,"jobs",run)?)!=keys {
            return Err(super::conflict("Preparation family ownership changed before first-pass admission"));
        }
        Ok(())
    }).await?;
    let review = app
        .change_preparation_first(&run, |d| {
            super::preparation_review::settle_first(d, &run, &request, &first, &super::now())
        })
        .await?;
    if review.is_some() {
        app.change_preparation_admission(&run, |d| admit_groups(d,&run,&first,false)).await?;
    }
    let (result, reviewed) = match review {
        None => (first, false),
        Some(_) => (super::preparation_review::chunks::run(&app,&run,preflight).await?,true),
    };
    (result,reviewed)
    };
    // Settle the original held groups before a repair creates proposals and
    // advances item revisions. Never readmit the original result over a child.
    let outcome=app.change_preparation_admission(&run, |d|
        admit_result(d,&run,&result,reviewed,scheduled.held,scheduled.selected)).await?;
    let repaired=match super::answering_repair_plan::run_pending(&app,&run).await {
        Ok(result)=>result,
        Err(error)=>{
            app.change(|d|super::answering_repair_plan::record_work_failure(d,&run,&error.1)).await?;
            return Err(error);
        },
    };
    if repaired.is_some() {
        app.change(|d| {
            let saved=super::row(d,"jobs",&run)?["prepareOutcome"].clone();
            super::answering_repair_plan::merge_outcome(d,&run,saved)
        }).await
    } else { Ok(outcome) }
}

fn admit_result(d:&mut Value,run:&str,result:&Value,reviewed:bool,held:Vec<Value>,selected:Vec<String>)->super::ApiResult<Value>{
        preflight(d, run)?;
        if reviewed {super::preparation_review::chunks::current(d,super::row(d,"jobs",run)?)?;}
        if !super::row(d,"jobs",run)?["preparationStages"]["groupAdmission"].is_array(){
            let admission=super::prepare_bundle::admit_to(d,run,None,result)?;
            if reviewed {super::preparation_review::record_review(d,run,Ok(result),&super::now())?;}
            let outcome=enrich(admission,result,held,selected);
            super::row_mut(d,"jobs",run)?["prepareOutcome"]=outcome.clone();
            return Ok(outcome);
        }
        admit_groups(d,run,result,reviewed)?;
        if reviewed {
            super::preparation_review::record_review(d, run, Ok(result), &super::now())?;
        }
        let job=super::row(d,"jobs",run)?.clone();
        let groups=job["preparationStages"]["groupAdmission"].as_array().ok_or_else(||super::conflict("Preparation groups missing"))?;
        let candidates:Vec<Value>=groups.iter().flat_map(|g|g["admission"]["candidates"].as_array().cloned().unwrap_or_default()).collect();
        let status=if groups.iter().any(|g|g["status"]=="stale"){"stale"}
            else if candidates.iter().any(|c|c["status"]=="review"){"review"}else{"held"};
        let admission=json!({"conversationId":null,"prepareBundleId":job["prepareBundle"]["id"],
            "status":status,"reason":null,"candidates":candidates});
        let mut combined=job["preparationStages"]["first"]["result"].clone();
        if reviewed {
            for assessment in result["assessments"].as_array().into_iter().flatten(){
                if let Some(old)=combined["assessments"].as_array_mut().and_then(|rows|rows.iter_mut().find(|a|a["itemId"]==assessment["itemId"])) {*old=assessment.clone();}
            }
        }
        for group in groups {
            for assessment in group["admission"]["finalAssessments"].as_array().into_iter().flatten(){
                if let Some(old)=combined["assessments"].as_array_mut().and_then(|rows|rows.iter_mut().find(|a|a["itemId"]==assessment["itemId"])) {
                    *old=assessment.clone();
                }
            }
        }
        let mut outcome = enrich(admission, &combined, held, selected);
        outcome["factDependencies"]=super::fact_followup::summary(super::row(d,"jobs",run)?);
        super::row_mut(d, "jobs", run)?["prepareOutcome"] = outcome.clone();
        Ok(outcome)
}

pub(crate) fn group_result(result:&Value,ids:&[Value])->Value{
    let allowed:std::collections::BTreeSet<&str>=ids.iter().filter_map(Value::as_str).collect();
    let mut subset=result.clone();
    for key in ["assessments","proposals"] {subset[key]=json!(result[key].as_array().into_iter().flatten()
        .filter(|v|v["itemId"].as_str().is_some_and(|id|allowed.contains(id))).collect::<Vec<_>>());}
    if result["editorialEvidence"].is_object(){subset["editorialEvidence"]["entries"]=json!(result["editorialEvidence"]["entries"]
        .as_array().into_iter().flatten().filter(|v|v["itemId"].as_str().is_some_and(|id|allowed.contains(id))).collect::<Vec<_>>());}
    if subset["runMetadata"]["promptVersion"]=="communityhero-preparation-v1-single-pass" {
        let dependencies:std::collections::BTreeMap<&str,Vec<&str>>=result["runMetadata"]["decisionDependencies"]["entries"]
            .as_array().into_iter().flatten().filter_map(|entry|entry["itemId"].as_str().map(|id|(id,
                entry["dependsOnItemIds"].as_array().into_iter().flatten().filter_map(Value::as_str).collect())))
            .collect();
        let mut blocked:std::collections::BTreeSet<String>=subset["assessments"].as_array().into_iter().flatten()
            .filter(|row|row["outcome"]=="needs_attention").filter_map(|row|row["itemId"].as_str().map(str::to_owned)).collect();
        let cross:std::collections::BTreeSet<&str>=allowed.iter().copied().filter(|id|dependencies.get(id)
            .is_some_and(|deps|deps.iter().any(|dependency|!allowed.contains(dependency)))).collect();
        blocked.extend(cross.iter().map(|id|(*id).to_owned()));
        loop {
            let next:Vec<String>=allowed.iter().copied().filter(|id|!blocked.contains(*id)&&dependencies.get(id)
                .is_some_and(|deps|deps.iter().any(|dependency|blocked.contains(*dependency)))).map(str::to_owned).collect();
            if next.is_empty(){break}
            blocked.extend(next);
        }
        for assessment in subset["assessments"].as_array_mut().into_iter().flatten(){
            if assessment["outcome"]!="needs_attention"&&assessment["itemId"].as_str().is_some_and(|id|blocked.contains(id)){
                let cross_group=assessment["itemId"].as_str().is_some_and(|id|cross.contains(id));
                assessment["outcome"]=json!("needs_attention");
                assessment["reason"]=json!(if cross_group{
                    "Решение зависит от другого независимого контекста; требуется отдельная проверка оператором."
                }else{
                    "Решение зависит от комментария, остановленного проверкой; требуется проверка оператором."
                });
                assessment["tags"]=json!(["needs_fact"]);
            }
        }
        subset["proposals"]=json!(subset["proposals"].as_array().into_iter().flatten()
            .filter(|proposal|proposal["itemId"].as_str().is_none_or(|id|!blocked.contains(id))).cloned().collect::<Vec<_>>());
        if subset["editorialEvidence"].is_object(){subset["editorialEvidence"]["entries"]=json!(subset["editorialEvidence"]["entries"]
            .as_array().into_iter().flatten().filter(|entry|entry["itemId"].as_str().is_none_or(|id|!blocked.contains(id)))
            .cloned().collect::<Vec<_>>());}
        // The full validated dependency graph remains in the immutable first
        // result. A group-only admission copy cannot honestly project edges
        // to recipients outside that group, so it carries only safe decisions.
        subset["runMetadata"].as_object_mut().unwrap().remove("decisionDependencies");
    }
    if subset["moderationEvidence"].is_object() {
        // A held cross-group decision no longer has an action to authorize.
        // Retain only exact moderation proofs for this admission copy's final
        // proposals; the complete first-stage proof remains immutable.
        let actions:std::collections::BTreeSet<(String,String)>=subset["proposals"].as_array()
            .into_iter().flatten().filter_map(|proposal|Some((
                proposal["itemId"].as_str()?.to_owned(),proposal["kind"].as_str()?.to_owned()))).collect();
        subset["moderationEvidence"]["entries"]=json!(result["moderationEvidence"]["entries"]
            .as_array().into_iter().flatten().filter(|entry|entry["itemId"].as_str()
                .zip(entry["kind"].as_str()).is_some_and(|(id,kind)|actions.contains(&(id.to_owned(),kind.to_owned()))))
            .cloned().collect::<Vec<_>>());
    }
    // A single high pass can research one independent branch while another
    // has no web evidence. Keep the immutable full first result intact, but
    // pass only this group's recipient-scoped source proof to admission.
    if subset["runMetadata"]["promptVersion"]=="communityhero-preparation-v1-single-pass"
        && subset["runMetadata"]["research"].is_object(){
        let research=&mut subset["runMetadata"]["research"];
        let sources:Vec<Value>=research["sources"].as_array().into_iter().flatten()
            .filter(|v|v["itemId"].as_str().is_some_and(|id|allowed.contains(id))).cloned().collect();
        research["status"]=json!(if sources.is_empty(){"no_sources"}else{"completed"});
        research["sources"]=json!(sources);
        if let Some(holds)=research["evidenceHolds"].as_array(){
            let scoped:Vec<Value>=holds.iter().filter(|v|v["itemId"].as_str().is_some_and(|id|allowed.contains(id))).cloned().collect();
            if scoped.is_empty(){research.as_object_mut().unwrap().remove("evidenceHolds");}
            else{research["evidenceHolds"]=json!(scoped);}
        }
        if let Some(rejected)=research.get_mut("rejectedSources"){
            let scoped:Vec<Value>=rejected["sources"].as_array().into_iter().flatten()
                .filter(|v|v["itemId"].as_str().is_some_and(|id|allowed.contains(id))).cloned().collect();
            if scoped.is_empty(){research.as_object_mut().unwrap().remove("rejectedSources");}
            else{
                let used:std::collections::BTreeSet<&str>=scoped.iter()
                    .filter_map(|v|v["openedUrlSha256"].as_str()).collect();
                rejected["openedUrlSha256"]=json!(rejected["openedUrlSha256"].as_array().into_iter().flatten()
                    .filter(|v|v.as_str().is_some_and(|hash|used.contains(hash))).cloned().collect::<Vec<_>>());
                rejected["sources"]=json!(scoped);
            }
        }
    }
    subset
}

/// Native completed group partitions can settle only a proven no-candidate
/// member. Missing, duplicate or foreign assessments never release paid scope.
pub(crate) fn terminal_no_candidate_member(job:&Value,id:&str)->bool{
    if job["purpose"]!="engine_prepare"||job["status"]!="completed"{return false;}
    let Some(selected)=job["selectedItemIds"].as_array().filter(|ids|!ids.is_empty())else{return false};
    let Some(groups)=job["preparationStages"]["groupAdmission"].as_array().filter(|groups|!groups.is_empty())else{return false};
    let mut covered=std::collections::BTreeSet::new();let mut terminal=false;
    for group in groups{
        if group["status"]!="admitted"{return false;}
        let Some(ids)=group["itemIds"].as_array().filter(|ids|!ids.is_empty())else{return false};
        let Some(assessments)=group["admission"]["finalAssessments"].as_array()else{return false};
        let Some(candidates)=group["admission"]["candidates"].as_array()else{return false};
        if assessments.len()!=ids.len()||assessments.iter().any(|a|!ids.contains(&a["itemId"]))
            ||candidates.iter().any(|candidate|!ids.contains(&candidate["itemId"])){return false;}
        for member in ids{
            let Some(member_id)=member.as_str().filter(|s|!s.is_empty())else{return false};
            if !selected.contains(member)||!covered.insert(member_id.to_owned()){return false;}
            let matches=assessments.iter().filter(|a|a["itemId"]==*member).collect::<Vec<_>>();
            if matches.len()!=1{return false;}
            if member_id==id{terminal=matches[0]["outcome"]=="needs_attention"&&!candidates.iter().any(|c|c["itemId"]==*member);}
        }
    }
    covered.len()==selected.len()&&selected.iter().all(|member|member.as_str().is_some_and(|member|covered.contains(member)))&&terminal
}

fn admit_groups(d:&mut Value,run:&str,result:&Value,reviewed:bool)->super::ApiResult<()> {
    let job=super::row(d,"jobs",run)?.clone();
    let review_ids:std::collections::BTreeSet<String>=super::preparation_review::plan_review(
        &job["prepareBundle"]["request"],&job["preparationStages"]["first"]["result"]).map_err(super::bad)?
        .and_then(|v|v["items"].as_array().cloned()).unwrap_or_default().iter()
        .filter_map(|v|v["id"].as_str().map(str::to_owned)).collect();
    let groups=job["preparationStages"]["groupAdmission"].as_array().ok_or_else(||super::conflict("Preparation groups missing"))?;
    for (index,group) in groups.iter().enumerate(){
        if group["status"]!="pending"{continue;}
        let ids=group["itemIds"].as_array().ok_or_else(||super::bad("Invalid preparation group"))?;
        let needs_review=ids.iter().any(|id|id.as_str().is_some_and(|v|review_ids.contains(v)));
        if needs_review!=reviewed{continue;}
        let subset=group_result(result,ids);
        let mut admission=super::prepare_bundle::admit_group(d,run,&subset,group)?;
        admission["finalAssessments"]=subset["assessments"].clone();
        let status=if admission["status"]=="stale"{"stale"}else{"admitted"};
        let saved=&mut super::row_mut(d,"jobs",run)?["preparationStages"]["groupAdmission"][index];
        saved["status"]=json!(status);saved["admission"]=admission;
    }
    Ok(())
}

/// Same admission contract for local recovery; its caller owns one transaction.
pub(crate) fn admit_completed_review(d:&mut Value,run:&str,result:&Value)->super::ApiResult<Value>{
    let job=super::row(d,"jobs",run)?;
    let held=job["held"].as_array().cloned().unwrap_or_default();
    let selected=job["selectedItemIds"].as_array().into_iter().flatten().filter_map(|id|id.as_str().map(str::to_owned)).collect();
    admit_result(d,run,result,true,held,selected)
}

fn first_capture_recovery_job(d:&Value,run:&str,expected:&str)->super::ApiResult<Value>{
    let job=super::row(d,"jobs",run)?.clone();let bundle=&job["prepareBundle"];let request=&bundle["request"];
    let binding=super::active_binding(d)?;super::bridge_account(&binding)?;
    let sha=|v:&Value|v.as_str().is_some_and(|s|s.len()==64&&s.bytes().all(|b|b.is_ascii_digit()||(b'a'..=b'f').contains(&b)));
    let initial=&job["preparationStages"]["initialAdmission"];let first=&job["preparationStages"]["firstAdmission"];
    let owner=&first["owner"];
    let recipients=match job["purpose"].as_str(){
        Some("engine_prepare")=>job["selectedItemIds"]==bundle["itemIds"],
        Some("auto_prepare")=>job["requestedItemIds"]==bundle["itemIds"],
        Some("auto_revalidate")=>bundle["itemIds"].as_array().is_some_and(|ids|ids.len()==1&&ids[0]==job["refId"]),
        _=>false,
    };
    if expected.len()!=64||!expected.bytes().all(|b|b.is_ascii_digit()||(b'a'..=b'f').contains(&b))
        ||super::list(d,"jobs").iter().filter(|j|j["id"]==run).count()!=1
        ||job["kind"]!="assistant"||!recipients
        ||!matches!(job["status"].as_str(),Some("failed"|"interrupted"|"completed"))
        ||bundle["version"]!=1||!request.is_object()||bundle["digest"]!=expected
        ||format!("{:x}",Sha256::digest(request.to_string().as_bytes()))!=expected
        ||request["account"]!=d["account"]||request["connectorBinding"]!=binding.to_json()
        ||bundle["itemIds"].as_array().is_none_or(|ids|ids.is_empty()||ids.len()>100||ids.iter().any(|id|id.as_str().is_none_or(str::is_empty)))
        ||initial.as_object().is_none_or(|o|o.len()!=5)||initial["version"]!=1||initial["status"]!="scheduled"
        ||first.as_object().is_none_or(|o|o.len()!=5)||first["version"]!=1||first["status"]!="reserved"
        ||initial["requestSha256"]!=expected||first["requestSha256"]!=expected
        ||initial["owner"]!=*owner||owner.as_object().is_none_or(|o|o.len()!=4)
        ||owner["account"]!=d["account"]||owner["runtimeId"].as_str().is_none_or(str::is_empty)
        ||!sha(&owner["releaseSha256"])||owner["epoch"].as_u64().is_none_or(|v|v==0)
        ||initial["admittedAt"].as_str().is_none()||first["reservedAt"].as_str().is_none(){
        return Err(super::conflict("Original first capture recovery ownership changed"));
    }
    Ok(job)
}
fn first_capture_replay(job:&Value)->super::ApiResult<Option<Value>>{
    let first=&job["preparationStages"]["first"];
    if first.is_null(){
        if job["status"]=="completed"||!job["prepareOutcome"].is_null(){return Err(super::conflict("Original first capture recovery state changed"));}
        return Ok(None);
    }
    if first["status"]!="completed"{return Err(super::conflict("Original first capture recovery state changed"));}
    if job["prepareOutcome"].is_object(){return Ok(Some(job["prepareOutcome"].clone()));}
    if job["status"]=="interrupted"&&first["reviewRequired"]==true&&job["prepareOutcome"].is_null(){
        return Ok(Some(json!({"jobId":job["id"],"status":"interrupted","firstRecovered":true,"reviewResumeRequired":true,"dispatchAuthorized":false})));
    }
    Err(super::conflict("Original first capture recovery state changed"))
}
fn merge_first_capture_replay(d:&mut Value,run:&str,expected:&str)->super::ApiResult<Value>{
    let job=first_capture_recovery_job(d,run,expected)?;
    let outcome=first_capture_replay(&job)?.ok_or_else(||super::conflict("Original first capture recovery state changed"))?;
    if !job["prepareOutcome"].is_object(){return Ok(outcome);}
    // Original group admission is already durable. A completed repair may have
    // changed recipient revisions, so only merge its saved outcome here.
    let mut outcome=super::answering_repair_plan::merge_outcome(d,run,outcome)?;
    outcome["dispatchAuthorized"]=json!(false);
    let saved=super::row_mut(d,"jobs",run)?;saved["prepareOutcome"]=outcome.clone();saved["result"]=outcome.clone();Ok(outcome)
}
fn restore_first_capture_item_status(d:&mut Value,job:&Value)->super::ApiResult<()> {
    let (key,interrupted)=match job["purpose"].as_str(){
        Some("auto_prepare")=>("autoPreparation","error"),
        Some("auto_revalidate")=>("autoRevalidation","held"),
        _=>return Ok(()),
    };
    for id in job["prepareBundle"]["itemIds"].as_array().unwrap(){
        let id=id.as_str().unwrap();let original=super::row(d,"items",id)?.clone();
        let native_recovery=key=="autoPreparation"&&original[key]["jobId"]==job["id"]
            &&original[key]["status"]==interrupted&&original[key]["requiresReview"]==true
            &&original[key]["reasonCode"]=="paid_attempt_recovery_required";
        let clear_recovery=if native_recovery {
            let mut candidate=original.clone();candidate[key]["requiresReview"]=json!(false);
            let saved=job["prepareBundle"]["request"]["items"].as_array().into_iter().flatten().find(|item|item["id"]==id);
            saved.is_some_and(|saved|saved["revision"]==original["revision"])
                &&super::prepare_bundle::current(d,&job["prepareBundle"]).is_ok()
                &&super::preparation_unit::current_request(d,&job["prepareBundle"]["request"],&super::now()).is_ok()
                &&super::preparation_materials::require_request(d,&job["prepareBundle"]["request"]).is_ok()
                &&super::preparation_reservations::assert_available(d,&[id.to_owned()],job["id"].as_str()).is_ok()
                &&super::auto_prepare::eligible(d,&candidate,chrono::Utc::now().timestamp())
        }else{false};
        let item=super::row_mut(d,"items",id)?;
        // Restore only the spinner that native restart recovery projected for
        // this exact job. All source/manual/operation eligibility is still
        // decided by the original automatic settlement reducer below.
        if item[key]["jobId"]==job["id"]&&item[key]["status"]==interrupted {
            item[key]["status"]=json!("running");
            if clear_recovery {
                item[key]["requiresReview"]=json!(false);
                item[key].as_object_mut().unwrap().remove("reasonCode");
            }
        }
    }
    Ok(())
}
/// Existing exact-job recovery only: no new reservation, source acquisition,
/// worker slot, bridge or model call. Historical request/owner pins remain intact.
pub(crate) async fn recover_first_capture(app:&super::App,run:&str,expected_request_digest:&str)->super::ApiResult<Value>{
    let snapshot=app.db.read_preparation_context(run).await?;
    super::runtime_lifecycle::current_owner(&snapshot,&app.lifecycle_owner)?;
    let job=first_capture_recovery_job(&snapshot,run,expected_request_digest)?;
    if first_capture_replay(&job)?.is_some(){
        drop(snapshot);return app.change(|d|{
            super::runtime_lifecycle::current_owner(d,&app.lifecycle_owner)?;
            merge_first_capture_replay(d,run,expected_request_digest)
        }).await;
    }
    let request=job["prepareBundle"]["request"].clone();drop(snapshot);
    let first=super::preparation_review::recover_first_if_retained(app,run,&request).await?
        .ok_or_else(||super::conflict("Original paid first capture is unavailable; reservation remains held and generation is not replayed"))?;
    app.change(|d|{
        super::runtime_lifecycle::current_owner(d,&app.lifecycle_owner)?;
        let job=first_capture_recovery_job(d,run,expected_request_digest)?;
        if first_capture_replay(&job)?.is_some(){return merge_first_capture_replay(d,run,expected_request_digest);}
        if super::preparation_workers::pending_conflict(d,&job,true){return Err(super::conflict("Another assistant job owns this preparation scope"));}
        // Status is temporarily runnable only inside this atomic native reducer;
        // no runnable state, new firstAdmission or task is committed.
        super::row_mut(d,"jobs",run)?["status"]=json!("running");
        let review=super::preparation_review::settle_first(d,run,&request,&first,&super::now())?;
        restore_first_capture_item_status(d,&job)?;
        let outcome=if review.is_some(){
            if job["preparationStages"]["groupAdmission"].is_array(){
                if job["purpose"]=="engine_prepare"{admit_groups(d,run,&first,false)?;}
                else if job["purpose"]=="auto_prepare"{
                    super::auto_prepare::settle_first_groups(d,run,&first,chrono::Utc::now().timestamp())?;
                }
            }
            json!({"jobId":run,"status":"interrupted","firstRecovered":true,"reviewResumeRequired":true,"dispatchAuthorized":false})
        }else{
            let outcome=if job["purpose"]=="engine_prepare" {
                let held=job["held"].as_array().cloned().unwrap_or_default();
                let selected=job["selectedItemIds"].as_array().into_iter().flatten().filter_map(|id|id.as_str().map(str::to_owned)).collect();
                admit_result(d,run,&first,false,held,selected)?
            }else{super::auto_prepare::complete(d,run,&first,chrono::Utc::now().timestamp())?};
            let mut outcome=super::answering_repair_plan::merge_outcome(d,run,outcome)?;
            outcome["dispatchAuthorized"]=json!(false);
            super::row_mut(d,"jobs",run)?["prepareOutcome"]=outcome.clone();outcome
        };
        let saved=super::row_mut(d,"jobs",run)?;
        if review.is_some(){saved["status"]=json!("interrupted");}
        else{saved["status"]=json!("completed");saved["result"]=outcome.clone();saved["finishedAt"]=json!(super::now());saved["error"]=Value::Null;}
        Ok(outcome)
    }).await
}

fn schedule_admitted(d:&mut Value,input:Input,token:&super::runtime_lifecycle::OwnerToken)->super::ApiResult<Scheduled> {
    super::runtime_lifecycle::require_admission(d,token,super::runtime_lifecycle::AdmissionClass::Preparation)?;
    let scheduled=schedule(d,input)?;
    super::preparation_review::record_initial_admission(d,token,&scheduled.job_id,&super::now())?;
    Ok(scheduled)
}

pub async fn prepare(
    State(app): State<super::App>,
    axum::Extension(actor):axum::Extension<super::operator_auth::Actor>,
    Json(body): Json<Value>,
) -> super::ApiResult<Json<Value>> {
    let input = parse(&body)?;
    if let Some(result)=super::local_admission::replay_committed(&app,"prepare",&body,&actor).await? {return Ok(Json(result));}
    let lifecycle=app.lifecycle_admission_token(super::runtime_lifecycle::AdmissionClass::Preparation).await?;
    super::media_fullframes::refresh(&app).await?;
    // No job, attempt or receipt is persisted until the exact JS projection
    // and its image envelope fit. The adapter never runs inside the writer.
    let snapshot=app.db.read_preparation_schedule().await?;
    let (_,preview)=super::runtime_lifecycle_app::Capture::preview(&app,&lifecycle,snapshot,|d|schedule(d,input.clone()))?;
    capacity::require(&app,preview.request.as_ref()).await?;
    let (result,scheduled)=app.change_preparation_schedule(|d| {
        let request=super::local_admission::request(d,"prepare",&body,&actor)?;
        if let Some(ref request)=request {
            if let Some(result)=super::local_admission::replay(d,request,&actor)? {return Ok((result,None));}
        }
        let scheduled=schedule_admitted(d,input,&lifecycle)?;
        if let Some(mode)=body.get("workflowMode") {super::row_mut(d,"jobs",&scheduled.job_id)?["workflowMode"]=mode.clone();}
        capacity::same_capture(preview.request.as_ref(),scheduled.request.as_ref())?;
        let mut result=json!({"jobId":scheduled.job_id});
        if let Some(mode)=body.get("workflowMode"){result["workflowMode"]=mode.clone();}
        if let Some(reservation)=super::row(d,"jobs",&scheduled.job_id)?.get("scopeReservation") {
            result["scopeReservation"]=json!({"version":1,"ownerJobId":reservation["ownerJobId"],"keysDigest":reservation["keysDigest"]});
        }
        if let Some(ref request)=request {super::local_admission::commit(d,request,&mut result)?;}
        Ok((result,Some(scheduled)))
    }).await?;
    // A receipt replay always returns the original job, including interrupted
    // jobs after restart. It never schedules or starts an assistant again.
    let Some(scheduled)=scheduled else {return Ok(Json(result))};
    spawn_preparation(&app,scheduled);
    Ok(Json(result))
}

pub(crate) fn spawn_preparation(app:&super::App,scheduled:Scheduled){
    let job_id=scheduled.job_id.clone();let worker=app.clone();
    app.spawn(job_id,run(worker,scheduled,false));
}

pub(crate) fn spawn_review_resume(app:&super::App,job_id:String){
    let worker=app.clone();
    app.spawn(job_id.clone(),async move {
        let d=worker.db.read_preparation_context(&job_id).await?;let job=super::row(&d,"jobs",&job_id)?;
        let scheduled=Scheduled{job_id:job_id.clone(),request:Some(job["prepareBundle"]["request"].clone()),
            selected:job["selectedItemIds"].as_array().into_iter().flatten().filter_map(|v|v.as_str().map(str::to_owned)).collect(),
            held:job["held"].as_array().cloned().unwrap_or_default()};
        run(worker,scheduled,true).await
    });
}

/// Explicitly admitted pending-work resume AFTER original final admission.
/// Child admission may advance recipient revisions; never readmit the original.
pub(crate) fn spawn_pending_work_resume(app:&super::App,job_id:String){
    let worker=app.clone();
    app.spawn(job_id.clone(),async move {
        let before=worker.db.read_preparation_context(&job_id).await?;
        let job=super::row(&before,"jobs",&job_id)?;
        if job["kind"]!="assistant"||job["status"]!="running"||!matches!(job["purpose"].as_str(),Some("engine_prepare"|"auto_prepare"|"auto_revalidate"))||!job["prepareOutcome"].is_object()
            ||job["preparationStages"]["first"]["status"]!="completed"{
            return Err(super::conflict("Pending repair resume requires original final admission"));
        }
        let original=job["prepareOutcome"].clone();let purpose=job["purpose"].clone();let keys=super::preparation_workers::keys(&before,job);drop(before);
        let lease=worker.preparation_workers.acquire(keys.clone()).await;
        let _assistant_guard=if lease.slot()==0{Some(worker.assistant_gate.clone().lock_owned().await)}else{None};
        lease.scope(async {
            let fresh=worker.db.read_preparation_context(&job_id).await?;let job=super::row(&fresh,"jobs",&job_id)?;
            if job["kind"]!="assistant"||job["status"]!="running"||job["purpose"]!=purpose||job["prepareOutcome"]!=original
                ||keys.is_some()&&super::preparation_workers::keys(&fresh,job)!=keys{
                return Err(super::conflict("Pending repair ownership changed while waiting"));
            }
            drop(fresh);
            super::answering_repair_plan::run_pending(&worker,&job_id).await?;
            worker.change(|d|{
                let job=super::row(d,"jobs",&job_id)?;
                if job["kind"]!="assistant"||job["status"]!="running"||job["purpose"]!=purpose||job["prepareOutcome"]!=original{
                    return Err(super::conflict("Pending repair original outcome changed"));
                }
                super::answering_repair_plan::merge_outcome(d,&job_id,original)
            }).await
        }).await
    });
}

#[cfg(test)]
#[path = "preparation_admission_tests.rs"]
mod lifecycle_admission_tests;

#[cfg(test)]
#[path = "preparation_first_capture_recovery_tests.rs"]
mod first_capture_recovery_tests;

#[cfg(test)]
#[path = "preparation_unpaid_material_refresh_tests.rs"]
mod unpaid_material_refresh_tests;

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) fn complete_first_fixture(d:&mut Value,run:&str,result:&Value)->Value{
        let mut first=single_pass_result(result.clone());
        let job=super::super::row(d,"jobs",run).unwrap().clone();
        crate::model_material_receipt::fixture_result(d,run,&job["prepareBundle"]["request"],&mut first).unwrap();
        super::super::preparation_review::settle_first(d,run,&job["prepareBundle"]["request"],&first,&super::super::now()).unwrap();
        let held=job["held"].as_array().cloned().unwrap_or_default();
        let selected=job["selectedItemIds"].as_array().unwrap().iter().map(|v|v.as_str().unwrap().to_owned()).collect();
        let outcome=admit_result(d,run,&first,false,held,selected).unwrap();
        super::super::row_mut(d,"jobs",run).unwrap()["status"]=json!("completed");
        outcome
    }

    #[test]
    fn pending_scope_continues_while_an_independent_first_group_is_dispatching() {
        let mut d=family_fixture();
        let mut scheduled=schedule(&mut d,Input{item_ids:vec!["ready".into(),"media".into()],instruction:None}).unwrap();
        two_pass_capture(&mut d,&mut scheduled);
        let mut first=family_result(json!({"text":"Original two-pass source evidence","sources":[],
            "assessments":[{"itemId":"ready","outcome":"reply","reason":"Source-independent greeting","tags":["feedback"]},
                {"itemId":"media","outcome":"needs_attention","reason":"Exact fact remains unavailable","tags":["needs_fact"]}],
            "proposals":[{"itemId":"ready","kind":"reply_and_close","text":"Thanks for your feedback"}]}));
        crate::model_material_receipt::fixture_result(&mut d,&scheduled.job_id,scheduled.request.as_ref().unwrap(),&mut first).unwrap();
        assert!(super::super::preparation_review::settle_first(&mut d,&scheduled.job_id,scheduled.request.as_ref().unwrap(),&first,&super::super::now()).unwrap().is_some());
        admit_groups(&mut d,&scheduled.job_id,&first,false).unwrap();
        let ready=super::super::row(&d,"items","ready").unwrap().clone();
        super::super::list_mut(&mut d,"operations").push(json!({"id":"sending-first","itemId":"ready","status":"dispatching","target":ready}));
        assert!(preflight(&d,&scheduled.job_id).is_ok());
        let pending=super::super::row(&d,"items","media").unwrap().clone();
        super::super::list_mut(&mut d,"operations").push(json!({"id":"historic-alias","itemId":"removed-local-alias","status":"unknown","target":pending}));
        assert!(preflight(&d,&scheduled.job_id).is_err(),"pending canonical alias UNKNOWN remains quarantined");
    }

    pub(crate) fn fixture(video: bool) -> Value {
        let mut d = super::super::empty();
        super::super::accounts::initialize(&mut d, super::super::accounts::Profile::LikeAvto)
            .unwrap();
        d["items"] = json!([
            {"id":"ready","itemId":"c-ready","objectId":"o-ready","platform":"VK","postKey":"ready-post","conversationKey":"ready-thread","branchId":"ready-branch","postId":"ready-post","revision":1,"draft":"","workflow":"attention","providerStatus":"new"},
            {"id":"media","itemId":"c-media","objectId":"o-media","platform":"VK","postKey":"media-post","conversationKey":"media-thread","branchId":"media-branch","postId":"media-post","revision":1,"draft":"","workflow":"attention","providerStatus":"new"}
        ]);
        d["branches"] = json!([
            {"id":"ready-branch","postId":"ready-post","messages":[{"id":"c-ready","text":"Спасибо"}],"contextComplete":true},
            {"id":"media-branch","postId":"media-post","messages":[{"id":"c-media","text":"Что в видео?"}],"contextComplete":true}
        ]);
        d["posts"] = json!([
            {"id":"ready-post","postKey":"ready-post","objectId":"o-ready","platform":"VK","text":"Обычный пост"},
            {"id":"media-post","postKey":"media-post","objectId":"o-media","platform":"VK","text":"Видео","attachments":if video {json!([{"type":"video"}])} else {json!([])}}
        ]);
        d
    }

    pub(crate) fn baw_fixture(video:bool)->Value {
        let mut d=crate::empty();crate::accounts::initialize(&mut d,crate::accounts::Profile::BawRussia).unwrap();
        d["items"]=json!([
            {"id":"ready","itemId":"c-ready","objectId":"12182","platform":"VK","postKey":"ready-post","conversationKey":"ready-thread","branchId":"ready-branch","postId":"ready-post","revision":1,"draft":"","workflow":"attention","providerStatus":"new"},
            {"id":"media","itemId":"c-media","objectId":"12182","platform":"VK","postKey":"media-post","conversationKey":"media-thread","branchId":"media-branch","postId":"media-post","revision":1,"draft":"","workflow":"attention","providerStatus":"new"}]);
        d["branches"]=json!([
            {"id":"ready-branch","postId":"ready-post","messages":[{"id":"c-ready","text":"Thanks"}],"contextComplete":true},
            {"id":"media-branch","postId":"media-post","messages":[{"id":"c-media","text":"What about the source?"}],"contextComplete":true}]);
        d["posts"]=json!([
            {"id":"ready-post","postKey":"ready-post","objectId":"12182","platform":"VK","text":"Exact BAW post","attachments":[]},
            {"id":"media-post","postKey":"media-post","objectId":"12182","platform":"VK","text":"Separate BAW source","attachments":if video{json!([{"type":"video"}])}else{json!([])}}]);d
    }
    // Distinct BAW copies with actual complete-speech evidence. Per-branch
    // settlement cannot rely on accidental grouping of unrelated text posts.
    fn family_fixture()->Value {
        let mut d=baw_fixture(false);let at=super::super::now();
        d["posts"][0]["sourceUrl"]=json!("https://www.youtube.com/watch?v=AbCdEf123_-");
        d["posts"][1]["sourceUrl"]=json!("https://vk.com/video-1_1");
        for post in d["posts"].as_array_mut().unwrap(){post["attachments"]=json!([{"type":"video"}]);}
        let source=crate::media_fullframes::source_version(&d["posts"][0],d["account"].as_str().unwrap());
        let target=crate::media_fullframes::source_version(&d["posts"][1],d["account"].as_str().unwrap());
        d["materials"]=json!([{"id":"family-speech","account":d["account"],"kind":"transcript","postKey":"ready-post","sourceUrl":d["posts"][0]["sourceUrl"],
            "text":"Complete synthetic source speech","transcription":{"partial":false,"audioStatus":"transcribed","coverage":"full_audio","sourceVersion":source,"mediaDurationSeconds":60.0,"audioDurationSeconds":60.0}}]);
        crate::knowledge::sync_catalog(&mut d,&at).unwrap();let head=d["knowledge_versions"][0].clone();
        d["settings"]["mediaAudioEquivalences"]=json!({"media-post":{"schemaVersion":1,"status":"active","revision":1,"account":d["account"],
            "connectorBinding":crate::active_binding(&d).unwrap().to_json(),"targetPostId":"media-post","targetPostKey":"media-post","targetSourceVersion":target,
            "sourcePostId":"ready-post","sourcePostKey":"ready-post","sourceVersion":source,"transcript":{"entryId":head["entryId"],"versionId":head["id"],"hash":head["hash"]}}});d
    }

    fn routine_result() -> Value {
        json!({
            "text":"Draft ready",
            "sources":[],
            "assessments":[{"itemId":"ready","outcome":"reply","reason":"Friendly feedback","tags":["feedback"]}],
            "proposals":[{"itemId":"ready","kind":"reply_and_close","text":"Спасибо!"}]
        })
    }

    pub(crate) fn single_pass_result(mut result:Value)->Value {
        if result.get("factDependencies").is_none(){result["factDependencies"]=json!([]);}
        result["runMetadata"]=json!({"schemaVersion":1,"model":crate::codex_model_policy::MODEL,"modelProfile":crate::codex_model_policy::PROFILE,"reasoningEffort":"high",
            "promptVersion":"communityhero-preparation-v1-single-pass","instructionSha256":"a".repeat(64),
            "inputSha256":"b".repeat(64),"cliSha256":crate::codex_model_policy::CLI_SHA256,"elapsedMs":1,
            "completedAt":"2026-09-28T00:00:00Z"});
        result["runMetadata"]["decisionDependencies"]=json!({"version":1,"entries":result["assessments"].as_array().unwrap().iter()
            .map(|a|json!({"itemId":a["itemId"],"dependsOnItemIds":[]})).collect::<Vec<_>>()});
        result["runMetadata"]["researchLimitContract"]=json!("uncapped_evidence_v1");
        result["editorialEvidence"]=json!({"version":1,"contract":crate::editorial_review::CONTRACT,
            "entries":result["proposals"].as_array().unwrap().iter().map(|p|json!({"itemId":p["itemId"],
                "kind":p["kind"],"textSha256":crate::editorial_review::hash_text(p["text"].as_str().unwrap()),
                "decision":"accept","reason":"Exact source and final action checked",
                "checks":{"companyRules":"pass","intent":"pass","factualScope":"pass"}})).collect::<Vec<_>>()});
        result
    }
    pub(crate) fn lifecycle_first_fixture()->Value {single_pass_result(routine_result())}
    fn family_result(result:Value)->Value {
        let mut result=single_pass_result(result);
        result["runMetadata"]["decisionMediaContract"]=json!(crate::decision_media::CONTRACT);
        for entry in result["editorialEvidence"]["entries"].as_array_mut().unwrap(){
            // Synthetic greetings use no audiovisual fact; native capture
            // still supplies and pins complete original source speech.
            entry["mediaDependency"]=json!({"audio":"independent","visual":"independent"});
        }
        result
    }
    fn two_pass_capture(d:&mut Value,scheduled:&mut Scheduled) {
        let run=&scheduled.job_id;
        scheduled.request.as_mut().unwrap().as_object_mut().unwrap().remove("preparationMode");
        scheduled.request.as_mut().unwrap().as_object_mut().unwrap().remove("factDependencyContract");
        let job=super::super::row_mut(d,"jobs",run).unwrap();
        job["prepareBundle"]["request"]=scheduled.request.as_ref().unwrap().clone();rehash(&mut job["prepareBundle"]);
        job.as_object_mut().unwrap().remove("scopeReservation");
        let reservation=crate::preparation_reservations::capture(d,run).unwrap();
        super::super::row_mut(d,"jobs",run).unwrap()["scopeReservation"]=reservation;
        let scope=crate::preparation_workers::capture(d,super::super::row(d,"jobs",run).unwrap()).unwrap();
        super::super::row_mut(d,"jobs",run).unwrap()["preparationWorkerScope"]=scope;
    }
    // Historical two-pass fixtures remain a recovery contract. Remove all
    // new server-owned selectors and update their immutable bundle digest.
    fn legacy_mode(d:&mut Value,scheduled:&mut Scheduled) {
        super::super::row_mut(d,"jobs",&scheduled.job_id).unwrap().as_object_mut().unwrap().remove("scopeReservation");
        let request=scheduled.request.as_mut().unwrap().as_object_mut().unwrap();
        for field in ["preparationMode","responseContract","modelContextContract","researchPolicy","researchLimitContract","recoveryEvidenceContract","factDependencyContract","visualNeedContract","visualSelection",
            "strictGroupContract","strictGroup","mandatoryMaterialContract","postContextBundle","materialReadiness"] {request.remove(field);}
        let bundle=&mut super::super::row_mut(d,"jobs",&scheduled.job_id).unwrap()["prepareBundle"];
        let request=bundle["request"].as_object_mut().unwrap();
        for field in ["preparationMode","responseContract","modelContextContract","researchPolicy","researchLimitContract","recoveryEvidenceContract","factDependencyContract","visualNeedContract","visualSelection",
            "strictGroupContract","strictGroup","mandatoryMaterialContract","postContextBundle","materialReadiness"] {request.remove(field);}
        rehash(bundle);
    }

    #[test]
    fn compact_response_contract_is_server_captured_and_old_absent_bundle_stays_valid(){
        let d=fixture(false);
        let bundle=build_request(&d,&[json!("ready")],None).unwrap();
        assert_eq!(bundle["request"]["preparationMode"],"single_pass_v1");
        assert_eq!(bundle["request"]["responseContract"],"compact_decisions_v1");
        assert_eq!(bundle["digest"],json!(format!("{:x}",Sha256::digest(bundle["request"].to_string().as_bytes()))));
        let mut old=bundle.clone();
        for field in ["responseContract","modelContextContract","researchPolicy","researchLimitContract","recoveryEvidenceContract","visualNeedContract","visualSelection"] {old["request"].as_object_mut().unwrap().remove(field);}
        rehash(&mut old);
        assert_ne!(old["digest"],bundle["digest"],"selector is part of the captured request");
        assert!(super::super::prepare_bundle::current(&d,&old).is_ok(),"older paid requests absent the selector retain their source binding");
        let mut forged=bundle.clone();forged["request"]["responseContract"]=json!("other");
        assert!(super::super::prepare_bundle::current(&d,&forged).is_err(),"changing a captured selector without rehash cannot retarget a job");
    }

    #[test]
    fn recovery_evidence_survives_immutable_first_settlement_without_proposals(){
        let mut d=baw_fixture(false);
        let scheduled=schedule(&mut d,Input{item_ids:vec!["ready".into()],instruction:None}).unwrap();
        let mut result=single_pass_result(json!({"text":"Held","sources":[],"proposals":[],
            "assessments":[{"itemId":"ready","outcome":"needs_attention","reason":"Source not observed","tags":["needs_fact"]}]}));
        let recovery=json!({"version":1,"contract":"held_candidates_v1","inputSha256":"b".repeat(64),"admitted":false,
            "items":[{"itemId":"ready","kind":"reply_and_close","text":"Original paid candidate",
                "textSha256":crate::editorial_review::hash_text("Original paid candidate"),"reason":"Original reasoning","holdReason":"Source not observed",
                "editorial":{"decision":"accept","reason":"Original editorial","checks":{"intent":"pass","companyRules":"pass","factualScope":"pass"}},
                "sources":[{"itemId":"ready","url":"https://example.com/source","title":"Source","claim":"Candidate fact","trust":"source_only"}],"dependsOnItemIds":[]}],
            "activities":[],"omittedItemsCount":0,"omittedActivitiesCount":0});
        result["runMetadata"]["quarantinedRecovery"]=recovery.clone();
        crate::model_material_receipt::fixture_result(&mut d,&scheduled.job_id,scheduled.request.as_ref().unwrap(),&mut result).unwrap();
        super::super::preparation_review::settle_first(&mut d,&scheduled.job_id,scheduled.request.as_ref().unwrap(),&result,&super::super::now()).unwrap();
        let first=&super::super::row(&d,"jobs",&scheduled.job_id).unwrap()["preparationStages"]["first"]["result"];
        assert_eq!(first["runMetadata"]["quarantinedRecovery"],recovery);
        assert_eq!(first["proposals"],json!([]));
        for key in ["proposals","approvals","operations"] {assert!(d[key].as_array().unwrap().is_empty());}
    }
    #[test]
    fn new_context_selectors_are_captured_and_paid_r9_request_remains_unchanged(){
        let mut d=fixture(false);
        let scheduled=schedule(&mut d,Input{item_ids:vec!["ready".into()],instruction:None}).unwrap();
        let captured=scheduled.request.as_ref().unwrap();
        assert_eq!(captured["modelContextContract"],"shared_moderation_v1");
        assert_eq!(captured["researchPolicy"],"context_sufficient_v1");
        assert_eq!(captured["researchLimitContract"],"uncapped_evidence_v1");
        assert_eq!(captured["recoveryEvidenceContract"],"held_candidates_v1");
        let mut old=super::super::row(&d,"jobs",&scheduled.job_id).unwrap()["prepareBundle"].clone();
        for field in ["modelContextContract","researchPolicy","researchLimitContract","recoveryEvidenceContract","visualNeedContract","visualSelection"] {old["request"].as_object_mut().unwrap().remove(field);}
        rehash(&mut old);
        let saved=old.clone();
        let old_bytes=old["request"].to_string();
        // The estimate counts selectors before admission; no final-JS size
        // equivalence is implied by this scalar accounting assertion.
        assert_eq!(model_request_bytes(captured)-model_request_bytes(&old["request"]),
            captured.to_string().len()-old_bytes.len());
        let legacy=super::super::row_mut(&mut d,"jobs",&scheduled.job_id).unwrap();
        legacy["prepareBundle"]=old;
        // This fixture recreates a request captured before scope reservations.
        legacy.as_object_mut().unwrap().remove("scopeReservation");
        assert!(super::super::prepare_bundle::current(&d,&saved).is_ok(),"historical source evidence remains readable");
        assert!(preflight(&d,&scheduled.job_id).is_err(),"legacy selectors do not authorize a new first paid call");
        assert_eq!(super::super::row(&d,"jobs",&scheduled.job_id).unwrap()["prepareBundle"],saved,
            "reading a paid R9 request must not upgrade its selectors or digest");
        for field in ["modelContextContract","researchPolicy","researchLimitContract","recoveryEvidenceContract","visualNeedContract","visualSelection"] {
            let mut forged=saved.clone();forged["request"][field]=captured[field].clone();
            assert!(super::super::prepare_bundle::current(&d,&forged).is_err(),"{field} is bound by the saved digest");
            assert!(parse(&json!({"itemIds":["ready"],field:captured[field]})).is_err(),"selectors are server-owned");
        }
    }

    #[test]
    #[ignore = "offline canonical capacity fixture export; requires explicit output directory"]
    fn export_canonical_model_capacity_fixtures(){
        let output=std::path::PathBuf::from(std::env::var("COMMUNITYHERO_CAPACITY_FIXTURE_DIR").expect("explicit synthetic fixture directory"));
        assert!(output.is_absolute()&&output.is_dir());
        for (name,count,common,text_len) in [("rules300",300,200,8),("rules47",47,20,2000)] {
            let mut d=fixture(false);d["items"]=json!([]);d["posts"]=json!([]);d["branches"]=json!([]);
            for n in 0..100 {
                let post=format!("post-{n}");let branch=format!("branch-{n}");let id=format!("item-{n}");
                d["posts"].as_array_mut().unwrap().push(json!({"id":post,"postKey":post,"platform":"VK","text":"A supplied post","attachments":[]}));
                d["branches"].as_array_mut().unwrap().push(json!({"id":branch,"postId":post,"contextComplete":true,
                    "messages":[{"id":format!("comment-{n}"),"text":"A supplied comment"}]}));
                d["items"].as_array_mut().unwrap().push(json!({"id":id,"itemId":format!("comment-{n}"),"objectId":"object",
                    "postId":post,"postKey":post,"branchId":branch,"platform":"VK","text":"A supplied comment",
                    "revision":1,"workflow":"attention","draft":"","providerStatus":"new"}));
            }
            crate::knowledge::sync_catalog(&mut d,"2026-09-29T00:00:00Z").unwrap();
            for n in 0..count {
                let mut instruction=json!({"requestId":format!("capacity-{name}-{n}"),"title":format!("Rule {n}"),"text":"x".repeat(text_len)});
                if n>=common {instruction["postKey"]=json!(format!("post-{}",n-common));}
                crate::knowledge::save_instruction(&mut d,&instruction,"2026-09-29T00:00:00Z").unwrap();
            }
            let ids=(0..100).map(|n|json!(format!("item-{n}"))).collect::<Vec<_>>();
            let bundle=build_request(&d,&ids,None).unwrap();
            let request=&bundle["request"];
            assert_eq!(request["materials"].as_array().unwrap().len(),count);
            assert_eq!(request["moderationContext"]["ruleRefs"].as_array().unwrap().len(),count);
            assert!(crate::prepare_bundle::current(&d,&bundle).is_ok());
            let bytes=model_request_bytes(request);assert!(bytes<=MAX_REQUEST_BYTES);
            let path=output.join(format!("{name}.json"));
            let mut file=std::fs::OpenOptions::new().write(true).create_new(true).open(path).unwrap();
            use std::io::Write;file.write_all(request.to_string().as_bytes()).unwrap();
            println!("canonical-capacity name={name} items=100 selectedRules={count} rustBytes={bytes}");
        }
    }

    #[test]
    fn completed_or_uncertain_operation_is_held_before_model_call() {
        for (status, reason) in [
            ("unknown", "operation_outcome_unknown"),
            ("dispatching", "operation_dispatch_in_progress"),
            ("succeeded", "operation_already_succeeded"),
        ] {
            let mut d=fixture(false);
            d["operations"]=json!([{"id":"prior","itemId":"ready","status":status}]);
            let scheduled=schedule(&mut d,Input{item_ids:vec!["ready".into()],instruction:None}).unwrap();
            assert!(scheduled.request.is_none(),"{status} must not reach the model");
            assert!(scheduled.selected.is_empty());
            assert_eq!(json!(scheduled.held),json!([{"itemId":"ready","reason":reason}]));
            assert_eq!(all_held_result(scheduled.held)["status"],"held");
        }
        let mut d=fixture(false);
        d["operations"]=json!([{"id":"prior","itemId":"ready","status":"failed"}]);
        assert!(schedule(&mut d,Input{item_ids:vec!["ready".into()],instruction:None}).unwrap().request.is_some(),
            "a proven failed operation is outside the final group guard");
    }

    #[test]
    fn protected_operation_holds_only_its_recipient_and_unrelated_group_admits() {
        for (status, reason) in [
            ("unknown", "operation_outcome_unknown"),
            ("dispatching", "operation_dispatch_in_progress"),
            ("succeeded", "operation_already_succeeded"),
        ] {
            let mut d=baw_fixture(false);
            d["operations"]=json!([{"id":"prior","itemId":"ready","status":status}]);
            let before=d.clone();
            assert!(schedule(&mut d,Input{item_ids:vec!["ready".into(),"media".into()],instruction:None}).is_err());
            assert_eq!(d,before,"mixed selection cannot gain authority by filtering a held recipient");
            let plan=crate::prepare_plan::build(&d,&json!({"itemIds":["ready","media"]})).unwrap();
            assert_eq!(plan["held"],json!([{"itemId":"ready","reason":reason}]));
            assert_eq!(plan["batches"][0]["itemIds"],json!(["media"]));
            let scheduled=schedule(&mut d,Input{item_ids:vec!["media".into()],instruction:None}).unwrap();
            assert_eq!(scheduled.selected,vec!["media"],"{status}");
            assert!(scheduled.held.is_empty());
            assert_eq!(scheduled.request.as_ref().unwrap()["items"].as_array().unwrap().len(),1);
            assert_eq!(scheduled.request.as_ref().unwrap()["items"][0]["id"],"media");
            let mut first=single_pass_result(json!({"text":"Independent branch","sources":[],
                "assessments":[{"itemId":"media","outcome":"reply","reason":"Current branch supports answer","tags":["feedback"]}],
                "proposals":[{"itemId":"media","kind":"reply_and_close","text":"Спасибо!"}]}));
            let run=&scheduled.job_id;
            crate::model_material_receipt::fixture_result(&mut d,run,scheduled.request.as_ref().unwrap(),&mut first).unwrap();
            assert!(super::super::preparation_review::settle_first(&mut d,run,scheduled.request.as_ref().unwrap(),
                &first,&super::super::now()).unwrap().is_none());
            let outcome=admit_result(&mut d,run,&first,false,scheduled.held,scheduled.selected).unwrap();
            assert_eq!(outcome["held"],json!([]));
            assert_eq!(outcome["preparedItemIds"],json!(["media"]));
            assert_eq!(d["proposals"].as_array().unwrap().len(),1);
            assert_eq!(d["proposals"][0]["itemId"],"media");
        }
    }

    #[test]
    fn routine_branch_settles_before_unrelated_review_and_is_not_replayed() {
        let mut d=family_fixture();
        let mut scheduled=schedule(&mut d,Input{item_ids:vec!["ready".into(),"media".into()],instruction:None}).unwrap();
        legacy_mode(&mut d,&mut scheduled);
        let run=&scheduled.job_id;
        let request=scheduled.request.as_ref().unwrap();
        let first=json!({"text":"First pass","sources":[],"assessments":[
            {"itemId":"ready","outcome":"reply","reason":"Friendly feedback","tags":["feedback"]},
            {"itemId":"media","outcome":"needs_attention","reason":"Needs source check","tags":["needs_fact"]}],
            "proposals":[{"itemId":"ready","kind":"reply_and_close","text":"Спасибо!"}]});
        let first=family_result(first);
        let review=super::super::preparation_review::settle_first(&mut d,run,request,&first,&super::super::now()).unwrap().unwrap();
        assert_eq!(review["items"].as_array().unwrap().len(),1);
        assert_eq!(review["items"][0]["id"],"media");
        admit_groups(&mut d,run,&first,false).unwrap();
        assert_eq!(d["proposals"].as_array().unwrap().len(),1);
        assert_eq!(d["items"][0]["workflow"],"prepared");
        let groups=super::super::row(&d,"jobs",run).unwrap()["preparationStages"]["groupAdmission"].as_array().unwrap();
        assert_eq!(groups.iter().find(|g|g["itemIds"].as_array().unwrap().contains(&json!("ready"))).unwrap()["status"],"admitted");
        admit_groups(&mut d,run,&first,false).unwrap();
        assert_eq!(d["proposals"].as_array().unwrap().len(),1,"durable admission must not replay");
        d["posts"][1]["text"]=json!("Unrelated hard branch changed");
        assert_eq!(d["proposals"].as_array().unwrap().len(),1);
        assert_eq!(d["items"][0]["workflow"],"prepared");
        assert!(crate::preparation_review::chunks::current(&d,super::super::row(&d,"jobs",run).unwrap()).is_err(),
            "stale pending review must fail closed before another model call");
    }
    #[test]
    fn shared_rule_invalidates_each_captured_branch_but_unrelated_post_does_not(){
        let mut d=family_fixture();
        let rule=crate::knowledge::save_instruction(&mut d,&json!({"requestId":"group-rule-initial","title":"Company rule","text":"Current company rule"}),&super::super::now()).unwrap();
        let scheduled=schedule(&mut d,Input{item_ids:vec!["ready".into(),"media".into()],instruction:None}).unwrap();
        let job=super::super::row(&d,"jobs",&scheduled.job_id).unwrap().clone();
        let groups=job["preparationStages"]["groupAdmission"].as_array().unwrap();
        assert_eq!(groups.len(),2);
        d["posts"][1]["text"]=json!("Changed only media post");
        let ready=groups.iter().find(|g|g["itemIds"].as_array().unwrap().contains(&json!("ready"))).unwrap();
        let media=groups.iter().find(|g|g["itemIds"].as_array().unwrap().contains(&json!("media"))).unwrap();
        assert!(crate::prepare_bundle::current_group(&d,&job["prepareBundle"],ready).is_ok());
        assert!(crate::prepare_bundle::current_group(&d,&job["prepareBundle"],media).is_err());
        crate::knowledge::save_instruction(&mut d,&json!({"requestId":"group-rule-revised","entryId":rule["version"]["entryId"],
            "expectedVersionId":rule["version"]["id"],"title":"Company rule","text":"Rule changed"}),&super::super::now()).unwrap();
        assert!(groups.iter().all(|g|crate::prepare_bundle::current_group(&d,&job["prepareBundle"],g).is_err()));
    }
    #[test]
    fn no_review_batch_admits_current_branch_when_other_branch_becomes_stale(){
        let mut d=family_fixture();
        let scheduled=schedule(&mut d,Input{item_ids:vec!["ready".into(),"media".into()],instruction:None}).unwrap();
        let run=&scheduled.job_id;
        let mut first=family_result(json!({"text":"Routine","sources":[],"assessments":[
            {"itemId":"ready","outcome":"reply","reason":"Thanks","tags":["feedback"]},
            {"itemId":"media","outcome":"reply","reason":"Thanks","tags":["feedback"]}],
            "proposals":[{"itemId":"ready","kind":"reply_and_close","text":"Спасибо!"},
                {"itemId":"media","kind":"reply_and_close","text":"Спасибо!"}]}));
        crate::model_material_receipt::fixture_result(&mut d,run,scheduled.request.as_ref().unwrap(),&mut first).unwrap();
        assert!(super::super::preparation_review::settle_first(&mut d,run,scheduled.request.as_ref().unwrap(),&first,&super::super::now()).unwrap().is_none());
        d["posts"][1]["text"]=json!("Changed only media post");
        let outcome=admit_result(&mut d,run,&first,false,vec![],vec!["ready".into(),"media".into()]).unwrap();
        assert_eq!(outcome["status"],"stale");
        assert_eq!(d["proposals"].as_array().unwrap().len(),1);
        assert_eq!(d["proposals"][0]["itemId"],"ready");
        assert_eq!(d["proposals"][0]["editorialReview"]["decision"],"accept");
        assert_eq!(d["items"][0]["workflow"],"prepared");
        assert_eq!(d["items"][1]["workflow"],"attention");
    }
    #[test]
    fn single_pass_web_evidence_is_scoped_per_group_without_rewriting_first_result(){
        let mut d=family_fixture();
        let scheduled=schedule(&mut d,Input{item_ids:vec!["ready".into(),"media".into()],instruction:None}).unwrap();
        let run=&scheduled.job_id;
        let mut first=family_result(json!({"text":"Two independent answers","sources":[],"assessments":[
            {"itemId":"ready","outcome":"reply","reason":"Context supports greeting","tags":["feedback"]},
            {"itemId":"media","outcome":"reply","reason":"Current web fact supports answer","tags":["question"]}],
            "proposals":[{"itemId":"ready","kind":"reply_and_close","text":"Спасибо!"},
                {"itemId":"media","kind":"reply_and_close","text":"Вот подтверждённый ответ."}]}));
        first["runMetadata"]["research"]=json!({"version":1,"status":"completed","model":crate::codex_model_policy::MODEL,"modelProfile":crate::codex_model_policy::PROFILE,
            "reasoningEffort":"high","instructionSha256":"a".repeat(64),"inputSha256":"b".repeat(64),
            "elapsedMs":1,"webCalls":1,"completedAt":"2026-09-28T00:00:00Z",
            "sources":[{"itemId":"media","url":"https://example.com/current-fact","title":"Current fact",
                "claim":"Exact scoped answer"}]});
        crate::model_material_receipt::fixture_result(&mut d,run,scheduled.request.as_ref().unwrap(),&mut first).unwrap();
        assert!(super::super::preparation_review::settle_first(&mut d,run,scheduled.request.as_ref().unwrap(),
            &first,&super::super::now()).unwrap().is_none());
        let outcome=admit_result(&mut d,run,&first,false,vec![],vec!["ready".into(),"media".into()]).unwrap();
        assert_eq!(outcome["status"],"review");
        assert_eq!(d["proposals"].as_array().unwrap().len(),2);
        let source=&d["jobs"].as_array().unwrap().iter().find(|j|j["id"]==*run).unwrap()["preparationStages"]["first"]["result"]["runMetadata"]["research"];
        assert_eq!(source["sources"].as_array().unwrap().len(),1,"immutable first pass keeps full web evidence");
        for proposal in d["proposals"].as_array().unwrap(){
            assert_eq!(proposal["editorialReview"]["decision"],"accept");
            let research=&proposal["generationMetadata"]["research"];
            if proposal["itemId"]=="ready" {
                assert_eq!(research["status"],"no_sources");
                assert!(research["sources"].as_array().unwrap().is_empty());
            }else{
                assert_eq!(proposal["itemId"],"media");
                assert_eq!(research["status"],"completed");
                assert_eq!(research["sources"][0]["itemId"],"media");
            }
        }
    }
    #[test]
    fn single_pass_cross_group_dependency_holds_only_relying_decision(){
        let mut d=family_fixture();
        let scheduled=schedule(&mut d,Input{item_ids:vec!["ready".into(),"media".into()],instruction:None}).unwrap();
        let run=&scheduled.job_id;
        let mut first=family_result(json!({"text":"Two decisions","sources":[],"assessments":[
            {"itemId":"ready","outcome":"reply","reason":"Uses other branch","tags":["question"]},
            {"itemId":"media","outcome":"reply","reason":"Independent source","tags":["feedback"]}],
            "proposals":[{"itemId":"ready","kind":"reply_and_close","text":"Dependent answer"},
                {"itemId":"media","kind":"reply_and_close","text":"Independent answer"}]}));
        first["runMetadata"]["decisionDependencies"]["entries"][0]["dependsOnItemIds"]=json!(["media"]);
        crate::model_material_receipt::fixture_result(&mut d,run,scheduled.request.as_ref().unwrap(),&mut first).unwrap();
        assert!(super::super::preparation_review::settle_first(&mut d,run,scheduled.request.as_ref().unwrap(),
            &first,&super::super::now()).unwrap().is_none());
        let outcome=admit_result(&mut d,run,&first,false,vec![],vec!["ready".into(),"media".into()]).unwrap();
        assert_eq!(outcome["status"],"review");
        assert_eq!(outcome["needsAttention"].as_array().unwrap().len(),1);
        assert_eq!(outcome["needsAttention"][0]["itemId"],"ready");
        assert_eq!(d["proposals"].as_array().unwrap().len(),1);
        assert_eq!(d["proposals"][0]["itemId"],"media");
        assert_eq!(d["proposals"][0]["editorialReview"]["decision"],"accept");
        assert_eq!(d["jobs"].as_array().unwrap().iter().find(|j|j["id"]==*run).unwrap()["preparationStages"]["first"]["result"]["assessments"][0]["outcome"],
            "reply","original model result stays immutable for audit");
    }
    #[test]
    fn single_pass_within_group_dependencies_remain_and_cross_group_holds_propagate(){
        let ids=["a","b","c","d","outside"];
        let mut result=single_pass_result(json!({"text":"Dependency graph","sources":[],
            "assessments":ids.iter().map(|id|json!({"itemId":id,"outcome":"reply","reason":"Reason","tags":["feedback"]})).collect::<Vec<_>>(),
            "proposals":ids.iter().map(|id|json!({"itemId":id,"kind":"reply_and_close","text":format!("Answer {id}")})).collect::<Vec<_>>()}));
        let entries=&mut result["runMetadata"]["decisionDependencies"]["entries"];
        entries[0]["dependsOnItemIds"]=json!(["b"]); // within group: retained
        entries[2]["dependsOnItemIds"]=json!(["outside"]); // cross group: held
        entries[3]["dependsOnItemIds"]=json!(["c"]); // transitive: held
        let group=group_result(&result,&[json!("a"),json!("b"),json!("c"),json!("d")]);
        assert_eq!(group["proposals"].as_array().unwrap().iter().map(|p|p["itemId"].as_str().unwrap()).collect::<Vec<_>>(),vec!["a","b"]);
        assert_eq!(group["assessments"][0]["outcome"],"reply");
        assert_eq!(group["assessments"][1]["outcome"],"reply");
        assert_eq!(group["assessments"][2]["outcome"],"needs_attention");
        assert_eq!(group["assessments"][3]["outcome"],"needs_attention");
        assert_eq!(group["editorialEvidence"]["entries"].as_array().unwrap().len(),2);
        assert!(group["runMetadata"].get("decisionDependencies").is_none());
        assert_eq!(result["runMetadata"]["decisionDependencies"]["entries"].as_array().unwrap().len(),5);
    }

    #[test]
    fn scoped_moderation_proof_follows_only_surviving_exact_group_actions() {
        let mut result=single_pass_result(json!({"text":"Moderation decisions","sources":[],
            "assessments":[
                {"itemId":"a","outcome":"hide","reason":"Depends on other branch","tags":["moderation"]},
                {"itemId":"b","outcome":"delete","reason":"Independent rule violation","tags":["moderation"]},
                {"itemId":"outside","outcome":"delete","reason":"Separate branch","tags":["moderation"]}],
            "proposals":[{"itemId":"a","kind":"hide","text":""},
                {"itemId":"b","kind":"delete","text":""},
                {"itemId":"outside","kind":"delete","text":""}]}));
        result["runMetadata"]["decisionDependencies"]["entries"][0]["dependsOnItemIds"]=json!(["outside"]);
        result["moderationEvidence"]=json!({"version":1,"entries":[
            {"itemId":"a","kind":"hide","ruleRefs":[]},
            {"itemId":"b","kind":"delete","ruleRefs":[]},
            {"itemId":"outside","kind":"delete","ruleRefs":[]}]});
        let group=group_result(&result,&[json!("a"),json!("b")]);
        assert_eq!(group["proposals"],json!([{"itemId":"b","kind":"delete","text":""}]));
        assert_eq!(group["moderationEvidence"]["entries"],json!([{"itemId":"b","kind":"delete","ruleRefs":[]}]));
        assert_eq!(result["moderationEvidence"]["entries"].as_array().unwrap().len(),3,
            "the complete first-stage moderation proof stays immutable");
    }

    #[tokio::test]
    async fn scoped_workers_overlap_native_bridge_and_cancel_same_family_waiter_before_model() {
        let (mut app,temp)=super::first_capture_recovery_tests::baw_app().await;
        app.preparation_workers=std::sync::Arc::new(crate::preparation_workers::Pool::new(4).unwrap());
        let mut d=baw_fixture(false);
        for key in ["knowledge_entries","knowledge_versions","feedback"] {d[key]=json!([]);}
        d["runtimeLifecycle"]=app.db.read().await.unwrap()["runtimeLifecycle"].clone();
        let mut same=d["items"][0].clone();same["id"]=json!("same-family");same["itemId"]=json!("c-same");
        same["branchId"]=json!("same-branch");same["conversationKey"]=json!("same-thread");
        super::super::list_mut(&mut d,"items").push(same);
        super::super::list_mut(&mut d,"branches").push(json!({"id":"same-branch","postId":"ready-post",
            "messages":[{"id":"c-same","text":"Спасибо"}],"contextComplete":true}));
        for id in ["third","fourth"] {
            let mut item=d["items"][0].clone();item["id"]=json!(id);item["itemId"]=json!(format!("c-{id}"));
            item["postId"]=json!(format!("{id}-post"));item["postKey"]=item["postId"].clone();
            item["objectId"]=json!(format!("o-{id}"));item["branchId"]=json!(format!("{id}-branch"));item["conversationKey"]=json!(format!("{id}-thread"));
            super::super::list_mut(&mut d,"items").push(item);
            super::super::list_mut(&mut d,"branches").push(json!({"id":format!("{id}-branch"),"postId":format!("{id}-post"),
                "messages":[{"id":format!("c-{id}"),"text":"Спасибо"}],"contextComplete":true}));
            super::super::list_mut(&mut d,"posts").push(json!({"id":format!("{id}-post"),"postKey":format!("{id}-post"),
                "objectId":format!("o-{id}"),"platform":"VK","text":"Independent post","attachments":[]}));
        }
        app.db.change(|data|{*data=d;Ok(())}).await.unwrap();
        let baseline=app.db.read().await.unwrap();
        assert!(app.change_preparation_schedule(|d| {
            let scheduled=schedule(d,Input{item_ids:vec!["ready".into()],instruction:None})?;
            super::super::row_mut(d,"jobs",&scheduled.job_id)?["preparationWorkerScope"]["keys"]=json!([]);
            Ok(scheduled)
        }).await.is_err(),"scoped storage must reject fabricated family ownership");
        assert_eq!(app.db.read().await.unwrap(),baseline,"invalid capture must not leave a paid job");
        let lifecycle=app.lifecycle_admission_token(crate::runtime_lifecycle::AdmissionClass::Preparation).await.unwrap();
        let first=app.change_preparation_schedule(|d|schedule_admitted(d,Input{item_ids:vec!["ready".into()],instruction:None},&lifecycle)).await.unwrap();
        let second=app.change_preparation_schedule(|d|schedule_admitted(d,Input{item_ids:vec!["media".into()],instruction:None},&lifecycle)).await.unwrap();
        let third=app.change_preparation_schedule(|d|schedule_admitted(d,Input{item_ids:vec!["third".into()],instruction:None},&lifecycle)).await.unwrap();
        let fourth=app.change_preparation_schedule(|d|schedule_admitted(d,Input{item_ids:vec!["fourth".into()],instruction:None},&lifecycle)).await.unwrap();
        let waiting=app.change_preparation_schedule(|d|schedule_admitted(d,Input{item_ids:vec!["same-family".into()],instruction:None},&lifecycle)).await.unwrap();
        let waiting_id=waiting.job_id.clone();
        let saved=app.db.read().await.unwrap();
        for job in [&first.job_id,&second.job_id,&third.job_id,&fourth.job_id,&waiting.job_id] {
            assert!(crate::preparation_workers::keys(&saved,super::super::row(&saved,"jobs",job).unwrap()).is_some(),"fixture establishes exact family independence");
        }
        app.node=std::env::var_os("COMMUNITYHERO_TEST_NODE").map(std::path::PathBuf::from).unwrap_or_else(||"node".into());
        app.bridge=temp.path().join("scoped-workers.mjs");
        let script=r#"import fs from 'node:fs/promises';import path from 'node:path';
import {materialInvocation} from __MATERIAL_MODULE__;
let raw='';for await(const part of process.stdin)raw+=part;const request=JSON.parse(raw);
if(request.purpose!=='triage')throw new Error('Unexpected model stage');
const id=request.items[0].id;const root=__ROOT__;
await fs.writeFile(path.join(root,'started-'+id),JSON.stringify({slot:process.env.COMMUNITYHERO_PREPARE_WORKER_SLOT}));
for(;;){try{await fs.access(path.join(root,'release-'+id));break;}catch{await new Promise(r=>setTimeout(r,10));}}
const result=__RESULT__;for(const key of ['assessments','proposals'])for(const row of result[key])row.itemId=id;
for(const row of result.runMetadata.decisionDependencies.entries)row.itemId=id;
for(const row of result.editorialEvidence.entries)row.itemId=id;
if(request.postContextBundle.members.some(member=>member.assets.some(asset=>asset.modality==='photo')))throw Error('Unexpected fixture photo');
const input=JSON.stringify(request);
const invocation=materialInvocation({payload:request,input},{manifest:[]},{instructions:'Synthetic BAW worker fixture',schema:'{}',cliSha256:result.runMetadata.cliSha256,stdin:input});
Object.assign(result.runMetadata,{inputSha256:invocation.actualTextInputSha256,instructionSha256:invocation.instructionSha256,materialInvocation:invocation,visualNeedContract:request.visualNeedContract,visualSelection:request.visualSelection});
process.stdout.write(JSON.stringify({ok:true,result}));"#
            .replace("__MATERIAL_MODULE__",&json!(format!("file:///{}",std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../adapters/assistant-materials.mjs").to_string_lossy().replace('\\',"/"))).to_string())
            .replace("__ROOT__",&json!(temp.path().to_string_lossy()).to_string())
            .replace("__RESULT__",&single_pass_result(routine_result()).to_string());
        std::fs::write(&app.bridge,script).unwrap();
        let sender=app.execution_gate.lock().await;
        let first_run=tokio::spawn({let worker=app.clone();async move{crate::runtime_lifecycle_app::with_job(first.job_id.clone(),run(worker,first,false)).await}});
        let wait_started=|id:&str| {let file=temp.path().join(format!("started-{id}"));async move {
            tokio::time::timeout(std::time::Duration::from_secs(15),async {while !file.exists(){tokio::time::sleep(std::time::Duration::from_millis(10)).await;}}).await.unwrap();
        }};
        wait_started("ready").await;
        let waiting_run=tokio::spawn({let worker=app.clone();async move{crate::runtime_lifecycle_app::with_job(waiting.job_id.clone(),run(worker,waiting,false)).await}});
        tokio::task::yield_now().await;
        let second_run=tokio::spawn({let worker=app.clone();async move{crate::runtime_lifecycle_app::with_job(second.job_id.clone(),run(worker,second,false)).await}});
        wait_started("media").await;
        let third_run=tokio::spawn({let worker=app.clone();async move{crate::runtime_lifecycle_app::with_job(third.job_id.clone(),run(worker,third,false)).await}});
        wait_started("third").await;
        let fourth_run=tokio::spawn({let worker=app.clone();async move{crate::runtime_lifecycle_app::with_job(fourth.job_id.clone(),run(worker,fourth,false)).await}});
        wait_started("fourth").await;
        assert!(!temp.path().join("started-same-family").exists());
        let first_slot:Value=serde_json::from_str(&std::fs::read_to_string(temp.path().join("started-ready")).unwrap()).unwrap();
        let second_slot:Value=serde_json::from_str(&std::fs::read_to_string(temp.path().join("started-media")).unwrap()).unwrap();
        assert_eq!(first_slot["slot"],"0");assert_eq!(second_slot["slot"],"1");
        for (id,slot) in [("third","2"),("fourth","3")] {
            let started:Value=serde_json::from_str(&std::fs::read_to_string(temp.path().join(format!("started-{id}"))).unwrap()).unwrap();
            assert_eq!(started["slot"],slot);
            std::fs::write(temp.path().join(format!("release-{id}")),"done").unwrap();
        }
        std::fs::write(temp.path().join("release-media"),"done").unwrap();
        assert_eq!(tokio::time::timeout(std::time::Duration::from_secs(15),second_run).await.unwrap().unwrap().unwrap()["status"],"review");
        assert_eq!(tokio::time::timeout(std::time::Duration::from_secs(15),third_run).await.unwrap().unwrap().unwrap()["status"],"review");
        assert_eq!(tokio::time::timeout(std::time::Duration::from_secs(15),fourth_run).await.unwrap().unwrap().unwrap()["status"],"review");
        assert!(!temp.path().join("started-same-family").exists(),"free slot cannot overtake an active same family");
        app.change_job(&waiting_id,|data|{super::super::row_mut(data,"jobs",&waiting_id)?["status"]=json!("cancelled");Ok(())}).await.unwrap();
        std::fs::write(temp.path().join("release-ready"),"done").unwrap();
        assert_eq!(tokio::time::timeout(std::time::Duration::from_secs(15),first_run).await.unwrap().unwrap().unwrap()["status"],"review");
        assert!(waiting_run.await.unwrap().unwrap_err().1.contains("cancelled"));
        assert!(!temp.path().join("started-same-family").exists());
        drop(sender);let saved=app.db.read().await.unwrap();
        assert_eq!(saved["proposals"].as_array().unwrap().len(),4);
        for key in ["approvals","operations"] {assert!(saved[key].as_array().unwrap().is_empty());}
        app.db.close().await;
    }

    #[tokio::test]
    async fn first_pass_worker_settles_before_stale_admission_without_second_model_call() {
        let (mut app,temp)=super::first_capture_recovery_tests::baw_app().await;
        let mut initial=baw_fixture(false);
        // The domain-only fixture omits migrated storage collections. This
        // worker test replaces a complete database document, so supply them.
        for key in ["knowledge_entries","knowledge_versions","feedback"] {initial[key]=json!([]);}
        initial["runtimeLifecycle"]=app.db.read().await.unwrap()["runtimeLifecycle"].clone();
        let lifecycle=app.lifecycle_admission_token(crate::runtime_lifecycle::AdmissionClass::Preparation).await.unwrap();
        let scheduled=schedule_admitted(&mut initial,Input{item_ids:vec!["ready".into()],instruction:None},&lifecycle).unwrap();
        let run_id=scheduled.job_id.clone();
        app.db.change(|d|{*d=initial;Ok(())}).await.unwrap();
        app.node=std::env::var_os("COMMUNITYHERO_TEST_NODE").map(std::path::PathBuf::from)
            .unwrap_or_else(||std::path::PathBuf::from("node"));
        app.bridge=temp.path().join("first-settlement.mjs");
        let started=temp.path().join("started.json");let release=temp.path().join("release");
        let first=single_pass_result(routine_result());
        let returned=temp.path().join("returned.json");
        let script=r#"import fs from 'node:fs/promises';
import {materialInvocation} from __MATERIAL_MODULE__;
let raw='';for await(const part of process.stdin)raw+=part;
const request=JSON.parse(raw);if(request.purpose!=='triage')throw new Error('Unexpected extra model call');
await fs.writeFile(__START__,raw);
for(;;){try{await fs.access(__RELEASE__);break;}catch{await new Promise(r=>setTimeout(r,10));}}
const result=__RESULT__;const input=JSON.stringify(request);
if(request.postContextBundle.members.some(member=>member.assets.some(asset=>asset.modality==='photo')))throw Error('Unexpected fixture photo');
const invocation=materialInvocation({payload:request,input},{manifest:[]},{instructions:'Synthetic BAW stale-source fixture',schema:'{}',cliSha256:result.runMetadata.cliSha256,stdin:input});
Object.assign(result.runMetadata,{inputSha256:invocation.actualTextInputSha256,instructionSha256:invocation.instructionSha256,materialInvocation:invocation,visualNeedContract:request.visualNeedContract,visualSelection:request.visualSelection});
await fs.writeFile(__RETURNED__,JSON.stringify(result));
process.stdout.write(JSON.stringify({ok:true,result}));"#
            .replace("__MATERIAL_MODULE__",&json!(format!("file:///{}",std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../adapters/assistant-materials.mjs").to_string_lossy().replace('\\',"/"))).to_string())
            .replace("__RETURNED__",&json!(returned.to_string_lossy()).to_string())
            .replace("__START__",&json!(started.to_string_lossy()).to_string())
            .replace("__RELEASE__",&json!(release.to_string_lossy()).to_string()).replace("__RESULT__",&first.to_string());
        std::fs::write(&app.bridge,script).unwrap();
        let worker=app.clone();let pending=tokio::spawn(async move {crate::runtime_lifecycle_app::with_job(scheduled.job_id.clone(),run(worker,scheduled,false)).await});
        tokio::time::timeout(std::time::Duration::from_secs(15),async {
            while !started.exists(){tokio::time::sleep(std::time::Duration::from_millis(10)).await;}
        }).await.unwrap();
        app.db.change(|d|{d["posts"][0]["text"]=json!("Shared evidence changed during model work");Ok(())}).await.unwrap();
        std::fs::write(release,"return now").unwrap();
        let outcome=tokio::time::timeout(std::time::Duration::from_secs(15),pending).await.unwrap().unwrap().unwrap();
        assert_eq!(outcome["status"],"stale");
        let saved=app.db.read().await.unwrap();let job=super::super::row(&saved,"jobs",&run_id).unwrap();
        assert_eq!(job["preparationStages"]["first"]["status"],"completed");
        assert_eq!(job["preparationStages"]["first"]["result"]["proposals"],first["proposals"]);
        let original:Value=serde_json::from_slice(&std::fs::read(returned).unwrap()).unwrap();
        assert_eq!(job["preparationStages"]["first"]["result"]["runMetadata"],crate::prepare_bundle::generation_metadata(&original).unwrap().unwrap());
        assert_eq!(job["retainedEvidence"].as_array().unwrap().len(),1,"one original model result is retained before stale admission");
        let retained=crate::runtime_paid_result::resolve(&app,&run_id,"assistant",&job["retainedEvidence"][0]).await.unwrap();
        assert_eq!(retained["response"],original,"stale source cannot rewrite the original paid response");
        assert_eq!(job["prepareOutcome"]["status"],"stale");
        for key in ["proposals","approvals","operations"]{assert!(saved[key].as_array().unwrap().is_empty());}
        app.db.close().await;
    }

    #[test]
    fn first_settlement_rejects_request_retargeting_and_cancelled_owner() {
        for variant in ["request","mode","account","cancelled","bundle"] {
            let mut d=fixture(false);
            let scheduled=schedule(&mut d,Input{item_ids:vec!["ready".into()],instruction:None}).unwrap();
            let mut request=scheduled.request.unwrap();
            match variant {
                "request"=>request["items"][0]["text"]=json!("Different input"),
                "mode"=>request["preparationMode"]=json!("legacy"),
                "account"=>d["account"]=json!("BAW Russia"),
                "cancelled"=>super::super::row_mut(&mut d,"jobs",&scheduled.job_id).unwrap()["status"]=json!("failed"),
                _=>super::super::row_mut(&mut d,"jobs",&scheduled.job_id).unwrap()["prepareBundle"]["digest"]=json!("0".repeat(64)),
            }
            let before=d.clone();
            assert!(super::super::preparation_review::settle_first(&mut d,&scheduled.job_id,&request,&single_pass_result(routine_result()),&super::super::now()).is_err(),"{variant}");
            assert_eq!(d,before);
        }
    }

    #[test]
    fn image_budget_counts_exact_origins_and_shared_posts_not_urls_or_recipients() {
        let request = json!({"items":[
            {"id":"a","postId":"p","attachments":[{"type":"sticker"},{"type":"video"}]},
            {"id":"b","branchId":"b","commentAttachments":[{"type":"image"}]},
            {"id":"c","postId":"q","attachments":[]}
        ],"branches":[{"id":"b","postId":"p"}],"posts":[
            {"id":"p","attachments":[{"type":"photo","url":"https://cdn.example/same"},{"type":"photo","url":"https://cdn.example/same"},{"type":"video"}]},
            {"id":"q","attachments":[{"type":"image","url":"https://cdn.example/same"}]},
            {"id":"unrelated","attachments":[{"type":"photo"}]}
        ]});
        assert_eq!(image_count(&request), 5);
    }

    #[test]
    fn selected_image_budget_ignores_optional_carousels_but_keeps_comment_photos_and_exact_shared_slots(){
        let mut request=json!({"visualNeedContract":crate::prepare_bundle::visual::CONTRACT,
            "visualSelection":crate::prepare_bundle::visual::empty(),"items":[
                {"id":"a","postId":"p","attachments":[{"type":"sticker"}]},
                {"id":"b","postId":"p","attachments":[{"type":"photo"}]}],"branches":[],
            "posts":[{"id":"p","attachments":[{"type":"photo","url":"https://example.com/a.jpg"},{"type":"image","url":"https://example.com/b.jpg"}]}]});
        assert_eq!(image_count(&request),2,"own-comment images remain mandatory");
        request["visualSelection"]["postImages"]=json!([
            {"itemId":"a","postId":"p","attachmentIndices":[1],"reason":"Read displayed price"},
            {"itemId":"b","postId":"p","attachmentIndices":[1],"reason":"Review the same displayed price"}]);
        assert_eq!(image_count(&request),3,"selected shared source occupies one slot");
        request["visualSelection"]["postImages"][1]["attachmentIndices"]=json!([0,1]);assert_eq!(image_count(&request),4);
        request["visualSelection"]["postImages"][1]["itemId"]=json!("other");assert_eq!(image_count(&request),MAX_REQUEST_IMAGES+1,"invalid selector cannot evade capacity");
    }

    #[test]
    fn mandatory_post_photos_keep_exact_sources_and_hold_common_material_over_capacity(){
        let mut d=baw_fixture(false);
        d["posts"][0]["attachments"]=json!((0..20).map(|n|json!({"type":"photo","url":format!("https://cdn.example/{n}.png")})).collect::<Vec<_>>());
        let before=d.clone();assert!(build_request(&d,&[json!("ready")],None).unwrap_err().contains("16-image budget"));
        assert_eq!(d,before,"capacity may not trim mandatory photos or mutate source evidence");
        d["posts"][0]["attachments"].as_array_mut().unwrap().truncate(MAX_REQUEST_IMAGES);
        let bundle=build_request(&d,&[json!("ready")],None).unwrap();
        assert_eq!(bundle["request"]["visualNeedContract"],crate::prepare_bundle::visual::CONTRACT);
        assert_eq!(bundle["request"]["visualSelection"]["postImages"][0]["attachmentIndices"],json!((0..MAX_REQUEST_IMAGES).collect::<Vec<_>>()));
        assert_eq!(image_count(&bundle["request"]),MAX_REQUEST_IMAGES);
        assert_eq!(bundle["request"]["posts"][0]["attachments"],d["posts"][0]["attachments"]);
    }

    #[test]
    fn model_budget_counts_exact_copies_once_but_capture_keeps_them(){
        let text="x".repeat(300_000);
        let request=json!({"items":[{"id":"i","text":text,"preview":text}],"posts":[]});
        assert!(request.to_string().len()>MAX_REQUEST_BYTES);
        assert!(model_request_bytes(&request)<MAX_REQUEST_BYTES);
        assert_eq!(request["items"][0]["preview"],request["items"][0]["text"]);
        let mut changed=request.clone();changed["items"][0]["preview"]=json!("different context");
        assert!(model_request_bytes(&changed)>model_request_bytes(&request));
    }

    #[test]
    fn model_budget_omits_only_proven_video_material_and_preserves_canonical_capture(){
        let mut d=fixture(false);
        d["posts"][0]["attachments"]=json!([{"type":"video"}]);
        let evidence=crate::media_fullframes::fixture_for_post("LikeAvto",&d["posts"][0]);
        d["materials"]=json!([{"id":"budget-video","kind":"visual_context","postKey":"ready-post",
            "account":"LikeAvto","mediaSha256":evidence["source"]["mediaSha256"],
            "text":"Video frame observations","visualEvidence":evidence}]);
        crate::knowledge::sync_catalog(&mut d,"2026-09-28T00:00:00Z").unwrap();
        let bundle=build_request(&d,&[json!("ready")],None).unwrap();
        let before=bundle.clone(); let request=&bundle["request"];
        assert_eq!(request["materials"].as_array().unwrap().len(),1);
        assert_eq!(request["materials"][0]["visualEvidence"],evidence);
        let mut expected=request.clone(); expected["materials"]=json!([]);
        assert_eq!(model_request_bytes(request),model_request_bytes(&expected));
        assert!(model_request_bytes(request)<request.to_string().len());
        assert_eq!(bundle,before);
        assert_eq!(bundle["digest"],json!(format!("{:x}",Sha256::digest(request.to_string().as_bytes()))));
        assert_eq!(request["knowledgeManifest"],expected["knowledgeManifest"],"manifest remains conservatively counted");
    }

    #[test]
    fn model_budget_counts_photo_ambiguous_legacy_and_tampered_visuals_conservatively(){
        let evidence=crate::media_fullframes::fixture("LikeAvto","budget-conservative");
        let material=json!({"kind":"visual_context","postKey":"budget-conservative","visualEvidence":evidence});
        let mut variants=Vec::new();
        let mut photo=material.clone(); photo["visualEvidence"]=json!({"kind":"photo","text":"Direct photo observations"}); variants.push(photo);
        let mut unknown=material.clone(); unknown.as_object_mut().unwrap().remove("postKey"); variants.push(unknown);
        let mut mismatch=material.clone(); mismatch["postKey"]=json!("other"); variants.push(mismatch);
        let mut legacy=material.clone(); legacy["visualEvidence"]["schemaVersion"]=json!(1); legacy["visualEvidence"]["coverage"]["kind"]=json!("sampled_frames"); variants.push(legacy);
        let mut wrong_kind=material.clone(); wrong_kind["kind"]=json!("ocr"); variants.push(wrong_kind);
        for duration in [json!(0),json!(-1),json!(1.5),json!(9_007_199_254_740_992_u64)] {
            let mut invalid=material.clone(); invalid["visualEvidence"]["source"]["durationMs"]=duration; variants.push(invalid);
        }
        let mut coverage=material.clone(); coverage["visualEvidence"]["coverage"]["kind"]=json!("unclassified"); variants.push(coverage);
        let mut tampered=material.clone(); tampered["visualEvidence"]["aggregate"][0]["observation"]=json!("Altered observation"); variants.push(tampered);
        for variant in variants {
            let request=json!({"materials":[variant],"items":[],"posts":[]});
            assert_eq!(model_request_bytes(&request),request.to_string().len());
        }
    }

    #[test]
    fn model_budget_requires_current_exact_video_proof_without_rewarming(){
        let evidence=crate::media_fullframes::fixture("LikeAvto","budget-cold-proof");
        let request=json!({"materials":[{"kind":"visual_context","postKey":"budget-cold-proof","visualEvidence":evidence}],"items":[],"posts":[]});
        assert!(model_request_bytes(&request)<request.to_string().len());
        crate::media_fullframes::expire_test_proof(&evidence);
        assert_eq!(model_request_bytes(&request),request.to_string().len());
        assert!(crate::media_fullframes::validate_evidence(&evidence).is_err(),"counting must not reload artifact files");
    }
    #[test]
    fn complete_capture_over_legacy_ceiling_can_pass_bounded_model_projection(){
        let mut d=fixture(false);
        let source=d["items"][0].clone();
        let long="x".repeat(17_000);
        d["items"]=json!((0..24).map(|n|{
            let mut item=source.clone();item["id"]=json!(format!("copy-{n}"));
            item["itemId"]=json!(format!("comment-{n}"));item["text"]=json!(long);item["preview"]=json!(long);item
        }).collect::<Vec<_>>());
        let ids:Vec<Value>=(0..24).map(|n|json!(format!("copy-{n}"))).collect();
        let bundle=build_request(&d,&ids,None).unwrap();
        assert!(bundle["request"].to_string().len()>MAX_REQUEST_BYTES);
        assert!(bundle["request"].to_string().len()<MAX_CAPTURE_BYTES);
        assert!(model_request_bytes(&bundle["request"])<MAX_REQUEST_BYTES);
        assert_eq!(bundle["request"]["items"][0]["preview"],bundle["request"]["items"][0]["text"]);
    }

    #[test]
    fn image_capacity_is_checked_before_a_preparation_job_is_created() {
        let mut d=fixture(false);
        d["items"][0]["attachments"]=json!((0..17).map(|n|json!({"type":"photo","url":format!("https://cdn.example/{n}.png")})).collect::<Vec<_>>());
        let before=d.clone();
        assert!(schedule(&mut d, Input {item_ids:vec!["ready".into()],instruction:None}).is_err());
        assert_eq!(d,before);
        d["items"][0]["attachments"].as_array_mut().unwrap().pop();
        let bundle=build_request(&d,&[json!("ready")],None).unwrap();
        assert_eq!(image_count(&bundle["request"]),16);
    }

    #[test]
    fn body_is_strict_and_bounded() {
        assert!(parse(&json!({"itemIds":["one"],"instruction":"context"})).is_ok());
        for body in [
            json!({}),
            json!({"itemIds":[]}),
            json!({"itemIds":["one","one"]}),
            json!({"itemIds":[1]}),
            json!({"itemIds":["one"],"extra":true}),
            json!({"itemIds":["one"],"instruction":"x".repeat(MAX_INSTRUCTION+1)}),
        ] {
            assert!(parse(&body).is_err());
        }
        assert!(
            parse(&json!({"itemIds":(0..MAX_ITEMS).map(|n|format!("i{n}")).collect::<Vec<_>>() }))
                .is_ok()
        );
        assert!(
            parse(&json!({"itemIds":(0..=MAX_ITEMS).map(|n|format!("i{n}")).collect::<Vec<_>>() }))
                .is_err()
        );
    }

    #[test]
    fn mixed_schedule_is_rejected_and_missing_video_is_unpaid_until_mandatory_readiness() {
        let mut d = baw_fixture(true);
        let before=d.clone();
        assert!(schedule(&mut d,Input{item_ids:vec!["ready".into(),"media".into()],instruction:None}).is_err());
        assert_eq!(d,before,"mixed direct admission must create no job or paid intent");
        let scheduled = schedule(
            &mut d,
            Input {
                item_ids: vec!["media".into()],
                instruction: Some("Keep it short".into()),
            },
        )
        .unwrap();
        assert_eq!(scheduled.selected, ["media"]);
        assert!(scheduled.held.is_empty());
        assert!(crate::decision_media::enabled(scheduled.request.as_ref().unwrap()));
        assert!(scheduled.request.as_ref().unwrap()["posts"].as_array().unwrap().iter()
            .any(|post|post["decisionMediaEvidence"]["audioReady"]==false));
        assert_eq!(scheduled.request.as_ref().unwrap()["purpose"], "triage");
        assert!(
            scheduled.request.as_ref().unwrap()["instruction"]
                .as_str()
                .unwrap()
                .contains("Keep it short")
        );
        let job = super::super::row(&d, "jobs", &scheduled.job_id).unwrap();
        assert_eq!(job["kind"], "assistant");
        assert_eq!(job["purpose"], "engine_prepare");
        assert_eq!(job["prepareBundle"]["itemIds"], json!(["media"]));
        assert!(super::super::prepare_bundle::current(&d, &job["prepareBundle"]).is_ok());
        assert!(preflight_capture(&d, &scheduled.job_id).is_ok(),"native unpaid acquisition can retain exact source authority");
        assert!(preflight(&d, &scheduled.job_id).is_err(),"missing speech cannot start a paid assessment");
    }

    #[test]
    fn all_held_completes_without_a_model_request() {
        let mut d = fixture(true);
        d["items"] = json!([d["items"][1].clone()]);
        d["branches"] = json!([d["branches"][1].clone()]);
        d["posts"] = json!([d["posts"][1].clone()]);
        let post=d["posts"][0].clone();
        d["settings"]["postMediaPolicies"]=json!({(post["id"].as_str().unwrap()):{
            "version":1,"revision":1,"status":"active","postId":post["id"],"mode":"full_audio_visual",
            "account":d["account"],"connectorBinding":d["connectorBinding"],
            "sourceVersion":crate::media_fullframes::source_version(&post,d["account"].as_str().unwrap())}});
        let scheduled = schedule(
            &mut d,
            Input {
                item_ids: vec!["media".into()],
                instruction: None,
            },
        )
        .unwrap();
        assert!(scheduled.request.is_none());
        assert!(
            super::super::row(&d, "jobs", &scheduled.job_id).unwrap()["prepareBundle"].is_null()
        );
        let result = all_held_result(scheduled.held);
        assert_eq!(result["status"], "held");
        assert!(result["candidates"].as_array().unwrap().is_empty());
        assert!(d["proposals"].as_array().unwrap().is_empty());
        assert!(d["approvals"].as_array().unwrap().is_empty());
        assert!(d["operations"].as_array().unwrap().is_empty());
    }

    #[test]
    fn first_pass_is_archived_before_draft_admission() {
        let mut d = baw_fixture(false);
        d["items"] = json!([d["items"][0].clone()]);
        d["branches"] = json!([d["branches"][0].clone()]);
        d["posts"] = json!([d["posts"][0].clone()]);
        let scheduled = schedule(
            &mut d,
            Input {
                item_ids: vec!["ready".into()],
                instruction: None,
            },
        )
        .unwrap();
        let mut first = single_pass_result(routine_result());
        crate::model_material_receipt::fixture_result(&mut d,&scheduled.job_id,scheduled.request.as_ref().unwrap(),&mut first).unwrap();
        assert!(
            super::super::preparation_review::record_first(
                &mut d,
                &scheduled.job_id,
                &first,
                "2026-09-22T12:00:00Z"
            )
            .unwrap()
            .is_none()
        );
        assert!(d["proposals"].as_array().unwrap().is_empty());
        let admission =
            super::super::prepare_bundle::admit_to(&mut d, &scheduled.job_id, None, &first)
                .unwrap();
        let result = enrich(admission, &first, scheduled.held, scheduled.selected);
        assert_eq!(result["preparedItemIds"], json!(["ready"]));
        assert_eq!(d["proposals"][0]["prepareRunId"], scheduled.job_id);
        assert_eq!(d["proposals"][0]["status"], "draft");
        assert_eq!(d["proposals"][0]["editorialReview"]["decision"],"accept",
            "single-pass admission must bind the exact final decision to an editorial receipt");
        assert!(d["approvals"].as_array().unwrap().is_empty());
        assert!(d["operations"].as_array().unwrap().is_empty());
    }

    #[test]
    fn review_is_planned_but_never_admitted_from_first_pass() {
        let mut d = fixture(false);
        d["items"] = json!([d["items"][0].clone()]);
        d["branches"] = json!([d["branches"][0].clone()]);
        d["posts"] = json!([d["posts"][0].clone()]);
        let mut scheduled = schedule(
            &mut d,
            Input {
                item_ids: vec!["ready".into()],
                instruction: None,
            },
        )
        .unwrap();
        legacy_mode(&mut d,&mut scheduled);
        let first = json!({
            "text":"Needs research",
            "sources":[],
            "assessments":[{"itemId":"ready","outcome":"needs_attention","reason":"Need a fact","tags":["needs_fact"]}],
            "proposals":[]
        });
        let review = super::super::preparation_review::record_first(
            &mut d,
            &scheduled.job_id,
            &first,
            "2026-09-22T12:00:00Z",
        )
        .unwrap()
        .unwrap();
        assert_eq!(review["purpose"], "triage_review");
        assert_eq!(review["firstPass"]["trust"], "untrusted_model_output");
        assert!(d["proposals"].as_array().unwrap().is_empty());
        assert!(d["approvals"].as_array().unwrap().is_empty());
        assert!(d["operations"].as_array().unwrap().is_empty());
    }

    #[test]
    fn mixed_batch_reviews_missed_close_and_hold_then_reports_each_recipient() {
        let mut d = family_fixture();
        let mut scheduled = schedule(&mut d, Input {
            item_ids: vec!["ready".into(), "media".into()],
            instruction: None,
        }).unwrap();
        legacy_mode(&mut d,&mut scheduled);
        assert_eq!(scheduled.selected, ["ready", "media"]);
        assert_eq!(scheduled.request.as_ref().unwrap()["items"].as_array().unwrap().len(), 2);
        let first = json!({
            "text":"Initial decisions", "sources":[],
            "assessments":[
                {"itemId":"ready","outcome":"close","reason":"Initial close","tags":[]},
                {"itemId":"media","outcome":"needs_attention","reason":"Need exact fact","tags":["needs_fact"]}
            ],
            "proposals":[{"itemId":"ready","kind":"close","text":""}]
        });
        let review = super::super::preparation_review::record_first(
            &mut d, &scheduled.job_id, &first, "2026-09-22T12:00:00Z"
        ).unwrap().unwrap();
        assert_eq!(review["purpose"], "triage_review");
        assert_eq!(review["items"].as_array().unwrap().len(), 2);
        assert_eq!(review["firstPass"]["assessments"].as_array().unwrap().len(), 2);
        assert!(d["proposals"].as_array().unwrap().is_empty(), "first-pass close is not admitted");

        let revised = json!({
            "text":"The close missed a useful reply; the factual question still needs evidence",
            "sources":[],
            "assessments":[
                {"itemId":"ready","outcome":"reply","reason":"Useful response","tags":["feedback"]},
                {"itemId":"media","outcome":"needs_attention","reason":"Exact fact unavailable","tags":["needs_fact"]}
            ],
            "proposals":[{"itemId":"ready","kind":"reply_and_close","text":"Спасибо за отзыв!"}]
        });
        let revised=family_result(revised);
        let admission = super::super::prepare_bundle::admit_to(
            &mut d, &scheduled.job_id, None, &revised
        ).unwrap();
        let result = enrich(admission, &revised, scheduled.held, scheduled.selected);
        assert_eq!(result["status"], "review");
        assert_eq!(result["preparedItemIds"], json!(["ready"]));
        assert_eq!(result["needsAttention"], json!([{"itemId":"media","reason":"Exact fact unavailable"}]));
        assert_eq!(d["proposals"].as_array().unwrap().len(), 1);
        assert!(d["approvals"].as_array().unwrap().is_empty());
        assert!(d["operations"].as_array().unwrap().is_empty());
    }

    #[test]
    fn reviewed_hold_without_candidate_is_explicitly_held() {
        let admission=json!({"status":"discussed","candidates":[]});
        let result=enrich(admission,&json!({"assessments":[{"itemId":"media","outcome":"needs_attention","reason":"Missing answer"}]}),vec![],vec!["media".into()]);
        assert_eq!(result["status"],"held");
        assert_eq!(result["needsAttention"],json!([{"itemId":"media","reason":"Missing answer"}]));
        assert!(result["preparedItemIds"].as_array().unwrap().is_empty());
    }
}
