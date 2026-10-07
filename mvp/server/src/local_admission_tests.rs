use super::*;

fn actor()->operator_auth::Actor {operator_auth::Actor::local_owner("synthetic")}
fn begin(d:&Value,kind:&str,body:&Value)->Request {request(d,kind,body,&actor()).unwrap().unwrap()}

#[test]
fn exact_payload_actor_and_account_bind_both_admission_kinds() {
    for kind in ["prepare","approval"] {
        let mut d=empty();let body=json!({"requestId":"same-key","items":["a"],"nested":{"x":1,"y":2}});
        let request=begin(&d,kind,&body);
        let mut result=if kind=="prepare"{json!({"jobId":"original-job"})}else{json!({"id":"original-approval"})};
        commit(&mut d,&request,&mut result).unwrap();
        let before=d.clone();
        let replayed=replay(&d,&begin(&d,kind,&body),&actor()).unwrap().unwrap();
        assert_eq!(replayed["replayed"],true);
        assert_eq!(replayed[if kind=="prepare"{"jobId"}else{"id"}],result[if kind=="prepare"{"jobId"}else{"id"}]);
        assert_eq!(d,before);
        let mut changed=body.clone();changed["nested"]["x"]=json!(3);
        assert_eq!(replay(&d,&begin(&d,kind,&changed),&actor()).unwrap_err().0,StatusCode::CONFLICT);
        let mut other=actor();other.id="other-operator".into();
        assert!(replay(&d,&request,&other).is_err());
        d["account"]=json!("BAW Russia");
        assert!(replay(&d,&begin(&d,kind,&body),&actor()).is_err());
        // The request key is namespaced by endpoint: no cross-kind substitution.
        assert!(find_receipt(&before,if kind=="prepare"{"approval"}else{"prepare"},"same-key").unwrap().is_none());
    }
}

#[test]
fn keys_are_bounded_and_legacy_has_no_receipt() {
    let d=empty();assert!(request(&d,"prepare",&json!({"itemIds":["a"]}),&actor()).unwrap().is_none());
    for key in [json!(null),json!(""),json!("bad/key"),json!("secret\n"),json!("x".repeat(161))] {
        assert!(request(&d,"prepare",&json!({"requestId":key}),&actor()).is_err());
    }
    assert!(validate("execute","request-key").is_ok());
    assert!(validate("invalid-kind","request-key").is_err());
}

#[test]
fn negative_history_is_append_only_exact_and_never_inferred_from_absence() {
    let mut d=empty();d["connectorBinding"]=active_binding(&d).unwrap().to_json();
    d[connection_gate::FIELD]=json!({"gateEpoch":1,"connectionBinding":d["connectorBinding"]});
    let body=json!({"requestId":"negative-key","approvalId":"approval-1"});let request=begin(&d,"execute",&body);
    assert!(find_rejection(&d,"execute","negative-key").unwrap().is_none());
    assert!(check_reevaluation(&d,&request,&actor(),&json!({"reevaluate":{"evaluationId":"absent","receiptSha256":"a".repeat(64)}})).is_err());
    let first=reject_execute(&mut d,&request,&actor(),"approval-1","dependency",vec![json!("blocking-reconcile")]).unwrap();
    let before=d.clone();assert_eq!(check_reevaluation(&d,&request,&actor(),&body).unwrap().unwrap()["evaluationId"],first["evaluationId"]);
    assert_eq!(d,before);
    let controlled=json!({"requestId":"negative-key","approvalId":"approval-1","reevaluate":{
        "evaluationId":first["evaluationId"],"receiptSha256":first["receiptSha256"]}});
    let same=begin(&d,"execute",&controlled);
    assert!(check_reevaluation(&d,&same,&actor(),&controlled).unwrap().is_none());
    let second=reject_execute(&mut d,&same,&actor(),"approval-1","dependency",vec![json!("blocking-reconcile")]).unwrap();
    assert_eq!(second["parentEvaluationId"],first["evaluationId"]);assert_eq!(second["parentReceiptSha256"],first["receiptSha256"]);
    assert_ne!(second["evaluationId"],first["evaluationId"]);
    assert!(check_reevaluation(&d,&same,&actor(),&controlled).is_err(),"stale negative cannot wake dependency");
    assert_eq!(list(&d,"audit")[0],list(&before,"audit")[0]);
    let mut changed=body.clone();changed["approvalId"]=json!("different-approval");
    assert!(check_reevaluation(&d,&begin(&d,"execute",&changed),&actor(),&changed).is_err());
    let mut missing=d.clone();list_mut(&mut missing,"audit").remove(0);
    assert!(find_rejection(&missing,"execute","negative-key").is_err());
}

