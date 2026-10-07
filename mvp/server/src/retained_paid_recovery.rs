//! Recover exact retained suggestions as NEW operator drafts. Never a model
//! completion, reservation release, editorial verdict, approval, or retry.
//! Integration must supply an immutable server-installed capture, a complete
//! writer snapshot, and the ordinary proposal/approval/execution guards.
use crate::*;
use crate::operator_auth::Actor;
use sha2::{Digest, Sha256};
use std::{collections::BTreeSet, sync::Arc};

pub(crate) const CONTRACT: &str = "communityhero-retained-paid-draft-v1";
pub(crate) const FIELD: &str = "retainedPaidRecovery";
const CAPTURE_CONTRACT: &str = "quarantined-retained-first-v1";
const CHECKS: &[&str] = &["recipientIntent", "companyRules", "branchHistory", "factualSupport", "mediaDependency", "exactText"];
fn hash(bytes: &[u8]) -> String { format!("{:x}", Sha256::digest(bytes)) }
fn digest(v: &Value) -> String { hash(v.to_string().as_bytes()) }
fn text<'a>(v: &'a Value, k: &str) -> ApiResult<&'a str> {
    v[k].as_str().filter(|s| !s.trim().is_empty()).ok_or_else(|| bad(&format!("Missing recovery {k}")))
}
fn rows<'a>(v: &'a Value, k: &str) -> ApiResult<&'a Vec<Value>> {
    v[k].as_array().ok_or_else(|| conflict(&format!("Missing recovery ledger {k}")))
}
fn exact(v: &Value, keys: &[&str]) -> ApiResult<()> {
    if v.as_object().is_none_or(|o| o.len() != keys.len() || keys.iter().any(|k| !o.contains_key(*k))) {
        return Err(bad("Unexpected recovery request fields"));
    }
    Ok(())
}
fn authority(actor: &Actor) -> ApiResult<Value> {
    if actor.id != "local-owner" || actor.role != "owner" {
        return Err(ApiError(StatusCode::FORBIDDEN, "Retained recovery requires authenticated local owner".into()));
    }
    let binding = dispatch_authority::approval_binding(actor);
    dispatch_authority::admit(&json!({"approvedBy": actor.public_json(), "approvalAuthority": binding}), actor)?;
    Ok(binding)
}

