//! Conservative, deterministic interpretation of immutable operator observations.
//! A text edit is evidence of a difference, never proof of a factual error.
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};

fn rows<'a>(d: &'a Value, key: &str) -> &'a [Value] {
    d[key].as_array().map(Vec::as_slice).unwrap_or(&[])
}
fn s<'a>(v: &'a Value, key: &str) -> &'a str {
    v[key].as_str().unwrap_or("")
}
fn origin_key(e: &Value) -> String {
    json!([
        e["itemId"],
        e["sourceProposalId"],
        e["sourceProposalRevision"]
    ])
    .to_string()
}
fn decision_key(e: &Value) -> String {
    json!([origin_key(e), e["draftSessionId"]]).to_string()
}
fn action(v: &str) -> &str {
    match v {
        "reply_and_close" => "reply",
        "close_without_reply" => "close",
        "attention" => "needs_attention",
        "waiting" => "wait",
        _ => v,
    }
}
fn count(map: &mut BTreeMap<String, usize>, key: impl Into<String>) {
    *map.entry(key.into()).or_default() += 1;
}

fn rule_set_digest(origin: &Value) -> Option<String> {
    let manifest = origin["knowledgeManifest"].as_array()?;
    // A set of exact rule versions, independent of retrieval ordering. Facts and
    // transcripts are deliberately not presented as rule version changes.
    let mut rules: Vec<String> = manifest
        .iter()
        .filter(|v| v["kind"] == "rule")
        .map(Value::to_string)
        .collect();
    rules.sort();
    rules.dedup();
    Some(format!(
        "{:x}",
        Sha256::digest(json!(rules).to_string().as_bytes())
    ))
}

fn technical_metrics(d: &Value, now_seconds: i64) -> Value {
    let jobs: Vec<&Value> = rows(d, "jobs")
        .iter()
        .filter(|j| j["kind"] == "assistant")
        .collect();
    let mut statuses = BTreeMap::new();
    let mut models = BTreeMap::new();
    let mut elapsed = vec![];
    let mut completed_missing = 0;
    for j in &jobs {
        count(&mut statuses, j["status"].as_str().unwrap_or("unknown"));
        count(
            &mut models,
            j["runMetadata"]["model"].as_str().unwrap_or("unknown"),
        );
        if j["status"] == "completed" {
            if let Some(ms) = j["runMetadata"]["elapsedMs"].as_u64() {
                elapsed.push(ms);
            } else {
                completed_missing += 1;
            }
        }
    }
    elapsed.sort_unstable();
    let latency = if elapsed.is_empty() {
        Value::Null
    } else {
        let n = elapsed.len();
        json!({"minMs":elapsed[0],"maxMs":elapsed[n-1],"p50Ms":elapsed[n.div_ceil(2)-1],"p95Ms":elapsed[(n*95).div_ceil(100)-1],"method":"nearest rank over known completed-run adapter elapsedMs"})
    };
    let mut queue = BTreeMap::new();
    let mut open = 0;
    let mut sources = BTreeMap::new();
    for item in rows(d, "items") {
        if ![
            "attention",
            "needs_attention",
            "prepared",
            "waiting",
            "wait",
        ]
        .contains(&s(item, "workflow"))
        {
            continue;
        }
        open += 1;
        let (field, stamp) = if item["providerObservedAt"].is_string() {
            ("providerObservedAt", s(item, "providerObservedAt"))
        } else {
            ("observedAt", s(item, "observedAt"))
        };
        let parsed = chrono::DateTime::parse_from_rfc3339(stamp).ok();
        let bucket = match parsed {
            None => "unknownTimestamp",
            Some(t) => {
                count(&mut sources, field);
                let age = now_seconds.saturating_sub(t.timestamp());
                if age < -60 {
                    "futureTimestamp"
                } else if age <= 600 {
                    "within10Minutes"
                } else if age <= 3600 {
                    "between10MinutesAnd1Hour"
                } else {
                    "olderThan1Hour"
                }
            }
        };
        count(&mut queue, bucket);
    }
    json!({"qualitySignal":false,"generation":{"totalAssistantJobs":jobs.len(),"statusCounts":statuses,"modelCounts":models,"completedLatency":{"knownCount":elapsed.len(),"unknownCount":completed_missing,"distribution":latency,"scope":"completed runs with recorded adapter elapsedMs; not end-to-end queue latency"}},"openQueueFreshness":{"asOfUnixSeconds":now_seconds,"openItems":open,"ageBuckets":queue,"timestampFields":sources,"thresholdSeconds":[600,3600],"futureClockToleranceSeconds":60,"meaning":"age of last recorded observation, not proof that the external queue is complete","unknownWorkflowExcluded":true}})
}

