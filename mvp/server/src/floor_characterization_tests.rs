//! Original-libtest PG/CAS characterization, selected explicitly by root.
//! No fixture lifecycle bootstrap, SQL seeding, model, provider or ASR calls.
use crate::*;
use crate::runtime_lifecycle_startup::fixture_producer as producer;
use sqlx::{Connection,Row};
use std::str::FromStr;

fn fields(v:&Value,keys:&[&str])->ApiResult<()> {
    if v.as_object().is_none_or(|o|o.len()!=keys.len()||keys.iter().any(|k|!o.contains_key(*k))) {
        return Err(bad("Closed native floor fixture contract required"));
    }Ok(())
}
fn reject_unsettled(d:&Value)->ApiResult<()> {
    if list(d,"jobs").iter().any(|j|matches!(j["status"].as_str(),Some("running"|"queued"))) {
        return Err(conflict("Prior native floor fixture work must be quiescent"));
    }Ok(())
}
fn protected_rows(before:&Value,after:&Value)->ApiResult<()> {
    for table in ["jobs","operations","approvals","audit","materials","knowledge_entries","knowledge_versions","feedback"] {
        for old in list(before,table) {
            if row(after,table,required(old,"id")?)?!=old {
                return Err(conflict("Original protected fixture row changed"));
            }
        }
    }Ok(())
}
pub(super) fn material_invocation(request:&Value,result:&mut Value) {
    let mut body=model_material_receipt::expected(request);
    body["schemaVersion"]=json!(1);body["contract"]=json!(preparation_materials::CONTRACT);body["completenessStatus"]=json!("complete");
    for(key,source)in[("actualTextInputSha256","inputSha256"),("instructionSha256","instructionSha256"),("cliSha256","cliSha256")] {
        body[key]=result["runMetadata"][source].clone();
    }
    body["schemaSha256"]=json!(media_fullframes::hash(&json!({"fixture":"source-bound-synthetic-material-delivery-v1"})));
    body["stagedPhotos"]=json!(preparation_materials::rows(&body,"requiredPhotos").iter().map(|r| {
        let photo=preparation_materials::rows(&request["postContextBundle"],"members").iter().find(|m|m["canonicalPostId"]==r["postId"])
            .and_then(|m|preparation_materials::rows(m,"assets").iter().find(|a|a["attachmentIndex"]==r["attachmentIndex"])).map(|a|&a["photo"]);
        json!({"postId":r["postId"],"attachmentIndex":r["attachmentIndex"],"sha256":r["artifact"]["sha256"],"bytes":r["artifact"]["bytes"],
            "width":photo.map(|p|p["width"].clone()),"height":photo.map(|p|p["height"].clone())})
    }).collect::<Vec<_>>());body["deliveredPhotos"]=body["stagedPhotos"].clone();
    body["optionalFrameRefs"]=request.get("optionalFrameRefs").cloned().unwrap_or(json!([]));
    body["stagedFrames"]=json!(preparation_materials::rows(request,"optionalFrameRefs").iter().map(|r|json!({"refSha256":media_fullframes::hash(r),
        "sha256":r["sha256"],"mime":r["mime"],"width":r["width"],"height":r["height"],"bytes":r["artifact"]["bytes"],
        "requestedTimestampMs":r["requestedTimestampMs"],"actualPts":r["actualPts"],"timeBase":r["timeBase"]})).collect::<Vec<_>>());
    body["deliveredFrames"]=body["stagedFrames"].clone();result["runMetadata"]["materialInvocation"]=body;
}

fn validate_fixture_snapshot(d:&Value,snapshot:&Value,fresh:bool)->ApiResult<()> {
    fields(snapshot,&["posts","branches","items"])?;
    if d["account"]!="BAW Russia" {return Err(conflict("Native floor fixture must belong to BAW Russia"));}
    let binding=active_binding(d)?.to_json();
    fn native_proof(v:&Value)->bool {
        match v {
            Value::Object(o)=>o.iter().any(|(key,value)|[
                "jobs","operations","approvals","materials","knowledge_entries","knowledge_versions","feedback",
                "photoAcquisition","commentPhotoAcquisition","visualProgress","retainedEvidence","modelMaterialReceipts",
                "runtimeLifecycle","mediaPolicy","preparationMediaPolicy",
            ].contains(&key.as_str())||native_proof(value)),
            Value::Array(a)=>a.iter().any(native_proof), _=>false,
        }
    }
    for table in ["posts","branches","items"] {
        let values=snapshot[table].as_array().filter(|a|!a.is_empty()&&a.len()<=64)
            .ok_or_else(||bad("Bounded native source rows required"))?;
        let mut ids=std::collections::HashSet::new();
        for value in values {
            let key=required(value,"id")?;
            if !value.is_object()||native_proof(value)||!ids.insert(key)||key.len()>160
                ||!knowledge::in_account(value,"BAW Russia")
                ||value.get("connectorBinding").is_some_and(|b|*b!=binding)
                ||fresh&&list(d,table).iter().any(|old|old["id"]==key)
            {return Err(conflict("Native fixture source identity or proof invalid"));}
        }
    }
    for branch in list(snapshot,"branches") {
        if !list(snapshot,"posts").iter().any(|post|post["id"]==branch["postId"])
            ||branch["contextComplete"]!=true||!branch["messages"].is_array()
        {return Err(bad("Exact fixture branch/post context required"));}
    }
    for item in list(snapshot,"items") {
        let post=list(snapshot,"posts").iter().find(|post|post["id"]==item["postId"])
            .ok_or_else(||bad("Fixture item post missing"))?;
        if item["postKey"]!=post["postKey"]||required(item,"itemId")?.is_empty()
            ||!list(snapshot,"branches").iter().any(|branch|branch["id"]==item["branchId"]&&branch["postId"]==post["id"])
            ||item.get("draft").is_some_and(|value|value!="")
            ||item.get("workflow").is_some_and(|value|value!="attention")
        {return Err(bad("Exact unprepared fixture item required"));}
    }
    Ok(())
}
fn derive_fixture_speech(d:&Value,input:&Value,manual_inputs:&[Value])->ApiResult<Value> {
    fields(input,&["id","account","kind","postKey","sourceUrl","text","transcription","classification"])?;
    fields(&input["transcription"],&["partial","audioStatus","coverage","sourceVersion","mediaDurationSeconds","audioDurationSeconds"])?;
    let post=list(d,"posts").iter().find(|post|post["postKey"]==input["postKey"])
        .ok_or_else(||bad("Speech post missing"))?;
    let version=media_fullframes::source_version(post,"BAW Russia");
    if manual_inputs.iter().any(|body|body["postId"]==post["id"])
        ||input["account"]!="BAW Russia"||input["kind"]!="transcript"
        ||input["classification"]!="SYNTHETIC-NATIVE-CORPUS"
        ||post["sourceUrl"].as_str().is_none_or(|s|s.is_empty())||input["sourceUrl"]!=post["sourceUrl"]
        ||input["transcription"]["coverage"]!="full_audio"||input["transcription"]["audioStatus"]!="transcribed"
        ||input["text"].as_str().is_none_or(|s|s.trim().is_empty())
    {return Err(bad("Separate synthetic complete-speech fixture required"));}
    let mut speech=input.clone();
    if speech["transcription"]["sourceVersion"]=="NATIVE-CAPTURED" {
        speech["transcription"]["sourceVersion"]=json!(version);
    }
    if !knowledge::proven_full_audio(&speech["transcription"],&version) {
        return Err(bad("Exact complete fixture speech required"));
    }
    speech["sourceOrigin"]=json!("synthetic-complete-speech-fixture");
    Ok(speech)
}

