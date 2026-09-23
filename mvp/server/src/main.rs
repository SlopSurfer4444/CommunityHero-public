use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, Path, Request, State},
    http::{Method, StatusCode},
    middleware::{self, Next},
    response::{
        IntoResponse, Response,
        sse::{Event, KeepAlive, Sse},
    },
    routing::{get, patch, post},
};
use serde_json::{Value, json};
use sqlx::{
    SqlitePool,
    sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions},
};
use std::{collections::HashMap, convert::Infallible, path::PathBuf, sync::Arc, time::Duration};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    process::Command,
    sync::{Mutex, broadcast},
};
use tower_http::{compression::CompressionLayer, services::{ServeDir, ServeFile}};
mod connectors;
mod accounts;
mod engine_api;
mod engine_prepare;
mod storage;
mod performance;
mod writer_gate;
mod db_guards;
mod bootstrap_cache;
mod workspace_delta;
mod workspace_version;
mod readiness;
mod worker_supervision;
#[cfg(test)] mod worker_supervision_fault_tests;
mod sync_scan;
mod fast_status;
mod snapshot_order;
mod archive_import;
mod brand_repair;
mod thread_graph;
mod prepare_bundle;
mod auto_prepare;
mod preparation_review;
mod preparation_restart;
mod research_cache;
mod customer_case_context;
mod knowledge;
mod company_knowledge_cli;
mod media_queue;
mod media_processing;
mod media_visual;
mod media_artifacts;
mod media_frame_contract;
mod media_fullframes;
mod media_frame_decoder;
mod media_frame_selection;
#[cfg(test)] mod media_persistence_tests;
#[cfg(test)] mod media_restored_acceptance_tests;
mod feedback;
mod feedback_reporting;
mod operator_auth;
mod dispatch_authority;
mod dispatch_evidence;
mod dispatch_wave;
mod operator_http;
mod assistant_context;
mod assistant_dialogue;
mod assistant_tools;
mod assistant_action_review;
#[cfg(test)] mod operator_http_tests;
#[cfg(test)] mod finish_storage_tests;
#[cfg(test)] mod preparation_profile_tests;
use connectors::{ApprovedRoute, Capabilities, ConnectorBinding, ResourceRef, legacy_actions};
use storage::Database;

// The old local database has one known account. This is a compatibility mapping,
// never a fallback for an unknown connection or for another account.
fn legacy_binding() -> Value {
    accounts::Profile::LikeAvto.binding()
}
fn active_binding(d: &Value) -> ApiResult<ConnectorBinding> {
    let profile = accounts::Profile::from_workspace(d)?;
    let value = if d.get("connectorBinding").is_none() {
        if profile != accounts::Profile::LikeAvto {
            return Err(conflict("Account requires an explicit connector binding"));
        }
        legacy_binding()
    } else {
        d["connectorBinding"].clone()
    };
    let binding = ConnectorBinding::from_json(&value).map_err(|e| conflict(e.0))?;
    binding
        .validate_scope("local-pilot", profile.display())
        .map_err(|e| conflict(e.0))?;
    Ok(binding)
}
fn bridge_account(binding: &ConnectorBinding) -> ApiResult<&'static str> {
    for profile in [accounts::Profile::LikeAvto, accounts::Profile::BawRussia] {
        if binding.to_json() == profile.binding() { return Ok(profile.key()); }
    }
    Err(conflict("Connector is not implemented or its configuration changed"))
}
fn bound_item(binding: &ConnectorBinding, item: &Value) -> ApiResult<Value> {
    let mut target = item.clone();
    if target.get("connectorBinding").is_none() && bridge_account(binding).is_ok() {
        target["connectorBinding"] = binding.to_json();
    }
    ResourceRef::from_item(binding, &target).map_err(|e| conflict(e.0))?;
    Ok(target)
}
fn validate_route(p: &Value, binding: &ConnectorBinding, item: &Value) -> ApiResult<()> {
    bridge_account(binding)?;
    let approved_binding = ConnectorBinding::from_json(&p["routeTarget"]["connectorBinding"])
        .map_err(|_| conflict("Proposal needs a new connector-bound review"))?;
    let approved =
        ResourceRef::from_item(&approved_binding, &p["routeTarget"]).map_err(|e| conflict(e.0))?;
    let current = ResourceRef::from_item(binding, item).map_err(|e| conflict(e.0))?;
    let actions = legacy_actions(required(p, "kind")?, p["text"].as_str().unwrap_or(""))
        .map_err(|e| conflict(e.0))?;
    ApprovedRoute::new(approved, actions)
        .and_then(|r| r.validate_dispatch(binding, &current,
            &Capabilities::angryspace_for_platform(item["platform"].as_str().unwrap_or(""))))
        .map_err(|e| conflict(e.0))
}
fn operation_account(op: &Value) -> ApiResult<&'static str> {
    // Old UNKNOWN operations need explicit routing migration, not guessed dispatch.
    let binding = ConnectorBinding::from_json(&op["target"]["connectorBinding"])
        .map_err(|_| conflict("Legacy operation needs routing reconciliation"))?;
    ResourceRef::from_item(&binding, &op["target"]).map_err(|e| conflict(e.0))?;
    bridge_account(&binding)
}
fn outcome_matches_item(d: &Value, op: &Value) -> bool {
    let check = || -> ApiResult<bool> {
        let binding = active_binding(d)?;
        let current = bound_item(&binding, row(d, "items", required(op, "itemId")?)?)?;
        let original_binding = ConnectorBinding::from_json(&op["target"]["connectorBinding"])
            .map_err(|e| conflict(e.0))?;
        let original =
            ResourceRef::from_item(&original_binding, &op["target"]).map_err(|e| conflict(e.0))?;
        let target = ResourceRef::from_item(&binding, &current).map_err(|e| conflict(e.0))?;
        Ok(original == target)
    };
    check().unwrap_or(false)
}

#[cfg(test)]
mod tests;

type ApiResult<T> = Result<T, ApiError>;
#[cfg(windows)]
struct ProcessTree(usize);
#[cfg(windows)]
impl ProcessTree {
    fn attach(child: &tokio::process::Child) -> ApiResult<Self> {
        use windows_sys::Win32::System::JobObjects::*;
        unsafe {
            let handle = CreateJobObjectW(std::ptr::null(), std::ptr::null());
            if handle.is_null() {
                return Err(internal("Process containment unavailable"));
            }
            let guard = Self(handle as usize);
            let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
            info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            if SetInformationJobObject(
                handle,
                JobObjectExtendedLimitInformation,
                &info as *const _ as *const _,
                std::mem::size_of_val(&info) as u32,
            ) == 0
                || AssignProcessToJobObject(
                    handle,
                    child
                        .raw_handle()
                        .ok_or_else(|| internal("Process handle unavailable"))?
                        as _,
                ) == 0
            {
                return Err(internal("Process containment failed"));
            }
            Ok(guard)
        }
    }
}
#[cfg(windows)]
impl Drop for ProcessTree {
    fn drop(&mut self) {
        unsafe {
            windows_sys::Win32::Foundation::CloseHandle(self.0 as _);
        }
    }
}
#[derive(Debug)]
struct ApiError(StatusCode, String);
impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.0, Json(json!({"error":self.1}))).into_response()
    }
}
impl From<sqlx::Error> for ApiError {
    fn from(error: sqlx::Error) -> Self {
        // Never format the error: database details can include SQL values,
        // user content or credentials. Only a closed set of categories is logged.
        let category = match error {
            sqlx::Error::PoolTimedOut => "pool_timeout",
            sqlx::Error::PoolClosed => "pool_closed",
            sqlx::Error::Database(_) => "database",
            sqlx::Error::Io(_) => "io",
            sqlx::Error::Tls(_) => "tls",
            sqlx::Error::Protocol(_) => "protocol",
            sqlx::Error::RowNotFound => "row_not_found",
            _ => "other",
        };
        eprintln!("database_operation_failed category={category}");
        internal("Database operation failed")
    }
}
fn bad(s: &str) -> ApiError {
    ApiError(StatusCode::BAD_REQUEST, s.into())
}
fn conflict(s: &str) -> ApiError {
    ApiError(StatusCode::CONFLICT, s.into())
}
fn internal(s: &str) -> ApiError {
    ApiError(StatusCode::INTERNAL_SERVER_ERROR, s.into())
}
fn id() -> String {
    uuid::Uuid::new_v4().to_string()
}
fn now() -> String {
    chrono::Utc::now().to_rfc3339()
}
fn list<'a>(d: &'a Value, k: &str) -> &'a Vec<Value> {
    d[k].as_array().expect("workspace array")
}
fn list_mut<'a>(d: &'a mut Value, k: &str) -> &'a mut Vec<Value> {
    d[k].as_array_mut().expect("workspace array")
}
fn row<'a>(d: &'a Value, k: &str, id: &str) -> ApiResult<&'a Value> {
    list(d, k)
        .iter()
        .find(|v| v["id"] == id)
        .ok_or_else(|| ApiError(StatusCode::NOT_FOUND, format!("{k} record not found")))
}
fn row_mut<'a>(d: &'a mut Value, k: &str, id: &str) -> ApiResult<&'a mut Value> {
    list_mut(d, k)
        .iter_mut()
        .find(|v| v["id"] == id)
        .ok_or_else(|| ApiError(StatusCode::NOT_FOUND, format!("{k} record not found")))
}
fn required<'a>(v: &'a Value, k: &str) -> ApiResult<&'a str> {
    v[k].as_str()
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| bad(&format!("Missing {k}")))
}
fn check_revision(v: &Value, expected: &Value) -> ApiResult<()> {
    if !expected.is_u64() || v["revision"] != *expected {
        Err(conflict("Revision changed; review current content"))
    } else {
        Ok(())
    }
}
fn bump(v: &mut Value) {
    v["revision"] = json!(v["revision"].as_u64().unwrap_or(0) + 1);
}
fn audit(d: &mut Value, action: &str, ref_id: &str) {
    list_mut(d, "audit").push(json!({"id":id(),"action":action,"refId":ref_id,"createdAt":now()}));
}
fn empty() -> Value {
    json!({"account":"LikeAvto","items":[],"posts":[],"branches":[],"conversations":[],"proposals":[],"approvals":[],"operations":[],"materials":[],"jobs":[],"audit":[],"settings":{"provider":"LikeAvto","assistant":"local-codex","externalWrites":"explicit-confirmation","concurrencyBoundary":"Use LikeAvto as the sole active operator. Known conveyor processes are rejected; no shared cross-application lock is available.","adapterStatus":"Capabilities are verified by the latest jobs; configuration alone is not readiness."},"sync":{"status":"never"}})
}

