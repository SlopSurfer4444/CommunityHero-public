//! Connection waits preserve one original campaign. A marker is neither a
//! dispatch permit nor permission to re-evaluate a negative admission.
use crate::*;
use axum::Extension;
use super::{Launch, owned, validate_checkpoint};

const FIELD: &str = "connectionContinuation";
const KIND: &str = "conductor-connection-continuation";
#[path="conductor_connection_journal.rs"]
mod journal;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Reason { Start, Recover, Resume, Wake }

fn invalid() -> ApiError { conflict("Conductor connection continuation is invalid or changed") }
fn exact(value:&Value,keys:&[&str])->bool {
    value.as_object().is_some_and(|fields|fields.len()==keys.len()&&keys.iter().all(|key|fields.contains_key(*key)))
}
fn hash(value:&Value)->bool {value.as_str().is_some_and(|s|s.len()==64&&s.bytes().all(|b|b.is_ascii_digit()||matches!(b,b'a'..=b'f')))}
fn positive(value:&Value)->ApiResult<u64>{value.as_u64().filter(|n|*n>0).ok_or_else(invalid)}
fn id(value:&Value)->ApiResult<&str>{value.as_str().filter(|s|!s.is_empty()&&s.len()<=160&&s.bytes().all(|b|b.is_ascii_alphanumeric()||matches!(b,b'-'|b'_'))).ok_or_else(invalid)}