async fn corpus(app:&App,input:&Value,record:&Value,store:&media_artifacts::ArtifactStore)->ApiResult<Value> {
    fields(input,&["snapshot","transcripts","photoPostIds","manualRequests","answer"])?;
    let manual_inputs=input["manualRequests"].as_array().filter(|v|v.len()==1)
        .ok_or_else(||bad("Exactly one bounded manual corpus request required"))?;
    let before=app.db.read().await?;reject_unsettled(&before)?;
    validate_fixture_snapshot(&before,&input["snapshot"],false)?;
    if !list(&before,"operations").iter().any(|op|op["status"]=="unknown") {
        return Err(conflict("Root must supply genuinely native protected UNKNOWN corpus before population"));
    }
    app.change_source_snapshot_scoped(storage::SourceReadIntent::Snapshot(&input["snapshot"]),|d| {
        if runtime_lifecycle::ledger_digest(d)?!=record["preLedgerSha256"].as_str().unwrap() {return Err(conflict("Corpus pre-ledger drift"));}
        source_snapshot_scoped::merge_snapshot(d,&input["snapshot"])
    }).await?;
    app.change(|d| {
        for speech in input["transcripts"].as_array().filter(|v|!v.is_empty()).ok_or_else(||bad("Exact native speech corpus required"))? {
            let speech=derive_fixture_speech(d,speech,manual_inputs)?;
            let key=required(&speech,"id")?;
            if list(d,"materials").iter().any(|m|m["id"]==key) {return Err(conflict("Corpus material already exists"));}
            list_mut(d,"materials").push(speech);
        }
        knowledge::sync_catalog(d,&now()).map_err(bad)?;
        for post in input["photoPostIds"].as_array().filter(|v|!v.is_empty()).ok_or_else(||bad("Native photo corpus required"))? {
            photo_acquisition::fixture_commit_photo_in(d,post.as_str().ok_or_else(||bad("Photo post required"))?,&now(),store)?;
        }Ok(())
    }).await?;
    // Real root-owned FFmpeg sampling of an ALREADY retained genuine source.
    // from_env pins/version-checks actual tools; no fake decoder adapter exists.
    let tools=media_frame_sample_decode::SampleTools::from_env(&app.lifecycle_work,Duration::from_secs(30)).await.map_err(|e|conflict(&e))?;
    let token=app.lifecycle_admission_token(runtime_lifecycle::AdmissionClass::Preparation).await?;
    let mut manual=Vec::new();let mut source_admissions=Vec::new();
    for input_body in manual_inputs {
        let current=app.db.read().await?;
        let (_,pin)=manual_frame_request::fixture_native_request(&current,input_body)?;drop(current);
        let generated=media_frame_sample_decode::fixture_generate_pinned_source(store,&pin,&tools,&app.lifecycle_work)
            .await.map_err(|e|conflict(&e))?;
        let source=app.change(|d| {
            let admission=manual_frame_request::fixture_retain_generated_source(d,input_body,&generated,&token,&tools,store,&now())?;
            let post=row(d,"posts",required(input_body,"postId")?)?;
            let material=json!({"id":id(),"account":"BAW Russia","kind":"transcript","classification":"SYNTHETIC-NATIVE-CORPUS",
                "sourceOrigin":"genuine-local-ffmpeg-testsrc","postKey":post["postKey"],"sourceUrl":post["sourceUrl"],
                "text":"[Actual generated fixture inspected: no audio stream.]",
                "transcription":{"partial":false,"coverage":"no_audio_stream","audioStatus":"no_audio_stream",
                    "sourceVersion":pin["sourceVersion"],"mediaDurationSeconds":admission["observation"]["durationMs"].as_u64().unwrap() as f64/1000.0,
                    "audioDurationSeconds":null},"nativeSourceObservation":admission["observation"]["observationArtifact"]});
            if !knowledge::proven_full_audio(&material["transcription"],required(&pin,"sourceVersion")?) {
                return Err(conflict("Generated no-audio source coverage invalid"));
            }
            list_mut(d,"materials").push(material);knowledge::sync_catalog(d,&now()).map_err(bad)?;Ok(admission)
        }).await?;
        let body=&source["request"];
        let fixture=app.change(|d|manual_frame_request::fixture_begin_pinned(d,body,"root-native-floor-fixture",&token,&tools,store,&now())).await?;
        let decoded=media_frame_sample_decode::decode_sample(store,&fixture["plan"],&tools,&app.lifecycle_work).await.map_err(|e|conflict(&e))?;
        manual.push(app.change(|d|manual_frame_request::fixture_settle_pinned(d,&fixture,&decoded,&token,&tools,store,&now())).await?);
        source_admissions.push(source);
    }
    let item=required(&record["fixture"],"itemId")?.to_owned();
    let scheduled=app.change_preparation_schedule(|d| {
        let scheduled=engine_prepare::schedule(d,engine_prepare::Input{item_ids:vec![item.clone()],instruction:None})?;
        preparation_review::record_initial_admission(d,&token,&scheduled.job_id,&now())?;Ok(scheduled)
    }).await?;
    let request=scheduled.request().ok_or_else(||conflict("Corpus item must be fully ready"))?.clone();let run=&scheduled.job_id;
    app.change_preparation_first(run,|d|preparation_review::reserve_first_admitted(d,&token,run,&request,&now())).await?;
    let mut wire=request.clone();app.account.bind_request(&mut wire)?;wire["operation"]=json!("assistant");
    let mut response=engine_prepare::tests::single_pass_result(input["answer"].clone());material_invocation(&wire,&mut response);
    model_material_receipt::validate_result(&wire,&response).map_err(bad)?;
    let paid=runtime_lifecycle_app::with_job(run.clone(),runtime_paid_result::retain(app,"assistant",&wire,&response)).await?
        .ok_or_else(||conflict("Native corpus paid capture required"))?;
    app.change_job(run,|d|runtime_paid_result::attach(d,run,&paid,&app.lifecycle_owner)).await?;
    let material=model_material_receipt::retain(app,&wire,&response,&paid).await?.ok_or_else(||conflict("Native corpus material capture required"))?;
    app.change_job(run,|d|model_material_receipt::attach(d,run,&material)).await?;
    response["modelMaterialReceipt"]=material.clone();
    let admitted=app.change_preparation_first(run,|d| {
        if preparation_review::settle_first(d,run,&request,&response,&now())?.is_some() {return Err(conflict("Corpus must use complete single-pass response"));}
        prepare_bundle::admit_to(d,run,None,&response)
    }).await?;
    // Same normal durable completion reducer as production workers.
    app.finish(run,Ok(admitted)).await;
    let after=app.db.read().await?;protected_rows(&before,&after)?;
    let capture=runtime_paid_result::resolve(app,run,"assistant",&paid).await?;
    let mut original_response=response.clone();original_response.as_object_mut().unwrap().remove("modelMaterialReceipt");
    if capture["request"]!=wire||capture["response"]!=original_response {
        return Err(conflict("Corpus immutable paid bytes changed"));
    }
    Ok(json!({"kind":"native-original-floor-corpus-observation","classification":"SYNTHETIC-NATIVE-CORPUS","account":"baw-russia", "before":before,"after":after,
        "paidJobId":run,"paidReference":paid,"materialPointer":material,"manualJobs":manual,"sourceAdmissions":source_admissions,"fixtureInput":input,"syntheticResponseEmissions":1,"realModelEffects":0,"providerEffects":0,"asrEffects":0}))
}

