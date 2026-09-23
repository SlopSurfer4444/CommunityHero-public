use super::*;

async fn sqlite() -> (Database, tempfile::TempDir) {
    let folder = tempfile::tempdir().unwrap();
    let pool = crate::open_db(&folder.path().join("workspace.sqlite"))
        .await
        .unwrap();
    (Database::Sqlite(pool), folder)
}

fn seed(d: &mut Value) {
    d["jobs"] = json!([
        {"id":"old","kind":"prepare","status":"completed","refId":"history","prepareBundle":{"request":"large evidence"}},
        {"id":"running","kind":"sync","status":"running","refId":""}
    ]);
    d["items"] = json!([
        {"id":"one","objectId":"11391","itemId":"42","providerStatus":"new","workflow":"prepared","revision":3,"draft":"human draft","draftEdited":true},
        {"id":"two","objectId":"11391","itemId":"43","draft":"untouched"}
    ]);
    d["audit"] = json!([{"id":"audit-existing","action":"existing","refId":"one"}]);
}

async fn exercise(db: &Database) {
    let baseline = db.read().await.unwrap();
    let source = db.read_source_status().await.unwrap();
    assert_eq!(
        source["items"][0]["objectId"],
        baseline["items"][0]["objectId"]
    );
    assert!(source["items"][0].get("draft").is_none());
    assert!(source["items"][0].get("connectorBinding").is_none());
    assert_eq!(source["jobs"].as_array().unwrap().len(), 1);
    assert!(
        db.change_source_claim_observed(|d| {
            d["items"][0]["workflow"] = json!("closed");
            Ok(())
        })
        .await
        .is_err()
    );
    assert_eq!(db.read().await.unwrap(), baseline);
    let (_, changed) = db
        .change_job_observed("running", |d| {
            assert_eq!(rows(d, "jobs")?.len(), 1);
            assert!(rows(d, "items")?.is_empty());
            assert!(rows(d, "audit")?.is_empty());
            d["jobs"][0]["status"] = json!("completed");
            d["jobs"][0]["result"] = json!({"once":true});
            d["sync"]["status"] = json!("completed");
            Ok(())
        })
        .await
        .unwrap();
    assert!(changed);
    let saved = db.read().await.unwrap();
    assert_eq!(saved["jobs"][0], baseline["jobs"][0]);
    assert_eq!(saved["audit"], baseline["audit"]);
    assert_eq!(saved["items"], baseline["items"]);
    let (_, changed) = db.change_job_observed("running", |_| Ok(())).await.unwrap();
    assert!(!changed);
    let (_, changed) = db
        .change_schedule_observed(|d| {
            assert!(rows(d, "jobs")?.is_empty());
            crate::new_job(d, "sync", "")?;
            d["sync"]["claim"] = json!("owner");
            Ok(())
        })
        .await
        .unwrap();
    assert!(changed);
    assert!(
        db.change_schedule_observed(|d| crate::new_job(d, "sync", ""))
            .await
            .is_err()
    );
    let before = db.read().await.unwrap();
    assert!(
        db.change_job_observed("running", |d| {
            d["jobs"][0]["status"] = json!("failed");
            d["sync"]["status"] = json!("error");
            d["audit"]
                .as_array_mut()
                .unwrap()
                .push(json!({"id":"forbidden"}));
            Ok(())
        })
        .await
        .is_err()
    );
    assert_eq!(db.read().await.unwrap(), before);
    let routes = [json!({"objectId":"11391","itemId":"42"})];
    let (_, changed) = db
        .change_status_observed(&routes, |d| {
            assert_eq!(rows(d, "items")?.len(), 1);
            assert!(rows(d, "jobs")?.is_empty());
            d["items"][0]["providerStatus"] = json!("closed");
            d["items"][0]["workflow"] = json!("closed");
            d["items"][0]["revision"] = json!(4);
            d["items"][0]["statusObservedAt"] = json!("2026-09-22T12:00:00Z");
            d["sync"]["pendingContext"] = json!({"new":{"status":"queued"}});
            Ok(())
        })
        .await
        .unwrap();
    assert!(changed);
    let after = db.read().await.unwrap();
    assert_eq!(after["items"][0]["draft"], "human draft");
    assert_eq!(after["items"][1], baseline["items"][1]);
    assert_eq!(after["items"][0]["revision"], 4);
    for field in ["draft", "id", "objectId", "postId"] {
        assert!(
            db.change_status_observed(&routes, |d| {
                d["items"][0][field] = json!("forbidden");
                Ok(())
            })
            .await
            .is_err()
        );
        assert_eq!(db.read().await.unwrap(), after);
    }
    assert!(
        db.change_schedule_observed(|d| {
            d["jobs"]
                .as_array_mut()
                .unwrap()
                .push(json!({"id":"old","status":"running"}));
            Ok(())
        })
        .await
        .is_err()
    );
    assert_eq!(db.read().await.unwrap(), after);
}

