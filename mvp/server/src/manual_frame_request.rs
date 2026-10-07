//! Explicit, authenticated source-frame requests before paid generation.
//! Owns local extraction authority only. No paid parent, repair round, ASR,
//! OCR or model dispatch can be manufactured by this module. Missing video is
//! acquired by one separately fenced download-only child of the native request.
use crate::media_artifacts::{ArtifactRef, ArtifactStore};
use crate::media_frame_sample_decode::{SampleTools, decode_sample, verify_sample_result};
use crate::preparation_materials::{hash, rows};
use crate::runtime_lifecycle::{AdmissionClass, OwnerToken};
use serde_json::{Value, json};
use std::{path::PathBuf, time::Duration};

pub(crate) const ORIGIN: &str = "manual_before_generation";
const CONTRACT: &str = "ManualVideoFrameRequest.v1";
const PURPOSE: &str = "manual_video_frames";
const RESULT: &str = "ManualVideoFrameResult.v1";

fn txt<'a>(v: &'a Value, key: &str) -> &'a str { v[key].as_str().unwrap_or("") }
fn sha(v: &Value) -> bool { crate::media_frame_sample::sha(v) }
fn exact(v: &Value, fields: &[&str]) -> bool {
    v.as_object().is_some_and(|o| o.len() == fields.len() && fields.iter().all(|k| o.contains_key(*k)))
}
fn uuid(v: &Value) -> bool {
    v.as_str().is_some_and(|s| uuid::Uuid::parse_str(s).is_ok_and(|u| u.to_string() == s))
}
fn token_value(t: &OwnerToken) -> Value {
    json!({"account":t.account,"runtimeId":t.runtime_id,"releaseSha256":t.release_sha256,"epoch":t.epoch})
}
fn unsigned(v: &Value, field: &str) -> Value {
    let mut v = v.clone();
    if let Some(o) = v.as_object_mut() { o.remove(field); }
    v
}

fn parse(body: &Value) -> Result<(), &'static str> {
    let mut fields = vec!["requestId", "postId", "attachmentIndex", "expectedSourceVersion", "requestedTimeOrIntent", "reason"];
    if body.get("prepareJobId").is_some() || body.get("expectedPrepareBundleDigest").is_some() {
        fields.extend(["prepareJobId", "expectedPrepareBundleDigest"]);
        if txt(body, "prepareJobId").is_empty() || !sha(&body["expectedPrepareBundleDigest"]) {
            return Err("manual_frame_prepare_capture_invalid");
        }
    }
    if !exact(body, &fields) || !uuid(&body["requestId"])
        || txt(body, "postId").is_empty() || txt(body, "postId").len() > 256
        || body["attachmentIndex"].as_u64().is_none_or(|n| n > 10_000)
        || !sha(&body["expectedSourceVersion"])
        || txt(body, "reason").trim().is_empty() || txt(body, "reason").len() > 1_000 {
        return Err("manual_frame_request_invalid");
    }
    let intent = &body["requestedTimeOrIntent"];
    if intent["timelineBasis"] != "relative_video_start" { return Err("manual_frame_timeline_invalid"); }
    match txt(intent, "kind") {
        "known_range" => {
            if !exact(intent, &["kind", "timelineBasis", "startMs", "endMs"]) { return Err("manual_frame_range_invalid"); }
            let start = intent["startMs"].as_u64().ok_or("manual_frame_range_invalid")?;
            let end = intent["endMs"].as_u64().ok_or("manual_frame_range_invalid")?;
            if end <= start || end - start > 30_000 { return Err("manual_frame_range_invalid"); }
        }
        "uniform_overview" if exact(intent, &["kind", "timelineBasis"]) => {}
        _ => return Err("manual_frame_intent_invalid"),
    }
    Ok(())
}

fn paid_started(job: &Value) -> bool {
    let stages = &job["preparationStages"];
    !stages["first"].is_null() || stages.get("firstAdmission").is_some()
        || !stages["review"].is_null() || stages.get("reviewChunks").is_some()
        || stages.get("answeringRepairs").is_some() || stages.get("repairBudget").is_some()
        || !rows(job, "retainedEvidence").is_empty() || !rows(job, "modelMaterialReceipts").is_empty()
        || job.get("originatingAnsweringAttemptId").is_some()
}
fn job_has_post(job: &Value, pin: &Value) -> bool {
    rows(&job["prepareBundle"]["request"]["postContextBundle"], "members").iter()
        .any(|m| m["canonicalPostId"] == pin["postId"] && m["postSourceVersion"] == pin["sourceVersion"])
        || rows(&job["prepareBundle"]["request"], "posts").iter().any(|p| p["id"] == pin["postId"]
            && crate::media_fullframes::source_version(p, txt(pin, "companyId")) == pin["sourceVersion"])
}
fn require_unpaid(d: &Value, body: &Value, pin: &Value) -> Result<(), &'static str> {
    if let Some(id) = body["prepareJobId"].as_str() {
        let job = crate::row(d, "jobs", id).map_err(|_| "manual_frame_prepare_job_missing")?;
        if job["kind"] != "assistant" || job["status"] != "running"
            || !matches!(txt(job, "purpose"), "engine_prepare" | "auto_prepare" | "auto_revalidate")
            || job["prepareBundle"]["digest"] != body["expectedPrepareBundleDigest"]
            || job["prepareBundle"]["digest"] != hash(&job["prepareBundle"]["request"])
            || job["prepareBundle"]["request"]["account"] != pin["companyId"]
            || job["preparationStages"]["initialAdmission"]["status"] != "scheduled"
            || job["preparationStages"]["initialAdmission"]["requestSha256"] != job["prepareBundle"]["digest"]
            || !job_has_post(job, pin) || paid_started(job) {
            return Err("manual_frame_prepare_capture_spent_or_changed");
        }
        let owner = crate::runtime_lifecycle::admission_token(d, AdmissionClass::Preparation)
            .map_err(|_| "manual_frame_prepare_owner_changed")?;
        if job["preparationStages"]["initialAdmission"]["owner"] != token_value(&owner) {
            return Err("manual_frame_prepare_owner_changed");
        }
    } else if rows(d, "jobs").iter().any(|j| job_has_post(j, pin) && paid_started(j)) {
        // A standalone request cannot be used to silently enrich an already
        // paid first capture. A later review needs its own explicit authority.
        return Err("manual_frame_first_generation_already_admitted");
    }
    Ok(())
}

fn current_capture(d: &Value, need: &Value, unpaid: bool) -> Result<(), &'static str> {
    if need["schemaVersion"] != 1 || need["contract"] != CONTRACT || need["origin"] != ORIGIN
        || !uuid(&need["needId"]) || !uuid(&need["requestId"])
        || need["needSha256"] != hash(&unsigned(need, "needSha256"))
        || need["companyId"] != d["account"] || txt(need, "authorizedBy").is_empty()
        || need.get("originatingAnsweringAttemptId").is_some() || need.get("requestingPaidAttemptId").is_some()
        || need.get("parentPaidResultRef").is_some() || need.get("repairBudget").is_some()
        || need["requestSha256"] != hash(&need["request"]) {
        return Err("manual_frame_native_capture_invalid");
    }
    parse(&need["request"])?;
    crate::media_speech_assets::current(d, &need["assetPin"]).map_err(|_| "manual_frame_source_changed")?;
    let pin = &need["assetPin"];
    if need["member"] != json!({"postId":pin["postId"],"connectorBinding":pin["connectorBinding"]})
        || need["companyId"] != pin["companyId"] || need["asset"]["attachmentIndex"] != pin["attachmentIndex"]
        || need["asset"]["attachmentIdentity"] != pin["attachmentIdentity"] || need["asset"]["sourceVersion"] != pin["sourceVersion"]
        || need["request"]["postId"] != pin["postId"] || need["request"]["attachmentIndex"] != pin["attachmentIndex"]
        || need["request"]["expectedSourceVersion"] != pin["sourceVersion"] || need["requestId"] != need["request"]["requestId"]
        || need["requestedTimeOrIntent"] != need["request"]["requestedTimeOrIntent"] || need["reason"] != need["request"]["reason"] {
        return Err("manual_frame_native_pins_changed");
    }
    if unpaid { require_unpaid(d, &need["request"], pin)?; }
    Ok(())
}