pub(crate) fn validate_observation(value:&Value)->ApiResult<()> {
    if value["kind"]=="missing"&&exact(value,&["kind"]){return Ok(());}
    if !exact(value,&["kind","gateEpoch","owner","connectionBinding","state","availabilityState"])
        ||value["kind"]!="present"||!matches!(value["state"].as_str(),Some("open"|"closing"|"blocked"))
        ||!matches!(value["availabilityState"].as_str(),Some("ready"|"blocked"|"recovering"|"needs_owner"|"unverified")) {return Err(invalid());}
    positive(&value["gateEpoch"])?;runtime_lifecycle::parse_token(&value["owner"])?;
    ConnectorBinding::from_json(&value["connectionBinding"]).map_err(|_|invalid())?;Ok(())
}
fn validate_negative(value:&Value)->ApiResult<()> {
    if !exact(value,&["kind","requestId","approvalId","payloadHash","evaluationId","receiptSha256"])
        ||value["kind"]!="execute-rejection"||!hash(&value["payloadHash"])||!hash(&value["receiptSha256"]){return Err(invalid());}
    for key in ["requestId","approvalId","evaluationId"]{id(&value[key])?;}Ok(())
}
fn validate_proof(value:&Value)->ApiResult<()> {
    if !exact(value,&["version","kind","caseSha256","reopenReceiptSha256","gateEpoch","owner","connectionBinding","storageGeneration","protectedGeneration","protectedReceiptSha256","availabilitySha256","archiveFenceSha256","lifecycleReceiptSha256","archiveRestoreReceiptSha256"])
        ||value["version"]!=1||value["kind"]!="verified-connection-continuation-admission"{return Err(invalid());}
    for key in ["caseSha256","reopenReceiptSha256","protectedReceiptSha256","availabilitySha256","archiveFenceSha256","lifecycleReceiptSha256","archiveRestoreReceiptSha256"]{if !hash(&value[key]){return Err(invalid());}}
    positive(&value["gateEpoch"])?;positive(&value["protectedGeneration"])?;
    runtime_lifecycle::parse_token(&value["owner"])?;ConnectorBinding::from_json(&value["connectionBinding"]).map_err(|_|invalid())?;
    conductor_authority::workspace_generation(&json!({"storageGeneration":value["storageGeneration"]}))?;Ok(())
}
pub(crate) fn validate_marker(value:&Value,job:&Value)->ApiResult<()> {
    let waiting=value["state"]=="waiting_connection";
    let keys=if waiting {vec!["version","kind","state","runId","priorLeaseGeneration","firstLaunch","gateObservation","reason","dependency"]}
        else {vec!["version","kind","state","runId","priorLeaseGeneration","selectedLeaseGeneration","firstLaunch","acceptedAdmission","dependency"]};
    if job["kind"]!="conductor"||job["conductor"]["mode"]!="execute"
        ||!exact(value,&keys)||value["version"]!=1||value["kind"]!=KIND||value["runId"]!=job["id"]
        ||!value["firstLaunch"].is_boolean()||!matches!(value["state"].as_str(),Some("waiting_connection"|"ready_for_child"|"waiting_owner")){return Err(invalid());}
    let prior=positive(&value["priorLeaseGeneration"])?;let current=positive(&job["conductor"]["leaseGeneration"])?;
    if waiting {
        if current!=prior||!matches!(value["reason"].as_str(),Some("gate_missing"|"gate_unverified"|"gate_closing"|"gate_blocked")){return Err(invalid());}
        if value["firstLaunch"]==true&&(prior!=1||job["conductor"]["childEverStarted"]!=false){return Err(invalid());}
        validate_observation(&value["gateObservation"])?;
        if value["gateObservation"]["kind"]=="present"&&value["gateObservation"]["connectionBinding"]!=job["connectorBinding"]{return Err(invalid());}
    } else {
        let selected=positive(&value["selectedLeaseGeneration"])?;
        let expected=if value["firstLaunch"]==true {if prior!=1{return Err(invalid());}1}else{prior.checked_add(1).ok_or_else(invalid)?};
        if selected!=current||selected!=expected{return Err(invalid());}validate_proof(&value["acceptedAdmission"])?;
        if value["acceptedAdmission"]["connectionBinding"]!=job["connectorBinding"]{return Err(invalid());}
    }
    if !value["dependency"].is_null(){validate_negative(&value["dependency"])?;}
    if value["state"]=="waiting_owner"&&value["dependency"].is_null(){return Err(invalid());}Ok(())
}
fn bound_proof(d:&Value,job:&Value)->ApiResult<Option<Value>> {
    if job["account"]!=d["account"]||job["connectorBinding"]!=active_binding(d)?.to_json(){return Err(invalid());}
    let proof=connection_gate::current_continuation_admission(d)?;
    if proof.as_ref().is_some_and(|proof|proof["connectionBinding"]!=job["connectorBinding"]){return Err(invalid());}
    Ok(proof)
}
pub(crate) fn require_launch_projection(app:&App,d:&Value,job:&Value)->ApiResult<()> {
    dispatch_authority::require_unheld(app)?;
    if retained_backlog(d)?{return Err(conflict("Conductor waits for original lifecycle backlog"));}
    let marker=&job["conductor"][FIELD];validate_marker(marker,job)?;
    if marker["state"]!="ready_for_child"||!marker["dependency"].is_null()
        ||bound_proof(d,job)?.as_ref()!=Some(&marker["acceptedAdmission"]){return Err(invalid());}
    Ok(())
}
pub(crate) async fn require_launch_ready(app:&App,job:&Value)->ApiResult<()> {
    require_launch_projection(app,&app.db.read_metadata().await?,job)
}
pub(crate) fn has_deferred_connection_work(d:&Value)->ApiResult<bool> {
    if list(d,"jobs").iter().filter(|job|job["kind"]=="conductor"&&job["conductor"]["desiredState"]=="running").take(2).count()>1 {return Err(invalid());}
    let mut found=false;
    for job in list(d,"jobs").iter().filter(|job|job["kind"]=="conductor"&&job["conductor"]["mode"]=="execute"
        &&job["conductor"]["desiredState"]=="running"&&!job["conductor"][FIELD].is_null()) {
        validate_marker(&job["conductor"][FIELD],job)?;found=true;
    }
    Ok(found)
}
fn waiting(job:&Value,observation:Value,dependency:Value)->ApiResult<Value> {
    validate_observation(&observation)?;if !dependency.is_null(){validate_negative(&dependency)?;}
    let first=job["conductor"]["childEverStarted"]==false&&job["conductor"]["leaseGeneration"]==1;
    let reason=if observation["kind"]=="missing"{"gate_missing"}else if observation["state"]=="closing"{"gate_closing"}
        else if observation["availabilityState"]=="unverified"||(observation["state"]=="open"&&observation["availabilityState"]=="ready"){"gate_unverified"}else{"gate_blocked"};
    Ok(json!({"version":1,"kind":KIND,"state":"waiting_connection","runId":job["id"],
        "priorLeaseGeneration":job["conductor"]["leaseGeneration"],"firstLaunch":first,
        "gateObservation":observation,"reason":reason,"dependency":dependency}))
}
fn selected(job:&Value,proof:Value,dependency:Value)->ApiResult<Value> {
    validate_proof(&proof)?;let prior=positive(&job["conductor"]["leaseGeneration"])?;
    let first=job["conductor"]["childEverStarted"]==false&&prior==1;
    let generation=if first{1}else{prior.checked_add(1).ok_or_else(invalid)?};
    Ok(json!({"version":1,"kind":KIND,"state":"ready_for_child","runId":job["id"],"priorLeaseGeneration":prior,
        "selectedLeaseGeneration":generation,"firstLaunch":first,"acceptedAdmission":proof,"dependency":dependency}))
}
fn ready_marker(job:&Value,previous:&Value,proof:Value,dependency:Value)->ApiResult<Value> {
    let mut marker=if matches!(previous["state"].as_str(),Some("ready_for_child"|"waiting_owner"))&&previous["acceptedAdmission"]==proof {
        validate_marker(previous,job)?;previous.clone()
    }else{selected(job,proof,dependency.clone())?};
    marker["state"]=json!(if dependency.is_null(){"ready_for_child"}else{"waiting_owner"});
    marker["dependency"]=dependency;Ok(marker)
}
fn retained_backlog(value:&Value)->ApiResult<bool> {
    let backlog=&value["runtimeLifecycle"]["queuedBacklog"];
    if backlog.is_null(){return Ok(false);}
    Ok(!backlog["jobs"].as_array().ok_or_else(invalid)?.is_empty())
}
pub(crate) async fn check_journal(app:&App,job:&Value)->ApiResult<()> {
    let run=required(job,"id")?;if uuid::Uuid::parse_str(run).is_err(){return Err(invalid());}
    let file=app.data.join("conductor").join(run).join("queue.json");
    let data_root=tokio::fs::canonicalize(&app.data).await.map_err(|_|invalid())?;
    for directory in [app.data.join("conductor"),app.data.join("conductor").join(run)] {
        if tokio::fs::try_exists(&directory).await.map_err(|_|invalid())?
            &&!tokio::fs::canonicalize(directory).await.map_err(|_|invalid())?.starts_with(&data_root){return Err(invalid());}
    }
    if !tokio::fs::try_exists(&file).await.map_err(|_|invalid())? {
        if job["conductor"]["childEverStarted"]==false {return Ok(());}return Err(conflict("Conductor checkpoint missing; recovery required"));
    }
    // Existing prepare_child performs canonical directory/link checks before
    // any child launch. Here a valid journal is required before durable claim.
    validate_checkpoint(&file,job,app.port).await.map(|_|())
}
async fn negative_for(app:&App,job:&Value,binding:&Value)->ApiResult<Option<Value>> {
    validate_negative(binding)?;
    let run=required(job,"id")?;let generation=positive(&job["conductor"]["leaseGeneration"])?;
    conductor_authority::check_control(app,job).await?;
    let actor=conductor_authority::actor_from_grant(job)?;
    let receipt=app.db.read_local_admission_receipt("execute",id(&binding["requestId"])?).await?.ok_or_else(invalid)?;
    let ctx=conductor_authority::Context{run_id:run.to_owned(),lease_generation:generation,actor:actor.clone()};
    if receipt["action"]==local_admission::ACTION {
        if receipt["payloadHash"]!=binding["payloadHash"]||receipt["result"]["approvalId"]!=binding["approvalId"]
            ||receipt["conductorRunId"]!=job["id"]{return Err(invalid());}
        let body=json!({"approvalId":binding["approvalId"],"requestId":binding["requestId"]});
        let result=conductor_authority::with_context(ctx,local_admission::replay_committed(app,"execute",&body,&actor)).await?.ok_or_else(invalid)?;
        if result["approvalId"]!=binding["approvalId"]{return Err(invalid());}
        let execution=app.db.read_job(required(&result,"jobId")?).await?.ok_or_else(invalid)?;
        if execution["kind"]!="execute"||execution["refId"]!=binding["approvalId"]{return Err(invalid());}
        conductor_authority::require_prior_attribution(Some(&conductor_authority::Context{run_id:run.to_owned(),lease_generation:generation,actor:conductor_authority::actor_from_grant(job)?}),&execution)?;
        return Ok(None);
    }
    let view=conductor_authority::with_context(ctx,async {local_admission::rejection_view(&receipt,"execute",id(&binding["requestId"])? ,app.account.key(),&actor)}).await?;
    if view["approvalId"]!=binding["approvalId"]||view["payloadHash"]!=binding["payloadHash"]||view["reason"]!="dependency"{return Err(invalid());}
    Ok(Some(json!({"kind":"execute-rejection","requestId":view["requestId"],"approvalId":view["approvalId"],"payloadHash":view["payloadHash"],
        "evaluationId":view["evaluationId"],"receiptSha256":view["receiptSha256"]})))
}

