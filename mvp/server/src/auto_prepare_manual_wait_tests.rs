//! Actual automatic worker: no preparation lease while manual material waits.
use super::*;
use crate::{manual_frame_request, preparation_review, runtime_lifecycle::AdmissionClass};
use std::time::Duration;

#[tokio::test]
async fn automatic_pending_manual_work_releases_slot_then_terminal_failure_never_retries_first() {
    let(app,temp)=manual_frame_request::native_app().await;
    let token=app.lifecycle_admission_token(AdmissionClass::Preparation).await.unwrap();
    let(run,request)=app.change(|d|{
        d["items"].as_array_mut().unwrap().retain(|item|item["id"]=="ready");
        crate::row_mut(d,"items","ready")?["providerObservedAt"]=json!(crate::now());
        let(run,request)=claim(d,chrono::Utc::now().timestamp())?.ok_or_else(||crate::conflict("Native automatic fixture not claimable"))?;
        preparation_review::record_initial_admission(d,&token,&run,&crate::now())?;Ok((run,request))
    }).await.unwrap();
    let before=app.db.read().await.unwrap();let mut native=before.clone();
    let extraction=manual_frame_request::native_fixture_begin(&mut native,&run,temp.path());
    let extra=crate::list(&native,"jobs").iter().filter(|j|!crate::list(&before,"jobs").iter().any(|old|old["id"]==j["id"]))
        .cloned().collect::<Vec<_>>();
    app.change(|d|{crate::list_mut(d,"jobs").extend(extra);Ok(())}).await.unwrap();
    spawn_worker(&app,run.clone(),Some(request.clone()));
    tokio::time::timeout(Duration::from_secs(5),async {
        loop {let d=app.db.read().await.unwrap();
            if crate::row(&d,"jobs",&run).unwrap()["preparationStages"]["manualMaterialWait"]["status"]=="waiting"{break;}
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }).await.expect("actual auto worker must wait for manual frames");
    let lease=tokio::time::timeout(Duration::from_millis(500),app.preparation_workers.acquire(None)).await
        .expect("automatic material wait must release the sole worker slot");
    let gate=app.assistant_gate.try_lock().expect("automatic material wait must release assistant gate");drop(gate);drop(lease);
    app.change(|d|{crate::row_mut(d,"jobs",extraction.job["id"].as_str().unwrap())?["status"]=json!("failed");Ok(())}).await.unwrap();
    app.preparation_wake.notify_one();
    tokio::time::timeout(Duration::from_secs(5),async {
        loop {let d=app.db.read().await.unwrap();
            if crate::row(&d,"jobs",&run).unwrap()["status"]!="running"&&app.tasks.lock().await.is_empty(){break;}
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }).await.expect("terminal manual failure must settle the automatic worker finitely");
    let after=app.db.read().await.unwrap();let job=crate::row(&after,"jobs",&run).unwrap();
    assert_eq!(job["prepareBundle"]["request"],request);assert!(job["preparationStages"]["first"].is_null());
    assert!(job["preparationStages"].get("firstAdmission").is_none());
    assert!(job.get("retainedEvidence").is_none_or(|v|v.as_array().is_some_and(Vec::is_empty)));
    assert_eq!(job["preparationStages"]["manualMaterialWait"]["status"],"held");
    let item=crate::row(&after,"items","ready").unwrap();assert!(item["autoPreparation"]["retryAt"].is_null());
    assert!(!app.node.exists());assert!(!app.bridge.exists());app.db.close().await;
}
