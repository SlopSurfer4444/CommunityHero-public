use crate::*;
use crate::storage::AdmissionScope;

async fn connected_recovery_lifecycle(app:&App) {
    let db=&app.db;
    let (baseline,capture)=retained_paid_recovery::tests::fixture();
    let actor=retained_paid_recovery::tests::actor();
    let body=retained_paid_recovery::tests::body(&baseline,&capture);
    db.change(|d|{*d=baseline.clone();Ok(())}).await.unwrap();
    crate::runtime_lifecycle_startup::initialize_db_fixture(db).await.unwrap();
    // Only this connected synthetic downstream case requires send admission;
    // recovery itself remains separate from permission to dispatch.
    db.change(|d|crate::connection_gate::fixture_open(d)).await.unwrap();
    let baseline=db.read().await.unwrap();
    let _capture=retained_paid_recovery_registry::install_test_capture(capture.clone());
    let(first,changed)=db.commit_retained_paid_recovery_observed(&actor,&capture,&body,&app.lifecycle_owner).await.unwrap();
    assert!(changed);
    let admitted=db.read().await.unwrap();
    let refs=first["proposals"].clone();let key=refs[0]["id"].as_str().unwrap();
    let(replay,changed)=db.commit_retained_paid_recovery_observed(&actor,&capture,&body,&app.lifecycle_owner).await.unwrap();
    assert!(!changed);assert_eq!(replay["proposals"],refs);assert_eq!(db.read().await.unwrap(),admitted);
    assert_eq!(admitted["jobs"],baseline["jobs"],"original paid owner must remain unchanged");

    let preview_body=json!({"proposals":refs});
    let view=db.read_operator_editorial(&preview_body).await.unwrap();
    assert_eq!(view,admitted,"recovery review must read complete proof and owner closure");
    assert!(view.get("scopeOwners").is_none());
    let before_review=db.read().await.unwrap();
    // Native preview itself refuses a manual-material waiver for recovered
    // model text; do not invoke the successful-preview test body constructor.
    let held=operator_editorial::capture(&view,&actor,&preview_body).unwrap_err();
    assert_eq!(held.1,"operator_material_manual_origin_unproven");
    assert_eq!(db.read().await.unwrap(),before_review,"Recovered model text cannot gain a manual-origin receipt");
    db.change(|d|editorial_review::fixture_accept(d,key).map_err(conflict)).await.unwrap();
    let reviewed=db.read().await.unwrap();
    assert!(row(&reviewed,"proposals",key).unwrap()["editorialModelMaterialReceipt"].is_object());
    assert!(row(&reviewed,"proposals",key).unwrap()["operatorMaterialReceipt"].is_null());
    assert_eq!(row(&reviewed,"jobs",capture.job_id().unwrap()).unwrap(),&baseline["jobs"][0]);
    let approve=json!({"requestId":"retained-downstream-approve","proposals":refs});
    let(approval,_)=db.change_admission_observed(AdmissionScope::Approval(&approve),|d|approval_admission::create(d,&actor,&approve)).await.unwrap();
    let approval_id=approval["id"].as_str().unwrap();
    let execute=json!({"requestId":"retained-downstream-execute","approvalId":approval_id});
    let((_,scheduled),_)=db.change_admission_observed(AdmissionScope::Execute{approval:approval_id,body:&execute},|d|execute_admission::admit(d,&actor,approval_id,&execute)).await.unwrap();
    let(_,operations)=scheduled.unwrap();assert_eq!(operations.len(),1);
    let op=&operations[0];
    let dispatched=db.read_dispatch_context(key).await.unwrap();
    assert_eq!(dispatched,db.read().await.unwrap(),"actual dispatch needs full receipt/approval/owner state");
    assert_eq!(op["approvedRetainedPaidRecoverySha256"],row(&dispatched,"proposals",key).unwrap()[retained_paid_recovery::FIELD]["proofSha256"]);
    assert!(dispatch_diagnostics::local_check(&dispatched,op).is_ok());
    let mut forged=op.clone();forged["approvedRetainedPaidRecoverySha256"]=json!("f".repeat(64));
    assert!(dispatch_diagnostics::local_check(&dispatched,&forged).is_err());
    assert_eq!(row(&dispatched,"jobs",capture.job_id().unwrap()).unwrap(),&baseline["jobs"][0]);
    assert!(preparation_reservations::assert_available(&dispatched,&["i0".into()],None).is_err(),"draft recovery never authorizes another paid call");
    // Exercise the real post-network receipt/outcome transition using synthetic
    // input, then the independently observed final outcome. These bounded
    // persistence paths must work after current-context authority has expired.
    db.change_source_snapshot_observed(|d|{d["posts"][0]["text"]=json!("Source changed after the effect was admitted");Ok(())}).await.unwrap();
    app.change_execute_transition(op,json!({"syntheticReceipt":true}),"unknown",|d|
        apply_operation_outcome(d,op,"unknown",json!({"verificationPhase":"verifying","providerRetryAllowed":false}))).await.unwrap();
    let uncertain=db.read().await.unwrap();
    assert_eq!(row(&uncertain,"operations",op["id"].as_str().unwrap()).unwrap()["status"],"unknown");
    app.change_operation_outcome(op,"succeeded",|d|
        apply_operation_outcome(d,op,"succeeded",json!({"syntheticIndependentReadback":true}))).await.unwrap();
    let settled=db.read().await.unwrap();
    assert_eq!(row(&settled,"operations",op["id"].as_str().unwrap()).unwrap()["status"],"succeeded");
    assert_eq!(row(&settled,"jobs",capture.job_id().unwrap()).unwrap(),&baseline["jobs"][0]);
    assert_eq!(row(&settled,"proposals",key).unwrap()[retained_paid_recovery::FIELD],row(&admitted,"proposals",key).unwrap()[retained_paid_recovery::FIELD]);
    // No worker spawn or provider execution occurs in this test.
}