/// Claim registration BEFORE the generation transaction. A duplicate event
/// cannot advance the generation of an already owned supervisor.
pub(crate) async fn continue_run(app:&App,run:&str,reason:Reason)->ApiResult<()> {
    let Some(registration)=conductor_child::claim(app,run) else{return Ok(());};
    let before=app.db.read_job(run).await?.ok_or_else(invalid)?;
    if before["conductor"]["desiredState"]!="running"&&!(reason==Reason::Resume&&before["conductor"]["desiredState"]=="paused") {return Ok(());}
    if before["status"]=="completed"||before["conductor"]["desiredState"]=="revoked" {return Ok(());}
    conductor_authority::check_control(app,&before).await?;check_journal(app,&before).await?;
    let marker=&before["conductor"][FIELD];
    if !marker.is_null(){
        // Pause has already advanced the fence. It preserves the old negative
        // identity but an explicit resume selects a new current lease claim.
        let mut marker_job=before.clone();
        if before["conductor"]["desiredState"]=="paused" {
            marker_job["conductor"]["leaseGeneration"]=if marker["state"]=="waiting_connection"{marker["priorLeaseGeneration"].clone()}else{marker["selectedLeaseGeneration"].clone()};
        }
        validate_marker(marker,&marker_job)?;
    }
    let dependency=if marker["dependency"].is_null(){Value::Null}else{negative_for(app,&before,&marker["dependency"]).await?.unwrap_or(Value::Null)};
    let held=dispatch_authority::require_unheld(app).is_err();
    let _transition=conductor_authority::transition_guard(app,run).await;
    let selected_claim=app.change_job(run,|d|{
        let original=row(d,"jobs",run)?.clone();
        if original["conductor"]["leaseGeneration"]!=before["conductor"]["leaseGeneration"]
            ||original["conductor"]["desiredState"]!=before["conductor"]["desiredState"]
            ||original["conductor"][FIELD]!=before["conductor"][FIELD]{return Err(invalid());}
        conductor_authority::check_workspace_generation(d,&original)?;
        if original["account"]!=d["account"]||original["connectorBinding"]!=active_binding(d)?.to_json(){return Err(invalid());}
        if reason==Reason::Resume&&original["conductor"]["desiredState"]=="paused" {
            row_mut(d,"jobs",run)?["conductor"]["desiredState"]=json!("running");
        }
        if original["conductor"]["mode"]!="execute" {
            let generation=positive(&original["conductor"]["leaseGeneration"])?;
            let generation=if reason==Reason::Recover||original["conductor"]["childEverStarted"]==true||original["conductor"]["desiredState"]=="paused"{generation.checked_add(1).ok_or_else(invalid)?}else{generation};
            let job=row_mut(d,"jobs",run)?;job["conductor"]["leaseGeneration"]=json!(generation);job["status"]=json!("recovering");return Ok(Some(Value::Null));
        }
        let observation=connection_gate::continuation_gate_observation(d)?;
        let proof=if held||dispatch_authority::require_unheld(app).is_err()||retained_backlog(d)?{None}else{bound_proof(d,&original)?};
        let Some(proof)=proof else {
            let marker=waiting(&original,observation,dependency.clone())?;
            let job=row_mut(d,"jobs",run)?;job["conductor"][FIELD]=marker;job["status"]=json!("waiting_dependency");return Ok(None);
        };
        let previous=if original["conductor"]["desiredState"]=="running"{original["conductor"][FIELD].clone()}else{Value::Null};
        let marker=ready_marker(&original,&previous,proof,dependency.clone())?;
        let job=row_mut(d,"jobs",run)?;job["conductor"]["leaseGeneration"]=marker["selectedLeaseGeneration"].clone();
        job["conductor"][FIELD]=marker.clone();job["status"]=json!(if dependency.is_null(){"recovering"}else{"waiting_owner"});
        Ok(if dependency.is_null(){Some(marker)}else{None})
    }).await?;
    drop(_transition);
    if let Some(claim)=selected_claim {conductor_child::spawn_claimed(app,run.to_owned(),registration,claim);}
    Ok(())
}

