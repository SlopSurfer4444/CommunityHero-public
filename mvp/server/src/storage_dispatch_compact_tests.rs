use super::*;
use serde_json::json;

async fn sqlite() -> (Database, tempfile::TempDir) {
    let folder = tempfile::tempdir().unwrap();
    let pool = crate::open_db(&folder.path().join("dispatch-compact.sqlite")).await.unwrap();
    (Database::Sqlite(pool), folder)
}
async fn seed(db: &Database, data: &Value) {
    let Database::Sqlite(pool) = db else { unreachable!() };
    sqlx::query("UPDATE workspace SET payload=? WHERE id=1").bind(data.to_string()).execute(pool).await.unwrap();
}
fn fixture(profile: crate::accounts::Profile) -> (Value, String, Value) {
    let mut data = crate::empty();
    data["account"] = json!(profile.display());
    data["connectorBinding"] = profile.binding();
    data["posts"] = json!([{"id":"post","postKey":"post-key","text":"Synthetic video", "attachments":[{"type":"video"}]}]);
    data["branches"] = json!([{"id":"branch","postId":"post","messages":[{"id":"message","text":"Synthetic question"}]}]);
    data["items"] = json!([{"id":"item","itemId":"external-item","objectId":"object","postId":"post","branchId":"branch",
        "postKey":"post-key","conversationKey":"conversation","connectorBinding":profile.binding(),"platform":"vk",
        "text":"Synthetic question","revision":1,"workflow":"attention","providerStatus":"new",
        "contextEvidenceDigest":"a".repeat(64),"branchContextDigest":"b".repeat(64)}]);
    let post = data["posts"][0].clone();
    let source = crate::media_fullframes::source_version(&post, profile.display());
    let mut progress = crate::media_fullframes::initial(profile.display(), &profile.binding(), &post, "now");
    progress["phase"] = json!("inventory");
    progress["source"] = json!({"sha256":"a".repeat(64),"bytes":1024});
    progress["sourceIdentity"] = json!({"account":profile.display(),"postKey":"post-key","mediaSha256":"a".repeat(64),"durationMs":181000});
    data["jobs"] = json!([{"id":"media","kind":"media","purpose":"auto_media","status":"completed","visualContractVersion":2,
        "account":profile.display(),"connectorBinding":profile.binding(),"refId":"post","createdAt":"2026-01-01T00:00:00Z",
        "result":{"visualProgress":progress}}]);
    data["materials"] = json!([{"id":"transcript","account":profile.display(),"postKey":"post-key","kind":"transcript",
        "text":"Complete spoken source","transcription":{"partial":false,"coverage":"full_audio","sourceVersion":source,
        "mediaDurationSeconds":181.0,"audioDurationSeconds":181.0}}]);
    crate::knowledge::sync_catalog(&mut data,"2026-01-01T00:00:00Z").unwrap();
    // This retained generation exercises legacy bundle/provenance projection,
    // not the new exact-review contract for manually staged video drafts.
    let proposal = crate::create_generated_proposal(&mut data,&json!({"itemId":"item","kind":"close","expectedRevision":1})).unwrap();
    assert!(!crate::decision_media::enabled(&proposal));
    let id = proposal["id"].as_str().unwrap().to_owned();
    let bundle = crate::prepare_bundle::build(&data,&[json!("item")],&[]).unwrap();
    let proposal = crate::row_mut(&mut data,"proposals",&id).unwrap();
    proposal["prepareRunId"] = json!("prepare");
    proposal["prepareBundleId"] = bundle["id"].clone();
    proposal["prepareBundleDigest"] = bundle["digest"].clone();
    data["jobs"].as_array_mut().unwrap().push(json!({"id":"prepare","kind":"assistant","status":"completed","prepareBundle":bundle}));
    let operation = json!({"id":"next","target":{"connectorBinding":profile.binding()},"action":{"action":"reply_and_close","conversationKey":"conversation"}});
    data["operations"] = json!([{"id":"prior","itemId":"item","proposalId":id,"approvalId":"historical-approval","status":"unknown",
        "target":{"connectorBinding":profile.binding()},"action":{"action":"reply_and_close","conversationKey":"conversation"}}]);
    assert!(result(&data,&id).is_ok());
    (data,id,operation)
}
fn result(data: &Value, id: &str) -> Result<Value, (u16, String)> {
    crate::row(data,"proposals",id).and_then(|proposal|crate::proposal_current(data,proposal)).map_err(|error|(error.0.as_u16(),error.1))
}

