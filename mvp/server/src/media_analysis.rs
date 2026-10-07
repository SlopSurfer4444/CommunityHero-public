//! Durable per-company, verified-file ASR exclusion reducer.
//! Invoke only inside the existing writer transaction. This module does no IO,
//! inference, scheduling, credential lookup or clock reads. CAS outputs must be
//! written and independently verified BEFORE capture; verificationSha256 binds
//! the caller's verified immutable closure receipt, not a boolean success flag.
//! Analysis survives alias/catalog drift. UNKNOWN is never a retry permission.
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;

pub(crate) const VERSION: u64 = 1;
pub(crate) fn hash(value: &Value) -> String {
    format!("{:x}", Sha256::digest(value.to_string().as_bytes()))
}
fn text<'a>(v: &'a Value, k: &str) -> Result<&'a str, String> {
    v[k].as_str().filter(|s| !s.trim().is_empty()).ok_or_else(|| format!("media_analysis_missing_{k}"))
}
fn number(v: &Value, k: &str) -> Result<u64, String> {
    v[k].as_u64().ok_or_else(|| format!("media_analysis_invalid_{k}"))
}
fn digest(v: &Value, k: &str) -> Result<(), String> {
    let s = text(v, k)?;
    if s.len() != 64 || !s.bytes().all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c)) {
        return Err(format!("media_analysis_invalid_{k}"));
    }
    Ok(())
}
fn reference(v: &Value) -> Result<(), String> {
    if v.as_object().is_none_or(|o| o.len() != 2) { return Err("media_analysis_invalid_cas_reference".into()); }
    digest(v, "sha256")?;
    number(v, "bytes")?;
    Ok(())
}
fn array<'a>(v: &'a Value, k: &str) -> Result<&'a Vec<Value>, String> {
    v[k].as_array().ok_or_else(|| format!("media_analysis_invalid_{k}"))
}
fn request_key(request: &Value) -> Result<Value, String> {
    text(request, "companyId")?;
    if request["stage"] != "asr" { return Err("media_analysis_unsupported_stage".into()); }
    let file = &request["verifiedFile"];
    digest(file, "sha256")?; digest(file, "receiptSha256")?; digest(file, "probeSha256")?;
    if number(file, "bytes")? == 0 { return Err("media_analysis_empty_input".into()); }
    Ok(json!({"companyId":request["companyId"],"sha256":file["sha256"],"stage":"asr"}))
}
fn initialize(ledger: &mut Value, request: &Value) -> Result<(), String> {
    request_key(request)?;
    if ledger.is_null() {
        *ledger = json!({"schemaVersion":VERSION,"companyId":request["companyId"],"analyses":[],"applicability":[]});
    }
    if ledger["schemaVersion"] != VERSION || ledger["companyId"] != request["companyId"] {
        return Err("media_analysis_company_or_version_mismatch".into());
    }
    array(ledger, "analyses")?; array(ledger, "applicability")?;
    Ok(())
}
fn locate(ledger: &Value, request: &Value) -> Result<Option<usize>, String> {
    let key = request_key(request)?;
    if ledger.is_null() { return Ok(None); }
    if ledger["schemaVersion"] != VERSION || ledger["companyId"] != request["companyId"] {
        return Err("media_analysis_company_or_version_mismatch".into());
    }
    let matches: Vec<_> = array(ledger,"analyses")?.iter().enumerate().filter(|(_,a)|a["key"]==key).collect();
    if matches.len() > 1 { return Err("media_analysis_duplicate_exclusion".into()); }
    let Some((index, analysis)) = matches.first() else { return Ok(None); };
    // New acquisition receipts may differ for identical bytes; an incompatible
    // size or probe interpretation must never pass as equivalent content.
    if analysis["verifiedFile"]["bytes"] != request["verifiedFile"]["bytes"]
        || analysis["verifiedFile"]["probeSha256"] != request["verifiedFile"]["probeSha256"] {
        return Err("media_analysis_asset_conflict".into());
    }
    Ok(Some(*index))
}
fn current<'a>(analysis: &'a Value) -> Result<&'a Value, String> {
    let attempts = array(analysis,"attempts")?;
    attempts.last().ok_or_else(||"media_analysis_attempt_missing".into())
}
fn fence(analysis: &Value, request: &Value) -> Result<usize, String> {
    let attempts = array(analysis,"attempts")?;
    let attempt = current(analysis)?;
    for k in ["attemptId","owner","epoch","specSha256","manifestKey"] {
        if request[k].is_null() || request[k] != attempt[k] { return Err("media_analysis_fence_changed".into()); }
    }
    Ok(attempts.len()-1)
}
fn plan(request: &Value) -> Result<(), String> {
    let duration = number(request,"durationMs")?;
    if duration == 0 { return Err("media_analysis_duration_missing".into()); }
    if request["noAudio"]==true {
        if !array(request,"segments")?.is_empty() {return Err("media_analysis_no_audio_plan_invalid".into());}
        digest(request,"noAudioVerificationSha256")?;
        return Ok(());
    }
    let mut end = 0;
    for (i,s) in array(request,"segments")?.iter().enumerate() {
        if number(s,"index")? != i as u64 || number(s,"startMs")? != end { return Err("media_analysis_segment_plan_gap".into()); }
        end = number(s,"endMs")?;
        if end <= number(s,"startMs")? || end > duration { return Err("media_analysis_segment_plan_invalid".into()); }
    }
    if end != duration { return Err("media_analysis_segment_plan_incomplete".into()); }
    Ok(())
}
fn new_attempt(request: &Value) -> Result<Value, String> {
    text(request,"attemptId")?; text(request,"owner")?; text(request,"manifestKey")?;
    digest(request,"specSha256")?;
    if number(request,"epoch")? == 0 { return Err("media_analysis_epoch_missing".into()); }
    plan(request)?;
    Ok(json!({"attemptId":request["attemptId"],"owner":request["owner"],"epoch":request["epoch"],
        "specSha256":request["specSha256"],"manifestKey":request["manifestKey"],
        "durationMs":request["durationMs"],"plan":request["segments"],"originalRequest":request,
        "durationToleranceMs":request["durationToleranceMs"].as_u64().unwrap_or(0),
        "noAudio":request["noAudio"]==true,"noAudioVerificationSha256":request["noAudioVerificationSha256"],"segments":[],
        "status":"owned","dispatched":[],"failure":null,"reconciliation":null,"recoveryAuthority":null}))
}
/// No mutation is visible on validation failure, including initialization.
fn change(ledger: &mut Value, operation: impl FnOnce(&mut Value)->Result<Value,String>) -> Result<Value,String> {
    let mut next = ledger.clone();
    let outcome = operation(&mut next)?;
    validate_transition(ledger,&next)?;
    *ledger = next;
    Ok(outcome)
}
fn view(analysis: &Value, request: &Value) -> Result<Value,String> {
    digest(request,"specSha256")?;
    if array(analysis,"attempts")?.is_empty() && !analysis["adoption"].is_null() {
        let result=array(analysis,"results")?.iter().find(|r|r["specSha256"]==request["specSha256"]);
        if let Some(result)=result {validate_result(analysis,result)?;}
        return Ok(json!({"disposition":if result.is_some(){"reuse"}else{"incompatible"},"key":analysis["key"],"verifiedFile":analysis["verifiedFile"],"analysis":analysis,"attempt":null,"result":result}));
    }
    let attempt = current(analysis)?;
    let state = text(attempt,"status")?;
    // Existing or uncertain work excludes dispatch BEFORE compatibility is
    // considered. A different configured model is never a new reservation lane.
    let result = array(analysis,"results")?.iter().rev().find(|r|r["specSha256"]==request["specSha256"]);
    if let Some(result)=result {validate_result(analysis,result)?;}
    let disposition = match state {
        "owned" => "owned", "unknown" => "unknown", "held" => "held",
        "completed" if result.is_some() => "reuse", "completed" => "incompatible",
        _ => return Err("media_analysis_status_invalid".into())
    };
    Ok(json!({"disposition":disposition,"key":analysis["key"],"verifiedFile":analysis["verifiedFile"],
        "attempt":attempt,"analysis":analysis,"result":if disposition=="reuse" {result.cloned()} else {None}}))
}
pub(crate) fn reserve(ledger: &mut Value, request: &Value) -> Result<Value,String> {
    change(ledger,|next| {
        initialize(next,request)?;
        if let Some(index)=locate(next,request)? { return view(&next["analyses"][index],request); }
        let attempt = new_attempt(request)?;
        if array(next,"analyses")?.iter().any(|a|array(a,"attempts").is_ok_and(|v|v.iter().any(|t|t["attemptId"]==request["attemptId"] || t["manifestKey"]==request["manifestKey"]))) {
            return Err("media_analysis_attempt_or_manifest_reused".into());
        }
        let analysis=json!({"key":request_key(request)?,"verifiedFile":request["verifiedFile"],"attempts":[attempt],"results":[]});
        next["analyses"].as_array_mut().unwrap().push(analysis.clone());
        let mut result=view(&analysis,request)?; result["disposition"]=json!("reserved"); Ok(result)
    })
}
pub(crate) fn read_result(ledger: &Value, request: &Value) -> Result<Value,String> {
    if let Some(index)=locate(ledger,request)? { view(&ledger["analyses"][index],request) }
    else { Ok(json!({"disposition":"absent"})) }
}
fn validate_result(analysis: &Value, result: &Value) -> Result<(),String> {
    digest(result,"resultSha256")?;
    let mut payload=result.clone(); payload.as_object_mut().ok_or("media_analysis_result_invalid")?.remove("resultSha256");
    if hash(&payload)!=result["resultSha256"].as_str().unwrap() {return Err("media_analysis_result_hash_mismatch".into());}
    reference(&result["manifest"])?; reference(&result["normalizedOutput"])?; digest(result,"verificationSha256")?;
    if result["sourceKind"]=="legacy_adopted" {
        if result["adoption"]!=analysis["adoption"] || !result["rawOutput"].is_null() || !result["attemptId"].is_null() || !result["owner"].is_null() {
            return Err("media_analysis_legacy_provenance_invalid".into());
        }
        validate_adoption(&analysis["adoption"])?;
        if result["specSha256"]!=analysis["adoption"]["compatibility"]["specSha256"] || result["coverage"]!=analysis["adoption"]["coverage"]
            || result["outcome"]!=adoption_outcome(&analysis["adoption"])? {
            return Err("media_analysis_legacy_compatibility_invalid".into());
        }
        if result["companyId"]!=analysis["key"]["companyId"] || result["verifiedFile"]!=analysis["verifiedFile"] {return Err("media_analysis_result_asset_mismatch".into());}
        return Ok(());
    }
    let attempt=array(analysis,"attempts")?.iter().find(|a|a["attemptId"]==result["attemptId"]).ok_or("media_analysis_result_attempt_missing")?;
    for k in ["owner","epoch","specSha256","manifestKey","originalRequest","segments"] {
        if result[k]!=attempt[k] {return Err("media_analysis_result_binding_mismatch".into());}
    }
    if result["companyId"]!=analysis["key"]["companyId"] || result["verifiedFile"]!=analysis["verifiedFile"] {
        return Err("media_analysis_result_asset_mismatch".into());
    }
    if array(attempt,"segments")?.len()!=array(attempt,"plan")?.len()
        || result["coverage"]["durationMs"]!=attempt["durationMs"]
        || result["coverage"]["kind"]!=if attempt["noAudio"]==true {json!("no_audio_stream")}else{json!("full_audio")}
        || !matches!(text(result,"outcome")?,"transcript"|"no_speech"|"no_audio")
        || (result["outcome"]=="no_audio")!=(attempt["noAudio"]==true) {
        return Err("media_analysis_full_coverage_invalid".into());
    }
    Ok(())
}
fn validate_adoption(adoption:&Value)->Result<(),String> {
    digest(adoption,"receiptSha256")?;digest(adoption,"verificationSha256")?;
    for k in ["entryId","versionId"] {text(&adoption["originalKnowledge"],k)?;}
    digest(&adoption["originalKnowledge"],"sha256")?;
    digest(&adoption["compatibility"],"specSha256")?;digest(&adoption["compatibility"],"authorityReceiptSha256")?;
    if adoption["compatibility"]["policy"]!="verified_legacy_normalized_full_audio"
        || !matches!(text(&adoption["coverage"],"kind")?,"full_audio"|"no_audio_stream") || number(&adoption["coverage"],"durationMs")?==0 {
        return Err("media_analysis_legacy_adoption_invalid".into());
    }
    adoption_outcome(adoption)?;
    Ok(())
}
fn adoption_outcome(adoption:&Value)->Result<Value,String> {
    let outcome=adoption["outcome"].as_str().unwrap_or("transcript");
    if !matches!(outcome,"transcript"|"no_speech"|"no_audio") || (outcome=="no_audio")!=(adoption["coverage"]["kind"]=="no_audio_stream") {
        return Err("media_analysis_legacy_outcome_invalid".into());
    }
    Ok(json!(outcome))
}
/// Bootstrap a retained, verified complete historical transcript. No historical
/// dispatch ID, owner, raw output or model digest is invented. A specifically
/// admitted compatibility receipt binds the current spec separately.
pub(crate) fn adopt_completed(ledger:&mut Value,request:&Value)->Result<Value,String> {
    change(ledger,|next|{
        initialize(next,request)?;
        if let Some(index)=locate(next,request)? {return view(&next["analyses"][index],request);}
        validate_adoption(&request["adoption"])?;
        reference(&request["normalizedOutput"])?;reference(&request["manifest"])?;
        if request["specSha256"]!=request["adoption"]["compatibility"]["specSha256"] {return Err("media_analysis_legacy_compatibility_invalid".into());}
        let mut result=json!({"sourceKind":"legacy_adopted","adoption":request["adoption"],"companyId":request["companyId"],"verifiedFile":request["verifiedFile"],"specSha256":request["specSha256"],
            "manifest":request["manifest"],"normalizedOutput":request["normalizedOutput"],"rawOutput":null,"coverage":request["adoption"]["coverage"],"outcome":adoption_outcome(&request["adoption"])? ,"verificationSha256":request["adoption"]["verificationSha256"]});
        result["resultSha256"]=json!(hash(&result));
        let analysis=json!({"key":request_key(request)?,"verifiedFile":request["verifiedFile"],"attempts":[],"results":[result],"adoption":request["adoption"]});
        next["analyses"].as_array_mut().unwrap().push(analysis.clone());view(&analysis,request)
    })
}

