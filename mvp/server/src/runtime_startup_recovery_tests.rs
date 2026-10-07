//! Connected source fixture. ROOT alone compiles/runs this new test module.
//! No fabricated lifecycle phases: every transition uses the actual reducer.
use crate::{runtime_lifecycle as lifecycle, runtime_lifecycle_backlog as backlog};
use lifecycle::{AdmissionClass,AdmittedTarget,OwnerToken,RuntimeIdentity,SettledNative};
use serde_json::{json,Value};

fn fixture()->(Value,OwnerToken) {
    let mut d=crate::empty();
    d["connectorBinding"]=json!({"id":"synthetic-like-binding","accountId":"LikeAvto"});
    d["knowledge_entries"]=json!([]);d["knowledge_versions"]=json!([]);
    d["jobs"]=json!([
        {"id":"raw-queued-media","kind":"media","purpose":"auto_media","account":"LikeAvto",
         "connectorBinding":{"id":"synthetic-like-binding","accountId":"LikeAvto"},
         "refId":"post-one","status":"queued","createdAt":"2026-10-04T00:00:00Z",
         "startedAt":null,"finishedAt":null,"result":null,"visualContractVersion":2,
         "sourceAttempts":[],"groupKey":"synthetic-group","fallbackAllowed":false,"manualRequested":false},
        {"id":"paid-paused-checkpoint","kind":"media","purpose":"auto_media","account":"LikeAvto",
         "refId":"paid-post","status":"paused","visualContractVersion":2,
         "sourceAttempts":[{"id":"paid-attempt","status":"completed","receipt":{"sha256":"f".repeat(64),"paid":true}}],
         "result":{"visualProgress":{"schemaVersion":2,"leaseEpoch":7,"phase":"scan",
             "committedCursor":3,"retainedArtifacts":[{"path":"synthetic/frame-3.png","sha256":"e".repeat(64)}]}},
         "operatorDecision":{"kind":"paused","reason":"retain exact paid checkpoint"},"future":{"raw":"preserved"}}
    ]);
    d["operations"]=json!([{"id":"unknown-operation","status":"unknown","attemptId":"unknown-attempt",
        "action":{"actionId":"unknown-action","text":"exact uncertain reply","target":{"id":"external-one"}},
        "receipt":{"reason":"response lost","providerRetryAllowed":false},"future":{"raw":[1,2,3]}}]);
    let owner=OwnerToken{account:"LikeAvto".into(),runtime_id:"explicit-predecessor".into(),release_sha256:"a".repeat(64),epoch:1};
    let ledger=lifecycle::ledger_digest(&d).unwrap();
    lifecycle::initialize(&mut d,owner.clone(),&"b".repeat(64),&ledger).unwrap();
    (d,owner)
}
fn target()->AdmittedTarget {AdmittedTarget{release_sha256:"c".repeat(64),media_analysis_generation:1,asr_disabled:false}}
fn settled(owner:&OwnerToken)->SettledNative {
    SettledNative{owner:owner.clone(),application_tasks:0,provider_queued:0,provider_dispatched:0,
        provider_contained:true,credential_writers:0,unresolved_effects:0}
}
fn successor()->(Value,OwnerToken,OwnerToken) {
    let(mut d,prior)=fixture();
    let raw_jobs=d["jobs"].clone();let unknown=d["operations"].clone();
    let drain=lifecycle::begin_drain_for_release(&mut d,&prior,&target(),"connected-transfer").unwrap();
    let transfer=lifecycle::mark_drained(&mut d,&drain,&settled(&drain)).unwrap();
    lifecycle::commit_stop_checkpoint(&mut d,&drain,&transfer).unwrap();
    let next=lifecycle::accept_successor(&mut d,&drain,&transfer,"explicit-successor",&"c".repeat(64),&"d".repeat(64),false).unwrap();
    assert_eq!(d["jobs"],raw_jobs,"actual transfer must preserve every raw queue/checkpoint row");
    assert_eq!(d["operations"],unknown,"actual transfer must preserve UNKNOWN history");
    (d,prior,next)
}
#[test]
fn actual_successor_startup_recovery_defers_media_and_preserves_raw_history() {
    let(mut d,prior,next)=successor();
    let raw_jobs=d["jobs"].clone();let unknown=d["operations"].clone();
    assert!(backlog::preserved_at_recovery(&d,&d["jobs"][0]).unwrap());
    assert!(crate::runtime_lifecycle_app::startup_recovery(&mut d).unwrap(),"retained queue must defer eager media recovery");
    assert_eq!(d["jobs"],raw_jobs,"queued media and paid paused checkpoint must remain exact raw rows");
    assert_eq!(d["operations"],unknown,"UNKNOWN history must remain exact and never replayed");
    assert_eq!(lifecycle::status(&d).unwrap()["phase"],"running");
    let identity=RuntimeIdentity{account:next.account.clone(),runtime_id:next.runtime_id.clone(),release_sha256:next.release_sha256.clone()};
    for class in [AdmissionClass::SourceRead,AdmissionClass::Media,AdmissionClass::Preparation,AdmissionClass::SocialDispatch] {
        assert_eq!(lifecycle::bound_admission_token(&d,&identity,class).unwrap(),next);
        assert!(lifecycle::require_admission(&d,&prior,class).is_err());
    }
    backlog::require_job_claim(&d,"raw-queued-media",&next).unwrap();
}
#[test]
fn actual_closed_phases_reject_startup_recovery_without_any_mutation() {
    let(mut d,owner)=fixture();
    let drain=lifecycle::begin_drain_for_release(&mut d,&owner,&target(),"closed-transfer").unwrap();
    let snapshot=d.clone();assert!(crate::runtime_lifecycle_app::startup_recovery(&mut d).is_err());assert_eq!(d,snapshot);
    let transfer=lifecycle::mark_drained(&mut d,&drain,&settled(&drain)).unwrap();
    let snapshot=d.clone();assert!(crate::runtime_lifecycle_app::startup_recovery(&mut d).is_err());assert_eq!(d,snapshot);
    lifecycle::commit_stop_checkpoint(&mut d,&drain,&transfer).unwrap();
    let snapshot=d.clone();assert!(crate::runtime_lifecycle_app::startup_recovery(&mut d).is_err());assert_eq!(d,snapshot);
}
#[test]
fn eager_media_reducer_rewrites_this_queue_so_deferral_assertion_is_material() {
    let(mut d,_,_)=successor();let raw=d["jobs"][0].clone();
    // Isolate the exact unstarted row: no filesystem/artifact access or paid call.
    d["jobs"]=json!([raw.clone()]);
    crate::media_queue::recover(&mut d,"2026-10-04T00:00:01Z").unwrap();
    assert_ne!(d["jobs"][0],raw,"fixture must detect accidental eager media recovery");
}

