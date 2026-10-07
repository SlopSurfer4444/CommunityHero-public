//! Deterministic midpoint/end sampling: at most two original frames per second.
//! Uses the durable inventory emitted by our decoder after successful EOF.
//! CAS authenticates its bytes, not the provenance of arbitrary external inventories.
//! This avoids a second decode/hash of unselected pixels; extraction still checks
//! selected RGB hashes before any pixels reach vision.
//! Sampling is independent of scene/text changes and can miss brief text.

use crate::media_artifacts::{ArtifactRef, ArtifactStore, JsonlWriter};
use crate::media_frame_decoder::DECODER_CONTRACT;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fs::File;
use std::io::{BufRead, BufReader, Read};
use std::path::Path;

const MAX_INVENTORY_LINE_BYTES: u64 = 8192;
const MAX_FRAME_PIXELS: u64 = 64_000_000;
const MAX_FRAME_BYTES: u64 = 256 * 1024 * 1024;
const MAX_INDEX_BYTES: usize = 16 * 1024 * 1024;
const MAX_DESCRIPTOR_BYTES: u64 = 1024 * 1024;
pub(crate) const POLICY_ID: &str = "fixed_mid_end_2fps_v1";

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
    let (inventory_ref, source_ref, _, _, frame_count) = validate_descriptor(&descriptor)?;
    store
        .verify(&source_ref)
        .map_err(|_| "visual_selection_source_artifact_invalid")?;
    verify_source_file(source_path, &source_ref)?;
    let inventory_path = store
        .path(&inventory_ref)
        .map_err(|_| "visual_selection_inventory_missing")?;
    let inventory_file =
        File::open(inventory_path).map_err(|_| "visual_selection_inventory_missing")?;
    let index_ref = ArtifactRef::from_json(&descriptor["index"])
        .map_err(|_| "visual_selection_index_invalid")?;
    let index_bytes = store
        .read_bytes(&index_ref, MAX_INDEX_BYTES as u64)
        .map_err(|_| "visual_selection_inventory_index_missing")?;
    let inventory_index: Value = serde_json::from_slice(&index_bytes)
        .map_err(|_| "visual_selection_inventory_index_invalid")?;
    let mut writer = store
        .begin_jsonl()
        .map_err(|_| "visual_selection_store_failed")?;
    let mut offsets = Vec::new();
    let mut index_bytes_estimate =
        serde_json::to_vec(&json!({"schemaVersion":2,"kind":"media_frame_index","offsets":[]}))
            .map_err(|_| "visual_selection_index_invalid")?
            .len();
    let mut output_offset = 0_u64;
    let mut selected_count = 0_u64;
    let mut reason_counts = BTreeMap::<String, u64>::new();
    select_verified_inventory(
        BufReader::new(inventory_file),
        &inventory_ref,
        &descriptor,
        &inventory_index,
        |selected| {
            emit_selection(
                selected,
                &mut writer,
                &mut offsets,
                &mut index_bytes_estimate,
                &mut output_offset,
                &mut selected_count,
                &mut reason_counts,
            )
        },
    )?;
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

