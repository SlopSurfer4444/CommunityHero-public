//! Synthetic reducer tests only: no provider, database, model invocation or media tool.
use super::*;

const AT: &str = "2026-10-02T10:00:00Z";

fn owner() -> crate::operator_auth::Actor {
    crate::operator_auth::Actor::local_owner("decision-media-offline-owner")
}

fn fixture(count: usize) -> Value {
    let mut d = crate::empty();
    crate::accounts::initialize(&mut d, crate::accounts::Profile::LikeAvto).unwrap();
    for name in ["feedback", "knowledge_entries", "knowledge_versions", "materials"] {
        d[name] = json!([]);
    }
    d["items"] = json!((0..count).map(|n| json!({
        "id":format!("i{n}"),"itemId":format!("c{n}"),"targetId":format!("comment-11341-c{n}"),"objectId":"11341","platform":"VK",
        "postKey":"post","conversationKey":format!("thread{n}"),"branchId":"branch","postId":"post",
        "revision":1,"draft":"","workflow":"attention","providerStatus":"new",
        "contextEvidenceDigest":format!("context-{n}"),"branchContextDigest":"branch-digest",
        "text":"Красивый цвет!","connectorBinding":d["connectorBinding"]
    })).collect::<Vec<_>>());
    d["posts"] = json!([{"id":"post","postKey":"post","objectId":"11341","platform":"VK",
        "text":"Новая модель","attachments":[{"type":"video","url":"https://example.invalid/offline-video.mp4"}]}]);
    d["branches"] = json!([{"id":"branch","postId":"post","contextComplete":true,
        "messages":[{"id":"parent","parentId":null,"role":"customer","text":"Как вам цвет?"}]}]);
    for n in 0..count {
        d["branches"][0]["messages"].as_array_mut().unwrap().push(json!({
            "id":format!("comment-11341-c{n}"),"parentId":"parent","role":"customer","text":"Красивый цвет!"
        }));
    }
    d
}

fn add(d: &mut Value, item: &str, kind: &str, text: &str) -> Value {
    let revision = crate::row(d, "items", item).unwrap()["revision"].clone();
    let p = crate::create_proposal(d, &json!({"itemId":item,"expectedRevision":revision,"kind":kind,"text":text})).unwrap();
    assert_eq!(p["nativeCreationOrigin"], "operator_manual_v1", "the native producer must establish manual origin");
    json!({"id":p["id"],"revision":p["revision"]})
}

fn independent() -> Value { json!({"audio":"independent","visual":"independent"}) }

fn complete_source_audio(d: &mut Value) {
    let source = crate::media_fullframes::source_version(&d["posts"][0], d["account"].as_str().unwrap());
    let transcription = json!({"partial":false,"coverage":"full_audio","audioStatus":"transcribed",
        "mediaDurationSeconds":120.0,"audioDurationSeconds":120.0,"sourceVersion":source});
    assert!(crate::knowledge::proven_full_audio(&transcription, &source));
    d["materials"] = json!([{"id":"decision-source-speech","account":d["account"],"postKey":"post",
        "kind":"transcript","text":"Complete synthetic speech from this exact video.","transcription":transcription}]);
    let admitted_at = (chrono::Utc::now() - chrono::Duration::minutes(5)).to_rfc3339();
    crate::knowledge::sync_catalog(d, &admitted_at).unwrap();
    let evidence = EvidenceContext::new(d).strict_media_evidence(&d["posts"][0]).unwrap();
    assert_eq!(evidence["audioReady"], true, "fixture must pass the current catalog selector");
    assert_eq!(evidence["audioHasContent"], true);
}

fn assert_independent_media_approved(d: &Value, reference: &Value) {
    let mut approved = d.clone();
    let gate = crate::media_context_gate::inspect(&EvidenceContext::new(d), &d["proposals"][0]).unwrap();
    assert_eq!(gate["status"], "ready");
    assert_eq!(gate["requirements"][0]["reason"], "exact_review_media_independent");
    assert_eq!(crate::create_approval(&mut approved, &owner(), &json!({"proposals":[reference]})).unwrap()["status"], "approved");
    for key in ["materials", "knowledge_entries", "knowledge_versions", "jobs", "operations"] {
        assert_eq!(approved[key], d[key], "approval must not acquire media or dispatch: {key}");
    }
}

fn verdict(batch: &Value, dependencies: &[Value]) -> Value {
    let candidates = batch["request"]["editorialCandidates"].as_array().unwrap();
    assert_eq!(candidates.len(), dependencies.len());
    let mut metadata = crate::editorial_review::fixture_metadata();
    metadata["model"] = json!(crate::codex_model_policy::MODEL);
    metadata["modelProfile"] = json!(crate::codex_model_policy::PROFILE);
    metadata["reasoningEffort"] = json!("high");
    metadata["cliSha256"] = json!(crate::codex_model_policy::CLI_SHA256);
    metadata["completedAt"] = json!(AT);
    json!({"text":"Synthetic exact semantic review","sources":[],"proposals":[],"runMetadata":metadata,
        "editorial":candidates.iter().zip(dependencies).map(|(c,needs)| json!({
            "proposalId":c["proposalId"],"proposalRevision":c["proposalRevision"],"itemId":c["itemId"],
            "textSha256":c["textSha256"],"contextDigest":c["contextDigest"],"rulesDigest":c["rulesDigest"],
            "decision":"accept","reason":"Exact action is grounded in supplied comment and company rules",
            "proposedText":null,"checks":{"companyRules":"pass","intent":"pass","factualScope":"pass"},
            "mediaDependency":needs
        })).collect::<Vec<_>>()})
}

fn model_accept(d: &mut Value, reference: &Value) {
    let plan = crate::editorial_review::plan_new(d, &json!([reference]), AT).unwrap();
    assert!(plan["held"].as_array().unwrap().is_empty(), "{plan}");
    assert_eq!(plan["batches"].as_array().unwrap().len(), 1);
    let batch = &plan["batches"][0];
    let result = captured_verdict(d, batch, &[independent()]);
    let out = crate::editorial_review::admit(d, batch, &result, AT).unwrap();
    assert_eq!(out["outcomes"][0]["decision"], "accept", "{out}");
}

fn captured_verdict(d: &mut Value, batch: &Value, dependencies: &[Value]) -> Value {
    let mut result = verdict(batch, dependencies);
    crate::editorial_review::fixture_capture_result(d, batch, &mut result).unwrap();
    result
}

fn assert_material_hold(d: &Value, reference: &Value) {
    let before = d.clone();
    let plan = crate::editorial_review::plan_new(d, &json!([reference]), AT).unwrap();
    assert!(plan["batches"].as_array().unwrap().is_empty(), "{plan}");
    assert_eq!(plan["held"].as_array().unwrap().len(), 1, "{plan}");
    assert_eq!(plan["held"][0]["reason"], "mandatory_material_not_ready", "{plan}");
    assert_eq!(*d, before, "planning a missing material must not create paid work or a receipt");
}