#[test]
fn tampered_negative_proof_or_rotated_actor_cannot_authorize_reevaluation() {
    let mut d=empty();d[connection_gate::FIELD]=json!({"gateEpoch":1,"connectionBinding":active_binding(&d).unwrap().to_json()});
    let body=json!({"requestId":"bound-reject","approvalId":"approval-1"});let request=begin(&d,"execute",&body);
    let first=reject_execute(&mut d,&request,&actor(),"approval-1","precondition_changed",vec![]).unwrap();
    let mut other=actor();other.id="other-actor".into();
    let raw=find_rejection(&d,"execute","bound-reject").unwrap().unwrap();
    assert!(rejection_view(raw,"execute","bound-reject",accounts::Profile::from_workspace(&d).unwrap().key(),&other).is_err());
    let mut bad=raw.clone();bad["noAttemptProof"]["operationCreated"]=json!(true);bad["receiptSha256"]=json!(negative_digest(&bad).unwrap());
    assert!(rejection_view(&bad,"execute","bound-reject",accounts::Profile::from_workspace(&d).unwrap().key(),&actor()).is_err());
    assert_eq!(first["retryAuthorized"],false);
}

#[test]
fn deterministic_negative_ids_cannot_hide_corruption_and_parent_chain_is_exact() {
    let mut d=empty();d[connection_gate::FIELD]=json!({"gateEpoch":1,"connectionBinding":active_binding(&d).unwrap().to_json()});
    let body=json!({"requestId":"negative-chain","approvalId":"approval-1"});let request=begin(&d,"execute",&body);
    let first=reject_execute(&mut d,&request,&actor(),"approval-1","dependency",vec![]).unwrap();
    let second=reject_execute(&mut d,&request,&actor(),"approval-1","dependency",vec![]).unwrap();
    assert_eq!(find_rejection(&d,"execute","negative-chain").unwrap().unwrap()["evaluationId"],second["evaluationId"]);
    for field in ["action","kind","requestId","refId"] {
        let mut corrupt=d.clone();corrupt["audit"][0][field]=json!("unrelated");
        corrupt["audit"][0]["receiptSha256"]=json!(negative_digest(&corrupt["audit"][0]).unwrap());
        assert!(find_rejection(&corrupt,"execute","negative-chain").is_err(),"deterministic identity must retain corrupt {field}");
    }
    for (row,field,value) in [(0,"parentEvaluationId",json!("fake-parent")),
        (0,"parentReceiptSha256",first["receiptSha256"].clone()),
        (1,"parentEvaluationId",json!("different-parent")),(1,"parentReceiptSha256",json!("b".repeat(64))),
        (1,"evaluationId",first["evaluationId"].clone()),(1,"approvalId",json!("other-approval"))] {
        let mut corrupt=d.clone();corrupt["audit"][row][field]=value;
        corrupt["audit"][row]["receiptSha256"]=json!(negative_digest(&corrupt["audit"][row]).unwrap());
        assert!(find_rejection(&corrupt,"execute","negative-chain").is_err(),"rehashed {field} must not break exact immutable chain");
    }
    let mut removed=d.clone();removed["audit"][0].as_object_mut().unwrap().remove("parentEvaluationId");
    removed["audit"][0]["receiptSha256"]=json!(negative_digest(&removed["audit"][0]).unwrap());
    assert!(find_rejection(&removed,"execute","negative-chain").is_err());
}

