//! Explicit cached-source audio work. The visual job is an immutable origin,
//! not a job whose partial frames can be reclassified as complete.
use super::*;
use crate::{ApiResult, App};

pub(super) const TEXT_PROFILE:&str="full_audio_screen_text_v1";
pub(super) const SPEECH_PROFILE:&str="full_video_speech_v1";
fn speech_pin(d:&Value,origin:&str,epoch:u64)->ApiResult<Value>{
    let mut expected=pin(d,origin,epoch)?;
    let profile=crate::row(d,"jobs",origin)?["acquisitionProfile"].clone();
    if profile!=SPEECH_PROFILE&&profile!=TEXT_PROFILE{return Err(crate::conflict("Cached speech acquisition profile changed"));}
    let p=&expected["progress"];
    if p["phase"]!="inventory"&&!(p["phase"]=="held"&&p["resumePhase"]=="inventory"){return Err(crate::conflict("Cached speech requires retained source checkpoint"));}
    expected["profile"]=json!(SPEECH_PROFILE);expected["originAcquisitionProfile"]=profile;Ok(expected)
}
fn text_pin(d:&Value,origin:&str,epoch:u64)->ApiResult<Value>{
    let mut expected=pin(d,origin,epoch)?;
    if crate::row(d,"jobs",origin)?["acquisitionProfile"]!=TEXT_PROFILE{
        return Err(crate::conflict("Cached text acquisition profile changed"));
    }
    let p=&expected["progress"];
    if p["phase"]!="inventory"&&!(p["phase"]=="held"&&p["resumePhase"]=="inventory"){
        return Err(crate::conflict("Cached text requires the downloaded source checkpoint"));
    }
    expected["profile"]=json!(TEXT_PROFILE);Ok(expected)
}
fn current_pin(d:&Value,expected:&Value)->ApiResult<Value>{
    let origin=text(expected,"originJobId");
    let epoch=expected["originEpoch"].as_u64().ok_or_else(||crate::conflict("Cached audio epoch missing"))?;
    if expected["profile"]==TEXT_PROFILE{text_pin(d,origin,epoch)}
    else if expected["profile"]==SPEECH_PROFILE{speech_pin(d,origin,epoch)}
    else if expected["profile"].is_null(){pin(d,origin,epoch)}
    else{Err(crate::conflict("Unknown cached acquisition profile"))}
}

fn pin(d:&Value,origin_id:&str,epoch:u64)->ApiResult<Value>{
    let origin=crate::row(d,"jobs",origin_id)?;
    let p=&origin["result"]["visualProgress"];
    let post=crate::row(d,"posts",text(p,"sourcePostId"))?;
    crate::media_speech_assets::require_progress(d,p).map_err(|e|crate::conflict(&e))?;
    if p.get("assetPin").is_some()&&origin["videoSpeechAssetPin"]!=p["assetPin"]{return Err(crate::conflict("Cached speech origin asset changed"));}
    let policy=crate::post_media_policy::effective(d,post)?;
    let required_asset=p.get("assetPin").is_some()&&origin["acquisitionProfile"]==SPEECH_PROFILE;
    if (policy["mode"]!="full_audio_only"&&!required_asset) || origin["kind"]!="media" || !current_job(origin) || origin["refId"]!=post["id"]
        || !matches!(text(origin,"status"),"failed"|"queued"|"paused") || !p["leaseId"].is_null()
        || p["leaseEpoch"].as_u64()!=Some(epoch) || p["schemaVersion"]!=2
        || !matches!(text(p,"phase"),"held"|"inventory"|"scan"|"select"|"finalize")
        || (p["phase"]=="held" && !matches!(text(p,"resumePhase"),"inventory"|"scan"|"select"|"finalize"))
        || p["account"]!=d["account"] || origin["account"]!=d["account"]
        || p["connectorBinding"]!=crate::active_binding(d)?.to_json()
        || p["sourceVersion"]!=crate::media_fullframes::source_version(post,account_scope(d)?)
        || p["sourcePostKey"]!=post["postKey"] || p["materialEpoch"]!=material_epoch(d,post)
        || p["sourceIdentity"]["account"]!=p["account"] || p["sourceIdentity"]["postKey"]!=p["sourcePostKey"]
        || p["sourceIdentity"]["mediaSha256"]!=p["source"]["sha256"]
        || p["sourceIdentity"]["durationMs"].as_u64().is_none_or(|n|n==0){
        return Err(crate::conflict("Cached audio policy, source, or visual checkpoint changed"));
    }
    crate::media_fullframes::reference(&p["source"]).map_err(|e|crate::conflict(&e))?;
    Ok(json!({"originJobId":origin_id,"originEpoch":epoch,"progress":p,"policy":policy}))
}
fn required_ready(d:&Value,post:&Value,progress:&Value,at:&str)->ApiResult<bool>{
    if let Some(pin)=progress.get("assetPin"){crate::media_speech_assets::ready(d,pin,at).map_err(|e|crate::conflict(&e))}
    else{has_required_media(d,post,at)}
}

fn reserve(d:&mut Value,origin_id:&str,epoch:u64,at:&str)->ApiResult<(String,Value)>{
    let expected=pin(d,origin_id,epoch)?;
    reserve_pinned(d,expected,at)
}
fn reserve_pinned(d:&mut Value,expected:Value,at:&str)->ApiResult<(String,Value)>{
    let origin_id=text(&expected,"originJobId");
    if rows(d,"jobs").iter().any(|j|
        (matches!(text(j,"kind"),"media"|"media_audio") && matches!(text(j,"status"),"running"|"unknown"|"dispatching"))
        || (j["kind"]=="media_audio" && j["audioPin"]["originJobId"]==origin_id
            && j["audioPin"]["policy"]==expected["policy"]
            && j["audioPin"]["progress"]["source"]==expected["progress"]["source"])){
        return Err(crate::conflict("Cached audio work active or already attempted; inspect its durable result"));
    }
    let id=crate::id();
    let account=d["account"].clone();let binding=crate::active_binding(d)?.to_json();
    let purpose=if expected["profile"]==TEXT_PROFILE{"cached_screen_text"}else if expected["profile"]==SPEECH_PROFILE{"required_video_speech"}else{"explicit_cached_audio_only"};
    crate::list_mut(d,"jobs").push(json!({"id":id,"kind":"media_audio","purpose":purpose,
        "account":account,"connectorBinding":binding,"refId":expected["progress"]["sourcePostId"],
        "status":"running","createdAt":at,"startedAt":at,"audioPin":expected}));
    crate::audit(d,"media.cached_audio_requested",&id);
    Ok((id,expected))
}

fn validate_result(result:&Value,expected:&Value)->ApiResult<()>{
    let p=&expected["progress"];
    let materials=result["materials"].as_array().ok_or_else(||crate::conflict("Cached audio result invalid"))?;
    let screen_text=expected["profile"]==TEXT_PROFILE;
    let max_materials=if screen_text{2}else{1};
    if materials.is_empty() || materials.len()>max_materials{return Err(crate::conflict("Cached acquisition material count invalid"));}
    let m=&materials[0];let t=&m["transcription"];
    let expected_ms=p["sourceIdentity"]["durationMs"].as_u64().unwrap_or(0);
    let duration_ms=t["mediaDurationSeconds"].as_f64().filter(|n|n.is_finite()&&*n>0.0&&*n<1e12).map(|n|(n*1000.0).round() as u64);
    let audio_seconds=t["audioDurationSeconds"].as_f64().filter(|n|n.is_finite()&&*n>0.0);
    if m["kind"]!="transcript" || m["account"]!=p["account"] || m["postKey"]!=p["sourcePostKey"]
        || m["mediaSha256"]!=p["source"]["sha256"] || m["sourceUrl"]!=p["sourceProjection"]["sourceUrl"]
        || text(m,"text").trim().is_empty() || t["partial"]!=false
        || t["sourceVersion"]!=p["sourceVersion"]
        || !crate::knowledge::proven_full_audio(t,text(p,"sourceVersion"))
        || !matches!(text(t,"coverage"),"full_audio"|"no_audio_stream")
        || (t["coverage"]=="full_audio" && audio_seconds.is_none_or(|n|n+0.25<(expected_ms as f64/1000.0)))
        || (t["coverage"]=="no_audio_stream" && (t["audioStatus"]!="no_audio_stream" || !t["audioDurationSeconds"].is_null()))
        || duration_ms.is_none_or(|n|n.abs_diff(expected_ms)>1000){
        return Err(crate::conflict("Cached audio identity or full coverage not proven"));
    }
    if screen_text{
        let ocr=&t["ocr"];
        if !matches!(text(ocr,"status"),"completed"|"no_text_found"|"not_applicable"|"unavailable"|"failed"|"partial")
            || ocr["sourceVersion"]!=p["sourceVersion"] || ocr["exhaustive"]!=false
            || (matches!(text(ocr,"status"),"completed"|"no_text_found"|"partial")
                && (ocr["coverage"]!="sampled_frames"||ocr["sampledFrames"].as_u64().is_none_or(|n|n==0||n>30)
                    ||ocr["failedFrames"].as_u64().is_none()))
            || (matches!(text(ocr,"status"),"completed"|"no_text_found")&&ocr["failedFrames"]!=0)
            || (ocr["status"]=="partial"&&ocr["failedFrames"].as_u64().is_none_or(|n|n==0||n>=ocr["sampledFrames"].as_u64().unwrap_or(0)))
            || (ocr["status"]=="completed"&&materials.len()!=2)
            || (matches!(text(ocr,"status"),"no_text_found"|"not_applicable"|"unavailable"|"failed")&&materials.len()!=1){
            return Err(crate::conflict("Cached screen text outcome unproven"));
        }
        if let Some(screen)=materials.get(1){
            if screen["kind"]!="ocr"||screen["account"]!=p["account"]||screen["postKey"]!=p["sourcePostKey"]
                ||screen["mediaSha256"]!=p["source"]["sha256"]||screen["sourceUrl"]!=p["sourceProjection"]["sourceUrl"]
                ||screen["ocr"]!=*ocr||text(screen,"text").trim().is_empty(){
                return Err(crate::conflict("Cached screen text identity changed"));
            }
        }
    }
    Ok(())
}