/// Original local observations may settle after a worker failure or restart.
/// This does not make that parent's old paid capture runnable again.
fn original_capture_current(d:&Value,job:&Value)->Result<(),&'static str>{
    let need=&job["manualFrameRequest"];current_capture(d,need,false)?;
    if let Some(parent_id)=need["request"]["prepareJobId"].as_str(){
        let parent=crate::row(d,"jobs",parent_id).map_err(|_|"manual_frame_prepare_job_missing")?;
        if parent["kind"]!="assistant" || !matches!(txt(parent,"purpose"),"engine_prepare"|"auto_prepare"|"auto_revalidate")
            || !matches!(txt(parent,"status"),"running"|"failed"|"interrupted") || paid_started(parent)
            || parent["prepareBundle"]["digest"]!=need["request"]["expectedPrepareBundleDigest"]
            || parent["prepareBundle"]["digest"]!=hash(&parent["prepareBundle"]["request"])
            || parent["prepareBundle"]["request"]["account"]!=need["companyId"]
            || parent["preparationStages"]["initialAdmission"]["status"]!="scheduled"
            || parent["preparationStages"]["initialAdmission"]["requestSha256"]!=parent["prepareBundle"]["digest"]
            || parent["preparationStages"]["initialAdmission"]["owner"]!=job["frameLease"]["owner"]
            || !job_has_post(parent,&need["assetPin"]){return Err("manual_frame_original_prepare_capture_changed");}
    } else {require_unpaid(d,&need["request"],&need["assetPin"])?;}
    Ok(())
}

/// Reuse only exact native asset selectors, or the unambiguous legacy single
/// video source. A matching URL is never proof of a multi-video attachment.
fn retained_source(d: &Value, pin: &Value) -> Result<Value, &'static str> {
    crate::media_speech_assets::current(d, pin).map_err(|_| "manual_frame_source_changed")?;
    let post = crate::row(d, "posts", txt(pin, "postId")).map_err(|_| "manual_frame_source_missing")?;
    let videos = rows(post, "attachments").iter().filter(|a| matches!(txt(a, "type"), "video" | "clip" | "reel")).count();
    let mut found: Option<Value> = None;
    for job in rows(d, "jobs") {
        let p = &job["result"]["visualProgress"];
        if !matches!(txt(job,"kind"),"media"|"manual_frame_source") || p["schemaVersion"] != 2 || p["account"] != pin["companyId"]
            || p["sourcePostId"] != pin["postId"] || p["sourcePostKey"] != pin["postKey"]
            || p["sourceVersion"] != pin["sourceVersion"] || p["connectorBinding"] != pin["connectorBinding"]
            || p["sourceIdentity"]["account"] != pin["companyId"] || p["sourceIdentity"]["postKey"] != pin["postKey"]
            || p["sourceIdentity"]["mediaSha256"] != p["source"]["sha256"]
            || p["sourceIdentity"]["durationMs"].as_u64().is_none_or(|n| n == 0 || n > 14_400_000)
            || ArtifactRef::from_json(&p["source"]).is_err() { continue; }
        if job["kind"]=="manual_frame_source" {
            if job["status"]!="completed"{continue;}
            let Ok(parent)=crate::row(d,"jobs",txt(job,"parentManualFrameRequestId"))else{continue};
            if source_observation(d,parent,job).is_err(){continue;}
        }
        if p.get("assetPin").is_some() {
            if p["assetPin"] != *pin || crate::media_speech_assets::require_progress(d, p).is_err() { continue; }
        } else if videos != 1 { continue; }
        let source = json!({"sourceArtifactRef":p["source"],"sourceDurationMs":p["sourceIdentity"]["durationMs"],
            "originJobId":job["id"],"checkpoint":p});
        if found.as_ref().is_some_and(|old| old["sourceArtifactRef"] != source["sourceArtifactRef"]
            || old["sourceDurationMs"] != source["sourceDurationMs"]) { return Err("manual_frame_source_ambiguous"); }
        if found.is_none() { found = Some(source); }
    }
    found.ok_or("manual_frame_source_acquisition_required")
}

#[derive(Debug)]
enum Admission { Replay(Value), Fresh(Value) }
fn claim(d: &mut Value, body: &Value, actor: &str, token: &OwnerToken, at: &str) -> crate::ApiResult<Admission> {
    parse(body).map_err(crate::bad)?;
    if actor.is_empty() || actor.len() > 256 { return Err(crate::bad("manual_frame_actor_invalid")); }
    if let Some(job) = rows(d, "jobs").iter().find(|j| j["id"] == body["requestId"]) {
        if job["purpose"] != PURPOSE || job["manualFrameRequest"]["request"] != *body
            || job["manualFrameRequest"]["authorizedBy"] != actor {
            return Err(crate::conflict("manual_frame_idempotency_conflict"));
        }
        current_capture(d, &job["manualFrameRequest"], false).map_err(crate::conflict)?;
        return Ok(Admission::Replay(job.clone()));
    }
    crate::runtime_lifecycle::require_admission(d, token, AdmissionClass::Preparation)?;
    let post = crate::row(d, "posts", txt(body, "postId"))?;
    let pin = crate::media_speech_assets::capture(d, post, body["attachmentIndex"].as_u64().unwrap() as usize)
        .map_err(|e| crate::conflict(&e))?;
    if pin["sourceVersion"] != body["expectedSourceVersion"] { return Err(crate::conflict("manual_frame_source_changed")); }
    require_unpaid(d, body, &pin).map_err(crate::conflict)?;
    if body.get("prepareJobId").is_none() && rows(d,"jobs").iter().any(|j|j["kind"]=="assistant"&&j["status"]=="running"&&job_has_post(j,&pin)&&!paid_started(j)){
        return Err(crate::conflict("manual_frame_prepare_capture_required"));
    }
    if rows(d, "jobs").iter().any(|j| j["purpose"] == PURPOSE && j["manualFrameRequest"]["assetPin"] == pin
        && j["manualFrameRequest"]["requestedTimeOrIntent"] == body["requestedTimeOrIntent"]
        && (j.get("extractionIntent").is_some() || j.get("sourceJobId").is_some())) {
        return Err(crate::conflict("manual_frame_exact_extraction_already_attempted"));
    }
    let source = retained_source(d, &pin);
    let mut asset = json!({"attachmentIndex":pin["attachmentIndex"],"attachmentIdentity":pin["attachmentIdentity"],"sourceVersion":pin["sourceVersion"]});
    if let Ok(source) = &source {
        asset["sourceArtifactRef"] = source["sourceArtifactRef"].clone();
        asset["sourceArtifactSha256"] = source["sourceArtifactRef"]["sha256"].clone();
    }
    let mut need = json!({"schemaVersion":1,"contract":CONTRACT,"origin":ORIGIN,"needId":crate::id(),
        "requestId":body["requestId"],"companyId":pin["companyId"],"authorizedBy":actor,"request":body,"requestSha256":hash(body),
        "assetPin":pin,"member":{"postId":pin["postId"],"connectorBinding":pin["connectorBinding"]},"asset":asset,
        "requestedTimeOrIntent":body["requestedTimeOrIntent"],"reason":body["reason"],"createdAt":at});
    need["needSha256"] = json!(hash(&need));
    current_capture(d, &need, true).map_err(crate::conflict)?;
    let mut job = json!({"id":body["requestId"],"kind":"media","purpose":PURPOSE,"status":"running","account":pin["companyId"],
        "refId":pin["postId"],"connectorBinding":pin["connectorBinding"],"manualFrameRequest":need,
        "frameLease":{"id":crate::id(),"epoch":1,"owner":token_value(token)},"createdAt":at,"modelCalled":false,"retryAuthorized":false});
    match source {
        Ok(source) => job["retainedSource"] = source,
        Err("manual_frame_source_acquisition_required") => {
            let source_job=new_source_child(d,&job,token,at)?;
            job["sourceJobId"]=source_job["id"].clone();crate::list_mut(d,"jobs").push(source_job);
        }
        Err(reason) => {
            job["status"] = json!("held"); job["reasonCode"] = json!(reason); job["finishedAt"] = json!(at);
            job["nextAction"] = json!({"kind":"acquire_exact_video_source_only","assetPin":pin,"asr":false,"ocr":false,"vision":false});
        }
    }
    crate::list_mut(d, "jobs").push(job.clone());
    crate::audit(d, "media.manual_frames_captured", txt(body, "requestId"));
    Ok(if job["status"] == "held" { Admission::Replay(job) } else { Admission::Fresh(job) })
}

