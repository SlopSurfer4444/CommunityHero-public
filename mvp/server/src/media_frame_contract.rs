//! Version 2 contract for a bounded, complete decoded-frame vision chunk.
//! This module validates data only; it never reads media or calls a provider.

use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;

pub(crate) const VERSION: u64 = 2;
pub(crate) const MAX_CHUNK_POSITIONS: u64 = 32;
pub(crate) const MAX_CHUNK_IMAGES: usize = 32;

const REQUEST_KEYS: &[&str] = &[
    "schemaVersion",
    "workId",
    "createdAtUtc",
    "source",
    "inventory",
    "chunk",
    "frames",
    "manifestSha256",
];
const SOURCE_KEYS: &[&str] = &["account", "postKey", "mediaSha256", "durationMs"];
const INVENTORY_KEYS: &[&str] = &[
    "sha256",
    "frameCount",
    "selectionSha256",
    "selectionPolicySha256",
    "selectedFrameCount",
    "decoderContractSha256",
    "timeBaseNumerator",
    "timeBaseDenominator",
    "width",
    "height",
    "pixelFormat",
];
const CHUNK_KEYS: &[&str] = &[
    "firstSelectionIndex",
    "endSelectionIndexExclusive",
    "previousReceiptSha256",
    "leaseId",
];
const REQUEST_FRAME_KEYS: &[&str] = &[
    "id",
    "frameIndex",
    "selectionIndex",
    "selectionReasons",
    "pts",
    "timestampMs",
    "pixelSha256",
    "sha256",
    "path",
    "mimeType",
];
const RESPONSE_KEYS: &[&str] = &[
    "schemaVersion",
    "status",
    "source",
    "inventory",
    "chunk",
    "manifestSha256",
    "frames",
    "summary",
    "provenance",
];
const OBSERVATION_KEYS: &[&str] = &[
    "id",
    "frameIndex",
    "selectionIndex",
    "selectionReasons",
    "pts",
    "timestampMs",
    "pixelSha256",
    "sha256",
    "status",
    "scene",
    "text",
    "numbers",
    "uncertainties",
];
const NUMBER_KEYS: &[&str] = &["raw", "value", "unit", "currency", "uncertain"];
const PROVENANCE_KEYS: &[&str] = &["backend", "model", "instructionSha256"];

/// Bind the exact Rust-side request. An existing manifest field is replaced;
/// other unexpected fields remain and cause validation to fail.
pub(crate) fn seal_request(mut request: Value) -> Value {
    let Some(object) = request.as_object_mut() else {
        return request;
    };
    object.remove("manifestSha256");
    let hash = digest(&request);
    request["manifestSha256"] = Value::String(hash);
    request
}

/// Returns only the accepted, exact-schema response. An incomplete response
/// must be held by the caller and cannot become durable completed evidence.
pub(crate) fn validate_response(request: &Value, response: &Value) -> Result<Value, &'static str> {
    validate_request(request)?;
    if !exact(response, RESPONSE_KEYS)
        || response["schemaVersion"] != VERSION
        || response["status"] != "complete"
        || response["source"] != request["source"]
        || response["inventory"] != request["inventory"]
        || response["chunk"] != request["chunk"]
        || response["manifestSha256"] != request["manifestSha256"]
        || !nonempty(&response["summary"], 16_000)
    {
        return Err("visual_chunk_response_incomplete");
    }
    let provenance = &response["provenance"];
    if !valid_provenance(provenance, &request["frames"]) {
        return Err("visual_chunk_provenance_invalid");
    }
    let expected = request["frames"]
        .as_array()
        .ok_or("visual_chunk_request_invalid")?;
    let actual = response["frames"]
        .as_array()
        .ok_or("visual_chunk_frames_incomplete")?;
    if actual.len() != expected.len() {
        return Err("visual_chunk_frames_incomplete");
    }
    let mut seen = BTreeSet::new();
    for frame in actual {
        validate_observation(frame)?;
        let id = frame["id"].as_str().ok_or("visual_chunk_frame_invalid")?;
        if !seen.insert(id) {
            return Err("visual_chunk_frame_duplicate");
        }
        let original = expected
            .iter()
            .find(|candidate| candidate["id"] == frame["id"])
            .ok_or("visual_chunk_frame_unknown")?;
        for key in [
            "id",
            "frameIndex",
    "selectionIndex",
    "selectionReasons",
            "pts",
            "timestampMs",
            "pixelSha256",
            "sha256",
        ] {
            if frame[key] != original[key] {
                return Err("visual_chunk_frame_binding_changed");
            }
        }
    }
    Ok(response.clone())
}

