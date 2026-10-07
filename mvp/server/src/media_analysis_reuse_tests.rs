use super::*;
const AT:&str="2026-10-04T08:00:00Z";
fn fixture()->(Value,Value,Value,Value,Value) {fixture_for(false)}
pub(crate) fn fixture_for(no_audio:bool)->(Value,Value,Value,Value,Value) {
    fixture_for_outcome(if no_audio{"no_audio"}else{"transcript"})
}
pub(crate) fn fixture_for_outcome(outcome:&str)->(Value,Value,Value,Value,Value){
    assert!(matches!(outcome,"transcript"|"no_audio"|"no_speech"));let no_audio=outcome=="no_audio";
    let store=crate::media_fullframes::store().unwrap();
    let source=store.put_bytes(b"deterministic retained file fixture").unwrap().to_json();
    let file=json!({"sha256":source["sha256"],"bytes":source["bytes"],"receiptSha256":"b".repeat(64),"probeSha256":"c".repeat(64)});
    let donor=json!({"id":"donor","postKey":"vk:donor","account":"LikeAvto","title":"Original upload","sourceUrl":"https://example.test/original","attachments":[{"type":"video","url":"https://example.test/original.mp4"}]});
    let target=json!({"id":"target","postKey":"ig:target","account":"LikeAvto","title":"Different cross-post title","sourceUrl":"https://example.test/target","attachments":[{"type":"video","url":"https://example.test/target.mp4"}]});
    let mut material=json!({"id":"paid-transcript","kind":"transcript","title":"Original upload","account":"LikeAvto","postKey":donor["postKey"],"sourceUrl":donor["sourceUrl"],"mediaSha256":source["sha256"],"text":"Existing complete spoken words.",
        "transcription":{"sourceVersion":crate::media_fullframes::source_version(&donor,"LikeAvto"),"sourcePostKey":donor["postKey"],"partial":false,"coverage":"full_audio","audioStatus":"transcribed","mediaDurationSeconds":1.0,"audioDurationSeconds":1.0,
            "ocr":{"sourceVersion":crate::media_fullframes::source_version(&donor,"LikeAvto"),"coverage":"sampled_frames","status":"completed","sampledFrames":3,"failedFrames":0,"exhaustive":false}}});
    if no_audio {material["text"]=json!("[Audio inspection: no_audio_stream. No spoken words were recovered.]");
        material["transcription"]["coverage"]=json!("no_audio_stream");material["transcription"]["audioStatus"]=json!("no_audio_stream");material["transcription"]["audioDurationSeconds"]=Value::Null;}
    if outcome=="no_speech"{material["text"]=json!("[Audio inspected: no speech detected.]");material["transcription"]["audioStatus"]=json!("inspected_no_speech");}
    let donor_ocr=json!({"id":"donor-screen-text","kind":"ocr","title":"Original upload screen text","account":"LikeAvto",
        "postKey":donor["postKey"],"sourceUrl":donor["sourceUrl"],"mediaSha256":source["sha256"],
        "text":"Original donor end-card CTA: offer only on original upload.","ocr":material["transcription"]["ocr"]});
    let mut d=crate::empty();d["posts"]=json!([donor,target]);d["materials"]=json!([material,donor_ocr]);
    crate::knowledge::sync_catalog(&mut d,AT).unwrap();
    let connector=crate::active_binding(&d).unwrap().to_json();
    let mut progress=crate::media_fullframes::initial("LikeAvto",&connector,&d["posts"][1],AT);
    progress["source"]=source.clone();progress["sourceIdentity"]=json!({"account":"LikeAvto","postKey":target["postKey"],"mediaSha256":source["sha256"]});
    let receipt=json!({"companyId":"LikeAvto","verifiedFile":file,"target":{"connectorBinding":connector,"postId":"target","postKey":"ig:target","sourceVersion":progress["sourceVersion"],"attachmentIndex":0,"attachmentIdentity":attachment_identity(&target["attachments"][0]),"aliasRevision":1}});
    let spec="d".repeat(64);let attempt=uuid::Uuid::new_v4().to_string();
    let mut request=json!({"companyId":"LikeAvto","stage":"asr","verifiedFile":file,"specSha256":spec,"attemptId":attempt,"owner":"fixture-worker","epoch":1,"manifestKey":attempt,
        "durationMs":1000,"segments":[{"index":0,"startMs":0,"endMs":1000}]});
    if no_audio {request["noAudio"]=json!(true);request["noAudioVerificationSha256"]=json!("e".repeat(64));request["segments"]=json!([]);}
    let mut ledger=Value::Null;crate::media_analysis::reserve(&mut ledger,&request).unwrap();
    let mut segments=Vec::new();
    if !no_audio {
        let mut dispatched=request.clone();dispatched["segmentIndex"]=json!(0);crate::media_analysis::mark_dispatched(&mut ledger,&dispatched).unwrap();
        let segment=crate::media_processing::analysis_output::capture_segment(&store,&request,0,"existing words","existing words",1000).unwrap();
        let mut segment_request=request.clone();segment_request["segment"]=segment.clone();crate::media_analysis::commit_segment(&mut ledger,&segment_request).unwrap();segments.push(segment);
    }
    let audio=json!({"materials":[material],"coverage":{"kind":if no_audio {"no_audio_stream"} else {"full_audio"},"durationMs":1000},"outcome":outcome,"reused":false});
    let result=crate::media_processing::analysis_output::capture_full(&store,&request,&segments,&audio).unwrap();
    let mut completed=request;completed["result"]=result;crate::media_analysis::commit_full_result(&mut ledger,&completed).unwrap();
    crate::media_analysis::put_ledger(&mut d,&ledger).unwrap();
    let payload=crate::media_fullframes::read(&store,&completed["result"]["normalizedOutput"]).unwrap();
    (d,progress,receipt,ledger,payload)
}
fn selected(d:&Value,p:&Value,r:&Value,l:&Value,v:&Value)->(Value,ExactFileReuseProof) {
    select_prepared(d,p,l,r,v,&"d".repeat(64)).unwrap().unwrap()
}

