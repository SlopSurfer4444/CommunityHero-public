use super::*;

#[test]
fn bounded_schedule_preserves_spent_repair_without_requiring_omitted_child() {
    let root=json!({"id":"origin","kind":"assistant","status":"running","refId":"",
        "preparationStages":{"repairBudget":{"schemaVersion":1,"maxRounds":1,"consumedRounds":1,
            "authority":"admitted_workflow_v1"},"answeringRepairs":[{
                "originatingAnsweringAttemptId":"origin","roundOrdinal":1,"childJobId":"completed-child",
                "planSha256":"saved"}]}});
    let before=json!({"sync":{},"jobs":[root],"items":[]});
    let mut after=before.clone();
    after["jobs"].as_array_mut().unwrap().push(json!({"id":"next-sync","kind":"sync","status":"queued","refId":""}));
    validate_scope(&before,&after,&Scope::Schedule).unwrap();
    validate_scope(&before,&after,&Scope::SourceClaim).unwrap();
    let mut changed=before.clone();changed["jobs"][0]["status"]=json!("completed");
    validate_scope(&before,&changed,&Scope::Job("origin")).unwrap();
    changed["jobs"][0]["preparationStages"]["repairBudget"]["consumedRounds"]=json!(0);
    assert!(validate_scope(&before,&changed,&Scope::Job("origin")).is_err());
}

#[test]
fn bounded_schedule_cannot_mint_repair_or_frame_authority() {
    let before=json!({"sync":{},"jobs":[],"items":[]});
    for field in ["originatingAnsweringAttemptId","roundOrdinal","answeringRepairPlan","repairPaidIntent",
        "frameNeed","framePlan","frameLease","frameResult"] {
        let mut after=before.clone();
        let mut job=json!({"id":"new","kind":"assistant","status":"queued","refId":""});
        job[field]=Value::Null;
        after["jobs"]=json!([job]);
        assert!(validate_scope(&before,&after,&Scope::Schedule).is_err(),"{field}");
        assert!(validate_scope(&before,&after,&Scope::SourceClaim).is_err(),"{field}");
    }
    for purpose in ["answering_repair","targeted_video_frames"] {
        let mut after=before.clone();
        after["jobs"]=json!([{"id":"new","kind":"assistant","status":"queued","refId":"","purpose":purpose}]);
        assert!(validate_scope(&before,&after,&Scope::Schedule).is_err());
    }
}

#[tokio::test]
#[ignore = "read-only exact BAW standby timing; PGPASSWORD supplied privately"]
async fn postgres_metadata_read_live_readonly_probe() {
    let url = std::env::var("COMMUNITYHERO_METADATA_READ_LIVE_URL").expect("explicit read-only probe URL");
    assert_eq!(url, "postgresql://ch_migrate@127.0.0.1:55439/communityhero_knowledge_baw_standby_20260922");
    let reader = PgPoolOptions::new().max_connections(1)
        .after_connect(|connection, _| Box::pin(async move {
            sqlx::query("SET default_transaction_read_only = on").execute(connection).await?;
            Ok(())
        })).connect(&url).await.unwrap();
    let database: String = sqlx::query_scalar("SELECT current_database()")
        .fetch_one(&reader).await.unwrap();
    assert_eq!(database, "communityhero_knowledge_baw_standby_20260922");
    // This test constructs a read-only pool directly: Database::postgres would
    // acquire the application writer lease and is deliberately not called.
    let db = Database::Postgres { writer: reader.clone(), reader };
    let mut full_ms = Vec::new();
    let mut metadata_ms = Vec::new();
    let mut bytes = (0, 0);
    for iteration in 0..3 {
        let started = std::time::Instant::now();
        let (full, scoped, first_ms, second_ms) = if iteration % 2 == 0 {
            let full = db.read().await.unwrap();
            let first_ms = started.elapsed().as_secs_f64() * 1000.0;
            let started = std::time::Instant::now();
            let scoped = db.read_metadata().await.unwrap();
            (full, scoped, first_ms, started.elapsed().as_secs_f64() * 1000.0)
        } else {
            let scoped = db.read_metadata().await.unwrap();
            let first_ms = started.elapsed().as_secs_f64() * 1000.0;
            let started = std::time::Instant::now();
            let full = db.read().await.unwrap();
            (full, scoped, first_ms, started.elapsed().as_secs_f64() * 1000.0)
        };
        assert_eq!(scoped["account"], full["account"]);
        assert!(TABLES.iter().all(|table| scoped.get(table).is_none()));
        bytes = (full.to_string().len(), scoped.to_string().len());
        if iteration % 2 == 0 {
            full_ms.push(first_ms); metadata_ms.push(second_ms);
        } else {
            metadata_ms.push(first_ms); full_ms.push(second_ms);
        }
    }
    full_ms.sort_by(f64::total_cmp);
    metadata_ms.sort_by(f64::total_cmp);
    eprintln!("metadata read parity: full bytes {} ms {:?}; scoped bytes {} ms {:?}", bytes.0, full_ms, bytes.1, metadata_ms);
    db.close().await;
}

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
    let before = db.read().await.unwrap();
    assert_eq!(db.read_source_status().await.unwrap(), project(&before, &Scope::SourceClaim).unwrap());
    assert_eq!(db.read().await.unwrap(), before);
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
        db.change_status_observed(&vec![json!({"objectId":"x","itemId":"y"}); STATUS_ROUTE_LIMIT+1], |_| Ok(()))
            .await
            .is_err()
    );
}

