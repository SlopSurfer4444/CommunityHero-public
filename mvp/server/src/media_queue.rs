//! Durable account-scoped media preparation. One group job, bounded source attempts.
//! This module does not install a runtime, publish comments, or infer a full transcript.
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;
use std::sync::atomic::{AtomicBool, Ordering};
#[path="media_cached_audio.rs"]
pub(crate) mod cached_audio;

// Also covers the interval between cancellation and the child process exiting.
static MEDIA_GATE: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
// Coalesced completion hints; the durable queue remains authoritative.
static MEDIA_WAKE: tokio::sync::Notify = tokio::sync::Notify::const_new();
/// Discovery has no process ownership. Each company App coalesces committed
/// source changes independently while the execution gate retains its lease.
pub(crate) struct Discovery {
    pending: AtomicBool,
    gate: tokio::sync::Mutex<()>,
}
impl Default for Discovery {
    fn default()->Self{Self{pending:AtomicBool::new(true),gate:tokio::sync::Mutex::new(())}}
}
struct DiscoveryAttempt<'a>{state:&'a Discovery,committed:bool}
impl Drop for DiscoveryAttempt<'_>{
    fn drop(&mut self){
        // Error or cancellation before settlement must not consume the hint.
        if !self.committed{self.state.pending.store(true,Ordering::Release);MEDIA_WAKE.notify_one();}
    }
}
async fn discover_pending(app:&super::App)->super::ApiResult<()> {
    let Ok(_guard)=app.media_discovery.gate.try_lock() else{return Ok(())};
    if !app.media_discovery.pending.swap(false,Ordering::AcqRel){return Ok(())}
    let mut attempt=DiscoveryAttempt{state:&app.media_discovery,committed:false};
    // Clear before observation: a sync committed during this transaction keeps
    // its own pending hint. Never refresh proofs or claim a process here.
    app.change_media(|d|reconcile(d,&super::now())).await?;
    attempt.committed=true;
    if app.media_discovery.pending.load(Ordering::Acquire){MEDIA_WAKE.notify_one();}
    Ok(())
}
async fn wait_for_trigger(wake: &tokio::sync::Notify, interval: &mut tokio::time::Interval) {
    tokio::select! {
        _ = wake.notified() => {},
        _ = interval.tick() => {},
    }
}
pub(crate) async fn wait_for_work(interval: &mut tokio::time::Interval) {
    wait_for_trigger(&MEDIA_WAKE, interval).await;
}
/// A hint only after durable incoming-sync settlement; no queue/model work here.
pub(crate) fn notify_after_sync_commit(app:&super::App) {
    app.media_discovery.pending.store(true,Ordering::Release);
    MEDIA_WAKE.notify_one();
}
fn media_enabled(background:Option<&str>,generation:Option<&str>,media:Option<&str>)->bool {
    background!=Some("1") && match media {
        Some("1")=>true,
        Some("0")=>false,
        None=>generation!=Some("1"),
        _=>false,
    }
}
/// Explicit media enablement is independent of replies; unset retains legacy mode.
pub(crate) fn background_enabled()->bool {
    background_enabled_with(|key|std::env::var_os(key).map(|value|value.to_str().unwrap_or("invalid").to_owned()))
}
pub(crate) fn background_enabled_with(env:impl Fn(&str)->Option<String>)->bool {
    media_enabled(env("COMMUNITYHERO_BACKGROUND_DISABLED").as_deref(),
        env("COMMUNITYHERO_BACKGROUND_GENERATION_DISABLED").as_deref(),
        env("COMMUNITYHERO_MEDIA_PREPARATION_ENABLED").as_deref())
}
type CommentCutoff=Option<chrono::DateTime<chrono::FixedOffset>>;
fn parse_comment_cutoff(value:Option<&str>)->Result<CommentCutoff,&'static str> {
    value.map(|raw| {
        if raw.trim()!=raw||raw.len()>64{return Err("Invalid media comment cutoff");}
        let at=chrono::DateTime::parse_from_rfc3339(raw).map_err(|_|"Invalid media comment cutoff")?;
        if at.timestamp_subsec_nanos()!=0{return Err("Media comment cutoff must use whole seconds");}
        Ok(at)
    }).transpose()
}
pub(crate) fn cutoff_unix_with(value:Option<&str>)->super::ApiResult<Option<i64>> {
    parse_comment_cutoff(value).map(|cutoff|cutoff.map(|at|at.timestamp())).map_err(super::bad)
}
fn comment_cutoff()->super::ApiResult<CommentCutoff> {
    match std::env::var("COMMUNITYHERO_MEDIA_COMMENT_CUTOFF_UTC") {
        Ok(raw)=>parse_comment_cutoff(Some(&raw)).map_err(super::bad),
        Err(std::env::VarError::NotPresent)=>Ok(None),
        Err(_)=>Err(super::bad("Invalid media comment cutoff")),
    }
}
fn comment_in_scope(item:&Value,cutoff:&CommentCutoff)->bool {
    matches!(text(item,"providerStatus"),"new"|"inprogress"|"in_progress")
        && matches!(text(item,"workflow"),"attention"|"prepared"|"waiting"|"wait"|"active")
        && cutoff.as_ref().is_none_or(|cutoff| item["createdAt"].as_str()
            .and_then(|at|chrono::DateTime::parse_from_rfc3339(at).ok()).is_some_and(|at|at<=*cutoff))
}
// A durable resume is already accepted before this hint is emitted. Keep
// scheduler I/O/proof verification out of its HTTP acknowledgement, while an
// explicit resume still starts work when the periodic background loop is off.
fn spawn_resume_scheduler<F,Fut>(mut schedule:F)->tokio::task::JoinHandle<()>
where F:FnMut()->Fut+Send+'static,Fut:std::future::Future<Output=super::ApiResult<()>>+Send+'static{
    tokio::spawn(super::worker_supervision::supervise_background("media-resume",move||{
        let future=schedule();
        async move {if let Err(error)=future.await {eprintln!("Accepted media resume scheduler: {}",error.1);}}
    }))
}
#[derive(Clone)]
enum ResumeFence { VisualEpoch(u64), DownloadPermit { id:String, attempt_index:usize } }
#[derive(Clone)]
struct ResumeTarget { job_id:String, fence:ResumeFence }
impl ResumeTarget {
    fn from_receipt(receipt:&Value)->super::ApiResult<Self> {
        let job_id=text(receipt,"jobId").to_owned();
        if job_id.is_empty() { return Err(super::internal("Media resume receipt has no job ID")); }
        let fence=if let Some(epoch)=receipt["leaseEpoch"].as_u64() {
            ResumeFence::VisualEpoch(epoch)
        } else {
            let id=text(receipt,"id").to_owned();
            let attempt_index=receipt["attemptIndex"].as_u64().and_then(|n|usize::try_from(n).ok());
            if id.is_empty() || attempt_index.is_none() { return Err(super::internal("Media resume receipt has no retry permit")); }
            ResumeFence::DownloadPermit{id,attempt_index:attempt_index.unwrap()}
        };
        Ok(Self{job_id,fence})
    }
    fn matches(&self,job:&Value)->bool {
        if !current_job(job)||job["kind"]!="media"||job["status"]!="queued" {return false;}
        match &self.fence {
            ResumeFence::VisualEpoch(epoch)=>{
                let progress=&job["result"]["visualProgress"];
                progress["schemaVersion"]==2 && progress["leaseEpoch"]==*epoch
                    && progress["leaseId"].is_null() && !matches!(text(progress,"phase"),"held"|"complete")
            },
            ResumeFence::DownloadPermit{id,attempt_index}=>{
                let attempts=rows(job,"sourceAttempts");
                job["result"]["visualProgress"]["schemaVersion"]!=2
                    && attempts.get(*attempt_index).is_some_and(|attempt|
                        attempt["retryPermit"]["id"]==*id && attempt["status"]=="interrupted")
            }
        }
    }
}
fn schedule_resumed_media(app:&super::App,target:ResumeTarget,guard:tokio::sync::MutexGuard<'static,()>){
    let app=app.clone();
    // Transfer ownership acquired before the durable permit was written. A
    // periodic tick cannot overtake this exact-target one-shot scheduler.
    let initial=std::sync::Arc::new(std::sync::Mutex::new(Some(guard)));
    spawn_resume_scheduler(move||{
        let app=app.clone();let target=target.clone();let initial=initial.clone();
        async move {
            let prior=initial.lock().map_err(|_|super::internal("Media resume gate poisoned"))?.take();
            let guard=match prior {Some(guard)=>guard,None=>MEDIA_GATE.lock().await};
            tick_resumed(&app,&target,guard).await
        }
    });
}
const PURPOSE: &str = "auto_media";
fn current_job(job:&Value)->bool {job["purpose"]==PURPOSE && job["visualContractVersion"]==2}
fn open_comments_only()->bool {std::env::var("COMMUNITYHERO_MEDIA_OPEN_COMMENTS_ONLY").as_deref()==Ok("1")
    ||std::env::var_os("COMMUNITYHERO_MEDIA_COMMENT_CUTOFF_UTC").is_some()}
