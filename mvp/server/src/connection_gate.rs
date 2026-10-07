//! One company/connection gate for ordinary and conductor mutations. A durable
//! pre-arm is conservative intent, never evidence that a provider was called.
//! Reducers require the existing writer; the short mutex is never held over I/O.
use crate::*;
use sha2::{Digest, Sha256};
use std::sync::{OnceLock, Weak};
use tokio::sync::{Mutex, OwnedMutexGuard};

pub(crate) const FIELD: &str = "connectionSendGate.v1";
pub(crate) const PERMIT_FIELD: &str = "dispatchPermit";
pub(crate) const MAX_MUTATION_PERMITS: usize = 4;
pub(crate) const MAX_DRAIN_MS: u64 = 900_000;
// Every field emitted by prearm is retained unchanged through cessation. Extra
// fields remain preserved, but no missing native identity field proves final0.
const PERMIT_IDENTITY_FIELDS: &[&str] = &["version","id","operationId","attemptId","account",
    "connectionBinding","owner","gateEpoch","archiveFenceSha256","armedAt","providerAttemptObserved"];

#[derive(Clone, Copy)]
pub(crate) enum Scope<'a> { Operation(&'a Value), Control }

type Gates = std::sync::Mutex<HashMap<String, Weak<Mutex<()>>>>;
static GATES: OnceLock<Gates> = OnceLock::new();

/// All held-lock paths use M -> optional conductor guard -> writer. A writer
/// must not call this function, and an old conductor guard must be dropped first.
pub(crate) async fn lock(app: &App) -> OwnedMutexGuard<()> {
    let key = json!([app.data.to_string_lossy(), app.account.key()]).to_string();
    let gate = {
        let mut gates = GATES.get_or_init(Default::default).lock().unwrap_or_else(|e| e.into_inner());
        gates.retain(|_, gate| gate.strong_count() > 0);
        if let Some(gate) = gates.get(&key).and_then(Weak::upgrade) { gate }
        else {
            let gate = Arc::new(Mutex::new(()));
            gates.insert(key, Arc::downgrade(&gate)); gate
        }
    };
    gate.lock_owned().await
}

