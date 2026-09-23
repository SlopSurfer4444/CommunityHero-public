//! Opt-in profiling against a private, offline workspace snapshot. No bridges.
use super::*;

#[test]
#[ignore = "requires COMMUNITYHERO_PROFILE_SNAPSHOT; never accesses a live database"]
fn profile_preparation_snapshot() {
    let path = std::env::var("COMMUNITYHERO_PROFILE_SNAPSHOT").expect("snapshot path");
    let started = std::time::Instant::now();
    let mut d: Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    eprintln!("profile parse_ms={} items={} posts={} jobs={}", started.elapsed().as_millis(), list(&d,"items").len(),list(&d,"posts").len(),list(&d,"jobs").len());
    let now = chrono::Utc::now().timestamp();
    let at = chrono::Utc::now().to_rfc3339();
    // Keep provider observation eligible on the offline fixture, without editing
    // source bodies, identity, transcripts, or live state.
    for item in list_mut(&mut d,"items") { item["providerObservedAt"] = json!(at); }
    let step = std::time::Instant::now();
    media_queue::reconcile(&mut d,&at).unwrap();
    eprintln!("profile media_reconcile_ms={}",step.elapsed().as_millis());
    let step = std::time::Instant::now();
    let attention:Vec<Value>=list(&d,"items").iter().filter(|i|i["workflow"]=="attention").cloned().collect();
    let states=media_queue::preparation_states(&d,&attention,&at).unwrap();
    let waits=states.values().filter(|s|s.is_some()).count();
    eprintln!("profile media_gates_ms={} waits={waits}",step.elapsed().as_millis());
    let step = std::time::Instant::now();
    auto_prepare::reconcile_stale(&mut d,now);
    eprintln!("profile stale_ms={}",step.elapsed().as_millis());
    let step = std::time::Instant::now();
    let receipt=preparation_restart::plan(&mut d,"offline-performance-check",false,now).unwrap();
    eprintln!("profile restart_preview_ms={} eligible={}",step.elapsed().as_millis(),receipt["eligibleCount"]);
    let step = std::time::Instant::now();
    let claim = auto_prepare::claim(&mut d,now).unwrap();
    eprintln!("profile claim_ms={} claimed={}",step.elapsed().as_millis(),claim.is_some());
    assert!(step.elapsed()<Duration::from_secs(5),"Claim must leave headroom below the database pool wait budget");
}
