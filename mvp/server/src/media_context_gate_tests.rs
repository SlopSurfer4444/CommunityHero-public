//! Pure synthetic reducer checks. No database, auth file, provider or media tool.
use super::*;

pub(crate) fn fixture(kind: &str, attachments: Value) -> Value {
    let mut d = crate::empty();
    crate::accounts::initialize(&mut d, crate::accounts::Profile::LikeAvto).unwrap();
    for name in ["feedback", "knowledge_entries", "knowledge_versions", "materials"] {
        d[name] = json!([]);
    }
    d["items"] = json!([{"id":"i","itemId":"provider-comment","targetId":"message",
        "objectId":"11341","platform":"VK","postId":"post","postKey":"post","conversationKey":"thread",
        "branchId":"branch","revision":1,"workflow":"prepared","providerStatus":"new",
        "contextEvidenceDigest":"comment-context","branchContextDigest":"branch-context",
        "text":"A readable comment","attachments":[],"connectorBinding":d["connectorBinding"]}]);
    d["posts"] = json!([{"id":"post","postKey":"post","objectId":"11341","platform":"VK",
        "text":"A readable publication","attachments":attachments}]);
    d["branches"] = json!([{"id":"branch","postId":"post","contextComplete":true,
        "messages":[{"id":"message","parentId":null,"role":"customer","text":"A readable comment"}]}]);
    d["proposals"] = json!([{"id":"p","itemId":"i","revision":1,"itemRevision":1,
        "kind":kind,"text":if kind=="reply_and_close"{"A proposed reply"}else{""},"status":"draft",
        "routeTarget":{"connectorBinding":d["connectorBinding"],"objectId":"11341","itemId":"provider-comment",
            "postKey":"post","conversationKey":"thread"},
        "contextEvidenceDigest":"comment-context","branchContextDigest":"branch-context"}]);
    d
}

fn photos() -> Value {
    json!((0..3).map(|n|json!({"type":"photo","url":format!("https://example.invalid/photo-{n}.png")})).collect::<Vec<_>>())
}

pub(crate) fn video_fixture(kind: &str) -> Value {
    fixture(kind,json!([{"type":"video","url":"https://example.invalid/video.mp4"}]))
}

fn state(d: &Value) -> Value {
    inspect(&prepare_bundle::EvidenceContext::new(d), &d["proposals"][0]).unwrap()
}

fn named_operator() -> operator_auth::Actor {
    // Auth permission enforcement has its own tests. This reducer's authorized
    // argument simulates the verified result, never an HTTP client assertion.
    operator_auth::Actor {id:"alice".into(),name:"Alice".into(),role:"operator".into(),
        csrf_token:"synthetic-csrf".into(),authority_generation:Some("a".repeat(64))}
}

fn grant_body(d: &Value) -> Value {
    json!({"expectedProposalRevision":d["proposals"][0]["revision"],
        "expectedContextDigest":state(d)["contextDigest"],"reason":"Reviewed the failed exact acquisition"})
}

pub(crate) fn failed_video_attempt(d: &mut Value) {
    let source = media_fullframes::source_version(&d["posts"][0],d["account"].as_str().unwrap());
    d["jobs"] = json!([{"id":"acquisition","kind":"media","status":"failed",
        "account":d["account"],"connectorBinding":d["connectorBinding"],
        "sourceAttempts":[{"id":"attempt","postId":"post","sourceVersion":source,
            "status":"failed","phase":"download","error":"source_download_failed_network"}]}]);
}