/// The descriptor reference must come from the engine's completed decoder pass.
/// Hash the same bytes we parse, so a file change after path() verification cannot
/// substitute rows. Staged selection output is published only after this returns.
fn select_verified_inventory<R: BufRead>(
    mut reader: R,
    reference: &ArtifactRef,
    descriptor: &Value,
    index: &Value,
    mut emit: impl FnMut(Value) -> Result<(), &'static str>,
) -> Result<(), &'static str> {
    let frame_count = descriptor["frameCount"]
        .as_u64()
        .ok_or("visual_selection_descriptor_invalid")?;
    let first_pts = descriptor["firstPts"]
        .as_str()
        .and_then(|s| s.parse::<i64>().ok())
        .ok_or("visual_selection_descriptor_invalid")?;
    let mut core = SelectorCore::new(
        descriptor["timeBaseNumerator"].as_u64().unwrap_or(0),
        descriptor["timeBaseDenominator"].as_u64().unwrap_or(0),
    )?;
    let index_offsets = index["offsets"]
        .as_array()
        .ok_or("visual_selection_inventory_index_invalid")?;
    if index.as_object().is_none_or(|object| object.len() != 3)
        || index["schemaVersion"] != 2
        || index["kind"] != "media_frame_index"
        || index_offsets.len() as u64 != frame_count.div_ceil(32)
    {
        return Err("visual_selection_inventory_index_invalid");
    }
    let mut hash = Sha256::new();
    let mut bytes = 0_u64;
    let mut line = Vec::new();
    for frame_index in 0..frame_count {
        if frame_index % 32 == 0 {
            let expected = json!({"frameIndex":frame_index,"byteOffset":bytes});
            if index_offsets[(frame_index / 32) as usize] != expected {
                return Err("visual_selection_inventory_index_invalid");
            }
        }
        line.clear();
        let read = reader
            .by_ref()
            .take(MAX_INVENTORY_LINE_BYTES + 1)
            .read_until(b'\n', &mut line)
            .map_err(|_| "visual_selection_inventory_read_failed")?;
        if read == 0 {
            return Err("visual_selection_inventory_short");
        }
        if read as u64 > MAX_INVENTORY_LINE_BYTES || line.last() != Some(&b'\n') {
            return Err("visual_selection_inventory_row_invalid");
        }
        bytes = bytes
            .checked_add(read as u64)
            .filter(|n| *n <= reference.bytes)
            .ok_or("visual_selection_inventory_content_mismatch")?;
        hash.update(&line);
        let row: Value =
            serde_json::from_slice(&line).map_err(|_| "visual_selection_inventory_row_invalid")?;
        validate_row(&row, frame_index)?;
        if frame_index == 0
            && row["pts"].as_str().and_then(|s| s.parse::<i64>().ok()) != Some(first_pts)
        {
            return Err("visual_selection_inventory_first_pts_mismatch");
        }
        for selected in core.push(row)? {
            emit(selected)?;
        }
    }
    let mut extra = [0_u8; 1];
    if reader
        .read(&mut extra)
        .map_err(|_| "visual_selection_inventory_read_failed")?
        != 0
    {
        return Err("visual_selection_inventory_extra");
    }
    if bytes != reference.bytes || format!("{:x}", hash.finalize()) != reference.sha256 {
        return Err("visual_selection_inventory_content_mismatch");
    }
    for selected in core.finish()? {
        emit(selected)?;
    }
    Ok(())
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
    let identity = &descriptor["sourceIdentity"];
    if identity.as_object().is_none_or(|object| object.len() != 4)
        || identity["account"]
            .as_str()
            .is_none_or(|s| s.trim().is_empty() || s.len() > 256)
        || identity["postKey"]
            .as_str()
            .is_none_or(|s| s.trim().is_empty() || s.len() > 1024)
        || identity["durationMs"].as_u64().is_none()
        || identity["mediaSha256"] != source.sha256
        || descriptor["firstPts"]
            .as_str()
            .and_then(|s| s.parse::<i64>().ok())
            .is_none()
        || descriptor["timeBaseNumerator"]
            .as_u64()
            .is_none_or(|n| n == 0)
        || descriptor["timeBaseDenominator"]
            .as_u64()
            .is_none_or(|n| n == 0)
    {
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
    width
        .checked_mul(height)
        .filter(|n| *n <= MAX_FRAME_PIXELS)
        .and_then(|n| n.checked_mul(3))
        .filter(|n| *n > 0 && *n <= MAX_FRAME_BYTES)
        .ok_or("visual_selection_dimensions_invalid")?;
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

struct SelectorCore {
    numerator: u64,
    denominator: u64,
    first_pts: Option<i64>,
    last_pts: Option<i64>,
    seen: u64,
    bucket: Option<Bucket>,
}
struct Bucket {
    second: u128,
    midpoint: Value,
    midpoint_distance: u128,
    end: Value,
}

impl SelectorCore {
    fn new(numerator: u64, denominator: u64) -> Result<Self, &'static str> {
        if numerator == 0 || denominator == 0 {
            return Err("visual_selection_timebase_invalid");
        }
        Ok(Self {
            numerator,
            denominator,
            first_pts: None,
            last_pts: None,
            seen: 0,
            bucket: None,
        })
    }

    fn push(&mut self, row: Value) -> Result<Vec<Value>, &'static str> {
        validate_row(&row, self.seen)?;
        let pts = row["pts"].as_str().unwrap().parse::<i64>().unwrap();
        if self.last_pts.is_some_and(|last| pts < last) {
            return Err("visual_selection_timestamp_order_invalid");
        }
        let first = *self.first_pts.get_or_insert(pts);
        let scaled = u128::try_from(i128::from(pts) - i128::from(first))
            .ok()
            .and_then(|delta| delta.checked_mul(u128::from(self.numerator)))
            .ok_or("visual_selection_timestamp_overflow")?;
        let den = u128::from(self.denominator);
        let timestamp = scaled
            .checked_mul(1000)
            .map(|n| n / den)
            .and_then(|n| u64::try_from(n).ok())
            .ok_or("visual_selection_timestamp_overflow")?;
        if row["timestampMs"] != timestamp {
            return Err("visual_selection_timestamp_mismatch");
        }
        let second = scaled / den;
        // Compare exact rational distance to .5s without rounding to milliseconds.
        let distance = ((scaled % den) * 2).abs_diff(den);
        let mut emitted = Vec::new();
        if self
            .bucket
            .as_ref()
            .is_some_and(|bucket| bucket.second != second)
        {
            emitted = self.flush(false);
        }
        match &mut self.bucket {
            Some(bucket) => {
                // Equal-distance ties retain the earlier inventory frame.
                if distance < bucket.midpoint_distance {
                    bucket.midpoint = row.clone();
                    bucket.midpoint_distance = distance;
                }
                // The latest frame is nearest the open right boundary, including
                // duplicate-PTS frames. This also preserves the actual final frame.
                bucket.end = row;
            }
            None => {
                self.bucket = Some(Bucket {
                    second,
                    midpoint: row.clone(),
                    midpoint_distance: distance,
                    end: row,
                })
            }
        }
        self.last_pts = Some(pts);
        self.seen = self
            .seen
            .checked_add(1)
            .ok_or("visual_selection_too_large")?;
        Ok(emitted)
    }

    fn flush(&mut self, final_bucket: bool) -> Vec<Value> {
        let Some(bucket) = self.bucket.take() else {
            return Vec::new();
        };
        let mut mid = bucket.midpoint;
        let mut end = bucket.end;
        let end_reasons = if final_bucket {
            json!(["second_end", "last"])
        } else {
            json!(["second_end"])
        };
        if mid["frameIndex"] == end["frameIndex"] {
            mid["reasons"] = if final_bucket {
                json!(["second_midpoint", "second_end", "last"])
            } else {
                json!(["second_midpoint", "second_end"])
            };
            vec![mid]
        } else {
            mid["reasons"] = json!(["second_midpoint"]);
            end["reasons"] = end_reasons;
            vec![mid, end]
        }
    }

    fn finish(&mut self) -> Result<Vec<Value>, &'static str> {
        if self.seen == 0 {
            return Err("visual_selection_empty");
        }
        Ok(self.flush(true))
    }
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

