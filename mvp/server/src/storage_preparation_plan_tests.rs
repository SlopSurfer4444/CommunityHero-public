use super::*;

fn add(d: &mut Value, id: &str, author: &str, platform: &str) {
    let binding = crate::active_binding(d).unwrap().to_json();
    crate::list_mut(d,"items").push(json!({"id":id,"itemId":format!("remote-{id}"),"objectId":"object",
        "postId":format!("post-{id}"),"postKey":format!("object:{id}"),"branchId":format!("branch-{id}"),
        "conversationKey":format!("thread-{id}"),"connectorBinding":binding,"platform":platform,"authorId":author,
        "text":format!("Comment {id}"),"workflow":"attention","providerStatus":"new","revision":1,
        "draft":"","createdAt":"2020-01-01T00:00:00Z"}));
    crate::list_mut(d,"posts").push(json!({"id":format!("post-{id}"),"postKey":format!("object:{id}"),"text":format!("Publication {id}"),"platform":platform}));
    crate::list_mut(d,"branches").push(json!({"id":format!("branch-{id}"),"postId":format!("post-{id}"),"contextComplete":true,
        "messages":[{"id":format!("remote-{id}"),"text":format!("Comment {id}"),"role":"participant","createdAt":"2020-01-01T00:00:00Z"}]}));
}
fn fixture(profile: crate::accounts::Profile) -> Value {
    let mut d = crate::empty();
    normalize(&mut d);
    crate::accounts::initialize(&mut d,profile).unwrap();
    add(&mut d,"a","author","VK");
    add(&mut d,"prior","author","VK");
    add(&mut d,"b","other-author","VK");
    add(&mut d,"other-platform","author","OK");
    add(&mut d,"foreign","author","VK");
    let foreign = crate::row_mut(&mut d,"items","foreign").unwrap();
    foreign["account"] = json!("Other company");
    let history = crate::row_mut(&mut d,"branches","branch-prior").unwrap();
    history["messages"].as_array_mut().unwrap().push(json!({"id":"published-reply","providerItemId":"reply",
        "providerObjectId":"object","replyToProviderItemId":"remote-prior","providerOfficial":true,
        "authorId":"brand","role":"brand","roleEvidence":"provider-official","text":"Пришлите номер договора.","createdAt":"2020-01-02T00:00:00Z"}));
    crate::knowledge::sync_catalog(&mut d,"2020-01-01T00:00:00Z").unwrap();
    crate::knowledge::save_instruction(&mut d,&json!({"requestId":"rule","title":"Rule","text":"Preserve uncertainty"}),"2020-01-01T00:00:00Z").unwrap();
    d
}
fn body() -> Value { json!({"itemIds":["a","b"]}) }
fn confirmed_family_fixture()->Value {
    let mut d=fixture(crate::accounts::Profile::LikeAvto);
    for (id,url) in [("a","https://www.youtube.com/watch?v=AbCdEf123_-"),("b","https://vk.com/video-1_1"),("other-platform","https://ok.ru/video/123456")] {
        let post=crate::row_mut(&mut d,"posts",&format!("post-{id}")).unwrap();
        post["sourceUrl"]=json!(url);post["attachments"]=json!([{"type":"video"}]);
    }
    let source=crate::row(&d,"posts","post-a").unwrap().clone();
    let source_version=crate::media_fullframes::source_version(&source,"LikeAvto");
    crate::list_mut(&mut d,"materials").push(json!({"id":"family-speech","account":"LikeAvto","kind":"transcript","postKey":source["postKey"],"sourceUrl":source["sourceUrl"],
        "text":"HISTORICAL_TRANSCRIPT_ARCHIVE".repeat(100),"transcription":{"partial":false,"coverage":"full_audio","sourceVersion":source_version,
            "mediaDurationSeconds":120.0,"audioDurationSeconds":120.0}}));
    let at=crate::now();crate::knowledge::sync_catalog(&mut d,&at).unwrap();
    crate::row_mut(&mut d,"materials","family-speech").unwrap()["text"]=json!("Current complete source transcript");
    crate::knowledge::sync_catalog(&mut d,&at).unwrap();
    let entry=crate::list(&d,"knowledge_entries").iter().find(|entry|entry["sourceMaterialId"]=="family-speech").unwrap();
    let head=crate::row(&d,"knowledge_versions",entry["currentVersionId"].as_str().unwrap()).unwrap().clone();
    d["settings"]["mediaAudioEquivalences"]=json!({});
    for id in ["b","other-platform"] {
        let target=crate::row(&d,"posts",&format!("post-{id}")).unwrap().clone();
        d["settings"]["mediaAudioEquivalences"][format!("post-{id}")]=json!({"schemaVersion":1,"status":"active","revision":1,
            "account":"LikeAvto","connectorBinding":crate::active_binding(&d).unwrap().to_json(),"targetPostId":target["id"],"targetPostKey":target["postKey"],
            "targetSourceVersion":crate::media_fullframes::source_version(&target,"LikeAvto"),"sourcePostId":source["id"],"sourcePostKey":source["postKey"],
            "sourceVersion":source_version,"transcript":{"entryId":head["entryId"],"versionId":head["id"],"hash":head["hash"]}});
    }
    d
}
fn assert_confirmed_family_projection(d:&Value,view:&Value){
    let ids=vec!["b".to_owned(),"other-platform".to_owned()];let at=crate::now();
    let full=crate::prepare_plan::family_windows(d,&ids,100,2,&at).unwrap();
    assert_eq!(full["windows"],json!([["b","other-platform"]]));
    assert_eq!(full,crate::prepare_plan::family_windows(view,&ids,100,2,&at).unwrap());
    assert!(crate::list(view,"items").iter().all(|item|item["id"]!="a"),"donor is outside recipient scope");
    assert!(crate::row(view,"posts","post-a").is_ok(),"unselected donor source is still evidence");
    assert!(crate::list(view,"knowledge_versions").len()<crate::list(d,"knowledge_versions").len());
    assert!(crate::list(view,"materials").is_empty());
    assert!(!view.to_string().contains("HISTORICAL_TRANSCRIPT_ARCHIVE"));
    crate::knowledge::validate_catalog(view).unwrap();
}
fn assert_evidence_parity(d: &Value, view: &Value) {
    let ids = [json!("a")];
    let full = crate::engine_prepare::build_request(d,&ids,None).unwrap();
    let scoped = crate::engine_prepare::build_request(view,&ids,None).unwrap();
    assert_eq!(full["dependencyDigest"],scoped["dependencyDigest"]);
    for key in ["items","branches","posts","materials","customerCases","knowledgeManifest"] {
        assert_eq!(full["request"][key],scoped["request"][key],"{key}");
    }
    let cases = &scoped["request"]["customerCases"];
    assert_eq!(cases[0]["messages"][0]["itemId"],"prior");
    assert_eq!(cases[0]["priorContractRequests"][0]["sourceItemId"],"prior");
    assert_eq!(crate::prepare_plan::build(d,&body()).unwrap(),crate::prepare_plan::build(view,&body()).unwrap());
}

