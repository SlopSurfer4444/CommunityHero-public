use super::*;

#[test]
fn operator_alternatives_and_informational_dialogue_do_not_create_paid_ownership() {
    let mut d = fixture();
    d["jobs"] = json!([{"id":"discussion","kind":"assistant","purpose":"discussion","status":"completed",
        "prepareBundle":{"itemIds":[]},"result":{"text":"Private answer","proposals":[]}}]);
    let target = crate::row(&d, "items", "a").unwrap().clone();
    d["proposals"] = json!([{"id":"manual-one","itemId":"a","status":"draft","routeTarget":target},
        {"id":"manual-two","itemId":"a","status":"draft","routeTarget":target}]);
    assert!(available(&d, "a").is_ok());
    assert_proposal(&d, &d["proposals"][0]).unwrap();
    assert_proposal(&d, &d["proposals"][1]).unwrap();
    job(&mut d, "paid", &["a"]);
    assert!(
        assert_proposal(&d, &d["proposals"][0]).is_err(),
        "paid preparation still excludes unrelated manual action"
    );
}

#[test]
fn explicit_restart_and_completed_successor_release_old_stale_controls() {
    let mut d = fixture();
    job(&mut d, "old", &["a"]);
    d["jobs"][0]["status"] = json!("completed");
    let old = proposal(&mut d, "old", "a", "stale");
    d["proposals"][0]["staleReason"] = json!("Operator requested a fresh preparation run");
    d["items"][0]["autoRevalidation"] = json!({"restartRunId":"operator-restart"});
    d["preparationRuns"] = json!([{"runId":"operator-restart","applied":true,"itemIds":["a"]}]);
    assert!(available(&d, "a").is_ok());
    d["preparationRuns"] = json!([]);
    assert!(available(&d, "a").is_err());
    job(&mut d, "new", &["a"]);
    let new = crate::row_mut(&mut d, "jobs", "new").unwrap();
    new["purpose"] = json!("auto_revalidate");
    new["refId"] = json!("a");
    new["status"] = json!("completed");
    new.as_object_mut().unwrap().remove("scopeReservation");
    new["prepareBundle"]["request"]["previousDecision"] =
        json!({"itemId":"a","prepareRunId":"old","proposalId":old});
    new["prepareBundle"]["digest"] = json!(hash(&new["prepareBundle"]["request"]));
    let proof = capture(&d, "new").unwrap();
    crate::row_mut(&mut d, "jobs", "new").unwrap()["scopeReservation"] = proof;
    let current = proposal(&mut d, "new", "a", "draft");
    let owner = compact_owner(&d, crate::row(&d, "jobs", "old").unwrap()).unwrap();
    let old_control = json!({"id":old,"itemId":"a","prepareRunId":"old","status":"stale","staleReason":"Operator requested a fresh preparation run"});
    let new_job = crate::row(&d, "jobs", "new").unwrap().clone();
    let new_proposal = crate::row(&d, "proposals", &current).unwrap().clone();
    d["jobs"] = json!([new_job]);
    d["proposals"] = json!([new_proposal]);
    d["scopeOwners"] = json!([owner]);
    d["scopeProposals"] = json!([old_control]);
    assert_proposal(&d, &d["proposals"][0]).unwrap();
    operation(&mut d, &old, "a", "unknown");
    assert!(
        assert_proposal(&d, &d["proposals"][0]).is_err(),
        "supersession never releases UNKNOWN"
    );
}

fn fixture() -> Value {
    let mut d = crate::empty();
    crate::accounts::initialize(&mut d, crate::accounts::Profile::LikeAvto).unwrap();
    let binding = d["connectorBinding"].clone();
    d["items"] = json!([
        {"id":"a","branchId":"branch-a","objectId":"post-a","itemId":"external-a","postKey":"post-a","conversationKey":"thread-a","connectorBinding":binding},
        {"id":"b","branchId":"branch-a","objectId":"post-b","itemId":"external-b","postKey":"post-b","conversationKey":"thread-b","connectorBinding":binding},
        {"id":"c","branchId":"other-local-branch","objectId":"post-a","itemId":"external-c","postKey":"post-a","conversationKey":"thread-a","connectorBinding":binding},
        {"id":"d","branchId":"branch-d","objectId":"post-d","itemId":"external-d","postKey":"post-d","conversationKey":"thread-d","connectorBinding":binding}
    ]);
    d
}
fn job(d: &mut Value, name: &str, ids: &[&str]) {
    let items: Vec<_> = ids
        .iter()
        .map(|id| crate::row(d, "items", id).unwrap().clone())
        .collect();
    let request =
        json!({"account":d["account"],"connectorBinding":d["connectorBinding"],"items":items});
    d["jobs"].as_array_mut().unwrap().push(json!({"id":name,"kind":"assistant","purpose":"engine_prepare","status":"running",
        "selectedItemIds":ids,"prepareBundle":{"id":format!("bundle-{name}"),"version":1,"itemIds":ids,"request":request,"digest":hash(&request)},
        "preparationStages":{"first":null,"review":null,"groupAdmission":[]}}));
    let reservation = capture(d, name).unwrap();
    crate::row_mut(d, "jobs", name).unwrap()["scopeReservation"] = reservation;
}
fn available(d: &Value, id: &str) -> ApiResult<()> {
    assert_available(d, &[id.to_owned()], None)
}
fn proposal(d: &mut Value, run: &str, id: &str, status: &str) -> String {
    let name = format!("proposal-{run}-{id}");
    let target = crate::row(d, "items", id).unwrap().clone();
    d["proposals"].as_array_mut().unwrap().push(
        json!({"id":name,"itemId":id,"prepareRunId":run,"status":status,
        "routeTarget":target}),
    );
    name
}
fn operation(d: &mut Value, proposal_id: &str, id: &str, status: &str) {
    let target = crate::row(d, "items", id).unwrap().clone();
    d["operations"]
        .as_array_mut()
        .unwrap()
        .push(json!({"id":format!("operation-{proposal_id}"),
        "proposalId":proposal_id,"itemId":id,"status":status,"target":target,"evidence":{}}));
}

#[test]
fn independent_scopes_overlap_sender_but_same_branch_and_real_conversation_block() {
    let mut d = fixture();
    job(&mut d, "A", &["a"]);
    assert!(
        available(&d, "d").is_ok(),
        "independent next preparation may overlap prior scope sender"
    );
    for id in ["a", "b", "c"] {
        let error = available(&d, id).unwrap_err();
        assert_eq!(error.0, axum::http::StatusCode::CONFLICT, "{id}");
    }
    assert!(
        assert_available(&d, &["a".into()], Some("A")).is_ok(),
        "trusted own job may consume saved work"
    );
    assert!(
        assert_available(&d, &["d".into()], Some("A")).is_err(),
        "owner exemption has exact membership"
    );
}

