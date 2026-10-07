use super::*;

const NODE: &str = "C:/Users/hello/AppData/Local/Microsoft/WinGet/Packages/OpenJS.NodeJS.LTS_Microsoft.Winget.Source_8wekyb3d8bbwe/node-v24.15.0-win-x64/node.exe";

struct FaultHarness {
    app: App,
    actor: operator_auth::Actor,
    log: PathBuf,
    mode: PathBuf,
    _temp: tempfile::TempDir,
}

impl FaultHarness {
    async fn new(readback_mode: &str) -> Self {
        let temp = tempfile::tempdir().unwrap();
        let db = open_db(&temp.path().join("workspace.sqlite"))
            .await
            .unwrap();
        let (events, _) = broadcast::channel(8);
        let log = temp.path().join("calls.jsonl");
        let effect = temp.path().join("provider-effect.txt");
        let mode = temp.path().join("readback-mode.txt");
        std::fs::write(&mode, readback_mode).unwrap();
        let bridge = temp.path().join("fault-adapter.mjs");
        let script = r#"import {appendFile,readFile,writeFile} from 'node:fs/promises';
let input='';for await(const chunk of process.stdin)input+=chunk;const r=JSON.parse(input);
const action=r.actions?.[0];const itemId=r.itemId??action?.itemId;
await appendFile(__LOG__,JSON.stringify({operation:r.operation,itemId,actionId:action?.actionId})+'\n');
if(r.operation==='context'){
  process.stdout.write(JSON.stringify({ok:true,result:{itemId,objectId:'11391',postKey:'11391:post-1',conversationKey:'11391:'+itemId,contextEvidenceDigest:'a'.repeat(64)}}));
}else if(r.operation==='execute'){
  await writeFile(__EFFECT__,JSON.stringify({actionId:action.actionId,itemId:action.itemId}));
  process.stdout.write(JSON.stringify({ok:false,error:{code:'timeout_after_side_effect'}}));
}else if(r.operation==='readback'){
  const mode=(await readFile(__MODE__,'utf8')).trim();let saved=null;try{saved=JSON.parse(await readFile(__EFFECT__,'utf8'));}catch{}
  const status=mode!=='unknown'&&saved?.actionId===action.actionId&&saved?.itemId===action.itemId?'verified':'unknown';
  process.stdout.write(JSON.stringify({ok:true,result:{account:mode==='foreign-account'?'baw-russia':mode==='missing-account'?undefined:r.account,results:[{actionId:action.actionId,itemId:action.itemId,status}]}}));
}else{process.stdout.write(JSON.stringify({ok:false,error:{code:'unsupported'}}));}"#
            .replace("__LOG__", &json!(log.to_string_lossy()).to_string())
            .replace("__EFFECT__", &json!(effect.to_string_lossy()).to_string())
            .replace("__MODE__", &json!(mode.to_string_lossy()).to_string());
        std::fs::write(&bridge, script).unwrap();
        let app = App {lifecycle_task_count: Default::default(), lifecycle_admission: Arc::new(crate::runtime_lifecycle_startup::Admission::fixture(accounts::Profile::LikeAvto)), lifecycle_owner: Arc::new(crate::runtime_lifecycle_startup::Admission::fixture(accounts::Profile::LikeAvto).identity().clone()), lifecycle_provider_token: Default::default(), lifecycle_work: Default::default(), media_discovery: Default::default(),preparation_wake: Default::default(),provider_session: Default::default(),
            account: accounts::Profile::LikeAvto,navigation:crate::account_navigation::Navigation::root(),
            db: Database::Sqlite(db),
            gate: Arc::new(crate::writer_gate::WriterGate::default()),
            execution_gate: Arc::new(Mutex::new(())),
            preparation_workers: Default::default(),editorial_gate: Default::default(),assistant_gate: Arc::new(Mutex::new(())),assistant_chat_gate: Arc::new(Mutex::new(())),
            events,
            csrf: "fault-test".into(),
            auth: None,
            public_origin: None,
            external_writes: true,
            port: 0,
            data: temp.path().to_owned(),
            bridge,
            node: PathBuf::from(NODE),
            tasks: Arc::new(Mutex::new(HashMap::new())),
            bootstrap_cache: Arc::new(bootstrap_cache::Cache::default()),
        };
        crate::runtime_lifecycle_app::initialize_app_fixture(&app).await.unwrap();
        app.change(|data| {
            data["items"] = json!([{
                "id":"item-1","itemId":"comment-1","objectId":"11391",
                "postKey":"11391:post-1","conversationKey":"11391:comment-1",
                "contextEvidenceDigest":"a".repeat(64),"providerStatus":"new",
                "revision":1,"workflow":"attention","draft":"","waitingReason":"",
                "dueAt":null
            }]);
            connection_gate::fixture_open(data)?;
            Ok(())
        })
        .await
        .unwrap();
        Self {
            app,
            actor: operator_auth::Actor::local_owner("fault-test"),
            log,
            mode,
            _temp: temp,
        }
    }

