//! Memory/isolation fixtures exercise native reducers and task ownership only;
//! they are not protected-registry, provider or original-release receipts.
use super::*;

fn signed_projection(binding:&Value)->Value {
    let mut value=json!({"version":1,"account":"baw-russia","connectionBinding":binding,"state":"ready","generation":321,
        "phase":"committed","reason":"owner_session_committed","canonicalCaseId":"11111111-1111-4111-8111-111111111111",
        "grantSpent":true,"candidatePresent":true,"scopeVerified":true,"parentCount":2,"sendGateOpen":false,
        "retryOriginalOperation":false,"socialRequests":0});
    resign(&mut value);value
}
fn resign(value:&mut Value){value.as_object_mut().unwrap().remove("receiptSha256");
    value["receiptSha256"]=json!(format!("{:x}",Sha256::digest(canonical_json(value).as_bytes())));}
fn read(value:Value)->ProtectedAvailabilityRead {
    ProtectedAvailabilityRead{observation:parse_protected(value).unwrap(),observed:std::time::Instant::now()}
}
fn fixture()->(Value,VerifiedBootstrapCase,ProtectedAvailabilityRead) {
    let mut d=empty();accounts::initialize(&mut d,accounts::Profile::BawRussia).unwrap();
    d["storageGeneration"]=json!("11111111-1111-4111-8111-111111111111");
    d["runtimeLifecycle"]=json!({"schemaVersion":1,"owner":{"account":"BAW Russia","runtimeId":"synthetic-continuation",
        "releaseSha256":"a".repeat(64),"epoch":1},"phase":"running","history":[],"target":null,
        "transfer":null,"mediaAnalysisGeneration":1,"queuedBacklog":null});
    let binding=active_binding(&d).unwrap().to_json();
    let archive=json!({"mode":"archive_fence","epoch":1,"safetyFence":{"companyId":"baw-russia","connectionScope":binding,
        "archiveManifestHash":"c".repeat(64),"sourceBindingRefs":[{"sourceSha256":"b".repeat(64),"archiveManifestHash":"c".repeat(64)}],
        "predicate":"all_retained_external_aliases","coverage":{"complete":true,"declaredRows":1},"sealedAt":"2026-10-07T00:00:00Z"},
        "sealedSourceRefs":[{"sha256":"b".repeat(64)}],"recipientReservations":[],
        "quarantineCoverage":{"complete":true,"namespaces":[]},"archiveRefs":[{"sha256":"c".repeat(64)}]});
    let case=parse_case(json!({"kind":"communityhero-clean-start-case.v1","bootstrap":{"version":1,
        "companyId":"baw-russia","connectionBinding":binding,"expectedStorageGeneration":d["storageGeneration"],
        "owner":d["runtimeLifecycle"]["owner"],"lifecycleReceiptSha256":"d".repeat(64),
        "predecessorContainmentReceiptSha256":"e".repeat(64),"archiveRestoreReceiptSha256":"f".repeat(64),
        "mutationPermitCap":1,"drainDeadlineMs":1000,"archiveFence":archive,"expectedProtectedGeneration":321}}),
        "a".repeat(64),PathBuf::from("synthetic-memory-case")).unwrap();
    let read=read(signed_projection(&binding));
    connection_gate::initialize_closed(&mut d,"e".repeat(64).as_str(),1,1000,true).unwrap();
    connection_gate::request_close(&mut d,"fixture-continuation","owner_recovery").unwrap();
    connection_gate::finalize_close(&mut d,"fixture-continuation").unwrap();
    let epoch=d[connection_gate::FIELD]["gateEpoch"].as_u64().unwrap();
    external_reconciliation::install(&mut d,&case.bootstrap["archiveFence"],epoch).unwrap();
    let availability=connection_gate::observe_availability(&mut d,&read.observation).unwrap();
    connection_gate::reopen(&mut d,&json!({"expectedGateEpoch":epoch,"owner":owner_value(&case.owner),
        "connectionBinding":binding,"availability":availability,"lifecycleReceiptSha256":case.bootstrap["lifecycleReceiptSha256"],
        "caseSha256":case.sha256,"archiveRestoreReceiptSha256":case.bootstrap["archiveRestoreReceiptSha256"]})).unwrap();
    continuation::persist(&mut d,&case,&read).unwrap();(d,case,read)
}