pub(crate) fn digest(value: &Value) -> String {
    format!("{:x}", Sha256::digest(value.to_string().as_bytes()))
}
fn hash(value: &Value) -> bool {
    value.as_str().is_some_and(|s| s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit()))
}
fn denied() -> ApiError { conflict("Connection dispatch gate is closed, unverified or changed") }
fn owner(d: &Value) -> ApiResult<Value> {
    runtime_lifecycle::admission_token(d, runtime_lifecycle::AdmissionClass::SocialDispatch)?;
    Ok(d["runtimeLifecycle"]["owner"].clone())
}
fn control(d: &Value) -> ApiResult<&Value> {
    let gate = d.get(FIELD).ok_or_else(denied)?;
    if gate["version"] != 1 || gate["account"] != accounts::Profile::from_workspace(d)?.key()
        || gate["connectionBinding"] != active_binding(d)?.to_json()
        || gate["gateEpoch"].as_u64().is_none_or(|n| n == 0)
        || !matches!(gate["state"].as_str(), Some("open" | "closing" | "blocked"))
        || gate["mutationPermitCap"].as_u64().is_none_or(|n| n == 0 || n > MAX_MUTATION_PERMITS as u64)
        || gate["drainDeadlineMs"].as_u64().is_none_or(|n| n == 0 || n > MAX_DRAIN_MS)
        || gate["cohort"].as_array().is_none_or(|rows|rows.len()>MAX_MUTATION_PERMITS) { return Err(denied()); }
    if !gate["closingIntent"].is_null() {
        if !gate["closingIntent"].is_object()||gate["closingIntent"]["cohortSha256"]!=digest(&gate["cohort"])
            ||gate["closingIntent"]["cohortCount"].as_u64()!=gate["cohort"].as_array().map(|rows|rows.len() as u64)
            ||gate["closingIntent"]["owner"]!=gate["owner"] {
            return Err(denied());
        }
    } else if gate["state"]=="closing"||!gate["cohort"].as_array().unwrap().is_empty() {return Err(denied());}
    Ok(gate)
}
fn capable(op: &Value) -> bool { op[PERMIT_FIELD]["phase"] == "dispatch_armed" }
fn matching_scope(gate: &Value, op: &Value) -> bool {
    op[PERMIT_FIELD]["connectionBinding"] == gate["connectionBinding"]
        && op[PERMIT_FIELD]["account"] == gate["account"]
}
/// A malformed settled row is evidence of an incomplete projection too. Do
/// not omit it from a scoped load and then interpret the remainder as zero.
pub(crate) fn valid_permit(op: &Value) -> bool {
    let permit = &op[PERMIT_FIELD];
    if permit.as_object().is_none_or(|fields|!PERMIT_IDENTITY_FIELDS.iter().all(|key|fields.contains_key(*key))
        ||!fields.contains_key("phase")) {return false;}
    let Ok(owner)=runtime_lifecycle::parse_token(&permit["owner"]) else {return false;};
    let Some(account)=permit["account"].as_str().and_then(|key|accounts::Profile::parse(key).ok()) else {return false;};
    let Ok(binding)=ConnectorBinding::from_json(&permit["connectionBinding"]) else {return false;};
    let Some(armed_at)=permit["armedAt"].as_str().and_then(|at|chrono::DateTime::parse_from_rfc3339(at).ok()) else {return false;};
    permit["version"] == 1
        && permit["id"].as_str().is_some_and(|id| !id.is_empty())
        && op["id"].as_str().is_some_and(|id| !id.is_empty())
        && op["attemptId"].as_str().is_some_and(|id| !id.is_empty())
        && permit["operationId"] == op["id"] && permit["attemptId"] == op["attemptId"]
        && permit["gateEpoch"].as_u64().is_some_and(|epoch| epoch > 0)
        && account.display()==owner.account
        && binding.to_json()==permit["connectionBinding"]
        && binding.validate_scope("local-pilot",&owner.account).is_ok()
        && permit["connectionBinding"]==op["target"]["connectorBinding"]
        && (permit["archiveFenceSha256"].is_null()||hash(&permit["archiveFenceSha256"]))
        && permit["providerAttemptObserved"]==false
        && match permit["phase"].as_str() {
            Some("dispatch_armed") => permit.get("cessation").is_none() && permit.get("settledAt").is_none(),
            Some("transport_settled") => {
                let receipt=&permit["cessation"];
                receipt.as_object().is_some_and(|fields|fields.len()==6
                    && ["kind","evidenceSha256","owner","operationId","attemptId","permitId"].iter().all(|key|fields.contains_key(*key)))
                    && matches!(receipt["kind"].as_str(),Some("returned"|"contained"|"proven_unsent"))
                    && hash(&receipt["evidenceSha256"]) && receipt["owner"]==permit["owner"]
                    && receipt["operationId"]==op["id"] && receipt["attemptId"]==op["attemptId"]
                    && receipt["permitId"]==permit["id"]
                    && permit["settledAt"].as_str().and_then(|at|chrono::DateTime::parse_from_rfc3339(at).ok())
                        .is_some_and(|settled_at|settled_at>=armed_at)
            },
            _ => false,
        }
}
fn active<'a>(d: &'a Value, gate: &Value) -> ApiResult<Vec<&'a Value>> {
    let mut result = vec![];
    for op in list(d, "operations").iter().filter(|op|op.get(PERMIT_FIELD).is_some()) {
        if !valid_permit(op) {
            return Err(conflict("Malformed original dispatch permit cannot be treated as inactive"));
        }
        if capable(op) {
            if !matching_scope(gate, op) { return Err(conflict("Foreign or malformed active dispatch permit")); }
            result.push(op);
        }
    }
    Ok(result)
}

