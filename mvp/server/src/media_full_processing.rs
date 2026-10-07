//! One durable full-frame media phase/chunk. All process/artifact work is outside
//! the workspace writer. Only the exact cursor checkpoint enters change_job.
use super::*;
use crate::media_fullframes as full;
#[path = "media_transcript_reuse.rs"]
pub(crate) mod transcript_reuse;
#[path = "media_gpu_outcome.rs"]
pub(crate) mod gpu_outcome;

pub(super) fn finish_vision_bridge(gate:Option<gpu_gate::Lease>,bridge:crate::ApiResult<Value>,resource:gpu_outcome::Outcome)->Result<Value,String>{
    if bridge.is_ok()||resource.permits_release(){if let Some(gate)=gate{
        // A failed durable clean write remains fail-closed (dirty or invalid).
        // Keep the original inference error; cleanup is not inference success.
        let clean=gate.finish();if bridge.is_ok(){clean?;}else if clean.is_err(){eprintln!("media_gpu_clean_write_failed");}
    }}
    bridge.map_err(|e|format!("visual_backend_failed: {}",e.1))
}

pub(crate) fn importable_download_failure(error:&str)->bool {
    super::download_failure::known(error)
        && !matches!(error,"source_download_failed_process_unknown"|"source_download_failed_wait_failed")
}
/// Probe only an already bounded, hashed private local file. No network input,
/// downloader, ASR or GPU is involved. Full decode catches corrupt/truncated AV;
/// expected duration is an explicit owner provenance claim, not a download time.
pub(crate) async fn probe_retained_source(input:&Path,expected_ms:u64)->Result<Value,String>{
    let ffprobe=MediaConfig::tool("COMMUNITYHERO_MEDIA_FFPROBE")?;
    let ffmpeg=MediaConfig::tool("COMMUNITYHERO_MEDIA_FFMPEG")?;
    probe_retained_with_tools(&ffprobe,&ffmpeg,input,expected_ms).await
}
pub(crate) async fn probe_retained_with_tools(ffprobe:&Path,ffmpeg:&Path,input:&Path,expected_ms:u64)->Result<Value,String>{
    if expected_ms==0||expected_ms>14_400_000{return Err("import_duration_invalid".into());}
    // Pick only a self-contained container from its signature, not extension or
    // demuxer autodetection. MOV private options are invalid on Matroska in
    // ffmpeg, so attach dref/path prohibitions only to the forced MOV demuxer.
    let mut header=[0u8;12];std::fs::File::open(input).and_then(|mut file|file.read_exact(&mut header)).map_err(|_|"import_container_unsupported")?;
    let format=if header[..4]==[0x1a,0x45,0xdf,0xa3]{"matroska"}else if &header[4..8]==b"ftyp"{"mov"}else{return Err("import_container_unsupported".into());};
    let mut input_options=vec!["-protocol_whitelist".into(),"file".into(),"-format_whitelist".into(),"mov,matroska,webm".into(),"-f".into(),format.into()];
    if format=="mov"{input_options.extend(["-enable_drefs".into(),"0".into(),"-use_absolute_path".into(),"0".into()]);}
    let mut args=vec!["-v".into(),"error".into()];args.extend(input_options.clone());args.extend(["-show_entries".into(),"format=duration:stream=codec_type".into(),"-of".into(),"json".into(),input.display().to_string()]);
    let output=run_tool(ffprobe,&args,Duration::from_secs(120),true,"import_probe_failed").await?;
    let probe:Value=serde_json::from_str(&output).map_err(|_|"import_probe_invalid")?;
    let duration=probe["format"]["duration"].as_str().and_then(|s|s.parse::<f64>().ok()).filter(|d|d.is_finite()&&*d>0.0).ok_or("import_duration_missing")?;
    let measured_ms=(duration*1000.0).round() as u64;
    if measured_ms.abs_diff(expected_ms)>250{return Err("import_duration_mismatch".into());}
    let streams=probe["streams"].as_array().ok_or("import_streams_missing")?;
    if !["video","audio"].iter().all(|kind|streams.iter().any(|s|s["codec_type"]==*kind)){return Err("import_full_av_required".into());}
    let mut decoded=Vec::new();
    // Separate progress prevents a complete audio track from hiding an early
    // video EOF (or the inverse) in an otherwise valid container.
    for stream in ["0:v:0","0:a:0"] {
        let mut args=vec!["-nostdin".into(),"-v".into(),"error".into(),"-xerror".into(),"-err_detect".into(),"explode".into(),"-threads".into(),"2".into()];
        args.extend(input_options.clone());args.extend(["-i".into(),input.display().to_string(),"-map".into(),stream.into(),"-threads".into(),"2".into(),"-filter_threads".into(),"2".into(),"-progress".into(),"pipe:1".into(),"-nostats".into(),"-f".into(),"null".into(),"-".into()]);
        let output=run_tool(ffmpeg,&args,Duration::from_secs(600),true,"import_full_decode_failed").await?;
        let decoded_ms=output.lines().filter_map(|line|line.strip_prefix("out_time_us=").and_then(|n|n.parse::<u64>().ok())).max().unwrap_or(0)/1000;
        if !output.lines().any(|line|line=="progress=end")||decoded_ms.saturating_add(1000)<expected_ms||decoded_ms>expected_ms.saturating_add(1000){return Err("import_incomplete_decode".into());}
        decoded.push(decoded_ms);
    }
    Ok(json!({"hasVideo":true,"hasAudio":true,"fullDecode":true,"durationMs":expected_ms,"measuredDurationMs":measured_ms,"decodedVideoDurationMs":decoded[0],"decodedAudioDurationMs":decoded[1],"method":"ffprobe_and_full_ffmpeg_decode"}))
}

/// Explicit audio-only policy work uses the existing content-addressed source.
/// It never enters download, frame extraction, vision, or visual checkpoint code.
pub(crate) async fn cached_audio(app:&crate::App,id:&str,progress:&Value,reuse:Option<(Value,Value)>)->Result<Value,String>{
    cached_audio_or_text(app,id,progress,false,reuse).await
}

