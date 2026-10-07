use super::*;

#[cfg(windows)]
#[tokio::test]
async fn media_bridge_containment_is_empty_before_success() {
    let mut command=Command::new("cmd.exe");
    command.args(["/C","ping -n 30 127.0.0.1 > NUL"])
        .stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null())
        .kill_on_drop(true).creation_flags(0x08000000);
    let mut child=command.spawn().unwrap();
    let tree=ProcessTree::attach(&child).unwrap();
    tree.stop_and_wait().await.unwrap();
    assert!(child.wait().await.is_ok());
    // Idempotent readback, including the no-descendants fast path.
    tree.stop_and_wait().await.unwrap();
}

fn pilot_actor(id: &str) -> operator_auth::Actor {
    operator_auth::Actor { id: id.into(), name: id.into(), role: "operator".into(), csrf_token: "test".into(), authority_generation: Some("a".repeat(64)) }
}

#[test]
fn dispatch_configuration_rejects_invalid_values_and_caps_large_waves() {
    assert_eq!(dispatch_parallelism_value("1").unwrap(),1);
    assert_eq!(dispatch_parallelism_value("4").unwrap(),dispatch_wave::MAX_IN_FLIGHT);
    assert_eq!(dispatch_parallelism_value("72").unwrap(),dispatch_wave::MAX_IN_FLIGHT);
    for value in ["", "0", "-1", "not-a-number"] {
        assert!(dispatch_parallelism_value(value).is_err(),"{value}");
    }
}

#[tokio::test]
async fn publication_requires_explicit_opt_in_except_exact_fake_transport() {
    let (mut app, _temp) = test_app().await;
    app.external_writes = false;
    assert_eq!(app.check_execution().unwrap_err().0, StatusCode::FORBIDDEN);
    app.bridge = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../tests/fake-bridge.mjs");
    assert!(app.check_execution().is_ok());
    app.bridge = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../adapters/bridge.mjs");
    assert_eq!(app.check_execution().unwrap_err().0, StatusCode::FORBIDDEN);
    app.external_writes = true;
    assert!(app.check_execution().is_ok());
}

#[test]
fn publication_approval_belongs_to_reviewer_and_legacy_to_local_owner() {
    let approved = json!({"approvedBy":{"id":"dmitry"}});
    assert!(check_approval_actor(&approved, &pilot_actor("dmitry")).is_ok());
    assert_eq!(check_approval_actor(&approved, &pilot_actor("alexey")).unwrap_err().0, StatusCode::FORBIDDEN);
    assert!(check_approval_actor(&json!({}), &pilot_actor("dmitry")).is_err());
    assert!(check_approval_actor(&json!({}), &operator_auth::Actor::local_owner("test")).is_ok());
}

#[tokio::test]
async fn other_operator_cannot_consume_reviewers_approval() {
    let (app, _temp) = test_app_with_post().await;
    let p = proposal_new(State(app.clone()), Json(json!({"itemId":"item-1","kind":"reply_and_close","text":"Exact reviewed reply","expectedRevision":1}))).await.unwrap().0;
    app.change(|d| { crate::editorial_review::fixture_accept(d,p["id"].as_str().unwrap()).unwrap(); Ok(()) }).await.unwrap();
    let approval = approval_new(State(app.clone()), axum::Extension(pilot_actor("dmitry")), Json(json!({"proposals":[{"id":p["id"],"revision":p["revision"]}]}))).await.unwrap().0;
    assert_eq!(approval["approvedBy"]["id"], "dmitry");
    let reviewed_jobs=app.read().await.unwrap()["jobs"].clone();
    let result = execute(State(app.clone()), axum::Extension(pilot_actor("alexey")), Path(approval["id"].as_str().unwrap().into())).await;
    assert_eq!(result.unwrap_err().0, StatusCode::FORBIDDEN);
    let d = app.read().await.unwrap();
    assert_eq!(d["approvals"][0]["status"], "approved");
    assert!(list(&d,"operations").is_empty());
    assert_eq!(d["jobs"],reviewed_jobs,"Denied execution preserves the actual editorial capture");
}

#[tokio::test]
async fn simultaneous_reviewers_publish_through_one_lane_without_retries() {
    let (mut app, temp) = test_app_with_post().await;
    let access_file = temp.path().join("access.json");
    let operator_tokens = ["dmitry", "alexey"].map(|id| (id, format!("{id}-{}", "test-key".repeat(8))));
    let operators: Vec<Value> = operator_tokens.iter().map(|(id, token)| {
        use sha2::{Digest, Sha256};
        json!({"id":id,"name":id,"tokenHash":format!("{:x}", Sha256::digest(token.as_bytes()))})
    }).collect();
    std::fs::write(&access_file, json!({"operators":operators}).to_string()).unwrap();
    let auth = operator_auth::Auth::open(temp.path(), access_file).await.unwrap();
    let mut actors = HashMap::new();
    for (id, token) in operator_tokens { actors.insert(id, auth.login(&token).await.unwrap().actor); }
    app.auth = Some(auth);
    let log = temp.path().join("fake-effects.jsonl");
    app.node = PathBuf::from("C:/Users/hello/AppData/Local/Microsoft/WinGet/Packages/OpenJS.NodeJS.LTS_Microsoft.Winget.Source_8wekyb3d8bbwe/node-v24.15.0-win-x64/node.exe");
    app.bridge = temp.path().join("isolated-publisher.mjs");
    let script = r#"import {appendFile} from 'node:fs/promises';
let input='';for await(const chunk of process.stdin)input+=chunk;const r=JSON.parse(input);
const target=r.itemId??r.actions[0].itemId;
await appendFile(__LOG__,JSON.stringify({operation:r.operation,target})+'\n');
await new Promise(resolve=>setTimeout(resolve,50));
const result=r.operation==='context'?{itemId:target,objectId:'11391',postKey:'11391:post-1',conversationKey:'11391:'+target,contextEvidenceDigest:'a'.repeat(64)}:{account:r.account,results:r.actions.map(a=>({actionId:a.actionId,itemId:a.itemId,status:'verified'}))};
process.stdout.write(JSON.stringify({ok:true,result}));"#.replace("__LOG__", &json!(log.to_string_lossy()).to_string());
    std::fs::write(&app.bridge, script).unwrap();
    app.change(|d| { connection_gate::fixture_open(d)?;let mut second=fixture();second["id"]=json!("item-2");second["itemId"]=json!("comment-2");second["conversationKey"]=json!("11391:comment-2");list_mut(d,"items").push(second);create_post_fixture(d,"item-2") }).await.unwrap();
    let mut approvals=vec![];
    for (target,actor) in [("item-1","dmitry"),("item-2","alexey")] {
        let p=proposal_new(State(app.clone()),Json(json!({"itemId":target,"kind":"reply_and_close","text":"Reviewed response","expectedRevision":1}))).await.unwrap().0;
        app.change(|d| { crate::editorial_review::fixture_accept(d,p["id"].as_str().unwrap()).unwrap(); Ok(()) }).await.unwrap();
        let a=approval_new(State(app.clone()),axum::Extension(actors[actor].clone()),Json(json!({"proposals":[{"id":p["id"],"revision":p["revision"]}]}))).await.unwrap().0;
        approvals.push(a["id"].as_str().unwrap().to_owned());
    }
    let (a,b)=tokio::join!(execute(State(app.clone()),axum::Extension(actors["dmitry"].clone()),Path(approvals[0].clone())),execute(State(app.clone()),axum::Extension(actors["alexey"].clone()),Path(approvals[1].clone())));
    assert!(a.is_ok() && b.is_ok());
    tokio::time::timeout(Duration::from_secs(15),async { loop {let d=app.read().await.unwrap();if list(&d,"operations").iter().all(|op|op["status"]=="succeeded"){break;}tokio::time::sleep(Duration::from_millis(40)).await;} }).await.unwrap();
    let entries:Vec<Value>=std::fs::read_to_string(&log).unwrap().lines().map(|line|serde_json::from_str(line).unwrap()).collect();
    assert_eq!(entries.len(),6,"one context, one mutation, one readback per action");
    for batch in entries.chunks(3) {
        assert_eq!(batch[0]["operation"],"context");assert_eq!(batch[1]["operation"],"execute");assert_eq!(batch[2]["operation"],"readback");
        assert_eq!(batch[0]["target"],batch[1]["target"]);assert_eq!(batch[1]["target"],batch[2]["target"]);
    }
    assert_ne!(entries[0]["target"],entries[3]["target"]);
    let d=app.read().await.unwrap();
    assert!(list(&d,"operations").iter().all(|op|op["approvedBy"]["id"]==op["executedBy"]["id"]));
    assert!(list(&d,"operations").iter().all(|op|op["dispatchPermit"]["phase"]=="transport_settled"&&connection_gate::valid_permit(op)));
    assert!(execute(State(app.clone()),axum::Extension(pilot_actor("dmitry")),Path(approvals[0].clone())).await.is_err());
}

