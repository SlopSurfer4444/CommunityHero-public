//! Synthetic reducers and isolated CAS fixtures; root runs Cargo, no live work.
use super::*;
const AT:&str="2026-10-03T08:00:00Z";
fn fixture()->(Value,Value){
    let mut d=crate::empty();crate::accounts::initialize(&mut d,crate::accounts::Profile::LikeAvto).unwrap();
    for k in ["feedback","knowledge_entries","knowledge_versions","materials"]{d[k]=json!([]);}
    d["posts"]=json!([{"id":"post","postKey":"11341:post","objectId":"11341","platform":"VK","text":"Source publication","attachments":[{"type":"photo","url":"https://example.invalid/photo.jpg"}]}]);
    d["items"]=json!([{"id":"i","itemId":"comment","objectId":"11341","postId":"post","postKey":"11341:post","conversationKey":"thread","targetId":"message","platform":"VK","branchId":"branch","revision":1,"workflow":"prepared","providerStatus":"new","text":"Readable source comment","draft":"Existing paid draft","attachments":[],"contextEvidenceDigest":"context","branchContextDigest":"branch-context","connectorBinding":d["connectorBinding"]}]);
    d["branches"]=json!([{"id":"branch","postId":"post","contextComplete":true,"messages":[{"id":"message","role":"customer","text":"Readable source comment"}]}]);
    let body=json!({"receiptId":uuid::Uuid::new_v4().to_string(),"postId":"post","expectedSourceVersion":media_fullframes::source_version(&d["posts"][0],text(&d,"account")),"attachmentDigest":hash(&d["posts"][0]["attachments"])});
    (d,body)
}
fn fresh(d:&mut Value,body:&Value)->Claim{match claim(d,body,"owner",AT).unwrap(){Admission::Fresh(c)=>c,_=>panic!("fresh expected")}}
fn outcome(c:&Claim,store:&ArtifactStore)->Value{
    // Reducer fixture bytes are opaque synthetic artifacts; structural image
    // acceptance belongs to the connected JS downloader tests, not this mock.
    let bytes=format!("synthetic isolated pixels {}",text(&c.job,"id"));let r=store.put_bytes(bytes.as_bytes()).unwrap();
    json!({"images":[{"postId":c.pin["postId"],"attachmentIndex":0,"origin":"post_attachment","attachmentSha256":hash(&c.pin["attachments"][0]),"sha256":r.sha256,"bytes":r.bytes,"mime":"image/jpeg","width":10,"height":10,"artifact":r.to_json()}],"failures":[]})
}
fn settled()->(Value,Value,Value){let(mut d,b)=fixture();let c=fresh(&mut d,&b);let s=store().unwrap();let r=commit(&mut d,&c,&outcome(&c,&s),AT,&s).unwrap();(d,b,r)}

fn bounded_fixture(count:usize)->(Value,Value){
    let(mut d,mut body)=fixture();let template=d["items"][0].clone();
    d["items"]=json!((0..count).map(|n|{let mut item=template.clone();item["id"]=json!(format!("local-{n}"));item["itemId"]=json!(format!("provider-{n}"));item}).collect::<Vec<_>>());
    // The recipient selection never selects attachment indices. Every one of
    // these five source photos remains in the native source pin and receipt.
    d["posts"][0]["attachments"]=json!((0..5).map(|n|json!({"type":"photo","url":format!("https://example.invalid/source-{n}.jpg")})).collect::<Vec<_>>());
    body["expectedSourceVersion"]=json!(media_fullframes::source_version(&d["posts"][0],text(&d,"account")));body["attachmentDigest"]=json!(hash(&d["posts"][0]["attachments"]));
    (d,body)
}
fn select_five(body:&mut Value){body["recipients"]=json!((0..5).map(|n|json!({"itemId":format!("local-{n}"),"expectedRevision":1})).collect::<Vec<_>>());}

