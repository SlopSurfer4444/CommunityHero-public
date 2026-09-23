//! Deterministic all-frame metric pass and sparse visual selection.
//! Metrics use a <=640-pixel grayscale view only; original RGB/PNG bytes are
//! never resized for vision. Every decoded RGB frame is bound to its inventory
//! pixel hash before its features can influence selection.

use crate::media_artifacts::{ArtifactRef, ArtifactStore, JsonlWriter};
use crate::media_frame_decoder::DECODER_CONTRACT;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fs::File;
use std::io::{BufRead, BufReader, Read};
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;
use tokio::io::AsyncReadExt;
use tokio::process::Command;

const TIMEOUT: Duration = Duration::from_secs(4 * 60 * 60);
const MAX_FRAME_BYTES: u64 = 256 * 1024 * 1024;
const MAX_INDEX_BYTES: usize = 16 * 1024 * 1024;
const MAX_DESCRIPTOR_BYTES: u64 = 1024 * 1024;
const FEATURE_LONG_EDGE: usize = 640;
const TILE: usize = 16;
const EDGE_THRESHOLD: u16 = 24;
const BASELINE_MS: u64 = 500;
const LOCAL_FLIP_PERMILLE: u32 = 90;
const LOCAL_EDGE_DENSITY_DELTA_PERMILLE: u32 = 70;
const LOCAL_VS_MEDIAN: u32 = 3;
const LOCAL_STABLE_FLIP_PERMILLE: u32 = 20;
const LOCAL_STABLE_MEAN_DELTA: u32 = 4;
const SCENE_BROAD_TILE_PERMILLE: u32 = 150;
const SCENE_BROAD_PERCENT: u32 = 40;
const SCENE_HISTOGRAM_TV_PERMILLE: u32 = 180;
const PULSE_EDGE_FLIP_PERMILLE: u32 = 120;
const PULSE_EDGE_GAIN_PERMILLE: u32 = 60;
const PULSE_RETURN_FLIP_PERMILLE: u32 = 50;

