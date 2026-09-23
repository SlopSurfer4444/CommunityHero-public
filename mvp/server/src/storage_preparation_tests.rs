use super::*;
use serde_json::json;
use std::time::Instant;

const NOW: i64 = 1_765_000_000;

fn fixture() -> Value {
    let at = chrono::DateTime::from_timestamp(NOW, 0).unwrap().to_rfc3339();
    let created = chrono::DateTime::from_timestamp(NOW - 60, 0).unwrap().to_rfc3339();
    let mut d = crate::empty();
    d["items"] = json!([{"id":"i","itemId":"c","objectId":"o","postKey":"p","conversationKey":"thread",
        "branchId":"b","postId":"post","revision":1,"draft":"","workflow":"attention",
        "providerStatus":"new","createdAt":created,"providerObservedAt":at}]);
    d["branches"] = json!([{"id":"b","postId":"post","messages":[{"id":"c","text":"Hi"}],"contextComplete":false}]);
    d["posts"] = json!([{"id":"post","postKey":"p","text":"Post"}]);
    let research_at = crate::now();
    let mut archive = json!({"id":"research:old","jobId":"paid-research","account":"LikeAvto",
        "connectorBinding":null,"createdAt":research_at,"trust":"source_only","activePolicy":false,
        "posts":[{"id":"post","postKey":"p","text":"Post"}],"bindings":[{"itemId":"i","postKey":"p"}],
        "review":{"status":"completed","research":{"version":1,"status":"completed","trust":"source_only",
            "webCalls":1,"completedAt":research_at,"sources":[{"itemId":"i","url":"https://manufacturer.example/specs",
                "title":"Specs","claim":"Source describes this model","trust":"source_only"}]}}});
    archive["checksum"] = json!(crate::research_cache::checksum(&archive));
    d["preparationResearch"] = json!([archive]);
    d["jobs"] = json!([
        {"id":"sync-old","kind":"sync","status":"completed","payload":{"bulk":"unrelated"}},
        {"id":"discussion-old","kind":"assistant","purpose":"discussion","status":"completed","toolResults":[{"private":"unrelated"}]}
    ]);
    d["conversations"] = json!([{"id":"private","messages":[{"text":"unrelated"}]}]);
    d["audit"] = json!([{"id":"audit-old","action":"other","refId":"i"}]);
    d["approvals"] = json!([{"id":"approval-old","status":"approved","proposals":[]}]);
    d["feedback"] = json!([{"id":"feedback-old","itemId":"i"}]);
    d
}

fn normalize_ids(value: &mut Value) {
    match value {
        Value::String(text) if uuid::Uuid::parse_str(text).is_ok() => *text = "<new-id>".into(),
        Value::Array(values) => values.iter_mut().for_each(normalize_ids),
        Value::Object(fields) => fields.values_mut().for_each(normalize_ids),
        _ => (),
    }
}

#[test]
fn claim_matches_full_workspace_and_preserves_omitted_history() {
    let mut full = fixture();
    let original = full.clone();
    let before = projection_of(&full).unwrap();
    assert!(!crate::list(&before, "jobs").iter().any(|j| j["id"] == "sync-old" || j["id"] == "discussion-old"));
    for name in OMITTED { assert!(before.get(*name).is_none()); }
    let mut after = before.clone();
    let mut full_result = crate::auto_prepare::claim(&mut full, NOW).unwrap();
    let mut projected_result = crate::auto_prepare::claim(&mut after, NOW).unwrap();
    assert!(full_result.is_some(), "fixture must exercise an actual preparation claim");
    assert!(projected_result.is_some());
    assert!(after["jobs"].as_array().unwrap().last().unwrap()["prepareBundle"]["researchManifest"].as_array().is_some_and(|rows| !rows.is_empty()),
        "claim must pin reusable paid research");
    validate_claim_change(&before, &after).unwrap();
    let mut merged = original;
    merge_claim_delta(&mut merged, &before, &after).unwrap();
    // new_job timestamps at wall clock time, while the claim's durable
    // eligibility/reconciliation clock is the explicit NOW argument.
    full["jobs"].as_array_mut().unwrap().last_mut().unwrap()["createdAt"] = json!("<claim-time>");
    merged["jobs"].as_array_mut().unwrap().last_mut().unwrap()["createdAt"] = json!("<claim-time>");
    normalize_ids(&mut full);
    normalize_ids(&mut merged);
    normalize_ids(&mut full_result.as_mut().unwrap().1);
    normalize_ids(&mut projected_result.as_mut().unwrap().1);
    assert_eq!(full, merged);
    assert_eq!(full_result.as_ref().map(|(_, request)| request), projected_result.as_ref().map(|(_, request)| request));
    assert_eq!(merged["conversations"].as_array().unwrap().len(), 1);
    assert_eq!(merged["approvals"].as_array().unwrap().len(), 1);
}

