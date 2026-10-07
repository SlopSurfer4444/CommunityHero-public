use super::*;
use crate::media_analysis as ledger;
use std::sync::{Mutex,atomic::{AtomicUsize,Ordering}};

struct Lifecycle{ledger:Mutex<Value>}
impl Lifecycle{fn new()->Self{Self{ledger:Mutex::new(Value::Null)}}}
impl AudioLifecycle for Lifecycle{
    fn binding(&self)->Value{Value::Null}
    fn reserve(&self,request:Value)->LifecycleFuture<'_,Value>{Box::pin(async move{
        ledger::reserve(&mut self.ledger.lock().unwrap(),&request)
    })}
    fn event(&self,event:&'static str,request:Value)->LifecycleFuture<'_,()>{Box::pin(async move{
        let mut db=self.ledger.lock().unwrap();
        let result=match event {
            "mark_dispatched"=>ledger::mark_dispatched(&mut db,&request),
            "commit_segment"=>ledger::commit_segment(&mut db,&request),
            "commit_full_result"=>ledger::commit_full_result(&mut db,&request),
            "fail"=>ledger::fail(&mut db,&request),_=>Err("fixture_event_invalid".into())
        }?;
        if result["disposition"]=="already_dispatched"{return Err("media_asr_already_dispatched".into());}Ok(())
    })}
}
fn request(count:usize)->Value{
    let plan:Vec<Value>=(0..count).map(|i|json!({"index":i,"startMs":i as u64*1000,"endMs":(i as u64+1)*1000})).collect();
    json!({"companyId":"company-a","verifiedFile":{"sha256":"a".repeat(64),"bytes":1234,"receiptSha256":"b".repeat(64),"probeSha256":"c".repeat(64)},
        "stage":"asr","attemptId":"attempt-one","owner":"worker-one","epoch":1,"specSha256":"d".repeat(64),"manifestKey":"manifest-one",
        "segments":plan,"durationMs":count.max(1)*1000,"sourceVersion":"alias-original"})
}
fn audio(request:&Value)->Value{json!({"materials":[{"kind":"transcript","text":"paid words","account":"company-a","postKey":"original-post","transcription":{"sourceVersion":"alias-original"}}],
    "coverage":{"kind":"full_audio","durationMs":request["durationMs"]},"outcome":"transcript"})}
fn segment_event(request:&Value,index:usize)->Value{let mut event=request.clone();event["segmentIndex"]=json!(index);event}

#[tokio::test]
async fn completed_segment_cas_and_real_ledger_precede_next_dispatch(){
    let temp=tempfile::tempdir().unwrap();let store=ArtifactStore::open(temp.path()).unwrap();let life=Lifecycle::new();let req=request(3);
    let reserved=life.reserve(req.clone()).await.unwrap();let calls=AtomicUsize::new(0);
    let(out,texts)=execute_segments(&store,&req,&reserved,&life,|index,_,_|{
        assert_eq!(life.ledger.lock().unwrap()["analyses"][0]["attempts"][0]["segments"].as_array().unwrap().len(),index);
        if index>0{reconcile_segment(&store,&req,index-1).unwrap();}
        calls.fetch_add(1,Ordering::SeqCst);std::future::ready(Ok((format!("raw {index}"),format!("text {index}"),1000)))
    }).await.unwrap();
    assert_eq!(calls.load(Ordering::SeqCst),3);assert_eq!(out.len(),3);assert_eq!(texts[2],"text 2");
}