fn operator_request(d: &Value, reference: &Value, needs: Value) -> Value {
    let refs = json!([reference]);
    let preview = crate::operator_editorial::capture(d, &owner(), &json!({"proposals":refs})).unwrap();
    json!({"requestId":"offline-exact-media-review","proposals":refs,"previewDigest":preview["previewDigest"],
        "operatorReview":{"version":1,"method":crate::operator_editorial::METHOD,"entries":[{
            "candidate":preview["entries"][0]["candidate"],"mediaDependency":needs,
            "checks":{"companyRules":"pass","intent":"pass","factualScope":"pass"},
            "reason":"Operator delegate reviewed the exact action and supplied current source evidence"
        }]}})
}

fn current(d: &Value, index: usize) -> bool {
    crate::editorial_review::require_current(&EvidenceContext::new(d), &d["proposals"][index]).is_ok()
}

// Regression for a completed duration probe during an explicit operator review.
// The acquisition checkpoint is real schema-shaped synthetic input; it is not
// admitted full-audio/visual decision evidence and performs no media work.
fn acquisition_probe(d:&mut Value,duration:u64,source_sha:&str){
    let post=&d["posts"][0];let binding=crate::active_binding(d).unwrap().to_json();
    let mut progress=crate::media_fullframes::initial(d["account"].as_str().unwrap(),&binding,post,AT);
    progress["source"]=json!({"sha256":source_sha,"bytes":1024});progress["phase"]=json!("inventory");
    progress["sourceIdentity"]=json!({"account":d["account"],"postKey":post["postKey"],"mediaSha256":source_sha,"durationMs":duration});
    let probe=json!({"id":"synthetic-duration-probe","kind":"media","purpose":"auto_media","status":"queued",
        "connectorBinding":binding,"visualContractVersion":2,"account":d["account"],"refId":post["id"],"result":{"visualProgress":progress}});
    let jobs=d["jobs"].as_array_mut().unwrap();
    if let Some(saved)=jobs.iter_mut().find(|j|j["id"]=="synthetic-duration-probe"){*saved=probe;}else{jobs.push(probe);}
    assert_eq!(crate::post_media_policy::effective(d,&d["posts"][0]).unwrap()["decisionBasis"]["sourceSha256"],source_sha);
}
fn operator_batch_request(d:&Value,refs:&Value,needs:&Value)->Value{
    let preview=crate::operator_editorial::capture(d,&owner(),&json!({"proposals":refs})).unwrap();
    json!({"requestId":"synthetic-probe-review","proposals":refs,"previewDigest":preview["previewDigest"],
        "operatorReview":{"version":1,"method":crate::operator_editorial::METHOD,"entries":preview["entries"].as_array().unwrap().iter().map(|entry|
            json!({"candidate":entry["candidate"],"mediaDependency":needs,"checks":{"companyRules":"pass","intent":"pass","factualScope":"pass"},
                "reason":"Synthetic exact decision independent of acquisition observations"})).collect::<Vec<_>>()}})
}
#[test]
fn independent_operator_batch_survives_acquisition_probe_without_false_media_readiness(){
    let mut d=fixture(27);let mut refs=Vec::new();
    for n in 0..27{refs.push(add(&mut d,&format!("i{n}"),"delete",""));}
    let refs=json!(refs);let request=operator_batch_request(&d,&refs,&independent());
    let old=crate::operator_editorial::capture(&d,&owner(),&json!({"proposals":refs})).unwrap();
    acquisition_probe(&mut d,71261,&"a".repeat(64));
    let new=crate::operator_editorial::capture(&d,&owner(),&json!({"proposals":refs})).unwrap();
    assert_ne!(old["entries"][0]["evidence"]["posts"][0]["mediaPolicy"],new["entries"][0]["evidence"]["posts"][0]["mediaPolicy"]);
    assert_eq!(old["entries"][0]["candidate"]["contextDigest"],new["entries"][0]["candidate"]["contextDigest"]);
    assert_ne!(old["entries"][0]["candidate"]["acquisitionMediaDigest"],new["entries"][0]["candidate"]["acquisitionMediaDigest"]);
    assert_eq!(old["previewDigest"],new["previewDigest"]);
    let result=crate::operator_editorial::admit(&mut d,&owner(),&request).unwrap();
    assert_eq!(result["accepted"].as_array().unwrap().len(),27);
    assert!(!EvidenceContext::new(&d).video_ready(&d["items"][0]).unwrap());
    assert!(current(&d,0));d["proposals"][0]["status"]=json!("approved");
    assert!(current(&d,0),"receipt revalidation is pure, not draft admission");
    assert!(d["operations"].as_array().unwrap().is_empty());
    let job=d["jobs"].as_array().unwrap().iter().find(|j|j["purpose"]=="operator_assisted_review").unwrap();
    assert_eq!(job["operatorReviewPreview"]["entries"][0]["evidence"]["posts"][0]["mediaPolicy"],new["entries"][0]["evidence"]["posts"][0]["mediaPolicy"],"full observed evidence is retained");
    assert_eq!(crate::operator_editorial::preview_digest(&job["operatorReviewPreview"]),request["previewDigest"].as_str().unwrap());
}
#[test]
fn required_operator_media_keeps_exact_acquisition_identity_and_atomic_batch(){
    let mut d=fixture(2);complete_source_audio(&mut d);
    let refs=json!([add(&mut d,"i0","reply_and_close","Exact source speech"),add(&mut d,"i1","reply_and_close","Exact second speech")]);
    let needs=json!({"audio":"required","visual":"independent"});
    let request=operator_batch_request(&d,&refs,&needs);
    acquisition_probe(&mut d,71261,&"a".repeat(64));let before=d.clone();
    assert!(crate::operator_editorial::admit(&mut d,&owner(),&request).is_err());assert_eq!(d,before);
    let fresh=operator_batch_request(&d,&refs,&needs);crate::operator_editorial::admit(&mut d,&owner(),&fresh).unwrap();
    assert!(current(&d,0));acquisition_probe(&mut d,71261,&"b".repeat(64));
    assert!(!current(&d,0),"required decision retains acquisition byte identity");
}
#[test]
fn independent_operator_probe_exemption_preserves_source_policy_and_owner_floor(){
    for mutation in ["post","attachment","branch","floor","missing_digest","bad_digest"]{
        let mut d=fixture(1);let reference=add(&mut d,"i0","delete","");let mut request=operator_request(&d,&reference,independent());
        match mutation{
            "post"=>d["posts"][0]["text"]=json!("Meaningful changed source"),
            "attachment"=>d["posts"][0]["attachments"][0]["url"]=json!("https://example.invalid/different-video.mp4"),
            "branch"=>d["branches"][0]["messages"][0]["text"]=json!("Changed parent"),
            "floor"=>{let version=crate::media_fullframes::source_version(&d["posts"][0],d["account"].as_str().unwrap());
                d["settings"]["postMediaPolicies"]=json!({"post":{"version":1,"revision":1,"status":"active","postId":"post","mode":"full_audio_visual",
                    "account":d["account"],"connectorBinding":d["connectorBinding"],"sourceVersion":version}});},
            "missing_digest"=>{request["operatorReview"]["entries"][0]["candidate"].as_object_mut().unwrap().remove("acquisitionMediaDigest");},
            _=>request["operatorReview"]["entries"][0]["candidate"]["acquisitionMediaDigest"]=json!("forged"),
        }
        let before=d.clone();assert!(crate::operator_editorial::admit(&mut d,&owner(),&request).is_err(),"{mutation}");assert_eq!(d,before,"{mutation}: atomic rejection");
    }
}
#[test]
fn historical_operator_receipt_and_model_receipt_keep_strict_old_fingerprint(){
    for historical_operator in [false,true]{
        let mut d=fixture(1);complete_source_audio(&mut d);let reference=add(&mut d,"i0","delete","");
        if historical_operator{
            let plan=crate::editorial_review::plan_new(&d,&json!([reference]),AT).unwrap();
            let candidate=plan["batches"][0]["request"]["editorialCandidates"][0].clone();
            assert!(candidate.get("operatorEvidenceContract").is_none());
            let judgment=json!({"proposalId":candidate["proposalId"],"proposalRevision":candidate["proposalRevision"],"itemId":candidate["itemId"],
                "textSha256":candidate["textSha256"],"contextDigest":candidate["contextDigest"],"rulesDigest":candidate["rulesDigest"],
                "decision":"accept","reason":"Synthetic historical exact operator review","proposedText":null,
                "checks":{"companyRules":"pass","intent":"pass","factualScope":"pass"},"mediaDependency":independent()});
            crate::editorial_review::store_operator_receipt(&mut d,&candidate,&judgment,&json!({"kind":"operator_assisted_review","researchManifest":[]}),AT).unwrap();
        }else{model_accept(&mut d,&reference);}
        assert!(current(&d,0));acquisition_probe(&mut d,71261,&"a".repeat(64));assert!(!current(&d,0));
    }
}
#[test]
fn one_meaningful_stale_recipient_still_rejects_whole_operator_batch_atomically(){
    let mut d=fixture(2);let refs=json!([add(&mut d,"i0","delete",""),add(&mut d,"i1","delete","")]);
    let request=operator_batch_request(&d,&refs,&independent());d["items"][1]["text"]=json!("Changed exact second recipient");
    let before=d.clone();assert!(crate::operator_editorial::admit(&mut d,&owner(),&request).is_err());
    assert_eq!(d,before,"no silently partial batch or receipt writes");
}
#[tokio::test]
async fn versioned_operator_digest_passes_storage_validator_and_replays_atomically(){
    let mut d=fixture(2);let refs=json!([add(&mut d,"i0","delete",""),add(&mut d,"i1","delete","")]);
    let request=operator_batch_request(&d,&refs,&independent());acquisition_probe(&mut d,71261,&"a".repeat(64));
    let folder=tempfile::tempdir().unwrap();let db=crate::Database::Sqlite(crate::open_db(&folder.path().join("probe-review.sqlite")).await.unwrap());
    db.change(|workspace|{*workspace=d;Ok(())}).await.unwrap();let before=db.read().await.unwrap();
    let failed:crate::ApiResult<(Value,bool)>=db.change_admission_observed(crate::storage::AdmissionScope::OperatorEditorial(&request),|workspace|{
        crate::operator_editorial::admit(workspace,&owner(),&request)?;Err(crate::internal("synthetic late projected-review failure"))
    }).await;
    assert!(failed.is_err());assert_eq!(db.read().await.unwrap(),before,"receipt/job/ledger rollback");
    for resign in [false,true] {
        let tampered=db.change_admission_observed(crate::storage::AdmissionScope::OperatorEditorial(&request),|workspace|{
            let result=crate::operator_editorial::admit(workspace,&owner(),&request)?;
            let job_index=workspace["jobs"].as_array().unwrap().len()-1;
            workspace["jobs"][job_index]["operatorReviewPreview"]["entries"][0]["evidence"]["posts"][0]["mediaPolicy"]["decisionBasis"]["durationMs"]=json!(99999);
            if resign {
                // Also forge all derived receipt/journal pins: validation must
                // compare retained acquisition evidence to immutable before.
                let digest=crate::editorial_review::operator_acquisition_digest(&workspace["jobs"][job_index]["operatorReviewPreview"]["entries"][0]["evidence"]);
                workspace["jobs"][job_index]["operatorReviewPreview"]["entries"][0]["candidate"]["acquisitionMediaDigest"]=json!(digest);
                let mut receipt=workspace["proposals"][0]["editorialReview"].clone();
                receipt["candidate"]["acquisitionMediaDigest"]=json!(digest);receipt.as_object_mut().unwrap().remove("receiptSha256");
                receipt["receiptSha256"]=json!(crate::editorial_review::hash_text(&receipt.to_string()));
                workspace["proposals"][0]["editorialReview"]=receipt.clone();
                let last=workspace["proposals"][0]["editorialReviews"].as_array().unwrap().len()-1;
                workspace["proposals"][0]["editorialReviews"][last]=receipt.clone();
                let audit_index=workspace["audit"].as_array().unwrap().len()-2;
                workspace["audit"][audit_index]["receiptSha256"][0]=receipt["receiptSha256"].clone();
            }
            Ok(result)
        }).await;
        assert!(tampered.is_err(),"raw acquisition corruption, consistent forged pins={resign}");
        assert_eq!(db.read().await.unwrap(),before,"tamper cannot persist any receipt/job/ledger state");
    }
    let (result,changed)=db.change_admission_observed(crate::storage::AdmissionScope::OperatorEditorial(&request),|workspace|
        crate::operator_editorial::admit(workspace,&owner(),&request)).await.unwrap();assert!(changed);
    let saved=db.read().await.unwrap();assert_eq!(result["accepted"].as_array().unwrap().len(),2);assert!(current(&saved,0));
    let (replay,changed)=db.change_admission_observed(crate::storage::AdmissionScope::OperatorEditorial(&request),|workspace|
        crate::operator_editorial::admit(workspace,&owner(),&request)).await.unwrap();assert!(!changed);
    assert_eq!(replay["jobId"],result["jobId"]);assert_eq!(replay["replayed"],true);assert_eq!(db.read().await.unwrap(),saved);db.close().await;
}

