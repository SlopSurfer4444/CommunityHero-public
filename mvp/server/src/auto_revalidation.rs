//! Opt-in, continuous review of saved automatic decisions against changed evidence.
//! Claims consume a durable per-source attempt before calling a model. No dispatch.
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use crate::{ApiResult, list, row, row_mut};

const PURPOSE: &str = "auto_revalidate";
pub(super) fn validated_config(body: &Value) -> ApiResult<Value> {
    let fields=body.as_object().ok_or_else(||crate::bad("Preparation settings must be an object"))?;
    if fields.keys().any(|k| !["enabled","dailyLimit","itemDailyLimit","debounceSeconds"].contains(&k.as_str())) {
        return Err(crate::bad("Unknown preparation setting"));
    }
    let enabled=body["enabled"].as_bool().ok_or_else(||crate::bad("enabled must be a boolean"))?;
    // Old saved configurations may still send these two knobs. Validate their
    // shape, then discard them: a historical batch size is not a daily quota.
    for key in ["dailyLimit","itemDailyLimit"] {
        if body.get(key).is_some() && !body[key].as_u64().is_some_and(|value|value>0) {
            return Err(crate::bad(&format!("{key} must be a positive integer")));
        }
    }
    let debounce=if body.get("debounceSeconds").is_none(){120}else{
        body["debounceSeconds"].as_u64().filter(|value|(30..=3600).contains(value))
            .ok_or_else(||crate::bad("debounceSeconds is outside the allowed range"))?
    };
    Ok(json!({"enabled":enabled,"debounceSeconds":debounce}))
}
pub(super) fn status(d: &Value, now: i64) -> Value {
    let media_states=crate::media_queue::preparation_states(d,list(d,"items"),&super::stamp(now));
    let candidates=list(d,"items").iter().filter(|i| eligible_with_media(d,i,now,media_states.as_ref().ok().and_then(|s|s.get(i["id"].as_str().unwrap_or(""))).map_or(true,Option::is_some)) && source_candidate(d,i).is_some()).count();
    let group_review_required=list(d,"items").iter().filter(|i| eligibility_block(d,i,now,
        media_states.as_ref().ok().and_then(|s|s.get(i["id"].as_str().unwrap_or(""))).map_or(true,Option::is_some))==Some("group_review_required")).count();
    let mut value=status_summary(d,now);
    value["candidateCount"]=json!(candidates);
    value["groupReviewRequiredCount"]=json!(group_review_required);
    value.as_object_mut().unwrap().remove("eligibility");
    value
}
/// Observed counters and configuration only. This is never an admission preview:
/// no missing eligibility proof is converted into a zero or a permission.
pub(super) fn status_summary(d:&Value,now:i64)->Value {
    let raw=&d["settings"]["autoPreparation"]["revalidation"];
    let effective=json!({"enabled":raw["enabled"]==true,"debounceSeconds":raw["debounceSeconds"].as_i64().unwrap_or(120).clamp(30,3600)});
    // The summary projection contains aggregate workflow counters, not items.
    // Full status reaches this helper only after its unchanged list/eligibility
    // evaluation. A projection's absent items never stand for actual zero work.
    let workflow=if let Some(items)=d["items"].as_array(){json!({
        "prepared":items.iter().filter(|i|i["workflow"]=="prepared").count(),
        "needsAttention":items.iter().filter(|i|i["workflow"]=="attention").count(),
        "stale":items.iter().filter(|i|i["workflow"]=="attention"&&i["autoPreparation"]["status"]=="stale").count()})}
        else{d["maintenancePreparationWorkflow"].clone()};
    let recent:Vec<&Value>=list(d,"jobs").iter().filter(|j|j["purpose"]==PURPOSE && super::time(&j["claimedAt"]).is_some_and(|at|at>now-86400)).collect();
    let active:Vec<&Value>=list(d,"jobs").iter().filter(|j|j["kind"]=="assistant"
        && matches!(j["status"].as_str(),Some("running"|"queued"))).collect();
    let active_count=|purpose:&str|active.iter().filter(|j|j["purpose"]==purpose).count();
    let prepared=recent.iter().filter(|j|j["prepareOutcome"]["status"]=="prepared").count();
    let needs_attention=recent.iter().filter(|j|j["prepareOutcome"]["status"]=="needs_attention").count();
    let recent_running=recent.iter().filter(|j|matches!(j["status"].as_str(),Some("running"|"queued"))).count();
    // Preserve the old fields for API consumers, but explicitly identify their
    // historical scope. Current queue and lane counts never come from outcomes.
    json!({"configuration":effective,"mode":"continuous","candidateCount":null,"groupReviewRequiredCount":null,
        "eligibility":{"status":"not_evaluated","detailQuery":"eligibility=full"},"claimedLast24Hours":recent.len(),
        "observedAt":super::stamp(now),"legacyCountersScope":"revalidation_last_24_hours",
        "currentWorkflow":workflow,
        "activeJobs":{"initialPreparation":active_count("auto_prepare"),"revalidation":active_count(PURPOSE),
            "discussion":active_count("discussion"),
            "otherPreparation":active.iter().filter(|j|!matches!(j["purpose"].as_str(),Some("auto_prepare"|"auto_revalidate"|"discussion"))).count()},
        "revalidationLast24Hours":{"claimed":recent.len(),"prepared":prepared,"needsAttention":needs_attention,
            "running":recent_running,"failed":recent.iter().filter(|j|j["status"]=="failed").count()},
        "running":recent_running,"prepared":prepared,"needsAttention":needs_attention})
}
fn config(d: &Value) -> Option<i64> {
    let c = &d["settings"]["autoPreparation"]["revalidation"];
    (c["enabled"] == true).then(||c["debounceSeconds"].as_i64().unwrap_or(120).clamp(30, 3600))
}
fn eligible_with_media(d: &Value, item: &Value, now: i64, media_wait: bool) -> bool {
    eligibility_block(d,item,now,media_wait).is_none()
}
fn eligibility_block(d: &Value, item: &Value, now: i64, media_wait: bool) -> Option<&'static str> {
    if media_wait&&!crate::decision_media::may_assess(d,item).unwrap_or(false){return Some("media_prerequisite_unavailable");}
    if item["workflow"] != "attention" { return Some("workflow_changed"); }
    if !matches!(item["providerStatus"].as_str(), Some("new" | "inprogress")) { return Some("provider_status_changed"); }
    if !item["draft"].as_str().unwrap_or("").trim().is_empty() { return Some("operator_draft_present"); }
    if item["draftEdited"] == true { return Some("operator_draft_edited"); }
    if !item["autoPreparation"]["humanOverrideAt"].is_null() { return Some("human_override_present"); }
    if !matches!(item["autoPreparation"]["status"].as_str(), Some("stale" | "needs_attention")) { return Some("preparation_state_changed"); }
    if !super::valid_provider_observation(item,now) { return Some("provider_observation_invalid"); }
    let Ok(binding) = crate::active_binding(d) else { return Some("connector_binding_unavailable"); };
    if crate::bridge_account(&binding).is_err() || crate::bound_item(&binding, item).is_err() { return Some("item_binding_changed"); }
    if list(d,"proposals").iter().any(|p| p["itemId"] == item["id"]
        && matches!(p["status"].as_str(), Some("draft" | "approved" | "dispatching" | "unknown"))) { return Some("protected_proposal_present"); }
    if list(d,"operations").iter().any(|o| o["itemId"] == item["id"]
        && matches!(o["status"].as_str(), Some("dispatching" | "unknown"))) { return Some("protected_operation_present"); }
    if crate::preparation_restart::grouped_previous(d,item) {return Some("group_review_required");}
    None
}
fn previous(d: &Value, item: &Value) -> Option<(Value, Value)> {
    if crate::preparation_restart::fresh_context_requested(d,item) {
        restart_run(d,item)?;
        return crate::preparation_restart::fresh_context_previous(d,item);
    }
    let saved = item["autoPreparation"]["savedProposalId"].as_str()
        .and_then(|id| row(d,"proposals",id).ok());
    if saved.is_some_and(|p| p["status"] != "stale" || p["origin"].is_object()
        || p["history"].as_array().is_some_and(|h| !h.is_empty())) { return None; }
    if saved.and_then(|p|p["text"].as_str()).is_some_and(|text|text.encode_utf16().count()>12000) { return None; }
    let run = saved.and_then(|p| p["prepareRunId"].as_str().or_else(||p["recovery"]["prepareRunId"].as_str()))
        .or_else(||item["autoPreparation"]["jobId"].as_str())?;
    let job = row(d,"jobs",run).ok()?;
    let explicit_error = saved.is_none() && restart_run(d,item).is_some()
        && crate::preparation_restart::authorized_error_retry(d,item,job)
        && matches!(job["status"].as_str(),Some("completed"|"failed"))
        && job["refId"]==item["id"];
    let repaired_owner=if job["purpose"]=="answering_repair" {
        let proposal=saved?;
        let owner=crate::answering_repair_plan::automatic_proposal_origin(d,proposal)?;
        let root=row(d,"jobs",&owner).ok()?;
        if root["status"]!="completed"||item["autoPreparation"]["jobId"]!=owner{return None;}
        Some(owner)
    }else{None};
    if (!matches!(job["purpose"].as_str(), Some("auto_prepare" | "auto_revalidate"))&&repaired_owner.is_none())
        || (job["status"] != "completed" && !explicit_error) { return None; }
    let bundle=&job["prepareBundle"];
    if bundle["version"]!=1 || bundle["itemIds"]!=json!([item["id"]])
        || bundle["digest"]!=format!("{:x}",Sha256::digest(bundle["request"].to_string().as_bytes())) {return None;}
    if saved.is_some_and(|p| {
        let origin=if p["recovery"].is_object(){&p["recovery"]}else{p};
        origin["prepareBundleId"]!=bundle["id"] || origin["prepareBundleDigest"]!=bundle["digest"]
    }) {return None;}
    if explicit_error && (bundle["request"]["items"].as_array().is_none_or(|v|v.len()!=1||v[0]["id"]!=item["id"])
        || crate::prepare_bundle::current(d,bundle).is_err()) {return None;}
    if saved.is_none() && job["prepareOutcome"]["status"] != "needs_attention" && !explicit_error { return None; }
    let decision = json!({"itemId":item["id"],"proposalId":saved.map(|p|p["id"].clone()),
        "prepareRunId":run,"outcome":saved.map(|p|match p["kind"].as_str(){
            Some("reply_and_close")=>"reply",Some("close")=>"close",Some("hide")=>"hide",Some("delete")=>"delete",_=>"needs_attention"
        }).unwrap_or("needs_attention"),
        "text":saved.and_then(|p|p["text"].as_str()).unwrap_or(""),
        "reason":if explicit_error {job.get("error").unwrap_or(&job["prepareOutcome"]["reason"])} else {&job["prepareOutcome"]["reason"]},
        "sourceChangeReason":item["autoPreparation"]["sourceChangeReason"]});
    Some((job["prepareBundle"].clone(), decision))
}
// Explicit operator restart is separate from source truth. Only a durable run
// receipt which names this exact item may authorize an unchanged-source review.
fn restart_run<'a>(d: &'a Value, item: &'a Value) -> Option<&'a str> {
    let id=item["autoRevalidation"]["restartRunId"].as_str()?;
    d["preparationRuns"].as_array()?.iter().find(|r| r["runId"]==id
        && r["applied"]==true && r["itemIds"].as_array().is_some_and(|ids|ids.contains(&item["id"])))?;
    if list(d,"jobs").iter().any(|j| j["purpose"]==PURPOSE && j["refId"]==item["id"] && j["restartRunId"]==id) { return None; }
    Some(id)
}
// Shared by the status projection and the durable claim. Status must not count
// a source already consumed by a completed or failed attempt as runnable work.
fn source_candidate(d: &Value, item: &Value) -> Option<(Value, Option<String>, String)> {
    let id=item["id"].as_str()?;
    let (old_bundle,decision)=previous(d,item)?;
    let restart=restart_run(d,item).map(str::to_owned);
    if restart.is_none() && crate::prepare_bundle::current(d,&old_bundle).is_ok() { return None; }
    let source=crate::prepare_bundle::review_fingerprint(d,id).ok()?;
    if item["autoRevalidation"]["pendingDigest"]==source
        && item["autoRevalidation"]["jobId"].is_string() { return None; }
    if list(d,"jobs").iter().any(|j| j["purpose"]==PURPOSE && j["refId"]==id
        && match restart.as_deref() {
            Some(run)=>j["restartRunId"]==run,
            None=>j["sourceDigest"]==source,
        }) { return None; }
    Some((decision,restart,source))
}
fn candidates(d: &Value, now: i64) -> ApiResult<Vec<Value>> {
    let media_states=crate::media_queue::preparation_states(d,list(d,"items"),&super::stamp(now))?;
    Ok(list(d,"items").iter().filter(|item| eligible_with_media(d,item,now,
        media_states.get(item["id"].as_str().unwrap_or("")).map_or(true,Option::is_some))).cloned().collect())
}
// Pure admission preview shared by fairness and the exclusive claim. Merely
// stale/settling or permanently blocked work must not drain healthy workers.
fn ready_capture(d: &Value, item: &Value, now: i64, debounce: i64) -> Option<(Value, Option<String>, String)> {
    let id=item["id"].as_str()?;
    let (decision,restart,source)=source_candidate(d,item)?;
    if item["autoRevalidation"]["pendingDigest"] != source
        || !super::time(&item["autoRevalidation"]["observedAt"]).is_some_and(|at|at<=now-debounce) {return None;}
    // A previous repaired decision truthfully names its paid child. Reservation
    // ownership is separately derived from the proven original workflow owner.
    let previous_owner=if row(d,"jobs",decision["prepareRunId"].as_str()?).ok()?["purpose"]=="answering_repair" {
        let proposal=row(d,"proposals",decision["proposalId"].as_str()?).ok()?;
        crate::answering_repair_plan::automatic_proposal_origin(d,proposal)?
    }else{decision["prepareRunId"].as_str()?.to_owned()};
    crate::preparation_reservations::assert_available(d,&[id.to_owned()],Some(&previous_owner)).ok()?;
    let mut bundle=crate::prepare_bundle::triage(d,id).ok()?;
    bundle["request"]["previousDecision"]=decision;
    bundle["digest"]=json!(format!("{:x}",Sha256::digest(bundle["request"].to_string().as_bytes())));
    Some((bundle,restart,source))
}
pub(super) fn ready(d: &Value, now: i64) -> ApiResult<bool> {
    let Some(debounce)=config(d) else {return Ok(false);};
    Ok(candidates(d,now)?.iter().any(|item|ready_capture(d,item,now,debounce).is_some()))
}
pub(super) fn observe(d: &mut Value, now: i64) -> ApiResult<()> {
    let interrupted: Vec<(String,String)> = list(d,"items").iter().filter_map(|i| {
        if i["autoRevalidation"]["status"] != "running" { return None; }
        let job = i["autoRevalidation"]["jobId"].as_str()?;
        let status = row(d,"jobs",job).ok().and_then(|j|j["status"].as_str()).unwrap_or("missing");
        (!matches!(status,"running"|"queued")).then(||(i["id"].as_str().unwrap_or("").to_owned(),job.to_owned()))
    }).collect();
    for (id, _) in interrupted {
        let state = &mut row_mut(d,"items",&id)?["autoRevalidation"];
        state["status"] = json!("held");
        state["reason"] = json!("Перепроверка прервана. Сохранённое решение оставлено; автоматический повтор не запускается.");
        state["finishedAt"] = json!(super::stamp(now));
    }
    if config(d).is_none(){return Ok(());}
    let candidates=candidates(d,now)?;
    // Observe stability even while independent initial work is active. Otherwise
    // continuous refill can prevent the debounce clock from ever starting.
    for item in &candidates {
        let Some(id) = item["id"].as_str() else { continue; };
        let Some((_,restart,source)) = source_candidate(d,item) else { continue; };
        if item["autoRevalidation"]["pendingDigest"] != source {
            row_mut(d,"items",id)?["autoRevalidation"] = json!({"status":"settling","pendingDigest":source,"observedAt":super::stamp(now),"restartRunId":restart});
        }
    }
    Ok(())
}
pub(super) fn claim(d: &mut Value, now: i64) -> ApiResult<Option<(String, Value)>> {
    observe(d,now)?;
    if d["settings"]["autoPreparation"]["continuousPreparation"].is_object()
        && crate::continuous_preparation::admission_reason(d).is_some(){return Ok(None);}
    let Some(debounce)=config(d) else{return Ok(None);};
    let candidates=candidates(d,now)?;
    if list(d,"jobs").iter().any(|j| j["kind"]=="assistant" && j["purpose"]!="discussion"
        && matches!(j["status"].as_str(),Some("running"|"queued"))) {return Ok(None);}
    for item in candidates {
        let Some((bundle,restart,source))=ready_capture(d,&item,now,debounce) else {continue;};
        let id=item["id"].as_str().expect("captured review recipient");
        let request = bundle["request"].clone();
        let job = crate::new_job(d,"assistant",id)?;
        let stored = row_mut(d,"jobs",&job)?;
        stored["purpose"] = json!(PURPOSE);
        stored["prepareBundle"] = bundle;
        stored["sourceDigest"] = json!(source);
        if let Some(run)=restart {stored["restartRunId"]=json!(run);}
        stored["claimedAt"] = json!(super::stamp(now));
        let reservation=crate::preparation_reservations::capture(d,&job)?;
        crate::preparation_reservations::check(d,&reservation,Some(&job))?;
        row_mut(d,"jobs",&job)?["scopeReservation"]=reservation;
        let state = &mut row_mut(d,"items",id)?["autoRevalidation"];
        state["status"] = json!("running");
        state["jobId"] = json!(job);
        state["startedAt"] = json!(super::stamp(now));
        if crate::continuous_preparation::enabled(d){
            let parent=request["previousDecision"]["prepareRunId"].as_str().filter(|id|row(d,"jobs",id)
                .is_ok_and(|j|j["continuousPreparationOrigin"]["kind"]=="continuous_background")).map(str::to_owned);
            crate::continuous_preparation::stamp_background_job(d,&job,parent.as_deref())?;
        }
        return Ok(Some((job,request)));
    }
    Ok(None)
}
pub(super) fn failed(d: &mut Value, job_id: &str, reason: &str, now: i64) -> ApiResult<()> {
    let job = row(d,"jobs",job_id)?.clone();
    let item = row_mut(d,"items",crate::required(&job,"refId")?)?;
    if item["autoRevalidation"]["jobId"] == job_id {
        item["autoRevalidation"]["status"] = json!("held");
        item["autoRevalidation"]["reason"] = json!(reason);
        item["autoRevalidation"]["finishedAt"] = json!(super::stamp(now));
    }
    Ok(())
}
pub(super) fn complete(d: &mut Value, job_id: &str, result: &Value, now: i64) -> ApiResult<Value> {
    let job = row(d,"jobs",job_id)?.clone();
    let id = crate::required(&job,"refId")?;
    let item = row(d,"items",id)?;
    let rejection=if job["status"]!="running" {Some(("job_not_running",None))}
        else if item["autoRevalidation"]["jobId"]!=job_id {Some(("review_job_pointer_changed",None))}
        else if let Some(code)=eligibility_block(d,item,now,super::waiting_for_media(d,item,now)) {Some((code,None))}
        else if let Err(detail)=crate::prepare_bundle::current(d,&job["prepareBundle"]) {Some(("bundle_no_longer_current",Some(detail)))}
        else {match crate::prepare_bundle::review_fingerprint(d,id) {
            Ok(source) if job["sourceDigest"]==source=>None,
            Ok(_)=>Some(("source_fingerprint_changed",None)),
            Err(detail)=>Some(("source_fingerprint_unavailable",Some(detail))),
        }};
    if let Some((code,detail))=rejection {
        failed(d,job_id,"Контекст или черновик изменился во время перепроверки",now)?;
        let outcome=json!({"status":"stale","itemId":id,"reason":"Revalidation source or operator draft changed",
            "rejectionCode":code,"rejectionDetail":detail});
        row_mut(d,"jobs",job_id)?["prepareOutcome"]=outcome.clone();
        return Ok(outcome);
    }
    // Reuse the full initial-preparation admission contract on a staged copy.
    // A failed admission cannot wipe or relabel the operator's saved candidate.
    let mut staged = d.clone();
    let auto = &mut row_mut(&mut staged,"items",id)?["autoPreparation"];
    *auto = json!({"status":"running","jobId":job_id,"attempts":1,"requiresReview":false,
        "inputDigest":job["prepareBundle"]["dependencyDigest"],"updatedAt":super::stamp(now)});
    let outcome = super::complete_initial(&mut staged,job_id,result,now)?;
    if !matches!(outcome["status"].as_str(),Some("prepared"|"needs_attention")) {
        failed(d,job_id,"Перепроверка не принята; сохранённое решение оставлено",now)?;
        row_mut(d,"jobs",job_id)?["prepareOutcome"]=outcome.clone();
        return Ok(outcome);
    }
    // A successful review that still needs a person is durable too. Keep the
    // initial-preparation/legacy-recovery path from launching or reviving it.
    row_mut(&mut staged,"items",id)?["autoPreparation"]["requiresReview"] = json!(outcome["status"]=="needs_attention");
    let state=&mut row_mut(&mut staged,"items",id)?["autoRevalidation"];
    state["status"]=json!("completed");
    state["finishedAt"]=json!(super::stamp(now));
    *d=staged;
    Ok(outcome)
}

