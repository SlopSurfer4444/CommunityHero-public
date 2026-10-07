use super::*;
const AT:&str="2026-09-24T12:00:00Z";
fn profile()->Value{let mut p=json!({"version":1,"account":"likeavto","model":"gpt-6-astra","reasoningEffort":"medium","promptVersion":"review-fixture",
    "instructionSha256":"b".repeat(64),"toolsProfileSha256":"c".repeat(64),"runtimeSha256":"d".repeat(64),"cliSha256":"e".repeat(64)});p["profileSha256"]=json!(hash(&p));p}
fn uncapped_profile()->Value{
    let mut p=profile();p.as_object_mut().unwrap().remove("profileSha256");
    p["version"]=json!(3);p["model"]=json!(crate::codex_model_policy::MODEL);p["webCallLimit"]=Value::Null;
    p["cliSha256"]=json!(crate::codex_model_policy::CLI_SHA256);
    p["promptVersion"]=json!("communityhero-drafting-v21-review-uncapped-evidence");
    p["profileSha256"]=json!(hash(&p));p
}
fn uncapped_result(request:&Value,calls:u64,source_count:usize)->Value{
    let mut value=result(request,calls);let p=uncapped_profile();
    for key in ["model","reasoningEffort","cliSha256","promptVersion"]{value["runMetadata"][key]=p[key].clone();}
    value["runMetadata"]["modelProfile"]=json!(crate::codex_model_policy::PROFILE);
    let r=&mut value["runMetadata"]["research"];
    r["model"]=p["model"].clone();r["modelProfile"]=json!(crate::codex_model_policy::PROFILE);r["webCallLimit"]=Value::Null;
    if source_count>0{r["status"]=json!("completed");r["sources"]=json!((0..source_count).map(|n|json!({
        "itemId":request["items"][0]["id"],"url":format!("https://manufacturer.example/source-{n}"),"title":"Primary source","claim":"Observed fact"})).collect::<Vec<_>>());}
    value
}

#[test]
fn uncapped_review_more_than_fifty_calls_and_sources_bind_full_composite_provenance(){
    let (mut d,run)=fixture(1);initialize(&mut d,&run,&uncapped_profile()).unwrap();
    let request=reserve(&mut d,&run,AT).unwrap().unwrap();
    assert_eq!(request["reviewChunk"]["version"],2);assert_eq!(request["reviewChunk"]["maxWebCalls"],Value::Null);
    save(&mut d,&run,&request,Ok(&uncapped_result(&request,80,80)),AT).unwrap();
    let job=row(&d,"jobs",&run).unwrap();validate_usage(&job["preparationStages"]["reviewChunks"]).unwrap();
    let combined=aggregate(job).unwrap();let metadata=composite_metadata(&combined).unwrap();
    assert_eq!(metadata["chargedWebCalls"],80);assert_eq!(metadata["observedCompletedWebCalls"],80);
    assert_eq!(metadata["webCallLimit"],Value::Null);assert_eq!(research_projection(&metadata).unwrap()["sources"].as_array().unwrap().len(),80);
    let mut forged=combined.clone();forged["runMetadata"].as_object_mut().unwrap().remove("webCallLimit");
    assert!(composite_metadata(&forged).is_err(),"large usage cannot be relabeled as a historical eight-call receipt");
    let mut forged=combined;forged["runMetadata"]["profile"]=profile();assert!(composite_metadata(&forged).is_err());
}

#[test]
fn uncapped_unknown_spend_stays_unknown_after_successful_explicit_retry(){
    let (mut d,run)=fixture(1);initialize(&mut d,&run,&uncapped_profile()).unwrap();
    let request=reserve(&mut d,&run,AT).unwrap().unwrap();
    save(&mut d,&run,&request,Err("ADAPTER_TIMEOUT"),AT).unwrap();
    let state=&row(&d,"jobs",&run).unwrap()["preparationStages"]["reviewChunks"];
    let prior=state["chunks"][0]["attempts"][0].clone();
    assert_eq!(state["usage"]["chargedWebCalls"],Value::Null);assert_eq!(state["usage"]["actualTotalWebCallsKnown"],false);
    assert_eq!(state["usage"]["unknownAttemptCount"],1);
    let retry=reserve(&mut d,&run,AT).unwrap().unwrap();save(&mut d,&run,&retry,Ok(&uncapped_result(&retry,80,0)),AT).unwrap();
    let job=row(&d,"jobs",&run).unwrap();assert_eq!(job["preparationStages"]["reviewChunks"]["chunks"][0]["attempts"][0],prior);
    let result=aggregate(job).unwrap();let metadata=composite_metadata(&result).unwrap();
    assert_eq!(metadata["chargedWebCalls"],Value::Null);assert_eq!(metadata["unknownAttemptCount"],1);
    assert_eq!(metadata["observedCompletedWebCalls"],80);assert_eq!(metadata["actualTotalWebCallsKnown"],false);
    let mut forged=job["preparationStages"]["reviewChunks"].clone();forged["usage"]["chargedWebCalls"]=json!(80);
    assert!(validate_usage(&forged).is_err());
}
fn fixture(count:usize)->(Value,String){
    fixture_named(count,"")
}
fn fixture_source(count:usize,prefix:&str)->Value{
    let mut d=crate::empty();crate::accounts::initialize(&mut d,crate::accounts::Profile::LikeAvto).unwrap();
    for key in ["knowledge_entries","knowledge_versions","feedback"]{if !d[key].is_array(){d[key]=json!([]);}}
    d["items"]=json!((0..count).map(|n|json!({"id":format!("{prefix}i{n}"),"itemId":format!("{prefix}c{n}"),"objectId":format!("{prefix}object"),"platform":"VK",
        "postKey":format!("{prefix}post"),"conversationKey":format!("{prefix}thread{n}"),"branchId":format!("{prefix}branch"),"postId":format!("{prefix}post"),"revision":1,"draft":"","workflow":"attention","providerStatus":"new"})).collect::<Vec<_>>());
    d["posts"]=json!([{"id":format!("{prefix}post"),"postKey":format!("{prefix}post"),"objectId":format!("{prefix}object"),"platform":"VK","text":"Post"}]);
    d["branches"]=json!([{"id":format!("{prefix}branch"),"postId":format!("{prefix}post"),"messages":[],"contextComplete":true}]);
    d
}
fn fixture_named(count:usize,prefix:&str)->(Value,String){
    let d=fixture_source(count,prefix);
    let ids:Vec<_>=rows(&d,"items").iter().map(|i|i["id"].clone()).collect();
    let bundle=legacy_bundle(&d,&ids);
    capture_first_fixture(d,bundle)
}
fn capture_first_fixture(d:Value,bundle:Value)->(Value,String){
    capture_first_fixture_with_groups(d,bundle,None,false)
}
fn capture_first_fixture_with_groups(mut d:Value,bundle:Value,groups:Option<Value>,automatic:bool)->(Value,String){
    let ids=bundle["itemIds"].as_array().unwrap().clone();let request=bundle["request"].clone();
    let purpose=if automatic{"auto_prepare"}else{"engine_prepare"};
    let run=crate::new_job(&mut d,"assistant",purpose).unwrap();
    let j=crate::row_mut(&mut d,"jobs",&run).unwrap();j["purpose"]=json!(purpose);j["prepareBundle"]=bundle;
    j["preparationStages"]=json!({"first":null,"review":null});j["selectedItemIds"]=json!(ids);j["held"]=json!([]);
    if let Some(groups)=groups{j["preparationStages"]["groupAdmission"]=groups;}
    if automatic{
        j["refId"]=ids[0].clone();
        for item in d["items"].as_array_mut().unwrap(){item["autoPreparation"]=json!({"jobId":run,"status":"running","attempts":1});}
    }
    let mut first=json!({"text":"First pass retained","sources":[],"proposals":[],"assessments":ids.iter().map(|id|json!({"itemId":id,"outcome":"needs_attention","reason":"Needs review","tags":["needs_fact"]})).collect::<Vec<_>>()});
    if crate::preparation_materials::enabled(&request){
        first["runMetadata"]=crate::editorial_review::fixture_metadata();
        crate::model_material_receipt::fixture_result(&mut d,&run,&request,&mut first).unwrap();
    }
    super::super::record_first(&mut d,&run,&first,AT).unwrap();(d,run)
}
fn current_material_review_fixture(count:usize)->(Value,String){
    // A chunk-runner harness, not a new engine scheduling route (which is
    // single-pass). All current proofs exist BEFORE recording the first paid
    // result; no historical checkpoint is upgraded or rewritten.
    let mut d=fixture_source(count,"");d["posts"][0]["attachments"]=json!([]);
    let ids=rows(&d,"items").iter().map(|i|i["id"].clone()).collect::<Vec<_>>();
    let mut bundle=crate::prepare_bundle::build_engine_capture(&d,&ids,&[]).unwrap();
    bundle["request"]["purpose"]=json!("triage");
    crate::decision_media::attach_request(&d,&mut bundle["request"]).unwrap();
    crate::preparation_unit::attach(&d,&mut bundle,AT).unwrap();
    crate::preparation_materials::attach_request(&d,&mut bundle["request"]).unwrap();
    bundle["digest"]=json!(hash(&bundle["request"]));
    capture_first_fixture(d,bundle)
}
fn current_material_review_preflight(d:&Value,run:&str)->ApiResult<()>{
    let job=row(d,"jobs",run)?;let bundle=&job["prepareBundle"];
    if job["status"]!="running"{return Err(conflict("Review fixture owner is not running"));}
    crate::prepare_bundle::current(d,bundle).map_err(conflict)?;
    crate::preparation_unit::current_bundle(d,bundle,&now()).map_err(conflict)?;
    crate::preparation_materials::require_request(d,&bundle["request"]).map_err(conflict)?;
    current(d,job)
}
fn legacy_bundle(d:&Value,ids:&[Value])->Value{
    // Immutable two-pass checkpoints predate strict-family and mandatory-material
    // capture. Construct their historical evidence directly, never downgrade a
    // freshly scheduled request or manufacture a modern delivery receipt.
    let mut bundle=crate::prepare_bundle::build_engine_capture(d,ids,&[]).unwrap();
    bundle["request"]["purpose"]=json!("triage");
    bundle["request"]["visualNeedContract"]=json!(prepare_bundle::visual::CONTRACT);
    bundle["request"]["visualSelection"]=prepare_bundle::visual::empty();
    crate::decision_media::attach_request(d,&mut bundle["request"]).unwrap();
    bundle["digest"]=json!(hash(&bundle["request"]));
    bundle
}
fn result(request:&Value,calls:u64)->Value{
    let p=profile();json!({"text":"Reviewed","sources":[],"proposals":[],
        "assessments":rows(request,"items").iter().map(|i|json!({"itemId":i["id"],"outcome":"needs_attention","reason":"Needs source","tags":["needs_fact"]})).collect::<Vec<_>>(),
        "runMetadata":{"schemaVersion":1,"model":p["model"],"reasoningEffort":p["reasoningEffort"],"promptVersion":p["promptVersion"],
            "instructionSha256":p["instructionSha256"],"inputSha256":"f".repeat(64),"cliSha256":p["cliSha256"],"elapsedMs":20,"completedAt":AT,
            "reviewChunk":request["reviewChunk"],"imageEvidence":[],
            "research":{"version":1,"status":"no_sources","model":p["model"],"reasoningEffort":"medium","instructionSha256":p["instructionSha256"],
                "inputSha256":"f".repeat(64),"toolsProfileSha256":p["toolsProfileSha256"],"elapsedMs":20,"completedAt":AT,"webCalls":calls,"sources":[]}}})
}

#[test]
fn unknown_reservations_never_become_zero_spend(){
    let state=json!({"chunks":[{"attempts":[{"status":"unknown","reservedWebCalls":2,"inputBytes":10},
        {"status":"completed","reservedWebCalls":2,"observedWebCalls":1,"inputBytes":10}]}]});
    assert_eq!(usage(&state),(3,20));
    let bad=json!({"chunks":[{"attempts":[{"status":"completed","reservedWebCalls":2,"inputBytes":10}]}]});
    assert_eq!(usage(&bad).0,8);
}
#[test]
fn unfinished_chunks_share_only_the_unspent_budget(){
    for (count,limits) in [(1,vec![8]),(26,vec![4,8]),(76,vec![2,3,4,8])] {
        let (mut d,run)=fixture(count);initialize(&mut d,&run,&profile()).unwrap();
        for limit in limits {
            let request=reserve(&mut d,&run,AT).unwrap().unwrap();
            assert_eq!(request["reviewChunk"]["maxWebCalls"],limit);
            save(&mut d,&run,&request,Ok(&result(&request,0)),AT).unwrap();
        }
        assert!(reserve(&mut d,&run,AT).unwrap().is_none());
        assert_eq!(aggregate(row(&d,"jobs",&run).unwrap()).unwrap()["runMetadata"]["chargedWebCalls"],0);
    }
}