#[derive(Clone)]
struct App {
    // Startup validates this immutable identity against the database binding.
    // Bridge calls must not re-read/deserialize the full workspace to obtain it.
    account: accounts::Profile,
    db: Database,
    gate: Arc<writer_gate::WriterGate>,
    execution_gate: Arc<Mutex<()>>,
    assistant_gate: Arc<Mutex<()>>,
    assistant_chat_gate: Arc<Mutex<()>>,
    events: broadcast::Sender<()>,
    csrf: String,
    auth: Option<operator_auth::Auth>,
    public_origin: Option<String>,
    external_writes: bool,
    port: u16,
    data: PathBuf,
    bridge: PathBuf,
    node: PathBuf,
    tasks: Arc<Mutex<HashMap<String, tokio::task::AbortHandle>>>, 
    bootstrap_cache: Arc<bootstrap_cache::Cache>,
}
impl App {
    fn check_execution(&self) -> ApiResult<()> {
        // Database import never grants live authority. Real dispatch requires a
        // deliberate startup opt-in as well as the exact per-action approval.
        // Only our deterministic, network-free bridge is exempt for acceptance.
        let fake = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../tests/fake-bridge.mjs").canonicalize();
        let configured = self.bridge.canonicalize();
        let fake_transport = matches!((fake, configured), (Ok(a), Ok(b)) if a == b);
        if !self.external_writes && !fake_transport {
            return Err(ApiError(StatusCode::FORBIDDEN,
                "External publishing is disabled by the server operator".into()));
        }
        Ok(())
    }
    async fn read(&self) -> ApiResult<Value> {
        self.db.read().await
    }
    async fn read_assistant(&self, job: Option<&str>, conversation: &str) -> ApiResult<Value> {
        let _timing=performance::Span::new("assistant.read.total");
        self.db.read_assistant_context(job, conversation).await
    }
    async fn read_assistant_dialogue(&self, job: Option<&str>, conversation: &str) -> ApiResult<Value> {
        let _timing=performance::Span::new("assistant.dialogue.read.total");
        self.db.read_assistant_dialogue(job, conversation, &[]).await
    }
    async fn change_assistant_dialogue<T>(&self, job: Option<&str>, conversation: &str, extra_ids: &[Value], f: impl FnOnce(&mut Value) -> ApiResult<T>) -> ApiResult<T> {
        let _timing=performance::Span::new("assistant.dialogue.change.total");
        let waiting=performance::Span::new("assistant.dialogue.writer.wait");
        let _guard = self.gate.acquire(writer_gate::Class::Interactive).await;
        drop(waiting);
        let (result, changed) = self.db.change_assistant_dialogue_observed(job, conversation, extra_ids, f).await?;
        if changed { self.bootstrap_cache.invalidate(); let _ = self.events.send(()); }
        Ok(result)
    }
    async fn change_assistant<T>(&self, job: Option<&str>, conversation: &str, f: impl FnOnce(&mut Value) -> ApiResult<T>) -> ApiResult<T> {
        let _timing=performance::Span::new("assistant.change.total");
        let waiting=performance::Span::new("assistant.writer.wait");
        let _guard = self.gate.acquire(writer_gate::Class::Interactive).await;
        drop(waiting);
        let (result, changed) = self.db.change_assistant_observed(job, conversation, f).await?;
        if changed { self.bootstrap_cache.invalidate(); let _ = self.events.send(()); }
        Ok(result)
    }
    async fn create_conversation(&self, actor: &operator_auth::Actor, title: &str, item_ids: &[Value]) -> ApiResult<Value> {
        let _total = performance::Span::new("conversation.create.total");
        let waiting = performance::Span::new("conversation.create.writer_wait");
        let _guard = self.gate.acquire(writer_gate::Class::Interactive).await;
        drop(waiting);
        let conversation = self.db.create_conversation(&actor.id, title, item_ids).await?;
        self.bootstrap_cache.invalidate();
        let _ = self.events.send(());
        Ok(conversation)
    }
    fn observe_media_proofs(&self) {
        if self.bootstrap_cache.observe_external_epoch(media_fullframes::proof_epoch()) {
            let _=self.events.send(());
        }
    }
    async fn read_bootstrap(&self) -> ApiResult<Value> {
        self.observe_media_proofs();
        self.bootstrap_cache.get(|| async {
            let mut raw=self.db.read_bootstrap_source().await?;
            media_fullframes::project_media_readiness(&mut raw)?;
            // Expiry discovered by the pure projector invalidates this load's
            // start generation; Cache retains it only as an observed delta base.
            self.observe_media_proofs();
            // Retain compact job metadata until actor filtering applies its limit.
            let mut jobs=std::mem::take(list_mut(&mut raw,"jobs"));
            for job in &mut jobs { sanitize_bootstrap_job(job); }
            let mut compact=bootstrap_view(raw, "");
            compact["jobs"]=json!(jobs);
            compact.as_object_mut().unwrap().remove("csrfToken");
            compact.as_object_mut().unwrap().remove("operator");
            Ok(compact)
        }).await
    }
    async fn change<T>(&self, f: impl FnOnce(&mut Value) -> ApiResult<T>) -> ApiResult<T> {
        let _total = performance::Span::new("workspace.change.total");
        let waiting = performance::Span::new("workspace.writer.wait");
        let _guard = self.gate.acquire(writer_gate::Class::Standard).await;
        drop(waiting);
        let (result, changed) = self.db.change_observed(f).await?;
        if changed { self.bootstrap_cache.invalidate();
            let _ = self.events.send(());
        }
        Ok(result)
    }
    async fn change_media<T>(&self, f: impl FnOnce(&mut Value) -> ApiResult<T>) -> ApiResult<T> {
        let _total = performance::Span::new("media.change.total");
        let waiting = performance::Span::new("media.writer.wait");
        let _guard = self.gate.acquire(writer_gate::Class::Standard).await;
        drop(waiting);
        let (result, changed) = self.db.change_media_observed(f).await?;
        if changed { self.bootstrap_cache.invalidate(); let _ = self.events.send(()); }
        Ok(result)
    }
    async fn change_preparation_claim<T>(&self, f: impl FnOnce(&mut Value) -> ApiResult<T>) -> ApiResult<T> {
        let _total = performance::Span::new("preparation.claim.total");
        let waiting = performance::Span::new("preparation.writer.wait");
        let _guard = self.gate.acquire(writer_gate::Class::Standard).await;
        drop(waiting);
        let (result, changed) = self.db.change_preparation_claim_observed(f).await?;
        if changed { self.bootstrap_cache.invalidate(); let _ = self.events.send(()); }
        Ok(result)
    }
    async fn change_item<T>(&self, key: &str, f: impl FnOnce(&mut Value) -> ApiResult<T>) -> ApiResult<T> {
        let _guard = self.gate.acquire(writer_gate::Class::Standard).await;
        let (result, changed) = self.db.change_item_observed(key, f).await?;
        if changed { self.bootstrap_cache.invalidate(); let _ = self.events.send(()); }
        Ok(result)
    }
    async fn change_job<T>(&self, key: &str, f: impl FnOnce(&mut Value) -> ApiResult<T>) -> ApiResult<T> {
        let _guard = self.gate.acquire(writer_gate::Class::Standard).await;
        let (result, changed) = self.db.change_job_observed(key, f).await?;
        if changed { self.bootstrap_cache.invalidate(); let _ = self.events.send(()); }
        Ok(result)
    }
    async fn change_schedule<T>(&self, f: impl FnOnce(&mut Value) -> ApiResult<T>) -> ApiResult<T> {
        let _guard = self.gate.acquire(writer_gate::Class::Standard).await;
        let (result, changed) = self.db.change_schedule_observed(f).await?;
        if changed { self.bootstrap_cache.invalidate(); let _ = self.events.send(()); }
        Ok(result)
    }
    async fn change_status<T>(&self, routes: &[Value], f: impl FnOnce(&mut Value) -> ApiResult<T>) -> ApiResult<T> {
        let _guard = self.gate.acquire(writer_gate::Class::Standard).await;
        let (result, changed) = self.db.change_status_observed(routes, f).await?;
        if changed { self.bootstrap_cache.invalidate(); let _ = self.events.send(()); }
        Ok(result)
    }
    async fn change_source_claim<T>(&self, f: impl FnOnce(&mut Value) -> ApiResult<T>) -> ApiResult<T> {
        let _guard = self.gate.acquire(writer_gate::Class::Standard).await;
        let (result, changed) = self.db.change_source_claim_observed(f).await?;
        if changed { self.bootstrap_cache.invalidate(); let _ = self.events.send(()); }
        Ok(result)
    }
    async fn bridge(&self, operation: &str, args: Value) -> ApiResult<Value> {
        if operation == "execute" {
            self.check_execution()?;
        }
        let mut request = args;
        self.account.bind_request(&mut request)?;
        request["operation"] = json!(operation);
        let mut command = Command::new(&self.node);
        command
            .arg(&self.bridge)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true);
        #[cfg(windows)]
        command.creation_flags(0x08000000);
        let mut child = command
            .spawn()
            .map_err(|_| internal("Adapter runtime unavailable"))?;
        #[cfg(windows)]
        let _tree = ProcessTree::attach(&child)?;
        let mut input = child
            .stdin
            .take()
            .ok_or_else(|| internal("Adapter input unavailable"))?;
        input
            .write_all(request.to_string().as_bytes())
            .await
            .map_err(|_| internal("Adapter request failed"))?;
        drop(input);
        let mut stdout = child
            .stdout
            .take()
            .ok_or_else(|| internal("Adapter output unavailable"))?;
        let timeout = if matches!(operation,"media_vision"|"media_vision_chunk") {
            3600
        } else if operation == "media" {
            4520
        } else if operation == "assistant" {
            600
        } else {
            240
        };
        let output = tokio::time::timeout(Duration::from_secs(timeout), async {
            let mut buf = Vec::new();
            (&mut stdout)
                .take(32 * 1024 * 1024 + 1)
                .read_to_end(&mut buf)
                .await?;
            if buf.len() > 32 * 1024 * 1024 {
                return Err(std::io::Error::other("too large"));
            }
            let status = child.wait().await?;
            if !status.success() {
                return Err(std::io::Error::other("failed"));
            }
            Ok(buf)
        })
        .await
        .map_err(|_| internal("Adapter timed out; action outcome may be unknown"))?
        .map_err(|_| internal("Adapter process failed"))?;
        let envelope: Value =
            serde_json::from_slice(&output).map_err(|_| internal("Invalid adapter response"))?;
        if envelope["ok"] != true {
            let code = envelope["error"]["code"]
                .as_str()
                .unwrap_or("adapter_error");
            return Err(internal(&format!(
                "Adapter failed ({})",
                code.chars()
                    .filter(|c| c.is_ascii_alphanumeric() || *c == '_')
                    .take(80)
                    .collect::<String>()
            )));
        }
        Ok(envelope["result"].clone())
    }
    async fn finish(&self, job: &str, result: ApiResult<Value>) {
        let mut delay = Duration::from_secs(1);
        loop {
        // Retry only the durable completion record, never the worker/provider call.
        // Keep the task registered until completion is committed or server exit.
        let committed = self
            .change_job(job, |d| {
                let j = row_mut(d, "jobs", job)?;
                let refresh_review=matches!(j["kind"].as_str(),Some("execute"|"reconcile"));
                if media_fullframes::finish(j,&result,&now()){return Ok(false);}
                if j["status"] != "running" && j["status"] != "queued" {
                    return Ok(refresh_review);
                }
                let archive_job = j["purpose"] == "archive_import";
                let archive_id = j["refId"].clone();
                let sync_job = j["kind"] == "sync" && !archive_job;
                let sync_error = result.as_ref().err().map(|e| e.1.clone());
                let sync_partial = result.as_ref().ok().is_some_and(|v| v["partial"] == true);
                j["finishedAt"] = json!(now());
                match &result {
                    Ok(v) => {
                        j["status"] = json!("completed");
                        j["result"] = v.clone();
                    }
                    Err(e) => {
                        j["status"] = json!("failed");
                        j["error"] = json!(e.1);
                    }
                }
                if archive_job && d["sync"]["archive"]["id"] == archive_id {
                    archive_import::finish(d, sync_error.as_deref());
                }
                if sync_job {
                    sync_scan::schedule_next(d, sync_error.is_some(), chrono::Utc::now().timestamp());
                    if let Some(error) = sync_error {
                        d["sync"]["status"] = json!("error");
                        d["sync"]["lastError"] = json!(error);
                        d["sync"]["lastFailedAt"] = json!(now());
                    } else {
                        d["sync"]["status"] =
                            json!(if sync_partial { "partial" } else { "completed" });
                        d["sync"]["lastError"] = Value::Null;
                    }
                }
                Ok(refresh_review)
            })
            .await;
        if let Ok(refresh_review)=committed {
            // The focused job transaction has committed and released its gate.
            // Only action/reconciliation completions need the private chat receipt.
            if !refresh_review || self.change(|d| assistant_action_review::refresh_execution_receipts(d,job)).await.is_ok(){break;}
        }
        eprintln!("job_finalization_pending; retrying durable completion");
        tokio::time::sleep(delay).await;
        delay = (delay * 2).min(Duration::from_secs(30));
        }
        self.tasks.lock().await.remove(job);
    }
    async fn job(&self, kind: &str, ref_id: &str) -> ApiResult<String> {
        self.change_schedule(|d| new_job(d, kind, ref_id)).await
    }
    fn spawn(
        &self,
        job: String,
        f: impl std::future::Future<Output = ApiResult<Value>> + Send + 'static,
    ) {
        let app = self.clone();
        tokio::spawn(async move {
            let mut handles = app.tasks.lock().await;
            if handles.contains_key(&job) { return; }
            let worker = app.clone();
            let key = job.clone();
            let task = tokio::spawn(async move {
                worker_supervision::run_if_active(&worker, &key, f).await
            });
            handles.insert(job.clone(), task.abort_handle());
            drop(handles);
            let exit = worker_supervision::observe(task).await;
            worker_supervision::finalize(&app, &job, exit).await;
        });
    }
}
fn new_job(d: &mut Value, kind: &str, ref_id: &str) -> ApiResult<String> {
    if kind == "sync"
        && list(d, "jobs")
            .iter()
            .any(|j| j["kind"] == kind && (j["status"] == "running" || j["status"] == "queued"))
    {
        return Err(conflict("A job of this kind is already running"));
    }
    let key = id();
    list_mut(d, "jobs")
        .push(json!({"id":key,"kind":kind,"refId":ref_id,"status":"running","createdAt":now()}));
    Ok(key)
}

