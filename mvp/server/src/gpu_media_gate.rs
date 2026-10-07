//! Cooperative, cross-process admission for GPU-heavy media work.
//!
//! The lock and recovery marker occupy the same persistent file. A released OS
//! lock only proves that the owner handle closed; a dirty marker must be
//! reconciled separately before another request may start.
use serde_json::{Value, json};
use std::{env, fs::{File, OpenOptions}, io::{Read, Seek, SeekFrom, Write}, path::{Path, PathBuf}, time::{Duration, Instant}};

// Waiting retains the same worker and its completed ASR segments. A resource
// deadline is not an inference failure and must never trigger an ASR replay.
const WAIT: Duration = Duration::from_secs(2 * 60 * 60);
const POLL: Duration = Duration::from_millis(100);
const MAX_MARKER: u64 = 4096;

#[cfg(test)]
#[path="media_gpu_release_tests.rs"]
mod resource_release_tests;


/// Diagnostic observations only; never an ASR checkpoint or retry authority.
#[derive(Clone,Copy,Debug,PartialEq,Eq)]
pub(super) enum FinalizeStage { VisualVerification, CatalogLookup, AudioProbe, AudioExtraction,
    GpuAdmission, AsrRunning, AsrSegmentComplete, Ocr, CatalogAdmission, CatalogAdmitted }
impl FinalizeStage {
    fn name(self)->&'static str {match self {
        Self::VisualVerification=>"visual_verification",Self::CatalogLookup=>"catalog_lookup",
        Self::AudioProbe=>"audio_probe",Self::AudioExtraction=>"audio_extraction",
        Self::GpuAdmission=>"gpu_admission",Self::AsrRunning=>"asr_running",
        Self::AsrSegmentComplete=>"asr_segment_complete",Self::Ocr=>"ocr",
        Self::CatalogAdmission=>"catalog_admission",Self::CatalogAdmitted=>"catalog_admitted"}}
}
pub(super) fn finalize_progress(job:&mut Value,stage:FinalizeStage,completed:Option<u64>,total:Option<u64>,at:&str)->bool {
    let p=&job["result"]["visualProgress"];
    if job["kind"]!="media"||job["status"]!="running"||p["schemaVersion"]!=2||p["phase"]!="finalize"
        ||p["leaseEpoch"].as_u64().is_none()||p["leaseId"].as_str().is_none_or(|s|s.is_empty()||s.len()>128||!s.bytes().all(|c|c.is_ascii_alphanumeric()||c==b'-')){return false;}
    let Ok(time)=chrono::DateTime::parse_from_rfc3339(at) else{return false};
    let old=&job["finalizeProgress"];
    let same=old["version"]==1&&old["leaseEpoch"]==p["leaseEpoch"]&&old["leaseId"]==p["leaseId"];
    let prior_completed=if same {old["asrCompletedSegments"].as_u64().unwrap_or(0)}else{0};
    let prior_total=if same {old["asrTotalSegments"].as_u64()}else{None};
    let completed=completed.unwrap_or(prior_completed);let total=total.or(prior_total);
    if completed>16||total.is_some_and(|n|n>16||completed>n)
        ||same&&(completed<prior_completed||prior_total.is_some_and(|n|total!=Some(n))){return false;}
    if same&&old["lastProgressAtUtc"].as_str().and_then(|s|chrono::DateTime::parse_from_rfc3339(s).ok()).is_some_and(|last|time<last){return false;}
    if same&&old["stage"]==stage.name()&&old["asrCompletedSegments"]==completed&&old["asrTotalSegments"]==json!(total){return false;}
    let valid_time=|key:&str|old[key].as_str().filter(|s|chrono::DateTime::parse_from_rfc3339(s).is_ok());
    let started=if same {valid_time("startedAtUtc").unwrap_or(at)}else{at};
    let stage_started=if same&&old["stage"]==stage.name(){valid_time("stageStartedAtUtc").unwrap_or(at)}else{at};
    job["finalizeProgress"]=json!({"version":1,"leaseEpoch":p["leaseEpoch"],"leaseId":p["leaseId"],
        "stage":stage.name(),"startedAtUtc":started,"stageStartedAtUtc":stage_started,"lastProgressAtUtc":at,
        "asrCompletedSegments":completed,"asrTotalSegments":total,
        "countScope":"current_worker_observation","resumeAuthority":false});
    true
}

pub(super) struct Lease { file: File }

/// The dispatch closure is called exactly once, only after admission. In
/// particular, a caller's preceding ASR segments remain outside this wait.
pub(super) async fn run_after_admission<T,F,D>(admission:impl std::future::Future<Output=Result<Option<Lease>,String>>,dispatch:D)->Result<T,String>
where D:FnOnce()->F,F:std::future::Future<Output=Result<T,String>> {
    let gate=admission.await?;
    let result=dispatch().await;
    if result.is_ok(){if let Some(gate)=gate{gate.finish()?;}}
    result
}

pub(super) struct JobContext<'a> { app: &'a crate::App, id: &'a str, binding: Value }

