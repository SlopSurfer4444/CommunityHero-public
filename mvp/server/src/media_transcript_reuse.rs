//! Reuse an already admitted, explicitly complete audio transcript. This does
//! not create a transcript, infer identity from titles, or alter source history.
use serde_json::{Value, json};

/// Post-independent exact-file path. The returned material remains the original
/// donor; applicability lives in the returned native pin and must be admitted
/// separately. A legacy URL/title match cannot enter this path.
pub(crate) fn select_verified(d:&Value,progress:&Value,ledger:&Value,receipt:&Value,spec:&str)
    ->Result<Option<(Value,Value)>,String> {
    crate::media_analysis_reuse::select_verified(d,progress,ledger,receipt,spec)
}

pub(crate) fn validate_verified_pin(d:&Value,progress:&Value,ledger:&Value,receipt:&Value,pin:&Value)->Result<(),String> {
    crate::media_analysis_reuse::validate_pin(d,progress,ledger,receipt,pin)
}

fn text<'a>(v: &'a Value, key: &str) -> &'a str {
    v[key].as_str().unwrap_or("")
}
fn rows<'a>(v: &'a Value, key: &str) -> &'a [Value] {
    v[key].as_array().map(Vec::as_slice).unwrap_or(&[])
}

pub(crate) fn select(d: &Value, progress: &Value, at: &str) -> Result<Option<Value>, &'static str> {
    let binding = crate::active_binding(d).map_err(|_| "audio_reuse_account_invalid")?;
    if d["account"] != progress["account"] || binding.to_json() != progress["connectorBinding"] {
        return Err("audio_reuse_account_changed");
    }
    let post = rows(d, "posts")
        .iter()
        .find(|p| p["id"] == progress["sourcePostId"])
        .ok_or("audio_reuse_post_missing")?;
    if post["postKey"] != progress["sourcePostKey"]
        || crate::media_fullframes::source_version(post, text(progress, "account"))
            != progress["sourceVersion"]
    {
        return Err("audio_reuse_source_changed");
    }
    let source = crate::media_artifacts::ArtifactRef::from_json(&progress["source"])
        .map_err(|_| "audio_reuse_source_invalid")?;
    if progress["sourceIdentity"]["account"] != progress["account"]
        || progress["sourceIdentity"]["postKey"] != progress["sourcePostKey"]
        || progress["sourceIdentity"]["mediaSha256"] != source.sha256
    {
        return Err("audio_reuse_source_invalid");
    }
    let selected = crate::knowledge::select(d, &[], std::slice::from_ref(post), at)?;
    for material in rows(&selected, "materials") {
        if material["kind"] != "transcript"
            || text(material, "text").trim().is_empty()
            || material["transcription"]["partial"] != false
        {
            continue;
        }
        let transcription = &material["transcription"];
        // Explicitly contradictory coverage must not be hidden by partial=false.
        if transcription
            .get("coverage")
            .is_some_and(|v| v != "full_audio")
        {
            continue;
        }
        let Some(provenance) = rows(&selected, "manifest")
            .iter()
            .find(|p| p["versionId"] == material["knowledgeVersionId"])
        else {
            continue;
        };
        let exact = material["mediaSha256"] == source.sha256;
        let bindings=rows(provenance,"mediaBinding");
        let explicit=bindings.iter().any(|b|b["postKey"]==post["postKey"]&&!rows(b,"identities").is_empty());
        let policy_title=bindings.iter().any(|b|b["postKey"]==post["postKey"]
            && b["match"]=="exact_normalized_title"
            && b["authorization"]=="account_scoped_exact_title_reuse");
        if !exact && !explicit && !policy_title {
            continue;
        }
        let current_ms = progress["sourceIdentity"]["durationMs"]
            .as_u64()
            .filter(|n| *n > 0);
        let prior_ms = transcription["mediaDurationSeconds"]
            .as_f64()
            .filter(|n| n.is_finite() && *n > 0.0 && *n < (u64::MAX / 1000) as f64)
            .map(|n| (n * 1000.0).round() as u64);
        if current_ms
            .zip(prior_ms)
            .is_some_and(|(a, b)| a.abs_diff(b) > 1000)
            || !exact && current_ms.zip(prior_ms).is_none()
        {
            continue;
        }
        return Ok(Some(
            json!({"policy":"admitted_complete_transcript_reuse_v1",
            "account":progress["account"],"targetPostKey":post["postKey"],"targetMediaSha256":source.sha256,
            "match":if exact{"exact_media_sha256"}else if explicit{"admitted_explicit_identity"}else{"account_scoped_exact_title_policy"},
            "sourceMaterialId":material["id"],"sourcePostKey":material["postKey"],
            "transcription":transcription,"provenance":provenance}),
        ));
    }
    Ok(None)
}

