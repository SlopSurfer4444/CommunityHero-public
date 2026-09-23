//! Offline FFmpeg RGB24 frame inventory and exact-index PNG extraction.
//! A caller supplies the admitted source, absolute FFmpeg binary and private
//! work directory. Inventory and index are immutable CAS objects; a failed
//! decode never publishes a partial inventory descriptor.

use crate::media_artifacts::{ArtifactRef, ArtifactStore};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::fs::{self, File};
use std::io::Read;
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;

const INVENTORY_TIMEOUT: Duration = Duration::from_secs(4 * 60 * 60);
const EXTRACT_TIMEOUT: Duration = Duration::from_secs(4 * 60 * 60);
const PNG_TIMEOUT: Duration = Duration::from_secs(120);
const MAX_FRAMEHASH_LINE: usize = 4096;
const MAX_INDEX_BYTES: usize = 16 * 1024 * 1024;
const MAX_FRAME_BYTES: u64 = 256 * 1024 * 1024;
const MAX_FRAME_PIXELS: u64 = 64_000_000;
const MAX_PNG_BYTES: u64 = 32 * 1024 * 1024;
const MAX_PNG_CHUNK_BYTES: u64 = 128 * 1024 * 1024;

pub(crate) const DECODER_CONTRACT: &str =
    "communityhero.media.rgb24.full-frame.ffmpeg8.1.framehash-sha256.passthrough-demux.v2";

/// Returns the CAS reference of a descriptor. The descriptor's `source`,
/// `inventory`, and `index` fields are exact ArtifactRef JSON values.
pub(crate) async fn inventory(
    ffmpeg: &Path,
    source_path: &Path,
    source_ref: &Value,
    identity: &Value,
    store: &ArtifactStore,
) -> Result<Value, String> {
    check_tool_and_source(ffmpeg, source_path)?;
    let source_ref = ArtifactRef::from_json(source_ref).map_err(|_| "visual_source_ref_invalid")?;
    let (_, _, media_sha, _) = parse_identity(identity)?;
    if source_ref.sha256 != media_sha {
        return Err("visual_source_identity_mismatch".into());
    }
    store
        .verify(&source_ref)
        .map_err(|_| "visual_source_artifact_invalid")?;
    verify_source_file(source_path, &source_ref)?;
    let mut writer = store
        .begin_jsonl()
        .map_err(|_| "visual_inventory_store_failed")?;
    let mut command = base_command(ffmpeg);
    command
        .args([
            "-i",
            source_path.to_str().ok_or("visual_source_path_invalid")?,
            "-map",
            "0:v:0",
            "-vf",
            "format=rgb24",
            "-fps_mode",
            "passthrough",
            "-enc_time_base",
            "demux",
            "-c:v",
            "rawvideo",
            "-f",
            "framehash",
            "-hash",
            "sha256",
            "pipe:1",
        ])
        .stdout(Stdio::piped());
    let mut child = command
        .spawn()
        .map_err(|_| "visual_inventory_spawn_failed")?;
    let stdout = child
        .stdout
        .take()
        .ok_or("visual_inventory_stdout_missing")?;
    let mut lines = BufReader::new(stdout).lines();
    let mut parser = FramehashParser::default();
    let mut offsets = Vec::new();
    let mut index_bytes_estimate =
        serde_json::to_vec(&json!({"schemaVersion":2,"kind":"media_frame_index","offsets":[]}))
            .map_err(|_| "visual_index_serialize_failed")?
            .len();
    let mut byte_offset = 0_u64;
    let outcome = tokio::time::timeout(INVENTORY_TIMEOUT, async {
        while let Some(line) = lines
            .next_line()
            .await
            .map_err(|_| "visual_inventory_read_failed")?
        {
            if line.len() > MAX_FRAMEHASH_LINE {
                return Err("visual_inventory_line_too_long");
            }
            let Some(row) = parser.push(&line)? else {
                continue;
            };
            let index = row["frameIndex"]
                .as_u64()
                .ok_or("visual_inventory_row_invalid")?;
            add_index_offset(&mut offsets, &mut index_bytes_estimate, index, byte_offset)?;
            let encoded =
                serde_json::to_vec(&row).map_err(|_| "visual_inventory_serialize_failed")?;
            byte_offset = byte_offset
                .checked_add(encoded.len() as u64 + 1)
                .ok_or("visual_inventory_too_large")?;
            writer
                .append(&row)
                .map_err(|_| "visual_inventory_store_failed")?;
        }
        let status = child
            .wait()
            .await
            .map_err(|_| "visual_inventory_wait_failed")?;
        if !status.success() {
            return Err("visual_inventory_decode_failed");
        }
        parser.finish()
    })
    .await;
    let metadata = match outcome {
        Ok(Ok(metadata)) => metadata,
        Ok(Err(error)) => {
            let _ = child.kill().await;
            let _ = child.wait().await;
            return Err(error.into());
        }
        Err(_) => {
            let _ = child.kill().await;
            let _ = child.wait().await;
            return Err("visual_inventory_timeout".into());
        }
    };
    let inventory_ref = writer
        .finish()
        .map_err(|_| "visual_inventory_store_failed")?;
    let index = json!({"schemaVersion":2,"kind":"media_frame_index","offsets":offsets});
    let index_bytes = serde_json::to_vec(&index).map_err(|_| "visual_index_serialize_failed")?;
    if index_bytes.len() > MAX_INDEX_BYTES {
        return Err("visual_index_too_large".into());
    }
    let index_ref = store
        .put_bytes(&index_bytes)
        .map_err(|_| "visual_index_store_failed")?;
    let descriptor = json!({
        "kind":"media_frame_inventory", "schemaVersion":2,
        "source":source_ref.to_json(), "inventory":inventory_ref.to_json(), "index":index_ref.to_json(),
        "sourceIdentity":identity,
        "frameCount":metadata.frame_count, "firstPts":metadata.first_pts.to_string(),
        "timeBaseNumerator":metadata.time_base_numerator,
        "timeBaseDenominator":metadata.time_base_denominator,
        "width":metadata.width, "height":metadata.height, "pixelFormat":"rgb24",
        "decoderContractSha256":format!("{:x}", Sha256::digest(DECODER_CONTRACT.as_bytes())),
    });
    let descriptor_bytes =
        serde_json::to_vec(&descriptor).map_err(|_| "visual_descriptor_serialize_failed")?;
    let descriptor_ref = store
        .put_bytes(&descriptor_bytes)
        .map_err(|_| "visual_descriptor_store_failed")?;
    Ok(descriptor_ref.to_json())
}

