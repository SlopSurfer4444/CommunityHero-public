use super::*;
use serde_json::json;

fn fixture() -> (Value, Value, String) {
    let mut data = crate::empty();
    normalize(&mut data);
    crate::accounts::initialize(&mut data, crate::accounts::Profile::LikeAvto).unwrap();
    let binding = data["connectorBinding"].clone();
    data["posts"] = json!([{"id":"post","postKey":"post-key","text":"Synthetic text publication"}]);
    data["branches"] = json!([{"id":"branch","postId":"post","messages":[{"id":"message","text":"Synthetic question"}]}]);
    let item = json!({"id":"target","itemId":"external-target","objectId":"object","postId":"post","branchId":"branch",
        "postKey":"post-key","conversationKey":"thread","connectorBinding":binding,"platform":"vk","text":"Synthetic question",
        "revision":1,"workflow":"attention","providerStatus":"new","contextEvidenceDigest":"a".repeat(64),"branchContextDigest":"b".repeat(64)});
    let mut other = item.clone();
    other["id"] = json!("other"); other["itemId"] = json!("external-other");
    other["conversationKey"] = json!("other-thread"); other["branchId"] = Value::Null;
    data["items"] = json!([item.clone(), other]);
    crate::knowledge::sync_catalog(&mut data, &crate::now()).unwrap();
    data["proposals"] = json!([{"id":"old-proposal","itemId":"target","status":"failed","revision":1,"kind":"reply_and_close"}]);
    data["approvals"] = json!([{"id":"old-approval","proposals":[{"id":"old-proposal","revision":1}]}]);
    data["operations"] = json!([{"id":"old-reply","itemId":"target","proposalId":"old-proposal","approvalId":"old-approval","status":"unknown",
        "target":item,"action":{"action":"reply_and_close","actionId":"old-reply","objectId":"object","itemId":"external-target","conversationKey":"thread"},
        "executeReceipt":{"mutationOutcome":"uncertain","immutableReceipt":"original"},"evidence":{"verificationPhase":"unconfirmed"}}]);
    let actor = crate::operator_auth::Actor::local_owner("synthetic-bounded-owner");
    let proposal = crate::create_proposal(&mut data, &json!({"itemId":"target","expectedRevision":1,"kind":"close","text":"",
        "closePreserveUnknownReplies":["old-reply"],"_verifiedActor":actor.public_json()})).unwrap();
    let id = proposal["id"].as_str().unwrap().to_owned();
    let approval = crate::create_approval(&mut data, &actor, &json!({"requestId":"bounded-owner-approval",
        "admissionMode":"partial","proposals":[{"id":id,"revision":proposal["revision"]}]})).unwrap();
    assert_eq!(approval["accepted"].as_array().unwrap().len(), 1);
    let approval_id = approval["id"].as_str().unwrap().to_owned();
    let (_, scheduled) = crate::execute_admission::admit(&mut data, &actor, &approval_id,
        &json!({"approvalId":approval_id,"requestId":"bounded-owner-execute"})).unwrap();
    let (_, admitted) = scheduled.unwrap();
    assert_eq!(admitted.len(), 1);
    let own = admitted[0].clone();
    validate(&data).unwrap();
    assert!(crate::dispatch_diagnostics::local_check(&data, &own).is_ok());
    (data, own, id)
}

fn verdict(data: &Value, own: &Value) -> bool {
    crate::dispatch_diagnostics::local_check(data, own).is_ok()
}

fn close_scope(data: &Value, own: &Value, id: &str) -> bool {
    crate::preparation_reservations::assert_operator_close_for_operation(data,
        crate::row(data, "proposals", id).unwrap(), own).is_ok()
}

async fn view(db: &Database, data: &Value, id: &str) -> Value {
    // Each case submits the complete evolving fixture. Check the exact same
    // stable-ID prefix required by production storage before invoking its writer.
    let previous = db.read().await.unwrap();
    for table in TABLES {
        let old = rows(&previous, table).unwrap();
        let new = rows(data, table).unwrap();
        assert!(new.len() >= old.len(), "fixture removed history from {table}");
        assert!(old.iter().zip(new).all(|(a,b)| a["id"] == b["id"]),
            "fixture reordered history in {table}");
    }
    db.change(|stored| {*stored = data.clone(); Ok(())}).await.unwrap();
    let before = db.read().await.unwrap();
    let result = db.read_dispatch_context(id).await.unwrap();
    assert_eq!(db.read().await.unwrap(), before, "dispatch snapshot cannot mutate or replace canonical history");
    result
}

