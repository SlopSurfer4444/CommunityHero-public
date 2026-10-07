use super::*;
fn spent()->Value{
    let mut d=crate::empty();crate::accounts::initialize(&mut d,crate::accounts::Profile::BawRussia).unwrap();
    d["jobs"]=json!([{"id":"root","kind":"assistant","preparationStages":{"first":{"status":"completed"},"repairBudget":{"schemaVersion":1,"maxRounds":1,"consumedRounds":1,"authority":"admitted_workflow_v1"},"answeringRepairs":[{"childJobId":"child","originatingAnsweringAttemptId":"root","roundOrdinal":1,"planSha256":"p"}]}},{"id":"child","status":"unknown","originatingAnsweringAttemptId":"root","roundOrdinal":1}]);d
}
fn plan()->Value{let mut p=json!({"schemaVersion":1,"contract":CONTRACT,"companyId":"BAW Russia","originatingAnsweringAttemptId":"root","roundOrdinal":1,"needSet":[],"needSetDigest":hash(&json!([]))});p["planSha256"]=json!(hash(&p));p}
#[test]fn parallel_needs_changed_versions_and_unknown_never_mint_second_round(){
    for change in ["need_a","need_b","result_version","base_version","lease_owner","restart"]{
        let mut d=spent();let before=d.clone();let mut p=plan();p["variant"]=json!(change);p.as_object_mut().unwrap().remove("planSha256");p["planSha256"]=json!(hash(&p));
        assert!(claim_fenced(&mut d,&p,"2026-10-06T00:00:00Z").is_err());assert_eq!(d,before,"{change}");
    }
}
#[test]fn malformed_present_budgets_and_scope_are_not_missing_history(){
    assert!(validate_change(&json!({"jobs":null}),&json!({"jobs":[]})).is_err());
    let mut d=spent();d["jobs"][0]["preparationStages"]["answeringRepairs"]=json!({});assert!(validate_change(&d,&d).is_err());
    let d=spent();let mut after=d.clone();after["jobs"][0]["preparationStages"].as_object_mut().unwrap().remove("repairBudget");assert!(validate_change(&d,&after).is_err());
    after=d.clone();after["jobs"][1].as_object_mut().unwrap().remove("originatingAnsweringAttemptId");assert!(validate_change(&d,&after).is_err());
}
fn fresh()->(Value,Value){
    let at="2026-10-06T00:00:00Z";let mut d=crate::media_audio_equivalence::tests::fixture();d["posts"]=json!([d["posts"][1].clone()]);
    d["items"][0]["postId"]=json!("source");d["items"][0]["postKey"]=json!("12185:source");d["items"][0]["objectId"]=json!("12185");d["branches"][0]["postId"]=json!("source");
    let mut second=d["items"][0].clone();second["id"]=json!("j");second["itemId"]=json!("j");d["items"].as_array_mut().unwrap().push(second);
    let mut bundle=crate::prepare_bundle::build_engine_capture(&d,&[json!("i"),json!("j")],&[]).unwrap();crate::preparation_unit::attach(&d,&mut bundle,at).unwrap();crate::preparation_materials::attach_request(&d,&mut bundle["request"]).unwrap();crate::preparation_materials::require_request(&d,&bundle["request"]).unwrap();
    let mut needs=Vec::new();let mut results=Vec::new();
    for (ordinal,item) in ["i","j"].iter().enumerate(){
        let need_id=format!("n{ordinal}");let job_id=format!("frame{ordinal}");let mut need=json!({"schemaVersion":1,"contract":crate::video_frame_work::NEED_CONTRACT,"needId":need_id,"companyId":d["account"],"originatingAnsweringAttemptId":"root","requestingPaidAttemptId":"root","member":{"postId":"source","connectorBinding":d["connectorBinding"]},"asset":{"attachmentIndex":0,"attachmentIdentity":crate::media_analysis_reuse::attachment_identity(&d["posts"][0]["attachments"][0]),"sourceVersion":crate::media_fullframes::source_version(&d["posts"][0],"BAW Russia"),"sourceArtifactSha256":"a".repeat(64),"sourceArtifactRef":{"sha256":"a".repeat(64),"bytes":1}},"affectedRecipientIds":[item],"budget":{"maxFrames":8},"requestedTimeOrIntent":{"kind":"known_range","timelineBasis":"relative_video_start","startMs":1000,"endMs":2000}});need["needSha256"]=json!(hash(&need));
        let frame=json!({"sha256":"b".repeat(64),"artifact":{"sha256":"b".repeat(64),"bytes":1},"actualPts":1100,"timeBase":{"num":1,"den":1000},"requestedTimestampMs":1000});let mut result=json!({"schemaVersion":1,"contract":crate::video_frame_work::RESULT_CONTRACT,"needId":need_id,"needSha256":need["needSha256"],"companyId":d["account"],"member":need["member"],"asset":need["asset"],"status":"complete","frameJobId":job_id,"requestedTimeOrIntent":need["requestedTimeOrIntent"],"coverage":{"exhaustive":false},"toolVersion":"synthetic-pure-only","frames":[frame],"decoderResult":{"status":"complete","frames":[frame]}});result["resultSha256"]=json!(hash(&result));
        crate::list_mut(&mut d,"jobs").push(json!({"id":job_id,"kind":"media","purpose":"targeted_video_frames","status":"completed","frameNeed":need,"frameResult":result}));needs.push(need);results.push(result);
    }
    bundle["digest"]=json!(hash(&bundle["request"]));crate::list_mut(&mut d,"jobs").push(json!({"id":"root","kind":"assistant","purpose":"engine_prepare","status":"running","prepareBundle":bundle,"prepareOutcome":{"status":"needs_attention","candidates":[]},"preparationStages":{"first":{"status":"completed"}},"videoFrameNeeds":needs}));
    let mut request=bundle["request"].clone();request["optionalFrameRefs"]=json!(needs.iter().flat_map(|n|crate::video_frame_work::frame_refs(results.iter().find(|r|r["needId"]==n["needId"]).unwrap(),n)).collect::<Vec<_>>());
    let plan=capture(&d,"root",&needs,&results,&request,at).unwrap();(d,plan)
}
#[test]fn one_atomic_round_fans_in_two_needs_and_unknown_preserves_spent_identity(){
    let(mut d,plan)=fresh();let before=d.clone();let reserved=claim_fenced(&mut d,&plan,"2026-10-06T00:00:00Z").unwrap();validate_change(&before,&d).unwrap();
    crate::db_guards::validate_change(&before,&d).unwrap();
    for fault in ["unknown_alias","root_claim","child_intent"]{
        let mut changed=d.clone();
        match fault{
            "unknown_alias"=>{let target=changed["items"][0].clone();crate::list_mut(&mut changed,"operations").push(json!({"id":"alias-unknown","itemId":"another-canonical-row","status":"unknown","target":target}));},
            "root_claim"=>crate::row_mut(&mut changed,"jobs","root").unwrap()["preparationStages"]["answeringRepairs"][0]["childJobId"]=json!("foreign-child"),
            _=>crate::row_mut(&mut changed,"jobs",reserved["jobId"].as_str().unwrap()).unwrap()["repairPaidIntent"]["retryAuthorized"]=json!(true),
        }
        assert!(crate::db_guards::validate_change(&before,&changed).is_err(),"{fault}");
    }
    let child=reserved["jobId"].as_str().unwrap().to_owned();let mut old=crate::row(&d,"jobs",&child).unwrap().clone();let mut next=old.clone();next["repairPaidIntent"]["status"]=json!("dispatching");assert!(validate_job_change(&json!({"jobs":[old]}),&json!({"jobs":[next]})).is_ok());old=next;next=old.clone();next["repairPaidIntent"]["status"]=json!("unknown");assert!(validate_job_change(&json!({"jobs":[old]}),&json!({"jobs":[next]})).is_ok());
    crate::row_mut(&mut d,"jobs",&child).unwrap()["status"]=json!("unknown");let after=d.clone();assert!(claim_fenced(&mut d,&plan,"later").is_err());assert_eq!(d,after);assert_eq!(crate::row(&d,"jobs","root").unwrap()["preparationStages"]["repairBudget"]["consumedRounds"],1);
    let root=crate::row(&d,"jobs","root").unwrap().clone();let mut progressing=root.clone();progressing["frameNeedOutcome"]=json!({"status":"complete"});assert!(validate_job_change(&json!({"jobs":[root]}),&json!({"jobs":[progressing]})).is_ok());progressing["preparationStages"]["repairBudget"]["consumedRounds"]=json!(0);assert!(validate_job_change(&json!({"jobs":[root]}),&json!({"jobs":[progressing]})).is_err());
}
#[test]fn omitted_need_recipient_or_frame_cannot_admit_a_partial_repair(){
    let(d,plan)=fresh();let needs=rows(&plan,"needSet");let results=rows(&plan,"frameResults");let request=&plan["request"];
    assert!(capture(&d,"root",&needs[..1],&results[..1],request,"at").is_err());let mut bad=request.clone();bad["items"].as_array_mut().unwrap().pop();assert!(capture(&d,"root",needs,results,&bad,"at").is_err());bad=request.clone();bad["optionalFrameRefs"].as_array_mut().unwrap().pop();assert!(capture(&d,"root",needs,results,&bad,"at").is_err());
}
#[test]fn foreign_paid_response_and_foreign_merged_outcome_cannot_escape_affected_set(){
    let(mut d,plan)=fresh();let request=&plan["request"];
    let mut result=json!({"text":"Exact held decisions","sources":[],"assessments":[{"itemId":"i","outcome":"needs_attention","reason":"Context gap","tags":["missing_context"]},{"itemId":"j","outcome":"needs_attention","reason":"Context gap","tags":["missing_context"]}],"proposals":[]});
    assert!(crate::preparation_review::plan_review(request,&result).is_ok());result["assessments"][1]["itemId"]=json!("foreign");assert!(crate::preparation_review::plan_review(request,&result).is_err());
    let child=claim_fenced(&mut d,&plan,"at").unwrap()["jobId"].as_str().unwrap().to_owned();let job=crate::row_mut(&mut d,"jobs",&child).unwrap();job["status"]=json!("completed");job["prepareOutcome"]=json!({"affectedRecipientIds":["i","j"],"candidates":[{"itemId":"foreign","status":"rejected"}],"finalAssessments":[]});
    let before=d.clone();assert!(merge_outcome(&mut d,"root",json!({"candidates":[]})).is_err());assert_eq!(d,before);
}
#[test]fn native_repair_permit_exempts_only_frozen_origin_and_never_unknown_operation(){
    let(mut d,plan)=fresh();crate::row_mut(&mut d,"jobs","root").unwrap()["purpose"]=json!("engine_prepare");
    crate::row_mut(&mut d,"jobs","root").unwrap()["prepareOutcome"]=json!({"status":"needs_attention","candidates":[]});
    let child=claim_fenced(&mut d,&plan,"at").unwrap()["jobId"].as_str().unwrap().to_owned();crate::row_mut(&mut d,"jobs",&child).unwrap()["repairPaidIntent"]["status"]=json!("dispatching");
    crate::preparation_reservations::assert_available(&d,&["i".into(),"j".into()],Some(&child)).unwrap();
    d["operations"]=json!([{"id":"unknown","itemId":"i","status":"unknown"}]);assert!(crate::preparation_reservations::assert_available(&d,&["i".into()],Some(&child)).is_err());
    d["operations"]=json!([]);let job=crate::row_mut(&mut d,"jobs",&child).unwrap();job["answeringRepairPlan"]["affectedRecipientIds"]=json!(["foreign"]);assert!(crate::preparation_reservations::assert_available(&d,&["i".into()],Some(&child)).is_err());
}
#[test]fn auto_repair_merge_preserves_unrelated_originals_and_derived_tags_do_not_stale_saved_context(){
    let(mut d,plan)=fresh();let child=claim_fenced(&mut d,&plan,"at").unwrap()["jobId"].as_str().unwrap().to_owned();let repaired=json!({"repairJobId":child,"affectedRecipientIds":["i","j"],"candidates":[{"itemId":"i","proposalId":"saved","status":"review"}],
        "finalAssessments":[{"itemId":"i","outcome":"reply","reason":"Frame detail now supplied","tags":["question"]},{"itemId":"j","outcome":"needs_attention","reason":"Still bounded missing context","tags":["missing_context"]}]});
    let job=crate::row_mut(&mut d,"jobs",&child).unwrap();job["status"]=json!("completed");job["prepareOutcome"]=repaired;job["prepareOutcome"]["status"]=json!("review");
    let root=crate::row_mut(&mut d,"jobs","root").unwrap();root["purpose"]=json!("auto_prepare");
    for item in crate::list_mut(&mut d,"items"){item["autoPreparation"]=json!({"jobId":"root","status":"needs_attention","attempts":1});item["triageTags"]=json!(["missing_context"]);}
    d["items"][0]["workflow"]=json!("prepared");d["items"][0]["revision"]=json!(2);
    let source=crate::prepare_bundle::review_fingerprint(&d,"i").unwrap();d["proposals"]=json!([{"id":"saved","itemId":"i","itemRevision":2,"prepareRunId":child,"reviewContextDigest":source,"status":"draft","revision":1,"kind":"reply_and_close","text":"Saved native repair draft"}]);
    let affected=rows(&crate::row(&d,"jobs",&child).unwrap()["prepareOutcome"],"affectedRecipientIds").to_vec();let pins=settlement_pins(&d,&affected).unwrap();crate::row_mut(&mut d,"jobs",&child).unwrap()["prepareOutcome"]["settlementPins"]=json!(pins);
    let candidates=rows(&crate::row(&d,"jobs",&child).unwrap()["prepareOutcome"],"candidates").to_vec();let proposal_pins=proposal_pins(&d,&candidates).unwrap();crate::row_mut(&mut d,"jobs",&child).unwrap()["prepareOutcome"]["proposalSettlementPins"]=json!(proposal_pins);
    let untouched=json!({"itemId":"unrelated","status":"prepared","reason":"Original"});let original=json!({"status":"needs_attention","items":[{"itemId":"i","status":"needs_attention"},{"itemId":"j","status":"needs_attention"},untouched],"admission":{"candidates":[],"held":[{"itemId":"i"}]}});
    let outcome=merge_outcome(&mut d,"root",original.clone()).unwrap();assert_eq!(outcome["items"][2],untouched);assert_eq!(outcome["items"][0]["status"],"prepared");assert_eq!(d["items"][0]["autoPreparation"]["status"],"prepared");assert_eq!(d["items"][0]["autoPreparation"]["attempts"],1);assert_eq!(d["items"][0]["autoPreparation"]["jobId"],"root");assert_eq!(crate::prepare_bundle::review_fingerprint(&d,"i").unwrap(),source);
    let settled=d.clone();let replay=merge_outcome(&mut d,"root",outcome.clone()).unwrap();assert_eq!(replay,outcome,"same frozen child may report its admitted outcome again");assert_eq!(d,settled,"completed merge replay changes no projection");
    d["items"][0]["draftEdited"]=json!(true);let before=d.clone();assert!(merge_outcome(&mut d,"root",outcome).is_err());assert_eq!(d,before,"human draft wins the settlement-to-merge race");
}
#[test]fn pending_resume_unknown_child_preserves_one_spent_round_and_exact_digests(){
    let(mut d,plan)=fresh();crate::row_mut(&mut d,"jobs","root").unwrap()["purpose"]=json!("engine_prepare");let child=claim_fenced(&mut d,&plan,"at").unwrap()["jobId"].as_str().unwrap().to_owned();
    let job=crate::row_mut(&mut d,"jobs",&child).unwrap();job["status"]=json!("unknown");job["repairPaidIntent"]["status"]=json!("unknown");
    let root=crate::row_mut(&mut d,"jobs","root").unwrap();root["status"]=json!("interrupted");root["prepareOutcome"]=json!({"status":"needs_attention","candidates":[]});let request_digest=root["prepareBundle"]["digest"].as_str().unwrap().to_owned();let needs_digest=hash(&root["videoFrameNeeds"]);
    let token=crate::runtime_lifecycle::OwnerToken{account:"BAW Russia".into(),runtime_id:"fixture-owner".into(),release_sha256:"e".repeat(64),epoch:1};let ledger=crate::runtime_lifecycle::ledger_digest(&d).unwrap();crate::runtime_lifecycle::initialize(&mut d,token.clone(),&"f".repeat(64),&ledger).unwrap();
    let before=d.clone();assert!(claim_pending_resume(&mut d,&token,"root",&request_digest,&"a".repeat(64),"at").is_err());assert_eq!(d,before);
    claim_pending_resume(&mut d,&token,"root",&request_digest,&needs_digest,"at").unwrap();assert_eq!(crate::row(&d,"jobs",&child).unwrap(),crate::row(&before,"jobs",&child).unwrap());assert_eq!(crate::row(&d,"jobs","root").unwrap()["preparationStages"]["repairBudget"]["consumedRounds"],1);
    let after=d.clone();assert!(claim_fenced(&mut d,&plan,"at").is_err());assert_eq!(d,after);
}
#[test]fn unknown_operation_blocks_paid_claim_before_budget_or_child_and_reserved_dispatch_permit(){
    let(mut d,plan)=fresh();d["operations"]=json!([{"id":"unknown","itemId":"i","status":"unknown"}]);let before=d.clone();assert!(claim_fenced(&mut d,&plan,"at").is_err());assert_eq!(d,before);
    d["operations"]=json!([]);let child=claim_fenced(&mut d,&plan,"at").unwrap()["jobId"].as_str().unwrap().to_owned();
    crate::preparation_reservations::assert_available(&d,&["i".into(),"j".into()],Some(&child)).unwrap();
    d["operations"]=json!([{"id":"late","itemId":"j","status":"dispatching"}]);assert!(crate::preparation_reservations::assert_available(&d,&["i".into(),"j".into()],Some(&child)).is_err());
}
#[test]fn no_proposal_settlement_pin_rejects_new_operator_decision_and_stale_reply_cannot_apply(){
    let(mut d,plan)=fresh();let child=claim_fenced(&mut d,&plan,"at").unwrap()["jobId"].as_str().unwrap().to_owned();
    let affected=json!(["i","j"]);let pins=settlement_pins(&d,affected.as_array().unwrap()).unwrap();
    let job=crate::row_mut(&mut d,"jobs",&child).unwrap();job["status"]=json!("completed");job["prepareOutcome"]=json!({"status":"discussed","repairJobId":child,"affectedRecipientIds":affected,"settlementPins":pins,"proposalSettlementPins":[],"candidates":[],"finalAssessments":[{"itemId":"i","outcome":"needs_attention","reason":"Hold","tags":["missing_context"]},{"itemId":"j","outcome":"needs_attention","reason":"Hold","tags":["missing_context"]}]});
    let original=json!({"status":"needs_attention","candidates":[]});d["items"][0]["decision"]=json!("operator-choice");let before=d.clone();assert!(merge_outcome(&mut d,"root",original.clone()).is_err());assert_eq!(d,before);
    crate::row_mut(&mut d,"jobs",&child).unwrap()["prepareOutcome"]["status"]=json!("stale");let before=d.clone();assert!(merge_outcome(&mut d,"root",original).is_err());assert_eq!(d,before);
}
#[test]fn repair_paid_owner_is_original_and_cannot_be_substituted_or_removed(){
    let(mut d,plan)=fresh();let child=claim_fenced(&mut d,&plan,"at").unwrap()["jobId"].as_str().unwrap().to_owned();let job=crate::row(&d,"jobs",&child).unwrap();
    let paid=json!({"company":"baw-russia","account":"BAW Russia","runtimeOwner":{"account":"BAW Russia","runtimeId":"isolated-material-fixture","releaseSha256":"e".repeat(64)}});require_paid_owner(job,&paid).unwrap();
    for (field,value) in [("account",json!("baw-russia")),("account",json!("LikeAvto")),("runtimeId",json!("another-runtime")),("releaseSha256",json!("f".repeat(64)))]{let mut foreign=paid.clone();foreign["runtimeOwner"][field]=value;assert!(require_paid_owner(job,&foreign).is_err(),"{field}");}
    for field in ["company","account"]{let mut foreign=paid.clone();foreign[field]=json!("foreign");assert!(require_paid_owner(job,&foreign).is_err(),"{field}");}
    let old=job.clone();let mut changed=old.clone();changed["repairPaidIntent"].as_object_mut().unwrap().remove("owner");assert!(validate_job_change(&json!({"jobs":[old]}),&json!({"jobs":[changed]})).is_err());
}
#[test]fn grouped_revalidation_cannot_consume_a_singleton_repair_round(){
    let(mut d,plan)=fresh();crate::row_mut(&mut d,"jobs","root").unwrap()["purpose"]=json!("auto_revalidate");
    let before=d.clone();assert!(claim_fenced(&mut d,&plan,"at").is_err());assert_eq!(d,before);
}
#[test]fn no_proposal_paid_return_cannot_pin_a_recipient_revision_changed_during_call(){
    let(mut d,plan)=fresh();let child=claim_fenced(&mut d,&plan,"at").unwrap()["jobId"].as_str().unwrap().to_owned();
    crate::row_mut(&mut d,"jobs",&child).unwrap()["repairPaidIntent"]["status"]=json!("dispatching");
    let mut result=crate::engine_prepare::tests::single_pass_result(json!({"text":"Still requires a person","sources":[],"assessments":[{"itemId":"i","outcome":"needs_attention","reason":"Hold","tags":["missing_context"]},{"itemId":"j","outcome":"needs_attention","reason":"Hold","tags":["missing_context"]}],"proposals":[]}));
    crate::model_material_receipt::fixture_result(&mut d,&child,&plan["request"],&mut result).unwrap();
    d["items"][0]["revision"]=json!(99);let before=d.clone();let error=settle_child(&mut d,&child,&result).unwrap_err();assert_eq!(error.1,"Repair recipient changed during paid attempt; saved result remains preserved");assert_eq!(d,before);assert_eq!(crate::row(&d,"jobs",&child).unwrap()["retainedEvidence"].as_array().unwrap().len(),1);
}
#[test]fn persisted_repair_proposal_edit_or_retirement_prevents_merge_with_unchanged_item(){
    let(mut d,plan)=fresh();let child=claim_fenced(&mut d,&plan,"at").unwrap()["jobId"].as_str().unwrap().to_owned();
    d["proposals"]=json!([{"id":"saved","itemId":"i","itemRevision":d["items"][0]["revision"],"prepareRunId":child,"status":"draft","revision":1,"kind":"reply_and_close","text":"Native saved reply"}]);
    let candidates=json!([{"itemId":"i","proposalId":"saved","status":"review"}]);let affected=json!(["i","j"]);let item_pins=settlement_pins(&d,affected.as_array().unwrap()).unwrap();let saved_pins=proposal_pins(&d,candidates.as_array().unwrap()).unwrap();
    let job=crate::row_mut(&mut d,"jobs",&child).unwrap();job["status"]=json!("completed");job["prepareOutcome"]=json!({"status":"review","repairJobId":child,"affectedRecipientIds":affected,"settlementPins":item_pins,"proposalSettlementPins":saved_pins,"candidates":candidates,"finalAssessments":[]});
    for action in ["edit","stale","retired","approved"]{
        let mut changed=d.clone();if action=="edit"{crate::edit_proposal(&mut changed,"saved",&json!({"expectedRevision":1,"text":"Operator edited reply"})).unwrap();}else{changed["proposals"][0]["status"]=json!(action);}
        assert_eq!(changed["items"],d["items"],"real proposal edit/status does not bump item");let before=changed.clone();assert!(merge_outcome(&mut changed,"root",json!({"candidates":[]})).is_err(),"{action}");assert_eq!(changed,before,"{action}");
    }
}