// This lane admits the immutable analysis output through its typed current
// alias proof. Original provenance is never rewritten to satisfy a target pin.
fn validate_analysis_result(id:&str,expected:&Value,result:&Value)->ApiResult<()>{
    let p=&expected["progress"];
    let proof=&result["audioAnalysisApplicability"]["pin"];
    let analysis=&result["audioAnalysis"];
    let request=&analysis["targetRequest"];
    let alias=&request["originalAlias"];
    if let Some(pin)=p.get("assetPin"){
        if alias["assetPin"]!=*pin||proof["target"]["assetPin"]!=*pin{return Err(crate::conflict("Cached analysis selected video changed"));}
    }
    let materials=result["materials"].as_array().ok_or_else(||crate::conflict("Cached analysis material list missing"))?;
    let screen_text=expected["profile"]==TEXT_PROFILE;
    if result["audioAnalysisReuse"]!=true || !result["audioAnalysisApplicability"]["receipt"].is_object()
        || materials.is_empty() || materials.len()>if screen_text{2}else{1}
        || materials[0]!=proof["originalMaterial"] || materials[0]["kind"]!="transcript"
        || analysis["result"]!=proof["result"] || analysis["request"]!=proof["result"]["originalRequest"]
        || (analysis["request"].is_null()&&proof["result"]["sourceKind"]!="legacy_adopted")
        || request["companyId"]!=p["account"] || request["executionJobId"]!=id || request["stage"]!="asr"
        || request["specSha256"]!=proof["specSha256"] || request["verifiedFile"]!=proof["verifiedFile"]
        || request["sourceVersion"]!=p["sourceVersion"] || alias["account"]!=p["account"]
        || alias["connectorBinding"]!=p["connectorBinding"] || alias["sourcePostId"]!=p["sourcePostId"]
        || alias["sourcePostKey"]!=p["sourcePostKey"] || alias["sourceVersion"]!=p["sourceVersion"]
        || alias["sourceProjection"]!=p["sourceProjection"]
        || proof["result"]["coverage"]["durationMs"].as_u64().zip(p["sourceIdentity"]["durationMs"].as_u64())
            .is_none_or(|(actual,expected)|actual.abs_diff(expected)>1000)
        || request["durationMs"]!=proof["result"]["coverage"]["durationMs"]{
        return Err(crate::conflict("Cached analysis original output or target request changed"));
    }
    if !screen_text{
        if result.get("currentScreenText").is_some()||proof.get("currentScreenText").is_some(){
            return Err(crate::conflict("Cached audio-only analysis cannot admit screen text"));
        }
        return Ok(());
    }
    let current=&result["currentScreenText"];let ocr=&current["ocr"];
    if current["account"]!=p["account"] || current["postKey"]!=p["sourcePostKey"]
        || current["mediaSha256"]!=p["source"]["sha256"] || current["sourceUrl"]!=p["sourceProjection"]["sourceUrl"]
        || current["sourceVersion"]!=p["sourceVersion"] || current["connectorBinding"]!=p["connectorBinding"]
        || ocr["sourceVersion"]!=p["sourceVersion"] || ocr["exhaustive"]!=false
        || !matches!(text(ocr,"status"),"completed"|"no_text_found"|"not_applicable"|"unavailable"|"failed"|"partial")
        || (matches!(text(ocr,"status"),"completed"|"no_text_found"|"partial")
            && (ocr["coverage"]!="sampled_frames"||ocr["sampledFrames"].as_u64().is_none_or(|n|n==0||n>30)
                ||ocr["failedFrames"].as_u64().is_none()))
        || (matches!(text(ocr,"status"),"completed"|"no_text_found")&&ocr["failedFrames"]!=0)
        || (ocr["status"]=="partial"&&ocr["failedFrames"].as_u64().is_none_or(|n|n==0||n>=ocr["sampledFrames"].as_u64().unwrap_or(0)))
        || (ocr["status"]=="completed"&&materials.len()!=2)
        || (matches!(text(ocr,"status"),"no_text_found"|"not_applicable"|"unavailable"|"failed")&&materials.len()!=1)
        || (matches!(text(ocr,"status"),"completed"|"no_text_found")&&proof["currentScreenText"]!=*current)
        || (!matches!(text(ocr,"status"),"completed"|"no_text_found")&&proof.get("currentScreenText").is_some()){
        return Err(crate::conflict("Cached analysis target screen outcome unproven"));
    }
    if let Some(screen)=materials.get(1){
        if screen["kind"]!="ocr"||text(screen,"id").trim().is_empty()||screen["account"]!=p["account"]
            ||screen["postKey"]!=p["sourcePostKey"]||screen["mediaSha256"]!=p["source"]["sha256"]
            ||screen["sourceUrl"]!=p["sourceProjection"]["sourceUrl"]||screen["ocr"]!=*ocr
            ||text(screen,"text").trim().is_empty()||screen.get("transcription").is_some(){
            return Err(crate::conflict("Cached analysis target OCR material changed"));
        }
    }
    Ok(())
}
fn admit_analysis(d:&mut Value,id:&str,expected:&Value,result:&Value)->ApiResult<Value>{
    validate_analysis_result(id,expected,result)?;
    let at=crate::now();let applicability=&result["audioAnalysisApplicability"];
    let ledger=crate::media_analysis::ledger_from_workspace(d).map_err(|e|crate::conflict(&e))?;
    let admitted=crate::media_analysis_reuse::admit(d,&expected["progress"],&ledger,
        &applicability["receipt"],&applicability["pin"],&at).map_err(|e|crate::conflict(&e))?;
    // Only target pixels enter the catalog. The original speech arrives through
    // knowledge's typed analysis binding, without a duplicate donor import.
    let screen_materials=result["materials"].as_array().unwrap().iter().skip(1).cloned().collect::<Vec<_>>();
    if !screen_materials.is_empty(){crate::merge_materials(d,&json!({"materials":screen_materials}))?;}
    let post=crate::row(d,"posts",text(&expected["progress"],"sourcePostId"))?;
    if !required_ready(d,post,&expected["progress"],&at)?{return Err(crate::conflict("Complete cached analysis was not admitted to the current catalog"));}
    crate::audit(d,"media.cached_audio_admitted",id);
    let mut receipt=json!({"processed":true,"mode":"full_audio_only","visualContextStatus":"not_available",
        "sourcePostKey":expected["progress"]["sourcePostKey"],"source":expected["progress"]["source"],
        "policy":expected["policy"],"acquisitionProfile":expected["profile"],
        "transcription":result["materials"][0]["transcription"],"audioAnalysis":result["audioAnalysis"],"audioAnalysisReuse":true,
        "audioAnalysisApplicability":{"receipt":applicability["receipt"],"pin":admitted["result"]["proof"]}});
    if let Some(current)=result.get("currentScreenText"){receipt["currentScreenText"]=current.clone();}
    Ok(receipt)
}
fn admit(d:&mut Value,id:&str,expected:&Value,result:&Value)->ApiResult<Value>{
    let job=crate::row(d,"jobs",id)?;
    if job["kind"]!="media_audio" || job["status"]!="running" || job["audioPin"]!=*expected
        || current_pin(d,expected)?!=*expected {
        return Err(crate::conflict("Cached audio admission policy or ownership changed"));
    }
    if result.get("audioAnalysisApplicability").is_some(){return admit_analysis(d,id,expected,result);}
    if expected["progress"].get("assetPin").is_some(){return Err(crate::conflict("Per-video speech requires exact retained-file analysis applicability"));}
    if result["audioAnalysisReuse"]==true{return Err(crate::conflict("Cached analysis reuse requires current typed applicability"));}
    validate_result(result,expected)?;
    if let Some(reuse)=result.get("audioReuse"){
        // current_pin above admits only TEXT_PROFILE or the legacy null audio
        // profile. Reuse must be the same exact-file proof for either lane.
        if reuse["match"]!="exact_media_sha256"{
            return Err(crate::conflict("Cached audio reuse is not exact"));
        }
        crate::media_processing::full::transcript_reuse::validate_pin(d,&expected["progress"],reuse,&crate::now()).map_err(crate::conflict)?;
    }
    let post=crate::row(d,"posts",text(&expected["progress"],"sourcePostId"))?.clone();
    crate::merge_materials(d,result)?;
    // Persist successful speech even when optional screen extraction failed.
    // Strict readiness is a separate consumer of the exact OCR outcome.
    if !required_ready(d,&post,&expected["progress"],&crate::now())?{return Err(crate::conflict("Complete cached audio was not admitted to the current catalog"));}
    crate::audit(d,"media.cached_audio_admitted",id);
    let mut receipt=json!({"processed":true,"mode":"full_audio_only","visualContextStatus":"not_available",
        "sourcePostKey":expected["progress"]["sourcePostKey"],"source":expected["progress"]["source"],
        "policy":expected["policy"],"acquisitionProfile":expected["profile"],"transcription":result["materials"][0]["transcription"]});
    if let Some(reuse)=result.get("audioReuse"){receipt["audioReuse"]=reuse.clone();}
    Ok(receipt)
}

pub(super) async fn run(app:&App,id:&str,expected:&Value)->ApiResult<Value>{
    let d=app.read().await?;
    let actual=current_pin(&d,expected)?;
    if actual!=*expected || crate::row(&d,"jobs",id)?["audioPin"]!=*expected{return Err(crate::conflict("Cached audio source changed before processing"));}
    let reuse=crate::media_processing::full::cached_text_audio_reuse(&d,&expected["progress"],&crate::now()).map_err(|e|crate::bad(&e))?;
    drop(d);
    let result=if expected["profile"]==TEXT_PROFILE{crate::media_processing::full::cached_text(app,id,&expected["progress"],reuse).await}
        else{crate::media_processing::full::cached_audio(app,id,&expected["progress"],reuse).await}.map_err(|e|crate::bad(&e))?;
    app.change(|d|admit(d,id,expected,&result)).await
}

