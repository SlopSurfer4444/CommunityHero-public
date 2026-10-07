//! Owner-authorized recovery of a retained original, never a new download claim.
//! Ingress is an operator-private, company/binding-scoped directory beneath the
//! configured scratch root. Its ACL and configured ancestors must be trusted;
//! this is not an upload directory for untrusted local OS users.
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{fs::{self, OpenOptions}, io::{Read, Write}, path::{Component, Path, PathBuf}};
use crate::{ApiResult, media_artifacts::ArtifactRef};

const MAX_BYTES:u64=500*1024*1024;
fn text<'a>(v:&'a Value,k:&str)->&'a str{v[k].as_str().unwrap_or("")}
fn rows<'a>(v:&'a Value,k:&str)->&'a [Value]{v[k].as_array().map(Vec::as_slice).unwrap_or(&[])}
fn sha(s:&str)->bool{s.len()==64&&s.bytes().all(|c|c.is_ascii_digit()||(b'a'..=b'f').contains(&c))}
fn fields(v:&Value,allowed:&[&str])->ApiResult<()>{
    if v.as_object().is_none_or(|o|o.len()!=allowed.len()||o.keys().any(|k|!allowed.contains(&k.as_str()))){return Err(crate::bad("Invalid retained source import fields"));}Ok(())
}
fn parse_request(body:&Value,at:&str)->ApiResult<()> {
    fields(body,&["receiptId","account","postId","postKey","jobId","attemptIndex","sourceKey","sourceUrl","sourceVersion","connectorBinding","expectedError","artifactId","bytes","sha256","durationMs","provenance"])?;
    for k in ["receiptId","artifactId"] {if uuid::Uuid::parse_str(text(body,k)).is_err()||text(body,k).len()!=36{return Err(crate::bad("Import IDs must be UUIDs"));}}
    if !sha(text(body,"sha256"))||!sha(text(body,"sourceVersion"))||body["bytes"].as_u64().is_none_or(|n|n==0||n>MAX_BYTES)
        ||body["durationMs"].as_u64().is_none_or(|n|n==0||n>14_400_000)||body["attemptIndex"].as_u64().is_none(){return Err(crate::bad("Invalid retained source bounds"));}
    let proof=&body["provenance"];
    fields(proof,&["acquiredAt","method","evidenceSha256","note"])?;
    let acquired=chrono::DateTime::parse_from_rfc3339(text(proof,"acquiredAt")).map_err(|_|crate::bad("Invalid acquisition timestamp"))?;
    let now=chrono::DateTime::parse_from_rfc3339(at).map_err(|_|crate::bad("Invalid import timestamp"))?;
    if acquired>now || !sha(text(proof,"evidenceSha256")) || text(proof,"method")!="operator_retained_original_review"
        ||text(proof,"note").trim().is_empty()||text(proof,"note").len()>2000{return Err(crate::bad("Retained original provenance is required"));}
    Ok(())
}

