//! Correlation for local job/approval admission. This grants no dispatch/retry authority.
//! A missing committed receipt means pending or unknown, never "safe to resend".
use crate::*;
use axum::Extension;
use sha2::{Digest,Sha256};

pub(crate) const ACTION:&str="local_admission.committed";
pub(crate) const REJECTED_ACTION:&str="local_admission.rejected";
pub(crate) const MAX_REJECTED_EVALUATIONS:usize=16;
pub(crate) struct Request {
    key:String,kind:String,account:String,actor_id:String,actor_role:String,hash:String,payload_hash:String,
}
pub(crate) fn receipt_id(kind:&str,key:&str)->String {
    format!("local-admission:{:x}",Sha256::digest(format!("{ACTION}\0{kind}\0{key}").as_bytes()))
}
fn validate(kind:&str,key:&str)->ApiResult<()> {
    if !matches!(kind,"prepare"|"approval"|"execute"|"editorial"|"editorial-repair"|"proposal-revalidate"|"proposal-source-rebind") {return Err(bad("Invalid local admission kind"));}
    if key.is_empty()||key.len()>160||!key.bytes().all(|b|b.is_ascii_alphanumeric()||b==b'-'||b==b'_') {
        return Err(bad("Invalid local admission requestId"));
    }
    Ok(())
}
pub(crate) fn request(d:&Value,kind:&str,body:&Value,actor:&operator_auth::Actor)->ApiResult<Option<Request>> {
    let Some(value)=body.get("requestId") else{return Ok(None)};
    let key=value.as_str().ok_or_else(||bad("Invalid local admission requestId"))?;
    validate(kind,key)?;
    let mut payload=body.clone();
    payload.as_object_mut().ok_or_else(||bad("Local admission requires an object"))?.remove("requestId");
    if kind=="execute" {payload.as_object_mut().unwrap().remove("reevaluate");}
    let account=accounts::Profile::from_workspace(d)?.key().to_owned();
    let payload_hash=format!("{:x}",Sha256::digest(payload.to_string().as_bytes()));
    let bytes=serde_json::to_vec(&json!({"kind":kind,"account":account,"actorId":actor.id,"actorRole":actor.role,"payload":payload}))
        .map_err(|_|bad("Invalid local admission payload"))?;
    if bytes.len()>2_000_000 {return Err(bad("Local admission payload too large"));}
    Ok(Some(Request{key:key.into(),kind:kind.into(),account,actor_id:actor.id.clone(),actor_role:actor.role.clone(),
        hash:format!("{:x}",Sha256::digest(bytes)),payload_hash}))
}
pub(crate) fn find_receipt<'a>(d:&'a Value,kind:&str,key:&str)->ApiResult<Option<&'a Value>> {
    validate(kind,key)?;
    let id=receipt_id(kind,key);
    let mut matches=list(d,"audit").iter().filter(|r|r["id"]==id||
        (r["action"]==ACTION&&r["kind"]==kind&&r["requestId"]==key));
    let first=matches.next();
    if matches.next().is_some()||first.is_some_and(|r|r["action"]!=ACTION||r["kind"]!=kind||r["requestId"]!=key||r["refId"]!=key) {
        return Err(internal("Invalid local admission receipt identity"));
    }
    Ok(first)
}
fn saved_result(receipt:&Value,kind:&str,key:&str,account:&str,actor:&operator_auth::Actor)->ApiResult<Value> {
    if let Some(ctx)=conductor_authority::current_context(){
        conductor_authority::fence_actor(Some(&ctx),actor)?;
        if receipt.get("conductorRunId").is_none()||receipt.get("grantGeneration").is_none(){
            return Err(conflict("Local admission receipt is outside this conductor run"));
        }
        conductor_authority::require_prior_attribution(Some(&ctx),receipt)?;
    }
    if receipt["kind"]!=kind||receipt["requestId"]!=key||receipt["account"]!=account
        ||receipt["actorId"]!=actor.id||receipt["actorRole"]!=actor.role {
        return Err(conflict("Local admission request belongs to another account or operator"));
    }
    let mut result=receipt["result"].clone();
    if !result.is_object()||result["requestId"]!=key
        ||(matches!(kind,"prepare"|"execute"|"editorial")&&result["jobId"].as_str().is_none_or(str::is_empty))
        ||(kind=="execute"&&result["approvalId"].as_str().is_none_or(str::is_empty))
        ||(kind=="approval"&&result["id"].as_str().is_none_or(str::is_empty)
            &&!(result["id"].is_null()&&result["admissionMode"]=="partial"&&result["status"]=="held"
                &&result["accepted"].as_array().is_some_and(Vec::is_empty)
                &&result["held"].as_array().is_some_and(|rows|!rows.is_empty()))) {
        return Err(internal("Invalid local admission result"));
    }
    if kind=="execute" && receipt["payloadHash"]!=format!("{:x}",Sha256::digest(
        json!({"approvalId":result["approvalId"]}).to_string().as_bytes())) {
        return Err(internal("Invalid execution admission approval binding"));
    }
    if kind=="editorial-repair" && (result["repairId"]!=receipt_id(kind,key)||result["status"]!="repaired"
        ||result["parentReviewJobId"].as_str().is_none_or(str::is_empty)
        ||result["newRefs"].as_array().is_none_or(Vec::is_empty)||result["oldRefs"].as_array().is_none_or(Vec::is_empty)){
        return Err(internal("Invalid editorial repair admission result"));
    }
    if kind=="proposal-revalidate" && (result["contract"]!=crate::proposal_revalidation::CONTRACT
        ||!matches!(result["status"].as_str(),Some("revalidated"|"held"))
        ||result["oldRefs"].as_array().zip(result["newRefs"].as_array()).is_none_or(|(old,new)|old.len()!=new.len()
            ||old.iter().zip(new).any(|(a,b)|a["id"]!=b["id"]||a["revision"].as_u64().and_then(|n|n.checked_add(1))!=b["revision"].as_u64()))
        ||result["held"].as_array().is_none()||result["approvalRequired"]!=true||result["editorialReviewRequired"]!=true||result["retryAllowed"]!=false){
        return Err(internal("Invalid proposal revalidation admission result"));
    }
    if kind=="proposal-source-rebind" && (result["contract"]!=crate::proposal_source_rebind::CONTRACT
        ||!matches!(result["status"].as_str(),Some("rebound"|"held"))
        ||result["oldRefs"].as_array().zip(result["newRefs"].as_array()).is_none_or(|(old,new)|old.len()!=new.len()
            ||old.iter().zip(new).any(|(a,b)|a["id"]!=b["id"]||a["revision"].as_u64().and_then(|n|n.checked_add(1))!=b["revision"].as_u64()))
        ||result["held"].as_array().is_none()||result["approvalRequired"]!=true||result["editorialReviewRequired"]!=true
        ||result["generationDispatched"]!=false||result["externalActions"]!=0||result["retryAllowed"]!=false) {
        return Err(internal("Invalid proposal source-rebind admission result"));
    }
    result["replayed"]=json!(true);
    Ok(result)
}
pub(crate) fn replay(d:&Value,request:&Request,actor:&operator_auth::Actor)->ApiResult<Option<Value>> {
    let Some(receipt)=find_receipt(d,&request.kind,&request.key)? else{return Ok(None)};
    replay_receipt(receipt,request,actor).map(Some)
}
fn replay_receipt(receipt:&Value,request:&Request,actor:&operator_auth::Actor)->ApiResult<Value> {
    if receipt["requestHash"]!=request.hash {return Err(conflict("Local admission requestId reused with different payload or operator"));}
    saved_result(receipt,&request.kind,&request.key,&request.account,actor)
}
/// Fast read for already committed admissions; the mutation transaction still
/// checks again after a missing receipt, so concurrent requests stay atomic.
pub(crate) async fn replay_committed(app:&App,kind:&str,body:&Value,actor:&operator_auth::Actor)->ApiResult<Option<Value>> {
    let Some(request)=request(&json!({"account":app.account.display()}),kind,body,actor)? else{return Ok(None)};
    let Some(receipt)=app.db.read_local_admission_receipt(kind,&request.key).await? else{return Ok(None)};
    if receipt["action"]==REJECTED_ACTION{return Ok(None);}
    replay_receipt(&receipt,&request,actor).map(Some)
}
/// Call only inside the same storage transaction that creates the result.
pub(crate) fn commit(d:&mut Value,request:&Request,result:&mut Value)->ApiResult<()> {
    if find_receipt(d,&request.kind,&request.key)?.is_some(){return Err(internal("Local admission already committed"));}
    if !result.is_object(){return Err(internal("Invalid local admission result"));}
    result["requestId"]=json!(request.key);result["replayed"]=json!(false);
    let mut receipt=json!({"id":receipt_id(&request.kind,&request.key),"action":ACTION,
        "refId":request.key,"requestId":request.key,"kind":request.kind,"account":request.account,
        "actorId":request.actor_id,"actorRole":request.actor_role,"requestHash":request.hash,"payloadHash":request.payload_hash,
        "result":result,"createdAt":now()});
    if let Some(ctx)=conductor_authority::current_context(){conductor_authority::tag(&ctx,&mut receipt);}
    list_mut(d,"audit").push(receipt);
    Ok(())
}

