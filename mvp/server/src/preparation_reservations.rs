//! Durable preparation ownership on the existing job/proposal/operation ledger.
//! Pure guards: callers capture and persist `scopeReservation` in the same writer
//! transaction as scheduling, before any model dispatch. No timers or terminal
//! preparation status release paid/uncertain work. Complete ledger projections
//! are required; an omitted reservation cannot be detected by this module.
use crate::{ApiResult, ConnectorBinding, ResourceRef, conflict};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;

fn rows<'a>(d: &'a Value, key: &str) -> &'a [Value] {
    d[key].as_array().map(Vec::as_slice).unwrap_or(&[])
}
fn proposals(d: &Value) -> impl Iterator<Item = &Value> {
    let mut seen = BTreeSet::new();
    rows(d, "proposals")
        .iter()
        .chain(rows(d, "scopeProposals"))
        .filter(move |p| p["id"].as_str().is_none_or(|id| seen.insert(id)))
}
fn string<'a>(v: &'a Value, key: &str) -> ApiResult<&'a str> {
    v[key]
        .as_str()
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| conflict("Preparation reservation identity is missing"))
}
fn hash(v: &Value) -> String {
    format!("{:x}", Sha256::digest(v.to_string().as_bytes()))
}
fn scope(binding: &ConnectorBinding) -> Value {
    // Revision and connection id do not make an unresolved provider recipient
    // new. Actual provider account and connector namespaces remain separate.
    json!([
        binding.workspace_id,
        binding.account_id,
        binding.connector.as_str(),
        binding.provider_account_id
    ])
}
fn key(parts: Value) -> String {
    parts.to_string()
}

// Share the canonical recipient aliases with bounded readiness consumers.
// The active company/connector binding and existing key policy remain authority.
pub(crate) fn recipient_keys(d: &Value, item: &Value) -> ApiResult<BTreeSet<String>> {
    item_keys(&crate::active_binding(d)?, item)
}

fn item_keys(binding: &ConnectorBinding, item: &Value) -> ApiResult<BTreeSet<String>> {
    for field in ["account", "accountId"] {
        if item
            .get(field)
            .is_some_and(|v| v.as_str() != Some(binding.account_id.as_str()))
        {
            return Err(conflict("Preparation reservation company changed"));
        }
    }
    if let Some(saved) = item.get("connectorBinding") {
        if ConnectorBinding::from_json(saved).map_err(|e| conflict(e.0))? != *binding {
            return Err(conflict(
                "Preparation reservation recipient binding changed",
            ));
        }
    }
    let local = json!([binding.workspace_id, binding.account_id]);
    let mut keys = BTreeSet::from([key(json!(["item", local, string(item, "id")?]))]);
    if let Some(object) = item["objectId"].as_str().filter(|v| !v.is_empty()) {
        if let Some(recipient) = item["itemId"].as_str().filter(|v| !v.is_empty()) {
            keys.insert(key(json!(["recipient", scope(binding), object, recipient])));
        }
        if let Some(conversation) = item["conversationKey"].as_str().filter(|v| !v.is_empty()) {
            keys.insert(key(json!([
                "conversation",
                scope(binding),
                object,
                conversation
            ])));
        }
    }
    if let Some(branch) = item["branchId"].as_str().filter(|s| !s.is_empty()) {
        keys.insert(key(json!(["branch", local, branch])));
    }
    Ok(keys)
}

fn captured(d: &Value, job: &Value) -> ApiResult<Value> {
    captured_control(d, job, true)
}
fn captured_control(d: &Value, job: &Value, verify_request: bool) -> ApiResult<Value> {
    let current_binding = crate::active_binding(d)?;
    let bundle = &job["prepareBundle"];
    if bundle["version"] != 1
        || (verify_request && bundle["digest"] != hash(&bundle["request"]))
        || bundle["request"]["account"] != current_binding.account_id
    {
        return Err(conflict("Preparation reservation capture is invalid"));
    }
    let binding = if bundle["request"]["connectorBinding"].is_null()
        && current_binding.to_json() == crate::legacy_binding()
    {
        current_binding.clone()
    } else {
        ConnectorBinding::from_json(&bundle["request"]["connectorBinding"])
            .map_err(|e| conflict(e.0))?
    };
    binding
        .validate_scope(&current_binding.workspace_id, &current_binding.account_id)
        .map_err(|e| conflict(e.0))?;
    let ids = bundle["itemIds"]
        .as_array()
        .filter(|v| !v.is_empty() && v.len() <= 100)
        .ok_or_else(|| conflict("Preparation reservation recipients are missing"))?;
    let mut seen = BTreeSet::new();
    let mut keys = BTreeSet::new();
    let mut recipients = Vec::new();
    for id in ids {
        let id = id
            .as_str()
            .filter(|s| !s.is_empty())
            .ok_or_else(|| conflict("Preparation reservation recipient is invalid"))?;
        if !seen.insert(id) {
            return Err(conflict(
                "Preparation reservation recipients are duplicated",
            ));
        }
        let matches: Vec<_> = rows(&bundle["request"], "items")
            .iter()
            .filter(|i| i["id"] == id)
            .collect();
        if matches.len() != 1 {
            return Err(conflict(
                "Preparation reservation saved recipient is missing or duplicated",
            ));
        }
        let item = matches[0];
        let recipient_keys = item_keys(&binding, item)?;
        keys.extend(recipient_keys.iter().cloned());
        recipients.push(json!({"itemId":id,"keys":recipient_keys}));
    }
    if rows(&bundle["request"], "items").len() != ids.len() {
        return Err(conflict(
            "Preparation reservation has foreign saved recipients",
        ));
    }
    recipients.sort_by(|a, b| a["itemId"].as_str().cmp(&b["itemId"].as_str()));
    let keys = json!(keys);
    Ok(
        json!({"version":1,"ownerJobId":string(job,"id")?,"account":binding.account_id,
        "connectorBinding":binding.to_json(),"prepareBundleId":string(bundle,"id")?,
        "prepareBundleDigest":bundle["digest"],"keysDigest":hash(&keys),"keys":keys,"recipients":recipients}),
    )
}

/// Return the immutable saved capture on replay; never refresh from live items.
pub(crate) fn capture(d: &Value, job_id: &str) -> ApiResult<Value> {
    let job = match crate::row(d, "jobs", job_id) {
        Ok(job) => job,
        Err(error) => {
            return rows(d, "scopeOwners")
                .iter()
                .find(|j| j["id"] == job_id)
                .map(|job| {
                    // Full native repair controls preserve frozen request bytes.
                    // Do not route them through legacy_reservation -> capture.
                    if job["scopeOwnerProjection"]["version"] != 1 && job["prepareBundle"].is_object() {
                        let expected = captured(d, job)?;
                        if let Some(saved) = job.get("scopeReservation") {
                            if saved != &expected {
                                return Err(conflict("Preparation scope reservation binding changed"));
                            }
                            return Ok(saved.clone());
                        }
                        return Ok(expected);
                    }
                    legacy_reservation(d, job)?
                        .ok_or_else(|| conflict("Preparation reservation capture is missing"))
                })
                .unwrap_or(Err(error));
        }
    };
    let expected = captured(d, job)?;
    if let Some(saved) = job.get("scopeReservation") {
        if saved != &expected {
            return Err(conflict("Preparation scope reservation binding changed"));
        }
        return Ok(saved.clone());
    }
    Ok(expected)
}

fn keys(reservation: &Value) -> ApiResult<BTreeSet<String>> {
    let values = reservation["keys"]
        .as_array()
        .filter(|v| !v.is_empty())
        .ok_or_else(|| conflict("Preparation scope reservation keys are missing"))?;
    let mut result = BTreeSet::new();
    for value in values {
        let value = value
            .as_str()
            .ok_or_else(|| conflict("Preparation scope reservation key is invalid"))?;
        if !result.insert(value.to_owned()) {
            return Err(conflict("Preparation scope reservation key is duplicated"));
        }
    }
    if reservation["version"] != 1 || reservation["keysDigest"] != hash(&json!(result)) {
        return Err(conflict("Preparation scope reservation keys changed"));
    }
    Ok(result)
}

fn proposal_owner(proposal: &Value) -> Option<&str> {
    proposal["prepareRunId"]
        .as_str()
        .or_else(|| proposal["origin"]["prepareRunId"].as_str())
        .or_else(|| proposal["recovery"]["prepareRunId"].as_str())
}
fn paid_lineage(proposal: &Value) -> bool {
    proposal_owner(proposal).is_some()
        || proposal["paidGeneration"] == true
        || !proposal["prepareBundleId"].is_null()
        || !proposal["generationMetadata"].is_null()
}
fn same_proposal_control(supplied: &Value, stored: &Value) -> bool {
    if supplied["id"].as_str().is_none_or(|id| id.is_empty())
        || supplied["id"] != stored["id"]
        || supplied["itemId"] != stored["itemId"]
        || supplied["status"] != stored["status"]
        || proposal_owner(supplied) != proposal_owner(stored)
        || paid_lineage(supplied) != paid_lineage(stored)
    {
        return false;
    }
    for field in ["revision", "kind", "prepareBundleId", "prepareBundleDigest"] {
        if !stored[field].is_null() && supplied[field] != stored[field] {
            return false;
        }
    }
    for field in [
        "id",
        "branchId",
        "objectId",
        "itemId",
        "postKey",
        "conversationKey",
        "connectorBinding",
        "account",
        "accountId",
    ] {
        if supplied["routeTarget"][field] != stored["routeTarget"][field] {
            return false;
        }
    }
    true
}
fn explicit_foreign(d: &Value, record: &Value) -> bool {
    [
        record.get("account"),
        record.get("accountId"),
        record["connectorBinding"].get("accountId"),
    ]
    .into_iter()
    .flatten()
    .any(|v| v.as_str().is_some_and(|a| Some(a) != d["account"].as_str()))
}
fn record_keys(d: &Value, record: &Value, target_field: &str) -> ApiResult<BTreeSet<String>> {
    let binding = crate::active_binding(d)?;
    let target = &record[target_field];
    if explicit_foreign(d, record) || explicit_foreign(d, target) {
        return Ok(BTreeSet::new());
    }
    let mut result = BTreeSet::new();
    if let Some(id) = record["itemId"].as_str().filter(|v| !v.is_empty()) {
        result.insert(key(json!([
            "item",
            [binding.workspace_id, binding.account_id],
            id
        ])));
        // Current aliases supplement, never replace, the immutable target.
        if let Some(item) = rows(d, "items").iter().find(|i| i["id"] == id) {
            result.extend(item_keys(&binding, item)?);
        }
    }
    if target.is_object() {
        // Legacy missing binding belongs only to this same-company ledger.
        let saved = target
            .get("connectorBinding")
            .map(ConnectorBinding::from_json)
            .transpose()
            .map_err(|e| conflict(e.0))?
            .unwrap_or(binding);
        if saved.account_id != d["account"].as_str().unwrap_or("") {
            return Ok(BTreeSet::new());
        }
        if let (Some(object), Some(item)) = (target["objectId"].as_str(), target["itemId"].as_str())
        {
            result.insert(key(json!(["recipient", scope(&saved), object, item])));
        }
        if let (Some(object), Some(conversation)) = (
            target["objectId"].as_str(),
            target["conversationKey"].as_str(),
        ) {
            result.insert(key(json!([
                "conversation",
                scope(&saved),
                object,
                conversation
            ])));
        }
        if let Some(branch) = target["branchId"].as_str() {
            result.insert(key(json!([
                "branch",
                [saved.workspace_id, saved.account_id],
                branch
            ])));
        }
    }
    Ok(result)
}
fn overlaps(a: &BTreeSet<String>, b: &BTreeSet<String>) -> bool {
    !a.is_disjoint(b)
}
fn known_operation(operation: &Value) -> bool {
    matches!(
        operation["status"].as_str(),
        Some("succeeded" | "failed" | "stale")
    ) && operation["evidence"]["requiresReadback"] != true
}

