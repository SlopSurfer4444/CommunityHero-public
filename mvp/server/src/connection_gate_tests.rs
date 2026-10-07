use super::*;

pub(crate) fn workspace()->Value {
    let mut d=empty();d["account"]=json!("LikeAvto");d["connectorBinding"]=legacy_binding();
    d["runtimeLifecycle"]=json!({"schemaVersion":1,"owner":{"account":"LikeAvto","runtimeId":"synthetic-runtime",
        "releaseSha256":"a".repeat(64),"epoch":1},"phase":"running","history":[],"target":null,
        "transfer":null,"mediaAnalysisGeneration":1,"queuedBacklog":null});
    fixture_open(&mut d).unwrap();d
}
pub(crate) fn operation(d:&Value,n:usize)->Value {
    let target=json!({"id":format!("local-{n}"),"itemId":format!("external-{n}"),"objectId":"object",
        "postKey":"post","conversationKey":"thread","revision":1,"connectorBinding":active_binding(d).unwrap().to_json()});
    json!({"id":format!("operation-{n}"),"attemptId":format!("attempt-{n}"),"status":"dispatching",
        "action":{"action":"reply_and_close","actionId":format!("action-{n}"),"itemId":format!("external-{n}")},
        "target":target,"dispatchAuthority":{"synthetic":true}})
}
fn add(d:&mut Value,n:usize)->Value {let op=operation(d,n);list_mut(d,"operations").push(op.clone());op}
pub(crate) fn witness(permit:&Value,kind:CessationKind)->TransportCessation {
    TransportCessation{operation_id:permit["operationId"].as_str().unwrap().into(),attempt_id:permit["attemptId"].as_str().unwrap().into(),
        permit_id:permit["id"].as_str().unwrap().into(),owner:permit["owner"].clone(),evidence_sha256:"b".repeat(64),kind}
}

#[test]
fn ordinary_and_conductor_share_the_same_prearmed_finite_cohort() {
    let mut d=workspace();let mut ops=vec![];
    for n in 0..100 {ops.push(add(&mut d,n));}
    for op in ops.iter().take(4){prearm(&mut d,op).unwrap();}
    assert!(prearm(&mut d,&ops[4]).is_err());
    let intent=request_close(&mut d,"shared-auth-failure","auth_unavailable").unwrap();
    assert_eq!(intent["cohortCount"],4);
    for op in ops.iter().skip(4){assert!(prearm(&mut d,op).is_err());}
    assert!(finalize_close(&mut d,"shared-auth-failure").is_err());
    assert_eq!(d[FIELD]["state"],"closing");
    assert_eq!(list(&d,"operations").iter().filter(|op|capable(op)).count(),4);
}

#[test]
fn prearm_before_enqueue_crash_is_never_no_attempt_and_restart_stays_closed() {
    let mut d=workspace();let op=add(&mut d,1);let permit=prearm(&mut d,&op).unwrap();
    assert_eq!(permit["providerAttemptObserved"],false);
    restart_closed(&mut d).unwrap();
    assert_eq!(row(&d,"operations","operation-1").unwrap()[PERMIT_FIELD]["phase"],"dispatch_armed");
    assert_eq!(row(&d,"operations","operation-1").unwrap()["status"],"dispatching");
    assert!(prearm(&mut d,&op).is_err());
    request_close(&mut d,"restart-close","restart").unwrap();
    restart_closed(&mut d).unwrap();assert_eq!(d[FIELD]["state"],"closing");
    assert!(finalize_close(&mut d,"restart-close").is_err());
}

#[test]
fn containment_retires_transport_but_preserves_original_remote_uncertainty() {
    let mut d=workspace();let op=add(&mut d,1);let permit=prearm(&mut d,&op).unwrap();
    request_close(&mut d,"drain","handoff").unwrap();
    settle(&mut d,&witness(&permit,CessationKind::Contained)).unwrap();
    assert_eq!(row(&d,"operations","operation-1").unwrap()["status"],"dispatching");
    let receipt=finalize_close(&mut d,"drain").unwrap();
    assert_eq!(receipt["providerCapablePermits"],0);
    assert_eq!(finalize_close(&mut d,"drain").unwrap(),receipt,"ACK loss recovers same barrier");
    assert!(prearm(&mut d,&op).is_err());
}