/// NEVER Deserialize this type or accept its expected hash from HTTP. Registry
/// installation verifies separately pinned bytes; original raw bytes stay owned
/// here and in the immutable private installed file, not in public summaries.
#[derive(Clone)]
pub(crate) struct InstalledCapture {
    packet: Arc<Value>, packet_bytes: Arc<[u8]>, raw_response_bytes: Arc<[u8]>, sha256: String,
}
impl InstalledCapture {
    pub(crate) fn from_private_bytes(bytes: &[u8], installed_sha256: &str, raw_response_bytes: &[u8]) -> ApiResult<Self> {
        if bytes.len() > 8 * 1024 * 1024 || raw_response_bytes.len() > 8 * 1024 * 1024
            || installed_sha256.len() != 64 || hash(bytes) != installed_sha256 {
            return Err(conflict("Installed retained capture bytes changed"));
        }
        let packet: Value = serde_json::from_slice(bytes).map_err(|_| bad("Invalid retained capture JSON"))?;
        if packet["contract"] != CAPTURE_CONTRACT || packet["admitted"] != false
            || packet["historicalRuntimeSourceVerified"] != false || packet["historicalInstructionSourceVerified"] != false
            || packet["plan"]["admitted"] != false || packet["plan"]["nativeReceipt"] != false
            || packet["evidence"]["responseSha256"] != hash(raw_response_bytes)
            || packet["evidence"]["localTurnCompleted"] != true {
            return Err(conflict("Retained capture cannot assert historical admission or change raw output"));
        }
        let installed = Self { packet: Arc::new(packet), packet_bytes: Arc::from(bytes), raw_response_bytes: Arc::from(raw_response_bytes), sha256: installed_sha256.into() };
        installed.membership()?;
        Ok(installed)
    }
    pub(crate) fn sha256(&self) -> &str { &self.sha256 }
    pub(crate) fn job_id(&self) -> ApiResult<&str> { text(&self.packet["originalJob"], "id") }
    fn candidate(&self, item: &str) -> ApiResult<&Value> {
        let found: Vec<_> = rows(&self.packet["plan"], "candidates")?.iter().filter(|c| c["proposal"]["itemId"] == item).collect();
        if found.len() != 1 { return Err(conflict("Recipient is held or has no exact retained suggestion")); }
        Ok(found[0])
    }
    fn membership(&self) -> ApiResult<()> {
        let job = &self.packet["originalJob"]; let plan = &self.packet["plan"];
        let ids = rows(&job["prepareBundle"], "itemIds")?;
        if ids.is_empty() || ids.len() > 100 || plan["originalJobId"] != job["id"]
            || plan["bundleId"] != job["prepareBundle"]["id"] || plan["bundleDigest"] != job["prepareBundle"]["digest"] {
            return Err(conflict("Retained original membership or bundle changed"));
        }
        let mut selected = BTreeSet::new();
        for id in ids { if !selected.insert(id.as_str().ok_or_else(|| bad("Invalid original recipient"))?) { return Err(conflict("Duplicate original recipient")); } }
        let mut seen = BTreeSet::new();
        for candidate in rows(plan, "candidates")? {
            let p = &candidate["proposal"]; let item = text(p, "itemId")?;
            if !selected.contains(item) || !seen.insert(item) || candidate["admitted"] != false || candidate["nativeReceipt"] != false {
                return Err(conflict("Retained candidate membership or historical status changed"));
            }
            action_text(p)?;
        }
        for held in rows(plan, "held")? {
            let item = text(held, "itemId")?;
            if !selected.contains(item) || !seen.insert(item) { return Err(conflict("Retained hold membership changed")); }
        }
        if selected != seen { return Err(conflict("Retained 51-member partition is incomplete")); }
        Ok(())
    }
}
fn action_text(p: &Value) -> ApiResult<()> {
    let kind = text(p, "kind")?; let body = p["text"].as_str().ok_or_else(|| bad("Retained text missing"))?;
    if !["reply_and_close", "close", "hide", "delete"].contains(&kind) || body.len() > 20000
        || (kind == "reply_and_close" && body.trim().is_empty()) || (kind != "reply_and_close" && !body.is_empty()) {
        return Err(bad("Invalid retained action and exact text"));
    }
    Ok(())
}
fn owner<'a>(d: &'a Value, installed: &InstalledCapture) -> ApiResult<&'a Value> {
    for k in ["jobs", "items", "proposals", "approvals", "operations", "posts", "branches", "materials", "knowledge_entries", "knowledge_versions", "preparationResearch", "audit"] { rows(d, k)?; }
    if d.get("scopeOwners").is_some() || d.get("scopeProposals").is_some() { return Err(conflict("Retained recovery requires a full writer snapshot")); }
    let job = row(d, "jobs", installed.job_id()?)?;
    if *job != installed.packet["originalJob"] || job["kind"] != "assistant" || job["purpose"] != "engine_prepare"
        || job["status"] != "failed" || job["error"] != "Adapter failed (ASSISTANT_INVALID_RESPONSE)"
        || !job["preparationStages"]["first"].is_null() || !job["preparationStages"]["review"].is_null()
        || !job["result"].is_null() || !job["recovery"].is_null() || !job["scopeModelAttempt"].is_null()
        || !job["scopeFailure"].is_null() || !job["scopeReservation"].is_object() {
        return Err(conflict("Original failed paid job changed or remains uncertain"));
    }
    // Mandatory native capture. Never replace this with JS digests or
    // trust flags to work around a numeric-serialization discrepancy.
    let binding = active_binding(d)?;
    if preparation_reservations::capture(d, installed.job_id()?)? != job["scopeReservation"]
        || job["scopeReservation"]["connectorBinding"] != binding.to_json()
        || job["prepareBundle"]["request"]["account"] != d["account"] {
        return Err(conflict("Original native reservation/company/route changed"));
    }
    Ok(job)
}
fn group<'a>(job: &'a Value, item: &str) -> ApiResult<&'a Value> {
    let found: Vec<_> = rows(&job["preparationStages"], "groupAdmission")?.iter().filter(|g| g["itemIds"].as_array().is_some_and(|ids| ids.contains(&json!(item)))).collect();
    if found.len() != 1 { return Err(conflict("Original recipient group missing or duplicated")); }
    Ok(found[0])
}
fn original_group_current(d: &Value, installed: &InstalledCapture, item: &str) -> ApiResult<()> {
    let job = owner(d, installed)?;
    prepare_bundle::current_group(d, &job["prepareBundle"], group(job, item)?).map_err(conflict)
}
fn quiescent(d: &Value) -> ApiResult<()> {
    if rows(d, "jobs")?.iter().any(|j| matches!(j["status"].as_str(), Some("queued" | "running" | "pending"))
        && matches!(j["kind"].as_str(), Some("assistant" | "editorial_review" | "execute" | "reconcile"))) {
        return Err(conflict("Active model/editorial/execution/readback workers must settle before retained draft creation"));
    }
    Ok(())
}
fn creation_guard(d: &Value, installed: &InstalledCapture, item_id: &str) -> ApiResult<Value> {
    let source = installed.candidate(item_id)?;
    // Admission is once per immutable capture/member, independent of proposal
    // lifecycle. Cancellation/staleness is not authority to recreate paid
    // output under a different request. Same-request receipt replay is handled
    // before this guard in commit and never recreates a draft.
    if rows(d, "proposals")?.iter().any(|p| p[FIELD]["captureSha256"] == installed.sha256()
        && (p["itemId"] == item_id || p[FIELD]["proposalBinding"]["itemId"] == item_id)) {
        return Err(conflict("Retained original member already has a recovery admission"));
    }
    for receipt in rows(d, "audit")?.iter().filter(|a| a["action"] == "retained_paid_recovery.committed"
        && a["captureSha256"] == installed.sha256()) {
        if let Some(ids) = receipt["itemIds"].as_array() {
            if ids.contains(&json!(item_id)) { return Err(conflict("Retained original member already has a recovery receipt")); }
        } else {
            // Older native receipt shape bound proposal IDs. Resolve them
            // without consulting status; missing mapping fails closed.
            for reference in rows(receipt, "proposals")? {
                let prior = row(d, "proposals", text(reference, "id")?)?;
                if prior["itemId"] == item_id || prior[FIELD]["proposalBinding"]["itemId"] == item_id {
                    return Err(conflict("Retained original member already has a recovery receipt"));
                }
            }
        }
    }
    original_group_current(d, installed, item_id)?; quiescent(d)?;
    let binding = active_binding(d)?; let item = bound_item(&binding, row(d, "items", item_id)?)?;
    if matches!(item["workflow"].as_str(), Some("closed" | "deleted" | "waiting")) || item["providerStatus"] == "deleted"
        || item["revision"].as_u64().is_none_or(|v| v == 0) {
        return Err(conflict("Retained recipient is closed, waiting, deleted or unversioned"));
    }
    preparation_reservations::assert_available(d, &[item_id.into()], Some(installed.job_id()?))?;
    if rows(d, "proposals")?.iter().any(|p| p["itemId"] == item_id
        && !matches!(p["status"].as_str(), Some("failed" | "stale" | "cancelled" | "superseded"))) {
        return Err(conflict("Recipient has a newer proposal; recovery cannot replace it"));
    }
    if rows(d, "operations")?.iter().any(|op| recipient_operation_blocks(op, &source["proposal"], &item)) {
        return Err(conflict("Recipient already has an admitted operation; no retained retry"));
    }
    let context = prepare_bundle::EvidenceContext::new(d);
    if source["proposal"]["kind"] == "reply_and_close" {
        reply_constraints::validate_reply(&context, &item, text(&source["proposal"], "text")?).map_err(conflict)?;
    }
    Ok(item)
}
fn stable_research(mut selection: Value) -> Value {
    for pin in selection["manifest"].as_array_mut().into_iter().flatten() {
        if let Some(o) = pin.as_object_mut() { o.remove("selectedAt"); }
    }
    selection
}
fn reviewed_evidence(d: &Value, item_id: &str) -> ApiResult<Value> {
    let context = prepare_bundle::EvidenceContext::new(d);
    let mut evidence = context.evidence_for_item(item_id).map_err(conflict)?;
    let cached = research_cache::select(d, rows(&evidence, "items")?, rows(&evidence, "posts")?, &now()).map_err(conflict)?;
    evidence["retainedCurrentResearch"] = stable_research(cached);
    Ok(evidence)
}
fn stable_review_evidence(mut evidence: Value) -> Value {
    // Ordinary creation advances workflow/revision. Those are checked exactly
    // against the native proposal separately, not treated as new source facts.
    for item in evidence["items"].as_array_mut().into_iter().flatten() {
        if let Some(fields) = item.as_object_mut() {
            for key in ["revision", "workflow", "draft"] { fields.remove(key); }
        }
    }
    evidence
}
fn preview(d: &Value, installed: &InstalledCapture, item_id: &str) -> ApiResult<Value> {
    let item = creation_guard(d, installed, item_id)?;
    let context = prepare_bundle::EvidenceContext::new(d);
    // Rebuild eligibility at the current time, but selectedAt alone is not a
    // semantic source change. Hash includes material/hash/expiry/claim scope.
    let evidence = reviewed_evidence(d, item_id)?;
    Ok(json!({"itemId": item_id, "currentItem": item, "candidate": installed.candidate(item_id)?,
        "currentEvidence": evidence, "reviewContextDigest": context.review_fingerprint(item_id).map_err(conflict)?,
        "source": "retained_suggestion_unverified_historical_runtime", "historicalRuntimeSourceVerified": false,
        "historicalInstructionSourceVerified": false, "status": "review_required"}))
}
pub(crate) fn plan(d: &Value, actor: &Actor, installed: &InstalledCapture, item_ids: &[String]) -> ApiResult<Value> {
    let authority = authority(actor)?; owner(d, installed)?;
    if item_ids.is_empty() || item_ids.len() > 100 || item_ids.iter().collect::<BTreeSet<_>>().len() != item_ids.len() {
        return Err(bad("Choose unique retained recipients"));
    }
    let mut candidates = vec![]; let mut held = vec![];
    for id in item_ids {
        // Held original members are still visible in the plan, never guessed.
        if !rows(&installed.packet["originalJob"]["prepareBundle"], "itemIds")?.contains(&json!(id)) { return Err(bad("Foreign retained recipient")); }
        match preview(d, installed, id) {
            Ok(row) => candidates.push(row),
            Err(e) => held.push(json!({"itemId": id, "reason": e.1, "status": "held"})),
        }
    }
    let mut result = json!({"contract": CONTRACT, "captureSha256": installed.sha256(), "jobId": installed.job_id()?,
        "account": d["account"], "connectorBinding": d["connectorBinding"], "authority": authority,
        "itemIds": item_ids, "candidates": candidates, "held": held, "createsDraftsOnly": true});
    result["planDigest"] = json!(digest(&result)); Ok(result)
}