/// Decode from frame zero through the greatest requested index. The supplied
/// rows must come from the verified inventory object selected by the caller.
/// The raw RGB digest is checked before any frame is offered to vision.
pub(crate) async fn extract(
    ffmpeg: &Path,
    source_path: &Path,
    descriptor: &Value,
    rows: &[Value],
    workdir: &Path,
) -> Result<Vec<Value>, String> {
    check_tool_and_source(ffmpeg, source_path)?;
    let source_ref = ArtifactRef::from_json(&descriptor["source"])
        .map_err(|_| "visual_descriptor_source_invalid")?;
    verify_source_file(source_path, &source_ref)?;
    if !workdir.is_absolute() || !workdir.is_dir() {
        return Err("visual_workdir_invalid".into());
    }
    let (width, height, frame_count) = descriptor_dimensions(descriptor)?;
    if rows.is_empty() || rows.len() > 32 {
        return Err("visual_extract_chunk_invalid".into());
    }
    let mut selected = rows.to_vec();
    selected.sort_by_key(|row| row["frameIndex"].as_u64().unwrap_or(u64::MAX));
    let mut indices = BTreeSet::new();
    for row in &selected {
        validate_inventory_row(row, frame_count)?;
        if !indices.insert(
            row["frameIndex"]
                .as_u64()
                .ok_or("visual_inventory_row_invalid")?,
        ) {
            return Err("visual_extract_duplicate_index".into());
        }
    }
    let last = *indices.last().ok_or("visual_extract_chunk_invalid")?;
    let frame_limit = last.checked_add(1).ok_or("visual_extract_chunk_invalid")?;
    let frame_bytes = width
        .checked_mul(height)
        .filter(|pixels| *pixels <= MAX_FRAME_PIXELS)
        .and_then(|pixels| pixels.checked_mul(3))
        .filter(|bytes| *bytes > 0 && *bytes <= MAX_FRAME_BYTES)
        .ok_or("visual_frame_dimensions_invalid")?;
    let mut command = base_command(ffmpeg);
    command
        .args([
            "-i",
            source_path.to_str().ok_or("visual_source_path_invalid")?,
            "-map",
            "0:v:0",
            "-vf",
            "format=rgb24",
            "-fps_mode",
            "passthrough",
            "-enc_time_base",
            "demux",
            "-c:v",
            "rawvideo",
            "-pix_fmt",
            "rgb24",
            "-frames:v",
            &frame_limit.to_string(),
            "-f",
            "rawvideo",
            "pipe:1",
        ])
        .stdout(Stdio::piped());
    let mut child = command.spawn().map_err(|_| "visual_extract_spawn_failed")?;
    let mut stdout = child.stdout.take().ok_or("visual_extract_stdout_missing")?;
    let frame_dir = workdir.join("vision-frames");
    fs::create_dir_all(&frame_dir).map_err(|_| "visual_frame_dir_failed")?;
    let mut frame = vec![0_u8; frame_bytes as usize];
    let mut produced = Vec::with_capacity(selected.len());
    let mut generated_paths = Vec::with_capacity(selected.len());
    let mut encoded_total = 0_u64;
    let outcome = tokio::time::timeout(EXTRACT_TIMEOUT, async {
        let mut next_selected = 0_usize;
        for index in 0..frame_limit {
            stdout
                .read_exact(&mut frame)
                .await
                .map_err(|_| "visual_extract_truncated")?;
            if next_selected >= selected.len() || selected[next_selected]["frameIndex"] != index {
                continue;
            }
            let row = &selected[next_selected];
            let pixel_sha = format!("{:x}", Sha256::digest(&frame));
            if row["pixelSha256"] != pixel_sha {
                return Err("visual_extract_pixel_mismatch");
            }
            let png_path = frame_dir.join(format!(
                "frame-{index:012}-{}.png",
                uuid::Uuid::new_v4().simple()
            ));
            generated_paths.push(png_path.clone());
            encode_png(ffmpeg, &frame, width, height, &png_path).await?;
            let (png_sha, png_bytes) = hash_png(&png_path)?;
            encoded_total = encoded_total
                .checked_add(png_bytes)
                .ok_or("visual_png_chunk_too_large")?;
            if encoded_total > MAX_PNG_CHUNK_BYTES {
                return Err("visual_png_chunk_too_large");
            }
            produced.push(json!({
                "id":format!("frame-{index:012}"), "frameIndex":index,
                "pts":row["pts"], "timestampMs":row["timestampMs"],
                "pixelSha256":pixel_sha, "sha256":png_sha,
                "path":png_path.to_string_lossy().to_string(), "mimeType":"image/png",
            }));
            next_selected += 1;
        }
        if next_selected != selected.len() {
            return Err("visual_extract_incomplete");
        }
        let mut extra = [0_u8; 1];
        if stdout
            .read(&mut extra)
            .await
            .map_err(|_| "visual_extract_read_failed")?
            != 0
        {
            return Err("visual_extract_extra_output");
        }
        let status = child
            .wait()
            .await
            .map_err(|_| "visual_extract_wait_failed")?;
        if !status.success() {
            return Err("visual_extract_decode_failed");
        }
        Ok(())
    })
    .await;
    match outcome {
        Ok(Ok(())) => Ok(produced),
        Ok(Err(error)) => {
            let _ = child.kill().await;
            let _ = child.wait().await;
            for path in generated_paths {
                let _ = fs::remove_file(path);
            }
            Err(error.into())
        }
        Err(_) => {
            let _ = child.kill().await;
            let _ = child.wait().await;
            for path in generated_paths {
                let _ = fs::remove_file(path);
            }
            Err("visual_extract_timeout".into())
        }
    }
}

