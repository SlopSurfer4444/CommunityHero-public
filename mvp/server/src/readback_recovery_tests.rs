use super::*;

pub(crate) fn operation(at:i64)->Value {
    json!({"id":"op","itemId":"item-1","proposalId":"p","approvalId":"approval","attemptId":"dispatch-attempt",
        "status":"unknown","createdAt":chrono::DateTime::from_timestamp(at,0).unwrap().to_rfc3339(),
        "action":{"actionId":"op","action":"close","itemId":"comment-1","objectId":"11391","conversationKey":"11391:comment-1",
            "contextEvidenceDigest":"a".repeat(64),"expectedStatuses":["new"],"workTime":0},
        "target":{"connectorBinding":accounts::Profile::LikeAvto.binding(),"id":"item-1","itemId":"comment-1","objectId":"11391",
            "postKey":"11391:post-1","conversationKey":"11391:comment-1","contextEvidenceDigest":"a".repeat(64)}})
}
fn metadata()->Value {json!({"account":"LikeAvto","connectorBinding":accounts::Profile::LikeAvto.binding()})}

#[test]
fn retry_budget_delay_clock_and_restart_budget_are_durable_inputs() {
    let op=operation(1000);let meta=metadata();
    assert!(planned_job(&meta,&op,false,0,None,1029,None).is_none());
    let first=planned_job(&meta,&op,false,0,None,1030,None).unwrap();
    assert_eq!(first["kind"],"reconcile");assert_eq!(first["readbackOnly"],true);
    assert_eq!(first["automaticReadback"]["attempt"],1);
    let previous=chrono::DateTime::from_timestamp(1030,0).unwrap().to_rfc3339();
    assert!(planned_job(&meta,&op,false,1,Some(&previous),1149,None).is_none());
    assert_eq!(planned_job(&meta,&op,false,1,Some(&previous),1150,None).unwrap()["automaticReadback"]["attempt"],2);
    assert!(planned_job(&meta,&op,false,2,Some(&previous),1629,None).is_none());
    assert_eq!(planned_job(&meta,&op,false,2,Some(&previous),1630,None).unwrap()["automaticReadback"]["attempt"],3);
    assert!(planned_job(&meta,&op,false,3,Some(&previous),5000,None).is_none());
    assert!(planned_job(&meta,&op,false,1,None,5000,None).is_none());
    // Old UNKNOWN operations can enter the same bounded recovery after runtime
    // admission; age alone is neither evidence of success nor a retry budget.
    assert!(planned_job(&meta,&op,false,0,None,1000+86400*3,None).is_some());
    assert!(planned_job(&meta,&op,false,0,None,999,None).is_none());
    assert!(planned_job(&meta,&op,true,0,None,1100,None).is_none());
}

#[test]
fn scope_binding_action_and_reply_identity_are_never_inferred() {
    let meta=metadata();let original=operation(1000);
    for (index,mut op) in [original.clone(),original.clone(),original.clone(),original.clone()].into_iter().enumerate() {
        match index {0=>op["status"]=json!("succeeded"),1=>op["target"]["connectorBinding"]=accounts::Profile::BawRussia.binding(),
            2=>op["action"]["itemId"]=json!("other"),_=>op["target"]["connectorBinding"]=Value::Null};
        assert!(planned_job(&meta,&op,false,0,None,1100,None).is_none());
    }
    let mut reply=original;reply["action"]["action"]=json!("reply_and_close");reply["action"]["reply"]=json!("approved reply");
    assert!(planned_job(&meta,&reply,false,0,None,1100,None).is_none());
    reply["action"]["readbackEvidence"]=json!({"baselineReplyIds":[]});
    assert!(planned_job(&meta,&reply,false,0,None,1100,None).is_some());
    reply["action"]["readbackEvidence"]=json!({"baselineReplyIds":["same","same"]});
    assert!(planned_job(&meta,&reply,false,0,None,1100,None).is_none());
}