pub(crate) fn transcript(d: &mut Value, silent: bool, ocr_metadata_only: bool) {
    let source = media_fullframes::source_version(&d["posts"][0],d["account"].as_str().unwrap());
    let mut transcription = json!({"sourceVersion":source,"partial":false,
        "coverage":if silent{"no_audio_stream"}else{"full_audio"},
        "audioStatus":if silent{"no_audio_stream"}else{"transcribed"},
        "mediaDurationSeconds":120.0,"audioDurationSeconds":if silent{Value::Null}else{json!(120.0)}});
    if ocr_metadata_only {
        transcription["ocr"] = json!({"sourceVersion":source,"coverage":"sampled_frames",
            "sampledFrames":1,"failedFrames":0,"status":"completed"});
    }
    d["materials"] = json!([{"id":"speech","account":d["account"],"kind":"transcript","postKey":"post",
        "text":if silent{"[Audio inspection: no_audio_stream. No spoken words were recovered.]"}else{"Actual source speech supplies general context."},
        "transcription":transcription}]);
    // Keep this synthetic head admitted before the real-time EvidenceContext:
    // the previous 10:00Z fixture was in the future during the morning run.
    let admitted_at=(chrono::Utc::now()-chrono::Duration::minutes(5)).to_rfc3339();
    crate::knowledge::sync_catalog(d,&admitted_at).unwrap();
}

fn image_failure(index: usize, stage: &str) -> Value {
    json!({"itemId":"i","itemIds":["i"],"postId":"post","attachmentIndex":index,
        "origin":"post_attachment","stage":stage,"category":"image_network"})
}

pub(crate) fn image_metadata(d: &mut Value, indices: &[usize], failures: Vec<Value>) {
    let bundle = prepare_bundle::build(d,&[json!("i")],&[]).unwrap();
    let metadata = json!({"schemaVersion":1,"imageEvidence":indices.iter().enumerate().map(|(number,index)|
        json!({"imageNumber":number+1,"itemId":"i","itemIds":["i"],"postId":"post",
            "attachmentIndex":index,"origin":"post_attachment","sha256":"b".repeat(64),
            "mime":"image/png","width":640,"height":480})).collect::<Vec<_>>(),"imageFailures":failures});
    assert!(prepare_bundle::validate_image_evidence_binding(&metadata,&bundle).is_ok(),
        "The hostile coverage input must remain bound to genuine fixture sources");
    assert!(prepare_bundle::current(d,&bundle).is_ok());
    d["jobs"] = json!([{"id":"image-preparation","kind":"assistant","status":"completed","prepareBundle":bundle}]);
    d["proposals"][0]["prepareRunId"] = json!("image-preparation");
    d["proposals"][0]["prepareBundleDigest"] = d["jobs"][0]["prepareBundle"]["digest"].clone();
    d["proposals"][0]["generationMetadata"] = metadata;
}

#[test]
fn three_missing_images_block_reply_and_empty_close_despite_generic_ready() {
    for kind in ["reply_and_close","close"] {
        let mut d = fixture(kind,photos());
        d["proposals"][0]["mediaContext"] = json!({"status":"ready"});
        d["proposals"][0]["mediaDependency"] = json!({"audio":"independent","visual":"independent"});
        let observed = state(&d);
        assert_eq!(observed["status"],"missing", "{kind}");
        assert_eq!(rows(&observed,"requirements").len(),3);
        assert!(rows(&observed,"requirements").iter().all(|r|r["ready"]==false));
        assert!(require(&prepare_bundle::EvidenceContext::new(&d),&d["proposals"][0],None).is_err());
    }
}

#[test]
fn independent_moderation_and_text_only_decisions_need_no_media() {
    for kind in ["hide","delete"] {
        let d = fixture(kind,photos());
        assert_eq!(state(&d)["status"],"not-required");
        assert!(require(&prepare_bundle::EvidenceContext::new(&d),&d["proposals"][0],None).is_ok());
    }
    for kind in ["reply_and_close","close"] {
        let d = fixture(kind,json!([]));
        assert_eq!(state(&d)["status"],"not-required");
        assert_eq!(require(&prepare_bundle::EvidenceContext::new(&d),&d["proposals"][0],None).is_ok(),kind!="reply_and_close","a bare legacy reply has no mandatory delivery or native manual proof");
    }
}

#[test]
fn unavailable_or_contradictory_metadata_never_becomes_text_only() {
    for kind in ["reply_and_close","close"] {
        for collection in ["items","posts"] {
            for state_name in ["unknown","unavailable","present"] {
                let mut d=fixture(kind,json!([]));d[collection][0]["attachmentsState"]=json!(state_name);
                assert_eq!(state(&d)["status"],"missing","{kind}/{collection}/{state_name}");
                assert!(require(&prepare_bundle::EvidenceContext::new(&d),&d["proposals"][0],None).is_err());
            }
            let mut d=fixture(kind,json!([]));d[collection][0]["attachmentsState"]=json!("none");
            assert_eq!(state(&d)["status"],"not-required");
        }
    }
}