#[test]
fn historical_auto_jobs_and_proposal_references_remain_visible() {
    let mut full = fixture();
    full["jobs"].as_array_mut().unwrap().extend([
        json!({"id":"old-prep","kind":"assistant","purpose":"auto_prepare","status":"completed","refId":"i"}),
        json!({"id":"old-media","kind":"media","status":"failed","refId":"post"}),
        json!({"id":"legacy-ref","kind":"custom","status":"completed"}),
    ]);
    full["proposals"] = json!([{"id":"p","itemId":"i","status":"stale","prepareRunId":"legacy-ref"}]);
    let projected = projection_of(&full).unwrap();
    assert_eq!(crate::list(&projected, "jobs").iter().map(|j| j["id"].as_str().unwrap()).collect::<Vec<_>>(),
        vec!["old-prep", "old-media", "legacy-ref"]);
}

#[test]
fn fresh_context_claim_keeps_receipt_and_failed_job_proof_in_scoped_storage() {
    let mut baseline=fixture();
    let (initial,_)=crate::auto_prepare::claim(&mut baseline,NOW).unwrap().unwrap();
    let job=crate::row_mut(&mut baseline,"jobs",&initial).unwrap();
    job["status"]=json!("failed");job["error"]=json!("ASSISTANT_INVALID_RESEARCH_UNOBSERVED_URL");
    let mut latest=job.clone();latest["id"]=json!("latest-held");latest["purpose"]=json!("auto_revalidate");
    baseline["jobs"].as_array_mut().unwrap().push(latest);
    baseline["items"][0]["autoPreparation"]["status"]=json!("needs_attention");
    baseline["items"][0]["autoRevalidation"]=json!({"status":"held","jobId":"latest-held"});
    baseline["branches"][0]["messages"][0]["text"]=json!("Current source");
    baseline["settings"]["autoPreparation"]=json!({"revalidation":{"enabled":true,"debounceSeconds":30}});
    let receipt=crate::preparation_restart::plan_context(&mut baseline,"fresh-scoped",true,NOW+1,Some(&["i".into()]),true).unwrap();
    assert_eq!(receipt["eligibleCount"],1);
    let before=projection_of(&baseline).unwrap();
    assert_eq!(before["preparationRuns"],baseline["preparationRuns"]);
    assert_eq!(crate::row(&before,"jobs","latest-held").unwrap(),crate::row(&baseline,"jobs","latest-held").unwrap());
    assert!(crate::row(&before,"jobs",&initial).is_ok());
    assert!(crate::row(&before,"jobs","discussion-old").is_err());
    let mut full=baseline.clone();let mut after=before.clone();
    assert!(crate::auto_prepare::claim(&mut full,NOW+2).unwrap().is_none());
    assert!(crate::auto_prepare::claim(&mut after,NOW+2).unwrap().is_none());
    let full_claim=crate::auto_prepare::claim(&mut full,NOW+33).unwrap().unwrap();
    let scoped_claim=crate::auto_prepare::claim(&mut after,NOW+33).unwrap().unwrap();
    assert_eq!(full_claim.1,scoped_claim.1);
    assert_eq!(scoped_claim.1["previousDecision"]["prepareRunId"],"latest-held");
    validate_claim_change(&before,&after).unwrap();
    let mut merged=baseline;merge_claim_delta(&mut merged,&before,&after).unwrap();
    for doc in [&mut full,&mut merged] {
        doc["jobs"].as_array_mut().unwrap().last_mut().unwrap()["createdAt"]=json!("<claim-time>");
        normalize_ids(doc);
    }
    assert_eq!(full,merged);
    assert_eq!(merged["conversations"].as_array().unwrap().len(),1);
    assert_eq!(merged["preparationRuns"][0]["runId"],"fresh-scoped");
}