#[test]
fn completed_spend_reallocates_only_the_remaining_calls(){
    let (mut d,run)=fixture(26);initialize(&mut d,&run,&profile()).unwrap();
    let first=reserve(&mut d,&run,AT).unwrap().unwrap();
    assert_eq!(first["reviewChunk"]["maxWebCalls"],4);
    save(&mut d,&run,&first,Ok(&result(&first,2)),AT).unwrap();
    let saved_first=row(&d,"jobs",&run).unwrap()["preparationStages"]["reviewChunks"]["chunks"][0]["attempts"][0].clone();
    let second=reserve(&mut d,&run,AT).unwrap().unwrap();
    assert_eq!(second["reviewChunk"]["maxWebCalls"],6);
    assert_eq!(row(&d,"jobs",&run).unwrap()["preparationStages"]["reviewChunks"]["chunks"][0]["attempts"][0],saved_first);
    save(&mut d,&run,&second,Ok(&result(&second,6)),AT).unwrap();
    let state=&row(&d,"jobs",&run).unwrap()["preparationStages"]["reviewChunks"];
    assert_eq!(usage(state).0,WEB_BUDGET);
    assert_eq!(aggregate(row(&d,"jobs",&run).unwrap()).unwrap()["runMetadata"]["chargedWebCalls"],WEB_BUDGET);
}
#[test]
fn uncertain_single_chunk_uses_full_reservation_and_cannot_replay(){
    let (mut d,run)=fixture(1);initialize(&mut d,&run,&profile()).unwrap();
    let request=reserve(&mut d,&run,AT).unwrap().unwrap();
    assert_eq!(request["reviewChunk"]["maxWebCalls"],WEB_BUDGET);
    save(&mut d,&run,&request,Err("ADAPTER_TIMEOUT"),AT).unwrap();
    let before=d.clone();
    assert!(reserve(&mut d,&run,AT).is_err());
    assert_eq!(d,before,"no budget refund or second dispatch after uncertain outcome");
    let state=&row(&d,"jobs",&run).unwrap()["preparationStages"]["reviewChunks"];
    assert_eq!(usage(state).0,WEB_BUDGET);
    assert_eq!(state["usage"]["unknownAttemptCount"],1);
}
#[test]
fn exact_explicit_auto_review_resume_clears_only_its_hold(){
    let (mut d,run)=fixture(1);initialize(&mut d,&run,&profile()).unwrap();
    let job=row_mut(&mut d,"jobs",&run).unwrap();job["purpose"]=json!("auto_prepare");job["refId"]=json!("i0");job["status"]=json!("failed");
    d["items"][0]["autoPreparation"]=json!({"jobId":run,"status":"error","reviewResumeRequired":true});
    let plan=row(&d,"jobs",&run).unwrap()["preparationStages"]["reviewChunks"]["planDigest"].as_str().unwrap().to_owned();
    assert!(claim_resume(&mut d,&run,&"0".repeat(64)).is_err());
    assert_eq!(d["items"][0]["autoPreparation"]["reviewResumeRequired"],true);
    claim_resume(&mut d,&run,&plan).unwrap();
    assert_eq!(d["items"][0]["autoPreparation"]["reviewResumeRequired"],false);
    assert_eq!(d["items"][0]["autoPreparation"]["jobId"],run);
    assert!(rows(&d,"operations").is_empty());
}
#[test]
fn chunk_requests_keep_supporting_context_and_exact_first_pass_subset(){
    let request=json!({"items":[{"id":"a"},{"id":"b"}],"posts":[{"id":"p","text":"support"}],
        "firstPass":{"text":"full explanation","assessments":[{"itemId":"a"},{"itemId":"b"}],"proposals":[{"itemId":"b"}]},
        "materials":[{"id":"policy","text":"policy"}]});
    let part=subset(&request,&json!(["a"]));
    assert_eq!(part["posts"],request["posts"]);assert_eq!(part["materials"],request["materials"]);
    assert_eq!(part["items"],json!([{"id":"a"}]));assert_eq!(part["firstPass"]["assessments"],json!([{"itemId":"a"}]));
    assert_eq!(part["firstPass"]["proposals"],json!([]));
}

#[test]
fn chunk_requests_scope_29_customer_cases_without_losing_history_or_source(){
    let ids:Vec<_>=(0..29).map(|n|json!(format!("recipient-{n}"))).collect();
    let cases:Vec<_>=ids.iter().map(|id|json!({"itemId":id,"accountId":"LikeAvto",
        "authorId":format!("author-{}",id.as_str().unwrap()),"scope":"account_platform_author",
        "messages":[{"itemId":"unattached-history","text":"Earlier statement on another post"}],
        "brandReplies":[{"sourceItemId":"unattached-history","text":"Earlier brand reply"}],
        "priorContractRequests":[{"sourceItemId":"unattached-history","text":"Earlier contract request"}]})).collect();
    let request=json!({"items":ids.iter().map(|id|json!({"id":id})).collect::<Vec<_>>(),
        "customerCases":cases,"firstPass":{"text":"Original full explanation","sources":[],
            "assessments":ids.iter().map(|id|json!({"itemId":id,"outcome":"reply"})).collect::<Vec<_>>(),
            "proposals":ids.iter().map(|id|json!({"itemId":id,"text":"First draft"})).collect::<Vec<_>>()},
        "posts":[{"id":"post","postKey":"shared","text":"Shared original post"}],
        "branches":[{"id":"branch","messages":[{"id":"unattached-history","text":"Full branch context"}]}],
        "materials":[{"id":"material","itemIds":ids,"text":"Shared evidence"}],
        "knowledgeManifest":[{"entryId":"entry","versionId":"version","mediaBinding":[{"postKey":"shared"}]}],
        "contextMetadata":{"digest":"original-digest"}});
    let original=request.clone();let original_hash=hash(&request);let mut covered=BTreeSet::new();
    for (index,chunk_ids) in ids.chunks(25).enumerate(){
        let part=subset(&request,&json!(chunk_ids));
        let expected=if index==0{25}else{4};
        assert_eq!(rows(&part,"items").len(),expected);
        assert_eq!(rows(&part,"customerCases").len(),expected);
        for case in rows(&part,"customerCases"){
            assert!(chunk_ids.contains(&case["itemId"]),"adapter rejects a case for an unattached recipient");
            assert!(covered.insert(case["itemId"].as_str().unwrap().to_owned()));
            assert_eq!(Some(case),rows(&request,"customerCases").iter().find(|original|original["itemId"]==case["itemId"]),
                "retain all same-author history even when its message IDs are outside the chunk");
        }
        for key in ["posts","branches","materials","knowledgeManifest","contextMetadata"]{
            assert_eq!(part[key],request[key],"preserve {key}");
        }
        for key in ["assessments","proposals"]{
            assert_eq!(rows(&part["firstPass"],key).len(),expected);
            assert!(rows(&part["firstPass"],key).iter().all(|v|chunk_ids.contains(&v["itemId"])));
        }
        assert_eq!(part["firstPass"]["text"],request["firstPass"]["text"]);
    }
    assert_eq!(covered.len(),29);
    assert_eq!(request,original);assert_eq!(hash(&request),original_hash);
}

#[test]
fn chunk_requests_preserve_customer_case_presence_and_invalid_shapes(){
    let base=json!({"items":[{"id":"a"},{"id":"b"}],"firstPass":{"assessments":[],"proposals":[]}});
    assert!(subset(&base,&json!(["a"])).get("customerCases").is_none());
    for value in [Value::Null,json!([]),json!({"itemId":"b"}),json!("invalid")]{
        let mut request=base.clone();request["customerCases"]=value.clone();
        assert_eq!(subset(&request,&json!(["a"]))["customerCases"],value);
    }
    let mut request=base;
    request["customerCases"]=json!([{"itemId":"b","messages":[]}]);
    assert_eq!(subset(&request,&json!(["a"]))["customerCases"],json!([]));
}

#[test]
fn checkpoints_skip_completed_chunk_and_assemble_only_exact_complete_set(){
    let (mut d,run)=fixture(26);initialize(&mut d,&run,&profile()).unwrap();
    let request=reserve(&mut d,&run,AT).unwrap().unwrap();
    assert_eq!(rows(&request,"items").len(),25);assert_eq!(request["account"],"likeavto");
    let mut raw=request.clone();raw.as_object_mut().unwrap().remove("reviewChunk");raw.as_object_mut().unwrap().remove("reviewChunkRequestJson");assert_eq!(request["reviewChunk"]["requestSha256"],hash(&raw));
    assert!(reserve(&mut d,&run,AT).is_err(),"running reservation is exclusive");
    save(&mut d,&run,&request,Ok(&result(&request,1)),AT).unwrap();
    assert!(aggregate(row(&d,"jobs",&run).unwrap()).is_err());assert!(rows(&d,"proposals").is_empty());
    let next=reserve(&mut d,&run,AT).unwrap().unwrap();assert_eq!(next["items"][0]["id"],"i9");
    save(&mut d,&run,&next,Ok(&result(&next,0)),AT).unwrap();
    assert!(reserve(&mut d,&run,AT).unwrap().is_none());
    let combined=aggregate(row(&d,"jobs",&run).unwrap()).unwrap();assert_eq!(rows(&combined,"assessments").len(),26);
    let metadata=prepare_bundle::generation_metadata(&combined).unwrap().unwrap();assert_eq!(metadata["schemaVersion"],2);
    assert_eq!(metadata["chargedWebCalls"],1);assert_eq!(metadata["observedCompletedWebCalls"],1);
    prepare_bundle::validate_image_evidence_binding(&metadata,&row(&d,"jobs",&run).unwrap()["prepareBundle"]).unwrap();
    super::super::record_review(&mut d,&run,Ok(&combined),AT).unwrap();
    assert_eq!(d["preparationResearch"][0]["review"]["research"]["version"],2);
}

#[tokio::test]
async fn scoped_sqlite_checkpoint_accepts_domain_reserve_and_zero_web_save(){
    let (mut initial,run)=fixture(1);
    initial["jobs"].as_array_mut().unwrap().push(json!({"id":"other-history","kind":"assistant",
        "purpose":"auto_prepare","status":"completed","private":"retain"}));
    let folder=tempfile::tempdir().unwrap();
    let db=crate::Database::Sqlite(crate::open_db(&folder.path().join("review.sqlite")).await.unwrap());
    db.change(|d|{*d=initial;Ok(())}).await.unwrap();
    let (_,changed)=db.change_preparation_review_checkpoint_observed(&run,|d|initialize(d,&run,&profile())).await.unwrap();
    assert!(changed);
    let (request,changed)=db.change_preparation_review_checkpoint_observed(&run,|d|reserve(d,&run,AT)).await.unwrap();
    assert!(changed);
    let request=request.unwrap();
    let ((),changed)=db.change_preparation_review_checkpoint_observed(&run,|d|
        save(d,&run,&request,Ok(&result(&request,0)),AT)).await.unwrap();
    assert!(changed);
    let saved=db.read().await.unwrap();
    let state=&row(&saved,"jobs",&run).unwrap()["preparationStages"]["reviewChunks"];
    assert_eq!(state["status"],"completed");
    assert_eq!(state["usage"]["chargedWebCalls"],0);
    assert_eq!(state["usage"]["observedCompletedWebCalls"],0);
    assert_eq!(state["usage"]["reservedUnconfirmedWebCalls"],0);
    assert_eq!(state["usage"]["actualTotalWebCallsKnown"],true);
    assert_eq!(row(&saved,"jobs","other-history").unwrap()["private"],"retain");
    let (next,changed)=db.change_preparation_review_checkpoint_observed(&run,|d|reserve(d,&run,AT)).await.unwrap();
    assert!(next.is_none());assert!(!changed);
    assert_eq!(db.read().await.unwrap(),saved);
    db.close().await;
}

#[tokio::test]
async fn scoped_sqlite_checkpoint_keeps_uncertain_error_after_source_changes(){
    let (initial,run)=fixture(1);
    let folder=tempfile::tempdir().unwrap();
    let db=crate::Database::Sqlite(crate::open_db(&folder.path().join("uncertain.sqlite")).await.unwrap());
    db.change(|d|{*d=initial;Ok(())}).await.unwrap();
    db.change_preparation_review_checkpoint_observed(&run,|d|initialize(d,&run,&profile())).await.unwrap();
    let (request,_)=db.change_preparation_review_checkpoint_observed(&run,|d|reserve(d,&run,AT)).await.unwrap();
    let request=request.unwrap();
    db.change(|d|{d["posts"][0]["text"]=json!("New source evidence");Ok(())}).await.unwrap();
    let (_,changed)=db.change_preparation_review_checkpoint_observed(&run,|d|
        save(d,&run,&request,Err("ADAPTER_TIMEOUT"),AT)).await.unwrap();
    assert!(changed);
    let saved=db.read().await.unwrap();
    let job=row(&saved,"jobs",&run).unwrap();
    assert_eq!(job["status"],"running");
    assert_eq!(job["preparationStages"]["reviewChunks"]["status"],"held");
    assert_eq!(job["preparationStages"]["reviewChunks"]["chunks"][0]["attempts"][0]["status"],"unknown");
    assert_eq!(saved["posts"][0]["text"],"New source evidence");
    db.close().await;
}