#[test]
fn photo_acquisition_bounded_legacy_four_fields_remain_all_post_and_preserve_limit(){
    let(mut d,body)=bounded_fixture(100);let c=fresh(&mut d,&body);
    assert_eq!(c.items.len(),100);assert_eq!(c.job["request"],body);assert!(c.job.get("recipientPins").is_none());assert_eq!(c.pin["attachments"],d["posts"][0]["attachments"]);
    let(mut d,body)=bounded_fixture(101);let before=d.clone();assert!(claim(&mut d,&body,"owner",AT).is_err());assert_eq!(d,before);
    let(mut d,body)=bounded_fixture(0);let before=d.clone();assert!(claim(&mut d,&body,"owner",AT).is_err());assert_eq!(d,before);
}
#[test]
fn photo_acquisition_bounded_explicit_five_large_post_keeps_full_source_and_no_semantic_admission(){
    let(mut d,mut body)=bounded_fixture(137);select_five(&mut body);let before=d.clone();let c=fresh(&mut d,&body);
    assert_eq!(c.items.len(),5);assert_eq!(c.items[4],json!({"id":"local-4","postId":"post","postKey":"11341:post"}));
    assert_eq!(c.job["recipientPins"].as_array().unwrap().len(),5);assert_eq!(c.job["recipientPins"][0]["providerItemId"],"provider-0");
    assert_eq!(c.pin["attachments"],before["posts"][0]["attachments"]);crate::db_guards::validate_change(&before,&d).unwrap();let claimed=d.clone();
    let store=store().unwrap();let images:Vec<_>=(0..5).map(|index|{let bytes=format!("fixture pixels {} slot {index}",text(&c.job,"id"));let r=store.put_bytes(bytes.as_bytes()).unwrap();
        json!({"postId":"post","attachmentIndex":index,"origin":"post_attachment","attachmentSha256":hash(&c.pin["attachments"][index]),"sha256":r.sha256,"bytes":r.bytes,"mime":"image/jpeg","width":10,"height":10,"artifact":r.to_json()})}).collect();
    let receipt=commit(&mut d,&c,&json!({"images":images,"failures":[]}),AT,&store).unwrap();crate::db_guards::validate_change(&claimed,&d).unwrap();
    assert_eq!(rows(&receipt,"images").len(),5);assert_eq!(receipt["sourcePin"]["attachments"],before["posts"][0]["attachments"]);
    assert_eq!(receipt["semanticAcceptance"],false);assert_eq!(receipt["modelCalled"],false);assert!(receipt.get("recipientPins").is_none());assert_eq!(d["items"],before["items"]);assert_eq!(d["proposals"],before["proposals"]);
}
#[test]
fn photo_acquisition_bounded_malformed_recipients_reject_before_durable_claim(){
    for selection in [json!(null),json!([]),json!({}),json!([{"itemId":"local-0","expectedRevision":0}]),json!([{"itemId":"local-0","expectedRevision":-1}]),json!([{"itemId":"local-0","expectedRevision":1.5}]),json!([{"itemId":"local-0","expectedRevision":"1"}]),json!([{"itemId":"local-0"}]),json!([{"itemId":"local-0","expectedRevision":1,"url":"https://example.invalid"}]),json!([{"itemId":"local-0","expectedRevision":1},{"itemId":"local-0","expectedRevision":1}]),json!((0..101).map(|n|json!({"itemId":format!("local-{n}"),"expectedRevision":1})).collect::<Vec<_>>())]{
        let(mut d,mut body)=bounded_fixture(137);body["recipients"]=selection;let before=d.clone();assert!(claim(&mut d,&body,"owner",AT).is_err());assert_eq!(d,before);
    }
    for id in ["", " local-0", "local-0 ","local,0","local\n0","provider-0"]{
        let(mut d,mut body)=bounded_fixture(137);body["recipients"]=json!([{"itemId":id,"expectedRevision":1}]);let before=d.clone();assert!(claim(&mut d,&body,"owner",AT).is_err());assert_eq!(d,before);
    }
    let(mut d,mut body)=bounded_fixture(137);body["recipients"]=json!([{"itemId":"x".repeat(129),"expectedRevision":1}]);let before=d.clone();assert!(claim(&mut d,&body,"owner",AT).is_err());assert_eq!(d,before);
}
#[test]
fn photo_acquisition_bounded_stale_missing_foreign_recipient_rejects_before_claim(){
    for fault in ["stale","missing","other-post","other-key","foreign-binding","foreign-account","foreign-accountId","foreign-scope","foreign-media-scope"]{
        let(mut d,mut body)=bounded_fixture(137);select_five(&mut body);
        match fault{
            "stale"=>d["items"][0]["revision"]=json!(2),"missing"=>body["recipients"][0]["itemId"]=json!("absent"),
            "other-post"=>d["items"][0]["postId"]=json!("other"),"other-key"=>d["items"][0]["postKey"]=json!("other"),
            "foreign-binding"=>d["items"][0]["connectorBinding"]=crate::accounts::Profile::BawRussia.binding(),
            "foreign-account"=>d["items"][0]["account"]=json!("BAW Russia"),"foreign-accountId"=>d["items"][0]["accountId"]=json!("BAW Russia"),
            "foreign-scope"=>d["items"][0]["scope"]=json!({"account":"BAW Russia"}),_=>d["items"][0]["sourceMediaScope"]=json!({"account":"BAW Russia"})
        }
        let before=d.clone();assert!(claim(&mut d,&body,"owner",AT).is_err(),"{fault}");assert_eq!(d,before);
    }
}
#[test]
fn photo_acquisition_bounded_unknown_outside_selection_and_source_owner_still_block(){
    for source_owner in [false,true]{let(mut d,mut body)=bounded_fixture(137);select_five(&mut body);
        if source_owner{list_mut(&mut d,"jobs").push(json!({"id":"source-owner","kind":"media","refId":"post","status":"unknown"}));}
        else{list_mut(&mut d,"operations").push(json!({"id":"outside-selection","itemId":"local-136","status":"unknown"}));}
        let before=d.clone();assert!(claim(&mut d,&body,"owner",AT).is_err());assert_eq!(d,before);
    }
}
#[test]
fn photo_acquisition_bounded_exact_replay_never_resizes_or_retargets_attempt(){
    let(mut d,mut body)=bounded_fixture(137);select_five(&mut body);let c=fresh(&mut d,&body);let claimed=d.clone();
    assert!(matches!(claim(&mut d,&body,"owner",AT).unwrap(),Admission::Replay(_)));assert_eq!(d,claimed);
    let mut changed=body.clone();changed["recipients"][0]["itemId"]=json!("local-6");assert!(claim(&mut d,&changed,"owner",AT).is_err());assert_eq!(d,claimed);
    let mut subset=body.clone();subset["recipients"].as_array_mut().unwrap().pop();assert!(claim(&mut d,&subset,"owner",AT).is_err());assert_eq!(d,claimed);
    let mut legacy=body.clone();legacy.as_object_mut().unwrap().remove("recipients");assert!(claim(&mut d,&legacy,"owner",AT).is_err());assert_eq!(d,claimed);
    let mut another=body.clone();another["receiptId"]=json!(uuid::Uuid::new_v4().to_string());assert!(claim(&mut d,&another,"owner",AT).is_err());assert_eq!(d,claimed);
    assert_eq!(c.items.len(),5);
}
#[test]
fn photo_acquisition_bounded_commit_rechecks_selected_revision_and_exact_route(){
    for fault in ["revision","postId","postKey","account","binding","provider-route"]{
        let(mut d,mut body)=bounded_fixture(137);select_five(&mut body);let c=fresh(&mut d,&body);let store=store().unwrap();
        // Honest all-slot failures exercise transaction admission without any
        // model or download seam, and do not make the image requirement ready.
        let failures:Vec<_>=(0..5).map(|i|json!({"postId":"post","attachmentIndex":i,"stage":"acquisition","category":"image_source_unavailable"})).collect();
        match fault{"revision"=>d["items"][0]["revision"]=json!(2),"postId"=>d["items"][0]["postId"]=json!("other"),"postKey"=>d["items"][0]["postKey"]=json!("other"),"account"=>d["items"][0]["account"]=json!("foreign"),"binding"=>d["items"][0]["connectorBinding"]=crate::accounts::Profile::BawRussia.binding(),_=>d["items"][0]["itemId"]=json!("retargeted-provider-id")}
        let before=d.clone();assert!(commit(&mut d,&c,&json!({"images":[],"failures":failures}),AT,&store).is_err(),"{fault}");assert_eq!(d,before);
    }
}

