//! Durable account-scoped media preparation. One group job, bounded source attempts.
//! This module does not install a runtime, publish comments, or infer a full transcript.
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};

// Also covers the interval between cancellation and the child process exiting.
static MEDIA_GATE: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
const PURPOSE: &str = "auto_media";
fn current_job(job:&Value)->bool {job["purpose"]==PURPOSE && job["visualContractVersion"]==2}

fn text<'a>(v: &'a Value, key: &str) -> &'a str { v[key].as_str().unwrap_or("") }
fn rows<'a>(v: &'a Value, key: &str) -> &'a [Value] { v[key].as_array().map(Vec::as_slice).unwrap_or(&[]) }
fn video(post: &Value) -> bool {
    super::knowledge::is_video_post(post)
}
fn account_scope(d: &Value) -> super::ApiResult<&str> {
    let binding = super::active_binding(d)?;
    super::bridge_account(&binding)?;
    d["account"].as_str().ok_or_else(|| super::conflict("Media account is not configured"))
}
fn allowed(d: &Value) -> bool {
    account_scope(d).is_ok()
}
fn group(d: &Value, post: &Value) -> Option<String> {
    QueueIndex::new(d).groups.get(text(post,"id")).cloned()
}
struct QueueIndex {
    groups: BTreeMap<String,String>,
    candidates: BTreeMap<String,Vec<Value>>,
    open_groups: BTreeSet<String>,
}
impl QueueIndex {
    fn new(d:&Value)->Self {
        let groups=account_scope(d).map(|scope|super::knowledge::visual_groups(d,scope).into_iter().map(|(id,key)|{
            let post=rows(d,"posts").iter().find(|p|p["id"]==id).unwrap();
            let duration=post["durationMs"].as_u64().or_else(||post["durationSeconds"].as_f64().filter(|v|v.is_finite()&&*v>0.0).map(|v|(v*1000.0).round() as u64));
            (id,format!("visual-v2:{key}:duration:{}",duration.map(|v|v.to_string()).unwrap_or_default()))
        }).collect::<BTreeMap<_,_>>()).unwrap_or_default();
        let mut candidates:BTreeMap<String,Vec<Value>>=BTreeMap::new();
        let mut post_keys=BTreeMap::new();
        for post in rows(d,"posts") {
            if !video(post)||text(post,"id").is_empty(){continue;}
            if let Some(key)=groups.get(text(post,"id")) {
                candidates.entry(key.clone()).or_default().push(post.clone());
                if !text(post,"postKey").is_empty(){post_keys.insert(text(post,"postKey"),key);}
            }
        }
        for posts in candidates.values_mut(){posts.sort_by(|a,b|text(a,"id").cmp(text(b,"id")));}
        let mut open_groups=BTreeSet::new();
        if let Ok(binding)=super::active_binding(d) {
            for item in rows(d,"items") {
                if matches!(text(item,"providerStatus"),"new"|"inprogress"|"in_progress")
                    && matches!(text(item,"workflow"),"attention"|"prepared"|"waiting"|"wait"|"active")
                    && super::bound_item(&binding,item).is_ok() {
                    if let Some(key)=groups.get(text(item,"postId")){open_groups.insert(key.clone());}
                    if let Some(key)=post_keys.get(text(item,"postKey")){open_groups.insert((*key).clone());}
                }
            }
        }
        Self{groups,candidates,open_groups}
    }
    fn posts(&self,key:&str)->&[Value]{self.candidates.get(key).map(Vec::as_slice).unwrap_or(&[])}
}
fn open_post(d: &Value, post: &Value) -> bool {
    let Ok(binding) = super::active_binding(d) else { return false; };
    rows(d, "items").iter().any(|item| {
        matches!(text(item, "providerStatus"), "new" | "inprogress" | "in_progress")
            && matches!(text(item, "workflow"), "attention" | "prepared" | "waiting" | "wait" | "active")
            && super::bound_item(&binding, item).is_ok()
            && (item["postId"] == post["id"] || (!text(post, "postKey").is_empty() && item["postKey"] == post["postKey"]))
    })
}
fn candidates(d: &Value, key: &str) -> Vec<Value> {
    QueueIndex::new(d).posts(key).to_vec()
}
fn source_attempts<'a>(d:&'a Value,post:&Value)->Vec<(&'a Value,usize,&'a Value)>{
    let source=account_scope(d).ok().and_then(|scope|super::knowledge::media_source_key(post,scope));
    let version=account_scope(d).ok().map(|scope|super::media_fullframes::source_version(post,scope));
    let mut matches=Vec::new();
    for job in rows(d,"jobs").iter().filter(|j|j["kind"]=="media"&&current_job(j)){
        for(index,attempt)in rows(job,"sourceAttempts").iter().enumerate(){
            let same_post=attempt["postId"]==post["id"]||(!text(post,"postKey").is_empty()&&attempt["postKey"]==post["postKey"]);
            let current_revision=attempt["sourceVersion"].is_null()||version.as_ref().is_some_and(|v|attempt["sourceVersion"]==*v);
            if (same_post&&current_revision)||(!same_post&&source.as_ref().is_some_and(|s|attempt["sourceKey"]==*s)){matches.push((job,index,attempt));}
        }
    }matches
}
fn current_source_job(d:&Value,job:&Value)->bool{
    let p=&job["result"]["visualProgress"];if p["schemaVersion"]!=2{return true;}
    rows(d,"posts").iter().find(|post|post["id"]==p["sourcePostId"]).is_some_and(|post|account_scope(d).is_ok_and(|scope|p["sourceVersion"]==super::media_fullframes::source_version(post,scope)))
}
fn attempted(d: &Value, post: &Value) -> bool {
    let Some(scope)=account_scope(d).ok() else{return true;};
    let source = super::knowledge::media_source_key(post, scope);
    let opaque=rows(d, "jobs").iter().filter(|j| j["kind"] == "media").any(|job| {
        job["purpose"] != PURPOSE && (job["refId"] == post["id"]
            || source.as_ref().is_some_and(|s| rows(d,"posts").iter().find(|p|p["id"]==job["refId"])
                .and_then(|p|super::knowledge::media_source_key(p,scope)).as_ref()==Some(s)))
    });
    if opaque{return true;}
    let attempts=source_attempts(d,post);
    match attempts.as_slice(){
        []=>false,
        [(job,_,attempt)]=>!(current_job(job)&&job["status"]!="cancelled"
            && attempt["retryPermit"]["verifiedNoMediaProcesses"]==true && !text(&attempt["retryPermit"],"id").is_empty()
            && (attempt["status"]=="interrupted" || attempt["status"]=="failed"
                && attempt["retryPermit"]["kind"]=="download_failure"
                && attempt["retryPermit"]["sourceVersion"]==super::media_fullframes::source_version(post,scope)
                && super::active_binding(d).is_ok_and(|binding|attempt["retryPermit"]["connectorBinding"]==binding.to_json()))),
        _=>true,
    }
}
fn next_source(d: &Value, job: &Value) -> Option<Value> {
    next_source_indexed(d,job,&QueueIndex::new(d))
}
fn next_source_indexed(d:&Value,job:&Value,index:&QueueIndex)->Option<Value>{
    if job["downloadRetry"].is_object(){
        let permit=&job["downloadRetry"];
        let post=index.posts(text(job,"groupKey")).iter().find(|p|p["id"]==permit["postId"])?;
        let scope=account_scope(d).ok()?;
        if permit["sourceVersion"]!=super::media_fullframes::source_version(post,scope)
            || permit["connectorBinding"]!=super::active_binding(d).ok()?.to_json(){return None;}
        return (!attempted(d,post)).then(||post.clone());
    }
    let mut ready: Vec<_> = index.posts(text(job,"groupKey")).iter().filter(|p| !attempted(d,p)).cloned().collect();
    let previous = rows(job, "sourceAttempts").last().map(|a| text(a, "channel")).unwrap_or("");
    ready.sort_by_key(|p| (!previous.is_empty() && text(p, "channel") == previous, text(p, "id").to_owned()));
    ready.into_iter().next()
}
fn has_required_media(d: &Value, post: &Value, at: &str) -> super::ApiResult<bool> {
    super::knowledge::TranscriptLookup::new(d,at).and_then(|lookup|lookup.ready(post)).map_err(|e|super::bad(&e))
}
fn covered(d: &Value, job: &Value, at: &str) -> super::ApiResult<bool> {
    covered_indexed(job,&QueueIndex::new(d),&super::knowledge::TranscriptLookup::new(d,at).map_err(|e|super::bad(&e))?)
}
fn covered_indexed(job:&Value,index:&QueueIndex,transcripts:&super::knowledge::TranscriptLookup)->super::ApiResult<bool>{
    for post in index.posts(text(job,"groupKey")){if transcripts.ready(post).map_err(|e|super::bad(&e))?{return Ok(true);}}
    Ok(false)
}
fn enqueue(d: &mut Value, post: &Value, at: &str, manual: bool) -> super::ApiResult<Value> {
    let index=QueueIndex::new(d);
    let transcripts=super::knowledge::TranscriptLookup::new(d,at).map_err(|e|super::bad(&e))?;
    enqueue_indexed(d,post,at,manual,&index,&transcripts)
}
fn enqueue_indexed(d:&mut Value,post:&Value,at:&str,manual:bool,index:&QueueIndex,transcripts:&super::knowledge::TranscriptLookup)->super::ApiResult<Value>{
    if !allowed(d) || !video(post) { return Err(super::bad("Post has no eligible account video")); }
    let key = index.groups.get(text(post,"id")).ok_or_else(|| super::bad("Media group identity unavailable"))?;
    if transcripts.ready(post).map_err(|e|super::bad(&e))? {
        return Ok(json!({"reused":true,"status":"completed","postId":post["id"]}));
    }
    if let Some(id) = rows(d, "jobs").iter().find(|j| current_job(j) && j["groupKey"] == *key && current_source_job(d,j)).map(|j|text(j,"id").to_owned()) {
        let job = super::row_mut(d, "jobs", &id)?;
        if manual { job["manualRequested"] = json!(true); }
        return Ok(json!({"jobId":job["id"],"status":job["status"],"deduplicated":true}));
    }
    let id = super::id();
    let mut job = json!({"id":id,"kind":"media","purpose":PURPOSE,"visualContractVersion":2,"account":account_scope(d)?,"refId":post["id"],"groupKey":key,"status":"queued","sourceAttempts":[],"fallbackAllowed":true,"manualRequested":manual,"createdAt":at});
    if next_source_indexed(d, &job,index).is_none() {
        job["status"] = json!("failed");
        job["finishedAt"] = json!(at);
        job["error"] = json!("All known media sources have already been attempted");
    }
    let status = job["status"].clone();
    super::list_mut(d, "jobs").push(job);
    Ok(json!({"jobId":id,"status":status}))
}