fn observation(code:&str,http:Option<u64>)->Value {
    json!({"account":"likeavto","operation":"readback","results":[{"actionId":"op","itemId":"comment-1","status":"unknown","code":code,"httpStatus":http}]})
}

#[test]
fn permanent_failures_are_held_and_transient_observations_respect_retry_after() {
    let meta=metadata();let mut op=operation(1000);
    for (code,http,expected) in [("READBACK_NOT_VERIFIED",None,true),("TRANSPORT_ERROR",None,true),("HTTP_ERROR",Some(429),true),
        ("HTTP_ERROR",Some(500),true),("HTTP_ERROR",Some(400),false),("ACCOUNT_SCOPE_MISMATCH",None,false),("RESPONSE_SCHEMA_ERROR",None,false)] {
        op["evidence"]=observation(code,http);
        assert_eq!(planned_job(&meta,&op,false,0,None,1100,None).is_some(),expected,"{code}/{http:?}");
    }
    op["evidence"]=observation("HTTP_ERROR",Some(429));op["evidence"]["results"][0]["retryAfterMs"]=json!(300001);
    assert!(planned_job(&meta,&op,false,0,None,1300,None).is_none());
    assert!(planned_job(&meta,&op,false,0,None,1301,None).is_some());
    op["evidence"]["account"]=json!("baw-russia");assert!(planned_job(&meta,&op,false,0,None,1500,None).is_none());
    op["evidence"]=observation("READBACK_NOT_VERIFIED",None);
    op["evidence"]["results"][0]["readbackObservation"]=json!({"itemIdentityMatches":false});
    assert!(planned_job(&meta,&op,false,0,None,1500,None).is_none());
    for (error,expected) in [("Adapter failed (ADAPTER_PROCESS_FAILED; exitCode=3221226505)",true),("Adapter failed (ADAPTER_TIMEOUT)",true),
        ("Adapter failed (ACCOUNT_SCOPE_MISMATCH)",false),("Adapter failed (CANCELLED)",false),("Invalid adapter response",false),
        ("Adapter process failed (stage=exit; code=Some(-1073740791); codeHex=0xC0000409)",true),
        ("Adapter timed out; action outcome may be unknown",true),("Adapter process failed (stage=stdout; category=output_limit)",false)] {
        op["evidence"]=json!({"phase":"readback","error":error});
        assert_eq!(planned_job(&meta,&op,false,0,None,1100,None).is_some(),expected);
    }
}

#[test]
fn first_retry_delay_starts_at_latest_observation_not_old_operation_creation() {
    let meta=metadata();let mut op=operation(1000);
    op["updatedAt"]=json!(chrono::DateTime::from_timestamp(4000,0).unwrap().to_rfc3339());
    op["evidence"]=observation("HTTP_ERROR",Some(429));op["evidence"]["results"][0]["retryAfterMs"]=json!(300000);
    assert!(planned_job(&meta,&op,false,0,None,4299,None).is_none());
    assert!(planned_job(&meta,&op,false,0,None,4300,None).is_some());
}

#[test]
fn abnormal_execution_exit_is_readback_eligible_without_replaying_worker() {
    let mut op=operation(1000);
    op["evidence"]=json!({"workerExit":{"code":"worker_panicked","requiresReadback":true,"providerRetryAllowed":false}});
    assert!(planned_job(&metadata(),&op,false,0,None,1100,None).is_some());
    op["evidence"]["workerExit"]["requiresReadback"]=json!(false);
    assert!(planned_job(&metadata(),&op,false,0,None,1100,None).is_none());
}

#[tokio::test]
async fn competing_manual_and_automatic_claims_create_only_one_job() {
    let (app,_temp)=crate::tests::test_app().await;
    app.change(|d|{d["operations"]=json!([operation(chrono::Utc::now().timestamp()-60)]);Ok(())}).await.unwrap();
    let (automatic,manual)=tokio::join!(claim(&app,"op",None),claim(&app,"op",Some(json!({"id":"owner"}))));
    assert_eq!(usize::from(automatic.unwrap().is_some())+usize::from(manual.unwrap().is_some()),1);
    assert_eq!(app.db.read().await.unwrap()["jobs"].as_array().unwrap().len(),1);
}