#[test]
fn full_asr_is_general_context_without_optional_ocr() {
    let mut d = video_fixture("reply_and_close");transcript(&mut d,false,false);
    let context = prepare_bundle::EvidenceContext::new(&d);
    let proof = context.strict_media_evidence(&d["posts"][0]).unwrap();
    assert_eq!(proof["audioReady"],true);
    assert_eq!(proof["audioHasContent"],true);
    assert_eq!(proof["screenTextHasContent"],false);
    assert_eq!(inspect(&context,&d["proposals"][0]).unwrap()["status"],"ready");
}

#[test]
fn silent_video_needs_actual_screen_text_or_visual_proof_not_ocr_status_alone() {
    for ocr_metadata_only in [false,true] {
        let mut d = video_fixture("close");transcript(&mut d,true,ocr_metadata_only);
        let context = prepare_bundle::EvidenceContext::new(&d);
        let proof = context.strict_media_evidence(&d["posts"][0]).unwrap();
        assert_eq!(proof["audioReady"],true,"Silent-track absence itself must be source-proven");
        assert_eq!(proof["audioHasContent"],false);
        assert_eq!(proof["visualReady"],false);
        assert_eq!(proof["screenTextHasContent"],false,"No OCR text exists in this fixture");
        assert_eq!(inspect(&context,&d["proposals"][0]).unwrap()["status"],"missing");
    }
}

#[test]
fn exact_image_coverage_rejects_partial_receipts() {
    let mut d = fixture("reply_and_close",photos());
    image_metadata(&mut d,&[0,1,2],vec![]);
    assert_eq!(state(&d)["status"],"ready");
    d["proposals"][0]["generationMetadata"]["imageEvidence"].as_array_mut().unwrap().pop();
    let observed = state(&d);
    assert_eq!(observed["status"],"missing");
    assert_eq!(rows(&observed,"requirements").iter().filter(|r|r["ready"]!=true).count(),1);
}

#[test]
fn local_owner_and_unpermitted_named_operator_cannot_waive() {
    let mut d = video_fixture("close");failed_video_attempt(&mut d);
    let body = grant_body(&d);let before = d.clone();
    assert!(grant(&mut d,"p",&body,&operator_auth::Actor::local_owner("synthetic"),true).is_err());
    assert_eq!(d,before);
    assert!(grant(&mut d,"p",&body,&named_operator(),false).is_err());
    assert_eq!(d,before);
}

#[test]
fn named_permission_waives_one_exact_decision_after_current_source_failure() {
    for kind in ["reply_and_close","close"] {
        let mut d = video_fixture(kind);failed_video_attempt(&mut d);
        let before = state(&d);let body = grant_body(&d);
        let result = grant(&mut d,"p",&body,&named_operator(),true).unwrap();
        assert_eq!(result["status"],"waived");
        assert_eq!(result["contextDigest"],before["contextDigest"]);
        assert_eq!(result["waiver"]["actor"]["id"],"alice");
        assert_eq!(result["waiver"]["attempts"][0]["attemptId"],"attempt");
        assert_eq!(rows(&d,"audit").len(),1);
        assert_eq!(require(&prepare_bundle::EvidenceContext::new(&d),&d["proposals"][0],None).is_ok(),kind!="reply_and_close","a waiver never substitutes mandatory answering materials");
        let mut sibling = d["proposals"][0].clone();sibling["id"] = json!("other-proposal");
        assert!(require(&prepare_bundle::EvidenceContext::new(&d),&sibling,None).is_err());
    }
}

