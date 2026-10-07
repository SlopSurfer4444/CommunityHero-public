//! Connected native reducer/handler tests. Synthetic fixtures only; no provider,
//! model or media acquisition. ROOT owns execution and aggregate acceptance.
use super::*;

fn actor()->operator_auth::Actor {operator_editorial::tests::actor()}
fn fixture(count:usize)->(Value,Value) {
    let (mut d,first)=operator_editorial::tests::fixture();
    let base_item=d["items"][0].clone();let base_post=d["posts"][0].clone();let base_branch=d["branches"][0].clone();
    let mut refs=vec![first[0].clone()];
    for n in 1..count {
        let item_id=format!("i{n}");let post_id=format!("post{n}");let branch_id=format!("branch{n}");
        let mut item=base_item.clone();item["id"]=json!(item_id);item["itemId"]=json!(format!("c{n}"));
        item["conversationKey"]=json!(format!("thread{n}"));item["postId"]=json!(post_id);item["postKey"]=json!(post_id);
        item["branchId"]=json!(branch_id);item["revision"]=json!(1);item["workflow"]=json!("attention");
        let mut post=base_post.clone();post["id"]=json!(post_id);post["postKey"]=json!(post_id);
        post["connectorBinding"]=item["connectorBinding"].clone();
        let mut branch=base_branch.clone();branch["id"]=json!(branch_id);branch["postId"]=json!(post_id);
        list_mut(&mut d,"items").push(item);list_mut(&mut d,"posts").push(post);list_mut(&mut d,"branches").push(branch);
        let p=create_proposal(&mut d,&json!({"itemId":item_id,"expectedRevision":1,"kind":"reply_and_close","text":"Тоже интересный вариант 🙂"})).unwrap();
        refs.push(json!({"id":p["id"],"revision":p["revision"]}));
    }
    (d,json!(refs))
}
fn review_request(preview:&Value)->Value {
    json!({"requestId":"offline-frontier-exact-review","proposals":preview["proposals"],"previewDigest":preview["previewDigest"],
        "operatorReview":{"version":1,"method":operator_editorial::METHOD,"entries":preview["entries"].as_array().unwrap().iter().map(|e|
            json!({"candidate":e["candidate"],"checks":{"companyRules":"pass","intent":"pass","factualScope":"pass"},
                "reason":"Synthetic owner delegate reviewed the exact ready subset evidence"})).collect::<Vec<_>>()}})
}
fn assert_partition(result:&Value,refs:&Value) {
    assert_eq!(result["requested"],*refs);
    let mut selected=std::collections::BTreeSet::new();
    for r in result["readyForOperatorReview"].as_array().unwrap(){assert!(selected.insert(r.to_string()));}
    for h in result["held"].as_array().unwrap(){assert!(selected.insert(h["reference"].to_string()));assert!(!h["reason"].as_str().unwrap().is_empty());}
    assert_eq!(selected,refs.as_array().unwrap().iter().map(Value::to_string).collect());
}

#[test]
fn mixed_legacy_media_and_recipient_holds_do_not_poison_exact_ready_preview() {
    let (mut d,refs)=fixture(4);
    d["posts"][0]["sourceUrl"]=json!("https://www.youtube.com/watch?v=offline-frontier");
    d["items"][2]["workflow"]=json!("waiting");let before=d.clone();
    let result=capture(&d,&actor(),&json!({"proposals":refs})).unwrap();assert_partition(&result,&refs);
    let ready=json!([refs[1],refs[3]]);assert_eq!(result["readyForOperatorReview"],ready);
    assert_eq!(result["held"],json!([
        {"reference":refs[0],"stage":"operator_candidate","reason":"Operator review requires complete video evidence"},
        {"reference":refs[2],"stage":"operator_candidate","reason":"Operator review recipient changed or held"}]));
    assert_eq!(result["preview"],operator_editorial::capture(&d,&actor(),&json!({"proposals":ready})).unwrap());
    assert_eq!(d,before,"frontier must preserve all history, receipts, approvals, jobs and operations");
    let request=review_request(&result["preview"]);let mut reviewed=d.clone();
    let admitted=operator_editorial::admit(&mut reviewed,&actor(),&request).unwrap();assert_eq!(admitted["accepted"],ready);
    assert_eq!(reviewed["proposals"][0],d["proposals"][0]);assert_eq!(reviewed["proposals"][2],d["proposals"][2]);
    assert_eq!(reviewed["operations"],d["operations"]);assert_eq!(reviewed["approvals"],d["approvals"]);
}