/// Returns the immutable selection descriptor's ArtifactRef JSON. Its
/// `selection` object is JSONL; `selectionIndex` is the next-work cursor.
pub(crate) async fn select(
    ffmpeg: &Path,
    source_path: &Path,
    inventory_descriptor_ref: &Value,
    store: &ArtifactStore,
) -> Result<Value, String> {
    if !ffmpeg.is_absolute() || !ffmpeg.is_file() {
        return Err("visual_selection_ffmpeg_invalid".into());
    }
    if !source_path.is_absolute() || !source_path.is_file() {
        return Err("visual_selection_source_invalid".into());
    }
    let descriptor_ref = ArtifactRef::from_json(inventory_descriptor_ref)
        .map_err(|_| "visual_selection_descriptor_ref_invalid")?;
    let descriptor_bytes = store
        .read_bytes(&descriptor_ref, MAX_DESCRIPTOR_BYTES)
        .map_err(|_| "visual_selection_descriptor_missing")?;
    let descriptor: Value = serde_json::from_slice(&descriptor_bytes)
        .map_err(|_| "visual_selection_descriptor_invalid")?;
    let (inventory_ref, source_ref, width, height, frame_count) = validate_descriptor(&descriptor)?;
    store
        .verify(&source_ref)
        .map_err(|_| "visual_selection_source_artifact_invalid")?;
    verify_source_file(source_path, &source_ref)?;
    let inventory_path = store
        .path(&inventory_ref)
        .map_err(|_| "visual_selection_inventory_missing")?;
    let inventory_file =
        File::open(inventory_path).map_err(|_| "visual_selection_inventory_missing")?;
    let mut inventory_lines = BufReader::new(inventory_file).lines();
    let frame_bytes = width
        .checked_mul(height)
        .and_then(|n| n.checked_mul(3))
        .filter(|n| *n > 0 && *n <= MAX_FRAME_BYTES)
        .ok_or("visual_selection_dimensions_invalid")?;
    let mut command = Command::new(ffmpeg);
    command
        .args([
            "-nostdin",
            "-hide_banner",
            "-loglevel",
            "error",
            "-i",
            source_path
                .to_str()
                .ok_or("visual_selection_source_invalid")?,
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
            "-f",
            "rawvideo",
            "pipe:1",
        ])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .stdout(Stdio::piped())
        .kill_on_drop(true);
    #[cfg(windows)]
    command.creation_flags(0x08000000);
    let mut child = command
        .spawn()
        .map_err(|_| "visual_selection_spawn_failed")?;
    let mut stdout = child
        .stdout
        .take()
        .ok_or("visual_selection_stdout_missing")?;
    let mut writer = store
        .begin_jsonl()
        .map_err(|_| "visual_selection_store_failed")?;
    let mut core = SelectorCore::default();
    let mut offsets = Vec::new();
    let mut index_bytes_estimate =
        serde_json::to_vec(&json!({"schemaVersion":2,"kind":"media_frame_index","offsets":[]}))
            .map_err(|_| "visual_selection_index_invalid")?
            .len();
    let mut output_offset = 0_u64;
    let mut selected_count = 0_u64;
    let mut reason_counts = BTreeMap::<String, u64>::new();
    let mut rgb = vec![0_u8; frame_bytes as usize];
    let outcome = tokio::time::timeout(TIMEOUT, async {
        for frame_index in 0..frame_count {
            let line = inventory_lines
                .next()
                .ok_or("visual_selection_inventory_short")?
                .map_err(|_| "visual_selection_inventory_read_failed")?;
            let row: Value = serde_json::from_str(&line)
                .map_err(|_| "visual_selection_inventory_row_invalid")?;
            validate_row(&row, frame_index)?;
            stdout
                .read_exact(&mut rgb)
                .await
                .map_err(|_| "visual_selection_decode_short")?;
            let pixel_sha = format!("{:x}", Sha256::digest(&rgb));
            if row["pixelSha256"] != pixel_sha {
                return Err("visual_selection_pixel_mismatch");
            }
            let metrics = Metrics::from_rgb(&rgb, width as usize, height as usize)?;
            for selected in core.push(row, metrics)? {
                emit_selection(
                    selected,
                    &mut writer,
                    &mut offsets,
                    &mut index_bytes_estimate,
                    &mut output_offset,
                    &mut selected_count,
                    &mut reason_counts,
                )?;
            }
        }
        for selected in core.finish()? {
            emit_selection(
                selected,
                &mut writer,
                &mut offsets,
                &mut index_bytes_estimate,
                &mut output_offset,
                &mut selected_count,
                &mut reason_counts,
            )?;
        }
        if inventory_lines.next().is_some() {
            return Err("visual_selection_inventory_extra");
        }
        let mut extra = [0_u8; 1];
        if stdout
            .read(&mut extra)
            .await
            .map_err(|_| "visual_selection_read_failed")?
            != 0
        {
            return Err("visual_selection_decode_extra");
        }
        let status = child
            .wait()
            .await
            .map_err(|_| "visual_selection_wait_failed")?;
        if !status.success() {
            return Err("visual_selection_decode_failed");
        }
        Ok(())
    })
    .await;
    match outcome {
        Ok(Ok(())) => {}
        Ok(Err(error)) => {
            let _ = child.kill().await;
            let _ = child.wait().await;
            return Err(error.into());
        }
        Err(_) => {
            let _ = child.kill().await;
            let _ = child.wait().await;
            return Err("visual_selection_timeout".into());
        }
    }
    let selection_ref = writer
        .finish()
        .map_err(|_| "visual_selection_store_failed")?;
    let selection_index = json!({"schemaVersion":2,"kind":"media_frame_index","offsets":offsets});
    let selection_index_bytes =
        serde_json::to_vec(&selection_index).map_err(|_| "visual_selection_index_invalid")?;
    if selection_index_bytes.len() > MAX_INDEX_BYTES {
        return Err("visual_selection_index_too_large".into());
    }
    let selection_index_ref = store
        .put_bytes(&selection_index_bytes)
        .map_err(|_| "visual_selection_index_store_failed")?;
    let policy = policy();
    let selection_descriptor = json!({
        "schemaVersion":2,"kind":"media_frame_selection",
        "inventoryDescriptor":descriptor_ref.to_json(),"selection":selection_ref.to_json(),
        "selectionIndex":selection_index_ref.to_json(),"selectedCount":selected_count,
        "frameCount":frame_count,"policy":policy,"reasonCounts":reason_counts,
    });
    let bytes = serde_json::to_vec(&selection_descriptor)
        .map_err(|_| "visual_selection_descriptor_invalid")?;
    let reference = store
        .put_bytes(&bytes)
        .map_err(|_| "visual_selection_descriptor_store_failed")?;
    Ok(reference.to_json())
}

