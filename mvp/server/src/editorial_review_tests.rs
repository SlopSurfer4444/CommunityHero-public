use super::*;
const AT: &str = "2026-09-27T01:00:00Z";

#[test]
fn fresh_review_never_reuses_generation_or_exempts_current_close(){
    let mut d=fixture(2);let reply=add(&mut d,"i0","Exact final reply");accepted(&mut d,&reply);
    let item=crate::row(&d,"items","i1").unwrap().clone();
    let close=crate::create_proposal(&mut d,&json!({"itemId":"i1","expectedRevision":item["revision"],"kind":"close"})).unwrap();
    let refs=json!([reply,{"id":close["id"],"revision":close["revision"]}]);
    let prior=plan_new(&d,&refs,AT).unwrap();assert_eq!(prior["reused"],json!([refs[0]]));assert_eq!(prior["notRequired"],json!([refs[1]]));
    let fresh=plan_fresh(&d,&refs,AT).unwrap();assert_eq!(fresh["fresh"],true);assert_eq!(fresh["reused"],json!([]));assert_eq!(fresh["notRequired"],json!([]));
    let count:usize=rows(&fresh,"batches").iter().map(|b|rows(&b["request"],"editorialCandidates").len()).sum();assert_eq!(count,2);
    assert!(prior.get("fresh").is_none(),"legacy plan bytes keep absent selector");
}

fn fixture(count: usize) -> Value {
    let mut d=crate::empty();crate::accounts::initialize(&mut d,crate::accounts::Profile::LikeAvto).unwrap();
    d["feedback"]=json!([]);
    d["knowledge_entries"]=json!([]);d["knowledge_versions"]=json!([]);
    d["items"]=json!((0..count).map(|n|json!({"id":format!("i{n}"),"itemId":format!("c{n}"),"objectId":"11341","platform":"VK",
        "postKey":"post","conversationKey":format!("thread{n}"),"branchId":"branch","postId":"post","revision":1,"draft":"",
        "workflow":"attention","providerStatus":"new","contextEvidenceDigest":format!("context-{n}"),"branchContextDigest":"branch-digest",
        "text":"Мечтаю о V8","connectorBinding":d["connectorBinding"]})).collect::<Vec<_>>());
    d["posts"]=json!([{"id":"post","postKey":"post","objectId":"11341","platform":"VK","text":"Post","attachments":[]}]);
    d["branches"]=json!([{"id":"branch","postId":"post","messages":[],"contextComplete":true}]);
    d
}
fn add(d:&mut Value,item:&str,body:&str)->Value {
    let p=crate::create_proposal(d,&json!({"itemId":item,"expectedRevision":1,"kind":"reply_and_close","text":body})).unwrap();
    json!({"id":p["id"],"revision":p["revision"]})
}
fn response(batch:&Value)->Value {
    let mut metadata=fixture_metadata();
    if batch["request"]["editorialModelProfile"]==MODEL_PROFILE {
        metadata["model"]=json!(crate::codex_model_policy::MODEL);
        metadata["modelProfile"]=json!(crate::codex_model_policy::PROFILE);
        metadata["reasoningEffort"]=json!("high");
        metadata["cliSha256"]=json!("86e8ef1013f98df51fdeea446597f7e3ca32e454d1d4d8c0402a68b03c311d70");
    }
    json!({"text":"Reviewed exact final replies","sources":[],"proposals":[],"editorial":rows(&batch["request"],"editorialCandidates").iter().map(|c|json!({
        "proposalId":c["proposalId"],"proposalRevision":c["proposalRevision"],"itemId":c["itemId"],"textSha256":c["textSha256"],
        "contextDigest":c["contextDigest"],"rulesDigest":c["rulesDigest"],"decision":"accept","reason":"Appropriate grounded reply under supplied company rules",
        "proposedText":null,"checks":{"companyRules":"pass","intent":"pass","factualScope":"pass"}})).collect::<Vec<_>>(),"runMetadata":metadata})
}
fn accepted(d:&mut Value,reference:&Value) {
    fixture_accept(d,reference["id"].as_str().unwrap()).unwrap();
}

