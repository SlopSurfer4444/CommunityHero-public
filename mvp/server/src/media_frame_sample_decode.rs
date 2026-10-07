//! Bounded source-PTS sampling into the existing immutable CAS. No model,
//! download, OCR, provider, job admission or paid-repair authority lives here.
use crate::media_artifacts::{ArtifactRef, ArtifactStore};
use crate::media_frame_sample::{CONTRACT, digest, sha, source_time, uint, validate_plan};
use crate::runtime_native_child::OwnedChild;
use crate::runtime_owned_work::{Kind, Registry};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    future::Future,
    io::Read,
    path::{Path, PathBuf},
    pin::Pin,
    time::{Duration, Instant},
};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt},
    process::Command,
};

const META_LIMIT: u64 = 1024 * 1024;
const DIAGNOSTIC_LIMIT: u64 = 4 * 1024 * 1024;
const LINE_LIMIT: usize = 64 * 1024;

#[derive(Clone, Debug)]
pub(crate) struct SampleTools {
    pub ffmpeg: PathBuf,
    pub ffprobe: PathBuf,
    pub ffmpeg_sha256: String,
    pub ffprobe_sha256: String,
    pub ffmpeg_version: String,
    pub ffprobe_version: String,
    /// Additional caller deadline; the profile can only shorten it.
    pub deadline: Duration,
}
impl SampleTools {
    /// Only configured FFmpeg/FFprobe are required. Pipes and CAS need no
    /// downloader, ASR/OCR/model configuration or mutable scratch output.
    pub(crate) async fn from_env(registry: &Registry, limit: Duration) -> Result<Self, String> {
        let path = |key| {
            std::env::var_os(key)
                .map(PathBuf::from)
                .filter(|p| p.is_absolute() && p.is_file())
                .ok_or_else(|| format!("frame_sample_config_missing_{key}"))
        };
        let ffmpeg = path("COMMUNITYHERO_MEDIA_FFMPEG")?;
        let ffprobe = path("COMMUNITYHERO_MEDIA_FFPROBE")?;
        if limit.is_zero() {
            return Err("frame_sample_deadline_invalid".into());
        }
        let started = Instant::now();
        let ffmpeg_sha256 = hash_file(&ffmpeg)?;
        let ffprobe_sha256 = hash_file(&ffprobe)?;
        let process = LocalProcess { registry };
        let version = |executable| ProcessSpec {
            kind: "version",
            executable,
            args: vec!["-version".into()],
            stdin: vec![],
            stdout_limit: 64 * 1024,
            stderr_limit: 64 * 1024,
            deadline: Duration::from_secs(10).min(limit),
            guard: None,
        };
        let mut ffmpeg_spec = version(ffmpeg.clone());
        ffmpeg_spec.deadline = deadline(started, limit)?.min(ffmpeg_spec.deadline);
        let ffmpeg_out = execute(&process, &ffmpeg_spec).await?;
        let mut ffprobe_spec = version(ffprobe.clone());
        ffprobe_spec.deadline = deadline(started, limit)?.min(ffprobe_spec.deadline);
        let ffprobe_out = execute(&process, &ffprobe_spec).await?;
        let line = |bytes: &[u8]| -> Result<String, String> {
            std::str::from_utf8(bytes)
                .ok()
                .and_then(|s| s.lines().next())
                .filter(|s| !s.is_empty() && s.len() <= 4096)
                .map(str::to_owned)
                .ok_or("frame_sample_tool_version_missing".into())
        };
        let tools = Self {
            ffmpeg,
            ffprobe,
            ffmpeg_sha256,
            ffprobe_sha256,
            ffmpeg_version: line(&ffmpeg_out.stdout)?,
            ffprobe_version: line(&ffprobe_out.stdout)?,
            deadline: limit,
        };
        tool_spec(&tools)?;
        deadline(started, limit)?;
        Ok(tools)
    }
}

#[derive(Clone, Debug)]
pub(crate) struct ProcessSpec {
    pub kind: &'static str,
    pub executable: PathBuf,
    pub args: Vec<String>,
    pub stdin: Vec<u8>,
    pub stdout_limit: u64,
    pub stderr_limit: u64,
    pub deadline: Duration,
    guard: Option<DecodeGuard>,
}
#[derive(Debug)]
pub(crate) struct ProcessOutput {
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}
pub(crate) trait SampleProcess: Send + Sync {
    fn execute<'a>(
        &'a self,
        spec: &'a ProcessSpec,
    ) -> Pin<Box<dyn Future<Output = Result<ProcessOutput, String>> + Send + 'a>>;
}
pub(crate) struct LocalProcess<'r> {
    registry: &'r Registry,
}
impl SampleProcess for LocalProcess<'_> {
    fn execute<'a>(
        &'a self,
        spec: &'a ProcessSpec,
    ) -> Pin<Box<dyn Future<Output = Result<ProcessOutput, String>> + Send + 'a>> {
        Box::pin(run_process(spec, self.registry))
    }
}

#[derive(Clone, Debug)]
struct Metadata {
    index: u64,
    width: u64,
    height: u64,
    num: u64,
    den: u64,
    start_pts: i64,
    duration_ms: u64,
}
#[derive(Clone, Debug)]
struct DecodeGuard {
    metadata: Metadata,
    target_ms: u64,
    end_ms: u64,
    max_preroll_ms: u64,
    max_frames: u64,
    max_duration_ms: u64,
}
#[derive(Default)]
struct Observation {
    count: u64,
    first: Option<i64>,
    last: Option<i64>,
    pts: Vec<i64>,
}
impl Observation {
    fn push(&mut self, line: &str, guard: &DecodeGuard) -> Result<(), String> {
        if !line.contains("showinfo") || !line.contains(" n:") {
            return Ok(());
        }
        let value = line
            .split(" pts:")
            .nth(1)
            .and_then(|s| s.split_whitespace().next())
            .and_then(|s| s.parse::<i64>().ok())
            .ok_or("frame_sample_decode_pts_missing")?;
        if self.last.is_some_and(|last| value < last) {
            return Err("frame_sample_decode_pts_backward".into());
        }
        let offset = relative_numerator(value, &guard.metadata)?;
        let den = i128::from(guard.metadata.den);
        if offset < (i128::from(guard.target_ms) - i128::from(guard.max_preroll_ms)) * den
            || offset > i128::from(guard.end_ms.saturating_add(1000)) * den
        {
            return Err("frame_sample_preroll_or_window_exceeded".into());
        }
        self.count = self
            .count
            .checked_add(1)
            .ok_or("frame_sample_decode_count_overflow")?;
        if self.count > guard.max_frames {
            return Err("frame_sample_decode_count_exceeded".into());
        }
        self.first.get_or_insert(value);
        self.last = Some(value);
        self.pts.push(value);
        if self.duration_ms(&guard.metadata)? > guard.max_duration_ms {
            return Err("frame_sample_decode_duration_exceeded".into());
        }
        Ok(())
    }
    fn duration_ms(&self, meta: &Metadata) -> Result<u64, String> {
        let (Some(first), Some(last)) = (self.first, self.last) else {
            return Ok(0);
        };
        let span = relative_numerator(last, meta)? - relative_numerator(first, meta)?;
        // One decoded position still consumes a millisecond of finite budget.
        u64::try_from((span + i128::from(meta.den) - 1) / i128::from(meta.den))
            .map(|n| n.max(1))
            .map_err(|_| "frame_sample_decode_duration_overflow".into())
    }
}