#[derive(Clone)]
struct Admission { job:Value, post:Value, epoch:String, projection:Value }
fn replay(d:&Value,body:&Value,actor:&str)->ApiResult<Option<Value>>{
    for job in rows(d,"jobs") {let receipt=&job["sourceImport"];
        if receipt["id"]==body["receiptId"] {
            if receipt["request"]!=*body||receipt["authorizedBy"]!=actor{return Err(crate::conflict("Import receipt is already bound"));}
            // Replays do not retarget an old receipt after connector/source replacement.
            let post=crate::row(d,"posts",text(body,"postId"))?;
            if d["account"]!=body["account"]||crate::active_binding(d)?.to_json()!=body["connectorBinding"]
                ||crate::media_fullframes::source_version(post,text(body,"account"))!=body["sourceVersion"]{return Err(crate::conflict("Imported source identity changed"));}
            return Ok(Some(receipt.clone()));
        }
    }Ok(None)
}
fn admit(d:&Value,body:&Value,at:&str)->ApiResult<Admission>{
    parse_request(body,at)?;
    let account=text(body,"account");let binding=crate::active_binding(d)?.to_json();
    let post=crate::row(d,"posts",text(body,"postId"))?;
    let job=crate::row(d,"jobs",text(body,"jobId"))?;
    let index=body["attemptIndex"].as_u64().and_then(|n|usize::try_from(n).ok()).ok_or_else(||crate::bad("Invalid attempt index"))?;
    let attempt=rows(job,"sourceAttempts").get(index).ok_or_else(||crate::conflict("Failed source attempt missing"))?;
    let key=crate::knowledge::media_source_key(post,account).ok_or_else(||crate::conflict("Source identity unavailable"))?;
    if account.is_empty()||d["account"]!=account||job["account"]!=account||job["connectorBinding"]!=binding||body["connectorBinding"]!=binding
        ||(!post["connectorBinding"].is_null()&&post["connectorBinding"]!=binding)||job["refId"]!=post["id"]
        ||post["postKey"]!=body["postKey"]||attempt["postId"]!=post["id"]||attempt["postKey"]!=post["postKey"]
        ||body["sourceKey"]!=key||attempt["sourceKey"]!=key||attempt["sourceVersion"]!=body["sourceVersion"]
        ||crate::media_fullframes::source_version(post,account)!=body["sourceVersion"]||!crate::knowledge::is_video_post(post){return Err(crate::conflict("Retained source account, binding or version changed"));}
    let progress=&job["result"]["visualProgress"];
    let empty_shell=*progress==json!({"phase":"held","leaseId":null,"resumePhase":null});
    let known_failure=crate::media_processing::full::importable_download_failure(text(attempt,"error"));
    let attempts=rows(job,"sourceAttempts");
    let consumed=|index:usize,prior:&Value|!text(&prior["retryPermit"],"id").is_empty()&&attempts.iter().any(|next|
        next["retryOf"]["permitId"]==prior["retryPermit"]["id"]&&next["retryOf"]["jobId"]==job["id"]&&next["retryOf"]["attemptIndex"]==index
            &&next["sourceKey"]==prior["sourceKey"]&&next["sourceVersion"]==prior["sourceVersion"]);
    let permits_settled=attempts.iter().enumerate().all(|(index,a)|a["retryPermit"].is_null()||consumed(index,a))
        &&(job["downloadRetry"].is_null()||attempts.iter().enumerate().any(|(index,a)|a["retryPermit"]==job["downloadRetry"]&&consumed(index,a)));
    if job["kind"]!="media"||job["purpose"]!="auto_media"||job["visualContractVersion"]!=2||job["status"]!="failed"
        ||attempt["status"]!="failed"||attempt["error"]!=body["expectedError"]
        ||!(known_failure||matches!(text(attempt,"error"),"source_download_failed"|"source_file_missing"|"download_timeout"))
        ||matches!(text(attempt,"error"),"source_download_failed_process_unknown"|"source_download_failed_wait_failed")
        ||attempts.iter().any(|a|a["status"]!="failed"||matches!(text(a,"error"),"source_download_failed_process_unknown"|"source_download_failed_wait_failed"))
        ||!permits_settled||!job["sourceImport"].is_null()||!(progress.is_null()||empty_shell){return Err(crate::conflict("Import requires terminal download failure without checkpoint or pending retry"));}
    if rows(d,"jobs").iter().any(|other|matches!(text(other,"kind"),"media"|"media_audio")&&(
        matches!(text(other,"status"),"running"|"unknown"|"dispatching")
        ||other["id"]!=job["id"]&&matches!(text(other,"status"),"queued"|"paused")&&(other["groupKey"]==job["groupKey"]||other["refId"]==post["id"])
        ||other["id"]!=job["id"]&&rows(other,"sourceAttempts").iter().any(|a|a["sourceKey"]==key&&a["sourceVersion"]==body["sourceVersion"]))) {
        return Err(crate::conflict("Media ownership is active, unresolved or belongs to another job"));
    }
    // The caller cannot supply a replacement locator. It must be an exact URL
    // already on this version of the post, with the same canonical source key.
    let url=text(body,"sourceUrl");
    let present=["sourceUrl","url"].iter().any(|k|post[*k]==url)||rows(post,"attachments").iter().any(|a|["sourceUrl","source_url","url"].iter().any(|k|a[*k]==url));
    let mut locator=post.clone();locator["attachments"]=json!([]);locator["sourceUrl"]=json!(url);
    if !present||crate::knowledge::media_source_key(&locator,account).as_deref()!=Some(&key){return Err(crate::conflict("Source locator does not match the retained original"));}
    let projection=json!({"account":account,"postKey":post["postKey"],"title":post["title"],"sourceUrl":url,"fallbackUrl":null});
    crate::media_processing::MediaSource::from_projection(&projection,account,text(post,"postKey")).map_err(|_|crate::bad("Invalid source projection"))?;
    if post["durationMs"].as_u64().is_some_and(|ms|ms.abs_diff(body["durationMs"].as_u64().unwrap())>1000){return Err(crate::conflict("Retained duration conflicts with source"));}
    Ok(Admission{job:job.clone(),post:post.clone(),epoch:crate::media_queue::material_epoch(d,post),projection})
}

