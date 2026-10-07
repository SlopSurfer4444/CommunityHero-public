//! Bind review and execution to the same credential generation and recheck it
//! at each provider dispatch. A browser session is an admission mechanism, not
//! the lifetime of an admitted job. Expiry/logout does not cancel admitted work;
//! allow-list removal/rotation does. Readback is intentionally never gated here.
use crate::*;
use operator_auth::Actor;
use std::collections::HashMap;
use std::sync::{Mutex as StateMutex, OnceLock};

#[derive(Default)]
struct HoldState { generation:u64, held:bool, exhausted:bool }
impl HoldState {
    fn hold(&mut self) {
        self.held=true;
        if let Some(next)=self.generation.checked_add(1) {self.generation=next;}
        else {self.exhausted=true;}
    }
}
static TRANSPORT_FAILURE_HOLDS: OnceLock<StateMutex<HashMap<String,HoldState>>> = OnceLock::new();
pub(crate) struct RecoveryHoldToken { key:String,generation:u64 }
fn hold_key(app:&App)->String {json!([app.data.to_string_lossy(),app.account.key()]).to_string()}
/// Immediate process-local stop when a real provider receipt cannot be safely
/// persisted/retired. The durable permit stays armed; this is no replay ledger.
pub(crate) fn hold_transport_failure(app:&App) {
    TRANSPORT_FAILURE_HOLDS.get_or_init(Default::default).lock().unwrap_or_else(|error|error.into_inner()).entry(hold_key(app)).or_default().hold();
}
pub(crate) fn require_unheld(app:&App)->ApiResult<()> {
    if TRANSPORT_FAILURE_HOLDS.get_or_init(Default::default).lock().unwrap_or_else(|error|error.into_inner()).get(&hold_key(app)).is_some_and(|state|state.held||state.exhausted) {
        return Err(conflict("Transport evidence recovery is required before new dispatch permits"));
    }
    Ok(())
}
/// Capture after this coordinator's close/drain, before the trusted inspection.
/// Every newly classified failure increments even an already-held generation.
pub(crate) fn capture_recovery_hold(app:&App)->ApiResult<RecoveryHoldToken> {
    let key=hold_key(app);
    let mut holds=TRANSPORT_FAILURE_HOLDS.get_or_init(Default::default).lock().unwrap_or_else(|error|error.into_inner());
    let state=holds.entry(key.clone()).or_default();
    if state.exhausted{return Err(conflict("Transport recovery generation exhausted; hold retained"));}
    Ok(RecoveryHoldToken{key,generation:state.generation})
}
/// Only the configured case coordinator calls this after verified durable
/// reopen. A later failure wins, including one racing with protected readback.
pub(crate) fn clear_after_recovery_admission(app:&App,token:&RecoveryHoldToken)->ApiResult<()> {
    let key=hold_key(app);
    let mut holds=TRANSPORT_FAILURE_HOLDS.get_or_init(Default::default).lock().unwrap_or_else(|error|error.into_inner());
    let state=holds.get_mut(&key).filter(|state|token.key==key&&!state.exhausted&&state.generation==token.generation)
        .ok_or_else(||conflict("Transport failure changed during recovery admission; hold retained"))?;
    state.held=false;Ok(())
}

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
    media_context_gate::check_dispatch_authority(app, op).await?;
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
    // Stop the next pre-context read as soon as the shared failure is observed;
    // no new provider-facing work is useful while its recovery is held.
    require_unheld(app)?;
    if check(app, op).await.is_ok() && conductor_authority::check_dispatch(app,op).await.is_ok() {
        return Ok(true);
    }
    set_outcome(
        app,
        op,
        "stale",
        json!({
            "reason":"Execution authority paused, revoked, rotated or unavailable; no external execute call",
            "authorityDenied":true,"providerCallAttempted":false
        }),
    )
    .await?;
    Ok(false)
}

/// The conductor read guard is transient; the operation-linked durable permit
/// remains provider-capable through queue/network cancellation until a positive
/// root-owned transport witness retires it. Drop never means no-attempt.
pub(crate) struct Permit {
    _guard: Option<tokio::sync::OwnedRwLockReadGuard<()>>,
    identity: Value,
}
impl Permit {
    pub(crate) fn identity(&self) -> &Value { &self.identity }
    /// Call after the bridge await, before acquiring M for settlement. This
    /// releases the OLD conductor read guard but does not retire durable intent.
    pub(crate) fn release(self) -> Value { self.identity.clone() }
}
pub(crate) async fn begin(app: &App, op: &Value) -> ApiResult<Option<Permit>> {
    let _company = connection_gate::lock(app).await;
    require_unheld(app)?;
    let guard=match op["conductorRunId"].as_str() {
        Some(run)=>Some(conductor_authority::dispatch_guard(app,run).await),
        None=>None,
    };
    if !permit(app,op).await? { return Ok(None); }
    let identity = app.change_connection_gate(connection_gate::Scope::Operation(op),
        |d| {require_unheld(app)?;connection_gate::prearm(d,op)}).await?;
    Ok(Some(Permit {_guard:guard,identity}))
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
pub(crate) mod tests;