/// Full speech and bounded screen text from the exact cached source. No scene
/// inventory, selection, neural vision or visual coverage is produced.
pub(crate) async fn cached_text(app:&crate::App,id:&str,progress:&Value,reuse:Option<(Value,Value)>)->Result<Value,String>{
    cached_audio_or_text(app,id,progress,true,reuse).await
}
async fn cached_audio_or_text(app:&crate::App,id:&str,progress:&Value,screen_text:bool,reuse:Option<(Value,Value)>)->Result<Value,String>{
    let job=gpu_gate::JobContext::bind(app,id,None).await?;
    if !job.matches_audio_progress(progress){return Err("gpu_gate_job_source_changed".into());}
    let store=full::store()?;
    let reference=full::reference(&progress["source"])?;
    let input=store.path(&reference).map_err(|_|"media_source_artifact_unavailable")?;
    if progress["sourceIdentity"]["mediaSha256"]!=reference.sha256
        || progress["sourceIdentity"]["account"]!=progress["account"]
        || progress["sourceIdentity"]["postKey"]!=progress["sourcePostKey"] {
        return Err("media_source_identity_mismatch".into());
    }
    let source=MediaSource::from_projection(&progress["sourceProjection"],progress["account"].as_str().ok_or("media_account_missing")?,progress["sourcePostKey"].as_str().ok_or("media_post_missing")?)?;
    let config=MediaConfig::for_phase("audio")?;
    std::fs::create_dir_all(&config.scratch).map_err(|_|"media_scratch_unavailable")?;
    let work=config.scratch.join(format!("media-audio-{}",uuid::Uuid::new_v4()));
    std::fs::create_dir(&work).map_err(|_|"media_scratch_unavailable")?;
    let _scratch=ScratchGuard(work.clone());
    // Reuse was selected from the caller's already-read, source-checked snapshot.
    // Final admission must reproduce the exact proof under its writer transaction.
    let lifecycle=crate::media_analysis_runtime::Runtime::bind(app,id,Some(progress)).await?;
    let mut result=reuse_audio_or_transcribe(reuse,async{
        transcribe_input_mode(&config,&source,&work,&input,false,Some(&job),Some(&lifecycle)).await
    }).await?;
    if screen_text{
        let duration=result["materials"][0]["transcription"]["mediaDurationSeconds"].as_f64();
        result=append_screen_text(result,&source,&work,&input,duration,Some(&lifecycle)).await?;
    }
    if result["audioAnalysis"].is_object(){return prepare_audio_analysis(app,progress,result,screen_text).await;}
    for material in result["materials"].as_array_mut().ok_or("media_result_invalid")? {
        if material["kind"]=="transcript" {material["transcription"]["sourceVersion"]=progress["sourceVersion"].clone();
            if screen_text{material["transcription"]["ocr"]["sourceVersion"]=progress["sourceVersion"].clone();material["transcription"]["ocr"]["exhaustive"]=json!(false);}}
        if screen_text&&material["kind"]=="ocr" {material["ocr"]["sourceVersion"]=progress["sourceVersion"].clone();material["ocr"]["exhaustive"]=json!(false);}
    }
    Ok(result)
}

/// Resolve the immutable paid result outside the writer. The target receipt is
/// current alias applicability; it never changes the original transcription.
pub(crate) async fn prepare_audio_analysis(app:&crate::App,progress:&Value,mut result:Value,screen_text:bool)->Result<Value,String>{
    let target=if result["audioAnalysis"]["targetRequest"].is_object(){result["audioAnalysis"]["targetRequest"].clone()}
        else{result["audioAnalysis"]["request"].clone()};
    result["audioAnalysis"]["targetRequest"]=target.clone();
    if screen_text {
        let mut outcome=if result["currentScreenText"].is_object(){result["currentScreenText"].clone()}
            else{json!({"account":progress["account"],"postKey":progress["sourcePostKey"],
                "sourceUrl":progress["sourceProjection"]["sourceUrl"],"mediaSha256":progress["source"]["sha256"],
                "sourceVersion":progress["sourceVersion"],"connectorBinding":progress["connectorBinding"],
                "ocr":result["materials"][0]["transcription"]["ocr"]})};
        outcome["ocr"]["sourceVersion"]=progress["sourceVersion"].clone();
        outcome["ocr"]["exhaustive"]=json!(false);
        result["currentScreenText"]=outcome.clone();
        for material in result["materials"].as_array_mut().ok_or("media_result_invalid")? {
            if material["kind"]=="ocr" {material["ocr"]=outcome["ocr"].clone();}
        }
    }
    let d=app.read().await.map_err(|e|e.1)?;
    let ledger=crate::media_analysis::ledger_from_workspace(&d)?;
    let receipt=crate::media_analysis_reuse::receipt_for_request(&d,progress,&target)?;
    let (original,mut pin)=crate::media_analysis_reuse::select_verified(&d,progress,&ledger,&receipt,
        target["specSha256"].as_str().ok_or("media_analysis_spec_missing")?)?.ok_or("media_analysis_complete_result_missing")?;
    if matches!(result["currentScreenText"]["ocr"]["status"].as_str(),Some("completed"|"no_text_found")) {
        crate::media_analysis_reuse::attach_current_screen_text(&mut pin,&result["currentScreenText"])?;
    }
    crate::media_analysis_reuse::warm_pin(&d,&pin)?;
    let materials=result["materials"].as_array_mut().ok_or("media_result_invalid")?;
    let transcript=materials.iter_mut().find(|m|m["kind"]=="transcript").ok_or("media_transcript_missing")?;
    *transcript=original;
    // The capture helper commits through an event returning (). Select the
    // enriched ledger result rather than forwarding its pre-commit envelope.
    result["audioAnalysis"]["result"]=pin["result"].clone();
    result["audioAnalysis"]["request"]=pin["result"]["originalRequest"].clone();
    result["audioAnalysisReuse"]=json!(true);
    result["audioAnalysisApplicability"]=json!({"receipt":receipt,"pin":pin});
    Ok(result)
}

/// Exact target material admission for the full-frame lane. Paid donor speech
/// is exposed by the typed edge, never reimported under the target post ID.
pub(crate) fn admit_audio_analysis(d:&mut Value,progress:&Value,result:&Value)->crate::ApiResult<bool>{
    let Some(applicability)=result.get("audioAnalysisApplicability") else{return Ok(false)};
    let ledger=crate::media_analysis::ledger_from_workspace(d).map_err(|e|crate::conflict(&e))?;
    let pin=&applicability["pin"];
    let materials=result["materials"].as_array().ok_or_else(||crate::conflict("Media analysis materials missing"))?;
    if materials.iter().filter(|m|m["kind"]=="transcript").count()!=1
        || materials.iter().find(|m|m["kind"]=="transcript")!=Some(&pin["originalMaterial"]) {
        return Err(crate::conflict("Media analysis original output changed"));
    }
    let target_materials:Vec<Value>=materials.iter().filter(|m|m["kind"]!="transcript").cloned().collect();
    for material in &target_materials {
        if !matches!(material["kind"].as_str(),Some("ocr"|"visual_context"))
            || material["account"]!=progress["account"]||material["postKey"]!=progress["sourcePostKey"]
            ||material["sourceUrl"]!=progress["sourceProjection"]["sourceUrl"]
            ||material["mediaSha256"]!=progress["source"]["sha256"] {
            return Err(crate::conflict("Media analysis target material changed"));
        }
        if material["kind"]=="ocr" && (material["ocr"]!=result["currentScreenText"]["ocr"]
            || material["ocr"]["sourceVersion"]!=progress["sourceVersion"]
            ||material["ocr"]["exhaustive"]!=false) {
            return Err(crate::conflict("Media analysis target screen outcome changed"));
        }
    }
    crate::media_analysis_reuse::admit(d,progress,&ledger,&applicability["receipt"],pin,&crate::now())
        .map_err(|e|crate::conflict(&e))?;
    crate::merge_materials(d,&json!({"materials":target_materials}))?;
    Ok(true)
}

