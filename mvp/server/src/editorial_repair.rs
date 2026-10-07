//! Proof-bound local text revision. A repair is never approval or execution.
use crate::*;
use axum::Extension;
use std::collections::HashSet;

const FIELDS:&[&str]=&["proposalId","proposalRevision","textSha256","contextDigest","rulesDigest","receiptSha256"];
pub(crate) fn normalized(job:&str,body:&Value)->ApiResult<Value>{
    if job.is_empty()||job.len()>256{return Err(bad("Invalid editorial parent job"));}
    let fields=body.as_object().ok_or_else(||bad("Editorial repair requires an object"))?;
    if fields.len()!=2||!fields.contains_key("requestId")||!fields.contains_key("expected"){
        return Err(bad("Editorial repair requires requestId and expected proof, never caller text"));
    }
    let refs=body["expected"].as_array().filter(|refs|!refs.is_empty()&&refs.len()<=100)
        .ok_or_else(||bad("Choose 1 to 100 exact editorial repair proofs"))?;
    let mut seen=HashSet::new();
    for r in refs {
        let f=r.as_object().ok_or_else(||bad("Invalid editorial repair proof"))?;
        let id=required(r,"proposalId")?;
        if f.len()!=FIELDS.len()||FIELDS.iter().any(|key|!f.contains_key(*key))||id.len()>256||!seen.insert(id)
            ||r["proposalRevision"].as_u64().is_none_or(|n|n==0)
            ||FIELDS[2..].iter().any(|key|!r[*key].as_str().is_some_and(|s|s.len()==64&&s.bytes().all(|c|c.is_ascii_digit()||(b'a'..=b'f').contains(&c)))) {
            return Err(bad("Invalid or duplicate editorial repair proof"));
        }
    }
    let mut canonical=body.clone();canonical["reviewJobId"]=json!(job);Ok(canonical)
}

fn same_proof(expected:&Value,c:&Value,receipt:&Value)->bool{
    FIELDS.iter().all(|field|if *field=="receiptSha256"{expected[*field]==receipt[*field]}else{expected[*field]==c[*field]})
}

