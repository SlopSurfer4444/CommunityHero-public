use super::*;

fn input()->Value {json!({"requestId":"fixed-request","scope":{"itemIds":["item-1"]},"mode":"prepare","actionKinds":[]})}
async fn fixture()->(App,tempfile::TempDir,String) {
    fixture_with_actor(operator_auth::Actor::local_owner("test-authorizer")).await
}
async fn fixture_with_actor(actor:operator_auth::Actor)->(App,tempfile::TempDir,String) {
    fixture_with_scope(actor,"prepare",None).await
}
pub(super) async fn fixture_with_scope(actor:operator_auth::Actor,mode:&str,generation:Option<&str>)->(App,tempfile::TempDir,String) {
    let (app,temp)=crate::tests::test_app().await;
    if let Some(generation)=generation {
        // Model reviewed pristine fixture provisioning BEFORE normal writers.
        // Production's immutable working-generation guard remains exercised;
        // an ordinary App::change must not mint a new database episode.
        let mut data=app.read().await.unwrap();data["storageGeneration"]=json!(generation);
        crate::working_generation::current(&data).unwrap();
        let Database::Sqlite(pool)=&app.db else{panic!("isolated pristine SQLite fixture required")};
        sqlx::query("UPDATE workspace SET payload=? WHERE id=1").bind(data.to_string()).execute(pool).await.unwrap();
    }
    let mut request=input();request["mode"]=json!(mode);
    if mode=="execute" {request["actionKinds"]=json!(["reply_and_close"]);}
    let body=parse(&request).unwrap();
    let run=app.change(|d| {
        d["connectorBinding"]=legacy_binding();
        if mode=="execute" {connection_gate::initialize_closed(d,&"a".repeat(64),1,20,false)?;connection_gate::fixture_open(d)?;}
        let grant=conductor_authority::create_grant(d,&actor,&body)?;
        let run=new_job(d,"conductor","fixed-request")?;
        let account=d["account"].clone();let binding=d["connectorBinding"].clone();
        let job=row_mut(d,"jobs",&run)?;
        job["account"]=account;job["connectorBinding"]=binding;
        job["conductor"]=json!({"version":1,"desiredState":"running","leaseGeneration":1,"mode":mode,
            "scope":body["scope"],"limits":body["limits"],"grant":grant,"childEverStarted":false});
        Ok(run)
    }).await.unwrap();(app,temp,run)
}

#[test]
fn campaign_manifest_is_distinct_from_batch_capacity_and_paths_are_closed() {
    let mut body=input();body["scope"]["itemIds"]=json!((0..1200).map(|n|format!("item-{n}")).collect::<Vec<_>>());
    assert_eq!(parse(&body).unwrap()["scope"]["itemIds"].as_array().unwrap().len(),1200);
    body["limits"]=json!({"batchSize":101});assert!(parse(&body).is_err());
    for key in ["baseUrl","path","cookie","capability"] {let mut body=input();body[key]=json!("arbitrary");assert!(parse(&body).is_err());}
    let mut body=input();body["scope"]["itemIds"]=json!(["item-1","item-1"]);assert!(parse(&body).is_err());
    body=input();body["scope"]["cutoffUtc"]=json!("2026-10-01T12:00:00+03:00");
    assert_eq!(parse(&body).unwrap()["scope"]["cutoffUtc"],"2026-10-01T09:00:00Z");
    body=input();body["workspaceGeneration"]=json!("11111111-1111-4111-8111-111111111111");
    assert_eq!(parse(&body).unwrap()["workspaceGeneration"],body["workspaceGeneration"]);
    for generation in [json!("not-a-generation"),json!("11111111-1111-1111-8111-111111111111"),json!(1)] {
        body["workspaceGeneration"]=generation;assert!(parse(&body).is_err());
    }
}

