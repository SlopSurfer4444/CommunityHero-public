use super::*;

#[test]
fn capability_is_exact_and_request_fields_are_closed() {
    let hash: [u8;32]=Sha256::digest(b"ephemeral-capability").into();
    assert!(capability_matches(&hash,"ephemeral-capability"));
    assert!(!capability_matches(&hash,"ephemeral-capabilitY"));
    assert!(!capability_matches(&hash,""));
    assert!(fields(&json!({"itemIds":["one"],"baseUrl":"https://foreign"}),&["itemIds"],&["itemIds"]).is_err());
    assert!(fields(&json!({"itemIds":["one"]}),&["itemIds"],&["itemIds"]).is_ok());
    assert!(fields(&json!({"approvalId":"one","requestId":"same","reevaluate":{}}),&["approvalId","requestId"],&["approvalId","requestId"]).is_err(),"private execute and dependency read remain closed");
}

#[test]
fn selection_ids_are_exact_bounded_and_not_query_injection() {
    assert_eq!(ids(&json!({"itemIds":["one","two"]})).unwrap(),vec!["one","two"]);
    for value in [json!({"itemIds":[]}),json!({"itemIds":["one","one"]}),
        json!({"itemIds":["one,two"]}),json!({"itemIds":["one\n"]}),json!({"itemIds":[1]})] {
        assert!(ids(&value).is_err());
    }
}

#[test]
fn family_metadata_bound_does_not_expand_preparation_bound() {
    let selected:Vec<_>=(0..5000).map(|n|format!("item-{n}")).collect();
    let input=json!({"itemIds":selected});
    assert!(ids(&input).is_err());
    assert_eq!(ids_limit(&input,5000).unwrap().len(),5000);
    let mut overflow=input.clone();overflow["itemIds"].as_array_mut().unwrap().push(json!("overflow"));
    assert!(ids_limit(&overflow,5000).is_err());
    let mut duplicate=input;duplicate["itemIds"][4999]=json!("item-0");
    assert!(ids_limit(&duplicate,5000).is_err());
}