fn job_binding(job:&Value)->Value {
    let keys=["id","kind","account","connectorBinding","refId","createdAt","startedAt","attemptId","sourceAttempts","audioPin"];
    let mut binding=serde_json::Map::new();
    for key in keys {binding.insert(key.into(),job[key].clone());}
    binding.insert("visualProgress".into(),job["result"]["visualProgress"].clone());
    Value::Object(binding)
}
impl<'a> JobContext<'a> {
    #[cfg(test)]
    pub(super) async fn validate_current_for_test(&self)->Result<(),String>{self.ready(false).await}
    pub(super) async fn bind(app:&'a crate::App,id:&'a str,progress:Option<&Value>)->Result<Self,String>{
        let job=app.db.read_job(id).await.map_err(|e|e.1)?.ok_or("gpu_gate_job_missing")?;
        if job["status"]!="running" || progress.is_some_and(|p|job["result"]["visualProgress"]!=*p){return Err("gpu_gate_job_changed".into());}
        Ok(Self{app,id,binding:job_binding(&job)})
    }
    fn validate(&self,job:&Value)->Result<(),String>{
        if job["status"]!="running" || job_binding(job)!=self.binding{return Err("gpu_gate_job_changed".into());}
        Ok(())
    }
    fn validate_workspace(&self,d:&Value)->Result<(),String>{
        let job=crate::row(d,"jobs",self.id).map_err(|e|e.1)?;
        self.validate(job)?;
        let progress=if job["kind"]=="media_audio" {&job["audioPin"]["progress"]} else {&job["result"]["visualProgress"]};
        if !progress.is_object(){return Err("gpu_gate_job_source_missing".into());}
        crate::media_speech_assets::require_progress(d,progress)?;
        let binding=crate::active_binding(d).map_err(|e|e.1)?.to_json();
        let account=d["account"].as_str().ok_or("gpu_gate_account_missing")?;
        let post=crate::row(d,"posts",progress["sourcePostId"].as_str().ok_or("gpu_gate_post_missing")?).map_err(|e|e.1)?;
        if progress["account"]!=account || progress["connectorBinding"]!=binding
            || job["account"]!=account || job["connectorBinding"]!=binding
            || progress["sourcePostKey"]!=post["postKey"]
            || progress["sourceVersion"]!=crate::media_fullframes::source_version(post,account)
            || progress["materialEpoch"]!=crate::media_queue::material_epoch(d,post) {
            return Err("gpu_gate_job_source_changed".into());
        }
        if job["kind"]=="media" && !crate::post_media_policy::visual_required(d,post).map_err(|e|e.1)? {
            return Err("gpu_gate_job_policy_changed".into());
        }
        if job["kind"]=="media_audio" {
            let pin=&job["audioPin"];
            let origin=crate::row(d,"jobs",pin["originJobId"].as_str().ok_or("gpu_gate_audio_origin_missing")?).map_err(|e|e.1)?;
            if origin["result"]["visualProgress"]!=*progress || origin["result"]["visualProgress"]["leaseEpoch"]!=pin["originEpoch"]
                || !origin["result"]["visualProgress"]["leaseId"].is_null()
                || !matches!(origin["status"].as_str(),Some("failed"|"queued"|"paused"))
                || crate::post_media_policy::effective(d,post).map_err(|e|e.1)?!=pin["policy"] {
                return Err("gpu_gate_job_source_changed".into());
            }
        }
        Ok(())
    }
    pub(super) fn matches_audio_progress(&self,progress:&Value)->bool {self.binding["audioPin"]["progress"]==*progress}
    async fn ready(&self,waited:bool)->Result<(),String>{
        // A still-identical job is insufficient if its current post, company,
        // connector or admitted material heads changed during resource wait.
        let current=self.app.read().await.map_err(|e|e.1)?;
        self.validate_workspace(&current)?;
        drop(current);
        if waited {
            self.transition(None).await
        } else {
            let job=self.app.db.read_job(self.id).await.map_err(|e|e.1)?.ok_or("gpu_gate_job_missing")?;
            self.validate(&job)
        }
    }
    /// Best effort, bounded stage transitions. Diagnostic persistence cannot fail
    /// processing, renew a lease, or authorize a retry. Binding excludes this field.
    pub(super) async fn observe_finalize(&self,stage:FinalizeStage,completed:Option<u64>,total:Option<u64>) {
        if self.binding["kind"]!="media"||self.binding["visualProgress"]["phase"]!="finalize" {return;}
        let _=self.app.change_job(self.id,|d|{
            let job=crate::row_mut(d,"jobs",self.id)?;
            self.validate(job).map_err(|e|crate::conflict(&e))?;
            finalize_progress(job,stage,completed,total,&crate::now());
            Ok(())
        }).await;
    }
    async fn transition(&self,wait:Option<Value>)->Result<(),String>{
        self.app.change_job(self.id,|d|{
            let job=crate::row_mut(d,"jobs",self.id)?;
            self.validate(job).map_err(|e|crate::conflict(&e))?;
            if let Some(wait)=wait {job["resourceWait"]=wait;}
            else {job.as_object_mut().ok_or_else(||crate::conflict("gpu_gate_job_invalid"))?.remove("resourceWait");}
            Ok(())
        }).await.map_err(|e|e.1)
    }
}

fn gate_path() -> Result<Option<PathBuf>, String> {
    let Some(value)=env::var_os("COMMUNITYHERO_GPU_GATE_FILE") else {return Ok(None)};
    let path = PathBuf::from(value);
    if !path.is_absolute() || !path.is_file() { return Err("gpu_gate_invalid".into()); }
    // Neither the file nor a path component may redirect a server to a
    // different lock. The deployment still must attest the same file identity
    // and private ACL for both account manifests.
    for component in path.ancestors() {
        if component.as_os_str().is_empty(){continue;}
        let metadata=std::fs::symlink_metadata(component).map_err(|_|"gpu_gate_invalid")?;
        if metadata.file_type().is_symlink() {
            return Err("gpu_gate_linked".into());
        }
        #[cfg(windows)] {
            use std::os::windows::fs::MetadataExt;
            if metadata.file_attributes() & 0x400 != 0 {return Err("gpu_gate_linked".into());}
        }
    }
    Ok(Some(path))
}