async fn local_operation(app:&App,input:&Value,store:&media_artifacts::ArtifactStore,synthetic_unknown:bool)->ApiResult<Value> {
    fields(input,&["itemId","text","photoPostId"])?;
    let item_id=required(input,"itemId")?.to_owned();let text=required(input,"text")?;
    let local_gate=app.change(|d| {
        let before=d.get(connection_gate::FIELD).cloned().unwrap_or(Value::Null);
        // Explicit cfg(test) synthetic local gate fixture uses ordinary
        // initialize/reopen reducers. It claims no real auth/provider handoff.
        connection_gate::fixture_open(d)?;
        Ok(json!({"classification":"SYNTHETIC-LOCAL-GATE-OPEN","before":before,"after":d[connection_gate::FIELD]}))
    }).await?;
    let media=app.change(|d|photo_acquisition::fixture_commit_photo_in(d,required(input,"photoPostId")?,&now(),store)).await?;
    let item=app.db.read().await?;
    let item=row(&item,"items",&item_id)?;
    let draft_request=json!({"expectedRevision":item["revision"],"draft":text,"draftEdited":true});
    let draft=app.change_item(&item_id,|d|patch_item(d,&item_id,&draft_request)).await?;
    let proposal_request=json!({"itemId":item_id,"expectedRevision":draft["revision"],"kind":"reply_and_close","text":text});
    let proposal=app.create_proposal(&proposal_request).await?;
    let key=required(&proposal,"id")?.to_owned();let refs=json!([{"id":key,"revision":proposal["revision"]}]);
    let actor=operator_auth::Actor::local_owner("synthetic-native-local-floor");
    let review=app.change(|d| {
        let preview=operator_editorial::capture(d,&actor,&json!({"proposals":refs}))?;
        let entries=preview["entries"].as_array().ok_or_else(||bad("Native exact operator preview required"))?.iter().map(|entry| {
            let mut judgment=json!({"candidate":entry["candidate"],"checks":{"companyRules":"pass","intent":"pass","factualScope":"pass"},
                "reason":"Explicit synthetic native local fixture judgment; no human or model review claim"});
            if decision_media::enabled(&entry["candidate"]) {judgment["mediaDependency"]=json!({"audio":"independent","visual":"independent"});}judgment
        }).collect::<Vec<_>>();
        let body=json!({"requestId":format!("floor-review-{}",key),"proposals":refs,"previewDigest":preview["previewDigest"],
            "operatorReview":{"version":1,"method":operator_editorial::METHOD,"entries":entries}});
        let admitted=operator_editorial::admit(d,&actor,&body)?;Ok(json!({"preview":preview,"request":body,"result":admitted}))
    }).await?;
    let approval_request=json!({"requestId":format!("floor-approval-{}",key),"proposals":refs});
    let approval=app.change_admission(storage::AdmissionScope::Approval(&approval_request),|d|create_approval(d,&actor,&approval_request)).await?;
    let approval_id=required(&approval,"id")?.to_owned();let execute_request=json!({"approvalId":approval_id,"requestId":format!("floor-execute-{}",key)});
    let evaluated=app.change_admission(storage::AdmissionScope::Execute{approval:&approval_id,body:&execute_request},|d|execute_admission::evaluate(d,&actor,&approval_id,&execute_request)).await?;
    let (admission,job,operations)=match evaluated {
        execute_admission::Evaluation::Admitted(result,Some((job,operations)))=>(result,job,operations),
        _=>return Err(conflict("Native positive must admit one NEW guarded local operation")),
    };
    if operations.len()!=1 {return Err(conflict("Exactly one approved local floor operation required"));}
    let admitted=app.db.read().await?;
    if synthetic_unknown {
        // Explicit test-only uncertain boundary. No network call occurred; this
        // exercises the same durable receipt/outcome reducer as dispatch.
        if app.external_writes||app.check_execution().is_ok() {return Err(conflict("Synthetic UNKNOWN requires disabled real transport"));}
        let receipt=json!({"classification":"SYNTHETIC-NATIVE-CORPUS","simulatedNetworkBoundary":true,
            "providerCallAttempted":false,"providerEffects":0,"mutationOutcome":"synthetic-uncertain"});
        let evidence=json!({"classification":"SYNTHETIC-NATIVE-CORPUS","receipt":receipt,
            "verificationPhase":"synthetic-unconfirmed","providerCallAttempted":false,"providerRetryAllowed":false});
        let op=&operations[0];
        app.change_execute_transition(op,receipt.clone(),"unknown",|d|apply_operation_outcome(d,op,"unknown",evidence)).await?;
        app.finish(&job,Err(conflict("Synthetic uncertainty retained; retry forbidden"))).await;
        let after=app.db.read().await?;let stored=row(&after,"operations",required(op,"id")?)?;
        if stored["status"]!="unknown"||stored["executeReceipt"]!=receipt||stored["evidence"]["providerRetryAllowed"]!=false
            ||row(&after,"proposals",&key)?["status"]!="unknown"||row(&after,"jobs",&job)?["status"]!="failed"
            ||!stored[connection_gate::PERMIT_FIELD].is_null() {return Err(conflict("Native UNKNOWN settlement evidence incomplete"));}
        return Ok(json!({"kind":"native-approved-synthetic-unknown-operation-observation","classification":"SYNTHETIC-NATIVE-CORPUS",
            "itemId":item_id,"localGate":local_gate,"executed":false,"photoClaimCommit":media,"draftRequest":draft_request,"draft":draft,
            "proposalRequest":proposal_request,"proposal":proposal,"operatorReview":review,"approvalRequest":approval_request,"approval":approval,
            "executeRequest":execute_request,"admission":admission,"admittedWorkspace":admitted,"operation":stored,"providerEffects":0,"modelEffects":0}));
    }
    let error=match app.check_execution(){Err(error)=>error,Ok(())=>return Err(conflict("Native local floor must retain operator-disabled transport"))};
    if error.0!=StatusCode::FORBIDDEN {return Err(conflict("Actual operator-disabled guard required"));}
    // The real production transport guard is evaluated before ANY bridge call.
    // Persist its conclusive no-attempt stop through the ordinary outcome path.
    let outcome=dispatch_diagnostics::persist_stop(app,&operations[0],dispatch_diagnostics::read_failure("native_fixture_execution_disabled",&error)).await?;
    if outcome!=dispatch_diagnostics::Outcome::Failed {return Err(conflict("Guarded local operation must remain failed/unsent"));}
    app.finish(&job,Err(error)).await;
    let after=app.db.read().await?;let stored=row(&after,"operations",required(&operations[0],"id")?)?;
    if stored["status"]!="failed"||stored["evidence"]["providerCallAttempted"]!=false||stored["evidence"]["mutationOutcome"]!="not-attempted"
        ||!stored["executeReceipt"].is_null()||!stored[connection_gate::PERMIT_FIELD].is_null()||row(&after,"jobs",&job)?["status"]!="failed" {
        return Err(conflict("Guarded native operation cannot claim a provider effect or dispatch permit"));
    }
    Ok(json!({"kind":"native-approved-local-guarded-operation-observation","classification":"SYNTHETIC-NATIVE-CORPUS","itemId":item_id,"localGate":local_gate,"executed":false,
        "photoClaimCommit":media,"draftRequest":draft_request,"draft":draft,"proposalRequest":proposal_request,"proposal":proposal,"operatorReview":review,
        "approvalRequest":approval_request,"approval":approval,"executeRequest":execute_request,"admission":admission,"admittedWorkspace":admitted,"operation":stored,
        "executionGuardHttpStatus":403,"providerEffects":0,"modelEffects":0}))
}