/// Idempotent reconciliation; call under App.change. Does not launch processes.
pub(crate) fn reconcile(d: &mut Value, at: &str) -> super::ApiResult<()> {
    if !allowed(d) { return Ok(()); }
    if d["mediaQueue"]["inputDigest"] == input_digest(d, at) { return Ok(()); }
    let index=QueueIndex::new(d);
    let transcripts=super::knowledge::TranscriptLookup::new(d,at).map_err(|e|super::bad(&e))?;
    for key in &index.open_groups {
        if let Some(post)=index.posts(key).first(){enqueue_indexed(d,post,at,false,&index,&transcripts)?;}
    }
    let jobs: Vec<_> = rows(d, "jobs").iter().filter(|j| current_job(j) && matches!(text(j, "status"), "queued" | "failed" | "interrupted" | "paused")).cloned().collect();
    for job in jobs {
        if job["result"]["visualProgress"]["schemaVersion"]==2 {continue;}
        let done = covered_indexed(&job,&index,&transcripts)?;
        let retry = job["fallbackAllowed"] == true && next_source_indexed(d,&job,&index).is_some();
        let wanted = job["manualRequested"] == true || index.open_groups.contains(text(&job,"groupKey"));
        let stored = super::row_mut(d, "jobs", text(&job, "id"))?;
        if done {
            stored["status"] = json!("completed");
            stored["finishedAt"] = json!(at);
            stored["result"] = json!({"reused":true});
            stored.as_object_mut().unwrap().remove("error");
        } else if retry && wanted {
            stored["status"] = json!("queued");
            stored["finishedAt"] = Value::Null;
        } else if retry {
            stored["status"] = json!("paused");
        } else if stored["status"] == "queued" || stored["status"] == "interrupted" {
            stored["status"] = json!("failed");
            stored["finishedAt"] = json!(at);
            stored["error"] = json!("No untried eligible media source remains");
        }
    }
    // A successful shared result supersedes an old scheduling/download banner;
    // the sourceAttempts ledger retains each historical interruption/failure.
    for job in super::list_mut(d,"jobs") {
        if current_job(job) && job["status"]=="completed" { job.as_object_mut().unwrap().remove("error"); }
    }
    d["mediaQueue"] = json!({"inputDigest":input_digest(d, at)});
    Ok(())
}

fn input_digest(d: &Value, at: &str) -> String {
    // The time bucket also rechecks evidence validity windows. Unchanged queues
    // avoid repeated full knowledge validation and title grouping every tick.
    let minute = chrono::DateTime::parse_from_rfc3339(at).map(|v|v.timestamp()/60).unwrap_or(0);
    let items: Vec<_> = rows(d,"items").iter().map(|i|json!([i["id"],i["objectId"],i["itemId"],i["connectorBinding"],i["postId"],i["postKey"],i["providerStatus"],i["workflow"]])).collect();
    let versions: Vec<_> = rows(d,"knowledge_versions").iter().map(|v|json!([v["id"],v["hash"]])).collect();
    let jobs: Vec<_> = rows(d,"jobs").iter().filter(|j|j["kind"]=="media").collect();
    let input = json!([d["account"],d["connectorBinding"],d["posts"],items,d["knowledge_entries"],versions,jobs,minute,super::media_visual::VERSION]);
    format!("{:x}",Sha256::digest(input.to_string().as_bytes()))
}

/// Run after generic startup recovery. Interrupted dispatches stay consumed
/// unless a local owner explicitly records a bounded process-cessation permit.
pub(crate) fn recover(d: &mut Value, at: &str) -> super::ApiResult<()> {
    for job in super::list_mut(d, "jobs") {
        if !current_job(job) { continue; }
        if job["result"]["visualProgress"]["schemaVersion"]==2 {super::media_fullframes::recover(job).map_err(|e|super::bad(&e))?;continue;}
        if matches!(text(job, "status"), "running" | "interrupted" | "queued")
            || (job["status"] != "cancelled" && rows(job,"sourceAttempts").iter().any(|a|a["status"]=="running")) {
            for attempt in super::list_mut(job, "sourceAttempts") {
                if attempt["status"] == "running" {
                    attempt["status"] = json!("interrupted");
                    attempt["finishedAt"] = json!(at);
                }
            }
            job["status"] = json!("interrupted");
        }
    }
    reconcile(d, at)
}

