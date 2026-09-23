use super::*;

fn actor() -> Actor { Actor::local_owner("recovery-test") }

fn reviewed(d: &mut Value) -> String {
    d["items"] = json!([{"id":"item-1","itemId":"comment-1","objectId":"11391","postKey":"11391:post-1",
        "conversationKey":"11391:comment-1","contextEvidenceDigest":"a".repeat(64),"providerStatus":"new",
        "revision":1,"workflow":"prepared","draft":"Exact answer","author":"Customer","text":"Question"}]);
    d["conversations"] = json!([{"id":"chat","operatorId":"local-owner","messages":[{"id":"request","role":"user","text":"Send prepared"}]}]);
    let review = prepare(d, &actor(), "chat", "request", &json!({"mode":"execute_prepared","items":[{"id":"item-1","revision":1}]})).unwrap();
    d["conversations"][0]["messages"].as_array_mut().unwrap().push(json!({"id":"confirmation","role":"user","text":"Да, выполняй"}));
    review["reviewId"].as_str().unwrap().to_owned()
}

async fn stop_tasks(app: &App) {
    let tasks: Vec<_> = app.tasks.lock().await.drain().map(|(_,task)|task).collect();
    for task in &tasks { task.abort(); }
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while tasks.iter().any(|task| !task.is_finished()) { tokio::task::yield_now().await; }
    }).await.unwrap();
}

fn review(d: &Value) -> &Value { &d["conversations"][0]["actionReviews"][0] }

#[tokio::test]
async fn every_committed_boundary_recovers_once_without_readmission() {
    for fault in ["before_claim_commit", "claimed", "before_admitting_commit", "admitting", "execution_admitted", "before_admitted_commit", "admitted"] {
        let (mut app, temp) = crate::tests::test_app().await;
        let rid = app.change(|d| Ok(reviewed(d))).await.unwrap();
        let result = execute_review_inner(app.clone(), actor(), "chat", "confirmation", &json!({"reviewId":rid}),
            |at| if at == fault { Err(conflict("Injected process interruption")) } else { Ok(()) }).await;
        if fault=="before_admitted_commit" {
            assert_eq!(result.unwrap()["resultReadbackPending"],true,"known admission survives receipt write failure");
        } else { assert!(result.is_err(), "{fault}"); }
        stop_tasks(&app).await;
        // Reopen the actual temporary store; recovery cannot rely on memory.
        if let Database::Sqlite(pool) = &app.db { pool.close().await; }
        app.db = Database::Sqlite(open_db(&temp.path().join("workspace.sqlite")).await.unwrap());
        let before = app.read().await.unwrap();
        app.change(|d| { crate::recover(d); recover_execution_receipts(d) }).await.unwrap();
        let recovered = app.read().await.unwrap();
        assert_eq!(list(&before,"approvals").len(),list(&recovered,"approvals").len(),"{fault}");
        assert_eq!(list(&before,"operations").len(),list(&recovered,"operations").len(),"{fault}");
        assert_eq!(list(&before,"jobs").len(),list(&recovered,"jobs").len(),"{fault}");
        match fault {
            "before_claim_commit" => {
                assert_eq!(review(&recovered)["status"],"presented");
                assert!(list(&recovered,"approvals").is_empty());
            }
            "claimed" | "before_admitting_commit" | "admitting" => {
                assert_eq!(review(&recovered)["status"],"not_started","{fault}");
                assert_eq!(review(&recovered)["outcome"]["externalOutcome"],"not_started");
                assert_eq!(list(&recovered,"approvals").len(),1);
                assert!(list(&recovered,"operations").is_empty());
            }
            _ => {
                assert_eq!(review(&recovered)["status"],"admitted","{fault}: {}",review(&recovered));
                assert_eq!(review(&recovered)["execution"]["jobId"],recovered["jobs"][0]["id"]);
                assert_eq!(list(&recovered,"operations").len(),1);
                assert_eq!(review(&recovered)["outcome"]["terminal"],true);
                assert_ne!(review(&recovered)["outcome"]["externalOutcome"],"succeeded");
            }
        }
        app.change(recover_execution_receipts).await.unwrap();
        assert_eq!(app.read().await.unwrap(), recovered, "recovery must be idempotent: {fault}");
        if fault != "before_claim_commit" {
            assert!(execute_review(app.clone(),actor(),"chat","confirmation",&json!({"reviewId":rid})).await.is_err());
            assert_eq!(app.read().await.unwrap(),recovered);
        }
    }
}