async fn unknown_seed(app:&App,input:&Value,store:&media_artifacts::ArtifactStore)->ApiResult<Value> {
    fields(input,&["snapshot","itemId","text","photoPostId"])?;
    let before=app.db.read().await?;reject_unsettled(&before)?;
    if list(&before,"operations").iter().any(|op|op["status"]=="unknown") {return Err(conflict("UNKNOWN seed must not repeat or replace prior uncertainty"));}
    validate_fixture_snapshot(&before,&input["snapshot"],true)?;
    let item=list(&input["snapshot"],"items").iter().find(|item|item["id"]==input["itemId"])
        .ok_or_else(||bad("UNKNOWN seed item absent from source snapshot"))?;
    let photo=list(&input["snapshot"],"posts").iter().find(|post|post["id"]==input["photoPostId"])
        .ok_or_else(||bad("UNKNOWN seed photo absent from source snapshot"))?;
    if item["postId"]!=photo["id"]||!list(photo,"attachments").iter().any(|a|a["type"]=="photo") {
        return Err(bad("UNKNOWN seed exact item/photo required"));
    }
    // Only ordinary source rows are merged; native operations/proofs cannot be injected.
    app.change_source_snapshot_scoped(storage::SourceReadIntent::Snapshot(&input["snapshot"]),|d| {
        if runtime_lifecycle::ledger_digest(d)?!=runtime_lifecycle::ledger_digest(&before)? {
            return Err(conflict("UNKNOWN seed pre-ledger drift"));
        }
        reject_unsettled(d)?;
        if list(d,"operations").iter().any(|op|op["status"]=="unknown") {return Err(conflict("UNKNOWN seed must not repeat or replace prior uncertainty"));}
        validate_fixture_snapshot(d,&input["snapshot"],true)?;
        source_snapshot_scoped::merge_snapshot(d,&input["snapshot"])
    }).await?;
    let operation_input=json!({"itemId":input["itemId"],"text":input["text"],"photoPostId":input["photoPostId"]});
    let operation=local_operation(app,&operation_input,store,true).await?;
    let after=app.db.read().await?;protected_rows(&before,&after)?;
    if list(&after,"operations").iter().filter(|op|op["status"]=="unknown").count()!=1 {return Err(conflict("Exactly one native protected UNKNOWN required"));}
    Ok(json!({"kind":"native-original-floor-unknown-seed-observation","classification":"SYNTHETIC-NATIVE-CORPUS",
        "before":before,"after":after,"fixtureInput":input,"operation":operation,"providerEffects":0,"modelEffects":0,"asrEffects":0}))
}

