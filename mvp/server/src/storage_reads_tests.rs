use super::*;
use serde_json::json;

async fn sqlite() -> (Database, tempfile::TempDir) {
    let folder = tempfile::tempdir().unwrap();
    let pool = crate::open_db(&folder.path().join("workspace.sqlite"))
        .await
        .unwrap();
    (Database::Sqlite(pool), folder)
}

async fn seed_read_fixture(db: &Database, data: &Value) {
    let Database::Sqlite(pool) = db else { unreachable!() };
    // A synthetic read-only contract fixture, independent of domain writers.
    sqlx::query("UPDATE workspace SET payload=? WHERE id=1")
        .bind(data.to_string()).execute(pool).await.unwrap();
}

fn retrieval_fixture() -> Value {
    let mut data = crate::empty();
    data["items"] = json!([
        {"id":"fallback","itemId":"external-fallback","branchId":"b","targetId":"m","text":"Полный привод 100%_literal","createdAt":"2026-09-22T10:00:00Z","workflow":"closed","platform":"vk"},
        {"id":"explicit","branchId":"b","targetId":"m","author":"İPEK","text":"","preview":"Ответ про привод","postId":"p","createdAt":"2026-09-22T10:00:00Z","workflow":"attention"},
        {"id":"dangling-post","postId":"missing","branchId":"b","author":"Олег","title":"Fallback title","text":"Привод","createdAt":null},
        {"id":"untitled-post","postId":"untitled","title":"Must not become title","text":"Привод","createdAt":""}
    ]);
    data["posts"] = json!([{"id":"p","title":"Changan A06"},{"id":"untitled"}]);
    data["items"][0]["revision"]=json!(7);
    data["items"][0]["triageTags"]=json!(["technical-question"]);
    data["branches"] = json!([{"id":"b","postId":"p","messages":[{"id":"m","author":"Олег","text":"PRIVATE_BRANCH_BODY"}],"observedMessages":[{"text":"PRIVATE_OBSERVATION"}]}]);
    for n in (0..25).rev() {
        data["items"].as_array_mut().unwrap().push(json!({"id":format!("tie-{n:02}"),"author":"Олег","text":"Привод","createdAt":"2026-09-21T10:00:00Z"}));
    }
    data["items"].as_array_mut().unwrap().push(json!({"id":"long","text":"🦀".repeat(900),"title":"Ж".repeat(300)}));
    data["knowledge_entries"] = json!([{"id":"entry-b","scope":{"account":"LikeAvto"}},{"id":"entry-a"}]);
    data["knowledge_versions"] = json!([{"id":"version-b","text":"First version"},{"id":"version-a","text":"Next version"}]);
    data["feedback"] = json!([{"id":"owner-feedback","private":"OWNER_ONLY"}]);
    data
}

#[tokio::test]
async fn search_projection_keeps_unicode_fallbacks_literals_order_and_caps() {
    let (db, _folder) = sqlite().await;
    let data = retrieval_fixture();
    seed_read_fixture(&db, &data).await;
    let projected = db.read_search_context().await.unwrap();
    for query in ["ОЛЕГ привод", "changan", "İPEK", "i\u{307}pek", "100%_literal", "Fallback title", "Must not become title", "🦀🦀", "nonexistent", "x", "  "] {
        for limit in [0, 1, 20, 100] {
            assert_eq!(without_observation_time(crate::assistant_context::search(&projected,query,limit)),
                without_observation_time(crate::assistant_context::search(&data,query,limit)), "{query}/{limit}");
        }
    }
    let result=crate::assistant_context::search(&projected,"Привод",100).unwrap();
    assert_eq!(result["items"].as_array().unwrap().len(),20);
    assert_eq!(result["total"],29);
    assert_eq!(result["items"][0]["id"],"explicit");
    assert_eq!(result["items"][2]["id"],"tie-00");
    let serialized=projected.to_string();
    assert!(!serialized.contains("PRIVATE_BRANCH_BODY"));
    assert!(!serialized.contains("PRIVATE_OBSERVATION"));
    assert!(!serialized.contains("OWNER_ONLY"));
    db.close().await;
}

