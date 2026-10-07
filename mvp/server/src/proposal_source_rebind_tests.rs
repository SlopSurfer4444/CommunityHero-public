//! Isolated native reducer/storage fixtures; execution belongs to root's queue.
use super::*;
fn actor()->operator_auth::Actor{operator_auth::Actor::local_owner("offline-source-rebind")}
fn fixture()->(Value,Value){
    let mut d=crate::empty();accounts::initialize(&mut d,accounts::Profile::BawRussia).unwrap();
    // Explicit complete isolated ledger before source capture or DB seeding.
    d["feedback"]=json!([]);d["knowledge_entries"]=json!([]);d["knowledge_versions"]=json!([]);
    d["items"]=json!([{"id":"rebind-item","itemId":"comment-1","objectId":"12182","platform":"VK","postKey":"12182:post",
        "conversationKey":"thread-1","branchId":"old-branch","postId":"post","targetId":"comment-1","revision":1,"workflow":"attention",
        "providerStatus":"new","text":"Синтетический отзыв","contextEvidenceDigest":"old-context","branchContextDigest":"old-branch-digest","connectorBinding":d["connectorBinding"]}]);
    d["posts"]=json!([{"id":"post","postKey":"12182:post","objectId":"12182","platform":"VK","text":"Полный синтетический пост","attachments":[],"attachmentsState":"none"}]);
    d["branches"]=json!([{"id":"old-branch","postId":"post","contextComplete":true,"messages":[{"id":"comment-1","providerItemId":"comment-1","providerObjectId":"12182","role":"customer","text":"Синтетический отзыв"}]}]);
    let bundle=prepare_bundle::build(&d,&[json!("rebind-item")],&[]).unwrap();
    let generated=create_generated_proposal(&mut d,&json!({"itemId":"rebind-item","expectedRevision":1,"kind":"reply_and_close","text":"Спасибо за отзыв.","sources":[]})).unwrap();
    let key=generated["id"].as_str().unwrap();let original=row_mut(&mut d,"proposals",key).unwrap();
    original["prepareRunId"]=json!("original-paid-owner");original["prepareBundleId"]=bundle["id"].clone();original["prepareBundleDigest"]=bundle["digest"].clone();
    original["sourceContextDigest"]=original["reviewContextDigest"].clone();
    d["jobs"]=json!([{"id":"original-paid-owner","kind":"assistant","purpose":"engine_prepare","refId":"engine_prepare","status":"completed","selectedItemIds":["rebind-item"],
        "prepareBundle":bundle,"preparationStages":{"first":{"status":"completed","result":{"text":"Preserved synthetic first result","proposals":[{"itemId":"rebind-item","kind":"reply_and_close","text":"Спасибо за отзыв."}]}},"groupAdmission":[]}}]);
    let reservation=preparation_reservations::capture(&d,"original-paid-owner").unwrap();d["jobs"][0]["scopeReservation"]=reservation;
    d["items"][0]["text"]=json!("Уточнённый синтетический отзыв");d["items"][0]["contextEvidenceDigest"]=json!("current-context");bump(&mut d["items"][0]);
    d["branches"][0]["messages"][0]["text"]=d["items"][0]["text"].clone();
    let refs=json!([{"id":generated["id"],"revision":generated["revision"],"expectedItemRevision":d["items"][0]["revision"]}]);(d,refs)
}
fn preview_body(refs:&Value)->Value{json!({"contract":CONTRACT,"admissionMode":"partial","proposals":refs})}
fn request(d:&Value,refs:&Value)->Value{
    let preview=capture(d,&actor(),&preview_body(refs)).unwrap();
    json!({"requestId":"offline-source-rebind","contract":CONTRACT,"admissionMode":"partial","proposals":refs,"previewDigest":preview["previewDigest"]})
}
#[test]
fn new_semantic_revision_preserves_text_paid_owner_and_requires_its_own_current_delivery(){
    let(mut d,refs)=fixture();let before=d.clone();let preview=capture(&d,&actor(),&preview_body(&refs)).unwrap();
    assert_eq!(preview["held"],json!([]));assert_eq!(preview["entries"].as_array().unwrap().len(),1);assert_eq!(d,before);
    let result=admit(&mut d,&actor(),&request(&before,&refs)).unwrap();assert_eq!(result["status"],"rebound");
    let key=result["newRefs"][0]["id"].as_str().unwrap();let p=row(&d,"proposals",key).unwrap();
    assert_eq!(p["revision"],2);assert_eq!(p["id"],before["proposals"][0]["id"]);assert_eq!(p["text"],before["proposals"][0]["text"]);
    for field in ["jobs","operations","approvals","items","posts","branches"]{assert_eq!(d[field],before[field],"rebind cannot mutate {field}");}
    assert_eq!(p["history"][0],historical(&before["proposals"][0]));assert!(p["editorialReview"].is_null());assert!(p["editorialModelMaterialReceipt"].is_null());
    assert_eq!(preparation_materials::require_proposal(&prepare_bundle::EvidenceContext::new(&d),p),Err("source_rebind_current_editorial_material_receipt_required"));
    assert!(require_current(&prepare_bundle::EvidenceContext::new(&d),p).is_err());
    assert!(preparation_reservations::assert_available(&d,&["rebind-item".into()],None).is_err(),"new revision never releases original generation budget");
    editorial_review::fixture_accept(&mut d,key).unwrap();
    let p=row(&d,"proposals",key).unwrap();require_current(&prepare_bundle::EvidenceContext::new(&d),p).unwrap();
    assert_ne!(p["editorialModelMaterialReceipt"]["nativeJobId"],p["prepareRunId"]);assert_eq!(d["jobs"][0],before["jobs"][0]);
    assert!(d["operations"].as_array().unwrap().is_empty());assert!(d["approvals"].as_array().unwrap().is_empty());
    for status in ["approved","dispatching","unknown","confirmed"] {
        let mut transitioned=d.clone();row_mut(&mut transitioned,"proposals",key).unwrap()["status"]=json!(status);
        validate_origin_and_current_target(&prepare_bundle::EvidenceContext::new(&transitioned),row(&transitioned,"proposals",key).unwrap()).unwrap();
    }
    let mut changed=d.clone();changed["posts"][0]["text"]=json!("Changed current semantic source after review");
    assert!(validate_origin_and_current_target(&prepare_bundle::EvidenceContext::new(&changed),row(&changed,"proposals",key).unwrap()).is_err());
}
#[test]
fn exact_replay_and_late_failure_are_atomic_and_never_schedule_model_or_operation(){
    let(mut d,refs)=fixture();let body=request(&d,&refs);let first=admit(&mut d,&actor(),&body).unwrap();let saved=d.clone();
    let replay=admit(&mut d,&actor(),&body).unwrap();assert_eq!(replay["replayed"],true);assert_eq!(replay["newRefs"],first["newRefs"]);assert_eq!(d,saved);
    let mut changed=body.clone();changed["previewDigest"]=json!("changed");assert!(admit(&mut d,&actor(),&changed).is_err());assert_eq!(d,saved);
    let(mut d,refs)=fixture();let body=request(&d,&refs);d["audit"]=json!([{"id":local_admission::receipt_id(ADMISSION,"offline-source-rebind"),"action":"foreign"}]);
    let before=d.clone();assert!(admit(&mut d,&actor(),&body).is_err());assert_eq!(d,before,"late invalid receipt identity cannot retain a revision");
}
#[test]
fn recipient_unknown_pending_material_and_retained_origin_are_explicit_holds(){
    let(d,refs)=fixture();
    for mutation in ["recipient","unknown","pending","owner-running","owner-unknown","retained","photo","no-old-source","native-origin","approved","stale-cancel"]{
        let mut changed=d.clone();match mutation{
            "recipient"=>changed["items"][0]["itemId"]=json!("another-recipient"),
            "unknown"=>changed["operations"]=json!([{"id":"unknown","status":"unknown","proposalId":"prior","itemId":"alias","target":changed["items"][0]}]),
            "pending"=>list_mut(&mut changed,"jobs").push(json!({"id":"pending-editorial","kind":"editorial_review","status":"running","editorialReferences":[{"id":refs[0]["id"],"revision":1}]})),
            "owner-running"=>changed["jobs"][0]["status"]=json!("running"),
            "owner-unknown"=>changed["jobs"][0]["scopeModelAttempt"]=json!({"status":"unknown"}),
            "retained"=>changed["proposals"][0][retained_paid_recovery::FIELD]=json!({"version":1}),
            "photo"=>changed["posts"][0]["attachments"]=json!([{"type":"photo","url":"https://example.invalid/unacquired.png"}]),
            "no-old-source"=>{changed["proposals"][0].as_object_mut().unwrap().remove("sourceContextDigest");},
            "native-origin"=>changed["proposals"][0]["nativeCreationOrigin"]=json!("operator_manual_v1"),
            "approved"=>changed["proposals"][0]["status"]=json!("approved"),
            _=>{changed["proposals"][0]["status"]=json!("stale");changed["proposals"][0]["staleReason"]=json!("Operator requested a fresh preparation run");}
        }
        let before=changed.clone();let preview=capture(&changed,&actor(),&preview_body(&refs)).unwrap();
        assert_eq!(preview["entries"],json!([]),"{mutation}");assert_eq!(preview["held"].as_array().unwrap().len(),1,"{mutation}");assert_eq!(changed,before);
        if mutation=="retained"{assert_eq!(preview["held"][0]["reason"],"retained_origin_rebind_unsupported");}
    }
}
#[test]
fn old_and_current_branch_keys_both_fence_rebind_without_changing_original_scope(){
    let(mut d,mut refs)=fixture();let old_scope=d["jobs"][0]["scopeReservation"].clone();
    let mut new_branch=d["branches"][0].clone();new_branch["id"]=json!("current-branch");list_mut(&mut d,"branches").push(new_branch);
    d["items"][0]["branchId"]=json!("current-branch");d["items"][0]["branchContextDigest"]=json!("new-branch-digest");bump(&mut d["items"][0]);refs[0]["expectedItemRevision"]=d["items"][0]["revision"].clone();
    let preview=capture(&d,&actor(),&preview_body(&refs)).unwrap();assert_eq!(preview["held"],json!([]));
    let scope=&preview["entries"][0]["proposal"][FIELD]["reservationScope"];
    assert_ne!(scope["oldKeys"],scope["currentKeys"]);assert!(list(scope,"unionKeys").len()>list(scope,"oldKeys").len());assert_eq!(d["jobs"][0]["scopeReservation"],old_scope);
    for branch in ["old-branch","current-branch"]{
        let mut blocked=d.clone();let mut sibling=blocked["items"][0].clone();sibling["id"]=json!("sibling");sibling["itemId"]=json!("sibling-external");sibling["branchId"]=json!(branch);sibling["conversationKey"]=json!("another-thread");
        list_mut(&mut blocked,"items").push(sibling.clone());let request=json!({"account":blocked["account"],"connectorBinding":blocked["connectorBinding"],"items":[sibling]});
        list_mut(&mut blocked,"jobs").push(json!({"id":"other-paid-owner","kind":"assistant","purpose":"engine_prepare","status":"running","selectedItemIds":["sibling"],
            "prepareBundle":{"id":"other-bundle","version":1,"itemIds":["sibling"],"request":request,"digest":hash(&request)}}));
        let preview=capture(&blocked,&actor(),&preview_body(&refs)).unwrap();assert_eq!(preview["entries"],json!([]),"{branch} remains fenced");
    }
}
#[tokio::test]
async fn native_writer_permit_rejects_generic_proof_mint_and_protects_existing_provenance(){
    let(mut d,refs)=fixture();let before=d.clone();let body=request(&d,&refs);admit(&mut d,&actor(),&body).unwrap();
    assert!(validate_change(&before,&d).is_err(),"matching JSON alone is not native authority");
    ADMISSION_PERMIT.scope((actor(),body.clone()),async{validate_change(&before,&d).unwrap()}).await;
    for field in [FIELD,"sourceContextDigest","modelMaterialReceipt","prepareBundleDigest","history"]{
        let mut changed=d.clone();changed["proposals"][0][field]=json!("tampered");assert!(validate_change(&d,&changed).is_err(),"{field}");
    }
}
#[tokio::test]
async fn sqlite_native_transition_reopens_replays_and_rolls_back_after_admission(){
    let folder=tempfile::tempdir().unwrap();let db=Database::Sqlite(open_db(&folder.path().join("source-rebind.sqlite")).await.unwrap());
    let(d,refs)=fixture();db.change(|workspace|{*workspace=d;Ok(())}).await.unwrap();let before=db.read().await.unwrap();let body=request(&before,&refs);
    let failed:ApiResult<Value>=ADMISSION_PERMIT.scope((actor(),body.clone()),db.change(|workspace|{admit_inner(workspace,&actor(),&body)?;Err(internal("offline failure after source revision"))})).await;
    assert!(failed.is_err());assert_eq!(db.read().await.unwrap(),before);
    let first=ADMISSION_PERMIT.scope((actor(),body.clone()),db.change(|workspace|admit_inner(workspace,&actor(),&body))).await.unwrap();
    let saved=db.read().await.unwrap();db.close().await;
    let reopened=Database::Sqlite(open_db(&folder.path().join("source-rebind.sqlite")).await.unwrap());
    let replay=ADMISSION_PERMIT.scope((actor(),body.clone()),reopened.change(|workspace|admit_inner(workspace,&actor(),&body))).await.unwrap();
    assert_eq!(replay["replayed"],true);assert_eq!(replay["newRefs"],first["newRefs"]);assert_eq!(reopened.read().await.unwrap(),saved);reopened.close().await;
}