#[tokio::test(flavor="current_thread")]
async fn sqlite_retained_recovery_editorial_approval_execute_dispatch_connected() {
    let(app,_temp)=crate::tests::test_app().await;
    connected_recovery_lifecycle(&app).await;app.db.close().await;
}

#[tokio::test(flavor="current_thread")]
#[ignore="requires a fresh isolated PostgreSQL fixture; run this selector alone"]
async fn postgres_retained_recovery_editorial_approval_execute_dispatch_connected() {
    std::env::var("COMMUNITYHERO_WRITER_V51_TEST_URL").expect("explicit isolated fixture URL");
    let(mut app,_temp)=crate::tests::test_app().await;
    let db=super::super::preparation::writer_v51_fixture_db().await;
    app.db.close().await;app.db=db;
    connected_recovery_lifecycle(&app).await;app.db.close().await;
}

#[tokio::test(flavor="current_thread")]
async fn sqlite_retained_recovery_generic_forgery_and_late_failure_roll_back() {
    let(app,_temp)=crate::tests::test_app().await;
    let(baseline,capture)=retained_paid_recovery::tests::fixture();
    let actor=retained_paid_recovery::tests::actor();
    let body=retained_paid_recovery::tests::body(&baseline,&capture);
    app.db.change(|d|{*d=baseline.clone();Ok(())}).await.unwrap();
    crate::runtime_lifecycle_startup::initialize_db_fixture(&app.db).await.unwrap();
    let baseline=app.db.read().await.unwrap();
    let _capture=retained_paid_recovery_registry::install_test_capture(capture.clone());
    let owner=capture.job_id().unwrap();
    assert!(app.db.change_job_observed(owner,|d|{row_mut(d,"jobs",owner)?["status"]=json!("completed");Ok(())}).await.is_err(),"job-only writer cannot rewrite pinned original failure even before a draft exists");
    assert_eq!(app.db.read().await.unwrap(),baseline);
    // Even the real reducer cannot append proofs through an ordinary callback.
    assert!(app.db.change(|d|retained_paid_recovery::commit(d,&actor,&capture,&body)).await.is_err());
    assert_eq!(app.db.read().await.unwrap(),baseline);
    let mut invalid=body.clone();invalid["checks"]["exactText"]=json!("hold");
    assert!(app.db.commit_retained_paid_recovery_observed(&actor,&capture,&invalid,&app.lifecycle_owner).await.is_err());
    assert_eq!(app.db.read().await.unwrap(),baseline);
    let(created,_)=app.db.commit_retained_paid_recovery_observed(&actor,&capture,&body,&app.lifecycle_owner).await.unwrap();
    let current=app.db.read().await.unwrap();let key=created["proposals"][0]["id"].as_str().unwrap();
    assert!(app.db.change(|d|{row_mut(d,"proposals",key)?[retained_paid_recovery::FIELD]["reason"]=json!("Forged review");Ok(())}).await.is_err());
    assert_eq!(app.db.read().await.unwrap(),current);
    // A late failure after a valid ordinary status edit must not persist.
    let refs=json!({"proposals":created["proposals"]});
    let failed:ApiResult<((),bool)>=app.db.change_admission_observed(AdmissionScope::Approval(&refs),|d|{
        row_mut(d,"proposals",key)?["status"]=json!("approved");Err(conflict("synthetic late failure"))
    }).await;
    assert!(failed.is_err());assert_eq!(app.db.read().await.unwrap(),current);app.db.close().await;
}
