//! Connected writer-route fixtures; UNRUN in this offline source worker.
use super::*;
fn reference(d:&Value,hash:&str)->Value {
    json!({"version":1,"kind":"native-paid-capture-ref",
        "company":crate::accounts::Profile::from_workspace(d).unwrap().key(),"account":d["account"],
        "binding":{"nativeJobId":"paid-job","operation":"assistant"},
        "runtimeOwner":{"account":d["account"],"runtimeId":"old-runtime","releaseSha256":"a".repeat(64)},
        "requestSha256":"b".repeat(64),"responseSha256":"c".repeat(64),
        "artifact":{"sha256":hash,"bytes":100},"retryAuthorized":false,"dispatchAuthorized":false})
}
async fn exercise(db:&Database) {
    db.change(|d| {
        let first=reference(d,&"d".repeat(64));let second=reference(d,&"e".repeat(64));
        d["jobs"]=json!([{"id":"paid-job","kind":"assistant","refId":"item","status":"running",
            "retainedEvidence":[first,second],"result":{"largeLegacy":"preserved"}},
            {"id":"other-job","kind":"sync","status":"completed","refId":""}]);Ok(())
    }).await.unwrap();
    let before=db.read().await.unwrap();
    for mode in ["remove","clear","rebind","reverse","foreign-append"] {
        assert!(db.change_job_observed("paid-job",|d| {
            let foreign=reference(d,&"f".repeat(64));let job=crate::row_mut(d,"jobs","paid-job")?;
            match mode {
                "remove"=>{job.as_object_mut().unwrap().remove("retainedEvidence");},
                "clear"=>{job["retainedEvidence"]=json!([]);},
                "rebind"=>{job["retainedEvidence"][0]["binding"]["operation"]=json!("media");},
                "reverse"=>{job["retainedEvidence"].as_array_mut().unwrap().reverse();},
                _=>{let mut foreign=foreign;foreign["account"]=json!("another-company");
                    job["retainedEvidence"].as_array_mut().unwrap().push(foreign);},
            }Ok(())
        }).await.is_err(),"{mode}");
        assert_eq!(db.read().await.unwrap(),before,"failed exact-job mutation rolls back {mode}");
    }
    let (_,changed)=db.change_job_observed("paid-job",|d| {
        let next=reference(d,&"f".repeat(64));let job=crate::row_mut(d,"jobs","paid-job")?;
        job["retainedEvidence"].as_array_mut().unwrap().push(next);
        job["status"]=json!("completed");Ok(())
    }).await.unwrap();
    assert!(changed);
    let after=db.read().await.unwrap();
    assert_eq!(after["jobs"][0]["retainedEvidence"][0],before["jobs"][0]["retainedEvidence"][0]);
    assert_eq!(after["jobs"][0]["retainedEvidence"].as_array().unwrap().len(),3);
    assert_eq!(after["jobs"][0]["result"],before["jobs"][0]["result"]);
    assert_eq!(after["jobs"][1],before["jobs"][1]);
    for table in ["items","operations","approvals","audit"] {assert_eq!(after[table],before[table]);}
}
#[tokio::test]
async fn sqlite_exact_job_preserves_paid_history_via_full_workspace_guard() {
    let folder=tempfile::tempdir().unwrap();
    let db=Database::Sqlite(crate::open_db(&folder.path().join("workspace.sqlite")).await.unwrap());
    exercise(&db).await;db.close().await;
}
#[tokio::test]
#[ignore="ROOT-only fresh disposable writer_v51 PostgreSQL fixture; run alone"]
async fn postgres_exact_job_preserves_paid_history_via_narrow_scope_guard() {
    let db=super::super::writer_v51_fixture_db().await;exercise(&db).await;db.close().await;
}
#[test]
fn borrowed_protected_members_preserve_null_missing_nested_and_identity_semantics() {
    let before=json!({"sync":{},"jobs":[{"full":"large"}],"items":[],"account":"LikeAvto",
        "protected":{"ordered":[1,null,true,{"x":"original"}]}});
    let mut after=before.clone();after["jobs"][0]["full"]=json!("changed");
    assert!(same_except(&before,&after,&["sync","jobs","items"]));
    after["protected"]["ordered"][3]["x"]=json!("changed");
    assert!(!same_except(&before,&after,&["sync","jobs","items"]));
    let mut after=before.clone();after["newReadonly"]=Value::Null;
    assert!(!same_except(&before,&after,&["sync","jobs","items"]));
    after=before.clone();after.as_object_mut().unwrap().remove("protected");
    assert!(!same_except(&before,&after,&["sync","jobs","items"]));
    assert!(!same_except(&before,&Value::Null,&["sync","jobs","items"]));
}