#[test]
fn canonical_absent_optional_sections_preserve_unknown_delivery_and_original_bundle(){
    let(mut d,refs)=fixture();let before=d.clone();
    let original=&before["jobs"][0];
    assert!(original.get("retainedEvidence").is_none());
    assert!(original["prepareBundle"]["request"].get("customerCases").is_none());
    let bundle_bytes=serde_json::to_vec(&original["prepareBundle"]).unwrap();
    let preview=capture(&d,&actor(),&preview_body(&refs)).unwrap();
    assert_eq!(preview["held"],json!([]));assert_eq!(preview["entries"].as_array().unwrap().len(),1);
    let entry=&preview["entries"][0];
    assert_eq!(entry["oldSource"]["customerCases"],json!([]));
    assert_eq!(entry["oldSource"]["generationBundleDigest"],original["prepareBundle"]["digest"]);
    assert_eq!(entry["proposal"][FIELD]["paidProvenanceClass"],"legacy_saved_generated_text_delivery_unknown");
    assert!(entry["proposal"][FIELD]["paidOrigin"]["retainedEvidence"].is_null());
    assert_eq!(d,before,"Preview must not fill optional fields in historical evidence");
    let result=admit(&mut d,&actor(),&request(&before,&refs)).unwrap();
    assert_eq!(serde_json::to_vec(&d["jobs"][0]["prepareBundle"]).unwrap(),bundle_bytes);
    assert_eq!(d["jobs"],before["jobs"]);assert_eq!(d["operations"],before["operations"]);assert_eq!(d["approvals"],before["approvals"]);
    let p=row(&d,"proposals",result["newRefs"][0]["id"].as_str().unwrap()).unwrap();
    assert_eq!(p["history"][0],historical(&before["proposals"][0]));
    assert!(require_current(&prepare_bundle::EvidenceContext::new(&d),p).is_err(),"Legacy delivery never becomes current model delivery by source rebind");
}