/// Exact disposable clone only: never accepts a production or standby URL.
/// The writer transaction is rolled back; no claim or history is committed.
#[tokio::test]
#[ignore = "requires explicit isolated v48 source-reader clone URL"]
async fn postgres_source_status_reader_survives_held_writer() {
    let url = std::env::var("COMMUNITYHERO_SOURCE_READ_TEST_URL").expect("explicit isolated clone URL");
    assert_eq!(url, "postgresql://ch_migrate@127.0.0.1:55439/communityhero_point_read_test_v48_20260926", "refusing non-test URL");
    let check = PgPoolOptions::new().max_connections(1).connect(&url).await.unwrap();
    let name: String = sqlx::query_scalar("SELECT current_database()").fetch_one(&check).await.unwrap();
    assert_eq!(name, "communityhero_point_read_test_v48_20260926", "refusing non-test database");
    check.close().await;
    let db = Database::postgres(&url).await.unwrap();
    let before = db.read().await.unwrap();
    // This is also the SQLite source-read projection, including all empty
    // history placeholders, source fields and ordered full active-job payloads.
    let expected = project(&before, &Scope::SourceClaim).unwrap();
    let Database::Postgres { writer, reader } = &db else { unreachable!() };
    let mut held = writer.begin().await.unwrap();
    sqlx::query("SELECT id FROM communityhero.workspaces WHERE id=$1 FOR UPDATE")
        .bind(WORKSPACE).fetch_one(&mut *held).await.unwrap();
    sqlx::query("UPDATE communityhero.workspaces SET metadata=jsonb_set(metadata,'{v48UncommittedReadProbe}','true'::jsonb) WHERE id=$1")
        .bind(WORKSPACE).execute(&mut *held).await.unwrap();
    let started = std::time::Instant::now();
    let observed = tokio::time::timeout(Duration::from_secs(5), db.read_source_status()).await
        .expect("source read must not wait for held writer").unwrap();
    let read_ms = started.elapsed().as_secs_f64()*1000.0;
    assert_eq!(observed, expected);
    assert!(observed.get("v48UncommittedReadProbe").is_none());
    let readonly: String = sqlx::query_scalar("SHOW default_transaction_read_only").fetch_one(reader).await.unwrap();
    assert_eq!(readonly, "on");
    held.rollback().await.unwrap();
    assert_eq!(db.read().await.unwrap(), before, "source read must not mutate claims or history");
    println!("SOURCE_READER_PROBE {}", json!({"readMs":read_ms,"items":expected["items"].as_array().unwrap().len(),"activeJobs":expected["jobs"].as_array().unwrap().len(),"writerHeld":true,"claimMutation":false}));
    db.close().await;
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

#[tokio::test]
async fn source_reader_preserves_scoped_validation_without_writing() {
    let (db, _folder) = sqlite().await;
    db.change(|d| { seed(d); Ok(()) }).await.unwrap();
    let baseline = db.read().await.unwrap();
    let Database::Sqlite(pool) = &db else { unreachable!() };
    for case in 0..4 {
        let mut malformed = baseline.clone();
        match case {
            0 => malformed["sync"] = json!(false),
            1 => malformed["items"][1]["id"] = malformed["items"][0]["id"].clone(),
            2 => { malformed["items"][0].as_object_mut().unwrap().remove("id"); },
            _ => malformed["jobs"][1]["kind"] = json!(17),
        }
        let projected = project(&malformed, &Scope::SourceClaim).unwrap();
        assert!(validate_scope(&projected, &projected, &Scope::SourceClaim).is_err(), "old scoped-read contract must reject case {case}");
        sqlx::query("UPDATE workspace SET payload=? WHERE id=1").bind(malformed.to_string()).execute(pool).await.unwrap();
        assert!(db.read_source_status().await.is_err(), "reader must reject case {case}");
        assert_eq!(db.read().await.unwrap(), malformed, "read must not repair or mutate malformed state");
    }
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