fn validate_descriptor(
    descriptor: &Value,
) -> Result<(ArtifactRef, ArtifactRef, u64, u64, u64), String> {
    if descriptor["kind"] != "media_frame_inventory"
        || descriptor["schemaVersion"] != 2
        || descriptor["pixelFormat"] != "rgb24"
        || descriptor["decoderContractSha256"]
            != format!("{:x}", Sha256::digest(DECODER_CONTRACT.as_bytes()))
    {
        return Err("visual_selection_descriptor_invalid".into());
    }
    let source = ArtifactRef::from_json(&descriptor["source"])
        .map_err(|_| "visual_selection_descriptor_invalid")?;
    let inventory = ArtifactRef::from_json(&descriptor["inventory"])
        .map_err(|_| "visual_selection_descriptor_invalid")?;
    ArtifactRef::from_json(&descriptor["index"])
        .map_err(|_| "visual_selection_descriptor_invalid")?;
    if descriptor["sourceIdentity"]["mediaSha256"] != source.sha256 {
        return Err("visual_selection_descriptor_invalid".into());
    }
    let width = descriptor["width"]
        .as_u64()
        .filter(|n| *n > 0)
        .ok_or("visual_selection_descriptor_invalid")?;
    let height = descriptor["height"]
        .as_u64()
        .filter(|n| *n > 0)
        .ok_or("visual_selection_descriptor_invalid")?;
    let count = descriptor["frameCount"]
        .as_u64()
        .filter(|n| *n > 0)
        .ok_or("visual_selection_descriptor_invalid")?;
    Ok((inventory, source, width, height, count))
}

fn verify_source_file(path: &Path, reference: &ArtifactRef) -> Result<(), String> {
    let mut file = File::open(path).map_err(|_| "visual_selection_source_read_failed")?;
    let mut hash = Sha256::new();
    let mut bytes = 0_u64;
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|_| "visual_selection_source_read_failed")?;
        if read == 0 {
            break;
        }
        bytes = bytes
            .checked_add(read as u64)
            .ok_or("visual_selection_source_invalid")?;
        if bytes > reference.bytes {
            return Err("visual_selection_source_changed".into());
        }
        hash.update(&buffer[..read]);
    }
    if bytes != reference.bytes || format!("{:x}", hash.finalize()) != reference.sha256 {
        return Err("visual_selection_source_changed".into());
    }
    Ok(())
}

fn validate_row(row: &Value, index: u64) -> Result<(), &'static str> {
    let object = row
        .as_object()
        .ok_or("visual_selection_inventory_row_invalid")?;
    if object.len() != 4
        || !["frameIndex", "pts", "timestampMs", "pixelSha256"]
            .iter()
            .all(|key| object.contains_key(*key))
        || row["frameIndex"] != index
        || row["pts"]
            .as_str()
            .is_none_or(|s| s.parse::<i64>().is_err())
        || row["timestampMs"].as_u64().is_none()
        || row["pixelSha256"].as_str().is_none_or(|s| !valid_sha(s))
    {
        return Err("visual_selection_inventory_row_invalid");
    }
    Ok(())
}

fn valid_sha(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
}

#[derive(Clone)]
struct Metrics {
    edge: Vec<u8>,
    edge_counts: Vec<u16>,
    gray_sums: Vec<u32>,
    pixels_per_tile: Vec<u16>,
    histogram: [u32; 16],
    width: usize,
    height: usize,
    tiles_x: usize,
}