fn take_source(d: &mut Value, job_id: &str, at: &str) -> super::ApiResult<Option<Value>> {
    let job = super::row(d, "jobs", job_id)?.clone();
    if !allowed(d) || job["status"] != "running" { return Ok(None); }
    // A twin's result may arrive between the previous failure and fallback
    // reservation. Reuse it under the same App.change transaction as claiming.
    if covered(d, &job, at)? { return Ok(None); }
    if job["manualRequested"] != true && !candidates(d,text(&job,"groupKey")).iter().any(|p|open_post(d,p)) { return Ok(None); }
    let Some(post) = next_source(d, &job) else { return Ok(None); };
    let history=source_attempts(d,&post);
    let ordinal=history.len()+1;
    let retry_of=history.last().map(|(job,index,attempt)|json!({"jobId":job["id"],"attemptIndex":index,"permitId":attempt["retryPermit"]["id"]}));
    let source_key=account_scope(d).ok().and_then(|scope|super::knowledge::media_source_key(&post,scope));
    let source_version=super::media_fullframes::source_version(&post,account_scope(d)?);
    let stored = super::row_mut(d, "jobs", job_id)?;
    stored["refId"] = post["id"].clone();
    super::list_mut(stored, "sourceAttempts").push(json!({"id":super::id(),"postId":post["id"],"postKey":post["postKey"],"sourceKey":source_key,"sourceVersion":source_version,"channel":post["channel"],"status":"running","startedAt":at,"attemptNumber":ordinal,"retryOf":retry_of}));
    Ok(Some(post))
}
fn claim(d: &mut Value, at: &str) -> super::ApiResult<Option<(String, Value)>> {
    reconcile(d, at)?;
    if rows(d,"jobs").iter().any(|j|j["kind"]=="media"&&j["status"]=="running"){return Ok(None);}
    let mut candidates:Vec<_>=rows(d,"jobs").iter().filter(|j|current_job(j)&&j["status"]=="queued").map(|j|(text(j,"startedAt").to_owned(),text(j,"id").to_owned())).collect();
    candidates.sort();
    for (_,id) in candidates {
        let existing=super::row(d,"jobs",&id)?["result"]["visualProgress"].clone();
        let account=account_scope(d)?.to_owned();let binding=super::active_binding(d)?;
        let resumed=if existing["schemaVersion"]==2 {
            let post=rows(d,"posts").iter().find(|p|p["id"]==existing["sourcePostId"]).cloned();
            let valid=post.as_ref().is_some_and(|p|existing["account"]==account&&existing["connectorBinding"]==binding.to_json()&&existing["sourceVersion"]==super::media_fullframes::source_version(p,&account)&&existing["materialEpoch"]==material_epoch(d,p)&&!matches!(text(&existing,"phase"),"held"|"complete"));
            if !valid {
                let job=super::row_mut(d,"jobs",&id)?;job["status"]=json!("failed");job["finishedAt"]=json!(at);job["error"]=json!("media_resume_source_or_material_changed");
                let p=&mut job["result"]["visualProgress"];p["resumePhase"]=p["phase"].clone();p["phase"]=json!("held");p["leaseId"]=Value::Null;
                continue;
            }
            post
        }else{None};
        let stored=super::row_mut(d,"jobs",&id)?;stored["status"]=json!("running");stored["startedAt"]=json!(at);
        let post=if existing["schemaVersion"]==2{resumed}else{take_source(d,&id,at)?};
        let Some(post)=post else {
            let done=covered(d,super::row(d,"jobs",&id)?,at)?;let job=super::row_mut(d,"jobs",&id)?;
            job["status"]=json!(if done{"completed"}else{"paused"});job["finishedAt"]=if done{json!(at)}else{Value::Null};
            if done{job["result"]=json!({"reused":true});job.as_object_mut().unwrap().remove("error");}
            continue;
        };
        let mut progress=if existing["schemaVersion"]==2{existing}else{let mut p=super::media_fullframes::initial(&account,&binding.to_json(),&post,at);p["materialEpoch"]=json!(material_epoch(d,&post));p};
        super::media_fullframes::claim(&mut progress,&super::id()).map_err(|e|super::bad(&e))?;
        super::row_mut(d,"jobs",&id)?["result"]=json!({"visualProgress":progress});
        return Ok(Some((id,post)));
    }
    Ok(None)
}
pub(crate) fn claim_when_ready(d: &mut Value, at: &str, runtime_ready: bool) -> super::ApiResult<Option<(String, Value)>> {
    if !runtime_ready { return Ok(None); }
    claim(d, at)
}
fn downloadable_failure(reason: &str) -> bool {
    let reason = reason.to_ascii_lowercase();
    ["source_download_failed", "source_file_missing", "source_url_missing", "media_source_missing", "vk_page_failed", "vk_media_unresolved", "vk_media_download_failed", "vk_source_file_missing", "direct_media_download_failed", "download_timeout"].iter().any(|code| reason.contains(code))
}
fn mark_attempt(d: &mut Value, job_id: &str, post: &Value, error: Option<&str>, at: &str) -> super::ApiResult<()> {
    let job = super::row_mut(d, "jobs", job_id)?;
    if job["status"] != "running" { return Err(super::conflict("Media job no longer owns this attempt")); }
    let attempt = super::list_mut(job, "sourceAttempts").iter_mut().find(|a| a["postId"] == post["id"] && a["status"] == "running").ok_or_else(|| super::conflict("Media source attempt is no longer active"))?;
    attempt["status"] = json!(if error.is_some() { "failed" } else { "completed" });
    attempt["finishedAt"] = json!(at);
    if let Some(error) = error { attempt["error"] = json!(error); }
    job["fallbackAllowed"] = json!(error.is_some_and(downloadable_failure));
    if error.is_some_and(downloadable_failure)&&job["result"]["visualProgress"]["phase"]=="download" {job["result"]["visualProgress"]=Value::Null;}
    if error.is_none() { job.as_object_mut().unwrap().remove("error"); }
    Ok(())
}
fn annotate(result: &mut Value, post: &Value) {
    // Legacy/cache imports retain their existing coverage instead of claiming a
    // fresh Whisper run or guessing the duration of an older transcript.
    if result["reused"] == true { return; }
    let Some(materials) = result["materials"].as_array_mut() else { return; };
    for material in materials {
        if material["kind"] == "transcript" && !material["transcription"].is_object() {
            material["transcription"] = json!({"model":"local-whisper","partial":true,"maxAudioSeconds":900,"coverage":"first_900_seconds_or_shorter","sourcePostKey":post["postKey"]});
        }
    }
}
fn material_epoch(d:&Value,post:&Value)->String{
    let heads:Vec<_>=rows(d,"knowledge_entries").iter().filter(|e|matches!(text(e,"kind"),"transcript"|"visual_context")&&rows(&e["scope"],"postKeys").iter().any(|k|*k==post["postKey"])).map(|e|json!([e["id"],e["currentVersionId"]])).collect();
    super::media_fullframes::hash(&json!(heads))
}
async fn run_full(app:&super::App,id:&str,post:Value)->super::ApiResult<Value>{
    let _guard=MEDIA_GATE.lock().await;
    let d=app.read().await?;let job=super::row(&d,"jobs",id)?;let progress=job["result"]["visualProgress"].clone();
    if job["status"]!="running"||progress["sourceVersion"]!=super::media_fullframes::source_version(&post,account_scope(&d)?)||progress["connectorBinding"]!=super::active_binding(&d)?.to_json(){return Err(super::conflict("Media worker binding changed"));}
    let current=super::row(&d,"posts",text(&post,"id"))?;
    if super::media_fullframes::source_version(current,account_scope(&d)?)!=progress["sourceVersion"]||material_epoch(&d,current)!=progress["materialEpoch"]{return Err(super::conflict("Media source or material version changed"));}
    let source=if progress["phase"]=="download"{
        let binding=super::active_binding(&d)?;
        let projection=app.bridge("media_source",json!({"account":super::bridge_account(&binding)?,"postId":post["id"],"post":post})).await?;
        Some(super::media_processing::MediaSource::from_projection(&projection,account_scope(&d)?,text(&post,"postKey")).map_err(|e|super::bad(&e))?)
    }else{None};
    let result=match super::media_processing::full::step(app,id,source.as_ref(),progress.clone()).await{
        Ok(v)=>v,
        Err(error)=>{
            if progress["phase"]=="download"&&downloadable_failure(&error){app.change(|d|mark_attempt(d,id,&post,Some(&error),&super::now())).await?;}
            return Err(super::bad(&error));
        }
    };
    if result["resume"]==true{return Ok(result);}
    let expected=result["visualProgress"].clone();
    app.change(|d|{
        let current=super::row(d,"posts",text(&post,"id"))?;
        if super::active_binding(d)?.to_json()!=progress["connectorBinding"]||super::media_fullframes::source_version(current,account_scope(d)?)!=progress["sourceVersion"]||material_epoch(d,current)!=progress["materialEpoch"]{return Err(super::conflict("Media source or material version changed"));}
        let job=super::row(d,"jobs",id)?;
        if job["status"]!="running"||job["result"]["visualProgress"]!=expected{return Err(super::conflict("Media worker lease changed"));}
        super::merge_materials(d,&result)?;
        if !has_required_media(d,&post,&super::now())?{return Err(super::conflict("Full media evidence not admitted"));}
        mark_attempt(d,id,&post,None,&super::now())?;
        Ok(json!({"processed":true,"sourcePostKey":post["postKey"]}))
    }).await
}
async fn run(app: &super::App, id: &str, mut post: Value) -> super::ApiResult<Value> {
    if super::row(&app.read().await?,"jobs",id)?["visualContractVersion"]==2{return run_full(app,id,post).await;}
    let _guard = MEDIA_GATE.lock().await;
    loop {
        // Check again after waiting for a cancelled predecessor to exit.
        let d = app.read().await?;
        if super::row(&d, "jobs", id)?["status"] != "running" { return Err(super::conflict("Media job cancelled")); }
        if has_required_media(&d, &post, &super::now())? {
            app.change(|d| mark_attempt(d, id, &post, None, &super::now())).await?;
            return Ok(json!({"reused":true}));
        }
        let binding=super::active_binding(&d)?;
        let bridge_account=super::bridge_account(&binding)?;
        let result = async {
            let projection=app.bridge("media_source", json!({"account":bridge_account,"postId":post["id"],"post":post})).await?;
            let source=super::media_processing::MediaSource::from_projection(&projection, account_scope(&d)?, text(&post,"postKey"))
                .map_err(|code|super::bad(&code))?;
            super::media_processing::process(&source,app).await.map_err(|code|super::bad(&code))
        }.await;
        match result {
            Ok(mut result) => {
                annotate(&mut result, &post);
                let admitted = app.change(|d| {
                    if super::active_binding(d)? != binding { return Err(super::conflict("Media account changed during processing")); }
                    let current=rows(d,"posts").iter().find(|p|p["id"]==post["id"])
                        .ok_or_else(||super::conflict("Media source post disappeared during processing"))?;
                    if current["postKey"]!=post["postKey"] || super::knowledge::media_source_key(current,account_scope(d)?)
                        !=super::knowledge::media_source_key(&post,account_scope(d)?) {
                        return Err(super::conflict("Media source identity changed during processing"));
                    }
                    mark_attempt(d, id, &post, None, &super::now())?;
                    super::merge_materials(d, &result)?;
                    if !has_required_media(d, &post, &super::now())? { return Err(super::bad("Media result has no admitted audio and visual evidence")); }
                    Ok(json!({"processed":true,"sourcePostKey":post["postKey"],"reused":result["reused"] == true}))
                }).await;
                if let Err(error) = &admitted {
                    // A rejected result is not a download failure. Persist the
                    // terminal attempt separately after the admission rollback.
                    app.change(|d| {
                        if super::active_binding(d)? != binding { return Err(super::conflict("Media account changed during processing")); }
                        mark_attempt(d, id, &post, Some(&error.1), &super::now())
                    }).await?;
                }
                return admitted;
            }
            Err(error) => {
                let next = app.change(|d| {
                    if super::active_binding(d)? != binding { return Err(super::conflict("Media account changed during processing")); }
                    mark_attempt(d, id, &post, Some(&error.1), &super::now())?;
                    if downloadable_failure(&error.1) { take_source(d, id, &super::now()) } else { Ok(None) }
                }).await?;
                match next {
                    Some(next) => post = next,
                    None => {
                        if has_required_media(&app.read().await?, &post, &super::now())? {
                            return Ok(json!({"reused":true}));
                        }
                        return Err(error);
                    }
                }
            }
        }
    }
}
pub(crate) async fn tick(app: &super::App) -> super::ApiResult<()> {
    // A missing local runtime cannot consume a durable source attempt. Queued
    // work remains available when the operator supplies the native paths.
    super::media_fullframes::refresh(app).await?;
    let ready=super::media_processing::preflight().is_ok();
    let claimed = app.change_media(|d| claim_when_ready(d, &super::now(), ready)).await?;
    if let Some((id, post)) = claimed {
        let worker = app.clone();
        let run_id = id.clone();
        app.spawn(id, async move { run(&worker, &run_id, post).await });
    }
    Ok(())
}
pub(crate) async fn request(app: &super::App, post_id: &str) -> super::ApiResult<Value> {
    super::media_fullframes::refresh(app).await?;
    super::media_processing::preflight().map_err(|code|super::bad(&code))?;
    let result = app.change(|d| {
        let post = super::row(d, "posts", post_id)?.clone();
        enqueue(d, &post, &super::now(), true)
    }).await?;
    tick(app).await?;
    Ok(result)
}