fn reject_links(path:&Path)->Result<(),String>{
    let mut prefix=PathBuf::new();
    for component in path.components(){prefix.push(component);if matches!(component,Component::Prefix(_)){continue;}
        let meta=fs::symlink_metadata(&prefix).map_err(|_|"import_path_unavailable")?;
        if meta.file_type().is_symlink(){return Err("import_path_link_rejected".into());}
        #[cfg(windows)] {use std::os::windows::fs::MetadataExt;if meta.file_attributes()&0x400!=0{return Err("import_path_link_rejected".into());}}
    }Ok(())
}
/// Operators stage <artifactId>.media here; this function never creates ingress.
fn ingress(scratch:&Path,body:&Value)->Result<PathBuf,String>{
    if !scratch.is_absolute(){return Err("import_scratch_invalid".into());}
    let company=crate::media_fullframes::hash(&json!([body["account"],body["connectorBinding"]]));
    let root=scratch.join("retained-source-import").join(company);
    reject_links(&root)?;
    if !root.is_dir(){return Err("import_ingress_missing".into());}Ok(root)
}
struct Staged(PathBuf);
impl Drop for Staged {fn drop(&mut self){let _=fs::remove_file(&self.0);}}
fn copy_bounded(root:&Path,body:&Value)->Result<Staged,String>{
    // Recheck IDs here too: this helper is the only filesystem entry point.
    let artifact=text(body,"artifactId");
    if uuid::Uuid::parse_str(artifact).is_err()||artifact.len()!=36{return Err("import_artifact_id_invalid".into());}
    let bytes=body["bytes"].as_u64().filter(|n|*n>0&&*n<=MAX_BYTES).ok_or("import_size_invalid")?;
    if !sha(text(body,"sha256")){return Err("import_hash_invalid".into());}
    reject_links(root)?;
    let path=root.join(format!("{artifact}.media"));reject_links(&path)?;
    let mut options=OpenOptions::new();options.read(true);
    #[cfg(windows)] {use std::os::windows::fs::OpenOptionsExt;options.custom_flags(0x00200000).share_mode(1);}
    #[cfg(target_os="linux")] {use std::os::unix::fs::OpenOptionsExt;options.custom_flags(0x20000);}
    let mut input=options.open(&path).map_err(|_|"import_source_open_failed")?;
    let meta=input.metadata().map_err(|_|"import_source_metadata_failed")?;
    #[cfg(windows)] {use std::os::windows::fs::MetadataExt;if meta.file_attributes()&0x400!=0{return Err("import_path_link_rejected".into());}}
    #[cfg(unix)] {use std::os::unix::fs::MetadataExt;let after=fs::symlink_metadata(&path).map_err(|_|"import_source_changed")?;if after.file_type().is_symlink()||after.ino()!=meta.ino()||after.dev()!=meta.dev(){return Err("import_source_changed".into());}}
    if !meta.is_file()||meta.len()!=bytes{return Err("import_source_size_mismatch".into());}
    let output=root.join(format!(".verified-{}.media",uuid::Uuid::new_v4()));
    let mut options=OpenOptions::new();options.write(true).create_new(true);
    #[cfg(unix)] {use std::os::unix::fs::OpenOptionsExt;options.mode(0o600);}
    let mut target=options.open(&output).map_err(|_|"import_staging_failed")?;
    let staged=Staged(output);let mut hash=Sha256::new();let mut total=0u64;let mut buffer=[0u8;64*1024];
    loop {let n=input.read(&mut buffer).map_err(|_|"import_source_read_failed")?;if n==0{break;}
        total=total.checked_add(n as u64).ok_or("import_size_invalid")?;
        if total>bytes||total>MAX_BYTES{return Err("import_source_size_mismatch".into());}
        hash.update(&buffer[..n]);target.write_all(&buffer[..n]).map_err(|_|"import_staging_failed")?;
    }
    target.sync_all().map_err(|_|"import_staging_failed")?;
    if total!=bytes||format!("{:x}",hash.finalize())!=text(body,"sha256"){return Err("import_source_hash_mismatch".into());}
    Ok(staged)
}
fn commit(d:&mut Value,body:&Value,actor:&str,expected:&Admission,source:&ArtifactRef,probe:&Value,at:&str)->ApiResult<Value>{
    if let Some(receipt)=replay(d,body,actor)?{return Ok(receipt);}
    let current=admit(d,body,at)?;
    if current.job!=expected.job||current.post!=expected.post||current.epoch!=expected.epoch||current.projection!=expected.projection{return Err(crate::conflict("Source import state changed during verification"));}
    if source.sha256!=text(body,"sha256")||source.bytes!=body["bytes"].as_u64().unwrap()||probe["hasVideo"]!=true||probe["hasAudio"]!=true||probe["fullDecode"]!=true||probe["durationMs"]!=body["durationMs"]{return Err(crate::conflict("Source import proof mismatch"));}
    let receipt=json!({"id":body["receiptId"],"kind":"retained_original_import","jobId":body["jobId"],"authorizedBy":actor,"acceptedAt":at,"request":body,"source":source.to_json(),"probe":probe});
    let mut progress=crate::media_fullframes::initial(text(body,"account"),&body["connectorBinding"],&current.post,at);
    progress["source"]=source.to_json();progress["sourceIdentity"]=json!({"account":body["account"],"postKey":body["postKey"],"mediaSha256":source.sha256,"durationMs":body["durationMs"]});
    progress["sourceProjection"]=current.projection;progress["materialEpoch"]=json!(current.epoch);progress["phase"]=json!("inventory");
    let job=crate::row_mut(d,"jobs",text(body,"jobId"))?;
    let ordinal=rows(job,"sourceAttempts").len()+1;
    crate::list_mut(job,"sourceAttempts").push(json!({"id":crate::id(),"kind":"retained_original_import","importReceiptId":body["receiptId"],"postId":body["postId"],"postKey":body["postKey"],"sourceKey":body["sourceKey"],"sourceVersion":body["sourceVersion"],"channel":current.post["channel"],"status":"running","startedAt":at,"attemptNumber":ordinal}));
    job["sourceImport"]=receipt.clone();job["status"]=json!("queued");job["finishedAt"]=Value::Null;job["manualRequested"]=json!(true);job["fallbackAllowed"]=json!(false);
    job["result"]=json!({"visualProgress":progress,"mediaSchedulerTurn":job["result"]["mediaSchedulerTurn"]});
    job.as_object_mut().unwrap().remove("error");d["mediaQueue"]=Value::Null;
    crate::audit(d,"media.retained_source_imported",text(body,"receiptId"));Ok(receipt)
}

