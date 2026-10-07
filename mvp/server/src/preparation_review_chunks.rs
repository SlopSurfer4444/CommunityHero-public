//! Same-job stronger-review checkpoints. No approvals, dispatch or automatic resume.
use crate::*;
use sha2::{Digest,Sha256};
use std::collections::BTreeSet;
use axum::extract::Path;

const WEB_BUDGET:u64=8;
const ATTEMPTS:usize=2;
const MAX_INPUT_BYTES:u64=2_400_000;
fn hash(v:&Value)->String{format!("{:x}",Sha256::digest(v.to_string().as_bytes()))}
fn rows<'a>(v:&'a Value,k:&str)->&'a [Value]{v[k].as_array().map(Vec::as_slice).unwrap_or(&[])}
fn digest(v:&Value)->bool{v.as_str().is_some_and(|s|s.len()==64&&s.bytes().all(|b|b.is_ascii_hexdigit()))}
fn selected(request:&Value)->Result<BTreeSet<String>,&'static str>{
    let items=request["items"].as_array().filter(|v|!v.is_empty()&&v.len()<=100).ok_or("Invalid review recipients")?;
    let mut ids=BTreeSet::new();for item in items {
        let id=item["id"].as_str().filter(|s|!s.is_empty()&&s.len()<=256).ok_or("Invalid review recipient")?;
        if !ids.insert(id.to_owned()){return Err("Duplicate review recipient");}
    }Ok(ids)
}
fn subset(request:&Value,ids:&Value)->Value{
    let allowed:BTreeSet<String>=ids.as_array().into_iter().flatten()
        .filter_map(|id|id.as_str().map(str::to_owned)).collect();
    super::subset_request(request,&allowed)
}
fn review_request(job:&Value)->ApiResult<Value>{
    if job["preparationStages"]["first"]["status"]!="completed"||job["preparationStages"]["first"]["reviewRequired"]!=true {
        return Err(conflict("Durable review requires the complete persisted first pass"));
    }
    super::plan_review_for_job(job)
        .map_err(conflict)?.ok_or_else(||conflict("Stronger review is not required"))
}
fn validate_profile(profile:&Value,account:&str)->ApiResult<()> {
    let mut unsigned=profile.clone();if let Some(object)=unsigned.as_object_mut(){object.remove("profileSha256");}
    let route=profile["version"]==1&&profile["model"]=="gpt-6-astra"
        ||profile["version"]==2&&profile["model"]==crate::codex_model_policy::MODEL
        ||profile["version"]==3&&profile["model"]==crate::codex_model_policy::MODEL
            &&profile.get("webCallLimit")==Some(&Value::Null)
            &&profile["cliSha256"]==crate::codex_model_policy::CLI_SHA256
            &&profile["promptVersion"]=="communityhero-drafting-v21-review-uncapped-evidence";
    if !route||profile["account"]!=account||profile["reasoningEffort"]!="medium"
        || profile["promptVersion"].as_str().is_none_or(str::is_empty)
        || ["profileSha256","instructionSha256","toolsProfileSha256","runtimeSha256","cliSha256"].iter().any(|k|!digest(&profile[*k]))
        ||profile["profileSha256"]!=hash(&unsigned) {
        return Err(conflict("Review runtime profile is unavailable or changed"));
    }Ok(())
}
pub(crate) fn present(job:&Value)->bool{job["preparationStages"]["reviewChunks"].is_object()}
fn plan_digest(state:&Value)->String{
    let mut plan=state.clone();plan.as_object_mut().unwrap().remove("planDigest");plan.as_object_mut().unwrap().remove("usage");plan["status"]=json!("pending");
    if let Some(chunks)=plan["chunks"].as_array_mut(){for chunk in chunks{chunk["attempts"]=json!([]);chunk["result"]=Value::Null;}}
    hash(&plan)
}

/// Validate the captured work's identity, not whether its sources are still current.
/// Settlement preserves evidence for this attempt only; it grants no admission.
pub(crate) fn owned(d:&Value,job:&Value)->ApiResult<()> {
    let binding=active_binding(d)?;
    let account=bridge_account(&binding)?;
    let bundle=&job["prepareBundle"];
    let state=&job["preparationStages"]["reviewChunks"];
    validate_profile(&state["profile"],account)?;
    if job["kind"]!="assistant"||!["engine_prepare","auto_prepare","auto_revalidate"].contains(&job["purpose"].as_str().unwrap_or(""))
        ||bundle["version"]!=1||bundle["digest"]!=hash(&bundle["request"])
        ||bundle["request"]["account"]!=d["account"]||bundle["request"]["connectorBinding"]!=binding.to_json()
        ||state["bundleId"]!=bundle["id"]||state["bundleDigest"]!=bundle["digest"]
        ||state["requestDigest"]!=hash(&review_request(job)?)||state["connectorBinding"]!=binding.to_json()
        ||state["planDigest"]!=plan_digest(state){
        return Err(conflict("Review checkpoint ownership changed"));
    }
    Ok(())
}