impl Metrics {
    fn from_rgb(rgb: &[u8], width: usize, height: usize) -> Result<Self, &'static str> {
        let expected = width
            .checked_mul(height)
            .and_then(|n| n.checked_mul(3))
            .ok_or("visual_selection_dimensions_invalid")?;
        if rgb.len() != expected {
            return Err("visual_selection_dimensions_invalid");
        }
        let step = width.max(height).div_ceil(FEATURE_LONG_EDGE).max(1);
        let w = width.div_ceil(step);
        let h = height.div_ceil(step);
        let mut gray = vec![0_u8; w * h];
        let mut histogram = [0_u32; 16];
        for y in 0..h {
            for x in 0..w {
                let p = ((y * step) * width + x * step) * 3;
                let lum = ((77_u32 * rgb[p] as u32
                    + 150_u32 * rgb[p + 1] as u32
                    + 29_u32 * rgb[p + 2] as u32)
                    >> 8) as u8;
                gray[y * w + x] = lum;
                histogram[(lum / 16) as usize] += 1;
            }
        }
        let tiles_x = w.div_ceil(TILE);
        let tiles_y = h.div_ceil(TILE);
        let count = tiles_x * tiles_y;
        let mut edge = vec![0_u8; w * h];
        let mut edge_counts = vec![0_u16; count];
        let mut gray_sums = vec![0_u32; count];
        let mut pixels_per_tile = vec![0_u16; count];
        for y in 0..h {
            for x in 0..w {
                let i = y * w + x;
                let tile = (y / TILE) * tiles_x + x / TILE;
                let left = if x > 0 { gray[i - 1] } else { gray[i] };
                let up = if y > 0 { gray[i - w] } else { gray[i] };
                let gradient = gray[i].abs_diff(left) as u16 + gray[i].abs_diff(up) as u16;
                if gradient >= EDGE_THRESHOLD {
                    edge[i] = 1;
                    edge_counts[tile] += 1;
                }
                gray_sums[tile] += gray[i] as u32;
                pixels_per_tile[tile] += 1;
            }
        }
        Ok(Self {
            edge,
            edge_counts,
            gray_sums,
            pixels_per_tile,
            histogram,
            width: w,
            height: h,
            tiles_x,
        })
    }
}

#[derive(Default)]
struct SelectorCore {
    pending: VecDeque<FrameState>,
    next_baseline_ms: u64,
    seen: u64,
}
struct FrameState {
    row: Value,
    metrics: Metrics,
    reasons: BTreeSet<&'static str>,
}

impl SelectorCore {
    fn push(&mut self, row: Value, metrics: Metrics) -> Result<Vec<Value>, &'static str> {
        let index = row["frameIndex"]
            .as_u64()
            .ok_or("visual_selection_inventory_row_invalid")?;
        if index != self.seen {
            return Err("visual_selection_inventory_order_invalid");
        }
        let timestamp = row["timestampMs"]
            .as_u64()
            .ok_or("visual_selection_inventory_row_invalid")?;
        if self.pending.back().is_some_and(|previous| {
            previous.row["timestampMs"]
                .as_u64()
                .is_some_and(|last| timestamp < last)
        }) {
            return Err("visual_selection_timestamp_order_invalid");
        }
        let mut reasons = BTreeSet::new();
        if self.seen == 0 {
            reasons.insert("first");
            self.next_baseline_ms = BASELINE_MS;
        } else if timestamp >= self.next_baseline_ms {
            reasons.insert("baseline");
            self.next_baseline_ms = timestamp
                .checked_div(BASELINE_MS)
                .and_then(|n| n.checked_add(1))
                .and_then(|n| n.checked_mul(BASELINE_MS))
                .unwrap_or(u64::MAX);
        }
        self.pending.push_back(FrameState {
            row,
            metrics,
            reasons,
        });
        self.seen += 1;
        let len = self.pending.len();
        if len >= 2 {
            let scene = scene_cut(
                &self.pending[len - 2].metrics,
                &self.pending[len - 1].metrics,
            );
            if scene {
                self.pending[len - 2].reasons.insert("scene_before");
                self.pending[len - 1].reasons.insert("scene_after");
            }
        }
        if len >= 3 {
            if persistent_local_change(
                &self.pending[len - 3].metrics,
                &self.pending[len - 2].metrics,
                &self.pending[len - 1].metrics,
            ) {
                self.pending[len - 3].reasons.insert("local_change_before");
                self.pending[len - 2].reasons.insert("local_change_after");
            }
            if transient_pulse(
                &self.pending[len - 3],
                &self.pending[len - 2],
                &self.pending[len - 1],
            ) {
                self.pending[len - 2].reasons.insert("transient_pulse");
            }
        }
        let mut emitted = Vec::new();
        if self.pending.len() > 2 {
            if let Some(value) = selected_row(self.pending.pop_front().expect("pending")) {
                emitted.push(value);
            }
        }
        Ok(emitted)
    }
    fn finish(&mut self) -> Result<Vec<Value>, &'static str> {
        if self.seen == 0 {
            return Err("visual_selection_empty");
        }
        self.pending
            .back_mut()
            .ok_or("visual_selection_empty")?
            .reasons
            .insert("last");
        let mut emitted = Vec::new();
        while let Some(frame) = self.pending.pop_front() {
            if let Some(value) = selected_row(frame) {
                emitted.push(value);
            }
        }
        Ok(emitted)
    }
}