#[test]
fn terminal_prepare_status_retains_paid_ready_and_interrupted_work() {
    for status in ["completed", "interrupted", "failed", "cancelled"] {
        let mut d = fixture();
        job(&mut d, "A", &["a"]);
        crate::row_mut(&mut d, "jobs", "A").unwrap()["status"] = json!(status);
        crate::row_mut(&mut d, "jobs", "A").unwrap()["preparationStages"]["first"] =
            json!({"status":"completed","result":{"text":"Paid draft"}});
        proposal(&mut d, "A", "a", "draft");
        assert!(available(&d, "a").is_err(), "{status}");
        assert!(
            available(&d, "b").is_err(),
            "branch remains owned for {status}"
        );
        assert!(available(&d, "d").is_ok());
    }
    let mut d = fixture();
    job(&mut d, "A", &["a"]);
    d["jobs"][0]["status"] = json!("interrupted");
    assert!(
        available(&d, "a").is_err(),
        "missing checkpoint is no proof of no spending"
    );
}

#[test]
fn unknown_alias_blocks_even_after_connection_revision_change_and_known_sibling() {
    let mut d = fixture();
    let mut alias = crate::row(&d, "items", "a").unwrap().clone();
    alias["id"] = json!("historical-alias");
    d["operations"] = json!([
        {"id":"unknown","itemId":"historical-alias","proposalId":"historical","status":"unknown","target":alias},
        {"id":"known","itemId":"a","proposalId":"other","status":"succeeded","target":alias,"evidence":{}}
    ]);
    assert!(available(&d, "a").is_err());
    assert!(available(&d, "c").is_err());
    assert!(available(&d, "d").is_ok());
    d["connectorBinding"]["revision"] = json!(2);
    for item in d["items"].as_array_mut().unwrap() {
        item["connectorBinding"]["revision"] = json!(2);
    }
    assert!(
        available(&d, "a").is_err(),
        "revision cannot rename an unresolved provider recipient"
    );
    d["operations"][0]["status"] = json!("dispatching");
    assert!(available(&d, "a").is_err());
}

#[test]
fn settled_known_operations_release_only_entire_bound_scope() {
    for status in ["succeeded", "failed", "stale"] {
        let mut d = fixture();
        job(&mut d, "A", &["a", "d"]);
        d["jobs"][0]["status"] = json!("completed");
        let a = proposal(&mut d, "A", "a", status);
        operation(&mut d, &a, "a", status);
        assert!(
            available(&d, "b").is_err(),
            "unsettled paid recipient retains scope"
        );
        let other = proposal(&mut d, "A", "d", status);
        operation(&mut d, &other, "d", status);
        assert!(
            available(&d, "b").is_ok(),
            "known {status} outcomes settle complete scope"
        );
        d["operations"][0]["target"]["itemId"] = json!("retargeted");
        assert!(
            available(&d, "b").is_err(),
            "unrelated route cannot settle saved recipient"
        );
        d["operations"][0]["target"]["itemId"] = json!("external-a");
        d["operations"][0]["status"] = json!("unknown");
        assert!(available(&d, "b").is_err(), "unknown is never settled");
    }
}

#[test]
fn immutable_capture_replays_saved_request_and_storage_rejects_rebinding() {
    let mut d = fixture();
    job(&mut d, "A", &["a"]);
    let before = d.clone();
    let saved = capture(&d, "A").unwrap();
    d["items"][0]["text"] = json!("new text");
    assert_eq!(
        capture(&d, "A").unwrap(),
        saved,
        "replay keeps original scope"
    );
    assert!(validate_change(&before, &d).is_ok());
    d["items"][0]["itemId"] = json!("new recipient");
    assert_eq!(capture(&d, "A").unwrap(), saved);
    assert!(assert_available(&d, &["a".into()], Some("A")).is_err());
    for mode in ["keys", "delete", "bundle", "digest"] {
        let mut after = before.clone();
        match mode {
            "keys" => after["jobs"][0]["scopeReservation"]["keys"][0] = json!("changed"),
            "delete" => {
                after["jobs"][0]
                    .as_object_mut()
                    .unwrap()
                    .remove("scopeReservation");
            }
            "bundle" => {
                after["jobs"][0]["prepareBundle"]["request"]["items"][0]["itemId"] =
                    json!("changed")
            }
            _ => after["jobs"][0]["scopeReservation"]["keysDigest"] = json!("0".repeat(64)),
        }
        assert!(validate_change(&before, &after).is_err(), "{mode}");
    }
}

#[test]
fn company_isolation_and_foreign_capture_fail_closed() {
    let mut d = fixture();
    job(&mut d, "A", &["a"]);
    d["account"] = json!("BAW Russia");
    d["connectorBinding"] = crate::accounts::Profile::BawRussia.binding();
    assert!(
        capture(&d, "A").is_err(),
        "company switch never retargets a captured reservation"
    );
    let mut d = fixture();
    let mut foreign = d["items"][0].clone();
    foreign["connectorBinding"] = crate::accounts::Profile::BawRussia.binding();
    d["operations"] = json!([{"id":"foreign","itemId":"a","status":"unknown","target":foreign}]);
    assert!(
        available(&d, "a").is_ok(),
        "explicit other-company external alias never combines ledgers"
    );
    d["items"][0]["account"] = json!("BAW Russia");
    assert!(available(&d, "a").is_err());
}

#[test]
fn cancelled_unpaid_requires_explicit_witness_and_no_retained_work() {
    let mut d = fixture();
    job(&mut d, "A", &["a"]);
    d["jobs"][0]["status"] = json!("cancelled");
    assert!(available(&d, "a").is_err());
    d["jobs"][0]["scopeCancellation"] =
        json!({"version":1,"modelDispatchPrevented":true,"noResult":true});
    assert!(available(&d, "a").is_ok());
    d["jobs"][0]["scopeModelAttempt"] = json!({"status":"unknown"});
    assert!(available(&d, "a").is_err());
    d["jobs"][0]
        .as_object_mut()
        .unwrap()
        .remove("scopeModelAttempt");
    proposal(&mut d, "A", "a", "draft");
    assert!(available(&d, "a").is_err());
}

#[test]
fn legacy_active_pending_and_missing_scope_are_conservative_but_known_failed_can_release() {
    let mut d = fixture();
    job(&mut d, "legacy", &["a"]);
    d["jobs"][0]
        .as_object_mut()
        .unwrap()
        .remove("scopeReservation");
    assert!(available(&d, "a").is_err());
    assert!(available(&d, "d").is_ok());
    d["jobs"][0]["status"] = json!("failed");
    d["jobs"][0]["error"] = json!("ADAPTER_TIMEOUT: known terminal adapter failure");
    assert!(
        available(&d, "a").is_ok(),
        "inspected legacy failed/no-output is eligible for fresh preparation"
    );
    d["jobs"][0]["status"] = json!("interrupted");
    assert!(available(&d, "a").is_err());
    d["jobs"][0]
        .as_object_mut()
        .unwrap()
        .remove("prepareBundle");
    d["jobs"][0]
        .as_object_mut()
        .unwrap()
        .remove("selectedItemIds");
    assert!(
        available(&d, "d").is_err(),
        "unknown legacy active scope cannot spend independently"
    );
    let mut d = fixture();
    job(&mut d, "legacy", &["a"]);
    d["jobs"][0]
        .as_object_mut()
        .unwrap()
        .remove("scopeReservation");
    d["jobs"][0]["status"] = json!("completed");
    let p = proposal(&mut d, "legacy", "a", "succeeded");
    operation(&mut d, &p, "a", "succeeded");
    assert!(
        available(&d, "b").is_ok(),
        "settled legacy history releases by evidence"
    );
}