#[test]
fn stale_saved_proposal_claim_matches_full_and_holds_paid_result() {
    let mut full = fixture();
    full["items"][0]["workflow"] = json!("prepared");
    full["items"][0]["autoPreparation"] = json!({"status":"prepared","jobId":"old-prep"});
    full["jobs"].as_array_mut().unwrap().push(json!({"id":"old-prep","kind":"assistant",
        "purpose":"auto_prepare","status":"completed","refId":"i","prepareBundle":{"id":"old-bundle"}}));
    full["proposals"] = json!([{"id":"saved","itemId":"i","status":"draft","prepareRunId":"old-prep",
        "prepareBundleId":"other-bundle","revision":1}]);
    let original = full.clone();
    let before = projection_of(&full).unwrap();
    let mut after = before.clone();
    assert!(crate::auto_prepare::claim(&mut full, NOW).unwrap().is_none());
    assert!(crate::auto_prepare::claim(&mut after, NOW).unwrap().is_none());
    validate_claim_change(&before, &after).unwrap();
    let mut merged = original;
    merge_claim_delta(&mut merged, &before, &after).unwrap();
    assert_eq!(full, merged);
    assert_eq!(merged["proposals"][0]["status"], "stale");
    assert_eq!(merged["items"][0]["autoPreparation"]["savedProposalId"], "saved");
    assert_eq!(merged["items"][0]["autoPreparation"]["requiresReview"], true);
}

#[test]
fn revalidation_claim_matches_full_and_preserves_saved_proposal() {
    let mut baseline = fixture();
    let (original_job, _) = crate::auto_prepare::claim(&mut baseline, NOW).unwrap().unwrap();
    let response = json!({"text":"Reviewed","sources":[],"assessments":[{"itemId":"i","outcome":"reply",
        "reason":"Evidence supports this reply"}],"proposals":[{"itemId":"i","kind":"reply_and_close","text":"Saved reply"}]});
    crate::auto_prepare::complete(&mut baseline, &original_job, &response, NOW).unwrap();
    crate::row_mut(&mut baseline, "jobs", &original_job).unwrap()["status"] = json!("completed");
    baseline["materials"].as_array_mut().unwrap().push(json!({"id":"new-evidence","kind":"transcript","postKey":"p","text":"New evidence"}));
    crate::auto_prepare::reconcile_stale(&mut baseline, NOW + 1);
    assert_eq!(baseline["proposals"][0]["status"], "stale");
    baseline["settings"]["autoPreparation"] = json!({"revalidation":{"enabled":true,"debounceSeconds":30}});
    let saved = baseline["proposals"][0].clone();
    let before = projection_of(&baseline).unwrap();
    let mut full = baseline.clone();
    let mut after = before.clone();
    assert!(crate::auto_prepare::claim(&mut full, NOW + 1).unwrap().is_none());
    assert!(crate::auto_prepare::claim(&mut after, NOW + 1).unwrap().is_none());
    assert_eq!(after["items"][0]["autoRevalidation"]["status"], "settling");
    let full_result = crate::auto_prepare::claim(&mut full, NOW + 32).unwrap();
    let projected_result = crate::auto_prepare::claim(&mut after, NOW + 32).unwrap();
    assert!(full_result.is_some() && projected_result.is_some());
    assert_eq!(full_result.as_ref().unwrap().1, projected_result.as_ref().unwrap().1);
    validate_claim_change(&before, &after).unwrap();
    let old_job_count = crate::list(&baseline, "jobs").len();
    let mut merged = baseline;
    merge_claim_delta(&mut merged, &before, &after).unwrap();
    assert_eq!(merged["proposals"][0], saved);
    for doc in [&mut full, &mut merged] {
        doc["jobs"].as_array_mut().unwrap()[old_job_count]["createdAt"] = json!("<claim-time>");
        normalize_ids(doc);
    }
    assert_eq!(full, merged);
    assert_eq!(merged["jobs"].as_array().unwrap().last().unwrap()["purpose"], "auto_revalidate");
}

#[test]
fn rejects_authority_source_and_unexpected_job_mutations() {
    let before = projection_of(&fixture()).unwrap();
    let mut after = before.clone();
    after["audit"] = json!([{"id":"new","action":"unexpected","refId":"i"}]);
    assert!(validate_claim_change(&before, &after).is_err());
    let mut after = before.clone();
    after["items"][0]["draft"] = json!("hidden edit");
    assert!(validate_claim_change(&before, &after).is_err());
    let mut after = before.clone();
    after["jobs"].as_array_mut().unwrap().push(json!({"id":"new","kind":"sync","status":"running"}));
    assert!(validate_claim_change(&before, &after).is_err());
}