#[tokio::test]
async fn durable_seed_precedes_launch_and_missing_admitted_journal_fails_closed() {
    let (app,_temp,run)=fixture().await;
    let launch=prepare_child(&app,&run).await.unwrap();assert!(launch.resume);
    let job=app.db.read_job(&run).await.unwrap().unwrap();
    assert_eq!(job["conductor"]["childEverStarted"],true);
    let checkpoint=validate_checkpoint(&launch.checkpoint_path,&job,app.port).await.unwrap();
    assert_eq!(checkpoint["workflowId"],run);assert_eq!(checkpoint["workflowMode"],"prepare_review_only");
    assert_eq!(checkpoint["flowPolicy"]["maxRepairRounds"],0);assert!(checkpoint["workflowGeneration"].is_null());
    assert_eq!(launch.max_repair_rounds,0);
    assert_eq!(checkpoint["conductorRunId"],run);assert!(checkpoint["slices"].as_array().unwrap().is_empty());
    assert!(prepare_child(&app,&run).await.unwrap().resume);
    tokio::fs::remove_file(&launch.checkpoint_path).await.unwrap();
    assert!(prepare_child(&app,&run).await.is_err());fail_launch(&app,&run).await.unwrap();
    let job=app.db.read_job(&run).await.unwrap().unwrap();assert_eq!(job["status"],"recovery_required");assert_eq!(job["conductor"]["desiredState"],"paused");
}
#[tokio::test]
async fn prepare_seed_preserves_exact_working_generation_and_rejects_missing_workflow_identity() {
    let generation="11111111-1111-4111-8111-111111111111";
    let (app,_temp,run)=fixture_with_scope(operator_auth::Actor::local_owner("test"),"prepare",Some(generation)).await;
    let launch=prepare_child(&app,&run).await.unwrap();let job=app.db.read_job(&run).await.unwrap().unwrap();
    let checkpoint=validate_checkpoint(&launch.checkpoint_path,&job,app.port).await.unwrap();
    assert_eq!(launch.workspace_generation.as_deref(),Some(generation));assert_eq!(checkpoint["workflowGeneration"],generation);
    assert_eq!(checkpoint["workflowId"],run);assert_eq!(checkpoint["workflowMode"],"prepare_review_only");
    for field in ["workflowId","workflowGeneration","workflowMode"] {
        let mut corrupt=checkpoint.clone();corrupt.as_object_mut().unwrap().remove(field);
        tokio::fs::write(&launch.checkpoint_path,corrupt.to_string()).await.unwrap();
        assert!(validate_checkpoint(&launch.checkpoint_path,&job,app.port).await.is_err(),"{field}");
    }
    tokio::fs::write(&launch.checkpoint_path,checkpoint.to_string()).await.unwrap();
    assert_eq!(prepare_child(&app,&run).await.unwrap().workspace_generation.as_deref(),Some(generation));
}
#[tokio::test]
async fn execute_pause_recovery_cannot_advance_past_a_provider_capable_prior_permit() {
    let (app,_temp,run)=fixture_with_scope(operator_auth::Actor::local_owner("test"),"execute",None).await;
    app.change(|d|{
        let op=connection_gate::tests::operation(d,1);list_mut(d,"operations").push(op.clone());connection_gate::prearm(d,&op)?;
        let job=row_mut(d,"jobs",&run)?;job["conductor"]["desiredState"]=json!("pausing");job["status"]=json!("interrupted");Ok(())
    }).await.unwrap();
    assert!(recover(&app).await.is_err());let d=app.read().await.unwrap();let job=row(&d,"jobs",&run).unwrap();
    assert_eq!(job["conductor"]["desiredState"],"pausing");assert_eq!(job["conductor"]["leaseGeneration"],1);
    assert_eq!(d[connection_gate::FIELD]["state"],"closing");assert!(d[connection_gate::FIELD]["finalReceipt"].is_null());
    assert_eq!(d["operations"][0][connection_gate::PERMIT_FIELD]["phase"],"dispatch_armed");
}