#[test]
fn final_barrier_rejects_missing_captured_row_and_changed_witness_or_intent() {
    let mut d=workspace();let op=add(&mut d,1);let permit=prearm(&mut d,&op).unwrap();
    request_close(&mut d,"drain","handoff").unwrap();
    assert!(request_close(&mut d,"drain","different").is_err());
    let mut wrong=witness(&permit,CessationKind::Returned);wrong.attempt_id="other-attempt".into();
    assert!(settle(&mut d,&wrong).is_err());
    list_mut(&mut d,"operations").clear();
    assert!(finalize_close(&mut d,"drain").is_err(),"projection omission cannot mean cohort0");
}

#[test]
fn malformed_permit_or_rewritten_captured_cohort_cannot_prove_zero() {
    let mut d=workspace();let op=add(&mut d,1);let permit=prearm(&mut d,&op).unwrap();
    request_close(&mut d,"drain-integrity","handoff").unwrap();
    let mut corrupt=d.clone();corrupt["operations"][0][PERMIT_FIELD]["phase"]=json!("missing_or_unknown");
    assert!(finalize_close(&mut corrupt,"drain-integrity").is_err());
    let mut corrupt=d.clone();corrupt[FIELD]["cohort"]=json!([]);
    assert!(finalize_close(&mut corrupt,"drain-integrity").is_err(),"captured cohort digest binds all pre-existing permits");
    settle(&mut d,&witness(&permit,CessationKind::Returned)).unwrap();
    d["operations"][0][PERMIT_FIELD]["gateEpoch"]=json!(999);
    assert!(finalize_close(&mut d,"drain-integrity").is_err(),"original captured permit identity stays exact after cessation");
}
#[test]
fn phase_flip_without_exact_native_cessation_receipt_cannot_remove_a_capable_permit() {
    let mut d=workspace();let op=add(&mut d,1);let permit=prearm(&mut d,&op).unwrap();
    request_close(&mut d,"receipt-integrity","handoff").unwrap();
    let armed=d.clone();
    d["operations"][0][PERMIT_FIELD]["phase"]=json!("transport_settled");
    assert!(!valid_permit(&d["operations"][0]));assert!(validate_change(&armed,&d).is_err());
    assert!(finalize_close(&mut d,"receipt-integrity").is_err());
    d=armed.clone();settle(&mut d,&witness(&permit,CessationKind::Returned)).unwrap();
    assert!(valid_permit(&d["operations"][0]));
    for field in ["kind","evidenceSha256","owner","operationId","attemptId","permitId"] {
        let mut corrupt=d.clone();corrupt["operations"][0][PERMIT_FIELD]["cessation"].as_object_mut().unwrap().remove(field);
        assert!(!valid_permit(&corrupt["operations"][0]),"{field}");assert!(finalize_close(&mut corrupt,"receipt-integrity").is_err());
    }
    for (field,value) in [("kind",json!("timeout")),("evidenceSha256",json!("not-a-hash")),
        ("owner",json!({"foreign":true})),("operationId",json!("different")),("attemptId",json!("different")),("permitId",json!("different"))] {
        let mut corrupt=d.clone();corrupt["operations"][0][PERMIT_FIELD]["cessation"][field]=value;
        assert!(!valid_permit(&corrupt["operations"][0]),"{field}");
    }
    d["operations"][0][PERMIT_FIELD]["settledAt"]=json!("missing-time");assert!(!valid_permit(&d["operations"][0]));
}
#[test]
fn incomplete_native_identity_cannot_be_omitted_as_settled_or_finalized_as_zero() {
    let mut d=workspace();let op=add(&mut d,1);let permit=prearm(&mut d,&op).unwrap();
    request_close(&mut d,"complete-native-identity","handoff").unwrap();
    let armed=d.clone();settle(&mut d,&witness(&permit,CessationKind::Returned)).unwrap();
    let settled=d.clone();
    for original in [&armed,&settled] {
        assert!(valid_permit(&original["operations"][0]));
        for field in PERMIT_IDENTITY_FIELDS.iter().copied().chain(std::iter::once("phase")) {
            let mut corrupt=original.clone();corrupt["operations"][0][PERMIT_FIELD].as_object_mut().unwrap().remove(field);
            assert!(!valid_permit(&corrupt["operations"][0]),"missing native {field}");
            assert!(validate_change(original,&corrupt).is_err(),"missing native {field}");
            assert!(finalize_close(&mut corrupt,"complete-native-identity").is_err(),"missing native {field}");
            assert!(settle(&mut corrupt,&witness(&permit,CessationKind::Returned)).is_err(),"missing native {field}");
        }
    }
    let reject=|op:Value| {
        assert!(!valid_permit(&op));let mut corrupt=settled.clone();corrupt["operations"][0]=op;
        assert!(validate_change(&settled,&corrupt).is_err());
        assert!(finalize_close(&mut corrupt,"complete-native-identity").is_err());
    };
    // Equality of two corrupt owner objects is not a parsed native identity.
    for owner in [json!({}),json!({"account":"LikeAvto","runtimeId":"synthetic-runtime","releaseSha256":"a".repeat(64)}),
        json!({"account":"LikeAvto","runtimeId":"synthetic-runtime","releaseSha256":"a".repeat(64),"epoch":0}),
        json!({"account":"LikeAvto","runtimeId":"bad runtime","releaseSha256":"a".repeat(64),"epoch":1}),
        json!({"account":"LikeAvto","runtimeId":"synthetic-runtime","releaseSha256":"A".repeat(64),"epoch":1}),
        json!({"account":"LikeAvto","runtimeId":"synthetic-runtime","releaseSha256":"a".repeat(64),"epoch":1,"extra":true})] {
        let mut corrupt=settled["operations"][0].clone();corrupt[PERMIT_FIELD]["owner"]=owner.clone();
        corrupt[PERMIT_FIELD]["cessation"]["owner"]=owner;reject(corrupt);
    }
    for (field,value) in [("account",json!("baw-russia")),("account",json!("LikeAvto")),
        ("armedAt",json!("not-a-native-time")),("providerAttemptObserved",json!(true)),
        ("providerAttemptObserved",Value::Null),("archiveFenceSha256",json!("invalid-hash"))] {
        let mut corrupt=settled["operations"][0].clone();corrupt[PERMIT_FIELD][field]=value;reject(corrupt);
    }
    for field in ["id","workspaceId","accountId","connector","revision","providerAccountId"] {
        let mut corrupt=settled["operations"][0].clone();
        corrupt[PERMIT_FIELD]["connectionBinding"].as_object_mut().unwrap().remove(field);
        corrupt["target"]["connectorBinding"]=corrupt[PERMIT_FIELD]["connectionBinding"].clone();reject(corrupt);
    }
    for (field,value) in [("workspaceId",json!("foreign-workspace")),("accountId",json!("BAW Russia")),
        ("connector",json!("unknown-connector")),("revision",json!(0))] {
        let mut corrupt=settled["operations"][0].clone();corrupt[PERMIT_FIELD]["connectionBinding"][field]=value;
        corrupt["target"]["connectorBinding"]=corrupt[PERMIT_FIELD]["connectionBinding"].clone();reject(corrupt);
    }
    let mut corrupt=settled["operations"][0].clone();corrupt[PERMIT_FIELD]["connectionBinding"]["extra"]=json!(true);
    corrupt["target"]["connectorBinding"]=corrupt[PERMIT_FIELD]["connectionBinding"].clone();reject(corrupt);
    let mut corrupt=settled["operations"][0].clone();corrupt["target"]["connectorBinding"]["revision"]=json!(2);reject(corrupt);
    let mut corrupt=settled["operations"][0].clone();corrupt[PERMIT_FIELD]["armedAt"]=json!("2026-10-07T01:00:01+00:00");
    corrupt[PERMIT_FIELD]["settledAt"]=json!("2026-10-07T01:00:00+00:00");reject(corrupt);
    let mut augmented=settled["operations"][0].clone();augmented[PERMIT_FIELD]["retainedExtension"]=json!({"original":true});
    assert!(valid_permit(&augmented),"unknown top-level permit fields remain preserved");
    augmented[PERMIT_FIELD]["archiveFenceSha256"]=json!("c".repeat(64));assert!(valid_permit(&augmented));
    augmented[PERMIT_FIELD]["armedAt"]=json!("2026-10-07T04:00:00+03:00");
    augmented[PERMIT_FIELD]["settledAt"]=json!("2026-10-07T01:00:01Z");assert!(valid_permit(&augmented));
}
#[tokio::test]
async fn existing_closing_deadline_bounds_mutex_wait_and_keeps_original_permit_armed() {
    let (app,_temp)=crate::tests::test_app().await;
    app.change(|d| {
        initialize_closed(d,&"a".repeat(64),1,20,false)?;fixture_open(d)?;
        let op=operation(d,1);list_mut(d,"operations").push(op.clone());prearm(d,&op)?;
        request_close(d,"bounded-drain","handoff")?;Ok(())
    }).await.unwrap();
    let held=lock(&app).await;
    let result=tokio::time::timeout(std::time::Duration::from_secs(2),close_and_drain(&app,"bounded-drain","handoff")).await;
    assert!(result.is_ok(),"the existing durable deadline must include waiting for M");assert!(result.unwrap().is_err());drop(held);
    let d=app.read().await.unwrap();assert_eq!(d[FIELD]["state"],"closing");assert!(d[FIELD]["finalReceipt"].is_null());
    assert_eq!(d["operations"][0][PERMIT_FIELD]["phase"],"dispatch_armed");
}

