use super::*;

fn actor() -> operator_auth::Actor { operator_auth::Actor::local_owner("synthetic") }

async fn setup() -> (App, tempfile::TempDir, String) {
    setup_with_source(false).await
}

async fn setup_with_source(complete_post:bool) -> (App, tempfile::TempDir, String) {
    let (app, temp) = crate::tests::test_app().await;
    app.change(|d|connection_gate::fixture_open(d)).await.unwrap();
    if complete_post {app.change(|d|crate::tests::create_post_fixture(d,"item-1")).await.unwrap();}
    let p = app.change(|d| create_proposal(d, &json!({"itemId":"item-1","kind":"close","expectedRevision":1}))).await.unwrap();
    let a = approval_new(State(app.clone()), Extension(actor()), Json(json!({"proposals":[{"id":p["id"],"revision":p["revision"]}]}))).await.unwrap().0;
    (app, temp, required(&a,"id").unwrap().to_owned())
}

#[tokio::test]
async fn later_editorial_receipt_cannot_reauthorize_an_older_approval() {
    let (app,_temp,key)=setup_with_source(true).await;
    app.change(|d| {
        let proposal=d["proposals"][0]["id"].as_str().unwrap().to_owned();
        d["items"][0]["text"]=json!("New source after operator approved the earlier decision");
        crate::editorial_review::fixture_accept(d,&proposal).map_err(conflict)?;
        assert!(proposal_current(d,&d["proposals"][0]).is_ok());
        assert!(d["approvals"][0]["proposals"][0]["proposal"]["editorialReview"].is_null());
        Ok(())
    }).await.unwrap();
    let before=app.read().await.unwrap();
    let error=app.change(|d| admit(d,&actor(),&key,&payload(&key,&json!({}))?)).await.unwrap_err();
    assert_eq!(error.1,"Editorial review changed after approval");
    assert_eq!(app.read().await.unwrap(),before);
    let proposal=&before["proposals"][0];
    let new_approval=approval_new(State(app.clone()),Extension(actor()),Json(json!({"proposals":[
        {"id":proposal["id"],"revision":proposal["revision"]}]}))).await.unwrap().0;
    let new_key=required(&new_approval,"id").unwrap().to_owned();
    app.change(|d| admit(d,&actor(),&new_key,&payload(&new_key,&json!({}))?)).await.unwrap();
    let after=app.read().await.unwrap();
    assert_eq!(after["operations"].as_array().unwrap().len(),1);
    assert_eq!(after["operations"][0]["approvedEditorialReceiptSha256"],
        after["proposals"][0]["editorialReview"]["receiptSha256"]);
    app.db.close().await;
}

#[test]
fn execute_payload_binds_authoritative_path_and_rejects_ambiguous_bodies() {
    assert_eq!(payload("approval-a", &json!({"requestId":"key"})).unwrap(), json!({"approvalId":"approval-a","requestId":"key"}));
    assert_eq!(payload("approval-a", &json!({})).unwrap(), json!({"approvalId":"approval-a"}));
    for body in [json!(null), json!([]), json!({"approvalId":"approval-a"}), json!({"unknown":true})] {
        assert!(payload("approval-a", &body).is_err());
    }
}

