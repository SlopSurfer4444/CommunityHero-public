//! Connected native lifetime entry tests; fake native tickets do not establish
//! real descendant cessation. Actual child containment is tested by its owner.
use super::*;
use analysis_output::{AudioLifecycle,LifecycleFuture};
use crate::{media_analysis as ledger,runtime_owned_work::{self,Registry,Kind}};
use std::sync::{Arc,Mutex,atomic::{AtomicUsize,Ordering}};
struct Lifecycle {ledger:Mutex<Value>,ocr_calls:AtomicUsize}
impl Lifecycle{fn new()->Self{Self{ledger:Mutex::new(Value::Null),ocr_calls:AtomicUsize::new(0)}}}
impl AudioLifecycle for Lifecycle {
    fn binding(&self)->Value{json!({"companyId":"fixture-company"})}
    fn reserve(&self,request:Value)->LifecycleFuture<'_,Value>{Box::pin(async move{ledger::reserve(&mut self.ledger.lock().unwrap(),&request)})}
    fn event(&self,event:&'static str,request:Value)->LifecycleFuture<'_,()>{Box::pin(async move{
        if event=="permit_ocr"{self.ocr_calls.fetch_add(1,Ordering::SeqCst);return Ok(());}
        let mut db=self.ledger.lock().unwrap();let result=match event{
            "mark_dispatched"=>ledger::mark_dispatched(&mut db,&request),"commit_segment"=>ledger::commit_segment(&mut db,&request),
            "commit_full_result"=>ledger::commit_full_result(&mut db,&request),"fail"=>ledger::fail(&mut db,&request),_=>Err("fixture_event_invalid".into())
        }?;
        if result["disposition"]=="already_dispatched"{return Err("fixture_no_paid_replay".into());}Ok(())
    })}
}
fn request()->Value{json!({"companyId":"fixture-company","verifiedFile":{"sha256":"a".repeat(64),"bytes":123,"receiptSha256":"b".repeat(64),"probeSha256":"c".repeat(64)},
    "stage":"asr","attemptId":"attempt","owner":"worker","epoch":1,"manifestKey":"manifest","specSha256":"d".repeat(64),"durationMs":1000,
    "segments":[{"index":0,"startMs":0,"endMs":1000}]})}

#[tokio::test]
async fn cancellation_before_first_child_clears_logical_stage_without_orphan(){
    let registry=Registry::default();let ready=Arc::new(tokio::sync::Notify::new());let child_registry=registry.clone();let child_ready=ready.clone();
    let task=tokio::spawn(async move{runtime_owned_work::with_registry(child_registry,media_owned_stage(async move{
        child_ready.notify_one();std::future::pending::<Result<(),String>>().await
    })).await});
    ready.notified().await;assert_eq!(registry.snapshot().unwrap().active,1);
    let token=registry.close().unwrap();task.abort();let _=task.await;
    let state=registry.snapshot().unwrap();assert_eq!(state.active,0);assert_eq!(state.unresolved,0);
    registry.resume(&token).unwrap();
}

#[tokio::test]
async fn canceled_segment_retains_dispatch_and_independent_started_child_ticket(){
    let registry=Registry::default();let life=Arc::new(Lifecycle::new());let ready=Arc::new(tokio::sync::Notify::new());
    let temp=tempfile::tempdir().unwrap();let store=crate::media_artifacts::ArtifactStore::open(temp.path()).unwrap();let req=request();
    let task_registry=registry.clone();let task_life=life.clone();let task_req=req.clone();let task_ready=ready.clone();
    let task=tokio::spawn(async move{runtime_owned_work::with_registry(task_registry.clone(),media_owned_stage(async move{
        let reserved=task_life.reserve(task_req.clone()).await?;
        analysis_output::execute_segments(&store,&task_req,&reserved,&*task_life,|_,_,_|{
            let r=task_registry.clone();let ready=task_ready.clone();async move{
                // A fake started ticket exercises independent ownership; it
                // cannot certify any real native process stopped.
                let mut native=r.begin_child(Kind::MediaTool).map_err(|e|e.1)?;native.mark_started();ready.notify_one();
                let returned=std::future::pending::<Result<(String,String,u64),String>>().await;drop(native);returned
            }
        }).await.map(|_|())
    })).await});
    ready.notified().await;assert_eq!(registry.snapshot().unwrap().active,2);
    let token=registry.close().unwrap();task.abort();let _=task.await;
    let state=registry.snapshot().unwrap();assert_eq!(state.active,0);assert_eq!(state.unresolved,1);assert!(registry.resume(&token).is_err());
    let retained=life.ledger.lock().unwrap().clone();assert_eq!(retained["analyses"][0]["attempts"][0]["dispatched"].as_array().unwrap().len(),1);
    // The original durable dispatch remains exclusion evidence after logical
    // cancellation, even without a captured output or fail callback.
    let mut event=req.clone();event["segmentIndex"]=json!(0);
    assert_eq!(life.event("mark_dispatched",event).await.unwrap_err(),"fixture_no_paid_replay");
    let id=registry.unresolved_ids().unwrap()[0];registry.observe_cessation(&token,id).unwrap();registry.resume(&token).unwrap();
    assert_eq!(life.ledger.lock().unwrap()["analyses"][0]["attempts"][0]["dispatched"].as_array().unwrap().len(),1);
}

#[tokio::test]
async fn drain_after_full_paid_capture_rejects_new_ocr_stage_and_preserves_audio(){
    let registry=Registry::default();let life=Lifecycle::new();let temp=tempfile::tempdir().unwrap();let store=crate::media_artifacts::ArtifactStore::open(temp.path()).unwrap();let req=request();
    runtime_owned_work::with_registry(registry.clone(),media_owned_stage(async{
        let reserved=life.reserve(req.clone()).await?;
        let(segments,_)=analysis_output::execute_segments(&store,&req,&reserved,&life,|_,_,_|std::future::ready(Ok(("raw speech".into(),"speech".into(),1000)))).await?;
        let audio=json!({"materials":[{"kind":"transcript","text":"speech"}],"coverage":{"kind":"full_audio","durationMs":1000},"outcome":"transcript"});
        analysis_output::complete_before_followup(&store,&req,&segments,audio,&life,|audio|async{
            registry.close().unwrap();let tools=AtomicUsize::new(0);
            let(_,meta)=ocr_after_admission(Some(&life),||async{tools.fetch_add(1,Ordering::SeqCst);("forbidden OCR".into(),json!({"status":"completed"}))}).await;
            assert_eq!(tools.load(Ordering::SeqCst),0);assert_eq!(life.ocr_calls.load(Ordering::SeqCst),0);
            assert_eq!(meta["status"],"unavailable");assert_eq!(meta["reason"],"lifecycle_draining");
            let state=ledger::read_result(&life.ledger.lock().unwrap(),&req).unwrap();assert_eq!(state["disposition"],"reuse");
            assert_eq!(analysis_output::read_full(&store,&req,&state["result"]).unwrap()["materials"][0]["text"],"speech");
            Ok(audio)
        }).await.map(|_|())
    })).await.unwrap();
    assert_eq!(registry.snapshot().unwrap().active,0);assert_eq!(registry.snapshot().unwrap().unresolved,0);
    assert_eq!(life.reserve(req).await.unwrap()["disposition"],"reuse");
}

#[tokio::test]
async fn closed_stage_admission_runs_no_reserved_or_native_future(){
    let registry=Registry::default();registry.close().unwrap();let polls=AtomicUsize::new(0);
    let result=runtime_owned_work::with_registry(registry.clone(),media_owned_stage(async{polls.fetch_add(1,Ordering::SeqCst);Ok::<_,String>(())})).await;
    assert!(result.is_err());assert_eq!(polls.load(Ordering::SeqCst),0);assert_eq!(registry.snapshot().unwrap().active,0);
}