/// Exact revisions and uncertain/completed operations are checked for every recipient,
/// before spending and before all-or-nothing proposal admission.
pub(crate) fn current(d:&Value,job:&Value)->ApiResult<()> {
    let binding=active_binding(d)?;
    let account=bridge_account(&binding)?;
    let bundle=&job["prepareBundle"];
    let review=review_request(job)?;
    if !job["preparationStages"]["groupAdmission"].is_array(){
        prepare_bundle::current(d,bundle).map_err(conflict)?;
    }else{
        for original in rows(&review,"items") {
            let id=required(original,"id")?;
            let group=rows(&job["preparationStages"],"groupAdmission").iter()
                .find(|g|rows(g,"itemIds").iter().any(|v|v.as_str()==Some(id)))
                .ok_or_else(||conflict("Review group is missing"))?;
            if group["status"]!="pending"||prepare_bundle::current_group(d,bundle,group).is_err() {
                return Err(conflict("Review group evidence changed"));
            }
        }
    }
    if bundle["request"]["connectorBinding"]!=binding.to_json(){return Err(conflict("Review connector binding changed"));}
    for original in rows(&review,"items") {
        let item=row(d,"items",required(original,"id")?)?;
        if item["revision"]!=original["revision"]||item["draftEdited"]==true||!item["draft"].as_str().unwrap_or("").is_empty()
            || !matches!(item["providerStatus"].as_str(),Some("new"|"inprogress"))
            || list(d,"operations").iter().any(|o|o["itemId"]==item["id"]&&matches!(o["status"].as_str(),Some("dispatching"|"unknown"|"succeeded"))) {
            return Err(conflict("Review recipient changed or has a protected operation"));
        }
    }
    if present(job){
        let state=&job["preparationStages"]["reviewChunks"];
        validate_profile(&state["profile"],account)?;
        if state["bundleId"]!=bundle["id"]||state["bundleDigest"]!=bundle["digest"]
            ||state["requestDigest"]!=hash(&review_request(job)?)||state["connectorBinding"]!=binding.to_json()
            ||state["planDigest"]!=plan_digest(state){
            return Err(conflict("Review checkpoint binding changed"));
        }
    }Ok(())
}
fn initialize(d:&mut Value,run:&str,profile:&Value)->ApiResult<()> {
    let job=row(d,"jobs",run)?.clone();current(d,&job)?;
    validate_profile(profile,bridge_account(&active_binding(d)?)?)?;
    if present(&job){if job["preparationStages"]["reviewChunks"]["profile"]!=*profile{return Err(conflict("Review runtime profile changed; start a fresh review"));}return Ok(());}
    if !job["preparationStages"]["review"].is_null(){return Err(conflict("Legacy review failure requires a fresh review"));}
    let request=review_request(&job)?;selected(&request).map_err(conflict)?;
    let ids:Vec<Value>=rows(&request,"items").iter().map(|i|i["id"].clone()).collect();
    let chunks:Vec<_>=ids.chunks(25).enumerate().map(|(index,ids)|json!({"id":format!("chunk-{}",index+1),"itemIds":ids,"attempts":[],"result":null})).collect();
    let uncapped=profile["version"]==3;
    let mut state=json!({"version":if uncapped{2}else{1},"bundleId":job["prepareBundle"]["id"],"bundleDigest":job["prepareBundle"]["digest"],
        "requestDigest":hash(&request),"connectorBinding":job["prepareBundle"]["request"]["connectorBinding"],"profile":profile,"chunks":chunks,
        "maxWebCalls":if uncapped{Value::Null}else{json!(WEB_BUDGET)},"maxInputBytes":((request.to_string().len() as u64+2048)*4).min(MAX_INPUT_BYTES),
        "status":"pending","automaticResume":false});
    state["planDigest"]=json!(plan_digest(&state));
    row_mut(d,"jobs",run)?["preparationStages"]["reviewChunks"]=state;Ok(())
}
fn usage(state:&Value)->(u64,u64){
    let mut calls=0;let mut bytes=0;
    for chunk in rows(state,"chunks"){for attempt in rows(chunk,"attempts"){
        // Missing completion is never interpreted as zero spend.
        calls+=if attempt["status"]=="completed"{attempt["observedWebCalls"].as_u64().unwrap_or(WEB_BUDGET)}else if state["version"]==2{0}else{attempt["reservedWebCalls"].as_u64().unwrap_or(WEB_BUDGET)};
        bytes+=attempt["inputBytes"].as_u64().unwrap_or(MAX_INPUT_BYTES);
    }}(calls,bytes)
}
fn update_usage(state:&mut Value){
    let (charged,bytes)=usage(state);let mut observed=0;let mut uncertain=0;let mut reserved=0;
    for chunk in rows(state,"chunks"){for attempt in rows(chunk,"attempts"){
        if attempt["status"]=="completed"{observed+=attempt["observedWebCalls"].as_u64().unwrap_or(WEB_BUDGET);}
        else{reserved+=attempt["reservedWebCalls"].as_u64().unwrap_or(WEB_BUDGET);if attempt["status"]=="unknown"{uncertain+=1;}}
    }}
    if state["version"]==2 {
        let pending=rows(state,"chunks").iter().flat_map(|c|rows(c,"attempts")).any(|a|a["status"]!="completed");
        state["usage"]=json!({"chargedWebCalls":if pending{Value::Null}else{json!(observed)},
            "observedCompletedWebCalls":observed,"reservedUnconfirmedWebCalls":if pending{Value::Null}else{json!(0)},
            "unknownAttemptCount":uncertain,"inputBytes":bytes,"webBudget":null,"actualTotalWebCallsKnown":!pending,
            "enforcement":"durable_attempt_ownership_and_observed_activity"});
        return;
    }
    state["usage"]=json!({"chargedWebCalls":charged,"observedCompletedWebCalls":observed,"reservedUnconfirmedWebCalls":reserved,
        "unknownAttemptCount":uncertain,"inputBytes":bytes,"webBudget":WEB_BUDGET,"actualTotalWebCallsKnown":reserved==0,
        "enforcement":"durable_reservation_and_observed_event_rejection"});
}
pub(crate) fn validate_state_policy(state:&Value)->ApiResult<()> {
    let valid=state["version"]==1&&state["maxWebCalls"].as_u64()==Some(WEB_BUDGET)
        &&matches!(state["profile"]["version"].as_u64(),Some(1|2))
        ||state["version"]==2&&state.get("maxWebCalls")==Some(&Value::Null)
            &&state["profile"]["version"]==3&&state["profile"].get("webCallLimit")==Some(&Value::Null);
    if !valid{return Err(conflict("Invalid review state policy"));}Ok(())
}
pub(crate) fn validate_usage(state:&Value)->ApiResult<()> {
    validate_state_policy(state)?;
    let mut expected=state.clone();update_usage(&mut expected);
    if expected["usage"]!=state["usage"]{return Err(conflict("Review usage differs from durable attempts"));}Ok(())
}
fn reserve(d:&mut Value,run:&str,at:&str)->ApiResult<Option<Value>>{
    let job=row(d,"jobs",run)?.clone();current(d,&job)?;
    if job["status"]!="running"{return Err(conflict("Review job no longer running"));}
    let request=review_request(&job)?;let state=&job["preparationStages"]["reviewChunks"];
    let Some(index)=rows(state,"chunks").iter().position(|c|c["result"].is_null()) else{return Ok(None);};
    let chunk=&state["chunks"][index];let attempts=rows(chunk,"attempts");
    if attempts.len()>=ATTEMPTS||attempts.last().is_some_and(|a|a["status"]=="running") {return Err(conflict("Review chunk requires explicit resume or exhausted attempts"));}
    if attempts.last().is_some_and(|a|a["retryable"]==false){return Err(conflict("Review chunk failure requires a fresh review"));}
    validate_state_policy(state)?;
    let (spent,bytes)=usage(state);if state["version"]==1&&spent>=WEB_BUDGET{return Err(conflict("Review aggregate web reservation exhausted"));}
    let mut req=subset(&request,&chunk["itemIds"]);
    // App::bridge canonicalizes the company; hash precisely that wire request.
    req["account"]=state["profile"]["account"].clone();
    let request_hash=hash(&req);
    let wire_request=req.to_string();
    // Divide only the uncharged budget among unfinished chunks. Existing
    // attempts keep their durable reservation, including uncertain outcomes.
    let unfinished=rows(state,"chunks").iter().filter(|c|c["result"].is_null()).count() as u64;
    let max_web_calls=if state["version"]==2{Value::Null}else{json!((WEB_BUDGET-spent+unfinished-1)/unfinished)};
    let contract=json!({"version":if state["version"]==2{2}else{1},"attemptId":id(),"chunkId":chunk["id"],"profileSha256":state["profile"]["profileSha256"],"requestSha256":request_hash,"maxWebCalls":max_web_calls});
    req["reviewChunk"]=contract.clone();let size=req.to_string().len() as u64;
    // Bind original UTF-8 bytes without pretending Rust and JS serialize numbers
    // identically. The adapter validates this copy and never includes it in model input.
    req["reviewChunkRequestJson"]=json!(wire_request);
    if bytes+size>state["maxInputBytes"].as_u64().unwrap_or(0){return Err(conflict("Review aggregate input budget exhausted"));}
    crate::conductor_authority::fence_job_capture(d,run,"review")?;
    let saved=&mut row_mut(d,"jobs",run)?["preparationStages"]["reviewChunks"];
    saved["chunks"][index]["attempts"].as_array_mut().unwrap().push(json!({"id":contract["attemptId"],"status":"running","startedAt":at,
        "contract":contract,"requestDigest":request_hash,"inputBytes":size,"reservedWebCalls":contract["maxWebCalls"],"observedWebCalls":null}));
    saved["status"]=json!("running");update_usage(saved);Ok(Some(req))
}
fn save(d:&mut Value,run:&str,request:&Value,outcome:Result<&Value,&str>,at:&str)->ApiResult<()> {
    let job=row(d,"jobs",run)?.clone();let state=&job["preparationStages"]["reviewChunks"];
    owned(d,&job)?;
    let contract=&request["reviewChunk"];
    let index=rows(state,"chunks").iter().position(|c|c["id"]==contract["chunkId"]).ok_or_else(||conflict("Review chunk missing"))?;
    let attempt=rows(&state["chunks"][index],"attempts").last().ok_or_else(||conflict("Review reservation missing"))?;
    if attempt["status"]!="running"||attempt["contract"]!=*contract{return Err(conflict("Review attempt ownership changed"));}
    let mut expected=subset(&review_request(&job)?,&state["chunks"][index]["itemIds"]);
    expected["account"]=state["profile"]["account"].clone();
    let wire=expected.to_string();
    if contract["requestSha256"]!=hash(&expected)||attempt["requestDigest"]!=contract["requestSha256"] {
        return Err(conflict("Review request differs from reservation"));
    }
    expected["reviewChunk"]=contract.clone();expected["reviewChunkRequestJson"]=json!(wire);
    if *request!=expected{return Err(conflict("Review return request binding changed"));}
    let validated=match outcome {
        Ok(result)=>{
            let mut clean=super::clean_result(result,&selected(request).map_err(bad)?).map_err(bad)?;
            let metadata=prepare_bundle::generation_metadata(result).map_err(bad)?.ok_or_else(||bad("Chunk provenance missing"))?;
            super::validate_evidence_quality_company(&metadata,d["account"].as_str().unwrap_or(""),result).map_err(bad)?;
            if metadata["schemaVersion"]!=1||metadata["reviewChunk"]!=*contract||metadata["model"]!=state["profile"]["model"]
                || metadata["reasoningEffort"]!=state["profile"]["reasoningEffort"]||metadata["promptVersion"]!=state["profile"]["promptVersion"]
                ||metadata["cliSha256"]!=state["profile"]["cliSha256"]
                || metadata["research"]["toolsProfileSha256"]!=state["profile"]["toolsProfileSha256"]{
                return Err(bad("Review chunk provenance differs from reservation"));
            }
            prepare_bundle::validate_image_evidence_binding(&metadata,&json!({"request":request})).map_err(bad)?;
            let observed=metadata["research"]["webCalls"].as_u64().filter(|n|*n<=9_007_199_254_740_991&&(contract["version"]==2||*n<=contract["maxWebCalls"].as_u64().unwrap_or(0))).ok_or_else(||bad("Review exceeded reserved web calls"))?;
            clean["runMetadata"]=metadata;Some((clean,observed))
        },Err(_)=>None
    };
    let saved=&mut row_mut(d,"jobs",run)?["preparationStages"]["reviewChunks"];
    let chunk=&mut saved["chunks"][index];let attempt=chunk["attempts"].as_array_mut().unwrap().last_mut().unwrap();
    attempt["finishedAt"]=json!(at);
    if let Some((clean,observed))=validated{
        attempt["status"]=json!("completed");attempt["observedWebCalls"]=json!(observed);attempt["resultDigest"]=json!(hash(&clean));chunk["result"]=clean;
    }else{
        let (code,retryable)=super::failure_category(outcome.err().unwrap());
        attempt["status"]=json!("unknown");attempt["errorCode"]=json!(code);attempt["retryable"]=json!(retryable);
        saved["status"]=json!("held");
    }
    if rows(saved,"chunks").iter().all(|c|!c["result"].is_null()){saved["status"]=json!("completed");}
    update_usage(saved);Ok(())
}

