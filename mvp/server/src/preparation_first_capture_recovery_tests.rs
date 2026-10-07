//! BAW-only endpoint/restart regression. No model/provider/adapter is available.
use super::*;
use crate::*;

pub(super) async fn baw_app()->(App,tempfile::TempDir){
    let temp=tempfile::tempdir().unwrap();let db=open_db(&temp.path().join("baw-recovery.sqlite")).await.unwrap();
    let (events,_)=broadcast::channel(8);let profile=accounts::Profile::BawRussia;
    let admission=Arc::new(runtime_lifecycle_startup::Admission::fixture(profile));
    let app=App{lifecycle_task_count:Default::default(),lifecycle_owner:Arc::new(admission.identity().clone()),lifecycle_admission:admission,
        lifecycle_provider_token:Default::default(),lifecycle_work:Default::default(),media_discovery:Default::default(),preparation_wake:Default::default(),provider_session:Default::default(),
        account:profile,navigation:account_navigation::Navigation::root(),db:Database::Sqlite(db),gate:Arc::new(writer_gate::WriterGate::default()),execution_gate:Arc::new(Mutex::new(())),
        preparation_workers:Default::default(),editorial_gate:Default::default(),assistant_gate:Arc::new(Mutex::new(())),assistant_chat_gate:Arc::new(Mutex::new(())),
        events,csrf:"baw-recovery".into(),auth:None,public_origin:None,external_writes:false,port:0,data:temp.path().to_owned(),
        bridge:temp.path().join("MODEL_CALLS_MUST_BE_ZERO.mjs"),node:temp.path().join("NO_MODEL_RUNTIME"),tasks:Arc::new(Mutex::new(HashMap::new())),bootstrap_cache:Arc::new(bootstrap_cache::Cache::default())};
    let initial=super::tests::baw_fixture(false);app.db.change(|d|{
        // Preserve mandatory native catalog/feedback collections while seeding.
        for (key,value) in initial.as_object().unwrap(){d[key]=value.clone();}Ok(())
    }).await.unwrap();
    runtime_lifecycle_app::initialize_app_fixture(&app).await.unwrap();(app,temp)
}
fn original_result(wire_request:&Value)->Value{
    let mut result=super::tests::single_pass_result(json!({"text":"Original synthetic paid BAW result","sources":[],
        "assessments":[{"itemId":"ready","outcome":"reply","reason":"Exact source supports greeting","tags":["feedback"]}],
        "proposals":[{"itemId":"ready","kind":"reply_and_close","text":"Спасибо за отзыв!"}]}));
    let mut invocation=model_material_receipt::expected(wire_request);
    invocation["schemaVersion"]=json!(1);invocation["contract"]=json!(preparation_materials::CONTRACT);invocation["completenessStatus"]=json!("complete");
    invocation["actualTextInputSha256"]=result["runMetadata"]["inputSha256"].clone();invocation["instructionSha256"]=result["runMetadata"]["instructionSha256"].clone();
    invocation["cliSha256"]=result["runMetadata"]["cliSha256"].clone();invocation["schemaSha256"]=json!("f".repeat(64));
    invocation["stagedPhotos"]=json!([]);invocation["deliveredPhotos"]=json!([]);invocation["optionalFrameRefs"]=json!([]);
    result["runMetadata"]["materialInvocation"]=invocation;model_material_receipt::validate_result(wire_request,&result).unwrap();result
}
async fn interrupted(app:&App,retain:bool)->(Scheduled,Value,Value){
    interrupted_mode(app,retain,false).await
}
async fn interrupted_mode(app:&App,retain:bool,review_required:bool)->(Scheduled,Value,Value){
    let token=app.lifecycle_admission_token(runtime_lifecycle::AdmissionClass::Preparation).await.unwrap();
    let scheduled=app.change_preparation_schedule(|d|{
        if !review_required{return schedule_admitted(d,Input{item_ids:vec!["ready".into()],instruction:None},&token);}
        // Synthetic historical two-pass capture, still exact BAW/native pins.
        let mut scheduled=schedule(d,Input{item_ids:vec!["ready".into()],instruction:None})?;
        let request=scheduled.request.as_mut().unwrap();request.as_object_mut().unwrap().remove("preparationMode");
        request.as_object_mut().unwrap().remove("factDependencyContract");
        let job=row_mut(d,"jobs",&scheduled.job_id)?;job["prepareBundle"]["request"]=request.clone();rehash(&mut job["prepareBundle"]);
        job.as_object_mut().unwrap().remove("scopeReservation");
        let reservation=preparation_reservations::capture(d,&scheduled.job_id)?;row_mut(d,"jobs",&scheduled.job_id)?["scopeReservation"]=reservation;
        let scope=preparation_workers::capture(d,row(d,"jobs",&scheduled.job_id)?).ok_or_else(||conflict("Historical fixture worker scope unavailable"))?;
        row_mut(d,"jobs",&scheduled.job_id)?["preparationWorkerScope"]=scope;
        preparation_review::record_initial_admission(d,&token,&scheduled.job_id,&now())?;Ok(scheduled)
    }).await.unwrap();
    let request=scheduled.request.as_ref().unwrap().clone();
    app.change_preparation_first(&scheduled.job_id,|d|preparation_review::reserve_first_admitted(d,&token,&scheduled.job_id,&request,&now())).await.unwrap();
    // Actual main bridge normalization: canonical account -> transport key,
    // then the flat operation field. Retention is native CAS I/O only.
    let mut wire=request.clone();app.account.bind_request(&mut wire).unwrap();wire["operation"]=json!("assistant");
    let mut result=original_result(&wire);
    if review_required{result["assessments"][0]["tags"]=json!(["needs_fact"]);}
    if retain{
        let paid=runtime_lifecycle_app::with_job(scheduled.job_id.clone(),runtime_paid_result::retain(app,"assistant",&wire,&result)).await.unwrap().unwrap();
        app.change_job(&scheduled.job_id,|d|runtime_paid_result::attach(d,&scheduled.job_id,&paid,&app.lifecycle_owner)).await.unwrap();
    }
    app.change_job(&scheduled.job_id,|d|{let job=row_mut(d,"jobs",&scheduled.job_id)?;job["status"]=json!("interrupted");job["error"]=json!("Fixture process exited after paid attachment before first settlement");Ok(())}).await.unwrap();
    (scheduled,wire,result)
}
async fn endpoint(app:&App,run:&str,digest:&Value)->ApiResult<Value>{
    preparation_review::chunks::resume(State(app.clone()),Path(run.to_owned()),Json(json!({"expectedFirstRequestDigest":digest}))).await.map(|v|v.0)
}
fn assert_no_model(app:&App){
    assert!(!app.node.exists());assert!(!app.bridge.exists());let work=app.lifecycle_work.snapshot().unwrap();
    assert_eq!(work.active,0);assert_eq!(work.unresolved,0);
}
async fn interrupted_auto(app:&App,retain:bool)->(String,Value){
    let token=app.lifecycle_admission_token(runtime_lifecycle::AdmissionClass::Preparation).await.unwrap();
    let(run,request)=app.change(|d|{
        d["items"].as_array_mut().unwrap().retain(|item|item["id"]=="ready");
        row_mut(d,"items","ready")?["providerObservedAt"]=json!(now());
        let claimed=auto_prepare::claim(d,chrono::Utc::now().timestamp())?.ok_or_else(||conflict("BAW automatic fixture was not claimable"))?;
        preparation_review::record_initial_admission(d,&token,&claimed.0,&now())?;Ok(claimed)
    }).await.unwrap();
    app.change_preparation_first(&run,|d|preparation_review::reserve_first_admitted(d,&token,&run,&request,&now())).await.unwrap();
    let mut wire=request.clone();app.account.bind_request(&mut wire).unwrap();wire["operation"]=json!("assistant");
    if retain{
        let result=original_result(&wire);
        let paid=runtime_lifecycle_app::with_job(run.clone(),runtime_paid_result::retain(app,"assistant",&wire,&result)).await.unwrap().unwrap();
        app.change_job(&run,|d|runtime_paid_result::attach(d,&run,&paid,&app.lifecycle_owner)).await.unwrap();
    }
    app.change(|d|{
        row_mut(d,"jobs",&run)?["status"]=json!("interrupted");
        auto_prepare::recover_jobs(d,chrono::Utc::now().timestamp());Ok(())
    }).await.unwrap();
    (run,request)
}
#[tokio::test]
async fn automatic_first_capture_recovers_after_restart_through_the_same_endpoint_without_new_paid_work(){
    let(mut app,temp)=baw_app().await;let(run,_request)=interrupted_auto(&app,true).await;
    let before=app.db.read().await.unwrap();let old=row(&before,"jobs",&run).unwrap();let item=row(&before,"items","ready").unwrap();
    assert_eq!(item["autoPreparation"]["status"],"error");assert_eq!(item["autoPreparation"]["reasonCode"],"paid_attempt_recovery_required");
    let digest=old["prepareBundle"]["digest"].clone();let old_scope=old["scopeReservation"].clone();let old_admission=old["preparationStages"]["firstAdmission"].clone();
    app.db.close().await;app.db=Database::Sqlite(open_db(&temp.path().join("baw-recovery.sqlite")).await.unwrap());
    let outcome=endpoint(&app,&run,&digest).await.unwrap();assert_eq!(outcome["status"],"prepared");assert_eq!(outcome["dispatchAuthorized"],false);
    let after=app.db.read().await.unwrap();let saved=row(&after,"jobs",&run).unwrap();let prepared=row(&after,"items","ready").unwrap();
    assert_eq!(saved["purpose"],"auto_prepare");assert!(saved["selectedItemIds"].is_null());assert_eq!(saved["requestedItemIds"],json!(["ready"]));
    assert_eq!(saved["scopeReservation"],old_scope);assert_eq!(saved["preparationStages"]["firstAdmission"],old_admission);
    assert_eq!(saved["preparationStages"]["first"]["status"],"completed");assert_eq!(saved["status"],"completed");
    assert_eq!(prepared["autoPreparation"]["jobId"],item["autoPreparation"]["jobId"]);assert_eq!(prepared["autoPreparation"]["attempts"],item["autoPreparation"]["attempts"]);
    assert_eq!(prepared["autoPreparation"]["inputDigest"],item["autoPreparation"]["inputDigest"]);assert_eq!(prepared["autoPreparation"]["status"],"prepared");
    assert_eq!(saved["retainedEvidence"].as_array().unwrap().len(),1);assert_eq!(saved["modelMaterialReceipts"].as_array().unwrap().len(),1);assert_eq!(list(&after,"proposals").len(),1);
    assert_eq!(endpoint(&app,&run,&digest).await.unwrap(),outcome);assert_eq!(app.db.read().await.unwrap(),after);
    assert!(app.tasks.lock().await.is_empty());assert_no_model(&app);app.db.close().await;
}
#[tokio::test]
async fn automatic_recovery_preserves_original_paid_first_and_current_manual_source_unknown_or_foreign_item_state(){
    for fault in ["manual","source","unknown","owner","other_review"]{
        let(app,_temp)=baw_app().await;let(run,_request)=interrupted_auto(&app,true).await;
        app.change(|d|{
            match fault{
                "manual"=>{let item=row_mut(d,"items","ready")?;item["draft"]=json!("Saved operator draft");item["draftEdited"]=json!(true);item["workflow"]=json!("waiting");item["decision"]=json!("operator_choice");item["reason"]=json!("Operator reason");bump(item);},
                "source"=>row_mut(d,"posts","ready-post")?["text"]=json!("Different current BAW source"),
                "unknown"=>{let target=row(d,"items","ready")?.clone();list_mut(d,"operations").push(json!({"id":"baw-unknown","itemId":"ready","kind":"reply_and_close","status":"unknown","target":target,"evidence":{}}));},
                "owner"=>row_mut(d,"items","ready")?["autoPreparation"]["jobId"]=json!("foreign-stopped-owner"),
                _=>{let auto=&mut row_mut(d,"items","ready")?["autoPreparation"];auto["reasonCode"]=json!("operator_required_review");},
            }Ok(())
        }).await.unwrap();
        let before=app.db.read().await.unwrap();let prior=row(&before,"items","ready").unwrap();let job=row(&before,"jobs",&run).unwrap();
        let outcome=endpoint(&app,&run,&job["prepareBundle"]["digest"]).await.unwrap();assert_eq!(outcome["status"],"stale","{fault}");assert_eq!(outcome["dispatchAuthorized"],false);
        let after=app.db.read().await.unwrap();let saved=row(&after,"jobs",&run).unwrap();let current=row(&after,"items","ready").unwrap();
        assert_eq!(saved["preparationStages"]["first"]["status"],"completed");assert_eq!(saved["preparationStages"]["firstAdmission"],job["preparationStages"]["firstAdmission"]);
        assert_eq!(saved["retainedEvidence"],job["retainedEvidence"]);assert!(list(&after,"proposals").is_empty());assert_eq!(after["operations"],before["operations"]);
        for key in ["draft","draftEdited","workflow","revision"]{assert_eq!(current[key],prior[key],"{fault}: {key}");}
        if fault=="manual"{for key in ["decision","reason"]{assert_eq!(current[key],prior[key],"{key}");}}
        for key in ["jobId","attempts","inputDigest"]{assert_eq!(current["autoPreparation"][key],prior["autoPreparation"][key],"{fault}: {key}");}
        if fault=="other_review"{assert_eq!(current["autoPreparation"]["requiresReview"],true);}
        assert!(app.tasks.lock().await.is_empty());assert_no_model(&app);app.db.close().await;
    }
}
#[tokio::test]
async fn automatic_missing_original_capture_keeps_native_recovery_hold_and_spend_reservation(){
    let(app,_temp)=baw_app().await;let(run,_request)=interrupted_auto(&app,false).await;let before=app.db.read().await.unwrap();
    let digest=row(&before,"jobs",&run).unwrap()["prepareBundle"]["digest"].clone();
    assert!(endpoint(&app,&run,&digest).await.unwrap_err().1.contains("reservation remains held"));
    assert_eq!(app.db.read().await.unwrap(),before);assert_no_model(&app);app.db.close().await;
}
#[tokio::test]
async fn existing_resume_endpoint_recovers_first_after_database_reopen_without_model_or_duplicate_admission(){
    let(mut app,temp)=baw_app().await;let(scheduled,wire,original)=interrupted(&app,true).await;let run=&scheduled.job_id;
    let before=app.db.read().await.unwrap();let old=row(&before,"jobs",run).unwrap();let digest=old["prepareBundle"]["digest"].clone();
    let admission=old["preparationStages"]["firstAdmission"].clone();assert!(old["preparationStages"]["first"].is_null());assert!(old["modelMaterialReceipts"].is_null());
    app.db.close().await;app.db=Database::Sqlite(open_db(&temp.path().join("baw-recovery.sqlite")).await.unwrap());
    let outcome=endpoint(&app,run,&digest).await.unwrap();assert_eq!(outcome["status"],"review");assert_eq!(outcome["preparedItemIds"],json!(["ready"]));
    let saved=app.db.read().await.unwrap();let job=row(&saved,"jobs",run).unwrap();
    assert_eq!(job["status"],"completed");assert_eq!(job["prepareBundle"],old["prepareBundle"]);assert_eq!(job["preparationStages"]["firstAdmission"],admission);
    assert_eq!(job["preparationStages"]["first"]["result"]["proposals"],original["proposals"]);
    assert_eq!(job["retainedEvidence"].as_array().unwrap().len(),1);assert_eq!(job["modelMaterialReceipts"].as_array().unwrap().len(),1);
    assert_eq!(job["modelMaterialReceipts"][0]["paidResultRef"],old["retainedEvidence"][0]);assert_eq!(saved["proposals"].as_array().unwrap().len(),1);
    let capture=runtime_paid_result::resolve(&app,run,"assistant",&job["retainedEvidence"][0]).await.unwrap();assert_eq!(capture["request"],wire);assert_eq!(capture["response"],original);
    assert_eq!(endpoint(&app,run,&digest).await.unwrap(),outcome);assert_eq!(app.db.read().await.unwrap(),saved,"endpoint replay never admits a duplicate proposal");
    assert!(app.tasks.lock().await.is_empty());assert_no_model(&app);app.db.close().await;
}
#[tokio::test]
async fn original_first_is_retained_when_current_copy_is_stale_and_no_proposal_is_admitted(){
    let(app,_temp)=baw_app().await;let(scheduled,_wire,original)=interrupted(&app,true).await;let run=&scheduled.job_id;
    let digest=app.db.read().await.unwrap()["jobs"][0]["prepareBundle"]["digest"].clone();
    app.change(|d|{row_mut(d,"posts","ready-post")?["text"]=json!("Source changed after original paid capture");Ok(())}).await.unwrap();
    let outcome=endpoint(&app,run,&digest).await.unwrap();assert_eq!(outcome["status"],"stale");
    let d=app.db.read().await.unwrap();assert_eq!(row(&d,"jobs",run).unwrap()["preparationStages"]["first"]["result"]["proposals"],original["proposals"]);
    assert!(list(&d,"proposals").is_empty());assert_no_model(&app);app.db.close().await;
}
#[tokio::test]
async fn uncertain_first_and_retargeted_endpoint_requests_remain_held_without_generation(){
    let(app,_temp)=baw_app().await;let(scheduled,_wire,_original)=interrupted(&app,false).await;let run=&scheduled.job_id;
    let before=app.db.read().await.unwrap();let digest=row(&before,"jobs",run).unwrap()["prepareBundle"]["digest"].clone();
    assert!(endpoint(&app,run,&digest).await.unwrap_err().1.contains("reservation remains held"));
    assert!(endpoint(&app,run,&json!("0".repeat(64))).await.is_err());
    assert!(preparation_review::chunks::resume(State(app.clone()),Path(run.clone()),Json(json!({"expectedFirstRequestDigest":digest,"expectedPlanDigest":"0".repeat(64)}))).await.is_err());
    assert_eq!(app.db.read().await.unwrap(),before);assert!(app.tasks.lock().await.is_empty());assert_no_model(&app);app.db.close().await;
}
#[tokio::test]
async fn multiple_original_first_captures_require_reconciliation_without_changing_reservation(){
    let(app,_temp)=baw_app().await;let(scheduled,wire,mut original)=interrupted(&app,true).await;let run=&scheduled.job_id;
    original["text"]=json!("Second conflicting original response");
    let paid=runtime_lifecycle_app::with_job(run.clone(),runtime_paid_result::retain(&app,"assistant",&wire,&original)).await.unwrap().unwrap();
    app.change_job(run,|d|runtime_paid_result::attach(d,run,&paid,&app.lifecycle_owner)).await.unwrap();
    let before=app.db.read().await.unwrap();let digest=row(&before,"jobs",run).unwrap()["prepareBundle"]["digest"].clone();
    assert!(endpoint(&app,run,&digest).await.unwrap_err().1.contains("Multiple original paid first captures"));
    assert_eq!(app.db.read().await.unwrap(),before);assert_no_model(&app);app.db.close().await;
}
#[tokio::test]
async fn recovered_review_required_first_has_an_explicit_admitted_resume_without_a_preexisting_chunk_plan(){
    let(app,_temp)=baw_app().await;let(scheduled,_wire,_original)=interrupted_mode(&app,true,true).await;let run=&scheduled.job_id;
    let digest=row(&app.db.read().await.unwrap(),"jobs",run).unwrap()["prepareBundle"]["digest"].clone();
    let recovered=endpoint(&app,run,&digest).await.unwrap();assert_eq!(recovered["reviewResumeRequired"],true);assert_eq!(recovered["dispatchAuthorized"],false);
    let state=app.db.read().await.unwrap();let job=row(&state,"jobs",run).unwrap();
    assert_eq!(job["preparationStages"]["first"]["reviewRequired"],true);assert!(job["preparationStages"]["reviewChunks"].is_null());assert!(job["prepareOutcome"].is_null());
    let first=job["preparationStages"]["first"].clone();let reservation=job["preparationStages"]["firstAdmission"].clone();assert_no_model(&app);
    // Keep the normal worker behind its existing lane while checking admission.
    // The test proves reachability without calling review_profile or a model.
    let lane=app.assistant_gate.lock().await;
    let admitted=preparation_review::chunks::resume(State(app.clone()),Path(run.clone()),Json(json!({"expectedFirstRequestDigest":digest,"resumeReview":true}))).await.unwrap().0;
    assert_eq!(admitted["jobId"],json!(run));assert_eq!(admitted["status"],"running");
    let resumed=app.db.read().await.unwrap();let job=row(&resumed,"jobs",run).unwrap();
    assert_eq!(job["preparationStages"]["first"],first);assert_eq!(job["preparationStages"]["firstAdmission"],reservation);
    tokio::time::timeout(std::time::Duration::from_secs(2),async{loop{
        let handles=app.tasks.lock().await;if let Some(task)=handles.get(run){task.abort();break;}drop(handles);tokio::task::yield_now().await;
    }}).await.unwrap();drop(lane);tokio::task::yield_now().await;assert_no_model(&app);app.db.close().await;
}
