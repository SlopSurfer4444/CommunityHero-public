//! Connected native timing/correlation tests. Root owns the native execution queue.
use super::*;
use crate::trace_context::{self,TraceContext};
use serde_json::{Value,json};
fn context(job:&str)->TraceContext{TraceContext::root("baw-russia","fixture-runtime",7,&"a".repeat(64)).unwrap().with_job(job).unwrap()}
fn ends(events:&[Value])->Vec<&Value>{events.iter().filter(|e|e["eventType"]=="span_end").collect()}

#[tokio::test]
async fn explicit_parent_survives_await_and_spawn_without_a_global_stack(){
    let (_,events)=trace_context::capture(trace_context::scope(context("job-one"),async{
        let ctx=trace_context::current().unwrap();let mut parent=Span::start("fixture.parent",SpanClass::Container,&ctx);
        let child_ctx=parent.context().unwrap();
        let task=tokio::spawn(trace_context::scope(child_ctx,async{tokio::task::yield_now().await;let mut span=Span::new("fixture.child");span.finish("completed",None);}));
        tokio::task::yield_now().await;task.await.unwrap();parent.finish("completed",None);
    })).await;
    assert!(events.iter().all(trace_context::valid_event));
    let done=ends(&events);assert_eq!(done.len(),2);
    assert_eq!(done[0]["parentSpanId"],done[1]["spanId"]);
    assert!(done.iter().all(|e|e["ids"]["jobId"]=="job-one"));
    assert!(done[0]["endNs"].as_str().unwrap().parse::<u128>().unwrap()<=done[1]["endNs"].as_str().unwrap().parse::<u128>().unwrap());
    assert!(trace_context::current().is_none());
}
#[tokio::test]
async fn interleaved_operations_have_disjoint_context_and_terminal_ids(){
    let one=context("one").with_operation("op-one",Some("attempt-one")).unwrap();
    let two=context("two").with_operation("op-two",Some("attempt-two")).unwrap();
    let (_,events)=trace_context::capture(async{
        tokio::join!(trace_context::scope(one,async{tokio::task::yield_now().await;let mut s=Span::new("fixture.stage");s.finish("failed",Some("query_failed"));}),trace_context::scope(two,async{let mut s=Span::new("fixture.stage");tokio::task::yield_now().await;s.finish("completed",None);}));
    }).await;
    let done=ends(&events);assert_eq!(done.len(),2);
    for e in done{let id=e["ids"]["jobId"].as_str().unwrap();assert_eq!(e["ids"]["logicalOperationId"],format!("op-{id}"));assert_eq!(e["ids"]["dispatchAttemptId"],format!("attempt-{id}"));}
    assert_ne!(events.iter().find(|e|e["ids"]["jobId"]=="one").unwrap()["traceId"],events.iter().find(|e|e["ids"]["jobId"]=="two").unwrap()["traceId"]);
}
#[tokio::test]
async fn dropped_explicit_span_is_unresolved_and_finish_is_idempotent(){
    let (_,events)=trace_context::capture(trace_context::scope(context("cancel"),async{
        let ctx=trace_context::current().unwrap();let mut completed=Span::start("fixture.stage",SpanClass::Activity,&ctx);completed.finish("completed",None);completed.finish("failed",None);
        let _unfinished=Span::start("fixture.wait",SpanClass::Wait,&ctx);
    })).await;
    let done=ends(&events);assert_eq!(done.len(),2);assert_eq!(done[0]["outcome"],"completed");assert_eq!(done[1]["outcome"],"unresolved");assert_eq!(done[1]["reasonCode"],"unfinished_span");
}
#[tokio::test]
async fn measurements_preserve_unknown_and_logical_clone_basis(){
    let (_,events)=trace_context::capture(trace_context::scope(context("counts"),async{
        let mut span=Span::new("fixture.stage");span.measurements(StorageMeasurements{payload_read:ReadMeasurements{rows:Some(2),bytes:Some(100),statements:Some(1)},header_read:ReadMeasurements{rows:Some(7),..Default::default()},clone_bytes:Some(100),..Default::default()});span.finish("completed",None);
    })).await;
    let done=ends(&events);let m=&done[0]["measurements"];assert_eq!(m["payloadRead"]["rows"],2);assert!(m["headerRead"]["bytes"].is_null());assert!(m["closureRows"].is_null());assert_eq!(m["cloneMeasurementClass"],"derived");assert_eq!(m["cloneBasis"],"logical_serialized_size");
}
#[tokio::test]
async fn foreign_or_raw_telemetry_is_atomically_rejected_without_changing_work(){
    let (returned,events)=trace_context::capture(trace_context::scope(context("wire"),async{
        let ctx=trace_context::current().unwrap();let event=ctx.event("marker","provider.receive","milestone");let mut envelope=json!({"version":1,"context":ctx.to_json(),"events":[event],"droppedEventCount":0,"complete":true});
        assert!(trace_context::accept_telemetry(&envelope,&ctx));envelope["events"][0]["rawComment"]="secret".into();assert!(!trace_context::accept_telemetry(&envelope,&ctx));envelope["events"][0].as_object_mut().unwrap().remove("rawComment");envelope["context"]["companyKey"]="likeavto".into();assert!(!trace_context::accept_telemetry(&envelope,&ctx));17
    })).await;
    assert_eq!(returned,17);assert_eq!(events.iter().filter(|e|e["stage"]=="provider.receive").count(),1);assert_eq!(events.iter().filter(|e|e["absenceReason"]=="invalid_telemetry").count(),2);assert!(!json!(events).to_string().contains("secret"));
}
#[test]
fn closed_contract_rejects_unknown_counts_invalid_context_and_raw_payload(){
    let ctx=context("check");let mut event=ctx.event("marker","fixture.stage","milestone");assert!(trace_context::valid_event(&event));event["measurements"]=json!({"inputTokens":null});assert!(trace_context::valid_event(&event));event["measurements"]=json!({"secret":1});assert!(!trace_context::valid_event(&event));
    assert!(TraceContext::root("baw-russia","bad\nsecret",1,&"a".repeat(64)).is_none());assert!(ctx.with_id("targetText","raw text").is_none());
    event=ctx.event("span_end","fixture.stage","activity");event["spanId"]="span-one".into();event["startNs"]="1".into();event["endNs"]="3".into();event["monoNs"]="3".into();event["elapsedNs"]="2".into();event["outcome"]="completed".into();assert!(trace_context::valid_event(&event));event["elapsedNs"]="3".into();assert!(!trace_context::valid_event(&event));
}

#[tokio::test]
async fn transport_operation_adapter_preserves_binding_and_nested_scope_restoration(){
    let (_,events)=trace_context::capture(trace_context::scope(context("legacy-bridge"),async{
        operation_scope("actual-operation",async{
            let adapted=current_trace_context().unwrap();assert_eq!(adapted.to_json()["ids"]["logicalOperationId"],"actual-operation");
            trace_context::scope(adapted,async{
                assert!(operation_scope("different-operation",async{current_trace_context()}).await.is_none());
                assert_eq!(current_trace_context().unwrap().to_json()["ids"]["logicalOperationId"],"actual-operation");
            }).await;
        }).await;
        assert!(current_trace_context().unwrap().to_json()["ids"]["logicalOperationId"].is_null());
    })).await;
    assert!(events.iter().any(|event|event["absenceReason"]=="scope_changed"));
    assert!(events.iter().all(|event|event["ids"]["logicalOperationId"]!="different-operation"));
}