    async fn execute_reviewed_close(&self) -> String {
        let Json(proposal) = proposal_new(
            State(self.app.clone()),
            Json(json!({"itemId":"item-1","kind":"close","expectedRevision":1})),
        )
        .await
        .unwrap();
        let Json(approval) = approval_new(
            State(self.app.clone()),
            axum::Extension(self.actor.clone()),
            Json(json!({"proposals":[{"id":proposal["id"],"revision":proposal["revision"]}]})),
        )
        .await
        .unwrap();
        let Json(started) = execute(
            State(self.app.clone()),
            axum::Extension(self.actor.clone()),
            Path(approval["id"].as_str().unwrap().to_owned()),
        )
        .await
        .unwrap();
        started["jobId"].as_str().unwrap().to_owned()
    }

    async fn wait_job(&self, job_id: &str) -> Value {
        tokio::time::timeout(Duration::from_secs(15), async {
            loop {
                let data = self.app.read().await.unwrap();
                let job = row(&data, "jobs", job_id).unwrap();
                if !matches!(job["status"].as_str(), Some("running" | "queued")) {
                    return data;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap()
    }

    fn calls(&self) -> Vec<Value> {
        std::fs::read_to_string(&self.log)
            .unwrap_or_default()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }
}

#[tokio::test]
async fn missing_job_skips_worker_without_finalization_retry() {
    let harness = FaultHarness::new("unknown").await;
    let exit = worker_supervision::run_if_active(&harness.app, "missing-job", async {
        panic!("a missing job must never execute its worker future");
        #[allow(unreachable_code)]
        Ok(json!(null))
    })
    .await;
    assert!(matches!(exit, worker_supervision::WorkerExit::Skipped));
    tokio::time::timeout(
        Duration::from_millis(200),
        worker_supervision::finalize(&harness.app, "missing-job", exit),
    )
    .await
    .unwrap();
    harness.app.db.close().await;
}

#[tokio::test]
async fn timeout_after_side_effect_uses_readback_without_blind_execute_retry() {
    let harness = FaultHarness::new("verified").await;
    let job = harness.execute_reviewed_close().await;
    let data = harness.wait_job(&job).await;
    assert_eq!(row(&data, "jobs", &job).unwrap()["status"], "completed");
    assert_eq!(data["operations"][0]["status"], "succeeded");
    assert_eq!(data["items"][0]["providerStatus"], "closed");
    let calls = harness.calls();
    assert_eq!(
        calls
            .iter()
            .filter(|call| call["operation"] == "execute")
            .count(),
        1
    );
    assert_eq!(
        calls
            .iter()
            .filter(|call| call["operation"] == "readback")
            .count(),
        1
    );
    harness.app.db.close().await;
}

#[tokio::test]
async fn unresolved_readback_stays_unknown_and_manual_reconcile_never_reexecutes() {
    let harness = FaultHarness::new("unknown").await;
    let job = harness.execute_reviewed_close().await;
    let first = harness.wait_job(&job).await;
    assert_eq!(first["operations"][0]["status"], "unknown");
    assert_eq!(first["proposals"][0]["status"], "unknown");
    let operation = first["operations"][0]["id"].as_str().unwrap().to_owned();

    std::fs::write(&harness.mode, "verified").unwrap();
    let Json(started) = reconcile(
        State(harness.app.clone()),
        axum::Extension(harness.actor.clone()),
        Path(operation),
    )
    .await
    .unwrap();
    let data = harness.wait_job(started["jobId"].as_str().unwrap()).await;
    assert_eq!(data["operations"][0]["status"], "succeeded");
    let calls = harness.calls();
    assert_eq!(
        calls
            .iter()
            .filter(|call| call["operation"] == "execute")
            .count(),
        1,
        "manual reconciliation must not repeat the provider mutation"
    );
    assert_eq!(
        calls
            .iter()
            .filter(|call| call["operation"] == "readback")
            .count(),
        2
    );
    harness.app.db.close().await;
}

#[tokio::test]
async fn verified_readback_for_foreign_or_missing_account_remains_unknown() {
    for mode in ["foreign-account","missing-account"] {
        let harness=FaultHarness::new(mode).await;
        let job=harness.execute_reviewed_close().await;
        let data=harness.wait_job(&job).await;
        assert_eq!(data["operations"][0]["evidence"]["results"][0]["status"],"verified");
        assert_eq!(data["operations"][0]["status"],"unknown");
        assert_eq!(harness.calls().iter().filter(|call|call["operation"]=="execute").count(),1);
        harness.app.db.close().await;
    }
}

#[tokio::test]
async fn panic_after_fake_provider_effect_becomes_unknown_then_readback_only() {
    let harness = FaultHarness::new("verified").await;
    let (job, operation) = harness
        .app
        .change(|data| {
            let proposal = json!({
                "id":"proposal-panic","itemId":"item-1","revision":1,
                "itemRevision":1,"kind":"close","text":"","status":"dispatching"
            });
            list_mut(data, "proposals").push(proposal);
            let job = new_job(data, "execute", "approval-panic")?;
            let mut target = row(data, "items", "item-1")?.clone();
            target["connectorBinding"] = legacy_binding();
            let authority=dispatch_authority::approval_binding(&harness.actor);
            let operation = json!({
                "id":"operation-panic","approvalId":"approval-panic",
                "proposalId":"proposal-panic","itemId":"item-1",
                "attemptId":"panic-attempt","createdAt":now(),
                "approvedBy":harness.actor.public_json(),"executedBy":harness.actor.public_json(),
                "dispatchAuthority":{"approved":authority,"executed":authority},
                "status":"dispatching","target":target,
                "action":{"actionId":"operation-panic","action":"close","itemId":"comment-1",
                    "objectId":"11391","conversationKey":"11391:comment-1",
                    "contextEvidenceDigest":"a".repeat(64),"expectedStatuses":["new"],"workTime":0}
            });
            list_mut(data, "operations").push(operation.clone());
            Ok((job, operation))
        })
        .await
        .unwrap();
    let worker = harness.app.clone();
    harness.app.spawn(job.clone(), async move {
        let permit=dispatch_authority::begin(&worker,&operation).await.unwrap().unwrap();
        let execution=dispatch_transport::execute(&worker,&operation,permit).await;
        assert!(execution.result.is_err(),"Synthetic provider reports timeout after its recorded effect");
        assert!(execution.local_error.is_none(),"Native cessation and original receipt must commit before the artificial panic");
        panic!("panic after provider side effect");
        #[allow(unreachable_code)]
        Ok(json!(null))
    });
    let failed = harness.wait_job(&job).await;
    assert_eq!(row(&failed, "jobs", &job).unwrap()["status"], "failed");
    assert_eq!(failed["operations"][0]["status"], "unknown");
    assert_eq!(failed["operations"][0]["dispatchPermit"]["phase"],"transport_settled");
    assert!(connection_gate::valid_permit(&failed["operations"][0]));
    let receipt=failed["operations"][0]["executeReceipt"].clone();
    assert!(receipt["error"].is_string());
    assert_eq!(harness.calls().iter().filter(|call|call["operation"]=="execute").count(),1);
    assert_eq!(
        failed["operations"][0]["evidence"]["workerExit"]["requiresReadback"],
        true
    );

    let Json(started) = reconcile(
        State(harness.app.clone()),
        axum::Extension(harness.actor.clone()),
        Path("operation-panic".into()),
    )
    .await
    .unwrap();
    let data = harness.wait_job(started["jobId"].as_str().unwrap()).await;
    assert_eq!(data["operations"][0]["status"], "succeeded");
    assert_eq!(data["operations"][0]["executeReceipt"],receipt);
    let calls = harness.calls();
    assert_eq!(
        calls
            .iter()
            .filter(|call| call["operation"] == "execute")
            .count(),
        1
    );
    assert_eq!(
        calls
            .iter()
            .filter(|call| call["operation"] == "readback")
            .count(),
        1
    );
    harness.app.db.close().await;
}

#[tokio::test]
async fn cancellation_preserves_terminal_cancel_and_rejects_external_abort() {
    let harness = FaultHarness::new("unknown").await;
    let assistant = harness.app.job("assistant", "conversation").await.unwrap();
    harness.app.spawn(assistant.clone(), async {
        std::future::pending::<()>().await;
        Ok(json!(null))
    });
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if harness.app.tasks.lock().await.contains_key(&assistant) {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let _ = cancel(State(harness.app.clone()), Path(assistant.clone()))
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if !harness.app.tasks.lock().await.contains_key(&assistant) {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(
        row(&harness.app.read().await.unwrap(), "jobs", &assistant).unwrap()["status"],
        "cancelled"
    );

    let execute_job = harness.app.job("execute", "approval-live").await.unwrap();
    let before = harness.app.read().await.unwrap();
    let rejected = cancel(State(harness.app.clone()), Path(execute_job.clone()))
        .await
        .unwrap_err();
    assert_eq!(rejected.0, StatusCode::CONFLICT);
    let after = harness.app.read().await.unwrap();
    assert_eq!(
        row(&after, "jobs", &execute_job).unwrap()["status"],
        "running"
    );
    assert_eq!(after["operations"], before["operations"]);
    harness.app.db.close().await;
}