fn hash(value:&Value)->bool {
    value.as_str().is_some_and(|s|s.len()==64&&s.bytes().all(|b|b.is_ascii_hexdigit()))
}
pub(crate) fn negative_id(kind:&str,key:&str,index:u64)->String {
    format!("local-admission-negative:{:x}",Sha256::digest(json!([kind,key,index]).to_string().as_bytes()))
}
fn negative_digest(receipt:&Value)->ApiResult<String> {
    let mut bound=receipt.clone();
    bound.as_object_mut().ok_or_else(||internal("Invalid negative admission receipt"))?.remove("receiptSha256");
    Ok(format!("{:x}",Sha256::digest(bound.to_string().as_bytes())))
}
/// Complete bounded same-key rejection history must be projected under the
/// writer. Positive committed receipts remain at their original deterministic ID.
pub(crate) fn find_rejection<'a>(d:&'a Value,kind:&str,key:&str)->ApiResult<Option<&'a Value>> {
    validate(kind,key)?;
    let expected_ids:std::collections::HashSet<_>=(1..=MAX_REJECTED_EVALUATIONS as u64)
        .map(|index|negative_id(kind,key,index)).collect();
    let rows:Vec<_>=list(d,"audit").iter().filter(|r|
        r["id"].as_str().is_some_and(|id|expected_ids.contains(id))
        ||r["action"]==REJECTED_ACTION&&r["kind"]==kind&&r["requestId"]==key).collect();
    if rows.len()>MAX_REJECTED_EVALUATIONS{return Err(internal("Negative admission evaluation history exceeds its bounded contract"));}
    let mut ordered=std::collections::BTreeMap::new();let mut evaluation_ids=std::collections::HashSet::new();
    for receipt in rows {
        let index=receipt["evaluationIndex"].as_u64().filter(|n|*n>0&&*n<=MAX_REJECTED_EVALUATIONS as u64)
            .ok_or_else(||internal("Invalid negative admission evaluation index"))?;
        let evaluation_id=receipt["evaluationId"].as_str().filter(|id|!id.is_empty()&&id.len()<=160)
            .ok_or_else(||internal("Invalid negative admission evaluation identity"))?;
        if ordered.insert(index,receipt).is_some()||!evaluation_ids.insert(evaluation_id)
            ||kind!="execute"||receipt["action"]!=REJECTED_ACTION||receipt["kind"]!=kind
            ||receipt["requestId"]!=key||receipt["id"]!=negative_id(kind,key,index)||receipt["refId"]!=key
            ||!hash(&receipt["receiptSha256"])||receipt["receiptSha256"]!=negative_digest(receipt)?
            ||receipt["noAttemptProof"]!=json!({"executeJobCreated":false,
                "operationCreated":false,"approvalConsumed":false,"providerDispatchArmed":false}) {
            return Err(internal("Invalid negative admission identity or immutable digest"));
        }
    }
    let mut previous:Option<&Value>=None;
    for (position,(index,receipt)) in ordered.iter().enumerate() {
        if *index!=position as u64+1 {return Err(internal("Negative admission history projection is incomplete"));}
        if let Some(parent)=previous {
            if receipt["parentEvaluationId"]!=parent["evaluationId"]||receipt["parentReceiptSha256"]!=parent["receiptSha256"]
                ||["account","actorId","actorRole","actorAuthority","requestHash","payloadHash","approvalId","connectionBinding","conductorRunId"]
                    .iter().any(|field|receipt[*field]!=parent[*field]) {
                return Err(internal("Negative admission immutable parent chain or request binding changed"));
            }
        } else if !receipt.get("parentEvaluationId").is_some_and(Value::is_null)
            ||!receipt.get("parentReceiptSha256").is_some_and(Value::is_null) {
            return Err(internal("First negative admission must have explicit null parents"));
        }
        previous=Some(receipt);
    }
    Ok(previous)
}

