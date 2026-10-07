//! Synthetic integrated R7 acceptance. ROOT registers this module and runs it
//! against the combined source; this file makes no provider/model calls.
use crate::*;
use std::collections::BTreeSet;

const COUNT: usize = 640;
const WINDOW: usize = 80;

fn actor() -> operator_auth::Actor {
    operator_auth::Actor::local_owner("wave-offline-acceptance")
}

// Expected readiness is specified by the workload, before any frontier call.
// Three ready and five independently blocked recipients per eight. A different
// company deliberately uses the same external IDs, isolating by binding/store.
fn mixed(profile: accounts::Profile) -> (Value, Vec<Value>, BTreeSet<String>) {
    let (base, _) = operator_editorial::tests::fixture();
    let mut d = crate::empty();
    accounts::initialize(&mut d, profile).unwrap();
    d["feedback"] = json!([]);
    d["knowledge_entries"] = json!([]);
    d["knowledge_versions"] = json!([]);
    let mut refs = vec![];
    let mut ready = BTreeSet::new();
    for index in 0..COUNT {
        let key = format!("wave-{index}");
        let mut item = base["items"][0].clone();
        // The seed fixture already created its own proposal and bumped this
        // item. Rebuild pre-proposal input rather than inheriting that state.
        item["revision"] = json!(1);
        item["workflow"] = json!("attention");
        item["draft"] = json!("");
        item["id"] = json!(key);
        item["itemId"] = json!(format!("shared-external-{index}"));
        item["objectId"] = json!(format!("shared-object-{index}"));
        item["postId"] = json!(format!("post-{index}"));
        item["postKey"] = item["postId"].clone();
        item["branchId"] = json!(format!("branch-{index}"));
        item["conversationKey"] = json!(format!("conversation-{index}"));
        item["connectorBinding"] = d["connectorBinding"].clone();
        let mut post = base["posts"][0].clone();
        post["id"] = item["postId"].clone();
        post["postKey"] = item["postKey"].clone();
        post["objectId"] = item["objectId"].clone();
        post["connectorBinding"] = item["connectorBinding"].clone();
        let mut branch = base["branches"][0].clone();
        branch["id"] = item["branchId"].clone();
        branch["postId"] = item["postId"].clone();
        list_mut(&mut d, "items").push(item);
        list_mut(&mut d, "posts").push(post);
        list_mut(&mut d, "branches").push(branch);
    }
    for index in 0..COUNT {
        let key = format!("wave-{index}");
        let p = create_proposal(&mut d, &json!({"itemId":key,
            "expectedRevision":1,"kind":"reply_and_close",
            "text":format!("Synthetic exact reply {index}")})).unwrap();
        let reference = json!({"id":p["id"],"revision":p["revision"]});
        refs.push(reference.clone());
        match index % 8 {
            0 | 5 | 6 => { ready.insert(p["id"].as_str().unwrap().to_owned()); }
            1 => {
                // A different canonical alias of the same scoped recipient.
                let target = row(&d, "items", &key).unwrap().clone();
                list_mut(&mut d, "operations").push(json!({"id":format!("unknown-{index}"),
                    "itemId":format!("old-alias-{index}"),"status":"unknown",
                    "target":target,"evidence":{"syntheticPreserve":"exact-uncertain-operation"}}));
            }
            2 => {
                list_mut(&mut d, "jobs").push(json!({"id":format!("paid-review-{index}"),
                    "kind":"editorial_review","purpose":"editorial_review","status":"running",
                    "editorialReferences":[reference],
                    "preparationStages":{"first":{"status":"completed", "result":{
                        "text":"Retained paid synthetic output", "proposals":[p]}}}}));
            }
            3 => {
                // Introduce mandatory media after draft capture; no full audio.
                row_mut(&mut d, "posts", &format!("post-{index}")).unwrap()["sourceUrl"] =
                    json!(format!("https://www.youtube.com/watch?v=wave-{index}"));
            }
            4 => { row_mut(&mut d, "items", &key).unwrap()["workflow"] = json!("waiting"); }
            _ => { row_mut(&mut d, "proposals", p["id"].as_str().unwrap()).unwrap()["status"] = json!("stale"); }
        }
    }
    assert_eq!(ready.len(), 240);
    (d, refs, ready)
}

fn ids(refs: &[Value]) -> BTreeSet<String> {
    refs.iter().map(|r| r["id"].as_str().unwrap().to_owned()).collect()
}