#[derive(Default)]
struct FramehashParser {
    time_base: Option<(u64, u64)>,
    dimensions: Option<(u64, u64)>,
    first_pts: Option<i64>,
    frame_count: u64,
}

struct InventoryMetadata {
    time_base_numerator: u64,
    time_base_denominator: u64,
    width: u64,
    height: u64,
    first_pts: i64,
    frame_count: u64,
}

impl FramehashParser {
    fn push(&mut self, line: &str) -> Result<Option<Value>, &'static str> {
        let line = line.trim();
        if line.is_empty() {
            return Ok(None);
        }
        if line.starts_with('#') {
            if self.frame_count > 0 {
                return Err("visual_inventory_header_late");
            }
            if let Some(value) = line.strip_prefix("#tb 0:") {
                if self.time_base.is_some() {
                    return Err("visual_inventory_timebase_duplicate");
                }
                let (num, den) = value
                    .trim()
                    .split_once('/')
                    .ok_or("visual_inventory_timebase_invalid")?;
                let num = num
                    .parse::<u64>()
                    .map_err(|_| "visual_inventory_timebase_invalid")?;
                let den = den
                    .parse::<u64>()
                    .map_err(|_| "visual_inventory_timebase_invalid")?;
                if num == 0 || den == 0 {
                    return Err("visual_inventory_timebase_invalid");
                }
                self.time_base = Some((num, den));
            } else if let Some(value) = line.strip_prefix("#dimensions 0:") {
                if self.dimensions.is_some() {
                    return Err("visual_inventory_dimensions_duplicate");
                }
                let (width, height) = value
                    .trim()
                    .split_once('x')
                    .ok_or("visual_inventory_dimensions_invalid")?;
                let width = width
                    .parse::<u64>()
                    .map_err(|_| "visual_inventory_dimensions_invalid")?;
                let height = height
                    .parse::<u64>()
                    .map_err(|_| "visual_inventory_dimensions_invalid")?;
                if width == 0 || height == 0 {
                    return Err("visual_inventory_dimensions_invalid");
                }
                self.dimensions = Some((width, height));
            }
            return Ok(None);
        }
        let (num, den) = self.time_base.ok_or("visual_inventory_timebase_missing")?;
        let (width, height) = self
            .dimensions
            .ok_or("visual_inventory_dimensions_missing")?;
        let fields: Vec<_> = line.split(',').map(str::trim).collect();
        if fields.len() != 6 || fields[0] != "0" {
            return Err("visual_inventory_row_invalid");
        }
        let pts = fields[2]
            .parse::<i64>()
            .map_err(|_| "visual_inventory_pts_invalid")?;
        let size = fields[4]
            .parse::<u64>()
            .map_err(|_| "visual_inventory_size_invalid")?;
        let expected_size = width
            .checked_mul(height)
            .filter(|pixels| *pixels <= MAX_FRAME_PIXELS)
            .and_then(|n| n.checked_mul(3))
            .filter(|n| *n <= MAX_FRAME_BYTES)
            .ok_or("visual_inventory_dimensions_invalid")?;
        if size != expected_size {
            return Err("visual_inventory_size_invalid");
        }
        let pixel_sha = fields[5];
        if !valid_sha(pixel_sha) {
            return Err("visual_inventory_hash_invalid");
        }
        let first = *self.first_pts.get_or_insert(pts);
        let delta = i128::from(pts) - i128::from(first);
        if delta < 0 {
            return Err("visual_inventory_pts_out_of_order");
        }
        let timestamp = delta
            .checked_mul(i128::from(num))
            .and_then(|n| n.checked_mul(1000))
            .map(|n| n / i128::from(den))
            .and_then(|n| u64::try_from(n).ok())
            .ok_or("visual_inventory_timestamp_overflow")?;
        let index = self.frame_count;
        self.frame_count = self
            .frame_count
            .checked_add(1)
            .ok_or("visual_inventory_too_many_frames")?;
        Ok(Some(
            json!({"frameIndex":index,"pts":pts.to_string(),"timestampMs":timestamp,"pixelSha256":pixel_sha}),
        ))
    }

    fn finish(self) -> Result<InventoryMetadata, &'static str> {
        let (num, den) = self.time_base.ok_or("visual_inventory_timebase_missing")?;
        let (width, height) = self
            .dimensions
            .ok_or("visual_inventory_dimensions_missing")?;
        let first_pts = self.first_pts.ok_or("visual_inventory_empty")?;
        Ok(InventoryMetadata {
            time_base_numerator: num,
            time_base_denominator: den,
            width,
            height,
            first_pts,
            frame_count: self.frame_count,
        })
    }
}

