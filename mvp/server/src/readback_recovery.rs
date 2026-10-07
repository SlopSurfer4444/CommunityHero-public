//! Bounded durable recovery through the existing readback path. Never execute.
use crate::*;
use std::sync::atomic::{AtomicI64,Ordering};

pub(crate) const MAX_AUTOMATIC_ATTEMPTS:u64=3;
const DELAYS:[i64;3]=[30,120,600];
// Cursor is a fairness hint only. The database owns admission and retry budget.
static CURSOR:AtomicI64=AtomicI64::new(-1);

fn timestamp(value:&Value)->Option<i64> {
    value.as_str().and_then(|s|chrono::DateTime::parse_from_rfc3339(s).ok()).map(|t|t.timestamp())
}

fn admitted_receipt(bytes:&[u8],account:&str,pid:u32,token:&str,manifest:&str)->bool {
    if bytes.len()>4096||token.len()!=32||!token.bytes().all(|b|b.is_ascii_hexdigit())
        ||manifest.len()!=64||!manifest.bytes().all(|b|b.is_ascii_hexdigit()) {return false;}
    let Ok(value)=serde_json::from_slice::<Value>(bytes) else{return false;};
    value["version"].as_u64()==Some(1)&&value["token"].as_str()==Some(token)
        &&value["account"].as_str()==Some(account)&&value["pid"].as_u64()==Some(u64::from(pid))
        &&value["manifestSha256"].as_str()==Some(manifest)&&timestamp(&value["admittedAtUtc"]).is_some()
}

async fn automatic_admitted(app:&App)->bool {
    let (Ok(path),Ok(token),Ok(manifest))=(std::env::var("COMMUNITYHERO_AUTOMATIC_READBACK_ADMISSION_FILE"),
        std::env::var("COMMUNITYHERO_AUTOMATIC_READBACK_ADMISSION_TOKEN"),std::env::var("COMMUNITYHERO_AUTOMATIC_READBACK_ADMISSION_MANIFEST_SHA256")) else{return false;};
    if !std::path::Path::new(&path).is_absolute(){return false;}
    let Ok(file)=tokio::fs::File::open(path).await else{return false;};
    let mut bytes=Vec::new();
    if file.take(4097).read_to_end(&mut bytes).await.is_err(){return false;}
    admitted_receipt(&bytes,app.account.key(),std::process::id(),&token,&manifest)
}

fn route_matches(metadata:&Value,op:&Value)->bool {
    let check=||->ApiResult<bool>{
        let account=accounts::Profile::from_workspace(metadata)?;
        Ok(operation_account(op)?==account.key()
            && op["target"]["connectorBinding"]==active_binding(metadata)?.to_json()
            && op["action"]["actionId"]==op["id"]
            && ["itemId","objectId","conversationKey"].iter().all(|key|
                op["action"][*key].is_string()&&op["action"][*key]==op["target"][*key]))
    };
    check().unwrap_or(false)
}

fn identity_evidence(op:&Value)->bool {
    if op["action"]["action"]!="reply_and_close" {return true;}
    let evidence=&op["action"]["readbackEvidence"];
    dispatch_evidence::reply_baseline(&evidence["baselineReplyIds"]).is_some()
        || evidence["expectedReplyId"].as_str().is_some_and(|id|!id.is_empty()&&id.len()<=200
            &&id.bytes().all(|b|b.is_ascii_alphanumeric()||b==b'_'||b==b'-'))
}

fn retryable_observation(op:&Value)->bool {
    let evidence=&op["evidence"];
    if evidence.is_null()||evidence.get("receipt").is_some() {return true;}
    if evidence["workerExit"]["requiresReadback"]==true&&evidence["workerExit"]["providerRetryAllowed"]==false
        &&matches!(evidence["workerExit"]["code"].as_str(),Some("worker_panicked"|"worker_cancelled")) {return true;}
    if let Some(error)=evidence["error"].as_str() {
        // Explicit fixed transport categories only; never retry scope/identity
        // rejection, cancellation, arbitrary error strings or invalid output.
        if evidence["phase"]=="readback"&&(error=="Adapter timed out; action outcome may be unknown"
            ||error=="Adapter runtime unavailable"
            ||["Adapter process failed (stage=exit;","Adapter process failed (stage=wait; kind=","Adapter process failed (stage=stdout; kind="]
                .iter().any(|prefix|error.starts_with(prefix))) {return true;}
        return ["ADAPTER_PROCESS_FAILED","ADAPTER_PROCESS_UNAVAILABLE","ADAPTER_TIMEOUT","PROVIDER_UNAVAILABLE","TRANSPORT_ERROR"]
            .iter().any(|code|error.starts_with(&format!("Adapter failed ({code};"))||error==format!("Adapter failed ({code})"));
    }
    let Some(rows)=evidence["results"].as_array().filter(|rows|rows.len()==1) else{return false;};
    let row=&rows[0];
    if evidence["account"].as_str()!=operation_account(op).ok()
        ||row["actionId"]!=op["action"]["actionId"]||row["itemId"]!=op["action"]["itemId"] {return false;}
    match row["code"].as_str() {
        Some("READBACK_NOT_VERIFIED")=>!["itemIdentityMatches","replySetValid","baselinePreserved"].iter()
            .any(|field|row["readbackObservation"][*field]==false),
        Some("TRANSPORT_ERROR")=>true,
        Some("HTTP_ERROR")=>row["httpStatus"].as_u64().is_some_and(|code|code==429||(500..=599).contains(&code)),
        _=>false,
    }
}

