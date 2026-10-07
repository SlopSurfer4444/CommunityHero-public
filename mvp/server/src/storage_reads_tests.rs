use super::*;
use serde_json::json;

fn dispatch_fixture(profile: crate::accounts::Profile, recovered: bool) -> (Value, String) {
    let mut d = crate::empty();
    d["account"] = json!(profile.display());
    d["connectorBinding"] = profile.binding();
    d["posts"] = json!([{"id":"post","postKey":"object:post","text":"Synthetic publication"},
        {"id":"older-post","postKey":"object:older","text":"Earlier publication"}]);
    d["branches"] = json!([{"id":"branch","postId":"post","messages":[{"id":"message","role":"customer","text":"Question"}]},
        {"id":"older-branch","postId":"older-post","messages":[]}]);
    let item = json!({"id":"target","itemId":"external-target","objectId":"object","postId":"post","branchId":"branch",
        "postKey":"object:post","conversationKey":"object:thread","connectorBinding":profile.binding(),"platform":"vk",
        "authorId":"synthetic-author","text":"Question","createdAt":"2026-01-02T00:00:00Z","revision":1,"workflow":"attention",
        "providerStatus":"new","draft":"","contextEvidenceDigest":"a".repeat(64),"branchContextDigest":"b".repeat(64)});
    let mut older=item.clone();
    older["id"]=json!("older");older["itemId"]=json!("external-older");older["postId"]=json!("older-post");older["branchId"]=json!("older-branch");
    older["postKey"]=json!("object:older");older["text"]=json!("Earlier customer statement");older["createdAt"]=json!("2026-01-01T00:00:00Z");
    d["items"]=json!([item,older]);
    crate::knowledge::sync_catalog(&mut d,"2026-01-01T00:00:00Z").unwrap();
    crate::knowledge::save_instruction(&mut d,&json!({"requestId":"rule","title":"Synthetic rule","text":"Preserve uncertainty"}),"2026-01-01T00:00:00Z").unwrap();
    let p=crate::create_proposal(&mut d,&json!({"itemId":"target","kind":"close","expectedRevision":1})).unwrap();
    let key=p["id"].as_str().unwrap().to_owned();
    let ids=if recovered {json!(["target"])} else {json!(["target","older"])};
    let bundle=crate::prepare_bundle::build(&d,ids.as_array().unwrap(),&[]).unwrap();
    assert!(bundle["request"]["customerCases"].as_array().is_some_and(|v|!v.is_empty()));
    let p=crate::row_mut(&mut d,"proposals",&key).unwrap();
    if recovered {
        p["recovery"]=json!({"kind":"recovered_context_metadata","prepareRunId":"recovery-job","prepareBundleId":bundle["id"],"prepareBundleDigest":bundle["digest"]});
    } else {
        p["prepareRunId"]=json!("prepare-job");p["prepareBundleId"]=bundle["id"].clone();p["prepareBundleDigest"]=bundle["digest"].clone();
    }
    d["jobs"]=json!([{"id":if recovered {"recovery-job"} else {"prepare-job"},"kind":"assistant","status":"completed","prepareBundle":bundle},
        {"id":"unrelated-job","kind":"assistant","status":"completed","result":{"private":"UNRELATED_JOB".repeat(100_000)}}]);
    d["conversations"]=json!([{"id":"unrelated-chat","messages":[{"text":"PRIVATE_CHAT".repeat(100_000)}]}]);
    d["approvals"]=json!([{"id":"approval","private":"OMITTED_APPROVAL"}]);
    d["audit"]=json!([{"id":"audit","private":"OMITTED_AUDIT"}]);
    d["feedback"]=json!([{"id":"feedback","private":"OMITTED_FEEDBACK"}]);
    d["proposals"].as_array_mut().unwrap().push(json!({"id":"other-proposal","prepareRunId":"unrelated-job"}));
    d["operations"]=json!([{"id":"unknown-reply","status":"unknown","target":{"connectorBinding":profile.binding()},"action":{"action":"reply_and_close","conversationKey":"object:thread"}}]);
    assert!(crate::proposal_current(&d,crate::row(&d,"proposals",&key).unwrap()).is_ok());
    (d,key)
}

fn dispatch_result(d:&Value,key:&str)->Result<Value,(u16,String)> {
    crate::row(d,"proposals",key).and_then(|p|crate::proposal_current(d,p)).map_err(|e|(e.0.as_u16(),e.1))
}

#[tokio::test]
async fn dispatch_projection_preserves_multi_item_recovery_rules_and_customer_history() {
    let (db,_folder)=sqlite().await;
    for profile in [crate::accounts::Profile::LikeAvto,crate::accounts::Profile::BawRussia] {
        for recovered in [false,true] {
            let (d,key)=dispatch_fixture(profile,recovered);seed_read_fixture(&db,&d).await;
            let view=db.read_dispatch_context(&key).await.unwrap();
            assert_eq!(dispatch_result(&view,&key),dispatch_result(&d,&key));
            for name in ["posts","branches","items","operations","materials","knowledge_entries","knowledge_versions"] {assert_eq!(view[name],d[name],"{name}");}
            assert_eq!(view["proposals"].as_array().unwrap().len(),1);
            assert_eq!(view["jobs"],json!([d["jobs"][0]]));
            for name in ["conversations","approvals","audit","feedback"] {assert_eq!(view[name],json!([]));}
            let op=json!({"id":"next","target":{"connectorBinding":profile.binding()},"action":{"action":"reply_and_close","conversationKey":"object:thread"}});
            assert_eq!(crate::dispatch_evidence::conversation_blocker(&view,&op),crate::dispatch_evidence::conversation_blocker(&d,&op));
            assert_eq!(crate::prepare_bundle::review_fingerprint(&view,"target"),crate::prepare_bundle::review_fingerprint(&d,"target"));
            assert!(view.to_string().len()*20<d.to_string().len());
            assert!(!view.to_string().contains("UNRELATED_JOB"));
            assert!(db.read_dispatch_context("missing' OR 1=1 --").await.unwrap()["proposals"].as_array().unwrap().is_empty());
        }
    }
    db.close().await;
}

#[tokio::test]
async fn dispatch_projection_retains_exact_editorial_operation_marker() {
    let (db,_folder)=sqlite().await;
    let (mut d,key)=dispatch_fixture(crate::accounts::Profile::LikeAvto,false);
    d["operations"]=json!([]);
    d["posts"][0]["text"]=json!("Updated publication reviewed after generation");
    // A fresh editorial capture needs proven attachment metadata. This is a
    // synthetic text-only post, not an exemption for a legacy saved request.
    d["posts"][0]["attachments"]=json!([]);
    crate::editorial_review::fixture_accept(&mut d,&key).unwrap();
    assert!(crate::proposal_current(&d,crate::row(&d,"proposals",&key).unwrap()).is_ok());
    let receipt=d["proposals"][0]["editorialReview"]["receiptSha256"].clone();
    let op=json!({"id":"reviewed-operation","proposalId":key,"itemId":"target",
        "target":d["items"][0],"approvedEditorialReceiptSha256":receipt});
    d["operations"]=json!([op.clone()]);
    assert!(crate::dispatch_diagnostics::local_check(&d,&op).is_ok());
    seed_read_fixture(&db,&d).await;
    let view=db.read_dispatch_context(&key).await.unwrap();
    assert_eq!(view["approvals"],json!([]));
    assert_eq!(view["operations"][0]["approvedEditorialReceiptSha256"],receipt);
    let native=d["proposals"][0]["editorialModelMaterialReceipt"]["nativeJobId"].as_str().unwrap();
    assert_eq!(crate::row(&view,"jobs",native).unwrap(),crate::row(&d,"jobs",native).unwrap(),"exact full native editorial capture/history retained");
    assert!(crate::dispatch_diagnostics::local_check(&view,&op).is_ok());
    db.close().await;
}

