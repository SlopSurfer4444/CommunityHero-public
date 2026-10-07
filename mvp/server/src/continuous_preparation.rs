//! Existing producer's read/review-only tail and finite native Codex admission.
//! Issued slots are immutable intent, never money/HTTP/token ceilings or refunds.
use crate::*;
use axum::Extension;
use std::collections::{BTreeMap, BTreeSet};

pub(crate) const MODE:&str="prepare_review_only";
pub(crate) const BUDGET_CONTRACT:&str="communityhero-codex-invocation-budget-v1";
const ORIGIN:&str="continuousPreparationOrigin";
const RESERVATIONS:&str="codexInvocationReservations";
const MAX_BRIDGE_SLOTS:u64=8;
const MAX_LIMIT:u64=100_000;
const MAX_TAIL_REFS:usize=32;

fn policy(d:&Value)->&Value {&d["settings"]["autoPreparation"]["continuousPreparation"]}
fn hash(v:&Value)->String {editorial_review::hash_text(&v.to_string())}
fn binding_hash(d:&Value)->ApiResult<String>{Ok(hash(&active_binding(d)?.to_json()))}
fn positive(body:&Value,key:&str,max:u64)->ApiResult<u64>{
    body[key].as_u64().filter(|v|*v>0&&*v<=max).ok_or_else(||bad(&format!("{key} must be a positive bounded integer")))
}
fn rows<'a>(v:&'a Value,key:&str)->&'a [Value]{v[key].as_array().map(Vec::as_slice).unwrap_or(&[])}
fn carry_has_job(mut carry:&Value,id:&Value)->bool {
    for _ in 0..16 {
        if carry.is_null(){return false;}
        if rows(carry,"originalJobs").iter().any(|job|job["jobId"]==*id){return true;}
        carry=&carry["predecessorCarry"];
    }
    !carry.is_null() // Unknown over-depth closure cannot admit a colliding job.
}
fn enabled_policy(d:&Value)->ApiResult<&Value>{
    let p=policy(d);
    if p["enabled"]!=true{return Err(conflict("continuous_policy_disabled_or_unconfigured"));}
    let profile=accounts::Profile::from_workspace(d)?;
    if p["version"]!=1||p["workflowMode"]!=MODE||p["account"]!=profile.key()||p["companyId"]!=profile.display()
        ||p["queueEpoch"].as_str()!=Some(working_generation::current(d)?)
        ||p["connectorBindingSha256"]!=binding_hash(d)?||p["policyId"].as_str().is_none_or(|v|v.is_empty())
        ||p["policyRevision"].as_u64().is_none_or(|v|v==0)||p["issuedSlots"].as_u64().is_none() {
        return Err(conflict("continuous_policy_scope_unverified"));
    }
    for (key,max) in [("invocationLimit",MAX_LIMIT),("maxOriginalJobInvocations",MAX_LIMIT),
        ("invocationsPerBridge",MAX_BRIDGE_SLOTS),("maxActiveJobs",64),("maxReadyItems",100_000),("maxReadyBytes",64*1024*1024)] {
        positive(p,key,max)?;
    }
    let carry=&d["cleanStartArchive"]["invocationBudgetCarry"];
    if !carry.is_null() && (carry["contract"]!="communityhero-clean-start-invocation-carry.v1"
        ||carry["policyId"]!=p["policyId"]||carry["observedTokens"].as_u64().is_none()
        ||carry["tokenObservationIncomplete"].as_bool().is_none()||carry["issuedSlots"].as_u64().is_none()
        ||carry["issuedSlots"].as_u64()>p["issuedSlots"].as_u64()
        ||carry["refundAuthorized"]!=false||carry["resumeAuthorized"]!=false) {
        return Err(conflict("continuous_archived_usage_unverified"));
    }
    if !carry.is_null()&&rows(d,"jobs").iter().any(|job|carry_has_job(carry,&job["id"])) {
        return Err(conflict("continuous_archived_job_cannot_be_reimported_as_current"));
    }
    Ok(p)
}
pub(crate) fn enabled(d:&Value)->bool{enabled_policy(d).is_ok()}
fn issued_total(d:&Value,policy_id:&Value,original:Option<&str>)->u64{
    let scanned=list(d,"jobs").iter().filter(|j|j[ORIGIN]["policyId"]==*policy_id
        &&original.is_none_or(|root|j[ORIGIN]["originalJobId"]==root))
        .flat_map(|j|rows(j,RESERVATIONS)).map(|r|rows(&r["envelope"],"slotIds").len() as u64)
        .fold(0,u64::saturating_add);
    let durable=if let Some(root)=original{row(d,"jobs",root).ok().and_then(|j|j["codexInvocationIssuedSlots"].as_u64()).unwrap_or(0)}
        else if policy(d)["policyId"]==*policy_id{policy(d)["issuedSlots"].as_u64().unwrap_or(0)}else{0};
    scanned.max(durable)
}
fn observed_tokens(d:&Value,policy_id:&Value)->(u64,bool){
    let carry=&d["cleanStartArchive"]["invocationBudgetCarry"];
    let carried=!carry.is_null()&&carry["policyId"]==*policy_id;
    let mut total=if carried{carry["observedTokens"].as_u64().unwrap_or(u64::MAX)}else{0};
    let mut incomplete=carried&&carry["tokenObservationIncomplete"].as_bool().unwrap_or(true);
    let archived=|job:&Value|carried&&carry_has_job(carry,&job["id"]);
    for reservation in list(d,"jobs").iter().filter(|j|j[ORIGIN]["policyId"]==*policy_id&&!archived(j)).flat_map(|j|rows(j,RESERVATIONS)){
        let invoked=rows(&reservation["observation"],"invoked");
        if reservation["observation"].is_null() {incomplete=true;}
        for invocation in invoked{
            let usage=&invocation["usage"];
            if usage["status"]!="observed"{incomplete=true;continue;}
            total=total.saturating_add(usage["input_tokens"].as_u64().unwrap_or(0)).saturating_add(usage["output_tokens"].as_u64().unwrap_or(0));
        }
    }
    (total,incomplete)
}
fn invocation_partition(d:&Value,policy_id:&Value)->Value {
    let mut reserved=0u64;let mut settled=0u64;let mut unknown=0u64;let mut not_invoked=0u64;
    for job in list(d,"jobs").iter().filter(|j|j[ORIGIN]["policyId"]==*policy_id) {
        for reservation in rows(job,RESERVATIONS) {
            let slots=rows(&reservation["envelope"],"slotIds").len() as u64;
            let observation=&reservation["observation"];
            if observation.is_null() {
                if matches!(job["status"].as_str(),Some("queued"|"running")){reserved=reserved.saturating_add(slots);}
                else{unknown=unknown.saturating_add(slots);}
                continue;
            }
            let invoked=rows(observation,"invoked");
            let returned=invoked.iter().filter(|i|i["state"]=="returned").count() as u64;
            let unused=slots.saturating_sub(invoked.len() as u64);
            settled=settled.saturating_add(returned).saturating_add(unused);
            unknown=unknown.saturating_add((invoked.len() as u64).saturating_sub(returned));
            not_invoked=not_invoked.saturating_add(unused);
        }
    }
    // A clean-start counter floor is preserved even when old journals remain
    // in the archive. It is never reported as zero spend or a refundable slot.
    let accounted=reserved.saturating_add(settled).saturating_add(unknown);
    unknown=unknown.saturating_add(issued_total(d,policy_id,None).saturating_sub(accounted));
    json!({"unit":"issued_codex_process_slot","reserved":reserved,"settled":settled,"unknown":unknown,
        "observedNotInvoked":not_invoked,"refundAuthorized":false,"billableWireRequests":null})
}
fn current_refs(d:&Value)->Value{
    json!(list(d,"proposals").iter().filter(|p|p["status"]=="draft"
        &&p["prepareRunId"].as_str().is_some_and(|id|row(d,"jobs",id).is_ok_and(|j|j[ORIGIN]["kind"]=="continuous_background")))
        .map(|p|json!({"id":p["id"],"revision":p["revision"]})).collect::<Vec<_>>())
}
fn ready(d:&Value)->ApiResult<Value>{
    let mut view=editorial_endpoint::current_ready(d,&current_refs(d))?;
    let profile=accounts::Profile::from_workspace(d)?;
    let current_generation=working_generation::current(d).ok();let current_binding=binding_hash(d).ok();
    let mut ready=Vec::new();let mut held=rows(&view,"held").to_vec();
    for reference in rows(&view,"readyForOwnerApproval"){
        let p=row(d,"proposals",required(reference,"id")?)?;
        let job=row(d,"jobs",required(p,"prepareRunId")?)?;let origin=&job[ORIGIN];
        if origin["queueEpoch"].as_str()!=current_generation||origin["connectorBindingSha256"].as_str()!=current_binding.as_deref()
            ||origin["account"]!=profile.key()||origin["workflowMode"]!=MODE{
            held.push(json!({"reference":{"id":reference["id"],"revision":reference["revision"]},"reason":"continuous_original_queue_or_connection_scope_changed"}));
        }else{ready.push(reference.clone());}
    }
    view["readyForOwnerApproval"]=json!(ready);view["held"]=json!(held);Ok(view)
}
/// Recovery/readiness is independent from admission of another paid invocation.
pub(crate) fn admission_reason(d:&Value)->Option<String>{
    let p=match enabled_policy(d){Ok(p)=>p,Err(e)=>return Some(e.1)};
    if issued_total(d,&p["policyId"],None)>=p["invocationLimit"].as_u64().unwrap_or(0){return Some("continuous_invocation_budget_exhausted".into());}
    if let Some(stop)=p["observedTokenStop"].as_u64(){
        if observed_tokens(d,&p["policyId"]).0>=stop{return Some("continuous_observed_token_stop".into());}
    }
    let active=list(d,"jobs").iter().filter(|j|j[ORIGIN]["kind"]=="continuous_background"
        &&matches!(j["status"].as_str(),Some("queued"|"running"))).count() as u64;
    if active>=p["maxActiveJobs"].as_u64().unwrap_or(0){return Some("continuous_active_backpressure".into());}
    if let Ok(view)=ready(d){
        let refs=rows(&view,"readyForOwnerApproval");
        let bytes=refs.iter().filter_map(|r|row(d,"proposals",r["id"].as_str()?).ok())
            .map(|p|p["text"].as_str().unwrap_or("").len() as u64).fold(0,u64::saturating_add);
        if refs.len() as u64>=p["maxReadyItems"].as_u64().unwrap_or(0)||bytes>=p["maxReadyBytes"].as_u64().unwrap_or(0){
            return Some("continuous_ready_backpressure".into());
        }
    }
    None
}
pub(crate) async fn configure(State(app):State<App>,Extension(actor):Extension<operator_auth::Actor>,Json(body):Json<Value>)->ApiResult<Json<Value>>{
    if actor.role!="owner"{return Err(ApiError(StatusCode::FORBIDDEN,"Continuous preparation requires company owner".into()));}
    let fields=body.as_object().ok_or_else(||bad("Continuous policy requires an object"))?;
    if fields.keys().any(|k|!["enabled","workflowMode","invocationLimit","maxOriginalJobInvocations","invocationsPerBridge","maxActiveJobs","maxReadyItems","maxReadyBytes","observedTokenStop"].contains(&k.as_str()))
        ||body["enabled"].as_bool().is_none()||body["workflowMode"]!=MODE{return Err(bad("Invalid continuous preparation policy"));}
    for (key,max) in [("invocationLimit",MAX_LIMIT),("maxOriginalJobInvocations",MAX_LIMIT),("invocationsPerBridge",MAX_BRIDGE_SLOTS),
        ("maxActiveJobs",64),("maxReadyItems",100_000),("maxReadyBytes",64*1024*1024)]{positive(&body,key,max)?;}
    if body.get("observedTokenStop").is_some(){positive(&body,"observedTokenStop",u64::MAX)?;}
    let at=now();
    app.change(|d|{
        let generation=working_generation::current(d)?.to_owned();
        let profile=accounts::Profile::from_workspace(d)?;
        let old=policy(d).clone();
        let mut p=body.clone();
        p["version"]=json!(1);p["account"]=json!(profile.key());p["companyId"]=json!(profile.display());
        p["queueEpoch"]=json!(generation);p["connectorBindingSha256"]=json!(binding_hash(d)?);
        p["policyId"]=old.get("policyId").cloned().unwrap_or_else(||json!(id()));
        p["policyRevision"]=json!(old["policyRevision"].as_u64().unwrap_or(0).checked_add(1).ok_or_else(||conflict("Continuous policy revision exhausted"))?);
        p["configuredBy"]=json!(actor.id);p["configuredAt"]=json!(at);
        p["issuedSlots"]=old.get("issuedSlots").cloned().unwrap_or_else(||json!(0));
        // Explicit owner changes never erase already issued or unknown slots.
        if issued_total(d,&p["policyId"],None)>p["invocationLimit"].as_u64().unwrap_or(0){return Err(conflict("Invocation policy is below already issued slots"));}
        d["settings"]["autoPreparation"]["continuousPreparation"]=p;
        audit(d,"preparation.continuous_configured",&actor.id);
        Ok(Json(status_view(d,&at,true)))
    }).await
}
fn existing_origin<'a>(d:&'a Value,job:&Value)->ApiResult<&'a Value>{
    let root=job[ORIGIN]["originalJobId"].as_str().ok_or_else(||conflict("Continuous original job missing"))?;
    let original=row(d,"jobs",root)?;
    if original[ORIGIN]["originalJobId"]!=root||original[ORIGIN]["kind"]!="continuous_background"{return Err(conflict("Continuous original authority missing"));}
    Ok(&original[ORIGIN])
}
pub(crate) fn stamp_background_job(d:&mut Value,job_id:&str,parent:Option<&str>)->ApiResult<()>{
    if !row(d,"jobs",job_id)?[ORIGIN].is_null(){return Err(conflict("Continuous origin already admitted"));}
    let p=enabled_policy(d)?.clone();
    let profile=accounts::Profile::from_workspace(d)?;
    let root=if let Some(parent)=parent{
        let j=row(d,"jobs",parent)?;
        let origin=existing_origin(d,j)?;
        if origin["policyId"]!=p["policyId"]||origin["queueEpoch"]!=p["queueEpoch"]||origin["connectorBindingSha256"]!=p["connectorBindingSha256"]{
            return Err(conflict("Continuous descendant original scope changed"));
        }
        origin.clone()
    }else{json!({"originalJobId":job_id,"maxOriginalJobInvocations":p["maxOriginalJobInvocations"],"invocationsPerBridge":p["invocationsPerBridge"]})};
    let origin=json!({"version":1,"kind":"continuous_background","workflowMode":MODE,"account":profile.key(),"companyId":profile.display(),
        "queueEpoch":p["queueEpoch"],"connectorBindingSha256":p["connectorBindingSha256"],"policyId":p["policyId"],"policyRevision":p["policyRevision"],
        "nativeJobId":job_id,"originalJobId":root["originalJobId"],"parentJobId":parent,"maxOriginalJobInvocations":root["maxOriginalJobInvocations"],"invocationsPerBridge":root["invocationsPerBridge"]});
    let j=row_mut(d,"jobs",job_id)?;
    j[ORIGIN]=origin;j["workflowMode"]=json!(MODE);j[RESERVATIONS]=json!([]);
    j["codexInvocationIssuedSlots"]=json!(0);
    Ok(())
}
fn parent_id(job:&Value)->Option<&str>{
    job["originatingAnsweringAttemptId"].as_str().or_else(||job["parentPrepareJobId"].as_str())
        .or_else(||job["automaticFactContinuation"]["parentPrepareJobId"].as_str()).or_else(||job["continuousParentJobId"].as_str())
        .or_else(||(job["purpose"]=="auto_revalidate").then(||job["prepareBundle"]["request"]["previousDecision"]["prepareRunId"].as_str()).flatten())
}
// New operator-requested review of a background draft still descends from its
// native paid original. Request purpose/fresh cannot relabel it interactive.
fn inferred_parent(d:&Value,job:&Value)->ApiResult<Option<String>>{
    if let Some(parent)=parent_id(job){row(d,"jobs",parent)?;return Ok(Some(parent.to_owned()));}
    if job["kind"]!="editorial_review"{return Ok(None);}
    let mut originals:BTreeMap<String,String>=BTreeMap::new();let mut interactive=false;
    for reference in rows(job,"editorialReferences"){
        let p=row(d,"proposals",required(reference,"id")?)?;
        let Some(run)=p["prepareRunId"].as_str() else{interactive=true;continue;};
        let original=row(d,"jobs",run)?;
        if original[ORIGIN]["kind"]=="continuous_background"{
            originals.entry(required(&original[ORIGIN],"originalJobId")?.to_owned()).or_insert_with(||run.to_owned());
        }else{interactive=true;}
    }
    if originals.is_empty(){return Ok(None);}
    if originals.len()!=1||interactive{
        if !rows(&job["editorialPlan"],"batches").is_empty(){return Err(conflict("Paid editorial admission mixes continuous originals; select one original scope"));}
        return Ok(None); // All-current receipt reuse requires no model admission.
    }
    Ok(originals.into_values().next())
}
fn native_background(job:&Value)->bool{matches!(job["purpose"].as_str(),Some("auto_prepare"|"auto_revalidate"))}
/// Root calls this once inside the admission writer BEFORE centralized guard.
/// Existing/unknown jobs are never upgraded by a poll, restart or request body.
pub(crate) fn stamp_admissions(before:&Value,after:&mut Value)->ApiResult<()>{
    if before.get("jobs").is_none()&&after.get("jobs").is_none(){return Ok(());}
    let known:BTreeSet<_>=list(before,"jobs").iter().filter_map(|j|j["id"].as_str()).collect();
    let ids=list(after,"jobs").iter().filter(|j|!known.contains(j["id"].as_str().unwrap_or("")))
        .map(|j|required(j,"id").map(str::to_owned)).collect::<ApiResult<Vec<_>>>()?;
    for job_id in ids{
        let job=row(after,"jobs",&job_id)?.clone();
        if !job[ORIGIN].is_null(){continue;}
        let parent=inferred_parent(after,&job)?;
        let inherited=parent.as_deref().is_some_and(|id|row(after,"jobs",id).is_ok_and(|j|j[ORIGIN]["kind"]=="continuous_background"));
        if inherited || (native_background(&job)&&enabled(after)){stamp_background_job(after,&job_id,parent.as_deref().filter(|_|inherited))?;}
    }
    Ok(())
}
pub(crate) fn reserve_bridge(d:&mut Value,job_id:&str,operation:&str,request:&Value,at:&str)->ApiResult<Option<Value>>{
    if !matches!(operation,"assistant"|"assistant_research"){return Ok(None);}
    let job=row(d,"jobs",job_id)?.clone();
    let origin=&job[ORIGIN];
    if origin.is_null(){
        if native_background(&job)||inferred_parent(d,&job)?.as_deref().is_some_and(|id|row(d,"jobs",id).is_ok_and(|j|!j[ORIGIN].is_null())){
            return Err(conflict("continuous_background_origin_missing"));
        }
        return Ok(None); // Independent ownerinteractive admission owns this path.
    }
    let p=enabled_policy(d)?.clone();
    let original=existing_origin(d,&job)?.clone();
    if origin["nativeJobId"]!=job_id||origin["workflowMode"]!=MODE||origin["policyId"]!=p["policyId"]||origin["queueEpoch"]!=p["queueEpoch"]
        ||origin["connectorBindingSha256"]!=p["connectorBindingSha256"]||origin["account"]!=request["account"]||request["operation"]!=operation
        ||!matches!(job["status"].as_str(),Some("queued"|"running")){
        return Err(conflict("continuous_invocation_scope_changed"));
    }
    let request_sha=hash(request); // SAME original serde wire/paid digest; never JS reserialization.
    if rows(&job,RESERVATIONS).iter().any(|r|r["envelope"]["requestSha256"]==request_sha){return Err(conflict("continuous_original_invocation_already_issued"));}
    let count=positive(&original,"invocationsPerBridge",MAX_BRIDGE_SLOTS)?;
    let global=issued_total(d,&p["policyId"],None);
    let root_id=required(&original,"originalJobId")?;
    let root=issued_total(d,&p["policyId"],Some(root_id));
    if global.checked_add(count).is_none_or(|n|n>p["invocationLimit"].as_u64().unwrap())
        ||root.checked_add(count).is_none_or(|n|n>original["maxOriginalJobInvocations"].as_u64().unwrap_or(0)){
        return Err(conflict("continuous_invocation_budget_exhausted"));
    }
    if p["observedTokenStop"].as_u64().is_some_and(|stop|observed_tokens(d,&p["policyId"]).0>=stop){return Err(conflict("continuous_observed_token_stop"));}
    // Retained local material/review needs no connector mutation/send permit.
    connection_gate::preparation_dependency(d,false)?;
    let reservation_id=id();
    let envelope=json!({"version":1,"contract":BUDGET_CONTRACT,"account":origin["account"],"companyId":origin["companyId"],
        "connectorBindingSha256":origin["connectorBindingSha256"],"queueEpoch":origin["queueEpoch"],"policyId":origin["policyId"],
        "policyRevision":origin["policyRevision"],"nativeJobId":job_id,"originalJobId":root_id,"requestSha256":request_sha,
        "reservationId":reservation_id,"slotIds":(1..=count).map(|n|format!("{reservation_id}:{n}")).collect::<Vec<_>>()});
    if envelope.to_string().len()>16*1024{return Err(internal("Continuous transport envelope exceeds bound"));}
    d["settings"]["autoPreparation"]["continuousPreparation"]["issuedSlots"]=json!(global+count);
    row_mut(d,"jobs",root_id)?["codexInvocationIssuedSlots"]=json!(root+count);
    row_mut(d,"jobs",job_id)?[RESERVATIONS].as_array_mut().ok_or_else(||conflict("Continuous reservation journal missing"))?
        .push(json!({"envelope":envelope,"issuedAt":at,"observation":null,"observedAt":null}));
    Ok(Some(envelope))
}
pub(crate) fn settle_bridge(d:&mut Value,job_id:&str,envelope:&Value,result:&Value,at:&str)->ApiResult<()>{
    let proof=&result["runMetadata"]["invocationBudget"];
    if proof["version"]!=1||proof["contract"]!=BUDGET_CONTRACT||proof["reservationId"]!=envelope["reservationId"]||proof["nativeJobId"]!=job_id
        ||proof["originalJobId"]!=envelope["originalJobId"]||proof["requestSha256"]!=envelope["requestSha256"]||proof["issuedSlotIds"]!=envelope["slotIds"]
        ||proof["unit"]!="codex_process_invocation"||proof["hardTokenCeiling"]!=false||!proof["billableWireRequests"].is_null()||proof["refundAuthorized"]!=false{
        return Err(conflict("continuous_invocation_observation_invalid_paid_result_retained"));
    }
    let invoked=proof["invoked"].as_array().ok_or_else(||conflict("Continuous invoked observation missing"))?;
    let slots=envelope["slotIds"].as_array().ok_or_else(||conflict("Continuous issued slots missing"))?;
    if invoked.len()>slots.len(){return Err(conflict("Continuous invoked observation exceeds original issued slots"));}
    for (ordinal,invocation) in invoked.iter().enumerate(){
        if invocation["slotId"]!=slots[ordinal]||invocation["ordinal"].as_u64()!=Some(ordinal as u64+1)
            ||!matches!(invocation["stage"].as_str(),Some("primary"|"url_verification"|"visual_followup"))
            ||!matches!(invocation["state"].as_str(),Some("returned"|"unknown"|"armed"))||invocation["terminalEvents"].as_u64().is_none()
            ||!matches!(invocation["usage"]["status"].as_str(),Some("observed"|"unavailable"|"ambiguous"|"invalid")){
            return Err(conflict("Continuous invoked slot observation invalid; issued budget retained"));
        }
        if invocation["usage"]["status"]=="observed"{
            for key in ["input_tokens","output_tokens"]{if invocation["usage"][key].as_u64().is_none(){return Err(conflict("Continuous usage observation invalid"));}}
            if invocation["usage"].get("cached_input_tokens").is_some()&&invocation["usage"]["cached_input_tokens"].as_u64().is_none(){return Err(conflict("Continuous cached usage invalid"));}
        }
    }
    let reservation=row_mut(d,"jobs",job_id)?[RESERVATIONS].as_array_mut().ok_or_else(||conflict("Continuous reservation journal missing"))?
        .iter_mut().find(|r|r["envelope"]==*envelope).ok_or_else(||conflict("Original continuous reservation missing"))?;
    if !reservation["observation"].is_null(){
        if reservation["observation"]!=*proof{return Err(conflict("Continuous settled observation immutable"));}
        return Ok(());
    }
    reservation["observation"]=proof.clone();reservation["observedAt"]=json!(at);
    Ok(())
}
fn immutable_reservation(v:&Value)->Value{let mut v=v.clone();if let Some(o)=v.as_object_mut(){o.remove("observation");o.remove("observedAt");}v}
pub(crate) fn validate_change(before:&Value,after:&Value)->ApiResult<()>{
    if before.pointer("/cleanStartArchive/invocationBudgetCarry")!=after.pointer("/cleanStartArchive/invocationBudgetCarry") {
        return Err(internal("Archived continuous usage is immutable in the working database"));
    }
    let carry=&after["cleanStartArchive"]["invocationBudgetCarry"];
    if !carry.is_null()&&rows(after,"jobs").iter().any(|job|carry_has_job(carry,&job["id"])) {
        return Err(internal("Archived job identity cannot be reused in the working database"));
    }
    let old=policy(before);let new=policy(after);
    if !old.is_null()&&(new.is_null()||old["policyId"]!=new["policyId"]||new["policyRevision"].as_u64()<old["policyRevision"].as_u64()){
        return Err(internal("Continuous policy identity/revision is durable"));
    }
    if !old.is_null(){
        let mut prior=old.clone();let mut next=new.clone();
        prior.as_object_mut().ok_or_else(||internal("Continuous policy invalid"))?.remove("issuedSlots");
        next.as_object_mut().ok_or_else(||internal("Continuous policy invalid"))?.remove("issuedSlots");
        if prior!=next&&new["policyRevision"].as_u64()!=old["policyRevision"].as_u64().and_then(|v|v.checked_add(1)){
            return Err(internal("Continuous owner policy change requires next explicit revision"));
        }
    }
    if before.get("jobs").is_none()&&after.get("jobs").is_none(){if old.get("issuedSlots")!=new.get("issuedSlots"){return Err(internal("Scoped writes cannot change continuous issued slots"));}return Ok(());}
    for old in list(before,"jobs"){
        let Some(job_id)=old["id"].as_str() else{continue;};
        let Some(new)=list(after,"jobs").iter().find(|j|j["id"]==job_id) else{
            if !old[ORIGIN].is_null()||!old[RESERVATIONS].is_null(){return Err(internal("Continuous paid origin/journal cannot disappear"));}
            continue;
        };
        if !old[ORIGIN].is_null()&&(old[ORIGIN]!=new[ORIGIN]||old["workflowMode"]!=new["workflowMode"]
            ||old["purpose"]!=new["purpose"]||parent_id(old)!=parent_id(new)){return Err(internal("Continuous original authority is immutable"));}
        if old[ORIGIN].is_null()&&!new[ORIGIN].is_null(){return Err(internal("Existing jobs cannot acquire continuous authority"));}
        let previous=rows(old,RESERVATIONS);let current=rows(new,RESERVATIONS);
        if current.len()<previous.len(){return Err(internal("Continuous issued slots cannot be refunded"));}
        for (old,new) in previous.iter().zip(current){
            if immutable_reservation(old)!=immutable_reservation(new)||(!old["observation"].is_null()&&old["observation"]!=new["observation"]){
                return Err(internal("Continuous original reservation/observation is immutable"));
            }
        }
    }
    // A native debit is exactly the number of newly appended issued slots.
    // Policy reconfiguration, observation, polling and restart cannot reset it.
    let mut appended=0u64;let mut by_root:BTreeMap<String,u64>=BTreeMap::new();
    for job in list(after,"jobs"){
        let prior=list(before,"jobs").iter().find(|j|j["id"]==job["id"]);
        let old_len=prior.map(|j|rows(j,RESERVATIONS).len()).unwrap_or(0);
        for reservation in rows(job,RESERVATIONS).iter().skip(old_len){
            let count=rows(&reservation["envelope"],"slotIds").len() as u64;
            appended=appended.checked_add(count).ok_or_else(||internal("Continuous debit overflow"))?;
            let root=required(&reservation["envelope"],"originalJobId")?.to_owned();
            let debit=by_root.entry(root).or_default();*debit=debit.checked_add(count).ok_or_else(||internal("Continuous original debit overflow"))?;
        }
    }
    if !new.is_null(){
        let spent=new["issuedSlots"].as_u64().ok_or_else(||internal("Continuous durable spent counter missing"))?;
        if spent!=old["issuedSlots"].as_u64().unwrap_or(0).checked_add(appended).ok_or_else(||internal("Continuous debit overflow"))?
            ||spent>new["invocationLimit"].as_u64().unwrap_or(0){return Err(internal("Continuous issued counter cannot reset/inflate or exceed finite policy"));}
    }else if appended>0{return Err(internal("Continuous issued slots require native policy"));}
    for job in list(after,"jobs").iter().filter(|j|!j[ORIGIN].is_null()){
        let prior=list(before,"jobs").iter().find(|j|j["id"]==job["id"]);
        let count=job["codexInvocationIssuedSlots"].as_u64().ok_or_else(||internal("Continuous original debit counter missing"))?;
        let increment=by_root.get(required(job,"id")?).copied().unwrap_or(0);
        if count!=prior.and_then(|j|j["codexInvocationIssuedSlots"].as_u64()).unwrap_or(0).checked_add(increment).ok_or_else(||internal("Continuous original debit overflow"))?
            ||count>job[ORIGIN]["maxOriginalJobInvocations"].as_u64().unwrap_or(0){return Err(internal("Continuous original debit cannot reset/inflate or exceed original cap"));}
    }
    let mut reservations=BTreeSet::new();let mut slots=BTreeSet::new();
    for job in list(after,"jobs"){
        if job[ORIGIN].is_null()&&!rows(job,RESERVATIONS).is_empty(){return Err(internal("Continuous slots require original native origin"));}
        for reservation in rows(job,RESERVATIONS){
            let envelope=&reservation["envelope"];
            if envelope["nativeJobId"]!=job["id"]||envelope["originalJobId"]!=job[ORIGIN]["originalJobId"]||envelope["policyId"]!=job[ORIGIN]["policyId"]
                ||envelope["requestSha256"].as_str().is_none_or(|h|h.len()!=64||!h.bytes().all(|b|b.is_ascii_hexdigit()&&!b.is_ascii_uppercase()))
                ||!reservations.insert(required(envelope,"reservationId")?.to_owned()){
                return Err(internal("Continuous reservation scope/uniqueness invalid"));
            }
            let ids=envelope["slotIds"].as_array().filter(|v|!v.is_empty()&&v.len()<=MAX_BRIDGE_SLOTS as usize).ok_or_else(||internal("Continuous issued slot bound invalid"))?;
            if envelope["version"]!=1||envelope["contract"]!=BUDGET_CONTRACT||envelope["account"]!=job[ORIGIN]["account"]
                ||envelope["companyId"]!=job[ORIGIN]["companyId"]||envelope["queueEpoch"]!=job[ORIGIN]["queueEpoch"]
                ||envelope["connectorBindingSha256"]!=job[ORIGIN]["connectorBindingSha256"]||envelope["policyRevision"]!=job[ORIGIN]["policyRevision"]{
                return Err(internal("Continuous issued envelope cannot retarget original scope"));
            }
            let reservation_id=required(envelope,"reservationId")?;
            for (index,slot) in ids.iter().enumerate(){
                let slot=slot.as_str().ok_or_else(||internal("Continuous slot id invalid"))?;
                if slot!=format!("{reservation_id}:{}",index+1)||!slots.insert(slot.to_owned()){return Err(internal("Continuous slot reused or not original finite admission"));}
            }
        }
    }
    Ok(())
}
fn update_ready(d:&mut Value,at:&str)->ApiResult<Value>{
    let view=ready(d)?;
    let prior=d["settings"]["autoPreparation"]["continuousProgress"]["readyForOwnerApproval"].clone();
    let progress=&mut d["settings"]["autoPreparation"]["continuousProgress"];
    progress["version"]=json!(1);progress["workflowMode"]=json!(MODE);
    progress["readyForOwnerApproval"]=view["readyForOwnerApproval"].clone();progress["held"]=view["held"].clone();
    if prior!=view["readyForOwnerApproval"]&&!rows(&view,"readyForOwnerApproval").is_empty(){progress["lastReadyAt"]=json!(at);progress["lastProgressAt"]=json!(at);}
    Ok(view)
}
/// Called only after native successful source ingestion, never on a poll heartbeat.
pub(crate) fn record_intake(d:&mut Value,at:&str,observed:u64)->ApiResult<()>{
    let progress=&mut d["settings"]["autoPreparation"]["continuousProgress"];
    progress["version"]=json!(1);progress["lastIntakeAt"]=json!(at);progress["lastIntakeObserved"]=json!(observed);
    if observed>0{progress["lastProgressAt"]=json!(at);}
    Ok(())
}
pub(crate) async fn tick(app:&App)->ApiResult<()>{
    // Disabled/default company reads metadata only. An exhausted enabled policy
    // still reprojects retained results/recovery without new paid admissions.
    let metadata=app.db.read_metadata().await?;
    if !enabled(&metadata){return Ok(());}
    drop(metadata);
    // Reproject individually current retained results even after paid admission stops.
    let at=now();
    app.change(|d|{update_ready(d,&at)?;Ok(())}).await?;
    let snapshot=app.read().await?;
    if !enabled(&snapshot){return Ok(());}
    // Resume the same interrupted journal before considering a fresh admission.
    // Existing run reuses original captured paid units; reserve_bridge forbids
    // replacement of an issued unknown request even after a process restart.
    if let Some(existing)=list(&snapshot,"jobs").iter().find(|j|j["kind"]=="editorial_review"&&j["status"]=="interrupted"&&j[ORIGIN]["kind"]=="continuous_background"){
        let token=app.lifecycle_admission_token(runtime_lifecycle::AdmissionClass::Preparation).await?;
        let job_id=required(existing,"id")?.to_owned();
        app.change(|d|{runtime_lifecycle::require_admission(d,&token,runtime_lifecycle::AdmissionClass::Preparation)?;
            let job=row_mut(d,"jobs",&job_id)?;if job["status"]=="interrupted"{job["status"]=json!("running");}Ok(())}).await?;
        let worker=app.clone();let key=job_id.clone();
        app.spawn_with_completion(job_id,editorial_endpoint::run(worker.clone(),key),move||worker.preparation_wake.notify_one());
        return Ok(());
    }
    let scheduled:BTreeSet<(String,u64)>=list(&snapshot,"jobs").iter().filter(|j|j["kind"]=="editorial_review"&&j[ORIGIN]["kind"]=="continuous_background")
        .flat_map(|j|rows(j,"editorialReferences")).filter_map(|r|Some((r["id"].as_str()?.to_owned(),r["revision"].as_u64()?))).collect();
    let mut groups:BTreeMap<String,Vec<Value>>=BTreeMap::new();
    let context=prepare_bundle::EvidenceContext::new(&snapshot);
    for p in list(&snapshot,"proposals").iter().filter(|p|p["status"]=="draft"){
        let Some(run)=p["prepareRunId"].as_str() else{continue;};
        if !row(&snapshot,"jobs",run).is_ok_and(|j|j[ORIGIN]["kind"]=="continuous_background"){continue;}
        if proposal_current_with_context(p,&context).is_err(){continue;}
        if p["id"].as_str().zip(p["revision"].as_u64()).is_some_and(|(id,revision)|scheduled.contains(&(id.to_owned(),revision))){continue;}
        // This named mode requires a dedicated model review. Reuse exactly that
        // current receipt; do not globally tighten generation-receipt policy.
        if p["kind"]=="reply_and_close"&&(p["editorialReview"]["source"]["kind"]!="dedicated_model_review"
            ||editorial_review::dedicated_current(&context,p).is_err()){
            groups.entry(run.to_owned()).or_default().push(json!({"id":p["id"],"revision":p["revision"]}));
        }
    }
    if groups.is_empty(){return Ok(());}
    let token=app.lifecycle_admission_token(runtime_lifecycle::AdmissionClass::Preparation).await?;
    // One new/resumed child per wake. Held/pending exact units cannot starve an
    // independent following family. Existing jobs retain immutable memberships.
    for (parent,refs) in groups.into_iter().take(64){
        let refs=json!(refs.into_iter().take(MAX_TAIL_REFS).collect::<Vec<_>>());
        let request_id=format!("continuous-editorial-{}",hash(&json!([parent,refs])));
        if admission_reason(&snapshot).is_some(){return Ok(());}
        let body=json!({"requestId":request_id,"proposals":refs,"fresh":true});
        let mut actor=operator_auth::Actor::local_owner("native-continuous-tail");
        // Stable policy actor makes ACK/restart replay independent of the most
        // recent operator session without inventing approval/dispatch authority.
        actor.id=format!("continuous:{}",required(policy(&snapshot),"policyId")?);
        actor.name="Native continuous preparation".into();
        let (_,job)=app.change(|d|{
            runtime_lifecycle::require_admission(d,&token,runtime_lifecycle::AdmissionClass::Preparation)?;
            enabled_policy(d)?;
            if admission_reason(d).is_some(){return Ok((json!({"waiting":"continuous_admission_blocked"}),None));}
            let (result,job)=editorial_endpoint::schedule(d,&actor,&body)?;
            if let Some(id)=&job{
                row_mut(d,"jobs",id)?["continuousParentJobId"]=json!(parent);
                stamp_background_job(d,id,Some(&parent))?;
            }
            Ok((result,job))
        }).await?;
        if let Some(job)=job{
            let worker=app.clone();let key=job.clone();
            app.spawn_with_completion(job,editorial_endpoint::run(worker.clone(),key),move||worker.preparation_wake.notify_one());
            return Ok(());
        }
    }
    Ok(())
}
pub(crate) fn status_view(d:&Value,at:&str,complete:bool)->Value{
    let p=policy(d);let progress=&d["settings"]["autoPreparation"]["continuousProgress"];
    // Source writers already retain this ONLY after an admitted merge commits.
    // No extra settings mutation/full writer is needed for an intake heartbeat.
    let last_intake=[&progress["lastIntakeAt"],&d["sync"]["lastSyncedAt"]].into_iter()
        .filter_map(|value|Some((chrono::DateTime::parse_from_rfc3339(value.as_str()?).ok()?,value)))
        .max_by_key(|(at,_)|*at).map(|(_,value)|value.clone()).unwrap_or(Value::Null);
    let ready_view=if complete{ready(d).ok()}else{None};
    let count=|name:&str|if complete{Some(list(d,name).len())}else{None};
    let (tokens,incomplete)=observed_tokens(d,&p["policyId"]);
    let intake_dependency=connection_gate::preparation_dependency(d,true).unwrap_or_else(|e|json!({"status":"waiting_dependency","reason":e.1,"retryAuthorized":false}));
    let blocked=admission_reason(d);
    let open=complete.then(||list(d,"items").iter().filter(|i|i["workflow"]=="attention").count());
    let elapsed=progress["lastProgressAt"].as_str().and_then(|last|chrono::DateTime::parse_from_rfc3339(last).ok())
        .zip(chrono::DateTime::parse_from_rfc3339(at).ok()).map(|(last,at)|(at-last).num_seconds().max(0));
    let stalled=complete&&enabled(d)&&blocked.is_none()&&open.is_some_and(|n|n>0)&&elapsed.is_some_and(|n|n>300);
    json!({"version":1,"workflowMode":MODE,"enabled":enabled(d),"admissionBlockedReason":blocked,"stalled":stalled,"secondsSinceProgress":elapsed,
        "queueEpoch":d["storageGeneration"],"policyId":p["policyId"],"policyRevision":p["policyRevision"],"invocationUnit":"codex_process_invocation",
        "policyConfiguredAt":p["configuredAt"],"ownerPolicyRequired":!enabled(d),
        "waitingForOwnerApproval":ready_view.as_ref().map(|v|rows(v,"readyForOwnerApproval").len()),
        "invocationPartition":if complete{Some(invocation_partition(d,&p["policyId"]))}else{None},
        "issuedSlots":if complete{Some(issued_total(d,&p["policyId"],None))}else{None},"invocationLimit":p["invocationLimit"],
        "observedTokens":if complete{Some(tokens)}else{None},"tokenObservationIncomplete":incomplete||!complete,"hardTokenCeiling":false,"moneyCeiling":null,"billableWireRequests":null,
        "lastIntakeAt":last_intake,"lastIntakeBasis":"native_committed_source_page_or_explicit_receipt","lastReadyAt":progress["lastReadyAt"],"lastProgressAt":progress["lastProgressAt"],"observedAt":at,
        "backlog":{"coverage":if complete{"complete"}else{"unverified"},"observedItems":count("items"),"observedProposals":count("proposals"),
            "openUnprepared":if complete{Some(list(d,"items").iter().filter(|i|i["workflow"]=="attention").count())}else{None}},
        "readyForOwnerApproval":ready_view.as_ref().map(|v|v["readyForOwnerApproval"].clone()),"held":ready_view.as_ref().map(|v|v["held"].clone()),
        "intakeDependency":intake_dependency,"approvalRequired":true,"dispatchAuthorized":false,"retryAllowed":false})
}
#[cfg(test)]
#[path="continuous_preparation_tests.rs"]
mod tests;