fn without_observation_time(result:Result<Value,&'static str>)->Result<Value,&'static str> {
    result.map(|mut value| {
        let observed=value.as_object_mut().unwrap().remove("observedAt").unwrap();
        assert!(chrono::DateTime::parse_from_rfc3339(observed.as_str().unwrap()).is_ok());
        value
    })
}

#[tokio::test]
async fn hot_reads_exclude_unrelated_history_and_catalog_feedback_for_operators() {
    let (db, _folder) = sqlite().await;
    let mut data=retrieval_fixture();
    seed_read_fixture(&db,&data).await;
    let before=db.read_search_context().await.unwrap();
    let catalog=db.read_knowledge_catalog(false).await.unwrap();
    assert_eq!(catalog,json!({"entries":data["knowledge_entries"],"versions":data["knowledge_versions"]}));
    assert_eq!(db.read_knowledge_catalog(true).await.unwrap(),json!({"entries":data["knowledge_entries"],"versions":data["knowledge_versions"],"feedback":data["feedback"]}));
    data["jobs"]=json!([{"id":"historical","kind":"assistant","result":{"private":"MODEL_EVIDENCE".repeat(100_000)}}]);
    data["conversations"]=json!([{"id":"someone-else","operatorId":"other-actor","messages":[{"text":"PRIVATE_CHAT".repeat(100_000)}]}]);
    data["materials"]=json!([{"id":"unused","text":"MATERIAL_BODY".repeat(100_000)}]);
    data["audit"]=json!([{"id":"audit","details":"AUDIT_EVIDENCE".repeat(100_000)}]);
    seed_read_fixture(&db,&data).await;
    let after=db.read_search_context().await.unwrap();
    assert_eq!(before,after);
    assert_eq!(catalog,db.read_knowledge_catalog(false).await.unwrap());
    assert!(after.to_string().len()<20_000);
    assert!(data.to_string().len()>4_000_000);
    println!("HOT_READ_SYNTHETIC {}",json!({"fullBytes":data.to_string().len(),"searchBytes":after.to_string().len(),"operatorCatalogBytes":catalog.to_string().len()}));
    // No cache: edits appear in the next statement, history remains ordered.
    data["knowledge_versions"][0]["text"]=json!("Fresh version");
    data["items"][0]["text"]=json!("Fresh comment");
    seed_read_fixture(&db,&data).await;
    assert_eq!(db.read_knowledge_catalog(false).await.unwrap()["versions"][0]["text"],"Fresh version");
    assert_eq!(crate::assistant_context::search(&db.read_search_context().await.unwrap(),"Fresh comment",20).unwrap()["items"][0]["id"],"fallback");
    db.close().await;
}

#[tokio::test]
async fn retrieval_reads_handle_empty_catalog_and_missing_workspace() {
    let (db, _folder)=sqlite().await;
    assert_eq!(db.read_knowledge_catalog(false).await.unwrap(),json!({"entries":[],"versions":[]}));
    assert_eq!(db.read_knowledge_catalog(true).await.unwrap(),json!({"entries":[],"versions":[],"feedback":[]}));
    assert_eq!(crate::assistant_context::search(&db.read_search_context().await.unwrap(),"test",20).unwrap()["total"],0);
    let Database::Sqlite(pool)=&db else {unreachable!()};
    sqlx::query("DELETE FROM workspace WHERE id=1").execute(pool).await.unwrap();
    assert!(db.read_search_context().await.is_err());
    assert!(db.read_knowledge_catalog(false).await.is_err());
    assert!(db.read_bootstrap_source().await.is_err());
    db.close().await;
}

#[tokio::test]
async fn bootstrap_projection_keeps_actor_inputs_and_exact_existing_public_shape() {
    let (db,_folder)=sqlite().await;
    let mut data=retrieval_fixture();
    data["conversations"]=json!([{"id":"a","operatorId":"alice","messages":[{"text":"Alice private"}]},{"id":"b","operatorId":"bob","messages":[{"text":"Bob private"}]}]);
    data["approvals"]=json!([{"id":"approval","approvalAuthority":{"generation":"HIDDEN_AUTHORITY"},"proposals":[{"text":"reviewed"}]}]);
    data["operations"]=json!([{"id":"operation","dispatchAuthority":{"generation":"HIDDEN_AUTHORITY"},"evidence":{"publicOutcome":"unknown"}}]);
    data["companyKnowledgeCoverage"]=json!({"large":"COVERAGE_PRIVATE".repeat(100_000)});
    data["companyKnowledgeAuthority"]=json!({"version":1});
    data["jobs"]=json!([]);
    for n in 0..220 {
        data["jobs"].as_array_mut().unwrap().push(json!({"id":format!("job-{n}"),"kind":"assistant","refId":if n%2==0{"a"}else{"b"},"status":"completed",
            "prepareBundle":{"id":format!("bundle-{n}"),"request":{"hidden":"HIDDEN_REQUEST".repeat(300)},"dependencyDigest":"retain"},"result":{"summary":"retain"}}));
    }
    seed_read_fixture(&db,&data).await;
    let compact=db.read_bootstrap_source().await.unwrap();
    assert_eq!(crate::bootstrap_view(compact.clone(),"csrf"),crate::bootstrap_view(data.clone(),"csrf"));
    assert_eq!(compact["jobs"].as_array().unwrap().len(),220,"actor filtering must precede terminal history cap");
    assert_eq!(compact["conversations"],data["conversations"],"private chats must remain available to actor filtering");
    assert_eq!(compact["companyKnowledgeAuthority"],data["companyKnowledgeAuthority"]);
    let serialized=compact.to_string();
    for hidden in ["HIDDEN_AUTHORITY","HIDDEN_REQUEST","COVERAGE_PRIVATE","PRIVATE_OBSERVATION","OWNER_ONLY"] {assert!(!serialized.contains(hidden),"{hidden}");}
    assert!(serialized.len()<100_000);assert!(data.to_string().len()>2_000_000);
    println!("BOOTSTRAP_PROJECTION_SYNTHETIC {}",json!({"fullBytes":data.to_string().len(),"sourceBytes":serialized.len(),"allCompactJobs":compact["jobs"].as_array().unwrap().len()}));
    db.close().await;
}

#[tokio::test]
async fn bootstrap_internal_media_catalog_selects_only_exact_current_media_heads() {
    let (db,_folder)=sqlite().await;
    let mut data=crate::empty();
    data["knowledge_entries"]=json!([
        {"id":"audio","currentVersionId":"audio-new"},
        {"id":"visual","currentVersionId":"visual-current"},
        {"id":"rule","currentVersionId":"rule-current"}
    ]);
    data["knowledge_versions"]=json!([
        {"id":"audio-old","entryId":"audio","kind":"transcript","text":"OLD_MEDIA_PRIVATE"},
        {"id":"audio-new","entryId":"audio","kind":"transcript","text":"CURRENT_MEDIA_PRIVATE"},
        {"id":"visual-current","entryId":"visual","kind":"visual_context","visualEvidence":{"private":"PRIVATE_PROOF"}},
        {"id":"rule-current","entryId":"rule","kind":"rule","text":"UNRELATED_RULE"}
    ]);
    seed_read_fixture(&db,&data).await;
    let raw=db.read_bootstrap_source().await.unwrap();
    let internal=&raw["mediaReadinessCatalog"];
    assert_eq!(internal["knowledge_entries"].as_array().unwrap().len(),2);
    assert_eq!(internal["knowledge_versions"].as_array().unwrap().len(),2);
    assert_eq!(internal["knowledge_versions"][0]["id"],"audio-new");
    assert_eq!(internal["knowledge_versions"][1]["id"],"visual-current");
    assert!(!internal.to_string().contains("OLD_MEDIA_PRIVATE"));
    assert!(!internal.to_string().contains("UNRELATED_RULE"));
    let public=crate::bootstrap_view(raw,"csrf");
    assert!(public.get("mediaReadinessCatalog").is_none());
    assert!(!public.to_string().contains("CURRENT_MEDIA_PRIVATE"));
    assert!(!public.to_string().contains("PRIVATE_PROOF"));
    // Current-head replacement must be visible in the same statement snapshot.
    data["knowledge_entries"][0]["currentVersionId"]=json!("audio-old");
    seed_read_fixture(&db,&data).await;
    assert_eq!(db.read_bootstrap_source().await.unwrap()["mediaReadinessCatalog"]["knowledge_versions"][0]["id"],"audio-old");
    db.close().await;
}

#[tokio::test]
async fn catalog_http_preserves_actor_feedback_boundary() {
    let (app,_folder)=crate::tests::test_app().await;
    let data=retrieval_fixture();seed_read_fixture(&app.db,&data).await;
    for role in ["owner","operator"] {
        let actor=crate::operator_auth::Actor{id:role.into(),name:role.into(),role:role.into(),csrf_token:"synthetic".into(),authority_generation:None};
        let axum::Json(result)=crate::operator_http::knowledge_catalog(axum::extract::State(app.clone()),axum::Extension(actor)).await.unwrap();
        assert_eq!(result.get("feedback").is_some(),role=="owner");
        assert_eq!(result["entries"],data["knowledge_entries"]);
        assert_eq!(result["versions"],data["knowledge_versions"]);
    }
    app.db.close().await;
}

/// Root alone prepares/runs this explicitly isolated clone. It reports sizes
/// and timing only, never comment text, model evidence, or connection details.
#[tokio::test]
#[ignore = "requires COMMUNITYHERO_HOT_READ_TEST_URL for an isolated hot_read_test clone"]
async fn postgres_hot_read_clone_probe() {
    let url=std::env::var("COMMUNITYHERO_HOT_READ_TEST_URL").expect("explicit isolated clone URL");
    let pool=PgPoolOptions::new().max_connections(1).connect(&url).await.unwrap();
    let database:String=sqlx::query_scalar("SELECT current_database()").fetch_one(&pool).await.unwrap();
    assert!(database.contains("hot_read_test"),"refusing non-test database");
    pool.close().await;
    let db=Database::postgres(&url).await.unwrap();
    let start=std::time::Instant::now();let full=db.read().await.unwrap();let full_ms=start.elapsed().as_secs_f64()*1000.0;
    let start=std::time::Instant::now();let search=db.read_search_context().await.unwrap();let search_ms=start.elapsed().as_secs_f64()*1000.0;
    let start=std::time::Instant::now();let owner=db.read_knowledge_catalog(true).await.unwrap();let owner_ms=start.elapsed().as_secs_f64()*1000.0;
    let start=std::time::Instant::now();let operator=db.read_knowledge_catalog(false).await.unwrap();let operator_ms=start.elapsed().as_secs_f64()*1000.0;
    let start=std::time::Instant::now();let bootstrap=db.read_bootstrap_source().await.unwrap();let bootstrap_ms=start.elapsed().as_secs_f64()*1000.0;
    let mut current_entries=Vec::new();let mut current_versions=Vec::new();
    for entry in crate::list(&full,"knowledge_entries") {
        if let Some(version)=crate::list(&full,"knowledge_versions").iter().find(|v|
            v["entryId"]==entry["id"]&&v["id"]==entry["currentVersionId"]&&matches!(v["kind"].as_str(),Some("transcript"|"visual_context"))) {
            current_entries.push(entry.clone());current_versions.push(version.clone());
        }
    }
    assert_eq!(bootstrap["mediaReadinessCatalog"],json!({"knowledge_entries":current_entries,"knowledge_versions":current_versions}),"Internal readiness catalog differs from current media heads");
    assert!(crate::bootstrap_view(bootstrap.clone(),"synthetic-csrf")==crate::bootstrap_view(full.clone(),"synthetic-csrf"),"Bootstrap public shape differs from full read");
    assert!(owner==json!({"entries":full["knowledge_entries"],"versions":full["knowledge_versions"],"feedback":full["feedback"]}),"Owner catalog differs from full read");
    assert!(operator==json!({"entries":full["knowledge_entries"],"versions":full["knowledge_versions"]}),"Operator catalog differs from full read");
    let mut queries=vec!["привод".to_owned(),"LikeAvto".to_owned(),"100%_literal".to_owned(),"İPEK".to_owned()];
    if let Some(id)=full["items"].as_array().and_then(|items|items.first()).and_then(|item|item["id"].as_str()) {queries.push(id.to_owned());}
    for query in queries {assert!(without_observation_time(crate::assistant_context::search(&search,&query,20))==without_observation_time(crate::assistant_context::search(&full,&query,20)),"Search result differs from full read");}
    println!("HOT_READ_PG_PROBE {}",json!({"items":full["items"].as_array().unwrap().len(),"jobs":full["jobs"].as_array().unwrap().len(),
        "fullBytes":full.to_string().len(),"searchBytes":search.to_string().len(),"ownerCatalogBytes":owner.to_string().len(),"operatorCatalogBytes":operator.to_string().len(),
        "bootstrapBytes":bootstrap.to_string().len(),"bootstrapMs":bootstrap_ms,
        "fullMs":full_ms,"searchMs":search_ms,"ownerCatalogMs":owner_ms,"operatorCatalogMs":operator_ms}));
    db.close().await;
}

fn scheduler_fixture() -> Value {
    json!({
        "account":"LikeAvto",
        "jobs":[
            {"id":"status","kind":"status_sync","status":"running","result":{"large":"discard"}},
            {"id":"context","kind":"context_sync","status":"queued","request":{"large":"discard"}},
            {"id":"done","kind":"sync","status":"completed","result":{"large":"discard"}},
            {"id":"failed","kind":"context_sync","status":"error"},
            {"id":"unknown","kind":"execute","status":"unknown"}
        ],
        "sync":{
            "background":{"nextRunAt":"2026-09-22T12:00:00Z","large":"discard"},
            "fastStatus":{"nextRunAt":"bad-clock","results":{"large":"discard"}},
            "pendingContext":{
                "queued":{"status":"queued","queuedAt":"2026-09-22T11:00:00Z","objectId":"one","itemId":"two"},
                "live":{"status":"running","jobId":"context","retryAt":null},
                "orphan":{"status":"running","jobId":"done"},
                "retry":{"status":"error","retryAt":"2026-09-22T13:00:00Z","error":"discard"}
            },
            "scan":{"large":"discard"}
        },
        "branches":[{"id":"branch","messages":[{"text":"discard"}]}],
        "materials":[{"id":"material","text":"discard"}]
    })
}

// Independent expected projection over a complete read, including the exact
// narrow field contract used by the scheduler's existing admission predicates.
fn expected_schedule(full: &Value) -> Value {
    let pending = full["sync"]["pendingContext"]
        .as_object()
        .into_iter()
        .flatten()
        .map(|(key, value)| {
            (
                key.clone(),
                json!({
                    "status":value["status"],"jobId":value["jobId"],
                    "retryAt":value["retryAt"],"queuedAt":value["queuedAt"]
                }),
            )
        })
        .collect::<serde_json::Map<_, _>>();
    let jobs = full["jobs"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|j| matches!(j["status"].as_str(), Some("running" | "queued")))
        .map(|j| json!({"id":j["id"],"kind":j["kind"],"status":j["status"]}))
        .collect::<Vec<_>>();
    json!({"sync":{
        "fastStatus":{"nextRunAt":full["sync"]["fastStatus"]["nextRunAt"]},
        "background":{"nextRunAt":full["sync"]["background"]["nextRunAt"]},
        "pendingContext":pending
    },"jobs":jobs})
}

#[tokio::test]
async fn point_job_reads_are_current_and_missing_is_distinct_from_missing_workspace() {
    let (db, _folder) = sqlite().await;
    db.change(|d| {
        d["jobs"] = scheduler_fixture()["jobs"].clone();
        Ok(())
    })
    .await
    .unwrap();
    assert_eq!(
        db.read_job("status").await.unwrap(),
        Some(scheduler_fixture()["jobs"][0].clone())
    );
    assert_eq!(db.read_job("missing").await.unwrap(), None);
    assert_eq!(db.read_job("status' OR 1=1 --").await.unwrap(), None);
    db.change(|d| {
        d["jobs"][0]["status"] = json!("completed");
        Ok(())
    })
    .await
    .unwrap();
    assert_eq!(
        db.read_job("status").await.unwrap().unwrap()["status"],
        "completed"
    );
    let Database::Sqlite(pool) = &db else {
        unreachable!()
    };
    sqlx::query("DELETE FROM workspace WHERE id=1")
        .execute(pool)
        .await
        .unwrap();
    assert!(db.read_job("missing").await.is_err());
    assert!(db.read_schedule().await.is_err());
    assert!(db.readiness(Duration::from_secs(1)).await.is_err());
}

#[tokio::test]
async fn scheduling_reads_exclude_evidence_and_keep_retry_and_abandonment_inputs() {
    let (db, _folder) = sqlite().await;
    db.change(|d| {
        for (key, value) in scheduler_fixture().as_object().unwrap() {
            d[key] = value.clone();
        }
        for n in 0..200 {
            d["jobs"].as_array_mut().unwrap().push(json!({
                "id":format!("old-{n}"),"kind":"sync","status":"completed",
                "result":{"text":"x".repeat(8192)}
            }));
        }
        Ok(())
    })
    .await
    .unwrap();
    let full = db.read().await.unwrap();
    let schedule = db.read_schedule().await.unwrap();
    assert_eq!(schedule, expected_schedule(&full));
    assert!(schedule.to_string().len() < 1500);
    assert!(full.to_string().len() > 1_500_000);
    db.change(|d| {
        d["jobs"][0]["status"] = json!("completed");
        d["sync"]["background"]["nextRunAt"] = Value::Null;
        Ok(())
    })
    .await
    .unwrap();
    let next = db.read_schedule().await.unwrap();
    assert_eq!(next, expected_schedule(&db.read().await.unwrap()));
    assert_eq!(next["jobs"].as_array().unwrap().len(), 1);
    assert!(next["sync"]["background"]["nextRunAt"].is_null());
}

#[tokio::test]
async fn absent_and_malformed_optional_scheduler_fields_do_not_panic() {
    let (db, _folder) = sqlite().await;
    for sync in [
        Value::Null,
        json!({}),
        json!({"pendingContext":[]}),
        json!({
            "fastStatus":{"nextRunAt":"invalid"},"pendingContext":{
                "scalar":"no object","null":null,"array":[],"valid":{"status":"queued","retryAt":"bad-date"}
            }
        }),
    ] {
        db.change(|d| {
            d["sync"] = sync.clone();
            Ok(())
        })
        .await
        .unwrap();
        assert_eq!(
            db.read_schedule().await.unwrap(),
            expected_schedule(&db.read().await.unwrap())
        );
    }
}

#[tokio::test]
async fn readiness_deadline_covers_waiting_for_the_connection_and_pool_recovers() {
    let (db, _folder) = sqlite().await;
    db.readiness(Duration::from_secs(1)).await.unwrap();
    let Database::Sqlite(pool) = &db else {
        unreachable!()
    };
    let held = pool.acquire().await.unwrap();
    let started = std::time::Instant::now();
    let error = db.readiness(Duration::from_millis(30)).await.unwrap_err();
    assert!(error.1.contains("deadline"));
    assert!(started.elapsed() < Duration::from_secs(2));
    drop(held);
    db.readiness(Duration::from_secs(1)).await.unwrap();
}

#[tokio::test]
#[ignore = "requires COMMUNITYHERO_POINT_READ_TEST_URL for an isolated point_read_test clone"]
async fn postgres_readiness_uses_read_only_connection_while_main_pool_is_held() {
    let url=std::env::var("COMMUNITYHERO_POINT_READ_TEST_URL").expect("explicit isolated clone URL");
    let pool=PgPoolOptions::new().max_connections(1).connect(&url).await.unwrap();
    let database:String=sqlx::query_scalar("SELECT current_database()")
        .fetch_one(&pool).await.unwrap();
    assert!(database.contains("point_read_test"),"refusing non-test database");
    pool.close().await;
    let db=Database::postgres(&url).await.unwrap();
    let Database::Postgres { writer, .. }=&db else {unreachable!()};
    let held=writer.acquire().await.unwrap();
    db.readiness(Duration::from_secs(2)).await.expect("readiness must bypass the held lease pool");
    drop(held);
    db.close().await;
    assert!(db.readiness(Duration::from_secs(2)).await.is_err(),"closed pool must not report ready");
}

/// Root owns this disposable clone and runs the probe explicitly. All appended
/// records are synthetic; this test never invokes a provider or model.
#[tokio::test]
#[ignore = "requires COMMUNITYHERO_ASSISTANT_SCOPE_TEST_URL for isolated assistant_scope_test clone"]
async fn postgres_reader_pool_preserves_owner_and_allows_reads_during_write() {
    let url = std::env::var("COMMUNITYHERO_ASSISTANT_SCOPE_TEST_URL")
        .expect("explicit isolated clone URL");
    let probe = PgPoolOptions::new().max_connections(1).connect(&url).await.unwrap();
    let database: String = sqlx::query_scalar("SELECT current_database()")
        .fetch_one(&probe).await.unwrap();
    assert!(database.contains("assistant_scope_test"), "refusing non-test database");
    probe.close().await;

    let db = Database::postgres(&url).await.unwrap();
    let chat = format!("reader-pool-chat-{}", crate::id());
    let job_id = format!("reader-pool-job-{}", crate::id());
    let expected_job = json!({"id":job_id,"kind":"assistant","status":"completed",
        "refId":chat,"operatorId":"local-owner","result":{"synthetic":true}});
    db.change(|data| {
        data["conversations"].as_array_mut().unwrap().push(
            json!({"id":chat,"operatorId":"local-owner","messages":[]}));
        data["jobs"].as_array_mut().unwrap().push(expected_job.clone());
        Ok(())
    }).await.unwrap();
    let Database::Postgres { writer, reader } = &db else { unreachable!() };
    let mut held = writer.begin().await.unwrap();
    sqlx::query("SELECT id FROM communityhero.workspaces WHERE id=$1 FOR UPDATE")
        .bind(WORKSPACE).fetch_one(&mut *held).await.unwrap();

    let (point, scoped) = tokio::time::timeout(Duration::from_secs(15), async {
        tokio::join!(db.read_job(&job_id), db.read_assistant_context(Some(&job_id), &chat))
    }).await.expect("reads must not queue behind the held writer transaction");
    assert_eq!(point.unwrap(), Some(expected_job.clone()));
    let scoped = scoped.unwrap();
    assert_eq!(scoped["conversations"].as_array().unwrap().len(), 1);
    assert_eq!(scoped["conversations"][0]["id"], chat);
    assert!(scoped["jobs"].as_array().unwrap().contains(&expected_job));
    db.readiness(Duration::from_secs(2)).await.unwrap();
    let error = sqlx::query("UPDATE communityhero.workspaces SET account=account WHERE false")
        .execute(reader).await.expect_err("reader statements must remain read-only");
    assert_eq!(error.as_database_error().and_then(|e| e.code()).as_deref(), Some("25006"));
    held.rollback().await.unwrap();

    assert!(Database::postgres(&url).await.is_err(), "second owner must not obtain writer lease");
    reader.close().await;
    assert!(db.readiness(Duration::from_secs(2)).await.is_err(), "closed reader must fail readiness");
    assert!(!writer.is_closed(), "reader shutdown must not release writer ownership");
    db.close().await;
    assert!(reader.is_closed() && writer.is_closed());
    assert!(db.readiness(Duration::from_secs(2)).await.is_err());
    let replacement = Database::postgres(&url).await.unwrap();
    let Database::Postgres { writer, reader } = &replacement else { unreachable!() };
    writer.close().await;
    assert!(!reader.is_closed());
    assert!(replacement.readiness(Duration::from_secs(2)).await.is_err(), "closed writer must fail readiness");
    replacement.close().await;
}

/// Read-only probe for an explicitly prepared isolated clone. The name guard
/// prevents accidentally pointing this test at the held pilot. Root owns clone
/// creation and optional 1x/5x/10x historical-job seeding between probe runs.
#[tokio::test]
#[ignore = "requires COMMUNITYHERO_POINT_READ_TEST_URL for an isolated point_read_test clone"]
async fn postgres_point_read_clone_probe() {
    let url =
        std::env::var("COMMUNITYHERO_POINT_READ_TEST_URL").expect("explicit isolated clone URL");
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
        database.contains("point_read_test"),
        "refusing non-test database"
    );
    pool.close().await;
    let db = Database::postgres(&url).await.unwrap();
    let started = std::time::Instant::now();
    let full = db.read().await.unwrap();
    let full_ms = started.elapsed().as_secs_f64() * 1000.0;
    let started = std::time::Instant::now();
    let schedule = db.read_schedule().await.unwrap();
    let schedule_ms = started.elapsed().as_secs_f64() * 1000.0;
    assert_eq!(schedule, expected_schedule(&full));
    assert_eq!(
        db.read_job("missing-point-read-test-job").await.unwrap(),
        None
    );
    let started = std::time::Instant::now();
    let job = if let Some(job) = full["jobs"].as_array().unwrap().last() {
        let read = db.read_job(job["id"].as_str().unwrap()).await.unwrap();
        assert_eq!(read.as_ref(), Some(job));
        read
    } else {
        None
    };
    let job_ms = started.elapsed().as_secs_f64() * 1000.0;
    let started = std::time::Instant::now();
    db.readiness(Duration::from_secs(2)).await.unwrap();
    let readiness_ms = started.elapsed().as_secs_f64() * 1000.0;
    println!(
        "POINT_READ_PROBE {}",
        json!({
            "jobs":full["jobs"].as_array().unwrap().len(),"fullBytes":full.to_string().len(),
            "scheduleBytes":schedule.to_string().len(),"jobBytes":job.map(|j|j.to_string().len()),
            "fullMs":full_ms,"scheduleMs":schedule_ms,"jobMs":job_ms,"readinessMs":readiness_ms
        })
    );
    db.close().await;
}
