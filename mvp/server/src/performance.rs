//! Numeric timing only: no prompts, credentials, SQL or entity payloads.
//! Slow stages remain observable in normal operation; opt-in tracing includes all.
use std::{sync::OnceLock, time::Instant};

tokio::task_local! {
    static OPERATION_ID: Option<String>;
}

fn opaque_id(value: &str) -> Option<String> {
    (!value.is_empty() && value.len() <= 128
        && value.bytes().all(|b| b.is_ascii_alphanumeric() || b"-_:".contains(&b)))
        .then(|| value.to_owned())
}

/// Keep concurrent attempts separate without forwarding diagnostic fields to
/// the provider. This scope follows awaits, not unrelated spawned tasks.
pub(crate) async fn operation_scope<T>(operation_id: &str, work: impl std::future::Future<Output=T>) -> T {
    OPERATION_ID.scope(opaque_id(operation_id), work).await
}

/// Adapt the existing task-local operation scope at the transport boundary.
/// An incompatible already-bound operation is an observation gap, never a
/// silently retargeted trace or a reason to change the business request.
pub(crate) fn current_trace_context() -> Option<crate::trace_context::TraceContext> {
    let context = crate::trace_context::current()?;
    match OPERATION_ID.try_with(Clone::clone).ok().flatten() {
        Some(operation) => {
            let value = context.to_json();
            if value["ids"]["logicalOperationId"].as_str().is_some_and(|bound| bound != operation) {
                crate::trace_context::missing(&context, "trace.marker", "scope_changed");
                None
            } else { context.with_operation(&operation, None) }
        }
        None => Some(context),
    }
}

#[cfg(test)]
tokio::task_local! {
    static TEST_EVENTS: std::cell::RefCell<Vec<serde_json::Value>>;
}

#[cfg(test)]
pub(crate) async fn capture<T>(work: impl std::future::Future<Output=T>) -> (T, Vec<serde_json::Value>) {
    TEST_EVENTS.scope(std::cell::RefCell::new(Vec::new()), async {
        let result = work.await;
        let events = TEST_EVENTS.with(|events| events.take());
        (result, events)
    }).await
}

pub(crate) fn enabled() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| std::env::var("COMMUNITYHERO_PERF_TRACE").as_deref() == Ok("1"))
}