/// Every asset is a separate row in the existing durable jobs collection. No
/// global JSON ledger job or new unsupported top-level persistence collection.
pub(crate) fn ledger_from_workspace(workspace: &Value) -> Result<Value,String> {
    text(workspace,"account")?;
    let mut analyses=Vec::new();
    for job in array(workspace,"jobs")? {
        if job["kind"]!="media_analysis" {continue;}
        if job["account"]!=workspace["account"] || job["id"]!=job_id(&job["analysis"]["key"]) {
            return Err("media_analysis_job_identity_mismatch".into());
        }
        analyses.push(job["analysis"].clone());
    }
    let ledger=json!({"schemaVersion":VERSION,"companyId":workspace["account"],"analyses":analyses,"applicability":[]});
    validate_transition(&Value::Null,&ledger)?;
    Ok(ledger)
}
fn job_id(key:&Value)->Value {json!(format!("media-analysis-{}",hash(key)))}
pub(crate) fn put_ledger(workspace:&mut Value,ledger:&Value)->Result<(),String> {
    let before=ledger_from_workspace(workspace)?;
    validate_transition(&before,ledger)?;
    if ledger["companyId"]!=workspace["account"] {return Err("media_analysis_company_or_version_mismatch".into());}
    let mut jobs=array(workspace,"jobs")?.clone();
    for analysis in array(ledger,"analyses")? {
        let id=job_id(&analysis["key"]);
        if let Some(row)=jobs.iter_mut().find(|j|j["id"]==id) {
            if row["kind"]!="media_analysis" {return Err("media_analysis_job_collision".into());}
            row["analysis"]=analysis.clone();
        } else {
            // Non-runnable carrier. Nested attempts retain actual uncertainty.
            jobs.push(json!({"id":id,"kind":"media_analysis","status":"ledger","account":workspace["account"],"analysis":analysis}));
        }
    }
    workspace["jobs"]=json!(jobs);
    Ok(())
}
/// Global persistence guard for full snapshots and bounded job projections.
/// A projection may omit jobs on BOTH sides, or omit media rows on BOTH sides.
/// A present media row cannot be lost, retargeted or modified through another
/// writer. No byte/CAS/current-source verification is done inside this guard.
pub(crate) fn validate_workspace_change(before:&Value,after:&Value)->Result<(),String> {
    match (before.get("jobs"),after.get("jobs")) {
        (None,None)=>return Ok(()),
        (None,Some(_))|(Some(_),None)=>return Err("media_analysis_asymmetric_jobs_projection".into()),
        _=>{}
    }
    let old=array(before,"jobs")?;let new=array(after,"jobs")?;
    let media=|job:&Value|matches!(job["kind"].as_str(),Some("media_analysis"|"media_analysis_applicability"));
    if !old.iter().any(media) && !new.iter().any(media) {return Ok(());}
    if before["account"]!=after["account"] {return Err("media_analysis_company_or_version_mismatch".into());}
    text(before,"account")?;
    let mut identities=BTreeSet::new();
    for row in new {
        if !identities.insert(text(row,"id")?.to_owned()) {return Err("media_analysis_duplicate_job_identity".into());}
    }
    let old_ledger=ledger_from_workspace(before)?;let new_ledger=ledger_from_workspace(after)?;
    validate_transition(&old_ledger,&new_ledger)?;
    for row in new.iter().filter(|row|row["kind"]=="media_analysis") {
        if row["status"]!="ledger" {return Err("media_analysis_carrier_must_not_run".into());}
        if let Some(prior)=old.iter().find(|old|old["id"]==row["id"]) {
            if !same_carrier_fields(prior,row)? {return Err("media_analysis_carrier_identity_immutable".into());}
        }
    }
    let previous:Vec<&Value>=old.iter().filter(|row|row["kind"]=="media_analysis_applicability").collect();
    let current:Vec<&Value>=new.iter().filter(|row|row["kind"]=="media_analysis_applicability").collect();
    if !current.starts_with(&previous) {return Err("media_analysis_applicability_immutable".into());}
    for row in current {
        validate_applicability_row(row,text(after,"account")?)?;
        let pin=&row["result"]["proof"];
        if let Some(index)=locate(&new_ledger,pin)? {
            if !array(&new_ledger["analyses"][index],"results")?.iter().any(|result|result==&pin["result"]) {
                return Err("media_analysis_applicability_result_not_retained".into());
            }
        }
        if let Some(prior)=old.iter().find(|old|old["id"]==row["id"]) {
            if prior!=row {return Err("media_analysis_applicability_immutable".into());}
        }
    }
    Ok(())
}
// The ledger transition above validates analysis independently. Compare only
// the immutable carrier fields without cloning retained attempts/results just
// to remove them. Key presence and object-order-independent equality match the
// former clone/remove comparison, including unknown outer fields.
fn same_carrier_fields(prior:&Value,row:&Value)->Result<bool,String> {
    let prior=prior.as_object().ok_or("media_analysis_carrier_invalid")?;
    let row=row.as_object().ok_or("media_analysis_carrier_invalid")?;
    let fields=|object:&serde_json::Map<String,Value>|object.len()-usize::from(object.contains_key("analysis"));
    Ok(fields(prior)==fields(row) && prior.iter().filter(|(key,_)|key.as_str()!="analysis")
        .all(|(key,value)|row.get(key)==Some(value)))
}
fn validate_applicability_row(row:&Value,company:&str)->Result<(),String> {
    if row["kind"]!="media_analysis_applicability" || row["status"]!="completed" || row["account"]!=company
        || row["result"]["schemaVersion"]!=1 {return Err("media_analysis_applicability_invalid".into());}
    let pin=&row["result"]["proof"];
    if pin["schemaVersion"]!=1 || pin["kind"]!="verified_exact_file_analysis_reuse" || pin["stage"]!="asr"
        || pin["companyId"]!=company || pin["account"]!=company || row["id"]!=format!("media-applicability-{}",hash(pin)) {
        return Err("media_analysis_applicability_identity_invalid".into());
    }
    digest(pin,"resultSha256")?;digest(pin,"specSha256")?;
    request_key(pin)?;
    let target=&pin["target"];
    for key in ["postId","postKey"] {text(target,key)?;}
    digest(target,"sourceVersion")?;digest(target,"attachmentIdentity")?;
    number(target,"attachmentIndex")?;
    if number(target,"aliasRevision")?==0 {return Err("media_analysis_applicability_target_invalid".into());}
    let binding=&target["connectorBinding"];
    for key in ["id","workspaceId","accountId","connector","providerAccountId"] {text(binding,key)?;}
    if number(binding,"revision")?==0 {return Err("media_analysis_applicability_connector_invalid".into());}
    let result=&pin["result"];
    reference(&result["manifest"])?;reference(&result["normalizedOutput"])?;
    digest(result,"resultSha256")?;
    let mut payload=result.clone();payload.as_object_mut().ok_or("media_analysis_result_invalid")?.remove("resultSha256");
    if hash(&payload)!=text(result,"resultSha256")? || result["resultSha256"]!=pin["resultSha256"]
        || result["companyId"]!=company || result["specSha256"]!=pin["specSha256"]
        || result["verifiedFile"]["sha256"]!=pin["verifiedFile"]["sha256"]
        || result["verifiedFile"]["bytes"]!=pin["verifiedFile"]["bytes"]
        || result["verifiedFile"]["probeSha256"]!=pin["verifiedFile"]["probeSha256"] {
        return Err("media_analysis_applicability_result_invalid".into());
    }
    validate_standalone_completed_result(result)?;
    if !pin["donor"].is_null() {
        for key in ["entryId","versionId"] {text(&pin["donor"],key)?;}
        digest(&pin["donor"],"sha256")?;
        if pin["donor"]["scope"]["account"]!=company {return Err("media_analysis_applicability_donor_foreign".into());}
    }
    Ok(())
}
fn validate_standalone_completed_result(result:&Value)->Result<(),String> {
    digest(result,"verificationSha256")?;
    let binding=json!({"companyId":result["companyId"],"verifiedFile":result["verifiedFile"],"stage":"asr"});
    let key=request_key(&binding)?;
    if result["sourceKind"]=="legacy_adopted" {
        let analysis=json!({"key":key,"verifiedFile":result["verifiedFile"],"adoption":result["adoption"],"attempts":[]});
        return validate_result(&analysis,result);
    }
    let mut attempt=new_attempt(&result["originalRequest"])?;
    if request_key(&result["originalRequest"])?!=key || result["originalRequest"]["verifiedFile"]["bytes"]!=result["verifiedFile"]["bytes"]
        || result["originalRequest"]["verifiedFile"]["probeSha256"]!=result["verifiedFile"]["probeSha256"] {return Err("media_analysis_applicability_original_input_invalid".into());}
    attempt["segments"]=result["segments"].clone();
    for (i,segment) in array(result,"segments")?.iter().enumerate() {
        let planned=array(&attempt,"plan")?.get(i).ok_or("media_analysis_segment_out_of_plan")?;
        if segment["index"]!=i as u64 || segment["startMs"]!=planned["startMs"] || segment["endMs"]!=planned["endMs"]
            || number(segment,"actualDurationMs")?==0 || number(segment,"actualDurationMs")?.abs_diff(number(planned,"endMs")?-number(planned,"startMs")?)>number(&attempt,"durationToleranceMs")?
            || segment["specSha256"]!=attempt["specSha256"] || number(segment,"epoch")?==0 || number(segment,"epoch")?>number(&attempt,"epoch")? {
            return Err("media_analysis_applicability_segment_invalid".into());
        }
        for field in ["attemptId","owner","manifestKey"] {text(segment,field)?;}
        if segment["epoch"]==attempt["epoch"] {
            for field in ["attemptId","owner","manifestKey"] {if segment[field]!=attempt[field] {return Err("media_analysis_applicability_segment_owner_invalid".into());}}
        }
        reference(&segment["rawOutput"])?;reference(&segment["normalizedOutput"])?;digest(segment,"verificationSha256")?;
    }
    let analysis=json!({"key":key,"verifiedFile":result["verifiedFile"],"attempts":[attempt]});
    validate_result(&analysis,result)
}