/// Initialized only by the root lifecycle coordinator after legacy workers are
/// contained. Startup is closed even when a credential timestamp looks valid.
pub(crate) fn initialize_closed(d: &mut Value, receipt_sha256: &str, cap: usize, drain_ms: u64,
    archive_fence_required: bool) -> ApiResult<()> {
    if d.get(FIELD).is_some() || cap == 0 || cap > MAX_MUTATION_PERMITS || drain_ms == 0
        || drain_ms > MAX_DRAIN_MS || !hash(&json!(receipt_sha256))
        || list(d, "operations").iter().any(capable) { return Err(denied()); }
    let current_owner = owner(d)?;
    d[FIELD] = json!({"version":1,"account":accounts::Profile::from_workspace(d)?.key(),
        "connectionBinding":active_binding(d)?.to_json(),"owner":current_owner,"gateEpoch":1,
        "state":"blocked","availability":{"state":"unverified"},"mutationPermitCap":cap,
        "drainDeadlineMs":drain_ms,"archiveFenceRequired":archive_fence_required,
        "cohort":[],"closingIntent":null,"finalReceipt":null,"bootstrapReceiptSha256":receipt_sha256});
    Ok(())
}

pub(crate) fn fence_admission(d: &Value, targets: &[Value]) -> ApiResult<()> {
    let gate = control(d)?;
    if gate["state"] != "open" || gate["availability"]["state"] != "ready"
        || gate["owner"] != owner(d)? { return Err(denied()); }
    for target in targets {
        let bound = bound_item(&active_binding(d)?, target)?;
        external_reconciliation::fence_recipient(d, &bound, gate["archiveFenceRequired"] == true)?;
    }
    Ok(())
}

/// Dependency information only; this function grants neither send authority nor
/// permission to replace the current immutable source/paid preparation origin.
pub(crate) fn preparation_dependency(d: &Value, requires_connection: bool) -> ApiResult<Value> {
    if !requires_connection { return Ok(json!({"status":"ready","requiresConnection":false})); }
    let gate = control(d)?;
    let ready = gate["state"] == "open" && gate["availability"]["state"] == "ready" && gate["owner"] == owner(d)?;
    Ok(json!({"status":if ready {"ready"} else {"waiting_dependency"},"requiresConnection":true,
        "reason":gate["availability"]["reason"],"gateEpoch":gate["gateEpoch"],
        "connectionBinding":gate["connectionBinding"],"retryAuthorized":false}))
}

/// Root receives this only from the admitted protected connector projection.
/// A normal HTTP payload is never deserialized into a ready observation.
#[derive(Clone,Debug)]
pub(crate) struct AvailabilityObservation {
    pub(crate) projection:Value,
    pub(crate) protected_receipt_sha256:String,
}
pub(crate) fn observe_availability(d:&mut Value,observation:&AvailabilityObservation)->ApiResult<Value> {
    let gate=control(d)?.clone();let projection=&observation.projection;
    if projection["version"]!=1||projection["account"]!=gate["account"]
        ||projection["connectionBinding"]!=gate["connectionBinding"]
        ||!matches!(projection["state"].as_str(),Some("ready"|"blocked"|"recovering"|"needs_owner"|"unverified"))
        ||projection["generation"].as_u64().is_none()
        ||!hash(&json!(observation.protected_receipt_sha256))
        ||projection["receiptSha256"]!=observation.protected_receipt_sha256
        ||projection["generation"].as_u64()<gate["availability"]["generation"].as_u64()
        ||projection.get("reason").is_some_and(|reason|reason.as_str().is_none_or(|s|s.is_empty()||s.len()>80
            ||!s.bytes().all(|byte|byte.is_ascii_lowercase()||byte==b'_'))) {
        return Err(conflict("Protected connection availability projection is stale, foreign or malformed"));
    }
    if projection["state"]!="ready"&&gate["state"]=="open" {
        let intent=format!("auth-barrier:{}:{}",projection["generation"],&observation.protected_receipt_sha256[..24]);
        request_close(d,&intent,"auth_unavailable")?;
    }
    d[FIELD]["availability"]=projection.clone();
    // Ready observation never reopens the send gate or replays old references.
    Ok(d[FIELD]["availability"].clone())
}

