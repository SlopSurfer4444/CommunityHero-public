//! Deterministic finite video sampling. This module has no process, model or DB
//! access. Native callers supply current company/member/asset authority.
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

pub(crate) const CONTRACT: &str = "communityhero.bounded-video-sample.rgb24-source-pts.v1";
pub(crate) const RANGE_PROFILE: &str = "bounded_time_range_v1";
pub(crate) const OVERVIEW_PROFILE: &str = "bounded_uniform_overview_v1";

pub(crate) fn digest(value: &Value) -> String {
    format!("{:x}", Sha256::digest(value.to_string().as_bytes()))
}
pub(crate) fn sha(value: &Value) -> bool {
    value.as_str().is_some_and(|s| {
        s.len() == 64
            && s.bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    })
}
pub(crate) fn uint(value: &Value, key: &str) -> Result<u64, String> {
    value[key]
        .as_u64()
        .ok_or_else(|| format!("frame_sample_{key}_invalid"))
}
fn bounded(value: &Value, key: &str, max: u64) -> Result<u64, String> {
    let n = uint(value, key)?;
    if n == 0 || n > max {
        return Err(format!("frame_sample_{key}_invalid"));
    }
    Ok(n)
}

/// Input envelope contains the immutable need pins plus native verified source
/// duration, an explicit finite profile, mandatory-base usage and transport cap.
/// `deadlineMs` is a duration, not fresh paid/retry authority.
pub(crate) fn plan_sample(input: &Value) -> Result<Value, String> {
    if input["schemaVersion"] != 1
        || !sha(&input["needSha256"])
        || input["needId"].as_str().is_none_or(str::is_empty)
        || input["companyId"].as_str().is_none_or(str::is_empty)
        || input["member"]["postId"].as_str().is_none_or(str::is_empty)
        || !input["member"]["connectorBinding"].is_object()
        || input["asset"]["attachmentIndex"].as_u64().is_none()
        || !sha(&input["asset"]["attachmentIdentity"])
        || !sha(&input["asset"]["sourceVersion"])
        || !sha(&input["asset"]["sourceArtifactSha256"])
    {
        return Err("frame_sample_binding_invalid".into());
    }
    let source =
        crate::media_artifacts::ArtifactRef::from_json(&input["asset"]["sourceArtifactRef"])
            .map_err(|_| "frame_sample_source_ref_invalid")?;
    crate::media_artifacts::ArtifactRef::from_json(&input["sourceProofRef"])
        .map_err(|_| "frame_sample_source_proof_invalid")?;
    if source.sha256 != input["asset"]["sourceArtifactSha256"]
        || source.bytes == 0
        || source.bytes > 500 * 1024 * 1024
    {
        return Err("frame_sample_source_identity_invalid".into());
    }
    let duration = bounded(input, "sourceDurationMs", 14_400_000)?;
    let profile = &input["profile"];
    if profile["version"] != 1 {
        return Err("frame_sample_profile_unsupported".into());
    }
    let frames = bounded(profile, "maxFrames", 8)?;
    bounded(profile, "windowMs", 10_000)?;
    bounded(profile, "maxImageBytes", 32 * 1024 * 1024)?;
    bounded(profile, "maxArtifactBytes", 512 * 1024 * 1024)?;
    bounded(profile, "maxTotalPixels", 192_000_000)?;
    bounded(profile, "maxDecodedDurationMs", 120_000)?;
    bounded(profile, "maxDecodedFrames", 10_000)?;
    bounded(profile, "maxPrerollMs", 30_000)?;
    bounded(profile, "deadlineMs", 120_000)?;
    let need = &input["requestedTimeOrIntent"];
    if need["timelineBasis"] != "relative_video_start" {
        return Err("frame_sample_time_basis_unsupported".into());
    }
    let mut targets = Vec::new();
    match need["kind"].as_str() {
        Some("known_range") if profile["id"] == RANGE_PROFILE => {
            let start = uint(need, "startMs")?;
            let end = uint(need, "endMs")?;
            let step = bounded(profile, "rangeStepMs", 30_000)?;
            if start > end || start >= duration || end > duration || end - start > 30_000 {
                return Err("frame_sample_range_invalid".into());
            }
            let count = (end - start).div_ceil(step).max(1);
            if count > frames {
                return Err("frame_sample_count_budget_exhausted".into());
            }
            for ordinal in 0..count {
                let target = start
                    .checked_add(
                        ordinal
                            .checked_mul(step)
                            .ok_or("frame_sample_range_overflow")?,
                    )
                    .ok_or("frame_sample_range_overflow")?;
                targets.push(json!({"targetIndex":ordinal,"requestedTimestampMs":target,
                    "windowEndMs":target.saturating_add(uint(profile,"windowMs")?).min(if start==end {duration} else {end})}));
            }
        }
        Some("uniform_overview") if profile["id"] == OVERVIEW_PROFILE => {
            let count = bounded(profile, "overviewFrames", 12)?;
            if count < 6 || count > frames || duration < count {
                return Err("frame_sample_overview_budget_exhausted".into());
            }
            // Midpoints of equal temporal buckets avoid pretending EOF is a
            // decodable frame. No semantic locator or scene analysis is used.
            for ordinal in 0..count {
                let target = ((u128::from(ordinal) * 2 + 1) * u128::from(duration)
                    / (u128::from(count) * 2)) as u64;
                targets.push(json!({"targetIndex":ordinal,"requestedTimestampMs":target,
                    "windowEndMs":target.saturating_add(uint(profile,"windowMs")?).min(duration)}));
            }
        }
        _ => return Err("frame_sample_selection_unsupported".into()),
    }
    let base = &input["baseUsage"];
    let transport = &input["transportLimits"];
    let count = uint(base, "imageCount")?
        .checked_add(targets.len() as u64)
        .ok_or("frame_sample_count_overflow")?;
    if count > bounded(transport, "maxImages", 16)?
        || uint(base, "imageBytes")? > bounded(transport, "maxBytes", 32 * 1024 * 1024)?
        || uint(base, "pixels")? > bounded(transport, "maxPixels", 256_000_000)?
    {
        return Err("frame_sample_transport_budget_exhausted".into());
    }
    // A lower bound includes one observed frame per window. Actual keyframe
    // preroll and decoded positions are enforced by the subprocess reader.
    if targets.len() as u64 > uint(profile, "maxDecodedFrames")? {
        return Err("frame_sample_decode_budget_exhausted".into());
    }
    let mut plan = json!({"schemaVersion":1,"kind":"bounded_video_sample_plan",
        "decoderContractVersion":CONTRACT,"needId":input["needId"],"needSha256":input["needSha256"],
        "companyId":input["companyId"],"member":input["member"],"asset":input["asset"],
        "sourceProofRef":input["sourceProofRef"],"sourceDurationMs":duration,
        "requestedTimeOrIntent":need,"profile":profile,"baseUsage":base,"transportLimits":transport,
        "targets":targets,"exhaustive":false});
    plan["planSha256"] = json!(digest(&plan));
    Ok(plan)
}