/// Independently admit the storage delta against the prior reservation. A
/// writer-supplied completed label must never refund unobserved work. Reuse
/// save's validation on a job-only projection, not a clone of the full workspace.
pub(crate) fn validate_settlement(d:&Value,prior:&Value,next:&Value)->ApiResult<()> {
    owned(d,next)?;
    let before=&prior["preparationStages"]["reviewChunks"];
    let after=&next["preparationStages"]["reviewChunks"];
    let changed:Vec<_>=rows(before,"chunks").iter().zip(rows(after,"chunks")).enumerate()
        .filter(|(_, (old,new))|old!=new).collect();
    if changed.len()!=1{return Err(conflict("Review settlement must finish one reserved attempt"));}
    let (index,(old,chunk))=changed[0];
    let old_attempts=rows(old,"attempts");let attempts=rows(chunk,"attempts");
    let attempt=attempts.last().ok_or_else(||conflict("Review settlement has no attempt"))?;
    if old_attempts.len()!=attempts.len()||old_attempts.last().is_none_or(|a|a["status"]!="running") {
        return Err(conflict("Review settlement has no prior running reservation"));
    }
    let at=attempt["finishedAt"].as_str().filter(|at|chrono::DateTime::parse_from_rfc3339(at).is_ok())
        .ok_or_else(||bad("Review settlement completion time missing"))?;
    let expected=match attempt["status"].as_str() {
        Some("completed")=>{
            let contract=&old_attempts.last().unwrap()["contract"];
            let mut request=subset(&review_request(prior)?,&old["itemIds"]);
            request["account"]=before["profile"]["account"].clone();
            let wire=request.to_string();
            request["reviewChunk"]=contract.clone();request["reviewChunkRequestJson"]=json!(wire);
            let mut projected=json!({"account":d["account"],"jobs":[prior]});
            if let Some(binding)=d.get("connectorBinding"){projected["connectorBinding"]=binding.clone();}
            save(&mut projected,required(prior,"id")?,&request,Ok(&chunk["result"]),at)?;
            projected["jobs"][0]["preparationStages"]["reviewChunks"].clone()
        },
        Some("unknown")=>{
            // UNKNOWN carries no observed result and never releases its grant.
            let code=required(attempt,"errorCode")?;
            let reason=match code {"ADAPTER_PROCESS_FAILED"=>"Adapter process failed",
                "PREPARATION_CONTEXT_CHANGED"=>"Preparation evidence changed",_=>code};
            let (expected_code,retryable)=super::failure_category(reason);
            if code!=expected_code||attempt["retryable"]!=retryable{return Err(bad("Invalid review failure category"));}
            let mut expected=before.clone();
            let saved=expected["chunks"][index]["attempts"].as_array_mut().unwrap().last_mut().unwrap();
            saved["status"]=json!("unknown");saved["finishedAt"]=json!(at);
            saved["errorCode"]=json!(code);saved["retryable"]=json!(retryable);
            expected["status"]=json!("held");update_usage(&mut expected);expected
        },
        _=>return Err(bad("Review settlement is not terminal")),
    };
    if expected!=*after{return Err(conflict("Review settlement differs from validated result and usage"));}
    Ok(())
}
pub(crate) fn aggregate(job:&Value)->ApiResult<Value>{
    let request=review_request(job)?;let state=&job["preparationStages"]["reviewChunks"];
    if rows(state,"chunks").is_empty()||rows(state,"chunks").iter().any(|c|c["result"].is_null()){return Err(conflict("Review chunks are incomplete"));}
    let mut result=json!({"text":"","sources":[],"assessments":[],"proposals":[]});let mut chunks=vec![];let mut editorial=vec![];let mut has_editorial=false;
    for chunk in rows(state,"chunks"){
        let allowed=selected(&subset(&request,&chunk["itemIds"])).map_err(bad)?;
        let clean=super::clean_result(&chunk["result"],&allowed).map_err(bad)?;
        let attempt=rows(chunk,"attempts").last().ok_or_else(||bad("Review checkpoint missing"))?;
        if attempt["status"]!="completed"||attempt["resultDigest"]!=hash(&chunk["result"]){return Err(bad("Review checkpoint changed"));}
        result["text"]=json!(format!("{}{}\n",result["text"].as_str().unwrap(),clean["text"].as_str().unwrap()));
        for key in ["assessments","proposals"]{result[key].as_array_mut().unwrap().extend_from_slice(rows(&clean,key));}
        if let Some(proof)=clean.get("editorialEvidence") {
            has_editorial=true;editorial.extend_from_slice(rows(proof,"entries"));
        }
        chunks.push(json!({"id":chunk["id"],"itemIds":chunk["itemIds"],"attemptId":attempt["id"],"resultDigest":attempt["resultDigest"],"metadata":chunk["result"]["runMetadata"]}));
    }
    if has_editorial {result["editorialEvidence"]=json!({"version":1,"contract":crate::editorial_review::CONTRACT,"entries":editorial});}
    result=super::clean_result(&result,&selected(&request).map_err(bad)?).map_err(bad)?;
    let (charged,bytes)=usage(state);if state["version"]==1&&charged>WEB_BUDGET||bytes>state["maxInputBytes"].as_u64().unwrap_or(0){return Err(bad("Review aggregate budget exceeded"));}
    result["runMetadata"]=json!({"schemaVersion":2,"kind":"durable_review_chunks","planDigest":state["planDigest"],"profile":state["profile"],
        "bundleDigest":state["bundleDigest"],"requestDigest":state["requestDigest"],"chargedWebCalls":charged,"inputBytes":bytes,"chunks":chunks});
    if state["version"]==2 {
        let mut observed=state.clone();update_usage(&mut observed);
        result["runMetadata"]["webCallLimit"]=Value::Null;
        for key in ["chargedWebCalls","observedCompletedWebCalls","unknownAttemptCount","actualTotalWebCallsKnown"] {result["runMetadata"][key]=observed["usage"][key].clone();}
    }
    research_projection(&result["runMetadata"]).map_err(bad)?;
    Ok(result)
}