/// Frozen historical descriptor. This does not select new work or relabel old evidence.
pub(crate) fn legacy_policy() -> Value {
    let mut value = json!({"version":2,"metricColor":"BT601-grayscale-nearest","metricLongEdgeMax":640,
        "tileSize":16,"edgeThreshold":24,"baselineIntervalMs":500,
        "localFlipPermille":90,"localEdgeDensityDeltaPermille":70,
        "localVsMedian":3,"localStableFlipPermille":20,
        "localStableMeanDelta":4,"localRequiresAdjacentTiles":true,
        "sceneBroadTilePermille":150,"sceneBroadPercent":40,
        "sceneHistogramTvPermille":180,
        "pulseEdgeFlipPermille":120,"pulseEdgeGainPermille":60,
        "pulseReturnFlipPermille":50,
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

pub(crate) fn policy() -> Value {
    let mut value = json!({
        "version":3,"id":POLICY_ID,"bucketDurationMs":1000,"maximumFramesPerBucket":2,
        "timeBasis":"exact_pts_relative_to_first_frame","bucketBounds":"left_closed_right_open",
        "midpointMs":500,"midpointTieBreak":"earlier_frame_index",
        "endTarget":"right_boundary_from_below","endTieBreak":"later_frame_index",
        "emptyBuckets":"skip","partialFinalBucket":"same_targets_preserve_final_frame",
        "deduplication":"same_frame_index_only","adaptiveExtras":false,
        "coverageUncertainty":"Brief text or visual changes between sampled frames can be missed.",
        "reasonEnums":["second_midpoint","second_end","last"],
        "selectionIndexOffsetKey":"frameIndex","selectedFramesAreOriginalRgb24":true
    });
    value["sha256"] = json!(format!(
        "{:x}",
        Sha256::digest(value.to_string().as_bytes())
    ));
    value
}

#[cfg(test)]
mod tests {
    use super::*;
    fn row(index: u64, pts: i64, timestamp: u64) -> Value {
        json!({"frameIndex":index,"pts":pts.to_string(),"timestampMs":timestamp,
            "pixelSha256":format!("{:x}",Sha256::digest(index.to_le_bytes()))})
    }
    fn sample(pts: &[i64], num: u64, den: u64) -> Vec<Value> {
        let mut core = SelectorCore::new(num, den).unwrap();
        let mut selected = Vec::new();
        for (i, &pts_value) in pts.iter().enumerate() {
            let time = ((i128::from(pts_value) - i128::from(pts[0])) * i128::from(num) * 1000
                / i128::from(den)) as u64;
            selected.extend(core.push(row(i as u64, pts_value, time)).unwrap());
        }
        selected.extend(core.finish().unwrap());
        selected
    }
    fn indices(rows: &[Value]) -> Vec<u64> {
        rows.iter()
            .map(|r| r["frameIndex"].as_u64().unwrap())
            .collect()
    }
    fn inventory_fixture(rows: &[Value]) -> (Vec<u8>, ArtifactRef, Value, Value) {
        let mut bytes = Vec::new();
        let mut offsets = Vec::new();
        for (i, row) in rows.iter().enumerate() {
            if i % 32 == 0 {
                offsets.push(json!({"frameIndex":i,"byteOffset":bytes.len()}));
            }
            bytes.extend(serde_json::to_vec(row).unwrap());
            bytes.push(b'\n');
        }
        let reference = ArtifactRef {
            sha256: format!("{:x}", Sha256::digest(&bytes)),
            bytes: bytes.len() as u64,
        };
        let source = ArtifactRef {
            sha256: "b".repeat(64),
            bytes: 5,
        };
        let descriptor = json!({"kind":"media_frame_inventory","schemaVersion":2,
            "source":source.to_json(),"inventory":reference.to_json(),"index":source.to_json(),
            "sourceIdentity":{"account":"offline-test","postKey":"post","mediaSha256":source.sha256,"durationMs":2000},
            "width":16,"height":16,"frameCount":rows.len(),"firstPts":"0",
            "timeBaseNumerator":1,"timeBaseDenominator":1000,"pixelFormat":"rgb24",
            "decoderContractSha256":format!("{:x}",Sha256::digest(DECODER_CONTRACT.as_bytes()))});
        let index = json!({"schemaVersion":2,"kind":"media_frame_index","offsets":offsets});
        (bytes, reference, descriptor, index)
    }
    fn selected_inventory(
        bytes: &[u8],
        reference: &ArtifactRef,
        descriptor: &Value,
        index: &Value,
    ) -> Result<Vec<Value>, &'static str> {
        let mut selected = Vec::new();
        select_verified_inventory(
            std::io::Cursor::new(bytes),
            reference,
            descriptor,
            index,
            |row| {
                selected.push(row);
                Ok(())
            },
        )?;
        Ok(selected)
    }
    #[test]
    fn verified_stream_matches_existing_selector_rows_and_rejects_late_tamper() {
        let pts: Vec<i64> = (0..100).map(|i| i * 33).collect();
        let rows: Vec<Value> = pts
            .iter()
            .enumerate()
            .map(|(i, &p)| row(i as u64, p, p as u64))
            .collect();
        let (bytes, reference, descriptor, index) = inventory_fixture(&rows);
        assert_eq!(
            selected_inventory(&bytes, &reference, &descriptor, &index).unwrap(),
            sample(&pts, 1, 1000)
        );
        // Simulate bytes changed after the initial CAS path verification: even an
        // unselected frame's otherwise-valid hash must invalidate the whole pass.
        let mut tampered = rows.clone();
        tampered[0]["pixelSha256"] = json!("e".repeat(64));
        let (changed, _, _, _) = inventory_fixture(&tampered);
        assert_eq!(
            selected_inventory(&changed, &reference, &descriptor, &index).unwrap_err(),
            "visual_selection_inventory_content_mismatch"
        );
        let mut wrong_size = reference.clone();
        wrong_size.bytes += 1;
        assert!(selected_inventory(&bytes, &wrong_size, &descriptor, &index).is_err());
    }
    #[test]
    fn verified_stream_rejects_index_count_and_first_pts_mismatch() {
        let rows: Vec<Value> = (0..65).map(|i| row(i, i as i64 * 33, i * 33)).collect();
        let (bytes, reference, descriptor, index) = inventory_fixture(&rows);
        for field in ["byteOffset", "frameIndex"] {
            let mut bad = index.clone();
            bad["offsets"][1][field] = json!(1);
            assert!(selected_inventory(&bytes, &reference, &descriptor, &bad).is_err());
        }
        let mut bad = index.clone();
        bad["offsets"].as_array_mut().unwrap().pop();
        assert!(selected_inventory(&bytes, &reference, &descriptor, &bad).is_err());
        let mut bad = index.clone();
        bad["schemaVersion"] = json!(1);
        assert!(selected_inventory(&bytes, &reference, &descriptor, &bad).is_err());
        for count in [0, 64, 66] {
            let mut bad = descriptor.clone();
            bad["frameCount"] = json!(count);
            assert!(selected_inventory(&bytes, &reference, &bad, &index).is_err());
        }
        let mut bad = descriptor.clone();
        bad["firstPts"] = json!("1");
        assert!(selected_inventory(&bytes, &reference, &bad, &index).is_err());
    }
    #[test]
    fn verified_stream_rejects_row_schema_pts_order_timestamp_and_line_failures() {
        let rows = vec![row(0, 0, 0), row(1, 33, 33), row(2, 66, 66)];
        for (key, value) in [
            ("unexpected", json!(true)),
            ("frameIndex", json!(9)),
            ("pts", json!("-1")),
            ("timestampMs", json!(34)),
            ("pixelSha256", json!("bad")),
        ] {
            let mut bad = rows.clone();
            bad[1][key] = value;
            let (bytes, reference, descriptor, index) = inventory_fixture(&bad);
            assert!(
                selected_inventory(&bytes, &reference, &descriptor, &index).is_err(),
                "{key}"
            );
        }
        let (bytes, reference, descriptor, index) = inventory_fixture(&rows);
        assert!(
            selected_inventory(&bytes[..bytes.len() - 1], &reference, &descriptor, &index).is_err()
        );
        let mut extra = bytes.clone();
        extra.extend(b"{}\n");
        assert!(selected_inventory(&extra, &reference, &descriptor, &index).is_err());
        let oversized = vec![b' '; MAX_INVENTORY_LINE_BYTES as usize + 1];
        assert!(selected_inventory(&oversized, &reference, &descriptor, &index).is_err());
    }
    #[test]
    fn descriptor_dimensions_identity_and_decoder_contract_remain_strict() {
        let (_, _, descriptor, _) = inventory_fixture(&[row(0, 0, 0)]);
        assert!(validate_descriptor(&descriptor).is_ok());
        for (key, value) in [
            ("width", json!(0)),
            ("height", json!(u64::MAX)),
            ("frameCount", json!(0)),
            ("timeBaseNumerator", json!(0)),
            ("timeBaseDenominator", json!(0)),
            ("firstPts", json!("invalid")),
            ("pixelFormat", json!("gray")),
            ("decoderContractSha256", json!("a".repeat(64))),
        ] {
            let mut bad = descriptor.clone();
            bad[key] = value;
            assert!(validate_descriptor(&bad).is_err(), "{key}");
        }
        for (key, value) in [
            ("account", json!("")),
            ("postKey", json!(null)),
            ("mediaSha256", json!("c".repeat(64))),
            ("durationMs", json!(-1)),
            ("unexpected", json!(1)),
        ] {
            let mut bad = descriptor.clone();
            bad["sourceIdentity"][key] = value;
            assert!(validate_descriptor(&bad).is_err(), "{key}");
        }
    }
    async fn stored_fixture() -> (
        tempfile::TempDir,
        ArtifactStore,
        std::path::PathBuf,
        std::path::PathBuf,
        Value,
    ) {
        let temp = tempfile::tempdir().unwrap();
        let store = ArtifactStore::open(&temp.path().join("artifacts")).unwrap();
        let source = temp.path().join("source.mp4");
        std::fs::write(&source, b"offline source bytes").unwrap();
        let tool = temp.path().join("not-an-executable.exe");
        std::fs::write(&tool, b"must never execute").unwrap();
        let rows: Vec<Value> = (0..60).map(|i| row(i, i as i64 * 33, i * 33)).collect();
        let (bytes, _, mut descriptor, index) = inventory_fixture(&rows);
        let source_ref = store.put_file(&source).unwrap();
        descriptor["source"] = source_ref.to_json();
        descriptor["sourceIdentity"]["mediaSha256"] = json!(source_ref.sha256);
        descriptor["inventory"] = store.put_bytes(&bytes).unwrap().to_json();
        descriptor["index"] = store
            .put_bytes(&serde_json::to_vec(&index).unwrap())
            .unwrap()
            .to_json();
        let descriptor_ref = store
            .put_bytes(&serde_json::to_vec(&descriptor).unwrap())
            .unwrap()
            .to_json();
        (temp, store, source, tool, descriptor_ref)
    }
    #[tokio::test]
    async fn fixed_selection_uses_no_decoder_and_has_identical_output_artifacts() {
        let (_temp, store, source, tool, descriptor_ref) = stored_fixture().await;
        let selected_ref = select(&tool, &source, &descriptor_ref, &store)
            .await
            .unwrap();
        let selected_ref = ArtifactRef::from_json(&selected_ref).unwrap();
        let descriptor: Value = serde_json::from_slice(
            &store
                .read_bytes(&selected_ref, MAX_DESCRIPTOR_BYTES)
                .unwrap(),
        )
        .unwrap();
        let mut expected = sample(&(0..60).map(|i| i * 33).collect::<Vec<_>>(), 1, 1000);
        let mut reasons = BTreeMap::<String, u64>::new();
        for (i, row) in expected.iter_mut().enumerate() {
            row["selectionIndex"] = json!(i);
            for reason in row["reasons"].as_array().unwrap() {
                *reasons.entry(reason.as_str().unwrap().into()).or_default() += 1;
            }
        }
        let expected_rows = store.write_jsonl(expected).unwrap();
        let expected_index=store.put_bytes(&serde_json::to_vec(&json!({"schemaVersion":2,"kind":"media_frame_index","offsets":[{"frameIndex":0,"byteOffset":0}]})).unwrap()).unwrap();
        let expected_descriptor = json!({"schemaVersion":2,"kind":"media_frame_selection","inventoryDescriptor":descriptor_ref,
            "selection":expected_rows.to_json(),"selectionIndex":expected_index.to_json(),"selectedCount":4,
            "frameCount":60,"policy":policy(),"reasonCounts":reasons});
        assert_eq!(descriptor, expected_descriptor);
        assert_eq!(
            selected_ref,
            store
                .put_bytes(&serde_json::to_vec(&expected_descriptor).unwrap())
                .unwrap()
        );
    }
    #[tokio::test]
    async fn stored_descriptor_inventory_index_and_source_tampering_fail_closed() {
        for target in ["descriptor", "inventory", "index", "source", "source_file"] {
            let (_temp, store, source, tool, descriptor_ref) = stored_fixture().await;
            let reference = ArtifactRef::from_json(&descriptor_ref).unwrap();
            let descriptor: Value = serde_json::from_slice(
                &store.read_bytes(&reference, MAX_DESCRIPTOR_BYTES).unwrap(),
            )
            .unwrap();
            let path = match target {
                "descriptor" => store.path(&reference).unwrap(),
                "source_file" => source.clone(),
                _ => store
                    .path(&ArtifactRef::from_json(&descriptor[target]).unwrap())
                    .unwrap(),
            };
            std::fs::write(path, b"tampered").unwrap();
            assert!(
                select(&tool, &source, &descriptor_ref, &store)
                    .await
                    .is_err(),
                "{target}"
            );
        }
    }
    #[test]
    fn cfr_selects_midpoint_and_end_without_first_or_adaptive_extras() {
        let pts: Vec<i64> = (0..60).collect();
        let selected = sample(&pts, 1, 30);
        assert_eq!(indices(&selected), vec![15, 29, 45, 59]);
        assert_eq!(
            selected.last().unwrap()["reasons"],
            json!(["second_end", "last"])
        );
    }
    #[test]
    fn vfr_nearest_midpoint_is_exact_and_ties_choose_earlier() {
        assert_eq!(
            indices(&sample(
                &[0, 400, 600, 990, 1000, 1400, 1500, 1970],
                1,
                1000
            )),
            vec![1, 3, 6, 7]
        );
        // Millisecond flooring would tie these distances and pick the wrong frame.
        assert_eq!(
            indices(&sample(&[0, 499100, 500100, 999999], 1, 1000000)),
            vec![2, 3]
        );
    }
    #[test]
    fn short_single_frame_and_partial_final_bucket_deduplicate_actual_frame() {
        assert_eq!(indices(&sample(&[0], 1, 30)), vec![0]);
        assert_eq!(indices(&sample(&[0, 33, 66], 1, 1000)), vec![2]);
        assert_eq!(
            indices(&sample(&[0, 500, 999, 1000, 1100], 1, 1000)),
            vec![1, 2, 4]
        );
    }
    #[test]
    fn empty_seconds_are_skipped_and_exact_boundary_starts_new_bucket() {
        assert_eq!(
            indices(&sample(&[0, 500, 999, 1000, 4500, 4999], 1, 1000)),
            vec![1, 2, 3, 4, 5]
        );
    }
    #[test]
    fn duplicate_pts_keep_final_actual_frame_and_distinct_indices() {
        let selected = sample(&[-900, -400, -400], 1, 1000);
        assert_eq!(indices(&selected), vec![1, 2]);
        assert_eq!(selected[0]["pts"], "-400");
        assert_eq!(selected[1]["pixelSha256"], row(2, -400, 500)["pixelSha256"]);
    }
    #[test]
    fn selected_rows_preserve_exact_inventory_identity_and_per_bucket_bound() {
        let pts: Vec<i64> = (0..1000).map(|i| i * 13).collect();
        let selected = sample(&pts, 1, 1000);
        let mut counts = BTreeMap::new();
        for selected_row in &selected {
            let i = selected_row["frameIndex"].as_u64().unwrap();
            let mut original = row(i, pts[i as usize], pts[i as usize] as u64);
            original["reasons"] = selected_row["reasons"].clone();
            assert_eq!(&original, selected_row);
            *counts
                .entry(selected_row["timestampMs"].as_u64().unwrap() / 1000)
                .or_insert(0) += 1;
        }
        assert!(counts.values().all(|n| *n <= 2));
        assert_eq!(selected.last().unwrap()["frameIndex"], 999);
    }
    #[test]
    fn invalid_time_order_inventory_and_timebase_fail_closed() {
        assert!(SelectorCore::new(0, 1).is_err());
        assert!(SelectorCore::new(1, 0).is_err());
        assert!(SelectorCore::new(1, 1000).unwrap().finish().is_err());
        let mut core = SelectorCore::new(1, 1000).unwrap();
        assert!(core.push(row(1, 0, 0)).is_err());
        core.push(row(0, 0, 0)).unwrap();
        assert!(core.push(row(1, 1, 2)).is_err());
        core.push(row(1, 1, 1)).unwrap();
        assert!(core.push(row(2, 0, 0)).is_err());
    }
    #[test]
    fn sub_millisecond_reverse_pts_and_overflow_are_rejected() {
        let mut core = SelectorCore::new(1, 1000000).unwrap();
        core.push(row(0, 0, 0)).unwrap();
        core.push(row(1, 2, 0)).unwrap();
        assert!(core.push(row(2, 1, 0)).is_err());
        let mut core = SelectorCore::new(u64::MAX, 1).unwrap();
        core.push(row(0, i64::MIN, 0)).unwrap();
        assert!(core.push(row(1, i64::MAX, 0)).is_err());
    }
    #[test]
    fn policy_is_versioned_hashed_and_disclaims_transient_coverage() {
        let mut descriptor = policy();
        assert_eq!(descriptor["id"], POLICY_ID);
        assert_eq!(descriptor["version"], 3);
        assert_eq!(descriptor["adaptiveExtras"], false);
        assert!(
            descriptor["coverageUncertainty"]
                .as_str()
                .unwrap()
                .contains("missed")
        );
        let sha = descriptor
            .as_object_mut()
            .unwrap()
            .remove("sha256")
            .unwrap();
        assert_eq!(
            sha,
            format!("{:x}", Sha256::digest(descriptor.to_string().as_bytes()))
        );
        assert!(descriptor.get("pulseEdgeFlipPermille").is_none());
    }
}