fn selected_row(frame: FrameState) -> Option<Value> {
    if frame.reasons.is_empty() {
        return None;
    }
    Some(
        json!({"frameIndex":frame.row["frameIndex"],"pts":frame.row["pts"],
        "timestampMs":frame.row["timestampMs"],"pixelSha256":frame.row["pixelSha256"],
        "reasons":frame.reasons.into_iter().collect::<Vec<_>>() }),
    )
}

fn tile_flips(before: &Metrics, after: &Metrics) -> Vec<u16> {
    let mut flips = vec![0_u16; before.edge_counts.len()];
    for i in 0..before.edge.len() {
        if before.edge[i] != after.edge[i] {
            let tile = (i / before.width / TILE) * before.tiles_x + (i % before.width / TILE);
            flips[tile] += 1;
        }
    }
    flips
}

fn scene_cut(before: &Metrics, after: &Metrics) -> bool {
    if before.width != after.width || before.height != after.height {
        return true;
    }
    let tile_count = before.edge_counts.len();
    let flips = tile_flips(before, after);
    let mut broad = 0_usize;
    for tile in 0..tile_count {
        let pixels = before.pixels_per_tile[tile] as u32;
        let flip_permille = flips[tile] as u32 * 1000 / pixels;
        if flip_permille >= SCENE_BROAD_TILE_PERMILLE {
            broad += 1;
        }
    }
    let histogram_diff: u64 = before
        .histogram
        .iter()
        .zip(after.histogram.iter())
        .map(|(a, b)| a.abs_diff(*b) as u64)
        .sum();
    let pixels = (before.width * before.height) as u64;
    let total_variation_permille = histogram_diff * 500 / pixels;
    broad * 100 >= tile_count * SCENE_BROAD_PERCENT as usize
        && total_variation_permille >= SCENE_HISTOGRAM_TV_PERMILLE as u64
}

fn persistent_local_change(before: &Metrics, middle: &Metrics, after: &Metrics) -> bool {
    if before.width != middle.width
        || middle.width != after.width
        || before.height != middle.height
        || middle.height != after.height
    {
        return false;
    }
    let into = tile_flips(before, middle);
    let stable = tile_flips(middle, after);
    let mut sorted = into.clone();
    sorted.sort_unstable();
    let median = sorted[sorted.len() / 2] as u32;
    let mut eligible = vec![false; into.len()];
    for tile in 0..before.edge_counts.len() {
        let pixels = before.pixels_per_tile[tile] as u32;
        let edge_gain = middle.edge_counts[tile].saturating_sub(before.edge_counts[tile]) as u32
            * 1000
            / pixels;
        let mean_middle = middle.gray_sums[tile] / pixels;
        let mean_after = after.gray_sums[tile] / pixels;
        eligible[tile] = into[tile] as u32 * 1000 / pixels >= LOCAL_FLIP_PERMILLE
            && edge_gain >= LOCAL_EDGE_DENSITY_DELTA_PERMILLE
            && into[tile] as u32 >= LOCAL_VS_MEDIAN * median.max(1)
            && stable[tile] as u32 * 1000 / pixels <= LOCAL_STABLE_FLIP_PERMILLE
            && mean_middle.abs_diff(mean_after) <= LOCAL_STABLE_MEAN_DELTA;
    }
    adjacent_tiles(&eligible, before.tiles_x)
}