/// Called under M -> conductor -> writer, BEFORE bridge enqueue. The projection
/// must include the original operation and the complete active permit cohort.
pub(crate) fn prearm(d: &mut Value, original: &Value) -> ApiResult<Value> {
    fence_admission(d, &[original["target"].clone()])?;
    let gate = control(d)?.clone();
    let saved = row(d, "operations", required(original, "id")?)?;
    if ["id", "attemptId", "action", "target", "dispatchAuthority"].iter().any(|f| saved[*f] != original[*f])
        || saved["status"] != "dispatching" || saved.get(PERMIT_FIELD).is_some() {
        return Err(conflict("Original dispatch attempt already armed or changed; reconciliation required"));
    }
    if active(d, &gate)?.len() >= gate["mutationPermitCap"].as_u64().unwrap() as usize {
        return Err(conflict("Connection dispatch cohort is at its admitted finite capacity"));
    }
    let permit = json!({"version":1,"id":id(),"operationId":original["id"],"attemptId":original["attemptId"],
        "account":gate["account"],"connectionBinding":gate["connectionBinding"],"owner":gate["owner"],
        "gateEpoch":gate["gateEpoch"],"archiveFenceSha256":d.get(external_reconciliation::FIELD).map(digest),
        "phase":"dispatch_armed","armedAt":now(),"providerAttemptObserved":false});
    row_mut(d, "operations", required(original, "id")?)?[PERMIT_FIELD] = permit.clone();
    Ok(permit)
}

/// This type is constructed by the ROOT-owned transport observer, never parsed
/// from HTTP. A timeout/dropped Future/elapsed deadline is not a witness.
#[derive(Clone, Debug)]
pub(crate) struct TransportCessation {
    operation_id: String, attempt_id: String, permit_id: String,
    owner: Value, evidence_sha256: String,
    kind: CessationKind,
}
impl TransportCessation {
    pub(crate) fn from_predecessor(op:&Value,verified:crate::predecessor_recovery::VerifiedPredecessorCessation)->ApiResult<Self> {
        let (row,evidence_sha256)=verified.into_parts();let identity=&op[PERMIT_FIELD];
        if !valid_permit(op)||identity["phase"]!="dispatch_armed"||row["operationId"]!=op["id"]
            ||row["attemptId"]!=op["attemptId"]||row["permitId"]!=identity["id"]||row["owner"]!=identity["owner"]
            ||row["operationSha256"]!=digest(op)||row["permitSha256"]!=digest(identity)||!hash(&json!(evidence_sha256)){return Err(denied());}
        Ok(Self{operation_id:required(op,"id")?.into(),attempt_id:required(op,"attemptId")?.into(),permit_id:required(identity,"id")?.into(),owner:identity["owner"].clone(),evidence_sha256,kind:CessationKind::Contained})
    }
    /// Only the native supervisor can create the non-deserializable observation.
    /// Error strings and arbitrary result JSON cannot manufacture this witness.
    pub(crate) fn from_observation(op: &Value, identity: &Value,
        observation: provider_session::TransportObservation) -> ApiResult<Self> {
        let mut checked = op.clone(); checked[PERMIT_FIELD] = identity.clone();
        if !valid_permit(&checked) || identity["phase"] != "dispatch_armed" { return Err(denied()); }
        let (kind, evidence_sha256) = observation.into_parts();
        if !hash(&json!(evidence_sha256)) { return Err(denied()); }
        Ok(Self { operation_id: required(op, "id")?.into(), attempt_id: required(op, "attemptId")?.into(),
            permit_id: required(identity, "id")?.into(), owner: identity["owner"].clone(), evidence_sha256, kind })
    }
}

/// Only the typed offline importer calls this after all original armed permits
/// are contained. It retains an existing closing intent and never renews it.
pub(crate) fn close_after_predecessor(d:&mut Value)->ApiResult<()> {
    let gate=control(d)?.clone();if !active(d,&gate)?.is_empty(){return Err(denied());}
    if !gate["closingIntent"].is_null() {let id=required(&gate["closingIntent"],"id")?;finalize_close(d,id)?;}
    else {d[FIELD]["state"]=json!("blocked");}
    d[FIELD]["availability"]=json!({"state":"unverified","reason":"predecessor_contained"});
    d[FIELD].as_object_mut().ok_or_else(denied)?.remove("admittedContinuationProof");Ok(())
}

