//! Product-owned media processing. Connectors resolve sources; this module only
//! processes an admitted source and returns materials for the normal catalog.
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{env, io::Read, path::{Path, PathBuf}, process::Stdio, time::Duration};
use tokio::{io::AsyncReadExt, process::Command};

#[path="media_analysis_output.rs"]
pub(crate) mod analysis_output;
#[cfg(test)]
#[path="media_native_entry_tests.rs"]
mod native_entry_tests;

#[path="media_download_failure.rs"]
mod download_failure;
pub(crate) fn download_failure_policy(code: &str) -> Value { download_failure::policy(code) }

#[derive(Clone, Debug)]
pub(crate) struct MediaSource {
    pub account: String,
    pub post_key: String,
    pub title: String,
    pub source_url: String,
    pub fallback_url: Option<String>,
    pub source_discovery: Option<Value>,
}

impl MediaSource {
    pub fn from_projection(value: &Value, expected_account: &str, expected_post_key: &str) -> Result<Self, String> {
        let field = |key| value[key].as_str().unwrap_or("").trim().to_owned();
        let fallback=field("fallbackUrl");
        let mut source = Self { account: field("account"), post_key: field("postKey"), title: field("title"), source_url: field("sourceUrl"),
            fallback_url:(!fallback.is_empty()).then_some(fallback),source_discovery:None };
        if source.account != expected_account || source.post_key != expected_post_key || source.title.is_empty() {
            return Err("media_source_identity_mismatch".into());
        }
        let Some(authority) = source.source_url.strip_prefix("https://").and_then(|rest| rest.split('/').next()) else {
            return Err("media_source_invalid".into());
        };
        if authority.is_empty() || authority.contains('@') || authority.contains('#') || authority.contains('\\') || source.source_url.len() > 4096 {
            return Err("media_source_invalid".into());
        }
        if source.fallback_url.as_deref().is_some_and(|fallback| {
            let Some(authority)=fallback.strip_prefix("https://").and_then(|rest|rest.split('/').next()) else{return true;};
            authority.is_empty()||authority.contains('@')||authority.contains('#')||authority.contains('\\')||fallback.len()>8192
        }) { return Err("media_source_invalid".into()); }
        source.source_discovery=source_discovery_projection(value.get("sourceDiscovery"));
        if let Some(pin)=value.get("assetPin"){
            crate::media_speech_assets::validate_shape(pin)?;
            crate::media_speech_assets::require_projection(pin,value)?;
        }
        Ok(source)
    }
    pub(crate) fn projection(&self)->Value{
        let mut value=json!({"account":self.account,"postKey":self.post_key,"title":self.title,"sourceUrl":self.source_url,"fallbackUrl":self.fallback_url});
        if let Some(discovery)=&self.source_discovery{value["sourceDiscovery"]=discovery.clone();}
        value
    }
}

// Optional connector-observed diagnostic provenance. Malformed diagnostics are
// omitted after source identity admission; they never alter access or retries.
fn source_discovery_projection(value:Option<&Value>)->Option<Value>{
    let value=value.filter(|v|v.is_object())?;
    let status=value["status"].as_str()?;let stage=value["stage"].as_str()?;
    if value["schemaVersion"]!=1||!["source_admission","deadline","dns","http","redirect","locator_parse","unknown"].contains(&stage){return None;}
    let category=if status=="failed"{
        let category=value["category"].as_str()?;
        if !["timeout","cancelled","network","tls","auth_required","http_forbidden","rate_limited","unavailable","http_failed",
            "invalid_source","redirect_rejected","redirect_limit","too_large","incomplete_response","invalid_response","unknown"].contains(&category){return None;}
        json!(category)
    }else{
        if !["fallback_resolved","no_supported_locator"].contains(&status)||!value["category"].is_null()||stage!="locator_parse"{return None;}
        Value::Null
    };
    let mut clean=json!({"schemaVersion":1,"status":status,"category":category,"stage":stage});
    if let Some(tag)=value.get("platform"){
        let tag=tag.as_str().filter(|s|!s.is_empty()&&s.len()<=40&&s.bytes().all(|b|b.is_ascii_alphanumeric()||b==b'_'||b==b'-'))?;
        clean["platform"]=json!(tag);
    }
    Some(clean)
}

#[derive(Clone, Debug)]
struct MediaConfig {
    ytdlp: PathBuf,
    ytdlp_python: bool,
    ytdlp_node: Option<PathBuf>,
    ffmpeg: PathBuf,
    ffprobe: PathBuf,
    whisper: PathBuf,
    model: PathBuf,
    tesseract: Option<PathBuf>,
    tessdata: Option<PathBuf>,
    scratch: PathBuf,
}

impl MediaConfig {
    fn tool(name: &str) -> Result<PathBuf, String> {
        Self::tool_with(name,&|key|env::var_os(key))
    }
    fn tool_with(name:&str,get:&impl Fn(&str)->Option<std::ffi::OsString>)->Result<PathBuf,String>{
        let value = get(name).ok_or_else(|| format!("media_config_missing_{name}"))?;
        let path = PathBuf::from(value);
        if !path.is_absolute() || !path.is_file() { return Err(format!("media_config_invalid_{name}")); }
        Ok(path)
    }
    fn from_env() -> Result<Self, String> {
        Self::for_phase("all")
    }
    fn for_phase(phase:&str) -> Result<Self,String> {
        Self::for_phase_with(phase,&|key|env::var_os(key))
    }
    fn for_phase_with(phase:&str,get:&impl Fn(&str)->Option<std::ffi::OsString>)->Result<Self,String>{
        let needs=phase_requirements(phase)?;
        let tool=|name|if needs.contains(&name){Self::tool_with(name,get)}else{Ok(PathBuf::new())};
        let (ytdlp, ytdlp_python) = if matches!(phase,"all"|"download") {match (get("COMMUNITYHERO_MEDIA_YTDLP"), get("COMMUNITYHERO_MEDIA_YTDLP_PYTHON")) {
            (Some(_), Some(_)) => return Err("media_config_conflicting_ytdlp".into()),
            (Some(_), None) => (Self::tool_with("COMMUNITYHERO_MEDIA_YTDLP",get)?, false),
            (None, Some(_)) => (Self::tool_with("COMMUNITYHERO_MEDIA_YTDLP_PYTHON",get)?, true),
            (None, None) => return Err("media_config_missing_ytdlp".into()),
        }} else {(PathBuf::new(),false)};
        let scratch = PathBuf::from(get("COMMUNITYHERO_MEDIA_SCRATCH_DIR").ok_or("media_config_missing_scratch")?);
        if !scratch.is_absolute() { return Err("media_config_invalid_scratch".into()); }
        let ytdlp_node=match get("COMMUNITYHERO_MEDIA_YTDLP_NODE").filter(|_|matches!(phase,"all"|"download")) {
            Some(_) => Some(Self::tool_with("COMMUNITYHERO_MEDIA_YTDLP_NODE",get)?),
            None => None,
        };
        let tesseract = match get("COMMUNITYHERO_MEDIA_TESSERACT").filter(|_|matches!(phase,"all"|"finalize")) {
            Some(_) => Some(Self::tool_with("COMMUNITYHERO_MEDIA_TESSERACT",get)?),
            None => None,
        };
        let tessdata=match get("COMMUNITYHERO_MEDIA_TESSDATA_PREFIX").filter(|_|matches!(phase,"all"|"finalize")) {
            Some(value) => {let path=PathBuf::from(value);if !path.is_absolute()||!path.is_dir(){return Err("media_config_invalid_tessdata".into());}Some(path)},
            None=>None,
        };
        Ok(Self { ytdlp, ytdlp_python, ytdlp_node, ffmpeg: tool("COMMUNITYHERO_MEDIA_FFMPEG")?,
            ffprobe: tool("COMMUNITYHERO_MEDIA_FFPROBE")?, whisper: tool("COMMUNITYHERO_MEDIA_WHISPER_CLI")?,
            model: tool("COMMUNITYHERO_MEDIA_WHISPER_MODEL")?, tesseract, tessdata, scratch })
    }
}

fn phase_requirements(phase:&str)->Result<&'static [&'static str],String>{
    match phase {
        "all"|"audio"|"finalize"=>Ok(&["COMMUNITYHERO_MEDIA_FFMPEG","COMMUNITYHERO_MEDIA_FFPROBE","COMMUNITYHERO_MEDIA_WHISPER_CLI","COMMUNITYHERO_MEDIA_WHISPER_MODEL"]),
        "download"=>Ok(&["COMMUNITYHERO_MEDIA_FFMPEG","COMMUNITYHERO_MEDIA_FFPROBE"]),
        "inventory"|"select"|"scan"=>Ok(&["COMMUNITYHERO_MEDIA_FFMPEG"]),
        _=>Err("media_phase_not_claimable".into()),
    }
}

#[path="media_full_processing.rs"]
pub(crate) mod full;
#[path="gpu_media_gate.rs"]
mod gpu_gate;

pub(crate) async fn observe_finalize_catalog_admission(app:&crate::App,id:&str,progress:&Value) {
    if let Ok(observer)=gpu_gate::JobContext::bind(app,id,Some(progress)).await {
        observer.observe_finalize(gpu_gate::FinalizeStage::CatalogAdmission,None,None).await;
    }
}
pub(crate) fn mark_finalize_catalog_admitted(job:&mut Value) {
    gpu_gate::finalize_progress(job,gpu_gate::FinalizeStage::CatalogAdmitted,None,None,&crate::now());
}

#[cfg(test)]
pub(crate) async fn validate_gpu_job_for_test(app:&crate::App,id:&str,progress:&Value)->Result<(),String>{
    gpu_gate::JobContext::bind(app,id,Some(progress)).await?.validate_current_for_test().await
}

pub(crate) fn preflight() -> Result<(), String> {
    preflight_phase("all")
}
pub(crate) fn preflight_phase(phase:&str) -> Result<(), String> {
    MediaConfig::for_phase(phase)?;
    if matches!(phase,"all"|"scan"|"audio"|"finalize"){gpu_gate::preflight()?;}
    let evidence=PathBuf::from(env::var_os("COMMUNITYHERO_MEDIA_EVIDENCE_DIR").ok_or("visual_evidence_dir_missing")?);
    if !evidence.is_absolute(){return Err("visual_evidence_dir_invalid".into());}
    if !matches!(phase,"all"|"scan"){return Ok(());}
    match env::var("COMMUNITYHERO_MEDIA_VISION_BACKEND").as_deref() {
        Ok("local")=>{
            let endpoint=env::var("COMMUNITYHERO_MEDIA_VISION_LOCAL_ENDPOINT").unwrap_or_default();
            if endpoint.strip_prefix("http://127.0.0.1:").and_then(|p|p.parse::<u16>().ok()).is_none()
                || env::var("COMMUNITYHERO_MEDIA_VISION_LOCAL_MODEL").unwrap_or_default().trim().is_empty()
                || !env::var("COMMUNITYHERO_MEDIA_VISION_LOCAL_DIGEST").ok().is_some_and(|s|s.len()==64&&s.bytes().all(|c|c.is_ascii_digit()||(b'a'..=b'f').contains(&c))) {return Err("visual_local_config_missing".into());}
        },
        _=>return Err("visual_backend_not_configured".into()),
    }
    let path=PathBuf::from(env::var_os("COMMUNITYHERO_MEDIA_VISION_DATA_DIR").ok_or("visual_private_dir_missing")?);
    if !path.is_absolute(){return Err("visual_private_dir_invalid".into());}
    Ok(())
}

#[cfg(windows)]
mod process_tree {
    use windows_sys::Win32::{Foundation::{CloseHandle, HANDLE}, System::{JobObjects::{AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation, JobObjectBasicAccountingInformation, JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JOBOBJECT_BASIC_ACCOUNTING_INFORMATION, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE, QueryInformationJobObject, SetInformationJobObject, TerminateJobObject}, Threading::{OpenProcess, PROCESS_SET_QUOTA, PROCESS_TERMINATE}}};
    pub struct TreeGuard(HANDLE);
    unsafe impl Send for TreeGuard {}
    unsafe impl Sync for TreeGuard {}
    impl TreeGuard {
        pub fn attach(pid: u32) -> Result<Self, String> {
            unsafe {
                let job = CreateJobObjectW(std::ptr::null(), std::ptr::null());
                if job.is_null() { return Err("media_process_job_failed".into()); }
                let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
                info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
                let configured = SetInformationJobObject(job, JobObjectExtendedLimitInformation,
                    &info as *const _ as *const _, std::mem::size_of_val(&info) as u32);
                let handle = OpenProcess(PROCESS_SET_QUOTA | PROCESS_TERMINATE, 0, pid);
                let attached = !handle.is_null() && AssignProcessToJobObject(job, handle) != 0;
                if !handle.is_null() { CloseHandle(handle); }
                if configured == 0 || !attached { CloseHandle(job); return Err("media_process_job_failed".into()); }
                Ok(Self(job))
            }
        }
        pub async fn stop_and_wait(&self) -> bool {
            let mut terminated=false;
            let until=tokio::time::Instant::now()+std::time::Duration::from_secs(5);
            loop {
                if !terminated { terminated=unsafe { TerminateJobObject(self.0,1)!=0 }; }
                let mut info: JOBOBJECT_BASIC_ACCOUNTING_INFORMATION=unsafe { std::mem::zeroed() };
                let observed=unsafe { QueryInformationJobObject(self.0,JobObjectBasicAccountingInformation,
                    &mut info as *mut _ as *mut _,std::mem::size_of_val(&info) as u32,std::ptr::null_mut()) };
                if observed!=0 && info.ActiveProcesses==0 { return true; }
                if tokio::time::Instant::now()>=until {return false;}
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            }
        }
    }
    impl Drop for TreeGuard { fn drop(&mut self) { unsafe { CloseHandle(self.0); } } }
}

struct ScratchGuard(PathBuf);
impl Drop for ScratchGuard { fn drop(&mut self) { let _=std::fs::remove_dir_all(&self.0); } }

async fn run_tool(exe: &Path, args: &[String], deadline: Duration, capture: bool, code: &str) -> Result<String, String> {
    run_tool_until(exe,args,||tokio::time::sleep(deadline),capture,code).await
}

const DOWNLOAD_DIAGNOSTIC_LIMIT: usize = 16 * 1024;
// Keep only a bounded in-memory tail while draining the entire pipe, so a noisy
// downloader cannot deadlock on stderr. Raw diagnostics never leave this helper.
async fn read_download_diagnostic<R: tokio::io::AsyncRead + Unpin>(reader: &mut R) -> Vec<u8> {
    let mut tail=Vec::new();let mut chunk=[0u8;4096];
    loop {
        let count=match tokio::io::AsyncReadExt::read(reader,&mut chunk).await {Ok(0)|Err(_)=>break,Ok(n)=>n};
        tail.extend_from_slice(&chunk[..count]);
        if tail.len()>DOWNLOAD_DIAGNOSTIC_LIMIT {tail.drain(..tail.len()-DOWNLOAD_DIAGNOSTIC_LIMIT);}
    }
    tail
}
fn download_failure_code(stderr:&[u8])->&'static str {
    download_failure::classify(stderr)
}

async fn run_tool_until<D, F>(exe: &Path, args: &[String], deadline: D, capture: bool, code: &str) -> Result<String, String>
where D: FnOnce() -> F, F: std::future::Future<Output=()> {
    run_tool_until_observed(exe,args,deadline,capture,code,&mut ToolObservation::default()).await
}

