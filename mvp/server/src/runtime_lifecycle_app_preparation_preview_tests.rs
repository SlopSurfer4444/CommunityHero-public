//! ROOT executes: detached reducers use the same strict guard as production.
use super::*;
use serde_json::json;

async fn app_fixture()->(App,tempfile::TempDir) {
    let (app,temp)=crate::tests::test_app().await;
    let mut d=crate::engine_prepare::tests::fixture(false);
    for field in ["knowledge_entries","knowledge_versions","feedback"] {d[field]=json!([]);}
    d["runtimeLifecycle"]=app.db.read().await.unwrap()["runtimeLifecycle"].clone();
    app.db.change(|state|{*state=d;Ok(())}).await.unwrap();
    (app,temp)
}
fn input()->crate::engine_prepare::Input {
    crate::engine_prepare::Input{item_ids:vec!["ready".into()],instruction:None}
}

#[tokio::test]
async fn strict_missing_capture_rejects_without_test_reducer_fallback() {
    let (app,_temp)=app_fixture().await;
    let d=app.db.read().await.unwrap();let before=d.clone();
    assert!(WRITER.with(|v|v.borrow().is_none()));
    assert!(require_new_job_strict(&d,"assistant").unwrap_err().1.contains("fixed native writer identity"));
    assert_eq!(d,before);assert_eq!(app.db.read().await.unwrap(),before);
    app.db.close().await;
}

#[tokio::test]
async fn detached_manual_preview_is_strict_unpersisted_and_real_writer_rechecks() {
    let (app,_temp)=app_fixture().await;
    let token=app.lifecycle_admission_token(AdmissionClass::Preparation).await.unwrap();
    let before=app.db.read().await.unwrap();
    let (detached,preview)=Capture::preview(&app,&token,before.clone(),|d| {
        require_new_job_strict(d,"assistant")?;
        crate::engine_prepare::schedule(d,input())
    }).unwrap();
    assert_eq!(crate::list(&detached,"jobs").len(),crate::list(&before,"jobs").len()+1);
    assert_eq!(app.db.read().await.unwrap(),before,"speculation cannot persist jobs or reservations");
    assert!(WRITER.with(|v|v.borrow().is_none()));
    assert!(require_new_job_strict(&detached,"assistant").is_err(),"context ends before async sizing");
    let actual=app.change_preparation_schedule(|d| {
        require_new_job_strict(d,"assistant")?;
        runtime_lifecycle::require_admission(d,&token,AdmissionClass::Preparation)?;
        let scheduled=crate::engine_prepare::schedule(d,input())?;
        crate::engine_prepare::capacity::same_capture(preview.request(),scheduled.request())?;
        crate::preparation_review::record_initial_admission(d,&token,&scheduled.job_id,&crate::now())?;
        Ok(scheduled)
    }).await.unwrap();
    let saved=app.db.read().await.unwrap();
    assert!(crate::row(&saved,"jobs",&actual.job_id).is_ok());
    assert_eq!(saved["operations"],before["operations"]);assert_eq!(saved["approvals"],before["approvals"]);
    app.db.change_runtime_lifecycle_with_ledger(|d|runtime_lifecycle::begin_drain(d,&token,&"b".repeat(64),"preview-writer-race",false)).await.unwrap();
    let draining=app.db.read().await.unwrap();
    assert!(app.change_preparation_schedule(|d|crate::engine_prepare::schedule(d,input())).await.is_err());
    assert_eq!(app.db.read().await.unwrap(),draining,"actual writer independently rejects drain after preview");
    app.db.close().await;
}

#[tokio::test]
async fn preview_foreign_identity_stale_epoch_and_drain_reject_before_reducer() {
    let (app,_temp)=app_fixture().await;
    let token=app.lifecycle_admission_token(AdmissionClass::Preparation).await.unwrap();
    let before=app.db.read().await.unwrap();
    let mut foreign=app.clone();let mut identity=(*app.lifecycle_owner).clone();identity.runtime_id="obsolete-runtime".into();
    foreign.lifecycle_owner=std::sync::Arc::new(identity);
    let called=std::cell::Cell::new(false);
    assert!(Capture::preview(&foreign,&token,before.clone(),|_|{called.set(true);Ok(())}).is_err());
    let mut stale=token.clone();stale.epoch+=1;
    assert!(Capture::preview(&app,&stale,before.clone(),|_|{called.set(true);Ok(())}).is_err());
    let mut draining=before.clone();
    runtime_lifecycle::begin_drain(&mut draining,&token,&"b".repeat(64),"detached-drain",false).unwrap();
    assert!(Capture::preview(&app,&token,draining,|_|{called.set(true);Ok(())}).is_err());
    assert!(!called.get());assert!(WRITER.with(|v|v.borrow().is_none()));
    assert_eq!(app.db.read().await.unwrap(),before);
    app.db.close().await;
}

#[tokio::test]
async fn manual_endpoint_reaches_unpaid_capacity_after_preview_without_persisting() {
    let (mut app,temp)=app_fixture().await;
    app.node=std::env::var_os("COMMUNITYHERO_TEST_NODE").map(std::path::PathBuf::from).unwrap_or_else(||"node".into());
    app.bridge=temp.path().join("preview-capacity-only.mjs");
    let marker=temp.path().join("capacity-observed");
    let script=r#"import fs from 'node:fs/promises';
let raw='';for await(const part of process.stdin)raw+=part;const request=JSON.parse(raw);
if(request.operation!=='assistant_preflight')throw new Error('Only unpaid capacity is allowed');
await fs.writeFile(__MARKER__,request.operation);
process.stdout.write(JSON.stringify({ok:true,result:{version:1,results:request.requests.map(row=>({requestSha256:row.requestSha256,status:'oversized',textBytes:null,boundedModelBytes:null,maxModelBytes:550000}))}}));"#
        .replace("__MARKER__",&json!(marker.to_string_lossy()).to_string());
    std::fs::write(&app.bridge,script).unwrap();
    let before=app.db.read().await.unwrap();
    let actor=crate::operator_auth::Actor::local_owner("preview-operator");
    let error=STRICT_NEW_JOBS.scope(true,crate::engine_prepare::prepare(axum::extract::State(app.clone()),axum::Extension(actor),
        axum::Json(json!({"itemIds":["ready"]})))).await.unwrap_err();
    assert_eq!(error.1,crate::engine_prepare::capacity::OVERSIZED);
    assert!(STRICT_NEW_JOBS.try_with(|strict|*strict).is_err(),"strict mode ends with this endpoint future");
    assert_eq!(std::fs::read_to_string(marker).unwrap(),"assistant_preflight");
    assert_eq!(app.db.read().await.unwrap(),before);
    assert!(WRITER.with(|v|v.borrow().is_none()));
    app.db.close().await;
}