#[test]
fn manual_readback_does_not_reset_automatic_budget_or_override_scope() {
    let meta=metadata();let mut op=operation(1000);let actor=json!({"id":"owner"});
    op["action"]["action"]=json!("reply_and_close");
    let job=planned_job(&meta,&op,false,3,None,1000+86400*3,Some(&actor)).unwrap();
    assert_eq!(job["requestedBy"],actor);assert!(job.get("automaticReadback").is_none());
    assert!(planned_job(&meta,&op,true,0,None,1100,Some(&actor)).is_none());
    op["target"]["connectorBinding"]=accounts::Profile::BawRussia.binding();
    assert!(planned_job(&meta,&op,false,0,None,1100,Some(&actor)).is_none());
}

#[test]
fn startup_admission_is_bound_to_process_company_manifest_and_attempt() {
    let token="a".repeat(32);let manifest="b".repeat(64);
    let original=json!({"version":1,"token":token,"account":"baw-russia","pid":42,"manifestSha256":manifest,"admittedAtUtc":"2026-09-24T12:00:00Z"});
    let admitted=|value:&Value|admitted_receipt(value.to_string().as_bytes(),"baw-russia",42,&token,&manifest);
    assert!(admitted(&original));
    for (field,value) in [("version",json!(2)),("token",json!("c".repeat(32))),("account",json!("likeavto")),("pid",json!(43)),
        ("manifestSha256",json!("c".repeat(64))),("admittedAtUtc",json!("invalid"))] {
        let mut foreign=original.clone();foreign[field]=value;assert!(!admitted(&foreign),"{field}");
    }
    assert!(!admitted_receipt(b"{}","baw-russia",42,&token,&manifest));
    assert!(!admitted_receipt(b"{partial","baw-russia",42,&token,&manifest));
    assert!(!admitted_receipt(&vec![b' ';4097],"baw-russia",42,&token,&manifest));
    assert!(!admitted_receipt(original.to_string().as_bytes(),"baw-russia",42,"",&manifest));
}

#[tokio::test]
async fn claimed_worker_calls_readback_only_and_persists_verified_result() {
    let (mut app,temp)=crate::tests::test_app().await;
    let op=operation(chrono::Utc::now().timestamp()-60);
    app.change(|d|{d["operations"]=json!([op]);d["proposals"]=json!([{"id":"p","itemId":"item-1","status":"unknown"}]);Ok(())}).await.unwrap();
    let log=temp.path().join("readback-calls.jsonl");
    app.node=PathBuf::from("node");app.bridge=temp.path().join("readback-only-fixture.mjs");
    let source=r#"import {appendFile} from 'node:fs/promises';let raw='';for await(const chunk of process.stdin)raw+=chunk;const request=JSON.parse(raw);await appendFile(__LOG__,request.operation+'\n');if(request.operation!=='readback')process.exit(19);process.stdout.write(JSON.stringify({ok:true,result:{account:request.account,results:request.actions.map(a=>({actionId:a.actionId,itemId:a.itemId,status:'verified'}))}}));"#.replace("__LOG__",&json!(log.to_string_lossy()).to_string());
    std::fs::write(&app.bridge,source).unwrap();
    let job=manual(&app,"op",json!({"id":"owner"})).await.unwrap();
    tokio::time::timeout(Duration::from_secs(15),async {
        loop {if app.db.read_job(&job).await.unwrap().unwrap()["status"]=="completed"{break;}tokio::time::sleep(Duration::from_millis(20)).await;}
    }).await.unwrap();
    assert_eq!(std::fs::read_to_string(log).unwrap(),"readback\n");
    let stored=app.db.read().await.unwrap();assert_eq!(stored["operations"][0]["status"],"succeeded");
    assert_eq!(stored["operations"][0]["action"],op["action"]);
    assert!(claim(&app,"op",None).await.unwrap().is_none());
}