#[test]
fn all_held_has_no_admissible_preview_and_preserves_exact_hold_order() {
    let (mut d,refs)=fixture(2);d["items"][0]["workflow"]=json!("waiting");
    d["jobs"]=json!([{"id":"paid","kind":"editorial_review","status":"running","editorialReferences":[refs[1]],"capture":{"immutable":"old paid capture"}}]);
    let before=d.clone();let result=capture(&d,&actor(),&json!({"proposals":refs})).unwrap();assert_partition(&result,&refs);
    assert_eq!(result["readyForOperatorReview"],json!([]));assert!(result["preview"].is_null());
    assert_eq!(result["held"][0]["reference"],refs[0]);assert_eq!(result["held"][1]["reference"],refs[1]);assert_eq!(d,before);
}

#[test]
fn malformed_unknown_duplicate_and_stale_refs_remain_global_after_an_earlier_hold() {
    let (mut d,refs)=fixture(2);d["items"][0]["workflow"]=json!("waiting");
    let bad_refs=vec![json!([]),json!([refs[0],{"id":"foreign","revision":1}]),json!([refs[0],refs[0]]),
        json!([refs[0],{"id":refs[1]["id"],"revision":2}]),json!([refs[0],{"id":refs[1]["id"],"revision":"1"}]),
        json!([refs[0],{"id":" ","revision":1}]),json!([refs[0],{"id":refs[1]["id"],"revision":1,"extra":true}]),
        json!([refs[0],{"id":refs[1]["id"]}]),json!([refs[0],{"id":7,"revision":1}]),json!(vec![refs[0].clone();101])];
    for selection in bad_refs {
        let before=d.clone();assert!(capture(&d,&actor(),&json!({"proposals":selection})).is_err(),"{selection}");assert_eq!(d,before);
        if let Some(array)=selection.as_array(){let reversed=json!(array.iter().rev().cloned().collect::<Vec<_>>());assert!(capture(&d,&actor(),&json!({"proposals":reversed})).is_err());}
    }
    let expected=d["items"][1]["revision"].clone();
    let p=create_proposal(&mut d,&json!({"itemId":"i1","expectedRevision":expected,"kind":"reply_and_close","text":"Different exact reply"})).unwrap();
    assert!(capture(&d,&actor(),&json!({"proposals":[refs[0],refs[1],{"id":p["id"],"revision":p["revision"]}]})).is_err());
    let mut other=actor();other.role="operator".into();assert!(capture(&d,&other,&json!({"proposals":refs})).is_err());
    assert!(capture(&d,&actor(),&json!({"proposals":refs,"skipHold":true})).is_err());
    d["connectorBinding"]=json!({});assert!(capture(&d,&actor(),&json!({"proposals":refs})).is_err());
}

#[test]
fn missing_or_null_raw_binding_is_a_global_error_even_when_all_members_are_held() {
    let (mut d,refs)=fixture(2);d["items"][0]["workflow"]=json!("waiting");d["items"][1]["workflow"]=json!("waiting");
    for missing in [false,true] {
        let mut legacy=d.clone();
        if missing {legacy.as_object_mut().unwrap().remove("connectorBinding");}else{legacy["connectorBinding"]=Value::Null;}
        if missing {assert!(active_binding(&legacy).is_ok(),"fixture must actually exercise the legacy fallback");}
        let before=legacy.clone();let error=capture(&legacy,&actor(),&json!({"proposals":refs})).unwrap_err();
        assert_eq!(error.0,StatusCode::CONFLICT);assert_eq!(error.1,"Operator review frontier requires an explicit valid company binding");assert_eq!(legacy,before);
    }
    let before=d.clone();let result=capture(&d,&actor(),&json!({"proposals":refs})).unwrap();
    assert!(result["preview"].is_null());assert_eq!(result["connectorBinding"],d["connectorBinding"]);assert_eq!(d,before);
}