/// A new visual observation can split a formerly title-equivalent source
/// group. Check the exact catalog state that final admission will create before
/// skipping transcription of the target audio.
pub(crate) fn select_after_visual(d:&Value,progress:&Value,visual:&Value,at:&str)->Result<Option<Value>,&'static str>{
    let Some(pin)=select(d,progress,at)? else{return Ok(None)};
    let mut prospective=d.clone();
    crate::merge_materials(&mut prospective,&json!({"materials":[visual.clone()]}))
        .map_err(|_|"visual_admission_projection_failed")?;
    let admitted_at=crate::now();
    let post=rows(&prospective,"posts").iter().find(|p|p["id"]==progress["sourcePostId"])
        .ok_or("media_post_missing")?;
    let policy=crate::post_media_policy::effective(&prospective,post).map_err(|_|"media_policy_unavailable")?;
    let ready=crate::knowledge::TranscriptLookup::new(&prospective,&admitted_at)
        .map_err(|_|"media_catalog_unavailable")?
        .ready_for_policy(post,policy["visualRequired"]==true)
        .map_err(|_|"media_catalog_unavailable")?;
    Ok((ready&&validate_pin(&prospective,progress,&pin,&admitted_at).is_ok()).then_some(pin))
}

/// Called under the final admission transaction: a current selector decision
/// must reproduce the exact pinned version/hash/binding/coverage, or fail closed.
pub(crate) fn validate_pin(
    d: &Value,
    progress: &Value,
    pin: &Value,
    at: &str,
) -> Result<(), &'static str> {
    if select(d, progress, at)?.as_ref() != Some(pin) {
        return Err("audio_reuse_evidence_changed");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    const AT: &str = "2026-09-23T12:00:00Z";
    fn fixture() -> (Value, Value) {
        let post = json!({"id":"target","postKey":"target","account":"LikeAvto","title":"A particular road test with full details","sourceUrl":"https://youtu.be/AbCdEf123_-","attachments":[{"type":"video"}]});
        let mut d = json!({"account":"LikeAvto","posts":[post],"materials":[{"id":"transcript","kind":"transcript","account":"LikeAvto","postKey":"target","sourceUrl":post["sourceUrl"],"mediaSha256":"a".repeat(64),"text":"Existing spoken words.","transcription":{"partial":false,"coverage":"full_audio","mediaDurationSeconds":37.764,"model":"original-asr","sourcePostKey":"target"}}]});
        crate::knowledge::sync_catalog(&mut d, AT).unwrap();
        let binding = crate::active_binding(&d).unwrap();
        let mut progress =
            crate::media_fullframes::initial("LikeAvto", &binding.to_json(), &post, AT);
        progress["source"] = json!({"sha256":"a".repeat(64),"bytes":100});
        progress["sourceIdentity"] = json!({"account":"LikeAvto","postKey":"target","mediaSha256":"a".repeat(64),"durationMs":37764});
        (d, progress)
    }
    #[test]
    fn exact_audio_hit_preserves_provenance_and_existing_bundle_without_import() {
        let (d, progress) = fixture();
        let before = d.clone();
        let pin = select(&d, &progress, AT).unwrap().unwrap();
        assert_eq!(pin["match"], "exact_media_sha256");
        assert_eq!(pin["transcription"], d["materials"][0]["transcription"]);
        assert_eq!(
            pin["provenance"]["versionId"],
            d["knowledge_entries"][0]["currentVersionId"]
        );
        validate_pin(&d, &progress, &pin, AT).unwrap();
        let bundle = crate::knowledge::select(&d, &[json!({"postKey":"target"})], &[], AT).unwrap();
        assert_eq!(bundle["materials"][0]["text"], "Existing spoken words.");
        assert_eq!(bundle["manifest"][0], pin["provenance"]);
        assert_eq!(d, before);
    }
    #[test]
    fn partial_unknown_coverage_and_empty_audio_never_skip_transcription() {
        for mode in [
            "partial", "unknown", "coverage", "empty", "duration", "hash",
        ] {
            let (mut d, progress) = fixture();
            match mode {
                "partial" => d["materials"][0]["transcription"]["partial"] = json!(true),
                "unknown" => {
                    d["materials"][0]["transcription"]
                        .as_object_mut()
                        .unwrap()
                        .remove("partial");
                }
                "coverage" => {
                    d["materials"][0]["transcription"]["coverage"] = json!("first_900_seconds")
                }
                "empty" => d["materials"][0]["text"] = json!(" "),
                "duration" => {
                    d["materials"][0]["transcription"]["mediaDurationSeconds"] = json!(90)
                }
                _ => d["materials"][0]["mediaSha256"] = json!("b".repeat(64)),
            }
            crate::knowledge::sync_catalog(&mut d, AT).unwrap();
            assert!(select(&d, &progress, AT).unwrap().is_none(), "{mode}");
        }
    }
    #[test]
    fn stale_pin_and_account_or_source_changes_fail_closed() {
        let (mut d, progress) = fixture();
        let pin = select(&d, &progress, AT).unwrap().unwrap();
        d["materials"][0]["text"] = json!("Changed spoken words");
        crate::knowledge::sync_catalog(&mut d, AT).unwrap();
        assert!(validate_pin(&d, &progress, &pin, AT).is_err());
        let (mut d, progress) = fixture();
        d["account"] = json!("BAW Russia");
        assert!(select(&d, &progress, AT).is_err());
        let (mut d, progress) = fixture();
        d["posts"][0]["sourceUrl"] = json!("https://youtu.be/ZbCdEf123_-");
        assert!(select(&d, &progress, AT).is_err());
        let (mut d, progress) = fixture();
        let local_pin = select(&d, &progress, AT).unwrap().unwrap();
        d["materials"][0]["account"] = json!("BAW Russia");
        crate::knowledge::sync_catalog(&mut d, AT).unwrap();
        // A foreign raw import cannot erase or rewrite an admitted local head.
        // The surviving match is still exactly that original local version.
        assert_eq!(select(&d, &progress, AT).unwrap(), Some(local_pin));
        // Independently exercise a foreign-only catalog, without the local
        // fixture's previously admitted version masking this negative case.
        d["knowledge_entries"] = json!([]);
        d["knowledge_versions"] = json!([]);
        crate::knowledge::sync_catalog(&mut d, AT).unwrap();
        assert!(rows(&d, "knowledge_entries").is_empty());
        assert!(select(&d, &progress, AT).unwrap().is_none());
    }
    #[test]
    fn future_expired_and_corrupted_catalog_evidence_cannot_be_reused() {
        let (mut d, progress) = fixture();
        assert!(
            select(&d, &progress, "2026-09-23T11:59:59Z")
                .unwrap()
                .is_none()
        );
        d["knowledge_versions"][0]["validUntil"] = json!("2026-09-23T13:00:00Z");
        let mut payload = d["knowledge_versions"][0].clone();
        for key in ["id", "hash", "createdAt"] {
            payload.as_object_mut().unwrap().remove(key);
        }
        d["knowledge_versions"][0]["hash"] = json!(crate::media_fullframes::hash(&payload));
        let pin = select(&d, &progress, AT).unwrap().unwrap();
        assert!(
            select(&d, &progress, "2026-09-23T13:00:00Z")
                .unwrap()
                .is_none()
        );
        assert!(validate_pin(&d, &progress, &pin, "2026-09-23T13:00:00Z").is_err());
        d["knowledge_versions"][0]["text"] = json!("unhashed mutation");
        assert!(select(&d, &progress, AT).is_err());
    }
    #[test]
    fn same_title_different_video_never_reuses_origin() {
        for generic in [false, true] {
            let (mut d, mut progress) = fixture();
            let title = if generic {
                "Video by likeavto_import"
            } else {
                "A particular road test with full details"
            };
            d["posts"][0]["title"] = json!(title);
            d["posts"].as_array_mut().unwrap().push(json!({"id":"origin","postKey":"origin","title":title,"sourceUrl":"https://instagram.com/reel/ABC/","account":"LikeAvto"}));
            d["materials"][0]["postKey"] = json!("origin");
            d["materials"][0]["sourceUrl"] = json!("https://instagram.com/reel/ABC/");
            d["materials"][0]["mediaSha256"] = json!("b".repeat(64));
            d["materials"][0]["transcription"]["sourcePostKey"] = json!("origin");
            crate::knowledge::sync_catalog(&mut d, AT).unwrap();
            progress["sourceVersion"] = json!(crate::media_fullframes::source_version(
                &d["posts"][0],
                "LikeAvto"
            ));
            let pin = select(&d, &progress, AT).unwrap();
            assert!(pin.is_none(),"neither specific nor generic titles prove the source");
        }
    }
    #[test]
    fn completed_visual_observation_rejects_conflicting_title_audio_but_keeps_exact_sha(){
        for exact in [false,true] {
            let (base,mut progress)=fixture();
            let mut d=crate::empty();
            d["posts"]=base["posts"].clone();d["materials"]=base["materials"].clone();
            let title=d["posts"][0]["title"].clone();
            d["posts"].as_array_mut().unwrap().push(json!({"id":"origin","postKey":"origin",
                "account":"LikeAvto","title":title,
                "sourceUrl":"https://instagram.com/reel/ABC/","attachments":[{"type":"video"}]}));
            d["materials"][0]["postKey"]=json!("origin");
            d["materials"][0]["sourceUrl"]=json!("https://instagram.com/reel/ABC/");
            d["materials"][0]["transcription"]["sourcePostKey"]=json!("origin");
            d["materials"][0]["transcription"]["mediaDurationSeconds"]=json!(1.0);
            d["materials"][0]["transcription"]["audioDurationSeconds"]=json!(1.0);
            d["materials"][0]["transcription"]["coverage"]=json!("full_audio");
            let observed=crate::media_fullframes::fixture_for_post("LikeAvto",&d["posts"][0]);
            let target_sha=observed["source"]["mediaSha256"].clone();
            // Record the independently observed target bytes before current
            // catalog/policy admission. Progress alone is not a catalog alias.
            d["posts"][0]["mediaSha256"]=target_sha.clone();
            let evidence=crate::media_fullframes::fixture_for_post("LikeAvto",&d["posts"][0]);
            d["materials"][0]["mediaSha256"]=if exact{target_sha.clone()}else{json!("a".repeat(64))};
            // This fixture exercises completed visual acquisition, which is
            // now an exact owner opt-in rather than the acquisition default.
            d["settings"]["postMediaPolicies"]=json!({"target":{"version":1,"revision":1,"status":"active",
                "postId":"target","account":"LikeAvto","connectorBinding":crate::active_binding(&d).unwrap().to_json(),
                "sourceVersion":crate::media_fullframes::source_version(&d["posts"][0],"LikeAvto"),"mode":"full_audio_visual"}});
            crate::knowledge::sync_catalog(&mut d,AT).unwrap();
            progress["source"]=json!({"sha256":target_sha,"bytes":100});
            progress["sourceIdentity"]=json!({"account":"LikeAvto","postKey":"target",
                "mediaSha256":target_sha,"durationMs":1000});
            progress["sourceVersion"]=json!(crate::media_fullframes::source_version(&d["posts"][0],"LikeAvto"));
            let visual=json!({"id":"new-target-visual","title":"Visual context","text":"Reviewed every selected frame",
                "kind":"visual_context","account":"LikeAvto","postKey":"target",
                "sourceUrl":d["posts"][0]["sourceUrl"],"mediaSha256":target_sha,"visualEvidence":evidence});
            let pre=select(&d,&progress,AT).unwrap();
            assert_eq!(pre.is_some(),exact,"only the independently observed exact byte binding admits reuse before visual acquisition");
            if let Some(pin)=pre{assert_eq!(pin["match"],"exact_media_sha256");}
            let before=d.clone();
            let after=select_after_visual(&d,&progress,&visual,AT).unwrap();
            assert_eq!(after.is_some(),exact,"different observed source bytes must trigger target ASR");
            assert_eq!(d,before,"prospective admission must not mutate the workspace");
        }
    }
    #[test]
    fn explicit_source_identity_is_recorded_separately_from_title_policy() {
        let (mut d,progress)=fixture();
        let url=d["posts"][0]["sourceUrl"].clone();
        d["posts"].as_array_mut().unwrap().push(json!({"id":"origin","postKey":"origin","title":"Different publication title","sourceUrl":url,"account":"LikeAvto"}));
        d["materials"][0]["postKey"]=json!("origin");
        d["materials"][0]["mediaSha256"]=json!("b".repeat(64));
        d["materials"][0]["transcription"]["sourcePostKey"]=json!("origin");
        crate::knowledge::sync_catalog(&mut d,AT).unwrap();
        let pin=select(&d,&progress,AT).unwrap().unwrap();
        assert_eq!(pin["match"],"admitted_explicit_identity");
        assert_eq!(pin["sourcePostKey"],"origin");
        validate_pin(&d,&progress,&pin,AT).unwrap();
    }
}
