//! Versioned sampled visual evidence. Completion means the prescribed samples
//! were inspected, never that every frame or every brief overlay was observed.
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;

pub(crate) const VERSION: u64 = 1;
pub(crate) const MAX_FRAMES: usize = 96;
fn digest(value: &Value) -> String { format!("{:x}", Sha256::digest(value.to_string().as_bytes())) }
fn text<'a>(v: &'a Value, k: &str) -> &'a str { v[k].as_str().unwrap_or("") }
fn sha(v: &Value) -> bool { v.as_str().is_some_and(|s|s.len()==64 && s.bytes().all(|c|c.is_ascii_hexdigit())) }

/// Integer timestamps avoid rounding gaps. Last sample is 100ms before the
/// measured endpoint; extraction must actually produce that JPEG or fail.
pub(crate) fn sample_times(duration_ms:u64)->Result<Vec<u64>, &'static str> {
    if duration_ms < 100 {return Err("visual_duration_invalid");}
    let end=duration_ms.saturating_sub(100);
    if duration_ms > 192_000 {return Err("visual_sampling_budget_exceeded");}
    let mut times:BTreeSet<u64>=(0..=end).step_by(2000).collect();
    let tail=duration_ms.saturating_sub(10_000);
    times.extend((tail..=end).step_by(1000));
    times.insert(end);
    if times.len()>MAX_FRAMES {return Err("visual_sampling_budget_exceeded");}
    Ok(times.into_iter().collect())
}
pub(crate) fn coverage(duration_ms:u64,frames:&[Value])->Value {
    let times:Vec<_>=frames.iter().filter_map(|f|f["timestampMs"].as_u64()).collect();
    let max_gap=times.windows(2).map(|w|w[1].saturating_sub(w[0])).max().unwrap_or(0);
    json!({"kind":"sampled_frames","samplingVersion":VERSION,"durationMs":duration_ms,
        "regularIntervalMs":2000,"tailWindowMs":10000,"tailIntervalMs":1000,
        "tailStartMs":duration_ms.saturating_sub(10000),"maxGapMs":max_gap,
        "endingFrameId":frames.last().map(|f|f["id"].clone())})
}
pub(crate) fn seal_request(mut request:Value)->Value {
    request["manifestSha256"]=json!(digest(&request));request
}
fn strings(v:&Value,max:usize)->bool {v.as_array().is_some_and(|a|a.len()<=max && a.iter().all(|x|x.as_str().is_some_and(|s|s.len()<=4000)))}
fn nonempty(v:&Value,max:usize)->bool {v.as_str().is_some_and(|s|!s.trim().is_empty()&&s.len()<=max)}
fn allowed(v:&Value,keys:&[&str])->bool {v.as_object().is_some_and(|o|o.len()==keys.len()&&o.keys().all(|k|keys.contains(&k.as_str())))}