#[test]
fn native_operation_alias_and_paid_pending_holds_remain_local_and_immutable() {
    let (d,refs)=fixture(2);
    for state in ["unknown","dispatching","succeeded","running","queued","plan"] {
        let mut next=d.clone();
        if ["unknown","dispatching","succeeded"].contains(&state){next["operations"]=json!([{"id":"old-alias-op","itemId":"another-canonical-alias","status":state,"target":next["items"][0]}]);}
        else if state=="plan"{next["jobs"]=json!([{"id":"old-paid-plan","kind":"editorial_review","status":"running","editorialPlan":{"batches":[{"request":{"editorialCandidates":[{"itemId":"i0"}]}}]}}]);}
        else {next["jobs"]=json!([{"id":"old-paid","kind":"editorial_review","status":state,"editorialReferences":[{"id":refs[0]["id"],"revision":999}]}]);}
        let before=next.clone();let result=capture(&next,&actor(),&json!({"proposals":refs})).unwrap();assert_partition(&result,&refs);
        assert_eq!(result["readyForOperatorReview"],json!([refs[1]]));assert_eq!(result["held"][0]["reference"],refs[0]);
        let reason=if ["unknown","dispatching","succeeded"].contains(&state){"Editorial recipient has an admitted operation"}else{"Recover the existing editorial review before operator-assisted review"};
        assert_eq!(result["held"][0]["reason"],reason,"{state}");assert_eq!(next,before,"{state}");
    }
    for terminal in ["completed","failed"] {
        let mut next=d.clone();next["jobs"]=json!([{"id":"terminal-paid","kind":"editorial_review","status":terminal,"editorialReferences":refs,"capture":{"immutable":"paid journal"}}]);
        let before=next.clone();assert_eq!(capture(&next,&actor(),&json!({"proposals":refs})).unwrap()["readyForOperatorReview"],refs);assert_eq!(next,before);
    }
}

#[test]
fn current_route_and_typed_rule_errors_hold_only_the_affected_exact_members() {
    let (mut d,refs)=fixture(3);
    d["proposals"][0]["routeTarget"]["objectId"]=json!("different-provider-object");
    // Native capture selects rules at the current clock; the fixture must already be effective.
    let effective_at=(chrono::Utc::now()-chrono::Duration::minutes(5)).to_rfc3339();
    let saved=knowledge::reply_url_policy::save(&mut d,&json!({"requestId":"frontier-no-links","expectedVersionId":null,"values":[]}),&effective_at).unwrap();
    let current=knowledge::reply_url_policy::read(&d,&now()).unwrap();
    assert_eq!(current["configured"],true);assert_eq!(current["source"],"typed");
    assert_eq!(current["currentVersionId"],saved["savedVersionId"]);assert_eq!(current["values"],json!([]));
    d["proposals"][1]["text"]=json!("Подробнее https://forbidden.example/");let before=d.clone();
    let result=capture(&d,&actor(),&json!({"proposals":refs})).unwrap();assert_partition(&result,&refs);
    assert_eq!(result["readyForOperatorReview"],json!([refs[2]]));
    assert_eq!(result["held"][0]["reason"],"Operator review action route changed");
    assert_eq!(result["held"][1]["reason"],"Reply URL is not allowed by current policy");assert_eq!(d,before);
}