fn valid_provenance(value: &Value, frames: &Value) -> bool {
    // Historical single-model receipts retain their exact original contract.
    if exact(value, PROVENANCE_KEYS) {
        return nonempty(&value["backend"], 128) && nonempty(&value["model"], 256)
            && sha(&value["instructionSha256"]);
    }
    if !exact(value, &["schemaVersion", "kind", "frames", "rescue"])
        || value["schemaVersion"] != 2 || value["kind"] != "mixed_frames"
        || !exact(&value["rescue"], &["permitSha256", "cloudFrameCount", "cloudInvocationCount"])
        || !sha(&value["rescue"]["permitSha256"]) { return false; }
    let Some(expected) = frames.as_array() else { return false; };
    let Some(actual) = value["frames"].as_array() else { return false; };
    if actual.len() != expected.len() { return false; }
    let mut seen = BTreeSet::new(); let mut cloud = 0_u64; let mut instruction = None;
    for entry in actual {
        if !exact(entry, &["id", "backend", "model", "instructionSha256"])
            || !sha(&entry["instructionSha256"]) { return false; }
        let Some(id) = entry["id"].as_str() else { return false; };
        if !seen.insert(id) || !expected.iter().any(|frame| frame["id"] == id) { return false; }
        let current = entry["instructionSha256"].as_str().unwrap();
        if instruction.is_some_and(|prior| prior != current) { return false; }
        instruction = Some(current);
        match entry["backend"].as_str() {
            Some("codex_isolated") if entry["model"] == "gpt-6-luna" || entry["model"] == crate::codex_model_policy::MODEL => cloud += 1,
            Some("local_ollama") => {
                let Some((model, digest)) = entry["model"].as_str().and_then(|m| m.split_once("@sha256:")) else { return false; };
                if model.is_empty() || model.len() > 128 || !sha(&Value::String(digest.into())) { return false; }
            }
            _ => return false,
        }
    }
    cloud > 0 && cloud <= 32 && value["rescue"]["cloudFrameCount"] == cloud
        && value["rescue"]["cloudInvocationCount"] == cloud.div_ceil(4)
}

/// Shape and semantic checks for a single path-free durable observation.
/// The caller must additionally bind it to a request or stored inventory.
pub(crate) fn validate_observation(frame: &Value) -> Result<(), &'static str> {
    if !exact(frame, OBSERVATION_KEYS)
        || !nonempty(&frame["id"], 128)
        || frame["frameIndex"].as_u64().is_none()
        || frame["selectionIndex"].as_u64().is_none() || !reasons(&frame["selectionReasons"])
        || !signed_i64_string(&frame["pts"])
        || frame["timestampMs"].as_u64().is_none()
        || !sha(&frame["pixelSha256"])
        || !sha(&frame["sha256"])
        || !matches!(frame["status"].as_str(), Some("readable" | "unreadable" | "none"))
        || !nonempty(&frame["scene"], 4000)
        || !strings(&frame["text"], 64)
        || !strings(&frame["uncertainties"], 32)
    {
        return Err("visual_chunk_frame_invalid");
    }
    let numbers = frame["numbers"]
        .as_array()
        .filter(|items| items.len() <= 64)
        .ok_or("visual_chunk_numbers_invalid")?;
    for number in numbers {
        if !exact(number, NUMBER_KEYS)
            || !nonempty(&number["raw"], 256)
            || !["value", "unit", "currency"]
                .iter()
                .all(|key| number[*key].is_null() || nonempty(&number[*key], 128))
            || number["uncertain"].as_bool().is_none()
        {
            return Err("visual_chunk_numbers_invalid");
        }
        if number["uncertain"] == true && !number["value"].is_null() {
            return Err("visual_chunk_uncertain_number_has_value");
        }
    }
    Ok(())
}