fn base_command(ffmpeg: &Path) -> Command {
    let mut command = Command::new(ffmpeg);
    command
        .args(["-nostdin", "-hide_banner", "-loglevel", "error"])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    #[cfg(windows)]
    command.creation_flags(0x08000000);
    command
}

fn check_tool_and_source(ffmpeg: &Path, source: &Path) -> Result<(), String> {
    if !ffmpeg.is_absolute() || !ffmpeg.is_file() {
        return Err("visual_ffmpeg_path_invalid".into());
    }
    if !source.is_absolute() || !source.is_file() {
        return Err("visual_source_path_invalid".into());
    }
    Ok(())
}

fn verify_source_file(path: &Path, reference: &ArtifactRef) -> Result<(), String> {
    let mut file = File::open(path).map_err(|_| "visual_source_read_failed")?;
    let mut hash = Sha256::new();
    let mut bytes = 0_u64;
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|_| "visual_source_read_failed")?;
        if read == 0 {
            break;
        }
        bytes = bytes
            .checked_add(read as u64)
            .ok_or("visual_source_size_invalid")?;
        if bytes > reference.bytes {
            return Err("visual_source_identity_mismatch".into());
        }
        hash.update(&buffer[..read]);
    }
    if bytes != reference.bytes || format!("{:x}", hash.finalize()) != reference.sha256 {
        return Err("visual_source_identity_mismatch".into());
    }
    Ok(())
}

