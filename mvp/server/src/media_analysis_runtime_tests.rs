use super::*;

async fn fixture()->(Runtime,Value,tempfile::TempDir) {
    let (app,temp)=crate::tests::test_app().await;
    let store=crate::media_fullframes::store().unwrap();
    let source=store.put_bytes(format!("offline-media-{}",crate::id()).as_bytes()).unwrap().to_json();
    let post=json!({"id":"runtime-post","postKey":"runtime-post-key","text":"original metadata"});
    let mut progress=json!({"schemaVersion":2,"account":"LikeAvto","connectorBinding":app.account.binding(),
        "sourcePostId":post["id"],"sourcePostKey":post["postKey"],"sourceVersion":crate::media_fullframes::source_version(&post,"LikeAvto"),
        "source":source,"sourceProjection":{"sourceUrl":"https://invalid.example/offline"},"leaseId":"runtime-lease","leaseEpoch":1,"phase":"finalize"});
    app.change(|d| {
        crate::accounts::initialize(d,app.account)?;
        for name in ["knowledge_entries","knowledge_versions"] {if !d[name].is_array() {d[name]=json!([]);}}
        progress["connectorBinding"]=d["connectorBinding"].clone();
        crate::list_mut(d,"posts").push(post.clone());
        let job=json!({"id":"runtime-job","kind":"media","status":"running","account":d["account"],
            "connectorBinding":d["connectorBinding"],"result":{"visualProgress":progress}});
        crate::list_mut(d,"jobs").push(job);
        Ok(())
    }).await.unwrap();
    // test_app already bootstraps its native lifecycle owner. Exercise the
    // actual binding entry without replacing that owner or initializing twice.
    let runtime=Runtime::bind(&app,"runtime-job",Some(&progress)).await.unwrap();
    let file_receipt=json!({"method":"local_sha256_and_size","source":source});
    let probe=json!({"method":"ffprobe_selected_audio_stream_inventory","hasAudio":true,"mediaDurationSeconds":1.0});
    let mut request=runtime.binding();
    request["verifiedFile"]=json!({"sha256":source["sha256"],"bytes":source["bytes"],
        "receiptSha256":ledger::hash(&file_receipt),"probeSha256":ledger::hash(&probe)});
    request["verifiedReceipt"]=file_receipt;request["probeReceipt"]=probe;
    request["stage"]=json!("asr");request["specSha256"]=json!("c".repeat(64));
    request["durationMs"]=json!(1000);request["segments"]=json!([{"index":0,"startMs":0,"endMs":1000}]);
    (runtime,request,temp)
}
fn fresh(runtime:&Runtime,request:&Value)->(Runtime,Value) {
    let mut binding=runtime.binding.clone();let attempt=crate::id();
    binding["attemptId"]=json!(attempt);binding["owner"]=json!(format!("offline:{attempt}"));binding["manifestKey"]=json!(format!("offline:{attempt}"));
    let mut next=request.clone();for (k,v) in binding.as_object().unwrap() {next[k]=v.clone();}
    (Runtime {app:runtime.app.clone(),execution_job_id:runtime.execution_job_id.clone(),progress:runtime.progress.clone(),
        execution_pin:runtime.execution_pin.clone(),binding,lifecycle:runtime.lifecycle.clone()},next)
}
async fn segment(runtime:&Runtime,request:&Value)->Value {
    let mut event=request.clone();event["segmentIndex"]=json!(0);
    runtime.event("mark_dispatched",event.clone()).await.unwrap();
    let store=crate::media_fullframes::store().unwrap();
    let captured=output::capture_segment(&store,request,0,"offline raw speech","offline speech",1000).unwrap();
    event["segment"]=captured.clone();runtime.event("commit_segment",event).await.unwrap();captured
}
fn full(request:&Value,segment:&Value)->Value {
    output::capture_full(&crate::media_fullframes::store().unwrap(),request,&[segment.clone()],
        &json!({"materials":[{"kind":"transcript","text":"offline speech"}],"coverage":{"kind":"full_audio","durationMs":1000},"outcome":"transcript"})).unwrap()
}