fn new_source_child(d:&Value,parent:&Value,token:&OwnerToken,at:&str)->crate::ApiResult<Value>{
    crate::runtime_lifecycle::require_admission(d,token,AdmissionClass::Media)?;
    current_capture(d,&parent["manualFrameRequest"],true).map_err(crate::conflict)?;
    let pin=&parent["manualFrameRequest"]["assetPin"];let post=crate::row(d,"posts",txt(pin,"postId"))?;
    let mut progress=crate::media_fullframes::initial(txt(pin,"companyId"),&pin["connectorBinding"],post,at);
    progress["assetPin"]=pin.clone();progress["materialEpoch"]=json!(crate::media_queue::material_epoch(d,post));
    crate::media_fullframes::claim(&mut progress,&crate::id()).map_err(|e|crate::conflict(&e))?;
    Ok(json!({"id":crate::id(),"kind":"manual_frame_source","purpose":"manual_video_frame_source","status":"running",
        "account":pin["companyId"],"connectorBinding":pin["connectorBinding"],"refId":pin["postId"],"parentManualFrameRequestId":parent["id"],
        "sourceAssetPin":pin,"sourceOwner":token_value(token),"createdAt":at,"retryAuthorized":false,"sourceInitialProgress":progress,
        "sourceDownloadIntent":{"status":"reserved","owner":token_value(token),"progressSha256":hash(&progress),"leaseId":progress["leaseId"],"repeatAuthorized":false},
        "result":{"visualProgress":progress}}))
}
fn source_child_current(d:&Value,parent:&Value,child:&Value)->Result<(),&'static str>{
    current_capture(d,&parent["manualFrameRequest"],false)?;
    let pin=&parent["manualFrameRequest"]["assetPin"];let initial=&child["sourceInitialProgress"];
    if child["kind"]!="manual_frame_source"||child["purpose"]!="manual_video_frame_source"
        || child["id"]!=parent["sourceJobId"]||child["parentManualFrameRequestId"]!=parent["id"]||child["sourceAssetPin"]!=*pin
        || child["account"]!=pin["companyId"]||child["connectorBinding"]!=pin["connectorBinding"]
        || child["sourceOwner"]!=parent["frameLease"]["owner"]||child["sourceDownloadIntent"]["owner"]!=child["sourceOwner"]
        || child["sourceDownloadIntent"]["status"]!="reserved"||child["sourceDownloadIntent"]["repeatAuthorized"]!=false
        || child["sourceDownloadIntent"]["progressSha256"]!=hash(initial)||initial["phase"]!="download"||initial["assetPin"]!=*pin
        || child["sourceDownloadIntent"]["leaseId"]!=initial["leaseId"]||initial["leaseId"].as_str().is_none_or(str::is_empty)
        || initial["sourcePostId"]!=pin["postId"]||initial["sourcePostKey"]!=pin["postKey"]||initial["sourceVersion"]!=pin["sourceVersion"]
        || initial["account"]!=pin["companyId"]||initial["connectorBinding"]!=pin["connectorBinding"]{
        return Err("manual_frame_source_child_changed");
    }
    Ok(())
}
fn source_observation(d:&Value,parent:&Value,child:&Value)->Result<Value,&'static str>{
    source_child_current(d,parent,child)?;
    let p=&child["result"]["visualProgress"];let initial=&child["sourceInitialProgress"];
    crate::media_speech_assets::require_progress(d,p).map_err(|_|"manual_frame_source_changed")?;
    let r=ArtifactRef::from_json(&p["source"]).map_err(|_|"manual_frame_source_artifact_invalid")?;
    if !matches!(txt(child,"status"),"running"|"interrupted"|"unknown"|"completed") || !child["sourceDispatchedAt"].is_string()
        ||p["phase"]!="inventory"||r.bytes==0||r.bytes>500*1024*1024||p["sourceIdentity"]["mediaSha256"]!=r.sha256
        ||p["sourceIdentity"]["account"]!=initial["account"]||p["sourceIdentity"]["postKey"]!=initial["sourcePostKey"]
        ||p["sourceIdentity"]["durationMs"].as_u64().is_none_or(|n|n==0||n>14_400_000)
        ||["inventory","inventoryDescriptor","selectionDescriptor","latestReceipt","finalEvidence"].iter().any(|k|!p[*k].is_null())
        ||p["nextSelectionIndex"]!=0||p["completedSelectedFrames"]!=0
        ||["leaseId","leaseEpoch","account","connectorBinding","sourcePostId","sourcePostKey","sourceVersion","assetPin","materialEpoch"].iter().any(|k|p[*k]!=initial[*k]){
        return Err("manual_frame_source_checkpoint_invalid");
    }
    if parent["retainedSource"].is_object()&&parent["retainedSource"]["sourceArtifactRef"]!=p["source"]{
        return Err("manual_frame_original_source_bytes_changed");
    }
    let observed=json!({"sourceArtifactRef":p["source"],"sourceDurationMs":p["sourceIdentity"]["durationMs"],"originJobId":child["id"],"checkpoint":p});
    if child["status"]=="completed"{
        let receipt=&child["sourceAcquisitionReceipt"];
        if receipt["contract"]!="ManualVideoSourceReceipt.v1"||receipt["parentRequestId"]!=parent["id"]||receipt["assetPin"]!=*pin_for(parent)
            ||receipt["sourceOwner"]!=child["sourceOwner"]||receipt["sourceInitialProgressSha256"]!=hash(initial)
            ||receipt["source"]!=observed||receipt["receiptSha256"]!=hash(&unsigned(receipt,"receiptSha256"))
            ||parent["sourceAcquisitionRef"]!=json!({"jobId":child["id"],"receiptSha256":receipt["receiptSha256"]}){
            return Err("manual_frame_source_receipt_changed");
        }
    }
    Ok(observed)
}
fn pin_for(parent:&Value)->&Value{&parent["manualFrameRequest"]["assetPin"]}
fn bind_source_observation(d:&mut Value,parent:&Value,child:&Value,source:&Value,token:&OwnerToken,at:&str)->crate::ApiResult<Value>{
    crate::runtime_lifecycle::require_admission(d,token,AdmissionClass::Media)?;
    original_capture_current(d,parent).map_err(crate::conflict)?;
    if crate::row(d,"jobs",txt(parent,"id"))?!=parent||crate::row(d,"jobs",txt(child,"id"))?!=child
        ||source_observation(d,parent,child).map_err(crate::conflict)?!=*source{return Err(crate::conflict("manual_frame_source_settlement_changed"));}
    if child["status"]=="completed"{return Ok(parent.clone());}
    let mut receipt=json!({"contract":"ManualVideoSourceReceipt.v1","parentRequestId":parent["id"],"assetPin":child["sourceAssetPin"],
        "sourceOwner":child["sourceOwner"],"sourceInitialProgressSha256":hash(&child["sourceInitialProgress"]),"source":source,"at":at});
    receipt["receiptSha256"]=json!(hash(&receipt));let receipt_sha=receipt["receiptSha256"].clone();
    let saved=crate::row_mut(d,"jobs",txt(child,"id"))?;saved["status"]=json!("completed");saved["sourceAcquisitionReceipt"]=receipt;saved["finishedAt"]=json!(at);
    let saved=crate::row_mut(d,"jobs",txt(parent,"id"))?;
    if !saved["retainedSource"].is_object(){saved["retainedSource"]=source.clone();}
    saved["sourceAcquisitionRef"]=json!({"jobId":child["id"],"receiptSha256":receipt_sha});
    Ok(crate::row(d,"jobs",txt(parent,"id"))?.clone())
}
async fn acquire_source_only(app:&crate::App,parent:&Value,token:&OwnerToken)->crate::ApiResult<Value>{
    let snapshot=app.db.read_manual_frame_context().await?;let child=crate::row(&snapshot,"jobs",txt(parent,"sourceJobId"))?.clone();
    source_child_current(&snapshot,parent,&child).map_err(crate::conflict)?;
    let post=crate::row(&snapshot,"posts",txt(&parent["manualFrameRequest"]["member"],"postId"))?.clone();
    let binding=crate::active_binding(&snapshot)?;let progress=child["sourceInitialProgress"].clone();drop(snapshot);
    app.change(|d|{
        crate::runtime_lifecycle::require_admission(d,token,AdmissionClass::Media)?;
        current_capture(d,&parent["manualFrameRequest"],true).map_err(crate::conflict)?;
        if crate::row(d,"jobs",txt(parent,"id"))?!=parent||crate::row(d,"jobs",txt(&child,"id"))?!=&child
            ||child["status"]!="running"||child.get("sourceDispatchedAt").is_some()||child["result"]["visualProgress"]!=progress{
            return Err(crate::conflict("manual_frame_source_already_attempted"));
        }
        source_child_current(d,parent,&child).map_err(crate::conflict)?;
        crate::row_mut(d,"jobs",txt(&child,"id"))?["sourceDispatchedAt"]=json!(crate::now());Ok(())
    }).await?;
    let projection=app.bridge("media_source",json!({"account":crate::bridge_account(&binding)?,"postId":post["id"],"post":post,"assetPin":child["sourceAssetPin"]})).await?;
    crate::media_speech_assets::require_projection(&child["sourceAssetPin"],&projection).map_err(|e|crate::conflict(&e))?;
    let source=crate::media_processing::MediaSource::from_projection(&projection,txt(&child,"account"),txt(&progress,"sourcePostKey")).map_err(|e|crate::conflict(&e))?;
    app.change(|d|{
        crate::runtime_lifecycle::require_admission(d,token,AdmissionClass::Media)?;
        current_capture(d,&parent["manualFrameRequest"],true).map_err(crate::conflict)?;
        let saved=crate::row(d,"jobs",txt(&child,"id"))?;source_child_current(d,parent,saved).map_err(crate::conflict)?;
        if crate::row(d,"jobs",txt(parent,"id"))?!=parent||saved["status"]!="running"||saved["result"]["visualProgress"]!=progress{
            return Err(crate::conflict("manual_frame_source_pre_download_changed"));
        }Ok(())
    }).await?;
    // Exactly one download phase. Its inventory cursor is evidence of source
    // completion, never authority to invoke inventory/ASR/vision afterward.
    crate::media_processing::full::step(app,txt(&child,"id"),Some(&source),progress).await.map_err(|e|crate::conflict(&e))?;
    let snapshot=app.db.read_manual_frame_context().await?;let child=crate::row(&snapshot,"jobs",txt(&child,"id"))?.clone();
    let observed=source_observation(&snapshot,parent,&child).map_err(crate::conflict)?;drop(snapshot);
    let verify=observed["sourceArtifactRef"].clone();
    tokio::task::spawn_blocking(move||->Result<(),String>{crate::media_fullframes::store()?.verify(&ArtifactRef::from_json(&verify).map_err(|_|"manual_frame_source_artifact_invalid")?).map_err(|_|"manual_frame_source_artifact_unavailable".into())})
        .await.map_err(|_|crate::internal("manual_frame_source_verification_stopped"))?.map_err(|e|crate::conflict(&e))?;
    app.change(|d|bind_source_observation(d,parent,&child,&observed,token,&crate::now())).await
}