#[test]
fn sol_profile_is_new_capture_only_and_metadata_cannot_change_model_or_effort(){
    let mut d=fixture(1);let reference=add(&mut d,"i0","Exact final reply");
    let legacy=plan(&d,&json!([reference]),AT).unwrap();
    let next=plan_new(&d,&json!([reference]),AT).unwrap();
    let old_batch=&legacy["batches"][0];let new_batch=&next["batches"][0];
    assert!(old_batch["request"].get("editorialModelProfile").is_none());
    assert!(!independent_lane(&old_batch["request"]).unwrap());
    assert_eq!(new_batch["request"]["editorialModelProfile"],MODEL_PROFILE);
    assert!(independent_lane(&new_batch["request"]).unwrap());
    assert_ne!(new_batch["digest"],old_batch["digest"]);
    assert_eq!(new_batch["digest"],hash(&new_batch["request"]));
    assert!(old_batch["request"].get("visualNeedContract").is_none());
    assert!(old_batch["request"].get("visualSelection").is_none());
    assert_eq!(new_batch["request"]["visualNeedContract"],crate::prepare_bundle::visual::CONTRACT);
    assert_eq!(new_batch["request"]["visualSelection"],crate::prepare_bundle::visual::empty());
    assert_eq!(new_batch["request"]["manualFrameRequestIds"],json!([]));
    assert!(old_batch["request"].get("manualFrameRequestIds").is_none());
    let mut equivalent=new_batch["request"].clone();
    for field in ["editorialModelProfile","visualNeedContract","visualSelection","mandatoryMaterialContract","postContextBundle","materialReadiness","strictGroupContract","strictGroup","manualFrameRequestIds"] {
        equivalent.as_object_mut().unwrap().remove(field);
    }
    assert_eq!(equivalent,old_batch["request"],"profile capture must not rewrite recipient evidence");
    let mut result=response(new_batch);result["runMetadata"]["model"]=json!("gpt-6.1-sol");result["runMetadata"]["reasoningEffort"]=json!("high");
    fixture_capture_result(&mut d,new_batch,&mut result).unwrap();
    for (field,value) in [("model",json!("gpt-6-luna")),("model",json!("gpt-6-astra")),("reasoningEffort",json!("low")),("promptVersion",json!("other"))]{
        let before=d.clone();let mut invalid=result.clone();invalid["runMetadata"][field]=value;
        assert!(admit(&mut d,new_batch,&invalid,AT).is_err());assert_eq!(d,before,"reject before any receipt");
    }
    let mut forged=new_batch.clone();forged["request"]["editorialModelProfile"]=json!("luna_low_v1");forged["digest"]=json!(hash(&forged["request"]));
    assert!(admit(&mut d,&forged,&result,AT).is_err());
    for value in [Value::Null,json!("astra_high_v1"),json!("luna_low_v1")] {
        let mut invalid=new_batch["request"].clone();invalid["editorialModelProfile"]=value;assert!(independent_lane(&invalid).is_err());
    }
    let mut wrong_purpose=new_batch["request"].clone();wrong_purpose["purpose"]=json!("triage");assert!(independent_lane(&wrong_purpose).is_err());
    admit(&mut d,new_batch,&result,AT).unwrap();
    assert_eq!(d["proposals"][0]["editorialReview"]["source"]["runMetadata"]["model"],"gpt-6.1-sol");
    assert!(require_current(&EvidenceContext::new(&d),&d["proposals"][0]).is_ok());
    let mut historical=fixture(1);let prior=add(&mut historical,"i0","Exact final reply");
    let old=plan(&historical,&json!([prior]),AT).unwrap();let before=old.clone();
    admit(&mut historical,&old["batches"][0],&response(&old["batches"][0]),AT).unwrap();assert_eq!(old,before);
}

#[test]
fn final_text_from_any_origin_requires_semantic_receipt_and_current_rule_bindings() {
    let mut d=fixture(1);let r=add(&mut d,"i0","Тоже интересный вариант 🙂");
    let id=r["id"].as_str().unwrap();
    assert!(require_current(&EvidenceContext::new(&d),proposal(&d,id).unwrap()).is_err());
    accepted(&mut d,&r);
    assert!(require_current(&EvidenceContext::new(&d),proposal(&d,id).unwrap()).is_ok());
    for change in ["text","revision","branch","post","company","rule"] {
        let mut changed=d.clone();
        match change {
            "text"=>changed["proposals"][0]["text"]=json!("Changed text"),
            "revision"=>changed["proposals"][0]["revision"]=json!(2),
            "branch"=>changed["branches"][0]["messages"]=json!([{"id":"later","text":"New clarification"}]),
            "post"=>changed["posts"][0]["text"]=json!("Changed post"),
            "company"=>{changed["account"]=json!("BAW Russia");changed["connectorBinding"]=crate::accounts::Profile::BawRussia.binding();},
            _=>{crate::knowledge::save_instruction(&mut changed,&json!({"requestId":"new-rule","title":"Voice","text":"Use the company voice"}),AT).unwrap();}
        }
        assert!(require_current(&EvidenceContext::new(&changed),&changed["proposals"][0]).is_err(),"{change}");
    }
}