#[tokio::test]
async fn pure_rejection_commits_only_negative_then_exact_reevaluation_admits_once() {
    let (app,_temp,key)=setup().await;
    app.change(|d|{d[connection_gate::FIELD]["state"]=json!("blocked");Ok(())}).await.unwrap();
    let body=payload(&key,&json!({"requestId":"negative-exact"})).unwrap();
    let before=app.read().await.unwrap();
    let first=app.change(|d|evaluate(d,&actor(),&key,&body)).await.unwrap();
    let receipt=match first{Evaluation::Rejected(receipt,_)=>receipt,_=>panic!("Expected pure negative")};
    let negative=app.read().await.unwrap();
    for table in ["jobs","operations","approvals","proposals"]{assert_eq!(negative[table],before[table]);}
    assert_eq!(receipt["viewStatus"],"waiting_dependency");
    let repeated=app.change(|d|evaluate(d,&actor(),&key,&body)).await.unwrap();
    assert!(matches!(repeated,Evaluation::Rejected(_, _)));
    assert_eq!(app.read().await.unwrap(),negative,"repeat must not re-evaluate or append negatives");
    app.change(|d|connection_gate::fixture_open(d)).await.unwrap();
    let controlled=payload(&key,&json!({"requestId":"negative-exact","reevaluate":{
        "evaluationId":receipt["evaluationId"],"receiptSha256":receipt["receiptSha256"]}})).unwrap();
    let committed=app.change(|d|evaluate(d,&actor(),&key,&controlled)).await.unwrap();
    assert!(matches!(committed,Evaluation::Admitted(_,Some(_))));
    let after=app.read().await.unwrap();
    assert_eq!(list(&after,"jobs").len(),1);assert_eq!(list(&after,"operations").len(),1);
    assert_eq!(list(&after,"audit").iter().filter(|r|r["action"]==local_admission::REJECTED_ACTION).count(),1);
    let replay=app.change(|d|evaluate(d,&actor(),&key,&controlled)).await.unwrap();
    assert!(matches!(replay,Evaluation::Admitted(_,None)));
    assert_eq!(app.read().await.unwrap(),after);app.db.close().await;
}

#[tokio::test]
async fn failure_after_ready_mutations_rolls_back_and_never_becomes_negative() {
    let (app,_temp,key)=setup().await;let before=app.read().await.unwrap();
    let body=payload(&key,&json!({"requestId":"partial-rollback"})).unwrap();
    let failed:ApiResult<()>=app.change(|d|{
        assert!(matches!(evaluate(d,&actor(),&key,&body)?,Evaluation::Admitted(_,Some(_))));
        Err(conflict("synthetic storage failure after execute mutations"))
    }).await;
    assert!(failed.is_err());assert_eq!(app.read().await.unwrap(),before);
    assert!(local_admission::find_rejection(&before,"execute","partial-rollback").unwrap().is_none());
    assert!(app.db.read_local_admission_receipt("execute","partial-rollback").await.unwrap().is_none());
    app.db.close().await;
}

#[tokio::test]
async fn execute_admission_crash_before_spawn_survives_restart_and_never_redispatches() {
    let (mut app, temp, key) = setup().await;
    let body = json!({"requestId":"execute-lost-ack"});
    // Commit exactly the production admission transaction, then crash before
    // spawning. No transport is invoked in this test.
    let (first, scheduled) = app.change(|d| admit(d, &actor(), &key, &payload(&key,&body)?)).await.unwrap();
    assert!(scheduled.is_some());
    let before = app.read().await.unwrap();
    assert_eq!(list(&before,"operations").len(),1);
    assert_eq!(before["approvals"][0]["status"],"consumed");
    app.db.close().await;
    app.db = Database::Sqlite(open_db(&temp.path().join("workspace.sqlite")).await.unwrap());
    app.change(|d| { recover(d).unwrap(); bump(&mut d["items"][0]); Ok(()) }).await.unwrap();
    // Receipt replay precedes current eligibility and external-write settings.
    app.external_writes = false;
    let replay = post(State(app.clone()),Extension(actor()),Path(key.clone()),Bytes::from(body.to_string())).await.unwrap().0;
    assert_eq!(replay["jobId"],first["jobId"]); assert_eq!(replay["approvalId"],key);
    assert_eq!(replay["replayed"],true);
    let looked = local_admission::lookup(State(app.clone()),Extension(actor()),Path(("execute".into(),"execute-lost-ack".into()))).await.unwrap().0;
    assert_eq!(looked["status"],"committed"); assert_eq!(looked["result"],replay);
    use sha2::{Digest,Sha256};
    assert_eq!(looked["payloadHash"],format!("{:x}",Sha256::digest(json!({"approvalId":key}).to_string().as_bytes())));
    assert_eq!(looked["retryAuthorized"],false);
    let d = app.read().await.unwrap();
    assert_eq!(list(&d,"jobs").len(),1); assert_eq!(d["jobs"][0]["status"],"interrupted");
    assert_eq!(list(&d,"operations").len(),1); assert_eq!(d["operations"][0]["status"],"unknown");
    assert!(app.tasks.lock().await.is_empty());
    assert_eq!(list(&d,"audit").iter().filter(|r|r["action"]==local_admission::ACTION).count(),1);
    app.db.close().await;
}