#[test]
fn failed_cached_audio_pin_is_exact_source_evidence_and_foreign_pin_cannot_waive() {
    for change in ["none","source","account","binding","post","status","sha"] {
        let mut d=video_fixture("close");
        let source=media_fullframes::source_version(&d["posts"][0],d["account"].as_str().unwrap());
        let pin=json!({"originJobId":"origin","originEpoch":2,"progress":{
            "account":d["account"],"connectorBinding":d["connectorBinding"],"sourcePostId":"post",
            "sourcePostKey":"post","sourceVersion":source,"source":{"sha256":"b".repeat(64)},
            "sourceIdentity":{"account":d["account"],"postKey":"post","mediaSha256":"b".repeat(64)}},
            "policy":{"account":d["account"],"connectorBinding":d["connectorBinding"],"sourceVersion":source}});
        d["jobs"]=json!([{"id":"cached-failure","kind":"media_audio","status":"failed","refId":"post",
            "account":d["account"],"connectorBinding":d["connectorBinding"],"audioPin":pin,"error":"audio_extraction_failed"}]);
        match change {
            "source"=>d["jobs"][0]["audioPin"]["progress"]["sourceVersion"]=json!("foreign-source"),
            "account"=>d["jobs"][0]["audioPin"]["progress"]["account"]=json!("BAW Russia"),
            "binding"=>d["jobs"][0]["audioPin"]["policy"]["connectorBinding"]["id"]=json!("foreign"),
            "post"=>d["jobs"][0]["audioPin"]["progress"]["sourcePostId"]=json!("another-post"),
            "status"=>d["jobs"][0]["status"]=json!("running"),
            "sha"=>d["jobs"][0]["audioPin"]["progress"]["sourceIdentity"]["mediaSha256"]=json!("wrong"),
            _=>{},
        }
        let body=grant_body(&d);let result=grant(&mut d,"p",&body,&named_operator(),true);
        if change=="none"{assert_eq!(result.unwrap()["status"],"waived");}
        else {assert!(result.is_err(),"{change}");assert!(d["proposals"][0]["mediaContextWaiver"].is_null());}
    }
}

#[test]
fn stale_revision_digest_or_failure_source_cannot_grant() {
    for change in ["revision","digest","source","company","connector"] {
        let mut d = video_fixture("reply_and_close");failed_video_attempt(&mut d);
        let mut body = grant_body(&d);
        match change {
            "revision"=>body["expectedProposalRevision"]=json!(2),
            "digest"=>body["expectedContextDigest"]=json!("stale"),
            "source"=>d["jobs"][0]["sourceAttempts"][0]["sourceVersion"]=json!("f".repeat(64)),
            "company"=>d["jobs"][0]["account"]=json!("BAW Russia"),
            "connector"=>d["jobs"][0]["connectorBinding"]["id"]=json!("different-connection"),
            _=>unreachable!(),
        }
        let before = d.clone();
        assert!(grant(&mut d,"p",&body,&named_operator(),true).is_err(),"{change}");
        assert_eq!(d,before,"{change}");
    }
}

#[test]
fn waived_decision_rejects_text_source_route_company_and_revision_drift() {
    for change in ["text","source","route","company","revision","context"] {
        let mut d = video_fixture("reply_and_close");failed_video_attempt(&mut d);
        let body = grant_body(&d);grant(&mut d,"p",&body,&named_operator(),true).unwrap();
        match change {
            "text"=>d["proposals"][0]["text"]=json!("A changed reply"),
            "source"=>d["posts"][0]["attachments"][0]["url"]=json!("https://example.invalid/replaced.mp4"),
            "route"=>d["proposals"][0]["routeTarget"]["itemId"]=json!("another-recipient"),
            "company"=>d["account"]=json!("BAW Russia"),
            "revision"=>d["proposals"][0]["revision"]=json!(2),
            "context"=>d["items"][0]["contextEvidenceDigest"]=json!("changed-comment-context"),
            _=>unreachable!(),
        }
        assert!(require(&prepare_bundle::EvidenceContext::new(&d),&d["proposals"][0],None).is_err(),"{change}");
    }
}