pub(crate) fn admit(d:&mut Value,actor:&operator_auth::Actor,job_key:&str,body:&Value)->ApiResult<Value>{
    let canonical=normalized(job_key,body)?;
    let admission=local_admission::request(d,"editorial-repair",&canonical,actor)?
        .ok_or_else(||bad("Editorial repair requires requestId"))?;
    // Lost-ACK recovery precedes current revision checks and never reapplies.
    if let Some(saved)=local_admission::replay(d,&admission,actor)?{return Ok(saved);}
    let job=row(d,"jobs",job_key)?;
    conductor_authority::require_prior_attribution(conductor_authority::current_context().as_ref(),job)?;
    if job["kind"]!="editorial_review"||job["purpose"]!="editorial_review"||job["status"]!="completed"
        ||job["result"]!=job["editorialOutcome"]
        ||job["editorialPlan"]["account"]!=d["account"]||job["editorialPlan"]["connectorBinding"]!=d["connectorBinding"]
        ||(actor.role!="owner"&&job["operatorId"]!=actor.id){
        return Err(conflict("Editorial repair requires this company's completed model review"));
    }
    let mut selected=Vec::new();
    for expected in body["expected"].as_array().unwrap(){
        let key=required(expected,"proposalId")?;
        let p=row(d,"proposals",key)?;
        if !p["history"].is_null()&&!p["history"].is_array(){return Err(conflict("Editorial repair history is malformed"));}
        if p["kind"]!="reply_and_close"||p["revision"]!=expected["proposalRevision"]{
            return Err(conflict("Editorial repair proposal revision or action changed"));
        }
        let receipt=&p["editorialReview"];
        let c=editorial_review::repair_candidate(d,p,receipt).map_err(conflict)?;
        if !same_proof(expected,&c,receipt){return Err(conflict("Editorial repair expected proof changed"));}
        let parents:Vec<_>=job["editorialPlan"]["batches"].as_array().into_iter().flatten()
            .filter(|b|b["id"]==receipt["source"]["batchId"]).collect();
        if parents.len()!=1{return Err(conflict("Editorial repair parent batch differs"));}
        let settled=editorial_endpoint::settled_result(job,parents[0])?
            .ok_or_else(||conflict("Editorial repair result not durably settled"))?;
        let captures:Vec<_>=job["editorialBatches"].as_array().into_iter().flatten()
            .filter(|entry|entry["batchId"]==parents[0]["id"]).collect();
        if captures.len()!=1||captures[0]["capture"]["batch"]["digest"]!=receipt["source"]["batchDigest"]{
            return Err(conflict("Editorial repair dispatched source binding differs"));
        }
        let outcomes:Vec<_>=settled["outcomes"].as_array().into_iter().flatten().filter(|v|v["proposalId"]==key).collect();
        if outcomes.len()!=1||outcomes[0]["decision"]!="revise"||outcomes[0]["receiptSha256"]!=receipt["receiptSha256"]
            ||outcomes[0]["proposedText"]!=receipt["proposedText"]{
            return Err(conflict("Editorial repair suggestion is not this completed job's exact proof"));
        }
        let summary:Vec<_>=job["editorialOutcome"]["held"].as_array().into_iter().flatten()
            .filter(|v|v["reference"]["id"]==key&&v["reference"]["revision"]==p["revision"]).collect();
        if summary.len()!=1||summary[0]["decision"]!="revise"||summary[0]["repairExpected"]!=*expected
            ||summary[0]["suggestedText"]!=receipt["proposedText"]{
            return Err(conflict("Editorial repair public proof coverage differs"));
        }
        let suggestion=required(receipt,"proposedText")?;
        let binding=active_binding(d)?;let item=bound_item(&binding,row(d,"items",required(p,"itemId")?)?)?;
        reply_constraints::validate_reply(&prepare_bundle::EvidenceContext::new(d),&item,suggestion).map_err(conflict)?;
        let revision=p["revision"].as_u64().unwrap().checked_add(1).ok_or_else(||conflict("Editorial repair revision overflow"))?;
        // Repeated text in this lineage is a cycle, even under a new request ID.
        if p["history"].as_array().into_iter().flatten().any(|old|old["text"]==suggestion){return Err(conflict("Editorial repair makes no progress or repeats prior text"));}
        selected.push((p.clone(),suggestion.to_owned(),revision,expected.clone()));
    }
    let targets:Vec<_>=selected.iter().map(|(p,_,_,_)|p["itemId"].clone()).collect();
    let ctx=conductor_authority::fence_admission(d,"proposal",&targets)?;
    conductor_authority::fence_actor(ctx.as_ref(),actor)?;
    for (p,_,_,_) in &selected {conductor_authority::require_prior_attribution(ctx.as_ref(),p)?;}
    let repair_id=local_admission::receipt_id("editorial-repair",required(body,"requestId")?);
    let old_refs:Vec<_>=selected.iter().map(|(p,_,_,_)|json!({"id":p["id"],"revision":p["revision"]})).collect();
    let new_refs:Vec<_>=selected.iter().map(|(p,_,revision,_)|json!({"id":p["id"],"revision":revision})).collect();
    let mut result=json!({"version":1,"requestId":body["requestId"],"replayed":false,"repairId":repair_id,"status":"repaired",
        "parentReviewJobId":job_key,"account":d["account"],"connectorBinding":d["connectorBinding"],
        "oldRefs":old_refs,"newRefs":new_refs,"expected":body["expected"],"repairedAt":now()});
    result["repairReceiptSha256"]=json!(editorial_review::hash_text(&result.to_string()));
    // Every dependency was checked before any write; the storage transaction
    // atomically commits all selected new revisions and their durable receipt.
    for (before,suggestion,revision,expected) in selected {
        let p=row_mut(d,"proposals",required(&before,"id")?)?;
        let mut historical=before.clone();historical.as_object_mut().unwrap().remove("history");
        if !p["history"].is_array(){p["history"]=json!([]);}
        p["history"].as_array_mut().unwrap().push(historical);
        p["text"]=json!(suggestion);p["revision"]=json!(revision);p["status"]=json!("draft");
        p["editorialReview"]=Value::Null;
        p["editorialRepair"]=json!({"repairId":repair_id,"parentReviewJobId":job_key,"expected":expected,
            "repairReceiptSha256":result["repairReceiptSha256"],"newTextSha256":editorial_review::hash_text(&suggestion)});
    }
    local_admission::commit(d,&admission,&mut result)?;Ok(result)
}