#[test]
fn exact_independent_video_reply_uses_mandatory_speech_without_acquiring_visuals() {
    let mut d = fixture(1);
    assert!(!EvidenceContext::new(&d).video_ready(&d["items"][0]).unwrap());
    complete_source_audio(&mut d);
    let reference = add(&mut d, "i0", "reply_and_close", "Спасибо за отклик 🙂");
    assert!(enabled(&d["proposals"][0]));
    assert_eq!(d["proposals"][0]["status"], "draft");
    assert!(!current(&d, 0));
    let refs = json!({"proposals":[reference]});
    assert!(crate::create_approval(&mut d, &owner(), &refs).is_err());
    model_accept(&mut d, &reference);
    let receipt = &d["proposals"][0]["editorialReview"];
    assert_eq!(receipt["mediaDependency"], independent());
    assert_eq!(receipt["source"]["runMetadata"]["model"], "gpt-6.1-sol");
    assert_eq!(receipt["source"]["runMetadata"]["reasoningEffort"], "high");
    assert_eq!(receipt["candidate"]["decisionMediaEvidence"][0]["audioReady"], true);
    assert_eq!(receipt["candidate"]["decisionMediaEvidence"][0]["visualProvided"], false);
    assert!(current(&d, 0));
    assert_independent_media_approved(&d, &reference);
    assert_eq!(EvidenceContext::new(&d).decision_video_evidence(&d["posts"][0]).unwrap()["visualReady"], false,
        "decision independence must not fabricate visual completion");
}