#[test]
fn marked_dispatch_requires_intact_editorial_receipt_while_legacy_dispatch_preserves_guards() {
    let mut d=fixture(1);let reference=add(&mut d,"i0","Тоже хочется V8 🙂");accepted(&mut d,&reference);
    let p=&d["proposals"][0];let target=crate::proposal_current(&d,p).unwrap();
    let op=json!({"proposalId":p["id"],"target":target,"editorialPolicyVersion":1});
    assert!(crate::dispatch_diagnostics::local_check(&d,&op).is_ok());
    d["proposals"][0]["editorialReview"]["reason"]=json!("Unversioned receipt tampering");
    let failure=crate::dispatch_diagnostics::local_check(&d,&op).err().unwrap();
    assert_eq!(failure.outcome,crate::dispatch_diagnostics::Outcome::Stale);
    assert_eq!(failure.evidence["code"],"editorial_review_changed");
    assert_eq!(failure.evidence["providerCallAttempted"],false);
    let mut legacy=op.clone();legacy.as_object_mut().unwrap().remove("editorialPolicyVersion");
    assert!(crate::dispatch_diagnostics::local_check(&d,&legacy).is_ok(),"this fixture now has genuine current mandatory model delivery; legacy semantic policy does not erase its saved observation");
    d["items"][0]["revision"]=json!(99);
    assert_eq!(crate::dispatch_diagnostics::local_check(&d,&legacy).err().unwrap().evidence["code"],"local_target_revision_changed");
}

#[test]
fn model_revision_and_hold_persist_evidence_without_mutating_draft_approval_or_unknown() {
    for decision in ["revise","hold"] {
        let mut d=fixture(1);let r=add(&mut d,"i0","Жаль что завод никогда этого не сделает");
        let before=d["proposals"][0].clone();let plan=plan(&d,&json!([r]),AT).unwrap();let b=&plan["batches"][0];let mut out=response(b);
        out["editorial"][0]["decision"]=json!(decision);out["editorial"][0]["checks"]["intent"]=json!("fail");
        out["editorial"][0]["reason"]=json!("A wish is being answered as an objection without evidence");
        if decision=="revise" {out["editorial"][0]["proposedText"]=json!("V8 добавил бы характера 🙂");}
        admit(&mut d,b,&out,AT).unwrap();
        for key in ["text","kind","revision","status"] {assert_eq!(d["proposals"][0][key],before[key]);}
        assert_eq!(d["proposals"][0]["editorialReview"]["decision"],decision);
        assert!(require_current(&EvidenceContext::new(&d),&d["proposals"][0]).is_err());
        assert!(rows(&d,"operations").is_empty());assert!(rows(&d,"approvals").is_empty());
    }
}

#[test]
fn malformed_batch_is_atomic_and_stale_recipient_does_not_discard_other_review() {
    let mut d=fixture(2);let a=add(&mut d,"i0","Reply A");let b=add(&mut d,"i1","Reply B");
    let p=plan(&d,&json!([a,b]),AT).unwrap();let batch=&p["batches"][0];let response=response(batch);
    for field in ["proposalRevision","itemId","textSha256","rulesDigest","contextDigest"] {
        let mut bad=response.clone();bad["editorial"][0][field]=json!("forged");let before=d.clone();
        assert!(admit(&mut d,batch,&bad,AT).is_err());assert_eq!(d,before);
    }
    let mut duplicate=response.clone();duplicate["editorial"][1]=duplicate["editorial"][0].clone();
    assert!(admit(&mut d,batch,&duplicate,AT).is_err());
    d["proposals"][0]["text"]=json!("Later edit");
    let result=admit(&mut d,batch,&response,AT).unwrap();
    assert_eq!(result["outcomes"][0]["stale"],true);
    assert!(d["proposals"][0].get("editorialReview").is_none());
    assert_eq!(d["proposals"][1]["editorialReview"]["decision"],"accept");
}

#[test]
fn protected_operations_are_not_reviewed_or_touched_and_valid_receipts_reuse_without_call() {
    let mut d=fixture(1);let r=add(&mut d,"i0","Reply");accepted(&mut d,&r);
    let reused=plan(&d,&json!([r]),AT).unwrap();assert_eq!(reused["reused"],json!([r]));assert!(rows(&reused,"batches").is_empty());
    for status in ["unknown","dispatching","succeeded"] {
        let mut protected=d.clone();protected["operations"]=json!([{"id":"existing","itemId":"i0","status":status,"action":{"reply":"EXACT ADMITTED TEXT"}}]);
        let before=protected.clone();let p=plan(&protected,&json!([r]),AT).unwrap();
        assert_eq!(rows(&p,"held").len(),1);assert!(rows(&p,"batches").is_empty());assert_eq!(protected,before);
    }
}