/// True means configured and available; false means the legacy uncoordinated
/// mode is still active. Deployment must require true for both accounts.
pub(super) fn preflight() -> Result<bool, String> {
    let Some(path)=gate_path()? else {return Ok(false)};
    let expected=expected_identity()?;
    let file=OpenOptions::new().read(true).write(true).open(path).map_err(|_|"gpu_gate_unavailable")?;
    if validate_file(&file)?!=expected {return Err("gpu_gate_identity_mismatch".into());}
    Ok(true)
}

fn expected_identity()->Result<String,String>{
    let value=env::var("COMMUNITYHERO_GPU_GATE_FILE_ID").map_err(|_|"gpu_gate_identity_missing")?;
    let bytes=value.as_bytes();
    if bytes.len()!=25 || bytes[8]!=b':' || bytes.iter().enumerate().any(|(i,c)|i!=8 && !c.is_ascii_hexdigit()) || value!=value.to_ascii_lowercase() {
        return Err("gpu_gate_identity_invalid".into());
    }
    Ok(value)
}

fn validate_file(file:&File)->Result<String,String>{
    if !file.metadata().map_err(|_|"gpu_gate_unavailable")?.is_file(){return Err("gpu_gate_invalid".into());}
    #[cfg(windows)] {
        use std::os::windows::io::AsRawHandle;
        use windows_sys::Win32::Storage::FileSystem::{BY_HANDLE_FILE_INFORMATION,GetFileInformationByHandle};
        let mut info:BY_HANDLE_FILE_INFORMATION=unsafe{std::mem::zeroed()};
        if unsafe{GetFileInformationByHandle(file.as_raw_handle(),&mut info)}==0 || info.nNumberOfLinks!=1 {
            return Err("gpu_gate_linked_or_identity_unknown".into());
        }
        return Ok(format!("{:08x}:{:016x}",info.dwVolumeSerialNumber,(u64::from(info.nFileIndexHigh)<<32)|u64::from(info.nFileIndexLow)));
    }
    #[cfg(unix)] {
        use std::os::unix::fs::MetadataExt;
        let metadata=file.metadata().map_err(|_|"gpu_gate_unavailable")?;
        if metadata.nlink()!=1{return Err("gpu_gate_linked".into());}
        return Ok(format!("{:08x}:{:016x}",metadata.dev(),metadata.ino()));
    }
    #[allow(unreachable_code)]
    Err("gpu_gate_platform_unsupported".into())
}

fn read_marker(file:&mut File)->Result<Value,String>{
    let size=file.metadata().map_err(|_|"gpu_gate_unavailable")?.len();
    if size==0 || size>MAX_MARKER{return Err("gpu_gate_marker_invalid".into());}
    file.seek(SeekFrom::Start(0)).map_err(|_|"gpu_gate_unavailable")?;
    let mut bytes=Vec::with_capacity(size as usize);
    file.read_to_end(&mut bytes).map_err(|_|"gpu_gate_unavailable")?;
    serde_json::from_slice(&bytes).map_err(|_|"gpu_gate_marker_invalid".into())
}

fn write_marker(file:&mut File, value:&Value)->Result<(),String>{
    let bytes=serde_json::to_vec(value).map_err(|_|"gpu_gate_marker_invalid")?;
    if bytes.len() as u64>MAX_MARKER{return Err("gpu_gate_marker_invalid".into());}
    file.seek(SeekFrom::Start(0)).map_err(|_|"gpu_gate_unavailable")?;
    file.write_all(&bytes).map_err(|_|"gpu_gate_unavailable")?;
    file.set_len(bytes.len() as u64).map_err(|_|"gpu_gate_unavailable")?;
    file.sync_all().map_err(|_|"gpu_gate_unavailable".into())
}

fn clean(marker:&Value)->bool{
    marker["version"]==1 && marker["status"]=="clean" && marker.as_object().is_some_and(|o|o.len()==2)
}

#[cfg(windows)]
fn owner_start_filetime()->Result<Option<u64>,String>{
    use windows_sys::Win32::{Foundation::FILETIME,System::Threading::{GetCurrentProcess,GetProcessTimes}};
    let mut created:FILETIME=unsafe{std::mem::zeroed()};
    let mut exited:FILETIME=unsafe{std::mem::zeroed()};
    let mut kernel:FILETIME=unsafe{std::mem::zeroed()};
    let mut user:FILETIME=unsafe{std::mem::zeroed()};
    if unsafe{GetProcessTimes(GetCurrentProcess(),&mut created,&mut exited,&mut kernel,&mut user)}==0 {
        return Err("gpu_gate_owner_identity_unknown".into());
    }
    Ok(Some((u64::from(created.dwHighDateTime)<<32)|u64::from(created.dwLowDateTime)))
}
#[cfg(not(windows))]
fn owner_start_filetime()->Result<Option<u64>,String>{Ok(None)}

