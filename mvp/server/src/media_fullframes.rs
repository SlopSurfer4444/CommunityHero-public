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
pub(crate) fn read(store:&ArtifactStore,v:&Value)->Result<Value,String>{
    let r=reference(v)?;if r.bytes>JSON_LIMIT{return Err("media_artifact_json_too_large".into());}
    let path=verified_path(store,v)?;serde_json::from_slice(&std::fs::read(path).map_err(err)?).map_err(err)
}
#[derive(Clone)]struct Proof{objects:Vec<(PathBuf,Stamp)>,valid_until:Instant}
static PROOF_EPOCH:std::sync::atomic::AtomicU64=std::sync::atomic::AtomicU64::new(1);
pub(crate) fn proof_epoch()->u64{PROOF_EPOCH.load(std::sync::atomic::Ordering::SeqCst)}
static PROOFS:OnceLock<Mutex<BTreeMap<String,Proof>>>=OnceLock::new();
#[cfg(test)]
pub(crate) fn forget_test_proof(evidence:&Value){
    if PROOFS.get_or_init(||Mutex::new(BTreeMap::new())).lock().unwrap().remove(&hash(evidence)).is_some(){
        PROOF_EPOCH.fetch_add(1,std::sync::atomic::Ordering::SeqCst);
    }
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
pub(crate) fn selection_descriptor(store:&ArtifactStore,progress:&Value)->Result<Value,String>{
 let selection=read(store,&progress["selectionDescriptor"])?;let inventory=read(store,&progress["inventoryDescriptor"])?;
 if selection["frameCount"]!=inventory["frameCount"]{return Err("media_selection_frame_count_changed".into());}
 if selection["kind"]!="media_frame_selection"||selection["schemaVersion"]!=2||selection["inventoryDescriptor"]!=progress["inventoryDescriptor"]||selection["policy"]!=crate::media_frame_selection::policy()||selection["selectedCount"].as_u64().is_none_or(|n|n==0||n>selection["frameCount"].as_u64().unwrap_or(0)){return Err("media_selection_invalid".into());}
 Ok(selection)
}
pub(crate) fn selection_rows(store:&ArtifactStore,selection:&Value,start:u64,count:u64)->Result<Vec<Value>,String>{
 read_rows(store,&json!({"frameCount":selection["selectedCount"],"index":selection["selectionIndex"],"inventory":selection["selection"]}),start,count,"selectionIndex")
}
pub(crate) fn wire_inventory(d:&Value,selection:&Value,selection_ref:&Value)->Value{json!({"sha256":d["inventory"]["sha256"],"frameCount":d["frameCount"],"selectionSha256":selection_ref["sha256"],"selectionPolicySha256":selection["policy"]["sha256"],"selectedFrameCount":selection["selectedCount"],"decoderContractSha256":d["decoderContractSha256"],"timeBaseNumerator":d["timeBaseNumerator"],"timeBaseDenominator":d["timeBaseDenominator"],"width":d["width"],"height":d["height"],"pixelFormat":"rgb24"})}

/// Validate the entire immutable receipt chain against inventory, including
/// aliases to earlier exact pixels. A missing artifact is never a ready token.
pub(crate) fn reviewed(store:&ArtifactStore,descriptor_ref:&Value,selection_ref:&Value,latest:&Value)->Result<(BTreeMap<String,Value>,Vec<Value>,u64),String>{
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
    let mut last_source_index=None;let mut covered=0;let mut pixels=BTreeMap::new();let mut observations=Vec::new();let mut provenance=Value::Null;
    for (receipt_ref,receipt) in chain {
        let first=receipt["firstSelectionIndex"].as_u64().ok_or("media_chunk_invalid")?;
        let end=receipt["endSelectionIndexExclusive"].as_u64().ok_or("media_chunk_invalid")?;
        if first!=covered || end<=first || end-first>crate::media_frame_contract::MAX_CHUNK_POSITIONS{return Err("media_chunk_gap_or_overlap".into());}
        let inventory=selection_rows(store,&selection,first,end-first)?;
        for row in &inventory {let source_index=row["frameIndex"].as_u64().ok_or("media_selection_frame_invalid")?;if last_source_index.is_some_and(|n|source_index<=n)||last_source_index.is_none()&&source_index!=0{return Err("media_selection_order_invalid".into());}last_source_index=Some(source_index);let actual=inventory_rows(store,&d,row["frameIndex"].as_u64().ok_or("media_selection_frame_invalid")?,1)?;if ["frameIndex","pts","timestampMs","pixelSha256"].iter().any(|k|actual[0][*k]!=row[*k]){return Err("media_selection_inventory_mismatch".into());}}
        if !receipt["response"].is_null(){
            let response=crate::media_frame_contract::validate_response(&receipt["request"],&receipt["response"]).map_err(str::to_owned)?;
            if response["inventory"]!=wire_inventory(&d,&selection,selection_ref)||response["source"]!=d["sourceIdentity"]
                || response["chunk"]["leaseId"]!=receipt["leaseId"]||response["chunk"]["previousReceiptSha256"]!=receipt["previousReceipt"]["sha256"]
                || response["chunk"]["firstSelectionIndex"]!=first||response["chunk"]["endSelectionIndexExclusive"]!=end{return Err("media_chunk_binding_invalid".into());}
            if provenance.is_null(){provenance=response["provenance"].clone();}else if provenance!=response["provenance"]{return Err("media_model_epoch_changed".into());}
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
    Ok((pixels,observations,covered))
}
pub(crate) fn final_evidence(store:&ArtifactStore,progress:&Value)->Result<Value,String>{
    let d=descriptor(store,progress)?;let selection=selection_descriptor(store,progress)?;
    let (pixels,observations,covered)=reviewed(store,&progress["inventoryDescriptor"],&progress["selectionDescriptor"],&progress["latestReceipt"])?;
    if covered!=selection["selectedCount"] || progress["nextSelectionIndex"]!=covered {return Err("media_full_coverage_incomplete".into());}
    let (notes,overflow)=aggregate_observations(&observations)?;
    let aggregate=json!({"schemaVersion":VERSION,"kind":"media_visual_aggregate","sourceIdentity":d["sourceIdentity"],"inventoryDescriptor":progress["inventoryDescriptor"],"selectionDescriptor":progress["selectionDescriptor"],"aggregateOverflow":overflow,"observations":notes});
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
    let (pixels,observations,covered)=reviewed(&store,&final_value["inventoryDescriptor"],&final_value["selectionDescriptor"],&final_value["latestReceipt"])?;
    let aggregate=read(&store,&final_value["aggregate"])?;
    let (expected_notes,overflow)=aggregate_observations(&observations)?;
    if overflow||aggregate["observations"]!=expected_notes||d["sourceIdentity"]!=evidence["source"]||d["source"]["sha256"]!=evidence["source"]["mediaSha256"]
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
 let store=store().unwrap();let source=store.put_bytes(format!("synthetic source {account} {post}").as_bytes()).unwrap().to_json();
 let inventory_rows:Vec<_>=pixels.iter().enumerate().map(|(i,p)|json!({"frameIndex":i,"pts":(i*33).to_string(),"timestampMs":i*33,"pixelSha256":p})).collect();
 let inventory=store.write_jsonl(inventory_rows.clone()).unwrap().to_json();
 let index=put(&store,&json!({"schemaVersion":2,"kind":"media_frame_index","offsets":[{"frameIndex":0,"byteOffset":0}]})).unwrap();
 let identity=json!({"account":account,"postKey":post,"mediaSha256":source["sha256"],"durationMs":1000});
 let d=json!({"schemaVersion":2,"kind":"media_frame_inventory","source":source,"inventory":inventory,"index":index,"sourceIdentity":identity,"frameCount":pixels.len(),"timeBaseNumerator":1,"timeBaseDenominator":1000,"width":2,"height":2,"pixelFormat":"rgb24","decoderContractSha256":"c".repeat(64)});
 let dr=put(&store,&d).unwrap();let selected:Vec<_>=inventory_rows.iter().enumerate().map(|(i,r)|{let mut r=r.clone();r["selectionIndex"]=json!(i);r["reasons"]=json!(["baseline"]);r}).collect();
 let selection_data=store.write_jsonl(selected.clone()).unwrap().to_json();
 let selection=json!({"schemaVersion":2,"kind":"media_frame_selection","inventoryDescriptor":dr,"selection":selection_data,"selectionIndex":index,"selectedCount":pixels.len(),"frameCount":pixels.len(),"policy":crate::media_frame_selection::policy(),"reasonCounts":{"baseline":pixels.len()}});
 let sr=put(&store,&selection).unwrap();let lease="fixture-lease";let mut latest=Value::Null;let mut seen:BTreeMap<String,Value>=BTreeMap::new();
 for (i,row) in selected.iter().enumerate(){
  let pixel=txt(row,"pixelSha256");let mut req=Value::Null;let mut response=Value::Null;
  let prior=seen.get(pixel).cloned();let frame_id=prior.as_ref().map(|p|p["frameId"].clone()).unwrap_or_else(||json!(format!("f{i}")));
  if prior.is_none(){
   let frame=json!({"id":frame_id,"frameIndex":i,"selectionIndex":i,"selectionReasons":row["reasons"],"pts":row["pts"],"timestampMs":row["timestampMs"],"pixelSha256":pixel,"sha256":"d".repeat(64),"path":"private/vision-frames/f.png","mimeType":"image/png"});
   req=crate::media_frame_contract::seal_request(json!({"schemaVersion":2,"workId":"media-00000000-0000-0000-0000-000000000000","createdAtUtc":"2026-09-23T00:00:00Z","source":identity,"inventory":wire_inventory(&d,&selection,&sr),"chunk":{"firstSelectionIndex":i,"endSelectionIndexExclusive":i+1,"previousReceiptSha256":latest["sha256"],"leaseId":lease},"frames":[frame]}));
   let mut observation=frame;let object=observation.as_object_mut().unwrap();object.remove("path");object.remove("mimeType");
   observation["status"]=json!("readable");observation["scene"]=json!(format!("Frame group {pixel}"));observation["text"]=if i==1{json!(["2490000 RUB","до Владивостока"])}else{json!([])};observation["numbers"]=json!([]);observation["uncertainties"]=json!([]);
   response=json!({"schemaVersion":2,"status":"complete","source":identity,"inventory":req["inventory"],"chunk":req["chunk"],"manifestSha256":req["manifestSha256"],"frames":[observation],"summary":"Inspected frame","provenance":{"backend":"offline-test","model":"test","instructionSha256":"e".repeat(64)}});
  }
  let receipt=put(&store,&json!({"schemaVersion":2,"kind":"media_visual_chunk","inventoryDescriptor":dr,"selectionDescriptor":sr,"previousReceipt":latest,"firstSelectionIndex":i,"endSelectionIndexExclusive":i+1,"request":req,"response":response,"aliases":[{"selectionIndex":i,"frameIndex":i,"pts":row["pts"],"pixelSha256":pixel,"frameId":frame_id,"receipt":prior.as_ref().map(|p|p["receipt"].clone())}],"leaseId":lease})).unwrap();
  if prior.is_none(){seen.insert(pixel.to_owned(),json!({"frameId":frame_id,"receipt":receipt}));}latest=receipt;
 }
 let progress=json!({"sourceVersion":post_version,"account":account,"sourcePostKey":post,"source":source,"inventory":inventory,"inventoryDescriptor":dr,"selectionDescriptor":sr,"latestReceipt":latest,"nextSelectionIndex":pixels.len()});
 let evidence=final_evidence(&store,&progress).unwrap();verify_and_cache(&evidence).unwrap();evidence
}

#[cfg(test)]
mod tests{
 use super::*;
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
  let e=fixture_for_post("LikeAvto",&d["posts"][0]);d["materials"]=json!([{"id":"audio","kind":"transcript","postKey":"11391:p","text":"Audio evidence"},{"id":"visual","account":"LikeAvto","kind":"visual_context","postKey":"11391:p","mediaSha256":e["source"]["mediaSha256"],"text":"Selected visual observations","visualEvidence":e}]);crate::knowledge::sync_catalog(&mut d,"2026-09-23T00:00:00Z").unwrap();
  d["mediaReadinessCatalog"]=json!({"knowledge_entries":d["knowledge_entries"],"knowledge_versions":d["knowledge_versions"]});project_media_readiness(&mut d).unwrap();assert_eq!(d["items"][0]["mediaReadiness"]["status"],"ready");assert!(d.get("mediaReadinessCatalog").is_none());
  d["items"][0]["workflow"]=json!("closed");project_media_readiness(&mut d).unwrap();assert!(d["items"][0].get("mediaReadiness").is_none());
 }
 #[test] fn deleted_artifact_invalidates_exact_cached_proof_on_refresh(){let e=fixture("LikeAvto","delete-one-only");let s=store().unwrap();let path=s.path(&reference(&e["finalEvidence"]).unwrap()).unwrap();std::fs::remove_file(path).unwrap();assert!(verify_and_cache(&e).is_err());assert!(validate_evidence(&e).is_err());}
}