fn transient_pulse(before: &FrameState, middle: &FrameState, after: &FrameState) -> bool {
    let a = &before.metrics;
    let b = &middle.metrics;
    let c = &after.metrics;
    if a.width != b.width || b.width != c.width || a.height != b.height || b.height != c.height {
        return false;
    }
    if before.row["pixelSha256"] == after.row["pixelSha256"]
        && before.row["pixelSha256"] != middle.row["pixelSha256"]
    {
        let into = tile_flips(a, b);
        return into.iter().enumerate().any(|(tile, flip)| {
            let pixels = a.pixels_per_tile[tile] as u32;
            *flip as u32 * 1000 / pixels >= LOCAL_FLIP_PERMILLE
                && b.edge_counts[tile].saturating_sub(a.edge_counts[tile]) as u32 * 1000 / pixels
                    >= PULSE_EDGE_GAIN_PERMILLE
        });
    }
    let into = tile_flips(a, b);
    let out = tile_flips(b, c);
    let returned = tile_flips(a, c);
    let mut eligible = vec![false; into.len()];
    for tile in 0..into.len() {
        let pixels = a.pixels_per_tile[tile] as u32;
        let edge_gain =
            b.edge_counts[tile].saturating_sub(a.edge_counts[tile].max(c.edge_counts[tile])) as u32
                * 1000
                / pixels;
        eligible[tile] = into[tile] as u32 * 1000 / pixels >= PULSE_EDGE_FLIP_PERMILLE
            && out[tile] as u32 * 1000 / pixels >= PULSE_EDGE_FLIP_PERMILLE
            && returned[tile] as u32 * 1000 / pixels <= PULSE_RETURN_FLIP_PERMILLE
            && edge_gain >= PULSE_EDGE_GAIN_PERMILLE;
    }
    adjacent_tiles(&eligible, a.tiles_x)
}

fn adjacent_tiles(eligible: &[bool], tiles_x: usize) -> bool {
    eligible.iter().enumerate().any(|(index, active)| {
        *active
            && ((index % tiles_x + 1 < tiles_x && eligible.get(index + 1) == Some(&true))
                || eligible.get(index + tiles_x) == Some(&true))
    })
}

fn emit_selection(
    mut row: Value,
    writer: &mut JsonlWriter,
    offsets: &mut Vec<Value>,
    index_bytes_estimate: &mut usize,
    byte_offset: &mut u64,
    selected_count: &mut u64,
    reason_counts: &mut BTreeMap<String, u64>,
) -> Result<(), &'static str> {
    row["selectionIndex"] = json!(*selected_count);
    if *selected_count % 32 == 0 {
        let entry = json!({"frameIndex":*selected_count,"byteOffset":*byte_offset});
        let bytes = serde_json::to_vec(&entry)
            .map_err(|_| "visual_selection_index_invalid")?
            .len();
        *index_bytes_estimate = index_bytes_estimate
            .checked_add(bytes + usize::from(!offsets.is_empty()))
            .ok_or("visual_selection_index_too_large")?;
        if *index_bytes_estimate > MAX_INDEX_BYTES {
            return Err("visual_selection_index_too_large");
        }
        offsets.push(entry);
    }
    for reason in row["reasons"]
        .as_array()
        .ok_or("visual_selection_reason_invalid")?
    {
        let key = reason
            .as_str()
            .ok_or("visual_selection_reason_invalid")?
            .to_owned();
        *reason_counts.entry(key).or_default() += 1;
    }
    let bytes = serde_json::to_vec(&row)
        .map_err(|_| "visual_selection_row_invalid")?
        .len();
    *byte_offset = byte_offset
        .checked_add(bytes as u64 + 1)
        .ok_or("visual_selection_too_large")?;
    writer
        .append(&row)
        .map_err(|_| "visual_selection_store_failed")?;
    *selected_count = selected_count
        .checked_add(1)
        .ok_or("visual_selection_too_large")?;
    Ok(())
}