/// Repair markers retain their complete native authority. Neither terminal
/// legacy omission nor a Boolean compact control can discharge a paid round.
pub(crate) fn repair_owned(job: &Value) -> bool {
    job["purpose"] == "answering_repair"
        || !job["originatingAnsweringAttemptId"].is_null()
        || !job["repairPaidIntent"].is_null()
        || !job["answeringRepairPlan"].is_null()
        || !job["preparationStages"]["repairBudget"].is_null()
        || !job["preparationStages"]["answeringRepairs"].is_null()
        || !job["repairMergeReceipt"].is_null()
}
fn settled_attention(job: &Value, id: &str) -> bool {
    if job["status"] != "completed" {
        return false;
    }
    if job["purpose"]=="engine_prepare"{
        return if job["scopeOwnerProjection"]["version"]==1{
            rows(&job["scopeOwnerProjection"],"engineTerminalItemIds").contains(&json!(id))
        }else{crate::engine_prepare::terminal_no_candidate_member(job,id)};
    }
    if job["scopeOwnerProjection"]["attentionItemIds"]
        .as_array()
        .is_some_and(|ids| ids.iter().any(|v| v == id))
    {
        return true;
    }
    rows(&job["preparationStages"], "groupAdmission")
        .iter()
        .any(|g| {
            g["status"] == "admitted"
                && rows(g, "itemIds").iter().any(|v| v == id)
                && rows(&g["admission"], "finalAssessments")
                    .iter()
                    .any(|a| a["itemId"] == id && a["outcome"] == "needs_attention")
                && !rows(&g["admission"], "candidates")
                    .iter()
                    .any(|p| p["itemId"] == id)
        })
        || (job["prepareOutcome"]["status"] == "needs_attention" && job["refId"] == id)
}

// auto_prepare's pre-grouped admission stores its authoritative per-recipient
// disposition in prepareOutcome, rather than groupAdmission.finalAssessments.
// A completed no-candidate disposition is not an outstanding paid draft.
fn auto_terminal_item(job: &Value, id: &str) -> bool {
    if job["purpose"] != "auto_prepare" || job["status"] != "completed" {
        return false;
    }
    if job["scopeOwnerProjection"]["autoTerminalItemIds"]
        .as_array().is_some_and(|ids| ids.iter().any(|v| v == id)) {
        return true;
    }
    let outcome = &job["prepareOutcome"];
    let matching: Vec<_> = if outcome["items"].is_array() {
        rows(outcome, "items").iter().filter(|row| row["itemId"] == id).collect()
    } else if outcome["itemId"] == id {
        vec![outcome]
    } else {
        vec![]
    };
    matching.len() == 1
        && (matching[0]["status"] == "needs_attention"
            || (matching[0]["status"] == "stale"
                && matching[0]["reason"] == "Context changed during automatic preparation"))
        && matching[0]["proposalId"].is_null()
        && !rows(&outcome["admission"], "candidates").iter().any(|p| p["itemId"] == id)
}

fn settled_auto_scope(d: &Value, job: &Value, reservation: &Value, wanted: &BTreeSet<String>) -> ApiResult<bool> {
    if job["purpose"] != "auto_prepare" || job["status"] != "completed" || unfinished_model(job) {
        return Ok(false);
    }
    let owner = string(job, "id")?;
    let mut matched = false;
    for recipient in rows(reservation, "recipients") {
        let recipient_keys = rows(recipient, "keys").iter().filter_map(Value::as_str).map(str::to_owned).collect();
        if !overlaps(wanted, &recipient_keys) { continue; }
        matched = true;
        let id = string(recipient, "itemId")?;
        // Shared branches/conversations still fence every overlapping sibling.
        // Canonical paid proposals and uncertain operations remain authoritative.
        match repair_item_settlement(d, job, recipient)? {
            Some(true) => continue,
            Some(false) => return Ok(false),
            None => (),
        }
        if !auto_terminal_item(job, id)
            || proposals(d).any(|p| proposal_owner(p) == Some(owner) && p["itemId"] == id) {
            return Ok(false);
        }
    }
    for op in rows(d, "operations") {
        if !known_operation(op) && overlaps(wanted, &record_keys(d, op, "target")?) {
            return Ok(false);
        }
    }
    Ok(matched)
}

fn settled_engine_scope(d:&Value,job:&Value,reservation:&Value,wanted:&BTreeSet<String>)->ApiResult<bool>{
    if job["purpose"]!="engine_prepare"||job["status"]!="completed"||unfinished_model(job){return Ok(false);}
    let selected=rows(reservation,"recipients").iter().filter(|recipient|{
        let saved=rows(recipient,"keys").iter().filter_map(Value::as_str).map(str::to_owned).collect();
        overlaps(wanted,&saved)
    }).cloned().collect::<Vec<_>>();
    if selected.is_empty(){return Ok(false);}
    for recipient in &selected{
        let id=string(recipient,"itemId")?;
        let terminal=if job["scopeOwnerProjection"]["version"]==1{
            rows(&job["scopeOwnerProjection"],"engineTerminalItemIds").contains(&json!(id))
        }else{crate::engine_prepare::terminal_no_candidate_member(job,id)};
        if !terminal||proposals(d).any(|p|proposal_owner(p)==job["id"].as_str()&&p["itemId"]==id){return Ok(false);}
    }
    if rows(d,"operations").iter().any(|op|!known_operation(op)&&record_keys(d,op,"target").map_or(true,|keys|overlaps(wanted,&keys))){return Ok(false);}
    Ok(true)
}

fn safe_unpaid_cancel(d: &Value, job: &Value) -> bool {
    if repair_owned(job) { return false; }
    if job["scopeOwnerProjection"]["version"] == 1 {
        return job["scopeOwnerProjection"]["cancelledUnpaid"] == true
            && !proposals(d).any(|p| proposal_owner(p) == job["id"].as_str());
    }
    // This explicit trusted witness may only be saved before dispatch has ever
    // started. A missing first checkpoint alone is NOT no-spending evidence.
    job["status"] == "cancelled"
        && job["scopeCancellation"]
            == json!({"version":1,"modelDispatchPrevented":true,"noResult":true})
        && job["result"].is_null()
        && job["preparationStages"]["first"].is_null()
        && job["preparationStages"]["firstAdmission"].is_null()
        && job["preparationStages"]["review"].is_null()
        && job["preparationStages"]["reviewChunks"].is_null()
        && job["scopeModelAttempt"].is_null()
        && !proposals(d).any(|p| proposal_owner(p) == job["id"].as_str())
        && !rows(d, "operations")
            .iter()
            .any(|op| op["prepareRunId"] == job["id"])
}