#[test]
fn explicit_malformed_optional_paid_evidence_is_held_without_legacy_downgrade(){
    let(d,refs)=fixture();
    for malformed in [Value::Null,json!("missing"),json!({}),json!(1),json!(false)]{
        let mut changed=d.clone();changed["jobs"][0]["retainedEvidence"]=malformed;
        let before=changed.clone();let preview=capture(&changed,&actor(),&preview_body(&refs)).unwrap();
        assert_eq!(preview["entries"],json!([]));assert_eq!(preview["held"].as_array().unwrap().len(),1);
        assert_eq!(preview["held"][0]["reason"],"source_rebind_paid_evidence_unavailable");
        assert_eq!(changed,before);
    }
}

#[test]
fn explicit_malformed_optional_customer_context_is_held_without_bundle_rewrite(){
    let(d,refs)=fixture();
    for malformed in [Value::Null,json!("missing"),json!({}),json!(1),json!(false)]{
        let mut changed=d.clone();changed["jobs"][0]["prepareBundle"]["request"]["customerCases"]=malformed;
        // Bind this deliberately malformed retained request so provenance hash
        // validation cannot hide the optional-section schema boundary.
        let digest=hash(&changed["jobs"][0]["prepareBundle"]["request"]);
        changed["jobs"][0]["prepareBundle"]["digest"]=json!(digest);
        changed["proposals"][0]["prepareBundleDigest"]=changed["jobs"][0]["prepareBundle"]["digest"].clone();
        let before=changed.clone();let preview=capture(&changed,&actor(),&preview_body(&refs)).unwrap();
        assert_eq!(preview["entries"],json!([]));assert_eq!(preview["held"].as_array().unwrap().len(),1);
        assert_eq!(preview["held"][0]["reason"],"old_source_unavailable");assert_eq!(changed,before);
    }
}