#[test]
fn batch_deduplicates_shared_post_images_and_splits_independent_capacity() {
    let mut d=fixture(3);
    d["posts"][0]["attachments"]=json!((0..8).map(|n|json!({"type":"photo","url":format!("https://example.com/shared-{n}.jpg")})).collect::<Vec<_>>());
    let refs=json!([add(&mut d,"i0","A"),add(&mut d,"i1","B"),add(&mut d,"i2","C")]);
    let p=plan(&d,&refs,AT).unwrap();assert_eq!(rows(&p,"batches").len(),1);
    let request=&p["batches"][0]["request"];assert_eq!(rows(request,"posts").len(),1);assert_eq!(crate::engine_prepare::image_count(request),8);
    for n in 0..3 {d["items"][n]["attachments"]=json!((0..8).map(|i|json!({"type":"photo","url":format!("https://example.com/item-{n}-{i}.jpg")})).collect::<Vec<_>>());}
    let split=plan(&d,&refs,AT).unwrap();assert_eq!(rows(&split,"batches").len(),3);assert!(rows(&split,"held").is_empty());
    for b in rows(&split,"batches") {assert_eq!(crate::engine_prepare::image_count(&b["request"]),16);}
}

#[test]
fn strict_final_editorial_capture_observes_all_current_photos_and_preserves_recipient_scope(){
    let mut d=fixture(2);
    d["posts"][0]["attachments"]=json!((0..8).map(|n|json!({"type":"photo","url":format!("https://example.com/{n}.jpg")})).collect::<Vec<_>>());
    let a=add(&mut d,"i0","The exact detail is visible in the post");let b=add(&mut d,"i1","Thanks for the feedback");
    let refs=json!([a,b]);
    let old=plan(&d,&refs,AT).unwrap();assert_eq!(crate::engine_prepare::image_count(&old["batches"][0]["request"]),8);
    assert!(rows(&plan_new(&d,&refs,AT).unwrap(),"batches").is_empty(),"missing photo bytes hold before a paid editorial call");
    crate::photo_acquisition::fixture_commit_photo(&mut d,"post",AT).unwrap();
    let fresh=plan_new(&d,&refs,AT).unwrap();let request=&fresh["batches"][0]["request"];
    assert_eq!(request["visualNeedContract"],crate::prepare_bundle::visual::CONTRACT);
    assert_eq!(crate::engine_prepare::image_count(request),8);
    assert_eq!(rows(&request["visualSelection"],"postImages").len(),2,"Both exact decisions must receive current photos");
    assert!(rows(&request["visualSelection"],"postImages").iter().all(|r|rows(r,"attachmentIndices").len()==8));
    assert_eq!(request["posts"][0]["attachments"],d["posts"][0]["attachments"],"The observed sources remain exact");
    let selection=json!({"version":1,"postImages":[{"itemId":"i0","postId":"post","attachmentIndices":[2],"reason":"Identify the exact visible detail"}]});
    d["proposals"][0]["generationMetadata"]=json!({"visualNeedContract":crate::prepare_bundle::visual::CONTRACT,
        "visualSelection":crate::prepare_bundle::visual::empty(),"visualFollowup":{"status":"completed","selection":selection}});
    let selected=plan_new(&d,&refs,AT).unwrap();let request=&selected["batches"][0]["request"];
    assert_eq!(crate::engine_prepare::image_count(request),8,"Earlier selective generation cannot waive current strict photo coverage");
    assert!(rows(&request["visualSelection"],"postImages").iter().all(|r|rows(r,"attachmentIndices").len()==8));
    let captured_selection=request["visualSelection"].clone();
    let before=selected["batches"][0].clone();d["proposals"][0]["text"]=json!("Changed before dispatch");
    let dispatch=before_call(&d,&before).unwrap();assert_eq!(dispatch["held"].as_array().unwrap().len(),1);
    assert_eq!(rows(&dispatch["batch"]["request"]["visualSelection"],"postImages").len(),1);
    assert_eq!(dispatch["batch"]["request"]["visualSelection"]["postImages"][0]["itemId"],"i1");
    assert_eq!(crate::engine_prepare::image_count(&dispatch["batch"]["request"]),8);
    validate_dispatch_capture(&before,&dispatch).unwrap();
    assert_eq!(before["request"]["visualSelection"],captured_selection,"captured scope is immutable");
}

#[test]
fn oversized_single_source_is_explicit_hold_and_remaining_source_progresses() {
    let mut d=fixture(2);let refs=json!([add(&mut d,"i0","A"),add(&mut d,"i1","B")]);
    d["items"][0]["attachments"]=json!((0..17).map(|i|json!({"type":"photo","url":format!("https://example.com/{i}.jpg")})).collect::<Vec<_>>());
    let p=plan(&d,&refs,AT).unwrap();assert_eq!(rows(&p,"held").len(),1);assert_eq!(rows(&p,"batches").len(),1);
    assert_eq!(p["batches"][0]["request"]["editorialCandidates"][0]["itemId"],"i1");
}