#[test]
fn independent_delete_uses_real_operator_receipt_before_native_approval() {
    let mut d = fixture(1);
    d["items"][0]["text"] = json!("Synthetic prohibited spam");
    let reference = add(&mut d, "i0", "delete", "");
    let refs = json!({"proposals":[reference]});
    assert!(crate::create_approval(&mut d, &owner(), &refs).is_err());
    let request = operator_request(&d, &reference, independent());
    crate::operator_editorial::admit(&mut d, &owner(), &request).unwrap();
    assert_eq!(d["proposals"][0]["kind"], "delete");
    assert_eq!(d["proposals"][0]["text"], "");
    assert_eq!(d["proposals"][0]["editorialReview"]["source"]["kind"], "operator_assisted_review");
    assert!(current(&d, 0));
    assert_eq!(crate::create_approval(&mut d, &owner(), &refs).unwrap()["status"], "approved");
    assert!(d["operations"].as_array().unwrap().is_empty());
}

#[test]
fn technical_audio_visual_and_unresolved_model_dependencies_remain_held() {
    for (text, needs) in [
        ("В ролике заявлен расход 5 литров", json!({"audio":"required","visual":"independent"})),
        ("На кадре видны два разъёма", json!({"audio":"independent","visual":"required"})),
        ("Нужно уточнить смысл ролика", json!({"audio":"unknown","visual":"independent"})),
        ("Неясно, что показано", json!({"audio":"independent","visual":"unknown"})),
    ] {
        assert!(validate_judgment(&json!({"decisionMediaContract":CONTRACT,"decisionMediaEvidence":[]}),
            &json!({"decision":"accept","mediaDependency":needs})).is_err(),
            "a nonvideo sibling in a marked batch cannot claim supplied media without any capture");
        let mut d = fixture(1);
        if needs["audio"]!="required" {complete_source_audio(&mut d);}
        let reference = add(&mut d, "i0", "reply_and_close", text);
        if needs["audio"]=="required" {
            assert_material_hold(&d, &reference);
            assert!(crate::create_approval(&mut d, &owner(), &json!({"proposals":[reference]})).is_err());
            continue;
        }
        let plan = crate::editorial_review::plan_new(&d, &json!([reference]), AT).unwrap();
        let batch = &plan["batches"][0];
        let result = captured_verdict(&mut d, batch, &[needs]);
        let out = crate::editorial_review::admit(&mut d, batch, &result, AT).unwrap();
        assert_eq!(out["outcomes"][0]["decision"], "hold", "{text}: {out}");
        assert!(d["proposals"][0].get("editorialReview").is_none());
        assert!(crate::create_approval(&mut d, &owner(), &json!({"proposals":[reference]})).is_err());
    }
}

#[test]
fn operator_review_cannot_omit_forge_or_guess_media_dependencies() {
    let mut d = fixture(1);
    complete_source_audio(&mut d);
    let reference = add(&mut d, "i0", "reply_and_close", "Спасибо за отклик 🙂");
    let valid = operator_request(&d, &reference, independent());
    for needs in [Value::Null, json!({"audio":"independent"}),
        json!({"audio":"independent","visual":"independent","skip":true}),
        json!({"audio":"optional","visual":"independent"}),
        json!({"audio":"independent","visual":"required"}),
        json!({"audio":"independent","visual":"unknown"})] {
        let mut request = valid.clone();
        request["operatorReview"]["entries"][0]["mediaDependency"] = needs;
        let mut next = d.clone();
        assert!(crate::operator_editorial::admit(&mut next, &owner(), &request).is_err());
        assert_eq!(next, d, "invalid operator judgment has no partial durable effects");
    }
    let mut missing = valid.clone();
    missing["operatorReview"]["entries"][0].as_object_mut().unwrap().remove("mediaDependency");
    let before = d.clone();
    assert!(crate::operator_editorial::admit(&mut d, &owner(), &missing).is_err());
    assert_eq!(d, before);
    let mut operator = owner();
    operator.role = "operator".into();
    assert!(crate::operator_editorial::admit(&mut d, &operator, &valid).is_err());
    assert_eq!(d, before);
}

#[test]
fn exact_owner_media_policy_is_a_floor_even_for_independent_semantic_judgment() {
    for mode in ["full_audio_only", "full_audio_visual"] {
        let mut d = fixture(1);
        let source = crate::media_fullframes::source_version(&d["posts"][0], d["account"].as_str().unwrap());
        d["settings"]["postMediaPolicies"] = json!({"post":{
            "version":1,"revision":1,"status":"active","postId":"post","mode":mode,
            "account":d["account"],"connectorBinding":d["connectorBinding"],"sourceVersion":source
        }});
        let reference = add(&mut d, "i0", "delete", "");
        assert_material_hold(&d, &reference);
        let (candidate, _) = crate::editorial_review::operator_candidate(&d, &d["proposals"][0]).unwrap();
        let media = &candidate["decisionMediaEvidence"][0];
        assert_eq!(media["ownerAudioRequired"], true);
        assert_eq!(media["ownerVisualRequired"], mode == "full_audio_visual");
        assert!(validate_judgment(&candidate, &json!({"decision":"accept","mediaDependency":independent()})).is_err());
        let request = operator_request(&d, &reference, independent());
        let before = d.clone();
        assert!(crate::operator_editorial::admit(&mut d, &owner(), &request).is_err());
        assert_eq!(d, before);
        assert!(crate::create_approval(&mut d, &owner(), &json!({"proposals":[reference]})).is_err());

        // Exercise the visual floor separately after admitting actual synthetic
        // full-audio catalog evidence; the earlier missing audio cannot mask it.
        complete_source_audio(&mut d);
        let fresh = crate::editorial_review::plan_new(&d, &json!([reference]), AT).unwrap();
        let batch = &fresh["batches"][0];
        let media = &batch["request"]["editorialCandidates"][0]["decisionMediaEvidence"][0];
        assert_eq!(media["audioReady"], true);
        assert_eq!(media["audioProvided"], true);
        assert_eq!(media["visualReady"], false);
        let result = captured_verdict(&mut d, batch, &[independent()]);
        let out = crate::editorial_review::admit(&mut d, batch, &result, AT).unwrap();
        if mode == "full_audio_visual" {
            assert_eq!(out["outcomes"][0]["decision"], "hold", "missing visual still obeys owner's floor");
            assert!(!current(&d, 0));
            assert!(crate::create_approval(&mut d, &owner(), &json!({"proposals":[reference]})).is_err());
        } else {
            assert_eq!(out["outcomes"][0]["decision"], "accept");
            assert!(current(&d, 0));
            assert_eq!(crate::create_approval(&mut d, &owner(), &json!({"proposals":[reference]})).unwrap()["status"], "approved");
        }
    }
}