/// Explicit local-owner recovery only. The receipt is an operator attestation
/// after inspecting process cessation, never an automatic inference from a
/// persisted interrupted status. HTTP middleware also enforces owner + CSRF.
pub(crate) async fn retry_interrupted(
    axum::extract::State(app):axum::extract::State<super::App>,
    axum::Extension(actor):axum::Extension<super::operator_auth::Actor>,
    axum::Json(body):axum::Json<Value>,
)->super::ApiResult<axum::Json<Value>>{
    if actor.role!="owner"{return Err(super::bad("Interrupted media retry requires owner"));}
    let guard=MEDIA_GATE.try_lock().map_err(|_|super::conflict("A media process is still owned by this server"))?;
    let receipt=app.change(|d|{
        if body.get("leaseEpoch").is_some(){
            let object=body.as_object().ok_or_else(||super::bad("Invalid media resume request"))?;
            if object.keys().any(|k|!["jobId","leaseEpoch"].contains(&k.as_str())){return Err(super::bad("Unknown media resume field"));}
            let job_id=super::required(&body,"jobId")?;let epoch=body["leaseEpoch"].as_u64().ok_or_else(||super::bad("Invalid media lease epoch"))?;
            let job=super::row(d,"jobs",job_id)?;let progress=&job["result"]["visualProgress"];
            let post=super::row(d,"posts",text(progress,"sourcePostId"))?;
            if super::active_binding(d)?.to_json()!=progress["connectorBinding"]||super::media_fullframes::source_version(post,account_scope(d)?)!=progress["sourceVersion"]||material_epoch(d,post)!=progress["materialEpoch"]{return Err(super::conflict("Media resume source changed"));}
            super::media_fullframes::resume(super::row_mut(d,"jobs",job_id)?,epoch).map_err(|e|super::conflict(&e))?;
            super::audit(d,"media.visual_resume_authorized",job_id);Ok(json!({"jobId":job_id,"leaseEpoch":epoch,"status":"queued"}))
        }else{authorize_interrupted_retry(d,&body,&actor.id,&super::now())}
    }).await?;
    drop(guard);
    tick(&app).await?;
    Ok(axum::Json(receipt))
}
/// One explicit owner permit for a reviewed download-format repair, not an
/// automatic retry policy. The background worker retains runtime admission.
pub(crate) async fn retry_failed_download(
    axum::extract::State(app):axum::extract::State<super::App>,
    axum::Extension(actor):axum::Extension<super::operator_auth::Actor>,
    axum::Json(body):axum::Json<Value>,
)->super::ApiResult<axum::Json<Value>>{
    if actor.role!="owner"{return Err(super::bad("Download retry requires owner"));}
    let _guard=MEDIA_GATE.try_lock().map_err(|_|super::conflict("A media process is still owned by this server"))?;
    app.change(|d|authorize_download_retry(d,&body,&actor.id,&super::now()).map(axum::Json)).await
}
fn authorize_download_retry(d:&mut Value,body:&Value,actor:&str,at:&str)->super::ApiResult<Value>{
    let fields=body.as_object().ok_or_else(||super::bad("Invalid download retry request"))?;
    if fields.keys().any(|k|!["jobId","postId","attemptIndex","sourceVersion","connectorBinding","expectedError","verification"].contains(&k.as_str())){return Err(super::bad("Unknown download retry field"));}
    let verification=&body["verification"];
    let proof=verification.as_object().ok_or_else(||super::bad("Download fix review is required"))?;
    if proof.keys().any(|k|!["receiptId","checkedAt","method","noMediaProcesses","fixArtifactSha256"].contains(&k.as_str()))
        || verification["method"]!="operator_download_fix_review"||verification["noMediaProcesses"]!=true
        || text(verification,"fixArtifactSha256").len()!=64||!text(verification,"fixArtifactSha256").bytes().all(|c|c.is_ascii_hexdigit()){
        return Err(super::bad("Explicit process cessation and reviewed fix artifact are required"));
    }
    let receipt_id=verification["receiptId"].as_str().filter(|s|!s.is_empty()&&s.len()<=100&&s.bytes().all(|c|c.is_ascii_alphanumeric()||b"_-".contains(&c)))
        .ok_or_else(||super::bad("Invalid download retry receipt ID"))?;
    let index=body["attemptIndex"].as_u64().and_then(|n|usize::try_from(n).ok()).ok_or_else(||super::bad("Invalid source attempt index"))?;
    let post=super::row(d,"posts",text(body,"postId"))?.clone();
    let job=super::row(d,"jobs",text(body,"jobId"))?.clone();
    let attempt=rows(&job,"sourceAttempts").get(index).ok_or_else(||super::bad("Source attempt missing"))?;
    let scope=account_scope(d)?;let binding=super::active_binding(d)?.to_json();
    let version=super::media_fullframes::source_version(&post,scope);
    let source_key=super::knowledge::media_source_key(&post,scope).ok_or_else(||super::conflict("Media source identity unavailable"))?;
    if !video(&post)||!current_job(&job)||job["kind"]!="media"||job["account"]!=scope
        || attempt["postId"]!=post["id"]||attempt["postKey"]!=post["postKey"]||attempt["sourceKey"]!=source_key
        || attempt["sourceVersion"]!=version||body["sourceVersion"]!=version||body["connectorBinding"]!=binding
        || group(d,&post).is_none_or(|key|job["groupKey"]!=key){return Err(super::conflict("Download retry source or binding changed"));}
    if attempt["retryPermit"]["id"]==receipt_id{
        if attempt["retryPermit"]["request"]!=*body||attempt["retryPermit"]["authorizedBy"]!=actor{return Err(super::conflict("Retry receipt is already bound"));}
        return Ok(attempt["retryPermit"].clone());
    }
    let progress=&job["result"]["visualProgress"];
    let empty_legacy_shell=*progress==json!({"phase":"held","leaseId":null,"resumePhase":null});
    let previous_retry_consumed=job["downloadRetry"].is_null()||rows(&job,"sourceAttempts").iter().any(|a|
        a["retryOf"]["permitId"]==job["downloadRetry"]["id"]&&!text(&job["downloadRetry"],"id").is_empty());
    if job["status"]!="failed"||attempt["status"]!="failed"||rows(&job,"sourceAttempts").iter().any(|a|a["status"]!="failed")
        || !attempt["retryPermit"].is_null()||!previous_retry_consumed||!(progress.is_null()||empty_legacy_shell)
        || attempt["error"]!=body["expectedError"]
        || !matches!(text(attempt,"error"),"source_download_failed"|"source_download_failed_format_unavailable"){
        return Err(super::conflict("Only a reviewed terminal download failure without a checkpoint can be retried"));
    }
    if rows(d,"jobs").iter().any(|j|j["kind"]=="media"&&(matches!(text(j,"status"),"running"|"unknown"|"dispatching")
        ||j["status"]=="queued"&&(j["groupKey"]==job["groupKey"]||j["refId"]==post["id"]))) {
        return Err(super::conflict("Another media job is active or unresolved"));
    }
    let history=source_attempts(d,&post);
    if history.len()!=1||history[0].0["id"]!=job["id"]||history[0].1!=index||!attempted(d,&post){
        return Err(super::conflict("Source retry budget exhausted or attempt ownership changed"));
    }
    let parse=|value:&str|chrono::DateTime::parse_from_rfc3339(value).map(|v|v.timestamp()).map_err(|_|super::bad("Invalid retry verification timestamp"));
    let now=parse(at)?;let checked=parse(text(verification,"checkedAt"))?;let finished=parse(text(attempt,"finishedAt"))?;
    if checked<finished||checked>now||now-checked>300{return Err(super::conflict("Download fix verification must be fresh and after failure"));}
    if !open_post(d,&post)||covered(d,&job,at)?{return Err(super::conflict("Source is no longer open or complete evidence is available"));}
    let permit=json!({"id":receipt_id,"kind":"download_failure","jobId":job["id"],"postId":post["id"],"postKey":post["postKey"],"sourceKey":source_key,
        "sourceVersion":version,"connectorBinding":binding,"attemptIndex":index,"authorizedBy":actor,"createdAt":at,
        "verifiedNoMediaProcesses":true,"maxTotalSourceAttempts":2,"priorVisualProgress":progress,"request":body});
    let stored=super::row_mut(d,"jobs",text(&job,"id"))?;
    stored["sourceAttempts"][index]["retryPermit"]=permit.clone();stored["downloadRetry"]=permit.clone();
    if empty_legacy_shell{stored["result"]["visualProgress"]=Value::Null;}
    stored["manualRequested"]=json!(true);stored["fallbackAllowed"]=json!(true);stored["status"]=json!("queued");stored["finishedAt"]=Value::Null;
    // The original error remains in the attempt ledger; no history is reset.
    stored.as_object_mut().unwrap().remove("error");d["mediaQueue"]=Value::Null;
    super::audit(d,"media.download_retry_authorized",receipt_id);Ok(permit)
}
fn authorize_interrupted_retry(d:&mut Value,body:&Value,actor:&str,at:&str)->super::ApiResult<Value>{
    let fields=body.as_object().ok_or_else(||super::bad("Invalid interruption retry request"))?;
    if fields.keys().any(|key|!["postId","jobId","attemptIndex","verification"].contains(&key.as_str())){return Err(super::bad("Unknown interruption retry field"));}
    let verification=&body["verification"];
    let proof=verification.as_object().ok_or_else(||super::bad("Process cessation verification is required"))?;
    if proof.keys().any(|key|!["receiptId","checkedAt","method","noMediaProcesses"].contains(&key.as_str()))
        || verification["method"]!="operator_process_inspection"||verification["noMediaProcesses"]!=true{
        return Err(super::bad("Explicit operator process inspection is required"));
    }
    let receipt_id=verification["receiptId"].as_str().filter(|s|!s.is_empty()&&s.len()<=100&&s.bytes().all(|c|c.is_ascii_alphanumeric()||b"_-".contains(&c)))
        .ok_or_else(||super::bad("Invalid verification receipt ID"))?;
    let index=body["attemptIndex"].as_u64().and_then(|n|usize::try_from(n).ok()).ok_or_else(||super::bad("Invalid source attempt index"))?;
    let post=super::row(d,"posts",text(body,"postId"))?.clone();
    if !allowed(d)||!video(&post){return Err(super::bad("Post has no eligible video"));}
    let job=super::row(d,"jobs",text(body,"jobId"))?.clone();
    let attempt=rows(&job,"sourceAttempts").get(index).ok_or_else(||super::bad("Source attempt missing"))?;
    let scope=account_scope(d)?;
    if attempt["sourceKey"]!=json!(super::knowledge::media_source_key(&post,scope)){
        return Err(super::conflict("Media source identity changed since the interrupted attempt"));
    }
    // Replay the exact issued receipt without issuing another permit or budget.
    if attempt["retryPermit"]["id"]==receipt_id{
        if attempt["retryPermit"]["postId"]!=post["id"]||attempt["retryPermit"]["verification"]!=*verification{
            return Err(super::conflict("Verification receipt is already bound to another request"));
        }
        return Ok(attempt["retryPermit"].clone());
    }
    if rows(d,"jobs").iter().any(|j|j["kind"]=="media"&&j["status"]=="running"){return Err(super::conflict("Media is still running"));}
    if job["purpose"]!=PURPOSE||job["account"]!=scope||!matches!(text(&job,"status"),"failed"|"interrupted")||attempt["status"]!="interrupted"
        || index+1!=rows(&job,"sourceAttempts").len() || !attempt["retryPermit"].is_null(){
        return Err(super::conflict("Only the last interrupted source of a stopped job can be retried"));
    }
    let history=source_attempts(d,&post);
    if history.len()!=1||history[0].0["id"]!=job["id"]||history[0].1!=index{
        return Err(super::conflict("Source retry budget exhausted or attempt binding changed"));
    }
    let parse=|value:&str|chrono::DateTime::parse_from_rfc3339(value).map(|v|v.timestamp()).map_err(|_|super::bad("Invalid retry verification timestamp"));
    let now=parse(at)?;let checked=parse(text(verification,"checkedAt"))?;let finished=parse(text(attempt,"finishedAt"))?;
    if checked<finished||checked>now||now-checked>300{return Err(super::conflict("Process cessation verification must be fresh and after interruption"));}
    if has_required_media(d,&post,at)?{return Err(super::conflict("An admitted transcript is already available"));}
    let permit=json!({"id":receipt_id,"jobId":job["id"],"postId":post["id"],"postKey":post["postKey"],"sourceKey":super::knowledge::media_source_key(&post,scope),
        "attemptIndex":index,"createdAt":at,"authorizedBy":actor,"verifiedNoMediaProcesses":true,"verification":verification,"maxTotalSourceAttempts":2});
    let stored=super::row_mut(d,"jobs",text(&job,"id"))?;
    stored["sourceAttempts"][index]["retryPermit"]=permit.clone();
    stored["status"]=json!("queued");stored["fallbackAllowed"]=json!(true);stored["manualRequested"]=json!(true);
    stored["finishedAt"]=Value::Null;stored.as_object_mut().unwrap().remove("error");
    super::audit(d,"media.interrupted_retry_authorized",receipt_id);
    Ok(permit)
}