pub(crate) fn wake_deferred(app:&App)->std::pin::Pin<Box<dyn std::future::Future<Output=ApiResult<()>>+Send+'_>> {
    Box::pin(async move {
        let snapshot=app.read().await?;
        if retained_backlog(&snapshot)?{return Ok(());}
        has_deferred_connection_work(&snapshot)?;
        let runs=list(&snapshot,"jobs").iter().filter(|job|job["kind"]=="conductor"&&job["conductor"]["mode"]=="execute"
            &&job["conductor"]["desiredState"]=="running"&&!job["conductor"][FIELD].is_null())
            .map(|job|required(job,"id").map(str::to_owned)).collect::<ApiResult<Vec<_>>>()?;
        for run in runs {continue_run(app,&run,Reason::Wake).await?;}Ok(())
    })
}
pub(crate) async fn dependency_dto(app:&App,launch:&Launch,negative:Value,always:bool)->ApiResult<Value> {
    let metadata=app.db.read_metadata().await?;let observation=connection_gate::continuation_gate_observation(&metadata)?;
    if !negative.is_null(){validate_negative(&negative)?;}
    if !always&&launch.mode!="execute"{return Ok(Value::Null);}
    let job=app.db.read_job(&launch.run_id).await?.ok_or_else(invalid)?;
    if job["conductor"]["leaseGeneration"]!=launch.lease_generation||job["conductor"]["desiredState"]!="running"
        ||job["connectorBinding"]!=launch.connection_binding{return Err(invalid());}
    if !always&&dispatch_authority::require_unheld(app).is_ok()&&!retained_backlog(&metadata)?
        &&bound_proof(&metadata,&job)?.as_ref()==Some(&launch.continuation_claim["acceptedAdmission"]){return Ok(Value::Null);}
    Ok(json!({"version":1,"kind":"conductor-connection-dependency","runId":launch.run_id,
        "leaseGeneration":launch.lease_generation,"connectionBinding":launch.connection_binding,
        "gateObservation":observation,"executeRejection":negative}))
}
pub(crate) async fn dependency_for_rejection(app:&App,launch:&Launch,approval:&str,request:&str)->ApiResult<Value> {
    let job=app.db.read_job(&launch.run_id).await?.ok_or_else(invalid)?;
    let raw=app.db.read_local_admission_receipt("execute",request).await?.ok_or_else(invalid)?;
    let binding=json!({"kind":"execute-rejection","requestId":request,"approvalId":approval,
        "payloadHash":raw["payloadHash"],"evaluationId":raw["evaluationId"],"receiptSha256":raw["receiptSha256"]});
    let Some(negative)=negative_for(app,&job,&binding).await? else{return Err(conflict("Original execution is already admitted; inspect its exact receipt"));};
    if negative!=binding{return Err(conflict("Conductor dependency receipt is no longer the exact latest rejection"));}
    dependency_dto(app,launch,negative,true).await
}
pub(crate) async fn defer_child(app:&App,launch:&Launch,value:&Value)->ApiResult<()> {
    defer(app,launch,value,true).await
}
pub(crate) async fn defer_before_process(app:&App,launch:&Launch,value:&Value)->ApiResult<()> {
    if !value["executeRejection"].is_null(){return Err(invalid());}
    defer(app,launch,value,false).await
}
async fn defer(app:&App,launch:&Launch,value:&Value,saved_by_child:bool)->ApiResult<()> {
    if !exact(value,&["version","kind","runId","leaseGeneration","connectionBinding","gateObservation","executeRejection"])
        ||value["version"]!=1||value["kind"]!="conductor-connection-dependency"||value["runId"]!=launch.run_id
        ||value["leaseGeneration"]!=launch.lease_generation||value["connectionBinding"]!=launch.connection_binding||launch.mode!="execute"{return Err(invalid());}
    validate_observation(&value["gateObservation"])?;
    let before=app.db.read_job(&launch.run_id).await?.ok_or_else(invalid)?;
    conductor_authority::check_control(app,&before).await?;
    check_journal(app,&before).await?;
    journal::verify(app,&before,&launch.checkpoint_path,if saved_by_child{Some(value)}else{None},None).await?;
    let negative=if value["executeRejection"].is_null(){Value::Null}else{
        let latest=negative_for(app,&before,&value["executeRejection"]).await?.ok_or_else(invalid)?;
        if latest!=value["executeRejection"]{return Err(invalid());}latest
    };
    let _transition=conductor_authority::transition_guard(app,&launch.run_id).await;
    app.change_job(&launch.run_id,|d|{
        let original=row(d,"jobs",&launch.run_id)?.clone();
        if original["conductor"]["desiredState"]!="running"||original["conductor"]["leaseGeneration"]!=launch.lease_generation{return Ok(());}
        conductor_authority::check_workspace_generation(d,&original)?;
        if original["account"]!=d["account"]||original["connectorBinding"]!=active_binding(d)?.to_json(){return Err(invalid());}
        let observation=connection_gate::continuation_gate_observation(d)?;
        let proof=if dispatch_authority::require_unheld(app).is_ok()&&!retained_backlog(d)?{bound_proof(d,&original)?}else{None};
        let mut marker=if proof.as_ref()==Some(&launch.continuation_claim["acceptedAdmission"]) {
            ready_marker(&original,&launch.continuation_claim,proof.unwrap(),negative.clone())?
        }else{waiting(&original,observation,negative.clone())?};
        // A ready read with no negative is legitimate only as the original
        // selected claim's close/reopen race; it never chooses a new key.
        validate_marker(&marker,&original)?;
        let status=if marker["state"]=="waiting_owner"{"waiting_owner"}else{"waiting_dependency"};
        let job=row_mut(d,"jobs",&launch.run_id)?;job["conductor"][FIELD]=marker.take();job["status"]=json!(status);Ok(())
    }).await
}

