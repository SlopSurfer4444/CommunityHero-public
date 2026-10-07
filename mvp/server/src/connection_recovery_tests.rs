use super::*;

fn projection()->Value {
    let mut value=json!({"version":1,"account":"likeavto","connectionBinding":legacy_binding(),"state":"ready","generation":321,
        "phase":"committed","reason":"owner_session_committed","canonicalCaseId":"11111111-1111-4111-8111-111111111111",
        "grantSpent":true,"candidatePresent":true,"scopeVerified":true,"parentCount":2,"sendGateOpen":false,
        "retryOriginalOperation":false,"socialRequests":0});
    value["receiptSha256"]=json!(format!("{:x}",Sha256::digest(canonical_json(&value).as_bytes())));value
}
fn case_fixture()->(Value,Value) {
    let mut d=crate::connection_gate::tests::workspace();d["storageGeneration"]=json!("11111111-1111-4111-8111-111111111111");
    let binding=active_binding(&d).unwrap().to_json();let source=json!({"sourceSha256":"b".repeat(64),"archiveManifestHash":"c".repeat(64)});
    let archive=json!({"mode":"archive_fence","epoch":1,"safetyFence":{"companyId":"likeavto","connectionScope":binding,
        "archiveManifestHash":"c".repeat(64),"sourceBindingRefs":[source],"predicate":"all_retained_external_aliases",
        "coverage":{"complete":true,"declaredRows":1},"sealedAt":now()},"sealedSourceRefs":[{"sha256":"b".repeat(64)}],
        "recipientReservations":[],"quarantineCoverage":{"complete":true,"namespaces":[]},"archiveRefs":[{"sha256":"c".repeat(64)}]});
    let case=json!({"kind":"communityhero-clean-start-case.v1","bootstrap":{"version":1,"companyId":"likeavto","connectionBinding":binding,
        "expectedStorageGeneration":d["storageGeneration"],"owner":d["runtimeLifecycle"]["owner"],"lifecycleReceiptSha256":"d".repeat(64),
        "predecessorContainmentReceiptSha256":"e".repeat(64),"archiveRestoreReceiptSha256":"f".repeat(64),"mutationPermitCap":1,
        "drainDeadlineMs":1000,"archiveFence":archive,"expectedProtectedGeneration":321}});
    (d,case)
}
fn parsed(value:Value)->ApiResult<VerifiedBootstrapCase>{parse_case(value,"a".repeat(64),PathBuf::from("synthetic-case"))}

#[test]
fn owner_http_request_cannot_supply_ready_grant_or_case_authority() {
    let owner=operator_auth::Actor::local_owner("synthetic");owner_request(&owner,&Bytes::new()).unwrap();
    owner_request(&owner,&Bytes::from_static(b"{}")).unwrap();
    for bytes in [b"{\"ready\":true}".as_slice(),b"{\"casePath\":\"arbitrary\"}".as_slice(),b"[]".as_slice()] {
        assert!(owner_request(&owner,&Bytes::copy_from_slice(bytes)).is_err());
    }
    let mut operator=owner;operator.role="operator".into();assert!(owner_request(&operator,&Bytes::new()).is_err());
}

#[test]
fn safe_protected_receipt_rejects_rehashed_fabricated_ready_or_raw_fields() {
    parse_protected(projection()).unwrap();
    for (field,value) in [("candidatePresent",json!(false)),("scopeVerified",json!(false)),("phase",json!("unresolved")),
        ("socialRequests",json!(1)),("sendGateOpen",json!(true)),("rawBody",json!("private-canary"))] {
        let mut bad=projection();bad[field]=value;bad.as_object_mut().unwrap().remove("receiptSha256");
        bad["receiptSha256"]=json!(format!("{:x}",Sha256::digest(canonical_json(&bad).as_bytes())));
        assert!(parse_protected(bad).is_err());
    }
    let mut bad=projection();bad["generation"]=json!(322);assert!(parse_protected(bad).is_err());
    let read=ProtectedAvailabilityRead{observation:parse_protected(projection()).unwrap(),
        observed:std::time::Instant::now()-std::time::Duration::from_secs(31)};
    assert!(fresh(&read).is_err());
    assert_eq!(canonical_json(&json!({"z":{"b":2,"a":1},"a":[2,1]})),"{\"a\":[2,1],\"z\":{\"a\":1,\"b\":2}}");
}

