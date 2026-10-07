use super::*;
use crate::dispatch_authority::tests::Harness;

#[test]
fn immutable_workspace_generation_prevents_new_database_repinning() {
    let mut d=crate::connection_gate::tests::workspace();
    let item=crate::connection_gate::tests::operation(&d,1)["target"].clone();list_mut(&mut d,"items").push(item);
    let actor=Actor::local_owner("synthetic");
    let input=json!({"mode":"prepare","scope":{"itemIds":["local-1"]},"actionKinds":[]});
    let legacy=create_grant(&d,&actor,&input).unwrap();assert!(legacy["workspaceGeneration"].is_null());
    let mut job=json!({"id":"immutable-run","kind":"conductor","conductor":{"version":1,"leaseGeneration":1,"grant":legacy}});
    check_workspace_generation(&d,&job).unwrap();
    d["storageGeneration"]=json!("11111111-1111-4111-8111-111111111111");
    assert!(check_workspace_generation(&d,&job).is_err(),"legacy null must not become a new generation automatically");
    let grant=create_grant(&d,&actor,&input).unwrap();assert_eq!(grant["workspaceGeneration"],d["storageGeneration"]);
    job["conductor"]["grant"]=grant;check_workspace_generation(&d,&job).unwrap();
    let mut supplied=input.clone();supplied["workspaceGeneration"]=json!("22222222-2222-4222-8222-222222222222");
    assert!(create_grant(&d,&actor,&supplied).is_err());
    supplied["workspaceGeneration"]=Value::Null;assert!(create_grant(&d,&actor,&supplied).is_err());
    d["storageGeneration"]=json!("22222222-2222-4222-8222-222222222222");
    assert!(check_workspace_generation(&d,&job).is_err(),"resume/new lease keeps original storage pin");
    let before=json!({"jobs":[job]});let mut changed=before.clone();
    changed["jobs"][0]["conductor"]["grant"]["workspaceGeneration"]=d["storageGeneration"].clone();
    assert!(validate_change(&before,&changed).is_err());
}

async fn grant(h: &Harness, scope: &[&str]) -> Context {
    let run=h.app.change(|d| {
        let input=json!({"mode":"execute","scope":{"itemIds":scope},"actionKinds":["close","reply_and_close"]});
        let authorization=create_grant(d,&h.actor,&input)?;
        let run=new_job(d,"conductor","fixture")?;
        let account=d["account"].clone();let binding=active_binding(d)?.to_json();
        let job=row_mut(d,"jobs",&run)?;
        job["account"]=account;job["connectorBinding"]=binding;
        job["conductor"]=json!({"version":1,"desiredState":"running","leaseGeneration":1,
            "mode":"execute","scope":input["scope"],"grant":authorization});
        Ok(run)
    }).await.unwrap();
    Context{run_id:run,lease_generation:1,actor:h.actor.clone()}
}

#[tokio::test]
async fn transaction_fences_company_scope_actor_and_generation_without_partial_admission() {
    let h=Harness::new("").await;let ctx=grant(&h,&["item-1"]).await;
    let original=h.app.read().await.unwrap();
    with_context(ctx.clone(),async {
        h.app.change(|d| {
            assert!(fence_admission(d,"approval",&[json!("item-1")]).is_ok());
            assert!(fence_admission(d,"approval",&[json!("item-2")]).is_err());
            assert!(fence_admission(d,"delete",&[json!("item-1")]).is_err());
            assert!(fence_actor(Some(&ctx),&Actor::local_owner("csrf")).is_err());
            d["account"]=json!("BAW Russia");
            fence_admission(d,"approval",&[json!("item-1")])?;
            Ok(())
        }).await.unwrap_err();
    }).await;
    assert_eq!(original,h.app.read().await.unwrap(),"Failed closure commits no cross-company state");
    h.app.change_job(&ctx.run_id,|d|{row_mut(d,"jobs",&ctx.run_id)?["conductor"]["leaseGeneration"]=json!(2);Ok(())}).await.unwrap();
    with_context(ctx.clone(),async {
        assert!(h.app.change(|d|fence_admission(d,"approval",&[json!("item-1")])).await.is_err());
    }).await;
}