#[test]
fn ready_subset_revalidates_revision_source_route_and_owner_before_admission() {
    let (d,refs)=fixture(2);let result=capture(&d,&actor(),&json!({"proposals":refs})).unwrap();let request=review_request(&result["preview"]);
    for change in ["proposal_revision","source","recipient_revision","route","rules"] {
        let mut changed=d.clone();match change {
            "proposal_revision"=>changed["proposals"][0]["revision"]=json!(2),
            "source"=>changed["posts"][0]["text"]=json!("Changed after the coherent frontier capture"),
            "recipient_revision"=>changed["items"][0]["revision"]=json!(3),
            "route"=>changed["proposals"][0]["routeTarget"]["objectId"]=json!("different-object"),
            _=>{
                let effective_at=(chrono::Utc::now()-chrono::Duration::minutes(5)).to_rfc3339();
                let saved=knowledge::save_instruction(&mut changed,&json!({"requestId":"changed-rule","title":"Voice","text":"Use changed current company voice"}),&effective_at).unwrap();
                let current=operator_editorial::capture(&changed,&actor(),&json!({"proposals":refs})).unwrap();
                assert!(current["entries"][0]["evidence"]["knowledgeManifest"].as_array().unwrap().iter()
                    .any(|rule|rule["versionId"]==saved["version"]["id"]),"changed rule must be selected by native capture");
                assert_ne!(current["entries"][0]["candidate"]["rulesDigest"],result["preview"]["entries"][0]["candidate"]["rulesDigest"]);
                assert_ne!(current["previewDigest"],request["previewDigest"]);
            },
        }
        let before=changed.clone();assert!(operator_editorial::admit(&mut changed,&actor(),&request).is_err(),"{change}");assert_eq!(changed,before);
    }
    let mut changed=d.clone();let mut other=actor();other.id="different-owner".into();
    assert!(operator_editorial::admit(&mut changed,&other,&request).is_err());assert_eq!(changed,d);
}

#[test]
fn shared_context_preview_matches_native_candidates_and_rebuilds_after_source_change() {
    let (mut d,refs)=fixture(3);let context=prepare_bundle::EvidenceContext::new(&d);
    for reference in refs.as_array().unwrap(){let p=row(&d,"proposals",reference["id"].as_str().unwrap()).unwrap();
        assert_eq!(editorial_review::operator_candidate_with_context(&context,p).unwrap(),editorial_review::operator_candidate(&d,p).unwrap());}
    let old=capture(&d,&actor(),&json!({"proposals":refs})).unwrap();
    assert_eq!(old["preview"],operator_editorial::capture(&d,&actor(),&json!({"proposals":refs})).unwrap());
    for (index,entry) in old["preview"]["entries"].as_array().unwrap().iter().enumerate() {
        let materials=&entry["evidence"]["operatorMandatoryMaterials"];
        assert_eq!(materials["companyId"],d["account"]);
        assert_eq!(materials["readiness"]["status"],"ready");
        assert_eq!(materials["members"].as_array().unwrap().len(),1);
        assert_eq!(materials["members"][0]["canonicalPostId"],d["posts"][index]["id"]);
        assert_eq!(materials["members"][0]["connectorBinding"],d["connectorBinding"]);
        assert_eq!(materials["members"][0]["fields"]["text"],d["posts"][index]["text"]);
        assert_eq!(materials["members"][0]["fields"]["attachments"],json!([]));
    }
    d["posts"][1]["text"]=json!("Updated current source evidence");
    let fresh=capture(&d,&actor(),&json!({"proposals":refs})).unwrap();assert_ne!(fresh["preview"]["previewDigest"],old["preview"]["previewDigest"]);
    assert_eq!(fresh["preview"]["entries"][1]["evidence"]["posts"][0]["text"],"Updated current source evidence");
    assert_eq!(fresh["preview"]["entries"][1]["evidence"]["operatorMandatoryMaterials"]["members"][0]["fields"]["text"],
        "Updated current source evidence");
    assert_ne!(fresh["preview"]["entries"][1]["evidence"]["operatorMandatoryMaterials"]["contentSha256"],
        old["preview"]["entries"][1]["evidence"]["operatorMandatoryMaterials"]["contentSha256"]);
}

