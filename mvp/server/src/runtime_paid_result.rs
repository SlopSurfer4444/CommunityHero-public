//! Immutable company-bound paid captures. References are evidence only: never
//! a second attempt ledger, retry permission, or social dispatch authority.
use crate::{App,ApiResult,Value,internal};
use crate::media_artifacts::{ArtifactStore,ArtifactRef};
use serde_json::json;
use sha2::{Digest,Sha256};
const MAX_CAPTURE_BYTES:u64=128*1024*1024;
fn digest(value:&Value)->String {format!("{:x}",Sha256::digest(value.to_string().as_bytes()))}
fn valid_reference(reference:&Value)->bool {
    let fields=["version","kind","company","account","binding","runtimeOwner","requestSha256","responseSha256","artifact","retryAuthorized","dispatchAuthorized"];
    let hash=|v:&Value|v.as_str().is_some_and(|s|s.len()==64&&s.bytes().all(|b|b.is_ascii_digit()||(b'a'..=b'f').contains(&b)));
    let binding=&reference["binding"];
    let short=|v:&Value|v.as_str().is_some_and(|s|!s.is_empty()&&s.len()<=160);
    let provenance=["connectorBindingSha256","sourceIdentitySha256","actionsSha256"];
    let identifiers=["jobId","actionId","itemId","runId","purpose"];
    reference.as_object().is_some_and(|o|o.len()==fields.len()&&fields.iter().all(|key|o.contains_key(*key)))
        && reference["version"]==1&&reference["kind"]=="native-paid-capture-ref"
        && matches!(binding["operation"].as_str(),Some("assistant"|"assistant_research"|"media"|"media_vision"|"media_vision_chunk"))
        && binding.as_object().is_some_and(|o|o.contains_key("nativeJobId")&&o.keys().all(|key|
            key=="nativeJobId"||key=="operation"||identifiers.contains(&key.as_str())||provenance.contains(&key.as_str())))
        && (binding["nativeJobId"].is_null()||short(&binding["nativeJobId"]))
        && identifiers.iter().all(|key|binding.get(*key).is_none_or(short))
        && provenance.iter().all(|key|binding.get(*key).is_none_or(hash))
        && reference["runtimeOwner"].as_object().is_some_and(|o|o.len()==3&&["account","runtimeId","releaseSha256"].iter().all(|key|o.contains_key(*key)))
        && short(&reference["runtimeOwner"]["account"])&&short(&reference["runtimeOwner"]["runtimeId"])
        && hash(&reference["runtimeOwner"]["releaseSha256"])
        && hash(&reference["requestSha256"])&&hash(&reference["responseSha256"])
        && reference["retryAuthorized"]==false&&reference["dispatchAuthorized"]==false
}
// This exact native vocabulary is an owner attestation, not a transport
// Authorization header. Recognition only permits durable bytes; the existing
// media/family validators still establish the proof's authority/currentness.
fn audio_equivalence_label(value:&Value,container:Option<&str>)->bool {
    if !matches!(container,Some("audioEquivalence"|"supports"|"mediaBinding")){return false;}
    let fields=["match","authorization","equivalenceSha256","equivalenceRevision","targetPostId","postKey",
        "targetSourceVersion","sourcePostId","sourcePostKey","sourceVersion","account","connectorBinding",
        "transcript","identities","byteEqualityClaimed"];
    let hash=|v:&Value|v.as_str().is_some_and(|s|s.len()==64&&s.bytes().all(|b|b.is_ascii_digit()||(b'a'..=b'f').contains(&b)));
    value.as_object().is_some_and(|object|object.len()==fields.len()&&fields.iter().all(|key|object.contains_key(*key)))
        &&value["match"]=="owner_confirmed_audio_equivalence"&&value["authorization"]=="owner_confirmed_same_video"
        &&hash(&value["equivalenceSha256"])&&value["equivalenceRevision"].as_u64().is_some_and(|n|n>0)
        &&["targetPostId","postKey","targetSourceVersion","sourcePostId","sourcePostKey","sourceVersion"]
            .iter().all(|key|value[*key].as_str().is_some_and(|s|!s.is_empty()))
        &&matches!(value["account"].as_str(),Some("BAW Russia"|"LikeAvto"))&&value["connectorBinding"].is_object()
        &&value["transcript"].as_object().is_some_and(|object|object.len()==3&&["entryId","versionId","hash"].iter().all(|key|object.contains_key(*key)))
        &&hash(&value["transcript"]["hash"])&&["entryId","versionId"].iter().all(|key|value["transcript"][*key].as_str().is_some_and(|s|!s.is_empty()))
        &&value["identities"].as_array().is_some_and(Vec::is_empty)&&value["byteEqualityClaimed"]==false
}
fn credential_field(value:&Value)->bool { credential_field_in(value,None) }
fn credential_field_in(value:&Value,container:Option<&str>)->bool {
    let semantic_label=audio_equivalence_label(value,container);
    match value {
        Value::Object(fields)=>fields.iter().any(|(key,value)|{
            let normalized=key.to_ascii_lowercase().replace(['-','_'],"");
            let semantic=key=="authorization"&&semantic_label;
            (!semantic&&matches!(normalized.as_str(),"authorization"|"accesstoken"|"refreshtoken"|"password"|"clientsecret"|"credentials"|"credential"|"apikey"|"cookie"|"cookies"))
                ||credential_field_in(value,Some(key.as_str()))
        }),
        Value::Array(rows)=>rows.iter().any(|value|credential_field_in(value,container)),_=>false,
    }
}
fn record(app:&App,operation:&str,request:&Value,response:&Value)->ApiResult<Value> {
    // Validate before copying secrets into a retained document. Model bridge
    // requests must contain prompts/context, never provider credentials.
    if credential_field(request)||credential_field(response){return Err(internal("Paid capture contains forbidden credential fields; not retained"));}
    let identity=&app.lifecycle_owner;
    let mut binding=json!({"nativeJobId":crate::runtime_lifecycle_app::current_job(),"operation":operation});
    for key in ["jobId","actionId","itemId","runId","purpose"] {
        if let Some(value)=request.get(key).and_then(Value::as_str).filter(|value|!value.is_empty()&&value.len()<=160
            &&value.bytes().all(|b|b.is_ascii_alphanumeric()||matches!(b,b'-'|b'_'|b':'))) {binding[key]=json!(value);}
    }
    // Large/free-form provenance stays in the immutable request. Hot bindings
    // retain bounded digests, never a duplicate arbitrary source identity.
    for key in ["connectorBinding","sourceIdentity","actions"] {
        if let Some(value)=request.get(key) {binding[format!("{key}Sha256")]=json!(digest(value));}
    }
    Ok(json!({"version":2,"kind":"retained-native-paid-stage-result","company":app.account.key(),"account":app.account.display(),
        "runtimeOwner":{"account":identity.account,"runtimeId":identity.runtime_id,"releaseSha256":identity.release_sha256},
        "binding":binding,"requestSha256":digest(request),"responseSha256":digest(response),"request":request,"response":response,
        "retryAuthorized":false,"dispatchAuthorized":false}))
}
fn pointer(record:&Value,reference:&ArtifactRef)->Value {
    json!({"version":1,"kind":"native-paid-capture-ref","company":record["company"],"account":record["account"],
        "binding":record["binding"],"runtimeOwner":record["runtimeOwner"],"requestSha256":record["requestSha256"],
        "responseSha256":record["responseSha256"],"artifact":reference.to_json(),"retryAuthorized":false,"dispatchAuthorized":false})
}
/// Resolve immutable original bytes after restart; a current runtime identity is
/// deliberately NOT substituted for the historical capture's runtime owner.
/// Missing, corrupt, foreign or hash-mismatched objects return an error. There
/// is no model/provider call or fallback that could recreate paid evidence.
pub(crate) fn resolve_from(store:&ArtifactStore,company:&str,account:&str,job:Option<&str>,operation:&str,reference:&Value)->ApiResult<Value> {
    if !valid_reference(reference)
        || reference["company"]!=company || reference["account"]!=account
        || reference["binding"]["nativeJobId"].as_str()!=job
        || reference["binding"]["operation"]!=operation
        || reference["retryAuthorized"]!=false || reference["dispatchAuthorized"]!=false {
        return Err(crate::conflict("Paid capture reference identity mismatch"));
    }
    let artifact=ArtifactRef::from_json(&reference["artifact"]).map_err(|_|internal("Invalid paid capture artifact"))?;
    let bytes=store.read_bytes(&artifact,MAX_CAPTURE_BYTES).map_err(|_|internal("Paid capture missing or corrupt"))?;
    let record:Value=serde_json::from_slice(&bytes).map_err(|_|internal("Invalid paid capture document"))?;
    if record["version"]!=2 || record["kind"]!="retained-native-paid-stage-result"
        || record.get("request").is_none() || record.get("response").is_none()
        || pointer(&record,&artifact)!=*reference || digest(&record["request"])!=record["requestSha256"]
        || digest(&record["response"])!=record["responseSha256"] || credential_field(&record) {
        return Err(internal("Paid capture reconstruction mismatch"));
    }
    Ok(record)
}
/// Attach only an immutable reference to the EXISTING exact native job. The
/// caller owns the normal App.change_job writer gate and lifecycle capture.
/// Existing results, stages, status, UNKNOWN and approvals remain untouched.
pub(crate) fn attach(d:&mut Value,job:&str,reference:&Value,identity:&crate::runtime_lifecycle::RuntimeIdentity)->ApiResult<()> {
    crate::runtime_lifecycle::current_owner(d,identity)?;
    if !valid_reference(reference)
        || reference["company"]!=crate::accounts::Profile::from_workspace(d)?.key()
        || reference["account"]!=d["account"] || reference["runtimeOwner"]["account"]!=identity.account
        || reference["runtimeOwner"]["runtimeId"]!=identity.runtime_id
        || reference["runtimeOwner"]["releaseSha256"]!=identity.release_sha256
        || reference["binding"]["nativeJobId"]!=job
        || reference["retryAuthorized"]!=false || reference["dispatchAuthorized"]!=false {
        return Err(crate::conflict("Paid capture belongs to another job or owner"));
    }
    ArtifactRef::from_json(&reference["artifact"]).map_err(|_|internal("Invalid paid capture artifact"))?;
    if crate::list(d,"jobs").iter().filter(|row|row["id"]==job).count()!=1 {
        return Err(crate::conflict("Paid capture job missing or duplicated"));
    }
    let job=crate::row_mut(d,"jobs",job)?;
    if job.get("retainedEvidence").is_none(){job["retainedEvidence"]=json!([]);}
    let rows=job["retainedEvidence"].as_array_mut().ok_or_else(||internal("Invalid retained evidence list"))?;
    if rows.iter().any(|existing|existing==reference){return Ok(());}
    if rows.iter().any(|existing|existing["artifact"]==reference["artifact"]) {
        return Err(crate::conflict("Paid capture reference already has another binding"));
    }
    rows.push(reference.clone());
    Ok(())
}
pub(crate) async fn resolve(app:&App,job:&str,operation:&str,reference:&Value)->ApiResult<Value> {
    let root=crate::media_fullframes::store().map_err(|_|internal("Paid result evidence store unavailable"))?.root().join("runtime-paid").join(app.account.key());
    let company=app.account.key().to_owned();let account=app.account.display().to_owned();
    let job=job.to_owned();let operation=operation.to_owned();let reference=reference.clone();
    let retained_reader=crate::runtime_lifecycle_app::TaskCount::begin(app.lifecycle_task_count.clone());
    tokio::task::spawn_blocking(move||{
        let _retained_reader=retained_reader;
        let store=ArtifactStore::open(&root).map_err(|_|internal("Paid result evidence store unavailable"))?;
        resolve_from(&store,&company,&account,Some(&job),&operation,&reference)
    }).await.map_err(|_|internal("Paid capture reader failed"))?
}
/// The cold objects are immutable; their durable hot references are history.
/// Every scoped writer that sees a job preserves its old exact prefix. This
/// performs no filesystem I/O and grants no capture/attempt/dispatch authority.
pub(crate) fn validate_change(before:&Value,after:&Value)->ApiResult<()> {
    let old=before.get("jobs").and_then(Value::as_array);
    let new=after.get("jobs").and_then(Value::as_array);
    let (old,new)=match (old,new) {
        (None,None)=>return Ok(()),
        (Some(old),Some(new))=>(old,new),
        _=>return Err(internal("Paid capture history scope changed")),
    };
    for job in old {
        let Some(value)=job.get("retainedEvidence") else {continue;};
        let refs=value.as_array().ok_or_else(||internal("Invalid paid capture history"))?;
        let matches=new.iter().filter(|candidate|candidate["id"]==job["id"]).collect::<Vec<_>>();
        if matches.len()!=1 || matches[0]["retainedEvidence"].as_array().is_none_or(|next|!next.starts_with(refs)) {
            return Err(internal("Paid capture references cannot be deleted or rebound"));
        }
    }
    for job in new {
        let Some(value)=job.get("retainedEvidence") else {continue;};
        let refs=value.as_array().ok_or_else(||internal("Invalid paid capture history"))?;
        let account=after["account"].as_str().ok_or_else(||internal("Paid capture account missing"))?;
        let company=crate::accounts::Profile::from_workspace(after)?.key();
        let mut artifacts=std::collections::HashSet::new();
        for reference in refs {
            if !valid_reference(reference) || reference["company"]!=company || reference["account"]!=account
                || reference["binding"]["nativeJobId"]!=job["id"]
                || crate::media_artifacts::ArtifactRef::from_json(&reference["artifact"]).is_err()
                || !artifacts.insert(reference["artifact"]["sha256"].as_str()) {
                return Err(internal("Invalid paid capture history binding"));
            }
        }
    }
    Ok(())
}
pub(crate) async fn retain(app:&App,operation:&str,request:&Value,response:&Value)->ApiResult<Option<Value>> {
    if !matches!(operation,"assistant"|"assistant_research"|"media"|"media_vision"|"media_vision_chunk"){return Ok(None);}
    let record=record(app,operation,request,response)?;
    let root=crate::media_fullframes::store().map_err(|_|internal("Paid result evidence store unavailable"))?.root().join("runtime-paid").join(app.account.key());
    let retained_writer=crate::runtime_lifecycle_app::TaskCount::begin(app.lifecycle_task_count.clone());
    let reference=tokio::task::spawn_blocking(move||->ApiResult<_>{
        let _retained_writer=retained_writer;
        let store=ArtifactStore::open(&root).map_err(|_|internal("Paid result evidence store unavailable"))?;
        let bytes=record.to_string();
        if bytes.len() as u64>MAX_CAPTURE_BYTES{return Err(internal("Paid capture exceeds retention limit"));}
        let artifact=store.put_bytes(bytes.as_bytes()).map_err(|_|internal("Paid result evidence retention failed"))?;
        let reference=pointer(&record,&artifact);
        // Exercise the same verified reconstruction used after restart before
        // admitting this small reference into the existing job transaction.
        resolve_from(&store,record["company"].as_str().unwrap(),record["account"].as_str().unwrap(),
            record["binding"]["nativeJobId"].as_str(),record["binding"]["operation"].as_str().unwrap(),&reference)?;
        Ok(reference)
    }).await.map_err(|_|internal("Paid result evidence writer failed"))??;
    // Logs expose only CAS identity/size. Prompts, source identities, connector
    // values and arbitrary request provenance stay in the private capture.
    eprintln!("retained_native_paid_result={}",reference["artifact"]);
    Ok(Some(reference))
}
#[cfg(test)]
#[path="runtime_paid_result_tests.rs"]
mod tests;
#[cfg(test)]
#[path="runtime_paid_result_history_tests.rs"]
mod history_tests;