fn review_body(preview: &Value, request_id: &str) -> Value {
    json!({"requestId":request_id,"proposals":preview["proposals"],
        "previewDigest":preview["previewDigest"],"operatorReview":{"version":1,
        "method":operator_editorial::METHOD,
        "entries":preview["entries"].as_array().unwrap().iter().map(|entry| json!({
            "candidate":entry["candidate"],"checks":{"intent":"pass", "companyRules":"pass", "factualScope":"pass"},
            "reason":"Synthetic integration judgment of this exact supplied source and wording"
        })).collect::<Vec<_>>()}})
}

#[test]
fn wave_640_frontier_is_exhaustive_company_scoped_and_native_review_approval_connected() {
    for profile in [accounts::Profile::LikeAvto, accounts::Profile::BawRussia] {
        let (mut d, refs, expected_ready) = mixed(profile);
        let original = d.clone();
        let mut observed_ready = BTreeSet::new();
        let mut observed_held = BTreeSet::new();
        for (window_index, window) in refs.chunks(WINDOW).enumerate() {
            let frontier = operator_frontier::capture(&d, &actor(), &json!({"proposals":window})).unwrap();
            assert_eq!(frontier["account"], d["account"]);
            assert_eq!(frontier["connectorBinding"], d["connectorBinding"]);
            assert_eq!(frontier["requested"], json!(window));
            let ready = frontier["readyForOperatorReview"].as_array().unwrap();
            let held: Vec<Value> = frontier["held"].as_array().unwrap().iter()
                .map(|row| {
                    assert!(row["reason"].as_str().is_some_and(|s| !s.is_empty()));
                    assert!(row["stage"].as_str().is_some_and(|s| !s.is_empty()));
                    row["reference"].clone()
                }).collect();
            let wanted: BTreeSet<_> = ids(window).intersection(&expected_ready).cloned().collect();
            assert_eq!(ids(ready), wanted, "company={}, window={window_index}", profile.key());
            let wanted_ready: Vec<_> = window.iter().filter(|r| expected_ready.contains(r["id"].as_str().unwrap())).cloned().collect();
            let wanted_held: Vec<_> = window.iter().filter(|r| !expected_ready.contains(r["id"].as_str().unwrap())).cloned().collect();
            assert_eq!(ready, &wanted_ready, "exact ready references and order retained");
            assert_eq!(held, wanted_held, "exact held revisions and order retained");
            assert_eq!(ids(ready).intersection(&ids(&held)).count(), 0);
            assert_eq!(ids(ready).union(&ids(&held)).cloned().collect::<BTreeSet<_>>(), ids(window));
            observed_ready.extend(ids(ready));
            observed_held.extend(ids(&held));
            assert_eq!(d, original, "frontier is read-only");
            let native = operator_editorial::capture(&d, &actor(), &json!({"proposals":ready})).unwrap();
            assert_eq!(frontier["preview"], native, "frontier reuses exact native preview and digest");
            assert!(create_approval(&mut d.clone(), &actor(), &json!({"proposals":ready})).is_err(),
                "frontier eligibility supplies no semantic review or approval");
        }
        assert_eq!(observed_ready, expected_ready);
        assert_eq!(observed_held.len(), 400);

        // Cross the frontier -> semantic receipt -> ordinary approval boundary.
        // Keep each admission at the actual bound, not a fictional 640-item API.
        for (window_index, window) in refs.chunks(WINDOW).enumerate() {
            let frontier = operator_frontier::capture(&d, &actor(), &json!({"proposals":window})).unwrap();
            let ready = frontier["readyForOperatorReview"].clone();
            let body = review_body(&frontier["preview"], &format!("wave-review-{}-{window_index}", profile.key()));
            let admitted = operator_editorial::admit(&mut d, &actor(), &body).unwrap();
            let after_review = d.clone();
            let replay = operator_editorial::admit(&mut d, &actor(), &body).unwrap();
            assert_eq!(replay["jobId"], admitted["jobId"]);
            assert_eq!(replay["replayed"], true);
            assert_eq!(d, after_review, "replay does not create a second receipt/job");
            create_approval(&mut d, &actor(), &json!({"proposals":ready})).unwrap();
        }
        assert_eq!(d["operations"], original["operations"], "UNKNOWN aliases remain unchanged; no execution");
        for paid in list(&original, "jobs") {
            assert_eq!(row(&d, "jobs", paid["id"].as_str().unwrap()).unwrap(), paid, "paid output/owner preserved");
        }
        for reference in refs.iter().filter(|r| !expected_ready.contains(r["id"].as_str().unwrap())) {
            assert_eq!(row(&d, "proposals", reference["id"].as_str().unwrap()).unwrap(),
                row(&original, "proposals", reference["id"].as_str().unwrap()).unwrap(), "held proposal unchanged");
        }
        assert_eq!(list(&d, "approvals").len(), COUNT / WINDOW);
    }
}