#[test]
fn knowledge_only_projection_without_jobs_preserves_ordinary_selection() {
    let post=json!({"id":"legacy-post","postKey":"vk:legacy","account":"LikeAvto","title":"Existing upload",
        "sourceUrl":"https://example.test/legacy","attachments":[{"type":"video","url":"https://example.test/legacy.mp4"}]});
    let mut full=json!({"account":"LikeAvto","jobs":[],"posts":[post],"materials":[{
        "id":"legacy-transcript","kind":"transcript","account":"LikeAvto","postKey":post["postKey"],
        "title":"Existing upload","text":"Previously admitted full spoken text.","sourceUrl":post["sourceUrl"],
        "transcription":{"sourceVersion":crate::media_fullframes::source_version(&post,"LikeAvto"),
            "sourcePostKey":post["postKey"],"partial":false,"coverage":"full_audio","audioStatus":"transcribed",
            "mediaDurationSeconds":1.0,"audioDurationSeconds":1.0}}]});
    crate::knowledge::sync_catalog(&mut full,AT).unwrap();
    let expected=crate::knowledge::select(&full,&[],std::slice::from_ref(&full["posts"][0]),AT).unwrap();
    let mut projected=full.clone();projected.as_object_mut().unwrap().remove("jobs");
    let before=projected.clone();
    assert!(bindings(&projected,AT).unwrap().is_empty());
    assert_eq!(crate::knowledge::select(&projected,&[],std::slice::from_ref(&projected["posts"][0]),AT).unwrap(),expected);
    assert!(crate::knowledge::TranscriptLookup::new(&projected,AT).unwrap().has(&projected["posts"][0]).unwrap());
    assert!(crate::media_analysis::ledger_from_workspace(&projected).is_err(),"durable writer contract remains strict");
    assert_eq!(projected,before,"reader must not manufacture a jobs collection");
}
#[test]
fn absent_jobs_is_not_a_bypass_for_malformed_present_evidence() {
    let mut d=json!({"account":"LikeAvto"});
    assert!(bindings(&d,AT).unwrap().is_empty());
    assert!(bindings(&d,"invalid-time").is_err());
    for account in [Value::Null,json!(" "),json!(17)] {
        d["account"]=account;assert!(bindings(&d,AT).is_err());
    }
    d["account"]=json!("LikeAvto");
    for jobs in [Value::Null,json!({}),json!("omitted"),json!(17),
        json!([{"id":"forged-ledger","account":"LikeAvto","kind":"media_analysis","status":"ledger","analysis":{}}])] {
        d["jobs"]=jobs;assert!(bindings(&d,AT).is_err(),"present malformed jobs must fail closed: {}",d["jobs"]);
    }
}
#[test]
fn omitted_jobs_cannot_recover_warmed_cross_post_authority_from_cache() {
    let mut d=test_cold_reused_workspace();warm_workspace(&d).unwrap();
    assert_eq!(bindings(&d,AT).unwrap().len(),1);
    let target=d["posts"][1].clone();
    assert!(crate::knowledge::TranscriptLookup::new(&d,AT).unwrap().strict_media_evidence(&target).unwrap()["audioReady"]==true);
    let retained=d.as_object_mut().unwrap().remove("jobs").unwrap();
    assert!(bindings(&d,AT).unwrap().is_empty());
    let proof=crate::knowledge::TranscriptLookup::new(&d,AT).unwrap().strict_media_evidence(&target).unwrap();
    assert_eq!(proof["audioReady"],false);assert_eq!(proof["screenTextReady"],false);
    let selected=crate::knowledge::select(&d,&[],std::slice::from_ref(&target),AT).unwrap();
    assert!(!rows(&selected,"materials").iter().any(|m|m.get("exactFileAnalysisReuse").is_some()));
    d["jobs"]=retained;
    assert_eq!(bindings(&d,AT).unwrap().len(),1,"only current retained ledger and proof restore authority");
}