#[test]
fn manual_and_generated_proposal_exemptions_bind_exact_owner() {
    let mut d = fixture();
    job(&mut d, "A", &["a"]);
    let name = proposal(&mut d, "A", "a", "draft");
    let p = crate::row(&d, "proposals", &name).unwrap();
    assert_eq!(owner_for_proposal(&d, p).unwrap(), Some("A".into()));
    assert!(assert_proposal(&d, p).is_ok());
    let mut wrong = p.clone();
    wrong["itemId"] = json!("d");
    assert!(assert_proposal(&d, &wrong).is_err());
    let manual = json!({"itemId":"a","status":"draft"});
    assert!(assert_proposal(&d, &manual).is_err());
    let manual = json!({"itemId":"d","status":"draft"});
    assert!(assert_proposal(&d, &manual).is_ok());
    let mut edited = p.clone();
    edited.as_object_mut().unwrap().remove("prepareRunId");
    edited["origin"] = json!({"prepareRunId":"A"});
    assert!(assert_proposal(&d, &edited).is_ok());
    operation(&mut d, &name, "a", "unknown");
    assert!(
        assert_proposal(&d, crate::row(&d, "proposals", &name).unwrap()).is_ok(),
        "proposal action admission retains its existing exact recipient UNKNOWN guard"
    );
    assert!(
        assert_available(&d, &["a".into()], Some("A")).is_err(),
        "generation still fences its own UNKNOWN"
    );
}

#[test]
fn compact_owners_preserve_paid_legacy_and_unresolved_truth_without_saved_requests() {
    for status in ["running", "completed", "interrupted"] {
        let mut d = fixture();
        job(&mut d, "A", &["a"]);
        d["jobs"][0]["status"] = json!(status);
        d["jobs"][0]["preparationStages"]["first"] = json!({"result":{"text":"large paid result"}});
        let compact = compact_owner(&d, &d["jobs"][0]).unwrap();
        assert!(compact.get("prepareBundle").is_none());
        assert!(compact.get("preparationStages").is_none());
        d["scopeOwners"] = json!([compact]);
        d["jobs"] = json!([]);
        assert!(available(&d, "a").is_err());
        assert!(available(&d, "d").is_ok());
    }
    let mut d = fixture();
    job(&mut d, "A", &["a"]);
    d["jobs"][0]["status"] = json!("completed");
    let p = proposal(&mut d, "A", "a", "succeeded");
    operation(&mut d, &p, "a", "succeeded");
    let compact = compact_owner(&d, &d["jobs"][0]).unwrap();
    d["scopeOwners"] = json!([compact]);
    d["jobs"] = json!([]);
    assert!(available(&d, "b").is_ok());
    let before = d.clone();
    d["scopeOwners"][0]["status"] = json!("failed");
    assert!(validate_change(&before, &d).is_err());
}

#[test]
fn settled_attention_has_no_outstanding_paid_candidate_and_known_nonretryable_success_releases() {
    let mut d = fixture();
    job(&mut d, "A", &["a"]);
    d["jobs"][0]["status"] = json!("completed");
    d["jobs"][0]["preparationStages"]["groupAdmission"] = json!([{"status":"admitted","itemIds":["a"],
        "admission":{"candidates":[],"finalAssessments":[{"itemId":"a","outcome":"needs_attention"}]}}]);
    assert!(available(&d, "b").is_ok());
    let compact = compact_owner(&d, &d["jobs"][0]).unwrap();
    d["scopeOwners"] = json!([compact]);
    d["jobs"] = json!([]);
    assert!(available(&d, "b").is_ok());
    let mut d = fixture();
    job(&mut d, "A", &["a"]);
    d["jobs"][0]["status"] = json!("completed");
    let p = proposal(&mut d, "A", "a", "succeeded");
    operation(&mut d, &p, "a", "succeeded");
    d["operations"][0]["evidence"]["providerRetryAllowed"] = json!(false);
    assert!(available(&d, "b").is_ok());
}

#[test]
fn storage_only_reduced_legacy_request_preserves_route_controls_without_rehashing() {
    let mut d = fixture();
    job(&mut d, "A", &["a"]);
    let mut projected = d["jobs"][0].clone();
    projected
        .as_object_mut()
        .unwrap()
        .remove("scopeReservation");
    projected["prepareBundle"]["request"]["items"][0]
        .as_object_mut()
        .unwrap()
        .remove("postKey");
    // postKey remains required by ResourceRef, so a missing route cannot be
    // admitted even in a reduced control. Supply it, omit only harmless text.
    projected["prepareBundle"]["request"]["items"][0]["postKey"] = json!("post-a");
    projected["prepareBundle"]["request"]["instruction"] = json!("reduced control only");
    projected["scopeOwnerProjection"] =
        json!({"version":1,"retainedOutput":false,"cancelledUnpaid":false});
    let compact = compact_owner(&d, &projected).unwrap();
    d["scopeOwners"] = json!([compact]);
    d["jobs"] = json!([]);
    assert!(available(&d, "a").is_err());
    assert!(available(&d, "d").is_ok());
}

#[test]
fn ordinary_manual_alternatives_do_not_claim_paid_generation_scope() {
    let mut d = fixture();
    let mut p = json!({"id":"manual-a","itemId":"a","status":"draft","routeTarget":d["items"][0]});
    d["proposals"] = json!([p]);
    assert!(assert_proposal(&d, &p).is_ok());
    assert!(
        available(&d, "a").is_ok(),
        "ordinary alternatives do not reserve paid generation"
    );
    p["text"] = json!("not exact stored identity");
    assert!(assert_proposal(&d, &p).is_ok());
    p = d["proposals"][0].clone();
    d["proposals"]
        .as_array_mut()
        .unwrap()
        .push(json!({"id":"manual-b","itemId":"b","status":"draft"}));
    assert!(
        assert_proposal(&d, &p).is_ok(),
        "another ordinary manual alternative remains an operator review choice"
    );
}

#[test]
fn ordinary_discussion_and_terminal_unreserved_history_do_not_retrofit_ownership() {
    let mut d = fixture();
    d["jobs"] = json!([
        {"id":"chat","kind":"assistant","status":"running","prepareBundle":{"itemIds":[]}},
        {"id":"past","purpose":"auto_revalidate","status":"failed","refId":"a"},
        {"id":"old-group","purpose":"auto_prepare","status":"completed","prepareBundle":{"itemIds":["a","missing"]}}
    ]);
    assert!(available(&d, "a").is_ok());
    d["scopeProposals"] = json!([{"id":"paid","itemId":"a","status":"draft","prepareRunId":"past","paidGeneration":true}]);
    assert!(
        available(&d, "b").is_err(),
        "pending actual paid proposal remains a branch owner"
    );
    d["scopeProposals"][0]["status"] = json!("stale");
    assert!(available(&d, "a").is_ok());
    let compact = compact_owner(&d, &d["jobs"][1]).unwrap();
    assert_eq!(
        compact["scopeOwnerProjection"]["reservationWasSaved"],
        false
    );
    d["scopeOwners"] = json!([compact]);
    d["jobs"] = json!([]);
    assert!(available(&d, "a").is_ok());
}

