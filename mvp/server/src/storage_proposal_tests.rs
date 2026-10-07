use super::*;

fn fixture() -> Value {
    let mut d = crate::empty();
    normalize(&mut d);
    d["items"] = json!([{"id":"i","itemId":"c","objectId":"o","platform":"VK","postKey":"p",
        "conversationKey":"thread","branchId":"b","postId":"post","revision":1,"draft":"",
        "workflow":"attention","providerStatus":"new"}]);
    let mut other = d["items"][0].clone();
    other["id"] = json!("other"); other["itemId"] = json!("c-other");
    crate::list_mut(&mut d,"items").push(other);
    d["branches"] = json!([{"id":"b","postId":"post","messages":[{"id":"c","text":"Thank you"}],"contextComplete":true}]);
    d["posts"] = json!([{"id":"post","postKey":"p","text":"Post"}]);
    d["conversations"] = json!([{"id":"private","messages":[{"text":"unrelated".repeat(20_000)}]}]);
    d["approvals"] = json!([{"id":"approval-old","status":"approved","proposals":[],"authority":"keep"}]);
    d["audit"] = json!([{"id":"audit-old","action":"other","refId":"i"}]);
    d["operations"] = json!([{"id":"unknown-old","status":"unknown","evidence":{"receipt":"keep"}}]);
    d["jobs"] = json!([{"id":"old-job","kind":"assistant","status":"completed","result":{"text":"unrelated".repeat(20_000)}},
        {"id":"audio-evidence","kind":"media_audio","refId":"another-post","status":"running"}]);
    d
}
fn body() -> Value { json!({"itemId":"i","kind":"close","expectedRevision":1,"eventId":"event-1"}) }

#[test]
fn proposal_projection_preserves_feedback_replay_and_rejects_protected_mutations() {
    let original = fixture();
    let before = project_proposal(&original).unwrap();
    for table in ["approvals","audit","conversations"] { assert!(before.get(table).is_none()); }
    assert!(before.to_string().len()*10 < original.to_string().len());
    assert_eq!(before["operations"],original["operations"]);
    assert_eq!(before["jobs"].as_array().unwrap().len(),1);
    let mut after = before.clone();
    let proposal = crate::create_proposal(&mut after,&body()).unwrap();
    validate_proposal_change(&before,&after,"i").unwrap();
    let mut merged = original.clone();
    merge(&mut merged,&before,&after).unwrap();
    for table in ["approvals","audit","conversations","operations","jobs"] {
        assert_eq!(merged[table],original[table],"{table}");
    }
    assert_eq!(merged["feedback"][0]["proposalId"],proposal["id"]);
    let retry_before = after.clone();
    assert_eq!(crate::create_proposal(&mut after,&body()).unwrap(),proposal);
    assert_eq!(after,retry_before);
    for mutation in ["authority","history","foreign_item","old_feedback","delete_proposal","feedback_without_proposal"] {
        let mut changed = after.clone();
        match mutation {
            "authority" => changed["approvals"] = json!([]),
            "history" => changed["operations"][0]["status"] = json!("failed"),
            "foreign_item" => changed["items"][1]["workflow"] = json!("prepared"),
            "old_feedback" => changed["feedback"][0]["requestHash"] = json!("altered"),
            "delete_proposal" => changed["proposals"] = json!([]),
            _ => crate::list_mut(&mut changed,"feedback").push(json!({"id":"extra","itemId":"i","kind":"action_selected"})),
        }
        assert!(validate_proposal_change(&after,&changed,"i").is_err(),"{mutation}");
    }
}

#[tokio::test]
async fn sqlite_proposal_late_feedback_error_rolls_back_and_event_replay_is_exact() {
    let folder = tempfile::tempdir().unwrap();
    let path = folder.path().join("workspace.sqlite");
    let db = Database::Sqlite(crate::open_db(&path).await.unwrap());
    db.change(|d| { *d=fixture(); Ok(()) }).await.unwrap();
    let runtime=crate::native_fixture_owner_repair::initialize_db(&db).await.unwrap();
    let before = db.read().await.unwrap();
    let mut invalid = body(); invalid["sessionId"] = json!(["invalid after proposal creation"]);
    assert!(db.create_proposal_observed(&invalid,&runtime).await.is_err());
    assert_eq!(db.read().await.unwrap(),before);
    let (proposal,changed) = db.create_proposal_observed(&body(),&runtime).await.unwrap();
    assert!(changed);
    let saved = db.read().await.unwrap();
    let (replayed,changed) = db.create_proposal_observed(&body(),&runtime).await.unwrap();
    assert!(!changed); assert_eq!(replayed,proposal);
    let mut collision = body(); collision["itemId"] = json!("other");
    assert!(db.create_proposal_observed(&collision,&runtime).await.is_err());
    assert_eq!(db.read().await.unwrap(),saved);
    db.close().await;
    let reopened = Database::Sqlite(crate::open_db(&path).await.unwrap());
    let (replayed,changed) = reopened.create_proposal_observed(&body(),&runtime).await.unwrap();
    assert!(!changed); assert_eq!(replayed,proposal);
    assert_eq!(reopened.read().await.unwrap(),saved);
    reopened.close().await;
}