#[test]
fn different_post_same_verified_bytes_reuses_original_with_separate_target_proof() {
    let (mut d,p,r,l,v)=fixture();let before=l.clone();let original=v["audio"]["materials"][0].clone();
    let donor_strict=crate::knowledge::TranscriptLookup::new(&d,AT).unwrap().strict_media_evidence(&d["posts"][0]).unwrap();
    assert_eq!(donor_strict["screenTextReady"],true);assert_eq!(donor_strict["screenTextHasContent"],true);
    let (material,proof)=selected(&d,&p,&r,&l,&v);
    assert_eq!(material,original);assert_eq!(material["postKey"],"vk:donor");assert_eq!(proof.value["target"]["postKey"],"ig:target");
    assert_eq!(material["transcription"]["sourceVersion"],original["transcription"]["sourceVersion"]);
    warm_pin(&d,&proof.value).unwrap();let job=admit(&mut d,&p,&l,&r,&proof.value,AT).unwrap();
    assert!(job["result"]["proof"].get("normalizedPayload").is_none());assert!(job["result"]["proof"].get("originalMaterial").is_none());
    let bundle=crate::knowledge::select(&d,&[],std::slice::from_ref(&d["posts"][1]),AT).unwrap();
    assert!(rows(&bundle,"materials").iter().any(|m|m["text"]==original["text"]&&m["postKey"]=="vk:donor"&&m["exactFileAnalysisReuse"][0]["targetPostId"]=="target"));
    assert!(!rows(&bundle,"materials").iter().any(|m|m["kind"]=="ocr"&&m["postKey"]=="vk:donor"));
    let strict=crate::knowledge::TranscriptLookup::new(&d,AT).unwrap().strict_media_evidence(&d["posts"][1]).unwrap();
    assert_eq!(strict["audioReady"],true);assert_eq!(strict["screenTextReady"],false);assert_eq!(strict["visualReady"],false);
    assert_eq!(l,before,"selection/admission performs zero new ASR reservation or dispatch");
}
#[test]
fn target_metadata_change_invalidates_old_pin_but_fresh_alias_keeps_zero_asr() {
    let (mut d,mut p,mut r,l,v)=fixture();let before=l.clone();let (_,old)=selected(&d,&p,&r,&l,&v);
    d["posts"][1]["title"]=json!("Revised metadata");d["posts"][1]["sourceUrl"]=json!("https://example.test/revised");
    assert!(validate_pin(&d,&p,&l,&r,&old.value).is_err());
    p["sourceVersion"]=json!(crate::media_fullframes::source_version(&d["posts"][1],"LikeAvto"));r["target"]["sourceVersion"]=p["sourceVersion"].clone();r["target"]["aliasRevision"]=json!(2);
    let (original,fresh)=selected(&d,&p,&r,&l,&v);
    assert_ne!(old.value["target"],fresh.value["target"]);assert_eq!(original,v["audio"]["materials"][0]);assert_eq!(l,before);
}
#[test]
fn foreign_company_and_changed_connector_cannot_apply_same_byte_result() {
    let (d,p,r,l,v)=fixture();for mode in ["company","connector"] {
        let mut changed=d.clone();if mode=="company" {changed["account"]=json!("BAW Russia");} else {
            changed["connectorBinding"]=crate::active_binding(&d).unwrap().to_json();changed["connectorBinding"]["revision"]=json!(2);
        }
        assert!(select_prepared(&changed,&p,&l,&r,&v,&"d".repeat(64)).is_err(),"{mode}");
    }
}
#[test]
fn corrected_donor_head_rejects_selected_receipt_preserving_paid_result() {
    let (mut d,p,r,l,v)=fixture();let before=l.clone();let (_,pin)=selected(&d,&p,&r,&l,&v);
    d["materials"][0]["text"]=json!("Corrected transcript current head");crate::knowledge::sync_catalog(&mut d,AT).unwrap();
    assert!(validate_pin(&d,&p,&l,&r,&pin.value).is_err());assert!(select_prepared(&d,&p,&l,&r,&v,&"d".repeat(64)).is_err());assert_eq!(l,before);
}
#[test]
fn same_audio_different_container_cta_does_not_borrow_original_ocr() {
    let (mut d,mut p,mut r,l,v)=fixture();
    d["posts"][1]["attachments"][0]["caption"]=json!("Different end-card price and CTA");
    p["sourceVersion"]=json!(crate::media_fullframes::source_version(&d["posts"][1],"LikeAvto"));
    r["target"]["sourceVersion"]=p["sourceVersion"].clone();r["target"]["attachmentIdentity"]=json!(attachment_identity(&d["posts"][1]["attachments"][0]));
    p["source"]["sha256"]=json!("e".repeat(64));p["sourceIdentity"]["mediaSha256"]=p["source"]["sha256"].clone();r["verifiedFile"]["sha256"]=p["source"]["sha256"].clone();
    assert!(select_prepared(&d,&p,&l,&r,&v,&"d".repeat(64)).unwrap().is_none());
}
#[test]
fn equal_url_or_title_with_unequal_verified_bytes_is_not_reuse() {
    let (mut d,mut p,mut r,l,v)=fixture();d["posts"][1]=d["posts"][0].clone();d["posts"][1]["id"]=json!("target");d["posts"][1]["postKey"]=json!("ig:target");
    p["sourceVersion"]=json!(crate::media_fullframes::source_version(&d["posts"][1],"LikeAvto"));r["target"]["sourceVersion"]=p["sourceVersion"].clone();r["target"]["attachmentIdentity"]=json!(attachment_identity(&d["posts"][1]["attachments"][0]));
    p["source"]["sha256"]=json!("e".repeat(64));p["sourceIdentity"]["mediaSha256"]=p["source"]["sha256"].clone();r["verifiedFile"]["sha256"]=p["source"]["sha256"].clone();
    assert!(select_prepared(&d,&p,&l,&r,&v,&"d".repeat(64)).unwrap().is_none());
}
#[test]
fn target_attachment_mutation_or_missing_index_does_not_bridge_second_video() {
    let (mut d,p,mut r,l,v)=fixture();d["posts"][1]["attachments"].as_array_mut().unwrap().push(json!({"type":"video","url":"https://example.test/other.mp4"}));
    assert!(select_prepared(&d,&p,&l,&r,&v,&"d".repeat(64)).is_err());r["target"].as_object_mut().unwrap().remove("attachmentIndex");
    assert!(select_prepared(&d,&p,&l,&r,&v,&"d".repeat(64)).is_err());
}
#[test]
fn adoption_keeps_original_metadata_even_after_donor_post_edit() {
    let (mut d,_,r,l,_)=fixture();let before=l.clone();let original=d["materials"][0].clone();
    d["posts"][0]["title"]=json!("Edited donor title");d["posts"][0]["sourceUrl"]=json!("https://example.test/new-locator");
    let adoption=select_catalog_adoption(&d,&r["verifiedFile"],AT).unwrap().unwrap();
    assert_eq!(adoption["material"]["transcription"],original["transcription"]);assert_eq!(adoption["material"]["sourceUrl"],original["sourceUrl"]);
    assert_eq!(adoption["provenance"]["rawOutputsAbsent"],true);assert_eq!(l,before);
}