async fn read_completed(app:&App,run:&str,preflight:fn(&Value,&str)->ApiResult<()>)->ApiResult<Value>{
    app.db.read_preparation_context(run).await.and_then(|d|{preflight(&d,run)?;current(&d,row(&d,"jobs",run)?)?;aggregate(row(&d,"jobs",run)?)})
}

fn reserve_admitted(d:&mut Value,token:&crate::runtime_lifecycle::OwnerToken,run:&str,
    preflight:fn(&Value,&str)->ApiResult<()>,at:&str)->ApiResult<Option<Value>> {
    crate::runtime_lifecycle::require_admission(d,token,crate::runtime_lifecycle::AdmissionClass::Preparation)?;
    preflight(d,run)?;reserve(d,run,at)
}
pub(crate) async fn run(app:&App,run:&str,preflight:fn(&Value,&str)->ApiResult<()>)->ApiResult<Value>{
    app.db.read_preparation_context(run).await.and_then(|d|preflight(&d,run))?;
    let profile=app.bridge("assistant",json!({"purpose":"review_profile"})).await?;
    app.change_preparation_review_checkpoint(run,|d|{preflight(d,run)?;initialize(d,run,&profile)}).await?;
    loop {
        let lifecycle=app.lifecycle_admission_token(crate::runtime_lifecycle::AdmissionClass::Preparation).await?;
        // Count the physical slot before the paid writer admission. A drain
        // racing the commit must await this same owned stage, never replay it.
        let native=app.lifecycle_work.begin(crate::runtime_owned_work::Kind::Preparation)?;
        let next=app.change_preparation_review_checkpoint(run,|d|{
            reserve_admitted(d,&lifecycle,run,preflight,&now())
        }).await?;
        let Some(request)=next else{return read_completed(app,run,preflight).await;};
        match app.bridge_admitted("assistant",request.clone(),native).await {
            Ok(result)=>{
                let saved=app.change_preparation_review_checkpoint(run,|d|{
                    // Commit owned, validated output before any live-source gate.
                    // Next reservation/read_completed still performs full preflight.
                    save(d,run,&request,Ok(&result),&now())?;
                    Ok(row(d,"jobs",run)?["preparationStages"]["reviewChunks"]["status"]=="completed")
                }).await;
                match saved {
                    // Commit the final chunk before checking the fresh snapshot.
                    // No empty reservation transaction is needed after completion.
                    Ok(true)=>return read_completed(app,run,preflight).await,
                    Ok(false)=>(),
                    Err(error)=>{
                        app.change_preparation_review_checkpoint(run,|d|save(d,run,&request,Err(&error.1),&now())).await?;return Err(error);
                    }
                }
            },Err(error)=>{app.change_preparation_review_checkpoint(run,|d|save(d,run,&request,Err(&error.1),&now())).await?;return Err(conflict(&super::failure_message(&error.1)));}
        }
    }
}