#[tokio::test]
async fn admitted_missing_segment_recovery_replays_zero_completed_segments(){
    let temp=tempfile::tempdir().unwrap();let store=ArtifactStore::open(temp.path()).unwrap();let life=Lifecycle::new();let req=request(3);
    let reserved=life.reserve(req.clone()).await.unwrap();let old_calls=AtomicUsize::new(0);
    let err=execute_segments(&store,&req,&reserved,&life,|index,_,_|{
        if index==2{std::future::ready(Err("fixture_crash_after_dispatch".into()))}else{
            old_calls.fetch_add(1,Ordering::SeqCst);std::future::ready(Ok((format!("raw {index}"),format!("saved {index}"),1000)))
        }
    }).await.unwrap_err();assert_eq!(err,"fixture_crash_after_dispatch");assert_eq!(old_calls.load(Ordering::SeqCst),2);
    let mut recovered=req.clone();recovered["attemptId"]=json!("attempt-two");recovered["owner"]=json!("worker-two");recovered["epoch"]=json!(2);recovered["manifestKey"]=json!("manifest-two");
    {
        // Serialize/reload the actual reducer state before admitting recovery.
        let persisted=life.ledger.lock().unwrap().to_string();let mut db:Value=serde_json::from_str(&persisted).unwrap();
        let mut event=req.clone();event["action"]=json!("reconcile");event["cessationSha256"]=json!("e".repeat(64));event["reconciliationSha256"]=json!("f".repeat(64));event["resolution"]=json!("output_missing_after_dispatch");
        ledger::recover(&mut db,&event).unwrap();event["action"]=json!("new_attempt");event["nextAttempt"]=recovered.clone();
        event["authority"]=json!({"receiptSha256":"0".repeat(64),"actor":"fixture-explicit-owner","companyId":"company-a","fileSha256":"a".repeat(64),"priorAttemptId":"attempt-one","nextSpecSha256":"d".repeat(64),"purpose":"missing_segments"});
        ledger::recover(&mut db,&event).unwrap();*life.ledger.lock().unwrap()=db;
    }
    let reserved=life.reserve(recovered.clone()).await.unwrap();let new_calls=AtomicUsize::new(0);
    let(out,texts)=execute_segments(&store,&recovered,&reserved,&life,|index,_,_|{
        assert_eq!(index,2);new_calls.fetch_add(1,Ordering::SeqCst);std::future::ready(Ok(("new raw".into(),"new final".into(),1000)))
    }).await.unwrap();assert_eq!(new_calls.load(Ordering::SeqCst),1);assert_eq!(out.len(),3);assert_eq!(texts[0],"saved 0");
    complete_before_followup(&store,&recovered,&out,audio(&recovered),&life,|v|std::future::ready(Ok(v))).await.unwrap();
}

#[tokio::test]
async fn full_audio_survives_ocr_and_catalog_admission_failure(){
    for failure in ["ocr_failed","catalog_alias_conflict"]{
        let temp=tempfile::tempdir().unwrap();let store=ArtifactStore::open(temp.path()).unwrap();let life=Lifecycle::new();let req=request(1);
        let reserved=life.reserve(req.clone()).await.unwrap();let calls=AtomicUsize::new(0);
        let(out,_)=execute_segments(&store,&req,&reserved,&life,|_,_,_|{calls.fetch_add(1,Ordering::SeqCst);std::future::ready(Ok(("raw".into(),"words".into(),1000)))}).await.unwrap();
        assert_eq!(complete_before_followup(&store,&req,&out,audio(&req),&life,|_|async{
            let persisted=life.ledger.lock().unwrap().to_string();let db:Value=serde_json::from_str(&persisted).unwrap();
            let state=ledger::read_result(&db,&req).unwrap();assert_eq!(state["disposition"],"reuse");
            assert_eq!(read_full(&store,&req,&state["result"]).unwrap()["materials"][0]["text"],"paid words");
            Err(failure.into())
        }).await.unwrap_err(),failure);
        let state=life.reserve(req.clone()).await.unwrap();assert_eq!(state["disposition"],"reuse");assert_eq!(calls.load(Ordering::SeqCst),1);
    }
}

#[tokio::test]
async fn captured_output_reconciles_after_missing_ledger_commit_without_asr(){
    let temp=tempfile::tempdir().unwrap();let store=ArtifactStore::open(temp.path()).unwrap();let life=Lifecycle::new();let req=request(1);
    life.reserve(req.clone()).await.unwrap();life.event("mark_dispatched",segment_event(&req,0)).await.unwrap();
    capture_segment(&store,&req,0,"original raw","original text",1000).unwrap();
    let mut event=req.clone();event["reason"]=json!("fixture_process_crash");life.event("fail",event.clone()).await.unwrap();
    assert_eq!(life.reserve(req.clone()).await.unwrap()["disposition"],"unknown");
    event["segment"]=reconcile_segment(&store,&req,0).unwrap();life.event("commit_segment",event.clone()).await.unwrap();
    let full=capture_full(&store,&req,&[event["segment"].clone()],&audio(&req)).unwrap();
    let captured=reconcile_full(&store,&req).unwrap();assert_eq!(captured,full);
    event["result"]=captured;life.event("commit_full_result",event).await.unwrap();
    assert_eq!(life.reserve(req).await.unwrap()["disposition"],"reuse");
}