#[test]
fn no_audio_stream_is_complete_audio_evidence_without_speech_or_screens() {
    let (mut d,p,r,l,v)=fixture_for(true);let (_,proof)=selected(&d,&p,&r,&l,&v);
    warm_pin(&d,&proof.value).unwrap();admit(&mut d,&p,&l,&r,&proof.value,AT).unwrap();
    let strict=crate::knowledge::TranscriptLookup::new(&d,AT).unwrap().strict_media_evidence(&d["posts"][1]).unwrap();
    assert_eq!(strict["audioReady"],true);assert_eq!(strict["audioHasContent"],false);
    assert_eq!(strict["screenTextReady"],false);assert_eq!(strict["visualReady"],false);
}

#[test]
fn direct_current_target_no_text_found_is_sampled_readiness_without_donor_ocr() {
    let (mut d,p,r,l,v)=fixture();let (_,mut proof)=selected(&d,&p,&r,&l,&v);
    let outcome=json!({"account":"LikeAvto","postKey":"ig:target","sourceUrl":d["posts"][1]["sourceUrl"],
        "mediaSha256":r["verifiedFile"]["sha256"],"ocr":{"status":"no_text_found","coverage":"sampled_frames",
            "sourceVersion":p["sourceVersion"],"sampledFrames":3,"failedFrames":0,"exhaustive":false}});
    attach_current_screen_text(&mut proof.value,&outcome).unwrap();warm_pin(&d,&proof.value).unwrap();admit(&mut d,&p,&l,&r,&proof.value,AT).unwrap();
    let strict=crate::knowledge::TranscriptLookup::new(&d,AT).unwrap().strict_media_evidence(&d["posts"][1]).unwrap();
    assert_eq!(strict["screenTextReady"],true);assert_eq!(strict["screenTextHasContent"],false);assert_eq!(strict["visualReady"],false);
    let mut wrong=outcome.clone();wrong["postKey"]=json!("vk:donor");assert!(attach_current_screen_text(&mut proof.value,&wrong).is_err());
    wrong=outcome;wrong["ocr"]["sourceVersion"]=json!("old-version");assert!(attach_current_screen_text(&mut proof.value,&wrong).is_err());
}