pub(crate) fn validate_delta(before:&Value,after:&Value,canonical:&Value)->ApiResult<()>{
    if before==after{return Ok(());}
    let expected=canonical["expected"].as_array().ok_or_else(||internal("Editorial repair proof missing"))?;
    let old=list(before,"audit");let new=list(after,"audit");
    if new.len()!=old.len()+1||!new.starts_with(old){return Err(internal("Editorial repair admission receipt missing"));}
    let receipt=&new[old.len()];let result=&receipt["result"];
    let repair_id=local_admission::receipt_id("editorial-repair",required(canonical,"requestId")?);
    let mut unsigned=result.clone();unsigned.as_object_mut().ok_or_else(||internal("Editorial repair result missing"))?.remove("repairReceiptSha256");
    let mut payload=canonical.clone();payload.as_object_mut().unwrap().remove("requestId");
    if receipt["id"]!=repair_id||receipt["action"]!=local_admission::ACTION||receipt["kind"]!="editorial-repair"
        ||receipt["requestId"]!=canonical["requestId"]||receipt["payloadHash"]!=editorial_review::hash_text(&payload.to_string())
        ||receipt["account"]!=accounts::Profile::from_workspace(before)?.key()||result["account"]!=before["account"]
        ||result["connectorBinding"]!=before["connectorBinding"]||result["repairId"]!=repair_id||result["status"]!="repaired"
        ||result["requestId"]!=canonical["requestId"]||result["replayed"]!=false||result["parentReviewJobId"]!=canonical["reviewJobId"]
        ||result["expected"]!=canonical["expected"]||result["repairReceiptSha256"]!=editorial_review::hash_text(&unsigned.to_string()) {
        return Err(internal("Editorial repair durable authority differs"));
    }
    let mut old_refs=Vec::new();let mut new_refs=Vec::new();
    for e in expected{
        let key=required(e,"proposalId")?;let a=row(before,"proposals",key)?;let b=row(after,"proposals",key)?;
        let mut historical=a.clone();historical.as_object_mut().unwrap().remove("history");
        let mut history=a["history"].as_array().cloned().unwrap_or_default();history.push(historical);
        let revision=a["revision"].as_u64().and_then(|r|r.checked_add(1)).ok_or_else(||internal("Editorial repair revision invalid"))?;
        if b["text"]!=a["editorialReview"]["proposedText"]||b["revision"]!=revision||b["status"]!="draft"
            ||!b["editorialReview"].is_null()||b["history"]!=json!(history)
            ||b["editorialRepair"]!=json!({"repairId":repair_id,"parentReviewJobId":canonical["reviewJobId"],"expected":e,
                "repairReceiptSha256":result["repairReceiptSha256"],"newTextSha256":editorial_review::hash_text(b["text"].as_str().unwrap_or(""))}){
            return Err(internal("Editorial repair changed exact revision or history"));
        }
        old_refs.push(json!({"id":key,"revision":a["revision"]}));new_refs.push(json!({"id":key,"revision":revision}));
    }
    if result["oldRefs"]!=json!(old_refs)||result["newRefs"]!=json!(new_refs){return Err(internal("Editorial repair reference coverage differs"));}Ok(())
}

pub(crate) async fn post(State(app):State<App>,Extension(actor):Extension<operator_auth::Actor>,Path(job):Path<String>,Json(body):Json<Value>)->ApiResult<Json<Value>>{
    let canonical=normalized(&job,&body)?;
    if let Some(result)=local_admission::replay_committed(&app,"editorial-repair",&canonical,&actor).await?{return Ok(Json(result));}
    app.change_admission(storage::AdmissionScope::EditorialRepair{job:&job,body:&canonical},|d|admit(d,&actor,&job,&body)).await.map(Json)
}

#[cfg(test)]
#[path="editorial_repair_tests.rs"]
pub(crate) mod tests;