/// Dependency DTO only. Missing and closed gates remain explicit dependencies.
pub(crate) fn continuation_gate_observation(d:&Value)->ApiResult<Value> {
    let Some(_)=d.get(FIELD) else{return Ok(json!({"kind":"missing"}));};
    let gate=control(d)?;let parsed=runtime_lifecycle::parse_token(&gate["owner"])?;
    if parsed.account!=accounts::Profile::from_workspace(d)?.display()||gate["owner"]!=d["runtimeLifecycle"]["owner"] {return Err(denied());}
    if !matches!(gate["availability"]["state"].as_str(),Some("ready"|"blocked"|"recovering"|"needs_owner"|"unverified")){return Err(denied());}
    Ok(json!({"kind":"present","state":gate["state"],"availabilityState":gate["availability"]["state"],"gateEpoch":gate["gateEpoch"],"owner":gate["owner"],"connectionBinding":gate["connectionBinding"]}))
}
/// A proof is useful only while every committed projection still matches it.
/// Local transport hold is an additional App-level check owned by the caller.
pub(crate) fn current_continuation_admission(d:&Value)->ApiResult<Option<Value>> {
    continuation_gate_observation(d)?;let Some(gate)=d.get(FIELD) else{return Ok(None);};
    let Some(proof)=gate.get("admittedContinuationProof") else{return Ok(None);};
    let fields=["version","kind","caseSha256","reopenReceiptSha256","gateEpoch","owner","connectionBinding","storageGeneration","protectedGeneration","protectedReceiptSha256","availabilitySha256","archiveFenceSha256","lifecycleReceiptSha256","archiveRestoreReceiptSha256"];
    if !proof.as_object().is_some_and(|o|o.len()==fields.len()&&fields.iter().all(|f|o.contains_key(*f)))
        ||proof["version"]!=1||proof["kind"]!="verified-connection-continuation-admission" {return Err(denied());}
    runtime_lifecycle::parse_token(&proof["owner"])?;let binding=ConnectorBinding::from_json(&proof["connectionBinding"]).map_err(|_|denied())?;
    if binding.to_json()!=proof["connectionBinding"] {return Err(denied());}
    for key in ["caseSha256","reopenReceiptSha256","protectedReceiptSha256","availabilitySha256","archiveFenceSha256","lifecycleReceiptSha256","archiveRestoreReceiptSha256"] {if !hash(&proof[key]){return Err(denied());}}
    if proof["gateEpoch"].as_u64().is_none_or(|n|n==0)||proof["protectedGeneration"].as_u64().is_none_or(|n|n==0){return Err(denied());}
    if gate["state"]!="open"||gate["availability"]["state"]!="ready"||gate["owner"]!=owner(d)?
        ||proof["owner"]!=gate["owner"]||proof["connectionBinding"]!=gate["connectionBinding"]||proof["gateEpoch"]!=gate["gateEpoch"]
        ||proof["storageGeneration"]!=d["storageGeneration"]||Some(crate::working_generation::current(d)?)!=proof["storageGeneration"].as_str()
        ||proof["protectedGeneration"]!=gate["availability"]["generation"]||proof["protectedReceiptSha256"]!=gate["availability"]["receiptSha256"]
        ||proof["availabilitySha256"]!=digest(&gate["availability"])||proof["archiveFenceSha256"]!=digest(&d[external_reconciliation::FIELD])
        ||proof["reopenReceiptSha256"]!=gate["reopenReceiptSha256"]{return Ok(None);}
    Ok(Some(proof.clone()))
}
#[derive(Clone, Copy, Debug)]
pub(crate) enum CessationKind { Returned, Contained, ProvenUnsent }