#[test]
fn company_owner_generation_and_configured_deadline_fences_are_not_reset() {
    let mut d=workspace();let op=add(&mut d,1);
    d["runtimeLifecycle"]["owner"]["epoch"]=json!(2);
    assert!(prearm(&mut d,&op).is_err());
    d["runtimeLifecycle"]["owner"]["epoch"]=json!(1);
    d[FIELD]["drainDeadlineMs"]=json!(MAX_DRAIN_MS+1);
    assert!(request_close(&mut d,"bad-deadline","handoff").is_err());
    let mut missing=workspace();missing.as_object_mut().unwrap().remove(FIELD);
    assert!(fence_admission(&missing,&[op["target"].clone()]).is_err());
    assert_eq!(preparation_dependency(&missing,false).unwrap()["status"],"ready");
    assert!(preparation_dependency(&missing,true).is_err());
}

#[test]
fn typed_availability_failure_freezes_one_cohort_and_ready_never_reopens() {
    let mut d=workspace();let op=add(&mut d,1);prearm(&mut d,&op).unwrap();
    let projection=json!({"version":1,"account":"likeavto","connectionBinding":active_binding(&d).unwrap().to_json(),
        "state":"needs_owner","generation":1,"reason":"auth_unresolved","receiptSha256":"b".repeat(64)});
    observe_availability(&mut d,&AvailabilityObservation{projection:projection.clone(),protected_receipt_sha256:"b".repeat(64)}).unwrap();
    assert_eq!(d[FIELD]["state"],"closing");assert_eq!(d[FIELD]["closingIntent"]["cohortCount"],1);
    let mut ready=projection.clone();ready["state"]=json!("ready");ready["generation"]=json!(2);
    observe_availability(&mut d,&AvailabilityObservation{projection:ready,protected_receipt_sha256:"b".repeat(64)}).unwrap();
    assert_eq!(d[FIELD]["state"],"closing");
    assert!(observe_availability(&mut d,&AvailabilityObservation{projection,protected_receipt_sha256:"b".repeat(64)}).is_err());
}