#[test]
fn legacy_terminal_unknown_attempt_and_recovery_remain_controls() {
    for evidence in ["review", "recovery"] {
        let mut d = fixture();
        job(&mut d, "legacy", &["a"]);
        d["jobs"][0]
            .as_object_mut()
            .unwrap()
            .remove("scopeReservation");
        d["jobs"][0]["status"] = json!("failed");
        if evidence == "review" {
            d["jobs"][0]["preparationStages"]["reviewChunks"] =
                json!({"chunks":[{"attempts":[{"status":"unknown"}]}]});
        } else {
            d["jobs"][0]["recovery"] = json!({"status":"unknown"});
        }
        assert!(requires_control(&d["jobs"][0]), "{evidence}");
        assert!(available(&d, "a").is_err());
        let compact = compact_owner(&d, &d["jobs"][0]).unwrap();
        assert_eq!(
            compact["scopeOwnerProjection"]["reservationWasSaved"],
            false
        );
        assert_eq!(compact["scopeOwnerProjection"]["unfinishedModel"], true);
        d["scopeOwners"] = json!([compact]);
        d["jobs"] = json!([]);
        assert!(available(&d, "a").is_err());
    }
}

#[test]
fn compact_active_legacy_proof_does_not_become_saved_reservation_when_terminal() {
    let mut d = fixture();
    job(&mut d, "legacy", &["a"]);
    d["jobs"][0]
        .as_object_mut()
        .unwrap()
        .remove("scopeReservation");
    let mut compact = compact_owner(&d, &d["jobs"][0]).unwrap();
    assert!(compact.get("scopeReservation").is_some());
    assert_eq!(
        compact["scopeOwnerProjection"]["reservationWasSaved"],
        false
    );
    d["scopeOwners"] = json!([compact]);
    d["jobs"] = json!([]);
    assert!(available(&d, "a").is_err());
    d["scopeOwners"][0]["status"] = json!("completed");
    assert!(available(&d, "a").is_ok());
    compact = d["scopeOwners"][0].clone();
    compact["scopeOwnerProjection"]["reservationWasSaved"] = json!(true);
    d["scopeOwners"] = json!([compact]);
    assert!(
        available(&d, "a").is_err(),
        "actual saved proof cannot release by terminal status"
    );
}

fn failed_receipt(d: &mut Value) {
    let job = d["jobs"][0].clone();
    d["items"][0]["autoRevalidation"] = json!({"restartRunId":"fresh"});
    d["preparationRuns"] = json!([{"runId":"fresh","applied":true,"requestedItemIds":["a"],"itemIds":["a"],
        "errorRetries":[{"itemId":"a","jobId":job["id"],"bundleId":job["prepareBundle"]["id"],"bundleDigest":job["prepareBundle"]["digest"]}]}]);
}

#[test]
fn failed_saved_scope_can_be_superseded_by_exact_applied_receipt_but_unknown_model_cannot() {
    let mut d = fixture();
    job(&mut d, "A", &["a"]);
    d["jobs"][0]["status"] = json!("failed");
    failed_receipt(&mut d);
    assert!(available(&d, "a").is_ok());
    d["preparationRuns"][0]["errorRetries"][0]["bundleDigest"] = json!("forged");
    assert!(available(&d, "a").is_err());
    failed_receipt(&mut d);
    d["jobs"][0]["preparationStages"]["reviewChunks"] =
        json!({"chunks":[{"attempts":[{"status":"unknown"}]}]});
    assert!(available(&d, "a").is_err());
    let compact = compact_owner(&d, &d["jobs"][0]).unwrap();
    assert_eq!(compact["scopeOwnerProjection"]["unfinishedModel"], true);
    d["scopeOwners"] = json!([compact]);
    d["jobs"] = json!([]);
    assert!(available(&d, "a").is_err());
}

#[test]
fn scope_proposals_and_authentic_successor_preserve_supersession_without_changing_old_proof() {
    let mut d = fixture();
    job(&mut d, "A", &["a"]);
    d["jobs"][0]["status"] = json!("completed");
    let name = proposal(&mut d, "A", "a", "stale");
    job(&mut d, "B", &["a"]);
    d["jobs"][1]["purpose"] = json!("auto_revalidate");
    d["jobs"][1]["refId"] = json!("a");
    // Test a storage-constructed control binding without rewriting a paid job.
    let mut successor = compact_owner(&d, &d["jobs"][1]).unwrap();
    successor["scopeOwnerProjection"]["supersedes"] =
        json!({"prepareRunId":"A","proposalId":name,"itemId":"a"});
    let old = compact_owner(&d, &d["jobs"][0]).unwrap();
    d["scopeProposals"] = d["proposals"].clone();
    d["proposals"] = json!([]);
    d["scopeOwners"] = json!([old, successor]);
    d["jobs"] = json!([]);
    assert!(assert_available(&d, &["a".into()], Some("B")).is_ok());
    d["scopeOwners"][1]["scopeOwnerProjection"]["supersedes"]["proposalId"] = json!("foreign");
    assert!(assert_available(&d, &["a".into()], Some("B")).is_err());
}

#[test]
fn zero_selected_held_job_has_no_model_scope_but_pending_paid_lineage_still_fences() {
    let mut d = fixture();
    d["jobs"] = json!([{"id":"all-held","purpose":"engine_prepare","kind":"assistant","status":"running",
        "selectedItemIds":[],"preparationStages":{"first":null,"review":null},"result":{"status":"held","candidates":[]}}]);
    assert!(available(&d, "a").is_ok());
    let compact = compact_owner(&d, &d["jobs"][0]).unwrap();
    assert_eq!(compact["scopeOwnerProjection"]["noSelected"], true);
    d["scopeOwners"] = json!([compact]);
    d["jobs"] = json!([]);
    assert!(available(&d, "a").is_ok());
    d["scopeProposals"] =
        json!([{"id":"actual-paid","itemId":"a","prepareRunId":"all-held","status":"draft"}]);
    assert!(available(&d, "a").is_err());
}