#[tokio::test]
async fn execute_admission_concurrent_duplicate_commits_one_job_and_dispatch_payload() {
    let (app, _temp, key) = setup().await;
    let body = payload(&key,&json!({"requestId":"execute-race"})).unwrap();
    let admit_once = || app.change(|d| admit(d,&actor(),&key,&body));
    let (a,b) = tokio::join!(admit_once(),admit_once());
    let (a,sa) = a.unwrap(); let (b,sb) = b.unwrap();
    assert_eq!(a["jobId"],b["jobId"]); assert_ne!(a["replayed"],b["replayed"]);
    assert_ne!(sa.is_some(),sb.is_some(),"only the transaction winner may dispatch");
    let d = app.read().await.unwrap();
    assert_eq!(list(&d,"jobs").len(),1); assert_eq!(list(&d,"operations").len(),1);
    assert_eq!(list(&d,"audit").iter().filter(|r|r["action"]==local_admission::ACTION).count(),1);
    app.db.close().await;
}

#[tokio::test]
async fn execute_admission_rolls_back_job_operations_consumption_and_receipt_together() {
    let (app, _temp, key) = setup().await;
    let before = app.read().await.unwrap();
    let result: ApiResult<()> = app.change(|d| {
        admit(d,&actor(),&key,&payload(&key,&json!({"requestId":"execute-rollback"}))?)?;
        Err(conflict("synthetic transaction failure after receipt"))
    }).await;
    assert!(result.is_err()); assert_eq!(app.read().await.unwrap(),before);
    assert!(app.db.read_local_admission_receipt("execute","execute-rollback").await.unwrap().is_none());
    assert!(app.tasks.lock().await.is_empty()); app.db.close().await;
}

#[tokio::test]
async fn execute_admission_distinct_keys_racing_one_approval_consume_only_once() {
    let (app, _temp, key) = setup().await;
    let a = payload(&key,&json!({"requestId":"first-key"})).unwrap();
    let b = payload(&key,&json!({"requestId":"second-key"})).unwrap();
    let (a,b) = tokio::join!(app.change(|d| admit(d,&actor(),&key,&a)),app.change(|d| admit(d,&actor(),&key,&b)));
    assert_ne!(a.is_ok(),b.is_ok());
    let rejected = if let Err(e)=a {e} else {b.unwrap_err()};
    assert_eq!(rejected.0,StatusCode::CONFLICT);
    let d = app.read().await.unwrap();
    assert_eq!(list(&d,"jobs").len(),1); assert_eq!(list(&d,"operations").len(),1);
    assert_eq!(list(&d,"audit").iter().filter(|r|r["action"]==local_admission::ACTION).count(),1);
    app.db.close().await;
}