fn profile(intent: &Value) -> Value {
    json!({"id":if intent["kind"]=="known_range" {crate::media_frame_sample::RANGE_PROFILE} else {crate::media_frame_sample::OVERVIEW_PROFILE},
        "version":1,"maxFrames":8,"windowMs":1000,"maxImageBytes":8*1024*1024,"maxArtifactBytes":128*1024*1024,
        "maxTotalPixels":64_000_000,"maxDecodedDurationMs":30_000,"maxDecodedFrames":900,"maxPrerollMs":10_000,
        "deadlineMs":30_000,"rangeStepMs":4000,"overviewFrames":6})
}
fn base_usage(request: &Value) -> Value {
    let mut usage = json!({"imageCount":0,"imageBytes":0,"pixels":0});
    for image in rows(&request["postContextBundle"], "members").iter().flat_map(|m| rows(m, "assets"))
        .filter(|a| a["modality"] == "photo").map(|a| &a["photo"])
        .chain(rows(&request["postContextBundle"],"commentPhotos").iter().map(|source|&source["photo"]))
        .chain(rows(request, "optionalFrameRefs")) {
        usage["imageCount"] = json!(usage["imageCount"].as_u64().unwrap() + 1);
        usage["imageBytes"] = json!(usage["imageBytes"].as_u64().unwrap().saturating_add(image["artifact"]["bytes"].as_u64().unwrap_or(0)));
        usage["pixels"] = json!(usage["pixels"].as_u64().unwrap().saturating_add(image["width"].as_u64().unwrap_or(0).saturating_mul(image["height"].as_u64().unwrap_or(0))));
    }
    usage
}
fn make_plan(store: &ArtifactStore, job: &Value, base: Value) -> Result<Value, String> {
    let need = &job["manualFrameRequest"];
    let asset=sample_asset(job);
    let source = &job["retainedSource"];
    let receipt = json!({"source":source["sourceArtifactRef"],"originJobId":source["originJobId"],"checkpoint":source["checkpoint"]});
    let proof = json!({"schemaVersion":1,"kind":"retained_video_source","companyId":need["companyId"],"member":need["member"],"asset":asset,
        "sourceDurationMs":source["sourceDurationMs"],"verifiedReceipt":receipt,
        "verifiedFile":{"sha256":source["sourceArtifactRef"]["sha256"],"bytes":source["sourceArtifactRef"]["bytes"],"receiptSha256":hash(&receipt)}});
    let proof_ref = crate::media_frame_sample_decode::retain_source_proof(store, &proof)?;
    crate::media_frame_sample::plan_sample(&json!({"schemaVersion":1,"needId":need["needId"],"needSha256":need["needSha256"],
        "companyId":need["companyId"],"member":need["member"],"asset":asset,"sourceProofRef":proof_ref,
        "sourceDurationMs":source["sourceDurationMs"],"requestedTimeOrIntent":need["requestedTimeOrIntent"],"profile":profile(&need["requestedTimeOrIntent"]),
        "baseUsage":base,"transportLimits":{"maxImages":16,"maxBytes":32*1024*1024,"maxPixels":64_000_000}}))
}
fn sample_asset(job:&Value)->Value {
    let mut asset=job["manualFrameRequest"]["asset"].clone();
    asset["sourceArtifactRef"]=job["retainedSource"]["sourceArtifactRef"].clone();
    asset["sourceArtifactSha256"]=job["retainedSource"]["sourceArtifactRef"]["sha256"].clone();asset
}
fn saved_tools(t: &SampleTools) -> Value {
    json!({"ffmpeg":t.ffmpeg,"ffprobe":t.ffprobe,"ffmpegSha256":t.ffmpeg_sha256,"ffprobeSha256":t.ffprobe_sha256,
        "ffmpegVersion":t.ffmpeg_version,"ffprobeVersion":t.ffprobe_version,"deadlineMs":t.deadline.as_millis() as u64})
}
fn restore_tools(v: &Value) -> Result<SampleTools, &'static str> {
    if !exact(v, &["ffmpeg","ffprobe","ffmpegSha256","ffprobeSha256","ffmpegVersion","ffprobeVersion","deadlineMs"])
        || !sha(&v["ffmpegSha256"]) || !sha(&v["ffprobeSha256"])
        || ["ffmpeg", "ffprobe", "ffmpegVersion", "ffprobeVersion"].iter().any(|k| txt(v, k).is_empty())
        || v["deadlineMs"].as_u64().is_none_or(|n| n == 0 || n > 30_000) { return Err("manual_frame_tool_capture_invalid"); }
    Ok(SampleTools { ffmpeg:PathBuf::from(txt(v,"ffmpeg")), ffprobe:PathBuf::from(txt(v,"ffprobe")),
        ffmpeg_sha256:txt(v,"ffmpegSha256").into(),ffprobe_sha256:txt(v,"ffprobeSha256").into(),
        ffmpeg_version:txt(v,"ffmpegVersion").into(),ffprobe_version:txt(v,"ffprobeVersion").into(),
        deadline:Duration::from_millis(v["deadlineMs"].as_u64().unwrap()) })
}