async fn read_bounded<R: AsyncRead + Unpin>(mut reader: R, limit: u64) -> Result<Vec<u8>, String> {
    let mut bytes = Vec::new();
    let mut buffer = [0u8; 16 * 1024];
    loop {
        let n = reader
            .read(&mut buffer)
            .await
            .map_err(|_| "frame_sample_process_read_failed")?;
        if n == 0 {
            break;
        }
        if (bytes.len() as u64).saturating_add(n as u64) > limit {
            return Err("frame_sample_process_output_exceeded".into());
        }
        bytes.extend_from_slice(&buffer[..n]);
    }
    Ok(bytes)
}
async fn read_diagnostics<R: AsyncRead + Unpin>(
    mut reader: R,
    spec: &ProcessSpec,
) -> Result<Vec<u8>, String> {
    let mut bytes = Vec::new();
    let mut line = Vec::new();
    let mut observed = Observation::default();
    let mut buffer = [0u8; 16 * 1024];
    loop {
        let n = reader
            .read(&mut buffer)
            .await
            .map_err(|_| "frame_sample_process_read_failed")?;
        if n == 0 {
            break;
        }
        if (bytes.len() as u64).saturating_add(n as u64) > spec.stderr_limit {
            return Err("frame_sample_diagnostics_exceeded".into());
        }
        bytes.extend_from_slice(&buffer[..n]);
        for byte in &buffer[..n] {
            if *byte == b'\n' {
                if let Some(guard) = &spec.guard {
                    observed.push(
                        std::str::from_utf8(&line)
                            .map_err(|_| "frame_sample_diagnostics_invalid")?,
                        guard,
                    )?;
                }
                line.clear();
            } else {
                if line.len() == LINE_LIMIT {
                    return Err("frame_sample_diagnostic_line_exceeded".into());
                }
                line.push(*byte);
            }
        }
    }
    if let Some(guard) = &spec.guard {
        observed.push(
            std::str::from_utf8(&line).map_err(|_| "frame_sample_diagnostics_invalid")?,
            guard,
        )?;
    }
    Ok(bytes)
}
/// Own the writer until all queued writes finish, then close the actual pipe
/// BEFORE waiting for the child. ChildStdin::shutdown is a no-op on Windows
/// Tokio's Blocking writer; retaining that handle can deadlock image2pipe
/// probing against child.wait even after every input byte has been sent.
async fn send_input<W: AsyncWrite + Unpin>(mut writer: W, input: &[u8]) -> Result<(), String> {
    writer
        .write_all(input)
        .await
        .map_err(|_| "frame_sample_process_input_failed")?;
    writer
        .flush()
        .await
        .map_err(|_| "frame_sample_process_input_failed")?;
    drop(writer);
    Ok(())
}
async fn run_process(spec: &ProcessSpec, registry: &Registry) -> Result<ProcessOutput, String> {
    use std::process::Stdio;
    #[cfg(test)]
    let process_started = Instant::now();
    #[cfg(test)]
    eprintln!(
        "frame_sample_process_begin kind={} deadline_ms={}",
        spec.kind,
        spec.deadline.as_millis()
    );
    let mut command = Command::new(&spec.executable);
    command
        .args(&spec.args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    #[cfg(windows)]
    command.creation_flags(0x08000000);
    let mut child = OwnedChild::spawn(registry, Kind::MediaTool, &mut command)
        .await
        .map_err(|_| "frame_sample_process_start_or_containment_failed")?;
    let stdin = child
        .child_mut()
        .stdin
        .take()
        .ok_or("frame_sample_process_pipe_missing")?;
    let stdout = child
        .child_mut()
        .stdout
        .take()
        .ok_or("frame_sample_process_pipe_missing")?;
    let stderr = child
        .child_mut()
        .stderr
        .take()
        .ok_or("frame_sample_process_pipe_missing")?;
    let operation = async {
        let send = send_input(stdin, &spec.stdin);
        let wait = async {
            child
                .child_mut()
                .wait()
                .await
                .map_err(|_| "frame_sample_process_wait_failed".to_owned())
        };
        let (out, err, _, status) = tokio::try_join!(
            read_bounded(stdout, spec.stdout_limit),
            read_diagnostics(stderr, spec),
            send,
            wait
        )?;
        if !status.success() {
            return Err("frame_sample_process_failed".into());
        }
        Ok(ProcessOutput {
            stdout: out,
            stderr: err,
        })
    };
    let result = tokio::time::timeout(spec.deadline, operation)
        .await
        .unwrap_or_else(|_| Err("frame_sample_deadline_exceeded".into()));
    if result.is_err() {
        let _ = child.child_mut().start_kill();
    }
    child
        .settle()
        .await
        .map_err(|_| "frame_sample_process_reap_unknown")?;
    #[cfg(test)]
    eprintln!(
        "frame_sample_process_end kind={} elapsed_ms={} result={:?}",
        spec.kind,
        process_started.elapsed().as_millis(),
        result.as_ref().map(|_| "ok")
    );
    result
}

fn artifact(value: &Value) -> Result<ArtifactRef, String> {
    ArtifactRef::from_json(value).map_err(|_| "frame_sample_artifact_ref_invalid".into())
}
fn read_json(store: &ArtifactStore, value: &Value) -> Result<Value, String> {
    let bytes = store
        .read_bytes(&artifact(value)?, META_LIMIT)
        .map_err(|_| "frame_sample_artifact_unavailable")?;
    serde_json::from_slice(&bytes).map_err(|_| "frame_sample_artifact_json_invalid".into())
}
fn save_json(store: &ArtifactStore, value: &Value) -> Result<Value, String> {
    let bytes = serde_json::to_vec(value).map_err(|_| "frame_sample_artifact_json_invalid")?;
    if bytes.len() as u64 > META_LIMIT {
        return Err("frame_sample_artifact_metadata_exceeded".into());
    }
    store
        .put_bytes(&bytes)
        .map(|r| r.to_json())
        .map_err(|_| "frame_sample_artifact_write_failed".into())
}
fn hash_file(path: &Path) -> Result<String, String> {
    if !path.is_absolute() {
        return Err("frame_sample_tool_path_invalid".into());
    }
    let mut input = std::fs::File::open(path).map_err(|_| "frame_sample_tool_unavailable")?;
    if !input
        .metadata()
        .map_err(|_| "frame_sample_tool_unavailable")?
        .is_file()
    {
        return Err("frame_sample_tool_path_invalid".into());
    }
    let mut hash = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let n = input
            .read(&mut buffer)
            .map_err(|_| "frame_sample_tool_unavailable")?;
        if n == 0 {
            break;
        }
        hash.update(&buffer[..n]);
    }
    Ok(format!("{:x}", hash.finalize()))
}
fn tool_spec(tools: &SampleTools) -> Result<Value, String> {
    if !sha(&json!(tools.ffmpeg_sha256))
        || !sha(&json!(tools.ffprobe_sha256))
        || tools.ffmpeg_version.is_empty()
        || tools.ffprobe_version.is_empty()
        || tools.deadline.is_zero()
        || hash_file(&tools.ffmpeg)? != tools.ffmpeg_sha256
        || hash_file(&tools.ffprobe)? != tools.ffprobe_sha256
    {
        return Err("frame_sample_tool_identity_changed".into());
    }
    Ok(
        json!({"contract":CONTRACT,"ffmpegSha256":tools.ffmpeg_sha256,"ffprobeSha256":tools.ffprobe_sha256,
        "ffmpegVersion":tools.ffmpeg_version,"ffprobeVersion":tools.ffprobe_version,
        "pixelFormat":"rgb24","transform":"full_uncropped_noautorotate","timestampPolicy":"copyts_demux_timebase_passthrough"}),
    )
}

