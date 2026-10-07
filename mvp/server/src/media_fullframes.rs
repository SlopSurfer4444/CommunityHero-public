//! Full decoded-frame evidence, durable chunk receipts and fenced progress.
//! Frame observations live in immutable private artifacts, not workspace JSON.
use serde_json::{json,Value};
use sha2::{Digest,Sha256};
use std::{collections::{BTreeMap,BTreeSet},path::PathBuf,io::{BufRead,Seek,SeekFrom},sync::{Mutex,OnceLock},time::{Instant,Duration,SystemTime}};
use crate::media_artifacts::{ArtifactRef,ArtifactStore};

pub(crate) const VERSION:u64=2;
pub(crate) const CHUNK_FRAMES:u64=4;
const JSON_LIMIT:u64=16*1024*1024;
const AGGREGATE_LIMIT:usize=256_000;
pub(crate) fn hash(v:&Value)->String{format!("{:x}",Sha256::digest(v.to_string().as_bytes()))}
fn err(_:impl std::fmt::Display)->String{"media_evidence_artifact_unavailable".into()}
fn txt<'a>(v:&'a Value,k:&str)->&'a str{v[k].as_str().unwrap_or("")}
fn rows<'a>(v:&'a Value,k:&str)->&'a [Value]{v[k].as_array().map(Vec::as_slice).unwrap_or(&[])}
pub(crate) fn reference(v:&Value)->Result<ArtifactRef,String>{ArtifactRef::from_json(v).map_err(err)}
pub(crate) fn store()->Result<ArtifactStore,String>{
    let path=std::env::var_os("COMMUNITYHERO_MEDIA_EVIDENCE_DIR").map(PathBuf::from);
    #[cfg(test)]
    let path=path.or_else(||{static ROOT:std::sync::OnceLock<tempfile::TempDir>=std::sync::OnceLock::new();Some(ROOT.get_or_init(||tempfile::tempdir().unwrap()).path().to_owned())});
    ArtifactStore::open(&path.ok_or("media_evidence_directory_unconfigured")?).map_err(err)
}
pub(crate) fn put(store:&ArtifactStore,v:&Value)->Result<Value,String>{store.put_bytes(v.to_string().as_bytes()).map(|r|r.to_json()).map_err(err)}
#[derive(Clone,PartialEq,Eq)]
struct Stamp{bytes:u64,modified:Option<SystemTime>,created:Option<SystemTime>}
fn stamp(path:&std::path::Path)->Result<Stamp,String>{let m=std::fs::symlink_metadata(path).map_err(err)?;if !m.is_file()||m.file_type().is_symlink(){return Err("media_artifact_path_changed".into());}#[cfg(windows)]{use std::os::windows::fs::MetadataExt;if m.file_attributes()&0x400!=0{return Err("media_artifact_path_changed".into());}}Ok(Stamp{bytes:m.len(),modified:m.modified().ok(),created:m.created().ok()})}
static OBJECTS:OnceLock<Mutex<BTreeMap<PathBuf,Stamp>>>=OnceLock::new();
fn verified_path(store:&ArtifactStore,value:&Value)->Result<PathBuf,String>{
    let r=reference(value)?;let path=store.root().join("objects").join(&r.sha256[..2]).join(&r.sha256);
    let now=stamp(&path)?;if now.bytes!=r.bytes{return Err("media_artifact_size_changed".into());}
    let cache=OBJECTS.get_or_init(||Mutex::new(BTreeMap::new()));
    if cache.lock().map_err(err)?.get(&path)==Some(&now){return Ok(path);}
    let checked=store.path(&r).map_err(err)?;let after=stamp(&checked)?;
    if now!=after{return Err("media_artifact_changed_during_verification".into());}
    cache.lock().map_err(err)?.insert(checked.clone(),after);Ok(checked)
}
pub(crate) fn verify_reference(store:&ArtifactStore,value:&Value)->Result<PathBuf,String>{verified_path(store,value)}
pub(crate) fn read(store:&ArtifactStore,v:&Value)->Result<Value,String>{
    let r=reference(v)?;if r.bytes>JSON_LIMIT{return Err("media_artifact_json_too_large".into());}
    let path=verified_path(store,v)?;serde_json::from_slice(&std::fs::read(path).map_err(err)?).map_err(err)
}
#[derive(Clone)]struct Proof{objects:Vec<(PathBuf,Stamp)>,valid_until:Instant}
static PROOF_EPOCH:std::sync::atomic::AtomicU64=std::sync::atomic::AtomicU64::new(1);
pub(crate) fn proof_epoch()->u64{PROOF_EPOCH.load(std::sync::atomic::Ordering::SeqCst)}
pub(crate) fn invalidate_proof_epoch(){PROOF_EPOCH.fetch_add(1,std::sync::atomic::Ordering::SeqCst);}
static PROOFS:OnceLock<Mutex<BTreeMap<String,Proof>>>=OnceLock::new();
#[cfg(test)]
pub(crate) fn forget_test_proof(evidence:&Value){
    if PROOFS.get_or_init(||Mutex::new(BTreeMap::new())).lock().unwrap().remove(&hash(evidence)).is_some(){
        PROOF_EPOCH.fetch_add(1,std::sync::atomic::Ordering::SeqCst);
    }
}
#[cfg(test)]
pub(crate) fn expire_test_proof(evidence:&Value){
    PROOFS.get().unwrap().lock().unwrap().get_mut(&hash(evidence)).unwrap().valid_until=Instant::now()-Duration::from_secs(1);
}
/// This gate intentionally has NO filesystem or database calls. Only an exact
/// immutable proof warmed outside the writer transaction may pass.
pub(crate) fn validate_evidence(evidence:&Value)->Result<(),String>{
    let key=hash(evidence);let mut cache=PROOFS.get_or_init(||Mutex::new(BTreeMap::new())).lock().map_err(err)?;
    if cache.get(&key).is_some_and(|p|p.valid_until>Instant::now()){return Ok(());}
    if cache.remove(&key).is_some(){PROOF_EPOCH.fetch_add(1,std::sync::atomic::Ordering::SeqCst);}
    Err("media_full_proof_not_current".into())
}
pub(crate) fn verify_and_cache(evidence:&Value)->Result<(),String>{
    let key=hash(evidence);let cache=PROOFS.get_or_init(||Mutex::new(BTreeMap::new()));
    let existing={cache.lock().map_err(err)?.get(&key).cloned()};
    if let Some(mut proof)=existing{
        if proof.objects.iter().all(|(path,expected)|stamp(path).is_ok_and(|s|s==*expected)){
            let mut locked=cache.lock().map_err(err)?;
            let was_ready=locked.get(&key).is_some_and(|p|p.valid_until>Instant::now());
            proof.valid_until=Instant::now()+Duration::from_secs(120);locked.insert(key,proof);
            if !was_ready{PROOF_EPOCH.fetch_add(1,std::sync::atomic::Ordering::SeqCst);}return Ok(());
        }
        if cache.lock().map_err(err)?.remove(&key).is_some(){PROOF_EPOCH.fetch_add(1,std::sync::atomic::Ordering::SeqCst);}
    }
    verify_evidence_files(evidence)?;
    let store=store()?;let mut refs=Vec::new();let mut seen=BTreeSet::new();collect_refs(&store,&evidence["finalEvidence"],&mut refs,&mut seen,true)?;
    let mut objects=Vec::new();for reference in refs{let path=verified_path(&store,&reference)?;objects.push((path.clone(),stamp(&path)?));}
    let mut locked=cache.lock().map_err(err)?;let was_ready=locked.get(&key).is_some_and(|p|p.valid_until>Instant::now());
    locked.insert(key,Proof{objects,valid_until:Instant::now()+Duration::from_secs(120)});
    if !was_ready{PROOF_EPOCH.fetch_add(1,std::sync::atomic::Ordering::SeqCst);}Ok(())
}
fn collect_refs(store:&ArtifactStore,r:&Value,refs:&mut Vec<Value>,seen:&mut BTreeSet<String>,structured:bool)->Result<(),String>{
    let reference=reference(r)?;if !seen.insert(reference.sha256){return Ok(());}verified_path(store,r)?;refs.push(r.clone());
    if !structured{return Ok(());}let value=read(store,r)?;
    for key in ["source","inventory","index","inventoryDescriptor","selectionDescriptor","selection","selectionIndex","previousReceipt","latestReceipt","aggregate"]{
        if value[key].is_null(){continue;}if ArtifactRef::from_json(&value[key]).is_ok(){collect_refs(store,&value[key],refs,seen,!matches!(key,"source"|"inventory"|"selection"))?;}
    }Ok(())
}
pub(crate) async fn refresh(app:&crate::App)->crate::ApiResult<()> {
    crate::media_analysis_reuse::refresh(app).await?;
    crate::manual_frame_request::refresh(app).await?;
    let evidence=app.db.read_media_visual_evidence().await?;
    tokio::task::spawn_blocking(move||for e in evidence{let _=verify_and_cache(&e);}).await.map_err(|_|crate::internal("Media proof refresh stopped"))?;Ok(())
}

pub(crate) fn initial(account:&str,binding:&Value,post:&Value,at:&str)->Value{
    json!({"schemaVersion":VERSION,"account":account,"connectorBinding":binding,
        "sourcePostId":post["id"],"sourcePostKey":post["postKey"],"sourceVersion":source_version(post,account),
        "phase":"download","source":null,"inventory":null,"inventoryDescriptor":null,"selectionDescriptor":null,
        "nextSelectionIndex":0,"completedSelectedFrames":0,"latestReceipt":null,"finalEvidence":null,
        "leaseId":null,"leaseEpoch":0,"materialEpoch":null,"createdAt":at})
}
pub(crate) fn source_version(post:&Value,account:&str)->String{
    hash(&json!([account,post["id"],post["postKey"],crate::knowledge::media_source_key(post,account),post["contentSha256"],post["mediaSha256"],post["canonicalMediaId"],post["durationMs"],post["durationSeconds"],post["title"],post["text"],post["attachments"],post["sourceUrl"],post["url"]]))
}
pub(crate) fn claim(progress:&mut Value,lease:&str)->Result<(),String>{
    if progress["schemaVersion"]!=VERSION || matches!(txt(progress,"phase"),"complete"|"held") {return Err("media_progress_not_claimable".into());}
    progress["leaseEpoch"]=json!(progress["leaseEpoch"].as_u64().ok_or("media_progress_invalid")?.checked_add(1).ok_or("media_lease_exhausted")?);
    progress["leaseId"]=json!(lease);Ok(())
}
pub(crate) fn checkpoint(job:&mut Value,lease:&str,expected:&Value,next:Value)->Result<(),String>{
    let current=&job["result"]["visualProgress"];
    if job["status"]!="running" || current!=expected || current["leaseId"]!=lease
        || next["leaseId"]!=current["leaseId"] || next["leaseEpoch"]!=current["leaseEpoch"]
        || next.get("assetPin")!=current.get("assetPin")
        || ["account","connectorBinding","sourcePostId","sourcePostKey","sourceVersion"].iter().any(|k|next[*k]!=current[*k]) {
        return Err("media_progress_lease_changed".into());
    }
    job["result"]["visualProgress"]=next;Ok(())
}
/// Called by generic worker finalization. Local inference failure never erases
/// the committed cursor or turns a yielded chunk into completed video evidence.
pub(crate) fn finish(job:&mut Value,result:&crate::ApiResult<Value>,at:&str)->bool{
    if job["visualContractVersion"]!=VERSION{return false;}
    if job["status"]!="running"{return true;}
    // A failed download deliberately clears its uncommitted cursor so the queue
    // may reserve a different untried source. Do not recreate a partial "held"
    // object: that is neither a durable scan checkpoint nor an owner-resume hold.
    if job["result"]["visualProgress"].is_null(){
        job["status"]=json!("failed");job["finishedAt"]=json!(at);
        job["error"]=json!(result.as_ref().err().map(|e|e.1.as_str()).unwrap_or("media_completion_not_proven"));
        return true;
    }
    let progress=&mut job["result"]["visualProgress"];
    progress["leaseId"]=Value::Null;
    match result {
        Ok(v) if v["resume"]==true=>{job["status"]=json!("queued");job["finishedAt"]=Value::Null;job.as_object_mut().unwrap().remove("error");},
        Ok(v) if v["processed"]==true=>{progress["phase"]=json!("complete");job["status"]=json!("completed");job["finishedAt"]=json!(at);},
        _=>{let phase=progress["phase"].clone();progress["resumePhase"]=phase;progress["phase"]=json!("held");job["status"]=json!("failed");job["finishedAt"]=json!(at);job["error"]=json!(result.as_ref().err().map(|e|e.1.as_str()).unwrap_or("media_completion_not_proven"));},
    }
    true
}
pub(crate) fn recover(job:&mut Value)->Result<(),String>{
    if job["visualContractVersion"]!=VERSION || job["status"]=="cancelled" {return Ok(());}
    if matches!(txt(job,"status"),"running"|"interrupted") {
        let progress=&mut job["result"]["visualProgress"];
        if progress["schemaVersion"]!=VERSION{return Err("media_progress_invalid".into());}
        progress["leaseId"]=Value::Null;
        progress["leaseEpoch"]=json!(progress["leaseEpoch"].as_u64().unwrap_or(0)+1);
        job["status"]=json!("queued");job["finishedAt"]=Value::Null;
    }
    Ok(())
}
pub(crate) fn resume(job:&mut Value,expected_epoch:u64)->Result<(),String>{
    let p=&job["result"]["visualProgress"];
    if job["visualContractVersion"]!=VERSION || !matches!(txt(job,"status"),"failed"|"interrupted")
        || p["leaseEpoch"]!=expected_epoch || p["phase"]!="held" || !p["resumePhase"].is_string(){return Err("media_resume_precondition_changed".into());}
    let p=&mut job["result"]["visualProgress"];
    p["phase"]=p["resumePhase"].take();p["leaseId"]=Value::Null;
    job["status"]=json!("queued");job["finishedAt"]=Value::Null;job.as_object_mut().unwrap().remove("error");Ok(())
}

pub(crate) fn inventory_rows(store:&ArtifactStore,descriptor:&Value,start:u64,count:u64)->Result<Vec<Value>,String>{read_rows(store,descriptor,start,count,"frameIndex")}
fn read_rows(store:&ArtifactStore,descriptor:&Value,start:u64,count:u64,position:&str)->Result<Vec<Value>,String>{
    let total=descriptor["frameCount"].as_u64().ok_or("media_inventory_invalid")?;
    if start>total || count>crate::media_frame_contract::MAX_CHUNK_POSITIONS || start.saturating_add(count)>total{return Err("media_inventory_range_invalid".into());}
    let index=read(store,&descriptor["index"])?;
    let offset=rows(&index,"offsets").iter().rev().find(|v|v["frameIndex"].as_u64().is_some_and(|i|i<=start)).ok_or("media_inventory_index_invalid")?;
    let mut input=std::io::BufReader::new(std::fs::File::open(verified_path(store,&descriptor["inventory"])?).map_err(err)?);
    input.seek(SeekFrom::Start(offset["byteOffset"].as_u64().ok_or("media_inventory_index_invalid")?)).map_err(err)?;
    let first=offset["frameIndex"].as_u64().ok_or("media_inventory_index_invalid")?;
    let mut records=Vec::new();let mut line=String::new();
    for ordinal in first..start+count{line.clear();if input.read_line(&mut line).map_err(err)?==0||line.len()>8192||!line.ends_with('\n'){return Err("media_inventory_truncated".into());}let row:Value=serde_json::from_str(&line).map_err(err)?;if row[position]!=ordinal{return Err("media_inventory_index_changed".into());}if ordinal>=start{records.push(row);}}
    if records.len()!=count as usize{return Err("media_inventory_truncated".into());}
    for (offset,row) in records.iter().enumerate(){
        if row[position]!=start+offset as u64 || row["pts"].as_str().and_then(|v|v.parse::<i64>().ok()).is_none()
            || row["pixelSha256"].as_str().is_none_or(|s|s.len()!=64||!s.bytes().all(|c|c.is_ascii_hexdigit()))
            || row["timestampMs"].as_u64().is_none(){return Err("media_inventory_invalid".into());}
    }
    Ok(records)
}
pub(crate) fn descriptor(store:&ArtifactStore,progress:&Value)->Result<Value,String>{
    let d=read(store,&progress["inventoryDescriptor"])?;
    if d["schemaVersion"]!=VERSION || d["kind"]!="media_frame_inventory"
        || d["source"]!=progress["source"] || d["inventory"]!=progress["inventory"]
        || d["sourceIdentity"]["account"]!=progress["account"] || d["sourceIdentity"]["postKey"]!=progress["sourcePostKey"]
        || d["sourceIdentity"]["mediaSha256"]!=d["source"]["sha256"]
        || d["pixelFormat"]!="rgb24" || d["frameCount"].as_u64().is_none_or(|v|v==0)
        || ["width","height","timeBaseNumerator","timeBaseDenominator"].iter().any(|k|d[*k].as_u64().is_none_or(|v|v==0)) {
        return Err("media_inventory_descriptor_invalid".into());
    }
    verified_path(store,&d["source"])?;
    verified_path(store,&d["inventory"])?;
    Ok(d)
}
/// The descriptor must first pass selection_descriptor. Current policy equality
/// is exact; a matching id alone cannot relabel a historical receipt chain.
pub(crate) fn selection_is_current(selection:&Value)->bool {
 selection["policy"]==crate::media_frame_selection::policy()
}
fn selection_policy_supported(selection:&Value)->bool {
 selection_is_current(selection)||selection["policy"]==crate::media_frame_selection::legacy_policy()
}
pub(crate) fn selection_descriptor(store:&ArtifactStore,progress:&Value)->Result<Value,String>{
 let selection=read(store,&progress["selectionDescriptor"])?;let inventory=read(store,&progress["inventoryDescriptor"])?;
 if selection["frameCount"]!=inventory["frameCount"]{return Err("media_selection_frame_count_changed".into());}
 if selection["kind"]!="media_frame_selection"||selection["schemaVersion"]!=2||selection["inventoryDescriptor"]!=progress["inventoryDescriptor"]||!selection_policy_supported(&selection)||selection["selectedCount"].as_u64().is_none_or(|n|n==0||n>selection["frameCount"].as_u64().unwrap_or(0)){return Err("media_selection_invalid".into());}
 Ok(selection)
}
pub(crate) fn selection_rows(store:&ArtifactStore,selection:&Value,start:u64,count:u64)->Result<Vec<Value>,String>{
 read_rows(store,&json!({"frameCount":selection["selectedCount"],"index":selection["selectionIndex"],"inventory":selection["selection"]}),start,count,"selectionIndex")
}
pub(crate) fn wire_inventory(d:&Value,selection:&Value,selection_ref:&Value)->Value{json!({"sha256":d["inventory"]["sha256"],"frameCount":d["frameCount"],"selectionSha256":selection_ref["sha256"],"selectionPolicySha256":selection["policy"]["sha256"],"selectedFrameCount":selection["selectedCount"],"decoderContractSha256":d["decoderContractSha256"],"timeBaseNumerator":d["timeBaseNumerator"],"timeBaseDenominator":d["timeBaseDenominator"],"width":d["width"],"height":d["height"],"pixelFormat":"rgb24"})}

/// Validate the entire immutable receipt chain against inventory, including
/// aliases to earlier exact pixels. A missing artifact is never a ready token.
pub(crate) fn reviewed(store:&ArtifactStore,descriptor_ref:&Value,selection_ref:&Value,latest:&Value)->Result<(BTreeMap<String,Value>,Vec<Value>,u64),String>{
    let (pixels,observations,covered,_)=reviewed_with_provenance(store,descriptor_ref,selection_ref,latest)?;
    Ok((pixels,observations,covered))
}
fn reviewed_with_provenance(store:&ArtifactStore,descriptor_ref:&Value,selection_ref:&Value,latest:&Value)->Result<(BTreeMap<String,Value>,Vec<Value>,u64,Vec<Value>),String>{
    let d=read(store,descriptor_ref)?;let selection=selection_descriptor(store,&json!({"inventoryDescriptor":descriptor_ref,"selectionDescriptor":selection_ref}))?;
    let mut chain=Vec::new();let mut cursor=latest.clone();let mut seen=BTreeSet::new();
    while !cursor.is_null(){
        let r=reference(&cursor)?;
        if !seen.insert(r.sha256.clone()){return Err("media_receipt_cycle".into());}
        let value=read(store,&cursor)?;
        if value["schemaVersion"]!=VERSION || value["kind"]!="media_visual_chunk" || value["inventoryDescriptor"]!=*descriptor_ref||value["selectionDescriptor"]!=*selection_ref{return Err("media_chunk_binding_invalid".into());}
        chain.push((cursor.clone(),value.clone()));cursor=value["previousReceipt"].clone();
    }
    chain.reverse();
    let mut last_source_index=None;let mut covered=0;let mut pixels=BTreeMap::new();let mut observations=Vec::new();let mut model_provenance=Vec::new();
    for (receipt_ref,receipt) in chain {
        let first=receipt["firstSelectionIndex"].as_u64().ok_or("media_chunk_invalid")?;
        let end=receipt["endSelectionIndexExclusive"].as_u64().ok_or("media_chunk_invalid")?;
        if first!=covered || end<=first || end-first>crate::media_frame_contract::MAX_CHUNK_POSITIONS{return Err("media_chunk_gap_or_overlap".into());}
        let inventory=selection_rows(store,&selection,first,end-first)?;
        for row in &inventory {let source_index=row["frameIndex"].as_u64().ok_or("media_selection_frame_invalid")?;if last_source_index.is_some_and(|n|source_index<=n)||last_source_index.is_none()&&!selection_is_current(&selection)&&source_index!=0{return Err("media_selection_order_invalid".into());}last_source_index=Some(source_index);let actual=inventory_rows(store,&d,row["frameIndex"].as_u64().ok_or("media_selection_frame_invalid")?,1)?;if ["frameIndex","pts","timestampMs","pixelSha256"].iter().any(|k|actual[0][*k]!=row[*k]){return Err("media_selection_inventory_mismatch".into());}}
        if !receipt["response"].is_null(){
            let response=crate::media_frame_contract::validate_response(&receipt["request"],&receipt["response"]).map_err(str::to_owned)?;
            if response["inventory"]!=wire_inventory(&d,&selection,selection_ref)||response["source"]!=d["sourceIdentity"]
                || response["chunk"]["leaseId"]!=receipt["leaseId"]||response["chunk"]["previousReceiptSha256"]!=receipt["previousReceipt"]["sha256"]
                || response["chunk"]["firstSelectionIndex"]!=first||response["chunk"]["endSelectionIndexExclusive"]!=end{return Err("media_chunk_binding_invalid".into());}
            // Attribute the inspected frames to this exact immutable receipt.
            // Alias-only positions retain their original receipt and model.
            model_provenance.push(json!({"receipt":receipt_ref.clone(),"firstSelectionIndex":first,
                "endSelectionIndexExclusive":end,"frameIds":rows(&response,"frames").iter().map(|f|f["id"].clone()).collect::<Vec<_>>(),
                "provenance":response["provenance"]}));
            for frame in rows(&response,"frames"){
                let Some(row)=inventory.iter().find(|r|r["frameIndex"]==frame["frameIndex"]) else{return Err("media_frame_not_in_inventory".into());};
                if ["pts","timestampMs","pixelSha256","selectionIndex"].iter().any(|k|row[*k]!=frame[*k])||row["reasons"]!=frame["selectionReasons"]||pixels.contains_key(txt(frame,"pixelSha256")){return Err("media_frame_identity_changed".into());}
                pixels.insert(txt(frame,"pixelSha256").to_owned(),json!({"receipt":receipt_ref,"frameId":frame["id"],"frameIndex":frame["frameIndex"],"pts":frame["pts"]}));
                observations.push(frame.clone());
            }
        }
        if rows(&receipt,"aliases").len()!=inventory.len(){return Err("media_chunk_coverage_incomplete".into());}
        for (row,alias) in inventory.iter().zip(rows(&receipt,"aliases")){
            let expected=pixels.get(txt(row,"pixelSha256")).ok_or("media_frame_unreviewed")?;
            if alias["selectionIndex"]!=row["selectionIndex"]||alias["frameIndex"]!=row["frameIndex"]||alias["pts"]!=row["pts"]||alias["pixelSha256"]!=row["pixelSha256"]
                || alias["frameId"]!=expected["frameId"]
                || !(alias["receipt"].is_null()&&expected["receipt"]==receipt_ref) && alias["receipt"]!=expected["receipt"] {
                return Err("media_alias_not_exact".into());
            }
        }
        covered=end;
    }
    if covered==selection["selectedCount"]&&last_source_index!=d["frameCount"].as_u64().and_then(|n|n.checked_sub(1)){return Err("media_selection_ending_missing".into());}
    Ok((pixels,observations,covered,model_provenance))
}
pub(crate) fn final_evidence(store:&ArtifactStore,progress:&Value)->Result<Value,String>{
    let d=descriptor(store,progress)?;let selection=selection_descriptor(store,progress)?;
    let (pixels,observations,covered,model_provenance)=reviewed_with_provenance(store,&progress["inventoryDescriptor"],&progress["selectionDescriptor"],&progress["latestReceipt"])?;
    if covered!=selection["selectedCount"] || progress["nextSelectionIndex"]!=covered {return Err("media_full_coverage_incomplete".into());}
    let (notes,overflow)=aggregate_observations(&observations)?;
    let aggregate=json!({"schemaVersion":VERSION,"kind":"media_visual_aggregate","sourceIdentity":d["sourceIdentity"],"inventoryDescriptor":progress["inventoryDescriptor"],"selectionDescriptor":progress["selectionDescriptor"],"aggregateOverflow":overflow,"observations":notes,"modelProvenance":model_provenance});
    let aggregate_ref=put(store,&aggregate)?;
    let final_value=json!({"schemaVersion":VERSION,"kind":"media_visual_complete","sourcePostVersion":progress["sourceVersion"],"inventoryDescriptor":progress["inventoryDescriptor"],"selectionDescriptor":progress["selectionDescriptor"],"latestReceipt":progress["latestReceipt"],"frameCount":d["frameCount"],"selectedFrameCount":covered,"coveredSelectedFrameCount":covered,"uniqueReviewedFrames":pixels.len(),"aggregate":aggregate_ref,"sourceIdentity":d["sourceIdentity"],"aggregateOverflow":overflow});
    let final_ref=put(store,&final_value)?;
    Ok(json!({"schemaVersion":VERSION,"sourcePostVersion":progress["sourceVersion"],"source":d["sourceIdentity"],"finalEvidence":final_ref,"coverage":{"kind":"all_frames_fast_selected_neural","selectionPolicySha256":selection["policy"]["sha256"],"frameCount":d["frameCount"],"selectedFrameCount":covered,"coveredSelectedFrameCount":covered,"uniqueReviewedFrames":pixels.len()},"aggregateOverflow":overflow,"aggregate":if overflow {json!([])} else {aggregate["observations"].clone()}}))
}
/// Exact observation groups retain price/variant/qualifier co-occurrence. The
/// receipt chain retains every pixel/frame alias; context carries a representative
/// frame and all exact observation-group source references until the explicit cap.
fn aggregate_observations(observations:&[Value])->Result<(Value,bool),String>{
    let mut unique:BTreeMap<String,Value>=BTreeMap::new();
    for observation in observations {
        let note=json!({"scene":observation["scene"],"text":observation["text"],"numbers":observation["numbers"],"uncertainties":observation["uncertainties"]});
        let entry=unique.entry(hash(&note)).or_insert_with(||json!({"observation":note,"sources":[]}));
        entry["sources"].as_array_mut().unwrap().push(json!({"frameIndex":observation["frameIndex"],"pts":observation["pts"],"timestampMs":observation["timestampMs"],"pixelSha256":observation["pixelSha256"]}));
    }
    let notes=json!(unique.into_values().collect::<Vec<_>>());
    if serde_json::to_vec(&notes).map_err(err)?.len()>AGGREGATE_LIMIT {Ok((json!([]),true))} else {Ok((notes,false))}
}
fn verify_evidence_files(evidence:&Value)->Result<(),String>{
    if evidence["schemaVersion"]!=VERSION || evidence["sourcePostVersion"].as_str().is_none_or(|h|h.len()!=64||!h.bytes().all(|b|b.is_ascii_digit()||(b'a'..=b'f').contains(&b))) || evidence["aggregateOverflow"]!=false{return Err("media_full_context_not_ready".into());}
    let store=store()?;let final_value=read(&store,&evidence["finalEvidence"])?;
    if final_value["schemaVersion"]!=VERSION||final_value["kind"]!="media_visual_complete"||final_value["sourcePostVersion"]!=evidence["sourcePostVersion"]||final_value["sourceIdentity"]!=evidence["source"]||final_value["aggregateOverflow"]!=false{return Err("media_final_binding_invalid".into());}
    let d=read(&store,&final_value["inventoryDescriptor"])?;let selection=selection_descriptor(&store,&final_value)?;
    verified_path(&store,&d["source"])?;
    let (pixels,observations,covered,model_provenance)=reviewed_with_provenance(&store,&final_value["inventoryDescriptor"],&final_value["selectionDescriptor"],&final_value["latestReceipt"])?;
    let aggregate=read(&store,&final_value["aggregate"])?;
    let (expected_notes,overflow)=aggregate_observations(&observations)?;
    // Historical aggregates predate the index and are admitted only when every
    // response in their receipt chain has the same model provenance.
    let provenance_valid=match aggregate.get("modelProvenance") {
        Some(recorded)=>*recorded==json!(model_provenance),
        None=>model_provenance.first().is_some_and(|first|first["provenance"].as_object().is_some_and(|p|
            p.len()==3&&["backend","model","instructionSha256"].iter().all(|key|p.contains_key(*key)))
            &&model_provenance.iter().all(|entry|entry["provenance"]==first["provenance"])),
    };
    if overflow||aggregate["observations"]!=expected_notes||d["sourceIdentity"]!=evidence["source"]||d["source"]["sha256"]!=evidence["source"]["mediaSha256"]
        ||!provenance_valid
        ||aggregate["schemaVersion"]!=VERSION||aggregate["kind"]!="media_visual_aggregate"||aggregate["inventoryDescriptor"]!=final_value["inventoryDescriptor"]||aggregate["selectionDescriptor"]!=final_value["selectionDescriptor"]
        ||covered!=selection["selectedCount"]||d["frameCount"]!=final_value["frameCount"]||covered!=final_value["selectedFrameCount"]||covered!=final_value["coveredSelectedFrameCount"]
        || pixels.len() as u64!=final_value["uniqueReviewedFrames"]||evidence["coverage"]!=json!({"kind":"all_frames_fast_selected_neural","selectionPolicySha256":selection["policy"]["sha256"],"frameCount":d["frameCount"],"selectedFrameCount":covered,"coveredSelectedFrameCount":covered,"uniqueReviewedFrames":pixels.len()})
        || aggregate["sourceIdentity"]!=evidence["source"]||aggregate["observations"]!=evidence["aggregate"]||aggregate["aggregateOverflow"]!=false{return Err("media_final_coverage_invalid".into());}
    Ok(())
}

/// Pure bootstrap/delta projection. The SQL read must supply exact current media
/// heads in this private field from the SAME snapshot, before payload pruning.
pub(crate) fn project_media_readiness(value:&mut Value)->crate::ApiResult<()> {
    let mut d=json!({"account":value["account"],"posts":value["posts"],"items":value["items"],"jobs":value["jobs"],
        "knowledge_entries":value["mediaReadinessCatalog"]["knowledge_entries"],"knowledge_versions":value["mediaReadinessCatalog"]["knowledge_versions"]});
    if let Some(binding)=value.get("connectorBinding"){d["connectorBinding"]=binding.clone();}
    // These source-bound policies are part of the preparation gate. Retain only
    // its scoped settings, so UI readiness evaluates the same evidence as prepare.
    d["settings"]=json!({});
    for key in ["mediaPolicyDefaults","postMediaPolicies","mediaAudioEquivalences"] {
        if let Some(setting)=value["settings"].get(key){d["settings"][key]=setting.clone();}
    }
    if !d["knowledge_entries"].is_array(){d["knowledge_entries"]=json!([]);d["knowledge_versions"]=json!([]);}
    let states=crate::media_queue::preparation_states(&d,rows(&d,"items"),&crate::now())?;
    if let Some(items)=value["items"].as_array_mut(){for item in items {
        item.as_object_mut().ok_or_else(||crate::bad("Invalid item"))?.remove("mediaReadiness");
        if !matches!(txt(item,"providerStatus"),"new"|"inprogress"|"in_progress")||!matches!(txt(item,"workflow"),"attention"|"prepared"|"waiting"|"wait"|"active")||!crate::media_queue::requires_video(&d,item){continue;}
        let bound=crate::active_binding(&d).and_then(|b|crate::bound_item(&b,item)).is_ok();
        let status=if !bound {"media_unavailable"}else{states.get(txt(item,"id")).and_then(|s|*s).unwrap_or("ready")};
        item["mediaReadiness"]=json!({"schemaVersion":2,"required":true,"status":status});
    }}
    value.as_object_mut().ok_or_else(||crate::bad("Invalid bootstrap"))?.remove("mediaReadinessCatalog");Ok(())
}

#[cfg(test)]
pub(crate) fn fixture(account:&str,post:&str)->Value{fixture_frames(account,post,&["b".repeat(64)])}
#[cfg(test)]
fn fixture_frames(account:&str,post:&str,pixels:&[String])->Value{fixture_bound(account,post,pixels,&source_version(&json!({"postKey":post}),account))}
#[cfg(test)]
pub(crate) fn fixture_for_post(account:&str,post:&Value)->Value{fixture_bound(account,txt(post,"postKey"),&["b".repeat(64)],&source_version(post,account))}
#[cfg(test)]
fn fixture_bound(account:&str,post:&str,pixels:&[String],post_version:&str)->Value{
 let progress=fixture_progress(account,post,pixels,post_version,crate::media_frame_selection::legacy_policy(),&(0..pixels.len()).collect::<Vec<_>>());
 let evidence=final_evidence(&store().unwrap(),&progress).unwrap();verify_and_cache(&evidence).unwrap();evidence
}
#[cfg(test)]
fn fixture_progress(account:&str,post:&str,pixels:&[String],post_version:&str,policy:Value,selected_indices:&[usize])->Value {
 fixture_progress_with_models(account,post,pixels,post_version,policy,selected_indices,&[])
}
#[cfg(test)]
fn fixture_progress_with_models(account:&str,post:&str,pixels:&[String],post_version:&str,policy:Value,selected_indices:&[usize],models:&[Value])->Value {
 let store=store().unwrap();let source=store.put_bytes(format!("synthetic source {account} {post}").as_bytes()).unwrap().to_json();
 let inventory_rows:Vec<_>=pixels.iter().enumerate().map(|(i,p)|json!({"frameIndex":i,"pts":(i*33).to_string(),"timestampMs":i*33,"pixelSha256":p})).collect();
 let inventory=store.write_jsonl(inventory_rows.clone()).unwrap().to_json();
 let index=put(&store,&json!({"schemaVersion":2,"kind":"media_frame_index","offsets":[{"frameIndex":0,"byteOffset":0}]})).unwrap();
 let identity=json!({"account":account,"postKey":post,"mediaSha256":source["sha256"],"durationMs":1000});
 let d=json!({"schemaVersion":2,"kind":"media_frame_inventory","source":source,"inventory":inventory,"index":index,"sourceIdentity":identity,"frameCount":pixels.len(),"timeBaseNumerator":1,"timeBaseDenominator":1000,"width":2,"height":2,"pixelFormat":"rgb24","decoderContractSha256":"c".repeat(64)});
 let dr=put(&store,&d).unwrap();let reason=if policy==crate::media_frame_selection::legacy_policy(){"baseline"}else{"second_end"};let selected:Vec<_>=selected_indices.iter().enumerate().map(|(i,source_index)|{let mut r=inventory_rows[*source_index].clone();r["selectionIndex"]=json!(i);r["reasons"]=json!([reason]);r}).collect();
 let selection_data=store.write_jsonl(selected.clone()).unwrap().to_json();
 let selection=json!({"schemaVersion":2,"kind":"media_frame_selection","inventoryDescriptor":dr,"selection":selection_data,"selectionIndex":index,"selectedCount":selected.len(),"frameCount":pixels.len(),"policy":policy,"reasonCounts":{reason:selected.len()}});
 let sr=put(&store,&selection).unwrap();let lease="fixture-lease";let mut latest=Value::Null;let mut seen:BTreeMap<String,Value>=BTreeMap::new();
 for (i,row) in selected.iter().enumerate(){
  let pixel=txt(row,"pixelSha256");let mut req=Value::Null;let mut response=Value::Null;
  let prior=seen.get(pixel).cloned();let frame_id=prior.as_ref().map(|p|p["frameId"].clone()).unwrap_or_else(||json!(format!("f{i}")));
  if prior.is_none(){
   let frame=json!({"id":frame_id,"frameIndex":row["frameIndex"],"selectionIndex":i,"selectionReasons":row["reasons"],"pts":row["pts"],"timestampMs":row["timestampMs"],"pixelSha256":pixel,"sha256":"d".repeat(64),"path":"private/vision-frames/f.png","mimeType":"image/png"});
   req=crate::media_frame_contract::seal_request(json!({"schemaVersion":2,"workId":"media-00000000-0000-0000-0000-000000000000","createdAtUtc":"2026-09-23T00:00:00Z","source":identity,"inventory":wire_inventory(&d,&selection,&sr),"chunk":{"firstSelectionIndex":i,"endSelectionIndexExclusive":i+1,"previousReceiptSha256":latest["sha256"],"leaseId":lease},"frames":[frame]}));
   let mut observation=frame;let object=observation.as_object_mut().unwrap();object.remove("path");object.remove("mimeType");
   observation["status"]=json!("readable");observation["scene"]=json!(format!("Frame group {pixel}"));observation["text"]=if i==1{json!(["2490000 RUB","до Владивостока"])}else{json!([])};observation["numbers"]=json!([]);observation["uncertainties"]=json!([]);
   let provenance=models.get(i).cloned().unwrap_or_else(||json!({"backend":"offline-test","model":"test","instructionSha256":"e".repeat(64)}));
   response=json!({"schemaVersion":2,"status":"complete","source":identity,"inventory":req["inventory"],"chunk":req["chunk"],"manifestSha256":req["manifestSha256"],"frames":[observation],"summary":"Inspected frame","provenance":provenance});
  }
  let receipt=put(&store,&json!({"schemaVersion":2,"kind":"media_visual_chunk","inventoryDescriptor":dr,"selectionDescriptor":sr,"previousReceipt":latest,"firstSelectionIndex":i,"endSelectionIndexExclusive":i+1,"request":req,"response":response,"aliases":[{"selectionIndex":i,"frameIndex":row["frameIndex"],"pts":row["pts"],"pixelSha256":pixel,"frameId":frame_id,"receipt":prior.as_ref().map(|p|p["receipt"].clone())}],"leaseId":lease})).unwrap();
  if prior.is_none(){seen.insert(pixel.to_owned(),json!({"frameId":frame_id,"receipt":receipt}));}latest=receipt;
 }
 let progress=json!({"sourceVersion":post_version,"account":account,"sourcePostKey":post,"source":source,"inventory":inventory,"inventoryDescriptor":dr,"selectionDescriptor":sr,"latestReceipt":latest,"nextSelectionIndex":selected.len()});
 progress
}

#[cfg(test)]
mod tests{
 use super::*;
 fn rebind_aggregate(store:&ArtifactStore,evidence:&Value,change:impl FnOnce(&mut Value))->Value{
  let mut final_value=read(store,&evidence["finalEvidence"]).unwrap();
  let mut aggregate=read(store,&final_value["aggregate"]).unwrap();change(&mut aggregate);
  final_value["aggregate"]=put(store,&aggregate).unwrap();
  let mut revised=evidence.clone();revised["finalEvidence"]=put(store,&final_value).unwrap();revised
 }
 #[test] fn mixed_models_preserve_exact_receipt_provenance_and_final_coverage(){
  let pixels=["b".repeat(64),"c".repeat(64),"d".repeat(64)];let s=store().unwrap();
  let local=json!({"backend":"local_ollama","model":"qwen@sha256:local","instructionSha256":"e".repeat(64)});
  let luna=json!({"backend":"codex_isolated","model":"gpt-6-luna","instructionSha256":"f".repeat(64)});
  let progress=fixture_progress_with_models("LikeAvto","mixed-models",&pixels,&"a".repeat(64),
   crate::media_frame_selection::policy(),&[0,1,2],&[local.clone(),luna.clone(),local.clone()]);
  let evidence=final_evidence(&s,&progress).unwrap();assert_eq!(evidence["coverage"]["coveredSelectedFrameCount"],3);
  assert_eq!(evidence["coverage"]["uniqueReviewedFrames"],3);assert!(verify_evidence_files(&evidence).is_ok());
  let final_value=read(&s,&evidence["finalEvidence"]).unwrap();let aggregate=read(&s,&final_value["aggregate"]).unwrap();
  let index=aggregate["modelProvenance"].as_array().unwrap();assert_eq!(index.len(),3);
  for (i,expected) in [local.clone(),luna,local].iter().enumerate(){
   assert_eq!(index[i]["firstSelectionIndex"],i);assert_eq!(index[i]["endSelectionIndexExclusive"],i+1);
   assert_eq!(index[i]["provenance"],*expected);assert!(reference(&index[i]["receipt"]).is_ok());
   let receipt=read(&s,&index[i]["receipt"]).unwrap();assert_eq!(receipt["response"]["provenance"],*expected);
   assert_eq!(index[i]["frameIds"],json!([receipt["response"]["frames"][0]["id"]]));
  }
  let tampered=rebind_aggregate(&s,&evidence,|aggregate|aggregate["modelProvenance"][1]["provenance"]["model"]=json!("wrong"));
  assert_eq!(verify_evidence_files(&tampered).unwrap_err(),"media_final_coverage_invalid");
  let unindexed=rebind_aggregate(&s,&evidence,|aggregate|{aggregate.as_object_mut().unwrap().remove("modelProvenance");});
  assert_eq!(verify_evidence_files(&unindexed).unwrap_err(),"media_final_coverage_invalid");
 }
 #[test] fn historical_unindexed_homogeneous_aggregate_remains_admissible(){
  let pixels=["b".repeat(64),"c".repeat(64)];let s=store().unwrap();
  let progress=fixture_progress("LikeAvto","legacy-model-index",&pixels,&"a".repeat(64),crate::media_frame_selection::legacy_policy(),&[0,1]);
  let evidence=final_evidence(&s,&progress).unwrap();
  let legacy=rebind_aggregate(&s,&evidence,|aggregate|{aggregate.as_object_mut().unwrap().remove("modelProvenance");});
  assert!(verify_evidence_files(&legacy).is_ok());
 }
 #[test] fn mixed_frame_rescue_provenance_survives_receipt_chain_and_rejects_aggregate_relabel(){
  let s=store().unwrap();let pixels=["b".repeat(64),"c".repeat(64)];
  let local=json!({"backend":"local_ollama","model":"historical-local","instructionSha256":"e".repeat(64)});
  let mixed=json!({"schemaVersion":2,"kind":"mixed_frames","frames":[{"id":"f1","backend":"codex_isolated","model":"gpt-6-luna","instructionSha256":"e".repeat(64)}],
   "rescue":{"permitSha256":"f".repeat(64),"cloudFrameCount":1,"cloudInvocationCount":1}});
  let progress=fixture_progress_with_models("LikeAvto","mixed-frame-rescue",&pixels,&"a".repeat(64),
   crate::media_frame_selection::policy(),&[0,1],&[local.clone(),mixed.clone()]);
  let evidence=final_evidence(&s,&progress).unwrap();assert!(verify_evidence_files(&evidence).is_ok());
  let final_value=read(&s,&evidence["finalEvidence"]).unwrap();let aggregate=read(&s,&final_value["aggregate"]).unwrap();
  assert_eq!(aggregate["modelProvenance"][0]["provenance"],local);
  assert_eq!(aggregate["modelProvenance"][1]["provenance"],mixed);
  let relabel=rebind_aggregate(&s,&evidence,|a|a["modelProvenance"][1]["provenance"]["frames"][0]["model"]=json!("other"));
  assert_eq!(verify_evidence_files(&relabel).unwrap_err(),"media_final_coverage_invalid");
 }
 #[test] fn single_mixed_receipt_requires_aggregate_model_index(){
  let s=store().unwrap();let pixels=["b".repeat(64)];
  let mixed=json!({"schemaVersion":2,"kind":"mixed_frames","frames":[{"id":"f0","backend":"codex_isolated","model":"gpt-6-luna","instructionSha256":"e".repeat(64)}],
   "rescue":{"permitSha256":"f".repeat(64),"cloudFrameCount":1,"cloudInvocationCount":1}});
  let progress=fixture_progress_with_models("LikeAvto","single-mixed-index",&pixels,&"a".repeat(64),
   crate::media_frame_selection::policy(),&[0],&[mixed]);
  let evidence=final_evidence(&s,&progress).unwrap();assert!(verify_evidence_files(&evidence).is_ok());
  let unindexed=rebind_aggregate(&s,&evidence,|a|{a.as_object_mut().unwrap().remove("modelProvenance");});
  assert_eq!(verify_evidence_files(&unindexed).unwrap_err(),"media_final_coverage_invalid");
 }
 #[test] fn selection_policy_compatibility_is_exact_and_old_completed_evidence_stays_valid(){
  let e=fixture_frames("LikeAvto","legacy-policy",&["b".repeat(64),"c".repeat(64)]);
  assert!(verify_evidence_files(&e).is_ok());let s=store().unwrap();let final_value=read(&s,&e["finalEvidence"]).unwrap();
  let selection=selection_descriptor(&s,&final_value).unwrap();assert!(!selection_is_current(&selection));
  for policy in [crate::media_frame_selection::legacy_policy(),crate::media_frame_selection::policy()] {
   let mut changed=selection.clone();changed["policy"]=policy;changed["policy"]["unreviewedChange"]=json!(true);
   let mut progress=final_value.clone();progress["selectionDescriptor"]=put(&s,&changed).unwrap();
   assert_eq!(selection_descriptor(&s,&progress).unwrap_err(),"media_selection_invalid");
  }
 }
 #[test] fn fixed_selection_may_start_after_zero_but_legacy_must_start_at_zero(){
  let pixels=["b".repeat(64),"c".repeat(64),"d".repeat(64)];let version="a".repeat(64);let s=store().unwrap();
  let fixed=fixture_progress("LikeAvto","fixed-start",&pixels,&version,crate::media_frame_selection::policy(),&[1,2]);
  assert!(selection_is_current(&selection_descriptor(&s,&fixed).unwrap()));
  let evidence=final_evidence(&s,&fixed).unwrap();assert_eq!(evidence["coverage"]["selectedFrameCount"],2);assert!(verify_evidence_files(&evidence).is_ok());
  let legacy=fixture_progress("LikeAvto","legacy-start",&pixels,&version,crate::media_frame_selection::legacy_policy(),&[1,2]);
  assert_eq!(final_evidence(&s,&legacy).unwrap_err(),"media_selection_order_invalid");
 }
 #[test] fn both_selection_policies_require_final_frame_and_monotonic_source_indices(){
  let pixels=["b".repeat(64),"c".repeat(64),"d".repeat(64)];let s=store().unwrap();
  for policy in [crate::media_frame_selection::legacy_policy(),crate::media_frame_selection::policy()] {
   let missing=fixture_progress("LikeAvto","missing-end",&pixels,&"a".repeat(64),policy.clone(),&[0,1]);
   assert_eq!(final_evidence(&s,&missing).unwrap_err(),"media_selection_ending_missing");
   let reversed=fixture_progress("LikeAvto","reversed",&pixels,&"a".repeat(64),policy,&[0,2,1]);
   assert_eq!(final_evidence(&s,&reversed).unwrap_err(),"media_selection_order_invalid");
  }
 }
 #[test] fn historical_receipts_cannot_be_relabelled_as_fixed_selection(){
  let pixels=["b".repeat(64),"c".repeat(64)];let s=store().unwrap();
  let mut progress=fixture_progress("LikeAvto","mixed-policy",&pixels,&"a".repeat(64),crate::media_frame_selection::legacy_policy(),&[0,1]);
  let mut selection=selection_descriptor(&s,&progress).unwrap();selection["policy"]=crate::media_frame_selection::policy();progress["selectionDescriptor"]=put(&s,&selection).unwrap();
  assert_eq!(final_evidence(&s,&progress).unwrap_err(),"media_chunk_binding_invalid");
 }
 #[test] fn cleared_download_cursor_stays_absent_when_worker_finishes(){
  for result in [Err(crate::bad("source_download_failed")),Ok(json!({"resume":true})),Ok(json!({"processed":true}))] {
   let mut job=json!({"visualContractVersion":2,"status":"running","fallbackAllowed":true,"result":{"visualProgress":null},"sourceAttempts":[{"status":"failed"}]});
   let attempts=job["sourceAttempts"].clone();assert!(finish(&mut job,&result,"done"));
   assert_eq!(job["status"],"failed");assert_eq!(job["finishedAt"],"done");assert!(job["result"]["visualProgress"].is_null());assert_eq!(job["sourceAttempts"],attempts);
   assert_eq!(job["error"],if result.is_err(){"source_download_failed"}else{"media_completion_not_proven"});
  }
 }
 #[test] fn transient_price_and_exact_alias_survive_chunked_receipts(){let e=fixture_frames("LikeAvto","transient",&["b".repeat(64),"c".repeat(64),"b".repeat(64)]);assert_eq!(e["coverage"]["frameCount"],3);assert_eq!(e["coverage"]["coveredSelectedFrameCount"],3);assert_eq!(e["coverage"]["uniqueReviewedFrames"],2);assert!(e["aggregate"].to_string().contains("2490000"));}
 #[test] fn complete_fixture_has_pure_exact_proof_and_mutation_is_held(){let e=fixture("LikeAvto","pure-gate");assert!(validate_evidence(&e).is_ok());let mut bad=e.clone();bad["source"]["postKey"]=json!("other");assert!(validate_evidence(&bad).is_err());}
 #[test] fn leases_fence_abandoned_worker_and_failed_chunk_preserves_cursor(){
  let post=json!({"id":"p","postKey":"p"});let mut p=initial("LikeAvto",&json!({}),&post,"now");claim(&mut p,"old").unwrap();p["nextSelectionIndex"]=json!(32);p["latestReceipt"]=json!({"sha256":"a".repeat(64),"bytes":1});
  let mut job=json!({"visualContractVersion":2,"status":"running","result":{"visualProgress":p}});let old=job["result"]["visualProgress"].clone();
  recover(&mut job).unwrap();job["status"]=json!("running");claim(&mut job["result"]["visualProgress"],"new").unwrap();assert!(checkpoint(&mut job,"old",&old,old.clone()).is_err());
  let cursor=job["result"]["visualProgress"]["latestReceipt"].clone();assert!(finish(&mut job,&Err(crate::bad("model unavailable")),"later"));assert_eq!(job["result"]["visualProgress"]["latestReceipt"],cursor);assert_eq!(job["result"]["visualProgress"]["nextSelectionIndex"],32);
  let epoch=job["result"]["visualProgress"]["leaseEpoch"].as_u64().unwrap();assert!(resume(&mut job,epoch-1).is_err());resume(&mut job,epoch).unwrap();assert_eq!(job["status"],"queued");
 }
 #[test] fn aggregation_preserves_two_prices_variants_conditions_as_one_group(){
  let obs=json!({"frameIndex":7,"pts":"33","timestampMs":33,"pixelSha256":"a".repeat(64),"scene":"Two variants; association uncertain","text":["1.5 Turbo","2.0 Hybrid","от 2490000 RUB","от 3100000 RUB","до Владивостока"],"numbers":[{"raw":"от 2490000 RUB","value":"2490000","currency":"RUB","unit":null,"uncertain":false},{"raw":"от 3100000 RUB","value":"3100000","currency":"RUB","unit":null,"uncertain":false}],"uncertainties":["Variant to price association is not visually explicit"]});
  let (aggregate,overflow)=aggregate_observations(&[obs.clone(),obs.clone()]).unwrap();assert!(!overflow);assert_eq!(aggregate.as_array().unwrap().len(),1);assert_eq!(aggregate[0]["observation"]["numbers"],obs["numbers"]);assert_eq!(aggregate[0]["observation"]["text"],obs["text"]);assert_eq!(aggregate[0]["sources"].as_array().unwrap().len(),2);
 }
 #[test] fn overflow_never_truncates_facts_into_ready_context(){let obs=json!({"scene":"x".repeat(AGGREGATE_LIMIT+1),"text":[],"numbers":[],"uncertainties":[]});let (notes,overflow)=aggregate_observations(&[obs]).unwrap();assert!(overflow);assert_eq!(notes,json!([]));}
 #[test] fn proof_expiry_and_rewarming_advance_external_epoch(){let e=fixture("LikeAvto","expiry");let first=proof_epoch();PROOFS.get().unwrap().lock().unwrap().get_mut(&hash(&e)).unwrap().valid_until=Instant::now()-Duration::from_secs(1);assert!(validate_evidence(&e).is_err());assert!(proof_epoch()>first);let held=proof_epoch();verify_and_cache(&e).unwrap();assert!(proof_epoch()>held);assert!(validate_evidence(&e).is_ok());}
 #[test] fn pure_projection_holds_manual_draft_then_restores_ready_without_rewriting_it(){
  let mut d=crate::empty();crate::accounts::initialize(&mut d,crate::accounts::Profile::LikeAvto).unwrap();
  d["posts"]=json!([{"id":"p","postKey":"11391:p","title":"Video","attachments":[{"type":"video"}]}]);d["items"]=json!([{"id":"i","postId":"p","postKey":"11391:p","objectId":"11391","itemId":"i","conversationKey":"11391:thread","providerStatus":"new","workflow":"prepared","draft":{"text":"Owner manual text","editedByHuman":true}}]);
  let mut view=d.clone();project_media_readiness(&mut view).unwrap();assert_eq!(view["items"][0]["mediaReadiness"]["status"],"media_wait");assert_eq!(view["items"][0]["draft"],d["items"][0]["draft"]);assert_eq!(view["items"][0]["workflow"],"prepared");
  let e=fixture_for_post("LikeAvto",&d["posts"][0]);let source=source_version(&d["posts"][0],"LikeAvto");let seconds=e["source"]["durationMs"].as_f64().unwrap()/1000.0;d["materials"]=json!([{"id":"audio","kind":"transcript","postKey":"11391:p","text":"Audio evidence","transcription":{"partial":false,"coverage":"full_audio","sourceVersion":source,"mediaDurationSeconds":seconds,"audioDurationSeconds":seconds}},{"id":"visual","account":"LikeAvto","kind":"visual_context","postKey":"11391:p","mediaSha256":e["source"]["mediaSha256"],"text":"Selected visual observations","visualEvidence":e}]);crate::knowledge::sync_catalog(&mut d,"2026-09-23T00:00:00Z").unwrap();
  d["mediaReadinessCatalog"]=json!({"knowledge_entries":d["knowledge_entries"],"knowledge_versions":d["knowledge_versions"]});project_media_readiness(&mut d).unwrap();assert_eq!(d["items"][0]["mediaReadiness"]["status"],"ready");assert!(d.get("mediaReadinessCatalog").is_none());
  d["items"][0]["workflow"]=json!("closed");project_media_readiness(&mut d).unwrap();assert!(d["items"][0].get("mediaReadiness").is_none());
 }
 fn audio_only_projection_fixture()->Value {
  let mut d=crate::empty();crate::accounts::initialize(&mut d,crate::accounts::Profile::BawRussia).unwrap();
  d["posts"]=json!([
   {"id":"source","postKey":"12185:source","sourceUrl":"https://www.youtube.com/watch?v=_Ra61_HxCxc","title":"Source video","attachments":[{"type":"video"}]},
   {"id":"target","postKey":"12182:target","sourceUrl":"https://vk.com/video-1_1","title":"Different target video title","attachments":[{"type":"video"}]}]);
  d["items"]=json!((0..4).map(|n|json!({"id":format!("audio-item-{n}"),"itemId":format!("audio-item-{n}"),"postId":"source","postKey":"12185:source","objectId":"12185","conversationKey":format!("12185:thread-{n}"),"providerStatus":"new","workflow":"attention","revision":1})).collect::<Vec<_>>());
  crate::list_mut(&mut d,"items").push(json!({"id":"equivalent-item","itemId":"equivalent-item","postId":"target","postKey":"12182:target","objectId":"12182","conversationKey":"12182:thread","providerStatus":"new","workflow":"attention","revision":1}));
  let source_pin=source_version(&d["posts"][0],"BAW Russia");
  // Live RFC evidence: full 17:09 audio, including both transcription segments.
  d["materials"]=json!([{"id":"audio","account":"BAW Russia","kind":"transcript","postKey":"12185:source","revision":1,"sourceUrl":d["posts"][0]["sourceUrl"],"mediaSha256":"a".repeat(64),"text":"Complete source transcript fixture",
   "transcription":{"audioDurationSeconds":1029.24125,"audioStatus":"transcribed","coverage":"full_audio","maxAudioSeconds":1030,"mediaDurationSeconds":1029.261,"model":"local-whisper","modelFile":"ggml-large-v3.bin","ocr":{"status":"not_requested_audio_only"},"partial":false,
    "segments":[{"audioDurationSeconds":899.9925,"audioStatus":"transcribed","endSeconds":900,"startSeconds":0},{"audioDurationSeconds":129.24875,"audioStatus":"transcribed","endSeconds":1029.261,"startSeconds":900}],"sourcePostKey":"12185:source","sourceVersion":source_pin}}]);
  crate::knowledge::sync_catalog(&mut d,"2026-09-24T23:34:55Z").unwrap();
  let binding=crate::active_binding(&d).unwrap().to_json();
  d["settings"]["postMediaPolicies"]=json!({});
  for post in d["posts"].as_array().unwrap().clone(){d["settings"]["postMediaPolicies"][txt(&post,"id")]=json!({"version":1,"revision":1,"status":"active","postId":post["id"],"account":"BAW Russia","connectorBinding":binding,"sourceVersion":source_version(&post,"BAW Russia"),"mode":"full_audio_only"});}
  let v=&d["knowledge_versions"][0];
  d["settings"]["mediaAudioEquivalences"]=json!({"target":{"schemaVersion":1,"status":"active","revision":1,"account":"BAW Russia","connectorBinding":binding,"targetPostId":"target","targetPostKey":"12182:target","targetSourceVersion":source_version(&d["posts"][1],"BAW Russia"),"sourcePostId":"source","sourcePostKey":"12185:source","sourceVersion":source_version(&d["posts"][0],"BAW Russia"),"transcript":{"entryId":v["entryId"],"versionId":v["id"],"hash":v["hash"]}}});
  d
 }
 fn assert_projection_matches_preparation(d:&Value,expected:&[&str]) {
  let states=crate::media_queue::preparation_states(d,rows(d,"items"),&crate::now()).unwrap();
  let mut view=d.clone();view["mediaReadinessCatalog"]=json!({"knowledge_entries":d["knowledge_entries"],"knowledge_versions":d["knowledge_versions"]});
  project_media_readiness(&mut view).unwrap();
  for (item,want) in rows(&view,"items").iter().zip(expected){assert_eq!(item["mediaReadiness"]["status"],*want);assert_eq!(states.get(txt(item,"id")).and_then(|s|*s).unwrap_or("ready"),*want);}
  assert!(view.get("mediaReadinessCatalog").is_none());assert_eq!(view["settings"],d["settings"]);
 }
 #[test] fn audio_only_projection_preserves_full_audio_policy_and_owner_equivalence(){
  let d=audio_only_projection_fixture();assert_projection_matches_preparation(&d,&["ready","ready","ready","ready","ready"]);
  let mut partial=d.clone();partial["materials"][0]["transcription"]["partial"]=json!(true);crate::knowledge::sync_catalog(&mut partial,"2026-09-25T00:00:00Z").unwrap();
  assert_projection_matches_preparation(&partial,&["media_unavailable","media_unavailable","media_unavailable","media_unavailable","media_unavailable"]);
 }
 #[test] fn audio_only_projection_does_not_admit_foreign_company_policies_or_equivalence(){
  let mut d=audio_only_projection_fixture();
  d["settings"]["postMediaPolicies"]["source"]["account"]=json!("LikeAvto");
  d["settings"]["mediaAudioEquivalences"]["target"]["account"]=json!("LikeAvto");
  assert_projection_matches_preparation(&d,&["ready","ready","ready","ready","media_unavailable"]);
 }
 #[test] fn deleted_artifact_invalidates_exact_cached_proof_on_refresh(){let e=fixture("LikeAvto","delete-one-only");let s=store().unwrap();let path=s.path(&reference(&e["finalEvidence"]).unwrap()).unwrap();std::fs::remove_file(path).unwrap();assert!(verify_and_cache(&e).is_err());assert!(validate_evidence(&e).is_err());}
}