fn claim_resume(d:&mut Value,run:&str,expected:&str)->ApiResult<String>{
    let job=row(d,"jobs",run)?.clone();
    if !matches!(job["status"].as_str(),Some("failed"|"interrupted"))||!present(&job)
        ||job["preparationStages"]["reviewChunks"]["planDigest"]!=expected||!job["prepareOutcome"].is_null(){return Err(conflict("Review is not resumable from this exact checkpoint"));}
    current(d,&job)?;
    let purpose=required(&job,"purpose")?.to_owned();
    if !["engine_prepare","auto_prepare","auto_revalidate"].contains(&purpose.as_str()){return Err(conflict("Unsupported review job"));}
    if crate::preparation_workers::pending_conflict(d,&job,true){return Err(conflict("Another assistant job owns this preparation scope"));}
    check_pending_reservations(d,&job)?;
    if purpose!="engine_prepare" {
        let key=if purpose=="auto_prepare"{"autoPreparation"}else{"autoRevalidation"};
        let ids=job["prepareBundle"]["itemIds"].as_array().filter(|ids|!ids.is_empty()).ok_or_else(||conflict("Review recipients missing"))?;
        let request_ids=selected(&job["prepareBundle"]["request"]).map_err(conflict)?;
        let bound_ids:BTreeSet<_>=ids.iter().filter_map(|id|id.as_str().map(str::to_owned)).collect();
        if bound_ids.len()!=ids.len()||bound_ids!=request_ids
            ||(purpose=="auto_revalidate"&&(ids.len()!=1||ids[0]!=job["refId"])) {
            return Err(conflict("Automatic review recipient binding changed"));
        }
        let pending=pending_recipients(&job,&bound_ids);
        // Already admitted groups are immutable outcomes, not resume targets.
        for id in &pending {if row(d,"items",id)?[key]["jobId"]!=run{return Err(conflict("Automatic review ownership changed"));}}
        for id in &pending {
            let item=row_mut(d,"items",id)?;
            item[key]["status"]=json!("running");item[key]["retryAt"]=Value::Null;
            if key=="autoPreparation"{item[key]["reviewResumeRequired"]=json!(false);}
        }
    }
    let saved=row_mut(d,"jobs",run)?;
    for chunk in saved["preparationStages"]["reviewChunks"]["chunks"].as_array_mut().unwrap(){
        for attempt in chunk["attempts"].as_array_mut().unwrap(){if attempt["status"]=="running"{attempt["status"]=json!("unknown");attempt["errorCode"]=json!("REVIEW_PROCESS_INTERRUPTED");attempt["retryable"]=json!(true);}}
    }
    update_usage(&mut saved["preparationStages"]["reviewChunks"]);
    saved["status"]=json!("running");saved["finishedAt"]=Value::Null;saved["error"]=Value::Null;Ok(purpose)
}

fn pending_recipients(job:&Value,all:&BTreeSet<String>)->BTreeSet<String>{
    if !matches!(job["purpose"].as_str(),Some("auto_prepare"|"engine_prepare"))||!job["preparationStages"]["groupAdmission"].is_array(){return all.clone();}
    rows(&job["preparationStages"],"groupAdmission").iter().filter(|g|g["status"]=="pending")
        .flat_map(|g|rows(g,"itemIds").iter().filter_map(|id|id.as_str().map(str::to_owned)))
        .collect()
}
fn check_pending_reservations(d:&Value,job:&Value)->ApiResult<()> {
    let all: BTreeSet<String>=rows(&job["prepareBundle"],"itemIds").iter().filter_map(|id|id.as_str().map(str::to_owned)).collect();
    let pending=pending_recipients(job,&all).into_iter().collect::<Vec<_>>();
    crate::preparation_reservations::assert_available(d,&pending,Some(required(job,"id")?))
}
pub(crate) async fn resume(State(app):State<App>,Path(run):Path<String>,Json(body):Json<Value>)->ApiResult<Json<Value>>{
    if body.get("expectedFirstRequestDigest").is_some(){
        if body["resumePendingWork"]==true{
            if body.as_object().is_none_or(|o|o.len()!=4||!["expectedFirstRequestDigest","expectedNeedSetDigest","resumePendingWork","maxRepairRounds"].iter().all(|k|o.contains_key(*k)))
                ||!digest(&body["expectedFirstRequestDigest"])||!digest(&body["expectedNeedSetDigest"])||body["maxRepairRounds"]!=1{return Err(bad("Pending work resume requires exact original request/need digests and one repair round"));}
            let token=app.lifecycle_admission_token(crate::runtime_lifecycle::AdmissionClass::Preparation).await?;
            app.change(|d|crate::answering_repair_plan::claim_pending_resume(d,&token,&run,body["expectedFirstRequestDigest"].as_str().unwrap(),body["expectedNeedSetDigest"].as_str().unwrap(),&now())).await?;
            engine_prepare::spawn_pending_work_resume(&app,run.clone());
            return Ok(Json(json!({"jobId":run,"status":"running","originalFirstReplayed":false,"maxRepairRounds":1})));
        }
        if body["resumeReview"]==true{
            if body.as_object().is_none_or(|o|o.len()!=2)||!digest(&body["expectedFirstRequestDigest"]){return Err(bad("First review resume requires exact request digest and resumeReview only"));}
            let token=app.lifecycle_admission_token(crate::runtime_lifecycle::AdmissionClass::Preparation).await?;
            app.change(|d|{crate::runtime_lifecycle::require_admission(d,&token,crate::runtime_lifecycle::AdmissionClass::Preparation)?;
                let job=row(d,"jobs",&run)?.clone();if !matches!(job["purpose"].as_str(),Some("engine_prepare"|"auto_prepare"|"auto_revalidate"))||!matches!(job["status"].as_str(),Some("failed"|"interrupted"))||!job["prepareOutcome"].is_null()||present(&job)
                    ||job["prepareBundle"]["digest"]!=body["expectedFirstRequestDigest"]||hash(&job["prepareBundle"]["request"])!=body["expectedFirstRequestDigest"]
                    ||job["preparationStages"]["first"]["status"]!="completed"||job["preparationStages"]["first"]["reviewRequired"]!=true{return Err(conflict("Original completed first has no unplanned review to resume"));}
                current(d,&job)?;crate::preparation_materials::require_request(d,&job["prepareBundle"]["request"]).map_err(conflict)?;
                if crate::preparation_workers::pending_conflict(d,&job,true){return Err(conflict("Another assistant owns review scope"));}check_pending_reservations(d,&job)?;
                if job["purpose"]!="engine_prepare"{let key=if job["purpose"]=="auto_prepare"{"autoPreparation"}else{"autoRevalidation"};let all=selected(&job["prepareBundle"]["request"]).map_err(conflict)?;
                    for id in pending_recipients(&job,&all){if row(d,"items",&id)?[key]["jobId"]!=run{return Err(conflict("Automatic original review item ownership changed"));}let item=row_mut(d,"items",&id)?;item[key]["status"]=json!("running");item[key]["retryAt"]=Value::Null;if key=="autoPreparation"{item[key]["reviewResumeRequired"]=json!(false);}}}
                let saved=row_mut(d,"jobs",&run)?;saved["status"]=json!("running");saved["finishedAt"]=Value::Null;saved["error"]=Value::Null;Ok(())}).await?;
            let snapshot=app.db.read_preparation_context(&run).await?;let purpose=row(&snapshot,"jobs",&run)?["purpose"].clone();drop(snapshot);
            if purpose=="engine_prepare"{engine_prepare::spawn_review_resume(&app,run.clone());}else{auto_prepare::spawn_review_resume(&app,run.clone());}return Ok(Json(json!({"jobId":run,"status":"running","originalFirstReplayed":false,"reviewResumeRequired":true})));
        }
        if body.as_object().is_none_or(|o|o.len()!=1)||!digest(&body["expectedFirstRequestDigest"]){return Err(bad("Original capture recovery requires expectedFirstRequestDigest only"));}
        return engine_prepare::recover_first_capture(&app,&run,body["expectedFirstRequestDigest"].as_str().unwrap()).await.map(Json);
    }
    if body.as_object().is_none_or(|o|o.len()!=1)||!digest(&body["expectedPlanDigest"]){return Err(bad("Resume requires expectedPlanDigest only"));}
    let lifecycle=app.lifecycle_admission_token(crate::runtime_lifecycle::AdmissionClass::Preparation).await?;
    let purpose=app.change(|d|{
        crate::runtime_lifecycle::require_admission(d,&lifecycle,crate::runtime_lifecycle::AdmissionClass::Preparation)?;
        claim_resume(d,&run,body["expectedPlanDigest"].as_str().unwrap())
    }).await?;
    if purpose=="engine_prepare"{engine_prepare::spawn_review_resume(&app,run.clone());}
    else{auto_prepare::spawn_review_resume(&app,run.clone());}
    Ok(Json(json!({"jobId":run,"status":"running","dispatchAuthorized":false})))
}