/// Native caller has already proved company/member applicability of this ASR
/// or retained-source receipt. This helper verifies and retains its byte pins;
/// it never applies the old full-AV import requirement to silent video.
pub(crate) fn retain_source_proof(store: &ArtifactStore, proof: &Value) -> Result<Value, String> {
    let source = validate_source_proof(store, proof)?;
    store
        .verify(&source)
        .map_err(|_| "frame_sample_source_unavailable")?;
    save_json(store, proof)
}
fn validate_source_proof(store: &ArtifactStore, proof: &Value) -> Result<ArtifactRef, String> {
    if proof["schemaVersion"] != 1
        || proof["kind"] != "retained_video_source"
        || proof["companyId"].as_str().is_none_or(str::is_empty)
        || proof["member"]["postId"].as_str().is_none_or(str::is_empty)
        || !proof["member"]["connectorBinding"].is_object()
        || proof["asset"]["attachmentIndex"].as_u64().is_none()
        || !sha(&proof["asset"]["attachmentIdentity"])
        || !sha(&proof["asset"]["sourceVersion"])
        || !sha(&proof["asset"]["sourceArtifactSha256"])
        || uint(proof, "sourceDurationMs")? == 0
        || uint(proof, "sourceDurationMs")? > 14_400_000
    {
        return Err("frame_sample_source_proof_invalid".into());
    }
    let source = artifact(&proof["asset"]["sourceArtifactRef"])?;
    if source.bytes == 0
        || source.bytes > 500 * 1024 * 1024
        || source.sha256 != proof["asset"]["sourceArtifactSha256"]
        || proof["verifiedReceipt"]["source"] != source.to_json()
        || proof["verifiedFile"]["sha256"] != source.sha256
        || proof["verifiedFile"]["bytes"] != source.bytes
        || proof["verifiedFile"]["receiptSha256"] != digest(&proof["verifiedReceipt"])
    {
        return Err("frame_sample_source_receipt_changed".into());
    }
    for key in ["asrResultRef", "asrSpecRef"] {
        if !proof[key].is_null() {
            store
                .verify(&artifact(&proof[key])?)
                .map_err(|_| "frame_sample_asr_closure_unavailable")?;
        }
    }
    Ok(source)
}
pub(crate) fn verify_source_proof(store: &ArtifactStore, plan: &Value) -> Result<PathBuf, String> {
    let proof = read_json(store, &plan["sourceProofRef"])?;
    for key in ["companyId", "member", "asset", "sourceDurationMs"] {
        if proof[key] != plan[key] {
            return Err("frame_sample_source_binding_changed".into());
        }
    }
    let source = validate_source_proof(store, &proof)?;
    store
        .path(&source)
        .map_err(|_| "frame_sample_source_unavailable".into())
}
fn source_format(path: &Path) -> Result<&'static str, String> {
    let mut input = std::fs::File::open(path).map_err(|_| "frame_sample_source_unavailable")?;
    let mut header = [0u8; 12];
    let count = input
        .read(&mut header)
        .map_err(|_| "frame_sample_source_unavailable")?;
    if count >= 8 && &header[4..8] == b"ftyp" {
        Ok("mov")
    } else if count >= 4 && header[..4] == [0x1a, 0x45, 0xdf, 0xa3] {
        Ok("matroska")
    } else {
        Err("frame_sample_container_unsupported".into())
    }
}
fn source_args(format: &str) -> Vec<String> {
    let mut args = vec![
        "-protocol_whitelist".into(),
        "file,pipe".into(),
        "-format_whitelist".into(),
        format.into(),
        "-f".into(),
        format.into(),
    ];
    if format == "mov" {
        args.extend([
            "-enable_drefs".into(),
            "0".into(),
            "-use_absolute_path".into(),
            "0".into(),
        ]);
    }
    args
}
fn parse_metadata(value: &Value, expected_duration: u64) -> Result<Metadata, String> {
    let streams = value["streams"]
        .as_array()
        .filter(|s| s.len() == 1)
        .ok_or("frame_sample_video_stream_unsupported")?;
    let stream = &streams[0];
    let ratio = stream["time_base"]
        .as_str()
        .and_then(|s| s.split_once('/'))
        .ok_or("frame_sample_timebase_missing")?;
    let num = ratio
        .0
        .parse::<u64>()
        .ok()
        .filter(|n| *n > 0 && *n <= i32::MAX as u64)
        .ok_or("frame_sample_timebase_invalid")?;
    let den = ratio
        .1
        .parse::<u64>()
        .ok()
        .filter(|n| *n > 0 && *n <= i32::MAX as u64)
        .ok_or("frame_sample_timebase_invalid")?;
    let start_pts = stream["start_pts"]
        .as_i64()
        .or_else(|| stream["start_pts"].as_str().and_then(|s| s.parse().ok()))
        .ok_or("frame_sample_video_start_missing")?;
    let width = uint(stream, "width")?;
    let height = uint(stream, "height")?;
    if width == 0 || height == 0 || width.checked_mul(height).is_none_or(|n| n > 64_000_000) {
        return Err("frame_sample_dimensions_invalid".into());
    }
    let ticks = stream["duration_ts"]
        .as_i64()
        .or_else(|| stream["duration_ts"].as_str().and_then(|s| s.parse().ok()));
    let duration_ms = if let Some(ticks) = ticks.filter(|n| *n > 0) {
        u64::try_from(i128::from(ticks) * i128::from(num) * 1000 / i128::from(den))
            .map_err(|_| "frame_sample_duration_invalid")?
    } else if start_pts == 0 {
        let seconds = value["format"]["duration"]
            .as_str()
            .and_then(|s| s.parse::<f64>().ok())
            .filter(|s| s.is_finite() && *s > 0.0 && *s <= 14_400.0)
            .ok_or("frame_sample_duration_missing")?;
        (seconds * 1000.0).round() as u64
    } else {
        return Err("frame_sample_video_duration_missing".into());
    };
    if duration_ms == 0 || duration_ms > 14_400_000 || duration_ms.abs_diff(expected_duration) > 250
    {
        return Err("frame_sample_duration_changed".into());
    }
    Ok(Metadata {
        index: uint(stream, "index")?,
        width,
        height,
        num,
        den,
        start_pts,
        duration_ms,
    })
}
fn relative_numerator(pts: i64, meta: &Metadata) -> Result<i128, String> {
    (i128::from(pts) - i128::from(meta.start_pts))
        .checked_mul(i128::from(meta.num))
        .and_then(|n| n.checked_mul(1000))
        .ok_or("frame_sample_time_overflow".into())
}
fn absolute_seconds(meta: &Metadata, offset_ms: u64) -> Result<String, String> {
    let n = i128::from(meta.start_pts)
        .checked_mul(i128::from(meta.num))
        .and_then(|n| n.checked_mul(1_000_000_000))
        .map(|n| n / i128::from(meta.den))
        .and_then(|n| n.checked_add(i128::from(offset_ms) * 1_000_000))
        .ok_or("frame_sample_time_overflow")?;
    let magnitude = n.unsigned_abs();
    Ok(format!(
        "{}{}.{:09}",
        if n < 0 { "-" } else { "" },
        magnitude / 1_000_000_000,
        magnitude % 1_000_000_000
    ))
}
fn deadline(start: Instant, total: Duration) -> Result<Duration, String> {
    total
        .checked_sub(start.elapsed())
        .filter(|d| !d.is_zero())
        .ok_or("frame_sample_deadline_exceeded".into())
}
async fn execute(process: &dyn SampleProcess, spec: &ProcessSpec) -> Result<ProcessOutput, String> {
    let out = tokio::time::timeout(spec.deadline, process.execute(spec))
        .await
        .map_err(|_| "frame_sample_deadline_exceeded")??;
    if out.stdout.len() as u64 > spec.stdout_limit || out.stderr.len() as u64 > spec.stderr_limit {
        return Err("frame_sample_process_output_exceeded".into());
    }
    if out
        .stderr
        .split(|b| *b == b'\n')
        .any(|line| line.len() > LINE_LIMIT)
    {
        return Err("frame_sample_diagnostic_line_exceeded".into());
    }
    Ok(out)
}
fn frame_evidence(
    out: &ProcessOutput,
    guard: &DecodeGuard,
) -> Result<(Option<i64>, Observation), String> {
    let text = std::str::from_utf8(&out.stderr).map_err(|_| "frame_sample_diagnostics_invalid")?;
    let mut observed = Observation::default();
    let mut tb = None;
    let mut dimensions = None;
    let mut frame = None;
    for line in text.lines() {
        observed.push(line, guard)?;
        if let Some(s) = line.strip_prefix("#tb 0:") {
            if tb.replace(s.trim().to_owned()).is_some() || frame.is_some() {
                return Err("frame_sample_framehash_header_duplicate".into());
            }
        }
        if let Some(s) = line.strip_prefix("#dimensions 0:") {
            if dimensions.replace(s.trim().to_owned()).is_some() || frame.is_some() {
                return Err("frame_sample_framehash_header_duplicate".into());
            }
        }
        let parts: Vec<_> = line.split(',').map(str::trim).collect();
        if parts.len() == 6 && parts[0] == "0" {
            if frame.is_some() {
                return Err("frame_sample_extra_output_frame".into());
            }
            let pts = parts[2]
                .parse::<i64>()
                .map_err(|_| "frame_sample_framehash_pts_invalid")?;
            let size = parts[4]
                .parse::<u64>()
                .map_err(|_| "frame_sample_framehash_size_invalid")?;
            let hash = format!("{:x}", Sha256::digest(&out.stdout));
            if size != out.stdout.len() as u64 || parts[5] != hash {
                return Err("frame_sample_pixel_hash_changed".into());
            }
            frame = Some(pts);
        }
    }
    if out.stdout.is_empty() && frame.is_none() {
        return Ok((None, observed));
    }
    let pts = frame.ok_or("frame_sample_framehash_missing")?;
    let meta = &guard.metadata;
    if tb != Some(format!("{}/{}", meta.num, meta.den))
        || dimensions != Some(format!("{}x{}", meta.width, meta.height))
        || out.stdout.len() as u64 != meta.width * meta.height * 3
        || !observed.pts.contains(&pts)
    {
        return Err("frame_sample_source_pts_unproven".into());
    }
    let offset = relative_numerator(pts, meta)?;
    if pts < meta.start_pts
        || offset < i128::from(guard.target_ms) * i128::from(meta.den)
        || offset >= i128::from(guard.end_ms) * i128::from(meta.den)
    {
        return Err("frame_sample_actual_outside_window".into());
    }
    Ok((Some(pts), observed))
}
fn png_dimensions(bytes: &[u8]) -> Result<(u64, u64), String> {
    if bytes.len() < 33 || &bytes[..8] != b"\x89PNG\r\n\x1a\n" || &bytes[12..16] != b"IHDR" {
        return Err("frame_sample_png_invalid".into());
    }
    Ok((
        u32::from_be_bytes(bytes[16..20].try_into().unwrap()) as u64,
        u32::from_be_bytes(bytes[20..24].try_into().unwrap()) as u64,
    ))
}

