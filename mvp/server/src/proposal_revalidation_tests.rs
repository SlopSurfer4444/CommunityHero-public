//! Synthetic native reducer tests. ROOT alone runs Cargo/connected SQL.
use super::*;
fn actor()->operator_auth::Actor{operator_auth::Actor::local_owner("offline-revalidation-owner")}
fn fixture(kind:&str,owner_close:bool,video:bool)->(Value,Value){
    let (mut d,_)=operator_editorial::tests::fixture();d["proposals"]=json!([]);
    d["items"][0]["targetId"]=json!("addressed-comment");
    d["branches"][0]["messages"]=json!([{"id":"addressed-comment","text":"A self-contained readable comment"}]);
    if video{d["posts"][0]["attachments"]=json!([{"type":"video","url":"https://example.invalid/offline.mp4"}]);}
    let text=if kind=="reply_and_close"{"Exact paid draft text 🙂"}else{""};
    let mut body=json!({"itemId":"i0","expectedRevision":d["items"][0]["revision"],"kind":kind,"text":text});
    if owner_close{body["_verifiedActor"]=actor().public_json();}
    let p=create_proposal(&mut d,&body).unwrap();
    // Explicit historical schema fixture: no current marker was stored then.
    d["proposals"][0].as_object_mut().unwrap().remove("decisionMediaContract");
    d["proposals"][0].as_object_mut().unwrap().remove("nativeCreationOrigin");
    let refs=json!([{"id":p["id"],"revision":p["revision"]}]);(d,refs)
}
fn preview_body(refs:&Value)->Value{json!({"contract":CONTRACT,"admissionMode":"partial","proposals":refs})}
fn request(d:&Value,refs:&Value)->Value{
    let body=preview_body(refs);let preview=capture(d,&actor(),&body).unwrap();
    json!({"requestId":"offline-revalidate","contract":CONTRACT,"admissionMode":"partial","proposals":refs,"previewDigest":preview["previewDigest"]})
}
fn review(d:&mut Value,refs:&Value)->ApiResult<Value>{
    if refs.as_array().unwrap().iter().all(|r|row(d,"proposals",r["id"].as_str().unwrap()).unwrap()["kind"]=="reply_and_close") {
        for r in refs.as_array().unwrap() {
            editorial_review::fixture_accept(d,r["id"].as_str().unwrap()).map_err(conflict)?;
        }
        return Ok(json!({"accepted":refs}));
    }
    let mut body=operator_editorial::tests::body(d,refs);
    for entry in body["operatorReview"]["entries"].as_array_mut().unwrap(){entry["mediaDependency"]=json!({"audio":"independent","visual":"independent"});}
    operator_editorial::admit(d,&actor(),&body)
}
fn assert_preserved(before:&Value,after:&Value){
    for key in ["items","branches","posts","jobs","operations","approvals","feedback","materials","knowledge_entries","knowledge_versions"]{
        assert_eq!(before[key],after[key],"Revalidation cannot mutate {key}");
    }
    let mut a=before["proposals"][0].clone();let mut b=after["proposals"][0].clone();
    for key in ["revision","history","decisionMediaContract","proposalRevalidation","operatorCloseDecision"]{a.as_object_mut().unwrap().remove(key);b.as_object_mut().unwrap().remove(key);}
    assert_eq!(a,b,"All text/action/source/route/paid provenance fields remain exact");
}
#[test]
fn all_three_legacy_kinds_revalidate_same_id_but_require_fresh_semantics_and_approval(){
    for kind in ["reply_and_close","close","delete"]{
        let (mut d,refs)=fixture(kind,false,false);let before=d.clone();let body=request(&d,&refs);
        assert_eq!(d,before,"preview is read-only");
        let result=admit(&mut d,&actor(),&body).unwrap();assert_eq!(result["status"],"revalidated");
        assert_preserved(&before,&d);assert_eq!(d["proposals"].as_array().unwrap().len(),1);
        assert_eq!(d["proposals"][0]["id"],before["proposals"][0]["id"]);assert_eq!(d["proposals"][0]["revision"],2);
        assert_eq!(d["proposals"][0]["history"][0],historical(&before["proposals"][0]));
        assert_eq!(d["proposals"][0]["status"],"draft");
        assert!(create_approval(&mut d,&actor(),&json!({"proposals":result["newRefs"]})).is_err(),"contract migration alone never approves {kind}");
        assert!(create_approval(&mut d,&actor(),&json!({"proposals":refs})).is_err(),"old revisions cannot be approved");
        review(&mut d,&result["newRefs"]).unwrap();
        let approved=create_approval(&mut d,&actor(),&json!({"proposals":result["newRefs"]})).unwrap();assert_eq!(approved["status"],"approved");
        assert!(d["operations"].as_array().unwrap().is_empty());
    }
}
#[test]
fn existing_current_owner_close_is_narrowly_rebound_and_never_gains_default_semantic_bypass(){
    let (mut d,refs)=fixture("close",true,false);let before=d.clone();let body=request(&d,&refs);
    let old=before["proposals"][0]["operatorCloseDecision"].clone();let result=admit(&mut d,&actor(),&body).unwrap();
    let new=d["proposals"][0]["operatorCloseDecision"].clone();
    assert_eq!(new["proposalRevision"],2);assert_ne!(old["decisionSha256"],new["decisionSha256"]);
    let mut a=old.clone();let mut b=new.clone();for key in ["proposalRevision","decisionSha256"]{a.as_object_mut().unwrap().remove(key);b.as_object_mut().unwrap().remove(key);}assert_eq!(a,b);
    assert!(operator_close::current(&d,&d["proposals"][0],&d["items"][0]).unwrap());
    assert!(create_approval(&mut d,&actor(),&json!({"proposals":result["newRefs"]})).is_err(),"non-video owner CLOSE still needs a new exact receipt");
    review(&mut d,&result["newRefs"]).unwrap();assert!(create_approval(&mut d,&actor(),&json!({"proposals":result["newRefs"]})).is_ok());
    assert_eq!(d["proposals"][0]["history"][0]["operatorCloseDecision"],old);
    let event=list(&d,"audit").iter().find(|event|event["action"]=="proposal.revalidated").unwrap();
    assert_eq!(event["transitions"][0]["priorOperatorCloseDecisionSha256"],old["decisionSha256"]);
    assert_eq!(event["transitions"][0]["operatorCloseDecisionSha256"],new["decisionSha256"]);
}
#[test]
fn legacy_incomplete_video_can_be_staged_without_inventing_readiness_or_semantic_media_dependency(){
    for kind in ["reply_and_close","close","delete"]{
        let (mut d,refs)=fixture(kind,kind=="close",true);
        let item=d["items"][0].clone();assert!(!prepare_bundle::EvidenceContext::new(&d).video_ready(&item).unwrap());
        let body=request(&d,&refs);let result=admit(&mut d,&actor(),&body).unwrap();
        assert_eq!(result["status"],"revalidated");assert!(!prepare_bundle::EvidenceContext::new(&d).video_ready(&item).unwrap());
        assert!(create_approval(&mut d,&actor(),&json!({"proposals":result["newRefs"]})).is_err());
        if kind=="reply_and_close" {
            // Semantic independence cannot replace mandatory speech evidence
            // for a new review of an old model/legacy reply.
            let before=d.clone();
            assert!(review(&mut d,&result["newRefs"]).is_err());
            assert_eq!(d,before);
            assert!(create_approval(&mut d,&actor(),&json!({"proposals":result["newRefs"]})).is_err());
        } else {
            // Non-reply decisions retain their separate semantic dependency policy.
            review(&mut d,&result["newRefs"]).unwrap();
            assert!(create_approval(&mut d,&actor(),&json!({"proposals":result["newRefs"]})).is_ok());
        }
    }
}
#[test]
fn source_rules_revision_route_head_and_authority_changes_invalidate_frozen_preview_atomically(){
    let (d,refs)=fixture("reply_and_close",false,false);let body=request(&d,&refs);
    let baseline=capture(&d,&actor(),&preview_body(&refs)).unwrap();
    assert_eq!(baseline["entries"].as_array().unwrap().len(),1);assert_eq!(baseline["held"],json!([]));
    for change in ["text","revision","item","branch","post","rules","route","head","account","approval","unknown","pending","contract","history"]{
        let mut changed=d.clone();match change {
            "text"=>changed["proposals"][0]["text"]=json!("Changed text"),
            "revision"=>changed["proposals"][0]["revision"]=json!(2),
            "item"=>changed["items"][0]["revision"]=json!(99),
            "branch"=>changed["branches"][0]["messages"][0]["text"]=json!("Meaningfully different addressed source"),
            "post"=>changed["posts"][0]["text"]=json!("Changed post"),
            "rules"=>{knowledge::save_instruction(&mut changed,&json!({"requestId":"changed-rule","title":"Current voice","text":"Different exact company rule"}),&now()).unwrap();},
            "route"=>changed["proposals"][0]["routeTarget"]["objectId"]=json!("foreign-object"),
            "head"=>{let mut p=changed["proposals"][0].clone();p["id"]=json!("new-head");list_mut(&mut changed,"proposals").push(p);},
            "account"=>{changed["account"]=json!("BAW Russia");changed["connectorBinding"]=accounts::Profile::BawRussia.binding();},
            "approval"=>changed["approvals"]=json!([{"id":"active","status":"approved","proposals":[{"id":refs[0]["id"],"revision":1}]}]),
            "unknown"=>changed["operations"]=json!([{"id":"alias-unknown","proposalId":"old","itemId":"alias-local","status":"unknown","target":changed["items"][0]}]),
            "pending"=>changed["jobs"]=json!([{"id":"pending-paid-review","kind":"editorial_review","status":"running","editorialReferences":refs}]),
            "contract"=>changed["proposals"][0]["decisionMediaContract"]=json!("unknown-contract"),
            _=>changed["proposals"][0]["history"]=json!({"malformed":true}),
        }
        if change=="rules"{
            let fresh=capture(&changed,&actor(),&preview_body(&refs)).unwrap();
            let original=capture(&d,&actor(),&preview_body(&refs)).unwrap();
            assert_eq!(fresh["entries"].as_array().unwrap().len(),1,"Current rule case must exercise an eligible candidate");
            assert_eq!(fresh["held"],json!([]));assert_ne!(fresh["previewDigest"],baseline["previewDigest"]);
            assert_ne!(fresh["entries"][0]["candidate"]["rulesDigest"],original["entries"][0]["candidate"]["rulesDigest"],"The effective current rule is actually selected");
            assert!(fresh["entries"][0]["evidence"]["materials"].as_array().unwrap().iter().any(|m|m["text"]=="Different exact company rule"));
        }
        let before=changed.clone();assert!(admit(&mut changed,&actor(),&body).is_err(),"{change}");assert_eq!(changed,before,"{change}: atomic refusal");
    }
    let mut other=actor();other.role="operator".into();let mut changed=d.clone();assert!(admit(&mut changed,&other,&body).is_err());assert_eq!(changed,d);
}
#[test]
fn missing_addressed_context_is_independently_held_and_cannot_become_media_independence(){
    let (mut d,refs)=fixture("delete",false,false);d["branches"][0]["messages"]=json!([]);
    let before=d.clone();let preview=capture(&d,&actor(),&preview_body(&refs)).unwrap();
    assert_eq!(preview["entries"],json!([]));assert_eq!(preview["held"].as_array().unwrap().len(),1);
    let result=admit(&mut d,&actor(),&request(&before,&refs)).unwrap();assert_eq!(result["status"],"held");
    assert_eq!(d["proposals"],before["proposals"]);assert_eq!(d["jobs"],before["jobs"]);assert_eq!(d["operations"],before["operations"]);
}
#[test]
fn current_contract_receipts_and_historical_paid_fields_are_not_synthesized_or_rewritten(){
    let (mut d,refs)=fixture("reply_and_close",false,false);
    // A paid projection with missing durable owner is held, not regenerated.
    d["proposals"][0]["prepareRunId"]=json!("missing-paid-owner");d["proposals"][0]["generationMetadata"]=json!({"paidReceipt":"immutable"});
    let before=d.clone();let result=admit(&mut d,&actor(),&request(&before,&refs)).unwrap();assert_eq!(result["status"],"held");assert_eq!(d["proposals"],before["proposals"]);
    let (mut d,refs)=fixture("reply_and_close",false,false);
    let prior=json!({"receiptSha256":"historical-receipt","candidate":{"proposalRevision":1},"decision":"accept"});
    d["proposals"][0]["editorialReview"]=prior.clone();d["proposals"][0]["editorialReviews"]=json!([prior]);
    let before=d.clone();let result=admit(&mut d,&actor(),&request(&before,&refs)).unwrap();assert_preserved(&before,&d);
    assert_eq!(d["proposals"][0]["editorialReview"],before["proposals"][0]["editorialReview"]);
    assert!(require_current(&prepare_bundle::EvidenceContext::new(&d),&d["proposals"][0]).is_err());assert_eq!(result["newRefs"][0]["revision"],2);
}
#[test]
fn replay_never_migrates_twice_or_rechecks_historical_outcome_into_new_authority(){
    let (mut d,refs)=fixture("reply_and_close",false,false);let body=request(&d,&refs);let first=admit(&mut d,&actor(),&body).unwrap();
    d["posts"][0]["text"]=json!("Changed after migration");let before=d.clone();let replay=admit(&mut d,&actor(),&body).unwrap();
    assert_eq!(replay["newRefs"],first["newRefs"]);assert_eq!(replay["replayed"],true);assert_eq!(d,before);
    let mut different=body.clone();different["previewDigest"]=json!("different");assert!(admit(&mut d,&actor(),&different).is_err());assert_eq!(d,before);
}
#[test]
fn strict_owner_media_floor_survives_contract_adoption_and_independence_claim(){
    let (mut d,refs)=fixture("delete",false,true);let source=media_fullframes::source_version(&d["posts"][0],d["account"].as_str().unwrap());
    d["settings"]["postMediaPolicies"]=json!({"post":{"version":1,"revision":1,"status":"active","postId":"post","mode":"full_audio_visual",
        "account":d["account"],"connectorBinding":d["connectorBinding"],"sourceVersion":source}});
    let body=request(&d,&refs);let result=admit(&mut d,&actor(),&body).unwrap();assert_eq!(result["status"],"revalidated");
    let before=d.clone();assert!(review(&mut d,&result["newRefs"]).is_err());assert_eq!(d,before);
    assert!(create_approval(&mut d,&actor(),&json!({"proposals":result["newRefs"]})).is_err());
}
#[test]
fn legitimate_paid_reply_keeps_exact_owner_bundle_text_and_generation_history(){
    let (mut d,refs)=fixture("reply_and_close",false,false);
    let bundle=engine_prepare::build_request(&d,&[json!("i0")],None).unwrap();
    d["jobs"]=json!([{"id":"saved-paid-owner","kind":"assistant","purpose":"auto_prepare","status":"completed",
        "selectedItemIds":["i0"],"prepareBundle":bundle,"result":{"historicalPaidText":"Exact paid draft text 🙂"}}]);
    d["jobs"][0]["scopeReservation"]=preparation_reservations::capture(&d,"saved-paid-owner").unwrap();
    let p=&mut d["proposals"][0];p["prepareRunId"]=json!("saved-paid-owner");p["prepareBundleId"]=bundle["id"].clone();
    p["prepareBundleDigest"]=bundle["digest"].clone();p["generationMetadata"]=json!({"paidReceipt":"immutable-existing-metadata"});
    let before=d.clone();let body=request(&d,&refs);let result=admit(&mut d,&actor(),&body).unwrap();assert_eq!(result["status"],"revalidated");
    assert_preserved(&before,&d);review(&mut d,&result["newRefs"]).unwrap();
    assert!(create_approval(&mut d,&actor(),&json!({"proposals":result["newRefs"]})).is_ok());
    assert_eq!(d["jobs"][0],before["jobs"][0]);assert_eq!(d["proposals"][0]["prepareBundleDigest"],before["proposals"][0]["prepareBundleDigest"]);
}
#[test]
fn one_missing_context_is_held_while_other_exact_draft_revalidates_in_the_same_batch(){
    let (mut d,first)=fixture("reply_and_close",false,false);
    let mut item=d["items"][0].clone();item["id"]=json!("i1");item["itemId"]=json!("c1");item["targetId"]=json!("missing-target");
    item["branchId"]=json!("missing-branch");item["conversationKey"]=json!("other-thread");item["revision"]=json!(1);item["workflow"]=json!("attention");
    list_mut(&mut d,"items").push(item);list_mut(&mut d,"branches").push(json!({"id":"missing-branch","postId":"post","messages":[]}));
    let second=create_proposal(&mut d,&json!({"itemId":"i1","expectedRevision":1,"kind":"delete","text":""})).unwrap();
    let refs=json!([first[0],{"id":second["id"],"revision":second["revision"]}]);let before=d.clone();let body=request(&d,&refs);
    let result=admit(&mut d,&actor(),&body).unwrap();assert_eq!(result["newRefs"].as_array().unwrap().len(),1);assert_eq!(result["held"].as_array().unwrap().len(),1);
    assert_eq!(d["proposals"][1],before["proposals"][1]);assert_eq!(d["proposals"][0]["revision"],2);assert_eq!(d["operations"],before["operations"]);
}
#[test]
fn default_photo_requirement_is_not_waived_by_video_dependency_independence(){
    let (mut d,refs)=fixture("close",false,false);
    d["posts"][0]["attachments"]=json!([{"type":"photo","url":"https://example.invalid/unseen.jpg"}]);
    let body=request(&d,&refs);let result=admit(&mut d,&actor(),&body).unwrap();assert_eq!(result["status"],"revalidated");
    let media=media_context_gate::inspect(&prepare_bundle::EvidenceContext::new(&d),&d["proposals"][0]).unwrap();
    assert_eq!(media["status"],"missing");
    assert!(media["requirements"].as_array().unwrap().iter().any(|r|r["kind"]=="image"&&r["ready"]==false),"An applicable CLOSE really requires the unseen photo");
    let before=d.clone();assert!(review(&mut d,&result["newRefs"]).is_err());assert_eq!(d,before,"No receipt can admit an unseen required photo");
    assert!(create_approval(&mut d,&actor(),&json!({"proposals":result["newRefs"]})).is_err());
}
#[test]
fn corrupt_missing_or_broadened_owner_close_authority_is_never_rebound(){
    let (d,_)=fixture("close",true,false);
    for change in ["hash","reason","selectedBy","route","priorOrigin","missing","preservedUnknown","revisionJump"]{
        let mut changed=d.clone();match change {
            "hash"=>changed["proposals"][0]["operatorCloseDecision"]["decisionSha256"]=json!("forged"),
            "reason"=>changed["proposals"][0]["operatorCloseDecision"]["reason"]=json!("Changed decision"),
            "selectedBy"=>changed["proposals"][0]["operatorCloseDecision"]["selectedBy"]["id"]=json!("another-owner"),
            "route"=>changed["proposals"][0]["operatorCloseDecision"]["routeTarget"]["objectId"]=json!("retargeted"),
            "priorOrigin"=>changed["proposals"][0]["priorPreparationOrigin"]=json!({"prepareRunId":"different-paid-owner"}),
            "missing"=>{changed["proposals"][0].as_object_mut().unwrap().remove("operatorCloseDecision");},
            "preservedUnknown"=>changed["proposals"][0]["operatorCloseDecision"]["preservedUnknownReplies"]=json!({"operations":[{"operationId":"unknown"}]}),
            _=>{},
        }
        let before=changed.clone();let next=if change=="revisionJump"{3}else{2};
        assert!(operator_close::rebind_existing_for_revalidation(&changed,&changed["proposals"][0],&changed["items"][0],&actor(),next).is_err(),"{change}");
        assert_eq!(changed,before);
    }
    let mut remote=actor();remote.id="different-owner".into();remote.authority_generation=Some("a".repeat(64));
    assert!(operator_close::rebind_existing_for_revalidation(&d,&d["proposals"][0],&d["items"][0],&remote,2).is_err());
    let (ordinary,_)=fixture("close",false,false);assert!(operator_close::rebind_existing_for_revalidation(&ordinary,&ordinary["proposals"][0],&ordinary["items"][0],&actor(),2).is_err());
}
#[test]
fn earlier_approved_manual_recipient_action_blocks_revalidation_of_newest_draft(){
    let (mut d,refs)=fixture("reply_and_close",false,false);let mut earlier=d["proposals"][0].clone();
    earlier["id"]=json!("earlier-approved-manual");earlier["status"]=json!("approved");
    d["proposals"]=json!([earlier,d["proposals"][0]]);
    d["approvals"]=json!([{"id":"earlier-approved-action","status":"approved","proposals":[{"id":"earlier-approved-manual","revision":1,"proposal":earlier,"item":d["items"][0]}]}]);
    let before=d.clone();let preview=capture(&d,&actor(),&preview_body(&refs)).unwrap();assert_eq!(preview["entries"],json!([]));
    assert_eq!(preview["held"][0]["reason"],"Recipient has a competing approved or dispatched proposal");
    let result=admit(&mut d,&actor(),&request(&before,&refs)).unwrap();assert_eq!(result["status"],"held");assert_eq!(d["proposals"],before["proposals"]);assert_eq!(d["approvals"],before["approvals"]);
    // Immutable active approval remains blocking even if its proposal status is
    // inconsistent; revalidation does not repair or retarget that old authority.
    d["proposals"][0]["status"]=json!("draft");let before=d.clone();let mut next=request(&before,&refs);
    assert!(admit(&mut d,&actor(),&next).is_err(),"A changed held payload cannot reuse the committed requestId");assert_eq!(d,before);
    next["requestId"]=json!("offline-revalidate-active-approval");
    let preview=capture(&before,&actor(),&preview_body(&refs)).unwrap();assert_eq!(preview["entries"],json!([]));assert_eq!(preview["held"][0]["reason"],"Legacy recipient belongs to an active approval");
    let result=admit(&mut d,&actor(),&next).unwrap();
    assert_eq!(result["status"],"held");assert_eq!(d["proposals"],before["proposals"]);assert_eq!(d["approvals"],before["approvals"]);
}
#[test]
fn queued_semantic_review_of_older_same_recipient_draft_is_not_taken_over(){
    let (mut d,refs)=fixture("reply_and_close",false,false);let mut older=d["proposals"][0].clone();older["id"]=json!("older-manual-draft");
    d["proposals"]=json!([older,d["proposals"][0]]);
    d["jobs"]=json!([{"id":"queued-paid-review","kind":"editorial_review","status":"queued","editorialReferences":[{"id":"older-manual-draft","revision":1}]}]);
    let before=d.clone();let result=admit(&mut d,&actor(),&request(&before,&refs)).unwrap();assert_eq!(result["status"],"held");
    assert_eq!(d["proposals"],before["proposals"]);assert_eq!(d["jobs"],before["jobs"]);
}