#[test]
fn expired_catalog_donor_cannot_be_readded_by_alias_but_unrelated_proof_remains_ready() {
    let (mut d,p,r,l,v)=fixture();
    let expiry=chrono::Utc::now()+chrono::Duration::hours(1);
    d["knowledge_versions"][0]["validUntil"]=json!(expiry.to_rfc3339());
    let mut payload=d["knowledge_versions"][0].clone();for key in ["id","hash","createdAt"] {payload.as_object_mut().unwrap().remove(key);}
    d["knowledge_versions"][0]["hash"]=json!(digest(&payload));
    let (_,proof)=selected(&d,&p,&r,&l,&v);warm_pin(&d,&proof.value).unwrap();admit(&mut d,&p,&l,&r,&proof.value,AT).unwrap();
    let after=(expiry+chrono::Duration::seconds(1)).to_rfc3339();
    assert!(bindings(&d,&after).unwrap().is_empty());
    let bundle=crate::knowledge::select(&d,&[],std::slice::from_ref(&d["posts"][1]),&after).unwrap();
    assert!(!rows(&bundle,"materials").iter().any(|m|m.get("exactFileAnalysisReuse").is_some()));
    let expired=crate::knowledge::TranscriptLookup::new(&d,&after).unwrap().strict_media_evidence(&d["posts"][1]).unwrap();assert_eq!(expired["audioReady"],false);
    let (mut other,p2,r2,l2,v2)=fixture();let (_,valid)=selected(&other,&p2,&r2,&l2,&v2);
    warm_pin(&other,&valid.value).unwrap();admit(&mut other,&p2,&l2,&r2,&valid.value,AT).unwrap();
    let unaffected=crate::knowledge::TranscriptLookup::new(&other,&after).unwrap().strict_media_evidence(&other["posts"][1]).unwrap();assert_eq!(unaffected["audioReady"],true);
    assert_eq!(crate::media_analysis::read_result(&l,&VerifiedAlias::current(&d,&p,&r).unwrap().request(&"d".repeat(64))).unwrap()["disposition"],"reuse","expired catalog does not erase immutable paid output");
}