async fn security(State(app): State<App>, req: Request, next: Next) -> Response {
    operator_http::security(app,req,next).await
}
async fn bootstrap(State(app): State<App>) -> ApiResult<Json<Value>> {
    Ok(Json(bootstrap_view(app.read_bootstrap().await?, &app.csrf)))
}
fn sanitize_bootstrap_job(job:&mut Value) {
    if let Some(bundle)=job["prepareBundle"].as_object_mut(){bundle.remove("request");}
    if let Some(progress)=job["result"]["visualProgress"].as_object_mut(){
        for key in ["sourceProjection","leaseId","materialEpoch"]{progress.remove(key);}
    }
}
fn bootstrap_view(mut d: Value, csrf: &str) -> Value {
    dispatch_authority::sanitize_view(&mut d);
    d["csrfToken"] = json!(csrf);
    if let Some(obj) = d.as_object_mut() {
        obj.remove("audit");
        obj.remove("knowledge_versions");
        obj.remove("knowledge_entries");
        obj.remove("feedback");
        obj.remove("companyKnowledgeCoverage");
        // Internal current media heads serve readiness projection, never UI data.
        obj.remove("mediaReadinessCatalog");
    }
    for branch in list_mut(&mut d, "branches") {
        if let Some(branch) = branch.as_object_mut() {
            branch.remove("observedMessages");
        }
    }
    const TERMINAL_JOB_LIMIT: usize = 200;
    let jobs = std::mem::take(list_mut(&mut d, "jobs"));
    let total = jobs.len();
    let mut terminal_count = 0;
    let mut visible: Vec<Value> = jobs.into_iter().rev().filter(|job| {
        if matches!(job["status"].as_str(), Some("completed" | "failed" | "cancelled")) {
            terminal_count += 1;
            terminal_count <= TERMINAL_JOB_LIMIT
        } else {
            true
        }
    }).collect();
    visible.reverse();
    let returned = visible.len();
    for job in &mut visible {
        sanitize_bootstrap_job(job);
    }
    d["jobs"] = json!(visible);
    d["historyMetadata"] = json!({"jobs":{"total":total,"returned":returned,"omitted":total-returned,"terminalLimit":TERMINAL_JOB_LIMIT,"historyTruncated":returned<total}});
    d
}
async fn health() -> Json<Value> {
    Json(json!({"status":"ok"}))
}
async fn events(
    State(app): State<App>,
) -> Sse<impl futures_util::Stream<Item = Result<Event, Infallible>>> {
    let rx = app.events.subscribe();
    let stream = futures_util::stream::unfold(rx, |mut rx| async move {
        match rx.recv().await {
            Ok(_) | Err(broadcast::error::RecvError::Lagged(_)) => {
                Some((Ok(Event::default().event("refresh").data("{}")), rx))
            }
            Err(_) => None,
        }
    });
    Sse::new(stream).keep_alive(KeepAlive::default())
}

async fn item_patch(
    State(app): State<App>,
    Path(key): Path<String>,
    Json(body): Json<Value>,
) -> ApiResult<Json<Value>> {
    app.change_item(&key, |d| patch_item(d, &key, &body).map(Json)).await
}

/// Shared local item authority used by HTTP and bounded assistant workflow tools.
fn patch_item(d: &mut Value, key: &str, body: &Value) -> ApiResult<Value> {
    if list(d, "operations")
        .iter()
        .any(|o| o["itemId"] == key && o["status"] == "dispatching")
    {
        return Err(conflict("An approved action is being dispatched"));
    }
    if feedback::retry(d,&body,&key)?.is_some() {return Ok(row(d,"items",&key)?.clone());}
    let before=row(d,"items",&key)?.clone();
    let origin=feedback::origin(d,&before,&body)?;
    let item = row_mut(d, "items", &key)?;
    check_revision(item, &body["expectedRevision"])?;
    if let Some(w) = body["workflow"].as_str() {
        if !["attention", "prepared", "waiting"].contains(&w) {
            return Err(bad("Provider close requires an approved action"));
        }
        if item["workflow"] == "closed" {
            return Err(conflict("Provider-closed item cannot be reopened locally"));
        }
        item["workflow"] = json!(w);
    }
    for k in ["draft", "waitingReason", "dueAt"] {
        if body.get(k).is_some() {
            if k != "dueAt" && !body[k].is_string() {
                return Err(bad("Invalid item field"));
            }
            if k == "draft" && body[k].as_str().unwrap_or("").len()>20000 {return Err(bad("Draft too long"));}
            item[k] = body[k].clone();
        }
    }
    if body.get("draft").is_some() && (body["draftEdited"]==true || !origin.is_null() || body["draft"]!=before["draft"]) {
        item["draftEdited"]=json!(true);
        item["draftOrigin"]=origin.clone();
        if !origin.is_null(){
            item["draftOrigin"]["sourceProposalId"]=origin["id"].clone();
            item["draftOrigin"]["sourceProposalRevision"]=origin["revision"].clone();
            item["draftOrigin"]["draftSessionId"]=body["draftSessionId"].clone();
        }
        item["draftSessionId"]=body["draftSessionId"].clone();
    }
    if *item!=before {
        bump(item);
        if item["autoPreparation"].is_object() { item["autoPreparation"]["humanOverrideAt"]=json!(now()); }
    }
    let after = item.clone();
    if body.get("draft").is_some() && (before!=after || body.get("eventId").is_some()) {
        let baseline=if before["draftEdited"]==true || !before["draft"].as_str().unwrap_or("").is_empty(){before["draft"].clone()}else{origin["text"].as_str().map(|s|json!(s)).unwrap_or(json!(""))};
        let kind=if after["draft"].as_str()==Some(""){"proposal_cleared"}else{"draft_saved"};
        feedback::append(d,&body,&after,&origin,kind,json!({"before":baseline,"after":after["draft"],"itemRevision":after["revision"],"status":"pending_review"}))?;
    } else if before["workflow"]!=after["workflow"] {
        feedback::append(d,&body,&after,&origin,"action_selected",json!({"action":after["workflow"],"previousAction":before["workflow"]}))?;
    }
    Ok(row(d,"items",&key)?.clone())
}

fn merge_snapshot(d: &mut Value, snapshot: &Value) -> ApiResult<()> {
    let ordered_snapshot=snapshot_order::ordered(d,snapshot)?;
    let snapshot=&ordered_snapshot;
    for collection in ["posts", "branches"] {
        if let Some(rows) = snapshot[collection].as_array() {
            for source in rows {
                let mut observed = source.clone();
                if collection == "branches" {
                    observed["observedAt"] = json!(now());
                    observed["observedMessages"] = source["messages"].clone();
                }
                let value = &observed;
                let key = required(value, "id")?;
                let rows = list_mut(d, collection);
                if let Some(old) = rows.iter_mut().find(|r| r["id"] == key) {
                    *old = value.clone()
                } else {
                    rows.push(value.clone());
                }
            }
        }
    }
    if let Some(items) = snapshot["items"].as_array() {
        for value in items {
            let key = required(value, "id")?;
            let items = list_mut(d, "items");
            if let Some(old) = items.iter_mut().find(|r| r["id"] == key) {
                for identity in ["itemId", "objectId"] {
                    if old[identity] != value[identity] {
                        return Err(conflict("Provider identity changed for an existing record"));
                    }
                }
                if old.get("connectorBinding").is_some()
                    && old["connectorBinding"] != value["connectorBinding"]
                {
                    return Err(conflict("Imported record belongs to another connector"));
                }
                let mut incoming = value.clone();
                for k in [
                    "draft",
                    "draftEdited",
                    "draftOrigin",
                    "draftSessionId",
                    "workflow",
                    "waitingReason",
                    "dueAt",
                    "revision",
                    "branchContextDigest",
                    "autoPreparation",
                    "autoRevalidation",
                    "reason",
                    "decision",
                    "triageTags",
                ] {
                    incoming[k] = old[k].clone();
                }
                if old["contextEvidenceDigest"] != incoming["contextEvidenceDigest"]
                    || old["providerStatus"] != incoming["providerStatus"]
                    || old["postKey"] != incoming["postKey"]
                    || old["conversationKey"] != incoming["conversationKey"]
                {
                    bump(&mut incoming);
                }
                if value["providerStatus"] == "deleted" {
                    incoming["workflow"] = json!("deleted");
                } else if value["providerStatus"] == "closed" && old["workflow"] != "waiting" {
                    incoming["workflow"] = json!("closed");
                } else if ["closed", "deleted"].contains(&old["providerStatus"].as_str().unwrap_or(""))
                    && ["closed", "deleted"].contains(&old["workflow"].as_str().unwrap_or(""))
                    && ["new", "inprogress"].contains(&value["providerStatus"].as_str().unwrap_or(""))
                {
                    incoming["workflow"] = json!("attention");
                }
                incoming["providerObservedAt"] = incoming["contextObservedAt"].as_str().map(|s|json!(s)).unwrap_or_else(||json!(now()));
                *old = incoming;
            } else {
                let mut incoming = value.clone();
                incoming["revision"] = json!(1);
                incoming["draft"] = json!("");
                // Provider snapshots cannot create operator edits or feedback lineage.
                incoming["draftEdited"] = json!(false);
                incoming["draftOrigin"] = Value::Null;
                incoming["draftSessionId"] = Value::Null;
                incoming["waitingReason"] = json!("");
                incoming["dueAt"] = Value::Null;
                incoming["workflow"] = json!(match value["providerStatus"].as_str() {
                    Some("closed") => "closed",
                    Some("deleted") => "deleted",
                    _ => "attention",
                });
                incoming["providerObservedAt"] = incoming["contextObservedAt"].as_str().map(|s|json!(s)).unwrap_or_else(||json!(now()));
                items.push(incoming);
            }
        }
    }
    thread_graph::enrich(d);
    // Provider digest covers its own fragment. Our assembled thread may gain a
    // sibling independently, so version that context separately for approvals.
    use sha2::{Digest, Sha256};
    let branch_digests: HashMap<String,String> = list(d,"branches").iter().filter_map(|branch| {
        let key=branch["id"].as_str()?;
        // Author-history identity is enrichment, not preparation evidence. Keep
        // the pre-enrichment digest stable, including when an adapter adds null.
        // All other message fields remain in the approval context fingerprint.
        let mut messages=branch["messages"].clone();
        if let Some(messages)=messages.as_array_mut() {
            for message in messages {
                if let Some(fields)=message.as_object_mut() {
                    for key in ["authorId", "providerOfficial", "roleEvidence"] { fields.remove(key); }
                }
            }
        }
        let evidence=json!({"messages":messages,"contextComplete":branch["contextComplete"],"missingParentIds":branch["missingParentIds"],"contextTruncated":branch["contextTruncated"]});
        Some((key.to_string(),format!("{:x}",Sha256::digest(evidence.to_string().as_bytes()))))
    }).collect();
    for item in list_mut(d, "items") {
        if let Some(digest) = item["branchId"]
            .as_str()
            .and_then(|key| branch_digests.get(key))
        {
            if item["branchContextDigest"]
                .as_str()
                .is_some_and(|old| old != digest)
            {
                bump(item);
            }
            item["branchContextDigest"] = json!(digest);
        }
    }
    auto_prepare::reconcile_stale(d,chrono::Utc::now().timestamp());
    Ok(())
}
async fn sync_page(app: &App, mode: &str, cursor: Option<&str>) -> ApiResult<()> {
    let binding = active_binding(&app.read().await?)?;
    let account = bridge_account(&binding)?;
    let mut args = json!({"account":account,"mode":mode});
    if let Some(cursor) = cursor {
        args["cursor"] = json!(cursor);
    }
    let mut snapshot = app.bridge("read", args).await?;
    if let Some(items) = snapshot["items"].as_array_mut() {
        for item in items {
            *item = bound_item(&binding, item)?;
        }
    }
    app.change(|d| {
        if active_binding(d)? != binding {
            return Err(conflict("Connector changed during sync"));
        }
        d["connectorBinding"] = binding.to_json();
        merge_sync_page(d, &snapshot, mode, cursor.is_some())
    })
    .await
}
fn merge_sync_page(
    d: &mut Value,
    snapshot: &Value,
    mode: &str,
    continuation: bool,
) -> ApiResult<()> {
    merge_snapshot(d, snapshot)?;
    let timestamp = now();
    d["sync"]["lastSyncedAt"] = json!(timestamp);
    let page = json!({"coverage":snapshot["coverage"],"hasMore":snapshot["hasMore"],"cursor":snapshot["cursor"],"observedCount":snapshot["observedCount"],"scannedCount":snapshot["scannedCount"],"lastSyncedAt":timestamp});
    // Refresh the first page's data without rewinding an operator's continuation.
    // Keep exhausted pagination exhausted, even when the first page has more rows.
    if !continuation && d["sync"][mode]["paginationStarted"] == true {
        d["sync"][mode]["firstPage"] = page;
        d["sync"][mode]["lastSyncedAt"] = json!(timestamp);
    } else {
        d["sync"][mode] = page;
        d["sync"][mode]["paginationStarted"] = json!(continuation);
    }
    Ok(())
}
async fn run_sync(app: App) -> ApiResult<Value> {
    sync_scan::run(app).await
}
async fn sync(State(app): State<App>, Json(body): Json<Value>) -> ApiResult<Json<Value>> {
    let mode = body["mode"].as_str().map(str::to_string);
    if mode.as_ref().is_some_and(|m| m != "open" && m != "closed") {
        return Err(bad("Invalid sync mode"));
    }
    let cursor = body["cursor"].as_str().map(str::to_string);
    if cursor.is_some() && mode.is_none() {
        return Err(bad("Pagination requires a mode"));
    }
    let job = app.job("sync", "").await?;
    let worker = app.clone();
    app.spawn(job.clone(), async move {
        if let Some(mode) = mode {
            sync_page(&worker, &mode, cursor.as_deref()).await?;
            Ok(json!({"synced":true}))
        } else {
            run_sync(worker).await
        }
    });
    Ok(Json(json!({"jobId":job})))
}