/// Constructed only by commit, inaccessible to HTTP deserialization. Ordinary
/// creation must validate this typed permit before its single reservation
/// exemption. Other route, capability, media, revision and conductor gates stay.
pub(crate) struct RecoveryPermit { installed: InstalledCapture, reviewed: Value }
impl RecoveryPermit {
    pub(crate) fn validate_creation(&self, d: &Value, body: &Value) -> ApiResult<()> {
        let item_id = text(body, "itemId")?;
        if body["_verifiedActor"]["id"] != "local-owner" || body["_verifiedActor"]["role"] != "owner"
            || self.reviewed["itemId"] != item_id || body["expectedRevision"] != self.reviewed["currentItem"]["revision"]
            || body["kind"] != self.reviewed["candidate"]["proposal"]["kind"]
            || body["text"] != self.reviewed["candidate"]["proposal"]["text"] || preview(d, &self.installed, item_id)? != self.reviewed {
            return Err(conflict("Typed retained creation permit changed"));
        }
        Ok(())
    }
}
fn request_id(body: &Value) -> ApiResult<&str> {
    let id = text(body, "requestId")?;
    if id.len() > 100 || !id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_') { return Err(bad("Invalid retained requestId")); }
    Ok(id)
}
fn unsigned_proof(proof: &Value) -> Value { let mut v = proof.clone(); if let Some(o) = v.as_object_mut() { o.remove("proofSha256"); } v }
fn projection(p: &Value) -> Value {
    let mut v = serde_json::Map::new();
    for k in ["id", "revision", "itemId", "itemRevision", "kind", "text", "routeTarget", "contextEvidenceDigest", "branchContextDigest", "reviewContextDigest", "decisionMediaContract"] {
        v.insert(k.into(), p[k].clone());
    }
    Value::Object(v)
}
pub(crate) fn commit(d: &mut Value, actor: &Actor, installed: &InstalledCapture, body: &Value) -> ApiResult<Value> {
    exact(body, &["requestId", "planDigest", "itemIds", "checks", "reason"])?;
    let authority = authority(actor)?; owner(d, installed)?; let request = request_id(body)?;
    exact(&body["checks"], CHECKS)?;
    if body["checks"].as_object().unwrap().values().any(|v| v != "pass") || text(body, "reason")?.len() > 2000 {
        return Err(bad("Every explicit retained review check must pass with a bounded reason"));
    }
    let ids: Vec<String> = rows(body, "itemIds")?.iter().map(|v| v.as_str().map(str::to_owned).ok_or_else(|| bad("Invalid retained recipient"))).collect::<ApiResult<_>>()?;
    let request_digest = digest(&json!({"request": body, "authority": authority}));
    let old: Vec<_> = rows(d, "audit")?.iter().filter(|a| a["action"] == "retained_paid_recovery.committed" && a["requestId"] == request).collect();
    if old.len() > 1 { return Err(conflict("Duplicate retained commit receipt")); }
    if let Some(receipt) = old.first() {
        if receipt["requestDigest"] != request_digest || receipt["captureSha256"] != installed.sha256() { return Err(conflict("Retained requestId reused with different content")); }
        // Storage acknowledgement only; never dispatch, re-review or recreation.
        return Ok(json!({"requestId": request, "proposals": receipt["proposals"], "replayed": true, "externalActions": 0}));
    }
    let reviewed = plan(d, actor, installed, &ids)?;
    if body["planDigest"] != reviewed["planDigest"] { return Err(conflict("Retained exact text/current context/authority plan changed")); }
    if !rows(&reviewed, "held")?.is_empty() { return Err(conflict("Retained plan has held recipients; choose only separately reviewed current candidates")); }
    // Pure reducer owns rollback too; storage transaction must publish only
    // this complete transition. No partial draft survives a late guard failure.
    let mut next = d.clone(); let original_jobs = d["jobs"].clone(); let mut refs = vec![];
    for reviewed_row in rows(&reviewed, "candidates")? {
        let permit = RecoveryPermit { installed: installed.clone(), reviewed: reviewed_row.clone() };
        let candidate = &reviewed_row["candidate"]["proposal"];
        let proposal_body = json!({"itemId": reviewed_row["itemId"], "expectedRevision": reviewed_row["currentItem"]["revision"],
            "kind": candidate["kind"], "text": candidate["text"], "sources": [],
            "decisionMediaContract": decision_media::CONTRACT, "_verifiedActor": actor.public_json()});
        let mut p = crate::create_retained_recovery_proposal(&mut next, &proposal_body, &permit)?;
        let mut proof = json!({"version": 1, "contract": CONTRACT, "captureSha256": installed.sha256(),
            "originalJobId": installed.job_id()?, "originalJobSha256": digest(&installed.packet["originalJob"]),
            "originalScopeReservationSha256": digest(&installed.packet["originalJob"]["scopeReservation"]),
            "rawResponseSha256": hash(&installed.raw_response_bytes), "retainedCandidateSha256": digest(&reviewed_row["candidate"]),
            "requestId": request, "requestDigest": request_digest, "planDigest": reviewed["planDigest"],
            "selectedBy": actor.public_json(), "authority": authority, "checks": body["checks"], "reason": body["reason"],
            "currentReviewEvidenceSha256": digest(&stable_review_evidence(reviewed_row["currentEvidence"].clone())), "proposalBinding": projection(&p),
            "source": "authenticated_owner_recovery_of_retained_suggestion", "historicalRuntimeSourceVerified": false,
            "historicalInstructionSourceVerified": false, "modelCompletionClaimed": false, "selectedAt": now()});
        proof["proofSha256"] = json!(digest(&proof)); p[FIELD] = proof;
        *row_mut(&mut next, "proposals", text(&p, "id")?)? = p.clone();
        validate_proposal(&next, &p, installed, None)?;
        refs.push(json!({"id": p["id"], "revision": p["revision"]}));
    }
    if next["jobs"] != original_jobs { return Err(conflict("Retained draft creation changed original paid/worker jobs")); }
    list_mut(&mut next, "audit").push(json!({"id": id(), "action": "retained_paid_recovery.committed", "refId": installed.job_id()?,
        "requestId": request, "requestDigest": request_digest, "captureSha256": installed.sha256(), "itemIds": ids, "proposals": refs,
        "createdBy": actor.public_json(), "authority": authority, "createdAt": now(), "draftsOnly": true}));
    validate_change(d, &next, installed)?; *d = next;
    Ok(json!({"requestId": request, "proposals": refs, "replayed": false, "externalActions": 0, "editorialReviewRequired": true, "approvalRequired": true}))
}
pub(crate) fn artifact_sha256(p: &Value) -> Option<&str> { p[FIELD]["captureSha256"].as_str() }
pub(crate) fn assert_actor(p: &Value, actor: &Actor) -> ApiResult<()> {
    if p.get(FIELD).is_some() { authority(actor)?; }
    Ok(())
}