/// Pure admission used under the storage transaction. A claimed attempt remains
/// spent even if the process dies before the read; restarts cannot reset budget.
pub(crate) fn planned_job(metadata:&Value,op:&Value,active:bool,attempts:u64,last_at:Option<&str>,at:i64,requested_by:Option<&Value>)->Option<Value> {
    if active||op["status"]!="unknown"||!route_matches(metadata,op) {return None;}
    let automatic=requested_by.is_none();
    if automatic {
        if attempts>=MAX_AUTOMATIC_ATTEMPTS||!identity_evidence(op)||!retryable_observation(op) {return None;}
        let created=timestamp(&op["createdAt"])?;
        if at<created {return None;}
        let previous=if attempts==0 {timestamp(&op["updatedAt"]).unwrap_or(created).max(created)}else{timestamp(&json!(last_at?))?};
        let retry_after=op["evidence"]["results"][0]["retryAfterMs"].as_u64()
            .filter(|v|*v<=86_400_000).map_or(0,|ms|((ms+999)/1000) as i64);
        if at<previous+DELAYS[attempts as usize].max(retry_after) {return None;}
    }
    let mut job=json!({"id":id(),"kind":"reconcile","refId":op["id"],"status":"running",
        "createdAt":chrono::DateTime::from_timestamp(at,0)?.to_rfc3339(),"readbackOnly":true,
        "requestedBy":requested_by.cloned().unwrap_or_else(||json!({"id":"engine","kind":"automatic-readback"}))});
    if automatic {job["automaticReadback"]=json!({"version":1,"attempt":attempts+1,"maxAttempts":MAX_AUTOMATIC_ATTEMPTS});}
    Some(job)
}

async fn claim(app:&App,key:&str,requested_by:Option<Value>)->ApiResult<Option<(String,Value)>> {
    let token = app.lifecycle_admission_token(runtime_lifecycle::AdmissionClass::SocialDispatch).await?;
    if let Some(ctx)=conductor_authority::current_context(){conductor_authority::check_read(app,&ctx).await?;}
    let _guard=app.gate.acquire(writer_gate::Class::Standard).await;
    let claim=app.db.claim_readback_recovery_for_owner(key,chrono::Utc::now().timestamp(),requested_by.as_ref(), &token).await?;
    if claim.is_some(){app.bootstrap_cache.invalidate();let _=app.events.send(());}
    Ok(claim)
}

fn start(app:&App,job:String,op:Value) {
    let worker=app.clone();
    app.spawn(job,async move {let confirmed=reconcile_one(&worker,&op).await?;Ok(json!({"confirmed":confirmed,"readbackOnly":true}))});
}

/// Use this from the existing manual endpoint, so manual and automatic reads
/// share the same atomic exclusion and cannot race a succeeded->unknown update.
pub(crate) async fn manual(app:&App,key:&str,requested_by:Value)->ApiResult<String> {
    let (job,op)=claim(app,key,Some(requested_by)).await?
        .ok_or_else(||conflict("Operation is not eligible or readback/execution already running"))?;
    start(app,job.clone(),op);Ok(job)
}

pub(crate) async fn tick(app:&App)->ApiResult<()> {
    if std::env::var("COMMUNITYHERO_BACKGROUND_DISABLED").as_deref()==Ok("1")
        ||std::env::var("COMMUNITYHERO_AUTOMATIC_READBACK_DISABLED").as_deref()==Ok("1")
        ||!automatic_admitted(app).await {return Ok(());}
    let candidates=app.db.read_readback_candidates(CURSOR.load(Ordering::Relaxed)).await?;
    if candidates.is_empty(){CURSOR.store(-1,Ordering::Relaxed);return Ok(());}
    for (ordinal,key) in candidates {
        CURSOR.store(ordinal,Ordering::Relaxed);
        if !automatic_admitted(app).await {return Ok(());}
        if let Some((job,op))=claim(app,&key,None).await? {start(app,job,op);break;}
    }
    Ok(())
}

#[cfg(test)]
#[path="readback_recovery_tests.rs"]
pub(crate) mod tests;