#[tokio::test]
async fn mixed_spec_and_new_owner_cannot_dispatch_paid_file_twice() {
    let (runtime,request,_temp)=fixture().await;
    assert_eq!(runtime.reserve(request.clone()).await.unwrap()["disposition"],"reserved");
    let (other,mut next)=fresh(&runtime,&request);next["specSha256"]=json!("d".repeat(64));
    assert_eq!(other.reserve(next.clone()).await.unwrap()["disposition"],"held");
    next["segmentIndex"]=json!(0);assert!(other.event("mark_dispatched",next).await.is_err());
    let mut intent=request.clone();intent["segmentIndex"]=json!(0);
    runtime.event("mark_dispatched",intent.clone()).await.unwrap();
    assert!(runtime.event("mark_dispatched",intent.clone()).await.is_err());
    let calls=std::sync::atomic::AtomicUsize::new(0);
    let store=crate::media_fullframes::store().unwrap();
    let replay=output::execute_segments(&store,&request,&json!({"completedSegments":[]}),&runtime,|_,_,_| {
        calls.fetch_add(1,std::sync::atomic::Ordering::SeqCst);
        async {Ok::<_,String>(("raw".to_owned(),"words".to_owned(),1000))}
    }).await;
    assert!(replay.is_err());assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst),0);
    intent["reason"]=json!("output_missing_after_dispatch");runtime.event("fail",intent).await.unwrap();
    let held=other.reserve(fresh(&runtime,&request).1).await;
    // A differently bound request is rejected before it can alter the ledger.
    assert!(held.is_err());
    assert_eq!(ledger::ledger_from_workspace(&runtime.app.read().await.unwrap()).unwrap()["analyses"][0]["attempts"][0]["status"],"unknown");
}

#[tokio::test]
async fn hashless_legacy_asr_holds_cutover_but_unrelated_download_unknown_does_not() {
    let (runtime,request,_temp)=fixture().await;
    runtime.app.change(|d| {
        crate::list_mut(d,"jobs").push(json!({"id":"old-asr","kind":"media_audio","status":"unknown","mediaAnalysisBinding":{},"audioPin":{"progress":{}}}));Ok(())
    }).await.unwrap();
    assert!(runtime.reserve(request.clone()).await.unwrap_err().contains("legacy_asr_reconciliation_required"));
    assert!(crate::list(&runtime.app.read().await.unwrap(),"jobs").iter().all(|j|j["kind"]!="media_analysis"));
    runtime.app.change(|d| {
        let job=crate::row_mut(d,"jobs","old-asr")?;job["kind"]=json!("media");
        job["result"]=json!({"visualProgress":{"schemaVersion":2,"phase":"download"}});Ok(())
    }).await.unwrap();
    assert_eq!(runtime.reserve(request).await.unwrap()["disposition"],"reserved");
}

#[tokio::test]
async fn native_drain_rejects_stale_token_before_any_paid_reservation() {
    let (runtime,request,_temp)=fixture().await;
    runtime.app.db.change_runtime_lifecycle_with_ledger(|d| {
        runtime_lifecycle::begin_drain(d,&runtime.lifecycle,&"d".repeat(64),"offline-drain",false)?;Ok(())
    }).await.unwrap();
    assert!(runtime.reserve(request).await.is_err());
    let d=runtime.app.read().await.unwrap();
    assert!(crate::list(&d,"jobs").iter().all(|j|j["kind"]!="media_analysis"));
}