#[test]
fn no_failed_acquisition_and_not_started_images_cannot_waive() {
    let mut d = video_fixture("reply_and_close");let body = grant_body(&d);
    assert!(grant(&mut d,"p",&body,&named_operator(),true).is_err());
    let mut d = fixture("reply_and_close",photos());
    image_metadata(&mut d,&[0,1],vec![image_failure(2,"not_started")]);
    let observed = state(&d);
    assert_eq!(observed["status"],"missing");
    assert!(rows(&observed,"attempts").is_empty(),"No acquisition was attempted for the missing source");
    let body = grant_body(&d);let before = d.clone();
    assert!(grant(&mut d,"p",&body,&named_operator(),true).is_err());
    assert_eq!(d,before);
}

#[test]
fn current_failed_image_acquisition_can_be_waived_but_unknown_operation_cannot() {
    let mut d = fixture("reply_and_close",photos());
    image_metadata(&mut d,&[0,1],vec![image_failure(2,"acquisition")]);
    assert_eq!(state(&d)["attempts"][0]["status"],"failed");
    let mut held = d.clone();held["operations"] = json!([{"id":"unknown-op","itemId":"i","status":"unknown"}]);
    let body = grant_body(&held);let before = held.clone();
    assert!(grant(&mut held,"p",&body,&named_operator(),true).is_err());
    assert_eq!(held,before);
    let body = grant_body(&d);assert_eq!(grant(&mut d,"p",&body,&named_operator(),true).unwrap()["status"],"waived");
}

#[test]
fn operation_must_pin_the_exact_waiver() {
    let mut d = video_fixture("close");failed_video_attempt(&mut d);
    let body = grant_body(&d);grant(&mut d,"p",&body,&named_operator(),true).unwrap();
    let waiver = d["proposals"][0]["mediaContextWaiver"].clone();
    let op = json!({"approvedMediaContextWaiver":waiver});
    assert!(require(&prepare_bundle::EvidenceContext::new(&d),&d["proposals"][0],Some(&op)).is_ok());
    assert!(require(&prepare_bundle::EvidenceContext::new(&d),&d["proposals"][0],Some(&json!({}))).is_err());
    let mut changed = op;changed["approvedMediaContextWaiver"]["reason"] = json!("Different receipt");
    assert!(require(&prepare_bundle::EvidenceContext::new(&d),&d["proposals"][0],Some(&changed)).is_err());
}

fn admit_photo_review(d:&mut Value, reference:&Value, decision:&str) {
    let at=chrono::Utc::now().to_rfc3339();
    let plan=editorial_review::plan(d,&json!([reference]),&at).unwrap();
    assert!(rows(&plan,"held").is_empty(),"{plan}");
    let batch=&plan["batches"][0];let c=&batch["request"]["editorialCandidates"][0];
    let mut metadata=editorial_review::fixture_metadata();
    metadata["imageEvidence"]=json!((0..2).map(|index|json!({"imageNumber":index+1,
        "itemId":"i","itemIds":["i"],"postId":"post","attachmentIndex":index,
        "origin":"post_attachment","sha256":"b".repeat(64),"mime":"image/png","width":640,"height":480})).collect::<Vec<_>>());
    metadata["imageFailures"]=json!([image_failure(2,"acquisition")]);
    let verdict=json!({"proposalId":c["proposalId"],"proposalRevision":c["proposalRevision"],
        "itemId":c["itemId"],"textSha256":c["textSha256"],"contextDigest":c["contextDigest"],
        "rulesDigest":c["rulesDigest"],"decision":decision,"reason":"Synthetic exact caption review with third photo unavailable",
        "proposedText":null,"checks":{"companyRules":"pass","intent":"pass",
            "factualScope":if decision=="hold"{"uncertain"}else{"pass"}},
        "mediaDependency":{"audio":"independent","visual":"independent"}});
    let result=json!({"text":"Synthetic exact review","sources":[],"proposals":[],"editorial":[verdict],"runMetadata":metadata});
    let admitted=editorial_review::admit(d,batch,&result,&at).unwrap();
    assert_eq!(admitted["outcomes"][0]["decision"],decision);
}