#[test]
fn family_reader_keeps_conflict_corpus_without_loading_thread_or_paid_history(){
    let mut d=fixture(crate::accounts::Profile::LikeAvto);
    d["jobs"]=json!([{"id":"paid","result":{"text":"PRIVATE_HISTORY".repeat(100000)}}]);
    let view=family_project(&d,&["a".into(),"b".into()]).unwrap();
    assert_eq!(view["items"].as_array().unwrap().len(),2);
    for table in FAMILY {assert_eq!(view[*table],d[*table]);}
    for table in ["branches","jobs","operations","proposals","conversations"] {assert_eq!(view[table],json!([]));}
    let selected=vec!["a".to_owned(),"b".to_owned()];
    assert_eq!(crate::prepare_plan::family_windows(&d,&selected,100,2,&crate::now()).unwrap(),
        crate::prepare_plan::family_windows(&view,&selected,100,2,&crate::now()).unwrap());
}

#[tokio::test]
async fn sqlite_family_projection_resolves_confirmed_copies_with_unselected_donor_without_archive(){
    let folder=tempfile::tempdir().unwrap();
    let pool=crate::open_db(&folder.path().join("workspace.sqlite")).await.unwrap();let db=Database::Sqlite(pool.clone());
    let d=confirmed_family_fixture();
    sqlx::query("UPDATE workspace SET payload=? WHERE id=1").bind(d.to_string()).execute(&pool).await.unwrap();
    let view=db.read_preparation_families(&["b".into(),"other-platform".into()]).await.unwrap();
    assert_eq!(view,family_project(&d,&["b".into(),"other-platform".into()]).unwrap());
    assert_confirmed_family_projection(&d,&view);assert_eq!(db.read().await.unwrap(),d);db.close().await;
}

#[tokio::test]
#[ignore = "requires a fresh isolated PostgreSQL fixture; run this selector alone"]
async fn postgres_family_projection_resolves_confirmed_copies_with_unselected_donor_without_archive(){
    std::env::var("COMMUNITYHERO_WRITER_V51_TEST_URL").expect("explicit isolated fixture URL");
    let db=super::super::writer_v51_fixture_db().await;let d=confirmed_family_fixture();
    db.change(|state|{*state=d.clone();Ok(())}).await.unwrap();let before=db.read().await.unwrap();
    let view=db.read_preparation_families(&["b".into(),"other-platform".into()]).await.unwrap();
    assert_eq!(view,family_project(&before,&["b".into(),"other-platform".into()]).unwrap());
    assert_confirmed_family_projection(&before,&view);assert_eq!(db.read().await.unwrap(),before);db.close().await;
}

