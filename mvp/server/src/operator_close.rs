//! A fresh authenticated owner's empty CLOSE is an inbox decision, not a
//! claim about video content. Only that server-classified proposal may omit
//! audio/visual preparation; ordinary context, authority and ledger gates stay.
use crate::*;
use sha2::{Digest, Sha256};

pub(crate) fn requested(body: &Value, generated: bool) -> bool {
    !generated
        && body["kind"] == "close"
        && body["text"].as_str() == Some("")
        && body["_verifiedActor"]["id"] == "local-owner"
        && body["_verifiedActor"]["role"] == "owner"
}
fn digest(source: &Value) -> String {
    let mut unsigned = source.clone();
    unsigned.as_object_mut().unwrap().remove("decisionSha256");
    format!("{:x}", Sha256::digest(unsigned.to_string().as_bytes()))
}
fn prior_origin_digest(p: &Value) -> String {
    format!(
        "{:x}",
        Sha256::digest(p["priorPreparationOrigin"].to_string().as_bytes())
    )
}
pub(crate) fn capture_preserved(d: &Value, body: &Value, item: &Value) -> ApiResult<Value> {
    if body.get("closePreserveUnknownReplies").is_some() {
        unknown_reply_close::capture(d, item, &body["closePreserveUnknownReplies"])
    } else {
        Ok(Value::Null)
    }
}
pub(crate) fn classify(d: &Value, body: &Value, p: &Value, preserved: Value) -> Value {
    let mut source = json!({"version":1,"kind":"authenticated_operator_close",
        "selectedBy":body["_verifiedActor"],"account":d["account"],
        "itemId":p["itemId"],"itemRevision":p["itemRevision"],
        "proposalRevision":p["revision"],"reviewContextDigest":p["reviewContextDigest"],
        "priorPreparationOriginSha256":prior_origin_digest(p),
        "routeTarget":p["routeTarget"],"preservedUnknownReplies":preserved,
        "reason":body["reason"].as_str().filter(|reason| !reason.trim().is_empty() && reason.len()<=2000)
            .unwrap_or("Authenticated owner explicitly selected close without a reply"),
        "selectedAt":now()});
    source["decisionSha256"] = json!(digest(&source));
    source
}
pub(crate) fn current(d: &Value, p: &Value, item: &Value) -> ApiResult<bool> {
    current_for_operation(d, p, item, None)
}
pub(crate) fn current_for_operation(
    d: &Value,
    p: &Value,
    item: &Value,
    own: Option<&Value>,
) -> ApiResult<bool> {
    if let Some(op) = own {
        if op.get("approvedOperatorCloseDecisionSha256").is_some()
            && op["approvedOperatorCloseDecisionSha256"]
                != p["operatorCloseDecision"]["decisionSha256"]
        {
            return Err(conflict("Approved operator close decision changed"));
        }
        if p.get("operatorCloseDecision").is_some()
            && op["approvedOperatorCloseDecisionSha256"]
                != p["operatorCloseDecision"]["decisionSha256"]
        {
            return Err(conflict(
                "Operation lacks the exact approved operator close decision",
            ));
        }
    }
    let Some(source) = p.get("operatorCloseDecision") else {
        return Ok(false);
    };
    if !source.is_object()
        || p["kind"] != "close"
        || p["text"].as_str() != Some("")
        || source["version"] != 1
        || source["kind"] != "authenticated_operator_close"
        || source["selectedBy"]["id"] != "local-owner"
        || source["selectedBy"]["role"] != "owner"
        || source["account"] != d["account"]
        || source["itemId"] != item["id"]
        || source["itemRevision"] != p["itemRevision"]
        || source["itemRevision"] != item["revision"]
        || source["proposalRevision"] != p["revision"]
        || source["routeTarget"] != p["routeTarget"]
        || source["reviewContextDigest"] != p["reviewContextDigest"]
        || source["priorPreparationOriginSha256"] != prior_origin_digest(p)
        || source["decisionSha256"] != digest(source)
    {
        return Err(conflict("Authenticated operator close decision changed"));
    }
    if !source["preservedUnknownReplies"].is_null() {
        let preserved_current = match own {
            Some(op) => unknown_reply_close::validate_for_operation(d, p, item, op),
            None => unknown_reply_close::validate(d, p, item),
        };
        if !preserved_current {
            return Err(conflict(
                "Preserved UNKNOWN reply operations or active work changed",
            ));
        }
    }
    Ok(true)
}
pub(crate) fn assert_actor(p: &Value, actor: &operator_auth::Actor) -> ApiResult<()> {
    if p.get("operatorCloseDecision").is_some()
        && (actor.id != "local-owner" || actor.role != "owner")
    {
        return Err(ApiError(
            StatusCode::FORBIDDEN,
            "Authenticated operator close requires current owner authority".into(),
        ));
    }
    Ok(())
}

/// Preserve an already-current server-classified CLOSE across explicit same-ID
/// contract adoption. No ordinary/paid action can gain this exception here.
pub(crate) fn rebind_existing_for_revalidation(d:&Value,p:&Value,item:&Value,actor:&operator_auth::Actor,next_revision:u64)->ApiResult<Value>{
    assert_actor(p,actor)?;
    if actor.id!="local-owner"||actor.role!="owner"||!current(d,p,item)?
        ||p["revision"].as_u64().and_then(|n|n.checked_add(1))!=Some(next_revision)
        ||!p["operatorCloseDecision"]["preservedUnknownReplies"].is_null(){
        return Err(conflict("Revalidation cannot create or broaden owner CLOSE authority"));
    }
    assert_preparation(d,p)?;
    let mut source=p["operatorCloseDecision"].clone();
    source["proposalRevision"]=json!(next_revision);source["decisionSha256"]=json!(digest(&source));
    Ok(source)
}
pub(crate) fn assert_preparation(d: &Value, p: &Value) -> ApiResult<()> {
    if p.get("operatorCloseDecision").is_some() {
        preparation_reservations::assert_operator_close(d, p)
    } else {
        preparation_reservations::assert_proposal(d, p)
    }
}

#[cfg(test)]
#[path = "operator_close_tests.rs"]
mod tests;