/// One bounded changed span, measured in Unicode scalar values (not UTF-8 bytes).
/// Interior unchanged islands are deliberately not called independent edits.
pub fn text_difference(before: &str, after: &str) -> Value {
    const LIMIT: usize = 20_000;
    let a: Vec<char> = before.chars().take(LIMIT + 1).collect();
    let b: Vec<char> = after.chars().take(LIMIT + 1).collect();
    if a.len() > LIMIT || b.len() > LIMIT {
        return json!({"changed":before!=after,"bounded":true,"classification":"not_computed","reason":"text_limit"});
    }
    let prefix = a.iter().zip(&b).take_while(|(x, y)| x == y).count();
    let suffix = a[prefix..]
        .iter()
        .rev()
        .zip(b[prefix..].iter().rev())
        .take_while(|(x, y)| x == y)
        .count();
    let removed: String = a[prefix..a.len() - suffix].iter().collect();
    let inserted: String = b[prefix..b.len() - suffix].iter().collect();
    let kind = match (removed.is_empty(), inserted.is_empty()) {
        (true, true) => "unchanged",
        (true, false) => "inserted",
        (false, true) => "removed",
        _ => "replaced",
    };
    let joined = format!("{removed} {inserted}").to_lowercase();
    let mut signals = vec![];
    if joined.chars().any(|c| c.is_numeric()) {
        signals.push("numeric_text_changed");
    }
    if ["http", "www.", "@", "телефон", "whatsapp", "telegram"]
        .iter()
        .any(|p| joined.contains(p))
    {
        signals.push("contact_like_text_changed");
    }
    if [
        "обещ",
        "гарантир",
        "обязательно",
        "в следующем",
        "мы ответим",
        "we will",
        "guarantee",
        "promise",
    ]
    .iter()
    .any(|p| joined.contains(p))
    {
        signals.push("possible_commitment_changed");
    }
    if before.split_whitespace().collect::<Vec<_>>() == after.split_whitespace().collect::<Vec<_>>()
        && before != after
    {
        signals.push("whitespace_changed");
    }
    json!({"changed":before!=after,"classification":kind,"bounded":false,"commonPrefixCharacters":prefix,"removed":removed,"inserted":inserted,"signals":signals,"interpretation":"observable text difference; semantic cause requires review"})
}

