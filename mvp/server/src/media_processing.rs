//! Product-owned media processing. Connectors resolve sources; this module only
//! processes an admitted source and returns materials for the normal catalog.
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{env, io::Read, path::{Path, PathBuf}, process::Stdio, time::Duration};
use tokio::{io::AsyncReadExt, process::Command};

#[derive(Clone, Debug)]
pub(crate) struct MediaSource {
    pub account: String,
    pub post_key: String,
    pub title: String,
    pub source_url: String,
    pub fallback_url: Option<String>,
}

impl MediaSource {
    pub fn from_projection(value: &Value, expected_account: &str, expected_post_key: &str) -> Result<Self, String> {
        let field = |key| value[key].as_str().unwrap_or("").trim().to_owned();
        let fallback=field("fallbackUrl");
        let source = Self { account: field("account"), post_key: field("postKey"), title: field("title"), source_url: field("sourceUrl"),
            fallback_url:(!fallback.is_empty()).then_some(fallback) };
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
        Ok(source)
    }
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
        let value = env::var_os(name).ok_or_else(|| format!("media_config_missing_{name}"))?;
        let path = PathBuf::from(value);
        if !path.is_absolute() || !path.is_file() { return Err(format!("media_config_invalid_{name}")); }
        Ok(path)
    }
    fn from_env() -> Result<Self, String> {
        let (ytdlp, ytdlp_python) = match (env::var_os("COMMUNITYHERO_MEDIA_YTDLP"), env::var_os("COMMUNITYHERO_MEDIA_YTDLP_PYTHON")) {
            (Some(_), Some(_)) => return Err("media_config_conflicting_ytdlp".into()),
            (Some(_), None) => (Self::tool("COMMUNITYHERO_MEDIA_YTDLP")?, false),
            (None, Some(_)) => (Self::tool("COMMUNITYHERO_MEDIA_YTDLP_PYTHON")?, true),
            (None, None) => return Err("media_config_missing_ytdlp".into()),
        };
        let scratch = PathBuf::from(env::var_os("COMMUNITYHERO_MEDIA_SCRATCH_DIR").ok_or("media_config_missing_scratch")?);
        if !scratch.is_absolute() { return Err("media_config_invalid_scratch".into()); }
        let ytdlp_node=match env::var_os("COMMUNITYHERO_MEDIA_YTDLP_NODE") {
            Some(_) => Some(Self::tool("COMMUNITYHERO_MEDIA_YTDLP_NODE")?),
            None => None,
        };
        let tesseract = match env::var_os("COMMUNITYHERO_MEDIA_TESSERACT") {
            Some(_) => Some(Self::tool("COMMUNITYHERO_MEDIA_TESSERACT")?),
            None => None,
        };
        let tessdata=match env::var_os("COMMUNITYHERO_MEDIA_TESSDATA_PREFIX") {
            Some(value) => {let path=PathBuf::from(value);if !path.is_absolute()||!path.is_dir(){return Err("media_config_invalid_tessdata".into());}Some(path)},
            None=>None,
        };
        Ok(Self { ytdlp, ytdlp_python, ytdlp_node, ffmpeg: Self::tool("COMMUNITYHERO_MEDIA_FFMPEG")?,
            ffprobe: Self::tool("COMMUNITYHERO_MEDIA_FFPROBE")?, whisper: Self::tool("COMMUNITYHERO_MEDIA_WHISPER_CLI")?,
            model: Self::tool("COMMUNITYHERO_MEDIA_WHISPER_MODEL")?, tesseract, tessdata, scratch })
    }
}

#[path="media_full_processing.rs"]
pub(crate) mod full;