#[test]
fn photo_acquisition_claim_and_commit_follow_history_guards_preserve_paid_and_semantic_state(){
    let(mut d,b)=fixture();d["proposals"]=json!([{"id":"paid","itemId":"i","generationMetadata":{"paidRaw":"immutable"},"editorialReview":{"decision":"hold","receiptSha256":"historical"}}]);
    let initial=d.clone();let source_version=b["expectedSourceVersion"].clone();
    let prepared=prepare_bundle::fingerprint(&d,"i").unwrap();let reviewed=prepare_bundle::review_fingerprint(&d,"i").unwrap();
    let c=fresh(&mut d,&b);crate::db_guards::validate_change(&initial,&d).unwrap();let claimed=d.clone();
    let store=store().unwrap();let receipt=commit(&mut d,&c,&outcome(&c,&store),AT,&store).unwrap();
    crate::db_guards::validate_change(&claimed,&d).unwrap();
    assert_eq!(d["proposals"],initial["proposals"]);assert_eq!(d["items"],initial["items"]);
    assert_eq!(receipt["semanticAcceptance"],false);assert_eq!(receipt["modelCalled"],false);
    assert_eq!(media_fullframes::source_version(&d["posts"][0],text(&d,"account")),source_version);
    assert_eq!(prepare_bundle::fingerprint(&d,"i").unwrap(),prepared);assert_eq!(prepare_bundle::review_fingerprint(&d,"i").unwrap(),reviewed);
}
#[test]
fn photo_acquisition_replay_and_unknown_never_claim_again_or_retarget(){
    let(mut d,b,r)=settled();let original=d.clone();
    match claim(&mut d,&b,"owner",AT).unwrap(){Admission::Replay(v)=>assert_eq!(v["receipt"],r),_=>panic!("replay expected")};assert_eq!(d,original);
    assert!(claim(&mut d,&b,"foreign-owner",AT).is_err());
    let mut next=b.clone();next["receiptId"]=json!(uuid::Uuid::new_v4().to_string());assert!(claim(&mut d,&next,"owner",AT).is_err());
    let(mut uncertain,b)=fixture();let _=fresh(&mut uncertain,&b);uncertain["jobs"][0]["status"]=json!("unknown");
    let before=uncertain.clone();assert!(matches!(claim(&mut uncertain,&b,"owner",AT).unwrap(),Admission::Replay(_)));assert_eq!(uncertain,before);
}
#[test]
fn photo_acquisition_rejects_client_locator_path_metadata_and_stale_preconditions(){
    let(mut d,mut uppercase)=fixture();uppercase["receiptId"]=json!("AAAAAAAA-AAAA-AAAA-AAAA-AAAAAAAAAAAA");assert!(claim(&mut d,&uppercase,"owner",AT).is_err());assert!(rows(&d,"jobs").is_empty());
    for k in ["url","path","imageEvidence","account","attachments"]{let(mut d,mut b)=fixture();b[k]=json!("arbitrary");let before=d.clone();assert!(claim(&mut d,&b,"owner",AT).is_err());assert_eq!(d,before);}
    for k in ["expectedSourceVersion","attachmentDigest"]{let(mut d,mut b)=fixture();b[k]=json!("f".repeat(64));let before=d.clone();assert!(claim(&mut d,&b,"owner",AT).is_err());assert_eq!(d,before);}
}
#[test]
fn photo_acquisition_receipt_rejects_foreign_stale_and_metadata_tamper(){
    let(d,_,r)=settled();let s=store().unwrap();
    for field in ["account","connectorBinding","postId","sourceVersion","attachmentDigest"]{
        let mut forged=r.clone();forged["sourcePin"][field]=json!("foreign");forged["sourceDigest"]=json!(hash(&forged["sourcePin"]));forged["receiptSha256"]=json!(hash(&unsigned(&forged)));
        assert!(validate_receipt(&d,&d["posts"][0],&forged,&s).is_err(),"{field}");
    }
    let mut altered=r.clone();altered["images"][0]["width"]=json!(11);assert!(validate_receipt(&d,&d["posts"][0],&altered,&s).is_err());
    let mut stale=d.clone();stale["posts"][0]["attachments"][0]["url"]=json!("https://example.invalid/changed.jpg");assert!(validate_receipt(&stale,&stale["posts"][0],&r,&s).is_err());
}
#[test]
fn photo_acquisition_same_size_pixel_tamper_and_missing_object_fail_each_lookup(){
    for missing in [false,true]{let(mut d,_,r)=settled();let store=store().unwrap();let reference=image_meta(&r["images"][0]).unwrap();let path=store.path(&reference).unwrap();
        assert!(current_metadata(&d,&d["posts"][0]).is_ok());
        if missing{std::fs::remove_file(path).unwrap();}else{std::fs::write(path,vec![b'x';reference.bytes as usize]).unwrap();}
        assert!(current_metadata(&d,&d["posts"][0]).is_err());
        // Keep immutable rejected receipt, never grant a fallback ready token.
        d["posts"][0]["photoAcquisition"]=r;assert!(current_metadata(&d,&d["posts"][0]).is_err());
    }
}
#[test]
fn photo_acquisition_atomic_source_head_owner_and_competitor_cas_conflicts(){
    for fault in ["source","head","owner","competitor","unknown"]{
        let(mut d,b)=fixture();let c=fresh(&mut d,&b);let s=store().unwrap();let result=outcome(&c,&s);
        match fault{"source"=>d["posts"][0]["text"]=json!("changed"),"head"=>d["posts"][0]["photoAcquisition"]=json!({"other":"head"}),"owner"=>d["jobs"][0]["status"]=json!("unknown"),"competitor"=>list_mut(&mut d,"jobs").push(json!({"id":"competing","kind":"media","refId":"post","status":"running"})),_=>list_mut(&mut d,"operations").push(json!({"id":"unknown","itemId":"i","status":"unknown"}))}
        let before=d.clone();assert!(commit(&mut d,&c,&result,AT,&s).is_err(),"{fault}");assert_eq!(d,before);
    }
}
#[test]
fn photo_acquisition_restart_does_not_free_a_source_for_another_receipt(){
    let(mut d,b)=fixture();let _=fresh(&mut d,&b);crate::native_fixture_owner_repair::initialize_workspace(&mut d).unwrap(); crate::recover(&mut d).unwrap();assert_eq!(d["jobs"][0]["status"],"interrupted");
    d["posts"][0]["text"]=json!("new source revision");let next=json!({"receiptId":uuid::Uuid::new_v4().to_string(),"postId":"post","expectedSourceVersion":media_fullframes::source_version(&d["posts"][0],text(&d,"account")),"attachmentDigest":hash(&d["posts"][0]["attachments"])});
    let before=d.clone();assert!(claim(&mut d,&next,"owner",AT).is_err());assert_eq!(d,before);
}
#[test]
fn photo_acquisition_connector_refresh_preserves_native_head_and_rejects_injected_head(){
    let(mut d,_,receipt)=settled();let mut incoming=d["posts"][0].clone();incoming.as_object_mut().unwrap().remove("photoAcquisition");
    crate::merge_snapshot(&mut d,&json!({"posts":[incoming.clone()]})).unwrap();assert_eq!(d["posts"][0]["photoAcquisition"],receipt);assert!(current_metadata(&d,&d["posts"][0]).is_ok());
    incoming["photoAcquisition"]=json!({"injected":"connector"});crate::merge_snapshot(&mut d,&json!({"posts":[incoming.clone()]})).unwrap();assert_eq!(d["posts"][0]["photoAcquisition"],receipt);
    incoming["text"]=json!("changed source");crate::merge_snapshot(&mut d,&json!({"posts":[incoming]})).unwrap();assert_eq!(d["posts"][0]["photoAcquisition"],receipt);assert!(current_metadata(&d,&d["posts"][0]).is_err());
}
#[test]
fn photo_acquisition_partial_failure_is_not_ready_or_acceptance_and_backup_closure_is_exact(){
    let(mut d,b)=fixture();let c=fresh(&mut d,&b);let s=store().unwrap();
    let receipt=commit(&mut d,&c,&json!({"images":[],"failures":[{"postId":"post","attachmentIndex":0,"origin":"post_attachment","stage":"acquisition","category":"image_source_unavailable"}]}),AT,&s).unwrap();
    assert_eq!(receipt["semanticAcceptance"],false);assert!(rows(&current_metadata(&d,&d["posts"][0]).unwrap(),"imageEvidence").is_empty());assert!(artifact_refs(&d).unwrap().is_empty());
    let(d,_,r)=settled();let refs=artifact_refs(&d).unwrap();assert_eq!(refs.len(),1);assert_eq!(refs[0].to_json(),r["images"][0]["artifact"]);assert_eq!(s.backup_declaration(&refs).unwrap().objects.len(),1);
}