#[tokio::test]
async fn execute_admission_rejects_cross_path_actor_role_account_and_new_key_reexecution() {
    let (app, _temp, key) = setup().await;
    let body = json!({"requestId":"execute-bound"});
    app.change(|d| admit(d,&actor(),&key,&payload(&key,&body)?)).await.unwrap();
    let before = app.read().await.unwrap();
    // Replay binding is evaluated before looking up the second path at all.
    assert_eq!(run(app.clone(),actor(),"other-approval".into(),body.clone()).await.unwrap_err().0,StatusCode::CONFLICT);
    for field in ["id","role"] {
        let mut foreign = actor();
        if field=="id" {foreign.id="another-operator".into();} else {foreign.role="operator".into();}
        assert_eq!(run(app.clone(),foreign,key.clone(),body.clone()).await.unwrap_err().0,StatusCode::CONFLICT);
    }
    let mut foreign_app = app.clone(); foreign_app.account = accounts::Profile::BawRussia;
    assert_eq!(run(foreign_app,actor(),key.clone(),body.clone()).await.unwrap_err().0,StatusCode::CONFLICT);
    assert_eq!(app.read().await.unwrap(),before,"Mismatched replay bindings must not mutate the workspace");
    assert_eq!(run(app.clone(),actor(),key.clone(),json!({"requestId":"different-key"})).await.unwrap_err().0,StatusCode::CONFLICT);
    let after=app.read().await.unwrap();
    // A new key records a durable no-attempt rejection of the consumed
    // approval. Every control and prior audit entry must remain exact.
    let prior_audit=list(&before,"audit");let current_audit=list(&after,"audit");
    assert_eq!(current_audit.len(),prior_audit.len()+1);
    assert_eq!(&current_audit[..prior_audit.len()],prior_audit);
    let mut controls=after.clone();controls["audit"]=before["audit"].clone();
    assert_eq!(controls,before,"Rejected new keys cannot create or alter execution state");
    let committed=local_admission::find_receipt(&before,"execute","execute-bound").unwrap().unwrap();
    let rejected=local_admission::find_rejection(&after,"execute","different-key").unwrap().unwrap();
    assert_eq!(rejected,current_audit.last().unwrap());
    assert_eq!(rejected["action"],local_admission::REJECTED_ACTION);
    assert_eq!(rejected["kind"],"execute");assert_eq!(rejected["requestId"],"different-key");
    assert_eq!(rejected["refId"],"different-key");assert_eq!(rejected["approvalId"],key);
    for field in ["account","actorId","actorRole","requestHash","payloadHash"] {
        assert_eq!(rejected[field],committed[field],"{field}");
    }
    assert_eq!(rejected["actorAuthority"],dispatch_authority::approval_binding(&actor()));
    assert_eq!(rejected["connectionBinding"],active_binding(&before).unwrap().to_json());
    assert_eq!(rejected["gateEpoch"],before[connection_gate::FIELD]["gateEpoch"]);
    assert_eq!(rejected["evaluationIndex"],1);assert_eq!(rejected["reason"],"precondition_changed");
    assert_eq!(rejected["blockingJobIds"],json!([]));
    assert_eq!(rejected["parentEvaluationId"],Value::Null);assert_eq!(rejected["parentReceiptSha256"],Value::Null);
    assert_eq!(rejected["noAttemptProof"],json!({"executeJobCreated":false,"operationCreated":false,
        "approvalConsumed":false,"providerDispatchArmed":false}));
    assert!(local_admission::find_receipt(&after,"execute","different-key").unwrap().is_none());
    let looked=local_admission::lookup(State(app.clone()),Extension(actor()),Path(("execute".into(),"different-key".into()))).await.unwrap().0;
    assert_eq!(looked["status"],"rejected_local");assert_eq!(looked["viewStatus"],"rejected_local");
    assert_eq!(looked["evaluationId"],rejected["evaluationId"]);assert_eq!(looked["receiptSha256"],rejected["receiptSha256"]);
    assert_eq!(looked["retryAuthorized"],false);assert_eq!(looked["result"],Value::Null);
    assert_eq!(run(app.clone(),actor(),key.clone(),json!({"requestId":"different-key"})).await.unwrap_err().0,StatusCode::CONFLICT);
    assert_eq!(app.read().await.unwrap(),after,"Ordinary negative replay must not append another evaluation or execute again");
    assert!(app.tasks.lock().await.is_empty());
    app.db.close().await;
}