#[test]
fn attachment_ancillary_numbers_have_cross_language_string_locator_identity() {
    let attachment=json!({"type":"video","url":"https://example.test/exact","width":1.0,"ratio":0.75,"bitrate":1e20});
    let explicit=json!(["video",null,null,"https://example.test/exact",null,null]);
    assert_eq!(attachment_identity(&attachment),digest(&explicit));
}

#[test]
fn unchanged_rewarm_skips_heavy_decode_and_preserves_ready_membership() {
    let (d,p,r,l,v)=fixture();let (_,proof)=selected(&d,&p,&r,&l,&v);
    assert_eq!(test_counts(&proof.value),(0,0));
    warm_pin(&d,&proof.value).unwrap();assert_eq!(test_counts(&proof.value),(1,1));
    warm_pin(&d,&proof.value).unwrap();warm_immutable_pin(&durable(&proof.value)).unwrap();
    assert_eq!(test_counts(&proof.value),(1,1),"unchanged result and ready alias avoid heavy decoding and epoch transitions");
}

#[test]
fn parallel_same_company_result_warm_is_singleflight() {
    let (d,p,r,l,v)=fixture();let (_,proof)=selected(&d,&p,&r,&l,&v);
    let barrier=std::sync::Arc::new(std::sync::Barrier::new(8));let mut workers=Vec::new();
    for _ in 0..8 {
        let barrier=barrier.clone();let pin=durable(&proof.value);
        workers.push(std::thread::spawn(move || {barrier.wait();warm_immutable_pin(&pin)}));
    }
    for worker in workers {worker.join().unwrap().unwrap();}
    assert_eq!(test_counts(&proof.value),(1,1));
}

#[test]
fn changed_actual_manifest_raw_output_invalidates_all_aliases_sharing_result() {
    let (mut d,p,r,l,v)=fixture();let (_,proof)=selected(&d,&p,&r,&l,&v);
    warm_pin(&d,&proof.value).unwrap();admit(&mut d,&p,&l,&r,&proof.value,AT).unwrap();
    let mut second=d["posts"][1].clone();second["id"]=json!("target-second");second["postKey"]=json!("ig:target-second");
    second["title"]=json!("Another verified cross-post");d["posts"].as_array_mut().unwrap().push(second.clone());
    let mut p2=crate::media_fullframes::initial("LikeAvto",&r["target"]["connectorBinding"],&second,AT);
    p2["source"]=p["source"].clone();p2["sourceIdentity"]=p["sourceIdentity"].clone();p2["sourceIdentity"]["postKey"]=second["postKey"].clone();
    let mut r2=r.clone();r2["target"]["postId"]=second["id"].clone();r2["target"]["postKey"]=second["postKey"].clone();
    r2["target"]["sourceVersion"]=p2["sourceVersion"].clone();
    let (_,proof2)=selected(&d,&p2,&r2,&l,&v);warm_pin(&d,&proof2.value).unwrap();admit(&mut d,&p2,&l,&r2,&proof2.value,AT).unwrap();
    assert_eq!(bindings(&d,AT).unwrap().len(),2);
    let store=crate::media_fullframes::store().unwrap();
    let manifest=crate::media_fullframes::read(&store,&proof.value["result"]["manifest"]).unwrap();
    let raw=&manifest["segments"][0]["rawOutput"];
    let path=crate::media_fullframes::verify_reference(&store,raw).unwrap();
    // capture_segment binds the raw payload to this fixture's unique attempt;
    // never corrupt the deterministic shared retained source fixture.
    let original=std::fs::read(&path).unwrap();let mut changed=original.clone();changed.push(b' ');
    std::fs::write(&path,&changed).unwrap();
    assert!(warm_immutable_pin(&durable(&proof.value)).is_err());
    assert!(bindings(&d,AT).unwrap().is_empty(),"rewarming only the first pin invalidates both aliases through their shared closure");
    std::fs::write(&path,&original).unwrap();
    warm_pin(&d,&proof.value).unwrap();assert_eq!(bindings(&d,AT).unwrap().len(),1);
}

