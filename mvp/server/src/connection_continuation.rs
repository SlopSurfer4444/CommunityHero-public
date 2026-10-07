//! Native-only proof producer and request-independent finite coordinator.
//! JSON cannot construct either trusted protected-read or configured-case type.
use super::*;

fn proof(d:&Value,case:&VerifiedBootstrapCase,read:&ProtectedAvailabilityRead)->ApiResult<Value>{
    case_scope(d,case)?;fresh(read)?;
    let gate=&d[connection_gate::FIELD];
    if gate["state"]!="open"||gate["availability"]!=read.observation.projection
        ||gate["availability"]["state"]!="ready"
        ||gate["availability"]["generation"]!=case.bootstrap["expectedProtectedGeneration"]
        ||gate["owner"]!=owner_value(&case.owner)||gate["connectionBinding"]!=case.bootstrap["connectionBinding"]
        ||gate["availability"]["connectionBinding"]!=gate["connectionBinding"]
        ||d[external_reconciliation::FIELD]!=case.bootstrap["archiveFence"]
        ||!hash(&gate["reopenReceiptSha256"]){return Err(unavailable());}
    external_reconciliation::validate(d)?;
    Ok(json!({"version":1,"kind":"verified-connection-continuation-admission",
        "caseSha256":case.sha256,"reopenReceiptSha256":gate["reopenReceiptSha256"],
        "gateEpoch":gate["gateEpoch"],"owner":gate["owner"],"connectionBinding":gate["connectionBinding"],
        "storageGeneration":working_generation::current(d)?,"protectedGeneration":gate["availability"]["generation"],
        "protectedReceiptSha256":read.observation.protected_receipt_sha256,
        "availabilitySha256":connection_gate::digest(&gate["availability"]),
        "archiveFenceSha256":connection_gate::digest(&d[external_reconciliation::FIELD]),
        "lifecycleReceiptSha256":case.bootstrap["lifecycleReceiptSha256"],
        "archiveRestoreReceiptSha256":case.bootstrap["archiveRestoreReceiptSha256"]}))
}

/// Called in the same writer as the real reopen, using its committed epoch and
/// receipt. A typed consumer validates the stored result before it can commit.
pub(super) fn persist(d:&mut Value,case:&VerifiedBootstrapCase,read:&ProtectedAvailabilityRead)->ApiResult<Value>{
    let value=proof(d,case,read)?;
    d[connection_gate::FIELD]["admittedContinuationProof"]=value.clone();
    if connection_gate::current_continuation_admission(d)?.as_ref()!=Some(&value){return Err(unavailable());}
    Ok(value)
}

/// Entire protected projection, installed archive and root-configured receipts
/// must still match. This reducer changes no gate, lease, intent or audit row.
pub(super) fn replay(d:&Value,case:&VerifiedBootstrapCase,read:&ProtectedAvailabilityRead,unheld:bool)->ApiResult<Option<Value>>{
    case_scope(d,case)?;fresh(read)?;
    let Some(current)=connection_gate::current_continuation_admission(d)? else{return Ok(None);};
    if !unheld||d[connection_gate::FIELD]["availability"]!=read.observation.projection
        ||d[external_reconciliation::FIELD]!=case.bootstrap["archiveFence"]
        ||read.observation.projection["state"]!="ready"
        ||read.observation.projection["generation"]!=case.bootstrap["expectedProtectedGeneration"] {return Ok(None);}
    let expected=proof(d,case,read)?;
    if current!=expected{return Ok(None);}
    Ok(Some(json!({"status":"admitted","gate":d[connection_gate::FIELD],"caseSha256":case.sha256,
        "admittedContinuationProof":current,"replayed":true,"sendGateReopened":false})))
}

pub(super) fn spawn_owned<T,F>(app:&App,future:F)->ApiResult<tokio::sync::oneshot::Receiver<ApiResult<T>>>
where T:Send+'static,F:std::future::Future<Output=ApiResult<T>>+Send+'static {
    let work=app.lifecycle_work.begin(runtime_owned_work::Kind::Preparation)?;
    let task=runtime_lifecycle_app::TaskCount::begin(app.lifecycle_task_count.clone());
    let app=app.clone();let registry=app.lifecycle_work.clone();
    let deadline=tokio::time::Instant::now()+std::time::Duration::from_millis(connection_gate::MAX_DRAIN_MS);
    let (send,receive)=tokio::sync::oneshot::channel();
    tokio::spawn(async move {
        let _task=task;
        let result=runtime_owned_work::with_registry(registry,tokio::time::timeout_at(deadline,future)).await;
        let result=match result {Ok(result)=>result,Err(_)=>{
            dispatch_authority::hold_transport_failure(&app);
            Err(conflict("Connection recovery deadline expired; dispatch remains held and durable continuation requires verified recovery"))
        }};
        // This coordinator owns Rust reconciliation only. Each physical bridge
        // or conductor child retains its separate started-work containment duty.
        work.settled();
        let _=send.send(result);
    });
    Ok(receive)
}