#[tokio::test]
async fn missing_output_after_dispatch_remains_unknown_and_cannot_replay(){
    let temp=tempfile::tempdir().unwrap();let store=ArtifactStore::open(temp.path()).unwrap();let life=Lifecycle::new();let req=request(1);
    let reserved=life.reserve(req.clone()).await.unwrap();let calls=AtomicUsize::new(0);
    execute_segments(&store,&req,&reserved,&life,|_,_,_|{calls.fetch_add(1,Ordering::SeqCst);std::future::ready(Err::<(String,String,u64),String>("fixture_lost_output".into()))}).await.unwrap_err();
    assert!(reconcile_segment(&store,&req,0).is_err());let state=life.reserve(req.clone()).await.unwrap();assert_eq!(state["disposition"],"unknown");
    execute_segments(&store,&req,&state,&life,|_,_,_|{calls.fetch_add(1,Ordering::SeqCst);std::future::ready(Ok(("never".into(),"never".into(),1000)))}).await.unwrap_err();assert_eq!(calls.load(Ordering::SeqCst),1);
}

#[test]
fn immutable_asset_binding_accepts_alias_drift_and_rejects_company_file_spec_drift(){
    let temp=tempfile::tempdir().unwrap();let store=ArtifactStore::open(temp.path()).unwrap();let req=request(1);
    let segment=capture_segment(&store,&req,0,"raw","text",1000).unwrap();let mut changed=req.clone();changed["sourceVersion"]=json!("alias-new");
    assert_eq!(read_segment(&store,&changed,&segment).unwrap(),"text");
    for key in ["companyId","specSha256"]{let mut changed=req.clone();changed[key]=json!("foreign");assert!(read_segment(&store,&changed,&segment).is_err());}
    let mut changed=req.clone();changed["verifiedFile"]["sha256"]=json!("e".repeat(64));assert!(read_segment(&store,&changed,&segment).is_err());
    let mut corrupt=segment.clone();corrupt["actualDurationMs"]=json!(1);assert!(read_segment(&store,&req,&corrupt).is_err());
    assert!(capture_segment(&store,&req,0,"replacement","replacement",1000).is_err());
}
#[test]
fn bounded_raw_output_survives_json_escape_expansion(){
    let temp=tempfile::tempdir().unwrap();let store=ArtifactStore::open(temp.path()).unwrap();let req=request(1);
    let raw="\u{0001}".repeat(1024*1024);
    let segment=capture_segment(&store,&req,0,&raw,"normalized speech",1000).unwrap();
    assert!(segment["rawOutput"]["bytes"].as_u64().unwrap()>LIMIT);
    assert_eq!(read_segment(&store,&req,&segment).unwrap(),"normalized speech");
    assert_eq!(reconcile_segment(&store,&req,0).unwrap(),segment);
}
#[test]
fn declared_output_reader_rejects_oversize_and_torn_capture(){
    let temp=tempfile::tempdir().unwrap();let store=ArtifactStore::open(temp.path()).unwrap();let req=request(1);
    let path=slot(&store,&req,"segment-000").unwrap();
    std::fs::File::create(&path).unwrap().set_len(LIMIT+1).unwrap();
    assert_eq!(reconcile_segment(&store,&req,0).unwrap_err(),"media_asr_output_invalid");
    std::fs::write(&path,b"{\"binding\":").unwrap();
    assert!(reconcile_segment(&store,&req,0).is_err());
}

#[tokio::test]
async fn honest_no_audio_is_complete_without_any_dispatch(){
    let temp=tempfile::tempdir().unwrap();let store=ArtifactStore::open(temp.path()).unwrap();let life=Lifecycle::new();let mut req=request(0);
    req["noAudio"]=json!(true);req["noAudioVerificationSha256"]=json!("e".repeat(64));let reserved=life.reserve(req.clone()).await.unwrap();
    let(out,_)=execute_segments(&store,&req,&reserved,&life,|_,_,_|std::future::ready(Err::<(String,String,u64),String>("no_audio_must_not_dispatch".into()))).await.unwrap();
    let mut result=audio(&req);result["outcome"]=json!("no_audio");result["coverage"]["kind"]=json!("no_audio_stream");
    complete_before_followup(&store,&req,&out,result,&life,|v|std::future::ready(Ok(v))).await.unwrap();
    assert_eq!(life.reserve(req).await.unwrap()["result"]["outcome"],"no_audio");
}