async fn conversation_new(
    State(app): State<App>,
    axum::Extension(actor): axum::Extension<operator_auth::Actor>,
    Json(body): Json<Value>,
) -> ApiResult<Json<Value>> {
    let item_ids = body["itemIds"].as_array().cloned().unwrap_or_default();
    app.create_conversation(&actor, body["title"].as_str().unwrap_or("Обсуждение"), &item_ids).await.map(Json)
}
async fn conversation_message(
    State(app): State<App>,
    axum::Extension(actor): axum::Extension<operator_auth::Actor>,
    Path(key): Path<String>,
    Json(body): Json<Value>,
) -> ApiResult<Json<Value>> {
    let text = required(&body, "text")?.to_string();
    if text.len() > 50000 {
        return Err(bad("Message too long"));
    }
    let attached = body["itemIds"].as_array().cloned().unwrap_or_default();
    let admission_started = std::time::Instant::now();
    let (job, request) = app.change_assistant_dialogue(None, &key, &attached, |d| {
        if !operator_http::owned(row(d,"conversations",&key)?,&actor){return Err(ApiError(StatusCode::NOT_FOUND,"Conversation not found".into()))}
        if list(d,"jobs").iter().any(|j|j["kind"]=="assistant"&&j["refId"]==key&&j["status"]=="running"){
            return Err(conflict("Дождитесь ответа в этом обсуждении"));
        }
        if list(d,"jobs").iter().filter(|j|j["kind"]=="assistant"&&j["status"]=="running").count()>=10{
            return Err(conflict("Очередь ассистента заполнена. Попробуйте чуть позже"));
        }
        let message = json!({"id":id(),"role":"user","text":text,"itemIds":attached,"createdAt":now()});
        let mut messages = row(d,"conversations",&key)?["messages"].as_array().cloned().unwrap_or_default();
        messages.push(message.clone());
        let mut bundle = prepare_bundle::build(d,&attached,&messages).map_err(bad)?;
        let screen=assistant_context::screen(d,&body["screen"],&attached).map_err(bad)?;
        assistant_context::attach_screen(&mut bundle,screen).map_err(bad)?;
        assistant_context::attach_displayed_draft(d,&mut bundle,&body["displayedDraft"]).map_err(bad)?;
        let request = bundle["request"].clone();
        let job = new_job(d,"assistant",&key)?;
        row_mut(d,"jobs",&job)?["prepareBundle"] = bundle;
        row_mut(d,"jobs",&job)?["operatorId"] = json!(actor.id);
        row_mut(d,"jobs",&job)?["purpose"] = json!("discussion");
        row_mut(d,"jobs",&job)?["sourceUserMessageId"] = message["id"].clone();
        row_mut(d,"jobs",&job)?["admissionBeforeCommitMs"] = json!(admission_started.elapsed().as_millis() as u64);
        let chat = row_mut(d,"conversations",&key)?;
        chat["itemIds"] = json!(attached);
        chat["messages"].as_array_mut().unwrap().push(message);
        Ok((job,request))
    }).await?;
    let worker = app.clone();
    let job_key = job.clone();
    app.spawn(job.clone(),async move {
        // Personal discussions have their own model lane. Preparation cannot
        // hold this gate while a first pass or research review is running.
        let _assistant_guard=worker.assistant_chat_gate.lock().await;
        // Waiting for another operator must not spend a model call on evidence
        // which was already superseded while this request stood in line.
        let fresh=worker.read_assistant_dialogue(Some(&job_key), &key).await?;
        let queued=row(&fresh,"jobs",&job_key)?;
        if queued["status"]!="running"{return Err(conflict("Assistant request is no longer active"));}
        prepare_bundle::current(&fresh,&queued["prepareBundle"])
            .map_err(|_|conflict("Контекст изменился в очереди. Проверьте комментарий и отправьте запрос снова; модель не запускалась"))?;
        drop(fresh);
        assistant_dialogue::run(worker.clone(),job_key,key,actor,request).await
    });
    Ok(Json(json!({"jobId":job})))
}
async fn cancel(State(app): State<App>, Path(key): Path<String>) -> ApiResult<Json<Value>> {
    app.change(|d| {
        let j = row_mut(d, "jobs", &key)?;
        if ["execute", "reconcile"].contains(&j["kind"].as_str().unwrap_or("")) {
            return Err(conflict(
                "External operation must finish or reconcile; cancellation cannot undo it",
            ));
        }
        if j["status"] == "running" || (j["status"] == "queued" && j["kind"] == "media") {
            j["status"] = json!("cancelled");
            j["finishedAt"] = json!(now());
        }
        Ok(())
    })
    .await?;
    if let Some(handle) = app.tasks.lock().await.remove(&key) {
        handle.abort();
    }
    Ok(Json(json!({"ok":true})))
}