#[test]
fn wave_frontier_all_held_late_revision_and_foreign_binding_fail_without_side_effects() {
    let (d, refs, expected_ready) = mixed(accounts::Profile::LikeAvto);
    let held: Vec<_> = refs.iter().filter(|r| !expected_ready.contains(r["id"].as_str().unwrap())).take(WINDOW).cloned().collect();
    let before = d.clone();
    let frontier = operator_frontier::capture(&d, &actor(), &json!({"proposals":held})).unwrap();
    assert_eq!(frontier["account"], d["account"]);
    assert_eq!(frontier["connectorBinding"], d["connectorBinding"]);
    assert_eq!(frontier["readyForOperatorReview"], json!([]));
    assert_eq!(frontier["held"].as_array().unwrap().len(), WINDOW);
    assert_eq!(frontier["preview"], Value::Null);
    assert_eq!(d, before);
    let first_ready = refs.iter().find(|r| expected_ready.contains(r["id"].as_str().unwrap())).unwrap();
    let mut late = d.clone();
    row_mut(&mut late, "proposals", first_ready["id"].as_str().unwrap()).unwrap()["revision"] = json!(2);
    assert!(operator_frontier::capture(&late, &actor(), &json!({"proposals":[first_ready]})).is_err(),
        "old generation reference is a global conflict, never silently moved");
    let mut foreign = d.clone();
    row_mut(&mut foreign, "items", "wave-0").unwrap()["connectorBinding"] = accounts::Profile::BawRussia.binding();
    let f = operator_frontier::capture(&foreign, &actor(), &json!({"proposals":[first_ready]})).unwrap();
    assert_eq!(f["readyForOperatorReview"], json!([]));
    assert_eq!(f["held"].as_array().unwrap().len(), 1, "foreign recipient cannot become reviewable");
    assert!(operator_frontier::capture(&d, &actor(), &json!({"proposals":refs})).is_err(),
        "640-item test uses windows, preserving native 100-reference hard bound");
}

#[tokio::test]
async fn wave_640_separate_company_stores_reopen_with_paid_and_unknown_data_exact() {
    let temp = tempfile::tempdir().unwrap();
    for profile in [accounts::Profile::LikeAvto, accounts::Profile::BawRussia] {
        let file = temp.path().join(format!("{}.sqlite", profile.key()));
        let (initial, refs, expected_ready) = mixed(profile);
        let db = storage::Database::Sqlite(crate::open_db(&file).await.unwrap());
        db.change(|d| { *d = initial.clone(); Ok(()) }).await.unwrap();
        let before = db.read().await.unwrap();
        db.close().await;
        let reopened = storage::Database::Sqlite(crate::open_db(&file).await.unwrap());
        let restored = reopened.read().await.unwrap();
        assert_eq!(restored, before, "cold database reopen preserves exact durable paid/UNKNOWN state");
        let mut actual = BTreeSet::new();
        for window in refs.chunks(WINDOW) {
            let frontier = operator_frontier::capture(&restored, &actor(), &json!({"proposals":window})).unwrap();
            actual.extend(ids(frontier["readyForOperatorReview"].as_array().unwrap()));
        }
        assert_eq!(actual, expected_ready);
        assert_eq!(restored["account"], profile.display());
        assert_eq!(restored["connectorBinding"], profile.binding());
        reopened.close().await;
    }
}