// These synthetic tests retain actual PRIVATE CAS paid receipts and use native
// first/child/proposal reducers. No decoder/model/provider transport is invoked.
fn revalidation_stamp(timestamp:i64)->String{
    chrono::DateTime::from_timestamp(timestamp,0).unwrap().to_rfc3339_opts(chrono::SecondsFormat::Secs,true)
}
fn capture_revalidation_result(d:&mut Value,job:&str,raw:Value,visual:bool)->Value{
    capture_revalidation_result_wire(d,job,raw,visual,false)
}
fn capture_revalidation_result_wire(d:&mut Value,job:&str,raw:Value,visual:bool,wrapped:bool)->Value{
    let request=crate::row(d,"jobs",job).unwrap()["prepareBundle"]["request"].clone();
    let mut result=crate::engine_prepare::tests::single_pass_result(raw);
    if let Some(contract)=request.get("researchLimitContract"){result["runMetadata"]["researchLimitContract"]=contract.clone();}
    else{result["runMetadata"].as_object_mut().unwrap().remove("researchLimitContract");}
    if crate::decision_media::enabled(&request){
        result["runMetadata"]["decisionMediaContract"]=json!(crate::decision_media::CONTRACT);
        for entry in result["editorialEvidence"]["entries"].as_array_mut().unwrap(){
            entry["mediaDependency"]=json!({"audio":"independent","visual":if visual{"required"}else{"independent"}});
        }
    }
    if wrapped{capture_wrapped_material_result(d,job,&request,&mut result);}
    else{crate::model_material_receipt::fixture_result(d,job,&request,&mut result).unwrap();}result
}
fn capture_wrapped_material_result(d:&mut Value,job:&str,request:&Value,result:&mut Value){
    // Reuse the existing fixture's exact delivery-body construction on a
    // private scratch state. The real job receives only the genuine wrapped
    // CAS capture below; no original paid history is rewritten or removed.
    let mut scratch=d.clone();crate::model_material_receipt::fixture_result(&mut scratch,job,request,result).unwrap();
    let original=result.as_object_mut().unwrap().remove("modelMaterialReceipt").unwrap();
    let profile=crate::accounts::Profile::from_workspace(d).unwrap();
    let wire=json!({"account":profile.key(),"operation":"assistant","request":request});
    static CAS:std::sync::OnceLock<tempfile::TempDir>=std::sync::OnceLock::new();
    let store=crate::media_artifacts::ArtifactStore::open(CAS.get_or_init(||tempfile::tempdir().unwrap()).path()).unwrap();
    let record=json!({"version":2,"kind":"retained-native-paid-stage-result","company":profile.key(),"account":profile.display(),
        "runtimeOwner":original["paidResultRef"]["runtimeOwner"],"binding":{"nativeJobId":job,"operation":"assistant"},
        "requestSha256":hash(&wire),"responseSha256":hash(result),"request":wire,"response":result,"retryAuthorized":false,"dispatchAuthorized":false});
    let artifact=store.put_bytes(record.to_string().as_bytes()).unwrap();
    let paid=json!({"version":1,"kind":"native-paid-capture-ref","company":record["company"],"account":record["account"],
        "binding":record["binding"],"runtimeOwner":record["runtimeOwner"],"requestSha256":record["requestSha256"],
        "responseSha256":record["responseSha256"],"artifact":artifact.to_json(),"retryAuthorized":false,"dispatchAuthorized":false});
    let resolved=crate::runtime_paid_result::resolve_from(&store,profile.key(),profile.display(),Some(job),"assistant",&paid).unwrap();
    assert!(crate::preparation_review::first_capture_matches(profile,request,&resolved).unwrap());
    let body=original["body"].clone();
    let receipt_record=json!({"schemaVersion":1,"contract":crate::preparation_materials::CONTRACT,"request":wire,"body":body,"paidResultRef":paid});
    let receipt_artifact=store.put_bytes(receipt_record.to_string().as_bytes()).unwrap();
    let mut pointer=json!({"schemaVersion":1,"contract":crate::preparation_materials::CONTRACT,"companyId":profile.display(),
        "nativeJobId":job,"bodySha256":hash(&body),"body":body,"paidResultRef":paid,"artifact":receipt_artifact.to_json()});
    pointer["pointerSha256"]=json!(hash(&pointer));
    let native=crate::row_mut(d,"jobs",job).unwrap();if native.get("retainedEvidence").is_none(){native["retainedEvidence"]=json!([]);}
    native["retainedEvidence"].as_array_mut().unwrap().push(paid);
    crate::model_material_receipt::attach(d,job,&pointer).unwrap();result["modelMaterialReceipt"]=pointer;
}
fn revalidation_raw(prepared:bool,frames:bool)->Value{
    let mut result=if prepared{json!({"text":"Reviewed synthetic frame detail","sources":[],"assessments":[{"itemId":"i","outcome":"reply","reason":"Exact synthetic frame now supplied","tags":["question"]}],"proposals":[{"itemId":"i","kind":"reply_and_close","text":"Saved synthetic repair reply"}]})}
        else{json!({"text":"Still requires context","sources":[],"assessments":[{"itemId":"i","outcome":"needs_attention","reason":"Still missing bounded context","tags":["missing_context"]}],"proposals":[]})};
    if frames{result["videoFrameNeeds"]=json!([{"itemId":"i","postId":"source","attachmentIndex":0,"requestedTimeOrIntent":{"kind":"known_range","timelineBasis":"relative_video_start","startMs":1000,"endMs":2000},"reason":"Inspect the addressed bounded frame detail"}]);}result
}
pub(crate) fn fresh_revalidation()->(Value,String,Value){
    fresh_revalidation_wire(false)
}
fn fresh_revalidation_wire(wrapped:bool)->(Value,String,Value){
    let(mut d,now)=revalidation_base();
    let (previous,_)=crate::auto_prepare::claim(&mut d,now).unwrap().unwrap();
    let first=capture_revalidation_result_wire(&mut d,&previous,revalidation_raw(true,false),false,wrapped);
    settle_initial_revalidation_fixture(&mut d,&previous,&first,now);
    change_revalidation_fixture_source(&mut d,now);
    let(origin,_)=crate::auto_prepare::claim(&mut d,now+32).unwrap().unwrap();
    assert_eq!(crate::row(&d,"jobs",&origin).unwrap()["purpose"],"auto_revalidate");
    let held=capture_revalidation_result_wire(&mut d,&origin,revalidation_raw(false,true),false,wrapped);
    settle_held_revalidation_fixture(&mut d,&origin,&held,now+32);
    let plan=revalidation_frame_plan_fixture(&mut d,&origin);(d,origin,plan)
}
pub(crate) fn revalidation_base()->(Value,i64){
    let(mut d,_)=fresh();d["items"]=json!([d["items"][0].clone()]);d["jobs"]=json!([]);
    // A pristine SQLite reader normalizes these native collections. A source
    // fixture must retain that empty history when it replaces the placeholder.
    for collection in ["knowledge_entries","knowledge_versions","feedback"] {
        if d.get(collection).is_none(){d[collection]=json!([]);}
    }
    let now=chrono::Utc::now().timestamp();d["items"][0]["platform"]=json!("vk");
    d["items"][0]["providerObservedAt"]=json!(revalidation_stamp(now));d["items"][0]["createdAt"]=json!(revalidation_stamp(now-60));
    (d,now)
}
fn settle_initial_revalidation_fixture(d:&mut Value,previous:&str,first:&Value,now:i64){
    let request=crate::row(d,"jobs",previous).unwrap()["prepareBundle"]["request"].clone();
    crate::preparation_review::settle_first(d,previous,&request,first,&revalidation_stamp(now)).unwrap();
    assert_eq!(crate::auto_prepare::complete(d,previous,first,now).unwrap()["status"],"prepared");
    crate::row_mut(d,"jobs",previous).unwrap()["status"]=json!("completed");
}
fn change_revalidation_fixture_source(d:&mut Value,now:i64){
    let mut branch=d["branches"][0].clone();
    branch["messages"][0]["text"]=json!("New source asks for a bounded frame detail");
    // Retain the changed provider observation through the actual source reducer.
    // Editing assembled messages alone is undone by the next source merge.
    crate::merge_snapshot(d,&json!({"branches":[branch]})).unwrap();
    crate::auto_prepare::reconcile_stale(d,now+1);assert_eq!(d["proposals"][0]["status"],"stale");
    d["settings"]["autoPreparation"]["revalidation"]=json!({"enabled":true,"debounceSeconds":30});
    assert!(crate::auto_prepare::claim(d,now+1).unwrap().is_none());
}
fn settle_held_revalidation_fixture(d:&mut Value,origin:&str,held:&Value,at:i64){
    let request=crate::row(d,"jobs",origin).unwrap()["prepareBundle"]["request"].clone();
    crate::preparation_review::settle_first(d,origin,&request,held,&revalidation_stamp(at)).unwrap();
    assert_eq!(crate::auto_prepare::complete(d,origin,held,at).unwrap()["status"],"needs_attention");
}
fn revalidation_frame_plan_fixture(d:&mut Value,origin:&str)->Value{
    let needs=rows(crate::row(d,"jobs",origin).unwrap(),"videoFrameNeeds").to_vec();assert_eq!(needs.len(),1);
    let need=&needs[0];let frame_id=crate::id();
    let frame=json!({"sha256":"b".repeat(64),"artifact":{"sha256":"b".repeat(64),"bytes":1},"mime":"image/png","width":1,"height":1,"actualPts":1100,"timeBase":{"num":1,"den":1000},"requestedTimestampMs":1000});
    let mut frame_result=json!({"schemaVersion":1,"contract":crate::video_frame_work::RESULT_CONTRACT,"needId":need["needId"],"needSha256":need["needSha256"],"companyId":d["account"],"member":need["member"],"asset":need["asset"],"status":"complete","frameJobId":frame_id,"requestedTimeOrIntent":need["requestedTimeOrIntent"],"coverage":{"exhaustive":false},"toolVersion":"synthetic-native-revalidation-fixture","frames":[frame],"decoderResult":{"status":"complete","frames":[frame]}});
    frame_result["resultSha256"]=json!(hash(&frame_result));
    crate::list_mut(d,"jobs").push(json!({"id":frame_id,"kind":"media","purpose":"targeted_video_frames","status":"completed","frameNeed":need,"frameResult":frame_result}));
    let mut bundle=crate::prepare_bundle::triage(d,"i").unwrap();
    bundle["request"]["previousDecision"]=crate::row(d,"jobs",origin).unwrap()["prepareBundle"]["request"]["previousDecision"].clone();
    bundle["request"]["optionalFrameRefs"]=json!(crate::video_frame_work::frame_refs(&frame_result,need));
    crate::decision_media::attach_request(d,&mut bundle["request"]).unwrap();
    capture(d,origin,&needs,&[frame_result],&bundle["request"],&crate::now()).unwrap()
}
/// Build historical scopes in their actual order through the full native
/// database writer. Each callback receives a committed full state so a paired
/// backend can replay the SAME identities through every guarded transition.
/// Importing only the final afterimage would falsely introduce old and
/// replacement paid reservations simultaneously and is rightly rejected.
pub(crate) async fn fresh_revalidation_sqlite(db:&crate::Database,wrapped:bool)->(String,Value){
    assert!(matches!(db,crate::Database::Sqlite(_)),"isolated SQLite fixture only");
    let(base,now)=revalidation_base();
    fresh_revalidation_database(db,base,now,wrapped,&mut |_,_|{}).await
}
pub(crate) async fn fresh_revalidation_database(db:&crate::Database,base:Value,now:i64,wrapped:bool,
    checkpoint:&mut impl FnMut(&str,Value))->(String,Value){
    assert_eq!(base["account"],"BAW Russia");
    assert!(base.get("runtimeLifecycle").is_none(),"fixture starts before any native owner or paid work");
    db.change(|state|{
        for collection in ["jobs","proposals","operations","approvals"] {
            assert!(rows(state,collection).is_empty(),"fixture requires pristine {collection}");
        }
        *state=base;Ok(())
    }).await.expect("source-only BAW fixture must pass the actual writer");
    checkpoint("source_only_baw",db.read().await.unwrap());
    // This is exactly the existing isolated paid-CAS fixture owner. Establish
    // it before the first claim; no paid pointer or owner is later rebound.
    let token=crate::runtime_lifecycle::OwnerToken{account:"BAW Russia".into(),
        runtime_id:"isolated-material-fixture".into(),release_sha256:"e".repeat(64),epoch:1};
    db.change_runtime_lifecycle_with_ledger(|state|{
        let ledger=crate::runtime_lifecycle::ledger_digest(state)?;
        crate::runtime_lifecycle::initialize(state,token.clone(),&"f".repeat(64),&ledger)
    }).await.expect("initialize the exact test-only native paid owner before admission");
    checkpoint("initialize_native_owner",db.read().await.unwrap());
    let previous=db.change(|state|{
        let job=crate::auto_prepare::claim(state,now)?.expect("initial native claim").0;
        crate::preparation_review::record_initial_admission(state,&token,&job,&revalidation_stamp(now))?;Ok(job)
    })
        .await.expect("initial scope is created before any paid result");
    checkpoint("claim_initial",db.read().await.unwrap());
    reserve_revalidation_fixture_first(db,&token,&previous,now).await;
    checkpoint("reserve_initial_first",db.read().await.unwrap());
    let first=db.change(|state|Ok(capture_revalidation_result_wire(state,&previous,revalidation_raw(true,false),false,wrapped)))
        .await.expect("append exact initial paid CAS/material receipt");
    checkpoint("capture_initial_paid",db.read().await.unwrap());
    db.change(|state|{settle_initial_revalidation_fixture(state,&previous,&first,now);Ok(())})
        .await.expect("settle existing initial reservation through native FIRST/generation");
    checkpoint("settle_initial",db.read().await.unwrap());
    db.change(|state|{change_revalidation_fixture_source(state,now);Ok(())})
        .await.expect("retire the previous native draft after the real source change");
    checkpoint("observe_changed_source",db.read().await.unwrap());
    let origin=db.change(|state|{
        let before_ids=rows(state,"jobs").iter().map(|job|job["id"].clone()).collect::<Vec<_>>();
        let job=crate::auto_prepare::claim(state,now+32)?.expect("revalidation native claim").0;
        let stored=crate::row(state,"jobs",&job)?;
        assert_eq!(stored["purpose"],"auto_revalidate");
        assert!(!before_ids.contains(&json!(job)),"only the newly appended revalidation job may gain initial stages");
        assert!(stored.get("preparationStages").is_none());
        assert!(stored.get("retainedEvidence").is_none()&&stored.get("modelMaterialReceipts").is_none());
        // Match scheduler::commit_captured: unlike the grouped initial claim,
        // this native claim leaves the new job's stage bookkeeping absent.
        crate::row_mut(state,"jobs",&job)?["preparationStages"]=json!({"first":null,"review":null});
        let request=&crate::row(state,"jobs",&job)?["prepareBundle"]["request"];
        crate::preparation_unit::current_request(state,request,&revalidation_stamp(now+32)).map_err(crate::conflict)?;
        crate::preparation_review::record_initial_admission(state,&token,&job,&revalidation_stamp(now+32))?;Ok(job)
    })
        .await.expect("replacement reservation follows the already settled prior scope");
    checkpoint("claim_revalidation",db.read().await.unwrap());
    reserve_revalidation_fixture_first(db,&token,&origin,now+32).await;
    checkpoint("reserve_revalidation_first",db.read().await.unwrap());
    let held=db.change(|state|{
        assert_eq!(crate::row(state,"jobs",&origin)?["purpose"],"auto_revalidate");
        Ok(capture_revalidation_result_wire(state,&origin,revalidation_raw(false,true),false,wrapped))
    }).await.expect("append exact revalidation paid CAS/material receipt");
    checkpoint("capture_revalidation_paid",db.read().await.unwrap());
    db.change(|state|{settle_held_revalidation_fixture(state,&origin,&held,now+32);Ok(())})
        .await.expect("persist actual held FIRST and its native frame need");
    checkpoint("settle_revalidation_held",db.read().await.unwrap());
    // Decoder output remains the existing explicitly synthetic frame fixture;
    // claim/paid/settlement scopes and their persistence guards are all real.
    let plan=db.change(|state|Ok(revalidation_frame_plan_fixture(state,&origin)))
        .await.expect("append the synthetic frame observation to its existing native need");
    checkpoint("observe_synthetic_requested_frames",db.read().await.unwrap());
    (origin,plan)
}
async fn reserve_revalidation_fixture_first(db:&crate::Database,token:&crate::runtime_lifecycle::OwnerToken,job:&str,at:i64){
    db.change(|state|{
        let request=crate::row(state,"jobs",job)?["prepareBundle"]["request"].clone();
        crate::preparation_review::reserve_first_admitted(state,token,job,&request,&revalidation_stamp(at))
    }).await.expect("real single-use FIRST admission precedes every paid fixture capture");
}
async fn claim_revalidation_child_sqlite(db:&crate::Database,plan:&Value,prepared:bool,further:bool,wrapped:bool)->(String,Value){
    claim_revalidation_child_database(db,plan,prepared,further,wrapped,&mut |_,_|{}).await
}
async fn claim_revalidation_child_database(db:&crate::Database,plan:&Value,prepared:bool,further:bool,wrapped:bool,
    checkpoint:&mut impl FnMut(&str,Value))->(String,Value){
    let child=db.change(|state|Ok(claim_fenced(state,plan,&crate::now())?["jobId"].as_str().unwrap().to_owned()))
        .await.expect("atomic native child reservation precedes its paid capture");
    checkpoint("claim_repair_child",db.read().await.unwrap());
    let result=db.change(|state|{
        crate::row_mut(state,"jobs",&child)?["repairPaidIntent"]["status"]=json!("dispatching");
        Ok(capture_revalidation_result_wire(state,&child,revalidation_raw(prepared,further),prepared,wrapped))
    }).await.expect("append exact child paid capture to the existing reservation");
    checkpoint("capture_repair_child_paid",db.read().await.unwrap());
    (child,result)
}
pub(crate) async fn settle_revalidation_child_sqlite(db:&crate::Database,origin:&str,plan:&Value,prepared:bool,further:bool)->String{
    settle_revalidation_child_database(db,origin,plan,prepared,further,&mut |_,_|{}).await
}
pub(crate) async fn settle_revalidation_child_database(db:&crate::Database,origin:&str,plan:&Value,prepared:bool,further:bool,
    checkpoint:&mut impl FnMut(&str,Value))->String{
    let(child,result)=claim_revalidation_child_database(db,plan,prepared,further,false,checkpoint).await;
    let outcome=db.change(|state|settle_child(state,&child,&result)).await.expect("native paid child settlement");
    assert_eq!(outcome["status"],if prepared{"review"}else{"discussed"});
    let state=db.read().await.unwrap();assert!(settled_child(&state,origin).unwrap().is_some());
    checkpoint("settle_repair_child",state);child
}
pub(crate) fn settle_revalidation_child(d:&mut Value,origin:&str,plan:&Value,prepared:bool,further:bool)->String{
    let before_claim=d.clone();
    let child=claim_fenced(d,plan,&crate::now()).unwrap()["jobId"].as_str().unwrap().to_owned();
    crate::db_guards::validate_change(&before_claim,d).expect("native singleton repair claim must pass the actual persistence guard");
    crate::row_mut(d,"jobs",&child).unwrap()["repairPaidIntent"]["status"]=json!("dispatching");
    let result=capture_revalidation_result(d,&child,revalidation_raw(prepared,further),prepared);
    let before_settlement=d.clone();
    let outcome=settle_child(d,&child,&result).unwrap();assert_eq!(outcome["status"],if prepared{"review"}else{"discussed"});
    crate::db_guards::validate_change(&before_settlement,d).expect("native singleton paid repair settlement must pass the actual persistence guard");
    assert!(settled_child(d,origin).unwrap().is_some());child
}
pub(crate) fn merge_revalidation(d:&mut Value,origin:&str)->Value{
    let original=crate::row(d,"jobs",origin).unwrap()["prepareOutcome"].clone();merge_outcome(d,origin,original).unwrap()
}
#[test]fn repair_paid_request_digest_accepts_only_exact_native_direct_or_wrapper(){
    let(d,origin,_)=fresh_revalidation();let job=crate::row(&d,"jobs",&origin).unwrap();let request=&job["prepareBundle"]["request"];
    let profile=crate::accounts::Profile::from_workspace(&d).unwrap();let paid=&job["modelMaterialReceipts"][0]["paidResultRef"];
    assert!(repair_paid_request_matches(profile,request,paid).unwrap());
    let mut wrapped=json!({"account":profile.key(),"operation":"assistant","request":request});
    let mut pointer=paid.clone();pointer["requestSha256"]=json!(hash(&wrapped));
    assert!(repair_paid_request_matches(profile,request,&pointer).unwrap());
    for fault in ["inner","account","operation","extra"]{
        let mut wire=wrapped.clone();match fault{
            "inner"=>wire["request"]["instruction"]=json!("Another request"),
            "account"=>wire["account"]=json!("likeavto"),
            "operation"=>wire["operation"]=json!("readback"),
            _=>wire["extra"]=json!(true),
        }
        pointer["requestSha256"]=json!(hash(&wire));assert!(!repair_paid_request_matches(profile,request,&pointer).unwrap(),"{fault}");
    }
    wrapped["request"]=Value::Null;pointer["requestSha256"]=json!(hash(&wrapped));
    assert!(!repair_paid_request_matches(profile,request,&pointer).unwrap());
}
#[tokio::test]async fn sqlite_native_repair_generation_and_unknown_wrapped_recovery_keep_premerge_action_hold(){
    for wrapped in [false,true]{
        let temp=tempfile::tempdir().unwrap();let path=temp.path().join("native-repair.sqlite");
        let db=crate::Database::Sqlite(crate::open_db(&path).await.unwrap());
        let(origin,plan)=fresh_revalidation_sqlite(&db,wrapped).await;
        let original=crate::row(&db.read().await.unwrap(),"jobs",&origin).unwrap().clone();
        let(child,result)=claim_revalidation_child_sqlite(&db,&plan,true,false,wrapped).await;
        if wrapped{db.change(|state|{let job=crate::row_mut(state,"jobs",&child)?;job["status"]=json!("unknown");job["repairPaidIntent"]["status"]=json!("unknown");Ok(())}).await.unwrap();}
        let paid_before=crate::row(&db.read().await.unwrap(),"jobs",&child).unwrap().clone();
        assert_eq!(db.change(|state|settle_child(state,&child,&result)).await.unwrap()["status"],"review");
        let settled=db.read().await.unwrap();let saved=settled["proposals"][1].clone();
        assert_eq!(saved["editorialReview"]["decision"],"accept");
        assert!(crate::preparation_reservations::assert_proposal(&settled,&saved).is_err());
        let review=crate::editorial_review::plan_new(&settled,&json!([{"id":saved["id"],"revision":saved["revision"]}]),&crate::now()).unwrap();
        assert_eq!(rows(&review,"held").len(),1);assert!(rows(&review,"batches").is_empty());
        let completed=crate::row(&settled,"jobs",&child).unwrap();
        for field in ["retainedEvidence","modelMaterialReceipts","answeringRepairPlan","scopeReservation"]{assert_eq!(completed[field],paid_before[field],"{field}");}
        assert_eq!(completed["repairPaidIntent"]["owner"],paid_before["repairPaidIntent"]["owner"]);
        db.close().await;
        let db=crate::Database::Sqlite(crate::open_db(&path).await.unwrap());
        let merged=db.change(|state|{let old=crate::row(state,"jobs",&origin)?["prepareOutcome"].clone();merge_outcome(state,&origin,old)}).await.unwrap();
        assert_eq!(merged["status"],"prepared");let final_state=db.read().await.unwrap();
        crate::preparation_reservations::assert_proposal(&final_state,&saved).unwrap();
        for field in ["retainedEvidence","modelMaterialReceipts"]{assert_eq!(crate::row(&final_state,"jobs",&origin).unwrap()[field],original[field]);}
        assert_eq!(crate::row(&final_state,"jobs",&origin).unwrap()["preparationStages"]["first"],original["preparationStages"]["first"]);
        assert_eq!(crate::row(&final_state,"jobs",&origin).unwrap()["preparationStages"]["repairBudget"]["consumedRounds"],1);
        assert_eq!(final_state["proposals"],settled["proposals"]);assert!(rows(&final_state,"approvals").is_empty());assert!(rows(&final_state,"operations").is_empty());
        db.close().await;
    }
}
#[tokio::test]async fn sqlite_repair_generation_permit_rejects_changed_native_candidates_and_rolls_back_paid_history(){
    let temp=tempfile::tempdir().unwrap();let db=crate::Database::Sqlite(crate::open_db(&temp.path().join("repair-rollback.sqlite")).await.unwrap());
    let(origin,plan)=fresh_revalidation_sqlite(&db,false).await;
    let(child,result)=claim_revalidation_child_sqlite(&db,&plan,true,false,false).await;
    let before=db.read().await.unwrap();
    for fault in ["decision_contract_missing","decision_contract_wrong","generation_metadata","knowledge_manifest","knowledge_policy",
        "candidate_text","candidate_kind","candidate_id","duplicate_candidate","item_transition","source","route","child_paid_closure","root_paid_closure",
        "alias_unknown","foreign_reservation","paid_response"]{
        let mut returned=result.clone();if fault=="paid_response"{returned["text"]=json!("Not the immutable paid response");}
        let attempt=db.change(|state|crate::prepare_bundle::admit_to_with_created(state,&child,&returned,|created,ids|{
            assert_eq!(ids.len(),1,"the fixture must reach real native proposal creation");let id=&ids[0];
            assert_eq!(crate::row(created,"proposals",id)?["decisionMediaContract"],crate::decision_media::CONTRACT);
            match fault{
                "decision_contract_missing"=>{crate::row_mut(created,"proposals",id)?.as_object_mut().unwrap().remove("decisionMediaContract");},
                "decision_contract_wrong"=>crate::row_mut(created,"proposals",id)?["decisionMediaContract"]=json!("unsupported"),
                "generation_metadata"=>crate::row_mut(created,"proposals",id)?["generationMetadata"]["model"]=json!("other"),
                "knowledge_manifest"=>crate::row_mut(created,"proposals",id)?["knowledgeManifest"]=json!([{"entryId":"foreign","versionId":"foreign"}]),
                "knowledge_policy"=>crate::row_mut(created,"proposals",id)?["knowledgePolicyVersion"]=json!("foreign"),
                "candidate_text"=>crate::row_mut(created,"proposals",id)?["text"]=json!("Changed exact candidate"),
                "candidate_kind"=>crate::row_mut(created,"proposals",id)?["kind"]=json!("close"),
                "candidate_id"=>crate::row_mut(created,"proposals",id)?["id"]=json!("not-native-id"),
                "duplicate_candidate"=>{let duplicate=crate::row(created,"proposals",id)?.clone();crate::list_mut(created,"proposals").push(duplicate);},
                "item_transition"=>created["items"][0]["draft"]=json!("Human draft"),
                "source"=>created["branches"][0]["messages"][0]["text"]=json!("Changed source after creation"),
                "route"=>created["items"][0]["objectId"]=json!("other-provider-object"),
                "child_paid_closure"=>crate::row_mut(created,"jobs",&child)?["retainedEvidence"]=json!([]),
                "root_paid_closure"=>crate::row_mut(created,"jobs",&origin)?["retainedEvidence"]=json!([]),
                "alias_unknown"=>{let target=created["items"][0].clone();crate::list_mut(created,"operations").push(json!({"id":"alias-unknown","itemId":"old-local-alias","status":"unknown","target":target}));},
                "foreign_reservation"=>{let bundle=crate::row(created,"jobs",&child)?["prepareBundle"].clone();
                    crate::list_mut(created,"jobs").push(json!({"id":"foreign-owner","kind":"assistant","purpose":"engine_prepare","status":"running","prepareBundle":bundle}));
                    let scope=crate::preparation_reservations::capture(created,"foreign-owner")?;crate::row_mut(created,"jobs","foreign-owner")?["scopeReservation"]=scope;},
                _=>(),
            }Ok(())
        })).await;
        assert!(attempt.is_err(),"{fault}");assert_eq!(db.read().await.unwrap(),before,"{fault}: actual writer must roll back candidate, owner and paid history");
    }
    db.close().await;
}
#[test]fn revalidation_repair_projects_existing_child_and_preserves_original_paid_and_saved_draft(){
    let(mut d,origin,plan)=fresh_revalidation();let old=d["proposals"][0].clone();
    let before=crate::row(&d,"jobs",&origin).unwrap().clone();let previous=plan["request"]["previousDecision"].clone();
    assert_eq!(previous, before["prepareBundle"]["request"]["previousDecision"]);
    let child=settle_revalidation_child(&mut d,&origin,&plan,true,false);let saved=d["proposals"][1].clone();let revision=d["items"][0]["revision"].clone();
    assert_eq!(saved["editorialReview"]["decision"],"accept","native paid generation reuse must finish before merge");
    assert!(crate::preparation_reservations::assert_proposal(&d,&saved).is_err(),"generation permit cannot admit ordinary action before merge");
    let ordinary=crate::editorial_review::plan_new(&d,&json!([{"id":saved["id"],"revision":saved["revision"]}]),&crate::now()).unwrap();
    assert_eq!(ordinary["held"].as_array().unwrap().len(),1);assert!(ordinary["batches"].as_array().unwrap().is_empty());
    let outcome=merge_revalidation(&mut d,&origin);assert_eq!(outcome["status"],"prepared");assert_eq!(outcome["admission"]["candidates"][0]["proposalId"],saved["id"]);
    crate::preparation_reservations::assert_proposal(&d,&saved).expect("merged native draft becomes available to ordinary review");
    assert_eq!(d["proposals"],json!([old,saved.clone()]));assert_eq!(d["items"][0]["revision"],revision);
    assert_eq!(d["items"][0]["autoPreparation"]["jobId"],origin);assert_eq!(d["items"][0]["autoPreparation"]["attempts"],1);
    assert_eq!(d["items"][0]["autoPreparation"]["requiresReview"],false);assert_eq!(d["items"][0]["autoRevalidation"]["jobId"],origin);
    assert_eq!(d["items"][0]["autoRevalidation"]["pendingDigest"],before["sourceDigest"]);assert_eq!(d["items"][0]["autoRevalidation"]["status"],"completed");
    let root=crate::row(&d,"jobs",&origin).unwrap();assert_eq!(root["preparationStages"]["first"],before["preparationStages"]["first"]);
    assert_eq!(root["retainedEvidence"],before["retainedEvidence"]);assert_eq!(root["modelMaterialReceipts"],before["modelMaterialReceipts"]);
    assert_eq!(saved["prepareRunId"],child);assert_eq!(automatic_proposal_origin(&d,&saved),Some(origin.clone()));
    crate::proposal_current(&d,&saved).expect("the actual admitted repair remains current after projection");
    assert!(rows(&d,"approvals").is_empty());assert!(rows(&d,"operations").is_empty());
    d=serde_json::from_str(&d.to_string()).unwrap();let settled=d.clone();assert_eq!(merge_revalidation(&mut d,&origin),outcome);assert_eq!(d,settled);
}
#[test]fn revalidation_repair_held_result_is_finite_and_further_frames_never_reset_round(){
    let(mut d,origin,plan)=fresh_revalidation();let old=d["proposals"].clone();let child=settle_revalidation_child(&mut d,&origin,&plan,false,true);
    assert_eq!(merge_revalidation(&mut d,&origin)["status"],"needs_attention");assert_eq!(d["proposals"],old);
    assert_eq!(d["items"][0]["autoPreparation"]["requiresReview"],true);assert_eq!(d["items"][0]["workflow"],"attention");
    assert_eq!(crate::row(&d,"jobs",&child).unwrap()["frameNeedOutcome"]["reasonCode"],"answering_repair_limit_exhausted");
    let before=d.clone();assert!(claim_fenced(&mut d,&plan,"later").is_err());assert_eq!(d,before);
    assert_eq!(crate::row(&d,"jobs",&origin).unwrap()["preparationStages"]["repairBudget"]["consumedRounds"],1);
}
#[test]fn revalidation_repair_currentness_and_human_races_preserve_admitted_evidence(){
    let(d,origin,plan)=fresh_revalidation();
    for change in ["preparation_pointer","preparation_status","review_pointer","pending_digest","review_status","source","draft","edited","override","operation","foreign_proposal"]{
        let mut changed=d.clone();match change{
            "preparation_pointer"=>changed["items"][0]["autoPreparation"]["jobId"]=json!("newer"),
            "preparation_status"=>changed["items"][0]["autoPreparation"]["status"]=json!("stale"),
            "review_pointer"=>changed["items"][0]["autoRevalidation"]["jobId"]=json!("newer"),
            "pending_digest"=>changed["items"][0]["autoRevalidation"]["pendingDigest"]=json!("changed"),
            "review_status"=>changed["items"][0]["autoRevalidation"]["status"]=json!("requested"),
            "source"=>changed["branches"][0]["messages"][0]["text"]=json!("New source"),
            "draft"=>changed["items"][0]["draft"]=json!("Saved human draft"),
            "edited"=>changed["items"][0]["draftEdited"]=json!(true),
            "override"=>changed["items"][0]["autoPreparation"]["humanOverrideAt"]=json!(crate::now()),
            "operation"=>changed["operations"]=json!([{"id":"uncertain","itemId":"i","status":"unknown"}]),
            _=>crate::list_mut(&mut changed,"proposals").push(json!({"id":"human","itemId":"i","status":"draft","text":"Saved human choice"})),
        }let before=changed.clone();assert!(claim_fenced(&mut changed,&plan,&crate::now()).is_err(),"{change}");assert_eq!(changed,before,"{change}");
    }
    let mut admitted=d;let child=settle_revalidation_child(&mut admitted,&origin,&plan,true,false);
    for change in ["preparation_pointer","preparation_status","review_pointer","pending_digest","review_status","source","draft","edited","override","operation","proposal_edit","proposal_approved","proposal_retired","foreign_proposal"]{
        let mut changed=admitted.clone();match change{
            "preparation_pointer"=>changed["items"][0]["autoPreparation"]["jobId"]=json!("newer"),
            "preparation_status"=>changed["items"][0]["autoPreparation"]["status"]=json!("stale"),
            "review_pointer"=>changed["items"][0]["autoRevalidation"]["jobId"]=json!("newer"),
            "pending_digest"=>changed["items"][0]["autoRevalidation"]["pendingDigest"]=json!("changed"),
            "review_status"=>changed["items"][0]["autoRevalidation"]["status"]=json!("requested"),
            "source"=>changed["branches"][0]["messages"][0]["text"]=json!("New source"),
            "draft"=>changed["items"][0]["draft"]=json!("Saved human draft"),
            "edited"=>changed["items"][0]["draftEdited"]=json!(true),
            "override"=>changed["items"][0]["autoPreparation"]["humanOverrideAt"]=json!(crate::now()),
            "operation"=>changed["operations"]=json!([{"id":"uncertain","itemId":"i","status":"unknown"}]),
            "proposal_edit"=>{let id=changed["proposals"][1]["id"].as_str().unwrap().to_owned();crate::edit_proposal(&mut changed,&id,&json!({"expectedRevision":1,"text":"Human saved edit"})).unwrap();},
            "proposal_approved"=>changed["proposals"][1]["status"]=json!("approved"),
            "proposal_retired"=>changed["proposals"][1]["status"]=json!("retired"),
            _=>crate::list_mut(&mut changed,"proposals").push(json!({"id":"human","itemId":"i","status":"draft","text":"Saved human choice"})),
        }let original=crate::row(&changed,"jobs",&origin).unwrap()["prepareOutcome"].clone();let before=changed.clone();assert!(merge_outcome(&mut changed,&origin,original).is_err(),"{change}");assert_eq!(changed,before,"{change}");assert_eq!(crate::row(&changed,"jobs",&child).unwrap()["retainedEvidence"],crate::row(&admitted,"jobs",&child).unwrap()["retainedEvidence"]);
    }
}
#[test]fn repaired_automatic_draft_stales_then_next_source_revalidates_without_retargeting_child(){
    let(mut d,origin,plan)=fresh_revalidation();let child=settle_revalidation_child(&mut d,&origin,&plan,true,false);merge_revalidation(&mut d,&origin);
    crate::row_mut(&mut d,"jobs",&origin).unwrap()["status"]=json!("completed");let saved=d["proposals"][1].clone();let now=chrono::Utc::now().timestamp();
    d["branches"][0]["messages"][0]["text"]=json!("Another independently changed source detail");
    crate::auto_prepare::reconcile_stale(&mut d,now+40);assert_eq!(d["proposals"][1]["status"],"stale");assert_eq!(d["proposals"][1]["prepareRunId"],child);
    assert_eq!(d["items"][0]["autoPreparation"]["jobId"],origin);assert_eq!(d["items"][0]["autoPreparation"]["savedProposalId"],saved["id"]);assert_eq!(d["items"][0]["workflow"],"attention");
    assert!(crate::auto_prepare::claim(&mut d,now+41).unwrap().is_none());let(next,request)=crate::auto_prepare::claim(&mut d,now+72).unwrap().expect("proven retired child frees exactly the old settled workflow scope");
    assert_ne!(next,origin);assert_eq!(crate::row(&d,"jobs",&next).unwrap()["purpose"],"auto_revalidate");assert_eq!(request["previousDecision"]["prepareRunId"],child);assert_eq!(request["previousDecision"]["proposalId"],saved["id"]);
    assert_eq!(request["previousDecision"]["text"],saved["text"]);assert_eq!(crate::row(&d,"jobs",&origin).unwrap()["preparationStages"]["repairBudget"]["consumedRounds"],1);
    let next_result=capture_revalidation_result(&mut d,&next,revalidation_raw(true,false),false);
    crate::preparation_review::record_first(&mut d,&next,&next_result,&revalidation_stamp(now+73)).unwrap();
    assert_eq!(crate::auto_prepare::complete(&mut d,&next,&next_result,now+73).unwrap()["status"],"prepared");
    assert_eq!(d["proposals"].as_array().unwrap().len(),3);assert_eq!(d["proposals"][1]["text"],saved["text"]);assert_eq!(d["proposals"][1]["prepareRunId"],child);
    assert!(rows(&d,"approvals").is_empty());assert!(rows(&d,"operations").is_empty());
}
#[test]fn revalidation_pending_resume_keeps_unknown_child_and_original_epoch_round(){
    let(mut d,origin,plan)=fresh_revalidation();let child=claim_fenced(&mut d,&plan,&crate::now()).unwrap()["jobId"].as_str().unwrap().to_owned();
    let job=crate::row_mut(&mut d,"jobs",&child).unwrap();job["status"]=json!("unknown");job["repairPaidIntent"]["status"]=json!("unknown");
    crate::row_mut(&mut d,"jobs",&origin).unwrap()["status"]=json!("interrupted");let root=crate::row(&d,"jobs",&origin).unwrap().clone();
    let token=crate::runtime_lifecycle::OwnerToken{account:"BAW Russia".into(),runtime_id:"fixture-owner".into(),release_sha256:"e".repeat(64),epoch:1};let ledger=crate::runtime_lifecycle::ledger_digest(&d).unwrap();crate::runtime_lifecycle::initialize(&mut d,token.clone(),&"f".repeat(64),&ledger).unwrap();
    let request=root["prepareBundle"]["digest"].as_str().unwrap();let needs=hash(&root["videoFrameNeeds"]);let before=d.clone();let mut old_epoch=token.clone();old_epoch.epoch=2;
    assert!(claim_pending_resume(&mut d,&old_epoch,&origin,request,&needs,&crate::now()).is_err());assert_eq!(d,before);
    assert!(claim_pending_resume(&mut d,&token,&origin,request,&"a".repeat(64),&crate::now()).is_err());assert_eq!(d,before);
    assert_eq!(claim_pending_resume(&mut d,&token,&origin,request,&needs,&crate::now()).unwrap(),"auto_revalidate");
    assert_eq!(crate::row(&d,"jobs",&child).unwrap(),crate::row(&before,"jobs",&child).unwrap());let resumed=d.clone();assert!(claim_fenced(&mut d,&plan,&crate::now()).is_err());assert_eq!(d,resumed);
    assert_eq!(crate::row(&d,"jobs",&origin).unwrap()["preparationStages"]["repairBudget"]["consumedRounds"],1);
}
#[test]fn completed_revalidation_merge_replay_rejects_pointer_and_paid_closure_forgery(){
    let(mut d,origin,plan)=fresh_revalidation();let child=settle_revalidation_child(&mut d,&origin,&plan,true,false);merge_revalidation(&mut d,&origin);
    for change in ["pointer","source","root_budget","child_outcome","paid_owner","paid_evidence","proposal_content"]{
        let mut changed=d.clone();match change{
            "pointer"=>changed["items"][0]["autoRevalidation"]["jobId"]=json!("newer"),
            "source"=>changed["branches"][0]["messages"][0]["text"]=json!("New source"),
            "root_budget"=>crate::row_mut(&mut changed,"jobs",&origin).unwrap()["preparationStages"]["repairBudget"]["consumedRounds"]=json!(0),
            "child_outcome"=>crate::row_mut(&mut changed,"jobs",&child).unwrap()["prepareOutcome"]["finalAssessments"][0]["reason"]=json!("Forged outcome"),
            "paid_owner"=>crate::row_mut(&mut changed,"jobs",&child).unwrap()["repairPaidIntent"]["owner"]["runtimeId"]=json!("foreign"),
            "paid_evidence"=>crate::row_mut(&mut changed,"jobs",&child).unwrap()["retainedEvidence"]=json!([]),
            _=>changed["proposals"][1]["text"]=json!("Changed paid content"),
        }let original=crate::row(&changed,"jobs",&origin).unwrap()["prepareOutcome"].clone();let before=changed.clone();assert!(merge_outcome(&mut changed,&origin,original).is_err(),"{change}");assert_eq!(changed,before,"{change}");
    }
}
#[test]fn unknown_revalidation_repair_child_keeps_overlapping_scope_reserved_after_root_completed(){
    let(mut d,origin,plan)=fresh_revalidation();let child=claim_fenced(&mut d,&plan,&crate::now()).unwrap()["jobId"].as_str().unwrap().to_owned();
    let job=crate::row_mut(&mut d,"jobs",&child).unwrap();job["status"]=json!("unknown");job["repairPaidIntent"]["status"]=json!("unknown");
    crate::row_mut(&mut d,"jobs",&origin).unwrap()["status"]=json!("completed");
    let mut sibling=d["items"][0].clone();sibling["id"]=json!("overlap");sibling["itemId"]=json!("overlap");
    sibling["autoPreparation"]=Value::Null;sibling["autoRevalidation"]=Value::Null;
    crate::list_mut(&mut d,"items").push(sibling);d["branches"][0]["messages"].as_array_mut().unwrap().push(json!({"id":"overlap","text":"An overlapping new comment"}));
    assert!(crate::preparation_reservations::assert_available(&d,&["overlap".into()],None).is_err());
    let jobs=d["jobs"].clone();assert!(crate::auto_prepare::claim(&mut d,chrono::Utc::now().timestamp()+45).unwrap().is_none());assert_eq!(d["jobs"],jobs);
    assert_eq!(crate::row(&d,"jobs",&child).unwrap()["repairPaidIntent"]["status"],"unknown");
    assert_eq!(crate::row(&d,"jobs",&origin).unwrap()["preparationStages"]["repairBudget"]["consumedRounds"],1);
}
#[test]fn repair_projection_preserves_alias_unknown_and_foreign_reservations(){
    let(mut d,origin,plan)=fresh_revalidation();let child=settle_revalidation_child(&mut d,&origin,&plan,true,false);
    crate::preparation_reservations::assert_repair_projection(&d,&origin,&child,&["i".into()]).unwrap();
    for change in ["alias_unknown","foreign_job","foreign_paid_proposal","child_scope","root_route","outside_recipient"]{
        let mut changed=d.clone();match change{
            "alias_unknown"=>changed["operations"]=json!([{"id":"alias-unknown","itemId":"historical-local-id","status":"unknown","target":changed["items"][0]}]),
            "foreign_job"=>{
                let bundle=crate::row(&changed,"jobs",&child).unwrap()["prepareBundle"].clone();
                crate::list_mut(&mut changed,"jobs").push(json!({"id":"foreign-native-scope","kind":"assistant","purpose":"engine_prepare","status":"running","prepareBundle":bundle}));
                let saved=crate::preparation_reservations::capture(&changed,"foreign-native-scope").unwrap();
                crate::row_mut(&mut changed,"jobs","foreign-native-scope").unwrap()["scopeReservation"]=saved;
            },
            "foreign_paid_proposal"=>crate::list_mut(&mut changed,"proposals").push(json!({"id":"foreign-paid","itemId":"historical-local-id","status":"draft","paidGeneration":true,"routeTarget":d["items"][0]})),
            "child_scope"=>crate::row_mut(&mut changed,"jobs",&child).unwrap().as_object_mut().unwrap().remove("scopeReservation").map(|_|()).unwrap(),
            "root_route"=>changed["items"][0]["conversationKey"]=json!("changed-route"),
            _=>(),
        }
        let ids=if change=="outside_recipient"{vec!["outside".to_owned()]}else{vec!["i".to_owned()]};
        let before=changed.clone();assert!(crate::preparation_reservations::assert_repair_projection(&changed,&origin,&child,&ids).is_err(),"{change}");assert_eq!(changed,before,"{change}");
    }
}
#[test]fn merged_repair_candidate_remains_reviewable_and_human_edit_never_authorizes_automatic_reuse(){
    let(mut d,origin,plan)=fresh_revalidation();let child=settle_revalidation_child(&mut d,&origin,&plan,true,false);merge_revalidation(&mut d,&origin);
    crate::row_mut(&mut d,"jobs",&origin).unwrap()["status"]=json!("completed");
    let saved=d["proposals"][1].clone();crate::preparation_reservations::assert_proposal(&d,&saved).unwrap();
    assert!(crate::preparation_reservations::assert_available(&d,&["i".into()],None).is_err(),"existing paid draft still holds generation");
    let id=saved["id"].as_str().unwrap();crate::edit_proposal(&mut d,id,&json!({"expectedRevision":saved["revision"],"text":"Human edited the saved repair draft"})).unwrap();
    let edited=crate::row(&d,"proposals",id).unwrap().clone();
    crate::preparation_reservations::assert_proposal(&d,&edited).expect("ordinary human review keeps exact native candidate lineage");
    assert_eq!(automatic_proposal_origin(&d,&edited),None,"edited content never becomes machine-retirement authority");
    d["branches"][0]["messages"][0]["text"]=json!("Another source change");
    crate::auto_prepare::reconcile_stale(&mut d,chrono::Utc::now().timestamp()+40);
    assert_eq!(crate::row(&d,"proposals",id).unwrap()["status"],"draft","human edit is preserved");
    assert_eq!(crate::row(&d,"jobs",&origin).unwrap()["preparationStages"]["repairBudget"]["consumedRounds"],1);
    assert_eq!(crate::row(&d,"proposals",id).unwrap()["prepareRunId"],child);
}
#[test]fn native_merged_repair_settlement_controls_preserve_paid_proof_across_serialization(){
    let(mut d,origin,plan)=fresh_revalidation();let child=settle_revalidation_child(&mut d,&origin,&plan,true,false);merge_revalidation(&mut d,&origin);
    crate::row_mut(&mut d,"jobs",&origin).unwrap()["status"]=json!("completed");
    d["branches"][0]["messages"][0]["text"]=json!("Next independently changed source");
    crate::auto_prepare::reconcile_stale(&mut d,chrono::Utc::now().timestamp()+40);
    crate::preparation_reservations::assert_available(&d,&["i".into()],None).unwrap();
    let owners=rows(&d,"jobs").iter().filter(|job|crate::preparation_reservations::requires_control(job))
        .map(|job|crate::preparation_reservations::compact_owner(&d,job).unwrap()).collect::<Vec<_>>();
    let mut scoped=d.clone();scoped["scopeOwners"]=json!(owners);scoped["jobs"]=json!([]);
    scoped=serde_json::from_str(&scoped.to_string()).unwrap();
    let proved=merged_child(&scoped,&origin).unwrap().unwrap();assert_eq!(proved["id"],child);
    crate::preparation_reservations::assert_available(&scoped,&["i".into()],None).unwrap();
    for change in ["missing_child","changed_plan","changed_proposal","unknown_operation"]{
        let mut invalid=scoped.clone();match change{
            "missing_child"=>invalid["scopeOwners"].as_array_mut().unwrap().retain(|job|job["id"]!=child),
            "changed_plan"=>{let job=invalid["scopeOwners"].as_array_mut().unwrap().iter_mut().find(|job|job["id"]==child).unwrap();job["answeringRepairPlan"]["request"]["previousDecision"]["text"]=json!("Forged old source");},
            "changed_proposal"=>invalid["proposals"][1]["text"]=json!("Changed historical paid content"),
            _=>invalid["operations"]=json!([{"id":"alias-unknown","itemId":"old-local-alias","status":"unknown","target":invalid["items"][0]}]),
        }assert!(crate::preparation_reservations::assert_available(&invalid,&["i".into()],None).is_err(),"{change}");
    }
}
