//! Finite, account-bound provider transport. Written requests are never replayed.
use crate::{ApiResult, internal};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    process::Stdio,
    sync::{
        Arc, Mutex as StateMutex,
        atomic::{AtomicBool, AtomicU8, Ordering},
    },
    time::Duration,
};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWriteExt},
    process::Command,
    sync::{Mutex, Notify, mpsc, oneshot},
    time::{Instant, timeout_at},
};

const REQUEST_LIMIT: usize = 2 * 1024 * 1024;
const RESPONSE_LIMIT: usize = 12 * 1024 * 1024;
const QUEUE_CAPACITY: usize = 32;
const ACTIVE_LIMIT: usize = 4;
const REQUEST_COUNT: usize = 400;
const DEADLINE: Duration = Duration::from_secs(240);
const LIFETIME: Duration = Duration::from_secs(900);
const QUEUED: u8 = 0;
const CANCELED: u8 = 1;
const DISPATCHED: u8 = 2;

/// A native observation channel bound to ONE awaited bridge call. It cannot
/// be deserialized from adapter output or inferred from an error message.
#[derive(Clone,Debug)]
pub(crate) struct TransportObservation {
    kind:crate::connection_gate::CessationKind,
    evidence_sha256:String,
}
impl TransportObservation {
    pub(crate) fn into_parts(self)->(crate::connection_gate::CessationKind,String) {
        (self.kind,self.evidence_sha256)
    }
}
#[derive(Clone,Default)]
struct TransportObserver(Arc<StateMutex<Option<TransportObservation>>>);
impl TransportObserver {
    fn clear(&self){*self.0.lock().unwrap()=None;}
    fn record(&self,kind:crate::connection_gate::CessationKind,evidence:Value){
        *self.0.lock().unwrap()=Some(TransportObservation{kind,evidence_sha256:format!("{:x}",Sha256::digest(evidence.to_string().as_bytes()))});
    }
}
tokio::task_local! {static TRANSPORT_OBSERVER:TransportObserver;}
tokio::task_local! {static AUTH_FAILURE_OBSERVER:Arc<AtomicBool>;}
pub(crate) async fn observe_auth_failure<T>(future:impl std::future::Future<Output=T>)->(T,bool) {
    let observed=Arc::new(AtomicBool::new(false));
    let result=AUTH_FAILURE_OBSERVER.scope(observed.clone(),future).await;
    (result,observed.load(Ordering::SeqCst))
}
/// Positive safe structured facts only. Diagnostic prose is never classified.
pub(crate) fn is_auth_failure(error:&Value)->bool {
    matches!(error["code"].as_str(),Some("AUTH_REQUIRED"|"CREDENTIAL_MISSING"|"CREDENTIAL_INVALID"|"ACCOUNT_SCOPE_MISMATCH"))
        || (error["code"]=="HTTP_ERROR"&&matches!(error["httpStatus"].as_u64(),Some(401|403)))
        || (error["transportStage"].as_str().is_some_and(|stage|stage.starts_with("read-auth-"))
            &&crate::dispatch_evidence::safe_connection_state(&error["connectionState"]).is_some())
}
pub(crate) async fn observe_transport<T>(future:impl std::future::Future<Output=T>)->(T,Option<TransportObservation>) {
    let observer=TransportObserver::default();
    let result=TRANSPORT_OBSERVER.scope(observer.clone(),future).await;
    let witness=observer.0.lock().unwrap().clone();(result,witness)
}
pub(crate) fn observe_contained_bridge(cessation:crate::runtime_native_child::Cessation,output_sha256:&str) {
    let _=TRANSPORT_OBSERVER.try_with(|observer|observer.record(crate::connection_gate::CessationKind::Contained,
        json!({"version":1,"source":"native_owned_child","processId":cessation.process_id(),"outputSha256":output_sha256,"treeAbsenceObserved":true})));
}

#[derive(Clone, Copy, Debug)]
enum RequestClass { Read, Mutation }
impl RequestClass {
    fn label(self) -> &'static str { match self { Self::Read => "read", Self::Mutation => "mutation" } }
}

/// Metadata about the bytes actually observed; never retain or print a frame,
/// parser message, stderr, credential, comment text or decoded untrusted ID.
#[derive(Debug, PartialEq, Eq)]
struct FrameFailure {
    reason: &'static str,
    observed_bytes: usize,
    observed_prefix_sha256: String,
    complete: bool,
    invalid_utf8: bool,
    serde_category: Option<&'static str>,
    line: Option<usize>,
    column: Option<usize>,
}
impl FrameFailure {
    fn new(reason: &'static str, bytes: &[u8], complete: bool) -> Self {
        Self { reason, observed_bytes: bytes.len(), observed_prefix_sha256: format!("{:x}",Sha256::digest(bytes)),
            complete, invalid_utf8: std::str::from_utf8(bytes).is_err(), serde_category: None, line: None, column: None }
    }
    fn json_error(bytes: &[u8], error: &serde_json::Error) -> Self {
        let mut failure=Self::new("invalid_json",bytes,true);
        failure.serde_category=Some(match error.classify() {
            serde_json::error::Category::Io=>"io", serde_json::error::Category::Syntax=>"syntax",
            serde_json::error::Category::Data=>"data", serde_json::error::Category::Eof=>"eof",
        });
        failure.line=Some(error.line()); failure.column=Some(error.column()); failure
    }
    fn view(&self) -> Value {
        json!({"reason":self.reason,"observedBytes":self.observed_bytes,
            "observedPrefixSha256":self.observed_prefix_sha256,"frameComplete":self.complete,
            "invalidUtf8":self.invalid_utf8,"serdeCategory":self.serde_category,
            "line":self.line,"column":self.column})
    }
}

#[cfg(unix)]
unsafe extern "C" {
    fn kill(pid: i32, signal: i32) -> i32;
}
#[cfg(unix)]
struct ProcessGroup(u32);
#[cfg(unix)]
impl Drop for ProcessGroup {
    fn drop(&mut self) {
        if let Ok(pid) = i32::try_from(self.0) {
            if pid > 0 {
                // Also contains descendants if Tokio aborts this task during
                // application shutdown, before normal asynchronous cleanup.
                unsafe {
                    kill(-pid, 9);
                }
            }
        }
    }
}