fn owner_close_fixture()->(Value,String,Value){
    let(mut data,id,_)=fixture(crate::accounts::Profile::LikeAvto);
    normalize(&mut data);
    let item=data["items"][0].clone();
    let proposal_revision=crate::row(&data,"proposals",&id).unwrap()["revision"].clone();
    data["approvals"]=json!([{"id":"historical-approval","proposals":[{"id":id,"revision":proposal_revision}]},{"id":"new-close-approval","proposals":[{"id":id,"revision":proposal_revision}]}]);
    data["operations"]=json!([{"id":"prior","itemId":"item","proposalId":id,"approvalId":"historical-approval","status":"unknown",
        "target":item,"action":{"action":"reply_and_close","actionId":"prior","objectId":item["objectId"],"itemId":item["itemId"],"conversationKey":item["conversationKey"]},
        "executeReceipt":{"mutationOutcome":"uncertain","immutableReceipt":"preserve-original"},"evidence":{"verificationPhase":"unconfirmed"}}]);
    let preserved=crate::unknown_reply_close::capture(&data,&item,&json!(["prior"])).unwrap();
    crate::row_mut(&mut data,"proposals",&id).unwrap()["operatorCloseDecision"]=json!({"decisionSha256":"c".repeat(64),"preservedUnknownReplies":preserved});
    let own=json!({"id":"own-close","itemId":"item","proposalId":id,"approvalId":"new-close-approval","status":"dispatching",
        "approvedOperatorCloseDecisionSha256":"c".repeat(64),"target":item,
        "action":{"action":"close","actionId":"own-close","objectId":item["objectId"],"itemId":item["itemId"]}});
    data["operations"].as_array_mut().unwrap().push(own.clone());
    validate(&data).unwrap();
    (data,id,own)
}

async fn assert_owner_dispatch_unknown_parity(db:&Database,data:&Value,id:&str,own:&Value){
    let before=db.read().await.unwrap();let view=db.read_dispatch_context(id).await.unwrap();
    for op in ["prior","own-close"]{assert_eq!(crate::row(&view,"operations",op).unwrap(),crate::row(data,"operations",op).unwrap(),"full immutable proof and exact own marker must survive dispatch projection");}
    assert_eq!(view["activeExternalJobs"]["complete"],true);
    let item=crate::row(&view,"items","item").unwrap();let proposal=crate::row(&view,"proposals",id).unwrap();
    assert!(crate::unknown_reply_close::validate_for_operation(&view,proposal,item,own),"a distinct close is valid without resolving or replaying the original UNKNOWN reply");
    assert_eq!(db.read().await.unwrap(),before,"dispatch context is read-only");
}

#[tokio::test]
async fn owner_close_dispatch_retains_unknown_proof_and_rechecks_late_original_writer(){
    let(db,_folder)=sqlite().await;let(mut data,id,own)=owner_close_fixture();seed(&db,&data).await;
    assert_owner_dispatch_unknown_parity(&db,&data,&id,&own).await;
    data["jobs"].as_array_mut().unwrap().push(json!({"id":"late-original-readback","kind":"reconcile","status":"queued","refId":"prior"}));
    seed(&db,&data).await;let view=db.read_dispatch_context(&id).await.unwrap();
    assert!(!crate::unknown_reply_close::validate_for_operation(&view,crate::row(&view,"proposals",&id).unwrap(),crate::row(&view,"items","item").unwrap(),&own),"a late original readback denies the new close");
    assert_eq!(view["operations"],data["operations"]);
}

#[tokio::test]
#[ignore="requires a pristine explicitly isolated communityhero_writer_v51_test_ PostgreSQL fixture"]
async fn postgres_owner_close_dispatch_full_unknown_proof_parity(){
    let db=super::super::preparation::writer_v51_fixture_db().await;let(mut data,id,own)=owner_close_fixture();
    db.change(|d|{*d=data.clone();Ok(())}).await.unwrap();assert_owner_dispatch_unknown_parity(&db,&data,&id,&own).await;
    data["jobs"].as_array_mut().unwrap().push(json!({"id":"late-original-execute","kind":"execute","status":"running","refId":"historical-approval"}));
    db.change(|d|{*d=data.clone();Ok(())}).await.unwrap();let view=db.read_dispatch_context(&id).await.unwrap();
    assert!(!crate::unknown_reply_close::validate_for_operation(&view,crate::row(&view,"proposals",&id).unwrap(),crate::row(&view,"items","item").unwrap(),&own));
    assert_eq!(view["operations"],data["operations"]);db.close().await;
}