async fn interrupted_admission() -> (App, tempfile::TempDir, String) {
    let (app, temp) = crate::tests::test_app().await;
    let rid = app.change(|d| Ok(reviewed(d))).await.unwrap();
    assert!(execute_review_inner(app.clone(),actor(),"chat","confirmation",&json!({"reviewId":rid}),
        |at| if at=="execution_admitted" {Err(conflict("Interrupted"))}else{Ok(())}).await.is_err());
    stop_tasks(&app).await;
    (app,temp,rid)
}

#[tokio::test]
async fn recovery_rejects_cross_actor_revision_route_attempt_and_job_evidence() {
    let (app,_temp,_rid) = interrupted_admission().await;
    let base = app.read().await.unwrap();
    for mutation in 0..11 {
        let mut d = base.clone();
        match mutation {
            0 => d["conversations"][0]["operatorId"] = json!("other-operator"),
            1 => d["approvals"][0]["approvedBy"]["id"] = json!("other-operator"),
            2 => d["approvals"][0]["proposals"][0]["revision"] = json!(999),
            3 => d["approvals"][0]["proposals"][0]["item"]["contextEvidenceDigest"] = json!("b".repeat(64)),
            4 => d["approvals"][0]["assistantReview"]["attemptId"] = json!("other-attempt"),
            5 => d["approvals"][0]["approvalAuthority"]["generation"] = json!("rotated"),
            6 => d["operations"][0]["target"]["objectId"] = json!("other-account"),
            7 => d["operations"][0]["action"]["reply"] = json!("Changed response"),
            8 => { let duplicate=d["jobs"][0].clone(); d["jobs"].as_array_mut().unwrap().push(duplicate); },
            9 => d["conversations"][0]["messages"][2]["text"] = json!("No"),
            _ => d["operations"][0]["dispatchAuthority"]["executed"]["actorId"] = json!("other-operator"),
        }
        let evidence = (d["approvals"].clone(),d["operations"].clone(),d["jobs"].clone());
        recover_execution_receipts(&mut d).unwrap();
        assert_eq!(review(&d)["status"],"recovery_required","mutation {mutation}");
        assert_eq!(review(&d)["outcome"]["externalOutcome"],"unknown");
        assert_eq!(review(&d)["outcome"]["operations"],json!([]));
        assert_eq!((d["approvals"].clone(),d["operations"].clone(),d["jobs"].clone()),evidence);
        let once=d.clone();recover_execution_receipts(&mut d).unwrap();assert_eq!(d,once);
    }
}