#[test]
fn exact_failed_reducer_witness_allows_bounded_retry_without_releasing_paid_checkpoint() {
    let mut d = fixture();
    job(&mut d, "A", &["a"]);
    d["jobs"][0]["status"] = json!("failed");
    d["items"][0]["autoPreparation"] = json!({"jobId":"A","status":"error","attempts":1,"retryAt":"2026-09-30T01:00:00Z","reason":"Adapter failed (ASSISTANT_BUSY)"});
    assert!(available(&d, "a").is_ok());
    let compact = compact_owner(&d, &d["jobs"][0]).unwrap();
    assert_eq!(compact["scopeOwnerProjection"]["retrySafe"], true);
    d["scopeOwners"] = json!([compact]);
    d["jobs"] = json!([]);
    assert!(available(&d, "a").is_ok());
    let mut d = fixture();
    job(&mut d, "A", &["a"]);
    d["jobs"][0]["status"] = json!("failed");
    d["jobs"][0]["preparationStages"]["first"] =
        json!({"status":"completed","result":{"text":"paid first"}});
    d["items"][0]["autoPreparation"] = json!({"jobId":"A","status":"error","attempts":1,"retryAt":"2026-09-30T01:00:00Z","reason":"ASSISTANT_BUSY"});
    assert!(available(&d, "a").is_err());
}

#[test]
fn proposal_mode_uses_foreign_paid_owner_fence_while_generation_keeps_branch_unknown() {
    let mut d = fixture();
    job(&mut d, "A", &["a"]);
    let p = proposal(&mut d, "A", "a", "draft");
    operation(&mut d, "other", "c", "unknown");
    assert!(assert_proposal(&d, crate::row(&d, "proposals", &p).unwrap()).is_ok());
    assert!(assert_available(&d, &["a".into()], Some("A")).is_err());
    let manual = json!({"id":"manual-d","itemId":"d","status":"draft"});
    assert!(assert_proposal(&d, &manual).is_ok());
    let manual = json!({"id":"manual-b","itemId":"b","status":"draft"});
    assert!(
        assert_proposal(&d, &manual).is_err(),
        "foreign paid scope still fences manual admission"
    );
}

#[test]
fn stale_current_source_disposition_is_bound_and_does_not_release_unknown_or_forged_reason() {
    let mut d = fixture();
    job(&mut d, "A", &["a"]);
    d["jobs"][0]["status"] = json!("completed");
    let name = proposal(&mut d, "A", "a", "stale");
    d["proposals"][0]["prepareBundleId"] = d["jobs"][0]["prepareBundle"]["id"].clone();
    d["proposals"][0]["prepareBundleDigest"] = d["jobs"][0]["prepareBundle"]["digest"].clone();
    d["proposals"][0]["staleReason"] = json!("Review source context changed");
    d["items"][0]["autoPreparation"] =
        json!({"status":"stale","requiresReview":true,"jobId":"A","savedProposalId":name});
    assert!(available(&d, "a").is_ok());
    d["proposals"][0]["staleReason"] = json!("arbitrary stale string");
    assert!(available(&d, "a").is_err());
    d["proposals"][0]["staleReason"] = json!("Review source context changed");
    d["proposals"][0]["prepareBundleDigest"] = json!("forged");
    assert!(available(&d, "a").is_err());
    d["proposals"][0]["prepareBundleDigest"] = d["jobs"][0]["prepareBundle"]["digest"].clone();
    d["scopeProposals"] = d["proposals"].clone();
    d["proposals"] = json!([]);
    let compact = compact_owner(&d, &d["jobs"][0]).unwrap();
    d["scopeOwners"] = json!([compact]);
    d["jobs"] = json!([]);
    assert!(available(&d, "a").is_ok());
    operation(&mut d, &name, "a", "unknown");
    assert!(available(&d, "a").is_err());
}

#[test]
fn durable_failed_witness_survives_retry_pointer_replacement_and_requires_known_terminal_outcome() {
    let mut d = fixture();
    job(&mut d, "A", &["a"]);
    let before = d.clone();
    let witness = capture_failed_no_result(&d, "A", "ASSISTANT_BUSY").unwrap();
    assert_eq!(witness["category"], "ASSISTANT_BUSY");
    d["jobs"][0]["scopeFailure"] = witness;
    assert!(
        validate_change(&before, &d).is_ok(),
        "production failure reducer saves before App.finish"
    );
    assert!(
        available(&d, "a").is_err(),
        "running/crashed job remains owned"
    );
    d["jobs"][0]["status"] = json!("failed");
    d["items"][0]["autoPreparation"] = json!({"status":"running","jobId":"newer","attempts":3});
    assert!(
        available(&d, "a").is_ok(),
        "safe no-result outcome survives pointer overwrite"
    );
    let before = d.clone();
    d["jobs"][0]["scopeFailure"]["category"] = json!("unknown");
    assert!(validate_change(&before, &d).is_err());
    d = before;
    d["jobs"][0]["status"] = json!("interrupted");
    assert!(available(&d, "a").is_err());
    assert!(capture_failed_no_result(&d, "A", "ASSISTANT_BUSY").is_err());
}

#[test]
fn orphan_paid_proposal_self_exemption_compares_control_lineage_and_route() {
    let mut d = fixture();
    let p = json!({"id":"orphan","itemId":"a","status":"draft","revision":1,"kind":"reply_and_close",
        "prepareBundleId":"historical-bundle","prepareBundleDigest":"a".repeat(64),"text":"Exact historical text","routeTarget":d["items"][0]});
    let mut control = p.clone();
    control.as_object_mut().unwrap().remove("text");
    control["paidGeneration"] = json!(true);
    d["scopeProposals"] = json!([control]);
    assert!(assert_proposal(&d, &p).is_ok());
    assert!(available(&d, "a").is_err());
    for field in ["itemId", "prepareBundleId", "prepareBundleDigest"] {
        let mut changed = p.clone();
        changed[field] = json!("changed");
        assert!(assert_proposal(&d, &changed).is_err(), "{field}");
    }
    let mut changed = p.clone();
    changed["routeTarget"]["itemId"] = json!("retargeted");
    assert!(assert_proposal(&d, &changed).is_err());
}

#[test]
fn timeout_text_never_releases_new_proof_or_legacy_unknown_model_attempt() {
    for saved in [true,false] {
        let mut d=fixture();job(&mut d,"A",&["a"]);
        if !saved {d["jobs"][0].as_object_mut().unwrap().remove("scopeReservation");}
        d["jobs"][0]["status"]=json!("failed");d["jobs"][0]["error"]=json!("ADAPTER_TIMEOUT");
        if !saved {d["jobs"][0]["scopeModelAttempt"]=json!({"status":"unknown"});}
        d["items"][0]["autoPreparation"]=json!({"jobId":"A","status":"error","attempts":1,"retryAt":"2026-09-30T01:00:00Z","reason":"ADAPTER_TIMEOUT"});
        assert!(available(&d,"a").is_err());
        for reason in ["ADAPTER_TIMEOUT","ASSISTANT_FAILED","ADAPTER_PROCESS_FAILED","Adapter process failed"] {
            assert!(capture_failed_no_result(&d,"A",reason).is_err(),"{reason}");
        }
        let control=compact_owner(&d,&d["jobs"][0]).unwrap();
        assert_ne!(control["scopeOwnerProjection"]["retrySafe"],true);
        d["scopeOwners"]=json!([control]);d["jobs"]=json!([]);
        assert!(available(&d,"a").is_err(),"projected timeout must retain owner");
    }
}

