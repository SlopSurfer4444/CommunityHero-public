//! Drain suspension of provably unstarted durable queue rows. Reducers only:
//! callers hold the existing full workspace writer lock. Raw rows are never
//! changed and no receipt here authorizes dispatch, replay, or paid recovery.
use crate::{conflict, ApiResult};
use crate::runtime_lifecycle::{self, AdmissionClass, OwnerToken};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;

fn err() -> crate::ApiError {
    conflict("Runtime queued backlog binding changed; claim or transfer blocked")
}
fn exact(value: &Value, keys: &[&str]) -> bool {
    value.as_object().is_some_and(|object|
        object.len() == keys.len() && keys.iter().all(|key| object.contains_key(*key)))
}
fn token_value(owner: &OwnerToken) -> Value {
    json!({"account":owner.account,"runtimeId":owner.runtime_id,
        "releaseSha256":owner.release_sha256,"epoch":owner.epoch})
}
fn row_hash(row: &Value) -> ApiResult<String> {
    Ok(format!("{:x}", Sha256::digest(serde_json::to_vec(row).map_err(|_| err())?)))
}
fn hash(value: &Value) -> bool {
    value.as_str().is_some_and(|s| s.len() == 64
        && s.bytes().all(|b| b.is_ascii_digit() || matches!(b, b'a'..=b'f')))
}
fn jobs(workspace: &Value) -> ApiResult<&Vec<Value>> {
    let rows = workspace["jobs"].as_array().ok_or_else(err)?;
    let mut ids = BTreeSet::new();
    for row in rows {
        let id = row["id"].as_str().filter(|id| !id.is_empty()).ok_or_else(err)?;
        if !row.is_object() || !ids.insert(id) { return Err(err()); }
    }
    Ok(rows)
}

/// Finite constructor schemas, not an absence-of-result inference. Unknown
/// fields, legacy missing attempts, prior acquisition/retry authorities and
/// all stage admissions deliberately stay outside the retained queue.
fn unstarted(row: &Value, account: &str) -> bool {
    if row["status"] != "queued" || !row["finishedAt"].is_null()
        || !row["startedAt"].is_null() || !row["result"].is_null()
        || row.get("account").is_some_and(|v| v.as_str() != Some(account)) {
        return false;
    }
    let Some(fields) = row.as_object() else { return false; };
    let common = ["id", "kind", "purpose", "account", "connectorBinding", "refId",
        "status", "createdAt", "finishedAt", "startedAt", "result"];
    let specific: &[&str] = match row["kind"].as_str() {
        Some("media") if row["purpose"] == "auto_media" && row["visualContractVersion"] == 2
            && row["sourceAttempts"].as_array().is_some_and(Vec::is_empty) =>
            &["visualContractVersion", "sourceAttempts", "groupKey", "fallbackAllowed",
                "manualRequested", "acquisitionProfile"],
        Some("assistant") if matches!(row["purpose"].as_str(),
            Some("auto_prepare" | "auto_revalidate" | "engine_prepare")) =>
            &["prepareBundle", "requestedItemIds", "selectedItemIds", "held", "preparationStages"],
        Some("research") if matches!(row["purpose"].as_str(), Some("research" | "source_research")) =>
            &["request", "sourceUrls", "preparationStages"],
        _ => return false,
    };
    if fields.keys().any(|key|
        !common.contains(&key.as_str()) && !specific.contains(&key.as_str())) {
        return false;
    }
    if let Some(stages) = row.get("preparationStages") {
        // Explicit null first/review shells are harmless. Every admission,
        // reservation, checkpoint, attempt or unknown stage is a hold.
        if !stages.is_null() && !stages.as_object().is_some_and(|fields|
            fields.iter().all(|(key, value)| matches!(key.as_str(), "first" | "review") && value.is_null())) {
            return false;
        }
    }
    true
}

fn closed_owner(workspace: &Value) -> ApiResult<OwnerToken> {
    let lifecycle = runtime_lifecycle::status(workspace)?;
    if !matches!(lifecycle["phase"].as_str(), Some("draining" | "drained" | "stopped")) {
        return Err(err());
    }
    runtime_lifecycle::parse_token(&lifecycle["owner"])
}