pub(crate) async fn decode_sample(
    store: &ArtifactStore,
    plan: &Value,
    tools: &SampleTools,
    registry: &Registry,
) -> Result<Value, String> {
    decode_sample_with(store, plan, tools, &LocalProcess { registry }).await
}
pub(crate) async fn decode_sample_with(
    store: &ArtifactStore,
    plan: &Value,
    tools: &SampleTools,
    process: &dyn SampleProcess,
) -> Result<Value, String> {
    let started = Instant::now();
    validate_plan(plan)?;
    let total = tools
        .deadline
        .min(Duration::from_millis(uint(&plan["profile"], "deadlineMs")?));
    let spec = tool_spec(tools)?;
    let source = verify_source_proof(store, plan)?;
    let format = source_format(&source)?;
    let mut probe_args: Vec<String> = ["-v", "error"].iter().map(|s| (*s).into()).collect();
    probe_args.extend(source_args(format));
    probe_args.extend(
        [
            "-select_streams",
            "v:0",
            "-show_entries",
            "stream=index,width,height,time_base,start_pts,duration_ts:format=duration,start_time",
            "-of",
            "json",
        ]
        .iter()
        .map(|s| (*s).into()),
    );
    probe_args.push(source.display().to_string());
    let probe_out = execute(
        process,
        &ProcessSpec {
            kind: "probe",
            executable: tools.ffprobe.clone(),
            args: probe_args,
            stdin: vec![],
            stdout_limit: META_LIMIT,
            stderr_limit: META_LIMIT,
            deadline: deadline(started, total)?,
            guard: None,
        },
    )
    .await?;
    let probe: Value =
        serde_json::from_slice(&probe_out.stdout).map_err(|_| "frame_sample_probe_invalid")?;
    let meta = parse_metadata(&probe, uint(plan, "sourceDurationMs")?)?;
    let pixels = meta.width * meta.height;
    let raw_bytes = pixels * 3;
    let targets = plan["targets"]
        .as_array()
        .ok_or("frame_sample_targets_invalid")?;
    let profile = &plan["profile"];
    let transport = &plan["transportLimits"];
    let base = &plan["baseUsage"];
    let sampled_pixels = pixels
        .checked_mul(targets.len() as u64)
        .ok_or("frame_sample_pixels_overflow")?;
    if sampled_pixels > uint(profile, "maxTotalPixels")?
        || uint(base, "pixels")?.saturating_add(sampled_pixels) > uint(transport, "maxPixels")?
    {
        return Err("frame_sample_pixel_budget_exhausted".into());
    }
    let metadata_bytes = plan.to_string().len() as u64
        + probe.to_string().len() as u64
        + spec.to_string().len() as u64;
    let minimum_artifacts = raw_bytes
        .checked_add(33)
        .and_then(|n| n.checked_mul(targets.len() as u64))
        .and_then(|n| n.checked_add(metadata_bytes))
        .ok_or("frame_sample_bytes_overflow")?;
    if minimum_artifacts > uint(profile, "maxArtifactBytes")? {
        return Err("frame_sample_artifact_budget_exhausted".into());
    }
    let plan_ref = save_json(store, plan)?;
    let probe_ref = save_json(store, &probe)?;
    let spec_ref = save_json(store, &spec)?;
    let mut artifact_bytes =
        uint(&plan_ref, "bytes")? + uint(&probe_ref, "bytes")? + uint(&spec_ref, "bytes")?;
    let mut image_bytes = 0u64;
    let mut decoded_frames = 0u64;
    let mut decoded_ms = 0u64;
    let mut frames = Vec::new();
    let mut omissions = Vec::new();
    for target in targets {
        let target_ms = uint(target, "requestedTimestampMs")?;
        let end_ms = uint(target, "windowEndMs")?;
        let guard = DecodeGuard {
            metadata: meta.clone(),
            target_ms,
            end_ms,
            max_preroll_ms: uint(profile, "maxPrerollMs")?,
            max_frames: uint(profile, "maxDecodedFrames")?
                .checked_sub(decoded_frames)
                .ok_or("frame_sample_decode_count_exceeded")?,
            max_duration_ms: uint(profile, "maxDecodedDurationMs")?
                .checked_sub(decoded_ms)
                .ok_or("frame_sample_decode_duration_exceeded")?,
        };
        if guard.max_frames == 0 || guard.max_duration_ms == 0 {
            return Err("frame_sample_decode_budget_exhausted".into());
        }
        let absolute = absolute_seconds(&meta, target_ms)?;
        let mut args: Vec<String> = [
            "-nostdin",
            "-hide_banner",
            "-loglevel",
            "info",
            "-threads",
            "1",
            "-copyts",
            "-seek_timestamp",
            "1",
            "-noaccurate_seek",
            "-noautorotate",
            "-ss",
        ]
        .iter()
        .map(|s| (*s).into())
        .collect();
        args.push(absolute.clone());
        args.extend(source_args(format));
        args.extend([
            "-t".into(),
            format!("{:.3}", (end_ms - target_ms) as f64 / 1000.0),
            "-i".into(),
            source.display().to_string(),
            "-map".into(),
            format!("0:{}", meta.index),
            "-an".into(),
            "-sn".into(),
            "-dn".into(),
            "-vf".into(),
            format!("format=rgb24,showinfo=checksum=0,select=gte(t\\,{absolute})"),
            "-frames:v".into(),
            "1".into(),
            "-fps_mode".into(),
            "passthrough".into(),
            "-enc_time_base".into(),
            "demux".into(),
            "-c:v".into(),
            "rawvideo".into(),
            "-pix_fmt".into(),
            "rgb24".into(),
            "-avoid_negative_ts".into(),
            "disabled".into(),
            "-f".into(),
            "tee".into(),
            "[f=rawvideo]pipe:1|[f=framehash:hash=sha256]pipe:2".into(),
        ]);
        let out = execute(
            process,
            &ProcessSpec {
                kind: "sample",
                executable: tools.ffmpeg.clone(),
                args,
                stdin: vec![],
                stdout_limit: raw_bytes,
                stderr_limit: DIAGNOSTIC_LIMIT,
                deadline: deadline(started, total)?,
                guard: Some(guard.clone()),
            },
        )
        .await?;
        let (actual, observed) = frame_evidence(&out, &guard)?;
        decoded_frames = decoded_frames
            .checked_add(observed.count)
            .ok_or("frame_sample_decode_count_overflow")?;
        decoded_ms = decoded_ms
            .checked_add(observed.duration_ms(&meta)?)
            .ok_or("frame_sample_decode_duration_overflow")?;
        let Some(pts) = actual else {
            omissions.push(json!({"targetIndex":target["targetIndex"],"reasonCode":"no_frame_in_bounded_window"}));
            continue;
        };
        let encode_args = vec![
            "-hide_banner".into(),
            "-loglevel".into(),
            "error".into(),
            "-threads".into(),
            "1".into(),
            "-f".into(),
            "rawvideo".into(),
            "-pix_fmt".into(),
            "rgb24".into(),
            "-video_size".into(),
            format!("{}x{}", meta.width, meta.height),
            "-i".into(),
            "pipe:0".into(),
            "-frames:v".into(),
            "1".into(),
            "-c:v".into(),
            "png".into(),
            "-f".into(),
            "image2pipe".into(),
            "pipe:1".into(),
        ];
        let png = execute(
            process,
            &ProcessSpec {
                kind: "encode_png",
                executable: tools.ffmpeg.clone(),
                args: encode_args,
                stdin: out.stdout.clone(),
                stdout_limit: uint(profile, "maxImageBytes")?,
                stderr_limit: META_LIMIT,
                deadline: deadline(started, total)?,
                guard: None,
            },
        )
        .await?
        .stdout;
        if png_dimensions(&png)? != (meta.width, meta.height) {
            return Err("frame_sample_png_dimensions_changed".into());
        }
        let roundtrip_args = [
            "-hide_banner",
            "-loglevel",
            "error",
            "-threads",
            "1",
            "-f",
            "image2pipe",
            "-c:v",
            "png",
            "-i",
            "pipe:0",
            "-frames:v",
            "1",
            "-f",
            "rawvideo",
            "-pix_fmt",
            "rgb24",
            "pipe:1",
        ]
        .iter()
        .map(|s| (*s).into())
        .collect();
        let verified = execute(
            process,
            &ProcessSpec {
                kind: "verify_png",
                executable: tools.ffmpeg.clone(),
                args: roundtrip_args,
                stdin: png.clone(),
                stdout_limit: raw_bytes,
                stderr_limit: META_LIMIT,
                deadline: deadline(started, total)?,
                guard: None,
            },
        )
        .await?;
        if verified.stdout != out.stdout {
            return Err("frame_sample_png_pixels_changed".into());
        }
        image_bytes = image_bytes
            .checked_add(png.len() as u64)
            .ok_or("frame_sample_bytes_overflow")?;
        artifact_bytes = artifact_bytes
            .checked_add(raw_bytes)
            .and_then(|n| n.checked_add(png.len() as u64))
            .ok_or("frame_sample_bytes_overflow")?;
        if uint(base, "imageBytes")?.saturating_add(image_bytes) > uint(transport, "maxBytes")?
            || artifact_bytes > uint(profile, "maxArtifactBytes")?
        {
            return Err("frame_sample_byte_budget_exhausted".into());
        }
        let rgb_ref = store
            .put_bytes(&out.stdout)
            .map_err(|_| "frame_sample_artifact_write_failed")?;
        let png_ref = store
            .put_bytes(&png)
            .map_err(|_| "frame_sample_artifact_write_failed")?;
        let time = source_time(pts, meta.num, meta.den, meta.start_pts)?;
        frames.push(json!({"targetIndex":target["targetIndex"],"requestedTimestampMs":target_ms,"requestedWindowEndMs":end_ms,
            "actualPts":pts,"pts":time["pts"],"timeBase":{"num":meta.num,"den":meta.den},
            "sourceTimestamp":time["sourceTimestamp"],"sourceTimestampMs":time["sourceTimestampMs"],
            "timelineOffsetMs":time["timelineOffsetMs"],"timestampMs":time["timestampMs"],"timestampBasis":time["timestampBasis"],
            "videoStartPts":time["videoStartPts"],"millisecondRounding":time["millisecondRounding"],"frameIndex":null,
            "artifact":png_ref.to_json(),"sha256":png_ref.sha256,"mime":"image/png","width":meta.width,"height":meta.height,
            "pixelArtifact":rgb_ref.to_json(),"pixelSha256":rgb_ref.sha256,"pixelFormat":"rgb24","fullFrame":true,"cropped":false}));
    }
    verify_source_proof(store, plan)?;
    tool_spec(tools)?;
    deadline(started, total)?;
    let mut body = json!({"schemaVersion":1,"kind":"bounded_video_sample_result","decoderContractVersion":CONTRACT,
        "needId":plan["needId"],"needSha256":plan["needSha256"],"companyId":plan["companyId"],"member":plan["member"],"asset":plan["asset"],
        "requestedTimeOrIntent":plan["requestedTimeOrIntent"],"sourceProofRef":plan["sourceProofRef"],"planSha256":plan["planSha256"],
        "planRef":plan_ref,"probeRef":probe_ref,"specRef":spec_ref,"specSha256":digest(&spec),"toolVersion":tools.ffmpeg_version,
        "profile":profile,"status":if omissions.is_empty(){"complete"}else{"partial"},"frames":frames,
        "coverage":{"exhaustive":false,"sampledTargets":targets.len(),"missingTargets":omissions,"durationMs":meta.duration_ms,"shortEventsMayBeMissed":true},
        "used":{"imageCount":frames.len(),"imageBytes":image_bytes,"pixels":pixels * frames.len() as u64,"decodedFrames":decoded_frames,"decodedDurationMs":decoded_ms,"artifactBytes":0,"elapsedMs":started.elapsed().as_millis() as u64}});
    // Fixed-width decimal reserve keeps the receipt itself inside the finite CAS
    // byte budget, including the self-count field without a self hash.
    let reserve = body.to_string().len() as u64 + 32;
    if artifact_bytes.saturating_add(reserve) > uint(profile, "maxArtifactBytes")? {
        return Err("frame_sample_artifact_budget_exhausted".into());
    }
    body["used"]["artifactBytes"] = json!(artifact_bytes + reserve);
    let result_ref = save_json(store, &body)?;
    let mut result = body;
    result["resultSha256"] = result_ref["sha256"].clone();
    result["resultRef"] = result_ref;
    Ok(result)
}

