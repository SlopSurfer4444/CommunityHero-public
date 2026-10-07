use super::*;
fn workspace()->Value{
    let mut d=crate::empty();crate::accounts::initialize(&mut d,crate::accounts::Profile::BawRussia).unwrap();
    d["posts"]=json!([{"id":"p","account":"BAW Russia","postKey":"12182:p","text":"Exact price 990000","attachments":[],"attachmentsState":"none"}]);
    d["items"]=json!([{"id":"i","postId":"p","postKey":"12182:p"}]);d
}
fn request(d:&Value)->Value{json!({"account":d["account"],"connectorBinding":d["connectorBinding"],"items":d["items"],"posts":d["posts"],"branches":[],"materials":[],"knowledgeManifest":[]})}
#[test]fn empty_known_media_is_ready_but_unknown_metadata_is_not(){
    let d=workspace();let mut req=request(&d);attach_request(&d,&mut req).unwrap();require_request(&d,&req).unwrap();
    let mut changed=d.clone();changed["posts"][0]["attachmentsState"]=json!("unknown");let mut unknown=request(&changed);attach_request(&changed,&mut unknown).unwrap();assert_eq!(require_request(&changed,&unknown),Err("mandatory_material_not_ready"));
}
#[test]fn pre_manual_paid_capture_keeps_empty_selection_without_rewriting_history(){
    let d=workspace();let mut req=request(&d);attach_request(&d,&mut req).unwrap();assert_eq!(req["manualFrameRequestIds"],json!([]));
    req.as_object_mut().unwrap().remove("manualFrameRequestIds");let original=req.clone();
    require_request(&d,&req).unwrap();assert_eq!(req,original,"Validation cannot add new fields to frozen paid bytes");
    let mut injected=req.clone();injected["optionalFrameRefs"]=json!([{"origin":"manual_before_generation","manualRequestId":"not-a-native-request"}]);
    assert!(require_request(&d,&injected).is_err(),"Unproven manual pixels never become a frozen empty selection");
}
#[test]fn mandatory_photos_do_not_reuse_text_only_or_summary_evidence(){
    let mut d=workspace();d["posts"][0]["attachments"]=json!([{"type":"photo","url":"https://example.invalid/photo"}]);
    let mut req=request(&d);req["materials"]=json!([{"kind":"ocr","text":"Price 990000"}]);attach_request(&d,&mut req).unwrap();assert_eq!(require_request(&d,&req),Err("mandatory_material_not_ready"));
    assert_eq!(req["visualSelection"]["postImages"][0]["attachmentIndices"],json!([0]));
}
#[test]fn count_overflow_is_finite_capacity_and_all_photo_identities_are_retained(){
    let mut d=workspace();d["posts"][0]["attachments"]=json!((0..17).map(|index|json!({"type":"photo","url":format!("https://example.invalid/photo-{index}")})).collect::<Vec<_>>());
    let mut req=request(&d);attach_request(&d,&mut req).unwrap();
    assert_eq!(req["postContextBundle"]["members"][0]["assets"].as_array().unwrap().len(),17);
    assert_eq!(req["visualSelection"]["postImages"][0]["attachmentIndices"],json!((0..17).collect::<Vec<_>>()));
    let overflow=rows(&req["materialReadiness"],"requirements").iter().find(|r|r["kind"]=="post_photo_transport").unwrap();
    assert_eq!(overflow["status"],"unavailable");assert_eq!(overflow["reasonCode"],"post_photo_count_exceeds_transport");assert_eq!(overflow["photoCount"],17);
    assert_eq!(require_request(&d,&req),Err("mandatory_material_not_ready"));
}
#[test]fn changed_text_and_member_company_invalidate_frozen_bundle(){
    let d=workspace();let mut req=request(&d);attach_request(&d,&mut req).unwrap();let mut changed=d.clone();changed["posts"][0]["text"]=json!("Different price");assert_eq!(require_request(&changed,&req),Err("mandatory_material_source_changed"));
    changed=d.clone();changed["posts"][0]["connectorBinding"]=crate::accounts::Profile::LikeAvto.binding();assert_eq!(require_request(&changed,&req),Err("mandatory_material_foreign_company"));
}
#[test]fn fresh_editorial_delivery_is_bound_to_its_native_dispatch_not_prepare_bundle(){
    let(mut d,refs)=crate::operator_editorial::tests::fixture();
    d["proposals"][0]["nativeCreationOrigin"]=json!("model_generation_v1");
    assert!(require_proposal(&crate::prepare_bundle::EvidenceContext::new(&d),&d["proposals"][0]).is_err());
    crate::editorial_review::fixture_accept(&mut d,refs[0]["id"].as_str().unwrap()).unwrap();
    assert!(d["proposals"][0]["modelMaterialReceipt"].is_null());
    assert!(require_proposal(&crate::prepare_bundle::EvidenceContext::new(&d),&d["proposals"][0]).is_ok());
    let current=d.clone();
    for mutation in ["paid","request","candidate","source","detached"]{
        let mut d=current.clone();
        match mutation{
            "paid"=>d["proposals"][0]["editorialModelMaterialReceipt"]["paidResultRef"]["requestSha256"]=json!("0".repeat(64)),
            "request"=>d["jobs"][0]["editorialBatches"][0]["capture"]["batch"]["request"]["instruction"]=json!("Substitute request"),
            "candidate"=>d["proposals"][0]["text"]=json!("Unreviewed replacement"),
            "source"=>d["posts"][0]["text"]=json!("New publication"),
            _=>d["jobs"][0]["retainedEvidence"]=json!([]),
        }
        assert!(require_proposal(&crate::prepare_bundle::EvidenceContext::new(&d),&d["proposals"][0]).is_err(),"{mutation}");
    }
}
#[test]fn native_manual_material_receipt_is_truthful_current_and_not_a_generated_fallback(){
    let(mut d,refs)=crate::operator_editorial::tests::fixture();
    let request=crate::operator_editorial::tests::body(&d,&refs);
    crate::operator_editorial::admit(&mut d,&crate::operator_editorial::tests::actor(),&request).unwrap();
    let receipt=&d["proposals"][0]["operatorMaterialReceipt"];
    assert_eq!(receipt["contract"],OPERATOR_CONTRACT);assert_eq!(receipt["modelCalled"],false);
    assert!(receipt.get("paidResultRef").is_none());assert!(receipt.get("materialInvocation").is_none());
    assert!(require_proposal(&crate::prepare_bundle::EvidenceContext::new(&d),&d["proposals"][0]).is_ok());
    for mutation in ["generated","legacy","recovery","copied","source","photo","text"]{
        let mut changed=d.clone();
        match mutation{
            "generated"=>changed["proposals"][0]["nativeCreationOrigin"]=json!("model_generation_v1"),
            "legacy"=>{changed["proposals"][0].as_object_mut().unwrap().remove("nativeCreationOrigin");},
            "recovery"=>changed["proposals"][0]["nativeCreationOrigin"]=json!("retained_model_recovery_v1"),
            "copied"=>changed["proposals"][0]["sourceProposalId"]=json!("old-model"),
            "source"=>changed["posts"][0]["text"]=json!("Changed source"),
            "photo"=>changed["posts"][0]["attachments"]=json!([{"type":"photo","url":"https://example.invalid/missing"}]),
            _=>changed["proposals"][0]["text"]=json!("Changed reply"),
        }
        assert!(require_proposal(&crate::prepare_bundle::EvidenceContext::new(&changed),&changed["proposals"][0]).is_err(),"{mutation}");
    }
    let(mut generated,refs)=crate::operator_editorial::tests::fixture();generated["proposals"][0]["nativeCreationOrigin"]=json!("model_generation_v1");
    let before=generated.clone();
    assert!(crate::operator_editorial::capture(&generated,&crate::operator_editorial::tests::actor(),&json!({"proposals":refs})).is_err());
    assert_eq!(generated,before,"unknown original delivery cannot be silently recast as a manual reply");
}
#[test]fn self_edited_manual_lineage_requires_exact_native_history_and_stays_reviewable(){
    let(mut d,refs)=crate::operator_editorial::tests::fixture();let id=refs[0]["id"].as_str().unwrap().to_owned();
    for n in 1..=2{
        let p=crate::row(&d,"proposals",&id).unwrap().clone();
        crate::edit_proposal(&mut d,&id,&json!({"expectedRevision":p["revision"],"text":format!("Owner manual edit {n}")})).unwrap();
        assert!(genuine_manual(&d["proposals"][0]),"native self edit {n}");
        let refs=json!([{"id":id,"revision":d["proposals"][0]["revision"]}]);
        let body=crate::operator_editorial::tests::body(&d,&refs);
        let mut body=body;body["requestId"]=json!(format!("native-manual-edit-review-{n}"));
        crate::operator_editorial::admit(&mut d,&crate::operator_editorial::tests::actor(),&body).unwrap();
        assert!(require_proposal(&crate::prepare_bundle::EvidenceContext::new(&d),&d["proposals"][0]).is_ok());
    }
    for mutation in ["foreign","wrong_item","changed_origin","model","nested_model"]{
        let mut p=d["proposals"][0].clone();
        match mutation{
            "foreign"=>p["origin"]["id"]=json!("foreign-proposal"),
            "wrong_item"=>p["origin"]["itemId"]=json!("foreign-recipient"),
            "changed_origin"=>p["origin"]["text"]=json!("Forged origin"),
            "model"=>{p["origin"]["nativeCreationOrigin"]=json!("model_generation_v1");p["history"][0]=p["origin"].clone();},
            _=>{p["origin"]["nested"]=json!({"generationMetadata":{"model":"test"}});p["history"][0]=p["origin"].clone();},
        }
        assert!(!genuine_manual(&p),"{mutation}");
    }
}
#[test]fn metadata_recovery_uses_original_generation_even_when_editorial_revision_is_stale(){
    let(mut d,refs)=crate::operator_editorial::tests::fixture();
    let bundle=crate::engine_prepare::build_request(&d,&[json!("i0")],None).unwrap();
    d["jobs"]=json!([{"id":"original-answering","kind":"assistant","purpose":"engine_prepare","status":"completed","prepareBundle":bundle}]);
    let mut result=crate::engine_prepare::tests::single_pass_result(json!({"text":"Synthetic original answer","sources":[],
        "assessments":[{"itemId":"i0","outcome":"reply","reason":"Synthetic exact reply","tags":[]}],
        "proposals":[{"itemId":"i0","kind":"reply_and_close","text":d["proposals"][0]["text"]}]}));
    crate::model_material_receipt::fixture_result(&mut d,"original-answering",&bundle["request"],&mut result).unwrap();
    d["proposals"][0]["modelMaterialReceipt"]=result["modelMaterialReceipt"].clone();
    d["proposals"][0]["nativeCreationOrigin"]=json!("model_generation_v1");
    crate::editorial_review::fixture_accept(&mut d,refs[0]["id"].as_str().unwrap()).unwrap();
    d["proposals"][0]["revision"]=json!(2);
    let context=crate::prepare_bundle::EvidenceContext::new(&d);
    assert!(require_proposal(&context,&d["proposals"][0]).is_err(),"stale editorial candidate is not new dispatch authority");
    assert!(require_original_generation(&context,&d["proposals"][0]).is_ok(),"original paid model material observation is unchanged");
    for mutation in ["missing","corrupt","source"]{
        let mut changed=d.clone();
        match mutation{
            "missing"=>changed["proposals"][0]["modelMaterialReceipt"]=Value::Null,
            "corrupt"=>changed["proposals"][0]["modelMaterialReceipt"]["paidResultRef"]["requestSha256"]=json!("0".repeat(64)),
            _=>changed["posts"][0]["text"]=json!("Changed post"),
        }
        assert!(require_original_generation(&crate::prepare_bundle::EvidenceContext::new(&changed),&changed["proposals"][0]).is_err(),"{mutation}");
    }
}