#[test]
fn accepted_media_receipt_expires_on_exact_text_action_source_rule_and_parent_changes() {
    let mut d = fixture(1);
    complete_source_audio(&mut d);
    let reference = add(&mut d, "i0", "reply_and_close", "Спасибо за отклик 🙂");
    model_accept(&mut d, &reference);
    for change in ["text", "action", "post", "media", "rule", "parent"] {
        let mut next = d.clone();
        match change {
            "text" => next["proposals"][0]["text"] = json!("A different exact response"),
            "action" => { next["proposals"][0]["kind"] = json!("delete"); next["proposals"][0]["text"] = json!(""); }
            "post" => next["posts"][0]["text"] = json!("Updated source post"),
            "media" => next["posts"][0]["attachments"][0]["url"] = json!("https://example.invalid/replacement.mp4"),
            "rule" => {
                // Editorial evidence selects at now(); a future validFrom rule
                // is intentionally inactive and would not test invalidation.
                let saved = crate::knowledge::save_instruction(&mut next, &json!({"requestId":"offline-new-rule",
                    "title":"Current voice","text":"Use a different current company voice"}), &crate::now()).unwrap();
                let evidence = EvidenceContext::new(&next).evidence_for_item("i0").unwrap();
                assert!(evidence["knowledgeManifest"].as_array().unwrap().iter().any(|pin|
                    pin["kind"] == "rule" && pin["entryId"] == saved["entry"]["id"]
                        && pin["versionId"] == saved["version"]["id"]), "changed active rule is actual canonical editorial input");
            }
            "parent" => next["branches"][0]["messages"][0]["parentId"] = json!("another-parent"),
            _ => unreachable!(),
        }
        assert!(!current(&next, 0), "{change} must invalidate the exact semantic binding");
        assert!(crate::create_approval(&mut next, &owner(), &json!({"proposals":[reference]})).is_err(), "{change}");
        assert_eq!(next["proposals"][0]["editorialReview"], d["proposals"][0]["editorialReview"], "historical receipt stays intact");
    }
}

#[test]
fn same_text_sibling_cannot_inherit_another_proposals_media_receipt() {
    let mut d = fixture(2);
    complete_source_audio(&mut d);
    let a = add(&mut d, "i0", "reply_and_close", "Спасибо 🙂");
    let b = add(&mut d, "i1", "reply_and_close", "Спасибо 🙂");
    model_accept(&mut d, &a);
    d["proposals"][1]["editorialReview"] = d["proposals"][0]["editorialReview"].clone();
    assert!(current(&d, 0));
    assert!(!current(&d, 1));
    assert!(crate::create_approval(&mut d, &owner(), &json!({"proposals":[b]})).is_err());
    assert_eq!(d["proposals"][0]["editorialReview"]["candidate"]["proposalId"], a["id"]);
}

#[test]
fn legacy_receipt_and_unmarked_draft_are_never_upgraded_to_media_permission() {
    let mut d = fixture(1);
    d["posts"][0]["attachments"] = json!([]);
    let reference = add(&mut d, "i0", "reply_and_close", "Спасибо 🙂");
    crate::editorial_review::fixture_accept(&mut d, reference["id"].as_str().unwrap()).unwrap();
    let saved = d["proposals"][0]["editorialReview"].clone();
    assert!(current(&d, 0));
    assert!(!enabled(&d["proposals"][0]));
    d["posts"][0]["attachments"] = json!([{"type":"video","url":"https://example.invalid/new.mp4"}]);
    assert!(!current(&d, 0));
    assert!(crate::create_approval(&mut d, &owner(), &json!({"proposals":[reference]})).is_err());
    assert_eq!(d["proposals"][0]["editorialReview"], saved);
    assert!(!enabled(&d["proposals"][0]));
    assert!(saved.get("mediaDependency").is_none());
    assert!(saved["candidate"].get("decisionMediaEvidence").is_none());
}

#[test]
fn mixed_batch_approves_independent_reply_and_holds_required_visual_sibling() {
    let mut d = fixture(2);
    complete_source_audio(&mut d);
    let ready = add(&mut d, "i0", "reply_and_close", "Спасибо за отклик 🙂");
    let held = add(&mut d, "i1", "reply_and_close", "На кадре видны два разъёма");
    let plan = crate::editorial_review::plan_new(&d, &json!([ready, held]), AT).unwrap();
    assert_eq!(plan["batches"].as_array().unwrap().len(), 1);
    let batch = &plan["batches"][0];
    let result = captured_verdict(&mut d, batch, &[
        independent(), json!({"audio":"independent","visual":"required"})
    ]);
    let out = crate::editorial_review::admit(&mut d, batch, &result, AT).unwrap();
    assert_eq!(out["outcomes"][0]["decision"], "accept");
    assert_eq!(out["outcomes"][1]["decision"], "hold");
    assert!(current(&d, 0));
    assert!(!current(&d, 1));
    assert!(d["proposals"][1].get("editorialReview").is_none());
    assert!(crate::create_approval(&mut d, &owner(), &json!({"proposals":[held]})).is_err());
    assert_independent_media_approved(&d, &ready);
}

#[test]
fn generation_exemption_requires_exact_action_text_dependency_and_captured_video_source() {
    let d = fixture(1);
    let triage = crate::prepare_bundle::triage(&d, "i0").unwrap();
    assert!(enabled(&triage["request"]));
    assert_eq!(triage["request"]["responseContract"], "compact_decisions_v1");
    let mut request = EvidenceContext::new(&d).evidence_for_item("i0").unwrap();
    attach_request(&d, &mut request).unwrap();
    assert!(enabled(&request));
    let bundle = json!({"request":request});
    let proposal = json!({"itemId":"i0","kind":"reply_and_close","text":"Спасибо 🙂"});
    let mut metadata = crate::editorial_review::fixture_metadata();
    metadata["model"] = json!(crate::codex_model_policy::MODEL);
    metadata["modelProfile"] = json!(crate::codex_model_policy::PROFILE);
    metadata["reasoningEffort"] = json!("high");
    metadata["cliSha256"] = json!(crate::codex_model_policy::CLI_SHA256);
    metadata["decisionMediaContract"] = json!(CONTRACT);
    let result = json!({"proposals":[proposal],"runMetadata":metadata,"editorialEvidence":{"version":1,
        "contract":crate::editorial_review::CONTRACT,"entries":[{
            "itemId":"i0","kind":"reply_and_close","textSha256":crate::editorial_review::hash_text("Спасибо 🙂"),
            "decision":"accept","reason":"Synthetic exact generation judgment",
            "checks":{"companyRules":"pass","intent":"pass","factualScope":"pass"},"mediaDependency":independent()
        }]}});
    assert!(generation_ready(&d, &bundle, &result, &proposal).is_ok());
    for change in ["text", "action", "recipient", "missing_proof", "dependency", "source", "capture"] {
        let mut workspace = d.clone();
        let mut generated = proposal.clone();
        let mut proof = result.clone();
        let mut captured = bundle.clone();
        match change {
            "text" => generated["text"] = json!("Another final text"),
            "action" => { generated["kind"] = json!("delete"); generated["text"] = json!(""); }
            "recipient" => generated["itemId"] = json!("other-item"),
            "missing_proof" => { proof.as_object_mut().unwrap().remove("editorialEvidence"); }
            "dependency" => proof["editorialEvidence"]["entries"][0]["mediaDependency"]["audio"] = json!("required"),
            "source" => workspace["posts"][0]["attachments"][0]["url"] = json!("https://example.invalid/later.mp4"),
            "capture" => captured["request"]["posts"][0]["decisionMediaEvidence"]["sourceVersion"] = json!("forged"),
            _ => unreachable!(),
        }
        assert!(generation_ready(&workspace, &captured, &proof, &generated).is_err(), "{change}");
    }
}