#[tokio::test]
async fn photo_acquisition_approval_execute_and_dispatch_freeze_exact_pixels(){
    let(app,_temp)=crate::tests::test_app().await;let mut d=app.read().await.unwrap();
    crate::connection_gate::fixture_open(&mut d).unwrap();
    d["posts"]=json!([{"id":"photo-post","postKey":d["items"][0]["postKey"],"text":"Photo source","attachments":[{"type":"photo","url":"https://example.invalid/fixed.jpg"}]}]);
    d["items"][0]["postId"]=json!("photo-post");
    let body=json!({"receiptId":uuid::Uuid::new_v4().to_string(),"postId":"photo-post","expectedSourceVersion":media_fullframes::source_version(&d["posts"][0],text(&d,"account")),"attachmentDigest":hash(&d["posts"][0]["attachments"])});
    let c=fresh(&mut d,&body);let store=store().unwrap();commit(&mut d,&c,&outcome(&c,&store),AT,&store).unwrap();
    let proposal=crate::create_proposal(&mut d,&json!({"itemId":"item-1","expectedRevision":1,"kind":"close"})).unwrap();
    let actor=operator_auth::Actor::local_owner("synthetic");
    let approval=crate::approval_admission::create(&mut d,&actor,&json!({"proposals":[{"id":proposal["id"],"revision":proposal["revision"]}]})).unwrap();
    assert!(!approval["proposals"][0]["approvedPhotoAcquisitionProof"].is_null());let approved=d.clone();
    let(_,scheduled)=crate::execute_admission::admit(&mut d,&actor,text(&approval,"id"),&json!({})).unwrap();let op=scheduled.unwrap().1.remove(0);
    assert_eq!(op["approvedPhotoAcquisitionProof"],approval["proposals"][0]["approvedPhotoAcquisitionProof"]);
    let p=row(&d,"proposals",text(&proposal,"id")).unwrap();assert!(media_context_gate::require(&prepare_bundle::EvidenceContext::new(&d),p,Some(&op)).is_ok());
    app.change(|workspace|{*workspace=d.clone();Ok(())}).await.unwrap();
    let compact=app.db.read_dispatch_context(text(&proposal,"id")).await.unwrap();
    assert_eq!(compact["posts"][0]["photoAcquisition"],d["posts"][0]["photoAcquisition"]);
    assert_eq!(compact["operations"][0]["approvedPhotoAcquisitionProof"],op["approvedPhotoAcquisitionProof"]);
    let mut swapped=approved.clone();let mut receipt=swapped["posts"][0]["photoAcquisition"].clone();let r=store.put_bytes(b"different pixels, exact unchanged source locator").unwrap();
    receipt["images"][0]["artifact"]=r.to_json();receipt["images"][0]["sha256"]=json!(r.sha256);receipt["images"][0]["bytes"]=json!(r.bytes);receipt["receiptSha256"]=json!(hash(&unsigned(&receipt)));swapped["posts"][0]["photoAcquisition"]=receipt;
    assert_eq!(media_fullframes::source_version(&swapped["posts"][0],text(&swapped,"account")),body["expectedSourceVersion"]);
    assert!(crate::execute_admission::admit(&mut swapped,&actor,text(&approval,"id"),&json!({})).is_err());
    let p=row(&swapped,"proposals",text(&proposal,"id")).unwrap();
    // Already-dispatched truth is reconciled under its original capture. Pixel
    // currentness blocks a NEW dispatch; it never authorizes a blind replay.
    let mut not_dispatched=op.clone();not_dispatched["status"]=json!("queued");
    assert!(media_context_gate::require(&prepare_bundle::EvidenceContext::new(&swapped),p,Some(&not_dispatched)).is_err());
    let mut unknown=op.clone();unknown["status"]=json!("unknown");
    assert!(media_context_gate::require(&prepare_bundle::EvidenceContext::new(&swapped),p,Some(&unknown)).is_ok());
    app.db.close().await;
}