fn parse_identity(identity: &Value) -> Result<(&str, &str, &str, u64), String> {
    let object = identity
        .as_object()
        .ok_or("visual_source_identity_invalid")?;
    if object.len() != 4
        || !["account", "postKey", "mediaSha256", "durationMs"]
            .iter()
            .all(|key| object.contains_key(*key))
    {
        return Err("visual_source_identity_invalid".into());
    }
    let account = identity["account"]
        .as_str()
        .filter(|s| !s.trim().is_empty() && s.len() <= 256)
        .ok_or("visual_source_identity_invalid")?;
    let post_key = identity["postKey"]
        .as_str()
        .filter(|s| !s.trim().is_empty() && s.len() <= 1024)
        .ok_or("visual_source_identity_invalid")?;
    let media_sha = identity["mediaSha256"]
        .as_str()
        .filter(|s| valid_sha(s))
        .ok_or("visual_source_identity_invalid")?;
    let duration_ms = identity["durationMs"]
        .as_u64()
        .ok_or("visual_source_identity_invalid")?;
    Ok((account, post_key, media_sha, duration_ms))
}

fn descriptor_dimensions(descriptor: &Value) -> Result<(u64, u64, u64), String> {
    if descriptor["kind"] != "media_frame_inventory"
        || descriptor["schemaVersion"] != 2
        || descriptor["pixelFormat"] != "rgb24"
        || descriptor["decoderContractSha256"]
            != format!("{:x}", Sha256::digest(DECODER_CONTRACT.as_bytes()))
    {
        return Err("visual_descriptor_invalid".into());
    }
    for key in ["source", "inventory", "index"] {
        ArtifactRef::from_json(&descriptor[key]).map_err(|_| "visual_descriptor_invalid")?;
    }
    let (_, _, media_sha, _) = parse_identity(&descriptor["sourceIdentity"])?;
    if descriptor["source"]["sha256"] != media_sha {
        return Err("visual_descriptor_invalid".into());
    }
    let width = descriptor["width"]
        .as_u64()
        .filter(|n| *n > 0)
        .ok_or("visual_descriptor_invalid")?;
    let height = descriptor["height"]
        .as_u64()
        .filter(|n| *n > 0)
        .ok_or("visual_descriptor_invalid")?;
    let count = descriptor["frameCount"]
        .as_u64()
        .filter(|n| *n > 0)
        .ok_or("visual_descriptor_invalid")?;
    Ok((width, height, count))
}

