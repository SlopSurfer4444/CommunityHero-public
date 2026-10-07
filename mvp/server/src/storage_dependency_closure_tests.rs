use super::*;
use serde_json::json;

#[test]
fn global_legacy_inventory_and_exact_currentness_jobs_remain_independent() {
    let source=json!({"items":[{"id":"held","workflow":"attention","autoPreparation":{"requiresReview":true,"jobId":"group","savedProposalId":"saved"}}],
        "proposals":[{"id":"draft","status":"draft","prepareRunId":"current"},{"id":"saved","status":"stale","prepareRunId":"original"},
        {"id":"sent","status":"published","prepareRunId":"cold"}]});
    let controls=[(json!({"id":"cold","purpose":"auto_prepare","prepareOutcome":{"status":"needs_attention"}}),false),
        (json!({"id":"repair","status":"unknown"}),true)];
    let closure=SourceClosure::from_controls(&source,&controls);
    for id in ["group","original","current","repair"] {assert!(closure.full_jobs.contains(id));}
    assert!(!closure.full_jobs.contains("cold"));
    assert_eq!(controls.len(),2,"complete pointerless legacy candidate inventory is retained");
}

#[test]
fn nested_paid_parent_and_duplicate_pinned_archives_expand_without_truncation() {
    let mut closure=SourceClosure::default();
    assert!(closure.expand_jobs(&json!({"history":[{"originalJobId":"parent"}]})));
    closure.collect_archives(&json!({"researchManifest":[{"archiveId":"pin"}]}));
    let research=json!([{"id":"cold","body":"unrelated"},{"id":"pin","body":"first"},{"id":"pin","body":"second"}]);
    assert_eq!(research_projection(Some(&research),&closure).unwrap(),json!([
        {"id":"pin","body":"first"},{"id":"pin","body":"second"}]));
    closure.collect_archives(&json!({"archiveId":7}));
    assert!(closure.full_research);
    assert_eq!(research_projection(Some(&research),&closure),Some(research));
    assert_eq!(research_projection(None,&closure),None,"absence remains different from null");
}

#[test]
fn discovered_native_and_fact_parents_reclose_duplicate_bundle_inventory() {
    let controls=vec![(json!({"id":"origin","prepareBundle":{"id":"one"}}),true),
        (json!({"id":"fact-parent","prepareBundle":{"id":"shared"}}),false),
        (json!({"id":"first-collision","prepareBundle":{"id":"shared"}}),false),
        (json!({"id":"cold","prepareBundle":{"id":"other"}}),false)];
    let mut closure=SourceClosure::from_controls(&json!({}),&controls);
    assert!(closure.expand_jobs(&json!({"modelMaterialReceipt":{"nativeJobId":"material-parent"},
        "factDependencies":[{"pin":{"prepareJobId":"fact-parent"}}],"decoder":{"nativeSourceOriginJobId":"decoder-parent"}})));
    assert!(closure.expand_controls(&controls));
    for id in ["origin","fact-parent","first-collision","material-parent","decoder-parent"]{assert!(closure.full_jobs.contains(id));}
    assert!(!closure.full_jobs.contains("cold"));
}

#[test]
fn malformed_or_deep_job_lineage_keeps_entire_inventory() {
    let controls=vec![(json!({"id":"selected"}),true),(json!({"id":"cold"}),false)];
    for malformed in [json!({"prepareJobId":7}),json!({"nativeJobId":""}),json!({"childJobId":[]})] {
        let mut closure=SourceClosure::from_controls(&json!({}),&controls);
        assert!(closure.expand_jobs(&malformed));assert!(closure.full_job_inventory);
        closure.expand_controls(&controls);assert_eq!(closure.full_jobs.len(),2);
    }
    let mut deep=json!({"nativeJobId":"unseen-parent"});
    for _ in 0..66{deep=json!({"child":deep});}
    let mut closure=SourceClosure::from_controls(&json!({}),&controls);
    assert!(closure.expand_jobs(&deep));assert!(closure.full_job_inventory&&closure.full_research);
    closure.expand_controls(&controls);assert_eq!(closure.full_jobs.len(),2);
}
#[test]
fn frozen_manual_frame_selection_is_an_exact_job_dependency_not_an_empty_hint(){
    let mut closure=SourceClosure::default();
    assert!(closure.expand_jobs(&json!({"prepareBundle":{"request":{"manualFrameRequestIds":["native-manual-frame"]}}})));
    assert!(closure.full_jobs.contains("native-manual-frame"));assert!(!closure.full_job_inventory);
    closure.expand_jobs(&json!({"originatingAnsweringAttemptId":"native-repair-root","parentManualFrameRequestId":"native-manual-parent"}));
    assert!(closure.full_jobs.contains("native-repair-root"));assert!(closure.full_jobs.contains("native-manual-parent"));
    for invalid in [json!("[]"),json!([42]),json!([""]),json!(["a","b","c","d","e","f","g","h","i"])] {
        let mut closure=SourceClosure::default();closure.expand_jobs(&json!({"manualFrameRequestIds":invalid}));
        assert!(closure.full_job_inventory,"unproven selection keeps complete authority rather than omitting requested evidence");
    }
}