/// Release the OLD conductor/connector guard before acquiring M for this call.
/// Retirement retains the original dispatching/UNKNOWN recipient fence until
/// its own outcome transaction commits; cessation is not publication success.
pub(crate) fn settle(d: &mut Value, witness: &TransportCessation) -> ApiResult<Value> {
    control(d)?;
    if !hash(&json!(witness.evidence_sha256)) { return Err(denied()); }
    let op = row_mut(d, "operations", &witness.operation_id)?;
    let permit = &op[PERMIT_FIELD];
    if !valid_permit(op) || permit["id"] != witness.permit_id
        || permit["attemptId"] != witness.attempt_id || op["attemptId"] != witness.attempt_id
        || permit["operationId"] != witness.operation_id || permit["owner"] != witness.owner
        || !matches!(permit["phase"].as_str(), Some("dispatch_armed" | "transport_settled")) {
        return Err(conflict("Transport cessation does not match the original durable permit"));
    }
    let kind = match witness.kind { CessationKind::Returned => "returned", CessationKind::Contained => "contained", CessationKind::ProvenUnsent => "proven_unsent" };
    let receipt = json!({"kind":kind,"evidenceSha256":witness.evidence_sha256,"owner":witness.owner,
        "operationId":witness.operation_id,"attemptId":witness.attempt_id,"permitId":witness.permit_id});
    if permit["phase"] == "transport_settled" {
        if permit["cessation"] != receipt { return Err(conflict("Transport cessation evidence changed")); }
        return Ok(permit.clone());
    }
    op[PERMIT_FIELD]["phase"] = json!("transport_settled");
    op[PERMIT_FIELD]["cessation"] = receipt;
    op[PERMIT_FIELD]["settledAt"] = json!(now());
    Ok(op[PERMIT_FIELD].clone())
}

pub(crate) fn request_close(d: &mut Value, intent_id: &str, reason: &str) -> ApiResult<Value> {
    if intent_id.is_empty() || intent_id.len() > 160 || reason.is_empty() || reason.len() > 160 { return Err(bad("Invalid connection closing intent")); }
    let gate = control(d)?.clone();
    if gate["state"] == "closing" || gate["state"] == "blocked" && !gate["closingIntent"].is_null() {
        if gate["closingIntent"]["id"] != intent_id || gate["closingIntent"]["reason"] != reason {
            return Err(conflict("Another or changed connection closing intent exists"));
        }
        return Ok(gate["closingIntent"].clone());
    }
    let cohort: Vec<Value> = active(d, &gate)?.iter().map(|op| op[PERMIT_FIELD].clone()).collect();
    if cohort.len() > gate["mutationPermitCap"].as_u64().unwrap() as usize { return Err(denied()); }
    let intent = json!({"id":intent_id,"reason":reason,"owner":gate["owner"],"gateEpoch":gate["gateEpoch"],
        "cohortSha256":digest(&json!(cohort)),"cohortCount":cohort.len(),"requestedAt":now(),
        "drainDeadlineMs":gate["drainDeadlineMs"]});
    d[FIELD]["state"] = json!("closing"); d[FIELD]["closingIntent"] = intent.clone();
    d[FIELD]["cohort"] = json!(cohort);
    let mut availability = gate["availability"].clone();
    availability["state"] = json!("blocked"); availability["reason"] = json!(reason);
    d[FIELD]["availability"] = availability;
    Ok(intent)
}

pub(crate) fn finalize_close(d: &mut Value, intent_id: &str) -> ApiResult<Value> {
    let gate = control(d)?.clone();
    if gate["closingIntent"]["id"] != intent_id { return Err(denied()); }
    if !matches!(gate["state"].as_str(),Some("closing"|"blocked")) || !active(d, &gate)?.is_empty() { return Err(conflict("Connection still has provider-capable dispatch permits")); }
    for captured in gate["cohort"].as_array().unwrap() {
        let op = row(d, "operations", required(captured, "operationId")?)?;
        if PERMIT_IDENTITY_FIELDS
            .iter().any(|field|op[PERMIT_FIELD][*field]!=captured[*field])
            || op[PERMIT_FIELD]["phase"] != "transport_settled" { return Err(denied()); }
    }
    if gate["state"] == "blocked" {
        let receipt=&gate["finalReceipt"];
        if receipt.as_object().is_none_or(|fields|fields.len()!=8)
            ||receipt["version"]!=1||receipt["intentId"]!=intent_id||receipt["owner"]!=gate["owner"]
            ||receipt["gateEpoch"]!=gate["gateEpoch"]||receipt["providerCapablePermits"]!=0
            ||receipt["cohortCount"]!=gate["closingIntent"]["cohortCount"]||receipt["cohortSha256"]!=gate["closingIntent"]["cohortSha256"]
            ||receipt["settledAt"].as_str().is_none_or(|at|chrono::DateTime::parse_from_rfc3339(at).is_err()) {return Err(denied());}
        return Ok(receipt.clone());
    }
    let epoch = gate["gateEpoch"].as_u64().unwrap().checked_add(1).ok_or_else(denied)?;
    let receipt = json!({"version":1,"intentId":intent_id,"owner":gate["owner"],"gateEpoch":epoch,
        "cohortCount":gate["closingIntent"]["cohortCount"],"cohortSha256":gate["closingIntent"]["cohortSha256"],
        "providerCapablePermits":0,"settledAt":now()});
    d[FIELD]["gateEpoch"] = json!(epoch); d[FIELD]["state"] = json!("blocked");
    d[FIELD]["finalReceipt"] = receipt.clone(); Ok(receipt)
}

