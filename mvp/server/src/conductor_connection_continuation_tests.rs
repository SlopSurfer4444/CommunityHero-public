use super::*;
const GENERATION:&str="11111111-1111-4111-8111-111111111111";

async fn fixture()->(App,tempfile::TempDir,String) {
    crate::conductor::tests::fixture_with_scope(operator_auth::Actor::local_owner("continuation-fixture"),"execute",Some(GENERATION)).await
}
// Isolated test-only projection, never a production admission or release gate.
fn fixture_proof(d:&Value)->Value {
    let gate=&d[connection_gate::FIELD];let hash="a".repeat(64);
    json!({"version":1,"kind":"verified-connection-continuation-admission",
        "caseSha256":hash,"reopenReceiptSha256":gate["reopenReceiptSha256"],"gateEpoch":gate["gateEpoch"],
        "owner":gate["owner"],"connectionBinding":gate["connectionBinding"],"storageGeneration":d["storageGeneration"],
        "protectedGeneration":gate["availability"]["generation"],"protectedReceiptSha256":gate["availability"]["receiptSha256"],
        "availabilitySha256":connection_gate::digest(&gate["availability"]),"archiveFenceSha256":connection_gate::digest(&d[external_reconciliation::FIELD]),
        "lifecycleReceiptSha256":hash,"archiveRestoreReceiptSha256":hash})
}
async fn ready_fixture()->(App,tempfile::TempDir,Launch,Value) {
    let (app,temp,run)=fixture().await;
    let proof=app.change(|d|{let proof=fixture_proof(d);d[connection_gate::FIELD]["admittedContinuationProof"]=proof.clone();Ok(proof)}).await.unwrap();
    assert_eq!(connection_gate::current_continuation_admission(&app.read().await.unwrap()).unwrap(),Some(proof.clone()));
    app.change_job(&run,|d|{
        let marker=selected(row(d,"jobs",&run)?,proof.clone(),Value::Null)?;
        row_mut(d,"jobs",&run)?["conductor"][FIELD]=marker;Ok(())
    }).await.unwrap();
    let launch=crate::conductor::prepare_child(&app,&run).await.unwrap();
    assert_eq!(launch.continuation_claim["firstLaunch"],true);
    assert!(app.db.read_job(&run).await.unwrap().unwrap()["conductor"][FIELD].is_null());
    (app,temp,launch,proof)
}
async fn reject(app:&App,launch:&Launch)->Value {
    let ctx=conductor_authority::Context{run_id:launch.run_id.clone(),lease_generation:launch.lease_generation,actor:launch.actor.clone()};
    conductor_authority::with_context(ctx,app.change(|d|{
        let body=json!({"approvalId":"approval-one","requestId":"execute-one"});
        let request=local_admission::request(d,"execute",&body,&launch.actor)?.unwrap();
        local_admission::reject_execute(d,&request,&launch.actor,"approval-one","dependency",vec![])
    })).await.unwrap()
}
async fn saved_negative(launch:&Launch,dto:&Value) {
    let directory=PathBuf::from(format!("{}.slices",launch.checkpoint_path.display()));tokio::fs::create_dir_all(&directory).await.unwrap();
    let path=directory.join("original.json");let binding=&dto["executeRejection"];
    let child=json!({"schemaVersion":1,"account":launch.account,"baseUrl":launch.base_url,"phase":"stopped",
        "approvalId":binding["approvalId"],"executeRequestId":binding["requestId"],
        "pendingLocalAdmission":{"kind":"execute","requestId":binding["requestId"],"payloadHash":binding["payloadHash"]},
        "error":{"code":"REJECTED_LOCAL_ADMISSION","rejection":binding}});
    tokio::fs::write(&path,child.to_string()).await.unwrap();
    let mut root:Value=serde_json::from_slice(&tokio::fs::read(&launch.checkpoint_path).await.unwrap()).unwrap();
    root["connectionDependency"]=dto.clone();root["currentSlice"]=json!({"childPath":path});
    tokio::fs::write(&launch.checkpoint_path,root.to_string()).await.unwrap();
}