impl Lease {
    pub(super) async fn acquire(operation:&str,account:&str,endpoint:&str)->Result<Option<Self>,String>{
        Self::acquire_for_job(operation,account,endpoint,None).await
    }
    pub(super) async fn acquire_for_job(operation:&str,account:&str,endpoint:&str,job:Option<&JobContext<'_>>)->Result<Option<Self>,String>{
        let Some(path)=gate_path()? else {
            if let Some(job)=job {job.ready(false).await?;}
            return Ok(None)
        };
        let expected=expected_identity()?;
        let result=Self::acquire_path_observed(&path,operation,account,endpoint,WAIT,Some(&expected),job).await;
        if result.as_ref().is_err_and(|e|e!="gpu_gate_wait_timeout") {
            if let Some(job)=job {
                // Clear a formerly active wait on a known terminal gate error.
                // A cancelled/replaced owner is left for its own finalizer;
                // cleanup failure must not disguise the original gate error.
                if let Ok(Some(current))=job.app.db.read_job(job.id).await {
                    if current["resourceWait"]["resource"]=="gpu" && job.validate(&current).is_ok(){let _=job.transition(None).await;}
                }
            }
        }
        result.map(Some).map_err(|e|if e=="gpu_gate_wait_timeout"{"gpu_gate_resource_wait_exhausted".into()}else{e})
    }
    async fn acquire_path(path:&Path,operation:&str,account:&str,endpoint:&str,wait:Duration)->Result<Self,String>{
        Self::acquire_path_checked(path,operation,account,endpoint,wait,None).await
    }
    async fn acquire_path_checked(path:&Path,operation:&str,account:&str,endpoint:&str,wait:Duration,expected:Option<&str>)->Result<Self,String>{
        Self::acquire_path_observed(path,operation,account,endpoint,wait,expected,None).await
    }
    async fn acquire_path_observed(path:&Path,operation:&str,account:&str,endpoint:&str,wait:Duration,expected:Option<&str>,job:Option<&JobContext<'_>>)->Result<Self,String>{
        let started=Instant::now();
        let started_at=crate::now();
        let mut waited=false;
        loop {
            let mut file=OpenOptions::new().read(true).write(true).open(&path).map_err(|_|"gpu_gate_unavailable")?;
            let identity=validate_file(&file)?;
            if expected.is_some_and(|expected|identity!=expected){return Err("gpu_gate_identity_mismatch".into());}
            match file.try_lock() {
                Ok(())=>{
                    let marker=read_marker(&mut file)?;
                    if !clean(&marker){return Err("gpu_gate_dirty_or_invalid".into());}
                    // No GPU work has started: cancellation or stale ownership
                    // here safely closes a still-clean OS lock.
                    if let Some(job)=job {job.ready(waited).await?;}
                    let dirty=json!({"version":1,"status":"dirty","ownerPid":std::process::id(),
                        "ownerStartFiletime":owner_start_filetime()?,"admittedAtUtc":crate::now(),"operation":operation,"account":account,
                        "endpoint":endpoint,"childPid":null});
                    write_marker(&mut file,&dirty)?;
                    return Ok(Self{file});
                },
                Err(std::fs::TryLockError::WouldBlock)=>{},
                Err(_)=>return Err("gpu_gate_unavailable".into()),
            }
            if !waited {
                if let Some(job)=job {job.transition(Some(json!({"resource":"gpu","reason":"gpu_busy","state":"waiting","operation":operation,"startedAtUtc":started_at,"maxWaitSeconds":wait.as_secs()}))).await?;}
                waited=true;
            }
            if started.elapsed()>=wait {
                if let Some(job)=job {job.transition(Some(json!({"resource":"gpu","reason":"gpu_gate_resource_wait_exhausted","state":"exhausted","operation":operation,"startedAtUtc":started_at,"maxWaitSeconds":wait.as_secs()}))).await?;}
                return Err("gpu_gate_wait_timeout".into());
            }
            tokio::time::sleep(POLL.min(wait.saturating_sub(started.elapsed()))).await;
        }
    }
    pub(super) fn finish(mut self)->Result<(),String>{
        write_marker(&mut self.file,&json!({"version":1,"status":"clean"}))?;
        Ok(())
    }
}

