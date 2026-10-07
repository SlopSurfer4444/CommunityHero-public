//! A distinct owner-authorized inbox close never resolves or repeats an UNKNOWN reply.
//! The caller must additionally verify the authenticated operator-close decision.
use crate::*;
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;

fn digest(value: &Value) -> String {
    format!("{:x}", Sha256::digest(value.to_string().as_bytes()))
}

fn route(item: &Value) -> Value {
    json!({"itemId":item["id"],"objectId":item["objectId"],
        "providerItemId":item["itemId"],"connectorBinding":item["connectorBinding"],
        "conversationKey":item["conversationKey"]})
}

fn same_recipient(prior: &Value, item: &Value) -> bool {
    if prior["itemId"] == item["id"] {
        return true;
    }
    let target = &prior["target"];
    if target["objectId"] != item["objectId"] || target["itemId"] != item["itemId"] {
        return false;
    }
    let current = ConnectorBinding::from_json(&item["connectorBinding"]);
    let previous = ConnectorBinding::from_json(&target["connectorBinding"]);
    match (current, previous) {
        (Ok(a), Ok(b)) => {
            a.id == b.id
                && a.workspace_id == b.workspace_id
                && a.account_id == b.account_id
                && a.connector == b.connector
                && a.provider_account_id == b.provider_account_id
        }
        (Ok(a), Err(_)) => target["connectorBinding"]["accountId"]
            .as_str()
            .is_none_or(|account| account == a.account_id),
        _ => true,
    }
}

fn ids(value: &Value) -> ApiResult<BTreeSet<String>> {
    let rows = value
        .as_array()
        .filter(|rows| !rows.is_empty() && rows.len() <= 100)
        .ok_or_else(|| bad("Choose exact UNKNOWN reply operations to preserve"))?;
    let mut result = BTreeSet::new();
    for value in rows {
        let id = value
            .as_str()
            .filter(|id| {
                !id.is_empty()
                    && id.len() <= 160
                    && id
                        .bytes()
                        .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_')
            })
            .ok_or_else(|| bad("Invalid preserved UNKNOWN operation identity"))?;
        if !result.insert(id.to_owned()) {
            return Err(bad("Duplicate preserved UNKNOWN operation"));
        }
    }
    Ok(result)
}

/// Called on the current transaction snapshot, never on a filtered operation list.
/// Approval/execute projections must retain active execute/reconcile jobs.
pub(crate) fn capture(d: &Value, item: &Value, requested: &Value) -> ApiResult<Value> {
    let requested = ids(requested)?;
    let binding = ConnectorBinding::from_json(&item["connectorBinding"])
        .map_err(|_| conflict("Current close recipient binding is invalid"))?;
    if !matches!(item["providerStatus"].as_str(), Some("new" | "inprogress"))
        || ["id", "objectId", "itemId", "conversationKey"]
            .iter()
            .any(|key| item[*key].as_str().is_none_or(str::is_empty))
    {
        return Err(conflict(
            "Preserving UNKNOWN replies requires a current open recipient",
        ));
    }
    let operations = d["operations"]
        .as_array()
        .ok_or_else(|| internal("Complete operation history required"))?;
    let jobs = if d.get("scopeOwners").is_some() {
        let controls = &d["activeExternalJobs"];
        if controls["version"] != 1 || controls["complete"] != true {
            return Err(internal("Complete active external job controls required"));
        }
        controls["jobs"]
            .as_array()
            .ok_or_else(|| internal("Complete active external job controls required"))?
    } else {
        d["jobs"]
            .as_array()
            .ok_or_else(|| internal("Current job history required"))?
    };
    let blockers: Vec<_> = operations
        .iter()
        .filter(|op| {
            same_recipient(op, item)
                && matches!(
                    op["status"].as_str(),
                    Some("dispatching" | "unknown" | "succeeded")
                )
        })
        .collect();
    if blockers.is_empty() || blockers.len() != requested.len() {
        return Err(conflict(
            "Preserved operations differ from complete recipient blockers",
        ));
    }
    let mut captured = Vec::new();
    let mut seen = BTreeSet::new();
    for op in blockers {
        let id = op["id"]
            .as_str()
            .filter(|id| requested.contains(*id))
            .ok_or_else(|| conflict("Unreviewed recipient operation blocks this close"))?;
        if !seen.insert(id.to_owned())
            || op["status"] != "unknown"
            || op["action"]["action"] != "reply_and_close"
            || op["action"]["actionId"] != op["id"]
            || op["action"]["objectId"] != item["objectId"]
            || op["action"]["itemId"] != item["itemId"]
            || op["action"]["conversationKey"] != item["conversationKey"]
            || op["target"]["objectId"] != item["objectId"]
            || op["target"]["itemId"] != item["itemId"]
            || op["target"]["conversationKey"] != item["conversationKey"]
            || op["target"]["connectorBinding"] != item["connectorBinding"]
            || ConnectorBinding::from_json(&op["target"]["connectorBinding"]).is_err()
            || op["executeReceipt"].is_null()
            || !op["evidence"].is_object()
            || op["approvalId"].as_str().is_none_or(str::is_empty)
        {
            return Err(conflict(
                "Only exact unresolved reply operations may be preserved",
            ));
        }
        if jobs.iter().any(|job| {
            matches!(
                job["status"].as_str(),
                Some("running" | "queued" | "pending")
            ) && ((job["kind"] == "execute" && job["refId"] == op["approvalId"])
                || (job["kind"] == "reconcile" && job["refId"] == op["id"]))
        }) {
            return Err(conflict(
                "Original reply execution/readback must finish before a distinct close",
            ));
        }
        captured.push(json!({"operationId":id,"operationSha256":digest(op),
            "executeReceiptSha256":digest(&op["executeReceipt"]),"evidenceSha256":digest(&op["evidence"])}));
    }
    if seen != requested {
        return Err(conflict("Preserved operation identities differ"));
    }
    captured.sort_by(|a, b| a["operationId"].as_str().cmp(&b["operationId"].as_str()));
    Ok(
        json!({"version":1,"intent":"close_without_reply_preserving_unknown",
        "account":binding.account_id,"routeTarget":route(item),"operations":captured}),
    )
}

