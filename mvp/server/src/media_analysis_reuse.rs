//! Current alias applicability is separate from immutable paid analysis.
//! No title/URL equality, foreign-company lookup, ASR dispatch or catalog write.
use serde_json::{json, Value};
use crate::media_artifacts::ArtifactRef;

fn text<'a>(v: &'a Value, key: &str) -> &'a str { v[key].as_str().unwrap_or("") }
fn rows<'a>(v: &'a Value, key: &str) -> &'a [Value] { v[key].as_array().map(Vec::as_slice).unwrap_or(&[]) }
fn digest(v: &Value) -> String { crate::media_fullframes::hash(v) }
fn sha(v: &Value) -> bool { v.as_str().is_some_and(|s| s.len()==64 && s.bytes().all(|c|c.is_ascii_digit()||(b'a'..=b'f').contains(&c))) }
pub(crate) fn attachment_identity(attachment:&Value)->String {
    // Cross-language identity uses known string locators only. Complete native
    // sourceVersion still binds numeric and all other attachment metadata.
    let fields=["type","sourceUrl","source_url","url","id","canonicalMediaId"];
    digest(&Value::Array(fields.iter().map(|key|attachment[*key].as_str().map(|s|json!(s)).unwrap_or(Value::Null)).collect()))
}

/// A native typed receipt parser, not a byte-verification capability. Its
/// acquisition receipt and CAS closure must be verified outside the writer by
/// the integration owner before selection and warmed for final admission.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct VerifiedAlias { company: String, target: Value, file: Value }
impl VerifiedAlias {
    pub(crate) fn current(d:&Value, progress:&Value, receipt:&Value) -> Result<Self,String> {
        crate::accounts::Profile::from_workspace(d).map_err(|_|"reuse_company_invalid")?;
        let company=text(d,"account").to_owned();
        let binding=crate::active_binding(d).map_err(|_|"reuse_connector_invalid")?.to_json();
        if d["account"]!=progress["account"] || binding!=progress["connectorBinding"]
            || receipt["companyId"]!=company || receipt["target"]["connectorBinding"]!=binding {
            return Err("reuse_company_or_connector_changed".into());
        }
        let target=&receipt["target"];
        if let Some(pin)=progress.get("assetPin"){
            crate::media_speech_assets::require_progress(d,progress)?;
            if target["assetPin"]!=*pin||target["attachmentIndex"]!=pin["attachmentIndex"]||target["attachmentIdentity"]!=pin["attachmentIdentity"]{
                return Err("reuse_selected_asset_changed".into());
            }
        }else if target.get("assetPin").is_some(){return Err("reuse_selected_asset_missing".into());}
        let posts=rows(d,"posts").iter().filter(|p|p["id"]==progress["sourcePostId"]).collect::<Vec<_>>();
        if posts.len()!=1 { return Err("reuse_target_ambiguous_or_missing".into()); }
        let post=posts[0];
        if !crate::knowledge::in_account(post,text(d,"account"))
            || (!post["connectorBinding"].is_null()&&post["connectorBinding"]!=binding)
            || post["postKey"]!=progress["sourcePostKey"]
            || crate::media_fullframes::source_version(post,text(d,"account"))!=progress["sourceVersion"]
            || target["postId"]!=post["id"] || target["postKey"]!=post["postKey"]
            || target["sourceVersion"]!=progress["sourceVersion"]
            || target["aliasRevision"].as_u64().is_none_or(|n|n==0) {
            return Err("reuse_target_changed".into());
        }
        // An index plus the exact attachment projection prevents a multi-video
        // post from bridging its unrelated attachments. No whole-post fallback.
        let index=target["attachmentIndex"].as_u64().ok_or("reuse_attachment_missing")?;
        let attachment=rows(post,"attachments").get(usize::try_from(index).map_err(|_|"reuse_attachment_invalid")?)
            .ok_or("reuse_attachment_missing")?;
        if target["attachmentIdentity"]!=attachment_identity(attachment)
            || !matches!(text(attachment,"type"),"video"|"clip"|"reel")
            || !crate::knowledge::in_account(attachment,text(d,"account")) {
            return Err("reuse_attachment_changed".into());
        }
        let file=&receipt["verifiedFile"];
        let reference=ArtifactRef::from_json(&json!({"sha256":file["sha256"],"bytes":file["bytes"]}))
            .map_err(|_|"reuse_file_invalid")?;
        let source=ArtifactRef::from_json(&progress["source"]).map_err(|_|"reuse_source_invalid")?;
        if reference!=source || !sha(&file["receiptSha256"]) || !sha(&file["probeSha256"])
            || progress["sourceIdentity"]["account"]!=progress["account"]
            || progress["sourceIdentity"]["postKey"]!=progress["sourcePostKey"]
            || progress["sourceIdentity"]["mediaSha256"]!=reference.sha256 {
            return Err("reuse_verified_file_changed".into());
        }
        Ok(Self{company,target:target.clone(),file:file.clone()})
    }
    pub(crate) fn request(&self,spec:&str)->Value {
        json!({"companyId":self.company,"verifiedFile":self.file,"stage":"asr","specSha256":spec})
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ExactFileReuseProof { pub(crate) value: Value }

/// Select the current attachment from the admitted source projection. A single
/// video is unambiguous; multiple videos require one exact selected locator.
pub(crate) fn receipt_for_request(d:&Value,progress:&Value,request:&Value)->Result<Value,String> {
    let post=rows(d,"posts").iter().find(|p|p["id"]==progress["sourcePostId"]).ok_or("reuse_target_missing")?;
    let candidates=rows(post,"attachments").iter().enumerate().filter(|(_,a)|matches!(text(a,"type"),"video"|"clip"|"reel")).collect::<Vec<_>>();
    let matching=candidates.iter().filter(|(_,a)|["sourceUrl","source_url","url"].iter().any(|k|
        !text(a,k).is_empty()&&["sourceUrl","fallbackUrl"].iter().any(|s|a[*k]==progress["sourceProjection"][*s]))).collect::<Vec<_>>();
    let (index,attachment)=if let Some(pin)=progress.get("assetPin"){
        crate::media_speech_assets::require_progress(d,progress)?;
        if request["originalAlias"]["assetPin"]!=*pin{return Err("reuse_request_selected_asset_changed".into());}
        candidates.iter().copied().find(|(index,a)|pin["attachmentIndex"]==*index&&pin["attachmentIdentity"]==attachment_identity(a)).ok_or("reuse_selected_attachment_missing")?
    }else if candidates.len()==1 {candidates[0]} else if matching.len()==1 {*matching[0]} else {return Err("reuse_attachment_ambiguous".into())};
    let mut receipt=json!({"companyId":d["account"],"verifiedFile":request["verifiedFile"],"target":{
        "connectorBinding":progress["connectorBinding"],"postId":post["id"],"postKey":post["postKey"],
        "sourceVersion":progress["sourceVersion"],"attachmentIndex":index,"attachmentIdentity":attachment_identity(attachment),
        "aliasRevision":progress["leaseEpoch"].as_u64().filter(|n|*n>0).unwrap_or(1)}});
    if let Some(pin)=progress.get("assetPin"){receipt["target"]["assetPin"]=pin.clone();}
    VerifiedAlias::current(d,progress,&receipt)?;Ok(receipt)
}

fn validate_screen(pin:&Value,outcome:&Value)->Result<(),String> {
    let ocr=&outcome["ocr"];
    if outcome["account"]!=pin["account"]||outcome["postKey"]!=pin["target"]["postKey"]
        ||outcome["mediaSha256"]!=pin["verifiedFile"]["sha256"]
        ||ocr["sourceVersion"]!=pin["target"]["sourceVersion"]||ocr["coverage"]!="sampled_frames"
        ||ocr["exhaustive"]!=false||ocr["failedFrames"]!=0
        ||ocr["sampledFrames"].as_u64().is_none_or(|n|n==0||n>30)
        ||!matches!(text(ocr,"status"),"completed"|"no_text_found") {
        return Err("reuse_current_screen_invalid".into());
    }Ok(())
}
pub(crate) fn attach_current_screen_text(pin:&mut Value,outcome:&Value)->Result<(),String> {
    validate_screen(pin,outcome)?;pin["currentScreenText"]=outcome.clone();Ok(())
}

fn audio_only_pin(pin:&Value)->Value {
    let mut plain=pin.clone();plain.as_object_mut().unwrap().remove("currentScreenText");plain
}

fn complete(material:&Value,result:&Value,file:&Value)->bool {
    let tr=&material["transcription"];
    let duration=result["coverage"]["durationMs"].as_u64().filter(|n|*n>0);
    let media_ms=tr["mediaDurationSeconds"].as_f64().filter(|n|n.is_finite()&&*n>0.0&&*n<(u64::MAX/1000) as f64).map(|n|(n*1000.0).round() as u64);
    if material["kind"]!="transcript"||text(material,"text").trim().is_empty()
        || material["mediaSha256"]!=file["sha256"] || tr["partial"]!=false
        || duration.zip(media_ms).is_none_or(|(a,b)|a.abs_diff(b)>250) { return false; }
    match text(tr,"coverage") {
        "full_audio"=>result["coverage"]["kind"]=="full_audio"&&tr["audioDurationSeconds"].as_f64()
            .is_some_and(|n|n.is_finite()&&n>0.0&&n<(u64::MAX/1000) as f64
                &&Some(((n*1000.0).round() as u64).saturating_add(250))>=media_ms),
        "no_audio_stream"=>result["coverage"]["kind"]=="no_audio_stream"&&tr["audioStatus"]=="no_audio_stream"&&tr["audioDurationSeconds"].is_null(),
        _=>false,
    }
}

fn donor_pin(d:&Value,material:&Value,at:&str)->Result<Value,String> {
    crate::knowledge::validate_catalog(d).map_err(str::to_owned)?;
    let entries=rows(d,"knowledge_entries").iter().filter(|e|e["sourceMaterialId"]==material["id"]).collect::<Vec<_>>();
    if entries.len()>1 {return Err("reuse_donor_ambiguous".into());}
    let Some(entry)=entries.first() else {
        if rows(d,"knowledge_versions").iter().any(|v|v["sourceMaterialId"]==material["id"]) {
            return Err("reuse_donor_head_missing".into());
        }
        return Ok(Value::Null)
    };
    let version=rows(d,"knowledge_versions").iter().find(|v|v["id"]==entry["currentVersionId"]).ok_or("reuse_donor_missing")?;
    let now=chrono::DateTime::parse_from_rfc3339(at).map_err(|_|"reuse_timestamp_invalid")?;
    let from=version["validFrom"].as_str().map(chrono::DateTime::parse_from_rfc3339).transpose().map_err(|_|"reuse_timestamp_invalid")?;
    let until=version["validUntil"].as_str().map(chrono::DateTime::parse_from_rfc3339).transpose().map_err(|_|"reuse_timestamp_invalid")?;
    if version["status"]!="active"||!matches!(text(version,"trust"),"verified"|"source_only")
        ||from.is_some_and(|t|t>now)||until.is_some_and(|t|t<=now) {return Err("reuse_donor_not_applicable".into());}
    // A corrected current head must be selected anew. It cannot silently bless
    // the old normalized output, nor does the mismatch authorize another ASR.
    if version["scope"]["account"]!=d["account"] || !crate::knowledge::in_account(version,text(d,"account"))
        || ["text","transcription","mediaSha256","postKey","sourceUrl"].iter().any(|k|version[*k]!=material[*k]) {
        return Err("reuse_donor_head_changed".into());
    }
    Ok(json!({"entryId":entry["id"],"versionId":version["id"],"sha256":version["hash"],"scope":version["scope"]}))
}

/// Bootstrap selector for runtime-owned adoption. This proves original catalog
/// material; it grants no current target applicability and performs no ASR.
pub(crate) fn select_catalog_adoption(d:&Value,file:&Value,at:&str)->Result<Option<Value>,String> {
    crate::knowledge::validate_catalog(d).map_err(str::to_owned)?;
    let now=chrono::DateTime::parse_from_rfc3339(at).map_err(|_|"reuse_timestamp_invalid")?;
    let binding=crate::active_binding(d).map_err(|_|"reuse_connector_invalid")?.to_json();
    ArtifactRef::from_json(&json!({"sha256":file["sha256"],"bytes":file["bytes"]})).map_err(|_|"reuse_file_invalid")?;
    for entry in rows(d,"knowledge_entries") {
        let Some(v)=rows(d,"knowledge_versions").iter().find(|v|v["id"]==entry["currentVersionId"]) else {continue};
        if v["kind"]!="transcript" || v["scope"]["account"]!=d["account"]
            || !crate::knowledge::in_account(v,text(d,"account"))
            || (!v["connectorBinding"].is_null()&&v["connectorBinding"]!=binding)
            || v["mediaSha256"]!=file["sha256"] || !matches!(text(v,"trust"),"verified"|"source_only") {continue;}
        let from=v["validFrom"].as_str().map(chrono::DateTime::parse_from_rfc3339).transpose().map_err(|_|"reuse_timestamp_invalid")?;
        let until=v["validUntil"].as_str().map(chrono::DateTime::parse_from_rfc3339).transpose().map_err(|_|"reuse_timestamp_invalid")?;
        if from.is_some_and(|t|t>now)||until.is_some_and(|t|t<=now) {continue;}
        let tr=&v["transcription"];
        if text(tr,"sourceVersion").is_empty()||tr["sourcePostKey"]!=v["postKey"]||text(v,"postKey").is_empty() {continue;}
        let duration=tr["mediaDurationSeconds"].as_f64().filter(|n|n.is_finite()&&*n>0.0&&*n<(u64::MAX/1000) as f64).map(|n|(n*1000.0).round() as u64);
        let Some(duration)=duration else {continue};
        let coverage=json!({"kind":tr["coverage"],"durationMs":duration});
        let mut material=json!({"id":v["sourceMaterialId"],"title":v["title"],"kind":"transcript","text":v["text"],
            "account":d["account"],"postKey":v["postKey"],"sourceUrl":v["sourceUrl"],"mediaSha256":v["mediaSha256"],"transcription":tr});
        for key in ["connectorBinding","canonicalMediaId","contentSha256","attachments"] {if v.get(key).is_some(){material[key]=v[key].clone();}}
        if !complete(&material,&json!({"coverage":coverage}),file) {continue;}
        let donor=donor_pin(d,&material,at)?;
        let outcome=if tr["coverage"]=="no_audio_stream" {"no_audio"} else if tr["audioStatus"]=="inspected_no_speech" {"no_speech"} else {"transcript"};
        let audio=json!({"materials":[material],"reused":false,"coverage":coverage,"outcome":outcome});
        return Ok(Some(json!({"material":material,"audio":audio,"donor":donor,"coverage":coverage,"outcome":outcome,
            "provenance":{"kind":"verified_legacy_catalog","originalSourceVersion":tr["sourceVersion"],
                "originalSourceUrl":v["sourceUrl"],"catalogStatus":v["status"],"rawOutputsAbsent":true}})));
    }Ok(None)
}

/// Pure selector shared by selection and final writer admission. The payload
/// was resolved from normalizedOutput and hash checked outside the writer.
pub(crate) fn select_prepared(d:&Value,progress:&Value,ledger:&Value,receipt:&Value,
    payload:&Value,spec:&str)->Result<Option<(Value,ExactFileReuseProof)>,String> {
    select_prepared_at(d,progress,ledger,receipt,payload,spec,&crate::now())
}
pub(crate) fn select_prepared_at(d:&Value,progress:&Value,ledger:&Value,receipt:&Value,
    payload:&Value,spec:&str,at:&str)->Result<Option<(Value,ExactFileReuseProof)>,String> {
    let alias=VerifiedAlias::current(d,progress,receipt)?;
    if !sha(&json!(spec)) {return Err("reuse_spec_invalid".into());}
    let selected=crate::media_analysis::read_result(ledger,&alias.request(spec))?;
    if selected["disposition"]!="reuse" {return Ok(None);}
    let result=&selected["result"];
    let output=ArtifactRef::from_json(&result["normalizedOutput"]).map_err(|_|"reuse_output_invalid")?;
    let encoded=payload.to_string();
    if output.sha256!=digest(payload)||output.bytes!=encoded.len() as u64 {return Err("reuse_output_hash_changed".into());}
    if payload["schemaVersion"]!=1 || payload["binding"]["companyId"]!=alias.company
        || payload["binding"]["verifiedFile"]["sha256"]!=alias.file["sha256"]
        || payload["binding"]["verifiedFile"]["bytes"]!=alias.file["bytes"]
        || payload["binding"]["specSha256"]!=spec {
        return Err("reuse_original_output_binding_changed".into());
    }
    let materials=rows(&payload["audio"],"materials");
    let transcripts=materials.iter().filter(|m|m["kind"]=="transcript").collect::<Vec<_>>();
    if transcripts.len()!=1 {return Err("reuse_original_transcript_missing".into());}
    let original=transcripts[0];
    if original["account"]!=d["account"]||!crate::knowledge::in_account(original,text(d,"account"))
        || text(original,"postKey").is_empty()||text(&original["transcription"],"sourceVersion").is_empty()
        || original["transcription"]["sourcePostKey"]!=original["postKey"]
        || !complete(original,result,&alias.file) {return Err("reuse_original_provenance_or_coverage_invalid".into());}
    let donor=donor_pin(d,original,at)?;
    let proof=json!({"schemaVersion":1,"kind":"verified_exact_file_analysis_reuse",
        "companyId":alias.company,"account":d["account"],"target":alias.target,"verifiedFile":alias.file,
        "resultSha256":result["resultSha256"],"specSha256":spec,"stage":"asr",
        "result":result,"donor":donor,"originalMaterial":original,"normalizedPayload":payload,
        "match":"exact_media_sha256","sourceMaterialId":original["id"],"sourcePostKey":original["postKey"],
        "transcription":original["transcription"],"screenReuse":false});
    Ok(Some((original.clone(),ExactFileReuseProof{value:proof})))
}

/// Outside-writer CAS readback. A hash field or an equal URL is not verification.
pub(crate) fn select_verified(d:&Value,progress:&Value,ledger:&Value,receipt:&Value,spec:&str)
    ->Result<Option<(Value,Value)>,String> {
    let alias=VerifiedAlias::current(d,progress,receipt)?;
    let selected=crate::media_analysis::read_result(ledger,&alias.request(spec))?;
    if selected["disposition"]!="reuse" {return Ok(None);}
    let store=crate::media_fullframes::store()?;
    crate::media_fullframes::verify_reference(&store,&progress["source"])?;
    let payload=crate::media_fullframes::read(&store,&selected["result"]["normalizedOutput"])?;
    Ok(select_prepared(d,progress,ledger,receipt,&payload,spec)?.map(|(material,proof)|(material,proof.value)))
}

/// No CAS I/O here. The integration owner must revalidate its warmed artifact
/// closure alongside this pure currentness check in the final transaction.
pub(crate) fn validate_pin(d:&Value,progress:&Value,ledger:&Value,receipt:&Value,pin:&Value)->Result<(),String> {
    let selected=select_prepared(d,progress,ledger,receipt,&pin["normalizedPayload"],text(pin,"specSha256"))?;
    if selected.as_ref().map(|(_,proof)|&proof.value)!=Some(&audio_only_pin(pin)) {return Err("reuse_applicability_changed".into());}
    if let Some(screen)=pin.get("currentScreenText") {validate_screen(pin,screen)?;}
    Ok(())
}

pub(crate) fn admission_request(pin:&Value)->Result<Value,String> {
    if pin["schemaVersion"]!=1||pin["kind"]!="verified_exact_file_analysis_reuse"||!sha(&pin["resultSha256"])
        {return Err("reuse_pin_invalid".into());}
    Ok(json!({"companyId":pin["companyId"],"verifiedFile":pin["verifiedFile"],"stage":"asr",
        "specSha256":pin["specSha256"],"target":pin["target"],"resultSha256":pin["resultSha256"],
        "proofSha256":digest(pin),"donor":pin["donor"],"currentTarget":pin["target"],"currentDonor":pin["donor"]}))
}

fn durable(pin:&Value)->Value {
    let mut pin=pin.clone();
    for key in ["normalizedPayload","originalMaterial","transcription"] {pin.as_object_mut().unwrap().remove(key);}
    pin
}
fn progress_for(pin:&Value)->Value {
    let mut progress=json!({"account":pin["account"],"connectorBinding":pin["target"]["connectorBinding"],
        "sourcePostId":pin["target"]["postId"],"sourcePostKey":pin["target"]["postKey"],
        "sourceVersion":pin["target"]["sourceVersion"],
        "source":{"sha256":pin["verifiedFile"]["sha256"],"bytes":pin["verifiedFile"]["bytes"]},
        "sourceIdentity":{"account":pin["account"],"postKey":pin["target"]["postKey"],"mediaSha256":pin["verifiedFile"]["sha256"]}});
    if let Some(asset)=pin["target"].get("assetPin"){progress["assetPin"]=asset.clone();}progress
}
fn receipt_for(pin:&Value)->Value {json!({"companyId":pin["companyId"],"target":pin["target"],"verifiedFile":pin["verifiedFile"]})}
/// Applicability is a native completed evidence job, never a new worker lane.
/// Writer admission must also check the externally warmed closure via bindings.
pub(crate) fn admit(d:&mut Value,progress:&Value,ledger:&Value,receipt:&Value,pin:&Value,at:&str)->Result<Value,String> {
    validate_pin(d,progress,ledger,receipt,pin)?;
    let proof=durable(pin);
    if !cache().lock().map_err(|_|"reuse_proof_cache_unavailable")?.get(&digest(&proof))
        .is_some_and(|w|w.until>std::time::Instant::now()&&w.valid.load(std::sync::atomic::Ordering::Acquire)) {return Err("reuse_proof_not_warmed".into());}
    let id=format!("media-applicability-{}",digest(&proof));
    let jobs=d["jobs"].as_array_mut().ok_or("reuse_jobs_missing")?;
    if let Some(existing)=jobs.iter().find(|j|j["id"]==id) {
        if existing["result"]["proof"]!=proof {return Err("reuse_proof_collision".into());}
        return Ok(existing.clone());
    }
    let job=json!({"id":id,"kind":"media_analysis_applicability","status":"completed",
        "account":proof["account"],"createdAt":at,"completedAt":at,"result":{"schemaVersion":1,"proof":proof}});
    jobs.push(job.clone());Ok(job)
}

// This cache contains verified immutable evidence only; media_analysis remains
// the sole durable asset/attempt owner. It neither schedules nor authorizes ASR.
#[derive(Clone)]
struct Warmed { payload:std::sync::Arc<Value>, valid:std::sync::Arc<std::sync::atomic::AtomicBool>, until:std::time::Instant }
static WARMED:std::sync::OnceLock<std::sync::Mutex<std::collections::BTreeMap<String,Warmed>>>=std::sync::OnceLock::new();
fn cache()->&'static std::sync::Mutex<std::collections::BTreeMap<String,Warmed>> {WARMED.get_or_init(||std::sync::Mutex::new(std::collections::BTreeMap::new()))}

#[derive(Clone,PartialEq,Eq)]
struct FileStamp {
    bytes:u64, modified:Option<std::time::SystemTime>, created:Option<std::time::SystemTime>,
    #[cfg(unix)] identity:(u64,u64,i64,i64),
}
fn file_stamp(path:&std::path::Path)->Result<FileStamp,String> {
    // Reject observed links/reparse points at every ancestor, including the
    // store, objects directory and SHA prefix on the stamp-only fast path.
    for parent in path.ancestors() {
        if parent.as_os_str().is_empty(){continue;}
        let metadata=std::fs::symlink_metadata(parent).map_err(|_|"reuse_closure_file_missing")?;
        if metadata.file_type().is_symlink(){return Err("reuse_closure_linked".into());}
        #[cfg(windows)] {use std::os::windows::fs::MetadataExt;if metadata.file_attributes()&0x400!=0{return Err("reuse_closure_linked".into());}}
        if parent!=path&&!metadata.is_dir(){return Err("reuse_closure_parent_changed".into());}
    }
    let metadata=std::fs::symlink_metadata(path).map_err(|_|"reuse_closure_file_missing")?;
    if !metadata.is_file(){return Err("reuse_closure_file_changed".into());}
    #[cfg(unix)] let identity={use std::os::unix::fs::MetadataExt;(metadata.dev(),metadata.ino(),metadata.ctime(),metadata.ctime_nsec())};
    Ok(FileStamp{bytes:metadata.len(),modified:metadata.modified().ok(),created:metadata.created().ok(),#[cfg(unix)] identity})
}
struct VerifiedClosure {
    identity:String, payload:std::sync::Arc<Value>, files:Vec<(std::path::PathBuf,FileStamp)>,
    valid:std::sync::Arc<std::sync::atomic::AtomicBool>,
    #[cfg(test)] ready_transitions:std::sync::atomic::AtomicUsize,
}
#[derive(Default)]
struct ClosureSlot {
    entry:std::sync::Mutex<Option<std::sync::Arc<VerifiedClosure>>>,
    #[cfg(test)] heavy:std::sync::atomic::AtomicUsize,
}
static CLOSURES:std::sync::OnceLock<std::sync::Mutex<std::collections::BTreeMap<String,std::sync::Arc<ClosureSlot>>>>=std::sync::OnceLock::new();
fn closures()->&'static std::sync::Mutex<std::collections::BTreeMap<String,std::sync::Arc<ClosureSlot>>> {
    CLOSURES.get_or_init(||std::sync::Mutex::new(std::collections::BTreeMap::new()))
}
fn closure_key(pin:&Value,root:&std::path::Path)->String {
    digest(&json!([root.to_string_lossy(),pin["companyId"],pin["resultSha256"]]))
}
fn immutable_identity(pin:&Value)->Result<String,String> {
    if pin["schemaVersion"]!=1||pin["kind"]!="verified_exact_file_analysis_reuse"||pin["companyId"]!=pin["account"]
        ||!sha(&pin["resultSha256"]) {return Err("reuse_pin_invalid".into());}
    let result=&pin["result"];
    let mut unsigned=result.clone();unsigned.as_object_mut().ok_or("reuse_result_invalid")?.remove("resultSha256");
    if result["resultSha256"]!=pin["resultSha256"]||digest(&unsigned)!=text(pin,"resultSha256")
        ||result["companyId"]!=pin["companyId"]||result["specSha256"]!=pin["specSha256"]
        ||result["verifiedFile"]["sha256"]!=pin["verifiedFile"]["sha256"]
        ||result["verifiedFile"]["bytes"]!=pin["verifiedFile"]["bytes"]
        ||result["verifiedFile"]["probeSha256"]!=pin["verifiedFile"]["probeSha256"] {return Err("reuse_result_changed".into());}
    let original=&result["adoption"]["originalKnowledge"];
    if result["sourceKind"]=="legacy_adopted"&&(pin["donor"].is_null()
        ||original["entryId"]!=pin["donor"]["entryId"]||original["versionId"]!=pin["donor"]["versionId"]
        ||original["sha256"]!=pin["donor"]["sha256"]) {return Err("reuse_legacy_adoption_invalid".into());}
    Ok(digest(&json!([pin["companyId"],pin["verifiedFile"]["sha256"],pin["verifiedFile"]["bytes"],pin["verifiedFile"]["probeSha256"],result])))
}
fn closure_stamps(store:&crate::media_artifacts::ArtifactStore,pin:&Value)->Result<Vec<(std::path::PathBuf,FileStamp)>,String> {
    let result=&pin["result"];
    // Read the actual manifest, not the ledger's mirrored segment list. This
    // includes inherited recovery segments and all raw/normalized CAS outputs.
    let manifest=crate::media_fullframes::read(store,&result["manifest"])?;
    let mut references=vec![json!({"sha256":pin["verifiedFile"]["sha256"],"bytes":pin["verifiedFile"]["bytes"]}),result["manifest"].clone(),result["normalizedOutput"].clone()];
    if result["sourceKind"]!="legacy_adopted" {
        for segment in manifest["segments"].as_array().ok_or("reuse_closure_segments_missing")? {
            references.push(segment["rawOutput"].clone());references.push(segment["normalizedOutput"].clone());
        }
    }
    let mut files=Vec::new();let mut seen=std::collections::BTreeSet::new();
    for reference in references {
        let parsed=ArtifactRef::from_json(&reference).map_err(|_|"reuse_closure_reference_invalid")?;
        let expected=store.root().join("objects").join(&parsed.sha256[..2]).join(&parsed.sha256);
        let before=file_stamp(&expected)?;
        let checked=crate::media_fullframes::verify_reference(store,&reference)?;
        if checked!=expected||before.bytes!=parsed.bytes||file_stamp(&checked)?!=before {return Err("reuse_closure_changed_during_verification".into());}
        if seen.insert(checked.clone()){files.push((checked,before));}
    }Ok(files)
}
fn cached_immutable(pin:&Value)->Result<std::sync::Arc<VerifiedClosure>,String> {
    let identity=immutable_identity(pin)?;
    let store=crate::media_fullframes::store()?;let key=closure_key(pin,store.root());
    let slot=closures().lock().map_err(|_|"reuse_proof_cache_unavailable")?
        .entry(key).or_insert_with(||std::sync::Arc::new(ClosureSlot::default())).clone();
    // The result/company singleflight mutex is outside every global cache lock.
    // It only coordinates outside-writer verification, never writer admission.
    let mut entry=slot.entry.lock().map_err(|_|"reuse_proof_cache_unavailable")?;
    if let Some(verified)=entry.as_ref() {
        if verified.identity!=identity {return Err("reuse_cached_identity_changed".into());}
        if verified.valid.load(std::sync::atomic::Ordering::Acquire)
            &&verified.files.iter().all(|(path,stamp)|file_stamp(path).is_ok_and(|now|now==*stamp)) {return Ok(verified.clone());}
        // Every alias sharing this decoded result loses readiness immediately.
        verified.valid.store(false,std::sync::atomic::Ordering::Release);
        crate::media_fullframes::invalidate_proof_epoch();*entry=None;
    }
    #[cfg(test)] slot.heavy.fetch_add(1,std::sync::atomic::Ordering::Relaxed);
    let before=closure_stamps(&store,pin)?;
    let payload=verify_immutable(pin)?;
    if !before.iter().all(|(path,stamp)|file_stamp(path).is_ok_and(|now|now==*stamp)) {return Err("reuse_closure_changed_during_verification".into());}
    let verified=std::sync::Arc::new(VerifiedClosure{identity,payload:std::sync::Arc::new(payload),files:before,
        valid:std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true)),
        #[cfg(test)] ready_transitions:std::sync::atomic::AtomicUsize::new(0)});
    *entry=Some(verified.clone());Ok(verified)
}

/// Outside the writer. Verify original source, segment output closure and full
/// normalized output before current target applicability becomes selectable.
pub(crate) fn warm_pin(d:&Value,pin:&Value)->Result<(),String> {
    let key=digest(&durable(pin));
    let checked=(|| {
        let ledger=crate::media_analysis::ledger_from_workspace(d)?;
        let progress=progress_for(pin);let receipt=receipt_for(pin);
        let alias=VerifiedAlias::current(d,&progress,&receipt)?;
        let selected=crate::media_analysis::read_result(&ledger,&alias.request(text(pin,"specSha256")))?;
        if selected["disposition"]!="reuse"||selected["result"]!=pin["result"] {return Err("reuse_result_changed".into());}
        let verified=cached_immutable(pin)?;
        let Some((_,prepared))=select_prepared(d,&progress,&ledger,&receipt,&verified.payload,text(pin,"specSha256"))? else {return Err("reuse_result_unavailable".into())};
        if durable(&prepared.value)!=durable(&audio_only_pin(pin)) {return Err("reuse_applicability_changed".into());}
        Ok(verified)
    })();
    publish_warmed(key,checked)
}
fn publish_warmed(key:String,verified:Result<std::sync::Arc<VerifiedClosure>,String>)->Result<(),String> {
    let mut cache=cache().lock().map_err(|_|"reuse_proof_cache_unavailable")?;
    let was_ready=cache.get(&key).is_some_and(|w|w.until>std::time::Instant::now()&&w.valid.load(std::sync::atomic::Ordering::Acquire));
    match verified {
        Ok(verified)=>{cache.insert(key,Warmed{payload:verified.payload.clone(),valid:verified.valid.clone(),until:std::time::Instant::now()+std::time::Duration::from_secs(120)});
            if !was_ready {crate::media_fullframes::invalidate_proof_epoch();#[cfg(test)] verified.ready_transitions.fetch_add(1,std::sync::atomic::Ordering::Relaxed);}Ok(())},
        Err(error)=>{if cache.remove(&key).is_some(){crate::media_fullframes::invalidate_proof_epoch();}Err(error)},
    }
}
fn verify_immutable(pin:&Value)->Result<Value,String> {
    if pin["schemaVersion"]!=1||pin["kind"]!="verified_exact_file_analysis_reuse"||pin["companyId"]!=pin["account"]
        || !sha(&pin["resultSha256"]) {return Err("reuse_pin_invalid".into());}
    let store=crate::media_fullframes::store()?;
    crate::media_fullframes::verify_reference(&store,&json!({"sha256":pin["verifiedFile"]["sha256"],"bytes":pin["verifiedFile"]["bytes"]}))?;
    let result=&pin["result"];
    let mut unsigned=result.clone();unsigned.as_object_mut().ok_or("reuse_result_invalid")?.remove("resultSha256");
    if result["resultSha256"]!=pin["resultSha256"]||digest(&unsigned)!=text(pin,"resultSha256")
        || result["companyId"]!=pin["companyId"]||result["verifiedFile"]["sha256"]!=pin["verifiedFile"]["sha256"]
        || result["verifiedFile"]["bytes"]!=pin["verifiedFile"]["bytes"] {return Err("reuse_result_changed".into());}
    if result["sourceKind"]=="legacy_adopted" {
        let original=&result["adoption"]["originalKnowledge"];
        if original["entryId"]!=pin["donor"]["entryId"]||original["versionId"]!=pin["donor"]["versionId"]
            ||original["sha256"]!=pin["donor"]["sha256"]||pin["donor"].is_null()
            ||result["adoption"]["compatibility"]["policy"]!="verified_legacy_normalized_full_audio" {
            return Err("reuse_legacy_adoption_invalid".into());
        }
        let manifest=crate::media_fullframes::read(&store,&result["manifest"])?;
        if manifest["normalizedOutput"]!=result["normalizedOutput"]||manifest["originalKnowledge"]!=*original {
            return Err("reuse_legacy_manifest_invalid".into());
        }
    } else {
        let capture=json!({"manifest":result["manifest"],"normalizedOutput":result["normalizedOutput"],
            "coverage":result["coverage"],"outcome":result["outcome"],"verificationSha256":result["verificationSha256"]});
        crate::media_processing::analysis_output::read_full(&store,&result["originalRequest"],&capture)?;
    }
    let payload=crate::media_fullframes::read(&store,&result["normalizedOutput"])?;
    let output=ArtifactRef::from_json(&result["normalizedOutput"]).map_err(|_|"reuse_output_invalid")?;
    if digest(&payload)!=output.sha256||payload.to_string().len() as u64!=output.bytes
        ||payload["binding"]["companyId"]!=pin["companyId"]||payload["binding"]["verifiedFile"]["sha256"]!=pin["verifiedFile"]["sha256"]
        ||payload["binding"]["verifiedFile"]["bytes"]!=pin["verifiedFile"]["bytes"] {return Err("reuse_output_hash_changed".into());}
    Ok(payload)
}
/// Narrow persisted pin read restores the ephemeral proof cache after restart.
/// Alias/head/current-result checks still run against each consumer's snapshot.
pub(crate) async fn refresh(app:&crate::App)->crate::ApiResult<()> {
    let pins=app.db.read_media_analysis_proofs().await?;
    tokio::task::spawn_blocking(move || for pin in pins {let _=warm_immutable_pin(&pin);})
        .await.map_err(|_|crate::internal("Media analysis proof refresh stopped"))?;Ok(())
}
pub(crate) fn warm_immutable_pin(pin:&Value)->Result<(),String> {
    let key=digest(&durable(pin));
    publish_warmed(key,cached_immutable(pin))
}
pub(crate) fn warm_workspace(d:&Value)->Result<(),String> {
    for job in rows(d,"jobs").iter().filter(|j|j["kind"]=="media_analysis_applicability"&&j["status"]=="completed") {
        // Stale aliases remain historical evidence. They must not prevent an
        // unrelated asset from becoming ready, and cannot be selected below.
        let _=warm_pin(d,&job["result"]["proof"]);
    }Ok(())
}
/// Pure currentness readback, shared by discovery, preparation and dispatch.
/// Returns original paid material and target-specific edge; never copies OCR.
pub(crate) fn bindings(d:&Value,at:&str)->Result<Vec<Value>,String> {
    // Knowledge-only projections intentionally omit jobs. They have no retained
    // analysis/applicability authority, even if an immutable proof is warmed.
    // A present but malformed collection still reaches the strict ledger reader.
    if d.get("jobs").is_none() {
        d["account"].as_str().filter(|s|!s.trim().is_empty()).ok_or("media_analysis_missing_account")?;
        chrono::DateTime::parse_from_rfc3339(at).map_err(|_|"reuse_timestamp_invalid")?;
        return Ok(Vec::new());
    }
    let ledger=crate::media_analysis::ledger_from_workspace(d)?;
    let now=chrono::DateTime::parse_from_rfc3339(at).map_err(|_|"reuse_timestamp_invalid")?;
    let mut selected=Vec::new();
    for job in rows(d,"jobs").iter().filter(|j|j["kind"]=="media_analysis_applicability"&&j["status"]=="completed"&&j["account"]==d["account"]) {
        if chrono::DateTime::parse_from_rfc3339(text(job,"completedAt")).map_err(|_|"reuse_timestamp_invalid")?>now {continue;}
        let pin=&job["result"]["proof"];
        let warmed=cache().lock().map_err(|_|"reuse_proof_cache_unavailable")?.get(&digest(pin)).cloned();
        let Some(warmed)=warmed.filter(|w|w.until>std::time::Instant::now()&&w.valid.load(std::sync::atomic::Ordering::Acquire)) else {continue};
        let progress=progress_for(pin);let receipt=receipt_for(pin);
        let Ok(Some((material,prepared)))=select_prepared_at(d,&progress,&ledger,&receipt,&warmed.payload,text(pin,"specSha256"),at) else {continue};
        if durable(&prepared.value)!=audio_only_pin(pin) {continue;}
        if pin.get("currentScreenText").is_some_and(|screen|validate_screen(pin,screen).is_err()) {continue;}
        let donor=&pin["donor"];
        let transcript=if donor.is_null() {json!({"entryId":format!("analysis:{}",text(pin,"resultSha256")),
            "versionId":format!("analysis-result:{}",text(pin,"resultSha256")),"hash":pin["resultSha256"]})}
            else {json!({"entryId":donor["entryId"],"versionId":donor["versionId"],"hash":donor["sha256"]})};
        let edge=json!({"schemaVersion":1,"match":"verified_exact_file_analysis_reuse","postKey":pin["target"]["postKey"],
            "targetPostId":pin["target"]["postId"],"sourcePostKey":material["postKey"],"companyId":pin["companyId"],
            "connectorBinding":pin["target"]["connectorBinding"],"target":pin["target"],"verifiedFile":pin["verifiedFile"],
            "proofSha256":digest(pin),"resultSha256":pin["resultSha256"],"specSha256":pin["specSha256"],
            "normalizedOutput":pin["result"]["normalizedOutput"],"transcript":transcript,
            "originalSourceVersion":material["transcription"]["sourceVersion"],"coverage":pin["result"]["coverage"],"screenReuse":false});
        selected.push(json!({"edge":edge,"material":material,"currentScreenText":pin["currentScreenText"]}));
    }
    selected.sort_by_key(|v|v["edge"].to_string());selected.dedup();Ok(selected)
}

/// Supplies one immutable material per result and multiple explicit aliases.
/// Analysis-derived IDs are their own namespace, never fabricated catalog heads.
pub(crate) fn append_selected(bundle:&mut Value,bindings:&[Value],targets:&[&Value])->Result<(),String> {
    let mut grouped:std::collections::BTreeMap<String,(Value,Vec<Value>)>=std::collections::BTreeMap::new();
    for binding in bindings.iter().filter(|b|targets.iter().any(|p|p["id"]==b["edge"]["targetPostId"])) {
        let edge=&binding["edge"];let key=text(&edge["transcript"],"versionId").to_owned();
        let group=grouped.entry(key).or_insert_with(||(binding["material"].clone(),Vec::new()));
        group.1.push(edge.clone());
    }
    for (_, (mut material,edges)) in grouped {
        let reference=&edges[0]["transcript"];
        material["knowledgeEntryId"]=reference["entryId"].clone();material["knowledgeVersionId"]=reference["versionId"].clone();
        material["trust"]=json!("source_only");material["exactFileAnalysisReuse"]=json!(edges);
        // ASR provenance may contain an old optional OCR annotation. This does
        // not supply screen claims; the typed edge expressly denies screen reuse.
        let provenance=json!({"entryId":reference["entryId"],"versionId":reference["versionId"],"hash":reference["hash"],
            "kind":"transcript","scope":{"account":material["account"],"postKeys":[material["postKey"]]},
            "trust":"source_only","mediaBinding":edges});
        let materials=bundle["materials"].as_array_mut().ok_or("reuse_bundle_invalid")?;
        if let Some(existing)=materials.iter_mut().find(|m|m["knowledgeVersionId"]==material["knowledgeVersionId"]) {
            existing["exactFileAnalysisReuse"]=material["exactFileAnalysisReuse"].clone();
        } else {materials.push(material);}
        let manifest=bundle["manifest"].as_array_mut().ok_or("reuse_bundle_invalid")?;
        if let Some(existing)=manifest.iter_mut().find(|m|m["versionId"]==provenance["versionId"]) {
            let bindings=existing.as_object_mut().ok_or("reuse_bundle_invalid")?.entry("mediaBinding").or_insert_with(||json!([]));
            bindings.as_array_mut().ok_or("reuse_bundle_invalid")?.extend(edges);
        } else {manifest.push(provenance);}
    }Ok(())
}

#[cfg(test)]
fn test_evict_pin(pin:&Value) {
    cache().lock().unwrap().remove(&digest(&durable(pin)));
    let store=crate::media_fullframes::store().unwrap();
    let slot=closures().lock().unwrap().remove(&closure_key(pin,store.root()));
    if let Some(slot)=slot {
        let entry=slot.entry.lock().unwrap();
        if let Some(verified)=entry.as_ref() {verified.valid.store(false,std::sync::atomic::Ordering::Release);}
    }
    crate::media_fullframes::invalidate_proof_epoch();
}
#[cfg(test)]
fn test_counts(pin:&Value)->(usize,usize) {
    let store=crate::media_fullframes::store().unwrap();
    let slot=closures().lock().unwrap().get(&closure_key(pin,store.root())).cloned();
    let Some(slot)=slot else {return (0,0)};
    let heavy=slot.heavy.load(std::sync::atomic::Ordering::Relaxed);
    let ready=slot.entry.lock().unwrap().as_ref().map(|v|v.ready_transitions.load(std::sync::atomic::Ordering::Relaxed)).unwrap_or(0);
    (heavy,ready)
}
/// Native CAS-backed cold fixture for scoped database/dispatch acceptance.
/// Only this fixture's exact proof/result keys are evicted, never global state.
#[cfg(test)]
pub(crate) fn test_cold_reused_workspace()->Value {
    let (mut workspace,progress,receipt,ledger,payload)=tests::fixture_for(false);
    let (_,proof)=select_prepared(&workspace,&progress,&ledger,&receipt,&payload,&"d".repeat(64)).unwrap().unwrap();
    warm_pin(&workspace,&proof.value).unwrap();
    admit(&mut workspace,&progress,&ledger,&receipt,&proof.value,"2026-10-04T08:00:00Z").unwrap();
    test_evict_pin(&proof.value);workspace
}
#[cfg(test)]
#[path="media_analysis_reuse_tests.rs"]
pub(crate) mod tests;