#[tokio::test]
async fn closed_startup_preserves_new_original_run_and_missing_unstarted_journal() {
    let (app,_temp,run)=fixture().await;let before=app.read().await.unwrap();
    crate::conductor::recover(&app).await.unwrap();
    let after=app.read().await.unwrap();let job=row(&after,"jobs",&run).unwrap();
    assert_eq!(job["conductor"]["leaseGeneration"],1);assert_eq!(job["conductor"]["childEverStarted"],false);
    assert_eq!(job["conductor"][FIELD]["state"],"waiting_connection");
    assert_eq!(job["conductor"][FIELD]["firstLaunch"],true);assert_eq!(job["status"],"waiting_dependency");
    assert!(!tokio::fs::try_exists(app.data.join("conductor").join(&run).join("queue.json")).await.unwrap());
    for collection in ["approvals","proposals","operations","audit"]{assert_eq!(after[collection],before[collection]);}
    wake_deferred(&app).await.unwrap();assert_eq!(app.read().await.unwrap(),after,"repeat closed notification changes no lease or record");
    app.change_job(&run,|d|{row_mut(d,"jobs",&run)?["conductor"]["childEverStarted"]=json!(true);row_mut(d,"jobs",&run)?["conductor"][FIELD]["firstLaunch"]=json!(false);Ok(())}).await.unwrap();
    let corrupt=app.read().await.unwrap();assert!(wake_deferred(&app).await.is_err());assert_eq!(app.read().await.unwrap(),corrupt);
}
#[tokio::test]
async fn duplicate_claims_cannot_drop_the_active_guard_or_increment_lease() {
    let (app,_temp,run)=fixture().await;let before=app.read().await.unwrap();
    let registration=conductor_child::claim(&app,&run).unwrap();
    let mut tasks=Vec::new();
    for _ in 0..12 {let app=app.clone();let run=run.clone();tasks.push(tokio::spawn(async move{continue_run(&app,&run,Reason::Recover).await}));}
    for task in tasks {task.await.unwrap().unwrap();}
    assert!(conductor_child::claim(&app,&run).is_none());assert_eq!(app.read().await.unwrap(),before);
    drop(registration);assert!(conductor_child::claim(&app,&run).is_some());
}
#[tokio::test]
async fn first_launch_negative_keeps_selected_tuple_and_checkpoint_without_new_attempt() {
    let (app,_temp,launch,proof)=ready_fixture().await;
    let rejection=reject(&app,&launch).await;let before=app.read().await.unwrap();
    let dto=dependency_for_rejection(&app,&launch,"approval-one","execute-one").await.unwrap();
    saved_negative(&launch,&dto).await;let journal=tokio::fs::read(&launch.checkpoint_path).await.unwrap();
    assert_eq!(dto["gateObservation"]["state"],"open","known negative remains typed even after reopen race");
    assert_eq!(dto["executeRejection"]["receiptSha256"],rejection["receiptSha256"]);
    defer_child(&app,&launch,&dto).await.unwrap();
    let after=app.read().await.unwrap();let job=row(&after,"jobs",&launch.run_id).unwrap();let marker=&job["conductor"][FIELD];
    assert_eq!(marker["state"],"waiting_owner");assert_eq!(marker["firstLaunch"],true);
    assert_eq!(marker["priorLeaseGeneration"],1);assert_eq!(marker["selectedLeaseGeneration"],1);
    assert_eq!(job["conductor"]["childEverStarted"],true);assert_eq!(marker["acceptedAdmission"],proof);
    wake_deferred(&app).await.unwrap();assert_eq!(app.read().await.unwrap(),after);
    for collection in ["jobs","operations","approvals","proposals","audit"] {
        if collection!="jobs"{assert_eq!(after[collection],before[collection]);}
    }
    assert_eq!(tokio::fs::read(&launch.checkpoint_path).await.unwrap(),journal);
    // The actual positive-observation reducer reuses this selected tuple even
    // after childEverStarted=true; it does not reseed or allocate lease2.
    let resumed=ready_marker(job,marker,proof,Value::Null).unwrap();
    assert_eq!(resumed["selectedLeaseGeneration"],1);assert_eq!(resumed["firstLaunch"],true);
    assert_eq!(resumed["state"],"ready_for_child");
}
#[tokio::test]
async fn active_reclose_is_a_new_fenced_claim_not_a_new_run_or_request() {
    let (app,_temp,launch,_)=ready_fixture().await;reject(&app,&launch).await;
    app.change(|d|connection_gate::request_close(d,"fixture-reclose","reauthentication").map(|_|())).await.unwrap();
    let dto=dependency_for_rejection(&app,&launch,"approval-one","execute-one").await.unwrap();
    saved_negative(&launch,&dto).await;
    defer_child(&app,&launch,&dto).await.unwrap();
    let closed=app.read().await.unwrap();let original=row(&closed,"jobs",&launch.run_id).unwrap();
    assert_eq!(original["conductor"][FIELD]["state"],"waiting_connection");
    assert_eq!(original["conductor"][FIELD]["firstLaunch"],false);assert_eq!(original["conductor"]["leaseGeneration"],1);
    let mut fresh=fixture_proof(&closed);fresh["gateEpoch"]=json!(fresh["gateEpoch"].as_u64().unwrap()+1);
    let selected=ready_marker(original,&original["conductor"][FIELD],fresh.clone(),dto["executeRejection"].clone()).unwrap();
    assert_eq!(selected["state"],"waiting_owner");assert_eq!(selected["priorLeaseGeneration"],1);assert_eq!(selected["selectedLeaseGeneration"],2);
    let mut job=original.clone();job["conductor"]["leaseGeneration"]=json!(2);job["conductor"][FIELD]=selected.clone();
    assert_eq!(ready_marker(&job,&selected,fresh,dto["executeRejection"].clone()).unwrap(),selected);
    assert_eq!(selected["dependency"]["requestId"],"execute-one");assert_eq!(selected["runId"],launch.run_id);
}
#[tokio::test]
async fn latest_negative_parent_is_required_and_unknown_is_never_dependency() {
    let (app,_temp,launch,_)=ready_fixture().await;let first=reject(&app,&launch).await;
    let stale=dependency_for_rejection(&app,&launch,"approval-one","execute-one").await.unwrap();
    saved_negative(&launch,&stale).await;
    let second=reject(&app,&launch).await;
    assert_eq!(second["parentEvaluationId"],first["evaluationId"]);assert_eq!(second["parentReceiptSha256"],first["receiptSha256"]);
    let before=app.read().await.unwrap();assert!(defer_child(&app,&launch,&stale).await.is_err());assert_eq!(app.read().await.unwrap(),before);
    assert!(dependency_for_rejection(&app,&launch,"approval-one","missing-or-unknown-key").await.is_err());
    assert!(dependency_for_rejection(&app,&launch,"foreign-approval","execute-one").await.is_err());
    assert_eq!(app.read().await.unwrap(),before);
}
#[tokio::test]
async fn actual_child_journal_not_its_stale_summary_proves_negative_and_unknown_veto() {
    let (app,_temp,launch,_)=ready_fixture().await;reject(&app,&launch).await;
    let dto=dependency_for_rejection(&app,&launch,"approval-one","execute-one").await.unwrap();
    assert!(defer_child(&app,&launch,&dto).await.is_err(),"unsaved final disposition is not evidence");
    saved_negative(&launch,&dto).await;
    let child_path=PathBuf::from(format!("{}.slices",launch.checkpoint_path.display())).join("original.json");
    let saved:Value=serde_json::from_slice(&tokio::fs::read(&child_path).await.unwrap()).unwrap();
    let mut unknown=saved.clone();unknown["phase"]=json!("unknown");unknown["error"]["code"]=json!("UNKNOWN_MUTATION_OUTCOME");
    tokio::fs::write(&child_path,unknown.to_string()).await.unwrap();
    let before=app.read().await.unwrap();assert!(defer_child(&app,&launch,&dto).await.is_err());assert_eq!(app.read().await.unwrap(),before);
    // A real bulk slice uses its own exact native execute locator. No rewrite
    // into classic pendingLocalAdmission is necessary or permitted.
    let bulk=json!({"schemaVersion":1,"kind":"communityhero-reviewed-bulk","slices":[{
        "admission":{"id":"approval-one"},"executeAttemptId":"execute-one","executeAdmissionProtocol":"local-admission-v1",
        "executePayloadHash":dto["executeRejection"]["payloadHash"],"error":saved["error"]}]});
    tokio::fs::write(&child_path,bulk.to_string()).await.unwrap();
    defer_child(&app,&launch,&dto).await.unwrap();
    assert_eq!(app.db.read_job(&launch.run_id).await.unwrap().unwrap()["conductor"][FIELD]["state"],"waiting_owner");
}
#[tokio::test]
async fn positive_lookup_requires_original_native_job_and_reuses_owner_selection() {
    let (app,_temp,launch,proof)=ready_fixture().await;reject(&app,&launch).await;
    let dto=dependency_for_rejection(&app,&launch,"approval-one","execute-one").await.unwrap();saved_negative(&launch,&dto).await;defer_child(&app,&launch,&dto).await.unwrap();
    let job=app.db.read_job(&launch.run_id).await.unwrap().unwrap();let marker=job["conductor"][FIELD].clone();
    let ctx=conductor_authority::Context{run_id:launch.run_id.clone(),lease_generation:launch.lease_generation,actor:launch.actor.clone()};
    // Model already committed native evidence with the real commit primitive,
    // rather than invoking provider dispatch or relabeling a negative receipt.
    conductor_authority::with_context(ctx.clone(),app.change(|d|{
        let execution=new_job(d,"execute","approval-one")?;conductor_authority::tag(&ctx,row_mut(d,"jobs",&execution)?);
        let body=json!({"approvalId":"approval-one","requestId":"execute-one"});
        let request=local_admission::request(d,"execute",&body,&launch.actor)?.unwrap();
        local_admission::commit(d,&request,&mut json!({"jobId":execution,"approvalId":"approval-one"}))
    })).await.unwrap();
    let committed=app.read().await.unwrap();assert!(negative_for(&app,&job,&marker["dependency"]).await.unwrap().is_none());
    assert_eq!(app.read().await.unwrap(),committed,"observation creates no second execute job or audit");
    let resumed=ready_marker(&job,&marker,proof,Value::Null).unwrap();
    assert_eq!(resumed["firstLaunch"],true);assert_eq!(resumed["selectedLeaseGeneration"],1);
    assert_eq!(resumed["priorLeaseGeneration"],1);assert_eq!(resumed["state"],"ready_for_child");
}
#[tokio::test]
async fn explicit_action_rejects_extra_context_foreign_actor_and_stale_parent_before_execute() {
    let (app,_temp,launch,_)=ready_fixture().await;let rejection=reject(&app,&launch).await;
    let dto=dependency_for_rejection(&app,&launch,"approval-one","execute-one").await.unwrap();saved_negative(&launch,&dto).await;defer_child(&app,&launch,&dto).await.unwrap();
    let body=json!({"approvalId":"approval-one","requestId":"execute-one","reevaluate":{"evaluationId":rejection["evaluationId"],"receiptSha256":rejection["receiptSha256"]}});
    let before=app.read().await.unwrap();
    for fault in ["caller-lease","foreign-actor","changed-role","old-parent"] {
        let mut body=body.clone();let mut actor=launch.actor.clone();
        match fault {"caller-lease"=>body["leaseGeneration"]=json!(1),"foreign-actor"=>actor.id="foreign-owner".into(),"changed-role"=>actor.role="operator".into(),_=>body["reevaluate"]["receiptSha256"]=json!("b".repeat(64))};
        let admitted=std::sync::atomic::AtomicBool::new(false);
        assert!(reevaluate_owned(&app,&actor,&launch.run_id,&body,&admitted).await.is_err(),"{fault}");
        assert!(!admitted.load(std::sync::atomic::Ordering::Acquire));assert_eq!(app.read().await.unwrap(),before);
    }
}
#[tokio::test]
async fn pending_scan_is_exact_and_ready_claim_does_not_bypass_local_hold() {
    let (app,_temp,launch,_)=ready_fixture().await;
    let mut d=app.read().await.unwrap();assert!(!has_deferred_connection_work(&d).unwrap());
    let job=row_mut(&mut d,"jobs",&launch.run_id).unwrap();job["conductor"][FIELD]=launch.continuation_claim.clone();
    assert!(has_deferred_connection_work(&d).unwrap());
    assert!(require_launch_projection(&app,&d,row(&d,"jobs",&launch.run_id).unwrap()).is_ok());
    dispatch_authority::hold_transport_failure(&app);
    assert!(require_launch_projection(&app,&d,row(&d,"jobs",&launch.run_id).unwrap()).is_err());
    let job=row_mut(&mut d,"jobs",&launch.run_id).unwrap();job["conductor"][FIELD]["acceptedAdmission"]["connectionBinding"]=json!({});
    assert!(has_deferred_connection_work(&d).is_err());
}
#[tokio::test]
async fn canceled_http_receiver_does_not_cancel_registered_native_action() {
    use std::{future::Future,task::Poll};
    let (app,_temp,run)=fixture().await;let before=app.read().await.unwrap();
    // Correct closed body, but no waiting_owner marker: owned native preflight
    // rejects without ever reaching execute. Poll once to create the owned task
    // and pending response receiver, then cancel only that receiver.
    let body=json!({"approvalId":"approval-one","requestId":"execute-one","reevaluate":{"evaluationId":"evaluation-one","receiptSha256":"a".repeat(64)}});
    let mut response=Box::pin(execute_reevaluate(State(app.clone()),Extension(operator_auth::Actor::local_owner("continuation-fixture")),Path(run.clone()),Json(body)));
    std::future::poll_fn(|cx|match response.as_mut().poll(cx){Poll::Pending=>Poll::Ready(()),Poll::Ready(_)=>panic!("owned preflight must await its database read")}).await;
    assert_eq!(app.lifecycle_task_count.load(std::sync::atomic::Ordering::SeqCst),1);
    assert!(conductor_child::claim(&app,&run).is_none());drop(response);
    tokio::time::timeout(Duration::from_secs(2),async {
        while app.lifecycle_task_count.load(std::sync::atomic::Ordering::SeqCst)>0{tokio::task::yield_now().await;}
    }).await.unwrap();
    assert!(conductor_child::claim(&app,&run).is_some());assert_eq!(app.lifecycle_work.snapshot().unwrap().active,0);
    assert_eq!(app.read().await.unwrap(),before,"canceled receiver does not infer execution or rewrite rejection");
}
