//! Targeted post-context frame needs. Model declarations are requests, never
//! source, lease, extraction or paid authority. Decoder admission is separate.
use serde_json::{json,Value};
use crate::preparation_materials::{rows,hash};
pub(crate) const NEED_CONTRACT:&str="VideoFrameNeed.v1";
pub(crate) const RESULT_CONTRACT:&str="VideoFrameResult.v1";
fn txt<'a>(v:&'a Value,k:&str)->&'a str{v[k].as_str().unwrap_or("")}
fn sha(v:&Value)->bool{v.as_str().is_some_and(|s|s.len()==64&&s.bytes().all(|b|b.is_ascii_digit()||(b'a'..=b'f').contains(&b)))}
fn retained_source(d:&Value,need:&Value)->Result<Value,&'static str>{
    let post=crate::row(d,"posts",txt(&need["member"],"postId")).map_err(|_|"frame_source_post_missing")?;
    let videos=rows(post,"attachments").iter().filter(|a|matches!(txt(a,"type"),"video"|"clip"|"reel")).count();
    let index=need["asset"]["attachmentIndex"].as_u64().ok_or("frame_source_asset_missing")? as usize;
    let attachment=rows(post,"attachments").get(index).ok_or("frame_source_asset_missing")?;
    let mut found=None;
    for job in rows(d,"jobs"){
        let p=&job["result"]["visualProgress"];
        if job["kind"]!="media"||p["schemaVersion"]!=2||p["account"]!=d["account"]||p["sourcePostId"]!=post["id"]||p["sourcePostKey"]!=post["postKey"]
            ||p["connectorBinding"]!=need["member"]["connectorBinding"]||p["sourceVersion"]!=need["asset"]["sourceVersion"]
            ||p["sourceIdentity"]["account"]!=d["account"]||p["sourceIdentity"]["postKey"]!=post["postKey"]||p["sourceIdentity"]["mediaSha256"]!=p["source"]["sha256"]
            ||p["sourceIdentity"]["durationMs"].as_u64().is_none_or(|n|n==0||n>14_400_000)||crate::media_artifacts::ArtifactRef::from_json(&p["source"]).is_err(){continue;}
        let exact=videos==1||["sourceUrl","source_url","url"].iter().any(|key|attachment[*key].as_str().is_some_and(|s|!s.is_empty())&&["sourceUrl","fallbackUrl"].iter().any(|s|attachment[*key]==p["sourceProjection"][*s]));
        if !exact{continue;}
        let source=json!({"sourceArtifactRef":p["source"],"sourceArtifactSha256":p["source"]["sha256"],"sourceDurationMs":p["sourceIdentity"]["durationMs"],"originJobId":job["id"],"sourceReceipt":p});
        if found.as_ref().is_some_and(|old:&Value|old["sourceArtifactRef"]!=source["sourceArtifactRef"]){return Err("frame_source_ambiguous");}found=Some(source);
    }found.ok_or("frame_retained_source_unavailable")
}
pub(crate) fn validate_need(need:&Value)->Result<(),&'static str>{
    let mut unsigned=need.clone();unsigned.as_object_mut().ok_or("frame_need_invalid")?.remove("needSha256");
    if need["schemaVersion"]!=1||need["contract"]!=NEED_CONTRACT||need["needSha256"]!=hash(&unsigned)
        ||txt(need,"needId").is_empty()||txt(need,"originatingAnsweringAttemptId").is_empty()
        ||txt(need,"requestingPaidAttemptId").is_empty()||!sha(&need["asset"]["sourceVersion"])
        ||!sha(&need["asset"]["attachmentIdentity"])||!need["affectedRecipientIds"].is_array()
        ||rows(need,"affectedRecipientIds").is_empty()||need["budget"]["maxFrames"].as_u64().is_none_or(|n|n==0||n>8){return Err("frame_need_invalid");}
    let intent=&need["requestedTimeOrIntent"];
    match txt(intent,"kind"){
        "known_range"=>{let start=intent["startMs"].as_u64().ok_or("frame_need_range_invalid")?;let end=intent["endMs"].as_u64().ok_or("frame_need_range_invalid")?;
            if end<=start||end-start>30000{return Err("frame_need_range_invalid");}},
        "uniform_overview"=>{},_=>return Err("frame_need_intent_invalid"),
    }
    if intent["timelineBasis"]!="relative_video_start"{return Err("frame_need_timeline_unknown");}Ok(())
}
pub(crate) fn current_need(d:&Value,need:&Value)->Result<(),&'static str>{
    validate_need(need)?;
    if need["companyId"]!=d["account"]{return Err("frame_need_foreign_company");}
    let post=crate::row(d,"posts",txt(&need["member"],"postId")).map_err(|_|"frame_need_post_missing")?;
    let binding=if post["connectorBinding"].is_null(){d["connectorBinding"].clone()}else{post["connectorBinding"].clone()};
    let index=need["asset"]["attachmentIndex"].as_u64().and_then(|n|usize::try_from(n).ok()).ok_or("frame_need_asset_invalid")?;
    let attachment=rows(post,"attachments").get(index).ok_or("frame_need_asset_missing")?;
    if !matches!(txt(attachment,"type"),"video"|"clip"|"reel"){return Err("frame_need_not_video");}
    if need["member"]["connectorBinding"]!=binding||crate::media_fullframes::source_version(post,txt(d,"account"))!=need["asset"]["sourceVersion"]
        ||crate::media_analysis_reuse::attachment_identity(attachment)!=need["asset"]["attachmentIdentity"]{return Err("frame_need_source_changed");}Ok(())
}
pub(crate) fn record_needs(d:&mut Value,origin:&str,request:&Value,result:&Value,at:&str)->crate::ApiResult<Value>{
    record_needs_with_ids(d,origin,request,result,at,None)
}
fn record_needs_with_ids(d:&mut Value,origin:&str,request:&Value,result:&Value,at:&str,ids:Option<&[Value]>)->crate::ApiResult<Value>{
    if result.get("videoFrameNeeds").is_none(){return Ok(json!([]));}
    let declarations=result["videoFrameNeeds"].as_array().filter(|r|r.len()<=8).ok_or_else(||crate::bad("Bounded video frame needs required"))?;
    let receipt=crate::model_material_receipt::result_receipt(request,result).map_err(crate::bad)?.ok_or_else(||crate::bad("Frame request requires original paid material receipt"))?;
    if receipt["nativeJobId"]!=origin{return Err(crate::conflict("Frame need origin differs from paid attempt"));}
    let root=crate::row(d,"jobs",origin)?;
    let originating=root.get("originatingAnsweringAttemptId").cloned().unwrap_or_else(||json!(origin));
    if let Some(previous)=root.get("videoFrameNeeds"){
        let prior=previous.as_array().ok_or_else(||crate::conflict("Original frame needs malformed"))?;
        let original_declarations=prior.iter().map(|n|json!({"itemId":rows(n,"affectedRecipientIds").first().cloned().unwrap_or(Value::Null),"postId":n["member"]["postId"],"attachmentIndex":n["asset"]["attachmentIndex"],"requestedTimeOrIntent":n["requestedTimeOrIntent"],"reason":n["reason"]})).collect::<Vec<_>>();
        if original_declarations!=*declarations||prior.iter().any(|n|n["requestingPaidAttemptId"]!=origin||n["originatingAnsweringAttemptId"]!=originating||n["parentPaidResultRef"]!=receipt["paidResultRef"]||validate_need(n).is_err()){return Err(crate::conflict("Original frame needs are immutable"));}
        return Ok(previous.clone());
    }
    let mut needs=Vec::new();let mut seen=std::collections::BTreeSet::new();
    for (ordinal,declaration) in declarations.iter().enumerate(){
        let item=txt(declaration,"itemId");let post_id=txt(declaration,"postId");
        let index=declaration["attachmentIndex"].as_u64().ok_or_else(||crate::bad("Frame source index required"))?;
        if !rows(request,"items").iter().any(|i|i["id"]==item&&i["postId"]==post_id)
            ||!rows(result,"assessments").iter().any(|a|a["itemId"]==item&&a["outcome"]=="needs_attention"&&rows(a,"tags").contains(&json!("missing_context")))
            ||rows(result,"proposals").iter().any(|p|p["itemId"]==item)
            ||txt(declaration,"reason").trim().is_empty()||txt(declaration,"reason").len()>1000
            ||!seen.insert((item.to_owned(),post_id.to_owned(),index)){return Err(crate::bad("Frame need requires an exact held recipient without proposal"));}
        let member=rows(&request["postContextBundle"],"members").iter().find(|m|m["canonicalPostId"]==post_id).ok_or_else(||crate::bad("Frame member not captured"))?;
        let asset=rows(member,"assets").iter().find(|a|a["modality"]=="video"&&a["attachmentIndex"]==index).ok_or_else(||crate::bad("Frame video not captured"))?;
        let need_id=if let Some(ids)=ids{let id=ids.get(ordinal).and_then(Value::as_str).filter(|s|uuid::Uuid::parse_str(s).is_ok()).ok_or_else(||crate::bad("Native frame need identity invalid"))?;id.to_owned()}else{crate::id()};
        let mut need=json!({"schemaVersion":1,"contract":NEED_CONTRACT,"needId":need_id,"companyId":d["account"],
            "originatingAnsweringAttemptId":originating,"requestingPaidAttemptId":origin,"parentPaidResultRef":receipt["paidResultRef"],
            "member":{"postId":post_id,"connectorBinding":member["connectorBinding"]},
            "asset":{"attachmentIndex":index,"attachmentIdentity":asset["attachmentIdentity"],"sourceVersion":asset["sourceVersion"]},
            "baseContextSha256":request["postContextBundle"]["contentSha256"],"affectedRecipientIds":[item],
            "requestedTimeOrIntent":declaration["requestedTimeOrIntent"],"reason":declaration["reason"],
            "budget":{"maxFrames":8,"maxDecodedDurationMs":30000,"maxDecodedFrames":900,"maxBytes":32*1024*1024,"deadlineMs":30000},"createdAt":at});
        if let Ok(source)=retained_source(d,&need){need["asset"]["sourceArtifactRef"]=source["sourceArtifactRef"].clone();need["asset"]["sourceArtifactSha256"]=source["sourceArtifactSha256"].clone();need["sourceDurationMs"]=source["sourceDurationMs"].clone();need["sourceOriginJobId"]=source["originJobId"].clone();}
        need["needSha256"]=json!(hash(&need));validate_need(&need).map_err(crate::bad)?;needs.push(need);
    }
    let job=crate::row_mut(d,"jobs",origin)?;
    if job.get("videoFrameNeeds").is_some(){if job["videoFrameNeeds"]==json!(needs){return Ok(job["videoFrameNeeds"].clone());}return Err(crate::conflict("Original frame needs are immutable"));}
    job["videoFrameNeeds"]=json!(needs);job["frameNeedOutcome"]=json!({"status":if needs.is_empty(){"not_requested"}else{"requested"},"reasonCode":"frame_requested","createdAt":at});
    Ok(json!(needs))
}
/// Bounded first-stage storage may append only this exact native derivation.
/// Caller supplies the complete source/context projection captured for the
/// original first settlement; UUIDs are preserved for comparison, not reminted.
pub(crate) fn validate_created(before:&Value,new_job:&Value)->crate::ApiResult<()> {
    let origin=txt(new_job,"id");let old=crate::row(before,"jobs",origin)?;
    if old.get("videoFrameNeeds").is_some()||old.get("frameNeedOutcome").is_some()
        ||new_job["preparationStages"]["first"]["status"]!="completed"||!old["preparationStages"]["first"].is_null(){return Err(crate::conflict("Native first frame derivation is append-only"));}
    let declared=new_job["videoFrameNeeds"].as_array().ok_or_else(||crate::bad("Native frame needs array missing"))?;
    let ids=declared.iter().map(|n|n["needId"].clone()).collect::<Vec<_>>();let unique=ids.iter().filter_map(Value::as_str).collect::<std::collections::BTreeSet<_>>();
    if unique.len()!=ids.len(){return Err(crate::bad("Native frame need IDs duplicated"));}
    let result=&new_job["preparationStages"]["first"]["result"];let at=new_job["preparationStages"]["first"]["at"].as_str().ok_or_else(||crate::bad("Native first timestamp missing"))?;
    let mut expected=before.clone();record_needs_with_ids(&mut expected,origin,&old["prepareBundle"]["request"],result,at,Some(&ids))?;
    let expected=crate::row(&expected,"jobs",origin)?;
    if expected.get("videoFrameNeeds")!=new_job.get("videoFrameNeeds")||expected.get("frameNeedOutcome")!=new_job.get("frameNeedOutcome")
        ||result["nativeVideoFrameNeeds"]!=new_job["videoFrameNeeds"]{return Err(crate::conflict("Native frame needs differ from original paid derivation"));}Ok(())
}
pub(crate) fn validate_result(d:&Value,need:&Value,result:&Value)->Result<(),&'static str>{
    current_need(d,need)?;
    let mut unsigned=result.clone();unsigned.as_object_mut().ok_or("frame_result_invalid")?.remove("resultSha256");
    if result["schemaVersion"]!=1||result["contract"]!=RESULT_CONTRACT||result["resultSha256"]!=hash(&unsigned)
        ||result["needId"]!=need["needId"]||result["needSha256"]!=need["needSha256"]||result["companyId"]!=need["companyId"]
        ||result["member"]!=need["member"]||result["asset"]!=need["asset"]||result["status"]!="complete"
        ||result["requestedTimeOrIntent"]!=need["requestedTimeOrIntent"]||!result["coverage"].is_object()
        ||txt(result,"toolVersion").is_empty()||rows(result,"frames").is_empty()||rows(result,"frames").len()>need["budget"]["maxFrames"].as_u64().unwrap_or(0) as usize{return Err("frame_result_invalid");}
    for frame in rows(result,"frames"){
        if !sha(&frame["sha256"])||frame["artifact"]["sha256"]!=frame["sha256"]||frame["actualPts"].as_i64().is_none()
            ||frame["timeBase"]["num"].as_u64().is_none_or(|n|n==0)||frame["timeBase"]["den"].as_u64().is_none_or(|n|n==0)
            ||frame["requestedTimestampMs"].as_u64().is_none(){return Err("frame_result_pts_unproven");}
    }
    let owner=crate::row(d,"jobs",txt(result,"frameJobId")).map_err(|_|"frame_result_native_owner_missing")?;
    if owner["purpose"]!="targeted_video_frames"||owner["status"]!="completed"||owner["frameNeed"]!=*need||owner["frameResult"]!=*result
        ||result["decoderResult"]["status"]!="complete"||result["decoderResult"]["frames"]!=result["frames"]{return Err("frame_result_native_owner_unproven");}Ok(())
}
pub(crate) fn frame_refs(result:&Value,need:&Value)->Vec<Value>{rows(result,"frames").iter().map(|frame|{
    let mut reference=frame.clone();for (key,value) in [("needId",need["needId"].clone()),("needSha256",need["needSha256"].clone()),("resultSha256",result["resultSha256"].clone()),("frameJobId",result["frameJobId"].clone()),("companyId",need["companyId"].clone()),("postId",need["member"]["postId"].clone()),("connectorBinding",need["member"]["connectorBinding"].clone()),("attachmentIndex",need["asset"]["attachmentIndex"].clone()),("attachmentIdentity",need["asset"]["attachmentIdentity"].clone()),("sourceVersion",need["asset"]["sourceVersion"].clone()),("sourceArtifactSha256",need["asset"]["sourceArtifactSha256"].clone()),("affectedRecipientIds",need["affectedRecipientIds"].clone())]{reference[key]=value;}reference
}).collect()}
fn hold(d:&mut Value,origin:&str,reason:&str)->crate::ApiResult<()> {crate::row_mut(d,"jobs",origin)?["frameNeedOutcome"]=json!({"status":"held","reasonCode":reason,"retryAuthorized":false,"at":crate::now()});Ok(())}
/// Only this parent's requested needs fan in. Local acquisition failures become
/// finite, actionable holds; no decoder retry loop or model is hidden here.
pub(crate) async fn run_pending(app:&crate::App,origin:&str)->crate::ApiResult<Option<Vec<Value>>>{
    let snapshot=app.db.read_preparation_context(origin).await?;let root=crate::row(&snapshot,"jobs",origin)?;
    let needs=rows(root,"videoFrameNeeds").to_vec();if needs.is_empty(){return Ok(None);}
    if root.get("originatingAnsweringAttemptId").is_some(){app.change_job(origin,|d|hold(d,origin,"answering_repair_limit_exhausted")).await?;return Ok(None);}
    if root["preparationStages"]["first"]["status"]!="completed"{return Err(crate::conflict("Frame acquisition requires completed original paid answer"));}
    for need in &needs{if let Err(reason)=current_need(&snapshot,need){app.change_job(origin,|d|hold(d,origin,reason)).await?;return Ok(None);}}
    let store=crate::media_fullframes::store().map_err(|e|crate::internal(&e))?;
    let tools=match crate::media_frame_sample_decode::SampleTools::from_env(&app.lifecycle_work,std::time::Duration::from_secs(30)).await{Ok(v)=>v,Err(e)=>{app.change_job(origin,|d|hold(d,origin,&e)).await?;return Ok(None)}};
    let mut results=Vec::new();let mut base=base_usage(&root["prepareBundle"]["request"]);
    drop(snapshot);
    for need in needs{
        let snapshot=app.db.read_preparation_context(origin).await?;
        if let Err(reason)=current_need(&snapshot,&need){app.change_job(origin,|d|hold(d,origin,reason)).await?;return Ok(None);}
        let existing=rows(&snapshot,"jobs").iter().filter(|j|j["purpose"]=="targeted_video_frames"&&j["frameNeed"]["needId"]==need["needId"]).collect::<Vec<_>>();
        if existing.len()>1{app.change_job(origin,|d|hold(d,origin,"frame_job_ambiguous")).await?;return Ok(None);}
        if let Some(job)=existing.first(){
            if job["status"]!="completed"{app.change_job(origin,|d|hold(d,origin,"frame_job_unresolved_or_failed")).await?;return Ok(None);}
            let result=job["frameResult"].clone();
            if validate_result(&snapshot,&need,&result).is_err()||crate::media_frame_sample_decode::verify_sample_result(&store,&job["framePlan"],&result["decoderResult"],&tools).is_err(){app.change_job(origin,|d|hold(d,origin,"frame_result_reuse_unproven")).await?;return Ok(None);}
            add_usage(&mut base,&result);results.push(result);continue;
        }
        let source=match retained_source(&snapshot,&need){Ok(v)=>v,Err(reason)=>{app.change_job(origin,|d|hold(d,origin,reason)).await?;return Ok(None)}};
        if source["sourceArtifactRef"]!=need["asset"]["sourceArtifactRef"]||source["sourceArtifactSha256"]!=need["asset"]["sourceArtifactSha256"]{app.change_job(origin,|d|hold(d,origin,"frame_source_not_in_original_need")).await?;return Ok(None);}
        let proof=json!({"schemaVersion":1,"kind":"retained_video_source","companyId":need["companyId"],"member":need["member"],"asset":need["asset"],"sourceDurationMs":source["sourceDurationMs"],
            "verifiedReceipt":{"source":source["sourceArtifactRef"],"originJobId":source["originJobId"],"checkpoint":source["sourceReceipt"]},
            "verifiedFile":{"sha256":source["sourceArtifactSha256"],"bytes":source["sourceArtifactRef"]["bytes"],"receiptSha256":hash(&json!({"source":source["sourceArtifactRef"],"originJobId":source["originJobId"],"checkpoint":source["sourceReceipt"]}))}});
        let proof_ref=match crate::media_frame_sample_decode::retain_source_proof(&store,&proof){Ok(v)=>v,Err(e)=>{app.change_job(origin,|d|hold(d,origin,&e)).await?;return Ok(None)}};
        let known=need["requestedTimeOrIntent"]["kind"]=="known_range";
        let profile=json!({"id":if known{crate::media_frame_sample::RANGE_PROFILE}else{crate::media_frame_sample::OVERVIEW_PROFILE},"version":1,"maxFrames":8,"windowMs":1000,
            "maxImageBytes":8*1024*1024,"maxArtifactBytes":128*1024*1024,"maxTotalPixels":64000000,"maxDecodedDurationMs":30000,"maxDecodedFrames":900,"maxPrerollMs":10000,"deadlineMs":30000,"rangeStepMs":4000,"overviewFrames":6});
        let input=json!({"schemaVersion":1,"needId":need["needId"],"needSha256":need["needSha256"],"companyId":need["companyId"],"member":need["member"],"asset":need["asset"],"sourceProofRef":proof_ref,"sourceDurationMs":source["sourceDurationMs"],"requestedTimeOrIntent":need["requestedTimeOrIntent"],"profile":profile,"baseUsage":base,"transportLimits":{"maxImages":16,"maxBytes":32*1024*1024,"maxPixels":64000000}});
        let plan=match crate::media_frame_sample::plan_sample(&input){Ok(v)=>v,Err(e)=>{app.change_job(origin,|d|hold(d,origin,&e)).await?;return Ok(None)}};
        drop(snapshot);
        let token=app.lifecycle_admission_token(crate::runtime_lifecycle::AdmissionClass::Media).await?;let job_id=crate::id();let lease=crate::id();
        app.change(|d|{crate::runtime_lifecycle::require_admission(d,&token,crate::runtime_lifecycle::AdmissionClass::Media)?;current_need(d,&need).map_err(crate::conflict)?;
            if rows(d,"jobs").iter().any(|j|j["purpose"]=="targeted_video_frames"&&j["frameNeed"]["needId"]==need["needId"]){return Err(crate::conflict("Frame need already reserved"));}
            let company=d["account"].clone();crate::list_mut(d,"jobs").push(json!({"id":job_id,"kind":"media","purpose":"targeted_video_frames","status":"running","account":company,"connectorBinding":need["member"]["connectorBinding"],"refId":need["member"]["postId"],"frameNeed":need,"framePlan":plan,"frameLease":{"id":lease,"epoch":1,"runtimeId":token.runtime_id,"runtimeEpoch":token.epoch},"createdAt":crate::now()}));Ok(())}).await?;
        let decoded=crate::media_frame_sample_decode::decode_sample(&store,&plan,&tools,&app.lifecycle_work).await;
        let decoder=match decoded{Ok(v)if v["status"]=="complete"&&!rows(&v,"frames").is_empty()&&crate::media_frame_sample_decode::verify_sample_result(&store,&plan,&v,&tools).is_ok()=>v,
            other=>{let reason=other.err().unwrap_or_else(||"frame_acquisition_incomplete".into());app.change(|d|{let j=crate::row_mut(d,"jobs",&job_id)?;j["status"]=json!("failed");j["error"]=json!(reason);j["finishedAt"]=json!(crate::now());hold(d,origin,&reason)}).await?;return Ok(None)}};
        let mut result=json!({"schemaVersion":1,"contract":RESULT_CONTRACT,"needId":need["needId"],"needSha256":need["needSha256"],"companyId":need["companyId"],"member":need["member"],"asset":need["asset"],"status":"complete","frameJobId":job_id,"requestedTimeOrIntent":need["requestedTimeOrIntent"],"coverage":decoder["coverage"],"toolVersion":decoder["toolVersion"],"frames":decoder["frames"],"decoderResult":decoder});result["resultSha256"]=json!(hash(&result));
        app.change(|d|{current_need(d,&need).map_err(crate::conflict)?;let j=crate::row_mut(d,"jobs",&job_id)?;
            if j["status"]!="running"||j["frameLease"]["id"]!=lease||j["framePlan"]!=plan||j["frameNeed"]!=need{return Err(crate::conflict("Frame lease changed before durable result"));}
            j["frameResult"]=result.clone();j["status"]=json!("completed");j["finishedAt"]=json!(crate::now());validate_result(d,&need,&result).map_err(crate::conflict)?;Ok(())}).await?;
        add_usage(&mut base,&result);results.push(result);
    }
    app.change_job(origin,|d|{crate::row_mut(d,"jobs",origin)?["frameNeedOutcome"]=json!({"status":"complete","resultSha256":results.iter().map(|r|r["resultSha256"].clone()).collect::<Vec<_>>(),"at":crate::now()});Ok(())}).await?;Ok(Some(results))
}
fn base_usage(request:&Value)->Value{let photos=rows(&request["postContextBundle"],"members").iter().flat_map(|m|rows(m,"assets")).filter(|a|a["modality"]=="photo").collect::<Vec<_>>();json!({"imageCount":photos.len(),"imageBytes":photos.iter().map(|a|a["photo"]["artifact"]["bytes"].as_u64().unwrap_or(0)).sum::<u64>(),"pixels":photos.iter().map(|a|a["photo"]["width"].as_u64().unwrap_or(0).saturating_mul(a["photo"]["height"].as_u64().unwrap_or(0))).sum::<u64>()})}
fn add_usage(base:&mut Value,result:&Value){for (key,used) in [("imageCount","imageCount"),("imageBytes","imageBytes"),("pixels","pixels")]{base[key]=json!(base[key].as_u64().unwrap_or(0).saturating_add(result["decoderResult"]["used"][used].as_u64().unwrap_or(0)));}}
#[cfg(test)]#[path="video_frame_work_tests.rs"]mod tests;
#[cfg(test)]pub(crate) use tests::first_needs_fixture;
