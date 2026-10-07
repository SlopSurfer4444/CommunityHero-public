//! Compact archive-origin reservations. No native operation, approval or job is
//! fabricated, and a fresh local item ID never bypasses an external alias hold.
use crate::*;
use std::collections::HashSet;

pub(crate) const FIELD: &str = "externalActionReconciliation.v1";
pub(crate) const MAX_BYTES: usize = 2 * 1024 * 1024;
pub(crate) const MAX_RESERVATIONS: usize = 2048;
pub(crate) const MAX_SEALS: usize = 64;

fn exact(value: &Value, fields: &[&str]) -> bool {
    value.as_object().is_some_and(|o| o.len() == fields.len() && fields.iter().all(|key| o.contains_key(*key)))
}
fn text(value: &Value, key: &str) -> ApiResult<String> {
    value[key].as_str().filter(|s| !s.is_empty() && s.len() <= 4096 && s.trim() == *s
        && !s.chars().any(char::is_control)).map(str::to_owned)
        .ok_or_else(|| conflict("Archive safety identity is missing or malformed"))
}
fn hash(value: &Value) -> bool {
    value.as_str().is_some_and(|s| s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit()))
}
fn same_namespace(a: &Value, b: &Value) -> bool {
    ["id", "workspaceId", "accountId", "connector", "providerAccountId"].iter().all(|key| a[*key] == b[*key])
}
fn alias(value: &Value) -> ApiResult<()> {
    if value["namespace"] != "provider_item" || value.as_object().is_none_or(|fields|
        fields.keys().any(|key| !matches!(key.as_str(), "namespace" | "value" | "objectId"))) {
        return Err(conflict("Unsupported archive recipient alias"));
    }
    text(value, "value")?;
    if value.get("objectId").is_some() { text(value, "objectId")?; }
    Ok(())
}
fn matches_alias(alias: &Value, target: &Value) -> bool {
    alias["value"] == target["itemId"]
        && alias.get("objectId").is_none_or(|object| *object == target["objectId"])
}
fn route_identity(target:&Value)->Value {
    json!({"connectorBinding":target["connectorBinding"],"objectId":target["objectId"],"itemId":target["itemId"],
        "postKey":target["postKey"],"conversationKey":target["conversationKey"],"revision":target["revision"]})
}
fn current_proof(proof:&Value)->bool {
    let dates=proof["verifiedAt"].as_str().zip(proof["expiresAt"].as_str()).and_then(|(a,b)|
        chrono::DateTime::parse_from_rfc3339(a).ok().zip(chrono::DateTime::parse_from_rfc3339(b).ok()));
    dates.is_some_and(|(start,end)|start<=chrono::Utc::now()&&chrono::Utc::now()<end
        &&end.signed_duration_since(start).num_milliseconds()>0
        &&end.signed_duration_since(start).num_milliseconds()<=300_000)
}
fn source_refs(value: &Value) -> ApiResult<()> {
    let refs = value.as_array().filter(|refs| !refs.is_empty() && refs.len() <= 16)
        .ok_or_else(|| conflict("Archive safety source references are incomplete"))?;
    for source in refs {
        if !hash(&source["sourceSha256"]) || !hash(&source["archiveManifestHash"])
            ||source.as_object().is_none_or(|fields|fields.keys().any(|key|!matches!(key.as_str(),
                "sourceSha256"|"archiveManifestHash"|"lineSha256"|"operationId"|"attemptId")))
            || source.get("lineSha256").is_some_and(|v| !hash(v))
            || source.get("operationId").is_some_and(|v| !v.is_string())
            || source.get("attemptId").is_some_and(|v| !v.is_string()) {
            return Err(conflict("Archive safety source identity is malformed"));
        }
    }
    Ok(())
}

