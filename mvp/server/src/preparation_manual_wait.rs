//! Bounded, in-process waiting for an existing unpaid preparation capture.
//! A durable wait record is observation only, never restart/paid-call authority.
use crate::{ApiResult, App, conflict, row, row_mut};
use crate::runtime_lifecycle::{AdmissionClass, OwnerToken};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::time::Duration;
use tokio::time::Instant;

const MAX_WAIT: Duration = Duration::from_secs(120);
const POLL: Duration = Duration::from_secs(1);
type Preflight = fn(&Value, &str) -> ApiResult<()>;

/// Created only by the currently running worker; never loaded from saved JSON.
pub(crate) struct Wait {
    token: OwnerToken,
    limit: Duration,
    started: Option<(Instant, String, String, String)>,
    recorded: Option<(String, String, String)>,
}
impl Wait {
    pub(crate) fn new(token: OwnerToken) -> Self {
        Self { token, limit: MAX_WAIT, started: None, recorded: None }
    }
    pub(crate) fn admission_race(&mut self, request: &Value) { self.start(request); }
    fn start(&mut self, request: &Value) {
        if self.started.is_none() {
            let at = chrono::Utc::now();
            let deadline = at + chrono::Duration::milliseconds(self.limit.as_millis() as i64);
            self.started = Some((Instant::now(), at.to_rfc3339(), deadline.to_rfc3339(), hash(request)));
        }
    }
    fn remaining(&self) -> Duration {
        self.started.as_ref().map_or(self.limit, |s| self.limit.saturating_sub(s.0.elapsed()))
    }
}
fn hash(value: &Value) -> String { format!("{:x}", Sha256::digest(value.to_string().as_bytes())) }
fn owner(token: &OwnerToken) -> Value {
    json!({"account":token.account,"runtimeId":token.runtime_id,"releaseSha256":token.release_sha256,"epoch":token.epoch})
}
fn unpaid(d: &Value, run: &str, request: &Value, token: &OwnerToken, preflight: Preflight) -> ApiResult<()> {
    crate::runtime_lifecycle::require_admission(d, token, AdmissionClass::Preparation)?;
    let job = row(d, "jobs", run)?;
    let stages = &job["preparationStages"];
    let initial = &stages["initialAdmission"];
    let empty = |key: &str| job.get(key).is_none_or(|v| v.as_array().is_some_and(Vec::is_empty));
    if job["kind"] != "assistant" || job["status"] != "running"
        || !matches!(job["purpose"].as_str(), Some("engine_prepare"|"auto_prepare"|"auto_revalidate"))
        || job["prepareBundle"]["request"] != *request || job["prepareBundle"]["digest"] != hash(request)
        || !stages["first"].is_null() || !stages["review"].is_null()
        || ["firstAdmission", "reviewChunks", "answeringRepairs", "repairBudget"].iter().any(|k| stages.get(*k).is_some())
        || !empty("retainedEvidence") || !empty("modelMaterialReceipts")
        || job.get("scopeFailure").is_some() || !job["prepareOutcome"].is_null() || !job["result"].is_null()
        || initial.as_object().is_none_or(|o| o.len()!=5)
        || initial["version"] != 1 || initial["status"] != "scheduled" || initial["owner"] != owner(token)
        || initial["requestSha256"] != hash(request) || !initial["admittedAt"].is_string() {
        return Err(conflict("Manual material wait requires the same running unpaid first capture"));
    }
    preflight(d, run)?;
    crate::prepare_bundle::current(d, &job["prepareBundle"]).map_err(conflict)?;
    crate::preparation_unit::current_bundle(d, &job["prepareBundle"], &crate::now()).map_err(conflict)?;
    crate::fact_followup::automatic_continuation_current(d, job)
}
async fn record(app: &App, run: &str, request: &Value, wait: &mut Wait, preflight: Preflight,
    status: &str, reason: &str) -> ApiResult<()> {
    let Some((_, started, deadline, original)) = &wait.started else { return Ok(()); };
    let transition=(status.to_owned(),reason.to_owned(),hash(request));
    if wait.recorded.as_ref()==Some(&transition) {return Ok(());}
    app.change(|d| {
        unpaid(d, run, request, &wait.token, preflight)?;
        row_mut(d, "jobs", run)?["preparationStages"]["manualMaterialWait"] = json!({
            "version":1,"status":status,"reasonCode":reason,"owner":owner(&wait.token),
            "originalRequestSha256":original,"requestSha256":hash(request),
            "startedAt":started,"deadlineAt":deadline,"updatedAt":crate::now(),"resumeAuthorized":false});
        Ok(())
    }).await?;
    wait.recorded=Some(transition);Ok(())
}
fn observation(d:&Value)->String {
    // This is only a retry throttle, never scope/admission authority. Including
    // all manual jobs is conservative: unrelated changes may cause an extra
    // guarded refresh but can neither hide relevant work nor authorize FIRST.
    hash(&json!(d["jobs"].as_array().into_iter().flatten()
        .filter(|j|j["purpose"]=="manual_video_frames")
        .map(|j|json!([j["id"],j["status"],j["frameResult"]])).collect::<Vec<_>>()))
}
async fn pause(app:&App,run:&str,request:&Value,wait:&mut Wait,preflight:Preflight,reason:&str)->ApiResult<()> {
    wait.start(request);
    record(app,run,request,wait,preflight,"waiting",reason).await?;
    // A wake is an optimization; finite polling covers coalesced/lost wakes.
    tokio::select! {
        _ = app.preparation_wake.notified() => {},
        _ = tokio::time::sleep(wait.remaining().min(POLL)) => {},
    }
    Ok(())
}
/// All acquisition and waiting happens outside the writer and worker lease.
/// Each material refresh is a short ordinary transaction; pending work rolls it
/// back so multiple exact-parent requests keep their common original digest.
pub(crate) async fn acquire(app: &App, run: &str, request: &Value, wait: &mut Wait,
    preflight: Preflight) -> ApiResult<Value> {
    if crate::runtime_lifecycle_app::current_job().as_deref()!=Some(run) {
        return Err(conflict("Manual material wait lacks native task ownership"));
    }
    let snapshot = app.db.read_preparation_context(run).await?;
    unpaid(&snapshot, run, request, &wait.token, preflight)?;
    let job = row(&snapshot, "jobs", run)?;
    let ids = job["prepareBundle"]["itemIds"].as_array().cloned()
        .ok_or_else(|| conflict("Preparation recipients are missing"))?;
    let photos = request["materialReadiness"]["requirements"].as_array().into_iter().flatten()
        .any(|r| r["kind"] == "post_photo");
    drop(snapshot);
    if photos { crate::photo_acquisition::ensure_for_preparation(app, run, &ids).await?; }
    let mut last_attempt=None;
    loop {
        // The same deadline survives wakeups and a FIRST-admission race.
        if wait.started.is_some() && wait.remaining().is_zero() {
            record(app, run, request, wait, preflight, "held", "manual_frame_wait_timeout").await?;
            return Err(conflict("manual_frame_wait_timeout"));
        }
        // Poll through the bounded read projection. Still-running manual work
        // must not load a full workspace into the writer every second.
        let snapshot=app.db.read_preparation_context(run).await?;
        unpaid(&snapshot,run,request,&wait.token,preflight)?;
        let parent=row(&snapshot,"jobs",run)?;
        let reason=crate::manual_frame_request::require_no_pending(&snapshot,parent).err();
        let classification=crate::manual_frame_request::wait_classification(&snapshot,parent);
        let observed=observation(&snapshot);drop(snapshot);
        if classification=="terminal" {
            wait.start(request);
            record(app,run,request,wait,preflight,"held","manual_frame_wait_terminal").await?;
            return Err(conflict("manual_frame_wait_terminal"));
        }
        if let Some(reason)=reason {
            if reason=="manual_frame_requested_material_unresolved"||last_attempt.as_ref()==Some(&observed) {
                pause(app,run,request,wait,preflight,reason).await?;
                continue;
            }
        }
        last_attempt=Some(observed);
        crate::manual_frame_request::refresh(app).await?;
        let refreshed = app.change(|d| {
            unpaid(d, run, request, &wait.token, preflight)?;
            if wait.started.is_some()&&wait.remaining().is_zero(){return Err(conflict("manual_frame_wait_timeout"));}
            let request = super::refresh_unpaid_materials(d, run, request)?;
            crate::manual_frame_request::require_no_pending(d, row(d, "jobs", run)?).map_err(conflict)?;
            Ok(request)
        }).await;
        match refreshed {
            Ok(request) => {
                record(app, run, &request, wait, preflight, "ready", "manual_frames_captured").await?;
                return Ok(request);
            }
            Err(error) if crate::manual_frame_request::is_wait_reason(&error.1) => {
                pause(app,run,request,wait,preflight,&error.1).await?;
            }
            Err(error) if error.1=="manual_frame_wait_timeout" => {
                record(app,run,request,wait,preflight,"held",&error.1).await?;
                return Err(error);
            }
            Err(error) => return Err(error),
        }
    }
}

#[cfg(test)]
#[path="preparation_manual_wait_tests.rs"]
mod tests;