pub(crate) fn rejection_view(receipt:&Value,kind:&str,key:&str,account:&str,actor:&operator_auth::Actor)->ApiResult<Value> {
    let index=receipt["evaluationIndex"].as_u64().ok_or_else(||internal("Invalid negative admission evaluation"))?;
    if kind!="execute"||receipt["action"]!=REJECTED_ACTION||receipt["id"]!=negative_id(kind,key,index)
        ||receipt["refId"]!=key||receipt["kind"]!=kind||receipt["requestId"]!=key
        ||receipt["account"]!=account||receipt["actorId"]!=actor.id||receipt["actorRole"]!=actor.role
        ||receipt["actorAuthority"]!=dispatch_authority::approval_binding(actor)
        ||receipt["receiptSha256"]!=negative_digest(receipt)?||!hash(&receipt["payloadHash"])
        ||!hash(&receipt["requestHash"])||receipt["noAttemptProof"]!=json!({"executeJobCreated":false,
            "operationCreated":false,"approvalConsumed":false,"providerDispatchArmed":false})
        ||receipt["approvalId"].as_str().is_none_or(str::is_empty)||!receipt["blockingJobIds"].is_array() {
        return Err(conflict("Negative admission does not belong to this exact request, company or actor"));
    }
    if receipt["payloadHash"]!=format!("{:x}",Sha256::digest(json!({"approvalId":receipt["approvalId"]}).to_string().as_bytes())) {
        return Err(internal("Negative admission approval binding is invalid"));
    }
    if let Some(ctx)=conductor_authority::current_context(){
        conductor_authority::fence_actor(Some(&ctx),actor)?;
        conductor_authority::require_prior_attribution(Some(&ctx),receipt)?;
    }
    let mut result=receipt.clone();
    for field in ["id","action","refId","actorAuthority"]{result.as_object_mut().unwrap().remove(field);}
    result["status"]=json!("rejected_local");
    result["viewStatus"]=json!(if receipt["reason"]=="dependency"{"waiting_dependency"}else{"rejected_local"});
    result["reevaluationAvailable"]=json!(index<MAX_REJECTED_EVALUATIONS as u64);
    result["retryAuthorized"]=json!(false);result["result"]=Value::Null;
    Ok(result)
}