/// Store the path-free manifest together with its result. The original hash
/// binds the exact ephemeral request; durableManifestSha256 independently
/// binds all retained source, coverage and frame identities.
pub(crate) fn admit(request:&Value,result:&Value)->Result<Value,&'static str>{
    let mut unhashed=request.clone();unhashed.as_object_mut().ok_or("visual_manifest_invalid")?.remove("manifestSha256");
    if request["manifestSha256"]!=digest(&unhashed){return Err("visual_manifest_hash_invalid");}
    validate_response(request,result)?;
    let mut durable=request.clone();
    durable.as_object_mut().ok_or("visual_manifest_invalid")?.remove("manifestSha256");
    for frame in durable["frames"].as_array_mut().ok_or("visual_manifest_invalid")? {frame.as_object_mut().ok_or("visual_manifest_invalid")?.remove("path");}
    let evidence=json!({"schemaVersion":VERSION,"manifest":durable,"durableManifestSha256":digest(&durable),"result":result});
    validate(&evidence)?;Ok(evidence)
}
fn validate_response(request:&Value,result:&Value)->Result<(), &'static str>{
    if !allowed(result,&["schemaVersion","status","source","manifestSha256","coverage","frames","summary","provenance"])
        || result["schemaVersion"]!=VERSION || result["status"]!="complete"
        || result["source"]!=request["source"] || result["coverage"]!=request["coverage"]
        || result["manifestSha256"]!=request["manifestSha256"] || !sha(&result["manifestSha256"])
        || !nonempty(&result["summary"],16000) {return Err("visual_evidence_incomplete");}
    if !allowed(&result["provenance"],&["backend","model","instructionSha256"])
        || !nonempty(&result["provenance"]["backend"],128) || !nonempty(&result["provenance"]["model"],256)
        || !sha(&result["provenance"]["instructionSha256"]) {return Err("visual_provenance_invalid");}
    let expected=request["frames"].as_array().ok_or("visual_manifest_invalid")?;
    let actual=result["frames"].as_array().ok_or("visual_frames_invalid")?;
    if actual.len()!=expected.len() || actual.is_empty() || actual.len()>MAX_FRAMES {return Err("visual_frames_incomplete");}
    let mut seen=BTreeSet::new();
    for frame in actual {
        if !allowed(frame,&["id","sha256","timestampMs","status","scene","text","numbers","uncertainties"])
            || !seen.insert(text(frame,"id")) || !matches!(text(frame,"status"),"readable"|"none")
            || !nonempty(&frame["scene"],4000) || !strings(&frame["text"],64) || !strings(&frame["uncertainties"],32) {
            return Err("visual_frame_invalid");
        }
        let original=expected.iter().find(|f|f["id"]==frame["id"]).ok_or("visual_frame_unknown")?;
        if frame["sha256"]!=original["sha256"] || frame["timestampMs"]!=original["timestampMs"] || !sha(&frame["sha256"]){return Err("visual_frame_binding_changed");}
        let numbers=frame["numbers"].as_array().filter(|a|a.len()<=64).ok_or("visual_numbers_invalid")?;
        for n in numbers {
            if !allowed(n,&["raw","value","unit","currency","uncertain"]) || !nonempty(&n["raw"],256)
                || !["value","unit","currency"].iter().all(|k|n[*k].is_null()||nonempty(&n[*k],128))
                || !n["uncertain"].is_boolean() {return Err("visual_numbers_invalid");}
            if n["uncertain"]==true {return Err("visual_number_unreadable");}
        }
    }
    Ok(())
}
pub(crate) fn validate(evidence:&Value)->Result<(), &'static str>{
    let manifest=&evidence["manifest"];
    let result=&evidence["result"];
    if evidence["schemaVersion"]!=VERSION || evidence["durableManifestSha256"]!=digest(manifest)
        || manifest["schemaVersion"]!=VERSION || result["status"]!="complete"
        || result["source"]!=manifest["source"] || result["coverage"]!=manifest["coverage"]
        || !sha(&manifest["source"]["mediaSha256"]) {return Err("visual_evidence_invalid");}
    if !allowed(manifest,&["schemaVersion","workId","createdAtUtc","source","coverage","frames"])
        || !allowed(&manifest["source"],&["account","postKey","mediaSha256","durationMs"])
        || !nonempty(&manifest["source"]["account"],256) || !nonempty(&manifest["source"]["postKey"],1024)
        || !nonempty(&manifest["workId"],128) || !nonempty(&manifest["createdAtUtc"],64) {return Err("visual_manifest_invalid");}
    let mut request=manifest.clone();request["manifestSha256"]=result["manifestSha256"].clone();
    validate_response(&request,result)?;
    let duration=manifest["source"]["durationMs"].as_u64().ok_or("visual_duration_invalid")?;
    let expected=sample_times(duration)?;
    let frames=manifest["frames"].as_array().ok_or("visual_manifest_invalid")?;
    let actual:Vec<_>=frames.iter().filter_map(|f|f["timestampMs"].as_u64()).collect();
    if actual!=expected || manifest["coverage"]!=coverage(duration,frames)
        || frames.iter().any(|f|f.get("path").is_some()||!sha(&f["sha256"])) {return Err("visual_coverage_incomplete");}
    let responses=result["frames"].as_array().ok_or("visual_frames_invalid")?;
    if responses.len()!=frames.len(){return Err("visual_frames_incomplete");}
    let mut seen=BTreeSet::new();
    for f in responses {
        if !seen.insert(text(f,"id")) || !matches!(text(f,"status"),"readable"|"none")
            || !nonempty(&f["scene"],4000) || !frames.iter().any(|s|s["id"]==f["id"]&&s["sha256"]==f["sha256"]&&s["timestampMs"]==f["timestampMs"]) {
            return Err("visual_frames_incomplete");
        }
    }
    Ok(())
}