#[tokio::test]
async fn returned_output_survives_alias_change_and_native_drain() {
    let (runtime,request,_temp)=fixture().await;runtime.reserve(request.clone()).await.unwrap();
    runtime.event("permit_ocr",runtime.binding()).await.unwrap();
    let mut event=request.clone();event["segmentIndex"]=json!(0);
    runtime.event("mark_dispatched",event.clone()).await.unwrap();
    let store=crate::media_fullframes::store().unwrap();
    let captured=output::capture_segment(&store,&request,0,"offline raw speech","offline speech",1000).unwrap();
    let captured_full=full(&request,&captured);
    runtime.app.change(|d| {
        crate::row_mut(d,"posts","runtime-post")?["text"]=json!("changed metadata");
        Ok(())
    }).await.unwrap();
    runtime.app.db.change_runtime_lifecycle_with_ledger(|d| {
        runtime_lifecycle::begin_drain(d,&runtime.lifecycle,&"d".repeat(64),"offline-drain",false)?;Ok(())
    }).await.unwrap();
    assert!(runtime.event("permit_ocr",runtime.binding()).await.is_err());
    event["segment"]=captured;runtime.event("commit_segment",event.clone()).await.unwrap();
    event["result"]=captured_full;
    runtime.event("commit_full_result",event).await.unwrap();
    let state=ledger::ledger_from_workspace(&runtime.app.read().await.unwrap()).unwrap();
    assert_eq!(ledger::read_result(&state,&request).unwrap()["disposition"],"reuse");
    let mut dispatch=request.clone();dispatch["segmentIndex"]=json!(0);
    assert!(runtime.event("mark_dispatched",dispatch).await.is_err());
}

#[tokio::test]
async fn permit_ocr_uses_original_binding_without_requiring_an_asr_reservation() {
    let (runtime,_request,_temp)=fixture().await;
    let mut stale=runtime.binding();stale["owner"]=json!("different-owner");
    assert!(runtime.event("permit_ocr",stale).await.is_err());
    runtime.event("permit_ocr",runtime.binding()).await.unwrap();
    let d=runtime.app.read().await.unwrap();
    assert!(crate::list(&d,"jobs").iter().all(|j|j["kind"]!="media_analysis"));
    runtime.app.db.change_runtime_lifecycle_with_ledger(|d| {
        runtime_lifecycle::begin_drain(d,&runtime.lifecycle,&"d".repeat(64),"offline-drain",false)?;Ok(())
    }).await.unwrap();
    assert!(runtime.event("permit_ocr",runtime.binding()).await.is_err());
}

#[tokio::test]
async fn orphan_full_closure_reconciles_with_original_owner_and_zero_dispatch() {
    let (runtime,request,_temp)=fixture().await;runtime.reserve(request.clone()).await.unwrap();
    let captured=segment(&runtime,&request).await;full(&request,&captured);
    // Simulate crash after CAS capture and before final ledger commit by
    // constructing a different worker owner against the actual saved database.
    let (other,next)=fresh(&runtime,&request);
    let view=other.reserve(next.clone()).await.unwrap();
    assert_eq!(view["disposition"],"reuse");assert_eq!(view["originalRequest"],request);
    assert_eq!(view["result"]["attemptId"],runtime.binding["attemptId"]);
    let d=other.app.read().await.unwrap();let state=ledger::ledger_from_workspace(&d).unwrap();
    assert_eq!(state["analyses"][0]["attempts"].as_array().unwrap().len(),1);
    assert_eq!(state["analyses"][0]["attempts"][0]["dispatched"].as_array().unwrap().len(),1);
}

#[tokio::test]
async fn orphan_segment_is_adopted_but_never_grants_a_new_worker_dispatch() {
    let (runtime,request,_temp)=fixture().await;runtime.reserve(request.clone()).await.unwrap();
    let mut intent=request.clone();intent["segmentIndex"]=json!(0);runtime.event("mark_dispatched",intent).await.unwrap();
    output::capture_segment(&crate::media_fullframes::store().unwrap(),&request,0,"returned raw","returned words",1000).unwrap();
    let (other,next)=fresh(&runtime,&request);let view=other.reserve(next.clone()).await.unwrap();
    assert_eq!(view["disposition"],"held");assert_eq!(view["completedSegments"].as_array().unwrap().len(),1);
    assert_eq!(view["attempt"]["attemptId"],request["attemptId"]);
    let mut dispatch=next;dispatch["segmentIndex"]=json!(0);assert!(other.event("mark_dispatched",dispatch).await.is_err());
}

