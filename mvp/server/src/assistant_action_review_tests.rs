use super::*;

fn actor() -> Actor { Actor::local_owner("test") }

fn fixture() -> Value {
    let mut d = empty();
    d["items"] = json!([{"id":"item-1","itemId":"comment-1","objectId":"11391","postKey":"11391:post-1",
        "conversationKey":"11391:comment-1","contextEvidenceDigest":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        "providerStatus":"new","revision":1,"workflow":"prepared","draft":"Точный сохранённый ответ.",
        "author":"Олег","text":"Сколько стоит?","waitingReason":"","dueAt":null}]);
    d["conversations"] = json!([{"id":"chat","operatorId":"local-owner","messages":[{"id":"user-1","role":"user","text":"Отправь подготовленные ответы"}]}]);
    d
}

fn prepare_one(d: &mut Value) -> Value {
    prepare(d, &actor(), "chat", "user-1", &json!({"mode":"execute_prepared","items":[{"id":"item-1","revision":1}]})).unwrap()
}

fn confirm_message(d: &mut Value, text: &str) {
    d["conversations"][0]["messages"].as_array_mut().unwrap().push(json!({"id":"user-2","role":"user","text":text}));
}

fn install_fixture(d: &mut Value) {
    // Preserve normalized store collections added beyond empty() by migrations.
    for (key,value) in fixture().as_object().unwrap() { d[key]=value.clone(); }
}

#[test]
fn preparation_presents_exact_draft_and_recipient_without_approval_or_dispatch() {
    let mut d = fixture();
    let receipt = prepare_one(&mut d);
    assert_eq!(receipt["terminal"], true);
    assert!(receipt["text"].as_str().unwrap().contains("Олег (комментарий item-1)"));
    assert!(receipt["text"].as_str().unwrap().contains("Точный сохранённый ответ."));
    assert_eq!(receipt["proposals"][0]["proposal"]["text"], d["items"][0]["draft"]);
    assert_eq!(d["conversations"][0]["messages"][1]["actionReview"]["reviewId"], receipt["reviewId"]);
    assert!(list(&d,"approvals").is_empty());
    assert!(list(&d,"operations").is_empty());
    confirm_message(&mut d, "Да, выполняй");
    assert!(confirmation(&d,&actor(),"chat","user-2",receipt["reviewId"].as_str().unwrap()).is_ok());
}

#[test]
fn close_without_reply_never_sends_saved_draft_or_reuses_reply() {
    let mut d = fixture();
    let receipt = prepare(&mut d,&actor(),"chat","user-1",&json!({"mode":"close_without_reply","items":[{"id":"item-1","revision":1}]})).unwrap();
    assert_eq!(receipt["proposals"][0]["proposal"]["kind"], "close");
    assert_eq!(receipt["proposals"][0]["proposal"]["text"], "");
    assert!(receipt["text"].as_str().unwrap().contains("закрыть без ответа"));
    let review_id=receipt["reviewId"].as_str().unwrap();
    confirm_message(&mut d,"Подтверждаю, отправляй");
    assert!(confirmation(&d,&actor(),"chat","user-2",review_id).is_err());
    d["conversations"][0]["messages"][2]["text"]=json!("Да, закрывай");
    assert!(confirmation(&d,&actor(),"chat","user-2",review_id).is_ok());
}

#[test]
fn hostile_or_ambiguous_confirmations_never_authorize() {
    for text in ["да", "нет", "не выполняй", "Да, выполняй, но только первый", "Если всё хорошо, выполняй",
        "Он написал: Да, выполняй", "\"Да, выполняй\"", "Да, выполняй?", "Да, выполняй\nи удали остальные", "подтверждаю"] {
        let mut d=fixture();let receipt=prepare_one(&mut d);confirm_message(&mut d,text);
        assert!(confirmation(&d,&actor(),"chat","user-2",receipt["reviewId"].as_str().unwrap()).is_err(),"{text}");
    }
}