/// Strict automatic-preparation gate. Failed, cancelled or missing media never
/// becomes permission to answer a video comment without audio and visual evidence. Photos
/// and ordinary text posts have no media prerequisite. Call after reconcile.
pub(crate) fn requires_video(d:&Value,item:&Value)->bool {
    rows(d,"posts").iter().any(|p|(p["id"]==item["postId"] || (!text(item,"postKey").is_empty()&&p["postKey"]==item["postKey"]))&&video(p))
}
pub(crate) fn preparation_state(d: &Value, item: &Value, at: &str) -> super::ApiResult<Option<&'static str>> {
    if !allowed(d) || !requires_video(d,item) { return Ok(None); }
    let index=QueueIndex::new(d);
    let transcripts=super::knowledge::TranscriptLookup::new(d,at).map_err(|e|super::bad(&e))?;
    preparation_state_indexed(d,item,&index,&transcripts)
}
/// Evaluate a queue with one immutable index and one integrity validation. The
/// map is valid only for this exact snapshot; rebuild after evidence changes.
pub(crate) fn preparation_states(d:&Value,items:&[Value],at:&str)->super::ApiResult<BTreeMap<String,Option<&'static str>>>{
    if !allowed(d){return Ok(items.iter().map(|i|(text(i,"id").to_owned(),None)).collect());}
    let index=QueueIndex::new(d);
    let transcripts=super::knowledge::TranscriptLookup::new(d,at).map_err(|e|super::bad(&e))?;
    items.iter().map(|item|Ok((text(item,"id").to_owned(),preparation_state_indexed(d,item,&index,&transcripts)?))).collect()
}
fn preparation_state_indexed(d:&Value,item:&Value,index:&QueueIndex,transcripts:&super::knowledge::TranscriptLookup)->super::ApiResult<Option<&'static str>>{
    let binding = super::active_binding(d)?;
    if super::bound_item(&binding, item).is_err() { return Ok(None); }
    let Some(post) = rows(d,"posts").iter().find(|p| p["id"] == item["postId"]
        || (!text(item,"postKey").is_empty() && p["postKey"] == item["postKey"])) else { return Ok(None); };
    if !video(post) || transcripts.ready(post).map_err(|e|super::bad(&e))? { return Ok(None); }
    let Some(key) = index.groups.get(text(post,"id")) else { return Ok(Some("media_unavailable")); };
    let job = rows(d,"jobs").iter().rev().find(|j| current_job(j) && j["groupKey"] == *key && current_source_job(d,j));
    Ok(Some(match job.map(|j| text(j, "status")) {
        None | Some("queued" | "running") => "media_wait",
        _ => "media_unavailable",
    }))
}

/// Queue timing for UI/diagnostics only; this is not permission to bypass the
/// strict prerequisite after a timeout or terminal media failure.
pub(crate) fn pending_for_item(d: &Value, item: &Value) -> Option<i64> {
    if !allowed(d) { return None; }
    let binding = super::active_binding(d).ok()?;
    super::bound_item(&binding, item).ok()?;
    let post = rows(d,"posts").iter().find(|p| p["id"] == item["postId"]
        || (!text(item,"postKey").is_empty() && p["postKey"] == item["postKey"]))?;
    let key = group(d,post)?;
    let job = rows(d,"jobs").iter().rev().find(|j| current_job(j) && j["groupKey"] == key && current_source_job(d,j)
        && matches!(text(j,"status"),"queued"|"running"))?;
    let timestamp = job["startedAt"].as_str().or_else(||job["createdAt"].as_str())?;
    chrono::DateTime::parse_from_rfc3339(timestamp).ok().map(|v|v.timestamp())
}