#[test]
fn prior_generation_review_is_reused_for_exact_text_after_workflow_bump_but_not_edits() {
    let mut d=fixture(1);
    let bundle=crate::engine_prepare::build_request(&d,&[json!("i0")],None).unwrap();
    let r=add(&mut d,"i0","Тоже хочется V8 🙂");let id=r["id"].as_str().unwrap();
    d["proposals"][0]["prepareBundleId"]=bundle["id"].clone();d["proposals"][0]["prepareBundleDigest"]=bundle["digest"].clone();
    let result=json!({"proposals":[{"itemId":"i0","kind":"reply_and_close","text":"Тоже хочется V8 🙂"}],
        "editorialEvidence":{"version":1,"contract":CONTRACT,"entries":[{"itemId":"i0","kind":"reply_and_close","textSha256":hash_text("Тоже хочется V8 🙂"),
        "decision":"accept","reason":"Supports wish without invented promise","checks":{"companyRules":"pass","intent":"pass","factualScope":"pass"}}]},"runMetadata":fixture_metadata()});
    assert!(reuse_generation(&mut d,id,&bundle,&result,AT).unwrap());
    assert!(require_current(&EvidenceContext::new(&d),&d["proposals"][0]).is_ok());
    let mut edited=d.clone();edited["proposals"][0]["text"]=json!("External revised reply");
    assert!(!reuse_generation(&mut edited,id,&bundle,&result,AT).unwrap());
    assert!(require_current(&EvidenceContext::new(&edited),&edited["proposals"][0]).is_err());
    let legacy=json!({"proposals":result["proposals"]});assert!(!reuse_generation(&mut d,id,&bundle,&legacy,AT).unwrap());
}

#[test]
fn malformed_generation_proof_never_becomes_editorial_acceptance() {
    let result=json!({"proposals":[{"itemId":"i","kind":"reply_and_close","text":"Reply"}],"editorialEvidence":{"version":1,"contract":CONTRACT,
        "entries":[{"itemId":"i","kind":"reply_and_close","textSha256":hash_text("OTHER TEXT"),"decision":"accept","reason":"Review",
        "checks":{"companyRules":"pass","intent":"pass","factualScope":"pass"}}]}});
    assert!(generation_evidence(&result).is_err());
}

fn astra_generation_review_fixture(reviewed:usize)->(Value,Value) {
    let mut d=fixture(2);
    let bundle=crate::engine_prepare::build_request(&d,&[json!("i0"),json!("i1")],None).unwrap();
    let refs=json!([add(&mut d,"i0","Exact reply A"),add(&mut d,"i1","Exact reply B")]);
    let mut metadata=fixture_metadata();
    metadata["model"]=json!("gpt-6-astra");metadata["reasoningEffort"]=json!("high");
    metadata["promptVersion"]=json!("communityhero-preparation-v1-single-pass");
    let mut result=json!({"proposals":rows(&d,"proposals").iter().map(|p|json!({"itemId":p["itemId"],"kind":p["kind"],"text":p["text"]})).collect::<Vec<_>>(),
        "editorialEvidence":{"version":1,"contract":CONTRACT,"entries":rows(&d,"proposals").iter().take(reviewed).map(|p|json!({
            "itemId":p["itemId"],"kind":p["kind"],"textSha256":hash_text(p["text"].as_str().unwrap()),"decision":"accept",
            "reason":"Exact grounded generation review","checks":{"companyRules":"pass","intent":"pass","factualScope":"pass"}})).collect::<Vec<_>>()},"runMetadata":metadata});
    d["jobs"]=json!([{"id":"isolated-astra-generation","kind":"assistant","purpose":"engine_prepare","status":"completed","prepareBundle":bundle}]);
    crate::model_material_receipt::fixture_result(&mut d,"isolated-astra-generation",&bundle["request"],&mut result).unwrap();
    let mut ids=Vec::new();
    for p in d["proposals"].as_array_mut().unwrap().iter_mut().take(reviewed) {
        p["prepareBundleId"]=bundle["id"].clone();p["prepareBundleDigest"]=bundle["digest"].clone();
        p["modelMaterialReceipt"]=result["modelMaterialReceipt"].clone();
        ids.push(p["id"].as_str().unwrap().to_owned());
    }
    assert_eq!(reuse_generation_batch(&mut d,&ids,&bundle,&result,AT).unwrap(),ids);
    for p in rows(&d,"proposals").iter().take(reviewed) {
        assert!(require_current(&EvidenceContext::new(&d),p).is_ok());
        assert_eq!(p["editorialReview"]["source"]["kind"],"reused_generation_review");
        assert_eq!(p["editorialReview"]["source"]["runMetadata"]["model"],"gpt-6-astra");
        assert_eq!(p["editorialReview"]["source"]["runMetadata"]["reasoningEffort"],"high");
    }
    (d,refs)
}