fn require_lease(d: &Value, claimed: &Value, token: &OwnerToken, before_dispatch:bool) -> crate::ApiResult<()> {
    crate::runtime_lifecycle::require_admission(d, token, AdmissionClass::Media)?;
    let job = crate::row(d, "jobs", txt(claimed, "id"))?;
    if job["status"] != "running" || job["purpose"] != PURPOSE || job["frameLease"] != claimed["frameLease"]
        || job["frameLease"]["owner"] != token_value(token) || job["manualFrameRequest"] != claimed["manualFrameRequest"]
        || job["retainedSource"] != claimed["retainedSource"] { return Err(crate::conflict("manual_frame_lease_changed")); }
    if before_dispatch {current_capture(d, &job["manualFrameRequest"], true).map_err(crate::conflict)?;}
    else {original_capture_current(d,job).map_err(crate::conflict)?;}
    let source = retained_source(d, &job["manualFrameRequest"]["assetPin"]).map_err(crate::conflict)?;
    if source["sourceArtifactRef"] != job["retainedSource"]["sourceArtifactRef"]
        || source["sourceDurationMs"] != job["retainedSource"]["sourceDurationMs"] {
        return Err(crate::conflict("manual_frame_retained_source_changed"));
    }
    Ok(())
}
fn start(d: &mut Value, claimed: &Value, token: &OwnerToken, plan: &Value, tools: &SampleTools, at: &str) -> crate::ApiResult<()> {
    require_lease(d, claimed, token,true)?;
    crate::media_frame_sample::validate_plan(plan).map_err(|e| crate::conflict(&e))?;
    let need = &claimed["manualFrameRequest"];
    for k in ["needId", "needSha256", "companyId", "member", "requestedTimeOrIntent"] {
        if plan[k] != need[k] { return Err(crate::conflict("manual_frame_plan_changed")); }
    }
    if plan["asset"]!=sample_asset(claimed){return Err(crate::conflict("manual_frame_source_binding_changed"));}
    if plan["profile"] != profile(&need["requestedTimeOrIntent"]) { return Err(crate::conflict("manual_frame_profile_changed")); }
    let job = crate::row_mut(d, "jobs", txt(claimed, "id"))?;
    if job.get("extractionIntent").is_some() { return Err(crate::conflict("manual_frame_extraction_already_reserved")); }
    job["framePlan"] = plan.clone(); job["frameTools"] = saved_tools(tools);
    job["extractionIntent"] = json!({"schemaVersion":1,"status":"reserved","planSha256":plan["planSha256"],
        "lease":claimed["frameLease"],"at":at,"repeatAuthorized":false});
    Ok(())
}
fn result_value(claimed: &Value, decoder: &Value) -> Value {
    let need = &claimed["manualFrameRequest"];
    let mut result = json!({"schemaVersion":1,"contract":RESULT,"origin":ORIGIN,"frameJobId":claimed["id"],"requestId":need["requestId"],
        "needId":need["needId"],"needSha256":need["needSha256"],"companyId":need["companyId"],"member":need["member"],"asset":sample_asset(claimed),
        "requestedTimeOrIntent":need["requestedTimeOrIntent"],"status":"complete","coverage":decoder["coverage"],"frames":decoder["frames"],"decoderResult":decoder});
    result["resultSha256"] = json!(hash(&result)); result
}
fn settle(d: &mut Value, claimed: &Value, token: &OwnerToken, plan: &Value, decoder: &Value, at: &str) -> crate::ApiResult<Value> {
    require_lease(d, claimed, token,false)?;
    let job = crate::row(d, "jobs", txt(claimed, "id"))?;
    if job["framePlan"] != *plan || job["extractionIntent"]["planSha256"] != plan["planSha256"]
        || job["extractionIntent"]["lease"] != claimed["frameLease"] || decoder["status"] != "complete"
        || job["frameObservation"] != *decoder
        || decoder["needSha256"] != claimed["manualFrameRequest"]["needSha256"] || rows(decoder, "frames").is_empty() {
        return Err(crate::conflict("manual_frame_result_settlement_changed"));
    }
    let result = result_value(claimed, decoder);
    let job = crate::row_mut(d, "jobs", txt(claimed, "id"))?;
    job["frameResult"] = result; job["status"] = json!("completed"); job["finishedAt"] = json!(at);
    job["reasonCode"] = json!("manual_frames_complete");
    Ok(job.clone())
}
fn stop(d: &mut Value, claimed: &Value, reason: &str, at: &str) -> crate::ApiResult<Value> {
    let job = crate::row_mut(d, "jobs", txt(claimed, "id"))?;
    if job["status"] != "running" || job["frameLease"] != claimed["frameLease"] || job["manualFrameRequest"] != claimed["manualFrameRequest"] {
        return Err(crate::conflict("manual_frame_stop_lease_changed"));
    }
    // Once an extraction intent exists, an interrupted process or lost commit
    // is an observation gap. It never grants a fresh decoder invocation.
    job["status"] = json!(if job.get("extractionIntent").is_some() || job.get("sourceJobId").is_some() { "unknown" } else { "held" });
    job["reasonCode"] = json!(reason); job["finishedAt"] = json!(at);let stopped=job.clone();
    if let Some(id)=stopped["sourceJobId"].as_str(){
        if let Ok(child)=crate::row_mut(d,"jobs",id){
            if child["kind"]=="manual_frame_source"&&child["status"]=="running"&&child["parentManualFrameRequestId"]==stopped["id"]{
                child["status"]=json!("unknown");child["reasonCode"]=json!(reason);child["finishedAt"]=json!(at);
            }
        }
    }
    Ok(stopped)
}

fn retain_observation(d: &mut Value, claimed: &Value, decoder: &Value) -> crate::ApiResult<()> {
    let job = crate::row_mut(d,"jobs",txt(claimed,"id"))?;
    if job["status"]!="running" || job["frameLease"]!=claimed["frameLease"] || job["manualFrameRequest"]!=claimed["manualFrameRequest"]
        || job["extractionIntent"]["lease"]!=claimed["frameLease"] || job["framePlan"]["planSha256"]!=decoder["planSha256"] {
        return Err(crate::conflict("manual_frame_observation_owner_changed"));
    }
    if job.get("frameObservation").is_some() && job["frameObservation"]!=*decoder { return Err(crate::conflict("manual_frame_observation_immutable")); }
    job["frameObservation"]=decoder.clone(); Ok(())
}
fn recovered_job(d: &Value, job: &Value, store: &ArtifactStore) -> Result<Value,&'static str> {
    if !matches!(txt(job,"status"),"running"|"unknown"|"interrupted") || !job["frameObservation"].is_object() { return Err("manual_frame_observation_missing"); }
    original_capture_current(d,job)?;
    let mut recovered=job.clone(); recovered["frameResult"]=result_value(job,&job["frameObservation"]); recovered["status"]=json!("completed");
    validate_result(d,&recovered,store)?; Ok(recovered)
}
fn recoverable_source(d:&Value,parent:&Value,store:&ArtifactStore)->Result<(Value,Value),&'static str>{
    let child=crate::row(d,"jobs",txt(parent,"sourceJobId")).map_err(|_|"manual_frame_source_child_missing")?;
    let observed=source_observation(d,parent,child)?;
    store.verify(&ArtifactRef::from_json(&observed["sourceArtifactRef"]).map_err(|_|"manual_frame_source_artifact_invalid")?)
        .map_err(|_|"manual_frame_source_artifact_unavailable")?;
    Ok((child.clone(),observed))
}

pub(crate) async fn request(
    axum::extract::State(app): axum::extract::State<crate::App>,
    axum::Extension(actor): axum::Extension<crate::operator_auth::Actor>,
    axum::Json(body): axum::Json<Value>,
) -> crate::ApiResult<axum::Json<Value>> {
    if actor.role != "owner" { return Err(crate::ApiError(axum::http::StatusCode::FORBIDDEN, "Manual frames require owner".into())); }
    parse(&body).map_err(crate::bad)?;
    let token = app.lifecycle_admission_token(AdmissionClass::Preparation).await?;
    let work = app.lifecycle_work.begin(crate::runtime_owned_work::Kind::Preparation)?;
    crate::runtime_owned_work::with_admitted(work, async {
        let admission = app.change(|d| claim(d, &body, &actor.id, &token, &crate::now())).await?;
        let mut claimed = match admission {
            Admission::Replay(job) => {
                if job["status"] == "completed" {
                    let d = app.db.read_manual_frame_context().await?;
                    warm_result(&d, &job, &crate::media_fullframes::store().map_err(|e| crate::internal(&e))?).map_err(crate::conflict)?;
                }
                // Recovery consumes the retained original observation only.
                // An owner restart cannot create a second extraction intent.
                if matches!(txt(&job,"status"),"unknown"|"interrupted") || (job["status"]=="running" && job["frameLease"]["owner"]!=token_value(&token)) {
                    let snapshot=app.db.read_manual_frame_context().await?;let original=job.clone();
                    let (recovered,source)=tokio::task::spawn_blocking(move||{
                        let Ok(store)=crate::media_fullframes::store() else{return (None,None)};
                        (recovered_job(&snapshot,&original,&store).ok(),recoverable_source(&snapshot,&original,&store).ok())
                    }).await.map_err(|_|crate::internal("manual_frame_reconciliation_stopped"))?;
                    let outcome=app.change(|d| {
                        crate::runtime_lifecycle::require_admission(d,&token,AdmissionClass::Preparation)?;
                        if crate::row(d,"jobs",txt(&job,"id"))?!=&job { return Err(crate::conflict("manual_frame_recovery_owner_changed")); }
                        if let Some(mut recovered)=recovered {
                            original_capture_current(d,&recovered).map_err(crate::conflict)?;
                            recovered["recoveredBy"]=token_value(&token); recovered["finishedAt"]=json!(crate::now());
                            recovered["reasonCode"]=json!("manual_frames_original_observation_recovered");
                            *crate::row_mut(d,"jobs",txt(&job,"id"))?=recovered.clone(); Ok(axum::Json(recovered))
                        } else {
                            // A download checkpoint can settle its ORIGINAL source
                            // observation, but never re-run the download or invent
                            // an extraction intent after an UNKNOWN gap.
                            if let Some((child,observed))=source{
                                bind_source_observation(d,&job,&child,&observed,&token,&crate::now())?;
                            }
                            let current=crate::row_mut(d,"jobs",txt(&job,"id"))?; current["status"]=json!("unknown");
                            current["reasonCode"]=json!("manual_frame_original_observation_reconciliation_required"); Ok(axum::Json(current.clone()))
                        }
                    }).await?;
                    if outcome.0["status"]=="completed"{
                        refresh(&app).await?;app.preparation_wake.notify_one();
                    }
                    return Ok(outcome);
                }
                return Ok(axum::Json(job));
            }
            Admission::Fresh(job) => job,
        };
        if claimed.get("sourceJobId").is_some() && !claimed["retainedSource"].is_object() {
            match acquire_source_only(&app,&claimed,&token).await {
                Ok(job)=>claimed=job,
                Err(_)=>return app.change(|d|stop(d,&claimed,"manual_frame_source_acquisition_unresolved",&crate::now())).await.map(axum::Json),
            }
        }
        let store = match crate::media_fullframes::store() {
            Ok(v) => v, Err(_) => return app.change(|d| stop(d, &claimed, "manual_frame_store_unavailable", &crate::now())).await.map(axum::Json),
        };
        if claimed.get("sourceJobId").is_none() {
            let root=store.root().to_path_buf();let reference=claimed["retainedSource"]["sourceArtifactRef"].clone();
            let present=tokio::task::spawn_blocking(move||ArtifactRef::from_json(&reference).ok().is_some_and(|r|
                ArtifactStore::open(&root).is_ok_and(|s|s.verify(&r).is_ok())))
                .await.map_err(|_|crate::internal("manual_frame_source_verification_stopped"))?;
            if !present {
                claimed=app.change(|d|{
                    if crate::row(d,"jobs",txt(&claimed,"id"))?!=&claimed||claimed.get("extractionIntent").is_some(){return Err(crate::conflict("manual_frame_source_owner_changed"));}
                    let child=new_source_child(d,&claimed,&token,&crate::now())?;
                    crate::row_mut(d,"jobs",txt(&claimed,"id"))?["sourceJobId"]=child["id"].clone();crate::list_mut(d,"jobs").push(child);
                    Ok(crate::row(d,"jobs",txt(&claimed,"id"))?.clone())
                }).await?;
                match acquire_source_only(&app,&claimed,&token).await {
                    Ok(job)=>claimed=job,
                    Err(_)=>return app.change(|d|stop(d,&claimed,"manual_frame_original_source_reacquisition_unresolved",&crate::now())).await.map(axum::Json),
                }
            }
        }
        let tools = match SampleTools::from_env(&app.lifecycle_work, Duration::from_secs(30)).await {
            Ok(v) => v, Err(reason) => return app.change(|d| stop(d, &claimed, &reason, &crate::now())).await.map(axum::Json),
        };
        let d = app.db.read_manual_frame_context().await?;
        let base = if let Some(id) = body["prepareJobId"].as_str() { base_usage(&crate::row(&d, "jobs", id)?["prepareBundle"]["request"]) }
            else { json!({"imageCount":0,"imageBytes":0,"pixels":0}) };
        let planned = make_plan(&store, &claimed, base); drop(d);
        let plan = match planned { Ok(v) => v, Err(reason) => return app.change(|d| stop(d, &claimed, &reason, &crate::now())).await.map(axum::Json) };
        if app.change(|d| start(d, &claimed, &token, &plan, &tools, &crate::now())).await.is_err() {
            return app.change(|d| stop(d, &claimed, "manual_frame_predecode_currentness_changed", &crate::now())).await.map(axum::Json);
        }
        let decoded = decode_sample(&store, &plan, &tools, &app.lifecycle_work).await;
        let decoder = match decoded {
            Ok(v) if v["status"] == "complete" && !rows(&v,"frames").is_empty() && verify_sample_result(&store,&plan,&v,&tools).is_ok() => v,
            other => { let reason=other.err().unwrap_or_else(||"manual_frame_acquisition_incomplete".into());
                return app.change(|d|stop(d,&claimed,&reason,&crate::now())).await.map(axum::Json); }
        };
        app.change(|d|retain_observation(d,&claimed,&decoder)).await?;
        match app.change(|d| settle(d, &claimed, &token, &plan, &decoder, &crate::now())).await {
            Ok(job) => {
                let d=app.db.read_manual_frame_context().await?;
                warm_result(&d,&job,&store).map_err(crate::conflict)?;
                app.preparation_wake.notify_one();
                Ok(axum::Json(job))
            },
            Err(_) => app.change(|d| stop(d, &claimed, "manual_frame_result_commit_unconfirmed", &crate::now())).await.map(axum::Json),
        }
    }).await
}