#[tokio::test]
async fn dispatch_material_native_parent_child_duplicate_bundle_and_forgery_parity(){
    let(db,_folder)=sqlite().await;
    let(mut d,batch,result,native)=super::super::hot_admission::tests::native_editorial_material_fixture();
    crate::editorial_review::admit(&mut d,&batch,&result,"2026-10-06T00:00:01Z").unwrap();
    let key=d["proposals"][0]["id"].as_str().unwrap().to_owned();
    crate::row_mut(&mut d,"jobs",&native).unwrap()["nativeSourceOriginJobId"]=json!("dispatch-source-parent");
    for id in ["dispatch-source-parent","dispatch-duplicate-family"]{
        crate::list_mut(&mut d,"jobs").push(json!({"id":id,"kind":"material_acquisition","status":"completed",
            "prepareBundle":{"id":"dispatch-shared-bundle"},"proofHistory":{"retained":"exact whole body"}}));
    }
    d["operations"]=json!([{"id":"historic-unknown","itemId":"i0","proposalId":key,"status":"unknown",
        "target":{"connectorBinding":d["connectorBinding"]},"action":{"action":"reply_and_close","conversationKey":"other-thread"}}]);
    assert!(dispatch_result(&d,&key).is_ok(),"genuine new native proof is current before projection");
    seed_read_fixture(&db,&d).await;let view=db.read_dispatch_context(&key).await.unwrap();
    assert_eq!(dispatch_result(&view,&key),dispatch_result(&d,&key));
    for id in [native.as_str(),"dispatch-source-parent","dispatch-duplicate-family"]{
        assert_eq!(crate::row(&view,"jobs",id).unwrap(),crate::row(&d,"jobs",id).unwrap(),"complete {id}");
    }
    assert!(crate::row(&view,"jobs","cold-editorial-history").is_err(),"unrelated archive excluded");
    assert_eq!(view["operations"][0]["status"],"unknown");assert_eq!(view["operations"][0]["action"]["conversationKey"],"other-thread");
    for mutation in ["missing_job","wrong_job_kind","missing_history","missing_paid_capture","foreign_company","changed_cas_ref"]{
        let mut forged=d.clone();
        match mutation{
            "missing_job"=>forged["jobs"].as_array_mut().unwrap().retain(|j|j["id"]!=native),
            "wrong_job_kind"=>crate::row_mut(&mut forged,"jobs",&native).unwrap()["kind"]=json!("assistant"),
            "missing_history"=>crate::row_mut(&mut forged,"jobs",&native).unwrap()["modelMaterialReceipts"]=json!([]),
            "missing_paid_capture"=>crate::row_mut(&mut forged,"jobs",&native).unwrap()["retainedEvidence"]=json!([]),
            "foreign_company"=>forged["proposals"][0]["editorialModelMaterialReceipt"]["companyId"]=json!("BAW Russia"),
            _=>forged["proposals"][0]["editorialModelMaterialReceipt"]["paidResultRef"]["artifact"]["sha256"]=json!("0".repeat(64)),
        }
        if matches!(mutation,"foreign_company"|"changed_cas_ref"){
            let pointer=&mut forged["proposals"][0]["editorialModelMaterialReceipt"];
            pointer.as_object_mut().unwrap().remove("pointerSha256");pointer["pointerSha256"]=json!(crate::preparation_materials::hash(pointer));
        }
        let expected=dispatch_result(&forged,&key);assert!(expected.is_err(),"full native guard rejects {mutation}");
        seed_read_fixture(&db,&forged).await;let actual=db.read_dispatch_context(&key).await.unwrap();
        assert_eq!(dispatch_result(&actual,&key),expected,"no omitted-parent authority for {mutation}");
    }
    db.close().await;
}