#[cfg(test)]
pub(crate) fn fixture(account:&str,post_key:&str)->Value {
    let frames:Vec<_>=sample_times(12000).unwrap().iter().enumerate().map(|(i,t)|json!({"id":format!("f{i}"),"timestampMs":t,"sha256":"a".repeat(64),"path":format!("private/{i}.jpg")})).collect();
    let request=seal_request(json!({"schemaVersion":1,"workId":"media-test","createdAtUtc":"2026-09-23T00:00:00Z","source":{"account":account,"postKey":post_key,"mediaSha256":"b".repeat(64),"durationMs":12000},"coverage":coverage(12000,&frames),"frames":frames}));
    let result=json!({"schemaVersion":1,"status":"complete","source":request["source"],"manifestSha256":request["manifestSha256"],"coverage":request["coverage"],"frames":frames.iter().map(|f|json!({"id":f["id"],"sha256":f["sha256"],"timestampMs":f["timestampMs"],"status":"readable","scene":"Visible vehicle","text":[],"numbers":[],"uncertainties":[]})).collect::<Vec<_>>(),"summary":"Sampled video frames show a vehicle.","provenance":{"backend":"offline-test","model":"test","instructionSha256":"c".repeat(64)}});
    admit(&request,&result).unwrap()
}
#[cfg(test)] mod tests {
    use super::*;
    #[test] fn price_before_black_end_keeps_timestamp_currency_and_uncertainty(){
        let mut value=fixture("LikeAvto","post");
        let frames=value["result"]["frames"].as_array_mut().unwrap();
        let price=frames.iter_mut().find(|f|f["timestampMs"]==11000).unwrap();
        price["uncertainties"]=json!(["Price qualified as starting from; not a current verified quote"]);
        price["numbers"]=json!([{"raw":"от 125800 CNY","value":"125800","unit":null,"currency":"CNY","uncertain":false}]);
        let ending=frames.last_mut().unwrap();ending["status"]=json!("none");ending["scene"]=json!("Black ending frame");
        validate(&value).unwrap();
        assert_eq!(value["result"]["frames"].as_array().unwrap().iter().find(|f|f["timestampMs"]==11000).unwrap()["numbers"][0]["currency"],"CNY");
    }
    #[test] fn malformed_duplicate_unattributed_and_partial_observations_are_rejected(){
        let value=fixture("LikeAvto","post");
        let mut variants=Vec::new();
        let mut v=value.clone();v["result"]["status"]=json!("incomplete");variants.push(v);
        let mut v=value.clone();v["result"]["frames"][1]=v["result"]["frames"][0].clone();variants.push(v);
        let mut v=value.clone();v["result"]["frames"][0]["sha256"]=json!("e".repeat(64));variants.push(v);
        let mut v=value.clone();v["result"]["frames"][0]["scene"]=json!("");variants.push(v);
        let mut v=value.clone();v["result"]["frames"][0]["path"]=json!("private");variants.push(v);
        let mut v=value.clone();v["result"]["provenance"].as_object_mut().unwrap().remove("instructionSha256");variants.push(v);
        let mut v=value.clone();v["result"]["frames"][0]["numbers"]=json!([{"raw":"125800","value":"125800","unit":null,"currency":"CNY","uncertain":"no"}]);variants.push(v);
        for number in [json!({"raw":"125?00 CNY","value":"125800","unit":null,"currency":"CNY","uncertain":true}),json!({"raw":"125?00 CNY","value":null,"unit":null,"currency":"CNY","uncertain":true})] {
            let mut v=value.clone();v["result"]["frames"][0]["numbers"]=json!([number]);variants.push(v);
        }
        for v in variants{assert!(validate(&v).is_err());}
        let mut shortened=value;shortened["manifest"]["frames"].as_array_mut().unwrap().pop();
        shortened["durableManifestSha256"]=json!(digest(&shortened["manifest"]));
        assert!(validate(&shortened).is_err());
    }
    #[test] fn tail_contains_price_before_black_ending_and_has_no_sampling_gap(){
        let times=sample_times(60000).unwrap();
        for t in (50000..60000).step_by(1000){assert!(times.contains(&t));}
        assert_eq!(times.last(),Some(&59900));
        assert!(times.windows(2).all(|w|w[1]-w[0]<=2000));
    }
    #[test] fn unknown_and_overbudget_duration_never_complete(){assert!(sample_times(0).is_err());assert!(sample_times(192001).is_err());assert!(sample_times(190000).is_err());}
    #[test] fn durable_evidence_detects_missing_tail_frame_or_source_change(){
        let value=fixture("LikeAvto","post");assert!(validate(&value).is_ok());
        let mut missing=value.clone();missing["result"]["frames"].as_array_mut().unwrap().pop();assert!(validate(&missing).is_err());
        let mut source=value;source["result"]["source"]["mediaSha256"]=json!("d".repeat(64));assert!(validate(&source).is_err());
    }
    #[test] fn unreadable_is_not_completion(){let mut value=fixture("LikeAvto","post");value["result"]["frames"][0]["status"]=json!("unreadable");assert!(validate(&value).is_err());}
}
