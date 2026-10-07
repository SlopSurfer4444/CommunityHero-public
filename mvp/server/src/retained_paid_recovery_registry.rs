//! Server-owned, immutable retained capture registry. Paths and pins are read
//! once at startup; HTTP requests can select item IDs but never capture bytes,
//! paths, hashes, or authority.
use crate::{ApiResult, Value, conflict};
use crate::retained_paid_recovery::InstalledCapture;
use serde_json::json;
use std::{cell::RefCell, fs, path::PathBuf, sync::{Arc, OnceLock}};

static CAPTURE: OnceLock<Option<Arc<InstalledCapture>>> = OnceLock::new();
#[cfg(test)] thread_local! { static TEST_CAPTURE: RefCell<Option<InstalledCapture>> = const { RefCell::new(None) }; }

fn read_pin(name: &str) -> Result<String, String> {
    std::env::var(name).map_err(|_| format!("Missing {name}"))
}
fn read_file(path: &str) -> Result<Vec<u8>, String> {
    let path = PathBuf::from(path);
    if !path.is_absolute() { return Err("Retained capture paths must be absolute".into()); }
    let canonical = path.canonicalize().map_err(|_| "Retained capture file unavailable")?;
    let meta = fs::metadata(&canonical).map_err(|_| "Retained capture metadata unavailable")?;
    if !meta.is_file() || meta.len() > 8 * 1024 * 1024 { return Err("Retained capture file invalid or too large".into()); }
    fs::read(canonical).map_err(|_| "Retained capture file unavailable".into())
}
pub(crate) fn install_from_environment() -> Result<Option<Arc<InstalledCapture>>, String> {
    let names = ["COMMUNITYHERO_RETAINED_CAPTURE_PATH", "COMMUNITYHERO_RETAINED_CAPTURE_SHA256",
        "COMMUNITYHERO_RETAINED_RESPONSE_PATH", "COMMUNITYHERO_RETAINED_RESPONSE_SHA256"];
    let present = names.iter().filter(|n| std::env::var_os(n).is_some()).count();
    if present == 0 { return Ok(None); }
    if present != names.len() { return Err("Incomplete retained capture startup configuration".into()); }
    let packet_path = read_pin(names[0])?; let packet_hash = read_pin(names[1])?;
    let response_path = read_pin(names[2])?; let response_hash = read_pin(names[3])?;
    if response_hash.len() != 64 || !response_hash.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err("Invalid retained response startup pin".into());
    }
    let packet = read_file(&packet_path)?; let response = read_file(&response_path)?;
    use sha2::{Digest, Sha256};
    if format!("{:x}", Sha256::digest(&response)) != response_hash.to_ascii_lowercase() {
        return Err("Retained raw response startup pin mismatch".into());
    }
    let installed = InstalledCapture::from_private_bytes(&packet, &packet_hash.to_ascii_lowercase(), &response)
        .map_err(|_| "Retained capture startup validation failed".to_string())?;
    Ok(Some(Arc::new(installed)))
}
pub(crate) fn install(capture: Option<Arc<InstalledCapture>>) -> Result<(), String> {
    CAPTURE.set(capture).map_err(|_| "Retained capture registry already initialized".into())
}
pub(crate) fn capture() -> ApiResult<InstalledCapture> {
    #[cfg(test)] if let Some(value) = TEST_CAPTURE.with(|c| c.borrow().clone()) { return Ok(value); }
    CAPTURE.get().and_then(|v| v.as_ref()).map(|v| (**v).clone())
        .ok_or_else(|| conflict("No server-installed retained capture is configured"))
}
pub(crate) fn capture_if_configured() -> Option<InstalledCapture> {
    #[cfg(test)] if let Some(value) = TEST_CAPTURE.with(|c| c.borrow().clone()) { return Some(value); }
    CAPTURE.get().and_then(|v| v.as_ref()).map(|v| (**v).clone())
}
#[cfg(test)]
pub(crate) fn with_test_capture<T>(installed: InstalledCapture, f: impl FnOnce() -> T) -> T {
    struct Reset(Option<InstalledCapture>);
    impl Drop for Reset { fn drop(&mut self) { TEST_CAPTURE.with(|c| *c.borrow_mut() = self.0.take()); } }
    let previous = TEST_CAPTURE.with(|c| c.replace(Some(installed)));
    let _reset = Reset(previous); f()
}
#[cfg(test)]
pub(crate) struct TestCaptureGuard {
    previous: Option<InstalledCapture>,
    _not_send: std::marker::PhantomData<std::rc::Rc<()>>,
}
#[cfg(test)]
impl Drop for TestCaptureGuard {
    fn drop(&mut self) { TEST_CAPTURE.with(|c| *c.borrow_mut() = self.previous.take()); }
}
#[cfg(test)]
pub(crate) fn install_test_capture(installed: InstalledCapture) -> TestCaptureGuard {
    let previous = TEST_CAPTURE.with(|c| c.replace(Some(installed)));
    TestCaptureGuard { previous, _not_send: std::marker::PhantomData }
}
pub(crate) fn validate_proposal(d: &Value, p: &Value, own: Option<&Value>) -> ApiResult<Option<String>> {
    if p.get(crate::retained_paid_recovery::FIELD).is_none() { return Ok(None); }
    let installed = capture()?;
    crate::retained_paid_recovery::validate_proposal(d, p, &installed, own).map(Some)
}
pub(crate) fn validate_change(before: &Value, after: &Value, allow_delta: bool) -> ApiResult<()> {
    // Startup's fixed capture pin also protects the original failed job in
    // projections that contain the job row but omit the associated proposal.
    // Compare only when a writer exposes that row; bounded settlement can
    // omit it entirely and still preserve the proposal proof byte-for-byte.
    if let Some(installed) = capture_if_configured() {
        let id = installed.job_id()?;
        let old_job = before["jobs"].as_array().and_then(|jobs| jobs.iter().find(|j| j["id"] == id));
        let new_job = after["jobs"].as_array().and_then(|jobs| jobs.iter().find(|j| j["id"] == id));
        if old_job != new_job { return Err(conflict("Retained original failed job/reservation is immutable")); }
    }
    let old_proposals = before["proposals"].as_array();
    let new_proposals = after["proposals"].as_array();
    let old_audit = before["audit"].as_array();
    let new_audit = after["audit"].as_array();
    let old_proofs: Vec<_> = old_proposals.into_iter().flatten().filter(|p| p.get(crate::retained_paid_recovery::FIELD).is_some()).collect();
    let new_proofs: Vec<_> = new_proposals.into_iter().flatten().filter(|p| p.get(crate::retained_paid_recovery::FIELD).is_some()).collect();
    let old_receipts: Vec<_> = old_audit.into_iter().flatten().filter(|a| a["action"] == "retained_paid_recovery.committed").collect();
    let new_receipts: Vec<_> = new_audit.into_iter().flatten().filter(|a| a["action"] == "retained_paid_recovery.committed").collect();
    if old_proofs.is_empty() && new_proofs.is_empty() && old_receipts.is_empty() && new_receipts.is_empty() { return Ok(()); }

    // Bounded operation settlement projections may carry the exact proposal
    // proof while omitting jobs/audit. Existing evidence stays byte-identical
    // there; it does not need revalidation against current context after an
    // external attempt. Full commits below run the native validator instead.
    let mut added = vec![];
    for current in &new_proofs {
        let id = current["id"].as_str().ok_or_else(|| conflict("Recovery proposal identity missing"))?;
        match old_proposals.into_iter().flatten().find(|old| old["id"] == id) {
            Some(old) if old.get(crate::retained_paid_recovery::FIELD) == current.get(crate::retained_paid_recovery::FIELD) => {},
            Some(_) => return Err(conflict("Recovery proof cannot be grafted onto or changed on an existing proposal")),
            None => added.push(*current),
        }
    }
    for prior in &old_proofs {
        let id = prior["id"].as_str().ok_or_else(|| conflict("Recovery proposal identity missing"))?;
        let current = new_proposals.into_iter().flatten().find(|p| p["id"] == id)
            .ok_or_else(|| conflict("A scoped write cannot omit an existing retained proposal proof"))?;
        if prior.get(crate::retained_paid_recovery::FIELD) != current.get(crate::retained_paid_recovery::FIELD) {
            return Err(conflict("Retained proposal proof is immutable"));
        }
    }
    for current in &new_receipts {
        let id = current["id"].as_str().ok_or_else(|| conflict("Recovery receipt identity missing"))?;
        match old_audit.into_iter().flatten().find(|old| old["id"] == id) {
            Some(old) if old == *current => {},
            Some(_) => return Err(conflict("Retained commit receipt is immutable")),
            None => {},
        }
    }
    for prior in &old_receipts {
        let id = prior["id"].as_str().ok_or_else(|| conflict("Recovery receipt identity missing"))?;
        let current = new_audit.into_iter().flatten().find(|a| a["id"] == id)
            .ok_or_else(|| conflict("A scoped write cannot omit an existing retained commit receipt"))?;
        if prior != &current { return Err(conflict("Retained commit receipt is immutable")); }
    }
    let added_receipts: Vec<_> = new_receipts.iter().filter(|a| !old_receipts.iter().any(|old| old["id"] == a["id"])).collect();

    // Existing original-job rows cannot be removed, inserted, or edited by any
    // writer whose projection includes them. Settlement projections may omit
    // the job entirely while preserving the immutable proof on both sides.
    let job_ids: std::collections::BTreeSet<String> = old_proofs.iter().chain(new_proofs.iter())
        .filter_map(|p| p[crate::retained_paid_recovery::FIELD]["originalJobId"].as_str().map(str::to_owned)).collect();
    for job_id in job_ids {
        let old_job = before["jobs"].as_array().and_then(|jobs| jobs.iter().find(|j| j["id"] == job_id));
        let new_job = after["jobs"].as_array().and_then(|jobs| jobs.iter().find(|j| j["id"] == job_id));
        if old_job != new_job { return Err(conflict("Retained original failed job/reservation is immutable")); }
    }
    if added.is_empty() && added_receipts.is_empty() { return Ok(()); }
    if !allow_delta { return Err(conflict("Only the authenticated retained-recovery reducer may introduce a proof and receipt")); }
    if added.is_empty() || added_receipts.len() != 1 { return Err(conflict("Recovery proof and commit receipt must be one authenticated atomic delta")); }
    let installed = capture()?;
    crate::retained_paid_recovery::validate_change(before, after, &installed)?;
    let receipt = added_receipts[0];
    let expected_items = json!(added.iter().map(|p| p["itemId"].clone()).collect::<Vec<_>>());
    let expected_refs = json!(added.iter().map(|p| json!({"id":p["id"],"revision":p["revision"]})).collect::<Vec<_>>());
    if receipt["captureSha256"] != installed.sha256() || receipt["itemIds"] != expected_items
        || receipt["proposals"] != expected_refs {
        return Err(conflict("Recovery receipt does not bind the installed capture and complete proposal set"));
    }
    for p in added {
        let proof = &p[crate::retained_paid_recovery::FIELD];
        if proof["captureSha256"] != installed.sha256() || proof["originalJobId"] != installed.job_id()?
            || proof["requestId"] != receipt["requestId"] || proof["requestDigest"] != receipt["requestDigest"]
            || !receipt["itemIds"].as_array().is_some_and(|ids| ids.contains(&p["itemId"])) {
            return Err(conflict("Recovery proposal proof is not covered by its durable native commit receipt"));
        }
    }
    Ok(())
}
