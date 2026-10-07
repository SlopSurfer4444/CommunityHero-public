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
mod native_vk;
mod accounts;
mod account_navigation;
mod working_generation;
mod continuous_preparation;
mod connection_gate;
mod connection_recovery;
mod dispatch_transport;
mod external_reconciliation;
mod engine_api;
mod engine_prepare;
mod prepare_plan;
mod preparation_unit;
mod preparation_materials;
mod model_material_receipt;
mod answering_repair_plan;
mod video_frame_work;
mod source_snapshot_scoped;
mod storage;
mod performance;
mod trace_context;
#[cfg(test)]
#[path = "dispatch_performance_tests.rs"]
mod dispatch_performance_tests;
mod writer_gate;
mod db_guards;
mod bootstrap_cache;
mod workspace_delta;
mod workspace_version;
mod readiness;
mod runtime_mode;
mod runtime_lifecycle;
mod runtime_owned_work;
mod runtime_maintenance;
mod runtime_native_child;
mod runtime_lifecycle_app;
mod runtime_lifecycle_startup;
mod predecessor_recovery;
mod runtime_bootstrap_ledger_cli;
mod runtime_lifecycle_http;
mod runtime_paid_result;
mod runtime_lifecycle_backlog;
#[cfg(test)] mod runtime_startup_recovery_tests;
mod worker_supervision;
#[cfg(test)] mod worker_supervision_fault_tests;
mod sync_scan;
mod fast_status;
mod snapshot_order;
mod snapshot_domain;
#[cfg(test)] mod snapshot_domain_regression_tests;
mod archive_import;
mod brand_repair;
mod thread_graph;
mod prepare_bundle;
mod bounded_review;
mod operator_batch;
mod local_admission;
mod execute_admission;
mod approval_admission;
mod auto_prepare;
mod preparation_review;
mod preparation_reservations;
mod retained_paid_recovery;
mod retained_paid_recovery_registry;
mod retained_paid_recovery_endpoint;
mod preparation_workers;
mod codex_model_policy;
mod preparation_restart;
mod research_cache;
mod fact_followup;
mod customer_case_context;
mod knowledge;
mod company_knowledge_cli;
mod instruction_install_cli;
mod shared_moderation_install_cli;
#[cfg(test)] mod company_load_acceptance_tests;
mod media_queue;
mod media_status;
mod media_source_import;
mod photo_acquisition;
mod media_processing;
mod media_speech_assets;
mod manual_frame_request;
mod media_analysis;
mod media_analysis_runtime;
mod media_analysis_reuse;
mod media_visual;
mod media_artifacts;
mod media_frame_contract;
mod media_fullframes;
mod media_vision_admission;
mod post_media_policy;
mod media_audio_equivalence;
mod media_frame_decoder;
mod media_frame_sample;
mod media_frame_sample_decode;
mod media_frame_selection;
#[cfg(test)] mod media_persistence_tests;
#[cfg(test)] mod media_restored_acceptance_tests;
mod feedback;
mod feedback_reporting;
mod operator_auth;
mod dispatch_authority;
mod conductor_authority;
mod conductor;
mod conductor_http;
mod conductor_child;
mod dispatch_evidence;
mod dispatch_diagnostics;
mod target_refresh;
mod connector_auth_status;
mod reply_constraints;
mod editorial_review;
mod decision_media;
mod proposal_revalidation;
mod proposal_source_rebind;
mod media_context_gate;
mod editorial_endpoint;
mod editorial_repair;
mod operator_editorial;
mod operator_frontier;
mod operator_close;
mod unknown_reply_close;
mod provider_session;
mod readback_recovery;
mod dispatch_wave;
mod operator_http;
mod assistant_context;
mod assistant_dialogue;
mod assistant_tools;
mod assistant_action_review;
#[cfg(test)] mod operator_http_tests;
#[cfg(test)] mod finish_storage_tests;
#[cfg(test)] mod wave_integration_tests;
#[cfg(test)] mod native_fixture_owner_repair;
#[cfg(test)] mod wave_execution_fixture_tests;
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
    async fn stop_and_wait(&self) -> ApiResult<()> {
        use windows_sys::Win32::System::JobObjects::*;
        let deadline=tokio::time::Instant::now()+Duration::from_secs(30);
        loop {
            let mut info:JOBOBJECT_BASIC_ACCOUNTING_INFORMATION=unsafe{std::mem::zeroed()};
            let observed=unsafe{QueryInformationJobObject(self.0 as _,JobObjectBasicAccountingInformation,
                &mut info as *mut _ as *mut _,std::mem::size_of_val(&info) as u32,std::ptr::null_mut())};
            if observed==0{return Err(internal("Adapter process containment outcome unknown"));}
            if info.ActiveProcesses==0{return Ok(());}
            unsafe{TerminateJobObject(self.0 as _,1);}
            if tokio::time::Instant::now()>=deadline{return Err(internal("Adapter process containment still active"));}
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }
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
    json!({"account":"LikeAvto","items":[],"posts":[],"branches":[],"conversations":[],"proposals":[],"approvals":[],"operations":[],"materials":[],"jobs":[],"audit":[],"settings":{"mediaContextStrict":true,"provider":"LikeAvto","assistant":"local-codex","externalWrites":"explicit-confirmation","concurrencyBoundary":"Use LikeAvto as the sole active operator. Known conveyor processes are rejected; no shared cross-application lock is available.","adapterStatus":"Capabilities are verified by the latest jobs; configuration alone is not readiness."},"sync":{"status":"never"}})
}