async fn writes(app:&App,input:&Value,store:&media_artifacts::ArtifactStore)->ApiResult<Value> {
    fields(input,&["snapshot","probeJobId","proposalId","positive"])?;
    let before=app.db.read().await?;reject_unsettled(&before)?;
    let probe=row(&before,"jobs",required(input,"probeJobId")?)?["prepareBundle"].clone();
    row(&before,"proposals",required(input,"proposalId")?)?;
    let cache_before=app.bootstrap_cache.current_version();let mut events=app.events.subscribe();
    let (accepted,trace)=performance::capture(app.change_source_snapshot_scoped(storage::SourceReadIntent::Snapshot(&input["snapshot"]),|d|source_snapshot_scoped::merge_snapshot(d,&input["snapshot"]))).await;
    accepted?;let after=app.db.read().await?;protected_rows(&before,&after)?;
    if before==after {return Err(conflict("Useful source write required"));}
    let cache_after=app.bootstrap_cache.current_version();if cache_before==cache_after||events.try_recv().is_err()||events.try_recv().is_ok() {return Err(conflict("One committed cache/event invalidation required"));}
    app.change_source_snapshot_scoped(storage::SourceReadIntent::Snapshot(&input["snapshot"]),|_|Ok(())).await?;
    if app.bootstrap_cache.current_version()!=cache_after||events.try_recv().is_ok() {return Err(conflict("No-op invalidated cache"));}
    let positive=local_operation(app,&input["positive"],store,false).await?;
    loop {match events.try_recv() {
        Ok(_)|Err(broadcast::error::TryRecvError::Lagged(_))=>{},
        Err(broadcast::error::TryRecvError::Empty)=>break,
        Err(broadcast::error::TryRecvError::Closed)=>return Err(conflict("Native event channel unexpectedly closed")),
    }}
    let after=app.db.read().await?;protected_rows(&before,&after)?;
    let proposal=row(&after,"proposals",required(&positive["proposal"],"id")?)?.clone();
    let token=app.lifecycle_admission_token(runtime_lifecycle::AdmissionClass::Preparation).await?;
    let mut rejects=Vec::new();
    for case in ["stale-epoch","stale-source","stale-media","wrong-text","wrong-recipient","foreign-company","missing-material","tampered-material"] {
        let old=app.db.read().await?;let version=app.bootstrap_cache.current_version();
        let mut attempted=Value::Null;
        let failure:ApiResult<()>=app.change_source_snapshot_scoped(storage::SourceReadIntent::Snapshot(&input["snapshot"]),|d| {
            match case {
                "stale-epoch"=>{let mut stale=token.clone();stale.epoch+=1;attempted=json!({"owner":{"account":stale.account,"runtimeId":stale.runtime_id,"releaseSha256":stale.release_sha256,"epoch":stale.epoch}});runtime_lifecycle::require_admission(d,&stale,runtime_lifecycle::AdmissionClass::Preparation)?;},
                "stale-source"=>{attempted=probe.clone();prepare_bundle::current(d,&probe).map_err(conflict)?;},
                "stale-media"=>{let post=required(&input["positive"],"photoPostId")?;row_mut(d,"posts",post)?["photoAcquisition"]=Value::Null;
                    attempted=json!({"postId":post,"removedPhotoAcquisition":true,"proposal":proposal});proposal_current(d,&proposal)?;},
                "missing-material"|"tampered-material"=>{let mut p=proposal.clone();
                    if case=="missing-material" {for field in ["operatorMaterialReceipt","modelMaterialReceipt","editorialModelMaterialReceipt"] {p.as_object_mut().unwrap().remove(field);}}
                    else {p["operatorMaterialReceipt"]["receiptSha256"]=json!("0".repeat(64));}
                    attempted=p.clone();proposal_current(d,&p)?;},
                _=>{let mut p=proposal.clone();match case {"wrong-text"=>p["text"]=json!("Changed exact candidate"),"wrong-recipient"=>p["itemId"]=json!("foreign-recipient"),_=>p["routeTarget"]["connectorBinding"]["accountId"]=json!("LikeAvto")};attempted=p.clone();proposal_current(d,&p)?;},
            }
            Err(conflict("Connected negative unexpectedly admitted"))
        }).await;
        let error=failure.expect_err("negative must fail");
        // A sentinel failure would hide a disconnected guard; forbid it.
        if error.1=="Connected negative unexpectedly admitted" {return Err(conflict("Negative guard did not reject at connected boundary"));}
        let saved=app.db.read().await?;if saved!=old||app.bootstrap_cache.current_version()!=version||events.try_recv().is_ok() {return Err(conflict("Rejected writer changed durable state/cache"));}
        rejects.push(json!({"case":case,"httpStatus":error.0.as_u16(),"attemptedRequest":attempted,"before":old,"after":saved,"cacheBefore":version,"cacheAfter":app.bootstrap_cache.current_version()}));
    }
    let occupied=Database::postgres(&std::env::var("COMMUNITYHERO_DATABASE_URL").map_err(|_|bad("Existing fixture URL required"))?).await;
    if let Ok(db)=occupied {db.close().await;return Err(conflict("Second PG owner acquired occupied native lease"));}
    let after_lease=app.db.read().await?;
    if after_lease!=after {return Err(conflict("Rejected second owner changed workspace"));}
    rejects.push(json!({"case":"second-owner","httpStatus":409,"before":after,"after":after_lease,"cacheBefore":app.bootstrap_cache.current_version(),"cacheAfter":app.bootstrap_cache.current_version()}));
    let route=if trace.iter().any(|e|e["stage"]=="source.snapshot.clone_and_domain") {"full-source-writer"}
        else if trace.iter().any(|e|e["stage"]=="source.snapshot.writer.acquire") {"current-source-writer"}else{return Err(conflict("Actual native source writer route was not observed"));};
    let mut authorized=Vec::new();for table in ["jobs","operations","approvals","audit","materials","knowledge_entries","knowledge_versions","feedback"] {
        for value in list(&after,table){if !list(&before,table).iter().any(|old|old["id"]==value["id"]){authorized.push(format!("{table}/{}",required(value,"id")?));}}
    }
    Ok(json!({"kind":"native-original-floor-write-cache-fencing-observation","account":"baw-russia","before":before,"after":after,"observedRoute":route,
        "performanceEvents":trace,"cache":{"beforeVersion":cache_before,"afterVersion":cache_after,"noopVersion":cache_after},"positive":positive,"authorizedDeltaIds":authorized,"rejections":rejects,"secondOwnerRejected":true}))
}