/// Cheap discovery only. The actual recovery revalidates profile, ownership,
/// every settlement and currentness inside the admission transaction.
pub(crate) fn completed_candidate(job:&Value)->bool {
    let state=&job["preparationStages"]["reviewChunks"];
    job["kind"]=="assistant"&&matches!(job["purpose"].as_str(),Some("engine_prepare"|"auto_prepare"))
        &&matches!(job["status"].as_str(),Some("failed"|"interrupted"))
        &&job["prepareOutcome"].is_null()&&job["preparationStages"]["review"].is_null()
        &&job["preparationStages"]["first"]["status"]=="completed"&&job["preparationStages"]["first"]["reviewRequired"]==true
        &&state["status"]=="completed"&&(1..=4).contains(&rows(state,"chunks").len())
        &&rows(state,"chunks").iter().all(|chunk|chunk["result"].is_object()&&(1..=2).contains(&rows(chunk,"attempts").len())
            &&rows(chunk,"attempts").iter().all(|attempt|attempt["status"]=="completed"))
}

pub(crate) fn recover_in(d:&mut Value,run:&str,expected_plan:&str,profile:&Value,at:&str)->ApiResult<Value> {
    let replay=!row(d,"jobs",run)?["completedRecovery"].is_null();
    let outcome=recover_in_unlogged(d,run,expected_plan,profile,at)?;
    if !replay {audit(d,"preparation.completed_result_recovered",run);}
    Ok(outcome)
}