#[test]
fn unproven_mandatory_materials_and_legacy_origin_hold_only_the_exact_member() {
    let (base,refs)=fixture(2);
    for mutation in ["text_unavailable","attachments_unknown","photo_unacquired","legacy_origin"] {
        let mut d=base.clone();
        match mutation {
            "text_unavailable"=>{d["posts"][0].as_object_mut().unwrap().remove("text");},
            "attachments_unknown"=>d["posts"][0]["attachmentsState"]=json!("unknown"),
            "photo_unacquired"=>d["posts"][0]["attachments"]=json!([{"type":"photo","url":"https://example.invalid/unacquired.png"}]),
            _=>{d["proposals"][0].as_object_mut().unwrap().remove("nativeCreationOrigin");},
        }
        let expected=if mutation=="legacy_origin"{"operator_material_manual_origin_unproven"}else{"mandatory_material_not_ready"};
        let before=d.clone();
        let native_error=operator_editorial::capture(&d,&actor(),&json!({"proposals":[refs[0]]})).unwrap_err();
        assert_eq!(native_error.1,expected,"{mutation}");
        let frontier=capture(&d,&actor(),&json!({"proposals":refs})).unwrap();assert_partition(&frontier,&refs);
        assert_eq!(frontier["readyForOperatorReview"],json!([refs[1]]),"{mutation}");
        assert_eq!(frontier["held"],json!([{"reference":refs[0],"stage":"operator_candidate","reason":expected}]),"{mutation}");
        assert_eq!(frontier["preview"],operator_editorial::capture(&d,&actor(),&json!({"proposals":[refs[1]]})).unwrap());
        assert_eq!(d,before,"{mutation}: a read-only frontier cannot fill evidence or create proof");
    }
}

#[test]
fn current_and_corrupt_receipts_are_neither_invented_nor_rewritten_by_frontier() {
    let (mut d,refs)=fixture(2);let request=operator_editorial::tests::body(&d,&json!([refs[0]]));
    operator_editorial::admit(&mut d,&actor(),&request).unwrap();let receipt=d["proposals"][0]["editorialReview"].clone();
    let before=d.clone();let result=capture(&d,&actor(),&json!({"proposals":refs})).unwrap();assert_eq!(result["readyForOperatorReview"],refs);assert_eq!(d,before);
    assert_eq!(d["proposals"][0]["editorialReview"],receipt);assert!(d["proposals"][1].get("editorialReview").is_none());
    d["proposals"][0]["editorialReview"]["receiptSha256"]=json!("corrupt-historical-receipt");
    let before=d.clone();let result=capture(&d,&actor(),&json!({"proposals":refs})).unwrap();assert_eq!(result["readyForOperatorReview"],refs);assert_eq!(d,before);
}

#[test]
fn byte_limits_hold_one_oversized_member_and_refuse_an_oversized_aggregate_without_truncation() {
    let (mut d,refs)=fixture(2);d["posts"][0]["text"]=json!("x".repeat(operator_editorial::MAX_PREVIEW_BYTES));
    let before=d.clone();let result=capture(&d,&actor(),&json!({"proposals":refs})).unwrap();assert_partition(&result,&refs);
    assert_eq!(result["readyForOperatorReview"],json!([refs[1]]));assert_eq!(result["held"][0]["stage"],"evidence_budget");
    assert_eq!(result["held"][0]["reason"],"Operator review evidence exceeds 8 MiB; select fewer replies");
    assert_eq!(result["preview"],operator_editorial::capture(&d,&actor(),&json!({"proposals":[refs[1]]})).unwrap());assert_eq!(d,before);
    // Exact mandatory material fields are visible in addition to source posts.
    // Each complete singleton fits, but the two complete entries exceed 8 MiB.
    d["posts"][0]["text"]=json!("x".repeat(operator_editorial::MAX_PREVIEW_BYTES/3));
    d["posts"][1]["text"]=json!("y".repeat(operator_editorial::MAX_PREVIEW_BYTES/3));
    for reference in refs.as_array().unwrap(){
        let singleton=capture(&d,&actor(),&json!({"proposals":[reference]})).unwrap();
        assert_eq!(singleton["readyForOperatorReview"],json!([reference]));
        assert_eq!(singleton["held"],json!([]));
        assert_eq!(singleton["preview"],operator_editorial::capture(&d,&actor(),&json!({"proposals":[reference]})).unwrap());
    }
    let before=d.clone();let error=capture(&d,&actor(),&json!({"proposals":refs})).unwrap_err();
    assert_eq!(error.1,"Operator review evidence exceeds 8 MiB; select fewer replies");assert_eq!(d,before);
    let huge=json!({"proposals":[{"id":"x".repeat(2*1024*1024),"revision":1}]});
    let error=capture(&d,&actor(),&huge).unwrap_err();assert_eq!(error.1,"Operator review frontier input exceeds 2 MiB");assert_eq!(d,before);
}

