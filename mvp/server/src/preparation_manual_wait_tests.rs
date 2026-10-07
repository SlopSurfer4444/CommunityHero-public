//! Real SQLite writer and native frame/CAS reducers; no model/provider process.
use super::*;
use crate::{manual_frame_request, preparation_review, runtime_lifecycle_app};
use std::sync::{Arc, atomic::{AtomicUsize, Ordering}};

async fn scheduled() -> (App, tempfile::TempDir, crate::engine_prepare::Scheduled, OwnerToken) {
    let (app, temp) = manual_frame_request::native_app().await;
    let token = app.lifecycle_admission_token(AdmissionClass::Preparation).await.unwrap();
    let scheduled = app.change_preparation_schedule(|d| super::super::schedule_admitted(d,
        super::super::Input { item_ids:vec!["ready".into()], instruction:None }, &token)).await.unwrap();
    (app, temp, scheduled, token)
}
async fn add_manual(app:&App,run:&str,dir:&std::path::Path)->manual_frame_request::NativeExtractionFixture {
    let before=app.db.read().await.unwrap();let mut native=before.clone();
    let extraction=manual_frame_request::native_fixture_begin(&mut native,run,dir);
    let extra=crate::list(&native,"jobs").iter().filter(|j| !crate::list(&before,"jobs").iter().any(|old|old["id"]==j["id"]))
        .cloned().collect::<Vec<_>>();
    app.change(|d|{crate::list_mut(d,"jobs").extend(extra);Ok(())}).await.unwrap();extraction
}
async fn complete(app:&App,fixture:&manual_frame_request::NativeExtractionFixture,token:&OwnerToken) {
    let decoded=fixture.decode().await;
    let completed=app.change(|d|fixture.observe(d,token,&decoded)).await.unwrap();
    fixture.warm(&app.db.read().await.unwrap(),&completed);
    app.preparation_wake.notify_one();
}
async fn waiting(app:&App,run:&str) {
    tokio::time::timeout(Duration::from_secs(5),async {
        loop {
            let d=app.db.read().await.unwrap();
            if row(&d,"jobs",run).unwrap()["preparationStages"]["manualMaterialWait"]["status"]=="waiting" {break;}
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }).await.expect("native worker must reach the durable manual wait");
}
async fn free_resources(app:&App) {
    let lease=tokio::time::timeout(Duration::from_millis(500),app.preparation_workers.acquire(None)).await
        .expect("pending materials must not occupy the sole preparation slot");
    let gate=app.assistant_gate.try_lock().expect("pending materials must not hold the assistant gate");
    tokio::time::timeout(Duration::from_millis(500),app.change(|_|Ok(()))).await.unwrap().unwrap();
    drop(gate);drop(lease);
}
fn no_paid(d:&Value,run:&str) {
    let job=row(d,"jobs",run).unwrap();assert!(job["preparationStages"]["first"].is_null());
    assert!(job["preparationStages"].get("firstAdmission").is_none());
    assert!(job.get("retainedEvidence").is_none_or(|r|r.as_array().is_some_and(Vec::is_empty)));
    assert!(job.get("modelMaterialReceipts").is_none_or(|r|r.as_array().is_some_and(Vec::is_empty)));
}

#[tokio::test]
async fn pending_completion_refreshes_original_capture_before_one_native_first_admission() {
    let(app,temp,scheduled,token)=scheduled().await;let run=scheduled.job_id.clone();
    let original=scheduled.request().unwrap().clone();let initial=app.db.read().await.unwrap();
    let fixture=add_manual(&app,&run,temp.path()).await;let attempts=Arc::new(AtomicUsize::new(0));
    let task={let app=app.clone();let run=run.clone();let token=token.clone();let original=original.clone();let attempts=attempts.clone();
        tokio::spawn(runtime_lifecycle_app::with_job(run.clone(),async move {
            let mut wait=Wait::new(token.clone());
            let request=acquire(&app,&run,&original,&mut wait,super::super::preflight_capture).await?;
            let lease=app.preparation_workers.acquire(None).await;let _gate=app.assistant_gate.lock().await;
            lease.scope(async {
                app.change_preparation_first(&run,|d|preparation_review::reserve_first_admitted(d,&token,&run,&request,&crate::now())).await?;
                attempts.fetch_add(1,Ordering::SeqCst); // synthetic model boundary, after the actual atomic guard
                Ok::<_,crate::ApiError>(request)
            }).await
        }))};
    waiting(&app,&run).await;free_resources(&app).await;assert_eq!(attempts.load(Ordering::SeqCst),0);
    let waiting_snapshot=app.db.read().await.unwrap();
    tokio::time::sleep(Duration::from_millis(1100)).await;
    assert_eq!(app.db.read().await.unwrap(),waiting_snapshot,"unchanged pending polls must not write heartbeat records");
    no_paid(&app.db.read().await.unwrap(),&run);complete(&app,&fixture,&token).await;
    let request=tokio::time::timeout(Duration::from_secs(5),task).await.unwrap().unwrap().unwrap();
    assert_eq!(attempts.load(Ordering::SeqCst),1);assert_eq!(request["manualFrameRequestIds"],json!([fixture.job["id"]]));
    assert_eq!(request["optionalFrameRefs"][0]["actualPts"],1040);
    assert_eq!(request["postContextBundle"],original["postContextBundle"]);assert_eq!(request["materials"],original["materials"]);
    let after=app.db.read().await.unwrap();let old=row(&initial,"jobs",&run).unwrap();let new=row(&after,"jobs",&run).unwrap();
    assert_eq!(new["preparationStages"]["initialAdmission"]["owner"],old["preparationStages"]["initialAdmission"]["owner"]);
    assert_eq!(new["preparationStages"]["initialAdmission"]["admittedAt"],old["preparationStages"]["initialAdmission"]["admittedAt"]);
    assert_eq!(new["preparationStages"]["manualMaterialWait"]["status"],"ready");
    assert_eq!(new["preparationStages"]["manualMaterialWait"]["resumeAuthorized"],false);
    assert!(app.change_preparation_first(&run,|d|preparation_review::reserve_first_admitted(d,&token,&run,&request,&crate::now())).await.is_err());
    assert_eq!(app.db.read().await.unwrap(),after);assert!(!app.node.exists());assert!(!app.bridge.exists());app.db.close().await;
}

#[tokio::test]
async fn completion_after_empty_capture_is_refreshed_before_first_not_silently_omitted() {
    let(app,temp,scheduled,token)=scheduled().await;let run=&scheduled.job_id;let request=scheduled.request().unwrap();
    assert_eq!(request["manualFrameRequestIds"],json!([]));let fixture=add_manual(&app,run,temp.path()).await;
    complete(&app,&fixture,&token).await;
    assert_eq!(app.change_preparation_first(run,|d|preparation_review::reserve_first_admitted(d,&token,run,request,&crate::now())).await.unwrap_err().1,
        "manual_frame_unpaid_capture_refresh_required");
    let mut wait=Wait::new(token.clone());
    let refined=runtime_lifecycle_app::with_job(run.clone(),acquire(&app,run,request,&mut wait,super::super::preflight_capture)).await.unwrap();
    assert_eq!(refined["manualFrameRequestIds"],json!([fixture.job["id"]]));
    app.change_preparation_first(run,|d|preparation_review::reserve_first_admitted(d,&token,run,&refined,&crate::now())).await.unwrap();
    app.db.close().await;
}

#[tokio::test]
async fn late_manual_admission_releases_engine_lease_before_waiting_and_never_calls_model_on_failure() {
    let(app,temp,scheduled,token)=scheduled().await;let run=scheduled.job_id.clone();let request=scheduled.request().unwrap().clone();
    // Occupy the worker slot, so the actual engine passes acquisition and waits
    // immediately before its second preflight / atomic FIRST admission.
    let blocking_lease=app.preparation_workers.acquire(None).await;
    let task={let app=app.clone();let run=run.clone();tokio::spawn(runtime_lifecycle_app::with_job(run,
        super::super::run(app,scheduled,false)))};
    // The real writer stays usable while the worker waits for the slot.
    let fixture=add_manual(&app,&run,temp.path()).await;drop(blocking_lease);
    waiting(&app,&run).await;free_resources(&app).await;
    app.change(|d|{row_mut(d,"jobs",fixture.job["id"].as_str().unwrap())?["status"]=json!("failed");Ok(())}).await.unwrap();
    app.preparation_wake.notify_one();
    let error=tokio::time::timeout(Duration::from_secs(5),task).await.unwrap().unwrap().unwrap_err();
    assert_eq!(error.1,"manual_frame_wait_terminal");let after=app.db.read().await.unwrap();no_paid(&after,&run);
    assert_eq!(row(&after,"jobs",&run).unwrap()["prepareBundle"]["request"],request);
    assert_eq!(row(&after,"jobs",&run).unwrap()["preparationStages"]["manualMaterialWait"]["status"],"held");
    assert!(!app.node.exists());assert!(!app.bridge.exists());drop(token);app.db.close().await;
}

#[tokio::test]
async fn exact_first_admission_race_releases_lease_and_refreshes_before_one_attempt() {
    let(app,temp,mut scheduled,token)=scheduled().await;let run=scheduled.job_id.clone();
    let mut wait=Wait::new(token.clone());
    let original=scheduled.request().unwrap().clone();
    scheduled.request=Some(runtime_lifecycle_app::with_job(run.clone(),
        acquire(&app,&run,&original,&mut wait,super::super::preflight_capture)).await.unwrap());
    let request=scheduled.request().unwrap().clone();let state=app.db.read().await.unwrap();
    super::super::preflight(&state,&run).unwrap();
    let keys=crate::preparation_workers::keys(&state,row(&state,"jobs",&run).unwrap());
    let lease=app.preparation_workers.acquire(keys.clone()).await;let gate=app.assistant_gate.lock().await;
    // The operator commits manual work after ordinary readiness, before the
    // final atomic reservation. Neither the guard nor the native owned worker
    // may spend this stale, empty frame capture.
    let fixture=add_manual(&app,&run,temp.path()).await;
    let error=app.change_preparation_first(&run,|d|preparation_review::reserve_first_admitted(d,&token,&run,&request,&crate::now())).await.unwrap_err();
    assert_eq!(error.1,"manual_frame_requested_material_unresolved");
    let blocked=runtime_lifecycle_app::with_job(run.clone(),lease.scope(super::super::run_owned(
        app.clone(),scheduled,false,keys,Some(token.clone())))).await.unwrap_err();
    assert_eq!(blocked.1,error.1);no_paid(&app.db.read().await.unwrap(),&run);
    drop(gate);drop(lease);free_resources(&app).await;wait.admission_race(&request);
    complete(&app,&fixture,&token).await;
    let refined=runtime_lifecycle_app::with_job(run.clone(),acquire(&app,&run,&request,&mut wait,super::super::preflight_capture)).await.unwrap();
    assert_eq!(refined["manualFrameRequestIds"],json!([fixture.job["id"]]));
    let attempts=AtomicUsize::new(0);
    for _ in 0..2 {if app.change_preparation_first(&run,|d|preparation_review::reserve_first_admitted(d,&token,&run,&refined,&crate::now())).await.is_ok(){
        attempts.fetch_add(1,Ordering::SeqCst);
    }}
    assert_eq!(attempts.load(Ordering::SeqCst),1);assert!(!app.node.exists());assert!(!app.bridge.exists());app.db.close().await;
}

#[tokio::test]
async fn bounded_timeout_preserves_original_manual_observation_and_never_reserves_first() {
    let(app,temp,scheduled,token)=scheduled().await;let run=&scheduled.job_id;let request=scheduled.request().unwrap();
    let fixture=add_manual(&app,run,temp.path()).await;let before=app.db.read().await.unwrap();
    let manual=row(&before,"jobs",fixture.job["id"].as_str().unwrap()).unwrap().clone();
    let mut wait=Wait::new(token);wait.limit=Duration::from_millis(20);
    let error=runtime_lifecycle_app::with_job(run.clone(),acquire(&app,run,request,&mut wait,super::super::preflight_capture)).await.unwrap_err();
    assert_eq!(error.1,"manual_frame_wait_timeout");let after=app.db.read().await.unwrap();no_paid(&after,run);
    assert_eq!(row(&after,"jobs",fixture.job["id"].as_str().unwrap()).unwrap(),&manual);
    assert_eq!(row(&after,"jobs",run).unwrap()["preparationStages"]["manualMaterialWait"]["status"],"held");
    app.db.close().await;
}

#[tokio::test]
async fn reserved_or_retained_capture_is_never_refreshed_or_replayed() {
    let(app,_temp,scheduled,token)=scheduled().await;let run=&scheduled.job_id;let request=scheduled.request().unwrap();
    let initial=app.db.read().await.unwrap();
    for key in ["retainedEvidence","modelMaterialReceipts"] {
        let mut changed=initial.clone();row_mut(&mut changed,"jobs",run).unwrap()[key]=json!([{"existing":"unknown paid evidence"}]);
        assert!(unpaid(&changed,run,request,&token,super::super::preflight_capture).is_err(),"{key}");
    }
    app.change_preparation_first(run,|d|preparation_review::reserve_first_admitted(d,&token,run,request,&crate::now())).await.unwrap();
    let reserved=app.db.read().await.unwrap();let mut wait=Wait::new(token);
    assert!(runtime_lifecycle_app::with_job(run.clone(),acquire(&app,run,request,&mut wait,super::super::preflight_capture)).await.is_err());
    assert_eq!(app.db.read().await.unwrap(),reserved);assert!(!app.node.exists());assert!(!app.bridge.exists());app.db.close().await;
}

#[tokio::test]
async fn waiting_capture_cannot_follow_source_changes_or_lifecycle_epochs() {
    let(app,_temp,scheduled,token)=scheduled().await;let run=&scheduled.job_id;let request=scheduled.request().unwrap();
    let initial=app.db.read().await.unwrap();
    for fault in ["source","recipient","epoch","owner","status","initialOwner","request"] {
        let mut d=initial.clone();match fault {
            "source"=>d["posts"][0]["text"]=json!("Changed source after waiting"),
            "recipient"=>row_mut(&mut d,"items","ready").unwrap()["revision"]=json!(99),
            "epoch"=>d["runtimeLifecycle"]["owner"]["epoch"]=json!(token.epoch+1),
            "owner"=>d["runtimeLifecycle"]["owner"]["runtimeId"]=json!("successor"),
            "status"=>row_mut(&mut d,"jobs",run).unwrap()["status"]=json!("interrupted"),
            "initialOwner"=>row_mut(&mut d,"jobs",run).unwrap()["preparationStages"]["initialAdmission"]["owner"]["epoch"]=json!(99),
            _=>row_mut(&mut d,"jobs",run).unwrap()["prepareBundle"]["request"]["instruction"]=json!("changed"),
        }
        assert!(unpaid(&d,run,request,&token,super::super::preflight_capture).is_err(),"{fault}");
    }
    no_paid(&app.db.read().await.unwrap(),run);assert_eq!(app.db.read().await.unwrap(),initial);app.db.close().await;
}