#[test]
fn malformed_preserved_reference_set_disables_receipt_projection() {
    let proposal = |operations: Value| json!({"operatorCloseDecision":{"preservedUnknownReplies":{"operations":operations}}});
    for records in [json!([]), json!([{"operationId":"duplicate"},{"operationId":"duplicate"}]),
        json!([{"operationId":null}]), json!([{"operationId":"bad/id"}]), json!("not-an-array")] {
        assert!(protected_operations(&proposal(records)).is_none());
    }
    assert_eq!(protected_operations(&json!({"operatorCloseDecision":{"preservedUnknownReplies":null}})), Some(vec![]));
    assert_eq!(protected_operations(&proposal(json!([{"operationId":"old-reply"}]))), Some(vec!["old-reply".to_owned()]));
}

/// ROOT must supply a NEW pristine disposable clone. This test does not call a
/// provider, model or HTTP sender. The fixture helper refuses live database
/// names and requires an empty canonical history before any synthetic write.
#[tokio::test]
#[ignore = "requires a pristine explicitly isolated communityhero_writer_v51_test_ PostgreSQL fixture"]
async fn postgres_bounded_owner_close_connected_receipt_control_and_source_parity() {
    let db = crate::storage::preparation::writer_v51_fixture_db().await;
    let (base, own, id) = fixture();
    let baseline = view(&db, &base, &id).await;
    assert_eq!(verdict(&baseline, &own), verdict(&base, &own));
    for operation in ["old-reply", own["id"].as_str().unwrap()] {
        assert_eq!(crate::row(&baseline, "operations", operation).unwrap(), crate::row(&base, "operations", operation).unwrap());
    }
    assert_eq!(baseline["activeExternalJobs"]["complete"], true);
    for table in ["posts","branches","items","materials","knowledge_entries","knowledge_versions"] {
        assert_eq!(baseline[table], base[table], "complete {table} authority must remain current");
    }

    // Execute the production jobs predicate against retained media rows even
    // when no external job is active. It must keep the full media closure,
    // omit unrelated terminal jobs, and preserve the full-reader verdict.
    let mut retained_jobs = base.clone();
    retained_jobs["jobs"].as_array_mut().unwrap().extend(json!([
        {"id":"retained-media","kind":"media","status":"completed",
            "result":{"retainedSourceProof":{"opaque":"MEDIA_HISTORY_MUST_REMAIN_FULL"}}},
        {"id":"retained-audio","kind":"media_audio","status":"completed",
            "result":{"retainedReceipt":{"opaque":"AUDIO_HISTORY_MUST_REMAIN_FULL"}}},
        {"id":"unrelated-terminal-execute","kind":"execute","status":"completed"},
        {"id":"unrelated-terminal-other","kind":"synthetic_other","status":"completed"}
    ]).as_array().unwrap().iter().cloned());
    let retained_view = view(&db, &retained_jobs, &id).await;
    let mut expected_jobs = baseline["jobs"].as_array().unwrap().clone();
    for job_id in ["retained-media", "retained-audio"] {
        expected_jobs.push(crate::row(&retained_jobs, "jobs", job_id).unwrap().clone());
    }
    assert_eq!(retained_view["jobs"], json!(expected_jobs));
    for job in base["jobs"].as_array().unwrap() {
        assert_eq!(crate::row(&retained_jobs, "jobs", job["id"].as_str().unwrap()).unwrap(), job);
    }
    assert_eq!(verdict(&retained_view, &own), verdict(&retained_jobs, &own));
    assert_eq!(retained_view["activeExternalJobs"], baseline["activeExternalJobs"]);

    let mut cold = retained_jobs.clone();
    cold["operations"].as_array_mut().unwrap().push(json!({"id":"cold","itemId":"other","status":"failed","prepareRunId":"retained-paid-owner",
        "account":base["account"],"target":base["items"][1],"action":{"action":"close","actionId":"cold"},
        "executeReceipt":{"unrelated":"COLD_RECEIPT_NOT_AUTHORITY".repeat(100000)},"evidence":{"requiresReadback":false,"unrelated":"COLD_DIAGNOSTIC_NOT_AUTHORITY".repeat(100000)}}));
    let compact = view(&db, &cold, &id).await;
    assert_eq!(verdict(&compact, &own), verdict(&cold, &own));
    let cold_control = crate::row(&compact, "operations", "cold").unwrap();
    assert_eq!(cold_control["prepareRunId"], "retained-paid-owner");
    assert_eq!(cold_control["target"], cold["operations"][2]["target"]);
    assert_eq!(cold_control["evidence"], json!({"requiresReadback":false}));
    assert!(cold_control.get("executeReceipt").is_none());
    assert!(!compact.to_string().contains("COLD_RECEIPT_NOT_AUTHORITY"));
    assert!(!compact.to_string().contains("COLD_DIAGNOSTIC_NOT_AUTHORITY"));
    // Four concurrent production reader calls exercise the same acquisition
    // lane. This proves snapshot/guard behavior, not a throughput improvement.
    let (a,b,c,d) = tokio::join!(db.read_dispatch_context(&id),db.read_dispatch_context(&id),
        db.read_dispatch_context(&id),db.read_dispatch_context(&id));
    for concurrent in [a,b,c,d] {
        let concurrent = concurrent.unwrap();
        assert_eq!(verdict(&concurrent, &own), verdict(&cold, &own));
        assert!(!concurrent.to_string().contains("COLD_RECEIPT_NOT_AUTHORITY"));
    }

    // Current local aliases supplement the immutable saved target. Terminal
    // status does not release a recipient while requiresReadback remains true.
    let mut alias = cold.clone();
    alias["items"][1]["itemId"] = alias["items"][0]["itemId"].clone();
    alias["items"][1]["conversationKey"] = alias["items"][0]["conversationKey"].clone();
    alias["operations"].as_array_mut().unwrap().push(json!({"id":"late-alias","itemId":"other","status":"failed",
        "target":{"id":"other","objectId":"old-object","itemId":"old-recipient","connectorBinding":base["connectorBinding"]},
        "action":{"action":"close","actionId":"late-alias"},"evidence":{"requiresReadback":true}}));
    let projected = view(&db, &alias, &id).await;
    assert!(!verdict(&alias, &own)); assert_eq!(verdict(&projected, &own), verdict(&alias, &own));
    assert_eq!(crate::row(&projected, "operations", "late-alias").unwrap()["evidence"]["requiresReadback"], true);
    assert!(!close_scope(&alias, &own, &id));
    assert_eq!(close_scope(&projected, &own, &id), close_scope(&alias, &own, &id));
    crate::row_mut(&mut alias, "operations", "late-alias").unwrap()["evidence"]["requiresReadback"] = json!(false);
    let released = view(&db, &alias, &id).await;
    assert!(close_scope(&alias, &own, &id));
    assert_eq!(close_scope(&released, &own, &id), close_scope(&alias, &own, &id));

    // Preserve explicit company declarations. A BAW operation does not become
    // a Like recipient blocker because its cold receipt was projected.
    let mut foreign = alias.clone();
    // Restore only the synthetic current alias; retain every operation row.
    foreign["items"][1] = base["items"][1].clone();
    foreign["operations"].as_array_mut().unwrap().push(json!({"id":"foreign","itemId":"other","status":"unknown",
        "account":crate::accounts::Profile::BawRussia.display(),"accountId":crate::accounts::Profile::BawRussia.display(),
        "target":{"objectId":"object","itemId":"external-target","connectorBinding":crate::accounts::Profile::BawRussia.binding()},
        "action":{"action":"reply_and_close"},"evidence":{"requiresReadback":true}}));
    let projected = view(&db, &foreign, &id).await;
    assert!(close_scope(&foreign, &own, &id));
    assert_eq!(close_scope(&projected, &own, &id), close_scope(&foreign, &own, &id));
    assert_eq!(crate::row(&projected, "operations", "foreign").unwrap()["account"], crate::row(&foreign, "operations", "foreign").unwrap()["account"]);

    // Any late account-wide native writer remains visible, including a target
    // outside the selected proposal. The original UNKNOWN is never resolved.
    let mut late = foreign.clone();
    for job in [json!({"id":"late-reconcile","kind":"reconcile","status":"queued","refId":"old-reply"}),
        json!({"id":"other-execute","kind":"execute","status":"running","refId":"unrelated-approval"})] {
        let job_id = job["id"].as_str().unwrap().to_owned();
        late["jobs"].as_array_mut().unwrap().push(job.clone());
        let projected = view(&db, &late, &id).await;
        assert!(!verdict(&late, &own)); assert_eq!(verdict(&projected, &own), verdict(&late, &own));
        let mut expected_controls = baseline["activeExternalJobs"]["jobs"].as_array().unwrap().clone();
        expected_controls.push(job);
        assert_eq!(projected["activeExternalJobs"]["jobs"], json!(expected_controls));
        // Complete only the synthetic late job before the next independent case.
        // The original UNKNOWN operation and original execute job stay intact.
        crate::row_mut(&mut late, "jobs", &job_id).unwrap()["status"] = json!("completed");
        let settled = view(&db, &late, &id).await;
        assert!(verdict(&late, &own));
        assert_eq!(verdict(&settled, &own), verdict(&late, &own));
        assert_eq!(settled["activeExternalJobs"], baseline["activeExternalJobs"]);
    }

    // Exact preserved receipt changes are detected, not reconstructed from
    // selected fields or repaired by the reader.
    let mut changed = late.clone(); changed["operations"][0]["executeReceipt"]["immutableReceipt"] = json!("changed");
    let projected = view(&db, &changed, &id).await;
    assert!(!verdict(&changed, &own)); assert_eq!(verdict(&projected, &own), verdict(&changed, &own));

    // Restore the intentionally corrupted synthetic receipt so subsequent
    // negative cases cannot pass merely because this earlier case still blocks.
    changed["operations"][0]["executeReceipt"] = base["operations"][0]["executeReceipt"].clone();
    let restored = view(&db, &changed, &id).await;
    assert!(verdict(&changed, &own));
    assert_eq!(verdict(&restored, &own), verdict(&changed, &own));

    // A paid-recovery marker is an earlier, separately admitted route. Even
    // explicit null presence declines this bounded reader.
    let mut paid = changed.clone();
    crate::row_mut(&mut paid, "proposals", &id).unwrap()["retainedPaidRecovery"] = Value::Null;
    let before_graft = db.read().await.unwrap();
    assert!(db.change(|stored| {*stored = paid; Ok(())}).await.is_err(),
        "ordinary writers cannot graft even a null recovery proof");
    assert_eq!(db.read().await.unwrap(), before_graft);
    // Malformed persisted marker routing is tested only in this pristine,
    // explicitly isolated fixture. No production writer bypass is introduced.
    let Database::Postgres { writer, .. } = &db else { unreachable!() };
    assert_eq!(sqlx::query("UPDATE communityhero.proposals SET payload=jsonb_set(payload,'{retainedPaidRecovery}','null'::jsonb) WHERE workspace_id=$1 AND id=$2")
        .bind(WORKSPACE).bind(&id).execute(writer).await.unwrap().rows_affected(), 1);
    assert!(db.read_bounded_owner_close(&id).await.unwrap().is_none());
    assert_eq!(db.read_dispatch_context(&id).await.unwrap(), db.read().await.unwrap(),
        "paid marker presence must route to the complete recovery snapshot");
    assert_eq!(sqlx::query("UPDATE communityhero.proposals SET payload=payload-'retainedPaidRecovery' WHERE workspace_id=$1 AND id=$2")
        .bind(WORKSPACE).bind(&id).execute(writer).await.unwrap().rows_affected(), 1);
    assert_eq!(db.read().await.unwrap(), before_graft);

    // A second operation for the exact proposal must stay full; choosing one
    // own row must never exempt a competing close.
    let mut competing = changed.clone();
    let mut second = own.clone(); second["id"] = json!("other-own"); second["action"]["actionId"] = json!("other-own");
    second["status"] = json!("unknown"); second["executeReceipt"] = json!({"untouched":"second-own-receipt"});
    competing["operations"].as_array_mut().unwrap().push(second.clone());
    let projected = view(&db, &competing, &id).await;
    assert_eq!(crate::row(&projected, "operations", "other-own").unwrap(), &second);
    assert!(!verdict(&competing, &own)); assert_eq!(verdict(&projected, &own), verdict(&competing, &own));

    // Corrupt relational routing in this disposable fixture only. Both the
    // previous full reader and the new projection must quarantine the row.
    // The cold row already exists in the complete history; never reset a snapshot.
    let Database::Postgres { writer, .. } = &db else { unreachable!() };
    sqlx::query("UPDATE communityhero.operations SET status='unknown' WHERE workspace_id=$1 AND id='cold'")
        .bind(WORKSPACE).execute(writer).await.unwrap();
    assert!(db.read_dispatch_context(&id).await.is_err());
    assert!(db.read_operator_editorial(&json!({"proposals":[{"id":id}]})).await.is_err());
    db.close().await;
}