#[tokio::test]
async fn dispatch_compact_receipt_growth_does_not_grow_transferred_context_or_change_domain_results() {
    let (db,_folder) = sqlite().await;
    for profile in [crate::accounts::Profile::LikeAvto,crate::accounts::Profile::BawRussia] {
        let (mut data,id,operation) = fixture(profile);
        seed(&db,&data).await;
        let baseline = db.read_dispatch_context(&id).await.unwrap();
        for bytes in [1024, 1_048_576] {
            let blob = json!({"synthetic":"RECEIPT_NOT_DISPATCH_CONTEXT".repeat(bytes / 26)});
            data["operations"][0]["executeReceipt"] = blob.clone();
            data["operations"][0]["result"] = blob.clone();
            data["operations"][0]["action"]["readbackEvidence"] = blob.clone();
            data["operations"][0]["dispatchAuthority"] = blob.clone();
            data["jobs"][1]["result"] = blob.clone();
            data["jobs"][1]["request"] = blob.clone();
            data["jobs"][1]["runMetadata"] = blob.clone();
            data["jobs"][0]["result"]["visualProgress"]["frames"] = blob;
            seed(&db,&data).await;
            let started = std::time::Instant::now();
            let view = db.read_dispatch_context(&id).await.unwrap();
            let elapsed = started.elapsed();
            assert_eq!(result(&view,&id),result(&data,&id));
            assert_eq!(crate::dispatch_evidence::conversation_blocker(&view,&operation),Some("prior".into()));
            assert_eq!(crate::post_media_policy::probed_duration(&view,&view["posts"][0]),
                crate::post_media_policy::probed_duration(&data,&data["posts"][0]));
            // Empty result is retained when the original has only omitted keys.
            let mut expected = baseline.clone(); expected["jobs"][1]["result"] = json!({});
            assert_eq!(view,expected);
            let Database::Sqlite(pool) = &db else { unreachable!() };
            let statement = sqlite_dispatch_statement();
            let raw: String = sqlx::query_scalar(sqlx::AssertSqlSafe(statement.as_str())).bind(&id).fetch_one(pool).await.unwrap();
            assert!(!raw.contains("RECEIPT_NOT_DISPATCH_CONTEXT"));
            assert!(raw.len() < 30_000);
            eprintln!("dispatch compact synthetic: input_bytes={} output_bytes={} read_us={}",data.to_string().len(),raw.len(),elapsed.as_micros());
        }
    }
    db.close().await;
}

#[tokio::test]
async fn dispatch_compact_keeps_media_provenance_staleness_and_structural_failures() {
    let (db,_folder) = sqlite().await;
    let (baseline,id,_) = fixture(crate::accounts::Profile::BawRussia);
    for pointer in ["/jobs/0/account", "/jobs/0/connectorBinding", "/jobs/0/status", "/jobs/0/purpose", "/jobs/0/createdAt",
        "/jobs/0/result/visualProgress/phase", "/jobs/0/result/visualProgress/sourceVersion", "/jobs/0/result/visualProgress/source/sha256",
        "/jobs/0/result/visualProgress/sourceIdentity/durationMs", "/jobs/1/prepareBundle/digest", "/branches/0/messages/0/text"] {
        let mut data = baseline.clone();
        *data.pointer_mut(pointer).unwrap() = json!("changed");
        seed(&db,&data).await;
        let view = db.read_dispatch_context(&id).await.unwrap();
        assert_eq!(result(&view,&id),result(&data,&id),"{pointer}");
        assert_eq!(crate::post_media_policy::probed_duration(&view,&view["posts"][0]),
            crate::post_media_policy::probed_duration(&data,&data["posts"][0]),"{pointer}");
    }
    for pointer in ["/operations/0/id","/operations/0/itemId","/operations/0/proposalId","/operations/0/approvalId","/operations/0/status",
        "/jobs/0/id","/jobs/0/kind","/jobs/0/status","/jobs/0/refId"] {
        let mut data = baseline.clone(); *data.pointer_mut(pointer).unwrap() = json!({"invalid":true});
        seed(&db,&data).await;
        // Invalid media kind is outside the existing media-job selection, so
        // exercise that corruption on the explicitly selected preparation job.
        if pointer=="/jobs/0/kind" { data["jobs"][1]["kind"] = json!({"invalid":true}); seed(&db,&data).await; }
        assert!(db.read_dispatch_context(&id).await.is_err(),"{pointer}");
    }
    db.close().await;
}