#[tokio::test]
async fn stale_review_settlement_survives_restart_without_admission_or_budget_refund(){
    for variant in ["source","revision","human_edit","unknown_operation"] {
        let (initial,run)=fixture(26);
        let folder=tempfile::tempdir().unwrap();let path=folder.path().join("settlement.sqlite");
        let db=crate::Database::Sqlite(crate::open_db(&path).await.unwrap());
        db.change(|d|{*d=initial;Ok(())}).await.unwrap();
        crate::native_fixture_owner_repair::initialize_db(&db).await.unwrap();
        db.change_preparation_review_checkpoint_observed(&run,|d|initialize(d,&run,&profile())).await.unwrap();
        let (request,_)=db.change_preparation_review_checkpoint_observed(&run,|d|reserve(d,&run,AT)).await.unwrap();
        let request=request.unwrap();
        db.change(|d|{
            match variant {
                "source"=>d["posts"][0]["text"]=json!("Changed shared source"),
                "revision"=>d["items"][0]["revision"]=json!(2),
                "human_edit"=>{d["items"][0]["draft"]=json!("Operator text");d["items"][0]["draftEdited"]=json!(true);},
                _=>d["operations"]=json!([{"id":"protected","itemId":"i0","status":"unknown"}]),
            }
            Ok(())
        }).await.unwrap();
        let before=db.read().await.unwrap();
        let (_,changed)=db.change_preparation_review_checkpoint_observed(&run,|d|
            save(d,&run,&request,Ok(&result(&request,2)),AT)).await.unwrap();
        assert!(changed,"{variant}");
        let saved=db.read().await.unwrap();
        let job=row(&saved,"jobs",&run).unwrap();let state=&job["preparationStages"]["reviewChunks"];
        assert_eq!(state["chunks"][0]["attempts"][0]["status"],"completed");
        assert_eq!(state["usage"]["chargedWebCalls"],2);
        assert_eq!(state["usage"]["observedCompletedWebCalls"],2);
        assert_eq!(state["usage"]["reservedUnconfirmedWebCalls"],0);
        assert!(!state["chunks"][0]["result"].is_null());
        for key in ["items","posts","branches","operations","proposals","approvals"] {assert_eq!(saved[key],before[key],"{variant}: {key}");}
        assert!(current(&saved,job).is_err(),"stale result is only retained evidence");
        assert!(db.change_preparation_review_checkpoint_observed(&run,|d|reserve(d,&run,AT)).await.is_err(),"no spending on stale siblings");
        assert!(db.change_preparation_review_checkpoint_observed(&run,|d|save(d,&run,&request,Err("ADAPTER_TIMEOUT"),AT)).await.is_err(),"a settled return cannot be demoted");
        assert_eq!(db.read().await.unwrap(),saved);
        db.close().await;
        let db=crate::Database::Sqlite(crate::open_db(&path).await.unwrap());
        assert_eq!(db.read().await.unwrap(),saved,"commit survives process restart");
        db.change(|d|{crate::recover(d).unwrap();Ok(())}).await.unwrap();
        let recovered=db.read().await.unwrap();let job=row(&recovered,"jobs",&run).unwrap();
        assert_eq!(job["preparationStages"]["reviewChunks"],*state);
        let plan=state["planDigest"].as_str().unwrap();
        assert!(db.change(|d|claim_resume(d,&run,plan)).await.is_err(),"restart cannot authorize stale replay");
        db.close().await;
    }
}

#[test]
fn review_settlement_rejects_changed_owner_and_request_before_confirming_usage(){
    for variant in ["account","bundle","plan","profile","request","attempt","finished"] {
        let (mut d,run)=fixture(1);initialize(&mut d,&run,&profile()).unwrap();
        let mut request=reserve(&mut d,&run,AT).unwrap().unwrap();
        match variant {
            "account"=>d["account"]=json!("BAW Russia"),
            "bundle"=>row_mut(&mut d,"jobs",&run).unwrap()["prepareBundle"]["id"]=json!("replacement"),
            "plan"=>row_mut(&mut d,"jobs",&run).unwrap()["preparationStages"]["reviewChunks"]["chunks"][0]["itemIds"]=json!(["foreign"]),
            "profile"=>row_mut(&mut d,"jobs",&run).unwrap()["preparationStages"]["reviewChunks"]["profile"]["runtimeSha256"]=json!("0".repeat(64)),
            "request"=>request["posts"][0]["text"]=json!("Altered request with same contract"),
            "attempt"=>request["reviewChunk"]["attemptId"]=json!("old-worker"),
            _=>{save(&mut d,&run,&request,Err("ADAPTER_TIMEOUT"),AT).unwrap();},
        }
        let before=d.clone();
        assert!(save(&mut d,&run,&request,Ok(&result(&request,0)),AT).is_err(),"{variant}");
        assert_eq!(d,before,"rejected ownership does not settle usage: {variant}");
        assert_eq!(usage(&row(&d,"jobs",&run).unwrap()["preparationStages"]["reviewChunks"]).0,WEB_BUDGET);
    }
}

#[tokio::test]
async fn invalid_stale_review_return_keeps_unknown_reservation_in_storage(){
    let (initial,run)=fixture(1);let folder=tempfile::tempdir().unwrap();
    let db=crate::Database::Sqlite(crate::open_db(&folder.path().join("invalid.sqlite")).await.unwrap());
    db.change(|d|{*d=initial;Ok(())}).await.unwrap();
    db.change_preparation_review_checkpoint_observed(&run,|d|initialize(d,&run,&profile())).await.unwrap();
    let (request,_)=db.change_preparation_review_checkpoint_observed(&run,|d|reserve(d,&run,AT)).await.unwrap();let request=request.unwrap();
    db.change(|d|{d["posts"][0]["text"]=json!("Changed");Ok(())}).await.unwrap();
    let mut invalid=result(&request,0);invalid["runMetadata"]["cliSha256"]=json!("0".repeat(64));
    let before=db.read().await.unwrap();
    let error=db.change_preparation_review_checkpoint_observed(&run,|d|save(d,&run,&request,Ok(&invalid),AT)).await.unwrap_err();
    assert_eq!(db.read().await.unwrap(),before);
    db.change_preparation_review_checkpoint_observed(&run,|d|save(d,&run,&request,Err(&error.1),AT)).await.unwrap();
    let saved=db.read().await.unwrap();let state=&row(&saved,"jobs",&run).unwrap()["preparationStages"]["reviewChunks"];
    assert_eq!(state["usage"]["observedCompletedWebCalls"],0);assert_eq!(state["usage"]["chargedWebCalls"],WEB_BUDGET);
    assert_eq!(state["usage"]["unknownAttemptCount"],1);assert_eq!(state["usage"]["actualTotalWebCallsKnown"],false);
    assert!(state["chunks"][0]["result"].is_null());
    db.close().await;
}

// Shared by SQLite and the strictly guarded, ignored fresh PostgreSQL fixture.
pub(crate) async fn exercise_stale_settlement_storage(db:&crate::Database){
    let (mut initial,run)=fixture(1);
    initial["jobs"].as_array_mut().unwrap().push(json!({"id":"other-history","kind":"assistant","purpose":"engine_prepare","status":"completed","private":"retain"}));
    db.change(|d|{*d=initial;Ok(())}).await.unwrap();
    db.change_preparation_review_checkpoint_observed(&run,|d|initialize(d,&run,&profile())).await.unwrap();
    let (request,_)=db.change_preparation_review_checkpoint_observed(&run,|d|reserve(d,&run,AT)).await.unwrap();let request=request.unwrap();
    db.change(|d|{d["posts"][0]["text"]=json!("Changed before storage settlement");Ok(())}).await.unwrap();
    let before=db.read().await.unwrap();
    for variant in ["empty_completed","forged_provenance","usage_refund","unknown_result","unknown_observed","unknown_retryable","simultaneous_reserve"] {
        let rejected=db.change_preparation_review_checkpoint_observed(&run,|d|{
            match variant {
                "empty_completed"=>{
                    let state=&mut row_mut(d,"jobs",&run)?["preparationStages"]["reviewChunks"];
                    state["chunks"][0]["attempts"][0]["status"]=json!("completed");
                    state["chunks"][0]["attempts"][0]["observedWebCalls"]=json!(0);
                    update_usage(state);
                },
                "unknown_result"|"unknown_observed"|"unknown_retryable"=>{
                    save(d,&run,&request,Err("ADAPTER_TIMEOUT"),AT)?;
                    let state=&mut row_mut(d,"jobs",&run)?["preparationStages"]["reviewChunks"];
                    match variant {
                        "unknown_result"=>state["chunks"][0]["result"]=result(&request,0),
                        "unknown_observed"=>state["chunks"][0]["attempts"][0]["observedWebCalls"]=json!(0),
                        _=>state["chunks"][0]["attempts"][0]["retryable"]=json!(false),
                    }
                },
                _=>{
                    save(d,&run,&request,Ok(&result(&request,2)),AT)?;
                    let state=&mut row_mut(d,"jobs",&run)?["preparationStages"]["reviewChunks"];
                    match variant {
                        "forged_provenance"=>{
                            state["chunks"][0]["result"]["runMetadata"]["cliSha256"]=json!("0".repeat(64));
                            state["chunks"][0]["attempts"][0]["resultDigest"]=json!(hash(&state["chunks"][0]["result"]));
                        },
                        "usage_refund"=>state["chunks"][0]["attempts"][0]["observedWebCalls"]=json!(0),
                        _=>{
                            let mut extra=state["chunks"][0]["attempts"][0].clone();
                            extra["id"]=json!("extra-attempt");extra["contract"]["attemptId"]=json!("extra-attempt");
                            extra["status"]=json!("running");extra["reservedWebCalls"]=json!(1);extra["contract"]["maxWebCalls"]=json!(1);
                            for key in ["finishedAt","observedWebCalls","resultDigest"]{extra.as_object_mut().unwrap().remove(key);}
                            state["chunks"][0]["attempts"].as_array_mut().unwrap().push(extra);
                        },
                    }
                    update_usage(state);
                },
            }
            Ok(())
        }).await;
        assert!(rejected.is_err(),"storage must independently reject {variant}");
        assert_eq!(db.read().await.unwrap(),before,"rejected {variant} must roll back without budget refund");
    }
    let mut expected=before.clone();save(&mut expected,&run,&request,Ok(&result(&request,2)),AT).unwrap();
    let (_,changed)=db.change_preparation_review_checkpoint_observed(&run,|d|{
        assert_eq!(d["posts"],before["posts"],"scoped SQL must read current changed sources");
        assert_eq!(row(d,"jobs",&run)?,row(&before,"jobs",&run)?);
        save(d,&run,&request,Ok(&result(&request,2)),AT)
    }).await.unwrap();
    assert!(changed);assert_eq!(db.read().await.unwrap(),expected,"scoped persistence matches full domain settlement");
    assert!(current(&expected,row(&expected,"jobs",&run).unwrap()).is_err());
    assert_eq!(row(&expected,"jobs","other-history").unwrap()["private"],"retain");
    for key in ["proposals","approvals","operations"]{assert_eq!(expected[key],before[key]);}
}

#[tokio::test]
async fn sqlite_storage_independently_validates_stale_settlement_and_rejects_refunds(){
    let folder=tempfile::tempdir().unwrap();
    let db=crate::Database::Sqlite(crate::open_db(&folder.path().join("guard.sqlite")).await.unwrap());
    exercise_stale_settlement_storage(&db).await;db.close().await;
}