pub(crate) fn policy() -> Value {
    let mut value = json!({"version":2,"metricColor":"BT601-grayscale-nearest","metricLongEdgeMax":FEATURE_LONG_EDGE,
        "tileSize":TILE,"edgeThreshold":EDGE_THRESHOLD,"baselineIntervalMs":BASELINE_MS,
        "localFlipPermille":LOCAL_FLIP_PERMILLE,"localEdgeDensityDeltaPermille":LOCAL_EDGE_DENSITY_DELTA_PERMILLE,
        "localVsMedian":LOCAL_VS_MEDIAN,"localStableFlipPermille":LOCAL_STABLE_FLIP_PERMILLE,
        "localStableMeanDelta":LOCAL_STABLE_MEAN_DELTA,"localRequiresAdjacentTiles":true,
        "sceneBroadTilePermille":SCENE_BROAD_TILE_PERMILLE,"sceneBroadPercent":SCENE_BROAD_PERCENT,
        "sceneHistogramTvPermille":SCENE_HISTOGRAM_TV_PERMILLE,
        "pulseEdgeFlipPermille":PULSE_EDGE_FLIP_PERMILLE,"pulseEdgeGainPermille":PULSE_EDGE_GAIN_PERMILLE,
        "pulseReturnFlipPermille":PULSE_RETURN_FLIP_PERMILLE,
        "pulseExactRgbReturnAllowsSingleTile":true,"pulseNearReturnRequiresAdjacentTiles":true,
        "reasonEnums":["first","last","baseline","scene_before","scene_after","local_change_before","local_change_after","transient_pulse"],
        "selectionIndexOffsetKey":"frameIndex",
        "noCooldown":true,"selectedFramesAreOriginalRgb24":true});
    value["sha256"] = json!(format!(
        "{:x}",
        Sha256::digest(value.to_string().as_bytes())
    ));
    value
}

#[cfg(test)]
mod tests {
    use super::*;
    fn rgb(blank: u8) -> Vec<u8> {
        vec![blank; 64 * 64 * 3]
    }
    fn row(index: u64, time: u64) -> Value {
        json!({"frameIndex":index,"pts":index.to_string(),"timestampMs":time,"pixelSha256":"a".repeat(64)})
    }
    fn row_for_frame(index: u64, time: u64, frame: &[u8]) -> Value {
        json!({"frameIndex":index,"pts":index.to_string(),"timestampMs":time,
            "pixelSha256":format!("{:x}",Sha256::digest(frame))})
    }
    #[test]
    fn one_frame_price_pulse_is_selected_without_baseline_hit() {
        let mut core = SelectorCore::default();
        let blank = rgb(30);
        let mut price = blank.clone();
        for y in 16..32 {
            for x in 16..32 {
                let on = (y % 4 == 0) || (x % 3 == 0);
                let p = (y * 64 + x) * 3;
                for channel in &mut price[p..p + 3] {
                    *channel = if on { 240 } else { 30 };
                }
            }
        }
        let mut selected = Vec::new();
        for (index, time, bytes) in [(0, 0, &blank), (1, 33, &price), (2, 66, &blank)] {
            selected.extend(
                core.push(
                    row_for_frame(index, time, bytes),
                    Metrics::from_rgb(bytes, 64, 64).unwrap(),
                )
                .unwrap(),
            );
        }
        selected.extend(core.finish().unwrap());
        let middle = selected
            .iter()
            .find(|item| item["frameIndex"] == 1)
            .expect("brief price frame selected");
        assert!(
            middle["reasons"]
                .as_array()
                .unwrap()
                .iter()
                .any(|reason| reason == "transient_pulse")
        );
    }
    #[test]
    fn unchanged_motionless_video_uses_only_baseline_and_endpoints() {
        let mut core = SelectorCore::default();
        let frame = rgb(30);
        let mut selected = Vec::new();
        for index in 0..61_u64 {
            selected.extend(
                core.push(
                    row(index, index * 33),
                    Metrics::from_rgb(&frame, 64, 64).unwrap(),
                )
                .unwrap(),
            );
        }
        selected.extend(core.finish().unwrap());
        assert!(selected.len() <= 6);
        assert_eq!(selected.first().unwrap()["frameIndex"], 0);
        assert_eq!(selected.last().unwrap()["frameIndex"], 60);
    }

    fn moving_frame(index: usize, with_price: bool) -> Vec<u8> {
        let (width, height) = (96, 64);
        let mut image = vec![30_u8; width * height * 3];
        for y in 42..58 {
            for x in (index * 2)..(index * 2 + 12).min(width) {
                let p = (y * width + x) * 3;
                image[p..p + 3].fill(220);
            }
        }
        if with_price {
            for y in 16..32 {
                for x in 16..64 {
                    let p = (y * width + x) * 3;
                    image[p..p + 3].fill(if y % 4 == 0 || x % 3 == 0 { 240 } else { 30 });
                }
            }
        }
        image
    }