/// Claim only newly queued duration-policy work. Failed or uncertain audio
/// attempts remain held for inspection; this is not a retry mechanism.
fn automatic_candidate(d:&Value,at:&str,only_open:bool)->ApiResult<Option<Value>>{
    automatic_candidate_for(d,at,only_open,false)
}
fn automatic_candidate_for(d:&Value,at:&str,only_open:bool,screen_text:bool)->ApiResult<Option<Value>>{
    if rows(d,"jobs").iter().any(|j|matches!(text(j,"kind"),"media"|"media_audio")
        &&matches!(text(j,"status"),"running"|"unknown"|"dispatching")){return Ok(None);}
    let candidates:Vec<_>=rows(d,"jobs").iter().filter(|j|current_job(j)&&(j["status"]=="queued"
        ||j["status"]=="paused"&&j["mediaPolicyPause"]==true))
        .map(|j|(text(j,"id").to_owned(),j["result"]["visualProgress"]["leaseEpoch"].as_u64())).collect();
    for (origin,epoch) in candidates{
        let Some(epoch)=epoch else{continue};
        if (crate::row(d,"jobs",&origin)?["acquisitionProfile"]==TEXT_PROFILE)!=screen_text{continue;}
        let speech=crate::row(d,"jobs",&origin)?["acquisitionProfile"]==SPEECH_PROFILE;
        let Ok(expected)=(if screen_text{text_pin(d,&origin,epoch)}else if speech{speech_pin(d,&origin,epoch)}else{pin(d,&origin,epoch)}) else{continue};
        if !screen_text&&!speech&&expected["policy"]["decisionBasis"]["kind"]!="probed_duration_threshold"{continue;}
        let post=crate::row(d,"posts",text(&expected["progress"],"sourcePostId"))?;
        if (only_open&&!open_post(d,post)&&!super::manual_source_requested(d,crate::row(d,"jobs",&origin)?,post))||(!screen_text&&required_ready(d,post,&expected["progress"],at)?){continue;}
        if rows(d,"jobs").iter().any(|j|j["kind"]=="media_audio"&&j["audioPin"]["originJobId"]==origin
            &&j["audioPin"]["progress"]["source"]==expected["progress"]["source"]){continue;}
        return Ok(Some(expected));
    }
    Ok(None)
}
fn reserve_automatic(d:&mut Value,expected:&Value,at:&str,only_open:bool)->ApiResult<Option<(String,Value)>>{
    let screen_text=expected["profile"]==TEXT_PROFILE;
    if automatic_candidate_for(d,at,only_open,screen_text)?.as_ref()!=Some(expected){return Ok(None);}
    let origin=text(expected,"originJobId");let epoch=expected["originEpoch"].as_u64().ok_or_else(||crate::conflict("Cached audio epoch missing"))?;
    let reserved=if screen_text{let pinned=text_pin(d,origin,epoch)?;reserve_pinned(d,pinned,at)?}
        else if expected["profile"]==SPEECH_PROFILE{let pinned=speech_pin(d,origin,epoch)?;reserve_pinned(d,pinned,at)?}
        else{reserve(d,origin,epoch,at)?};
    crate::row_mut(d,"jobs",origin)?["status"]=json!("paused");Ok(Some(reserved))
}
async fn verify_source(expected:&Value)->ApiResult<()>{
    let progress=expected["progress"].clone();
    tokio::task::spawn_blocking(move||{
        crate::media_fullframes::store()?.path(&crate::media_fullframes::reference(&progress["source"])?)
            .map(|_|()).map_err(|_|"media_source_artifact_unavailable".to_owned())
    }).await.map_err(|_|crate::internal("Cached source verification stopped"))?.map_err(|e|crate::conflict(&e))
}
pub(super) async fn claim_ready(app:&App,only_open:bool)->ApiResult<Option<(String,Value)>>{
    claim_ready_checked(app,only_open,crate::media_processing::preflight_phase("audio")).await
}
pub(super) async fn claim_speech_ready(app:&App,only_open:bool)->ApiResult<Option<(String,Value)>>{
    let Some(expected)=app.change_media(|d|speech_candidate(d,&crate::now(),only_open)).await? else{return Ok(None)};
    if let Err(code)=crate::media_processing::preflight_phase("audio"){
        app.change_media(|d|{if speech_candidate(d,&crate::now(),only_open)?.as_ref()==Some(&expected){set_worker_block(crate::row_mut(d,"jobs",text(&expected,"originJobId"))?,"audio",Some(&code));}Ok(())}).await?;return Ok(None);
    }
    if verify_source(&expected).await.is_err(){
        app.change_media(|d|{if speech_candidate(d,&crate::now(),only_open)?.as_ref()==Some(&expected){let job=crate::row_mut(d,"jobs",text(&expected,"originJobId"))?;job["status"]=json!("paused");job["error"]=json!("media_cached_audio_source_unavailable");job.as_object_mut().unwrap().remove("mediaPolicyPause");}Ok(())}).await?;return Ok(None);
    }
    app.change(|d|{
        if speech_candidate(d,&crate::now(),only_open)?.as_ref()!=Some(&expected){return Ok(None);}
        let pinned=speech_pin(d,text(&expected,"originJobId"),expected["originEpoch"].as_u64().ok_or_else(||crate::bad("Speech source epoch missing"))?)?;
        let reserved=reserve_pinned(d,pinned,&crate::now())?;
        let origin=crate::row_mut(d,"jobs",text(&expected,"originJobId"))?;origin["status"]=json!("paused");set_worker_block(origin,"audio",None);Ok(Some(reserved))
    }).await
}
fn speech_candidate(d:&Value,at:&str,only_open:bool)->ApiResult<Option<Value>>{
    if rows(d,"jobs").iter().any(|j|matches!(text(j,"kind"),"media"|"media_audio")&&matches!(text(j,"status"),"running"|"unknown"|"dispatching")){return Ok(None);}
    for origin in rows(d,"jobs").iter().filter(|j|current_job(j)&&matches!(text(j,"acquisitionProfile"),SPEECH_PROFILE|TEXT_PROFILE)
        &&(j["status"]=="queued"||j["status"]=="paused"&&j["mediaPolicyPause"]==true)){
        let Some(epoch)=origin["result"]["visualProgress"]["leaseEpoch"].as_u64() else{continue};
        let Ok(expected)=speech_pin(d,text(origin,"id"),epoch) else{continue};
        let post=crate::row(d,"posts",text(&expected["progress"],"sourcePostId"))?;
        if only_open&&!open_post(d,post)&&!super::manual_source_requested(d,origin,post)||required_ready(d,post,&expected["progress"],at)?{continue;}
        if rows(d,"jobs").iter().any(|j|j["kind"]=="media_audio"&&j["audioPin"]["originJobId"]==origin["id"]&&j["audioPin"]["progress"]["source"]==expected["progress"]["source"]){continue;}
        return Ok(Some(expected));
    }Ok(None)
}
/// Queue owner calls this after the usual download/CAS checkpoint, under the
/// existing media execution gate. This never allocates a source download.
pub(super) async fn claim_text_ready(app:&App,only_open:bool)->ApiResult<Option<(String,Value)>>{
    claim_ready_for(app,only_open,crate::media_processing::preflight_phase("audio"),true).await
}
async fn claim_ready_checked(app:&App,only_open:bool,ready:Result<(),String>)->ApiResult<Option<(String,Value)>>{
    claim_ready_for(app,only_open,ready,false).await
}
async fn claim_ready_for(app:&App,only_open:bool,ready:Result<(),String>,screen_text:bool)->ApiResult<Option<(String,Value)>>{
    let Some(expected)=app.change_media(|d|automatic_candidate_for(d,&crate::now(),only_open,screen_text)).await? else{return Ok(None)};
    if let Err(code)=ready{
        app.change_media(|d|{
            if automatic_candidate_for(d,&crate::now(),only_open,screen_text)?.as_ref()==Some(&expected){
                set_worker_block(crate::row_mut(d,"jobs",text(&expected,"originJobId"))?,"audio",Some(&code));
            }Ok(())
        }).await?;
        return Ok(None);
    }
    if verify_source(&expected).await.is_err(){
        // Do not consume inference, and do not let one missing artifact starve
        // every other source. This exact checkpoint waits for explicit repair.
        app.change_media(|d|{
            let origin=text(&expected,"originJobId");
            if automatic_candidate_for(d,&crate::now(),only_open,screen_text)?.as_ref()==Some(&expected){
                let job=crate::row_mut(d,"jobs",origin)?;
                job["status"]=json!("paused");job["error"]=json!("media_cached_audio_source_unavailable");
                job.as_object_mut().unwrap().remove("mediaPolicyPause");
            }Ok(())
        }).await?;
        return Ok(None);
    }
    app.change(|d|{
        let reserved=reserve_automatic(d,&expected,&crate::now(),only_open)?;
        if reserved.is_some(){set_worker_block(crate::row_mut(d,"jobs",text(&expected,"originJobId"))?,"audio",None);}
        Ok(reserved)
    }).await
}
#[cfg(test)]
pub(super) fn claim_automatic(d:&mut Value,at:&str,only_open:bool)->ApiResult<Option<(String,Value)>>{
    let Some(expected)=automatic_candidate(d,at,only_open)? else{return Ok(None)};
    reserve_automatic(d,&expected,at,only_open)
}
#[cfg(test)]
pub(super) fn claim_text_automatic(d:&mut Value,at:&str,only_open:bool)->ApiResult<Option<(String,Value)>>{
    let Some(expected)=automatic_candidate_for(d,at,only_open,true)? else{return Ok(None)};
    reserve_automatic(d,&expected,at,only_open)
}

pub(crate) async fn request(app:&App,body:&Value)->ApiResult<Value>{
    let fields=body.as_object().ok_or_else(||crate::bad("Invalid cached audio request"))?;
    if fields.len()!=2 || fields.keys().any(|k|!matches!(k.as_str(),"jobId"|"leaseEpoch")){return Err(crate::bad("Unknown cached audio field"));}
    let origin=crate::required(body,"jobId")?;
    let epoch=body["leaseEpoch"].as_u64().ok_or_else(||crate::bad("Invalid cached audio epoch"))?;
    let guard=wait_for_retry_gate(&MEDIA_GATE,Duration::from_secs(300)).await?;
    // Artifact hashing is outside the writer; reserve repeats every identity check.
    let expected=pin(&app.read().await?,origin,epoch)?;
    verify_source(&expected).await?;
    crate::media_processing::preflight_phase("audio").map_err(|e|crate::bad(&e))?;
    let (id,expected)=app.change(|d|{
        if pin(d,origin,epoch)?!=expected{return Err(crate::conflict("Cached audio changed during artifact verification"));}
        reserve(d,origin,epoch,&crate::now())
    }).await?;
    let worker=app.clone();let run_id=id.clone();
    app.spawn_with_completion(id.clone(),async move {run(&worker,&run_id,&expected).await},move||{drop(guard);MEDIA_WAKE.notify_one();});
    Ok(json!({"jobId":id,"originJobId":origin,"status":"running","mode":"full_audio_only"}))
}