#[derive(Clone)]
struct App {
    media_discovery: Arc<media_queue::Discovery>,
    // Startup validates this immutable identity against the database binding.
    // Bridge calls must not re-read/deserialize the full workspace to obtain it.
    account: accounts::Profile,
    navigation: account_navigation::Navigation,
    db: Database,
    gate: Arc<writer_gate::WriterGate>,
    execution_gate: Arc<Mutex<()>>,
    assistant_gate: Arc<Mutex<()>>,
    preparation_workers: Arc<preparation_workers::Pool>,
    editorial_gate: Arc<Mutex<()>>,
    assistant_chat_gate: Arc<Mutex<()>>,
    preparation_wake: Arc<tokio::sync::Notify>,
    events: broadcast::Sender<()>,
    csrf: String,
    auth: Option<operator_auth::Auth>,
    public_origin: Option<String>,
    external_writes: bool,
    port: u16,
    data: PathBuf,
    bridge: PathBuf,
    node: PathBuf,
    lifecycle_admission: Arc<runtime_lifecycle_startup::Admission>,
    lifecycle_task_count: Arc<std::sync::atomic::AtomicUsize>,
    lifecycle_owner: Arc<runtime_lifecycle::RuntimeIdentity>,
    lifecycle_provider_token: Arc<Mutex<Option<(runtime_lifecycle::OwnerToken, provider_session::ProviderDrainToken, runtime_owned_work::DrainToken)>>>,
    lifecycle_work: runtime_owned_work::Registry,
    provider_session: provider_session::ProviderSession,
    tasks: Arc<Mutex<HashMap<String, tokio::task::AbortHandle>>>, 
    bootstrap_cache: Arc<bootstrap_cache::Cache>,
}
impl App {
    async fn trace_for_job(&self,job:&str,inherited:Option<trace_context::TraceContext>)->Option<trace_context::TraceContext>{
        if !performance::enabled(){return None;}
        let pins=self.lifecycle_admission.trace_release()?;
        let metadata=self.db.read_metadata().await.ok()?;
        let owner=runtime_lifecycle::current_owner(&metadata,&self.lifecycle_owner).ok()?;
        if owner.release_sha256!=pins.core_sha256{return None;}
        let inherited=inherited.filter(|context|{
            let value=context.to_json();
            value["companyKey"]==self.account.key()&&value["runtimeId"]==owner.runtime_id
                &&value["runtimeEpoch"]==owner.epoch&&value["sourcePin"]==pins.source_checkpoint_sha256
                &&value["lineage"]["coreSha256"]==pins.core_sha256&&value["lineage"]["binarySha256"]==pins.binary_sha256
        });
        let context=inherited.or_else(||trace_context::TraceContext::root(self.account.key(),&owner.runtime_id,
            owner.epoch,&pins.source_checkpoint_sha256))?;
        context.with_job(job)?.with_lineage("binarySha256",&pins.binary_sha256)?.with_lineage("coreSha256",&pins.core_sha256)
    }
    async fn lifecycle_admission_token(&self, class: runtime_lifecycle::AdmissionClass) -> ApiResult<runtime_lifecycle::OwnerToken> {
        runtime_maintenance::admission_token(self, class).await
    }
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
        let lifecycle_capture = runtime_lifecycle_app::Capture::read(self).await;
        let _timing=performance::Span::new("assistant.dialogue.change.total");
        let waiting=performance::Span::new("assistant.dialogue.writer.wait");
        let _guard = self.gate.acquire(writer_gate::Class::Interactive).await;
        drop(waiting);
        let (result, changed) = self.db.change_assistant_dialogue_observed(job, conversation, extra_ids, |d| lifecycle_capture.with(d, |d| f(d))).await?;
        if changed { self.bootstrap_cache.invalidate(); let _ = self.events.send(()); }
        Ok(result)
    }
    async fn change_assistant<T>(&self, job: Option<&str>, conversation: &str, f: impl FnOnce(&mut Value) -> ApiResult<T>) -> ApiResult<T> {
        let lifecycle_capture = runtime_lifecycle_app::Capture::read(self).await;
        let _timing=performance::Span::new("assistant.change.total");
        let waiting=performance::Span::new("assistant.writer.wait");
        let _guard = self.gate.acquire(writer_gate::Class::Interactive).await;
        drop(waiting);
        let (result, changed) = self.db.change_assistant_observed(job, conversation, |d| lifecycle_capture.with(d, |d| f(d))).await?;
        if changed { self.bootstrap_cache.invalidate(); let _ = self.events.send(()); }
        Ok(result)
    }
    async fn create_conversation(&self, actor: &operator_auth::Actor, title: &str, item_ids: &[Value]) -> ApiResult<Value> {
        let _total = performance::Span::new("conversation.create.total");
        let waiting = performance::Span::new("conversation.create.writer_wait");
        let _guard = self.gate.acquire(writer_gate::Class::Interactive).await;
        drop(waiting);
        let conversation = self.db.create_conversation(&actor.id, title, item_ids, &self.lifecycle_owner).await?;
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
    async fn change_conductor_start<T>(&self, body: &Value, actor: &operator_auth::Actor, start_hash: &str,
        f: impl FnOnce(&mut Value) -> ApiResult<T>) -> ApiResult<T> {
        let lifecycle_capture = runtime_lifecycle_app::Capture::read(self).await;
        let _total = performance::Span::new("conductor.start.change.total");
        let waiting = performance::Span::new("conductor.start.writer.wait");
        let _guard = self.gate.acquire(writer_gate::Class::Standard).await;
        drop(waiting);
        let _held = performance::Span::new("conductor.start.writer.held");
        let (result, changed) = self.db.change_conductor_start_observed(body, actor, start_hash, |d| lifecycle_capture.with(d, |d| f(d))).await?;
        if changed { self.bootstrap_cache.invalidate(); let _ = self.events.send(()); }
        Ok(result)
    }
    /// Immutable validation keeps the original writer serialization and fixed
    /// runtime identity; it neither refreshes an admission token nor writes jobs.
    async fn check_media_validation<T>(&self,scope:storage::MediaValidationScope<'_>,
        f:impl FnOnce(&Value)->ApiResult<T>)->ApiResult<T> {
        let stage=match scope.mode {
            storage::MediaValidationMode::Execution=>"media.validation.ocr.total",
            storage::MediaValidationMode::Reuse{..}=>"media.validation.reuse.total",
        };
        let _total=performance::Span::job(stage,scope.job);
        let waiting=performance::Span::job("media.validation.writer.wait",scope.job);
        let _guard=self.gate.acquire(writer_gate::Class::Standard).await;drop(waiting);
        let _held=performance::Span::job("media.validation.writer.held",scope.job);
        self.db.check_media_validation(scope,|view| {
            runtime_lifecycle::current_owner(view,&self.lifecycle_owner)?;
            let result=f(view)?;
            runtime_lifecycle::current_owner(view,&self.lifecycle_owner)?;
            Ok(result)
        }).await
    }
    async fn change<T>(&self, f: impl FnOnce(&mut Value) -> ApiResult<T>) -> ApiResult<T> {
        let lifecycle_capture = runtime_lifecycle_app::Capture::read(self).await;
        let _total = performance::Span::new("workspace.change.total");
        let waiting = performance::Span::new("workspace.writer.wait");
        let _guard = self.gate.acquire(writer_gate::Class::Standard).await;
        drop(waiting);
        let _held = performance::Span::new("workspace.writer.held");
        let (result, changed) = self.db.change_observed(|d| lifecycle_capture.with(d, |d| f(d))).await?;
        if changed { self.bootstrap_cache.invalidate();
            let _ = self.events.send(());
        }
        Ok(result)
    }
    async fn change_connection_gate<T>(&self,scope:connection_gate::Scope<'_>,f:impl FnOnce(&mut Value)->ApiResult<T>)->ApiResult<T> {
        let lifecycle_capture=runtime_lifecycle_app::Capture::read(self).await;
        let waiting=performance::Span::new("connection.gate.writer.wait");
        let _guard=self.gate.acquire(writer_gate::Class::Standard).await;drop(waiting);
        let _held=performance::Span::new("connection.gate.writer.held");
        let (result,changed)=self.db.change_connection_gate_observed(scope,|d|lifecycle_capture.with_jobless_scope(d,f)).await?;
        if changed{self.bootstrap_cache.invalidate();let _=self.events.send(());}
        Ok(result)
    }
    async fn commit_retained_paid_recovery(&self, actor: &operator_auth::Actor,
        installed: &retained_paid_recovery::InstalledCapture, body: &Value) -> ApiResult<Value> {
        let waiting = performance::Span::new("retained.recovery.writer.wait");
        let _guard = self.gate.acquire(writer_gate::Class::Standard).await;
        drop(waiting);
        let (result, changed) = self.db.commit_retained_paid_recovery_observed(actor, installed, body, &self.lifecycle_owner).await?;
        if changed { self.bootstrap_cache.invalidate(); let _ = self.events.send(()); }
        Ok(result)
    }
    async fn create_operator_batch(&self, body:&Value, actor:&operator_auth::Actor) -> ApiResult<Value> {
        let _total=performance::Span::new("operator.proposal_batch.total");
        let waiting=performance::Span::new("operator.proposal_batch.writer.wait");
        let _guard=self.gate.acquire(writer_gate::Class::Standard).await;
        drop(waiting);
        let _held=performance::Span::new("operator.proposal_batch.writer.held");
        let (result,changed)=self.db.create_operator_batch_observed(body,actor, &self.lifecycle_owner).await?;
        if changed {self.bootstrap_cache.invalidate();let _=self.events.send(());}
        Ok(result)
    }
    async fn change_media<T>(&self, f: impl FnOnce(&mut Value) -> ApiResult<T>) -> ApiResult<T> {
        let lifecycle_capture = runtime_lifecycle_app::Capture::read(self).await;
        let _total = performance::Span::new("media.change.total");
        let waiting = performance::Span::new("media.writer.wait");
        let _guard = self.gate.acquire(writer_gate::Class::Standard).await;
        drop(waiting);
        let _held = performance::Span::new("media.writer.held");
        let (result, changed) = self.db.change_media_observed(|d| lifecycle_capture.with(d, |d| f(d))).await?;
        if changed { self.bootstrap_cache.invalidate(); let _ = self.events.send(()); }
        Ok(result)
    }
    async fn change_preparation_claim<T>(&self, f: impl FnOnce(&mut Value) -> ApiResult<T>) -> ApiResult<T> {
        let lifecycle_capture = runtime_lifecycle_app::Capture::read(self).await;
        let _total = performance::Span::new("preparation.claim.total");
        let waiting = performance::Span::new("preparation.writer.wait");
        let _guard = self.gate.acquire(writer_gate::Class::Standard).await;
        drop(waiting);
        let (result, changed) = self.db.change_preparation_claim_observed(|d| lifecycle_capture.with(d, |d| f(d))).await?;
        if changed { self.bootstrap_cache.invalidate(); let _ = self.events.send(()); }
        Ok(result)
    }
    async fn change_preparation_schedule<T>(&self, f:impl FnOnce(&mut Value)->ApiResult<T>)->ApiResult<T> {
        let lifecycle_capture = runtime_lifecycle_app::Capture::read(self).await;
        let waiting=performance::Span::new("preparation.schedule.writer.wait");
        let _guard=self.gate.acquire(writer_gate::Class::Standard).await;
        drop(waiting);
        let (result,changed)=self.db.change_preparation_schedule_observed(|d| {
            let first = list(d, "jobs").len();
            let result = lifecycle_capture.with(d, |d| f(d))?;
            conductor_authority::fence_new_jobs(d, first)?;
            Ok(result)
        }).await?;
        if changed {self.bootstrap_cache.invalidate();let _=self.events.send(());}
        Ok(result)
    }
    async fn change_preparation_first<T>(&self, job_id: &str, f: impl FnOnce(&mut Value) -> ApiResult<T>) -> ApiResult<T> {
        let lifecycle_capture = runtime_lifecycle_app::Capture::read(self).await;
        let waiting = performance::Span::new("preparation.first.writer.wait");
        // Preserve completed model evidence with the shared bounded priority.
        let _guard = self.gate.acquire(writer_gate::Class::Interactive).await;
        drop(waiting);
        let (result, changed) = self.db.change_preparation_first_observed(job_id, |d| lifecycle_capture.with(d, |d| f(d))).await?;
        if changed { self.bootstrap_cache.invalidate(); let _ = self.events.send(()); }
        Ok(result)
    }
    async fn change_preparation_review_checkpoint<T>(&self, job_id:&str, f:impl FnOnce(&mut Value)->ApiResult<T>)->ApiResult<T> {
        let lifecycle_capture = runtime_lifecycle_app::Capture::read(self).await;
        let waiting=performance::Span::new("preparation.review_checkpoint.writer.wait");
        // Durable review progress shares the same burst limit as send receipts.
        let _guard=self.gate.acquire(writer_gate::Class::Interactive).await;
        drop(waiting);
        let (result,changed)=self.db.change_preparation_review_checkpoint_observed(job_id,|d| lifecycle_capture.with(d, |d| f(d))).await?;
        if changed {self.bootstrap_cache.invalidate();let _=self.events.send(());}
        Ok(result)
    }
    async fn create_proposal(&self, body: &Value) -> ApiResult<Value> {
        let _total = performance::Span::new("proposal.total");
        let waiting = performance::Span::new("proposal.writer.wait");
        let _guard = self.gate.acquire(writer_gate::Class::Standard).await;
        drop(waiting);
        let _occupancy = performance::Span::new("proposal.writer.held");
        let (result, changed) = self.db.create_proposal_observed(body, &self.lifecycle_owner).await?;
        if changed { self.bootstrap_cache.invalidate(); let _ = self.events.send(()); }
        Ok(result)
    }
    async fn edit_proposal(&self, key: &str, body: &Value) -> ApiResult<Value> {
        let _total = performance::Span::new("proposal.edit.total");
        let waiting = performance::Span::new("proposal.edit.writer.wait");
        let _guard = self.gate.acquire(writer_gate::Class::Interactive).await;
        drop(waiting);
        let (result, changed) = self.db.edit_proposal_observed(key, body, &self.lifecycle_owner).await?;
        if changed { self.bootstrap_cache.invalidate(); let _ = self.events.send(()); }
        Ok(result)
    }
    async fn change_admission<T>(&self, scope: storage::AdmissionScope<'_>, f: impl FnOnce(&mut Value) -> ApiResult<T>) -> ApiResult<T> {
        let lifecycle_capture = runtime_lifecycle_app::Capture::read(self).await;
        let social_token = if matches!(&scope, storage::AdmissionScope::Execute { .. }) {
            Some(self.lifecycle_admission_token(runtime_lifecycle::AdmissionClass::SocialDispatch).await?)
        } else { None };
        let _total = performance::Span::new("admission.change.total");
        let waiting = performance::Span::new("admission.writer.wait");
        let _guard = self.gate.acquire(writer_gate::Class::Standard).await;
        drop(waiting);
        let _held = performance::Span::new("admission.writer.held");
        let (result, changed) = self.db.change_admission_observed(scope, |d| {
            if let Some(token) = &social_token { runtime_lifecycle::require_admission(d, token, runtime_lifecycle::AdmissionClass::SocialDispatch)?; }
            let first = list(d, "jobs").len();
            let result = lifecycle_capture.with(d, |d| f(d))?;
            conductor_authority::fence_new_jobs(d, first)?;
            Ok(result)
        }).await?;
        if changed { self.bootstrap_cache.invalidate(); let _ = self.events.send(()); }
        Ok(result)
    }
    async fn change_source_snapshot<T>(&self, f: impl FnOnce(&mut Value) -> ApiResult<T>) -> ApiResult<T> {
        let lifecycle_capture = runtime_lifecycle_app::Capture::read(self).await;
        let _total=performance::Span::new("source.snapshot.total");
        let waiting=performance::Span::new("source.snapshot.writer.wait");
        let _guard=self.gate.acquire(writer_gate::Class::SourceSnapshot).await;
        drop(waiting);
        let _held = performance::Span::new("source.snapshot.writer.held");
        let (result,changed)=self.db.change_source_snapshot_observed(|d| lifecycle_capture.with(d, |d| f(d))).await?;
        if changed {
            self.bootstrap_cache.invalidate();let _=self.events.send(());
            media_queue::notify_after_sync_commit(self);
        }
        Ok(result)
    }
    async fn change_source_snapshot_scoped<T>(&self,intent:storage::SourceReadIntent<'_>,
        f:impl FnOnce(&mut Value)->ApiResult<T>)->ApiResult<T>{
        let lifecycle_capture=runtime_lifecycle_app::Capture::read(self).await;
        let _total=performance::Span::new("source.snapshot.total");
        let waiting=performance::Span::new("source.snapshot.writer.wait");
        let _guard=self.gate.acquire(writer_gate::Class::SourceSnapshot).await;
        drop(waiting);
        let _held=performance::Span::new("source.snapshot.writer.held");
        let completion=self.db.change_source_snapshot_scoped_completed(intent,
            |d|lifecycle_capture.with(d,|d|f(d))).await;
        if matches!(&completion.outcome,Ok((_,true))){
            self.bootstrap_cache.invalidate();let _=self.events.send(());
            media_queue::notify_after_sync_commit(self);
        }
        // PostgreSQL settlement and same-connection pool return finish inside
        // the completion API. Large retained projections may be destroyed only
        // after releasing this process-local writer, including error outcomes.
        drop(_held);
        drop(_guard);
        completion.cleanup.dispose().await;
        completion.outcome.map(|(result,_)|result)
    }
    async fn change_preparation_admission<T>(&self, job_id: &str, f: impl FnOnce(&mut Value) -> ApiResult<T>) -> ApiResult<T> {
        let lifecycle_capture = runtime_lifecycle_app::Capture::read(self).await;
        let _total = performance::Span::job("preparation.admission.total", job_id);
        let waiting = performance::Span::job("preparation.admission.writer.wait", job_id);
        let _guard = self.gate.acquire(writer_gate::Class::Standard).await;
        drop(waiting);
        let _occupancy = performance::Span::job("preparation.admission.writer.held", job_id);
        let (result, changed) = self.db.change_preparation_admission_observed(job_id, |d| lifecycle_capture.with(d, |d| f(d))).await?;
        if changed { self.bootstrap_cache.invalidate(); let _ = self.events.send(()); }
        Ok(result)
    }
    async fn change_item<T>(&self, key: &str, f: impl FnOnce(&mut Value) -> ApiResult<T>) -> ApiResult<T> {
        let lifecycle_capture = runtime_lifecycle_app::Capture::read(self).await;
        let _guard = self.gate.acquire(writer_gate::Class::Standard).await;
        let (result, changed) = self.db.change_item_observed(key, |d| lifecycle_capture.with_jobless_scope(d, |d| f(d))).await?;
        if changed { self.bootstrap_cache.invalidate(); let _ = self.events.send(()); }
        Ok(result)
    }
    async fn change_job<T>(&self, key: &str, f: impl FnOnce(&mut Value) -> ApiResult<T>) -> ApiResult<T> {
        let lifecycle_capture = runtime_lifecycle_app::Capture::read(self).await;
        let _guard = self.gate.acquire(writer_gate::Class::Standard).await;
        let (result, changed) = self.db.change_job_observed(key, |d| lifecycle_capture.with(d, |d| f(d))).await?;
        if changed { self.bootstrap_cache.invalidate(); let _ = self.events.send(()); }
        Ok(result)
    }
    async fn change_operation_evidence(&self, op: &Value, update: storage::OperationEvidenceUpdate) -> ApiResult<()> {
        let waiting = performance::Span::new("operation.evidence.writer.wait");
        // Durable operation evidence uses the bounded settlement queue.
        let _guard = self.gate.acquire(writer_gate::Class::Settlement).await;
        drop(waiting);
        let _held = performance::Span::new("operation.evidence.writer.held");
        let ((), changed) = self.db.change_operation_evidence_observed(op, update, &self.lifecycle_owner).await?;
        if changed { self.bootstrap_cache.invalidate(); let _ = self.events.send(()); }
        Ok(())
    }
    async fn change_operation_outcome<T>(&self, op: &Value, status: &str, f: impl FnOnce(&mut Value) -> ApiResult<T>) -> ApiResult<T> {
        let lifecycle_capture = runtime_lifecycle_app::Capture::read(self).await;
        let waiting = performance::Span::new("operation.outcome.writer.wait");
        // Settle external uncertainty/results; the gate bounds deferral of other writers.
        let _guard = self.gate.acquire(writer_gate::Class::Settlement).await;
        drop(waiting);
        let _held = performance::Span::new("operation.outcome.writer.held");
        let (result, changed) = self.db.change_operation_outcome_observed(op, status, |d| lifecycle_capture.with(d, |d| f(d))).await?;
        if changed { self.bootstrap_cache.invalidate(); let _ = self.events.send(()); }
        Ok(result)
    }
    // One queue admission, two durable commits: an outcome rejection must never
    // roll back the provider receipt already retained by the first transaction.
    async fn change_execute_transition<T>(&self, op: &Value, receipt: Value, status: &str,
        f: impl FnOnce(&mut Value) -> ApiResult<T>) -> ApiResult<T> {
        let lifecycle_capture = runtime_lifecycle_app::Capture::read(self).await;
        let _total = performance::Span::new("operation.execute_transition.total");
        if !matches!(status, "unknown" | "failed") {
            return Err(internal("Invalid post-execute transition"));
        }
        let waiting = performance::Span::new("operation.execute_transition.writer.wait");
        let _guard = self.gate.acquire(writer_gate::Class::Settlement).await;
        drop(waiting);
        let _held = performance::Span::new("operation.execute_transition.writer.held");
        let ((), changed) = self.db.change_operation_evidence_observed(
            op, storage::OperationEvidenceUpdate::ExecuteReceipt(receipt), &self.lifecycle_owner).await?;
        // Publish receipt durability before even starting the outcome unit.
        if changed { self.bootstrap_cache.invalidate(); let _ = self.events.send(()); }
        let (result, changed) = self.db.change_operation_outcome_observed(op, status, |d| lifecycle_capture.with(d, |d| f(d))).await?;
        if changed { self.bootstrap_cache.invalidate(); let _ = self.events.send(()); }
        Ok(result)
    }
    async fn change_schedule<T>(&self, f: impl FnOnce(&mut Value) -> ApiResult<T>) -> ApiResult<T> {
        let lifecycle_capture = runtime_lifecycle_app::Capture::read(self).await;
        let _guard = self.gate.acquire(writer_gate::Class::Standard).await;
        let (result, changed) = self.db.change_schedule_observed(|d| lifecycle_capture.with(d, |d| f(d))).await?;
        if changed { self.bootstrap_cache.invalidate(); let _ = self.events.send(()); }
        Ok(result)
    }
    async fn change_status<T>(&self, routes: &[Value], f: impl FnOnce(&mut Value) -> ApiResult<T>) -> ApiResult<T> {
        let lifecycle_capture = runtime_lifecycle_app::Capture::read(self).await;
        let _guard = self.gate.acquire(writer_gate::Class::Standard).await;
        let (result, changed) = self.db.change_status_observed(routes, |d| lifecycle_capture.with(d, |d| f(d))).await?;
        if changed { self.bootstrap_cache.invalidate(); let _ = self.events.send(()); }
        Ok(result)
    }
    async fn change_source_claim<T>(&self, f: impl FnOnce(&mut Value) -> ApiResult<T>) -> ApiResult<T> {
        let token = self.lifecycle_admission_token(runtime_lifecycle::AdmissionClass::SourceRead).await?;
        let lifecycle_capture = runtime_lifecycle_app::Capture::read(self).await;
        let _guard = self.gate.acquire(writer_gate::Class::Standard).await;
        let (result, changed) = self.db.change_source_claim_observed(|d| { runtime_lifecycle::require_admission(d, &token, runtime_lifecycle::AdmissionClass::SourceRead)?; lifecycle_capture.with(d, |d| f(d)) }).await?;
        if changed { self.bootstrap_cache.invalidate(); let _ = self.events.send(()); }
        Ok(result)
    }
    async fn bridge(&self, operation: &str, args: Value) -> ApiResult<Value> {
        self.bridge_observed(operation,args,None).await
    }
    async fn bridge_admitted(&self, operation:&str, args:Value, native:runtime_owned_work::Work)->ApiResult<Value> {
        if !matches!(operation,"assistant"|"assistant_research"|"media"|"media_vision"|"media_vision_chunk") { return Err(bad("Invalid admitted paid bridge operation")); }
        self.bridge_observed_inner(operation,args,None,Some(native)).await
    }
    async fn bridge_admitted_observed(&self, operation:&str, args:Value, native:runtime_owned_work::Work, resource:Option<&mut media_processing::full::gpu_outcome::Outcome>) -> ApiResult<Value> {
        if !matches!(operation,"media_vision"|"media_vision_chunk") { return Err(bad("Invalid admitted vision bridge operation")); }
        self.bridge_observed_inner(operation,args,resource,Some(native)).await
    }
    async fn bridge_observed(&self, operation: &str, args: Value, resource:Option<&mut media_processing::full::gpu_outcome::Outcome>) -> ApiResult<Value> {
        self.bridge_observed_inner(operation,args,resource,None).await
    }
    // Keep the shared transport state machine off every caller's async frame.
    // Boxing happens before polling; guards, native ownership and cancellation
    // still belong to the same future and are dropped by the original caller.
    fn bridge_observed_inner<'a>(&'a self, operation: &'a str, args: Value, mut resource:Option<&'a mut media_processing::full::gpu_outcome::Outcome>, native:Option<runtime_owned_work::Work>) -> std::pin::Pin<Box<impl std::future::Future<Output=ApiResult<Value>> + 'a>> {
        Box::pin(async move {
        if matches!(operation, "assistant" | "assistant_research" | "assistant_preflight") {
            if let Some(ctx) = conductor_authority::current_context() {
                conductor_authority::check_read(self, &ctx).await?;
            }
        }
        if let Some(resource)=resource.as_deref_mut(){*resource=Default::default();}
        let bridge_stage = match operation {
            "context" => "dispatch.bridge.context",
            "execute" => "dispatch.bridge.execute",
            "readback" => "dispatch.bridge.readback",
            _ => "adapter.bridge.other",
        };
        let mut _bridge_timing=match performance::current_trace_context(){
            Some(context)=>performance::Span::start(bridge_stage,performance::SpanClass::Container,&context),
            None=>performance::Span::new(bridge_stage),
        };
        let bridge_trace=_bridge_timing.context().or_else(performance::current_trace_context);
        if operation == "execute" {
            self.check_execution()?;
            dispatch_transport::require(&args)?;
        }
        let mut request = args;
        self.account.bind_request(&mut request)?;
        request["operation"] = json!(operation);
        let budget_job=runtime_lifecycle_app::current_job();
        let invocation_budget=if matches!(operation,"assistant"|"assistant_research") {
            if let Some(job)=budget_job.as_deref() {
                // The finite budget uses the complete original/descendant
                // ledger. A filtered active-job projection cannot count spend.
                self.change(|d|continuous_preparation::reserve_bridge(d,job,operation,&request,&now())).await?
            }else{None}
        }else{None};
        // Only the admitted provider bridge uses the shared session. Synthetic
        // test transports, fail-closed staging and model/media bridges keep
        // their explicit transport. A session error never falls back/replays.
        if provider_session::supports(operation) && self.bridge.file_name().is_some_and(|v|v=="bridge.mjs") {
            let worker=self.bridge.parent().ok_or_else(||internal("Provider worker path unavailable"))?.join("provider-session.mjs");
            if native.is_some() { return Err(bad("Paid native ticket cannot enter a pooled provider request")); }
            let mut work = self.lifecycle_work.begin(runtime_owned_work::Kind::DirectBridge)?;
            let request=self.provider_session.request(&self.node,&worker,self.account.key(),request);
            let observed=provider_session::observe_auth_failure(request);
            let (result,auth_failed)=match bridge_trace{Some(context)=>trace_context::scope(context,observed).await,None=>observed.await};
            work.settled();
            if auth_failed{connection_recovery::observe_provider_auth_failure(self);}
            _bridge_timing.finish(if result.is_ok(){"completed"}else{"unresolved"},None);
            return result;
        }
        let mut command = Command::new(&self.node);
        command.env_remove("COMMUNITYHERO_INVOCATION_BUDGET");
        if let Some(budget)=&invocation_budget {
            command.env("COMMUNITYHERO_INVOCATION_BUDGET",budget.to_string());
        }
        command.env_remove("COMMUNITYHERO_TRACE_CONTEXT");
        if let Some(context)=&bridge_trace{command.env("COMMUNITYHERO_TRACE_CONTEXT",context.to_json().to_string());}
        // Worker identity is a held in-process lease, never an input/model field.
        // Clear inherited selectors so a child cannot accidentally reuse a slot.
        command.env_remove("COMMUNITYHERO_PREPARE_WORKER_SLOT");
        if operation == "assistant" {
            if let Some(slot) = preparation_workers::current_slot() {
                command.env("COMMUNITYHERO_PREPARE_WORKER_SLOT", slot.to_string())
                    .env("COMMUNITYHERO_PREPARE_WORKERS", self.preparation_workers.width().to_string());
            }
        }
        command
            .arg(&self.bridge)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true);
        #[cfg(windows)]
        command.creation_flags(0x08000000);
        let kind = if matches!(operation, "media" | "media_vision" | "media_vision_chunk") { runtime_owned_work::Kind::MediaTool } else { runtime_owned_work::Kind::DirectBridge };
        let mut child = match native {
            Some(work) => runtime_native_child::OwnedChild::spawn_admitted(work, &mut command).await?,
            None => runtime_native_child::OwnedChild::spawn(&self.lifecycle_work, kind, &mut command).await?,
        };
        let mut input = child.child_mut()
            .stdin
            .take()
            .ok_or_else(|| internal("Adapter input unavailable"))?;
        input
            .write_all(request.to_string().as_bytes())
            .await
            .map_err(|_| internal("Adapter request failed"))?;
        drop(input);
        let mut stdout = child.child_mut()
            .stdout
            .take()
            .ok_or_else(|| internal("Adapter output unavailable"))?;
        let timeout = if matches!(operation,"media_vision"|"media_vision_chunk") {
            3600
        } else if operation == "media" {
            4520
        } else if matches!(operation,"assistant"|"assistant_research") {
            // Outlive the adapter's 45-minute absolute model-stage budget,
            // leaving bounded time for setup, admission and process cleanup.
            3000
        } else {
            240
        };
        let output = tokio::time::timeout(Duration::from_secs(timeout), async {
            let mut buf = Vec::new();
            (&mut stdout)
                .take(32 * 1024 * 1024 + 1)
                .read_to_end(&mut buf)
                .await.map_err(|error| format!("Adapter process failed (stage=stdout; kind={:?}; os={:?})",error.kind(),error.raw_os_error()))?;
            if buf.len() > 32 * 1024 * 1024 {
                return Err("Adapter process failed (stage=stdout; category=output_limit)".to_owned());
            }
            let status = child.child_mut().wait().await.map_err(|error| format!("Adapter process failed (stage=wait; kind={:?}; os={:?})",error.kind(),error.raw_os_error()))?;
            if !status.success() {
                return Err(format!("Adapter process failed (stage=exit; code={:?}; codeHex={})",status.code(),status.code().map(|code|format!("0x{:08X}",code as u32)).unwrap_or_else(||"none".to_owned())));
            }
            Ok(buf)
        })
        .await
        .map_err(|_| internal("Adapter timed out; action outcome may be unknown"))?
        .map_err(|reason| internal(&reason))?;
        // A successful bridge response is not proof that its local GPU children
        // have exited. Settle containment before the caller releases admission.
        // Execute has no paid output to retain. Its error or malformed envelope
        // must still carry positively observed native transport cessation.
        let child=if operation=="execute" {
            let (_,cessation)=child.settle_observed().await?;
            provider_session::observe_contained_bridge(cessation,&editorial_review::hash_text(&String::from_utf8_lossy(&output)));
            None
        }else{Some(child)};

        let envelope: Value =
            serde_json::from_slice(&output).map_err(|_| internal("Invalid adapter response"))?;
        if let Some(context)=&bridge_trace{
            if envelope.get("telemetry").is_some(){trace_context::accept_telemetry(&envelope["telemetry"],context);}
            else{trace_context::missing(context,"trace.marker","missing_js_completion");}
        }
        if envelope["ok"] != true {
            if provider_session::supports(operation)&&provider_session::is_auth_failure(&envelope["error"]){
                connection_recovery::observe_provider_auth_failure(self);
            }
            if let Some(resource)=resource {
                resource.observe(operation,&request,&envelope["error"],cfg!(windows));
            }
            return Err(internal(&dispatch_evidence::adapter_failure(&envelope["error"])));
        }
        // Retain the exact adapter output before deriving any native receipt.
        // Receipt failure must leave that paid result available for recovery.
        let mut result = envelope["result"].clone();
        if let Some(reference)=runtime_paid_result::retain(self, operation, &request, &result).await? {
            if let Some(job)=reference["binding"]["nativeJobId"].as_str() {
                self.change_job(job, |d|runtime_paid_result::attach(d,job,&reference,&self.lifecycle_owner)).await?;
                if let Some(pointer)=model_material_receipt::retain(self,&request,&result,&reference).await? {
                    self.change_job(job, |d|model_material_receipt::attach(d,job,&pointer)).await?;
                    result["modelMaterialReceipt"] = pointer;
                }
            }
        }
        if let (Some(job),Some(budget))=(budget_job.as_deref(),invocation_budget.as_ref()) {
            // Retention above precedes observation validation. Missing proof
            // cannot discard a paid output or refund an issued invocation.
            self.change(|d|continuous_preparation::settle_bridge(d,job,budget,&result,&now())).await?;
        }
        if let Some(child)=child {child.settle().await?;}
        _bridge_timing.finish("completed",None);
        Ok(result)
        })
    }
    async fn finish(&self, job: &str, result: ApiResult<Value>) {
        let mut delay = Duration::from_secs(1);
        let (wake_preparation,wake_media) = loop {
        // Retry only the durable completion record, never the worker/provider call.
        // Keep the task registered until completion is committed or server exit.
        let committed = self
            .change_job(job, |d| {
                let j = row_mut(d, "jobs", job)?;
                let refresh_review=matches!(j["kind"].as_str(),Some("execute"|"reconcile"));
                let wake_preparation=j["kind"]=="media" || (j["kind"]=="assistant" && j["purpose"]!="discussion");
                let wake_media=j["kind"]=="sync" && j["purpose"]!="archive_import" && result.is_ok();
                if media_fullframes::finish(j,&result,&now()){return Ok((false,wake_preparation,false));}
                if j["status"] != "running" && j["status"] != "queued" {
                    return Ok((refresh_review,wake_preparation,wake_media));
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
                        let retained_vision=j["result"]["visualProgress"].clone();
                        j["result"] = v.clone();
                        if j["kind"]=="media" && !retained_vision["visionStage"].is_null(){
                            j["result"]["visualProgress"]=retained_vision;
                        }
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
                Ok((refresh_review,wake_preparation,wake_media))
            })
            .await;
        if let Ok((refresh_review,wake_preparation,wake_media))=committed {
            // The focused job transaction has committed and released its gate.
            // Only action/reconciliation completions need the private chat receipt.
            if !refresh_review || self.change_admission(storage::AdmissionScope::ExecutionReceipts(job),|d| assistant_action_review::refresh_execution_receipts(d,job)).await.is_ok(){break (wake_preparation,wake_media);}
        }
        eprintln!("job_finalization_pending; retrying durable completion");
        tokio::time::sleep(delay).await;
        delay = (delay * 2).min(Duration::from_secs(30));
        };
        self.tasks.lock().await.remove(job);
        // One coalesced permit per application; only committed completion may
        // wake the next preparation. Runtime enablement is enforced by the loop.
        if wake_preparation {self.preparation_wake.notify_one();}
        if wake_media {media_queue::notify_after_sync_commit(self);}
    }
    async fn job(&self, kind: &str, ref_id: &str) -> ApiResult<String> {
        let token = self.lifecycle_admission_token(runtime_lifecycle_app::job_class(kind)).await?;
        if conductor_authority::current_context().is_some() {
            return self.change_schedule(|d| {
                runtime_lifecycle::require_admission(d, &token, runtime_lifecycle_app::job_class(kind))?;
                let context = conductor_authority::fence_admission(d, "read", &[])?
                    .ok_or_else(|| internal("Conductor job authority missing"))?;
                let key = new_job(d, kind, ref_id)?;
                conductor_authority::tag(&context, row_mut(d, "jobs", &key)?);
                Ok(key)
            }).await;
        }
        self.change_schedule(|d| { runtime_lifecycle::require_admission(d, &token, runtime_lifecycle_app::job_class(kind))?; new_job(d, kind, ref_id) }).await
    }
    fn spawn(
        &self,
        job: String,
        f: impl std::future::Future<Output = ApiResult<Value>> + Send + 'static,
    ) {
        self.spawn_with_completion(job, f, || {});
    }
    fn spawn_with_completion(
        &self,
        job: String,
        f: impl std::future::Future<Output = ApiResult<Value>> + Send + 'static,
        completed: impl FnOnce() + Send + 'static,
    ) {
        let lifecycle_task = runtime_lifecycle_app::TaskCount::begin(self.lifecycle_task_count.clone());
        // Task-local scopes embed their input in several nested async states.
        // Keep one concrete heap owner before either spawned wrapper is built.
        let f = Box::pin(f);
        let app = self.clone();
        let conductor_context = conductor_authority::current_context();
        let inherited_trace=trace_context::current();
        tokio::spawn(async move {
            let trace=app.trace_for_job(&job,inherited_trace).await;
            let work=async {
            let _lifecycle_task = lifecycle_task;
            let mut handles = app.tasks.lock().await;
            if handles.contains_key(&job) { return; }
            let worker = app.clone();
            let key = job.clone();
            let worker_trace=trace_context::current();
            let task = tokio::spawn(async move {
                let work=async {
                let context = match conductor_context {
                    Some(ctx) => Some(ctx),
                    None => match conductor_authority::context_for_job(&worker, &key).await {
                        Ok(context) => context,
                        Err(error) => return worker_supervision::WorkerExit::Completed(Err(error)),
                    },
                };
                let f = runtime_owned_work::with_registry(worker.lifecycle_work.clone(), runtime_lifecycle_app::with_job(key.clone(), f));
                match context {
                    Some(ctx) => conductor_authority::with_context(ctx, worker_supervision::run_if_active(&worker, &key, f)).await,
                    None => worker_supervision::run_if_active(&worker, &key, f).await,
                }
                };
                match worker_trace{Some(context)=>trace_context::scope(context,work).await,None=>work.await}
            });
            handles.insert(job.clone(), task.abort_handle());
            drop(handles);
            let exit = worker_supervision::observe(task).await;
            worker_supervision::finalize(&app, &job, exit).await;
            // Durable completion and task removal precede scheduler wakeups.
            completed();
            };
            match trace{
                Some(context)=>{
                    let mut span=performance::Span::start("trace.root",performance::SpanClass::Container,&context);
                    trace_context::scope(context,span.scope(work)).await;
                    span.finish("completed",None);
                },
                None=>work.await,
            }
        });
    }
}
#[cfg(test)]
#[path = "bridge_future_stack_tests.rs"]
mod bridge_future_stack_tests;
fn new_job(d: &mut Value, kind: &str, ref_id: &str) -> ApiResult<String> {
    runtime_lifecycle_app::require_new_job(d, kind)?;
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
    conductor_authority::sanitize_job(job);
    if let Some(bundle)=job["prepareBundle"].as_object_mut(){bundle.remove("request");}
    if let Some(fields)=job.as_object_mut(){
        for field in ["editorialPlan","editorialBatches","continuousPreparationOrigin","codexInvocationReservations","codexInvocationIssuedSlots","continuousParentJobId"]{fields.remove(field);}
    }
    if let Some(progress)=job.get_mut("result").and_then(|result|result.get_mut("visualProgress")).and_then(Value::as_object_mut){
        for key in ["sourceProjection","leaseId","materialEpoch"]{progress.remove(key);}
    }
    fn remove_private_observations(value:&mut Value) {
        match value {
            Value::Object(fields)=>{fields.remove("invocationBudget");for child in fields.values_mut(){remove_private_observations(child);}},
            Value::Array(rows)=>for child in rows{remove_private_observations(child);},
            _=>{},
        }
    }
    remove_private_observations(job);
}
fn bootstrap_view(mut d: Value, csrf: &str) -> Value {
    dispatch_authority::sanitize_view(&mut d);
    // The archived carry contains the same authority journals as live jobs.
    // Keep it durable for accounting/recovery, never expose it as UI metadata.
    if let Some(archive)=d["cleanStartArchive"].as_object_mut(){archive.remove("invocationBudgetCarry");}
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
    source_snapshot_scoped::merge_snapshot(d,snapshot)
}