async fn fixture()->(RpcState,tempfile::TempDir) {
    let (app,temp)=crate::tests::test_app().await;
    let actor=operator_auth::Actor::local_owner("captured-test-authorizer");
    let input=json!({"scope":{"itemIds":["item-1"]},"mode":"prepare","actionKinds":[]});
    app.change(|d|{
        d["connectorBinding"]=legacy_binding();
        list_mut(d,"items")[0]["postId"]=json!("selected-post");
        let grant=conductor_authority::create_grant(d,&actor,&input)?;
        let job=json!({"id":"run-one","kind":"conductor","status":"running",
            "account":d["account"],"connectorBinding":d["connectorBinding"],
            "conductor":{"version":1,"desiredState":"running","leaseGeneration":1,
                "mode":"prepare","scope":input["scope"],"grant":grant}});
        list_mut(d,"jobs").push(job);
        let mut outside=list(d,"items")[0].clone(); outside["id"]=json!("outside");outside["itemId"]=json!("outside-provider");outside["postId"]=json!("outside-post");
        list_mut(d,"items").push(outside);
        list_mut(d,"proposals").push(json!({"id":"inside-proposal","itemId":"item-1","revision":1}));
        list_mut(d,"proposals").push(json!({"id":"outside-proposal","itemId":"outside","revision":1}));
        Ok(())
    }).await.unwrap();
    let launch=Arc::new(conductor::Launch{run_id:"run-one".into(),lease_generation:1,actor:actor.clone(),
        account:app.account.key().into(),base_url:"http://127.0.0.1:4186".into(),
        checkpoint_path:temp.path().join("owned/checkpoint.json"),scope_item_ids:vec!["item-1".into()],
        mode:"prepare".into(),max_repair_rounds:2,max_cycles:2,batch_size:1,cutoff_utc:None,resume:false,workspace_generation:None,
        connection_binding:legacy_binding(),continuation_claim:Value::Null});
    let state=RpcState{app,launch,context:conductor_authority::Context{run_id:"run-one".into(),lease_generation:1,actor},
        capability_hash:Sha256::digest(b"test-only-capability").into(),observed_jobs:Default::default(),
        proposal_hints:Default::default(),active:Arc::new(std::sync::atomic::AtomicBool::new(true))};
    (state,temp)
}
fn headers()->HeaderMap {
    let mut headers=HeaderMap::new();
    for (name,value) in [("x-conductor-capability","test-only-capability"),("x-conductor-run","run-one"),("x-conductor-generation","1")] {
        headers.insert(name,value.parse().unwrap());
    }
    headers
}
#[tokio::test]
async fn prepare_status_has_explicit_null_connection_dependency_and_read_rpc_has_no_context_override() {
    let (state,_temp)=fixture().await;let before=state.app.read().await.unwrap();
    let status=dispatch(&state,"engineStatus",json!({})).await.unwrap();
    assert!(status.as_object().unwrap().contains_key("connectionDependency"));assert!(status["connectionDependency"].is_null());
    assert!(dispatch(&state,"connectionDependency",json!({"approvalId":"inside-proposal","requestId":"same","leaseGeneration":1})).await.is_err());
    assert_eq!(state.app.read().await.unwrap(),before);
}
#[tokio::test]
async fn rpc_rejects_misbound_capabilities_generations_pauses_and_scope() {
    let (state,_temp)=fixture().await;
    let health=json!({"operation":"health","args":{}});
    assert_eq!(rpc(State(state.clone()),headers(),Json(health.clone())).await.0["status"],200);
    for (name,value) in [("x-conductor-capability","wrong"),("x-conductor-run","another-run"),("x-conductor-generation","2")] {
        let mut wrong=headers();wrong.insert(name,value.parse().unwrap());
        assert_eq!(rpc(State(state.clone()),wrong,Json(health.clone())).await.0["status"],403);
    }
    assert_eq!(rpc(State(state.clone()),headers(),Json(json!({"operation":"reviewItems","args":{"itemIds":["outside"]}}))).await.0["status"],403);
    assert_eq!(rpc(State(state.clone()),headers(),Json(json!({"operation":"selectPrepareFamilies","args":{"itemIds":["outside"],"batchSize":60,"maxBatches":1}}))).await.0["status"],403);
    assert_eq!(rpc(State(state.clone()),headers(),Json(json!({"operation":"resolvePublicFacts","args":{"prepareJobId":"run-one","itemIds":["outside"]}}))).await.0["status"],403);
    assert_eq!(rpc(State(state.clone()),headers(),Json(json!({"operation":"resolvePublicFacts","args":{"prepareJobId":"unowned-job","itemIds":["item-1"]}}))).await.0["status"],403);
    assert_eq!(rpc(State(state.clone()),headers(),Json(json!({"operation":"resolvePublicFacts","args":{"prepareJobId":"run-one","itemIds":["item-1"],"account":"baw-russia"}}))).await.0["status"],400);
    assert_eq!(rpc(State(state.clone()),headers(),Json(json!({"operation":"executeUrl","args":{"url":"https://example.invalid"}}))).await.0["status"],400);
    let mut other_company=state.clone();other_company.app.account=accounts::Profile::BawRussia;
    assert_eq!(rpc(State(other_company),headers(),Json(health.clone())).await.0["status"],403);
    state.app.change_job("run-one",|d|{row_mut(d,"jobs","run-one")?["conductor"]["leaseGeneration"]=json!(2);Ok(())}).await.unwrap();
    assert_eq!(rpc(State(state.clone()),headers(),Json(health.clone())).await.0["status"],403);
    // Epochs are immutable and monotonic. Exercise pause on a separate valid
    // launch instead of manufacturing a forbidden generation rollback.
    let (paused,_paused_temp)=fixture().await;
    paused.app.change_job("run-one",|d|{row_mut(d,"jobs","run-one")?["conductor"]["desiredState"]=json!("paused");Ok(())}).await.unwrap();
    assert_eq!(rpc(State(paused),headers(),Json(health.clone())).await.0["status"],403);
    state.active.store(false,std::sync::atomic::Ordering::Release);
    assert!(authenticate(&state,&headers()).is_err());
}
#[tokio::test]
async fn proposal_hints_only_narrow_canonical_read_and_never_admit_foreign_refs() {
    let (state,_temp)=fixture().await;
    scope_review(&state,&["item-1".into()]).await.unwrap();
    assert!(proposal_scope(&state,&json!([{"id":"inside-proposal","revision":1}]),"id","review").await.is_ok());
    {let mut hints=state.proposal_hints.lock().await;
        hints.seeded=true;hints.item_by_proposal.insert("outside-proposal".into(),"item-1".into());}
    assert!(proposal_scope(&state,&json!([{"id":"outside-proposal","revision":1}]),"id","review").await.is_err());
    assert!(proposal_scope(&state,&json!([{"id":"not-in-any-scope","revision":1}]),"id","review").await.is_err());
    assert!(authorize_post(&state,"selected-post","read").await.is_ok());
    state.proposal_hints.lock().await.items_by_post.insert("outside-post".into(),HashSet::from(["item-1".into()]));
    assert!(authorize_post(&state,"outside-post","read").await.is_err());
}
#[tokio::test]
async fn private_listener_is_loopback_only_and_envelopes_handler_status() {
    let (state,_temp)=fixture().await;
    let server=start(&state.app,state.launch.clone(),"test-only-capability").await.unwrap();
    assert!(server.rpc_url.starts_with("http://127.0.0.1:"));
    let address=server.rpc_url.strip_prefix("http://").unwrap().strip_suffix("/rpc").unwrap();
    let mut stream=tokio::net::TcpStream::connect(address).await.unwrap();
    let body=r#"{"operation":"reviewItems","args":{"itemIds":["outside"]}}"#;
    stream.write_all(format!("POST /rpc HTTP/1.1\r\nHost: {address}\r\nContent-Type: application/json\r\nConnection: close\r\nx-conductor-capability: test-only-capability\r\nx-conductor-run: run-one\r\nx-conductor-generation: 1\r\nContent-Length: {}\r\n\r\n{body}",body.len()).as_bytes()).await.unwrap();
    let mut bytes=Vec::new();stream.read_to_end(&mut bytes).await.unwrap();
    let text=String::from_utf8(bytes).unwrap();assert!(text.starts_with("HTTP/1.1 200"));
    let body:Value=serde_json::from_str(text.split("\r\n\r\n").nth(1).unwrap()).unwrap();
    assert_eq!(body["status"],403);assert!(body["body"]["error"].is_string());
}

