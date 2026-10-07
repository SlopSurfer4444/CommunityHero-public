use super::*;
use crate::conductor_authority::{Context,with_context};

async fn negative_receipt_roundtrip(db:&Database){
    let mut d=crate::empty();normalize(&mut d);
    crate::native_fixture_owner_repair::initialize_workspace(&mut d).unwrap();
    crate::connection_gate::fixture_open(&mut d).unwrap();
    d["approvals"]=json!([{"id":"bounded-negative-approval","status":"approved","proposals":[]}]);
    db.change(|stored|{*stored=d;Ok(())}).await.unwrap();
    let actor=crate::operator_auth::Actor::local_owner("isolated-negative-test");
    let body=json!({"requestId":"bounded-negative-request","approvalId":"bounded-negative-approval"});
    let scope=super::super::AdmissionScope::Execute{approval:"bounded-negative-approval",body:&body};
    let before=db.read().await.unwrap();
    db.change_admission_observed(scope,|d|{
        let request=crate::local_admission::request(d,"execute",&body,&actor)?.unwrap();
        crate::local_admission::reject_execute(d,&request,&actor,"bounded-negative-approval","dependency",vec![json!("held-original-job")])
    }).await.unwrap();
    let first=db.read_local_admission_receipt("execute","bounded-negative-request").await.unwrap().unwrap();
    assert_eq!(first["evaluationIndex"],1);assert_eq!(first["noAttemptProof"]["operationCreated"],false);
    let snapshot=db.read().await.unwrap();
    for key in ["jobs","operations","approvals","proposals"] {assert_eq!(snapshot[key],before[key],"{key}");}
    let rejected=db.change_admission_observed(scope,|d|{
        let request=crate::local_admission::request(d,"execute",&body,&actor)?.unwrap();
        let outcome=crate::local_admission::reject_execute(d,&request,&actor,"bounded-negative-approval","dependency",vec![])?;
        crate::list_mut(d,"operations").push(json!({"id":"forged-effect","approvalId":"bounded-negative-approval","status":"dispatching"}));Ok(outcome)
    }).await;
    assert!(rejected.is_err());assert_eq!(db.read().await.unwrap(),snapshot,"invalid mixed effect+negative must rollback");
    db.change_admission_observed(scope,|d|{
        let request=crate::local_admission::request(d,"execute",&body,&actor)?.unwrap();
        crate::local_admission::reject_execute(d,&request,&actor,"bounded-negative-approval","dependency",vec![])
    }).await.unwrap();
    let second=db.read_local_admission_receipt("execute","bounded-negative-request").await.unwrap().unwrap();
    assert_eq!(second["evaluationIndex"],2);assert_eq!(second["parentEvaluationId"],first["evaluationId"]);
    assert_eq!(second["parentReceiptSha256"],first["receiptSha256"]);
    db.change(|d|{
        let request=crate::local_admission::request(d,"execute",&body,&actor)?.unwrap();
        crate::local_admission::commit(d,&request,&mut json!({"jobId":"original-job","approvalId":"bounded-negative-approval"}))
    }).await.unwrap();
    let positive=db.read_local_admission_receipt("execute","bounded-negative-request").await.unwrap().unwrap();
    assert_eq!(positive["action"],crate::local_admission::ACTION,"original committed positive wins over prior negatives");
}

#[tokio::test]
async fn sqlite_negative_admission_roundtrip_is_bounded_effect_free_and_positive_wins(){
    let temp=tempfile::tempdir().unwrap();let db=Database::Sqlite(crate::open_db(&temp.path().join("negative.sqlite")).await.unwrap());
    negative_receipt_roundtrip(&db).await;db.close().await;
}

#[tokio::test]
#[ignore="requires fresh isolated PostgreSQL fixture; run selector alone"]
async fn postgres_negative_admission_roundtrip_is_bounded_effect_free_and_positive_wins(){
    let db=super::super::preparation::writer_v51_fixture_db().await;
    negative_receipt_roundtrip(&db).await;db.close().await;
}

fn fixture()->(Value,Context){
    let mut d=crate::engine_prepare::tests::fixture(false);normalize(&mut d);
    let actor=crate::operator_auth::Actor{id:"receipt-owner".into(),name:"Receipt Owner".into(),role:"operator".into(),csrf_token:"offline".into(),authority_generation:Some("a".repeat(64))};
    let input=json!({"mode":"prepare","scope":{"itemIds":["ready"]},"actionKinds":[]});
    let grant=crate::conductor_authority::create_grant(&d,&actor,&input).unwrap();
    d["jobs"]=json!([{"id":"receipt-run","kind":"conductor","status":"running","account":d["account"],"connectorBinding":d["connectorBinding"],
        "conductor":{"version":1,"desiredState":"running","leaseGeneration":1,"mode":"prepare","scope":input["scope"],"grant":grant}}]);
    (d,Context{run_id:"receipt-run".into(),lease_generation:1,actor})
}