#[test]
fn marked_missing_media_requires_exact_semantic_judgment_and_owner_floor_stays_enforced() {
    let (mut d,refs)=fixture(2);d["posts"][0]["attachments"]=json!([{"type":"video","url":"https://example.invalid/offline-frontier.mp4"}]);
    d["items"][0]["targetId"]=json!("addressed-c0");
    d["branches"][0]["messages"]=json!([{"id":"addressed-c0","parentId":null,"role":"customer","text":"Мечтаю о V8"}]);
    d["proposals"][0]["decisionMediaContract"]=json!(decision_media::CONTRACT);
    let without_speech=d.clone();
    assert!(editorial_review::operator_candidate(&d,&d["proposals"][0]).is_ok(),
        "Semantic review eligibility alone does not prove mandatory speech");
    let held=capture(&d,&actor(),&json!({"proposals":refs})).unwrap();assert_partition(&held,&refs);
    assert_eq!(held["readyForOperatorReview"],json!([refs[1]]));
    assert_eq!(held["held"],json!([{"reference":refs[0],"stage":"operator_candidate","reason":"mandatory_material_not_ready"}]));
    assert_eq!(operator_editorial::capture(&d,&actor(),&json!({"proposals":[refs[0]]})).unwrap_err().1,"mandatory_material_not_ready");
    assert_eq!(d,without_speech,"Neither preview may invent missing speech evidence");
    // Supply independently specified complete synthetic speech through the
    // native catalog selector before testing semantic/owner visual decisions.
    let source=media_fullframes::source_version(&d["posts"][0],d["account"].as_str().unwrap());
    let transcription=json!({"partial":false,"coverage":"full_audio","audioStatus":"transcribed",
        "mediaDurationSeconds":120.0,"audioDurationSeconds":120.0,"sourceVersion":source});
    assert!(knowledge::proven_full_audio(&transcription,&source));
    let material=json!({"id":"frontier-complete-speech","account":d["account"],"postKey":d["posts"][0]["postKey"],
        "kind":"transcript","text":"Complete synthetic speech from this exact video.","transcription":transcription});
    list_mut(&mut d,"materials").push(material);
    knowledge::sync_catalog(&mut d,&(chrono::Utc::now()-chrono::Duration::minutes(5)).to_rfc3339()).unwrap();
    assert_eq!(prepare_bundle::EvidenceContext::new(&d).strict_media_evidence(&d["posts"][0]).unwrap()["audioReady"],true);
    let result=capture(&d,&actor(),&json!({"proposals":refs})).unwrap();assert_eq!(result["readyForOperatorReview"],refs);
    assert_eq!(result["preview"],operator_editorial::capture(&d,&actor(),&json!({"proposals":refs})).unwrap());
    let mut request=review_request(&result["preview"]);let before=d.clone();
    assert!(operator_editorial::admit(&mut d,&actor(),&request).is_err(),"preview eligibility cannot invent missing mediaDependency");assert_eq!(d,before);
    request["operatorReview"]["entries"][0]["mediaDependency"]=json!({"audio":"independent","visual":"independent"});
    let mut reviewed=d.clone();
    assert_eq!(operator_editorial::admit(&mut reviewed,&actor(),&request).unwrap()["accepted"],refs,
        "Complete mandatory speech plus exact independent semantics remains admissible");
    let source=media_fullframes::source_version(&d["posts"][0],d["account"].as_str().unwrap());
    d["settings"]["postMediaPolicies"]=json!({"post":{"version":1,"revision":1,"status":"active","postId":"post","mode":"full_audio_visual",
        "account":d["account"],"connectorBinding":d["connectorBinding"],"sourceVersion":source}});
    let result=capture(&d,&actor(),&json!({"proposals":refs})).unwrap();assert_eq!(result["readyForOperatorReview"],refs);
    let mut fresh=review_request(&result["preview"]);fresh["operatorReview"]["entries"][0]["mediaDependency"]=json!({"audio":"independent","visual":"independent"});
    let before=d.clone();assert!(operator_editorial::admit(&mut d,&actor(),&fresh).is_err(),"owner floor remains a judgment/admission prerequisite");assert_eq!(d,before);
    d["branches"][0]["messages"][0]["textUnavailable"]=json!(true);
    let result=capture(&d,&actor(),&json!({"proposals":refs})).unwrap();assert_eq!(result["readyForOperatorReview"],json!([refs[1]]));
    assert_eq!(result["held"][0]["reason"],"Decision requires unavailable parent or branch context");
}

