//! Connected registration tests construct private synthetic verified inputs;
//! they never weaken the production file/core/admission verifier.
use super::*;
use crate::writer_gate::Class;
use std::{future::Future,pin::Pin,task::Poll,time::Duration};
use serde_json::json;
fn candidate()->VerifiedTarget {
    VerifiedTarget{held:HeldTarget{target:AdmittedTarget{release_sha256:"e".repeat(64),media_analysis_generation:1,asr_disabled:true},
        reviewed_record:json!({"releaseSha256":"e".repeat(64),"mediaAnalysisGeneration":1,"asrDisabled":true,"admissionReceiptSha256":"f".repeat(64)})},
        file_pin:json!({"path":"synthetic-test-only","sha256":"d".repeat(64)})}
}
async fn pending<F:Future>(future:&mut Pin<Box<F>>) {
    std::future::poll_fn(|cx|{assert!(future.as_mut().poll(cx).is_pending());Poll::Ready(())}).await;
}
async fn owner(app:&crate::App)->OwnerToken {
    runtime_lifecycle::current_owner(&app.db.read_metadata().await.unwrap(),&app.lifecycle_owner).unwrap()
}
#[tokio::test]
async fn real_registration_queues_before_db_and_publishes_registry_only_after_commit() {
    let (app,_folder)=crate::tests::test_app().await;let expected=owner(&app).await;
    let original=app.db.read().await.unwrap();let held=app.gate.acquire(Class::Standard).await;
    let mut registration=Box::pin(app.lifecycle_admission.register_target(&app,&expected,candidate()));pending(&mut registration).await;
    assert_eq!(app.gate.queued_count(Class::Interactive),1);
    assert!(app.lifecycle_admission.target(&"e".repeat(64)).is_err());
    assert_eq!(app.db.read().await.unwrap(),original);
    drop(held);let receipt=registration.await.unwrap();assert_eq!(receipt["stopAuthorized"],false);
    assert!(app.lifecycle_admission.target(&"e".repeat(64)).is_ok());assert_eq!(app.db.read().await.unwrap(),original);app.db.close().await;
}
#[tokio::test]
async fn real_registration_rechecks_stale_expected_epoch_inside_current_locked_reducer() {
    let (app,_folder)=crate::tests::test_app().await;let expected=owner(&app).await;
    let target=app.lifecycle_admission.target(&"c".repeat(64)).unwrap();let held=app.gate.acquire(Class::Standard).await;
    let mut registration=Box::pin(app.lifecycle_admission.register_target(&app,&expected,candidate()));pending(&mut registration).await;
    assert_eq!(app.gate.queued_count(Class::Interactive),1);
    // This is the active holder's committed drain; registration is still gated.
    app.db.change_runtime_lifecycle_with_ledger(|d|runtime_lifecycle::begin_drain_for_release(d,&expected,&target,"owner-race")).await.unwrap();
    let drained=app.db.read().await.unwrap();drop(held);
    let error=registration.await.unwrap_err();assert_eq!(error.0,axum::http::StatusCode::CONFLICT);
    assert!(app.lifecycle_admission.target(&"e".repeat(64)).is_err());assert!(app.lifecycle_admission.target(&"c".repeat(64)).is_ok());
    assert_eq!(app.db.read().await.unwrap(),drained);app.db.close().await;
}
#[tokio::test]
async fn cancelled_real_registration_releases_gate_and_registry_without_publishing_target() {
    let (app,_folder)=crate::tests::test_app().await;let expected=owner(&app).await;let original=app.db.read().await.unwrap();
    for delivered in [false,true] {
        let held=app.gate.acquire(Class::Standard).await;
        let mut registration=Box::pin(app.lifecycle_admission.register_target(&app,&expected,candidate()));pending(&mut registration).await;
        assert_eq!(app.gate.queued_count(Class::Interactive),1);
        if delivered {drop(held);drop(registration);}else{drop(registration);drop(held);}
        assert_eq!(app.gate.queued_count(Class::Interactive),0);
        let permit=tokio::time::timeout(Duration::from_secs(1),app.gate.acquire(Class::Standard)).await.unwrap();drop(permit);
        assert!(app.lifecycle_admission.target(&"e".repeat(64)).is_err());assert!(app.lifecycle_admission.target(&"c".repeat(64)).is_ok());
        assert_eq!(app.db.read().await.unwrap(),original);
    }
    app.db.close().await;
}