#[tokio::test]
async fn manual_instruction_api_persists_replays_and_invalidates_prepared_context() {
    let (mut app, temp) = test_app_with_post().await;
    app.change(|d| {
        d["items"][0]["draft"] = json!("");
        d["items"][0]["providerObservedAt"] = json!(now());
        knowledge::sync_catalog(d, &now()).map_err(bad)?;
        let at = chrono::Utc::now().timestamp();
        let (job, _) = auto_prepare::claim(d, at)?.unwrap();
        let mut result=engine_prepare::tests::single_pass_result(json!({"text":"Ready","sources":[],"assessments":[{"itemId":"item-1","outcome":"reply","reason":"Friendly comment"}],"proposals":[{"itemId":"item-1","kind":"reply_and_close","text":"Thank you"}]}));
        let request=row(d,"jobs",&job)?["prepareBundle"]["request"].clone();
        model_material_receipt::fixture_result(d,&job,&request,&mut result)?;
        auto_prepare::complete(d,&job,&result,at)?;
        row_mut(d,"jobs",&job)?["status"] = json!("completed");
        Ok(())
    }).await.unwrap();
    let before = app.read().await.unwrap();
    assert_eq!(before["proposals"][0]["status"], "draft");
    let body = json!({"requestId":"durable-manual","title":"New post rule","text":"Explain the post plainly","postKey":"11391:post-1"});
    let Json(saved) = knowledge_instruction(State(app.clone()), Json(body.clone())).await.unwrap();
    let after = app.read().await.unwrap();
    assert_eq!(after["proposals"][0]["status"], "stale");
    assert_eq!(after["items"][0]["workflow"], "attention");
    assert_eq!(after["operations"], before["operations"]);
    let bundle = prepare_bundle::build(&after, &[json!("item-1")], &[]).unwrap();
    assert!(bundle["request"]["materials"].as_array().unwrap().iter().any(|m| m["knowledgeVersionId"] == saved["version"]["id"]));
    assert!(material_patch(State(app.clone()),Path(saved["material"]["id"].as_str().unwrap().into()),Json(json!({"expectedRevision":1,"title":"Unsafe edit","text":"Bypass"}))).await.is_err());
    app.db.close().await;
    app.db = Database::Sqlite(open_db(&temp.path().join("workspace.sqlite")).await.unwrap());
    app.change(|d| knowledge::sync_catalog(d,&now()).map_err(bad)).await.unwrap();
    let Json(replayed) = knowledge_instruction(State(app.clone()), Json(body)).await.unwrap();
    assert_eq!(replayed["replayed"], true);
    assert_eq!(replayed["version"], saved["version"]);
    assert_eq!(app.read().await.unwrap(), after);
    let Json(catalog) = knowledge_catalog(State(app.clone())).await.unwrap();
    assert!(catalog["versions"].as_array().unwrap().contains(&saved["version"]));
    app.db.close().await;
}

#[tokio::test]
async fn unchanged_item_patch_does_not_invalidate_revision_but_real_edit_does() {
    let (app,_temp)=test_app().await;
    let before=app.read().await.unwrap()["items"][0].clone();
    let key=before["id"].as_str().unwrap().to_owned();
    let Json(unchanged)=item_patch(State(app.clone()),Path(key.clone()),Json(json!({"expectedRevision":before["revision"],"draft":before["draft"],"workflow":before["workflow"]}))).await.unwrap();
    assert_eq!(unchanged["revision"],before["revision"]);
    let Json(changed)=item_patch(State(app.clone()),Path(key),Json(json!({"expectedRevision":before["revision"],"draft":"Actual human edit"}))).await.unwrap();
    assert_eq!(changed["revision"].as_u64().unwrap(),before["revision"].as_u64().unwrap()+1);
    assert_eq!(changed["draft"],"Actual human edit");
    app.db.close().await;
}

#[test]
fn explicit_provider_deletion_leaves_queue_and_preserves_human_draft() {
    let mut d = empty();
    d["items"] = json!([fixture()]);
    let mut incoming = fixture();
    incoming["providerStatus"] = json!("deleted");
    merge_snapshot(&mut d, &json!({"items":[incoming]})).unwrap();
    assert_eq!(d["items"][0]["workflow"], "deleted");
    assert_eq!(d["items"][0]["draft"], "Human draft");
    let revision=d["items"][0]["revision"].clone();
    assert!(create_proposal(&mut d,&json!({"itemId":"item-1","kind":"close","expectedRevision":revision})).is_err());
    merge_snapshot(&mut d, &json!({"items":[fixture()]})).unwrap();
    assert_eq!(d["items"][0]["workflow"], "attention");
    assert_eq!(d["items"][0]["draft"], "Human draft");
}

#[test]
fn assembled_thread_change_invalidates_proposal_without_provider_digest_change() {
    let mut d = empty();
    let mut item = fixture();
    item["branchId"] = json!("branch");
    item["postId"] = json!("post");
    let mut page = json!({"items":[item],"posts":[{"id":"post"}],"branches":[{"id":"branch","postId":"post","messages":[{"id":"message","text":"Original"}]}]});
    merge_snapshot(&mut d, &page).unwrap();
    let revision = d["items"][0]["revision"].clone();
    let p = create_proposal(
        &mut d,
        &json!({"itemId":"item-1","kind":"close","expectedRevision":revision}),
    )
    .unwrap();
    assert!(proposal_current(&d, &p).is_ok());
    merge_snapshot(&mut d, &page).unwrap();
    assert!(
        proposal_current(&d, &p).is_ok(),
        "identical reread must not invalidate approval"
    );
    page["branches"][0]["messages"][0]["text"] = json!("Changed thread context");
    merge_snapshot(&mut d, &page).unwrap();
    assert_eq!(
        d["items"][0]["contextEvidenceDigest"],
        p["contextEvidenceDigest"]
    );
    assert!(proposal_current(&d, &p).is_err());
}

#[test]
fn native_comment_locator_is_navigation_not_changed_reply_evidence() {
    let mut d = empty();
    let mut item = fixture();
    item["branchId"] = json!("branch");
    item["postId"] = json!("post");
    let mut page = json!({"items":[item],"posts":[{"id":"post"}],"branches":[{
        "id":"branch","postId":"post","messages":[{"id":"message","text":"Original"}]}]});
    merge_snapshot(&mut d, &page).unwrap();
    let revision = d["items"][0]["revision"].clone();
    let proposal = create_proposal(&mut d, &json!({"itemId":"item-1","kind":"close","expectedRevision":revision})).unwrap();
    let revision = d["items"][0]["revision"].clone();
    let digest = d["items"][0]["branchContextDigest"].clone();
    for locator in [json!("https://www.youtube.com/watch?v=abcdefghijk&lc=fixture"), Value::Null] {
        page["items"][0]["nativeUrl"] = locator.clone();
        page["branches"][0]["messages"][0]["nativeUrl"] = locator.clone();
        merge_snapshot(&mut d, &page).unwrap();
        assert_eq!(d["items"][0]["nativeUrl"], locator);
        assert_eq!(d["items"][0]["revision"], revision);
        assert_eq!(d["items"][0]["branchContextDigest"], digest);
        assert!(proposal_current(&d, &proposal).is_ok());
    }
    page["branches"][0]["messages"][0]["text"] = json!("Actual changed statement");
    merge_snapshot(&mut d, &page).unwrap();
    assert_ne!(d["items"][0]["branchContextDigest"], digest);
    assert!(proposal_current(&d, &proposal).is_err());
}