fn has_retained_checkpoint(job: &Value) -> bool {
    job["scopeOwnerProjection"]["retainedOutput"] == true
        || !job["result"].is_null()
        || !job["preparationStages"]["first"].is_null()
        || !job["preparationStages"]["review"].is_null()
        || !job["preparationStages"]["reviewChunks"].is_null()
        || !job["scopeModelAttempt"].is_null()
        || !job["repairPaidIntent"].is_null()
        || !rows(job, "modelMaterialReceipts").is_empty()
        || !rows(job, "retainedEvidence").is_empty()
        || !job["recovery"].is_null()
        || rows(&job["preparationStages"], "groupAdmission")
            .iter()
            .any(|g| !g["admission"].is_null())
}
fn has_retained_output(d: &Value, job: &Value) -> bool {
    has_retained_checkpoint(job)
        || proposals(d).any(|p| proposal_owner(p) == job["id"].as_str())
        || rows(d, "operations")
            .iter()
            .any(|op| op["prepareRunId"] == job["id"])
}
fn saved_reservation(job: &Value) -> bool {
    if job["scopeOwnerProjection"]["version"] == 1 {
        job["scopeOwnerProjection"]["reservationWasSaved"] == true
    } else {
        !job["scopeReservation"].is_null()
    }
}
fn terminal_unproven_legacy(_d: &Value, job: &Value) -> bool {
    // Owner decision: do not retrofit durable reservations onto terminal legacy
    // jobs. Their actual pending generated proposals and UNKNOWN operations are
    // still checked independently. Interrupted jobs remain uncertain owners.
    !repair_owned(job)
        && !saved_reservation(job)
        && matches!(
            job["status"].as_str(),
            Some("completed" | "failed" | "cancelled")
        )
        && !unfinished_model(job)
}
fn first_admission_pending(job:&Value)->bool {
    !job["preparationStages"]["firstAdmission"].is_null()
        && !(job["preparationStages"]["first"]["status"]=="completed"
            && job["preparationStages"]["first"]["result"].is_object())
}
fn first_admission_bound(job:&Value)->bool {
    let first=&job["preparationStages"]["firstAdmission"];
    let initial=&job["preparationStages"]["initialAdmission"];
    first.as_object().is_some_and(|o|o.len()==5&&["version","status","requestSha256","owner","reservedAt"].iter().all(|k|o.contains_key(*k)))
        &&first["version"]==1&&first["status"]=="reserved"&&first["reservedAt"].is_string()
        &&first["requestSha256"].as_str().is_some_and(|s|s.len()==64&&s.bytes().all(|b|b.is_ascii_digit()||matches!(b,b'a'..=b'f')))
        &&first["requestSha256"]==job["prepareBundle"]["digest"]
        &&first["requestSha256"]==job["scopeReservation"]["prepareBundleDigest"]
        &&initial.as_object().is_some_and(|o|o.len()==5&&["version","status","requestSha256","owner","admittedAt"].iter().all(|k|o.contains_key(*k)))
        &&initial["version"]==1&&initial["status"]=="scheduled"&&initial["admittedAt"].is_string()
        &&first["requestSha256"]==initial["requestSha256"]&&first["owner"]==initial["owner"]
        &&first["owner"].as_object().is_some_and(|o|o.len()==4&&["account","runtimeId","releaseSha256","epoch"].iter().all(|k|o.contains_key(*k)))
        &&first["owner"]["account"].is_string()&&first["owner"]["runtimeId"].is_string()
        &&first["owner"]["releaseSha256"].is_string()&&first["owner"]["epoch"].as_u64().is_some_and(|v|v>0)
}
fn first_no_dispatch_witness(job:&Value)->bool {
    let witness=&job["scopeFailure"];
    first_admission_bound(job)
        &&witness.as_object().is_some_and(|o|o.len()==6&&["version","ownerJobId","keysDigest","prepareBundleDigest","kind","category"].iter().all(|k|o.contains_key(*k)))
        &&witness==&json!({"version":1,"ownerJobId":job["id"],"keysDigest":job["scopeReservation"]["keysDigest"],
            "prepareBundleDigest":job["scopeReservation"]["prepareBundleDigest"],"kind":"known_failure_without_result","category":"ASSISTANT_BUSY"})
        &&job["scopeReservation"]["keysDigest"].is_string()
}
fn unfinished_model(job: &Value) -> bool {
    unfinished_other_model(job)||first_admission_unresolved(job)
}
fn first_admission_unresolved(job:&Value)->bool {
    job["scopeOwnerProjection"]["firstAdmissionUnresolved"]==true
        ||(first_admission_pending(job)&&!first_no_dispatch_witness(job))
}
fn unfinished_other_model(job: &Value) -> bool {
    job["scopeOwnerProjection"]["unfinishedModel"] == true
        || ((job["purpose"] == "answering_repair" || !job["repairPaidIntent"].is_null())
            && (job["repairPaidIntent"]["status"] != "completed" || job["status"] != "completed"))
        || !job["recovery"].is_null()
        || matches!(
            job["scopeModelAttempt"]["status"].as_str(),
            Some("running" | "unknown")
        )
        || ["first", "review"].iter().any(|s| {
            matches!(
                job["preparationStages"][*s]["status"].as_str(),
                Some("running" | "unknown")
            )
        })
        || rows(&job["preparationStages"]["reviewChunks"], "chunks")
            .iter()
            .any(|c| {
                rows(c, "attempts")
                    .iter()
                    .any(|a| matches!(a["status"].as_str(), Some("running" | "unknown")))
            })
}
fn no_selected_scope(d: &Value, job: &Value) -> bool {
    if repair_owned(job) { return false; }
    let empty = job["scopeOwnerProjection"]["noSelected"] == true
        || (job["selectedItemIds"].as_array().is_some_and(Vec::is_empty)
            && job["prepareBundle"].is_null()
             && job["preparationStages"]["first"].is_null()
             && job["preparationStages"]["firstAdmission"].is_null()
            && job["preparationStages"]["review"].is_null()
            && job["preparationStages"]["reviewChunks"].is_null()
            && job["scopeModelAttempt"].is_null());
    empty
        && !proposals(d).any(|p| proposal_owner(p) == job["id"].as_str())
        && !rows(d, "operations")
            .iter()
            .any(|o| o["prepareRunId"] == job["id"])
}
fn witnessed_retry_safe(d: &Value, job: &Value) -> bool {
    if job["status"] != "failed" || has_retained_output(d, job) || unfinished_model(job) {
        return false;
    }
    if job["scopeOwnerProjection"]["retrySafe"] == true {
        return true;
    }
    if let Some(witness) = job.get("scopeFailure") {
        let reservation = &job["scopeReservation"];
        return witness["version"] == 1
            && witness["ownerJobId"] == job["id"]
            && witness["keysDigest"] == reservation["keysDigest"]
            && witness["prepareBundleDigest"] == reservation["prepareBundleDigest"]
            && witness["kind"] == "known_failure_without_result"
            && witness["category"]
                .as_str()
                .is_some_and(|reason| confirmed_transient_category(reason).is_some());
    }
    let reservation = job.get("scopeReservation");
    let ids: Vec<&str> = reservation
        .map(|r| {
            rows(r, "recipients")
                .iter()
                .filter_map(|r| r["itemId"].as_str())
                .collect()
        })
        .unwrap_or_else(|| {
            job["prepareBundle"]["itemIds"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .collect()
        });
    !ids.is_empty()
        && ids.iter().all(|id| {
            rows(d, "items")
                .iter()
                .find(|i| i["id"] == *id)
                .is_some_and(|item| {
                    let state = &item["autoPreparation"];
                    state["jobId"] == job["id"]
                        && state["status"] == "error"
                        && state["attempts"].as_u64().is_some_and(|n| n > 0)
                        && state["retryAt"].is_string()
                        && state["reason"].as_str().is_some_and(|reason| {
                            confirmed_transient_category(reason).is_some()
                        })
                })
        })
}

fn confirmed_transient_category(reason: &str) -> Option<&'static str> {
    // BUSY is a confirmed admission refusal before model dispatch. A timeout
    // or process error can lose a paid response and must retain the owner.
    matches!(reason,"ASSISTANT_BUSY"|"Adapter failed (ASSISTANT_BUSY)").then_some("ASSISTANT_BUSY")
}

/// Trusted worker-failure reducer only: persist this immutable witness on the
/// failed job in the same transaction as its known failure outcome. This makes
/// safe release survive replacement of the item's mutable retry pointer. It
/// grants no new attempt budget; the normal automatic scheduler still owns it.
pub(crate) fn capture_failed_no_result(d: &Value, job_id: &str, reason: &str) -> ApiResult<Value> {
    let job = crate::row(d, "jobs", job_id)?;
    if !matches!(job["status"].as_str(), Some("running" | "failed"))
        || has_retained_output(d, job)
        || unfinished_other_model(job)
        || (first_admission_pending(job)&&!first_admission_bound(job))
        || confirmed_transient_category(reason).is_none()
    {
        return Err(conflict(
            "Preparation failure cannot release paid or uncertain work",
        ));
    }
    let reservation = capture(d, job_id)?;
    let category = confirmed_transient_category(reason).expect("checked above");
    let witness = json!({"version":1,"ownerJobId":job_id,"keysDigest":reservation["keysDigest"],
        "prepareBundleDigest":reservation["prepareBundleDigest"],"kind":"known_failure_without_result","category":category});
    if job.get("scopeFailure").is_some_and(|old| old != &witness) {
        return Err(conflict("Preparation failure release witness is immutable"));
    }
    Ok(witness)
}

fn authenticated_source_stale(d: &Value, job: &Value, proposal: &Value, id: &str) -> bool {
    if job["status"] != "completed" || unfinished_model(job) || proposal["status"] != "stale" {
        return false;
    }
    let reason = proposal["staleReason"].as_str().unwrap_or("");
    if !matches!(
        reason,
        "Review source context changed"
            | "Preparation evidence changed; prepare again"
            | "Comment context changed; create a new proposal"
            | "Recovered preparation source changed"
    ) {
        return false;
    }
    let bundle_id = job["prepareBundle"]
        .get("id")
        .unwrap_or(&job["scopeReservation"]["prepareBundleId"]);
    let bundle_digest = job["prepareBundle"]
        .get("digest")
        .unwrap_or(&job["scopeReservation"]["prepareBundleDigest"]);
    if proposal["prepareBundleId"] != *bundle_id
        || proposal["prepareBundleDigest"] != *bundle_digest
    {
        return false;
    }
    if rows(d, "items")
        .iter()
        .find(|i| i["id"] == id)
        .is_some_and(|item| {
            let state = &item["autoPreparation"];
            state["status"] == "stale"
                && state["requiresReview"] == true
                && state["jobId"] == job["id"]
                && state["savedProposalId"] == proposal["id"]
        })
    {
        return true;
    }
    if let Some(saved) = proposal["reviewContextDigest"]
        .as_str()
        .filter(|v| v.len() == 64 && v.bytes().all(|b| b.is_ascii_hexdigit()))
    {
        return crate::prepare_bundle::review_fingerprint(d, id)
            .is_ok_and(|observed| observed != saved);
    }
    reason == "Comment context changed; create a new proposal"
        && rows(d, "items")
            .iter()
            .find(|i| i["id"] == id)
            .is_some_and(|item| {
                item["revision"]
                    .as_u64()
                    .zip(proposal["itemRevision"].as_u64())
                    .is_some_and(|(current, saved)| current != saved)
            })
}
fn failed_superseded(d: &Value, job: &Value, id: &str) -> bool {
    if job["status"] != "failed" || unfinished_model(job) {
        return false;
    }
    let Some(item) = rows(d, "items").iter().find(|i| i["id"] == id) else {
        return false;
    };
    let Some(run) = item["autoRevalidation"]["restartRunId"].as_str() else {
        return false;
    };
    let bundle_id = job["prepareBundle"]
        .get("id")
        .unwrap_or(&job["scopeReservation"]["prepareBundleId"]);
    let digest = job["prepareBundle"]
        .get("digest")
        .unwrap_or(&job["scopeReservation"]["prepareBundleDigest"]);
    rows(d, "preparationRuns").iter().any(|r| {
        r["runId"] == run
            && r["applied"] == true
            && rows(r, "requestedItemIds").iter().any(|v| v == id)
            && rows(r, "itemIds").iter().any(|v| v == id)
            && ["errorRetries", "freshReanalyses"].iter().any(|field| {
                rows(r, field).iter().any(|p| {
                    p["itemId"] == id
                        && p["jobId"] == job["id"]
                        && p["bundleId"] == *bundle_id
                        && p["bundleDigest"] == *digest
                })
            })
    })
}