pub fn report(d: &Value) -> Value {
    let all = rows(d, "feedback");
    let mut seen = BTreeSet::new();
    let mut valid = vec![];
    let mut legacy = 0;
    let mut malformed = 0;
    for e in all {
        if e["schemaVersion"] != 2 {
            legacy += 1;
            continue;
        }
        let id = e["eventId"]
            .as_str()
            .or_else(|| e["id"].as_str())
            .unwrap_or("");
        if id.is_empty() || s(e, "kind").is_empty() {
            malformed += 1;
            continue;
        }
        if seen.insert(id.to_owned()) {
            valid.push(e);
        }
    }
    // Stable chronological order; later confirmation in the same review session supersedes earlier one.
    valid.sort_by_key(|e| (s(e, "createdAt").to_owned(), s(e, "id").to_owned()));
    let operator_events:Vec<&&Value>=valid.iter().filter(|e| !s(e,"kind").starts_with("execution_")).collect();
    let verified_operator_events=operator_events.iter().filter(|e|e["actorVerified"]==true).count();
    let mut exposures = BTreeSet::new();
    let mut confirmations = BTreeMap::new();
    let mut selections = BTreeMap::new();
    let mut event_counts = BTreeMap::new();
    let mut execution = BTreeMap::<String, &Value>::new();
    let mut labels = vec![];
    let mut reviews = BTreeMap::new();
    let mut covered_approvals = BTreeSet::new();
    for e in &valid {
        count(&mut event_counts, s(e, "kind"));
        if s(e, "kind") == "review_confirmed" && !s(e, "approvalId").is_empty() {
            covered_approvals.insert(s(e, "approvalId").to_owned());
        }
        match s(e, "kind") {
            "proposal_presented" if !s(e, "sourceProposalId").is_empty() => {
                exposures.insert(origin_key(e));
            }
            "review_confirmed"
                if !s(e, "sourceProposalId").is_empty() && !s(e, "draftSessionId").is_empty() =>
            {
                confirmations.insert(decision_key(e), *e);
            }
            "action_selected"
                if !s(e, "sourceProposalId").is_empty() && !s(e, "draftSessionId").is_empty() =>
            {
                selections.insert(decision_key(e), *e);
            }
            "execution_verified" | "execution_failed" | "execution_unknown"
                if !s(e, "operationId").is_empty() =>
            {
                execution.insert(s(e, "operationId").to_owned(), *e);
            }
            "feedback_labelled" => labels.push((*e).clone()),
            "candidate_reviewed" => {
                reviews.insert(s(e, "candidateId").to_owned(), *e);
            }
            _ => {}
        }
    }
    let mut no_edit = 0;
    let mut edited = 0;
    let mut incomplete = 0;
    let mut non_model = 0;
    let mut confirmed_origins = BTreeSet::new();
    let mut shifts = BTreeMap::new();
    let mut breakdown = BTreeMap::<String, BTreeMap<String, usize>>::new();
    let mut candidates = vec![];
    let mut observed_shifts = BTreeMap::new();
    let mut comparable_selections = 0;
    let mut selection_refs = vec![];
    for e in selections.values() {
        let from = action(s(&e["origin"], "kind"));
        let to = action(s(e, "action"));
        let allowed = ["reply", "close", "needs_attention", "wait"];
        if s(&e["origin"], "prepareRunId").is_empty()
            || !allowed.contains(&from)
            || !allowed.contains(&to)
        {
            continue;
        }
        comparable_selections += 1;
        if from != to {
            count(&mut observed_shifts, format!("{from}->{to}"));
            selection_refs.push(json!({"eventId":e["eventId"].as_str().unwrap_or_else(||s(e,"id")),"sourceProposalId":e["sourceProposalId"],"sourceProposalRevision":e["sourceProposalRevision"],"itemId":e["itemId"],"draftSessionId":e["draftSessionId"],"from":from,"to":to}));
        }
    }
    for e in confirmations.values() {
        confirmed_origins.insert(origin_key(e));
        if !s(e, "approvalId").is_empty() {
            covered_approvals.insert(s(e, "approvalId").to_owned());
        }
        let original = &e["origin"];
        if s(original, "prepareRunId").is_empty() {
            non_model += 1;
            continue;
        }
        let before = original["text"].as_str();
        let after = e["text"].as_str();
        let from = action(s(original, "kind"));
        let to = action(s(e, "action"));
        if from.is_empty() || to.is_empty() || before.is_none() || after.is_none() {
            incomplete += 1;
            continue;
        }
        let diff = text_difference(before.unwrap(), after.unwrap());
        let changed = diff["changed"] == true;
        if from == to && !changed {
            no_edit += 1;
        } else {
            edited += 1;
        }
        if from != to {
            count(&mut shifts, format!("{from}->{to}"));
        }
        for dimension in [
            "platform",
            "model",
            "policyVersion",
            "promptVersion",
            "instructionSha256",
        ] {
            let value = e
                .get(dimension)
                .filter(|v| !v.is_null())
                .or_else(|| original.get(dimension))
                .filter(|v| !v.is_null())
                .or_else(|| {
                    if dimension == "policyVersion" {
                        original.get("knowledgePolicyVersion")
                    } else {
                        None
                    }
                })
                .filter(|v| !v.is_null())
                .or_else(|| original["generationMetadata"].get(dimension))
                .filter(|v| !v.is_null());
            let key = value
                .map(|v| {
                    v.as_str()
                        .map(str::to_owned)
                        .unwrap_or_else(|| v.to_string())
                })
                .unwrap_or_else(|| "unknown".into());
            count(breakdown.entry(dimension.into()).or_default(), key);
        }
        count(
            breakdown.entry("ruleSetDigest".into()).or_default(),
            rule_set_digest(original).unwrap_or_else(|| "unknown".into()),
        );
        let tags = e
            .get("tags")
            .or_else(|| original.get("tags"))
            .and_then(Value::as_array);
        if let Some(tags) = tags {
            for tag in tags {
                if let Some(t) = tag.as_str() {
                    count(breakdown.entry("tags".into()).or_default(), t);
                }
            }
        }
        if changed || from != to {
            let event_id = e["eventId"].as_str().unwrap_or_else(|| s(e, "id"));
            let candidate_id = format!("feedback-candidate-{event_id}");
            let review = reviews.get(&candidate_id);
            let state = review
                .map(|v| s(v, "decision"))
                .filter(|v| ["approved", "rejected"].contains(v))
                .unwrap_or("pending_review");
            candidates.push(json!({"id":candidate_id,"status":state,"source":"heuristic","classification":"hypothesis","scope":"single_finalized_comparison","eventId":event_id,"actor":e["actor"],"actorVerified":e["actorVerified"],"itemId":e["itemId"],"sourceProposalId":e["sourceProposalId"],"sourceProposalRevision":e["sourceProposalRevision"],"draftSessionId":e["draftSessionId"],"proposalId":e["proposalId"],"approvalId":e["approvalId"],"origin":original,"beforeAction":from,"afterAction":to,"difference":diff,"review":review,"promotesPolicy":false,"explanation":"An operator confirmed a different text or action. This does not establish a factual error or a general rule; inspect context and sources before changing guidance."}));
        }
    }
    let mut execution_counts = BTreeMap::new();
    for e in execution.values() {
        count(&mut execution_counts, s(e, "kind"));
    }
    let mut patterns = BTreeMap::<String, BTreeSet<String>>::new();
    for c in &candidates {
        let id = s(c, "id").to_owned();
        if c["beforeAction"] != c["afterAction"] {
            patterns
                .entry(format!(
                    "action:{}->{}",
                    s(c, "beforeAction"),
                    s(c, "afterAction")
                ))
                .or_default()
                .insert(id.clone());
        }
        for signal in rows(&c["difference"], "signals") {
            if let Some(signal) = signal.as_str() {
                patterns
                    .entry(signal.to_owned())
                    .or_default()
                    .insert(id.clone());
            }
        }
        if c["difference"]["changed"] == true && rows(&c["difference"], "signals").is_empty() {
            patterns
                .entry("text_changed_reason_unknown".into())
                .or_default()
                .insert(id);
        }
    }
    let patterns: Vec<Value> = patterns.into_iter().map(|(signal,ids)|json!({"signal":signal,"distinctFinalizedComparisons":ids.len(),"candidateIds":ids,"source":"heuristic","classification":"hypothesis","requiresContextReview":true})).collect();
    let matched = exposures.intersection(&confirmed_origins).count();
    let uncovered_approvals = rows(d, "approvals")
        .iter()
        .filter(|a| !covered_approvals.contains(s(a, "id")))
        .count();
    json!({"schemaVersion":2,"coverage":{"totalStoredEvents":all.len(),"legacyEventsExcluded":legacy,"malformedEventsExcluded":malformed,"uniqueV2Events":valid.len(),"operatorEventsWithVerifiedActor":verified_operator_events,"operatorEventsWithoutVerifiedActor":operator_events.len()-verified_operator_events,"distinctPresentedProposalVersions":exposures.len(),"distinctConfirmedReviewSessions":confirmations.len(),"confirmedWithoutPresentation":confirmed_origins.difference(&exposures).count(),"incompleteComparisons":incomplete,"nonModelComparisonsExcluded":non_model,"approvalsWithoutLinkedConfirmation":uncovered_approvals},"metrics":{"confirmedAmongPresented":{"numerator":matched,"denominator":exposures.len(),"unit":"distinct item and source proposal version","meaning":"presentation does not prove reading"},"unchangedAmongComparableConfirmed":{"numerator":no_edit,"denominator":no_edit+edited,"unit":"final confirmation per draft session and source proposal version"},"confirmedWithTextOrActionChanges":edited,"actionShifts":shifts,"observedActionSelectionShifts":{"counts":observed_shifts,"comparableSessions":comparable_selections,"unit":"latest action selection per draft session and source proposal version","finalQualityVote":false,"confirmed":false,"trace":selection_refs}},"eventCounts":event_counts,"technical":technical_metrics(d,chrono::Utc::now().timestamp()),"execution":{"latestOutcomePerOperation":execution_counts,"qualitySignal":false},"breakdown":{"confirmedComparableBy":breakdown,"missingDimension":"unknown; never inferred from current mutable item","policyVersionMeaning":"knowledge selection schema; ruleSetDigest groups the immutable rule versions"},"candidates":candidates,"patterns":patterns,"explicitLabels":labels,"limitations":["Autosaves, clearing, navigation and abandoned dialogs are not negative votes.","No confirmation is inferred from an approval lacking linked observations.","Candidate approval is review metadata, not automatic knowledge or policy promotion.","No measured quality score without human-reviewed labels and explicit denominator."]})
}