#[test]
fn required_source_arrays_and_canonical_knowledge_scope_cannot_be_treated_as_optional(){
    let(d,_refs)=fixture();let original=d["jobs"][0].clone();let p=&d["proposals"][0];
    for key in ["items","branches","posts","materials","knowledgeManifest"]{
        for malformed in [None,Some(Value::Null),Some(json!("missing")),Some(json!({}))]{
            let mut job=original.clone();let fields=job["prepareBundle"]["request"].as_object_mut().unwrap();
            if let Some(value)=malformed{fields.insert(key.into(),value);}else{fields.remove(key);}
            let before=job.clone();assert!(old_source(&job,p).is_err(),"Required {key} cannot become empty context");assert_eq!(job,before);
        }
    }
    // Start with a real canonical producer, including its admitted scope array.
    let mut with_rule=d.clone();knowledge::save_instruction(&mut with_rule,&json!({"requestId":"rebind-scope-regression",
        "title":"Synthetic exact rule","text":"Use the supplied synthetic evidence.","postKey":"12182:post"}),&now()).unwrap();
    let bundle=prepare_bundle::build(&with_rule,&[json!("rebind-item")],&[]).unwrap();
    assert_eq!(bundle["request"]["knowledgeManifest"].as_array().unwrap().len(),1);
    assert_eq!(bundle["request"]["knowledgeManifest"][0]["scope"]["postKeys"],json!(["12182:post"]));
    let mut valid=original;valid["prepareBundle"]=bundle;assert!(old_source(&valid,p).is_ok());
    for malformed in [None,Some(Value::Null),Some(json!("12182:post")),Some(json!({})),Some(json!([null])),Some(json!([""]))]{
        let mut job=valid.clone();let scope=job["prepareBundle"]["request"]["knowledgeManifest"][0]["scope"].as_object_mut().unwrap();
        if let Some(value)=malformed{scope.insert("postKeys".into(),value);}else{scope.remove("postKeys");}
        let before=job.clone();assert!(old_source(&job,p).is_err(),"Malformed explicit scope cannot become a global source");assert_eq!(job,before);
    }
}