/// Cold reuse requires the same immutable plan, source/member bindings and
/// decoder pins. It performs CAS verification only; no decoder/model dispatch.
pub(crate) fn verify_sample_result(
    store: &ArtifactStore,
    plan: &Value,
    result: &Value,
    tools: &SampleTools,
) -> Result<(), String> {
    validate_plan(plan)?;
    verify_source_proof(store, plan)?;
    let mut body = result.clone();
    let object = body.as_object_mut().ok_or("frame_sample_result_invalid")?;
    object.remove("resultRef");
    object.remove("resultSha256");
    if read_json(store, &result["resultRef"])? != body
        || result["resultSha256"] != digest(&body)
        || result["resultRef"]["sha256"] != result["resultSha256"]
        || result["decoderContractVersion"] != CONTRACT
        || result["kind"] != "bounded_video_sample_result"
        || result["schemaVersion"] != 1
        || read_json(store, &result["planRef"])? != *plan
        || read_json(store, &result["specRef"])? != tool_spec(tools)?
    {
        return Err("frame_sample_result_changed".into());
    }
    for key in [
        "needId",
        "needSha256",
        "companyId",
        "member",
        "asset",
        "sourceProofRef",
        "requestedTimeOrIntent",
        "profile",
        "planSha256",
    ] {
        if result[key] != plan[key] {
            return Err("frame_sample_result_binding_changed".into());
        }
    }
    let probe = read_json(store, &result["probeRef"])?;
    let meta = parse_metadata(&probe, uint(plan, "sourceDurationMs")?)?;
    let frames = result["frames"]
        .as_array()
        .ok_or("frame_sample_result_frames_invalid")?;
    if frames.len() > uint(&plan["profile"], "maxFrames")? as usize {
        return Err("frame_sample_result_count_invalid".into());
    }
    if result["specSha256"] != digest(&read_json(store, &result["specRef"])?)
        || result["toolVersion"] != tools.ffmpeg_version
        || result["coverage"]["exhaustive"] != false
        || result["coverage"]["durationMs"] != meta.duration_ms
        || result["coverage"]["shortEventsMayBeMissed"] != true
    {
        return Err("frame_sample_result_coverage_invalid".into());
    }
    let mut image_bytes = 0u64;
    let mut retained_bytes = uint(&result["resultRef"], "bytes")?
        + uint(&result["planRef"], "bytes")?
        + uint(&result["probeRef"], "bytes")?
        + uint(&result["specRef"], "bytes")?;
    let mut seen = std::collections::BTreeSet::new();
    for frame in frames {
        let index = uint(frame, "targetIndex")?;
        if !seen.insert(index) {
            return Err("frame_sample_result_target_duplicate".into());
        }
        let target = plan["targets"]
            .as_array()
            .and_then(|r| r.get(index as usize))
            .ok_or("frame_sample_result_target_invalid")?;
        let pts = frame["actualPts"]
            .as_i64()
            .ok_or("frame_sample_result_pts_invalid")?;
        let time = source_time(pts, meta.num, meta.den, meta.start_pts)?;
        if frame["requestedTimestampMs"] != target["requestedTimestampMs"]
            || frame["requestedWindowEndMs"] != target["windowEndMs"]
            || frame["timeBase"] != json!({"num":meta.num,"den":meta.den})
            || frame["width"] != meta.width
            || frame["height"] != meta.height
            || frame["fullFrame"] != true
            || frame["cropped"] != false
            || !frame["frameIndex"].is_null()
            || frame["mime"] != "image/png"
            || frame["pixelFormat"] != "rgb24"
        {
            return Err("frame_sample_result_frame_changed".into());
        }
        for key in [
            "pts",
            "sourceTimestamp",
            "sourceTimestampMs",
            "timelineOffsetMs",
            "timestampMs",
            "timestampBasis",
            "videoStartPts",
            "millisecondRounding",
        ] {
            if frame[key] != time[key] {
                return Err("frame_sample_result_time_changed".into());
            }
        }
        let offset = relative_numerator(pts, &meta)?;
        if offset < i128::from(uint(target, "requestedTimestampMs")?) * i128::from(meta.den)
            || offset >= i128::from(uint(target, "windowEndMs")?) * i128::from(meta.den)
        {
            return Err("frame_sample_result_window_changed".into());
        }
        let png_ref = artifact(&frame["artifact"])?;
        let rgb_ref = artifact(&frame["pixelArtifact"])?;
        if frame["sha256"] != png_ref.sha256
            || frame["pixelSha256"] != rgb_ref.sha256
            || rgb_ref.bytes != meta.width * meta.height * 3
        {
            return Err("frame_sample_result_pixels_changed".into());
        }
        let png = store
            .read_bytes(&png_ref, uint(&plan["profile"], "maxImageBytes")?)
            .map_err(|_| "frame_sample_result_image_unavailable")?;
        if png_dimensions(&png)? != (meta.width, meta.height) {
            return Err("frame_sample_result_image_changed".into());
        }
        store
            .verify(&rgb_ref)
            .map_err(|_| "frame_sample_result_pixels_unavailable")?;
        image_bytes = image_bytes
            .checked_add(png_ref.bytes)
            .ok_or("frame_sample_result_bytes_overflow")?;
        retained_bytes = retained_bytes
            .checked_add(png_ref.bytes)
            .and_then(|n| n.checked_add(rgb_ref.bytes))
            .ok_or("frame_sample_result_bytes_overflow")?;
    }
    let omissions = result["coverage"]["missingTargets"]
        .as_array()
        .ok_or("frame_sample_result_coverage_invalid")?;
    for omission in omissions {
        let index = uint(omission, "targetIndex")?;
        if omission["reasonCode"] != "no_frame_in_bounded_window"
            || !seen.insert(index)
            || index >= plan["targets"].as_array().unwrap().len() as u64
        {
            return Err("frame_sample_result_coverage_invalid".into());
        }
    }
    let count = plan["targets"].as_array().unwrap().len() as u64;
    let pixels = meta.width * meta.height * frames.len() as u64;
    if seen.len() as u64 != count
        || result["coverage"]["sampledTargets"] != count
        || result["status"]
            != if omissions.is_empty() {
                "complete"
            } else {
                "partial"
            }
        || result["used"]["imageCount"] != frames.len() as u64
        || result["used"]["imageBytes"] != image_bytes
        || result["used"]["pixels"] != pixels
        || uint(&result["used"], "decodedFrames")? < frames.len() as u64
        || uint(&result["used"], "decodedFrames")? > uint(&plan["profile"], "maxDecodedFrames")?
        || uint(&result["used"], "decodedDurationMs")?
            > uint(&plan["profile"], "maxDecodedDurationMs")?
        || pixels > uint(&plan["profile"], "maxTotalPixels")?
        || uint(&plan["baseUsage"], "imageCount")?.saturating_add(frames.len() as u64)
            > uint(&plan["transportLimits"], "maxImages")?
        || uint(&plan["baseUsage"], "imageBytes")?.saturating_add(image_bytes)
            > uint(&plan["transportLimits"], "maxBytes")?
        || uint(&plan["baseUsage"], "pixels")?.saturating_add(pixels)
            > uint(&plan["transportLimits"], "maxPixels")?
        || uint(&result["used"], "artifactBytes")? < retained_bytes
        || uint(&result["used"], "artifactBytes")? > uint(&plan["profile"], "maxArtifactBytes")?
    {
        return Err("frame_sample_result_budget_invalid".into());
    }
    Ok(())
}