async fn reuse_audio_or_transcribe(
    reuse:Option<(Value,Value)>,
    transcribe:impl std::future::Future<Output=Result<Value,String>>,
)->Result<Value,String>{
    if let Some((material,pin))=reuse{
        // reused=false retains the existing acquisition result contract: optional
        // OCR still runs. audioReuse alone proves the speech cache hit.
        Ok(json!({"materials":[material],"reused":false,"audioReuse":pin}))
    }else{transcribe.await}
}

pub(crate) fn cached_text_audio_reuse(d:&Value,progress:&Value,at:&str)->Result<Option<(Value,Value)>,String>{
    // Legacy catalog reuse is post-level. Per-asset work must use the immutable
    // file ledger so the first video's transcript cannot stand for its sibling.
    if progress.get("assetPin").is_some(){crate::media_speech_assets::require_progress(d,progress)?;return Ok(None);}
    let Some(pin)=transcript_reuse::select(d,progress,at).map_err(str::to_owned)? else{return Ok(None)};
    if pin["match"]!="exact_media_sha256"||!crate::knowledge::proven_full_audio(&pin["transcription"],progress["sourceVersion"].as_str().unwrap_or("")){
        return Ok(None);
    }
    let post=d["posts"].as_array().into_iter().flatten().find(|p|p["id"]==progress["sourcePostId"]).ok_or("media_post_missing")?;
    let selected=crate::knowledge::select(d,&[],std::slice::from_ref(post),at).map_err(str::to_owned)?;
    let material=selected["materials"].as_array().into_iter().flatten().find(|m|m["id"]==pin["sourceMaterialId"]
        &&m["knowledgeVersionId"]==pin["provenance"]["versionId"]&&m["kind"]=="transcript"
        &&m["account"]==progress["account"]&&m["postKey"]==progress["sourcePostKey"]
        &&m["sourceUrl"]==progress["sourceProjection"]["sourceUrl"]&&m["mediaSha256"]==progress["source"]["sha256"]);
    Ok(material.map(|m|(m.clone(),pin)))
}

// Selection metadata belongs to the signed vision request, not the decoder's
// strict four-field inventory contract. Preserve the original rows separately.
fn extraction_rows(selected:&[Value])->Vec<Value>{selected.iter().map(|row|json!({
    "frameIndex":row["frameIndex"],"pts":row["pts"],"timestampMs":row["timestampMs"],"pixelSha256":row["pixelSha256"]
})).collect()}

fn finalized_materials(mut result:Value,visual:Value,progress:Value)->Result<Value,String>{
    if result["materials"].as_array().into_iter().flatten().any(|m|m["kind"]=="transcript"&&m["transcription"]["partial"]==true){return Err("audio_coverage_incomplete".into());}
    // ASR/OCR may outlive the short proof cache. Revalidate immutable artifacts
    // after that work, outside the writer, before catalog admission.
    full::verify_and_cache(&visual["visualEvidence"])?;
    result["materials"].as_array_mut().ok_or("media_result_invalid")?.push(visual);
    result["visualProgress"]=progress;
    Ok(result)
}