#[tokio::test]
#[ignore = "requires the explicitly isolated assistant scope PostgreSQL clone"]
async fn postgres_preparation_scope_clone_probe() {
    let url = std::env::var("COMMUNITYHERO_ASSISTANT_SCOPE_TEST_URL")
        .expect("explicit isolated PostgreSQL clone URL");
    assert!(url.starts_with("postgresql://") && url.contains("@127.0.0.1:"));
    assert!(url.contains("/communityhero_assistant_scope_test_remediation_20260923"));
    let db = Database::postgres(&url).await.unwrap();
    if let Database::Postgres { writer, .. } = &db {
        let database: String = sqlx::query_scalar("SELECT current_database()")
            .fetch_one(writer).await.unwrap();
        assert_eq!(database, "communityhero_assistant_scope_test_remediation_20260923");
    }
    let started = Instant::now();
    let full = db.read().await.unwrap();
    let full_ms = started.elapsed().as_secs_f64() * 1000.0;
    let expected = projection_of(&full).unwrap();
    let started = Instant::now();
    let (_, changed) = db.change_preparation_claim_observed(|scoped| {
        if *scoped != expected { return Err(internal("Preparation SQL projection differs from full workspace")); }
        Ok(())
    }).await.unwrap();
    let scoped_ms = started.elapsed().as_secs_f64() * 1000.0;
    assert!(!changed);
    eprintln!("preparation PG clone no-op: full read {full_ms:.2} ms; scoped transaction {scoped_ms:.2} ms; full bytes {}; scoped bytes {}",
        full.to_string().len(), expected.to_string().len());
    // This clone is disposable. Stage one fresh, non-video item after the
    // read-only probe. Do not alter the source/production database or launch
    // a model; the closure below calls claim only and has no worker spawn.
    let probe = format!("preparation-probe-{}", uuid::Uuid::new_v4());
    let post_id = format!("{probe}-post");
    let branch_id = format!("{probe}-branch");
    let item_id = format!("{probe}-item");
    let now = chrono::Utc::now().timestamp();
    let observed = chrono::DateTime::from_timestamp(now, 0).unwrap().to_rfc3339();
    db.change(|workspace| {
        if workspace["account"] != "LikeAvto" { return Err(internal("Probe clone is not LikeAvto")); }
        for job in crate::list_mut(workspace, "jobs") {
            if job["kind"] == "assistant" && job["purpose"] != "discussion"
                && matches!(job["status"].as_str(), Some("running" | "queued")) {
                job["status"] = json!("interrupted");
            }
        }
        crate::list_mut(workspace, "posts").push(json!({"id":post_id,"postKey":probe,"text":"Synthetic preparation probe"}));
        crate::list_mut(workspace, "branches").push(json!({"id":branch_id,"postId":post_id,
            "messages":[{"id":format!("{probe}-comment"),"text":"Synthetic question"}],"contextComplete":false}));
        crate::list_mut(workspace, "items").push(json!({"id":item_id,"itemId":format!("{probe}-comment"),
            "objectId":format!("{probe}-object"),"postKey":probe,"conversationKey":probe,
            "branchId":branch_id,"postId":post_id,"revision":1,"draft":"","workflow":"attention",
            "providerStatus":"new","createdAt":"2000-01-01T00:00:00Z","providerObservedAt":observed}));
        crate::media_queue::reconcile(workspace, &observed)?;
        Ok(())
    }).await.unwrap();
    let baseline = db.read().await.unwrap();
    let mut expected_after = baseline.clone();
    let expected_result = crate::auto_prepare::claim(&mut expected_after, now).unwrap();
    assert_eq!(expected_result.as_ref().map(|(_, request)| request["items"][0]["id"].as_str()),
        Some(Some(item_id.as_str())));
    let started = Instant::now();
    let (actual_result, changed) = db.change_preparation_claim_observed(|scoped| {
        crate::auto_prepare::claim(scoped, now)
    }).await.unwrap();
    let write_ms = started.elapsed().as_secs_f64() * 1000.0;
    assert!(changed);
    assert_eq!(actual_result.as_ref().map(|(_, request)| request["items"][0]["id"].as_str()),
        Some(Some(item_id.as_str())));
    let mut actual_after = db.read().await.unwrap();
    for table in OMITTED {
        assert!(actual_after[*table] == baseline[*table], "Excluded history changed: {table}");
    }
    let old_job_count = crate::list(&baseline, "jobs").len();
    for doc in [&mut expected_after, &mut actual_after] {
        for job in &mut doc["jobs"].as_array_mut().unwrap()[old_job_count..] {
            job["createdAt"] = json!("<claim-time>");
        }
        normalize_ids(doc);
    }
    assert!(actual_after == expected_after, "Persisted claim differs from full-document claim");
    assert!(crate::list(&actual_after, "jobs").iter().any(|j| j["id"] == "sync-old")
        || crate::list(&actual_after, "jobs").len() >= old_job_count + 1);
    eprintln!("preparation PG clone real claim: scoped write {write_ms:.2} ms; appended assistant job and updated item; excluded history unchanged");
    db.close().await;
}