#[tokio::test]
async fn sqlite_restart_keeps_finished_chunks_and_burns_uncertain_reservation(){
    let (mut d,run)=fixture(26);initialize(&mut d,&run,&profile()).unwrap();
    let first=reserve(&mut d,&run,AT).unwrap().unwrap();save(&mut d,&run,&first,Ok(&result(&first,1)),AT).unwrap();
    let mut second=reserve(&mut d,&run,AT).unwrap().unwrap();let old_attempt=second["reviewChunk"]["attemptId"].clone();
    // Simulate a previously dispatched v60 two-call contract. Its saved
    // reservation and wire contract remain authoritative after restart.
    second["reviewChunk"]["maxWebCalls"]=json!(2);
    let old_contract=second["reviewChunk"].clone();
    let old=&mut row_mut(&mut d,"jobs",&run).unwrap()["preparationStages"]["reviewChunks"]["chunks"][1]["attempts"][0];
    old["contract"]=old_contract.clone();old["reservedWebCalls"]=json!(2);
    let path=std::env::temp_dir().join(format!("review-chunks-{}.sqlite",crate::id()));
    let db=crate::Database::Sqlite(crate::open_db(&path).await.unwrap());db.change(|out|{*out=d;Ok(())}).await.unwrap();crate::native_fixture_owner_repair::initialize_db(&db).await.unwrap();db.close().await;
    let db=crate::Database::Sqlite(crate::open_db(&path).await.unwrap());
    db.change(|d|{crate::recover(d).unwrap();Ok(())}).await.unwrap();
    db.change(|d|{
        let plan=row(d,"jobs",&run)?["preparationStages"]["reviewChunks"]["planDigest"].as_str().unwrap().to_owned();
        claim_resume(d,&run,&plan)?;
        assert!(claim_resume(d,&run,&plan).is_err());
        let req=reserve(d,&run,AT)?.unwrap();assert_ne!(req["reviewChunk"]["attemptId"],old_attempt);
        assert_eq!(req["reviewChunk"]["maxWebCalls"],5);
        assert_eq!(req["items"][0]["id"],"i9");
        let state=&row(d,"jobs",&run)?["preparationStages"]["reviewChunks"];
        assert_eq!(usage(state).0,8);assert_eq!(state["chunks"][0]["attempts"].as_array().unwrap().len(),1);
        assert_eq!(state["chunks"][1]["attempts"][0]["contract"],old_contract);
        save(d,&run,&req,Ok(&result(&req,0)),AT)?;
        assert_eq!(aggregate(row(d,"jobs",&run)?)?["runMetadata"]["chargedWebCalls"],3);Ok(())
    }).await.unwrap();db.close().await;
}

#[test]
fn stale_context_profile_plan_and_protected_operations_cannot_reuse_chunks(){
    let (mut base,run)=fixture(2);initialize(&mut base,&run,&profile()).unwrap();
    for mode in ["revision","source","company","operation","partition","profile"]{
        let mut d=base.clone();match mode{
            "revision"=>d["items"][1]["revision"]=json!(2),
            "source"=>d["posts"][0]["text"]=json!("Changed"),
            "company"=>d["account"]=json!("BAW Russia"),
            "operation"=>d["operations"]=json!([{"id":"op","itemId":"i1","status":"unknown"}]),
            "partition"=>row_mut(&mut d,"jobs",&run).unwrap()["preparationStages"]["reviewChunks"]["chunks"][0]["itemIds"]=json!(["i0"]),
            "profile"=>row_mut(&mut d,"jobs",&run).unwrap()["preparationStages"]["reviewChunks"]["profile"]["runtimeSha256"]=json!("0".repeat(64)),_=>unreachable!()
        };assert!(reserve(&mut d,&run,AT).is_err(),"{mode}");
    }
    let mut changed=profile();changed["runtimeSha256"]=json!("0".repeat(64));assert!(initialize(&mut base,&run,&changed).is_err());
}

#[test]
fn rejected_partial_or_foreign_result_never_finishes_checkpoint(){
    for mode in ["missing","foreign","duplicate","cap","profile"] {
        let (mut d,run)=fixture(2);initialize(&mut d,&run,&profile()).unwrap();let req=reserve(&mut d,&run,AT).unwrap().unwrap();let mut r=result(&req,0);
        match mode {
            "missing"=>{r["assessments"].as_array_mut().unwrap().pop();},
            "foreign"=>r["assessments"][0]["itemId"]=json!("other"),
            "duplicate"=>r["assessments"][1]["itemId"]=r["assessments"][0]["itemId"].clone(),
            "cap"=>r["runMetadata"]["research"]["webCalls"]=json!(req["reviewChunk"]["maxWebCalls"].as_u64().unwrap()+1),
            "profile"=>r["runMetadata"]["reviewChunk"]["profileSha256"]=json!("0".repeat(64)),_=>unreachable!()
        }
        assert!(save(&mut d,&run,&req,Ok(&r),AT).is_err(),"{mode}");
        assert!(aggregate(row(&d,"jobs",&run).unwrap()).is_err());assert!(rows(&d,"proposals").is_empty());
        assert_eq!(usage(&row(&d,"jobs",&run).unwrap()["preparationStages"]["reviewChunks"]).0,8);
    }
}

#[test]
fn chunk_attempt_cap_is_durable_and_failures_do_not_refund(){
    let (mut d,run)=fixture(76);initialize(&mut d,&run,&profile()).unwrap();
    for _ in 0..2 {
        let req=reserve(&mut d,&run,AT).unwrap().unwrap();save(&mut d,&run,&req,Err("Adapter timed out"),AT).unwrap();
    }
    assert!(reserve(&mut d,&run,AT).is_err());assert_eq!(usage(&row(&d,"jobs",&run).unwrap()["preparationStages"]["reviewChunks"]).0,4);
}

#[test]
fn aggregate_web_and_input_limits_survive_successes_and_ambiguous_attempts(){
    let (mut d,run)=fixture(76);initialize(&mut d,&run,&profile()).unwrap();
    for _ in 0..4 {
        let req=reserve(&mut d,&run,AT).unwrap().unwrap();save(&mut d,&run,&req,Ok(&result(&req,2)),AT).unwrap();
    }
    let r=aggregate(row(&d,"jobs",&run).unwrap()).unwrap();assert_eq!(r["runMetadata"]["chargedWebCalls"],8);
    let (mut d,run)=fixture(76);initialize(&mut d,&run,&profile()).unwrap();
    let req=reserve(&mut d,&run,AT).unwrap().unwrap();save(&mut d,&run,&req,Err("ADAPTER_TIMEOUT"),AT).unwrap();
    for _ in 0..3 {
        let req=reserve(&mut d,&run,AT).unwrap().unwrap();
        let calls=req["reviewChunk"]["maxWebCalls"].as_u64().unwrap().min(2);
        save(&mut d,&run,&req,Ok(&result(&req,calls)),AT).unwrap();
    }
    let last=reserve(&mut d,&run,AT).unwrap().unwrap();assert_eq!(last["reviewChunk"]["maxWebCalls"],1);
    save(&mut d,&run,&last,Ok(&result(&last,1)),AT).unwrap();
    assert!(reserve(&mut d,&run,AT).unwrap().is_none());
    assert!(aggregate(row(&d,"jobs",&run).unwrap()).is_ok());
    let state=&row(&d,"jobs",&run).unwrap()["preparationStages"]["reviewChunks"];
    assert_eq!(state["usage"]["chargedWebCalls"],8);assert_eq!(state["usage"]["actualTotalWebCallsKnown"],false);
    assert_eq!(state["usage"]["unknownAttemptCount"],1);
    let (mut d,run)=fixture(26);initialize(&mut d,&run,&profile()).unwrap();
    let req=reserve(&mut d,&run,AT).unwrap().unwrap();save(&mut d,&run,&req,Ok(&result(&req,0)),AT).unwrap();
    // A prior persisted attempt has consumed the complete input allowance.
    // The next reservation must fail before creating another paid attempt.
    let state=&mut row_mut(&mut d,"jobs",&run).unwrap()["preparationStages"]["reviewChunks"];
    state["chunks"][0]["attempts"][0]["inputBytes"]=state["maxInputBytes"].clone();
    let before=d.clone();assert!(reserve(&mut d,&run,AT).is_err());assert_eq!(d,before);
}

#[test]
fn completed_chunk_sources_reenter_company_bound_cache_with_oldest_ttl(){
    let (mut d,run)=fixture(26);initialize(&mut d,&run,&profile()).unwrap();
    let first=reserve(&mut d,&run,AT).unwrap().unwrap();let mut r=result(&first,1);
    r["runMetadata"]["research"]["status"]=json!("completed");
    r["runMetadata"]["research"]["sources"]=json!([{"itemId":"i0","url":"https://example.com/fact","title":"Fact","claim":"Observed source claim","trust":"source_only"}]);
    save(&mut d,&run,&first,Ok(&r),AT).unwrap();
    let selected=crate::research_cache::select(&d,rows(&d,"items"),rows(&d,"posts"),AT).unwrap();assert!(rows(&selected,"materials").is_empty());
    let next=reserve(&mut d,&run,AT).unwrap().unwrap();let mut r=result(&next,0);r["runMetadata"]["research"]["completedAt"]=json!("2026-09-24T12:10:00Z");
    save(&mut d,&run,&next,Ok(&r),AT).unwrap();
    let combined=aggregate(row(&d,"jobs",&run).unwrap()).unwrap();
    super::super::record_review(&mut d,&run,Ok(&combined),"2026-09-24T12:10:00Z").unwrap();
    assert_eq!(d["preparationResearch"][0]["review"]["research"]["completedAt"],"2026-09-24T12:00:00+00:00");
    let selected=crate::research_cache::select(&d,rows(&d,"items"),rows(&d,"posts"),"2026-09-24T12:10:01Z").unwrap();
    assert_eq!(rows(&selected,"materials").len(),1);
    let expired=crate::research_cache::select(&d,rows(&d,"items"),rows(&d,"posts"),"2026-09-25T12:00:01Z").unwrap();assert!(rows(&expired,"materials").is_empty());
    let mut other=d.clone();other["account"]=json!("BAW Russia");
    assert!(rows(&crate::research_cache::select(&other,rows(&other,"items"),rows(&other,"posts"),"2026-09-24T12:10:01Z").unwrap(),"materials").is_empty());
    d["preparationResearch"][0]["review"]["research"]["sources"][0]["claim"]=json!("Forged flattened cache claim");
    d["preparationResearch"][0]["checksum"]=json!(crate::research_cache::checksum(&d["preparationResearch"][0]));
    assert!(rows(&crate::research_cache::select(&d,rows(&d,"items"),rows(&d,"posts"),"2026-09-24T12:10:01Z").unwrap(),"materials").is_empty());
}

// Replace synthetic chunk payloads without discarding test_app's real native owner.
// Keep this in the test module: production lifecycle admission remains unchanged.
async fn replace_chunk_fixture(app:&App,mut fixture:Value)->ApiResult<()> {
    let (lifecycle,owner)=app.db.change(|state| {
        let owner=crate::runtime_lifecycle::bound_admission_token(
            state,&app.lifecycle_owner,crate::runtime_lifecycle::AdmissionClass::Preparation)?;
        if fixture.get("runtimeLifecycle").is_some()||fixture["account"]!=state["account"] {
            return Err(conflict("Chunk fixture cannot supply or retarget native lifecycle"));
        }
        let lifecycle=state["runtimeLifecycle"].clone();
        fixture["runtimeLifecycle"]=lifecycle.clone();
        *state=fixture;
        Ok((lifecycle,owner))
    }).await?;
    let saved=app.db.read().await?;
    assert_eq!(saved["runtimeLifecycle"],lifecycle,"fixture replacement preserves owner, epoch, phase and history");
    assert_eq!(crate::runtime_lifecycle::bound_admission_token(
        &saved,&app.lifecycle_owner,crate::runtime_lifecycle::AdmissionClass::Preparation)?,owner);
    Ok(())
}

#[tokio::test]
async fn chunk_fixture_replacement_preserves_paid_history_and_rejects_owner_reset() {
    let (mut app,_temp)=crate::tests::test_app().await;
    let before=app.db.read().await.unwrap()["runtimeLifecycle"].clone();
    for _ in 0..2 {
        let (seed,run)=completed_recovery_fixture(2,false);
        let paid=row(&seed,"jobs",&run).unwrap().clone();
        replace_chunk_fixture(&app,seed).await.unwrap();
        let stored=app.db.read().await.unwrap();
        assert_eq!(stored["runtimeLifecycle"],before);
        assert_eq!(*row(&stored,"jobs",&run).unwrap(),paid,"replacement keeps complete paid outputs and attempt history");
    }
    let admitted_identity=app.lifecycle_owner.clone();
    let mut foreign=(*admitted_identity).clone();foreign.runtime_id="foreign-chunk-owner".into();
    app.lifecycle_owner=std::sync::Arc::new(foreign);
    let blocked=app.db.read().await.unwrap();
    assert!(replace_chunk_fixture(&app,fixture(1).0).await.is_err());
    assert_eq!(app.db.read().await.unwrap(),blocked,"foreign App cannot reset the fixture owner");
    app.lifecycle_owner=admitted_identity;
    app.db.change_runtime_lifecycle_with_ledger(|d| {
        let owner=crate::runtime_lifecycle::current_owner(d,&app.lifecycle_owner)?;
        crate::runtime_lifecycle::begin_drain(d,&owner,&"c".repeat(64),"chunk-fixture-drain",false)?;
        Ok(())
    }).await.unwrap();
    let draining=app.db.read().await.unwrap();
    assert!(replace_chunk_fixture(&app,fixture(1).0).await.is_err());
    assert_eq!(app.db.read().await.unwrap(),draining,"replacement cannot reopen drain or erase paid evidence");
    app.db.close().await;
}