#[test]
fn actual_reopen_produces_exact_current_continuation_proof() {
    let (d,case,read)=fixture();let proof=&d[connection_gate::FIELD]["admittedContinuationProof"];
    assert_eq!(proof.as_object().unwrap().len(),14);
    assert_eq!(proof["owner"]["account"],"BAW Russia");assert_eq!(proof["caseSha256"],case.sha256);
    assert_eq!(proof["reopenReceiptSha256"],d[connection_gate::FIELD]["reopenReceiptSha256"]);
    assert_eq!(proof["gateEpoch"],d[connection_gate::FIELD]["gateEpoch"]);
    assert_eq!(proof["protectedReceiptSha256"],read.observation.protected_receipt_sha256);
    assert_eq!(proof["availabilitySha256"],connection_gate::digest(&read.observation.projection));
    assert_eq!(proof["archiveFenceSha256"],connection_gate::digest(&d[external_reconciliation::FIELD]));
    assert_eq!(connection_gate::current_continuation_admission(&d).unwrap().as_ref(),Some(proof));
}

#[test]
fn configured_generation_requires_canonical_uuidv4_before_any_gate_write() {
    let (_,case,_)=fixture();
    for generation in ["unadmitted-workspace","11111111-1111-5111-8111-111111111111",
        "A1111111-1111-4111-8111-111111111111"] {
        let mut value=json!({"kind":"communityhero-clean-start-case.v1","bootstrap":case.bootstrap});
        value["bootstrap"]["expectedStorageGeneration"]=json!(generation);
        assert!(parse_case(value,"a".repeat(64),PathBuf::from("synthetic-memory-case")).is_err());
    }
}

#[test]
fn exact_repeat_preserves_active_permit_original_lease_and_all_metadata() {
    let (mut d,case,read)=fixture();let op=connection_gate::tests::operation(&d,1);
    list_mut(&mut d,"operations").push(op.clone());connection_gate::prearm(&mut d,&op).unwrap();
    d["jobs"]=json!([{"id":"original-run","kind":"conductor","status":"running",
        "conductor":{"mode":"execute","desiredState":"running","leaseGeneration":42,"childEverStarted":true}}]);
    let before=d.clone();
    for _ in 0..3 {let result=continuation::replay(&d,&case,&read,true).unwrap().unwrap();
        assert_eq!(result["replayed"],true);assert_eq!(result["sendGateReopened"],false);assert_eq!(d,before);}
    assert!(!conductor::has_deferred_connection_work(&d).unwrap(),"ordinary active child has no deferred marker");
    assert_eq!(d["jobs"][0]["conductor"]["leaseGeneration"],42);
    assert_eq!(d["operations"][0][connection_gate::PERMIT_FIELD]["phase"],"dispatch_armed");
}

#[test]
fn fresh_entire_projection_and_root_receipts_are_required_for_repeat() {
    let (d,case,original)=fixture();
    let mut foreign_binding=original.observation.projection["connectionBinding"].clone();foreign_binding["revision"]=json!(2);
    for (field,value) in [("generation",json!(322)),("reason",json!("owner_session_verified")),
        ("connectionBinding",foreign_binding),("account",json!("likeavto"))] {
        let mut changed=original.observation.projection.clone();changed[field]=value;resign(&mut changed);
        assert!(continuation::replay(&d,&case,&read(changed),true).unwrap().is_none(),"changed {field}");
    }
    for key in ["lifecycleReceiptSha256","archiveRestoreReceiptSha256"] {
        let mut changed=VerifiedBootstrapCase{sha256:case.sha256.clone(),path:case.path.clone(),
            bootstrap:case.bootstrap.clone(),owner:runtime_lifecycle::parse_token(&owner_value(&case.owner)).unwrap()};
        changed.bootstrap[key]=json!("9".repeat(64));
        assert!(continuation::replay(&d,&changed,&original,true).unwrap().is_none(),"configured {key}");
    }
    let changed=VerifiedBootstrapCase{sha256:"9".repeat(64),path:case.path.clone(),bootstrap:case.bootstrap.clone(),
        owner:runtime_lifecycle::parse_token(&owner_value(&case.owner)).unwrap()};
    assert!(continuation::replay(&d,&changed,&original,true).unwrap().is_none());
    assert!(continuation::replay(&d,&case,&original,false).unwrap().is_none(),"local hold cannot be cleared by replay");
    let stale=ProtectedAvailabilityRead{observation:parse_protected(original.observation.projection.clone()).unwrap(),
        observed:std::time::Instant::now()-std::time::Duration::from_secs(31)};
    assert!(continuation::replay(&d,&case,&stale,true).is_err());
}