fn validate_inventory_row(row: &Value, frame_count: u64) -> Result<(), String> {
    let object = row.as_object().ok_or("visual_inventory_row_invalid")?;
    if object.len() != 4
        || !["frameIndex", "pts", "timestampMs", "pixelSha256"]
            .iter()
            .all(|key| object.contains_key(*key))
        || row["frameIndex"].as_u64().is_none_or(|n| n >= frame_count)
        || row["pts"]
            .as_str()
            .is_none_or(|s| s.parse::<i64>().is_err())
        || row["timestampMs"].as_u64().is_none()
        || row["pixelSha256"].as_str().is_none_or(|s| !valid_sha(s))
    {
        return Err("visual_inventory_row_invalid".into());
    }
    Ok(())
}

async fn encode_png(
    ffmpeg: &Path,
    rgb: &[u8],
    width: u64,
    height: u64,
    output: &Path,
) -> Result<(), &'static str> {
    let mut command = base_command(ffmpeg);
    command
        .args([
            "-f",
            "rawvideo",
            "-pixel_format",
            "rgb24",
            "-video_size",
            &format!("{width}x{height}"),
            "-framerate",
            "1",
            "-i",
            "pipe:0",
            "-frames:v",
            "1",
            "-c:v",
            "png",
            "-f",
            "image2",
            "-update",
            "1",
            "-n",
            output.to_str().ok_or("visual_png_path_invalid")?,
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::null());
    let mut child = command.spawn().map_err(|_| "visual_png_spawn_failed")?;
    let mut input = child.stdin.take().ok_or("visual_png_stdin_missing")?;
    let result = tokio::time::timeout(PNG_TIMEOUT, async {
        input
            .write_all(rgb)
            .await
            .map_err(|_| "visual_png_write_failed")?;
        input
            .shutdown()
            .await
            .map_err(|_| "visual_png_write_failed")?;
        drop(input);
        let status = child.wait().await.map_err(|_| "visual_png_wait_failed")?;
        if !status.success() {
            return Err("visual_png_encode_failed");
        }
        Ok(())
    })
    .await;
    match result {
        Ok(Ok(())) => Ok(()),
        Ok(Err(error)) => {
            let _ = child.kill().await;
            let _ = child.wait().await;
            Err(error)
        }
        Err(_) => {
            let _ = child.kill().await;
            let _ = child.wait().await;
            Err("visual_png_timeout")
        }
    }
}

fn hash_png(path: &Path) -> Result<(String, u64), &'static str> {
    let mut file = File::open(path).map_err(|_| "visual_png_missing")?;
    let length = file.metadata().map_err(|_| "visual_png_missing")?.len();
    if length < 8 || length > MAX_PNG_BYTES {
        return Err("visual_png_size_invalid");
    }
    let mut signature = [0_u8; 8];
    file.read_exact(&mut signature)
        .map_err(|_| "visual_png_read_failed")?;
    if signature != [137, 80, 78, 71, 13, 10, 26, 10] {
        return Err("visual_png_invalid");
    }
    let mut hash = Sha256::new();
    hash.update(signature);
    let mut buffer = [0_u8; 64 * 1024];
    let mut read_total = 8_u64;
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|_| "visual_png_read_failed")?;
        if read == 0 {
            break;
        }
        read_total = read_total
            .checked_add(read as u64)
            .ok_or("visual_png_size_invalid")?;
        if read_total > MAX_PNG_BYTES {
            return Err("visual_png_size_invalid");
        }
        hash.update(&buffer[..read]);
    }
    if read_total != length {
        return Err("visual_png_size_changed");
    }
    Ok((format!("{:x}", hash.finalize()), read_total))
}