#[tokio::test]
async fn native_handler_uses_existing_storage_capture_and_never_mutates_history() {
    let folder=tempfile::tempdir().unwrap();let db=Database::Sqlite(crate::open_db(&folder.path().join("frontier.sqlite")).await.unwrap());
    let (mut app,_temp)=crate::tests::test_app().await;app.db.close().await;app.db=db;
    let (mut d,refs)=fixture(2);d["items"][0]["workflow"]=json!("waiting");
    list_mut(&mut d,"jobs").push(json!({"id":"historical","kind":"assistant","status":"completed","result":{"text":"history retained"}}));
    app.db.change(|value|{*value=d;Ok(())}).await.unwrap();let before=app.read().await.unwrap();
    let body=json!({"proposals":refs});let snapshot=app.db.read_operator_editorial(&body).await.unwrap();
    let Json(result)=preview(State(app.clone()),Extension(actor()),Json(body.clone())).await.unwrap();
    assert_eq!(result,capture(&snapshot,&actor(),&body).unwrap());assert_eq!(result["readyForOperatorReview"],json!([refs[1]]));
    assert_eq!(app.read().await.unwrap(),before,"native handler writes no ledger, receipt, approval or history");app.db.close().await;
}

#[tokio::test]
async fn cold_retained_media_warms_before_native_frontier_without_borrowing_donor_ocr() {
    // Real normalized/segment/source CAS artifacts and exact applicability;
    // the Media helper evicts only its own unique proof, never global caches.
    let media=crate::media_analysis_reuse::test_cold_reused_workspace();
    let paid=media["materials"][0].clone();
    let donor=row(&media,"posts","donor").unwrap();let media_target=row(&media,"posts","target").unwrap();
    let donor_version=crate::media_fullframes::source_version(donor,media["account"].as_str().unwrap());
    assert_eq!(paid["transcription"]["ocr"]["sourceVersion"],donor_version,"donor OCR must actually be valid for its donor");
    assert_eq!(paid["transcription"]["ocr"]["coverage"],"sampled_frames");
    assert_ne!(paid["transcription"]["ocr"]["sourceVersion"],crate::media_fullframes::source_version(media_target,media["account"].as_str().unwrap()));
    let donor_evidence=prepare_bundle::EvidenceContext::new(&media).strict_media_evidence(donor).unwrap();
    assert_eq!(donor_evidence["screenTextReady"],true);assert_eq!(donor_evidence["screenTextHasContent"],true);
    let (mut d,refs)=fixture(2);
    let sibling_post=d["posts"][1].clone();
    d["posts"]=media["posts"].clone();list_mut(&mut d,"posts").push(sibling_post);
    for field in ["materials","knowledge_entries","knowledge_versions","jobs"] {d[field]=media[field].clone();}
    let target=row(&d,"posts","target").unwrap().clone();
    d["items"][0]["postId"]=target["id"].clone();d["items"][0]["postKey"]=target["postKey"].clone();
    d["branches"][0]["postId"]=target["id"].clone();
    d["proposals"][0]["routeTarget"]["postKey"]=target["postKey"].clone();
    assert!(!crate::decision_media::enabled(&d["proposals"][0]),"legacy draft was created before video evidence graft; native media gating stays connected");
    // An explicit synthetic owner exception requires visual evidence. This
    // test changes no ordinary default of full audio plus target screen text.
    let source=crate::media_fullframes::source_version(&target,d["account"].as_str().unwrap());
    d["settings"]["postMediaPolicies"]=json!({"target":{"version":1,"revision":1,"status":"active","postId":"target",
        "mode":"full_audio_visual","account":d["account"],"connectorBinding":d["connectorBinding"],"sourceVersion":source}});
    let fingerprint=prepare_bundle::review_fingerprint(&d,"i0").unwrap();d["proposals"][0]["reviewContextDigest"]=json!(fingerprint);
    let cold=prepare_bundle::EvidenceContext::new(&d).strict_media_evidence(&target).unwrap();
    assert_eq!(cold["audioReady"],false,"fixture must start with its retained proof cold");
    assert_eq!(cold["screenTextReady"],false);assert_eq!(cold["visualReady"],false);
    let folder=tempfile::tempdir().unwrap();let db=Database::Sqlite(crate::open_db(&folder.path().join("cold-frontier.sqlite")).await.unwrap());
    let (mut app,_temp)=crate::tests::test_app().await;app.db.close().await;app.db=db;
    app.db.change(|value|{*value=d;Ok(())}).await.unwrap();let before=app.read().await.unwrap();
    let body=json!({"proposals":refs});
    // No test-side warm call: the production frontier handler's awaited cold
    // refresh must run before its actual scoped reader and native capture.
    let Json(result)=preview(State(app.clone()),Extension(actor()),Json(body.clone())).await.unwrap();
    let snapshot=app.db.read_operator_editorial(&body).await.unwrap();
    let target=row(&snapshot,"posts","target").unwrap();
    let strict=prepare_bundle::EvidenceContext::new(&snapshot).strict_media_evidence(target).unwrap();
    assert_eq!(strict["audioReady"],true,"actual reader retains ledger+applicability and warmed exact paid audio");
    assert_eq!(strict["audioHasContent"],true);
    assert_eq!(strict["screenTextReady"],false,"donor OCR must not claim screen coverage for the current target");
    assert_eq!(strict["visualReady"],false,"retained audio supplies no viewed target pixels");
    let evidence=prepare_bundle::EvidenceContext::new(&snapshot).evidence_for_item("i0").unwrap();
    assert!(evidence["materials"].as_array().unwrap().iter().any(|material|
        material["text"]==paid["text"]&&material["postKey"]==paid["postKey"]
        &&material["transcription"]["sourceVersion"]==paid["transcription"]["sourceVersion"]
        &&material["exactFileAnalysisReuse"].as_array().into_iter().flatten().any(|edge|edge["targetPostId"]==target["id"])),
        "native target evidence must supply the original paid audio through its typed exact-file edge");
    assert_partition(&result,&refs);assert_eq!(result["readyForOperatorReview"],json!([refs[1]]));
    assert_eq!(result["held"],json!([{"reference":refs[0],"stage":"operator_candidate","reason":"Operator review requires complete video evidence"}]));
    assert_eq!(result["preview"],operator_editorial::capture(&snapshot,&actor(),&json!({"proposals":[refs[1]]})).unwrap());
    assert_eq!(result,capture(&snapshot,&actor(),&body).unwrap());
    assert_eq!(app.read().await.unwrap(),before,"cold proof readback cannot mutate history, paid source, ledger, receipts, approvals or operations");
    app.db.close().await;
}