fn create_proposal(d: &mut Value, body: &Value) -> ApiResult<Value> {create_proposal_impl(d,body,false)}
fn create_generated_proposal(d: &mut Value, body: &Value) -> ApiResult<Value> {create_proposal_impl(d,body,true)}
fn create_proposal_impl(d: &mut Value, body: &Value, generated:bool) -> ApiResult<Value> {
    let target = required(body, "itemId")?;
    let mut item = row(d, "items", target)?.clone();
    if let Some(e)=feedback::retry(d,body,target)? {return Ok(row(d,"proposals",required(&e,"proposalId")?)?.clone());}
    let origin=if generated {Value::Null}else{feedback::origin(d,&item,body)?};
    let review_digest=prepare_bundle::review_fingerprint(d,target).map_err(conflict)?;
    if origin["sourceContextDigest"].is_string() && origin["sourceContextDigest"]!=review_digest {return Err(conflict("Source context changed; prepare a new suggestion"));}
    check_revision(&item, &body["expectedRevision"])?;
    let binding = active_binding(d)?;
    bridge_account(&binding)?;
    let route_target = bound_item(&binding, &item)?;
    let allow_closed_reply=!generated && body["allowClosedReply"]==true && body["kind"]=="reply_and_close";
    if (item["workflow"]=="closed" && !allow_closed_reply) || item["workflow"]=="deleted"
        || item["providerStatus"] == "deleted" {
        return Err(conflict("Item is closed or deleted"));
    }
    let kind = required(body, "kind")?;
    if !["reply_and_close", "close", "hide", "delete"].contains(&kind) {
        return Err(bad("Unsupported action"));
    }
    let text = body["text"].as_str().unwrap_or("");
    if kind == "reply_and_close" && text.trim().is_empty() {
        return Err(bad("Reply text required"));
    }
    if text.len() > 20000 {
        return Err(bad("Reply too long"));
    }
    if kind!="reply_and_close" && !text.trim().is_empty() {
        return Err(bad("Non-reply action cannot contain reply text"));
    }
    let caps=Capabilities::angryspace_for_platform(item["platform"].as_str().unwrap_or(""));
    if legacy_actions(kind,text).map_err(|e|bad(e.0))?.iter().any(|action|!caps.supports(action.kind())) {
        return Err(bad("Action is unavailable for this platform"));
    }
    if media_queue::preparation_state(d,&item,&now())?.is_some() {
        return Err(conflict("Video audio and visual evidence must be complete before preparing a decision"));
    }
    if item["workflow"] == "attention" {
        let current = row_mut(d, "items", target)?;
        current["workflow"] = json!("prepared");
        bump(current);
        item = current.clone();
    }
    let mut v = json!({"id":id(),"itemId":target,"kind":kind,"text":text,"routeTarget":route_target,"revision":1,"itemRevision":item["revision"],"contextEvidenceDigest":item["contextEvidenceDigest"],"branchContextDigest":item["branchContextDigest"],"status":"draft","sources":body["sources"].as_array().cloned().unwrap_or_default(),"createdAt":now()});
    if !origin.is_null(){v["origin"]=origin.clone();v["sourceProposalId"]=origin["id"].clone();v["sourceProposalRevision"]=origin["revision"].clone();}
    v["reviewContextDigest"]=json!(review_digest);
    if allow_closed_reply {v["allowClosedReply"]=json!(true);}
    v["draftSessionId"]=body.get("draftSessionId").cloned().unwrap_or_else(||item["draftSessionId"].clone());
    list_mut(d, "proposals").push(v.clone());
    if !generated && (body.get("eventId").is_some() || !origin.is_null()) {
        feedback::append(d,body,&item,&origin,"action_selected",json!({"action":kind,"text":text,"proposalId":v["id"],"proposalRevision":v["revision"]}))?;
    }
    Ok(v)
}
async fn proposal_new(State(app): State<App>, Json(body): Json<Value>) -> ApiResult<Json<Value>> {
    app.change(|d| create_proposal(d, &body).map(Json)).await
}
async fn recover_prepared(State(app): State<App>, Json(body): Json<Value>) -> ApiResult<Json<Value>> {
    if body["apply"]!=true {return recover_equivalent_prepared(&mut app.read().await?,false).map(Json);}
    app.change(|d| recover_equivalent_prepared(d,true).map(Json)).await
}
// Owner-only maintenance route (enforced by the shared /api/maintenance gate).
// Re-admit a persisted model stage after the provider merge discarded its local
// review pointer. Dry-run and apply execute the same checks on isolated state.
async fn recover_rejected_revalidation(State(app): State<App>, Json(body): Json<Value>) -> ApiResult<Json<Value>> {
    let fields=body.as_object().ok_or_else(||bad("Recovery request must be an object"))?;
    if fields.len()!=1 || !fields.contains_key("apply") {return Err(bad("Recovery requires only apply"));}
    let apply=body["apply"].as_bool().ok_or_else(||bad("apply must be boolean"))?;
    let at=chrono::Utc::now().timestamp();
    if apply {app.change(|d|recover_rejected_revalidation_plan(d,true,at).map(Json)).await}
    else {let mut snapshot=app.read().await?;recover_rejected_revalidation_plan(&mut snapshot,false,at).map(Json)}
}
fn recover_rejected_revalidation_plan(d:&mut Value,apply:bool,at:i64)->ApiResult<Value> {
    if !apply {
        let mut preview=d.clone();
        let mut report=recover_rejected_revalidation_plan(&mut preview,true,at)?;
        report["applied"]=json!(false);
        report["recoveredCount"]=json!(0);
        return Ok(report);
    }
    let ids:Vec<String>=list(d,"items").iter().filter(|i|i["workflow"]=="attention")
        .filter_map(|i|i["id"].as_str().map(str::to_owned)).collect();
    let mut eligible=0usize;
    let mut skipped=std::collections::BTreeMap::<&'static str,usize>::new();
    for id in ids {
        match rejected_revalidation_trial(d,&id,at) {
            Ok(trial)=>{eligible+=1;*d=trial;},
            Err(code)=>{*skipped.entry(code).or_default()+=1;},
        }
    }
    Ok(json!({"applied":true,"eligibleCount":eligible,"recoveredCount":eligible,"skippedByReason":skipped}))
}
fn rejected_revalidation_trial(d:&Value,id:&str,at:i64)->Result<Value,&'static str> {
    use sha2::{Digest,Sha256};
    let item=row(d,"items",id).map_err(|_|"item_missing")?;
    if item["workflow"]!="attention" || !matches!(item["autoPreparation"]["status"].as_str(),Some("stale"|"needs_attention")) {return Err("item_not_held");}
    if !matches!(item["providerStatus"].as_str(),Some("new"|"inprogress")) {return Err("provider_not_open");}
    if !item["draft"].as_str().unwrap_or("").trim().is_empty() || item["draftEdited"]==true
        || !item["autoPreparation"]["humanOverrideAt"].is_null() {return Err("operator_work_present");}
    if !item["autoRevalidation"].is_null() {return Err("review_state_present");}
    if list(d,"operations").iter().any(|o|o["itemId"]==id) {return Err("operation_history_present");}
    let proposals:Vec<&Value>=list(d,"proposals").iter().filter(|p|p["itemId"]==id).collect();
    if proposals.is_empty() {return Err("saved_proposal_missing");}
    for proposal in &proposals {
        if proposal["status"]!="stale" || proposal["origin"].is_object()
            || proposal["history"].as_array().is_some_and(|h|!h.is_empty())
            || proposal["text"].as_str().is_some_and(|s|s.encode_utf16().count()>12000) {return Err("protected_or_human_proposal");}
        let run=proposal["prepareRunId"].as_str().or_else(||proposal["recovery"]["prepareRunId"].as_str())
            .ok_or("unverified_proposal")?;
        let prior=row(d,"jobs",run).map_err(|_|"unverified_proposal")?;
        let bundle=&prior["prepareBundle"];
        let origin=if proposal["recovery"].is_object(){&proposal["recovery"]}else{proposal};
        if !matches!(prior["purpose"].as_str(),Some("auto_prepare"|"auto_revalidate")) || prior["status"]!="completed"
            || bundle["version"]!=1 || bundle["itemIds"]!=json!([item["id"]])
            || bundle["digest"]!=format!("{:x}",Sha256::digest(bundle["request"].to_string().as_bytes()))
            || origin["prepareBundleId"]!=bundle["id"] || origin["prepareBundleDigest"]!=bundle["digest"] {return Err("unverified_proposal");}
    }
    let saved=item["autoPreparation"]["savedProposalId"].as_str().ok_or("saved_proposal_missing")?;
    if !proposals.iter().any(|p|p["id"]==saved) {return Err("saved_proposal_mismatch");}
    // The most recent assistant decision wins, including a newer initial run.
    let job=list(d,"jobs").iter().rev().find(|j|j["kind"]=="assistant" && j["refId"]==id)
        .ok_or("model_decision_missing")?;
    if job["purpose"]!="auto_revalidate" || job["status"]!="completed" {return Err("newer_model_decision");}
    if !job["recovery"].is_null() {return Err("already_recovered");}
    let old_outcome=&job["prepareOutcome"];
    if old_outcome["status"]!="stale"
        || !(old_outcome["rejectionCode"]=="review_job_pointer_changed"
            || (old_outcome["rejectionCode"].is_null() && old_outcome["reason"]=="Revalidation source or operator draft changed")) {
        return Err("not_lost_pointer_rejection");
    }
    let first=&job["preparationStages"]["first"];
    if first["status"]!="completed" || !first["result"].is_object(){return Err("model_stage_missing");}
    let model_result=match first["reviewRequired"].as_bool() {
        Some(false)=>first["result"].clone(),
        Some(true)=>{
            let review=&job["preparationStages"]["review"];
            if review["status"]!="completed" || !review["result"].is_object()
                || !review["result"]["runMetadata"].is_object(){return Err("review_stage_missing");}
            review["result"].clone()
        },
        None=>return Err("model_stage_invalid"),
    };
    let bundle=&job["prepareBundle"];
    if bundle["version"]!=1 || bundle["itemIds"]!=json!([item["id"]])
        || prepare_bundle::current(d,bundle).is_err(){return Err("bundle_changed");}
    if prepare_bundle::review_fingerprint(d,id).ok().as_deref()!=job["sourceDigest"].as_str(){return Err("source_changed");}
    let run=required(job,"id").map_err(|_|"model_decision_missing")?.to_owned();
    let original_outcome=old_outcome.clone();
    let original_result=job["result"].clone();
    let original_finished=job["finishedAt"].clone();
    let source=job["sourceDigest"].clone();
    let mut trial=d.clone();
    row_mut(&mut trial,"items",id).map_err(|_|"item_missing")?["autoRevalidation"]=
        json!({"status":"running","jobId":run,"pendingDigest":source,"startedAt":job["claimedAt"]});
    row_mut(&mut trial,"jobs",&run).map_err(|_|"model_decision_missing")?["status"]=json!("running");
    let outcome=auto_prepare::complete(&mut trial,&run,&model_result,at).map_err(|_|"admission_error")?;
    if !matches!(outcome["status"].as_str(),Some("prepared"|"needs_attention")){return Err("admission_rejected");}
    let stored=row_mut(&mut trial,"jobs",&run).map_err(|_|"model_decision_missing")?;
    stored["status"]=json!("completed");
    stored["result"]=outcome.clone();
    stored["recovery"]=json!({"kind":"replayed_lost_review_pointer","originalPrepareOutcome":original_outcome,
        "originalResult":original_result,"originalFinishedAt":original_finished,"recoveredAt":chrono::DateTime::from_timestamp(at,0).unwrap().to_rfc3339(),"sourceDigest":source});
    audit(&mut trial,"preparation.revalidation_result_recovered",&run);
    Ok(trial)
}
fn recover_equivalent_prepared(d:&mut Value,apply:bool)->ApiResult<Value>{
    if !apply {let mut preview=d.clone();merge_snapshot(&mut preview,&json!({}))?;return recover_equivalent_prepared_inner(&mut preview,false);}
    // Normalize known adapter-only hash drift in the same transaction. Real
    // branch changes still invalidate their proposals through reconciliation.
    merge_snapshot(d,&json!({}))?;
    recover_equivalent_prepared_inner(d,true)
}
fn recover_equivalent_prepared_inner(d:&mut Value,apply:bool)->ApiResult<Value>{
    let candidates:Vec<Value>=list(d,"proposals").iter().rev().filter(|p|p["status"]=="stale" && p["staleReason"]=="Review source context changed").cloned().collect();
    let mut seen=std::collections::HashSet::new();
    let mut results=vec![];
    for p in candidates {
        let target=required(&p,"itemId")?;
        if !seen.insert(target.to_owned()){continue;}
        let item=row(d,"items",target)?.clone();
        if !matches!(item["providerStatus"].as_str(),Some("new"|"inprogress")) || item["workflow"]!="attention"
            || item["draftEdited"]==true || !item["draft"].as_str().unwrap_or("").is_empty()
            || !item["autoPreparation"]["humanOverrideAt"].is_null()
            || list(d,"operations").iter().any(|o|o["itemId"]==target)
            || list(d,"proposals").iter().any(|q|q["itemId"]==target && matches!(q["status"].as_str(),Some("draft"|"approved"|"dispatching"|"unknown"|"succeeded"))) {continue;}
        let Some(run)=p["prepareRunId"].as_str() else {continue};
        let Ok(job)=row(d,"jobs",run) else {continue};
        let bundle=&job["prepareBundle"];
        if p["prepareBundleId"]!=bundle["id"] || p["prepareBundleDigest"]!=bundle["digest"] {continue;}
        if !prepare_bundle::equivalent_saved_source(d,bundle,target).unwrap_or(false){results.push(json!({"proposalId":p["id"],"result":"source_changed"}));continue;}
        if !apply {results.push(json!({"proposalId":p["id"],"result":"equivalent"}));continue;}
        let v=create_proposal_impl(d,&json!({"itemId":target,"expectedRevision":item["revision"],"kind":p["kind"],"text":p["text"],"sources":p["sources"]}),true)?;
        let q=row_mut(d,"proposals",required(&v,"id")?)?;
        q["recovery"]=json!({"kind":"recovered_context_metadata","originProposalId":p["id"],"originProposalRevision":p["revision"],"prepareRunId":run,"prepareBundleId":p["prepareBundleId"],"prepareBundleDigest":p["prepareBundleDigest"],"sourceProposal":p,"recoveredAt":now()});
        q["generationMetadata"]=p["generationMetadata"].clone();
        q["knowledgeManifest"]=p["knowledgeManifest"].clone();
        let item=row_mut(d,"items",target)?;
        item["decision"]=json!(if p["kind"]=="close" {"no_reply"}else{"reply"});
        item["autoPreparation"]["requiresReview"]=json!(false);
        item["autoPreparation"]["status"]=json!("prepared");
        item["autoPreparation"]["savedProposalId"]=v["id"].clone();
        item["autoPreparation"]["reason"]=json!("Сохранённый ответ восстановлен после проверки неизменности контекста. Проверьте перед отправкой.");
        audit(d,"proposal.recovered_context_metadata",required(&v,"id")?);
        results.push(json!({"proposalId":p["id"],"result":"recovered","replacementId":v["id"]}));
    }
    Ok(json!({"applied":apply,"results":results}))
}
async fn proposal_patch(
    State(app): State<App>,
    Path(key): Path<String>,
    Json(body): Json<Value>,
) -> ApiResult<Json<Value>> {
    app.change(|d| {
        let p = row(d, "proposals", &key)?.clone();
        if feedback::retry(d,&body,required(&p,"itemId")?)?.is_some(){return Ok(Json(p));}
        check_revision(&p, &body["expectedRevision"])?;
        if ["dispatching", "unknown", "succeeded"].contains(&p["status"].as_str().unwrap_or("")) {
            return Err(conflict("Dispatched proposal cannot be edited"));
        }
        let text = body["text"].as_str().ok_or_else(|| bad("Text required"))?;
        if (p["kind"] == "reply_and_close" && text.trim().is_empty()) || text.len() > 20000 {
            return Err(bad("Invalid reply text"));
        }
        let item=row(d,"items",required(&p,"itemId")?)?.clone();
        let origin=p.get("origin").cloned().unwrap_or_else(||p.clone());
        let before=p.clone();
        let p = row_mut(d, "proposals", &key)?;
        if !p["origin"].is_object(){p["origin"]=origin.clone();}
        if !p["history"].is_array(){p["history"]=json!([]);}
        let mut historical=before.clone();historical.as_object_mut().unwrap().remove("history");
        p["history"].as_array_mut().unwrap().push(historical);
        p["text"] = json!(text);
        p["status"] = json!("draft");
        bump(p);
        let after=p.clone();
        feedback::append(d,&body,&item,&origin,"draft_saved",json!({"before":before["text"],"after":after["text"],"proposalId":key,"proposalRevision":after["revision"]}))?;
        Ok(Json(after))
    })
    .await
}
fn proposal_current(d: &Value, p: &Value) -> ApiResult<Value> {
    proposal_current_with_context(p, &prepare_bundle::EvidenceContext::new(d))
}
fn proposal_current_with_context(p: &Value, context: &prepare_bundle::EvidenceContext<'_>) -> ApiResult<Value> {
    let d = context.workspace();
    if p["recovery"]["kind"]=="recovered_context_metadata" {
        let recovery=&p["recovery"];
        let bundle=&row(d,"jobs",required(recovery,"prepareRunId")?)?["prepareBundle"];
        if recovery["prepareBundleId"]!=bundle["id"] || recovery["prepareBundleDigest"]!=bundle["digest"]
            || !context.equivalent_saved_source(bundle,required(p,"itemId")?).map_err(conflict)? {
            return Err(conflict("Recovered preparation source changed"));
        }
    }
    if let Some(expected)=p["reviewContextDigest"].as_str() {
        if context.review_fingerprint(required(p,"itemId")?).map_err(conflict)?!=expected {return Err(conflict("Review source context changed"));}
    }
    if let Some(run_id) = p["prepareRunId"].as_str() {
        let bundle = &row(d,"jobs",run_id)?["prepareBundle"];
        if p["prepareBundleId"] != bundle["id"] || p["prepareBundleDigest"] != bundle["digest"] {
            return Err(conflict("Proposal preparation provenance changed"));
        }
        context.current(bundle).map_err(conflict)?;
    }
    let binding = active_binding(d)?;
    let item = bound_item(&binding, row(d, "items", required(p, "itemId")?)?)?;
    validate_route(p, &binding, &item)?;
    if !context.video_ready(&item).map_err(conflict)? {
        return Err(conflict("Video audio and visual evidence must be complete before approval or dispatch"));
    }
    if item["revision"] != p["itemRevision"]
        || item["contextEvidenceDigest"] != p["contextEvidenceDigest"]
        || item["branchContextDigest"] != p["branchContextDigest"]
    {
        return Err(conflict("Comment context changed; create a new proposal"));
    }
    if item["workflow"] == "closed" && !(p["allowClosedReply"]==true && p["kind"]=="reply_and_close") {
        return Err(conflict("Comment is already closed"));
    }
    Ok(item.clone())
}
fn create_approval(d: &mut Value, actor: &operator_auth::Actor, body: &Value) -> ApiResult<Value> {
        let refs = body["proposals"]
            .as_array()
            .filter(|v| !v.is_empty() && v.len() <= 100)
            .ok_or_else(|| bad("Choose 1 to 100 proposals"))?;
        let mut targets = vec![];
        let mut exact = vec![];
        for r in refs {
            let p = row(d, "proposals", required(r, "id")?)?;
            check_revision(p, &r["revision"])?;
            if !["draft", "approved"].contains(&p["status"].as_str().unwrap_or("")) {
                return Err(conflict("Proposal is no longer available"));
            }
            let item = proposal_current(d, p)?;
            if targets.contains(&p["itemId"]) {
                return Err(bad("One action per recipient required"));
            }
            targets.push(p["itemId"].clone());
            exact.push(json!({"id":p["id"],"revision":p["revision"],"proposal":p,"item":item}));
        }
        let key = id();
        let mut v = json!({"id":key,"proposals":exact,"status":"approved","createdAt":now(),"approvedBy":actor.public_json(),"approvalAuthority":dispatch_authority::approval_binding(&actor)});
        for r in refs {
            row_mut(d, "proposals", required(r, "id")?)?["status"] = json!("approved");
        }
        for exact in v["proposals"].as_array().unwrap(){
            let p=&exact["proposal"];let origin=p.get("origin").cloned().unwrap_or_else(||p.clone());
            feedback::append(d,&json!({"eventId":format!("review:{}:{}",key,p["id"].as_str().unwrap()),"draftSessionId":p["draftSessionId"],"_verifiedActor":actor.public_json()}),&exact["item"],&origin,"review_confirmed",json!({"action":p["kind"],"text":p["text"],"proposalId":p["id"],"proposalRevision":p["revision"],"approvalId":key,"recipient":exact["item"]["itemId"]}))?;
        }
        list_mut(d, "approvals").push(v.clone());
        audit(d, "approval.created", &key);
        v.as_object_mut().unwrap().remove("approvalAuthority");
        Ok(v)
}
async fn approval_new(State(app): State<App>, axum::Extension(actor): axum::Extension<operator_auth::Actor>, Json(body): Json<Value>) -> ApiResult<Json<Value>> {
    media_fullframes::refresh(&app).await?;
    app.change(|d| create_approval(d, &actor, &body).map(Json)).await
}

