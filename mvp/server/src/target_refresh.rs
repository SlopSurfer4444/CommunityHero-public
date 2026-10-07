//! A single, connector-bound context read for a known local item. This never
//! dispatches an operation or treats absence from a provider page as deletion.
use super::*;
use axum::body::Bytes;

const KIND: &str = "target_refresh";

enum Claim {
    Existing(String),
    Started {
        job: String,
        binding: ConnectorBinding,
        target: Value,
        account: &'static str,
    },
}

fn claim(d: &mut Value, key: &str) -> ApiResult<Claim> {
    let binding = active_binding(d)?;
    let account = bridge_account(&binding)?;
    let item = list(d, "items")
        .iter()
        .find(|item| item["id"].as_str() == Some(key))
        .ok_or_else(|| ApiError(StatusCode::NOT_FOUND, "Local item not found".into()))?;
    let target = bound_item(&binding, item)?;
    let route = ResourceRef::from_item(&binding, &target).map_err(|e| conflict(e.0))?;
    // Only the selected company's connector can supply these opaque external
    // ids. The request never accepts an account, provider, or route body.
    if route.object_id.is_empty() || route.item_id.is_empty() {
        return Err(conflict("Local item has no exact connector target"));
    }
    if let Some(existing) = list(d, "jobs").iter().find(|job| {
        job["kind"] == KIND
            && job["refId"] == key
            && job["targetBinding"] == binding.to_json()
            && matches!(job["status"].as_str(), Some("running" | "queued"))
    }) {
        return Ok(Claim::Existing(required(existing, "id")?.to_string()));
    }
    let job = new_job(d, KIND, key)?;
    row_mut(d, "jobs", &job)?["targetBinding"] = binding.to_json();
    d["sync"]["targetRefresh"][key] = json!({
        "attemptedAt":now(),"jobId":job,"error":null
    });
    Ok(Claim::Started { job, binding, target, account })
}

fn admit(d: &mut Value, binding: &ConnectorBinding, target: &Value, snapshot: &Value) -> ApiResult<Value> {
    if active_binding(d)? != *binding {
        return Err(conflict("Connector changed during target refresh"));
    }
    let key = required(target, "id")?;
    let original = ResourceRef::from_item(binding, target).map_err(|e| conflict(e.0))?;
    let current = bound_item(binding, row(d, "items", key)?)?;
    let current = ResourceRef::from_item(binding, &current).map_err(|e| conflict(e.0))?;
    if current != original {
        return Err(conflict("Local target changed during refresh"));
    }
    let items = snapshot["items"]
        .as_array()
        .filter(|items| items.len() == 1)
        .ok_or_else(|| internal("Exact refresh omitted target"))?;
    let observed = bound_item(binding, &items[0])?;
    let observed = ResourceRef::from_item(binding, &observed).map_err(|e| conflict(e.0))?;
    if observed != original {
        return Err(conflict("Exact refresh returned another target"));
    }
    let ordered = snapshot_order::ordered(d, snapshot)?;
    if !ordered["items"].as_array().is_some_and(|items| items.len() == 1) {
        return Err(conflict("Exact refresh observation is older than current context or status"));
    }
    sync_scan::admit_exact_refresh(d, binding, target, &ordered)?;
    let item = row(d, "items", key)?;
    Ok(json!({
        "itemId":key,
        "refresh":"admitted",
        "itemRevision":item["revision"],
        "providerStatus":item["providerStatus"],
        "contextObservedAt":item["contextObservedAt"]
    }))
}