/// Called exactly at begin-drain after phase/epoch change, inside that SAME
/// transaction. It captures only eligible rows already present at that fence.
pub(crate) fn capture(workspace: &Value, owner: &OwnerToken) -> ApiResult<Value> {
    if runtime_lifecycle::status(workspace)?["phase"] != "draining"
        || closed_owner(workspace)? != *owner
        || !workspace["runtimeLifecycle"]["queuedBacklog"].is_null() {
        return Err(err());
    }
    let mut retained = Vec::new();
    for row in jobs(workspace)? {
        if unstarted(row, &owner.account) {
            retained.push(json!({"jobId":row["id"],"rowSha256":row_hash(row)?}));
        }
    }
    Ok(json!({"version":1,"owner":token_value(owner),"jobs":retained}))
}

/// Verify the entire retained inventory and every complete raw row. Call at
/// mark-drained, stop, resume and successor handoff before clearing suspension.
/// Native settlement remains a SEPARATE mandatory witness at those reducers.
pub(crate) fn validate(workspace: &Value) -> ApiResult<()> {
    let lifecycle = runtime_lifecycle::status(workspace)?;
    let overlay = &lifecycle["queuedBacklog"];
    if overlay.is_null() { return Ok(()); }
    let owner = closed_owner(workspace)?;
    if !exact(overlay, &["version", "owner", "jobs"]) || overlay["version"] != 1
        || runtime_lifecycle::parse_token(&overlay["owner"])? != owner {
        return Err(err());
    }
    let entries = overlay["jobs"].as_array().ok_or_else(err)?;
    let rows = jobs(workspace)?;
    let mut seen = BTreeSet::new();
    for entry in entries {
        if !exact(entry, &["jobId", "rowSha256"]) || !hash(&entry["rowSha256"]) {
            return Err(err());
        }
        let id = entry["jobId"].as_str().filter(|id| !id.is_empty()).ok_or_else(err)?;
        if !seen.insert(id) { return Err(err()); }
        let row = rows.iter().find(|row| row["id"] == id).ok_or_else(err)?;
        if !unstarted(row, &owner.account) || entry["rowSha256"] != row_hash(row)? {
            return Err(err());
        }
    }
    Ok(())
}

/// This is ONLY an exemption from the durable queued-work count. The caller
/// must first prove SettledNative under the same owner/epoch; it never exempts
/// running, checkpointed, unknown or otherwise ambiguous work.
pub(crate) fn retained_queued(workspace: &Value, row: &Value) -> ApiResult<bool> {
    validate(workspace)?;
    if row["status"] != "queued" { return Ok(false); }
    let overlay = &workspace["runtimeLifecycle"]["queuedBacklog"];
    if overlay.is_null() { return Ok(false); }
    let id = row["id"].as_str().ok_or_else(err)?;
    let digest = row_hash(row)?;
    Ok(overlay["jobs"].as_array().ok_or_else(err)?.iter()
        .any(|entry| entry["jobId"] == id && entry["rowSha256"] == digest))
}

/// For a NEW reservation of an existing queue row, inside its writer lock.
/// Do not call this from paid completion/checkpoint or UNKNOWN settlement.
pub(crate) fn require_job_claim(workspace: &Value, job_id: &str, owner: &OwnerToken) -> ApiResult<()> {
    let lifecycle = runtime_lifecycle::status(workspace)?;
    if lifecycle["phase"] != "running" || !lifecycle["queuedBacklog"].is_null()
        || !jobs(workspace)?.iter().any(|row| row["id"] == job_id) {
        return Err(err());
    }
    runtime_lifecycle::require_admission(workspace, owner, AdmissionClass::Preparation)
}