async fn selected(selector:&str)->ApiResult<()> {
    let verified=producer::verified_fixture_producer_from_environment()?;verified.require_selector(selector)?;
    let record=verified.record.clone();let input=verified.response.clone();let output=verified.barrier.clone();let cas_root=verified.cas_root.clone();let limit=verified.timeout_ms;
    let url=std::env::var("COMMUNITYHERO_DATABASE_URL").map_err(|_|bad("Existing isolated database URL required"))?;
    let app=verified.acquire_app(&url).await?;let store=media_fullframes::store().map_err(|e|conflict(&e))?;
    if store.root()!=cas_root||app.node.exists()||app.bridge.exists() {app.db.close().await;return Err(conflict("Pinned corpus CAS or forbidden bridge changed"));}
    let result=tokio::time::timeout(Duration::from_millis(limit),photo_acquisition::with_fixture_store(&store,async {
        if selector==producer::UNKNOWN_SELECTOR {unknown_seed(&app,&input,&store).await}else if selector==producer::CORPUS_SELECTOR {corpus(&app,&input,&record,&store).await}else{writes(&app,&input,&store).await}
    })).await;
    app.db.close().await;
    let observation=result.map_err(|_|conflict("Native floor fixture deadline: unresolved attempt"))??;
    producer::write_new(&output,&json!({"kind":"native-original-floor-fixture-observation","selector":selector,"admission":record,"observation":observation}))?;
    println!("WAVE_NATIVE_FLOOR_OBSERVATION={}",output.display());Ok(())
}

#[tokio::test]
#[ignore="ROOT original native artifact, SAME populated isolated BAW PG/CAS; real pinned offline FFmpeg only"]
async fn emit_mixed_native_floor_corpus(){selected(producer::CORPUS_SELECTOR).await.unwrap();}
#[tokio::test]
#[ignore="ROOT original native artifact, SAME isolated BAW PG/CAS; synthetic uncertainty, no provider"]
async fn emit_native_unknown_floor_seed(){selected(producer::UNKNOWN_SELECTOR).await.unwrap();}
#[tokio::test]
#[ignore="ROOT original native artifact, SAME populated isolated BAW PG/CAS; connected writer/cache/fencing"]
async fn characterize_full_source_write_cache_and_fencing(){selected(producer::WRITE_SELECTOR).await.unwrap();}

#[tokio::test]
#[ignore="ROOT original libtest role, independent read-only SAME isolated BAW PG/CAS observation"]
async fn emit_read_only_native_floor_snapshot(){read_only_snapshot().await.unwrap();}
async fn read_only_snapshot()->ApiResult<()> {
    let file=std::env::var("COMMUNITYHERO_FLOOR_READER_INPUT_PATH").map_err(|_|bad("Pinned native reader input required"))?;
    let expected=std::env::var("COMMUNITYHERO_FLOOR_READER_INPUT_SHA256").map_err(|_|bad("Pinned native reader hash required"))?;
    let bytes=producer::read_fixture_input(&file,&expected)?;
    let input:Value=serde_json::from_slice(&bytes).map_err(|_|bad("Native reader input invalid"))?;
    fields(&input,&["schemaVersion","kind","original","account","fixedOwner","database","casRoot","output","createdAt","expiresAt"])?;
    if input["schemaVersion"]!=1||input["kind"]!="root-native-floor-read-only-snapshot-input"||input["account"]!="baw-russia" {
        return Err(bad("Closed native read-only input required"));
    }
    producer::verify_original_test_role(&input["original"],&std::env::current_exe().map_err(|_|bad("Native reader role unavailable"))?)?;
    let created=chrono::DateTime::parse_from_rfc3339(required(&input,"createdAt")?).map_err(|_|bad("Native reader time invalid"))?;
    let expires=chrono::DateTime::parse_from_rfc3339(required(&input,"expiresAt")?).map_err(|_|bad("Native reader time invalid"))?;
    if created>chrono::Utc::now()||expires<=chrono::Utc::now()||expires-created>chrono::Duration::minutes(2) {return Err(conflict("Native reader grant expired"));}
    let url=std::env::var("COMMUNITYHERO_DATABASE_URL").map_err(|_|bad("Isolated reader URL required"))?;
    let options=sqlx::postgres::PgConnectOptions::from_str(&url).map_err(|_|bad("Isolated reader URL invalid"))?;let database=&input["database"];
    if options.get_host()!="127.0.0.1"||options.get_host()!=required(database,"host")?||u64::from(options.get_port())!=database["port"].as_u64().unwrap_or(0)
        ||options.get_database()!=Some(required(database,"name")?)||options.get_username()!=required(database,"role")? {return Err(conflict("Native reader database rebound"));}
    let mut connection=sqlx::PgConnection::connect_with(&options).await?;
    let identity:ApiResult<Value>=async {
        sqlx::query("SET default_transaction_read_only=on").execute(&mut connection).await?;
        let row=sqlx::query("SELECT current_database() AS db,current_user AS role,inet_server_addr()::text AS host,inet_server_port() AS port,(SELECT system_identifier::text FROM pg_control_system()) AS cluster")
            .fetch_one(&mut connection).await?;
        Ok(json!({"name":row.try_get::<String,_>("db")?,"role":row.try_get::<String,_>("role")?,"host":row.try_get::<String,_>("host")?,"port":row.try_get::<i32,_>("port")?,"clusterSystemId":row.try_get::<String,_>("cluster")?}))
    }.await;
    let closed=connection.close().await;let observed=identity?;closed?;
    for key in ["name","role","host","port","clusterSystemId"] {if observed[key]!=database[key] {return Err(conflict("Native reader cluster/database changed"));}}
    let workspace=storage::read_native_fixture_workspace_snapshot(&url,accounts::Profile::BawRussia).await?;
    let owner=runtime_lifecycle::parse_token(&input["fixedOwner"])?;
    let identity=runtime_lifecycle::RuntimeIdentity{account:owner.account.clone(),runtime_id:owner.runtime_id.clone(),release_sha256:owner.release_sha256.clone()};
    if runtime_lifecycle::current_owner(&workspace,&identity)?!=owner {return Err(conflict("Native reader owner/epoch changed"));}
    let store=media_fullframes::store().map_err(|_|conflict("Native reader CAS unavailable"))?;
    let root=std::fs::canonicalize(required(&input,"casRoot")?).map_err(|_|bad("Native reader CAS required"))?;
    if store.root()!=root {return Err(conflict("Native reader effective CAS rebound"));}
    let backends=storage::read_native_fixture_backend_cessation(&url).await?;
    let output=producer::fresh_fixture_output(required(&input,"output")?)?;
    let digest=runtime_lifecycle::ledger_digest(&workspace)?;
    let ledger=json!({"kind":"native-read-only-runtime-lifecycle-ledger-digest","status":"captured","account":"baw-russia","workspaceId":"local-pilot","expectedLedgerSha256":digest});
    producer::write_new(&output,&json!({"kind":"native-read-only-complete-workspace-snapshot","status":"captured","account":"baw-russia","workspaceId":"local-pilot",
        "input":{"path":file,"sha256":expected},"database":observed,"casRoot":root,"expectedLedgerSha256":digest,"workspace":workspace,"ledgerObservation":ledger,"backendObservation":backends}))?;
    println!("WAVE_NATIVE_READ_ONLY_SNAPSHOT={}",output.display());Ok(())
}