#[tokio::test]
async fn dispatch_projection_retains_media_duration_jobs_for_video_bundle_freshness() {
    let (db,_folder)=sqlite().await;
    let (mut d,key)=dispatch_fixture(crate::accounts::Profile::BawRussia,false);
    d["posts"][0]["attachments"]=json!([{"type":"video"}]);
    let post=d["posts"][0].clone();
    let binding=d["connectorBinding"].clone();
    let source=crate::media_fullframes::source_version(&post,"BAW Russia");
    let mut progress=crate::media_fullframes::initial("BAW Russia",&binding,&post,"now");
    progress["phase"]=json!("inventory");
    progress["source"]=json!({"sha256":"a".repeat(64),"bytes":1024});
    progress["sourceIdentity"]=json!({"account":"BAW Russia","postKey":post["postKey"],
        "mediaSha256":"a".repeat(64),"durationMs":181000});
    d["jobs"].as_array_mut().unwrap().push(json!({"id":"duration-job","kind":"media",
        "purpose":"auto_media","status":"completed","visualContractVersion":2,
        "account":"BAW Russia","connectorBinding":binding,"refId":post["id"],
        "createdAt":"2026-01-02T00:00:00Z","result":{"visualProgress":progress}}));
    d["jobs"].as_array_mut().unwrap().push(json!({"id":"other-media-job","kind":"media",
        "purpose":"auto_media","visualContractVersion":2,"refId":"older-post"}));
    d["jobs"].as_array_mut().unwrap().push(json!({"id":"legacy-media-job","kind":"media",
        "purpose":"auto_media","visualContractVersion":1,"refId":"post"}));
    d["materials"].as_array_mut().unwrap().push(json!({"id":"video-speech","account":"BAW Russia","postKey":"object:post",
        "kind":"transcript","text":"Complete spoken source",
        "transcription":{"partial":false,"coverage":"full_audio","sourceVersion":source,
            "mediaDurationSeconds":181.0,"audioDurationSeconds":181.0}}));
    crate::knowledge::sync_catalog(&mut d,"2026-01-02T00:00:00Z").unwrap();
    let bundle=crate::prepare_bundle::build(&d,&[json!("target"),json!("older")],&[]).unwrap();
    let video_post=bundle["request"]["posts"].as_array().unwrap().iter()
        .find(|p|p["id"]=="post").unwrap();
    assert_eq!(video_post["mediaPolicy"]["decisionBasis"]["durationMs"],181000);
    d["jobs"][0]["prepareBundle"]=bundle.clone();
    let review_digest=crate::prepare_bundle::review_fingerprint(&d,"target").unwrap();
    let proposal=crate::row_mut(&mut d,"proposals",&key).unwrap();
    proposal["prepareBundleId"]=bundle["id"].clone();
    proposal["prepareBundleDigest"]=bundle["digest"].clone();
    proposal["reviewContextDigest"]=json!(review_digest);
    assert!(dispatch_result(&d,&key).is_ok(),"{:?}",dispatch_result(&d,&key));

    let mut missing_media=d.clone();
    missing_media["jobs"].as_array_mut().unwrap().retain(|j|j["kind"]!="media");
    assert!(dispatch_result(&missing_media,&key).is_err(),
        "the former projection lost duration evidence and invalidated an unchanged bundle");

    seed_read_fixture(&db,&d).await;
    let view=db.read_dispatch_context(&key).await.unwrap();
    assert_eq!(dispatch_result(&view,&key),dispatch_result(&d,&key));
    assert_eq!(crate::prepare_bundle::review_fingerprint(&view,"target"),
        crate::prepare_bundle::review_fingerprint(&d,"target"));
    assert_eq!(view["jobs"].as_array().unwrap().iter().map(|j|j["id"].as_str().unwrap())
        .collect::<Vec<_>>(),vec!["prepare-job","duration-job","other-media-job"]);

    let mut short=d.clone();
    short["jobs"][2]["result"]["visualProgress"]["sourceIdentity"]["durationMs"]=json!(45662);
    let short_bundle=crate::prepare_bundle::build(&short,&[json!("target"),json!("older")],&[]).unwrap();
    let short_post=short_bundle["request"]["posts"].as_array().unwrap().iter()
        .find(|p|p["id"]=="post").unwrap();
    assert_eq!(short_post["mediaPolicy"]["mode"],"full_audio_only",
        "default text acquisition keeps full audio for short sources too; duration remains fingerprinted");
    assert_eq!(short_post["mediaPolicy"]["decisionBasis"]["durationMs"],45662);
    assert!(crate::prepare_bundle::current(&short,&short_bundle).is_ok());
    let mut short_without_media=short.clone();
    short_without_media["jobs"].as_array_mut().unwrap().retain(|j|j["kind"]!="media");
    assert_eq!(crate::prepare_bundle::current(&short_without_media,&short_bundle),
        Err("Preparation evidence changed; prepare again"),
        "duration remains in the fingerprint even when the selected mode stays the same");
    short["jobs"][0]["prepareBundle"]=short_bundle;
    seed_read_fixture(&db,&short).await;
    let short_view=db.read_dispatch_context(&key).await.unwrap();
    assert!(crate::prepare_bundle::current(&short_view,&short["jobs"][0]["prepareBundle"]).is_ok());

    d["branches"][0]["messages"][0]["text"]=json!("Changed customer statement");
    seed_read_fixture(&db,&d).await;
    let stale=db.read_dispatch_context(&key).await.unwrap();
    assert!(dispatch_result(&d,&key).is_err());
    assert_eq!(dispatch_result(&stale,&key),dispatch_result(&d,&key),
        "a real source change must still fail in the bounded dispatch context");
    db.close().await;
}

#[tokio::test]
async fn dispatch_projection_keeps_stale_evidence_and_account_failures_closed() {
    let (db,_folder)=sqlite().await;
    for recovered in [false,true] {
        let (baseline,key)=dispatch_fixture(crate::accounts::Profile::BawRussia,recovered);
        for mode in 0..9 {
            let mut d=baseline.clone();
            match mode {
                0=>d["items"][0]["revision"]=json!(99),
                1=>d["items"][0]["contextEvidenceDigest"]=json!("c".repeat(64)),
                2=>d["branches"][0]["messages"][0]["text"]=json!("Changed branch"),
                3=>d["posts"][0]["text"]=json!("Changed publication"),
                4=>d["items"][1]["text"]=json!("Changed cross-post customer history"),
                5=>{crate::knowledge::save_instruction(&mut d,&json!({"requestId":"new-rule","title":"New rule","text":"New reviewed condition"}),"2026-01-03T00:00:00Z").unwrap();},
                6=>d["knowledge_versions"][0]["hash"]=json!("tampered"),
                7=>d["connectorBinding"]=crate::accounts::Profile::LikeAvto.binding(),
                _=>d["jobs"][0]["prepareBundle"]["digest"]=json!("tampered")
            }
            seed_read_fixture(&db,&d).await;
            let view=db.read_dispatch_context(&key).await.unwrap();
            assert!(dispatch_result(&d,&key).is_err(),"{mode}/{recovered}");
            assert_eq!(dispatch_result(&view,&key),dispatch_result(&d,&key),"{mode}/{recovered}");
        }
    }
    db.close().await;
}

#[tokio::test]
async fn dispatch_projection_rejects_invalid_retained_source_and_normalizes_legacy_catalog() {
    let (db,_folder)=sqlite().await;
    let (baseline,key)=dispatch_fixture(crate::accounts::Profile::LikeAvto,false);
    for mode in 0..6 {
        let mut d=baseline.clone();
        match mode {
            0=>d["items"]=json!({"not":"a collection"}),
            1=>d["branches"][0]["postId"]=json!("missing"),
            2=>d["items"][0]["branchId"]=json!("missing"),
            3=>d["knowledge_entries"][0]["currentVersionId"]=json!("missing"),
            4=>{let duplicate=d["items"][0].clone();d["items"].as_array_mut().unwrap().push(duplicate);},
            _=>d["operations"][0]["proposalId"]=json!({"invalid":"type"})
        }
        seed_read_fixture(&db,&d).await;
        assert!(db.read_dispatch_context(&key).await.is_err(),"{mode}");
    }
    let mut legacy=crate::empty();
    for name in ["knowledge_entries","knowledge_versions","feedback"] {legacy.as_object_mut().unwrap().remove(name);}
    seed_read_fixture(&db,&legacy).await;
    let projected=db.read_dispatch_context("absent").await.unwrap();
    let full=db.read().await.unwrap();
    for name in ["knowledge_entries","knowledge_versions","feedback"] {assert_eq!(projected[name],full[name]);}
    db.close().await;
}