// The storage final scope owns an exact insert-only audit append alongside the
// reducer delta. Omitted audit bodies never become a partial history snapshot.
pub(crate) fn recover_in_unlogged(d:&mut Value,run:&str,expected_plan:&str,profile:&Value,at:&str)->ApiResult<Value> {
    let job=row(d,"jobs",run)?.clone();
    let state=&job["preparationStages"]["reviewChunks"];
    if !digest(&json!(expected_plan))||state["planDigest"]!=expected_plan{return Err(conflict("RECOVERY_PLAN_CHANGED"));}
    owned(d,&job)?;
    validate_profile(profile,bridge_account(&active_binding(d)?)?)?;
    if state["profile"]!=*profile{return Err(conflict("RECOVERY_PROFILE_CHANGED"));}
    let receipt=&job["completedRecovery"];
    if !receipt.is_null() {
        if receipt["version"]!=1||receipt["planDigest"]!=expected_plan||receipt["profileSha256"]!=profile["profileSha256"]
            ||job["status"]!="completed"||!job["prepareOutcome"].is_object()
            ||receipt["outcomeDigest"]!=hash(&job["prepareOutcome"]){return Err(conflict("RECOVERY_RECEIPT_CHANGED"));}
        // Replay reports a prior committed outcome, never readmits its proposals.
        return Ok(job["prepareOutcome"].clone());
    }
    if !completed_candidate(&job){return Err(conflict("RECOVERY_COMPLETED_REVIEW_REQUIRED"));}
    if crate::preparation_workers::pending_conflict(d,&job,false){return Err(conflict("RECOVERY_BUSY"));}
    check_pending_reservations(d,&job)?;
    current(d,&job)?;
    let mut usage_check=state.clone();update_usage(&mut usage_check);
    if usage_check["usage"]!=state["usage"]||state["usage"]["reservedUnconfirmedWebCalls"]!=0
        ||state["usage"]["unknownAttemptCount"]!=0{return Err(conflict("RECOVERY_UNSETTLED_USAGE"));}
    // Revalidate each immutable completed return against its captured request.
    // This synthetic prior exists only in memory and never changes reservations.
    for index in 0..rows(state,"chunks").len() {
        let mut prior=job.clone();let prior_state=&mut prior["preparationStages"]["reviewChunks"];
        let attempt=prior_state["chunks"][index]["attempts"].as_array_mut().unwrap().last_mut().unwrap();
        attempt["status"]=json!("running");attempt["observedWebCalls"]=Value::Null;
        for key in ["finishedAt","resultDigest"]{attempt.as_object_mut().unwrap().remove(key);}
        prior_state["chunks"][index]["result"]=Value::Null;prior_state["status"]=json!("running");update_usage(prior_state);
        validate_settlement(d,&prior,&job)?;
    }
    let result=aggregate(&job)?;
    let purpose=required(&job,"purpose")?;
    let ids=job["prepareBundle"]["itemIds"].as_array().filter(|ids|!ids.is_empty()).ok_or_else(||bad("Recovery recipients missing"))?;
    let bound_ids:BTreeSet<_>=ids.iter().filter_map(|id|id.as_str().map(str::to_owned)).collect();
    if bound_ids.len()!=ids.len()||bound_ids!=selected(&job["prepareBundle"]["request"]).map_err(bad)? {
        return Err(conflict("RECOVERY_RECIPIENTS_CHANGED"));
    }
    let pending=pending_recipients(&job,&bound_ids);
    for id in ids.iter().filter(|id|id.as_str().is_some_and(|id|pending.contains(id))) {
        let item=row(d,"items",id.as_str().ok_or_else(||bad("Recovery recipient invalid"))?)?;
        if !item["autoPreparation"]["humanOverrideAt"].is_null()||item["autoPreparation"]["requiresReview"]==true {
            return Err(conflict("RECOVERY_OPERATOR_HOLD"));
        }
        // Legacy engine jobs may lack a scope reservation. A newer protected
        // proposal still wins over recovery of their historical paid result.
        // Only pending recipients are checked; committed replay returned above.
        if list(d,"proposals").iter().any(|proposal|proposal["itemId"]==item["id"]
            &&matches!(proposal["status"].as_str(),Some("approved"|"dispatching"|"unknown"))) {
            return Err(conflict("RECOVERY_PROTECTED_PROPOSAL"));
        }
        if purpose!="engine_prepare" {
            let key=if purpose=="auto_prepare"{"autoPreparation"}else{"autoRevalidation"};
            if item[key]["jobId"]!=run{return Err(conflict("RECOVERY_OWNERSHIP_CHANGED"));}
        }
    }
    // All mutations below are one final-settlement transaction. Failure rolls back
    // ownership, admission, archives and finalization together.
    row_mut(d,"jobs",run)?["status"]=json!("running");
    if purpose!="engine_prepare" {
        for id in ids.iter().filter(|id|id.as_str().is_some_and(|id|pending.contains(id))) {
            let item=row_mut(d,"items",id.as_str().unwrap())?;
            let key=if purpose=="auto_prepare"{"autoPreparation"}else{"autoRevalidation"};
            item[key]["status"]=json!("running");
            if purpose=="auto_prepare"{item[key]["reviewResumeRequired"]=json!(false);}
        }
    }
    let outcome=if purpose=="engine_prepare" {
        engine_prepare::admit_completed_review(d,run,&result)?
    }else{
        let outcome=auto_prepare::complete(d,run,&result,chrono::DateTime::parse_from_rfc3339(at).map_err(|_|bad("Invalid recovery time"))?.timestamp())?;
        if !matches!(outcome["status"].as_str(),Some("prepared"|"needs_attention"))
            ||matches!(outcome["admission"]["status"].as_str(),Some("stale"|"rejected"))
            ||rows(&outcome["admission"],"candidates").iter().any(|c|c["status"]!="review")
            ||rows(&outcome,"items").iter().any(|item|!matches!(item["status"].as_str(),Some("prepared"|"needs_attention"))) {
            return Err(conflict("RECOVERY_ADMISSION_HELD"));
        }
        super::record_review(d,run,Ok(&result),at)?;outcome
    };
    let saved=row_mut(d,"jobs",run)?;
    saved["status"]=json!("completed");saved["result"]=outcome.clone();saved["finishedAt"]=json!(at);saved["error"]=Value::Null;
    saved["completedRecovery"]=json!({"version":1,"planDigest":expected_plan,"profileSha256":profile["profileSha256"],
        "outcomeDigest":hash(&outcome),"completedAt":at,"previousStatus":job["status"],"previousError":job["error"],"previousFinishedAt":job["finishedAt"]});
    Ok(outcome)
}

pub(crate) async fn recover_completed_job(app:&App,run:&str,expected_plan:&str)->ApiResult<Value>{
    if !digest(&json!(expected_plan)){return Err(bad("Recovery requires expectedPlanDigest"));}
    let job=app.db.read_job(run).await?.ok_or_else(||conflict("Recovery job missing"))?;
    if !completed_candidate(&job)&&job["completedRecovery"].is_null(){return Err(conflict("RECOVERY_COMPLETED_REVIEW_REQUIRED"));}
    // review_profile inspects local adapter/runtime hashes only. This function
    // never calls run/reserve, triage, triage_review, or any model/provider action.
    let profile=app.bridge("assistant",json!({"purpose":"review_profile"})).await?;
    let (outcome,committed)=app.recover_completed_preparation(run,expected_plan,&profile,&now()).await?;
    if committed {app.preparation_wake.notify_one();}
    Ok(outcome)
}

pub(crate) async fn recover_completed(State(app):State<App>,Path(run):Path<String>,Json(body):Json<Value>)->ApiResult<Json<Value>>{
    if body.as_object().is_none_or(|o|o.len()!=1)||!digest(&body["expectedPlanDigest"]){return Err(bad("Recovery requires expectedPlanDigest only"));}
    recover_completed_job(&app,&run,body["expectedPlanDigest"].as_str().unwrap()).await.map(Json)
}

