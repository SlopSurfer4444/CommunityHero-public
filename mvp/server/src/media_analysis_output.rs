//! Immutable ASR output closure. Output capture is independent of post/catalog
//! applicability: failed OCR or a later alias conflict cannot erase paid words.
use crate::media_artifacts::{ArtifactRef, ArtifactStore};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{fs::{self, OpenOptions}, io::{Read,Write}, path::{Path, PathBuf}};

pub(crate) type LifecycleFuture<'a,T> = std::pin::Pin<Box<dyn std::future::Future<Output=Result<T,String>>+Send+'a>>;
/// The concrete App adapter commits events under the existing company writer.
/// Asset output commits deliberately exclude mutable alias/catalog admission.
pub(crate) trait AudioLifecycle: Sync {
    fn binding(&self)->Value;
    fn reserve(&self,request:Value)->LifecycleFuture<'_,Value>;
    fn event(&self,event:&'static str,request:Value)->LifecycleFuture<'_,()>;
}
pub(crate) struct SegmentExecution {
    pub raw:String,pub normalized:String,pub actual_ms:u64,
    pub(super) gate:Option<super::gpu_gate::Lease>,
}
impl From<(String,String,u64)> for SegmentExecution {
    fn from((raw,normalized,actual_ms):(String,String,u64))->Self{Self{raw,normalized,actual_ms,gate:None}}
}

pub(crate) async fn execute_segments<L,D,F,T>(store:&ArtifactStore,request:&Value,reserved:&Value,lifecycle:&L,mut dispatch:D)->Result<(Vec<Value>,Vec<String>),String>
where L:AudioLifecycle+?Sized,D:FnMut(usize,u64,u64)->F,F:std::future::Future<Output=Result<T,String>>,T:Into<SegmentExecution> {
    let plan=request["segments"].as_array().ok_or("media_asr_segment_plan_invalid")?;
    let mut outputs=Vec::new();let mut texts=Vec::new();let mut bytes=0usize;
    for (index,window) in plan.iter().enumerate(){
        let start=window["startMs"].as_u64().ok_or("media_asr_segment_plan_invalid")?;
        let end=window["endMs"].as_u64().filter(|end|*end>start).ok_or("media_asr_segment_plan_invalid")?;
        let old=reserved["completedSegments"].as_array().or_else(||reserved["attempt"]["segments"].as_array())
            .and_then(|segments|segments.iter().find(|s|s["index"]==index));
        let (output,text)=if let Some(old)=old {(old.clone(),read_segment(store,request,old)?)}else{
            let mut intent=request.clone();intent["segmentIndex"]=json!(index);
            lifecycle.event("mark_dispatched",intent.clone()).await?;
            let returned=dispatch(index,start,end-start).await;
            let returned:SegmentExecution=match returned {
                Ok(output)=>output.into(),
                Err(e)=>{intent["reason"]=json!("output_missing_after_dispatch");let _=lifecycle.event("fail",intent).await;return Err(e);}
            };
            let output=match capture_segment(store,request,index,&returned.raw,&returned.normalized,returned.actual_ms){
                Ok(output)=>output,
                Err(e)=>{intent["reason"]=json!("output_missing_after_dispatch");let _=lifecycle.event("fail",intent).await;return Err(e);}
            };
            // Capture precedes the writer commit. A rejected commit leaves the
            // declared closure for fenced reconciliation without another ASR.
            intent["segment"]=output.clone();lifecycle.event("commit_segment",intent).await?;
            // GPU cleanup is follow-up to the durable paid output. A failed
            // clean marker keeps the resource quarantined but never loses words.
            if let Some(gate)=returned.gate{gate.finish()?;}
            (output,returned.normalized)
        };
        bytes=bytes.saturating_add(text.len());if bytes>512*1024{return Err("audio_transcript_limit".into());}
        outputs.push(output);texts.push(text);
    }Ok((outputs,texts))
}
pub(crate) async fn complete_before_followup<L,D,F>(store:&ArtifactStore,request:&Value,segments:&[Value],audio:Value,lifecycle:&L,followup:D)->Result<Value,String>
where L:AudioLifecycle+?Sized,D:FnOnce(Value)->F,F:std::future::Future<Output=Result<Value,String>> {
    let output=capture_full(store,request,segments,&audio)?;
    let mut event=request.clone();event["result"]=output.clone();
    lifecycle.event("commit_full_result",event).await?;
    let mut audio=audio;audio["audioAnalysis"]=json!({"request":request,"result":output});
    followup(audio).await
}

const LIMIT:u64=4*1024*1024;
// A bounded raw string can expand sixfold when encoded as JSON.
const OBJECT_LIMIT:u64=32*1024*1024;
#[cfg(test)]
#[path="media_analysis_output_tests.rs"]
mod tests;
fn error(_:impl std::fmt::Display)->String{"media_asr_output_unavailable".into()}
fn hash(value:&Value)->String{format!("{:x}",Sha256::digest(value.to_string().as_bytes()))}
fn segment_payload(value:&Value)->Value{json!({"index":value["index"],"startMs":value["startMs"],"endMs":value["endMs"],
    "actualDurationMs":value["actualDurationMs"],"outputBinding":value["outputBinding"],"rawOutput":value["rawOutput"],"normalizedOutput":value["normalizedOutput"]})}
fn result_payload(value:&Value)->Value{json!({"manifest":value["manifest"],"normalizedOutput":value["normalizedOutput"],"coverage":value["coverage"],"outcome":value["outcome"]})}
fn binding(request:&Value)->Result<Value,String>{
    for key in ["companyId","attemptId","owner","manifestKey","specSha256"] {
        if request[key].as_str().is_none_or(|v|v.is_empty()){return Err("media_asr_output_binding_invalid".into());}
    }
    if request["epoch"].as_u64().is_none()||request["verifiedFile"]["bytes"].as_u64().is_none_or(|n|n==0)
        ||request["verifiedFile"]["sha256"].as_str().is_none_or(|s|s.len()!=64||!s.bytes().all(|c|c.is_ascii_hexdigit())) {
        return Err("media_asr_output_binding_invalid".into());
    }
    Ok(json!({"companyId":request["companyId"],"verifiedFile":request["verifiedFile"],"attemptId":request["attemptId"],
        "owner":request["owner"],"epoch":request["epoch"],"specSha256":request["specSha256"],"manifestKey":request["manifestKey"],
        "segments":request["segments"],"durationMs":request["durationMs"]}))
}
fn put(store:&ArtifactStore,value:&Value)->Result<Value,String>{store.put_bytes(value.to_string().as_bytes()).map(|r|r.to_json()).map_err(error)}
fn read(store:&ArtifactStore,value:&Value)->Result<Value,String>{
    let r=ArtifactRef::from_json(value).map_err(error)?;
    serde_json::from_slice(&store.read_bytes(&r,OBJECT_LIMIT).map_err(error)?).map_err(error)
}
fn reject_links(path:&Path)->Result<(),String>{
    for p in path.ancestors(){if p.as_os_str().is_empty(){continue;}let m=fs::symlink_metadata(p).map_err(error)?;
        if m.file_type().is_symlink(){return Err("media_asr_output_linked".into());}
        #[cfg(windows)] {use std::os::windows::fs::MetadataExt;if m.file_attributes()&0x400!=0{return Err("media_asr_output_linked".into());}}
    }Ok(())
}
fn slot(store:&ArtifactStore,request:&Value,name:&str)->Result<PathBuf,String>{
    let b=binding(request)?;
    let dir=store.root().join("asr-attempts");
    reject_links(store.root())?;fs::create_dir_all(&dir).map_err(error)?;reject_links(&dir)?;
    let dir=dir.join(hash(&json!([b["companyId"],b["manifestKey"]])));
    fs::create_dir_all(&dir).map_err(error)?;reject_links(&dir)?;
    Ok(dir.join(format!("{name}.json")))
}
fn declare(store:&ArtifactStore,request:&Value,name:&str,value:&Value)->Result<(),String>{
    let path=slot(store,request,name)?;
    // create_new prevents stale workers overwriting any declared output. A
    // torn file is evidence of an interrupted capture, never replay authority.
    let mut file=OpenOptions::new().write(true).create_new(true).open(path).map_err(error)?;
    file.write_all(value.to_string().as_bytes()).map_err(error)?;file.sync_all().map_err(error)
}

pub(crate) fn capture_segment(store:&ArtifactStore,request:&Value,index:usize,raw:&str,normalized:&str,actual_ms:u64)->Result<Value,String>{
    if raw.len() as u64>LIMIT||normalized.len()>512*1024{return Err("audio_transcript_limit".into());}
    let plan=request["segments"].as_array().and_then(|p|p.get(index)).ok_or("media_asr_segment_plan_invalid")?;
    let start=plan["startMs"].as_u64().ok_or("media_asr_segment_plan_invalid")?;
    let end=plan["endMs"].as_u64().filter(|n|*n>start).ok_or("media_asr_segment_plan_invalid")?;
    if plan["index"]!=index||actual_ms.abs_diff(end-start)>250{return Err("audio_coverage_incomplete".into());}
    let b=binding(request)?;
    let raw_output=put(store,&json!({"schemaVersion":1,"binding":b,"index":index,"text":raw}))?;
    let normalized_output=put(store,&json!({"schemaVersion":1,"binding":b,"index":index,"text":normalized}))?;
    let mut output=json!({"index":index,"startMs":start,"endMs":end,"actualDurationMs":actual_ms,"outputBinding":b,
        "rawOutput":raw_output,"normalizedOutput":normalized_output});
    output["verificationSha256"]=json!(hash(&json!({"binding":b,"segment":segment_payload(&output)})));
    let closure=json!({"schemaVersion":1,"binding":b,"segment":output});
    declare(store,request,&format!("segment-{index:03}"),&closure)?;
    Ok(output)
}
pub(crate) fn read_segment(store:&ArtifactStore,request:&Value,segment:&Value)->Result<String,String>{
    let current=binding(request)?;
    let b=segment["outputBinding"].clone();
    // Recovery may have a new fenced attempt, while completed segments retain
    // the original capture owner. Only immutable analysis identity is reusable.
    for key in ["companyId","verifiedFile","specSha256","segments","durationMs"] {
        if b[key]!=current[key]{return Err("media_asr_output_binding_changed".into());}
    }
    let index=segment["index"].as_u64().ok_or("media_asr_segment_plan_invalid")? as usize;
    let plan=request["segments"].as_array().and_then(|p|p.get(index)).ok_or("media_asr_segment_plan_invalid")?;
    if segment["startMs"]!=plan["startMs"]||segment["endMs"]!=plan["endMs"]{return Err("media_asr_segment_plan_invalid".into());}
    let raw=read(store,&segment["rawOutput"])?;let normalized=read(store,&segment["normalizedOutput"])?;
    if raw["binding"]!=b||normalized["binding"]!=b||raw["index"]!=index||normalized["index"]!=index{return Err("media_asr_output_binding_changed".into());}
    if segment["verificationSha256"]!=hash(&json!({"binding":b,"segment":segment_payload(segment)})){return Err("media_asr_output_invalid".into());}
    let start=plan["startMs"].as_u64().ok_or("media_asr_segment_plan_invalid")?;let end=plan["endMs"].as_u64().ok_or("media_asr_segment_plan_invalid")?;
    let actual=segment["actualDurationMs"].as_u64().ok_or("media_asr_output_invalid")?;
    if end<=start||actual.abs_diff(end-start)>250{return Err("audio_coverage_incomplete".into());}
    normalized["text"].as_str().map(str::to_owned).ok_or("media_asr_output_invalid".into())
}
pub(crate) fn capture_full(store:&ArtifactStore,request:&Value,segments:&[Value],audio:&Value)->Result<Value,String>{
    let b=binding(request)?;
    let plan=request["segments"].as_array().ok_or("media_asr_segment_plan_invalid")?;
    if plan.len()!=segments.len(){return Err("audio_coverage_incomplete".into());}
    for (i,segment) in segments.iter().enumerate(){if segment["index"]!=i{return Err("audio_coverage_incomplete".into());}read_segment(store,request,segment)?;}
    let normalized_output=put(store,&json!({"schemaVersion":1,"binding":b,"audio":audio}))?;
    let manifest=put(store,&json!({"schemaVersion":1,"binding":b,"segments":segments,"normalizedOutput":normalized_output}))?;
    let mut result=json!({"manifest":manifest,"normalizedOutput":normalized_output,"coverage":audio["coverage"],"outcome":audio["outcome"]});
    result["verificationSha256"]=json!(hash(&json!({"binding":b,"result":result_payload(&result)})));
    declare(store,request,"full",&json!({"schemaVersion":1,"binding":b,"result":result}))?;Ok(result)
}
pub(crate) fn read_full(store:&ArtifactStore,request:&Value,result:&Value)->Result<Value,String>{
    let b=binding(request)?;
    let manifest=read(store,&result["manifest"])?;let normalized=read(store,&result["normalizedOutput"])?;
    if manifest["binding"]!=b||normalized["binding"]!=b||manifest["normalizedOutput"]!=result["normalizedOutput"]{return Err("media_asr_output_binding_changed".into());}
    if normalized["audio"]["coverage"]!=result["coverage"]||normalized["audio"]["outcome"]!=result["outcome"]
        || result["coverage"]["durationMs"]!=request["durationMs"]
        || !matches!(result["outcome"].as_str(),Some("transcript"|"no_speech"|"no_audio")) {
        return Err("media_asr_output_coverage_changed".into());
    }
    let segments=manifest["segments"].as_array().ok_or("media_asr_output_invalid")?;
    if segments.len()!=request["segments"].as_array().ok_or("media_asr_segment_plan_invalid")?.len(){return Err("audio_coverage_incomplete".into());}
    for (i,s) in segments.iter().enumerate(){if s["index"]!=i{return Err("audio_coverage_incomplete".into());}read_segment(store,request,s)?;}
    if result["verificationSha256"]!=hash(&json!({"binding":b,"result":result_payload(result)})){return Err("media_asr_output_invalid".into());}
    Ok(normalized["audio"].clone())
}
/// Reconciliation reads only the declared original-attempt closure. Missing or
/// torn output never authorizes paid dispatch and must remain UNKNOWN upstream.
fn read_declared(path:&Path)->Result<Value,String>{
    reject_links(path)?;
    let metadata=fs::symlink_metadata(path).map_err(error)?;
    if !metadata.is_file()||metadata.len()>LIMIT{return Err("media_asr_output_invalid".into());}
    let mut bytes=Vec::new();std::fs::File::open(path).map_err(error)?.take(LIMIT+1).read_to_end(&mut bytes).map_err(error)?;
    if bytes.len() as u64>LIMIT{return Err("media_asr_output_invalid".into());}
    serde_json::from_slice(&bytes).map_err(error)
}
pub(crate) fn reconcile_segment(store:&ArtifactStore,request:&Value,index:usize)->Result<Value,String>{
    let path=slot(store,request,&format!("segment-{index:03}"))?;
    let v=read_declared(&path)?;
    if v["binding"]!=binding(request)?{return Err("media_asr_output_binding_changed".into());}
    read_segment(store,request,&v["segment"])?;Ok(v["segment"].clone())
}
pub(crate) fn reconcile_full(store:&ArtifactStore,request:&Value)->Result<Value,String>{
    let path=slot(store,request,"full")?;
    let v=read_declared(&path)?;
    if v["binding"]!=binding(request)?{return Err("media_asr_output_binding_changed".into());}
    read_full(store,request,&v["result"])?;Ok(v["result"].clone())
}