#[test]
fn canonical_target_id_capture_rejects_unavailable_parent_target_or_missing_cursor() {
    let mut d = fixture(1);
    complete_source_audio(&mut d);
    d["items"][0]["id"] = json!("item-11341-abc");
    d["items"][0]["itemId"] = json!("abc");
    d["items"][0]["targetId"] = json!("comment-11341-abc");
    d["branches"][0]["contextComplete"] = json!(false);
    d["branches"][0]["contextTruncated"] = json!(true);
    d["branches"][0]["missingParentIds"] = json!(["unrelated-thread-parent"]);
    d["branches"][0]["messages"] = json!([
        {"id":"comment-11341-parent","parentId":null,"role":"customer","text":"Как вам цвет?"},
        {"id":"comment-11341-abc","parentId":"comment-11341-parent","role":"customer","text":"Красивый цвет!"}
    ]);
    let reference = add(&mut d, "item-11341-abc", "reply_and_close", "Спасибо за отклик 🙂");
    let refs = json!({"proposals":[reference]});
    assert!(crate::operator_editorial::capture(&d, &owner(), &refs).is_ok(),
        "complete addressed chain is reviewable despite uncertified whole thread and canonical ID namespaces");
    let mut parentless = d.clone();
    parentless["branches"][0]["messages"][1]["parentId"] = Value::Null;
    parentless["branches"][0]["messages"].as_array_mut().unwrap().remove(0);
    assert!(crate::operator_editorial::capture(&parentless, &owner(), &refs).is_ok(),
        "parentless addressed comment does not require unrelated missing thread history");
    for change in ["image_only_parent", "missing_parent", "missing_target", "unavailable_target", "image_only_target",
        "blank_parent_present", "blank_parent_unknown", "absent_parent_text", "blank_target", "deleted_parent", "deleted_target"] {
        let mut next = d.clone();
        match change {
            "image_only_parent" => {
                next["branches"][0]["messages"][0]["text"] = json!("");
                next["branches"][0]["messages"][0]["attachments"] = json!([{"type":"photo","url":"https://example.invalid/parent.jpg"}]);
            }
            "missing_parent" => { next["branches"][0]["messages"].as_array_mut().unwrap().remove(0); }
            "missing_target" => { next["branches"][0]["messages"].as_array_mut().unwrap().remove(1); }
            "unavailable_target" => next["branches"][0]["messages"][1]["textUnavailable"] = json!(true),
            "image_only_target" => {
                next["branches"][0]["messages"][1]["text"] = json!("");
                next["branches"][0]["messages"][1]["attachments"] = json!([{"type":"photo","url":"https://example.invalid/target.jpg"}]);
            }
            "blank_parent_present" | "blank_parent_unknown" => {
                let parent = &mut next["branches"][0]["messages"][0];
                parent["text"] = json!("  ");
                parent["attachmentsState"] = json!(if change == "blank_parent_present" {"present"} else {"unknown"});
                if change == "blank_parent_present" { parent["attachments"] = json!([]); }
                else { parent.as_object_mut().unwrap().remove("attachments"); }
            }
            "absent_parent_text" => { next["branches"][0]["messages"][0].as_object_mut().unwrap().remove("text"); }
            "blank_target" => {
                next["branches"][0]["messages"][1]["text"] = json!("");
                next["branches"][0]["messages"][1]["attachments"] = json!([]);
            }
            "deleted_parent" => next["branches"][0]["messages"][0]["deleted"] = json!(true),
            "deleted_target" => next["branches"][0]["messages"][1]["deleted"] = json!(true),
            _ => unreachable!(),
        }
        let context = EvidenceContext::new(&next);
        let evidence = context.evidence_for_item("item-11341-abc").unwrap();
        assert!(capture(&context, &next["items"][0], &evidence).is_err(), "{change}: exact capture must fail closed");
        assert!(crate::operator_editorial::capture(&next, &owner(), &refs).is_err(), "{change}: operator route must preserve the same hold");
        assert_eq!(next["proposals"][0], d["proposals"][0], "capture is read-only");
    }
}

#[test]
fn full_generation_reducer_approves_independent_reply_and_retains_required_visual_hold() {
    let mut d = fixture(2);
    complete_source_audio(&mut d);
    let mut bundle = crate::prepare_bundle::build(&d, &[json!("i0"), json!("i1")], &[]).unwrap();
    bundle["request"]["purpose"] = json!("triage");
    bundle["request"]["preparationMode"] = json!("single_pass_v1");
    attach_request(&d, &mut bundle["request"]).unwrap();
    crate::preparation_unit::attach(&d, &mut bundle, &crate::now()).unwrap();
    crate::preparation_materials::attach_request(&d, &mut bundle["request"]).unwrap();
    crate::preparation_materials::require_request(&d, &bundle["request"]).unwrap();
    bundle["digest"] = json!(crate::editorial_review::hash_text(&bundle["request"].to_string()));
    d["jobs"] = json!([{"id":"offline-generation","kind":"assistant","purpose":"engine_prepare",
        "account":d["account"],"connectorBinding":d["connectorBinding"],"status":"running","prepareBundle":bundle}]);
    let proposals = json!([
        {"itemId":"i0","kind":"reply_and_close","text":"Спасибо за отклик 🙂"},
        {"itemId":"i1","kind":"reply_and_close","text":"На кадре видны два разъёма"}
    ]);
    let mut metadata = crate::editorial_review::fixture_metadata();
    metadata["model"] = json!(crate::codex_model_policy::MODEL);
    metadata["modelProfile"] = json!(crate::codex_model_policy::PROFILE);
    metadata["reasoningEffort"] = json!("high");
    metadata["cliSha256"] = json!(crate::codex_model_policy::CLI_SHA256);
    metadata["promptVersion"] = json!("communityhero-preparation-v1-single-pass");
    metadata["decisionMediaContract"] = json!(CONTRACT);
    let mut result = json!({"text":"Synthetic mixed generation","sources":[],"proposals":proposals,"runMetadata":metadata,
        "editorialEvidence":{"version":1,"contract":crate::editorial_review::CONTRACT,
            "entries":proposals.as_array().unwrap().iter().enumerate().map(|(n,p)|json!({
                "itemId":p["itemId"],"kind":p["kind"],"textSha256":crate::editorial_review::hash_text(p["text"].as_str().unwrap()),
                "decision":"accept","reason":"Exact synthetic generation judgment for this candidate",
                "checks":{"companyRules":"pass","intent":"pass","factualScope":"pass"},
                "mediaDependency":if n==0 {independent()} else {json!({"audio":"independent","visual":"required"})}
            })).collect::<Vec<_>>()}});
    crate::model_material_receipt::fixture_result(&mut d, "offline-generation", &bundle["request"], &mut result).unwrap();
    let outcome = crate::prepare_bundle::admit_to(&mut d, "offline-generation", None, &result).unwrap();
    assert_eq!(outcome["status"], "review", "{outcome}");
    assert_eq!(outcome["candidates"][0]["status"], "review");
    assert_eq!(outcome["candidates"][1]["status"], "rejected");
    assert_eq!(outcome["candidates"][1]["reason"], "Exact decision requires unavailable media evidence");
    assert_eq!(d["jobs"][0]["prepareOutcome"], outcome, "held sibling is retained as a durable outcome");
    assert_eq!(d["proposals"].as_array().unwrap().len(), 1);
    assert_eq!(d["proposals"][0]["itemId"], "i0");
    assert_eq!(d["proposals"][0]["editorialReview"]["source"]["kind"], "reused_generation_review");
    assert!(current(&d, 0));
    assert_eq!(d["items"][1]["workflow"], "attention", "rejected sibling was never staged");
    let reference = json!({"id":d["proposals"][0]["id"],"revision":d["proposals"][0]["revision"]});
    assert_independent_media_approved(&d, &reference);
}