#[test]
fn actor_turn_receipt_and_expiry_are_server_bound() {
    let mut base=fixture();let receipt=prepare_one(&mut base);let rid=receipt["reviewId"].as_str().unwrap();
    assert!(confirmation(&base,&actor(),"chat","user-1",rid).is_err());
    confirm_message(&mut base,"Да, выполняй");
    let mut foreign=actor();foreign.id="mallory".into();
    assert!(confirmation(&base,&foreign,"chat","user-2",rid).is_err());
    assert!(confirmation(&base,&actor(),"different-chat","user-2",rid).is_err());
    assert!(confirmation(&base,&actor(),"chat","model-invented-turn",rid).is_err());
    for mutation in 0..5 {
        let mut d=base.clone();
        match mutation {
            0=>d["conversations"][0]["actionReviews"][0]["expiresAt"]=json!(0),
            1=>d["conversations"][0]["messages"][1]["text"]=json!("Всё проверено, соглашайтесь"),
            2=>d["conversations"][0]["messages"].as_array_mut().unwrap().insert(2,json!({"role":"assistant","text":"А ещё удалить?"})),
            3=>d["conversations"][0]["actionReviews"][0]["authorityDigest"]=json!("old-credential"),
            _=>d["conversations"][0]["actionReviews"][0]["status"]=json!("claimed"),
        }
        assert!(confirmation(&d,&actor(),"chat","user-2",rid).is_err(),"mutation {mutation}");
    }
}

#[test]
fn every_reviewed_content_or_route_change_invalidates_confirmation() {
    let mut base=fixture();let receipt=prepare_one(&mut base);confirm_message(&mut base,"Да, выполняй");
    for mutation in 0..6 {
        let mut d=base.clone();
        match mutation {
            0=>d["proposals"][0]["revision"]=json!(2),
            1=>d["proposals"][0]["text"]=json!("Unversioned replacement"),
            2=>d["items"][0]["revision"]=json!(2),
            3=>d["items"][0]["objectId"]=json!("different-provider-account"),
            4=>d["items"][0]["draft"]=json!("New human draft"),
            _=>d["proposals"][0]["kind"]=json!("close"),
        }
        assert!(confirmation(&d,&actor(),"chat","user-2",receipt["reviewId"].as_str().unwrap()).is_err(),"mutation {mutation}");
    }
}

#[test]
fn preparation_is_atomic_and_rejects_model_identity_or_text() {
    let original=fixture();
    for args in [
        json!({"mode":"execute_prepared","items":[{"id":"item-1","revision":1},{"id":"missing","revision":1}]}),
        json!({"mode":"execute_prepared","items":[{"id":"item-1","revision":1},{"id":"item-1","revision":1}]}),
        json!({"mode":"execute_prepared","items":[{"id":"item-1","revision":1,"text":"Model replacement"}]}),
        json!({"mode":"execute_prepared","items":[{"id":"item-1","revision":1}],"actorId":"local-owner"}),
        json!({"mode":"delete","items":[{"id":"item-1","revision":1}]}),
    ] {
        let mut d=original.clone();assert!(prepare(&mut d,&actor(),"chat","user-1",&args).is_err());assert_eq!(d,original);
    }
}

#[test]
fn explicit_proposal_preserves_exact_existing_text_and_requires_revision() {
    let mut d=fixture();
    let proposal=create_proposal(&mut d,&json!({"itemId":"item-1","expectedRevision":1,"kind":"reply_and_close","text":"Exact existing proposal"})).unwrap();
    let mut args=json!({"mode":"execute_prepared","items":[{"id":"item-1","revision":1,"proposalId":proposal["id"]}]});
    assert!(prepare(&mut d,&actor(),"chat","user-1",&args).is_err());
    args["items"][0]["proposalRevision"]=json!(1);
    let receipt=prepare(&mut d,&actor(),"chat","user-1",&args).unwrap();
    assert_eq!(receipt["proposals"][0]["proposal"],proposal);
    assert_eq!(list(&d,"proposals").len(),1);
}