    #[test]
    fn one_frame_price_on_changing_background_is_selected() {
        let mut core = SelectorCore::default();
        let mut selected = Vec::new();
        for index in 0..3_u64 {
            let image = moving_frame(index as usize, index == 1);
            selected.extend(
                core.push(
                    row_for_frame(index, index * 33, &image),
                    Metrics::from_rgb(&image, 96, 64).unwrap(),
                )
                .unwrap(),
            );
        }
        selected.extend(core.finish().unwrap());
        let middle = selected
            .iter()
            .find(|item| item["frameIndex"] == 1)
            .expect("moving-background price selected");
        assert!(
            middle["reasons"]
                .as_array()
                .unwrap()
                .iter()
                .any(|reason| reason == "transient_pulse")
        );
    }

    #[test]
    fn ordinary_moving_bar_does_not_select_every_frame() {
        let mut core = SelectorCore::default();
        let mut selected = Vec::new();
        for index in 0..40_u64 {
            let image = moving_frame(index as usize, false);
            selected.extend(
                core.push(
                    row_for_frame(index, index * 33, &image),
                    Metrics::from_rgb(&image, 96, 64).unwrap(),
                )
                .unwrap(),
            );
        }
        selected.extend(core.finish().unwrap());
        assert!(
            selected.len() <= 8,
            "ordinary motion selected {} frames",
            selected.len()
        );
    }

    #[cfg(windows)]
    #[tokio::test]
    #[ignore = "requires pinned local FFmpeg and read-only Honda/transient fixtures"]
    async fn local_fixture_selection_counts_and_transient_price() {
        use std::path::PathBuf;
        let ffmpeg = PathBuf::from(std::env::var_os("COMMUNITYHERO_TEST_FFMPEG").unwrap_or_else(||
            "C:/Users/hello/AppData/Local/Microsoft/WinGet/Packages/Gyan.FFmpeg_Microsoft.Winget.Source_8wekyb3d8bbwe/ffmpeg-8.1.1-full_build/bin/ffmpeg.exe".into()));
        let fixture_dir =
            PathBuf::from("C:/AIDev/Workspaces/scratch/communityhero-local-vision-canary-20260923");
        for (name, duration_ms) in [("honda.mp4", 62_000_u64), ("transient-price.mkv", 100_u64)] {
            let source = fixture_dir.join(name);
            let temp = tempfile::tempdir().unwrap();
            let store = ArtifactStore::open(&temp.path().join("objects")).unwrap();
            let source_ref = store.put_file(&source).unwrap();
            let identity = json!({"account":"local-fixture","postKey":name,
                "mediaSha256":source_ref.sha256,"durationMs":duration_ms});
            let inventory_ref = crate::media_frame_decoder::inventory(
                &ffmpeg,
                &source,
                &source_ref.to_json(),
                &identity,
                &store,
            )
            .await
            .unwrap();
            let selection_ref = select(&ffmpeg, &source, &inventory_ref, &store)
                .await
                .unwrap();
            let descriptor_ref = ArtifactRef::from_json(&selection_ref).unwrap();
            let descriptor: Value = serde_json::from_slice(
                &store
                    .read_bytes(&descriptor_ref, MAX_DESCRIPTOR_BYTES)
                    .unwrap(),
            )
            .unwrap();
            eprintln!(
                "fixture={} total={} selected={} reasonCounts={}",
                name,
                descriptor["frameCount"],
                descriptor["selectedCount"],
                descriptor["reasonCounts"]
            );
            let rows_ref = ArtifactRef::from_json(&descriptor["selection"]).unwrap();
            let rows_path = store.path(&rows_ref).unwrap();
            let selected: Vec<Value> = BufReader::new(File::open(rows_path).unwrap())
                .lines()
                .map(|line| serde_json::from_str(&line.unwrap()).unwrap())
                .collect();
            if name == "transient-price.mkv" {
                assert!(selected.iter().any(|row| {
                    row["frameIndex"] == 1
                        && row["reasons"].as_array().unwrap().iter().any(|reason| {
                            reason == "transient_pulse" || reason == "local_change_after"
                        })
                }));
            }
        }
    }
}