#[tokio::test]
async fn interrupted_pause_finishes_without_spawning_or_authorizing_new_work() {
    let (app,_temp,run)=fixture().await;
    app.change_job(&run,|d|{let job=row_mut(d,"jobs",&run)?;job["conductor"]["desiredState"]=json!("pausing");job["status"]=json!("interrupted");Ok(())}).await.unwrap();
    recover(&app).await.unwrap();let job=app.db.read_job(&run).await.unwrap().unwrap();
    assert_eq!(job["status"],"paused");assert_eq!(job["conductor"]["leaseGeneration"],2);
    assert!(conductor_authority::authorize(&app,&run,2,"prepare",&[json!("item-1")]).await.is_err());
}

#[tokio::test]
async fn failed_authorizer_resume_preserves_pause_and_generation() {
    // This isolated database never had an Auth authority for this originally
    // captured operator. Do not mutate the immutable grant to simulate revoke.
    let actor=operator_auth::Actor{id:"revoked-operator".into(),name:"Test".into(),role:"operator".into(),
        csrf_token:String::new(),authority_generation:Some("a".repeat(64))};
    let (app,_temp,run)=fixture_with_actor(actor).await;
    app.change_job(&run,|d|{let job=row_mut(d,"jobs",&run)?;job["conductor"]["desiredState"]=json!("paused");job["status"]=json!("paused");
        Ok(())}).await.unwrap();
    assert!(resume(State(app.clone()),Extension(operator_auth::Actor::local_owner("test")),Path(run.clone())).await.is_err());
    let job=app.db.read_job(&run).await.unwrap().unwrap();assert_eq!(job["conductor"]["desiredState"],"paused");assert_eq!(job["conductor"]["leaseGeneration"],1);
}

#[tokio::test]
async fn progress_rejects_nested_credentials_and_stale_child_updates() {
    let (app,_temp,run)=fixture().await;let mut launch=prepare_child(&app,&run).await.unwrap();
    for report in [json!({"summary":{"cookie":"secret"}}),json!({"itemHolds":[{"itemId":"outside","reason":"hold"}]}),
        json!({"itemHolds":[{"itemId":"item-1","reason":"hold","capability":"secret"}]})] {assert!(record_child_report(&app,&launch,&report).await.is_err());}
    record_child_report(&app,&launch,&json!({"summary":{"held":1},"itemHolds":[{"itemId":"item-1","reason":"Private fact required"}]})).await.unwrap();
    launch.lease_generation=2;assert!(record_child_report(&app,&launch,&json!({"event":"test"})).await.is_err());
}

#[tokio::test]
async fn child_success_cannot_claim_unverified_recipients_and_restart_budget_is_durable() {
    let (app,_temp,run)=fixture().await;let launch=prepare_child(&app,&run).await.unwrap();
    assert!(restart_allowed(&app,&launch).await.unwrap());assert!(restart_allowed(&app,&launch).await.unwrap());assert!(!restart_allowed(&app,&launch).await.unwrap());
    let actual=canonical_result(&app,&launch,&json!({"mode":"complete","summary":{"verified":1},"itemHolds":[]})).await.unwrap();
    assert_eq!(actual["summary"]["verified"],0);assert_eq!(actual["summary"]["held"],1);assert_eq!(actual["mode"],"complete-with-holds");
}