/// Called by reservation owner resolution AND proposal_current at approval,
/// execute admission and own-operation dispatch. No recursive assert_proposal.
pub(crate) fn validate_proposal(d: &Value, p: &Value, installed: &InstalledCapture, own_operation: Option<&Value>) -> ApiResult<String> {
    let job = owner(d, installed)?; let proof = &p[FIELD]; let item_id = text(p, "itemId")?;
    let candidate = installed.candidate(item_id)?;
    if proof["version"] != 1 || proof["contract"] != CONTRACT || proof["captureSha256"] != installed.sha256()
        || proof["originalJobId"] != job["id"] || proof["originalJobSha256"] != digest(job)
        || proof["originalScopeReservationSha256"] != digest(&job["scopeReservation"])
        || proof["rawResponseSha256"] != hash(&installed.raw_response_bytes)
        || hash(&installed.packet_bytes) != installed.sha256() || proof["retainedCandidateSha256"] != digest(candidate)
        || proof["selectedBy"]["id"] != "local-owner" || proof["selectedBy"]["role"] != "owner"
        || proof["authority"] != dispatch_authority::approval_binding(&Actor::local_owner("unused"))
        || proof["historicalRuntimeSourceVerified"] != false || proof["historicalInstructionSourceVerified"] != false
        || proof["modelCompletionClaimed"] != false || proof["proposalBinding"] != projection(p)
        || proof["proofSha256"] != digest(&unsigned_proof(proof)) || p["kind"] != candidate["proposal"]["kind"]
        || p["text"] != candidate["proposal"]["text"] || p["revision"] != 1
        || ["prepareRunId", "prepareBundleId", "prepareBundleDigest", "generationMetadata", "paidGeneration", "recovery", "operatorCloseDecision", "origin"].iter().any(|k| p.get(*k).is_some_and(|v| !v.is_null())) {
        return Err(conflict("Exact native retained recovery proof changed or forged generation lineage"));
    }
    exact(&proof["checks"], CHECKS)?;
    if proof["checks"].as_object().unwrap().values().any(|v| v != "pass") { return Err(conflict("Retained current semantic checks missing")); }
    let binding = active_binding(d)?; let item = bound_item(&binding, row(d, "items", item_id)?)?;
    validate_route(p, &binding, &item)?;
    let context = prepare_bundle::EvidenceContext::new(d);
    if p["itemRevision"] != item["revision"] || p["contextEvidenceDigest"] != item["contextEvidenceDigest"]
        || p["branchContextDigest"] != item["branchContextDigest"]
        || p["reviewContextDigest"] != context.review_fingerprint(item_id).map_err(conflict)?
        || proof["currentReviewEvidenceSha256"] != digest(&stable_review_evidence(reviewed_evidence(d, item_id)?)) {
        return Err(conflict("Retained current review/item/source context changed"));
    }
    // A typed exemption remains exact to this proposal. Never omit UNKNOWN or
    // competing operation rows. Only its independently authenticated, already
    // saved dispatching operation may be removed from the preparation check.
    let mut guard = d.clone();
    if let Some(op) = own_operation {
        if op["proposalId"] != p["id"] || op["itemId"] != p["itemId"] || op["status"] != "dispatching"
            || op["approvedRetainedPaidRecoverySha256"] != proof["proofSha256"]
            || !rows(d, "operations")?.iter().any(|saved| saved == op) || op["target"] != item
            || op["action"]["actionId"] != op["id"] || op["action"] != action_for(p, &item, text(op, "id")?)? {
            return Err(conflict("Retained recovery requires exact saved approved own operation"));
        }
        let approval = row(d, "approvals", text(op, "approvalId")?)?;
        if !rows(approval, "proposals")?.iter().any(|r| r["id"] == p["id"] && r["revision"] == p["revision"] && r["proposal"][FIELD] == *proof)
            || op["dispatchAuthority"]["approved"] != proof["authority"] || op["dispatchAuthority"]["executed"] != proof["authority"] {
            return Err(conflict("Retained own operation approval or authority changed"));
        }
        guard["operations"].as_array_mut().unwrap().retain(|saved| saved["id"] != op["id"]);
    }
    preparation_reservations::assert_available(&guard, &[item_id.into()], Some(installed.job_id()?))?;
    // Native reservation validates owner membership/route; exact recipient
    // action fences still reject known completed effects as well as UNKNOWN.
    if rows(&guard, "operations")?.iter().any(|op| recipient_operation_blocks(op, p, &item)) {
        return Err(conflict("Retained recipient already has an admitted effect"));
    }
    Ok(installed.job_id()?.into())
}