#[tokio::test]
async fn dispatch_compact_sqlite_preserves_json_types_absence_and_exact_connector_binding() {
    let (db,_folder) = sqlite().await;
    let Database::Sqlite(pool) = &db else { unreachable!() };
    for binding in [Value::Null,json!({"revision":2,"exact":true,"extra":null,"array":[false,"{}"]}),json!("legacy")] {
        for target in [json!({"connectorBinding":binding}),json!({}),Value::Null,json!([]),json!(true),json!("{\"x\":1}")] {
            let source = json!({"id":"operation","status":"unknown","target":target,"action":{"action":"reply_and_close","conversationKey":"true"}});
            let projection = compact_payload_sql("operations","?1",false,0);
            let sql = format!("SELECT {projection}");
            let raw: String = sqlx::query_scalar(sqlx::AssertSqlSafe(sql.as_str())).bind(source.to_string()).fetch_one(pool).await.unwrap();
            assert_eq!(parse(&raw).unwrap(),source);
        }
    }
    db.close().await;
}

// Pure SELECT fixture: root may run against an explicitly admitted isolated DB.
#[tokio::test]
#[ignore="requires explicitly isolated PostgreSQL URL for read-only SQL fixture"]
async fn postgres_dispatch_compact_sql_fixture_matches_sqlite_for_nested_types_and_domain_evidence() {
    let url=std::env::var("COMMUNITYHERO_DISPATCH_READONLY_URL").expect("explicit isolated URL required");
    let expected=std::env::var("COMMUNITYHERO_DISPATCH_READONLY_DATABASE").expect("explicit database required");
    let options=url.parse::<sqlx::postgres::PgConnectOptions>().unwrap()
        .password(&std::env::var("PGPASSWORD").expect("transient password required"));
    let pool=PgPoolOptions::new().max_connections(1).after_connect(|connection,_|Box::pin(async move {
        sqlx::query("SET default_transaction_read_only=on").execute(&mut *connection).await?;
        sqlx::query("SET statement_timeout='5s'").execute(connection).await?; Ok(())
    })).connect_with(options).await.unwrap_or_else(|_|panic!("isolated read connection failed"));
    let (database,readonly):(String,String)=sqlx::query_as("SELECT current_database(),current_setting('default_transaction_read_only')").fetch_one(&pool).await.unwrap();
    assert_eq!(database,expected); assert_eq!(readonly,"on");
    let (db,_folder)=sqlite().await;
    let Database::Sqlite(sqlite_pool)=&db else {unreachable!()};
    let (data,_,_)=fixture(crate::accounts::Profile::BawRussia);
    for table in ["operations","jobs"] {
        let pg=format!("SELECT ({})::text",compact_payload_sql(table,"$1::jsonb",true,0));
        let sqlite=format!("SELECT {}",compact_payload_sql(table,"?1",false,0));
        let values=data[table].as_array().unwrap().iter().cloned().chain([
            json!({"id":"types","kind":[],"status":true,"target":{"connectorBinding":{"n":2,"b":false,"null":null}},
                "action":{"action":null,"conversationKey":"[1]"},"result":{"visualProgress":{"sourceIdentity":[true,null],"source":{"bytes":1024}}}}),
            json!({"id":"absent"}),json!({"id":"null","result":null,"target":null}),
            json!({"id":"scalar","result":"{\"visualProgress\":1}","target":true})]);
        for value in values {
            let pg_raw:String=sqlx::query_scalar(sqlx::AssertSqlSafe(pg.as_str())).bind(value.to_string()).fetch_one(&pool).await.unwrap();
            let sqlite_raw:String=sqlx::query_scalar(sqlx::AssertSqlSafe(sqlite.as_str())).bind(value.to_string()).fetch_one(sqlite_pool).await.unwrap();
            assert_eq!(parse(&pg_raw).unwrap(),parse(&sqlite_raw).unwrap(),"{table}");
        }
    }
    db.close().await; pool.close().await;
}