fn repair_child_proposals_unchanged(d: &Value, child: &Value, id: &str) -> bool {
    let outcome = &child["prepareOutcome"];
    for candidate in rows(outcome, "candidates").iter().filter(|c| c["itemId"] == id) {
        let Some(proposal) = proposals(d).find(|p| p["id"] == candidate["proposalId"]
            && p["itemId"] == id && proposal_owner(p) == child["id"].as_str()) else { return false; };
        let Some(pin) = rows(outcome, "proposalSettlementPins").iter()
            .find(|pin| pin["proposalId"] == proposal["id"]) else { return false; };
        if proposal["prepareBundleId"] != child["prepareBundle"]["id"]
            || proposal["prepareBundleDigest"] != child["prepareBundle"]["digest"]
            || pin["contentSha256"] != crate::answering_repair_plan::proposal_content_hash(proposal) {
            return false;
        }
    }
    true
}

/// A repaired recipient is discharged through its exact merged native child.
/// Other recipients in an original group retain their ordinary dispositions.
fn repair_item_settlement(d: &Value, job: &Value, recipient: &Value) -> ApiResult<Option<bool>> {
    if !repair_owned(job) || job["purpose"] == "answering_repair" {
        return Ok(None);
    }
    let id = string(recipient, "itemId")?;
    let mut affected = BTreeSet::new();
    for need in rows(job, "videoFrameNeeds") {
        if rows(need, "affectedRecipientIds").is_empty() { return Ok(Some(false)); }
        for affected_id in rows(need, "affectedRecipientIds") {
            let Some(affected_id) = affected_id.as_str() else { return Ok(Some(false)); };
            affected.insert(affected_id);
        }
    }
    if affected.is_empty() { return Ok(Some(false)); }
    if !affected.contains(id) { return Ok(None); }
    let Some(child) = crate::answering_repair_plan::merged_child(d, string(job, "id")?)? else {
        return Ok(Some(false));
    };
    if !rows(&child["answeringRepairPlan"], "affectedRecipientIds").contains(&json!(id)) {
        return Err(conflict("Repair settlement recipient changed"));
    }
    let child_scope = capture(d, string(child, "id")?)?;
    let saved = rows(&child_scope, "recipients").iter().find(|r| r["itemId"] == id)
        .ok_or_else(|| conflict("Repair settlement recipient capture missing"))?.clone();
    if saved["keys"] != recipient["keys"] {
        return Err(conflict("Repair settlement original recipient route changed"));
    }
    let mut selected = child_scope;
    selected["recipients"] = json!([saved]);
    settled(d, child, &selected).map(Some)
}

fn settled(d: &Value, job: &Value, reservation: &Value) -> ApiResult<bool> {
    if safe_unpaid_cancel(d, job) {
        return Ok(true);
    }
    if matches!(job["status"].as_str(), Some("running" | "queued")) {
        return Ok(false);
    }
    if unfinished_model(job) {
        return Ok(false);
    }
    let owner = string(job, "id")?;
    if job["purpose"] == "answering_repair" {
        let origin = string(job, "originatingAnsweringAttemptId")?;
        let Some(child) = crate::answering_repair_plan::settled_child(d, origin)? else { return Ok(false); };
        if child["id"] != job["id"] { return Err(conflict("Repair settlement child changed")); }
    }
    for recipient in rows(reservation, "recipients") {
        let id = string(recipient, "itemId")?;
        match repair_item_settlement(d, job, recipient)? {
            Some(true) => continue,
            Some(false) => return Ok(false),
            None => (),
        }
        if job["purpose"] == "answering_repair" && !repair_child_proposals_unchanged(d, job, id) {
            return Ok(false);
        }
        let proposals: Vec<_> = proposals(d)
            .filter(|p| proposal_owner(p) == Some(owner) && p["itemId"] == id)
            .collect();
        // No result/proposal cannot establish that an expensive attempt was safe.
        if proposals.is_empty() {
            let repair_attention = job["purpose"] == "answering_repair"
                && rows(&job["prepareOutcome"], "finalAssessments").iter()
                    .any(|a| a["itemId"] == id && a["outcome"] == "needs_attention")
                && !rows(&job["prepareOutcome"], "candidates").iter().any(|c| c["itemId"] == id);
            if repair_attention || settled_attention(job, id) || failed_superseded(d, job, id) {
                continue;
            }
            return Ok(false);
        }
        for proposal in proposals {
            if proposal["status"] == "stale"
                && !rows(d, "operations")
                    .iter()
                    .any(|op| op["proposalId"] == proposal["id"])
                && (superseded(d, job, proposal, id)
                    || authenticated_source_stale(d, job, proposal, id))
            {
                continue;
            }
            if !matches!(
                proposal["status"].as_str(),
                Some("succeeded" | "failed" | "stale")
            ) {
                return Ok(false);
            }
            let operations: Vec<_> = rows(d, "operations")
                .iter()
                .filter(|op| op["proposalId"] == proposal["id"])
                .collect();
            if operations.is_empty()
                || operations
                    .iter()
                    .any(|op| !known_operation(op) || op["itemId"] != id)
            {
                return Ok(false);
            }
            // Settlement is tied to this saved route, not an arbitrary operation
            // for a local id that was retargeted after preparation.
            let wanted: BTreeSet<String> = rows(recipient, "keys")
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect();
            for operation in operations {
                let target = &operation["target"];
                let Some(saved_binding) = target.get("connectorBinding") else {
                    return Ok(false);
                };
                let Ok(saved_binding) = ConnectorBinding::from_json(saved_binding) else {
                    return Ok(false);
                };
                let (Some(object), Some(item)) =
                    (target["objectId"].as_str(), target["itemId"].as_str())
                else {
                    return Ok(false);
                };
                if !wanted.contains(&key(json!([
                    "recipient",
                    scope(&saved_binding),
                    object,
                    item
                ]))) {
                    return Ok(false);
                }
            }
        }
    }
    Ok(true)
}

fn superseded(d: &Value, job: &Value, proposal: &Value, item_id: &str) -> bool {
    if job["status"] != "completed" {
        return false;
    }
    let Some(item) = rows(d, "items").iter().find(|i| i["id"] == item_id) else {
        return false;
    };
    let explicit = item["autoRevalidation"]["restartRunId"]
        .as_str()
        .is_some_and(|run| {
            rows(d, "preparationRuns").iter().any(|r| {
                r["runId"] == run
                    && r["applied"] == true
                    && rows(r, "itemIds").iter().any(|id| id == item_id)
            })
        })
        && proposal["staleReason"] == "Operator requested a fresh preparation run";
    let revalidation = rows(d, "jobs")
        .iter()
        .chain(rows(d, "scopeOwners"))
        .any(|next| {
            let previous = if next["scopeOwnerProjection"]["version"] == 1 {
                &next["scopeOwnerProjection"]["supersedes"]
            } else {
                &next["prepareBundle"]["request"]["previousDecision"]
            };
            next["purpose"] == "auto_revalidate"
                && matches!(next["status"].as_str(), Some("running" | "completed"))
                && next["refId"] == item_id
                && previous["prepareRunId"] == job["id"]
                && previous["proposalId"] == proposal["id"]
        });
    explicit || revalidation
}

fn preparation_job(job: &Value) -> bool {
    repair_owned(job)
        || !job["scopeReservation"].is_null()
        || job["refId"] == "engine_prepare"
        || matches!(
            job["purpose"].as_str(),
            Some("engine_prepare" | "auto_prepare" | "auto_revalidate" | "answering_repair")
        )
}

/// Storage uses the same owner predicate before constructing compact controls.
/// Terminal legacy history is omitted unless it retains explicit uncertainty.
pub(crate) fn requires_control(job: &Value) -> bool {
    preparation_job(job) && !terminal_unproven_legacy(&Value::Null, job)
}
fn legacy_reservation(d: &Value, job: &Value) -> ApiResult<Option<Value>> {
    if job["scopeOwnerProjection"]["version"] == 1 {
        if let Some(saved) = job.get("scopeReservation") {
            keys(saved)?;
            if saved["ownerJobId"] != job["id"] {
                return Err(conflict("Preparation reservation owner changed"));
            }
            let binding = ConnectorBinding::from_json(&saved["connectorBinding"])
                .map_err(|e| conflict(e.0))?;
            let active = crate::active_binding(d)?;
            binding
                .validate_scope(&active.workspace_id, &active.account_id)
                .map_err(|e| conflict(e.0))?;
            return Ok(Some(saved.clone()));
        }
        // PostgreSQL may reduce the captured request to route fields BEFORE
        // transferring bytes. This storage-only control is never model or
        // proposal provenance, so its original request hash is not recomputed.
        if job.get("prepareBundle").is_some() {
            return captured_control(d, job, false).map(Some);
        }
        return Ok(None);
    }
    if job.get("prepareBundle").is_some() {
        return capture(d, string(job, "id")?).map(Some);
    }
    let ids = job["selectedItemIds"]
        .as_array()
        .or_else(|| job["itemIds"].as_array());
    let ids: Vec<String> = if let Some(ids) = ids {
        ids.iter()
            .filter_map(Value::as_str)
            .map(str::to_owned)
            .collect()
    } else {
        proposals(d)
            .filter(|p| proposal_owner(p) == job["id"].as_str())
            .filter_map(|p| p["itemId"].as_str().map(str::to_owned))
            .collect()
    };
    if ids.is_empty() {
        return Ok(None);
    }
    let mut scope_keys = BTreeSet::new();
    let binding = crate::active_binding(d)?;
    let mut recipients = Vec::new();
    for id in BTreeSet::from_iter(ids) {
        let Some(item) = rows(d, "items").iter().find(|i| i["id"] == id) else {
            return Ok(None);
        };
        let recipient_keys = item_keys(&binding, item)?;
        scope_keys.extend(recipient_keys.iter().cloned());
        recipients.push(json!({"itemId":id,"keys":recipient_keys}));
    }
    let scope_keys = json!(scope_keys);
    Ok(Some(
        json!({"version":1,"ownerJobId":job["id"],"keysDigest":hash(&scope_keys),"keys":scope_keys,"recipients":recipients}),
    ))
}