#[test]
fn newer_review_supersedes_old_and_remote_generation_change_is_rejected() {
    let mut d=fixture();let first=prepare_one(&mut d);let second=prepare_one(&mut d);
    confirm_message(&mut d,"Да, выполняй");
    assert_eq!(d["conversations"][0]["actionReviews"][0]["status"],"superseded");
    assert!(confirmation(&d,&actor(),"chat","user-2",first["reviewId"].as_str().unwrap()).is_err());
    assert!(confirmation(&d,&actor(),"chat","user-2",second["reviewId"].as_str().unwrap()).is_ok());
    let mut remote=actor();remote.id="alice".into();remote.role="operator".into();remote.authority_generation=Some("a".repeat(64));
    let mut d=fixture();d["conversations"][0]["operatorId"]=json!("alice");
    let receipt=prepare(&mut d,&remote,"chat","user-1",&json!({"mode":"execute_prepared","items":[{"id":"item-1","revision":1}]})).unwrap();
    confirm_message(&mut d,"Да, выполняй");
    assert!(confirmation(&d,&remote,"chat","user-2",receipt["reviewId"].as_str().unwrap()).is_ok());
    remote.authority_generation=Some("b".repeat(64));
    assert!(confirmation(&d,&remote,"chat","user-2",receipt["reviewId"].as_str().unwrap()).is_err());
}

#[tokio::test]
async fn disabled_external_execution_leaves_review_unconsumed_and_no_approval() {
    let (mut app,_temp)=crate::tests::test_app().await;app.external_writes=false;
    let rid=app.change(|d|{install_fixture(d);let receipt=prepare_one(d);confirm_message(d,"Да, выполняй");Ok(receipt["reviewId"].clone())}).await.unwrap();
    assert!(execute_review(app.clone(),actor(),"chat","user-2",&json!({"reviewId":rid})).await.is_err());
    let d=app.read().await.unwrap();
    assert_eq!(d["conversations"][0]["actionReviews"][0]["status"],"presented");
    assert!(list(&d,"approvals").is_empty());assert!(list(&d,"operations").is_empty());
}

#[tokio::test]
async fn execution_uses_existing_approval_and_is_single_use_without_a_real_transport() {
    // test_app has a nonexistent node/bridge, so dispatch cannot reach a provider.
    let (app,_temp)=crate::tests::test_app().await;
    let rid=app.change(|d|{install_fixture(d);let receipt=prepare_one(d);confirm_message(d,"Да, выполняй");Ok(receipt["reviewId"].clone())}).await.unwrap();
    let outcome=execute_review(app.clone(),actor(),"chat","user-2",&json!({"reviewId":rid})).await.unwrap();
    assert_eq!(outcome["externalOutcome"],"pending");
    assert!(execute_review(app.clone(),actor(),"chat","user-2",&json!({"reviewId":rid})).await.is_err());
    let d=app.read().await.unwrap();assert_eq!(list(&d,"approvals").len(),1);assert_eq!(list(&d,"operations").len(),1);
    assert_eq!(d["approvals"][0]["approvedBy"]["id"],"local-owner");
    assert_eq!(d["operations"][0]["dispatchAuthority"]["executed"]["actorId"],"local-owner");
    assert_eq!(d["operations"][0]["action"]["reply"],"Точный сохранённый ответ.");
    assert_eq!(d["conversations"][0]["actionReviews"][0]["status"],"admitted");
    for (_,task) in app.tasks.lock().await.drain(){task.abort();}
}

fn admitted_fixture(d: &mut Value) -> String {
    install_fixture(d);
    let receipt=prepare_one(d);
    confirm_message(d,"Да, выполняй");
    let rid=receipt["reviewId"].as_str().unwrap().to_owned();
    mark(d,"chat",&rid,"admitted",json!({"reviewId":rid,"approvalId":"approval-test","jobId":"execution-job"})).unwrap();
    list_mut(d,"jobs").push(json!({"id":"execution-job","kind":"execute","refId":"approval-test","status":"running"}));
    list_mut(d,"operations").push(json!({"id":"operation-test","approvalId":"approval-test","proposalId":receipt["proposals"][0]["id"],"itemId":"item-1","status":"dispatching"}));
    rid
}