#[cfg(test)]
#[path = "media_frame_sample_decode_tests.rs"]
mod tests;


/// Test-only local source generation for the admitted floor corpus. This type
/// cannot be constructed from a caller's JSON checkpoint or decoder result.
#[cfg(test)]
pub(crate) struct FixtureGeneratedSource {
    pin: Value,
    source: ArtifactRef,
    observation: Value,
    observation_ref: Value,
}
#[cfg(test)]
fn fixture_source_metadata(probe: &Value) -> Result<Metadata, String> {
    let metadata = parse_metadata(probe, 6000)?;
    let stream = &probe["streams"][0];
    if stream["codec_type"] != "video" || stream["codec_name"] != "mpeg4"
        || metadata.width != 64 || metadata.height != 48 || metadata.start_pts != 0
    {
        return Err("floor_fixture_source_stream_changed".into());
    }
    Ok(metadata)
}
#[cfg(test)]
impl FixtureGeneratedSource {
    pub(crate) fn verify(&self, store: &ArtifactStore, pin: &Value, tools: &SampleTools) -> Result<Value, String> {
        crate::media_speech_assets::validate_shape(pin)?;
        if self.pin != *pin || pin["companyId"] != "BAW Russia"
            || self.source.bytes == 0 || self.source.bytes > 1024 * 1024
            || self.observation["tools"] != tool_spec(tools)?
            || self.observation["generation"]["executable"] != json!(tools.ffmpeg)
            || self.observation["probe"]["executable"] != json!(tools.ffprobe)
            || self.observation["kind"] != "native-generated-floor-video-source"
            || self.observation["classification"] != "SYNTHETIC-NATIVE-CORPUS"
            || self.observation["sourceOrigin"] != "genuine-local-ffmpeg-testsrc"
            || self.observation["hasVideo"] != true || self.observation["hasAudio"] != false
            || self.observation["assetPin"] != *pin
            || self.observation["source"] != self.source.to_json()
            || read_json(store, &self.observation_ref)? != self.observation
        {
            return Err("floor_fixture_generated_source_changed".into());
        }
        store.verify(&self.source).map_err(|_| "floor_fixture_source_cas_changed")?;
        let raw = self.observation["probe"]["stdout"].as_str().ok_or("floor_fixture_probe_missing")?;
        let probe: Value = serde_json::from_str(raw).map_err(|_| "floor_fixture_probe_invalid")?;
        if probe != self.observation["probe"]["value"] {
            return Err("floor_fixture_probe_changed".into());
        }
        let metadata = fixture_source_metadata(&probe)?;
        if self.observation["durationMs"] != metadata.duration_ms {
            return Err("floor_fixture_duration_changed".into());
        }
        let mut observed = self.observation.clone();
        observed["observationArtifact"] = self.observation_ref.clone();
        Ok(observed)
    }
}
#[cfg(test)]
pub(crate) async fn fixture_generate_pinned_source(
    store: &ArtifactStore, pin: &Value, tools: &SampleTools, registry: &Registry,
) -> Result<FixtureGeneratedSource, String> {
    crate::media_speech_assets::validate_shape(pin)?;
    if pin["companyId"] != "BAW Russia" {
        return Err("floor_fixture_source_company_changed".into());
    }
    let tools_pin = tool_spec(tools)?;
    let started = Instant::now();
    let total = tools.deadline.min(Duration::from_secs(30));
    let scratch = tempfile::Builder::new().prefix("floor-native-source-")
        .tempdir_in(store.root()).map_err(|_| "floor_fixture_scratch_unavailable")?;
    let path = scratch.path().join("generated.mp4");
    let args: Vec<String> = [
        "-nostdin", "-hide_banner", "-loglevel", "error", "-f", "lavfi", "-i",
        "testsrc=size=64x48:rate=10:duration=6", "-c:v", "mpeg4", "-threads", "1",
        "-g", "5", "-an", "-movflags", "+faststart", "-n",
    ].iter().map(|s| (*s).into()).chain([path.display().to_string()]).collect();
    let process = LocalProcess { registry };
    let generation_spec = ProcessSpec {
        kind: "floor_fixture_source", executable: tools.ffmpeg.clone(), args,
        stdin: vec![], stdout_limit: 64 * 1024, stderr_limit: 64 * 1024,
        deadline: deadline(started, total)?.min(Duration::from_secs(20)), guard: None,
    };
    let generated = execute(&process, &generation_spec).await?;
    tool_spec(tools)?;
    let file = std::fs::symlink_metadata(&path).map_err(|_| "floor_fixture_source_missing")?;
    if !file.is_file() || file.file_type().is_symlink() || file.len() == 0 || file.len() > 1024 * 1024 {
        return Err("floor_fixture_source_file_invalid".into());
    }
    #[cfg(windows)] {
        use std::os::windows::fs::MetadataExt;
        if file.file_attributes() & 0x400 != 0 { return Err("floor_fixture_source_file_invalid".into()); }
    }
    let source = store.put_file(&path).map_err(|_| "floor_fixture_source_retention_failed")?;
    store.verify(&source).map_err(|_| "floor_fixture_source_cas_changed")?;
    let retained = store.path(&source).map_err(|_| "floor_fixture_source_unavailable")?;
    let mut probe_args = vec!["-v".into(), "error".into()];
    probe_args.extend(source_args(source_format(&retained)?));
    probe_args.extend([
        "-show_entries", "stream=index,codec_type,codec_name,width,height,time_base,start_pts,duration_ts:format=duration,start_time",
        "-of", "json",
    ].iter().map(|s| (*s).into()));
    probe_args.push(retained.display().to_string());
    let probe_spec = ProcessSpec {
        kind: "floor_fixture_probe", executable: tools.ffprobe.clone(), args: probe_args,
        stdin: vec![], stdout_limit: 64 * 1024, stderr_limit: 64 * 1024,
        deadline: deadline(started, total)?.min(Duration::from_secs(10)), guard: None,
    };
    let probed = execute(&process, &probe_spec).await?;
    let probe: Value = serde_json::from_slice(&probed.stdout).map_err(|_| "floor_fixture_probe_invalid")?;
    let metadata = fixture_source_metadata(&probe)?;
    if tool_spec(tools)? != tools_pin { return Err("floor_fixture_source_tools_changed".into()); }
    store.verify(&source).map_err(|_| "floor_fixture_source_cas_changed")?;
    let utf8 = |bytes: Vec<u8>| String::from_utf8(bytes).map_err(|_| "floor_fixture_process_output_invalid".to_owned());
    let observation = json!({
        "kind":"native-generated-floor-video-source", "classification":"SYNTHETIC-NATIVE-CORPUS",
        "sourceOrigin":"genuine-local-ffmpeg-testsrc", "assetPin":pin, "source":source.to_json(),
        "durationMs":metadata.duration_ms, "hasVideo":true, "hasAudio":false,
        "tools":tools_pin,
        "generation":{"executable":generation_spec.executable,"args":generation_spec.args,
            "deadlineMs":generation_spec.deadline.as_millis(),"stdout":utf8(generated.stdout)?,
            "stderr":utf8(generated.stderr)?,"status":"passed","cessation":"owned-child-settled"},
        "probe":{"executable":probe_spec.executable,"args":probe_spec.args,
            "deadlineMs":probe_spec.deadline.as_millis(),"stdout":utf8(probed.stdout)?,
            "stderr":utf8(probed.stderr)?,"value":probe,"status":"passed","cessation":"owned-child-settled"},
        "providerEffects":0,"modelEffects":0,"asrEffects":0,
    });
    let observation_ref = save_json(store, &observation)?;
    let source = FixtureGeneratedSource { pin: pin.clone(), source, observation, observation_ref };
    source.verify(store, pin, tools)?;
    deadline(started, total)?;
    Ok(source)
}
#[cfg(test)]
mod floor_generated_source_contract_tests {
    use super::*;
    fn probe() -> Value { json!({"streams":[{"index":0,"codec_type":"video","codec_name":"mpeg4",
        "width":64,"height":48,"time_base":"1/10240","start_pts":0,"duration_ts":61440}],
        "format":{"duration":"6.000000"}}) }
    #[test]
    fn floor_generated_source_probe_rejects_audio_ambiguous_streams_duration_and_geometry() {
        assert_eq!(fixture_source_metadata(&probe()).unwrap().duration_ms,6000);
        for case in ["audio","extra","duration","width","codec","start"] {
            let mut value=probe();
            match case {
                "audio"=>value["streams"][0]["codec_type"]=json!("audio"),
                "extra"=>value["streams"].as_array_mut().unwrap().push(json!({"codec_type":"audio"})),
                "duration"=>value["streams"][0]["duration_ts"]=json!(100),
                "width"=>value["streams"][0]["width"]=json!(65),
                "codec"=>value["streams"][0]["codec_name"]=json!("h264"),
                _=>value["streams"][0]["start_pts"]=json!(1),
            }
            assert!(fixture_source_metadata(&value).is_err(),"{case}");
        }
    }
}