pub(crate) fn supports(operation: &str) -> bool {
    matches!(
        operation,
        "caps" | "scan" | "read" | "context" | "head" | "status" | "execute" | "readback"
    )
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ProviderDrainToken {
    pub(crate) owner_generation: String,
    pub(crate) epoch: u64,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ProviderDrainPhase { Running, Draining, Drained, Unresolved }
#[derive(Clone, Debug)]
pub(crate) struct ProviderDrainStatus {
    pub(crate) token: ProviderDrainToken,
    pub(crate) phase: ProviderDrainPhase,
    pub(crate) queued: usize,
    pub(crate) dispatched: usize,
    pub(crate) account: Option<String>,
    pub(crate) worker_generation: u64,
    pub(crate) worker_pid: Option<u32>,
    pub(crate) worker_retired: bool,
    pub(crate) containment: bool,
    pub(crate) unresolved_stage: Option<&'static str>,
}
impl ProviderDrainToken {
    pub(crate) fn as_json(&self) -> Value {
        json!({"ownerGeneration":self.owner_generation,"epoch":self.epoch})
    }
    pub(crate) fn from_json(value: &Value) -> ApiResult<Self> {
        let object = value.as_object().filter(|object| object.len() == 2)
            .ok_or_else(|| internal("Provider drain token invalid"))?;
        let owner = object.get("ownerGeneration").and_then(Value::as_str)
            .filter(|owner| uuid::Uuid::parse_str(owner).is_ok())
            .ok_or_else(|| internal("Provider drain owner generation invalid"))?;
        let epoch = object.get("epoch").and_then(Value::as_u64)
            .ok_or_else(|| internal("Provider drain epoch invalid"))?;
        Ok(Self { owner_generation: owner.to_owned(), epoch })
    }
}
impl ProviderDrainStatus {
    pub(crate) fn as_json(&self) -> Value {
        let phase = match self.phase {
            ProviderDrainPhase::Running => "running", ProviderDrainPhase::Draining => "draining",
            ProviderDrainPhase::Drained => "drained", ProviderDrainPhase::Unresolved => "unresolved",
        };
        json!({"token":self.token.as_json(),"phase":phase,"queued":self.queued,
            "dispatched":self.dispatched,"account":self.account,
            "workerGeneration":self.worker_generation,"workerPid":self.worker_pid,
            "workerRetired":self.worker_retired,"containment":self.containment,
            "unresolvedStage":self.unresolved_stage})
    }
}
struct Lifecycle {
    status: StateMutex<ProviderDrainStatus>,
    changed: Notify,
}
impl Default for Lifecycle {
    fn default() -> Self {
        Self {
            status: StateMutex::new(ProviderDrainStatus {
                token: ProviderDrainToken { owner_generation: uuid::Uuid::new_v4().to_string(), epoch: 0 },
                phase: ProviderDrainPhase::Running, queued: 0, dispatched: 0,
                account: None, worker_generation: 0, worker_pid: None,
                worker_retired: true, containment: true, unresolved_stage: None,
            }),
            changed: Notify::new(),
        }
    }
}
fn finish_drain(status: &mut ProviderDrainStatus) {
    if status.phase == ProviderDrainPhase::Draining && status.queued == 0
        && status.dispatched == 0 && status.worker_retired && status.containment
        && status.unresolved_stage.is_none() {
        status.phase = ProviderDrainPhase::Drained;
    }
}
fn generation_retired(lifecycle: &Lifecycle, containment: bool, settled: bool, reason: &'static str) {
    let mut status = lifecycle.status.lock().unwrap();
    status.containment = containment;
    status.worker_retired = containment;
    if containment { status.worker_pid = None; }
    status.unresolved_stage = (!settled || !containment).then_some(reason);
    if !containment || (status.phase == ProviderDrainPhase::Draining && !settled) {
        status.phase = ProviderDrainPhase::Unresolved;
    }
    finish_drain(&mut status);
    lifecycle.changed.notify_waiters();
}
// A slot belongs to the receiver/pending map, not the canceled HTTP caller.
// Its lifetime bounds status counts and retains DISPATCHED until correlated settlement.
struct Admission {
    state: Arc<AtomicU8>,
    lifecycle: Arc<Lifecycle>,
}
impl Drop for Admission {
    fn drop(&mut self) {
        let mut status = self.lifecycle.status.lock().unwrap();
        if self.state.load(Ordering::SeqCst) == DISPATCHED {
            status.dispatched -= 1;
        } else { status.queued -= 1; }
        finish_drain(&mut status);
        self.lifecycle.changed.notify_waiters();
    }
}
#[derive(Clone, Default)]
pub(crate) struct ProviderSession {
    handle: Arc<Mutex<Option<Handle>>>,
    lifecycle: Arc<Lifecycle>,
}
struct Handle {
    node: PathBuf,
    worker: PathBuf,
    account: String,
    sender: mpsc::Sender<Job>,
}
struct Job {
    id: String,
    observer:Option<TransportObserver>,
    auth_observer:Option<Arc<AtomicBool>>,
    request_class: RequestClass,
    frame: Vec<u8>,
    queued_until: Instant,
    state: Arc<AtomicU8>,
    started: oneshot::Sender<()>,
    reply: oneshot::Sender<ApiResult<Value>>,
    admission: Admission,
    trace: Option<crate::trace_context::TraceContext>,
    request_span: Option<crate::performance::Span>,
    queue_span: Option<crate::performance::Span>,
}
struct Pending {
    until: Instant,
    observer:Option<TransportObserver>,
    auth_observer:Option<Arc<AtomicBool>>,
    request_class: RequestClass,
    reply: oneshot::Sender<ApiResult<Value>>,
    _admission: Option<Admission>,
    trace: Option<crate::trace_context::TraceContext>,
    request_span: Option<crate::performance::Span>,
    receive_span: Option<crate::performance::Span>,
}
// Dropping a caller only cancels work which has not crossed dispatch admission.
struct CancelQueued(Arc<AtomicU8>);
impl Drop for CancelQueued {
    fn drop(&mut self) {
        let _ = self
            .0
            .compare_exchange(QUEUED, CANCELED, Ordering::SeqCst, Ordering::SeqCst);
    }
}

impl ProviderSession {
    pub(crate) fn begin_drain(&self) -> ApiResult<ProviderDrainToken> {
        let mut status = self.lifecycle.status.lock().unwrap();
        if status.phase == ProviderDrainPhase::Running {
            let Some(epoch) = status.token.epoch.checked_add(1) else {
                status.phase = ProviderDrainPhase::Unresolved;
                status.unresolved_stage = Some("epoch_exhausted");
                self.lifecycle.changed.notify_waiters();
                return Err(internal("Provider drain epoch exhausted; admission closed"));
            };
            status.token.epoch = epoch;
            // A prior contained terminal generation has already returned its
            // uncertainty to the durable caller. It is not still in flight.
            if status.worker_retired && status.containment && status.dispatched == 0 {
                status.unresolved_stage = None;
            }
            status.phase = ProviderDrainPhase::Draining;
            finish_drain(&mut status);
        }
        let token = status.token.clone();
        self.lifecycle.changed.notify_waiters();
        Ok(token)
    }
    pub(crate) fn drain_status(&self) -> ProviderDrainStatus {
        self.lifecycle.status.lock().unwrap().clone()
    }
    pub(crate) fn resume(&self, token: &ProviderDrainToken) -> ApiResult<()> {
        let mut status = self.lifecycle.status.lock().unwrap();
        if token != &status.token || status.phase != ProviderDrainPhase::Drained
            || status.queued != 0 || status.dispatched != 0 || !status.worker_retired
            || !status.containment || status.unresolved_stage.is_some() {
            return Err(internal("Provider drain resume rejected; exact completed epoch required"));
        }
        status.phase = ProviderDrainPhase::Running;
        self.lifecycle.changed.notify_waiters();
        Ok(())
    }
    pub(crate) async fn request(
        &self,
        node: &Path,
        worker: &Path,
        account: &str,
        request: Value,
    ) -> ApiResult<Value> {
        let observer=TRANSPORT_OBSERVER.try_with(Clone::clone).ok();
        if let Some(observer)=&observer {observer.record(crate::connection_gate::CessationKind::ProvenUnsent,
            json!({"version":1,"source":"provider_native_admission","enqueued":false}));}
        if !matches!(account, "likeavto" | "baw-russia")
            || request["account"].as_str() != Some(account)
            || !request["operation"].as_str().is_some_and(supports)
        {
            return Err(internal(
                "Provider session request binding invalid; request not dispatched",
            ));
        }
        let request_class=if request["operation"]=="execute" {RequestClass::Mutation} else {RequestClass::Read};
        let id = uuid::Uuid::new_v4().to_string();
        let request_span = crate::performance::current_trace_context()
            .filter(|context| context.to_json()["companyKey"] == account)
            .map(|context| crate::performance::Span::start("provider.request", crate::performance::SpanClass::Container, &context));
        let mut trace = request_span.as_ref().and_then(crate::performance::Span::context);
        let mut frame = serde_json::to_vec(&json!({"id":id,"request":request})).map_err(|_| {
            internal("Provider session request encoding failed; request not dispatched")
        })?;
        if frame.len() > REQUEST_LIMIT {
            return Err(internal(
                "Provider session request limit; request not dispatched",
            ));
        }
        if let Some(context) = &trace {
            let observed = serde_json::to_vec(&json!({"id":id,"request":request,"traceContext":context.to_json()})).ok();
            if let Some(observed) = observed.filter(|observed| observed.len() <= REQUEST_LIMIT) {
                frame = observed;
            } else {
                crate::trace_context::missing(context, "trace.sink", "sink_full");
                trace = None;
            }
        }
        frame.push(b'\n');
        let sender = {
            let mut handle = self.handle.lock().await;
            if self.lifecycle.status.lock().unwrap().phase != ProviderDrainPhase::Running {
                return Err(internal("Provider session draining; request not dispatched"));
            }
            if let Some(current) = handle.as_ref() {
                if current.node != node || current.worker != worker || current.account != account {
                    return Err(internal(
                        "Provider session configuration changed; request not dispatched",
                    ));
                }
            } else {
                let (sender, receiver) = mpsc::channel(QUEUE_CAPACITY);
                tokio::spawn(supervise(
                    node.to_owned(),
                    worker.to_owned(),
                    account.to_owned(),
                    receiver,
                    self.lifecycle.clone(),
                ));
                *handle = Some(Handle {
                    node: node.to_owned(),
                    worker: worker.to_owned(),
                    account: account.to_owned(),
                    sender,
                });
            }
            handle.as_ref().unwrap().sender.clone()
        };
        let queued_until = Instant::now() + DEADLINE;
        // No waiters become hidden admitted jobs outside the bounded queue.
        let permit = sender.try_reserve().map_err(|_| internal(
            "Provider session queue full or unavailable; request not dispatched"))?;
        let state = Arc::new(AtomicU8::new(QUEUED));
        let _cancel = CancelQueued(state.clone());
        let (reply, result) = oneshot::channel();
        let (started, dispatched) = oneshot::channel();
        {
            let mut status = self.lifecycle.status.lock().unwrap();
            if status.phase != ProviderDrainPhase::Running {
                return Err(internal("Provider session draining; request not dispatched"));
            }
            status.queued += 1;
            let job = Job {
            id,
            observer:observer.clone(),
            auth_observer:AUTH_FAILURE_OBSERVER.try_with(Clone::clone).ok(),
            request_class,
            frame,
            queued_until,
            state: state.clone(),
            started,
            reply,
            admission: Admission { state: state.clone(), lifecycle: self.lifecycle.clone() },
            queue_span: trace.as_ref().map(|context| crate::performance::Span::start("provider.queue.wait", crate::performance::SpanClass::Wait, context)),
            trace,
            request_span,
            };
            if let Some(observer)=&observer {observer.clear();}
            permit.send(job);
        }
        if timeout_at(queued_until, dispatched).await.is_err()
            && state
                .compare_exchange(QUEUED, CANCELED, Ordering::SeqCst, Ordering::SeqCst)
                .is_ok()
        {
            if let Some(observer)=&observer {observer.record(crate::connection_gate::CessationKind::ProvenUnsent,
                json!({"version":1,"source":"provider_native_queue","cancellationWon":true}));}
            return Err(internal(
                "Provider session queue timed out; request not dispatched",
            ));
        }
        result.await.map_err(|_| internal("Provider session stopped; action outcome may be unknown; providerRetryAllowed=false"))?
    }
}

async fn supervise(
    node: PathBuf,
    worker: PathBuf,
    account: String,
    mut receiver: mpsc::Receiver<Job>,
    lifecycle: Arc<Lifecycle>,
) {
    // Generations are strictly sequential: retirement/crash containment settles
    // before any future, never-written job can start another process.
    loop {
        let changed = lifecycle.changed.notified();
        tokio::pin!(changed);
        changed.as_mut().enable();
        if lifecycle.status.lock().unwrap().phase != ProviderDrainPhase::Running {
            loop {
                let job = {
                    let status = lifecycle.status.lock().unwrap();
                    if status.phase == ProviderDrainPhase::Running { None }
                    else { receiver.try_recv().ok() }
                };
                match job { Some(job) => reject_unsent(job), None => break }
            }
            { let mut status = lifecycle.status.lock().unwrap(); finish_drain(&mut status); }
        }
        let first = tokio::select! {
            biased;
            _ = &mut changed => continue,
            job = receiver.recv() => match job { Some(job) => job, None => break },
        };
        if stale(&first) {
            reject_unsent(first);
            continue;
        }
        if !run_generation(&node, &worker, &account, first, &mut receiver, &lifecycle).await {
            receiver.close();
            while let Some(job) = receiver.recv().await {
                reject_unsent(job);
            }
            break;
        }
    }
}

fn stale(job: &Job) -> bool {
    job.state.load(Ordering::SeqCst) != QUEUED
        || job.reply.is_closed()
        || Instant::now() >= job.queued_until
}
fn reject_unsent(mut job: Job) {
    if let Some(observer)=&job.observer {observer.record(crate::connection_gate::CessationKind::ProvenUnsent,
        json!({"version":1,"source":"provider_native_queue","requestId":job.id,"dispatched":false}));}
    if let Some(span) = &mut job.queue_span { span.finish("cancelled", Some("not_attempted")); }
    if let Some(span) = &mut job.request_span { span.finish("cancelled", Some("not_attempted")); }
    let _ = job.reply.send(Err(internal(
        "Provider session request expired or unavailable; request not dispatched",
    )));
}

async fn run_generation(
    node: &Path,
    worker: &Path,
    account: &str,
    first: Job,
    receiver: &mut mpsc::Receiver<Job>,
    lifecycle: &Arc<Lifecycle>,
) -> bool {
    let mut command = Command::new(node);
    command
        .args(["--experimental-strip-types"])
        .arg(worker)
        .args(["--account", account])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .env_remove("COMMUNITYHERO_TRACE_CONTEXT")
        .kill_on_drop(true);
    #[cfg(windows)]
    command.creation_flags(0x08000000);
    #[cfg(unix)]
    command.process_group(0);
    // Spawn is synchronous and linearized against begin_drain as well as dispatch.
    let spawned = {
        let mut status = lifecycle.status.lock().unwrap();
        if status.phase != ProviderDrainPhase::Running {
            drop(status); reject_unsent(first); return true;
        }
        let Some(generation) = status.worker_generation.checked_add(1) else {
            status.phase = ProviderDrainPhase::Unresolved;
            status.unresolved_stage = Some("generation_exhausted");
            drop(status); reject_unsent(first); return false;
        };
        match command.spawn() {
            Ok(child) => {
                status.worker_generation = generation;
                status.worker_pid = child.id();
                status.account = Some(account.to_owned());
                status.worker_retired = false;
                status.containment = false;
                status.unresolved_stage = None;
                Ok(child)
            },
            Err(error) => Err(error),
        }
    };
    let mut child = match spawned {
        Ok(child) => child,
        Err(_) => {
            if let Some(observer)=&first.observer {observer.record(crate::connection_gate::CessationKind::ProvenUnsent,
                json!({"version":1,"source":"provider_native_spawn","requestId":first.id,"spawned":false}));}
            let _ = first.reply.send(Err(internal(
                "Provider worker unavailable; request not dispatched",
            )));
            return true;
        }
    };
    let pid = child.id().unwrap_or(0);
    #[cfg(unix)]
    let _group = ProcessGroup(pid);
    #[cfg(windows)]
    let tree = match crate::ProcessTree::attach(&child) {
        Ok(tree) => tree,
        Err(_) => {
            let _ = child.kill().await;
            let _ = child.wait().await;
            if let Some(observer)=&first.observer {observer.record(crate::connection_gate::CessationKind::ProvenUnsent,
                json!({"version":1,"source":"provider_native_setup","requestId":first.id,"inputWritten":false}));}
            let _ = first.reply.send(Err(internal(
                "Provider worker containment failed; request not dispatched",
            )));
            generation_retired(lifecycle, false, false, "containment_attach");
            return false;
        }
    };
    let mut input = child.stdin.take();
    let Some(output) = child.stdout.take() else {
        let _ = child.kill().await;
        let _ = child.wait().await;
        if let Some(observer)=&first.observer {observer.record(crate::connection_gate::CessationKind::ProvenUnsent,
            json!({"version":1,"source":"provider_native_setup","requestId":first.id,"inputWritten":false}));}
        let _ = first.reply.send(Err(internal(
            "Provider worker output unavailable; request not dispatched",
        )));
        generation_retired(lifecycle, false, false, "stdout_unavailable");
        return false;
    };
    let (frames_tx, mut frames) = mpsc::channel(ACTIVE_LIMIT);
    let reader = tokio::spawn(read_frames(output, frames_tx));
    let mut pending = HashMap::<String, Pending>::new();
    let worker_generation=lifecycle.status.lock().unwrap().worker_generation;
    let mut frame_failure=None;
    let mut sent = 0usize;
    let mut retiring = false;
    let lifetime = Instant::now() + LIFETIME;
    let mut next = Some(first);
    let reason = loop {
        let changed = lifecycle.changed.notified();
        tokio::pin!(changed);
        changed.as_mut().enable();
        if lifecycle.status.lock().unwrap().phase != ProviderDrainPhase::Running {
            retiring = true;
            while let Ok(job) = receiver.try_recv() { reject_unsent(job); }
        }
        if let Some(mut job) = next.take() {
            let admitted = {
                let mut status = lifecycle.status.lock().unwrap();
                if status.phase == ProviderDrainPhase::Running && !stale(&job)
                    && job.state.compare_exchange(QUEUED, DISPATCHED, Ordering::SeqCst, Ordering::SeqCst).is_ok() {
                    status.queued -= 1;
                    status.dispatched += 1;
                    true
                } else { false }
            };
            if admitted {
                if let Some(span) = &mut job.queue_span { span.finish("completed", None); }
                let until = Instant::now() + DEADLINE;
                let _ = job.started.send(());
                let mut send_span = job.trace.as_ref().map(|context| crate::performance::Span::start("provider.send", crate::performance::SpanClass::Activity, context));
                pending.insert(
                    job.id.clone(),
                    Pending {
                        until,
                        observer:job.observer,
                        auth_observer:job.auth_observer,
                        request_class: job.request_class,
                        reply: job.reply,
                        _admission: Some(job.admission),
                        trace: job.trace.clone(),
                        request_span: job.request_span,
                        receive_span: None,
                    },
                );
                sent += 1;
                // Insert before writing: even a partial write has an uncertain
                // external outcome and must never be replayed.
                let write_until = pending
                    .values()
                    .map(|entry| entry.until)
                    .min()
                    .unwrap_or(until);
                let written = match input.as_mut() {
                    Some(input) => timeout_at(write_until, input.write_all(&job.frame)).await,
                    None => break "stdin_closed",
                };
                if !matches!(written, Ok(Ok(()))) {
                    if let Some(span) = &mut send_span { span.finish("unresolved", Some("outcome_unknown")); }
                    break "stdin_write";
                }
                if let Some(span) = &mut send_span { span.finish("completed", None); }
                if let Some(entry) = pending.get_mut(&job.id) {
                    entry.receive_span = job.trace.as_ref().map(|context| crate::performance::Span::start("provider.receive", crate::performance::SpanClass::Wait, context));
                }
            } else {
                reject_unsent(job);
            }
        }
        if sent >= REQUEST_COUNT || Instant::now() >= lifetime {
            retiring = true;
        }
        if retiring {
            input.take(); // EOF tells the worker to drain, never kills execute.
            if pending.is_empty() {
                break "retired";
            }
        }
        let nearest = pending.values().map(|entry| entry.until).min();
        let wake = if retiring {
            nearest.unwrap_or_else(Instant::now)
        } else {
            nearest.map_or(lifetime, |deadline| deadline.min(lifetime))
        };
        tokio::select! {
            biased;
            _ = &mut changed => (),
            frame = frames.recv() => {
                match frame {
                    Some(Ok(frame)) => match decode(frame, &mut pending) {
                        Ok(true) => retiring = true,
                        Ok(false) => (),
                        Err(failure) => { let reason=failure.reason; frame_failure=Some(failure); break reason; },
                    },
                    Some(Err(failure)) => { let reason=failure.reason; frame_failure=Some(failure); break reason; },
                    None => break "stdout_closed",
                }
            }
            _ = tokio::time::sleep_until(wake) => {
                if pending.values().any(|entry| Instant::now() >= entry.until) { break "request_timeout"; }
                retiring = true;
            }
            job = receiver.recv(), if !retiring && pending.len() < ACTIVE_LIMIT => {
                match job { Some(job) => next = Some(job), None => retiring = true }
            }
        }
    };
    input.take();
    reader.abort();
    let _ = reader.await;
    // On terminal failure, settle the process tree before returning uncertain
    // results and before admitting the next generation.
    let had_pending = !pending.is_empty();
    let (exit, natural_exit) = if reason == "retired" || reason == "stdout_closed" {
        match tokio::time::timeout(Duration::from_secs(2), child.wait()).await {
            Ok(Ok(status)) => (Some(status), true),
            _ => {
                let _ = child.kill().await;
                (child.wait().await.ok(), false)
            }
        }
    } else {
        let _ = child.kill().await;
        (child.wait().await.ok(), false)
    };
    #[cfg(windows)]
    let containment = tree.stop_and_wait().await.is_ok();
    #[cfg(unix)]
    let containment = stop_process_group(pid).await;
    #[cfg(not(any(windows, unix)))]
    let containment = false;
    let containment = containment && exit.is_some();
    for (request_id, mut entry) in pending {
        if containment {if let Some(observer)=&entry.observer {observer.record(crate::connection_gate::CessationKind::Contained,
            json!({"version":1,"source":"provider_native_containment","requestId":request_id,
                "workerGeneration":worker_generation,"pid":pid,"treeAbsenceObserved":true,"exitObserved":exit.is_some()}));}}
        let diagnostic = terminal_diagnostic(reason, pid, exit, containment, natural_exit,
            worker_generation, &request_id, entry.request_class, frame_failure.as_ref());
        if let Some(context) = &entry.trace { crate::trace_context::missing(context, "trace.marker", "missing_js_completion"); }
        if let Some(span) = &mut entry.receive_span { span.finish("unresolved", Some("outcome_unknown")); }
        if let Some(span) = &mut entry.request_span { span.finish("unresolved", Some("outcome_unknown")); }
        let _ = entry.reply.send(Err(internal(&diagnostic)));
    }
    let settled = !had_pending && natural_exit && exit.is_some_and(|status| status.success());
    generation_retired(lifecycle, containment, settled, reason);
    containment
}

#[cfg(unix)]
async fn stop_process_group(pid: u32) -> bool {
    // This worker owns a fresh POSIX process group (see process_group above).
    // Use the OS primitive directly; do not launch a shell or a kill helper.
    let Ok(pid) = i32::try_from(pid) else {
        return false;
    };
    if pid <= 0 {
        return false;
    }
    let until = Instant::now() + Duration::from_secs(30);
    loop {
        if unsafe { kill(-pid, 0) } == -1 {
            return std::io::Error::last_os_error().raw_os_error() == Some(3); // ESRCH
        }
        unsafe {
            kill(-pid, 9);
        } // SIGKILL after pending work settled/terminal failure
        if Instant::now() >= until {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

fn terminal_diagnostic(
    reason: &str,
    pid: u32,
    exit: Option<std::process::ExitStatus>,
    containment: bool,
    natural_exit: bool,
    worker_generation: u64,
    affected_request_id: &str,
    request_class: RequestClass,
    frame: Option<&FrameFailure>,
) -> String {
    let code = exit.and_then(|status| status.code());
    #[cfg(unix)]
    let signal = {
        use std::os::unix::process::ExitStatusExt;
        exit.and_then(|status| status.signal())
    };
    #[cfg(not(unix))]
    let signal: Option<i32> = None;
    let diagnostic=json!({"version":1,"role":"provider-worker","pid":pid,"stage":reason,
        "workerGeneration":worker_generation,"affectedRequestId":affected_request_id,
        "requestClass":request_class.label(),"exitCode":code,"signal":signal,"containment":containment,
        "terminationObservation":if natural_exit {"natural_exit_observed"}else{"supervisor_stop_requested"},
        "frame":frame.map(FrameFailure::view),"providerRetryAllowed":false});
    let outcome=match request_class {RequestClass::Mutation=>"action outcome may be unknown",RequestClass::Read=>"read failed; no mutation requested"};
    format!("Provider session failed; {outcome}; providerRetryAllowed=false; diagnostic={diagnostic}")
}

fn decode(frame: Vec<u8>, pending: &mut HashMap<String, Pending>) -> Result<bool, FrameFailure> {
    let envelope: Value = serde_json::from_slice(&frame).map_err(|error| FrameFailure::json_error(&frame,&error))?;
    if envelope.get("type").is_some() {
        return match envelope["type"].as_str() {
            Some("retiring") if envelope.as_object().is_some_and(|object| object.len() == 1) => {
                Ok(true)
            }
            Some("fatal") => Err(FrameFailure::new("worker_fatal",&frame,true)),
            _ => Err(FrameFailure::new("invalid_control",&frame,true)),
        };
    }
    let id = envelope["id"]
        .as_str()
        .filter(|id| {
            !id.is_empty()
                && id.len() <= 80
                && id
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
        })
        .ok_or_else(||FrameFailure::new("invalid_id",&frame,true))?;
    if !envelope["ok"].is_boolean()
        || (envelope["ok"] == true && envelope.get("result").is_none())
        || (envelope["ok"] == false && !envelope["error"].is_object())
    {
        return Err(FrameFailure::new("invalid_envelope",&frame,true));
    }
    let mut entry = pending.remove(id).ok_or_else(||FrameFailure::new("unknown_or_duplicate_id",&frame,true))?;
    if envelope["ok"]==false&&is_auth_failure(&envelope["error"]) {
        if let Some(observer)=&entry.auth_observer{observer.store(true,Ordering::SeqCst);}
    }
    if let Some(observer)=&entry.observer {observer.record(crate::connection_gate::CessationKind::Returned,
        json!({"version":1,"source":"provider_correlated_response","requestId":id,
            "frameSha256":format!("{:x}",Sha256::digest(&frame))}));}
    if let Some(context) = &entry.trace {
        if let Some(telemetry) = envelope.get("telemetry") { crate::trace_context::accept_telemetry(telemetry, context); }
        else { crate::trace_context::missing(context, "trace.marker", "missing_js_completion"); }
    }
    let outcome = if envelope["ok"] == true { "completed" } else { "failed" };
    if let Some(span) = &mut entry.receive_span { span.finish(outcome, None); }
    if let Some(span) = &mut entry.request_span { span.finish(outcome, None); }
    let result = if envelope["ok"] == true {
        Ok(envelope["result"].clone())
    } else {
        Err(internal(&crate::dispatch_evidence::adapter_failure(
            &envelope["error"],
        )))
    };
    let _ = entry.reply.send(result);
    Ok(false)
}

async fn read_frames(
    mut output: impl AsyncRead + Unpin,
    sender: mpsc::Sender<Result<Vec<u8>, FrameFailure>>,
) {
    let mut frame = Vec::new();
    let mut chunk = [0u8; 8192];
    loop {
        let count = match output.read(&mut chunk).await {
            Ok(0) => {
                if !frame.is_empty() {
                    let _ = sender.send(Err(FrameFailure::new("truncated_frame",&frame,false))).await;
                }
                return;
            }
            Ok(count) => count,
            Err(_) => {
                let _ = sender.send(Err(FrameFailure::new("stdout_read",&frame,false))).await;
                return;
            }
        };
        for &byte in &chunk[..count] {
            if byte == b'\n' {
                if sender.send(Ok(std::mem::take(&mut frame))).await.is_err() {
                    return;
                }
            } else {
                if frame.len() == RESPONSE_LIMIT {
                    let _ = sender.send(Err(FrameFailure::new("response_limit",&frame,false))).await;
                    return;
                }
                frame.push(byte);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry() -> (Pending, oneshot::Receiver<ApiResult<Value>>) {
        let (reply, receive) = oneshot::channel();
        (
            Pending {
                observer:None,
                auth_observer:None,
                until: Instant::now() + DEADLINE,
                request_class: RequestClass::Read,
                reply,
                _admission: None,
                trace: None,
                request_span: None,
                receive_span: None,
            },
            receive,
        )
    }
    #[tokio::test]
    async fn auth_barrier_requires_correlated_structured_failure_and_ignores_text() {
        assert!(!is_auth_failure(&json!({"code":"TRANSPORT_ERROR","message":"AUTH_REQUIRED HTTP_ERROR 401"})));
        assert!(!is_auth_failure(&json!({"code":"HTTP_ERROR","httpStatus":500})));
        let observed=Arc::new(AtomicBool::new(false));let (mut request,received)=entry();request.auth_observer=Some(observed.clone());
        let mut pending=HashMap::from([("ours".to_owned(),request)]);
        let foreign=json!({"id":"foreign","ok":false,"error":{"code":"AUTH_REQUIRED"}});
        assert!(decode(serde_json::to_vec(&foreign).unwrap(),&mut pending).is_err());assert!(!observed.load(Ordering::SeqCst));
        let correlated=json!({"id":"ours","ok":false,"error":{"code":"HTTP_ERROR","httpStatus":401}});
        decode(serde_json::to_vec(&correlated).unwrap(),&mut pending).unwrap();
        assert!(received.await.unwrap().is_err());assert!(observed.load(Ordering::SeqCst));
    }
    #[tokio::test]
    async fn correlates_out_of_order_and_rejects_duplicate_without_consuming_other_request() {
        let (one, r1) = entry();
        let (two, r2) = entry();
        let mut pending = HashMap::from([("one".into(), one), ("two".into(), two)]);
        assert_eq!(
            decode(
                br#"{"id":"two","ok":true,"result":2}"#.to_vec(),
                &mut pending
            ),
            Ok(false)
        );
        assert_eq!(r2.await.unwrap().unwrap(), json!(2));
        assert_eq!(
            decode(
                br#"{"id":"two","ok":true,"result":2}"#.to_vec(),
                &mut pending
            ),
            Err(FrameFailure::new("unknown_or_duplicate_id",br#"{"id":"two","ok":true,"result":2}"#,true))
        );
        assert!(pending.contains_key("one"));
        decode(
            br#"{"id":"one","ok":true,"result":1}"#.to_vec(),
            &mut pending,
        )
        .unwrap();
        assert_eq!(r1.await.unwrap().unwrap(), json!(1));
    }
    #[tokio::test]
    async fn cancellation_keeps_sent_slot_until_correlated_response() {
        let (sent, canceled) = entry();
        drop(canceled);
        let mut pending = HashMap::from([("sent".into(), sent)]);
        assert_eq!(pending.len(), 1);
        decode(br#"{"type":"retiring"}"#.to_vec(), &mut pending).unwrap();
        assert_eq!(pending.len(), 1);
        decode(
            br#"{"id":"sent","ok":true,"result":null}"#.to_vec(),
            &mut pending,
        )
        .unwrap();
        assert!(pending.is_empty());
    }
    #[tokio::test]
    async fn invalid_or_missing_telemetry_does_not_change_correlated_business_result() {
        let context = crate::trace_context::TraceContext::root("baw-russia", "fixture-runtime", 7, &"a".repeat(64)).unwrap().with_job("wire-job").unwrap();
        let ((), events) = crate::trace_context::capture(crate::trace_context::scope(context, async {
            for telemetry in [Some(json!({"rawComment":"private-fixture"})), None] {
                let (mut sent, result) = entry();sent.trace = crate::trace_context::current();
                let mut pending = HashMap::from([("one".into(), sent)]);
                let mut envelope = json!({"id":"one","ok":true,"result":{"published":true}});
                if let Some(telemetry) = telemetry { envelope["telemetry"] = telemetry; }
                assert_eq!(decode(serde_json::to_vec(&envelope).unwrap(), &mut pending), Ok(false));
                assert_eq!(result.await.unwrap().unwrap(), json!({"published":true}));assert!(pending.is_empty());
            }
        })).await;
        assert!(events.iter().any(|event| event["absenceReason"] == "invalid_telemetry"));
        assert!(events.iter().any(|event| event["absenceReason"] == "missing_js_completion"));
        assert!(!json!(events).to_string().contains("private-fixture"));
    }
    #[test]
    fn cancel_and_dispatch_have_one_winner() {
        let state = Arc::new(AtomicU8::new(QUEUED));
        drop(CancelQueued(state.clone()));
        assert!(
            state
                .compare_exchange(QUEUED, DISPATCHED, Ordering::SeqCst, Ordering::SeqCst)
                .is_err()
        );
        let state = Arc::new(AtomicU8::new(DISPATCHED));
        drop(CancelQueued(state.clone()));
        assert_eq!(state.load(Ordering::SeqCst), DISPATCHED);
    }
    #[tokio::test]
    async fn malformed_envelopes_do_not_resolve_pending() {
        for frame in [
            r#"{"id":"x","ok":true}"#,
            r#"{"id":"x","ok":false,"error":"private"}"#,
            r#"{"type":"retiring","id":"x"}"#,
            "not json",
        ] {
            let (sent, _) = entry();
            let mut pending = HashMap::from([("x".into(), sent)]);
            assert!(decode(frame.as_bytes().to_vec(), &mut pending).is_err());
            assert!(pending.contains_key("x"));
        }
    }
    #[tokio::test]
    async fn reader_rejects_unterminated_or_oversize_frames() {
        let (tx, mut rx) = mpsc::channel(4);
        read_frames(&b"{\"id\":"[..], tx).await;
        let failure=rx.recv().await.unwrap().unwrap_err();
        assert_eq!(failure.reason,"truncated_frame");assert!(!failure.complete);
        assert_eq!(failure.observed_bytes,6);
        let (tx, mut rx) = mpsc::channel(4);
        let oversized = vec![b'x'; RESPONSE_LIMIT + 1];
        read_frames(&oversized[..], tx).await;
        let failure=rx.recv().await.unwrap().unwrap_err();
        assert_eq!(failure.reason,"response_limit");assert!(!failure.complete);
        assert_eq!(failure.observed_bytes,RESPONSE_LIMIT);
    }
    #[test]
    fn malformed_frame_diagnostics_keep_only_observed_metadata_and_never_guess_identity(){
        let secret="TOKEN-PRIVATE-CANARY";
        let syntax=format!(r#"{{"id":"untrusted-frame-id","secret":"{secret}","broken": }}"#).into_bytes();
        let depth=format!("{}null{}","[".repeat(160),"]".repeat(160)).into_bytes();
        let invalid_utf8=vec![b'{',b'"',b'x',b'"',b':',b'"',0xff,b'"',b'}'];
        for bytes in [syntax,depth,invalid_utf8] {
            let (one,_)=entry();let (two,_)=entry();
            let mut pending=HashMap::from([("read-one".into(),one),("send-two".into(),two)]);
            let failure=decode(bytes.clone(),&mut pending).unwrap_err();
            assert_eq!(failure.reason,"invalid_json");assert_eq!(failure.observed_bytes,bytes.len());
            assert_eq!(failure.observed_prefix_sha256,format!("{:x}",Sha256::digest(&bytes)));
            assert!(failure.complete);assert!(failure.serde_category.is_some());
            assert!(failure.line.is_some_and(|value|value>0));assert!(failure.column.is_some());
            assert_eq!(failure.invalid_utf8,std::str::from_utf8(&bytes).is_err());
            assert_eq!(pending.len(),2); // Neither correlated waiter is consumed.
            let safe=failure.view().to_string();
            assert!(!safe.contains(secret));assert!(!safe.contains("untrusted-frame-id"));
            assert!(failure.view().get("requestId").is_none());
        }
    }
    #[test]
    fn terminal_failure_distinguishes_affected_reads_from_mutations_without_replay(){
        let frame=FrameFailure::new("truncated_frame",b"TOKEN-PRIVATE-CANARY",false);
        let read=terminal_diagnostic("truncated_frame",42,None,true,false,3,"read-one",RequestClass::Read,Some(&frame));
        let send=terminal_diagnostic("truncated_frame",42,None,true,false,3,"send-two",RequestClass::Mutation,Some(&frame));
        assert!(read.contains("read failed; no mutation requested"));assert!(!read.contains("action outcome may be unknown"));
        assert!(send.contains("action outcome may be unknown"));
        for value in [read,send] {
            assert!(!value.contains("TOKEN-PRIVATE-CANARY"));
            let diagnostic:Value=serde_json::from_str(value.split_once("diagnostic=").unwrap().1).unwrap();
            assert_eq!(diagnostic["workerGeneration"],3);assert_eq!(diagnostic["providerRetryAllowed"],false);
            assert_eq!(diagnostic["terminationObservation"],"supervisor_stop_requested");
            assert_eq!(diagnostic["frame"]["frameComplete"],false);
            assert!(diagnostic["frame"].get("requestId").is_none());
        }
    }
    #[tokio::test]
    async fn rejects_company_mismatch_and_unapproved_operation_before_spawn() {
        let session = ProviderSession::default();
        let absent = Path::new("absent-node");
        assert!(
            session
                .request(
                    absent,
                    absent,
                    "likeavto",
                    json!({"account":"baw-russia","operation":"execute"})
                )
                .await
                .is_err()
        );
        assert!(
            session
                .request(
                    absent,
                    absent,
                    "likeavto",
                    json!({"account":"likeavto","operation":"media"})
                )
                .await
                .is_err()
        );
        assert!(session.handle.lock().await.is_none());
    }

    #[tokio::test]
    async fn transport_witness_is_native_correlated_and_not_inferred_from_error_text() {
        let observer=TransportObserver::default();
        let (mut one,receive)=entry();one.observer=Some(observer.clone());
        let mut pending=HashMap::from([("native-request".to_owned(),one)]);
        assert!(decode(br#"{"id":"foreign","ok":true,"result":{}}"#.to_vec(),&mut pending).is_err());
        assert!(observer.0.lock().unwrap().is_none());
        assert!(pending.contains_key("native-request"));
        decode(br#"{"id":"native-request","ok":false,"error":{"message":"timeout"}}"#.to_vec(),&mut pending).unwrap();
        assert!(receive.await.unwrap().is_err());
        let witness=observer.0.lock().unwrap().clone().unwrap();
        assert!(matches!(witness.kind,crate::connection_gate::CessationKind::Returned));
        assert_eq!(witness.evidence_sha256.len(),64);
        let (_,unobserved)=observe_transport(async {Err::<(),_>(internal("request not dispatched; text alone is not proof"))}).await;
        assert!(unobserved.is_none());
        let session=ProviderSession::default();let absent=Path::new("not-executed");
        let (result,unsent)=observe_transport(session.request(absent,absent,"likeavto",json!({"account":"foreign","operation":"execute"}))).await;
        assert!(result.is_err());assert!(matches!(unsent.unwrap().kind,crate::connection_gate::CessationKind::ProvenUnsent));
        assert!(session.handle.lock().await.is_none());
    }

    #[tokio::test]
    async fn drain_without_worker_rejects_new_work_and_requires_exact_completed_epoch() {
        let session = ProviderSession::default();
        let token = session.begin_drain().unwrap();
        assert_eq!(session.begin_drain().unwrap(), token);
        assert_eq!(session.drain_status().phase, ProviderDrainPhase::Drained);
        let absent = Path::new("absent-node");
        let error = session.request(absent, absent, "likeavto",
            json!({"account":"likeavto","operation":"execute"})).await.unwrap_err();
        assert!(error.1.contains("request not dispatched"));
        assert!(session.handle.lock().await.is_none());
        let mut wrong = token.clone(); wrong.epoch += 1;
        assert!(session.resume(&wrong).is_err());
        assert!(session.resume(&ProviderSession::default().begin_drain().unwrap()).is_err());
        session.resume(&token).unwrap();
        assert!(session.resume(&token).is_err());
        let next = session.begin_drain().unwrap();
        assert_eq!(next.epoch, token.epoch + 1);
        assert!(session.resume(&token).is_err());
        assert_eq!(ProviderDrainToken::from_json(&next.as_json()).unwrap(), next);
        assert!(ProviderDrainToken::from_json(&json!({"ownerGeneration":next.owner_generation,
            "epoch":next.epoch,"approved":true})).is_err());
        assert!(ProviderDrainToken::from_json(&json!({"ownerGeneration":"other","epoch":1})).is_err());
        assert_eq!(session.drain_status().as_json()["phase"], "drained");
    }
    #[test]
    fn timeout_or_uncontained_generation_never_becomes_a_successful_drain() {
        for (contained, stage) in [(true, "request_timeout"), (false, "containment_attach")] {
            let session = ProviderSession::default();
            {
                let mut status = session.lifecycle.status.lock().unwrap();
                status.worker_retired = false; status.containment = false;
                status.worker_pid = Some(123);
            }
            let token = session.begin_drain().unwrap();
            generation_retired(&session.lifecycle, contained, false, stage);
            let status = session.drain_status();
            assert_eq!(status.phase, ProviderDrainPhase::Unresolved);
            assert_eq!(status.unresolved_stage, Some(stage));
            assert!(session.resume(&token).is_err());
        }
    }
    #[test]
    fn canceled_dispatched_admission_keeps_drain_open_until_slot_is_settled() {
        let session = ProviderSession::default();
        let state = Arc::new(AtomicU8::new(DISPATCHED));
        session.lifecycle.status.lock().unwrap().dispatched = 1;
        let slot = Admission { state: state.clone(), lifecycle: session.lifecycle.clone() };
        drop(CancelQueued(state.clone()));
        let token = session.begin_drain().unwrap();
        assert_eq!(session.drain_status().dispatched, 1);
        assert_eq!(session.drain_status().phase, ProviderDrainPhase::Draining);
        assert!(session.resume(&token).is_err());
        drop(slot);
        assert_eq!(session.drain_status().phase, ProviderDrainPhase::Drained);
        session.resume(&token).unwrap();
    }

    // These fixtures execute only temporary scripts: no provider module,
    // credentials, HTTP, production database or installed runtime is loaded.
    struct Fixture {
        _dir: tempfile::TempDir,
        node: PathBuf,
        worker: PathBuf,
        ledger: PathBuf,
    }
    impl Fixture {
        fn new() -> Self {
            let dir = tempfile::tempdir().unwrap();
            let worker = dir.path().join("worker.mjs");
            let ledger = dir.path().join("attempts.ndjson");
            let source = r#"
import readline from 'node:readline';
import fs from 'node:fs';
import { spawn } from 'node:child_process';
const ledger = LEDGER;
let active = 0, peak = 0, count = 0, closing = false;
const lines = readline.createInterface({ input: process.stdin });
const send = value => process.stdout.write(JSON.stringify(value) + '\n');
lines.on('line', line => {
  const { id, request } = JSON.parse(line);
  count++; active++; peak = Math.max(peak, active);
  const attempt = { pid: process.pid, count, id, operation: request.operation };
  fs.appendFileSync(ledger, JSON.stringify(attempt) + '\n');
  if (request.mode === 'crash') process.exit(17);
  if (request.mode === 'crash_after') { setTimeout(() => process.exit(17), 150); return; }
  const descendantPid = request.mode === 'descendant'
    ? spawn(process.execPath, ['-e', 'setInterval(() => {}, 1000)'], { stdio: 'ignore' }).pid : null;
  if (request.mode === 'retire') { closing = true; send({ type: 'retiring' }); }
  setTimeout(() => {
    active--;
    send({ id, ok: true, result: { ...attempt, peak, descendantPid } });
    if (closing && active === 0) process.exit(0);
  }, request.delay || 0);
});
lines.on('close', () => { closing = true; if (!active) process.exit(0); });
"#
            .replace("LEDGER", &serde_json::to_string(&ledger).unwrap());
            std::fs::write(&worker, source).unwrap();
            let node = std::env::var_os("COMMUNITYHERO_TEST_NODE")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("node"));
            Self {
                _dir: dir,
                node,
                worker,
                ledger,
            }
        }
        fn attempts(&self) -> Vec<Value> {
            std::fs::read_to_string(&self.ledger)
                .unwrap_or_default()
                .lines()
                .map(|line| serde_json::from_str(line).unwrap())
                .collect()
        }
        async fn wait_attempts(&self, expected: usize) {
            tokio::time::timeout(Duration::from_secs(10), async {
                while self.attempts().len() < expected {
                    tokio::time::sleep(Duration::from_millis(5)).await;
                }
            })
            .await
            .expect("offline Node fixture did not receive request");
        }
        async fn call(&self, session: &ProviderSession, request: Value) -> ApiResult<Value> {
            tokio::time::timeout(
                Duration::from_secs(15),
                session.request(&self.node, &self.worker, "likeavto", request),
            )
            .await
            .expect("offline provider fixture did not settle")
        }
        async fn wait_drained(&self, session: &ProviderSession) -> ProviderDrainStatus {
            tokio::time::timeout(Duration::from_secs(10), async {
                loop {
                    let status = session.drain_status();
                    assert_ne!(status.phase, ProviderDrainPhase::Unresolved,
                        "artificial fixture drain failed: {:?}", status.unresolved_stage);
                    if status.phase == ProviderDrainPhase::Drained { return status; }
                    tokio::time::sleep(Duration::from_millis(5)).await;
                }
            }).await.expect("offline drain did not settle; deadline is not success")
        }
    }
    #[tokio::test]
    async fn node_fixture_reuses_worker_and_bounds_concurrent_dispatch() {
        let fixture = Fixture::new();
        let session = ProviderSession::default();
        let mut tasks = tokio::task::JoinSet::new();
        for _ in 0..9 {
            let session = session.clone();
            let node = fixture.node.clone();
            let worker = fixture.worker.clone();
            tasks.spawn(async move {
                session
                    .request(
                        &node,
                        &worker,
                        "likeavto",
                        json!({"account":"likeavto","operation":"context","delay":80}),
                    )
                    .await
            });
        }
        let mut pids = std::collections::HashSet::new();
        while let Some(result) = tokio::time::timeout(Duration::from_secs(15), tasks.join_next())
            .await
            .unwrap()
        {
            let result = result.unwrap().unwrap();
            pids.insert(result["pid"].as_u64().unwrap());
            assert!(result["peak"].as_u64().unwrap() <= ACTIVE_LIMIT as u64);
        }
        assert_eq!(pids.len(), 1);
        assert_eq!(fixture.attempts().len(), 9);
    }
    #[tokio::test]
    async fn node_fixture_crash_is_uncertain_and_never_replays_execute() {
        let fixture = Fixture::new();
        let session = ProviderSession::default();
        let (result,observation)=observe_transport(fixture.call(
                &session,
                json!({"account":"likeavto","operation":"execute","mode":"crash"}),
            )).await;
        let error=result.unwrap_err();
        assert!(error.1.contains("providerRetryAllowed=false"));
        let diagnostic:Value=serde_json::from_str(error.1.split_once("diagnostic=").unwrap().1).unwrap();
        assert_eq!(diagnostic["exitCode"],17);
        assert_eq!(diagnostic["stage"],"stdout_closed");
        assert_eq!(diagnostic["requestClass"],"mutation");
        assert_eq!(diagnostic["terminationObservation"],"natural_exit_observed");
        assert_eq!(diagnostic["containment"],true);
        assert_eq!(diagnostic["providerRetryAllowed"],false);
        let first=fixture.attempts();assert_eq!(first.len(),1);
        assert_eq!(diagnostic["affectedRequestId"],first[0]["id"]);
        assert_eq!(diagnostic["pid"],first[0]["pid"]);
        let observation=observation.expect("Native supervisor must positively observe owned worker containment");
        assert!(matches!(observation.kind,crate::connection_gate::CessationKind::Contained));
        assert_eq!(observation.evidence_sha256.len(),64);
        let next = fixture
            .call(
                &session,
                json!({"account":"likeavto","operation":"readback"}),
            )
            .await
            .unwrap();
        let attempts = fixture.attempts();
        assert_eq!(attempts.len(), 2);
        assert_eq!(attempts[0]["operation"], "execute");
        assert_eq!(attempts[1]["operation"], "readback");
        assert_ne!(attempts[0]["pid"], next["pid"]);
    }
    #[tokio::test]
    async fn node_fixture_canceled_execute_keeps_worker_and_does_not_replay() {
        let fixture = Fixture::new();
        let session = ProviderSession::default();
        let cloned = session.clone();
        let node = fixture.node.clone();
        let worker = fixture.worker.clone();
        let caller = tokio::spawn(async move {
            cloned
                .request(
                    &node,
                    &worker,
                    "likeavto",
                    json!({"account":"likeavto","operation":"execute","delay":200}),
                )
                .await
        });
        fixture.wait_attempts(1).await;
        caller.abort();
        let _ = caller.await;
        let next = fixture
            .call(
                &session,
                json!({"account":"likeavto","operation":"readback","delay":250}),
            )
            .await
            .unwrap();
        let attempts = fixture.attempts();
        assert_eq!(attempts.len(), 2);
        assert_eq!(attempts[0]["pid"], next["pid"]);
    }
    #[tokio::test]
    async fn node_fixture_retirement_drains_and_then_starts_future_generation() {
        let fixture = Fixture::new();
        let session = ProviderSession::default();
        let first = fixture
            .call(
                &session,
                json!({"account":"likeavto","operation":"execute","mode":"retire","delay":80}),
            )
            .await
            .unwrap();
        let next = fixture
            .call(
                &session,
                json!({"account":"likeavto","operation":"readback"}),
            )
            .await
            .unwrap();
        assert_ne!(first["pid"], next["pid"]);
        assert_eq!(fixture.attempts().len(), 2);
    }
    #[tokio::test]
    async fn node_fixture_explicit_drain_keeps_canceled_execute_and_rejects_queued_without_replay() {
        let fixture = Fixture::new();
        let session = ProviderSession::default();
        let mut callers = Vec::new();
        for index in 0..8 {
            let cloned = session.clone(); let node = fixture.node.clone(); let worker = fixture.worker.clone();
            callers.push(tokio::spawn(async move {
                cloned.request(&node, &worker, "likeavto",
                    json!({"account":"likeavto","operation":"execute","delay":700})).await
            }));
            if index == 0 { fixture.wait_attempts(1).await; }
        }
        fixture.wait_attempts(ACTIVE_LIMIT).await;
        tokio::time::timeout(Duration::from_secs(5), async {
            while session.drain_status().queued != 4 { tokio::task::yield_now().await; }
        }).await.unwrap();
        let token = session.begin_drain().unwrap();
        let owner = session.drain_status();
        assert_eq!(owner.dispatched, ACTIVE_LIMIT);
        assert_eq!(owner.worker_generation, 1);
        assert!(owner.worker_pid.is_some());
        assert_eq!(owner.phase, ProviderDrainPhase::Draining);
        callers[0].abort(); // A dispatched slot cannot be removed by this cancellation.
        assert!(session.resume(&token).is_err());
        let rejected = fixture.call(&session,
            json!({"account":"likeavto","operation":"readback"})).await.unwrap_err();
        assert!(rejected.1.contains("request not dispatched"));
        let completed = fixture.wait_drained(&session).await;
        assert_eq!(completed.queued, 0); assert_eq!(completed.dispatched, 0);
        assert!(completed.worker_retired && completed.containment);
        assert_eq!(fixture.attempts().len(), ACTIVE_LIMIT);
        for caller in callers { let _ = caller.await; }
        session.resume(&token).unwrap();
        let next = fixture.call(&session,
            json!({"account":"likeavto","operation":"readback"})).await.unwrap();
        assert_ne!(owner.worker_pid.unwrap() as u64, next["pid"].as_u64().unwrap());
        assert_eq!(fixture.attempts().len(), ACTIVE_LIMIT + 1);
    }
    #[tokio::test]
    async fn node_fixture_crash_during_drain_is_unresolved_even_after_containment() {
        let fixture = Fixture::new(); let session = ProviderSession::default();
        let cloned = session.clone(); let node = fixture.node.clone(); let worker = fixture.worker.clone();
        let caller = tokio::spawn(async move {
            cloned.request(&node, &worker, "likeavto",
                json!({"account":"likeavto","operation":"execute","mode":"crash_after"})).await
        });
        fixture.wait_attempts(1).await;
        let token = session.begin_drain().unwrap();
        let error = caller.await.unwrap().unwrap_err();
        assert!(error.1.contains("providerRetryAllowed=false"));
        tokio::time::timeout(Duration::from_secs(10), async {
            while session.drain_status().phase != ProviderDrainPhase::Unresolved {
                tokio::task::yield_now().await;
            }
        }).await.unwrap();
        let status = session.drain_status();
        assert!(status.containment);
        assert!(status.unresolved_stage.is_some());
        assert!(session.resume(&token).is_err());
        assert_eq!(fixture.attempts().len(), 1);
    }
    #[tokio::test]
    async fn node_fixture_drain_waits_for_owned_descendant_containment() {
        let fixture = Fixture::new(); let session = ProviderSession::default();
        let result = fixture.call(&session,
            json!({"account":"likeavto","operation":"context","mode":"descendant"})).await.unwrap();
        assert!(result["descendantPid"].as_u64().is_some_and(|pid| pid > 0));
        let token = session.begin_drain().unwrap();
        let status = fixture.wait_drained(&session).await;
        assert!(status.worker_retired && status.containment);
        assert_eq!(status.worker_pid, None);
        session.resume(&token).unwrap();
    }
}