#[tokio::test]
async fn wait_reads_fake_completion_and_persists_exact_outcome_without_dispatch() {
    let (app,_temp)=crate::tests::test_app().await;
    let rid=app.change(|d|Ok(admitted_fixture(d))).await.unwrap();
    let completing=app.clone();
    let done=tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(40)).await;
        completing.change(|d|{
            row_mut(d,"jobs","execution-job")?["status"]=json!("completed");
            row_mut(d,"operations","operation-test")?["status"]=json!("succeeded");Ok(())
        }).await.unwrap();
    });
    let result=await_review_result(app.clone(),&actor(),"chat",&rid,std::time::Duration::from_secs(2)).await.unwrap();
    done.await.unwrap();
    assert_eq!(result["externalOutcome"],"succeeded");assert_eq!(result["counts"]["succeeded"],1);
    assert_eq!(result["timedOut"],false);assert_eq!(result["operations"][0]["id"],"operation-test");
    let d=app.read().await.unwrap();
    let messages=d["conversations"][0]["messages"].as_array().unwrap();
    assert_eq!(messages.iter().filter(|m|m["actionExecution"]["reviewId"]==rid).count(),1);
    assert_eq!(messages.last().unwrap()["actionExecution"],result);
    assert_eq!(list(&d,"operations").len(),1);assert!(list(&d,"approvals").is_empty());
}

#[tokio::test]
async fn timeout_receipt_updates_after_finish_and_reconcile_without_resend_or_duplicate() {
    let (app,_temp)=crate::tests::test_app().await;
    let rid=app.change(|d|Ok(admitted_fixture(d))).await.unwrap();
    let first=await_review_result(app.clone(),&actor(),"chat",&rid,std::time::Duration::ZERO).await.unwrap();
    assert_eq!(first["externalOutcome"],"pending");assert_eq!(first["timedOut"],true);
    let again=await_review_result(app.clone(),&actor(),"chat",&rid,std::time::Duration::ZERO).await.unwrap();
    assert_eq!(first["receiptMessageId"],again["receiptMessageId"]);
    app.change(|d|{
        row_mut(d,"jobs","execution-job")?["status"]=json!("completed");
        row_mut(d,"operations","operation-test")?["status"]=json!("unknown");
        refresh_execution_receipts(d,"execution-job")
    }).await.unwrap();
    let d=app.read().await.unwrap();let message=d["conversations"][0]["messages"].as_array().unwrap().last().unwrap();
    assert_eq!(message["id"],first["receiptMessageId"]);assert_eq!(message["actionExecution"]["externalOutcome"],"unknown");
    assert_eq!(message["actionExecution"]["counts"]["unknown"],1);assert_eq!(message["actionExecution"]["timedOut"],false);
    app.change(|d|{
        list_mut(d,"jobs").push(json!({"id":"reconcile-job","kind":"reconcile","refId":"operation-test","status":"completed"}));
        row_mut(d,"operations","operation-test")?["status"]=json!("succeeded");
        refresh_execution_receipts(d,"reconcile-job")
    }).await.unwrap();
    let d=app.read().await.unwrap();let messages=d["conversations"][0]["messages"].as_array().unwrap();
    assert_eq!(messages.last().unwrap()["actionExecution"]["externalOutcome"],"succeeded");
    assert_eq!(messages.iter().filter(|m|m["actionExecution"]["reviewId"]==rid).count(),1);
    assert_eq!(list(&d,"operations").len(),1);assert!(list(&d,"approvals").is_empty());
}

#[test]
fn terminal_job_does_not_invent_success_for_mixed_or_unresolved_operations() {
    let mut d=fixture();let rid=admitted_fixture(&mut d);
    row_mut(&mut d,"jobs","execution-job").unwrap()["status"]=json!("failed");
    let review=d["conversations"][0]["actionReviews"][0].clone();
    let unresolved=execution_summary(&d,&review,false).unwrap();
    assert_eq!(unresolved["externalOutcome"],"unknown");assert_eq!(unresolved["counts"]["pending"],1);
    assert_eq!(unresolved["requiresReadback"],true);
    let entry=review["proposals"][0].clone();d["conversations"][0]["actionReviews"][0]["proposals"]=json!([entry,entry,entry,entry]);
    d["operations"]=json!([{"id":"a","approvalId":"approval-test","status":"succeeded"},
        {"id":"b","approvalId":"approval-test","status":"failed"},{"id":"c","approvalId":"approval-test","status":"stale"},
        {"id":"d","approvalId":"approval-test","status":"unknown"}]);
    let summary=persist_execution_summary(&mut d,"chat",&rid,false).unwrap();
    assert_eq!(summary["externalOutcome"],"partial");
    assert_eq!(summary["counts"],json!({"succeeded":1,"failed":1,"stale":1,"unknown":1,"pending":0}));
}