#[test]
fn new_sol_plan_reuses_all_exact_astra_high_generation_reviews_without_model_batch() {
    let (d,refs)=astra_generation_review_fixture(2);let before=d.clone();
    let planned=plan_new(&d,&refs,AT).unwrap();
    assert_eq!(planned["reused"],refs);
    assert!(rows(&planned,"batches").is_empty());assert!(rows(&planned,"held").is_empty());
    assert_eq!(d,before,"new model routing must not rewrite paid generation receipts");
}

#[test]
fn new_sol_plan_batches_only_missing_review_beside_exact_astra_high_generation_review() {
    let (d,refs)=astra_generation_review_fixture(1);let before=d.clone();
    let planned=plan_new(&d,&refs,AT).unwrap();
    assert_eq!(planned["reused"],json!([refs[0]]));assert!(rows(&planned,"held").is_empty());
    assert_eq!(rows(&planned,"batches").len(),1);
    let batch=&planned["batches"][0];assert_eq!(batch["request"]["editorialModelProfile"],MODEL_PROFILE);
    assert_eq!(rows(&batch["request"],"editorialCandidates").len(),1);
    assert_eq!(batch["request"]["editorialCandidates"][0]["proposalId"],refs[1]["id"]);
    assert_eq!(batch["request"]["editorialCandidates"][0]["itemId"],"i1");
    assert_eq!(batch["digest"],hash(&batch["request"]));assert_eq!(d,before);
}

#[test]
fn editorial_before_call_preserves_current_capture_and_holds_unknown_without_ambiguous_pins(){
    let mut d=fixture(2);let refs=json!([add(&mut d,"i0","Reply A"),add(&mut d,"i1","Reply B")]);
    let plan=plan_new(&d,&refs,AT).unwrap();let batch=&plan["batches"][0];
    let clean=before_call(&d,batch).unwrap();assert_eq!(clean["batch"],*batch);assert!(rows(&clean,"held").is_empty());
    d["operations"]=json!([{"id":"uncertain","itemId":"i0","status":"unknown","action":{"reply":"Original exact text"}}]);
    let before=d.clone();let filtered=before_call(&d,batch).unwrap();assert_eq!(d,before);
    assert_eq!(filtered["held"][0]["proposalId"],refs[0]["id"]);
    assert_eq!(rows(&filtered["batch"]["request"],"editorialCandidates").len(),1);
    assert_eq!(filtered["batch"]["request"]["editorialCandidates"][0]["proposalId"],refs[1]["id"]);
    for key in ["items","branches","posts","materials","knowledgeManifest","customerCases"]{
        assert_eq!(filtered["batch"]["request"][key],batch["request"][key],"captured evidence stays exact: {key}");
    }
    let mut ambiguous=batch.clone();ambiguous["request"]["editorialResearchPins"][1]=ambiguous["request"]["editorialResearchPins"][0].clone();
    ambiguous["digest"]=json!(hash(&ambiguous["request"]));assert!(before_call(&d,&ambiguous).is_err());
}

#[test]
fn review_provenance_and_exact_image_attribution_cannot_be_omitted_or_forged() {
    let mut d=fixture(1);let r=add(&mut d,"i0","Reply");let plan=plan(&d,&json!([r]),AT).unwrap();let b=&plan["batches"][0];
    let mut missing=response(b);missing.as_object_mut().unwrap().remove("runMetadata");let before=d.clone();
    assert!(admit(&mut d,b,&missing,AT).is_err());assert_eq!(d,before);
    let mut foreign=response(b);foreign["runMetadata"]["imageEvidence"]=json!([{"imageNumber":1,"itemId":"foreign","attachmentIndex":0,
        "origin":"comment_attachment","sha256":"a".repeat(64),"mime":"image/png","width":1,"height":1}]);
    assert!(admit(&mut d,b,&foreign,AT).is_err());assert_eq!(d,before);
}