/// Admission is generated by the root from verified protected auth/lifecycle
/// and archive receipts, not a user supplied ready flag or empty new database.
pub(crate) fn reopen(d: &mut Value, admission: &Value) -> ApiResult<Value> {
    let gate = control(d)?.clone();
    if gate["state"] != "blocked" || admission["expectedGateEpoch"] != gate["gateEpoch"]
        || admission["owner"] != owner(d)? || admission["connectionBinding"] != gate["connectionBinding"]
        || admission["availability"]["state"] != "ready" || !hash(&admission["availability"]["receiptSha256"])
        || admission["availability"]["generation"].as_u64().is_none()
        || !hash(&admission["lifecycleReceiptSha256"]) || !active(d, &gate)?.is_empty() { return Err(denied()); }
    if gate["archiveFenceRequired"] == true { external_reconciliation::validate(d)?; }
    let epoch = gate["gateEpoch"].as_u64().unwrap().checked_add(1).ok_or_else(denied)?;
    d[FIELD]["owner"] = admission["owner"].clone(); d[FIELD]["gateEpoch"] = json!(epoch);
    d[FIELD]["availability"] = admission["availability"].clone(); d[FIELD]["state"] = json!("open");
    d[FIELD]["reopenReceiptSha256"] = json!(digest(admission));
    d[FIELD]["closingIntent"] = Value::Null; d[FIELD]["cohort"] = json!([]); Ok(d[FIELD].clone())
}

/// Crash/startup closes new admission before any bridge worker can send. Old
/// pre-arms stay active for containment/reconciliation, even before enqueue.
pub(crate) fn restart_closed(d: &mut Value) -> ApiResult<()> {
    control(d)?;
    if d[FIELD]["state"] != "closing" { d[FIELD]["state"] = json!("blocked"); }
    d[FIELD]["availability"] = json!({"state":"unverified","reason":"restart_requires_admission"});
    Ok(())
}

/// Shared full/scoped storage validators call this for the exact rows loaded.
/// Unknown permit fields are retained; old intent can never be reset to resend.
pub(crate) fn validate_change(before:&Value,after:&Value)->ApiResult<()> {
    if after.get(FIELD).is_some(){let gate=control(after)?;active(after,gate)?;}
    if before.get(FIELD).is_some() {
        let previous=control(before)?;let next=control(after)?;
        if next["gateEpoch"].as_u64()<previous["gateEpoch"].as_u64()
            ||["version","account","connectionBinding","bootstrapReceiptSha256","mutationPermitCap","drainDeadlineMs","archiveFenceRequired"]
                .iter().any(|field|previous[*field]!=next[*field]) {
            return Err(conflict("Connection gate identity, finite budget or epoch changed outside its admission"));
        }
    }
    for original in list(before,"operations").iter().filter(|op|op.get(PERMIT_FIELD).is_some()) {
        let changed=row(after,"operations",required(original,"id")?)?;
        let previous=&original[PERMIT_FIELD];let next=&changed[PERMIT_FIELD];
        if PERMIT_IDENTITY_FIELDS
            .iter().any(|field|previous[*field]!=next[*field])
            ||!matches!((previous["phase"].as_str(),next["phase"].as_str()),
                (Some("dispatch_armed"),Some("dispatch_armed"|"transport_settled"))|(Some("transport_settled"),Some("transport_settled")))
            ||previous["phase"]=="transport_settled"&&previous!=next {
            return Err(conflict("Original dispatch permit identity/cessation cannot be deleted, reset or retargeted"));
        }
    }
    Ok(())
}

