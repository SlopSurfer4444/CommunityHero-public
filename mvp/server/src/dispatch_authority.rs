//! Bind review and execution to the same credential generation and recheck it
//! at each provider dispatch. A browser session is an admission mechanism, not
//! the lifetime of an admitted job. Expiry/logout does not cancel admitted work;
//! allow-list removal/rotation does. Readback is intentionally never gated here.
use crate::*;
use operator_auth::Actor;

fn denied() -> ApiError {
    ApiError(
        StatusCode::FORBIDDEN,
        "Operator authority changed or is unavailable; review and confirm again".into(),
    )
}

fn local(binding: &Value) -> bool {
    binding["actorId"] == "local-owner" && binding["kind"] == "local-owner"
}

pub(crate) fn approval_binding(actor: &Actor) -> Value {
    if actor.id == "local-owner" && actor.role == "owner" {
        json!({"kind":"local-owner","actorId":"local-owner"})
    } else {
        json!({"kind":"operator","actorId":actor.id,"generation":actor.authority_generation})
    }
}

/// Called inside the admission transaction; legacy remote approvals lack the
/// generation evidence and require a fresh review. Legacy local-owner approvals
/// retain compatibility without giving remote actors an ownership bypass.
pub(crate) fn admit(approval: &Value, actor: &Actor) -> ApiResult<Value> {
    check_approval_actor(approval, actor)?;
    let executing = approval_binding(actor);
    let approved = match approval.get("approvalAuthority") {
        Some(binding) => binding.clone(),
        None if local(&executing)
            && approval["approvedBy"]["id"]
                .as_str()
                .is_none_or(|id| id == "local-owner") =>
        {
            executing.clone()
        }
        None => return Err(denied()),
    };
    if approved != executing
        || (!local(&executing)
            && executing["generation"]
                .as_str()
                .is_none_or(|g| g.len() != 64))
    {
        return Err(denied());
    }
    Ok(json!({"approved":approved,"executed":executing}))
}

pub(crate) async fn check(app: &App, op: &Value) -> ApiResult<()> {
    let authority = &op["dispatchAuthority"];
    let approved = &authority["approved"];
    let executed = &authority["executed"];
    if approved != executed || !approved.is_object() {
        return Err(denied());
    }
    for (binding, attribution) in [(approved, &op["approvedBy"]), (executed, &op["executedBy"])] {
        let id = binding["actorId"].as_str().ok_or_else(denied)?;
        // Only old local approvals can legitimately lack approvedBy.
        if attribution["id"].as_str() != Some(id) && !(local(binding) && attribution.is_null()) {
            return Err(denied());
        }
    }
    if local(executed) {
        return Ok(());
    }
    if executed["kind"] != "operator" {
        return Err(denied());
    }
    let auth = app.auth.as_ref().ok_or_else(denied)?;
    let id = executed["actorId"].as_str().ok_or_else(denied)?;
    let generation = executed["generation"].as_str().ok_or_else(denied)?;
    match auth.authority_is_current(id, generation).await {
        Ok(true) => Ok(()),
        Ok(false) | Err(_) => Err(denied()),
    }
}

/// Use before the first context read and again after it, directly before execute.
/// A denied operation is terminal/stale, never UNKNOWN: no execute was attempted.
pub(crate) async fn permit(app: &App, op: &Value) -> ApiResult<bool> {
    if check(app, op).await.is_ok() {
        return Ok(true);
    }
    set_outcome(
        app,
        op,
        "stale",
        json!({
            "reason":"Operator authority revoked, rotated or unavailable; no external call",
            "authorityDenied":true
        }),
    )
    .await?;
    Ok(false)
}

pub(crate) fn sanitize_view(data: &mut Value) {
    for (collection, field) in [
        ("approvals", "approvalAuthority"),
        ("operations", "dispatchAuthority"),
    ] {
        if let Some(rows) = data[collection].as_array_mut() {
            for row in rows {
                if let Some(row) = row.as_object_mut() {
                    row.remove(field);
                }
            }
        }
    }
}

#[cfg(test)]
#[path = "dispatch_authority_tests.rs"]
mod tests;