#[tokio::test]
async fn unknown_seed_rejections_leave_workspace_cache_and_events_unchanged() {
    for case in ["existing-unknown","queued","running","malformed"] {
        let (mut app,temp,_,op)=crate::post_network_transition_tests::fixture().await;
        app.external_writes=false;
        if case=="existing-unknown" {
            set_outcome(&app,&op,"unknown",json!({"classification":"SYNTHETIC-TEST-PREEXISTING","providerRetryAllowed":false})).await.unwrap();
        } else if case=="queued"||case=="running" {
            app.change(|d|{list_mut(d,"jobs").push(json!({"id":"unsettled-fixture","kind":"fixture","status":case}));Ok(())}).await.unwrap();
        }
        let store=media_artifacts::ArtifactStore::open(&temp.path().join("rejected-seed-cas")).unwrap();
        let mut input=json!({"snapshot":{"posts":[],"branches":[],"items":[]},"itemId":"item-1","text":"synthetic","photoPostId":"absent-must-not-read"});
        if case=="malformed" {input["unauthorizedExtra"]=json!(true);}
        let before=app.db.read().await.unwrap();let cache=app.bootstrap_cache.current_version();let mut events=app.events.subscribe();
        let error=unknown_seed(&app,&input,&store).await.unwrap_err();
        let expected=match case {"existing-unknown"=>"UNKNOWN seed must not repeat or replace prior uncertainty","malformed"=>"Closed native floor fixture contract required",_=>"Prior native floor fixture work must be quiescent"};
        assert_eq!(error.1,expected,"{case}: fail at connected precondition before admission");
        assert_eq!(app.db.read().await.unwrap(),before,"{case}");assert_eq!(app.bootstrap_cache.current_version(),cache,"{case}");
        assert!(matches!(events.try_recv(),Err(broadcast::error::TryRecvError::Empty)),"{case}");
        assert!(!app.node.exists()&&!app.bridge.exists());app.db.close().await;
    }
}