impl Drop for Lease {
    fn drop(&mut self) {
        // On cancellation, crash, unknown bridge outcome, or a failed durable
        // clean write, release only the OS lock. The marker remains dirty.
        let _=self.file.unlock();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finalize_observations_are_closed_bound_and_not_resume_authority(){
        let mut job=json!({"id":"fixture","kind":"media","status":"running","resourceWait":{"state":"waiting"},
            "result":{"visualProgress":{"schemaVersion":2,"phase":"finalize","leaseEpoch":9,"leaseId":"lease","private":"preserved"}}});
        let binding=job_binding(&job);let protected=job["result"].clone();let wait=job["resourceWait"].clone();
        assert!(finalize_progress(&mut job,FinalizeStage::AudioExtraction,Some(0),Some(2),"2026-09-27T08:00:00Z"));
        assert!(finalize_progress(&mut job,FinalizeStage::AsrRunning,None,None,"2026-09-27T08:01:00Z"));
        assert!(finalize_progress(&mut job,FinalizeStage::AsrSegmentComplete,Some(1),None,"2026-09-27T08:02:00Z"));
        let proof=job["finalizeProgress"].clone();assert_eq!(proof["asrCompletedSegments"],1);assert_eq!(proof["asrTotalSegments"],2);
        assert_eq!(proof["startedAtUtc"],"2026-09-27T08:00:00Z");assert_eq!(proof["resumeAuthority"],false);
        assert_eq!(job_binding(&job),binding);assert_eq!(job["result"],protected);assert_eq!(job["resourceWait"],wait);
        assert!(!proof.to_string().contains("private"));
        assert!(!finalize_progress(&mut job,FinalizeStage::AsrSegmentComplete,Some(1),None,"2026-09-27T08:03:00Z"));
        assert!(!finalize_progress(&mut job,FinalizeStage::AsrSegmentComplete,Some(0),None,"2026-09-27T08:03:00Z"));
        assert!(!finalize_progress(&mut job,FinalizeStage::AsrSegmentComplete,Some(3),None,"2026-09-27T08:03:00Z"));
        assert!(!finalize_progress(&mut job,FinalizeStage::AsrRunning,None,None,"2026-09-27T07:59:00Z"));
        assert_eq!(job["finalizeProgress"],proof);
        job["result"]["visualProgress"]["leaseEpoch"]=json!(10);
        assert!(finalize_progress(&mut job,FinalizeStage::AudioProbe,None,None,"2026-09-27T08:04:00Z"));
        assert_eq!(job["finalizeProgress"]["asrCompletedSegments"],0);assert!(job["finalizeProgress"]["asrTotalSegments"].is_null());
    }
    #[test]
    fn invalid_finalize_observations_do_not_change_jobs(){
        let base=json!({"kind":"media","status":"running","result":{"visualProgress":{"schemaVersion":2,"phase":"finalize","leaseEpoch":1,"leaseId":"lease"}}});
        for (key,value) in [("status",json!("cancelled")),("kind",json!("media_audio"))]{let mut job=base.clone();job[key]=value;let before=job.clone();assert!(!finalize_progress(&mut job,FinalizeStage::AudioProbe,None,None,"2026-09-27T08:00:00Z"));assert_eq!(job,before);}
        for (key,value) in [("phase",json!("scan")),("leaseId",Value::Null),("schemaVersion",json!(1))]{let mut job=base.clone();job["result"]["visualProgress"][key]=value;let before=job.clone();assert!(!finalize_progress(&mut job,FinalizeStage::AudioProbe,None,None,"2026-09-27T08:00:00Z"));assert_eq!(job,before);}
        let mut job=base.clone();assert!(!finalize_progress(&mut job,FinalizeStage::AudioProbe,Some(17),Some(17),"2026-09-27T08:00:00Z"));assert_eq!(job,base);
        assert!(!finalize_progress(&mut job,FinalizeStage::AudioProbe,None,None,"private path https://example.invalid"));assert_eq!(job,base);
        job["finalizeProgress"]=json!({"version":1,"leaseEpoch":1,"leaseId":"lease","startedAtUtc":"private secret path","stageStartedAtUtc":"private secret path","stage":"audio_probe"});
        assert!(finalize_progress(&mut job,FinalizeStage::AsrRunning,None,None,"2026-09-27T08:00:00Z"));
        assert!(!job["finalizeProgress"].to_string().contains("private"));
    }
    #[tokio::test]
    async fn finalize_observation_uses_durable_job_channel_and_stale_owner_cannot_update(){
        let (app,_temp)=job_fixture().await;
        app.change_job("gpu-fixture",|d|{
            let job=crate::row_mut(d,"jobs","gpu-fixture")?;
            job["result"]["visualProgress"]["schemaVersion"]=json!(2);
            job["result"]["visualProgress"]["phase"]=json!("finalize");
            job["result"]["visualProgress"]["leaseEpoch"]=json!(9);
            job["resourceWait"]=json!({"resource":"gpu","state":"waiting"});Ok(())
        }).await.unwrap();
        let observer=JobContext::bind(&app,"gpu-fixture",None).await.unwrap();
        observer.observe_finalize(FinalizeStage::AudioProbe,None,None).await;
        let observed=app.db.read_job("gpu-fixture").await.unwrap().unwrap();
        assert_eq!(observed["finalizeProgress"]["stage"],"audio_probe");
        assert_eq!(observed["resourceWait"]["state"],"waiting");
        assert_eq!(job_binding(&observed),observer.binding);
        app.change_job("gpu-fixture",|d|{crate::row_mut(d,"jobs","gpu-fixture")?["status"]=json!("cancelled");Ok(())}).await.unwrap();
        let before=app.db.read_job("gpu-fixture").await.unwrap().unwrap();
        observer.observe_finalize(FinalizeStage::AsrRunning,Some(0),Some(1)).await;
        assert_eq!(app.db.read_job("gpu-fixture").await.unwrap().unwrap(),before);
    }

    fn fixture()->(tempfile::TempDir,PathBuf){
        let temp=tempfile::tempdir().unwrap();let path=temp.path().join("gpu.gate");
        std::fs::write(&path,b"{\"version\":1,\"status\":\"clean\"}").unwrap();(temp,path)
    }
    async fn job_fixture()->(crate::App,tempfile::TempDir){
        let (app,temp)=crate::tests::test_app().await;
        app.change(|d|{
            let post=json!({"id":"gpu-post","postKey":"gpu-post","title":"Bound GPU fixture","attachments":[{"type":"video","source_url":"https://example.invalid/video"}]});
            let account=d["account"].as_str().unwrap().to_owned();let binding=crate::active_binding(d)?.to_json();
            let progress=json!({"leaseId":"lease-1","sourceVersion":crate::media_fullframes::source_version(&post,&account),"account":account,"connectorBinding":binding,"sourcePostId":"gpu-post","sourcePostKey":"gpu-post","materialEpoch":crate::media_queue::material_epoch(d,&post)});
            d["settings"]["postMediaPolicies"]=json!({"gpu-post":{"version":1,"revision":1,"status":"active",
                "postId":"gpu-post","account":account,"connectorBinding":binding,"sourceVersion":progress["sourceVersion"],"mode":"full_audio_visual"}});
            crate::list_mut(d,"posts").push(post);
            crate::list_mut(d,"jobs").push(json!({"id":"gpu-fixture","kind":"media","status":"running","account":account,"connectorBinding":binding,"attemptId":"attempt-1","result":{"visualProgress":progress}}));Ok(())
        }).await.unwrap();
        (app,temp)
    }
    async fn until_waiting(app:&crate::App){
        tokio::time::timeout(Duration::from_secs(3),async {
            loop {
                if app.db.read_job("gpu-fixture").await.unwrap().unwrap()["resourceWait"]["state"]=="waiting"{return;}
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        }).await.expect("worker must publish resource wait before dispatch");
    }
    #[tokio::test]
    async fn waiting_second_audio_segment_preserves_first_and_dispatches_each_once(){
        let (_temp,path)=fixture();let (app,_db)=job_fixture().await;
        let job=JobContext::bind(&app,"gpu-fixture",None).await.unwrap();
        let calls=std::sync::atomic::AtomicUsize::new(0);
        let mut completed=vec![run_after_admission(async {Lease::acquire_path(&path,"whisper_asr","LikeAvto","fixture",Duration::ZERO).await.map(Some)},||async {calls.fetch_add(1,std::sync::atomic::Ordering::SeqCst);Ok("segment-one")}).await.unwrap()];
        let blocker=OpenOptions::new().read(true).write(true).open(&path).unwrap();blocker.lock().unwrap();
        let mut events=app.events.subscribe();
        let second=run_after_admission(async {Lease::acquire_path_observed(&path,"whisper_asr","LikeAvto","fixture",Duration::from_secs(3),None,Some(&job)).await.map(Some)},||async {calls.fetch_add(1,std::sync::atomic::Ordering::SeqCst);Ok("segment-two")});
        tokio::pin!(second);
        tokio::select!{_=&mut second=>panic!("dispatched before gate release"),_=until_waiting(&app)=>{}}
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst),1);
        let waiting=app.db.read_job("gpu-fixture").await.unwrap().unwrap()["resourceWait"].clone();
        assert_eq!(waiting["reason"],"gpu_busy");
        while events.try_recv().is_ok(){}
        tokio::select!{_=&mut second=>panic!("dispatched before gate release"),_=tokio::time::sleep(POLL*3)=>{}}
        assert_eq!(app.db.read_job("gpu-fixture").await.unwrap().unwrap()["resourceWait"],waiting);
        assert!(matches!(events.try_recv(),Err(tokio::sync::broadcast::error::TryRecvError::Empty)),"polling must not write jobs or emit change events");
        blocker.unlock().unwrap();
        completed.push(second.await.unwrap());
        assert_eq!(completed,vec!["segment-one","segment-two"]);
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst),2);
        assert!(app.db.read_job("gpu-fixture").await.unwrap().unwrap()["resourceWait"].is_null());
        assert!(clean(&read_marker(&mut OpenOptions::new().read(true).open(&path).unwrap()).unwrap()));
    }
    #[tokio::test]
    async fn cancelled_gpu_wait_never_mutates_marker_or_dispatches(){
        let (_temp,path)=fixture();let (app,_db)=job_fixture().await;
        let before=std::fs::read(&path).unwrap();
        let blocker=OpenOptions::new().read(true).write(true).open(&path).unwrap();blocker.lock().unwrap();
        let worker_app=app.clone();let worker_path=path.clone();
        let worker=tokio::spawn(async move {
            let job=JobContext::bind(&worker_app,"gpu-fixture",None).await.unwrap();
            run_after_admission(async {Lease::acquire_path_observed(&worker_path,"whisper_asr","LikeAvto","fixture",WAIT,None,Some(&job)).await.map(Some)},||async {panic!("cancelled wait must not dispatch");#[allow(unreachable_code)] Ok(())}).await
        });
        until_waiting(&app).await;worker.abort();assert!(worker.await.unwrap_err().is_cancelled());
        blocker.unlock().unwrap();assert_eq!(std::fs::read(&path).unwrap(),before);
    }
    #[tokio::test]
    async fn stale_gpu_job_is_rejected_before_marking_clean_gate_dirty(){
        let (_temp,path)=fixture();let (app,_db)=job_fixture().await;
        let job=JobContext::bind(&app,"gpu-fixture",None).await.unwrap();
        let blocker=OpenOptions::new().read(true).write(true).open(&path).unwrap();blocker.lock().unwrap();
        let waiting=Lease::acquire_path_observed(&path,"whisper_asr","LikeAvto","fixture",Duration::from_secs(3),None,Some(&job));tokio::pin!(waiting);
        tokio::select!{_=&mut waiting=>panic!("acquired busy gate"),_=until_waiting(&app)=>{}}
        app.change_job("gpu-fixture",|d|{crate::row_mut(d,"jobs","gpu-fixture")?["result"]["visualProgress"]["leaseId"]=json!("lease-2");Ok(())}).await.unwrap();
        blocker.unlock().unwrap();assert_eq!(waiting.await.err().as_deref(),Some("gpu_gate_job_changed"));
        assert!(clean(&read_marker(&mut OpenOptions::new().read(true).open(&path).unwrap()).unwrap()));
    }
    #[tokio::test]
    async fn gpu_wait_deadline_records_resource_exhaustion_without_marker_mutation(){
        let (_temp,path)=fixture();let (app,_db)=job_fixture().await;
        let job=JobContext::bind(&app,"gpu-fixture",None).await.unwrap();
        let before=std::fs::read(&path).unwrap();let blocker=OpenOptions::new().read(true).write(true).open(&path).unwrap();blocker.lock().unwrap();
        assert_eq!(Lease::acquire_path_observed(&path,"whisper_asr","LikeAvto","fixture",Duration::from_millis(20),None,Some(&job)).await.err().as_deref(),Some("gpu_gate_wait_timeout"));
        let stored=app.db.read_job("gpu-fixture").await.unwrap().unwrap();
        assert_eq!(stored["resourceWait"]["state"],"exhausted");assert_eq!(stored["resourceWait"]["reason"],"gpu_gate_resource_wait_exhausted");
        blocker.unlock().unwrap();assert_eq!(std::fs::read(&path).unwrap(),before);
    }
    #[test]
    fn gpu_job_binding_rejects_attempt_source_and_cached_audio_retargeting(){
        let base=json!({"status":"running","attemptId":"one","audioPin":{"originJobId":"one"},"result":{"visualProgress":{"sourceVersion":"one","leaseId":"one"}}});
        let expected=job_binding(&base);
        for pointer in ["/attemptId","/audioPin/originJobId","/result/visualProgress/sourceVersion","/result/visualProgress/leaseId"]{
            let mut changed=base.clone();*changed.pointer_mut(pointer).unwrap()=json!("other");assert_ne!(job_binding(&changed),expected);
        }
    }
    #[tokio::test]
    async fn gpu_grant_checks_current_company_source_material_and_policy(){
        let (app,_db)=job_fixture().await;
        let job=JobContext::bind(&app,"gpu-fixture",None).await.unwrap();
        let base=app.read().await.unwrap();assert!(job.validate_workspace(&base).is_ok());
        for change in ["account","connector","post","material","cancel","policy"]{
            let mut changed=base.clone();
            match change {
                "account"=>changed["account"]=json!("Other company"),
                "connector"=>changed["connectorBinding"]["id"]=json!("other-connection"),
                "post"=>crate::row_mut(&mut changed,"posts","gpu-post").unwrap()["title"]=json!("Changed source"),
                "material"=>crate::list_mut(&mut changed,"knowledge_entries").push(json!({"id":"new-head","kind":"transcript","currentVersionId":"new-version","scope":{"postKeys":["gpu-post"]}})),
                "cancel"=>crate::row_mut(&mut changed,"jobs","gpu-fixture").unwrap()["status"]=json!("cancelled"),
                "policy"=>{
                    let p=&job.binding["visualProgress"];
                    changed["settings"]["postMediaPolicies"]=json!({"gpu-post":{"status":"active","account":p["account"],"connectorBinding":p["connectorBinding"],"sourceVersion":p["sourceVersion"],"postId":"gpu-post","version":1,"revision":1,"mode":"full_audio_only"}});
                },
                _=>unreachable!(),
            }
            assert!(job.validate_workspace(&changed).is_err(),"must reject {change} change even with original job binding");
        }
    }
    #[tokio::test]
    async fn cached_audio_gpu_grant_checks_origin_and_exact_policy(){
        let (app,_db)=job_fixture().await;
        app.change(|d|{
            let origin=crate::row_mut(d,"jobs","gpu-fixture")?;
            origin["status"]=json!("paused");origin["result"]["visualProgress"]["leaseId"]=Value::Null;
            origin["result"]["visualProgress"]["leaseEpoch"]=json!(2);
            let progress=origin["result"]["visualProgress"].clone();
            d["settings"]["postMediaPolicies"]=json!({"gpu-post":{"status":"active","account":progress["account"],"connectorBinding":progress["connectorBinding"],"sourceVersion":progress["sourceVersion"],"postId":"gpu-post","version":1,"revision":1,"mode":"full_audio_only"}});
            let policy=crate::post_media_policy::effective(d,crate::row(d,"posts","gpu-post")?)?;
            crate::list_mut(d,"jobs").push(json!({"id":"audio-fixture","kind":"media_audio","status":"running","account":progress["account"],"connectorBinding":progress["connectorBinding"],"audioPin":{"originJobId":"gpu-fixture","originEpoch":2,"progress":progress,"policy":policy}}));Ok(())
        }).await.unwrap();
        let job=JobContext::bind(&app,"audio-fixture",None).await.unwrap();
        let base=app.read().await.unwrap();assert!(job.validate_workspace(&base).is_ok());
        for change in ["lease","epoch","status","policy"] {
            let mut d=base.clone();
            match change {
                "lease"=>crate::row_mut(&mut d,"jobs","gpu-fixture").unwrap()["result"]["visualProgress"]["leaseId"]=json!("new-lease"),
                "epoch"=>crate::row_mut(&mut d,"jobs","gpu-fixture").unwrap()["result"]["visualProgress"]["leaseEpoch"]=json!(3),
                "status"=>crate::row_mut(&mut d,"jobs","gpu-fixture").unwrap()["status"]=json!("running"),
                "policy"=>d["settings"]["postMediaPolicies"]["gpu-post"]["revision"]=json!(2),
                _=>unreachable!(),
            }
            assert!(job.validate_workspace(&d).is_err(),"must reject cached audio {change} change");
        }
    }
    #[tokio::test]
    async fn dirty_owner_drop_blocks_reentry(){
        let (_temp,path)=fixture();
        let first=Lease::acquire_path(&path,"asr","BAW","whisper",Duration::ZERO).await.unwrap();
        drop(first); // Simulates cancellation or an owner process losing its handle.
        assert_eq!(Lease::acquire_path(&path,"vision","LikeAvto","ollama",Duration::ZERO).await.err().as_deref(),Some("gpu_gate_dirty_or_invalid"));
        assert!(std::fs::read_to_string(&path).unwrap().contains("dirty"));
    }
    #[tokio::test]
    async fn wait_timeout_never_changes_marker_and_clean_completion_releases_gate(){
        let (_temp,path)=fixture();
        let first=Lease::acquire_path(&path,"asr","BAW","whisper",Duration::ZERO).await.unwrap();
        let second=Lease::acquire_path(&path,"vision","LikeAvto","ollama",Duration::from_millis(20)).await;
        assert_eq!(second.err().as_deref(),Some("gpu_gate_wait_timeout"));
        first.finish().unwrap();
        let next=Lease::acquire_path(&path,"vision","LikeAvto","ollama",Duration::ZERO).await.unwrap();
        next.finish().unwrap();
        assert!(clean(&read_marker(&mut OpenOptions::new().read(true).open(&path).unwrap()).unwrap()));
    }
    #[test]
    fn malformed_and_empty_markers_fail_closed(){
        let (_temp,path)=fixture();let mut file=OpenOptions::new().read(true).write(true).open(&path).unwrap();
        for bytes in [b"".as_slice(),b"{".as_slice(),b"{\"version\":1,\"status\":\"dirty\"}".as_slice()] {
            std::fs::write(&path,bytes).unwrap();
            assert!(!read_marker(&mut file).map(|v|clean(&v)).unwrap_or(false));
        }
    }
    #[test]
    fn hardlinked_gate_is_rejected(){
        let (temp,path)=fixture();
        std::fs::hard_link(&path,temp.path().join("alias.gate")).unwrap();
        let file=OpenOptions::new().read(true).write(true).open(&path).unwrap();
        assert!(validate_file(&file).is_err());
    }
    #[tokio::test]
    async fn wrong_file_identity_never_marks_dirty(){
        let (_temp,path)=fixture();
        let error=Lease::acquire_path_checked(&path,"asr","BAW","whisper",Duration::ZERO,Some("00000000:0000000000000000")).await.err();
        assert_eq!(error.as_deref(),Some("gpu_gate_identity_mismatch"));
        let mut file=OpenOptions::new().read(true).open(&path).unwrap();
        assert!(clean(&read_marker(&mut file).unwrap()));
    }
    #[test]
    fn child_holds_gate_for_fixture(){
        let Some(path)=std::env::var_os("COMMUNITYHERO_TEST_GPU_GATE_PATH") else {return};
        let path=PathBuf::from(path);
        let lease=tokio::runtime::Runtime::new().unwrap().block_on(
            Lease::acquire_path(&path,"vision","BAW","test-ollama",Duration::ZERO)).unwrap();
        std::fs::write(path.with_extension("ready"),b"ready").unwrap();
        if std::env::var_os("COMMUNITYHERO_TEST_GPU_GATE_CRASH").is_some(){std::process::exit(0);}
        let release=path.with_extension("release");
        let started=Instant::now();
        while !release.exists() && started.elapsed()<Duration::from_secs(15){std::thread::sleep(Duration::from_millis(10));}
        if !release.exists(){std::process::exit(2);}
        lease.finish().unwrap();
    }
    fn wait_ready(path:&Path,child:&mut std::process::Child){
        let started=Instant::now();
        while !path.with_extension("ready").exists(){
            assert!(child.try_wait().unwrap().is_none(),"fixture owner exited before admission");
            assert!(started.elapsed()<Duration::from_secs(10),"fixture owner never acquired gate");
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    #[tokio::test]
    async fn independent_processes_serialize_and_crash_remains_dirty(){
        for crash in [false,true]{
            let (_temp,path)=fixture();
            let mut child=std::process::Command::new(std::env::current_exe().unwrap());
            child.arg("--exact").arg("media_processing::gpu_gate::tests::child_holds_gate_for_fixture")
                .env("COMMUNITYHERO_TEST_GPU_GATE_PATH",&path)
                .stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null());
            if crash{child.env("COMMUNITYHERO_TEST_GPU_GATE_CRASH","1");}
            let mut child=child.spawn().unwrap();wait_ready(&path,&mut child);
            if crash{
                assert!(child.wait().unwrap().success());
                assert_eq!(Lease::acquire_path(&path,"asr","LikeAvto","whisper",Duration::ZERO).await.err().as_deref(),Some("gpu_gate_dirty_or_invalid"));
            }else{
                assert_eq!(Lease::acquire_path(&path,"asr","LikeAvto","whisper",Duration::from_millis(20)).await.err().as_deref(),Some("gpu_gate_wait_timeout"));
                std::fs::write(path.with_extension("release"),b"go").unwrap();
                assert!(child.wait().unwrap().success());
                Lease::acquire_path(&path,"asr","LikeAvto","whisper",Duration::ZERO).await.unwrap().finish().unwrap();
            }
        }
    }
}
