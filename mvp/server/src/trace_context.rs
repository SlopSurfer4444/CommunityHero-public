//! Closed metadata only. Correlation never grants publication or retry authority.
use serde_json::{Value,json};
use std::{future::Future,sync::{Arc,OnceLock,atomic::{AtomicBool,AtomicU64,Ordering},mpsc},time::Instant};
pub(crate) const MAX_EVENTS:usize=256;
pub(crate) const MAX_ENVELOPE_BYTES:usize=262144;
const MAX_SAFE:u64=9_007_199_254_740_991;
const SCHEMA:&str=include_str!("../../cli/trace-envelope-v1.schema.json");
fn contract()->&'static Value {static C:OnceLock<Value>=OnceLock::new();C.get_or_init(||serde_json::from_str(SCHEMA).expect("compiled trace contract"))}
pub(crate) fn allowed(kind:&str,value:&str)->bool {contract()[format!("x-{kind}")].as_array().is_some_and(|a|a.iter().any(|v|v.as_str()==Some(value)))}
pub(crate) fn opaque(value:&str)->bool {!value.is_empty()&&value.len()<=128&&value.bytes().all(|b|b.is_ascii_alphanumeric()||b"-_:".contains(&b))}
fn digest(value:&str)->bool {value.len()==64&&value.bytes().all(|b|b.is_ascii_digit()||(b'a'..=b'f').contains(&b))}
fn ns(v:&Value)->bool {v.as_str().is_some_and(|s|s=="0"||!s.is_empty()&&s.len()<=30&&!s.starts_with('0')&&s.bytes().all(|b|b.is_ascii_digit()))}
fn count(v:&Value)->bool {v.as_u64().is_some_and(|n|n<=MAX_SAFE)}
fn keys(v:&Value,names:&[&str])->bool {v.as_object().is_some_and(|o|o.keys().all(|k|names.contains(&k.as_str())))}
fn optional(v:Option<&Value>,predicate:impl Fn(&Value)->bool)->bool {v.is_none_or(|v|v.is_null()||predicate(v))}
fn ids(v:Option<&Value>)->bool {v.is_none_or(|v|keys(v,&["jobId","logicalOperationId","dispatchAttemptId","readbackAttemptId","paidAttemptId","proposalId","approvalId","groupId","localRequestDigest","proposalRevision"])
    &&v.as_object().unwrap().iter().all(|(k,v)|v.is_null()||if k=="proposalRevision"{count(v)}else{v.as_str().is_some_and(|s|if k=="localRequestDigest"{digest(s)}else{opaque(s)})}))}
fn lineage(v:Option<&Value>)->bool {v.is_none_or(|v|keys(v,&["contextVersion","groupVersion","bindingDigest","groupScopeDigest","lineageManifestDigest","bundleDigest","rulesDigest","mediaDigest","binarySha256","coreSha256"])
    &&v.as_object().unwrap().iter().all(|(k,v)|v.is_null()||v.as_str().is_some_and(|s|if matches!(k.as_str(),"contextVersion"|"groupVersion"){opaque(s)}else{digest(s)})))}
pub(crate) fn valid_context(v:&Value)->bool {keys(v,&["version","traceId","companyKey","runtimeId","runtimeEpoch","sourcePin","parentSpanId","ids","lineage"])
    &&v["version"]==1&&matches!(v["companyKey"].as_str(),Some("baw-russia"|"likeavto"))&&["traceId","runtimeId"].iter().all(|k|v[*k].as_str().is_some_and(opaque))
    &&count(&v["runtimeEpoch"])&&v["sourcePin"].as_str().is_some_and(digest)&&optional(v.get("parentSpanId"),|v|v.as_str().is_some_and(opaque))&&ids(v.get("ids"))&&lineage(v.get("lineage"))&&v.to_string().len()<=4096}