#[tokio::test]
async fn sync_resumes_after_failed_page_without_losing_checkpoint_or_duplicating_items() {
    let (mut app, temp) = test_app().await;
    app.node = PathBuf::from(
        "C:/Users/hello/AppData/Local/Microsoft/WinGet/Packages/OpenJS.NodeJS.LTS_Microsoft.Winget.Source_8wekyb3d8bbwe/node-v24.15.0-win-x64/node.exe",
    );
    // Wrapper injects a scenario without global environment mutation across tests.
    let scenario = temp.path().join("scenario.json");
    std::fs::write(&scenario, r#"{"pagination":true,"secondPageFailure":true}"#).unwrap();
    let real_bridge = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../tests/fake-bridge.mjs")
        .canonicalize()
        .unwrap();
    let wrapper = temp.path().join("fake-bridge-wrapper.mjs");
    let url = format!(
        "file:///{}",
        real_bridge
            .to_string_lossy()
            .trim_start_matches(r"\\?\")
            .replace('\\', "/")
    );
    std::fs::write(
        &wrapper,
        format!(
            "process.env.COMMUNITYHERO_TEST_SCENARIO={}; await import({});",
            json!(scenario.to_string_lossy()),
            json!(url)
        ),
    )
    .unwrap();
    app.bridge = wrapper;
    app.change(|d| {
        d["items"] = json!([]);
        Ok(())
    })
    .await
    .unwrap();
    let error = run_sync(app.clone()).await.unwrap_err();
    assert!(error.1.contains("TEST_PAGE_FAILURE"), "{}", error.1);
    let interrupted = app.read().await.unwrap();
    // OPEN pages are staged as one bounded batch. A failed second read cannot
    // commit the first page or advance its durable cursor independently.
    assert!(interrupted["sync"]["scan"]["open"]["cursor"].is_null());
    assert_eq!(interrupted["sync"]["scan"]["open"]["pages"], 0);
    assert!(list(&interrupted,"items").is_empty());
    app.db.close().await;
    app.db = Database::Sqlite(
        open_db(&temp.path().join("workspace.sqlite"))
            .await
            .unwrap(),
    );
    std::fs::write(&scenario, r#"{"pagination":true}"#).unwrap();
    let result = run_sync(app.clone()).await.unwrap();
    assert_eq!(result["partial"], true);
    let checkpoint = app.read().await.unwrap();
    assert_eq!(checkpoint["sync"]["scan"]["open"]["pages"], 2);
    assert_eq!(checkpoint["sync"]["scan"]["closed"]["pages"], 1);
    assert_eq!(checkpoint["sync"]["scan"]["closed"]["cursor"], "fake-second-page");
    // Closed history deliberately advances one page per cycle; the next cycle
    // resumes that cursor rather than rereading or duplicating its first page.
    let result = run_sync(app.clone()).await.unwrap();
    // This legacy fake bridge supplies cursor traversal but no requested-ID
    // accounting. Exhaustion cannot retrospectively certify context coverage.
    assert_eq!(result["partial"], true);
    assert_eq!(result["traversalComplete"], true);
    assert_eq!(result["contextComplete"], false);
    assert_eq!(result["snapshotConsistent"], false);
    let resumed = app.read().await.unwrap();
    assert_eq!(resumed["sync"]["scan"]["open"]["accounting"]["unverifiedPages"], 2);
    assert_eq!(resumed["sync"]["scan"]["closed"]["accounting"]["unverifiedPages"], 2);
    assert_eq!(
        resumed["sync"]["scan"]["id"],
        interrupted["sync"]["scan"]["id"]
    );
    assert_eq!(resumed["items"].as_array().unwrap().len(), 1);
    assert_eq!(resumed["sync"]["scan"]["open"]["pages"], 2);
    assert_eq!(resumed["sync"]["scan"]["closed"]["pages"], 2);
}

#[test]
fn provider_reopen_preserves_draft_and_invalidates_old_revision() {
    let mut d = empty();
    let mut original = fixture();
    original["providerStatus"] = json!("closed");
    original["workflow"] = json!("closed");
    d["items"] = json!([original]);
    merge_snapshot(&mut d, &json!({"items":[fixture()]})).unwrap();
    assert_eq!(d["items"][0]["workflow"], "attention");
    assert_eq!(d["items"][0]["draft"], "Human draft");
    assert_eq!(d["items"][0]["revision"], 2);
    let revision = d["items"][0]["revision"].clone();
    merge_snapshot(&mut d, &json!({"items":[fixture()]})).unwrap();
    assert_eq!(d["items"].as_array().unwrap().len(), 1);
    assert_eq!(d["items"][0]["revision"], revision);
}

#[test]
fn sync_cannot_rebind_an_existing_internal_id() {
    let mut d = empty();
    d["items"] = json!([fixture()]);
    let mut incoming = fixture();
    incoming["objectId"] = json!("other-account");
    assert!(merge_snapshot(&mut d, &json!({"items":[incoming]})).is_err());
}

#[tokio::test]
#[ignore = "requires seeded isolated PG via COMMUNITYHERO_REPOSITORY_TEST_URL"]
async fn postgres_repository_preserves_app_contract() {
    let url = std::env::var("COMMUNITYHERO_REPOSITORY_TEST_URL").unwrap();
    let (mut app, _temp) = test_app().await;
    app.db.close().await;
    app.db = Database::postgres(&url).await.unwrap();
    assert!(
        Database::postgres(&url).await.is_err(),
        "second server cannot own recovery/dispatch"
    );
    let initial = app.read().await.unwrap();
    let aborted: ApiResult<()> = app
        .change(|d| {
            d["items"][0]["draft"] = json!("must roll back");
            Err(conflict("abort"))
        })
        .await;
    assert!(aborted.is_err());
    assert_eq!(app.read().await.unwrap(), initial);
    let revision = initial["items"][0]["revision"].clone();
    let first = item_patch(
        State(app.clone()),
        Path("item-1".into()),
        Json(json!({"expectedRevision":revision,"draft":"first"})),
    );
    let second = item_patch(
        State(app.clone()),
        Path("item-1".into()),
        Json(json!({"expectedRevision":revision,"draft":"second"})),
    );
    let (a, b) = tokio::join!(first, second);
    assert_ne!(a.is_ok(), b.is_ok());
    let before_bad_ref = app.read().await.unwrap();
    assert!(
        app.change(|d| {
            list_mut(d, "proposals")
                .push(json!({"id":"bad-reference","itemId":"missing","status":"draft"}));
            Ok(())
        })
        .await
        .is_err()
    );
    assert_eq!(app.read().await.unwrap(), before_bad_ref);
    assert!(
        { app.external_writes=false; app.check_execution().is_err() },
        "migrated workspace cannot send through real bridge"
    );
    let saved = backup(State(app.clone())).await.unwrap().0;
    let restored: Value =
        serde_json::from_slice(&std::fs::read(saved["path"].as_str().unwrap()).unwrap()).unwrap();
    assert_eq!(restored, before_bad_ref);
    app.db.close().await;
    app.db = Database::postgres(&url).await.unwrap();
    assert_eq!(app.read().await.unwrap(), before_bad_ref);
    app.db.close().await;
}

#[tokio::test]
async fn connector_switch_invalidates_review_and_dispatch() {
    let (app, _temp) = test_app().await;
    let p = proposal_new(
        State(app.clone()),
        Json(json!({"itemId":"item-1","kind":"close","expectedRevision":1})),
    )
    .await
    .unwrap()
    .0;
    let approval = approval_new(
        State(app.clone()),
        axum::Extension(operator_auth::Actor::local_owner("test")),
        Json(json!({"proposals":[{"id":p["id"],"revision":1}]})),
    )
    .await
    .unwrap()
    .0;
    app.change(|d| {
        d["connectorBinding"] = legacy_binding();
        d["connectorBinding"]["revision"] = json!(2);
        Ok(())
    })
    .await
    .unwrap();
    assert!(
        approval_new(
            State(app.clone()),
            axum::Extension(operator_auth::Actor::local_owner("test")),
            Json(json!({"proposals":[{"id":p["id"],"revision":1}]}))
        )
        .await
        .is_err()
    );
    assert!(
        execute(
            State(app.clone()),
            axum::Extension(operator_auth::Actor::local_owner("test")),
            Path(approval["id"].as_str().unwrap().into())
        )
        .await
        .is_err()
    );
    assert!(list(&app.read().await.unwrap(), "operations").is_empty());
}

#[test]
fn resource_retargeting_without_revision_cannot_reuse_review() {
    let mut d = empty();
    list_mut(&mut d, "items").push(fixture());
    let p = create_proposal(
        &mut d,
        &json!({"itemId":"item-1","kind":"close","expectedRevision":1}),
    )
    .unwrap();
    assert!(proposal_current(&d, &p).is_ok());
    d["items"][0]["objectId"] = json!("another-account-object");
    assert!(proposal_current(&d, &p).is_err());
}

#[test]
fn unknown_connections_never_default_to_angryspace() {
    for binding in [Value::Null, json!({}), {
        let mut b = legacy_binding();
        b["connector"] = json!("vk");
        b
    }] {
        let mut d = empty();
        d["connectorBinding"] = binding;
        list_mut(&mut d, "items").push(fixture());
        assert!(
            create_proposal(
                &mut d,
                &json!({"itemId":"item-1","kind":"close","expectedRevision":1})
            )
            .is_err()
        );
        assert!(list(&d, "proposals").is_empty());
    }
}

#[test]
fn readback_uses_original_route_and_legacy_unknown_needs_migration() {
    let binding = ConnectorBinding::from_json(&legacy_binding()).unwrap();
    let op = json!({"target":bound_item(&binding, &fixture()).unwrap()});
    assert_eq!(operation_account(&op).unwrap(), "likeavto");
    assert!(operation_account(&json!({"target":fixture(),"status":"unknown"})).is_err());
    let mut altered = op.clone();
    altered["target"]["connectorBinding"]["providerAccountId"] = json!("another-account");
    assert!(operation_account(&altered).is_err());
}

#[tokio::test]
async fn old_readback_preserves_receipt_without_closing_rebound_item() {
    let (app, _temp) = test_app().await;
    let p = proposal_new(
        State(app.clone()),
        Json(json!({"itemId":"item-1","kind":"close","expectedRevision":1})),
    )
    .await
    .unwrap()
    .0;
    let op = json!({"id":"old-operation","proposalId":p["id"],"itemId":"item-1","target":p["routeTarget"],"status":"unknown"});
    app.change(|d| {
        list_mut(d, "operations").push(op.clone());
        d["connectorBinding"] = legacy_binding();
        d["connectorBinding"]["revision"] = json!(2);
        Ok(())
    })
    .await
    .unwrap();
    set_outcome(&app, &op, "succeeded", json!({"verified":true}))
        .await
        .unwrap();
    let d = app.read().await.unwrap();
    assert_eq!(d["operations"][0]["status"], "succeeded");
    assert_eq!(d["items"][0]["providerStatus"], "new");
    assert_eq!(d["items"][0]["workflow"], "prepared");
}

fn fixture() -> Value {
    json!({"id":"item-1","itemId":"comment-1","objectId":"11391","postKey":"11391:post-1","conversationKey":"11391:comment-1","contextEvidenceDigest":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","providerStatus":"new","revision":1,"workflow":"attention","draft":"Human draft","waitingReason":"","dueAt":null})
}
#[test]
fn moderation_is_explicit_platform_bound_and_has_no_fake_restore() {
    for (platform,kind,allowed) in [("tiktok","hide",true),("tiktok","delete",false),
        ("vk","delete",true),("instagram","delete",true),("youtube","delete",true),
        ("instagram","hide",false),("unknown","delete",false),("vk","restore",false)] {
        let mut data=empty();let mut item=fixture();item["platform"]=json!(platform);data["items"]=json!([item]);
        let result=create_proposal(&mut data,&json!({"itemId":"item-1","kind":kind,"text":"","expectedRevision":1}));
        assert_eq!(result.is_ok(),allowed,"{platform}/{kind}");
        if let Ok(proposal)=result {assert!(proposal_current(&data,&proposal).is_ok());}
    }
}
/// Explicit complete synthetic source for tests that exercise current reply
/// readiness. Call before creating a proposal; the sparse global fixture stays
/// unchanged so missing-source and legacy-contract tests keep their meaning.
pub(crate) fn create_post_fixture(d: &mut Value, item_id: &str) -> ApiResult<()> {
    assert!(!list(d,"proposals").iter().any(|p|p["itemId"]==item_id),
        "Source fixtures must be installed before proposal creation");
    let binding=active_binding(d)?.to_json();
    d["connectorBinding"]=binding.clone();
    let item=row_mut(d,"items",item_id)?;
    item["postId"]=json!("post-1");
    item["connectorBinding"]=binding.clone();
    let post=json!({"id":"post-1","postKey":item["postKey"],"objectId":item["objectId"],
        "text":"Complete synthetic source post","attachments":[],"connectorBinding":binding});
    if let Some(existing)=list(d,"posts").iter().find(|p|p["id"]=="post-1") {
        assert_eq!(existing,&post,"A source fixture must not replace existing evidence");
    } else { list_mut(d,"posts").push(post); }
    Ok(())
}

pub(crate) async fn test_app_with_post() -> (App, tempfile::TempDir) {
    let (app,temp)=test_app().await;
    app.change(|d|create_post_fixture(d,"item-1")).await.unwrap();
    (app,temp)
}

#[test]
fn proposal_creation_origin_rejects_client_markers_without_partial_state() {
    let mut source=empty();source["items"]=json!([fixture()]);
    create_post_fixture(&mut source,"item-1").unwrap();
    for marker in [json!("operator_manual_v1"),json!("model_generation_v1"),Value::Null] {
        for generated in [false,true] {
            let mut d=source.clone();
            let body=json!({"itemId":"item-1","expectedRevision":1,"kind":"reply_and_close",
                "text":"Exact synthetic draft","nativeCreationOrigin":marker});
            let error=if generated {create_generated_proposal(&mut d,&body)}else{create_proposal(&mut d,&body)}.unwrap_err();
            assert_eq!(error.0,StatusCode::BAD_REQUEST);
            assert_eq!(error.1,"Proposal creation origin is server-owned");
            assert_eq!(d,source,"Rejected client origin cannot insert a proposal or bump the recipient");
        }
    }
}

#[test]
fn native_creation_origin_distinguishes_manual_generated_derived_and_recovered_replies() {
    let mut source=empty();
    // The native store normalizes these collections before origin/feedback
    // reducers run. Pure fixtures must represent that initialized workspace.
    for key in ["feedback","knowledge_entries","knowledge_versions"] {source[key]=json!([]);}
    source["items"]=json!([fixture()]);
    create_post_fixture(&mut source,"item-1").unwrap();
    let body=json!({"itemId":"item-1","expectedRevision":1,"kind":"reply_and_close","text":"Exact synthetic draft"});
    let manual=create_proposal(&mut source.clone(),&body).unwrap();
    assert_eq!(manual["nativeCreationOrigin"],"operator_manual_v1");
    assert!(preparation_materials::genuine_manual(&manual));
    let mut d=source.clone();let generated=create_generated_proposal(&mut d,&body).unwrap();
    assert_eq!(generated["nativeCreationOrigin"],"model_generation_v1");
    assert!(!preparation_materials::genuine_manual(&generated));
    let derived_body=json!({"itemId":"item-1","expectedRevision":d["items"][0]["revision"],
        "kind":"reply_and_close","text":"Edited synthetic model draft",
        "sourceProposalId":generated["id"],"sourceProposalRevision":generated["revision"]});
    let derived=create_proposal(&mut d,&derived_body).unwrap();
    assert_eq!(derived["nativeCreationOrigin"],"model_derived_v1");
    assert!(!preparation_materials::genuine_manual(&derived));
    assert_eq!(derived["origin"]["id"],generated["id"]);
    for key in ["origin","priorPreparationOrigin","generationMetadata","prepareRunId","prepareBundleId",
        "prepareBundleDigest","sourceProposalId","sourceProposalRevision",retained_paid_recovery::FIELD] {
        let mut forged=manual.clone();forged[key]=json!("nonmanual-provenance");
        assert!(!preparation_materials::genuine_manual(&forged),"{key}");
    }
    let mut historical=manual;historical.as_object_mut().unwrap().remove("nativeCreationOrigin");
    assert!(!preparation_materials::genuine_manual(&historical),"Missing historical metadata never proves a manual origin");
    let (recovered,_,_,key)=retained_paid_recovery::tests::recovered_fixture();
    let proposal=row(&recovered,"proposals",&key).unwrap();
    assert_eq!(proposal["nativeCreationOrigin"],"retained_model_recovery_v1");
    assert!(!preparation_materials::genuine_manual(proposal));
}

#[test]
fn closed_reply_preserves_unresolved_moderation_and_route_guards() {
    let mut item=fixture();
    item["connectorBinding"]=legacy_binding();
    let original=item.clone();
    item["revision"]=json!(2);
    item["workflow"]=json!("closed");
    item["providerStatus"]=json!("closed");
    let proposal=json!({"itemId":item["id"],"kind":"reply_and_close","allowClosedReply":true});
    let prior=json!({"itemId":item["id"],"status":"succeeded","action":{"action":"close"},"target":original});
    assert!(!recipient_operation_blocks(&prior,&proposal,&item));
    let mut reply=prior.clone();reply["action"]["action"]=json!("reply_and_close");
    assert!(!recipient_operation_blocks(&reply,&proposal,&item));
    for status in ["dispatching","unknown"] {
        let mut unresolved=prior.clone();unresolved["status"]=json!(status);
        assert!(recipient_operation_blocks(&unresolved,&proposal,&item));
    }
    for kind in ["hide","delete"] {
        let mut moderation=prior.clone();moderation["action"]["action"]=json!(kind);
        assert!(recipient_operation_blocks(&moderation,&proposal,&item));
    }
    let mut ordinary=proposal.clone();ordinary["allowClosedReply"]=json!(false);
    assert!(recipient_operation_blocks(&prior,&ordinary,&item));
    let mut unadvanced=item.clone();unadvanced["revision"]=json!(1);
    assert!(recipient_operation_blocks(&prior,&proposal,&unadvanced));
    for key in ["itemId","objectId","postKey","conversationKey"] {
        let mut foreign=prior.clone();foreign["target"][key]=json!("foreign");
        assert!(recipient_operation_blocks(&foreign,&proposal,&item));
    }
    let mut rebound=prior.clone();rebound["target"]["connectorBinding"]["revision"]=json!(2);
    assert!(recipient_operation_blocks(&rebound,&proposal,&item));
    let mut unbound=prior.clone();unbound["target"].as_object_mut().unwrap().remove("connectorBinding");
    assert!(recipient_operation_blocks(&unbound,&proposal,&item));
}
#[test]
fn provider_recipient_alias_blocks_unknown_and_succeeded_effects_without_foreign_scope_collision() {
    let mut selected=fixture();selected["id"]=json!("new-local-row");
    selected["revision"]=json!(9);selected["workflow"]=json!("closed");selected["providerStatus"]=json!("closed");
    selected["connectorBinding"]=legacy_binding();
    let mut old=selected.clone();old["id"]=json!("old-local-row");old["revision"]=json!(1);
    let proposal=json!({"itemId":"new-local-row","kind":"reply_and_close","allowClosedReply":true});
    for status in ["dispatching","unknown","succeeded"] {
        let operation=json!({"itemId":"old-local-row","status":status,"action":{"action":"close"},"target":old});
        assert!(recipient_operation_blocks(&operation,&proposal,&selected),"{status}");
        let mut same_provider_different_connection=operation.clone();
        same_provider_different_connection["target"]["connectorBinding"]["id"]=json!("other-connection");
        assert!(!recipient_operation_blocks(&same_provider_different_connection,&proposal,&selected));
        let mut foreign_account=operation.clone();
        foreign_account["target"]["connectorBinding"]["accountId"]=json!("Other");
        assert!(!recipient_operation_blocks(&foreign_account,&proposal,&selected));
    }
    let mut malformed=json!({"itemId":"old-local-row","status":"unknown","target":old});
    malformed["target"]["connectorBinding"]=json!({});
    assert!(recipient_operation_blocks(&malformed,&proposal,&selected));
    malformed["target"]["itemId"]=json!("unrelated-comment");
    assert!(!recipient_operation_blocks(&malformed,&proposal,&selected));
}

#[test]
fn reply_to_closed_requires_explicit_manual_intent_and_current_evidence() {
    let mut data=empty();let mut item=fixture();item["providerStatus"]=json!("closed");item["workflow"]=json!("closed");data["items"]=json!([item]);
    create_post_fixture(&mut data,"item-1").unwrap();
    let mut body=json!({"itemId":"item-1","kind":"reply_and_close","text":"Reviewed followup","expectedRevision":1});
    assert!(create_proposal(&mut data,&body).is_err());
    body["allowClosedReply"]=json!(true);
    assert!(create_generated_proposal(&mut data,&body).is_err());
    let proposal=create_proposal(&mut data,&body).unwrap();
    editorial_review::fixture_accept(&mut data,proposal["id"].as_str().unwrap()).unwrap();
    let proposal=row(&data,"proposals",proposal["id"].as_str().unwrap()).unwrap().clone();
    let current=proposal_current(&data,&proposal).unwrap();
    assert_eq!(action_for(&proposal,&current,"operation").unwrap()["expectedStatuses"],json!(["closed"]));
    data["items"][0]["revision"]=json!(2);
    assert!(proposal_current(&data,&proposal).is_err());
}
pub(super) async fn test_app() -> (App, tempfile::TempDir) {
    let temp = tempfile::tempdir().unwrap();
    let db = open_db(&temp.path().join("workspace.sqlite"))
        .await
        .unwrap();
    let (events, _) = broadcast::channel(8);
    let app = App {lifecycle_task_count: Default::default(), lifecycle_admission: Arc::new(crate::runtime_lifecycle_startup::Admission::fixture(crate::accounts::Profile::LikeAvto)), lifecycle_owner: Arc::new(crate::runtime_lifecycle_startup::Admission::fixture(crate::accounts::Profile::LikeAvto).identity().clone()), lifecycle_provider_token: Default::default(), lifecycle_work: Default::default(), media_discovery: Default::default(),preparation_wake: Default::default(),provider_session: Default::default(),
        account: crate::accounts::Profile::LikeAvto,navigation:crate::account_navigation::Navigation::root(),
        db: Database::Sqlite(db),
        gate: Arc::new(crate::writer_gate::WriterGate::default()), execution_gate: Arc::new(Mutex::new(())),
        preparation_workers: Default::default(),editorial_gate: Default::default(),assistant_gate: Arc::new(Mutex::new(())),assistant_chat_gate: Arc::new(Mutex::new(())),
        events,
        csrf: id(),
                auth: None,
                public_origin: None,
        external_writes: true,
        port: 4186,
        data: temp.path().to_owned(),
        bridge: temp.path().join("never-execute.mjs"),
        node: temp.path().join("no-runtime"),
        tasks: Arc::new(Mutex::new(HashMap::new())),bootstrap_cache:Arc::new(bootstrap_cache::Cache::default()),
    };
    crate::runtime_lifecycle_app::initialize_app_fixture(&app).await.unwrap();
    app.change(|d| {
        list_mut(d, "items").push(fixture());
        Ok(())
    })
    .await
    .unwrap();
    (app, temp)
}
#[tokio::test]
async fn failed_batch_has_no_partial_state() {
    let (app, _temp) = test_app().await;
    let result: ApiResult<()> = app
        .change(|d| {
            list_mut(d, "items").push(json!({"id":"uncommitted"}));
            Err(conflict("abort"))
        })
        .await;
    assert!(result.is_err());
    assert_eq!(list(&app.read().await.unwrap(), "items").len(), 1);
}
#[tokio::test]
async fn simultaneous_edit_accepts_one_revision() {
    let (app, _temp) = test_app().await;
    let first = item_patch(
        State(app.clone()),
        Path("item-1".into()),
        Json(json!({"expectedRevision":1,"draft":"first"})),
    );
    let second = item_patch(
        State(app.clone()),
        Path("item-1".into()),
        Json(json!({"expectedRevision":1,"draft":"second"})),
    );
    let (a, b) = tokio::join!(first, second);
    assert_ne!(a.is_ok(), b.is_ok());
    assert_eq!(app.read().await.unwrap()["items"][0]["revision"], 2);
}
#[tokio::test]
async fn proposal_edit_invalidates_immutable_approval() {
    let (app, _temp) = test_app_with_post().await;
    let p=proposal_new(State(app.clone()),Json(json!({"itemId":"item-1","kind":"reply_and_close","text":"Exact old text","expectedRevision":1}))).await.unwrap().0;
    app.change(|d| { crate::editorial_review::fixture_accept(d,p["id"].as_str().unwrap()).unwrap(); Ok(()) }).await.unwrap();
    let approval = approval_new(
        State(app.clone()),
        axum::Extension(operator_auth::Actor::local_owner("test")),
        Json(json!({"proposals":[{"id":p["id"],"revision":1}]})),
    )
    .await
    .unwrap()
    .0;
    let _ = proposal_patch(
        State(app.clone()),
        Path(p["id"].as_str().unwrap().into()),
        Json(json!({"expectedRevision":1,"text":"Edited text"})),
    )
    .await
    .unwrap();
    let result = execute(
        State(app.clone()),
        axum::Extension(operator_auth::Actor::local_owner("test")),
        Path(approval["id"].as_str().unwrap().into()),
    )
    .await;
    assert_eq!(result.unwrap_err().0, StatusCode::CONFLICT);
    let d = app.read().await.unwrap();
    assert!(list(&d, "operations").is_empty());
    assert_eq!(
        d["approvals"][0]["proposals"][0]["proposal"]["text"],
        "Exact old text"
    );
    assert_eq!(d["items"][0]["draft"], "Human draft");
    assert_eq!(d["items"][0]["workflow"], "prepared");
}
#[tokio::test]
async fn unknown_target_cannot_be_dispatched_again() {
    let (app, _temp) = test_app().await;
    let p = proposal_new(
        State(app.clone()),
        Json(json!({"itemId":"item-1","kind":"close","text":"","expectedRevision":1})),
    )
    .await
    .unwrap()
    .0;
    let approval = approval_new(
        State(app.clone()),
        axum::Extension(operator_auth::Actor::local_owner("test")),
        Json(json!({"proposals":[{"id":p["id"],"revision":1}]})),
    )
    .await
    .unwrap()
    .0;
    app.change(|d| {
        list_mut(d, "operations")
            .push(json!({"id":"prior-attempt","itemId":"item-1","status":"unknown"}));
        Ok(())
    })
    .await
    .unwrap();
    let result = execute(
        State(app.clone()),
        axum::Extension(operator_auth::Actor::local_owner("test")),
        Path(approval["id"].as_str().unwrap().into()),
    )
    .await;
    assert_eq!(result.unwrap_err().0, StatusCode::CONFLICT);
    assert_eq!(
        app.read().await.unwrap()["approvals"][0]["status"],
        "approved"
    );
}
#[test]
fn recovery_is_unknown_never_retry() {
    let mut d = empty();
    list_mut(&mut d, "jobs").push(json!({"id":"j","kind":"assistant","status":"running"}));
    list_mut(&mut d, "operations").push(json!({"id":"o","status":"dispatching"}));
    list_mut(&mut d, "proposals").push(json!({"id":"p","status":"dispatching"}));
    crate::native_fixture_owner_repair::initialize_workspace(&mut d).unwrap();
    recover(&mut d).unwrap();
    assert_eq!(d["jobs"][0]["status"], "interrupted");
    assert_eq!(d["operations"][0]["status"], "unknown");
    assert_eq!(d["proposals"][0]["status"], "unknown");
}
#[test]
fn readback_requires_exact_identity_and_verified_result() {
    let action = json!({"actionId":"op-1","itemId":"comment-1"});
    for result in [
        json!({"confirmed":true}),
        json!({"results":[{"actionId":"other","itemId":"comment-1","status":"verified"}]}),
        json!({"results":[{"actionId":"op-1","itemId":"other","status":"verified"}]}),
        json!({"results":[{"actionId":"op-1","itemId":"comment-1","status":"unknown"}]}),
    ] {
        assert!(!readback_confirmed(&result, &action));
    }
    assert!(readback_confirmed(
        &json!({"results":[{"actionId":"op-1","itemId":"comment-1","status":"verified"}]}),
        &action
    ));
}
#[test]
fn sync_preserves_wait_and_draft_while_tracking_provider_close() {
    let mut d = empty();
    let mut item = fixture();
    item["workflow"] = json!("waiting");
    item["waitingReason"] = json!("Need stock answer");
    list_mut(&mut d, "items").push(item);
    let mut incoming = fixture();
    incoming["providerStatus"] = json!("closed");
    incoming["draft"] = json!("");
    merge_snapshot(&mut d, &json!({"items":[incoming]})).unwrap();
    assert_eq!(d["items"][0]["workflow"], "waiting");
    assert_eq!(d["items"][0]["providerStatus"], "closed");
    assert_eq!(d["items"][0]["draft"], "Human draft");
    assert_eq!(d["items"][0]["revision"], 2);
}
#[test]
fn import_does_not_overwrite_local_knowledge_edit() {
    let mut d = empty();
    list_mut(&mut d, "materials").push(
        json!({"id":"import-origin","text":"Human correction","locallyEdited":true,"revision":4}),
    );
    merge_materials(&mut d,&json!({"materials":[{"id":"origin","text":"Upstream replacement","title":"Rule","kind":"knowledge"}]})).unwrap();
    assert_eq!(d["materials"][0]["text"], "Human correction");
    assert_eq!(d["materials"][0]["revision"], 4);
    assert_eq!(d["materials"][0]["upstreamChanged"], true);
}
#[tokio::test]
async fn database_survives_reopen() {
    let (app, temp) = test_app().await;
    app.change(|d| {
        d["items"][0]["draft"] = json!("Persist me");
        Ok(())
    })
    .await
    .unwrap();
    app.db.close().await;
    let db = open_db(&temp.path().join("workspace.sqlite"))
        .await
        .unwrap();
    let payload: String = sqlx::query_scalar("SELECT payload FROM workspace WHERE id=1")
        .fetch_one(&db)
        .await
        .unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(&payload).unwrap()["items"][0]["draft"],
        "Persist me"
    );
}

#[test]
fn first_page_refresh_preserves_continuation_and_exhaustion() {
    let mut d = empty();
    let first = json!({"items":[],"cursor":"page-2","hasMore":true,"coverage":"first page","observedCount":10,"scannedCount":10});
    merge_sync_page(&mut d, &first, "open", false).unwrap();
    assert_eq!(d["sync"]["open"]["cursor"], "page-2");
    assert_eq!(d["sync"]["open"]["paginationStarted"], false);
    let continuation = json!({"items":[],"cursor":"page-3","hasMore":true,"coverage":"second page","observedCount":9,"scannedCount":10});
    merge_sync_page(&mut d, &continuation, "open", true).unwrap();
    let mut refresh = first.clone();
    refresh["items"] = json!([fixture()]);
    merge_sync_page(&mut d, &refresh, "open", false).unwrap();
    assert_eq!(d["sync"]["open"]["cursor"], "page-3");
    assert_eq!(d["sync"]["open"]["coverage"], "second page");
    assert_eq!(d["sync"]["open"]["observedCount"], 9);
    assert_eq!(d["sync"]["open"]["firstPage"]["cursor"], "page-2");
    assert_eq!(
        list(&d, "items").len(),
        1,
        "Refresh must still merge newly observed comments"
    );
    let final_page = json!({"items":[],"cursor":null,"hasMore":false,"coverage":"final page"});
    merge_sync_page(&mut d, &final_page, "open", true).unwrap();
    merge_sync_page(&mut d, &first, "open", false).unwrap();
    assert_eq!(d["sync"]["open"]["cursor"], Value::Null);
    assert_eq!(d["sync"]["open"]["hasMore"], false);
    assert_eq!(d["sync"]["open"]["coverage"], "final page");
    merge_sync_page(&mut d, &first, "closed", false).unwrap();
    assert_eq!(d["sync"]["closed"]["cursor"], "page-2");
    assert_eq!(d["sync"]["open"]["hasMore"], false);
}

#[tokio::test]
async fn sync_job_failure_and_recovery_update_visible_status() {
    let (app, _temp) = test_app().await;
    app.change(|d| {
        d["sync"]["status"] = json!("completed");
        d["sync"]["lastSyncedAt"] = json!("prior-success");
        Ok(())
    })
    .await
    .unwrap();
    let job = app.job("sync", "").await.unwrap();
    app.finish(&job, Err(internal("Adapter unavailable"))).await;
    let d = app.read().await.unwrap();
    assert_eq!(d["sync"]["status"], "error");
    assert_eq!(d["sync"]["lastError"], "Adapter unavailable");
    assert_eq!(d["sync"]["lastSyncedAt"], "prior-success");
    assert_eq!(d["jobs"][0]["status"], "failed");
    let retry = app.job("sync", "").await.unwrap();
    app.finish(&retry, Ok(json!({"synced":true}))).await;
    let d = app.read().await.unwrap();
    assert_eq!(d["sync"]["status"], "completed");
    assert!(d["sync"]["lastError"].is_null());
}

#[cfg(windows)]
#[tokio::test]
async fn cancelling_assistant_kills_entire_process_tree() {
    let (mut app, temp) = test_app().await;
    app.node = std::env::var_os("COMMUNITYHERO_TEST_NODE").map(PathBuf::from).unwrap_or_else(|| PathBuf::from(
        "C:/AIDev/Workspaces/repos/Angry.Space.Auto-symphony/data/private/angryspace-conveyor/provider-runtime/bundle/node.exe",
    ));
    assert!(
        app.node.exists(),
        "Existing Node runtime required for containment test"
    );
    let marker = temp.path().join("heartbeat.txt");
    let script = temp.path().join("tree.mjs");
    let child_script = format!(
        "require('node:fs').appendFileSync({},'x');setInterval(()=>require('node:fs').appendFileSync({},'x'),30)",
        json!(marker.to_string_lossy()),
        json!(marker.to_string_lossy())
    );
    let source = format!(
        "import {{spawn}} from 'node:child_process';process.stdin.resume();process.stdin.on('end',()=>{{spawn(process.execPath,['-e',{}],{{stdio:'ignore',windowsHide:true}});setTimeout(()=>{{process.stdout.write(JSON.stringify({{ok:true,result:{{text:'late'}}}}));}},30000);}});",
        json!(child_script)
    );
    std::fs::write(&script, source).unwrap();
    app.bridge = script;
    let job = app.job("assistant", "").await.unwrap();
    let worker = app.clone();
    app.spawn(job.clone(), async move {
        worker.bridge("assistant", json!({})).await
    });
    // Process-tree admission plus Node startup can exceed two seconds on a
    // loaded Windows host. Wait for the real heartbeat before testing cancel;
    // keep the deadline below the fixture's 30-second normal completion.
    for _ in 0..500 {
        if marker.exists() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(
        marker.exists(),
        "Child process must run before cancellation"
    );
    let _ = cancel(State(app.clone()), Path(job)).await.unwrap();
    tokio::time::sleep(Duration::from_millis(150)).await;
    let before = std::fs::metadata(&marker).unwrap().len();
    tokio::time::sleep(Duration::from_millis(150)).await;
    let after = std::fs::metadata(&marker).unwrap().len();
    assert_eq!(before, after, "Descendant must stop on cancellation");
    let d = app.read().await.unwrap();
    assert_eq!(d["jobs"][0]["status"], "cancelled");
}

#[tokio::test]
async fn feedback_origin_clear_replay_and_foreign_source() {
    let (app,_temp)=test_app().await;
    let original=app.read().await.unwrap()["items"][0].clone();
    let key=original["id"].as_str().unwrap().to_owned();
    app.change(|d| {list_mut(d,"proposals").push(json!({"id":"ai-source","itemId":key,"revision":1,"itemRevision":original["revision"],"kind":"reply_and_close","text":"Original AI answer","status":"draft","generationMetadata":{"model":"test"}}));Ok(())}).await.unwrap();
    let edit=json!({"expectedRevision":original["revision"],"draft":"Human answer","draftEdited":true,"sourceProposalId":"ai-source","sourceProposalRevision":1,"draftSessionId":"s1","eventId":"edit-1"});
    let Json(first)=item_patch(State(app.clone()),Path(key.clone()),Json(edit.clone())).await.unwrap();
    let data=app.read().await.unwrap();
    assert_eq!(data["feedback"][0]["before"],original["draft"]); // existing manual draft remains true baseline
    assert_eq!(data["feedback"][0]["origin"]["text"],"Original AI answer");
    let Json(replay)=item_patch(State(app.clone()),Path(key.clone()),Json(edit.clone())).await.unwrap();
    assert_eq!(first,replay);
    let mut collision=edit;collision["draft"]=json!("Different");
    assert!(item_patch(State(app.clone()),Path(key.clone()),Json(collision)).await.is_err());
    let clear=json!({"expectedRevision":first["revision"],"draft":"","draftEdited":true,"sourceProposalId":"ai-source","sourceProposalRevision":1,"draftSessionId":"s1","eventId":"edit-2"});
    let Json(cleared)=item_patch(State(app.clone()),Path(key.clone()),Json(clear)).await.unwrap();
    assert_eq!(cleared["draftEdited"],true);assert_eq!(cleared["draft"],"");
    let data=app.read().await.unwrap();
    assert_eq!(data["feedback"].as_array().unwrap().len(),2);
    assert_eq!(data["feedback"][1]["kind"],"proposal_cleared");
    let mut foreign=json!({"sourceProposalId":"ai-source","sourceProposalRevision":1});
    assert!(feedback::origin(&data,&json!({"id":"other","revision":1}),&foreign).is_err());
    foreign["sourceProposalRevision"]=json!(2);
    assert!(feedback::origin(&data,&cleared,&foreign).is_err());
    app.db.close().await;
}
#[test]
fn feedback_first_edit_uses_proposal_and_rejects_fabricated_execution() {
    let mut d=json!({"account":"LikeAvto","items":[{"id":"i","revision":1,"draft":""}],"proposals":[{"id":"p","itemId":"i","revision":1,"itemRevision":1,"text":"AI","kind":"reply_and_close"}],"feedback":[]});
    let item=d["items"][0].clone();let body=json!({"kind":"proposal_presented","eventId":"shown","itemId":"i","sourceProposalId":"p","sourceProposalRevision":1});
    let e=feedback::client_event(&mut d,&body).unwrap();assert_eq!(e["origin"]["text"],"AI");
    feedback::client_event(&mut d,&body).unwrap();assert_eq!(d["feedback"].as_array().unwrap().len(),1);
    let origin=feedback::origin(&d,&item,&body).unwrap();assert_eq!(origin["text"],"AI");
    let mut forged=body;forged["eventId"]=json!("fake");forged["kind"]=json!("execution_verified");
    assert!(feedback::client_event(&mut d,&forged).is_err());
}

#[tokio::test]
async fn first_ai_edit_then_undo_keeps_one_original_baseline() {
    let (app,_temp)=test_app().await;
    app.change(|d|{d["items"][0]["draft"]=json!("");d["items"][0].as_object_mut().unwrap().remove("draftEdited");
        let item=d["items"][0].clone();list_mut(d,"proposals").push(json!({"id":"original","itemId":item["id"],"revision":1,"itemRevision":item["revision"],"text":"AI original","kind":"reply_and_close","status":"draft"}));Ok(())}).await.unwrap();
    let original=app.read().await.unwrap()["items"][0].clone();let key=original["id"].as_str().unwrap().to_owned();
    let mut body=json!({"expectedRevision":original["revision"],"draft":"AI edited","draftEdited":true,"sourceProposalId":"original","sourceProposalRevision":1,"draftSessionId":"session","eventId":"first"});
    let Json(edited)=item_patch(State(app.clone()),Path(key.clone()),Json(body.clone())).await.unwrap();
    body["expectedRevision"]=edited["revision"].clone();body["draft"]=json!("AI original");body["eventId"]=json!("undo");
    item_patch(State(app.clone()),Path(key),Json(body)).await.unwrap();
    let d=app.read().await.unwrap();assert_eq!(d["feedback"][0]["before"],"AI original");assert_eq!(d["feedback"][1]["after"],"AI original");assert_eq!(d["feedback"][0]["origin"]["text"],d["feedback"][1]["origin"]["text"]);
    app.db.close().await;
}

#[test]
fn feedback_presentation_survives_reconcile_revision_without_history() {
    let mut d=json!({"account":"LikeAvto","items":[{"id":"i","revision":1,"draft":""}],"proposals":[{"id":"p","itemId":"i","revision":1,"itemRevision":1,"text":"Original","kind":"reply_and_close"}],"feedback":[]});
    let body=json!({"kind":"proposal_presented","eventId":"exposure","itemId":"i","sourceProposalId":"p","sourceProposalRevision":1});
    feedback::client_event(&mut d,&body).unwrap();
    d["proposals"][0]["revision"]=json!(2);d["proposals"][0]["status"]=json!("stale");d["items"][0]["revision"]=json!(2);
    let origin=feedback::origin(&d,&d["items"][0],&body).unwrap();
    assert_eq!(origin["revision"],1);assert_eq!(origin["text"],"Original");
    assert!(feedback::origin(&d,&json!({"id":"foreign","revision":2}),&body).is_err());
    d["feedback"]=json!([]);assert!(feedback::origin(&d,&d["items"][0],&body).is_err());
}
#[tokio::test]
async fn repeated_proposal_edits_keep_original_generation() {
    let (app,_temp)=test_app().await;
    app.change(|d|{let item=d["items"][0].clone();list_mut(d,"proposals").push(json!({"id":"direct-ai","itemId":item["id"],"revision":1,"itemRevision":item["revision"],"text":"AI original","kind":"reply_and_close","status":"draft","prepareRunId":"generation"}));Ok(())}).await.unwrap();
    let Json(first)=proposal_patch(State(app.clone()),Path("direct-ai".into()),Json(json!({"expectedRevision":1,"text":"Edit one","eventId":"patch-one"}))).await.unwrap();
    let Json(second)=proposal_patch(State(app.clone()),Path("direct-ai".into()),Json(json!({"expectedRevision":first["revision"],"text":"Edit two","eventId":"patch-two"}))).await.unwrap();
    assert_eq!(second["origin"]["text"],"AI original");assert_eq!(second["origin"]["revision"],1);
    let d=app.read().await.unwrap();assert_eq!(d["feedback"][0]["origin"]["text"],d["feedback"][1]["origin"]["text"]);assert_eq!(d["feedback"][1]["before"],"Edit one");
    app.db.close().await;
}
#[test]
fn manual_descendant_rejects_changed_source_context() {
    let mut d=empty();d["feedback"]=json!([]);d["items"]=json!([fixture()]);
    let item=d["items"][0].clone();let key=item["id"].as_str().unwrap();
    let digest=prepare_bundle::review_fingerprint(&d,key).unwrap();
    d["proposals"]=json!([{"id":"source","itemId":key,"revision":1,"itemRevision":item["revision"],"text":"AI","kind":"reply_and_close","sourceContextDigest":digest}]);
    d["items"][0]["contextEvidenceDigest"]=json!("changed-provider-context");
    let body=json!({"itemId":key,"expectedRevision":item["revision"],"kind":"close","sourceProposalId":"source","sourceProposalRevision":1});
    assert!(create_proposal(&mut d,&body).is_err());
}

#[tokio::test]
async fn cleared_draft_origin_survives_provider_refresh_and_database_reopen() {
    let (app,temp)=test_app().await;
    let origin=json!({"id":"original-generation","revision":1,"text":"Suggested answer","kind":"reply_and_close","itemId":"item-1"});
    app.change(|d|{
        d["items"][0]["draft"]=json!("");d["items"][0]["draftEdited"]=json!(true);
        d["items"][0]["draftOrigin"]=origin.clone();d["items"][0]["draftSessionId"]=json!("operator-session");
        let mut incoming=d["items"][0].clone();
        for field in ["draft","draftEdited","draftOrigin","draftSessionId"] {incoming.as_object_mut().unwrap().remove(field);}
        incoming["providerStatus"]=json!("inprogress");
        merge_snapshot(d,&json!({"items":[incoming]}))?;Ok(())
    }).await.unwrap();
    let stored=app.read().await.unwrap()["items"][0].clone();
    assert_eq!(stored["draft"],"");assert_eq!(stored["draftEdited"],true);
    assert_eq!(stored["draftOrigin"],origin);assert_eq!(stored["draftSessionId"],"operator-session");
    app.db.close().await;
    let db=open_db(&temp.path().join("workspace.sqlite")).await.unwrap();
    let payload:String=sqlx::query_scalar("SELECT payload FROM workspace WHERE id=1").fetch_one(&db).await.unwrap();
    let reopened:Value=serde_json::from_str(&payload).unwrap();
    for field in ["draft","draftEdited","draftOrigin","draftSessionId"] {assert_eq!(reopened["items"][0][field],stored[field]);}
    db.close().await;
}
#[test]
fn provider_cannot_import_operator_lineage_for_new_item() {
    let mut d=empty();let mut item=fixture();item["draftEdited"]=json!(true);item["draftOrigin"]=json!({"id":"forged"});item["draftSessionId"]=json!("forged");
    merge_snapshot(&mut d,&json!({"items":[item]})).unwrap();
    assert_eq!(d["items"][0]["draftEdited"],false);assert!(d["items"][0]["draftOrigin"].is_null());assert!(d["items"][0]["draftSessionId"].is_null());
}

#[tokio::test]
async fn video_prerequisite_blocks_previously_approved_dispatch_without_consuming_approval(){
    let (app,_temp)=test_app_with_post().await;
    let actor=operator_auth::Actor::local_owner("test");
    let p=proposal_new(State(app.clone()),Json(json!({"itemId":"item-1","kind":"reply_and_close","text":"Saved reviewed reply","expectedRevision":1}))).await.unwrap().0;
    app.change(|d| {editorial_review::fixture_accept(d,p["id"].as_str().unwrap()).map_err(bad)?;Ok(())}).await.unwrap();
    let approval=approval_new(State(app.clone()),axum::Extension(actor.clone()),Json(json!({"proposals":[{"id":p["id"],"revision":p["revision"]}]}))).await.unwrap().0;
    app.change(|d|{d["posts"]=json!([{"id":"post-1","postKey":d["items"][0]["postKey"],"attachments":[{"type":"video"}]}]);Ok(())}).await.unwrap();
    let before=app.read().await.unwrap();
    assert!(execute(State(app.clone()),axum::Extension(actor),Path(approval["id"].as_str().unwrap().to_owned())).await.is_err());
    let after=app.read().await.unwrap();
    for collection in ["proposals","approvals","operations","jobs"] {assert_eq!(after[collection],before[collection]);}
    assert_eq!(after["proposals"][0]["text"],"Saved reviewed reply");
}
#[tokio::test]
async fn missing_visual_context_stages_manual_draft_without_ready_admission(){
    let (app,_temp)=test_app().await;
    app.change(|d|{d["posts"]=json!([{"id":"post-1","postKey":d["items"][0]["postKey"],"attachments":[{"type":"video"}]}]);Ok(())}).await.unwrap();
    let before=app.read().await.unwrap();
    let proposal=proposal_new(State(app.clone()),Json(json!({"itemId":"item-1","kind":"reply_and_close","text":"Typed draft","expectedRevision":1}))).await.unwrap().0;
    let after=app.read().await.unwrap();
    assert_eq!(proposal["status"],"draft");assert!(decision_media::enabled(&proposal));
    assert!(proposal.get("editorialReview").is_none());assert!(proposal_current(&after,&proposal).is_err());
    let actor=operator_auth::Actor::local_owner("test");
    assert!(approval_new(State(app.clone()),axum::Extension(actor),Json(json!({"proposals":[{"id":proposal["id"],"revision":proposal["revision"]}]}))).await.is_err());
    let held=app.read().await.unwrap();assert_eq!(held,after);
    for field in ["posts","materials","jobs","approvals","operations"] {assert_eq!(held[field],before[field]);}
    app.db.close().await;
}