/// Bounded native coordinator. SQL/M are held only for reducer commits, never
/// while waiting for provider cessation. Expiration leaves closing durable.
pub(crate) async fn close_and_drain(app:&App,intent_id:&str,reason:&str)->ApiResult<Value> {
    let maximum_deadline=tokio::time::Instant::now()+std::time::Duration::from_millis(MAX_DRAIN_MS);
    // This bounded metadata read obtains an already-issued closing deadline
    // before waiting for M. Restart/re-entry cannot extend the original budget.
    let metadata=tokio::time::timeout_at(maximum_deadline,app.db.read_metadata()).await
        .map_err(|_|conflict("Connection closing metadata deadline expired; dispatch intent remains uncertain"))??;
    let gate=control(&metadata)?;
    let budget=gate["drainDeadlineMs"].as_u64().unwrap();
    let initial_deadline=if gate["closingIntent"].is_object() {
        drain_deadline(&gate["closingIntent"])?
    } else {tokio::time::Instant::now()+std::time::Duration::from_millis(budget)};
    let intent={
        tokio::time::timeout_at(initial_deadline,async {
            let _company=lock(app).await;
            app.change_connection_gate(Scope::Control,|d|request_close(d,intent_id,reason)).await
        }).await.map_err(|_|conflict("Connection closing admission deadline expired; dispatch intent remains uncertain"))??
    };
    let deadline=drain_deadline(&intent)?.min(initial_deadline);
    let mut updates=app.events.subscribe();
    loop {
        let result=tokio::time::timeout_at(deadline,async {
            let _company=lock(app).await;
            app.change_connection_gate(Scope::Control,|d| {
                let gate=control(d)?;
                if gate["closingIntent"]["id"]!=intent_id{return Err(denied());}
                if !active(d,gate)?.is_empty(){return Ok(None);}
                finalize_close(d,intent_id).map(Some)
            }).await
        }).await.map_err(|_|conflict("Connection drain deadline expired; closing and original uncertain effects remain held"))??;
        if let Some(receipt)=result{return Ok(receipt);}
        tokio::select! {
            _=tokio::time::sleep_until(deadline)=>return Err(conflict("Connection drain deadline expired; closing and original uncertain effects remain held")),
            _=updates.recv()=>{},
        }
    }
}
fn drain_deadline(intent:&Value)->ApiResult<tokio::time::Instant> {
    let requested=chrono::DateTime::parse_from_rfc3339(required(intent,"requestedAt")?)
        .map_err(|_|denied())?;
    let budget=intent["drainDeadlineMs"].as_u64().filter(|ms|*ms>0&&*ms<=MAX_DRAIN_MS).ok_or_else(denied)?;
    let elapsed=chrono::Utc::now().signed_duration_since(requested.with_timezone(&chrono::Utc)).num_milliseconds().max(0) as u64;
    Ok(tokio::time::Instant::now()+std::time::Duration::from_millis(budget.saturating_sub(elapsed)))
}

#[cfg(test)]
pub(crate) fn fixture_open(d:&mut Value)->ApiResult<()> {
    let receipt="a".repeat(64);
    if d.get(FIELD).is_none(){initialize_closed(d,&receipt,MAX_MUTATION_PERMITS,1000,false)?;}
    if d[FIELD]["state"]=="open"{return Ok(());}
    reopen(d,&json!({"expectedGateEpoch":d[FIELD]["gateEpoch"],"owner":d["runtimeLifecycle"]["owner"],
        "connectionBinding":active_binding(d)?.to_json(),"availability":{"state":"ready","generation":1,"receiptSha256":receipt},
        "lifecycleReceiptSha256":receipt})).map(|_|())
}

#[cfg(test)]
#[path = "connection_gate_tests.rs"]
pub(crate) mod tests;