#[tokio::test]
async fn legacy_missing_identity_is_unknown_and_late_exact_evidence_recovers() {
    let (app,_temp,rid) = interrupted_admission().await;
    let base = app.read().await.unwrap();
    let mut legacy = base.clone();
    legacy["conversations"][0]["actionReviews"][0]["execution"] = json!({"confirmationUserMessageId":"confirmation"});
    legacy["conversations"][0]["actionReviews"][0]["status"] = json!("claimed");
    recover_execution_receipts(&mut legacy).unwrap();
    assert_eq!(review(&legacy)["outcome"]["externalOutcome"],"unknown");
    assert!(review(&legacy)["execution"]["approvalId"].is_null(),"must not guess a matching approval");
    let mut legacy = base.clone();
    legacy["conversations"][0]["actionReviews"][0]["execution"].as_object_mut().unwrap().remove("attemptId");
    legacy["approvals"][0].as_object_mut().unwrap().remove("assistantReview");
    recover_execution_receipts(&mut legacy).unwrap();
    assert_eq!(review(&legacy)["status"],"admitted","legacy exact known approval can be joined");
    let mut d=base.clone();
    d["operations"][0]["status"]=json!("unknown");d["jobs"][0]["status"]=json!("interrupted");
    recover_execution_receipts(&mut d).unwrap();
    assert_eq!(review(&d)["outcome"]["externalOutcome"],"unknown");
    assert_eq!(review(&d)["outcome"]["terminal"],true);
    let message_id=review(&d)["resultMessageId"].clone();
    d["operations"][0]["status"]=json!("succeeded");
    let job_id=d["jobs"][0]["id"].as_str().unwrap().to_owned();
    refresh_execution_receipts(&mut d,&job_id).unwrap();
    assert_eq!(review(&d)["outcome"]["externalOutcome"],"succeeded");
    assert_eq!(review(&d)["resultMessageId"],message_id);
    assert_eq!(d["conversations"][0]["messages"].as_array().unwrap().iter().filter(|m|m["actionExecution"]["reviewId"]==rid).count(),1);
}

#[tokio::test]
async fn fake_provider_accept_before_receipt_loss_never_executes_twice() {
    let (mut app,temp)=crate::tests::test_app().await;
    let log=temp.path().join("effects.jsonl");
    app.node=PathBuf::from("C:/Users/hello/AppData/Local/Microsoft/WinGet/Packages/OpenJS.NodeJS.LTS_Microsoft.Winget.Source_8wekyb3d8bbwe/node-v24.15.0-win-x64/node.exe");
    app.bridge=temp.path().join("fake-review-provider.mjs");
    let script=r#"import {appendFile} from 'node:fs/promises';
let input='';for await(const chunk of process.stdin)input+=chunk;const r=JSON.parse(input);
await appendFile(__LOG__,JSON.stringify({operation:r.operation})+'\n');
const result=r.operation==='context'?{itemId:'comment-1',objectId:'11391',postKey:'11391:post-1',conversationKey:'11391:comment-1',contextEvidenceDigest:'a'.repeat(64)}:{results:r.actions.map(a=>({actionId:a.actionId,itemId:a.itemId,status:r.operation==='execute'?'verified':'unknown'}))};
process.stdout.write(JSON.stringify({ok:true,result}));"#.replace("__LOG__",&json!(log.to_string_lossy()).to_string());
    std::fs::write(&app.bridge,script).unwrap();
    let rid=app.change(|d|Ok(reviewed(d))).await.unwrap();
    assert!(execute_review_inner(app.clone(),actor(),"chat","confirmation",&json!({"reviewId":rid}),
        |at|if at=="execution_admitted"{Err(conflict("Lost receipt commit"))}else{Ok(())}).await.is_err());
    tokio::time::timeout(std::time::Duration::from_secs(15),async {
        loop {
            let d=app.read().await.unwrap();
            if d["jobs"][0]["status"]=="completed" { break; }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    }).await.unwrap();
    stop_tasks(&app).await;
    let effects=std::fs::read_to_string(&log).unwrap();
    assert_eq!(effects.lines().filter(|line|serde_json::from_str::<Value>(line).unwrap()["operation"]=="execute").count(),1);
    if let Database::Sqlite(pool)=&app.db{pool.close().await;}
    app.db=Database::Sqlite(open_db(&temp.path().join("workspace.sqlite")).await.unwrap());
    for _ in 0..3 {app.change(|d|{crate::recover(d);recover_execution_receipts(d)}).await.unwrap();}
    assert!(execute_review(app.clone(),actor(),"chat","confirmation",&json!({"reviewId":rid})).await.is_err());
    let d=app.read().await.unwrap();
    assert_eq!(review(&d)["outcome"]["externalOutcome"],"unknown");
    assert_eq!(list(&d,"operations").len(),1);
    assert_eq!(list(&d,"approvals").len(),1);
    assert_eq!(std::fs::read_to_string(&log).unwrap(),effects,"recovery must not call provider at all");
}