/// Explicit re-evaluation names the latest durable negative. Ordinary repeats
/// return that negative without re-running eligibility; absence stays unknown.
pub(crate) fn check_reevaluation(d:&Value,request:&Request,actor:&operator_auth::Actor,body:&Value)->ApiResult<Option<Value>> {
    let latest=find_rejection(d,&request.kind,&request.key)?;
    let Some(receipt)=latest else {
        if body.get("reevaluate").is_some(){return Err(conflict("No durable rejected admission to re-evaluate"));}
        return Ok(None);
    };
    let view=rejection_view(receipt,&request.kind,&request.key,&request.account,actor)?;
    if receipt["requestHash"]!=request.hash||receipt["connectionBinding"]!=active_binding(d)?.to_json(){
        return Err(conflict("Rejected admission request payload, actor or connection binding changed"));
    }
    let Some(control)=body.get("reevaluate") else {return Ok(Some(view));};
    if control.as_object().is_none_or(|fields|fields.len()!=2||!fields.contains_key("evaluationId")||!fields.contains_key("receiptSha256"))
        ||control["evaluationId"]!=receipt["evaluationId"]||control["receiptSha256"]!=receipt["receiptSha256"]
        ||view["reevaluationAvailable"]!=true {
        return Err(conflict("Re-evaluation requires the exact latest durable rejection"));
    }
    Ok(None)
}