#[tokio::test]
#[ignore = "explicit read-only existing-database parity probe; no owner lease or migrations"]
async fn postgres_dispatch_existing_readonly_parity_probe() {
    let url = std::env::var("COMMUNITYHERO_DISPATCH_READONLY_URL").expect("explicit read-only connection required");
    let expected_database = std::env::var("COMMUNITYHERO_DISPATCH_READONLY_DATABASE").expect("explicit database binding required");
    let options = url.parse::<sqlx::postgres::PgConnectOptions>()
        .unwrap_or_else(|_| panic!("invalid read-only connection options"))
        .password(&std::env::var("PGPASSWORD").expect("transient password required"));
    let pool = PgPoolOptions::new().max_connections(1)
        .after_connect(|connection, _| Box::pin(async move {
            sqlx::query("SET default_transaction_read_only = on").execute(&mut *connection).await?;
            sqlx::query("SET statement_timeout = '30s'").execute(connection).await?;
            Ok(())
        }))
        .connect_with(options).await.unwrap_or_else(|_| panic!("read-only connection failed"));
    let (database, readonly): (String, String) = sqlx::query_as("SELECT current_database(),current_setting('default_transaction_read_only')")
        .fetch_one(&pool).await.unwrap_or_else(|_| panic!("read-only identity check failed"));
    assert!(database == expected_database && readonly == "on", "read-only database binding mismatch");
    // Never use Database::postgres: that constructor acquires the writer lease.
    // Both fields share this read-only pool; only read APIs are called below.
    let db = Database::Postgres { writer: pool.clone(), reader: pool };
    let started = std::time::Instant::now();
    let before = db.read().await.unwrap_or_else(|_| panic!("full read failed"));
    let full_ms = started.elapsed().as_millis();
    let proposal = before["proposals"].as_array().expect("proposal collection").iter().rev()
        .find(|p| {
            let job_id = p["recovery"]["prepareRunId"].as_str().or_else(|| p["prepareRunId"].as_str());
            matches!(p["status"].as_str(), Some("succeeded" | "approved" | "draft" | "unknown"))
                && job_id.is_some_and(|id| before["jobs"].as_array().unwrap().iter()
                    .any(|j| j["id"] == id && j["status"] == "completed" && j["prepareBundle"].is_object()))
        }).expect("requires a proposal backed by a completed preparation");
    let key = proposal["id"].as_str().expect("proposal identity");
    let started = std::time::Instant::now();
    let projected = db.read_dispatch_context(key).await.unwrap_or_else(|_| panic!("dispatch projection read failed"));
    let projected_ms = started.elapsed().as_millis();
    let after = db.read().await.unwrap_or_else(|_| panic!("bracketing full read failed"));
    let expected_context = |full: &Value| {
        let p = full["proposals"].as_array().unwrap().iter().find(|p| p["id"] == key)
            .expect("selected proposal disappeared");
        let mut expected = full.clone();
        for table in ["conversations", "approvals", "audit", "feedback"] { expected[table] = json!([]); }
        expected["proposals"] = json!([p]);
        expected["jobs"] = Value::Array(full["jobs"].as_array().unwrap().iter().filter(|j| {
            j["id"].as_str().is_some_and(|id| Some(id) == p["prepareRunId"].as_str()
                || Some(id) == p["recovery"]["prepareRunId"].as_str())
                || (j["kind"]=="media" && j["purpose"]=="auto_media"
                    && j["visualContractVersion"]==2)
        }).cloned().collect());
        for table in ["operations", "jobs"] {
            expected[table] = Value::Array(expected[table].as_array().unwrap().iter()
                .map(|value| dispatch::compact_context_record(value, table)).collect());
        }
        expected
    };
    let expected = expected_context(&before);
    // Separate public calls each use their own consistent snapshot. Require
    // every retained byte to remain stable; unrelated job/audit churn is allowed.
    assert!(expected == expected_context(&after), "retained context changed across probe; parity inconclusive");
    assert!(projected == expected, "retained evidence/metadata or selected roots mismatch");
    let full_result = dispatch_result(&before, key);
    assert!(dispatch_result(&projected, key) == full_result, "proposal_current parity failed");
    let item_id = proposal["itemId"].as_str().expect("proposal item identity");
    assert!(crate::prepare_bundle::review_fingerprint(&projected, item_id)
        == crate::prepare_bundle::review_fingerprint(&before, item_id), "review fingerprint parity failed");
    let item = crate::row(&before, "items", item_id).unwrap();
    let operation = json!({"id":"dispatch-projection-readonly-parity-probe",
        "target":{"connectorBinding":item["connectorBinding"]},
        "action":{"action":"reply_and_close","conversationKey":item["conversationKey"]}});
    assert_eq!(crate::dispatch_evidence::conversation_blocker(&projected, &operation),
        crate::dispatch_evidence::conversation_blocker(&before, &operation), "UNKNOWN quarantine parity failed");
    println!("DISPATCH_READONLY_PARITY {}", json!({
        "retainedContextStable":true,"fullWorkspaceStable":before==after,"projectionEqual":true,"proposalCurrentEqual":true,"reviewFingerprintEqual":true,"conversationBlockerEqual":true,
        "proposalCurrentAccepted":full_result.is_ok(),"fullJobs":before["jobs"].as_array().unwrap().len(),
        "projectedJobs":projected["jobs"].as_array().unwrap().len(),"fullProposals":before["proposals"].as_array().unwrap().len(),
        "projectedProposals":projected["proposals"].as_array().unwrap().len(),
        "fullBytes":before.to_string().len(),"projectedBytes":projected.to_string().len(),
        "fullMs":full_ms,"projectedMs":projected_ms
    }));
    db.close().await;
}

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

#[tokio::test]
async fn engine_status_keeps_account_binding_and_current_counts() {
    let (db,_folder)=sqlite().await;
    let mut data=crate::empty();
    data["account"]=json!("BAW Russia");
    data["connectorBinding"]=crate::accounts::Profile::BawRussia.binding();
    data["items"]=json!([{"id":"one","workflow":"prepared"},{"id":"two","workflow":"attention"}]);
    data["operations"]=json!([{"id":"unknown","status":"unknown"},{"id":"active","status":"dispatching"}]);
    seed_read_fixture(&db,&data).await;
    let view=db.read_engine_status().await.unwrap();
    assert_eq!(crate::accounts::Profile::from_workspace(&view).unwrap(),crate::accounts::Profile::BawRussia);
    assert!(crate::active_binding(&view).is_ok());
    assert_eq!(view["items"],data["items"]);
    data["connectorBinding"]["accountId"]=json!("LikeAvto");
    seed_read_fixture(&db,&data).await;
    assert!(crate::active_binding(&db.read_engine_status().await.unwrap()).is_err());
    db.close().await;
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
            "prepareBundle":{"id":format!("bundle-{n}"),"request":{"hidden":"HIDDEN_REQUEST".repeat(300)},"dependencyDigest":"retain"},
            "editorialPlan":{"private":"HIDDEN_EDITORIAL_PLAN".repeat(300)},
            "editorialBatches":[{"private":"HIDDEN_EDITORIAL_BATCH".repeat(300)}],
            "result":{"summary":"retain","visualProgress":{"sourceProjection":{"private":"HIDDEN_VISUAL_SOURCE".repeat(300)},
                "leaseId":"HIDDEN_LEASE","materialEpoch":"HIDDEN_EPOCH","completed":5,"status":"running"}}}));
    }
    seed_read_fixture(&db,&data).await;
    let compact=db.read_bootstrap_source().await.unwrap();
    assert_eq!(crate::bootstrap_view(compact.clone(),"csrf"),crate::bootstrap_view(data.clone(),"csrf"));
    assert_eq!(compact["jobs"].as_array().unwrap().len(),220,"actor filtering must precede terminal history cap");
    assert_eq!(compact["conversations"],data["conversations"],"private chats must remain available to actor filtering");
    assert_eq!(compact["companyKnowledgeAuthority"],data["companyKnowledgeAuthority"]);
    let serialized=compact.to_string();
    for hidden in ["HIDDEN_AUTHORITY","HIDDEN_REQUEST","COVERAGE_PRIVATE","PRIVATE_OBSERVATION","OWNER_ONLY",
        "HIDDEN_EDITORIAL_PLAN","HIDDEN_EDITORIAL_BATCH","HIDDEN_VISUAL_SOURCE","HIDDEN_LEASE","HIDDEN_EPOCH"] {assert!(!serialized.contains(hidden),"{hidden}");}
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
                    "retryAt":value["retryAt"],"queuedAt":value["queuedAt"],"reason":value["reason"]
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
    assert_eq!(db.read_job_public("missing").await.unwrap(), None);
    assert_eq!(db.read_job_public("status' OR 1=1 --").await.unwrap(), None);
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
    assert_eq!(db.read_job_public("status").await.unwrap().unwrap()["status"],"completed");
    let Database::Sqlite(pool) = &db else {
        unreachable!()
    };
    sqlx::query("DELETE FROM workspace WHERE id=1")
        .execute(pool)
        .await
        .unwrap();
    assert!(db.read_job("missing").await.is_err());
    assert!(db.read_job_public("missing").await.is_err());
    assert!(db.read_schedule().await.is_err());
    assert!(db.readiness(Duration::from_secs(1)).await.is_err());
}