/// Explicit operator action supplies latest negative identity, never Context.
pub(crate) async fn execute_reevaluate(State(app):State<App>,Extension(actor):Extension<operator_auth::Actor>,Path(run):Path<String>,Json(body):Json<Value>)->ApiResult<Json<Value>> {
    // The request owns neither the continuation nor the commit-to-wake gap.
    // A canceled response receiver cannot cancel this native coordinator.
    let registration=conductor_child::claim(&app,&run).ok_or_else(||conflict("Conductor continuation is already owned"))?;
    let work=app.lifecycle_work.begin(runtime_owned_work::Kind::Preparation)?;
    let task=runtime_lifecycle_app::TaskCount::begin(app.lifecycle_task_count.clone());
    let (sender,receiver)=tokio::sync::oneshot::channel();
    tokio::spawn(async move {
        let _task=task;
        let admitted=std::sync::atomic::AtomicBool::new(false);
        let result=runtime_owned_work::with_registry(app.lifecycle_work.clone(),
            tokio::time::timeout(Duration::from_secs(120),reevaluate_owned(&app,&actor,&run,&body,&admitted))).await
            .unwrap_or_else(|_|Err(conflict("Conductor reevaluation response unresolved; inspect the original admission")));
        drop(registration);work.settled();
        // Read-only reconciliation accepts only actual same-key native evidence.
        // A second negative is kept waiting_owner; no automatic reevaluation.
        if admitted.load(std::sync::atomic::Ordering::Acquire){let _=tokio::time::timeout(Duration::from_secs(15),wake_deferred(&app)).await;}
        let _=sender.send(result);
    });
    receiver.await.map_err(|_|internal("Conductor reevaluation response unavailable; inspect the original admission"))?
}
async fn reevaluate_owned(app:&App,actor:&operator_auth::Actor,run:&str,body:&Value,admitted:&std::sync::atomic::AtomicBool)->ApiResult<Json<Value>> {
    if !exact(&body,&["approvalId","requestId","reevaluate"])||!exact(&body["reevaluate"],&["evaluationId","receiptSha256"]){return Err(bad("Conductor reevaluation requires only the exact original admission and latest evaluation"));}
    for key in ["approvalId","requestId"]{id(&body[key])?;}id(&body["reevaluate"]["evaluationId"])?;if !hash(&body["reevaluate"]["receiptSha256"]){return Err(invalid());}
    let job=app.db.read_job(run).await?.ok_or_else(invalid)?;owned(&job,actor)?;
    let generation=positive(&job["conductor"]["leaseGeneration"])?;
    let original_actor=conductor_authority::authorize(app,run,generation,"read",&[]).await?;
    if actor.id!=original_actor.id||actor.role!=original_actor.role
        ||dispatch_authority::approval_binding(&actor)!=dispatch_authority::approval_binding(&original_actor)
        ||job["conductor"]["mode"]!="execute"||job["conductor"][FIELD]["state"]!="waiting_owner"{return Err(invalid());}
    validate_marker(&job["conductor"][FIELD],&job)?;check_journal(&app,&job).await?;
    let dependency=&job["conductor"][FIELD]["dependency"];
    if dependency["approvalId"]!=body["approvalId"]||dependency["requestId"]!=body["requestId"]
        ||dependency["evaluationId"]!=body["reevaluate"]["evaluationId"]||dependency["receiptSha256"]!=body["reevaluate"]["receiptSha256"]{return Err(invalid());}
    let latest=negative_for(&app,&job,dependency).await?.ok_or_else(invalid)?;if latest!=*dependency{return Err(invalid());}
    let metadata=app.db.read_metadata().await?;dispatch_authority::require_unheld(&app)?;
    let proof=connection_gate::current_continuation_admission(&metadata)?.ok_or_else(invalid)?;
    if proof!=job["conductor"][FIELD]["acceptedAdmission"]{return Err(invalid());}
    let canonical=app.read().await?;let approval=row(&canonical,"approvals",id(&body["approvalId"] )?)?;
    let journal_path=tokio::fs::canonicalize(app.data.join("conductor").join(run).join("queue.json")).await.map_err(|_|invalid())?;
    journal::verify(app,&job,&journal_path,Some(&json!({"executeRejection":dependency})),Some(&canonical)).await?;
    let ctx=conductor_authority::Context{run_id:run.to_owned(),lease_generation:generation,actor:original_actor.clone()};
    conductor_authority::require_prior_attribution(Some(&ctx),approval)?;
    let targets=approval["proposals"].as_array().filter(|refs|!refs.is_empty()&&refs.len()<=100).ok_or_else(invalid)?
        .iter().map(|reference|row(&canonical,"proposals",required(reference,"id")?).map(|proposal|proposal["itemId"].clone())).collect::<ApiResult<Vec<_>>>()?;
    conductor_authority::authorize(app,run,generation,"execute",&targets).await?;
    admitted.store(true,std::sync::atomic::Ordering::Release);
    conductor_authority::with_context(ctx,execute_admission::run(app.clone(),original_actor,id(&body["approvalId"] )?.to_owned(),
        json!({"requestId":body["requestId"],"reevaluate":body["reevaluate"]}))).await
}

#[cfg(test)]
#[path="conductor_connection_continuation_tests.rs"]
mod tests;