fn schedule_and_commit(d:&mut Value,actor:&crate::operator_auth::Actor,key:&str)->ApiResult<String>{
    let body=json!({"requestId":key,"itemIds":["ready"]});
    let admission=crate::local_admission::request(d,"prepare",&body,actor)?.unwrap();
    let first=crate::list(d,"jobs").len();
    let scheduled=crate::engine_prepare::schedule(d,crate::engine_prepare::parse(&body)?)?;
    crate::conductor_authority::fence_new_jobs(d,first)?;
    crate::local_admission::commit(d,&admission,&mut json!({"jobId":scheduled.job_id}))?;
    Ok(scheduled.job_id)
}

#[tokio::test]
async fn actual_committed_prepare_receipt_accepts_exact_pair_and_rejects_foreign_or_partial_attribution(){
    let (mut d,ctx)=fixture();
    let (job,receipt)=with_context(ctx.clone(),async {
        let key=schedule_and_commit(&mut d,&ctx.actor,"tagged-prepare").unwrap();
        let job=crate::row(&d,"jobs",&key).unwrap().clone();
        let receipt=crate::local_admission::find_receipt(&d,"prepare","tagged-prepare").unwrap().unwrap().clone();
        validate_prepare_receipt(&receipt,&job,&d).unwrap();
        assert_eq!(receipt.as_object().unwrap().len(),14);
        for change in ["missing_run","missing_epoch","foreign_run","future_epoch","actor","extra","job","paused","scope"]{
            let mut r=receipt.clone();let mut j=job.clone();let mut w=d.clone();
            match change{
                "missing_run"=>{r.as_object_mut().unwrap().remove("conductorRunId");},
                "missing_epoch"=>{r.as_object_mut().unwrap().remove("grantGeneration");},
                "foreign_run"=>r["conductorRunId"]=json!("other-run"),
                "future_epoch"=>r["grantGeneration"]=json!(2),
                "actor"=>r["actorId"]=json!("other-actor"),
                "extra"=>r["extra"]=json!(true),
                "job"=>j["grantGeneration"]=json!(2),
                "paused"=>crate::row_mut(&mut w,"jobs",&ctx.run_id).unwrap()["conductor"]["desiredState"]=json!("paused"),
                _=>j["selectedItemIds"]=json!(["outside-grant"]),
            }
            assert!(validate_prepare_receipt(&r,&j,&w).is_err(),"{change}");
        }
        (job,receipt)
    }).await;
    assert!(validate_prepare_receipt(&receipt,&job,&d).is_err(),"tagged receipt requires current task-local grant");
    let (mut legacy,_)=fixture();legacy["jobs"]=json!([]);
    let actor=crate::operator_auth::Actor::local_owner("legacy-receipt");
    let key=schedule_and_commit(&mut legacy,&actor,"legacy-prepare").unwrap();
    let receipt=crate::local_admission::find_receipt(&legacy,"prepare","legacy-prepare").unwrap().unwrap();
    assert_eq!(receipt.as_object().unwrap().len(),12);
    validate_prepare_receipt(receipt,crate::row(&legacy,"jobs",&key).unwrap(),&legacy).unwrap();
}

#[tokio::test]
async fn sqlite_scheduling_with_real_commit_persists_job_and_receipt_atomically(){
    let temp=tempfile::tempdir().unwrap();let db=Database::Sqlite(crate::open_db(&temp.path().join("receipt.sqlite")).await.unwrap());
    let (full,ctx)=fixture();db.change(|d|{*d=full;Ok(())}).await.unwrap();let before=db.read().await.unwrap();
    let actor=ctx.actor.clone();
    let failed=with_context(ctx.clone(),db.change_preparation_schedule_observed(|d|{
        schedule_and_commit(d,&actor,"forged-receipt")?;
        crate::list_mut(d,"audit").last_mut().unwrap()["grantGeneration"]=json!(2);Ok(())
    })).await;
    assert!(failed.is_err());assert_eq!(db.read().await.unwrap(),before);
    assert!(db.read_local_admission_receipt("prepare","forged-receipt").await.unwrap().is_none());
    let (child,changed)=with_context(ctx.clone(),db.change_preparation_schedule_observed(|d|schedule_and_commit(d,&actor,"scoped-receipt"))).await.unwrap();
    assert!(changed);let after=db.read().await.unwrap();
    let receipt=db.read_local_admission_receipt("prepare","scoped-receipt").await.unwrap().unwrap();
    assert_eq!(receipt["conductorRunId"],ctx.run_id);assert_eq!(receipt["grantGeneration"],1);assert_eq!(receipt["result"]["jobId"],child);
    assert_eq!(crate::row(&after,"jobs",&ctx.run_id).unwrap(),crate::row(&before,"jobs",&ctx.run_id).unwrap());
    for table in ["items","posts","branches","proposals","operations","approvals"]{assert_eq!(after[table],before[table],"{table}");}
    db.close().await;
}