// Only the previous point endpoint's three omissions belong in the SQL
// projection. The bootstrap sanitizer hides additional visual fields.
fn expected_public_point_job(mut job:Value)->Value {
    if let Some(bundle)=job.get_mut("prepareBundle").and_then(Value::as_object_mut){bundle.remove("request");}
    if let Some(fields)=job.as_object_mut(){fields.remove("editorialPlan");fields.remove("editorialBatches");}
    job
}

#[tokio::test]
async fn public_point_job_omits_cold_bytes_at_sql_boundary_and_preserves_durable_ready_evidence() {
    let (db,_folder)=sqlite().await;
    let job=json!({"id":"preparation","kind":"assistant","purpose":"engine_prepare","status":"completed","refId":"recipient",
        "operatorId":"operator","scopeReservation":{"version":1,"ownerJobId":"preparation","keysDigest":"proof"},
        "prepareBundle":{"id":"bundle","digest":"capture","dependencyDigest":"dependencies","request":{"cold":"R".repeat(2_400_000)}},
        "editorialPlan":{"private":"P".repeat(20_000)},"editorialBatches":[{"private":"B".repeat(10_000)}],
        "preparationStages":{"groupAdmission":[{"itemIds":["recipient"],"status":"admitted","admission":{"candidates":[{"itemId":"recipient","proposalId":"proposal","status":"review"}]}}]},
        "result":{"candidates":[{"proposalId":"proposal"}],"visualProgress":{"sourceProjection":{"retain":"point endpoint preserves this"},"leaseId":"retain","materialEpoch":"retain","completed":3}},
        "error":null,"finishedAt":null});
    let mut data=crate::empty();data["jobs"]=json!([job.clone()]);
    data["jobs"].as_array_mut().unwrap().extend((0..201).map(|n|json!({"id":format!("later-{n}"),"kind":"assistant","status":"completed"})));
    assert!(!crate::bootstrap_view(data.clone(),"csrf")["jobs"].as_array().unwrap().iter().any(|j|j["id"]=="preparation"),
        "terminal point fixture must be outside bootstrap's display history");
    seed_read_fixture(&db,&data).await;
    let Database::Sqlite(pool)=&db else {unreachable!()};
    let stored_before:String=sqlx::query_scalar("SELECT payload FROM workspace WHERE id=1").fetch_one(pool).await.unwrap();
    // Exercise the exact production query before serde decoding, measuring
    // SQL-returned text rather than a later sanitized Rust serialization.
    let full:String=sqlx::query_scalar::<_,Option<String>>(SQLITE_JOB).bind("preparation").bind(false)
        .fetch_one(pool).await.unwrap().unwrap();
    let public:String=sqlx::query_scalar::<_,Option<String>>(SQLITE_JOB).bind("preparation").bind(true)
        .fetch_one(pool).await.unwrap().unwrap();
    assert_eq!(parse(&public).unwrap(),expected_public_point_job(job.clone()));
    assert!(full.len()-public.len()>=2_430_000);assert!(public.len()<2_000);
    assert_eq!(db.read_job_public("preparation").await.unwrap(),Some(expected_public_point_job(job.clone())));
    assert!(db.read_job("preparation").await.unwrap()==Some(job),"complete internal job evidence must remain exact");
    let stored_after:String=sqlx::query_scalar("SELECT payload FROM workspace WHERE id=1").fetch_one(pool).await.unwrap();
    assert!(stored_before==stored_after,"read projection never changes saved evidence");
    println!("PUBLIC_POINT_JOB_SQL_BYTES {}",json!({"fullBytes":full.len(),"publicBytes":public.len(),"removedBytes":full.len()-public.len()}));
    db.close().await;
}