#[test]
fn planner_projection_keeps_complete_author_tenant_and_catalog_evidence() {
    for profile in [crate::accounts::Profile::LikeAvto,crate::accounts::Profile::BawRussia] {
        let d = fixture(profile);
        let view = project(&d,&["a".into(),"b".into()]).unwrap();
        assert_evidence_parity(&d,&view);
        for key in COMPLETE { assert_eq!(d[*key],view[*key],"{key}"); }
        assert_eq!(view["items"],d["items"]);
        assert_eq!(view["branches"],d["branches"]);
        assert!(crate::row(&view,"items","prior").unwrap().get("text").is_some());
        assert_eq!(view["branches"].as_array().unwrap().len(),5);
    }
}

#[test]
fn planner_unknown_alias_hold_preserves_independent_recipient_and_never_mutates() {
    let mut d = fixture(crate::accounts::Profile::LikeAvto);
    add(&mut d,"old-alias","different-author","VK");
    crate::row_mut(&mut d,"items","old-alias").unwrap()["conversationKey"] = json!("thread-a");
    d["operations"] = json!([{"id":"uncertain","itemId":"old-alias","status":"unknown","target":{},"evidence":{"receipt":"retain"}}]);
    let before = d.clone();
    let view = project(&d,&["a".into(),"b".into()]).unwrap();
    let plan = crate::prepare_plan::build(&view,&body()).unwrap();
    assert_eq!(plan,crate::prepare_plan::build(&d,&body()).unwrap());
    assert_eq!(plan["held"][0]["itemId"],"a");
    assert_eq!(plan["held"][0]["reason"],"preparation_scope_reserved");
    assert_eq!(plan["batches"][0]["itemIds"],json!(["b"]));
    assert_eq!(view["operations"],d["operations"]);
    assert_eq!(d,before);
}

#[test]
fn planner_retained_paid_owner_holds_only_affected_scope() {
    let mut d = fixture(crate::accounts::Profile::LikeAvto);
    let scheduled = crate::engine_prepare::schedule(&mut d,crate::engine_prepare::Input {item_ids:vec!["a".into()],instruction:None}).unwrap();
    let job = crate::row_mut(&mut d,"jobs",&scheduled.job_id).unwrap();
    job["status"] = json!("failed");
    job["scopeModelAttempt"] = json!({"status":"unknown"});
    let before = d.clone();
    let view = project(&d,&["a".into(),"b".into()]).unwrap();
    let full = crate::prepare_plan::build(&d,&body()).unwrap();
    assert_eq!(full,crate::prepare_plan::build(&view,&body()).unwrap());
    assert_eq!(full["held"][0]["itemId"],"a");
    assert_eq!(full["held"][0]["reason"],"preparation_scope_reserved");
    assert_eq!(full["batches"][0]["itemIds"],json!(["b"]));
    assert!(crate::list(&view,"scopeOwners").iter().any(|owner|owner["id"]==scheduled.job_id));
    assert_eq!(d,before);
}

#[test]
fn planner_does_not_invent_stale_disposition_for_another_captured_recipient() {
    let mut d=fixture(crate::accounts::Profile::LikeAvto);
    let b=crate::row_mut(&mut d,"items","b").unwrap();
    b["branchId"]=json!("branch-a");b["postId"]=json!("post-a");b["postKey"]=json!("object:a");b["conversationKey"]=json!("thread-a");
    let scheduled=crate::engine_prepare::schedule(&mut d,crate::engine_prepare::Input{item_ids:vec!["a".into(),"b".into()],instruction:None}).unwrap();
    crate::row_mut(&mut d,"jobs",&scheduled.job_id).unwrap()["status"]=json!("completed");
    let bundle=crate::row(&d,"jobs",&scheduled.job_id).unwrap()["prepareBundle"].clone();
    for id in ["a","b"] {
        let fingerprint=crate::prepare_bundle::review_fingerprint(&d,id).unwrap();
        crate::list_mut(&mut d,"proposals").push(json!({"id":format!("stale-{id}"),"itemId":id,"status":"stale",
            "prepareRunId":scheduled.job_id,"prepareBundleId":bundle["id"],"prepareBundleDigest":bundle["digest"],
            "reviewContextDigest":fingerprint,"staleReason":"Review source context changed"}));
    }
    crate::row_mut(&mut d,"items","a").unwrap()["text"]=json!("Genuinely changed selected comment");
    assert_ne!(crate::prepare_bundle::review_fingerprint(&d,"a").unwrap(),d["proposals"][0]["reviewContextDigest"].as_str().unwrap());
    assert_eq!(crate::prepare_bundle::review_fingerprint(&d,"b").unwrap(),d["proposals"][1]["reviewContextDigest"].as_str().unwrap());
    let before=d.clone();let view=project(&d,&["a".into()]).unwrap();
    let selected=json!({"itemIds":["a"]});
    let plan=crate::prepare_plan::build(&d,&selected).unwrap();
    assert_eq!(plan["held"][0]["reason"],"preparation_scope_reserved");
    assert!(plan["batches"].as_array().unwrap().is_empty());
    assert_eq!(plan,crate::prepare_plan::build(&view,&selected).unwrap());
    assert_eq!(d,before);
}