/// Revalidation repair keeps the original workflow owner and paid result. The
/// admitted child's new revision cannot be checked against the original bundle.
#[derive(Clone,Copy)]
pub(crate) enum RepairPhase { BeforeAdmission, AfterAdmission }
pub(crate) fn repair_current(d:&Value,origin:&str,affected:&[Value],phase:RepairPhase)->ApiResult<()> {
    let root=row(d,"jobs",origin)?;let bundle=&root["prepareBundle"];let request=&bundle["request"];
    let id=crate::required(root,"refId")?;let item=row(d,"items",id)?;let binding=crate::active_binding(d)?;
    if root["kind"]!="assistant"||root["purpose"]!=PURPOSE||root.get("originatingAnsweringAttemptId").is_some()
        ||!matches!(root["status"].as_str(),Some("running"|"completed"|"failed"|"interrupted"))
        ||root["preparationStages"]["first"]["status"]!="completed"||bundle["version"]!=1
        ||bundle["itemIds"]!=json!([id])||crate::list(request,"items").len()!=1||request["items"][0]["id"]!=id
        ||affected.len()!=1||affected[0]!=id||bundle["digest"]!=crate::preparation_materials::hash(request)
        ||request["account"]!=d["account"]||request["connectorBinding"]!=binding.to_json()
        ||root["prepareOutcome"]["itemId"]!=id||!root["prepareOutcome"]["admission"].is_object()
        ||item["autoPreparation"]["jobId"]!=origin||item["autoPreparation"]["attempts"]!=1||item["autoPreparation"]["inputDigest"]!=bundle["dependencyDigest"]||item["autoRevalidation"]["jobId"]!=origin
        ||item["autoRevalidation"]["status"]!="completed"||!root["sourceDigest"].is_string()
        ||item["autoRevalidation"]["pendingDigest"]!=root["sourceDigest"] {
        return Err(crate::conflict("Repair revalidation singleton ownership changed"));
    }
    let original_receipt=crate::model_material_receipt::result_receipt(request,&root["preparationStages"]["first"]["result"])
        .map_err(crate::conflict)?.ok_or_else(||crate::conflict("Repair revalidation original paid material receipt missing"))?;
    if original_receipt["nativeJobId"]!=origin||!crate::list(root,"modelMaterialReceipts").contains(&original_receipt)
        ||!crate::list(root,"retainedEvidence").contains(&original_receipt["paidResultRef"]){
        return Err(crate::conflict("Repair revalidation original paid closure changed"));
    }
    if crate::list(root,"videoFrameNeeds").is_empty()||root["videoFrameNeeds"]!=root["preparationStages"]["first"]["result"]["nativeVideoFrameNeeds"]
        ||crate::list(root,"videoFrameNeeds").iter().any(|need|need["originatingAnsweringAttemptId"]!=origin||need["requestingPaidAttemptId"]!=origin
            ||need["parentPaidResultRef"]!=original_receipt["paidResultRef"]||crate::video_frame_work::current_need(d,need).is_err()){
        return Err(crate::conflict("Repair revalidation original frame need lineage changed"));
    }
    crate::bound_item(&binding,item)?;
    if item["draftEdited"]==true||!item["autoPreparation"]["humanOverrideAt"].is_null()
        ||!item["draft"].as_str().unwrap_or("").trim().is_empty()
        ||!matches!(item["providerStatus"].as_str(),Some("new"|"inprogress"))
        ||!super::valid_provider_observation(item,chrono::Utc::now().timestamp())
        ||root["sourceDigest"]!=crate::prepare_bundle::review_fingerprint(d,id).map_err(crate::conflict)? {
        return Err(crate::conflict("Repair revalidation source or operator decision changed"));
    }
    let completed=match phase {
        RepairPhase::BeforeAdmission=>{
            if root["prepareOutcome"]["status"]!="needs_attention"||item["autoPreparation"]["status"]!="needs_attention"
                ||item["autoPreparation"]["requiresReview"]!=true||item["workflow"]!="attention"||!crate::list(&root["prepareOutcome"]["admission"],"candidates").is_empty(){
                return Err(crate::conflict("Repair revalidation original held decision changed"));
            }None
        },
        RepairPhase::AfterAdmission=>{
            let expected=if root["repairMergeReceipt"].is_object(){root["prepareOutcome"]["status"].as_str().unwrap_or("")}else{"needs_attention"};
            if !matches!(expected,"needs_attention"|"prepared")||item["autoPreparation"]["status"]!=expected
                ||item["autoPreparation"]["requiresReview"]!=json!(expected!="prepared"){
                return Err(crate::conflict("Repair revalidation preparation state changed"));
            }
            let child=crate::answering_repair_plan::settled_child(d,origin)?.ok_or_else(||crate::conflict("Repair revalidation settled child missing"))?;
            let ids=vec![id.to_owned()];
            crate::preparation_reservations::assert_repair_projection(d,origin,crate::required(child,"id")?,&ids)?;
            Some(child)
        },
    };
    for proposal in crate::list(d,"proposals").iter().filter(|p|p["itemId"]==id
        &&matches!(p["status"].as_str(),Some("draft"|"approved"|"dispatching"|"unknown"))) {
        let own=completed.is_some_and(|child|proposal["status"]=="draft"&&proposal["prepareRunId"]==child["id"]
            &&crate::list(&child["prepareOutcome"],"candidates").iter().any(|c|c["proposalId"]==proposal["id"]&&c["itemId"]==id));
        if !own{return Err(crate::conflict("Repair revalidation protected proposal present"));}
    }
    if crate::list(d,"operations").iter().any(|o|o["itemId"]==id&&!matches!(o["status"].as_str(),Some("succeeded"|"failed"|"stale"))){
        return Err(crate::conflict("Repair revalidation unresolved operation present"));
    }
    Ok(())
}
pub(crate) fn merge_repair_projection(d:&mut Value,origin:&str,mut outcome:Value,repaired:&Value)->ApiResult<Value>{
    let affected=crate::list(repaired,"affectedRecipientIds");repair_current(d,origin,affected,RepairPhase::AfterAdmission)?;
    let id=affected[0].as_str().ok_or_else(||crate::conflict("Repair revalidation recipient missing"))?;
    if outcome["itemId"]!=id||!outcome["admission"].is_object()||outcome["status"]!="needs_attention" {
        return Err(crate::conflict("Repair revalidation original outcome changed"));
    }
    let assessments=crate::list(repaired,"finalAssessments");let candidates=crate::list(repaired,"candidates");
    if assessments.len()!=1||assessments[0]["itemId"]!=id||candidates.len()>1||candidates.iter().any(|c|c["itemId"]!=id||c["status"]!="review"){
        return Err(crate::conflict("Repair revalidation assessment coverage changed"));
    }
    let assessment=&assessments[0];let tags=super::assessment_tags(assessment)?;
    let reason=assessment["reason"].as_str().filter(|s|!s.trim().is_empty()&&s.len()<=12000).ok_or_else(||crate::conflict("Repair revalidation reason missing"))?;
    let prepared=if let Some(candidate)=candidates.first(){
        let proposal=row(d,"proposals",crate::required(candidate,"proposalId")?)?;let item=row(d,"items",id)?;
        let expected=match assessment["outcome"].as_str(){Some("reply")=>"reply_and_close",Some("close")=>"close",Some("hide")=>"hide",Some("delete")=>"delete",_=>return Err(crate::conflict("Repair revalidation action disagrees"))};
        if proposal["prepareRunId"]!=repaired["repairJobId"]||proposal["itemId"]!=id||proposal["kind"]!=expected
            ||proposal["itemRevision"]!=item["revision"]||item["workflow"]!="prepared"||proposal["status"]!="draft" {
            return Err(crate::conflict("Repair revalidation proposal no longer owns its saved draft"));
        }true
    }else{if assessment["outcome"]!="needs_attention"||row(d,"items",id)?["workflow"]!="attention"{return Err(crate::conflict("Repair revalidation held decision changed"));}false};
    crate::answering_repair_plan::merge_admission(&mut outcome["admission"],repaired);
    let status=if prepared{"prepared"}else{"needs_attention"};let at=crate::now();let item=row_mut(d,"items",id)?;
    item["autoPreparation"]["status"]=json!(status);item["autoPreparation"]["requiresReview"]=json!(!prepared);
    item["autoPreparation"]["reason"]=json!(reason);item["autoPreparation"]["retryAt"]=Value::Null;
    item["autoPreparation"]["repairJobId"]=repaired["repairJobId"].clone();item["autoPreparation"]["updatedAt"]=json!(at);
    item["autoRevalidation"]["repairJobId"]=repaired["repairJobId"].clone();item["autoRevalidation"]["finishedAt"]=json!(at);
    item["decision"]=assessment["outcome"].clone();item["reason"]=json!(reason);item["triageTags"]=tags;
    outcome["status"]=json!(status);outcome["reason"]=json!(reason);outcome["repairJobId"]=repaired["repairJobId"].clone();Ok(outcome)
}
#[cfg(test)]
mod tests {
    use super::*;
    use super::super::tests::{captured_result, complete_with_captured_result, legacy_two_pass_request};
    const NOW:i64=1_790_000_000;
    fn response()->Value {json!({"text":"Reviewed","sources":[],"assessments":[{"itemId":"i","outcome":"reply","reason":"Evidence supports this reply"}],"proposals":[{"itemId":"i","kind":"reply_and_close","text":"Сохранённый ответ"}]})}
    fn initial_fixture()->Value {
        // The shared fixture binds its own pristine synthetic account and
        // declares known-empty attachments before any preparation capture.
        let mut d=super::super::tests::fixture();
        d["items"][0]["createdAt"]=json!(super::super::stamp(NOW-60));
        d["items"][0]["providerObservedAt"]=json!(super::super::stamp(NOW));
        d
    }
    fn held()->Value {
        let mut d=initial_fixture();
        let (job,_)=super::super::claim(&mut d,NOW).unwrap().unwrap();
        assert_eq!(complete_with_captured_result(&mut d,&job,&response(),NOW).unwrap()["status"],"prepared");
        crate::proposal_current(&d,&d["proposals"][0]).expect("held fixture starts with current material provenance");
        row_mut(&mut d,"jobs",&job).unwrap()["status"]=json!("completed");
        d["materials"].as_array_mut().unwrap().push(json!({"id":"video","kind":"transcript","postKey":"p","text":"New evidence"}));
        super::super::reconcile_stale(&mut d,NOW+1);
        d
    }
    fn enabled(d:&mut Value){d["settings"]["autoPreparation"]=json!({"revalidation":{"enabled":true,"debounceSeconds":30}});}
    #[test]
    fn old_observation_revalidation_keeps_semantic_gates_without_age_expiry() {
        for age in [601,3600] {
            let mut d=held();enabled(&mut d);
            let observed=super::super::stamp(NOW-age);d["items"][0]["providerObservedAt"]=json!(observed);
            assert!(claim(&mut d,NOW+1).unwrap().is_none());
            let (job,_)=claim(&mut d,NOW+33).unwrap().unwrap();
            assert_eq!(complete_with_captured_result(&mut d,&job,&response(),NOW+3633).unwrap()["status"],"prepared");
            assert_eq!(d["items"][0]["providerObservedAt"],observed);
            assert!(list(&d,"approvals").is_empty());assert!(list(&d,"operations").is_empty());
        }
        for observation in [Value::Null,json!("bad-time"),json!(super::super::stamp(NOW+94))] {
            let mut d=held();enabled(&mut d);d["items"][0]["providerObservedAt"]=observation;
            let jobs=d["jobs"].clone();
            assert!(claim(&mut d,NOW+1).unwrap().is_none());assert!(claim(&mut d,NOW+33).unwrap().is_none());
            assert_eq!(d["jobs"],jobs);
        }
    }
    #[test]
    fn historical_group_does_not_hold_current_singleton_decision() {
        let mut d=held();enabled(&mut d);
        d["jobs"].as_array_mut().unwrap().insert(0,json!({"id":"old-group","purpose":"auto_prepare","status":"completed","refId":"other","prepareBundle":{"itemIds":["other","i"]}}));
        d["proposals"].as_array_mut().unwrap().insert(0,json!({"id":"old-proposal","itemId":"i","status":"stale","prepareRunId":"old-group"}));
        assert!(!crate::preparation_restart::grouped_previous(&d,&d["items"][0]));
        let ids=vec!["i".to_owned()];
        let preview=crate::preparation_restart::plan_scoped(&mut d,"current-only",false,NOW+1,Some(&ids)).unwrap();
        assert_eq!(preview["eligibleCount"],1);
        assert!(claim(&mut d,NOW+1).unwrap().is_none());
        assert!(claim(&mut d,NOW+33).unwrap().is_some());
        assert_eq!(d["proposals"][0]["id"],"old-proposal");
    }
    fn add_initial(d:&mut Value,number:usize) {
        let mut item=d["items"][0].clone();
        item["id"]=json!(format!("fresh-{number}"));item["itemId"]=json!(format!("fresh-comment-{number}"));
        // These fairness fixtures represent independently ready posts; a shared
        // post now correctly claims its comments together in a single job.
        let post=format!("fresh-post-{number}");let branch=format!("fresh-branch-{number}");
        item["postId"]=json!(post);item["postKey"]=json!(post);item["branchId"]=json!(branch);
        item["conversationKey"]=json!(branch);
        d["posts"].as_array_mut().unwrap().push(json!({"id":post,"text":"Independent post","attachments":[]}));
        d["branches"].as_array_mut().unwrap().push(json!({"id":branch,"postId":post,"messages":[{"id":item["itemId"],"text":"Hi"}],"contextComplete":false}));
        item["workflow"]=json!("attention");item["draft"]=json!("");
        for key in ["autoPreparation","autoRevalidation","decision","reason","preparationMediaWait"] {item.as_object_mut().unwrap().remove(key);}
        d["items"].as_array_mut().unwrap().push(item);
    }
    fn finish_automatic(d:&mut Value,job:&str,at:i64) {
        let ids=row(d,"jobs",job).unwrap()["prepareBundle"]["itemIds"].as_array().unwrap().clone();
        let mut answer=response();
        answer["assessments"]=json!(ids.iter().map(|id|json!({"itemId":id,"outcome":"reply",
            "reason":"Evidence supports this reply"})).collect::<Vec<_>>());
        answer["proposals"]=json!(ids.iter().map(|id|json!({"itemId":id,"kind":"reply_and_close",
            "text":"Сохранённый ответ"})).collect::<Vec<_>>());
        assert_eq!(complete_with_captured_result(d,job,&answer,at).unwrap()["status"],"prepared");
        row_mut(d,"jobs",job).unwrap()["status"]=json!("completed");
    }
    #[test]
    fn fairness_alternates_ready_classes_using_durable_admission_history() {
        let mut d=held();enabled(&mut d);
        add_initial(&mut d,0);
        assert!(claim(&mut d,NOW+1).unwrap().is_none()); // establish review stability window
        let mut at=NOW+32;
        for round in 0..6 {
            // Serialization simulates restarting with only committed job history.
            d=serde_json::from_str(&d.to_string()).unwrap();
            let (job,_)=super::super::claim(&mut d,at).unwrap().unwrap();
            let expected=if round%2==0 {"auto_revalidate"}else{"auto_prepare"};
            assert_eq!(row(&d,"jobs",&job).unwrap()["purpose"],expected,"round {round}");
            assert!(super::super::claim(&mut d,at+1).unwrap().is_none(),"one active lane");
            finish_automatic(&mut d,&job,at+1);
            if expected=="auto_revalidate" {
                d["materials"][0]["text"]=json!(format!("New evidence {round}"));
                super::super::reconcile_stale(&mut d,at+2);
                assert!(claim(&mut d,at+2).unwrap().is_none());
                if round<5 {add_initial(&mut d,round/2+1);}
            }
            at+=33;
        }
        assert!(list(&d,"operations").is_empty()&&list(&d,"approvals").is_empty());
    }
    #[test]
    fn parallel_refill_observes_review_stability_then_drains_without_starvation() {
        let mut d=held();enabled(&mut d);add_initial(&mut d,0);add_initial(&mut d,1);add_initial(&mut d,2);
        let next=|d:&mut Value,at| {
            super::super::reconcile_claim_state(d,at).unwrap();
            super::super::claim_reconciled(d,at,None,2).unwrap()
        };
        let (first,_)=next(&mut d,NOW+1).unwrap();
        assert_eq!(row(&d,"jobs",&first).unwrap()["purpose"],"auto_prepare");
        assert_eq!(d["items"][0]["autoRevalidation"]["status"],"settling");
        let (second,_)=next(&mut d,NOW+2).expect("settling review does not idle the second worker");
        let snapshot=d.clone();
        assert!(!ready(&d,NOW+30).unwrap());assert!(ready(&d,NOW+31).unwrap());assert_eq!(d,snapshot,"preview is pure");
        finish_automatic(&mut d,&first,NOW+32);
        add_initial(&mut d,3);
        assert!(next(&mut d,NOW+33).is_none(),"stable review drains instead of refilling the free slot");
        assert!(row(&d,"items","fresh-2").unwrap()["autoPreparation"]["jobId"].is_null());
        finish_automatic(&mut d,&second,NOW+34);
        let (review,_)=next(&mut d,NOW+35).expect("review gets the next turn once existing owners finish");
        assert_eq!(row(&d,"jobs",&review).unwrap()["purpose"],PURPOSE);
        assert!(list(&d,"operations").is_empty()&&list(&d,"approvals").is_empty());
    }
    #[test]
    fn ineligible_review_does_not_drain_parallel_preparation() {
        for block in ["disabled","operator_draft","unknown_operation","source_changed"] {
            let mut d=held();enabled(&mut d);add_initial(&mut d,0);add_initial(&mut d,1);
            assert!(claim(&mut d,NOW+1).unwrap().is_none());
            let (first,_)=super::super::claim_reconciled(&mut d,NOW+2,None,2).unwrap().unwrap();
            match block {
                "disabled"=>d["settings"]["autoPreparation"]["revalidation"]["enabled"]=json!(false),
                "operator_draft"=>d["items"][0]["draft"]=json!("Operator text"),
                "unknown_operation"=>d["operations"].as_array_mut().unwrap().push(json!({"id":"uncertain","itemId":"i","status":"unknown"})),
                _=>d["materials"][0]["text"]=json!("Changed again while settling"),
            }
            assert!(!ready(&d,NOW+32).unwrap(),"{block}");
            let (second,_)=super::super::claim_reconciled(&mut d,NOW+32,None,2).unwrap().unwrap();
            assert_ne!(first,second);assert_eq!(row(&d,"jobs",&second).unwrap()["purpose"],"auto_prepare","{block}");
        }
    }
    #[test]
    fn fairness_blocked_review_never_idles_initial_work_or_bypasses_protections() {
        for block in ["settling","disabled","invalid_provider","operator_draft","protected_proposal"] {
            let mut d=held();enabled(&mut d);add_initial(&mut d,1);
            match block {
                "disabled"=>d["settings"]["autoPreparation"]["revalidation"]["enabled"]=json!(false),
                "invalid_provider"=>d["items"][0]["providerObservedAt"]=json!(super::super::stamp(NOW+94)),
                "operator_draft"=>d["items"][0]["draft"]=json!("Preserve operator draft"),
                "protected_proposal"=>d["proposals"].as_array_mut().unwrap().push(json!({"id":"protected","itemId":"i","status":"unknown"})),
                _=>{}
            }
            let before=d["items"][0]["draft"].clone();
            let (job,_)=super::super::claim(&mut d,NOW+1).unwrap().unwrap();
            assert_eq!(row(&d,"jobs",&job).unwrap()["purpose"],"auto_prepare","{block}");
            assert_eq!(row(&d,"jobs",&job).unwrap()["refId"],"fresh-1","{block}");
            assert_eq!(d["items"][0]["draft"],before);
        }
    }
    #[test]
    fn fairness_active_unknown_or_queued_job_keeps_the_single_lane() {
        for purpose in [None,Some("auto_prepare"),Some("auto_revalidate")] {
            for status in ["queued","running"] {
                let mut d=held();enabled(&mut d);add_initial(&mut d,1);
                let mut active=json!({"id":"occupied","kind":"assistant","status":status});
                if let Some(purpose)=purpose {active["purpose"]=json!(purpose);}
                d["jobs"].as_array_mut().unwrap().push(active);
                let count=list(&d,"jobs").len();
                assert!(super::super::claim(&mut d,NOW+32).unwrap().is_none());
                assert_eq!(list(&d,"jobs").len(),count);
            }
        }
    }
    #[test]
    fn fairness_failed_review_yields_initial_without_repeating_consumed_source() {
        let mut d=held();enabled(&mut d);add_initial(&mut d,1);add_initial(&mut d,2);
        // held() retains the original completed preparation that produced the
        // stale candidate. The fairness pass adds two independent family jobs.
        let original_initial_jobs=list(&d,"jobs").iter().filter(|j|j["purpose"]=="auto_prepare").count();
        assert_eq!(original_initial_jobs,1);
        assert!(claim(&mut d,NOW+1).unwrap().is_none());
        let (review,_)=super::super::claim(&mut d,NOW+32).unwrap().unwrap();
        assert_eq!(row(&d,"jobs",&review).unwrap()["purpose"],PURPOSE);
        failed(&mut d,&review,"ASSISTANT_INVALID_RESEARCH",NOW+33).unwrap();
        row_mut(&mut d,"jobs",&review).unwrap()["status"]=json!("failed");
        let (initial,_)=super::super::claim(&mut d,NOW+34).unwrap().unwrap();
        assert_eq!(row(&d,"jobs",&initial).unwrap()["purpose"],"auto_prepare");
        assert_eq!(row(&d,"jobs",&initial).unwrap()["prepareBundle"]["itemIds"],json!(["fresh-1"]),
            "initial preparation yields to the first ready family without mixing independent posts");
        assert!(super::super::claim(&mut d,NOW+35).unwrap().is_none(),"one active lane");
        finish_automatic(&mut d,&initial,NOW+35);
        let (next,_)=super::super::claim(&mut d,NOW+36).unwrap().unwrap();
        assert_eq!(row(&d,"jobs",&next).unwrap()["purpose"],"auto_prepare");
        assert_eq!(row(&d,"jobs",&next).unwrap()["prepareBundle"]["itemIds"],json!(["fresh-2"]),
            "later independent family remains queued and consumed sources are not repeated");
        finish_automatic(&mut d,&next,NOW+37);
        assert!(super::super::claim(&mut d,NOW+38).unwrap().is_none());
        assert_eq!(list(&d,"jobs").iter().filter(|j|j["purpose"]==PURPOSE).count(),1);
        assert_eq!(list(&d,"jobs").iter().filter(|j|j["purpose"]=="auto_prepare").count(),original_initial_jobs+2);
        assert_eq!(d["items"][0]["autoRevalidation"]["status"],"held");
        assert!(list(&d,"operations").is_empty()&&list(&d,"approvals").is_empty());
    }
    #[test]
    fn status_distinguishes_current_workflow_all_active_jobs_and_legacy_outcomes() {
        let mut d=held();enabled(&mut d);
        d["jobs"].as_array_mut().unwrap().extend([
            json!({"id":"old-success","kind":"assistant","purpose":PURPOSE,"status":"completed","claimedAt":super::super::stamp(NOW-10),"prepareOutcome":{"status":"prepared"}}),
            json!({"id":"initial","kind":"assistant","purpose":"auto_prepare","status":"running"}),
            json!({"id":"unknown","kind":"assistant","status":"queued"}),
            json!({"id":"discussion","kind":"assistant","purpose":"discussion","status":"running"}),
            json!({"id":"old-active-review","kind":"assistant","purpose":PURPOSE,"status":"running","claimedAt":super::super::stamp(NOW-90000)})
        ]);
        let value=status(&d,NOW);
        assert_eq!(value["prepared"],1);assert_eq!(value["running"],0);
        assert_eq!(value["currentWorkflow"],json!({"prepared":0,"needsAttention":1,"stale":1}));
        assert_eq!(value["activeJobs"],json!({"initialPreparation":1,"revalidation":1,"otherPreparation":1,"discussion":1}));
        assert_eq!(value["revalidationLast24Hours"],json!({"claimed":1,"prepared":1,"needsAttention":0,"running":0,"failed":0}));
        assert_eq!(value["legacyCountersScope"],"revalidation_last_24_hours");
    }
    #[test]
    fn stale_copy_refreshes_old_manual_only_claim_without_promising_a_retry() {
        let mut d=held();
        let proposals=d["proposals"].clone();let jobs=d["jobs"].clone();
        let old="Сохранённое решение требует проверки. Повторная подготовка запускается оператором.";
        d["items"][0]["autoPreparation"]["reason"]=json!(old);d["items"][0]["reason"]=json!(old);
        super::super::reconcile_stale(&mut d,NOW+2);
        let reason=d["items"][0]["reason"].as_str().unwrap();
        assert!(reason.contains("текст сохранён"));assert!(!reason.contains("запускается оператором"));
        assert_eq!(d["items"][0]["autoPreparation"]["requiresReview"],true);
        assert_eq!(d["proposals"],proposals);assert_eq!(d["jobs"],jobs);
        let once=d.clone();super::super::reconcile_stale(&mut d,NOW+3);assert_eq!(d,once);
    }
    fn start(d:&mut Value)->(String,Value){enabled(d);assert!(claim(d,NOW+1).unwrap().is_none());claim(d,NOW+32).unwrap().unwrap()}
    #[test]
    fn opt_in_stability_delay_preserves_history_and_requires_current_provenance() {
        let mut d=held();let before=d.clone();
        assert!(claim(&mut d,NOW+1).unwrap().is_none());assert_eq!(d,before);
        let old=d["proposals"][0].clone();let (job,request)=start(&mut d);
        assert_eq!(request["previousDecision"]["text"],old["text"]);
        assert_eq!(request["messages"],json!([]));
        assert_eq!(d["proposals"][0],old);
        assert_eq!(d["items"][0]["autoPreparation"]["status"],"stale");
        assert!(crate::prepare_bundle::current(&d,&row(&d,"jobs",&job).unwrap()["prepareBundle"]).is_ok());
        let result=complete_with_captured_result(&mut d,&job,&response(),NOW+33).unwrap();
        assert_eq!(result["status"],"prepared");assert_eq!(d["proposals"][0],old);
        assert_eq!(d["proposals"].as_array().unwrap().len(),2);
        assert_eq!(d["proposals"][1]["prepareRunId"],job);
        assert!(crate::proposal_current(&d,&d["proposals"][1]).is_ok());
        assert!(d["approvals"].as_array().unwrap().is_empty());assert!(d["operations"].as_array().unwrap().is_empty());
    }
    #[test]
    fn personal_discussion_allows_review_but_changed_source_still_rejects_its_result() {
        let mut d=held();enabled(&mut d);
        let old=d["proposals"][0].clone();
        d["jobs"].as_array_mut().unwrap().push(json!({"id":"chat","kind":"assistant","purpose":"discussion","refId":"i","status":"running"}));
        assert!(claim(&mut d,NOW+1).unwrap().is_none());
        assert_eq!(d["items"][0]["autoRevalidation"]["status"],"settling");
        let (job,request)=claim(&mut d,NOW+32).unwrap().unwrap();
        assert_eq!(request["previousDecision"]["proposalId"],old["id"]);
        assert_eq!(d["proposals"][0],old);
        assert_eq!(row(&d,"jobs","chat").unwrap()["status"],"running");
        let captured=captured_result(&mut d,&job,&response()).unwrap();
        d["branches"][0]["messages"][0]["text"]=json!("Changed source during review");
        let result=super::super::complete(&mut d,&job,&captured,NOW+33).unwrap();
        assert_eq!(result["status"],"stale");
        assert_eq!(result["rejectionCode"],"bundle_no_longer_current");
        assert_eq!(d["proposals"],json!([old]));
    }
    #[test]
    fn active_preparation_and_unknown_legacy_assistant_jobs_keep_the_review_slot_busy() {
        for purpose in [Some("auto_prepare"),Some("auto_revalidate"),None] {
            for status in ["running","queued"] {
                let mut d=held();enabled(&mut d);
                let mut active=json!({"id":"other","kind":"assistant","refId":"other-item","status":status});
                if let Some(purpose)=purpose {active["purpose"]=json!(purpose);}
                d["jobs"].as_array_mut().unwrap().push(active);
                let jobs=d["jobs"].clone();let proposals=d["proposals"].clone();
                assert!(claim(&mut d,NOW+1).unwrap().is_none(),"{purpose:?}/{status}");
                assert_eq!(d["items"][0]["autoRevalidation"]["status"],"settling","{purpose:?}/{status}");
                assert!(d["items"][0]["autoRevalidation"]["jobId"].is_null());
                assert_eq!(d["jobs"],jobs);assert_eq!(d["proposals"],proposals);
                d["jobs"].as_array_mut().unwrap().pop();
                assert!(claim(&mut d,NOW+1).unwrap().is_none());
                assert_eq!(d["items"][0]["autoRevalidation"]["status"],"settling");
                assert!(claim(&mut d,NOW+32).unwrap().is_some());
            }
        }
    }
    #[test]
    fn source_changes_and_manual_edits_reject_inflight_result_without_losing_saved_text() {
        for change in ["text","draft","override","edited","proposal"] {
            let mut d=held();let (job,_)=start(&mut d);let old=d["proposals"][0].clone();
            let captured=captured_result(&mut d,&job,&response()).unwrap();
            match change {
                "text"=>d["branches"][0]["messages"][0]["text"]=json!("Edited source"),
                "draft"=>d["items"][0]["draft"]=json!("Human draft"),
                "override"=>d["items"][0]["autoPreparation"]["humanOverrideAt"]=json!(super::super::stamp(NOW+33)),
                "edited"=>d["items"][0]["draftEdited"]=json!(true),
                _=>d["proposals"].as_array_mut().unwrap().push(json!({"id":"manual","itemId":"i","status":"draft","text":"Human proposal"})),
            }
            let draft=d["items"][0]["draft"].clone();
            let result=super::super::complete(&mut d,&job,&captured,NOW+34).unwrap();
            assert_eq!(result["status"],"stale");assert_eq!(d["proposals"][0],old);assert_eq!(d["items"][0]["draft"],draft);
            let code=match change {"text"=>"bundle_no_longer_current","draft"=>"operator_draft_present",
                "override"=>"human_override_present","edited"=>"operator_draft_edited",_=>"protected_proposal_present"};
            assert_eq!(result["rejectionCode"],code);
        }
    }
    #[test]
    fn interrupted_run_consumes_source_budget_across_serialization_without_retry() {
        let mut d=held();let (job,_)=start(&mut d);
        row_mut(&mut d,"jobs",&job).unwrap()["status"]=json!("interrupted");
        d=serde_json::from_str(&d.to_string()).unwrap();
        assert!(claim(&mut d,NOW+34).unwrap().is_none());
        assert_eq!(d["items"][0]["autoRevalidation"]["status"],"held");
        assert_eq!(d["proposals"][0]["status"],"stale");
        let held=d.clone();assert!(claim(&mut d,NOW+40).unwrap().is_none());assert_eq!(d,held);
        assert_eq!(d["jobs"].as_array().unwrap().len(),2);
        assert_eq!(status(&d,NOW+40)["candidateCount"],0);
    }
    #[test]
    fn more_than_100_recent_jobs_do_not_block_new_evidence_but_unchanged_source_does() {
        let mut d=held();enabled(&mut d);
        for number in 0..101 { d["jobs"].as_array_mut().unwrap().push(json!({"id":format!("spent-{number}"),"kind":"assistant","purpose":PURPOSE,"status":"failed","refId":format!("other-{number}"),"claimedAt":super::super::stamp(NOW)})); }
        d["settings"]["autoPreparation"]["revalidation"]["dailyLimit"]=json!(1); // legacy stored value has no authority
        assert!(claim(&mut d,NOW+1).unwrap().is_none());assert_eq!(d["items"][0]["autoRevalidation"]["status"],"settling");
        assert_eq!(status(&d,NOW+1)["claimedLast24Hours"],101);
        assert!(claim(&mut d,NOW+32).unwrap().is_some());
        let mut d=held();enabled(&mut d);
        d["materials"]=json!([]);
        assert!(claim(&mut d,NOW+1).unwrap().is_none());
    }
    #[test]
    fn still_needs_attention_is_a_durable_result_not_a_retry_loop() {
        let mut d=held();let (job,_)=start(&mut d);
        let result=json!({"text":"Need verified price","sources":[],"assessments":[{"itemId":"i","outcome":"needs_attention","reason":"Нужна подтверждённая цена"}],"proposals":[]});
        assert_eq!(complete_with_captured_result(&mut d,&job,&result,NOW+33).unwrap()["status"],"needs_attention");
        row_mut(&mut d,"jobs",&job).unwrap()["status"]=json!("completed");
        let before=d.clone();
        assert!(super::super::claim(&mut d,NOW+34).unwrap().is_none());
        assert_eq!(d,before);
        assert_eq!(d["proposals"].as_array().unwrap().len(),1);
    }
    #[test]
    fn old_same_item_digests_do_not_block_new_evidence_but_provenance_guard_remains() {
        let mut d=held();enabled(&mut d);
        d["settings"]["autoPreparation"]["revalidation"]["itemDailyLimit"]=json!(1);
        for number in 0..3 { d["jobs"].as_array_mut().unwrap().push(json!({"id":format!("spent-{number}"),"kind":"assistant","purpose":PURPOSE,"status":"failed","refId":"i","sourceDigest":format!("old-source-{number}"),"claimedAt":super::super::stamp(NOW)})); }
        assert!(claim(&mut d,NOW+1).unwrap().is_none());
        assert!(claim(&mut d,NOW+32).unwrap().is_some());
        let mut d=held();enabled(&mut d);
        d["jobs"][0]["prepareBundle"]["request"]["materials"]=json!([{"text":"tampered"}]);
        assert!(claim(&mut d,NOW+1).unwrap().is_none());
        assert!(d["items"][0]["autoRevalidation"].is_null());
    }
    #[test]
    fn multiple_distinct_source_changes_on_one_item_are_admitted_in_one_day() {
        let mut d=held();enabled(&mut d);
        let mut digests=std::collections::HashSet::new();
        for number in 0..3 {
            let at=NOW+1+number*70;
            assert!(claim(&mut d,at).unwrap().is_none());
            let (job,_)=claim(&mut d,at+31).unwrap().unwrap();
            let digest=row(&d,"jobs",&job).unwrap()["sourceDigest"].as_str().unwrap().to_owned();
            assert!(digests.insert(digest));
            assert_eq!(complete_with_captured_result(&mut d,&job,&response(),at+32).unwrap()["status"],"prepared");
            row_mut(&mut d,"jobs",&job).unwrap()["status"]=json!("completed");
            d["materials"][0]["text"]=json!(format!("New evidence revision {number}"));
            super::super::reconcile_stale(&mut d,at+33);
        }
        assert_eq!(digests.len(),3);
        assert_eq!(list(&d,"jobs").iter().filter(|job|job["purpose"]==PURPOSE).count(),3);
    }
    #[test]
    fn unchanged_provider_merge_preserves_running_review_and_admits_result() {
        let mut d=held();
        let mut incoming=d["items"][0].clone();
        incoming.as_object_mut().unwrap().remove("autoRevalidation");
        incoming["contextObservedAt"]=json!(super::super::stamp(NOW+1));
        crate::merge_snapshot(&mut d,&json!({"items":[incoming.clone()]})).unwrap();
        let (job,_)=start(&mut d);
        let source=row(&d,"jobs",&job).unwrap()["sourceDigest"].clone();
        incoming["contextObservedAt"]=json!(super::super::stamp(NOW+33));
        crate::merge_snapshot(&mut d,&json!({"items":[incoming]})).unwrap();
        assert_eq!(d["items"][0]["autoRevalidation"]["jobId"],job);
        assert_eq!(crate::prepare_bundle::review_fingerprint(&d,"i").unwrap(),source);
        assert_eq!(complete_with_captured_result(&mut d,&job,&response(),NOW+34).unwrap()["status"],"prepared");
    }
    #[test]
    fn changed_provider_branch_keeps_review_pointer_but_rejects_old_result() {
        let mut d=held();
        let mut incoming=d["items"][0].clone();
        incoming.as_object_mut().unwrap().remove("autoRevalidation");
        incoming["contextObservedAt"]=json!(super::super::stamp(NOW+1));
        crate::merge_snapshot(&mut d,&json!({"items":[incoming.clone()]})).unwrap();
        let (job,_)=start(&mut d);
        let saved=d["proposals"][0].clone();
        let captured=captured_result(&mut d,&job,&response()).unwrap();
        incoming["contextObservedAt"]=json!(super::super::stamp(NOW+33));
        let mut changed_branch=d["branches"][0].clone();
        changed_branch["messages"][0]["text"]=json!("Changed provider evidence");
        crate::merge_snapshot(&mut d,&json!({"items":[incoming],"branches":[changed_branch]})).unwrap();
        assert_eq!(d["items"][0]["autoRevalidation"]["jobId"],job);
        let outcome=super::super::complete(&mut d,&job,&captured,NOW+34).unwrap();
        assert_eq!(outcome["status"],"stale");
        assert_eq!(outcome["rejectionCode"],"bundle_no_longer_current");
        assert_eq!(d["proposals"][0]["text"],saved["text"]);
        assert_eq!(d["proposals"].as_array().unwrap().len(),1);
    }
    #[test]
    fn missing_running_review_pointer_has_a_durable_specific_rejection() {
        let mut d=held();let (job,_)=start(&mut d);
        let captured=captured_result(&mut d,&job,&response()).unwrap();
        d["items"][0].as_object_mut().unwrap().remove("autoRevalidation");
        let outcome=super::super::complete(&mut d,&job,&captured,NOW+34).unwrap();
        assert_eq!(outcome["status"],"stale");
        assert_eq!(outcome["rejectionCode"],"review_job_pointer_changed");
        assert_eq!(row(&d,"jobs",&job).unwrap()["prepareOutcome"],outcome);
        assert_eq!(d["proposals"].as_array().unwrap().len(),1);
    }
    fn paid_lost_pointer()->Value {
        let mut d=held();let (job,_)=start(&mut d);
        let request=row(&d,"jobs",&job).unwrap()["prepareBundle"]["request"].clone();
        let mut result=crate::engine_prepare::tests::single_pass_result(response());
        // Match the actual captured triage route before creating paid evidence.
        if let Some(contract)=request.get("researchLimitContract") {
            result["runMetadata"]["researchLimitContract"]=contract.clone();
        }else{result["runMetadata"].as_object_mut().unwrap().remove("researchLimitContract");}
        crate::model_material_receipt::fixture_result(&mut d,&job,&request,&mut result).unwrap();
        crate::preparation_review::record_first(&mut d,&job,&result,&super::super::stamp(NOW+33)).unwrap();
        d["items"][0].as_object_mut().unwrap().remove("autoRevalidation");
        let outcome=super::super::complete(&mut d,&job,&result,NOW+34).unwrap();
        assert_eq!(outcome["status"],"stale");
        assert_eq!(outcome["rejectionCode"],"review_job_pointer_changed");
        let stored=row_mut(&mut d,"jobs",&job).unwrap();
        stored["status"]=json!("completed");
        stored["result"]=outcome;
        d
    }
    #[test]
    fn paid_rejection_replays_stored_model_stage_once_without_new_job_or_approval() {
        let mut d=paid_lost_pointer();let before=d.clone();
        let job=d["jobs"].as_array().unwrap().last().unwrap()["id"].as_str().unwrap().to_owned();
        let plan=crate::recover_rejected_revalidation_plan(&mut d,false,NOW+34).unwrap();
        assert_eq!(plan["eligibleCount"],1);assert_eq!(plan["recoveredCount"],0);
        assert_eq!(d,before);
        let applied=crate::recover_rejected_revalidation_plan(&mut d,true,NOW+34).unwrap();
        assert_eq!(applied["eligibleCount"],1);assert_eq!(applied["recoveredCount"],1);
        assert_eq!(d["jobs"].as_array().unwrap().len(),before["jobs"].as_array().unwrap().len());
        assert_eq!(d["proposals"].as_array().unwrap().len(),2);
        assert_eq!(d["proposals"][0]["text"],before["proposals"][0]["text"]);
        assert_eq!(d["items"][0]["autoRevalidation"]["jobId"],job);
        let stored=row(&d,"jobs",&job).unwrap();
        assert_eq!(stored["status"],"completed");
        assert_eq!(stored["prepareOutcome"]["status"],"prepared");
        assert_eq!(stored["recovery"]["originalPrepareOutcome"],before["jobs"][1]["prepareOutcome"]);
        assert_eq!(stored["preparationStages"],before["jobs"][1]["preparationStages"]);
        assert_eq!(stored["retainedEvidence"],before["jobs"][1]["retainedEvidence"]);
        assert_eq!(stored["modelMaterialReceipts"],before["jobs"][1]["modelMaterialReceipts"]);
        assert_eq!(d["proposals"][1]["modelMaterialReceipt"],before["jobs"][1]["preparationStages"]["first"]["result"]["modelMaterialReceipt"]);
        crate::proposal_current(&d,&d["proposals"][1]).expect("paid replay preserves current material provenance");
        assert!(d["approvals"].as_array().unwrap().is_empty());assert!(d["operations"].as_array().unwrap().is_empty());
        let after=d.clone();
        assert_eq!(crate::recover_rejected_revalidation_plan(&mut d,true,NOW+35).unwrap()["eligibleCount"],0);
        assert_eq!(d,after);
    }
    #[test]
    fn paid_rejection_recovery_holds_changed_or_unverified_work() {
        for change in ["draft","approved","operation","source","invalid_observation","newer_initial","newer_group_initial","missing_review","invalid_result","wrong_reason","wrong_code","requested_restart"] {
            let mut d=paid_lost_pointer();
            match change {
                "draft"=>d["items"][0]["draft"]=json!("Human draft"),
                "approved"=>d["proposals"][0]["status"]=json!("approved"),
                "operation"=>d["operations"]=json!([{"id":"op","itemId":"i","status":"completed"}]),
                "source"=>d["branches"][0]["messages"][0]["text"]=json!("Changed source"),
                "invalid_observation"=>d["items"][0]["providerObservedAt"]=json!(super::super::stamp(NOW+95)),
                "newer_initial"=>d["jobs"].as_array_mut().unwrap().push(json!({"id":"newer","kind":"assistant","purpose":"auto_prepare","refId":"i","status":"completed"})),
                "newer_group_initial"=>d["jobs"].as_array_mut().unwrap().push(json!({"id":"newer-group","kind":"assistant","purpose":"auto_prepare","refId":"other","status":"completed","prepareBundle":{"itemIds":["other","i"]}})),
                "missing_review"=>d["jobs"][1]["preparationStages"]["first"]["reviewRequired"]=json!(true),
                "invalid_result"=>d["jobs"][1]["preparationStages"]["first"]["result"]=json!({"not":"a model result"}),
                "wrong_reason"=>{
                    // Historical outcomes have no code and require the exact reason.
                    d["jobs"][1]["prepareOutcome"].as_object_mut().unwrap().remove("rejectionCode");
                    d["jobs"][1]["prepareOutcome"]["reason"]=json!("Other rejection");
                },
                "wrong_code"=>d["jobs"][1]["prepareOutcome"]["rejectionCode"]=json!("bundle_no_longer_current"),
                _=>d["items"][0]["autoRevalidation"]=json!({"status":"requested","restartRunId":"newer"}),
            }
            let before=d.clone();
            let plan=crate::recover_rejected_revalidation_plan(&mut d,true,NOW+34).unwrap();
            assert_eq!(plan["eligibleCount"],0,"{change}");
            assert_eq!(d,before,"{change}");
        }
    }
    #[test]
    fn grouped_nonfirst_recipient_cannot_restart_or_gain_fresh_budget() {
        for state in ["running","failed","completed"] {
            let mut d=failed_initial();
            d["jobs"][0]["refId"]=json!("other");
            d["jobs"][0]["prepareBundle"]["itemIds"]=json!(["other","i"]);
            d["jobs"][0]["status"]=json!(state);
            let before=d.clone();
            for fresh in [false,true] {
                let plan=crate::preparation_restart::plan_context(&mut d,"group-restart",false,NOW+1,Some(&["i".into()]),fresh).unwrap();
                assert_eq!(plan["eligibleCount"],0);
                assert_eq!(plan["skipped"][0]["reason"],if state=="running"{"active_or_recorded_operation"}else{"group_review_required"});
                assert_eq!(d,before);
            }
            d["items"][0]["autoPreparation"]["status"]=json!("needs_attention");
            assert_eq!(eligibility_block(&d,&d["items"][0],NOW+1,false),Some("group_review_required"));
            enabled(&mut d);
            assert!(claim(&mut d,NOW+1).unwrap().is_none());
            assert_eq!(d["jobs"].as_array().unwrap().len(),1);
        }
    }
    #[test]
    fn configuration_drops_legacy_limits_and_status_is_read_only() {
        assert_eq!(validated_config(&json!({"enabled":true})).unwrap(),json!({"enabled":true,"debounceSeconds":120}));
        assert_eq!(validated_config(&json!({"enabled":true,"dailyLimit":100,"itemDailyLimit":2,"debounceSeconds":30})).unwrap(),json!({"enabled":true,"debounceSeconds":30}));
        assert_eq!(validated_config(&json!({"enabled":true,"dailyLimit":101,"itemDailyLimit":6})).unwrap(),json!({"enabled":true,"debounceSeconds":120}));
        for invalid in [json!(null),json!({"enabled":"yes"}),json!({"enabled":true,"dailyLimit":0}),
            json!({"enabled":true,"dailyLimit":-1}),json!({"enabled":true,"itemDailyLimit":0}),
            json!({"enabled":true,"itemDailyLimit":1.5}),
            json!({"enabled":true,"debounceSeconds":0}),json!({"enabled":true,"debounceSeconds":3601}),
            json!({"enabled":true,"externalWrites":true})] { assert!(validated_config(&invalid).is_err()); }
        let d=held();let before=d.clone();let summary=status(&d,NOW+1);
        assert_eq!(summary["configuration"]["enabled"],false);
        assert_eq!(summary["mode"],"continuous");
        assert!(summary["configuration"].get("dailyLimit").is_none());
        assert_eq!(summary["candidateCount"],1);
        let brief=status_summary(&d,NOW+40);
        assert!(brief["candidateCount"].is_null(),"summary must not report zero for an unevaluated real candidate");
        assert_eq!(brief["eligibility"]["status"],"not_evaluated");
        assert_eq!(brief["currentWorkflow"],summary["currentWorkflow"]);
        assert_eq!(summary["claimedLast24Hours"],0);
        assert_eq!(d,before);
    }
    #[test]
    fn explicit_restart_previews_preserves_text_and_reviews_unchanged_source_once() {
        let mut d=held();d["materials"]=json!([]);enabled(&mut d);
        // Source is identical to the saved run. Turn its automatic candidate
        // back into the current prepared presentation for restart admission.
        d["proposals"][0]["status"]=json!("draft");
        d["items"][0]["workflow"]=json!("prepared");
        let original=d.clone();
        let preview=crate::preparation_restart::plan(&mut d,"fresh-1",false,NOW+1).unwrap();
        assert_eq!(preview["eligibleCount"],1);assert_eq!(d,original);
        let receipt=crate::preparation_restart::plan(&mut d,"fresh-1",true,NOW+1).unwrap();
        assert_eq!(receipt["eligibleCount"],1);
        assert_eq!(d["proposals"][0]["text"],original["proposals"][0]["text"]);
        assert_eq!(d["proposals"][0]["prepareBundleDigest"],original["proposals"][0]["prepareBundleDigest"]);
        let applied=d.clone();
        assert_eq!(crate::preparation_restart::plan(&mut d,"fresh-1",true,NOW+2).unwrap(),receipt);
        assert_eq!(d,applied);
        assert!(claim(&mut d,NOW+2).unwrap().is_none());
        let (job,_)=claim(&mut d,NOW+33).unwrap().unwrap();
        assert_eq!(row(&d,"jobs",&job).unwrap()["restartRunId"],"fresh-1");
        assert_eq!(row(&d,"jobs",&job).unwrap()["sourceDigest"],crate::prepare_bundle::review_fingerprint(&d,"i").unwrap());
        row_mut(&mut d,"jobs",&job).unwrap()["status"]=json!("interrupted");
        d=serde_json::from_str(&d.to_string()).unwrap();
        assert!(claim(&mut d,NOW+34).unwrap().is_none());
        assert!(claim(&mut d,NOW+100).unwrap().is_none());
        assert_eq!(list(&d,"jobs").len(),2);
        // An interrupted reserved attempt retains its paid ownership. A new
        // operator receipt does not establish that the original model stopped.
        crate::preparation_restart::plan(&mut d,"fresh-2",true,NOW+101).unwrap();
        assert!(claim(&mut d,NOW+102).unwrap().is_none());
        assert!(claim(&mut d,NOW+133).unwrap().is_none());
        assert_eq!(list(&d,"jobs").len(),2);
        assert_eq!(row(&d,"jobs",&job).unwrap()["scopeReservation"]["ownerJobId"],job);
    }
    #[test]
    fn restart_holds_operator_work_and_unverified_provenance() {
        for protected in ["draft","cleared","override","origin","history","approved","dispatching","unknown","operation","tampered"] {
            let mut d=held();
            match protected {
                "draft"=>d["items"][0]["draft"]=json!("Operator reply"),
                "cleared"=>d["items"][0]["draftEdited"]=json!(true),
                "override"=>d["items"][0]["autoPreparation"]["humanOverrideAt"]=json!("earlier"),
                "origin"=>d["proposals"][0]["origin"]=json!({"id":"human"}),
                "history"=>d["proposals"][0]["history"]=json!([{"text":"human edit"}]),
                "operation"=>d["operations"]=json!([{"id":"op","itemId":"i","status":"unknown"}]),
                "tampered"=>d["jobs"][0]["prepareBundle"]["request"]["materials"]=json!([{"text":"tampered"}]),
                _=>d["proposals"][0]["status"]=json!(protected),
            }
            let items=d["items"].clone();let proposals=d["proposals"].clone();let jobs=d["jobs"].clone();
            let receipt=crate::preparation_restart::plan(&mut d,"fresh",true,NOW+1).unwrap();
            assert_eq!(receipt["eligibleCount"],0,"{protected}");
            assert_eq!(d["items"],items,"{protected}");assert_eq!(d["proposals"],proposals);
            assert_eq!(d["jobs"],jobs);
        }
    }
    #[test]
    fn restart_marker_without_durable_receipt_cannot_bypass_source_gate() {
        let mut d=held();enabled(&mut d);d["materials"]=json!([]);
        d["items"][0]["autoRevalidation"]=json!({"restartRunId":"forged"});
        assert!(claim(&mut d,NOW+1).unwrap().is_none());
        crate::preparation_restart::plan(&mut d,"real",true,NOW+2).unwrap();
        d["settings"]["autoPreparation"]["revalidation"]["dailyLimit"]=json!(1);
        d["jobs"].as_array_mut().unwrap().push(json!({"id":"spent","purpose":PURPOSE,"status":"failed","claimedAt":super::super::stamp(NOW)}));
        assert!(claim(&mut d,NOW+3).unwrap().is_none());
        let (job,_)=claim(&mut d,NOW+60).unwrap().unwrap();
        assert_eq!(row(&d,"jobs",&job).unwrap()["restartRunId"],"real");
    }
    fn failed_initial()->Value {
        let mut d=initial_fixture();
        let (job,_)=super::super::claim(&mut d,NOW).unwrap().unwrap();
        // An archival known failure predates reservation/material contracts.
        // Build that archive before any paid capture, rather than rewriting a
        // successfully retained modern response into failed history.
        legacy_two_pass_request(&mut d,&job);
        let old=d["jobs"][0]["id"].clone();
        d["jobs"][0]["status"]=json!("failed");
        d["jobs"][0]["error"]=json!("ASSISTANT_INVALID_RESEARCH");
        d["jobs"][0]["prepareOutcome"]=Value::Null;
        d["items"][0]["autoPreparation"]=json!({"status":"error","jobId":old,"requiresReview":true});
        enabled(&mut d);d
    }
    #[test]
    fn scoped_restart_changes_only_selected_items_and_binds_idempotence() {
        let mut d=held();let mut other=d["items"][0].clone();other["id"]=json!("other");
        d["items"].as_array_mut().unwrap().push(other.clone());
        let before=d.clone();let ids=vec!["i".to_owned()];
        let preview=crate::preparation_restart::plan_scoped(&mut d,"scoped",false,NOW+1,Some(&ids)).unwrap();
        assert_eq!(preview["itemIds"],json!(["i"]));assert_eq!(d,before);
        let receipt=crate::preparation_restart::plan_scoped(&mut d,"scoped",true,NOW+1,Some(&ids)).unwrap();
        assert_eq!(d["items"][1],other);let applied=d.clone();
        assert_eq!(crate::preparation_restart::plan_scoped(&mut d,"scoped",true,NOW+2,Some(&ids)).unwrap(),receipt);
        assert!(crate::preparation_restart::plan(&mut d,"scoped",true,NOW+2).is_err());
        assert!(crate::preparation_restart::plan_scoped(&mut d,"scoped",true,NOW+2,Some(&["other".into()])).is_err());
        assert!(crate::preparation_restart::plan_scoped(&mut d,"missing",true,NOW+2,Some(&["missing".into()])).is_err());
        assert_eq!(d,applied);
        // Legacy receipts without requestedItemIds remain bound to all-scope.
        d["preparationRuns"][0].as_object_mut().unwrap().remove("requestedItemIds");
        assert!(crate::preparation_restart::plan(&mut d,"scoped",true,NOW+3).is_ok());
        assert!(crate::preparation_restart::plan_scoped(&mut d,"scoped",true,NOW+3,Some(&ids)).is_err());
    }
    #[test]
    fn scoped_error_restart_is_actually_claimed_once_without_stage_fallback() {
        let mut d=failed_initial();let original=d["jobs"][0].clone();
        assert!(claim(&mut d,NOW+1).unwrap().is_none());
        assert_eq!(crate::preparation_restart::plan(&mut d,"all",false,NOW+1).unwrap()["eligibleCount"],0);
        let ids=vec!["i".to_owned()];
        let receipt=crate::preparation_restart::plan_scoped(&mut d,"error-1",true,NOW+1,Some(&ids)).unwrap();
        assert_eq!(receipt["eligibleCount"],1);assert_eq!(receipt["errorRetries"][0]["jobId"],original["id"]);
        assert!(claim(&mut d,NOW+2).unwrap().is_none());
        let (job,request)=claim(&mut d,NOW+33).unwrap().unwrap();
        assert_eq!(request["previousDecision"]["outcome"],"needs_attention");
        assert_eq!(request["previousDecision"]["text"],"");
        assert_eq!(request["previousDecision"]["reason"],"ASSISTANT_INVALID_RESEARCH");
        assert_eq!(d["jobs"][0],original);assert!(d["proposals"].as_array().unwrap().is_empty());
        failed(&mut d,&job,"ASSISTANT_INVALID_RESEARCH",NOW+34).unwrap();
        row_mut(&mut d,"jobs",&job).unwrap()["status"]=json!("failed");
        d=serde_json::from_str(&d.to_string()).unwrap();
        assert!(claim(&mut d,NOW+35).unwrap().is_none());assert!(claim(&mut d,NOW+100).unwrap().is_none());
        assert_eq!(list(&d,"jobs").len(),2);
        crate::preparation_restart::plan_scoped(&mut d,"error-1",true,NOW+101,Some(&ids)).unwrap();
        assert!(claim(&mut d,NOW+102).unwrap().is_none());
        crate::preparation_restart::plan_scoped(&mut d,"error-2",true,NOW+103,Some(&ids)).unwrap();
        // A repeat requires a new verified error state, not the held marker alone.
        assert!(claim(&mut d,NOW+104).unwrap().is_none());
        assert!(claim(&mut d,NOW+140).unwrap().is_none());
    }
    #[test]
    fn scoped_error_restart_rejects_unverified_human_and_executed_work() {
        for guard in ["text","edited","override","approved","succeeded","operation","human","foreign","tampered","source","running","manual_job","forged"] {
            let mut d=failed_initial();
            match guard {
                "text"=>d["items"][0]["draft"]=json!("Human text"),
                "edited"=>d["items"][0]["draftEdited"]=json!(true),
                "override"=>d["items"][0]["autoPreparation"]["humanOverrideAt"]=json!("now"),
                "approved"|"succeeded"=>d["proposals"]=json!([{"id":"p","itemId":"i","status":guard}]),
                "operation"=>d["operations"]=json!([{"id":"op","itemId":"i","status":"succeeded"}]),
                "human"=>d["proposals"]=json!([{"id":"p","itemId":"i","status":"draft","origin":{"id":"human"}}]),
                "foreign"=>d["jobs"][0]["refId"]=json!("another"),
                "tampered"=>d["jobs"][0]["prepareBundle"]["request"]["instruction"]=json!("tampered"),
                "source"=>d["branches"][0]["messages"][0]["text"]=json!("changed"),
                "running"=>d["jobs"][0]["status"]=json!("running"),
                "manual_job"=>d["jobs"][0]["purpose"]=json!("engine_prepare"),
                _=>d["items"][0]["autoRevalidation"]=json!({"restartRunId":"forged"}),
            }
            if guard=="forged" {assert!(claim(&mut d,NOW+1).unwrap().is_none());continue;}
            let before=d.clone();
            let receipt=crate::preparation_restart::plan_scoped(&mut d,"safe",false,NOW+1,Some(&["i".into()])).unwrap();
            assert_eq!(receipt["eligibleCount"],0,"{guard}");assert_eq!(d,before,"{guard}");
        }
    }
    fn failed_held_with_changed_source()->Value {
        let mut d=failed_initial();
        let mut latest=d["jobs"][0].clone();latest["id"]=json!("latest-failure");latest["purpose"]=json!("auto_revalidate");
        latest["error"]=json!("ASSISTANT_INVALID_RESEARCH_UNOBSERVED_URL");
        d["jobs"].as_array_mut().unwrap().push(latest);
        d["items"][0]["autoPreparation"]["status"]=json!("needs_attention");
        d["items"][0]["autoRevalidation"]=json!({"status":"held","jobId":"latest-failure"});
        d["branches"][0]["messages"][0]["text"]=json!("Current corrected context");d
    }
    #[test]
    fn fresh_context_reanalysis_uses_current_evidence_and_saves_only_new_result() {
        let mut d=failed_held_with_changed_source();let before=d.clone();let ids=vec!["i".into()];
        assert_eq!(crate::preparation_restart::plan_scoped(&mut d,"ordinary",false,NOW+1,Some(&ids)).unwrap()["eligibleCount"],0);
        let preview=crate::preparation_restart::plan_context(&mut d,"fresh",false,NOW+1,Some(&ids),true).unwrap();
        assert_eq!(preview["eligibleCount"],1);assert_eq!(preview["freshReanalyses"][0]["jobId"],"latest-failure");assert_eq!(d,before);
        let receipt=crate::preparation_restart::plan_context(&mut d,"fresh",true,NOW+1,Some(&ids),true).unwrap();
        let applied=d.clone();
        assert_eq!(crate::preparation_restart::plan_context(&mut d,"fresh",true,NOW+2,Some(&ids),true).unwrap(),receipt);
        assert!(crate::preparation_restart::plan_scoped(&mut d,"fresh",true,NOW+2,Some(&ids)).is_err());assert_eq!(d,applied);
        assert!(claim(&mut d,NOW+2).unwrap().is_none());
        let (job,request)=claim(&mut d,NOW+33).unwrap().unwrap();
        assert_eq!(request["previousDecision"]["prepareRunId"],"latest-failure");
        assert_eq!(request["previousDecision"]["text"],"");assert_eq!(request["previousDecision"]["outcome"],"needs_attention");
        assert_eq!(request["branches"][0]["messages"][0]["text"],"Current corrected context");
        assert!(d["proposals"].as_array().unwrap().is_empty());assert_eq!(d["jobs"][0],before["jobs"][0]);assert_eq!(d["jobs"][1],before["jobs"][1]);
        let outcome=complete_with_captured_result(&mut d,&job,&response(),NOW+34).unwrap();
        assert_eq!(outcome["status"],"prepared");assert_eq!(d["proposals"].as_array().unwrap().len(),1);
        assert_eq!(d["proposals"][0]["prepareRunId"],job);assert!(crate::proposal_current(&d,&d["proposals"][0]).is_ok());
        assert!(d["approvals"].as_array().unwrap().is_empty());assert!(d["operations"].as_array().unwrap().is_empty());
    }
    #[test]
    fn fresh_context_rechecks_source_binding_and_operator_work_before_claim() {
        for change in ["source","target","account","binding","tampered","draft","edited","operation","proposal","newer","newer_group","scope"] {
            let mut d=failed_held_with_changed_source();let ids=vec!["i".into()];
            crate::preparation_restart::plan_context(&mut d,"fresh",true,NOW+1,Some(&ids),true).unwrap();
            assert!(claim(&mut d,NOW+2).unwrap().is_none());
            match change {
                "source"=>d["branches"][0]["messages"][0]["text"]=json!("Changed again"),
                "target"=>d["items"][0]["itemId"]=json!("another"),
                "account"=>d["account"]=json!("other"),
                "binding"=>d["connectorBinding"]=json!({"different":true}),
                "tampered"=>d["jobs"][1]["prepareBundle"]["request"]["instruction"]=json!("tampered"),
                "draft"=>d["items"][0]["draft"]=json!("Human draft"),
                "edited"=>d["items"][0]["draftEdited"]=json!(true),
                "operation"=>d["operations"]=json!([{"itemId":"i","status":"succeeded"}]),
                "proposal"=>d["proposals"]=json!([{"itemId":"i","status":"approved"}]),
                "newer"=>d["jobs"].as_array_mut().unwrap().push(json!({"id":"newer","refId":"i","purpose":"auto_prepare","status":"failed"})),
                "newer_group"=>d["jobs"].as_array_mut().unwrap().push(json!({"id":"newer","refId":"other","purpose":"auto_prepare","status":"failed","prepareBundle":{"itemIds":["other","i"]}})),
                _=>d["preparationRuns"][0]["requestedItemIds"]=json!(["other"]),
            }
            d=serde_json::from_str(&d.to_string()).unwrap();
            let jobs=d["jobs"].clone();let proposals=d["proposals"].clone();
            assert!(claim(&mut d,NOW+33).unwrap().is_none(),"{change}");
            assert_eq!(d["jobs"],jobs,"{change}");assert_eq!(d["proposals"],proposals,"{change}");
        }
    }
    #[test]
    fn fresh_context_rejects_forgery_and_cancelled_attempt_is_consumed() {
        for change in ["hash","target","account","cancelled","pointer","newer"] {
            let mut d=failed_held_with_changed_source();
            match change {
                "hash"=>d["jobs"][1]["prepareBundle"]["digest"]=json!("wrong"),
                "target"=>d["items"][0]["objectId"]=json!("other"),
                "account"=>d["account"]=json!("other"),
                "cancelled"=>d["jobs"][1]["status"]=json!("cancelled"),
                "pointer"=>d["items"][0]["autoRevalidation"]["jobId"]=json!("missing"),
                _=>d["jobs"].as_array_mut().unwrap().push(json!({"id":"newer","refId":"i","purpose":"auto_prepare","status":"failed"})),
            }
            let before=d.clone();let result=crate::preparation_restart::plan_context(&mut d,"fresh",false,NOW+1,Some(&["i".into()]),true).unwrap();
            assert_eq!(result["eligibleCount"],0,"{change}");assert_eq!(d,before);
        }
        let mut d=failed_held_with_changed_source();let ids=vec!["i".into()];
        crate::preparation_restart::plan_context(&mut d,"fresh",true,NOW+1,Some(&ids),true).unwrap();
        assert!(claim(&mut d,NOW+2).unwrap().is_none());let (job,_)=claim(&mut d,NOW+33).unwrap().unwrap();
        row_mut(&mut d,"jobs",&job).unwrap()["status"]=json!("cancelled");
        d=serde_json::from_str(&d.to_string()).unwrap();
        assert!(claim(&mut d,NOW+34).unwrap().is_none());assert!(claim(&mut d,NOW+100).unwrap().is_none());
        crate::preparation_restart::plan_context(&mut d,"fresh",true,NOW+101,Some(&ids),true).unwrap();
        assert!(claim(&mut d,NOW+140).unwrap().is_none());assert_eq!(list(&d,"jobs").len(),3);
    }
    #[test]
    fn stale_moderation_history_never_becomes_a_public_reply(){
        for kind in ["hide","delete"]{
            let mut d=held();d["proposals"][0]["kind"]=json!(kind);d["proposals"][0]["text"]=json!("");
            let (_,decision)=previous(&d,&d["items"][0]).unwrap();
            assert_eq!(decision["outcome"],kind);assert_eq!(decision["text"],"");
            assert_eq!(decision["proposalId"],d["proposals"][0]["id"]);
        }
    }