#[test]
fn current_held_photo_receipt_exposes_real_partial_acquisition_but_never_semantic_acceptance() {
    for kind in ["reply_and_close","close"] {
    let mut d=fixture(kind,photos());d["proposals"]=json!([]);
    let binding=crate::active_binding(&d).unwrap();
    let target=crate::connectors::ResourceRef::from_item(&binding,&d["items"][0]).unwrap();
    assert_eq!(target.post_key,"post");assert_eq!(target.conversation_key,"thread");
    let approval_actor=operator_auth::Actor::local_owner("synthetic-owner-close");
    let p=crate::create_proposal(&mut d,&json!({"itemId":"i","expectedRevision":1,
        "kind":kind,"text":if kind=="reply_and_close"{"A proposed reply"}else{""},
        "_verifiedActor":approval_actor.public_json()})).unwrap();
    crate::validate_route(&p,&binding,&d["items"][0]).unwrap();
    if kind=="close" {assert!(crate::operator_close::current(&d,&p,&d["items"][0]).unwrap());}
    let reference=json!({"id":p["id"],"revision":p["revision"]});
    let proposal_id=p["id"].as_str().unwrap();
    admit_photo_review(&mut d,&reference,"hold");
    let observed=state(&d);
    assert_eq!(observed["status"],"missing");
    assert_eq!(rows(&observed,"requirements").iter().filter(|r|r["ready"]==true).count(),2);
    assert_eq!(rows(&observed,"requirements").iter().filter(|r|r["ready"]!=true).count(),1);
    assert_eq!(rows(&observed,"attempts").len(),1);
    assert_eq!(observed["attempts"][0]["sourceId"],rows(&observed,"requirements").iter().find(|r|r["ready"]!=true).unwrap()["sourceId"]);
    // A damaged, foreign or stale receipt cannot contribute observations.
    for change in ["hash","company","candidate","source"] {
        let mut altered=d.clone();
        match change {
            "hash"=>altered["proposals"][0]["editorialReview"]["receiptSha256"]=json!("invalid"),
            "company"=>altered["proposals"][0]["editorialReview"]["account"]=json!("BAW Russia"),
            "candidate"=>altered["proposals"][0]["text"]=json!("Another reply"),
            "source"=>altered["posts"][0]["attachments"][0]["url"]=json!("https://example.invalid/changed.png"),
            _=>unreachable!(),
        }
        let result=state(&altered);
        assert!(rows(&result,"requirements").iter().all(|r|r["ready"]==false),"{change}");
        assert!(rows(&result,"attempts").is_empty(),"{change}");
    }
    let body=grant_body(&d);
    assert_eq!(grant(&mut d,proposal_id,&body,&named_operator(),true).unwrap()["status"],"waived");
    if kind=="close" {
        let error=require(&prepare_bundle::EvidenceContext::new(&d),&d["proposals"][0],None).unwrap_err();
        assert_eq!(error.1,"Current editorial HOLD requires a fresh exact review before closing");
    } else {assert!(require(&prepare_bundle::EvidenceContext::new(&d),&d["proposals"][0],None).is_err(),"partial images and a waiver cannot prove complete mandatory answering input");}
    assert_eq!(d["proposals"][0]["editorialReview"]["decision"],"hold");
    assert!(editorial_review::require_current(&prepare_bundle::EvidenceContext::new(&d),&d["proposals"][0]).is_err());
    let approval_body=json!({"proposals":[reference]});let before=d.clone();
    assert!(crate::create_approval(&mut d,&approval_actor,&approval_body).is_err());
    assert_eq!(d,before,"Manual exception must not admit a held semantic decision");
    // Semantic acceptance cannot prove delivery of the unavailable image to
    // the answering model. A legacy partial fixture remains ineligible.
    admit_photo_review(&mut d,&reference,"accept");
    assert_eq!(state(&d)["status"],"waived");
    if kind=="close" {
        let approval=crate::create_approval(&mut d,&approval_actor,&approval_body).unwrap();
        assert_eq!(approval["status"],"approved");
    } else {
        let before=d.clone();
        let error=crate::create_approval(&mut d,&approval_actor,&approval_body).unwrap_err();
        assert_eq!(error.1,"legacy_material_contract_unmet");
        assert_eq!(d,before,"A waiver and semantic verdict cannot invent mandatory image input");
    }
    assert!(rows(&d,"operations").is_empty(),"Approval is not dispatch");
    }
}