#[test]fn original_comment_source_only_receipt_is_mandatory_but_not_model_delivery(){
    let mut d=workspace();d["items"][0]["itemId"]=json!("provider-comment");d["items"][0]["objectId"]=json!("12182");d["items"][0]["conversationKey"]=json!("thread");d["items"][0]["platform"]=json!("VK");
    d["items"][0]["branchId"]=json!("branch");d["items"][0]["targetId"]=json!("message");d["items"][0]["revision"]=json!(1);d["items"][0]["connectorBinding"]=d["connectorBinding"].clone();
    let attachments=json!([{"type":"photo","url":"https://cdn.example/own.png"}]);d["items"][0]["attachments"]=attachments.clone();d["items"][0]["attachmentsState"]=json!("present");
    d["branches"]=json!([{"id":"branch","postId":"p","messages":[{"id":"message","role":"customer","roleEvidence":"connector-observed","attachments":attachments}]}]);
    let mut missing=request(&d);attach_request(&d,&mut missing).unwrap();assert_eq!(require_request(&d,&missing),Err("mandatory_material_not_ready"));
    let posts=d["posts"].clone();let receipt=crate::photo_acquisition::fixture_commit_comment_photo(&mut d,"i","2026-10-07T10:00:00Z").unwrap();assert_eq!(d["posts"],posts);assert_eq!(receipt["modelCalled"],false);
    let mut ready=request(&d);attach_request(&d,&mut ready).unwrap();require_request(&d,&ready).unwrap();assert_eq!(ready["postContextBundle"]["commentPhotos"].as_array().unwrap().len(),1);
    assert_eq!(ready["commentPhotoSources"][0]["sourceRole"]["role"],"customer");assert!(ready["commentPhotoSources"][0]["photo"].get("postId").is_none());
    assert!(crate::model_material_receipt::validate_result(&ready,&json!({"runMetadata":{}})).is_err(),"source receipt alone never proves model delivery");
    let mut changed=d.clone();changed["branches"][0]["messages"][0]["role"]=json!("brand");assert_eq!(require_request(&changed,&ready),Err("mandatory_material_source_changed"));
}
#[test]fn explicitly_unknown_or_conflicting_comment_metadata_is_a_finite_requirement(){
    for fault in ["unknown","present-without-slots","conflicting"]{
        let mut d=workspace();match fault{
            "unknown"=>d["items"][0]["attachmentsState"]=json!("unknown"),"present-without-slots"=>{d["items"][0]["attachmentsState"]=json!("present");d["items"][0]["attachments"]=json!([]);},
            _=>{d["items"][0]["attachments"]=json!([]);d["items"][0]["commentAttachments"]=json!([{"type":"photo","url":"https://cdn.example/conflicting.png"}]);}
        }let mut req=request(&d);attach_request(&d,&mut req).unwrap();assert_eq!(require_request(&d,&req),Err("mandatory_material_not_ready"),"{fault}");
        assert!(rows(&req["materialReadiness"],"requirements").iter().any(|requirement|requirement["kind"]=="comment_attachment_metadata"&&requirement["status"]=="unavailable"));
    }
}