fn action_for(p: &Value, item: &Value, operation: &str) -> ApiResult<Value> {
    for key in [
        "itemId",
        "objectId",
        "postKey",
        "conversationKey",
        "contextEvidenceDigest",
    ] {
        required(item, key)?;
    }
    let status = item["providerStatus"]
        .as_str()
        .filter(|s| ["new", "inprogress"].contains(s)
            || (*s=="closed" && p["allowClosedReply"]==true && p["kind"]=="reply_and_close"))
        .ok_or_else(|| conflict("Provider status is not actionable"))?;
    let mut action = json!({"actionId":operation,"action":p["kind"],"itemId":item["itemId"],"objectId":item["objectId"],"conversationKey":item["conversationKey"],"contextEvidenceDigest":item["contextEvidenceDigest"],"expectedStatuses":[status],"workTime":0});
    if p["kind"] == "reply_and_close" {
        action["reply"] = p["text"].clone();
    }
    Ok(action)
}
fn check_approval_actor(approval: &Value, actor: &operator_auth::Actor) -> ApiResult<()> {
    let permitted = match approval["approvedBy"]["id"].as_str() {
        Some(owner) => owner == actor.id,
        None => actor.id == "local-owner" && actor.role == "owner",
    };
    if !permitted {
        return Err(ApiError(StatusCode::FORBIDDEN,
            "Review and confirm this action from your own session".into()));
    }
    Ok(())
}
fn dispatch_parallelism() -> ApiResult<usize> {
    match std::env::var("COMMUNITYHERO_MAX_IN_FLIGHT") {
        Err(std::env::VarError::NotPresent) => Ok(100),
        Ok(value) => value.parse::<usize>().ok().filter(|value| *value > 0)
            .ok_or_else(|| bad("COMMUNITYHERO_MAX_IN_FLIGHT must be a positive integer")),
        Err(_) => Err(bad("Invalid COMMUNITYHERO_MAX_IN_FLIGHT")),
    }
}
fn recipient_operation_blocks(prior: &Value, proposal: &Value, item: &Value) -> bool {
    if prior["itemId"] != proposal["itemId"] { return false; }
    match prior["status"].as_str() {
        Some("dispatching" | "unknown") => true,
        Some("succeeded") => {
            // A new explicit follow-up may follow a completed ordinary reply/close.
            // It never reuses the earlier approval, route or unresolved attempt.
            let fresh_followup = proposal["allowClosedReply"] == true
                && proposal["kind"] == "reply_and_close"
                && item["workflow"] == "closed" && item["providerStatus"] == "closed"
                && matches!(prior["action"]["action"].as_str(), Some("close" | "reply_and_close"))
                && item["revision"].as_u64().zip(prior["target"]["revision"].as_u64())
                    .is_some_and(|(current, previous)| current > previous);
            if !fresh_followup { return true; }
            let same_route = || -> Result<bool, connectors::BoundaryError> {
                let binding = ConnectorBinding::from_json(&item["connectorBinding"])?;
                Ok(ResourceRef::from_item(&binding, item)?
                    == ResourceRef::from_item(&binding, &prior["target"])?)
            };
            !same_route().unwrap_or(false)
        }
        _ => false,
    }
}
async fn execute(State(app): State<App>, axum::Extension(actor): axum::Extension<operator_auth::Actor>, Path(key): Path<String>) -> ApiResult<Json<Value>> {
    media_fullframes::refresh(&app).await?;
    app.check_execution()?;
    let parallelism = dispatch_parallelism()?;
    let (job,operations)=app.change(|d| {let approval=row(d,"approvals",&key)?.clone();check_approval_actor(&approval,&actor)?;let authority=dispatch_authority::admit(&approval,&actor)?;if approval["status"]!="approved" {return Err(conflict("Approval already consumed; reconcile unresolved operations"));}let mut operations=vec![];for r in approval["proposals"].as_array().unwrap(){let p=row(d,"proposals",required(r,"id")?)?;check_revision(p,&r["revision"])?;if p["status"]!="approved"{return Err(conflict("Approval invalidated"));}let item=proposal_current(d,p)?;if list(d,"operations").iter().any(|o|recipient_operation_blocks(o,p,&item)){return Err(conflict("Recipient already has an unresolved or completed operation"));}let op=id();let action=action_for(p,&item,&op)?;operations.push(json!({"id":op,"approvalId":key,"proposalId":p["id"],"itemId":p["itemId"],"action":action,"target":item,"status":"dispatching","attemptId":id(),"createdAt":now(),"approvedBy":approval["approvedBy"],"executedBy":actor.public_json(),"dispatchAuthority":authority}));}let job=new_job(d,"execute",&key)?;row_mut(d,"approvals",&key)?["status"]=json!("consumed");for op in &operations{row_mut(d,"proposals",required(op,"proposalId")?)?["status"]=json!("dispatching");list_mut(d,"operations").push(op.clone());audit(d,"operation.admitted",required(op,"id")?);}Ok((job,operations))}).await?;
    let worker = app.clone();
    app.spawn(job.clone(), async move {
        // Distinct reviewers share one provider account. Queue approved batches
        // without holding the workspace transaction/UI gate during network I/O.
        let _dispatch_guard = worker.execution_gate.lock().await;
        dispatch_wave::run(worker.clone(), operations, parallelism).await
    });
    Ok(Json(json!({"jobId":job})))
}
async fn set_outcome(app: &App, op: &Value, status: &str, evidence: Value) -> ApiResult<()> {
    app.change(|d| {
        let key = required(op, "id")?;
        let stored = row_mut(d, "operations", key)?;
        stored["status"] = json!(status);
        stored["evidence"] = evidence;
        stored["updatedAt"] = json!(now());
        row_mut(d, "proposals", required(op, "proposalId")?)?["status"] = json!(status);
        if status == "succeeded" && outcome_matches_item(d, op) {
            let item = row_mut(d, "items", required(op, "itemId")?)?;
            let deleted=op["action"]["action"]=="delete";
            item["providerStatus"] = json!(if deleted {"deleted"} else {"closed"});
            if op["action"]["action"]=="hide" {item["hidden"]=json!(true);}
            if item["workflow"] != "waiting" {
                item["workflow"] = json!(if deleted {"deleted"} else {"closed"});
            }
            bump(item);
        }
        feedback::outcome(d,op,status)?;
        audit(d, &format!("operation.{status}"), key);
        Ok(())
    })
    .await
}
fn context_matches(context: &Value, target: &Value) -> bool {
    [
        "itemId",
        "objectId",
        "postKey",
        "conversationKey",
        "contextEvidenceDigest",
    ]
    .iter()
    .all(|k| context[*k].is_string() && context[*k] == target[*k])
}
fn readback_confirmed(v: &Value, action: &Value) -> bool {
    v["results"].as_array().is_some_and(|rows| {
        rows.len() == 1
            && rows[0]["actionId"] == action["actionId"]
            && rows[0]["itemId"] == action["itemId"]
            && rows[0]["status"] == "verified"
    })
}
async fn dispatch(app: App, mut op: Value) -> ApiResult<bool> {
    if let Some(blocker)=dispatch_evidence::conversation_blocker(&app.read().await?,&op) {
        set_outcome(&app,&op,"stale",json!({"reason":"Earlier reply in this conversation requires readback",
            "blockedByOperationId":blocker,"providerCallAttempted":false})).await?;
        return Ok(true);
    }
    if !dispatch_authority::permit(&app, &op).await? { return Ok(true); }
    let account = operation_account(&op)?;
    let context=app.bridge("context",json!({"account":account,"itemId":op["target"]["itemId"],"objectId":op["target"]["objectId"]})).await;
    if !context
        .as_ref()
        .is_ok_and(|v| context_matches(v, &op["target"]))
    {
        set_outcome(
            &app,
            &op,
            "stale",
            json!({"reason":"Fresh context unavailable or changed; no dispatch"}),
        )
        .await?;
        return Ok(true);
    }
    let still_current = app
        .read().await.and_then(|d| {
            let p = row(&d, "proposals", required(&op, "proposalId")?)?;
            let current = proposal_current(&d, p)?;
            if current["revision"] != op["target"]["revision"]
                || current["contextEvidenceDigest"] != op["target"]["contextEvidenceDigest"]
            {
                return Err(conflict("Target changed before dispatch"));
            }
            Ok(())
        });
    if still_current.is_err() {
        set_outcome(
            &app,
            &op,
            "stale",
            json!({"reason":"Local target changed before dispatch; no external call"}),
        )
        .await?;
        return Ok(true);
    }
    if op["action"]["action"]=="reply_and_close" {
        if let Some(evidence)=context.as_ref().ok()
            .and_then(|value| dispatch_evidence::reply_baseline(&value["officialReplyIds"])) {
            dispatch_evidence::persist(&app,&mut op,evidence).await?;
        }
    }
    // Context, workspace and durable baseline checks all await. Recheck actual
    // authority after the final write, directly before the provider call.
    if !dispatch_authority::permit(&app, &op).await? { return Ok(true); }
    let result = app
        .bridge(
            "execute",
            json!({"account":account,"actions":[op["action"]]}),
        )
        .await;
    if op["action"]["action"]=="reply_and_close" {
        if let Some(evidence)=result.as_ref().ok()
            .and_then(|value| dispatch_evidence::from_receipt(value,&op["action"],account)) {
            dispatch_evidence::persist(&app,&mut op,evidence).await?;
        }
    }
    let confirmed_failure=result.as_ref().is_ok_and(|value|
        dispatch_evidence::confirmed_failure(value,&op["action"],account));
    let receipt=match &result {Ok(value)=>value.clone(),Err(error)=>json!({"error":error.1})};
    dispatch_evidence::record_execute(&app,&op,receipt.clone()).await?;
    if confirmed_failure {
        set_outcome(&app,&op,"failed",json!({"receipt":receipt,"providerRetryAllowed":false})).await?;
        return Ok(true);
    }
    // A successful/ambiguous receipt is insufficient: readback proves effects.
    set_outcome(
        &app,
        &op,
        "unknown",
        match result {
            Ok(v) => json!({"receipt":v}),
            Err(e) => json!({"error":e.1}),
        },
    )
    .await?;
    reconcile_one(&app, &op).await
}
async fn reconcile_one(app: &App, op: &Value) -> ApiResult<bool> {
    let account = operation_account(op)?;
    let result = app
        .bridge(
            "readback",
            json!({"account":account,"actions":[op["action"]]}),
        )
        .await;
    match result {
        Ok(v) if readback_confirmed(&v, &op["action"]) => {
            set_outcome(app, op, "succeeded", v).await?;
            Ok(true)
        }
        Ok(v) => {
            set_outcome(app, op, "unknown", v).await?;
            Ok(false)
        }
        Err(e) => {
            set_outcome(app, op, "unknown", json!({"error":e.1})).await?;
            Ok(false)
        }
    }
}
async fn reconcile(State(app): State<App>, axum::Extension(actor): axum::Extension<operator_auth::Actor>, Path(key): Path<String>) -> ApiResult<Json<Value>> {
    let (job, op) = app
        .change(|d| {
            let op = row(d, "operations", &key)?.clone();
            if op["status"] != "unknown" {
                return Err(conflict("Only unknown operations need reconciliation"));
            }
            if list(d, "jobs")
                .iter()
                .any(|j| j["kind"] == "reconcile" && j["refId"] == key && j["status"] == "running")
            {
                return Err(conflict("Reconciliation already running"));
            }
            let job = new_job(d, "reconcile", &key)?;
            row_mut(d, "jobs", &job)?["requestedBy"] = actor.public_json();
            Ok((job, op))
        })
        .await?;
    let worker = app.clone();
    app.spawn(job.clone(), async move {
        let confirmed = reconcile_one(&worker, &op).await?;
        Ok(json!({"confirmed":confirmed}))
    });
    Ok(Json(json!({"jobId":job})))
}