pub(crate) fn validate(d: &Value, proposal: &Value, item: &Value) -> bool {
    if proposal["kind"] != "close" {
        return false;
    }
    let saved = &proposal["operatorCloseDecision"]["preservedUnknownReplies"];
    let Some(operations) = saved["operations"]
        .as_array()
        .filter(|rows| !rows.is_empty())
    else {
        return false;
    };
    let requested = json!(operations
        .iter()
        .map(|op| op["operationId"].clone())
        .collect::<Vec<_>>());
    capture(d, item, &requested).is_ok_and(|current| current == *saved)
}

/// This does not authenticate the decision: operator_close::current must pass first.
pub(crate) fn permits(d: &Value, prior: &Value, proposal: &Value, item: &Value) -> bool {
    validate(d, proposal, item)
        && proposal["operatorCloseDecision"]["preservedUnknownReplies"]["operations"]
            .as_array()
            .is_some_and(|rows| {
                rows.iter().any(|row| {
                    row["operationId"] == prior["id"] && row["operationSha256"] == digest(prior)
                })
            })
}

/// Dispatch already contains its newly admitted close. Exclude only that exact
/// canonical operation, never a competing close or another unresolved action.
/// The caller still authenticates operator_close::current and dispatch authority.
pub(crate) fn validate_for_operation(
    d: &Value,
    proposal: &Value,
    item: &Value,
    own: &Value,
) -> bool {
    let Some(operations) = d["operations"].as_array() else {
        return false;
    };
    let Some(id) = own["id"].as_str().filter(|id| !id.is_empty()) else {
        return false;
    };
    if own["status"] != "dispatching"
        || own["action"]["action"] != "close"
        || own["action"]["actionId"] != own["id"]
        || own["proposalId"].as_str().is_none_or(str::is_empty)
        || own["proposalId"] != proposal["id"]
        || own["approvalId"].as_str().is_none_or(str::is_empty)
        || proposal["operatorCloseDecision"]["decisionSha256"]
            .as_str()
            .is_none_or(|hash| {
                hash.len() != 64
                    || !hash
                        .bytes()
                        .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
            })
        || own["approvedOperatorCloseDecisionSha256"]
            != proposal["operatorCloseDecision"]["decisionSha256"]
        || own["itemId"] != item["id"]
        || own["action"]["objectId"] != item["objectId"]
        || own["action"]["itemId"] != item["itemId"]
        || own["target"]["connectorBinding"] != item["connectorBinding"]
        || operations.iter().filter(|op| op["id"] == id).count() != 1
        || operations.iter().find(|op| op["id"] == id) != Some(own)
    {
        return false;
    }
    // Do not clone source/media/history collections merely to omit our one op.
    let mut view = json!({"operations":operations.iter().filter(|op| op["id"] != id).collect::<Vec<_>>(),
        "jobs":d["jobs"]});
    if d.get("scopeOwners").is_some() {
        view["scopeOwners"] = json!({"partial":true});
        view["activeExternalJobs"] = d["activeExternalJobs"].clone();
    }
    validate(&view, proposal, item)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> (Value, Value) {
        let binding = json!({"id":"angryspace-baw-russia-v1","revision":1,"workspaceId":"local-pilot",
            "accountId":"BAW Russia","connector":"angryspace","providerAccountId":"baw-russia"});
        let item = json!({"id":"current","itemId":"target","objectId":"12182","conversationKey":"thread",
            "providerStatus":"new","connectorBinding":binding});
        let old = json!({"id":"old-reply","itemId":"current","approvalId":"old-approval","status":"unknown",
            "action":{"action":"reply_and_close","actionId":"old-reply","objectId":"12182","itemId":"target","conversationKey":"thread"},
            "target":item,"executeReceipt":{"mutationOutcome":"uncertain"},"evidence":{"verificationPhase":"unconfirmed"}});
        (json!({"operations":[old],"jobs":[]}), item)
    }
    fn proposal(d: &Value, item: &Value) -> Value {
        json!({"kind":"close","operatorCloseDecision":{"preservedUnknownReplies":capture(d,item,&json!(["old-reply"])).unwrap()}})
    }
    #[test]
    fn distinct_close_preserves_every_old_byte_and_only_allows_close() {
        let (d, item) = fixture();
        let before = d.clone();
        let mut p = proposal(&d, &item);
        assert!(permits(&d, &d["operations"][0], &p, &item));
        assert_eq!(d, before);
        for kind in ["reply_and_close", "hide", "delete"] {
            p["kind"] = json!(kind);
            assert!(!validate(&d, &p, &item));
        }
    }
    #[test]
    fn unresolved_nonreply_live_dispatch_and_completed_operation_never_permitted() {
        for status in ["dispatching", "succeeded"] {
            let (mut d, item) = fixture();
            d["operations"][0]["status"] = json!(status);
            assert!(capture(&d, &item, &json!(["old-reply"])).is_err());
        }
        for action in ["close", "hide", "delete"] {
            let (mut d, item) = fixture();
            d["operations"][0]["action"]["action"] = json!(action);
            assert!(capture(&d, &item, &json!(["old-reply"])).is_err());
        }
    }
    #[test]
    fn original_execution_and_reconciliation_quiescence_required() {
        for (kind, reference) in [("execute", "old-approval"), ("reconcile", "old-reply")] {
            for status in ["running", "queued", "pending"] {
                let (mut d, item) = fixture();
                d["jobs"] = json!([{"kind":kind,"refId":reference,"status":status}]);
                assert!(capture(&d, &item, &json!(["old-reply"])).is_err());
            }
        }
    }
    #[test]
    fn immutable_receipt_route_or_blocker_change_invalidates_decision() {
        let (d, item) = fixture();
        let p = proposal(&d, &item);
        for field in ["executeReceipt", "evidence"] {
            let mut changed = d.clone();
            changed["operations"][0][field]["changed"] = json!(true);
            assert!(!validate(&changed, &p, &item));
        }
        let mut changed = d.clone();
        let mut other = changed["operations"][0].clone();
        other["id"] = json!("another");
        changed["operations"].as_array_mut().unwrap().push(other);
        assert!(!validate(&changed, &p, &item));
        let mut moved = item.clone();
        moved["objectId"] = json!("12183");
        assert!(!validate(&d, &p, &moved));
        let mut closed = item.clone();
        closed["providerStatus"] = json!("closed");
        assert!(!validate(&d, &p, &closed));
    }
    #[test]
    fn alias_is_bound_foreign_company_ignored_malformed_alias_blocks() {
        let (mut d, item) = fixture();
        d["operations"][0]["itemId"] = json!("historical-alias");
        let p = proposal(&d, &item);
        assert!(validate(&d, &p, &item));
        d["operations"][0]["target"]["connectorBinding"] = json!({"accountId":"BAW Russia"});
        assert!(!validate(&d, &p, &item));
        d["operations"][0]["target"]["connectorBinding"] = json!({"accountId":"LikeAvto"});
        assert!(capture(&d, &item, &json!(["old-reply"])).is_err());
    }
    #[test]
    fn missing_duplicate_and_extra_requested_ids_fail_closed() {
        let (d, item) = fixture();
        for requested in [
            json!([]),
            json!(["old-reply", "old-reply"]),
            json!(["missing"]),
            json!(["old-reply", "missing"]),
        ] {
            assert!(capture(&d, &item, &requested).is_err());
        }
    }

    #[test]
    fn canonical_local_id_cannot_hide_contradictory_saved_recipient() {
        let (d, item) = fixture();
        for (section, field) in [
            ("target", "objectId"),
            ("target", "itemId"),
            ("target", "conversationKey"),
            ("action", "conversationKey"),
        ] {
            for replacement in [Value::Null, json!("different-recipient")] {
                let mut changed = d.clone();
                changed["operations"][0][section][field] = replacement;
                assert!(capture(&changed, &item, &json!(["old-reply"])).is_err());
            }
        }
        assert!(capture(&d, &item, &json!(["old-reply"])).is_ok());
    }

    #[test]
    fn dispatch_excludes_only_its_exact_canonical_new_close() {
        let (mut d, item) = fixture();
        let mut p = proposal(&d, &item);
        p["id"] = json!("new-proposal");
        p["operatorCloseDecision"]["decisionSha256"] = json!("a".repeat(64));
        let own = json!({"id":"new-close","proposalId":"new-proposal","itemId":"current","approvalId":"new-approval",
            "approvedOperatorCloseDecisionSha256":"a".repeat(64),
            "status":"dispatching","action":{"action":"close","actionId":"new-close","objectId":"12182","itemId":"target"},"target":item});
        d["operations"].as_array_mut().unwrap().push(own.clone());
        assert!(!validate(&d, &p, &item));
        assert!(validate_for_operation(&d, &p, &item, &own));
        let mut wrong_marker = own.clone();
        wrong_marker["approvedOperatorCloseDecisionSha256"] = json!("b".repeat(64));
        let mut wrong_view = d.clone();
        wrong_view["operations"][1] = wrong_marker.clone();
        assert!(!validate_for_operation(
            &wrong_view,
            &p,
            &item,
            &wrong_marker
        ));
        let mut wrong_target = own.clone();
        wrong_target["action"]["itemId"] = json!("other-target");
        wrong_view["operations"][1] = wrong_target.clone();
        assert!(!validate_for_operation(
            &wrong_view,
            &p,
            &item,
            &wrong_target
        ));
        let mut competing = own.clone();
        competing["id"] = json!("competing-close");
        competing["action"]["actionId"] = json!("competing-close");
        d["operations"].as_array_mut().unwrap().push(competing);
        assert!(!validate_for_operation(&d, &p, &item, &own));
        let mut forged = own.clone();
        forged["proposalId"] = json!("other-proposal");
        assert!(!validate_for_operation(&d, &p, &item, &forged));
    }

    #[test]
    fn scoped_missing_or_incomplete_job_controls_cannot_imply_quiescence() {
        let (mut d, item) = fixture();
        d["scopeOwners"] = json!({"version":1});
        assert!(capture(&d, &item, &json!(["old-reply"])).is_err());
        d["activeExternalJobs"] = json!({"version":1,"complete":false,"jobs":[]});
        assert!(capture(&d, &item, &json!(["old-reply"])).is_err());
        d["activeExternalJobs"]["complete"] = json!(true);
        assert!(capture(&d, &item, &json!(["old-reply"])).is_ok());
        d["activeExternalJobs"]["jobs"] =
            json!([{"kind":"reconcile","refId":"old-reply","status":"running"}]);
        assert!(capture(&d, &item, &json!(["old-reply"])).is_err());
    }
}
