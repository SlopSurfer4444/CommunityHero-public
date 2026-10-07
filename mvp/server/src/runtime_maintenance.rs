//! Runtime coordinator, called by authenticated native owner maintenance routes.
//! Durable fences precede retirement; no abort, retry, process stop or startup.
use crate::{App, ApiResult, Value, conflict};
use crate::runtime_lifecycle::{self, AdmittedTarget, OwnerToken, SettledNative};
use crate::provider_session::{ProviderDrainPhase, ProviderDrainToken};
use serde_json::json;

pub(crate) async fn admission_token(app:&App,class:runtime_lifecycle::AdmissionClass)->ApiResult<OwnerToken> {
    let metadata=app.db.read_metadata().await?;
    runtime_lifecycle::bound_admission_token(&metadata,&app.lifecycle_owner,class)
}
pub(crate) async fn status(app:&App)->ApiResult<Value> {
    let metadata=app.db.read_metadata().await?;
    runtime_lifecycle::current_owner(&metadata,&app.lifecycle_owner)?;
    let tasks=app.tasks.lock().await.len().max(app.lifecycle_task_count.load(std::sync::atomic::Ordering::SeqCst));
    let native=app.lifecycle_work.snapshot()?;
    let provider=app.provider_session.drain_status();
    Ok(json!({"schemaVersion":1,"kind":"native-runtime-maintenance","lifecycle":runtime_lifecycle::status(&metadata)?,
        "applicationTasks":tasks,"nativeActive":native.active,"nativeUnresolved":native.unresolved,"credentialWriters":native.credential_writers,
        "provider":provider.as_json(),"stopAuthorized":false}))
}
/// Target MUST come from the ROOT-admitted complete release closure; not caller
/// supplied capability flags. ExpectedOwner is the operator's exact precondition.
pub(crate) async fn begin(app:&App,expected:&OwnerToken,target:&AdmittedTarget,attempt:&str)->ApiResult<Value> {
    runtime_lifecycle::require_runtime_owner(expected,&app.lifecycle_owner)?;
    let mut session_token=app.lifecycle_provider_token.lock().await;
    let drain=app.change_runtime_lifecycle_with_ledger(|d|runtime_lifecycle::begin_drain_for_release(d,expected,target,attempt)).await?;
    // If retirement itself fails, durable phase stays Draining. Never reopen.
    let native=app.lifecycle_work.close()?;
    let provider=app.provider_session.begin_drain()?;
    *session_token=Some((drain.clone(),provider,native));
    Ok(json!({"lifecycle":app.db.read_runtime_lifecycle().await?,"provider":app.provider_session.drain_status().as_json(),"stopAuthorized":false}))
}
fn require_provider_token(expected:&OwnerToken,current:&Option<(OwnerToken,ProviderDrainToken,crate::runtime_owned_work::DrainToken)>,token:&ProviderDrainToken)->ApiResult<()> {
    if !current.as_ref().is_some_and(|(owner,bound,_)|owner==expected&&bound==token) {
        return Err(conflict("Native provider drain token not bound to durable owner"));
    }Ok(())
}
/// Polling does not abort anything. Caller retries observations, never effects.
/// Tree containment includes credential descendants inside the owned worker tree.
/// Other future credential backends must supply their own actual settled witness
/// before setting this count to zero; current transport owns only that tree.
pub(crate) async fn checkpoint(app:&App,expected:&OwnerToken)->ApiResult<Value> {
    runtime_lifecycle::require_runtime_owner(expected,&app.lifecycle_owner)?;
    let session_token=app.lifecycle_provider_token.lock().await;
    let provider=app.provider_session.drain_status();
    require_provider_token(expected,&session_token,&provider.token)?;
    if provider.phase!=ProviderDrainPhase::Drained||!provider.worker_retired||!provider.containment {
        return Err(conflict("Native provider retirement unresolved; release checkpoint blocked"));
    }
    let tasks=app.tasks.lock().await;
    let owned=app.lifecycle_work.snapshot()?;
    if !owned.closed {return Err(conflict("Native work admission is not closed"));}
    let native=SettledNative{owner:expected.clone(),application_tasks:tasks.len().max(app.lifecycle_task_count.load(std::sync::atomic::Ordering::SeqCst))+owned.active,provider_queued:provider.queued,
        provider_dispatched:provider.dispatched,provider_contained:provider.containment,
        credential_writers:owned.credential_writers,unresolved_effects:owned.unresolved+usize::from(provider.unresolved_stage.is_some())};
    app.change_runtime_lifecycle_with_ledger(|d|runtime_lifecycle::mark_drained(d,expected,&native)).await
}
pub(crate) async fn commit_stop_checkpoint(app:&App,expected:&OwnerToken,transfer:&Value)->ApiResult<()> {
    runtime_lifecycle::require_runtime_owner(expected,&app.lifecycle_owner)?;
    app.change_runtime_lifecycle_with_ledger(|d|runtime_lifecycle::commit_stop_checkpoint(d,expected,transfer)).await
}
pub(crate) async fn resume(app:&App,expected:&OwnerToken)->ApiResult<OwnerToken> {
    runtime_lifecycle::require_runtime_owner(expected,&app.lifecycle_owner)?;
    let mut session_token=app.lifecycle_provider_token.lock().await;
    let provider=app.provider_session.drain_status();require_provider_token(expected,&session_token,&provider.token)?;
    if provider.phase!=ProviderDrainPhase::Drained||!provider.worker_retired||!provider.containment {
        return Err(conflict("Native provider retirement unresolved; resume blocked"));
    }
    let tasks=app.tasks.lock().await;
    let owned=app.lifecycle_work.snapshot()?;
    if !owned.closed||owned.active!=0||owned.unresolved!=0 {return Err(conflict("Native work unresolved; resume blocked"));}
    let native=SettledNative{owner:expected.clone(),application_tasks:tasks.len().max(app.lifecycle_task_count.load(std::sync::atomic::Ordering::SeqCst)),provider_queued:provider.queued,
        provider_dispatched:provider.dispatched,provider_contained:provider.containment,credential_writers:owned.credential_writers,unresolved_effects:owned.unresolved+usize::from(provider.unresolved_stage.is_some())};
    // Durable admission opens LAST. If it fails after native session resume,
    // ordinary guarded claims remain closed; another begin is still required.
    let result=async {
        app.provider_session.resume(&provider.token)?;
        let work_token=&session_token.as_ref().ok_or_else(||conflict("Missing native drain binding"))?.2;
        app.lifecycle_work.resume(work_token)?;
        app.change_runtime_lifecycle_with_ledger(|d|runtime_lifecycle::resume_same_owner(d,expected,&native)).await
    }.await;
    if result.is_ok(){*session_token=None;}else{
        // A new native epoch MUST replace the previous binding. Otherwise the
        // durable drain could never recover from a failed resume transaction.
        *session_token=None;
        if let (Ok(native),Ok(provider))=(app.lifecycle_work.close(),app.provider_session.begin_drain()) {
            *session_token=Some((expected.clone(),provider,native));
        }
    }
    result
}