#[tokio::test]
async fn conductor_receipt_keeps_origin_and_replays_only_within_same_run_prior_epoch(){
    let mut d=empty();let actor=actor();let body=json!({"requestId":"epoch-receipt"});
    let ctx=conductor_authority::Context{run_id:"run-a".into(),lease_generation:1,actor:actor.clone()};
    conductor_authority::with_context(ctx.clone(),async {
        let r=request(&d,"prepare",&body,&actor).unwrap().unwrap();
        commit(&mut d,&r,&mut json!({"jobId":"original-job"})).unwrap();
    }).await;
    let original=d.clone();let receipt=find_receipt(&d,"prepare","epoch-receipt").unwrap().unwrap();
    assert_eq!(receipt["conductorRunId"],"run-a");assert_eq!(receipt["grantGeneration"],1);
    let mut resumed=ctx.clone();resumed.lease_generation=2;
    conductor_authority::with_context(resumed,async {
        let r=request(&d,"prepare",&body,&actor).unwrap().unwrap();
        assert_eq!(replay(&d,&r,&actor).unwrap().unwrap()["jobId"],"original-job");
        let mut untagged=d.clone();for key in ["conductorRunId","grantGeneration"]{untagged["audit"][0].as_object_mut().unwrap().remove(key);}
        assert!(replay(&untagged,&r,&actor).is_err());
    }).await;
    let mut other=ctx.clone();other.run_id="run-b".into();
    conductor_authority::with_context(other,async {
        assert!(replay(&d,&request(&d,"prepare",&body,&actor).unwrap().unwrap(),&actor).is_err());
    }).await;
    let mut older=ctx;older.lease_generation=0;
    conductor_authority::with_context(older,async {
        assert!(replay(&d,&request(&d,"prepare",&body,&actor).unwrap().unwrap(),&actor).is_err());
    }).await;
    assert_eq!(d,original);
}

#[tokio::test]
async fn durable_approval_replay_and_lookup_survive_restart_without_new_approval() {
    let (app,_temp)=crate::tests::test_app().await;
    let p=app.change(|d|create_proposal(d,&json!({"itemId":"item-1","kind":"close","expectedRevision":1}))).await.unwrap();
    let body=json!({"requestId":"approval-loss","proposals":[{"id":p["id"],"revision":p["revision"]}]});
    let first=crate::approval_new(State(app.clone()),Extension(actor()),Json(body.clone())).await.unwrap().0;
    // Simulate process startup on the durable workspace, including unchanged audit.
    app.change(|d|{crate::recover(d).unwrap();Ok(())}).await.unwrap();
    let second=crate::approval_new(State(app.clone()),Extension(actor()),Json(body)).await.unwrap().0;
    assert_eq!(first["id"],second["id"]);assert_eq!(second["replayed"],true);
    let d=app.db.read().await.unwrap();assert_eq!(list(&d,"approvals").len(),1);
    assert!(first.get("proposals").is_none());
    let receipt=find_receipt(&d,"approval","approval-loss").unwrap().unwrap();
    assert!(receipt["result"].to_string().len()<300);assert!(receipt.to_string().len()<1000);
    let looked=lookup(State(app.clone()),Extension(actor()),Path(("approval".into(),"approval-loss".into()))).await.unwrap().0;
    assert_eq!(looked["status"],"committed");assert_eq!(looked["result"]["id"],first["id"]);
    let missing=lookup(State(app.clone()),Extension(actor()),Path(("prepare".into(),"missing".into()))).await.unwrap().0;
    assert_eq!(missing["status"],"pending_or_unknown");assert_eq!(missing["retryAuthorized"],false);
    let mut foreign=actor();foreign.id="foreign".into();
    assert!(lookup(State(app.clone()),Extension(foreign),Path(("approval".into(),"approval-loss".into()))).await.is_err());
    app.db.close().await;
}