pub(crate) struct Span {
    stage: &'static str,
    start: Instant,
    job_id: Option<String>,
    operation_id: Option<String>,
    work: Option<(usize, usize, usize)>,
    writer_pool: Option<(bool,bool,usize,u32)>,
    causal: Option<CausalSpan>,
    measurements: Option<StorageMeasurements>,
}
#[derive(Clone,Copy)]
pub(crate) enum SpanClass {Container,Activity,Wait,Occupancy,Milestone}
impl SpanClass {fn name(self)->&'static str{match self{Self::Container=>"container",Self::Activity=>"activity",Self::Wait=>"wait",Self::Occupancy=>"occupancy",Self::Milestone=>"milestone"}}}
struct CausalSpan {context:crate::trace_context::TraceContext,span_id:String,start_ns:u128,class:SpanClass,links:Vec<serde_json::Value>,finished:bool,explicit:bool}
#[derive(Clone,Default)]
pub(crate) struct ReadMeasurements {pub(crate) rows:Option<u64>,pub(crate) bytes:Option<u64>,pub(crate) statements:Option<u64>}
impl ReadMeasurements {fn json(&self)->serde_json::Value{serde_json::json!({"rows":self.rows,"bytes":self.bytes,"statements":self.statements})}}
#[derive(Clone,Default)]
pub(crate) struct StorageMeasurements {
    pub(crate) payload_read:ReadMeasurements,pub(crate) header_read:ReadMeasurements,pub(crate) control_read:ReadMeasurements,pub(crate) discriminator_read:ReadMeasurements,pub(crate) payload_write:ReadMeasurements,
    pub(crate) collections:Option<u64>,pub(crate) changed_rows:Option<u64>,pub(crate) noop_rows:Option<u64>,pub(crate) closure_rows:Option<u64>,pub(crate) closure_bytes:Option<u64>,pub(crate) discovered_dependency_rows:Option<u64>,pub(crate) clone_bytes:Option<u64>,
    pub(crate) fallback_reason:Option<&'static str>,pub(crate) committed:Option<bool>,pub(crate) rollback_acknowledged:Option<bool>,pub(crate) connection_returned:Option<bool>,
}
impl StorageMeasurements {fn json(&self)->serde_json::Value {let mut v=serde_json::json!({"payloadRead":self.payload_read.json(),"headerRead":self.header_read.json(),"controlRead":self.control_read.json(),"discriminatorRead":self.discriminator_read.json(),"payloadWrite":self.payload_write.json(),"collections":self.collections,"changedRows":self.changed_rows,"noopRows":self.noop_rows,"closureRows":self.closure_rows,"closureBytes":self.closure_bytes,"discoveredDependencyRows":self.discovered_dependency_rows,"cloneBytes":self.clone_bytes,"fallbackReason":self.fallback_reason,"committed":self.committed,"rollbackAcknowledged":self.rollback_acknowledged,"connectionReturned":self.connection_returned});if self.clone_bytes.is_some(){v["cloneMeasurementClass"]="derived".into();v["cloneBasis"]="logical_serialized_size".into();}v}}
impl Span {
    pub(crate) fn new(stage: &'static str) -> Self {
        Self::create(stage,None)
    }
    fn create(stage:&'static str,job:Option<&str>)->Self {
        let operation_id=OPERATION_ID.try_with(Clone::clone).ok().flatten();
        let mut context=Self::current_context();
        if let (Some(ctx),Some(job))=(&context,job){context=ctx.with_job(job);}
        let mut span=Self {stage,start:Instant::now(),job_id:job.and_then(opaque_id),operation_id,work:None,writer_pool:None,causal:None,measurements:None};
        if let Some(ctx)=context {span.begin_causal(&ctx,if stage.ends_with(".total"){SpanClass::Container}else if stage.ends_with(".wait")||stage.ends_with("_wait"){SpanClass::Wait}else if stage.ends_with(".held"){SpanClass::Occupancy}else{SpanClass::Activity},false);}
        span
    }
    fn current_context()->Option<crate::trace_context::TraceContext>{current_trace_context()}
    pub(crate) fn start(stage:&'static str,class:SpanClass,context:&crate::trace_context::TraceContext)->Self {
        let mut span=Self {stage,start:Instant::now(),job_id:context.to_json()["ids"]["jobId"].as_str().and_then(opaque_id),operation_id:context.to_json()["ids"]["logicalOperationId"].as_str().and_then(opaque_id),work:None,writer_pool:None,causal:None,measurements:None};span.begin_causal(context,class,true);span
    }
    fn begin_causal(&mut self,context:&crate::trace_context::TraceContext,class:SpanClass,explicit:bool) {
        if !crate::trace_context::allowed("stages",self.stage){crate::trace_context::missing(context,"trace.legacy","legacy_unlinked");return;}
        let span_id=uuid::Uuid::new_v4().to_string();let start_ns=crate::trace_context::now_ns();let mut e=context.event("span_start",self.stage,class.name());e["spanId"]=span_id.clone().into();e["monoNs"]=start_ns.to_string().into();crate::trace_context::emit(context,e);
        self.causal=Some(CausalSpan{context:context.clone(),span_id,start_ns,class,links:Vec::new(),finished:false,explicit});
    }
    pub(crate) fn context(&self)->Option<crate::trace_context::TraceContext>{self.causal.as_ref().and_then(|c|c.context.with_parent(&c.span_id))}
    pub(crate) fn child(&self,stage:&'static str,class:SpanClass)->Self {match self.context(){Some(c)=>Self::start(stage,class,&c),None=>Self::new(stage)}}
    pub(crate) async fn scope<T>(&self,work:impl std::future::Future<Output=T>)->T {match self.context(){Some(c)=>crate::trace_context::scope(c,work).await,None=>work.await}}
    pub(crate) fn link(&mut self,kind:&str,trace_id:&str,span_id:Option<&str>,durable_id:Option<&str>){
        if !crate::trace_context::allowed("linkTypes",kind)||!crate::trace_context::opaque(trace_id)||span_id.is_none()&&durable_id.is_none()||span_id.is_some_and(|v|!crate::trace_context::opaque(v))||durable_id.is_some_and(|v|!crate::trace_context::opaque(v)){return;}
        if let Some(c)=&mut self.causal {if c.links.len()<32{let mut link=serde_json::json!({"type":kind,"traceId":trace_id});if let Some(s)=span_id{link["spanId"]=s.into();}if let Some(d)=durable_id{link["durableId"]=d.into();}c.links.push(link);}}
    }
    pub(crate) fn measurements(&mut self,measurements:StorageMeasurements){self.measurements=Some(measurements);}
    pub(crate) fn finish(&mut self,outcome:&str,reason:Option<&str>) {
        let Some(c)=&mut self.causal else{return;};if c.finished{return;}c.finished=true;
        let valid=crate::trace_context::allowed("outcomes",outcome)&&reason.is_none_or(|r|crate::trace_context::allowed("reasons",r));
        let end=crate::trace_context::now_ns();let mut e=c.context.event("span_end",self.stage,c.class.name());e["spanId"]=c.span_id.clone().into();e["startNs"]=c.start_ns.to_string().into();e["endNs"]=end.to_string().into();e["monoNs"]=end.to_string().into();e["elapsedNs"]=end.saturating_sub(c.start_ns).to_string().into();e["outcome"]=if valid{outcome}else{"unresolved"}.into();e["links"]=serde_json::json!(c.links);
        if let Some(r)=reason.filter(|r|crate::trace_context::allowed("reasons",r)){e["reasonCode"]=r.into();}
        if !valid{e["absenceReason"]="not_measured".into();}
        let mut counters=self.measurements.as_ref().map(StorageMeasurements::json).unwrap_or_else(||serde_json::json!({}));
        if let Some((rows,bytes,collections))=self.work{counters["rows"]=rows.into();counters["bytes"]=bytes.into();counters["collections"]=collections.into();}
        if let Some((committed,rollback,num_idle,size))=self.writer_pool{counters["committed"]=committed.into();counters["rollbackAcknowledged"]=rollback.into();counters["connectionReturned"]=true.into();counters["numIdle"]=num_idle.into();counters["poolSize"]=size.into();}
        e["measurements"]=counters;crate::trace_context::emit(&c.context,e);
    }
    /// Only opaque internal job identifiers, never request bodies or model text.
    pub(crate) fn job(stage: &'static str, job_id: &str) -> Self {
        Self::create(stage,Some(job_id))
    }
    /// Numeric work only. Callers count transferred rows/bytes/collections;
    /// this never logs identifiers, SQL, prompts or JSON field contents.
    pub(crate) fn counts(&mut self, rows: usize, bytes: usize, collections: usize) {
        self.work = Some((rows, bytes, collections));
    }
    /// Numeric acknowledged completion and advisory pool snapshot only.
    pub(crate) fn writer_pool_state(&mut self,committed:bool,rollback_acknowledged:bool,num_idle:usize,size:u32) {
        self.writer_pool=Some((committed,rollback_acknowledged,num_idle,size));
    }
    fn event(&self, elapsed_ms: f64) -> serde_json::Value {
        let mut event = serde_json::json!({"stage":self.stage,"elapsedMs":elapsed_ms,
            "timestamp":chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis,true),
            "processId":std::process::id()});
        if let Some(id)=&self.job_id {event["jobId"]=id.clone().into();}
        if let Some(id)=&self.operation_id {event["operationId"]=id.clone().into();}
        if let Some((rows,bytes,collections))=self.work {
            event["work"]=serde_json::json!({"rows":rows,"bytes":bytes,"collections":collections});
        }
        if let Some((committed,rollback_acknowledged,num_idle,size))=self.writer_pool {
            event["writerPool"]=serde_json::json!({"committed":u8::from(committed),"returned":1,
                "rollbackAcknowledged":u8::from(rollback_acknowledged),"numIdle":num_idle,"size":size});
        }
        event
    }
}
impl Drop for Span {
    fn drop(&mut self) {
        let explicit=self.causal.as_ref().is_some_and(|c|c.explicit);
        if self.causal.as_ref().is_some_and(|c|!c.finished){self.finish(if explicit{"unresolved"}else{"completed"},if explicit{Some("unfinished_span")}else{None});}
        let elapsed_ms = self.start.elapsed().as_secs_f64()*1000.0;
        #[cfg(test)]
        let _ = TEST_EVENTS.try_with(|events| events.borrow_mut().push(self.event(elapsed_ms)));
        if enabled() || elapsed_ms >= 250.0 {
            let event = self.event(elapsed_ms);
            crate::trace_context::emit_legacy(&event);
        }
    }
}

#[cfg(test)]
#[path="causal_trace_tests.rs"]
mod causal_trace_tests;

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn concurrent_operations_do_not_share_correlation() {
        let one=operation_scope("op-1",async {tokio::task::yield_now().await;Span::job("fixture.stage","job-1").event(1.0)});
        let two=operation_scope("op-2",async {tokio::task::yield_now().await;Span::new("fixture.stage").event(2.0)});
        let (one,two)=tokio::join!(one,two);
        assert_eq!(one["operationId"],"op-1");
        assert_eq!(one["jobId"],"job-1");
        assert_eq!(two["operationId"],"op-2");
        assert!(two.get("jobId").is_none());
        assert!(Span::new("fixture.stage").event(0.0).get("operationId").is_none());
        assert!(chrono::DateTime::parse_from_rfc3339(one["timestamp"].as_str().unwrap()).is_ok());
        assert_eq!(one["processId"],std::process::id());
    }

    #[tokio::test]
    async fn invalid_identifiers_are_not_logged_and_nested_scope_restores_parent() {
        assert!(opaque_id(&"x".repeat(129)).is_none());
        operation_scope("outer",async {
            let inner=operation_scope("secret\nvalue",async {Span::job("fixture.stage","untrusted\ntext").event(0.0)}).await;
            assert!(inner.get("operationId").is_none());
            assert!(inner.get("jobId").is_none());
            assert!(!inner.to_string().contains("secret"));
            assert_eq!(Span::new("fixture.stage").event(0.0)["operationId"],"outer");
        }).await;
    }
}

// R3 fixture telemetry: successful application statements only, excluding
// SQLx transaction protocol and connection initialization. Compiles out of releases.
#[cfg(test)]
pub(crate) fn r3_sql_read() { drop(Span::new("r3.sql.read")); }
#[cfg(test)]
pub(crate) fn r3_sql_write() { drop(Span::new("r3.sql.write")); }