#[test]
fn legacy_batch_action_proposals_share_branch_but_generation_and_paid_owner_still_fence() {
    let mut d=fixture();
    let p=json!({"id":"old-a","itemId":"a","status":"draft","prepareBundleId":"old-bundle",
        "prepareBundleDigest":"a".repeat(64),"routeTarget":d["items"][0]});
    let other=json!({"id":"old-b","itemId":"b","status":"draft","prepareBundleId":"old-bundle",
        "prepareBundleDigest":"a".repeat(64),"routeTarget":d["items"][1]});
    d["proposals"]=json!([p,other]);
    assert_proposal(&d,&d["proposals"][0]).unwrap();
    assert_proposal(&d,&d["proposals"][1]).unwrap();
    assert!(available(&d,"a").is_err(),"fresh generation fences whole branch");
    let mut duplicate=d["proposals"][0].clone();duplicate["id"]=json!("same-recipient");
    d["proposals"].as_array_mut().unwrap().push(duplicate);
    assert!(assert_proposal(&d,&d["proposals"][0]).is_err());
    d["proposals"].as_array_mut().unwrap().pop();
    job(&mut d,"paid",&["a"]);
    assert!(assert_proposal(&d,&d["proposals"][1]).is_err(),"foreign paid job owns shared branch");
}

fn terminal_auto(d: &mut Value, ids: &[&str], outcome: Value) {
    job(d, "auto", ids);
    let j = crate::row_mut(d, "jobs", "auto").unwrap();
    j["purpose"] = json!("auto_prepare");
    j["refId"] = json!(ids[0]);
    j["status"] = json!("completed");
    j["preparationStages"]["first"] = json!({"status":"completed"});
    j["preparationStages"]["review"] = json!({"status":"completed"});
    j["prepareOutcome"] = outcome;
}

#[test]
fn completed_auto_native_batch_dispositions_release_only_independent_no_candidate_scope() {
    for compact in [false, true] {
        let mut d = fixture();
        terminal_auto(&mut d, &["a", "d"], json!({"status":"needs_attention","items":[
            {"itemId":"a","status":"needs_attention","reason":"Missing fact"},
            {"itemId":"d","status":"prepared"}],"admission":{"candidates":[{"itemId":"d"}]}}));
        proposal(&mut d, "auto", "d", "draft");
        let original = d.clone();
        if compact {
            let owner = compact_owner(&d, &d["jobs"][0]).unwrap();
            assert_eq!(owner["scopeOwnerProjection"]["autoTerminalItemIds"], json!(["a"]));
            d["scopeOwners"] = json!([owner]); d["jobs"] = json!([]);
        }
        assert!(available(&d, "a").is_ok(), "completed native recipient compact={compact}");
        assert!(available(&d, "b").is_ok(), "its branch has no unresolved recipient");
        assert!(available(&d, "d").is_err(), "independent paid draft remains held");
        assert!(assert_available(&d, &["a".into(), "d".into()], None).is_err());
        assert_eq!(d["proposals"], original["proposals"], "read checks never alter paid result");
    }
    let mut d = fixture();
    terminal_auto(&mut d, &["a", "b"], json!({"status":"needs_attention","items":[
        {"itemId":"a","status":"needs_attention"},{"itemId":"b","status":"prepared"}],
        "admission":{"candidates":[{"itemId":"b"}]}}));
    proposal(&mut d, "auto", "b", "draft");
    assert!(available(&d, "a").is_err(), "same-branch paid sibling retains reservation");
}

#[test]
fn completed_auto_native_single_and_context_stale_layouts_preserve_alias_unknown_guards() {
    for disposition in ["needs_attention", "stale"] {
        for batch in [false, true] {
            for compact in [false, true] {
                let mut d = fixture();
                let row = json!({"itemId":"a","status":disposition,"reason":"Context changed during automatic preparation"});
                let outcome = if batch { json!({"status":disposition,"items":[row]}) } else { row };
                terminal_auto(&mut d, &["a"], outcome);
                if compact { d["scopeOwners"] = json!([compact_owner(&d, &d["jobs"][0]).unwrap()]); d["jobs"] = json!([]); }
                assert!(available(&d, "a").is_ok(), "{disposition} batch={batch} compact={compact}");
                let mut alias = d["items"][0].clone(); alias["id"] = json!("historical-alias");
                d["operations"] = json!([{"id":"uncertain","itemId":"historical-alias","status":"unknown","target":alias}]);
                assert!(available(&d, "a").is_err(), "external UNKNOWN always retained");
                let manual = json!({"id":"manual","itemId":"a","status":"draft","routeTarget":d["items"][0]});
                assert!(crate::recipient_operation_blocks_current(&d,&d["operations"][0],&manual,&d["items"][0]),
                    "native exact social guard still blocks alias UNKNOWN after reservation disposition settles");
            }
        }
    }
}

#[test]
fn completed_auto_disposition_cannot_release_model_unknown_paid_result_or_unproven_outcome() {
    for guard in ["model", "review", "recovery", "candidate", "proposal", "duplicate", "foreign", "prepared", "stale-reason", "running", "failed", "other-purpose"] {
        let mut d = fixture();
        terminal_auto(&mut d, &["a", "d"], json!({"status":"needs_attention","items":[{"itemId":"a","status":"needs_attention"}]}));
        // Missing unrelated result keeps generic whole-job settlement false.
        let j = &mut d["jobs"][0];
        match guard {
            "model" => j["scopeModelAttempt"] = json!({"status":"unknown"}),
            "review" => j["preparationStages"]["review"] = json!({"status":"unknown"}),
            "recovery" => j["recovery"] = json!({"status":"running"}),
            "candidate" => j["prepareOutcome"]["admission"] = json!({"candidates":[{"itemId":"a"}]}),
            "duplicate" => j["prepareOutcome"]["items"].as_array_mut().unwrap().push(json!({"itemId":"a","status":"prepared"})),
            "foreign" => j["prepareOutcome"]["items"][0]["itemId"] = json!("foreign"),
            "prepared" => j["prepareOutcome"]["items"][0]["status"] = json!("prepared"),
            "stale-reason" => j["prepareOutcome"]["items"][0] = json!({"itemId":"a","status":"stale","reason":"ADAPTER_TIMEOUT"}),
            "running" | "failed" => j["status"] = json!(guard),
            "other-purpose" => j["purpose"] = json!("engine_prepare"),
            _ => (),
        }
        if guard == "proposal" { proposal(&mut d, "auto", "a", "draft"); }
        assert!(available(&d, "a").is_err(), "full {guard}");
        let owner = compact_owner(&d, &d["jobs"][0]).unwrap();
        d["scopeOwners"] = json!([owner]); d["jobs"] = json!([]);
        assert!(available(&d, "a").is_err(), "compact {guard}");
    }
}