#[test]
fn same_ready_state_cannot_substitute_for_current_proof_or_archive() {
    let (d,case,read)=fixture();
    for field in ["protectedReceiptSha256","availabilitySha256","archiveFenceSha256","reopenReceiptSha256",
        "caseSha256","lifecycleReceiptSha256","archiveRestoreReceiptSha256"] {
        let mut changed=d.clone();changed[connection_gate::FIELD]["admittedContinuationProof"][field]=json!("9".repeat(64));
        assert!(continuation::replay(&changed,&case,&read,true).unwrap().is_none(),"changed {field}");
    }
    let mut changed=d.clone();changed[external_reconciliation::FIELD]["safetyFence"]["sealedAt"]=json!("2026-10-07T00:00:01Z");
    assert!(continuation::replay(&changed,&case,&read,true).unwrap().is_none());
    let mut changed=d.clone();changed[connection_gate::FIELD].as_object_mut().unwrap().remove("admittedContinuationProof");
    assert!(continuation::replay(&changed,&case,&read,true).unwrap().is_none());
    let mut changed=d;changed[connection_gate::FIELD]["admittedContinuationProof"]["ready"]=json!(true);
    assert!(continuation::replay(&changed,&case,&read,true).is_err(),"malformed proof fails closed");
}

#[test]
fn foreign_owner_workspace_and_closed_gate_never_replay() {
    let (d,case,read)=fixture();
    let mut changed=d.clone();changed["storageGeneration"]=json!("22222222-2222-4222-8222-222222222222");
    assert!(continuation::replay(&changed,&case,&read,true).is_err());
    let mut changed=d.clone();changed["runtimeLifecycle"]["owner"]["runtimeId"]=json!("different-owner");
    assert!(continuation::replay(&changed,&case,&read,true).is_err());
    let mut changed=d;connection_gate::request_close(&mut changed,"closed-again","auth_unavailable").unwrap();
    assert!(continuation::replay(&changed,&case,&read,true).unwrap().is_none());
}

#[tokio::test]
async fn owned_recovery_outlives_cancelled_response_and_settles_after_its_work() {
    let (app,_temp)=crate::tests::test_app().await;
    let (go_send,go_receive)=tokio::sync::oneshot::channel();
    let (done_send,done_receive)=tokio::sync::oneshot::channel();
    let response=continuation::spawn_owned(&app,async move {
        go_receive.await.unwrap();let _=done_send.send(());Ok(json!({"done":true}))
    }).unwrap();
    assert_eq!(app.lifecycle_work.snapshot().unwrap().active,1);
    assert_eq!(app.lifecycle_task_count.load(std::sync::atomic::Ordering::SeqCst),1);
    drop(response);go_send.send(()).unwrap();done_receive.await.unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(2),async {
        while app.lifecycle_task_count.load(std::sync::atomic::Ordering::SeqCst)!=0 {tokio::task::yield_now().await;}
    }).await.unwrap();
    let snapshot=app.lifecycle_work.snapshot().unwrap();assert_eq!(snapshot.active,0);assert_eq!(snapshot.unresolved,0);
}

#[tokio::test]
async fn runtime_drain_rejects_coordinator_before_any_future_poll() {
    let (app,_temp)=crate::tests::test_app().await;let token=app.lifecycle_work.close().unwrap();
    let polled=std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));let observed=polled.clone();
    assert!(continuation::spawn_owned(&app,async move {observed.store(true,std::sync::atomic::Ordering::SeqCst);Ok(())}).is_err());
    assert!(!polled.load(std::sync::atomic::Ordering::SeqCst));assert_eq!(app.lifecycle_work.snapshot().unwrap().active,0);
    assert_eq!(app.lifecycle_task_count.load(std::sync::atomic::Ordering::SeqCst),0);
    app.lifecycle_work.resume(&token).unwrap();
}