#[tokio::test]
async fn child_settlement_observation_failure_blocks_without_ghost_or_effect() {
    let (app,_temp,run)=fixture().await;let launch=prepare_child(&app,&run).await.unwrap();
    // Isolated corrupt imported alias: the bounded reader detects a potentially
    // matching UNKNOWN target without a trustworthy connector identity.
    let mut data=app.read().await.unwrap();
    let item=row_mut(&mut data,"items","item-1").unwrap();
    item["objectId"]=json!("11391");item["itemId"]=json!("recipient-1");item["connectorBinding"]=legacy_binding();
    data["operations"].as_array_mut().unwrap().push(json!({"id":"ambiguous-unknown","itemId":"lost-alias","status":"unknown",
        "target":{"objectId":"11391","itemId":"recipient-1","connectorBinding":{}}}));
    let Database::Sqlite(pool)=&app.db else {panic!("isolated SQLite fixture required")};
    sqlx::query("UPDATE workspace SET payload=? WHERE id=1").bind(data.to_string()).execute(pool).await.unwrap();
    let view=app.db.read_bounded_review(&launch.scope_item_ids,app.account.display()).await.unwrap();
    assert_eq!(view["coverage"]["operationsComplete"],false);
    finish_child(&app,&launch,Ok(json!({"mode":"complete","summary":{"verified":1},"itemHolds":[]}))).await.unwrap();
    let after=app.read().await.unwrap();let job=row(&after,"jobs",&run).unwrap();
    assert_eq!(job["status"],"blocked");assert_eq!(job["conductor"]["desiredState"],"paused");
    assert_eq!(list(&after,"operations"),list(&data,"operations"));
    assert!(!restart_allowed(&app,&launch).await.unwrap());
    // A trustworthy historical local alias belongs to the same original
    // provider recipient and must count UNKNOWN, never a harmless hold.
    row_mut(&mut data,"operations","ambiguous-unknown").unwrap()["target"]["connectorBinding"]=legacy_binding();
    sqlx::query("UPDATE workspace SET payload=? WHERE id=1").bind(data.to_string()).execute(pool).await.unwrap();
    let actual=canonical_result(&app,&launch,&json!({"mode":"complete"})).await.unwrap();
    assert_eq!(actual["summary"]["unknown"],1);assert_eq!(actual["summary"]["held"],0);assert_eq!(actual["mode"],"blocked");
    assert_eq!(actual["itemHolds"][0]["itemId"],"item-1");assert_eq!(actual["itemHolds"][0]["stage"],"unknown");
    // Restore the already-settled lifecycle before advancing its lease.
    sqlx::query("UPDATE workspace SET payload=? WHERE id=1").bind(after.to_string()).execute(pool).await.unwrap();
    // A late disposition from an older child cannot block a newer epoch.
    app.change_job(&run,|d|{let job=row_mut(d,"jobs",&run)?;job["conductor"]["leaseGeneration"]=json!(2);
        job["conductor"]["desiredState"]=json!("running");job["status"]=json!("running");Ok(())}).await.unwrap();
    finish_child(&app,&launch,Ok(json!({"summary":{"secret":"invalid"}}))).await.unwrap();
    let job=app.db.read_job(&run).await.unwrap().unwrap();assert_eq!(job["status"],"running");assert_eq!(job["conductor"]["leaseGeneration"],2);
}

#[tokio::test]
async fn streamed_hold_pages_preserve_exact_recipient_reasons_in_canonical_result() {
    let (app,_temp,run)=fixture().await;let launch=prepare_child(&app,&run).await.unwrap();
    record_child_report(&app,&launch,&json!({"event":"holds-reset"})).await.unwrap();
    record_child_report(&app,&launch,&json!({"event":"holds-page","itemHolds":[{"itemId":"item-1","reason":"Private company fact required","stage":"preparation"}]})).await.unwrap();
    record_child_report(&app,&launch,&json!({"event":"holds-complete","summary":{"held":1}})).await.unwrap();
    finish_child(&app,&launch,Ok(json!({"mode":"complete-with-holds","summary":{"held":1}}))).await.unwrap();
    let job=app.db.read_job(&run).await.unwrap().unwrap();
    assert_eq!(job["status"],"completed");assert_eq!(job["result"]["itemHolds"][0]["reason"],"Private company fact required");
    assert_eq!(job["result"]["summary"]["verified"],0);
}