#[test]
fn planner_omits_paid_history_without_dropping_current_alias_controls() {
    let mut d = fixture(crate::accounts::Profile::LikeAvto);
    d["jobs"] = json!([{"id":"old-paid","kind":"assistant","status":"completed","result":{"private":"PAID_HISTORY".repeat(200_000)}}]);
    d["conversations"] = json!([{"id":"private-chat","messages":[{"text":"PRIVATE_CHAT".repeat(200_000)}]}]);
    let view = project(&d,&["a".into(),"b".into()]).unwrap();
    assert_evidence_parity(&d,&view);
    let full_bytes=d.to_string().len();let scoped_bytes=view.to_string().len();
    eprintln!("planner synthetic bytes: full={full_bytes} scoped={scoped_bytes}; items={} full_branches={} scoped_branches={}",
        crate::list(&view,"items").len(),crate::list(&d,"branches").len(),crate::list(&view,"branches").len());
    assert!(scoped_bytes*100<full_bytes);
    assert!(view["jobs"].as_array().unwrap().is_empty());
    assert!(view["conversations"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn sqlite_preparation_plan_read_is_consistent_and_nonmutating() {
    let folder=tempfile::tempdir().unwrap();
    let pool=crate::open_db(&folder.path().join("workspace.sqlite")).await.unwrap();
    let db=Database::Sqlite(pool.clone());
    let d=fixture(crate::accounts::Profile::LikeAvto);
    sqlx::query("UPDATE workspace SET payload=? WHERE id=1").bind(d.to_string()).execute(&pool).await.unwrap();
    let view=db.read_preparation_plan(&["a".into(),"b".into()]).await.unwrap();
    assert_evidence_parity(&d,&view);
    let families=db.read_preparation_families(&["a".into(),"b".into()]).await.unwrap();
    assert_eq!(families,family_project(&d,&["a".into(),"b".into()]).unwrap());
    assert_eq!(db.read().await.unwrap(),d);
    db.close().await;
}

#[tokio::test]
#[ignore = "requires a fresh isolated PostgreSQL fixture; run this selector alone"]
async fn postgres_preparation_plan_scope_parity() {
    std::env::var("COMMUNITYHERO_WRITER_V51_TEST_URL").expect("explicit isolated fixture URL");
    let db=super::super::writer_v51_fixture_db().await;
    let mut d=fixture(crate::accounts::Profile::LikeAvto);
    add(&mut d,"old-alias","different-author","VK");
    crate::row_mut(&mut d,"items","old-alias").unwrap()["conversationKey"]=json!("thread-a");
    let scheduled=crate::engine_prepare::schedule(&mut d,crate::engine_prepare::Input{item_ids:vec!["b".into()],instruction:None}).unwrap();
    let job=crate::row_mut(&mut d,"jobs",&scheduled.job_id).unwrap();
    job["status"]=json!("failed");job["scopeModelAttempt"]=json!({"status":"unknown"});
    d["operations"]=json!([{"id":"uncertain","itemId":"old-alias","status":"unknown","target":{},"evidence":{"receipt":"retain"}}]);
    db.change(|state|{*state=d.clone();Ok(())}).await.unwrap();
    let before=db.read().await.unwrap();
    let view=db.read_preparation_plan(&["a".into(),"b".into()]).await.unwrap();
    assert_eq!(crate::prepare_plan::build(&before,&body()).unwrap(),crate::prepare_plan::build(&view,&body()).unwrap());
    assert_eq!(view,project(&before,&["a".into(),"b".into()]).unwrap());
    let families=db.read_preparation_families(&["a".into(),"b".into()]).await.unwrap();
    assert_eq!(families,family_project(&before,&["a".into(),"b".into()]).unwrap());
    assert_eq!(crate::prepare_plan::family_windows(&before,&["a".into(),"b".into()],100,2,&crate::now()).unwrap(),
        crate::prepare_plan::family_windows(&families,&["a".into(),"b".into()],100,2,&crate::now()).unwrap());
    assert_eq!(db.read().await.unwrap(),before);
    db.close().await;
}