#[cfg(test)]
mod tests {
    use super::*;
    fn event(id: &str, kind: &str) -> Value {
        json!({"id":id,"schemaVersion":2,"kind":kind,"itemId":"i","sourceProposalId":"p","sourceProposalRevision":1,"draftSessionId":"s","createdAt":id,"origin":{"text":"Привет 👋","kind":"reply","prepareRunId":"run"},"text":"Привет 👋","action":"reply","approvalId":"a"})
    }
    #[test]
    fn autosaves_and_missing_exposure_are_not_votes() {
        let r = report(
            &json!({"feedback":[event("1","draft_saved"),event("2","proposal_cleared"),{"kind":"draft_edit"}],"approvals":[{"id":"a"}]}),
        );
        assert_eq!(
            r["metrics"]["unchangedAmongComparableConfirmed"]["denominator"],
            0
        );
        assert_eq!(r["coverage"]["legacyEventsExcluded"], 1);
        assert_eq!(r["coverage"]["approvalsWithoutLinkedConfirmation"], 1);
    }
    #[test]
    fn deduplicates_exposure_and_confirmations_and_undo() {
        let mut edited = event("3", "review_confirmed");
        edited["text"] = json!("Здравствуйте");
        let r = report(
            &json!({"feedback":[event("1","proposal_presented"),event("2","proposal_presented"),edited,event("4","review_confirmed"),event("4","review_confirmed")]}),
        );
        assert_eq!(r["coverage"]["distinctPresentedProposalVersions"], 1);
        assert_eq!(
            r["metrics"]["unchangedAmongComparableConfirmed"]["numerator"],
            1
        );
        assert!(r["candidates"].as_array().unwrap().is_empty());
    }
    #[test]
    fn action_shift_is_candidate_but_execution_failure_is_not_quality() {
        let mut confirmed = event("2", "review_confirmed");
        confirmed["action"] = json!("close");
        confirmed["text"] = json!("");
        let mut failed = event("3", "execution_failed");
        failed["operationId"] = json!("op");
        let r = report(&json!({"feedback":[event("1","proposal_presented"),confirmed,failed]}));
        assert_eq!(r["metrics"]["actionShifts"]["reply->close"], 1);
        assert_eq!(r["candidates"][0]["status"], "pending_review");
        assert_eq!(r["candidates"][0]["promotesPolicy"], false);
        assert_eq!(r["execution"]["qualitySignal"], false);
    }
    #[test]
    fn unicode_diff_and_bound_do_not_claim_facts() {
        let diff = text_difference("Привет 👋 12", "Привет 👋 15");
        assert_eq!(diff["removed"], "2");
        assert_eq!(diff["inserted"], "5");
        assert_eq!(diff["signals"], json!(["numeric_text_changed"]));
        assert_eq!(text_difference(&"я".repeat(20_001), "")["bounded"], true);
        assert_eq!(text_difference("", "👋")["classification"], "inserted");
    }
    #[test]
    fn manual_proposal_is_not_ai_acceptance_and_review_never_promotes_policy() {
        let mut manual = event("1", "review_confirmed");
        manual["origin"]["prepareRunId"] = Value::Null;
        let r = report(&json!({"feedback":[manual]}));
        assert_eq!(r["coverage"]["nonModelComparisonsExcluded"], 1);
        assert_eq!(
            r["metrics"]["unchangedAmongComparableConfirmed"]["denominator"],
            0
        );
        let mut changed = event("2", "review_confirmed");
        changed["text"] = json!("Привет 123");
        let review = json!({"id":"3","schemaVersion":2,"kind":"candidate_reviewed","candidateId":"feedback-candidate-2","decision":"approved"});
        let r = report(&json!({"feedback":[changed,review]}));
        assert_eq!(r["candidates"][0]["status"], "approved");
        assert_eq!(r["candidates"][0]["promotesPolicy"], false);
        assert_eq!(r["patterns"][0]["distinctFinalizedComparisons"], 1);
    }
    #[test]
    fn breakdown_uses_captured_generation_not_current_item() {
        let mut e = event("1", "review_confirmed");
        e["origin"]["generationMetadata"] =
            json!({"model":"captured-model","instructionSha256":"original-hash"});
        e["origin"]["tags"] = json!(["needs_fact"]);
        let r = report(&json!({"feedback":[e],"items":[{"id":"i","platform":"must-not-infer"}]}));
        assert_eq!(
            r["breakdown"]["confirmedComparableBy"]["model"]["captured-model"],
            1
        );
        assert_eq!(
            r["breakdown"]["confirmedComparableBy"]["platform"]["unknown"],
            1
        );
        assert_eq!(
            r["breakdown"]["confirmedComparableBy"]["tags"]["needs_fact"],
            1
        );
    }
    #[test]
    fn rule_set_digest_tracks_versions_not_order_or_policy_schema() {
        let a = json!({"knowledgeManifest":[{"kind":"rule","entryId":"r1","versionId":"v1"},{"kind":"rule","entryId":"r2","versionId":"v2"}]});
        let b = json!({"knowledgeManifest":[{"kind":"rule","entryId":"r2","versionId":"v2"},{"kind":"rule","entryId":"r1","versionId":"v1"},{"kind":"fact","entryId":"f","versionId":"f1"}]});
        assert_eq!(rule_set_digest(&a), rule_set_digest(&b));
        let mut newer = a.clone();
        newer["knowledgeManifest"][0]["versionId"] = json!("v3");
        assert_ne!(rule_set_digest(&a), rule_set_digest(&newer));
        assert_eq!(rule_set_digest(&json!({})), None);
        let mut e = event("1", "review_confirmed");
        e["origin"]["knowledgePolicyVersion"] = json!(1);
        e["origin"]["knowledgeManifest"] = a["knowledgeManifest"].clone();
        let r = report(&json!({"feedback":[e]}));
        assert_eq!(
            r["breakdown"]["confirmedComparableBy"]["policyVersion"]["1"],
            1
        );
        assert_eq!(
            r["breakdown"]["confirmedComparableBy"]["ruleSetDigest"][rule_set_digest(&a).unwrap()],
            1
        );
    }
    #[test]
    fn observed_wait_and_attention_are_not_final_reviews_or_autosave_votes() {
        let mut wait = event("1", "action_selected");
        wait["action"] = json!("waiting");
        let mut attention = event("2", "action_selected");
        attention["action"] = json!("attention");
        let r = report(&json!({"feedback":[wait,attention,event("3","draft_saved")]}));
        let metric = &r["metrics"]["observedActionSelectionShifts"];
        assert_eq!(metric["counts"]["reply->needs_attention"], 1);
        assert!(metric["counts"]["reply->wait"].is_null());
        assert_eq!(metric["comparableSessions"], 1);
        assert_eq!(metric["finalQualityVote"], false);
        assert_eq!(
            r["metrics"]["unchangedAmongComparableConfirmed"]["denominator"],
            0
        );
        assert!(r["candidates"].as_array().unwrap().is_empty());
    }
    #[test]
    fn technical_metrics_keep_failed_generations_and_unknown_freshness_separate() {
        let now = chrono::DateTime::parse_from_rfc3339("2026-09-22T12:00:00Z")
            .unwrap()
            .timestamp();
        let d = json!({"jobs":[
            {"kind":"assistant","status":"completed","runMetadata":{"elapsedMs":100,"model":"m"}},
            {"kind":"assistant","status":"completed"},
            {"kind":"assistant","status":"failed"},
            {"kind":"sync","status":"completed","runMetadata":{"elapsedMs":1}}
        ],"items":[
            {"workflow":"attention","providerObservedAt":"2026-09-22T11:55:00Z"},
            {"workflow":"prepared","providerObservedAt":"2026-09-22T10:00:00Z"},
            {"workflow":"waiting","providerObservedAt":"bad-date"},
            {"workflow":"attention","providerObservedAt":"2026-09-22T12:05:00Z"},
            {"workflow":"closed","providerObservedAt":"2026-09-22T10:00:00Z"}
        ]});
        let t = technical_metrics(&d, now);
        assert_eq!(t["generation"]["totalAssistantJobs"], 3);
        assert_eq!(t["generation"]["modelCounts"]["unknown"], 2);
        assert_eq!(t["generation"]["completedLatency"]["knownCount"], 1);
        assert_eq!(t["generation"]["completedLatency"]["unknownCount"], 1);
        assert_eq!(t["openQueueFreshness"]["openItems"], 4);
        for bucket in [
            "within10Minutes",
            "olderThan1Hour",
            "unknownTimestamp",
            "futureTimestamp",
        ] {
            assert_eq!(t["openQueueFreshness"]["ageBuckets"][bucket], 1);
        }
        assert_eq!(t["qualitySignal"], false);
    }
}
