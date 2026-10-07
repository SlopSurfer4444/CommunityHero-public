use super::*;

async fn sqlite()->(Database,tempfile::TempDir){
    let dir=tempfile::tempdir().unwrap();let pool=crate::open_db(&dir.path().join("assistant-scope.sqlite")).await.unwrap();(Database::Sqlite(pool),dir)
}
#[tokio::test]async fn conversation_creation_preserves_operator_attachments_and_unrelated_state(){
    let (db,_dir)=sqlite().await;db.change(|d|{seed(d);Ok(())}).await.unwrap();
    let runtime=crate::native_fixture_owner_repair::initialize_db(&db).await.unwrap();
    let baseline=db.read().await.unwrap();
    let before_time=chrono::Utc::now();
    let attached=json!(["i","i"]);
    let created=db.create_conversation("operator-exact","",attached.as_array().unwrap(), &runtime).await.unwrap();
    assert_eq!(created["operatorId"],"operator-exact");assert_eq!(created["title"],"");
    assert_eq!(created["itemIds"],attached);assert_eq!(created["messages"],json!([]));
    uuid::Uuid::parse_str(created["id"].as_str().unwrap()).unwrap();
    let created_at=chrono::DateTime::parse_from_rfc3339(created["createdAt"].as_str().unwrap()).unwrap();
    assert!(created_at>=before_time&&created_at<=chrono::Utc::now());
    let mut expected=baseline.clone();expected["conversations"].as_array_mut().unwrap().push(created.clone());
    assert_eq!(db.read().await.unwrap(),expected);
    let empty=db.create_conversation("another-operator","Обсуждение",&[], &runtime).await.unwrap();
    assert_ne!(empty["id"],created["id"]);assert_eq!(empty["operatorId"],"another-operator");assert_eq!(empty["itemIds"],json!([]));
    expected["conversations"].as_array_mut().unwrap().push(empty);assert_eq!(db.read().await.unwrap(),expected);
}
#[tokio::test]async fn conversation_creation_rejects_unknown_and_invalid_ids_without_any_write(){
    let (db,_dir)=sqlite().await;db.change(|d|{seed(d);Ok(())}).await.unwrap();
    let runtime=crate::native_fixture_owner_repair::initialize_db(&db).await.unwrap();
    let baseline=db.read().await.unwrap();
    for (ids,status,message) in [
        (json!(["i","missing"]),axum::http::StatusCode::NOT_FOUND,"items record not found"),
        (json!(["i",42]),axum::http::StatusCode::BAD_REQUEST,"Invalid itemIds"),
        (json!(["missing",42]),axum::http::StatusCode::NOT_FOUND,"items record not found"),
        (json!([null,"missing"]),axum::http::StatusCode::BAD_REQUEST,"Invalid itemIds")
    ]{
        let error=db.create_conversation("operator","title",ids.as_array().unwrap(), &runtime).await.unwrap_err();
        assert_eq!(error.0,status);assert_eq!(error.1,message);assert_eq!(db.read().await.unwrap(),baseline);
    }
}
fn seed(d:&mut Value){
    d["items"]=json!([{"id":"i","itemId":"42","objectId":"11391","postKey":"11391:p","conversationKey":"11391:c","revision":1,"workflow":"attention","branchId":"b","postId":"p","text":"Стоимость?","contextEvidenceDigest":"a".repeat(64)}]);
    d["branches"]=json!([{"id":"b","postId":"p","messages":[]}]);d["posts"]=json!([{"id":"p","postKey":"11391:p","title":"Public post"}]);
    d["conversations"]=json!([{"id":"chat","operatorId":"local-owner","itemIds":["i"],"messages":[{"id":"u","role":"user","text":"Нужен ответ"}]},{"id":"other","operatorId":"bob","messages":[{"text":"private-other-conversation"}]}]);
    let bundle=crate::prepare_bundle::build(d,&[json!("i")],d["conversations"][0]["messages"].as_array().unwrap()).unwrap();
    d["jobs"]=json!([
        {"id":"job","kind":"assistant","status":"running","operatorId":"local-owner","refId":"chat","sourceUserMessageId":"u","prepareBundle":bundle},
        {"id":"source","kind":"assistant","status":"completed","refId":"i","prepareBundle":bundle},
        {"id":"recovery","kind":"assistant","status":"completed","refId":"i","prepareBundle":bundle},
        {"id":"active","kind":"assistant","status":"running","operatorId":"bob","refId":"other"}
    ]);
    d["proposals"]=json!([{"id":"p-draft","itemId":"i","status":"draft","prepareRunId":"source","recovery":{"prepareRunId":"recovery"}}]);
    d["audit"]=json!([{"id":"old-audit","action":"prior","refId":"i"}]);
    d["feedback"]=json!([{"id":"old-feedback","itemId":"i","kind":"prior","private":"old"}]);
    d["approvals"]=json!([{"id":"old-approval","status":"approved","proposals":[]}]);
}
fn seed_empty_dialogue(d:&mut Value){
    seed(d);
    d["conversations"][0]["itemIds"]=json!([]);
    d["materials"]=json!([
        {"id":"global","kind":"rule","title":"Global guidance","text":"Keep every claim grounded","revision":1},
        {"id":"media","kind":"transcript","title":"Video","text":"Scoped video evidence","postKey":"11391:p","revision":1}
    ]);
    crate::knowledge::sync_catalog(d,&crate::now()).unwrap();
    let bundle=crate::prepare_bundle::build(d,&[],&[]).unwrap();
    d["jobs"][0]["prepareBundle"]=bundle;
}
#[tokio::test]
async fn manual_model_review_native_receipt_survives_scoped_assistant_prepare_and_confirmation(){
    let(mut d,batch,result,native)=super::super::hot_admission::tests::native_editorial_material_fixture();
    crate::editorial_review::admit(&mut d,&batch,&result,"2026-10-06T00:00:01Z").unwrap();
    let actor=crate::operator_editorial::tests::actor();
    let proposal=d["proposals"][0].clone();let item=crate::row(&d,"items",proposal["itemId"].as_str().unwrap()).unwrap().clone();
    assert!(proposal["prepareRunId"].is_null());assert_eq!(proposal["editorialModelMaterialReceipt"]["nativeJobId"],native);
    d["conversations"]=json!([{"id":"native-chat","operatorId":actor.id,"itemIds":[item["id"]],"messages":[{"id":"request","role":"user","text":"Подготовь проверенный ответ"}]}]);
    let(db,_temp)=sqlite().await;db.change(|workspace|{*workspace=d;Ok(())}).await.unwrap();
    let full=db.read().await.unwrap();let view=db.read_assistant_context(None,"native-chat").await.unwrap();
    assert_eq!(view,projected(&full,None,"native-chat").unwrap());
    assert_eq!(crate::row(&view,"jobs",&native).unwrap(),crate::row(&full,"jobs",&native).unwrap());
    assert!(crate::row(&view,"jobs","cold-editorial-history").is_err());
    let args=json!({"mode":"execute_prepared","items":[{"id":item["id"],"revision":item["revision"],"proposalId":proposal["id"],"proposalRevision":proposal["revision"]}]});
    let receipt=db.change_assistant_observed(None,"native-chat",|workspace|crate::assistant_action_review::prepare(workspace,&actor,"native-chat","request",&args)).await.unwrap().0;
    assert_eq!(receipt["terminal"],true);assert_eq!(receipt["status"],"awaiting_confirmation");
    let reviewed=db.read().await.unwrap();assert_eq!(reviewed["jobs"],full["jobs"]);assert_eq!(reviewed["operations"],full["operations"]);assert_eq!(reviewed["approvals"],full["approvals"]);
    db.change_assistant_observed(None,"native-chat",|workspace|{workspace["conversations"][0]["messages"].as_array_mut().unwrap().push(json!({"id":"confirm","role":"user","text":"Да, отправляй"}));Ok(())}).await.unwrap();
    let confirmed=db.read().await.unwrap();let scoped=db.read_assistant_context(None,"native-chat").await.unwrap();
    assert_eq!(crate::assistant_action_review::pending_confirmation(&scoped,&actor,"native-chat","confirm").unwrap(),crate::assistant_action_review::pending_confirmation(&confirmed,&actor,"native-chat","confirm").unwrap());
    assert!(crate::assistant_action_review::pending_confirmation(&scoped,&actor,"native-chat","confirm").unwrap().is_some());
    // Missing paid native history is never treated as an unreviewed manual
    // exemption. Compare the same exact error in full and scoped readers.
    for mutation in ["missing","wrong_kind","history","paid"]{
        let mut hostile=confirmed.clone();
        let at=hostile["jobs"].as_array().unwrap().iter().position(|job|job["id"]==native).unwrap();
        match mutation{"missing"=>{hostile["jobs"].as_array_mut().unwrap().remove(at);},"wrong_kind"=>hostile["jobs"][at]["kind"]=json!("sync"),"history"=>hostile["jobs"][at]["modelMaterialReceipts"]=json!([]),_=>hostile["jobs"][at]["retainedEvidence"]=json!([])}
        let expected=crate::assistant_action_review::pending_confirmation(&hostile,&actor,"native-chat","confirm").unwrap_err();
        let projection=projected(&hostile,None,"native-chat").unwrap();
        let actual=crate::assistant_action_review::pending_confirmation(&projection,&actor,"native-chat","confirm").unwrap_err();
        assert_eq!(actual.1,expected.1,"{mutation}");
    }
    db.close().await;
}
#[tokio::test]async fn empty_dialogue_excludes_comment_corpus_but_preserves_global_source_checks(){
    let (db,_dir)=sqlite().await;
    db.change(|d|{
        seed_empty_dialogue(d);
        for n in 0..200 {
            d["items"].as_array_mut().unwrap().push(json!({"id":format!("unrelated-{n}"),"text":"unrelated comment content".repeat(1000)}));
            d["branches"].as_array_mut().unwrap().push(json!({"id":format!("branch-{n}"),"messages":[{"text":"unrelated branch content".repeat(1000)}]}));
        }
        Ok(())
    }).await.unwrap();
    let full=db.read().await.unwrap();
    let fast=db.read_assistant_dialogue(Some("job"),"chat",&[]).await.unwrap();
    assert_eq!(fast,projected_scope(&full,Some("job"),"chat",Scope::Dialogue(&[])).unwrap());
    for table in EMPTY_DIALOGUE_TABLES{assert!(rows(&fast,table).unwrap().is_empty(),"{table}");}
    for table in ["posts","knowledge_entries","knowledge_versions"]{assert_eq!(fast[table],full[table],"{table}");}
    let job_ids:Vec<_>=rows(&fast,"jobs").unwrap().iter().map(|j|j["id"].as_str().unwrap()).collect();
    assert_eq!(job_ids,vec!["job","active"]); // global concurrency still visible
    let full_bundle=crate::prepare_bundle::build(&full,&[],&[]).unwrap();
    let fast_bundle=crate::prepare_bundle::build(&fast,&[],&[]).unwrap();
    assert_eq!(full_bundle["dependencyDigest"],fast_bundle["dependencyDigest"]);
    assert_eq!(full_bundle["request"],fast_bundle["request"]);
    assert!(crate::prepare_bundle::current(&fast,&full["jobs"][0]["prepareBundle"]).is_ok());
    let corpus=db.read_assistant_context(Some("job"),"chat").await.unwrap();
    assert_eq!(crate::assistant_context::query(&corpus,&json!({}),true).unwrap()["total"],201);
    assert!(fast.to_string().len()*50<corpus.to_string().len());
    println!("EMPTY_DIALOGUE_BYTES corpus={} dialogue={}",corpus.to_string().len(),fast.to_string().len());
    // The entire historical catalog remains integrity-checked, even when no
    // comment is attached. A corrupt unrelated source still fails closed.
    let mut corrupt=fast.clone();corrupt["knowledge_versions"][1]["text"]=json!("tampered");
    assert!(crate::prepare_bundle::current(&corrupt,&full["jobs"][0]["prepareBundle"]).is_err());
}
#[tokio::test]async fn every_retained_or_admission_root_falls_back_to_exact_corpus(){
    let (db,_dir)=sqlite().await;
    for mode in ["attached","bundle","tools","request_tools","review","extra","malformed","array_string"] {
        db.change(|d|{
            seed_empty_dialogue(d);
            match mode {
                "attached"=>d["conversations"][0]["itemIds"]=json!(["i"]),
                "bundle"=>d["jobs"][0]["prepareBundle"]["itemIds"]=json!(["i"]),
                "tools"=>d["jobs"][0]["toolResults"]=json!([{"name":"read_comments","result":{"items":[{"id":"i"}]}}]),
                "request_tools"=>d["jobs"][0]["prepareBundle"]["request"]["toolResults"]=json!([{"name":"read_comments","result":{"items":[{"id":"i"}]}}]),
                "review"=>d["conversations"][0]["actionReviews"]=json!([{"id":"review","status":"presented"}]),
                "malformed"=>d["conversations"][0]["itemIds"]=json!({"bad":"shape"}),
                "array_string"=>d["conversations"][0]["itemIds"]=json!("[]"),
                _=>{}
            }
            Ok(())
        }).await.unwrap();
        let full=db.read().await.unwrap();
        let extra=if mode=="extra"{vec![json!("i")]}else{vec![]};
        let view=db.read_assistant_dialogue(Some("job"),"chat",&extra).await.unwrap();
        assert_eq!(view,projected(&full,Some("job"),"chat").unwrap(),"{mode}");
        assert_eq!(view,projected_scope(&full,Some("job"),"chat",Scope::Dialogue(&extra)).unwrap(),"{mode}");
    }
}
#[tokio::test]async fn empty_dialogue_delta_preserves_omitted_rows_and_authority(){
    let (db,_dir)=sqlite().await;db.change(|d|{seed_empty_dialogue(d);Ok(())}).await.unwrap();
    let before=db.read().await.unwrap();
    let result=db.change_assistant_dialogue_observed(Some("job"),"chat",&[],|d|{
        assert!(rows(d,"items")?.is_empty());
        crate::prepare_bundle::admit(d,"job","chat",&json!({"text":"Привет!","proposals":[],"sources":[]}))
    }).await.unwrap();
    assert!(result.1);
    let after=db.read().await.unwrap();
    for table in EMPTY_DIALOGUE_TABLES.iter().copied().chain(["posts","knowledge_entries","knowledge_versions","approvals"]){assert_eq!(after[table],before[table],"{table}");}
    assert_eq!(after["conversations"][1],before["conversations"][1]);
    assert_eq!(after["jobs"][1],before["jobs"][1]);assert_eq!(after["jobs"][2],before["jobs"][2]);
    assert_eq!(after["conversations"][0]["messages"].as_array().unwrap().last().unwrap()["text"],"Привет!");
    for mode in ["item","operation","proposal","source","other_job"] {
        let error=db.change_assistant_dialogue_observed(Some("job"),"chat",&[],|d|{
            match mode {
                "item"=>d["items"].as_array_mut().unwrap().push(before["items"][0].clone()),
                "operation"=>d["operations"].as_array_mut().unwrap().push(json!({"id":"forged","status":"unknown"})),
                "proposal"=>d["proposals"].as_array_mut().unwrap().push(json!({"id":"forged","itemId":"i","status":"draft"})),
                "source"=>d["knowledge_versions"][0]["text"]=json!("tampered"),
                _=>crate::row_mut(d,"jobs","active")?["status"]=json!("completed")
            }
            Ok(())
        }).await;
        assert!(error.is_err(),"{mode}");assert_eq!(db.read().await.unwrap(),after,"{mode}");
    }
    // Attached admissions use the complete graph even if the saved chat was empty.
    db.change_assistant_dialogue_observed(None,"chat",&[json!("i")],|d|{
        assert_eq!(rows(d,"items")?.len(),1);
        d["conversations"][0]["itemIds"]=json!(["i"]);Ok(())
    }).await.unwrap();
    assert_eq!(db.read_assistant_dialogue(None,"chat",&[]).await.unwrap()["items"],before["items"]);
}
#[tokio::test]async fn history_is_excluded_before_rust_and_projection_stays_fresh(){
    let (db,_dir)=sqlite().await;
    db.change(|d|{seed(d);for n in 0..150{d["jobs"].as_array_mut().unwrap().push(json!({"id":format!("irrelevant-{n}"),"kind":"assistant","status":"completed","private":"large unrelated model history".repeat(1024)}));}Ok(())}).await.unwrap();
    let full=db.read().await.unwrap();let snapshot=db.read_assistant_context(Some("job"),"chat").await.unwrap();
    assert_eq!(snapshot,projected(&full,Some("job"),"chat").unwrap());
    assert_eq!(snapshot["jobs"].as_array().unwrap().len(),4);assert_eq!(snapshot["conversations"].as_array().unwrap().len(),1);
    assert!(!snapshot.to_string().contains("large unrelated model history"));assert!(!snapshot.to_string().contains("private-other-conversation"));
    for key in ["audit","feedback","approvals"]{assert!(rows(&snapshot,key).unwrap().is_empty());}
    let full_bytes=full.to_string().len();let scope_bytes=snapshot.to_string().len();assert!(scope_bytes*50<full_bytes,"{scope_bytes} vs {full_bytes}");
    println!("ASSISTANT_SCOPE_BYTES full={full_bytes} scoped={scope_bytes}");
    db.change(|d|{d["items"][0]["text"]=json!("Changed current source");Ok(())}).await.unwrap();
    let fresh=db.read_assistant_context(Some("job"),"chat").await.unwrap();assert_eq!(fresh["items"][0]["text"],"Changed current source");
    assert!(crate::prepare_bundle::current(&fresh,&fresh["jobs"][0]["prepareBundle"]).is_err());
}
#[tokio::test]async fn partial_delta_preserves_unloaded_history_and_rejects_identity_reuse(){
    let (db,_dir)=sqlite().await;db.change(|d|{seed(d);d["jobs"].as_array_mut().unwrap().push(json!({"id":"unloaded","kind":"sync","status":"completed","text":"keep exactly"}));Ok(())}).await.unwrap();
    let before=db.read().await.unwrap();
    db.change_assistant_observed(Some("job"),"chat",|d|{
        assert_eq!(rows(d,"jobs")?.len(),4);assert!(rows(d,"audit")?.is_empty());
        crate::patch_item(d,"i",&json!({"expectedRevision":1,"workflow":"waiting","waitingReason":"Requested","_verifiedActor":{"id":"local-owner","role":"owner"}}))?;
        crate::row_mut(d,"jobs","job")?["toolResults"]=json!([{"id":"c","ok":true}]);
        crate::audit(d,"assistant.workflow","job");Ok(())
    }).await.unwrap();
    let after=db.read().await.unwrap();assert_eq!(after["items"][0]["workflow"],"waiting");assert_eq!(after["items"][0]["revision"],2);
    assert_eq!(after["conversations"][1],before["conversations"][1]);assert_eq!(after["jobs"][4],before["jobs"][4]);assert_eq!(after["approvals"],before["approvals"]);
    assert!(after["audit"].as_array().unwrap().starts_with(before["audit"].as_array().unwrap()));assert!(after["feedback"].as_array().unwrap().starts_with(before["feedback"].as_array().unwrap()));
    let error=db.change_assistant_observed(Some("job"),"chat",|d|{d["items"][0]["workflow"]=json!("attention");d["audit"].as_array_mut().unwrap().push(before["audit"][0].clone());Ok(())}).await;
    assert!(error.is_err());assert_eq!(db.read().await.unwrap(),after);
}
#[tokio::test]async fn scoped_admission_and_proposal_outcome_equal_full_domain(){
    // The old candidate is retired; admitting a second foreign paid draft over
    // an active candidate is intentionally fenced by the reservation contract.
    let mut result=json!({"text":"Готово","proposals":[{"itemId":"i","kind":"reply_and_close","text":"Уточняем стоимость."}],"sources":[],
        "runMetadata":{"schemaVersion":1,"model":"fixture","reasoningEffort":"medium","promptVersion":"storage-native-discussion-fixture",
            "instructionSha256":"a".repeat(64),"inputSha256":"b".repeat(64),"cliSha256":"c".repeat(64),
            "elapsedMs":1,"completedAt":"2026-10-06T00:00:00Z"}});
    let (db,_dir)=sqlite().await;db.change(|d|{
        seed(d);d["proposals"][0]["status"]=json!("stale");
        // This scenario presents a newly actionable reply. Unlike the archive
        // fixtures, it needs an actual native material/paid-result receipt.
        // Construct this isolated capture before fixture persistence.
        d["connectorBinding"]=crate::active_binding(d)?.to_json();
        d["posts"][0]["attachments"]=json!([]);
        let mut bundle=crate::prepare_bundle::build(d,&[json!("i")],d["conversations"][0]["messages"].as_array().unwrap()).map_err(crate::bad)?;
        bundle["request"]["purpose"]=json!("discussion");
        crate::preparation_unit::attach(d,&mut bundle,"2026-10-06T00:00:00Z").map_err(crate::bad)?;
        crate::preparation_materials::attach_request(d,&mut bundle["request"]).map_err(crate::bad)?;
        bundle["digest"]=json!(crate::editorial_review::hash_text(&bundle["request"].to_string()));
        let request=bundle["request"].clone();
        let job=crate::row_mut(d,"jobs","job")?;job["purpose"]=json!("discussion");job["prepareBundle"]=bundle;
        result["runMetadata"]["visualNeedContract"]=request["visualNeedContract"].clone();
        result["runMetadata"]["visualSelection"]=request["visualSelection"].clone();
        crate::model_material_receipt::fixture_result(d,"job",&request,&mut result)?;Ok(())
    }).await.unwrap();
    let full=db.read().await.unwrap();let view=db.read_assistant_context(Some("job"),"chat").await.unwrap();
    let full_stats=crate::assistant_context::query(&full,&json!({}),true).unwrap();let scope_stats=crate::assistant_context::query(&view,&json!({}),true).unwrap();
    assert_eq!(without(&full_stats,&["observedAt"]),without(&scope_stats,&["observedAt"]));
    assert_eq!(crate::prepare_bundle::review_fingerprint(&full,"i").unwrap(),crate::prepare_bundle::review_fingerprint(&view,"i").unwrap());
    let outcome=db.change_assistant_observed(Some("job"),"chat",|d|crate::prepare_bundle::admit(d,"job","chat",&result)).await.unwrap().0;
    assert_eq!(outcome["status"],"review");let stored=db.read().await.unwrap();let proposal=stored["proposals"].as_array().unwrap().last().unwrap();assert!(crate::proposal_current(&stored,proposal).is_ok());
    let scoped=db.read_assistant_context(Some("job"),"chat").await.unwrap();assert!(crate::proposal_current(&scoped,scoped["proposals"].as_array().unwrap().last().unwrap()).is_ok());
    let receipt=db.change_assistant_observed(Some("job"),"chat",|d|crate::assistant_action_review::prepare(d,&crate::operator_auth::Actor::local_owner("test"),"chat","u",&json!({"mode":"execute_prepared","items":[{"id":"i","revision":2}]}))).await.unwrap().0;
    assert_eq!(receipt["terminal"],true);assert_eq!(receipt["proposals"][0]["proposal"]["text"],"Уточняем стоимость.");
    assert_eq!(db.read().await.unwrap()["approvals"],full["approvals"]);
    let (_,changed)=db.change_assistant_observed(None,"chat",|d|{
        let id=crate::new_job(d,"assistant","chat")?;let job=crate::row_mut(d,"jobs",&id)?;job["operatorId"]=json!("local-owner");job["sourceUserMessageId"]=json!("new-user");
        d["conversations"][0]["messages"].as_array_mut().unwrap().push(json!({"id":"new-user","role":"user","text":"Дальше"}));Ok(())
    }).await.unwrap();assert!(changed);
    assert_eq!(db.read().await.unwrap()["jobs"].as_array().unwrap().len(),5);
}
#[tokio::test]async fn scope_cannot_rewrite_sources_other_jobs_or_authority(){
    let (db,_dir)=sqlite().await;db.change(|d|{seed(d);Ok(())}).await.unwrap();let baseline=db.read().await.unwrap();
    for mode in ["source","route","other_job","approval","operation","metadata","delete","other_chat","proposal"]{
        let result=db.change_assistant_observed(Some("job"),"chat",|d|{
            match mode{
                "source"=>d["posts"][0]["title"]=json!("tampered"),"route"=>d["items"][0]["itemId"]=json!("other"),
                "other_job"=>crate::row_mut(d,"jobs","source")?["prepareBundle"]=json!({}),"approval"=>d["approvals"].as_array_mut().unwrap().push(json!({"id":"forged"})),
                "operation"=>d["operations"].as_array_mut().unwrap().push(json!({"id":"forged"})),"metadata"=>d["settings"]["externalWrites"]=json!(true),
                "delete"=>{d["jobs"].as_array_mut().unwrap().remove(0);},"other_chat"=>d["conversations"].as_array_mut().unwrap().push(json!({"id":"intruder"})),
                _=>d["proposals"][0]["status"]=json!("approved")
            }Ok(())
        }).await;assert!(result.is_err(),"{mode}");assert_eq!(db.read().await.unwrap(),baseline,"{mode}");
    }
}