#[derive(Default)]
struct ToolObservation {stage:&'static str,exit_code:Option<i32>,os_error:Option<i32>,stderr_tail_bytes:usize,
    child_cessation_confirmed:bool,process_tree_cessation_confirmed:bool,retry_after:Option<download_failure::RetryAfter>,
    stdout_capture:Vec<u8>,stdout_complete:bool,stdout_eof:bool,stdout_truncated:bool,stdout_status:&'static str}

const TOOL_STDOUT_LIMIT:usize=4*1024*1024;
// Observation lives outside the cancellable future. Keep the first capped
// prefix plus one overflow detection byte, including on read failure/drain.
async fn capture_tool_stdout<R:tokio::io::AsyncRead+Unpin>(reader:&mut R,observation:&mut ToolObservation,code:&str)->Result<(),String>{
    observation.stdout_status="reading";let mut chunk=[0u8;4096];
    loop {
        let available=(TOOL_STDOUT_LIMIT+1).saturating_sub(observation.stdout_capture.len()).min(chunk.len());
        let count=match reader.read(&mut chunk[..available]).await{
            Ok(count)=>count,Err(_)=>{observation.stdout_status="read_failed";return Err(format!("{code}_output_failed"));},
        };
        observation.stdout_capture.extend_from_slice(&chunk[..count]);
        if observation.stdout_capture.len()>TOOL_STDOUT_LIMIT{
            observation.stdout_truncated=true;observation.stdout_status="output_limit";return Err(format!("{code}_output_limit"));
        }
        if count==0{observation.stdout_complete=true;observation.stdout_eof=true;observation.stdout_status="eof";return Ok(());}
    }
}

fn ocr_capture_projection(observation:&ToolObservation,successful:bool,bytes:usize)->Value{
    json!({"status":if successful{"completed"}else{"failed"},"complete":observation.stdout_complete,
        "truncated":observation.stdout_truncated,"streamEof":observation.stdout_eof,"stdoutStatus":if observation.stdout_status.is_empty(){"not_captured"}else{observation.stdout_status},"bytes":bytes,
        "limitBytes":TOOL_STDOUT_LIMIT,"retainedLimitBytes":TOOL_STDOUT_LIMIT+1,"stage":observation.stage,"deadline":observation.stage=="deadline",
        "exitCode":observation.exit_code,"childCessationConfirmed":observation.child_cessation_confirmed,
        "processTreeCessationConfirmed":observation.process_tree_cessation_confirmed})
}

fn mark_tool_deadline(observation:&mut ToolObservation){
    observation.stage="deadline";observation.stdout_complete=false;observation.stdout_status="deadline";
}

fn observe_tool_exit(observation:&mut ToolObservation,success:bool,exit_code:Option<i32>,diagnostic:&[u8],diagnose_download:bool,code:&str)->Result<(),String>{
    observation.stage="exit";observation.exit_code=exit_code;observation.stderr_tail_bytes=diagnostic.len();
    if diagnose_download&&!success{observation.retry_after=download_failure::retry_after(diagnostic,chrono::Utc::now());}
    if !success{return Err(if diagnose_download{download_failure_code(diagnostic).to_owned()}else{code.to_owned()});}
    Ok(())
}

async fn run_tool_until_observed<D, F>(exe: &Path, args: &[String], deadline: D, capture: bool, code: &str,
    observation:&mut ToolObservation) -> Result<String, String>
where D: FnOnce() -> F, F: std::future::Future<Output=()> {
    run_tool_until_observed_bytes(exe,args,deadline,capture,code,observation).await
        .map(|bytes|String::from_utf8_lossy(&bytes).into_owned())
}

// OCR keeps exact stdout bytes in CAS before deriving a UTF-8 prompt projection.
// The byte capture shares the existing ownership, output limit and settlement.
async fn run_tool_until_observed_bytes<D, F>(exe: &Path, args: &[String], deadline: D, capture: bool, code: &str,
    observation:&mut ToolObservation) -> Result<Vec<u8>, String>
where D: FnOnce() -> F, F: std::future::Future<Output=()> {
    let registry=crate::runtime_owned_work::current().map_err(|_|format!("{code}_runtime_owner_absent"))?;
    let mut command=Command::new(exe);
    let diagnose_download=code=="source_download_failed"&&!capture;
    command.args(args).stdin(Stdio::null()).stderr(if diagnose_download {Stdio::piped()}else{Stdio::null()})
        .stdout(if capture {Stdio::piped()}else{Stdio::null()});
    if args.first().is_some_and(|arg|arg=="-m") {
        command.env_remove("PYTHONPATH").env_remove("PYTHONHOME").env_remove("PYTHONSTARTUP")
            .env("PYTHONSAFEPATH","1").env("PYTHONNOUSERSITE","1");
    }
    observation.stage="spawn";
    let native=registry.begin_child(crate::runtime_owned_work::Kind::MediaTool).map_err(|_|format!("{code}_runtime_drain_not_dispatched"))?;
    let mut child=crate::runtime_native_child::OwnedChild::spawn_admitted_observed(native,&mut command,&mut observation.os_error)
        .await.map_err(|error|{
            if error.1=="Native child not spawned" {format!("{code}_spawn_failed")}
            else if diagnose_download {"source_download_failed_process_unknown".into()}
            else {format!("{code}_native_admission_or_containment_failed")}
        })?;
    observation.stage="running";
    let result={
        let raw=child.child_mut();
        let output=async {
            let mut stderr=raw.stderr.take();
            if capture {
                let mut stdout=raw.stdout.take().ok_or_else(||format!("{code}_output_failed"))?;
                capture_tool_stdout(&mut stdout,observation,code).await?;
            }
            let (status,diagnostic)=tokio::join!(raw.wait(),async {
                match stderr.as_mut(){Some(reader)=>read_download_diagnostic(reader).await,None=>Vec::new()}
            });
            observation.stage="wait";
            let status=status.map_err(|error|{observation.os_error=error.raw_os_error();format!("{code}_wait_failed")})?;
            observe_tool_exit(observation,status.success(),status.code(),&diagnostic,diagnose_download,code)
        };
        let deadline_signal=deadline();tokio::pin!(deadline_signal);
        tokio::select!{result=output=>result,_=&mut deadline_signal=>Err(format!("{code}_timeout"))}
    };
    if result.as_ref().is_err_and(|error|error==&format!("{code}_timeout")){
        mark_tool_deadline(observation);
    }
    if result.is_err(){let _=child.child_mut().start_kill();}
    let contained=child.settle().await.is_ok();
    observation.child_cessation_confirmed=contained;observation.process_tree_cessation_confirmed=contained;
    // Any failed cleanup is an owned unresolved effect, including successful
    // non-download tools. Never infer quiet from a completed OCR/model result.
    if !contained{return Err(if diagnose_download {"source_download_failed_process_unknown".into()}else{format!("{code}_process_unknown")});}
    // Success moves the existing buffer; every failure preserves it for OCR.
    result.map(|()|std::mem::take(&mut observation.stdout_capture))
}
// Intent and outcome use only source bindings and closed facts. Neither child
// stderr nor the source locator, title, post key or executable path is persisted.
fn download_attempt_record(attempt:&str,source:&MediaSource,locator:&str,ordinal:usize,
    observation:Option<&ToolObservation>,result:Option<&Result<PathBuf,String>>)->Value{
    let binding=digest(&serde_json::to_string(&json!([source.account,source.post_key,locator])).unwrap());
    let category=result.map(|result|match result{
        Ok(_)=>"completed",
        Err(error) if download_failure::known(error)=>error.as_str(),
        Err(_)=>"source_download_failed_unknown",
    });
    let mut record=json!({"schemaVersion":1,"attemptId":attempt,"createdAtUtc":super::now(),"sourceBindingSha256":binding,
        "locatorOrdinal":ordinal,"stage":observation.map(|o|o.stage).unwrap_or("intent"),"category":category,
        "exitCode":observation.and_then(|o|o.exit_code),"osError":observation.and_then(|o|o.os_error),
        "stderrTailBytes":observation.map(|o|o.stderr_tail_bytes),
        "childCessationConfirmed":observation.map(|o|o.child_cessation_confirmed),
        "processTreeCessationConfirmed":observation.map(|o|o.process_tree_cessation_confirmed),
        "retryAfter":if category==Some("source_download_failed_rate_limited") {
            observation.and_then(|o|o.retry_after.as_ref()).map(download_failure::RetryAfter::metadata)
        } else {None},
        "recovery":category.map(download_failure_policy)});
    if let Some(discovery)=&source.source_discovery{record["sourceDiscovery"]=discovery.clone();}
    record
}
fn persist_download_record(dir:&Path,attempt:&str,kind:&str,record:&Value)->Result<(),String>{
    use std::io::Write;
    let metadata=std::fs::symlink_metadata(dir).map_err(|_|"source_download_diagnostic_unavailable")?;
    if !dir.is_absolute()||!metadata.is_dir()||metadata.file_type().is_symlink(){return Err("source_download_diagnostic_unavailable".into());}
    let mut options=std::fs::OpenOptions::new();options.write(true).create_new(true);
    #[cfg(unix)] {use std::os::unix::fs::OpenOptionsExt;options.mode(0o600);}
    let mut file=options.open(dir.join(format!("download-attempt-{attempt}.{kind}.json"))).map_err(|_|"source_download_diagnostic_unavailable")?;
    file.write_all(&serde_json::to_vec(record).map_err(|_|"source_download_diagnostic_unavailable")?).and_then(|_|file.sync_all())
        .map_err(|_|"source_download_diagnostic_unavailable".into())
}
async fn download_source(config:&MediaConfig,source:&MediaSource,work:&Path,deadline:Duration)->Result<PathBuf,String>{
    let dir=PathBuf::from(env::var_os("COMMUNITYHERO_MEDIA_VISION_DATA_DIR").ok_or("source_download_diagnostic_unavailable")?);
    download_source_in(config,source,work,&dir,deadline).await
}
async fn download_source_in(config:&MediaConfig,source:&MediaSource,work:&Path,dir:&Path,deadline:Duration)->Result<PathBuf,String>{
    download_source_with(config,source,work,dir,deadline,|args,until|async move {
        let mut observation=ToolObservation::default();
        let result=run_tool_until_observed(&config.ytdlp,&args,||tokio::time::sleep_until(until),false,"source_download_failed",&mut observation).await;
        (result,observation)
    }).await
}
// This seam exercises the real recovery state machine without network access or
// changing process-wide runtime environment in parallel tests.
async fn download_source_with<R,F>(config:&MediaConfig,source:&MediaSource,work:&Path,dir:&Path,deadline:Duration,mut run:R)->Result<PathBuf,String>
where R:FnMut(Vec<String>,tokio::time::Instant)->F,
    F:std::future::Future<Output=(Result<String,String>,ToolObservation)> {
    let until=tokio::time::Instant::now()+deadline;
    let mut transient_retry_used=false;
    for (ordinal,locator) in std::iter::once(&source.source_url).chain(source.fallback_url.iter()).enumerate(){
        let mut transport="default";
        loop {
            // The same deadline covers all locators, transports and retry delays.
            if tokio::time::Instant::now()>=until {return Err("source_download_failed_timeout".into());}
            let attempt=uuid::Uuid::new_v4().to_string();
            let mut intent=download_attempt_record(&attempt,source,locator,ordinal,None,None);
            intent["transport"]=json!(transport);
            intent["transientRetryUsed"]=json!(transient_retry_used);
            persist_download_record(dir,&attempt,"intent",&intent)?;
            let mut args=download_args(config,work);
            if transport=="youtube_hls" {
                if let Some(index)=args.iter().position(|v|v=="--format") {args[index+1]=YOUTUBE_HLS_FORMAT.into();}
            }
            args.push(locator.clone());
            let (result,observation)=run(args,until).await;
            let result=if result.is_ok()&&!observation.child_cessation_confirmed {
                Err("source_download_failed_process_unknown".into())
            } else {result.and_then(|_|downloaded(work))};
            let mut record=download_attempt_record(&attempt,source,locator,ordinal,Some(&observation),Some(&result));
            record["transport"]=json!(transport);
            record["transientRetryUsed"]=json!(transient_retry_used);
            let error_code=result.as_ref().err().map(String::as_str).unwrap_or("completed");
            let recovery_ready=observation.process_tree_cessation_confirmed&&tokio::time::Instant::now()<until;
            record["recovery"]["automaticRetryPermitted"]=json!(recovery_ready&&!transient_retry_used
                &&download_failure::transient(error_code)&&tokio::time::Instant::now()+Duration::from_secs(1)<until);
            record["recovery"]["sameSourceHlsPermitted"]=json!(recovery_ready&&transport=="default"&&youtube_hls_fallback(locator,Some(error_code)));
            record["recovery"]["alternateBoundLocatorPermitted"]=json!(recovery_ready&&ordinal==0&&source.fallback_url.is_some()&&fallback_after_download_error(error_code));
            persist_download_record(dir,&attempt,"outcome",&record)?;
            let error=match result {Ok(path)=>return Ok(path),Err(error)=>error};
            // Reaping the direct child on other platforms still permits a normal
            // completed file. Recovery needs stronger OS process-tree evidence;
            // until a portable guard is verified it stays disabled there.
            if !observation.process_tree_cessation_confirmed {return Err(error);}
            if download_failure::transient(&error)&&!transient_retry_used {
                transient_retry_used=true;
                let delay=tokio::time::Instant::now()+Duration::from_secs(1);
                if delay>=until {return Err(error);}
                clear_download_residue(work)?;
                tokio::time::sleep_until(delay).await;
                continue;
            }
            // Existing evidence permits this same-source transport once, only
            // for an unspecified YouTube 403, never login/challenge/expiry.
            if transport=="default"&&youtube_hls_fallback(locator,Some(&error)) {
                clear_download_residue(work)?;
                transport="youtube_hls";
                continue;
            }
            if ordinal==0&&source.fallback_url.is_some()&&fallback_after_download_error(&error) {
                clear_download_residue(work)?;
                break;
            }
            return Err(error);
        }
    }
    Err("source_file_missing".into())
}

fn strings(values: &[&str]) -> Vec<String> { values.iter().map(|value| (*value).to_owned()).collect() }
fn normalized(text: &str) -> String { text.split_whitespace().collect::<Vec<_>>().join(" ") }
fn digest(value: &str) -> String { format!("{:x}", Sha256::digest(value.as_bytes())) }
fn audio_coverage(media: Option<f64>, audio: Option<f64>) -> (bool, &'static str) {
    match (media,audio) {
        (Some(media),Some(audio)) if audio+0.25>=media => (false,"full_audio"),
        (Some(_),Some(_)) => (true,"audio_shorter_than_media"),
        (_,None) => (true,"audio_duration_unknown"),
        (None,Some(_)) => (true,"media_duration_unknown"),
    }
}
const AUDIO_CHUNK_SECONDS: f64 = 900.0;
const MAX_AUDIO_CHUNKS: usize = 16;
const MAX_TRANSCRIPT_BYTES: usize = 512 * 1024;

fn audio_windows(duration:f64)->Result<Vec<(f64,f64)>,String>{
    if !duration.is_finite()||duration<=0.0{return Err("audio_duration_unknown".into());}
    let count=(duration/AUDIO_CHUNK_SECONDS).ceil();
    if count>MAX_AUDIO_CHUNKS as f64{return Err("audio_duration_limit".into());}
    Ok((0..count as usize).map(|i|{
        let start=i as f64*AUDIO_CHUNK_SECONDS;
        (start,(duration-start).min(AUDIO_CHUNK_SECONDS))
    }).collect())
}
fn audio_segment_covered(expected:f64,observed:Option<f64>)->bool{
    observed.is_some_and(|actual|actual.is_finite()&&actual>0.0&&actual+0.25>=expected&&actual<=expected+0.25)
}
fn file_digest(path: &Path) -> Result<String,String> {
    let file=std::fs::File::open(path).map_err(|_|"source_file_missing".to_owned())?;
    if file.metadata().map_err(|_|"source_file_missing".to_owned())?.len()>500*1024*1024 { return Err("source_file_too_large".into()); }
    let mut reader=std::io::BufReader::new(file);
    let mut sha=Sha256::new();
    let mut buffer=[0u8;1024*1024];
    loop { let size=reader.read(&mut buffer).map_err(|_|"source_file_read_failed".to_owned())?; if size==0{break;} sha.update(&buffer[..size]); }
    Ok(format!("{:x}",sha.finalize()))
}
async fn probe_duration(config: &MediaConfig, input: &Path) -> Option<f64> {
    let args=vec!["-v".into(),"error".into(),"-show_entries".into(),"format=duration".into(),"-of".into(),"default=nw=1:nk=1".into(),input.display().to_string()];
    run_tool(&config.ffprobe,&args,Duration::from_secs(120),true,"media_probe_failed").await.ok()
        .and_then(|text|text.trim().parse::<f64>().ok()).filter(|value|value.is_finite()&&*value>0.0)
}
fn video_stream_args(input: &Path) -> Vec<String> {
    vec!["-v".into(),"error".into(),"-select_streams".into(),"v".into(), "-show_entries".into(),
        "stream=index".into(),"-of".into(),"csv=p=0".into(),input.display().to_string()]
}
async fn has_video_stream(config: &MediaConfig, input: &Path) -> Option<bool> {
    run_tool(&config.ffprobe,&video_stream_args(input),Duration::from_secs(120),true,"ocr_probe_failed")
        .await.ok().map(|output|!output.trim().is_empty())
}

fn downloaded(work: &Path) -> Result<PathBuf, String> {
    let mut files=Vec::new();
    for entry in std::fs::read_dir(work).map_err(|_|"source_file_missing")? {
        let entry=entry.map_err(|_|"source_file_missing")?;
        let path=entry.path();
        if path.file_name().and_then(|v|v.to_str()).is_some_and(|name|name.starts_with("source.")) {
            let metadata=std::fs::symlink_metadata(&path).map_err(|_|"source_file_missing")?;
            if !metadata.is_file()||metadata.file_type().is_symlink()||metadata.len()==0
                || path.file_name().and_then(|v|v.to_str()).is_some_and(|name|name.ends_with(".part")||name.ends_with(".ytdl")) {
                return Err("source_download_failed_integrity".into());
            }
            files.push(path);
        }
    }
    if files.len() != 1 { return Err("source_file_missing".into()); }
    let file=files.remove(0);
    // Our exact output template is source.%(ext)s. A lone source.f137.mp4
    // (or source.faudio.m4a) is an intermediate stream, not a merged result.
    if file.file_name().and_then(|name|name.to_str()).and_then(|name|name.strip_prefix("source."))
        .is_none_or(|extension|extension.is_empty()||extension.contains('.')) {
        return Err("source_download_failed_integrity".into());
    }
    // yt-dlp's bound applies to each stream; enforce the source bound again on
    // the final merged container before probing, copying to CAS or decoding.
    if std::fs::metadata(&file).map_err(|_|"source_file_missing")?.len()>500*1024*1024 {
        return Err("source_file_too_large".into());
    }
    Ok(file)
}
fn fallback_after_download_error(error:&str)->bool{
    download_failure::alternate_locator(error)
}
fn clear_download_residue(work:&Path)->Result<(),String>{
    for entry in std::fs::read_dir(work).map_err(|_|"media_scratch_unavailable".to_owned())? {
        let entry=entry.map_err(|_|"media_scratch_unavailable".to_owned())?;
        let path=entry.path();
        if path.is_file()&&path.file_name().and_then(|name|name.to_str()).is_some_and(|name|name.starts_with("source.")){
            std::fs::remove_file(path).map_err(|_|"media_scratch_unavailable".to_owned())?;
        }
    }
    Ok(())
}

fn tsv_lines(tsv: &str) -> Vec<String> {
    use std::collections::BTreeMap;
    let mut groups: BTreeMap<(String,String,String,String), Vec<String>> = BTreeMap::new();
    for row in tsv.lines().skip(1) {
        let fields: Vec<_> = row.split('\t').collect();
        if fields.len() < 12 || fields[10].parse::<f64>().unwrap_or(-1.0) < 60.0 || !fields[11].chars().any(char::is_alphanumeric) { continue; }
        let key=(fields[1].into(),fields[2].into(),fields[3].into(),fields[4].into());
        groups.entry(key).or_default().push(fields[11].trim().to_owned());
    }
    groups.into_values().map(|words| normalized(&words.join(" ")))
        .filter(|line| line.chars().filter(|c| c.is_alphanumeric()).count() >= 4).collect()
}

const OCR_PROMPT_BYTES:usize=12000;

// The current visual source is bound independently of any reused audio donor.
fn ocr_capture_binding(source:&MediaSource,request:&Value,reference:&crate::media_artifacts::ArtifactRef)->Result<Value,String>{
    let alias=&request["originalAlias"];
    if source.account.is_empty()||source.post_key.is_empty()||request["companyId"]!=source.account
        || alias["account"]!=source.account||alias["sourcePostKey"]!=source.post_key
        || request["sourceVersion"].as_str().is_none_or(str::is_empty)
        || alias["sourceVersion"]!=request["sourceVersion"]||!alias["connectorBinding"].is_object()
        || alias["sourceProjection"]["sourceUrl"]!=source.source_url {
        return Err("ocr_source_binding_invalid".into());
    }
    Ok(json!({"companyId":request["companyId"],"account":source.account,"postKey":source.post_key,
        "sourceVersion":request["sourceVersion"],"connectorBinding":alias["connectorBinding"],
        "sourceLocatorSha256":digest(&source.source_url),"source":reference.to_json()}))
}

fn retain_ocr_sample(store:&crate::media_artifacts::ArtifactStore,binding:&Value,sample:&mut Value,raw:&[u8],observation:&ToolObservation)->Result<Vec<String>,String>{
    // Exact TSV includes low-confidence/noisy rows which prompt filtering drops.
    sample["rawOutput"]=store.put_bytes(raw).map_err(|_|"ocr_raw_capture_failed")?.to_json();
    sample["bindingSha256"]=json!(crate::media_fullframes::hash(binding));
    sample["status"]=json!("completed");
    sample["capture"]=ocr_capture_projection(observation,true,raw.len());
    Ok(tsv_lines(&String::from_utf8_lossy(raw)))
}

fn retain_failed_ocr_sample(store:&crate::media_artifacts::ArtifactStore,binding:&Value,sample:&mut Value,reason:&str,observation:&ToolObservation)->Result<(),String>{
    sample["rawOutput"]=if observation.stdout_status.is_empty()&&observation.stdout_capture.is_empty(){Value::Null}
        else{store.put_bytes(&observation.stdout_capture).map_err(|_|"ocr_raw_capture_failed")?.to_json()};
    sample["bindingSha256"]=json!(crate::media_fullframes::hash(binding));sample["status"]=json!("failed");
    sample["reason"]=json!(reason);sample["capture"]=ocr_capture_projection(observation,false,observation.stdout_capture.len());
    Ok(())
}

fn retain_ocr_result(store:&crate::media_artifacts::ArtifactStore,binding:&Value,spec:&Value,samples:&[Value],full_text:&str,mut meta:Value)->Result<(String,Value),String>{
    let normalized=store.put_bytes(full_text.as_bytes()).map_err(|_|"ocr_normalized_capture_failed")?.to_json();
    let text=full_text[..full_text.floor_char_boundary(full_text.len().min(OCR_PROMPT_BYTES))].to_owned();
    let projection=json!({"maxBytes":OCR_PROMPT_BYTES,"bytes":text.len(),"fullBytes":full_text.len(),"truncated":text.len()<full_text.len()});
    let manifest=json!({"schemaVersion":1,"kind":"sampled_visual_ocr","binding":binding,"spec":spec,
        "samples":samples,"normalizedOutput":normalized,"outcome":meta,"promptProjection":projection});
    let manifest=crate::media_fullframes::put(store,&manifest)?;
    meta["promptProjection"]=projection;
    let retained=json!({"schemaVersion":1,"kind":"sampled_visual_ocr","binding":binding,
        "specSha256":crate::media_fullframes::hash(spec),"manifest":manifest,"normalizedOutput":normalized});
    // Creation and cold resolution share the same verified closure consumer.
    // Failed samples remain observations; this does not admit visual identity.
    let (verified,_) = read_retained_ocr(store,&retained,binding,spec)?;
    if verified!=full_text{return Err("ocr_normalized_capture_changed".into());}
    meta["retainedEvidence"]=retained;
    Ok((text,meta))
}

/// Return the full normalized text plus per-sample raw/frame references only
/// after exact binding/spec admission and verified CAS readback. No API call,
/// regeneration, catalog applicability or ASR/visual equivalence is implied.
pub(crate) fn read_retained_ocr(store:&crate::media_artifacts::ArtifactStore,retained:&Value,expected_binding:&Value,expected_spec:&Value)->Result<(String,Vec<Value>),String>{
    let unavailable=|_|"ocr_retained_artifact_unavailable".to_owned();
    if retained["schemaVersion"]!=1||retained["kind"]!="sampled_visual_ocr"||retained["binding"]!=*expected_binding
        ||retained["specSha256"]!=crate::media_fullframes::hash(expected_spec){return Err("ocr_retained_binding_changed".into());}
    let manifest_ref=crate::media_artifacts::ArtifactRef::from_json(&retained["manifest"]).map_err(unavailable)?;
    let bytes=store.read_bytes(&manifest_ref,1024*1024).map_err(unavailable)?;
    let manifest:Value=serde_json::from_slice(&bytes).map_err(|_|"ocr_retained_manifest_invalid")?;
    if manifest["schemaVersion"]!=1||manifest["kind"]!="sampled_visual_ocr"||manifest["binding"]!=*expected_binding
        ||manifest["spec"]!=*expected_spec||manifest["normalizedOutput"]!=retained["normalizedOutput"]{
        return Err("ocr_retained_binding_changed".into());
    }
    let source=crate::media_artifacts::ArtifactRef::from_json(&expected_binding["source"]).map_err(unavailable)?;
    store.verify(&source).map_err(unavailable)?;
    let interval=expected_spec["intervalSeconds"].as_f64().filter(|v|v.is_finite()&&*v>0.0).ok_or("ocr_retained_manifest_invalid")?;
    if manifest["outcome"]["intervalSeconds"]!=interval {return Err("ocr_retained_manifest_invalid".into());}
    let samples=manifest["samples"].as_array().filter(|samples|!samples.is_empty()&&samples.len()<=30).ok_or("ocr_retained_manifest_invalid")?;
    let mut failed=0;let mut successful_lines=Vec::new();let mut seen=std::collections::HashSet::new();
    for (index,sample) in samples.iter().enumerate(){
        if sample["index"]!=index||sample["bindingSha256"]!=crate::media_fullframes::hash(expected_binding)
            ||sample["nominalSampleSeconds"]!=index as f64*interval{return Err("ocr_retained_manifest_invalid".into());}
        let frame=crate::media_artifacts::ArtifactRef::from_json(&sample["frame"]).map_err(unavailable)?;
        store.verify(&frame).map_err(unavailable)?;
        let capture=&sample["capture"];
        if capture["status"]!=sample["status"]||capture["complete"].as_bool().is_none()||capture["truncated"].as_bool().is_none()||capture["streamEof"].as_bool().is_none()
            ||capture["limitBytes"]!=TOOL_STDOUT_LIMIT||capture["retainedLimitBytes"]!=TOOL_STDOUT_LIMIT+1
            ||capture["deadline"]!=(capture["stage"]=="deadline")
            ||(capture["complete"]==true&&(capture["truncated"]==true||capture["streamEof"]!=true)){return Err("ocr_retained_manifest_invalid".into());}
        let raw=if sample["rawOutput"].is_null(){
            if capture["bytes"]!=0{return Err("ocr_retained_manifest_invalid".into());}None
        }else{
            let raw=crate::media_artifacts::ArtifactRef::from_json(&sample["rawOutput"]).map_err(unavailable)?;
            if raw.bytes>(TOOL_STDOUT_LIMIT+1) as u64||capture["bytes"]!=raw.bytes
                ||(raw.bytes>TOOL_STDOUT_LIMIT as u64&&(capture["truncated"]!=true||capture["complete"]!=false))
                ||(capture["truncated"]==true&&raw.bytes!=(TOOL_STDOUT_LIMIT+1) as u64){return Err("ocr_retained_manifest_invalid".into());}
            store.verify(&raw).map_err(unavailable)?;Some(raw)
        };
        match sample["status"].as_str(){
            Some("completed")=>{
                if capture["complete"]!=true||capture["streamEof"]!=true||capture["truncated"]!=false||capture["stdoutStatus"]!="eof"||capture["exitCode"]!=0
                    ||capture["stage"]!="exit"||capture["childCessationConfirmed"]!=true||capture["processTreeCessationConfirmed"]!=true{
                    return Err("ocr_retained_manifest_invalid".into());
                }
                let raw=raw.ok_or("ocr_retained_manifest_invalid")?;
                let raw=store.read_bytes(&raw,TOOL_STDOUT_LIMIT as u64).map_err(unavailable)?;
                for line in tsv_lines(&String::from_utf8_lossy(&raw)){if seen.insert(line.to_lowercase()){successful_lines.push(line);}}
            },
            Some("failed") if sample["reason"].as_str().is_some_and(|v|!v.is_empty())=>failed+=1,
            _=>return Err("ocr_retained_manifest_invalid".into()),
        }
    }
    let normalized=crate::media_artifacts::ArtifactRef::from_json(&retained["normalizedOutput"]).map_err(unavailable)?;
    let text=String::from_utf8(store.read_bytes(&normalized,128*1024*1024).map_err(unavailable)?).map_err(|_|"ocr_retained_text_invalid")?;
    // Failed/truncated/nonzero output is evidence only, never normalized words.
    if text!=successful_lines.join("\n"){return Err("ocr_retained_text_invalid".into());}
    let outcome=ocr_sample_outcome(&text,samples.len(),failed,interval);
    for key in ["status","coverage","exhaustive","sampledFrames","failedFrames","maxFrames"]{
        if manifest["outcome"][key]!=outcome[key]{return Err("ocr_retained_manifest_invalid".into());}
    }
    Ok((text,samples.clone()))
}

fn ocr_retention_failure(reason:&str)->(String,Value){
    (String::new(),json!({"status":"failed","reason":reason,"coverage":"unavailable","exhaustive":false}))
}

async fn ocr(config: &MediaConfig, source:&MediaSource, request:&Value, input: &Path, work: &Path, duration: Option<f64>) -> (String, Value) {
    match has_video_stream(config,input).await {
        Some(false)=>return (String::new(),json!({"status":"not_applicable","reason":"no_video_stream","coverage":"not_applicable"})),
        None=>return (String::new(),json!({"status":"unavailable","reason":"ocr_probe_failed","coverage":"unavailable"})),
        Some(true)=>{},
    }
    let Some(tesseract) = &config.tesseract else { return (String::new(), json!({"status":"unavailable","reason":"tesseract_not_configured","coverage":"unavailable"})); };
    // Fail before OCR dispatch if there is nowhere to retain the full output.
    let store=match crate::media_fullframes::store(){Ok(store)=>store,Err(_)=>return ocr_retention_failure("ocr_store_unavailable")};
    let reference=match store.put_file(input){Ok(reference)=>reference,Err(_)=>return ocr_retention_failure("ocr_source_capture_failed")};
    let binding=match ocr_capture_binding(source,request,&reference){Ok(binding)=>binding,Err(reason)=>return ocr_retention_failure(&reason)};
    let tool_hashes=match (file_digest(&config.ffmpeg),file_digest(tesseract)){
        (Ok(decoder),Ok(engine))=>(decoder,engine),_=>return ocr_retention_failure("ocr_tool_identity_unavailable"),
    };
    let frames=work.join("frames");
    if std::fs::create_dir(&frames).is_err() { return (String::new(), json!({"status":"failed","reason":"ocr_scratch_failed"})); }
    let interval=duration.filter(|value| value.is_finite()&&*value > 0.0).map_or(3.0, |value| (value / 30.0).max(2.0));
    let interval=(interval*1000.0).round()/1000.0;
    let args=vec!["-nostdin".into(),"-hide_banner".into(),"-loglevel".into(),"error".into(),"-y".into(),"-i".into(),input.display().to_string(),
        "-vf".into(),format!("fps=1/{interval:.3},scale=1600:-2:force_original_aspect_ratio=decrease"),"-frames:v".into(),"30".into(),frames.join("frame-%03d.jpg").display().to_string()];
    if run_tool(&config.ffmpeg,&args,Duration::from_secs(900),false,"ocr_frames_failed").await.is_err() {
        return (String::new(), json!({"status":"failed","reason":"ocr_frames_failed"}));
    }
    let mut paths=match std::fs::read_dir(&frames) { Ok(items)=>items.filter_map(Result::ok).map(|item|item.path()).filter(|p|p.extension().is_some_and(|ext|ext=="jpg")).collect::<Vec<_>>(), Err(_)=>vec![] };
    paths.sort();
    if paths.is_empty() { return (String::new(),json!({"status":"unavailable","reason":"no_video_frames","sampledFrames":0})); }
    let mut seen=std::collections::HashSet::new();
    let mut lines=Vec::new();
    let mut failed=0;
    let mut samples=Vec::new();
    let spec=json!({"decoderSha256":tool_hashes.0,"ocrSha256":tool_hashes.1,"language":"rus+eng","psm":11,
        "format":"tsv","normalizer":"tsv_confidence_60_deduplicated_v1","intervalSeconds":interval,"maxFrames":30,
        "timestampBasis":"nominal_ffmpeg_fps_output_index","scale":"1600:-2:force_original_aspect_ratio=decrease"});
    for (index,path) in paths.iter().take(30).enumerate() {
        let frame=match store.put_file(path){Ok(frame)=>frame.to_json(),Err(_)=>return ocr_retention_failure("ocr_frame_capture_failed")};
        let mut sample=json!({"index":index,"nominalSampleSeconds":index as f64*interval,"frame":frame,
            "bindingSha256":crate::media_fullframes::hash(&binding),"status":"failed","rawOutput":null});
        let mut args=vec![path.display().to_string(),"stdout".into()];
        if let Some(tessdata)=&config.tessdata {args.extend(["--tessdata-dir".into(),tessdata.display().to_string()]);}
        args.extend(strings(&["-l","rus+eng","--psm","11","tsv"]));
        let mut observation=ToolObservation::default();
        match run_tool_until_observed_bytes(tesseract,&args,||tokio::time::sleep(Duration::from_secs(120)),true,"ocr_failed",&mut observation).await {
            Ok(value)=>match retain_ocr_sample(&store,&binding,&mut sample,&value,&observation){
                Ok(output)=>for line in output {if seen.insert(line.to_lowercase()){lines.push(line);}},
                Err(reason)=>return ocr_retention_failure(&reason),
            },
            Err(reason)=>{failed+=1;if let Err(reason)=retain_failed_ocr_sample(&store,&binding,&mut sample,&reason,&observation){return ocr_retention_failure(&reason);}},
        }
        samples.push(sample);
    }
    let text=lines.join("\n");
    if file_digest(input).as_deref()!=Ok(reference.sha256.as_str()) {return ocr_retention_failure("ocr_source_changed_during_capture");}
    let mut meta=ocr_sample_outcome(&text,paths.len().min(30),failed,interval);
    if failed>0{meta["reason"]=json!(if failed==paths.len().min(30){"ocr_failed"}else{"some_ocr_frames_failed"});}
    match retain_ocr_result(&store,&binding,&spec,&samples,&text,meta){Ok(result)=>result,Err(reason)=>ocr_retention_failure(&reason)}
}

fn ocr_sample_outcome(text:&str,sampled:usize,failed:usize,interval:f64)->Value{
    let status=if failed==sampled {"failed"} else if failed>0 {"partial"}
        else if text.trim().is_empty(){"no_text_found"}else{"completed"};
    json!({"status":status,"coverage":"sampled_frames","exhaustive":false,
        "sampledFrames":sampled,"failedFrames":failed,"maxFrames":30,"intervalSeconds":interval})
}

// ASR is already complete before optional OCR configuration is inspected.
// A missing or invalid OCR dependency is an observation, never missing speech.
async fn media_owned_stage<T>(future:impl std::future::Future<Output=Result<T,String>>)->Result<T,String>{
    let registry=crate::runtime_owned_work::current().map_err(|e|e.1)?;
    // Fresh stage admission precedes its durable writer admission. Only native
    // children inherit this exact active stage ticket after drain closes.
    let work=registry.begin(crate::runtime_owned_work::Kind::MediaTool).map_err(|e|e.1)?;
    crate::runtime_owned_work::with_admitted(work,future).await
}
fn unavailable_ocr(reason:String)->(String,Value){
    let reason=if reason.len()<=160&&reason.bytes().all(|c|c.is_ascii_alphanumeric()||c==b'_'){reason}else{"media_followup_not_admitted".into()};
    (String::new(),json!({"status":"unavailable","reason":"lifecycle_draining","gateReason":reason,"coverage":"unavailable"}))
}
async fn ocr_after_admission<D,F>(lifecycle:Option<&dyn analysis_output::AudioLifecycle>,dispatch:D)->(String,Value)
where D:FnOnce()->F,F:std::future::Future<Output=(String,Value)> {
    // OCR is a new stage even when invoked inside an admitted ASR scope. A
    // closed registry cannot inherit ASR authority for this follow-up.
    match media_owned_stage(async{Ok(ocr_after_admission_owned(lifecycle,dispatch).await)}).await {
        Ok(outcome)=>outcome,Err(reason)=>unavailable_ocr(reason),
    }
}
async fn ocr_after_admission_owned<D,F>(lifecycle:Option<&dyn analysis_output::AudioLifecycle>,dispatch:D)->(String,Value)
where D:FnOnce()->F,F:std::future::Future<Output=(String,Value)> {
    let admission=match lifecycle {
        Some(lifecycle)=>lifecycle.event("permit_ocr",lifecycle.binding()).await,
        None=>Err("media_asr_lifecycle_missing".into()),
    };
    if let Err(reason)=admission {
        return unavailable_ocr(reason);
    }
    dispatch().await
}
async fn append_screen_text(result:Value,source:&MediaSource,work:&Path,input:&Path,duration:Option<f64>,lifecycle:Option<&dyn analysis_output::AudioLifecycle>)->Result<Value,String>{
    let (text,meta)=ocr_after_admission(lifecycle,||async {
        match MediaConfig::for_phase("finalize"){
            Ok(config)=>ocr(&config,source,&lifecycle.map(|l|l.binding()).unwrap_or(Value::Null),input,work,duration).await,
            Err(_)=>(String::new(),json!({"status":"unavailable","reason":"ocr_configuration_invalid","coverage":"unavailable"})),
        }
    }).await;
    let media_sha=file_digest(input)?;
    attach_screen_text(result,source,&text,&meta,&media_sha)
}
fn attach_screen_text(mut result:Value,source:&MediaSource,text:&str,meta:&Value,media_sha:&str)->Result<Value,String>{
    if meta["retainedEvidence"].is_object(){
        let retained=&meta["retainedEvidence"];let b=&retained["binding"];
        if retained["kind"]!="sampled_visual_ocr"||b["companyId"]!=source.account||b["account"]!=source.account
            ||b["postKey"]!=source.post_key||b["sourceLocatorSha256"]!=digest(&source.source_url)||b["source"]["sha256"]!=media_sha {
            return Err("ocr_source_binding_changed".into());
        }
    }
    let reused=result["audioAnalysisReuse"]==true;
    let target=result["audioAnalysis"]["targetRequest"].clone();
    let materials=result["materials"].as_array_mut().ok_or("media_result_invalid")?;
    let transcript=materials.iter_mut().find(|m|m["kind"]=="transcript").ok_or("media_transcript_missing")?;
    if !reused {transcript["transcription"]["ocr"]=meta.clone();}
    if !text.is_empty(){
        let key=digest(&format!("{}\n{}",source.account,source.post_key));
        materials.push(json!({"id":format!("media:ocr:{key}"),"title":format!("Текст в кадре: {}",source.title),"text":text,
            "kind":"ocr","account":source.account,"postKey":source.post_key,"sourceUrl":source.source_url,"mediaSha256":media_sha,"ocr":meta}));
    }
    if reused {result["currentScreenText"]=json!({"account":source.account,"postKey":source.post_key,"sourceUrl":source.source_url,
        "mediaSha256":media_sha,"sourceVersion":target["sourceVersion"],"connectorBinding":target["originalAlias"]["connectorBinding"],"ocr":meta});}
    Ok(result)
}

pub(crate) async fn process(source: &MediaSource, app: &super::App, id:&str) -> Result<Value, String> {
    let config=MediaConfig::from_env()?;
    std::fs::create_dir_all(&config.scratch).map_err(|_| "media_scratch_unavailable".to_owned())?;
    let work=config.scratch.join(format!("media-{}",uuid::Uuid::new_v4()));
    std::fs::create_dir(&work).map_err(|_| "media_scratch_unavailable".to_owned())?;
    let _scratch=ScratchGuard(work.clone());
    process_in(&config,source,&work,app,id).await
}

// Prefer <=720p with audio, including DASH/HLS split streams. A silent-video
// fallback is last; every branch requires video, never bestaudio by itself.
const VIDEO_DOWNLOAD_FORMAT: &str = "bestvideo[height<=720]+bestaudio/best[height<=720][vcodec!=none]/bestvideo+bestaudio/best[vcodec!=none]/bestvideo[height<=720]/bestvideo";
const YOUTUBE_HLS_FORMAT:&str="bestvideo[height<=720][protocol^=m3u8]+bestaudio[protocol^=m3u8]";
fn youtube_hls_fallback(locator:&str,error:Option<&str>)->bool {
    if error!=Some("source_download_failed_http_forbidden"){return false;}
    locator.strip_prefix("https://").and_then(|s|s.split('/').next())
        .is_some_and(|host|matches!(host,"www.youtube.com"|"youtube.com"|"m.youtube.com"|"youtu.be"))
}
fn download_args(config: &MediaConfig, work: &Path) -> Vec<String> {
    let mut download=Vec::new();
    if config.ytdlp_python { download.extend(strings(&["-m","yt_dlp"])); }
    download.extend(strings(&["--ignore-config","--no-plugin-dirs","--no-remote-components","--quiet","--no-warnings","--no-progress","--no-playlist"]));
    if let Some(node)=&config.ytdlp_node { download.extend(["--no-js-runtimes".into(),"--js-runtimes".into(),format!("node:{}",node.display())]); }
    download.extend(["--ffmpeg-location".into(),config.ffmpeg.display().to_string()]);
    // Rust owns the bounded retry budget. Do not let nested extractor, fragment,
    // filesystem or HTTP retries overlap its classification and total deadline.
    download.extend(strings(&["--merge-output-format","mkv","--socket-timeout","30","--retries","0","--fragment-retries","0","--extractor-retries","0","--file-access-retries","0","--abort-on-unavailable-fragments","--max-filesize","500M","--format",VIDEO_DOWNLOAD_FORMAT,"--output"]));
    download.push(work.join("source.%(ext)s").display().to_string());
    download
}

async fn process_in(config: &MediaConfig, source: &MediaSource, work: &Path, app: &super::App,id:&str) -> Result<Value, String> {
    let vision_worker=crate::media_vision_admission::Worker::capture_legacy(app,id,source).await.map_err(|e|e.1)?;
    let input=download_source(config,source,work,Duration::from_secs(90)).await?;
    process_downloaded(config,source,work,&input,app,id,Some(vision_worker)).await
}
async fn process_downloaded(config:&MediaConfig,source:&MediaSource,work:&Path,input:&Path,app:&super::App,id:&str,vision_worker:Option<crate::media_vision_admission::Worker>)->Result<Value,String>{
    let vision_worker=match vision_worker {Some(worker)=>worker,None=>crate::media_vision_admission::Worker::capture_legacy(app,id,source).await.map_err(|e|e.1)?};
    if has_video_stream(config,input).await!=Some(true) {return Err("visual_video_stream_missing".into());}
    let duration=probe_duration(config,input).await.ok_or("visual_duration_unknown")?;
    if !duration.is_finite() || duration<=0.0 {return Err("visual_duration_invalid".into());}
    let duration_ms=(duration*1000.0).round() as u64;
    let times=super::media_visual::sample_times(duration_ms).map_err(str::to_owned)?;
    let media_sha=file_digest(input)?;
    let created_at=super::now();
    let frame_dir=work.join("vision-frames");
    std::fs::create_dir(&frame_dir).map_err(|_|"visual_scratch_unavailable".to_owned())?;
    let mut frames=Vec::new();
    let extraction_started=std::time::Instant::now();
    let mut total_bytes=0u64;
    for (i,timestamp) in times.iter().enumerate() {
        if extraction_started.elapsed()>Duration::from_secs(600){return Err("visual_extraction_timeout".into());}
        let path=frame_dir.join(format!("frame-{i:03}.jpg"));
        let args=vec!["-nostdin".into(),"-hide_banner".into(),"-loglevel".into(),"error".into(),"-y".into(),"-ss".into(),format!("{:.3}",*timestamp as f64/1000.0),"-i".into(),input.display().to_string(),"-frames:v".into(),"1".into(),"-q:v".into(),"2".into(),path.display().to_string()];
        run_tool(&config.ffmpeg,&args,Duration::from_secs(60),false,"visual_frame_extraction_failed").await?;
        let sha=file_digest(&path).map_err(|_|"visual_frame_extraction_failed".to_owned())?;
        let bytes=std::fs::metadata(&path).map_err(|_|"visual_frame_extraction_failed".to_owned())?.len();
        total_bytes+=bytes;
        if bytes==0 || bytes>4*1024*1024 || total_bytes>128*1024*1024 {return Err("visual_frame_size_invalid".into());}
        frames.push(json!({"id":format!("frame-{i:03}"),"path":path,"sha256":sha,"timestampMs":timestamp}));
    }
    let request=super::media_visual::seal_request(json!({"schemaVersion":1,"workId":work.file_name().and_then(|v|v.to_str()),"createdAtUtc":created_at,
        "source":{"account":source.account,"postKey":source.post_key,"mediaSha256":media_sha,"durationMs":duration_ms},
        "coverage":super::media_visual::coverage(duration_ms,&frames),"frames":frames}));
    let stage=vision_worker.reserve("media_vision",&request).await.map_err(|e|e.1)?;
    let gate=gpu_gate::Lease::acquire("media_vision",&source.account,&env::var("COMMUNITYHERO_MEDIA_VISION_LOCAL_ENDPOINT").unwrap_or_default()).await?;
    let bridge=stage.dispatch(request.clone(),None).await.map(|(output,_)|output);
    if bridge.is_ok(){if let Some(gate)=gate{gate.finish()?;}}
    let response=bridge.map_err(|_|"visual_backend_failed".to_owned())?;
    let evidence=super::media_visual::admit(&request,&response).map_err(str::to_owned)?;
    let lifecycle=crate::media_analysis_runtime::Runtime::bind(app,id,None).await?;
    let mut result=transcribe_input_mode(config,source,work,input,true,None,Some(&lifecycle)).await?;
    let key=digest(&format!("{}\n{}",source.account,source.post_key));
    result["materials"].as_array_mut().ok_or("media_result_invalid")?.push(json!({
        "id":format!("media:visual-v1:{key}"),"title":format!("Визуальный контекст: {}",source.title),
        "text":response["summary"],"kind":"visual_context","account":source.account,"postKey":source.post_key,
        "sourceUrl":source.source_url,"mediaSha256":media_sha,"visualEvidence":evidence}));
    Ok(result)
}

fn audio_observation(has_audio:bool,raw:String)->(String,&'static str){
    let status=if !has_audio {"no_audio_stream"} else if raw.is_empty(){"inspected_no_speech"} else {"transcribed"};
    let text=if raw.is_empty(){format!("[Audio inspection: {status}. No spoken words were recovered.]")}else{raw};
    (text,status)
}
async fn transcribe_input(config: &MediaConfig, source: &MediaSource, work: &Path, input: &Path) -> Result<Value, String> {
    // Unbound local callers cannot create paid output outside the common file
    // guard. Native acceptance must supply its explicit isolated App/job fence.
    transcribe_input_mode(config,source,work,input,true,None,None).await
}
async fn transcribe_input_mode(config: &MediaConfig, source: &MediaSource, work: &Path, input: &Path, include_ocr:bool,job:Option<&gpu_gate::JobContext<'_>>,lifecycle:Option<&dyn analysis_output::AudioLifecycle>) -> Result<Value, String> {
    media_owned_stage(transcribe_input_mode_owned(config,source,work,input,include_ocr,job,lifecycle)).await
}
async fn transcribe_input_mode_owned(config: &MediaConfig, source: &MediaSource, work: &Path, input: &Path, include_ocr:bool,job:Option<&gpu_gate::JobContext<'_>>,lifecycle:Option<&dyn analysis_output::AudioLifecycle>) -> Result<Value, String> {
    let lifecycle=lifecycle.ok_or("media_asr_lifecycle_missing")?;
    let store=crate::media_fullframes::store()?;
    if let Some(job)=job {job.observe_finalize(gpu_gate::FinalizeStage::AudioProbe,None,None).await;}
    let media_sha=file_digest(input)?;
    let duration=probe_duration(config,input).await;
    let audio_stream_args=vec!["-v".into(),"error".into(),"-select_streams".into(),"a".into(),"-show_entries".into(),"stream=index".into(),"-of".into(),"csv=p=0".into(),input.display().to_string()];
    let has_audio=!run_tool(&config.ffprobe,&audio_stream_args,Duration::from_secs(30),true,"audio_inventory_failed").await?.trim().is_empty();
    let duration=duration.filter(|n|n.is_finite()&&*n>0.0).ok_or("audio_duration_unknown")?;
    let windows=if has_audio {audio_windows(duration)?}else{vec![]};
    let source_ref=store.put_file(input).map_err(|_|"media_source_artifact_unavailable")?;
    if source_ref.sha256!=media_sha{return Err("media_source_changed_during_capture".into());}
    let plan:Vec<Value>=windows.iter().enumerate().map(|(index,(start,len))|json!({"index":index,"startMs":(start*1000.0).round() as u64,"endMs":((start+len)*1000.0).round() as u64})).collect();
    let probe=json!({"method":"ffprobe_selected_audio_stream_inventory","hasAudio":has_audio,"mediaDurationSeconds":duration});
    let file_receipt=json!({"method":"local_sha256_and_size","source":source_ref.to_json()});
    let paths=[config.ffmpeg.clone(),config.ffprobe.clone(),config.whisper.clone(),config.model.clone()];
    let tool_hashes=tokio::task::spawn_blocking(move||paths.iter().map(|path|{
        // Configured model files can exceed the 500MiB media-input limit.
        let mut reader=std::io::BufReader::new(std::fs::File::open(path).map_err(|_|"media_asr_tool_identity_missing")?);
        let mut digest=Sha256::new();let mut buffer=[0u8;64*1024];
        loop{let n=reader.read(&mut buffer).map_err(|_|"media_asr_tool_identity_unavailable")?;if n==0{break;}digest.update(&buffer[..n]);}
        Ok::<_,String>(format!("{:x}",digest.finalize()))
    }).collect::<Result<Vec<String>,String>>()).await.map_err(|_|"media_asr_tool_identity_stopped")??;
    let spec=json!({"version":1,"decoderSha256":tool_hashes[0],"probeSha256":tool_hashes[1],
        "asrSha256":tool_hashes[2],"modelSha256":tool_hashes[3],"normalizer":"normalized_v1",
        "decoderArgs":["-vn","-ac","1","-ar","16000"],"asrArgs":["-l","auto","-nt","-np","--no-fallback"],"segments":plan});
    let mut request=lifecycle.binding();
    request["verifiedFile"]=json!({"sha256":source_ref.sha256,"bytes":source_ref.bytes,"receiptSha256":crate::media_fullframes::hash(&file_receipt),"probeSha256":crate::media_fullframes::hash(&probe)});
    request["stage"]=json!("asr");request["specSha256"]=json!(crate::media_fullframes::hash(&spec));
    request["verifiedReceipt"]=file_receipt;request["probeReceipt"]=probe;
    request["segments"]=json!(plan);request["durationMs"]=json!((duration*1000.0).round() as u64);
    request["durationToleranceMs"]=json!(250);
    if !has_audio {request["noAudio"]=json!(true);request["noAudioVerificationSha256"]=json!(crate::media_fullframes::hash(&request["probeReceipt"]));}
    let reserved=lifecycle.reserve(request.clone()).await?;
    if reserved["disposition"]=="reuse" {
        let original=&reserved["originalRequest"];
        let mut result=if reserved["result"]["adoption"]["kind"]=="verified_legacy_catalog" {reserved["audio"].clone()}
            else{analysis_output::read_full(&store,original,&reserved["result"])?};
        result["reused"]=json!(true);
        result["audioAnalysisReuse"]=json!(true);
        result["audioAnalysis"]=json!({"request":original,"targetRequest":request,"result":reserved["result"],"asrCalls":0,"segmentsAvoided":windows.len()});
        // Applicability is admitted separately by the current alias writer.
        if include_ocr {result=append_screen_text(result,source,work,input,Some(duration),Some(lifecycle)).await?;}
        return Ok(result);
    }
    if !matches!(reserved["disposition"].as_str(),Some("reserved"|"owned")){return Err(format!("media_asr_{}",reserved["disposition"].as_str().unwrap_or("held")));}
    let window_count=windows.len();
    let (outputs,texts)=analysis_output::execute_segments(&store,&request,&reserved,lifecycle,|index,start_ms,expected_ms|async move {
            let start=start_ms as f64/1000.0;let expected=expected_ms as f64/1000.0;
            if let Some(job)=job {job.observe_finalize(gpu_gate::FinalizeStage::AudioExtraction,Some(index as u64),Some(window_count as u64)).await;}
            let audio=work.join(format!("audio-{index:03}.wav"));
            let audio_args=vec!["-nostdin".into(),"-hide_banner".into(),"-loglevel".into(),"error".into(),"-y".into(),"-ss".into(),format!("{start:.3}"),
                "-i".into(),input.display().to_string(),"-vn".into(),"-ac".into(),"1".into(),"-ar".into(),"16000".into(),"-t".into(),format!("{expected:.3}"),audio.display().to_string()];
            run_tool(&config.ffmpeg,&audio_args,Duration::from_secs(900),false,"ffmpeg_failed").await?;
            let actual=probe_duration(config,&audio).await;
            if !audio_segment_covered(expected,actual){return Err("audio_coverage_incomplete".into());}
            let whisper_args=vec!["-m".into(),config.model.display().to_string(),"-f".into(),audio.display().to_string(),"-l".into(),"auto".into(),"-nt".into(),"-np".into(),"--no-fallback".into()];
            // Keep completed words/segments in this worker while another owner
            // uses the GPU; contention never restarts the segment loop.
            let endpoint=config.whisper.display().to_string();
            if let Some(job)=job {job.observe_finalize(gpu_gate::FinalizeStage::GpuAdmission,None,None).await;}
            let gate=gpu_gate::Lease::acquire_for_job("whisper_asr",&source.account,&endpoint,job).await?;
            if let Some(job)=job {job.observe_finalize(gpu_gate::FinalizeStage::AsrRunning,None,None).await;}
            let output=run_tool(&config.whisper,&whisper_args,Duration::from_secs(3600),true,"whisper_failed").await?;
            let raw=normalized(&output);
            // A cleanup failure must not discard already returned words.
            let _=std::fs::remove_file(&audio);
            Ok(analysis_output::SegmentExecution{raw:output,normalized:raw,actual_ms:(actual.unwrap()*1000.0).round() as u64,gate})
    }).await?;
    let mut segments=Vec::new();let mut words=Vec::new();let mut audio_duration=0.0;
    for (index,(segment,raw)) in outputs.iter().zip(texts.iter()).enumerate(){
        let start=segment["startMs"].as_u64().unwrap() as f64/1000.0;let end=segment["endMs"].as_u64().unwrap() as f64/1000.0;
        let actual=segment["actualDurationMs"].as_u64().unwrap() as f64/1000.0;
        if !raw.is_empty(){words.push(if windows.len()==1 {raw.clone()}else{format!("[{start:.0}s–{end:.0}s] {raw}")});}
        segments.push(json!({"startSeconds":start,"endSeconds":end,"audioDurationSeconds":actual,"audioStatus":if raw.is_empty(){"inspected_no_speech"}else{"transcribed"}}));
        audio_duration+=actual;
        if let Some(job)=job {job.observe_finalize(gpu_gate::FinalizeStage::AsrSegmentComplete,Some(index as u64+1),Some(windows.len() as u64)).await;}
    }
    let raw=words.join("\n");
    let (transcript,audio_status)=audio_observation(has_audio,raw);
    let (partial,coverage)=if has_audio {audio_coverage(Some(duration),Some(audio_duration))} else {(false,"no_audio_stream")};
    if partial{return Err("audio_coverage_incomplete".into());}
    let key=digest(&format!("{}\n{}",source.account,source.post_key));
    let transcription=json!({"audioStatus":audio_status,"model":"local-whisper","modelFile":config.model.file_name().and_then(|name|name.to_str()).unwrap_or("unknown"),
        "partial":partial,"maxAudioSeconds":if has_audio {audio_duration.ceil() as u64} else {0},"mediaDurationSeconds":duration,"audioDurationSeconds":if has_audio {Some(audio_duration)} else {None},"segments":segments,
        "coverage":coverage,"sourcePostKey":source.post_key,"sourceLocatorSha256":digest(&source.source_url),"sourceVersion":request["sourceVersion"],"ocr":{"status":"not_requested_audio_only"}});
    let materials=vec![json!({"id":format!("media:transcript:{key}"),"title":format!("Видео: {}",source.title),"text":transcript,
        "kind":"transcript","account":source.account,"postKey":source.post_key,"sourceUrl":source.source_url,"mediaSha256":media_sha,"transcription":transcription})];
    let result=json!({"materials":materials,"reused":false,"coverage":{"kind":coverage,"durationMs":request["durationMs"]},
        "outcome":if !has_audio {"no_audio"}else if audio_status=="inspected_no_speech"{"no_speech"}else{"transcript"}});
    let asr_calls=outputs.len()-reserved["completedSegments"].as_array().or_else(||reserved["attempt"]["segments"].as_array()).map(Vec::len).unwrap_or(0);
    analysis_output::complete_before_followup(&store,&request,&outputs,result,lifecycle,|mut result|async move {
        result["audioAnalysis"]["asrCalls"]=json!(asr_calls);
        if include_ocr {
            if let Some(job)=job {job.observe_finalize(gpu_gate::FinalizeStage::Ocr,None,None).await;}
            result=append_screen_text(result,source,work,input,Some(duration),Some(lifecycle)).await?;
        }
        Ok(result)
    }).await
}

#[cfg(test)]
mod tests {
    use super::*;
    // Authored CAS/unit coverage. These tests require the integration owner's
    // explicit native-test gate; the source-overlay delivery does not run them.
    fn ocr_fixture_binding(source:&MediaSource,reference:&crate::media_artifacts::ArtifactRef)->Value{
        let request=json!({"companyId":source.account,"sourceVersion":"current-visual-version",
            "originalAlias":{"account":source.account,"sourcePostKey":source.post_key,"sourceVersion":"current-visual-version",
                "connectorBinding":{"instance":"fixture-visual"},"sourceProjection":{"sourceUrl":source.source_url}}});
        ocr_capture_binding(source,&request,reference).unwrap()
    }
    fn ocr_fixture_source()->MediaSource{
        MediaSource{account:"company-a".into(),post_key:"current-visual".into(),title:"fixture".into(),
            source_url:"https://fixture.invalid/visual".into(),fallback_url:None,source_discovery:None}
    }
    fn ocr_fixture_tsv(text:&str)->Vec<u8>{
        format!("level\tpage_num\tblock_num\tpar_num\tline_num\tword_num\tleft\ttop\twidth\theight\tconf\ttext\n5\t1\t1\t1\t1\t1\t0\t0\t1\t1\t91\t{text}\n").into_bytes()
    }
    fn ocr_fixture_observation()->ToolObservation{
        ToolObservation{stage:"exit",exit_code:Some(0),stdout_complete:true,stdout_eof:true,stdout_status:"eof",
            child_cessation_confirmed:true,process_tree_cessation_confirmed:true,..ToolObservation::default()}
    }
    fn ocr_fixture_capture(store:&crate::media_artifacts::ArtifactStore)->(Value,Value,Value){
        let source=ocr_fixture_source();let reference=store.put_bytes(b"fixture-video").unwrap();let binding=ocr_fixture_binding(&source,&reference);
        let spec=json!({"format":"tsv","intervalSeconds":3.0});
        let raw=ocr_fixture_tsv("fixture normalized");
        let sample=json!({"index":0,"nominalSampleSeconds":0.0,"frame":store.put_bytes(b"fixture-frame").unwrap().to_json(),
            "rawOutput":store.put_bytes(&raw).unwrap().to_json(),"status":"completed","bindingSha256":crate::media_fullframes::hash(&binding),
            "capture":ocr_capture_projection(&ocr_fixture_observation(),true,raw.len())});
        let (_,meta)=retain_ocr_result(store,&binding,&spec,&[sample],"fixture normalized",ocr_sample_outcome("fixture normalized",1,0,3.0)).unwrap();
        (binding,spec,meta["retainedEvidence"].clone())
    }
    fn ocr_fixture_failed_capture(store:&crate::media_artifacts::ArtifactStore,observation:&ToolObservation,reason:&str)->(Value,Value,Value){
        let source=ocr_fixture_source();let reference=store.put_bytes(b"fixture-failed-video").unwrap();let binding=ocr_fixture_binding(&source,&reference);
        let spec=json!({"format":"tsv","intervalSeconds":3.0});
        let mut sample=json!({"index":0,"nominalSampleSeconds":0.0,"frame":store.put_bytes(b"failed-frame").unwrap().to_json()});
        retain_failed_ocr_sample(store,&binding,&mut sample,reason,observation).unwrap();
        let (prompt,meta)=retain_ocr_result(store,&binding,&spec,&[sample],"",ocr_sample_outcome("",1,1,3.0)).unwrap();
        assert!(prompt.is_empty());assert_eq!(meta["status"],"failed");
        (binding,spec,meta["retainedEvidence"].clone())
    }
    struct OcrPrefixThenPending {bytes:Vec<u8>,emitted:bool}
    impl tokio::io::AsyncRead for OcrPrefixThenPending {
        fn poll_read(self:std::pin::Pin<&mut Self>,_:&mut std::task::Context<'_>,buf:&mut tokio::io::ReadBuf<'_>)->std::task::Poll<std::io::Result<()>>{
            let this=self.get_mut();if this.emitted{return std::task::Poll::Pending;}
            assert!(this.bytes.len()<=buf.remaining());buf.put_slice(&this.bytes);this.emitted=true;
            std::task::Poll::Ready(Ok(()))
        }
    }
    #[tokio::test]
    async fn ocr_nonzero_exit_keeps_complete_stdout_but_never_normalizes_failed_lines(){
        let raw=ocr_fixture_tsv("FAILED_WORDS_MUST_NOT_ENTER_PROMPT");let mut reader=std::io::Cursor::new(raw.clone());
        let mut observation=ToolObservation::default();capture_tool_stdout(&mut reader,&mut observation,"ocr_failed").await.unwrap();
        assert_eq!(observe_tool_exit(&mut observation,false,Some(7),&[],false,"ocr_failed").unwrap_err(),"ocr_failed");
        observation.child_cessation_confirmed=true;observation.process_tree_cessation_confirmed=true;
        let temp=tempfile::tempdir().unwrap();let store=crate::media_artifacts::ArtifactStore::open(temp.path()).unwrap();
        let (binding,spec,retained)=ocr_fixture_failed_capture(&store,&observation,"ocr_failed");
        let cold=crate::media_artifacts::ArtifactStore::open(temp.path()).unwrap();let (text,samples)=read_retained_ocr(&cold,&retained,&binding,&spec).unwrap();
        assert!(text.is_empty());assert_eq!(samples[0]["status"],"failed");assert_eq!(samples[0]["capture"]["complete"],true);
        assert_eq!(samples[0]["capture"]["exitCode"],7);assert_eq!(samples[0]["capture"]["streamEof"],true);
        let r=crate::media_artifacts::ArtifactRef::from_json(&samples[0]["rawOutput"]).unwrap();assert_eq!(cold.read_bytes(&r,raw.len() as u64).unwrap(),raw);
        let path=store.path(&r).unwrap();std::fs::remove_file(path).unwrap();
        assert_eq!(read_retained_ocr(&cold,&retained,&binding,&spec).unwrap_err(),"ocr_retained_artifact_unavailable");
    }
    #[tokio::test]
    async fn ocr_timeout_cancellation_retains_prefix_and_eof_before_wait_stays_failed(){
        let raw=ocr_fixture_tsv("DEADLINE_WORDS_MUST_NOT_ENTER_PROMPT");
        for eof_before_wait in [false,true] {
            let mut observation=ToolObservation::default();
            if eof_before_wait{
                capture_tool_stdout(&mut std::io::Cursor::new(raw.clone()),&mut observation,"ocr_failed").await.unwrap();
            }else{
                let mut reader=OcrPrefixThenPending{bytes:raw.clone(),emitted:false};
                tokio::select!{biased;_ = capture_tool_stdout(&mut reader,&mut observation,"ocr_failed")=>panic!("fixture must await deadline"),_ = std::future::ready(())=>{}}
            }
            mark_tool_deadline(&mut observation);assert_eq!(observation.stdout_capture,raw);assert!(!observation.stdout_complete);
            assert_eq!(observation.stdout_eof,eof_before_wait);
            let temp=tempfile::tempdir().unwrap();let store=crate::media_artifacts::ArtifactStore::open(temp.path()).unwrap();
            let (binding,spec,retained)=ocr_fixture_failed_capture(&store,&observation,"ocr_failed_timeout");
            let (text,samples)=read_retained_ocr(&store,&retained,&binding,&spec).unwrap();assert!(text.is_empty());
            assert_eq!(samples[0]["capture"]["stage"],"deadline");assert_eq!(samples[0]["capture"]["status"],"failed");
            assert_eq!(samples[0]["capture"]["complete"],false);assert_eq!(samples[0]["capture"]["streamEof"],eof_before_wait);
        }
    }
    #[tokio::test]
    async fn ocr_output_limit_keeps_capped_prefix_and_detection_byte_as_failed_cas(){
        let raw=vec![b'x';TOOL_STDOUT_LIMIT+200];let mut observation=ToolObservation::default();
        let error=capture_tool_stdout(&mut std::io::Cursor::new(raw.as_slice()),&mut observation,"ocr_failed").await.unwrap_err();
        assert_eq!(error,"ocr_failed_output_limit");assert_eq!(observation.stdout_capture,raw[..TOOL_STDOUT_LIMIT+1]);
        assert!(!observation.stdout_complete);assert!(observation.stdout_truncated);
        let temp=tempfile::tempdir().unwrap();let store=crate::media_artifacts::ArtifactStore::open(temp.path()).unwrap();
        let (binding,spec,retained)=ocr_fixture_failed_capture(&store,&observation,&error);
        let (text,samples)=read_retained_ocr(&store,&retained,&binding,&spec).unwrap();assert!(text.is_empty());
        assert_eq!(samples[0]["capture"]["truncated"],true);assert_eq!(samples[0]["rawOutput"]["bytes"],TOOL_STDOUT_LIMIT+1);
    }
    #[tokio::test]
    async fn ocr_process_unknown_keeps_returned_stdout_and_spawn_failure_invents_no_raw(){
        let raw=ocr_fixture_tsv("UNKNOWN_PROCESS_WORDS_MUST_NOT_ENTER_PROMPT");let mut observation=ToolObservation::default();
        capture_tool_stdout(&mut std::io::Cursor::new(raw.clone()),&mut observation,"ocr_failed").await.unwrap();
        observe_tool_exit(&mut observation,true,Some(0),&[],false,"ocr_failed").unwrap();
        // Same state as failed child settlement: output remains in observation.
        let temp=tempfile::tempdir().unwrap();let store=crate::media_artifacts::ArtifactStore::open(temp.path()).unwrap();
        let (binding,spec,retained)=ocr_fixture_failed_capture(&store,&observation,"ocr_failed_process_unknown");
        let (text,samples)=read_retained_ocr(&store,&retained,&binding,&spec).unwrap();assert!(text.is_empty());
        assert_eq!(samples[0]["capture"]["childCessationConfirmed"],false);assert_eq!(samples[0]["capture"]["exitCode"],0);
        assert!(samples[0]["rawOutput"].is_object());
        let (_,_,spawn)=ocr_fixture_failed_capture(&store,&ToolObservation::default(),"ocr_failed_spawn_failed");
        let manifest=crate::media_fullframes::read(&store,&spawn["manifest"]).unwrap();assert!(manifest["samples"][0]["rawOutput"].is_null());
    }
    #[test]
    fn ocr_resolver_rejects_false_capture_completion_and_failed_word_injection(){
        let temp=tempfile::tempdir().unwrap();let store=crate::media_artifacts::ArtifactStore::open(temp.path()).unwrap();
        let (binding,spec,retained)=ocr_fixture_capture(&store);let manifest=crate::media_fullframes::read(&store,&retained["manifest"]).unwrap();
        for (key,value) in [("streamEof",json!(false)),("streamEof",Value::Null),("complete",json!(false)),("exitCode",json!(7)),("processTreeCessationConfirmed",json!(false)),("deadline",json!(true))] {
            let mut changed=manifest.clone();changed["samples"][0]["capture"][key]=value;
            let mut pointer=retained.clone();pointer["manifest"]=crate::media_fullframes::put(&store,&changed).unwrap();
            assert_eq!(read_retained_ocr(&store,&pointer,&binding,&spec).unwrap_err(),"ocr_retained_manifest_invalid");
        }
        let mut failed=ToolObservation{stdout_capture:ocr_fixture_tsv("INJECTED_FAILED_WORDS"),..ocr_fixture_observation()};
        failed.exit_code=Some(7);
        let (binding,spec,mut pointer)=ocr_fixture_failed_capture(&store,&failed,"ocr_failed");
        let mut manifest=crate::media_fullframes::read(&store,&pointer["manifest"]).unwrap();
        manifest["normalizedOutput"]=store.put_bytes(b"INJECTED_FAILED_WORDS").unwrap().to_json();pointer["normalizedOutput"]=manifest["normalizedOutput"].clone();
        pointer["manifest"]=crate::media_fullframes::put(&store,&manifest).unwrap();
        assert_eq!(read_retained_ocr(&store,&pointer,&binding,&spec).unwrap_err(),"ocr_retained_text_invalid");
        let manifest=crate::media_fullframes::read(&store,&pointer["manifest"]).unwrap();let raw=crate::media_artifacts::ArtifactRef::from_json(&manifest["samples"][0]["rawOutput"]).unwrap();
        let path=store.path(&raw).unwrap();std::fs::write(path,b"corrupt-failed-output").unwrap();
        assert_eq!(read_retained_ocr(&store,&pointer,&binding,&spec).unwrap_err(),"ocr_retained_artifact_unavailable");
    }
    #[test]
    fn ocr_resolver_rejects_missing_and_corrupt_objects_across_entire_closure(){
        for key in ["manifest","normalizedOutput","source","frame","rawOutput"] {
            for missing in [true,false] {
                let temp=tempfile::tempdir().unwrap();let store=crate::media_artifacts::ArtifactStore::open(temp.path()).unwrap();
                let (binding,spec,retained)=ocr_fixture_capture(&store);
                let manifest=crate::media_fullframes::read(&store,&retained["manifest"]).unwrap();
                let value=match key {"source"=>binding["source"].clone(),"frame"|"rawOutput"=>manifest["samples"][0][key].clone(),_=>retained[key].clone()};
                let reference=crate::media_artifacts::ArtifactRef::from_json(&value).unwrap();let path=store.path(&reference).unwrap();
                if missing {std::fs::remove_file(path).unwrap();}else{std::fs::write(path,b"corrupt-bytes").unwrap();}
                let cold=crate::media_artifacts::ArtifactStore::open(temp.path()).unwrap();
                assert_eq!(read_retained_ocr(&cold,&retained,&binding,&spec).unwrap_err(),"ocr_retained_artifact_unavailable","object={key}; missing={missing}");
            }
        }
    }
    #[test]
    fn ocr_resolver_rejects_foreign_binding_spec_dangling_refs_and_false_sample_completion(){
        let temp=tempfile::tempdir().unwrap();let store=crate::media_artifacts::ArtifactStore::open(temp.path()).unwrap();
        let (binding,spec,retained)=ocr_fixture_capture(&store);
        for key in ["companyId","postKey","sourceVersion","sourceLocatorSha256"] {
            let mut foreign=binding.clone();foreign[key]=json!("foreign");
            assert_eq!(read_retained_ocr(&store,&retained,&foreign,&spec).unwrap_err(),"ocr_retained_binding_changed");
        }
        let mut foreign_spec=spec.clone();foreign_spec["intervalSeconds"]=json!(4.0);
        assert_eq!(read_retained_ocr(&store,&retained,&binding,&foreign_spec).unwrap_err(),"ocr_retained_binding_changed");
        let mut pointer=retained.clone();pointer["normalizedOutput"]=store.put_bytes(b"foreign normalized").unwrap().to_json();
        assert_eq!(read_retained_ocr(&store,&pointer,&binding,&spec).unwrap_err(),"ocr_retained_binding_changed");
        let manifest=crate::media_fullframes::read(&store,&retained["manifest"]).unwrap();
        let mut dangling=manifest.clone();dangling["samples"][0]["rawOutput"]=json!({"sha256":"d".repeat(64),"bytes":8});
        dangling["samples"][0]["capture"]["bytes"]=json!(8);
        let mut pointer=retained.clone();pointer["manifest"]=crate::media_fullframes::put(&store,&dangling).unwrap();
        assert_eq!(read_retained_ocr(&store,&pointer,&binding,&spec).unwrap_err(),"ocr_retained_artifact_unavailable");
        let mut false_completion=manifest.clone();false_completion["samples"][0]["status"]=json!("failed");
        false_completion["samples"][0]["rawOutput"]=Value::Null;false_completion["samples"][0]["reason"]=json!("ocr_failed");
        pointer["manifest"]=crate::media_fullframes::put(&store,&false_completion).unwrap();
        assert_eq!(read_retained_ocr(&store,&pointer,&binding,&spec).unwrap_err(),"ocr_retained_manifest_invalid");
    }
    #[test]
    fn ocr_raw_cas_keeps_noise_and_non_utf8_bytes_independent_of_prompt_filtering(){
        let temp=tempfile::tempdir().unwrap();let store=crate::media_artifacts::ArtifactStore::open(temp.path()).unwrap();
        let source=ocr_fixture_source();let reference=store.put_bytes(b"current-video").unwrap();let binding=ocr_fixture_binding(&source,&reference);
        let mut raw=b"level\tpage_num\tblock_num\tpar_num\tline_num\tword_num\tleft\ttop\twidth\theight\tconf\ttext\n5\t1\t1\t1\t1\t1\t0\t0\t1\t1\t91\tVisible\n5\t1\t1\t1\t2\t1\t0\t0\t1\t1\t20\tLowConfidence".to_vec();
        raw.extend_from_slice(b"\nINVALID_UTF8:");raw.push(0xff);
        let frame=store.put_bytes(b"jpeg-fixture").unwrap();let mut sample=json!({"index":0,"frame":frame.to_json(),"nominalSampleSeconds":0.0});
        let lines=retain_ocr_sample(&store,&binding,&mut sample,&raw,&ocr_fixture_observation()).unwrap();assert_eq!(lines,vec!["Visible"]);
        let raw_ref=crate::media_artifacts::ArtifactRef::from_json(&sample["rawOutput"]).unwrap();
        assert_eq!(store.read_bytes(&raw_ref,raw.len() as u64).unwrap(),raw);
        assert_eq!(sample["bindingSha256"],crate::media_fullframes::hash(&binding));assert_eq!(sample["status"],"completed");
    }
    #[test]
    fn ocr_full_normalized_cas_and_sample_manifest_outlive_utf8_prompt_truncation(){
        let temp=tempfile::tempdir().unwrap();let store=crate::media_artifacts::ArtifactStore::open(temp.path()).unwrap();
        let source=ocr_fixture_source();let reference=store.put_bytes(b"current-video").unwrap();let binding=ocr_fixture_binding(&source,&reference);
        let full=format!("{}TAIL_MUST_REMAIN", "я".repeat(8000));
        let frame=store.put_bytes(b"frame-one").unwrap();let raw_bytes=ocr_fixture_tsv(&full);let raw=store.put_bytes(&raw_bytes).unwrap();
        let samples=vec![json!({"index":0,"nominalSampleSeconds":0.0,"frame":frame.to_json(),"rawOutput":raw.to_json(),"status":"completed","bindingSha256":crate::media_fullframes::hash(&binding),
            "capture":ocr_capture_projection(&ocr_fixture_observation(),true,raw_bytes.len())}),
            json!({"index":1,"nominalSampleSeconds":3.0,"frame":frame.to_json(),"rawOutput":null,"status":"failed","reason":"ocr_failed","bindingSha256":crate::media_fullframes::hash(&binding),
                "capture":ocr_capture_projection(&ToolObservation::default(),false,0)})];
        let spec=json!({"format":"tsv","intervalSeconds":3.0});
        let (prompt,meta)=retain_ocr_result(&store,&binding,&spec,&samples,&full,ocr_sample_outcome(&full,2,1,3.0)).unwrap();
        assert_eq!(prompt.len(),OCR_PROMPT_BYTES);assert!(prompt.is_char_boundary(prompt.len()));assert!(!prompt.contains("TAIL_MUST_REMAIN"));
        assert_eq!(meta["promptProjection"]["truncated"],true);assert_eq!(meta["status"],"partial");assert_eq!(meta["exhaustive"],false);
        let output=crate::media_artifacts::ArtifactRef::from_json(&meta["retainedEvidence"]["normalizedOutput"]).unwrap();
        let cold=crate::media_artifacts::ArtifactStore::open(temp.path()).unwrap();
        let (cold_text,cold_samples)=read_retained_ocr(&cold,&meta["retainedEvidence"],&binding,&spec).unwrap();
        assert_eq!(cold_text,full);assert_eq!(cold_samples,samples);assert_eq!(cold_samples[1]["status"],"failed");
        assert_eq!(cold.read_bytes(&output,full.len() as u64).unwrap(),full.as_bytes());
        let manifest_ref=crate::media_artifacts::ArtifactRef::from_json(&meta["retainedEvidence"]["manifest"]).unwrap();
        let manifest:Value=serde_json::from_slice(&cold.read_bytes(&manifest_ref,16*1024).unwrap()).unwrap();
        assert_eq!(manifest["binding"],binding);assert_eq!(manifest["samples"],json!(samples));assert_eq!(manifest["kind"],"sampled_visual_ocr");
        assert_eq!(manifest["normalizedOutput"],output.to_json());assert!(manifest.to_string().len()<5000);
    }
    #[test]
    fn ocr_capture_binding_rejects_changed_company_alias_and_locator(){
        let source=ocr_fixture_source();let reference=crate::media_artifacts::ArtifactRef{sha256:"a".repeat(64),bytes:20};
        let request=json!({"companyId":source.account,"sourceVersion":"visual-version","originalAlias":{"account":source.account,
            "sourcePostKey":source.post_key,"sourceVersion":"visual-version","connectorBinding":{"instance":"visual"},"sourceProjection":{"sourceUrl":source.source_url}}});
        assert!(ocr_capture_binding(&source,&request,&reference).is_ok());
        for pointer in ["/companyId","/originalAlias/account","/originalAlias/sourcePostKey","/originalAlias/sourceVersion","/originalAlias/sourceProjection/sourceUrl"] {
            let mut changed=request.clone();*changed.pointer_mut(pointer).unwrap()=json!("changed");
            assert_eq!(ocr_capture_binding(&source,&changed,&reference).unwrap_err(),"ocr_source_binding_invalid");
        }
    }
    #[test]
    fn retained_visual_ocr_never_relabels_reused_audio_donor(){
        let temp=tempfile::tempdir().unwrap();let store=crate::media_artifacts::ArtifactStore::open(temp.path()).unwrap();
        let source=ocr_fixture_source();let reference=store.put_bytes(b"current-video").unwrap();let binding=ocr_fixture_binding(&source,&reference);
        let raw=ocr_fixture_tsv("current visual words");
        let samples=vec![json!({"index":0,"nominalSampleSeconds":0.0,"frame":store.put_bytes(b"frame").unwrap().to_json(),
            "rawOutput":store.put_bytes(&raw).unwrap().to_json(),"status":"completed","bindingSha256":crate::media_fullframes::hash(&binding),
            "capture":ocr_capture_projection(&ocr_fixture_observation(),true,raw.len())})];
        let (text,meta)=retain_ocr_result(&store,&binding,&json!({"format":"tsv","intervalSeconds":3.0}),&samples,"current visual words",ocr_sample_outcome("current visual words",1,0,3.0)).unwrap();
        let donor=json!({"kind":"transcript","text":"paid donor audio","account":"company-a","postKey":"audio-donor",
            "mediaSha256":"b".repeat(64),"transcription":{"sourceVersion":"audio-donor-version","ocr":{"status":"not_requested_audio_only"}}});
        let paid=json!({"manifest":{"sha256":"c".repeat(64),"bytes":40}});
        let result=json!({"audioAnalysisReuse":true,"audioAnalysis":{"result":paid,"targetRequest":{"sourceVersion":"current-visual-version",
            "originalAlias":{"connectorBinding":{"instance":"fixture-visual"}}}},"materials":[donor]});
        let attached=attach_screen_text(result.clone(),&source,&text,&meta,&reference.sha256).unwrap();
        assert_eq!(attached["materials"][0],donor);assert_eq!(attached["audioAnalysis"]["result"],paid);
        assert_eq!(attached["materials"][1]["ocr"]["retainedEvidence"]["binding"]["postKey"],source.post_key);
        let mut changed=meta.clone();changed["retainedEvidence"]["binding"]["companyId"]=json!("company-b");
        assert_eq!(attach_screen_text(result,&source,&text,&changed,&reference.sha256).unwrap_err(),"ocr_source_binding_changed");
    }
    // Explicit fixture ownership only. Production has no missing-registry
    // fallback; direct OS helper tests supply a disposable registry themselves.
    async fn ocr_after_admission<D,F>(lifecycle:Option<&dyn analysis_output::AudioLifecycle>,dispatch:D)->(String,Value)
    where D:FnOnce()->F,F:std::future::Future<Output=(String,Value)> {
        crate::runtime_owned_work::with_registry(crate::runtime_owned_work::Registry::default(),super::ocr_after_admission(lifecycle,dispatch)).await
    }
    async fn run_tool_until<D,F>(exe:&Path,args:&[String],deadline:D,capture:bool,code:&str)->Result<String,String>
    where D:FnOnce()->F,F:std::future::Future<Output=()> {
        crate::runtime_owned_work::with_registry(crate::runtime_owned_work::Registry::default(),super::run_tool_until(exe,args,deadline,capture,code)).await
    }
    async fn run_tool_until_observed<D,F>(exe:&Path,args:&[String],deadline:D,capture:bool,code:&str,observation:&mut ToolObservation)->Result<String,String>
    where D:FnOnce()->F,F:std::future::Future<Output=()> {
        crate::runtime_owned_work::with_registry(crate::runtime_owned_work::Registry::default(),super::run_tool_until_observed(exe,args,deadline,capture,code,observation)).await
    }
    #[tokio::test]
    async fn denied_ocr_lifecycle_skips_tool_followup_and_preserves_paid_audio(){
        struct Denied;
        impl analysis_output::AudioLifecycle for Denied {
            fn binding(&self)->Value{json!({"companyId":"company-a","owner":"owned-worker"})}
            fn reserve(&self,_:Value)->analysis_output::LifecycleFuture<'_,Value>{Box::pin(async{Err("fixture_unused".into())})}
            fn event(&self,event:&'static str,request:Value)->analysis_output::LifecycleFuture<'_,()>{Box::pin(async move{
                assert_eq!(event,"permit_ocr");assert_eq!(request["owner"],"owned-worker");Err("runtime_lifecycle_draining".into())
            })}
        }
        let calls=std::sync::atomic::AtomicUsize::new(0);
        let(text,meta)=ocr_after_admission(Some(&Denied),||async {
            calls.fetch_add(1,std::sync::atomic::Ordering::SeqCst);("must not run".into(),json!({"status":"completed"}))
        }).await;
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst),0);
        assert_eq!(meta["status"],"unavailable");assert_eq!(meta["reason"],"lifecycle_draining");
        assert_eq!(meta["gateReason"],"runtime_lifecycle_draining");
        let donor=json!({"kind":"transcript","text":"already paid speech","transcription":{"sourceVersion":"original","ocr":{"status":"not_requested_audio_only"}}});
        let receipt=json!({"manifest":{"sha256":"a".repeat(64),"bytes":100}});
        let result=json!({"audioAnalysisReuse":true,"audioAnalysis":{"result":receipt,"targetRequest":{"sourceVersion":"target"}},"materials":[donor.clone()]});
        let source=MediaSource{account:"company-a".into(),post_key:"target-post".into(),title:"target".into(),source_url:"https://fixture.invalid/target".into(),fallback_url:None,source_discovery:None};
        let result=attach_screen_text(result,&source,&text,&meta,&"b".repeat(64)).unwrap();
        assert_eq!(result["materials"][0],donor);assert_eq!(result["audioAnalysis"]["result"],receipt);
        assert_eq!(result["currentScreenText"]["ocr"]["status"],"unavailable");
        let(_,none)=ocr_after_admission(None,||async{
            calls.fetch_add(1,std::sync::atomic::Ordering::SeqCst);("unbound must not run".into(),Value::Null)
        }).await;
        assert_eq!(none["gateReason"],"media_asr_lifecycle_missing");
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst),0);
    }
    #[test]
    fn reused_audio_preserves_donor_while_target_ocr_stays_separately_bound(){
        let donor=json!({"kind":"transcript","account":"company-a","postKey":"original-post","sourceUrl":"https://fixture.invalid/original",
            "mediaSha256":"a".repeat(64),"text":"original speech","transcription":{"sourceVersion":"original-version","ocr":{"status":"not_requested_audio_only"}}});
        let result=json!({"audioAnalysisReuse":true,"audioAnalysis":{"targetRequest":{"sourceVersion":"target-version","originalAlias":{"connectorBinding":{"instance":"target-instance"}}}},"materials":[donor.clone()]});
        let target=MediaSource{account:"company-a".into(),post_key:"target-post".into(),title:"target".into(),source_url:"https://fixture.invalid/target".into(),fallback_url:None,source_discovery:None};
        let outcome=json!({"status":"completed","coverage":"sampled_frames","exhaustive":false});
        let result=attach_screen_text(result,&target,"target price",&outcome,&"b".repeat(64)).unwrap();
        assert_eq!(result["materials"][0],donor);
        assert_eq!(result["materials"][1]["postKey"],"target-post");
        assert_eq!(result["materials"][1]["mediaSha256"],"b".repeat(64));
        assert_eq!(result["currentScreenText"]["ocr"]["exhaustive"],false);
        assert_eq!(result["currentScreenText"]["postKey"],"target-post");
        assert_eq!(result["currentScreenText"]["sourceVersion"],"target-version");
        assert_eq!(result["currentScreenText"]["connectorBinding"]["instance"],"target-instance");
    }
    #[test]
    fn phase_config_does_not_require_unrelated_tools(){
        let temp=tempfile::tempdir().unwrap();let exe=temp.path().join("tool");std::fs::write(&exe,b"fixture").unwrap();
        let get=|key:&str|match key {
            "COMMUNITYHERO_MEDIA_SCRATCH_DIR"=>Some(temp.path().as_os_str().to_owned()),
            "COMMUNITYHERO_MEDIA_FFMPEG"|"COMMUNITYHERO_MEDIA_FFPROBE"|"COMMUNITYHERO_MEDIA_WHISPER_CLI"|"COMMUNITYHERO_MEDIA_WHISPER_MODEL"=>Some(exe.as_os_str().to_owned()),
            // Broken unrelated optional tools also cannot hold cached audio.
            "COMMUNITYHERO_MEDIA_YTDLP_NODE"|"COMMUNITYHERO_MEDIA_TESSERACT"|"COMMUNITYHERO_MEDIA_TESSDATA_PREFIX"=>Some("relative-invalid".into()),
            _=>None,
        };
        let audio=MediaConfig::for_phase_with("audio",&get).unwrap();assert_eq!(audio.whisper,exe);assert!(audio.ytdlp.as_os_str().is_empty());assert!(audio.tesseract.is_none());
        assert!(MediaConfig::for_phase_with("download",&get).is_err());
        let decoder_only=|key:&str|if matches!(key,"COMMUNITYHERO_MEDIA_FFMPEG"|"COMMUNITYHERO_MEDIA_SCRATCH_DIR"){get(key)}else{None};
        for phase in ["inventory","select","scan"]{assert!(MediaConfig::for_phase_with(phase,&decoder_only).is_ok(),"{phase}");}
        for phase in ["audio","finalize","download","unknown"]{assert!(MediaConfig::for_phase_with(phase,&decoder_only).is_err(),"{phase}");}
        let no_whisper=|key:&str|if key=="COMMUNITYHERO_MEDIA_WHISPER_CLI"{None}else{get(key)};
        assert_eq!(MediaConfig::for_phase_with("audio",&no_whisper).unwrap_err(),"media_config_missing_COMMUNITYHERO_MEDIA_WHISPER_CLI");
    }
    #[test]
    fn downloaded_checks_merged_size_before_any_media_read(){
        let temp=tempfile::tempdir().unwrap();let path=temp.path().join("source.mkv");
        std::fs::write(&path,b"small fixture").unwrap();assert_eq!(downloaded(temp.path()).unwrap(),path);
        let file=std::fs::OpenOptions::new().write(true).open(&path).unwrap();
        #[cfg(windows)] {
            use std::os::windows::io::AsRawHandle;
            // Mark sparse before increasing logical length: no 500MiB payload
            // allocation/write is needed for this metadata-only regression.
            #[link(name="kernel32")]
            unsafe extern "system" {fn DeviceIoControl(device:*mut std::ffi::c_void,code:u32,input:*mut std::ffi::c_void,input_len:u32,output:*mut std::ffi::c_void,output_len:u32,returned:*mut u32,overlapped:*mut std::ffi::c_void)->i32;}
            let mut returned=0;let ok=unsafe {DeviceIoControl(file.as_raw_handle(),0x000900c4,std::ptr::null_mut(),0,std::ptr::null_mut(),0,&mut returned,std::ptr::null_mut())};
            assert_ne!(ok,0,"Sparse fixture setup must succeed before increasing length");
        }
        file.set_len(500*1024*1024+1).unwrap();
        assert_eq!(downloaded(temp.path()).unwrap_err(),"source_file_too_large");
        file.set_len(500*1024*1024).unwrap();assert_eq!(downloaded(temp.path()).unwrap(),path);
    }
    #[test]
    fn downloaded_rejects_split_residue_instead_of_admitting_one_fragment(){
        let temp=tempfile::tempdir().unwrap();
        for name in ["source.mkv","source.fvideo.mp4","source.faudio.m4a"]{std::fs::write(temp.path().join(name),b"fixture").unwrap();}
        assert_eq!(downloaded(temp.path()).unwrap_err(),"source_file_missing");
    }
    #[test]
    fn youtube_hls_recovery_is_scoped_to_public_youtube_forbidden_transport(){
        for url in ["https://www.youtube.com/watch?v=fixture","https://youtu.be/fixture"] {
            assert!(youtube_hls_fallback(url,Some("source_download_failed_http_forbidden")));
            for error in [None,Some("source_download_failed_auth"),Some("source_download_failed_rate_limited"),Some("source_download_failed_timeout"),Some("source_file_too_large")] {
                assert!(!youtube_hls_fallback(url,error));
            }
        }
        for url in ["https://youtube.com.evil.example/x","https://youtube.com@evil.example/x","http://youtube.com/x","https://vk.com/video","https://tiktok.com/video"] {
            assert!(!youtube_hls_fallback(url,Some("source_download_failed_http_forbidden")));
        }
    }
    #[test]
    fn fallback_clears_only_source_residue_and_preserves_unrelated_work(){
        let temp=tempfile::tempdir().unwrap();
        for name in ["source.fvideo.mp4","source.faudio.m4a","source.mkv.part","keep.json"] {std::fs::write(temp.path().join(name),b"fixture").unwrap();}
        assert!(fallback_after_download_error("source_download_failed_format_unavailable"));
        assert!(!fallback_after_download_error("source_download_failed_timeout"));
        assert!(!fallback_after_download_error("source_file_too_large"));
        clear_download_residue(temp.path()).unwrap();
        assert_eq!(std::fs::read_dir(temp.path()).unwrap().count(),1);
        assert!(temp.path().join("keep.json").exists());
    }
    fn download_fixture() -> (MediaConfig,MediaSource,tempfile::TempDir,tempfile::TempDir) {
        let config=MediaConfig { ytdlp:PathBuf::from("unused-test-tool"),ytdlp_python:false,ytdlp_node:None,
            ffmpeg:PathBuf::new(),ffprobe:PathBuf::new(),whisper:PathBuf::new(),model:PathBuf::new(),
            tesseract:None,tessdata:None,scratch:PathBuf::new() };
        let source=MediaSource {account:"fixture".into(),post_key:"exact-source".into(),title:"fixture".into(),
            source_url:"https://www.youtube.com/watch?v=fixture".into(),fallback_url:Some("https://bound.invalid/exact-source".into()),source_discovery:None};
        (config,source,tempfile::tempdir().unwrap(),tempfile::tempdir().unwrap())
    }
    #[tokio::test]
    async fn bounded_network_retry_cleans_residue_and_shares_original_deadline() {
        let (config,source,work,dir)=download_fixture();
        let mut calls=0;let mut deadlines=Vec::new();let work_path=work.path();
        let result=download_source_with(&config,&source,work_path,dir.path(),Duration::from_secs(10),|args,until| {
            calls+=1;deadlines.push(until);
            assert_eq!(args.last().unwrap(),&source.source_url);
            let result=if calls==1 {
                std::fs::write(work_path.join("source.mp4.part"),b"partial").unwrap();
                Err("source_download_failed_network".into())
            } else {
                assert!(!work_path.join("source.mp4.part").exists());
                std::fs::write(work_path.join("source.mkv"),b"fixture complete").unwrap();Ok(String::new())
            };
            async move {(result,ToolObservation{stage:"exit",exit_code:Some(1),child_cessation_confirmed:true,process_tree_cessation_confirmed:true,..Default::default()})}
        }).await.unwrap();
        assert_eq!(calls,2);assert_eq!(deadlines[0],deadlines[1]);assert_eq!(result,work_path.join("source.mkv"));
        let records=std::fs::read_dir(dir.path()).unwrap().map(|entry|serde_json::from_slice::<Value>(&std::fs::read(entry.unwrap().path()).unwrap()).unwrap()).collect::<Vec<_>>();
        assert_eq!(records.len(),4);
        assert!(records.iter().any(|record|record["category"]=="source_download_failed_network"&&record["recovery"]["automaticRetryEligible"]==true&&record["childCessationConfirmed"]==true));
    }
    #[tokio::test]
    async fn transient_budget_is_single_and_unsafe_classes_never_retry_or_fallback() {
        for (error,ceased,expected_calls) in [
            ("source_download_failed_network",true,2),("source_download_failed_network",false,1),
            ("source_download_failed_auth",true,1),("source_download_failed_challenge",true,1),
            ("source_download_failed_rate_limited",true,1),("source_download_failed_tls",true,1),
            ("source_download_failed_disk_full",true,1),("source_download_failed_permission",true,1),
            ("source_download_failed_integrity",true,1),("source_download_failed",true,1),
            ("source_download_failed_timeout",true,1),("source_download_failed_process_unknown",false,1),
        ] {
            let (config,source,work,dir)=download_fixture();let mut calls=0;
            let result=download_source_with(&config,&source,work.path(),dir.path(),Duration::from_secs(10),|_,_| {
                calls+=1;async move {(Err(error.into()),ToolObservation{stage:"exit",child_cessation_confirmed:ceased,process_tree_cessation_confirmed:ceased,..Default::default()})}
            }).await;
            assert_eq!(result.unwrap_err(),error);assert_eq!(calls,expected_calls,"{error}, cessation={ceased}");
        }
    }
    #[tokio::test]
    async fn rate_limit_cooldown_survives_receipt_readback_without_retry_or_locator_fallback() {
        let (config,source,work,dir)=download_fixture();let mut calls=0;
        let now=chrono::DateTime::parse_from_rfc3339("2026-09-27T12:00:00Z").unwrap().with_timezone(&chrono::Utc);
        let diagnostic=b"HTTP Error 429: Too Many Requests\nRetry-After: 120\nCookie: SECRET https://private.invalid/?token=PRIVATE";
        let retry_after=download_failure::retry_after(diagnostic,now).unwrap();
        let result=download_source_with(&config,&source,work.path(),dir.path(),Duration::from_secs(10),|_,_| {
            calls+=1;let retry_after=retry_after.clone();
            async move {(Err("source_download_failed_rate_limited".into()),ToolObservation{stage:"exit",exit_code:Some(1),
                child_cessation_confirmed:true,process_tree_cessation_confirmed:true,retry_after:Some(retry_after),..Default::default()})}
        }).await;
        assert_eq!(result.unwrap_err(),"source_download_failed_rate_limited");assert_eq!(calls,1);
        let records=std::fs::read_dir(dir.path()).unwrap().map(|entry|serde_json::from_slice::<Value>(
            &std::fs::read(entry.unwrap().path()).unwrap()).unwrap()).collect::<Vec<_>>();
        assert_eq!(records.len(),2);
        let outcome=records.iter().find(|record|record["category"]=="source_download_failed_rate_limited").unwrap();
        assert_eq!(outcome["retryAfter"]["notBeforeUtc"],"2026-09-27T12:02:00Z");
        assert_eq!(outcome["retryAfter"]["delaySeconds"],120);
        assert_eq!(outcome["recovery"]["automaticRetryPermitted"],false);
        assert_eq!(outcome["recovery"]["alternateBoundLocatorPermitted"],false);
        let rendered=serde_json::to_string(&records).unwrap();
        assert!(!rendered.contains("SECRET")&&!rendered.contains("PRIVATE")&&!rendered.contains("https:"));
        let unrelated=Err("source_download_failed_http_forbidden".into());
        let observation=ToolObservation{retry_after:Some(retry_after),..Default::default()};
        assert!(download_attempt_record("fixture",&source,&source.source_url,0,Some(&observation),Some(&unrelated))["retryAfter"].is_null());
    }
    #[tokio::test]
    async fn success_without_cessation_is_unknown_and_cannot_admit_a_file() {
        let (config,source,work,dir)=download_fixture();let mut calls=0;
        std::fs::write(work.path().join("source.mkv"),b"fixture complete").unwrap();
        let result=download_source_with(&config,&source,work.path(),dir.path(),Duration::from_secs(10),|_,_| {
            calls+=1;async {(Ok(String::new()),ToolObservation{stage:"exit",child_cessation_confirmed:false,..Default::default()})}
        }).await;
        assert_eq!(result.unwrap_err(),"source_download_failed_process_unknown");assert_eq!(calls,1);
        let outcome=std::fs::read_dir(dir.path()).unwrap().filter_map(Result::ok).find(|entry|entry.file_name().to_string_lossy().contains("outcome")).unwrap();
        let record:Value=serde_json::from_slice(&std::fs::read(outcome.path()).unwrap()).unwrap();
        assert_eq!(record["category"],"source_download_failed_process_unknown");
        assert_eq!(record["recovery"]["disposition"],"reconcile_process_before_retry");
    }
    #[tokio::test]
    async fn parent_reaped_without_tree_proof_allows_completion_but_never_recovery() {
        for error in [Some("source_download_failed_network"),Some("source_download_failed_http_forbidden"),None] {
            let (config,source,work,dir)=download_fixture();let mut calls=0;
            std::fs::write(work.path().join("source.mkv"),b"fixture complete").unwrap();
            let result=download_source_with(&config,&source,work.path(),dir.path(),Duration::from_secs(10),|_,_| {
                calls+=1;async move {(error.map_or_else(||Ok(String::new()),|error|Err(error.into())),
                    ToolObservation{stage:"exit",child_cessation_confirmed:true,process_tree_cessation_confirmed:false,..Default::default()})}
            }).await;
            if let Some(error)=error {assert_eq!(result.unwrap_err(),error);} else {assert!(result.is_ok());}
            assert_eq!(calls,1);
            let outcome=std::fs::read_dir(dir.path()).unwrap().filter_map(Result::ok).find(|entry|entry.file_name().to_string_lossy().contains("outcome")).unwrap();
            let record:Value=serde_json::from_slice(&std::fs::read(outcome.path()).unwrap()).unwrap();
            for action in ["automaticRetryPermitted","sameSourceHlsPermitted","alternateBoundLocatorPermitted"] {assert_eq!(record["recovery"][action],false);}
        }
    }
    #[tokio::test]
    async fn alternate_transport_and_locator_do_not_reset_deadline_or_binding() {
        let (config,source,work,dir)=download_fixture();let mut calls=0;let mut deadlines=Vec::new();
        // This test proves propagation of the same deadline, not disk fsync
        // latency. Inject the final transport timeout instead of requiring all
        // preceding immutable receipt writes to finish inside 250 wall-clock ms.
        let result=download_source_with(&config,&source,work.path(),dir.path(),Duration::from_secs(120),|args,until| {
            calls+=1;deadlines.push(until);
            let call=calls;let original=&source.source_url;let alternate=source.fallback_url.as_ref().unwrap();
            assert_eq!(args.last().unwrap(),if call<=2 {original}else{alternate});
            let index=args.iter().position(|v|v=="--format").unwrap();
            assert_eq!(args[index+1],if call==2 {YOUTUBE_HLS_FORMAT}else{VIDEO_DOWNLOAD_FORMAT});
            async move {
                (Err(if call==3 {"source_download_failed_timeout"} else {"source_download_failed_http_forbidden"}.into()),
                    ToolObservation{stage:"exit",child_cessation_confirmed:true,process_tree_cessation_confirmed:true,..Default::default()})
            }
        }).await;
        assert_eq!(result.unwrap_err(),"source_download_failed_timeout");assert_eq!(calls,3);
        assert!(deadlines.iter().all(|deadline|*deadline==deadlines[0]));
        let records:Vec<Value>=std::fs::read_dir(dir.path()).unwrap().map(|entry|{
            serde_json::from_slice(&std::fs::read(entry.unwrap().path()).unwrap()).unwrap()
        }).collect();
        assert_eq!(records.len(),6,"every dispatch retains an intent and outcome");
        let outcomes:Vec<_>=records.iter().filter(|r|r["stage"]=="exit").collect();
        assert_eq!(outcomes.len(),3);
        let primary=outcomes.iter().find(|r|r["locatorOrdinal"]==0&&r["transport"]=="default").unwrap();
        let hls=outcomes.iter().find(|r|r["locatorOrdinal"]==0&&r["transport"]=="youtube_hls").unwrap();
        let alternate=outcomes.iter().find(|r|r["locatorOrdinal"]==1).unwrap();
        assert_eq!(primary["sourceBindingSha256"],hls["sourceBindingSha256"]);
        assert_ne!(primary["sourceBindingSha256"],alternate["sourceBindingSha256"]);
        assert_eq!(alternate["category"],"source_download_failed_timeout");
        for outcome in &outcomes {
            let intent=records.iter().find(|r|r["stage"]=="intent"&&r["attemptId"]==outcome["attemptId"]).unwrap();
            for key in ["sourceBindingSha256","locatorOrdinal","transport"] {assert_eq!(intent[key],outcome[key]);}
        }
        // Exhausted budgets must stop before even an intent/dispatch. The real
        // process observation test separately injects an immediately ready
        // deadline and verifies child/tree cessation without elapsed-time races.
        let empty=tempfile::tempdir().unwrap();let mut expired_calls=0;
        let expired=download_source_with(&config,&source,work.path(),empty.path(),Duration::ZERO,|_,_| {
            expired_calls+=1;async {(Err("unexpected_dispatch".into()),ToolObservation::default())}
        }).await;
        assert_eq!(expired.unwrap_err(),"source_download_failed_timeout");assert_eq!(expired_calls,0);
        assert_eq!(std::fs::read_dir(empty.path()).unwrap().count(),0);
    }
    #[test]
    fn downloaded_never_admits_zero_bytes_or_partial_residue() {
        for partial in [false,true] {
            let work=tempfile::tempdir().unwrap();
            std::fs::write(work.path().join("source.mkv"),if partial{b"container".as_slice()}else{b""}).unwrap();
            if partial {std::fs::write(work.path().join("source.mp4.part"),b"unfinished").unwrap();}
            assert_eq!(downloaded(work.path()).unwrap_err(),"source_download_failed_integrity");
        }
        for name in ["source.f137.mp4","source.fvideo.mp4","source.faudio.m4a"] {
            let work=tempfile::tempdir().unwrap();std::fs::write(work.path().join(name),b"one stream only").unwrap();
            assert_eq!(downloaded(work.path()).unwrap_err(),"source_download_failed_integrity","{name}");
        }
        for error in ["source_download_failed_private","source_download_failed_geoblocked","source_download_failed_auth","source_download_failed_challenge","source_download_failed_dependency","source_download_failed_impersonation"] {
            assert!(!fallback_after_download_error(error));
        }
    }
    #[test]
    fn downloader_diagnostics_return_only_closed_codes_never_private_stderr(){
        let cases=[("HTTP Error 429: Too Many Requests","rate_limited"),("Sign in to confirm you're not a bot","challenge"),
            ("Requested format is not available","format_unavailable"),("No supported JavaScript runtime could be found","js_runtime"),
            ("Unsupported URL: https://private.invalid/video?token=secret","unsupported_url"),
            ("HTTP Error 403: Forbidden","http_forbidden"),("Video unavailable","unavailable"),("getaddrinfo failed","network")];
        for(message,category)in cases{
            let input=format!("ERROR: {message} https://private.invalid/video?token=DO_NOT_PERSIST Cookie: PRIVATE_COOKIE");
            let code=download_failure_code(input.as_bytes());assert_eq!(code,format!("source_download_failed_{category}"));
            assert!(!code.contains("DO_NOT_PERSIST")&&!code.contains("PRIVATE_COOKIE")&&!code.contains("https:"));
        }
        for input in [b"SECRET unknown error".as_slice(),b"",b"\xff\xfeSECRET"] {assert_eq!(download_failure_code(input),"source_download_failed");}
    }
    #[test]
    fn download_receipts_bind_exact_source_and_keep_only_closed_process_facts(){
        let source=MediaSource{account:"private-company".into(),post_key:"private-post".into(),title:"private-title".into(),
            source_url:"https://private.invalid/video?token=SECRET".into(),fallback_url:None,source_discovery:None};
        let observation=ToolObservation{stage:"exit",exit_code:Some(7),os_error:None,stderr_tail_bytes:123,child_cessation_confirmed:true,process_tree_cessation_confirmed:true,..Default::default()};
        let result=Err("source_download_failed_http_forbidden".into());
        let record=download_attempt_record("fixture-attempt",&source,&source.source_url,0,Some(&observation),Some(&result));
        let rendered=record.to_string();assert!(!rendered.contains("private")&&!rendered.contains("SECRET")&&!rendered.contains("https:"));
        assert_eq!(record["category"],"source_download_failed_http_forbidden");assert_eq!(record["exitCode"],7);
        assert_eq!(record["stderrTailBytes"],123);
        let other=download_attempt_record("fixture-attempt",&source,"https://other.invalid/video",1,None,None);
        assert_ne!(record["sourceBindingSha256"],other["sourceBindingSha256"]);
        let foreign=Err("private stderr SECRET https://invalid".into());
        assert_eq!(download_attempt_record("fixture",&source,&source.source_url,0,None,Some(&foreign))["category"],"source_download_failed_unknown");
        let dir=tempfile::tempdir().unwrap();persist_download_record(dir.path(),"fixture-attempt","outcome",&record).unwrap();
        let bytes=std::fs::read(dir.path().join("download-attempt-fixture-attempt.outcome.json")).unwrap();
        assert_eq!(serde_json::from_slice::<Value>(&bytes).unwrap(),record);
        assert!(persist_download_record(dir.path(),"fixture-attempt","outcome",&record).is_err(),"immutable receipt cannot be overwritten");
    }
    #[test]
    fn source_discovery_generic_diagnostics_survive_intent_failed_outcome_and_success_checkpoint(){
        let observation=json!({"schemaVersion":1,"platform":"fixture_connector","status":"failed","category":"http_forbidden","stage":"http",
            "url":"SECRET","headers":"SECRET","html":"SECRET","error":"SECRET","retryAllowed":true});
        let mut value=json!({"account":"fixture","postKey":"post","title":"fixture","sourceUrl":"https://fixture.invalid/source","fallbackUrl":null,"sourceDiscovery":observation});
        let source=MediaSource::from_projection(&value,"fixture","post").unwrap();
        assert!(!source.source_discovery.as_ref().unwrap().to_string().contains("SECRET"));
        let intent=download_attempt_record("fixture-intent",&source,&source.source_url,0,None,None);
        assert_eq!(intent["sourceDiscovery"]["category"],"http_forbidden");assert!(intent["category"].is_null());
        let error=Err("source_download_failed_http_forbidden".into());
        let failed=download_attempt_record("fixture-failed",&source,&source.source_url,0,None,Some(&error));
        assert_eq!(failed["sourceDiscovery"]["category"],"http_forbidden");assert_eq!(failed["category"],"source_download_failed_http_forbidden");
        assert!(!failed.to_string().contains("SECRET")&&!failed.to_string().contains("https://"));
        let completed=Ok(PathBuf::from("synthetic-source"));
        let success=download_attempt_record("fixture-success",&source,&source.source_url,0,None,Some(&completed));
        assert_eq!(success["category"],"completed");assert_eq!(success["sourceDiscovery"],intent["sourceDiscovery"]);
        value["sourceDiscovery"]=json!({"schemaVersion":1,"platform":"fixture_connector","status":"fallback_resolved","category":null,"stage":"locator_parse","error":"SECRET"});
        value["fallbackUrl"]=json!("https://fixture.invalid/bound-alternate");
        let source=MediaSource::from_projection(&value,"fixture","post").unwrap();
        let checkpoint=source.projection();
        assert_eq!(checkpoint["account"],value["account"]);assert_eq!(checkpoint["postKey"],value["postKey"]);assert_eq!(checkpoint["sourceUrl"],value["sourceUrl"]);assert_eq!(checkpoint["fallbackUrl"],value["fallbackUrl"]);
        assert_eq!(checkpoint["sourceDiscovery"]["status"],"fallback_resolved");assert!(checkpoint["sourceDiscovery"].get("error").is_none());
        assert_eq!(MediaSource::from_projection(&checkpoint,"fixture","post").unwrap().source_discovery,source.source_discovery);
        assert!(MediaSource::from_projection(&value,"other","post").is_err());assert!(MediaSource::from_projection(&value,"fixture","other").is_err());
    }
    #[test]
    fn source_discovery_invalid_optional_metadata_never_rejects_or_retargets_an_admitted_source(){
        let mut value=json!({"account":"fixture","postKey":"post","title":"fixture","sourceUrl":"https://fixture.invalid/source","fallbackUrl":"https://fixture.invalid/bound-alternate"});
        assert!(MediaSource::from_projection(&value,"fixture","post").unwrap().source_discovery.is_none());
        for metadata in [json!(null),json!("SECRET"),json!({"schemaVersion":1,"status":"failed","category":"SECRET","stage":"http"}),
            json!({"schemaVersion":1,"status":"failed","category":"network","stage":"SECRET"}),
            json!({"schemaVersion":1,"status":"failed","category":"network","stage":"http","platform":"https://private.invalid/SECRET"})]{
            value["sourceDiscovery"]=metadata;
            let source=MediaSource::from_projection(&value,"fixture","post").unwrap();
            assert!(source.source_discovery.is_none());assert_eq!(source.source_url,value["sourceUrl"].as_str().unwrap());assert_eq!(source.fallback_url.as_deref(),value["fallbackUrl"].as_str());
            assert!(source.projection().get("sourceDiscovery").is_none());
        }
    }
    #[cfg(windows)]
    #[tokio::test]
    async fn downloader_process_captures_only_normalized_rate_limit_cooldown() {
        let powershell=PathBuf::from(env::var_os("SystemRoot").unwrap_or_else(||"C:/Windows".into())).join("System32/WindowsPowerShell/v1.0/powershell.exe");
        let args=strings(&["-NoProfile","-NonInteractive","-Command",
            "[Console]::Error.WriteLine('HTTP Error 429: Too Many Requests'); [Console]::Error.WriteLine('Retry-After: 120'); [Console]::Error.WriteLine('Cookie: SECRET https://private.invalid/?token=PRIVATE'); exit 1"]);
        let mut observation=ToolObservation::default();
        let error=run_tool_until_observed(&powershell,&args,||tokio::time::sleep(Duration::from_secs(15)),false,"source_download_failed",&mut observation).await.unwrap_err();
        assert_eq!(error,"source_download_failed_rate_limited");
        assert!(observation.child_cessation_confirmed&&observation.process_tree_cessation_confirmed);
        let metadata=observation.retry_after.as_ref().unwrap().metadata();
        assert_eq!(metadata["delaySeconds"],120);
        let rendered=metadata.to_string();assert!(!rendered.contains("SECRET")&&!rendered.contains("PRIVATE")&&!rendered.contains("https:"));
    }
    #[cfg(windows)]
    #[tokio::test]
    async fn downloader_process_observation_records_exit_spawn_and_deadline_without_stderr(){
        let powershell=PathBuf::from(env::var_os("SystemRoot").unwrap_or_else(||"C:/Windows".into())).join("System32/WindowsPowerShell/v1.0/powershell.exe");
        let mut observation=ToolObservation::default();
        let args=strings(&["-NoProfile","-NonInteractive","-Command","[Console]::Error.WriteLine('HTTP Error 403 SECRET'); exit 7"]);
        let error=run_tool_until_observed(&powershell,&args,||tokio::time::sleep(Duration::from_secs(15)),false,"source_download_failed",&mut observation).await.unwrap_err();
        assert_eq!(error,"source_download_failed_http_forbidden");assert_eq!(observation.stage,"exit");assert_eq!(observation.exit_code,Some(7));
        assert!(observation.child_cessation_confirmed);
        assert!(observation.process_tree_cessation_confirmed);
        assert!(observation.stderr_tail_bytes>0);
        let mut missing=ToolObservation::default();
        let dir=tempfile::tempdir().unwrap();
        assert_eq!(run_tool_until_observed(&dir.path().join("missing.exe"),&[],||tokio::time::sleep(Duration::from_secs(1)),false,"source_download_failed",&mut missing).await.unwrap_err(),"source_download_failed_spawn_failed");
        assert_eq!(missing.stage,"spawn");assert!(missing.os_error.is_some());
        let mut expired=ToolObservation::default();
        let args=strings(&["-NoProfile","-NonInteractive","-Command","Start-Sleep -Seconds 30"]);
        assert_eq!(run_tool_until_observed(&powershell,&args,||std::future::ready(()),false,"source_download_failed",&mut expired).await.unwrap_err(),"source_download_failed_timeout");
        assert_eq!(expired.stage,"deadline");assert_eq!(expired.exit_code,None);
    }
    #[tokio::test]
    async fn downloader_stderr_is_fully_drained_but_only_bounded_tail_retained(){
        let(mut reader,mut writer)=tokio::io::duplex(1024);
        let (tail,())=tokio::join!(read_download_diagnostic(&mut reader),async {
            tokio::io::AsyncWriteExt::write_all(&mut writer,&vec![b'x';DOWNLOAD_DIAGNOSTIC_LIMIT*8]).await.unwrap();
            tokio::io::AsyncWriteExt::write_all(&mut writer,b"\nERROR: Requested format is not available").await.unwrap();
            tokio::io::AsyncWriteExt::shutdown(&mut writer).await.unwrap();
        });
        assert_eq!(tail.len(),DOWNLOAD_DIAGNOSTIC_LIMIT);assert_eq!(download_failure_code(&tail),"source_download_failed_format_unavailable");
    }
    #[test]
    fn silent_video_audio_is_explicit_evidence_never_invented_words(){
        let (text,status)=audio_observation(false,String::new());assert_eq!(status,"no_audio_stream");assert!(text.contains("No spoken words"));
        assert_eq!(audio_observation(true,String::new()).1,"inspected_no_speech");
        assert_eq!(audio_observation(true,"Real speech".into()),("Real speech".into(),"transcribed"));
    }
    #[test]
    fn source_identity_and_url_are_bounded() {
        let ok=json!({"account":"A","postKey":"native:opaque","title":"Video","sourceUrl":"https://example.org/clip"});
        assert!(MediaSource::from_projection(&ok,"A","native:opaque").is_ok());
        assert!(MediaSource::from_projection(&ok,"B","native:opaque").is_err());
        let mut unsafe_url=ok.clone(); unsafe_url["sourceUrl"]=json!("https://user:secret@example.org/clip");
        assert!(MediaSource::from_projection(&unsafe_url,"A","native:opaque").is_err());
    }
    #[test]
    fn ocr_tsv_filters_low_confidence_and_short_noise() {
        let tsv="level\tpage_num\tblock_num\tpar_num\tline_num\tword_num\tleft\ttop\twidth\theight\tconf\ttext\n5\t1\t1\t1\t1\t1\t0\t0\t1\t1\t91\tТест\n5\t1\t1\t1\t1\t2\t0\t0\t1\t1\t88\tмашины\n5\t1\t1\t1\t2\t1\t0\t0\t1\t1\t20\tмусор";
        assert_eq!(tsv_lines(tsv),vec!["Тест машины"]);
    }
    #[test]
    fn sampled_ocr_absence_is_distinct_from_partial_or_failed_extraction(){
        let absent=ocr_sample_outcome("",2,0,3.0);assert_eq!(absent["status"],"no_text_found");
        assert_eq!(absent["coverage"],"sampled_frames");assert_eq!(absent["exhaustive"],false);
        assert_eq!(ocr_sample_outcome("",2,1,3.0)["status"],"partial");
        assert_eq!(ocr_sample_outcome("",2,2,3.0)["status"],"failed");
        assert_eq!(ocr_sample_outcome("2 300 000",2,0,3.0)["status"],"completed");
    }
    #[test]
    fn audio_windows_and_coverage_include_long_video_tail(){
        assert_eq!(audio_coverage(Some(899.0),Some(899.0)),(false,"full_audio"));
        assert_eq!(audio_windows(901.0).unwrap(),vec![(0.0,900.0),(900.0,1.0)]);
        assert_eq!(audio_windows(1800.0).unwrap(),vec![(0.0,900.0),(900.0,900.0)]);
        assert_eq!(audio_coverage(Some(1800.0),Some(900.0)),(true,"audio_shorter_than_media"));
        assert_eq!(audio_coverage(Some(901.0),Some(900.0)),(true,"audio_shorter_than_media"));
        assert_eq!(audio_coverage(Some(1800.0),Some(1800.0)),(false,"full_audio"));
        assert!(!audio_segment_covered(900.0,Some(870.0)));
        assert!(audio_segment_covered(900.0,Some(899.9)));
        assert_eq!(audio_windows(900.0*17.0).unwrap_err(),"audio_duration_limit");
        assert_eq!(audio_coverage(Some(400.0),Some(200.0)),(true,"audio_shorter_than_media"));
        assert_eq!(audio_coverage(None,Some(100.0)),(true,"media_duration_unknown"));
        assert_eq!(audio_coverage(Some(100.0),None),(true,"audio_duration_unknown"));
    }
    #[test]
    fn native_downloader_pins_merge_runtime_and_requires_video_on_every_selector_branch() {
        let config=MediaConfig { ytdlp:PathBuf::from("yt-dlp.exe"),ytdlp_python:false,
            ytdlp_node:Some(PathBuf::from("node.exe")),ffmpeg:PathBuf::from("pinned/ffmpeg.exe"),ffprobe:PathBuf::new(),
            whisper:PathBuf::new(),model:PathBuf::new(),tesseract:None,tessdata:None,scratch:PathBuf::new() };
        let args=download_args(&config,Path::new("work"));
        let format=args.iter().position(|value|value=="--format").unwrap();
        assert_eq!(args[format+1],VIDEO_DOWNLOAD_FORMAT);
        assert_eq!(VIDEO_DOWNLOAD_FORMAT.split('/').count(),6);
        for branch in VIDEO_DOWNLOAD_FORMAT.split('/') {assert!(branch.starts_with("bestvideo")||branch.starts_with("best[")&&branch.contains("[vcodec!=none]"));}
        let ffmpeg=args.iter().position(|value|value=="--ffmpeg-location").unwrap();assert_eq!(args[ffmpeg+1],config.ffmpeg.display().to_string());
        let container=args.iter().position(|value|value=="--merge-output-format").unwrap();assert_eq!(args[container+1],"mkv");
        for option in ["--retries","--fragment-retries","--extractor-retries","--file-access-retries"] {
            let index=args.iter().position(|value|value==option).unwrap();assert_eq!(args[index+1],"0");
        }
        assert!(args.contains(&"--abort-on-unavailable-fragments".into()));
        assert_eq!(args.iter().filter(|arg|*arg=="--format").count(),1);
        let js=args.iter().position(|value|value=="--js-runtimes").unwrap();
        assert_eq!(args[js+1],"node:node.exe");
        assert_eq!(args[js-1],"--no-js-runtimes");
        assert!(args.contains(&"--no-remote-components".to_owned()));
    }
    #[test]
    fn video_inventory_queries_streams_before_ocr() {
        let args=video_stream_args(Path::new("audio.webm"));
        assert_eq!(args,strings(&["-v","error","-select_streams","v","-show_entries","stream=index","-of","csv=p=0","audio.webm"]));
    }
    /// A real, explicitly invoked local acceptance path. It never downloads;
    /// production callers cannot supply a filesystem input.
    #[tokio::test]
    #[ignore = "requires explicit native tools/model and a private local sample"]
    async fn local_sample_acceptance() {
        let config=MediaConfig::from_env().expect("native media paths are required");
        let input=PathBuf::from(env::var_os("COMMUNITYHERO_MEDIA_SAMPLE_INPUT").expect("sample input path required"));
        let output=PathBuf::from(env::var_os("COMMUNITYHERO_MEDIA_ACCEPTANCE_OUTPUT").expect("private output path required"));
        assert!(input.is_absolute()&&input.is_file()&&output.is_absolute());
        std::fs::create_dir_all(&config.scratch).unwrap();
        let work=config.scratch.join(format!("acceptance-{}",uuid::Uuid::new_v4()));
        std::fs::create_dir(&work).unwrap();
        let _scratch=ScratchGuard(work.clone());
        let required=|name|env::var(name).unwrap_or_else(|_|panic!("{name} is required for the local acceptance test"));
        let source=MediaSource { account:required("COMMUNITYHERO_MEDIA_SAMPLE_ACCOUNT"),
            post_key:required("COMMUNITYHERO_MEDIA_SAMPLE_POST_KEY"), title:required("COMMUNITYHERO_MEDIA_SAMPLE_TITLE"),
            source_url:required("COMMUNITYHERO_MEDIA_SAMPLE_SOURCE_URL"), fallback_url:None,source_discovery:None };
        let result=transcribe_input(&config,&source,&work,&input).await.expect("real ASR processing must succeed");
        assert!(result["materials"][0]["text"].as_str().is_some_and(|text|!text.is_empty()));
        assert!(result["materials"][0]["transcription"]["mediaDurationSeconds"].is_number());
        std::fs::write(output,serde_json::to_vec_pretty(&result).unwrap()).unwrap();
    }
    /// Root-owned functional canary only: exact local input, real configured
    /// vision backend and ASR, no download/provider or production database.
    #[tokio::test]
    #[ignore = "requires explicit reviewed local vision model and private video"]
    async fn local_visual_sample_acceptance(){
        preflight().expect("explicit visual backend configuration required");
        let config=MediaConfig::from_env().unwrap();
        let required=|name|env::var(name).unwrap_or_else(|_|panic!("{name} is required"));
        let input=PathBuf::from(required("COMMUNITYHERO_MEDIA_SAMPLE_INPUT"));
        let output=PathBuf::from(required("COMMUNITYHERO_MEDIA_VISUAL_ACCEPTANCE_OUTPUT"));
        assert!(input.is_absolute()&&input.is_file()&&output.is_absolute());
        let source=MediaSource{account:required("COMMUNITYHERO_MEDIA_SAMPLE_ACCOUNT"),post_key:required("COMMUNITYHERO_MEDIA_SAMPLE_POST_KEY"),title:required("COMMUNITYHERO_MEDIA_SAMPLE_TITLE"),source_url:required("COMMUNITYHERO_MEDIA_SAMPLE_SOURCE_URL"),fallback_url:None,source_discovery:None};
        std::fs::create_dir_all(&config.scratch).unwrap();
        let work=config.scratch.join(format!("media-{}",uuid::Uuid::new_v4()));std::fs::create_dir(&work).unwrap();let _scratch=ScratchGuard(work.clone());
        let (mut app,_temp)=crate::tests::test_app().await;
        app.bridge=PathBuf::from(required("COMMUNITYHERO_MEDIA_VISUAL_ACCEPTANCE_BRIDGE"));
        app.node=PathBuf::from(required("COMMUNITYHERO_NODE"));app.external_writes=false;
        let result=process_downloaded(&config,&source,&work,&input,&app,"fixture-media",None).await.expect("audio and visual canary must both complete");
        let visual=result["materials"].as_array().unwrap().iter().find(|m|m["kind"]=="visual_context").expect("visual result");
        super::super::media_visual::validate(&visual["visualEvidence"]).unwrap();
        assert!(visual["visualEvidence"]["manifest"]["frames"].as_array().unwrap().iter().all(|f|f.get("path").is_none()));
        std::fs::write(output,serde_json::to_vec_pretty(&result).unwrap()).unwrap();
    }
    #[cfg(windows)]
    #[tokio::test]
    async fn timeout_stops_spawned_process_tree() {
        use windows_sys::Win32::{Foundation::CloseHandle,System::Threading::{GetExitCodeProcess,OpenProcess,PROCESS_QUERY_LIMITED_INFORMATION}};
        let temp=tempfile::tempdir().unwrap();
        let script=temp.path().join("child.ps1");
        let pidfile=temp.path().join("child.pid");
        std::fs::write(&script,"$child = Start-Process -FilePath (Join-Path $PSHOME 'powershell.exe') -ArgumentList @('-NoProfile','-NonInteractive','-Command','Start-Sleep -Seconds 30') -PassThru -WindowStyle Hidden\n[System.IO.File]::WriteAllText($args[0], [string]$child.Id)\nStart-Sleep -Seconds 30\n").unwrap();
        let powershell=PathBuf::from(env::var_os("SystemRoot").unwrap()).join("System32/WindowsPowerShell/v1.0/powershell.exe");
        let args=vec!["-NoProfile".into(),"-NonInteractive".into(),"-File".into(),script.display().to_string(),pidfile.display().to_string()];
        // Under a busy full suite PowerShell startup can exceed five seconds.
        // Arm this test's timeout only after its grandchild is observed, so a
        // pass proves that termination reached an actual descendant.
        let ready=async {
            loop {
                if std::fs::read_to_string(&pidfile).ok().and_then(|raw|raw.parse::<u32>().ok()).is_some(){break;}
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        };
        let result=tokio::time::timeout(Duration::from_secs(45),
            run_tool_until(&powershell,&args,||ready,false,"test_process")).await
            .expect("helper did not become ready or terminate within the test bound");
        assert_eq!(result.unwrap_err(),"test_process_timeout");
        let pid=std::fs::read_to_string(&pidfile).expect("helper must have created descendant").parse::<u32>().unwrap();
        let mut stopped=false;
        for _ in 0..30 {
            unsafe {
                let handle=OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION,0,pid);
                if handle.is_null(){stopped=true;} else {
                    let mut code=0;
                    if GetExitCodeProcess(handle,&mut code)!=0 && code!=259 {stopped=true;}
                    CloseHandle(handle);
                }
            }
            if stopped {break;}
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        assert!(stopped,"descendant remained live after timeout");
    }
}
