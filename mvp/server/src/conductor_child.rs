//! Server-owned, bounded-lifetime Node continuation. No child owns send truth.
use crate::*;
use std::{collections::HashSet, sync::OnceLock};
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncWrite, BufReader};
use std::future::Future;

static RUNNING: OnceLock<std::sync::Mutex<HashSet<String>>> = OnceLock::new();
pub(crate) struct Registration(String);
impl Drop for Registration {
    fn drop(&mut self) { RUNNING.get_or_init(Default::default).lock().unwrap_or_else(|e| e.into_inner()).remove(&self.0); }
}
fn registration_key(app:&App,run:&str)->String {json!([app.data.to_string_lossy(),app.account.key(),run]).to_string()}
pub(crate) async fn wait_stopped(app:&App,run:&str)->ApiResult<()> {
    let key=registration_key(app,run);
    tokio::time::timeout(Duration::from_secs(15),async {
        while RUNNING.get_or_init(Default::default).lock().unwrap_or_else(|e|e.into_inner()).contains(&key) {
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }).await.map_err(|_|conflict("Prior conductor child is still stopping"))
}
pub(crate) fn claim(app:&App,run:&str)->Option<Registration> {
    let key=registration_key(app,run);
    if RUNNING.get_or_init(Default::default).lock().unwrap_or_else(|e|e.into_inner()).insert(key.clone()) {Some(Registration(key))}else{None}
}
pub(crate) fn spawn_claimed(app: &App, run: String, registration:Registration, selected_claim:Value) {
    let app=app.clone();
    tokio::spawn(async move {
        let registration=registration;
        let mut launch=match conductor::prepare_child(&app,&run).await {
            Ok(v)=>Arc::new(v),Err(_)=>{
                // A close that won the final pre-launch fence is deferred,
                // rather than being converted into terminal campaign failure.
                let dependency=async {
                    let job=app.db.read_job(&run).await?.ok_or_else(||conflict("Conductor run missing"))?;
                    conductor_authority::check_control(&app,&job).await?;
                    conductor::connection_continuation::check_journal(&app,&job).await?;
                    let metadata=app.db.read_metadata().await?;
                    Ok::<_,ApiError>(job["conductor"]["mode"]=="execute"&&
                        (dispatch_authority::require_unheld(&app).is_err()||connection_gate::current_continuation_admission(&metadata)?.is_none()))
                }.await;
                drop(registration);
                if matches!(dependency,Ok(true)) {let _=conductor::connection_continuation::continue_run(&app,&run,conductor::connection_continuation::Reason::Wake).await;}
                else {let _=conductor::fail_launch(&app,&run).await;}
                return;
            }
        };
        if launch.mode=="execute"&&launch.continuation_claim!=selected_claim {
            let _=conductor::fail_launch(&app,&run).await;return;
        }
        let result=loop {
            let result=supervise(&app,launch.clone()).await;
            if !result.as_ref().err().is_some_and(safe_transport_failure) {break result;}
            match conductor::restart_allowed(&app,&launch).await {
                Ok(true)=>{},Ok(false)=>break result,Err(_)=>break Err(internal("Conductor recovery validation failed")),
            }
            // Recovery observes existing durable checkpoints/receipts. It does
            // not mint a new run, lease, admission key, or effect operation.
            tokio::time::sleep(Duration::from_millis(250)).await;
            let next=match conductor::prepare_child_again(&app,&run,&launch.continuation_claim).await {
                Ok(next)=>Arc::new(next),Err(_)=>break Err(internal("Conductor recovery launch failed")),
            };
            if next.run_id!=launch.run_id || next.lease_generation!=launch.lease_generation
                || next.account!=launch.account || !next.resume
                || dispatch_authority::approval_binding(&next.actor)!=dispatch_authority::approval_binding(&launch.actor) {
                break Err(internal("Conductor recovery binding changed"));
            }
            launch=next;
        };
        let native_dependency=matches!(&result,Ok(Disposition::NativeDependency(_)));
        match result {
            Ok(Disposition::Dependency(value))|Ok(Disposition::NativeDependency(value))=>{
                // Child dependency returns only after actual normal exit and
                // owned containment. NativeDependency is created before any
                // RPC/process launch. Defer precedes registration release.
                let deferred=if native_dependency {conductor::connection_continuation::defer_before_process(&app,&launch,&value).await}
                    else {conductor::connection_continuation::defer_child(&app,&launch,&value).await};
                if deferred.is_ok() {
                    drop(registration);
                    let _=tokio::time::timeout(Duration::from_secs(15),conductor::wake_deferred(&app)).await;
                }else{let _=conductor::finish_child(&app,&launch,Err(internal("Conductor dependency proof invalid"))).await;}
            },
            Ok(Disposition::Result(value))=>{let _=conductor::finish_child(&app,&launch,Ok(value)).await;},
            Err(error)=>{let _=conductor::finish_child(&app,&launch,Err(error)).await;},
        }
    });
}
fn safe_transport_failure(error:&ApiError)->bool {
    error.0==StatusCode::INTERNAL_SERVER_ERROR && matches!(error.1.as_str(),
        "Conductor runtime unavailable"|"Conductor configuration failed"|"Conductor heartbeat failed"
        |"Conductor child failed; recover checkpoint"|"Conductor ended without result; recover checkpoint")
}

const MAX_FRAME:usize=1024*1024;
// read_line/read_until allocate before checking a limit. Bound allocation while
// consuming each buffered segment, including malformed/no-newline output.
async fn frame<R: AsyncBufRead + Unpin>(reader: &mut R) -> ApiResult<Option<Vec<u8>>> {
    let mut value=Vec::new();
    loop {
        let buffer=reader.fill_buf().await.map_err(|_|internal("Conductor output unavailable"))?;
        if buffer.is_empty(){return if value.is_empty(){Ok(None)}else{Err(internal("Incomplete conductor output frame"))};}
        let end=buffer.iter().position(|b|*b==b'\n').map(|n|n+1);
        let count=end.unwrap_or(buffer.len());
        if value.len()+count>MAX_FRAME{return Err(internal("Conductor output exceeds bound"));}
        value.extend_from_slice(&buffer[..count]); reader.consume(count);
        if end.is_some(){return Ok(Some(value));}
    }
}
fn output(value: &[u8]) -> ApiResult<Value> {
    let value:Value=serde_json::from_slice(value).map_err(|_|internal("Invalid conductor output"))?;
    let fields=value.as_object().ok_or_else(||internal("Invalid conductor output"))?;
    let payload=match value["type"].as_str(){Some("report")=>"report",Some("result")=>"result",Some("dependency_wait")=>"dependency",_=>return Err(internal("Invalid conductor output contract"))};
    if fields.len()!=2||!fields.contains_key(payload) {
        return Err(internal("Invalid conductor output contract"));
    }
    Ok(value)
}
fn settle_output(success:bool,result:Option<Value>)->ApiResult<Value> {
    if !success {
        // An explicit blocked result is a disposition, not an unexplained
        // transport exit eligible for automatic resume.
        if result.as_ref().is_some_and(|value|value["mode"]=="blocked") {return Ok(result.unwrap());}
        return Err(internal("Conductor child failed; recover checkpoint"));
    }
    result.ok_or_else(||internal("Conductor ended without result; recover checkpoint"))
}
#[derive(Debug)]
enum Disposition {Result(Value),Dependency(Value),NativeDependency(Value)}
fn settle_disposition(success:bool,result:Option<Disposition>)->ApiResult<Disposition> {
    match result {
        Some(Disposition::Dependency(value)) if success=>Ok(Disposition::Dependency(value)),
        Some(Disposition::Dependency(_))=>Err(internal("Conductor dependency exit was not normal")),
        Some(Disposition::NativeDependency(_))=>Err(internal("Native conductor disposition cannot originate in child output")),
        Some(Disposition::Result(value))=>settle_output(success,Some(value)).map(Disposition::Result),
        None=>settle_output(success,None).map(Disposition::Result),
    }
}

// These monitor futures are borrowed by one select, never spawned/detached.
// Dropping the select cancels every pending pulse/read and releases stdin.
enum MonitorTicks {
    Interval(tokio::time::Interval),
    #[cfg(test)] Manual(tokio::sync::mpsc::Receiver<()>),
}
impl MonitorTicks {
    fn interval(period:Duration)->Self {
        let mut value=tokio::time::interval(period);
        value.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        Self::Interval(value)
    }
    async fn tick(&mut self)->ApiResult<()> {
        match self {
            Self::Interval(interval)=>{interval.tick().await;Ok(())},
            #[cfg(test)] Self::Manual(receiver)=>receiver.recv().await.map(|_|()).ok_or_else(||internal("Conductor test clock closed")),
        }
    }
}
async fn heartbeat_monitor<W>(stdin:&mut W,generation:u64,mut ticks:MonitorTicks,write_bound:Duration)->ApiResult<()>
where W:AsyncWrite+Unpin {
    let message=format!("{}\n",json!({"type":"heartbeat","leaseGeneration":generation}));
    loop {
        ticks.tick().await?;
        if !matches!(tokio::time::timeout(write_bound,stdin.write_all(message.as_bytes())).await,Ok(Ok(()))) {
            return Err(internal("Conductor heartbeat failed"));
        }
    }
}
async fn authority_monitor<F,Fut>(mut check:F,mut ticks:MonitorTicks)->ApiResult<()>
where F:FnMut()->Fut,Fut:Future<Output=ApiResult<()>> {
    loop {ticks.tick().await?;check().await?;}
}
async fn monitor_business<B,H,A,F,T>(business:B,heartbeat:H,authority:A,
    mut child_exited:F,lifetime:Duration)->ApiResult<T>
where B:Future<Output=ApiResult<T>>,H:Future<Output=ApiResult<()>>,
      A:Future<Output=ApiResult<()>>,F:FnMut()->ApiResult<bool> {
    tokio::pin!(business,heartbeat,authority);
    let lifetime=tokio::time::sleep(lifetime);tokio::pin!(lifetime);
    let mut heartbeat_open=true;
    let mut heartbeat_exit_grace=None;
    loop {
        // Keep business alive across monitor ticks. In particular, neither a
        // partial stdout frame nor an awaited report prefix is thrown away.
        let grace=tokio::time::sleep_until(heartbeat_exit_grace.unwrap_or_else(||tokio::time::Instant::now()+Duration::from_secs(24*3600)));
        tokio::pin!(grace);
        tokio::select!{biased;
            ended=&mut authority=>return match ended {
                Err(error)=>Err(error),Ok(())=>Err(internal("Conductor authority monitor ended")),
            },
            result=&mut business=>return result,
            _=&mut heartbeat,if heartbeat_open=>{
                heartbeat_open=false;
                // Natural Node completion closes stdin just before stdout/exit.
                // Allow only a bounded process-exit race, while authority and
                // business continue to be polled. A dead child needs no pulse.
                if !child_exited()? {heartbeat_exit_grace=Some(tokio::time::Instant::now()+Duration::from_secs(2));}
            },
            _=&mut grace,if heartbeat_exit_grace.is_some()=>{
                heartbeat_exit_grace=None;
                if !child_exited()? {return Err(internal("Conductor heartbeat failed"));}
            },
            _=&mut lifetime=>return Err(internal("Conductor lifetime exceeded; recover checkpoint")),
        }
    }
}
async fn supervise(app: &App, launch: Arc<conductor::Launch>) -> ApiResult<Disposition> {
    let ctx=conductor_authority::Context{run_id:launch.run_id.clone(),lease_generation:launch.lease_generation,actor:launch.actor.clone()};
    // Admit configuration before opening a listener or starting a process.
    conductor_authority::check_read(app,&ctx).await?;
    if launch.mode=="execute" {
        let job=app.db.read_job(&launch.run_id).await?.ok_or_else(||conflict("Conductor missing"))?;
        let metadata=app.db.read_metadata().await?;
        if job["connectorBinding"]!=launch.connection_binding{return Err(conflict("Conductor connection binding changed before process launch"));}
        if dispatch_authority::require_unheld(app).is_err()
            ||connection_gate::current_continuation_admission(&metadata)?.as_ref()!=Some(&launch.continuation_claim["acceptedAdmission"]) {
            // No RPC/process has started. Native readiness itself supplies a
            // dependency disposition, with no claim of an execution rejection.
            return conductor::connection_continuation::dependency_dto(app,&launch,Value::Null,true).await.map(Disposition::NativeDependency);
        }
    }
    let capability=format!("{}{}",uuid::Uuid::new_v4().simple(),uuid::Uuid::new_v4().simple());
    let server=conductor_http::start(app,launch.clone(),&capability).await?;
    // app.bridge is a server-configured release asset, never a request path.
    let script=app.bridge.parent().and_then(|p|p.parent()).ok_or_else(||internal("Conductor asset root unavailable"))?
        .join("cli/conductor.mjs");
    let mut command=Command::new(&app.node);
    command.arg(script).stdin(std::process::Stdio::piped()).stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null()).kill_on_drop(true);
    #[cfg(windows)] command.creation_flags(0x08000000);
    let mut child=command.spawn().map_err(|_|internal("Conductor runtime unavailable"))?;
    #[cfg(windows)] let tree=ProcessTree::attach(&child)?;
    let mut stdin=child.stdin.take().ok_or_else(||internal("Conductor input unavailable"))?;
    let stdout=child.stdout.take().ok_or_else(||internal("Conductor output unavailable"))?;
    let config=json!({"version":1,"runId":launch.run_id,"leaseGeneration":launch.lease_generation,
        "workspaceGeneration":launch.workspace_generation,
        "connectionBinding":launch.connection_binding,
        "account":launch.account,"baseUrl":launch.base_url,"rpcUrl":server.rpc_url,"capability":capability,
        "checkpointPath":launch.checkpoint_path,"scopeItemIds":launch.scope_item_ids,"mode":launch.mode,
        "maxRepairRounds":launch.max_repair_rounds,"maxRepairs":launch.max_repair_rounds,
        "maxCycles":launch.max_cycles,"batchSize":launch.batch_size,"cutoffUtc":launch.cutoff_utc,
        "resume":launch.resume,"continueHeld":true,"freshEditorial":true});
    let outcome=async {
        if !matches!(tokio::time::timeout(Duration::from_secs(10),stdin.write_all(format!("{config}\n").as_bytes())).await,Ok(Ok(()))) {
            return Err(internal("Conductor configuration failed"));
        }
        let heartbeat=heartbeat_monitor(&mut stdin,launch.lease_generation,MonitorTicks::interval(Duration::from_secs(5)),Duration::from_secs(2));
        let authority=authority_monitor(||conductor_authority::check_read(app,&ctx),MonitorTicks::interval(Duration::from_secs(5)));
        let business=async {
            let mut stdout=BufReader::new(stdout);
            let mut result=None;let mut total_bytes=0usize;
            loop {
                match frame(&mut stdout).await? {
                    Some(bytes)=>{
                        total_bytes+=bytes.len();
                        if total_bytes>64*1024*1024{return Err(internal("Conductor lifetime output exceeds bound"));}
                        let received=output(&bytes)?;
                        if result.is_some(){return Err(internal("Conductor output followed final disposition"));}
                        if received["type"]=="report" {
                            conductor::record_child_report(app,&launch,&received["report"]).await?;
                        } else if received["type"]=="dependency_wait"{result=Some(Disposition::Dependency(received["dependency"].clone()));}
                        else{result=Some(Disposition::Result(received["result"].clone()));}
                    },
                    None=>break,
                }
            }
            Ok(result)
        };
        let result=monitor_business(business,heartbeat,authority,
            ||child.try_wait().map(|status|status.is_some()).map_err(|_|internal("Conductor exit unavailable")),Duration::from_secs(24*3600)).await;
        // The select owns both monitor futures. On EOF, report errors, grant
        // denial, expiry or supervisor cancellation, neither can outlive it.
        drop(stdin);
        let result=result?;
        let status=tokio::time::timeout(Duration::from_secs(10),child.wait()).await
            .map_err(|_|internal("Conductor exit unresolved"))?.map_err(|_|internal("Conductor exit unavailable"))?;
        settle_disposition(status.success(),result)
    }.await;
    // Close the RPC before containment cleanup; no surviving child can admit.
    drop(server);
    if outcome.is_err(){let _=child.kill().await;}
    #[cfg(windows)] tree.stop_and_wait().await?;
    outcome
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test] async fn output_is_bounded_before_deserialization() {
        let data=vec![b'x';MAX_FRAME+1]; let mut reader=BufReader::new(data.as_slice());
        assert!(frame(&mut reader).await.is_err());
        let mut reader=BufReader::new(b"{\"type\":\"result\",\"result\":{}}\n".as_slice());
        assert_eq!(output(&frame(&mut reader).await.unwrap().unwrap()).unwrap()["type"],"result");
        assert!(frame(&mut reader).await.unwrap().is_none());
    }
    #[test] fn output_contract_rejects_raw_logs_and_capability_fields() {
        assert!(output(b"raw model output").is_err());
        assert!(output(br#"{"type":"report","report":{},"capability":"secret"}"#).is_err());
        assert!(output(br#"{"type":"result","result":{}}"#).is_ok());
    }
    #[test] fn only_closed_transport_failures_allow_checkpoint_recovery() {
        assert!(safe_transport_failure(&internal("Conductor ended without result; recover checkpoint")));
        assert!(!safe_transport_failure(&internal("Invalid conductor output")));
        assert!(!safe_transport_failure(&internal("Conductor lifetime exceeded; recover checkpoint")));
        assert!(!safe_transport_failure(&ApiError(StatusCode::FORBIDDEN,"Conductor heartbeat failed".into())));
    }
    #[test] fn declared_blocked_exit_is_preserved_without_transport_restart() {
        assert_eq!(settle_output(false,Some(json!({"mode":"blocked"}))).unwrap()["mode"],"blocked");
        assert!(safe_transport_failure(&settle_output(false,None).unwrap_err()));
        assert!(settle_output(false,Some(json!({"mode":"complete"}))).is_err());
        assert_eq!(settle_output(true,Some(json!({"mode":"complete"}))).unwrap()["mode"],"complete");
    }
    #[test] fn dependency_disposition_requires_exact_frame_and_normal_exit() {
        assert!(output(br#"{"type":"dependency_wait","dependency":{}}"#).is_ok());
        for bytes in [br#"{"type":"dependency_wait","result":{}}"#.as_slice(),
            br#"{"type":"dependency_wait","dependency":{},"result":{}}"#.as_slice()] {
            assert!(output(bytes).is_err());
        }
        assert!(matches!(settle_disposition(true,Some(Disposition::Dependency(json!({})))),Ok(Disposition::Dependency(_))));
        let failure=settle_disposition(false,Some(Disposition::Dependency(json!({})))).unwrap_err();
        assert!(!safe_transport_failure(&failure),"failed dependency exit cannot automatically relaunch");
    }

    struct DropFlag(Arc<std::sync::atomic::AtomicBool>);
    impl Drop for DropFlag {fn drop(&mut self){self.0.store(true,std::sync::atomic::Ordering::SeqCst);}}
    fn flag()->Arc<std::sync::atomic::AtomicBool>{Arc::new(std::sync::atomic::AtomicBool::new(false))}
    fn dropped(value:&Arc<std::sync::atomic::AtomicBool>)->bool{value.load(std::sync::atomic::Ordering::SeqCst)}

    #[tokio::test]
    async fn heartbeat_survives_blocked_report_and_authority_past_watchdog_then_denial_cancels_both() {
        // Manual clock advances exactly the production 5-second tick. Four
        // accepted pulses cover 20 logical seconds, beyond Node's 15-second
        // watchdog. No wall-clock scheduler timing or test-util feature needed.
        for reason in ["revoked","stale_epoch"] {
            let (reader,mut writer)=tokio::io::duplex(512);
            let (pulse,pulses)=tokio::sync::mpsc::channel(1);
            let (check,checks)=tokio::sync::mpsc::channel(1);
            let (revoke,revoked)=tokio::sync::oneshot::channel::<()>();
            let (entered,entry)=tokio::sync::oneshot::channel();
            let (_finish_report,report)=tokio::sync::oneshot::channel::<()>();
            let report_closed=flag();let read_closed=flag();
            let report_mark=report_closed.clone();let read_mark=read_closed.clone();
            let work=tokio::spawn(async move {
                let heartbeat=heartbeat_monitor(&mut writer,7,MonitorTicks::Manual(pulses),Duration::from_secs(2));
                let mut revoked=Some(revoked);let mut entered=Some(entered);
                let authority=authority_monitor(move ||{
                    let wait=revoked.take().unwrap();let entered=entered.take().unwrap();let marker=read_mark.clone();
                    async move {let _closed=DropFlag(marker);let _=entered.send(());let _=wait.await;
                        Err(ApiError(StatusCode::FORBIDDEN,reason.into()))}
                },MonitorTicks::Manual(checks));
                let business=async move {let _closed=DropFlag(report_mark);let _=report.await;Ok::<Value,ApiError>(json!({"mode":"complete"}))};
                let result=monitor_business(business,heartbeat,authority,||Ok(false),Duration::from_secs(60)).await;
                drop(writer);result
            });
            check.send(()).await.unwrap();entry.await.unwrap();
            let mut reader=BufReader::new(reader);
            for _ in 0..4 {
                pulse.send(()).await.unwrap();
                let bytes=tokio::time::timeout(Duration::from_secs(2),frame(&mut reader)).await.unwrap().unwrap().unwrap();
                let message:Value=serde_json::from_slice(&bytes).unwrap();
                assert_eq!(message,json!({"type":"heartbeat","leaseGeneration":7}));
            }
            assert!(!work.is_finished());assert!(!dropped(&report_closed));assert!(!dropped(&read_closed));
            revoke.send(()).unwrap();
            let error=tokio::time::timeout(Duration::from_secs(2),work).await.unwrap().unwrap().unwrap_err();
            assert_eq!(error.0,StatusCode::FORBIDDEN);assert_eq!(error.1,reason);
            assert!(dropped(&report_closed));assert!(dropped(&read_closed));
            assert!(tokio::time::timeout(Duration::from_secs(2),frame(&mut reader)).await.unwrap().unwrap().is_none());
            assert!(pulse.send(()).await.is_err());assert!(check.send(()).await.is_err());
        }
    }

    #[tokio::test]
    async fn business_eof_and_supervisor_abort_drop_pending_authority_and_close_stdin() {
        for abort in [false,true] {
            let (reader,mut writer)=tokio::io::duplex(512);
            let (pulse,pulses)=tokio::sync::mpsc::channel(1);
            let (check,checks)=tokio::sync::mpsc::channel(1);
            let (_allow_authority,authority_wait)=tokio::sync::oneshot::channel::<()>();
            let (entered,entry)=tokio::sync::oneshot::channel();
            let (finish,business_wait)=tokio::sync::oneshot::channel::<()>();
            let authority_closed=flag();let marker=authority_closed.clone();
            let work=tokio::spawn(async move {
                let heartbeat=heartbeat_monitor(&mut writer,3,MonitorTicks::Manual(pulses),Duration::from_secs(2));
                let mut wait=Some(authority_wait);let mut entered=Some(entered);
                let authority=authority_monitor(move ||{
                    let wait=wait.take().unwrap();let entered=entered.take().unwrap();let marker=marker.clone();
                    async move {let _closed=DropFlag(marker);let _=entered.send(());let _=wait.await;Ok(())}
                },MonitorTicks::Manual(checks));
                let business=async move {let _=business_wait.await;Ok::<Value,ApiError>(json!({"mode":"complete"}))};
                let result=monitor_business(business,heartbeat,authority,||Ok(false),Duration::from_secs(60)).await;
                drop(writer);result
            });
            check.send(()).await.unwrap();entry.await.unwrap();
            let mut reader=BufReader::new(reader);
            pulse.send(()).await.unwrap();tokio::time::timeout(Duration::from_secs(2),frame(&mut reader)).await.unwrap().unwrap().unwrap();
            if abort {work.abort();assert!(tokio::time::timeout(Duration::from_secs(2),work).await.unwrap().unwrap_err().is_cancelled());}
            else {finish.send(()).unwrap();assert_eq!(tokio::time::timeout(Duration::from_secs(2),work).await.unwrap().unwrap().unwrap()["mode"],"complete");}
            assert!(dropped(&authority_closed));
            assert!(tokio::time::timeout(Duration::from_secs(2),frame(&mut reader)).await.unwrap().unwrap().is_none());
            assert!(pulse.send(()).await.is_err());assert!(check.send(()).await.is_err());
        }
    }

    #[tokio::test]
    async fn bounded_heartbeat_write_failure_preserves_natural_exit_but_not_authority_denial() {
        // An already exited child may close stdin before its final stdout has
        // been recorded. A concurrent denied grant still wins that race.
        let (finish,finished)=tokio::sync::oneshot::channel();
        let (observed,observation)=tokio::sync::oneshot::channel();
        let mut observed=Some(observed);
        let work=tokio::spawn(async move {
            monitor_business(async move {let _=finished.await;Ok::<Value,ApiError>(json!({"mode":"complete"}))},
                async {Err(internal("Conductor heartbeat failed"))},std::future::pending::<ApiResult<()>>(),
                move ||{let _=observed.take().unwrap().send(());Ok(true)},Duration::from_secs(2)).await
        });
        tokio::time::timeout(Duration::from_secs(2),observation).await.unwrap().unwrap();
        assert!(!work.is_finished());finish.send(()).unwrap();
        let complete=tokio::time::timeout(Duration::from_secs(2),work).await.unwrap().unwrap().unwrap();
        assert_eq!(complete["mode"],"complete");
        let denied=monitor_business(async {Ok::<Value,ApiError>(json!({"mode":"complete"}))},
            async {Err(internal("Conductor heartbeat failed"))},async {Err(ApiError(StatusCode::FORBIDDEN,"revoked".into()))},||Ok(true),Duration::from_secs(1)).await.unwrap_err();
        assert_eq!(denied.0,StatusCode::FORBIDDEN);
        // A writable pipe with no reader is bounded by its write deadline.
        let (_reader,mut writer)=tokio::io::duplex(1);
        let (pulse,pulses)=tokio::sync::mpsc::channel(1);pulse.send(()).await.unwrap();
        let failure=tokio::time::timeout(Duration::from_secs(2),heartbeat_monitor(&mut writer,1,
            MonitorTicks::Manual(pulses),Duration::from_millis(20))).await.unwrap().unwrap_err();
        assert!(safe_transport_failure(&failure));
    }
}