pub(crate) fn validate_plan(plan: &Value) -> Result<(), String> {
    if plan["kind"] != "bounded_video_sample_plan"
        || plan["decoderContractVersion"] != CONTRACT
        || plan["exhaustive"] != false
        || !sha(&plan["planSha256"])
    {
        return Err("frame_sample_plan_invalid".into());
    }
    let mut unsigned = plan.clone();
    unsigned
        .as_object_mut()
        .ok_or("frame_sample_plan_invalid")?
        .remove("planSha256");
    if plan["planSha256"] != digest(&unsigned) {
        return Err("frame_sample_plan_hash_changed".into());
    }
    let rebuilt = plan_sample(plan)?;
    if rebuilt != *plan {
        return Err("frame_sample_plan_changed".into());
    }
    Ok(())
}

/// Exact source seconds as a rational, signed milliseconds with truncation,
/// and elapsed offset from the probed video stream start. Never word timing.
pub(crate) fn source_time(pts: i64, num: u64, den: u64, start_pts: i64) -> Result<Value, String> {
    if num == 0 || den == 0 || pts < start_pts {
        return Err("frame_sample_time_invalid".into());
    }
    let numerator = i128::from(pts)
        .checked_mul(i128::from(num))
        .ok_or("frame_sample_time_overflow")?;
    let source_ms = numerator
        .checked_mul(1000)
        .map(|n| n / i128::from(den))
        .and_then(|n| i64::try_from(n).ok())
        .ok_or("frame_sample_time_overflow")?;
    let offset = (i128::from(pts) - i128::from(start_pts))
        .checked_mul(i128::from(num))
        .and_then(|n| n.checked_mul(1000))
        .map(|n| n / i128::from(den))
        .and_then(|n| u64::try_from(n).ok())
        .ok_or("frame_sample_time_overflow")?;
    Ok(
        json!({"pts":pts.to_string(),"timeBase":{"numerator":num,"denominator":den},
        "sourceTimestamp":{"numerator":numerator.to_string(),"denominator":den.to_string(),"unit":"seconds"},
        "sourceTimestampMs":source_ms,"timelineOffsetMs":offset,"timestampMs":offset,
        "timestampBasis":"relative_video_stream_start","videoStartPts":start_pts.to_string(),
        "millisecondRounding":"truncate_toward_zero"}),
    )
}

#[cfg(test)]
#[path = "media_frame_sample_tests.rs"]
mod tests;