/// Read-only storage projection for unrelated ownership controls. Put these in
/// ephemeral `scopeOwners`, never persist them as replacement job records. The
/// projection must be protected by the writer's existing source-delta guards.
/// Bound/current jobs and proposal-origin jobs stay full for their normal checks.
pub(crate) fn compact_owner(d: &Value, job: &Value) -> ApiResult<Value> {
    if repair_owned(job) {
        if job["scopeOwnerProjection"]["version"] == 1 {
            return Err(conflict("Repair ownership requires complete native root and child context"));
        }
        // Storage must preserve these exact bytes and the full child proposal
        // closure, or fall back to its guarded full context. They are read-only
        // canonical records, never newly minted settlement witnesses.
        return Ok(job.clone());
    }
    let mut compact = json!({"id":job["id"],"kind":job["kind"],"purpose":job["purpose"],
        "refId":job["refId"],"status":job["status"],"error":job["error"],
        "scopeOwnerProjection":{"version":1,"retainedOutput":has_retained_checkpoint(job),
            "reservationWasSaved":saved_reservation(job),"unfinishedModel":unfinished_model(job),
            "firstAdmissionUnresolved":first_admission_unresolved(job),
            "noSelected":no_selected_scope(d,job),
            "retrySafe":witnessed_retry_safe(d,job),
            "cancelledUnpaid":safe_unpaid_cancel(d,job)}});
    let previous = if job["scopeOwnerProjection"]["version"] == 1 {
        &job["scopeOwnerProjection"]["supersedes"]
    } else {
        &job["prepareBundle"]["request"]["previousDecision"]
    };
    if previous.is_object() {
        compact["scopeOwnerProjection"]["supersedes"] = json!({"prepareRunId":previous["prepareRunId"],"proposalId":previous["proposalId"],"itemId":previous["itemId"]});
    }
    if let Some(account) = job.get("account") {
        compact["account"] = account.clone();
    }
    for field in ["accountId", "connectorBinding"] {
        if let Some(value) = job.get(field) {
            compact[field] = value.clone();
        }
    }
    if let Some(witness) = job.get("scopeFailure") {
        compact["scopeFailure"] = witness.clone();
    }
    if !explicit_foreign(d, job)
        && preparation_job(job)
        && !safe_unpaid_cancel(d, job)
        && !terminal_unproven_legacy(d, job)
        && !no_selected_scope(d, job)
    {
        if let Some(mut reservation) = legacy_reservation(d, job)? {
            // Legacy route derivation lacks bundle identity but still binds a
            // single connector/company for the trusted read-only projection.
            if reservation.get("connectorBinding").is_none() {
                reservation["connectorBinding"] = crate::active_binding(d)?.to_json();
            }
            let attention: Vec<_> = rows(&reservation, "recipients")
                .iter()
                .filter_map(|r| r["itemId"].as_str())
                .filter(|id| settled_attention(job, id))
                .collect();
            compact["scopeOwnerProjection"]["attentionItemIds"] = json!(attention);
            let terminal: Vec<_> = rows(&reservation, "recipients").iter()
                .filter_map(|r| r["itemId"].as_str()).filter(|id| auto_terminal_item(job, id)).collect();
            compact["scopeOwnerProjection"]["autoTerminalItemIds"] = json!(terminal);
            let engine_terminal:Vec<_>=rows(&reservation,"recipients").iter().filter_map(|r|r["itemId"].as_str())
                .filter(|id|crate::engine_prepare::terminal_no_candidate_member(job,id)).collect();
            compact["scopeOwnerProjection"]["engineTerminalItemIds"]=json!(engine_terminal);
            compact["scopeReservation"] = reservation;
        }
    }
    Ok(compact)
}

/// Fail before fresh model spending or conflicting manual admission. An owner
/// exemption must come from a trusted generated proposal/job, never HTTP input.
pub(crate) fn assert_available(
    d: &Value,
    item_ids: &[String],
    owner_job_id: Option<&str>,
) -> ApiResult<()> {
    if item_ids.is_empty() {
        return Ok(());
    }
    let binding = crate::active_binding(d)?;
    let mut wanted = BTreeSet::new();
    for id in item_ids {
        wanted.extend(item_keys(&binding, crate::row(d, "items", id)?)?);
    }
    if let Some(owner) = owner_job_id {
        let reservation = capture(d, owner)?;
        if reservation["connectorBinding"] != binding.to_json() {
            return Err(conflict("Preparation reservation owner connector changed"));
        }
        for id in item_ids {
            let recipient = rows(&reservation, "recipients")
                .iter()
                .find(|r| r["itemId"] == id.as_str())
                .ok_or_else(|| conflict("Preparation reservation owner has a foreign recipient"))?;
            if recipient["keys"] != json!(item_keys(&binding, crate::row(d, "items", id)?)?) {
                return Err(conflict("Preparation reservation recipient route changed"));
            }
        }
    }
    let repair_origin=if let Some(owner)=owner_job_id {
        // Ordinary compact owners were already authenticated by capture(). A
        // repair exemption additionally requires its complete native job; a
        // compact ownership row cannot supply the consumed-round authority.
        let job=rows(d,"jobs").iter().find(|job|job["id"]==owner)
            .or_else(||rows(d,"scopeOwners").iter().find(|job|job["id"]==owner));
        if job.is_some_and(|job|job["purpose"]=="answering_repair") {Some(crate::answering_repair_plan::reservation_origin(d,owner,item_ids)?)}else{None}
    }else{None};
    check_keys_with_repair(d,&wanted,owner_job_id,None,repair_origin.as_deref())
}

// This capability exists only across one synchronous native admission call.
// Private fields and no Clone/Serialize/Deserialize prevent persisted or caller
// supplied JSON from becoming permission to review an unmerged repair draft.
pub(crate) struct RepairGenerationPermit {
    child_id: String,
    origin: String,
    child_sha256: String,
    root_sha256: String,
    bundle_sha256: String,
    result_sha256: String,
    material_receipt: Value,
    generation_metadata: Option<Value>,
    binding: Value,
    candidates: Vec<RepairGenerationCandidate>,
    existing_proposals: BTreeSet<String>,
    synchronous: std::marker::PhantomData<std::rc::Rc<()>>,
}
struct RepairGenerationCandidate {
    candidate: Value,
    expected_item: Value,
    route_target: Value,
    source_digest: String,
    decision_media_contract: Option<Value>,
}
pub(crate) fn capture_repair_generation(d:&Value,job_id:&str,bundle:&Value,result:&Value)->ApiResult<Option<RepairGenerationPermit>>{
    let child=crate::row(d,"jobs",job_id)?;
    if child["purpose"]!="answering_repair"||rows(result,"proposals").is_empty(){return Ok(None);}
    if child["prepareBundle"]!=*bundle||child["answeringRepairPlan"]["prepareBundle"]!=*bundle{
        return Err(conflict("Repair generation bundle differs from its native claim"));
    }
    let ids=rows(result,"proposals").iter().map(|p|string(p,"itemId").map(str::to_owned)).collect::<ApiResult<Vec<_>>>()?;
    let origin=crate::answering_repair_plan::reservation_origin(d,job_id,&ids)?;
    assert_available(d,&ids,Some(job_id))?;
    crate::prepare_bundle::current(d,bundle).map_err(conflict)?;
    let material_receipt=crate::answering_repair_plan::generation_paid_closure(d,job_id,result)?;
    let binding=crate::active_binding(d)?;
    let mut seen=BTreeSet::new();let mut candidates=Vec::new();
    for candidate in rows(result,"proposals"){
        let id=string(candidate,"itemId")?;
        if !seen.insert(id)||rows(&bundle["request"],"items").iter().filter(|i|i["id"]==id).count()!=1{
            return Err(conflict("Repair generation candidate membership changed"));
        }
        let item=crate::row(d,"items",id)?;
        if rows(&bundle["request"],"items").iter().find(|i|i["id"]==id).unwrap()["revision"]!=item["revision"]{
            return Err(conflict("Repair generation recipient revision changed"));
        }
        let route_target=crate::bound_item(&binding,item)?;
        let mut expected_item=item.clone();
        // This is exactly create_generated_proposal's local transition. Other
        // valid prior workflows retain their original revision and all fields.
        if item["workflow"]=="attention"{
            expected_item["workflow"]=json!("prepared");crate::bump(&mut expected_item);
        }
        let decision_media_contract=(crate::decision_media::enabled(&bundle["request"])&&crate::media_queue::requires_video(d,item))
            .then(||json!(crate::decision_media::CONTRACT));
        candidates.push(RepairGenerationCandidate{candidate:candidate.clone(),expected_item,route_target,decision_media_contract,
            source_digest:crate::prepare_bundle::review_fingerprint(d,id).map_err(conflict)?});
    }
    if proposals(d).any(|p|proposal_owner(p)==Some(job_id)){
        return Err(conflict("Repair generation cannot reuse an existing child proposal"));
    }
    let existing_proposals=proposals(d).map(|p|string(p,"id").map(str::to_owned)).collect::<ApiResult<BTreeSet<_>>>()?;
    let generation_metadata=crate::prepare_bundle::generation_metadata(result).map_err(conflict)?
        .as_ref().map(crate::prepare_bundle::proposal_generation_metadata);
    Ok(Some(RepairGenerationPermit{child_id:job_id.to_owned(),root_sha256:hash(crate::row(d,"jobs",&origin)?),origin,
        child_sha256:hash(child),bundle_sha256:hash(bundle),result_sha256:hash(result),material_receipt,generation_metadata,
        binding:binding.to_json(),candidates,existing_proposals,synchronous:std::marker::PhantomData}))
}
impl RepairGenerationPermit {
    pub(crate) fn validate_created(self,d:&Value,proposal_ids:&[String],bundle:&Value,result:&Value)->ApiResult<()>{
        if hash(bundle)!=self.bundle_sha256||hash(result)!=self.result_sha256
            ||hash(crate::row(d,"jobs",&self.child_id)?)!=self.child_sha256
            ||hash(crate::row(d,"jobs",&self.origin)?)!=self.root_sha256
            ||crate::active_binding(d)?.to_json()!=self.binding||proposal_ids.len()!=self.candidates.len(){
            return Err(conflict("Repair generation permit no longer matches its native invocation"));
        }
        crate::answering_repair_plan::generation_paid_closure(d,&self.child_id,result)?;
        crate::prepare_bundle::current(d,bundle).map_err(conflict)?;
        crate::preparation_materials::require_request(d,&bundle["request"]).map_err(conflict)?;
        let binding=crate::active_binding(d)?;let mut seen=BTreeSet::new();let mut recipients=BTreeSet::new();let mut wanted=BTreeSet::new();
        for id in proposal_ids{
            if !seen.insert(id)||self.existing_proposals.contains(id)||rows(d,"proposals").iter().filter(|p|p["id"]==id.as_str()).count()!=1{
                return Err(conflict("Repair generation must review only its newly created proposal identities"));
            }
            let proposal=crate::row(d,"proposals",id)?;
            let captured=self.candidates.iter().find(|c|c.candidate["itemId"]==proposal["itemId"])
                .ok_or_else(||conflict("Repair generation proposal escaped its paid candidates"))?;
            let item_id=string(proposal,"itemId")?;
            if !recipients.insert(item_id)||crate::row(d,"items",item_id)?!=&captured.expected_item
                ||proposal["nativeCreationOrigin"]!="model_generation_v1"||proposal["status"]!="draft"||proposal["revision"]!=1
                ||proposal["kind"]!=captured.candidate["kind"]||proposal["text"]!=captured.candidate["text"]
                ||proposal["prepareRunId"]!=self.child_id||proposal["prepareBundleId"]!=bundle["id"]||proposal["prepareBundleDigest"]!=bundle["digest"]
                ||proposal["itemRevision"]!=captured.expected_item["revision"]||proposal["routeTarget"]!=captured.route_target
                ||proposal["contextEvidenceDigest"]!=captured.expected_item["contextEvidenceDigest"]||proposal["branchContextDigest"]!=captured.expected_item["branchContextDigest"]
                ||proposal["reviewContextDigest"]!=captured.source_digest||proposal["sourceContextDigest"]!=captured.source_digest
                ||crate::prepare_bundle::review_fingerprint(d,item_id).map_err(conflict)?!=captured.source_digest
                ||proposal["modelMaterialReceipt"]!=self.material_receipt||proposal["mandatoryMaterialContract"]!=crate::preparation_materials::CONTRACT
                ||proposal.get("decisionMediaContract")!=captured.decision_media_contract.as_ref()
                ||proposal.get("generationMetadata")!=self.generation_metadata.as_ref()
                ||proposal.get("knowledgeManifest")!=Some(&bundle["request"]["knowledgeManifest"])
                ||proposal.get("knowledgePolicyVersion")!=Some(&bundle["request"]["knowledgePolicyVersion"])
                ||proposal["draftSessionId"]!=captured.expected_item["draftSessionId"]||proposal["sources"]!=json!([])
                ||!proposal["sourceProposalId"].is_null()||!proposal["sourceProposalRevision"].is_null()
                ||!proposal["operatorCloseDecision"].is_null()||!proposal["priorPreparationOrigin"].is_null()
                ||proposal.get(crate::retained_paid_recovery::FIELD).is_some()
                ||!proposal["origin"].is_null()||!proposal["history"].is_null()||!proposal["editorialReview"].is_null()||!proposal["editorialReviews"].is_null(){
                return Err(conflict("Repair generation candidate or native creation transition changed"));
            }
            crate::validate_route(proposal,&binding,&crate::bound_item(&binding,&captured.expected_item)?)?;
            wanted.extend(item_keys(&binding,&captured.expected_item)?);
        }
        if proposals(d).filter(|p|proposal_owner(p)==Some(self.child_id.as_str())).count()!=proposal_ids.len(){
            return Err(conflict("Repair generation has another saved child proposal"));
        }
        // The pre-mutation proof exempts exactly its immutable child and root.
        // Generation keeps alias UNKNOWN and every unrelated owner/proposal
        // fenced; it never invokes the ordinary unmerged action exemption.
        check_keys_with_repair(d,&wanted,Some(&self.child_id),None,Some(&self.origin))
    }
}