fn failure_diagnostic(error:&ApiError)->Value {
    let failure=dispatch_diagnostics::read_failure("target_context_read_failed",error);
    let mut diagnostic=failure.evidence["diagnostic"].clone();
    // The single target-refresh worker is read-only. Auth request facts live
    // in their own diagnostic; they never become a social mutation attempt.
    for key in ["providerCallAttempted","mutationOutcome","providerRetryAllowed"] {
        diagnostic[key]=failure.evidence[key].clone();
    }
    diagnostic
}
async fn run(app: App, job: String, binding: ConnectorBinding, target: Value, account: &'static str) -> ApiResult<Value> {
    let _total = performance::Span::job("target_refresh.run.total", &job);
    let key = required(&target, "id")?.to_string();
    let bridge_span = performance::Span::job("target_refresh.bridge.context", &job);
    let bridge_result = app.bridge("context", json!({
        "account":account,
        "objectId":target["objectId"],
        "itemId":target["itemId"],
        "snapshot":true
    })).await;
    drop(bridge_span);
    let outcome = match bridge_result {
        Ok(snapshot) => {
            let _admission = performance::Span::job("target_refresh.admit", &job);
            app.change_source_snapshot_scoped(storage::SourceReadIntent::Snapshot(&snapshot),|d| admit(d, &binding, &target, &snapshot)).await
        },
        Err(error) => Err(error),
    };
    if let Err(error) = &outcome {
        let diagnostic=failure_diagnostic(error);
        // Persist on the exact native job before worker finalization. The
        // finalizer updates status/error and preserves this closed evidence.
        app.change_job(&job, |d| {
            let stored=row_mut(d,"jobs",&job)?;
            if stored["kind"]==KIND && stored["refId"]==key && stored["targetBinding"]==binding.to_json()
                && matches!(stored["status"].as_str(),Some("running"|"queued")) {
                stored["diagnostic"]=diagnostic;
            }
            Ok(())
        }).await?;
        let message = error.1.clone();
        // A background scan or a later claim may own this marker by now.
        // The durable job itself remains the authoritative failure receipt.
        let _ = app.change_schedule(|d| {
            if d["sync"]["targetRefresh"][&key]["jobId"] == job {
                d["sync"]["targetRefresh"][&key]["completedAt"] = json!(now());
                d["sync"]["targetRefresh"][&key]["error"] = json!(message);
            }
            Ok(())
        }).await;
    }
    outcome
}