#[test]
fn independent_review_does_not_cover_extra_audio_video_unknown_or_comment_assets() {
    for asset in ["audio", "video", "unknown", "comment_audio", "metadata"] {
        let mut d = fixture(1);
        match asset {
            "audio" | "video" | "unknown" => d["posts"][0]["attachments"].as_array_mut().unwrap()
                .push(json!({"type":asset,"url":"https://example.invalid/extra-asset"})),
            "comment_audio" => d["items"][0]["attachments"] = json!([{"type":"audio","url":"https://example.invalid/comment-audio"}]),
            "metadata" => d["posts"][0]["attachmentsState"] = json!("unknown"),
            _ => unreachable!(),
        }
        complete_source_audio(&mut d);
        let reference = add(&mut d, "i0", "reply_and_close", "Спасибо 🙂");
        if matches!(asset, "video" | "metadata") {
            assert_material_hold(&d, &reference);
            assert!(d["proposals"][0].get("editorialReview").is_none());
        } else {
            model_accept(&mut d, &reference);
            assert!(current(&d, 0), "fresh receipt must be valid, not merely stale: {asset}");
        }
        let gate = crate::media_context_gate::inspect(&EvidenceContext::new(&d), &d["proposals"][0]).unwrap();
        assert_eq!(gate["status"], "missing", "{asset}: {gate}");
        if matches!(asset, "audio" | "video") {
            assert!(gate["requirements"].as_array().unwrap().iter()
                .any(|r|r["reason"] == "multi_asset_audio_coverage_unproven"));
        }
        let before = d.clone();
        assert!(crate::create_approval(&mut d, &owner(), &json!({"proposals":[reference]})).is_err());
        assert_eq!(d, before);
    }
}

#[test]
fn independent_video_review_holds_before_model_on_missing_mandatory_image() {
    let mut d = fixture(1);
    d["posts"][0]["attachments"].as_array_mut().unwrap()
        .push(json!({"type":"photo","url":"https://example.invalid/missing-photo.jpg"}));
    complete_source_audio(&mut d);
    let reference = add(&mut d, "i0", "reply_and_close", "Спасибо 🙂");
    assert_material_hold(&d, &reference);
    assert!(!current(&d, 0));
    assert!(d["proposals"][0].get("editorialReview").is_none());
    let mut request = EvidenceContext::new(&d).evidence_for_item("i0").unwrap();
    crate::preparation_materials::attach_request(&d, &mut request).unwrap();
    assert!(request["materialReadiness"]["requirements"].as_array().unwrap().iter().any(|r|
        r["kind"]=="post_photo"&&r["attachmentIndex"]==1&&r["reasonCode"]=="photo_acquisition_required"));
    let gate = crate::media_context_gate::inspect(&EvidenceContext::new(&d), &d["proposals"][0]).unwrap();
    assert_eq!(gate["status"], "missing");
    assert!(gate["requirements"].as_array().unwrap().iter().any(|r|r["reason"] == "image_evidence_missing"));
    assert!(!gate["requirements"].as_array().unwrap().iter().any(|r|r["reason"] == "exact_review_media_independent"));
    let before = d.clone();
    assert!(crate::create_approval(&mut d, &owner(), &json!({"proposals":[reference]})).is_err());
    assert_eq!(d, before);
}

#[test]
fn required_audio_review_uses_complete_source_proof_without_independence_exemption() {
    let mut d = fixture(1);
    complete_source_audio(&mut d);
    let reference = add(&mut d, "i0", "reply_and_close", "Ответ опирается на полный аудиотекст");
    let plan = crate::editorial_review::plan_new(&d, &json!([reference]), AT).unwrap();
    let batch = &plan["batches"][0];
    let result = captured_verdict(&mut d, batch, &[json!({"audio":"required","visual":"independent"})]);
    let out = crate::editorial_review::admit(&mut d, batch, &result, AT).unwrap();
    assert_eq!(out["outcomes"][0]["decision"], "accept", "{out}");
    let gate = crate::media_context_gate::inspect(&EvidenceContext::new(&d), &d["proposals"][0]).unwrap();
    assert_eq!(gate["status"], "ready");
    assert_eq!(gate["requirements"][0]["reason"], "complete_source_audio_observed");
    assert_eq!(crate::create_approval(&mut d, &owner(), &json!({"proposals":[reference]})).unwrap()["status"], "approved");
}

#[test]
fn exact_independent_close_needs_review_and_expires_when_customer_question_changes() {
    let mut d = fixture(1);
    complete_source_audio(&mut d);
    let reference = add(&mut d, "i0", "close", "");
    assert!(crate::create_approval(&mut d, &owner(), &json!({"proposals":[reference]})).is_err());
    model_accept(&mut d, &reference);
    assert_independent_media_approved(&d, &reference);
    d["items"][0]["text"] = json!("А какой расход заявлен в видео?");
    d["branches"][0]["messages"][1]["text"] = d["items"][0]["text"].clone();
    assert!(!current(&d, 0));
    let before = d.clone();
    assert!(crate::create_approval(&mut d, &owner(), &json!({"proposals":[reference]})).is_err());
    assert_eq!(d, before, "CLOSE must not sweep a newly changed question");
}