fn completed_engine_partition(d:&mut Value){
    job(d,"engine-owner",&["a","d"]);
    let current=&mut d["jobs"][0];current["status"]=json!("completed");
    current["preparationStages"]["groupAdmission"]=json!([
        {"status":"admitted","itemIds":["a"],"admission":{"finalAssessments":[{"itemId":"a","outcome":"needs_attention"}],"candidates":[]}},
        {"status":"admitted","itemIds":["d"],"admission":{"finalAssessments":[{"itemId":"d","outcome":"reply"}],"candidates":[{"itemId":"d","status":"review","proposalId":"proposal-engine-owner-d"}]}}
    ]);
    proposal(d,"engine-owner","d","draft");
}
#[test]
fn completed_engine_partition_releases_only_independent_exact_no_candidate_member(){
    for compact in [false,true]{
        let mut d=fixture();completed_engine_partition(&mut d);
        if compact{d["scopeOwners"]=json!([compact_owner(&d,&d["jobs"][0]).unwrap()]);d["jobs"]=json!([]);}
        assert!(available(&d,"a").is_ok(),"independent terminal member compact={compact}");
        assert!(available(&d,"d").is_err(),"paid sibling remains outstanding compact={compact}");
        assert!(available(&d,"b").is_ok(),"no outstanding paid member shares branch-a");
        let mut alias=d["items"][0].clone();alias["id"]=json!("old-alias");
        d["operations"]=json!([{"id":"uncertain","status":"unknown","itemId":"old-alias","target":alias}]);
        assert!(available(&d,"a").is_err(),"UNKNOWN retained compact={compact}");
    }
    let mut d=fixture();completed_engine_partition(&mut d);d["items"][3]["branchId"]=json!("branch-a");
    // Refresh only this synthetic pre-dispatch fixture capture to represent an
    // originally shared branch, rather than altering a real paid reservation.
    d["jobs"][0]["prepareBundle"]["request"]["items"][1]=d["items"][3].clone();
    d["jobs"][0]["prepareBundle"]["digest"]=json!(hash(&d["jobs"][0]["prepareBundle"]["request"]));
    d["jobs"][0].as_object_mut().unwrap().remove("scopeReservation");let saved=capture(&d,"engine-owner").unwrap();d["jobs"][0]["scopeReservation"]=saved;
    assert!(available(&d,"a").is_err(),"overlapping same-branch pending sibling stays fenced");
}
#[test]
fn engine_scope_partition_missing_duplicate_foreign_and_unknown_never_release_paid_work(){
    for guard in ["missing","duplicate","foreign","pending","model-unknown","candidate","descendant","partial-selected"]{
        let mut d=fixture();completed_engine_partition(&mut d);
        match guard{
            "missing"=>d["jobs"][0]["preparationStages"]["groupAdmission"][0]["admission"]["finalAssessments"]=json!([]),
            "duplicate"=>{let repeated=d["jobs"][0]["preparationStages"]["groupAdmission"][0].clone();d["jobs"][0]["preparationStages"]["groupAdmission"].as_array_mut().unwrap().push(repeated);},
            "foreign"=>d["jobs"][0]["preparationStages"]["groupAdmission"][0]["admission"]["finalAssessments"][0]["itemId"]=json!("foreign"),
            "pending"=>d["jobs"][0]["preparationStages"]["groupAdmission"][0]["status"]=json!("pending"),
            "model-unknown"=>d["jobs"][0]["scopeModelAttempt"]=json!({"status":"unknown"}),
            "candidate"=>d["jobs"][0]["preparationStages"]["groupAdmission"][0]["admission"]["candidates"]=json!([{"itemId":"a","status":"review"}]),
            "descendant"=>{proposal(&mut d,"engine-owner","a","draft");},
            _=>d["jobs"][0]["selectedItemIds"]=json!(["a"]),
        }
        assert!(available(&d,"a").is_err(),"full {guard}");
        let owner=compact_owner(&d,&d["jobs"][0]).unwrap();d["scopeOwners"]=json!([owner]);d["jobs"]=json!([]);
        assert!(available(&d,"a").is_err(),"compact {guard}");
    }
}

fn operator_close_candidate(d: &mut Value, preserved: Value) -> Value {
    d["items"][0]["revision"] = json!(1);
    d["items"][0]["providerStatus"] = json!("new");
    let binding = crate::active_binding(d).unwrap();
    let item = &d["items"][0];
    let mut p = json!({"id":"owner-close","itemId":"a","kind":"close","text":"","revision":1,
        "itemRevision":1,"reviewContextDigest":"current-context","routeTarget":crate::bound_item(&binding,item).unwrap(),"status":"draft"});
    let body = json!({"_verifiedActor":crate::operator_auth::Actor::local_owner("fixture").public_json()});
    p["operatorCloseDecision"] = crate::operator_close::classify(d, &body, &p, preserved);
    p
}

#[test]
fn explicit_owner_close_retains_terminal_model_unknown_and_paid_result_without_releasing_generation() {
    for status in ["completed", "failed", "interrupted"] {
        for compact in [false, true] {
            let mut d = fixture(); job(&mut d, "paid", &["a"]);
            d["jobs"][0]["status"] = json!(status);
            d["jobs"][0]["scopeModelAttempt"] = json!({"status":"unknown"});
            d["jobs"][0]["preparationStages"]["first"] = json!({"status":"completed","result":{"text":"Original paid output"}});
            proposal(&mut d, "paid", "a", "draft");
            let p = operator_close_candidate(&mut d, Value::Null);
            if compact {
                d["scopeOwners"] = json!([compact_owner(&d,&d["jobs"][0]).unwrap()]);
                d["jobs"] = json!([]); d["activeExternalJobs"] = json!({"version":1,"complete":true,"jobs":[]});
            }
            let before = d.clone();
            assert_operator_close(&d, &p).unwrap();
            assert_eq!(d, before, "new final decision never modifies paid/uncertain history");
            assert!(available(&d,"a").is_err(), "ordinary generation still held {status}/{compact}");
            assert!(assert_proposal(&d,&p).is_err(), "ordinary admission still held");
        }
    }
}

#[test]
fn explicit_owner_close_blocks_live_preparation_external_workers_and_untrusted_or_generated_decisions() {
    for guard in ["queued", "running", "execute", "reconcile", "missing-coverage", "untrusted", "reply", "paid", "route", "item-revision"] {
        let mut d = fixture();
        let mut p = operator_close_candidate(&mut d, Value::Null);
        match guard {
            "queued" | "running" => { job(&mut d,"paid",&["a"]); d["jobs"][0]["status"]=json!(guard); d["jobs"][0]["scopeModelAttempt"]=json!({"status":"unknown"}); },
            "execute" | "reconcile" => d["jobs"] = json!([{"id":"external","kind":guard,"status":"running","refId":"active"}]),
            "missing-coverage" => { d["scopeOwners"]=json!([]); },
            "untrusted" => { p.as_object_mut().unwrap().remove("operatorCloseDecision"); },
            "reply" => p["kind"]=json!("reply_and_close"),
            "paid" => p["prepareRunId"]=json!("paid"),
            "route" => p["routeTarget"]["itemId"]=json!("foreign"),
            "item-revision" => d["items"][0]["revision"]=json!(2),
            _ => unreachable!(),
        }
        assert!(assert_operator_close(&d,&p).is_err(),"{guard}");
    }
    let mut d = fixture(); let p = operator_close_candidate(&mut d, Value::Null);
    job(&mut d,"independent",&["d"]);
    assert_operator_close(&d,&p).unwrap();
    job(&mut d,"same-branch",&["b"]);
    assert!(assert_operator_close(&d,&p).is_err(),"same branch running paid writer held");
}