#[tokio::test]
async fn execute_admission_legacy_shape_and_empty_http_body_remain_supported() {
    let (app, _temp, key) = setup().await;
    let (result,scheduled) = app.change(|d| admit(d,&actor(),&key,&payload(&key,&json!({}))?)).await.unwrap();
    assert_eq!(result.as_object().unwrap().len(),1); assert!(result["jobId"].is_string()); assert!(scheduled.is_some());
    let before = app.read().await.unwrap();
    assert!(list(&before,"audit").iter().all(|r|r["action"]!=local_admission::ACTION));
    // A consumed approval conflicts, proving the empty request got past the
    // body parser. Malformed JSON and forged path fields fail before admission.
    assert_eq!(post(State(app.clone()),Extension(actor()),Path(key.clone()),Bytes::new()).await.unwrap_err().0,StatusCode::CONFLICT);
    for body in ["null","{","{\"approvalId\":\"forged\"}","{\"requestId\":null}"] {
        assert_eq!(post(State(app.clone()),Extension(actor()),Path(key.clone()),Bytes::copy_from_slice(body.as_bytes())).await.unwrap_err().0,StatusCode::BAD_REQUEST);
    }
    assert_eq!(app.read().await.unwrap(),before); app.db.close().await;
}

#[tokio::test]
async fn execute_admission_replay_rejects_corrupt_saved_job_or_approval_identity() {
    let (app, _temp, key) = setup().await;
    let body = payload(&key,&json!({"requestId":"execute-corrupt"})).unwrap();
    app.change(|d| admit(d,&actor(),&key,&body)).await.unwrap();
    let d = app.read().await.unwrap();
    for field in ["jobId","approvalId"] {
        let mut corrupted = d.clone();
        list_mut(&mut corrupted,"audit").iter_mut().find(|r|r["action"]==local_admission::ACTION).unwrap()["result"][field]=json!("");
        let request = local_admission::request(&corrupted,"execute",&body,&actor()).unwrap().unwrap();
        assert_eq!(local_admission::replay(&corrupted,&request,&actor()).unwrap_err().0,StatusCode::INTERNAL_SERVER_ERROR);
    }
    let mut corrupted = d.clone();
    list_mut(&mut corrupted,"audit").iter_mut().find(|r|r["action"]==local_admission::ACTION).unwrap()["result"]["approvalId"]=json!("different-approval");
    let request = local_admission::request(&corrupted,"execute",&body,&actor()).unwrap().unwrap();
    assert_eq!(local_admission::replay(&corrupted,&request,&actor()).unwrap_err().0,StatusCode::INTERNAL_SERVER_ERROR);
    app.db.close().await;
}

#[tokio::test]
async fn execute_admission_http_route_extracts_empty_legacy_and_keyed_bodies() {
    let (app, _temp, key) = setup().await;
    let body = json!({"requestId":"http-key"});
    let (first,_) = app.change(|d| admit(d,&actor(),&key,&payload(&key,&body)?)).await.unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let router = Router::new().route("/api/approvals/{id}/execute",axum::routing::post(post))
        .layer(Extension(actor())).with_state(app.clone());
    let server = tokio::spawn(async move { axum::serve(listener,router).await.unwrap(); });
    for (json_header,body,status) in [(false,"",409),(true,"",409),(true,"{}",409),
        (true,"{\"requestId\":\"http-key\"}",200),(true,"{",400)] {
        let mut socket = tokio::net::TcpStream::connect(address).await.unwrap();
        let header = if json_header {"Content-Type: application/json\r\n"} else {""};
        let request = format!("POST /api/approvals/{key}/execute HTTP/1.1\r\nHost: localhost\r\n{header}Content-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len());
        socket.write_all(request.as_bytes()).await.unwrap();
        let mut response = vec![];
        tokio::time::timeout(Duration::from_secs(5),socket.read_to_end(&mut response)).await.unwrap().unwrap();
        let response = String::from_utf8(response).unwrap();
        assert!(response.starts_with(&format!("HTTP/1.1 {status}")),"{response}");
        if status==200 {
            let result: Value = serde_json::from_str(response.split_once("\r\n\r\n").unwrap().1).unwrap();
            assert_eq!(result["jobId"],first["jobId"]); assert_eq!(result["replayed"],true);
        }
    }
    server.abort(); let _ = server.await;
    assert!(app.tasks.lock().await.is_empty());
    app.db.close().await;
}