    #[test]
    fn fact_only_observation_starts_debounce_then_review_gets_one_fair_turn(){
        let mut d=held();enabled(&mut d);let before_jobs=d["jobs"].clone();let before_auto=d["items"][0]["autoPreparation"].clone();
        // The fact-tail tick observes before returning to drain a busy lane.
        observe(&mut d,NOW+1).unwrap();
        assert_eq!(d["items"][0]["autoRevalidation"]["observedAt"],super::super::stamp(NOW+1));
        assert_eq!(d["jobs"],before_jobs);assert_eq!(d["items"][0]["autoPreparation"],before_auto);
        let lookup=crate::new_job(&mut d,"assistant","public_fact_followup").unwrap();
        row_mut(&mut d,"jobs",&lookup).unwrap()["purpose"]=json!("public_fact_followup");
        let jobs=d["jobs"].clone();observe(&mut d,NOW+32).unwrap();
        assert!(ready(&d,NOW+32).unwrap());assert!(claim(&mut d,NOW+32).unwrap().is_none());
        assert_eq!(d["jobs"],jobs);assert_eq!(d["items"][0]["autoPreparation"],before_auto);
        row_mut(&mut d,"jobs",&lookup).unwrap()["status"]=json!("completed");
        let (review,_)=claim(&mut d,NOW+33).unwrap().unwrap();
        assert_eq!(row(&d,"jobs",&review).unwrap()["purpose"],PURPOSE);
        let claimed=d["jobs"].clone();assert!(claim(&mut d,NOW+34).unwrap().is_none());assert_eq!(d["jobs"],claimed);
        assert_eq!(d["items"][0]["autoPreparation"],before_auto);
    }

}