#[cfg(test)]
pub(crate) mod tests{
    use super::*;
    // Authored native seam fixture: CAS capture -> ledger -> verified selection
    // and warm -> the actual cached admission callback -> native knowledge.
    // No external tool, provider, API or paid ASR process is invoked.
    fn analysis_fixture(screen_text:bool,fresh:bool,status:&str)->(Value,String,Value,Value){
        analysis_fixture_mode(screen_text,fresh,status,false)
    }
    fn analysis_fixture_mode(screen_text:bool,fresh:bool,status:&str,no_audio:bool)->(Value,String,Value,Value){
        analysis_fixture_mode_for(crate::accounts::Profile::LikeAvto,screen_text,fresh,status,no_audio)
    }
    fn analysis_fixture_mode_for(profile:crate::accounts::Profile,screen_text:bool,fresh:bool,status:&str,no_audio:bool)->(Value,String,Value,Value){
        let at="2026-10-02T00:00:00Z";
        let mut d=fixture_for(profile);
        if screen_text{d["jobs"][0]["status"]=json!("queued");d["jobs"][0]["acquisitionProfile"]=json!(TEXT_PROFILE);
            d["jobs"][0]["result"]["visualProgress"]["phase"]=json!("inventory");}
        d["posts"][0]["sourceUrl"]=json!("https://example.test/target");
        d["posts"][0]["attachments"][0]["url"]=json!("https://example.test/target.mp4");
        let version=crate::media_fullframes::source_version(&d["posts"][0],text(&d,"account"));
        let store=crate::media_fullframes::store().unwrap();
        let source=store.put_bytes(b"cached admission retained source fixture").unwrap().to_json();
        d["settings"]["postMediaPolicies"]["p"]["sourceVersion"]=json!(version);
        let p=&mut d["jobs"][0]["result"]["visualProgress"];
        p["sourceVersion"]=json!(version);p["source"]=source.clone();
        p["sourceIdentity"]["mediaSha256"]=source["sha256"].clone();p["sourceIdentity"]["durationMs"]=json!(1000);
        p["sourceProjection"]["sourceUrl"]=json!("https://example.test/target");
        let mut donor=d["posts"][0].clone();
        if !fresh{
            donor["id"]=json!("donor");donor["postKey"]=json!("provider:donor");donor["title"]=json!("Original upload title");
            donor["sourceUrl"]=json!("https://example.test/donor");donor["attachments"][0]["url"]=json!("https://example.test/donor.mp4");
            crate::list_mut(&mut d,"posts").push(donor.clone());
        }
        let donor_version=crate::media_fullframes::source_version(&donor,text(&d,"account"));
        let mut original=json!({"id":"analysis-original","kind":"transcript","account":d["account"],"title":donor["title"],
            "postKey":donor["postKey"],"sourceUrl":donor["sourceUrl"],"mediaSha256":source["sha256"],"text":"Original complete spoken words",
            "transcription":{"sourceVersion":donor_version,"sourcePostKey":donor["postKey"],"partial":false,"coverage":"full_audio",
                "audioStatus":"transcribed","mediaDurationSeconds":1.0,"audioDurationSeconds":1.0,
                "ocr":{"sourceVersion":donor_version,"coverage":"sampled_frames","status":"completed","sampledFrames":3,"failedFrames":0,"exhaustive":false}}});
        if fresh{original["transcription"].as_object_mut().unwrap().remove("ocr");}
        if no_audio{original["text"]=json!("[Audio inspection: no_audio_stream. No spoken words were recovered.]");
            original["transcription"]["coverage"]=json!("no_audio_stream");original["transcription"]["audioStatus"]=json!("no_audio_stream");
            original["transcription"]["audioDurationSeconds"]=Value::Null;}
        if !fresh{d["materials"]=json!([original]);}
        crate::knowledge::sync_catalog(&mut d,at).unwrap();
        let epoch=material_epoch(&d,&d["posts"][0]);d["jobs"][0]["result"]["visualProgress"]["materialEpoch"]=json!(epoch);
        let expected=if screen_text{text_pin(&d,"origin",7).unwrap()}else{pin(&d,"origin",7).unwrap()};
        let (id,expected)=reserve_pinned(&mut d,expected,at).unwrap();
        let file=json!({"sha256":source["sha256"],"bytes":source["bytes"],"receiptSha256":"b".repeat(64),"probeSha256":"c".repeat(64)});
        let attempt=crate::id();let spec="d".repeat(64);
        let mut original_request=json!({"companyId":d["account"],"executionJobId":if fresh{id.as_str()}else{"paid-origin"},"stage":"asr",
            "verifiedFile":file,"specSha256":spec,"attemptId":attempt,"owner":"fixture-worker","epoch":7,"manifestKey":attempt,
            "durationMs":1000,"segments":[{"index":0,"startMs":0,"endMs":1000}],"sourceVersion":donor_version,
            "originalAlias":{"account":d["account"],"connectorBinding":expected["progress"]["connectorBinding"],
                "sourcePostId":donor["id"],"sourcePostKey":donor["postKey"],"sourceVersion":donor_version,
                "sourceProjection":if fresh{expected["progress"]["sourceProjection"].clone()}else{json!({"account":d["account"],"postKey":donor["postKey"],"title":donor["title"],"sourceUrl":donor["sourceUrl"]})}}});
        if no_audio{original_request["noAudio"]=json!(true);original_request["noAudioVerificationSha256"]=json!("e".repeat(64));original_request["segments"]=json!([]);}
        let mut ledger=Value::Null;crate::media_analysis::reserve(&mut ledger,&original_request).unwrap();
        let mut segments=Vec::new();
        if !no_audio{
            let mut dispatched=original_request.clone();dispatched["segmentIndex"]=json!(0);
            crate::media_analysis::mark_dispatched(&mut ledger,&dispatched).unwrap();
            let segment=crate::media_processing::analysis_output::capture_segment(&store,&original_request,0,"existing words","existing words",1000).unwrap();
            let mut segment_request=original_request.clone();segment_request["segment"]=segment.clone();
            crate::media_analysis::commit_segment(&mut ledger,&segment_request).unwrap();segments.push(segment);
        }
        let audio=json!({"materials":[original],"reused":false,"coverage":{"kind":if no_audio{"no_audio_stream"}else{"full_audio"},"durationMs":1000},
            "outcome":if no_audio{"no_audio"}else{"transcript"}});
        let output=crate::media_processing::analysis_output::capture_full(&store,&original_request,&segments,&audio).unwrap();
        let mut completed=original_request.clone();completed["result"]=output;
        crate::media_analysis::commit_full_result(&mut ledger,&completed).unwrap();
        crate::media_analysis::put_ledger(&mut d,&ledger).unwrap();
        let mut target_request=original_request.clone();target_request["executionJobId"]=json!(id);
        target_request["sourceVersion"]=expected["progress"]["sourceVersion"].clone();
        target_request["originalAlias"]=json!({"account":d["account"],"connectorBinding":expected["progress"]["connectorBinding"],
            "sourcePostId":expected["progress"]["sourcePostId"],"sourcePostKey":expected["progress"]["sourcePostKey"],
            "sourceVersion":expected["progress"]["sourceVersion"],"sourceProjection":expected["progress"]["sourceProjection"]});
        let receipt=crate::media_analysis_reuse::receipt_for_request(&d,&expected["progress"],&target_request).unwrap();
        let (material,mut proof)=crate::media_analysis_reuse::select_verified(&d,&expected["progress"],&ledger,&receipt,&spec).unwrap().unwrap();
        let mut candidate=json!({"materials":[material],"audioAnalysisReuse":true,"audioAnalysis":{"request":original_request,
            "targetRequest":target_request,"result":proof["result"]}});
        if screen_text{
            let mut ocr=json!({"status":status,"sourceVersion":expected["progress"]["sourceVersion"],"exhaustive":false,
                "coverage":"sampled_frames","sampledFrames":2,"failedFrames":0});
            if status=="unavailable"{ocr=json!({"status":"unavailable","sourceVersion":expected["progress"]["sourceVersion"],
                "exhaustive":false,"coverage":"unavailable","reason":"ocr_configuration_invalid"});}
            let current=json!({"account":d["account"],"postKey":expected["progress"]["sourcePostKey"],"sourceUrl":expected["progress"]["sourceProjection"]["sourceUrl"],
                "mediaSha256":source["sha256"],"sourceVersion":expected["progress"]["sourceVersion"],"connectorBinding":expected["progress"]["connectorBinding"],"ocr":ocr});
            if matches!(status,"completed"|"no_text_found"){crate::media_analysis_reuse::attach_current_screen_text(&mut proof,&current).unwrap();}
            candidate["currentScreenText"]=current;
            if status=="completed"{candidate["materials"].as_array_mut().unwrap().push(json!({"id":"cached-target-ocr","kind":"ocr","title":"Current target end card",
                "text":"Цена целевого ролика 2 300 000","account":d["account"],"postKey":expected["progress"]["sourcePostKey"],
                "sourceUrl":expected["progress"]["sourceProjection"]["sourceUrl"],"mediaSha256":source["sha256"],"ocr":ocr}));}
        }
        crate::media_analysis_reuse::warm_pin(&d,&proof).unwrap();
        candidate["audioAnalysisApplicability"]=json!({"receipt":receipt,"pin":proof});
        (d,id,expected,candidate)
    }
    #[test]
    fn typed_cached_admission_preserves_original_for_fresh_and_current_target_outcomes(){
        for fresh in [false,true]{for status in ["completed","no_text_found","unavailable"]{
            let (mut d,id,expected,candidate)=analysis_fixture(true,fresh,status);
            let original=candidate["materials"][0].clone();let catalog=d["materials"].clone();let versions=d["knowledge_versions"].clone();
            let origin=d["jobs"][0].clone();let ledger=crate::media_analysis::ledger_from_workspace(&d).unwrap();
            let receipt=admit(&mut d,&id,&expected,&candidate).unwrap();
            assert_eq!(receipt["transcription"],original["transcription"]);assert_eq!(receipt["currentScreenText"],candidate["currentScreenText"]);
            assert_eq!(receipt["audioAnalysis"]["request"],candidate["audioAnalysis"]["request"]);
            assert!(receipt["audioAnalysisApplicability"]["pin"].get("normalizedPayload").is_none());
            assert_eq!(d["jobs"][0],origin);assert_eq!(crate::media_analysis::ledger_from_workspace(&d).unwrap(),ledger);
            for material in catalog.as_array().unwrap(){assert_eq!(rows(&d,"materials").iter().find(|m|m["id"]==material["id"]),Some(material));}
            for version in versions.as_array().unwrap(){assert_eq!(rows(&d,"knowledge_versions").iter().find(|v|v["id"]==version["id"]),Some(version));}
            assert_eq!(rows(&d,"materials").iter().filter(|m|m["kind"]=="transcript").count(),if fresh{0}else{1});
            let strict=crate::knowledge::TranscriptLookup::new(&d,&crate::now()).unwrap().strict_media_evidence(&d["posts"][0]).unwrap();
            assert_eq!(strict["audioReady"],true);assert_eq!(strict["visualReady"],false);
            assert_eq!(strict["screenTextReady"],status!="unavailable");assert_eq!(strict["screenTextHasContent"],status=="completed");
            assert!(has_required_media(&d,&d["posts"][0],&crate::now()).unwrap());
        }}
        let (mut d,id,expected,candidate)=analysis_fixture(false,false,"unused");
        assert!(admit(&mut d,&id,&expected,&candidate).is_ok());
    }
    #[test]
    fn typed_cached_admission_rejects_stale_foreign_and_overlay_proofs_without_mutation(){
        for case in ["stale","foreign","overlay","original","donor_ocr","ocr_account","ocr_version","target_request","original_request","missing_proof","count","closure"]{
            let (mut d,id,mut expected,mut candidate)=analysis_fixture(true,false,"completed");
            match case{
                "stale"=>d["posts"][0]["title"]=json!("New target metadata"),
                "foreign"=>candidate["audioAnalysisApplicability"]["pin"]["account"]=json!("BAW Russia"),
                "overlay"=>{let overlay=crate::media_fullframes::store().unwrap().put_bytes(b"different retained target container and overlay").unwrap().to_json();
                    d["jobs"][0]["result"]["visualProgress"]["source"]=overlay.clone();
                    d["jobs"][0]["result"]["visualProgress"]["sourceIdentity"]["mediaSha256"]=overlay["sha256"].clone();
                    expected["progress"]=d["jobs"][0]["result"]["visualProgress"].clone();
                    crate::row_mut(&mut d,"jobs",&id).unwrap()["audioPin"]=expected.clone();
                    assert_eq!(current_pin(&d,&expected).unwrap(),expected,"the current target checkpoint itself is valid");},
                "original"=>candidate["materials"][0]["postKey"]=expected["progress"]["sourcePostKey"].clone(),
                "donor_ocr"=>candidate["currentScreenText"]["ocr"]=candidate["materials"][0]["transcription"]["ocr"].clone(),
                "ocr_account"=>candidate["materials"][1]["account"]=json!("BAW Russia"),
                "ocr_version"=>candidate["materials"][1]["ocr"]["sourceVersion"]=json!("old-source"),
                "target_request"=>candidate["audioAnalysis"]["targetRequest"]["executionJobId"]=json!("other-worker"),
                "original_request"=>candidate["audioAnalysis"]["request"]["originalAlias"]["sourcePostKey"]=expected["progress"]["sourcePostKey"].clone(),
                "missing_proof"=>{candidate.as_object_mut().unwrap().remove("audioAnalysisApplicability");},
                "count"=>candidate["materials"].as_array_mut().unwrap().push(json!({"kind":"ocr","text":"donor overlay"})),
                _=>candidate["audioAnalysisApplicability"]["pin"]["normalizedPayload"]["audio"]["materials"][0]["text"]=json!("Changed normalized original"),
            }
            let before=d.clone();assert!(admit(&mut d,&id,&expected,&candidate).is_err(),"{case}");assert_eq!(d,before,"{case}");
        }
    }
    #[tokio::test]
    async fn fresh_bare_capture_prepares_then_actual_cached_callback_admits_speech_and_no_audio(){
        for no_audio in [false,true]{
            let (app,_temp)=crate::tests::test_app().await;
            let (mut d,id,expected,prepared)=analysis_fixture_mode(true,true,"no_text_found",no_audio);
            // Pure domain fixtures omit feedback; the actual storage writer
            // requires its collection even when this scenario has no feedback.
            d["feedback"]=json!([]);
            let original=prepared["materials"][0].clone();
            let captured=&prepared["audioAnalysis"]["result"];
            // This is the fresh worker shape: its capture has not been enriched
            // with ledger fields or resultSha256, and OCR is only a target sidecar.
            let bare=json!({"manifest":captured["manifest"],"normalizedOutput":captured["normalizedOutput"],
                "coverage":captured["coverage"],"outcome":captured["outcome"],"verificationSha256":captured["verificationSha256"]});
            assert!(bare.get("resultSha256").is_none());
            let mut worker=json!({"materials":[original],"audioAnalysis":{"request":prepared["audioAnalysis"]["request"],"result":bare}});
            worker["materials"][0]["transcription"]["ocr"]=prepared["currentScreenText"]["ocr"].clone();
            let native_lifecycle=app.read().await.unwrap()["runtimeLifecycle"].clone();
            // Seed synthetic media under the actual App writer, retaining the
            // native owner that its pre/post callback guards must verify.
            app.change(|state|{let lifecycle=state["runtimeLifecycle"].clone();*state=d.clone();state["runtimeLifecycle"]=lifecycle;Ok(())}).await.unwrap();
            assert_eq!(app.read().await.unwrap()["runtimeLifecycle"],native_lifecycle);
            let candidate=crate::media_processing::full::prepare_audio_analysis(&app,&expected["progress"],worker,true).await.unwrap();
            assert_eq!(candidate["audioAnalysis"]["result"],*captured);
            assert_eq!(candidate["materials"][0],original,"preparation restores immutable output before admission");
            let receipt=app.change(|state|admit(state,&id,&expected,&candidate)).await.unwrap();
            assert_eq!(receipt["transcription"],original["transcription"]);
            let admitted=app.read().await.unwrap();
            assert_eq!(admitted["feedback"],d["feedback"],"native admission must retain the complete fixture's feedback history");
            let strict=crate::knowledge::TranscriptLookup::new(&admitted,&crate::now()).unwrap().strict_media_evidence(&admitted["posts"][0]).unwrap();
            assert_eq!(strict["audioReady"],true);assert_eq!(strict["audioHasContent"],!no_audio);
            assert_eq!(strict["screenTextReady"],true);assert_eq!(strict["screenTextHasContent"],false);
            assert_eq!(crate::media_analysis::ledger_from_workspace(&admitted).unwrap(),crate::media_analysis::ledger_from_workspace(&d).unwrap());
            assert!(rows(&admitted,"materials").is_empty(),"no duplicate transcript or fabricated empty OCR enters the catalog");
        }
    }
    fn exact_reuse_fixture(screen_text:bool)->(Value,String,Value,Value){
        let at="2026-10-02T00:00:00Z";
        let mut d=if screen_text{text_fixture()}else{fixture()};
        d["posts"][0]["sourceUrl"]=json!("https://example.test/video");
        let version=crate::media_fullframes::source_version(&d["posts"][0],text(&d,"account"));
        d["settings"]["postMediaPolicies"]["p"]["sourceVersion"]=json!(version);
        d["jobs"][0]["result"]["visualProgress"]["sourceVersion"]=json!(version);
        let initial=if screen_text{text_pin(&d,"origin",7).unwrap()}else{pin(&d,"origin",7).unwrap()};
        let candidate=if screen_text{text_result(&initial,"no_text_found")}else{result(&initial)};
        d["materials"]=candidate["materials"].clone();crate::knowledge::sync_catalog(&mut d,at).unwrap();
        let epoch=material_epoch(&d,&d["posts"][0]);d["jobs"][0]["result"]["visualProgress"]["materialEpoch"]=json!(epoch);
        let expected=if screen_text{text_pin(&d,"origin",7).unwrap()}else{pin(&d,"origin",7).unwrap()};
        let (id,expected)=reserve_pinned(&mut d,expected,at).unwrap();
        let proof=crate::media_processing::full::transcript_reuse::select(&d,&expected["progress"],at).unwrap().unwrap();
        assert_eq!(proof["match"],"exact_media_sha256");
        let selected=crate::knowledge::select(&d,&[],&[d["posts"][0].clone()],at).unwrap();
        let material=rows(&selected,"materials").iter().find(|m|m["id"]==proof["sourceMaterialId"]).unwrap().clone();
        let candidate=json!({"materials":[material],"reused":false,"audioReuse":proof});(d,id,expected,candidate)
    }
    #[test]
    fn both_cached_lanes_admit_exact_reuse_and_retain_durable_proof(){
        for screen_text in [false,true]{
            let (mut d,id,expected,candidate)=exact_reuse_fixture(screen_text);
            let origin=d["jobs"][0].clone();let versions=d["knowledge_versions"].clone();
            let unknown=json!({"id":"unrelated-unknown","status":"unknown","providerRetryAllowed":false});
            crate::list_mut(&mut d,"operations").push(unknown.clone());
            let receipt=admit(&mut d,&id,&expected,&candidate).unwrap();
            assert_eq!(receipt["audioReuse"],candidate["audioReuse"]);
            for version in versions.as_array().unwrap(){
                assert_eq!(rows(&d,"knowledge_versions").iter().find(|v|v["id"]==version["id"]),Some(version),"reuse cannot rewrite admitted speech history");
            }
            assert_eq!(d["jobs"][0],origin);assert_eq!(d["operations"][0],unknown);
        }
    }
    #[test]
    fn cached_reuse_rejects_changed_head_foreign_pin_title_and_source_version(){
        for screen_text in [false,true]{for case in ["head","foreign","title","version","sha"]{
            let (mut d,id,expected,mut candidate)=exact_reuse_fixture(screen_text);
            match case{
                "head"=>{d["materials"][0]["text"]=json!("Changed speech");crate::knowledge::sync_catalog(&mut d,&crate::now()).unwrap();},
                "foreign"=>candidate["audioReuse"]["account"]=json!("another-company"),
                "title"=>candidate["audioReuse"]["match"]=json!("account_scoped_exact_title_policy"),
                "version"=>d["posts"][0]["title"]=json!("Changed source version"),
                _=>candidate["materials"][0]["mediaSha256"]=json!("b".repeat(64)),
            }
            let before=d.clone();assert!(admit(&mut d,&id,&expected,&candidate).is_err(),"{case}");assert_eq!(d,before);
        }}
    }