pub(crate) async fn import_retained_source(
    axum::extract::State(app):axum::extract::State<crate::App>,
    axum::Extension(actor):axum::Extension<crate::operator_auth::Actor>,
    axum::Json(body):axum::Json<Value>,
)->ApiResult<axum::Json<Value>>{
    if actor.role!="owner"{return Err(crate::bad("Retained source import requires owner"));}
    parse_request(&body,&crate::now())?;
    let _guard=crate::media_queue::source_import_guard()?;
    let snapshot=app.read().await?;
    if let Some(receipt)=replay(&snapshot,&body,&actor.id)?{return Ok(axum::Json(receipt));}
    let expected=admit(&snapshot,&body,&crate::now())?;drop(snapshot);
    let scratch=std::env::var_os("COMMUNITYHERO_MEDIA_SCRATCH_DIR").map(PathBuf::from).ok_or_else(||crate::bad("Import scratch is not configured"))?;
    let copied=body.clone();
    let staged=tokio::task::spawn_blocking(move||copy_bounded(&ingress(&scratch,&copied)?,&copied)).await.map_err(|_|crate::internal("Import copy stopped"))?.map_err(|e|crate::bad(&e))?;
    let probe=crate::media_processing::full::probe_retained_source(&staged.0,body["durationMs"].as_u64().unwrap()).await.map_err(|e|crate::bad(&e))?;
    let source=tokio::task::spawn_blocking(move||{
        let store=crate::media_fullframes::store()?;
        store.put_file(&staged.0).map_err(|_|"import_artifact_failed".to_owned())
    }).await.map_err(|_|crate::internal("Import persistence stopped"))?.map_err(|e|crate::bad(&e))?;
    // All filesystem/process work is complete. A rejected/crashed transaction
    // leaves at most an unreferenced immutable CAS object, never a partial job.
    let result=app.change(|d|commit(d,&body,&actor.id,&expected,&source,&probe,&crate::now())).await?;
    crate::media_queue::source_import_ready();Ok(axum::Json(result))
}

#[cfg(test)]
#[path="media_source_import_tests.rs"]
mod tests;
