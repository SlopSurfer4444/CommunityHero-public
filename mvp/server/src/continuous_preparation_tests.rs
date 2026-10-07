use super::*;
const AT:&str="2026-10-07T03:00:00Z";
fn fixture()->Value{
    let binding=accounts::Profile::BawRussia.binding();
    let mut d=json!({"account":"BAW Russia","connectorBinding":binding,"storageGeneration":"e4e1d9f2-49b8-4b48-846f-ab113f519f89",
        "jobs":[],"items":[],"proposals":[],"posts":[],"branches":[],"materials":[],"knowledge":[],"operations":[],"approvals":[],"audit":[],
        "settings":{"autoPreparation":{"continuousPreparation":{"version":1,"enabled":true,"workflowMode":MODE,"account":"baw-russia","companyId":"BAW Russia",
        "queueEpoch":"e4e1d9f2-49b8-4b48-846f-ab113f519f89","policyId":"policy-fixture","policyRevision":1,
        "invocationLimit":6,"maxOriginalJobInvocations":6,"invocationsPerBridge":2,"maxActiveJobs":8,"maxReadyItems":20,"maxReadyBytes":4096,"issuedSlots":0}}}});
    d["settings"]["autoPreparation"]["continuousPreparation"]["connectorBindingSha256"]=json!(binding_hash(&d).unwrap());d
}
fn admit(d:&mut Value,job:&str,parent:Option<&str>){
    let before=d.clone();
    list_mut(d,"jobs").push(json!({"id":job,"kind":"assistant","purpose":if parent.is_some(){"public_fact_followup"}else{"auto_prepare"},"status":"running"}));
    if let Some(parent)=parent{row_mut(d,"jobs",job).unwrap()["parentPrepareJobId"]=json!(parent);}
    stamp_admissions(&before,d).unwrap();validate_change(&before,d).unwrap();
}
fn request(n:u64)->Value{json!({"account":"baw-russia","operation":"assistant","materialVersion":n,"text":"Привет"})}
fn proof(envelope:&Value)->Value{
    json!({"runMetadata":{"invocationBudget":{"version":1,"contract":BUDGET_CONTRACT,"reservationId":envelope["reservationId"],
        "nativeJobId":envelope["nativeJobId"],"originalJobId":envelope["originalJobId"],"requestSha256":envelope["requestSha256"],
        "issuedSlotIds":envelope["slotIds"],"invoked":[{"slotId":envelope["slotIds"][0],"ordinal":1,"stage":"primary","state":"returned","terminalEvents":1,
            "usage":{"status":"observed","basis":"codex_turn_completed_event","input_tokens":3,"output_tokens":2}}],
        "unit":"codex_process_invocation","hardTokenCeiling":false,"billableWireRequests":null,"refundAuthorized":false}}})
}
#[test]
fn original_actual_slots_are_issued_before_spawn_and_restart_material_versions_never_reset_budget(){
    let mut d=fixture();admit(&mut d,"root",None);
    let before=d.clone();let wire=request(1);
    let envelope=reserve_bridge(&mut d,"root","assistant",&wire,AT).unwrap().unwrap();
    assert_eq!(envelope["requestSha256"],hash(&wire));assert_eq!(policy(&d)["issuedSlots"],2);
    assert_eq!(row(&d,"jobs","root").unwrap()["codexInvocationIssuedSlots"],2);validate_change(&before,&d).unwrap();
    d=serde_json::from_str(&d.to_string()).unwrap();
    assert!(reserve_bridge(&mut d,"root","assistant",&wire,AT).is_err());
    reserve_bridge(&mut d,"root","assistant",&request(2),AT).unwrap();
    reserve_bridge(&mut d,"root","assistant",&request(3),AT).unwrap();
    assert!(reserve_bridge(&mut d,"root","assistant",&request(4),AT).is_err());assert_eq!(policy(&d)["issuedSlots"],6);
    assert!(list(&d,"approvals").is_empty());assert!(list(&d,"operations").is_empty());
}
#[test]
fn owner_initiated_background_descendant_inherits_original_cap_and_cannot_spoof_interactive_purpose(){
    let mut d=fixture();admit(&mut d,"root",None);admit(&mut d,"child",Some("root"));
    let before=d.clone();let envelope=reserve_bridge(&mut d,"child","assistant",&request(1),AT).unwrap().unwrap();
    assert_eq!(envelope["originalJobId"],"root");assert_eq!(row(&d,"jobs","root").unwrap()["codexInvocationIssuedSlots"],2);
    assert_eq!(row(&d,"jobs","child").unwrap()["codexInvocationIssuedSlots"],0);validate_change(&before,&d).unwrap();
    let before=d.clone();row_mut(&mut d,"jobs","child").unwrap()["purpose"]=json!("discussion");
    assert!(validate_change(&before,&d).is_err());
}
#[test]
fn malformed_missing_or_refund_proof_keeps_original_issued_budget_and_valid_observation_is_immutable(){
    let mut d=fixture();admit(&mut d,"root",None);
    let envelope=reserve_bridge(&mut d,"root","assistant",&request(1),AT).unwrap().unwrap();
    let issued=d.clone();
    assert!(settle_bridge(&mut d,"root",&envelope,&json!({}),AT).is_err());assert_eq!(d,issued);
    let mut forged=proof(&envelope);forged["runMetadata"]["invocationBudget"]["refundAuthorized"]=json!(true);
    assert!(settle_bridge(&mut d,"root",&envelope,&forged,AT).is_err());assert_eq!(d,issued);
    settle_bridge(&mut d,"root",&envelope,&proof(&envelope),AT).unwrap();validate_change(&issued,&d).unwrap();
    let settled=d.clone();settle_bridge(&mut d,"root",&envelope,&proof(&envelope),AT).unwrap();assert_eq!(d,settled);
    assert_eq!(policy(&d)["issuedSlots"],2);
}
#[test]
fn guard_rejects_policy_reset_original_counter_rollback_journal_deletion_and_origin_retarget(){
    let mut d=fixture();admit(&mut d,"root",None);reserve_bridge(&mut d,"root","assistant",&request(1),AT).unwrap();
    for change in ["policy","root_counter","journal","origin","slot"]{
        let mut tampered=d.clone();
        match change{
            "policy"=>tampered["settings"]["autoPreparation"]["continuousPreparation"]["issuedSlots"]=json!(0),
            "root_counter"=>row_mut(&mut tampered,"jobs","root").unwrap()["codexInvocationIssuedSlots"]=json!(0),
            "journal"=>row_mut(&mut tampered,"jobs","root").unwrap()[RESERVATIONS]=json!([]),
            "origin"=>row_mut(&mut tampered,"jobs","root").unwrap()[ORIGIN]["originalJobId"]=json!("other"),
            _=>row_mut(&mut tampered,"jobs","root").unwrap()[RESERVATIONS][0]["envelope"]["slotIds"][0]=json!("replacement"),
        }
        assert!(validate_change(&d,&tampered).is_err(),"{change}");
    }
}
#[test]
fn default_and_legacy_generation_fail_closed_without_minting_policy_or_epoch(){
    let mut d=fixture();d["settings"]["autoPreparation"]["continuousPreparation"]=Value::Null;
    let before=d.clone();assert!(!enabled(&d));assert!(admission_reason(&d).is_some());assert_eq!(d,before);
    let mut d=fixture();d.as_object_mut().unwrap().remove("storageGeneration");
    assert!(!enabled(&d));assert_eq!(d.get("storageGeneration"),None);
}
#[test]
fn progress_partitions_pending_settled_unknown_and_archived_issued_slots_without_refund(){
    let mut d=fixture();admit(&mut d,"root",None);
    let envelope=reserve_bridge(&mut d,"root","assistant",&request(1),AT).unwrap().unwrap();
    assert_eq!(invocation_partition(&d,&policy(&d)["policyId"])["reserved"],2);
    settle_bridge(&mut d,"root",&envelope,&proof(&envelope),AT).unwrap();
    let settled=invocation_partition(&d,&policy(&d)["policyId"]);
    assert_eq!(settled["settled"],2);assert_eq!(settled["observedNotInvoked"],1);
    reserve_bridge(&mut d,"root","assistant",&request(2),AT).unwrap();
    row_mut(&mut d,"jobs","root").unwrap()["status"]=json!("failed");
    d["settings"]["autoPreparation"]["continuousPreparation"]["issuedSlots"]=json!(6);
    let view=status_view(&d,AT,true);let partition=&view["invocationPartition"];
    assert_eq!(partition["reserved"],0);assert_eq!(partition["settled"],2);assert_eq!(partition["unknown"],4);
    assert_eq!(partition["refundAuthorized"],false);assert_eq!(view["issuedSlots"],6);
    assert!(status_view(&d,AT,false)["invocationPartition"].is_null());
}
#[test]
fn pristine_generation_retains_archived_token_stop_and_unknown_usage_without_double_counting(){
    let mut d=fixture();
    d["cleanStartArchive"]=json!({"invocationBudgetCarry":{"contract":"communityhero-clean-start-invocation-carry.v1",
        "policyId":"policy-fixture","issuedSlots":2,"observedTokens":8,"tokenObservationIncomplete":true,
        "originalJobs":[{"jobId":"archived-job"}],"resumeAuthorized":false,"refundAuthorized":false}});
    d["settings"]["autoPreparation"]["continuousPreparation"]["issuedSlots"]=json!(2);
    d["settings"]["autoPreparation"]["continuousPreparation"]["observedTokenStop"]=json!(8);
    assert_eq!(observed_tokens(&d,&json!("policy-fixture")),(8,true));
    assert_eq!(admission_reason(&d).as_deref(),Some("continuous_observed_token_stop"));
    let before=d.clone();
    d["cleanStartArchive"]["invocationBudgetCarry"]["observedTokens"]=json!(0);
    assert!(validate_change(&before,&d).is_err());
    d=before.clone();admit(&mut d,"new-job",None);
    assert!(reserve_bridge(&mut d,"new-job","assistant",&request(1),AT).is_err());
    let status=status_view(&d,AT,true);assert_eq!(status["observedTokens"],8);assert_eq!(status["tokenObservationIncomplete"],true);
}

#[test]
fn owner_fresh_editorial_of_background_draft_inherits_native_original_instead_of_interactive_escape(){
    let mut d=fixture();admit(&mut d,"root",None);
    list_mut(&mut d,"proposals").push(json!({"id":"proposal","revision":1,"prepareRunId":"root"}));
    let before=d.clone();list_mut(&mut d,"jobs").push(json!({"id":"review","kind":"editorial_review","purpose":"editorial_review","status":"running",
        "editorialReferences":[{"id":"proposal","revision":1}],"editorialPlan":{"batches":[{}]}}));
    stamp_admissions(&before,&mut d).unwrap();validate_change(&before,&d).unwrap();
    assert_eq!(row(&d,"jobs","review").unwrap()[ORIGIN]["originalJobId"],"root");
    let envelope=reserve_bridge(&mut d,"review","assistant",&request(1),AT).unwrap().unwrap();assert_eq!(envelope["originalJobId"],"root");
}