#[cfg(test)]
mod tests {
    use super::*;
    const AT: &str = "2026-09-22T08:00:00Z";
    fn fixture() -> Value {
        fixture_for(super::super::accounts::Profile::LikeAvto)
    }
    fn fixture_for(profile: super::super::accounts::Profile) -> Value {
        let mut d = super::super::empty();
        super::super::accounts::initialize(&mut d,profile).unwrap();
        for (id, channel, object) in [("post-11391:one", "VK", "11391"), ("post-11390:two", "YouTube", "11390")] {
            super::super::list_mut(&mut d, "posts").push(json!({"id":id,"postKey":&id[5..],"objectId":object,"title":"Обзор семейного автомобиля","channel":channel,"attachments":[{"type":"video"}]}));
            super::super::list_mut(&mut d, "items").push(json!({"id":format!("item-{object}-one"),"itemId":"one","objectId":object,"postId":id,"postKey":&id[5..],"conversationKey":format!("{object}:thread"),"providerStatus":"new","workflow":"attention"}));
        }
        add_visual_fixture(&mut d);
        super::super::knowledge::sync_catalog(&mut d, AT).unwrap();
        d
    }
    // Old queue cases now supply both required components; transcript-only
    // rejection has its own regression case below.
    fn add_visual_fixture(d:&mut Value){
        let account=d["account"].as_str().unwrap().to_owned();
        let media:Vec<_>=rows(d,"materials").iter().filter(|m|m["kind"]=="transcript").cloned().collect();
        for m in media {
            let key=text(&m,"postKey");
            let evidence=rows(d,"posts").iter().find(|p|p["postKey"]==key).map(|p|super::super::media_fullframes::fixture_for_post(&account,p)).unwrap_or_else(||super::super::media_fullframes::fixture(&account,key));
            super::super::list_mut(d,"materials").push(json!({"id":format!("visual-{}",text(&m,"id")),"kind":"visual_context","account":account,
                "postKey":key,"sourceUrl":m["sourceUrl"],"mediaSha256":evidence["source"]["mediaSha256"],"text":"Full visual context","visualEvidence":evidence}));
        }
    }
    #[test]
    fn transcript_only_never_releases_video_and_legacy_attempts_do_not_consume_visual_work(){
        let mut d=fixture();let post=d["posts"][0].clone();let item=d["items"][0].clone();
        d["materials"]=json!([{"id":"audio","kind":"transcript","postKey":post["postKey"],"text":"Audio words"}]);
        super::super::knowledge::sync_catalog(&mut d,AT).unwrap();
        assert!(preparation_state(&d,&item,AT).unwrap().is_some());
        d["jobs"]=json!([{"id":"old","kind":"media","purpose":"auto_media","status":"completed","refId":post["id"],"sourceAttempts":[{"postId":post["id"],"postKey":post["postKey"],"status":"completed"}]}]);
        let historical=d["jobs"][0].clone();
        let new=enqueue(&mut d,&post,AT,false).unwrap();assert_eq!(new["status"],"queued");assert_eq!(d["jobs"][0],historical);
        add_visual_fixture(&mut d);super::super::knowledge::sync_catalog(&mut d,AT).unwrap();
        assert_eq!(preparation_state(&d,&item,AT).unwrap(),None);
    }
    #[test]
    fn missing_native_runtime_preserves_queued_source_for_later_claim() {
        let mut d=fixture();
        let post=d["posts"][0].clone();
        enqueue(&mut d,&post,AT,true).unwrap();
        let before=d["jobs"][0].clone();
        assert!(claim_when_ready(&mut d,AT,false).unwrap().is_none());
        assert_eq!(d["jobs"][0],before);
        assert!(claim_when_ready(&mut d,AT,true).unwrap().is_some());
        assert_eq!(d["jobs"][0]["sourceAttempts"].as_array().unwrap().len(),1);
    }
    fn stopped_source()->(Value,String,Value){
        let mut d=fixture();d["posts"].as_array_mut().unwrap().truncate(1);
        let (id,post)=claim(&mut d,AT).unwrap().unwrap();
        d["jobs"][0]["result"]=json!({}); // source-only legacy recovery contract
        super::super::recover(&mut d);recover(&mut d,AT).unwrap();
        assert_eq!(d["jobs"][0]["status"],"failed");
        (d,id,post)
    }
    fn retry_body(id:&str,post:&Value)->Value{
        json!({"postId":post["id"],"jobId":id,"attemptIndex":0,"verification":{"receiptId":"checked-media-stopped","checkedAt":AT,"method":"operator_process_inspection","noMediaProcesses":true}})
    }
    #[test]
    fn interrupted_retry_needs_explicit_stop_receipt_and_keeps_two_attempt_limit(){
        let (mut d,id,post)=stopped_source();
        assert!(claim(&mut d,AT).unwrap().is_none());
        let body=retry_body(&id,&post);
        let permit=authorize_interrupted_retry(&mut d,&body,"local-owner",AT).unwrap();
        assert_eq!(permit["maxTotalSourceAttempts"],2);
        let (retry_id,retry_post)=claim(&mut d,AT).unwrap().unwrap();
        assert_eq!(retry_id,id);assert_eq!(retry_post["id"],post["id"]);
        assert!(claim(&mut d,AT).unwrap().is_none());
        assert_eq!(d["jobs"][0]["sourceAttempts"][0]["status"],"interrupted");
        assert_eq!(d["jobs"][0]["sourceAttempts"][1]["attemptNumber"],2);
        assert_eq!(d["jobs"][0]["sourceAttempts"][1]["retryOf"]["permitId"],permit["id"]);
        assert_eq!(authorize_interrupted_retry(&mut d,&body,"local-owner",AT).unwrap(),permit);
        d["jobs"][0]["result"]=json!({});
        super::super::recover(&mut d);recover(&mut d,AT).unwrap();
        assert!(claim(&mut d,AT).unwrap().is_none());
        let mut new_body=body;new_body["attemptIndex"]=json!(1);new_body["verification"]["receiptId"]=json!("second-request");
        assert!(authorize_interrupted_retry(&mut d,&new_body,"local-owner",AT).is_err());
    }
    #[test]
    fn retry_rejects_cancelled_failed_running_unverified_and_stale_receipts(){
        for condition in ["cancelled","failed-attempt","running","unverified","stale","before-interruption","wrong-post","source-changed"]{
            let (mut d,id,post)=stopped_source();let mut body=retry_body(&id,&post);
            match condition{
                "cancelled"=>d["jobs"][0]["status"]=json!("cancelled"),
                "failed-attempt"=>d["jobs"][0]["sourceAttempts"][0]["status"]=json!("failed"),
                "running"=>{super::super::list_mut(&mut d,"jobs").push(json!({"id":"other","kind":"media","status":"running"}));},
                "unverified"=>body["verification"]["noMediaProcesses"]=json!(false),
                "stale"=>body["verification"]["checkedAt"]=json!("2026-09-22T07:50:00Z"),
                "before-interruption"=>d["jobs"][0]["sourceAttempts"][0]["finishedAt"]=json!("2026-09-22T08:00:01Z"),
                "source-changed"=>d["posts"][0]["sourceUrl"]=json!("https://youtu.be/AbCdEf123_-"),
                _=>body["postId"]=json!("missing-post"),
            }
            assert!(authorize_interrupted_retry(&mut d,&body,"local-owner",AT).is_err(),"{condition}");
            assert!(d["jobs"][0]["sourceAttempts"][0]["retryPermit"].is_null());
        }
    }
    #[test]
    fn recovered_shared_transcript_clears_only_current_error_keeps_attempt_history(){
        let (mut d,id,post)=stopped_source();
        d["materials"]=json!([{"id":"recovered-cache","kind":"transcript","postKey":post["postKey"],"text":"Recovered transcript"}]);
        add_visual_fixture(&mut d);
        super::super::knowledge::sync_catalog(&mut d,AT).unwrap();
        reconcile(&mut d,AT).unwrap();
        assert_eq!(super::super::row(&d,"jobs",&id).unwrap()["status"],"completed");
        assert!(super::super::row(&d,"jobs",&id).unwrap()["error"].is_null());
        assert_eq!(d["jobs"][0]["sourceAttempts"][0]["status"],"interrupted");
        d["jobs"][0]["error"]=json!("Old misleading error");reconcile(&mut d,AT).unwrap();
        assert!(d["jobs"][0]["error"].is_null());
        assert!(authorize_interrupted_retry(&mut d,&retry_body(&id,&post),"local-owner",AT).is_err());
    }
    #[test]
    fn same_title_deduplicates_and_one_running_blocks_manual_and_auto() {
        let mut d = fixture();
        reconcile(&mut d, AT).unwrap();
        assert_eq!(super::super::list(&d, "jobs").len(), 1);
        let (id, _) = claim(&mut d, AT).unwrap().unwrap();
        assert!(claim(&mut d, AT).unwrap().is_none());
        let post = d["posts"][1].clone();
        assert_eq!(enqueue(&mut d, &post, AT, true).unwrap()["jobId"], id);
        assert_eq!(d["jobs"][0]["sourceAttempts"].as_array().unwrap().len(), 1);
    }
    #[test]
    fn queue_and_same_title_reuse_are_scoped_to_each_configured_account() {
        for profile in [super::super::accounts::Profile::LikeAvto,super::super::accounts::Profile::BawRussia] {
            let mut d=fixture_for(profile);
            reconcile(&mut d,AT).unwrap();
            assert_eq!(rows(&d,"jobs").len(),1);
            assert_eq!(d["jobs"][0]["account"],profile.display());
            d["materials"]=json!([{"id":"shared","account":profile.display(),"kind":"transcript","postKey":"11391:one","text":"Account-local transcript"}]);
            add_visual_fixture(&mut d);
        super::super::knowledge::sync_catalog(&mut d,AT).unwrap();
            reconcile(&mut d,AT).unwrap();
            assert!(rows(&d,"jobs").iter().all(|job|job["status"]=="completed"));
            assert!(rows(&d,"posts").iter().all(|post|has_required_media(&d,post,AT).unwrap()));
        }
    }
    #[test]
    fn failed_source_moves_to_twin_once_and_never_loops() {
        let mut d = fixture();
        let (id, first) = claim(&mut d, AT).unwrap().unwrap();
        mark_attempt(&mut d, &id, &first, Some("SOURCE_DOWNLOAD_FAILED"), AT).unwrap();
        let second = take_source(&mut d, &id, AT).unwrap().unwrap();
        assert_ne!(first["id"], second["id"]);
        mark_attempt(&mut d, &id, &second, Some("SOURCE_DOWNLOAD_FAILED"), AT).unwrap();
        assert!(take_source(&mut d, &id, AT).unwrap().is_none());
        d["jobs"][0]["status"] = json!("failed");
        for _ in 0..4 { assert!(claim(&mut d, AT).unwrap().is_none()); }
        assert_eq!(super::super::list(&d, "jobs").len(), 1);
    }
    fn failed_download()->(Value,String,Value,Value){
        let mut d=fixture();d["posts"].as_array_mut().unwrap().truncate(1);
        let (id,post)=claim(&mut d,AT).unwrap().unwrap();
        mark_attempt(&mut d,&id,&post,Some("source_download_failed"),AT).unwrap();
        let job=super::super::row_mut(&mut d,"jobs",&id).unwrap();
        assert!(super::super::media_fullframes::finish(job,&Err(super::super::bad("source_download_failed")),AT));
        let body=json!({"jobId":id,"postId":post["id"],"attemptIndex":0,"sourceVersion":d["jobs"][0]["sourceAttempts"][0]["sourceVersion"],
            "connectorBinding":super::super::active_binding(&d).unwrap().to_json(),"expectedError":"source_download_failed",
            "verification":{"receiptId":"reviewed-download-format-fix","checkedAt":AT,"method":"operator_download_fix_review","noMediaProcesses":true,"fixArtifactSha256":"a".repeat(64)}});
        (d,id,post,body)
    }
    #[test]
    fn failed_download_retry_preserves_ledger_and_consumes_one_explicit_permit(){
        let (mut d,id,post,body)=failed_download();let original=d["jobs"][0]["sourceAttempts"][0].clone();
        assert!(claim(&mut d,AT).unwrap().is_none());
        let permit=authorize_download_retry(&mut d,&body,"local-owner",AT).unwrap();
        assert_eq!(authorize_download_retry(&mut d,&body,"local-owner",AT).unwrap(),permit);
        let (claimed,selected)=claim(&mut d,AT).unwrap().unwrap();assert_eq!(claimed,id);assert_eq!(selected,post);
        assert!(claim(&mut d,AT).unwrap().is_none());
        let mut first=d["jobs"][0]["sourceAttempts"][0].clone();first.as_object_mut().unwrap().remove("retryPermit");assert_eq!(first,original);
        assert_eq!(d["jobs"][0]["sourceAttempts"][1]["attemptNumber"],2);
        assert_eq!(d["jobs"][0]["sourceAttempts"][1]["retryOf"]["permitId"],permit["id"]);
        mark_attempt(&mut d,&id,&post,Some("source_download_failed"),AT).unwrap();
        super::super::media_fullframes::finish(super::super::row_mut(&mut d,"jobs",&id).unwrap(),&Err(super::super::bad("source_download_failed")),AT);
        assert!(claim(&mut d,AT).unwrap().is_none());
        let mut again=body;again["attemptIndex"]=json!(1);again["verification"]["receiptId"]=json!("another-fix");
        assert!(authorize_download_retry(&mut d,&again,"local-owner",AT).is_err());
    }
    #[test]
    fn failed_download_retry_admits_only_exact_historical_empty_cursor_shell(){
        let (mut d,_,_,body)=failed_download();let shell=json!({"phase":"held","leaseId":null,"resumePhase":null});
        d["jobs"][0]["result"]["visualProgress"]=shell.clone();
        let permit=authorize_download_retry(&mut d,&body,"local-owner",AT).unwrap();assert_eq!(permit["priorVisualProgress"],shell);
        assert!(d["jobs"][0]["result"]["visualProgress"].is_null());assert!(claim(&mut d,AT).unwrap().is_some());
        for checkpoint in [json!({"phase":"held","leaseId":null,"resumePhase":null,"schemaVersion":2}),json!({"phase":"held","leaseId":null,"resumePhase":"download"}),json!({})]{
            let (mut d,_,_,body)=failed_download();d["jobs"][0]["result"]["visualProgress"]=checkpoint;let before=d.clone();
            assert!(authorize_download_retry(&mut d,&body,"local-owner",AT).is_err());assert_eq!(d,before);
        }
    }
    #[test]
    fn failed_download_retry_guards_fail_without_mutation(){
        for condition in ["running","unknown","same-queued","completed","wrong-account","wrong-binding","wrong-version","changed-post","wrong-error","auth-error","rate-error","network-error","js-error","timeout-error","unverified","stale","receipt-mismatch","closed","prior-success"]{
            let (mut d,_,_,mut body)=failed_download();
            match condition{
                "running"|"unknown"=>{super::super::list_mut(&mut d,"jobs").push(json!({"id":"other","kind":"media","status":condition}));},
                "same-queued"=>{let key=d["jobs"][0]["groupKey"].clone();super::super::list_mut(&mut d,"jobs").push(json!({"id":"other","kind":"media","status":"queued","groupKey":key}));},
                "completed"=>d["jobs"][0]["status"]=json!("completed"),
                "wrong-account"=>d["jobs"][0]["account"]=json!("other"),
                "wrong-binding"=>body["connectorBinding"]=json!({}),
                "wrong-version"=>body["sourceVersion"]=json!("old"),
                "changed-post"=>d["posts"][0]["title"]=json!("changed"),
                "wrong-error"=>body["expectedError"]=json!("source_download_failed_format_unavailable"),
                "auth-error"|"rate-error"|"network-error"|"js-error"|"timeout-error"=>{let error=match condition{"auth-error"=>"source_download_failed_auth","rate-error"=>"source_download_failed_rate_limited","network-error"=>"source_download_failed_network","js-error"=>"source_download_failed_js_runtime",_=>"source_download_failed_timeout"};d["jobs"][0]["sourceAttempts"][0]["error"]=json!(error);body["expectedError"]=json!(error);},
                "unverified"=>body["verification"]["noMediaProcesses"]=json!(false),
                "stale"=>body["verification"]["checkedAt"]=json!("2026-09-22T07:00:00Z"),
                "receipt-mismatch"=>{authorize_download_retry(&mut d,&body,"local-owner",AT).unwrap();body["verification"]["fixArtifactSha256"]=json!("b".repeat(64));},
                "closed"=>d["items"][0]["workflow"]=json!("closed"),
                _=>{let mut prior=d["jobs"][0].clone();prior["id"]=json!("prior");prior["status"]=json!("completed");prior["sourceAttempts"][0]["status"]=json!("completed");super::super::list_mut(&mut d,"jobs").push(prior);},
            }
            let before=d.clone();assert!(authorize_download_retry(&mut d,&body,"local-owner",AT).is_err(),"{condition}");assert_eq!(d,before,"{condition}");
        }
    }
    #[test]
    fn failed_download_retry_stays_pinned_and_rechecks_current_source_before_claim(){
        for change in ["source","binding"]{
            let (mut d,id,_,body)=failed_download();authorize_download_retry(&mut d,&body,"local-owner",AT).unwrap();
            if change=="source"{d["posts"][0]["title"]=json!("new source version");}else{d["jobs"][0]["downloadRetry"]["connectorBinding"]=json!({});}
            let claimed=claim(&mut d,AT).unwrap();
            // A changed post may independently enqueue its new source revision;
            // it must never consume the old job's exact-version retry permit.
            assert!(claimed.is_none_or(|(claimed_id,_)|claimed_id!=id));assert_eq!(rows(&d["jobs"][0],"sourceAttempts").len(),1);
        }
        let (mut d,_,_,mut body)=failed_download();d["jobs"][0]["sourceAttempts"][0]["error"]=json!("source_download_failed_format_unavailable");body["expectedError"]=json!("source_download_failed_format_unavailable");
        super::super::list_mut(&mut d,"jobs").push(json!({"id":"unrelated","kind":"media","status":"queued","groupKey":"other"}));
        assert!(authorize_download_retry(&mut d,&body,"local-owner",AT).is_ok());
    }
    #[test]
    fn failed_download_retry_can_select_earlier_failed_twin_with_per_source_budget(){
        let mut d=fixture();let (id,first)=claim(&mut d,AT).unwrap().unwrap();
        mark_attempt(&mut d,&id,&first,Some("source_download_failed"),AT).unwrap();
        let second=take_source(&mut d,&id,AT).unwrap().unwrap();
        mark_attempt(&mut d,&id,&second,Some("source_download_failed"),AT).unwrap();
        super::super::media_fullframes::finish(super::super::row_mut(&mut d,"jobs",&id).unwrap(),&Err(super::super::bad("source_download_failed")),AT);
        let original=rows(&d["jobs"][0],"sourceAttempts").to_vec();
        let mut body=json!({"jobId":id,"postId":first["id"],"attemptIndex":0,"sourceVersion":original[0]["sourceVersion"],
            "connectorBinding":super::super::active_binding(&d).unwrap().to_json(),"expectedError":"source_download_failed",
            "verification":{"receiptId":"earlier-source","checkedAt":AT,"method":"operator_download_fix_review","noMediaProcesses":true,"fixArtifactSha256":"a".repeat(64)}});
        for status in ["running","queued","completed","interrupted"]{
            let mut rejected=d.clone();rejected["jobs"][0]["sourceAttempts"][1]["status"]=json!(status);let before=rejected.clone();
            assert!(authorize_download_retry(&mut rejected,&body,"local-owner",AT).is_err());assert_eq!(rejected,before);
        }
        authorize_download_retry(&mut d,&body,"local-owner",AT).unwrap();
        assert_eq!(claim(&mut d,AT).unwrap().unwrap(),(id.clone(),first.clone()));
        assert_eq!(d["jobs"][0]["sourceAttempts"][1],original[1]);
        assert_eq!(d["jobs"][0]["sourceAttempts"][2]["retryOf"]["attemptIndex"],0);
        mark_attempt(&mut d,&id,&first,Some("source_download_failed"),AT).unwrap();
        super::super::media_fullframes::finish(super::super::row_mut(&mut d,"jobs",&id).unwrap(),&Err(super::super::bad("source_download_failed")),AT);
        assert!(claim(&mut d,AT).unwrap().is_none());
        // A different source needs a separate explicit review; the first receipt
        // never automatically retries the other twin, nor exhausts its budget.
        body["postId"]=second["id"].clone();body["attemptIndex"]=json!(1);body["sourceVersion"]=original[1]["sourceVersion"].clone();body["verification"]["receiptId"]=json!("later-source");
        authorize_download_retry(&mut d,&body,"local-owner",AT).unwrap();
        assert_eq!(claim(&mut d,AT).unwrap().unwrap(),(id,second));
        assert_eq!(d["jobs"][0]["sourceAttempts"][3]["attemptNumber"],2);
        assert_eq!(d["jobs"][0]["sourceAttempts"][0]["retryPermit"]["id"],"earlier-source");
    }
    #[test]
    fn download_failure_finalization_retries_only_untried_twin_without_malformed_hold(){
        let mut d=fixture();let (id,first)=claim(&mut d,AT).unwrap().unwrap();
        mark_attempt(&mut d,&id,&first,Some("source_download_failed"),AT).unwrap();
        let job=super::super::row_mut(&mut d,"jobs",&id).unwrap();
        assert!(super::super::media_fullframes::finish(job,&Err(super::super::bad("source_download_failed")),AT));
        assert!(job["result"]["visualProgress"].is_null());assert_eq!(job["status"],"failed");
        let(next_id,second)=claim(&mut d,AT).unwrap().unwrap();assert_eq!(next_id,id);assert_ne!(first["id"],second["id"]);
        let job=super::super::row(&d,"jobs",&id).unwrap();assert_eq!(job["result"]["visualProgress"]["schemaVersion"],2);assert_eq!(job["result"]["visualProgress"]["phase"],"download");
        assert_eq!(rows(job,"sourceAttempts").len(),2);assert_eq!(job["sourceAttempts"][0]["status"],"failed");
        mark_attempt(&mut d,&id,&second,Some("source_download_failed"),AT).unwrap();
        let job=super::super::row_mut(&mut d,"jobs",&id).unwrap();
        super::super::media_fullframes::finish(job,&Err(super::super::bad("source_download_failed")),AT);
        for _ in 0..4 {assert!(claim(&mut d,AT).unwrap().is_none());}
        let job=super::super::row(&d,"jobs",&id).unwrap();assert_eq!(job["status"],"failed");assert!(job["result"]["visualProgress"].is_null());assert_eq!(rows(job,"sourceAttempts").len(),2);
        assert_eq!(rows(&d,"jobs").len(),1);
    }
    #[test]
    fn historical_queued_held_shell_recovers_then_claims_only_untried_twin(){
        let mut d=fixture();let (id,first)=claim(&mut d,AT).unwrap().unwrap();
        mark_attempt(&mut d,&id,&first,Some("source_download_failed"),AT).unwrap();
        let job=super::super::row_mut(&mut d,"jobs",&id).unwrap();
        job["result"]["visualProgress"]=json!({"leaseId":null,"resumePhase":null,"phase":"held"});job["status"]=json!("queued");
        let failed_attempt=job["sourceAttempts"][0].clone();
        super::super::recover(&mut d);recover(&mut d,AT).unwrap();
        let (next_id,second)=claim(&mut d,AT).unwrap().unwrap();assert_eq!(next_id,id);assert_ne!(first["id"],second["id"]);
        let job=super::super::row(&d,"jobs",&id).unwrap();assert_eq!(job["sourceAttempts"][0],failed_attempt);assert_eq!(rows(job,"sourceAttempts").len(),2);
        let progress=&job["result"]["visualProgress"];assert_eq!(progress["schemaVersion"],2);assert_eq!(progress["phase"],"download");assert_eq!(progress["sourcePostId"],second["id"]);assert!(!text(progress,"leaseId").is_empty());assert!(progress.get("resumePhase").is_none());
    }
    #[test]
    fn recovery_resumes_same_committed_cursor_without_consuming_another_source() {
        let mut d=fixture();let (id,first)=claim(&mut d,AT).unwrap().unwrap();
        d["jobs"][0]["result"]["visualProgress"]["nextSelectionIndex"]=json!(4);
        let prior_epoch=d["jobs"][0]["result"]["visualProgress"]["leaseEpoch"].as_u64().unwrap();
        super::super::recover(&mut d);recover(&mut d,AT).unwrap();
        let (next_id,second)=claim(&mut d,AT).unwrap().unwrap();assert_eq!(id,next_id);assert_eq!(first["id"],second["id"]);
        assert_eq!(rows(&d["jobs"][0],"sourceAttempts").len(),1);assert_eq!(d["jobs"][0]["result"]["visualProgress"]["nextSelectionIndex"],4);
        assert!(d["jobs"][0]["result"]["visualProgress"]["leaseEpoch"].as_u64().unwrap()>prior_epoch);
    }
    #[test]
    fn stale_resumed_source_is_held_without_starving_next_job(){
        for drift in ["removed","changed","account"] {
            let mut d=fixture();d["posts"][1]["title"]=json!("Independent other video");
            let (first,post)=claim(&mut d,AT).unwrap().unwrap();
            let job=super::super::row_mut(&mut d,"jobs",&first).unwrap();job["result"]["visualProgress"]["nextSelectionIndex"]=json!(4);
            super::super::media_fullframes::finish(job,&Ok(json!({"resume":true})),AT);
            match drift {"removed"=>d["posts"].as_array_mut().unwrap().retain(|p|p["id"]!=post["id"]),"changed"=>{super::super::row_mut(&mut d,"posts",text(&post,"id")).unwrap()["sourceUrl"]=json!("https://example.org/new-video");},_=>{super::super::row_mut(&mut d,"jobs",&first).unwrap()["result"]["visualProgress"]["account"]=json!("Other");}}
            reconcile(&mut d,AT).unwrap();
            for job in super::super::list_mut(&mut d,"jobs"){if job["id"]!=first {job["startedAt"]=json!("2026-09-22T08:00:01Z");}}
            let (next,_) = claim(&mut d,"2026-09-22T08:00:02Z").unwrap().unwrap();assert_ne!(first,next,"{drift}");
            let held=super::super::row(&d,"jobs",&first).unwrap();assert_eq!(held["status"],"failed");assert_eq!(held["result"]["visualProgress"]["nextSelectionIndex"],4);assert_eq!(held["result"]["visualProgress"]["phase"],"held");
        }
    }
    #[test]
    fn chunk_yield_serves_other_video_before_returning_to_same_cursor(){
        let mut d=fixture();d["posts"][1]["title"]=json!("Independent other video");let(first,_)=claim(&mut d,AT).unwrap().unwrap();
        super::super::media_fullframes::finish(super::super::row_mut(&mut d,"jobs",&first).unwrap(),&Ok(json!({"resume":true})),AT);
        let(second,_)=claim(&mut d,"2026-09-22T08:00:01Z").unwrap().unwrap();assert_ne!(first,second);
        super::super::media_fullframes::finish(super::super::row_mut(&mut d,"jobs",&second).unwrap(),&Ok(json!({"resume":true})),AT);
        let(third,_)=claim(&mut d,"2026-09-22T08:00:02Z").unwrap().unwrap();assert_eq!(third,first);assert_eq!(rows(super::super::row(&d,"jobs",&first).unwrap(),"sourceAttempts").len(),1);
    }
    #[test]
    fn historical_failed_sources_are_not_retried() {
        let mut d = fixture();
        let posts = d["posts"].as_array().unwrap().clone();
        for post in posts { super::super::list_mut(&mut d, "jobs").push(json!({"id":format!("old-{}", post["id"]),"kind":"media","refId":post["id"],"status":"failed"})); }
        assert!(claim(&mut d, AT).unwrap().is_none());
        assert_eq!(d["jobs"][2]["status"], "failed");
    }
    #[test]
    fn closed_comments_photos_and_other_accounts_do_not_queue() {
        let mut d = fixture();
        for item in super::super::list_mut(&mut d, "items") { item["providerStatus"] = json!("closed"); }
        assert!(claim(&mut d, AT).unwrap().is_none());
        let mut d = fixture();
        for post in super::super::list_mut(&mut d, "posts") { post["attachments"] = json!([{"type":"image"}]); }
        assert!(claim(&mut d, AT).unwrap().is_none());
        let mut d = fixture(); d["account"] = json!("Other");
        assert!(claim(&mut d, AT).unwrap().is_none());
    }
    #[test]
    fn shared_transcript_avoids_launch_and_conflicting_identity_splits_groups() {
        let mut d = fixture();
        d["materials"] = json!([{"id":"already","kind":"transcript","postKey":"11391:one","text":"Existing transcript"}]);
        add_visual_fixture(&mut d);
        super::super::knowledge::sync_catalog(&mut d, AT).unwrap();
        assert!(claim(&mut d, AT).unwrap().is_none());
        assert!(super::super::list(&d, "jobs").is_empty());
        let mut d = fixture();
        d["posts"][0]["canonicalMediaId"] = json!("first");
        d["posts"][1]["canonicalMediaId"] = json!("second");
        reconcile(&mut d, AT).unwrap();
        assert_eq!(super::super::list(&d, "jobs").len(), 2);
    }
    #[test]
    fn asr_failure_is_terminal_and_partial_metadata_is_honest() {
        let mut d = fixture();
        let (id, post) = claim(&mut d, AT).unwrap().unwrap();
        mark_attempt(&mut d, &id, &post, Some("MEDIA_MODEL_MISSING"), AT).unwrap();
        d["jobs"][0]["status"] = json!("failed");
        assert!(claim(&mut d, AT).unwrap().is_none());
        let mut result = json!({"reused":false,"materials":[{"kind":"transcript"}]});
        annotate(&mut result, &post);
        assert_eq!(result["materials"][0]["transcription"]["partial"], true);
        assert_eq!(result["materials"][0]["transcription"]["maxAudioSeconds"], 900);
        let mut cached = json!({"reused":true,"materials":[{"kind":"transcript"}]});
        annotate(&mut cached, &post);
        assert!(cached["materials"][0]["transcription"].is_null());
    }
    #[test]
    fn closed_queued_group_pauses_and_manual_request_can_resume() {
        let mut d = fixture();
        reconcile(&mut d, AT).unwrap();
        let item = d["items"][0].clone();
        assert!(pending_for_item(&d,&item).is_some());
        for item in super::super::list_mut(&mut d,"items") { item["workflow"] = json!("closed"); }
        assert!(claim(&mut d, AT).unwrap().is_none());
        assert_eq!(d["jobs"][0]["status"],"paused");
        assert!(pending_for_item(&d,&item).is_none());
        let post = d["posts"][0].clone();
        enqueue(&mut d,&post,AT,true).unwrap();
        assert!(claim(&mut d, AT).unwrap().is_some());
    }
    #[test]
    fn cancelled_group_is_not_reopened_and_late_new_twin_is_allowed_after_failure() {
        let mut d = fixture();
        reconcile(&mut d, AT).unwrap();
        d["jobs"][0]["status"] = json!("cancelled");
        assert!(claim(&mut d, AT).unwrap().is_none());
        let post = d["posts"][0].clone();
        assert_eq!(enqueue(&mut d,&post,AT,true).unwrap()["status"],"cancelled");
        let mut d = fixture();
        let late = d["posts"].as_array_mut().unwrap().pop().unwrap();
        let (id, first) = claim(&mut d, AT).unwrap().unwrap();
        mark_attempt(&mut d,&id,&first,Some("SOURCE_DOWNLOAD_FAILED"),AT).unwrap();
        d["jobs"][0]["status"] = json!("failed");
        assert!(claim(&mut d, AT).unwrap().is_none());
        super::super::list_mut(&mut d,"posts").push(late.clone());
        let (_, next) = claim(&mut d, AT).unwrap().unwrap();
        assert_eq!(next["id"],late["id"]);
    }
    #[test]
    fn source_url_alias_is_not_an_independent_fallback_and_adapter_metadata_survives() {
        let mut d = fixture();
        d["posts"][0]["attachments"][0]["url"] = json!("https://youtu.be/AbCdEf123_-");
        d["posts"][1]["attachments"][0]["url"] = json!("https://www.youtube.com/watch?v=AbCdEf123_-");
        let (id, first) = claim(&mut d, AT).unwrap().unwrap();
        mark_attempt(&mut d,&id,&first,Some("SOURCE_DOWNLOAD_FAILED"),AT).unwrap();
        assert!(take_source(&mut d,&id,AT).unwrap().is_none());
        let actual = json!({"model":"whisper-small","partial":true,"maxAudioSeconds":900});
        let mut result = json!({"reused":false,"materials":[{"kind":"transcript","transcription":actual}]});
        annotate(&mut result,&first);
        assert_eq!(result["materials"][0]["transcription"],actual);
    }