/// Startup recovery must not rewrite a suspended inventory or mark its queue
/// interrupted while draining/stopped. Missing lifecycle state is an error,
/// not an implicit resume. Startup coordinator separately admits bootstrap.
pub(crate) fn recovery_allowed(workspace: &Value) -> ApiResult<bool> {
    let lifecycle = runtime_lifecycle::status(workspace)?;
    Ok(lifecycle["phase"] == "running" && lifecycle["queuedBacklog"].is_null())
}

/// The admitted transition archives suspension before clearing the live
/// overlay. Preserve ONLY unchanged unstarted rows from that latest transition
/// during startup recovery; a later claim changes its hash and loses exemption.
/// This skips a destructive recovery rewrite, never grants a new reservation.
pub(crate) fn preserved_at_recovery(workspace: &Value, row: &Value) -> ApiResult<bool> {
    let lifecycle = runtime_lifecycle::status(workspace)?;
    if lifecycle["phase"] != "running" || !lifecycle["queuedBacklog"].is_null() {
        return Ok(false);
    }
    let current = runtime_lifecycle::parse_token(&lifecycle["owner"])?;
    if !unstarted(row, &current.account) { return Ok(false); }
    let Some(event) = lifecycle["history"].as_array().ok_or_else(err)?.last() else { return Ok(false); };
    let archive = match event["kind"].as_str() {
        Some("successor-accepted") => &event["evidence"]["transfer"]["queuedBacklog"],
        Some("maintenance-resumed") => &event["evidence"]["queuedBacklog"],
        _ => return Ok(false),
    };
    if runtime_lifecycle::parse_token(&event["owner"])? != current { return Err(err()); }
    if archive.is_null() { return Ok(false); }
    if !exact(archive, &["version", "owner", "jobs"]) || archive["version"] != 1 { return Err(err()); }
    let prior = runtime_lifecycle::parse_token(&archive["owner"])?;
    if prior.account != current.account || prior.epoch.checked_add(1) != Some(current.epoch) { return Err(err()); }
    if event["kind"] == "successor-accepted" {
        let transfer = &event["evidence"]["transfer"];
        if !exact(transfer, &["ledgerSha256", "owner", "target", "nativeSettled", "queuedBacklog"])
            || !hash(&transfer["ledgerSha256"]) || transfer["nativeSettled"] != true
            || runtime_lifecycle::parse_token(&transfer["owner"])? != prior
            || transfer["target"]["releaseSha256"] != current.release_sha256
            || prior.runtime_id == current.runtime_id { return Err(err()); }
    } else if prior.runtime_id != current.runtime_id || prior.release_sha256 != current.release_sha256
        || event["evidence"]["previousEpoch"] != prior.epoch { return Err(err()); }
    let entries = archive["jobs"].as_array().ok_or_else(err)?;
    let mut ids = BTreeSet::new();
    for entry in entries {
        if !exact(entry, &["jobId", "rowSha256"]) || !hash(&entry["rowSha256"]) { return Err(err()); }
        let id = entry["jobId"].as_str().filter(|id| !id.is_empty()).ok_or_else(err)?;
        if !ids.insert(id) { return Err(err()); }
    }
    let id = row["id"].as_str().ok_or_else(err)?;
    if !jobs(workspace)?.iter().any(|current_row| current_row == row) { return Err(err()); }
    let digest = row_hash(row)?;
    Ok(entries.iter().any(|entry| entry["jobId"] == id && entry["rowSha256"] == digest))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> (Value, OwnerToken) {
        let owner = OwnerToken{account:"LikeAvto".into(),runtime_id:"runtime-one".into(),
            release_sha256:"a".repeat(64),epoch:2};
        let workspace = json!({"account":"LikeAvto","connectorBinding":{},"jobs":[],
            "operations":[{"id":"unknown-op","status":"unknown","reply":"do not replay"}],
            "approvals":[],"audit":[],"materials":[],"knowledge_entries":[],"knowledge_versions":[],
            "runtimeLifecycle":{"schemaVersion":1,"owner":token_value(&owner),"phase":"draining",
                "history":[],"mediaAnalysisGeneration":1,"queuedBacklog":null,"transfer":null,
                "target":{"releaseSha256":"b".repeat(64),"attemptId":"drain-one",
                    "asrDisabled":false,"mediaAnalysisGeneration":1}}});
        (workspace, owner)
    }
    fn media(id: &str) -> Value {
        json!({"id":id,"kind":"media","purpose":"auto_media","status":"queued",
            "visualContractVersion":2,"sourceAttempts":[],"account":"LikeAvto",
            "refId":"post-one","createdAt":"now","finishedAt":null})
    }
    fn suspend(workspace: &mut Value, owner: &OwnerToken) {
        workspace["runtimeLifecycle"]["queuedBacklog"] = capture(workspace, owner).unwrap();
    }
    #[test] fn preserves_full_raw_queue_and_unknown_history() {
        let (mut workspace, owner) = fixture();
        workspace["jobs"] = json!([media("media-one"),
            {"id":"prep-one","kind":"assistant","purpose":"engine_prepare","status":"queued",
                "preparationStages":{"first":null,"review":null}},
            {"id":"research-one","kind":"research","purpose":"source_research","status":"queued"}]);
        let original = runtime_lifecycle::ledger_digest(&workspace).unwrap();
        let raw = workspace["jobs"].clone();let ops = workspace["operations"].clone();
        suspend(&mut workspace, &owner);validate(&workspace).unwrap();
        assert_eq!(workspace["jobs"], raw);assert_eq!(workspace["operations"], ops);
        assert_eq!(runtime_lifecycle::ledger_digest(&workspace).unwrap(), original);
        for row in workspace["jobs"].as_array().unwrap() {
            assert!(retained_queued(&workspace, row).unwrap());
            assert!(require_job_claim(&workspace, row["id"].as_str().unwrap(),&owner).is_err());
        }
        assert!(!recovery_allowed(&workspace).unwrap());
    }
    #[test] fn paid_inflight_unknown_and_legacy_missing_attempts_are_never_retained() {
        let (mut workspace, owner) = fixture();
        let mut running = media("running");running["status"] = json!("running");
        let mut checkpoint = media("checkpoint");checkpoint["result"] = json!({"visualProgress":{"leaseId":null,"phase":"scan"}});
        let mut attempted = media("attempted");attempted["sourceAttempts"] = json!([{"status":"unknown"}]);
        let mut legacy = media("legacy");legacy.as_object_mut().unwrap().remove("sourceAttempts");
        workspace["jobs"] = json!([running, checkpoint, attempted, legacy,
            {"id":"paid-prep","kind":"assistant","purpose":"engine_prepare","status":"queued",
                "preparationStages":{"first":null,"firstAdmission":{"status":"reserved"}}},
            {"id":"unknown-stage","kind":"assistant","purpose":"auto_prepare","status":"queued",
                "preparationStages":{"first":null,"futurePaidStage":null}}]);
        suspend(&mut workspace, &owner);
        assert!(workspace["runtimeLifecycle"]["queuedBacklog"]["jobs"].as_array().unwrap().is_empty());
        for row in workspace["jobs"].as_array().unwrap() {assert!(!retained_queued(&workspace,row).unwrap());}
    }
    #[test] fn whole_row_drift_duplicate_id_and_forged_owner_block() {
        let (mut workspace, owner) = fixture();workspace["jobs"] = json!([media("one")]);
        suspend(&mut workspace, &owner);
        let original = workspace.clone();workspace["jobs"][0]["refId"] = json!("changed");assert!(validate(&workspace).is_err());
        workspace = original.clone();workspace["jobs"].as_array_mut().unwrap().push(media("one"));assert!(validate(&workspace).is_err());
        workspace = original;workspace["runtimeLifecycle"]["queuedBacklog"]["owner"]["epoch"] = json!(1);assert!(validate(&workspace).is_err());
    }
    #[test] fn normal_admission_returns_only_after_explicit_owner_resume() {
        let (mut workspace, owner) = fixture();workspace["jobs"] = json!([media("one")]);suspend(&mut workspace,&owner);
        assert!(require_job_claim(&workspace,"one",&owner).is_err());validate(&workspace).unwrap();
        // Mirrors ROOT-owned resume only AFTER SettledNative validation.
        workspace["runtimeLifecycle"]["queuedBacklog"] = Value::Null;
        workspace["runtimeLifecycle"]["owner"]["epoch"] = json!(3);
        workspace["runtimeLifecycle"]["phase"] = json!("running");
        workspace["runtimeLifecycle"]["target"] = Value::Null;
        let mut resumed = owner.clone();resumed.epoch = 3;
        require_job_claim(&workspace,"one",&resumed).unwrap();assert!(recovery_allowed(&workspace).unwrap());
        let mut stale = owner;stale.epoch = 2;
        assert!(runtime_lifecycle::require_admission(&workspace,&stale,AdmissionClass::Media).is_err());
    }
    fn native(owner: &OwnerToken) -> runtime_lifecycle::SettledNative {
        runtime_lifecycle::SettledNative{owner:owner.clone(),application_tasks:0,provider_queued:0,
            provider_dispatched:0,provider_contained:true,credential_writers:0,unresolved_effects:0}
    }
    fn running_fixture() -> (Value, OwnerToken) {
        let (mut workspace, owner) = fixture();
        workspace["runtimeLifecycle"]["phase"] = json!("running");
        workspace["runtimeLifecycle"]["target"] = Value::Null;
        (workspace, owner)
    }
    #[test] fn complete_transfer_keeps_original_queue_through_actual_recovery_selector() {
        let (mut workspace, owner) = running_fixture();workspace["jobs"] = json!([media("one")]);
        let raw = workspace["jobs"].clone();let unknown = workspace["operations"].clone();
        let drain = runtime_lifecycle::begin_drain(&mut workspace,&owner,&"b".repeat(64),"handoff",false).unwrap();
        let mut busy = native(&drain);busy.application_tasks = 1;
        assert!(runtime_lifecycle::mark_drained(&mut workspace,&drain,&busy).is_err());
        let transfer = runtime_lifecycle::mark_drained(&mut workspace,&drain,&native(&drain)).unwrap();
        runtime_lifecycle::commit_stop_checkpoint(&mut workspace,&drain,&transfer).unwrap();
        let next = runtime_lifecycle::accept_successor(&mut workspace,&drain,&transfer,"runtime-two",
            &"b".repeat(64),&"c".repeat(64),false).unwrap();
        assert!(recovery_allowed(&workspace).unwrap());
        assert!(preserved_at_recovery(&workspace,&workspace["jobs"][0]).unwrap());
        assert_eq!(workspace["jobs"],raw);assert_eq!(workspace["operations"],unknown);
        require_job_claim(&workspace,"one",&next).unwrap();
        workspace["jobs"][0]["status"] = json!("running");
        assert!(!preserved_at_recovery(&workspace,&workspace["jobs"][0]).unwrap());
    }
    #[test] fn actual_same_owner_resume_archives_queue_but_never_paid_uncertainty() {
        let (mut workspace, owner) = running_fixture();workspace["jobs"] = json!([media("one")]);
        let drain = runtime_lifecycle::begin_drain(&mut workspace,&owner,&"b".repeat(64),"resume",false).unwrap();
        runtime_lifecycle::resume_same_owner(&mut workspace,&drain,&native(&drain)).unwrap();
        assert!(preserved_at_recovery(&workspace,&workspace["jobs"][0]).unwrap());
        workspace["jobs"][0]["sourceAttempts"] = json!([{"status":"unknown"}]);
        assert!(!preserved_at_recovery(&workspace,&workspace["jobs"][0]).unwrap());
        let (mut workspace, owner) = running_fixture();
        let mut paid = media("paid");paid["result"] = json!({"visualProgress":{"phase":"scan","leaseId":null}});
        workspace["jobs"] = json!([paid]);
        let drain = runtime_lifecycle::begin_drain(&mut workspace,&owner,&"b".repeat(64),"paid-hold",false).unwrap();
        assert!(runtime_lifecycle::mark_drained(&mut workspace,&drain,&native(&drain)).is_err());
    }
}