async fn feedback_event(State(app):State<App>,Json(body):Json<Value>)->ApiResult<Json<Value>> {
    let key=required(&body,"itemId")?.to_owned();
    app.change_item(&key,|d| feedback::client_event(d,&body).map(Json)).await
}
async fn feedback_report(State(app):State<App>)->ApiResult<Json<Value>> {
    Ok(Json(feedback_reporting::report(&app.read().await?)))
}
async fn knowledge_catalog(State(app): State<App>) -> ApiResult<Json<Value>> {
    Ok(Json(app.db.read_knowledge_catalog(true).await?))
}
async fn knowledge_revision(State(app): State<App>,Path(key): Path<String>,Json(body): Json<Value>) -> ApiResult<Json<Value>> {
    app.change(|d| {
        let version=knowledge::revise(d,&key,&body,&now()).map_err(conflict)?;
        auto_prepare::reconcile_stale(d,chrono::Utc::now().timestamp());
        audit(d,"knowledge.version",&key);
        Ok(Json(version))
    }).await
}

async fn knowledge_instruction(State(app): State<App>, Json(body): Json<Value>) -> ApiResult<Json<Value>> {
    app.change(|d| {
        let result = knowledge::save_instruction(d, &body, &now()).map_err(conflict)?;
        if result["replayed"] != true {
            auto_prepare::reconcile_stale(d, chrono::Utc::now().timestamp());
            audit(d, "knowledge.instruction", result["entry"]["id"].as_str().unwrap());
        }
        Ok(Json(result))
    }).await
}

async fn knowledge_normalization(State(app): State<App>, axum::Extension(actor): axum::Extension<operator_auth::Actor>, Json(body): Json<Value>) -> ApiResult<Json<Value>> {
    if actor.role!="owner" {return Err(ApiError(StatusCode::FORBIDDEN,"Owner required".into()));}
    let action=required(&body,"action")?;
    if !matches!(action,"apply"|"rollback") {return Err(bad("Unknown normalization action"));}
    let reviewed=required(&body,"reviewedPlanHash")?;
    let dry=body["dryRun"].as_bool().ok_or_else(||bad("Explicit dryRun required"))?;
    let reduce=|d:&mut Value|->ApiResult<Value>{
        match action {
            "apply"=>knowledge::rule_normalization::apply(d,&body["plan"],reviewed,&now()),
            _=>knowledge::rule_normalization::rollback(d,&body["plan"],reviewed,&now()),
        }.map_err(conflict)
    };
    if dry {
        let mut candidate=app.read().await?;
        return Ok(Json(json!({"dryRun":true,"receipt":reduce(&mut candidate)?})));
    }
    app.change(|d| {
        let result=reduce(d)?;
        if result["replayed"]!=true {
            auto_prepare::reconcile_stale(d,chrono::Utc::now().timestamp());
            audit(d,"knowledge.normalization",result["requestId"].as_str().unwrap());
        }
        Ok(Json(json!({"dryRun":false,"receipt":result})))
    }).await
}

async fn material_new(State(app): State<App>, Json(body): Json<Value>) -> ApiResult<Json<Value>> {
    let title = required(&body, "title")?.to_string();
    let text = required(&body, "text")?.to_string();
    if body["kind"] != "knowledge" {
        return Err(bad("Manual material must be knowledge"));
    }
    app.change(|d|{let v=json!({"id":id(),"title":title,"text":text,"kind":"knowledge","sourceUrl":body["sourceUrl"],"revision":1,"updatedAt":now()});list_mut(d,"materials").push(v.clone());knowledge::sync_catalog(d,&now()).map_err(bad)?;Ok(Json(v))}).await
}
async fn material_patch(
    State(app): State<App>,
    Path(key): Path<String>,
    Json(body): Json<Value>,
) -> ApiResult<Json<Value>> {
    app.change(|d| {
        let m = row_mut(d, "materials", &key)?;
        if m["manualInstruction"] == true {
            return Err(conflict("Edit manual instructions through the versioned instruction endpoint"));
        }
        check_revision(m, &body["expectedRevision"])?;
        m["title"] = json!(required(&body, "title")?);
        m["text"] = json!(required(&body, "text")?);
        m["updatedAt"] = json!(now());
        m["locallyEdited"] = json!(true);
        bump(m);
        let result=m.clone();
        knowledge::sync_catalog(d,&now()).map_err(bad)?;
        auto_prepare::reconcile_stale(d,chrono::Utc::now().timestamp());
        Ok(Json(result))
    })
    .await
}
fn merge_materials(d: &mut Value, result: &Value) -> ApiResult<()> {
    let materials = result["materials"]
        .as_array()
        .ok_or_else(|| bad("Adapter returned no material list"))?;
    for source in materials {
        if knowledge::company_import::has_authority(d)
            && !matches!(source["kind"].as_str(), Some("transcript" | "ocr" | "visual_context")) { continue; }
        let mut v = source.clone();
        let source_id = required(&v, "id")?.to_string();
        v["id"] = json!(format!("import-{source_id}"));
        if knowledge::company_import::is_managed_material(d, v["id"].as_str().unwrap()) { continue; }
        v["imported"] = json!(true);
        v["updatedAt"] = json!(now());
        v["revision"] = json!(1);
        let target = list_mut(d, "materials");
        if let Some(old) = target.iter_mut().find(|m| m["id"] == v["id"]) {
            if old["locallyEdited"] == true {
                old["upstreamText"] = v["text"].clone();
                old["upstreamChanged"] = json!(old["text"] != v["text"]);
                continue;
            }
            if v.get("transcription").is_none() && old["text"] == v["text"] && old["sourceUrl"] == v["sourceUrl"] {
                if let Some(metadata)=old.get("transcription") {v["transcription"]=metadata.clone();}
            }
            if ["text", "title", "sourceUrl", "postKey", "kind", "transcription", "visualEvidence", "mediaSha256"]
                .iter().any(|key| old[*key] != v[*key]) {
                v["revision"] = json!(old["revision"].as_u64().unwrap_or(0) + 1);
                *old = v;
            }
        } else {
            target.push(v);
        }
    }
    knowledge::sync_catalog(d,&now()).map_err(bad)?;
    auto_prepare::reconcile_stale(d,chrono::Utc::now().timestamp());
    Ok(())
}
async fn materials_import(State(app): State<App>) -> ApiResult<Json<Value>> {
    let snapshot = app.read().await?;
    if knowledge::company_import::has_authority(&snapshot) {
        return Ok(Json(json!({"imported":0,"authority":"communityhero","legacyImportSuppressed":true})));
    }
    let binding = active_binding(&snapshot)?;
    let account = bridge_account(&binding)?;
    let job = app.job("materials", "").await?;
    let worker = app.clone();
    app.spawn(job.clone(), async move {
        let result = worker
            .bridge("materials", json!({"account":account}))
            .await?;
        worker.change(|d| merge_materials(d, &result)).await?;
        Ok(json!({"imported":result["materials"].as_array().map_or(0,Vec::len)}))
    });
    Ok(Json(json!({"jobId":job})))
}
async fn media(State(app): State<App>, Json(body): Json<Value>) -> ApiResult<Json<Value>> {
    let key = required(&body, "postId")?.to_string();
    Ok(Json(media_queue::request(&app, &key).await?))
}
async fn backup(State(app): State<App>) -> ApiResult<Json<Value>> {
    let _guard = app.gate.acquire(writer_gate::Class::Standard).await;
    let folder = app.data.join("backups");
    std::fs::create_dir_all(&folder).map_err(|_| internal("Backup directory unavailable"))?;
    let path = app.db.backup(&folder).await?;
    Ok(Json(json!({"createdAt":now(),"path":path})))
}

