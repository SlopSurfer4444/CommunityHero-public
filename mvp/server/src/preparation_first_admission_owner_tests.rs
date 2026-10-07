//! Exercise full and compact ownership with the actual admission reducers.
use super::*;

fn reserved()->Value {
    let mut d=crate::engine_prepare::tests::fixture(false);
    d["posts"][0]["attachments"]=json!([]);
    let bundle=crate::engine_prepare::build_request(&d,&[json!("ready")],None).unwrap();
    let request=bundle["request"].clone();
    d["jobs"]=json!([{"id":"reserved","kind":"assistant","purpose":"engine_prepare","status":"running",
        "selectedItemIds":["ready"],"prepareBundle":{"version":1,"id":"reserved-bundle","itemIds":["ready"],
            "digest":hash(&request),"request":request},"preparationStages":{"first":null,"review":null,"groupAdmission":[]}}]);
    d["jobs"][0]["prepareBundle"]=bundle;
    let scope=capture(&d,"reserved").unwrap();d["jobs"][0]["scopeReservation"]=scope;
    crate::native_fixture_owner_repair::initialize_workspace(&mut d).unwrap();
    let token=crate::runtime_lifecycle::admission_token(&d,crate::runtime_lifecycle::AdmissionClass::Preparation).unwrap();
    crate::preparation_review::record_initial_admission(&mut d,&token,"reserved",&crate::now()).unwrap();
    crate::preparation_review::reserve_first_admitted(&mut d,&token,"reserved",&request,&crate::now()).unwrap();
    d
}
fn projected(d:&Value)->Value {
    let mut view=d.clone();let owner=compact_owner(d,&d["jobs"][0]).unwrap();
    view["scopeOwners"]=json!([owner]);view["jobs"]=json!([]);view
}

#[test]
fn reserved_first_without_output_survives_terminal_labels_and_owner_exemptions() {
    for status in ["running","queued","interrupted","failed","cancelled","completed"] {
        let mut d=reserved();d["jobs"][0]["status"]=json!(status);
        // A mutable retry pointer is not a durable no-dispatch witness.
        d["items"][0]["autoPreparation"]=json!({"jobId":"reserved","status":"error","attempts":1,
            "retryAt":"2026-10-04T09:00:00Z","reason":"ASSISTANT_BUSY"});
        assert!(unfinished_model(&d["jobs"][0]),"{status}");
        let projection=projected(&d);
        for view in [&d,&projection] {
            assert!(assert_available(view,&["ready".into()],None).is_err(),"{status} foreign scope");
            assert!(assert_available(view,&["ready".into()],Some("reserved")).is_err(),"{status} cannot restart owner");
        }
    }
}

#[test]
fn exact_busy_refusal_witness_discharge_is_bound_and_does_not_cover_other_attempts() {
    let mut d=reserved();
    let witness=capture_failed_no_result(&d,"reserved","ASSISTANT_BUSY").unwrap();
    d["jobs"][0]["status"]=json!("failed");d["jobs"][0]["scopeFailure"]=witness;
    assert!(!unfinished_model(&d["jobs"][0]));
    assert!(assert_available(&d,&["ready".into()],None).is_ok());
    assert!(assert_available(&projected(&d),&["ready".into()],None).is_ok());
    for field in ["ownerJobId","keysDigest","prepareBundleDigest","category"] {
        let mut changed=d.clone();changed["jobs"][0]["scopeFailure"][field]=json!("unbound");
        assert!(unfinished_model(&changed["jobs"][0]),"{field}");
        assert!(assert_available(&changed,&["ready".into()],None).is_err());
    }
    d["jobs"][0]["preparationStages"]["review"]=json!({"status":"unknown"});
    assert!(unfinished_model(&d["jobs"][0]),"BUSY cannot discharge another stage");
    assert!(capture_failed_no_result(&d,"reserved","ASSISTANT_BUSY").is_err());
}

#[test]
fn timeout_is_not_busy_and_completed_first_output_retains_settled_allowance() {
    let mut d=reserved();
    assert!(capture_failed_no_result(&d,"reserved","Adapter timed out; action outcome may be unknown").is_err());
    d["jobs"][0]["status"]=json!("completed");
    d["jobs"][0]["preparationStages"]["first"]=json!({"status":"completed","result":{"text":"retained owned result"}});
    d["jobs"][0]["preparationStages"]["groupAdmission"]=json!([{"status":"admitted","itemIds":["ready"],
        "admission":{"finalAssessments":[{"itemId":"ready","outcome":"needs_attention"}],"candidates":[]}}]);
    assert!(!unfinished_model(&d["jobs"][0]));
    assert!(assert_available(&d,&["ready".into()],None).is_ok());
    assert!(assert_available(&projected(&d),&["ready".into()],None).is_ok());
    assert!(assert_available(&d,&["ready".into()],Some("reserved")).is_ok());
    assert!(capture_failed_no_result(&d,"reserved","ASSISTANT_BUSY").is_err(),"saved paid output never becomes no-spending evidence");
}

#[test]
fn floating_witness_versions_remain_unresolved_in_full_and_compact_controls() {
    let mut d=reserved();
    d["jobs"][0]["scopeFailure"]=capture_failed_no_result(&d,"reserved","ASSISTANT_BUSY").unwrap();
    d["jobs"][0]["status"]=json!("failed");
    assert!(!unfinished_model(&d["jobs"][0]));
    for field in ["firstAdmission","initialAdmission","scopeFailure"] {
        let mut changed=d.clone();
        if field=="scopeFailure" { changed["jobs"][0][field]["version"]=json!(1.0); }
        else { changed["jobs"][0]["preparationStages"][field]["version"]=json!(1.0); }
        assert!(unfinished_model(&changed["jobs"][0]),"{field}");
        for view in [&changed,&projected(&changed)] {
            assert!(assert_available(view,&["ready".into()],None).is_err(),"{field} foreign hold");
            assert!(assert_available(view,&["ready".into()],Some("reserved")).is_err(),"{field} same-owner hold");
        }
    }
}