/// Project a terminal native repair without recapturing changed item revisions.
/// Only the frozen original root and that exact proved child are exempted.
pub(crate) fn assert_repair_projection(
    d: &Value, origin: &str, child_id: &str, item_ids: &[String],
) -> ApiResult<()> {
    let child = crate::answering_repair_plan::settled_child(d, origin)?
        .ok_or_else(|| conflict("Repair projection terminal child missing"))?;
    if child["id"] != child_id || item_ids.is_empty() {
        return Err(conflict("Repair projection child identity changed"));
    }
    let affected = rows(&child["answeringRepairPlan"], "affectedRecipientIds");
    let selected: BTreeSet<_> = item_ids.iter().collect();
    if selected.len() != item_ids.len() || item_ids.iter().any(|id| !affected.contains(&json!(id))) {
        return Err(conflict("Repair projection recipient outside frozen scope"));
    }
    let binding = crate::active_binding(d)?;
    let original_scope = capture(d, origin)?;
    let child_scope = capture(d, child_id)?;
    let mut wanted = BTreeSet::new();
    for reservation in [&original_scope, &child_scope] {
        if reservation["connectorBinding"] != binding.to_json() {
            return Err(conflict("Repair projection saved connector changed"));
        }
        for id in item_ids {
            let current = item_keys(&binding, crate::row(d, "items", id)?)?;
            let recipient = rows(reservation, "recipients").iter().find(|r| r["itemId"] == id.as_str())
                .ok_or_else(|| conflict("Repair projection saved recipient missing"))?;
            if recipient["keys"] != json!(current) {
                return Err(conflict("Repair projection recipient route changed"));
            }
            wanted.extend(current);
        }
    }
    check_keys_with_repair(d, &wanted, Some(child_id), None, Some(origin))
}

/// Validate the capture and check overlapping ledger ownership, excluding only
/// its exact own job. Uncertain operations are never exempted by job ownership.
pub(crate) fn check(d: &Value, reservation: &Value, exclude_job: Option<&str>) -> ApiResult<()> {
    let owner = string(reservation, "ownerJobId")?;
    if capture(d, owner)? != *reservation || exclude_job.is_some_and(|id| id != owner) {
        return Err(conflict("Preparation reservation owner binding changed"));
    }
    if exclude_job==Some(owner)&&rows(d,"jobs").iter().any(|job|job["id"]==owner&&job["purpose"]=="answering_repair"){
        let recipients=rows(reservation,"recipients").iter().map(|r|string(r,"itemId").map(str::to_owned)).collect::<ApiResult<Vec<_>>>()?;
        let origin=crate::answering_repair_plan::reservation_origin(d,owner,&recipients)?;
        // The candidate afterimage has just added this native child capture.
        // Authenticate its exact consumed root claim before exempting only the
        // root. Operations and every unrelated reservation remain checked.
        return check_keys_with_repair(d,&keys(reservation)?,Some(owner),None,Some(&origin));
    }
    check_keys(d, &keys(reservation)?, exclude_job)
}
fn check_keys(d: &Value, wanted: &BTreeSet<String>, owner: Option<&str>) -> ApiResult<()> {
    check_keys_for_proposal(d, wanted, owner, None)
}
fn check_keys_for_proposal(
    d: &Value,
    wanted: &BTreeSet<String>,
    owner: Option<&str>,
    exact_proposal: Option<&Value>,
) -> ApiResult<()> {
    let repair_origin = match (owner, exact_proposal) {
        (Some(owner), Some(proposal)) => repair_action_origin(d, owner, proposal)?,
        _ => None,
    };
    check_keys_with_repair(d,wanted,owner,exact_proposal,repair_origin.as_deref())
}
fn repair_action_origin(d: &Value, owner: &str, proposal: &Value) -> ApiResult<Option<String>> {
    let Some(job) = rows(d, "jobs").iter().chain(rows(d, "scopeOwners")).find(|job| job["id"] == owner) else {
        return Ok(None);
    };
    if job["purpose"] != "answering_repair" { return Ok(None); }
    let origin = string(job, "originatingAnsweringAttemptId")?;
    let Some(child) = crate::answering_repair_plan::merged_child(d, origin)? else {
        return Err(conflict("Repair proposal requires its native merged workflow"));
    };
    let source_id = proposal["id"].as_str().or_else(|| proposal["origin"]["id"].as_str());
    if child["id"] != owner || !rows(&child["prepareOutcome"], "candidates").iter().any(|candidate|
        candidate["itemId"] == proposal["itemId"] && candidate["status"] == "review"
            && (candidate["proposalId"].as_str() == source_id
                || candidate["proposalId"] == proposal["origin"]["id"])) {
        return Err(conflict("Repair proposal is outside its exact native admitted candidate"));
    }
    // This is ordinary local review of that saved candidate or a canonically
    // captured human alternative. It cannot release generation scope, retry
    // the paid child, or exempt any other root/child/operation.
    Ok(Some(origin.to_owned()))
}
fn check_keys_with_repair(
    d:&Value,wanted:&BTreeSet<String>,owner:Option<&str>,exact_proposal:Option<&Value>,repair_origin:Option<&str>,
) -> ApiResult<()> {
    // Proposal action admission already has its own exact recipient operation
    // guard. The branch-wide generation fence must not replace that contract.
    for op in rows(d, "operations")
        .iter()
        .filter(|_| exact_proposal.is_none())
    {
        if !known_operation(op) && overlaps(wanted, &record_keys(d, op, "target")?) {
            return Err(conflict(
                "Preparation scope conflicts with an unresolved operation",
            ));
        }
    }
    let mut seen = BTreeSet::new();
    for job in rows(d, "jobs").iter().chain(rows(d, "scopeOwners")) {
        if !seen.insert(string(job, "id")?) {
            continue;
        }
        if !preparation_job(job) || explicit_foreign(d, job) {
            continue;
        }
        if job["id"].as_str()==owner {
            if first_admission_unresolved(job) {
                return Err(conflict("First preparation admission is unresolved; owner identity does not authorize model restart"));
            }
            continue;
        }
        // Only the exact original owner proved by the frozen consumed round is
        // exempted. Other jobs and every operation keep their ordinary fences.
        if job["id"].as_str()==repair_origin&&repair_origin.is_some(){continue;}
        if safe_unpaid_cancel(d, job)

            || terminal_unproven_legacy(d, job)
            || no_selected_scope(d, job)
            || witnessed_retry_safe(d, job)
        {
            continue;
        }
        let Some(reservation) = legacy_reservation(d, job)? else {
            return Err(conflict("Legacy preparation ownership is unresolved"));
        };
        if overlaps(wanted, &keys(&reservation)?) && !settled(d, job, &reservation)?
            && !settled_auto_scope(d, job, &reservation, wanted)?
            && !settled_engine_scope(d,job,&reservation,wanted)? {
            return Err(conflict(
                "Preparation scope is reserved by prior paid or unfinished work",
            ));
        }
    }
    for proposal in proposals(d) {
        // Ordinary operator alternatives are review choices, not evidence of
        // outstanding paid generation. Existing approval/recipient guards own
        // their action admission. Generated lineage still holds this scope.
        if !paid_lineage(proposal) {
            continue;
        }
        if exact_proposal.is_some_and(|p| same_proposal_control(p, proposal)) {
            continue;
        }
        if explicit_foreign(d, proposal)
            || matches!(
                proposal["status"].as_str(),
                Some("succeeded" | "failed" | "stale" | "cancelled" | "superseded")
            )
        {
            continue;
        }
        if proposal_owner(proposal).is_some_and(|id| Some(id) == owner) {
            continue;
        }
        if repair_origin.is_some()&&proposal_owner(proposal)==repair_origin{continue;}
        let recorded=record_keys(d, proposal, "routeTarget")?;
        let overlapping=if exact_proposal.is_some() {
            // Legacy generated candidates may share a reviewed bundle/branch.
            // Their action admission protects exact recipients; durable paid
            // job reservations above still enforce whole-branch exclusivity.
            wanted.iter().filter(|k|k.starts_with("[\"item\",")||k.starts_with("[\"recipient\","))
                .any(|k|recorded.contains(k))
        }else{overlaps(wanted,&recorded)};
        if overlapping {
            return Err(conflict(
                "Preparation scope conflicts with an existing proposal",
            ));
        }
    }
    Ok(())
}