#[tokio::test]
async fn paused_after_approval_aborts_execute_transaction_and_preserves_approval() {
    let h=Harness::new("").await;let ctx=grant(&h,&["item-1","item-2"]).await;
    let approval=with_context(ctx.clone(),h.approval()).await;
    h.app.change_job(&ctx.run_id,|d|{row_mut(d,"jobs",&ctx.run_id)?["conductor"]["desiredState"]=json!("paused");Ok(())}).await.unwrap();
    let before=h.app.read().await.unwrap();
    with_context(ctx.clone(),async {
        assert!(h.app.change_admission(storage::AdmissionScope::Execute{approval:&approval,body:&json!({})},
            |d|execute_admission::admit(d,&h.actor,&approval,&json!({}))).await.is_err());
    }).await;
    assert_eq!(before,h.app.read().await.unwrap());assert!(h.calls().is_empty());
}

#[tokio::test]
async fn paused_or_rotated_after_enqueue_fences_all_queued_operations() {
    for rotate in [false,true] {
        let h=Harness::new("").await;let ctx=grant(&h,&["item-1","item-2"]).await;
        let approval=with_context(ctx.clone(),h.approval()).await;
        let waiting=h.app.execution_gate.lock().await;
        with_context(ctx.clone(),h.enqueue(approval)).await;
        h.app.change_job(&ctx.run_id,|d| {
            let c=&mut row_mut(d,"jobs",&ctx.run_id)?["conductor"];
            if rotate{c["leaseGeneration"]=json!(2);}else{c["desiredState"]=json!("paused");}Ok(())
        }).await.unwrap();
        drop(waiting);let data=h.finished().await;
        assert!(h.calls().is_empty());
        assert!(list(&data,"operations").iter().all(|op|op["status"]=="stale"
            &&op["conductorRunId"]==ctx.run_id&&op["grantGeneration"]==1));
    }
}

#[tokio::test]
async fn replay_recovers_original_job_without_rescheduling_or_retargeting_grant() {
    let h=Harness::new("").await;let ctx=grant(&h,&["item-1","item-2"]).await;
    let approval=with_context(ctx.clone(),h.approval()).await;
    let body=json!({"requestId":"conductor-replay-test","approvalId":approval});
    with_context(ctx.clone(),async {
        let first=h.app.change_admission(storage::AdmissionScope::Execute{approval:&approval,body:&body},|d| {
            let first=execute_admission::admit(d,&h.actor,&approval,&body)?;
            assert!(first.1.is_some());
            Ok(first.0)
        }).await.unwrap();
        h.app.change_job(&ctx.run_id,|d|{row_mut(d,"jobs",&ctx.run_id)?["conductor"]["desiredState"]=json!("paused");Ok(())}).await.unwrap();
        h.app.change_admission(storage::AdmissionScope::Execute{approval:&approval,body:&body},|d| {
            let replay=execute_admission::admit(d,&h.actor,&approval,&body)?;
            assert_eq!(first["jobId"],replay.0["jobId"]);
            assert_eq!(first["approvalId"],replay.0["approvalId"]);
            assert_eq!(replay.0["replayed"],true);assert!(replay.1.is_none());
            assert_eq!(list(d,"operations").len(),2);
            assert!(list(d,"operations").iter().all(|op|op["conductorRunId"]==ctx.run_id&&op["grantGeneration"]==1));
            Ok(())
        }).await.unwrap();
    }).await;
    assert!(h.calls().is_empty());
}