#[cfg(test)]
pub(crate) fn test_workspace_fixture(account:&str)->Value {tests::workspace_fixture(account)}
/// Persist before dispatching one segment. An identical intent readback is
/// `already_dispatched`, never a permission to invoke inference again.
pub(crate) fn mark_dispatched(ledger: &mut Value, request: &Value) -> Result<Value,String> {
    change(ledger,|next| {
        let index=locate(next,request)?.ok_or("media_analysis_absent")?;
        let n=fence(&next["analyses"][index],request)?;
        let attempt=&mut next["analyses"][index]["attempts"][n];
        if attempt["status"]!="owned" {return Err("media_analysis_not_dispatchable".into());}
        let segment_index=number(request,"segmentIndex")?;
        let length=array(attempt,"plan")?.len();
        if segment_index >= length as u64 { return Err("media_analysis_segment_out_of_plan".into()); }
        if array(attempt,"dispatched")?.iter().any(|d|d["index"]==segment_index) {
            return Ok(json!({"disposition":"already_dispatched","attempt":attempt}));
        }
        if segment_index != array(attempt,"segments")?.len() as u64 { return Err("media_analysis_previous_segment_not_durable".into()); }
        let dispatch=json!({"index":segment_index,"attemptId":attempt["attemptId"],"owner":attempt["owner"],"epoch":attempt["epoch"],"specSha256":attempt["specSha256"],"manifestKey":attempt["manifestKey"]});
        attempt["dispatched"].as_array_mut().unwrap().push(dispatch);
        Ok(json!({"disposition":"dispatch_reserved","attempt":attempt}))
    })
}
pub(crate) fn commit_segment(ledger: &mut Value, request: &Value) -> Result<Value,String> {
    change(ledger,|next| {
        let index=locate(next,request)?.ok_or("media_analysis_absent")?;
        let n=fence(&next["analyses"][index],request)?;
        let attempt=&mut next["analyses"][index]["attempts"][n];
        if !matches!(text(attempt,"status")?,"owned"|"unknown") {return Err("media_analysis_not_captureable".into());}
        let s=&request["segment"]; let i=number(s,"index")?;
        reference(&s["rawOutput"])?; reference(&s["normalizedOutput"])?; digest(s,"verificationSha256")?;
        let expected=array(attempt,"plan")?.get(i as usize).ok_or("media_analysis_segment_out_of_plan")?;
        if s["startMs"]!=expected["startMs"] || s["endMs"]!=expected["endMs"] || number(s,"actualDurationMs")?==0
            || number(s,"actualDurationMs")?.abs_diff(number(expected,"endMs")?-number(expected,"startMs")?)>number(attempt,"durationToleranceMs")? {
            return Err("media_analysis_segment_coverage_mismatch".into());
        }
        if !array(attempt,"dispatched")?.iter().any(|d|d["index"]==i) { return Err("media_analysis_segment_not_dispatched".into()); }
        let mut captured=s.clone();
        for k in ["attemptId","owner","epoch","specSha256","manifestKey"] {captured[k]=attempt[k].clone();}
        if let Some(existing)=array(attempt,"segments")?.iter().find(|s|s["index"]==i) {
            if existing!=&captured { return Err("media_analysis_segment_immutable".into()); }
            return Ok(json!({"disposition":"already_captured","segment":existing}));
        }
        if i!=array(attempt,"segments")?.len() as u64 {return Err("media_analysis_segment_commit_out_of_order".into());}
        attempt["segments"].as_array_mut().unwrap().push(captured.clone());
        Ok(json!({"disposition":"captured","segment":captured}))
    })
}
pub(crate) fn commit_full_result(ledger: &mut Value, request: &Value) -> Result<Value,String> {
    change(ledger,|next| {
        let index=locate(next,request)?.ok_or("media_analysis_absent")?;
        let n=fence(&next["analyses"][index],request)?;
        let analysis=&mut next["analyses"][index];
        let attempt=&analysis["attempts"][n];
        if !matches!(text(attempt,"status")?,"owned"|"unknown"|"completed") {return Err("media_analysis_not_captureable".into());}
        if array(attempt,"segments")?.len()!=array(attempt,"plan")?.len() {return Err("media_analysis_full_coverage_missing".into());}
        let result=&request["result"];
        reference(&result["manifest"])?; reference(&result["normalizedOutput"])?; digest(result,"verificationSha256")?;
        let no_audio=attempt["noAudio"]==true;
        if result["coverage"]["kind"]!=if no_audio {json!("no_audio_stream")} else {json!("full_audio")}
            || result["coverage"]["durationMs"]!=attempt["durationMs"]
            || !matches!(text(result,"outcome")?,"transcript"|"no_speech"|"no_audio") {
            return Err("media_analysis_full_coverage_invalid".into());
        }
        if (result["outcome"]=="no_audio")!=no_audio {return Err("media_analysis_no_audio_probe_mismatch".into());}
        if no_audio && attempt["noAudioVerificationSha256"].is_null() {return Err("media_analysis_no_audio_proof_missing".into());}
        let mut captured=result.clone();
        for k in ["attemptId","owner","epoch","specSha256","manifestKey"] {captured[k]=attempt[k].clone();}
        captured["verifiedFile"]=analysis["verifiedFile"].clone();
        captured["companyId"]=analysis["key"]["companyId"].clone();
        captured["segments"]=attempt["segments"].clone();
        captured["originalRequest"]=attempt["originalRequest"].clone();
        captured.as_object_mut().unwrap().remove("resultSha256");
        captured["resultSha256"]=json!(hash(&captured));
        if let Some(existing)=array(analysis,"results")?.iter().find(|r|r["attemptId"]==request["attemptId"]) {
            if existing!=&captured {return Err("media_analysis_result_immutable".into());}
            return Ok(json!({"disposition":"already_completed","result":existing}));
        }
        analysis["results"].as_array_mut().unwrap().push(captured.clone());
        analysis["attempts"][n]["status"]=json!("completed");
        Ok(json!({"disposition":"completed","result":captured}))
    })
}
/// An uncertain return or a failure after any dispatch remains UNKNOWN. A
/// classified pre-dispatch failure is held, but still cannot auto-retry.
pub(crate) fn fail(ledger: &mut Value, request: &Value) -> Result<Value,String> {
    change(ledger,|next| {
        let index=locate(next,request)?.ok_or("media_analysis_absent")?;
        let n=fence(&next["analyses"][index],request)?;
        let attempt=&mut next["analyses"][index]["attempts"][n];
        text(request,"reason")?;
        if attempt["status"]=="completed" {return Ok(json!({"disposition":"completed_preserved"}));}
        let unknown=!array(attempt,"dispatched")?.is_empty() || request["uncertain"]==true || attempt["status"]=="unknown";
        attempt["status"]=json!(if unknown {"unknown"} else {"held"});
        if attempt["failure"].is_null() {attempt["failure"]=json!({"reason":request["reason"],"uncertain":unknown});}
        Ok(json!({"disposition":attempt["status"],"attempt":attempt}))
    })
}
/// Reconciliation does not run inference. Opening a new generation requires
/// exact exceptional authority AFTER cessation/output reconciliation. Timeout,
/// process absence and a new configuration never satisfy this reducer.
pub(crate) fn recover(ledger: &mut Value, request: &Value) -> Result<Value,String> {
    if request["action"]=="adopt_full_result" {return commit_full_result(ledger,request);}
    change(ledger,|next| {
        let index=locate(next,request)?.ok_or("media_analysis_absent")?;
        let n=fence(&next["analyses"][index],request)?;
        let analysis=&mut next["analyses"][index];
        match text(request,"action")? {
            "reconcile" => {
                let a=&mut analysis["attempts"][n];
                if !matches!(text(a,"status")?,"unknown"|"held") {return Err("media_analysis_reconcile_state_invalid".into());}
                digest(request,"cessationSha256")?; digest(request,"reconciliationSha256")?;
                let resolution=text(request,"resolution")?;
                if !matches!(resolution,"not_dispatched"|"output_missing_after_dispatch") || (resolution=="not_dispatched" && !array(a,"dispatched")?.is_empty()) {
                    return Err("media_analysis_reconciliation_invalid".into());
                }
                let receipt=json!({"cessationSha256":request["cessationSha256"],"reconciliationSha256":request["reconciliationSha256"],"resolution":resolution});
                if !a["reconciliation"].is_null() && a["reconciliation"]!=receipt {return Err("media_analysis_reconciliation_immutable".into());}
                a["reconciliation"]=receipt; a["status"]=json!("held");
                Ok(json!({"disposition":"reconciled_held","attempt":a}))
            },
            "new_attempt" => {
                let previous=&analysis["attempts"][n];
                if !matches!(text(previous,"status")?,"completed"|"held") || (previous["status"]=="held" && previous["reconciliation"].is_null()) {
                    return Err("media_analysis_unreconciled_owner".into());
                }
                let authority=&request["authority"];
                digest(authority,"receiptSha256")?; text(authority,"actor")?;
                if authority["companyId"]!=analysis["key"]["companyId"] || authority["fileSha256"]!=analysis["key"]["sha256"]
                    || authority["priorAttemptId"]!=previous["attemptId"] || authority["nextSpecSha256"]!=request["nextAttempt"]["specSha256"]
                    || !matches!(text(authority,"purpose")?,"missing_segments"|"upgrade") {return Err("media_analysis_recovery_authority_mismatch".into());}
                let mut a=new_attempt(&request["nextAttempt"])?;
                if request_key(&request["nextAttempt"])?!=analysis["key"] || request["nextAttempt"]["verifiedFile"]["bytes"]!=analysis["verifiedFile"]["bytes"]
                    || request["nextAttempt"]["verifiedFile"]["probeSha256"]!=analysis["verifiedFile"]["probeSha256"] {return Err("media_analysis_recovery_asset_changed".into());}
                if number(&a,"epoch")?!=number(previous,"epoch")?.checked_add(1).ok_or("media_analysis_epoch_exhausted")? {
                    return Err("media_analysis_recovery_epoch_invalid".into());
                }
                if array(analysis,"attempts")?.iter().any(|old|old["attemptId"]==a["attemptId"] || old["manifestKey"]==a["manifestKey"]) {return Err("media_analysis_attempt_or_manifest_reused".into());}
                if authority["purpose"]=="missing_segments" {
                    for k in ["specSha256","plan","durationMs","durationToleranceMs","noAudio","noAudioVerificationSha256"] {if a[k]!=previous[k] {return Err("media_analysis_recovery_spec_changed".into());}}
                    // Original segment ownership is retained; no inference replay.
                    a["segments"]=previous["segments"].clone();
                    a["dispatched"]=json!([]);
                }
                a["recoveryAuthority"]=authority.clone();
                analysis["attempts"].as_array_mut().unwrap().push(a.clone());
                Ok(json!({"disposition":"recovery_reserved","attempt":a}))
            }, _ => Err("media_analysis_recovery_action_invalid".into())
        }
    })
}