fn validate_native_result(d: &Value, job: &Value) -> Result<(), &'static str> {
    let need = &job["manualFrameRequest"];
    current_capture(d, need, false)?;
    let result = &job["frameResult"];
    if job["purpose"] != PURPOSE || job["kind"] != "media" || job["status"] != "completed"
        || job["id"] != need["requestId"] || job["account"] != need["companyId"]
        || job["frameLease"] != job["extractionIntent"]["lease"]
        || job["extractionIntent"]["planSha256"] != job["framePlan"]["planSha256"]
        || result != &result_value(job, &result["decoderResult"]) || result["decoderResult"]["status"] != "complete"
        || job["frameObservation"]!=result["decoderResult"]
        || rows(result, "frames").is_empty() {
        return Err("manual_frame_result_native_owner_unproven");
    }
    for k in ["needId","needSha256","companyId","member","requestedTimeOrIntent"] {
        if job["framePlan"][k]!=need[k] || result["decoderResult"][k]!=need[k] {return Err("manual_frame_result_native_pins_changed");}
    }
    if job["framePlan"]["asset"]!=sample_asset(job)||result["decoderResult"]["asset"]!=sample_asset(job) {return Err("manual_frame_result_source_binding_changed");}
    if job["framePlan"]["profile"]!=profile(&need["requestedTimeOrIntent"]) {return Err("manual_frame_result_profile_changed");}
    crate::media_frame_sample::validate_plan(&job["framePlan"]).map_err(|_|"manual_frame_result_plan_changed")?;
    Ok(())
}
fn validate_result(d: &Value, job: &Value, store: &ArtifactStore) -> Result<(), &'static str> {
    validate_native_result(d,job)?;
    let result=&job["frameResult"];
    let tools = restore_tools(&job["frameTools"])?;
    verify_sample_result(store, &job["framePlan"], &result["decoderResult"], &tools).map_err(|_| "manual_frame_result_cas_unproven")
}

// Exact immutable evidence is verified outside the DB writer. This bounded
// cache grants no extraction/paid authority and cannot survive a process restart
// or a lifecycle owner change. Failed re-verification revokes the prior proof.
static WARMED: std::sync::OnceLock<std::sync::Mutex<std::collections::BTreeMap<String,std::time::Instant>>> = std::sync::OnceLock::new();
fn cache() -> &'static std::sync::Mutex<std::collections::BTreeMap<String,std::time::Instant>> {
    WARMED.get_or_init(||std::sync::Mutex::new(std::collections::BTreeMap::new()))
}
fn proof_key(d:&Value,job:&Value)->String {
    hash(&json!([d["account"],d["runtimeLifecycle"]["owner"],job["id"],job["manualFrameRequest"],job["frameLease"],
        job["retainedSource"],job["extractionIntent"],job["framePlan"],job["frameTools"],job["frameObservation"],job["frameResult"]]))
}
fn warm_result(d:&Value,job:&Value,store:&ArtifactStore)->Result<(),&'static str> {
    let key=proof_key(d,job);
    let checked=validate_result(d,job,store);
    let mut proofs=cache().lock().map_err(|_|"manual_frame_proof_cache_unavailable")?;
    proofs.retain(|_,until|*until>std::time::Instant::now());
    if checked.is_ok() {if proofs.len()>=512 {proofs.clear();}proofs.insert(key,std::time::Instant::now()+Duration::from_secs(120));}
    else {proofs.remove(&key);}
    checked
}
fn require_warmed(d:&Value,job:&Value)->Result<(),&'static str> {
    validate_native_result(d,job)?;
    if !cache().lock().map_err(|_|"manual_frame_proof_cache_unavailable")?.get(&proof_key(d,job))
        .is_some_and(|until|*until>std::time::Instant::now()) {return Err("manual_frame_proof_not_warmed");}
    Ok(())
}
/// Call from an outside-writer evidence refresh (including restart) before
/// preparation capture/admission. Invalid jobs revoke proofs and hold only
/// their own selected post; unrelated post work is not blocked here.
pub(crate) fn warm_context(d:&Value)->Result<(),String> {
    let jobs=rows(d,"jobs").iter().filter(|j|j["purpose"]==PURPOSE&&j["status"]=="completed"&&current_capture(d,&j["manualFrameRequest"],false).is_ok()).collect::<Vec<_>>();
    if jobs.is_empty(){return Ok(());}
    let store=crate::media_fullframes::store()?;
    for job in jobs {let _=warm_result(d,job,&store);}
    Ok(())
}
pub(crate) async fn refresh(app:&crate::App)->crate::ApiResult<()> {
    let snapshot=app.db.read_manual_frame_context().await?;
    tokio::task::spawn_blocking(move||warm_context(&snapshot)).await
        .map_err(|_|crate::internal("manual_frame_evidence_refresh_task_failed"))?
        .map_err(|e|crate::conflict(&e))
}
fn frame_refs(job: &Value) -> Vec<Value> {
    let need = &job["manualFrameRequest"]; let result = &job["frameResult"];
    rows(result, "frames").iter().map(|frame| {
        let mut r = frame.clone();
        for (key,value) in [
            ("origin",json!(ORIGIN)),("manualRequestId",job["id"].clone()),("needId",need["needId"].clone()),
            ("needSha256",need["needSha256"].clone()),("resultSha256",result["resultSha256"].clone()),("frameJobId",job["id"].clone()),
            ("companyId",need["companyId"].clone()),("postId",need["member"]["postId"].clone()),("connectorBinding",need["member"]["connectorBinding"].clone()),
            ("attachmentIndex",need["asset"]["attachmentIndex"].clone()),("attachmentIdentity",need["asset"]["attachmentIdentity"].clone()),
            ("sourceVersion",need["asset"]["sourceVersion"].clone()),("sourceArtifactSha256",result["asset"]["sourceArtifactSha256"].clone()),
            ("affectedRecipientIds",json!([])),
        ] { r[key] = value; }
        r
    }).collect()
}