#[tokio::test]
async fn retained_legacy_catalog_is_adopted_without_invented_attempt_or_asr() {
    let (runtime,request,_temp)=fixture().await;
    let material=json!({"id":"original-paid-audio","title":"Original paid audio","kind":"transcript","text":"saved paid speech",
        "account":"LikeAvto","postKey":runtime.progress["sourcePostKey"],"sourceUrl":runtime.progress["sourceProjection"]["sourceUrl"],
        "mediaSha256":request["verifiedFile"]["sha256"],"transcription":{"partial":false,"coverage":"full_audio","mediaDurationSeconds":1.0,
            "audioDurationSeconds":1.0,"audioStatus":"transcribed","sourcePostKey":runtime.progress["sourcePostKey"],
            "sourceVersion":runtime.progress["sourceVersion"],"ocr":{"status":"not_requested_audio_only"}}});
    runtime.app.change(|d|crate::merge_materials(d,&json!({"materials":[material]}))).await.unwrap();
    let view=runtime.reserve(request.clone()).await.unwrap();
    assert_eq!(view["disposition"],"reuse");assert_eq!(view["result"]["sourceKind"],"legacy_adopted");
    assert!(view["result"]["attemptId"].is_null());assert!(view["result"]["owner"].is_null());
    assert_eq!(view["audio"]["materials"][0]["text"],"saved paid speech");
    let state=ledger::ledger_from_workspace(&runtime.app.read().await.unwrap()).unwrap();
    assert!(state["analyses"][0]["attempts"].as_array().unwrap().is_empty());
    let (other,mut next)=fresh(&runtime,&request);next["specSha256"]=json!("d".repeat(64));
    assert_eq!(other.reserve(next.clone()).await.unwrap()["disposition"],"incompatible");
    next["segmentIndex"]=json!(0);assert!(other.event("mark_dispatched",next).await.is_err());
}

#[tokio::test]
async fn corrupt_cas_reuse_is_rejected_without_fresh_owner() {
    let (runtime,request,_temp)=fixture().await;runtime.reserve(request.clone()).await.unwrap();
    let captured=segment(&runtime,&request).await;let result=full(&request,&captured);
    let mut event=request.clone();event["result"]=result.clone();runtime.event("commit_full_result",event).await.unwrap();
    let store=crate::media_fullframes::store().unwrap();
    let path=store.path(&crate::media_fullframes::reference(&result["normalizedOutput"]).unwrap()).unwrap();
    std::fs::write(path,b"corrupt").unwrap();
    let (other,next)=fresh(&runtime,&request);assert!(other.reserve(next).await.is_err());
    let state=ledger::ledger_from_workspace(&other.app.read().await.unwrap()).unwrap();
    assert_eq!(state["analyses"][0]["attempts"].as_array().unwrap().len(),1);
    assert_eq!(state["analyses"][0]["attempts"][0]["status"],"completed");
}