fn wanted(job:&Value,index:&QueueIndex,only_open:bool)->bool {
    index.open_groups.contains(text(job,"groupKey")) || (!only_open && job["manualRequested"]==true)
}
pub(super) fn manual_source_requested(d:&Value,job:&Value,post:&Value)->bool{
    let Ok(cutoff)=comment_cutoff() else{return false;};
    manual_source_requested_with_cutoff(d,job,post,&cutoff)
}
fn manual_source_requested_with_cutoff(d:&Value,job:&Value,post:&Value,cutoff:&CommentCutoff)->bool{
    let pin=&job["manualAcquisition"];
    if pin["version"]!=1||job["manualRequested"]!=true||!current_job(job)
        ||!job_binding_matches(d,job)||job["refId"]!=post["id"]||pin["postId"]!=post["id"]
        ||pin["postKey"]!=post["postKey"]||pin["account"]!=d["account"]
        ||pin["connectorBinding"]!=super::active_binding(d).ok().map(|b|b.to_json()).unwrap_or(Value::Null)
        ||pin["sourceVersion"]!=super::media_fullframes::source_version(post,text(&d,"account"))
        ||!video(post)||!super::knowledge::in_account(post,text(&d,"account"))
        ||(!post["connectorBinding"].is_null()&&post["connectorBinding"]!=pin["connectorBinding"]){return false;}
    cutoff.as_ref().is_none_or(|cutoff|rows(d,"items").iter().any(|item|
        (item["postId"]==post["id"]||(!text(post,"postKey").is_empty()&&item["postKey"]==post["postKey"]))
            &&super::active_binding(d).is_ok_and(|b|super::bound_item(&b,item).is_ok())
            &&item["createdAt"].as_str().and_then(|s|chrono::DateTime::parse_from_rfc3339(s).ok()).is_some_and(|at|at<=*cutoff)))
}
fn wanted_scoped(d:&Value,job:&Value,index:&QueueIndex,only_open:bool)->bool{
    let Ok(cutoff)=comment_cutoff() else{return false;};
    wanted_scoped_with_cutoff(d,job,index,only_open,&cutoff)
}
fn wanted_scoped_with_cutoff(d:&Value,job:&Value,index:&QueueIndex,only_open:bool,cutoff:&CommentCutoff)->bool{
    wanted(job,index,only_open)||index.posts(text(job,"groupKey")).iter()
        .any(|post|manual_source_requested_with_cutoff(d,job,post,cutoff))
}

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
        match comment_cutoff() {
            Ok(cutoff)=>Self::with_cutoff(d,&cutoff),
            Err(_)=>Self{groups:BTreeMap::new(),candidates:BTreeMap::new(),open_groups:BTreeSet::new()},
        }
    }
    fn with_cutoff(d:&Value,cutoff:&CommentCutoff)->Self {
        let binding=super::active_binding(d).ok().map(|binding|binding.to_json());
        let foreign:BTreeSet<&str>=rows(d,"posts").iter().filter(|post|!post["connectorBinding"].is_null()
            &&binding.as_ref()!=Some(&post["connectorBinding"])).map(|post|text(post,"id")).collect();
        let groups:BTreeMap<String,String>=account_scope(d).map(|scope|super::knowledge::visual_groups(d,scope).into_iter()
            .filter(|(id,_)|!foreign.contains(id.as_str()))
            .map(|(id,key)|(id,format!("visual-v2:{key}"))).collect()).unwrap_or_default();
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
                if comment_in_scope(item,cutoff)
                    && super::bound_item(&binding,item).is_ok() {
                    if let Some(key)=groups.get(text(item,"postId")){open_groups.insert(key.clone());}
                    if let Some(key)=post_keys.get(text(item,"postKey")){open_groups.insert((*key).clone());}
                }
            }
        }
        // Keep already persisted duration-suffixed keys addressable after the
        // queue adopts the knowledge selector's tolerant video grouping. This
        // preserves in-flight cursors and their source-attempt ledgers.
        for job in rows(d,"jobs").iter().filter(|j|current_job(j)&&job_binding_matches(d,j)) {
            let alias=text(job,"groupKey");
            let source_id=job["result"]["visualProgress"]["sourcePostId"].as_str()
                .unwrap_or_else(||text(job,"refId"));
            let Some(canonical)=groups.get(source_id) else{continue;};
            if alias.is_empty()||alias==canonical{continue;}
            if let Some(peers)=candidates.get(canonical).cloned(){candidates.entry(alias.to_owned()).or_insert(peers);}
            if open_groups.contains(canonical){open_groups.insert(alias.to_owned());}
        }
        Self{groups,candidates,open_groups}
    }
    fn posts(&self,key:&str)->&[Value]{self.candidates.get(key).map(Vec::as_slice).unwrap_or(&[])}
}
fn open_post(d: &Value, post: &Value) -> bool {
    let Ok(cutoff)=comment_cutoff() else{return false;};
    open_post_with_cutoff(d,post,&cutoff)
}
fn open_post_with_cutoff(d:&Value,post:&Value,cutoff:&CommentCutoff)->bool {
    let Ok(binding) = super::active_binding(d) else { return false; };
    if !post["connectorBinding"].is_null()&&post["connectorBinding"]!=binding.to_json(){return false;}
    rows(d, "items").iter().any(|item| {
        comment_in_scope(item,cutoff)
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
    for job in rows(d,"jobs").iter().filter(|j|j["kind"]=="media"&&current_job(j)&&job_binding_matches(d,j)){
        for(index,attempt)in rows(job,"sourceAttempts").iter().enumerate(){
            let same_post=attempt["postId"]==post["id"]||(!text(post,"postKey").is_empty()&&attempt["postKey"]==post["postKey"]);
            let current_revision=attempt["sourceVersion"].is_null()||version.as_ref().is_some_and(|v|attempt["sourceVersion"]==*v);
            if (same_post&&current_revision)||(!same_post&&source.as_ref().is_some_and(|s|attempt["sourceKey"]==*s)){matches.push((job,index,attempt));}
        }
    }matches
}
fn job_binding_matches(d:&Value,job:&Value)->bool{
    let Ok(account)=account_scope(d) else{return false;};
    if job["account"]!=account{return false;}
    let Ok(binding)=super::active_binding(d) else{return false;};
    let current=binding.to_json();
    (job["connectorBinding"].is_null()||job["connectorBinding"]==current)
        && (job["result"]["visualProgress"]["connectorBinding"].is_null()
            ||job["result"]["visualProgress"]["connectorBinding"]==current)
}
// Legacy absence is compatible only at verified admission. Never repair an
// explicit foreign/malformed binding or a checkpoint whose source has changed.
fn verified_checkpoint_binding(d:&Value,job:&Value)->super::ApiResult<Value>{
    let account=account_scope(d)?;let binding=super::active_binding(d)?.to_json();
    let p=&job["result"]["visualProgress"];
    super::media_speech_assets::require_progress(d,p).map_err(|e|super::conflict(&e))?;
    if p.get("assetPin").is_some()&&job["videoSpeechAssetPin"]!=p["assetPin"]{
        return Err(super::conflict("Media speech asset ownership changed"));
    }
    let post=super::row(d,"posts",text(p,"sourcePostId"))?;
    if !current_job(job)||job["kind"]!="media"||!job_binding_matches(d,job)
        ||p["schemaVersion"]!=2||p["account"]!=account||p["connectorBinding"]!=binding
        ||job["refId"]!=post["id"]||p["sourcePostKey"]!=post["postKey"]
        ||!super::knowledge::in_account(post,account)
        ||(!post["connectorBinding"].is_null()&&post["connectorBinding"]!=binding)
        ||p["sourceVersion"]!=super::media_fullframes::source_version(post,account)
        ||p["materialEpoch"]!=material_epoch(d,post)||!job_processing_wanted(d,job,post)
        ||(!job["manualAcquisition"].is_null()&&!manual_source_requested(d,job,post))
        ||p["leaseEpoch"].as_u64().and_then(|epoch|epoch.checked_add(1)).is_none(){
        return Err(super::conflict("Media resume source or binding changed"));
    }
    Ok(binding)
}

fn authorize_visual_resume(d:&mut Value,job_id:&str,epoch:u64)->super::ApiResult<Value>{
    let job=super::row(d,"jobs",job_id)?;
    verified_checkpoint_binding(d,job)?;
    let p=&job["result"]["visualProgress"];
    if !p["leaseId"].is_null(){return Err(super::conflict("Media resume still owns a lease"));}
    // Resume only the existing checkpoint. This cannot turn a held source into
    // a fresh download or bypass its attempt budget.
    if job["connectorBinding"].is_null(){
        if !matches!(text(p,"resumePhase"),"inventory"|"select"|"scan"|"finalize")
            ||super::media_fullframes::reference(&p["source"]).is_err()
            ||p["sourceIdentity"]["account"]!=p["account"]
            ||p["sourceIdentity"]["postKey"]!=p["sourcePostKey"]
            ||p["sourceIdentity"]["mediaSha256"]!=p["source"]["sha256"]{
            return Err(super::conflict("Legacy media resume requires a bound retained source"));
        }
    }
    super::media_fullframes::resume(super::row_mut(d,"jobs",job_id)?,epoch).map_err(|e|super::conflict(&e))?;
    super::audit(d,"media.visual_resume_authorized",job_id);
    Ok(json!({"jobId":job_id,"leaseEpoch":epoch,"status":"queued"}))
}
// Shared by the scoped transaction validator: independently derive the one
// permitted legacy migration from the old workspace, never trust a marker.
pub(crate) fn verified_legacy_binding_claim(d:&Value,prior:&Value,next:&Value)->bool{
    let Ok(binding)=verified_checkpoint_binding(d,prior) else{return false;};
    let old=&prior["result"]["visualProgress"];let new=&next["result"]["visualProgress"];
    if !prior["connectorBinding"].is_null()||prior["status"]!="queued"||next["status"]!="running"
        ||next["connectorBinding"]!=binding||!old["leaseId"].is_null()
        ||!matches!(text(old,"phase"),"download"|"inventory"|"select"|"scan"|"finalize")
        ||text(new,"leaseId").is_empty()||old["leaseEpoch"].as_u64().and_then(|n|n.checked_add(1))!=new["leaseEpoch"].as_u64()
        ||prior["sourceAttempts"]!=next["sourceAttempts"]
        ||next_scheduler_turn(d).ok()!=new["schedulerTurn"].as_u64()
        ||new["schedulerTurn"]!=next["result"]["mediaSchedulerTurn"] {return false;}
    let mut expected=old.clone();
    for key in ["leaseId","leaseEpoch","schedulerTurn"]{expected[key]=new[key].clone();}
    *new==expected
}
fn current_source_job(d:&Value,job:&Value)->bool{
    if !job_binding_matches(d,job){return false;}
    if job.get("videoSpeechAssetPin").is_some()
        &&super::media_speech_assets::current(d,&job["videoSpeechAssetPin"]).is_err(){return false;}
    if superseded_unstarted_acquisition(d,job){return false;}
    let p=&job["result"]["visualProgress"];if p["schemaVersion"]!=2{return true;}
    rows(d,"posts").iter().find(|post|post["id"]==p["sourcePostId"]).is_some_and(|post|account_scope(d).is_ok_and(|scope|
        p["sourceVersion"]==super::media_fullframes::source_version(post,scope)
            && p["materialEpoch"]==material_epoch(d,post)))
}
fn unstarted_acquisition(job:&Value)->bool{
    current_job(job)&&job["kind"]=="media"
        &&(job["acquisitionProfile"].is_null()||job["acquisitionProfile"]==cached_audio::TEXT_PROFILE
            ||job["acquisitionProfile"]==cached_audio::SPEECH_PROFILE)
        &&job["sourceAttempts"].as_array().is_some_and(Vec::is_empty)
        &&job["result"]["visualProgress"].is_null()
        &&job["legacyMissingStages"].is_null()&&job["reconciledAcquisition"].is_null()
        &&job["downloadRetry"].is_null()&&job["sourceRollover"].is_null()
}
fn superseded_unstarted_acquisition(d:&Value,job:&Value)->bool{
    if !unstarted_acquisition(job)||job["status"]!="cancelled"{return false;}
    let Some(next)=job["result"]["acquisitionSupersededBy"].as_str().filter(|s|!s.is_empty()) else{return false;};
    rows(d,"jobs").iter().any(|new|new["id"]==next&&current_job(new)&&new["kind"]=="media"
        &&new["account"]==job["account"]&&new["connectorBinding"]==job["connectorBinding"]
        &&new["refId"]==job["refId"]
        &&new["acquisitionSupersedes"]["jobId"]==job["id"]
        &&new["acquisitionSupersedes"]["sourceVersion"].as_str().is_some_and(|s|s.len()==64)
        &&new["acquisitionSupersedes"]["policySha256"].as_str().is_some_and(|s|s.len()==64))
}
// Missing-stage admission is selected and append-only. Historical missing pins
// remain unknown; they never assert source-byte equality or full audio coverage.
fn opaque_source_matches(d:&Value,job:&Value,post:&Value)->bool{
    job["purpose"]!=PURPOSE&&media_source_matches(d,job,post)
}
fn media_source_matches(d:&Value,job:&Value,post:&Value)->bool{
    let Some(scope)=account_scope(d).ok() else{return true;};
    let source=super::knowledge::media_source_key(post,scope);
    job["kind"]=="media" && (job["refId"]==post["id"]
        || source.as_ref().is_some_and(|s|rows(d,"posts").iter().find(|p|p["id"]==job["refId"])
            .and_then(|p|super::knowledge::media_source_key(p,scope)).as_ref()==Some(s)))
}
fn unstarted_opaque_hold(job:&Value)->bool{
    job["status"]=="failed" && job["sourceAttempts"].as_array().is_some_and(Vec::is_empty)
        && job["result"]["visualProgress"].is_null()
        && job["error"]=="All known media sources have already been attempted"
}
fn competing_current_work(d:&Value,post:&Value,own_id:Option<&str>)->bool{
    rows(d,"jobs").iter().any(|j|media_source_matches(d,j,post)&&j["purpose"]==PURPOSE&&!current_job(j)&&j["status"]!="completed")
        || rows(d,"jobs").iter().any(|j|current_job(j)&&media_source_matches(d,j,post)
        && own_id.is_none_or(|id|text(j,"id")!=id)
        && (!job_binding_matches(d,j)||current_source_job(d,j)
            || !matches!(text(j,"status"),"completed"|"failed"|"cancelled"))
        && !(job_binding_matches(d,j)&&unstarted_opaque_hold(j)))
}
fn legacy_history_shape(job:&Value,scope:&str,binding:&Value,post:&Value)->bool{
    // Absent/null historical ledger is explicitly unknown, not a consumed V2
    // budget. A present non-array ledger or malformed attempt is never empty.
    let Some(value)=job.get("sourceAttempts") else{return true;};
    if value.is_null(){return true;}
    value.as_array().is_some_and(|attempts|attempts.iter().all(|a|a.is_object()
        && a["status"]=="completed" && super::knowledge::in_account(a,scope)
        && (a["connectorBinding"].is_null()||a["connectorBinding"]==*binding)
        && a.get("postId").is_none_or(|v|v.is_string()&&*v==post["id"])
        && a.get("postKey").is_none_or(|v|v.is_string()&&*v==post["postKey"])
        && a.get("sourceVersion").is_none_or(|v|v.as_str().is_some_and(|s|s.len()==64&&s.bytes().all(|b|b.is_ascii_hexdigit())))
        && a.get("sourceKey").is_none_or(Value::is_string)
        && a.get("attemptNumber").is_none_or(|v|v.as_u64().is_some_and(|n|n>0))))
}
fn legacy_completed_for(d:&Value,job:&Value,post:&Value)->bool{
    let Ok(scope)=account_scope(d) else{return false;};
    let Ok(binding)=super::active_binding(d) else{return false;};
    job["kind"]=="media" && !text(job,"id").is_empty()
        && rows(d,"jobs").iter().filter(|j|j["id"]==job["id"]).count()==1
        && job["purpose"].is_null() && job["visualContractVersion"].is_null()
        && job["refId"]==post["id"] && job["status"]=="completed" && job["result"]["processed"]==true
        && job["result"].as_object().is_some_and(|result|result.len()==1&&result.contains_key("processed"))
        && super::knowledge::in_account(job,scope)
        && (job["connectorBinding"].is_null()||job["connectorBinding"]==binding.to_json())
        && job["result"]["visualProgress"].is_null() && job["result"]["visualEvidence"].is_null()
        && legacy_history_shape(job,scope,&binding.to_json(),post)
}
fn legacy_upgrade_pin(d:&Value,post:&Value,owner:bool)->super::ApiResult<Option<Value>>{
    let blockers:Vec<_>=rows(d,"jobs").iter().filter(|j|opaque_source_matches(d,j,post)).collect();
    if blockers.is_empty(){return Ok(None);}
    if !owner{return Err(super::conflict("Legacy missing-stage processing requires owner"));}
    let scope=account_scope(d)?;let binding=super::active_binding(d)?.to_json();
    if !super::knowledge::in_account(post,scope)
        || (!post["connectorBinding"].is_null()&&post["connectorBinding"]!=binding)
        || !source_attempts(d,post).is_empty()
        || competing_current_work(d,post,None)
        || blockers.iter().any(|j|!legacy_completed_for(d,j,post)){
        return Err(super::conflict("Legacy media work is not eligible for missing-stage admission"));
    }
    let source_key=super::knowledge::media_source_key(post,scope)
        .ok_or_else(||super::conflict("Current media source identity unavailable"))?;
    let lineage:Vec<_>=blockers.iter().map(|j|json!({"jobId":j["id"],"snapshotSha256":super::media_fullframes::hash(j)})).collect();
    Ok(Some(json!({"version":1,"account":scope,"connectorBinding":binding,
        "postId":post["id"],"postKey":post["postKey"],"sourceKey":source_key,
        "sourceVersion":super::media_fullframes::source_version(post,scope),
        "materialEpoch":material_epoch(d,post),"admissionPolicy":super::post_media_policy::effective(d,post)?,
        "ownerPolicySha256":super::media_fullframes::hash(&d["settings"]["postMediaPolicies"][text(post,"id")]),
        "policyDefaultsSha256":super::media_fullframes::hash(&d["settings"]["mediaPolicyDefaults"]),"legacyJobs":lineage})))
}
fn legacy_upgrade_current(d:&Value,job:&Value,post:&Value)->bool{
    legacy_upgrade_bound(d,job,post,true)
}
fn legacy_upgrade_proposed(d:&Value,job:&Value,post:&Value)->bool{
    legacy_upgrade_bound(d,job,post,false)
}
fn legacy_upgrade_bound(d:&Value,job:&Value,post:&Value,persisted:bool)->bool{
    let pin=&job["legacyMissingStages"];let Ok(scope)=account_scope(d) else{return false;};
    let Ok(binding)=super::active_binding(d) else{return false;};
    if pin["version"]!=1 || !current_job(job)||!job_binding_matches(d,job)||text(job,"id").is_empty()
        || !job["sourceAttempts"].is_array()
        || !rows(job,"sourceAttempts").iter().all(|a|a.is_object()
            && matches!(text(a,"status"),"running"|"completed"|"failed"|"interrupted")
            && a["postId"]==pin["postId"]&&a["postKey"]==pin["postKey"]
            && a["sourceKey"]==pin["sourceKey"]&&a["sourceVersion"]==pin["sourceVersion"]
            && a["attemptNumber"].as_u64().is_some_and(|n|n>0))
        || rows(d,"jobs").iter().filter(|j|j["id"]==job["id"]).count()!=usize::from(persisted)
        || job["manualRequested"]!=true || pin["account"]!=scope
        || pin["connectorBinding"]!=binding.to_json() || job["refId"]!=pin["postId"]
        || post["id"]!=pin["postId"] || post["postKey"]!=pin["postKey"]
        || !super::knowledge::in_account(post,scope)
        || (!post["connectorBinding"].is_null()&&post["connectorBinding"]!=binding.to_json())
        || pin["sourceKey"].as_str()!=super::knowledge::media_source_key(post,scope).as_deref()
        || pin["sourceVersion"]!=super::media_fullframes::source_version(post,scope)
        || pin["materialEpoch"]!=material_epoch(d,post)
        || pin["ownerPolicySha256"]!=super::media_fullframes::hash(&d["settings"]["postMediaPolicies"][text(post,"id")])
        || pin["policyDefaultsSha256"]!=super::media_fullframes::hash(&d["settings"]["mediaPolicyDefaults"])
        || competing_current_work(d,post,Some(text(job,"id"))) {return false;}
    let lineage=rows(pin,"legacyJobs");
    !lineage.is_empty() && lineage.iter().all(|item|rows(d,"jobs").iter().find(|j|j["id"]==item["jobId"])
        .is_some_and(|j|legacy_completed_for(d,j,post)&&item["snapshotSha256"]==super::media_fullframes::hash(j)))
        && !rows(d,"jobs").iter().any(|j|opaque_source_matches(d,j,post)
            && !lineage.iter().any(|item|item["jobId"]==j["id"]&&item["snapshotSha256"]==super::media_fullframes::hash(j)))
}
fn attempted(d: &Value, post: &Value) -> bool {
    attempted_except_legacy(d,post,None)
}
// One explicit new read allocation. Historical job hashes are evidence of
// lineage, never proof that their acquisition succeeded or that audio is full.
fn reconciled_scope(d:&Value,post:&Value)->super::ApiResult<(String,String)> {
    let account=account_scope(d)?;
    let source=super::knowledge::media_source_key(post,account)
        .ok_or_else(||super::conflict("Media source identity unavailable"))?;
    let binding=super::active_binding(d)?.to_json();
    let stable=super::media_fullframes::hash(&json!([account,binding["id"],binding["connector"],
        binding["accountId"],binding["providerAccountId"],source]));
    Ok((source,stable))
}
// Historical auto_media jobs can predate the V2 visual contract. A processed
// V1 summary is still opaque: it does not prove current visual coverage or
// disclose the complete number of earlier acquisition attempts.
fn reconciled_unproven_v1(job:&Value)->bool {
    job["purpose"]==PURPOSE && job["visualContractVersion"].is_null()
        && job["status"]=="completed" && job["result"]["processed"]==true
        && job["result"]["visualProgress"].is_null()
        && job["result"]["visualEvidence"].is_null()
}
fn reconciled_lineage(d:&Value,post:&Value,own_id:Option<&str>)->super::ApiResult<(Vec<Value>,String)> {
    let (source,scope)=reconciled_scope(d,post)?;
    let account=account_scope(d)?;let binding=super::active_binding(d)?.to_json();
    let mut lineage=Vec::new();let mut latest=None;let mut opaque_seen=false;
    if rows(d,"jobs").iter().any(|j|j["reconciledAcquisition"]["stableScope"]==scope
        && own_id.is_none_or(|id|text(j,"id")!=id)) {
        return Err(super::conflict("Media source already has a reconciled acquisition allocation"));
    }
    for job in rows(d,"jobs").iter().filter(|j|j["kind"]=="media"
        && own_id.is_none_or(|id|text(j,"id")!=id)
        && (media_source_matches(d,j,post)||rows(j,"sourceAttempts").iter().any(|a|a["sourceKey"]==source))) {
        let attempts=match job.get("sourceAttempts") {
            None|Some(Value::Null) if !current_job(job)=>&[][..],
            Some(Value::Array(attempts))=>attempts.as_slice(),
            _=>return Err(super::conflict("Media source attempt history malformed")),
        };
        if !job["reconciledAcquisition"].is_null(){
            return Err(super::conflict("Media source already has a reconciled acquisition allocation"));
        }
        if !super::knowledge::in_account(job,account)
            || (!job["connectorBinding"].is_null()&&job["connectorBinding"]!=binding)
            || !matches!(text(job,"status"),"completed"|"failed")
            || attempts.iter().any(|a|!a.is_object()
                || !matches!(text(a,"status"),"completed"|"failed")
                || !super::knowledge::in_account(a,account)
                || !a["connectorBinding"].is_null()&&a["connectorBinding"]!=binding
                || !a["sourceKey"].is_null()&&a["sourceKey"]!=source
                || a.get("postId").is_some_and(|v|v.as_str().is_none_or(|id|
                    rows(d,"posts").iter().find(|p|p["id"]==id).is_none_or(|p|
                        super::knowledge::media_source_key(p,account).as_deref()!=Some(source.as_str()))))
                || a.get("sourceVersion").is_some_and(|v|!v.is_null()&&v.as_str().is_none_or(|s|s.len()!=64||!s.bytes().all(|b|b.is_ascii_hexdigit())))
                || !a["retryPermit"].is_null())
            || !job["downloadRetry"].is_null() && !attempts.iter()
                .any(|a|a["retryOf"]["permitId"]==job["downloadRetry"]["id"])
            || !job["result"]["visualProgress"]["leaseId"].is_null()
            || !job["result"]["visualProgress"]["source"].is_null() {
            return Err(super::conflict("Media source ownership requires reconciliation"));
        }
        let finished=chrono::DateTime::parse_from_rfc3339(text(job,"finishedAt"))
            .map_err(|_|super::conflict("Media source terminal time unavailable"))?;
        if finished>chrono::Utc::now(){return Err(super::conflict("Media source terminal time invalid"));}
        latest=Some(latest.map_or(finished,|prior:chrono::DateTime<chrono::FixedOffset>|prior.max(finished)));
        opaque_seen|=opaque_source_matches(d,job,post)||reconciled_unproven_v1(job);
        lineage.push(json!({"jobId":job["id"],"snapshotSha256":super::media_fullframes::hash(job)}));
    }
    if lineage.is_empty()||!opaque_seen{return Err(super::conflict("No terminal opaque source history to reconcile"));}
    lineage.sort_by(|a,b|text(a,"jobId").cmp(text(b,"jobId")));
    Ok((lineage,latest.unwrap().to_rfc3339()))
}
fn reconciled_source_bound(d:&Value,job:&Value,post:&Value)->bool {
    let p=&job["result"]["visualProgress"];
    if p.is_null(){return rows(job,"sourceAttempts").is_empty();}
    if p["source"].is_null(){return p["phase"]=="download";}
    let phase=if p["phase"]=="held"{text(p,"resumePhase")}else{text(p,"phase")};
    let Ok(binding)=super::active_binding(d) else{return false;};
    let account=match account_scope(d){Ok(account)=>account,Err(_)=>return false};
    p["schemaVersion"]==2 && matches!(phase,"inventory"|"select"|"scan"|"finalize"|"complete")
        &&p["account"]==account &&p["connectorBinding"]==binding.to_json()
        &&p["sourcePostId"]==post["id"] &&p["sourcePostKey"]==post["postKey"]
        &&p["sourceVersion"]==job["reconciledAcquisition"]["sourceVersion"]
        &&p["sourceIdentity"]["account"]==account
        &&p["sourceIdentity"]["postKey"]==post["postKey"]
        &&p["sourceIdentity"]["mediaSha256"]==p["source"]["sha256"]
        &&p["sourceIdentity"]["durationMs"].as_u64().is_some_and(|n|n>0)
        &&super::media_fullframes::reference(&p["source"]).is_ok()
}
fn reconciled_policy_settings(d:&Value,post:&Value)->String {
    super::media_fullframes::hash(&json!([d["settings"]["postMediaPolicies"][text(post,"id")],
        d["settings"]["mediaPolicyDefaults"]]))
}
// A new source's own ffprobe result may add a duration decisionBasis to the
// effective policy after the admitted download checkpoint. Compare the grant
// with the same policy excluding only this job's newly learned duration. All
// stored owner policy, other jobs, account and source checks still apply.
fn reconciled_policy_current(d:&Value,job:&Value,post:&Value)->bool {
    let pin=&job["reconciledAcquisition"];
    let Ok(current)=super::post_media_policy::effective(d,post) else{return false;};
    // V62 grants lacked an explicit default-threshold pin. They can retain
    // exact policy parity, but cannot use the new derived-duration exception.
    if pin["policySettingsSha256"].is_null(){return pin["policySha256"]==super::media_fullframes::hash(&current);}
    if pin["policySettingsSha256"]!=reconciled_policy_settings(d,post){return false;}
    if pin["policySha256"]==super::media_fullframes::hash(&current){return true;}
    let p=&job["result"]["visualProgress"];
    if !reconciled_source_bound(d,job,post)||p["source"].is_null(){return false;}
    let Some(probe)=super::post_media_policy::probed_duration(d,post) else{return false;};
    if probe["durationMs"]!=p["sourceIdentity"]["durationMs"]
        ||probe["sourceSha256"]!=p["source"]["sha256"]
        ||current["decisionBasis"]["kind"]!="probed_duration_threshold"
        ||current["decisionBasis"]["durationMs"]!=probe["durationMs"]
        ||current["decisionBasis"]["sourceSha256"]!=probe["sourceSha256"]{return false;}
    // effective() consults only account, binding, settings and same-post V2
    // jobs for its duration. Retain every other candidate and its original
    // order; only this grant's probe is absent from the baseline.
    let jobs:Vec<_>=rows(d,"jobs").iter().filter(|other|other["id"]!=job["id"]
        &&other["kind"]=="media"&&other["purpose"]==PURPOSE
        &&other["visualContractVersion"]==2&&other["refId"]==post["id"]).cloned().collect();
    let mut baseline=json!({"account":d["account"],"jobs":jobs});
    for key in ["connectorBinding","settings"] {
        if let Some(value)=d.get(key){baseline[key]=value.clone();}
    }
    super::post_media_policy::effective(&baseline,post).is_ok_and(|old|
        pin["policySha256"]==super::media_fullframes::hash(&old))
}
fn reconciled_current(d:&Value,job:&Value,post:&Value)->bool {
    let pin=&job["reconciledAcquisition"];
    if pin["version"]!=1||job["kind"]!="media"||!current_job(job)||!job_binding_matches(d,job)
        ||job["refId"]!=post["id"]||pin["postId"]!=post["id"]||pin["postKey"]!=post["postKey"]
        ||pin["sourceVersion"]!=super::media_fullframes::source_version(post,text(&pin,"account"))
        ||pin["materialEpoch"]!=material_epoch(d,post)
        ||!reconciled_source_bound(d,job,post)||!reconciled_policy_current(d,job,post)
        ||pin["account"]!=d["account"]||pin["connectorBinding"]!=super::active_binding(d).ok().map(|b|b.to_json()).unwrap_or(Value::Null)
        ||!open_post(d,post){return false;}
    let Ok((source,scope))=reconciled_scope(d,post) else{return false;};
    if pin["sourceKey"]!=source||pin["stableScope"]!=scope{return false;}
    let Ok((lineage,cutoff))=reconciled_lineage(d,post,Some(text(job,"id"))) else{return false;};
    pin["priorJobs"]==json!(lineage)&&pin["terminalCutoffUtc"]==cutoff
}
fn reconciled_no_fallback(projection:&Value)->bool {
    projection.get("fallbackUrl").is_none_or(|value|
        value.is_null()||value.as_str()==Some(""))
}
fn reconciled_locator_matches(post:&Value,projection:&Value,account:&str,source_key:&str)->bool {
    let canonical=|raw:&str|{
        if raw.is_empty(){return false;}
        let mut identity=post.clone();identity["attachments"]=json!([]);
        identity["postKey"]=Value::Null;
        identity["sourceUrl"]=json!(raw);
        super::knowledge::media_source_key(&identity,account).as_deref()==Some(source_key)
    };
    let Some(locator)=projection["sourceUrl"].as_str() else{return false;};
    if !canonical(locator){return false;}
    ["sourceUrl","attachmentSourceUrl","url"].iter()
        .any(|key|canonical(text(post,key)))
        || rows(post,"attachments").iter().any(|a|
            ["sourceUrl","source_url","url"].iter().any(|key|canonical(text(a,key))))
}
fn attempted_except_legacy(d:&Value,post:&Value,pin:Option<&Value>)->bool {
    let Some(scope)=account_scope(d).ok() else{return true;};
    let opaque=rows(d,"jobs").iter().any(|job|opaque_source_matches(d,job,post)
        && !pin.is_some_and(|pin|rows(pin,"legacyJobs").iter().any(|item|
            item["jobId"]==job["id"]&&item["snapshotSha256"]==super::media_fullframes::hash(job)
                && legacy_completed_for(d,job,post))));
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
    if let Some(pin)=job.get("videoSpeechAssetPin"){
        super::media_speech_assets::current(d,pin).ok()?;
        if job["acquisitionProfile"]!=cached_audio::SPEECH_PROFILE||!rows(job,"sourceAttempts").is_empty(){return None;}
        // An exact attachment owns its own attempt. A title-family sibling or
        // another attachment's failure is never an alternative source.
        return rows(d,"posts").iter().find(|p|p["id"]==pin["postId"]&&p["id"]==job["refId"]).cloned();
    }
    if job["reconciledAcquisition"].is_object(){
        let post=index.posts(text(job,"groupKey")).iter().find(|p|p["id"]==job["reconciledAcquisition"]["postId"])?;
        return (reconciled_current(d,job,post)&&rows(job,"sourceAttempts").is_empty()).then(||post.clone());
    }
    if job["legacyMissingStages"].is_object(){
        let pin=&job["legacyMissingStages"];
        let post=index.posts(text(job,"groupKey")).iter().find(|p|p["id"]==pin["postId"])?;
        return (legacy_upgrade_current(d,job,post)
            && !attempted_except_legacy(d,post,Some(pin))).then(||post.clone());
    }
    if job["sourceRollover"].is_object(){
        let pin=&job["sourceRollover"];
        let post=index.posts(text(job,"groupKey")).iter().find(|p|p["id"]==pin["postId"])?;
        let scope=account_scope(d).ok()?;
        if pin["sourceKey"].as_str()!=super::knowledge::media_source_key(post,scope).as_deref()
            || text(pin,"sourceVersion")!=super::media_fullframes::source_version(post,scope)
            || pin["connectorBinding"]!=super::active_binding(d).ok()?.to_json()
            || !open_post(d,post) || attempted(d,post){return None;}
        return Some(post.clone());
    }
    if job["downloadRetry"].is_object(){
        let permit=&job["downloadRetry"];
        let post=index.posts(text(job,"groupKey")).iter().find(|p|p["id"]==permit["postId"])?;
        let scope=account_scope(d).ok()?;
        if permit["sourceVersion"]!=super::media_fullframes::source_version(post,scope)
            || permit["connectorBinding"]!=super::active_binding(d).ok()?.to_json(){return None;}
        return (!attempted(d,post)).then(||post.clone());
    }
    if !job["manualAcquisition"].is_null(){
        let post=index.posts(text(job,"groupKey")).iter().find(|p|p["id"]==job["refId"])?;
        return (manual_source_requested(d,job,post)&&job_processing_wanted(d,job,post)&&!attempted(d,post)).then(||post.clone());
    }
    let mut ready: Vec<_> = index.posts(text(job,"groupKey")).iter()
        .filter(|p| !attempted(d,p)&&job_processing_wanted(d,job,p)).cloned().collect();
    let previous = rows(job, "sourceAttempts").last().map(|a| text(a, "channel")).unwrap_or("");
    ready.sort_by_key(|p| (!previous.is_empty() && text(p, "channel") == previous, text(p, "id").to_owned()));
    ready.into_iter().next()
}
fn has_required_media(d: &Value, post: &Value, at: &str) -> super::ApiResult<bool> {
    if video_count(post)>1&&!super::media_speech_assets::all_ready(d,post,at).map_err(|e|super::conflict(&e))?{return Ok(false);}
    let lookup=super::knowledge::TranscriptLookup::new(d,at).map_err(|e|super::bad(&e))?;
    policy_ready(d,post,&lookup)
}
fn policy_ready(d:&Value,post:&Value,lookup:&super::knowledge::TranscriptLookup)->super::ApiResult<bool>{
    let policy=super::post_media_policy::effective(d,post)?;
    lookup.ready_for_policy(post,policy["visualRequired"]==true).map_err(|e|super::bad(&e))
}
fn visual_wanted(d:&Value,post:&Value)->bool{
    super::post_media_policy::effective(d,post).is_ok_and(|p|p["visualRequired"]==true)
}
fn default_text_wanted(d:&Value,post:&Value)->bool{
    super::post_media_policy::effective(d,post).is_ok_and(|p|
        p["mode"]=="full_audio_only" && p["ownerAuthorizedAudioOnly"]!=true)
}
fn text_download_wanted(d:&Value,job:&Value,post:&Value)->bool{
    (job["acquisitionProfile"]==cached_audio::TEXT_PROFILE||job["acquisitionProfile"]==cached_audio::SPEECH_PROFILE)
        && default_text_wanted(d,post)
        && (job["result"]["visualProgress"].is_null()
            ||job["result"]["visualProgress"]["phase"]=="download"
            ||job["result"]["visualProgress"]["phase"]=="held"
                &&job["result"]["visualProgress"]["resumePhase"]=="download")
}
fn job_processing_wanted(d:&Value,job:&Value,post:&Value)->bool{
    if let Some(pin)=job.get("videoSpeechAssetPin"){
        return job["acquisitionProfile"]==cached_audio::SPEECH_PROFILE&&pin["postId"]==post["id"]
            &&super::media_speech_assets::current(d,pin).is_ok()
            &&(job["result"]["visualProgress"].is_null()
                ||job["result"]["visualProgress"]["phase"]=="download"
                ||job["result"]["visualProgress"]["phase"]=="held"
                    &&job["result"]["visualProgress"]["resumePhase"]=="download");
    }
    // A captured text profile can never be converted into frame processing by
    // a later policy change. Only a new explicit visual job may take that path.
    if job["acquisitionProfile"]==cached_audio::TEXT_PROFILE||job["acquisitionProfile"]==cached_audio::SPEECH_PROFILE{text_download_wanted(d,job,post)}
    else{visual_wanted(d,post)}
}
fn job_visual_wanted(d:&Value,job:&Value)->bool{
    let p=&job["result"]["visualProgress"];
    rows(d,"posts").iter().find(|post|post["id"]==p["sourcePostId"]||post["id"]==job["refId"])
        .is_none_or(|post|job_processing_wanted(d,job,post))
}
fn covered(d: &Value, job: &Value, at: &str) -> super::ApiResult<bool> {
    if let Some(pin)=job.get("videoSpeechAssetPin"){
        return super::media_speech_assets::ready(d,pin,at).map_err(|e|super::conflict(&e));
    }
    if job["legacyMissingStages"].is_object(){
        let post=super::row(d,"posts",text(&job["legacyMissingStages"],"postId"))?;
        return has_required_media(d,post,at);
    }
    covered_indexed(job,&QueueIndex::new(d),&super::knowledge::TranscriptLookup::new(d,at).map_err(|e|super::bad(&e))?)
}
fn covered_indexed(job:&Value,index:&QueueIndex,transcripts:&super::knowledge::TranscriptLookup)->super::ApiResult<bool>{
    for post in index.posts(text(job,"groupKey")){
        let ready=if job["acquisitionProfile"]==cached_audio::TEXT_PROFILE{
            let state=transcripts.strict_media_evidence(post).map_err(super::bad)?;
            state["audioReady"]==true&&state["screenTextReady"]==true
        }else if job["acquisitionProfile"]==cached_audio::SPEECH_PROFILE{
            transcripts.strict_media_evidence(post).map_err(super::bad)?["audioReady"]==true
        }else{transcripts.ready(post).map_err(|e|super::bad(&e))?};
        if ready{return Ok(true);}
    }
    Ok(false)
}
// A failed, empty visual checkpoint belongs to its attempted source. A separate
// open source in the title group can have its own job without rewriting that
// attempt or treating the two provider identities as the same video.
fn held_before_review_of_other_source(d:&Value,job:&Value,post:&Value)->bool{
    let progress=&job["result"]["visualProgress"];
    let source=account_scope(d).ok().and_then(|scope|super::knowledge::media_source_key(post,scope))
        .filter(|key|!key.starts_with("post:"));
    job["status"]=="failed" && progress["schemaVersion"]==2 && progress["phase"]=="held"
        && progress["nextSelectionIndex"]==0 && progress["completedSelectedFrames"]==0
        && progress["latestReceipt"].is_null() && progress["finalEvidence"].is_null()
        && open_post(d,post) && !attempted(d,post) && source.is_some()
        && !rows(job,"sourceAttempts").is_empty()
        && rows(job,"sourceAttempts").iter().all(|a|!text(a,"sourceKey").is_empty()
            && !text(a,"sourceKey").starts_with("post:")
            && a["sourceKey"]!=source.as_ref().unwrap().as_str())
}
fn enqueue(d: &mut Value, post: &Value, at: &str, manual: bool) -> super::ApiResult<Value> {
    let index=QueueIndex::new(d);
    let transcripts=super::knowledge::TranscriptLookup::new(d,at).map_err(|e|super::bad(&e))?;
    enqueue_indexed(d,post,at,manual,&index,&transcripts)
}
fn enqueue_indexed(d:&mut Value,post:&Value,at:&str,manual:bool,index:&QueueIndex,transcripts:&super::knowledge::TranscriptLookup)->super::ApiResult<Value>{
    enqueue_indexed_selected(d,post,at,manual,index,transcripts,None)
}
fn enqueue_selected(d:&mut Value,post:&Value,at:&str,owner:bool)->super::ApiResult<Value>{
    let index=QueueIndex::new(d);
    let transcripts=super::knowledge::TranscriptLookup::new(d,at).map_err(|e|super::bad(&e))?;
    let result=enqueue_indexed_selected(d,post,at,true,&index,&transcripts,Some(owner))?;
    if owner{if let Some(id)=result["jobId"].as_str(){
        let pin=json!({"version":1,"postId":post["id"],"postKey":post["postKey"],"account":account_scope(d)?,
            "connectorBinding":super::active_binding(d)?.to_json(),"sourceVersion":super::media_fullframes::source_version(post,account_scope(d)?)});
        let job=super::row(d,"jobs",id)?;
        if !current_job(job)||!job_binding_matches(d,job)||job["refId"]!=post["id"]{
            return Err(super::conflict("Selected acquisition is owned by another source"));
        }
        if !job["manualAcquisition"].is_null()&&job["manualAcquisition"]!=pin{
            return Err(super::conflict("Selected manual acquisition source changed"));
        }
        super::row_mut(d,"jobs",id)?["manualAcquisition"]=pin;
    }}
    Ok(result)
}
// Explicit new read allocation after terminal opaque history is reconciled.
// This path keeps every old record intact and never declares historical audio
// or visual coverage. Its new attempt uses the normal unique scratch directory.
fn admit_reconciled_acquisition(d:&mut Value,post_id:&str,receipt_id:&str,actor_id:&str,at:&str)->super::ApiResult<Value>{
    let post=super::row(d,"posts",post_id)?.clone();
    if !allowed(d)||!video(&post)||!open_post(d,&post)||!visual_wanted(d,&post)
        || !post["connectorBinding"].is_null()
            && post["connectorBinding"]!=super::active_binding(d)?.to_json()
        ||!super::knowledge::in_account(&post,account_scope(d)?) {
        return Err(super::conflict("Selected media source is not open for visual acquisition"));
    }
    if rows(d,"jobs").iter().any(|j|matches!(text(j,"kind"),"media"|"media_audio")
        && matches!(text(j,"status"),"running"|"unknown"|"dispatching")) {
        return Err(super::conflict("Media process ownership is active or unresolved"));
    }
    let (source,scope)=reconciled_scope(d,&post)?;
    if source.starts_with("post:") {
        return Err(super::conflict("Canonical media source identity unavailable for reacquisition"));
    }
    if rows(d,"jobs").iter().any(|j|j["reconciledAcquisition"]["receiptId"]==receipt_id
        && (j["reconciledAcquisition"]["stableScope"]!=scope
            || j["reconciledAcquisition"]["postId"]!=post_id
            || j["reconciledAcquisition"]["authorizedBy"]!=actor_id)) {
        return Err(super::conflict("Reconciliation receipt already belongs to another allocation"));
    }
    if let Some(job)=rows(d,"jobs").iter().find(|j|j["reconciledAcquisition"]["stableScope"]==scope) {
        if job["reconciledAcquisition"]["receiptId"]==receipt_id && job["reconciledAcquisition"]["postId"]==post_id
            && job["reconciledAcquisition"]["authorizedBy"]==actor_id {
            return Ok(json!({"jobId":job["id"],"status":job["status"],"deduplicated":true}));
        }
        return Err(super::conflict("Media source already has a reconciled acquisition allocation"));
    }
    let (lineage,cutoff)=reconciled_lineage(d,&post,None)?;
    let lookup=super::knowledge::TranscriptLookup::new(d,at).map_err(|e|super::bad(&e))?;
    if policy_ready(d,&post,&lookup)? {return Err(super::conflict("Media source already has complete evidence"));}
    let index=QueueIndex::new(d);
    let group=index.groups.get(post_id).ok_or_else(||super::conflict("Media group identity unavailable"))?;
    let account=account_scope(d)?.to_owned();let binding=super::active_binding(d)?.to_json();
    let policy=super::post_media_policy::effective(d,&post)?;
    let id=super::id();
    let pin=json!({"version":1,"receiptId":receipt_id,"authorizedBy":actor_id,"account":account,"connectorBinding":binding,
        "postId":post_id,"postKey":post["postKey"],"sourceKey":source,"stableScope":scope,
        "sourceVersion":super::media_fullframes::source_version(&post,&account),
        "materialEpoch":material_epoch(d,&post),"policySha256":super::media_fullframes::hash(&policy),
        "policySettingsSha256":reconciled_policy_settings(d,&post),
        "terminalCutoffUtc":cutoff,"priorJobs":lineage,"priorAttemptCount":"unknown_if_legacy_missing"});
    super::list_mut(d,"jobs").push(json!({"id":id,"kind":"media","purpose":PURPOSE,
        "visualContractVersion":2,"account":account,"connectorBinding":binding,"refId":post_id,
        "groupKey":group,"status":"queued","sourceAttempts":[],"fallbackAllowed":false,
        "manualRequested":true,"createdAt":at,"reconciledAcquisition":pin}));
    d["mediaQueue"]=Value::Null;
    super::audit(d,"media.reconciled_acquisition_admitted",receipt_id);
    Ok(json!({"jobId":id,"status":"queued"}))
}
pub(crate) async fn request_reconciled_acquisition(
    axum::extract::State(app):axum::extract::State<super::App>,
    axum::Extension(actor):axum::Extension<super::operator_auth::Actor>,
    axum::Json(body):axum::Json<Value>,
)->super::ApiResult<axum::Json<Value>>{
    if actor.role!="owner"{return Err(super::bad("Media reconciliation requires owner"));}
    let object=body.as_object().filter(|o|o.len()==2&&o.contains_key("postId")&&o.contains_key("receiptId"))
        .ok_or_else(||super::bad("Expected exact media reconciliation fields"))?;
    let post_id=object["postId"].as_str().filter(|s|!s.is_empty()&&s.len()<=256)
        .ok_or_else(||super::bad("Invalid selected post ID"))?;
    let receipt=object["receiptId"].as_str().filter(|s|uuid::Uuid::parse_str(s).is_ok()&&s.len()==36)
        .ok_or_else(||super::bad("Invalid reconciliation receipt ID"))?;
    super::media_processing::preflight_phase("download").map_err(|code|super::bad(&code))?;
    let guard=wait_for_retry_gate(&MEDIA_GATE,Duration::from_secs(300)).await?;
    let snapshot=app.read().await?;
    let active=app.tasks.lock().await;
    if active.keys().any(|id|rows(&snapshot,"jobs").iter().any(|j|j["id"]==id.as_str()
        && matches!(text(j,"kind"),"media"|"media_audio"))) {
        return Err(super::conflict("Media worker task is still active"));
    }
    drop(snapshot);drop(active);
    let result=app.change(|d|admit_reconciled_acquisition(d,post_id,receipt,&actor.id,&super::now())).await?;
    drop(guard);MEDIA_WAKE.notify_one();Ok(axum::Json(result))
}
fn video_count(post:&Value)->usize{
    rows(post,"attachments").iter().filter(|a|matches!(a["type"].as_str(),Some("video"|"clip"|"reel"))).count()
}
fn enqueue_speech_asset(d:&mut Value,post:&Value,at:&str,manual:bool,key:&str)->super::ApiResult<Value>{
    let assets=super::media_speech_assets::outcomes(d,post,at).map_err(|e|super::conflict(&e))?;
    // Reuse the exact open or uncertain intent before scheduling a sibling.
    // Failed/UNKNOWN attempts remain visible and are never silently replaced.
    for asset in assets.iter().filter(|a|a["speech"].is_null()&&a["status"]!="failed"){
        if let Some(job)=rows(d,"jobs").iter().find(|j|j["videoSpeechAssetPin"]==asset["assetPin"]
            &&matches!(text(j,"status"),"queued"|"running"|"paused"|"interrupted"|"unknown")){
            return Ok(json!({"jobId":job["id"],"status":job["status"],"deduplicated":true,"assetPin":asset["assetPin"]}));
        }
    }
    let Some(pin)=super::media_speech_assets::next_unattempted(d,post,at).map_err(|e|super::conflict(&e))? else{
        return Ok(json!({"status":"held","postId":post["id"],"reason":"video_speech_assets_require_inspection","assets":assets}));
    };
    let id=super::id();
    let job=json!({"id":id,"kind":"media","purpose":PURPOSE,"visualContractVersion":2,
        "account":account_scope(d)?,"connectorBinding":super::active_binding(d)?.to_json(),
        "refId":post["id"],"groupKey":key,"status":"queued","sourceAttempts":[],"fallbackAllowed":false,
        "manualRequested":manual,"createdAt":at,"finishedAt":null,"acquisitionProfile":cached_audio::SPEECH_PROFILE,
        "videoSpeechAssetPin":pin});
    super::list_mut(d,"jobs").push(job);
    Ok(json!({"jobId":id,"status":"queued","assetPin":pin}))
}
fn enqueue_indexed_selected(d:&mut Value,post:&Value,at:&str,manual:bool,index:&QueueIndex,transcripts:&super::knowledge::TranscriptLookup,selected_owner:Option<bool>)->super::ApiResult<Value>{
    if !allowed(d) || !video(post) { return Err(super::bad("Post has no eligible account video")); }
    let key = index.groups.get(text(post,"id")).ok_or_else(|| super::bad("Media group identity unavailable"))?;
    if video_count(post)>1&&!super::media_speech_assets::all_ready(d,post,at).map_err(|e|super::conflict(&e))?{
        return enqueue_speech_asset(d,post,at,manual,key);
    }
    let text_profile=default_text_wanted(d,post);
    let requested_profile=if text_profile{json!(cached_audio::SPEECH_PROFILE)}else{Value::Null};
    let ready=if text_profile{let state=transcripts.strict_media_evidence(post).map_err(super::bad)?;
        state["audioReady"]==true
    }else if selected_owner.is_some(){policy_ready(d,post,transcripts)?}else{transcripts.ready(post).map_err(|e|super::bad(&e))?};
    if ready {
        return Ok(json!({"reused":true,"status":"completed","postId":post["id"]}));
    }
    let selected_legacy=selected_owner.is_some()&&rows(d,"jobs").iter().any(|j|opaque_source_matches(d,j,post));
    // Only an unstarted discovery intent may be replaced by the owner's exact
    // visual choice. Attempted sources, retained CAS and paid work stay owned.
    let supersedes=(visual_wanted(d,post)||text_profile).then(||rows(d,"jobs").iter().find(|j|
        unstarted_acquisition(j)&&job_binding_matches(d,j)&&j["refId"]==post["id"]
            &&j["result"]["acquisitionSupersededBy"].is_null()
            &&index.posts(text(j,"groupKey")).iter().any(|p|p["id"]==post["id"])
            &&j["acquisitionProfile"]!=requested_profile
            &&matches!(text(j,"status"),"queued"|"paused"|"cancelled")
            &&(j["status"]!="paused"||j["mediaPolicyPause"]==true))
        .map(|j|text(j,"id").to_owned())).flatten();
    if let Some(id) = rows(d, "jobs").iter().find(|j| current_job(j) && current_source_job(d,j)&&j.get("videoSpeechAssetPin").is_none()
        &&supersedes.as_deref()!=Some(text(j,"id"))
        // A selected request owns one exact post, not a title-family peer.
        // Background discovery retains its existing group-level deduplication.
        && (selected_owner.is_none() || j["refId"]==post["id"])
        && index.posts(text(j,"groupKey")).iter().any(|candidate|candidate["id"]==post["id"])
        && !held_before_review_of_other_source(d,j,post)
        && (!selected_legacy || j["refId"]==post["id"]
            && !unstarted_opaque_hold(j)))
        .map(|j|text(j,"id").to_owned()) {
        let job = super::row_mut(d, "jobs", &id)?;
        if selected_owner.is_some(){
            let progress=&job["result"]["visualProgress"];
            if progress["schemaVersion"]==2
                && (progress["sourcePostId"]!=post["id"] || progress["sourcePostKey"]!=post["postKey"]){
                return Err(super::conflict("Selected acquisition checkpoint is owned by another source"));
            }
            if job["acquisitionProfile"]!=requested_profile{
                // Only the existing zero-work supersession above may replace a
                // profile. Paid, attempted and uncertain work remains owned.
                return Err(super::conflict("Selected acquisition profile differs; inspect retained work"));
            }
        }
        if manual { job["manualRequested"] = json!(true); }
        return Ok(json!({"jobId":job["id"],"status":job["status"],"deduplicated":true}));
    }
    if !visual_wanted(d,post)&&!text_profile{
        return Ok(json!({"status":if policy_ready(d,post,transcripts)?{"completed"}else{"held"},
            "postId":post["id"],"mode":"full_audio_only","cachedAudioRequestRequired":true}));
    }
    let legacy_upgrade=if selected_legacy{legacy_upgrade_pin(d,post,selected_owner==Some(true))?}else{None};
    let rollover_from=rows(d,"jobs").iter().find(|j|current_job(j)&&current_source_job(d,j)
        && text(j,"groupKey")==key.as_str() && held_before_review_of_other_source(d,j,post))
        .map(|j|text(j,"id").to_owned());
    let id = super::id();
    let mut job = json!({"id":id,"kind":"media","purpose":PURPOSE,"visualContractVersion":2,"account":account_scope(d)?,"connectorBinding":super::active_binding(d)?.to_json(),"refId":post["id"],"groupKey":key,"status":"queued","sourceAttempts":[],"fallbackAllowed":true,"manualRequested":manual,"createdAt":at,"finishedAt":null});
    if text_profile{job["acquisitionProfile"]=requested_profile;}
    if let Some(pin)=legacy_upgrade {job["legacyMissingStages"]=pin;}
    if job["legacyMissingStages"].is_null(){if let Some(from)=rollover_from {
        job["sourceRollover"]=json!({"fromJobId":from,"postId":post["id"],
            "sourceKey":super::knowledge::media_source_key(post,account_scope(d)?),
            "sourceVersion":super::media_fullframes::source_version(post,account_scope(d)?),
            "connectorBinding":super::active_binding(d)?.to_json()});
    }
    }
    // Before append, only the proposed identity may be absent from the ledger.
    // Source claims and workers use the persisted validator exclusively.
    let available=if job["legacyMissingStages"].is_object(){
        legacy_upgrade_proposed(d,&job,post)&&!attempted_except_legacy(d,post,Some(&job["legacyMissingStages"]))
    }else if selected_owner==Some(true){
        // enqueue_selected will pin this exact source after admission. A free
        // title peer cannot make an already-owned selected source available.
        !attempted(d,post)&&job_processing_wanted(d,&job,post)
    }else{next_source_indexed(d,&job,index).is_some()};
    if !available {
        if supersedes.is_some(){return Err(super::conflict("Media acquisition source is already owned; inspect retained work"));}
        job["status"] = json!("failed");
        job["finishedAt"] = json!(at);
        job["error"] = json!("All known media sources have already been attempted");
    }
    if let Some(old)=supersedes.as_deref(){
        let policy=super::post_media_policy::effective(d,post)?;
        job["acquisitionSupersedes"]=json!({"jobId":old,"sourceVersion":policy["sourceVersion"],"policySha256":policy["policySha256"]});
    }
    let status = job["status"].clone();
    super::list_mut(d, "jobs").push(job);
    if let Some(old)=supersedes{
        let prior=super::row_mut(d,"jobs",&old)?;
        prior["status"]=json!("cancelled");prior["finishedAt"]=json!(at);
        prior["result"]["acquisitionSupersededBy"]=json!(id);
    }
    Ok(json!({"jobId":id,"status":status}))
}

/// Idempotent reconciliation; call under App.change. Does not launch processes.
pub(crate) fn reconcile(d: &mut Value, at: &str) -> super::ApiResult<()> {
    reconcile_scoped(d,at,open_comments_only())
}
fn reconcile_scoped(d: &mut Value, at: &str, only_open:bool) -> super::ApiResult<()> {
    reconcile_with_cutoff(d,at,only_open,&comment_cutoff()?)
}
fn reconcile_with_cutoff(d:&mut Value,at:&str,only_open:bool,cutoff:&CommentCutoff)->super::ApiResult<()> {
    let only_open=only_open||cutoff.is_some();
    if !allowed(d) { return Ok(()); }
    if d["mediaQueue"]["inputDigest"] == input_digest_with_cutoff(d,at,only_open,cutoff) { return Ok(()); }
    // New intents must first persist as queued/failed. The scoped storage
    // contract cannot admit an appended row already completed or paused.
    let persisted_count=rows(d,"jobs").len();
    let index=QueueIndex::with_cutoff(d,cutoff);
    let transcripts=super::knowledge::TranscriptLookup::new(d,at).map_err(|e|super::bad(&e))?;
    for key in &index.open_groups {
        if rows(d,"jobs").iter().any(|j|j["reconciledAcquisition"].is_object()
            && j["groupKey"]==key.as_str()
            && matches!(text(j,"status"),"queued"|"running"|"paused")) {continue;}
        let open_posts:Vec<_>=index.posts(key).iter().filter(|post|open_post_with_cutoff(d,post,cutoff)).cloned().collect();
        for post in &open_posts {enqueue_indexed(d,post,at,false,&index,&transcripts)?;}
    }
    let jobs: Vec<_> = rows(d, "jobs").iter().take(persisted_count).filter(|j| current_job(j) && job_binding_matches(d,j)
        && matches!(text(j, "status"), "queued" | "failed" | "interrupted" | "paused")).cloned().collect();
    for mut job in jobs {
        if !job_visual_wanted(d,&job){
            // Policy changes suspend visual work; an impossible queued job must
            // not look runnable. Preserve every frame and the source ledger.
            if matches!(text(&job,"status"),"queued"|"interrupted")
                && job["result"]["visualProgress"]["leaseId"].is_null() {
                let stored=super::row_mut(d,"jobs",text(&job,"id"))?;
                stored["status"]=json!("paused");
                stored["mediaPolicyPause"]=json!(true);
            }
            continue;
        }
        if job["status"]=="paused" && job["mediaPolicyPause"]==true
            && job["result"]["visualProgress"]["leaseId"].is_null() {
            let stored=super::row_mut(d,"jobs",text(&job,"id"))?;
            stored.as_object_mut().unwrap().remove("mediaPolicyPause");
            if stored["mediaScopePause"]!=true {stored["status"]=json!("queued");}
            job=stored.clone();
        }
        let needed=wanted_scoped_with_cutoff(d,&job,&index,only_open,cutoff);
        if job["result"]["visualProgress"]["schemaVersion"]==2 {
            let stored=super::row_mut(d,"jobs",text(&job,"id"))?;
            if only_open && !needed && matches!(text(&job,"status"),"queued"|"interrupted") {
                stored["status"]=json!("paused");
                stored["mediaScopePause"]=json!(true);
            } else if needed && job["mediaScopePause"]==true && job["status"]=="paused" {
                stored["status"]=json!("queued");
                stored.as_object_mut().unwrap().remove("mediaScopePause");
            }
            continue;
        }
        if job["reconciledAcquisition"].is_object() {
            let current=index.posts(text(&job,"groupKey")).iter().any(|post|
                post["id"]==job["reconciledAcquisition"]["postId"]
                    && reconciled_current(d,&job,post));
            if !current || !rows(&job,"sourceAttempts").is_empty() {
                let stored=super::row_mut(d,"jobs",text(&job,"id"))?;
                stored["status"]=json!("failed");stored["finishedAt"]=json!(at);
                stored["error"]=json!("media_reconciled_acquisition_reconciliation_required");
            } else if only_open && !needed {
                super::row_mut(d,"jobs",text(&job,"id"))?["status"]=json!("paused");
            } else if needed && job["status"]=="paused" {
                super::row_mut(d,"jobs",text(&job,"id"))?["status"]=json!("queued");
            }
            continue;
        }
        let done = if job["legacyMissingStages"].is_object()||job.get("videoSpeechAssetPin").is_some(){covered(d,&job,at)?}else{covered_indexed(&job,&index,&transcripts)?};
        let first_asset_attempt=job.get("videoSpeechAssetPin").is_some()
            &&rows(&job,"sourceAttempts").is_empty()&&job["result"]["visualProgress"].is_null()
            &&matches!(text(&job,"status"),"queued"|"paused");
        let retry = (first_asset_attempt||job["fallbackAllowed"] == true) && next_source_indexed(d,&job,&index).is_some();
        let stored = super::row_mut(d, "jobs", text(&job, "id"))?;
        if done {
            stored["status"] = json!("completed");
            stored["finishedAt"] = json!(at);
            let retained_vision=stored["result"]["visualProgress"].clone();
            stored["result"] = json!({"reused":true});
            if !retained_vision["visionStage"].is_null(){stored["result"]["visualProgress"]=retained_vision;}
            stored.as_object_mut().unwrap().remove("error");
        } else if retry && needed {
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
    // Keep discovery dirty until the next transaction classifies these now
    // persisted intents; otherwise an equal digest could hide their transition.
    if rows(d,"jobs").len()==persisted_count {
        d["mediaQueue"] = json!({"inputDigest":input_digest_with_cutoff(d,at,only_open,cutoff)});
    }
    Ok(())
}

fn input_digest(d: &Value, at: &str, only_open:bool) -> String {
    input_digest_with_cutoff(d,at,only_open,&comment_cutoff().unwrap_or(None))
}
fn input_digest_with_cutoff(d:&Value,at:&str,only_open:bool,cutoff:&CommentCutoff)->String {
    // The time bucket also rechecks evidence validity windows. Unchanged queues
    // avoid repeated full knowledge validation and title grouping every tick.
    let minute = chrono::DateTime::parse_from_rfc3339(at).map(|v|v.timestamp()/60).unwrap_or(0);
    let items: Vec<_> = rows(d,"items").iter().map(|i|json!([i["id"],i["objectId"],i["itemId"],i["connectorBinding"],i["postId"],i["postKey"],i["providerStatus"],i["workflow"],i["createdAt"]])).collect();
    let versions: Vec<_> = rows(d,"knowledge_versions").iter().map(|v|json!([v["id"],v["hash"]])).collect();
    let jobs: Vec<_> = rows(d,"jobs").iter().filter(|j|j["kind"]=="media").collect();
    let input = json!([d["account"],d["connectorBinding"],d["posts"],items,d["knowledge_entries"],versions,jobs,minute,super::media_visual::VERSION,only_open,cutoff.as_ref().map(|value|value.to_rfc3339()),d["settings"]["postMediaPolicies"],d["settings"]["mediaAudioEquivalences"],d["settings"]["mediaPolicyDefaults"]]);
    format!("{:x}",Sha256::digest(input.to_string().as_bytes()))
}

/// Run after generic startup recovery. Interrupted dispatches stay consumed
/// unless a local owner explicitly records a bounded process-cessation permit.
pub(crate) fn recover(d: &mut Value, at: &str) -> super::ApiResult<()> {
    let unspent_reconciled:std::collections::HashSet<String>=rows(d,"jobs").iter()
        .filter(|job|job["reconciledAcquisition"].is_object()
            && matches!(text(job,"status"),"queued"|"interrupted")
            && rows(job,"sourceAttempts").is_empty()
            && job["result"]["visualProgress"].is_null())
        .filter(|job|rows(d,"posts").iter().find(|post|post["id"]==job["reconciledAcquisition"]["postId"])
            .is_some_and(|post|reconciled_current(d,job,post)))
        .map(|job|text(job,"id").to_owned()).collect();
    // Validate every recovery increment before mutating any job. The underlying
    // checkpoint recovery predates checked epoch arithmetic.
    for job in rows(d,"jobs").iter().filter(|j|current_job(j)&&matches!(text(j,"status"),"running"|"interrupted")&&j["result"]["visualProgress"]["schemaVersion"]==2){
        if job["result"]["visualProgress"]["leaseEpoch"].as_u64().and_then(|epoch|epoch.checked_add(1)).is_none(){return Err(super::conflict("media_recovery_epoch_invalid_or_exhausted"));}
    }
    for job in super::list_mut(d, "jobs") {
        if !current_job(job) { continue; }
        if unspent_reconciled.contains(text(job,"id")) {
            job["status"]=json!("queued");job["finishedAt"]=Value::Null;
            job.as_object_mut().unwrap().remove("error");
            continue;
        }
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
    take_source_scoped(d,job_id,at,open_comments_only())
}
fn take_source_scoped(d: &mut Value, job_id: &str, at: &str, only_open:bool) -> super::ApiResult<Option<Value>> {
    let job = super::row(d, "jobs", job_id)?.clone();
    if !job_binding_matches(d,&job) || job["status"] != "running" { return Ok(None); }
    // A twin's result may arrive between the previous failure and fallback
    // reservation. Reuse it under the same App.change transaction as claiming.
    if covered(d, &job, at)? { return Ok(None); }
    let selected_manual=rows(d,"posts").iter().find(|p|p["id"]==job["refId"])
        .is_some_and(|post|manual_source_requested(d,&job,post));
    if (only_open || job["manualRequested"] != true) && !selected_manual
        && !candidates(d,text(&job,"groupKey")).iter().any(|p|open_post(d,p)) { return Ok(None); }
    let Some(post) = next_source(d, &job) else { return Ok(None); };
    if job["legacyMissingStages"].is_object()&&only_open&&!open_post(d,&post){return Ok(None);}
    if !job_processing_wanted(d,&job,&post){return Ok(None);}
    let history=source_attempts(d,&post);
    let ordinal=history.len()+1;
    let retry_of=history.last().map(|(job,index,attempt)|json!({"jobId":job["id"],"attemptIndex":index,"permitId":attempt["retryPermit"]["id"]}));
    let source_key=account_scope(d).ok().and_then(|scope|super::knowledge::media_source_key(&post,scope));
    let source_version=super::media_fullframes::source_version(&post,account_scope(d)?);
    let asset_pin=job.get("videoSpeechAssetPin").cloned();
    let stored = super::row_mut(d, "jobs", job_id)?;
    stored["refId"] = post["id"].clone();
    super::list_mut(stored, "sourceAttempts").push(json!({"id":super::id(),"postId":post["id"],"postKey":post["postKey"],"sourceKey":source_key,"sourceVersion":source_version,"channel":post["channel"],"status":"running","startedAt":at,"attemptNumber":ordinal,"retryOf":retry_of}));
    if let Some(pin)=asset_pin{
        let attempt=super::list_mut(stored,"sourceAttempts").last_mut().expect("source attempt just appended");
        attempt["assetPin"]=pin;attempt["attemptNumber"]=json!(1);attempt["retryOf"]=Value::Null;
    }
    Ok(Some(post))
}
// Seven completion-oriented leases, then one oldest-waiter lease. Waiting
// jobs get a fair turn within 8*N successful claims for N existing waiters;
// normally timestamped new arrivals cannot overtake an older fair waiter.
const FAIR_MEDIA_CLAIM_INTERVAL:u64=8;
fn scheduler_turn(job:&Value)->u64 {
    job["result"]["mediaSchedulerTurn"].as_u64().unwrap_or(0)
        .max(job["result"]["visualProgress"]["schedulerTurn"].as_u64().unwrap_or(0))
}
fn next_scheduler_turn(d:&Value)->super::ApiResult<u64> {
    rows(d,"jobs").iter().filter(|j|current_job(j)).map(scheduler_turn).max().unwrap_or(0)
        .checked_add(1).ok_or_else(||super::bad("media_scheduler_turn_exhausted"))
}
fn waiting_since(job:&Value)->i64 {
    ["startedAt","createdAt"].iter().find_map(|key|chrono::DateTime::parse_from_rfc3339(text(job,key)).ok())
        .map(|date|date.timestamp_millis()).unwrap_or(i64::MIN)
}
fn completion_priority(d:&Value,job:&Value)->(u8,bool,u64) {
    let progress=&job["result"]["visualProgress"];
    let phase=match text(progress,"phase") {"finalize"=>0,"scan"=>1,"select"=>2,"inventory"=>3,"download"|""=>4,_=>5};
    if phase==0{return (0,false,0);}
    // Downloaded-source duration is authoritative for this estimate; provider
    // post duration is only a fallback before download. No CAS I/O under writer.
    let post=rows(d,"posts").iter().find(|p|p["id"]==progress["sourcePostId"]||p["id"]==job["refId"]);
    let duration=progress["sourceIdentity"]["durationMs"].as_u64().filter(|n|*n>0).or_else(||post.and_then(|p|
        p["durationMs"].as_u64().filter(|n|*n>0).or_else(||p["durationSeconds"].as_f64()
            .filter(|n|n.is_finite()&&*n>0.0&&*n<(u64::MAX/1000) as f64).map(|n|(n*1000.0).ceil() as u64))));
    // A priority hint only, never a selected-frame total or completion proof.
    let remaining=duration.and_then(|ms|ms.div_ceil(1000).checked_mul(2))
        .map(|total|total.saturating_sub(progress["completedSelectedFrames"].as_u64().unwrap_or(0)));
    (phase,remaining.is_none(),remaining.unwrap_or(u64::MAX))
}
fn scheduled_candidates(d:&Value,turn:u64,only_open:bool)->Vec<String> {
    let index=only_open.then(||QueueIndex::new(d));
    let has_binding=account_scope(d).is_ok();
    let mut jobs:Vec<_>=rows(d,"jobs").iter().filter(|j|current_job(j)&&(!has_binding||job_binding_matches(d,j))&&j["status"]=="queued"
        && job_visual_wanted(d,j)
        && index.as_ref().is_none_or(|index|wanted_scoped(d,j,index,true))).collect();
    let fair=turn%FAIR_MEDIA_CLAIM_INTERVAL==0;
    jobs.sort_by(|a,b| {
        let priority=if fair{std::cmp::Ordering::Equal}else{completion_priority(d,a).cmp(&completion_priority(d,b))};
        priority.then_with(||waiting_since(a).cmp(&waiting_since(b))).then_with(||text(a,"id").cmp(text(b,"id")))
    });
    jobs.into_iter().map(|j|text(j,"id").to_owned()).collect()
}
fn claim(d: &mut Value, at: &str) -> super::ApiResult<Option<(String, Value)>> {
    claim_scoped(d,at,open_comments_only())
}
fn claim_scoped(d: &mut Value, at: &str, only_open:bool) -> super::ApiResult<Option<(String, Value)>> {
    claim_selected(d,at,only_open,None)
}
fn claim_resumed(d:&mut Value,at:&str,target:&ResumeTarget)->super::ApiResult<Option<(String,Value)>> {
    claim_selected(d,at,open_comments_only(),Some(target))
}
fn claim_selected(d:&mut Value,at:&str,only_open:bool,target:Option<&ResumeTarget>)->super::ApiResult<Option<(String,Value)>> {
    claim_selected_ready(d,at,only_open,target,&|_|Ok(()))
}
fn claim_selected_ready(d:&mut Value,at:&str,only_open:bool,target:Option<&ResumeTarget>,ready:&impl Fn(&str)->Result<(),String>)->super::ApiResult<Option<(String,Value)>> {
    comment_cutoff()?;
    if rows(d,"jobs").iter().any(|j|j["kind"]=="media"&&j["status"]=="running"){return Ok(None);}
    let persisted_count=rows(d,"jobs").len();
    let turn=next_scheduler_turn(d)?;
    if target.is_none() { reconcile_scoped(d, at, only_open)?; }
    let candidates=scheduled_candidates(d,turn,only_open);
    let candidates=if let Some(target)=target {
        if !candidates.iter().any(|id|id==&target.job_id)
            || !target.matches(super::row(d,"jobs",&target.job_id)?) {return Ok(None);}
        vec![target.job_id.clone()]
    }else{candidates};
    for id in candidates {
        // A twin can finish while this durable visual checkpoint waits. Check
        // current catalog coverage before leasing more frames or audio work.
        let prior=super::row(d,"jobs",&id)?.clone();
        if prior["legacyMissingStages"].is_object(){
            let post=rows(d,"posts").iter().find(|p|p["id"]==prior["legacyMissingStages"]["postId"]);
            if !post.is_some_and(|p|legacy_upgrade_current(d,&prior,p)){
                let job=super::row_mut(d,"jobs",&id)?;job["status"]=json!("failed");
                job["finishedAt"]=json!(at);job["error"]=json!("media_legacy_upgrade_binding_changed");
                continue;
            }
        }
        if prior["reconciledAcquisition"].is_object(){
            let post=rows(d,"posts").iter().find(|p|p["id"]==prior["reconciledAcquisition"]["postId"]);
            if !post.is_some_and(|p|reconciled_current(d,&prior,p)){
                let job=super::row_mut(d,"jobs",&id)?;job["status"]=json!("failed");
                job["finishedAt"]=json!(at);job["error"]=json!("media_reconciled_acquisition_binding_changed");
                continue;
            }
            if !rows(&prior,"sourceAttempts").is_empty()
                && prior["result"]["visualProgress"]["phase"]=="download" {
                let job=super::row_mut(d,"jobs",&id)?;job["status"]=json!("failed");
                job["finishedAt"]=json!(at);
                job["error"]=json!("media_reconciled_acquisition_interrupted_unknown");
                let progress=&mut job["result"]["visualProgress"];
                if progress["schemaVersion"]==2 {
                    progress["resumePhase"]=progress["phase"].clone();
                    progress["phase"]=json!("held");progress["leaseId"]=Value::Null;
                }
                continue;
            }
            if has_required_media(d,post.expect("reconciled current post checked"),at)? {
                let job=super::row_mut(d,"jobs",&id)?;job["status"]=json!("completed");
                job["finishedAt"]=json!(at);
                let retained_vision=job["result"]["visualProgress"].clone();job["result"]=json!({"reused":true});
                if !retained_vision["visionStage"].is_null(){job["result"]["visualProgress"]=retained_vision;}
                job.as_object_mut().unwrap().remove("error");
                continue;
            }
        }
        if covered(d,&prior,at)? {
            // A direct claim can also discover a new ready sibling intent.
            // Persist it first; never allocate a redundant source attempt or
            // complete an appended row inside the same scoped transaction.
            if !rows(d,"jobs").iter().take(persisted_count).any(|j|j["id"]==id){continue;}
            let job=super::row_mut(d,"jobs",&id)?;
            job["status"]=json!("completed");job["finishedAt"]=json!(at);
            job["result"]["reused"]=json!(true);
            job.as_object_mut().unwrap().remove("error");
            continue;
        }
        let existing=super::row(d,"jobs",&id)?["result"]["visualProgress"].clone();
        let account=account_scope(d)?.to_owned();let binding=super::active_binding(d)?;
        let resumed=if existing["schemaVersion"]==2 {
            let post=rows(d,"posts").iter().find(|p|p["id"]==existing["sourcePostId"]).cloned();
            let valid=post.is_some()&&verified_checkpoint_binding(d,&prior).is_ok()
                &&existing["leaseId"].is_null()&&matches!(text(&existing,"phase"),"download"|"inventory"|"select"|"scan"|"finalize");
            if !valid {
                let job=super::row_mut(d,"jobs",&id)?;job["status"]=json!("failed");job["finishedAt"]=json!(at);job["error"]=json!("media_resume_source_or_material_changed");
                let p=&mut job["result"]["visualProgress"];p["resumePhase"]=p["phase"].clone();p["phase"]=json!("held");p["leaseId"]=Value::Null;
                continue;
            }
            post
        }else{None};
        let phase=if existing["schemaVersion"]==2 {text(&existing,"phase")}else{"download"};
        if let Err(code)=ready(phase){
            set_worker_block(super::row_mut(d,"jobs",&id)?,phase,Some(&code));
            continue;
        }
        let stored=super::row_mut(d,"jobs",&id)?;
        set_worker_block(stored,phase,None);
        stored["status"]=json!("running");stored["startedAt"]=json!(at);
        let post=if existing["schemaVersion"]==2{resumed}else{take_source_scoped(d,&id,at,only_open)?};
        let Some(post)=post else {
            let done=covered(d,super::row(d,"jobs",&id)?,at)?;let job=super::row_mut(d,"jobs",&id)?;
            job["status"]=json!(if done{"completed"}else{"paused"});job["finishedAt"]=if done{json!(at)}else{Value::Null};
            if done{
                let retained_vision=job["result"]["visualProgress"].clone();job["result"]=json!({"reused":true});
                if !retained_vision["visionStage"].is_null(){job["result"]["visualProgress"]=retained_vision;}
                job.as_object_mut().unwrap().remove("error");
            }
            continue;
        };
        let mut progress=if existing["schemaVersion"]==2{existing}else{let mut p=super::media_fullframes::initial(&account,&binding.to_json(),&post,at);p["materialEpoch"]=json!(material_epoch(d,&post));p};
        if let Some(pin)=prior.get("videoSpeechAssetPin"){
            if progress.get("assetPin").is_some_and(|saved|saved!=pin){return Err(super::conflict("Media speech progress retargeted"));}
            progress["assetPin"]=pin.clone();
            super::media_speech_assets::require_progress(d,&progress).map_err(|e|super::conflict(&e))?;
        }
        super::media_fullframes::claim(&mut progress,&super::id()).map_err(|e|super::bad(&e))?;
        // Failed download clears progress; finalization retains progress. Both
        // copies preserve the scheduling watermark across those existing paths.
        progress["schedulerTurn"]=json!(turn);
        // A new source/lease cannot erase an earlier possibly paid stage.
        if !prior["result"]["visualProgress"]["visionStage"].is_null(){
            progress["visionStage"]=prior["result"]["visualProgress"]["visionStage"].clone();
        }
        let stored=super::row_mut(d,"jobs",&id)?;
        if stored["result"]["visualProgress"]["schemaVersion"]==2 {stored["connectorBinding"]=binding.to_json();}
        stored["result"]=json!({"visualProgress":progress,"mediaSchedulerTurn":turn});
        return Ok(Some((id,post)));
    }
    if target.is_none()&&rows(d,"jobs").len()==persisted_count{d["mediaQueue"]["inputDigest"]=json!(input_digest(d,at,only_open));}
    Ok(None)
}
fn set_worker_block(job:&mut Value,phase:&str,code:Option<&str>){
    match code {
        Some(code)=>{job["workerBlock"]=json!({"stage":if matches!(phase,"download"|"inventory"|"select"|"scan"|"finalize"|"audio"){phase}else{"unknown"},"code":if worker_block_code(code){code}else{"media_runtime_not_ready"}});},
        None=>{job.as_object_mut().unwrap().remove("workerBlock");},
    }
}
pub(crate) const WORKER_BLOCK_CODES:&[&str]=&[
    "media_runtime_not_ready",
    "media_phase_not_claimable",
    "media_config_conflicting_ytdlp",
    "media_config_missing_ytdlp",
    "media_config_missing_scratch",
    "media_config_invalid_scratch",
    "media_config_invalid_tessdata",
    "visual_evidence_dir_missing",
    "visual_evidence_dir_invalid",
    "visual_local_config_missing",
    "visual_backend_not_configured",
    "visual_private_dir_missing",
    "visual_private_dir_invalid",
    "gpu_gate_invalid",
    "gpu_gate_unavailable",
    "gpu_gate_linked",
    "gpu_gate_identity_missing",
    "gpu_gate_identity_invalid",
    "gpu_gate_identity_mismatch",
    "gpu_gate_linked_or_identity_unknown",
    "gpu_gate_platform_unsupported",
    "media_config_missing_COMMUNITYHERO_MEDIA_YTDLP",
    "media_config_missing_COMMUNITYHERO_MEDIA_YTDLP_PYTHON",
    "media_config_missing_COMMUNITYHERO_MEDIA_YTDLP_NODE",
    "media_config_missing_COMMUNITYHERO_MEDIA_FFMPEG",
    "media_config_missing_COMMUNITYHERO_MEDIA_FFPROBE",
    "media_config_missing_COMMUNITYHERO_MEDIA_WHISPER_CLI",
    "media_config_missing_COMMUNITYHERO_MEDIA_WHISPER_MODEL",
    "media_config_missing_COMMUNITYHERO_MEDIA_TESSERACT",
    "media_config_invalid_COMMUNITYHERO_MEDIA_YTDLP",
    "media_config_invalid_COMMUNITYHERO_MEDIA_YTDLP_PYTHON",
    "media_config_invalid_COMMUNITYHERO_MEDIA_YTDLP_NODE",
    "media_config_invalid_COMMUNITYHERO_MEDIA_FFMPEG",
    "media_config_invalid_COMMUNITYHERO_MEDIA_FFPROBE",
    "media_config_invalid_COMMUNITYHERO_MEDIA_WHISPER_CLI",
    "media_config_invalid_COMMUNITYHERO_MEDIA_WHISPER_MODEL",
    "media_config_invalid_COMMUNITYHERO_MEDIA_TESSERACT",
];
pub(crate) fn worker_block_code(code:&str)->bool{WORKER_BLOCK_CODES.contains(&code)}
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
pub(crate) fn material_epoch(d:&Value,post:&Value)->String{
    let heads:Vec<_>=rows(d,"knowledge_entries").iter().filter(|e|matches!(text(e,"kind"),"transcript"|"visual_context")&&rows(&e["scope"],"postKeys").iter().any(|k|*k==post["postKey"])).map(|e|json!([e["id"],e["currentVersionId"]])).collect();
    super::media_fullframes::hash(&json!(heads))
}
// Keep the old immutable proof rooted in the ordinary jobs backup traversal.
// A policy transition never edits a signed receipt or consumes another download.
fn reselect_progress(d:&mut Value,id:&str,expected:&Value,at:&str)->super::ApiResult<Value>{
    let original=super::row(d,"jobs",id)?.clone();
    if original["kind"]!="media" || original["status"]!="running"
        || original["result"]["visualProgress"]!=*expected || expected["phase"]!="scan"
        || expected["leaseId"].as_str().is_none_or(str::is_empty) {
        return Err(super::conflict("Media policy transition lease changed"));
    }
    let post=super::row(d,"posts",text(expected,"sourcePostId"))?;
    if super::active_binding(d)?.to_json()!=expected["connectorBinding"]
        || account_scope(d)?!=text(expected,"account")
        || super::media_fullframes::source_version(post,account_scope(d)?)!=expected["sourceVersion"]
        || material_epoch(d,post)!=expected["materialEpoch"] {
        return Err(super::conflict("Media policy transition source changed"));
    }
    let history_id=super::id();
    let mut history=original.clone();
    history["id"]=json!(history_id);history["kind"]=json!("media_policy_history");
    history["purpose"]=json!("selection_policy_history");history["status"]=json!("completed");
    history["createdAt"]=json!(at);history["finishedAt"]=json!(at);
    history["originJobId"]=json!(id);
    history["replacementPolicySha256"]=super::media_frame_selection::policy()["sha256"].clone();
    let mut next=expected.clone();
    next["phase"]=json!("select");
    for key in ["selectionDescriptor","latestReceipt","finalEvidence"] {next[key]=Value::Null;}
    next["nextSelectionIndex"]=json!(0);next["completedSelectedFrames"]=json!(0);
    super::media_fullframes::checkpoint(super::row_mut(d,"jobs",id)?,text(expected,"leaseId"),expected,next)
        .map_err(|e|super::conflict(&e))?;
    let job = super::row_mut(d,"jobs",id)?;
    if job.get("selectionPolicyHistoryIds").is_none() {
        job["selectionPolicyHistoryIds"] = json!([]);
    }
    job["selectionPolicyHistoryIds"].as_array_mut()
        .ok_or_else(||super::conflict("invalid selection policy history"))?.push(json!(history_id));
    super::list_mut(d,"jobs").push(history);
    super::audit(d,"media.selection_policy_changed",id);
    Ok(json!({"resume":true,"selectionPolicyChanged":true}))
}
async fn run_full(app:&super::App,id:&str,post:Value)->super::ApiResult<Value>{
    let d=app.read().await?;let job=super::row(&d,"jobs",id)?;let progress=job["result"]["visualProgress"].clone();
    super::media_speech_assets::require_progress(&d,&progress).map_err(|e|super::conflict(&e))?;
    if progress.get("assetPin").is_some()&&job["videoSpeechAssetPin"]!=progress["assetPin"]{return Err(super::conflict("Media speech asset ownership changed"));}
    if job["status"]!="running"||progress["sourceVersion"]!=super::media_fullframes::source_version(&post,account_scope(&d)?)||progress["connectorBinding"]!=super::active_binding(&d)?.to_json(){return Err(super::conflict("Media worker binding changed"));}
    let current=super::row(&d,"posts",text(&post,"id"))?;
    if job["legacyMissingStages"].is_object()&&!legacy_upgrade_current(&d,job,current){return Err(super::conflict("Media legacy upgrade binding changed"));}
    if job["reconciledAcquisition"].is_object()&&!reconciled_current(&d,job,current){return Err(super::conflict("Media reconciled acquisition source changed"));}
    if !job_processing_wanted(&d,job,current){return Err(super::conflict("Media acquisition profile is not permitted by current post policy"));}
    if super::media_fullframes::source_version(current,account_scope(&d)?)!=progress["sourceVersion"]||material_epoch(&d,current)!=progress["materialEpoch"]{return Err(super::conflict("Media source or material version changed"));}
    if progress["phase"]=="scan" {
        let inspected=progress.clone();
        let reselect=tokio::task::spawn_blocking(move||->Result<bool,String>{
            let store=super::media_fullframes::store()?;
            let selection=super::media_fullframes::selection_descriptor(&store,&inspected)?;
            if selection["policy"]==super::media_frame_selection::policy()
                || inspected["nextSelectionIndex"]==selection["selectedCount"] {return Ok(false);}
            super::media_fullframes::descriptor(&store,&inspected)?;
            let (_,_,covered)=super::media_fullframes::reviewed(&store,&inspected["inventoryDescriptor"],&inspected["selectionDescriptor"],&inspected["latestReceipt"])?;
            if inspected["nextSelectionIndex"]!=covered || inspected["completedSelectedFrames"]!=covered {
                return Err("media_policy_transition_cursor_invalid".into());
            }
            Ok(true)
        }).await.map_err(|_|super::internal("Media policy inspection stopped"))?.map_err(|e|super::conflict(&e))?;
        if reselect {return app.change(|d|reselect_progress(d,id,&progress,&super::now())).await;}
    }
    let source=if progress["phase"]=="download"{
        let binding=super::active_binding(&d)?;
        let mut request=json!({"account":super::bridge_account(&binding)?,"postId":post["id"],"post":post});
        if let Some(pin)=progress.get("assetPin"){request["assetPin"]=pin.clone();}
        let projection=app.bridge("media_source",request).await?;
        if let Some(pin)=progress.get("assetPin"){
            super::media_speech_assets::require_projection(pin,&projection).map_err(|e|super::conflict(&e))?;
        }
        if job["reconciledAcquisition"].is_object(){
            if !reconciled_no_fallback(&projection)
                ||!reconciled_locator_matches(&post,&projection,account_scope(&d)?,
                    text(&job["reconciledAcquisition"],"sourceKey")) {
                return Err(super::conflict("Media reconciled source locator changed"));
            }
        }
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
    super::media_processing::observe_finalize_catalog_admission(app,id,&expected).await;
    app.change(|d|{
        let current=super::row(d,"posts",text(&post,"id"))?;
        if !visual_wanted(d,current){return Err(super::conflict("Visual policy changed during processing"));}
        if super::active_binding(d)?.to_json()!=progress["connectorBinding"]||super::media_fullframes::source_version(current,account_scope(d)?)!=progress["sourceVersion"]||material_epoch(d,current)!=progress["materialEpoch"]{return Err(super::conflict("Media source or material version changed"));}
        let job=super::row(d,"jobs",id)?;
        if job["legacyMissingStages"].is_object()&&!legacy_upgrade_current(d,job,current){return Err(super::conflict("Media legacy upgrade binding changed"));}
        if job["reconciledAcquisition"].is_object()&&!reconciled_current(d,job,current){return Err(super::conflict("Media reconciled acquisition source changed"));}
        if job["status"]!="running"||job["result"]["visualProgress"]!=expected{return Err(super::conflict("Media worker lease changed"));}
        if !super::media_processing::full::admit_audio_analysis(d,&expected,&result)? {
            super::merge_materials(d,&result)?;
        }
        if let Some(pin)=result.get("audioReuse") {
            super::media_processing::full::transcript_reuse::validate_pin(d,&expected,pin,&super::now()).map_err(super::conflict)?;
        }
        if !has_required_media(d,&post,&super::now())?{return Err(super::conflict("Full media evidence not admitted"));}
        // Admission marker commits with the admitted materials, never ahead of them.
        super::media_processing::mark_finalize_catalog_admitted(super::row_mut(d,"jobs",id)?);
        mark_attempt(d,id,&post,None,&super::now())?;
        Ok(json!({"processed":true,"sourcePostKey":post["postKey"]}))
    }).await
}
async fn run(app: &super::App, id: &str, mut post: Value) -> super::ApiResult<Value> {
    {let d=app.read().await?;let job=super::row(&d,"jobs",id)?;
        if !job_processing_wanted(&d,job,&post){return Err(super::conflict("Media acquisition profile is not permitted by current post policy"));}}
    if super::row(&app.read().await?,"jobs",id)?["visualContractVersion"]==2{return run_full(app,id,post).await;}
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
            super::media_processing::process(&source,app,id).await.map_err(|code|super::bad(&code))
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
    discover_pending(app).await?;
    // Retain process ownership from admission through durable finalization,
    // including cancellation and completion-record retries. Clean active-worker
    // ticks skip the writer; committed arrivals only reconcile the durable queue.
    let Ok(guard) = MEDIA_GATE.try_lock() else { return Ok(()); };
    if let Some((id,expected))=cached_audio::claim_speech_ready(app,open_comments_only()).await?{
        let worker=app.clone();let run_id=id.clone();
        app.spawn_with_completion(id,async move{cached_audio::run(&worker,&run_id,&expected).await},move||{
            drop(guard);MEDIA_WAKE.notify_one();
        });
        return Ok(());
    }
    if let Some((id,expected))=cached_audio::claim_ready(app,open_comments_only()).await?{
        let worker=app.clone();let run_id=id.clone();
        app.spawn_with_completion(id,async move{cached_audio::run(&worker,&run_id,&expected).await},move||{
            drop(guard);MEDIA_WAKE.notify_one();
        });
        return Ok(());
    }
    super::media_fullframes::refresh(app).await?;
    let ready=phase_readiness();
    let claimed = app.change_media(|d| claim_selected_ready(d,&super::now(),open_comments_only(),None,&|phase|phase_ready(&ready,phase))).await?;
    if let Some((id, post)) = claimed {
        let worker = app.clone();
        let run_id = id.clone();
        app.spawn_with_completion(id, async move { run(&worker, &run_id, post).await }, move || {
            drop(guard);
            MEDIA_WAKE.notify_one();
        });
    }
    Ok(())
}
async fn tick_resumed(app:&super::App,target:&ResumeTarget,guard:tokio::sync::MutexGuard<'static,()>)->super::ApiResult<()> {
    // A failed runtime preflight leaves the permitted job durable for a later
    // periodic pass; it does not spend a source attempt or choose another job.
    let ready=phase_readiness();
    super::media_fullframes::refresh(app).await?;
    let claimed=app.change_media(|d|claim_selected_ready(d,&super::now(),open_comments_only(),Some(target),&|phase|phase_ready(&ready,phase))).await?;
    if let Some((id,post))=claimed {
        let worker=app.clone();let run_id=id.clone();
        app.spawn_with_completion(id,async move {run(&worker,&run_id,post).await},move||{
            drop(guard);
            MEDIA_WAKE.notify_one();
        });
    }
    Ok(())
}
fn phase_readiness()->BTreeMap<&'static str,Result<(),String>>{
    ["download","inventory","select","scan","finalize"].into_iter()
        .map(|phase|(phase,super::media_processing::preflight_phase(phase))).collect()
}
fn phase_ready(ready:&BTreeMap<&'static str,Result<(),String>>,phase:&str)->Result<(),String>{
    ready.get(phase).cloned().unwrap_or_else(||Err("media_phase_not_claimable".into()))
}
pub(crate) fn selected_post_id(body:&Value)->super::ApiResult<&str>{
    let object=body.as_object().filter(|m|m.len()==1&&m.contains_key("postId"))
        .ok_or_else(||super::bad("Expected exact selected postId"))?;
    object["postId"].as_str().filter(|id|!id.is_empty()&&id.len()<=256&&id.trim()==*id)
        .ok_or_else(||super::bad("Invalid selected postId"))
}
pub(crate) async fn request(app: &super::App, post_id: &str, actor:&super::operator_auth::Actor) -> super::ApiResult<Value> {
    super::media_fullframes::refresh(app).await?;
    super::media_processing::preflight_phase("download").map_err(|code|super::bad(&code))?;
    let result = app.change(|d| {
        let post = super::row(d, "posts", post_id)?.clone();
        enqueue_selected(d, &post, &super::now(), actor.role=="owner")
    }).await?;
    tick(app).await?;
    Ok(result)
}

/// Explicit local-owner recovery only. The receipt is an operator attestation
/// after inspecting process cessation, never an automatic inference from a
/// persisted interrupted status. HTTP middleware also enforces owner + CSRF.
async fn wait_for_retry_gate<'a>(gate:&'a tokio::sync::Mutex<()>,limit:Duration)->super::ApiResult<tokio::sync::MutexGuard<'a,()>>{
    tokio::time::timeout(limit,gate.lock()).await
        .map_err(|_|super::conflict("Timed out waiting for media process ownership"))
}
/// A policy change drains the active chunk and its durable completion before
/// the source policy is committed. FIFO waiting prevents periodic try_lock
/// claims from starting another chunk ahead of the owner change.
pub(crate) async fn wait_for_policy_change()->super::ApiResult<tokio::sync::MutexGuard<'static,()>>{
    wait_for_retry_gate(&MEDIA_GATE,Duration::from_secs(300)).await
}
pub(crate) fn source_import_guard()->super::ApiResult<tokio::sync::MutexGuard<'static,()>>{
    MEDIA_GATE.try_lock().map_err(|_|super::conflict("A media process is still owned by this server"))
}
pub(crate) fn source_import_ready(){ MEDIA_WAKE.notify_one(); }
#[cfg(test)]
pub(crate) async fn imported_audio_candidate(app:&super::App)->super::ApiResult<Option<(String,Value)>>{
    cached_audio::claim_ready(app,false).await
}
pub(crate) async fn retry_interrupted(
    axum::extract::State(app):axum::extract::State<super::App>,
    axum::Extension(actor):axum::Extension<super::operator_auth::Actor>,
    axum::Json(body):axum::Json<Value>,
)->super::ApiResult<axum::Json<Value>>{
    if actor.role!="owner"{return Err(super::bad("Interrupted media retry requires owner"));}
    // Tokio's queued lock gives an owner retry its turn after the current
    // worker releases ownership; tick's try_lock cannot repeatedly overtake it.
    // No workspace read or mutation occurs until ownership is acquired.
    let guard=wait_for_retry_gate(&MEDIA_GATE,Duration::from_secs(300)).await?;
    let receipt=app.change(|d|{
        if body.get("leaseEpoch").is_some(){
            let object=body.as_object().ok_or_else(||super::bad("Invalid media resume request"))?;
            if object.keys().any(|k|!["jobId","leaseEpoch"].contains(&k.as_str())){return Err(super::bad("Unknown media resume field"));}
            let job_id=super::required(&body,"jobId")?;let epoch=body["leaseEpoch"].as_u64().ok_or_else(||super::bad("Invalid media lease epoch"))?;
            authorize_visual_resume(d,job_id,epoch)
        }else{authorize_interrupted_retry(d,&body,&actor.id,&super::now())}
    }).await?;
    let target=ResumeTarget::from_receipt(&receipt)?;
    schedule_resumed_media(&app,target,guard);
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
    if proof.keys().any(|k|!["receiptId","checkedAt","method","noMediaProcesses","fixArtifactSha256","completedDownload"].contains(&k.as_str()))
        || !matches!(text(verification,"method"),"operator_download_fix_review"|"operator_completed_youtube_hls_review")||verification["noMediaProcesses"]!=true
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
    let reviewed_hls=reviewed_youtube_hls_download(verification,&source_key,&version,&binding)?;
    if !video(&post)||!current_job(&job)||job["kind"]!="media"||job["account"]!=scope
        || attempt["postId"]!=post["id"]||attempt["postKey"]!=post["postKey"]||attempt["sourceKey"]!=source_key
        || attempt["sourceVersion"]!=version||body["sourceVersion"]!=version||body["connectorBinding"]!=binding
        || group(d,&post).is_none_or(|key|job["groupKey"]!=key){return Err(super::conflict("Download retry source or binding changed"));}
    if attempt["retryPermit"]["id"]==receipt_id{
        if attempt["retryPermit"]["request"]!=*body||attempt["retryPermit"]["authorizedBy"]!=actor{return Err(super::conflict("Retry receipt is already bound"));}
        return Ok(attempt["retryPermit"].clone());
    }
    // The claim guard checks opaque source history before a retry permit. Do
    // not queue new work that this unchanged guard would necessarily reject.
    // Historical absence never proves an unused acquisition budget.
    if rows(d,"jobs").iter().any(|j|opaque_source_matches(d,j,&post)){
        return Err(super::conflict("media_download_retry_opaque_history_reconciliation_required"));
    }
    let progress=&job["result"]["visualProgress"];
    let empty_legacy_shell=*progress==json!({"phase":"held","leaseId":null,"resumePhase":null});
    let previous_retry_consumed=job["downloadRetry"].is_null()||rows(&job,"sourceAttempts").iter().any(|a|
        a["retryOf"]["permitId"]==job["downloadRetry"]["id"]&&!text(&job["downloadRetry"],"id").is_empty());
    if job["status"]!="failed"||attempt["status"]!="failed"||rows(&job,"sourceAttempts").iter().any(|a|a["status"]!="failed")
        || !attempt["retryPermit"].is_null()||!previous_retry_consumed||!(progress.is_null()||empty_legacy_shell)
        || attempt["error"]!=body["expectedError"]
        || !(matches!(text(attempt,"error"),"source_download_failed"|"source_download_failed_format_unavailable")
            || reviewed_hls&&matches!(text(attempt,"error"),"source_download_failed_auth"|"source_download_failed_http_forbidden")){
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
    if reviewed_hls&&parse(text(&verification["completedDownload"],"completedAt"))?<finished {
        return Err(super::conflict("Completed download must follow the failed attempt"));
    }
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
// An explicit owner attestation of a completed same-source download, not an
// automatic retry or permission to switch URLs/accounts. The operator retains
// the real file and ffprobe/hash receipt; this API checks the exact binding and
// still applies the existing fresh-review, one-shot, no-checkpoint and budget
// guards. It does not claim to independently probe an operator's private file.
fn reviewed_youtube_hls_download(verification:&Value,source_key:&str,version:&str,binding:&Value)->super::ApiResult<bool>{
    let evidence=&verification["completedDownload"];
    if verification["method"]!="operator_completed_youtube_hls_review" {
        if !evidence.is_null(){return Err(super::bad("Unexpected completed download proof"));}
        return Ok(false);
    }
    let fields=evidence.as_object().ok_or_else(||super::bad("Completed YouTube HLS download proof is required"))?;
    let video_id=source_key.strip_prefix("yt:").unwrap_or("");
    let sha=text(evidence,"mediaSha256");
    if fields.keys().any(|k|!["sourceKey","sourceVersion","connectorBinding","transport","mediaSha256","bytes","durationMs","hasVideo","hasAudio","completedAt"].contains(&k.as_str()))
        || video_id.len()!=11||!video_id.bytes().all(|b|b.is_ascii_alphanumeric()||b"_-".contains(&b))
        || evidence["sourceKey"]!=source_key||evidence["sourceVersion"]!=version||evidence["connectorBinding"]!=*binding
        || evidence["transport"]!="youtube_hls"||sha.len()!=64||!sha.bytes().all(|b|b.is_ascii_hexdigit())
        || !evidence["bytes"].as_u64().is_some_and(|v|v>0&&v<=500*1024*1024)
        || !evidence["durationMs"].as_u64().is_some_and(|v|v>0&&v<=4*60*60*1000)
        || evidence["hasVideo"]!=true||evidence["hasAudio"]!=true {
        return Err(super::conflict("Completed download proof does not match this YouTube source"));
    }
    let completed=chrono::DateTime::parse_from_rfc3339(text(evidence,"completedAt")).map_err(|_|super::bad("Invalid completed download timestamp"))?.timestamp();
    let checked=chrono::DateTime::parse_from_rfc3339(text(verification,"checkedAt")).map_err(|_|super::bad("Invalid retry verification timestamp"))?.timestamp();
    if completed>checked||checked-completed>300{return Err(super::conflict("Completed download proof must be fresh"));}
    Ok(true)
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
    let jobs=PreparationJobs::new(d,&index);
    // Many comments point at one post; evaluate each post once per snapshot.
    let binding=super::active_binding(d)?;
    let mut by_post:BTreeMap<String,Option<&'static str>>=BTreeMap::new();
    let mut states=BTreeMap::new();
    for item in items {
        let state=if super::bound_item(&binding,item).is_err(){None}else{
            let post=rows(d,"posts").iter().find(|p|p["id"]==item["postId"]
                ||(!text(item,"postKey").is_empty()&&p["postKey"]==item["postKey"]));
            if let Some(post)=post {
                let id=text(post,"id");
                if let Some(state)=by_post.get(id){*state}else{
                    let state=preparation_state_for_post(d,post,&index,&transcripts,Some(&jobs))?;
                    by_post.insert(id.to_owned(),state);state
                }
            }else{None}
        };
        states.insert(text(item,"id").to_owned(),state);
    }
    Ok(states)
}
fn preparation_state_indexed(d:&Value,item:&Value,index:&QueueIndex,transcripts:&super::knowledge::TranscriptLookup)->super::ApiResult<Option<&'static str>>{
    let binding = super::active_binding(d)?;
    if super::bound_item(&binding, item).is_err() { return Ok(None); }
    let Some(post) = rows(d,"posts").iter().find(|p| p["id"] == item["postId"]
        || (!text(item,"postKey").is_empty() && p["postKey"] == item["postKey"])) else { return Ok(None); };
    preparation_state_for_post(d,post,index,transcripts,None)
}
/// Job references preserve saved order, including historical group aliases.
/// They are valid only while this database snapshot and QueueIndex are alive.
struct PreparationJobs<'a>{by_post:BTreeMap<String,Vec<&'a Value>>,pending_audio:BTreeSet<&'a str>}
impl<'a> PreparationJobs<'a>{
    fn new(d:&'a Value,index:&QueueIndex)->Self{
        let mut by_post:BTreeMap<String,Vec<&Value>>=BTreeMap::new();
        let mut pending_audio=BTreeSet::new();
        for job in rows(d,"jobs"){
            if job["kind"]=="media_audio"&&matches!(text(job,"status"),"queued"|"running"){
                if let Some(id)=job["refId"].as_str(){pending_audio.insert(id);}
            }
            if current_job(job){
                for post in index.posts(text(job,"groupKey")){
                    by_post.entry(text(post,"id").to_owned()).or_default().push(job);
                }
            }
        }
        Self{by_post,pending_audio}
    }
}
fn preparation_state_for_post(d:&Value,post:&Value,index:&QueueIndex,transcripts:&super::knowledge::TranscriptLookup,
    jobs:Option<&PreparationJobs<'_>>)->super::ApiResult<Option<&'static str>>{
    if !video(post){return Ok(None);}
    let policy=super::post_media_policy::effective_for_preparation(d,post)?;
    let visual_required=policy["visualRequired"]==true;
    // Acquisition may reuse a historical transcript to avoid another download,
    // but preparation always requires current full-audio coverage, even when
    // an exact owner policy additionally requires frames.
    if transcripts.ready_for_policy(post,false).map_err(|e|super::bad(&e))?
        && (!visual_required||transcripts.has_visual(post).map_err(|e|super::bad(&e))?) {return Ok(None);}
    if !visual_required{
        let pending_audio=match jobs{
            Some(jobs)=>jobs.pending_audio.contains(text(post,"id")),
            None=>rows(d,"jobs").iter().any(|j|j["kind"]=="media_audio"&&j["refId"]==post["id"]&&matches!(text(j,"status"),"queued"|"running")),
        };
        if pending_audio{return Ok(Some("media_wait"));}
        if !visual_wanted(d,post)&&!default_text_wanted(d,post){return Ok(Some("media_unavailable"));}
    }
    // A regular media job can still produce the missing audio transcript. The
    // preparation-only audio default must not mislabel that work unavailable.
    if !index.groups.contains_key(text(post,"id")) { return Ok(Some("media_unavailable")); }
    let job=match jobs{
        Some(jobs)=>jobs.by_post.get(text(post,"id")).and_then(|candidates|candidates.iter().rev()
            .find(|j|current_source_job(d,j)).copied()),
        None=>rows(d,"jobs").iter().rev().find(|j|current_job(j)&&current_source_job(d,j)
            &&index.posts(text(j,"groupKey")).iter().any(|candidate|candidate["id"]==post["id"])),
    };
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
    let index=QueueIndex::new(d);
    index.groups.get(text(post,"id"))?;
    let job = rows(d,"jobs").iter().rev().find(|j| current_job(j) && current_source_job(d,j)
        && index.posts(text(j,"groupKey")).iter().any(|candidate|candidate["id"]==post["id"])
        && matches!(text(j,"status"),"queued"|"running"))?;
    let timestamp = job["startedAt"].as_str().or_else(||job["createdAt"].as_str())?;
    chrono::DateTime::parse_from_rfc3339(timestamp).ok().map(|v|v.timestamp())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn accepted_resume_scheduling_does_not_await_blocked_or_failed_target_or_wake_generic(){
        use std::sync::{Arc,atomic::{AtomicUsize,Ordering}};
        let wake=tokio::sync::Notify::new();
        let started=Arc::new(tokio::sync::Notify::new());let release=Arc::new(tokio::sync::Notify::new());
        let observed=Arc::new(AtomicUsize::new(0));
        let (began,unblock,calls)=(started.clone(),release.clone(),observed.clone());
        let task=spawn_resume_scheduler(move||{
            let (began,unblock,calls)=(began.clone(),unblock.clone(),calls.clone());
            async move {calls.fetch_add(1,Ordering::SeqCst);began.notify_one();unblock.notified().await;
                Err(super::super::internal("offline scheduler failure after durable acceptance"))}
        });
        // No running periodic scheduler is needed: explicit work was spawned.
        tokio::time::timeout(Duration::from_secs(2),started.notified()).await.unwrap();
        assert!(!task.is_finished(),"scheduler remains blocked after acknowledgement helper returned");
        assert!(tokio::time::timeout(Duration::from_millis(30),wake.notified()).await.is_err(),
            "one-shot target scheduling must not wake a generic queue claim");
        release.notify_one();
        tokio::time::timeout(Duration::from_secs(2),task).await.unwrap().unwrap();
        assert_eq!(observed.load(Ordering::SeqCst),1,"ordinary scheduling errors are logged, not blind retries");
    }
    #[tokio::test]
    async fn accepted_resume_scheduler_panics_are_supervised(){
        use std::sync::{Arc,atomic::{AtomicUsize,Ordering}};
        let calls=Arc::new(AtomicUsize::new(0));let observed=calls.clone();
        let task=spawn_resume_scheduler(move||{
            let calls=calls.clone();async move {
                if calls.fetch_add(1,Ordering::SeqCst)==0{panic!("offline first scheduler generation");}
                Ok(())
            }
        });
        tokio::time::timeout(Duration::from_secs(3),task).await.unwrap().unwrap();
        assert_eq!(observed.load(Ordering::SeqCst),2,"panic must be observed and scheduler restarted");
    }
    #[tokio::test]
    async fn owner_retry_waiter_precedes_tick_and_timeout_or_cancel_leave_gate_free(){
        let gate=std::sync::Arc::new(tokio::sync::Mutex::new(()));
        let active=gate.lock().await;
        let (queued_tx,queued_rx)=tokio::sync::oneshot::channel();
        let (acquired_tx,mut acquired_rx)=tokio::sync::oneshot::channel();
        let (release_tx,release_rx)=tokio::sync::oneshot::channel();
        let waiter_gate=gate.clone();
        let waiter=tokio::spawn(async move {
            queued_tx.send(()).unwrap();
            let _owner=wait_for_retry_gate(&waiter_gate,Duration::from_secs(2)).await.unwrap();
            acquired_tx.send(()).unwrap();
            release_rx.await.unwrap();
        });
        queued_rx.await.unwrap();
        assert!(tokio::time::timeout(Duration::from_millis(30),&mut acquired_rx).await.is_err());
        drop(active);
        assert!(gate.try_lock().is_err(),"tick must not overtake an owner already waiting");
        tokio::time::timeout(Duration::from_secs(2),&mut acquired_rx).await.unwrap().unwrap();
        assert!(gate.try_lock().is_err(),"owner holds gate until its transaction finishes");
        release_tx.send(()).unwrap();waiter.await.unwrap();
        assert!(gate.try_lock().is_ok());

        let active=gate.lock().await;
        let timed_out=wait_for_retry_gate(&gate,Duration::from_millis(20)).await;
        assert!(timed_out.is_err(),"bounded wait returns a conflict");
        drop(active);
        assert!(gate.try_lock().is_ok(),"timeout must remove its queued waiter");

        let active=gate.lock().await;
        let cancelled_gate=gate.clone();
        let cancelled=tokio::spawn(async move {
            wait_for_retry_gate(&cancelled_gate,Duration::from_secs(2)).await.unwrap();
        });
        tokio::task::yield_now().await;
        cancelled.abort();assert!(cancelled.await.unwrap_err().is_cancelled());
        drop(active);
        assert!(gate.try_lock().is_ok(),"cancelled owner must not strand media ownership");
    }
    #[tokio::test]
    async fn completion_wake_is_retained_coalesced_and_keeps_recovery_poll() {
        use std::time::Duration;
        let wake = tokio::sync::Notify::new();
        let mut interval = tokio::time::interval(Duration::from_secs(15));
        interval.tick().await;
        // Completions before the scheduler waits retain one hint, never a queue
        // of stale hints that could repeatedly acquire the writer with no work.
        wake.notify_one();
        wake.notify_one();
        tokio::time::timeout(Duration::from_secs(2), wait_for_trigger(&wake, &mut interval)).await.unwrap();
        assert!(tokio::time::timeout(Duration::from_millis(30), wait_for_trigger(&wake, &mut interval)).await.is_err());
        let signal = async { tokio::task::yield_now().await; wake.notify_one(); };
        let (result, ()) = tokio::join!(tokio::time::timeout(Duration::from_secs(2), wait_for_trigger(&wake, &mut interval)), signal);
        result.unwrap();
        interval.reset_immediately();
        tokio::time::timeout(Duration::from_secs(2), wait_for_trigger(&wake, &mut interval)).await.unwrap();
    }

    #[tokio::test]
    async fn media_ownership_skips_writer_until_checkpoint_commits_and_task_is_removed() {
        use std::time::Duration;
        let (mut app, _temp) = crate::tests::test_app().await;
        app.external_writes = false;
        let key = app.job("media", "offline-scheduler").await.unwrap();
        app.change_job(&key, |d| {
            let job = crate::row_mut(d, "jobs", &key)?;
            job["visualContractVersion"] = json!(2);
            job["result"] = json!({"visualProgress":{"schemaVersion":2,"phase":"scan","leaseId":"offline-lease","nextSelectionIndex":4}});
            Ok(())
        }).await.unwrap();
        let media_guard = MEDIA_GATE.lock().await;
        // No committed incoming change is waiting for discovery in this case.
        app.media_discovery.pending.store(false,Ordering::Release);
        let writer_guard = app.gate.acquire(crate::writer_gate::Class::Standard).await;
        let (ran_tx, ran_rx) = tokio::sync::oneshot::channel();
        let (done_tx, mut done_rx) = tokio::sync::oneshot::channel();
        app.spawn_with_completion(key.clone(), async move {
            ran_tx.send(()).unwrap();
            Ok(json!({"resume":true}))
        }, move || {
            drop(media_guard);
            done_tx.send(()).unwrap();
        });
        tokio::time::timeout(Duration::from_secs(2), ran_rx).await.unwrap().unwrap();
        // The worker has returned, but its completion transaction is blocked.
        // Another media tick must return without joining the occupied writer.
        tokio::time::timeout(Duration::from_secs(2), tick(&app)).await.unwrap().unwrap();
        assert!(MEDIA_GATE.try_lock().is_err());
        assert!(matches!(done_rx.try_recv(), Err(tokio::sync::oneshot::error::TryRecvError::Empty)));
        assert!(app.tasks.lock().await.contains_key(&key));
        assert_eq!(app.db.read_job(&key).await.unwrap().unwrap()["status"], "running");
        drop(writer_guard);
        tokio::time::timeout(Duration::from_secs(2), done_rx).await.unwrap().unwrap();
        assert!(MEDIA_GATE.try_lock().is_ok());
        assert!(!app.tasks.lock().await.contains_key(&key));
        let job = app.db.read_job(&key).await.unwrap().unwrap();
        assert_eq!(job["status"], "queued");
        assert!(job["result"]["visualProgress"]["leaseId"].is_null());
        assert_eq!(job["result"]["visualProgress"]["nextSelectionIndex"], 4);
    }

    #[test]
    fn active_durable_media_job_does_not_reconcile_or_change_queue() {
        let mut d = fixture();
        claim(&mut d, AT).unwrap().unwrap();
        d["mediaQueue"]["inputDigest"] = json!("stale");
        let before = d.clone();
        assert!(claim(&mut d, AT).unwrap().is_none());
        assert_eq!(d, before);
    }
    const AT: &str = "2026-09-22T08:00:00Z";
    fn fixture() -> Value {
        fixture_for(super::super::accounts::Profile::LikeAvto)
    }
    #[test]
    fn baw_default_speech_handoff_preserves_source_and_never_claims_screen_text(){
        let mut d=fixture_for(crate::accounts::Profile::BawRussia);
        d["posts"].as_array_mut().unwrap().truncate(1);
        d["items"].as_array_mut().unwrap().truncate(1);
        d["settings"]["postMediaPolicies"]=json!({});
        let (mut d,origin,_)=text_download_checkpoint_for(d,false);
        let captured=crate::row(&d,"jobs",&origin).unwrap().clone();
        assert_eq!(captured["acquisitionProfile"],cached_audio::SPEECH_PROFILE);
        reconcile(&mut d,AT).unwrap();
        assert!(claim(&mut d,AT).unwrap().is_none(),"speech source must not enter frame inventory");
        assert!(cached_audio::claim_text_automatic(&mut d,AT,false).unwrap().is_none(),"new speech work must not invoke OCR");
        let (child,pin)=cached_audio::claim_automatic(&mut d,AT,false).unwrap().unwrap();
        assert_eq!(pin["profile"],cached_audio::SPEECH_PROFILE);
        assert_eq!(pin["originJobId"],origin);
        assert_eq!(pin["progress"],captured["result"]["visualProgress"]);
        assert_eq!(crate::row(&d,"jobs",&child).unwrap()["purpose"],"required_video_speech");
        assert_eq!(crate::row(&d,"jobs",&origin).unwrap()["sourceAttempts"],captured["sourceAttempts"]);
        crate::row_mut(&mut d,"jobs",&child).unwrap()["status"]=json!("unknown");
        let uncertain=d.clone();
        assert!(cached_audio::claim_automatic(&mut d,AT,false).unwrap().is_none());
        assert_eq!(d,uncertain,"uncertain speech extraction is never blindly retried");
    }
    #[test]
    fn retired_visual_override_fresh_request_uses_speech_profile(){
        let mut d=fresh_text_fixture();select_visual_fixture(&mut d);
        let post=d["posts"][0].clone();let prior=d["settings"]["postMediaPolicies"][text(&post,"id")].clone();
        super::super::post_media_policy::retire_visual_for_test(&mut d,text(&post,"id")).unwrap();
        let receipt=enqueue_selected(&mut d,&post,AT,true).unwrap();
        let job=super::super::row(&d,"jobs",text(&receipt,"jobId")).unwrap();
        assert_eq!(job["acquisitionProfile"],cached_audio::SPEECH_PROFILE);
        assert!(default_text_wanted(&d,&post));assert!(!visual_wanted(&d,&post));
        assert_eq!(d["settings"]["postMediaPolicies"][text(&post,"id")]["previousRecord"],prior);
        let (mut d,id,_)=text_download_checkpoint_for(d,true);
        let (child,pin)=cached_audio::claim_automatic(&mut d,AT,true).unwrap().unwrap();
        assert_eq!(pin["originJobId"],id);assert_eq!(pin["profile"],cached_audio::SPEECH_PROFILE);
        assert_eq!(super::super::row(&d,"jobs",&child).unwrap()["purpose"],"required_video_speech");
    }
    fn fresh_text_fixture()->Value{
        let mut d=fixture();
        d["posts"].as_array_mut().unwrap().truncate(1);
        d["items"].as_array_mut().unwrap().truncate(1);
        d["settings"]["postMediaPolicies"]=json!({});
        d
    }
    fn selected_collision_fixture()->(Value,Value,Value){
        let mut d=fixture();d["settings"]["postMediaPolicies"]=json!({});
        d["posts"][0]["sourceUrl"]=json!("https://vk.com/video-135891342_456248867");
        d["posts"][1]["sourceUrl"]=json!("https://www.youtube.com/watch?v=kU0v-_gixbA");
        let peer=d["posts"][0].clone();let target=d["posts"][1].clone();
        assert_eq!(group(&d,&peer),group(&d,&target),"regression requires a proven identity group collision");
        assert_ne!(super::super::knowledge::media_source_key(&peer,text(&d,"account")),
            super::super::knowledge::media_source_key(&target,text(&d,"account")));
        let receipt=enqueue_selected(&mut d,&peer,AT,true).unwrap();
        let (d,id,source)=text_download_checkpoint_for(d,false);
        assert_eq!(json!(id),receipt["jobId"]);assert_eq!(source["id"],peer["id"]);
        assert!(current_source_job(&d,&d["jobs"][0]));
        (d,peer,target)
    }
    #[test]
    fn selected_exact_text_ignores_foreign_title_peer_and_preserves_owned_cas(){
        for status in ["paused","completed","unknown","running","dispatching","failed","cancelled","queued"]{
            for peer_text in [true,false]{
                let (mut d,_,target)=selected_collision_fixture();
                d["jobs"][0]["status"]=json!(status);
                if !peer_text{d["jobs"][0]["acquisitionProfile"]=Value::Null;}
                let retained=d["jobs"][0].clone();
                assert_eq!(rows(&retained,"sourceAttempts").len(),1);
                assert!(retained["result"]["visualProgress"]["source"].is_object());
                let receipt=enqueue_selected(&mut d,&target,AT,true).unwrap();
                assert_eq!(receipt["status"],"queued","{status}/{peer_text}");
                assert_ne!(receipt["jobId"],retained["id"]);
                assert_eq!(d["jobs"][0],retained,"foreign attempts, checkpoint, CAS and manual pin remain immutable");
                assert_eq!(rows(&d,"jobs").len(),2);
                let job=crate::row(&d,"jobs",text(&receipt,"jobId")).unwrap().clone();
                assert_eq!(job["refId"],target["id"]);assert_eq!(job["acquisitionProfile"],cached_audio::SPEECH_PROFILE);
                assert!(rows(&job,"sourceAttempts").is_empty());
                assert!(manual_source_requested(&d,&job,&target));
                assert_eq!(next_source(&d,&job).unwrap()["id"],target["id"],"new work remains exactly pinned");
                assert_eq!(enqueue_selected(&mut d,&target,AT,true).unwrap()["jobId"],receipt["jobId"]);
                assert_eq!(rows(&d,"jobs").len(),2);assert_eq!(d["jobs"][0],retained);
            }
        }
    }
    #[test]
    fn selected_exact_existing_paid_or_unknown_text_work_deduplicates_without_retry(){
        for status in ["paused","completed","unknown","running","dispatching","failed","cancelled","queued"]{
            let (mut d,peer,_)=selected_collision_fixture();d["jobs"][0]["status"]=json!(status);
            let retained=d["jobs"][0].clone();
            let receipt=enqueue_selected(&mut d,&peer,AT,true).unwrap();
            assert_eq!(receipt["jobId"],retained["id"]);assert_eq!(receipt["status"],status);
            assert_eq!(receipt["deduplicated"],true);assert_eq!(rows(&d,"jobs").len(),1);
            assert_eq!(d["jobs"][0],retained,"dedup is not a retry, resume or new attempt");
        }
    }
    #[test]
    fn selected_exact_owned_profile_or_checkpoint_conflict_cannot_mutate_work(){
        for field in ["profile","checkpoint_source"]{
            let (mut d,peer,target)=selected_collision_fixture();
            if field=="profile"{d["jobs"][0]["acquisitionProfile"]=Value::Null;}
            else{
                let mut progress=super::super::media_fullframes::initial(text(&d,"account"),
                    &super::super::active_binding(&d).unwrap().to_json(),&target,AT);
                progress["materialEpoch"]=json!(material_epoch(&d,&target));
                d["jobs"][0]["result"]["visualProgress"]=progress;
            }
            assert!(current_source_job(&d,&d["jobs"][0]),"case must reach selected admission guard");
            let before=d.clone();let result=enqueue_selected(&mut d,&peer,AT,true);
            assert!(result.is_err(),"{field}");assert_eq!(d,before,"conflict keeps all owned work immutable");
        }
    }
    #[test]
    fn unselected_background_discovery_retains_existing_family_dedup(){
        let (mut d,_,target)=selected_collision_fixture();let retained=d["jobs"][0].clone();
        let receipt=enqueue(&mut d,&target,AT,false).unwrap();
        assert_eq!(receipt["jobId"],retained["id"]);assert_eq!(receipt["deduplicated"],true);
        assert_eq!(rows(&d,"jobs").len(),1);assert_eq!(d["jobs"][0],retained);
    }
    #[test]
    fn selected_attempted_source_cannot_be_queued_using_unattempted_title_peer(){
        let (mut d,peer,target)=selected_collision_fixture();
        let mut spare=peer.clone();spare["id"]=json!("post-11391:spare");spare["postKey"]=json!("11391:spare");
        spare["sourceUrl"]=json!("https://vk.com/video-135891342_456248868");
        crate::list_mut(&mut d,"posts").push(spare.clone());
        let attempt=json!({"postId":target["id"],"postKey":target["postKey"],"status":"unknown","attemptNumber":2,
            "sourceKey":super::super::knowledge::media_source_key(&target,text(&d,"account")),
            "sourceVersion":super::super::media_fullframes::source_version(&target,text(&d,"account"))});
        d["jobs"][0]["sourceAttempts"].as_array_mut().unwrap().push(attempt);
        assert!(attempted(&d,&target));assert!(!attempted(&d,&spare));
        let retained=d["jobs"][0].clone();
        let receipt=enqueue_selected(&mut d,&target,AT,true).unwrap();
        assert_eq!(receipt["status"],"failed","a free sibling is not selected-source availability");
        let job=crate::row(&d,"jobs",text(&receipt,"jobId")).unwrap();
        assert_eq!(job["refId"],target["id"]);assert!(rows(job,"sourceAttempts").is_empty());
        assert!(next_source(&d,job).is_none(),"UNKNOWN target ownership never falls back to a free title peer");
        assert_eq!(d["jobs"][0],retained,"uncertain source attempt stays with its original job");
    }
    fn text_download_checkpoint()->(Value,String,Value){
        text_download_checkpoint_for(fresh_text_fixture(),false)
    }
    fn text_download_checkpoint_for(mut d:Value,only_open:bool)->(Value,String,Value){
        let (id,post)=claim_scoped(&mut d,AT,only_open).unwrap().unwrap();
        let before=super::super::row(&d,"jobs",&id).unwrap()["result"]["visualProgress"].clone();
        assert_eq!(before["phase"],"download");
        let mut next=before.clone();next["phase"]=json!("inventory");
        next["source"]=json!({"sha256":"a".repeat(64),"bytes":1024});
        next["sourceIdentity"]=json!({"account":d["account"],"postKey":post["postKey"],
            "mediaSha256":"a".repeat(64),"durationMs":62061});
        next["sourceProjection"]=json!({"account":d["account"],"postKey":post["postKey"],
            "title":post["title"],"sourceUrl":"https://example.test/current-video","fallbackUrl":null});
        if let Some(pin)=before.get("assetPin"){next["sourceProjection"]["assetPin"]=pin.clone();}
        super::super::media_fullframes::checkpoint(super::super::row_mut(&mut d,"jobs",&id).unwrap(),
            text(&before,"leaseId"),&before,next).unwrap();
        let result:super::super::ApiResult<Value>=Ok(json!({"resume":true}));
        assert!(super::super::media_fullframes::finish(super::super::row_mut(&mut d,"jobs",&id).unwrap(),&result,AT));
        (d,id,post)
    }
    #[test]
    fn exact_closed_manual_text_acquisition_reaches_cached_handoff_without_opening_history(){
        let mut d=fresh_text_fixture();
        d["items"][0]["providerStatus"]=json!("closed");d["items"][0]["workflow"]=json!("closed");
        let post=d["posts"][0].clone();
        let mut other=post.clone();other["id"]=json!("post-11391:unrequested");
        other["postKey"]=json!("11391:unrequested");other["title"]=json!("Unrequested closed archive");
        crate::list_mut(&mut d,"posts").push(other.clone());
        let mut item=d["items"][0].clone();item["id"]=json!("unrequested-closed");item["itemId"]=json!("unrequested");
        item["postId"]=other["id"].clone();item["postKey"]=other["postKey"].clone();
        crate::list_mut(&mut d,"items").push(item);
        reconcile_scoped(&mut d,AT,true).unwrap();assert!(rows(&d,"jobs").is_empty());
        let requested=enqueue_selected(&mut d,&post,AT,true).unwrap();
        let id=text(&requested,"jobId").to_owned();
        assert!(manual_source_requested(&d,crate::row(&d,"jobs",&id).unwrap(),&post));
        let (mut d,claimed,source)=text_download_checkpoint_for(d,true);
        assert_eq!(claimed,id);assert_eq!(source["id"],post["id"]);
        reconcile_scoped(&mut d,AT,true).unwrap();
        assert!(claim_scoped(&mut d,AT,true).unwrap().is_none());
        let (audio,pin)=cached_audio::claim_automatic(&mut d,AT,true).unwrap().unwrap();
        assert_eq!(pin["originJobId"],id);assert_eq!(pin["progress"]["sourcePostId"],post["id"]);
        assert_eq!(crate::row(&d,"jobs",&audio).unwrap()["refId"],post["id"]);
        assert!(rows(&d,"jobs").iter().all(|job|job["refId"]!=other["id"]));
        assert_eq!(rows(crate::row(&d,"jobs",&id).unwrap(),"sourceAttempts").len(),1);
    }
    #[test]
    fn closed_manual_source_change_refuses_group_fallback_and_keeps_original_pin(){
        let mut d=fresh_text_fixture();d["items"][0]["providerStatus"]=json!("closed");
        let original=d["posts"][0].clone();
        let receipt=enqueue_selected(&mut d,&original,AT,true).unwrap();let id=text(&receipt,"jobId").to_owned();
        let before=crate::row(&d,"jobs",&id).unwrap().clone();
        let mut peer=original.clone();peer["id"]=json!("post-11391:peer");peer["postKey"]=json!("11391:peer");
        crate::list_mut(&mut d,"posts").push(peer);
        d["posts"][0]["sourceUrl"]=json!("https://vk.com/video-11391_991");
        let current=d["posts"][0].clone();
        assert!(!manual_source_requested(&d,&before,&current));
        assert!(next_source(&d,&before).is_none());
        assert!(claim_scoped(&mut d,AT,true).unwrap().is_none());
        let after=crate::row(&d,"jobs",&id).unwrap();
        assert_eq!(after["manualAcquisition"],before["manualAcquisition"]);
        assert!(rows(after,"sourceAttempts").is_empty());
        assert!(enqueue_selected(&mut d,&current,AT,true).is_err(),"manual source proof is not silently rebound");
    }
    #[test]
    fn closed_manual_permission_retains_exact_account_binding_source_and_comment_cutoff(){
        let mut d=fresh_text_fixture();d["items"][0]["providerStatus"]=json!("closed");
        d["items"][0]["createdAt"]=json!("2026-09-27T16:00:00Z");
        let post=d["posts"][0].clone();let receipt=enqueue_selected(&mut d,&post,AT,true).unwrap();
        let job=crate::row(&d,"jobs",text(&receipt,"jobId")).unwrap().clone();
        let cutoff=parse_comment_cutoff(Some("2026-09-27T16:00:00Z")).unwrap();
        assert!(manual_source_requested_with_cutoff(&d,&job,&post,&cutoff));
        d["items"][0]["createdAt"]=json!("2026-09-27T16:00:01Z");
        assert!(!manual_source_requested_with_cutoff(&d,&job,&post,&cutoff));
        d["items"][0]["createdAt"]=Value::Null;
        assert!(!manual_source_requested_with_cutoff(&d,&job,&post,&cutoff));
        assert!(manual_source_requested_with_cutoff(&d,&job,&post,&None));
        for field in ["account","connectorBinding","sourceVersion","postId","postKey"]{
            let mut altered=job.clone();altered["manualAcquisition"][field]=json!("other");
            assert!(!manual_source_requested_with_cutoff(&d,&altered,&post,&None),"{field} is an exact source boundary");
        }
    }
    #[test]
    fn multi_video_discovery_commit_then_claim_keeps_one_unspent_asset_intent(){
        let mut d=fresh_text_fixture();d["posts"][0]["attachments"]=json!([
            {"type":"video","source_url":"https://example.test/first.mp4"},
            {"type":"video","source_url":"https://example.test/second.mp4"}]);
        // Production discovery commits before the scheduler's claim transaction.
        reconcile(&mut d,AT).unwrap();assert_eq!(rows(&d,"jobs").len(),1);
        let before=d["jobs"][0].clone();assert_eq!(before["status"],"queued");assert_eq!(before["fallbackAllowed"],false);
        assert!(claim_selected_ready(&mut d,AT,false,None,&|_|Err("media_runtime_unavailable".into())).unwrap().is_none());
        assert_eq!(d["jobs"][0]["status"],"queued");assert!(rows(&d["jobs"][0],"sourceAttempts").is_empty());
        assert_eq!(d["jobs"][0]["videoSpeechAssetPin"],before["videoSpeechAssetPin"]);
        let (id,_)=claim_selected_ready(&mut d,AT,false,None,&|_|Ok(())).unwrap().unwrap();
        assert_eq!(id,before["id"].as_str().unwrap());assert_eq!(rows(&d,"jobs").len(),1);
        let claimed=crate::row(&d,"jobs",&id).unwrap();assert_eq!(claimed["status"],"running");
        assert_eq!(rows(claimed,"sourceAttempts").len(),1);assert_eq!(claimed["sourceAttempts"][0]["assetPin"],before["videoSpeechAssetPin"]);
        let post=d["posts"][0].clone();mark_attempt(&mut d,&id,&post,Some("synthetic definitive failure"),AT).unwrap();
        crate::row_mut(&mut d,"jobs",&id).unwrap()["status"]=json!("failed");
        let failed=crate::row(&d,"jobs",&id).unwrap().clone();
        reconcile(&mut d,AT).unwrap();
        assert_eq!(crate::row(&d,"jobs",&id).unwrap()["status"],"failed");
        assert_eq!(crate::row(&d,"jobs",&id).unwrap()["sourceAttempts"],failed["sourceAttempts"]);
    }
    #[test]
    fn multi_video_speech_owns_exact_attachment_and_never_claims_inventory(){
        for visual_required in [false,true]{
            let mut d=fresh_text_fixture();
            d["posts"][0]["attachments"]=json!([
                {"type":"video","source_url":"https://example.test/first.mp4"},
                {"type":"video","source_url":"https://example.test/second.mp4"}]);
            if visual_required{
                let post=d["posts"][0].clone();
                let binding=crate::active_binding(&d).unwrap().to_json();
                d["settings"]["postMediaPolicies"][text(&post,"id")]=json!({"version":1,"revision":1,"status":"active",
                    "postId":post["id"],"account":d["account"],"connectorBinding":binding,
                    "sourceVersion":crate::media_fullframes::source_version(&post,text(&d,"account")),"mode":"full_audio_visual"});
            }
            let (mut d,id,post)=text_download_checkpoint_for(d,false);
            let original=crate::row(&d,"jobs",&id).unwrap().clone();
            let pin=&original["videoSpeechAssetPin"];
            assert_eq!(pin["attachmentIndex"],0);
            assert_eq!(original["result"]["visualProgress"]["assetPin"],*pin);
            assert_eq!(original["sourceAttempts"][0]["assetPin"],*pin);
            assert_eq!(original["sourceAttempts"][0]["attemptNumber"],1);
            assert!(crate::media_speech_assets::require_progress(&d,&original["result"]["visualProgress"]).is_ok());
            reconcile(&mut d,AT).unwrap();
            assert!(claim_selected_ready(&mut d,AT,false,None,&|phase|if phase=="audio"{Err("audio unavailable".into())}else{Ok(())}).unwrap().is_none(),"required speech must not decode inventory even with a visual policy");
            let stored=crate::row(&d,"jobs",&id).unwrap();
            assert_eq!(stored["status"],"paused");assert_eq!(stored["result"]["visualProgress"],original["result"]["visualProgress"]);
            let (audio,pinned)=cached_audio::claim_automatic(&mut d,AT,false).unwrap().unwrap();
            assert_eq!(pinned["progress"]["assetPin"],*pin);
            assert_eq!(crate::row(&d,"jobs",&audio).unwrap()["purpose"],"required_video_speech");
            assert!(!has_required_media(&d,&post,AT).unwrap());
        }
    }
    #[test]
    fn multi_video_failed_asset_keeps_its_history_and_allows_distinct_sibling(){
        let mut d=fresh_text_fixture();
        d["posts"][0]["attachments"]=json!([
            {"type":"video","source_url":"https://example.test/same.mp4"},
            {"type":"video","source_url":"https://example.test/same.mp4"}]);
        let post=d["posts"][0].clone();
        let first=enqueue(&mut d,&post,AT,false).unwrap();
        let original=crate::row_mut(&mut d,"jobs",text(&first,"jobId")).unwrap();
        original["status"]=json!("failed");original["error"]=json!("synthetic definitive pre-dispatch failure");
        let original=original.clone();
        let second=enqueue(&mut d,&post,AT,false).unwrap();
        assert_ne!(first["jobId"],second["jobId"]);assert_eq!(second["assetPin"]["attachmentIndex"],1);
        assert_eq!(crate::row(&d,"jobs",text(&first,"jobId")).unwrap(),&original);
        let duplicate=enqueue(&mut d,&post,AT,false).unwrap();assert_eq!(duplicate["jobId"],second["jobId"]);
        let job=crate::row(&d,"jobs",text(&second,"jobId")).unwrap().clone();
        assert_eq!(next_source(&d,&job).unwrap()["id"],post["id"]);
        let mut changed=d.clone();changed["posts"][0]["attachments"][1]["source_url"]=json!("https://example.test/changed.mp4");
        assert!(next_source(&changed,&job).is_none());
        assert!(!current_source_job(&changed,&job));
    }
    #[test]
    fn multi_video_unknown_asset_never_creates_a_replacement_attempt(){
        let mut d=fresh_text_fixture();d["posts"][0]["attachments"]=json!([
            {"type":"video","source_url":"https://example.test/first.mp4"},
            {"type":"video","source_url":"https://example.test/second.mp4"}]);
        let post=d["posts"][0].clone();let first=enqueue(&mut d,&post,AT,false).unwrap();
        crate::row_mut(&mut d,"jobs",text(&first,"jobId")).unwrap()["status"]=json!("unknown");
        let before=d.clone();let held=enqueue(&mut d,&post,AT,false).unwrap();
        assert_eq!(held["jobId"],first["jobId"]);assert_eq!(held["status"],"unknown");assert_eq!(d,before);
    }
    #[test]
    fn fresh_speech_acquisition_download_hands_same_cas_to_audio_without_frame_claim(){
        let pending=fresh_text_fixture();
        assert_eq!(preparation_state(&pending,&pending["items"][0],AT).unwrap(),Some("media_wait"),
            "fresh default speech acquisition must remain runnable before discovery claims its source");
        let (mut d,id,post)=text_download_checkpoint();
        let before=super::super::row(&d,"jobs",&id).unwrap().clone();
        assert_eq!(before["acquisitionProfile"],cached_audio::SPEECH_PROFILE);
        assert_eq!(rows(&before,"sourceAttempts").len(),1);
        reconcile(&mut d,AT).unwrap();
        assert!(claim(&mut d,AT).unwrap().is_none(),"text CAS cannot enter inventory/frame processing");
        let origin=super::super::row(&d,"jobs",&id).unwrap();
        assert_eq!(origin["status"],"paused");assert_eq!(origin["mediaPolicyPause"],true);
        assert_eq!(origin["result"]["visualProgress"],before["result"]["visualProgress"]);
        let (audio,expected)=cached_audio::claim_automatic(&mut d,AT,false).unwrap().unwrap();
        assert_eq!(expected["originJobId"],id);assert_eq!(expected["profile"],cached_audio::SPEECH_PROFILE);
        assert_eq!(expected["progress"],before["result"]["visualProgress"]);
        assert_eq!(super::super::row(&d,"jobs",&audio).unwrap()["purpose"],"required_video_speech");
        assert_eq!(rows(super::super::row(&d,"jobs",&id).unwrap(),"sourceAttempts").len(),1);
        assert!(cached_audio::claim_automatic(&mut d,AT,false).unwrap().is_none());
        super::super::row_mut(&mut d,"jobs",&audio).unwrap()["status"]=json!("failed");
        assert!(cached_audio::claim_automatic(&mut d,AT,false).unwrap().is_none(),"failed extraction is not auto retried");
        assert!(!has_required_media(&d,&post,AT).unwrap());
    }
    #[test]
    fn captured_speech_profile_cannot_become_visual_when_owner_changes_policy(){
        let (mut d,id,post)=text_download_checkpoint();
        let checkpoint=super::super::row(&d,"jobs",&id).unwrap()["result"]["visualProgress"].clone();
        d["settings"]["postMediaPolicies"][text(&post,"id")]=json!({"version":1,"revision":1,"status":"active",
            "postId":post["id"],"account":d["account"],"connectorBinding":super::super::active_binding(&d).unwrap().to_json(),
            "sourceVersion":super::super::media_fullframes::source_version(&post,text(&d,"account")),"mode":"full_audio_visual"});
        reconcile(&mut d,AT).unwrap();
        assert!(claim(&mut d,AT).unwrap().is_none());
        assert!(cached_audio::claim_automatic(&mut d,AT,false).unwrap().is_none());
        let origin=super::super::row(&d,"jobs",&id).unwrap();
        assert_eq!(origin["status"],"paused");assert_eq!(origin["result"]["visualProgress"],checkpoint);
        assert_eq!(rows(origin,"sourceAttempts").len(),1);
    }
    #[test]
    fn zero_work_profile_switch_allocates_one_compatible_job_and_preserves_attempted_work(){
        for start_text in [true,false]{
            let mut d=if start_text{fresh_text_fixture()}else{fixture()};
            d["posts"].as_array_mut().unwrap().truncate(1);d["items"].as_array_mut().unwrap().truncate(1);
            reconcile(&mut d,AT).unwrap();let old=d["jobs"][0].clone();let post=d["posts"][0].clone();
            if start_text{select_visual_fixture(&mut d);}else{d["settings"]["postMediaPolicies"]=json!({});}
            reconcile(&mut d,AT).unwrap();
            assert_eq!(rows(&d,"jobs").len(),2);
            let retained=super::super::row(&d,"jobs",text(&old,"id")).unwrap();
            assert_eq!(retained["status"],"cancelled");
            for field in ["acquisitionProfile","sourceAttempts","refId","connectorBinding","account"]{
                assert_eq!(retained[field],old[field],"zero-work lineage must not rewrite {field}");
            }
            let new=d["jobs"][1].clone();assert_ne!(new["id"],old["id"]);
            assert_eq!(new["acquisitionProfile"]==cached_audio::SPEECH_PROFILE,!start_text);
            assert_eq!(new["acquisitionSupersedes"]["jobId"],old["id"]);
            assert_eq!(new["acquisitionSupersedes"]["sourceVersion"],super::super::media_fullframes::source_version(&post,text(&d,"account")));
            let mut expected_retained=old.clone();expected_retained["status"]=json!("cancelled");
            expected_retained["finishedAt"]=json!(AT);expected_retained["result"]["acquisitionSupersededBy"]=new["id"].clone();
            assert_eq!(retained,&expected_retained,"only terminal settlement and exact paired lineage may change");
            let receipt=enqueue_selected(&mut d,&post,AT,true).unwrap();
            assert_eq!(receipt["jobId"],new["id"]);assert_eq!(receipt["deduplicated"],true);
            assert_eq!(rows(&d,"jobs").len(),2);
            let (claimed,_)=claim(&mut d,AT).unwrap().unwrap();assert_eq!(json!(claimed),new["id"]);
            let captured=super::super::row(&d,"jobs",&claimed).unwrap().clone();
            assert_eq!(rows(&captured,"sourceAttempts").len(),1);
            super::super::media_fullframes::finish(super::super::row_mut(&mut d,"jobs",&claimed).unwrap(),&Ok(json!({"resume":true})),AT);
            if start_text{d["settings"]["postMediaPolicies"]=json!({});}else{select_visual_fixture(&mut d);}
            reconcile(&mut d,AT).unwrap();
            assert_eq!(rows(&d,"jobs").len(),2,"attempted work cannot be profile-replaced");
            assert_eq!(super::super::row(&d,"jobs",&claimed).unwrap()["sourceAttempts"],captured["sourceAttempts"]);
            let before=d.clone();
            assert!(enqueue_selected(&mut d,&post,AT,true).is_err(),"attempted incompatible profile must hold, not deduplicate or allocate");
            assert_eq!(d,before,"profile conflict preserves every owned row");
            assert!(claim(&mut d,AT).unwrap().is_none());
        }
        let mut explicit=fixture();let post=explicit["posts"][0].clone();
        explicit["settings"]["postMediaPolicies"][text(&post,"id")]["mode"]=json!("full_audio_only");
        let result=enqueue(&mut explicit,&post,AT,true).unwrap();
        assert_eq!(result["cachedAudioRequestRequired"],true);
        assert!(rows(&explicit,"jobs").is_empty());
    }
    #[tokio::test]
    async fn zero_work_profile_supersession_persists_through_scoped_media_transaction(){
        let (app,_temp)=crate::tests::test_app().await;
        let mut d=fresh_text_fixture();reconcile(&mut d,AT).unwrap();let old=d["jobs"][0].clone();
        seed_current_owner_media_fixture(&app,d).await;
        app.change(|state|{select_visual_fixture(state);Ok(())}).await.unwrap();
        app.change_media(|state|reconcile(state,AT)).await.unwrap();
        let after=app.read().await.unwrap();assert_eq!(rows(&after,"jobs").len(),2);
        assert_eq!(after["jobs"][0]["status"],"cancelled");
        assert_eq!(after["jobs"][0]["sourceAttempts"],old["sourceAttempts"]);
        assert_eq!(after["jobs"][0]["acquisitionProfile"],old["acquisitionProfile"]);
        assert_eq!(after["jobs"][1]["acquisitionSupersedes"]["jobId"],old["id"]);
        assert!(after["jobs"][1]["acquisitionProfile"].is_null());
        assert!(app.tasks.lock().await.is_empty());app.db.close().await;
    }

    #[tokio::test]
    async fn discovery_wake_queues_first_comment_while_other_source_runs_without_duplicate_claim(){
        let (app,_temp)=crate::tests::test_app().await;
        let mut d=fixture();d["items"].as_array_mut().unwrap().truncate(1);
        let (running,_)=claim(&mut d,AT).unwrap().unwrap();
        let running_before=crate::row(&d,"jobs",&running).unwrap().clone();
        let mut post=d["posts"][0].clone();post["id"]=json!("post-11391:arrival");
        post["postKey"]=json!("11391:arrival");post["canonicalMediaId"]=json!("arrival-source");
        post["title"]=json!("Independent newly observed media");
        post["canonicalMediaId"]=json!("independent-new-video");
        post["sourceUrl"]=json!("https://vk.com/video-11391_987654");
        crate::list_mut(&mut d,"posts").push(post.clone());
        assert_ne!(group(&d,&post).unwrap(),text(&running_before,"groupKey"),
            "the arrival must be a different media family, not another comment on the running source");
        seed_current_owner_media_fixture(&app,d).await;
        app.media_discovery.pending.store(false,Ordering::Release);
        let execution=MEDIA_GATE.lock().await;
        let mut incoming=fixture()["items"][0].clone();incoming["id"]=json!("first-arrival");
        incoming["itemId"]=json!("first-arrival-target");incoming["postId"]=post["id"].clone();
        incoming["postKey"]=post["postKey"].clone();incoming["conversationKey"]=post["postKey"].clone();
        // Exercise the real committed-source hook, not a direct queue mutation.
        app.change_source_snapshot(|state|{crate::list_mut(state,"items").push(incoming.clone());Ok(())}).await.unwrap();
        assert!(app.media_discovery.pending.load(Ordering::Acquire));
        tokio::time::timeout(Duration::from_secs(2),tick(&app)).await.unwrap().unwrap();
        let after=app.read().await.unwrap();
        let queued:Vec<_>=rows(&after,"jobs").iter().filter(|j|j["refId"]==post["id"]).collect();
        assert_eq!(queued.len(),1);assert_eq!(queued[0]["status"],"queued");
        assert!(rows(queued[0],"sourceAttempts").is_empty());
        assert_eq!(crate::row(&after,"jobs",&running).unwrap(),&running_before);
        assert!(app.tasks.lock().await.is_empty(),"discovery cannot launch another process");
        // Multiple sync wake hints and a second comment still own just one job.
        app.change_source_snapshot(|state|{let mut next=incoming.clone();next["id"]=json!("second-arrival");
            next["itemId"]=json!("second-arrival-target");crate::list_mut(state,"items").push(next);Ok(())}).await.unwrap();
        notify_after_sync_commit(&app);tick(&app).await.unwrap();
        let repeated=app.read().await.unwrap();
        assert_eq!(rows(&repeated,"jobs").iter().filter(|j|j["refId"]==post["id"]).count(),1);
        assert_eq!(rows(&repeated,"jobs").iter().filter(|j|j["status"]=="running").count(),1);
        assert_eq!(repeated["operations"],after["operations"]);assert_eq!(repeated["proposals"],after["proposals"]);
        drop(execution);app.db.close().await;
    }

    #[tokio::test]
    async fn discovery_cancellation_restores_hint_and_sync_during_discovery_is_not_lost(){
        let (app,_temp)=crate::tests::test_app().await;
        let writer=app.gate.acquire(crate::writer_gate::Class::Standard).await;
        let worker=app.clone();let task=tokio::spawn(async move{discover_pending(&worker).await});
        tokio::time::timeout(Duration::from_secs(2),async {
            while app.media_discovery.pending.load(Ordering::Acquire){tokio::task::yield_now().await;}
        }).await.unwrap();
        task.abort();assert!(task.await.unwrap_err().is_cancelled());
        assert!(app.media_discovery.pending.load(Ordering::Acquire));
        let worker=app.clone();let task=tokio::spawn(async move{discover_pending(&worker).await});
        tokio::time::timeout(Duration::from_secs(2),async {
            while app.media_discovery.pending.load(Ordering::Acquire){tokio::task::yield_now().await;}
        }).await.unwrap();
        notify_after_sync_commit(&app);
        drop(writer);task.await.unwrap().unwrap();
        assert!(app.media_discovery.pending.load(Ordering::Acquire),"later committed sync retains a distinct discovery hint");
        discover_pending(&app).await.unwrap();assert!(!app.media_discovery.pending.load(Ordering::Acquire));
        // Failed settlement is also retained, with no process or source attempt.
        app.db.close().await;notify_after_sync_commit(&app);
        assert!(discover_pending(&app).await.is_err());assert!(app.media_discovery.pending.load(Ordering::Acquire));
    }
    #[test]
    fn early_media_enablement_is_independent_and_preserves_unset_legacy_modes() {
        for background in [None,Some("0"),Some("1")] {
            for generation in [None,Some("0"),Some("1")] {
                for media in [None,Some("0"),Some("1"),Some("invalid")] {
                    let actual=background_enabled_with(|key|match key {
                        "COMMUNITYHERO_BACKGROUND_DISABLED"=>background,
                        "COMMUNITYHERO_BACKGROUND_GENERATION_DISABLED"=>generation,
                        "COMMUNITYHERO_MEDIA_PREPARATION_ENABLED"=>media,
                        _=>None,
                    }.map(str::to_owned));
                    let expected=background!=Some("1") && match media {
                        Some("1")=>true,None=>generation!=Some("1"),_=>false,
                    };
                    assert_eq!(actual,expected,"{background:?}/{generation:?}/{media:?}");
                }
            }
        }
    }
    #[test]
    fn early_media_first_comment_is_durable_and_many_comments_do_not_duplicate_work() {
        let mut d=fixture();d["items"].as_array_mut().unwrap().truncate(1);
        let preserved=json!([d["items"],d["proposals"],d["approvals"],d["operations"]]);
        reconcile_with_cutoff(&mut d,AT,true,&None).unwrap();
        assert_eq!(rows(&d,"jobs").len(),1);assert_eq!(d["jobs"][0]["kind"],"media");
        assert_eq!(d["jobs"][0]["status"],"queued");assert!(rows(&d["jobs"][0],"sourceAttempts").is_empty());
        assert_eq!(json!([d["items"],d["proposals"],d["approvals"],d["operations"]]),preserved);
        let original=d["jobs"][0].clone();
        for n in 0..100 {let mut item=d["items"][0].clone();item["id"]=json!(format!("arrival-{n}"));
            item["itemId"]=json!(format!("recipient-{n}"));crate::list_mut(&mut d,"items").push(item);}
        reconcile_with_cutoff(&mut d,AT,true,&None).unwrap();assert_eq!(rows(&d,"jobs").len(),1);
        assert_eq!(d["jobs"][0],original);
        let mut restarted:Value=serde_json::from_str(&d.to_string()).unwrap();
        recover(&mut restarted,AT).unwrap();reconcile_with_cutoff(&mut restarted,AT,true,&None).unwrap();
        assert_eq!(rows(&restarted,"jobs").len(),1);assert_eq!(restarted["jobs"][0]["id"],original["id"]);
    }
    #[test]
    fn early_media_known_admitted_evidence_reuses_before_download_without_reply_work() {
        let mut d=fixture();d["items"].as_array_mut().unwrap().truncate(1);
        d["materials"]=json!([{"id":"already","kind":"transcript","postKey":"11391:one","text":"Existing transcript"}]);
        add_visual_fixture(&mut d);crate::knowledge::sync_catalog(&mut d,AT).unwrap();
        let before=d["items"].clone();
        reconcile_with_cutoff(&mut d,AT,true,&None).unwrap();
        assert!(rows(&d,"jobs").is_empty());assert_eq!(d["items"],before);
        assert!(rows(&d,"proposals").is_empty());assert!(rows(&d,"approvals").is_empty());
        assert!(rows(&d,"operations").is_empty());
    }
    #[test]
    fn early_media_cutoff_is_inclusive_strict_and_reconciles_changed_arrival_time() {
        let cutoff=parse_comment_cutoff(Some("2026-09-27T16:00:00Z")).unwrap();
        assert_eq!(cutoff_unix_with(Some("2026-09-27T16:00:00Z")).unwrap(),Some(1790524800));
        for raw in ["","tomorrow","2026-09-27","2026-09-27T16:00:00"," 2026-09-27T16:00:00Z","2026-09-27T16:00:00.001Z"] {
            assert!(parse_comment_cutoff(Some(raw)).is_err(),"{raw}");
        }
        for created in [json!(null),json!("not-a-date"),json!("2026-09-27T16:00:00.001Z")] {
            let mut d=fixture();d["items"].as_array_mut().unwrap().truncate(1);d["items"][0]["createdAt"]=created;
            reconcile_with_cutoff(&mut d,AT,false,&cutoff).unwrap();assert!(rows(&d,"jobs").is_empty());
            // No wait for a new minute or an unrelated field: the arrival time
            // and cutoff are part of the durable reconciliation input digest.
            d["items"][0]["createdAt"]=json!("2026-09-27T16:00:00Z");
            reconcile_with_cutoff(&mut d,AT,false,&cutoff).unwrap();assert_eq!(rows(&d,"jobs").len(),1);
        }
    }
    #[test]
    fn early_media_foreign_comment_post_and_closed_comment_never_admit_a_source() {
        for failure in ["foreign-comment","foreign-post","closed"] {
            let mut d=fixture();d["items"].as_array_mut().unwrap().truncate(1);
            let foreign=crate::accounts::Profile::BawRussia.binding();
            match failure {
                "foreign-comment"=>d["items"][0]["connectorBinding"]=foreign,
                "foreign-post"=>d["posts"][0]["connectorBinding"]=foreign,
                _=>d["items"][0]["workflow"]=json!("closed"),
            }
            reconcile_with_cutoff(&mut d,AT,true,&None).unwrap();assert!(rows(&d,"jobs").is_empty(),"{failure}");
        }
    }
    fn legacy_bound_checkpoint()->(Value,String){
        let mut d=fixture();d["posts"].as_array_mut().unwrap().truncate(1);
        let (id,_)=claim(&mut d,AT).unwrap().unwrap();
        let job=crate::row_mut(&mut d,"jobs",&id).unwrap();job.as_object_mut().unwrap().remove("connectorBinding");
        job["status"]=json!("queued");
        let p=&mut job["result"]["visualProgress"];p["phase"]=json!("scan");p["leaseId"]=Value::Null;
        p["source"]=json!({"sha256":"a".repeat(64),"bytes":12});
        p["sourceIdentity"]=json!({"account":p["account"],"postKey":p["sourcePostKey"],"mediaSha256":"a".repeat(64),"durationMs":120000});
        p["inventoryDescriptor"]=json!({"sha256":"b".repeat(64),"bytes":34});
        p["selectionDescriptor"]=json!({"sha256":"c".repeat(64),"bytes":56});
        p["latestReceipt"]=json!({"sha256":"d".repeat(64),"bytes":78});
        p["nextSelectionIndex"]=json!(24);p["completedSelectedFrames"]=json!(24);
        for attempt in crate::list_mut(job,"sourceAttempts"){attempt["status"]=json!("failed");attempt["error"]=json!("source_download_failed");}
        // Full fixture persistence requires these collections even when empty.
        for key in ["knowledge_entries","knowledge_versions","feedback"]{if d.get(key).is_none(){d[key]=json!([]);}}
        (d,id)
    }
    /// Replace only the synthetic workspace content under the test App's actual
    /// admitted native writer; retain its initialized lifecycle and owner epoch.
    /// This is fixture setup, not another startup owner or a guard bypass.
    async fn seed_current_owner_media_fixture(app:&crate::App,fixture:Value) {
        let token=crate::runtime_maintenance::admission_token(app,crate::runtime_lifecycle::AdmissionClass::Media).await.unwrap();
        crate::runtime_lifecycle::require_runtime_owner(&token,&app.lifecycle_owner).unwrap();
        let lifecycle=app.change(|state| {
            assert_eq!(fixture["account"],state["account"],"fixture must keep the native company's account");
            let lifecycle=state["runtimeLifecycle"].clone();assert!(!lifecycle.is_null(),"native test App must already own its lifecycle");
            *state=fixture;state["runtimeLifecycle"]=lifecycle.clone();
            crate::runtime_lifecycle::require_admission(state,&token,crate::runtime_lifecycle::AdmissionClass::Media)?;
            Ok(lifecycle)
        }).await.unwrap();
        assert_eq!(app.read().await.unwrap()["runtimeLifecycle"],lifecycle,"fixture seed must retain the exact native owner and epoch");
    }
    #[tokio::test]
    async fn legacy_binding_claim_persists_and_passes_unchanged_gpu_guard(){
        for kind in ["absent","null","current"]{
            let (mut d,id)=legacy_bound_checkpoint();
            if kind=="null"{d["jobs"][0]["connectorBinding"]=Value::Null;}
            if kind=="current"{d["jobs"][0]["connectorBinding"]=d["connectorBinding"].clone();}
            let before=d["jobs"][0].clone();let (app,_temp)=crate::tests::test_app().await;
            seed_current_owner_media_fixture(&app,d.clone()).await;
            let claim=app.change_media(|state|claim_selected_ready(state,AT,false,None,&|_|Ok(()))).await.unwrap().unwrap();assert_eq!(claim.0,id);
            let after=app.read().await.unwrap();let job=crate::row(&after,"jobs",&id).unwrap();
            assert_eq!(job["connectorBinding"],after["connectorBinding"]);assert_eq!(job["sourceAttempts"],before["sourceAttempts"]);
            for field in ["source","sourceIdentity","inventoryDescriptor","selectionDescriptor","latestReceipt","nextSelectionIndex","completedSelectedFrames"]{assert_eq!(job["result"]["visualProgress"][field],before["result"]["visualProgress"][field],"{field}");}
            crate::media_processing::validate_gpu_job_for_test(&app,&id,&job["result"]["visualProgress"]).await.unwrap();
            app.db.close().await;
        }
    }
    #[tokio::test]
    async fn legacy_binding_claim_retains_pre_reconciliation_scheduler_watermark(){
        let (mut d,id)=legacy_bound_checkpoint();
        let post=json!({"id":"watermark-post","postKey":"11391:watermark","objectId":"11391","title":"Independent recovered source","channel":"VK","attachments":[{"type":"video"}]});
        crate::list_mut(&mut d,"posts").push(post.clone());
        select_visual_fixture(&mut d);
        crate::list_mut(&mut d,"materials").push(json!({"id":"watermark-audio","kind":"transcript","postKey":post["postKey"],"text":"Recovered source transcript"}));
        add_visual_fixture(&mut d);crate::knowledge::sync_catalog(&mut d,AT).unwrap();
        let key=group(&d,&post).unwrap();let binding=d["connectorBinding"].clone();let account=d["account"].clone();
        crate::list_mut(&mut d,"jobs").push(json!({"id":"watermark-job","kind":"media","purpose":PURPOSE,"visualContractVersion":2,"account":account,"connectorBinding":binding,"groupKey":key,"refId":post["id"],"status":"failed","manualRequested":true,"sourceAttempts":[],"result":{"mediaSchedulerTurn":99}}));
        let mut preview=d.clone();reconcile_scoped(&mut preview,AT,false).unwrap();
        assert_eq!(crate::row(&preview,"jobs","watermark-job").unwrap()["status"],"completed");
        assert!(crate::row(&preview,"jobs","watermark-job").unwrap()["result"]["mediaSchedulerTurn"].is_null(),"fixture must exercise reconcile dropping the older watermark");
        let (app,_temp)=crate::tests::test_app().await;seed_current_owner_media_fixture(&app,d.clone()).await;
        assert_eq!(app.change_media(|state|claim_selected_ready(state,AT,false,None,&|_|Ok(()))).await.unwrap().unwrap().0,id);
        let after=app.read().await.unwrap();assert_eq!(crate::row(&after,"jobs",&id).unwrap()["result"]["mediaSchedulerTurn"],100);
        app.db.close().await;
    }
    #[tokio::test]
    async fn legacy_binding_scope_rejects_unverified_stamp_and_hostile_claim(){
        for case in ["foreign","malformed","account","post_account","post_binding","source","post_key","progress_binding","material","policy","cancelled","held","epoch"]{
            let (mut d,id)=legacy_bound_checkpoint();
            let post_key=d["posts"][0]["postKey"].clone();
            match case {
                "foreign"=>d["jobs"][0]["connectorBinding"]=json!({"foreign":true}),
                "malformed"=>d["jobs"][0]["connectorBinding"]=json!("bad"),
                "account"=>d["jobs"][0]["account"]=json!("BAW Russia"),
                "post_account"=>d["posts"][0]["account"]=json!("BAW Russia"),
                "post_binding"=>d["posts"][0]["connectorBinding"]=json!({"foreign":true}),
                "source"=>d["posts"][0]["title"]=json!("Replacement source"),
                "post_key"=>d["jobs"][0]["result"]["visualProgress"]["sourcePostKey"]=json!("other"),
                "progress_binding"=>d["jobs"][0]["result"]["visualProgress"]["connectorBinding"]=Value::Null,
                "material"=>{
                    let material=json!({"id":"new-head-material","kind":"transcript","account":d["account"],"postKey":post_key,"text":"New recorded source observation"});
                    crate::list_mut(&mut d,"materials").push(material);crate::knowledge::sync_catalog(&mut d,AT).unwrap();
                    assert_ne!(material_epoch(&d,&d["posts"][0]),d["jobs"][0]["result"]["visualProgress"]["materialEpoch"],"fixture must change a real catalog head");
                },
                "policy"=>{let p=d["jobs"][0]["result"]["visualProgress"].clone();d["settings"]["postMediaPolicies"][text(&p,"sourcePostId")]=json!({"version":1,"revision":1,"status":"active","postId":p["sourcePostId"],"account":p["account"],"connectorBinding":p["connectorBinding"],"sourceVersion":p["sourceVersion"],"mode":"full_audio_only"});},
                "cancelled"=>d["jobs"][0]["status"]=json!("cancelled"),
                "held"=>{d["jobs"][0]["status"]=json!("failed");d["jobs"][0]["result"]["visualProgress"]["phase"]=json!("held");},
                _=>d["jobs"][0]["result"]["visualProgress"]["leaseEpoch"]=json!(u64::MAX),
            }
            let (app,_temp)=crate::tests::test_app().await;seed_current_owner_media_fixture(&app,d.clone()).await;
            let before=app.read().await.unwrap();
            assert!(app.change_media(|state|{let binding=state["connectorBinding"].clone();crate::row_mut(state,"jobs",&id)?["connectorBinding"]=binding;Ok(())}).await.is_err(),"unverified stamp {case}");
            assert_eq!(app.read().await.unwrap(),before,"rejected transaction rolls back {case}");
            let epoch=d["jobs"][0]["result"]["visualProgress"]["leaseEpoch"].as_u64().unwrap();let target=ResumeTarget{job_id:id.clone(),fence:ResumeFence::VisualEpoch(epoch)};
            let result=app.change_media(|state|claim_selected_ready(state,AT,false,Some(&target),&|_|Ok(()))).await.unwrap();
            assert!(result.is_none(),"{case}");let after=app.read().await.unwrap();
            let job=crate::row(&after,"jobs",&id).unwrap();assert_eq!(job.get("connectorBinding"),before["jobs"][0].get("connectorBinding"),"{case}");
            assert_eq!(job["sourceAttempts"],before["jobs"][0]["sourceAttempts"],"{case}");
            app.db.close().await;
        }
    }
    #[test]
    fn legacy_binding_owner_resume_retains_checkpoint_and_rejects_wrong_recovery(){
        let (mut d,id)=legacy_bound_checkpoint();let job=crate::row_mut(&mut d,"jobs",&id).unwrap();
        job["status"]=json!("running");crate::media_fullframes::finish(job,&Err(crate::conflict("gpu_gate_job_source_changed")),AT);
        let held=d.clone();let epoch=d["jobs"][0]["result"]["visualProgress"]["leaseEpoch"].as_u64().unwrap();
        assert!(claim_selected_ready(&mut d,AT,false,None,&|_|Ok(())).unwrap().is_none());
        assert!(d["jobs"][0]["connectorBinding"].is_null());
        let receipt=authorize_visual_resume(&mut d,&id,epoch).unwrap();assert_eq!(receipt["status"],"queued");
        assert!(d["jobs"][0]["connectorBinding"].is_null(),"only actual admission stamps binding");
        let target=ResumeTarget::from_receipt(&receipt).unwrap();claim_selected_ready(&mut d,AT,false,Some(&target),&|_|Ok(())).unwrap().unwrap();
        assert_eq!(d["jobs"][0]["sourceAttempts"],held["jobs"][0]["sourceAttempts"]);
        assert_eq!(d["jobs"][0]["result"]["visualProgress"]["phase"],"scan");
        for field in ["source","inventoryDescriptor","selectionDescriptor","latestReceipt","nextSelectionIndex"]{assert_eq!(d["jobs"][0]["result"]["visualProgress"][field],held["jobs"][0]["result"]["visualProgress"][field]);}
        for case in ["foreign","source","cancelled","epoch","download","missing_source"]{
            let mut bad=held.clone();match case {
                "foreign"=>bad["jobs"][0]["connectorBinding"]=json!({"foreign":true}),
                "source"=>bad["posts"][0]["title"]=json!("Changed source"),
                "cancelled"=>bad["jobs"][0]["status"]=json!("cancelled"),
                "epoch"=>bad["jobs"][0]["result"]["visualProgress"]["leaseEpoch"]=json!(u64::MAX),
                "download"=>bad["jobs"][0]["result"]["visualProgress"]["resumePhase"]=json!("download"),
                _=>bad["jobs"][0]["result"]["visualProgress"]["source"]=Value::Null,
            }
            let before=bad.clone();assert!(authorize_visual_resume(&mut bad,&id,epoch).is_err(),"{case}");assert_eq!(bad,before,"{case}");
        }
    }
    #[test]
    fn legacy_binding_recovery_rejects_bad_epoch_before_any_mutation(){
        for epoch in [Value::Null,json!("1"),json!(-1),json!(u64::MAX)]{
            let (mut d,_)=legacy_bound_checkpoint();d["jobs"][0]["status"]=json!("interrupted");d["jobs"][0]["result"]["visualProgress"]["leaseId"]=json!("old-lease");d["jobs"][0]["result"]["visualProgress"]["leaseEpoch"]=epoch;
            let before=d.clone();assert!(recover(&mut d,AT).is_err());assert_eq!(d,before);
        }
    }
    #[test]
    fn policy_reselection_retains_download_attempt_and_roots_old_receipts(){
        let mut d=fixture();d["posts"].as_array_mut().unwrap().truncate(1);let (id,_)=claim(&mut d,AT).unwrap().unwrap();
        let job=super::super::row_mut(&mut d,"jobs",&id).unwrap();
        let p=&mut job["result"]["visualProgress"];
        p["phase"]=json!("scan");p["source"]=json!({"sha256":"a".repeat(64),"bytes":12});
        p["inventoryDescriptor"]=json!({"sha256":"b".repeat(64),"bytes":34});
        p["selectionDescriptor"]=json!({"sha256":"c".repeat(64),"bytes":56});
        p["latestReceipt"]=json!({"sha256":"d".repeat(64),"bytes":78});
        p["nextSelectionIndex"]=json!(24);p["completedSelectedFrames"]=json!(24);
        let expected=p.clone();let attempts=job["sourceAttempts"].clone();
        let result=reselect_progress(&mut d,&id,&expected,AT).unwrap();
        let current=super::super::row(&d,"jobs",&id).unwrap();
        let next=current["result"]["visualProgress"].clone();
        assert_eq!(next["phase"],"select");assert_eq!(next["source"],expected["source"]);
        assert_eq!(next["inventoryDescriptor"],expected["inventoryDescriptor"]);
        assert!(next["latestReceipt"].is_null());assert_eq!(next["nextSelectionIndex"],0);
        assert_eq!(current["sourceAttempts"],attempts);
        let history=super::super::row(&d,"jobs",current["selectionPolicyHistoryIds"][0].as_str().unwrap()).unwrap();
        assert_eq!(history["kind"],"media_policy_history");assert_eq!(history["status"],"completed");
        assert_eq!(history["result"]["visualProgress"],expected);
        let before=d.clone();assert!(reselect_progress(&mut d,&id,&expected,AT).is_err());assert_eq!(d,before);
        assert!(super::super::media_fullframes::finish(super::super::row_mut(&mut d,"jobs",&id).unwrap(),&Ok(result),AT));
        let (resumed_id,_) = claim(&mut d,AT).unwrap().unwrap();assert_eq!(resumed_id,id);
        let resumed=super::super::row(&d,"jobs",&id).unwrap();
        assert_eq!(resumed["sourceAttempts"],attempts);
        assert_eq!(resumed["result"]["visualProgress"]["phase"],"select");
    }
    #[test]
    fn policy_reselection_rejects_changed_source_material_and_lease(){
        let mut d=fixture();let (id,_)=claim(&mut d,AT).unwrap().unwrap();
        super::super::row_mut(&mut d,"jobs",&id).unwrap()["result"]["visualProgress"]["phase"]=json!("scan");
        let expected=super::super::row(&d,"jobs",&id).unwrap()["result"]["visualProgress"].clone();
        for drift in ["source","material","lease","account"] {
            let mut changed=d.clone();
            match drift {
                "source"=>super::super::row_mut(&mut changed,"posts",text(&expected,"sourcePostId")).unwrap()["title"]=json!("Changed"),
                "material"=>super::super::list_mut(&mut changed,"knowledge_entries").push(json!({"id":"fresh","kind":"transcript","currentVersionId":"v2","scope":{"postKeys":[expected["sourcePostKey"]]}})),
                "lease"=>super::super::row_mut(&mut changed,"jobs",&id).unwrap()["result"]["visualProgress"]["leaseId"]=json!("other"),
                _=>changed["account"]=json!("another-company"),
            }
            let before=changed.clone();assert!(reselect_progress(&mut changed,&id,&expected,AT).is_err(),"{drift}");assert_eq!(changed,before);
        }
    }
    fn fixture_for(profile: super::super::accounts::Profile) -> Value {
        let mut d = super::super::empty();
        for key in ["knowledge_entries","knowledge_versions","feedback"]{d[key]=json!([]);}
        super::super::accounts::initialize(&mut d,profile).unwrap();
        for (id, channel, object) in [("post-11391:one", "VK", "11391"), ("post-11390:two", "YouTube", "11390")] {
            super::super::list_mut(&mut d, "posts").push(json!({"id":id,"postKey":&id[5..],"objectId":object,"title":"Обзор семейного автомобиля","canonicalMediaId":"fixture-exact-video","channel":channel,"attachments":[{"type":"video"}]}));
            super::super::list_mut(&mut d, "items").push(json!({"id":format!("item-{object}-one"),"itemId":"one","objectId":object,"postId":id,"postKey":&id[5..],"conversationKey":format!("{object}:thread"),"providerStatus":"new","workflow":"attention"}));
        }
        select_visual_fixture(&mut d);
        add_visual_fixture(&mut d);
        super::super::knowledge::sync_catalog(&mut d, AT).unwrap();
        d
    }
    // Frame/lease cases must explicitly choose visual acquisition after all
    // source metadata in their setup is final; stale overrides never reactivate.
    fn select_visual_fixture(d:&mut Value){
        d["settings"]["postMediaPolicies"]=json!({});
        for post in d["posts"].as_array().unwrap().clone(){
            d["settings"]["postMediaPolicies"][text(&post,"id")]=json!({"version":1,"revision":1,
                "status":"active","postId":post["id"],"account":d["account"],
                "connectorBinding":super::super::active_binding(&d).unwrap().to_json(),
                "sourceVersion":super::super::media_fullframes::source_version(&post,text(&d,"account")),
                "mode":"full_audio_visual"});
        }
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
        assert!(preparation_state(&d,&item,AT).unwrap().is_some(),"legacy audio remains unproven even after visual work");
    }
    fn processed_only_fixture()->Value{
        let mut d=fixture();
        d["posts"].as_array_mut().unwrap().truncate(1);
        d["materials"]=json!([]);d["knowledge_entries"]=json!([]);d["knowledge_versions"]=json!([]);
        let post=d["posts"][0].clone();
        // Actual LA shape: no purpose/contract/evidence/transcription coverage.
        d["jobs"]=json!([{"id":"legacy-processed","kind":"media","refId":post["id"],
            "status":"completed","result":{"processed":true},"sourceAttempts":[]}]);d
    }
    #[test]
    fn preparation_accepts_proven_full_audio_without_frames_but_keeps_exact_visual_override(){
        let mut base=fixture();
        base["settings"]["postMediaPolicies"]=json!({});
        let post=base["posts"][0].clone();
        let item=base["items"][0].clone();
        let source=super::super::media_fullframes::source_version(&post,"LikeAvto");
        for coverage in ["missing","partial","unknown","full"]{
            let mut d=base.clone();
            if coverage!="missing"{
                d["materials"]=json!([{"id":"speech","account":"LikeAvto","postKey":post["postKey"],
                    "kind":"transcript","text":"Complete spoken source",
                    "transcription":{"partial":coverage=="partial","coverage":if coverage=="unknown"{"unknown"}else{"full_audio"},
                        "sourceVersion":source,"mediaDurationSeconds":120.0,"audioDurationSeconds":120.0}}]);
                super::super::knowledge::sync_catalog(&mut d,AT).unwrap();
            }
            assert_eq!(preparation_state(&d,&item,AT).unwrap().is_none(),coverage=="full","{coverage}");
            if coverage=="full"{
                let binding=super::super::active_binding(&d).unwrap().to_json();
                d["settings"]["postMediaPolicies"]=json!({(text(&post,"id")):{"version":1,"revision":1,
                    "status":"active","postId":post["id"],"account":"LikeAvto",
                    "connectorBinding":binding,"sourceVersion":source,"mode":"full_audio_visual"}});
                assert!(preparation_state(&d,&item,AT).unwrap().is_some(),"exact visual override must still hold");
                d["settings"]["postMediaPolicies"][text(&post,"id")]["sourceVersion"]=json!("stale source");
                assert_eq!(preparation_state(&d,&item,AT).unwrap(),None,"stale override must not apply");
                d["posts"][0]["title"]=json!("New source identity");
                assert!(preparation_state(&d,&item,AT).unwrap().is_some(),"old audio cannot retarget new source");
            }
        }
    }
    #[test]
    fn legacy_missing_stage_selected_admission_preserves_history_and_initial_budget(){
        let mut d=processed_only_fixture();let post=d["posts"][0].clone();
        d["jobs"][0]["sourceAttempts"]=json!([{"postId":post["id"],"postKey":post["postKey"],"status":"completed","sourceVersion":"0".repeat(64),"attemptNumber":7}]);
        let old=d["jobs"][0].clone();
        assert!(!has_required_media(&d,&post,AT).unwrap());assert!(attempted(&d,&post));
        let admitted=enqueue_selected(&mut d,&post,AT,true).unwrap();assert_eq!(admitted["status"],"queued");
        let id=admitted["jobId"].as_str().unwrap();let job=super::super::row(&d,"jobs",id).unwrap();
        assert_eq!(job["legacyMissingStages"]["sourceVersion"],super::super::media_fullframes::source_version(&post,text(&d,"account")));
        assert_eq!(job["legacyMissingStages"]["legacyJobs"][0]["jobId"],old["id"]);
        let again=enqueue_selected(&mut d,&post,AT,true).unwrap();assert_eq!(again["jobId"],admitted["jobId"]);
        let (claimed,selected)=claim_selected_ready(&mut d,AT,false,None,&|_|Ok(())).unwrap().unwrap();
        assert_eq!(claimed,id);assert_eq!(selected["id"],post["id"]);assert_eq!(d["jobs"][0],old);
        let attempts=rows(super::super::row(&d,"jobs",id).unwrap(),"sourceAttempts");
        assert_eq!(attempts.len(),1);assert_eq!(attempts[0]["attemptNumber"],1);assert!(attempts[0]["retryOf"].is_null());
        assert!(!has_required_media(&d,&post,AT).unwrap(),"a queue receipt never proves full audio or visual readiness");
    }
    #[test]
    fn legacy_missing_stage_unknown_failed_foreign_and_nonowner_are_held(){
        for case in ["running","unknown","dispatching","failed","cancelled","interrupted","account","connector","attempt","nonowner"]{
            let mut d=processed_only_fixture();let post=d["posts"][0].clone();
            match case {
                "account"=>d["jobs"][0]["account"]=json!("baw-russia"),
                "connector"=>d["jobs"][0]["connectorBinding"]=json!({"accountId":"other"}),
                "attempt"=>d["jobs"][0]["sourceAttempts"]=json!([{"postId":post["id"],"status":"unknown"}]),
                "nonowner"=>{},_=>d["jobs"][0]["status"]=json!(case),
            }
            let before=d.clone();assert!(enqueue_selected(&mut d,&post,AT,case!="nonowner").is_err(),"{case}");assert_eq!(d,before);
        }
    }
    #[test]
    fn legacy_missing_stage_background_never_promotes_and_selected_source_never_falls_back(){
        let mut d=processed_only_fixture();let post=d["posts"][0].clone();let old=d["jobs"][0].clone();
        let automatic=enqueue(&mut d,&post,AT,false).unwrap();assert_eq!(automatic["status"],"failed");
        let automatic_id=automatic["jobId"].as_str().unwrap();
        let automatic_job=super::super::row(&d,"jobs",automatic_id).unwrap().clone();
        assert!(automatic_job["legacyMissingStages"].is_null());assert!(rows(&automatic_job,"sourceAttempts").is_empty());
        let admitted=enqueue_selected(&mut d,&post,AT,true).unwrap();let id=admitted["jobId"].as_str().unwrap();
        assert_eq!(admitted["status"],"queued");assert_ne!(id,automatic_id);
        let peer=fixture()["posts"][1].clone();super::super::list_mut(&mut d,"posts").push(peer.clone());
        let job=super::super::row(&d,"jobs",id).unwrap().clone();
        assert!(job["legacyMissingStages"].is_object());
        assert!(QueueIndex::new(&d).posts(text(&job,"groupKey")).iter().any(|p|p["id"]==peer["id"]),"fixture must offer a proven identity fallback");
        assert!(!attempted(&d,&peer),"independent peer source must remain eligible");
        assert_eq!(next_source(&d,&job).unwrap()["id"],post["id"]);
        // The selected job's own failed source exhausts its initial budget.
        // A still-eligible title peer cannot replace this exact source.
        let failed=json!({"id":"bounded-attempt","postId":post["id"],"postKey":post["postKey"],
            "sourceKey":super::super::knowledge::media_source_key(&post,text(&d,"account")),
            "sourceVersion":super::super::media_fullframes::source_version(&post,text(&d,"account")),
            "status":"failed","attemptNumber":1,"error":"source_download_failed"});
        let stored=super::super::row_mut(&mut d,"jobs",id).unwrap();
        stored["sourceAttempts"]=json!([failed]);stored["fallbackAllowed"]=json!(true);stored["status"]=json!("failed");
        let job=stored.clone();assert!(legacy_upgrade_current(&d,&job,&post),"refusal must come from exhausted source budget, not a malformed fixture");
        assert!(!attempted(&d,&peer));assert!(next_source(&d,&job).is_none());
        assert_eq!(super::super::row(&d,"jobs",automatic_id).unwrap(),&automatic_job);assert_eq!(d["jobs"][0],old);
    }
    #[test]
    fn legacy_missing_stage_current_v2_failure_cannot_reset_attempt_budget(){
        let mut d=processed_only_fixture();let post=d["posts"][0].clone();
        let failure=json!({"id":"spent","kind":"media","purpose":PURPOSE,"visualContractVersion":2,
            "account":d["account"],"connectorBinding":super::super::active_binding(&d).unwrap().to_json(),
            "sourceAttempts":[{"postId":post["id"],"postKey":post["postKey"],"status":"failed"}]});
        d["jobs"].as_array_mut().unwrap().push(failure);
        let before=d.clone();assert!(enqueue_selected(&mut d,&post,AT,true).is_err());assert_eq!(d,before);
    }
    #[test]
    fn legacy_missing_stage_opaque_current_v2_unknown_without_attempts_still_blocks(){
        let mut d=processed_only_fixture();let post=d["posts"][0].clone();
        let unknown=json!({"id":"unknown-v2","kind":"media","purpose":PURPOSE,"visualContractVersion":2,
            "account":d["account"],"connectorBinding":super::super::active_binding(&d).unwrap().to_json(),
            "refId":post["id"],"status":"unknown","sourceAttempts":[]});
        d["jobs"].as_array_mut().unwrap().push(unknown);
        let before=d.clone();assert!(enqueue_selected(&mut d,&post,AT,true).is_err());assert_eq!(d,before);
    }
    #[test]
    fn legacy_missing_stage_peer_ownership_after_admission_fences_claim_resume_and_merge(){
        for status in ["unknown","dispatching","running","queued","interrupted","failed"]{
            let mut d=processed_only_fixture();let post=d["posts"][0].clone();let old=d["jobs"][0].clone();
            let accepted=enqueue_selected(&mut d,&post,AT,true).unwrap();let id=accepted["jobId"].as_str().unwrap();
            let peer=json!({"id":"late-peer","kind":"media","purpose":PURPOSE,"visualContractVersion":2,
                "account":d["account"],"connectorBinding":super::super::active_binding(&d).unwrap().to_json(),
                "refId":post["id"],"status":status,"sourceAttempts":[]});
            super::super::list_mut(&mut d,"jobs").push(peer);
            let job=super::super::row(&d,"jobs",id).unwrap();
            assert!(!legacy_upgrade_current(&d,job,&post),"{status}");assert!(next_source(&d,job).is_none(),"{status}");
            // Shared validator also guards resumed checkpoints, worker start and
            // final merge, where the original attempt is already consumed.
            let mut resumed=job.clone();resumed["result"]=json!({"visualProgress":{"schemaVersion":2,"phase":"scan"}});
            assert!(!legacy_upgrade_current(&d,&resumed,&post),"{status}");
            assert!(claim_selected_ready(&mut d,AT,false,None,&|_|Ok(())).unwrap().is_none(),"{status}");
            assert!(rows(super::super::row(&d,"jobs",id).unwrap(),"sourceAttempts").is_empty());assert_eq!(d["jobs"][0],old);
        }
        let mut d=processed_only_fixture();let post=d["posts"][0].clone();
        let accepted=enqueue_selected(&mut d,&post,AT,true).unwrap();let id=accepted["jobId"].as_str().unwrap();
        let mut progress=super::super::media_fullframes::initial(text(&d,"account"),&super::super::active_binding(&d).unwrap().to_json(),&post,AT);
        progress["phase"]=json!("inventory");progress["materialEpoch"]=json!(material_epoch(&d,&post));progress["leaseId"]=Value::Null;
        let attempts=json!([{"postId":post["id"],"postKey":post["postKey"],"status":"running","attemptNumber":1,
            "sourceKey":super::super::knowledge::media_source_key(&post,text(&d,"account")),
            "sourceVersion":super::super::media_fullframes::source_version(&post,text(&d,"account"))}]);
        let stored=super::super::row_mut(&mut d,"jobs",id).unwrap();stored["result"]=json!({"visualProgress":progress});stored["sourceAttempts"]=attempts.clone();
        assert!(legacy_upgrade_current(&d,super::super::row(&d,"jobs",id).unwrap(),&post),"resume fixture must be admissible before UNKNOWN peer appears");
        let peer=json!({"id":"unknown-after-checkpoint","kind":"media","purpose":PURPOSE,"visualContractVersion":2,
            "account":d["account"],"connectorBinding":super::super::active_binding(&d).unwrap().to_json(),
            "refId":post["id"],"status":"unknown","sourceAttempts":[]});super::super::list_mut(&mut d,"jobs").push(peer);
        assert!(claim_selected_ready(&mut d,AT,false,None,&|_|Ok(())).unwrap().is_none());
        assert_eq!(super::super::row(&d,"jobs",id).unwrap()["sourceAttempts"],attempts,"resume must preserve consumed attempt");
        assert_eq!(super::super::row(&d,"jobs",id).unwrap()["error"],"media_legacy_upgrade_binding_changed");
        let mut legacy_peer=d["jobs"].as_array().unwrap().last().unwrap().clone();legacy_peer.as_object_mut().unwrap().remove("visualContractVersion");
        d["jobs"].as_array_mut().unwrap().pop();super::super::list_mut(&mut d,"jobs").push(legacy_peer);
        assert!(!legacy_upgrade_current(&d,super::super::row(&d,"jobs",id).unwrap(),&post),"an unresolved auto_media V1 peer is also ownership, not absent history");
    }
    #[test]
    fn legacy_missing_stage_initial_admission_and_persisted_identity_are_distinct(){
        let mut d=processed_only_fixture();let post=d["posts"][0].clone();
        let pin=legacy_upgrade_pin(&d,&post,true).unwrap().unwrap();
        let proposed=json!({"id":"proposed-job","kind":"media","purpose":PURPOSE,"visualContractVersion":2,
            "account":d["account"],"connectorBinding":super::super::active_binding(&d).unwrap().to_json(),
            "refId":post["id"],"manualRequested":true,"sourceAttempts":[],"legacyMissingStages":pin});
        assert!(legacy_upgrade_proposed(&d,&proposed,&post));assert!(!legacy_upgrade_current(&d,&proposed,&post));
        super::super::list_mut(&mut d,"jobs").push(proposed.clone());
        assert!(legacy_upgrade_current(&d,&proposed,&post));assert!(!legacy_upgrade_proposed(&d,&proposed,&post));
        super::super::list_mut(&mut d,"jobs").push(proposed.clone());
        assert!(!legacy_upgrade_current(&d,&proposed,&post));assert!(!legacy_upgrade_proposed(&d,&proposed,&post));
        let mut fresh=processed_only_fixture();let receipt=enqueue_selected(&mut fresh,&post,AT,true).unwrap();
        assert_eq!(receipt["status"],"queued");assert_eq!(rows(&fresh,"jobs").len(),2);
        let job=super::super::row(&fresh,"jobs",receipt["jobId"].as_str().unwrap()).unwrap();
        assert!(legacy_upgrade_current(&fresh,job,&post));assert!(rows(job,"sourceAttempts").is_empty());
    }
    #[test]
    fn legacy_missing_stage_stale_unknown_peer_after_admission_remains_quarantined(){
        for status in ["unknown","dispatching","running","interrupted","queued","paused"]{
            let mut d=processed_only_fixture();let post=d["posts"][0].clone();
            let accepted=enqueue_selected(&mut d,&post,AT,true).unwrap();let id=accepted["jobId"].as_str().unwrap();
            let peer=json!({"id":"stale-unresolved","kind":"media","purpose":PURPOSE,"visualContractVersion":2,
                "account":d["account"],"connectorBinding":super::super::active_binding(&d).unwrap().to_json(),
                "refId":post["id"],"status":status,"sourceAttempts":[],
                "result":{"visualProgress":{"schemaVersion":2,"sourcePostId":post["id"],
                    "sourceVersion":"0".repeat(64),"materialEpoch":"obsolete","phase":"download"}}});
            assert!(!current_source_job(&d,&peer),"fixture must exercise stale checkpoint branch");
            super::super::list_mut(&mut d,"jobs").push(peer);
            assert!(!legacy_upgrade_current(&d,super::super::row(&d,"jobs",id).unwrap(),&post),"{status}");
            assert!(claim_selected_ready(&mut d,AT,false,None,&|_|Ok(())).unwrap().is_none(),"{status}");
            assert!(rows(super::super::row(&d,"jobs",id).unwrap(),"sourceAttempts").is_empty());
        }
    }
    #[test]
    fn legacy_missing_stage_malformed_history_and_processed_result_are_not_empty(){
        for malformed in [json!({}),json!("bad"),json!(false),json!(3),json!([null]),
            json!([{"status":"completed","postId":3}]),json!([{"status":"completed","sourceVersion":false}]),
            json!([{"status":"completed","attemptNumber":0}])]{
            let mut d=processed_only_fixture();let post=d["posts"][0].clone();d["jobs"][0]["sourceAttempts"]=malformed;
            let before=d.clone();assert!(enqueue_selected(&mut d,&post,AT,true).is_err());assert_eq!(d,before);
        }
        for result in [json!({"processed":"true"}),json!({"processed":true,"unresolved":true}),json!([true])]{
            let mut d=processed_only_fixture();let post=d["posts"][0].clone();d["jobs"][0]["result"]=result;
            let before=d.clone();assert!(enqueue_selected(&mut d,&post,AT,true).is_err());assert_eq!(d,before);
        }
        for absent in [true,false]{
            let mut d=processed_only_fixture();let post=d["posts"][0].clone();
            if absent{d["jobs"][0].as_object_mut().unwrap().remove("sourceAttempts");}else{d["jobs"][0]["sourceAttempts"]=Value::Null;}
            assert_eq!(enqueue_selected(&mut d,&post,AT,true).unwrap()["status"],"queued");
        }
    }
    #[test]
    fn legacy_missing_stage_process_body_is_exact_and_typed(){
        assert_eq!(selected_post_id(&json!({"postId":"post-11390:two"})).unwrap(),"post-11390:two");
        for body in [Value::Null,json!([]),json!(true),json!({}),json!({"postId":null}),json!({"postId":3}),
            json!({"postId":""}),json!({"postId":" spaced "}),json!({"postId":"post-11390:two","retry":true})]{
            assert!(selected_post_id(&body).is_err());
        }
    }
    #[test]
    fn legacy_missing_stage_current_ledger_corruption_never_becomes_fresh_work(){
        for ledger in [Value::Null,json!({}),json!([null]),json!([{"status":"unknown"}]),
            json!([{"status":"running","postId":"other","attemptNumber":1}])]{
            let mut d=processed_only_fixture();let post=d["posts"][0].clone();
            let accepted=enqueue_selected(&mut d,&post,AT,true).unwrap();let id=accepted["jobId"].as_str().unwrap();
            super::super::row_mut(&mut d,"jobs",id).unwrap()["sourceAttempts"]=ledger.clone();
            assert!(!legacy_upgrade_current(&d,super::super::row(&d,"jobs",id).unwrap(),&post));
            assert!(claim_selected_ready(&mut d,AT,false,None,&|_|Ok(())).unwrap().is_none());
            assert_eq!(super::super::row(&d,"jobs",id).unwrap()["sourceAttempts"],ledger);
        }
    }
    #[test]
    fn legacy_missing_stage_preflight_spends_nothing_and_drift_blocks_claim(){
        let mut d=processed_only_fixture();let post=d["posts"][0].clone();
        let admitted=enqueue_selected(&mut d,&post,AT,true).unwrap();let id=admitted["jobId"].as_str().unwrap();
        assert!(claim_selected_ready(&mut d,AT,false,None,&|_|Err("media_runtime_not_ready".into())).unwrap().is_none());
        assert!(rows(super::super::row(&d,"jobs",id).unwrap(),"sourceAttempts").is_empty());
        for drift in ["source","post_key","account","owner_policy","defaults","material","legacy_attempt"]{
            let mut changed=d.clone();
            match drift {
                "source"=>changed["posts"][0]["title"]=json!("changed source metadata"),
                "post_key"=>changed["posts"][0]["postKey"]=json!("other-key"),
                "account"=>changed["account"]=json!("baw-russia"),
                "owner_policy"=>changed["settings"]["postMediaPolicies"]=json!({(text(&post,"id")): {"revision":99}}),
                "defaults"=>changed["settings"]["mediaPolicyDefaults"]=json!({"audioOnlyAboveSeconds":181}),
                "material"=>super::super::list_mut(&mut changed,"knowledge_entries").push(json!({"id":"fresh","kind":"transcript","currentVersionId":"v2","scope":{"postKeys":[post["postKey"]]}})),
                _=>changed["jobs"][0]["sourceAttempts"]=json!([{"postId":post["id"],"status":"unknown"}]),
            }
            let job=super::super::row(&changed,"jobs",id).unwrap();assert!(next_source(&changed,job).is_none(),"{drift}");
            assert!(!legacy_upgrade_current(&changed,job,&changed["posts"][0]),"{drift}");
        }
    }
    #[test]
    fn legacy_missing_stage_durable_restart_keeps_one_job_and_attempt(){
        let mut d=processed_only_fixture();let post=d["posts"][0].clone();
        let admitted=enqueue_selected(&mut d,&post,AT,true).unwrap();let id=admitted["jobId"].as_str().unwrap();
        let mut restarted:Value=serde_json::from_str(&d.to_string()).unwrap();
        assert_eq!(enqueue_selected(&mut restarted,&post,AT,true).unwrap()["jobId"],admitted["jobId"]);
        assert_eq!(rows(&restarted,"jobs").len(),2);
        claim_selected_ready(&mut restarted,AT,false,None,&|_|Ok(())).unwrap().unwrap();
        assert_eq!(rows(super::super::row(&restarted,"jobs",id).unwrap(),"sourceAttempts").len(),1);
    }
    #[test]
    fn legacy_missing_stage_colliding_company_ids_do_not_share_admission(){
        let mut like=processed_only_fixture();let post=like["posts"][0].clone();
        let admitted=enqueue_selected(&mut like,&post,AT,true).unwrap();
        let job=super::super::row(&like,"jobs",admitted["jobId"].as_str().unwrap()).unwrap().clone();
        let mut baw=super::super::empty();super::super::accounts::initialize(&mut baw,super::super::accounts::Profile::BawRussia).unwrap();
        let historical=processed_only_fixture();baw["posts"]=historical["posts"].clone();baw["items"]=historical["items"].clone();
        baw["jobs"]=historical["jobs"].clone();baw["materials"]=json!([]);baw["knowledge_entries"]=json!([]);baw["knowledge_versions"]=json!([]);
        assert!(!legacy_upgrade_current(&baw,&job,&post));
        let baw_receipt=enqueue_selected(&mut baw,&post,AT,true).unwrap();
        let baw_job=super::super::row(&baw,"jobs",baw_receipt["jobId"].as_str().unwrap()).unwrap();
        assert_ne!(job["legacyMissingStages"]["account"],baw_job["legacyMissingStages"]["account"]);
        assert_ne!(job["legacyMissingStages"]["connectorBinding"],baw_job["legacyMissingStages"]["connectorBinding"]);
        assert_eq!(like["jobs"][0],baw["jobs"][0],"identical historical IDs confer no cross-company authority");
    }
    #[test]
    fn legacy_missing_stage_owner_audio_only_policy_does_not_launch_visual_work(){
        let mut d=processed_only_fixture();let post=d["posts"][0].clone();
        let policy=json!({"version":1,"revision":1,"status":"active","mode":"full_audio_only",
            "account":d["account"],"connectorBinding":super::super::active_binding(&d).unwrap().to_json(),
            "postId":post["id"],"sourceVersion":super::super::media_fullframes::source_version(&post,text(&d,"account"))});
        d["settings"]["postMediaPolicies"]=json!({(text(&post,"id")):policy});
        let old=d["jobs"].clone();let held=enqueue_selected(&mut d,&post,AT,true).unwrap();
        assert_eq!(held["status"],"held");assert_eq!(held["mode"],"full_audio_only");
        assert_eq!(held["cachedAudioRequestRequired"],true);assert_eq!(d["jobs"],old);
        assert!(!has_required_media(&d,&post,AT).unwrap(),"unknown legacy coverage cannot prove full audio");
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
    #[test]
    fn stage_preflight_persists_bounded_reason_without_attempt_or_repeated_changes(){
        let mut d=fixture();d["posts"].as_array_mut().unwrap().truncate(1);
        let blocked=|_:&str|Err("media_config_missing_COMMUNITYHERO_MEDIA_YTDLP".into());
        assert!(claim_selected_ready(&mut d,AT,false,None,&blocked).unwrap().is_none());
        let job=&d["jobs"][0];assert_eq!(job["status"],"queued");assert!(rows(job,"sourceAttempts").is_empty());
        assert_eq!(job["workerBlock"],json!({"stage":"download","code":"media_config_missing_COMMUNITYHERO_MEDIA_YTDLP"}));
        let discovered=d.clone();
        assert!(claim_selected_ready(&mut d,AT,false,None,&blocked).unwrap().is_none());
        // The appended intent first persists; the next pass settles discovery.
        // Only its queue digest may change, never the job, reason or attempts.
        let mut settled=discovered;settled["mediaQueue"]=d["mediaQueue"].clone();
        assert_eq!(d,settled);assert_eq!(d["mediaQueue"],json!({"inputDigest":input_digest(&d,AT,false)}));
        let held=d.clone();assert!(claim_selected_ready(&mut d,AT,false,None,&blocked).unwrap().is_none());assert_eq!(d,held);
        let (id,_)=claim_selected_ready(&mut d,AT,false,None,&|phase|{assert_eq!(phase,"download");Ok(())}).unwrap().unwrap();
        let job=super::super::row(&d,"jobs",&id).unwrap();assert!(job["workerBlock"].is_null());assert_eq!(rows(job,"sourceAttempts").len(),1);
        let attempts=job["sourceAttempts"].clone();
        let job=super::super::row_mut(&mut d,"jobs",&id).unwrap();job["status"]=json!("queued");job["result"]["visualProgress"]["phase"]=json!("inventory");job["result"]["visualProgress"]["leaseId"]=Value::Null;
        assert!(claim_selected_ready(&mut d,AT,false,None,&|phase|{assert_eq!(phase,"inventory");Ok(())}).unwrap().is_some());
        assert_eq!(super::super::row(&d,"jobs",&id).unwrap()["sourceAttempts"],attempts);
        let mut job=json!({});set_worker_block(&mut job,"untrusted-stage",Some("secret path or arbitrary text"));
        assert_eq!(job["workerBlock"],json!({"stage":"unknown","code":"media_runtime_not_ready"}));
    }
    #[test]
    fn gpu_resource_exhaustion_is_held_across_recovery_without_source_replay(){
        let mut d=fixture();d["posts"].as_array_mut().unwrap().truncate(1);
        let (id,_)=claim(&mut d,AT).unwrap().unwrap();
        let job=super::super::row_mut(&mut d,"jobs",&id).unwrap();
        job["result"]["visualProgress"]["phase"]=json!("finalize");
        job["result"]["visualProgress"]["nextSelectionIndex"]=json!(32);
        job["resourceWait"]=json!({"resource":"gpu","reason":"gpu_gate_resource_wait_exhausted","state":"exhausted","operation":"whisper_asr"});
        let attempts=job["sourceAttempts"].clone();
        assert!(super::super::media_fullframes::finish(job,&Err(super::super::bad("gpu_gate_resource_wait_exhausted")),AT));
        assert_eq!(job["status"],"failed");assert_eq!(job["result"]["visualProgress"]["phase"],"held");
        assert_eq!(job["result"]["visualProgress"]["nextSelectionIndex"],32);
        assert!(!downloadable_failure("gpu_gate_resource_wait_exhausted"));
        crate::native_fixture_owner_repair::initialize_workspace(&mut d).unwrap(); super::super::recover(&mut d).unwrap();recover(&mut d,AT).unwrap();
        assert!(claim(&mut d,AT).unwrap().is_none());
        let job=super::super::row(&d,"jobs",&id).unwrap();
        assert_eq!(job["status"],"failed");assert_eq!(job["sourceAttempts"],attempts);
        assert_eq!(job["resourceWait"]["state"],"exhausted");
    }
    fn stopped_source()->(Value,String,Value){
        let mut d=fixture();d["posts"].as_array_mut().unwrap().truncate(1);
        let (id,post)=claim(&mut d,AT).unwrap().unwrap();
        d["jobs"][0]["result"]=json!({}); // source-only legacy recovery contract
        crate::native_fixture_owner_repair::initialize_workspace(&mut d).unwrap(); super::super::recover(&mut d).unwrap();recover(&mut d,AT).unwrap();
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
        crate::native_fixture_owner_repair::initialize_workspace(&mut d).unwrap(); super::super::recover(&mut d).unwrap();recover(&mut d,AT).unwrap();
        assert!(claim(&mut d,AT).unwrap().is_none());
        let mut new_body=body;new_body["attemptIndex"]=json!(1);new_body["verification"]["receiptId"]=json!("second-request");
        assert!(authorize_interrupted_retry(&mut d,&new_body,"local-owner",AT).is_err());
    }
    #[test]
    fn resumed_claim_stays_on_receipt_target_when_foreign_job_joins_queue(){
        let (mut d,id,post)=stopped_source();
        let permit=authorize_interrupted_retry(&mut d,&retry_body(&id,&post),"local-owner",AT).unwrap();
        let target=ResumeTarget::from_receipt(&permit).unwrap();
        let mut other=fixture()["posts"][1].clone();
        other["title"]=json!("Independent video group queued after owner retry");
        other["canonicalMediaId"]=json!("independent-retry-video");
        d["posts"].as_array_mut().unwrap().push(other.clone());
        let foreign=enqueue(&mut d,&other,AT,true).unwrap()["jobId"].as_str().unwrap().to_owned();
        super::super::row_mut(&mut d,"jobs",&foreign).unwrap()["startedAt"]=json!("2000-01-01T00:00:00Z");
        let candidates=scheduled_candidates(&d,next_scheduler_turn(&d).unwrap(),false);
        assert_eq!(candidates.first(),Some(&foreign),"generic scheduling would select the new foreign job");
        let (claimed,source)=claim_selected(&mut d,AT,false,Some(&target)).unwrap().unwrap();
        assert_eq!(claimed,id);
        assert_eq!(source["id"],post["id"]);
        assert_eq!(super::super::row(&d,"jobs",&foreign).unwrap()["status"],"queued");
        let before=d.clone();
        assert!(claim_selected(&mut d,AT,false,Some(&target)).unwrap().is_none());
        assert_eq!(d,before,"replayed scheduler cannot consume another lease while target runs");
    }
    #[test]
    fn resumed_claim_rejects_foreign_permit_and_stale_visual_epoch_without_queue_mutation(){
        let (mut d,id,post)=stopped_source();
        let permit=authorize_interrupted_retry(&mut d,&retry_body(&id,&post),"local-owner",AT).unwrap();
        let mut other=fixture()["posts"][1].clone();other["title"]=json!("Another video");other["canonicalMediaId"]=json!("independent-video");
        d["posts"].as_array_mut().unwrap().push(other.clone());
        let foreign=enqueue(&mut d,&other,AT,true).unwrap()["jobId"].as_str().unwrap().to_owned();
        let mut forged=ResumeTarget::from_receipt(&permit).unwrap();forged.job_id=foreign;
        let before=d.clone();
        assert!(claim_selected(&mut d,AT,false,Some(&forged)).unwrap().is_none());
        assert_eq!(d,before);

        let mut visual=fixture();visual["posts"].as_array_mut().unwrap().truncate(1);
        let (visual_id,_)=claim(&mut visual,AT).unwrap().unwrap();
        let job=super::super::row_mut(&mut visual,"jobs",&visual_id).unwrap();
        job["result"]["visualProgress"]["phase"]=json!("scan");
        super::super::media_fullframes::finish(job,&Err(super::super::conflict("offline failure")),AT);
        let epoch=job["result"]["visualProgress"]["leaseEpoch"].as_u64().unwrap();
        super::super::media_fullframes::resume(job,epoch).unwrap();
        let target=ResumeTarget::from_receipt(&json!({"jobId":visual_id,"leaseEpoch":epoch})).unwrap();
        job["result"]["visualProgress"]["leaseEpoch"]=json!(epoch+1);
        let before=visual.clone();
        assert!(claim_selected(&mut visual,AT,false,Some(&target)).unwrap().is_none());
        assert_eq!(visual,before,"stale owner epoch cannot claim or reconcile unrelated work");
    }
    #[test]
    fn completed_one_shot_visual_lease_cannot_be_reclaimed_from_old_receipt(){
        let mut d=fixture();d["posts"].as_array_mut().unwrap().truncate(1);
        let (id,_)=claim(&mut d,AT).unwrap().unwrap();
        let job=super::super::row_mut(&mut d,"jobs",&id).unwrap();
        job["result"]["visualProgress"]["phase"]=json!("scan");
        super::super::media_fullframes::finish(job,&Err(super::super::conflict("offline failure")),AT);
        let epoch=job["result"]["visualProgress"]["leaseEpoch"].as_u64().unwrap();
        super::super::media_fullframes::resume(job,epoch).unwrap();
        let target=ResumeTarget::from_receipt(&json!({"jobId":id,"leaseEpoch":epoch})).unwrap();
        assert_eq!(claim_selected(&mut d,AT,false,Some(&target)).unwrap().unwrap().0,id);
        let job=super::super::row_mut(&mut d,"jobs",&id).unwrap();
        super::super::media_fullframes::finish(job,&Ok(json!({"resume":true})),AT);
        let before=d.clone();
        assert!(claim_selected(&mut d,AT,false,Some(&target)).unwrap().is_none());
        assert_eq!(d,before,"old receipt cannot reacquire a yielded job with a new lease epoch");
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
    fn exact_identity_deduplicates_and_one_running_blocks_manual_and_auto() {
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
    fn cross_platform_duration_tolerance_coalesces_without_crossing_a_conflict() {
        let mut d=fixture_for(super::super::accounts::Profile::BawRussia);
        d["posts"][0]["durationMs"]=json!(44000);
        d["posts"][1]["durationSeconds"]=json!(44.8);
        let groups=QueueIndex::new(&d);
        assert_eq!(groups.groups["post-11391:one"],groups.groups["post-11390:two"]);
        reconcile(&mut d,AT).unwrap();
        assert_eq!(rows(&d,"jobs").len(),1);
        d["posts"][1]["durationSeconds"]=json!(47.0);
        let groups=QueueIndex::new(&d);
        assert_ne!(groups.groups["post-11391:one"],groups.groups["post-11390:two"]);
    }
    #[test]
    fn persisted_duration_key_still_owns_inflight_twin_after_group_upgrade() {
        let mut d=fixture();
        reconcile(&mut d,AT).unwrap();
        let id=text(&d["jobs"][0],"id").to_owned();
        let old_key=format!("{}:duration:44000",text(&d["jobs"][0],"groupKey"));
        d["jobs"][0]["groupKey"]=json!(old_key);
        let twin=d["posts"][1].clone();
        assert_eq!(enqueue(&mut d,&twin,AT,true).unwrap()["jobId"],id);
        assert_eq!(rows(&d,"jobs").len(),1);
        assert_eq!(claim(&mut d,AT).unwrap().unwrap().0,id);
        assert_eq!(rows(&d["jobs"][0],"sourceAttempts").len(),1);
    }
    #[test]
    fn a_changed_connector_or_material_epoch_cannot_claim_an_old_twin_job() {
        let mut d=fixture();
        reconcile(&mut d,AT).unwrap();
        d["jobs"][0]["connectorBinding"]=json!({"accountId":"Other"});
        let twin=d["posts"][1].clone();
        let fresh=enqueue(&mut d,&twin,AT,true).unwrap();
        assert_ne!(fresh["jobId"],d["jobs"][0]["id"]);
        let mut d=fixture();
        let (id,_)=claim(&mut d,AT).unwrap().unwrap();
        d["jobs"][0]["result"]["visualProgress"]["materialEpoch"]=json!("stale");
        let twin=d["posts"][1].clone();
        assert_ne!(enqueue(&mut d,&twin,AT,true).unwrap()["jobId"],id);
    }
    #[test]
    fn queue_and_exact_identity_reuse_are_scoped_to_each_configured_account() {
        for profile in [super::super::accounts::Profile::LikeAvto,super::super::accounts::Profile::BawRussia] {
            let mut d=fixture_for(profile);
            reconcile(&mut d,AT).unwrap();
            assert_eq!(rows(&d,"jobs").len(),1);
            assert_eq!(d["jobs"][0]["account"],profile.display());
            d["materials"]=json!([{"id":"shared","account":profile.display(),"kind":"transcript","postKey":"11391:one","text":"Account-local transcript"}]);
            add_visual_fixture(&mut d);
            d["posts"][1]["mediaSha256"]=d["materials"][1]["mediaSha256"].clone();
            select_visual_fixture(&mut d);
        super::super::knowledge::sync_catalog(&mut d,AT).unwrap();
            reconcile(&mut d,AT).unwrap();
            assert!(rows(&d,"jobs").iter().all(|job|job["status"]=="completed"));
            assert!(rows(&d,"posts").iter().all(|post|has_required_media(&d,post,AT).unwrap()));
        }
    }
    #[test]
    fn same_title_missing_target_is_not_completed_by_donor_or_legacy_title_job(){
        for profile in [super::super::accounts::Profile::LikeAvto,super::super::accounts::Profile::BawRussia]{
            let mut d=fixture_for(profile);
            for post in d["posts"].as_array_mut().unwrap(){post.as_object_mut().unwrap().remove("canonicalMediaId");}
            select_visual_fixture(&mut d);
            let donor=d["posts"][0].clone();let target=d["posts"][1].clone();
            let admitted=enqueue(&mut d,&donor,AT,false).unwrap();let id=text(&admitted,"jobId").to_owned();
            d["materials"]=json!([{"id":"paid-donor-speech","account":profile.display(),"kind":"transcript",
                "postKey":donor["postKey"],"text":"Donor spoken words"}]);
            add_visual_fixture(&mut d);super::super::knowledge::sync_catalog(&mut d,AT).unwrap();
            let job=super::super::row_mut(&mut d,"jobs",&id).unwrap();
            job["status"]=json!("completed");
            job["groupKey"]=json!(format!("visual-v2:{}:title:legacy-title-hash",profile.display()));
            job["sourceAttempts"]=json!([{"postId":donor["id"],"postKey":donor["postKey"],"status":"completed","paidReceipt":"retain-original"}]);
            job["result"]=json!({"rawPaidOutput":"retain-original"});let retained=job.clone();
            assert!(has_required_media(&d,&donor,AT).unwrap());assert!(!has_required_media(&d,&target,AT).unwrap());
            let requested=enqueue(&mut d,&target,AT,false).unwrap();
            assert_eq!(requested["status"],"queued");assert_ne!(requested["jobId"],id);
            assert_ne!(requested["deduplicated"],true);assert_ne!(requested["reused"],true);
            let target_job=super::super::row(&d,"jobs",text(&requested,"jobId")).unwrap();
            assert!(!covered(&d,target_job,AT).unwrap(),"donor completeness cannot satisfy the requested different video");
            assert_eq!(super::super::row(&d,"jobs",&id).unwrap(),&retained,"old title job and paid attempts remain intact");
            assert_eq!(rows(&d,"jobs").len(),2);
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
    fn reviewed_hls_retry_requires_complete_fresh_exact_source_proof(){
        for error in ["source_download_failed_auth","source_download_failed_http_forbidden"] {
            let mut d=fixture();d["posts"].as_array_mut().unwrap().truncate(1);
            d["posts"][0]["channel"]=json!("YouTube");d["posts"][0]["sourceUrl"]=json!("https://www.youtube.com/watch?v=AbCdEf123_-");
            let (id,post)=claim(&mut d,AT).unwrap().unwrap();
            mark_attempt(&mut d,&id,&post,Some(error),AT).unwrap();
            super::super::media_fullframes::finish(super::super::row_mut(&mut d,"jobs",&id).unwrap(),&Err(super::super::bad(error)),AT);
            let binding=super::super::active_binding(&d).unwrap().to_json();
            let version=d["jobs"][0]["sourceAttempts"][0]["sourceVersion"].clone();
            let body=json!({"jobId":id,"postId":post["id"],"attemptIndex":0,"sourceVersion":version,"connectorBinding":binding,"expectedError":error,
                "verification":{"receiptId":"reviewed-hls","checkedAt":AT,"method":"operator_completed_youtube_hls_review","noMediaProcesses":true,"fixArtifactSha256":"a".repeat(64),
                    "completedDownload":{"sourceKey":"yt:AbCdEf123_-","sourceVersion":version,"connectorBinding":binding,"transport":"youtube_hls","mediaSha256":"b".repeat(64),"bytes":5000,"durationMs":10000,"hasVideo":true,"hasAudio":true,"completedAt":AT}}});
            for case in ["missing","source","version","account","silent","oversize","stale","method","transport"] {
                let mut bad=body.clone();
                match case {
                    "missing"=>bad["verification"]["completedDownload"]=Value::Null,
                    "source"=>bad["verification"]["completedDownload"]["sourceKey"]=json!("yt:OtherVid123"),
                    "version"=>bad["verification"]["completedDownload"]["sourceVersion"]=json!("changed"),
                    "account"=>bad["verification"]["completedDownload"]["connectorBinding"]["accountId"]=json!("other"),
                    "silent"=>bad["verification"]["completedDownload"]["hasAudio"]=json!(false),
                    "oversize"=>bad["verification"]["completedDownload"]["bytes"]=json!(501*1024*1024),
                    "stale"=>bad["verification"]["completedDownload"]["completedAt"]=json!("2020-01-01T00:00:00Z"),
                    "method"=>bad["verification"]["method"]=json!("operator_download_fix_review"),
                    _=>bad["verification"]["completedDownload"]["transport"]=json!("other"),
                }
                let mut copy=d.clone();assert!(authorize_download_retry(&mut copy,&bad,"local-owner",AT).is_err(),"{case}");assert_eq!(copy,d);
            }
            for blocked in ["source_download_failed_rate_limited","source_download_failed_timeout","source_download_failed_network"] {
                let mut copy=d.clone();copy["jobs"][0]["sourceAttempts"][0]["error"]=json!(blocked);
                let before=copy.clone();let mut bad=body.clone();bad["expectedError"]=json!(blocked);
                assert!(authorize_download_retry(&mut copy,&bad,"local-owner",AT).is_err());assert_eq!(copy,before);
            }
            let permit=authorize_download_retry(&mut d,&body,"local-owner",AT).unwrap();
            assert_eq!(authorize_download_retry(&mut d,&body,"local-owner",AT).unwrap(),permit);
            let (claimed,_)=claim(&mut d,AT).unwrap().unwrap();assert_eq!(claimed,id);
            assert_eq!(d["jobs"][0]["sourceAttempts"].as_array().unwrap().len(),2);
        }
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
    fn failed_download_retry_rejects_same_source_opaque_history_without_mutation(){
        for status in ["completed","failed"]{
            for alias in [false,true]{
                let (mut d,_,post,body)=failed_download();
                let legacy_post=if alias{
                    let mut peer=post.clone();peer["id"]=json!("legacy-source-alias");
                    super::super::list_mut(&mut d,"posts").push(peer.clone());peer
                }else{post.clone()};
                super::super::list_mut(&mut d,"jobs").push(json!({"id":"legacy-opaque",
                    "kind":"media","refId":legacy_post["id"],"status":status,"result":{"processed":true}}));
                let before=d.clone();
                let error=authorize_download_retry(&mut d,&body,"local-owner",AT).unwrap_err();
                assert_eq!(error.1,"media_download_retry_opaque_history_reconciliation_required","{status} alias={alias}");
                assert_eq!(d,before,"{status} alias={alias}");
            }
        }
    }
    #[test]
    fn failed_download_retry_ignores_different_source_opaque_history_and_claims_once(){
        for status in ["completed","failed"]{
            let (mut d,id,post,body)=failed_download();
            let mut other=post.clone();other["id"]=json!("legacy-different-source");other["postKey"]=json!("other:source");
            super::super::list_mut(&mut d,"posts").push(other.clone());
            assert_ne!(super::super::knowledge::media_source_key(&post,text(&d,"account")),
                super::super::knowledge::media_source_key(&other,text(&d,"account")));
            let legacy=json!({"id":"legacy-opaque","kind":"media","refId":other["id"],"status":status,"result":{"processed":true}});
            super::super::list_mut(&mut d,"jobs").push(legacy.clone());
            let permit=authorize_download_retry(&mut d,&body,"local-owner",AT).unwrap();
            let (claimed,selected)=claim(&mut d,AT).unwrap().unwrap();assert_eq!(claimed,id);assert_eq!(selected,post);
            let attempts=rows(super::super::row(&d,"jobs",&id).unwrap(),"sourceAttempts");
            assert_eq!(attempts.len(),2);assert_eq!(attempts[1]["retryOf"]["permitId"],permit["id"]);
            assert!(claim(&mut d,AT).unwrap().is_none());
            assert_eq!(*super::super::row(&d,"jobs","legacy-opaque").unwrap(),legacy);
        }
    }
    fn terminal_opaque_reacquire_fixture()->Value{
        let mut d=processed_only_fixture();
        // This selected VK video has a real canonical provider identity. The
        // generic queue fixture intentionally has only an unbound video marker.
        d["posts"][0]["sourceUrl"]=json!("https://vk.com/video-11391_123456");
        select_visual_fixture(&mut d);
        let post=d["posts"][0].clone();
        assert_eq!(super::super::knowledge::media_source_key(&post,text(&d,"account")).as_deref(),Some("vk:-11391_123456"));
        d["jobs"][0]["status"]=json!("failed");d["jobs"][0]["finishedAt"]=json!(AT);
        d["jobs"][0]["result"]=json!({});
        d
    }
    fn terminal_v1_auto_with_failed_v2_fixture()->Value{
        let mut d=terminal_opaque_reacquire_fixture();let post=d["posts"][0].clone();
        let account=text(&d,"account").to_owned();
        let source=super::super::knowledge::media_source_key(&post,&account).unwrap();
        let version=super::super::media_fullframes::source_version(&post,&account);
        let old=&mut d["jobs"][0];
        old["purpose"]=json!(PURPOSE);old["status"]=json!("completed");
        old["result"]=json!({"processed":true,"reused":true,"sourcePostKey":post["postKey"]});
        old["sourceAttempts"]=json!([{"id":"v1-attempt","postId":post["id"],
            "postKey":post["postKey"],"sourceKey":source,"sourceVersion":null,
            "status":"completed","attemptNumber":1,"finishedAt":AT}]);
        assert!(!opaque_source_matches(&d,&d["jobs"][0],&post));
        assert!(reconciled_unproven_v1(&d["jobs"][0]));
        super::super::list_mut(&mut d,"jobs").push(json!({"id":"v2-failed","kind":"media",
            "purpose":PURPOSE,"visualContractVersion":2,"account":account,
            "refId":post["id"],"status":"failed","finishedAt":AT,
            "result":{"visualProgress":{"schemaVersion":2,"phase":"held",
                "nextSelectionIndex":0,"completedSelectedFrames":0}},
            "sourceAttempts":[{"id":"v2-attempt","postId":post["id"],
                "postKey":post["postKey"],"sourceKey":source,"sourceVersion":version,
                "status":"failed","attemptNumber":1,"finishedAt":AT,
                "error":"source_download_failed"}]}));
        d
    }
    fn reconciled_download_checkpoint(duration_ms:u64)->(Value,String,Vec<Value>){
        let mut d=terminal_v1_auto_with_failed_v2_fixture();
        let post=d["posts"][0].clone();let old=d["jobs"].as_array().unwrap().clone();
        let id=admit_reconciled_acquisition(&mut d,text(&post,"id"),
            "11111111-1111-4111-8111-111111111111","local-owner",AT).unwrap()["jobId"]
            .as_str().unwrap().to_owned();
        assert_eq!(claim(&mut d,AT).unwrap().unwrap().0,id);
        let before=super::super::row(&d,"jobs",&id).unwrap()["result"]["visualProgress"].clone();
        assert_eq!(before["phase"],"download");
        let lease=text(&before,"leaseId").to_owned();assert!(!lease.is_empty());
        let store=super::super::media_fullframes::store().unwrap();
        let source=store.put_bytes(b"offline source checkpoint for reconciled probe tests").unwrap().to_json();
        assert!(store.path(&super::super::media_fullframes::reference(&source).unwrap()).is_ok());
        let mut after=before.clone();after["phase"]=json!("inventory");
        after["source"]=source.clone();
        after["sourceIdentity"]=json!({"account":d["account"],"postKey":post["postKey"],
            "mediaSha256":source["sha256"],"durationMs":duration_ms});
        let projection=json!({"account":d["account"],"postKey":post["postKey"],
            "title":post["title"],"sourceUrl":post["sourceUrl"],"fallbackUrl":null});
        let media_source=super::super::media_processing::MediaSource::from_projection(
            &projection,text(&d,"account"),text(&post,"postKey")).unwrap();
        after["sourceProjection"]=media_source.projection();
        super::super::media_fullframes::checkpoint(super::super::row_mut(&mut d,"jobs",&id).unwrap(),
            &lease,&before,after).unwrap();
        let result:super::super::ApiResult<Value>=Ok(json!({"resume":true}));
        assert!(super::super::media_fullframes::finish(
            super::super::row_mut(&mut d,"jobs",&id).unwrap(),&result,AT));
        assert_eq!(super::super::row(&d,"jobs",&id).unwrap()["status"],"queued");
        assert_eq!(d["jobs"][0],old[0]);assert_eq!(d["jobs"][1],old[1]);
        (d,id,old)
    }
    #[test]
    fn reconciled_own_short_probe_keeps_exact_source_and_claims_inventory_without_redownload(){
        let (mut d,id,old)=reconciled_download_checkpoint(62_061);
        let post=d["posts"][0].clone();
        let job=super::super::row(&d,"jobs",&id).unwrap();
        let policy=super::super::post_media_policy::effective(&d,&post).unwrap();
        assert_eq!(policy["mode"],"full_audio_visual");
        assert_eq!(policy["decisionBasis"]["kind"],"exact_owner_override");
        assert_eq!(job["reconciledAcquisition"]["policySha256"],super::super::media_fullframes::hash(&policy));
        assert!(reconciled_current(&d,job,&post));
        let (claimed,selected)=claim(&mut d,AT).unwrap().unwrap();
        assert_eq!(claimed,id);assert_eq!(selected["id"],post["id"]);
        let job=super::super::row(&d,"jobs",&id).unwrap();
        assert_eq!(job["result"]["visualProgress"]["phase"],"inventory");
        assert_eq!(rows(job,"sourceAttempts").len(),1,"the same attempt resumes without download allocation");
        assert_eq!(d["jobs"][0],old[0]);assert_eq!(d["jobs"][1],old[1]);
    }
    #[test]
    fn reconciled_own_long_probe_does_not_retarget_explicit_visual_owner_work(){
        let (mut d,id,_)=reconciled_download_checkpoint(180_001);
        let post=d["posts"][0].clone();
        let policy=super::super::post_media_policy::effective(&d,&post).unwrap();
        assert_eq!(policy["mode"],"full_audio_visual");assert_eq!(policy["visualRequired"],true);
        assert!(reconciled_current(&d,super::super::row(&d,"jobs",&id).unwrap(),&post));
        assert_eq!(claim(&mut d,AT).unwrap().unwrap().0,id);
        let job=super::super::row(&d,"jobs",&id).unwrap();
        assert_eq!(job["status"],"running");
        assert_eq!(job["result"]["visualProgress"]["phase"],"inventory");
        assert_eq!(rows(job,"sourceAttempts").len(),1);
        let retained=job["result"]["visualProgress"]["source"].clone();
        assert!(super::super::media_fullframes::store().unwrap().path(
            &super::super::media_fullframes::reference(&retained).unwrap()).is_ok());
        assert!(cached_audio::claim_automatic(&mut d,AT,false).unwrap().is_none());
        let projection=&super::super::row(&d,"jobs",&id).unwrap()["result"]["visualProgress"]["sourceProjection"];
        assert!(super::super::media_processing::MediaSource::from_projection(
            projection,text(&d,"account"),text(&post,"postKey")).is_ok());
        assert_eq!(super::super::row(&d,"jobs",&id).unwrap()["result"]["visualProgress"]["source"],retained);
        assert_eq!(super::super::row(&d,"jobs",&id).unwrap()["result"]["visualProgress"]["sourceProjection"],projection.clone());
        assert_eq!(super::super::post_media_policy::effective(&d,&post).unwrap()["mode"],"full_audio_visual");
    }
    #[test]
    fn reconciled_own_probe_never_masks_policy_source_history_or_corrupt_source_drift(){
        for case in ["manual-policy","threshold","source-version","history","foreign-account",
            "foreign-binding","wrong-source-sha","malformed-duration"]{
            let (mut d,id,_)=reconciled_download_checkpoint(62_061);
            let post=d["posts"][0].clone();
            match case {
                "manual-policy"=>{
                    let record=json!({"version":1,"revision":2,"status":"active","postId":post["id"],
                        "account":d["account"],"connectorBinding":d["connectorBinding"],
                        "sourceVersion":super::super::media_fullframes::source_version(&post,text(&d,"account")),
                        "mode":"full_audio_visual"});
                    d["settings"]["postMediaPolicies"][text(&post,"id")]=record;
                },
                "threshold"=>d["settings"]["mediaPolicyDefaults"]=json!({"audioOnlyAboveSeconds":181}),
                "source-version"=>d["posts"][0]["title"]=json!("Changed title"),
                "history"=>d["jobs"][0]["finishedAt"]=json!("2026-09-23T08:00:00Z"),
                "foreign-account"=>super::super::row_mut(&mut d,"jobs",&id).unwrap()["result"]["visualProgress"]["sourceIdentity"]["account"]=json!("Other"),
                "foreign-binding"=>super::super::row_mut(&mut d,"jobs",&id).unwrap()["result"]["visualProgress"]["connectorBinding"]=json!({"foreign":true}),
                "wrong-source-sha"=>super::super::row_mut(&mut d,"jobs",&id).unwrap()["result"]["visualProgress"]["sourceIdentity"]["mediaSha256"]=json!("b".repeat(64)),
                _=>super::super::row_mut(&mut d,"jobs",&id).unwrap()["result"]["visualProgress"]["sourceIdentity"]["durationMs"]=json!("62061"),
            }
            assert!(!reconciled_current(&d,super::super::row(&d,"jobs",&id).unwrap(),&d["posts"][0]),"{case}");
        }
    }
    #[test]
    fn reconciled_v62_grant_without_settings_pin_accepts_only_exact_owner_policy_parity(){
        let (mut d,id,_)=reconciled_download_checkpoint(62_061);
        let job=super::super::row_mut(&mut d,"jobs",&id).unwrap();
        job["reconciledAcquisition"].as_object_mut().unwrap().remove("policySettingsSha256");
        assert!(reconciled_current(&d,super::super::row(&d,"jobs",&id).unwrap(),&d["posts"][0]));
        let post_id=text(&d["posts"][0],"id").to_owned();
        d["settings"]["postMediaPolicies"][&post_id]["revision"]=json!(2);
        assert!(!reconciled_current(&d,super::super::row(&d,"jobs",&id).unwrap(),&d["posts"][0]));
        assert_eq!(d["jobs"].as_array().unwrap().iter().find(|j|j["id"]==id).unwrap()["status"],"queued");
    }
    #[test]
    fn terminal_v1_auto_summary_and_failed_v2_admit_one_new_read_without_rewriting_history(){
        let mut d=terminal_v1_auto_with_failed_v2_fixture();
        let post=text(&d["posts"][0],"id").to_owned();
        let before=d["jobs"].as_array().unwrap().clone();
        let admitted=admit_reconciled_acquisition(&mut d,&post,
            "11111111-1111-4111-8111-111111111111","local-owner",AT).unwrap();
        let id=admitted["jobId"].as_str().unwrap();
        assert_eq!(d["jobs"][0],before[0]);assert_eq!(d["jobs"][1],before[1]);
        let pin=&super::super::row(&d,"jobs",id).unwrap()["reconciledAcquisition"];
        assert_eq!(pin["priorJobs"].as_array().unwrap().len(),2);
        assert_eq!(pin["priorAttemptCount"],"unknown_if_legacy_missing");
        assert_eq!(claim(&mut d,AT).unwrap().unwrap().0,id);
        assert_eq!(d["jobs"][0],before[0]);assert_eq!(d["jobs"][1],before[1]);
        assert_eq!(rows(super::super::row(&d,"jobs",id).unwrap(),"sourceAttempts").len(),1);
        assert!(claim(&mut d,AT).unwrap().is_none());
    }
    #[test]
    fn terminal_v1_auto_lineage_rejects_malformed_foreign_or_unresolved_history(){
        for case in ["v1-not-processed","v1-proof","v1-foreign-account","v1-malformed-ledger",
            "v2-running","v2-unknown","v2-foreign-binding","v2-malformed-ledger",
            "v2-attempt-unknown","v2-attempt-foreign","v2-attempt-other-source"]{
            let mut d=terminal_v1_auto_with_failed_v2_fixture();
            match case {
                "v1-not-processed"=>d["jobs"][0]["result"]["processed"]=json!(false),
                "v1-proof"=>d["jobs"][0]["result"]["visualProgress"]["source"]=json!({"sha256":"a".repeat(64)}),
                "v1-foreign-account"=>d["jobs"][0]["account"]=json!("Other"),
                "v1-malformed-ledger"=>d["jobs"][0]["sourceAttempts"]=json!({"not":"an array"}),
                "v2-running"|"v2-unknown"=>d["jobs"][1]["status"]=json!(case.strip_prefix("v2-").unwrap()),
                "v2-foreign-binding"=>d["jobs"][1]["connectorBinding"]=json!({"foreign":true}),
                "v2-malformed-ledger"=>d["jobs"][1]["sourceAttempts"]=json!({"not":"an array"}),
                "v2-attempt-unknown"=>d["jobs"][1]["sourceAttempts"][0]["status"]=json!("unknown"),
                "v2-attempt-foreign"=>d["jobs"][1]["sourceAttempts"][0]["account"]=json!("Other"),
                _=>d["jobs"][1]["sourceAttempts"][0]["sourceKey"]=json!("yt:Different99_"),
            }
            let before=d.clone();let post=text(&d["posts"][0],"id").to_owned();
            assert!(admit_reconciled_acquisition(&mut d,&post,
                "11111111-1111-4111-8111-111111111111","local-owner",AT).is_err(),"{case}");
            assert_eq!(d,before,"{case}");
        }
    }
    #[test]
    fn terminal_opaque_reacquire_adds_one_new_source_attempt_without_rewriting_history(){
        let mut d=terminal_opaque_reacquire_fixture();let old=d["jobs"][0].clone();
        let post=d["posts"][0].clone();
        let receipt="11111111-1111-4111-8111-111111111111";
        let admitted=admit_reconciled_acquisition(&mut d,text(&post,"id"),receipt,"local-owner",AT).unwrap();
        assert_eq!(admitted["status"],"queued");assert_eq!(d["jobs"][0],old);
        assert_eq!(admit_reconciled_acquisition(&mut d,text(&post,"id"),receipt,"local-owner",AT).unwrap()["deduplicated"],true);
        let before=d.clone();
        assert!(admit_reconciled_acquisition(&mut d,text(&post,"id"),"22222222-2222-4222-8222-222222222222","local-owner",AT).is_err());
        assert_eq!(d,before);
        let (claimed,selected)=claim(&mut d,AT).unwrap().unwrap();
        assert_eq!(claimed,admitted["jobId"]);assert_eq!(selected["id"],post["id"]);
        assert_eq!(d["jobs"][0],old);
        assert_eq!(rows(&d["jobs"][1],"sourceAttempts").len(),1);
        assert_eq!(d["jobs"][1]["sourceAttempts"][0]["attemptNumber"],1);
        assert!(claim(&mut d,AT).unwrap().is_none());
    }
    #[test]
    fn terminal_opaque_reacquire_rejects_unresolved_and_drift_without_mutation(){
        for case in ["running","unknown","missing-finish","foreign","binding","not-opaque","malformed-attempt","foreign-attempt"]{
            let mut d=terminal_opaque_reacquire_fixture();
            match case {
                "running"|"unknown"=>d["jobs"][0]["status"]=json!(case),
                "missing-finish"=>d["jobs"][0]["finishedAt"]=Value::Null,
                "foreign"=>d["jobs"][0]["account"]=json!("Other"),
                "binding"=>d["jobs"][0]["connectorBinding"]=json!({"foreign":true}),
                "malformed-attempt"=>d["jobs"][0]["sourceAttempts"]=json!({"not":"an array"}),
                "foreign-attempt"=>d["jobs"][0]["sourceAttempts"]=json!([{"status":"failed","account":"Other"}]),
                _=>{d["jobs"][0]["purpose"]=json!(PURPOSE);d["jobs"][0]["visualContractVersion"]=json!(2);},
            }
            let before=d.clone();let id=text(&d["posts"][0],"id").to_owned();
            assert!(admit_reconciled_acquisition(&mut d,&id,"11111111-1111-4111-8111-111111111111","local-owner",AT).is_err(),"{case}");
            assert_eq!(d,before,"{case}");
        }
    }
    #[test]
    fn terminal_opaque_reacquire_receipt_cannot_allocate_a_second_source(){
        let mut d=terminal_opaque_reacquire_fixture();
        let first=text(&d["posts"][0],"id").to_owned();
        let receipt="11111111-1111-4111-8111-111111111111";
        admit_reconciled_acquisition(&mut d,&first,receipt,"local-owner",AT).unwrap();
        let mut other=d["posts"][0].clone();
        other["id"]=json!("other-post");other["postKey"]=json!("other:post");
        other["sourceUrl"]=json!("https://www.youtube.com/watch?v=GhIjKl456_-");
        other["attachments"]=json!([]);
        assert_ne!(reconciled_scope(&d,&d["posts"][0]).unwrap().1,reconciled_scope(&d,&other).unwrap().1);
        let mut old=d["jobs"][0].clone();old["id"]=json!("other-legacy");old["refId"]=other["id"].clone();
        super::super::list_mut(&mut d,"posts").push(other);
        super::super::list_mut(&mut d,"jobs").push(old);
        let before=d.clone();
        assert!(admit_reconciled_acquisition(&mut d,"other-post",receipt,"local-owner",AT).is_err());
        assert_eq!(d,before);
    }
    #[test]
    fn terminal_opaque_reacquire_download_interruption_never_replays_attempt(){
        let mut d=terminal_opaque_reacquire_fixture();
        let post=text(&d["posts"][0],"id").to_owned();
        let id=admit_reconciled_acquisition(&mut d,&post,"11111111-1111-4111-8111-111111111111","local-owner",AT)
            .unwrap()["jobId"].as_str().unwrap().to_owned();
        assert_eq!(claim(&mut d,AT).unwrap().unwrap().0,id);
        let attempts=super::super::row(&d,"jobs",&id).unwrap()["sourceAttempts"].clone();
        {
            let job=super::super::row_mut(&mut d,"jobs",&id).unwrap();
            job["status"]=json!("interrupted");
            job["result"]["visualProgress"]["leaseId"]=Value::Null;
        }
        super::super::media_fullframes::recover(super::super::row_mut(&mut d,"jobs",&id).unwrap()).unwrap();
        assert!(claim(&mut d,AT).unwrap().is_none());
        let job=super::super::row(&d,"jobs",&id).unwrap();
        assert_eq!(job["status"],"failed");
        assert_eq!(job["error"],"media_reconciled_acquisition_interrupted_unknown");
        assert_eq!(job["sourceAttempts"],attempts);
    }
    #[test]
    fn terminal_opaque_reacquire_unspent_grant_survives_startup_recovery(){
        let mut d=terminal_opaque_reacquire_fixture();
        let post=text(&d["posts"][0],"id").to_owned();
        let id=admit_reconciled_acquisition(&mut d,&post,"11111111-1111-4111-8111-111111111111","local-owner",AT)
            .unwrap()["jobId"].as_str().unwrap().to_owned();
        // Generic startup recovery runs first and labels even queued work interrupted.
        let job=super::super::row_mut(&mut d,"jobs",&id).unwrap();
        job["status"]=json!("interrupted");job["finishedAt"]=json!(AT);
        job["error"]=json!("Server restarted during job");
        recover(&mut d,AT).unwrap();
        assert_eq!(super::super::row(&d,"jobs",&id).unwrap()["status"],"queued");
        assert!(rows(super::super::row(&d,"jobs",&id).unwrap(),"sourceAttempts").is_empty());
        assert_eq!(claim(&mut d,AT).unwrap().unwrap().0,id);
        assert_eq!(rows(super::super::row(&d,"jobs",&id).unwrap(),"sourceAttempts").len(),1);
    }
    #[test]
    fn terminal_opaque_reacquire_accepts_adapter_empty_fallback_only(){
        assert!(reconciled_no_fallback(&json!({"sourceUrl":"https://www.youtube.com/watch?v=AbCdEf123_-","fallbackUrl":""})));
        assert!(reconciled_no_fallback(&json!({"sourceUrl":"https://www.youtube.com/watch?v=AbCdEf123_-"})));
        assert!(reconciled_no_fallback(&json!({"fallbackUrl":null})));
        for fallback in [json!("https://example.invalid/other"),json!(true),json!({})] {
            assert!(!reconciled_no_fallback(&json!({"fallbackUrl":fallback})));
        }
    }
    #[test]
    fn terminal_opaque_reacquire_accepts_normalized_same_source_locator(){
        let d=terminal_opaque_reacquire_fixture();let mut post=d["posts"][0].clone();
        post["sourceUrl"]=json!("https://youtube.com/watch?v=AbCdEf123_-&t=30");
        post["attachments"]=json!([]);
        let account=text(&d,"account");
        let source=super::super::knowledge::media_source_key(&post,account).unwrap();
        let normalized=json!({"sourceUrl":"https://youtube.com/watch?v=AbCdEf123_-","fallbackUrl":""});
        assert!(reconciled_locator_matches(&post,&normalized,account,&source));
        let switched=json!({"sourceUrl":"https://youtube.com/watch?v=ZzYyXx987_-","fallbackUrl":""});
        assert!(!reconciled_locator_matches(&post,&switched,account,&source));
        post["sourceUrl"]=json!("https://example.org/video/old");
        let unsupported=super::super::knowledge::media_source_key(&post,account).unwrap();
        let changed_unsupported=json!({"sourceUrl":"https://example.org/video/new","fallbackUrl":""});
        assert!(!reconciled_locator_matches(&post,&changed_unsupported,account,&unsupported));
    }
    #[test]
    fn terminal_opaque_reacquire_rejects_unsupported_source_before_allocation(){
        let mut d=terminal_opaque_reacquire_fixture();
        d["posts"][0]["sourceUrl"]=json!("https://example.org/video/old");
        d["posts"][0]["attachments"]=json!([]);
        let before=d.clone();let post=text(&d["posts"][0],"id").to_owned();
        assert!(admit_reconciled_acquisition(&mut d,&post,"11111111-1111-4111-8111-111111111111","local-owner",AT).is_err());
        assert_eq!(d,before);
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
        crate::native_fixture_owner_repair::initialize_workspace(&mut d).unwrap(); super::super::recover(&mut d).unwrap();recover(&mut d,AT).unwrap();
        let (next_id,second)=claim(&mut d,AT).unwrap().unwrap();assert_eq!(next_id,id);assert_ne!(first["id"],second["id"]);
        let job=super::super::row(&d,"jobs",&id).unwrap();assert_eq!(job["sourceAttempts"][0],failed_attempt);assert_eq!(rows(job,"sourceAttempts").len(),2);
        let progress=&job["result"]["visualProgress"];assert_eq!(progress["schemaVersion"],2);assert_eq!(progress["phase"],"download");assert_eq!(progress["sourcePostId"],second["id"]);assert!(!text(progress,"leaseId").is_empty());assert!(progress.get("resumePhase").is_none());
    }
    #[test]
    fn recovery_resumes_same_committed_cursor_without_consuming_another_source() {
        let mut d=fixture();let (id,first)=claim(&mut d,AT).unwrap().unwrap();
        d["jobs"][0]["result"]["visualProgress"]["nextSelectionIndex"]=json!(4);
        let prior_epoch=d["jobs"][0]["result"]["visualProgress"]["leaseEpoch"].as_u64().unwrap();
        crate::native_fixture_owner_repair::initialize_workspace(&mut d).unwrap(); super::super::recover(&mut d).unwrap();recover(&mut d,AT).unwrap();
        let (next_id,second)=claim(&mut d,AT).unwrap().unwrap();assert_eq!(id,next_id);assert_eq!(first["id"],second["id"]);
        assert_eq!(rows(&d["jobs"][0],"sourceAttempts").len(),1);assert_eq!(d["jobs"][0]["result"]["visualProgress"]["nextSelectionIndex"],4);
        assert!(d["jobs"][0]["result"]["visualProgress"]["leaseEpoch"].as_u64().unwrap()>prior_epoch);
    }
    #[test]
    fn stale_resumed_source_is_held_without_starving_next_job(){
        for drift in ["removed","changed","account"] {
            let mut d=fixture();d["posts"][1]["title"]=json!("Independent other video");d["posts"][1]["canonicalMediaId"]=json!("independent-video");
            select_visual_fixture(&mut d);
            assert!(rows(&d,"posts").iter().all(|p|visual_wanted(&d,p)),"both independent sources need explicit visual acquisition");
            reconcile(&mut d,AT).unwrap();
            let initial=d["posts"][0]["id"].clone();
            for job in super::super::list_mut(&mut d,"jobs"){
                job["createdAt"]=json!(if job["refId"]==initial{"2026-09-22T07:59:59Z"}else{AT});
            }
            let (first,post)=claim(&mut d,AT).unwrap().unwrap();
            assert_eq!(post["id"],initial,"the stale source fixture must be deterministic");
            let captured=super::super::row(&d,"jobs",&first).unwrap()["result"]["visualProgress"].clone();
            assert_eq!(captured["sourcePostId"],post["id"]);
            assert_eq!(captured["sourceVersion"],super::super::media_fullframes::source_version(&post,text(&d,"account")));
            assert!(visual_wanted(&d,&post));
            let job=super::super::row_mut(&mut d,"jobs",&first).unwrap();job["result"]["visualProgress"]["nextSelectionIndex"]=json!(4);
            super::super::media_fullframes::finish(job,&Ok(json!({"resume":true})),AT);
            match drift {"removed"=>d["posts"].as_array_mut().unwrap().retain(|p|p["id"]!=post["id"]),"changed"=>{super::super::row_mut(&mut d,"posts",text(&post,"id")).unwrap()["sourceUrl"]=json!("https://example.org/new-video");},_=>{super::super::row_mut(&mut d,"jobs",&first).unwrap()["result"]["visualProgress"]["account"]=json!("Other");}}
            // A changed source is still explicitly visual work. The obsolete
            // checkpoint must fail its source guard, not merely pause because
            // its old source-bound owner policy ceased to apply.
            if drift=="changed"{select_visual_fixture(&mut d);}
            if drift=="removed"{assert!(rows(&d,"posts").iter().all(|p|p["id"]!=post["id"]));}
            else{
                let current=super::super::row(&d,"posts",text(&post,"id")).unwrap();
                assert!(visual_wanted(&d,current));
                if drift=="changed"{assert_ne!(captured["sourceVersion"],super::super::media_fullframes::source_version(current,text(&d,"account")));}
                else{assert_ne!(super::super::row(&d,"jobs",&first).unwrap()["result"]["visualProgress"]["account"],d["account"]);}
            }
            reconcile(&mut d,AT).unwrap();
            // A changed locator may legitimately enqueue new work for the same
            // stable post. Order all three intents: stale checkpoint first,
            // independent waiter next, replacement source last. Initial claim
            // set the stale job's startedAt to AT, overriding its createdAt.
            for job in super::super::list_mut(&mut d,"jobs"){if job["id"]!=first {
                job["startedAt"]=json!(if job["refId"]!=post["id"]{"2026-09-22T08:00:01Z"}else{"2026-09-22T08:00:02Z"});
            }}
            let order=scheduled_candidates(&d,next_scheduler_turn(&d).unwrap(),false);
            assert_eq!(order.first(),Some(&first),"{drift}: the obsolete checkpoint must be examined before the independent waiter");
            assert_ne!(super::super::row(&d,"jobs",&order[1]).unwrap()["refId"],post["id"],"{drift}: independent waiter precedes replacement source");
            let (next,selected) = claim_scoped(&mut d,"2026-09-22T08:00:03Z",false).unwrap().unwrap();assert_ne!(first,next,"{drift}");
            assert_ne!(selected["id"],post["id"],"the independent source must get its turn");
            assert!(visual_wanted(&d,&selected));
            let fresh=super::super::row(&d,"jobs",&next).unwrap();
            assert_eq!(fresh["result"]["visualProgress"]["sourceVersion"],super::super::media_fullframes::source_version(&selected,text(&d,"account")));
            let held=super::super::row(&d,"jobs",&first).unwrap();assert_eq!(held["status"],"failed");assert_eq!(held["result"]["visualProgress"]["nextSelectionIndex"],4);assert_eq!(held["result"]["visualProgress"]["phase"],"held");
        }
    }
    #[test]
    fn completion_burst_resumes_scan_then_serves_oldest_waiter(){
        let mut d=fixture();d["posts"][1]["title"]=json!("Independent other video");d["posts"][1]["canonicalMediaId"]=json!("independent-video");
        select_visual_fixture(&mut d);
        let(first,_)=claim(&mut d,AT).unwrap().unwrap();
        {
            let job=super::super::row_mut(&mut d,"jobs",&first).unwrap();
            let p=&mut job["result"]["visualProgress"];p["phase"]=json!("scan");p["sourceIdentity"]=json!({"durationMs":37764});
            p["completedSelectedFrames"]=json!(20);p["nextSelectionIndex"]=json!(20);
            super::super::media_fullframes::finish(job,&Ok(json!({"resume":true})),AT);
        }
        for second in 1..7 {
            let at=format!("2026-09-22T08:00:{second:02}Z");
            let(next,_)=claim(&mut d,&at).unwrap().unwrap();assert_eq!(next,first);
            let job=super::super::row_mut(&mut d,"jobs",&first).unwrap();
            assert_eq!(job["result"]["visualProgress"]["nextSelectionIndex"],20);assert_eq!(rows(job,"sourceAttempts").len(),1);
            super::super::media_fullframes::finish(job,&Ok(json!({"resume":true})),&at);
        }
        let(second,_)=claim(&mut d,"2026-09-22T08:00:07Z").unwrap().unwrap();assert_ne!(first,second);
        super::super::media_fullframes::finish(super::super::row_mut(&mut d,"jobs",&second).unwrap(),&Ok(json!({"resume":true})),AT);
        let mut restarted:Value=serde_json::from_str(&d.to_string()).unwrap();
        let(third,_)=claim(&mut restarted,"2026-09-22T08:00:08Z").unwrap().unwrap();assert_eq!(third,first);
    }
    fn scheduled_job(id:&str,phase:&str,duration:Value,completed:u64,at:&str)->Value {
        json!({"id":id,"kind":"media","purpose":"auto_media","visualContractVersion":2,"status":"queued","createdAt":at,
            "result":{"visualProgress":{"schemaVersion":2,"phase":phase,"sourceIdentity":{"durationMs":duration},"completedSelectedFrames":completed}}})
    }
    #[test]
    fn scheduler_finishes_shortest_of_sixteen_scans_without_round_robin_delay(){
        let totals=[76,78,82,120,160,200,260,400,600,800,1000,1200,1600,2058,3470,3846];
        let mut d=json!({"posts":[],"jobs":totals.iter().enumerate().map(|(i,total)|scheduled_job(&format!("job{i:02}"),"scan",json!(total*500),20,AT)).collect::<Vec<_>>()});
        let mut finalized_at=None;
        for turn in 1..=20 {
            let selected=scheduled_candidates(&d,turn,false)[0].clone();
            let job=super::super::row_mut(&mut d,"jobs",&selected).unwrap();
            job["startedAt"]=json!((chrono::DateTime::parse_from_rfc3339(AT).unwrap()+chrono::Duration::seconds(turn as i64)).to_rfc3339());
            let progress=&mut job["result"]["visualProgress"];
            if progress["phase"]=="finalize" {finalized_at=Some(turn);assert_eq!(selected,"job00");break;}
            let done=progress["completedSelectedFrames"].as_u64().unwrap()+4;progress["completedSelectedFrames"]=json!(done);
            let total=progress["sourceIdentity"]["durationMs"].as_u64().unwrap()/500;
            if done>=total {progress["phase"]=json!("finalize");}
        }
        assert_eq!(finalized_at,Some(17)); // Round-robin needs >200 claims here.
    }
    #[test]
    fn scheduler_unknown_duration_and_legacy_metadata_are_backward_compatible(){
        let mut jobs=vec![scheduled_job("known","scan",json!(37764),20,AT)];
        for (i,duration) in [Value::Null,json!(0),json!(-1),json!("37764"),json!(u64::MAX)].into_iter().enumerate(){
            jobs.push(scheduled_job(&format!("bad{i}"),"scan",duration,0,AT));
        }
        let d=json!({"posts":[],"jobs":jobs});assert_eq!(scheduled_candidates(&d,1,false)[0],"known");assert_eq!(next_scheduler_turn(&d).unwrap(),1);
        let mut final_job=scheduled_job("final","finalize",Value::Null,0,AT);final_job["result"]["visualProgress"]["schedulerTurn"]=json!(15);
        let mut d=d;super::super::list_mut(&mut d,"jobs").push(final_job);
        assert_eq!(scheduled_candidates(&d,1,false)[0],"final");assert_eq!(next_scheduler_turn(&d).unwrap(),16);
    }
    #[test]
    fn scheduler_oldest_waiter_survives_continuous_short_arrivals(){
        let mut d=json!({"posts":[],"jobs":[scheduled_job("long","scan",json!(1922881),0,AT)]});let mut last_long=0;
        for turn in 1..=40 {
            let at=format!("2026-09-22T08:00:{turn:02}Z");
            super::super::list_mut(&mut d,"jobs").push(scheduled_job(&format!("short{turn}"),"scan",json!(1000),0,&at));
            let chosen=scheduled_candidates(&d,turn,false)[0].clone();let job=super::super::row_mut(&mut d,"jobs",&chosen).unwrap();job["startedAt"]=json!(at);
            if chosen=="long" {assert!(turn-last_long<=8);last_long=turn;}else{job["status"]=json!("completed");}
        }
        assert_eq!(last_long,40);
    }
    #[test]
    fn scheduler_all_persistent_waiters_get_service_within_eight_times_queue_size(){
        let mut d=json!({"posts":[],"jobs":(0..16).map(|i|scheduled_job(&format!("job{i:02}"),"scan",json!(100000+i*1000),0,AT)).collect::<Vec<_>>()});let mut seen=BTreeSet::new();
        for turn in 1..=128 {
            let chosen=scheduled_candidates(&d,turn,false)[0].clone();seen.insert(chosen.clone());
            super::super::row_mut(&mut d,"jobs",&chosen).unwrap()["startedAt"]=json!((chrono::DateTime::parse_from_rfc3339(AT).unwrap()+chrono::Duration::seconds(turn as i64)).to_rfc3339());
        }
        assert_eq!(seen.len(),16);
    }
    #[test]
    fn scheduler_failed_download_keeps_fairness_watermark(){
        let mut d=fixture();let(id,post)=claim(&mut d,AT).unwrap().unwrap();mark_attempt(&mut d,&id,&post,Some("source_download_failed"),AT).unwrap();
        assert!(super::super::row(&d,"jobs",&id).unwrap()["result"]["visualProgress"].is_null());assert_eq!(next_scheduler_turn(&d).unwrap(),2);
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
        d["posts"][1]["mediaSha256"]=d["materials"][1]["mediaSha256"].clone();
        select_visual_fixture(&mut d);
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
    fn open_comment_scope_defers_manual_archive_job_and_reopens_when_needed() {
        let mut d=fixture();
        for item in super::super::list_mut(&mut d,"items") {item["workflow"]=json!("closed");}
        let post=d["posts"][0].clone();
        enqueue(&mut d,&post,AT,true).unwrap();
        assert!(claim_scoped(&mut d,AT,true).unwrap().is_none());
        assert_eq!(d["jobs"][0]["status"],"paused");
        assert_eq!(d["jobs"][0]["manualRequested"],true);
        d["items"][0]["workflow"]=json!("attention");
        assert!(claim_scoped(&mut d,AT,true).unwrap().is_some());
    }
    #[test]
    fn open_comment_scope_pauses_durable_checkpoint_without_discarding_it() {
        let mut d=fixture();
        let (id,_)=claim_scoped(&mut d,AT,true).unwrap().unwrap();
        let job=super::super::row_mut(&mut d,"jobs",&id).unwrap();
        job["status"]=json!("queued");
        job["result"]["visualProgress"]["leaseId"]=Value::Null;
        job["result"]["visualProgress"]["phase"]=json!("scan");
        job["result"]["visualProgress"]["nextSelectionIndex"]=json!(4);
        let checkpoint=job["result"]["visualProgress"].clone();
        for item in super::super::list_mut(&mut d,"items") {item["workflow"]=json!("closed");}
        assert!(claim_scoped(&mut d,AT,true).unwrap().is_none());
        let job=super::super::row(&d,"jobs",&id).unwrap();
        assert_eq!(job["status"],"paused");
        assert_eq!(job["mediaScopePause"],true);
        assert_eq!(job["result"]["visualProgress"],checkpoint);
        d["items"][0]["workflow"]=json!("attention");
        reconcile_scoped(&mut d,AT,true).unwrap();
        let job=super::super::row(&d,"jobs",&id).unwrap();
        assert_eq!(job["status"],"queued");
        assert!(job.get("mediaScopePause").is_none());
        assert_eq!(job["result"]["visualProgress"],checkpoint);
    }
    #[test]
    fn late_twin_coverage_finishes_queued_checkpoint_without_new_lease() {
        let mut d=fixture();
        let (id,post)=claim_scoped(&mut d,AT,true).unwrap().unwrap();
        mark_attempt(&mut d,&id,&post,None,AT).unwrap();
        let job=super::super::row_mut(&mut d,"jobs",&id).unwrap();
        job["status"]=json!("queued");
        job["result"]["visualProgress"]["leaseId"]=Value::Null;
        job["result"]["visualProgress"]["phase"]=json!("scan");
        job["result"]["visualProgress"]["nextSelectionIndex"]=json!(4);
        let checkpoint=job["result"]["visualProgress"].clone();
        let attempts=job["sourceAttempts"].clone();
        let twin=d["posts"].as_array().unwrap().iter().find(|p|p["id"]!=post["id"]).unwrap().clone();
        d["materials"]=json!([{"id":"late-twin","kind":"transcript","postKey":twin["postKey"],"text":"Already transcribed twin"}]);
        add_visual_fixture(&mut d);
        // The recipient has an independently observed exact byte match; a
        // canonical group or matching title alone cannot transfer visual proof.
        let twin_sha=d["materials"][1]["mediaSha256"].clone();
        super::super::row_mut(&mut d,"posts",text(&post,"id")).unwrap()["mediaSha256"]=twin_sha;
        select_visual_fixture(&mut d);
        super::super::knowledge::sync_catalog(&mut d,AT).unwrap();
        assert!(claim_scoped(&mut d,AT,true).unwrap().is_none());
        let job=super::super::row(&d,"jobs",&id).unwrap();
        assert_eq!(job["status"],"completed");
        assert_eq!(job["result"]["reused"],true);
        assert_eq!(job["result"]["visualProgress"],checkpoint);
        assert_eq!(job["sourceAttempts"],attempts);
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
    fn empty_held_group()->(Value,String,Value,Value){
        let mut d=fixture();
        d["posts"][0]["sourceUrl"]=json!("https://vk.com/video-123_456");
        d["posts"][1]["sourceUrl"]=json!("https://youtube.com/watch?v=AbCdEf123_-");
        let (id,first)=claim_scoped(&mut d,AT,true).unwrap().unwrap();
        let other=rows(&d,"posts").iter().find(|p|p["id"]!=first["id"]).unwrap().clone();
        let job=super::super::row_mut(&mut d,"jobs",&id).unwrap();
        job["status"]=json!("failed");job["error"]=json!("Adapter process failed");
        job["result"]["visualProgress"]["phase"]=json!("held");
        job["result"]["visualProgress"]["leaseId"]=Value::Null;
        (d,id,first,other)
    }
    #[test]
    fn empty_held_visual_source_allows_distinct_open_group_source_without_rewriting_history(){
        let (mut d,old_id,first,other)=empty_held_group();
        let old=super::super::row(&d,"jobs",&old_id).unwrap().clone();
        // A closed candidate sorts ahead of both open posts. The rollover job
        // must stay pinned to its own open post, including at claim time.
        super::super::list_mut(&mut d,"posts").push(json!({"id":"post-11389:closed","postKey":"11389:closed","objectId":"11389","title":"Обзор семейного автомобиля","canonicalMediaId":"fixture-exact-video","channel":"VK","attachments":[{"type":"video"}]}));
        super::super::list_mut(&mut d,"items").push(json!({"id":"closed-item","itemId":"closed","objectId":"11389","postId":"post-11389:closed","postKey":"11389:closed","conversationKey":"11389:closed","providerStatus":"new","workflow":"closed"}));
        assert_ne!(super::super::knowledge::media_source_key(&first,text(&d,"account")),
            super::super::knowledge::media_source_key(&other,text(&d,"account")));
        reconcile_scoped(&mut d,AT,true).unwrap();
        assert_eq!(rows(&d,"jobs").len(),2);
        assert_eq!(super::super::row(&d,"jobs",&old_id).unwrap(),&old);
        let next=rows(&d,"jobs").iter().find(|j|j["id"]!=old_id).unwrap();
        assert_eq!(next["status"],"queued");assert_eq!(next["refId"],other["id"]);
        assert_eq!(next["sourceRollover"]["fromJobId"],old_id);
        assert_eq!(next["sourceRollover"]["postId"],other["id"]);
        assert!(next["sourceAttempts"].as_array().unwrap().is_empty());
        let item=rows(&d,"items").iter().find(|i|i["postId"]==other["id"]).unwrap();
        assert_eq!(preparation_state(&d,item,AT).unwrap(),Some("media_wait"));
        let (new_id,selected)=claim_scoped(&mut d,AT,true).unwrap().unwrap();
        assert_ne!(new_id,old_id);assert_eq!(selected["id"],other["id"]);
        assert_eq!(super::super::row(&d,"jobs",&new_id).unwrap()["result"]["visualProgress"]["sourcePostId"],other["id"]);
        assert_eq!(super::super::row(&d,"jobs",&old_id).unwrap(),&old);
    }
    #[test]
    fn empty_held_rollover_rejects_alias_prior_progress_consumed_source_and_closed_candidate(){
        for condition in ["alias","unidentified","partial","receipt","consumed","closed"]{
            let (mut d,old_id,first,other)=empty_held_group();
            match condition{
                "alias"=>{
                    d["posts"][0]["sourceUrl"]=json!("https://youtu.be/AbCdEf123_-");
                    d["posts"][1]["sourceUrl"]=json!("https://youtube.com/watch?v=AbCdEf123_-");
                    let scope=text(&d,"account").to_owned();
                    let updated_first=rows(&d,"posts").iter().find(|p|p["id"]==first["id"]).unwrap();
                    let version=super::super::media_fullframes::source_version(updated_first,&scope);
                    d["jobs"][0]["sourceAttempts"][0]["sourceKey"]=json!("yt:AbCdEf123_-");
                    d["jobs"][0]["result"]["visualProgress"]["sourceVersion"]=json!(version);
                },
                "unidentified"=>{for post in d["posts"].as_array_mut().unwrap(){if post["id"]==other["id"]{post["sourceUrl"]=Value::Null;}}},
                "partial"=>{d["jobs"][0]["result"]["visualProgress"]["nextSelectionIndex"]=json!(4);d["jobs"][0]["result"]["visualProgress"]["completedSelectedFrames"]=json!(4);},
                "receipt"=>{d["jobs"][0]["result"]["visualProgress"]["latestReceipt"]=json!({"sha256":"a".repeat(64),"bytes":12});},
                "consumed"=>{let source=super::super::knowledge::media_source_key(&other,text(&d,"account"));d["jobs"][0]["sourceAttempts"].as_array_mut().unwrap().push(json!({"postId":other["id"],"postKey":other["postKey"],"sourceKey":source,"status":"failed"}));},
                _=>{for item in d["items"].as_array_mut().unwrap(){if item["postId"]==other["id"]{item["workflow"]=json!("closed");}}},
            }
            reconcile_scoped(&mut d,AT,true).unwrap();
            assert_eq!(rows(&d,"jobs").len(),1,"{condition}");
            assert_eq!(d["jobs"][0]["id"],old_id);
        }
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
        d["posts"][0]["mediaSha256"]=d["materials"][1]["mediaSha256"].clone();
        select_visual_fixture(&mut d);
        super::super::knowledge::sync_catalog(&mut d, AT).unwrap();
        assert!(preparation_state(&d, &item, AT).unwrap().is_some(),"unknown cross-post coverage is not full audio proof");
        let post=d["posts"][0].clone();
        let source=super::super::media_fullframes::source_version(&post,"LikeAvto");
        super::super::list_mut(&mut d,"materials").push(json!({"id":"target-full-audio","account":"LikeAvto",
            "postKey":post["postKey"],"kind":"transcript","text":"Complete source-bound audio",
            "transcription":{"partial":false,"coverage":"full_audio","sourceVersion":source,
                "mediaDurationSeconds":120.0,"audioDurationSeconds":120.0}}));
        super::super::knowledge::sync_catalog(&mut d,AT).unwrap();
        assert_eq!(preparation_state(&d,&item,AT).unwrap(),None);
    }

    #[test]
    fn batch_media_states_match_individual_gate_for_repeated_posts(){
        let mut d=fixture();
        let mut second_comment=d["items"][0].clone();
        second_comment["id"]=json!("second-comment-same-post");
        super::super::list_mut(&mut d,"items").push(second_comment);
        for status in [None,Some("queued"),Some("failed"),Some("completed")] {
            d["jobs"]=status.map_or(json!([]),|status|json!([{"id":"history","kind":"media","purpose":"auto_media",
                "visualContractVersion":2,"status":status,"refId":d["posts"][0]["id"]}]));
            let items=rows(&d,"items");
            let batch=preparation_states(&d,items,AT).unwrap();
            for item in items {
                assert_eq!(batch.get(text(item,"id")).copied().unwrap(),preparation_state(&d,item,AT).unwrap(),"{status:?}");
            }
        }
        d["jobs"]=json!([]);
        reconcile(&mut d,AT).unwrap();
        assert_eq!(rows(&d,"jobs").len(),1);
        let old_key=format!("{}:duration:44000",text(&d["jobs"][0],"groupKey"));
        d["jobs"][0]["groupKey"]=json!(old_key.clone());
        assert_eq!(QueueIndex::new(&d).posts(&old_key).len(),2);
        for status in ["queued","failed"]{
            d["jobs"][0]["status"]=json!(status);
            let items=rows(&d,"items");
            let batch=preparation_states(&d,items,AT).unwrap();
            for item in items{
                assert_eq!(batch.get(text(item,"id")).copied().unwrap(),preparation_state(&d,item,AT).unwrap(),"historical alias {status}");
            }
        }
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
            super::super::list_mut(&mut initial, "posts").push(json!({"id":id,"postKey":&id[5..],"title":title,"canonicalMediaId":"fixture-exact-video","channel":channel,"attachments":[{"type":"video"}]}));
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
        assert!(d["items"].as_array().unwrap().iter().all(|item| preparation_state(&d, item, AT).unwrap().is_some()),
            "shared legacy transcript may stop acquisition fallback but does not prove complete audio for preparation");
    }
}