fn reasons(v:&Value)->bool{v.as_array().is_some_and(|a|!a.is_empty()&&a.len()<=8&&a.iter().all(|r|matches!(r.as_str(),Some("first"|"last"|"baseline"|"scene_before"|"scene_after"|"local_change_before"|"local_change_after"|"transient_pulse"|"second_midpoint"|"second_end"))))}

fn validate_request(request: &Value) -> Result<(), &'static str> {
    if !exact(request, REQUEST_KEYS)
        || request["schemaVersion"] != VERSION
        || !nonempty(&request["workId"], 128)
        || !nonempty(&request["createdAtUtc"], 64)
        || !sha(&request["manifestSha256"])
    {
        return Err("visual_chunk_request_invalid");
    }
    let mut unhashed = request.clone();
    unhashed
        .as_object_mut()
        .ok_or("visual_chunk_request_invalid")?
        .remove("manifestSha256");
    if request["manifestSha256"] != digest(&unhashed) {
        return Err("visual_chunk_manifest_hash_invalid");
    }
    let source = &request["source"];
    if !exact(source, SOURCE_KEYS)
        || !nonempty(&source["account"], 256)
        || !nonempty(&source["postKey"], 1024)
        || !sha(&source["mediaSha256"])
        || source["durationMs"].as_u64().is_none()
    {
        return Err("visual_chunk_source_invalid");
    }
    let inventory = &request["inventory"];
    if !exact(inventory, INVENTORY_KEYS)
        || !sha(&inventory["sha256"])
        || !sha(&inventory["decoderContractSha256"])
        || !sha(&inventory["selectionSha256"]) || !sha(&inventory["selectionPolicySha256"])
        || inventory["selectedFrameCount"].as_u64().is_none_or(|n|n==0||n>inventory["frameCount"].as_u64().unwrap_or(0))
        || inventory["frameCount"].as_u64().is_none_or(|n| n == 0)
        || inventory["timeBaseNumerator"]
            .as_u64()
            .is_none_or(|n| n == 0)
        || inventory["timeBaseDenominator"]
            .as_u64()
            .is_none_or(|n| n == 0)
        || inventory["width"].as_u64().is_none_or(|n| n == 0)
        || inventory["height"].as_u64().is_none_or(|n| n == 0)
        || inventory["pixelFormat"] != "rgb24"
    {
        return Err("visual_chunk_inventory_invalid");
    }
    let chunk = &request["chunk"];
    let first = chunk["firstSelectionIndex"]
        .as_u64()
        .ok_or("visual_chunk_range_invalid")?;
    let end = chunk["endSelectionIndexExclusive"]
        .as_u64()
        .ok_or("visual_chunk_range_invalid")?;
    if !exact(chunk, CHUNK_KEYS)
        || first >= end
        || end
            > inventory["selectedFrameCount"]
                .as_u64()
                .ok_or("visual_chunk_inventory_invalid")?
        || end - first > MAX_CHUNK_POSITIONS
        || !nonempty(&chunk["leaseId"], 128)
        || !(chunk["previousReceiptSha256"].is_null() || sha(&chunk["previousReceiptSha256"]))
    {
        return Err("visual_chunk_range_invalid");
    }
    let frames = request["frames"]
        .as_array()
        .ok_or("visual_chunk_frames_invalid")?;
    if frames.is_empty() || frames.len() > MAX_CHUNK_IMAGES {
        return Err("visual_chunk_frames_invalid");
    }
    let mut ids = BTreeSet::new();
    let mut positions = BTreeSet::new();
    for frame in frames {
        let index = frame["selectionIndex"]
            .as_u64()
            .ok_or("visual_chunk_frame_invalid")?;
        let id = frame["id"].as_str().ok_or("visual_chunk_frame_invalid")?;
        if !exact(frame, REQUEST_FRAME_KEYS)
            || !nonempty(&frame["id"], 128)
            || !ids.insert(id)
            || !positions.insert(index)
            || frame["frameIndex"].as_u64().is_none_or(|n|n>=inventory["frameCount"].as_u64().unwrap_or(0))
            || !reasons(&frame["selectionReasons"])
            || index < first
            || index >= end
            || !signed_i64_string(&frame["pts"])
            || frame["timestampMs"].as_u64().is_none()
            || !sha(&frame["pixelSha256"])
            || !sha(&frame["sha256"])
            || !nonempty(&frame["path"], 4096)
            || frame["mimeType"] != "image/png"
        {
            return Err("visual_chunk_frame_invalid");
        }
    }
    Ok(())
}