#[tokio::test]
async fn wave_640_native_drain_writer_reopen_and_successor_fence_preserve_paid_unknown() {
    use runtime_lifecycle::{AdmissionClass, OwnerToken, SettledNative};
    let temp = tempfile::tempdir().unwrap();
    for profile in [accounts::Profile::LikeAvto, accounts::Profile::BawRussia] {
        let file = temp.path().join(format!("drain-{}.sqlite", profile.key()));
        let (mut initial, _, _) = mixed(profile);
        let ledger = runtime_lifecycle::ledger_digest(&initial).unwrap();
        let owner = OwnerToken { account: profile.display().into(), runtime_id: "wave-owner-one".into(),
            release_sha256: "a".repeat(64), epoch: 1 };
        runtime_lifecycle::initialize(&mut initial, owner.clone(), &"b".repeat(64), &ledger).unwrap();
        let db = storage::Database::Sqlite(crate::open_db(&file).await.unwrap());
        db.change(|d| { *d = initial; Ok(()) }).await.unwrap();
        let drain = db.change(|d| runtime_lifecycle::begin_drain(d, &owner,
            &"c".repeat(64), "wave-update", false)).await.unwrap();
        let after_drain = db.read().await.unwrap();
        // These callbacks execute against current locked writer state, after an
        // earlier preflight captured the old generation. No simulated gate flag.
        for class in [AdmissionClass::SourceRead, AdmissionClass::Media,
            AdmissionClass::Preparation, AdmissionClass::SocialDispatch] {
            assert!(db.change(|d| {
                runtime_lifecycle::require_admission(d, &owner, class)?;
                list_mut(d, "jobs").push(json!({"id":"late-old-generation", "status":"running"}));
                Ok(())
            }).await.is_err());
            assert_eq!(db.read().await.unwrap(), after_drain, "rejected callback has no partial reservation");
        }
        db.close().await;
        let db = storage::Database::Sqlite(crate::open_db(&file).await.unwrap());
        assert_eq!(db.read().await.unwrap(), after_drain, "restart never implicitly turns draining into running");
        let quiet = |token: &OwnerToken| SettledNative { owner: token.clone(), application_tasks: 0,
            provider_queued: 0, provider_dispatched: 0, provider_contained: true,
            credential_writers: 0, unresolved_effects: 0 };
        assert!(db.change(|d| runtime_lifecycle::mark_drained(d, &drain, &quiet(&drain))).await.is_err(),
            "live durable paid jobs block even with a synthetic quiet native observation");
        // Existing paid completion/checkpoint writes remain possible in drain.
        db.change(|d| {
            for job in list_mut(d, "jobs") { job["status"] = json!("paused"); }
            Ok(())
        }).await.unwrap();
        let checkpointed = db.read().await.unwrap();
        let mut refreshing = quiet(&drain); refreshing.credential_writers = 1;
        assert!(db.change(|d| runtime_lifecycle::mark_drained(d, &drain, &refreshing)).await.is_err(),
            "underway auth refresh blocks release even when job labels look quiet");
        let transfer = db.change(|d| runtime_lifecycle::mark_drained(d, &drain, &quiet(&drain))).await.unwrap();
        db.change(|d| runtime_lifecycle::commit_stop_checkpoint(d, &drain, &transfer)).await.unwrap();
        db.close().await;
        let db = storage::Database::Sqlite(crate::open_db(&file).await.unwrap());
        let stopped = db.read().await.unwrap();
        assert_eq!(stopped["runtimeLifecycle"]["phase"], "stopped");
        assert!(db.change(|d| runtime_lifecycle::accept_successor(d, &drain, &transfer,
            "wave-owner-two", &"d".repeat(64), &"e".repeat(64), false)).await.is_err(),
            "wrong exact release cannot accept transfer");
        assert_eq!(db.read().await.unwrap(), stopped);
        let successor = db.change(|d| runtime_lifecycle::accept_successor(d, &drain, &transfer,
            "wave-owner-two", &"c".repeat(64), &"e".repeat(64), false)).await.unwrap();
        let final_state = db.read().await.unwrap();
        assert_eq!(final_state["jobs"], checkpointed["jobs"], "all paused paid output preserved across successor");
        assert_eq!(final_state["operations"], checkpointed["operations"], "all exact UNKNOWN records preserved");
        assert_eq!(runtime_lifecycle::ledger_digest(&final_state).unwrap(), transfer["ledgerSha256"].as_str().unwrap());
        assert!(runtime_lifecycle::require_admission(&final_state, &owner, AdmissionClass::Media).is_err());
        assert!(runtime_lifecycle::require_admission(&final_state, &drain, AdmissionClass::Preparation).is_err());
        runtime_lifecycle::require_admission(&final_state, &successor, AdmissionClass::Preparation).unwrap();
        db.close().await;
    }
}