// Residual R7 validation checks: actual native App path, zero external calls.
async fn fixture_on(postgres:bool,kind:&str)->(Runtime,Value,tempfile::TempDir) {
    let (mut runtime,request,temp)=fixture().await;
    if kind=="media_audio" {
        runtime.app.change(|d| {
            let job=crate::row_mut(d,"jobs","runtime-job")?;
            let progress=job["result"]["visualProgress"].clone();
            job["kind"]=json!("media_audio");job["audioPin"]=json!({"progress":progress});Ok(())
        }).await.unwrap();
        runtime=Runtime::bind(&runtime.app,"runtime-job",Some(&runtime.progress)).await.unwrap();
    }
    if postgres {
        let state=runtime.app.read().await.unwrap();let db=crate::storage::writer_v51_fixture_db().await;
        db.change(|d| {for (key,value) in state.as_object().unwrap() {
            if key!="runtimeLifecycle" {d[key]=value.clone();}
        }Ok(())}).await.unwrap();
        db.change_runtime_lifecycle_with_ledger(|d|crate::runtime_lifecycle_startup::initialize_fixture(d,&runtime.app.lifecycle_owner).map(|_|())).await.unwrap();
        let sqlite=std::mem::replace(&mut runtime.app.db,db);sqlite.close().await;
    }
    // Rebinding media_audio above changes attempt identity only; this fixture
    // returns the matching request, never retargeting a paid existing request.
    let mut request=request;for (key,value) in runtime.binding.as_object().unwrap(){request[key]=value.clone();}
    (runtime,request,temp)
}
// Native commit adds immutable ownership and provenance to the raw CAS output.
// Reuse validates that retained record, exactly as Runtime::reserve_inner does.
fn committed_result_fixture(d:&Value,request:&Value,captured:&Value)->Value {
    let state=ledger::ledger_from_workspace(d).unwrap();
    let view=ledger::read_result(&state,request).unwrap();
    assert_eq!(view["disposition"],"reuse");
    let result=view["result"].clone();
    for key in ["manifest","normalizedOutput","coverage","outcome","verificationSha256"] {
        assert_eq!(result[key],captured[key],"paid CAS output must be unchanged: {key}");
    }
    for key in ["attemptId","owner","epoch","specSha256","manifestKey","companyId","verifiedFile"] {
        assert_eq!(result[key],request[key],"original paid binding must be unchanged: {key}");
    }
    assert_eq!(result["segments"],view["attempt"]["segments"]);
    assert_eq!(result["originalRequest"],*request);
    let mut payload=result.clone();payload.as_object_mut().unwrap().remove("resultSha256");
    assert_eq!(result["resultSha256"],ledger::hash(&payload));
    assert_ne!(result,*captured,"pre-commit output is not the committed provenance record");
    result
}
async fn assert_pure_permit(runtime:&Runtime) {
    let before=runtime.app.read().await.unwrap();
    let (result,events)=crate::performance::capture(runtime.permit_followup()).await;result.unwrap();
    assert_eq!(runtime.app.read().await.unwrap(),before);
    assert!(events.iter().any(|e|e["stage"]=="media.validation.ocr.total"));
    assert!(events.iter().all(|e|e["stage"]!="workspace.change.total"&&e["stage"]!="workspace.change.load"));
}
async fn readonly_validation_checks(runtime:Runtime,request:Value) {
    assert_pure_permit(&runtime).await;
    let before=runtime.app.read().await.unwrap();
    let (mut wrong,_)=fresh(&runtime,&request);wrong.lifecycle.epoch+=1;
    assert!(wrong.permit_followup().await.is_err());
    let (mut wrong,_)=fresh(&runtime,&request);
    wrong.app.lifecycle_owner=std::sync::Arc::new(runtime_lifecycle::RuntimeIdentity{runtime_id:"foreign-runtime".into(),..(*wrong.app.lifecycle_owner).clone()});
    assert!(wrong.permit_followup().await.is_err());
    let (mut wrong,_)=fresh(&runtime,&request);wrong.binding["companyId"]=json!("BAW Russia");
    assert!(wrong.permit_followup().await.is_err());
    let (mut wrong,_)=fresh(&runtime,&request);wrong.progress["sourcePostId"]=json!("absent-post");
    assert!(wrong.permit_followup().await.is_err());
    let (mut wrong,_)=fresh(&runtime,&request);wrong.execution_job_id="absent-job".into();
    assert!(wrong.permit_followup().await.is_err());
    assert_eq!(runtime.app.read().await.unwrap(),before,"failed checks cannot change durable state");

    // Actual current source changes after Runtime::bind must win inside fence.
    runtime.app.change(|d|{crate::row_mut(d,"posts","runtime-post")?["text"]=json!("source changed after capture");Ok(())}).await.unwrap();
    let changed=runtime.app.read().await.unwrap();assert!(runtime.permit_followup().await.is_err());
    assert_eq!(runtime.app.read().await.unwrap(),changed);
    runtime.app.change(|d|{*crate::row_mut(d,"posts","runtime-post")?=crate::row(&before,"posts","runtime-post")?.clone();Ok(())}).await.unwrap();
    runtime.app.change(|d|{crate::row_mut(d,"jobs","runtime-job")?["status"]=json!("failed");Ok(())}).await.unwrap();
    assert!(runtime.permit_followup().await.is_err());
    runtime.app.change(|d|{crate::row_mut(d,"jobs","runtime-job")?["status"]=json!("running");Ok(())}).await.unwrap();
    assert_pure_permit(&runtime).await;
    runtime.app.db.change_runtime_lifecycle_with_ledger(|d|runtime_lifecycle::begin_drain(d,&runtime.lifecycle,&"d".repeat(64),"validation-drain",false).map(|_|())).await.unwrap();
    let drained=runtime.app.read().await.unwrap();assert!(runtime.permit_followup().await.is_err());
    assert_eq!(runtime.app.read().await.unwrap(),drained);
}
#[tokio::test]
async fn scoped_validation_media_and_media_audio_are_readonly_and_reject_stale_fences() {
    for kind in ["media","media_audio"] {
        let (runtime,request,_temp)=fixture_on(false,kind).await;
        readonly_validation_checks(runtime,request).await;
    }
}
#[tokio::test]
#[ignore="ROOT-only fresh isolated writer_v51 PostgreSQL fixture; run alone"]
async fn postgres_scoped_media_validation_preserves_current_owner_and_media_audio() {
    let (runtime,request,_temp)=fixture_on(true,"media_audio").await;
    runtime.reserve(request.clone()).await.unwrap();
    let captured=segment(&runtime,&request).await;let result=full(&request,&captured);
    let mut event=request.clone();event["result"]=result.clone();runtime.event("commit_full_result",event).await.unwrap();
    let before=runtime.app.read().await.unwrap();
    let retained=committed_result_fixture(&before,&request,&result);
    assert_eq!(runtime.validate_reused_output(&request,&result,&json!({}),&crate::now()).await.unwrap_err(),
        "media_analysis_result_changed","raw CAS output must not bypass committed provenance");
    runtime.validate_reused_output(&request,&retained,&json!({}),&crate::now()).await.unwrap();
    assert_eq!(runtime.app.read().await.unwrap(),before);
    readonly_validation_checks(runtime,request).await;
}
#[tokio::test]
async fn scoped_reuse_validation_preserves_paid_result_and_same_owner_drain_completion() {
    let (runtime,request,_temp)=fixture().await;runtime.reserve(request.clone()).await.unwrap();
    let captured=segment(&runtime,&request).await;let result=full(&request,&captured);
    let mut event=request.clone();event["result"]=result.clone();runtime.event("commit_full_result",event).await.unwrap();
    let audio=json!({});let at=crate::now();let before=runtime.app.read().await.unwrap();
    let retained=committed_result_fixture(&before,&request,&result);
    assert_eq!(runtime.validate_reused_output(&request,&result,&audio,&at).await.unwrap_err(),
        "media_analysis_result_changed","raw CAS output must not bypass committed provenance");
    let result=retained;
    let (seen,events)=crate::performance::capture(runtime.validate_reused_output(&request,&result,&audio,&at)).await;seen.unwrap();
    assert!(events.iter().any(|e|e["stage"]=="media.validation.reuse.total"));
    assert!(events.iter().all(|e|e["stage"]!="workspace.change.total"));
    let mut forged=result.clone();forged["resultSha256"]=json!("0".repeat(64));
    assert_eq!(runtime.validate_reused_output(&request,&forged,&audio,&at).await.unwrap_err(),
        "media_analysis_result_changed");
    assert_eq!(runtime.app.read().await.unwrap(),before);
    runtime.app.db.change_runtime_lifecycle_with_ledger(|d|runtime_lifecycle::begin_drain(d,&runtime.lifecycle,&"d".repeat(64),"reuse-validation-drain",false).map(|_|())).await.unwrap();
    let drained=runtime.app.read().await.unwrap();
    // Already captured result validation is not a new paid/OCR admission.
    runtime.validate_reused_output(&request,&result,&audio,&at).await.unwrap();
    assert!(runtime.permit_followup().await.is_err());
    assert_eq!(runtime.app.read().await.unwrap(),drained);
}
#[tokio::test]
async fn scoped_legacy_reuse_rechecks_whole_catalogue_and_current_donor() {
    let (runtime,request,_temp)=fixture().await;
    let mut material=json!({"id":"original-paid-audio","title":"Original paid audio","kind":"transcript","text":"saved paid speech",
        "account":"LikeAvto","postKey":runtime.progress["sourcePostKey"],"sourceUrl":runtime.progress["sourceProjection"]["sourceUrl"],
        "mediaSha256":request["verifiedFile"]["sha256"],"transcription":{"partial":false,"coverage":"full_audio","mediaDurationSeconds":1.0,
            "audioDurationSeconds":1.0,"audioStatus":"transcribed","sourcePostKey":runtime.progress["sourcePostKey"],
            "sourceVersion":runtime.progress["sourceVersion"],"ocr":{"status":"not_requested_audio_only"}}});
    runtime.app.change(|d|crate::merge_materials(d,&json!({"materials":[material]}))).await.unwrap();
    let view=runtime.reserve(request.clone()).await.unwrap();assert_eq!(view["result"]["sourceKind"],"legacy_adopted");
    let before=runtime.app.read().await.unwrap();
    let (seen,events)=crate::performance::capture(runtime.validate_reused_output(&request,&view["result"],&view["audio"],&crate::now())).await;seen.unwrap();
    assert!(events.iter().any(|event|event["stage"]=="media.validation.reuse.total"));
    assert!(events.iter().all(|event|event["stage"]!="workspace.change.total"));assert_eq!(runtime.app.read().await.unwrap(),before);
    material["text"]=json!("corrected paid transcript");
    runtime.app.change(|d|crate::merge_materials(d,&json!({"materials":[material]}))).await.unwrap();
    let changed=runtime.app.read().await.unwrap();
    assert!(runtime.validate_reused_output(&request,&view["result"],&view["audio"],&crate::now()).await.is_err());
    assert_eq!(runtime.app.read().await.unwrap(),changed);
    assert_eq!(ledger::ledger_from_workspace(&changed).unwrap(),ledger::ledger_from_workspace(&before).unwrap(),"donor correction cannot alter paid evidence");
}
#[tokio::test]
async fn media_validation_waiter_observes_drain_committed_before_its_writer_turn() {
    let (runtime,request,_temp)=fixture().await;let (waiter,_)=fresh(&runtime,&request);
    let gate=runtime.app.gate.acquire(crate::writer_gate::Class::Standard).await;
    let (ready_tx,ready_rx)=tokio::sync::oneshot::channel();
    let pending=tokio::spawn(async move {ready_tx.send(()).unwrap();waiter.permit_followup().await});
    ready_rx.await.unwrap();
    // The test owns the App writer gate and performs the prior turn's exact
    // durable drain directly; the waiting check cannot use a pre-gate snapshot.
    runtime.app.db.change_runtime_lifecycle_with_ledger(|d|runtime_lifecycle::begin_drain(d,&runtime.lifecycle,&"d".repeat(64),"queued-validation-drain",false).map(|_|())).await.unwrap();
    let drained=runtime.app.read().await.unwrap();drop(gate);
    assert!(pending.await.unwrap().is_err());assert_eq!(runtime.app.read().await.unwrap(),drained);
}