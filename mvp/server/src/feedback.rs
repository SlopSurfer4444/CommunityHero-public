//! Observations are immutable evidence, never automatically promoted policy.
use crate::{ApiResult, bad, conflict, id, list, list_mut, now, row};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

fn token(body: &Value, key: &str) -> ApiResult<Option<String>> {
    match body.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) if !s.is_empty() && s.len() <= 160 && !s.chars().any(char::is_control) => Ok(Some(s.clone())),
        _ => Err(bad("Invalid feedback identifier")),
    }
}
fn fingerprint(body: &Value) -> String { format!("{:x}", Sha256::digest(body.to_string().as_bytes())) }
pub fn retry(d: &Value, body: &Value, item: &str) -> ApiResult<Option<Value>> {
    let Some(key) = token(body, "eventId")? else { return Ok(None) };
    if let Some(e) = list(d,"feedback").iter().find(|e| e["id"] == key) {
        if e["itemId"] != item || e["requestHash"] != fingerprint(body) { return Err(conflict("Feedback event identifier reused with different request")); }
        return Ok(Some(e.clone()));
    }
    Ok(None)
}
pub fn origin(d: &Value, item: &Value, body: &Value) -> ApiResult<Value> {
    let Some(key)=token(body,"sourceProposalId")? else {
        if !body["sourceProposalRevision"].is_null() {return Err(bad("Source proposal ID required"));}
        return Ok(item.get("draftOrigin").cloned().unwrap_or(Value::Null));
    };
    let revision=body["sourceProposalRevision"].as_u64().filter(|r|*r>0).ok_or_else(||bad("Source proposal revision required"))?;
    let captured=&item["draftOrigin"];
    if captured["id"]==key && captured["revision"]==revision {return Ok(captured.clone());}
    let current=row(d,"proposals",&key)?;
    if current["itemId"]!=item["id"] {return Err(conflict("Source proposal belongs to another item"));}
    let presentation=list(d,"feedback").iter().find(|e|e["schemaVersion"]==2 && e["kind"]=="proposal_presented" && e["itemId"]==item["id"] && e["sourceProposalId"]==key && e["sourceProposalRevision"]==revision && e["origin"]["id"]==key && e["origin"]["itemId"]==item["id"] && e["origin"]["revision"]==revision);
    let observed=presentation.is_some();
    let proposal=if current["revision"]==revision {current}else{
        presentation.map(|event|&event["origin"]).or_else(||current["history"].as_array().and_then(|history|history.iter().find(|p|p["revision"]==revision && p["itemId"]==item["id"] && observed))).ok_or_else(||conflict("Source proposal revision was not observed"))?
    };
    // A prior presentation may bind a historically observed version after context refresh.
    if proposal["itemRevision"]!=item["revision"] && !observed {
        return Err(conflict("Source proposal was not observed before context changed"));
    }
    let mut snapshot=proposal.clone();
    if snapshot.get("platform").is_none(){snapshot["platform"]=item["platform"].clone();}
    if snapshot.get("tags").is_none(){snapshot["tags"]=item["triageTags"].clone();}
    if let Some(o)=snapshot.as_object_mut(){o.remove("history");}
    Ok(snapshot)
}
pub fn append(d:&mut Value, body:&Value, item:&Value, origin:&Value, kind:&str, extra:Value)->ApiResult<Value>{
    if let Some(e)=retry(d,body,item["id"].as_str().unwrap_or(""))? {return Ok(e);}
    let key=token(body,"eventId")?.unwrap_or_else(id);
    let session=token(body,"draftSessionId")?.map(Value::String).unwrap_or_else(||item["draftSessionId"].clone());
    let mut e=json!({"id":key,"eventId":key,"schemaVersion":2,"kind":kind,"accountId":d["account"],"itemId":item["id"],"sourceProposalId":origin["id"],"sourceProposalRevision":origin["revision"],"origin":origin,"draftSessionId":session,"sessionId":token(body,"sessionId")?,"actor":"local_operator","createdAt":now(),"requestHash":fingerprint(body),"prepareRunId":origin["prepareRunId"],"prepareBundleId":origin["prepareBundleId"],"prepareBundleDigest":origin["prepareBundleDigest"],"platform":item["platform"]});
    for (k,v) in extra.as_object().into_iter().flatten(){e[k]=v.clone();}
    // HTTP wrappers supply this field from authenticated request extensions,
    // replacing client input. Internal observations without it stay unattributed.
    if kind.starts_with("execution_") {
        e["actor"]=json!({"id":"server","role":"system"});
        e["actorVerified"]=json!(true);
    } else if body["_verifiedActor"].is_object() {
        e["actor"]=body["_verifiedActor"].clone();
        e["actorVerified"]=json!(true);
    } else {
        e["actor"]=json!({"id":"unknown","role":"unknown"});
        e["actorVerified"]=json!(false);
    }
    if !d["feedback"].is_array(){d["feedback"]=json!([]);}
    list_mut(d,"feedback").push(e.clone()); Ok(e)
}
pub fn client_event(d:&mut Value, body:&Value)->ApiResult<Value>{
    let item=row(d,"items",crate::required(body,"itemId")?)?.clone();
    token(body,"eventId")?.ok_or_else(||bad("Feedback eventId required"))?;
    if let Some(e)=retry(d,body,item["id"].as_str().unwrap())?{return Ok(e);}
    let kind=body["kind"].as_str().or(body["type"].as_str()).ok_or_else(||bad("Event kind required"))?;
    if !["proposal_presented","feedback_labelled","candidate_reviewed"].contains(&kind){return Err(bad("Unsupported client observation"));}
    let origin=origin(d,&item,body)?;
    if origin.is_null(){return Err(bad("Source proposal required"));}
    let extra=if kind=="candidate_reviewed" {
        let candidate=crate::required(body,"candidateId")?;
        let review=candidate.strip_prefix("feedback-candidate-").ok_or_else(||bad("Invalid candidate"))?;
        if !list(d,"feedback").iter().any(|e| e["id"]==review && e["itemId"]==item["id"] && e["kind"]=="review_confirmed" && e["sourceProposalId"]==origin["id"] && e["sourceProposalRevision"]==origin["revision"]){return Err(bad("Candidate review source not found"));}
        let decision=crate::required(body,"decision")?;
        if !["approved","rejected"].contains(&decision){return Err(bad("Invalid candidate decision"));}
        json!({"candidateId":candidate,"decision":decision,"source":"operator","promotesKnowledge":false})
    }else if kind=="feedback_labelled" {
        let label=crate::required(body,"label")?;
        if !["factual_error","wrong_recipient","unnecessary_reply","missed_complaint","unnecessary_escalation","style","missing_source","unsupported_promise","other"].contains(&label){return Err(bad("Unsupported feedback label"));}
        let note=body["note"].as_str().unwrap_or("");if note.len()>2000{return Err(bad("Feedback note too long"));}
        json!({"label":label,"note":note,"source":"operator","status":"pending_review"})
    }else{json!({"source":"operator"})};
    append(d,body,&item,&origin,kind,extra)
}
pub fn outcome(d:&mut Value,op:&Value,status:&str)->ApiResult<()> {
    let kind=match status {"succeeded"=>"execution_verified","unknown"=>"execution_unknown",_=>"execution_failed"};
    let p=row(d,"proposals",crate::required(op,"proposalId")?)?.clone();
    let item=row(d,"items",crate::required(op,"itemId")?)?.clone();
    let origin=p.get("origin").cloned().unwrap_or_else(||p.clone());
    let body=json!({"eventId":format!("execution:{}:{status}",op["id"].as_str().unwrap_or(""))});
    append(d,&body,&item,&origin,kind,json!({"operationId":op["id"],"proposalId":p["id"],"status":status,"action":p["kind"],"text":p["text"],"source":"server","actor":"server"}))?;Ok(())
}