pub(crate) async fn start(State(app): State<App>, Path(key): Path<String>, body: Bytes) -> ApiResult<Json<Value>> {
    if key.is_empty() || key.len() > 256 {
        return Err(ApiError(StatusCode::BAD_REQUEST, "Invalid local item id".into()));
    }
    if !body.is_empty() {
        return Err(ApiError(StatusCode::BAD_REQUEST, "Target refresh takes no request body".into()));
    }
    let claim = app.change_source_claim(|d| claim(d, &key)).await?;
    match claim {
        Claim::Existing(job) => Ok(Json(json!({
            "jobId":job,"itemId":key,"status":"running","deduplicated":true
        }))),
        Claim::Started { job, binding, target, account } => {
            let worker = app.clone();
            let worker_job = job.clone();
            app.spawn(job.clone(), async move { run(worker, worker_job, binding, target, account).await });
            Ok(Json(json!({
                "jobId":job,"itemId":key,"status":"running","deduplicated":false
            })))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    fn item() -> Value {
        json!({
            "id":"local-one","objectId":"provider-object","itemId":"provider-comment",
            "postKey":"provider-post","conversationKey":"provider-thread",
            "providerStatus":"new","workflow":"prepared","revision":4,"draft":"human draft"
        })
    }

    #[test]
    fn claim_is_company_bound_and_deduplicates_active_job() {
        let mut d = empty();
        d["items"] = json!([item()]);
        assert!(matches!(claim(&mut d,"absent"), Err(ApiError(StatusCode::NOT_FOUND,_))));
        let first = match claim(&mut d,"local-one").unwrap() {
            Claim::Started { job, binding, target, account } => {
                assert_eq!(account, "likeavto");
                assert_eq!(target["connectorBinding"], binding.to_json());
                job
            }
            Claim::Existing(_) => panic!("first claim must start"),
        };
        assert_eq!(d["jobs"].as_array().unwrap().len(),1);
        assert!(matches!(claim(&mut d,"local-one").unwrap(),Claim::Existing(job) if job==first));
        assert_eq!(d["jobs"].as_array().unwrap().len(),1);
        let mut foreign = empty();
        foreign["items"] = json!([item()]);
        foreign["items"][0]["connectorBinding"] = accounts::Profile::BawRussia.binding();
        assert!(claim(&mut foreign,"local-one").is_err());
        assert_eq!(foreign["jobs"], json!([]));
        let mut baw=empty();
        baw["account"]=json!("BAW Russia");
        baw["connectorBinding"]=accounts::Profile::BawRussia.binding();
        baw["items"]=json!([item()]);
        match claim(&mut baw,"local-one").unwrap() {
            Claim::Started {account,binding,..} => {
                assert_eq!(account,"baw-russia");
                assert_eq!(binding.to_json(),accounts::Profile::BawRussia.binding());
            }
            Claim::Existing(_) => panic!("BAW first claim must start"),
        }
    }

    #[tokio::test]
    async fn scoped_storage_persists_binding_and_deduplicates_after_new_read() {
        let temp=tempfile::tempdir().unwrap();
        let db=Database::Sqlite(open_db(&temp.path().join("workspace.sqlite")).await.unwrap());
        db.change(|d|{d["items"]=json!([item()]);Ok(())}).await.unwrap();
        let (first,changed)=db.change_source_claim_observed(|d|claim(d,"local-one")).await.unwrap();
        assert!(changed);
        let first=match first {Claim::Started {job,..}=>job,Claim::Existing(_)=>panic!("fresh database already claimed")};
        let (second,changed)=db.change_source_claim_observed(|d|claim(d,"local-one")).await.unwrap();
        assert!(!changed);
        assert!(matches!(second,Claim::Existing(job) if job==first));
        let stored=db.read().await.unwrap();
        assert_eq!(stored["jobs"].as_array().unwrap().len(),1);
        assert_eq!(stored["jobs"][0]["targetBinding"],active_binding(&stored).unwrap().to_json());
        assert_eq!(stored["sync"]["targetRefresh"]["local-one"]["jobId"],first);
    }

    #[test]
    fn admission_uses_full_route_and_preserves_draft_while_bumping_revision() {
        let mut d = empty();
        d["items"] = json!([item()]);
        let binding = active_binding(&d).unwrap();
        let target = bound_item(&binding, &d["items"][0]).unwrap();
        let mut observed = item();
        observed["providerStatus"] = json!("closed");
        observed["draft"] = json!("");
        let mut wrong = observed.clone();
        wrong["postKey"] = json!("another-post");
        assert!(admit(&mut d,&binding,&target,&json!({"items":[wrong]})).is_err());
        assert_eq!(d["items"][0]["revision"],4);
        d["items"][0]["conversationKey"] = json!("moved-thread");
        assert!(admit(&mut d,&binding,&target,&json!({"items":[observed.clone()]})).is_err());
        d["items"][0]["conversationKey"] = json!("provider-thread");
        let result = admit(&mut d,&binding,&target,&json!({"items":[observed]})).unwrap();
        assert_eq!(result["refresh"],"admitted");
        assert_eq!(d["items"][0]["providerStatus"],"closed");
        assert_eq!(d["items"][0]["draft"],"human draft");
        assert!(d["items"][0]["revision"].as_u64().unwrap()>4);
        assert_eq!(d["operations"],json!([]));
    }

    #[tokio::test]
    async fn exact_refresh_source_writer_matches_full_admission_and_retains_history() {
        let temp = tempfile::tempdir().unwrap();
        let db = Database::Sqlite(open_db(&temp.path().join("workspace.sqlite")).await.unwrap());
        let history = "retained history ".repeat(256);
        db.change(|d| {
            d["items"] = json!([item()]);
            d["proposals"] = json!([{"id":"proposal","itemId":"local-one","status":"approved","text":"Approved text"}]);
            d["approvals"] = json!([{"id":"approval","status":"approved","proposals":[{"id":"proposal"}],"context":{"text":history}}]);
            d["operations"] = json!([{"id":"unknown-operation","itemId":"local-one","proposalId":"proposal","approvalId":"approval","status":"unknown","receipt":{"text":history}}]);
            d["jobs"] = json!([{"id":"retained-job","kind":"assistant","status":"completed","result":{"text":history}}]);
            d["audit"] = json!([{"id":"audit-old","action":"history","refId":"local-one","payload":history}]);
            d["feedback"] = json!([{"id":"feedback-old","itemId":"local-one","text":history}]);
            d["conversations"] = json!([{"id":"conversation-old","itemIds":["local-one"],"messages":[{"text":history}]}]);
            Ok(())
        }).await.unwrap();
        let before = db.read().await.unwrap();
        let binding = active_binding(&before).unwrap();
        let target = bound_item(&binding, &before["items"][0]).unwrap();
        let mut observed = item();
        observed["providerStatus"] = json!("closed");
        observed["contextObservedAt"] = json!("2026-09-24T12:05:00Z");
        observed["providerStatusObservedAt"] = observed["contextObservedAt"].clone();
        let snapshot = json!({"items":[observed]});
        let mut expected = before.clone();
        let expected_result = admit(&mut expected, &binding, &target, &snapshot).unwrap();
        let (result, changed) = db.change_source_snapshot_observed(|d| admit(d, &binding, &target, &snapshot)).await.unwrap();
        assert!(changed);
        assert_eq!(result, expected_result);
        let after = db.read().await.unwrap();
        for table in ["conversations","approvals","audit","feedback","operations","jobs"] {
            assert_eq!(after[table], before[table], "exact refresh retains {table}");
        }
        let mut comparable = after.clone();
        for key in ["observedAt","completedAt"] {
            assert!(after["sync"]["targetRefresh"]["local-one"][key].as_str().is_some());
            comparable["sync"]["targetRefresh"]["local-one"][key] = Value::Null;
            expected["sync"]["targetRefresh"]["local-one"][key] = Value::Null;
        }
        assert_eq!(comparable, expected);
        let mut stale = snapshot.clone();
        stale["items"][0]["contextObservedAt"] = json!("2026-09-24T12:04:00Z");
        assert!(db.change_source_snapshot_observed(|d| admit(d, &binding, &target, &stale)).await.is_err());
        assert_eq!(db.read().await.unwrap(), after, "stale exact observation must roll back");
        let mut foreign = binding.clone();
        foreign.account_id = "BAW Russia".into();
        assert!(db.change_source_snapshot_observed(|d| admit(d, &foreign, &target, &snapshot)).await.is_err());
        assert_eq!(db.read().await.unwrap(), after, "foreign binding must roll back");
        db.close().await;
    }

    #[test]
    fn older_exact_observation_is_not_reported_as_refreshed() {
        let mut d=empty();
        let mut current=item();
        current["contextObservedAt"]=json!("2026-09-24T12:02:00Z");
        d["items"]=json!([current]);
        let binding=active_binding(&d).unwrap();
        let target=bound_item(&binding,&d["items"][0]).unwrap();
        let mut observed=item();
        observed["contextObservedAt"]=json!("2026-09-24T12:01:00Z");
        assert!(admit(&mut d,&binding,&target,&json!({"items":[observed]})).is_err());
        assert_eq!(d["items"][0]["contextObservedAt"],"2026-09-24T12:02:00Z");
    }

    async fn request(port:u16,host:&str,path:&str,csrf:Option<&str>,body:&str)->(u16,Value) {
        let mut stream=tokio::net::TcpStream::connect((std::net::Ipv4Addr::LOCALHOST,port)).await.unwrap();
        let mut raw=format!("POST {path} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\nContent-Length: {}\r\n",body.len());
        if host=="remote.example.test" {raw.push_str("Origin: https://remote.example.test\r\n");}
        if let Some(csrf)=csrf {raw.push_str(&format!("x-csrf-token: {csrf}\r\n"));}
        raw.push_str("\r\n");raw.push_str(body);
        stream.write_all(raw.as_bytes()).await.unwrap();
        let mut bytes=vec![];
        tokio::time::timeout(Duration::from_secs(5),stream.read_to_end(&mut bytes)).await.unwrap().unwrap();
        let raw=String::from_utf8(bytes).unwrap();
        let (head,body)=raw.split_once("\r\n\r\n").unwrap();
        let status=head.lines().next().unwrap().split_whitespace().nth(1).unwrap().parse().unwrap();
        let body=serde_json::from_str(body).unwrap_or(Value::Null);
        (status,body)
    }

    #[tokio::test]
    async fn route_requires_owner_and_csrf_and_does_not_accept_foreign_target_body() {
        let temp=tempfile::tempdir().unwrap();
        let db=Database::Sqlite(open_db(&temp.path().join("workspace.sqlite")).await.unwrap());
        let listener=tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port=listener.local_addr().unwrap().port();
        let (events,_)=broadcast::channel(8);
        let app=App {lifecycle_task_count: Default::default(), lifecycle_admission: Arc::new(crate::runtime_lifecycle_startup::Admission::fixture(accounts::Profile::LikeAvto)), lifecycle_owner: Arc::new(crate::runtime_lifecycle_startup::Admission::fixture(accounts::Profile::LikeAvto).identity().clone()), lifecycle_provider_token: Default::default(), lifecycle_work: Default::default(), media_discovery: Default::default(),preparation_wake: Default::default(),provider_session: Default::default(),
            account:accounts::Profile::LikeAvto,navigation:crate::account_navigation::Navigation::root(), db,
            gate:Arc::new(writer_gate::WriterGate::default()),
            execution_gate:Arc::new(Mutex::new(())),
            preparation_workers: Default::default(),editorial_gate: Default::default(),assistant_gate:Arc::new(Mutex::new(())),assistant_chat_gate:Arc::new(Mutex::new(())),
            events,csrf:"exact-csrf".into(),auth:None,
            public_origin:Some("https://remote.example.test".into()),external_writes:false,
            port,data:temp.path().to_owned(),bridge:temp.path().join("unused.mjs"),
            node:temp.path().join("unused-node"),tasks:Arc::new(Mutex::new(HashMap::new())),
            bootstrap_cache:Arc::new(bootstrap_cache::Cache::default())
        };
        crate::runtime_lifecycle_app::initialize_app_fixture(&app).await.unwrap();
        let router=routes(app.clone(),temp.path().join("empty-web"));
        let server=tokio::spawn(async move{axum::serve(listener,router).await.unwrap()});
        let path="/api/engine/items/local-one/context-refresh";
        assert_eq!(request(port,"remote.example.test",path,None,"").await.0,401);
        let host=format!("127.0.0.1:{port}");
        assert_eq!(request(port,&host,path,None,"").await.0,403);
        assert_eq!(request(port,&host,path,Some("exact-csrf"),"{\"account\":\"BAW\"}").await.0,400);
        assert_eq!(request(port,&host,path,Some("exact-csrf"),"").await.0,404);
        server.abort();
    }

    #[tokio::test]
    async fn loopback_refresh_uses_one_read_only_bridge_call_and_finishes_durable_job() {
        let temp=tempfile::tempdir().unwrap();
        let bridge=temp.path().join("exact-context.mjs");
        tokio::fs::write(&bridge,r#"
            const chunks=[];
            for await (const chunk of process.stdin) chunks.push(chunk);
            const request=JSON.parse(Buffer.concat(chunks).toString());
            if(request.operation!=='context'||request.account!=='likeavto'||
               request.objectId!=='provider-object'||request.itemId!=='provider-comment'||
               request.snapshot!==true){
              process.stdout.write(JSON.stringify({ok:false,error:{code:'INVALID_CONTEXT_TARGET'}}));
              process.exit(0);
            }
            await new Promise(resolve=>setTimeout(resolve,500));
            process.stdout.write(JSON.stringify({ok:true,result:{items:[{
              id:'local-one',objectId:'provider-object',itemId:'provider-comment',
              postKey:'provider-post',conversationKey:'provider-thread',
              providerStatus:'closed',contextObservedAt:'2026-09-24T12:05:00Z',
              providerStatusObservedAt:'2026-09-24T12:05:00Z'
            }]}}));
        "#).await.unwrap();
        let db=Database::Sqlite(open_db(&temp.path().join("workspace.sqlite")).await.unwrap());
        db.change(|d|{d["items"]=json!([item()]);Ok(())}).await.unwrap();
        let listener=tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port=listener.local_addr().unwrap().port();
        let (events,_)=broadcast::channel(8);
        let app=App {lifecycle_task_count: Default::default(), lifecycle_admission: Arc::new(crate::runtime_lifecycle_startup::Admission::fixture(accounts::Profile::LikeAvto)), lifecycle_owner: Arc::new(crate::runtime_lifecycle_startup::Admission::fixture(accounts::Profile::LikeAvto).identity().clone()), lifecycle_provider_token: Default::default(), lifecycle_work: Default::default(), media_discovery: Default::default(),preparation_wake: Default::default(),provider_session: Default::default(),
            account:accounts::Profile::LikeAvto,navigation:crate::account_navigation::Navigation::root(),db,
            gate:Arc::new(writer_gate::WriterGate::default()),
            execution_gate:Arc::new(Mutex::new(())),
            preparation_workers: Default::default(),editorial_gate: Default::default(),assistant_gate:Arc::new(Mutex::new(())),assistant_chat_gate:Arc::new(Mutex::new(())),
            events,csrf:"exact-csrf".into(),auth:None,public_origin:None,external_writes:false,
            port,data:temp.path().to_owned(),bridge,node:PathBuf::from("node"),
            tasks:Arc::new(Mutex::new(HashMap::new())),
            bootstrap_cache:Arc::new(bootstrap_cache::Cache::default())
        };
        crate::runtime_lifecycle_app::initialize_app_fixture(&app).await.unwrap();
        let router=routes(app.clone(),temp.path().join("empty-web"));
        let server=tokio::spawn(async move{axum::serve(listener,router).await.unwrap()});
        let host=format!("127.0.0.1:{port}");
        let path="/api/engine/items/local-one/context-refresh";
        let first=request(port,&host,path,Some("exact-csrf"),"").await;
        assert_eq!(first.0,200,"{}",first.1);
        assert_eq!(first.1["deduplicated"],false);
        let second=request(port,&host,path,Some("exact-csrf"),"").await;
        assert_eq!(second.0,200,"{}",second.1);
        assert_eq!(second.1["deduplicated"],true);
        assert_eq!(second.1["jobId"],first.1["jobId"]);
        let job_id=first.1["jobId"].as_str().unwrap();
        let mut terminal=None;
        for _ in 0..200 {
            let job=app.db.read_job(job_id).await.unwrap().unwrap();
            if job["status"]!="running" {terminal=Some(job);break;}
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        let job=terminal.expect("refresh job must reach durable terminal state");
        assert_eq!(job["status"],"completed","{}",job);
        assert_eq!(job["result"]["refresh"],"admitted");
        let stored=app.read().await.unwrap();
        assert_eq!(stored["items"][0]["providerStatus"],"closed");
        assert_eq!(stored["items"][0]["draft"],"human draft");
        assert!(stored["operations"].as_array().unwrap().is_empty());
        server.abort();
    }
}

#[cfg(test)] mod auth_job_diagnostic_tests {
    use super::*;
    fn diagnostic()->Value {json!({"version":1,"diagnosticId":"00000000-0000-4000-8000-000000000001",
        "observation":"durable_barrier","stage":"fetch","cause":"timeout","watchdogFired":true,
        "elapsedMs":15000.125,"deadlineMs":15000,"responseReceived":false,"httpStatusValid":false,
        "responseDisposal":"not_requested","trigger":"proactive","generation":4})}
    #[tokio::test] async fn target_refresh_failure_keeps_closed_auth_diagnostic_on_exact_native_job() {
        let (mut app,temp)=crate::tests::test_app().await;
        let bridge=temp.path().join("auth-target-refresh-fixture.mjs");
        let error=json!({"code":"TRANSPORT_ERROR","operation":"read-auth-proactive-token-lifecycle-blocked",
            "transportStage":"read-auth-proactive-token-lifecycle-blocked","authExchangeDiagnostic":diagnostic(),
            "connectionState":{"status":"needs_user","reason":"recovery_uncertain"},"message":"PRIVATE"});
        let script=format!("const chunks=[];for await(const chunk of process.stdin)chunks.push(chunk);const req=JSON.parse(Buffer.concat(chunks).toString());if(req.operation!=='context'||req.account!=='likeavto')throw Error('wrong call');process.stdout.write(JSON.stringify({}));",json!({"ok":false,"error":error}));
        tokio::fs::write(&bridge,script).await.unwrap();app.bridge=bridge;app.node=PathBuf::from("node");
        app.change(|d|{d["items"]=json!([{"id":"auth-local","objectId":"o","itemId":"i",
            "postKey":"p","conversationKey":"t","revision":1}]);Ok(())}).await.unwrap();
        let claimed=app.change_source_claim(|d|claim(d,"auth-local")).await.unwrap();
        let (job,binding,target,account)=match claimed {
            Claim::Started{job,binding,target,account}=>(job,binding,target,account),Claim::Existing(_)=>panic!("new job")
        };
        let error=run(app.clone(),job.clone(),binding,target,account).await.unwrap_err();
        app.finish(&job,Err(error)).await;
        let saved=app.db.read_job(&job).await.unwrap().unwrap();
        assert_eq!(saved["status"],"failed");
        assert_eq!(saved["diagnostic"]["authExchangeDiagnostic"],diagnostic());
        assert_eq!(saved["diagnostic"]["nativeHttpStatus"],500);
        assert_eq!(saved["diagnostic"]["authRequestAttempted"],false);
        assert_eq!(saved["diagnostic"]["providerCallAttempted"],false);
        assert_eq!(saved["diagnostic"]["mutationOutcome"],"not-attempted");
        assert_eq!(saved["diagnostic"]["providerRetryAllowed"],false);
        assert!(!saved["diagnostic"].to_string().contains("PRIVATE"));
        let data=app.read().await.unwrap();assert!(list(&data,"operations").is_empty());
        app.db.close().await;
        let db=Database::Sqlite(open_db(&temp.path().join("workspace.sqlite")).await.unwrap());
        assert_eq!(db.read_job(&job).await.unwrap().unwrap()["diagnostic"],saved["diagnostic"]);
        db.close().await;
    }
}