#[tokio::test]
async fn missing_grant_never_hydrates_owner_and_current_actor_rotation_revokes_campaign() {
    let h=Harness::new("").await;let ctx=grant(&h,&["item-1","item-2"]).await;
    check_read(&h.app,&ctx).await.unwrap();
    use sha2::{Digest,Sha256};
    std::fs::write(&h.access,json!({"operators":[{"id":"alice","name":"Alice",
        "tokenHash":format!("{:x}",Sha256::digest("new-credential-generation".as_bytes()))}]}).to_string()).unwrap();
    assert!(check_read(&h.app,&ctx).await.is_err());
    let mut job=h.app.db.read_job(&ctx.run_id).await.unwrap().unwrap();
    job["conductor"]["grant"]=Value::Null;
    assert!(actor_from_grant(&job).is_err());
    let mut foreign=h.app.clone();foreign.account=accounts::Profile::BawRussia;
    assert!(authorize(&foreign,&ctx.run_id,1,"read",&[]).await.is_err());
}

#[tokio::test]
async fn dispatch_barrier_retains_parallel_sends_and_excludes_transition() {
    let h=Harness::new("").await;let ctx=grant(&h,&["item-1"]).await;
    let first=dispatch_guard(&h.app,&ctx.run_id).await;
    let second=tokio::time::timeout(Duration::from_millis(100),dispatch_guard(&h.app,&ctx.run_id)).await.unwrap();
    let another=dispatch_guard(&h.app,"independent-run").await;
    let app=h.app.clone();let run=ctx.run_id.clone();
    let transition=tokio::spawn(async move{transition_guard(&app,&run).await});
    tokio::task::yield_now().await;assert!(!transition.is_finished());
    drop(first);tokio::task::yield_now().await;assert!(!transition.is_finished());
    drop(second);let exclusive=tokio::time::timeout(Duration::from_secs(1),transition).await.unwrap().unwrap();
    assert!(tokio::time::timeout(Duration::from_millis(20),dispatch_guard(&h.app,&ctx.run_id)).await.is_err());
    drop(exclusive);drop(another);
}

#[tokio::test]
async fn transition_releases_company_mutex_while_waiting_for_previous_network_guard() {
    let h=Harness::new("").await;let ctx=grant(&h,&["item-1"]).await;
    let read=dispatch_guard(&h.app,&ctx.run_id).await;
    let app=h.app.clone();let run=ctx.run_id.clone();
    let transition=tokio::spawn(async move{transition_guard(&app,&run).await});
    tokio::task::yield_now().await;
    let company=tokio::time::timeout(Duration::from_millis(100),connection_gate::lock(&h.app)).await.unwrap();
    drop(company);assert!(!transition.is_finished());
    drop(read);let guard=tokio::time::timeout(Duration::from_secs(1),transition).await.unwrap().unwrap();drop(guard);
}