/// Resolve an exemption only through saved generated provenance and exact
/// membership. A manual proposal without such provenance has no owner exemption.
pub(crate) fn owner_for_proposal(d: &Value, proposal: &Value) -> ApiResult<Option<String>> {
    if proposal.get(crate::proposal_source_rebind::FIELD).is_some(){
        crate::proposal_source_rebind::validate_origin_and_current_target(&crate::prepare_bundle::EvidenceContext::new(d),proposal)?;
        let old=crate::proposal_source_rebind::original_proposal(proposal)?;
        let scope=source_rebind_scope(d,old)?;
        return Ok(Some(string(&scope,"ownerJobId")?.to_owned()));
    }
    // Retained recovery has its own native proof; never manufacture generated
    // provenance or allow this action exemption to release generation scope.
    if proposal.get(crate::retained_paid_recovery::FIELD).is_some() {
        return crate::retained_paid_recovery_registry::validate_proposal(d,proposal,None);
    }
    let Some(owner) = proposal_owner(proposal) else {
        return Ok(None);
    };
    let item = string(proposal, "itemId")?;
    let reservation = capture(d, owner)?;
    let binding = crate::active_binding(d)?;
    let current = crate::bound_item(&binding, crate::row(d, "items", item)?)?;
    let approved =
        ResourceRef::from_item(&binding, &proposal["routeTarget"]).map_err(|e| conflict(e.0))?;
    if approved != ResourceRef::from_item(&binding, &current).map_err(|e| conflict(e.0))?
        || proposal
            .get("prepareBundleId")
            .is_some_and(|v| v != &reservation["prepareBundleId"])
        || proposal
            .get("prepareBundleDigest")
            .is_some_and(|v| v != &reservation["prepareBundleDigest"])
    {
        return Err(conflict(
            "Preparation proposal owner route or capture changed",
        ));
    }
    let recipient = rows(&reservation, "recipients")
        .iter()
        .find(|r| r["itemId"] == item)
        .ok_or_else(|| conflict("Preparation reservation owner has a foreign recipient"))?;
    let wanted = item_keys(&binding, crate::row(d, "items", item)?)?;
    if reservation["connectorBinding"] != binding.to_json() || recipient["keys"] != json!(wanted) {
        return Err(conflict("Preparation reservation recipient route changed"));
    }
    check_keys_for_proposal(d, &wanted, Some(owner), Some(proposal))?;
    Ok(Some(owner.to_owned()))
}

/// An action/review-only exemption for the exact preserved old generated
/// owner. The immutable generation reservation is never refreshed or released.
/// Both historical branch aliases and current aliases retain their fences.
pub(crate) fn source_rebind_scope(d:&Value,old:&Value)->ApiResult<Value>{
    let owner=proposal_owner(old).ok_or_else(||conflict("Source rebind requires its native paid owner"))?;
    let job=crate::row(d,"jobs",owner)?;
    if job["status"]!="completed"||unfinished_model(job){return Err(conflict("Source rebind paid owner is unfinished or uncertain"));}
    let reservation=capture(d,owner)?;
    let id=string(old,"itemId")?;
    let recipient=rows(&reservation,"recipients").iter().find(|r|r["itemId"]==id)
        .ok_or_else(||conflict("Source rebind old paid recipient unavailable"))?;
    let binding=crate::active_binding(d)?;let item=crate::row(d,"items",id)?;
    let current=crate::bound_item(&binding,item)?;
    if reservation["connectorBinding"]!=binding.to_json()
        ||ResourceRef::from_item(&binding,&old["routeTarget"]).map_err(|e|conflict(e.0))?
            !=ResourceRef::from_item(&binding,&current).map_err(|e|conflict(e.0))?
        ||old["prepareBundleId"]!=reservation["prepareBundleId"]||old["prepareBundleDigest"]!=reservation["prepareBundleDigest"]{
        return Err(conflict("Source rebind recipient or old preparation binding changed"));
    }
    let old_keys=rows(recipient,"keys").iter().map(|key|key.as_str().map(str::to_owned)
        .ok_or_else(||conflict("Source rebind old recipient keys unavailable"))).collect::<ApiResult<BTreeSet<_>>>()?;
    let current_keys=item_keys(&binding,item)?;let mut union=old_keys.clone();union.extend(current_keys.iter().cloned());
    for op in rows(d,"operations"){
        if !known_operation(op)&&overlaps(&union,&record_keys(d,op,"target")?){return Err(conflict("Source rebind scope has an unresolved operation"));}
    }
    check_keys_for_proposal(d,&union,Some(owner),Some(old))?;
    Ok(json!({"ownerJobId":owner,"oldKeys":old_keys,"currentKeys":current_keys,"unionKeys":union,
        "scopeReservationSha256":hash(&reservation),"prepareBundleId":reservation["prepareBundleId"],"prepareBundleDigest":reservation["prepareBundleDigest"]}))
}

pub(crate) fn assert_proposal(d: &Value, proposal: &Value) -> ApiResult<()> {
    let owner = owner_for_proposal(d, proposal)?;
    let binding = crate::active_binding(d)?;
    let wanted = item_keys(
        &binding,
        crate::row(d, "items", string(proposal, "itemId")?)?,
    )?;
    check_keys_for_proposal(d, &wanted, owner.as_deref(), Some(proposal))
}

/// An explicit authenticated owner may choose a fresh empty CLOSE without
/// consuming or releasing an old paid/model outcome. Generation and ordinary
/// proposal admission deliberately keep their existing uncertainty fences.
pub(crate) fn assert_operator_close(d: &Value, proposal: &Value) -> ApiResult<()> {
    operator_close_scope(d, proposal, None)
}

pub(crate) fn assert_operator_close_for_operation(d: &Value, proposal: &Value, own: &Value) -> ApiResult<()> {
    if own["proposalId"] != proposal["id"] || own["itemId"] != proposal["itemId"]
        || own["status"] != "dispatching" || own["action"]["action"] != "close"
        || own["action"]["actionId"] != own["id"]
        || !rows(d, "operations").iter().any(|op| op == own) {
        return Err(conflict("Operator close requires its exact saved operation"));
    }
    let binding = crate::active_binding(d)?;
    let item = crate::row(d, "items", string(proposal, "itemId")?)?;
    if own["action"]["objectId"] != item["objectId"] || own["action"]["itemId"] != item["itemId"]
        || ResourceRef::from_item(&binding, &own["target"]).map_err(|e| conflict(e.0))?
            != ResourceRef::from_item(&binding, &proposal["routeTarget"]).map_err(|e| conflict(e.0))? {
        return Err(conflict("Operator close operation recipient route changed"));
    }
    operator_close_scope(d, proposal, Some(own))
}

fn operator_close_scope(d: &Value, proposal: &Value, own: Option<&Value>) -> ApiResult<()> {
    let item = crate::row(d, "items", string(proposal, "itemId")?)?;
    if paid_lineage(proposal) || !crate::operator_close::current_for_operation(d, proposal, item, own)? {
        return Err(conflict("A fresh authenticated operator close decision is required"));
    }
    let binding = crate::active_binding(d)?;
    let target = crate::bound_item(&binding, item)?;
    if ResourceRef::from_item(&binding, &proposal["routeTarget"]).map_err(|e| conflict(e.0))?
        != ResourceRef::from_item(&binding, &target).map_err(|e| conflict(e.0))? {
        return Err(conflict("Operator close recipient route changed"));
    }
    let wanted = item_keys(&binding, item)?;
    for job in rows(d, "jobs").iter().chain(rows(d, "scopeOwners")) {
        if !preparation_job(job) || explicit_foreign(d, job)
            || !matches!(job["status"].as_str(), Some("queued" | "running")) { continue; }
        let Some(reservation) = legacy_reservation(d, job)? else {
            return Err(conflict("Legacy preparation ownership is unresolved"));
        };
        if overlaps(&wanted, &keys(&reservation)?) {
            return Err(conflict("Active preparation must finish before an operator close"));
        }
    }
    // Scoped projections supply complete account-wide active transport controls.
    // This final-decision path requires quiescence; it never takes over a worker.
    let jobs = if d.get("scopeOwners").is_some() {
        if d["activeExternalJobs"]["version"] != 1 || d["activeExternalJobs"]["complete"] != true {
            return Err(crate::internal("Complete active external job controls required"));
        }
        d["activeExternalJobs"]["jobs"].as_array()
            .ok_or_else(|| crate::internal("Complete active external job controls required"))?
    } else {
        d["jobs"].as_array().ok_or_else(|| crate::internal("Current job history required"))?
    };
    if jobs.iter().any(|j| matches!(j["kind"].as_str(), Some("execute" | "reconcile"))
        && matches!(j["status"].as_str(), Some("queued" | "running" | "pending"))
        && !own.is_some_and(|op| j["kind"] == "execute"
            && op["approvalId"].as_str().is_some_and(|id| !id.is_empty() && j["refId"] == id))) {
        return Err(conflict("External execution/readback must finish before an operator close"));
    }
    // Keep exact social-recipient UNKNOWN protection. The sole exception is
    // the separately captured, immutable native preserved-reply close contract.
    let recipient_keys: BTreeSet<_> = wanted.iter()
        .filter(|k| k.starts_with("[\"item\",") || k.starts_with("[\"recipient\","))
        .cloned().collect();
    for op in rows(d, "operations") {
        if own.is_some_and(|own| own == op) { continue; }
        if known_operation(op) || !overlaps(&recipient_keys, &record_keys(d, op, "target")?) { continue; }
        let preserved = if own.is_some() {
            // current_for_operation already validated the entire captured set
            // after omitting only our exact native dispatching close.
            rows(&proposal["operatorCloseDecision"]["preservedUnknownReplies"], "operations")
                .iter().any(|saved| saved["operationId"] == op["id"] && saved["operationSha256"] == hash(op))
        } else {
            crate::unknown_reply_close::permits(d, op, proposal, item)
        };
        if !preserved {
            return Err(conflict("Operator close conflicts with an unresolved operation"));
        }
    }
    Ok(())
}