pub(crate) fn preflight() -> Result<(), String> {
    MediaConfig::from_env()?;
    let evidence=PathBuf::from(env::var_os("COMMUNITYHERO_MEDIA_EVIDENCE_DIR").ok_or("visual_evidence_dir_missing")?);
    if !evidence.is_absolute(){return Err("visual_evidence_dir_invalid".into());}
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
        pub async fn stop_and_wait(&self) {
            let mut terminated=false;
            loop {
                if !terminated { terminated=unsafe { TerminateJobObject(self.0,1)!=0 }; }
                let mut info: JOBOBJECT_BASIC_ACCOUNTING_INFORMATION=unsafe { std::mem::zeroed() };
                let observed=unsafe { QueryInformationJobObject(self.0,JobObjectBasicAccountingInformation,
                    &mut info as *mut _ as *mut _,std::mem::size_of_val(&info) as u32,std::ptr::null_mut()) };
                if observed!=0 && info.ActiveProcesses==0 { return; }
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
    let text=String::from_utf8_lossy(stderr).to_ascii_lowercase();
    let has=|needles:&[&str]|needles.iter().any(|needle|text.contains(needle));
    // Categories are diagnostic hints, never permission to use credentials,
    // install components, bypass controls or retry a consumed source.
    if has(&["http error 429","too many requests","rate limit"]){"source_download_failed_rate_limited"}
    else if has(&["sign in","login required","authentication required","cookies are required","private video"]){"source_download_failed_auth"}
    else if has(&["requested format is not available","requested format not available","no video formats found"]){"source_download_failed_format_unavailable"}
    else if has(&["no supported javascript runtime","javascript runtime is not available","challenge solving failed","n challenge solving failed"]){"source_download_failed_js_runtime"}
    else if has(&["http error 403","403 forbidden"]){"source_download_failed_http_forbidden"}
    else if has(&["video unavailable","video is unavailable","has been removed","does not exist","not available in your country"]){"source_download_failed_unavailable"}
    else if has(&["timed out","timeout","name resolution","getaddrinfo failed","connection refused","connection reset","network is unreachable"]){"source_download_failed_network"}
    else {"source_download_failed"}
}

async fn run_tool_until<D, F>(exe: &Path, args: &[String], deadline: D, capture: bool, code: &str) -> Result<String, String>
where D: FnOnce() -> F, F: std::future::Future<Output=()> {
    let mut command = Command::new(exe);
    let diagnose_download=code=="source_download_failed"&&!capture;
    command.args(args).stdin(Stdio::null()).stderr(if diagnose_download {Stdio::piped()} else {Stdio::null()})
        .stdout(if capture { Stdio::piped() } else { Stdio::null() }).kill_on_drop(true);
    if args.first().is_some_and(|arg|arg=="-m") {
        command.env_remove("PYTHONPATH").env_remove("PYTHONHOME").env_remove("PYTHONSTARTUP")
            .env("PYTHONSAFEPATH","1").env("PYTHONNOUSERSITE","1");
    }
    #[cfg(windows)] command.creation_flags(0x08000000);
    let mut child = command.spawn().map_err(|_| format!("{code}_spawn_failed"))?;
    #[cfg(windows)]
    let tree = match process_tree::TreeGuard::attach(child.id().ok_or_else(|| format!("{code}_spawn_failed"))?) {
        Ok(tree) => tree,
        Err(error) => { let _ = child.kill().await; let _ = child.wait().await; return Err(error); }
    };
    let output = async {
        let mut stderr=child.stderr.take();
        let mut bytes = Vec::new();
        if capture {
            let mut stdout = child.stdout.take().ok_or_else(|| format!("{code}_output_failed"))?;
            (&mut stdout).take(4 * 1024 * 1024 + 1).read_to_end(&mut bytes).await.map_err(|_| format!("{code}_output_failed"))?;
            if bytes.len() > 4 * 1024 * 1024 { return Err(format!("{code}_output_limit")); }
        }
        let (status,diagnostic)=tokio::join!(child.wait(),async {
            match stderr.as_mut(){Some(reader)=>read_download_diagnostic(reader).await,None=>Vec::new()}
        });
        let status=status.map_err(|_| format!("{code}_wait_failed"))?;
        if !status.success() { return Err(if diagnose_download {download_failure_code(&diagnostic).to_owned()} else {code.to_owned()}); }
        Ok(String::from_utf8_lossy(&bytes).into_owned())
    };
    let deadline_signal=deadline();
    tokio::pin!(deadline_signal);
    let result = tokio::select! {
        result=output => result,
        _=&mut deadline_signal => Err(format!("{code}_timeout")),
    };
    #[cfg(windows)]
    tree.stop_and_wait().await;
    #[cfg(windows)] drop(tree);
    if result.is_err() { let _ = child.kill().await; let _ = child.wait().await; }
    result
}

fn strings(values: &[&str]) -> Vec<String> { values.iter().map(|value| (*value).to_owned()).collect() }
fn normalized(text: &str) -> String { text.split_whitespace().collect::<Vec<_>>().join(" ") }
fn digest(value: &str) -> String { format!("{:x}", Sha256::digest(value.as_bytes())) }
fn audio_coverage(media: Option<f64>, audio: Option<f64>) -> (bool, &'static str) {
    match (media,audio) {
        (Some(media),Some(_audio)) if media>900.0 => (true,"first_900_seconds"),
        (Some(media),Some(audio)) if audio+2.0>=media => (false,"full_audio"),
        (Some(_),Some(_)) => (true,"audio_shorter_than_media"),
        (_,None) => (true,"audio_duration_unknown"),
        (None,Some(_)) => (true,"media_duration_unknown"),
    }
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
    let mut files = std::fs::read_dir(work).map_err(|_| "source_file_missing".to_owned())?
        .filter_map(Result::ok).map(|entry| entry.path())
        .filter(|path| path.is_file() && path.file_name().and_then(|v| v.to_str()).is_some_and(|name| name.starts_with("source.") && !name.ends_with(".part") && !name.ends_with(".ytdl")))
        .collect::<Vec<_>>();
    if files.len() != 1 { return Err("source_file_missing".into()); }
    let file=files.remove(0);
    // yt-dlp's bound applies to each stream; enforce the source bound again on
    // the final merged container before probing, copying to CAS or decoding.
    if std::fs::metadata(&file).map_err(|_|"source_file_missing")?.len()>500*1024*1024 {
        return Err("source_file_too_large".into());
    }
    Ok(file)
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

async fn ocr(config: &MediaConfig, input: &Path, work: &Path, duration: Option<f64>) -> (String, Value) {
    if has_video_stream(config,input).await==Some(false) {
        return (String::new(),json!({"status":"not_applicable","reason":"no_video_stream"}));
    }
    let Some(tesseract) = &config.tesseract else { return (String::new(), json!({"status":"unavailable","reason":"tesseract_not_configured"})); };
    let frames=work.join("frames");
    if std::fs::create_dir(&frames).is_err() { return (String::new(), json!({"status":"failed","reason":"ocr_scratch_failed"})); }
    let interval=duration.filter(|value| *value > 0.0).map_or(3.0, |value| (value / 30.0).max(2.0));
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
    for path in paths.iter().take(30) {
        let mut args=vec![path.display().to_string(),"stdout".into()];
        if let Some(tessdata)=&config.tessdata {args.extend(["--tessdata-dir".into(),tessdata.display().to_string()]);}
        args.extend(strings(&["-l","rus+eng","--psm","11","tsv"]));
        match run_tool(tesseract,&args,Duration::from_secs(120),true,"ocr_failed").await {
            Ok(value)=>for line in tsv_lines(&value) { if seen.insert(line.to_lowercase()) { lines.push(line); } },
            Err(_)=>failed+=1,
        }
    }
    let mut text=lines.join("\n");
    text.truncate(text.floor_char_boundary(text.len().min(12000)));
    let status=if failed==paths.len().min(30) {"failed"} else if failed>0 {"partial"} else {"completed"};
    (text,json!({"status":status,"sampledFrames":paths.len().min(30),"failedFrames":failed,"maxFrames":30,"intervalSeconds":interval}))
}

pub(crate) async fn process(source: &MediaSource, app: &super::App) -> Result<Value, String> {
    let config=MediaConfig::from_env()?;
    std::fs::create_dir_all(&config.scratch).map_err(|_| "media_scratch_unavailable".to_owned())?;
    let work=config.scratch.join(format!("media-{}",uuid::Uuid::new_v4()));
    std::fs::create_dir(&work).map_err(|_| "media_scratch_unavailable".to_owned())?;
    let _scratch=ScratchGuard(work.clone());
    process_in(&config,source,&work,app).await
}

// Prefer <=720p with audio, including DASH/HLS split streams. A silent-video
// fallback is last; every branch requires video, never bestaudio by itself.
const VIDEO_DOWNLOAD_FORMAT: &str = "bestvideo[height<=720]+bestaudio/best[height<=720][vcodec!=none]/bestvideo+bestaudio/best[vcodec!=none]/bestvideo[height<=720]/bestvideo";
fn download_args(config: &MediaConfig, work: &Path) -> Vec<String> {
    let mut download=Vec::new();
    if config.ytdlp_python { download.extend(strings(&["-m","yt_dlp"])); }
    download.extend(strings(&["--ignore-config","--no-plugin-dirs","--no-remote-components","--quiet","--no-warnings","--no-progress","--no-playlist"]));
    if let Some(node)=&config.ytdlp_node { download.extend(["--no-js-runtimes".into(),"--js-runtimes".into(),format!("node:{}",node.display())]); }
    download.extend(["--ffmpeg-location".into(),config.ffmpeg.display().to_string()]);
    download.extend(strings(&["--merge-output-format","mkv","--socket-timeout","30","--retries","2","--fragment-retries","2","--max-filesize","500M","--format",VIDEO_DOWNLOAD_FORMAT,"--output"]));
    download.push(work.join("source.%(ext)s").display().to_string());
    download
}

async fn process_in(config: &MediaConfig, source: &MediaSource, work: &Path, app: &super::App) -> Result<Value, String> {
    let download=download_args(config,work);
    let mut input=None;
    for locator in std::iter::once(&source.source_url).chain(source.fallback_url.iter()) {
        let mut args=download.clone();args.push(locator.clone());
        match run_tool(&config.ytdlp,&args,Duration::from_secs(90),false,"source_download_failed").await.and_then(|_|downloaded(work)) {
            Ok(file)=>{input=Some(file);break;}
            Err(error) if locator==&source.source_url && source.fallback_url.is_some() &&
                matches!(error.as_str(),"source_download_failed"|"source_file_missing"|
                    "source_download_failed_auth"|"source_download_failed_rate_limited"|"source_download_failed_format_unavailable"|
                    "source_download_failed_js_runtime"|"source_download_failed_http_forbidden"|"source_download_failed_unavailable"|"source_download_failed_network")=>{
                    for entry in std::fs::read_dir(work).map_err(|_|"media_scratch_unavailable".to_owned())?.filter_map(Result::ok){
                        let path=entry.path();if path.is_file()&&path.file_name().and_then(|name|name.to_str()).is_some_and(|name|name.starts_with("source.")){
                            let _=std::fs::remove_file(path);
                        }
                    }
                }
            Err(error)=>return Err(error),
        }
    }
    let input=input.ok_or("source_file_missing")?;
    process_downloaded(config,source,work,&input,app).await
}
async fn process_downloaded(config:&MediaConfig,source:&MediaSource,work:&Path,input:&Path,app:&super::App)->Result<Value,String>{
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
    let response=app.bridge("media_vision",request.clone()).await.map_err(|_|"visual_backend_failed".to_owned())?;
    let evidence=super::media_visual::admit(&request,&response).map_err(str::to_owned)?;
    let mut result=transcribe_input(config,source,work,input).await?;
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
    let media_sha=file_digest(input)?;
    let duration=probe_duration(config,input).await;
    let audio=work.join("audio.wav");
    let audio_args=vec!["-nostdin".into(),"-hide_banner".into(),"-loglevel".into(),"error".into(),"-y".into(),"-i".into(),input.display().to_string(),
        "-vn".into(),"-ac".into(),"1".into(),"-ar".into(),"16000".into(),"-t".into(),"900".into(),audio.display().to_string()];
    let audio_stream_args=vec!["-v".into(),"error".into(),"-select_streams".into(),"a".into(),"-show_entries".into(),"stream=index".into(),"-of".into(),"csv=p=0".into(),input.display().to_string()];
    let has_audio=!run_tool(&config.ffprobe,&audio_stream_args,Duration::from_secs(30),true,"audio_inventory_failed").await?.trim().is_empty();
    if has_audio {run_tool(&config.ffmpeg,&audio_args,Duration::from_secs(900),false,"ffmpeg_failed").await?;}
    let audio_duration=if has_audio {probe_duration(config,&audio).await} else {duration};
    let whisper_args=vec!["-m".into(),config.model.display().to_string(),"-f".into(),audio.display().to_string(),"-l".into(),"auto".into(),"-nt".into(),"-np".into(),"--no-fallback".into()];
    let raw=if has_audio {normalized(&run_tool(&config.whisper,&whisper_args,Duration::from_secs(3600),true,"whisper_failed").await?)} else {String::new()};
    let (transcript,audio_status)=audio_observation(has_audio,raw);
    let (ocr_text,ocr_meta)=ocr(config,&input,work,duration).await;
    let (partial,coverage)=audio_coverage(duration,audio_duration);
    let key=digest(&format!("{}\n{}",source.account,source.post_key));
    let transcription=json!({"audioStatus":audio_status,"model":"local-whisper","modelFile":config.model.file_name().and_then(|name|name.to_str()).unwrap_or("unknown"),
        "partial":partial,"maxAudioSeconds":900,"mediaDurationSeconds":duration,"audioDurationSeconds":audio_duration,
        "coverage":coverage,"sourcePostKey":source.post_key,"sourceLocatorSha256":digest(&source.source_url),"ocr":ocr_meta});
    let mut materials=vec![json!({"id":format!("media:transcript:{key}"),"title":format!("Видео: {}",source.title),"text":transcript,
        "kind":"transcript","account":source.account,"postKey":source.post_key,"sourceUrl":source.source_url,"mediaSha256":media_sha,"transcription":transcription})];
    if !ocr_text.is_empty(){materials.push(json!({"id":format!("media:ocr:{key}"),"title":format!("Текст в кадре: {}",source.title),"text":ocr_text,
        "kind":"ocr","account":source.account,"postKey":source.post_key,"sourceUrl":source.source_url,"mediaSha256":media_sha,"ocr":ocr_meta}));}
    Ok(json!({"materials":materials,"reused":false}))
}

#[cfg(test)]
mod tests {
    use super::*;
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
    fn downloader_diagnostics_return_only_closed_codes_never_private_stderr(){
        let cases=[("HTTP Error 429: Too Many Requests","rate_limited"),("Sign in to confirm you're not a bot","auth"),
            ("Requested format is not available","format_unavailable"),("No supported JavaScript runtime could be found","js_runtime"),
            ("HTTP Error 403: Forbidden","http_forbidden"),("Video unavailable","unavailable"),("getaddrinfo failed","network")];
        for(message,category)in cases{
            let input=format!("ERROR: {message} https://private.invalid/video?token=DO_NOT_PERSIST Cookie: PRIVATE_COOKIE");
            let code=download_failure_code(input.as_bytes());assert_eq!(code,format!("source_download_failed_{category}"));
            assert!(!code.contains("DO_NOT_PERSIST")&&!code.contains("PRIVATE_COOKIE")&&!code.contains("https:"));
        }
        for input in [b"SECRET unknown error".as_slice(),b"",b"\xff\xfeSECRET"] {assert_eq!(download_failure_code(input),"source_download_failed");}
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
    fn audio_coverage_never_calls_unknown_or_long_media_full() {
        assert_eq!(audio_coverage(Some(899.0),Some(899.0)),(false,"full_audio"));
        assert_eq!(audio_coverage(Some(901.0),Some(900.0)),(true,"first_900_seconds"));
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
            source_url:required("COMMUNITYHERO_MEDIA_SAMPLE_SOURCE_URL"), fallback_url:None };
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
        let source=MediaSource{account:required("COMMUNITYHERO_MEDIA_SAMPLE_ACCOUNT"),post_key:required("COMMUNITYHERO_MEDIA_SAMPLE_POST_KEY"),title:required("COMMUNITYHERO_MEDIA_SAMPLE_TITLE"),source_url:required("COMMUNITYHERO_MEDIA_SAMPLE_SOURCE_URL"),fallback_url:None};
        std::fs::create_dir_all(&config.scratch).unwrap();
        let work=config.scratch.join(format!("media-{}",uuid::Uuid::new_v4()));std::fs::create_dir(&work).unwrap();let _scratch=ScratchGuard(work.clone());
        let (mut app,_temp)=crate::tests::test_app().await;
        app.bridge=PathBuf::from(required("COMMUNITYHERO_MEDIA_VISUAL_ACCEPTANCE_BRIDGE"));
        app.node=PathBuf::from(required("COMMUNITYHERO_NODE"));app.external_writes=false;
        let result=process_downloaded(&config,&source,&work,&input,&app).await.expect("audio and visual canary must both complete");
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