async fn save(app:&crate::App,id:&str,expected:&Value,next:Value)->Result<(),String>{
    let lease=expected["leaseId"].as_str().ok_or("media_lease_missing")?.to_owned();
    app.change_job(id,|d|full::checkpoint(crate::row_mut(d,"jobs",id)?,&lease,expected,next).map_err(|e|crate::conflict(&e))).await.map_err(|e|e.1)
}
fn chunk_limit(value:Option<&str>)->Result<u64,String>{
    match value {
        None=>Ok(full::CHUNK_FRAMES),
        Some(raw)=>raw.parse::<u64>().ok().filter(|n|*n>0&&*n<=crate::media_frame_contract::MAX_CHUNK_POSITIONS)
            .ok_or_else(||"media_chunk_limit_invalid".into()),
    }
}
pub(crate) async fn step(app:&crate::App,id:&str,source:Option<&MediaSource>,progress:Value)->Result<Value,String>{
    let setting=std::env::var("COMMUNITYHERO_MEDIA_CHUNK_FRAMES").ok();
    step_with_limit(app,id,source,progress,chunk_limit(setting.as_deref())?).await
}
#[cfg(test)]
pub(crate) async fn step_for_test(app:&crate::App,id:&str,source:Option<&MediaSource>,progress:Value,limit:u64)->Result<Value,String>{step_with_limit(app,id,source,progress,limit).await}
async fn step_with_limit(app:&crate::App,id:&str,source:Option<&MediaSource>,mut progress:Value,limit:u64)->Result<Value,String>{
    if limit==0||limit>crate::media_frame_contract::MAX_CHUNK_POSITIONS{return Err("media_chunk_limit_invalid".into());}
    let vision_worker=if progress["phase"]=="scan" {
        Some(crate::media_vision_admission::Worker::capture(app,id,Some(&progress)).await.map_err(|e|e.1)?)
    }else{None};
    let config=MediaConfig::for_phase(progress["phase"].as_str().ok_or("media_phase_missing")?)?;let store=full::store()?;
    std::fs::create_dir_all(&config.scratch).map_err(|_|"media_scratch_unavailable")?;
    let work=config.scratch.join(format!("media-{}",uuid::Uuid::new_v4()));
    std::fs::create_dir(&work).map_err(|_|"media_scratch_unavailable")?;let _scratch=ScratchGuard(work.clone());
    let mut next=progress.clone();
    match progress["phase"].as_str().ok_or("media_phase_missing")? {
        "download"=>{
            let source=source.ok_or("media_source_projection_missing")?;
            let input=download_source(&config,source,&work,Duration::from_secs(600)).await?;
            if has_video_stream(&config,&input).await!=Some(true){return Err("visual_video_stream_missing".into());}
            let duration=probe_duration(&config,&input).await.filter(|d|d.is_finite()&&*d>0.0).ok_or("visual_duration_unknown")?;
            let source_ref=store.put_file(&input).map_err(|_|"media_source_artifact_failed")?.to_json();
            next["source"]=source_ref.clone();next["sourceIdentity"]=json!({"account":source.account,"postKey":source.post_key,"mediaSha256":source_ref["sha256"],"durationMs":(duration*1000.0).round() as u64});
            next["sourceProjection"]=source.projection();
            if let Some(pin)=progress.get("assetPin"){
                crate::media_speech_assets::validate_shape(pin)?;
                next["sourceProjection"]["assetPin"]=pin.clone();
            }
            next["phase"]=json!("inventory");save(app,id,&progress,next).await?;
        },
        "inventory"=>{
            let path=store.path(&full::reference(&progress["source"])?).map_err(|_|"media_source_artifact_unavailable")?;
            let descriptor_ref=crate::media_frame_decoder::inventory(&config.ffmpeg,&path,&progress["source"],&progress["sourceIdentity"],&store).await?;
            let descriptor=full::read(&store,&descriptor_ref)?;
            next["inventory"]=descriptor["inventory"].clone();next["inventoryDescriptor"]=descriptor_ref;
            next["phase"]=json!("select");save(app,id,&progress,next).await?;
        },
        "select"=>{
            let path=store.path(&full::reference(&progress["source"])?).map_err(|_|"media_source_artifact_unavailable")?;
            next["selectionDescriptor"]=crate::media_frame_selection::select(&config.ffmpeg,&path,&progress["inventoryDescriptor"],&store).await?;
            next["phase"]=json!("scan");save(app,id,&progress,next).await?;
        },
        "scan"=>{
            let d=full::descriptor(&store,&progress)?;let first=progress["nextSelectionIndex"].as_u64().ok_or("media_cursor_invalid")?;
            let selection=full::selection_descriptor(&store,&progress)?;
            let total=selection["selectedCount"].as_u64().ok_or("media_inventory_invalid")?;
            if first>=total{return Err("media_cursor_invalid".into());}
            let end=(first+limit).min(total);
            let inventory=full::selection_rows(&store,&selection,first,end-first)?;
            let (mut reviewed,_,covered)=full::reviewed(&store,&progress["inventoryDescriptor"],&progress["selectionDescriptor"],&progress["latestReceipt"])?;
            if covered!=first{return Err("media_cursor_receipt_mismatch".into());}
            let mut unique=std::collections::BTreeSet::new();
            let needed:Vec<_>=inventory.iter().filter(|r|!reviewed.contains_key(r["pixelSha256"].as_str().unwrap_or(""))&&unique.insert(r["pixelSha256"].as_str().unwrap_or("").to_owned())).cloned().collect();
            let mut request=Value::Null;let mut response=Value::Null;
            if !needed.is_empty(){
                let path=store.path(&full::reference(&progress["source"])?).map_err(|_|"media_source_artifact_unavailable")?;
                let created=crate::now();
                let decode_rows=extraction_rows(&needed);
                let mut frames=crate::media_frame_decoder::extract(&config.ffmpeg,&path,&d,&decode_rows,&work).await?;
                for frame in &mut frames {let selected=needed.iter().find(|r|r["frameIndex"]==frame["frameIndex"]).ok_or("media_selection_frame_missing")?;frame["selectionIndex"]=selected["selectionIndex"].clone();frame["selectionReasons"]=selected["reasons"].clone();}
                request=crate::media_frame_contract::seal_request(json!({"schemaVersion":2,"workId":work.file_name().and_then(|s|s.to_str()),"createdAtUtc":created,
                    "source":d["sourceIdentity"],"inventory":full::wire_inventory(&d,&selection,&progress["selectionDescriptor"]),"chunk":{"firstSelectionIndex":first,"endSelectionIndexExclusive":end,"previousReceiptSha256":progress["latestReceipt"]["sha256"],"leaseId":progress["leaseId"]},"frames":frames}));
                let stage=vision_worker.ok_or("vision_worker_missing")?.reserve("media_vision_chunk",&request).await.map_err(|e|e.1)?;
                progress=stage.progress().clone();next["visionStage"]=progress["visionStage"].clone();
                let job=gpu_gate::JobContext::bind(app,id,Some(&progress)).await?;
                let gate=gpu_gate::Lease::acquire_for_job("media_vision_chunk",progress["account"].as_str().ok_or("media_account_missing")?,&env::var("COMMUNITYHERO_MEDIA_VISION_LOCAL_ENDPOINT").unwrap_or_default(),Some(&job)).await?;
                let mut resource=gpu_outcome::Outcome::Unknown;
                let bridge=match stage.dispatch(request.clone(),Some(&mut resource)).await {
                    Ok((output,settled))=>{progress=settled;next["visionStage"]=progress["visionStage"].clone();Ok(output)},
                    Err(error)=>Err(error),
                };
                let output=finish_vision_bridge(gate,bridge,resource)?;
                response=crate::media_frame_contract::validate_response(&request,&output).map_err(str::to_owned)?;
                for frame in response["frames"].as_array().ok_or("media_response_invalid")? {reviewed.insert(frame["pixelSha256"].as_str().unwrap().to_owned(),json!({"frameId":frame["id"],"receipt":null}));}
            }
            let aliases:Vec<_>=inventory.iter().map(|r|{let proof=&reviewed[r["pixelSha256"].as_str().unwrap()];json!({"selectionIndex":r["selectionIndex"],"frameIndex":r["frameIndex"],"pts":r["pts"],"pixelSha256":r["pixelSha256"],"frameId":proof["frameId"],"receipt":proof["receipt"]})}).collect();
            let receipt=json!({"schemaVersion":2,"kind":"media_visual_chunk","inventoryDescriptor":progress["inventoryDescriptor"],"selectionDescriptor":progress["selectionDescriptor"],"previousReceipt":progress["latestReceipt"],"firstSelectionIndex":first,"endSelectionIndexExclusive":end,"request":request,"response":response,"aliases":aliases,"leaseId":progress["leaseId"]});
            next["latestReceipt"]=full::put(&store,&receipt)?;next["nextSelectionIndex"]=json!(end);next["completedSelectedFrames"]=json!(end);
            if end==total{next["phase"]=json!("finalize");}
            save(app,id,&progress,next).await?;
        },
        "finalize"=>{
            if let Ok(observer)=gpu_gate::JobContext::bind(app,id,Some(&progress)).await {
                observer.observe_finalize(gpu_gate::FinalizeStage::VisualVerification,None,None).await;
            }
            let evidence=full::final_evidence(&store,&progress)?;
            next["finalEvidence"]=evidence["finalEvidence"].clone();
            next.as_object_mut().ok_or("media_progress_invalid")?.remove("audioReuse");
            // Persist scan completion even if context overflows or later audio fails.
            save(app,id,&progress,next.clone()).await?;
            if evidence["aggregateOverflow"]==true{return Err("visual_aggregate_overflow".into());}
            let source=MediaSource::from_projection(&progress["sourceProjection"],progress["account"].as_str().ok_or("media_account_missing")?,progress["sourcePostKey"].as_str().ok_or("media_post_missing")?)?;
            let input=store.path(&full::reference(&progress["source"])?).map_err(|_|"media_source_artifact_unavailable")?;
            full::verify_and_cache(&evidence)?;
            let key=digest(&format!("{}\n{}",source.account,source.post_key));
            let visual=json!({"id":format!("media:visual-v2:{key}"),"title":format!("Visual context: {}",source.title),"text":"Every decoded frame fast-screened; policy-selected frames visually reviewed. Historical source observations, not verified current offers. Selection is not a guarantee of detecting every transient detail.","kind":"visual_context","account":source.account,"postKey":source.post_key,"sourceUrl":source.source_url,"mediaSha256":progress["source"]["sha256"],"visualEvidence":evidence});
            // One current account-bound catalog snapshot, outside the writer.
            // Reuse keeps original catalog evidence; do not relabel/import it.
            // Rebind after the existing final-evidence checkpoint changed next.
            if let Ok(observer)=gpu_gate::JobContext::bind(app,id,Some(&next)).await {
                observer.observe_finalize(gpu_gate::FinalizeStage::CatalogLookup,None,None).await;
            }
            let reuse_state=app.read().await.map_err(|e|e.1)?;
            // The visual observation may retire a former title-only binding.
            // If it does, transcribe the actual cached target audio.
            let audio_reuse=transcript_reuse::select_after_visual(&reuse_state,&progress,&visual,&crate::now())
                .map_err(str::to_owned)?;
            drop(reuse_state);
            let result=if let Some(pin)=audio_reuse {
                let expected=next.clone();
                next["audioReuse"]=pin.clone();
                save(app,id,&expected,next.clone()).await?;
                json!({"materials":[],"reused":false,"audioReuse":pin})
            }else{
                let job=gpu_gate::JobContext::bind(app,id,Some(&next)).await?;
                let lifecycle=crate::media_analysis_runtime::Runtime::bind(app,id,Some(&next)).await?;
                let result=transcribe_input_mode(&config,&source,&work,&input,true,Some(&job),Some(&lifecycle)).await?;
                prepare_audio_analysis(app,&next,result,true).await?
            };
            return finalized_materials(result,visual,next);
        },
        _=>return Err("media_phase_not_claimable".into())
    }
    Ok(json!({"resume":true}))
}