fn selected_for(request: &Value, job: &Value) -> bool {
    rows(&request["postContextBundle"], "members").iter().any(|m| m["canonicalPostId"] == job["manualFrameRequest"]["member"]["postId"]
        && m["postSourceVersion"] == job["manualFrameRequest"]["asset"]["sourceVersion"]
        && m["connectorBinding"] == job["manualFrameRequest"]["member"]["connectorBinding"])
}
pub(crate) fn require_no_pending(d: &Value, job: &Value) -> Result<(), &'static str> {
    let request = &job["prepareBundle"]["request"];
    for manual in rows(d,"jobs").iter().filter(|m|relevant_to_capture(job,m)) {
        if manual["status"] == "completed" {
            if !rows(request,"manualFrameRequestIds").contains(&manual["id"])
                || frame_refs(manual).iter().any(|r|!rows(request,"optionalFrameRefs").contains(r)) {
                return Err("manual_frame_unpaid_capture_refresh_required");
            }
            continue;
        }
        return Err("manual_frame_requested_material_unresolved");
    }
    Ok(())
}
fn relevant_to_capture(parent:&Value,manual:&Value)->bool{
    manual["purpose"]==PURPOSE && selected_for(&parent["prepareBundle"]["request"],manual)
        && manual["manualFrameRequest"]["request"]["prepareJobId"].as_str().is_none_or(|id|Some(id)==parent["id"].as_str())
}
pub(crate) fn is_wait_reason(reason:&str)->bool{
    matches!(reason,"manual_frame_requested_material_unresolved"|"manual_frame_unpaid_capture_refresh_required")
}
/// A worker may wait only for its exact unpaid capture's still-running local
/// work. UNKNOWN/interrupted/held and unbound completed omissions are finite.
pub(crate) fn wait_classification(d:&Value,parent:&Value)->&'static str{
    if require_no_pending(d,parent).is_ok(){return "ready";}
    let mut active=false;
    for job in rows(d,"jobs").iter().filter(|m|relevant_to_capture(parent,m)){
        match txt(job,"status"){
            "running"=>active=true,
            "completed" if job["manualFrameRequest"]["request"]["prepareJobId"]==parent["id"]=>active=true,
            _=>return "terminal",
        }
    }
    if active{"active"}else{"terminal"}
}

/// Called after the mandatory bundle is constructed. Existing selection is
/// frozen, including an empty list; later native jobs cannot broaden it.
pub(crate) fn attach_request(d: &Value, request: &mut Value) -> Result<(), &'static str> {
    for r in rows(request,"optionalFrameRefs") {
        if r["origin"]!=ORIGIN && crate::row(d,"jobs",txt(r,"frameJobId")).is_ok_and(|j|j["purpose"]==PURPOSE) {
            return Err("manual_frame_origin_changed");
        }
    }
    if request.get("manualFrameRequestIds").is_none() {
        let ids = if rows(request,"optionalFrameRefs").iter().any(|r|r["origin"]==ORIGIN) {
            rows(request,"optionalFrameRefs").iter().filter(|r|r["origin"]==ORIGIN).map(|r|r["manualRequestId"].clone()).collect::<Vec<_>>()
        } else if request["purpose"] == "editorial_review" { vec![] }
        else {
            rows(d,"jobs").iter().filter(|j|j["purpose"]==PURPOSE&&j["status"]=="completed"&&selected_for(request,j))
                .map(|j|j["id"].clone()).collect::<Vec<_>>()
        };
        let mut unique = std::collections::BTreeSet::new();
        request["manualFrameRequestIds"] = json!(ids.into_iter().filter(|id|unique.insert(id.to_string())).collect::<Vec<_>>());
    }
    // An explicit manual request may target an already scheduled UNPAID
    // capture. Enrichment is derived only from its exact original digest and
    // unchanged non-material input. Once paid, or once that digest moves,
    // this branch cannot add an ID. Scope refresh still checks the whole delta.
    let mut refined=false;
    // Collect before modifying `request`; selection never borrows a mutable
    // request through an iterator across the append below.
    let candidates=rows(d,"jobs").iter().filter(|j|j["purpose"]==PURPOSE&&j["status"]=="completed"&&selected_for(request,j)).collect::<Vec<_>>();
    for manual in candidates {
        let body=&manual["manualFrameRequest"]["request"];
        let Some(id)=body["prepareJobId"].as_str() else {continue};
        let Ok(owner)=crate::row(d,"jobs",id) else {continue};
        let mut incoming=request.clone(); let mut saved=owner["prepareBundle"]["request"].clone();
        for v in [&mut incoming,&mut saved] { if let Some(o)=v.as_object_mut() { for k in ["postContextBundle","materialReadiness","visualSelection","visualNeedContract","mandatoryMaterialContract","manualFrameRequestIds","optionalFrameRefs"] {o.remove(k);} } }
        if incoming==saved && owner["prepareBundle"]["digest"]==body["expectedPrepareBundleDigest"]
            && require_unpaid(d,body,&manual["manualFrameRequest"]["assetPin"]).is_ok()
            && !rows(request,"manualFrameRequestIds").contains(&manual["id"]) {
            request["manualFrameRequestIds"].as_array_mut().ok_or("manual_frame_selection_invalid")?.push(manual["id"].clone());
            refined=true;
        }
    }
    let ids = request["manualFrameRequestIds"].as_array().filter(|v|v.len()<=8).ok_or("manual_frame_selection_invalid")?;
    let mut seen = std::collections::BTreeSet::new(); let mut expected = Vec::new();
    for id in ids {
        if !uuid(id) || !seen.insert(id.to_string()) { return Err("manual_frame_selection_invalid"); }
        let job = crate::row(d,"jobs",id.as_str().unwrap()).map_err(|_|"manual_frame_selection_missing")?;
        if !selected_for(request,job) { return Err("manual_frame_selection_foreign"); }
        require_warmed(d,job)?;
        expected.extend(frame_refs(job));
    }
    let prior = rows(request,"optionalFrameRefs").iter().filter(|r|r["origin"]==ORIGIN).cloned().collect::<Vec<_>>();
    if !prior.is_empty() && prior != expected && !(refined&&expected.starts_with(&prior)) { return Err("manual_frame_selection_changed"); }
    let mut refs = rows(request,"optionalFrameRefs").iter().filter(|r|r["origin"]!=ORIGIN).cloned().collect::<Vec<_>>(); refs.extend(expected);
    if !refs.is_empty() { request["optionalFrameRefs"] = json!(refs); }
    let used = base_usage(request);
    if !rows(request,"optionalFrameRefs").is_empty() && (used["imageCount"].as_u64().unwrap()>16 || used["imageBytes"].as_u64().unwrap()>32*1024*1024 || used["pixels"].as_u64().unwrap()>64_000_000) {
        return Err("manual_frame_combined_transport_exceeded");
    }
    Ok(())
}
pub(crate) fn require_refs(d: &Value, request: &Value) -> Result<(), &'static str> {
    let mut checked = request.clone(); attach_request(d, &mut checked)?;
    if checked.get("optionalFrameRefs").cloned().unwrap_or_else(||json!([])) != request.get("optionalFrameRefs").cloned().unwrap_or_else(||json!([]))
        || checked.get("manualFrameRequestIds").cloned().unwrap_or_else(||json!([])) != request.get("manualFrameRequestIds").cloned().unwrap_or_else(||json!([])) {
        return Err("manual_frame_captured_selection_changed");
    }
    Ok(())
}

#[cfg(test)]
#[path = "manual_frame_request_tests.rs"]
mod tests;
#[cfg(test)]
pub(crate) use tests::{NativeExtractionFixture,native_app,native_fixture_begin,native_read_fixture};

