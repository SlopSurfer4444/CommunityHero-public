//! Independent uncached capture oracle, extended from installed R7r2 to the
//! explicit current strict-family and mandatory-material capture policies.
//! Only the random capture ID is normalized; request bytes/digests/errors are exact.
use super::*;
const AT:&str="2020-01-01T08:00:00Z";

fn fixture(profile:crate::accounts::Profile,n:usize,rules:usize)->(Value,Vec<Value>){
    let mut d=crate::empty();crate::accounts::initialize(&mut d,profile).unwrap();
    let sample=super::tests::fixture(false);
    for field in ["posts","branches"]{d[field]=sample[field].clone();}
    d["posts"][0]["attachments"]=json!([]);
    let mut ids=Vec::new();let mut items=Vec::new();let mut messages=Vec::new();
    for index in 0..n{
        let id=format!("recipient-{index:03}");let mut item=sample["items"][0].clone();
        item["id"]=json!(id);item["itemId"]=json!(format!("external-{index:03}"));
        item["text"]=json!(format!("Thank you for the explanation {index}."));
        item["account"]=json!(profile.display());item["connectorBinding"]=profile.binding();
        messages.push(json!({"id":item["itemId"],"text":item["text"],"authorId":format!("author-{index:03}")}));
        ids.push(item["id"].clone());items.push(item);
    }
    d["items"]=json!(items);d["branches"][0]["messages"]=json!(messages);
    crate::knowledge::sync_catalog(&mut d,AT).unwrap();
    for index in 0..rules{
        crate::knowledge::save_instruction(&mut d,&json!({"requestId":format!("capture-rule-{index}"),
            "title":format!("Rule {index}"),"text":format!("Account rule {index}. {}","Use only actual source evidence. ".repeat(16))}),AT).unwrap();
    }
    (d,ids)
}
fn normalized(mut result:Result<Value,&'static str>)->Result<Value,&'static str>{
    if let Ok(value)=&mut result{value.as_object_mut().unwrap().remove("id");}result
}
fn pair(d:&Value,ids:&[Value],instruction:Option<&str>)->(Result<Value,&'static str>,usize,usize){
    let before=d.clone();
    let(old,old_count)=crate::prepare_bundle::catalog_measure::measure(||current_policy_oracle_at(d,ids,instruction,Some(AT)));
    let(new,new_count)=crate::prepare_bundle::catalog_measure::measure(||build_request_at(d,ids,instruction,Some(AT)));
    let old=normalized(old);let new=normalized(new);
    assert_eq!(new,old,"complete capture/request/fingerprints or exact first error must match");
    assert_eq!(*d,before,"capture must not mutate its source or durable histories");
    (new,old_count,new_count)
}

#[test]
fn capture_context_exact_request_and_company_parity_with_one_catalog(){
    for profile in [crate::accounts::Profile::LikeAvto,crate::accounts::Profile::BawRussia]{
        for n in [1,4,16]{
            let(d,ids)=fixture(profile,n,8);let (capture,old,new)=pair(&d,&ids,Some("Preserve each recipient's meaning."));
            let capture=capture.unwrap();assert_eq!(capture["request"]["account"],profile.display());
            assert_eq!(capture["request"]["strictGroupContract"],crate::preparation_unit::CONTRACT);
            assert_eq!(capture["request"]["strictGroup"]["kind"],"post");
            assert_eq!(capture["request"]["strictGroup"]["recipients"].as_array().unwrap().len(),n);
            assert_eq!(capture["request"]["strictGroup"]["copies"].as_array().unwrap().len(),1);
            assert_eq!(capture["request"]["mandatoryMaterialContract"],crate::preparation_materials::CONTRACT);
            assert_eq!(capture["request"]["postContextBundle"]["materialPolicy"]["version"],crate::preparation_materials::POLICY);
            assert_eq!(capture["request"]["materialReadiness"]["status"],"ready");
            assert_eq!(capture["factSourceFingerprints"].as_object().unwrap().len(),n);
            assert_eq!(new,1,"real Catalog OnceCell initialization count for one no-video capture");
            assert_eq!(old,n+1,"installed path reconstructs its catalog for every recipient fingerprint");
            let serialized=capture["request"].to_string();
            assert_eq!(capture["digest"],json!(format!("{:x}",Sha256::digest(serialized.as_bytes()))));
        }
    }
}

#[test]
fn capture_context_preserves_exact_failure_and_never_uses_prior_capture_cache(){
    let(d,ids)=fixture(crate::accounts::Profile::LikeAvto,3,2);
    let baseline=pair(&d,&ids,None).0.unwrap();
    for case in ["missing","duplicate","nonstring","bad_catalog_hash","duplicate_catalog","foreign_catalog"]{
        let mut changed=d.clone();let mut selected=ids.clone();
        match case{
            "missing"=>selected[0]=json!("absent"),
            "duplicate"=>selected.push(selected[0].clone()),
            "nonstring"=>selected[0]=json!(false),
            "bad_catalog_hash"=>changed["knowledge_versions"][0]["hash"]=json!("0".repeat(64)),
            "duplicate_catalog"=>{let row=changed["knowledge_versions"][0].clone();changed["knowledge_versions"].as_array_mut().unwrap().push(row);},
            "foreign_catalog"=>changed["knowledge_versions"][0]["scope"]["account"]=json!("BAW Russia"),
            _=>unreachable!(),
        }
        assert!(pair(&changed,&selected,None).0.is_err(),"{case}");
    }
    let mut changed=d.clone();changed["items"][0]["text"]=json!("Changed source after earlier capture.");
    let updated=pair(&changed,&ids,None).0.unwrap();
    assert_ne!(updated["factSourceFingerprints"][ids[0].as_str().unwrap()],baseline["factSourceFingerprints"][ids[0].as_str().unwrap()]);
    assert_ne!(updated["digest"],baseline["digest"]);
    let mut revised=d.clone();let entry=revised["knowledge_entries"][0].clone();
    crate::knowledge::save_instruction(&mut revised,&json!({"requestId":"capture-rule-revision","entryId":entry["id"],
        "expectedVersionId":entry["currentVersionId"],"title":"Changed rule","text":"A newly admitted company rule."}),AT).unwrap();
    assert_ne!(pair(&revised,&ids,None).0.unwrap()["digest"],baseline["digest"]);
}

#[test]
fn capture_context_video_requires_strict_groups_and_preserves_uncertain_history(){
    let mut d=super::tests::fixture(true);crate::knowledge::sync_catalog(&mut d,AT).unwrap();
    d["operations"]=json!([{"id":"unknown-operation","itemId":"media","status":"unknown","target":{"connectorBinding":d["connectorBinding"]}}]);
    d["jobs"]=json!([{"id":"unknown-paid-job","kind":"assistant","status":"unknown","prepareBundle":{"opaque":"retained"}}]);
    assert_eq!(pair(&d,&[json!("ready"),json!("media")],None).0.unwrap_err(),crate::preparation_unit::MIXED);
    // Each strict unit retains advisory missing-speech requirements, without
    // changing historical uncertain operations or granting dispatch authority.
    let capture=pair(&d,&[json!("media")],None).0.unwrap();
    assert_eq!(capture["request"]["decisionMediaContract"],crate::decision_media::CONTRACT);
    assert_eq!(capture["request"]["strictGroup"]["kind"],"post");
    assert_eq!(capture["request"]["materialReadiness"]["status"],"pending");
    assert!(capture["request"]["materialReadiness"]["requirements"].as_array().unwrap().iter().any(|r|r["kind"]=="video_speech"&&r["status"]!="ready"));
}

#[test]
#[ignore="offline paired synthetic benchmark; ROOT execution owner only"]
fn benchmark_capture_context_catalog_reuse_100_recipients(){
    let(d,ids)=fixture(crate::accounts::Profile::LikeAvto,100,80);
    let start=std::time::Instant::now();let(old,old_count)=crate::prepare_bundle::catalog_measure::measure(||current_policy_oracle_at(&d,&ids,None,Some(AT)));let old_us=start.elapsed().as_micros();
    let start=std::time::Instant::now();let(new,new_count)=crate::prepare_bundle::catalog_measure::measure(||build_request_at(&d,&ids,None,Some(AT)));let new_us=start.elapsed().as_micros();
    let old=normalized(old).unwrap();let new=normalized(new).unwrap();assert_eq!(new,old);
    assert_eq!(old_count,101);assert_eq!(new_count,1);
    eprintln!("capture_context_benchmark {}",json!({"recipients":100,"catalogEntries":80,"oldCatalogBuilds":old_count,
        "newCatalogBuilds":new_count,"oldMicroseconds":old_us,"newMicroseconds":new_us,"exactRequestBytes":new["request"].to_string().len(),
        "note":"synthetic immutable capture; excludes SQL, lock wait, provider, paid model and source import; no elapsed threshold"}));
}

fn current_policy_oracle_at(d: &Value, ids: &[Value], instruction: Option<&str>, fact_selected_at:Option<&str>) -> Result<Value, &'static str> {
    let mut bundle = crate::prepare_bundle::build_engine_capture(d, ids, &[])?;
    bundle["request"]["purpose"] = json!("triage");
    // New work uses one complete generation, including factual and editorial
    // checks. Existing paid jobs retain their immutable captured request.
    bundle["request"]["preparationMode"] = json!("single_pass_v1");
    bundle["request"]["responseContract"] = json!("compact_decisions_v1");
    // Capture model presentation/research semantics only for newly scheduled
    // work. Recovery consumes its saved request without inserting defaults.
    bundle["request"]["modelContextContract"] = json!("shared_moderation_v1");
    bundle["request"]["researchPolicy"] = json!("context_sufficient_v1");
    bundle["request"]["researchLimitContract"] = json!("uncapped_evidence_v1");
    bundle["request"]["recoveryEvidenceContract"] = json!("held_candidates_v1");
    bundle["request"]["visualNeedContract"] = json!(crate::prepare_bundle::visual::CONTRACT);
    bundle["request"]["visualSelection"] = crate::prepare_bundle::visual::empty();
    bundle["request"]["factDependencyContract"] = json!(crate::fact_followup::CONTRACT);
    bundle["factSourceFingerprints"]=json!({});
    for id in ids.iter().filter_map(Value::as_str){
        bundle["factSourceFingerprints"][id]=json!(crate::prepare_bundle::review_fingerprint(d,id)?);
    }
    let facts=crate::fact_followup::select(d,ids,fact_selected_at.unwrap_or(&crate::now()))?;
    if let Some(materials)=bundle["request"]["materials"].as_array_mut(){
        materials.extend(facts["materials"].as_array().into_iter().flatten().cloned());
    }
    crate::decision_media::attach_request(d,&mut bundle["request"])?;
    // Independent orchestration retains the uncached base capture/fingerprint
    // path while naming each newly required policy explicitly. It never calls
    // build_request_at or its shared EvidenceContext.
    bundle["request"]["strictGroupContract"]=json!(crate::preparation_unit::CONTRACT);
    bundle["request"]["strictGroup"]=crate::preparation_unit::capture(d,ids,&crate::now())?;
    bundle["factFollowupManifest"]=facts["manifest"].clone();
    if let Some(instruction) = instruction.filter(|value| !value.is_empty()) {
        let base = bundle["request"]["instruction"].as_str().unwrap_or("");
        bundle["request"]["instruction"] = json!(format!("{base}\n\nAdditional operator instruction:\n{instruction}"));
    }
    crate::preparation_materials::attach_request(d,&mut bundle["request"])?;
    if bundle["request"].to_string().len() > MAX_CAPTURE_BYTES {
        return Err("Selected assistant evidence exceeds the 2400000-byte complete-capture budget; split recipients");
    }
    if model_request_bytes(&bundle["request"]) > MAX_REQUEST_BYTES {
        return Err("Selected assistant evidence exceeds the 550000-byte budget; reduce attachments or instruction");
    }
    if image_count(&bundle["request"]) > MAX_REQUEST_IMAGES {
        return Err(IMAGE_CAPACITY_ERROR);
    }
    rehash(&mut bundle);
    Ok(bundle)
}