#[test]
fn fresh_explicit_closed_followup_can_be_reviewed_without_replaying_completed_operation() {
    let mut d=fixture(1);let old=d["items"][0].clone();
    d["operations"]=json!([{"id":"old-op","itemId":"i0","status":"succeeded","action":{"action":"close"},"target":old}]);
    d["items"][0]["workflow"]=json!("closed");d["items"][0]["providerStatus"]=json!("closed");d["items"][0]["revision"]=json!(3);
    let p=crate::create_proposal(&mut d,&json!({"itemId":"i0","expectedRevision":3,"kind":"reply_and_close","allowClosedReply":true,"text":"Fresh explicit reply"})).unwrap();
    let refs=json!([{"id":p["id"],"revision":p["revision"]}]);
    let before=d["operations"].clone();let allowed=plan(&d,&refs,AT).unwrap();assert!(rows(&allowed,"held").is_empty());assert_eq!(rows(&allowed,"batches").len(),1);
    for changed in ["unchanged_revision","foreign_route","unknown","dispatching"] {
        let mut bad=d.clone();match changed {
            "unchanged_revision"=>bad["items"][0]["revision"]=json!(1),
            "foreign_route"=>bad["operations"][0]["target"]["connectorBinding"]=crate::accounts::Profile::BawRussia.binding(),
            status=>bad["operations"][0]["status"]=json!(status)
        }
        let held=plan(&bad,&refs,AT).unwrap();assert_eq!(rows(&held,"held").len(),1,"{changed}");assert!(rows(&held,"batches").is_empty());
    }
    assert_eq!(d["operations"],before);
}

#[test]
fn edited_draft_receives_qualified_cached_research_and_frozen_pins_reject_source_changes() {
    let mut d=fixture(1);let now=crate::now();
    let mut archive=json!({"id":"research:test","jobId":"test","account":d["account"],"connectorBinding":d["connectorBinding"],
        "createdAt":now,"trust":"source_only","activePolicy":false,"posts":[d["posts"][0]],"bindings":[{"itemId":"original","postKey":"post"}],
        "review":{"status":"completed","research":{"version":1,"status":"completed","trust":"source_only","webCalls":1,"completedAt":now,
            "sources":[{"itemId":"original","url":"https://manufacturer.example/specs","title":"Table","claim":"Base version statement","trust":"source_only",
            "claimKind":"source_statement","scope":{"model":"Q06","trim":"Base","market":"China"},"sourceScope":{"model":"Q06","trim":"Base","market":"China"}}]}}});
    archive["checksum"]=json!(crate::research_cache::checksum(&archive));d["preparationResearch"]=json!([archive]);
    let r=add(&mut d,"i0","Edited reply about the Base version");let p=plan(&d,&json!([r]),AT).unwrap();let b=&p["batches"][0];
    let material=rows(&b["request"],"materials").iter().find(|m|m["kind"]=="research").unwrap();
    assert_eq!(material["scope"]["trim"],"Base");assert_eq!(material["sourceScope"]["market"],"China");assert_eq!(material["trust"],"source_only");
    assert_eq!(rows(&b["request"]["editorialResearchPins"][0],"manifest").len(),1);
    admit(&mut d,b,&response(b),AT).unwrap();assert!(require_current(&EvidenceContext::new(&d),&d["proposals"][0]).is_ok());
    // New unrelated research is ignored by the historical pinned receipt.
    let mut extra=d["preparationResearch"][0].clone();extra["id"]=json!("research:extra");extra["jobId"]=json!("extra");
    extra["review"]["research"]["sources"][0]["claim"]=json!("Additional claim");extra["checksum"]=json!(crate::research_cache::checksum(&extra));
    d["preparationResearch"].as_array_mut().unwrap().push(extra);assert!(require_current(&EvidenceContext::new(&d),&d["proposals"][0]).is_ok());
    d["preparationResearch"][0]["review"]["research"]["sources"][0]["scope"]["trim"]=json!("Different trim");
    d["preparationResearch"][0]["checksum"]=json!(crate::research_cache::checksum(&d["preparationResearch"][0]));
    assert!(require_current(&EvidenceContext::new(&d),&d["proposals"][0]).is_err());
}