fn source_snapshot_fixture(d:&Value)->Value {
    let binding=active_binding(d).unwrap().to_json();
    json!({"posts":[{"id":"seed-post","postKey":"seed-post","objectId":"12182","platform":"VK",
            "account":"BAW Russia","connectorBinding":binding,"text":"Public synthetic source",
            "attachments":[{"type":"photo","url":"https://example.invalid/seed.png"}]}],
        "items":[{"id":"seed-item","itemId":"seed-comment","objectId":"12182","platform":"VK",
            "account":"BAW Russia","connectorBinding":binding,"postId":"seed-post","postKey":"seed-post",
            "conversationKey":"seed-thread","branchId":"seed-branch","targetId":"seed-comment",
            "revision":1,"draft":"","workflow":"attention","providerStatus":"new","text":"Public synthetic comment"}],
        "branches":[{"id":"seed-branch","postId":"seed-post","contextComplete":true,
            "messages":[{"id":"seed-comment","role":"customer","text":"Public synthetic comment"}]}]})
}
#[test]
fn source_snapshot_fixture_rejects_native_proof_foreign_binding_and_ambiguous_members() {
    let mut d=empty();accounts::initialize(&mut d,accounts::Profile::BawRussia).unwrap();
    let before=d.clone();let snapshot=source_snapshot_fixture(&d);
    validate_fixture_snapshot(&d,&snapshot,true).unwrap();
    for case in ["proof","account","binding","duplicate","post","branch","draft","collection"] {
        let mut value=snapshot.clone();
        match case {
            "proof"=>value["posts"][0]["photoAcquisition"]=json!({}),
            "account"=>value["items"][0]["account"]=json!("LikeAvto"),
            "binding"=>value["posts"][0]["connectorBinding"]["revision"]=json!(999),
            "duplicate"=>{let duplicate=value["items"][0].clone();value["items"].as_array_mut().unwrap().push(duplicate);},
            "post"=>value["items"][0]["postId"]=json!("absent"),
            "branch"=>value["items"][0]["branchId"]=json!("absent"),
            "draft"=>value["items"][0]["draft"]=json!("Asserted paid content"),
            _=>value["jobs"]=json!([]),
        }
        assert!(validate_fixture_snapshot(&d,&value,true).is_err(),"{case}");assert_eq!(d,before);
    }
}
#[tokio::test]
async fn unknown_initial_source_seed_uses_native_photo_and_one_protected_uncertain_operation() {
    let (app,temp)=manual_frame_request::native_app().await;
    let before=app.db.read().await.unwrap();
    let input=json!({"snapshot":source_snapshot_fixture(&before),"itemId":"seed-item",
        "text":"Explicit synthetic local approved answer","photoPostId":"seed-post"});
    let store=media_artifacts::ArtifactStore::open(&temp.path().join("unknown-native-cas")).unwrap();
    let observation=photo_acquisition::with_fixture_store(&store,unknown_seed(&app,&input,&store)).await.unwrap();
    let after=app.db.read().await.unwrap();protected_rows(&before,&after).unwrap();
    assert_eq!(observation["fixtureInput"],input);assert_eq!(observation["before"],before);
    assert_eq!(list(&after,"operations").iter().filter(|op|op["status"]=="unknown").count(),1);
    let op=&observation["operation"]["operation"];
    assert_eq!(op["status"],"unknown");assert_eq!(op["evidence"]["providerRetryAllowed"],false);
    let post=row(&after,"posts","seed-post").unwrap();
    let image=&post["photoAcquisition"]["images"][0];
    let reference=media_artifacts::ArtifactRef::from_json(&image["artifact"]).unwrap();
    let bytes=store.read_bytes(&reference,1024).unwrap();
    assert_eq!(&bytes[..8],b"\x89PNG\r\n\x1a\n");assert_eq!(image["width"],1);assert_eq!(image["height"],1);
    assert_eq!(observation["providerEffects"],0);assert_eq!(observation["modelEffects"],0);assert_eq!(observation["asrEffects"],0);
    assert!(!app.node.exists()&&!app.bridge.exists());app.db.close().await;
}
#[test]
fn synthetic_speech_derives_only_its_native_post_version_and_rejects_manual_noaudio_relabel() {
    let mut d=empty();accounts::initialize(&mut d,accounts::Profile::BawRussia).unwrap();
    d["posts"]=json!([{"id":"speech-post","postKey":"speech-post","sourceUrl":"https://example.invalid/speech.mp4",
        "account":"BAW Russia","attachments":[{"type":"video","url":"https://example.invalid/speech.mp4"}]}]);
    let input=json!({"id":"speech","account":"BAW Russia","kind":"transcript","postKey":"speech-post",
        "sourceUrl":"https://example.invalid/speech.mp4","text":"Explicit synthetic full speech fixture",
        "classification":"SYNTHETIC-NATIVE-CORPUS","transcription":{"partial":false,"audioStatus":"transcribed",
            "coverage":"full_audio","sourceVersion":"NATIVE-CAPTURED","mediaDurationSeconds":60.0,"audioDurationSeconds":60.0}});
    let observed=derive_fixture_speech(&d,&input,&[]).unwrap();
    assert_eq!(input["transcription"]["sourceVersion"],"NATIVE-CAPTURED");
    assert_eq!(observed["transcription"]["sourceVersion"],media_fullframes::source_version(&d["posts"][0],"BAW Russia"));
    assert!(derive_fixture_speech(&d,&input,&[json!({"postId":"speech-post"})]).is_err());
    for case in ["locator","classification","version","partial","coverage","extra"] {
        let mut value=input.clone();
        match case {
            "locator"=>value["sourceUrl"]=json!("https://example.invalid/other.mp4"),
            "classification"=>value["classification"]=json!("ACTUAL-ASR"),
            "version"=>value["transcription"]["sourceVersion"]=json!("a".repeat(64)),
            "partial"=>value["transcription"]["partial"]=json!(true),
            "coverage"=>value["transcription"]["coverage"]=json!("no_audio_stream"),
            _=>value["actualAsrResult"]=json!({}),
        }
        assert!(derive_fixture_speech(&d,&value,&[]).is_err(),"{case}");
    }
}
#[test]
fn shared_synthetic_material_metadata_remains_bound_to_current_native_wire() {
    // Synthetic unit references only; this test does not establish CAS/model delivery.
    let mut d=empty();accounts::initialize(&mut d,accounts::Profile::BawRussia).unwrap();
    let binding=active_binding(&d).unwrap().to_json();
    let mut wire=json!({"account":"BAW Russia","connectorBinding":binding,"mandatoryMaterialContract":preparation_materials::CONTRACT,
        "materialReadiness":{"status":"ready"},"postContextBundle":{"companyId":"BAW Russia","contentSha256":"a".repeat(64),
            "members":[{"canonicalPostId":"p","connectorBinding":binding,"postSourceVersion":"b".repeat(64),"fields":{"text":"Synthetic"},
                "assets":[{"modality":"photo","attachmentIndex":0,"attachmentIdentity":"c".repeat(64),"sourceVersion":"b".repeat(64),
                    "acquisitionReceiptSha256":"d".repeat(64),"photo":{"artifact":{"sha256":"e".repeat(64),"bytes":69},"width":1,"height":1}}]}]},
        "optionalFrameRefs":[{"sha256":"f".repeat(64),"mime":"image/png","width":64,"height":48,
            "artifact":{"sha256":"f".repeat(64),"bytes":64},"requestedTimestampMs":1000,"actualPts":10240,"timeBase":{"num":1,"den":10240}}]});
    let content=json!({"text":"Synthetic","sources":[],"assessments":[{"itemId":"i","intent":"reply"}],
        "proposals":[{"itemId":"i","kind":"reply_and_close","text":"Synthetic"}]});
    let mut response=engine_prepare::tests::single_pass_result(content);
    let baseline=response.clone();material_invocation(&wire,&mut response);
    let mut restored=response.clone();restored["runMetadata"].as_object_mut().unwrap().remove("materialInvocation");assert_eq!(restored,baseline);
    model_material_receipt::validate_result(&wire,&response).unwrap();
    for case in ["photo","frame","account","receipt","delivered"] {
        let mut changed=response.clone();
        match case {
            "photo"=>changed["runMetadata"]["materialInvocation"]["requiredPhotos"][0]["artifact"]["sha256"]=json!("a".repeat(64)),
            "frame"=>changed["runMetadata"]["materialInvocation"]["deliveredFrames"][0]["actualPts"]=json!(0),
            "account"=>changed["runMetadata"]["materialInvocation"]["companyId"]=json!("LikeAvto"),
            "receipt"=>changed["runMetadata"]["materialInvocation"]["paidResultRef"]=json!({}),
            _=>changed["runMetadata"]["materialInvocation"]["deliveredPhotos"]=json!([]),
        }
        assert!(model_material_receipt::validate_result(&wire,&changed).is_err(),"{case}");
    }
    wire["connectorBinding"]["accountId"]=json!("LikeAvto");assert!(model_material_receipt::validate_result(&wire,&response).is_err());
}