    #[test]
    fn strict_video_gate_survives_time_failure_cancellation_and_accepts_shared_result() {
        let mut d = fixture();
        let item = d["items"][0].clone();
        assert_eq!(preparation_state(&d, &item, AT).unwrap(), Some("media_wait"));
        reconcile(&mut d, AT).unwrap();
        assert_eq!(preparation_state(&d, &item, "2026-09-23T08:00:00Z").unwrap(), Some("media_wait"));
        for status in ["failed", "cancelled", "interrupted", "paused", "completed"] {
            d["jobs"][0]["status"] = json!(status);
            assert_eq!(preparation_state(&d, &item, AT).unwrap(), Some("media_unavailable"));
        }
        d["materials"] = json!([{"id":"twin-result","kind":"transcript","postKey":"11390:two","text":"Transcript from the other platform"}]);
        add_visual_fixture(&mut d);
        super::super::knowledge::sync_catalog(&mut d, AT).unwrap();
        assert_eq!(preparation_state(&d, &item, AT).unwrap(), None);
    }

    #[test]
    fn media_gate_excludes_photos_text_and_foreign_accounts_but_includes_reels() {
        let mut d = fixture();
        let item = d["items"][0].clone();
        for attachment in [json!({"type":"reel"}), json!({"type":"clip"}), json!({"mimeType":"video/mp4"})] {
            d["posts"][0]["attachments"] = json!([attachment]);
            assert!(group(&d, &d["posts"][0]).is_some());
            assert_eq!(preparation_state(&d, &item, AT).unwrap(), Some("media_wait"));
        }
        d["posts"][0]["attachments"] = json!([{"type":"image"}]);
        for url in ["", "https://instagram.com/p/a-photo/", "https://example.org/youtube.com/watch?v=AbCdEf123_-"] {
            d["posts"][0]["sourceUrl"] = json!(url);
            assert_eq!(preparation_state(&d, &item, AT).unwrap(), None);
        }
        d["posts"][0]["sourceUrl"] = json!("https://instagram.com/reel/a-video/");
        assert_eq!(preparation_state(&d, &item, AT).unwrap(), Some("media_wait"));
        d["account"] = json!("Other");
        assert_eq!(preparation_state(&d, &item, AT).unwrap(), None);
    }

