//! One durable full-frame media phase/chunk. All process/artifact work is outside
//! the workspace writer. Only the exact cursor checkpoint enters change_job.
use super::*;
use crate::media_fullframes as full;

// Selection metadata belongs to the signed vision request, not the decoder's
// strict four-field inventory contract. Preserve the original rows separately.
fn extraction_rows(selected:&[Value])->Vec<Value>{selected.iter().map(|row|json!({
    "frameIndex":row["frameIndex"],"pts":row["pts"],"timestampMs":row["timestampMs"],"pixelSha256":row["pixelSha256"]
})).collect()}

async fn save(app:&crate::App,id:&str,expected:&Value,next:Value)->Result<(),String>{
    let lease=expected["leaseId"].as_str().ok_or("media_lease_missing")?.to_owned();
    app.change_job(id,|d|full::checkpoint(crate::row_mut(d,"jobs",id)?,&lease,expected,next).map_err(|e|crate::conflict(&e))).await.map_err(|e|e.1)
}
pub(crate) async fn step(app:&crate::App,id:&str,source:Option<&MediaSource>,progress:Value)->Result<Value,String>{step_with_limit(app,id,source,progress,full::CHUNK_FRAMES).await}
#[cfg(test)]
pub(crate) async fn step_for_test(app:&crate::App,id:&str,source:Option<&MediaSource>,progress:Value,limit:u64)->Result<Value,String>{step_with_limit(app,id,source,progress,limit).await}
async fn step_with_limit(app:&crate::App,id:&str,source:Option<&MediaSource>,progress:Value,limit:u64)->Result<Value,String>{
    if limit==0||limit>full::CHUNK_FRAMES{return Err("media_chunk_limit_invalid".into());}
    let config=MediaConfig::from_env()?;let store=full::store()?;
    std::fs::create_dir_all(&config.scratch).map_err(|_|"media_scratch_unavailable")?;
    let work=config.scratch.join(format!("media-{}",uuid::Uuid::new_v4()));
    std::fs::create_dir(&work).map_err(|_|"media_scratch_unavailable")?;let _scratch=ScratchGuard(work.clone());
    let mut next=progress.clone();
    match progress["phase"].as_str().ok_or("media_phase_missing")? {
        "download"=>{
            let source=source.ok_or("media_source_projection_missing")?;
            let download=download_args(&config,&work);let mut input=None;
            for locator in std::iter::once(&source.source_url).chain(source.fallback_url.iter()){
                let mut args=download.clone();args.push(locator.clone());
                match run_tool(&config.ytdlp,&args,Duration::from_secs(600),false,"source_download_failed").await.and_then(|_|downloaded(&work)){
                    Ok(path)=>{input=Some(path);break;},
                    Err(_) if locator==&source.source_url&&source.fallback_url.is_some()=>{},
                    Err(e)=>return Err(e)
                }
            }
            let input=input.ok_or("source_file_missing")?;
            if has_video_stream(&config,&input).await!=Some(true){return Err("visual_video_stream_missing".into());}
            let duration=probe_duration(&config,&input).await.filter(|d|d.is_finite()&&*d>0.0).ok_or("visual_duration_unknown")?;
            let source_ref=store.put_file(&input).map_err(|_|"media_source_artifact_failed")?.to_json();
            next["source"]=source_ref.clone();next["sourceIdentity"]=json!({"account":source.account,"postKey":source.post_key,"mediaSha256":source_ref["sha256"],"durationMs":(duration*1000.0).round() as u64});
            next["sourceProjection"]=json!({"account":source.account,"postKey":source.post_key,"title":source.title,"sourceUrl":source.source_url,"fallbackUrl":source.fallback_url});
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
                let output=app.bridge("media_vision_chunk",request.clone()).await.map_err(|e|format!("visual_backend_failed: {}",e.1))?;
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
            let evidence=full::final_evidence(&store,&progress)?;
            next["finalEvidence"]=evidence["finalEvidence"].clone();
            // Persist scan completion even if context overflows or later audio fails.
            save(app,id,&progress,next.clone()).await?;
            if evidence["aggregateOverflow"]==true{return Err("visual_aggregate_overflow".into());}
            let source=MediaSource::from_projection(&progress["sourceProjection"],progress["account"].as_str().ok_or("media_account_missing")?,progress["sourcePostKey"].as_str().ok_or("media_post_missing")?)?;
            let input=store.path(&full::reference(&progress["source"])?).map_err(|_|"media_source_artifact_unavailable")?;
            let mut result=transcribe_input(&config,&source,&work,&input).await?;
            if result["materials"].as_array().into_iter().flatten().any(|m|m["kind"]=="transcript"&&m["transcription"]["partial"]==true){return Err("audio_coverage_incomplete".into());}
            full::verify_and_cache(&evidence)?;
            let key=digest(&format!("{}\n{}",source.account,source.post_key));
            result["materials"].as_array_mut().ok_or("media_result_invalid")?.push(json!({"id":format!("media:visual-v2:{key}"),"title":format!("Visual context: {}",source.title),"text":"Every decoded frame fast-screened; policy-selected frames visually reviewed. Historical source observations, not verified current offers. Selection is not a guarantee of detecting every transient detail.","kind":"visual_context","account":source.account,"postKey":source.post_key,"sourceUrl":source.source_url,"mediaSha256":progress["source"]["sha256"],"visualEvidence":evidence}));
            result["visualProgress"]=next;return Ok(result);
        },
        _=>return Err("media_phase_not_claimable".into())
    }
    Ok(json!({"resume":true}))
}

#[cfg(test)]
mod tests{
 use super::*;
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