async fn adapter_fixture(fail_second:bool)->(App,tempfile::TempDir,String,std::path::PathBuf){
    let (mut app,temp)=crate::tests::test_app().await;let (d,run)=fixture(26);
    replace_chunk_fixture(&app,d).await.unwrap();
    app.node=std::env::var_os("COMMUNITYHERO_TEST_NODE").map(std::path::PathBuf::from)
        .unwrap_or_else(||std::path::PathBuf::from("node"));
    app.bridge=temp.path().join("chunk-adapter.mjs");let log=temp.path().join("model-calls.jsonl");
    let script=r#"import fs from 'node:fs/promises';import {createHash} from 'node:crypto';
let input='';for await(const part of process.stdin)input+=part;const r=JSON.parse(input);const profile=__PROFILE__;
if(r.purpose==='review_profile'){process.stdout.write(JSON.stringify({ok:true,result:profile}));process.exit(0);}
if(r.operation!=='assistant'||r.purpose!=='triage_review'||!r.reviewChunk)throw new Error('Unexpected operation');
await fs.appendFile(__LOG__,JSON.stringify({chunkId:r.reviewChunk.chunkId,attemptId:r.reviewChunk.attemptId})+'\n');
if(__FAIL__&&r.reviewChunk.chunkId==='chunk-2'){process.stdout.write(JSON.stringify({ok:false,error:{code:'ADAPTER_TIMEOUT'}}));process.exit(0);}
const result={text:'Reviewed',sources:[],proposals:[],assessments:r.items.map(i=>({itemId:i.id,outcome:'needs_attention',reason:'Needs fact',tags:['needs_fact']})),
runMetadata:{schemaVersion:1,model:profile.model,reasoningEffort:profile.reasoningEffort,promptVersion:profile.promptVersion,instructionSha256:profile.instructionSha256,
inputSha256:'f'.repeat(64),cliSha256:profile.cliSha256,elapsedMs:1,completedAt:__AT__,reviewChunk:r.reviewChunk,imageEvidence:[],
research:{version:1,status:'no_sources',model:profile.model,reasoningEffort:'medium',instructionSha256:profile.instructionSha256,inputSha256:'f'.repeat(64),toolsProfileSha256:profile.toolsProfileSha256,elapsedMs:1,completedAt:__AT__,webCalls:0,sources:[]}}};
if(r.mandatoryMaterialContract==='mandatory_post_materials_v1'){
 const b=r.postContextBundle;if(r.materialReadiness.status!=='ready'||b.members.some(m=>m.assets.length!==0))throw new Error('Fixture supports verified text-only requests');
 const sha=v=>createHash('sha256').update(JSON.stringify(v)).digest('hex');
 result.runMetadata.materialInvocation={schemaVersion:1,contract:r.mandatoryMaterialContract,completenessStatus:'complete',companyId:b.companyId,
 postContextBundleSha256:b.contentSha256,memberPins:b.members.map(m=>({postId:m.canonicalPostId,connectorBinding:m.connectorBinding,sourceVersion:m.postSourceVersion,postFieldsSha256:sha(m.fields)})),
 requiredPhotos:[],suppliedSpeech:[],actualTextInputSha256:result.runMetadata.inputSha256,instructionSha256:profile.instructionSha256,schemaSha256:'f'.repeat(64),cliSha256:profile.cliSha256,
 stagedPhotos:[],deliveredPhotos:[],optionalFrameRefs:[],stagedFrames:[],deliveredFrames:[]};
}
process.stdout.write(JSON.stringify({ok:true,result}));"#
        .replace("__PROFILE__",&profile().to_string()).replace("__LOG__",&json!(log.to_string_lossy()).to_string())
        .replace("__FAIL__",if fail_second{"true"}else{"false"}).replace("__AT__",&json!(AT).to_string());
    std::fs::write(&app.bridge,script).unwrap();(app,temp,run,log)
}
async fn terminal(app:&App,run:&str)->Value{
    tokio::time::timeout(std::time::Duration::from_secs(30),async{
        loop{let d=app.db.read().await.unwrap();let j=row(&d,"jobs",run).unwrap();
            if matches!(j["status"].as_str(),Some("failed"|"completed")){return j.clone();}
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    }).await.unwrap()
}
#[tokio::test]
async fn historical_engine_worker_resume_holds_without_materials_and_preserves_paid_chunks(){
    let (app,_temp,_initial_run,log)=adapter_fixture(false).await;
    let (mut d,run)=fixture(76);initialize(&mut d,&run,&profile()).unwrap();
    let first=reserve(&mut d,&run,AT).unwrap().unwrap();
    save(&mut d,&run,&first,Ok(&result(&first,0)),AT).unwrap();
    let pending=reserve(&mut d,&run,AT).unwrap().unwrap();
    save(&mut d,&run,&pending,Err("ADAPTER_TIMEOUT"),AT).unwrap();
    row_mut(&mut d,"jobs",&run).unwrap()["status"]=json!("failed");
    let original=row(&d,"jobs",&run).unwrap().clone();
    let request=review_request(&original).unwrap();
    assert!(request.get("mandatoryMaterialContract").is_none());
    assert!(request.get("strictGroupContract").is_none());
    assert!(original["preparationStages"]["first"]["result"].get("modelMaterialReceipt").is_none());
    replace_chunk_fixture(&app,d).await.unwrap();
    let body=json!({"expectedPlanDigest":original["preparationStages"]["reviewChunks"]["planDigest"]});
    resume(State(app.clone()),Path(run.clone()),Json(body)).await.unwrap();
    let held=terminal(&app,&run).await;
    assert_eq!(held["status"],"failed");
    assert!(held["error"].as_str().is_some_and(|e|e.contains("Unsupported strict preparation unit contract")),"{held}");
    assert_eq!(held["prepareBundle"],original["prepareBundle"]);
    assert_eq!(held["preparationStages"],original["preparationStages"],"HOLD retains first result, completed/unknown attempts and charged budget exactly");
    assert!(!log.exists(),"historical material absence must stop before any new model call");
    let saved=app.read().await.unwrap();
    for key in ["proposals","approvals","operations"]{assert!(rows(&saved,key).is_empty());}
    app.db.close().await;
}
#[tokio::test]
async fn current_material_chunk_worker_resume_after_failed_attempt_calls_only_unfinished_chunk(){
    let (app,_temp,_initial_run,log)=adapter_fixture(true).await;
    let (d,run)=current_material_review_fixture(76);let first=d["jobs"][0]["preparationStages"]["first"].clone();
    replace_chunk_fixture(&app,d).await.unwrap();
    assert!(crate::runtime_lifecycle_app::with_job(run.clone(),super::run(&app,&run,current_material_review_preflight)).await.is_err());
    let failed=app.db.read_job(&run).await.unwrap().unwrap();
    assert!(!failed["preparationStages"]["reviewChunks"]["chunks"][0]["result"].is_null());
    let completed_chunk=failed["preparationStages"]["reviewChunks"]["chunks"][0].clone();
    assert!(rows(&app.read().await.unwrap(),"proposals").is_empty());
    let script=std::fs::read_to_string(&app.bridge).unwrap().replace("if(true&&r.reviewChunk.chunkId", "if(false&&r.reviewChunk.chunkId");std::fs::write(&app.bridge,script).unwrap();
    let plan=failed["preparationStages"]["reviewChunks"]["planDigest"].as_str().unwrap();
    app.change(|d|{row_mut(d,"jobs",&run)?["status"]=json!("failed");claim_resume(d,&run,plan)?;Ok(())}).await.unwrap();
    let aggregate=crate::runtime_lifecycle_app::with_job(run.clone(),super::run(&app,&run,current_material_review_preflight)).await.unwrap();
    let completed=app.db.read_job(&run).await.unwrap().unwrap();
    assert_eq!(completed["preparationStages"]["reviewChunks"]["status"],"completed");
    assert_eq!(completed["preparationStages"]["first"],first);
    assert_eq!(completed["preparationStages"]["reviewChunks"]["chunks"][0],completed_chunk);
    let calls:Vec<Value>=std::fs::read_to_string(log).unwrap().lines().map(|s|serde_json::from_str(s).unwrap()).collect();
    assert_eq!(calls.iter().filter(|c|c["chunkId"]=="chunk-1").count(),1);
    assert_eq!(calls.iter().filter(|c|c["chunkId"]=="chunk-2").count(),2);
    assert_ne!(calls[1]["attemptId"],calls[2]["attemptId"]);
    assert_eq!(aggregate["runMetadata"]["schemaVersion"],2);assert_eq!(completed["preparationStages"]["reviewChunks"]["usage"]["chargedWebCalls"],3);
    assert_eq!(rows(&completed,"modelMaterialReceipts").len(),5,"first and four completed chunks retain real material receipts");
    assert!(completed["prepareOutcome"].is_null(),"chunk completion is not engine final admission");
    let saved=app.read().await.unwrap();for key in ["proposals","approvals","operations"]{assert!(rows(&saved,key).is_empty());}
    app.db.close().await;
}
#[tokio::test]
async fn ungrouped_historical_completed_chunks_hold_after_restart_without_new_model_calls(){
    let (app,_temp,run,log)=adapter_fixture(false).await;
    let preflight=|d:&Value,run:&str|current(d,row(d,"jobs",run)?);
    let result=super::run(&app,&run,preflight).await.unwrap();assert_eq!(rows(&result,"assessments").len(),26);
    app.change(|d|{crate::recover(d).unwrap();Ok(())}).await.unwrap();
    let d=app.read().await.unwrap();assert_eq!(row(&d,"jobs",&run).unwrap()["status"],"interrupted");
    let body=json!({"expectedPlanDigest":row(&d,"jobs",&run).unwrap()["preparationStages"]["reviewChunks"]["planDigest"]});
    // Complete paid chunks are retained, but this historical ungrouped capture
    // has neither the modern first contract nor an original group admission plan.
    let error=recover_completed(State(app.clone()),Path(run.clone()),Json(body)).await.err().unwrap();
    assert_eq!(error.1,"Unsupported strict preparation unit contract");
    assert_eq!(app.read().await.unwrap(),d,"HOLD preserves the first result, chunk requests/results and budget");
    assert!(row(&d,"jobs",&run).unwrap()["preparationStages"]["groupAdmission"].is_null());
    assert_eq!(std::fs::read_to_string(log).unwrap().lines().count(),2,"recovery reads the local profile without another model call");
    for key in ["proposals","approvals","operations"]{assert!(rows(&d,key).is_empty());}
    app.db.close().await;
}


#[tokio::test]
async fn final_success_skips_empty_reservation_but_keeps_remaining_chunks_and_restart_equivalent(){
    for count in [1,26] {
        let (app,_temp,_initial_run,log)=adapter_fixture(false).await;
        let (d,run)=fixture(count);replace_chunk_fixture(&app,d).await.unwrap();
        let preflight=|d:&Value,run:&str|current(d,row(d,"jobs",run)?);
        let (outcome,events)=crate::performance::capture(super::run(&app,&run,preflight)).await;
        let result=outcome.unwrap();let chunks=(count+24)/25;
        assert_eq!(rows(&result,"assessments").len(),count);
        assert_eq!(std::fs::read_to_string(&log).unwrap().lines().count(),chunks);
        assert_eq!(events.iter().filter(|e|e["stage"]=="preparation.review_checkpoint.writer.wait").count(),1+2*chunks,
            "one initialize, one reservation and one durable save per actual chunk; no final empty reservation");
        let completed=app.read().await.unwrap();let before=row(&completed,"jobs",&run).unwrap().clone();
        assert_eq!(before["preparationStages"]["reviewChunks"]["status"],"completed");
        assert_eq!(before["preparationStages"]["reviewChunks"]["usage"]["reservedUnconfirmedWebCalls"],0);
        assert!(rows(&completed,"operations").is_empty());
        let (restarted,restart_events)=crate::performance::capture(super::run(&app,&run,preflight)).await;
        assert_eq!(restarted.unwrap(),result,"complete-on-entry returns identical aggregate and original provenance");
        assert_eq!(restart_events.iter().filter(|e|e["stage"]=="preparation.review_checkpoint.writer.wait").count(),2,
            "historical complete-on-entry retains initialize and guarded empty reservation");
        assert_eq!(std::fs::read_to_string(&log).unwrap().lines().count(),chunks,"restart never recalls the model");
        assert_eq!(*row(&app.read().await.unwrap(),"jobs",&run).unwrap(),before,"journal and budget unchanged");
        app.db.close().await;
    }
}

#[tokio::test]
async fn completed_fast_path_rechecks_fresh_state_without_erasing_saved_results(){
    let (app,_temp,run,_log)=adapter_fixture(false).await;
    let preflight=|d:&Value,run:&str|current(d,row(d,"jobs",run)?);
    let accepted=super::run(&app,&run,preflight).await.unwrap();
    let baseline=app.read().await.unwrap();
    for variant in ["revision","source","unknown"] {
        let mut changed=baseline.clone();
        match variant {
            "revision"=>changed["items"][0]["revision"]=json!(2),
            "source"=>changed["posts"][0]["text"]=json!("Changed after durable review save"),
            _=>changed["operations"]=json!([{"id":"uncertain","itemId":"i0","status":"unknown"}]),
        }
        let saved=row(&changed,"jobs",&run).unwrap().clone();
        app.db.change(|d|{*d=changed;Ok(())}).await.unwrap();
        assert!(read_completed(&app,&run,preflight).await.is_err(),"fresh {variant} must reject completed result");
        assert_eq!(*row(&app.read().await.unwrap(),"jobs",&run).unwrap(),saved,"rejecting admission cannot erase completed chunk evidence");
    }
    app.db.change(|d|{*d=baseline;Ok(())}).await.unwrap();
    assert!(read_completed(&app,&run,|_,_|Err(conflict("Cancelled by caller preflight"))).await.is_err());
    assert_eq!(read_completed(&app,&run,preflight).await.unwrap(),accepted);
    app.db.close().await;
}

#[tokio::test]
async fn review_worker_settles_actual_return_before_stale_preflight_and_stops_spending(){
    let (app,temp,run,log)=adapter_fixture(false).await;
    let release=temp.path().join("release-review");
    let script=std::fs::read_to_string(&app.bridge).unwrap();
    let gated=script.replace("process.stdout.write(JSON.stringify({ok:true,result}));",&format!(
        "for(;;){{try{{await fs.access({});break;}}catch{{await new Promise(r=>setTimeout(r,10));}}}}\nprocess.stdout.write(JSON.stringify({{ok:true,result}}));",
        json!(release.to_string_lossy())));
    assert_ne!(gated,script);std::fs::write(&app.bridge,gated).unwrap();
    let worker=app.clone();let job_id=run.clone();
    let pending=tokio::spawn(async move {super::run(&worker,&job_id,|d,id|current(d,row(d,"jobs",id)?)).await});
    tokio::time::timeout(std::time::Duration::from_secs(15),async {
        while !log.exists(){tokio::time::sleep(std::time::Duration::from_millis(10)).await;}
    }).await.unwrap();
    app.db.change(|d|{d["posts"][0]["text"]=json!("Source changed before model return");Ok(())}).await.unwrap();
    std::fs::write(release,"return now").unwrap();
    assert!(tokio::time::timeout(std::time::Duration::from_secs(15),pending).await.unwrap().unwrap().is_err());
    let saved=app.db.read().await.unwrap();let state=&row(&saved,"jobs",&run).unwrap()["preparationStages"]["reviewChunks"];
    assert_eq!(state["chunks"][0]["attempts"][0]["status"],"completed");
    assert!(!state["chunks"][0]["result"].is_null());
    assert_eq!(state["usage"]["chargedWebCalls"],0);assert_eq!(state["usage"]["actualTotalWebCallsKnown"],true);
    assert!(state["chunks"][1]["attempts"].as_array().unwrap().is_empty());
    assert_eq!(std::fs::read_to_string(log).unwrap().lines().count(),1);
    for key in ["proposals","approvals","operations"]{assert!(rows(&saved,key).is_empty());}
    app.db.close().await;
}

fn completed_recovery_fixture(count:usize,automatic:bool)->(Value,String){
    completed_recovery_fixture_named(count,automatic,"")
}
fn completed_recovery_fixture_named(count:usize,automatic:bool,prefix:&str)->(Value,String){
    // Historical grouped recovery starts with a real pending group plan BEFORE
    // its first paid capture. Never retrofit groups or material proof to a saved
    // checkpoint. Only local drafts can be recovered from this legacy result.
    let mut d=fixture_source(count,prefix);
    for item in d["items"].as_array_mut().unwrap(){item["providerObservedAt"]=json!(crate::now());}
    let ids=rows(&d,"items").iter().map(|item|item["id"].clone()).collect::<Vec<_>>();
    let bundle=legacy_bundle(&d,&ids);
    let groups=crate::prepare_bundle::capture_groups(&d,&bundle).unwrap();
    let (mut d,run)=capture_first_fixture_with_groups(d,bundle,Some(groups),automatic);
    initialize(&mut d,&run,&profile()).unwrap();
    while let Some(request)=reserve(&mut d,&run,AT).unwrap(){
        let mut review=result(&request,1);
        review["assessments"]=json!(rows(&request,"items").iter().map(|item|json!({"itemId":item["id"],"outcome":"reply","reason":"Checked feedback","tags":["feedback"]})).collect::<Vec<_>>());
        review["proposals"]=json!(rows(&request,"items").iter().map(|item|json!({"itemId":item["id"],"kind":"reply_and_close","text":"Спасибо за отзыв!"})).collect::<Vec<_>>());
        save(&mut d,&run,&request,Ok(&review),AT).unwrap();
    }
    if automatic {
        for item in d["items"].as_array_mut().unwrap(){item["autoPreparation"]=json!({"jobId":run,"status":"error","reviewResumeRequired":true,"attempts":1});}
    }
    let job=row_mut(&mut d,"jobs",&run).unwrap();job["status"]=json!("interrupted");job["error"]=json!("Interrupted after settlement");
    assert!(!rows(&job["preparationStages"],"groupAdmission").is_empty());
    assert!(rows(&job["preparationStages"],"groupAdmission").iter().all(|group|group["status"]=="pending"));
    assert!(job["prepareOutcome"].is_null());assert!(rows(&d,"proposals").is_empty());
    (d,run)
}

// Reuse the immutable historical profile/checkpoint fixture for R9 final tests.
pub(crate) fn r9_final_fixture(automatic:bool,prefix:&str)->(Value,String,Value,String,Value){
    let (d,run)=completed_recovery_fixture_named(2,automatic,prefix);
    let job=row(&d,"jobs",&run).unwrap();
    let plan=job["preparationStages"]["reviewChunks"]["planDigest"].as_str().unwrap().to_owned();
    let result=aggregate(job).unwrap();
    (d,run,profile(),plan,result)
}

// Namespace source recipients/routes BEFORE native request capture and the
// actual initialize/reserve/save paid checkpoint lifecycle. No captured digest
// or retained reference is renamed or recomputed by the workload.
pub(crate) fn r9_final_workload_fixture(prefix:&str)->(Value,String,Value,String,Value){
    assert!(!prefix.is_empty());
    let (d,run)=completed_recovery_fixture_named(2,true,prefix);
    let job=row(&d,"jobs",&run).unwrap();
    let plan=job["preparationStages"]["reviewChunks"]["planDigest"].as_str().unwrap().to_owned();
    let result=aggregate(job).unwrap();(d,run,profile(),plan,result)
}

pub(crate) fn r9_final_singleton_fixture()->(Value,String,Value){
    let (mut d,run)=completed_recovery_fixture(1,true);
    row_mut(&mut d,"jobs",&run).unwrap()["status"]=json!("running");
    d["items"][0]["autoPreparation"]["status"]=json!("running");d["items"][0]["autoPreparation"]["reviewResumeRequired"]=json!(false);
    let result=aggregate(row(&d,"jobs",&run).unwrap()).unwrap();(d,run,result)
}

pub(crate) fn r9_final_grouped_fixture(hold:bool,settled_first:bool)->(Value,String,Value){
    r9_final_grouped_fixture_with_proof(hold,settled_first,None,false)
}

fn r9_final_grouped_fixture_with_proof(hold:bool,settled_first:bool,sibling_hold:Option<bool>,sibling_image:bool)->(Value,String,Value){
    let (mut d,run)=fixture(2);
    d["items"][1]["branchId"]=json!("other-branch");d["items"][1]["postId"]=json!("other-post");d["items"][1]["postKey"]=json!("other-post");
    d["branches"].as_array_mut().unwrap().push(json!({"id":"other-branch","postId":"other-post","messages":[],"contextComplete":true}));
    d["posts"].as_array_mut().unwrap().push(json!({"id":"other-post","postKey":"other-post","objectId":"object","platform":"VK","text":"Other post"}));
    if sibling_image{d["posts"][1]["attachments"]=json!([{"type":"photo","url":"https://images.example.com/sibling.png"}]);}
    for item in d["items"].as_array_mut().unwrap(){item["providerObservedAt"]=json!(crate::now());item["autoPreparation"]=json!({"jobId":run,"status":"running","attempts":1});}
    let ids=json!(["i0","i1"]);let mut bundle=legacy_bundle(&d,ids.as_array().unwrap());
    if sibling_image {
        // Acquired pixels require an exact opt-in in the captured request;
        // merely attaching a sibling image does not authorize staging it.
        bundle["request"]["visualNeedContract"]=json!(prepare_bundle::visual::CONTRACT);
        bundle["request"]["visualSelection"]=json!({"version":1,"postImages":[{"itemId":"i1","postId":"other-post",
            "attachmentIndices":[0],"reason":"Synthetic sibling acquisition binding"}]});
        bundle["digest"]=json!(hash(&bundle["request"]));
    }
    let groups=crate::prepare_bundle::capture_groups(&d,&bundle).unwrap();
    let job=row_mut(&mut d,"jobs",&run).unwrap();job["purpose"]=json!("auto_prepare");job["prepareBundle"]=bundle;
    job["preparationStages"]=json!({"first":null,"review":null,"groupAdmission":groups});
    let mut first=json!({"text":"First","sources":[],"proposals":[],"assessments":[
        {"itemId":"i0","outcome":"needs_attention","reason":"Needs review","tags":["needs_fact"]},
        {"itemId":"i1","outcome":"needs_attention","reason":"Needs review","tags":["needs_fact"]}]});
    if settled_first{
        first["assessments"][0]=json!({"itemId":"i0","outcome":"reply","reason":"Routine feedback","tags":["feedback"]});
        first["proposals"]=json!([{"itemId":"i0","kind":"reply_and_close","text":"Спасибо!"}]);
    }
    super::super::record_first(&mut d,&run,&first,AT).unwrap();
    if settled_first{
        let index=row(&d,"jobs",&run).unwrap()["preparationStages"]["groupAdmission"].as_array().unwrap().iter()
            .position(|g|rows(g,"itemIds").contains(&json!("i0"))).unwrap();
        let group=row(&d,"jobs",&run).unwrap()["preparationStages"]["groupAdmission"][index].clone();
        let subset=crate::engine_prepare::group_result(&first,&group["itemIds"].as_array().unwrap());
        let mut admission=crate::prepare_bundle::admit_group(&mut d,&run,&subset,&group).unwrap();
        admission["items"]=json!([{"itemId":"i0","status":"prepared"}]);
        let saved=&mut row_mut(&mut d,"jobs",&run).unwrap()["preparationStages"]["groupAdmission"][index];
        saved["status"]=json!("admitted");saved["admission"]=admission;
        d["items"][0]["autoPreparation"]["status"]=json!("prepared");
    }
    initialize(&mut d,&run,&profile()).unwrap();
    while let Some(request)=reserve(&mut d,&run,AT).unwrap(){
        let mut review=result(&request,1);
        if !hold{
            review["assessments"]=json!(rows(&request,"items").iter().map(|item|json!({"itemId":item["id"],"outcome":"reply","reason":"Checked feedback","tags":["feedback"]})).collect::<Vec<_>>());
            review["proposals"]=json!(rows(&request,"items").iter().map(|item|json!({"itemId":item["id"],"kind":"reply_and_close","text":"Спасибо за отзыв!"})).collect::<Vec<_>>());
        }
        if let Some(sibling_hold)=sibling_hold {
            if sibling_hold {
                review["assessments"][1]=json!({"itemId":"i1","outcome":"needs_attention","reason":"Incomplete primary evidence","tags":["needs_fact"]});
                review["proposals"]=json!(rows(&review,"proposals").iter().filter(|p|p["itemId"]!="i1").cloned().collect::<Vec<_>>());
                review["runMetadata"]["research"]["evidenceHolds"]=json!([{"version":1,"accountKey":"likeavto","itemId":"i1",
                    "url":"https://maker.example/configuration","reason":"incomplete_extraction",
                    "scope":{"model":"Q06","trim":"selected","market":"CN","modelYear":"2025"},
                    "extraction":{"status":"empty","observedAt":AT,"rowLabels":["Motor count"],"columnLabels":["selected"],"values":[]},
                    "renderedFallback":{"status":"unsupported","attempts":0,"capability":"web.run_text_only"}}]);
            }
            review["editorialEvidence"]=json!({"version":1,"contract":crate::editorial_review::CONTRACT,
                "entries":rows(&review,"proposals").iter().map(|p|json!({"itemId":p["itemId"],"kind":p["kind"],
                    "textSha256":crate::editorial_review::hash_text(p["text"].as_str().unwrap()),"decision":"accept","reason":"Explicit synthetic review",
                    "checks":{"companyRules":"pass","intent":"pass","factualScope":"pass"}})).collect::<Vec<_>>()});
        }
        if sibling_image {
            review["runMetadata"]["imageEvidence"]=json!([{"imageNumber":1,"itemId":"i1","itemIds":["i1"],"postId":"other-post",
                "origin":"post_attachment","attachmentIndex":0,"sha256":"a".repeat(64),"mime":"image/png","width":100,"height":100}]);
        }
        save(&mut d,&run,&request,Ok(&review),AT).unwrap();
    }
    let result=aggregate(row(&d,"jobs",&run).unwrap()).unwrap();(d,run,result)
}

#[test]
fn grouped_composite_admission_keeps_complete_provenance_and_only_group_drafts(){
    for sibling_hold in [false,true] {
        let (mut d,run,complete)=r9_final_grouped_fixture_with_proof(false,false,Some(sibling_hold),false);
        let checkpoints=row(&d,"jobs",&run).unwrap()["preparationStages"]["reviewChunks"].clone();
        let groups=row(&d,"jobs",&run).unwrap()["preparationStages"]["groupAdmission"].clone();
        let group=groups.as_array().unwrap().iter().find(|g|rows(g,"itemIds")==[json!("i0")]).unwrap();
        let scoped=crate::engine_prepare::group_result(&complete,rows(group,"itemIds"));
        assert!(prepare_bundle::generation_metadata(&scoped).is_err(),"a narrowed assessment set cannot retarget complete chunk provenance");
        let metadata=prepare_bundle::generation_metadata(&complete).unwrap().unwrap();
        let admitted=prepare_bundle::admit_group(&mut d,&run,&scoped,group).unwrap();
        assert_eq!(admitted["status"],"review");assert_eq!(rows(&admitted,"candidates").len(),1);
        assert_eq!(rows(&d,"proposals").len(),1);let proposal=&d["proposals"][0];assert_eq!(proposal["itemId"],"i0");
        assert_eq!(proposal["generationMetadata"]["chunks"],metadata["chunks"]);
        assert_eq!(proposal["editorialReview"]["decision"],"accept");
        assert_eq!(proposal["editorialReview"]["source"]["runMetadata"],metadata);
        assert_eq!(proposal["editorialReview"]["source"]["resultSha256"],hash(&complete));
        assert_eq!(row(&d,"jobs",&run).unwrap()["runMetadata"],metadata);
        assert_eq!(row(&d,"jobs",&run).unwrap()["preparationStages"]["reviewChunks"],checkpoints);
        let sibling=groups.as_array().unwrap().iter().find(|g|rows(g,"itemIds")==[json!("i1")]).unwrap();
        let scoped=crate::engine_prepare::group_result(&complete,rows(sibling,"itemIds"));
        let admitted=prepare_bundle::admit_group(&mut d,&run,&scoped,sibling).unwrap();
        assert_eq!(admitted["status"],if sibling_hold{"held"}else{"review"});
        assert_eq!(rows(&d,"proposals").len(),if sibling_hold{1}else{2});
        assert_eq!(row(&d,"jobs",&run).unwrap()["preparationStages"]["reviewChunks"],checkpoints);
    }
}

#[test]
fn grouped_composite_admission_rejects_forged_projection_and_foreign_group_without_writes(){
    let (base,run,complete)=r9_final_grouped_fixture_with_proof(false,false,Some(true),false);
    let group=row(&base,"jobs",&run).unwrap()["preparationStages"]["groupAdmission"][0].clone();
    let scoped=crate::engine_prepare::group_result(&complete,rows(&group,"itemIds"));
    for variant in ["text","assessment","editorial","chunk_recipients","chunk_digest","sibling_hold","group","missing_metadata","schema1"] {
        let mut forged=scoped.clone();let mut selected=group.clone();
        match variant {
            "text"=>{forged["proposals"][0]["text"]=json!("Changed final text");forged["editorialEvidence"]["entries"][0]["textSha256"]=json!(crate::editorial_review::hash_text("Changed final text"));},
            "assessment"=>forged["assessments"][0]["reason"]=json!("Changed decision"),
            "editorial"=>forged["editorialEvidence"]["entries"][0]["reason"]=json!("Changed review"),
            "chunk_recipients"=>forged["runMetadata"]["chunks"][0]["itemIds"]=json!(["i0"]),
            "chunk_digest"=>forged["runMetadata"]["chunks"][0]["resultDigest"]=json!("0".repeat(64)),
            "sibling_hold"=>{forged["runMetadata"]["chunks"][0]["metadata"]["research"].as_object_mut().unwrap().remove("evidenceHolds");},
            "missing_metadata"=>{forged.as_object_mut().unwrap().remove("runMetadata");},
            "schema1"=>{
                forged["runMetadata"]=complete["runMetadata"]["chunks"][0]["metadata"].clone();
                forged["runMetadata"].as_object_mut().unwrap().remove("research");
                assert!(prepare_bundle::generation_metadata(&forged).is_ok(),"the downgrade packet is otherwise valid legacy metadata");
            },
            _=>selected["key"]=json!("foreign-group"),
        }
        let mut d=base.clone();assert!(prepare_bundle::admit_group(&mut d,&run,&forged,&selected).is_err(),"{variant}");assert_eq!(d,base,"{variant}");
    }
}

#[test]
fn grouped_composite_admission_rejects_changed_durable_capture_without_writes(){
    let (base,run,complete)=r9_final_grouped_fixture_with_proof(false,false,Some(true),false);
    let group=row(&base,"jobs",&run).unwrap()["preparationStages"]["groupAdmission"][0].clone();
    let scoped=crate::engine_prepare::group_result(&complete,rows(&group,"itemIds"));
    for variant in ["result","digest","status","missing"] {
        let mut d=base.clone();let checkpoint=&mut row_mut(&mut d,"jobs",&run).unwrap()["preparationStages"]["reviewChunks"]["chunks"][0];
        match variant {
            "result"=>checkpoint["result"]["proposals"][0]["text"]=json!("Changed captured text"),
            "digest"=>checkpoint["attempts"][0]["resultDigest"]=json!("0".repeat(64)),
            "status"=>checkpoint["attempts"][0]["status"]=json!("unknown"),
            _=>checkpoint["result"]=Value::Null,
        }
        let before=d.clone();assert!(prepare_bundle::admit_group(&mut d,&run,&scoped,&group).is_err(),"{variant}");assert_eq!(d,before,"{variant}");
    }
}

#[test]
fn grouped_composite_admission_keeps_sibling_hold_and_company_guards(){
    let (base,run,_)=r9_final_grouped_fixture_with_proof(false,false,Some(true),false);
    let group=row(&base,"jobs",&run).unwrap()["preparationStages"]["groupAdmission"][0].clone();
    for variant in ["executable_hold","foreign_company"] {
        let mut d=base.clone();let checkpoint=&mut row_mut(&mut d,"jobs",&run).unwrap()["preparationStages"]["reviewChunks"]["chunks"][0];
        // Hostile durable input with a self-consistent digest must still fail
        // semantic/account validation; it cannot borrow the other group's proof.
        if variant=="executable_hold" {
            checkpoint["result"]["assessments"][1]["outcome"]=json!("reply");
            checkpoint["result"]["proposals"].as_array_mut().unwrap().push(json!({"itemId":"i1","kind":"reply_and_close","text":"Executable held sibling"}));
        }else{checkpoint["result"]["runMetadata"]["research"]["evidenceHolds"][0]["accountKey"]=json!("baw-russia");}
        checkpoint["attempts"][0]["resultDigest"]=json!(hash(&checkpoint["result"]));
        let complete=aggregate(row(&d,"jobs",&run).unwrap()).unwrap();
        let scoped=crate::engine_prepare::group_result(&complete,rows(&group,"itemIds"));
        let before=d.clone();assert!(prepare_bundle::admit_group(&mut d,&run,&scoped,&group).is_err(),"{variant}");assert_eq!(d,before,"{variant}");
    }
}

#[test]
fn grouped_composite_admission_retains_and_validates_off_group_image_proof(){
    let (base,run,complete)=r9_final_grouped_fixture_with_proof(false,false,Some(true),true);
    let group=row(&base,"jobs",&run).unwrap()["preparationStages"]["groupAdmission"][0].clone();
    let checkpoints=row(&base,"jobs",&run).unwrap()["preparationStages"]["reviewChunks"].clone();
    let image=complete["runMetadata"]["chunks"][0]["metadata"]["imageEvidence"][0].clone();
    assert_eq!(image["itemId"],"i1");assert_eq!(image["postId"],"other-post");
    let scoped=crate::engine_prepare::group_result(&complete,rows(&group,"itemIds"));
    let mut d=base.clone();let admitted=prepare_bundle::admit_group(&mut d,&run,&scoped,&group).unwrap();
    assert_eq!(admitted["status"],"review");assert_eq!(rows(&d,"proposals").len(),1);assert_eq!(d["proposals"][0]["itemId"],"i0");
    assert_eq!(d["proposals"][0]["generationMetadata"]["chunks"][0]["metadata"]["imageEvidence"],json!([image.clone()]));
    assert_eq!(d["proposals"][0]["editorialReview"]["source"]["runMetadata"]["chunks"][0]["metadata"]["imageEvidence"],json!([image]));
    assert_eq!(row(&d,"jobs",&run).unwrap()["preparationStages"]["reviewChunks"],checkpoints);
    for variant in ["post","attachment","recipient"] {
        let mut d=base.clone();let checkpoint=&mut row_mut(&mut d,"jobs",&run).unwrap()["preparationStages"]["reviewChunks"]["chunks"][0];
        let image=&mut checkpoint["result"]["runMetadata"]["imageEvidence"][0];
        match variant {
            "post"=>image["postId"]=json!("post"),
            "attachment"=>image["attachmentIndex"]=json!(1),
            _=>{image["itemId"]=json!("i0");image["itemIds"]=json!(["i0"]);},
        }
        checkpoint["attempts"][0]["resultDigest"]=json!(hash(&checkpoint["result"]));
        let complete=aggregate(row(&d,"jobs",&run).unwrap()).unwrap();
        let scoped=crate::engine_prepare::group_result(&complete,rows(&group,"itemIds"));
        let before=d.clone();assert!(prepare_bundle::admit_group(&mut d,&run,&scoped,&group).is_err(),"{variant}");assert_eq!(d,before,"{variant}");
    }
}

#[test]
fn grouped_explicit_resume_checks_all_owners_before_resetting_any_member(){
    let (base,run)=completed_recovery_fixture(2,true);
    let plan=row(&base,"jobs",&run).unwrap()["preparationStages"]["reviewChunks"]["planDigest"].as_str().unwrap().to_owned();
    for variant in ["last_owner","duplicate","missing","foreign"] {
        let mut d=base.clone();
        match variant {
            "last_owner"=>d["items"][1]["autoPreparation"]["jobId"]=json!("new-owner"),
            "duplicate"=>row_mut(&mut d,"jobs",&run).unwrap()["prepareBundle"]["itemIds"]=json!(["i0","i0"]),
            "missing"=>row_mut(&mut d,"jobs",&run).unwrap()["prepareBundle"]["itemIds"]=json!(["i0"]),
            _=>row_mut(&mut d,"jobs",&run).unwrap()["prepareBundle"]["itemIds"]=json!(["i0","foreign"]),
        }
        let before=d.clone();assert!(claim_resume(&mut d,&run,&plan).is_err(),"{variant}");assert_eq!(d,before);
    }
    let mut d=base;let before=row(&d,"jobs",&run).unwrap()["preparationStages"]["reviewChunks"].clone();
    claim_resume(&mut d,&run,&plan).unwrap();
    for item in rows(&d,"items"){assert_eq!(item["autoPreparation"]["status"],"running");assert_eq!(item["autoPreparation"]["reviewResumeRequired"],false);}
    assert_eq!(row(&d,"jobs",&run).unwrap()["preparationStages"]["reviewChunks"],before);
}

#[test]
fn grouped_auto_resume_keeps_already_admitted_branch_unchanged(){
    let (mut d,run)=fixture(2);
    d["items"][1]["branchId"]=json!("other-branch");
    d["branches"].as_array_mut().unwrap().push(json!({"id":"other-branch","postId":"post","messages":[],"contextComplete":true}));
    let ids=json!(["i0","i1"]);let bundle=legacy_bundle(&d,ids.as_array().unwrap());
    let groups=crate::prepare_bundle::capture_groups(&d,&bundle).unwrap();
    let job=row_mut(&mut d,"jobs",&run).unwrap();job["prepareBundle"]=bundle;
    job["preparationStages"]=json!({"first":null,"review":null,"groupAdmission":groups});
    job["purpose"]=json!("auto_prepare");
    let first=json!({"text":"First","sources":[],"assessments":[
        {"itemId":"i0","outcome":"reply","reason":"Routine","tags":["feedback"]},
        {"itemId":"i1","outcome":"needs_attention","reason":"Needs facts","tags":["needs_fact"]}],
        "proposals":[{"itemId":"i0","kind":"reply_and_close","text":"Спасибо!"}]});
    super::super::record_first(&mut d,&run,&first,AT).unwrap();
    initialize(&mut d,&run,&profile()).unwrap();
    let job=row_mut(&mut d,"jobs",&run).unwrap();
    job["preparationStages"]["groupAdmission"][0]["status"]=json!("admitted");
    job["preparationStages"]["groupAdmission"][0]["admission"]=json!({"status":"review","candidates":[{"itemId":"i0","status":"review","proposalId":"saved"}],"items":[{"itemId":"i0","status":"prepared"}]});
    job["status"]=json!("failed");
    d["items"][0]["autoPreparation"]=json!({"jobId":run,"status":"prepared","attempts":1});
    d["items"][1]["autoPreparation"]=json!({"jobId":run,"status":"error","reviewResumeRequired":true,"attempts":1});
    // An independently admitted group's UNKNOWN stays quarantined, but cannot
    // widen the pending review from i1 back to the already settled i0.
    d["operations"]=json!([{"id":"settled-group-unknown","itemId":"i0","status":"unknown","target":d["items"][0]}]);
    let protected=d["operations"].clone();
    let prior=d["items"][0]["autoPreparation"].clone();
    let plan=row(&d,"jobs",&run).unwrap()["preparationStages"]["reviewChunks"]["planDigest"].as_str().unwrap().to_owned();
    let mut pending_unknown=d.clone();let pending_target=pending_unknown["items"][1].clone();
    pending_unknown["operations"].as_array_mut().unwrap().push(
        json!({"id":"pending-group-unknown","itemId":"i1","status":"unknown","target":pending_target}));
    let blocked=pending_unknown.clone();assert!(claim_resume(&mut pending_unknown,&run,&plan).is_err());assert_eq!(pending_unknown,blocked);
    claim_resume(&mut d,&run,&plan).unwrap();
    assert_eq!(d["items"][0]["autoPreparation"],prior);
    assert_eq!(d["items"][1]["autoPreparation"]["status"],"running");
    assert_eq!(d["operations"],protected);
    assert!(crate::preparation_reservations::assert_available(&d,&["i0".into()],Some(&run)).is_err(),"UNKNOWN remains quarantined");
}

pub(crate) async fn exercise_completed_recovery_storage(db:&crate::Database,automatic:bool){
        let (initial,run)=completed_recovery_fixture(2,automatic);
        let plan=row(&initial,"jobs",&run).unwrap()["preparationStages"]["reviewChunks"]["planDigest"].as_str().unwrap().to_owned();
        let retained=row(&initial,"jobs",&run).unwrap()["preparationStages"]["reviewChunks"].clone();
        db.change(|d|{*d=initial;Ok(())}).await.unwrap();
        let before=db.read().await.unwrap();
        for variant in ["profile","plan","source","human_edit","operator_hold","unknown_operation","unknown_attempt","usage","forged_result","unsupported","first_only","foreign_account","ownership"] {
            if variant=="ownership"&&!automatic {continue;}
            let mut current_profile=profile();if variant=="profile" {current_profile.as_object_mut().unwrap().remove("profileSha256");current_profile["runtimeSha256"]=json!("0".repeat(64));current_profile["profileSha256"]=json!(hash(&current_profile));}
            let expected=if variant=="plan"{"0".repeat(64)}else{plan.clone()};
            let rejected=db.change(|d|{match variant {
                "source"=>d["posts"][0]["text"]=json!("Changed source"),
                "human_edit"=>{d["items"][1]["draft"]=json!("Operator text");d["items"][1]["draftEdited"]=json!(true);},
                "operator_hold"=>d["items"][1]["autoPreparation"]["humanOverrideAt"]=json!(AT),
                "unknown_operation"=>d["operations"]=json!([{"id":"unknown","itemId":"i1","status":"unknown"}]),
                "unknown_attempt"=>row_mut(d,"jobs",&run)?["preparationStages"]["reviewChunks"]["chunks"][0]["attempts"][0]["status"]=json!("unknown"),
                "usage"=>row_mut(d,"jobs",&run)?["preparationStages"]["reviewChunks"]["usage"]["chargedWebCalls"]=json!(0),
                "forged_result"=>row_mut(d,"jobs",&run)?["preparationStages"]["reviewChunks"]["chunks"][0]["result"]["proposals"][0]["text"]=json!("Changed result"),
                "unsupported"=>row_mut(d,"jobs",&run)?["purpose"]=json!("auto_revalidate"),
                "first_only"=>row_mut(d,"jobs",&run)?["preparationStages"]["first"]["reviewRequired"]=json!(false),
                "foreign_account"=>d["account"]=json!("BAW Russia"),
                "ownership"=>d["items"][1]["autoPreparation"]["jobId"]=json!("replacement"),
                _=>(),
            };let outcome=recover_in(d,&run,&expected,&current_profile,&crate::now());
                assert!(outcome.is_err(),"recovery domain must reject before storage validation: {automatic} {variant}");outcome
            }).await;
            assert!(rejected.is_err(),"{automatic} {variant}");
            assert_eq!(db.read().await.unwrap(),before,"full rollback: {automatic} {variant}");
        }
        // A failure after admission logic still rolls back new proposals/archive/ownership.
        let fault=db.change::<()>(|d|{
            recover_in(d,&run,&plan,&profile(),&crate::now())?;
            assert_eq!(rows(d,"proposals").len(),2,"the reducer must create new drafts before the injected fault");
            assert!(rows(d,"audit").iter().any(|a|a["action"]=="preparation.completed_result_recovered"&&a["refId"]==run));
            Err(conflict("Injected after admission before commit"))
        }).await.unwrap_err();
        assert_eq!(fault.1,"Injected after admission before commit","a preflight rejection is not rollback coverage");
        assert_eq!(db.read().await.unwrap(),before);
        let outcome=db.change(|d|recover_in(d,&run,&plan,&profile(),&crate::now())).await.unwrap();
        let saved=db.read().await.unwrap();let job=row(&saved,"jobs",&run).unwrap();
        assert_eq!(job["status"],"completed");assert_eq!(job["prepareOutcome"],outcome);
        assert_eq!(job["prepareBundle"],row(&before,"jobs",&run).unwrap()["prepareBundle"]);
        assert_eq!(job["preparationStages"]["first"],row(&before,"jobs",&run).unwrap()["preparationStages"]["first"]);
        assert_eq!(job["preparationStages"]["reviewChunks"],retained,"reservations and original results are immutable");
        assert!(rows(&before,"proposals").is_empty());
        assert_eq!(rows(&saved,"proposals").len(),2);assert!(rows(&saved,"approvals").is_empty());assert!(rows(&saved,"operations").is_empty());
        assert!(rows(&job["preparationStages"],"groupAdmission").iter().all(|group|group["status"]=="admitted"));
        for proposal in rows(&saved,"proposals"){
            assert_eq!(crate::proposal_current(&saved,proposal).unwrap_err().1,"legacy_material_contract_unmet");
        }
        assert_eq!(rows(&saved,"audit").iter().filter(|a|a["action"]=="preparation.completed_result_recovered"&&a["refId"]==run).count(),1);
        assert_eq!(rows(&saved,"preparationResearch").len(),1);
        assert_eq!(db.change(|d|recover_in(d,&run,&plan,&profile(),&crate::now())).await.unwrap(),outcome);
        assert_eq!(db.read().await.unwrap(),saved,"lost-response replay is byte-identical, without duplicate proposals or archive");
}

#[tokio::test]
async fn sqlite_completed_recovery_is_atomic_owned_and_replayable(){
    for automatic in [false,true] {
        let temp=tempfile::tempdir().unwrap();let path=temp.path().join("completed-recovery.sqlite");
        let db=crate::Database::Sqlite(crate::open_db(&path).await.unwrap());
        exercise_completed_recovery_storage(&db,automatic).await;let before=db.read().await.unwrap();db.close().await;
        let db=crate::Database::Sqlite(crate::open_db(&path).await.unwrap());assert_eq!(db.read().await.unwrap(),before);db.close().await;
    }
}

#[tokio::test]
async fn completed_recovery_endpoint_calls_only_local_profile_and_wakes_after_commit(){
    let (mut app,temp)=crate::tests::test_app().await;
    let (initial,run)=completed_recovery_fixture(2,false);
    let plan=row(&initial,"jobs",&run).unwrap()["preparationStages"]["reviewChunks"]["planDigest"].clone();
    let paid=row(&initial,"jobs",&run).unwrap().clone();assert!(rows(&initial,"proposals").is_empty());
    replace_chunk_fixture(&app,initial).await.unwrap();
    app.node=std::env::var_os("COMMUNITYHERO_TEST_NODE").map(std::path::PathBuf::from).unwrap_or_else(||std::path::PathBuf::from("node"));
    app.bridge=temp.path().join("profile-only.mjs");let log=temp.path().join("calls.jsonl");
    let script=r#"import fs from 'node:fs/promises';let input='';for await(const part of process.stdin)input+=part;
const r=JSON.parse(input);await fs.appendFile(__LOG__,JSON.stringify({operation:r.operation,purpose:r.purpose})+'\n');
if(r.operation!=='assistant'||r.purpose!=='review_profile')throw new Error('Model/provider calls forbidden');
process.stdout.write(JSON.stringify({ok:true,result:__PROFILE__}));"#
        .replace("__LOG__",&json!(log.to_string_lossy()).to_string()).replace("__PROFILE__",&profile().to_string());
    std::fs::write(&app.bridge,script).unwrap();
    let body=json!({"expectedPlanDigest":plan});
    let first=recover_completed(State(app.clone()),Path(run.clone()),Json(body.clone())).await.unwrap().0;
    tokio::time::timeout(std::time::Duration::from_millis(100),app.preparation_wake.notified()).await.unwrap();
    let committed=app.db.read().await.unwrap();
    let recovered=row(&committed,"jobs",&run).unwrap();
    assert_eq!(recovered["prepareBundle"],paid["prepareBundle"]);
    assert_eq!(recovered["preparationStages"]["first"],paid["preparationStages"]["first"]);
    assert_eq!(recovered["preparationStages"]["reviewChunks"],paid["preparationStages"]["reviewChunks"]);
    assert_eq!(rows(&committed,"proposals").len(),2,"local recovery admits a pending group, not an already committed replay");
    for proposal in rows(&committed,"proposals"){
        assert_eq!(crate::proposal_current(&committed,proposal).unwrap_err().1,"legacy_material_contract_unmet");
    }
    assert_eq!(rows(&committed,"audit").iter().filter(|a|a["action"]=="preparation.completed_result_recovered"&&a["refId"]==run).count(),1);
    assert!(rows(&committed,"approvals").is_empty()&&rows(&committed,"operations").is_empty());
    let replay=recover_completed(State(app.clone()),Path(run.clone()),Json(body)).await.unwrap().0;
    assert_eq!(first,replay);assert_eq!(app.db.read().await.unwrap(),committed);
    assert!(tokio::time::timeout(std::time::Duration::from_millis(20),app.preparation_wake.notified()).await.is_err(),"replay does not wake new work");
    let calls:Vec<Value>=std::fs::read_to_string(log).unwrap().lines().map(|line|serde_json::from_str(line).unwrap()).collect();
    assert_eq!(calls.len(),2);assert!(calls.iter().all(|call|call["purpose"]=="review_profile"));
    app.db.close().await;
}

#[test]
fn lifecycle_drain_between_paid_chunks_preserves_settled_output_and_blocks_new_reserve() {
    let (mut d,run)=fixture(20);
    let owner=crate::runtime_lifecycle::OwnerToken{account:"LikeAvto".into(),runtime_id:"review-fixture".into(),release_sha256:"a".repeat(64),epoch:1};
    let ledger=crate::runtime_lifecycle::ledger_digest(&d).unwrap();
    crate::runtime_lifecycle::initialize(&mut d,owner.clone(),&"b".repeat(64),&ledger).unwrap();
    initialize(&mut d,&run,&profile()).unwrap();
    let request=reserve_admitted(&mut d,&owner,&run,|_,_|Ok(()),AT).unwrap().unwrap();
    crate::runtime_lifecycle::begin_drain(&mut d,&owner,&"c".repeat(64),"review-drain",false).unwrap();
    save(&mut d,&run,&request,Ok(&result(&request,1)),AT).unwrap();
    let settled=d.clone();
    assert!(reserve_admitted(&mut d,&owner,&run,|_,_|Ok(()),AT).is_err());
    assert_eq!(d,settled,"drain cannot create a new attempt or rewrite paid output");
    assert_eq!(row(&d,"jobs",&run).unwrap()["preparationStages"]["reviewChunks"]["chunks"][0]["attempts"][0]["status"],"completed");
}