#[tokio::test]
async fn reconcile_scopes_original_operation_and_reads_back_without_new_send() {
    let (mut state,temp)=fixture().await;
    let mut original=readback_recovery::tests::operation(chrono::Utc::now().timestamp()-60);
    original["id"]=json!("owned-original");original["action"]["actionId"]=json!("owned-original");
    original["proposalId"]=json!("inside-proposal");original["conductorRunId"]=json!("run-one");original["grantGeneration"]=json!(1);
    let actor=state.context.actor.public_json();let authority=dispatch_authority::approval_binding(&state.context.actor);
    original["approvedBy"]=actor.clone();original["executedBy"]=actor;
    original["dispatchAuthority"]=json!({"approved":authority,"executed":authority});
    let mut wrong_run=original.clone();wrong_run["id"]=json!("wrong-run");wrong_run["action"]["actionId"]=json!("wrong-run");wrong_run["conductorRunId"]=json!("another-run");
    let mut outside=original.clone();outside["id"]=json!("outside-original");outside["itemId"]=json!("outside");outside["action"]["actionId"]=json!("outside-original");
    state.app.change(|d|{d["operations"]=json!([original,wrong_run,outside]);row_mut(d,"proposals","inside-proposal")?["status"]=json!("unknown");Ok(())}).await.unwrap();
    let log=temp.path().join("conductor-readback-only.jsonl");
    state.app.node=PathBuf::from("node");state.app.bridge=temp.path().join("conductor-readback-fixture.mjs");
    let source=r#"import {appendFile} from 'node:fs/promises';let raw='';for await(const chunk of process.stdin)raw+=chunk;const request=JSON.parse(raw);await appendFile(__LOG__,request.operation+':'+request.actions[0].actionId+'\n');if(request.operation!=='readback')process.exit(19);process.stdout.write(JSON.stringify({ok:true,result:{account:request.account,results:request.actions.map(a=>({actionId:a.actionId,itemId:a.itemId,status:'verified'}))}}));"#.replace("__LOG__",&json!(log.to_string_lossy()).to_string());
    std::fs::write(&state.app.bridge,source).unwrap();
    scope_review(&state,&["item-1".into()]).await.unwrap();
    for operation in ["wrong-run","outside-original"] {
        let response=rpc(State(state.clone()),headers(),Json(json!({"operation":"reconcile","args":{"operationId":operation}}))).await.0;
        assert_eq!(response["status"],403);
    }
    assert!(list(&state.app.read().await.unwrap(),"jobs").iter().all(|job|job["kind"]!="reconcile"));
    let response=rpc(State(state.clone()),headers(),Json(json!({"operation":"reconcile","args":{"operationId":"owned-original"}}))).await.0;
    assert_eq!(response["status"],200);
    let job=response["body"]["jobId"].as_str().unwrap();
    tokio::time::timeout(Duration::from_secs(15),async {
        loop {if state.app.db.read_job(job).await.unwrap().unwrap()["status"]=="completed"{break;}tokio::time::sleep(Duration::from_millis(20)).await;}
    }).await.unwrap();
    let stored=state.app.read().await.unwrap();
    assert_eq!(std::fs::read_to_string(log).unwrap(),"readback:owned-original\n");
    assert_eq!(list(&stored,"operations").len(),3);
    let settled=row(&stored,"operations","owned-original").unwrap();
    assert_eq!(settled["action"],original["action"]);assert_eq!(settled["status"],"succeeded");
    let recorded=row(&stored,"jobs",job).unwrap();assert_eq!(recorded["refId"],"owned-original");assert_eq!(recorded["readbackOnly"],true);
    // A fresh transport after restart observes the same original readback job,
    // even if it never saw the original POST acknowledgement in memory.
    state.observed_jobs.lock().await.clear();
    assert_eq!(owned_job(&state,job).await.unwrap()["refId"],"owned-original");
}