#[tokio::test]
async fn sqlite_proposal_rechecks_observed_origin_against_current_source() {
    let folder = tempfile::tempdir().unwrap();
    let db = Database::Sqlite(crate::open_db(&folder.path().join("workspace.sqlite")).await.unwrap());
    let mut initial = fixture();
    let digest = crate::prepare_bundle::review_fingerprint(&initial,"i").unwrap();
    initial["items"][0]["draftOrigin"] = json!({"id":"source","revision":1,"itemId":"i","sourceContextDigest":digest});
    db.change(|d| { *d=initial; Ok(()) }).await.unwrap();
    db.change(|d| { d["branches"][0]["messages"][0]["text"] = json!("Changed context"); Ok(()) }).await.unwrap();
    let runtime=crate::native_fixture_owner_repair::initialize_db(&db).await.unwrap();
    let before = db.read().await.unwrap();
    assert!(db.create_proposal_observed(&body(),&runtime).await.is_err());
    assert_eq!(db.read().await.unwrap(),before);
    db.close().await;
}

fn normalize_result(value:&mut Value) {
    match value {
        Value::Array(rows) => rows.iter_mut().for_each(normalize_result),
        Value::Object(fields) => for (key,v) in fields {
            if key=="createdAt" { *v=json!("<time>"); } else { normalize_result(v); }
        },
        Value::String(s) if uuid::Uuid::parse_str(s).is_ok() => *s="<id>".into(),
        _=>(),
    }
}

#[tokio::test]
#[ignore = "requires a pristine explicitly isolated communityhero_writer_v51_test_ PostgreSQL fixture"]
async fn postgres_proposal_parity_and_late_feedback_sql_rollback() {
    let db = writer_v51_fixture_db().await;
    db.change(|d| { *d=fixture(); Ok(()) }).await.unwrap();
    let runtime=crate::native_fixture_owner_repair::initialize_db(&db).await.unwrap();
    let baseline = db.read().await.unwrap();
    let Database::Postgres { writer,.. } = &db else { unreachable!() };
    sqlx::query("CREATE FUNCTION pg_temp.writer_v51_reject() RETURNS trigger LANGUAGE plpgsql AS 'BEGIN RAISE EXCEPTION ''synthetic final write rejection''; END;'")
        .execute(writer).await.unwrap();
    sqlx::query("CREATE TRIGGER writer_v51_reject BEFORE INSERT ON communityhero.feedback FOR EACH ROW EXECUTE FUNCTION pg_temp.writer_v51_reject()")
        .execute(writer).await.unwrap();
    let failed = db.create_proposal_observed(&body(),&runtime).await;
    sqlx::query("DROP TRIGGER writer_v51_reject ON communityhero.feedback").execute(writer).await.unwrap();
    assert!(failed.is_err());
    assert_eq!(db.read().await.unwrap(),baseline,"late feedback SQL failure must roll back proposal and workflow");
    let mut expected = baseline.clone();
    crate::create_proposal(&mut expected,&body()).unwrap();
    let started = std::time::Instant::now();
    let (proposal,changed) = db.create_proposal_observed(&body(),&runtime).await.unwrap();
    assert!(changed);
    let elapsed = started.elapsed().as_secs_f64()*1000.0;
    let actual = db.read().await.unwrap();
    let mut normalized = actual.clone();
    normalize_result(&mut normalized); normalize_result(&mut expected);
    assert_eq!(normalized,expected,"full-domain persisted parity");
    let (retry,changed) = db.create_proposal_observed(&body(),&runtime).await.unwrap();
    assert!(!changed); assert_eq!(retry,proposal); assert_eq!(db.read().await.unwrap(),actual);
    let mut collision=body(); collision["itemId"]=json!("other");
    assert!(db.create_proposal_observed(&collision,&runtime).await.is_err());
    assert_eq!(db.read().await.unwrap(),actual);
    eprintln!("WRITER_V51_PG proposal=true lateSqlRollback=true fullDomainParity=true eventReplay=true scopedMs={elapsed:.2}");
    db.close().await;
}