    #[tokio::test]
    async fn four_concurrent_claimers_reserve_one_download_and_late_transcript_stops_fallback() {
        let mut initial = fixture();
        for (id, channel, title) in [
            ("post-11341:three", "Instagram", "  Обзор   семейного автомобиля #likeavto"),
            ("post-11391:four", "VK", "ОБЗОР СЕМЕЙНОГО АВТОМОБИЛЯ"),
        ] {
            super::super::list_mut(&mut initial, "posts").push(json!({"id":id,"postKey":&id[5..],"title":title,"channel":channel,"attachments":[{"type":"video"}]}));
        }
        let d = std::sync::Arc::new(tokio::sync::Mutex::new(initial));
        let mut tasks = Vec::new();
        for _ in 0..4 {
            let shared = d.clone();
            tasks.push(tokio::spawn(async move {
                // App.change serializes durable snapshot mutation in this way.
                let mut d = shared.lock().await;
                claim(&mut d, AT).unwrap()
            }));
        }
        let mut claims = Vec::new();
        for task in tasks { if let Some(claimed) = task.await.unwrap() { claims.push(claimed); } }
        assert_eq!(claims.len(), 1);
        let (id, first) = claims.pop().unwrap();
        let mut d = d.lock().await;
        assert_eq!(d["jobs"].as_array().unwrap().len(), 1);
        mark_attempt(&mut d, &id, &first, Some("SOURCE_DOWNLOAD_FAILED"), AT).unwrap();
        d["materials"] = json!([{"id":"racing-result","kind":"transcript","postKey":"11391:one","text":"Already obtained while waiting"}]);
        add_visual_fixture(&mut d);
        super::super::knowledge::sync_catalog(&mut d, AT).unwrap();
        assert!(take_source(&mut d, &id, AT).unwrap().is_none());
        assert_eq!(d["jobs"][0]["sourceAttempts"].as_array().unwrap().len(), 1);
        assert!(d["items"].as_array().unwrap().iter().all(|item| preparation_state(&d, item, AT).unwrap().is_none()));
    }
}