#[derive(Clone,Debug)]
pub(crate) struct TraceContext {value:Value,anchor:Arc<AtomicBool>,
    #[cfg(test)] sink:Option<Arc<std::sync::Mutex<Vec<Value>>>>,
}
impl TraceContext {
    pub(crate) fn from_json(v:&Value)->Option<Self> {valid_context(v).then(||Self{value:v.clone(),anchor:Default::default(),#[cfg(test)]sink:None})}
    pub(crate) fn root(company_key:&str,runtime_id:&str,runtime_epoch:u64,source_pin:&str)->Option<Self> {
        Self::from_json(&json!({"version":1,"traceId":uuid::Uuid::new_v4().to_string(),"companyKey":company_key,"runtimeId":runtime_id,"runtimeEpoch":runtime_epoch,"sourcePin":source_pin,"ids":{},"lineage":{}}))
    }
    pub(crate) fn to_json(&self)->Value {self.value.clone()}
    pub(crate) fn with_parent(&self,id:&str)->Option<Self> {if !opaque(id){return None;}let mut c=self.clone();c.value["parentSpanId"]=id.into();Some(c)}
    pub(crate) fn with_job(&self,id:&str)->Option<Self> {self.with_id("jobId",id)}
    pub(crate) fn with_id(&self,key:&str,id:&str)->Option<Self> {let mut c=self.clone();if !c.value["ids"].is_object(){c.value["ids"]=json!({});}c.value["ids"][key]=id.into();valid_context(&c.value).then_some(c)}
    pub(crate) fn with_lineage(&self,key:&str,id:&str)->Option<Self> {let mut c=self.clone();if !c.value["lineage"].is_object(){c.value["lineage"]=json!({});}c.value["lineage"][key]=id.into();valid_context(&c.value).then_some(c)}
    pub(crate) fn with_operation(&self,id:&str,attempt:Option<&str>)->Option<Self> {let c=self.with_id("logicalOperationId",id)?;match attempt{Some(a)=>c.with_id("dispatchAttemptId",a),None=>Some(c)}}
    pub(crate) fn event(&self,event_type:&str,stage:&str,class:&str)->Value {
        let mut v=self.to_json();v["eventId"]=uuid::Uuid::new_v4().to_string().into();v["eventType"]=event_type.into();v["stage"]=stage.into();v["spanClass"]=class.into();v["measurementClass"]="measured".into();
        v["processClockId"]=clock().1.clone().into();v["processId"]=std::process::id().into();v["monoNs"]=now_ns().to_string().into();v["wallUtc"]=chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis,true).into();v["links"]=json!([]);v
    }
}
tokio::task_local! {static CURRENT:TraceContext;}
pub(crate) fn current()->Option<TraceContext> {CURRENT.try_with(Clone::clone).ok()}
pub(crate) async fn scope<T>(mut context:TraceContext,work:impl Future<Output=T>)->T {
    #[cfg(test)] {if context.sink.is_none(){context.sink=CAPTURE.try_with(Clone::clone).ok();}}
    if !context.anchor.swap(true,Ordering::Relaxed){emit(&context,context.event("clock_anchor","trace.clock_anchor","milestone"));}
    CURRENT.scope(context,work).await
}
fn clock()->&'static (Instant,String) {static CLOCK:OnceLock<(Instant,String)>=OnceLock::new();CLOCK.get_or_init(||(Instant::now(),uuid::Uuid::new_v4().to_string()))}
pub(crate) fn now_ns()->u128 {clock().0.elapsed().as_nanos()}
pub(crate) fn measurements_valid(v:&Value)->bool {
    let Some(o)=v.as_object()else{return false;};o.iter().all(|(k,v)| {
        if matches!(k.as_str(),"payloadRead"|"headerRead"|"controlRead"|"discriminatorRead"|"payloadWrite") {keys(v,&["rows","bytes","statements"])&&v.as_object().unwrap().values().all(|v|v.is_null()||count(v))}
        else if allowed("numericMeasurements",k) {v.is_null()||count(v)}
        else if matches!(k.as_str(),"committed"|"rollbackAcknowledged"|"connectionReturned"|"providerCallAttempted") {v.is_null()||v.is_boolean()}
        else if k=="fallbackReason" {v.is_null()||v.as_str().is_some_and(|s|allowed("reasons",s))}
        else if k=="cloneMeasurementClass" {v=="derived"}else if k=="cloneBasis" {v=="logical_serialized_size"}else{false}
    })&&(v["cloneBytes"].is_null()||(v["cloneMeasurementClass"]=="derived"&&v["cloneBasis"]=="logical_serialized_size"))
}
pub(crate) fn valid_event(v:&Value)->bool {
    if !keys(v,&["version","eventId","eventType","traceId","companyKey","runtimeId","runtimeEpoch","sourcePin","processClockId","processId","monoNs","wallUtc","spanId","parentSpanId","stage","spanClass","startNs","endNs","elapsedNs","links","ids","lineage","measurements","outcome","reasonCode","measurementClass","absenceReason","droppedEventCount","clockUncertaintyNs"]){return false;}
    let context=json!({"version":v["version"],"traceId":v["traceId"],"companyKey":v["companyKey"],"runtimeId":v["runtimeId"],"runtimeEpoch":v["runtimeEpoch"],"sourcePin":v["sourcePin"],"ids":v.get("ids").cloned().unwrap_or(json!({})),"lineage":v.get("lineage").cloned().unwrap_or(json!({}))});
    if !valid_context(&context)||!["eventId","processClockId"].iter().all(|k|v[*k].as_str().is_some_and(opaque))||!count(&v["processId"])||!ns(&v["monoNs"])||v["wallUtc"].as_str().is_none_or(|s|s.len()!=24||!s.ends_with('Z')||chrono::DateTime::parse_from_rfc3339(s).is_err())||!matches!(v["measurementClass"].as_str(),Some("measured"|"derived"|"historical"|"unknown")){return false;}
    for (key,kind) in [("eventType","eventTypes"),("stage","stages"),("spanClass","spanClasses")] {if !v[key].as_str().is_some_and(|s|allowed(kind,s)){return false;}}
    if !["spanId","parentSpanId"].iter().all(|k|optional(v.get(*k),|v|v.as_str().is_some_and(opaque)))||!["startNs","endNs","elapsedNs","clockUncertaintyNs"].iter().all(|k|optional(v.get(*k),ns)){return false;}
    for (key,kind) in [("outcome","outcomes"),("reasonCode","reasons"),("absenceReason","reasons")] {if !optional(v.get(key),|v|v.as_str().is_some_and(|s|allowed(kind,s))){return false;}}
    if !optional(v.get("droppedEventCount"),count)||v.get("measurements").is_some_and(|v|!measurements_valid(v)){return false;}
    if let Some(links)=v.get("links") {let Some(links)=links.as_array()else{return false;};if links.len()>32||links.iter().any(|l|!keys(l,&["type","traceId","spanId","durableId"])||!l["type"].as_str().is_some_and(|s|allowed("linkTypes",s))||!l["traceId"].as_str().is_some_and(opaque)||!["spanId","durableId"].iter().all(|k|optional(l.get(*k),|v|v.as_str().is_some_and(opaque)))||(l["spanId"].is_null()&&l["durableId"].is_null())){return false;}}
    if matches!(v["eventType"].as_str(),Some("span_start"|"span_end"))&&!v["spanId"].as_str().is_some_and(opaque){return false;}
    if v["eventType"]=="span_end"&&(!["startNs","endNs","elapsedNs"].iter().all(|k|ns(&v[*k]))||!v["outcome"].as_str().is_some_and(|s|allowed("outcomes",s))){return false;}
    if v["eventType"]=="span_end" {let start=v["startNs"].as_str().unwrap().parse::<u128>().unwrap();let end=v["endNs"].as_str().unwrap().parse::<u128>().unwrap();let elapsed=v["elapsedNs"].as_str().unwrap().parse::<u128>().unwrap();if end<start||end-start!=elapsed||v["monoNs"]!=v["endNs"]{return false;}}
    v.to_string().len()<=8192
}
pub(crate) fn same_binding(a:&Value,b:&Value)->bool { ["traceId","companyKey","runtimeId","runtimeEpoch","sourcePin"].iter().all(|k|a[*k]==b[*k])
    &&["ids","lineage"].iter().all(|k|b[*k].as_object().is_none_or(|m|m.iter().all(|(field,v)|v.is_null()||a[*k][field]==*v)))}
pub(crate) fn accept_telemetry(v:&Value,expected:&TraceContext)->bool {
    let good=keys(v,&["version","context","events","droppedEventCount","complete"])&&v["version"]==1&&valid_context(&v["context"])&&same_binding(&v["context"],&expected.value)&&v["context"]["parentSpanId"]==expected.value["parentSpanId"]&&count(&v["droppedEventCount"])&&v["complete"].is_boolean()&&v.to_string().len()<=MAX_ENVELOPE_BYTES
        &&v["events"].as_array().is_some_and(|events|events.len()<=MAX_EVENTS&&events.iter().all(|e|valid_event(e)&&same_binding(e,&v["context"])&&(e["parentSpanId"].is_null()||e["parentSpanId"]!=e["spanId"])));
    if !good{missing(expected,"trace.marker","invalid_telemetry");return false;}
    for event in v["events"].as_array().unwrap(){emit(expected,event.clone());}
    if v["complete"]!=true||v["droppedEventCount"]!=0 {let mut event=expected.event("missing","trace.sink","milestone");event["absenceReason"]="missing_js_completion".into();event["measurementClass"]="unknown".into();event["droppedEventCount"]=v["droppedEventCount"].clone();emit(expected,event);}
    true
}
pub(crate) fn missing(c:&TraceContext,stage:&str,reason:&str){let mut e=c.event("missing",stage,"milestone");e["absenceReason"]=reason.into();e["measurementClass"]="unknown".into();emit(c,e);}
static DROPPED:AtomicU64=AtomicU64::new(0);
fn queue(line:String) {static SINK:OnceLock<Option<mpsc::SyncSender<String>>>=OnceLock::new();let sink=SINK.get_or_init(||{let (send,recv)=mpsc::sync_channel::<String>(512);std::thread::Builder::new().name("trace-output".into()).spawn(move||{for line in recv{use std::io::Write;if writeln!(std::io::stderr().lock(),"{line}").is_err(){DROPPED.fetch_add(1,Ordering::Relaxed);}}}).ok().map(|_|send)});if sink.as_ref().is_none_or(|s|s.try_send(line).is_err()){DROPPED.fetch_add(1,Ordering::Relaxed);}}
pub(crate) fn emit(c:&TraceContext,mut e:Value) {
    if !valid_event(&e){DROPPED.fetch_add(1,Ordering::Relaxed);return;}
    let dropped=DROPPED.load(Ordering::Relaxed);if dropped>0{e["droppedEventCount"]=dropped.into();}
    #[cfg(test)] if let Some(sink)=&c.sink {if let Ok(mut events)=sink.lock(){if events.len()<4096{events.push(e);} }return;}
    if crate::performance::enabled(){queue(format!("causal_trace {e}"));}
}
pub(crate) fn emit_legacy(e:&Value){queue(format!("performance {e}"));}
#[cfg(test)] tokio::task_local! {static CAPTURE:Arc<std::sync::Mutex<Vec<Value>>>;}
#[cfg(test)] pub(crate) async fn capture<T>(work:impl Future<Output=T>)->(T,Vec<Value>){let events=Arc::new(std::sync::Mutex::new(Vec::new()));CAPTURE.scope(events.clone(),async move{let result=work.await;let collected=events.lock().unwrap().clone();(result,collected)}).await}