#[tokio::test]
async fn scoped_writes_preserve_history_claim_atomicity_drafts_and_rollback() {
    let (db, _folder) = sqlite().await;
    db.change(|d| {
        seed(d);
        Ok(())
    })
    .await
    .unwrap();
    exercise(&db).await;
}

#[tokio::test]
async fn rejected_or_unchanged_scope_never_writes() {
    let (db, _folder) = sqlite().await;
    db.change(|d| {
        seed(d);
        Ok(())
    })
    .await
    .unwrap();
    let Database::Sqlite(pool) = &db else {
        unreachable!()
    };
    sqlx::query("CREATE TRIGGER reject_write BEFORE UPDATE ON workspace BEGIN SELECT RAISE(ABORT,'write attempted'); END").execute(pool).await.unwrap();
    assert!(
        !db.change_job_observed("running", |_| Ok(()))
            .await
            .unwrap()
            .1
    );
    assert!(!db.change_status_observed(&[], |_| Ok(())).await.unwrap().1);
    let error = db
        .change_job_observed("running", |d| {
            d["settings"]["protected"] = json!(true);
            Ok(())
        })
        .await
        .unwrap_err();
    assert!(error.1.contains("read-only"));
    assert!(
        db.change_status_observed(&vec![json!({"objectId":"x","itemId":"y"}); 801], |_| Ok(()))
            .await
            .is_err()
    );
}

#[tokio::test]
async fn failed_job_metadata_commit_rolls_back_both() {
    let (db, _folder) = sqlite().await;
    db.change(|d| {
        seed(d);
        Ok(())
    })
    .await
    .unwrap();
    let before = db.read().await.unwrap();
    let Database::Sqlite(pool) = &db else {
        unreachable!()
    };
    sqlx::query("CREATE TRIGGER reject_write BEFORE UPDATE ON workspace BEGIN SELECT RAISE(ABORT,'write attempted'); END").execute(pool).await.unwrap();
    assert!(
        db.change_job_observed("running", |d| {
            d["jobs"][0]["status"] = json!("completed");
            d["sync"]["status"] = json!("completed");
            Ok(())
        })
        .await
        .is_err()
    );
    assert_eq!(db.read().await.unwrap(), before);
}