#[tokio::test]
async fn public_point_job_matches_previous_endpoint_for_absent_and_nonobject_bundles() {
    let (app,_folder)=crate::tests::test_app().await;
    for bundle in [None,Some(Value::Null),Some(json!("text")),Some(json!(17)),Some(json!(true)),
        Some(json!([{"request":"array member must remain"}])),Some(json!({})),Some(json!({"request":null,"digest":"retain"}))] {
        let mut job=json!({"id":"malformed","kind":"execute","status":"completed","refId":"approval",
            "editorialPlan":null,"editorialBatches":[],"result":{"status":"unknown"},"custom":{"request":"retain"}});
        if let Some(bundle)=bundle {job["prepareBundle"]=bundle;}
        let mut data=crate::empty();data["jobs"]=json!([job.clone()]);seed_read_fixture(&app.db,&data).await;
        let expected=expected_public_point_job(job.clone());
        assert_eq!(app.db.read_job_public("malformed").await.unwrap(),Some(expected.clone()));
        // The old endpoint's mutable indexing also inserts an absent bundle as
        // null. Its retained defensive sanitizer preserves that HTTP behavior.
        let mut endpoint_expected=expected;
        let _=endpoint_expected["prepareBundle"].as_object_mut();
        assert_eq!(crate::engine_api::job(axum::extract::State(app.clone()),axum::extract::Path("malformed".into())).await.unwrap().0,endpoint_expected);
        assert_eq!(app.db.read_job("malformed").await.unwrap(),Some(job));
    }
    let Database::Sqlite(pool)=&app.db else {unreachable!()};
    sqlx::query("UPDATE workspace SET payload='invalid JSON' WHERE id=1").execute(pool).await.unwrap();
    assert!(app.db.read_job("malformed").await.is_err());
    assert!(app.db.read_job_public("malformed").await.is_err());
    app.db.close().await;
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
    if let Some(job)=job.as_ref(){
        assert_eq!(db.read_job_public(job["id"].as_str().unwrap()).await.unwrap(),Some(expected_public_point_job(job.clone())));
    }
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

#[test]
fn bootstrap_text_merge_preserves_right_biased_metadata_and_rejects_nonobjects(){
    let metadata=json!({"account":"LikeAvto","sync":{"keep":true},"future":{"unicode":"雪","number":1.062437807104632},"jobs":[{"stale":true}]});
    let entities=json!({"jobs":[{"id":"current","unknown":[true,null,"🦀"]}],"branches":[]});
    let merged=merge_bootstrap_projection(&metadata.to_string(),&entities.to_string()).unwrap();
    assert_eq!(merged,json!({"account":"LikeAvto","sync":{"keep":true},"future":metadata["future"],"jobs":entities["jobs"],"branches":[]}));
    for (metadata,entities) in [("null","{}"),("[]","{}"),("{}","[]"),("{}","null"),("{bad","{}")]{
        assert!(merge_bootstrap_projection(metadata,entities).is_err());
    }
}

fn large_projection_fixture()->Value{
    let mut d=crate::empty();normalize(&mut d);
    d["posts"]=json!([{"id":"p","postKey":"synthetic:p"}]);
    d["branches"]=json!([{"id":"b","postId":"p","messages":[],"observedMessages":[{"text":"HIDDEN_OBSERVATION"}]}]);
    d["items"]=json!([{"id":"i","postId":"p","branchId":"b","workflow":"prepared","draft":"Retained draft","revision":1}]);
    d["conversations"]=json!([{"id":"c1","operatorId":"one","messages":[{"text":"Retained private one"}]},{"id":"c2","operatorId":"two","messages":[{"text":"Retained private two"}]}]);
    d["jobs"]=json!([{"id":"job-one","kind":"assistant","status":"completed","refId":"c1","prepareBundle":{"request":{"private":"HIDDEN_REQUEST"}},"editorialPlan":"HIDDEN_PLAN","editorialBatches":["HIDDEN_BATCH"],"result":{"visible":"Retain visible","visualProgress":{"sourceProjection":"HIDDEN_SOURCE","leaseId":"HIDDEN_LEASE","materialEpoch":"HIDDEN_EPOCH","status":"completed"}}},
        {"id":"job-two","kind":"assistant","status":"completed","refId":"c2","result":{"visible":"Retain other actor"}}]);
    d["feedback"]=json!([{"id":"feedback-base","itemId":"i","kind":"operator_note","text":"OWNER_FEEDBACK","unknown":{"number":1.062437807104632,"values":[null,true,"雪"]}}]);
    d["companyKnowledgeCoverage"]=json!({"private":"HIDDEN_COVERAGE"});
    d["futureMetadata"]=json!({"retain":[null,true,"雪"],"number":1.062437807104632});
    crate::knowledge::sync_catalog(&mut d,"2026-10-01T00:00:00Z").unwrap();
    crate::knowledge::save_instruction(&mut d,&json!({"requestId":"rule-head","title":"Exact rule 雪","text":"Preserve uncertainty and exact content"}),"2026-10-01T00:00:00Z").unwrap();
    d
}

// Opt-in because a real regression requires crossing PostgreSQL's 256 MiB
// JSONB container boundary. Every row remains small and valid; this is not a
// production clone, a timing benchmark or permission to execute by default.
#[tokio::test]
#[ignore="requires a pristine explicitly isolated communityhero_writer_v51_test_ PostgreSQL fixture; allocates >256 MiB response data"]
async fn postgres_large_catalog_and_bootstrap_text_projection_preserve_contract(){
    let db=super::super::preparation::writer_v51_fixture_db().await;
    db.change(|d|{*d=large_projection_fixture();Ok(())}).await.unwrap();
    let small=db.read().await.unwrap();
    assert_eq!(db.read_knowledge_catalog(true).await.unwrap(),json!({"entries":small["knowledge_entries"],"versions":small["knowledge_versions"],"feedback":small["feedback"]}));
    assert_eq!(db.read_knowledge_catalog(false).await.unwrap(),json!({"entries":small["knowledge_entries"],"versions":small["knowledge_versions"]}));
    let projected=db.read_bootstrap_source().await.unwrap();
    assert_eq!(crate::bootstrap_view(projected.clone(),"csrf"),crate::bootstrap_view(small.clone(),"csrf"));
    for hidden in ["HIDDEN_OBSERVATION","HIDDEN_REQUEST","HIDDEN_PLAN","HIDDEN_BATCH","HIDDEN_SOURCE","HIDDEN_LEASE","HIDDEN_EPOCH","HIDDEN_COVERAGE","OWNER_FEEDBACK"]{
        assert!(!projected.to_string().contains(hidden),"{hidden}");
    }
    drop(projected);
    let Database::Postgres{writer,reader,..}=&db else{unreachable!()};
    const CHUNK:i32=16*1024*1024;
    sqlx::query("INSERT INTO communityhero.feedback(workspace_id,id,item_id,ordinal,payload) SELECT $1,'large-feedback-'||n,'i',n,jsonb_build_object('id','large-feedback-'||n,'itemId','i','kind','operator_note','text',repeat('F',$2),'unknown',jsonb_build_object('unicode','雪','values',jsonb_build_array(null,true,'🦀'))) FROM generate_series(1,17) n")
        .bind(WORKSPACE).bind(CHUNK).execute(writer).await.unwrap();
    let legacy_error=sqlx::query_scalar::<_,String>("SELECT jsonb_agg(payload ORDER BY ordinal)::text FROM communityhero.feedback WHERE workspace_id=$1")
        .bind(WORKSPACE).fetch_one(reader).await.unwrap_err();
    assert!(matches!(legacy_error,sqlx::Error::Database(ref e) if e.code().as_deref()==Some("54000")),"fixture must exercise the actual old feedback aggregation failure");
    let operator=db.read_knowledge_catalog(false).await.unwrap();
    assert_eq!(operator,json!({"entries":small["knowledge_entries"],"versions":small["knowledge_versions"]}));
    drop(operator);
    {
        let owner=db.read_knowledge_catalog(true).await.unwrap();
        assert_eq!(owner["entries"],small["knowledge_entries"]);assert_eq!(owner["versions"],small["knowledge_versions"]);
        let feedback=owner["feedback"].as_array().unwrap();assert_eq!(feedback.len(),18);assert_eq!(feedback[0],small["feedback"][0]);
        let expected="F".repeat(CHUNK as usize);
        for (n,event) in feedback.iter().enumerate().skip(1){
            assert_eq!(event["id"],format!("large-feedback-{n}"));assert_eq!(event["itemId"],"i");assert_eq!(event["text"].as_str(),Some(expected.as_str()));
            assert_eq!(event["unknown"],json!({"unicode":"雪","values":[null,true,"🦀"]}));
        }
    }
    // Each collection stays below 256 MiB; their combined object crosses the
    // old bootstrap limit, matching the independently confirmed live defect.
    sqlx::query("INSERT INTO communityhero.branches(workspace_id,id,post_id,ordinal,payload) SELECT $1,'large-branch-'||n,'p',n,jsonb_build_object('id','large-branch-'||n,'postId','p','publicEvidence',repeat('B',$2),'messages',jsonb_build_array(),'observedMessages',jsonb_build_array('HIDDEN_OBSERVATION')) FROM generate_series(1,9) n")
        .bind(WORKSPACE).bind(CHUNK).execute(writer).await.unwrap();
    sqlx::query("INSERT INTO communityhero.jobs(workspace_id,id,kind,status,ref_id,ordinal,payload) SELECT $1,'large-job-'||n,'assistant','completed','c1',n+1,jsonb_build_object('id','large-job-'||n,'kind','assistant','status','completed','refId','c1','result',jsonb_build_object('visible',repeat('J',$2)),'prepareBundle',jsonb_build_object('request','HIDDEN_REQUEST'),'editorialPlan','HIDDEN_PLAN','editorialBatches',jsonb_build_array('HIDDEN_BATCH')) FROM generate_series(1,9) n")
        .bind(WORKSPACE).bind(CHUNK).execute(writer).await.unwrap();
    let legacy_error=sqlx::query_scalar::<_,String>("SELECT jsonb_build_object('jobs',(SELECT jsonb_agg(payload ORDER BY ordinal) FROM communityhero.jobs WHERE workspace_id=$1),'branches',(SELECT jsonb_agg(payload ORDER BY ordinal) FROM communityhero.branches WHERE workspace_id=$1))::text")
        .bind(WORKSPACE).fetch_one(reader).await.unwrap_err();
    assert!(matches!(legacy_error,sqlx::Error::Database(ref e) if e.code().as_deref()==Some("54000") && e.message().contains("object elements")),"fixture must exercise combined-object failure, not an oversized individual array");
    {
        let bootstrap=db.read_bootstrap_source().await.unwrap();
        assert_eq!(bootstrap["futureMetadata"],small["futureMetadata"]);assert_eq!(bootstrap["conversations"],small["conversations"]);
        assert!(bootstrap.get("companyKnowledgeCoverage").is_none());assert!(bootstrap.get("feedback").is_none());
        assert_eq!(bootstrap["items"],small["items"]);assert_eq!(bootstrap["posts"],small["posts"]);
        let branches=bootstrap["branches"].as_array().unwrap();assert_eq!(branches.len(),10);
        let jobs=bootstrap["jobs"].as_array().unwrap();assert_eq!(jobs.len(),11,"all actors/jobs retained until HTTP filtering");
        let expected_branch="B".repeat(CHUNK as usize);let expected_job="J".repeat(CHUNK as usize);
        for n in 1..=9{
            assert_eq!(branches[n]["id"],format!("large-branch-{n}"));assert_eq!(branches[n]["publicEvidence"].as_str(),Some(expected_branch.as_str()));assert!(branches[n].get("observedMessages").is_none());
            assert_eq!(jobs[n+1]["id"],format!("large-job-{n}"));assert_eq!(jobs[n+1]["result"]["visible"].as_str(),Some(expected_job.as_str()));
            assert!(jobs[n+1].get("editorialPlan").is_none());assert!(jobs[n+1].get("editorialBatches").is_none());assert!(jobs[n+1]["prepareBundle"].get("request").is_none());
        }
    }
    let count:i64=sqlx::query_scalar("SELECT count(*) FROM communityhero.feedback WHERE workspace_id=$1").bind(WORKSPACE).fetch_one(reader).await.unwrap();assert_eq!(count,18,"read projections must not alter history");
    println!("LARGE_READ_PROJECTION {}",json!({"feedbackPayloadBytes":17_i64*i64::from(CHUNK),"eachBootstrapCollectionBytes":9_i64*i64::from(CHUNK),"combinedBootstrapPayloadBytes":18_i64*i64::from(CHUNK),"legacyFeedback54000":true,"legacyBootstrapObject54000":true,"operatorFeedbackOmitted":true,"orderingAndExactContent":true}));
    db.close().await;
}

fn bounded_knowledge_fixture(count:usize)->Value {
    let mut data=crate::empty();crate::accounts::initialize(&mut data,crate::accounts::Profile::BawRussia).unwrap();
    data["materials"]=json!((0..count).map(|n|json!({"id":format!("fact-{n:03}"),"kind":"reference","title":format!("Точный факт {n}"),"text":format!("Проверенный 🦀 факт {n}"),"revision":1})).collect::<Vec<_>>());
    crate::knowledge::sync_catalog(&mut data,"2026-01-01T00:00:00Z").unwrap();data
}
#[tokio::test]
async fn knowledge_heads_bound_history_feedback_bytes_and_keep_exact_current_versions() {
    let(db,_folder)=sqlite().await;let mut data=bounded_knowledge_fixture(25);
    data["materials"][0]["text"]=json!("Текущий 🦀 факт");data["materials"][0]["revision"]=json!(2);
    crate::knowledge::sync_catalog(&mut data,"2026-01-02T00:00:00Z").unwrap();
    seed_read_fixture(&db,&data).await;
    let baseline=db.read_knowledge_heads(&KnowledgeHeadsQuery::default()).await.unwrap();
    assert_eq!(baseline["heads"].as_array().unwrap().len(),20);assert!(baseline["nextCursor"].is_string());
    assert_eq!(baseline["coverage"]["selectionComplete"],false);
    let serialized=baseline.to_string();assert!(!serialized.contains("knowledge_versions"));assert!(!serialized.contains("Проверенный 🦀 факт"));
    let current=data["knowledge_versions"].as_array().unwrap().iter().find(|v|v["id"]==data["knowledge_entries"][0]["currentVersionId"]).unwrap();
    assert_eq!(baseline["heads"][0]["hash"],current["hash"]);assert_eq!(baseline["heads"][0]["versionId"],current["id"]);
    assert_eq!(baseline["heads"][0]["textLength"],current["text"].as_str().unwrap().len());
    for n in 0..250 {data["knowledge_versions"].as_array_mut().unwrap().push(json!({"id":format!("cold-history-{n}"),"text":"PRIVATE_HISTORY".repeat(4096)}));}
    data["feedback"]=json!([{"id":"owner-private","text":"PRIVATE_FEEDBACK".repeat(100_000)}]);seed_read_fixture(&db,&data).await;
    let mut after=db.read_knowledge_heads(&KnowledgeHeadsQuery::default()).await.unwrap();let mut before=baseline;
    after.as_object_mut().unwrap().remove("observedAt");before.as_object_mut().unwrap().remove("observedAt");assert_eq!(after,before,"unrelated history and owner feedback never enter selected decoding");
    assert_eq!(db.read_knowledge_catalog(true).await.unwrap()["feedback"],data["feedback"],"old catalog stays explicit and unchanged");
    db.close().await;
}
#[tokio::test]
async fn knowledge_head_pagination_and_exact_verify_survive_concurrent_revision() {
    let(db,_folder)=sqlite().await;let mut data=bounded_knowledge_fixture(3);seed_read_fixture(&db,&data).await;
    let query=KnowledgeHeadsQuery{limit:Some(1),..Default::default()};let first=db.read_knowledge_heads(&query).await.unwrap();
    let cursor=first["nextCursor"].as_str().unwrap().to_owned();
    let second=db.read_knowledge_heads(&KnowledgeHeadsQuery{cursor:Some(cursor.clone()),..query.clone()}).await.unwrap();
    assert_ne!(first["heads"][0]["id"],second["heads"][0]["id"]);
    let entry=first["heads"][0]["id"].as_str().unwrap();let old=first["heads"][0]["versionId"].as_str().unwrap();
    assert_eq!(db.read_knowledge_entry(entry,Some(old)).await.unwrap()["version"]["id"],old);
    data["materials"][0]["text"]=json!("Revision committed after heads read");data["materials"][0]["revision"]=json!(2);
    crate::knowledge::sync_catalog(&mut data,"2026-01-02T00:00:00Z").unwrap();seed_read_fixture(&db,&data).await;
    assert_eq!(db.read_knowledge_entry(entry,Some(old)).await.unwrap_err().0,StatusCode::CONFLICT);
    let history=db.read_knowledge_version(old).await.unwrap();assert_eq!(history["version"]["id"],old);assert_eq!(history["isCurrent"],false);
    assert_eq!(db.read_knowledge_entry(entry,None).await.unwrap()["version"]["id"],data["knowledge_entries"][0]["currentVersionId"]);
    assert!(db.read_knowledge_heads(&KnowledgeHeadsQuery{cursor:Some(cursor.clone()),limit:Some(2),..Default::default()}).await.is_err());
    assert!(db.read_knowledge_heads(&KnowledgeHeadsQuery{cursor:Some(cursor.clone()),entry_ids:Some(entry.into()),..query.clone()}).await.is_err());
    data["account"]=json!("LikeAvto");data["connectorBinding"]=crate::accounts::Profile::LikeAvto.binding();seed_read_fixture(&db,&data).await;
    assert!(db.read_knowledge_heads(&KnowledgeHeadsQuery{cursor:Some(cursor),..query}).await.is_err(),"cursor cannot cross a company or binding");
    db.close().await;
}
#[tokio::test]
async fn knowledge_selected_reads_reject_selector_hash_duplicate_foreign_and_raw_budgets() {
    let(db,_folder)=sqlite().await;let baseline=bounded_knowledge_fixture(1);
    let entry=baseline["knowledge_entries"][0]["id"].as_str().unwrap();let version=baseline["knowledge_versions"][0]["id"].as_str().unwrap();
    for mutation in ["hash","missing_version","missing_material","duplicate_head","duplicate_version","foreign_scope","oversize_text","oversize_head"] {
        let mut data=baseline.clone();match mutation {
            "hash"=>data["knowledge_versions"][0]["hash"]=json!("0".repeat(64)),
            "missing_version"=>data["knowledge_versions"]=json!([]),
            "missing_material"=>data["materials"]=json!([]),
            "duplicate_head"=>{let row=data["knowledge_entries"][0].clone();data["knowledge_entries"].as_array_mut().unwrap().push(row);},
            "duplicate_version"=>{let row=data["knowledge_versions"][0].clone();data["knowledge_versions"].as_array_mut().unwrap().push(row);},
            "foreign_scope"=>{data["knowledge_versions"][0]["scope"]["account"]=json!("LikeAvto");},
            "oversize_text"|"oversize_head"=>{
                data["materials"][0][if mutation=="oversize_text"{"text"}else{"title"}]=json!("🦀".repeat(if mutation=="oversize_text"{300_000}else{70_000}));
                data["materials"][0]["revision"]=json!(2);crate::knowledge::sync_catalog(&mut data,"2026-01-02T00:00:00Z").unwrap();
            },_=>unreachable!(),
        }
        seed_read_fixture(&db,&data).await;
        assert!(db.read_knowledge_heads(&KnowledgeHeadsQuery::default()).await.is_err(),"{mutation}");
        if mutation!="oversize_head" {assert!(db.read_knowledge_entry(entry,None).await.is_err(),"{mutation}");}
        if mutation=="oversize_text" {assert_eq!(db.read_knowledge_entry(entry,None).await.unwrap_err().0,StatusCode::PAYLOAD_TOO_LARGE);}
    }
    seed_read_fixture(&db,&baseline).await;
    assert_eq!(db.read_knowledge_entry("missing",None).await.unwrap_err().0,StatusCode::NOT_FOUND);
    assert_eq!(db.read_knowledge_version("missing").await.unwrap_err().0,StatusCode::NOT_FOUND);
    assert_eq!(db.read_knowledge_version(version).await.unwrap()["isCurrent"],true);
    for query in [KnowledgeHeadsQuery{limit:Some(101),..Default::default()},KnowledgeHeadsQuery{limit:Some(0),..Default::default()},
        KnowledgeHeadsQuery{entry_ids:Some(entry.into()),source_material_ids:Some("fact-000".into()),..Default::default()},
        KnowledgeHeadsQuery{entry_ids:Some(format!("{entry},{entry}")),..Default::default()},
        KnowledgeHeadsQuery{source_material_ids:Some((0..21).map(|n|format!("m{n}")).collect::<Vec<_>>().join(",")),..Default::default()}] {
        assert!(db.read_knowledge_heads(&query).await.is_err());
    }
    let by_source=db.read_knowledge_heads(&KnowledgeHeadsQuery{source_material_ids:Some("fact-000".into()),..Default::default()}).await.unwrap();
    assert_eq!(by_source["heads"][0]["id"],entry);assert_eq!(by_source["heads"].as_array().unwrap().len(),1);
    assert!(serde_json::from_value::<KnowledgeHeadsQuery>(json!({"account":"LikeAvto"})).is_err(),"query allowlist rejects a client company override");
    db.close().await;
}#[tokio::test]
async fn knowledge_empty_heads_are_complete_and_missing_workspace_remains_error(){
    let(db,_folder)=sqlite().await;seed_read_fixture(&db,&bounded_knowledge_fixture(0)).await;
    let heads=db.read_knowledge_heads(&KnowledgeHeadsQuery::default()).await.unwrap();assert_eq!(heads["heads"],json!([]));
    assert!(heads["nextCursor"].is_null());assert_eq!(heads["coverage"]["selectionComplete"],true);
    assert_eq!(db.read_knowledge_entry("missing",None).await.unwrap_err().0,StatusCode::NOT_FOUND);
    let Database::Sqlite(pool)=&db else{unreachable!()};sqlx::query("DELETE FROM workspace WHERE id=1").execute(pool).await.unwrap();
    assert!(db.read_knowledge_heads(&KnowledgeHeadsQuery::default()).await.is_err());
    assert!(db.read_knowledge_version("missing").await.is_err());db.close().await;
}
#[test]
fn public_bootstrap_hides_archived_and_live_invocation_authority_without_losing_durable_history() {
    let mut data=crate::empty();normalize(&mut data);
    data["cleanStartArchive"]=json!({"archiveSha256":"retained-archive","invocationBudgetCarry":{
        "originalJobs":[{"origin":{"marker":"PRIVATE_ORIGIN"},"reservations":[{"marker":"PRIVATE_RESERVATION"}]}],
        "predecessorCarry":{"originalJobs":[{"marker":"PRIVATE_PREDECESSOR"}]}}});
    data["jobs"]=json!([{"id":"visible-job","status":"completed","result":{"text":"visible result",
        "runMetadata":{"invocationBudget":{"marker":"PRIVATE_BUDGET"}}}}]);
    let view=crate::bootstrap_view(data.clone(),"csrf");
    assert_eq!(view["cleanStartArchive"]["archiveSha256"],"retained-archive");
    assert!(view["cleanStartArchive"].get("invocationBudgetCarry").is_none());
    assert!(!view.to_string().contains("PRIVATE_"));
    assert_eq!(view["jobs"][0]["result"]["text"],"visible result");
    assert!(data["cleanStartArchive"].get("invocationBudgetCarry").is_some());
    assert_eq!(data["jobs"][0]["result"]["runMetadata"]["invocationBudget"]["marker"],"PRIVATE_BUDGET");
}