#[tokio::test]
async fn sqlite_app_media_writer_fences_reclaims_and_allows_existing_settlement() {
    let(app,_temp)=crate::tests::test_app().await;
    let source=fixture().0["jobs"][0].clone();
    app.change(|d| {
        for id in ["transactional-queued","transactional-running-complete","transactional-running-pause"] {
            let mut job=source.clone();job["id"]=json!(id);
            job["connectorBinding"]=d["connectorBinding"].clone();job["account"]=d["account"].clone();
            crate::list_mut(d,"jobs").push(job);
        }
        Ok(())
    }).await.unwrap();
    // Actual Running writer admission, not a fabricated lifecycle phase flip.
    app.change_media(|d| {
        crate::row_mut(d,"jobs","transactional-running-complete")?["status"]=json!("running");
        crate::row_mut(d,"jobs","transactional-running-pause")?["status"]=json!("running");
        Ok(())
    }).await.unwrap();
    let metadata=app.db.read_metadata().await.unwrap();
    let owner=lifecycle::bound_admission_token(&metadata,&app.lifecycle_owner,AdmissionClass::Media).unwrap();
    app.db.change_runtime_lifecycle_with_ledger(|d| {
        lifecycle::begin_drain_for_release(d,&owner,&target(),"sqlite-writer-drain")
    }).await.unwrap();
    let frozen=app.db.read().await.unwrap();let frozen_bytes=serde_json::to_vec(&frozen).unwrap();
    let raw_queue=crate::row(&frozen,"jobs","transactional-queued").unwrap().clone();
    assert!(app.change_media(|d| {
        crate::row_mut(d,"jobs","transactional-queued")?["status"]=json!("running");Ok(())
    }).await.is_err(),"existing queued row entering Running is a new admission");
    let actual=app.db.read().await.unwrap();assert_eq!(actual,frozen);assert_eq!(serde_json::to_vec(&actual).unwrap(),frozen_bytes);
    assert!(app.change_media(|d| {
        let mut next=raw_queue.clone();next["id"]=json!("transactional-new-queued");
        crate::list_mut(d,"jobs").push(next);Ok(())
    }).await.is_err(),"new queued JSON must not bypass closed admission");
    let actual=app.db.read().await.unwrap();assert_eq!(actual,frozen);assert_eq!(serde_json::to_vec(&actual).unwrap(),frozen_bytes);
    app.change_media(|d| {
        crate::row_mut(d,"jobs","transactional-running-complete")?["status"]=json!("completed");
        crate::row_mut(d,"jobs","transactional-running-pause")?["status"]=json!("paused");
        Ok(())
    }).await.unwrap();
    let settled=app.db.read().await.unwrap();
    assert_eq!(crate::row(&settled,"jobs","transactional-running-complete").unwrap()["status"],"completed");
    assert_eq!(crate::row(&settled,"jobs","transactional-running-pause").unwrap()["status"],"paused");
    assert_eq!(crate::row(&settled,"jobs","transactional-queued").unwrap(),&raw_queue);
    let mut expected=frozen;
    crate::row_mut(&mut expected,"jobs","transactional-running-complete").unwrap()["status"]=json!("completed");
    crate::row_mut(&mut expected,"jobs","transactional-running-pause").unwrap()["status"]=json!("paused");
    assert_eq!(settled,expected,"settlement changes only already admitted rows");
    assert_eq!(lifecycle::status(&settled).unwrap()["phase"],"draining");
    assert!(lifecycle::require_admission(&settled,&owner,AdmissionClass::Media).is_err());
    app.db.close().await;
}