/// Writes only to an explicitly named disposable clone. Probe jobs/clocks remain
/// in that clone as evidence. Root owns its creation and retirement; this test
/// never accepts the production DB name.
#[tokio::test]
#[ignore = "requires COMMUNITYHERO_WRITE_TEST_URL for isolated bounded_write_test clone"]
async fn postgres_bounded_write_clone_probe() {
    let url = std::env::var("COMMUNITYHERO_WRITE_TEST_URL").expect("explicit isolated clone URL");
    let pool = PgPoolOptions::new()
        .max_connections(1)
        .connect(&url)
        .await
        .unwrap();
    let database: String = sqlx::query_scalar("SELECT current_database()")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert!(
        database.contains("bounded_write_test"),
        "refusing non-test database"
    );
    crate::db_guards::require_schema(&pool).await.unwrap();
    pool.close().await;
    // Do not delete immutable history from the clone. Use unique jobs and its
    // existing source routes, then prove unchanged history by targeted reads.
    let db = Database::postgres(&url).await.unwrap();
    let history = db.read().await.unwrap();
    let projected = db.read_source_status().await.unwrap();
    assert!(
        projected["items"]
            .as_array()
            .unwrap()
            .iter()
            .all(|i| i.get("draft").is_none() && i.get("text").is_none())
    );
    assert_eq!(
        projected["items"].as_array().unwrap().len(),
        history["items"].as_array().unwrap().len()
    );
    let started = std::time::Instant::now();
    let (key, changed) = db
        .change_schedule_observed(|d| crate::new_job(d, "bounded_write_probe", ""))
        .await
        .unwrap();
    let claim_ms = started.elapsed().as_secs_f64() * 1000.0;
    assert!(changed);
    let started = std::time::Instant::now();
    let (_, changed) = db
        .change_job_observed(&key, |d| {
            assert_eq!(rows(d, "jobs")?.len(), 1);
            d["jobs"][0]["status"] = json!("completed");
            d["jobs"][0]["result"] = json!({"probe":true});
            Ok(())
        })
        .await
        .unwrap();
    let finish_ms = started.elapsed().as_secs_f64() * 1000.0;
    assert!(changed);
    let routes = history["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|i| i["objectId"].is_string() && i["itemId"].is_string())
        .take(1)
        .map(|i| json!({"objectId":i["objectId"],"itemId":i["itemId"]}))
        .collect::<Vec<_>>();
    let started = std::time::Instant::now();
    let observed_at = crate::now();
    let (_, changed) = db
        .change_status_observed(&routes, |d| {
            for item in d["items"].as_array_mut().unwrap() {
                item["statusObservedAt"] = json!(observed_at);
            }
            Ok(())
        })
        .await
        .unwrap();
    let status_ms = started.elapsed().as_secs_f64() * 1000.0;
    assert!(changed || routes.is_empty());
    let saved = db.read().await.unwrap();
    let mut expected_items = history["items"].clone();
    for item in expected_items.as_array_mut().unwrap() {
        if routes.iter().any(|r|r["objectId"]==item["objectId"]&&r["itemId"]==item["itemId"]) {
            item["statusObservedAt"] = json!(observed_at);
        }
    }
    assert_eq!(saved["items"], expected_items);
    assert!(!db.change_job_observed(&key, |_|Ok(())).await.unwrap().1);
    if !routes.is_empty() {
        assert!(db.change_status_observed(&routes, |d|{d["items"][0]["draft"]=json!("forbidden");Ok(())}).await.is_err());
        assert_eq!(db.read().await.unwrap(),saved);
    }
    assert_eq!(saved["audit"], history["audit"]);
    assert_eq!(saved["approvals"], history["approvals"]);
    assert_eq!(
        &saved["jobs"].as_array().unwrap()[..history["jobs"].as_array().unwrap().len()],
        history["jobs"].as_array().unwrap().as_slice()
    );
    assert_eq!(
        db.read_job(&key).await.unwrap().unwrap()["status"],
        "completed"
    );
    // Fail the second SQL write (metadata), after the job update, to prove the
    // job and its scheduling metadata roll back together.
    let Database::Postgres { writer: pool, .. } = &db else {
        unreachable!()
    };
    sqlx::query("ALTER TABLE communityhero.workspaces ADD CONSTRAINT bounded_write_probe_fail CHECK (metadata#>'{sync,boundedWriteFail}' IS NULL)").execute(pool).await.unwrap();
    assert!(
        db.change_job_observed(&key, |d| {
            d["jobs"][0]["status"] = json!("failed");
            d["sync"]["boundedWriteFail"] = json!(true);
            Ok(())
        })
        .await
        .is_err()
    );
    sqlx::query("ALTER TABLE communityhero.workspaces DROP CONSTRAINT bounded_write_probe_fail")
        .execute(pool)
        .await
        .unwrap();
    assert_eq!(db.read().await.unwrap(), saved);
    println!(
        "BOUNDED_WRITE_PROBE {}",
        json!({"historicalJobs":history["jobs"].as_array().unwrap().len(),"claimMs":claim_ms,"finishMs":finish_ms,"statusMs":status_ms,"scopeRoutes":routes.len()})
    );
    db.close().await;
}