/// Begin a release/floor corpus observation on the caller's explicitly pinned
/// isolated store. Requires an existing admitted source checkpoint; it cannot
/// invent a download child, fabricate pixels or use a fake decoder process.
/// The returned plan must be decoded by the root's real FFmpeg runner.
#[cfg(test)]
pub(crate) fn fixture_begin_pinned(d:&mut Value,body:&Value,actor:&str,token:&OwnerToken,tools:&SampleTools,store:&ArtifactStore,at:&str)->crate::ApiResult<Value>{
    let post=crate::row(d,"posts",txt(body,"postId"))?;
    let index=body["attachmentIndex"].as_u64().and_then(|value|usize::try_from(value).ok()).ok_or_else(||crate::bad("manual_frame_fixture_slot_invalid"))?;
    let pin=crate::media_speech_assets::capture(d,post,index).map_err(|reason|crate::conflict(&reason))?;
    let source=retained_source(d,&pin).map_err(crate::conflict)?;
    store.verify(&ArtifactRef::from_json(&source["sourceArtifactRef"]).map_err(|_|crate::bad("manual_frame_fixture_source_invalid"))?).map_err(|_|crate::conflict("manual_frame_fixture_source_unavailable"))?;
    restore_tools(&saved_tools(tools)).map_err(crate::bad)?;
    let mut next=d.clone();
    let claimed=match claim(&mut next,body,actor,token,at)?{Admission::Fresh(job)=>job,Admission::Replay(_)=>return Err(crate::conflict("manual_frame_fixture_already_attempted"))};
    if claimed.get("sourceJobId").is_some(){return Err(crate::conflict("manual_frame_fixture_requires_retained_source"));}
    let base=if let Some(id)=body["prepareJobId"].as_str(){base_usage(&crate::row(&next,"jobs",id)?["prepareBundle"]["request"])}else{json!({"imageCount":0,"imageBytes":0,"pixels":0})};
    let plan=make_plan(store,&claimed,base).map_err(|reason|crate::conflict(&reason))?;
    start(&mut next,&claimed,token,&plan,tools,at)?;*d=next;
    Ok(json!({"claimed":claimed,"plan":plan}))
}
/// Admit only the exact real decoder observation for the captured plan/tools,
/// source CAS and current owner. Every production settlement validator runs.
#[cfg(test)]
pub(crate) fn fixture_settle_pinned(d:&mut Value,fixture:&Value,decoder:&Value,token:&OwnerToken,tools:&SampleTools,store:&ArtifactStore,at:&str)->crate::ApiResult<Value>{
    let claimed=&fixture["claimed"];let plan=&fixture["plan"];
    if crate::row(d,"jobs",txt(claimed,"id"))?["frameTools"]!=saved_tools(tools){return Err(crate::conflict("manual_frame_fixture_tools_changed"));}
    verify_sample_result(store,plan,decoder,tools).map_err(|reason|crate::conflict(&reason))?;
    let mut next=d.clone();retain_observation(&mut next,claimed,decoder)?;
    let job=settle(&mut next,claimed,token,plan,decoder,at)?;
    warm_result(&next,&job,store).map_err(crate::conflict)?;*d=next;Ok(job)
}


/// Fixture input keeps the source version as an explicit native-capture marker;
/// only the separately derived DTO is passed to unchanged production guards.
#[cfg(test)]
pub(crate) fn fixture_native_request(d:&Value,input:&Value)->crate::ApiResult<(Value,Value)> {
    if !exact(input,&["requestId","postId","attachmentIndex","expectedSourceVersion","requestedTimeOrIntent","reason"]) {
        return Err(crate::bad("manual_frame_floor_input_not_closed"));
    }
    let post=crate::row(d,"posts",txt(input,"postId"))?;
    let index=input["attachmentIndex"].as_u64().and_then(|n|usize::try_from(n).ok())
        .ok_or_else(||crate::bad("manual_frame_fixture_slot_invalid"))?;
    let pin=crate::media_speech_assets::capture(d,post,index).map_err(|e|crate::conflict(&e))?;
    let mut body=input.clone();
    if body["expectedSourceVersion"]=="NATIVE-CAPTURED" {body["expectedSourceVersion"]=pin["sourceVersion"].clone();}
    parse(&body).map_err(crate::bad)?;
    if body["expectedSourceVersion"]!=pin["sourceVersion"] {return Err(crate::conflict("manual_frame_source_changed"));}
    require_unpaid(d,&body,&pin).map_err(crate::conflict)?;
    Ok((body,pin))
}
/// Native-generated source checkpoint only. It is a synthetic local fixture,
/// never evidence that a production download/import or provider call occurred.
#[cfg(test)]
pub(crate) fn fixture_retain_generated_source(
    d:&mut Value,input:&Value,generated:&crate::media_frame_sample_decode::FixtureGeneratedSource,
    token:&OwnerToken,tools:&SampleTools,store:&ArtifactStore,at:&str,
)->crate::ApiResult<Value> {
    crate::runtime_lifecycle::require_admission(d,token,AdmissionClass::Preparation)?;
    crate::runtime_lifecycle::require_admission(d,token,AdmissionClass::Media)?;
    let (body,pin)=fixture_native_request(d,input)?;
    if pin["companyId"]!="BAW Russia" || rows(d,"jobs").iter().any(|job|
        job["id"]==body["requestId"] || job["result"]["visualProgress"]["assetPin"]==pin)
    {return Err(crate::conflict("manual_frame_floor_source_already_present"));}
    let observation=generated.verify(store,&pin,tools).map_err(|e|crate::conflict(&e))?;
    if observation["hasAudio"]!=false || observation["hasVideo"]!=true {
        return Err(crate::conflict("manual_frame_floor_source_probe_changed"));
    }
    let mut next=d.clone();
    let progress=json!({"schemaVersion":2,"account":pin["companyId"],
        "sourcePostId":pin["postId"],"sourcePostKey":pin["postKey"],
        "connectorBinding":pin["connectorBinding"],"sourceVersion":pin["sourceVersion"],"assetPin":pin,
        "source":observation["source"],"sourceIdentity":{"account":pin["companyId"],"postKey":pin["postKey"],
            "mediaSha256":observation["source"]["sha256"],"durationMs":observation["durationMs"]}});
    crate::media_speech_assets::require_progress(&next,&progress).map_err(|e|crate::conflict(&e))?;
    let job=json!({"id":crate::id(),"kind":"media","purpose":"floor_generated_source_only","status":"completed",
        "classification":"SYNTHETIC-NATIVE-CORPUS","sourceOrigin":"genuine-local-ffmpeg-testsrc",
        "account":pin["companyId"],"connectorBinding":pin["connectorBinding"],"refId":pin["postId"],
        "fixtureOwner":token_value(token),"fixtureSourceObservation":observation,
        "createdAt":at,"finishedAt":at,"modelCalled":false,"providerEffects":0,"asrEffects":0,
        "result":{"visualProgress":progress}});
    crate::list_mut(&mut next,"jobs").push(job.clone());
    let retained=retained_source(&next,&pin).map_err(crate::conflict)?;
    if retained["originJobId"]!=job["id"] || retained["sourceArtifactRef"]!=observation["source"]
        || retained["sourceDurationMs"]!=observation["durationMs"]
    {return Err(crate::conflict("manual_frame_floor_source_checkpoint_changed"));}
    *d=next;
    Ok(json!({"kind":"native-floor-generated-source-admission","classification":"SYNTHETIC-NATIVE-CORPUS",
        "input":input,"request":body,"assetPin":pin,"sourceJob":job,"observation":observation}))
}
#[cfg(test)]
mod floor_native_request_contract_tests {
    use super::*;
    #[test]
    fn native_capture_marker_is_derived_separately_and_production_parse_stays_strict() {
        let mut d=crate::empty();crate::accounts::initialize(&mut d,crate::accounts::Profile::BawRussia).unwrap();
        d["posts"]=json!([{"id":"floor-post","postKey":"floor-post","account":"BAW Russia",
            "connectorBinding":crate::active_binding(&d).unwrap().to_json(),
            "attachments":[{"type":"video","url":"https://example.invalid/floor.mp4"}]}]);
        let input=json!({"requestId":crate::id(),"postId":"floor-post","attachmentIndex":0,
            "expectedSourceVersion":"NATIVE-CAPTURED","requestedTimeOrIntent":{"kind":"known_range",
                "timelineBasis":"relative_video_start","startMs":1000,"endMs":2000},"reason":"Synthetic floor source"});
        assert!(parse(&input).is_err());
        let (body,pin)=fixture_native_request(&d,&input).unwrap();
        assert_eq!(input["expectedSourceVersion"],"NATIVE-CAPTURED");
        assert_eq!(body["expectedSourceVersion"],pin["sourceVersion"]);parse(&body).unwrap();
        for case in ["version","slot","extra","request-id","range"] {
            let mut changed=input.clone();
            match case {
                "version"=>changed["expectedSourceVersion"]=json!("a".repeat(64)),
                "slot"=>changed["attachmentIndex"]=json!(1),
                "extra"=>changed["sourceCheckpoint"]=json!({}),
                "request-id"=>changed["requestId"]=json!("asserted"),
                _=>changed["requestedTimeOrIntent"]["endMs"]=json!(0),
            }
            assert!(fixture_native_request(&d,&changed).is_err(),"{case}");
        }
    }
}