    fn text_fixture()->Value{
        let mut d=fixture();d["jobs"][0]["status"]=json!("queued");
        d["jobs"][0]["result"]["visualProgress"]["phase"]=json!("inventory");
        d["jobs"][0]["acquisitionProfile"]=json!(TEXT_PROFILE);d
    }
    #[test]
    fn new_baw_speech_admission_reuses_retained_legacy_text_source_without_rewriting_origin(){
        let mut d=fixture_for(crate::accounts::Profile::BawRussia);d["jobs"][0]["status"]=json!("queued");
        d["jobs"][0]["acquisitionProfile"]=json!(TEXT_PROFILE);d["jobs"][0]["result"]["visualProgress"]["phase"]=json!("inventory");
        let source=d["jobs"][0]["result"]["visualProgress"].clone();
        let expected=speech_candidate(&d,"2026-10-02T00:00:00Z",false).unwrap().unwrap();
        assert_eq!(expected["profile"],SPEECH_PROFILE);assert_eq!(expected["originAcquisitionProfile"],TEXT_PROFILE);
        let(id,pin)=reserve_pinned(&mut d,expected.clone(),"2026-10-02T00:00:00Z").unwrap();
        assert_eq!(d["jobs"][0]["acquisitionProfile"],TEXT_PROFILE);assert_eq!(d["jobs"][0]["result"]["visualProgress"],source);assert_eq!(pin,expected);
        assert_eq!(crate::row(&d,"jobs",&id).unwrap()["purpose"],"required_video_speech");
        assert!(speech_candidate(&d,"2026-10-02T00:00:01Z",false).unwrap().is_none());
    }
    fn per_video_admission_fixture(profile:crate::accounts::Profile,speech_profile:bool)->(Value,String,Value,Value,Value){
        let (mut d,id,mut expected,mut prepared)=analysis_fixture_mode_for(profile,false,false,"unavailable",false);
        d["posts"][0]["attachments"].as_array_mut().unwrap().push(json!({"type":"video","url":"https://example.test/second.mp4"}));
        let version=crate::media_fullframes::source_version(&d["posts"][0],text(&d,"account"));
        d["settings"]["postMediaPolicies"]["p"]["sourceVersion"]=json!(version);
        let asset=crate::media_speech_assets::capture(&d,&d["posts"][0],0).unwrap();
        let mut progress=expected["progress"].clone();progress["sourceVersion"]=json!(version);progress["assetPin"]=asset.clone();progress["sourceProjection"]["assetPin"]=asset.clone();
        progress["materialEpoch"]=json!(material_epoch(&d,&d["posts"][0]));
        d["jobs"][0]["videoSpeechAssetPin"]=asset.clone();d["jobs"][0]["result"]["visualProgress"]=progress.clone();
        expected["progress"]=progress.clone();expected["policy"]=crate::post_media_policy::effective(&d,&d["posts"][0]).unwrap();
        if speech_profile{
            d["jobs"][0]["status"]=json!("queued");d["jobs"][0]["acquisitionProfile"]=json!(SPEECH_PROFILE);
            d["jobs"][0]["sourceAttempts"]=json!([]);d["jobs"][0]["result"]["visualProgress"]["phase"]=json!("inventory");
            d["jobs"][0]["groupKey"]=json!(super::super::group(&d,&d["posts"][0]).unwrap());
            expected=speech_pin(&d,"origin",7).unwrap();progress=expected["progress"].clone();
            crate::row_mut(&mut d,"jobs",&id).unwrap()["purpose"]=json!("required_video_speech");
        }
        crate::row_mut(&mut d,"jobs",&id).unwrap()["audioPin"]=expected.clone();
        let target=&mut prepared["audioAnalysis"]["targetRequest"];
        target["sourceVersion"]=progress["sourceVersion"].clone();target["originalAlias"]["sourceVersion"]=progress["sourceVersion"].clone();
        target["originalAlias"]["sourceProjection"]=progress["sourceProjection"].clone();target["originalAlias"]["assetPin"]=asset.clone();
        let ledger=crate::media_analysis::ledger_from_workspace(&d).unwrap();
        let receipt=crate::media_analysis_reuse::receipt_for_request(&d,&progress,&prepared["audioAnalysis"]["targetRequest"]).unwrap();
        let (_,proof)=crate::media_analysis_reuse::select_verified(&d,&progress,&ledger,&receipt,&"d".repeat(64)).unwrap().unwrap();
        crate::media_analysis_reuse::warm_pin(&d,&proof).unwrap();prepared["audioAnalysisApplicability"]=json!({"receipt":receipt,"pin":proof});
        (d,id,expected,prepared,asset)
    }
    // BAW is selected before source/ledger capture. This shares the actual
    // admission seam below; neither immutable ASR output nor its donor alias is
    // retargeted. Storage tests receive one committed alias and an open sibling.
    pub(crate) fn native_first_alias_fixture()->Value{
        let (mut d,id,expected,prepared,asset)=per_video_admission_fixture(crate::accounts::Profile::BawRussia,true);
        let result=admit(&mut d,&id,&expected,&prepared).unwrap();
        let admitted_at=crate::now();
        let job=crate::row_mut(&mut d,"jobs",&id).unwrap();job["status"]=json!("completed");
        job["finishedAt"]=json!(admitted_at);job["result"]=result;
        let origin=crate::row_mut(&mut d,"jobs","origin").unwrap();origin["status"]=json!("paused");origin["mediaPolicyPause"]=json!(true);
        let binding=crate::active_binding(&d).unwrap().to_json();
        d["items"]=json!([{"id":"item-first-alias","itemId":"first-alias","objectId":"11391","postId":"p","postKey":"provider:p",
            "conversationKey":"11391:first-alias","connectorBinding":binding,"providerStatus":"new","workflow":"attention",
            "createdAt":"2026-10-02T00:00:00Z"}]);d["feedback"]=json!([]);
        assert!(crate::media_speech_assets::ready(&d,&asset,&admitted_at).unwrap());
        assert!(!crate::media_speech_assets::all_ready(&d,&d["posts"][0],&admitted_at).unwrap());
        d
    }
    #[test]
    fn per_video_cached_admission_commits_first_alias_while_other_video_is_pending(){
        let (mut d,id,expected,prepared,asset)=per_video_admission_fixture(crate::accounts::Profile::LikeAvto,false);
        let result=admit(&mut d,&id,&expected,&prepared).unwrap();assert_eq!(result["processed"],true);
        // Native admission uses the real current time. Its newly completed
        // applicability cannot be selected by a historical as-of query.
        assert!(!crate::media_speech_assets::ready(&d,&asset,"2026-10-02T00:00:00Z").unwrap());
        let admitted_at=crate::now();
        assert!(crate::media_speech_assets::ready(&d,&asset,&admitted_at).unwrap());
        assert!(!crate::media_speech_assets::all_ready(&d,&d["posts"][0],&admitted_at).unwrap());
        let mut foreign=prepared.clone();foreign["audioAnalysis"]["targetRequest"]["originalAlias"]["assetPin"]["attachmentIndex"]=json!(1);
        let before=d.clone();assert!(admit(&mut d,&id,&expected,&foreign).is_err());assert_eq!(d,before);
    }
    #[test]
    fn new_baw_speech_profile_reserves_audio_only_and_keeps_original_source_checkpoint(){
        let mut d=fixture_for(crate::accounts::Profile::BawRussia);
        d["jobs"][0]["status"]=json!("queued");d["jobs"][0]["acquisitionProfile"]=json!(SPEECH_PROFILE);
        d["jobs"][0]["result"]["visualProgress"]["phase"]=json!("inventory");
        let original=d["jobs"][0]["result"]["visualProgress"].clone();
        let expected=automatic_candidate_for(&d,"2026-10-02T00:00:00Z",false,false).unwrap().unwrap();
        assert_eq!(expected["profile"],SPEECH_PROFILE);assert!(automatic_candidate_for(&d,"2026-10-02T00:00:00Z",false,true).unwrap().is_none());
        let(id,pin)=reserve_automatic(&mut d,&expected,"2026-10-02T00:00:00Z",false).unwrap().unwrap();
        assert_eq!(crate::row(&d,"jobs",&id).unwrap()["purpose"],"required_video_speech");assert_eq!(pin["profile"],SPEECH_PROFILE);assert_eq!(d["jobs"][0]["result"]["visualProgress"],original);
        crate::row_mut(&mut d,"jobs",&id).unwrap()["status"]=json!("unknown");assert!(automatic_candidate_for(&d,"2026-10-02T00:00:01Z",false,false).unwrap().is_none());
    }
    fn text_result(expected:&Value,status:&str)->Value{
        let mut out=result(expected);
        out["materials"][0]["transcription"]["ocr"]=json!({"status":status,"sourceVersion":expected["progress"]["sourceVersion"],
            "exhaustive":false,"coverage":"sampled_frames","sampledFrames":2,"failedFrames":0,"maxFrames":30,"intervalSeconds":35.0});
        if status=="completed"{
            let mut screen=out["materials"][0].clone();screen["kind"]=json!("ocr");screen["id"]=json!("screen-fixture");
            screen["text"]=json!("Цена 2 300 000");screen["ocr"]=screen["transcription"]["ocr"].clone();
            screen.as_object_mut().unwrap().remove("transcription");out["materials"].as_array_mut().unwrap().push(screen);
        }
        out
    }
    #[test]
    fn text_profile_preserves_source_and_does_not_enter_old_automatic_audio_lane(){
        let mut d=text_fixture();let before=d["jobs"][0]["result"]["visualProgress"].clone();
        assert!(automatic_candidate(&d,"2026-10-02T00:00:00Z",false).unwrap().is_none());
        let expected=automatic_candidate_for(&d,"2026-10-02T00:00:00Z",false,true).unwrap().unwrap();
        let (id,pin)=reserve_automatic(&mut d,&expected,"2026-10-02T00:00:00Z",false).unwrap().unwrap();
        assert_eq!(pin["profile"],TEXT_PROFILE);assert_eq!(crate::row(&d,"jobs",&id).unwrap()["purpose"],"cached_screen_text");
        assert_eq!(d["jobs"][0]["result"]["visualProgress"],before);
        crate::row_mut(&mut d,"jobs",&id).unwrap()["status"]=json!("failed");
        assert!(automatic_candidate_for(&d,"2026-10-02T00:00:01Z",false,true).unwrap().is_none());
    }
    #[test]
    fn screen_text_rejects_foreign_ocr_and_false_no_text_proof(){
        let d=text_fixture();let expected=text_pin(&d,"origin",7).unwrap();
        let valid=text_result(&expected,"completed");assert!(validate_result(&valid,&expected).is_ok());
        for key in ["account","postKey","mediaSha256","sourceUrl"]{
            let mut bad=valid.clone();bad["materials"][1][key]=json!("foreign");assert!(validate_result(&bad,&expected).is_err());
        }
        let mut absent=text_result(&expected,"no_text_found");assert!(validate_result(&absent,&expected).is_ok());
        absent["materials"][0]["transcription"]["ocr"]["failedFrames"]=json!(1);assert!(validate_result(&absent,&expected).is_err());
        let mut wrong=valid;wrong["materials"][0]["transcription"]["ocr"]["exhaustive"]=json!(true);assert!(validate_result(&wrong,&expected).is_err());
    }
    #[test]
    fn cached_admission_rejects_audio_shorter_than_reported_media_within_checkpoint_tolerance(){
        for screen_text in [false,true]{
            let mut d=if screen_text{text_fixture()}else{fixture()};
            d["jobs"][0]["result"]["visualProgress"]["sourceIdentity"]["durationMs"]=json!(100000);
            let expected=if screen_text{text_pin(&d,"origin",7).unwrap()}else{pin(&d,"origin",7).unwrap()};
            let (id,expected)=reserve_pinned(&mut d,expected,"2026-10-02T00:00:00Z").unwrap();
            let mut short=if screen_text{text_result(&expected,"no_text_found")}else{result(&expected)};
            short["materials"][0]["transcription"]["mediaDurationSeconds"]=json!(101.0);
            short["materials"][0]["transcription"]["audioDurationSeconds"]=json!(100.0);
            let source_version=text(&expected["progress"],"sourceVersion");
            assert!(!crate::knowledge::proven_full_audio(&short["materials"][0]["transcription"],source_version));
            assert!(validate_result(&short,&expected).is_err());
            let before=d.clone();assert!(admit(&mut d,&id,&expected,&short).is_err());assert_eq!(d,before,"rejection cannot admit unusable audio or report processing success");
            short["materials"][0]["transcription"]["mediaDurationSeconds"]=json!(100.25);
            assert!(crate::knowledge::proven_full_audio(&short["materials"][0]["transcription"],source_version));
            assert!(validate_result(&short,&expected).is_ok(),"retain the catalog's actual coverage tolerance");
        }
    }
    #[test]
    fn screen_unavailability_keeps_proven_audio_but_old_audio_contract_rejects_extra_ocr(){
        let mut d=text_fixture();let expected=text_pin(&d,"origin",7).unwrap();
        let (id,pin)=reserve_pinned(&mut d,expected,"2026-10-02T00:00:00Z").unwrap();
        let mut unavailable=text_result(&pin,"unavailable");
        unavailable["materials"][0]["transcription"]["ocr"]=json!({"status":"unavailable","reason":"tesseract_not_configured",
            "sourceVersion":pin["progress"]["sourceVersion"],"coverage":"unavailable","exhaustive":false});
        assert!(admit(&mut d,&id,&pin,&unavailable).is_ok());
        assert!(rows(&d,"materials").iter().any(|m|m["kind"]=="transcript"));
        let original=fixture();let old=super::pin(&original,"origin",7).unwrap();
        assert!(validate_result(&text_result(&pin,"completed"),&old).is_err());
    }
    #[test]
    fn text_pin_never_weakens_explicit_full_visual_or_accepts_profile_change(){
        let mut d=text_fixture();let expected=text_pin(&d,"origin",7).unwrap();
        for phase in ["select","scan","finalize","download"]{
            let mut wrong=d.clone();wrong["jobs"][0]["result"]["visualProgress"]["phase"]=json!(phase);
            assert!(text_pin(&wrong,"origin",7).is_err());
        }
        let mut held=d.clone();held["jobs"][0]["result"]["visualProgress"]["phase"]=json!("held");
        held["jobs"][0]["result"]["visualProgress"]["resumePhase"]=json!("inventory");assert!(text_pin(&held,"origin",7).is_ok());
        held["jobs"][0]["result"]["visualProgress"]["resumePhase"]=json!("scan");assert!(text_pin(&held,"origin",7).is_err());
        d["jobs"][0]["acquisitionProfile"]=json!("other");assert!(current_pin(&d,&expected).is_err());
        d["jobs"][0]["acquisitionProfile"]=json!(TEXT_PROFILE);
        d["settings"]["postMediaPolicies"]["p"]["mode"]=json!("full_audio_visual");assert!(text_pin(&d,"origin",7).is_err());
    }
    fn fixture()->Value{
        fixture_for(crate::accounts::Profile::BawRussia)
    }
    fn fixture_for(profile:crate::accounts::Profile)->Value{
        let mut d=crate::empty();crate::accounts::initialize(&mut d,profile).unwrap();
        let post=json!({"id":"p","postKey":"provider:p","title":"Source video","attachments":[{"type":"video"}]});
        d["posts"]=json!([post]);
        let version=crate::media_fullframes::source_version(&post,text(&d,"account"));
        let binding=crate::active_binding(&d).unwrap().to_json();
        d["settings"]["postMediaPolicies"]=json!({"p":{"version":1,"revision":1,"status":"active","postId":"p",
            "account":d["account"],"connectorBinding":binding,"sourceVersion":version,"mode":"full_audio_only"}});
        let mut p=crate::media_fullframes::initial(text(&d,"account"),&binding,&post,"2026-09-25T00:00:00Z");
        p["sourceVersion"]=json!(version);p["phase"]=json!("held");p["resumePhase"]=json!("scan");p["leaseEpoch"]=json!(7);
        p["materialEpoch"]=json!(material_epoch(&d,&post));p["nextSelectionIndex"]=json!(612);p["completedSelectedFrames"]=json!(612);
        p["source"]=json!({"sha256":"a".repeat(64),"bytes":1234});
        p["sourceIdentity"]=json!({"account":d["account"],"postKey":post["postKey"],"mediaSha256":"a".repeat(64),"durationMs":1029261});
        p["sourceProjection"]=json!({"account":d["account"],"postKey":post["postKey"],"title":"Source video","sourceUrl":"https://example.test/video"});
        d["jobs"]=json!([{"id":"origin","kind":"media","purpose":"auto_media","account":d["account"],"connectorBinding":binding,"visualContractVersion":2,"status":"failed","refId":"p","result":{"visualProgress":p}}]);d
    }
    fn result(expected:&Value)->Value{
        let p=&expected["progress"];
        json!({"materials":[{"id":"audio-fixture","kind":"transcript","title":"Audio","text":"Complete spoken words",
            "account":p["account"],"postKey":p["sourcePostKey"],"mediaSha256":p["source"]["sha256"],"sourceUrl":p["sourceProjection"]["sourceUrl"],
            "transcription":{"partial":false,"coverage":"full_audio","mediaDurationSeconds":1029.261,"audioDurationSeconds":1029.261,"sourceVersion":p["sourceVersion"]}}]})
    }
    #[test]
    fn reserve_keeps_visual_checkpoint_and_blocks_replay_including_failed_attempt(){
        let mut d=fixture();let origin=d["jobs"][0].clone();
        let (id,expected)=reserve(&mut d,"origin",7,"2026-09-25T00:00:01Z").unwrap();
        assert_eq!(d["jobs"][0],origin);assert_eq!(crate::row(&d,"jobs",&id).unwrap()["kind"],"media_audio");
        assert!(reserve(&mut d,"origin",7,"2026-09-25T00:00:02Z").is_err());
        crate::row_mut(&mut d,"jobs",&id).unwrap()["status"]=json!("failed");
        assert!(reserve(&mut d,"origin",7,"2026-09-25T00:00:02Z").is_err());
        assert_eq!(expected["progress"]["nextSelectionIndex"],612);
    }
    #[test]
    fn cached_audio_rejects_wrong_epoch_policy_company_source_active_lease_and_download(){
        for case in ["epoch","policy","company","source","lease","download","material","active"]{
            let mut d=fixture();
            match case{
                "epoch"=>d["jobs"][0]["result"]["visualProgress"]["leaseEpoch"]=json!(8),
                "policy"=>d["settings"]["postMediaPolicies"]["p"]["mode"]=json!("full_audio_visual"),
                "company"=>d["jobs"][0]["result"]["visualProgress"]["account"]=json!("LikeAvto"),
                "source"=>d["jobs"][0]["result"]["visualProgress"]["source"]["sha256"]=json!("b".repeat(64)),
                "lease"=>d["jobs"][0]["result"]["visualProgress"]["leaseId"]=json!("running"),
                "download"=>d["jobs"][0]["result"]["visualProgress"]["resumePhase"]=json!("download"),
                "material"=>d["jobs"][0]["result"]["visualProgress"]["materialEpoch"]=json!("changed"),
                _=>crate::list_mut(&mut d,"jobs").push(json!({"id":"other","kind":"media","status":"running"})),
            }
            assert!(reserve(&mut d,"origin",7,"2026-09-25T00:00:01Z").is_err(),"{case}");
        }
    }
    #[test]
    fn no_partial_mislabeled_foreign_or_visual_result_can_be_admitted(){
        let d=fixture();let expected=pin(&d,"origin",7).unwrap();let valid=result(&expected);
        assert!(validate_result(&valid,&expected).is_ok());
        for case in ["partial","duration","short_audio","coverage","source","source_version","account","visual","extra"]{
            let mut v=valid.clone();
            match case{
                "partial"=>v["materials"][0]["transcription"]["partial"]=json!(true),
                "duration"=>v["materials"][0]["transcription"]["mediaDurationSeconds"]=json!(600),
                "short_audio"=>v["materials"][0]["transcription"]["audioDurationSeconds"]=json!(600),
                "coverage"=>v["materials"][0]["transcription"]["coverage"]=json!("sampled"),
                "source"=>v["materials"][0]["mediaSha256"]=json!("b".repeat(64)),
                "source_version"=>v["materials"][0]["transcription"]["sourceVersion"]=json!("old-source"),
                "account"=>v["materials"][0]["account"]=json!("LikeAvto"),
                "visual"=>v["materials"][0]["kind"]=json!("visual_context"),
                _=>v["materials"].as_array_mut().unwrap().push(json!({"kind":"visual_context"})),
            }
            assert!(validate_result(&v,&expected).is_err(),"{case}");
        }
    }
    #[test]
    fn admission_rechecks_policy_revision_and_original_checkpoint(){
        for case in ["policy","cursor","cancel"]{
            let mut d=fixture();let (id,expected)=reserve(&mut d,"origin",7,"2026-09-25T00:00:01Z").unwrap();
            match case{
                "policy"=>d["settings"]["postMediaPolicies"]["p"]["revision"]=json!(2),
                "cursor"=>d["jobs"][0]["result"]["visualProgress"]["nextSelectionIndex"]=json!(644),
                _=>crate::row_mut(&mut d,"jobs",&id).unwrap()["status"]=json!("cancelled"),
            }
            let before=d.clone();assert!(admit(&mut d,&id,&expected,&result(&expected)).is_err(),"{case}");assert_eq!(d,before);
        }
    }
    #[test]
    fn successful_audio_admission_retains_incomplete_visual_checkpoint(){
        let mut d=fixture();let origin=d["jobs"][0].clone();
        let (id,expected)=reserve(&mut d,"origin",7,"2026-09-25T00:00:01Z").unwrap();
        let admitted=admit(&mut d,&id,&expected,&result(&expected)).unwrap();
        assert_eq!(admitted["visualContextStatus"],"not_available");
        assert_eq!(d["jobs"][0],origin);
        assert_eq!(d["jobs"][0]["result"]["visualProgress"]["phase"],"held");
        assert!(rows(&d,"materials").iter().all(|m|m["kind"]!="visual_context"));
        assert!(has_required_media(&d,&d["posts"][0],&crate::now()).unwrap());
    }
    #[test]
    fn old_audio_cannot_release_reauthorized_changed_video_at_same_post_key(){
        let mut d=fixture();let (id,expected)=reserve(&mut d,"origin",7,"2026-09-25T00:00:01Z").unwrap();
        admit(&mut d,&id,&expected,&result(&expected)).unwrap();
        let old_key=d["posts"][0]["postKey"].clone();
        d["posts"][0]["title"]=json!("Replacement source");
        let new_version=crate::media_fullframes::source_version(&d["posts"][0],text(&d,"account"));
        assert_ne!(new_version,expected["progress"]["sourceVersion"]);
        d["settings"]["postMediaPolicies"]["p"]["sourceVersion"]=json!(new_version);
        d["settings"]["postMediaPolicies"]["p"]["revision"]=json!(2);
        assert_eq!(d["posts"][0]["postKey"],old_key);
        assert_eq!(crate::post_media_policy::effective(&d,&d["posts"][0]).unwrap()["mode"],"full_audio_only");
        assert!(!has_required_media(&d,&d["posts"][0],&crate::now()).unwrap());
    }
    #[test]
    fn actual_startup_recovery_then_policy_accepts_fresh_epoch_without_visual_work(){
        for status in ["queued","running"]{
            let mut d=fixture();let policy=d["settings"]["postMediaPolicies"].clone();
            d["settings"]["postMediaPolicies"]=json!({});
            d["jobs"][0]["status"]=json!(status);
            d["jobs"][0]["result"]["visualProgress"]["phase"]=json!("scan");
            if status=="running" {d["jobs"][0]["result"]["visualProgress"]["leaseId"]=json!("old-process-lease");}
            crate::native_fixture_owner_repair::initialize_workspace(&mut d).unwrap(); crate::recover(&mut d).unwrap();
            assert_eq!(d["jobs"][0]["status"],"interrupted");
            super::super::recover(&mut d,"2026-09-25T00:00:01Z").unwrap();
            assert_eq!(d["jobs"][0]["status"],"paused");
            assert_eq!(d["jobs"][0]["mediaPolicyPause"],true);
            assert_eq!(d["jobs"][0]["result"]["visualProgress"]["leaseEpoch"],8);
            assert!(d["jobs"][0]["result"]["visualProgress"]["leaseId"].is_null());
            d["settings"]["postMediaPolicies"]=policy;
            let origin=d["jobs"][0].clone();
            assert!(reserve(&mut d,"origin",7,"2026-09-25T00:00:01Z").is_err());
            reserve(&mut d,"origin",8,"2026-09-25T00:00:01Z").unwrap();
            assert_eq!(d["jobs"][0],origin);
            assert_eq!(d["jobs"][0]["result"]["visualProgress"]["nextSelectionIndex"],612);
        }
    }
    #[test]
    fn audio_restart_stays_interrupted_and_never_redispatches(){
        let mut d=fixture();let (id,_)=reserve(&mut d,"origin",7,"2026-09-25T00:00:01Z").unwrap();
        crate::native_fixture_owner_repair::initialize_workspace(&mut d).unwrap(); crate::recover(&mut d).unwrap();
        super::super::recover(&mut d,"2026-09-25T00:00:01Z").unwrap();
        assert_eq!(crate::row(&d,"jobs",&id).unwrap()["status"],"interrupted");
        assert!(reserve(&mut d,"origin",7,"2026-09-25T00:00:01Z").is_err());
        assert!(scheduled_candidates(&d,1,false).is_empty());
    }
    #[test]
    fn exhausted_audio_resource_wait_remains_spent_and_is_not_automatically_retried(){
        let mut d=fixture();d["settings"]["postMediaPolicies"]=json!({});d["jobs"][0]["status"]=json!("queued");
        let (id,_)=claim_automatic(&mut d,"2026-09-25T00:00:01Z",false).unwrap().unwrap();
        let job=crate::row_mut(&mut d,"jobs",&id).unwrap();job["status"]=json!("failed");job["error"]=json!("gpu_gate_resource_wait_exhausted");
        job["resourceWait"]=json!({"resource":"gpu","state":"exhausted","reason":"gpu_gate_resource_wait_exhausted"});
        let pin=job["audioPin"].clone();
        crate::native_fixture_owner_repair::initialize_workspace(&mut d).unwrap(); crate::recover(&mut d).unwrap();super::super::recover(&mut d,"2026-09-25T00:00:02Z").unwrap();
        assert!(claim_automatic(&mut d,"2026-09-25T00:00:02Z",false).unwrap().is_none());
        assert!(reserve(&mut d,"origin",7,"2026-09-25T00:00:02Z").is_err());
        let job=crate::row(&d,"jobs",&id).unwrap();assert_eq!(job["status"],"failed");assert_eq!(job["audioPin"],pin);
    }
    #[test]
    fn audio_only_skips_visual_queue_without_marking_frames_complete(){
        let mut d=fixture();d["jobs"][0]["status"]=json!("queued");
        let origin=d["jobs"][0].clone();
        assert!(scheduled_candidates(&d,1,false).is_empty());
        assert!(claim_scoped(&mut d,"2026-09-25T00:00:01Z",false).unwrap().is_none());
        assert_eq!(d["jobs"][0]["status"],"paused");
        assert_eq!(d["jobs"][0]["mediaPolicyPause"],true);
        assert_eq!(d["jobs"][0]["result"],origin["result"]);
        assert_eq!(d["jobs"][0]["sourceAttempts"],origin["sourceAttempts"]);
        let before=input_digest(&d,"2026-09-25T00:00:01Z",false);
        d["settings"]["postMediaPolicies"]["p"]["mode"]=json!("full_audio_visual");
        assert_ne!(input_digest(&d,"2026-09-25T00:00:01Z",false),before);
        reconcile_scoped(&mut d,"2026-09-25T00:00:01Z",false).unwrap();
        assert_eq!(scheduled_candidates(&d,1,false),vec!["origin"]);
        assert!(d["jobs"][0].get("mediaPolicyPause").is_none());
    }
    #[test]
    fn long_probed_inventory_goes_straight_to_full_audio_without_visual_work(){
        let mut d=fixture();d["settings"]["postMediaPolicies"]=json!({});
        d["jobs"][0]["status"]=json!("queued");
        let p=&mut d["jobs"][0]["result"]["visualProgress"];
        p["phase"]=json!("inventory");p["nextSelectionIndex"]=json!(0);p["completedSelectedFrames"]=json!(0);
        let original=p.clone();assert!(automatic_candidate(&d,"2026-09-25T00:00:01Z",false).unwrap().is_some());assert!(scheduled_candidates(&d,1,false).is_empty());
        reconcile_scoped(&mut d,"2026-09-25T00:00:01Z",false).unwrap();
        assert_eq!(d["jobs"][0]["status"],"paused");
        assert_eq!(d["jobs"][0]["mediaPolicyPause"],true);
        let (id,expected)=claim_automatic(&mut d,"2026-09-25T00:00:01Z",false).unwrap().unwrap();
        assert_eq!(d["jobs"][0]["status"],"paused");assert_eq!(d["jobs"][0]["result"]["visualProgress"],original);
        assert_eq!(expected["policy"]["ownerAuthorizedAudioOnly"],false);
        admit(&mut d,&id,&expected,&result(&expected)).unwrap();
        assert!(has_required_media(&d,&d["posts"][0],&crate::now()).unwrap());
        assert!(rows(&d,"materials").iter().all(|m|m["kind"]!="visual_context"));
        assert_eq!(d["jobs"][0]["result"]["visualProgress"],original);
    }
    #[test]
    fn policy_pause_never_releases_unmarked_owner_pause_or_failed_work(){
        for status in ["paused","failed","cancelled"] {
            let mut d=fixture();d["jobs"][0]["status"]=json!(status);
            let origin=d["jobs"][0].clone();
            reconcile_scoped(&mut d,"2026-09-25T00:00:01Z",false).unwrap();
            assert_eq!(d["jobs"][0],origin);
            d["settings"]["postMediaPolicies"]["p"]["mode"]=json!("full_audio_visual");
            reconcile_scoped(&mut d,"2026-09-25T00:00:01Z",false).unwrap();
            assert_eq!(d["jobs"][0]["status"],status);
            assert!(scheduled_candidates(&d,1,false).is_empty());
        }
    }
    #[test]
    fn restored_visual_policy_still_obeys_closed_comment_scope(){
        let mut d=fixture();d["jobs"][0]["status"]=json!("queued");
        reconcile_scoped(&mut d,"2026-09-25T00:00:01Z",true).unwrap();
        assert_eq!(d["jobs"][0]["mediaPolicyPause"],true);
        for item in crate::list_mut(&mut d,"items"){item["workflow"]=json!("closed");}
        d["settings"]["postMediaPolicies"]["p"]["mode"]=json!("full_audio_visual");
        reconcile_scoped(&mut d,"2026-09-25T00:00:01Z",true).unwrap();
        assert_eq!(d["jobs"][0]["status"],"paused");
        assert_eq!(d["jobs"][0]["mediaScopePause"],true);
        assert!(scheduled_candidates(&d,1,true).is_empty());
    }
    #[test]
    fn short_inventory_or_failed_audio_never_enters_automatic_retry(){
        for duration in [179999,180000]{
            let mut d=fixture();d["settings"]["postMediaPolicies"]=json!({});d["jobs"][0]["status"]=json!("queued");
            d["jobs"][0]["result"]["visualProgress"]["phase"]=json!("inventory");
            d["jobs"][0]["result"]["visualProgress"]["sourceIdentity"]["durationMs"]=json!(duration);
            assert!(automatic_candidate(&d,"2026-09-25T00:00:01Z",false).unwrap().is_none());assert!(claim_automatic(&mut d,"2026-09-25T00:00:01Z",false).unwrap().is_none());
            assert!(scheduled_candidates(&d,1,false).is_empty(),"new default cannot retarget an unprofiled paid origin to visual work");
            let post=d["posts"][0].clone();
            d["settings"]["postMediaPolicies"]["p"]=json!({"version":1,"revision":1,"status":"active",
                "postId":"p","account":d["account"],"connectorBinding":crate::active_binding(&d).unwrap().to_json(),
                "sourceVersion":crate::media_fullframes::source_version(&post,account_scope(&d).unwrap()),
                "mode":"full_audio_visual"});
            assert_eq!(scheduled_candidates(&d,1,false),vec!["origin"],"exact visual authority retains the same cached checkpoint");
        }
        let mut d=fixture();d["settings"]["postMediaPolicies"]=json!({});d["jobs"][0]["status"]=json!("queued");
        let (id,_)=claim_automatic(&mut d,"2026-09-25T00:00:01Z",false).unwrap().unwrap();
        crate::row_mut(&mut d,"jobs",&id).unwrap()["status"]=json!("failed");d["jobs"][0]["status"]=json!("queued");
        assert!(claim_automatic(&mut d,"2026-09-25T00:00:02Z",false).unwrap().is_none());
    }
    #[tokio::test]
    async fn audio_preflight_block_does_not_reserve_or_repeat_writes(){
        let (app,_temp)=crate::tests::test_app().await;
        let mut d=fixture_for(crate::accounts::Profile::LikeAvto);d["settings"]["postMediaPolicies"]=json!({});d["jobs"][0]["status"]=json!("queued");
        app.change(|state|{for key in ["account","connectorBinding","settings","posts","jobs"]{state[key]=d[key].clone();}Ok(())}).await.unwrap();
        let blocked=||Err("media_config_missing_COMMUNITYHERO_MEDIA_WHISPER_MODEL".into());
        assert!(claim_ready_checked(&app,false,blocked()).await.unwrap().is_none());
        let held=app.read().await.unwrap();assert_eq!(held["jobs"].as_array().unwrap().len(),1);
        assert_eq!(held["jobs"][0]["workerBlock"],json!({"stage":"audio","code":"media_config_missing_COMMUNITYHERO_MEDIA_WHISPER_MODEL"}));
        assert_eq!(held["jobs"][0]["result"],d["jobs"][0]["result"]);
        assert!(claim_ready_checked(&app,false,blocked()).await.unwrap().is_none());
        assert_eq!(app.read().await.unwrap(),held);
    }
    #[tokio::test]
    async fn unavailable_cached_bytes_do_not_consume_automatic_audio_attempt(){
        let (app,_temp)=crate::tests::test_app().await;
        let mut d=fixture_for(crate::accounts::Profile::LikeAvto);d["settings"]["postMediaPolicies"]=json!({});d["jobs"][0]["status"]=json!("queued");
        let missing=crate::media_fullframes::hash(&json!(crate::id()));
        d["jobs"][0]["result"]["visualProgress"]["source"]["sha256"]=json!(missing);
        d["jobs"][0]["result"]["visualProgress"]["sourceIdentity"]["mediaSha256"]=json!(missing);
        reconcile_scoped(&mut d,&crate::now(),false).unwrap();
        assert_eq!(d["jobs"][0]["mediaPolicyPause"],true);
        app.change(|state|{for key in ["account","connectorBinding","settings","posts","jobs"]{state[key]=d[key].clone();}Ok(())}).await.unwrap();
        let before=app.read().await.unwrap();
        assert!(claim_ready_checked(&app,false,Ok(())).await.unwrap().is_none());
        let after=app.read().await.unwrap();assert_eq!(after["jobs"].as_array().unwrap().len(),1);
        assert_eq!(after["jobs"][0]["status"],"paused");assert_eq!(after["jobs"][0]["error"],"media_cached_audio_source_unavailable");
        assert!(after["jobs"][0]["mediaPolicyPause"].is_null());
        assert_eq!(after["jobs"][0]["result"],before["jobs"][0]["result"]);
        assert!(automatic_candidate(&after,&crate::now(),false).unwrap().is_none());
        assert_eq!(after["audit"],before["audit"]);
        // A second independent, verified cached source remains eligible on the next tick.
        let mut second=fixture_for(crate::accounts::Profile::LikeAvto);
        second["posts"][0]["id"]=json!("p2");second["posts"][0]["postKey"]=json!("provider:p2");
        let post=second["posts"][0].clone();
        let version=crate::media_fullframes::source_version(&post,text(&second,"account"));
        let epoch=material_epoch(&second,&post);
        let source=crate::media_fullframes::store().unwrap().put_bytes(b"verified cached source fixture").unwrap().to_json();
        let j=&mut second["jobs"][0];j["id"]=json!("origin2");j["refId"]=json!("p2");j["status"]=json!("queued");
        let p=&mut j["result"]["visualProgress"];
        p["sourcePostId"]=json!("p2");p["sourcePostKey"]=post["postKey"].clone();p["sourceVersion"]=json!(version);p["materialEpoch"]=json!(epoch);
        p["sourceIdentity"]["postKey"]=post["postKey"].clone();p["sourceIdentity"]["mediaSha256"]=source["sha256"].clone();p["source"]=source;
        p["sourceProjection"]["postKey"]=post["postKey"].clone();
        app.change(|state|{crate::list_mut(state,"posts").push(post.clone());crate::list_mut(state,"jobs").push(second["jobs"][0].clone());Ok(())}).await.unwrap();
        let (_,pin)=claim_ready_checked(&app,false,Ok(())).await.unwrap().unwrap();
        assert_eq!(pin["originJobId"],"origin2");
        // Restoring visual policy cannot silently reopen the unavailable first source.
        app.change(|state|{state["settings"]["mediaPolicyDefaults"]=json!({"audioOnlyAboveSeconds":1200});Ok(())}).await.unwrap();
        app.change_media(|state|reconcile_scoped(state,&crate::now(),false)).await.unwrap();
        let restored=app.read().await.unwrap();
        assert_eq!(restored["jobs"][0]["status"],"paused");
        assert_eq!(restored["jobs"][0]["error"],"media_cached_audio_source_unavailable");
        assert!(restored["jobs"][0]["mediaPolicyPause"].is_null());
    }
    #[test]
    fn automatic_reservation_rechecks_exact_source_after_artifact_verification(){
        for change in ["epoch","source","threshold"]{
            let mut d=fixture();d["settings"]["postMediaPolicies"]=json!({});d["jobs"][0]["status"]=json!("queued");
            let expected=automatic_candidate(&d,"2026-09-25T00:00:01Z",false).unwrap().unwrap();
            match change{
                "epoch"=>d["jobs"][0]["result"]["visualProgress"]["leaseEpoch"]=json!(8),
                "source"=>d["posts"][0]["title"]=json!("Replaced video"),
                _=>d["settings"]["mediaPolicyDefaults"]=json!({"audioOnlyAboveSeconds":1200}),
            }
            let before=d.clone();assert!(reserve_automatic(&mut d,&expected,"2026-09-25T00:00:01Z",false).unwrap().is_none());assert_eq!(d,before);
        }
    }
}