#[tokio::test]
async fn pause_during_sent_action_preserves_settlement_and_readback_but_stops_next_conversation_reply() {
    let h=Harness::new("gate_execute").await;let ctx=grant(&h,&["item-1","item-2"]).await;
    let approval=with_context(ctx.clone(),async {
        let refs=h.app.change(|d| {
            d["items"][1]["conversationKey"]=d["items"][0]["conversationKey"].clone();
            for n in 1..=2 {crate::tests::create_post_fixture(d,&format!("item-{n}"))?;}
            let mut refs=vec![];
            for n in 1..=2 {
                let p=create_proposal(d,&json!({"itemId":format!("item-{n}"),"kind":"reply_and_close",
                    "expectedRevision":1,"text":format!("Reviewed response {n}")}))?;
                editorial_review::fixture_accept(d,p["id"].as_str().unwrap()).map_err(bad)?;
                refs.push(json!({"id":p["id"],"revision":p["revision"]}));
            }
            Ok(json!(refs))
        }).await.unwrap();
        approval_new(State(h.app.clone()),axum::Extension(h.actor.clone()),Json(json!({"proposals":refs})))
            .await.unwrap().0["id"].as_str().unwrap().to_owned()
    }).await;
    with_context(ctx.clone(),h.enqueue(approval)).await;
    tokio::time::timeout(Duration::from_secs(5),async {
        while !h.calls().iter().any(|call|call["operation"]=="execute") {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }).await.unwrap();
    h.app.change_job(&ctx.run_id,|d|{row_mut(d,"jobs",&ctx.run_id)?["conductor"]["desiredState"]=json!("pausing");Ok(())}).await.unwrap();
    h.release_execute();
    let data=h.finished().await;
    assert_eq!(h.calls().iter().map(|v|v["operation"].as_str().unwrap()).collect::<Vec<_>>(),["context","execute","readback"]);
    assert_eq!(data["operations"][0]["status"],"succeeded");
    assert_eq!(data["operations"][1]["status"],"stale");
    assert_eq!(data["items"][0]["providerStatus"],"closed");
    assert_eq!(data["items"][1]["providerStatus"],"new");
}

#[tokio::test]
async fn restart_reuses_immutable_unexecuted_approval_with_current_grant_without_retargeting() {
    let h=Harness::new("").await;let prior=grant(&h,&["item-1","item-2"]).await;
    let approval=with_context(prior.clone(),h.approval()).await;
    h.app.change_job(&prior.run_id,|d|{row_mut(d,"jobs",&prior.run_id)?["conductor"]["leaseGeneration"]=json!(3);Ok(())}).await.unwrap();
    let ctx=Context{lease_generation:3,..prior.clone()};
    with_context(ctx.clone(),async {
        let source=h.app.read().await.unwrap();
        for mode in ["foreign-run","future-epoch","changed-authority"] {
            let mut trial=source.clone();
            let p=row_mut(&mut trial,"approvals",&approval).unwrap();
            match mode {
                "foreign-run"=>p["conductorRunId"]=json!("another-run"),
                "future-epoch"=>p["grantGeneration"]=json!(4),
                _=>p["approvalAuthority"]["generation"]=json!("f".repeat(64)),
            }
            let before=trial.clone();
            assert!(execute_admission::admit(&mut trial,&h.actor,&approval,&json!({})).is_err(),"{mode}");
            assert_eq!(trial,before,"{mode}");
        }
        h.app.change_admission(storage::AdmissionScope::Execute{approval:&approval,body:&json!({})},|d| {
            execute_admission::admit(d,&h.actor,&approval,&json!({}))?;
            assert_eq!(row(d,"approvals",&approval)?["grantGeneration"],1);
            assert!(list(d,"operations").iter().all(|op|op["conductorRunId"]==ctx.run_id&&op["grantGeneration"]==3));
            Ok(())
        }).await.unwrap();
    }).await;
    assert!(h.calls().is_empty());
}

#[tokio::test]
async fn epoch_transition_cannot_regrant_actor_company_scope_connector_or_action_budget() {
    let h=Harness::new("").await;let ctx=grant(&h,&["item-1"]).await;
    let original=h.app.read().await.unwrap();
    for field in ["actor","scope","connector","company","mode","actions","epoch"] {
        let mut changed=original.clone();let job=row_mut(&mut changed,"jobs",&ctx.run_id).unwrap();
        match field {
            "actor"=>job["conductor"]["grant"]["actor"]["id"]=json!("another-owner"),
            "scope"=>job["conductor"]["scope"]["itemIds"]=json!(["item-2"]),
            "connector"=>job["connectorBinding"]=accounts::Profile::BawRussia.binding(),
            "company"=>job["account"]=json!("BAW Russia"),
            "mode"=>job["conductor"]["mode"]=json!("prepare"),
            "actions"=>job["conductor"]["grant"]["actionKinds"]=json!(["delete"]),
            _=>job["conductor"]["leaseGeneration"]=json!(0),
        }
        assert!(validate_change(&original,&changed).is_err(),"{field}");
    }
    let mut paused=original.clone();let job=row_mut(&mut paused,"jobs",&ctx.run_id).unwrap();
    job["conductor"]["desiredState"]=json!("paused");job["conductor"]["leaseGeneration"]=json!(2);
    assert!(validate_change(&original,&paused).is_ok());
}

#[test]
fn public_job_sanitizer_removes_generation_without_manufacturing_campaign_fields() {
    let mut ordinary=json!({"id":"manual-job","kind":"assistant"});let before=ordinary.clone();
    sanitize_job(&mut ordinary);assert_eq!(ordinary,before);
    let mut job=json!({"conductor":{"grant":{"actor":{"id":"alice"},"authorityGeneration":{"generation":"private"}}}});
    sanitize_job(&mut job);
    assert!(job["conductor"]["grant"].get("authorityGeneration").is_none());
    assert_eq!(job["conductor"]["grant"]["actor"]["id"],"alice");
}

#[tokio::test]
async fn original_old_epoch_readback_claim_is_atomic_paused_denied_and_operation_never_retagged() {
    let h=Harness::new("").await;let prior=grant(&h,&["item-1"]).await;
    let at=chrono::Utc::now().timestamp();
    let mut op=readback_recovery::tests::operation(at-60);
    let authority=dispatch_authority::approval_binding(&h.actor);
    op["dispatchAuthority"]=json!({"approved":authority,"executed":authority});
    op["approvedBy"]=h.actor.public_json();op["executedBy"]=h.actor.public_json();tag(&prior,&mut op);
    h.app.change(|d|{d["operations"]=json!([op]);
        let campaign=&mut row_mut(d,"jobs",&prior.run_id)?["conductor"];
        campaign["desiredState"]=json!("paused");campaign["leaseGeneration"]=json!(3);Ok(())}).await.unwrap();
    let ctx=Context{lease_generation:3,..prior.clone()};
    with_context(ctx.clone(),async {
        let before=h.app.read().await.unwrap();
        assert!(h.app.db.claim_readback_recovery("op",at,Some(&h.actor.public_json())).await.is_err());
        assert_eq!(h.app.read().await.unwrap(),before,"Paused grant admits no readback job");
        h.app.change_job(&ctx.run_id,|d|{row_mut(d,"jobs",&ctx.run_id)?["conductor"]["desiredState"]=json!("running");Ok(())}).await.unwrap();
        let (job,saved)=h.app.db.claim_readback_recovery("op",at,Some(&h.actor.public_json())).await.unwrap().unwrap();
        assert_eq!(saved,op);
        let data=h.app.read().await.unwrap();let job=row(&data,"jobs",&job).unwrap();
        assert_eq!(job["readbackOnly"],true);assert_eq!(job["conductorRunId"],ctx.run_id);assert_eq!(job["grantGeneration"],3);
        assert_eq!(data["operations"][0],op);assert_eq!(op["grantGeneration"],1);
    }).await;
    assert!(h.calls().is_empty(),"Claim itself never calls execute/readback");
}

#[tokio::test]
async fn readback_claim_rejects_foreign_or_future_operations_and_other_authorizers() {
    let h=Harness::new("").await;let ctx=grant(&h,&["item-1"]).await;
    let mut op=readback_recovery::tests::operation(chrono::Utc::now().timestamp()-60);
    let authority=dispatch_authority::approval_binding(&h.actor);
    op["dispatchAuthority"]=json!({"approved":authority,"executed":authority});tag(&ctx,&mut op);
    let d=h.app.read().await.unwrap();
    with_context(ctx.clone(),async {
        for mode in ["foreign","future","authorizer","target"] {
            let mut operation=op.clone();let mut requester=h.actor.public_json();
            match mode {
                "foreign"=>operation["conductorRunId"]=json!("another-run"),
                "future"=>operation["grantGeneration"]=json!(2),
                "authorizer"=>requester["id"]=json!("another-actor"),
                _=>operation["target"]["id"]=json!("item-2"),
            }
            let mut job=json!({"kind":"reconcile","refId":"op","readbackOnly":true});let before=job.clone();
            assert!(fence_readback_claim(&d,&operation,Some(&requester),&mut job).is_err(),"{mode}");
            assert_eq!(job,before,"{mode}");
        }
    }).await;
}