#[test]
fn independent_floor_rejects_tampered_unmarked_foreign_and_revised_receipts() {
    let mut original = fixture(1);
    complete_source_audio(&mut original);
    let reference = add(&mut original, "i0", "reply_and_close", "Спасибо 🙂");
    model_accept(&mut original, &reference);
    for change in ["receipt_hash", "dependency", "unmarked", "revision", "account", "binding"] {
        let mut d = original.clone();
        match change {
            "receipt_hash" => d["proposals"][0]["editorialReview"]["receiptSha256"] = json!("0".repeat(64)),
            "dependency" => d["proposals"][0]["editorialReview"]["mediaDependency"]["audio"] = json!("required"),
            "unmarked" => { d["proposals"][0].as_object_mut().unwrap().remove("decisionMediaContract"); }
            "revision" => d["proposals"][0]["revision"] = json!(99),
            "account" => d["proposals"][0]["editorialReview"]["account"] = json!("foreign-company"),
            "binding" => d["proposals"][0]["editorialReview"]["connectorBinding"] = json!({"id":"foreign-binding"}),
            _ => unreachable!(),
        }
        let gate = crate::media_context_gate::inspect(&EvidenceContext::new(&d), &d["proposals"][0]).unwrap();
        assert!(!gate["requirements"].as_array().unwrap().iter().any(|r|r["reason"]=="exact_review_media_independent"),
            "invalid receipt cannot grant semantic independence: {change}: {gate}");
        let before = d.clone();
        assert!(crate::create_approval(&mut d, &owner(), &json!({"proposals":[reference]})).is_err(), "{change}");
        assert_eq!(d, before);
    }
}

#[test]
fn independent_video_reply_execution_keeps_exact_approved_receipt_and_source_pins() {
    let mut d = fixture(1);
    let fixture_owner = crate::runtime_lifecycle_startup::Admission::fixture(crate::accounts::Profile::LikeAvto);
    crate::runtime_lifecycle_startup::initialize_fixture(&mut d, fixture_owner.identity()).unwrap();
    crate::connection_gate::fixture_open(&mut d).unwrap();
    complete_source_audio(&mut d);
    let reference = add(&mut d, "i0", "reply_and_close", "Спасибо 🙂");
    model_accept(&mut d, &reference);
    let approval = crate::create_approval(&mut d, &owner(), &json!({"proposals":[reference]})).unwrap();
    let key = approval["id"].as_str().unwrap();
    let receipt_sha = approval["proposals"][0]["proposal"]["editorialReview"]["receiptSha256"].clone();
    let media_before = d["materials"].clone();
    // Obtain a later valid receipt before scheduling; live admitted operations
    // correctly prohibit another review of this same recipient.
    let mut reviewed_again = d.clone();
    let plan = crate::editorial_review::plan_fresh(&reviewed_again, &json!([reference]), AT).unwrap();
    let batch = &plan["batches"][0];
    let mut response = verdict(batch, &[independent()]);
    response["editorial"][0]["reason"] = json!("A later exact independent review");
    crate::editorial_review::fixture_capture_result(&mut reviewed_again, batch, &mut response).unwrap();
    crate::editorial_review::admit(&mut reviewed_again, batch, &response, AT).unwrap();
    assert!(current(&reviewed_again, 0));
    assert_ne!(reviewed_again["proposals"][0]["editorialReview"]["receiptSha256"], receipt_sha);
    let before = reviewed_again.clone();
    assert!(crate::execute_admission::admit(&mut reviewed_again, &owner(), key, &json!({})).is_err());
    assert_eq!(reviewed_again, before);
    let (_, scheduled) = crate::execute_admission::admit(&mut d, &owner(), key, &json!({})).unwrap();
    let (_, operations) = scheduled.unwrap();
    assert!(!operations.is_empty(), "native scheduling must be admitted, not a UI-only ready state");
    for op in &operations {
        assert_eq!(op["approvedEditorialReceiptSha256"], receipt_sha);
        assert!(crate::dispatch_diagnostics::local_check(&d, op).is_ok());
    }
    assert_eq!(d["materials"], media_before);
    let op = &operations[0];
    let mut changed_source = d.clone();
    changed_source["posts"][0]["attachments"][0]["url"] = json!("https://example.invalid/replaced.mp4");
    let stop = crate::dispatch_diagnostics::local_check(&changed_source, op).err().unwrap();
    assert_eq!(stop.evidence["providerCallAttempted"], false);

    // A new valid review of identical text is not the approved receipt.
    d["proposals"][0]["editorialReview"] = reviewed_again["proposals"][0]["editorialReview"].clone();
    let stop = crate::dispatch_diagnostics::local_check(&d, op).err().unwrap();
    assert_eq!(stop.evidence["code"], "approved_editorial_receipt_changed");
    assert_eq!(stop.evidence["providerCallAttempted"], false);
}

#[test]
fn moderation_execution_admission_and_local_check_retain_exact_approved_receipt() {
    let mut d = fixture(1);
    let fixture_owner = crate::runtime_lifecycle_startup::Admission::fixture(crate::accounts::Profile::LikeAvto);
    crate::runtime_lifecycle_startup::initialize_fixture(&mut d, fixture_owner.identity()).unwrap();
    crate::connection_gate::fixture_open(&mut d).unwrap();
    let reference = add(&mut d, "i0", "delete", "");
    let request = operator_request(&d, &reference, independent());
    crate::operator_editorial::admit(&mut d, &owner(), &request).unwrap();
    // Capture the alternative legitimate review while the proposal is still a
    // draft. An approved proposal cannot be reset to draft merely for this test.
    let mut reviewed_again = d.clone();
    let approval = crate::create_approval(&mut d, &owner(), &json!({"proposals":[reference]})).unwrap();
    let key = approval["id"].as_str().unwrap();
    let approved_receipt = approval["proposals"][0]["proposal"]["editorialReview"]["receiptSha256"].clone();

    // A second legitimate current review still needs a new approval.
    let mut request = operator_request(&reviewed_again, &reference, independent());
    request["requestId"] = json!("offline-later-moderation-review");
    request["operatorReview"]["entries"][0]["reason"] = json!("A later explicit operator review of the same exact moderation decision");
    crate::operator_editorial::admit(&mut reviewed_again, &owner(), &request).unwrap();
    assert!(current(&reviewed_again, 0));
    assert_ne!(reviewed_again["proposals"][0]["editorialReview"]["receiptSha256"], approved_receipt);
    let before = reviewed_again.clone();
    assert!(crate::execute_admission::admit(&mut reviewed_again, &owner(), key, &json!({})).is_err());
    assert_eq!(reviewed_again, before, "changed approved receipt cannot create an operation");

    let (_, scheduled) = crate::execute_admission::admit(&mut d, &owner(), key, &json!({})).unwrap();
    let (_, operations) = scheduled.unwrap();
    assert_eq!(operations.len(), 1);
    let op = &operations[0];
    assert_eq!(op["action"]["action"], "delete");
    assert_eq!(op["approvedEditorialReceiptSha256"], approved_receipt);
    assert!(crate::dispatch_diagnostics::local_check(&d, op).is_ok());
    d["proposals"][0]["editorialReview"] = reviewed_again["proposals"][0]["editorialReview"].clone();
    assert!(current(&d, 0), "later receipt is current but is not the operator-approved receipt");
    let failure = crate::dispatch_diagnostics::local_check(&d, op).err().unwrap();
    assert_eq!(failure.outcome, crate::dispatch_diagnostics::Outcome::Stale);
    assert_eq!(failure.evidence["code"], "approved_editorial_receipt_changed");
    assert_eq!(failure.evidence["providerCallAttempted"], false);
    assert_eq!(d["operations"][0]["approvedEditorialReceiptSha256"], approved_receipt);
}