fn valid_sha(text: &str) -> bool {
    text.len() == 64
        && text
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn add_index_offset(
    offsets: &mut Vec<Value>,
    bytes_estimate: &mut usize,
    frame_index: u64,
    byte_offset: u64,
) -> Result<(), &'static str> {
    if frame_index % 32 != 0 {
        return Ok(());
    }
    let entry = json!({"frameIndex":frame_index,"byteOffset":byte_offset});
    let entry_bytes = serde_json::to_vec(&entry)
        .map_err(|_| "visual_index_serialize_failed")?
        .len();
    *bytes_estimate = bytes_estimate
        .checked_add(entry_bytes + usize::from(!offsets.is_empty()))
        .ok_or("visual_index_too_large")?;
    if *bytes_estimate > MAX_INDEX_BYTES {
        return Err("visual_index_too_large");
    }
    offsets.push(entry);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn framehash_preserves_duplicate_pts_and_indexes_every_position() {
        let mut parser = FramehashParser::default();
        assert!(parser.push("#tb 0: 1/1000").unwrap().is_none());
        assert!(parser.push("#dimensions 0: 2x1").unwrap().is_none());
        let hash = "a".repeat(64);
        let first = parser
            .push(&format!("0, -4, -4, 1, 6, {hash}"))
            .unwrap()
            .unwrap();
        let second = parser
            .push(&format!("0, -4, -4, 1, 6, {hash}"))
            .unwrap()
            .unwrap();
        let third = parser
            .push(&format!("0, -3, 12, 1, 6, {hash}"))
            .unwrap()
            .unwrap();
        assert_eq!(first["frameIndex"], 0);
        assert_eq!(second["frameIndex"], 1);
        assert_eq!(first["timestampMs"], 0);
        assert_eq!(second["timestampMs"], 0);
        assert_eq!(third["timestampMs"], 16);
        let metadata = parser.finish().unwrap();
        assert_eq!(metadata.frame_count, 3);
        assert_eq!(metadata.first_pts, -4);
    }

    #[test]
    fn framehash_rejects_missing_headers_wrong_size_and_backward_pts() {
        let mut parser = FramehashParser::default();
        assert!(
            parser
                .push(&format!("0, 0, 0, 1, 6, {}", "a".repeat(64)))
                .is_err()
        );
        parser.push("#tb 0: 1/1000").unwrap();
        parser.push("#dimensions 0: 2x1").unwrap();
        assert!(
            parser
                .push(&format!("0, 0, 0, 1, 5, {}", "a".repeat(64)))
                .is_err()
        );
        parser
            .push(&format!("0, 0, 5, 1, 6, {}", "a".repeat(64)))
            .unwrap();
        assert!(
            parser
                .push(&format!("0, 0, 4, 1, 6, {}", "a".repeat(64)))
                .is_err()
        );
    }

    #[test]
    fn exact_rows_reject_extra_keys_and_duplicate_selected_index() {
        let row = json!({"frameIndex":1,"pts":"1","timestampMs":1,"pixelSha256":"a".repeat(64)});
        assert!(validate_inventory_row(&row, 2).is_ok());
        let mut extra = row.clone();
        extra["path"] = json!("untrusted");
        assert!(validate_inventory_row(&extra, 2).is_err());
        let mut outside = row;
        outside["frameIndex"] = json!(2);
        assert!(validate_inventory_row(&outside, 2).is_err());
    }

    #[test]
    fn index_records_exact_byte_offsets_at_32_frame_boundaries() {
        let mut offsets = Vec::new();
        let mut bytes_estimate =
            serde_json::to_vec(&json!({"schemaVersion":2,"kind":"media_frame_index","offsets":[]}))
                .unwrap()
                .len();
        let mut byte_offset = 0_u64;
        for index in 0..65_u64 {
            add_index_offset(&mut offsets, &mut bytes_estimate, index, byte_offset).unwrap();
            byte_offset += 77;
        }
        assert_eq!(
            offsets,
            vec![
                json!({"frameIndex":0,"byteOffset":0}),
                json!({"frameIndex":32,"byteOffset":32*77}),
                json!({"frameIndex":64,"byteOffset":64*77}),
            ]
        );
        let encoded = serde_json::to_vec(
            &json!({"schemaVersion":2,"kind":"media_frame_index","offsets":offsets}),
        )
        .unwrap();
        assert_eq!(bytes_estimate, encoded.len());
    }
}