#[test]
fn deleting_full_normalized_output_invalidates_warmed_applicability() {
    let (mut d,p,r,l,v)=fixture();let (_,proof)=selected(&d,&p,&r,&l,&v);
    warm_pin(&d,&proof.value).unwrap();admit(&mut d,&p,&l,&r,&proof.value,AT).unwrap();
    let store=crate::media_fullframes::store().unwrap();
    let path=crate::media_fullframes::verify_reference(&store,&proof.value["result"]["normalizedOutput"]).unwrap();
    let original=std::fs::read(&path).unwrap();std::fs::remove_file(&path).unwrap();
    assert!(warm_immutable_pin(&durable(&proof.value)).is_err());assert!(bindings(&d,AT).unwrap().is_empty());
    std::fs::write(&path,&original).unwrap();
}

#[test]
fn cached_closure_contains_source_full_outputs_and_actual_segment_outputs() {
    let (d,p,r,l,v)=fixture();let (_,proof)=selected(&d,&p,&r,&l,&v);
    let verified=cached_immutable(&proof.value).unwrap();let store=crate::media_fullframes::store().unwrap();
    let manifest=crate::media_fullframes::read(&store,&proof.value["result"]["manifest"]).unwrap();
    let mut expected=vec![json!({"sha256":r["verifiedFile"]["sha256"],"bytes":r["verifiedFile"]["bytes"]}),
        proof.value["result"]["manifest"].clone(),proof.value["result"]["normalizedOutput"].clone()];
    for segment in manifest["segments"].as_array().unwrap() {expected.push(segment["rawOutput"].clone());expected.push(segment["normalizedOutput"].clone());}
    for reference in expected {
        let path=crate::media_fullframes::verify_reference(&store,&reference).unwrap();
        assert!(verified.files.iter().any(|(cached,_)|cached==&path));
    }
    assert_eq!(test_counts(&proof.value).0,1);
}

#[test]
fn same_claimed_result_hash_cannot_borrow_modified_refs_or_foreign_company_payload() {
    let (d,p,r,l,v)=fixture();let (_,proof)=selected(&d,&p,&r,&l,&v);warm_pin(&d,&proof.value).unwrap();
    let mut refs=durable(&proof.value);refs["result"]["normalizedOutput"]["sha256"]=json!("e".repeat(64));
    assert!(warm_immutable_pin(&refs).is_err());
    let mut company=durable(&proof.value);company["companyId"]=json!("BAW Russia");company["account"]=json!("BAW Russia");
    assert!(warm_immutable_pin(&company).is_err());assert_eq!(test_counts(&company),(0,0));
    warm_pin(&d,&proof.value).unwrap();assert_eq!(test_counts(&proof.value),(1,1));
}

#[test]
fn cold_real_cas_fixture_recovers_only_after_workspace_warm() {
    let d=test_cold_reused_workspace();assert!(bindings(&d,AT).unwrap().is_empty());
    let job=rows(&d,"jobs").iter().find(|j|j["kind"]=="media_analysis_applicability").unwrap();
    assert_eq!(job["completedAt"],AT);assert_eq!(job["result"]["proof"]["target"]["postId"],"target");
    warm_workspace(&d).unwrap();let bound=bindings(&d,AT).unwrap();assert_eq!(bound.len(),1);
    let strict=crate::knowledge::TranscriptLookup::new(&d,AT).unwrap().strict_media_evidence(&d["posts"][1]).unwrap();
    assert_eq!(strict["audioReady"],true);assert_eq!(strict["screenTextReady"],false);
}