fn exact(value: &Value, keys: &[&str]) -> bool {
    value.as_object().is_some_and(|object| {
        object.len() == keys.len() && object.keys().all(|key| keys.contains(&key.as_str()))
    })
}

fn nonempty(value: &Value, max: usize) -> bool {
    value
        .as_str()
        .is_some_and(|text| !text.trim().is_empty() && text.len() <= max)
}

fn strings(value: &Value, max: usize) -> bool {
    value.as_array().is_some_and(|items| {
        items.len() <= max
            && items
                .iter()
                .all(|item| item.as_str().is_some_and(|text| text.len() <= 4000))
    })
}

fn sha(value: &Value) -> bool {
    value.as_str().is_some_and(|text| {
        text.len() == 64
            && text
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    })
}

fn signed_i64_string(value: &Value) -> bool {
    value
        .as_str()
        .is_some_and(|text| !text.is_empty() && text.len() <= 20 && text.parse::<i64>().is_ok())
}

fn digest(value: &Value) -> String {
    format!("{:x}", Sha256::digest(canonical_json(value).as_bytes()))
}

/// Canonicalize recursively even if another dependency enables serde_json's
/// preserve_order feature. JSON numbers and strings use serde_json escaping.
fn canonical_json(value: &Value) -> String {
    match value {
        Value::Array(items) => {
            let mut encoded = String::from("[");
            for (index, item) in items.iter().enumerate() {
                if index != 0 {
                    encoded.push(',');
                }
                encoded.push_str(&canonical_json(item));
            }
            encoded.push(']');
            encoded
        }
        Value::Object(object) => {
            let mut entries: Vec<_> = object.iter().collect();
            entries.sort_unstable_by(|left, right| left.0.cmp(right.0));
            let mut encoded = String::from("{");
            for (index, (key, item)) in entries.into_iter().enumerate() {
                if index != 0 {
                    encoded.push(',');
                }
                encoded.push_str(&serde_json::to_string(key).expect("JSON object key"));
                encoded.push(':');
                encoded.push_str(&canonical_json(item));
            }
            encoded.push('}');
            encoded
        }
        primitive => primitive.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn request() -> Value {
        seal_request(json!({
            "schemaVersion": 2, "workId": "video-work", "createdAtUtc": "2026-09-23T00:00:00Z",
            "source": {"account":"LikeAvto","postKey":"post","mediaSha256":"a".repeat(64),"durationMs":12000},
            "inventory": {"sha256":"b".repeat(64),"frameCount":100,"selectedFrameCount":100,"selectionSha256":"4".repeat(64),"selectionPolicySha256":"5".repeat(64),"decoderContractSha256":"c".repeat(64),
                "timeBaseNumerator":1,"timeBaseDenominator":1000,"width":1920,"height":1080,"pixelFormat":"rgb24"},
            "chunk": {"firstSelectionIndex":64,"endSelectionIndexExclusive":96,"previousReceiptSha256":null,"leaseId":"lease"},
            "frames": [
                {"id":"f64","frameIndex":64,"selectionIndex":64,"selectionReasons":["baseline"],"pts":"-1","timestampMs":0,"pixelSha256":"d".repeat(64),"sha256":"e".repeat(64),"path":"private/64.png","mimeType":"image/png"},
                {"id":"f95","frameIndex":95,"selectionIndex":95,"selectionReasons":["baseline"],"pts":"125","timestampMs":125,"pixelSha256":"f".repeat(64),"sha256":"1".repeat(64),"path":"private/95.png","mimeType":"image/png"}
            ]
        }))
    }

    fn response(request: &Value) -> Value {
        let frames: Vec<Value> = request["frames"].as_array().unwrap().iter().map(|frame| json!({
            "id": frame["id"], "frameIndex": frame["frameIndex"], "selectionIndex":frame["selectionIndex"],"selectionReasons":frame["selectionReasons"], "pts": frame["pts"],
            "timestampMs": frame["timestampMs"], "pixelSha256": frame["pixelSha256"],
            "sha256": frame["sha256"], "status": "readable", "scene": "Vehicle, price from 125800 CNY",
            "text": ["от 125800 CNY"], "numbers": [{"raw":"от 125800 CNY","value":"125800",
                "unit":null,"currency":"CNY","uncertain":false}],
            "uncertainties": ["Price is stated as starting from"]
        })).collect();
        json!({
            "schemaVersion":2,"status":"complete","source":request["source"],"inventory":request["inventory"],
            "chunk":request["chunk"],"manifestSha256":request["manifestSha256"],"frames":frames,
            "summary":"Two inspected frames show a vehicle and qualified price.",
            "provenance":{"backend":"offline-test","model":"test","instructionSha256":"2".repeat(64)}
        })
    }

    #[test]
    fn fixed_sampling_reasons_roundtrip_in_sealed_response(){
        let mut input=request();
        input["frames"][0]["selectionReasons"]=json!(["second_midpoint"]);
        input["frames"][1]["selectionReasons"]=json!(["second_end"]);
        let input=seal_request(input);let output=response(&input);
        assert!(validate_response(&input,&output).is_ok());
        let mut invented=input.clone();invented["frames"][0]["selectionReasons"]=json!(["invented"]);
        let invented=seal_request(invented);assert!(validate_response(&invented,&response(&invented)).is_err());
    }
    #[test]
    fn accepts_exact_subset_and_returns_path_free_observations() {
        let request = request();
        let response = response(&request);
        let accepted = validate_response(&request, &response).unwrap();
        assert_eq!(accepted, response);
        assert!(accepted.to_string().contains("от 125800 CNY"));
        assert!(!accepted.to_string().contains("private/64.png"));
        assert_eq!(request["chunk"]["endSelectionIndexExclusive"], 96);
    }

    #[test]
    fn mixed_frame_provenance_is_exact_bounded_and_does_not_relabel_local_frames() {
        let req=request(); let mut output=response(&req);
        output["provenance"]=json!({"schemaVersion":2,"kind":"mixed_frames","frames":[
            {"id":"f64","backend":"local_ollama","model":format!("fixture:1@sha256:{}","e".repeat(64)),"instructionSha256":"2".repeat(64)},
            {"id":"f95","backend":"codex_isolated","model":"gpt-6-luna","instructionSha256":"2".repeat(64)}],
            "rescue":{"permitSha256":"3".repeat(64),"cloudFrameCount":1,"cloudInvocationCount":1}});
        assert!(validate_response(&req,&output).is_ok());
        let mut sol61=output.clone();sol61["provenance"]["frames"][1]["model"]=json!(crate::codex_model_policy::MODEL);
        assert!(validate_response(&req,&sol61).is_ok(),"new cloud evidence keeps exact four-frame rescue accounting");
        for (pointer,replacement) in [
            ("/provenance/schemaVersion",json!(3)),("/provenance/frames/1/id",json!("f64")),
            ("/provenance/frames/1/backend",json!("invented")),("/provenance/frames/1/model",json!("gpt-6-sol")),
            ("/provenance/frames/0/model",json!("unbound-model")),("/provenance/frames/1/instructionSha256",json!("9".repeat(64))),
            ("/provenance/rescue/cloudFrameCount",json!(2)),("/provenance/rescue/cloudInvocationCount",json!(2)),
            ("/provenance/rescue/permitSha256",json!("invalid"))] {
            let mut changed=output.clone();*changed.pointer_mut(pointer).unwrap()=replacement;
            assert_eq!(validate_response(&req,&changed).unwrap_err(),"visual_chunk_provenance_invalid");
        }
        output["provenance"]["frames"].as_array_mut().unwrap().pop();
        assert!(validate_response(&req,&output).is_err());
        assert!(validate_response(&req,&response(&req)).is_ok(),"historical three-key provenance remains valid");
    }

    #[test]
    fn rejects_changed_bindings_partial_uncertain_and_foreign_fields() {
        let request = request();
        let good = response(&request);
        let mut variants = Vec::new();
        let mut variant = good.clone();
        variant["status"] = json!("incomplete");
        variants.push(variant);
        let mut variant = good.clone();
        variant["frames"].as_array_mut().unwrap().pop();
        variants.push(variant);
        let mut variant = good.clone();
        variant["frames"][1] = variant["frames"][0].clone();
        variants.push(variant);
        let mut variant = good.clone();
        variant["frames"][0]["pixelSha256"] = json!("3".repeat(64));
        variants.push(variant);
        let mut variant = good.clone();
        variant["frames"][0]["numbers"][0]["uncertain"] = json!(true);
        variants.push(variant);
        let mut variant = good.clone();
        variant["frames"][0]["path"] = json!("private/64.png");
        variants.push(variant);
        let mut variant = good.clone();
        variant["provenance"]["unexpected"] = json!(1);
        variants.push(variant);
        for variant in variants {
            assert!(validate_response(&request, &variant).is_err());
        }
    }

    #[test]
    fn successful_inspection_retains_legibility_unknown_without_fabricating_number(){
        let request=request();let mut response=response(&request);
        response["frames"][0]["status"]=json!("unreadable");response["frames"][0]["numbers"][0]["uncertain"]=json!(true);response["frames"][0]["numbers"][0]["value"]=Value::Null;response["frames"][0]["uncertainties"]=json!(["Price partly obscured; no numeric claim permitted"]);
        let accepted=validate_response(&request,&response).unwrap();assert_eq!(accepted["frames"][0]["numbers"][0]["value"],Value::Null);assert_eq!(accepted["frames"][0]["status"],"unreadable");
    }
    #[test]
    fn rejects_tampered_manifest_and_out_of_range_or_oversized_chunk() {
        let request = request();
        let response = response(&request);
        let mut changed = request.clone();
        changed["inventory"]["width"] = json!(640);
        assert!(validate_response(&changed, &response).is_err());
        let mut changed = request.clone();
        changed["chunk"]["endSelectionIndexExclusive"] = json!(97);
        changed = seal_request(changed);
        assert!(validate_response(&changed, &response).is_err());
        let mut changed = request.clone();
        changed["frames"][0]["frameIndex"] = json!(63);
        changed = seal_request(changed);
        assert!(validate_response(&changed, &response).is_err());
    }
}