pub(crate) fn validate_value(d: &Value, safety: &Value) -> ApiResult<()> {
    if safety.to_string().len() > MAX_BYTES { return Err(conflict("reconciliation_budget_exceeded")); }
    if !exact(safety, &["mode", "epoch", "safetyFence", "sealedSourceRefs", "recipientReservations", "quarantineCoverage", "archiveRefs"])
        || safety["mode"] != "archive_fence" || safety["epoch"].as_u64().is_none_or(|n| n == 0) {
        return Err(conflict("Invalid archive safety schema"));
    }
    let header = &safety["safetyFence"];
    if !exact(header, &["companyId", "connectionScope", "archiveManifestHash", "sourceBindingRefs", "predicate", "coverage", "sealedAt"])
        || header["companyId"] != accounts::Profile::from_workspace(d)?.key()
        || header["connectionScope"] != active_binding(d)?.to_json()
        || !hash(&header["archiveManifestHash"]) || header["coverage"]["complete"] != true
        || !header["coverage"]["declaredRows"].is_u64() {
        return Err(conflict("Archive safety company, connection or declared coverage mismatch"));
    }
    text(header, "predicate")?; text(header, "sealedAt")?;
    source_refs(&header["sourceBindingRefs"])?;
    for field in ["sealedSourceRefs", "archiveRefs"] {
        let refs = safety[field].as_array().filter(|refs| !refs.is_empty() && refs.len() <= MAX_SEALS)
            .ok_or_else(|| conflict("Archive safety seal/reference budget exceeded"))?;
        if refs.iter().any(|r| !hash(&r["sha256"])) { return Err(conflict("Archive safety referenced bytes lack identities")); }
    }
    let declared_sources:HashSet<_>=safety["sealedSourceRefs"].as_array().unwrap().iter().map(|r|r["sha256"].as_str().unwrap()).collect();
    let declared_archives:HashSet<_>=safety["archiveRefs"].as_array().unwrap().iter().map(|r|r["sha256"].as_str().unwrap()).collect();
    if !declared_archives.contains(header["archiveManifestHash"].as_str().unwrap()) {
        return Err(conflict("Archive manifest bytes are outside the declared restore closure"));
    }
    let bound_refs=|refs:&Value|->ApiResult<()> {
        source_refs(refs)?;
        if refs.as_array().unwrap().iter().any(|source|
            !declared_sources.contains(source["sourceSha256"].as_str().unwrap())
                ||!declared_archives.contains(source["archiveManifestHash"].as_str().unwrap())) {
            return Err(conflict("Archive recipient evidence is outside the declared source/reference closure"));
        }
        Ok(())
    };
    bound_refs(&header["sourceBindingRefs"])?;
    let reservations = safety["recipientReservations"].as_array()
        .filter(|rows| rows.len() <= MAX_RESERVATIONS).ok_or_else(|| conflict("reconciliation_budget_exceeded"))?;
    let mut unique = HashSet::new();
    for reservation in reservations {
        let binding = ConnectorBinding::from_json(&reservation["connectionScope"]).map_err(|e| conflict(e.0))?;
        if !same_namespace(&binding.to_json(), &header["connectionScope"])
            ||!exact(reservation,&["connectionScope","alias","origin","evidenceClass","disposition","sourceRefs"])
            || !matches!(reservation["origin"].as_str(), Some("archived_native" | "external_manual"))
            || !matches!(reservation["evidenceClass"].as_str(), Some("unknown" | "inflight" | "confirmed_reply" | "possible_effect" | "unmapped"))
            || !matches!(reservation["disposition"].as_str(), Some("excluded" | "readback_only")) {
            return Err(conflict("Archive recipient reservation scope or disposition is invalid"));
        }
        alias(&reservation["alias"])?; bound_refs(&reservation["sourceRefs"])?;
        let identity = json!([reservation["connectionScope"], reservation["alias"]]).to_string();
        if !unique.insert(identity) { return Err(conflict("Duplicate archive recipient reservation")); }
    }
    let quarantines = safety["quarantineCoverage"]["namespaces"].as_array()
        .filter(|rows| rows.len() <= MAX_SEALS).ok_or_else(|| conflict("Archive namespace coverage missing or over budget"))?;
    if safety["quarantineCoverage"]["complete"] != true { return Err(conflict("Archive namespace coverage incomplete")); }
    for quarantine in quarantines {
        if !same_namespace(&quarantine["connectionScope"], &header["connectionScope"])
            || quarantine["reason"].as_str().is_none_or(str::is_empty) {
            return Err(conflict("Archive quarantine namespace scope missing"));
        }
        bound_refs(&quarantine["sourceRefs"])?;
    }
    Ok(())
}

pub(crate) fn validate(d: &Value) -> ApiResult<()> {
    let safety = d.get(FIELD).ok_or_else(|| conflict("Archive safety fence is required"))?;
    validate_value(d, safety)
}