/// Called ONLY for a recognized result of immutable execute prevalidation,
/// before any new job/operation/approval consumption. Never catch mutating Err.
pub(crate) fn reject_execute(d:&mut Value,request:&Request,actor:&operator_auth::Actor,approval:&str,
    reason:&str,blocking_jobs:Vec<Value>)->ApiResult<Value> {
    if request.kind!="execute" {return Err(internal("Only pure execute rejection has a durable negative contract"));}
    if d[connection_gate::FIELD]["gateEpoch"].as_u64().is_none_or(|epoch|epoch==0)
        ||d[connection_gate::FIELD]["connectionBinding"]!=active_binding(d)?.to_json()
        ||blocking_jobs.iter().any(|job|job.as_str().is_none_or(str::is_empty)) {
        return Err(internal("Negative admission lacks its exact gate/binding/dependency projection"));
    }
    let parent=find_rejection(d,&request.kind,&request.key)?.cloned();
    let index=parent.as_ref().map_or(1,|r|r["evaluationIndex"].as_u64().unwrap()+1);
    if index>MAX_REJECTED_EVALUATIONS as u64{return Err(conflict("Local admission re-evaluation budget exhausted"));}
    let mut receipt=json!({"id":negative_id(&request.kind,&request.key,index),"action":REJECTED_ACTION,"refId":request.key,
        "requestId":request.key,"kind":request.kind,"account":request.account,"actorId":request.actor_id,"actorRole":request.actor_role,
        "actorAuthority":dispatch_authority::approval_binding(actor),"requestHash":request.hash,"payloadHash":request.payload_hash,
        "approvalId":approval,"connectionBinding":active_binding(d)?.to_json(),"gateEpoch":d[connection_gate::FIELD]["gateEpoch"],
        "evaluationId":id(),"evaluationIndex":index,"reason":reason,"blockingJobIds":blocking_jobs,
        "parentEvaluationId":parent.as_ref().map(|r|r["evaluationId"].clone()),
        "parentReceiptSha256":parent.as_ref().map(|r|r["receiptSha256"].clone()),
        "noAttemptProof":{"executeJobCreated":false,"operationCreated":false,"approvalConsumed":false,"providerDispatchArmed":false},
        "createdAt":now()});
    if let Some(ctx)=conductor_authority::current_context(){conductor_authority::tag(&ctx,&mut receipt);}
    receipt["receiptSha256"]=json!(negative_digest(&receipt)?);
    let view=rejection_view(&receipt,&request.kind,&request.key,&request.account,actor)?;
    list_mut(d,"audit").push(receipt);Ok(view)
}
pub(crate) async fn lookup(State(app):State<App>,Extension(actor):Extension<operator_auth::Actor>,
    Path((kind,key)):Path<(String,String)>)->ApiResult<Json<Value>> {
    validate(&kind,&key)?;
    let Some(receipt)=app.db.read_local_admission_receipt(&kind,&key).await? else {
        return Ok(Json(json!({"requestId":key,"kind":kind,"account":app.account.key(),
            "status":"pending_or_unknown","result":null,"retryAuthorized":false})));
    };
    if receipt["action"]==REJECTED_ACTION {
        return Ok(Json(rejection_view(&receipt,&kind,&key,app.account.key(),&actor)?));
    }
    let result=saved_result(&receipt,&kind,&key,app.account.key(),&actor)?;
    Ok(Json(json!({"requestId":key,"kind":kind,"account":app.account.key(),"status":"committed","result":result,"payloadHash":receipt["payloadHash"],
        "retryAuthorized":false})))
}

#[cfg(test)]
#[path="local_admission_tests.rs"]
mod tests;