#[test]
fn owner_close_external_unknown_needs_exact_preservation_and_own_dispatch_never_authorizes_retry() {
    let mut d = fixture();
    let unpreserved = operator_close_candidate(&mut d, Value::Null);
    let item = d["items"][0].clone();
    d["operations"] = json!([{"id":"old-reply","itemId":"historical-alias","approvalId":"old-approval","status":"unknown",
        "target":item,"action":{"action":"reply_and_close","actionId":"old-reply","objectId":item["objectId"],"itemId":item["itemId"],"conversationKey":item["conversationKey"]},
        "executeReceipt":{"mutationOutcome":"uncertain"},"evidence":{"verificationPhase":"unconfirmed"}}]);
    assert!(assert_operator_close(&d,&unpreserved).is_err(),"unpreserved alias UNKNOWN remains blocked");
    let preserved = crate::unknown_reply_close::capture(&d,&item,&json!(["old-reply"])).unwrap();
    let p = operator_close_candidate(&mut d,preserved);
    let old = d["operations"][0].clone();
    assert_operator_close(&d,&p).unwrap();
    let own = json!({"id":"new-close","itemId":"a","proposalId":p["id"],"approvalId":"new-approval","status":"dispatching",
        "target":p["routeTarget"],"action":{"action":"close","actionId":"new-close","objectId":item["objectId"],"itemId":item["itemId"],"conversationKey":item["conversationKey"]},
        "approvedOperatorCloseDecisionSha256":p["operatorCloseDecision"]["decisionSha256"]});
    d["operations"].as_array_mut().unwrap().push(own.clone());
    d["jobs"] = json!([{"id":"own-execute","kind":"execute","refId":"new-approval","status":"running"}]);
    assert_operator_close_for_operation(&d,&p,&own).unwrap();
    assert_eq!(d["operations"][0],old,"old UNKNOWN remains byte-for-byte identical");
    let mut forged = own.clone(); forged["approvalId"] = json!("different");
    assert!(assert_operator_close_for_operation(&d,&p,&forged).is_err());
    d["operations"][1]["status"] = json!("unknown");
    assert!(assert_operator_close_for_operation(&d,&p,&d["operations"][1]).is_err(),"UNKNOWN close cannot dispatch again");
    d["operations"][1] = own.clone();
    d["jobs"].as_array_mut().unwrap().push(json!({"id":"old-readback","kind":"reconcile","refId":"old-reply","status":"running"}));
    assert!(assert_operator_close_for_operation(&d,&p,&own).is_err(),"old late readback must settle before new distinct dispatch");
}

#[test]
fn repair_intent_never_becomes_terminal_legacy_or_unpaid_cancellation_authority() {
    for state in ["reserved", "dispatching", "unknown", "completed"] {
        for status in ["completed", "failed", "cancelled", "interrupted"] {
            let mut d = fixture(); job(&mut d, "repair", &["a"]);
            let record = &mut d["jobs"][0];
            record["purpose"] = json!("answering_repair");
            record["status"] = json!(status);
            record["originatingAnsweringAttemptId"] = json!("original");
            record["repairPaidIntent"] = json!({"status":state,"retryAuthorized":false});
            record["scopeCancellation"] = json!({"version":1,"modelDispatchPrevented":true,"noResult":true});
            record.as_object_mut().unwrap().remove("scopeReservation");
            let record = &d["jobs"][0];
            assert!(requires_control(record), "{state}/{status}");
            assert!(!terminal_unproven_legacy(&d, record), "{state}/{status}");
            assert!(!safe_unpaid_cancel(&d, record), "{state}/{status}");
            assert!(has_retained_checkpoint(record), "{state}/{status}");
            let control = compact_owner(&d, record).unwrap();
            assert_eq!(control, *record, "full native authority is retained unchanged");
            assert!(available(&d, "b").is_err(), "full {state}/{status}");
            d["scopeOwners"] = json!([control]); d["jobs"] = json!([]);
            assert!(available(&d, "b").is_err(), "scoped {state}/{status}");
            assert!(available(&d, "d").is_ok(), "independent scope {state}/{status}");
        }
    }
}

#[test]
fn stripped_or_renamed_repair_controls_cannot_mint_legacy_settlement() {
    let mut d = fixture(); job(&mut d, "repair", &["a"]);
    d["jobs"][0]["status"] = json!("completed");
    d["jobs"][0]["repairPaidIntent"] = json!({"status":"unknown","retryAuthorized":false});
    d["jobs"][0]["purpose"] = json!("discussion");
    d["jobs"][0].as_object_mut().unwrap().remove("scopeReservation");
    assert!(requires_control(&d["jobs"][0]), "paid intent remains authoritative after an invalid purpose change");
    assert!(available(&d,"a").is_err());
    let mut control = compact_owner(&d,&d["jobs"][0]).unwrap();
    control["scopeOwnerProjection"] = json!({"version":1,"retainedOutput":false,"unfinishedModel":false,
        "reservationWasSaved":false,"cancelledUnpaid":true,"noSelected":true});
    assert!(compact_owner(&d,&control).is_err(), "Boolean control cannot replace native repair proof");
    d["scopeOwners"] = json!([control]); d["jobs"] = json!([]);
    assert!(available(&d,"b").is_err(), "invented cancellation/empty scope never releases the repair");
}

#[test]
fn completed_repair_root_without_merge_cannot_release_its_original_held_disposition() {
    let mut d = fixture(); job(&mut d, "original", &["a"]);
    d["jobs"][0]["purpose"] = json!("auto_revalidate");
    d["jobs"][0]["refId"] = json!("a");
    d["jobs"][0]["status"] = json!("completed");
    d["jobs"][0]["prepareOutcome"] = json!({"itemId":"a","status":"needs_attention"});
    d["jobs"][0]["videoFrameNeeds"] = json!([{"affectedRecipientIds":["a"]}]);
    d["jobs"][0]["preparationStages"]["repairBudget"] =
        json!({"schemaVersion":1,"maxRounds":1,"consumedRounds":1,"authority":"admitted_workflow_v1"});
    d["jobs"][0]["preparationStages"]["answeringRepairs"] =
        json!([{"childJobId":"missing-native-child","roundOrdinal":1}]);
    assert!(available(&d,"b").is_err(), "completed attention is not repair settlement");
    let control=compact_owner(&d,&d["jobs"][0]).unwrap();
    assert_eq!(control,d["jobs"][0]);
    d["scopeOwners"]=json!([control]);d["jobs"]=json!([]);
    assert!(available(&d,"b").is_err());
    assert!(available(&d,"d").is_ok());
}