/// Narrow/full persistence boundary: forbid deletion, replacement, foreign
/// company records, loss of UNKNOWN, and edits to immutable intent/output.
pub(crate) fn validate_transition(before: &Value, after: &Value) -> Result<(),String> {
    if after.is_null() {if before.is_null(){return Ok(());}return Err("media_analysis_ledger_deleted".into());}
    if after["schemaVersion"]!=VERSION {return Err("media_analysis_version_invalid".into());}
    text(after,"companyId")?; array(after,"applicability")?;
    let all=array(after,"analyses")?; let mut keys=BTreeSet::new(); let mut ids=BTreeSet::new(); let mut manifests=BTreeSet::new();
    for a in all {
        if a["key"]["companyId"]!=after["companyId"] || a["key"]["stage"]!="asr" || a["key"]["sha256"]!=a["verifiedFile"]["sha256"] || !keys.insert(hash(&a["key"])) {
            return Err("media_analysis_invalid_exclusion".into());
        }
        let probe=json!({"companyId":after["companyId"],"verifiedFile":a["verifiedFile"],"stage":"asr"});
        if a["key"]!=request_key(&probe)? {return Err("media_analysis_noncanonical_exclusion_key".into());}
        for result in array(a,"results")? {validate_result(a,result)?;}
        let attempts=array(a,"attempts")?;
        if !a["adoption"].is_null() && !attempts.is_empty() {return Err("media_analysis_legacy_upgrade_not_admitted".into());}
        if attempts.is_empty() {
            validate_adoption(&a["adoption"])?;
            if array(a,"results")?.len()!=1 {return Err("media_analysis_legacy_output_missing".into());}
        }
        for (position,attempt) in attempts.iter().enumerate() {
            if !ids.insert(text(attempt,"attemptId")?.to_owned()) || !manifests.insert(text(attempt,"manifestKey")?.to_owned()) {
                return Err("media_analysis_duplicate_attempt_or_manifest".into());
            }
            if !matches!(text(attempt,"status")?,"owned"|"unknown"|"held"|"completed") {return Err("media_analysis_status_invalid".into());}
            if !attempt["reconciliation"].is_null() {validate_reconciliation(attempt)?;}
            let expected=new_attempt(&attempt["originalRequest"])?;
            if request_key(&attempt["originalRequest"])?!=a["key"] || attempt["originalRequest"]["verifiedFile"]["bytes"]!=a["verifiedFile"]["bytes"]
                || attempt["originalRequest"]["verifiedFile"]["probeSha256"]!=a["verifiedFile"]["probeSha256"] {return Err("media_analysis_attempt_asset_changed".into());}
            for k in ["attemptId","owner","epoch","specSha256","manifestKey","durationMs","plan","noAudio","noAudioVerificationSha256","durationToleranceMs"] {
                if attempt[k]!=expected[k] {return Err("media_analysis_corrupt_attempt_intent".into());}
            }
            let previous=position.checked_sub(1).map(|p|&attempts[p]);
            if let Some(prior)=previous {validate_successor(&a["key"],prior,attempt)?;}
            else if !attempt["recoveryAuthority"].is_null() {return Err("media_analysis_unbound_recovery_authority".into());}
            let mut dispatched=BTreeSet::new();
            for intent in array(attempt,"dispatched")? {
                let index=number(intent,"index")?;
                if index>=array(attempt,"plan")?.len() as u64 || index>array(attempt,"segments")?.len() as u64 || !dispatched.insert(index) {return Err("media_analysis_invalid_dispatch_intent".into());}
                for k in ["attemptId","owner","epoch","specSha256","manifestKey"] {if intent[k]!=attempt[k] {return Err("media_analysis_dispatch_binding_changed".into());}}
            }
            for (i,segment) in array(attempt,"segments")?.iter().enumerate() {
                let planned=array(attempt,"plan")?.get(i).ok_or("media_analysis_segment_out_of_plan")?;
                if segment["index"]!=i as u64 || segment["startMs"]!=planned["startMs"] || segment["endMs"]!=planned["endMs"]
                    || number(segment,"actualDurationMs")?==0 || number(segment,"actualDurationMs")?.abs_diff(number(planned,"endMs")?-number(planned,"startMs")?)>number(attempt,"durationToleranceMs")? {
                    return Err("media_analysis_segment_coverage_mismatch".into());
                }
                reference(&segment["rawOutput"])?;reference(&segment["normalizedOutput"])?;digest(segment,"verificationSha256")?;
                let retained=previous.is_some_and(|prior|attempt["recoveryAuthority"]["purpose"]=="missing_segments" && array(prior,"segments").is_ok_and(|old|old.get(i)==Some(segment)));
                if !retained {
                    if !dispatched.contains(&(i as u64)) {return Err("media_analysis_segment_not_dispatched".into());}
                    for k in ["attemptId","owner","epoch","specSha256","manifestKey"] {if segment[k]!=attempt[k] {return Err("media_analysis_segment_owner_changed".into());}}
                }
            }
            let results=array(a,"results")?.iter().filter(|r|r["attemptId"]==attempt["attemptId"]).count();
            if (attempt["status"]=="completed" && results!=1) || (attempt["status"]!="completed" && results!=0) {
                return Err("media_analysis_completed_output_missing".into());
            }
        }
    }
    if before.is_null() {return Ok(());}
    if before["schemaVersion"]!=after["schemaVersion"] || before["companyId"]!=after["companyId"] {return Err("media_analysis_identity_replaced".into());}
    let old=array(before,"analyses")?;
    if all.len()<old.len() {return Err("media_analysis_history_removed".into());}
    for (i,prior) in old.iter().enumerate() {
        let next=&all[i];
        for k in ["key","verifiedFile","adoption"] {if prior[k]!=next[k] {return Err("media_analysis_asset_immutable".into());}}
        let attempts=array(prior,"attempts")?; let later=array(next,"attempts")?;
        if later.len()<attempts.len() || !array(next,"results")?.starts_with(array(prior,"results")?) {return Err("media_analysis_history_removed".into());}
        for (j,p) in attempts.iter().enumerate() {
            let n=&later[j];
            if j+1<attempts.len() && p!=n {return Err("media_analysis_old_attempt_immutable".into());}
            for k in ["attemptId","owner","epoch","specSha256","manifestKey","durationMs","plan","originalRequest","noAudio","noAudioVerificationSha256","durationToleranceMs","recoveryAuthority"] {
                if p[k]!=n[k] {return Err("media_analysis_intent_immutable".into());}
            }
            for k in ["segments","dispatched"] {if !array(n,k)?.starts_with(array(p,k)?) {return Err("media_analysis_output_or_dispatch_removed".into());}}
            for k in ["failure","reconciliation"] {if !p[k].is_null() && p[k]!=n[k] {return Err("media_analysis_receipt_immutable".into());}}
            let old_state=text(p,"status")?; let new_state=text(n,"status")?;
            let valid=old_state==new_state || matches!((old_state,new_state),("owned","unknown"|"held"|"completed")|("unknown","completed"))
                || (old_state=="unknown" && new_state=="held" && !n["reconciliation"].is_null());
            if !valid {return Err("media_analysis_state_regressed".into());}
        }
    }
    Ok(())
}
fn validate_successor(key:&Value,prior:&Value,next:&Value)->Result<(),String> {
    if !matches!(text(prior,"status")?,"completed"|"held") || (prior["status"]=="held" && prior["reconciliation"].is_null()) {
        return Err("media_analysis_unreconciled_owner".into());
    }
    if prior["status"]=="held" {
        validate_reconciliation(prior)?;
    }
    let authority=&next["recoveryAuthority"];digest(authority,"receiptSha256")?;text(authority,"actor")?;
    if number(next,"epoch")?!=number(prior,"epoch")?.checked_add(1).ok_or("media_analysis_epoch_exhausted")?
        || authority["companyId"]!=key["companyId"] || authority["fileSha256"]!=key["sha256"]
        || authority["priorAttemptId"]!=prior["attemptId"] || authority["nextSpecSha256"]!=next["specSha256"]
        || !matches!(text(authority,"purpose")?,"missing_segments"|"upgrade") {return Err("media_analysis_recovery_authority_mismatch".into());}
    if authority["purpose"]=="missing_segments" {
        for k in ["specSha256","plan","durationMs","durationToleranceMs","noAudio","noAudioVerificationSha256"] {if next[k]!=prior[k] {return Err("media_analysis_recovery_spec_changed".into());}}
        if !array(next,"segments")?.starts_with(array(prior,"segments")?) {return Err("media_analysis_recovery_output_lost".into());}
    }
    Ok(())
}
fn validate_reconciliation(attempt:&Value)->Result<(),String> {
    let receipt=&attempt["reconciliation"];
    digest(receipt,"cessationSha256")?;digest(receipt,"reconciliationSha256")?;
    let resolution=text(receipt,"resolution")?;
    if !matches!(resolution,"not_dispatched"|"output_missing_after_dispatch")
        || (resolution=="not_dispatched" && !array(attempt,"dispatched")?.is_empty()) {
        return Err("media_analysis_reconciliation_invalid".into());
    }
    Ok(())
}

#[cfg(test)]
#[path = "media_analysis_tests.rs"]
mod tests;
