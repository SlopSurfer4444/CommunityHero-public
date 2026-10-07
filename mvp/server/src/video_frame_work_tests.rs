use super::*;
pub(crate) fn first_needs_fixture()->(Value,Value){
    let at="2026-10-06T00:00:00Z";let mut d=crate::media_audio_equivalence::tests::fixture();d["posts"]=json!([d["posts"][1].clone()]);
    d["items"][0]["postId"]=json!("source");d["items"][0]["postKey"]=json!("12185:source");d["items"][0]["objectId"]=json!("12185");d["branches"][0]["postId"]=json!("source");
    let mut bundle=crate::prepare_bundle::build_engine_capture(&d,&[json!("i")],&[]).unwrap();
    bundle["request"]["purpose"]=json!("triage");bundle["request"]["preparationMode"]=json!("single_pass_v1");bundle["request"]["researchLimitContract"]=json!("uncapped_evidence_v1");
    crate::preparation_unit::attach(&d,&mut bundle,at).unwrap();crate::preparation_materials::attach_request(&d,&mut bundle["request"]).unwrap();bundle["digest"]=json!(hash(&bundle["request"]));let request=bundle["request"].clone();
    crate::list_mut(&mut d,"jobs").push(json!({"id":"root","kind":"assistant","purpose":"engine_prepare","status":"running","prepareBundle":bundle,"preparationStages":{"first":null}}));
    let mut result=crate::engine_prepare::tests::single_pass_result(json!({"text":"Need the requested video range","sources":[],"assessments":[{"itemId":"i","outcome":"needs_attention","reason":"The spoken transcript does not identify the visible detail","tags":["missing_context"]}],"proposals":[],
        "videoFrameNeeds":[{"itemId":"i","postId":"source","attachmentIndex":0,"requestedTimeOrIntent":{"kind":"known_range","timelineBasis":"relative_video_start","startMs":1000,"endMs":2000},"reason":"Inspect the visible detail in this range"}]}));
    crate::model_material_receipt::fixture_result(&mut d,"root",&request,&mut result).unwrap();let before=d.clone();crate::preparation_review::record_first(&mut d,"root",&result,at).unwrap();(before,d)
}
#[test]fn first_needs_derivation_replay_is_exact_and_scoped_creation_rejects_forged_pins(){
    let(before,mut after)=first_needs_fixture();let job=crate::row(&after,"jobs","root").unwrap().clone();validate_created(&before,&job).unwrap();
    let original=after.clone();record_needs(&mut after,"root",&job["prepareBundle"]["request"],&job["preparationStages"]["first"]["result"],"different-replay-time").unwrap();assert_eq!(after,original);
    for field in ["companyId","member","asset","parentPaidResultRef","budget","createdAt"]{
        let mut bad=job.clone();bad["videoFrameNeeds"][0][field]=json!("forged");let need=&mut bad["videoFrameNeeds"][0];need.as_object_mut().unwrap().remove("needSha256");need["needSha256"]=json!(hash(need));
        bad["preparationStages"]["first"]["result"]["nativeVideoFrameNeeds"]=bad["videoFrameNeeds"].clone();assert!(validate_created(&before,&bad).is_err(),"{field}");
    }
}
fn fixture()->(Value,Value){
    let mut d=crate::empty();crate::accounts::initialize(&mut d,crate::accounts::Profile::BawRussia).unwrap();
    d["posts"]=json!([{"id":"p","postKey":"12182:p","text":"Video caption","attachments":[{"type":"video","url":"https://example.invalid/video"}]}]);
    let mut need=json!({"schemaVersion":1,"contract":NEED_CONTRACT,"needId":"n","companyId":d["account"],"originatingAnsweringAttemptId":"root","requestingPaidAttemptId":"root",
        "member":{"postId":"p","connectorBinding":d["connectorBinding"]},"asset":{"attachmentIndex":0,"sourceVersion":crate::media_fullframes::source_version(&d["posts"][0],"BAW Russia"),"attachmentIdentity":crate::media_analysis_reuse::attachment_identity(&d["posts"][0]["attachments"][0])},
        "affectedRecipientIds":["i"],"budget":{"maxFrames":8},"requestedTimeOrIntent":{"kind":"known_range","timelineBasis":"relative_video_start","startMs":1000,"endMs":2000}});
    need["needSha256"]=json!(hash(&need));(d,need)
}
#[test]fn requested_range_is_not_source_pts_and_changed_source_fails(){
    let(mut d,need)=fixture();current_need(&d,&need).unwrap();d["posts"][0]["text"]=json!("Changed caption");assert_eq!(current_need(&d,&need),Err("frame_need_source_changed"));
    let(_,mut bad)=fixture();bad["requestedTimeOrIntent"]["endMs"]=json!(100000);bad.as_object_mut().unwrap().remove("needSha256");bad["needSha256"]=json!(hash(&bad));assert_eq!(validate_need(&bad),Err("frame_need_range_invalid"));
}
#[test]fn missing_actual_pts_cannot_be_relabelled_from_requested_time(){
    let(d,need)=fixture();let mut result=json!({"schemaVersion":1,"contract":RESULT_CONTRACT,"needId":need["needId"],"needSha256":need["needSha256"],"companyId":need["companyId"],"member":need["member"],"asset":need["asset"],"status":"complete","requestedTimeOrIntent":need["requestedTimeOrIntent"],"coverage":{"kind":"bounded_range"},"toolVersion":"fixture-only","frames":[{"sha256":"a".repeat(64),"artifact":{"sha256":"a".repeat(64),"bytes":1},"requestedTimestampMs":1000,"timeBase":{"num":1,"den":90000}}]});
    result["resultSha256"]=json!(hash(&result));assert_eq!(validate_result(&d,&need,&result),Err("frame_result_pts_unproven"));
}