#[tokio::test]
async fn legacy_approval_shape_is_preserved_and_keyed_receipt_never_copies_large_context() {
    let (app,_temp)=crate::tests::test_app().await;
    let p=app.change(|d|{
        d["items"][0]["text"]=json!("Large comment context. ".repeat(1000));
        create_proposal(d,&json!({"itemId":"item-1","kind":"close","expectedRevision":1}))
    }).await.unwrap();
    let body=json!({"proposals":[{"id":p["id"],"revision":p["revision"]}]});
    let legacy=crate::approval_new(State(app.clone()),Extension(actor()),Json(body.clone())).await.unwrap().0;
    assert!(legacy["proposals"][0]["item"]["text"].as_str().unwrap().len()>20_000);
    let mut keyed=body;keyed["requestId"]=json!("large-approval");
    let result=crate::approval_new(State(app.clone()),Extension(actor()),Json(keyed)).await.unwrap().0;
    assert!(result.get("proposals").is_none());
    let d=app.db.read().await.unwrap();let receipt=find_receipt(&d,"approval","large-approval").unwrap().unwrap();
    assert!(receipt.to_string().len()<1000);
    assert_eq!(list(&d,"approvals").len(),2);app.db.close().await;
}

#[tokio::test]
async fn prepare_replay_after_restart_does_not_spawn_and_keeps_interrupted_job() {
    let (app,_temp)=crate::tests::test_app_with_post().await;
    let body=json!({"requestId":"prepare-loss","itemIds":["item-1"]});
    let lifecycle=app.lifecycle_admission_token(crate::runtime_lifecycle::AdmissionClass::Preparation).await.unwrap();
    // Commit the exact admission without calling spawn: model commit/ack crash.
    let original=app.change_preparation_schedule(|d| {
        let request=begin(d,"prepare",&body);
        let scheduled=crate::engine_prepare::schedule(d,crate::engine_prepare::parse(&body)?)?;
        crate::preparation_review::record_initial_admission(d,&lifecycle,&scheduled.job_id,&crate::now())?;
        let mut result=json!({"jobId":scheduled.job_id});
        commit(d,&request,&mut result)?;Ok(result)
    }).await.unwrap();
    app.change(|d|{crate::recover(d).unwrap();Ok(())}).await.unwrap();
    let replayed=crate::engine_prepare::prepare(State(app.clone()),Extension(actor()),Json(body)).await.unwrap().0;
    assert_eq!(original["jobId"],replayed["jobId"]);assert_eq!(replayed["replayed"],true);
    let d=app.db.read().await.unwrap();assert_eq!(list(&d,"jobs").len(),1);
    assert_eq!(d["jobs"][0]["status"],"interrupted");assert!(app.tasks.lock().await.is_empty());
    app.db.close().await;
}

#[tokio::test]
async fn transaction_failure_leaves_no_result_or_receipt() {
    let (app,_temp)=crate::tests::test_app().await;
    let before=app.db.read().await.unwrap();
    let result:ApiResult<()>=app.change(|d|{
        let request=begin(d,"approval",&json!({"requestId":"aborted"}));
        list_mut(d,"approvals").push(json!({"id":"transient","status":"approved"}));
        commit(d,&request,&mut json!({"id":"transient"}))?;
        Err(conflict("synthetic rollback"))
    }).await;
    assert!(result.is_err());assert_eq!(app.db.read().await.unwrap(),before);app.db.close().await;
}

#[tokio::test]
async fn concurrent_approval_posts_share_one_durable_result() {
    let (app,_temp)=crate::tests::test_app().await;
    let p=app.change(|d|create_proposal(d,&json!({"itemId":"item-1","kind":"close","expectedRevision":1}))).await.unwrap();
    let body=json!({"requestId":"concurrent-approval","proposals":[{"id":p["id"],"revision":p["revision"]}]});
    let (a,b)=tokio::join!(
        crate::approval_new(State(app.clone()),Extension(actor()),Json(body.clone())),
        crate::approval_new(State(app.clone()),Extension(actor()),Json(body)));
    assert_eq!(a.unwrap().0["id"],b.unwrap().0["id"]);
    let d=app.db.read().await.unwrap();assert_eq!(list(&d,"approvals").len(),1);
    assert_eq!(list(&d,"audit").iter().filter(|r|r["action"]==ACTION).count(),1);
    app.db.close().await;
}