#[cfg(test)]
mod tests{
 use super::*;
 #[tokio::test]
 async fn exact_cached_speech_does_not_poll_asr_but_missing_proof_does(){
  let material=json!({"kind":"transcript","text":"Admitted speech"});
  let pin=json!({"match":"exact_media_sha256","provenance":{"versionId":"prior"}});
  let calls=std::cell::Cell::new(0);
  let hit=reuse_audio_or_transcribe(Some((material.clone(),pin.clone())),async{
   calls.set(calls.get()+1);Err("ASR must not run for an exact admitted hit".into())
  }).await.unwrap();
  assert_eq!(calls.get(),0);assert_eq!(hit["materials"],json!([material]));assert_eq!(hit["audioReuse"],pin);
  let miss=reuse_audio_or_transcribe(None,async{calls.set(calls.get()+1);Err("expected ASR failure".into())}).await;
  assert_eq!(calls.get(),1);assert_eq!(miss.unwrap_err(),"expected ASR failure");
 }

 #[test]
 fn cached_text_reuses_only_exact_source_proven_current_audio(){
  let at="2026-10-02T00:00:00Z";let mut d=crate::empty();crate::accounts::initialize(&mut d,crate::accounts::Profile::BawRussia).unwrap();
  let post=json!({"id":"text-reuse","account":d["account"],"postKey":"text-reuse","title":"Existing full speech","sourceUrl":"https://example.test/video","attachments":[{"type":"video"}]});
  d["posts"]=json!([post]);let mut progress=full::initial(d["account"].as_str().unwrap(),&crate::active_binding(&d).unwrap().to_json(),&post,at);
  progress["source"]=json!({"sha256":"a".repeat(64),"bytes":100});
  progress["sourceIdentity"]=json!({"account":d["account"],"postKey":post["postKey"],"mediaSha256":"a".repeat(64),"durationMs":37000});
  progress["sourceProjection"]=json!({"sourceUrl":post["sourceUrl"]});
  d["materials"]=json!([{"id":"prior-speech","kind":"transcript","account":d["account"],"postKey":post["postKey"],"sourceUrl":post["sourceUrl"],"mediaSha256":"a".repeat(64),"text":"Already transcribed speech", "transcription":{"partial":false,"coverage":"full_audio","audioDurationSeconds":37.0,"mediaDurationSeconds":37.0,"sourceVersion":progress["sourceVersion"]}}]);
  crate::knowledge::sync_catalog(&mut d,at).unwrap();let before=d.clone();
  let (material,pin)=cached_text_audio_reuse(&d,&progress,at).unwrap().unwrap();assert_eq!(material["text"],"Already transcribed speech");assert_eq!(pin["match"],"exact_media_sha256");assert_eq!(d,before);
  d["materials"][0]["transcription"]["sourceVersion"]=json!("stale");crate::knowledge::sync_catalog(&mut d,at).unwrap();
  assert!(cached_text_audio_reuse(&d,&progress,at).unwrap().is_none());
 }
 #[test]
 fn final_admission_refreshes_proof_after_slow_audio_without_reprocessing_frames(){
  let post=json!({"id":"vk-proof-refresh","postKey":"vk-proof-refresh","title":"Tax on sale",
   "sourceUrl":"https://vk.ru/wall-123_4","attachments":[{"type":"video","source_url":"https://vk.com/video-123_5"}]});
  let evidence=full::fixture_for_post("BAW Russia",&post);
  let original=evidence["finalEvidence"].clone();
  let visual=json!({"id":"visual-refresh","kind":"visual_context","account":"BAW Russia","postKey":post["postKey"],
   "sourceUrl":"https://vk.com/video-123_5","mediaSha256":evidence["source"]["mediaSha256"],"text":"Observed frames","visualEvidence":evidence});
  let audio=json!({"materials":[{"id":"audio-refresh","kind":"transcript","account":"BAW Russia","postKey":post["postKey"],
   "sourceUrl":"https://vk.com/video-123_5","mediaSha256":evidence["source"]["mediaSha256"],"text":"Complete speech","transcription":{"partial":false}}]});
  // A delayed ASR leaves the same immutable evidence but no current cache proof.
  full::forget_test_proof(&evidence);
  assert!(full::validate_evidence(&evidence).is_err());
  let result=finalized_materials(audio,visual,json!({"finalEvidence":original})).unwrap();
  assert_eq!(result["materials"][1]["visualEvidence"]["finalEvidence"],original);
  let mut workspace=json!({"account":"BAW Russia","posts":[post],"materials":result["materials"]});
  crate::knowledge::sync_catalog(&mut workspace,"2026-09-26T00:00:00Z").unwrap();
  assert!(crate::knowledge::TranscriptLookup::new(&workspace,"2026-09-26T00:00:00Z").unwrap().ready(&workspace["posts"][0]).unwrap());
 }
 #[test]
 fn final_admission_still_rejects_changed_artifacts_and_partial_audio(){
  let evidence=full::fixture("BAW Russia","refresh-reject");
  let visual=json!({"kind":"visual_context","visualEvidence":evidence});
  assert_eq!(finalized_materials(json!({"materials":[{"kind":"transcript","transcription":{"partial":true}}]}),visual.clone(),Value::Null).unwrap_err(),"audio_coverage_incomplete");
  let mut changed=visual;
  changed["visualEvidence"]["finalEvidence"]["sha256"]=json!("0".repeat(64));
  assert!(finalized_materials(json!({"materials":[]}),changed,Value::Null).is_err());
 }
 #[test]
 fn configured_chunk_size_is_bounded_without_changing_legacy_default(){
  assert_eq!(chunk_limit(None).unwrap(),4);
  assert_eq!(chunk_limit(Some("32")).unwrap(),32);
  for value in ["0","33","-1","abc",""] {assert!(chunk_limit(Some(value)).is_err());}
 }
 /// Explicit offline real-source canary: disposable workspace, no downloads,
 /// no social operations. Exercises the production decoder, bridge and receipts.
 #[tokio::test]
 #[ignore="requires explicitly configured real-video bridge and private output"]
 async fn real_video_visual_route_acceptance(){
  preflight().unwrap();let config=MediaConfig::from_env().unwrap();let store=full::store().unwrap();
  let required=|k|env::var(k).unwrap_or_else(|_|panic!("{k} required"));
  let input=PathBuf::from(required("COMMUNITYHERO_MEDIA_SAMPLE_INPUT"));
  let output=PathBuf::from(required("COMMUNITYHERO_MEDIA_VISUAL_ACCEPTANCE_OUTPUT"));
  let account=required("COMMUNITYHERO_MEDIA_SAMPLE_ACCOUNT");let post_key=required("COMMUNITYHERO_MEDIA_SAMPLE_POST_KEY");
  let (mut app,_temp)=crate::tests::test_app().await;
  app.account=match account.as_str(){"BAW Russia"=>crate::accounts::Profile::BawRussia,"LikeAvto"=>crate::accounts::Profile::LikeAvto,_=>panic!("Unsupported canary account")};
  let profile=app.account;
  app.change(|d|{d["account"]=json!(profile.display());d["connectorBinding"]=profile.binding();Ok(())}).await.unwrap();
  app.bridge=PathBuf::from(required("COMMUNITYHERO_MEDIA_VISUAL_ACCEPTANCE_BRIDGE"));app.node=PathBuf::from(required("COMMUNITYHERO_NODE"));app.external_writes=false;
  let source_ref=store.put_file(&input).unwrap().to_json();let duration=probe_duration(&config,&input).await.unwrap();
  let post=json!({"id":"real-video-canary","postKey":post_key});let id="real-video-canary";
  app.change(|d|{let binding=crate::active_binding(d)?;let mut p=full::initial(&account,&binding.to_json(),&post,&crate::now());full::claim(&mut p,"offline-canary").unwrap();p["phase"]=json!("inventory");p["source"]=source_ref.clone();p["sourceIdentity"]=json!({"account":account,"postKey":post_key,"mediaSha256":source_ref["sha256"],"durationMs":(duration*1000.0).round() as u64});crate::list_mut(d,"jobs").push(json!({"id":id,"kind":"media","status":"running","result":{"visualProgress":p}}));Ok(())}).await.unwrap();
  let started=std::time::Instant::now();
  for _ in 0..100 {
   let state=app.read().await.unwrap();let p=crate::row(&state,"jobs",id).unwrap()["result"]["visualProgress"].clone();
   if p["phase"]=="finalize" {
    let evidence=full::final_evidence(&store,&p).unwrap();assert_ne!(evidence["aggregateOverflow"],true);full::verify_and_cache(&evidence).unwrap();
    std::fs::write(output,serde_json::to_vec_pretty(&json!({"status":"passed","seconds":started.elapsed().as_secs_f64(),"progress":p,"evidence":evidence,"socialWrites":0})).unwrap()).unwrap();return;
   }
   step_for_test(&app,id,None,p,32).await.expect("real-video phase failed");
  }
  panic!("real-video canary exceeded bounded steps");
 }
 /// Offline completion of a previously scanned real BAW video. The input
 /// receipt and CAS are immutable canary artifacts; only a disposable SQLite
 /// workspace is used. No vision bridge, download, or social operation runs.
 #[tokio::test]
 #[ignore="requires the pinned BAW visual receipt, private CAS and native Whisper runtime"]
 async fn real_baw_audio_visual_finalize_acceptance(){
  let required=|k|env::var(k).unwrap_or_else(|_|panic!("{k} required"));
  let input=PathBuf::from(required("COMMUNITYHERO_MEDIA_FULL_ACCEPTANCE_INPUT"));
  let output=PathBuf::from(required("COMMUNITYHERO_MEDIA_FULL_ACCEPTANCE_OUTPUT"));
  assert!(input.is_absolute()&&output.is_absolute());
  let receipt:Value=serde_json::from_slice(&std::fs::read(&input).unwrap()).unwrap();
  assert_eq!(receipt["status"],"passed");
  let mut progress=receipt["progress"].clone();
  let expected_post_key="12182:6aad3c7f6aa20d24327cea82";
  let expected_sha="7a12bf3fa3c5da7eed59c6ab141c3c8ae574cb5668f6b230ed4eee619adad64b";
  assert_eq!(progress["phase"],"finalize");
  assert_eq!(progress["account"],"BAW Russia");
  assert_eq!(progress["sourcePostKey"],expected_post_key);
  assert_eq!(progress["sourceIdentity"]["postKey"],expected_post_key);
  assert_eq!(progress["source"]["sha256"],expected_sha);
  assert_eq!(progress["sourceIdentity"]["mediaSha256"],expected_sha);
  assert_eq!(progress["nextSelectionIndex"],89);
  assert_eq!(progress["completedSelectedFrames"],89);
  assert_eq!(receipt["evidence"]["source"],progress["sourceIdentity"]);
  assert_eq!(receipt["evidence"]["aggregateOverflow"],false);
  let store=full::store().unwrap();
  let evidence=full::final_evidence(&store,&progress).unwrap();
  assert_eq!(evidence,receipt["evidence"],"existing CAS must reproduce the original visual proof");
  full::verify_and_cache(&evidence).unwrap();
  let post=json!({"id":"real-video-canary","postKey":expected_post_key});
  assert_eq!(full::source_version(&post,"BAW Russia"),progress["sourceVersion"]);
  // The visual-only canary did not persist sourceProjection. This projection is
  // explicit test metadata; account, post key, content hash and visual proof
  // retain their original exact identities.
  progress["sourceProjection"]=json!({"account":"BAW Russia","postKey":expected_post_key,
   "title":"BAW real video offline canary","sourceUrl":"https://vk.com/","fallbackUrl":null});
  let (mut app,_temp)=crate::tests::test_app().await;
  app.account=crate::accounts::Profile::BawRussia;app.external_writes=false;
  let profile=app.account;
  let id="real-baw-full-canary";
  app.change(|d|{d["account"]=json!(profile.display());d["connectorBinding"]=profile.binding();
   assert_eq!(d["connectorBinding"],progress["connectorBinding"]);
   crate::list_mut(d,"posts").push(post.clone());
   crate::list_mut(d,"jobs").push(json!({"id":id,"kind":"media","visualContractVersion":2,
    "status":"running","result":{"visualProgress":progress}}));Ok(())}).await.unwrap();
  let state=app.read().await.unwrap();
  let before=crate::row(&state,"jobs",id).unwrap()["result"]["visualProgress"].clone();
  let started=std::time::Instant::now();
  let result=step_for_test(&app,id,None,before,1).await.expect("real BAW finalize failed");
  assert_eq!(result["reused"],false,"the isolated canary must run real ASR");
  let materials=result["materials"].as_array().expect("finalized materials");
  let transcript=materials.iter().find(|m|m["kind"]=="transcript").expect("audio transcript material");
  let visual=materials.iter().find(|m|m["kind"]=="visual_context").expect("visual context material");
  assert_eq!(transcript["mediaSha256"],expected_sha);
  assert_eq!(transcript["postKey"],expected_post_key);
  assert_eq!(transcript["transcription"]["model"],"local-whisper");
  assert_eq!(transcript["transcription"]["audioStatus"],"transcribed");
  assert_eq!(transcript["transcription"]["partial"],false);
  assert_eq!(transcript["transcription"]["coverage"],"full_audio");
  assert!(!transcript["text"].as_str().unwrap_or("").trim().is_empty());
  assert_eq!(visual["mediaSha256"],expected_sha);
  assert_eq!(visual["postKey"],expected_post_key);
  assert_eq!(visual["visualEvidence"],evidence);
  assert_eq!(result["visualProgress"]["finalEvidence"],evidence["finalEvidence"]);
  full::verify_and_cache(&visual["visualEvidence"]).unwrap();
  let report=json!({"status":"passed","seconds":started.elapsed().as_secs_f64(),
   "sourcePostKey":expected_post_key,"sourceSha256":expected_sha,
   "originalVisualEvidence":evidence["finalEvidence"],"result":result,"socialWrites":0,
   "workspace":"disposable SQLite","sourceProjection":"offline canary placeholder URL"});
  std::fs::write(output,serde_json::to_vec_pretty(&report).unwrap()).unwrap();
 }
 /// Admit the already completed native-ASR result through the same catalog
 /// merge as the worker, then reopen disposable SQLite and check the real gate.
 /// This second phase deliberately avoids a second Whisper or visual call.
 #[tokio::test]
 #[ignore="requires an existing real BAW full-media canary receipt and private CAS"]
 async fn real_baw_audio_visual_catalog_durability_acceptance(){
  let input=PathBuf::from(env::var("COMMUNITYHERO_MEDIA_FULL_ACCEPTANCE_OUTPUT").expect("full canary receipt path"));
  assert!(input.is_absolute());
  let receipt:Value=serde_json::from_slice(&std::fs::read(&input).unwrap()).unwrap();
  assert_eq!(receipt["status"],"passed");
  assert_eq!(receipt["result"]["reused"],false);
  let result=&receipt["result"];
  let evidence=&result["materials"].as_array().unwrap().iter()
   .find(|m|m["kind"]=="visual_context").unwrap()["visualEvidence"];
  assert_eq!(evidence["finalEvidence"],receipt["originalVisualEvidence"]);
  full::verify_and_cache(evidence).unwrap();
  let post_key="12182:6aad3c7f6aa20d24327cea82";
  let post=json!({"id":"real-video-canary","postKey":post_key});
  assert_eq!(full::source_version(&post,"BAW Russia"),evidence["sourcePostVersion"]);
  let (mut app,temp)=crate::tests::test_app().await;
  app.account=crate::accounts::Profile::BawRussia;app.external_writes=false;
  let profile=app.account;
  app.change(|d|{d["account"]=json!(profile.display());d["connectorBinding"]=profile.binding();
   crate::list_mut(d,"posts").push(post.clone());
   crate::merge_materials(d,result)?;
   assert!(crate::knowledge::TranscriptLookup::new(d,&crate::now()).unwrap().ready(&post).unwrap(),
    "audio and visual materials must be admitted together");
   Ok(())}).await.unwrap();
  let committed=app.read().await.unwrap();
  let materials=committed["materials"].clone();
  let entries=committed["knowledge_entries"].clone();
  let versions=committed["knowledge_versions"].clone();
  assert!(materials.as_array().unwrap().iter().any(|m|m["kind"]=="transcript"));
  assert!(materials.as_array().unwrap().iter().any(|m|m["kind"]=="visual_context"));
  assert!(crate::knowledge::TranscriptLookup::new(&committed,&crate::now()).unwrap().ready(&post).unwrap());
  app.db.close().await;
  app.db=crate::Database::Sqlite(crate::open_db(&temp.path().join("workspace.sqlite")).await.unwrap());
  full::verify_and_cache(evidence).unwrap();
  let reopened=app.read().await.unwrap();
  assert_eq!(reopened["materials"],materials);
  assert_eq!(reopened["knowledge_entries"],entries);
  assert_eq!(reopened["knowledge_versions"],versions);
  let lookup=crate::knowledge::TranscriptLookup::new(&reopened,&crate::now()).unwrap();
  assert!(lookup.has(&post).unwrap());
  assert!(lookup.has_visual(&post).unwrap());
  assert!(lookup.ready(&post).unwrap(),"reopened catalog must pass required-media gate");
  let report=json!({"status":"passed","sourcePostKey":post_key,"sourceSha256":receipt["sourceSha256"],
   "admittedMaterialKinds":materials.as_array().unwrap().iter().map(|m|m["kind"].clone()).collect::<Vec<_>>(),
   "knowledgeEntries":entries.as_array().unwrap().len(),"knowledgeVersions":versions.as_array().unwrap().len(),
   "sqliteReopened":true,"transcriptLookupReady":true,"visualProofVerifiedAfterReopen":true,
   "socialWrites":0,"workspace":"disposable SQLite"});
  let output=input.with_file_name("admission-result.json");
  std::fs::write(output,serde_json::to_vec_pretty(&report).unwrap()).unwrap();
 }
 #[test]
 fn selected_rows_project_exact_decoder_inventory_without_losing_request_metadata(){
  let selected=json!({"selectionIndex":7,"frameIndex":23,"pts":"767","timestampMs":767,"pixelSha256":"a".repeat(64),"reasons":["transient_pulse"]});
  let original=selected.clone();let rows=extraction_rows(&[selected.clone()]);
  assert_eq!(rows,vec![json!({"frameIndex":23,"pts":"767","timestampMs":767,"pixelSha256":"a".repeat(64)})]);
  assert_eq!(rows[0].as_object().unwrap().len(),4);
  assert_eq!(selected,original);assert_eq!(selected["selectionIndex"],7);assert_eq!(selected["reasons"],json!(["transient_pulse"]));
 }
 /// Root-run only. No provider/download or production DB: explicit local source,
 /// pinned local model bridge, disposable SQLite, durable private evidence CAS.
 #[tokio::test]
 #[ignore="requires explicit reviewed local SMART bridge/model and transient video"]
 async fn local_smart_chunk_restart_acceptance(){
  preflight().unwrap();let config=MediaConfig::from_env().unwrap();let store=full::store().unwrap();
  let required=|k|env::var(k).unwrap_or_else(|_|panic!("{k} required"));
  let input=PathBuf::from(required("COMMUNITYHERO_MEDIA_SAMPLE_INPUT"));let output=PathBuf::from(required("COMMUNITYHERO_MEDIA_VISUAL_ACCEPTANCE_OUTPUT"));assert!(input.is_absolute()&&output.is_absolute());
  let account=required("COMMUNITYHERO_MEDIA_SAMPLE_ACCOUNT");let post_key=required("COMMUNITYHERO_MEDIA_SAMPLE_POST_KEY");let title=required("COMMUNITYHERO_MEDIA_SAMPLE_TITLE");let url=required("COMMUNITYHERO_MEDIA_SAMPLE_SOURCE_URL");
  let (mut app,temp)=crate::tests::test_app().await;app.bridge=PathBuf::from(required("COMMUNITYHERO_MEDIA_VISUAL_ACCEPTANCE_BRIDGE"));app.node=PathBuf::from(required("COMMUNITYHERO_NODE"));app.external_writes=false;
  let source_ref=store.put_file(&input).unwrap().to_json();let duration=probe_duration(&config,&input).await.unwrap();
  let post=json!({"id":"transient-local","postKey":post_key,"title":title,"sourceUrl":url,"attachments":[{"type":"video"}]});let id="smart-canary-job";
  app.change(|d|{let binding=crate::active_binding(d)?;let mut progress=full::initial(&account,&binding.to_json(),&post,&crate::now());full::claim(&mut progress,"canary-first").unwrap();progress["phase"]=json!("inventory");progress["source"]=source_ref.clone();progress["sourceIdentity"]=json!({"account":account,"postKey":post_key,"mediaSha256":source_ref["sha256"],"durationMs":(duration*1000.0).round() as u64});progress["sourceProjection"]=json!({"account":account,"postKey":post_key,"title":title,"sourceUrl":url,"fallbackUrl":null});crate::list_mut(d,"posts").push(post.clone());crate::list_mut(d,"jobs").push(json!({"id":id,"kind":"media","visualContractVersion":2,"status":"running","result":{"visualProgress":progress}}));Ok(())}).await.unwrap();
  let mut restarted=false;let mut first_receipt=Value::Null;let mut final_result=Value::Null;
  for _ in 0..20 {
   let state=app.read().await.unwrap();let progress=crate::row(&state,"jobs",id).unwrap()["result"]["visualProgress"].clone();
   if !restarted&&progress["nextSelectionIndex"]==1 {
    first_receipt=progress["latestReceipt"].clone();app.db.close().await;app.db=crate::Database::Sqlite(crate::open_db(&temp.path().join("workspace.sqlite")).await.unwrap());
    app.change_job(id,|d|{let job=crate::row_mut(d,"jobs",id)?;full::recover(job).unwrap();job["status"]=json!("running");full::claim(&mut job["result"]["visualProgress"],"canary-after-reopen").unwrap();Ok(())}).await.unwrap();restarted=true;continue;
   }
   let result=step_for_test(&app,id,None,progress,1).await.expect("SMART chunk must complete");
   if result["materials"].is_array(){final_result=result;break;}
  }
  assert!(restarted);let visual=final_result["materials"].as_array().unwrap().iter().find(|m|m["kind"]=="visual_context").unwrap();full::verify_and_cache(&visual["visualEvidence"]).unwrap();
  let final_doc=full::read(&store,&visual["visualEvidence"]["finalEvidence"]).unwrap();let mut cursor=final_doc["latestReceipt"].clone();let mut model_frames=0;let mut receipts=0;let mut original_retained=false;
  while !cursor.is_null(){if cursor==first_receipt{original_retained=true;}let receipt=full::read(&store,&cursor).unwrap();model_frames+=receipt["response"]["frames"].as_array().map(Vec::len).unwrap_or(0);receipts+=1;cursor=receipt["previousReceipt"].clone();}
  assert!(original_retained);assert_eq!(model_frames,2,"blank exact pixels must be reviewed once");assert_eq!(receipts,3);assert!(visual["visualEvidence"]["aggregate"].to_string().contains("2490000"),"33ms price observation must survive selection/restart/aggregation");
  let receipt=json!({"status":"passed","sqliteReopenedAfterFirstChunk":true,"firstReceiptRetained":true,"modelFrames":model_frames,"chunks":receipts,"result":final_result});std::fs::write(output,serde_json::to_vec_pretty(&receipt).unwrap()).unwrap();
 }
}