// Connected handler/transaction coverage; synthetic Extension is not HTTP auth acceptance.
#[tokio::test]
#[ignore="requires a pristine explicitly isolated communityhero_writer_v51_test_ PostgreSQL fixture"]
async fn postgres_legacy_revalidation_native_handlers_transaction_and_replay() {
    let (mut baseline, paid_refs) = fixture("reply_and_close", false, false);
    let mut negative_refs = Vec::new();
    // Separate exact recipients/branches prevent one hostile case masking another.
    for n in 1..=2 {
        let mut item = baseline["items"][0].clone();
        item["revision"] = json!(1); item["workflow"] = json!("attention");
        item["id"] = json!(format!("i{n}")); item["itemId"] = json!(format!("c{n}"));
        item["conversationKey"] = json!(format!("thread{n}"));
        item["branchId"] = json!(format!("branch{n}"));
        item["postId"] = json!(format!("post{n}")); item["postKey"] = json!(format!("post{n}"));
        item["targetId"] = json!(format!("addressed-comment-{n}"));
        item["contextEvidenceDigest"] = json!(format!("context-{n}"));
        item["branchContextDigest"] = json!(format!("branch-digest-{n}"));
        let mut post = baseline["posts"][0].clone();
        post["id"] = item["postId"].clone(); post["postKey"] = item["postKey"].clone();
        let mut branch = baseline["branches"][0].clone();
        branch["id"] = item["branchId"].clone(); branch["postId"] = item["postId"].clone();
        branch["messages"][0]["id"] = item["targetId"].clone();
        list_mut(&mut baseline, "items").push(item.clone());
        list_mut(&mut baseline, "posts").push(post);
        list_mut(&mut baseline, "branches").push(branch);
        let p = create_proposal(&mut baseline, &json!({"itemId":item["id"],"expectedRevision":1,
            "kind":"reply_and_close","text":"Preserve this exact legacy reply"})).unwrap();
        row_mut(&mut baseline,"proposals",p["id"].as_str().unwrap()).unwrap()
            .as_object_mut().unwrap().remove("decisionMediaContract");
        row_mut(&mut baseline,"proposals",p["id"].as_str().unwrap()).unwrap()
            .as_object_mut().unwrap().remove("nativeCreationOrigin");
        negative_refs.push(json!([{"id":p["id"],"revision":p["revision"]}]));
    }
    // Nonempty native paid provenance: the endpoint may neither regenerate nor erase it.
    let bundle = engine_prepare::build_request(&baseline, &[json!("i0")], None).unwrap();
    baseline["jobs"] = json!([{"id":"saved-paid-owner","kind":"assistant","purpose":"auto_prepare",
        "status":"completed","selectedItemIds":["i0"],"prepareBundle":bundle,
        "result":{"historicalPaidText":"Exact paid draft text 🙂"}}]);
    baseline["jobs"][0]["scopeReservation"] = preparation_reservations::capture(&baseline,"saved-paid-owner").unwrap();
    let p = &mut baseline["proposals"][0];
    p["prepareRunId"] = json!("saved-paid-owner"); p["prepareBundleId"] = bundle["id"].clone();
    p["prepareBundleDigest"] = bundle["digest"].clone();
    p["generationMetadata"] = json!({"paidReceipt":"immutable-existing-metadata"});

    let db = crate::storage::writer_v51_fixture_db().await;
    let (mut app, _temp) = crate::tests::test_app().await;
    app.db.close().await; app.db = db;
    app.db.change(|d| { *d = baseline; Ok(()) }).await.unwrap();
    crate::runtime_lifecycle_app::initialize_app_fixture(&app).await.unwrap();
    let owner = actor();
    let post_body = |key: &str, refs: &Value, preview: &Value| json!({"requestId":key,
        "contract":CONTRACT,"admissionMode":"partial","proposals":refs,"previewDigest":preview["previewDigest"]});
    let before = app.db.read().await.unwrap();
    let paid_preview = super::preview(State(app.clone()),axum::Extension(owner.clone()),Json(preview_body(&paid_refs))).await.unwrap().0;
    assert_eq!(paid_preview["entries"].as_array().unwrap().len(),1,"success must reach adoption, not HOLD");
    assert_eq!(paid_preview["held"],json!([]));
    assert_eq!(app.db.read().await.unwrap(),before,"native preview is read-only");
    let paid_body = post_body("pg-revalidation-paid",&paid_refs,&paid_preview);

    // The real full writer persists proposals before audit. A nontransactional sequence
    // proves we reached the late SQL fault, rather than an earlier domain refusal.
    let Database::Postgres { writer, .. } = &app.db else { unreachable!() };
    sqlx::query("CREATE SEQUENCE communityhero.revalidation_late_fault_seen").execute(writer).await.unwrap();
    sqlx::query("CREATE FUNCTION pg_temp.revalidation_reject_audit() RETURNS trigger LANGUAGE plpgsql AS 'BEGIN PERFORM nextval(''communityhero.revalidation_late_fault_seen''); RAISE EXCEPTION ''synthetic late revalidation audit failure''; END;'")
        .execute(writer).await.unwrap();
    sqlx::query("CREATE TRIGGER revalidation_reject_audit BEFORE INSERT ON communityhero.audit FOR EACH ROW EXECUTE FUNCTION pg_temp.revalidation_reject_audit()")
        .execute(writer).await.unwrap();
    assert!(super::post(State(app.clone()),axum::Extension(owner.clone()),Json(paid_body.clone())).await.is_err());
    let called: bool = sqlx::query_scalar("SELECT is_called FROM communityhero.revalidation_late_fault_seen").fetch_one(writer).await.unwrap();
    let visits: i64 = sqlx::query_scalar("SELECT last_value FROM communityhero.revalidation_late_fault_seen").fetch_one(writer).await.unwrap();
    assert!(called,"late audit INSERT must actually execute"); assert_eq!(visits,1);
    assert_eq!(app.db.read().await.unwrap(),before,"failed SQL transaction rolls back proposal, history, audit and admission receipt");
    sqlx::query("DROP TRIGGER revalidation_reject_audit ON communityhero.audit").execute(writer).await.unwrap();

    let result = super::post(State(app.clone()),axum::Extension(owner.clone()),Json(paid_body.clone())).await.unwrap().0;
    assert_eq!(result["status"],"revalidated"); assert_eq!(result["oldRefs"],paid_refs);
    assert_eq!(result["newRefs"],json!([{"id":paid_refs[0]["id"],"revision":2}]));
    assert_eq!(result["approvalRequired"],true); assert_eq!(result["editorialReviewRequired"],true);
    assert_eq!(result["retryAllowed"],false);
    let adopted = app.db.read().await.unwrap();
    assert_preserved(&before,&adopted);
    assert_eq!(adopted["proposals"].as_array().unwrap().len(),3,"same IDs, no replacement drafts");
    assert_eq!(&adopted["proposals"].as_array().unwrap()[1..],&before["proposals"].as_array().unwrap()[1..]);
    let p = &adopted["proposals"][0];
    assert_eq!(p["id"],before["proposals"][0]["id"]); assert_eq!(p["revision"],2);
    assert_eq!(p["decisionMediaContract"],decision_media::CONTRACT);
    assert_eq!(p["history"][0],historical(&before["proposals"][0]));
    assert_eq!(p["proposalRevalidation"]["adoptedBy"],owner.public_json());
    for key in ["text","prepareRunId","prepareBundleId","prepareBundleDigest","generationMetadata"] {
        assert_eq!(p[key],before["proposals"][0][key],"paid provenance {key}");
    }
    assert_eq!(adopted["jobs"],before["jobs"]); assert_eq!(adopted["operations"],json!([]));
    assert_eq!(adopted["approvals"],json!([]));
    let replay = super::post(State(app.clone()),axum::Extension(owner.clone()),Json(paid_body)).await.unwrap().0;
    assert_eq!(replay["replayed"],true); assert_eq!(replay["newRefs"],result["newRefs"]);
    assert_eq!(app.db.read().await.unwrap(),adopted,"durable endpoint replay adds no revision, receipt or audit");
    let denial = crate::approval_new(State(app.clone()),axum::Extension(owner.clone()),
        Json(json!({"requestId":"pg-revalidation-no-semantics","proposals":result["newRefs"]}))).await.unwrap_err();
    assert_eq!(denial.0,StatusCode::CONFLICT); assert_eq!(denial.1,"Exact final decision requires editorial review");
    assert_eq!(app.db.read().await.unwrap(),adopted,"contract adoption alone cannot create a new approval");

    let drift_refs = &negative_refs[1];
    let drift_preview = super::preview(State(app.clone()),axum::Extension(owner.clone()),Json(preview_body(drift_refs))).await.unwrap().0;
    assert_eq!(drift_preview["entries"].as_array().unwrap().len(),1);
    let drift_body = post_body("pg-revalidation-source-drift",drift_refs,&drift_preview);
    app.change(|d| { d["branches"][2]["messages"][0]["text"] = json!("Hostile changed addressed source after native preview"); Ok(()) }).await.unwrap();
    let drifted = app.db.read().await.unwrap();
    let refreshed = super::preview(State(app.clone()),axum::Extension(owner.clone()),Json(preview_body(drift_refs))).await.unwrap().0;
    assert_ne!(refreshed["previewDigest"],drift_preview["previewDigest"],"meaningful source mutation must invalidate the preview");
    let denial = super::post(State(app.clone()),axum::Extension(owner.clone()),Json(drift_body)).await.unwrap_err();
    assert_eq!(denial.0,StatusCode::CONFLICT); assert_eq!(denial.1,"Legacy revalidation preview changed; review current evidence again");
    assert_eq!(app.db.read().await.unwrap(),drifted,"source drift refusal leaves the complete persisted ledger unchanged");

    let unknown_refs = &negative_refs[0];
    let unknown_preview = super::preview(State(app.clone()),axum::Extension(owner.clone()),Json(preview_body(unknown_refs))).await.unwrap().0;
    assert_eq!(unknown_preview["entries"].as_array().unwrap().len(),1);
    let unknown_body = post_body("pg-revalidation-unknown",unknown_refs,&unknown_preview);
    app.change(|d| { let target = d["items"][1].clone(); list_mut(d,"operations").push(json!({
        "id":"pg-revalidation-unknown-operation","proposalId":unknown_refs[0]["id"],"itemId":"i1",
        "status":"unknown","target":target,"evidence":{"synthetic":true,"requiresReadback":true}})); Ok(()) }).await.unwrap();
    let uncertain = app.db.read().await.unwrap();
    let held = super::preview(State(app.clone()),axum::Extension(owner.clone()),Json(preview_body(unknown_refs))).await.unwrap().0;
    assert_eq!(held["entries"],json!([])); assert_eq!(held["held"].as_array().unwrap().len(),1);
    assert_eq!(held["held"][0]["reason"],"Legacy proposal or recipient has an admitted operation");
    let denial = super::post(State(app.clone()),axum::Extension(owner),Json(unknown_body)).await.unwrap_err();
    assert_eq!(denial.0,StatusCode::CONFLICT); assert_eq!(denial.1,"Legacy revalidation preview changed; review current evidence again");
    let final_state = app.db.read().await.unwrap();
    assert_eq!(final_state,uncertain,"UNKNOWN is preserved and cannot produce a new revision, operation, job, approval or retry");
    assert_eq!(final_state["jobs"],before["jobs"],"no paid regeneration on any path");
    assert_eq!(final_state["operations"][0]["status"],"unknown");
    app.db.close().await;
}