// Only the parent-created disposable clone may run this write probe. No provider
// calls, no removal of history, and never the production database name.
#[tokio::test]
#[ignore="requires COMMUNITYHERO_ASSISTANT_SCOPE_TEST_URL for isolated assistant_scope_test clone"]
async fn postgres_create_conversation_clone_probe(){
    let url=std::env::var("COMMUNITYHERO_ASSISTANT_SCOPE_TEST_URL").expect("explicit isolated clone URL");
    let pool=PgPoolOptions::new().max_connections(1).connect(&url).await.unwrap();
    let database:String=sqlx::query_scalar("SELECT current_database()").fetch_one(&pool).await.unwrap();
    assert!(database.contains("assistant_scope_test"),"refusing non-test database");
    crate::db_guards::require_schema(&pool).await.unwrap();pool.close().await;
    let db=Database::postgres(&url).await.unwrap();
    let runtime=crate::runtime_lifecycle_startup::initialize_db_fixture(&db).await.unwrap();
    let baseline=db.read().await.unwrap();let mut expected=baseline.clone();
    let started=std::time::Instant::now();
    let empty=db.create_conversation("probe-operator-empty","Обсуждение",&[], &runtime).await.unwrap();
    let empty_ms=started.elapsed().as_secs_f64()*1000.0;
    assert_eq!(empty["operatorId"],"probe-operator-empty");assert_eq!(empty["itemIds"],json!([]));assert_eq!(empty["messages"],json!([]));
    uuid::Uuid::parse_str(empty["id"].as_str().unwrap()).unwrap();
    chrono::DateTime::parse_from_rfc3339(empty["createdAt"].as_str().unwrap()).unwrap();
    expected["conversations"].as_array_mut().unwrap().push(empty.clone());
    let attached_id=baseline["items"].as_array().unwrap().first().expect("clone must contain an attachment fixture")["id"].clone();
    let attached=vec![attached_id.clone(),attached_id];let started=std::time::Instant::now();
    let created=db.create_conversation("probe-operator-attached","",&attached, &runtime).await.unwrap();
    let attached_ms=started.elapsed().as_secs_f64()*1000.0;
    assert_ne!(created["id"],empty["id"]);assert_eq!(created["operatorId"],"probe-operator-attached");
    assert_eq!(created["title"],"");assert_eq!(created["itemIds"],json!(attached));
    expected["conversations"].as_array_mut().unwrap().push(created);
    assert_eq!(db.read().await.unwrap(),expected); // all unrelated rows and metadata exact
    for (ids,status,message) in [
        (vec![json!(format!("missing-{}",crate::id())),json!(42)],axum::http::StatusCode::NOT_FOUND,"items record not found"),
        (vec![json!(42)],axum::http::StatusCode::BAD_REQUEST,"Invalid itemIds")
    ]{
        let error=db.create_conversation("probe-operator","Rejected",&ids, &runtime).await.unwrap_err();
        assert_eq!(error.0,status);assert_eq!(error.1,message);assert_eq!(db.read().await.unwrap(),expected);
    }
    println!("CREATE_CONVERSATION_CLONE_PROBE {}",json!({"emptyCreateMs":empty_ms,"attachedCreateMs":attached_ms,"existingConversations":rows(&baseline,"conversations").unwrap().len(),"workspaceBytes":baseline.to_string().len()}));
    db.close().await;
}
#[tokio::test]
#[ignore="requires COMMUNITYHERO_ASSISTANT_SCOPE_TEST_URL for isolated assistant_scope_test clone"]
async fn postgres_empty_dialogue_clone_probe(){
    let url=std::env::var("COMMUNITYHERO_ASSISTANT_SCOPE_TEST_URL").expect("explicit isolated clone URL");
    let pool=PgPoolOptions::new().max_connections(1).connect(&url).await.unwrap();
    let database:String=sqlx::query_scalar("SELECT current_database()").fetch_one(&pool).await.unwrap();
    assert!(database.contains("assistant_scope_test"),"refusing non-test database");
    crate::db_guards::require_schema(&pool).await.unwrap();pool.close().await;
    let db=Database::postgres(&url).await.unwrap();
    let chat=format!("dialogue-probe-{}",crate::id());
    db.change(|d|{
        d["conversations"].as_array_mut().unwrap().push(json!({"id":chat,"operatorId":"local-owner","itemIds":[],"messages":[]}));Ok(())
    }).await.unwrap();
    let baseline=db.read().await.unwrap();
    let started=std::time::Instant::now();
    let corpus=db.read_assistant_context(None,&chat).await.unwrap();let corpus_ms=started.elapsed().as_secs_f64()*1000.0;
    let started=std::time::Instant::now();
    let fast=db.read_assistant_dialogue(None,&chat,&[]).await.unwrap();let dialogue_ms=started.elapsed().as_secs_f64()*1000.0;
    assert_eq!(fast,projected_scope(&baseline,None,&chat,Scope::Dialogue(&[])).unwrap());
    let expected=crate::prepare_bundle::build(&corpus,&[],&[]).unwrap();
    let actual=crate::prepare_bundle::build(&fast,&[],&[]).unwrap();
    assert_eq!(actual["request"],expected["request"]);assert_eq!(actual["dependencyDigest"],expected["dependencyDigest"]);
    let started=std::time::Instant::now();
    let job=db.change_assistant_dialogue_observed(None,&chat,&[],|d|{
        let bundle=crate::prepare_bundle::build(d,&[],&[]).map_err(crate::bad)?;
        let id=crate::new_job(d,"assistant",&chat)?;
        let job=crate::row_mut(d,"jobs",&id)?;
        job["operatorId"]=json!("local-owner");job["sourceUserMessageId"]=json!("probe-user");job["prepareBundle"]=bundle;
        crate::row_mut(d,"conversations",&chat)?["messages"].as_array_mut().unwrap().push(json!({"id":"probe-user","role":"user","text":"Привет"}));
        Ok(id)
    }).await.unwrap().0;let admission_ms=started.elapsed().as_secs_f64()*1000.0;
    let started=std::time::Instant::now();
    db.change_assistant_dialogue_observed(Some(&job),&chat,&[],|d|{
        let bundle=crate::row(d,"jobs",&job)?["prepareBundle"].clone();
        crate::prepare_bundle::current(d,&bundle).map_err(crate::bad)?;
        crate::prepare_bundle::admit(d,&job,&chat,&json!({"text":"Привет!","proposals":[],"sources":[]}))
    }).await.unwrap();let finish_ms=started.elapsed().as_secs_f64()*1000.0;
    let saved=db.read().await.unwrap();
    for table in TABLES{
        match table{
            "jobs"=>assert!(rows(&saved,table).unwrap().starts_with(rows(&baseline,table).unwrap())),
            "conversations"=>{for old in rows(&baseline,table).unwrap(){if old["id"]!=chat{assert!(rows(&saved,table).unwrap().contains(old));}}},
            "audit"|"feedback"=>assert!(rows(&saved,table).unwrap().starts_with(rows(&baseline,table).unwrap())),
            _=>assert_eq!(saved[table],baseline[table],"{table}")
        }
    }
    // Arbitrary tool receipts force the full corpus, including discovery-only
    // search results whose IDs intentionally did not enter prepareBundle.
    db.change_assistant_dialogue_observed(Some(&job),&chat,&[],|d|{
        crate::row_mut(d,"jobs",&job)?["toolResults"]=json!([{"name":"search_comments","ok":true,"result":{"items":[]}}]);Ok(())
    }).await.unwrap();
    assert_eq!(db.read_assistant_dialogue(Some(&job),&chat,&[]).await.unwrap(),db.read_assistant_context(Some(&job),&chat).await.unwrap());
    println!("EMPTY_DIALOGUE_CLONE_PROBE {}",json!({"corpusBytes":corpus.to_string().len(),"dialogueBytes":fast.to_string().len(),"corpusReadMs":corpus_ms,"dialogueReadMs":dialogue_ms,"admissionMs":admission_ms,"finishMs":finish_ms}));
    db.close().await;
}
#[tokio::test]
#[ignore="requires COMMUNITYHERO_ASSISTANT_SCOPE_TEST_URL for isolated assistant_scope_test clone"]
async fn postgres_assistant_scope_clone_probe(){
    let url=std::env::var("COMMUNITYHERO_ASSISTANT_SCOPE_TEST_URL").expect("explicit isolated clone URL");
    let pool=PgPoolOptions::new().max_connections(1).connect(&url).await.unwrap();
    let database:String=sqlx::query_scalar("SELECT current_database()").fetch_one(&pool).await.unwrap();
    assert!(database.contains("assistant_scope_test"),"refusing non-test database");crate::db_guards::require_schema(&pool).await.unwrap();pool.close().await;
    let db=Database::postgres(&url).await.unwrap();
    let chat=format!("scope-chat-{}",crate::id());let audit_id=format!("scope-audit-{}",crate::id());
    db.change(|d|{d["conversations"].as_array_mut().unwrap().push(json!({"id":chat,"operatorId":"local-owner","messages":[]}));Ok(())}).await.unwrap();
    let started=std::time::Instant::now();
    let job=db.change_assistant_observed(None,&chat,|d|{
        let id=crate::new_job(d,"assistant",&chat)?;crate::row_mut(d,"jobs",&id)?["operatorId"]=json!("local-owner");Ok(id)
    }).await.unwrap().0;let admission_ms=started.elapsed().as_secs_f64()*1000.0;
    let baseline=db.read().await.unwrap();let started=std::time::Instant::now();
    let scoped=db.read_assistant_context(Some(&job),&chat).await.unwrap();let read_ms=started.elapsed().as_secs_f64()*1000.0;
    assert_eq!(scoped,projected(&baseline,Some(&job),&chat).unwrap());
    let started=std::time::Instant::now();
    assert!(db.change_assistant_observed(Some(&job),&chat,|d|{
        crate::row_mut(d,"jobs",&job)?["result"]=json!({"scopeProbe":true});
        crate::row_mut(d,"conversations",&chat)?["messages"].as_array_mut().unwrap().push(json!({"id":"probe-message","role":"assistant","text":"Isolated scope probe"}));
        d["audit"].as_array_mut().unwrap().push(json!({"id":audit_id,"action":"assistant.scope_probe","refId":job}));Ok(())
    }).await.unwrap().1);let change_ms=started.elapsed().as_secs_f64()*1000.0;
    let saved=db.read().await.unwrap();
    for table in TABLES{
        if table=="jobs"{for old in rows(&baseline,table).unwrap(){if old["id"]!=job{assert!(rows(&saved,table).unwrap().contains(old));}}}
        else if table=="conversations"{for old in rows(&baseline,table).unwrap(){if old["id"]!=chat{assert!(rows(&saved,table).unwrap().contains(old));}}}
        else if table=="audit"{assert!(rows(&saved,table).unwrap().starts_with(rows(&baseline,table).unwrap()));assert_eq!(rows(&saved,table).unwrap().len(),rows(&baseline,table).unwrap().len()+1);}
        else{assert_eq!(saved[table],baseline[table],"{table}");}
    }
    // A duplicate unloaded audit identity fails after chat/job SQL updates;
    // the entire transaction must roll back, without erasing any history.
    assert!(db.change_assistant_observed(Some(&job),&chat,|d|{
        crate::row_mut(d,"jobs",&job)?["result"]=json!({"mustRollBack":true});
        crate::row_mut(d,"conversations",&chat)?["messages"].as_array_mut().unwrap().push(json!({"id":"rolled-back"}));
        d["audit"].as_array_mut().unwrap().push(json!({"id":audit_id,"action":"assistant.scope_probe","refId":job}));Ok(())
    }).await.is_err());assert_eq!(db.read().await.unwrap(),saved);
    assert!(!db.change_assistant_observed(Some(&job),&chat,|_|Ok(())).await.unwrap().1);
    println!("ASSISTANT_SCOPE_PROBE {}",json!({"fullBytes":baseline.to_string().len(),"scopedBytes":scoped.to_string().len(),"fullJobs":rows(&baseline,"jobs").unwrap().len(),"scopedJobs":rows(&scoped,"jobs").unwrap().len(),"admissionMs":admission_ms,"readMs":read_ms,"changeMs":change_ms}));
    db.close().await;
}