/// Schema 2 states explicitly that several model invocations were assembled.
/// Their individual input/instruction/image provenance is never rewritten.
pub(crate) fn composite_metadata(result:&Value)->Result<Value,&'static str>{
    let value=&result["runMetadata"];let invalid="Invalid composite review provenance";
    let uncapped=value.get("webCallLimit")==Some(&Value::Null);
    if value.get("webCallLimit").is_some()&&!uncapped{return Err(invalid);}
    if value["schemaVersion"]!=2||value["kind"]!="durable_review_chunks"
        ||["planDigest","bundleDigest","requestDigest"].iter().any(|k|!digest(&value[*k]))
        ||(!uncapped&&value["chargedWebCalls"].as_u64().is_none_or(|n|n>WEB_BUDGET))
        ||value["inputBytes"].as_u64().is_none_or(|n|n>MAX_INPUT_BYTES){return Err(invalid);}
    let profile=&value["profile"];
    validate_profile(profile,profile["account"].as_str().ok_or(invalid)?).map_err(|_|invalid)?;
    if uncapped&&(profile["version"]!=3||value["unknownAttemptCount"].as_u64().is_none()){return Err(invalid);}
    let expected: BTreeSet<_>=rows(result,"assessments").iter().filter_map(|v|v["itemId"].as_str().map(str::to_owned)).collect();
    if expected.is_empty()||expected.len()!=rows(result,"assessments").len(){return Err(invalid);}
    let chunks=value["chunks"].as_array().filter(|c|!c.is_empty()&&c.len()<=4).ok_or(invalid)?;
    let mut seen=BTreeSet::new();let mut chunk_ids=BTreeSet::new();let mut clean_chunks=vec![];let mut calls=0;
    for chunk in chunks{
        let cid=chunk["id"].as_str().filter(|s|!s.is_empty()).ok_or(invalid)?;
        if !chunk_ids.insert(cid)||!digest(&chunk["resultDigest"]){return Err(invalid);}
        let ids=chunk["itemIds"].as_array().filter(|v|!v.is_empty()&&v.len()<=25).ok_or(invalid)?;
        for id in ids {let id=id.as_str().ok_or(invalid)?;if !expected.contains(id)||!seen.insert(id.to_owned()){return Err(invalid);}}
        if chunk["metadata"]["schemaVersion"]!=1{return Err(invalid);}
        let chunk_result=json!({"assessments":rows(result,"assessments").iter().filter(|v|ids.contains(&v["itemId"])).collect::<Vec<_>>(),
            "proposals":rows(result,"proposals").iter().filter(|v|ids.contains(&v["itemId"])).collect::<Vec<_>>(),"runMetadata":chunk["metadata"]});
        let m=prepare_bundle::generation_metadata(&chunk_result)?.ok_or(invalid)?;
        let contract=&m["reviewChunk"];
        if uncapped!=(contract["version"]==2)||uncapped!=(m["research"].get("webCallLimit")==Some(&Value::Null)){return Err(invalid);}
        if m["model"]!=profile["model"]||m["reasoningEffort"]!=profile["reasoningEffort"]||m["promptVersion"]!=profile["promptVersion"]
            ||m["cliSha256"]!=profile["cliSha256"]||m["research"]["toolsProfileSha256"]!=profile["toolsProfileSha256"]
            ||contract["profileSha256"]!=profile["profileSha256"]||contract["chunkId"]!=chunk["id"]||contract["attemptId"]!=chunk["attemptId"] {return Err(invalid);}
        calls+=m["research"]["webCalls"].as_u64().ok_or(invalid)?;
        clean_chunks.push(json!({"id":chunk["id"],"itemIds":ids,"attemptId":chunk["attemptId"],"resultDigest":chunk["resultDigest"],"metadata":m}));
    }
    if seen!=expected||(!uncapped&&calls>value["chargedWebCalls"].as_u64().unwrap()){return Err(invalid);}
    if uncapped&&(value["observedCompletedWebCalls"]!=calls||value["actualTotalWebCallsKnown"]!=(value["unknownAttemptCount"]==0)
        ||(value["unknownAttemptCount"]==0&&value["chargedWebCalls"]!=calls)
        ||(value["unknownAttemptCount"]!=0&&value.get("chargedWebCalls")!=Some(&Value::Null))){return Err(invalid);}
    let mut clean=json!({"schemaVersion":2,"kind":"durable_review_chunks","planDigest":value["planDigest"],"profile":profile,
        "bundleDigest":value["bundleDigest"],"requestDigest":value["requestDigest"],"chargedWebCalls":value["chargedWebCalls"],
        "observedCompletedWebCalls":calls,"inputBytes":value["inputBytes"],"chunks":clean_chunks});
    if uncapped{for key in ["webCallLimit","unknownAttemptCount","actualTotalWebCallsKnown"]{clean[key]=value[key].clone();}}
    Ok(clean)
}
pub(crate) fn composite_images(metadata:&Value,bundle:&Value)->Result<(),&'static str>{
    if metadata["bundleDigest"]!=bundle["digest"]{return Err("Composite review bundle changed");}
    for chunk in rows(metadata,"chunks") {
        let mut b=bundle.clone();let ids=chunk["itemIds"].as_array().ok_or("Invalid chunk recipients")?;
        b["request"]["items"]=json!(rows(&bundle["request"],"items").iter().filter(|v|ids.contains(&v["id"])).collect::<Vec<_>>());
        if bundle["request"]["visualSelection"]["postImages"].is_array(){
            b["request"]["visualSelection"]=subset(&bundle["request"],&json!(ids))["visualSelection"].clone();
        }
        if rows(&b["request"],"items").len()!=ids.len(){return Err("Chunk image recipients differ");}
        prepare_bundle::validate_image_evidence_binding(&chunk["metadata"],&b)?;
    }Ok(())
}
pub(crate) fn chunk_contract(value:&Value)->Result<Value,&'static str>{
    let invalid="Invalid review chunk contract";
    if !matches!(value["version"].as_u64(),Some(1|2))||value.as_object().is_none_or(|o|o.len()!=6)
        ||["attemptId","chunkId"].iter().any(|k|value[*k].as_str().is_none_or(|s|s.is_empty()||s.len()>128||!s.bytes().all(|c|c.is_ascii_alphanumeric()||b"_-".contains(&c))))
        ||["profileSha256","requestSha256"].iter().any(|k|!digest(&value[*k]))
        ||(if value["version"]==2{value.get("maxWebCalls")!=Some(&Value::Null)}else{value["maxWebCalls"].as_u64().is_none_or(|n|n==0||n>WEB_BUDGET)}){return Err(invalid);}Ok(value.clone())
}
pub(crate) fn research_projection(metadata:&Value)->Result<Value,&'static str>{
    let mut sources=vec![];let mut evidence_holds=vec![];let mut calls=0u64;let mut earliest=None;
    for chunk in rows(metadata,"chunks") {
        let research=&chunk["metadata"]["research"];
        calls+=research["webCalls"].as_u64().ok_or("Missing chunk research usage")?;
        let at=research["completedAt"].as_str().ok_or("Missing chunk research time")?;
        let stamp=chrono::DateTime::parse_from_rfc3339(at).map_err(|_|"Invalid chunk research time")?;
        earliest=Some(earliest.map_or(stamp,|old:chrono::DateTime<chrono::FixedOffset>|old.min(stamp)));
        sources.extend_from_slice(rows(research,"sources"));
        evidence_holds.extend_from_slice(rows(research,"evidenceHolds"));
    }
    let uncapped=metadata.get("webCallLimit")==Some(&Value::Null);
    if (!uncapped&&(sources.len()>30||evidence_holds.len()>30||calls>WEB_BUDGET))
        ||json!([&sources,&evidence_holds]).to_string().len()>2*1024*1024{return Err("Composite research exceeds original aggregate limits");}
    let mut projection=json!({"version":2,"kind":"durable_review_chunks","status":if sources.is_empty(){"no_sources"}else{"completed"},
        "trust":"source_only","webCalls":calls,"completedAt":earliest.ok_or("Missing completed review chunks")?.to_rfc3339(),
        "sources":sources,"planDigest":metadata["planDigest"]});
    if uncapped{projection["webCallLimit"]=Value::Null;}
    if !evidence_holds.is_empty(){projection["evidenceHolds"]=json!(evidence_holds);}
    Ok(projection)
}

#[cfg(test)]
pub(crate) use tests::exercise_stale_settlement_storage;
#[cfg(test)]
pub(crate) use tests::exercise_completed_recovery_storage;
#[cfg(test)]
pub(crate) use tests::r9_final_fixture;
#[cfg(test)]
pub(crate) use tests::r9_final_workload_fixture;
#[cfg(test)]
pub(crate) use tests::r9_final_grouped_fixture;
#[cfg(test)]
pub(crate) use tests::r9_final_singleton_fixture;
#[cfg(test)]
#[path="preparation_review_chunks_tests.rs"]
mod tests;