pub(crate) fn fence_recipient(d: &Value, target: &Value, required: bool) -> ApiResult<()> {
    let Some(safety) = d.get(FIELD) else {
        return if required { Err(conflict("Archive safety fence is required")) } else { Ok(()) };
    };
    validate_value(d, safety)?;
    let binding = active_binding(d)?.to_json();
    if target["connectorBinding"] != binding { return Err(conflict("Current target binding differs from archive safety scope")); }
    for reservation in safety["recipientReservations"].as_array().unwrap() {
        if same_namespace(&reservation["connectionScope"], &binding) && matches_alias(&reservation["alias"], target) {
            return Err(conflict("Recipient is retained by an archived external-effect reservation; readback only"));
        }
    }
    for quarantine in safety["quarantineCoverage"]["namespaces"].as_array().unwrap() {
        if same_namespace(&quarantine["connectionScope"], &binding) {
            // A namespace exception is admitted only by a root-owned verified
            // readback reducer. Plain absence in a projection never clears it.
            let clears = quarantine["clearTargets"].as_array().is_some_and(|proofs| proofs.iter().any(|proof|
                proof["target"] == route_identity(target) && proof["connectionScope"] == binding
                    && proof["archiveManifestHash"] == safety["safetyFence"]["archiveManifestHash"]
                    && proof["coverageComplete"] == true && proof["noPublishedBrandReply"] == true
                    && hash(&proof["readbackReceiptSha256"]) && hash(&proof["archiveLookupReceiptSha256"])
                    && proof["sourceRevision"] == target["revision"] && current_proof(proof)));
            if !clears { return Err(conflict("Recipient namespace has ambiguous archived effects; positive current proof required")); }
        }
    }
    Ok(())
}

fn retain_holds(previous:&Value,next:&Value)->ApiResult<()> {
    let rows=next["recipientReservations"].as_array().ok_or_else(||conflict("Archive reservations missing"))?;
    for reservation in previous["recipientReservations"].as_array().into_iter().flatten() {
        if !rows.contains(reservation){return Err(conflict("Archived external-effect holds cannot be pruned or weakened; explicit reconciliation is required"));}
    }
    let namespaces=next["quarantineCoverage"]["namespaces"].as_array().ok_or_else(||conflict("Archive quarantine coverage missing"))?;
    for previous in previous["quarantineCoverage"]["namespaces"].as_array().into_iter().flatten() {
        if !namespaces.iter().any(|next|["connectionScope","reason","sourceRefs"].iter().all(|field|previous[*field]==next[*field])) {
            return Err(conflict("Ambiguous archived namespace cannot disappear in a fresh queue"));
        }
    }
    Ok(())
}

/// Under the SAME common M and writer, after final barrier zero. This reducer
/// never queries a provider or imports the old workflow/operation histories.
pub(crate) fn install(d: &mut Value, safety: &Value, expected_epoch: u64) -> ApiResult<Value> {
    validate_value(d, safety)?;
    let gate = &d[connection_gate::FIELD];
    if gate["state"] != "blocked" || gate["gateEpoch"].as_u64() != Some(expected_epoch)
        || gate["finalReceipt"]["providerCapablePermits"] != 0
        || list(d, "operations").iter().any(|op| op[connection_gate::PERMIT_FIELD]["phase"] == "dispatch_armed") {
        return Err(conflict("Archive fence activation requires the final zero-permit barrier"));
    }
    if let Some(prior) = d.get(FIELD) {
        if prior == safety { return Ok(json!({"status":"installed","replayed":true,"fenceSha256":connection_gate::digest(safety)})); }
        if prior["epoch"].as_u64().and_then(|epoch| epoch.checked_add(1)) != safety["epoch"].as_u64() {
            return Err(conflict("Archive fence epoch/payload changed outside guarded replacement"));
        }
        retain_holds(prior,safety)?;
    }
    d[FIELD] = safety.clone();
    let receipt = json!({"status":"installed","replayed":false,"fenceSha256":connection_gate::digest(safety),
        "gateEpoch":expected_epoch,"archiveManifestHash":safety["safetyFence"]["archiveManifestHash"]});
    let mut event = receipt.clone(); event["id"] = json!(format!("external-safety:{}", connection_gate::digest(&receipt)));
    event["action"] = json!("external_safety.installed"); event["createdAt"] = json!(now());
    list_mut(d, "audit").push(event); Ok(receipt)
}

pub(crate) fn validate_change(before:&Value,after:&Value)->ApiResult<()> {
    if let Some(next)=after.get(FIELD){validate_value(after,next)?;}
    if let Some(previous)=before.get(FIELD) {
        let next=after.get(FIELD).ok_or_else(||conflict("Archive safety fence cannot be dropped by reset or queue replacement"))?;
        if previous!=next&&previous["epoch"].as_u64().and_then(|n|n.checked_add(1))!=next["epoch"].as_u64(){
            return Err(conflict("Archive safety epoch cannot regress or change without an admitted replacement"));
        }
        retain_holds(previous,next)?;
    }
    Ok(())
}

#[cfg(test)]
#[path = "external_reconciliation_tests.rs"]
mod tests;