fn routes(app: App, web: PathBuf) -> Router {
    Router::new()
        .route("/api/health", get(health))
        .route("/api/engine/status", get(accounts::status))
        .route("/api/engine/prepare", post(engine_prepare::prepare))
        .route("/api/engine/jobs/{id}", get(engine_api::job))
        .route("/api/engine/capabilities", get(engine_api::provider_capabilities))
        .route("/api/engine/scan", post(engine_api::scan))
        .route("/api/engine/export", get(engine_api::export))
        .route("/api/ready", get(readiness::ready))
        .route("/api/bootstrap", get(operator_http::bootstrap))
        .route("/api/bootstrap/delta", get(operator_http::bootstrap_delta))
        .route("/api/workspace-version", get(workspace_version::get))
        .route("/api/session", get(operator_http::session))
        .route("/api/session/login", post(operator_http::login))
        .route("/api/session/logout", post(operator_http::logout))
        .route("/api/events", get(events))
        .route("/api/sync", post(sync))
        .route("/api/archive/import", post(archive_import::import).get(archive_import::status))
        .route("/api/items/{id}", patch(operator_http::item_patch))
        .route("/api/items/search", get(operator_http::search))
        .route("/api/conversations", post(conversation_new))
        .route(
            "/api/conversations/{id}/messages",
            post(conversation_message),
        )
        .route("/api/jobs/{id}/cancel", post(cancel))
        .route("/api/proposals", post(operator_http::proposal_new))
        .route("/api/maintenance/recover-prepared", post(recover_prepared))
        .route("/api/maintenance/revalidation/recover-rejected", post(recover_rejected_revalidation))
        .route("/api/maintenance/preparation", post(auto_prepare::configure).get(auto_prepare::status))
        .route("/api/maintenance/preparation/restart", post(preparation_restart::restart))
        .route("/api/maintenance/media/retry-interrupted", post(media_queue::retry_interrupted))
        .route("/api/maintenance/media/retry-download-failed", post(media_queue::retry_failed_download))
        .route("/api/maintenance/repair-brand-roles", post(brand_repair::repair))
        .route("/api/proposals/{id}", patch(operator_http::proposal_patch))
        .route("/api/approvals", post(approval_new))
        .route("/api/approvals/{id}/execute", post(execute))
        .route("/api/operations/{id}/reconcile", post(reconcile))
        .route("/api/feedback/events", post(operator_http::feedback_event))
        .route("/api/feedback/report", get(feedback_report))
        .route("/api/knowledge", get(operator_http::knowledge_catalog))
        .route("/api/knowledge/instructions", get(operator_http::instruction_catalog).post(knowledge_instruction))
        .route("/api/maintenance/knowledge-normalization", post(knowledge_normalization))
        .route("/api/knowledge/{id}/versions", post(knowledge_revision))
        .route("/api/materials", post(material_new))
        .route("/api/materials/{id}", patch(material_patch))
        .route("/api/materials/import", post(materials_import))
        .route("/api/materials/process", post(media))
        .route("/api/backup", post(backup))
        .fallback_service(
            ServeDir::new(&web).not_found_service(ServeFile::new(web.join("index.html"))),
        )
        .layer(DefaultBodyLimit::max(2 * 1024 * 1024))
        // Negotiate transport compression for large JSON and static assets.
        // The default predicate excludes event-stream responses so refresh
        // notifications keep streaming without compression buffering.
        .layer(CompressionLayer::new())
        .layer(middleware::from_fn_with_state(app.clone(), security))
        .with_state(app)
}

async fn open_db(path: &std::path::Path) -> ApiResult<SqlitePool> {
    let db = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(
            SqliteConnectOptions::new()
                .filename(path)
                .create_if_missing(true)
                .journal_mode(SqliteJournalMode::Wal)
                .busy_timeout(Duration::from_secs(5)),
        )
        .await?;
    sqlx::query("CREATE TABLE IF NOT EXISTS workspace(id INTEGER PRIMARY KEY CHECK(id=1),payload TEXT NOT NULL)").execute(&db).await?;
    sqlx::query("INSERT OR IGNORE INTO workspace(id,payload) VALUES(1,?)")
        .bind(empty().to_string())
        .execute(&db)
        .await?;
    Ok(db)
}
fn recover(d: &mut Value) {
    for job in list_mut(d, "jobs") {
        if job["status"] == "running" || job["status"] == "queued" {
            job["status"] = json!("interrupted");
            job["finishedAt"] = json!(now());
            job["error"] = json!("Server restarted during job");
        }
    }
    let mut interrupted_operations=vec![];
    for op in list_mut(d, "operations") {
        if op["status"] == "dispatching" {
            interrupted_operations.push(op.clone());
            op["status"] = json!("unknown");
            op["updatedAt"] = json!(now());
        }
    }
    for p in list_mut(d, "proposals") {
        if p["status"] == "dispatching" {
            p["status"] = json!("unknown");
        }
    }
    for op in interrupted_operations {let _=feedback::outcome(d,&op,"unknown");}
    auto_prepare::recover_jobs(d,chrono::Utc::now().timestamp());
    auto_prepare::reconcile_stale(d,chrono::Utc::now().timestamp());
}
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    if std::env::args_os().nth(1).as_deref() == Some(std::ffi::OsStr::new("knowledge-import")) {
        return company_knowledge_cli::run(std::env::args_os().skip(2)).await;
    }
    dispatch_parallelism().map_err(|error| error.1)?;
    let selected_account = accounts::Profile::parse(
        &std::env::var("COMMUNITYHERO_ACCOUNT").unwrap_or_else(|_| "likeavto".into())
    ).map_err(|error| error.1)?;
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .to_path_buf();
    let data = std::env::var_os("COMMUNITYHERO_DATA_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| root.join("data"));
    std::fs::create_dir_all(&data)?;
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(data.join("server.lock"))?;
    lock.try_lock()
        .map_err(|_| "Another CommunityHero server owns this database")?;
    let port = std::env::var("COMMUNITYHERO_PORT")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(4186);
    let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, port)).await?;
    let db = match std::env::var("COMMUNITYHERO_DATABASE_URL") {
        Ok(url) => Database::postgres(&url).await.map_err(|e| e.1)?,
        Err(std::env::VarError::NotPresent) => Database::Sqlite(
            open_db(&data.join("workspace.sqlite"))
                .await
                .map_err(|e| e.1)?,
        ),
        Err(_) => return Err("Invalid database configuration".into()),
    };
    let (events, _) = broadcast::channel(32);
    let node=std::env::var_os("COMMUNITYHERO_NODE").map(PathBuf::from).unwrap_or_else(||PathBuf::from("C:/AIDev/Workspaces/repos/Angry.Space.Auto-symphony/data/private/angryspace-conveyor/provider-runtime/bundle/node.exe"));
    let auth = operator_auth::Auth::load(&data).await?;
    let public_origin = operator_http::configured_origin(auth.is_some())?;
    let app = App {
        account: selected_account,
        db,
        gate: Arc::new(writer_gate::WriterGate::default()),
        execution_gate: Arc::new(Mutex::new(())),
        assistant_gate: Arc::new(Mutex::new(())),assistant_chat_gate: Arc::new(Mutex::new(())),
        events,
        csrf: id(),
        auth,
        public_origin,
        external_writes: std::env::var("COMMUNITYHERO_EXTERNAL_WRITES").as_deref() == Ok("enabled"),
        port,
        data,
        bridge: std::env::var_os("COMMUNITYHERO_BRIDGE")
            .map(PathBuf::from)
            .unwrap_or_else(|| root.join("adapters/bridge.mjs")),
        node,
        tasks: Arc::new(Mutex::new(HashMap::new())),bootstrap_cache:Arc::new(bootstrap_cache::Cache::default()),
    };
    app.change(|d| {
        accounts::initialize(d, selected_account)?;
        knowledge::sync_catalog(d,&now()).map_err(bad)?;
        recover(d);
        assistant_action_review::recover_execution_receipts(d)?;
        media_queue::recover(d, &now())?;
        Ok(())
    })
    .await
    .map_err(|e| e.1)?;
    // Artifact availability is separate from automatic provider/model work.
    // Verification runs outside the writer, even when queue automation is off.
    let media_proofs=app.clone();
    tokio::spawn(worker_supervision::supervise_background("media-proof",move||{
        let media_proofs=media_proofs.clone();async move {
            let mut interval=tokio::time::interval(Duration::from_secs(15));
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                interval.tick().await;
                if let Err(error)=media_fullframes::refresh(&media_proofs).await {
                    eprintln!("Media proof refresh: {}",error.1);
                }
                media_proofs.observe_media_proofs();
            }
        }
    }));
    if std::env::var("COMMUNITYHERO_BACKGROUND_DISABLED").as_deref()!=Ok("1") {
    let periodic = app.clone();
    tokio::spawn(worker_supervision::supervise_background("sync", move || { let periodic=periodic.clone(); async move {
        let mut interval = tokio::time::interval(Duration::from_secs(5));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            interval.tick().await;
            if let Err(error) = sync_scan::tick(&periodic).await {
                eprintln!("Background queue synchronization: {}", error.1);
            }
        }
    }}));
    // A local review may need fresh provider statuses without starting paid
    // preparation/transcription work. Keep synchronization independently usable.
    if std::env::var("COMMUNITYHERO_BACKGROUND_GENERATION_DISABLED").as_deref()!=Ok("1") {
    let automatic = app.clone();
    tokio::spawn(worker_supervision::supervise_background("preparation", move || { let automatic=automatic.clone(); async move {
        let mut interval = tokio::time::interval(Duration::from_secs(15));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            interval.tick().await;
            if let Err(error) = auto_prepare::tick(&automatic).await {
                eprintln!("Automatic preparation queue: {}", error.1);
            }
        }
    }}));
    let automatic_media = app.clone();
    tokio::spawn(worker_supervision::supervise_background("media", move || { let automatic_media=automatic_media.clone(); async move {
        let mut interval = tokio::time::interval(Duration::from_secs(15));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            interval.tick().await;
            if let Err(error) = media_queue::tick(&automatic_media).await {
                eprintln!("Automatic media queue: {}", error.1);
            }
        }
    }}));
    }
    }
    println!("CommunityHero {} workspace: http://127.0.0.1:{port}", selected_account.display());
    let web = std::env::var_os("COMMUNITYHERO_WEB_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| root.join("workshop"));
    axum::serve(listener, routes(app, web)).await?;
    drop(lock);
    Ok(())
}