#[cfg(unix)]
#[test]
fn stamp_fast_path_rejects_linked_parent_even_when_file_stamp_matches() {
    use std::os::unix::fs::symlink;
    let root=std::env::temp_dir().join(format!("media-reuse-stamp-{}",uuid::Uuid::new_v4()));
    let parent=root.join("objects");std::fs::create_dir_all(&parent).unwrap();
    let path=parent.join("object");std::fs::write(&path,b"retained").unwrap();let before=file_stamp(&path).unwrap();
    let moved=root.join("moved");std::fs::rename(&parent,&moved).unwrap();symlink(&moved,&parent).unwrap();
    assert!(file_stamp(&path).is_err());assert!(file_stamp(&moved.join("object")).is_ok_and(|stamp|stamp==before));
    std::fs::remove_file(&parent).unwrap();std::fs::remove_file(moved.join("object")).unwrap();
    std::fs::remove_dir(&moved).unwrap();std::fs::remove_dir(&root).unwrap();
}

/// ROOT may use --exact --nocapture to obtain production-selected Rust fixtures
/// for the adapter acceptance seam. No model/provider/process is invoked here.
fn native_selected_adapter_fixture(no_audio:bool)->Value {
    let (mut d,p,r,l,v)=fixture_for(no_audio);let (_,proof)=selected(&d,&p,&r,&l,&v);
    warm_pin(&d,&proof.value).unwrap();admit(&mut d,&p,&l,&r,&proof.value,AT).unwrap();
    d["connectorBinding"]=crate::active_binding(&d).unwrap().to_json();
    d["items"]=json!([{"id":"native-target-item","postId":"target","postKey":"ig:target",
        "branchId":"native-target-branch","text":"A question about this upload","revision":1,"workflow":"attention"}]);
    d["branches"]=json!([{"id":"native-target-branch","postId":"target","contextComplete":true,
        "messages":[{"id":"native-target-item","role":"participant","text":"A question about this upload"}]}]);
    // Exercise the production projection: acquisition and preparation policies
    // occupy separate fields, and target visual status cannot borrow donor OCR.
    let mut evidence=crate::prepare_bundle::EvidenceContext::new(&d).evidence_for_item("native-target-item").unwrap();
    evidence["coverage"]=json!(if no_audio {"no_audio_stream"} else {"full_audio"});
    evidence
}
#[test]
fn native_adapter_fixture_uses_real_preparation_projection_for_both_audio_outcomes() {
    for no_audio in [false,true] {
        let evidence=native_selected_adapter_fixture(no_audio);
        let post=&evidence["posts"][0];
        assert_eq!(post["id"],"target");assert!(post["mediaPolicy"].get("purpose").is_none());
        assert_eq!(post["preparationMediaPolicy"]["purpose"],"preparation");
        assert_eq!(post["visualContextStatus"],"missing");
        let material=rows(&evidence,"materials").iter().find(|m|m["exactFileAnalysisReuse"].is_array()).unwrap();
        let edge=&material["exactFileAnalysisReuse"][0];
        assert_eq!(post["mediaPolicy"]["sourceVersion"],edge["target"]["sourceVersion"]);
        assert_eq!(post["preparationMediaPolicy"]["sourceVersion"],edge["target"]["sourceVersion"]);
        assert_eq!(material["transcription"]["coverage"],evidence["coverage"]);
        assert!(!rows(&evidence,"materials").iter().any(|m|m["kind"]=="ocr"||m["kind"]=="visual_context"));
    }
}
#[test]
#[ignore="ROOT-owned cross-language fixture capture"]
fn emit_native_selected_adapter_fixtures() {
    let fixtures=[false,true].into_iter().map(native_selected_adapter_fixture).collect::<Vec<_>>();
    println!("MEDIA_ANALYSIS_NATIVE_FIXTURES={}",json!(fixtures));
}