#[test]
fn dedicated_review_refreshes_changed_post_or_rules_without_rewriting_generation_digest() {
    for change in ["post","rule"] {
        let mut d=fixture(1);let r=add(&mut d,"i0","Тоже интересный вариант 🙂");
        let old=d["proposals"][0]["reviewContextDigest"].clone();
        match change {
            "post"=>d["posts"][0]["text"]=json!("Updated post context"),
            _=>{crate::knowledge::save_instruction(&mut d,&json!({"requestId":"rule-v2",
                "title":"Current voice","text":"Use the current company voice"}),AT).unwrap();}
        }
        assert_ne!(crate::prepare_bundle::review_fingerprint(&d,"i0").unwrap(),old);
        assert!(crate::proposal_current(&d,&d["proposals"][0]).is_err());
        let planned=plan_new(&d,&json!([r.clone()]),AT).unwrap();
        assert_eq!(rows(&planned,"batches").len(),1,"{change}");
        let batch=&planned["batches"][0];
        let mut result=response(batch);fixture_capture_result(&mut d,batch,&mut result).unwrap();
        assert_eq!(admit(&mut d,batch,&result,AT).unwrap()["outcomes"][0]["decision"],"accept");
        assert_eq!(d["proposals"][0]["reviewContextDigest"],old,"original generation remains historical");
        assert!(crate::proposal_current(&d,&d["proposals"][0]).is_ok(),"{change}");
        let approval=crate::create_approval(&mut d,&crate::operator_auth::Actor::local_owner("synthetic"),
            &json!({"proposals":[r]})).unwrap();
        let op=json!({"id":"synthetic-op","approvalId":approval["id"],"proposalId":d["proposals"][0]["id"],
            "itemId":"i0","target":d["items"][0],"editorialPolicyVersion":1,
            "approvedEditorialReceiptSha256":d["proposals"][0]["editorialReview"]["receiptSha256"]});
        assert!(crate::dispatch_diagnostics::local_check(&d,&op).is_ok(),"{change}");
        d["posts"][0]["text"]=json!("Changed again after the exact review");
        assert!(crate::dispatch_diagnostics::local_check(&d,&op).is_err());
    }
}

#[test]
fn stale_close_can_receive_exact_review_but_operator_hold_and_unknown_remain_blocked() {
    let mut d=fixture(1);
    let p=crate::create_proposal(&mut d,&json!({"itemId":"i0","expectedRevision":1,"kind":"close"})).unwrap();
    let r=json!({"id":p["id"],"revision":p["revision"]});
    assert_eq!(plan(&d,&json!([r.clone()]),AT).unwrap()["notRequired"],json!([r]));
    d["posts"][0]["text"]=json!("Updated post before close review");
    let planned=plan(&d,&json!([r.clone()]),AT).unwrap();
    assert_eq!(rows(&planned,"batches").len(),1);
    let batch=&planned["batches"][0];admit(&mut d,batch,&response(batch),AT).unwrap();
    assert!(crate::proposal_current(&d,&d["proposals"][0]).is_ok());
    let actor=crate::operator_auth::Actor::local_owner("synthetic");
    let approval=crate::create_approval(&mut d,&actor,&json!({"proposals":[r.clone()]})).unwrap();
    assert_eq!(approval["status"],"approved");
    let mut held=d.clone();held["items"][0]["workflow"]=json!("waiting");
    assert!(crate::proposal_current(&held,&held["proposals"][0]).is_err());
    assert_eq!(plan(&held,&json!([r.clone()]),AT).unwrap()["held"][0]["reason"],"Editorial recipient has an operator hold");
    let mut unknown=d.clone();unknown["operations"]=json!([{"id":"prior","itemId":"i0","status":"unknown"}]);
    let result=crate::approval_admission::create(&mut unknown,&actor,&json!({"requestId":"unknown-block",
        "admissionMode":"partial","proposals":[r]})).unwrap();
    assert_eq!(result["held"][0]["reason"],"recipient_operation_blocked");
}

#[test]
fn refreshed_source_requires_intact_receipt_and_unchanged_route_and_approval_snapshot() {
    let mut d=fixture(1);let r=add(&mut d,"i0","Exact reply");accepted(&mut d,&r);
    let original_approval=crate::create_approval(&mut d,&crate::operator_auth::Actor::local_owner("synthetic"),
        &json!({"proposals":[r.clone()]})).unwrap();
    let old_receipt=d["proposals"][0]["editorialReview"]["receiptSha256"].clone();
    d["posts"][0]["text"]=json!("New source after old approval");
    let planned=plan_new(&d,&json!([r.clone()]),AT).unwrap();let batch=&planned["batches"][0];
    let mut result=response(batch);fixture_capture_result(&mut d,batch,&mut result).unwrap();admit(&mut d,batch,&result,AT).unwrap();
    assert_ne!(d["proposals"][0]["editorialReview"]["receiptSha256"],old_receipt);
    let old_op=json!({"id":"old-op","approvalId":original_approval["id"],"proposalId":d["proposals"][0]["id"],
        "itemId":"i0","target":d["items"][0],"editorialPolicyVersion":1});
    assert_eq!(crate::dispatch_diagnostics::local_check(&d,&old_op).err().unwrap().evidence["diagnostic"]["localPredicate"],
        "review_source_changed");
    let mut forged=d.clone();forged["proposals"][0]["editorialReview"]["reason"]=json!("forged");
    assert!(crate::proposal_current(&forged,&forged["proposals"][0]).is_err());
    let mut retargeted=d.clone();retargeted["proposals"][0]["routeTarget"]["connectorBinding"]
        =crate::accounts::Profile::BawRussia.binding();
    assert!(crate::proposal_current(&retargeted,&retargeted["proposals"][0]).is_err());
}