#[test]
fn case_is_parameterized_and_exact_owner_generation_archive_bound() {
    let (d,value)=case_fixture();let case=parsed(value.clone()).unwrap();case_scope(&d,&case).unwrap();
    assert_eq!(case.bootstrap["expectedProtectedGeneration"],321,"no hardcoded current auth generation");
    for (field,changed) in [("expectedStorageGeneration",json!("22222222-2222-4222-8222-222222222222")),
        ("companyId",json!("baw-russia")),("owner",json!({"account":"LikeAvto","runtimeId":"different-owner","releaseSha256":"a".repeat(64),"epoch":1}))] {
        let mut changed_case=value.clone();changed_case["bootstrap"][field]=changed;
        assert!(case_scope(&d,&parsed(changed_case).unwrap()).is_err());
    }
    for field in ["lifecycleReceiptSha256","predecessorContainmentReceiptSha256","archiveRestoreReceiptSha256"] {
        let mut bad=value.clone();bad["bootstrap"][field]=Value::Null;assert!(parsed(bad).is_err());
    }
    let mut bad=value.clone();bad["bootstrap"]["ready"]=json!(true);assert!(parsed(bad).is_err());
    let mut bad=value.clone();bad["bootstrap"]["mutationPermitCap"]=json!(5);assert!(parsed(bad).is_err());
    let mut bad=value.clone();bad["bootstrap"]["expectedProtectedGeneration"]=json!(0);assert!(parsed(bad).is_err());
    let mut bad=value;bad["bootstrap"]["archiveFence"]["quarantineCoverage"]["complete"]=json!(false);
    assert!(case_scope(&d,&parsed(bad).unwrap()).is_err());
}
#[tokio::test]
async fn protected_inspection_unavailable_closes_gate_without_fabricating_a_generation_or_clearing_memory_hold() {
    let (app,_temp)=crate::tests::test_app().await;
    app.change(|d|connection_gate::fixture_open(d)).await.unwrap();
    block_protected_unavailable(&app).await.unwrap();let d=app.read().await.unwrap();
    assert_eq!(d[connection_gate::FIELD]["state"],"blocked");
    assert_eq!(d[connection_gate::FIELD]["availability"]["generation"],1);
    assert_eq!(d[connection_gate::FIELD]["availability"]["reason"],"protected_read_unavailable");
    assert_eq!(d[connection_gate::FIELD]["finalReceipt"]["providerCapablePermits"],0);
    assert!(dispatch_authority::require_unheld(&app).is_err());
    assert!(list(&d,"operations").is_empty());assert!(list(&d,"approvals").is_empty());
}
#[tokio::test]
async fn native_classified_auth_hook_closes_immediately_and_tracks_one_bounded_company_worker() {
    let (app,_temp)=crate::tests::test_app().await;
    app.change(|d|connection_gate::fixture_open(d)).await.unwrap();
    observe_provider_auth_failure(&app);observe_provider_auth_failure(&app);
    assert!(dispatch_authority::require_unheld(&app).is_err());
    assert_eq!(app.lifecycle_task_count.load(std::sync::atomic::Ordering::SeqCst),1);
    tokio::time::timeout(std::time::Duration::from_secs(2),async {
        while app.lifecycle_task_count.load(std::sync::atomic::Ordering::SeqCst)!=0 {tokio::task::yield_now().await;}
    }).await.unwrap();
    let d=app.read().await.unwrap();assert_eq!(d[connection_gate::FIELD]["state"],"blocked");
    assert_eq!(d[connection_gate::FIELD]["finalReceipt"]["providerCapablePermits"],0);
    assert!(list(&d,"operations").is_empty());assert!(list(&d,"approvals").is_empty());
    assert!(dispatch_authority::require_unheld(&app).is_err());
}