async fn sync_page(app: &App, mode: &str, cursor: Option<&str>) -> ApiResult<()> {
    let binding = active_binding(&app.db.read_engine_status().await?)?;
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
    app.change_source_snapshot_scoped(storage::SourceReadIntent::Snapshot(&snapshot),|d| {
        if active_binding(d)? != binding {
            return Err(conflict("Connector changed during sync"));
        }
        d["connectorBinding"] = binding.to_json();
        merge_sync_page(d, &snapshot, mode, cursor.is_some())
    })
    .await?;
    // change_source_snapshot already emitted the coalesced media hint after commit.
    Ok(())
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
        assistant_dialogue::attach_proposal_policy(d,&mut bundle)?;
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
fn create_proposal_impl(d: &mut Value, body: &Value, generated: bool) -> ApiResult<Value> {
    create_proposal_impl_with_recovery(d, body, generated, None)
}
pub(crate) fn create_retained_recovery_proposal(d: &mut Value, body: &Value, permit: &retained_paid_recovery::RecoveryPermit) -> ApiResult<Value> {
    if body.get(retained_paid_recovery::FIELD).is_some() { return Err(bad("Recovery proof is server-owned")); }
    create_proposal_impl_with_recovery(d, body, false, Some(permit))
}
fn create_proposal_impl_with_recovery(d: &mut Value, body: &Value, generated:bool, recovery: Option<&retained_paid_recovery::RecoveryPermit>) -> ApiResult<Value> {
    if body.get(retained_paid_recovery::FIELD).is_some() { return Err(bad("Recovery proof is server-owned")); }
    if body.get("nativeCreationOrigin").is_some() { return Err(bad("Proposal creation origin is server-owned")); }
    if let Some(permit) = recovery { permit.validate_creation(d, body)?; }
    let owner_close = recovery.is_none() && operator_close::requested(body, generated);
    if owner_close && body.get("decisionMediaContract").is_some() && !decision_media::enabled(body) {
        return Err(bad("Unsupported owner close decision media contract"));
    }
    if body.get("closePreserveUnknownReplies").is_some() && !owner_close {
        return Err(bad("Preserving UNKNOWN replies requires an authenticated owner empty CLOSE"));
    }
    let target = required(body, "itemId")?;
    if !generated { conductor_authority::fence_admission(d, "proposal", &[json!(target)])?; }
    let mut item = row(d, "items", target)?.clone();
    if recovery.is_none() { if let Some(e)=feedback::retry(d,body,target)? {return Ok(row(d,"proposals",required(&e,"proposalId")?)?.clone());} }
    let origin=if generated || recovery.is_some() {Value::Null}else{feedback::origin(d,&item,body)?};
    let review_digest=prepare_bundle::review_fingerprint(d,target).map_err(conflict)?;
    if !owner_close && origin["sourceContextDigest"].is_string() && origin["sourceContextDigest"]!=review_digest {return Err(conflict("Source context changed; prepare a new suggestion"));}
    check_revision(&item, &body["expectedRevision"])?;
    let binding = active_binding(d)?;
    bridge_account(&binding)?;
    let route_target = bound_item(&binding, &item)?;
    if !generated && !owner_close && recovery.is_none() {
        preparation_reservations::assert_proposal(d,&json!({"itemId":item["id"],"origin":origin,"routeTarget":route_target}))?;
    }
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
    let close_preserved = if owner_close { operator_close::capture_preserved(d,body,&item)? } else { Value::Null };
    if owner_close {
        // Action-only authority does not settle, release or retry paid generation.
        // Validate a complete fresh decision before any workflow mutation.
        let mut selection = json!({"itemId":item["id"],"kind":"close","text":"","revision":1,
            "itemRevision":item["revision"],"routeTarget":route_target,"reviewContextDigest":review_digest});
        selection["operatorCloseDecision"] = operator_close::classify(d,body,&selection,close_preserved.clone());
        preparation_reservations::assert_operator_close(d,&selection)?;
    }
    // New video decisions may be staged for exact semantic review. This is
    // neither media readiness nor permission to approve/send the draft.
    let media_review=(!owner_close || decision_media::enabled(body)) && media_queue::requires_video(d,&item)
        && (!generated || decision_media::enabled(body));
    if !owner_close && !media_review && media_queue::preparation_state(d,&item,&now())?.is_some() {
        return Err(conflict("Video audio and visual evidence must be complete before preparing a decision"));
    }
    if item["workflow"] == "attention" {
        let current = row_mut(d, "items", target)?;
        current["workflow"] = json!("prepared");
        bump(current);
        item = current.clone();
    }
    let mut v = json!({"id":id(),"itemId":target,"kind":kind,"text":text,"routeTarget":route_target,"revision":1,"itemRevision":item["revision"],"contextEvidenceDigest":item["contextEvidenceDigest"],"branchContextDigest":item["branchContextDigest"],"status":"draft","sources":body["sources"].as_array().cloned().unwrap_or_default(),"createdAt":now()});
    // This identifies the native creation route, not an inferred absence of
    // provenance. Copied/model/recovered drafts cannot claim a manual origin.
    v["nativeCreationOrigin"]=json!(if recovery.is_some(){"retained_model_recovery_v1"}
        else if generated{"model_generation_v1"}else if !origin.is_null(){"model_derived_v1"}else{"operator_manual_v1"});
    if !origin.is_null() {
        if owner_close {
            // Historical paid provenance is retained as observation, never as
            // generation authority for this independent owner close decision.
            v["priorPreparationOrigin"] = origin.clone();
        } else {
            v["origin"]=origin.clone();v["sourceProposalId"]=origin["id"].clone();v["sourceProposalRevision"]=origin["revision"].clone();
        }
    }
    v["reviewContextDigest"]=json!(review_digest);
    if media_review {v["decisionMediaContract"]=json!(decision_media::CONTRACT);}
    if owner_close { v["operatorCloseDecision"] = operator_close::classify(d, body, &v, close_preserved); }
    if allow_closed_reply {v["allowClosedReply"]=json!(true);}
    v["draftSessionId"]=body.get("draftSessionId").cloned().unwrap_or_else(||item["draftSessionId"].clone());
    if let Some(ctx) = conductor_authority::current_context() { conductor_authority::tag(&ctx, &mut v); }
    list_mut(d, "proposals").push(v.clone());
    if !generated && (owner_close || body.get("eventId").is_some() || !origin.is_null()) {
        feedback::append(d,body,&item,&origin,"action_selected",json!({"action":kind,"text":text,"proposalId":v["id"],"proposalRevision":v["revision"],"operatorCloseDecision":v["operatorCloseDecision"]}))?;
    }
    Ok(v)
}
async fn proposal_new(State(app): State<App>, Json(body): Json<Value>) -> ApiResult<Json<Value>> {
    app.create_proposal(&body).await.map(Json)
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
    let job=list(d,"jobs").iter().rev().find(|j|j["kind"]=="assistant" && preparation_restart::job_targets(j,&json!(id)))
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
        // Equivalent text/context does not recreate missing mandatory material
        // evidence. Preserve the original paid pointer only after proving that
        // its complete material input is still attached and current.
        if p["kind"]=="reply_and_close" {
            if let Err(reason)=preparation_materials::require_original_generation(&prepare_bundle::EvidenceContext::new(d),&p) {
                results.push(json!({"proposalId":p["id"],"result":"materials_unproven","reason":reason}));continue;
            }
        }
        if !apply {results.push(json!({"proposalId":p["id"],"result":"equivalent"}));continue;}
        let v=create_proposal_impl(d,&json!({"itemId":target,"expectedRevision":item["revision"],"kind":p["kind"],"text":p["text"],"sources":p["sources"]}),true)?;
        let q=row_mut(d,"proposals",required(&v,"id")?)?;
        q["recovery"]=json!({"kind":"recovered_context_metadata","originProposalId":p["id"],"originProposalRevision":p["revision"],"prepareRunId":run,"prepareBundleId":p["prepareBundleId"],"prepareBundleDigest":p["prepareBundleDigest"],"sourceProposal":p,"recoveredAt":now()});
        q["generationMetadata"]=p["generationMetadata"].clone();
        q["knowledgeManifest"]=p["knowledgeManifest"].clone();
        for field in ["modelMaterialReceipt","mandatoryMaterialContract"] {
            if let Some(value)=p.get(field){q[field]=value.clone();}
        }
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
    Ok(Json(app.edit_proposal(&key, &body).await?))
}
fn edit_proposal(d: &mut Value, key: &str, body: &Value) -> ApiResult<Value> {
        let p = row(d, "proposals", key)?.clone();
        if feedback::retry(d,body,required(&p,"itemId")?)?.is_some(){return Ok(p);}
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
        let p = row_mut(d, "proposals", key)?;
        if !p["origin"].is_object(){p["origin"]=origin.clone();}
        if !p["history"].is_array(){p["history"]=json!([]);}
        let mut historical=before.clone();historical.as_object_mut().unwrap().remove("history");
        p["history"].as_array_mut().unwrap().push(historical);
        p["text"] = json!(text);
        p["status"] = json!("draft");
        bump(p);
        let after=p.clone();
        feedback::append(d,body,&item,&origin,"draft_saved",json!({"before":before["text"],"after":after["text"],"proposalId":key,"proposalRevision":after["revision"]}))?;
        Ok(after)
}
fn proposal_current(d: &Value, p: &Value) -> ApiResult<Value> {
    proposal_current_with_context(p, &prepare_bundle::EvidenceContext::new(d))
}
fn proposal_current_with_context(p: &Value, context: &prepare_bundle::EvidenceContext<'_>) -> ApiResult<Value> {
    proposal_current_checked(p,context).map_err(|failure|failure.error)
}
// Mark the check before evaluating it. The same first failure and ApiError are
// returned to ordinary callers; dispatch retains a closed, content-free reason.
fn proposal_current_checked(p: &Value, context: &prepare_bundle::EvidenceContext<'_>)
    -> Result<Value,dispatch_diagnostics::LocalPreconditionFailure> {
    proposal_current_checked_mode(p,context,true,None)
}
fn proposal_current_checked_mode(p: &Value, context: &prepare_bundle::EvidenceContext<'_>,allow_current_review:bool,own_operation:Option<&Value>)
    -> Result<Value,dispatch_diagnostics::LocalPreconditionFailure> {
    use dispatch_diagnostics::LocalPredicate;
    let mut predicate=LocalPredicate::RecoveredPreparation;
    let mut review_fingerprints=None;
    let result=(|| -> ApiResult<Value> {
    let d = context.workspace();
    retained_paid_recovery_registry::validate_proposal(d, p, own_operation)?;
    let source_rebound=proposal_source_rebind::validate_origin_and_current_target(context,p)?.is_some();
    let mut current_editorial_source=false;
    if p["recovery"]["kind"]=="recovered_context_metadata" {
        let recovery=&p["recovery"];
        let bundle=&row(d,"jobs",required(recovery,"prepareRunId")?)?["prepareBundle"];
        if recovery["prepareBundleId"]!=bundle["id"] || recovery["prepareBundleDigest"]!=bundle["digest"]
            || !context.equivalent_saved_source(bundle,required(p,"itemId")?).map_err(conflict)? {
            return Err(conflict("Recovered preparation source changed"));
        }
    }
    if let Some(expected)=p["reviewContextDigest"].as_str() {
        predicate=LocalPredicate::ReviewSourceUnavailable;
        let observed=context.review_fingerprint(required(p,"itemId")?).map_err(conflict)?;
        if observed!=expected {
            if allow_current_review && editorial_review::dedicated_current(context,p).is_ok() {
                current_editorial_source=true;
            } else {
                predicate=LocalPredicate::ReviewSourceChanged;
                review_fingerprints=dispatch_diagnostics::safe_review_fingerprints(expected,&observed);
                return Err(conflict("Review source context changed"));
            }
        }
    }
    if let Some(run_id) = p["prepareRunId"].as_str() {
        predicate=LocalPredicate::PreparationProvenance;
        let bundle = &row(d,"jobs",run_id)?["prepareBundle"];
        if p["prepareBundleId"] != bundle["id"] || p["prepareBundleDigest"] != bundle["digest"] {
            return Err(conflict("Proposal preparation provenance changed"));
        }
        predicate=LocalPredicate::PreparationSources;
        if source_rebound {
            // A native rebind preserves the original answering bundle as paid
            // provenance. Current recipient/material semantics require the new
            // dedicated review below; old source pins are never relabelled.
            context.reviewed_bundle_provenance(bundle,required(p,"itemId")?).map_err(conflict)?;
        } else if p["reviewContextDigest"].is_string() {
            if current_editorial_source {
                // Keep the immutable generation bundle and recipient membership.
                // The accepted dedicated receipt validates the current research
                // pins; its predecessor's pins remain historical provenance.
                context.reviewed_bundle_provenance(bundle, required(p,"itemId")?).map_err(conflict)?;
            } else if let Err(original)=context.reviewed_bundle_current(bundle, required(p,"itemId")?) {
                if allow_current_review && editorial_review::dedicated_current(context,p).is_ok() {
                    context.reviewed_bundle_provenance(bundle, required(p,"itemId")?).map_err(conflict)?;
                } else {return Err(conflict(original));}
            }
        } else {
            context.current(bundle).map_err(conflict)?;
        }
    }
    predicate=LocalPredicate::ConnectorBinding;
    let binding = active_binding(d)?;
    predicate=LocalPredicate::TargetBinding;
    let item = bound_item(&binding, row(d, "items", required(p, "itemId")?)?)?;
    if p["kind"]=="reply_and_close" {
        predicate=LocalPredicate::ReplyConstraints;
        reply_constraints::validate_reply(context,&item,required(p,"text")?).map_err(conflict)?;
    }
    predicate=LocalPredicate::Route;
    validate_route(p, &binding, &item)?;
    predicate=LocalPredicate::VideoEvidence;
    proposal_revalidation::require_current(context,p)?;
    proposal_source_rebind::require_current(context,p)?;
    let owner_close = operator_close::current_for_operation(d,p,&item,own_operation)?;
    if owner_close {
        if let Some(op) = own_operation {
            preparation_reservations::assert_operator_close_for_operation(d,p,op)?;
        }
    }
    if !owner_close && !media_context_gate::has_current_waiver(context,p)
        && !editorial_review::decision_media_current(context,p,&item).map_err(conflict)? {
        return Err(conflict("Video audio and visual evidence must be complete before approval or dispatch"));
    }
    predicate=LocalPredicate::ProposalItemContext;
    if item["revision"] != p["itemRevision"]
        || item["contextEvidenceDigest"] != p["contextEvidenceDigest"]
        || item["branchContextDigest"] != p["branchContextDigest"]
    {
        return Err(conflict("Comment context changed; create a new proposal"));
    }
    predicate=LocalPredicate::ItemAlreadyClosed;
    if item["workflow"] == "waiting" {
        return Err(conflict("Comment is waiting for operator review"));
    }
    if item["workflow"] == "closed" && !(p["allowClosedReply"]==true && p["kind"]=="reply_and_close") {
        return Err(conflict("Comment is already closed"));
    }
    predicate=LocalPredicate::VideoEvidence;
    media_context_gate::require(context,p,own_operation)?;
    Ok(item.clone())
    })();
    result.map_err(|error|dispatch_diagnostics::LocalPreconditionFailure {error,predicate,review_fingerprints})
}
fn create_approval(d: &mut Value, actor: &operator_auth::Actor, body: &Value) -> ApiResult<Value> {
    approval_admission::create(d, actor, body)
}

async fn approval_new(State(app): State<App>, axum::Extension(actor): axum::Extension<operator_auth::Actor>, Json(body): Json<Value>) -> ApiResult<Json<Value>> {
    if let Some(result)=local_admission::replay_committed(&app,"approval",&body,&actor).await? {return Ok(Json(result));}
    media_fullframes::refresh(&app).await?;
    app.change_admission(storage::AdmissionScope::Approval(&body),|d| {
        let request=local_admission::request(d,"approval",&body,&actor)?;
        if let Some(ref request)=request {
            if let Some(result)=local_admission::replay(d,request,&actor)? {return Ok(Json(result));}
        }
        let mut result=create_approval(d,&actor,&body)?;
        if let Some(ref request)=request {
            // Keyed admission receipts need the immutable approval identity,
            // not another copy of its embedded comments and media context.
            // Legacy callers without a key keep their existing full response.
            if !approval_admission::partial(&body)? {
                result=json!({"id":result["id"],"status":result["status"]});
            }
            local_admission::commit(d,request,&mut result)?;
        }
        Ok(Json(result))
    }).await
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
        Err(std::env::VarError::NotPresent) => Ok(dispatch_wave::MAX_IN_FLIGHT),
        Ok(value) => dispatch_parallelism_value(&value),
        Err(_) => Err(bad("Invalid COMMUNITYHERO_MAX_IN_FLIGHT")),
    }
}
fn dispatch_parallelism_value(value:&str)->ApiResult<usize> {
    value.parse::<usize>().ok().filter(|value|*value>0)
        .map(|value|value.min(dispatch_wave::MAX_IN_FLIGHT))
        .ok_or_else(||bad("COMMUNITYHERO_MAX_IN_FLIGHT must be a positive integer"))
}
fn recipient_operation_blocks_current(d: &Value, prior: &Value, proposal: &Value, item: &Value) -> bool {
    recipient_operation_blocks(prior,proposal,item)
        && !(operator_close::current(d,proposal,item).unwrap_or(false)
            && unknown_reply_close::permits(d,prior,proposal,item))
}
fn recipient_operation_blocks(prior: &Value, proposal: &Value, item: &Value) -> bool {
    let same_local_item=prior["itemId"]==proposal["itemId"];
    if !same_local_item {
        // A provider recipient may have more than one historical local row.
        // Never treat the other row's revision as permission for a follow-up.
        let target=&prior["target"];
        if target["objectId"]!=item["objectId"]||target["itemId"]!=item["itemId"] {return false;}
        let current=ConnectorBinding::from_json(&item["connectorBinding"]);
        let previous=ConnectorBinding::from_json(&target["connectorBinding"]);
        let same_scope=match (current,previous) {
            (Ok(current),Ok(previous))=>current.id==previous.id
                && current.workspace_id==previous.workspace_id
                && current.account_id==previous.account_id
                && current.connector==previous.connector
                && current.provider_account_id==previous.provider_account_id,
            // A malformed legacy binding with the same recipient is ambiguous
            // only inside this account. Explicit foreign accounts stay separate.
            (Ok(current),Err(_))=>target["connectorBinding"]["accountId"].as_str()
                .is_none_or(|account|account==current.account_id),
            _=>true,
        };
        return same_scope&&matches!(prior["status"].as_str(),Some("dispatching"|"unknown"|"succeeded"));
    }
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
    execute_admission::run(app, actor, key, json!({})).await
}
async fn set_outcome(app: &App, op: &Value, status: &str, evidence: Value) -> ApiResult<()> {
    let _timing = performance::Span::new("dispatch.persist_outcome");
    app.change_operation_outcome(op, status, |d| apply_operation_outcome(d, op, status, evidence)).await
}
fn apply_operation_outcome(d: &mut Value, op: &Value, status: &str, mut evidence: Value) -> ApiResult<()> {
        let key = required(op, "id")?;
        let stored = row_mut(d, "operations", key)?;
        dispatch_diagnostics::retain_execute_observation(stored,&mut evidence);
        stored["status"] = json!(status);
        stored["evidence"] = evidence;
        stored["updatedAt"] = json!(now());
        row_mut(d, "proposals", required(op, "proposalId")?)?["status"] = json!(status);
        if operation_outcome_needs_attention(d,op,status) {
            // Readiness alone changes. Keep drafts, revisions, context evidence
            // and historical authority intact; this grants no retry permission.
            row_mut(d,"items",required(op,"itemId")?)?["workflow"]=json!("attention");
        }
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
}
fn operation_outcome_has_ready_sibling(d:&Value,op:&Value)->bool {
    let Ok(item)=row(d,"items",op["itemId"].as_str().unwrap_or("")) else{return false};
    item_has_ready_proposal(d,item,op["proposalId"].as_str())
}
fn item_has_ready_proposal(d:&Value,item:&Value,excluded:Option<&str>)->bool {
    list(d,"proposals").iter().any(|p|p["id"].as_str()!=excluded&&p["itemId"]==item["id"]
        &&matches!(p["status"].as_str(),Some("draft"|"approved"))
        &&p["itemRevision"]==item["revision"]&&p["contextEvidenceDigest"]==item["contextEvidenceDigest"]
        &&p["branchContextDigest"]==item["branchContextDigest"]
        &&match p["kind"].as_str(){Some("reply_and_close")=>p["text"].as_str().is_some_and(|s|!s.trim().is_empty()),
            Some("close"|"hide"|"delete")=>true,_=>false})
}
fn repair_prepared_readiness(d:&mut Value) {
    let repairs:Vec<(String,&'static str)>=list(d,"items").iter().filter(|item|item["workflow"]=="prepared")
        .filter_map(|item|{
            let unknown=list(d,"operations").iter().any(|op|op["status"]=="unknown"&&op["itemId"]==item["id"]&&outcome_matches_item(d,op));
            let human_draft=item["draftEdited"]==true&&item["draft"].as_str().is_some_and(|s|!s.trim().is_empty());
            let reason=if unknown {"unresolved_external_operation"}
                else if !item_has_ready_proposal(d,item,None)&&!human_draft {"no_current_ready_proposal_or_human_draft"}
                else{return None};
            Some((item["id"].as_str()?.to_owned(),reason))
        }).collect();
    for (key,reason) in repairs {
        if let Ok(item)=row_mut(d,"items",&key){item["workflow"]=json!("attention");}
        list_mut(d,"audit").push(json!({"id":id(),"action":"item.readiness_repaired","refId":key,
            "reason":reason,"createdAt":now()}));
    }
}
fn operation_outcome_needs_attention(d:&Value,op:&Value,status:&str)->bool {
    if !matches!(status,"failed"|"stale"|"unknown")||!outcome_matches_item(d,op){return false;}
    let Ok(item)=row(d,"items",op["itemId"].as_str().unwrap_or("")) else{return false};
    if item["workflow"]!="prepared"||item["revision"].as_u64().is_none()
        ||item["revision"]!=op["target"]["revision"]
        ||item["contextEvidenceDigest"]!=op["target"]["contextEvidenceDigest"]
        ||item["branchContextDigest"]!=op["target"]["branchContextDigest"] {return false;}
    !d["operationOutcomeHasReadySibling"].as_bool().unwrap_or_else(||operation_outcome_has_ready_sibling(d,op))
}
#[cfg(test)]
#[path="operation_readiness_tests.rs"]
mod operation_readiness_tests;
fn readback_confirmed(v: &Value, action: &Value) -> bool {
    let (Some(action_id),Some(item_id))=(action["actionId"].as_str().filter(|id|!id.is_empty()),
        action["itemId"].as_str().filter(|id|!id.is_empty())) else{return false;};
    v["results"].as_array().is_some_and(|rows| {
        rows.len() == 1
            && rows[0]["actionId"].as_str()==Some(action_id)
            && rows[0]["itemId"].as_str()==Some(item_id)
            && rows[0]["status"] == "verified"
    })
}
async fn dispatch(app: App, op: Value) -> ApiResult<dispatch_diagnostics::Outcome> {
    let operation_id=op["id"].as_str().unwrap_or("").to_owned();
    performance::operation_scope(&operation_id,dispatch_inner(app,op)).await
}
async fn dispatch_inner(app: App, mut op: Value) -> ApiResult<dispatch_diagnostics::Outcome> {
    use dispatch_diagnostics::{Outcome, persist_stop};
    let _timing = performance::Span::new("dispatch.total");
    // Conversation quarantine applies to replies only. Closing an item must not
    // load the entire workspace merely to obtain an unconditional None.
    if op["action"]["action"] == "reply_and_close" {
      let blocker=match app.db.read_reply_conversation_blocker(&op).await {
        Ok(blocker)=>blocker,
        Err(error)=>return persist_stop(&app,&op,dispatch_diagnostics::read_failure("conversation_check_unavailable",&error)).await,
      };
      if let Some(blocker)=blocker {
        set_outcome(&app,&op,"stale",json!({"reason":"Earlier reply in this conversation requires readback",
            "blockedByOperationId":blocker,"providerCallAttempted":false})).await?;
        return Ok(Outcome::Stale);
      }
    }
    if !dispatch_authority::permit(&app, &op).await? { return Ok(Outcome::Stale); }
    let account = operation_account(&op)?;
    let context=app.bridge("context",json!({"account":account,"itemId":op["target"]["itemId"],"objectId":op["target"]["objectId"]})).await;
    match &context {
        Err(error)=>return persist_stop(&app,&op,dispatch_diagnostics::read_failure("fresh_context_read_failed",error)).await,
        Ok(value)=>if let Err(failure)=dispatch_diagnostics::context_check(value,&op["target"]) {
            return persist_stop(&app,&op,failure).await;
        },
    }
    let local=match app.db.read_dispatch_context(required(&op,"proposalId")?).await {
        Ok(data)=>data,
        Err(error)=>return persist_stop(&app,&op,dispatch_diagnostics::read_failure("local_context_read_failed",&error)).await,
    };
    if let Err(error)=knowledge::refresh_dispatch_visual_proofs(&local,required(&op,"itemId")?).await {
        return persist_stop(&app,&op,dispatch_diagnostics::read_failure("local_media_proof_unavailable",&error)).await;
    }
    if let Err(failure)=dispatch_diagnostics::local_check(&local,&op) {
        return persist_stop(&app,&op,failure).await;
    }
    if op["action"]["action"]=="reply_and_close" {
        if let Some(evidence)=context.as_ref().ok()
            .and_then(|value| dispatch_evidence::reply_baseline(&value["officialReplyIds"])) {
            dispatch_evidence::persist(&app,&mut op,evidence).await?;
        }
    }
    // Context, workspace and durable baseline checks all await. Recheck actual
    // authority after the final write, directly before the provider call.
    let Some(dispatch_permit) = dispatch_authority::begin(&app, &op).await? else { return Ok(Outcome::Stale); };
    let execution=dispatch_transport::execute(&app,&op,dispatch_permit).await;
    let result=execution.result;
    if op["action"]["action"]=="reply_and_close" {
        if let Some(evidence)=result.as_ref().ok()
            .and_then(|value| dispatch_evidence::from_receipt(value,&op["action"],account)) {
            dispatch_evidence::persist(&app,&mut op,evidence).await?;
        }
    }
    let confirmed_failure=result.as_ref().is_ok_and(|value|
        dispatch_evidence::confirmed_failure(value,&op["action"],account));
    let receipt=match &result {Ok(value)=>value.clone(),Err(error)=>json!({"error":error.1})};
    if confirmed_failure {
        let evidence=dispatch_diagnostics::known_failure_evidence(receipt.clone(),&op,account);
        app.change_execute_transition(&op,receipt,"failed",|d|
            apply_operation_outcome(d,&op,"failed",evidence)).await?;
        if let Some(error)=execution.local_error{return Err(error);}
        return Ok(Outcome::Failed);
    }
    // A successful/ambiguous receipt is insufficient: commit UNKNOWN before
    // independent readback, retaining the separate receipt commit on any error.
    let evidence=match result {
        Ok(v) => json!({"receipt":v,"verificationPhase":"verifying","providerRetryAllowed":false}),
        Err(e) => json!({"error":e.1,"verificationPhase":"verifying","providerRetryAllowed":false}),
    };
    app.change_execute_transition(&op,receipt,"unknown",|d|
        apply_operation_outcome(d,&op,"unknown",evidence)).await?;
    let outcome=if reconcile_one(&app, &op).await? {Outcome::Succeeded}else{Outcome::Unknown};
    if let Some(error)=execution.local_error{return Err(error);}
    Ok(outcome)
}
async fn reconcile_one(app: &App, op: &Value) -> ApiResult<bool> {
    performance::operation_scope(op["id"].as_str().unwrap_or(""),reconcile_one_inner(app,op)).await
}
async fn reconcile_one_inner(app: &App, op: &Value) -> ApiResult<bool> {
    let _timing = performance::Span::new("dispatch.reconcile.total");
    let account = operation_account(op)?;
    let result = app
        .bridge(
            "readback",
            json!({"account":account,"actions":[op["action"]]}),
        )
        .await;
    match result {
        Ok(v) if dispatch_evidence::readback_account_matches(&v,account)
            && readback_confirmed(&v, &op["action"]) => {
            set_outcome(app, op, "succeeded", v).await?;
            Ok(true)
        }
        Ok(mut v) => {
            if !v.is_object() {v=json!({"observation":v});}
            v["verificationPhase"]=json!("unconfirmed");
            v["providerRetryAllowed"]=json!(false);
            set_outcome(app, op, "unknown", v).await?;
            Ok(false)
        }
        Err(e) => {
            set_outcome(app, op, "unknown", json!({"phase":"readback","verificationPhase":"unavailable","error":e.1,"providerRetryAllowed":false})).await?;
            Ok(false)
        }
    }
}
async fn reconcile(State(app): State<App>, axum::Extension(actor): axum::Extension<operator_auth::Actor>, Path(key): Path<String>) -> ApiResult<Json<Value>> {
    let job=readback_recovery::manual(&app,&key,actor.public_json()).await?;
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

async fn knowledge_rule_revision(State(app): State<App>, axum::Extension(actor): axum::Extension<operator_auth::Actor>, Json(body): Json<Value>) -> ApiResult<Json<Value>> {
    if actor.role!="owner" {return Err(ApiError(StatusCode::FORBIDDEN,"Owner required".into()));}
    if body.as_object().is_none_or(|o|o.keys().any(|k|!matches!(k.as_str(),"plan"|"reviewedPlanHash"|"dryRun"))) {
        return Err(bad("Unsupported rule revision request field"));
    }
    let reviewed=required(&body,"reviewedPlanHash")?;
    let dry=body["dryRun"].as_bool().ok_or_else(||bad("Explicit dryRun required"))?;
    let reduce=|d:&mut Value|knowledge::rule_revision::apply(d,&body["plan"],reviewed,&now()).map_err(conflict);
    if dry {
        let mut candidate=app.read().await?;
        return Ok(Json(json!({"dryRun":true,"receipt":reduce(&mut candidate)?})));
    }
    app.change(|d| {
        let result=reduce(d)?;
        if result["replayed"]!=true {
            auto_prepare::reconcile_stale(d,chrono::Utc::now().timestamp());
            audit(d,"knowledge.rule_revision",result["requestId"].as_str().unwrap());
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
        if knowledge::protects_reply_url_material(d,&key) {
            return Err(conflict("Edit reply URL policy through its versioned endpoint"));
        }
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
        if knowledge::protects_reply_url_material(d,v["id"].as_str().unwrap()) { continue; }
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
async fn media(State(app): State<App>, axum::Extension(actor): axum::Extension<operator_auth::Actor>, Json(body): Json<Value>) -> ApiResult<Json<Value>> {
    let key = media_queue::selected_post_id(&body)?.to_string();
    Ok(Json(media_queue::request(&app, &key, &actor).await?))
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
        .route("/api/accounts", get(account_navigation::get))
        .route("/api/engine/status", get(accounts::status))
        .route("/api/engine/items/{id}/context-refresh", post(target_refresh::start))
        .route("/api/engine/posts/{id}/media-policy", get(post_media_policy::get).put(post_media_policy::put))
        .route("/api/engine/posts/{id}/media-status", get(media_status::get))
        .route("/api/engine/posts/{id}/audio-equivalence", get(media_audio_equivalence::get).put(media_audio_equivalence::put).delete(media_audio_equivalence::delete))
        .route("/api/engine/prepare", post(engine_prepare::prepare))
        .route("/api/engine/prepare/{id}/review-resume", post(preparation_review::chunks::resume))
        .route("/api/engine/prepare/{id}/recover-completed", post(preparation_review::chunks::recover_completed))
        .route("/api/engine/prepare/plan", post(prepare_plan::plan))
        .route("/api/engine/retained-recovery/plan", post(retained_paid_recovery_endpoint::plan))
        .route("/api/engine/retained-recovery/commit", post(retained_paid_recovery_endpoint::commit))
        .route("/api/engine/prepare/families", post(prepare_plan::conductor_family_windows))
        .route("/api/engine/prepare/facts/resolve", post(fact_followup::resolve))
        .route("/api/engine/jobs/{id}", get(engine_api::job))
        .route("/api/engine/reply-url-policy", get(knowledge::reply_url_policy::get).put(knowledge::reply_url_policy::put))
        .route("/api/engine/capabilities", get(engine_api::provider_capabilities))
.route("/api/engine/connection/auth-status", get(connector_auth_status::status))
        .route("/api/engine/scan", post(engine_api::scan))
        .route("/api/engine/export", get(engine_api::export))
        .route("/api/ready", get(readiness::ready))
        .route("/api/maintenance/runtime/register-target", post(runtime_lifecycle_http::register_target))
        .route("/api/maintenance/runtime/begin", post(runtime_lifecycle_http::begin))
        .route("/api/maintenance/runtime/status", post(runtime_lifecycle_http::status))
        .route("/api/maintenance/runtime/checkpoint", post(runtime_lifecycle_http::checkpoint))
        .route("/api/maintenance/runtime/stop-checkpoint", post(runtime_lifecycle_http::commit_stop))
        .route("/api/maintenance/runtime/resume", post(runtime_lifecycle_http::resume))
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
        .route("/api/items/review-bundle", get(bounded_review::get))
        .route("/api/conversations", post(conversation_new))
        .route(
            "/api/conversations/{id}/messages",
            post(conversation_message),
        )
        .route("/api/jobs/{id}/cancel", post(cancel))
        .route("/api/jobs/{id}", get(editorial_endpoint::get))
        .route("/api/proposals", post(operator_http::proposal_new))
        .route("/api/proposals/{id}/media-context", get(media_context_gate::get))
        .route("/api/proposals/{id}/media-context-waiver", post(media_context_gate::put))
        .route("/api/proposals/batch", post(operator_batch::create))
        .route("/api/proposals/batch/{request_id}", get(operator_batch::lookup))
        .route("/api/local-admissions/{kind}/{request_id}", get(local_admission::lookup))
        .route("/api/maintenance/recover-prepared", post(recover_prepared))
        .route("/api/maintenance/revalidation/recover-rejected", post(recover_rejected_revalidation))
        .route("/api/maintenance/preparation", post(auto_prepare::configure).get(auto_prepare::status))
        .route("/api/maintenance/preparation/continuous", post(continuous_preparation::configure))
        .route("/api/maintenance/connection/refresh", post(connection_recovery::refresh))
        .route("/api/maintenance/connection/admit", post(connection_recovery::admit))
        .route("/api/items/{id}/photo-acquisition/preflight", get(photo_acquisition::comment_preflight))
        .route("/api/maintenance/runtime-mode", get(runtime_mode::get))
        .route("/api/maintenance/preparation/restart", post(preparation_restart::restart))
        .route("/api/maintenance/media/retry-interrupted", post(media_queue::retry_interrupted))
        .route("/api/maintenance/media/retry-download-failed", post(media_queue::retry_failed_download))
        .route("/api/maintenance/media/reacquire-terminal-source", post(media_queue::request_reconciled_acquisition))
        .route("/api/maintenance/media/import-retained-source", post(media_source_import::import_retained_source))
        .route("/api/maintenance/media/acquire-photos", post(photo_acquisition::acquire))
        .route("/api/maintenance/media/request-video-frames", post(manual_frame_request::request))
        .route("/api/maintenance/media/photo-source/{id}", get(photo_acquisition::preflight))
        .route("/api/maintenance/media/cached-audio", post(post_media_policy::request_cached_audio))
        .route("/api/maintenance/repair-brand-roles", post(brand_repair::repair))
        .route("/api/proposals/{id}", patch(operator_http::proposal_patch))
        .route("/api/approvals", post(approval_new))
        .route("/api/proposals/editorial-review", post(editorial_endpoint::post))
        .route("/api/editorial-reviews/{id}/repairs", post(editorial_repair::post))
        .route("/api/conductor/runs", post(conductor::start))
        .route("/api/conductor/runs/{id}", get(conductor::status))
        .route("/api/conductor/runs/{id}/pause", post(conductor::pause))
        .route("/api/conductor/runs/{id}/resume", post(conductor::resume))
        .route("/api/conductor/runs/{id}/execute-reevaluate", post(conductor::execute_reevaluate))
        .route("/api/proposals/operator-review/preview", post(operator_editorial::preview))
        .route("/api/proposals/operator-review/frontier", post(operator_frontier::preview))
        .route("/api/proposals/revalidate/preview", post(proposal_revalidation::preview))
        .route("/api/proposals/revalidate", post(proposal_revalidation::post))
        .route("/api/proposals/source-rebind/preview", post(proposal_source_rebind::preview))
        .route("/api/proposals/source-rebind", post(proposal_source_rebind::post))
        .route("/api/proposals/operator-review", post(operator_editorial::post))
        .route("/api/approvals/{id}/execute", post(execute_admission::post))
        .route("/api/operations/{id}/reconcile", post(reconcile))
        .route("/api/feedback/events", post(operator_http::feedback_event))
        .route("/api/feedback/report", get(feedback_report))
        .route("/api/knowledge", get(operator_http::knowledge_catalog))
        .route("/api/knowledge/heads", get(operator_http::knowledge_heads))
        .route("/api/knowledge/entries/{id}", get(operator_http::knowledge_entry))
        .route("/api/knowledge/versions/{id}", get(operator_http::knowledge_version))
        .route("/api/knowledge/instructions", get(operator_http::instruction_catalog).post(knowledge_instruction))
        .route("/api/maintenance/knowledge-normalization", post(knowledge_normalization))
        .route("/api/maintenance/knowledge-rule-revision", post(knowledge_rule_revision))
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
        .layer(middleware::from_fn_with_state(app.clone(), runtime_lifecycle_app::native_context))
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
fn recover(d: &mut Value) -> ApiResult<()> {
    if !runtime_lifecycle_backlog::recovery_allowed(d)? { return Err(conflict("Runtime recovery is closed or queued suspension is not released")); }
    let mut retained_ids = std::collections::BTreeSet::new();
    for job in list(d, "jobs") {
        if runtime_lifecycle_backlog::preserved_at_recovery(d, job)? {
            retained_ids.insert(job["id"].as_str().ok_or_else(|| conflict("Retained queue identity missing"))?.to_owned());
        }
    }
    for job in list_mut(d, "jobs") {
        if job["status"] == "queued" && job["id"].as_str().is_some_and(|id| retained_ids.contains(id)) { continue; }
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
    if retained_ids.is_empty() {
        auto_prepare::recover_jobs(d,chrono::Utc::now().timestamp());
        auto_prepare::reconcile_stale(d,chrono::Utc::now().timestamp());
    }
    // Repair historical "prepared" labels conservatively; this preserves all
    // drafts and immutable action history, and never schedules an external call.
    repair_prepared_readiness(d);
    Ok(())
}
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    if std::env::args_os().nth(1).as_deref() == Some(std::ffi::OsStr::new("predecessor-recovery")) {
        let args=std::env::args_os().skip(1).map(|arg|arg.into_string())
            .collect::<Result<Vec<_>,_>>().map_err(|_|"Invalid predecessor recovery arguments")?;
        let result=predecessor_recovery::run_command(&args).await.map_err(|error|error.1)?;
        println!("{result}");
        return Ok(());
    }
    if std::env::args_os().nth(1).as_deref() == Some(std::ffi::OsStr::new("lifecycle-ledger-digest")) {
        return runtime_bootstrap_ledger_cli::run(std::env::args_os().skip(2)).await;
    }
    if std::env::args_os().nth(1).as_deref() == Some(std::ffi::OsStr::new("knowledge-import")) {
        return company_knowledge_cli::run(std::env::args_os().skip(2)).await;
    }
    if std::env::args_os().nth(1).as_deref() == Some(std::ffi::OsStr::new("instruction-install")) {
        return instruction_install_cli::run(std::env::args_os().skip(2)).await;
    }
    if std::env::args_os().nth(1).as_deref() == Some(std::ffi::OsStr::new("shared-moderation-install")) {
        return shared_moderation_install_cli::run(std::env::args_os().skip(2)).await;
    }
    dispatch_parallelism().map_err(|error| error.1)?;
    let retained_capture = retained_paid_recovery_registry::install_from_environment()?;
    retained_paid_recovery_registry::install(retained_capture)?;
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
    let lifecycle_admission = Arc::new(runtime_lifecycle_startup::from_environment(selected_account.display()).map_err(|e| e.1)?);
    working_generation::verify_startup(&db,selected_account,&data,&lifecycle_admission).await.map_err(|e|e.1)?;
    // Record the exact native launch before App or any worker exists; recovery
    // consumes its one-shot predecessor proof in the same transaction.
    if lifecycle_admission.requires_verified_startup() {
        lifecycle_admission.initialize_verified_startup(&db).await.map_err(|e|e.1)?;
    } else {
        db.change_runtime_lifecycle_with_ledger(|d| lifecycle_admission.initialize_workspace(d)).await.map_err(|e|e.1)?;
    }
    let node=std::env::var_os("COMMUNITYHERO_NODE").map(PathBuf::from).unwrap_or_else(||PathBuf::from("C:/AIDev/Workspaces/repos/Angry.Space.Auto-symphony/data/private/angryspace-conveyor/provider-runtime/bundle/node.exe"));
    let auth = operator_auth::Auth::load(&data).await?;
    let public_origin = operator_http::configured_origin(auth.is_some())?;
    let navigation = account_navigation::Navigation::load(selected_account, public_origin.as_deref())?;
    let app = App {lifecycle_task_count: Default::default(), lifecycle_owner: Arc::new(lifecycle_admission.identity().clone()), lifecycle_admission: lifecycle_admission.clone(), lifecycle_provider_token: Default::default(), lifecycle_work: Default::default(), media_discovery: Default::default(),preparation_wake: Default::default(),provider_session: Default::default(),
        account: selected_account,
        navigation,
        db,
        gate: Arc::new(writer_gate::WriterGate::default()),
        execution_gate: Arc::new(Mutex::new(())),
        preparation_workers: Arc::new(preparation_workers::Pool::from_env().map_err(|e| e.1)?),
        editorial_gate: Default::default(),assistant_gate: Arc::new(Mutex::new(())),assistant_chat_gate: Arc::new(Mutex::new(())),
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
    let retained_backlog = app.change(|d| {
        accounts::initialize(d, selected_account)?;
        if d.get(connection_gate::FIELD).is_some(){connection_gate::restart_closed(d)?;}
        knowledge::sync_catalog(d,&now()).map_err(bad)?;
        runtime_lifecycle_app::startup_recovery(d)
    })
    .await
    .map_err(|e| e.1)?;
    // Artifact availability is separate from automatic provider/model work.
    // Resume only explicitly admitted running campaigns, preserving all company
    // pauses and the original canonical child admission/operation references.
    if !retained_backlog { conductor::recover(&app).await.map_err(|e| e.1)?; }
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
    let readbacks=app.clone();
    tokio::spawn(worker_supervision::supervise_background("readback-recovery",move||{let readbacks=readbacks.clone();async move {
        let mut interval=tokio::time::interval(Duration::from_secs(15));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            interval.tick().await;
            if let Err(error)=readback_recovery::tick(&readbacks).await {
                eprintln!("Automatic readback recovery: {}",error.1);
            }
        }
    }}));
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
    if std::env::var("COMMUNITYHERO_COMMENT_PREPARATION_DISABLED").as_deref()!=Ok("1") {
    let automatic = app.clone();
    tokio::spawn(worker_supervision::supervise_background("preparation", move || { let automatic=automatic.clone(); async move {
        let mut interval = tokio::time::interval(Duration::from_secs(15));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tokio::select! {
                _ = interval.tick() => {},
                _ = automatic.preparation_wake.notified() => {},
            }
            if let Err(error) = auto_prepare::tick(&automatic).await {
                eprintln!("Automatic preparation queue: {}", error.1);
            }
        }
    }}));
    }
    }
    if media_queue::background_enabled() {
    let automatic_media = app.clone();
    tokio::spawn(worker_supervision::supervise_background("media", move || { let automatic_media=automatic_media.clone(); async move {
        let mut interval = tokio::time::interval(Duration::from_secs(15));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            media_queue::wait_for_work(&mut interval).await;
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

#[cfg(test)]
#[path="post_network_transition_tests.rs"]
mod post_network_transition_tests;

#[cfg(test)]
#[path = "worker_future_stack_tests.rs"]
mod worker_future_stack_tests;