/// Attach to the storage writer validator. Admission proof and original failed
/// owner remain immutable even while ordinary editorial/status history evolves.
pub(crate) fn validate_change(before: &Value, after: &Value, installed: &InstalledCapture) -> ApiResult<()> {
    if row(before, "jobs", installed.job_id()?)? != row(after, "jobs", installed.job_id()?)?
        || row(after, "jobs", installed.job_id()?)? != &installed.packet["originalJob"] {
        return Err(conflict("Original failed job/reservation/first/result are immutable"));
    }
    for prior in rows(before, "proposals")?.iter().filter(|p| p.get(FIELD).is_some()) {
        let current = row(after, "proposals", text(prior, "id")?)?;
        if prior[FIELD] != current[FIELD] { return Err(conflict("Retained recovery proof cannot be removed or rewritten")); }
    }
    for current in rows(after, "proposals")?.iter().filter(|p| p.get(FIELD).is_some()) {
        if let Some(prior) = rows(before, "proposals")?.iter().find(|p| p["id"] == current["id"]) {
            if prior.get(FIELD).is_none() { return Err(conflict("Recovery proof may be attached only to a newly created retained draft")); }
        } else {
            validate_proposal(after, current, installed, None)?;
        }
    }
    let old: Vec<_> = rows(before, "audit")?.iter().filter(|a| a["action"] == "retained_paid_recovery.committed").collect();
    let next: Vec<_> = rows(after, "audit")?.iter().filter(|a| a["action"] == "retained_paid_recovery.committed").collect();
    if next.len() < old.len() || !next.starts_with(&old) { return Err(conflict("Retained commit audit is append-only")); }
    Ok(())
}

#[cfg(test)]
#[path = "retained_paid_recovery_tests.rs"]
pub(crate) mod tests;