// Borrow the complete job collection, including completed/paid/uncertain
// controls. Duplicate string IDs retain the first row for find semantics while
// field presence aggregates across every duplicate for any semantics. Other
// JSON IDs use the original exact comparison; never stringify an untrusted ID.
struct JobHistoryEntry<'a> {
    first: &'a Value,
    has_failure: bool,
    has_reservation: bool,
}
struct JobHistoryIndex<'a> {
    rows: &'a [Value],
    strings: std::collections::HashMap<&'a str, JobHistoryEntry<'a>>,
}
impl<'a> JobHistoryIndex<'a> {
    fn new(rows: &'a [Value]) -> Self {
        let mut strings = std::collections::HashMap::with_capacity(rows.len());
        for row in rows {
            let Some(id) = row["id"].as_str() else { continue; };
            let entry = strings.entry(id).or_insert(JobHistoryEntry {
                first: row, has_failure: false, has_reservation: false,
            });
            entry.has_failure |= row.get("scopeFailure").is_some();
            entry.has_reservation |= row.get("scopeReservation").is_some();
        }
        Self { rows, strings }
    }
    fn first(&self, id: &Value) -> Option<&'a Value> {
        match id.as_str() {
            Some(id) => self.strings.get(id).map(|entry| entry.first),
            None => self.rows.iter().find(|row| row["id"] == *id),
        }
    }
    fn has_failure(&self, id: &Value) -> bool {
        match id.as_str() {
            Some(id) => self.strings.get(id).is_some_and(|entry| entry.has_failure),
            None => self.rows.iter().any(|row| row["id"] == *id && row.get("scopeFailure").is_some()),
        }
    }
    fn has_reservation(&self, id: &Value) -> bool {
        match id.as_str() {
            Some(id) => self.strings.get(id).is_some_and(|entry| entry.has_reservation),
            None => self.rows.iter().any(|row| row["id"] == *id && row.get("scopeReservation").is_some()),
        }
    }
}

fn unpaid_material_refinement(d:&Value,old:&Value,next:&Value)->ApiResult<()> {
    let fail=||conflict("Preparation scope permits only exact unpaid material refinement");
    let stages=&old["preparationStages"];let initial=&stages["initialAdmission"];
    let empty=|job:&Value,key:&str|job.get(key).is_none_or(|v|v.as_array().is_some_and(Vec::is_empty));
    if old["kind"]!="assistant"||old["status"]!="running"
        ||!matches!(old["purpose"].as_str(),Some("engine_prepare"|"auto_prepare"|"auto_revalidate"))
        ||!stages["first"].is_null()||!stages["review"].is_null()||stages.get("firstAdmission").is_some()
        ||stages.get("reviewChunks").is_some()||stages.get("answeringRepairs").is_some()||stages.get("repairBudget").is_some()
        ||!empty(old,"retainedEvidence")||!empty(old,"modelMaterialReceipts")
        ||old.get("scopeFailure").is_some()||old["prepareOutcome"].is_object()
        ||initial.as_object().is_none_or(|o|o.len()!=5)||initial["version"]!=1||initial["status"]!="scheduled"
        ||initial["requestSha256"]!=old["prepareBundle"]["digest"]
        ||!crate::preparation_materials::enabled(&old["prepareBundle"]["request"]){return Err(fail());}
    let token=crate::runtime_lifecycle::admission_token(d,crate::runtime_lifecycle::AdmissionClass::Preparation)?;
    if initial["owner"]!=json!({"account":token.account,"runtimeId":token.runtime_id,"releaseSha256":token.release_sha256,"epoch":token.epoch}){return Err(fail());}
    crate::prepare_bundle::current(d,&old["prepareBundle"]).map_err(|_|fail())?;
    crate::preparation_unit::current_bundle(d,&old["prepareBundle"],&crate::now()).map_err(|_|fail())?;
    crate::fact_followup::automatic_continuation_current(d,old)?;
    if stages["groupAdmission"].is_array()&&stages["groupAdmission"]!=crate::prepare_bundle::capture_groups(d,&old["prepareBundle"]).map_err(|_|fail())?{return Err(fail());}
    let mut expected_bundle=old["prepareBundle"].clone();
    // A manual frame request pins the original UNPAID owner's digest. The
    // storage afterimage already contains its proposed refined digest. Derive
    // the allowed delta against the authenticated old owner, while retaining
    // current posts, native frame results, runtime owner and every other job.
    // This proof-only view is never persisted or used as paid authority.
    let proof_context=if rows(d,"jobs").iter().any(|job|job["purpose"]=="manual_video_frames"
        &&job["manualFrameRequest"]["request"]["prepareJobId"]==old["id"]){
        let mut proof=d.clone();
        let matches=rows(&proof,"jobs").iter().filter(|job|job["id"]==old["id"]).count();
        if matches!=1{return Err(fail());}
        *crate::row_mut(&mut proof,"jobs",string(old,"id")?)?=old.clone();
        std::borrow::Cow::Owned(proof)
    }else{std::borrow::Cow::Borrowed(d)};
    crate::preparation_materials::attach_request(&proof_context,&mut expected_bundle["request"]).map_err(|_|fail())?;
    crate::preparation_materials::require_request(&proof_context,&expected_bundle["request"]).map_err(|_|fail())?;
    expected_bundle["digest"]=json!(hash(&expected_bundle["request"]));
    // attach_request is the entire allowed request delta. Source, family,
    // recipients, rules, instruction and all bundle metadata remain captured.
    let mut expected=old.clone();expected["prepareBundle"]=expected_bundle;
    expected["preparationStages"]["initialAdmission"]["requestSha256"]=expected["prepareBundle"]["digest"].clone();
    expected["preparationStages"]["groupAdmission"]=crate::prepare_bundle::capture_groups(d,&expected["prepareBundle"]).map_err(|_|fail())?;
    let prior=captured(d,old)?;if old["scopeReservation"]!=prior{return Err(fail());}
    let refined=captured(d,&expected)?;
    let mut same=refined.clone();same["prepareBundleDigest"]=prior["prepareBundleDigest"].clone();
    if same!=prior{return Err(fail());}
    expected["scopeReservation"]=refined;
    if *next!=expected{return Err(fail());}Ok(())
}
/// Only a native, ready mandatory-material enrichment of an UNSPENT capture
/// may update its existing scope digest. Keys/owner/recipient aliases do not move.
pub(crate) fn refresh_unpaid_scope(d:&mut Value,run:&str,prior_job:&Value)->ApiResult<()> {
    if prior_job["id"]!=run{return Err(conflict("Unpaid scope refinement owner changed"));}
    let mut candidate=crate::row(d,"jobs",run)?.clone();candidate["scopeReservation"]=captured(d,&candidate)?;
    unpaid_material_refinement(d,prior_job,&candidate)?;
    crate::row_mut(d,"jobs",run)?["scopeReservation"]=candidate["scopeReservation"].clone();Ok(())
}

/// Storage seam: old reservations cannot disappear or change except a fully
/// validated unpaid mandatory-material refinement. New reservations
/// must bind the immutable captured request, including same-request replays.
pub(crate) fn validate_change(before: &Value, after: &Value) -> ApiResult<()> {
    if before.get("scopeOwners") != after.get("scopeOwners")
        || before.get("scopeProposals") != after.get("scopeProposals")
    {
        return Err(conflict("Preparation ownership projection is read-only"));
    }
    let old_jobs = JobHistoryIndex::new(rows(before, "jobs"));
    let new_jobs = JobHistoryIndex::new(rows(after, "jobs"));
    for old in rows(before, "jobs") {
        if let Some(witness) = old.get("scopeFailure") {
            let next = new_jobs.first(&old["id"])
                .ok_or_else(|| conflict("Preparation failure witness owner disappeared"))?;
            if next.get("scopeFailure") != Some(witness) {
                return Err(conflict("Preparation failure release witness is immutable"));
            }
        }
        if let Some(saved) = old.get("scopeReservation") {
            let next = new_jobs.first(&old["id"])
                .ok_or_else(|| conflict("Preparation reservation owner disappeared"))?;
            if next.get("scopeReservation") != Some(saved)
                || saved["account"] != after["account"]
                || (next["prepareBundle"] != old["prepareBundle"]
                    && captured(after, next)? != *saved)
            {
                unpaid_material_refinement(after,old,next)?;
            }
        }
    }
    for job in rows(after, "jobs") {
        if let Some(witness) = job.get("scopeFailure") {
            if !old_jobs.has_failure(&job["id"])
            {
                let expected = capture_failed_no_result(
                    after,
                    string(job, "id")?,
                    string(witness, "category")?,
                )?;
                if !matches!(job["status"].as_str(), Some("running" | "failed"))
                    || expected != *witness
                {
                    return Err(conflict(
                        "Preparation failure release witness binding changed",
                    ));
                }
            }
        }
        if let Some(saved) = job.get("scopeReservation") {
            if !old_jobs.has_reservation(&job["id"])
            {
                if captured(after, job)? != *saved {
                    return Err(conflict("Preparation scope reservation binding changed"));
                }
                check(after, saved, Some(string(job, "id")?))?;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
#[path = "preparation_reservations_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "preparation_reservations_index_tests.rs"]
mod index_tests;

#[cfg(test)]
#[path = "preparation_first_admission_owner_tests.rs"]
mod first_admission_tests;
