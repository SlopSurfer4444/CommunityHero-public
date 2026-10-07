//! Root-configured recovery admission. HTTP callers request inspection/admission;
//! they cannot provide ready flags, grant namespaces or bootstrap proof JSON.
//! Protected inspection is read-only and occurs outside M/SQL. OAuth remains in
//! the protected connector registry; native receives a bound safe projection.
use crate::*;
use axum::{Extension,body::Bytes};
use sha2::{Digest,Sha256};

const CASE_PATH_ENV:&str="COMMUNITYHERO_CONNECTION_ADMISSION_CASE_PATH";
const CASE_HASH_ENV:&str="COMMUNITYHERO_CONNECTION_ADMISSION_CASE_SHA256";
const MAX_CASE_BYTES:u64=external_reconciliation::MAX_BYTES as u64+65_536;
const MAX_PROJECTION_AGE:std::time::Duration=std::time::Duration::from_secs(30);
#[path="connection_continuation.rs"]
mod continuation;
type ClosingWorkers=std::sync::Mutex<std::collections::HashSet<String>>;
static CLOSING_WORKERS:std::sync::OnceLock<ClosingWorkers>=std::sync::OnceLock::new();
struct ClosingWorker(String);
impl Drop for ClosingWorker {
    fn drop(&mut self){CLOSING_WORKERS.get_or_init(Default::default).lock().unwrap_or_else(|error|error.into_inner()).remove(&self.0);}
}
/// Called only for a positively classified, request-correlated native AUTH
/// observation. Never parse an ApiError string to invoke this hook. Immediate
/// local closure precedes spawn; the old dispatch guard may then be released
/// while this ONE company task waits for durable cohort cessation.
pub(crate) fn observe_provider_auth_failure(app:&App) {
    dispatch_authority::hold_transport_failure(app);
    let key=json!([app.data.to_string_lossy(),app.account.key()]).to_string();
    if !CLOSING_WORKERS.get_or_init(Default::default).lock().unwrap_or_else(|error|error.into_inner()).insert(key.clone()){return;}
    let worker=ClosingWorker(key);
    let task=runtime_lifecycle_app::TaskCount::begin(app.lifecycle_task_count.clone());
    let app=app.clone();
    tokio::spawn(async move {
        let _worker=worker;let _task=task;
        let deadline=tokio::time::Instant::now()+std::time::Duration::from_millis(connection_gate::MAX_DRAIN_MS);
        let _=tokio::time::timeout_at(deadline,block_protected_unavailable(&app)).await;
        // Any error/timeout retains the local hold and original durable permit.
        // This helper waits for no native task count and cannot self-block drain.
    });
}

fn unavailable()->ApiError{conflict("Verified root recovery case or protected availability is unavailable")} 
fn hash(value:&Value)->bool{value.as_str().is_some_and(|s|s.len()==64&&s.bytes().all(|b|b.is_ascii_digit()||matches!(b,b'a'..=b'f')))}
fn exact(value:&Value,fields:&[&str])->bool{value.as_object().is_some_and(|o|o.len()==fields.len()&&fields.iter().all(|field|o.contains_key(*field)))}
fn canonical_json(value:&Value)->String {
    match value {
        Value::Object(fields)=>{let mut keys:Vec<_>=fields.keys().collect();keys.sort();
            format!("{{{}}}",keys.into_iter().map(|key|format!("{}:{}",serde_json::to_string(key).unwrap(),canonical_json(&fields[key]))).collect::<Vec<_>>().join(","))},
        Value::Array(values)=>format!("[{}]",values.iter().map(canonical_json).collect::<Vec<_>>().join(",")),
        _=>value.to_string(),
    }
}
fn owner_value(owner:&runtime_lifecycle::OwnerToken)->Value{json!({"account":owner.account,"runtimeId":owner.runtime_id,
    "releaseSha256":owner.release_sha256,"epoch":owner.epoch})}

/// Created only from exact root-configured, hash-pinned case bytes. This type is
/// never deserialized by an HTTP handler. Whole case artifact may contain other
/// clean-start receipts; its compact bootstrap DTO has exactly twelve fields.
pub(crate) struct VerifiedBootstrapCase {
    sha256:String,path:PathBuf,bootstrap:Value,owner:runtime_lifecycle::OwnerToken,
}
fn parse_case(value:Value,sha256:String,path:PathBuf)->ApiResult<VerifiedBootstrapCase>{
    if value["kind"]!="communityhero-clean-start-case.v1"||!hash(&json!(sha256)){return Err(unavailable());}
    let bootstrap=&value["bootstrap"];
    if !exact(bootstrap,&["version","companyId","connectionBinding","expectedStorageGeneration","owner",
        "lifecycleReceiptSha256","predecessorContainmentReceiptSha256","archiveRestoreReceiptSha256",
        "mutationPermitCap","drainDeadlineMs","archiveFence","expectedProtectedGeneration"])
        ||bootstrap["version"]!=1||!hash(&bootstrap["lifecycleReceiptSha256"])
        ||!hash(&bootstrap["predecessorContainmentReceiptSha256"])||!hash(&bootstrap["archiveRestoreReceiptSha256"])
        ||bootstrap["mutationPermitCap"].as_u64().is_none_or(|n|n==0||n>connection_gate::MAX_MUTATION_PERMITS as u64)
        ||bootstrap["drainDeadlineMs"].as_u64().is_none_or(|n|n==0||n>connection_gate::MAX_DRAIN_MS)
        ||bootstrap["expectedProtectedGeneration"].as_u64().is_none_or(|n|n==0){return Err(unavailable());}
    let generation=bootstrap["expectedStorageGeneration"].as_str().ok_or_else(unavailable)?;
    let parsed=uuid::Uuid::parse_str(generation).map_err(|_|unavailable())?;
    if parsed.get_version_num()!=4||parsed.to_string()!=generation{return Err(unavailable());}
    let owner=runtime_lifecycle::parse_token(&bootstrap["owner"]).map_err(|_|unavailable())?;
    ConnectorBinding::from_json(&bootstrap["connectionBinding"]).map_err(|_|unavailable())?;
    Ok(VerifiedBootstrapCase{sha256,path,bootstrap:bootstrap.clone(),owner})
}
async fn case_bytes(path:&std::path::Path,expected:&str)->ApiResult<Vec<u8>>{
    if !path.is_absolute()||!hash(&json!(expected)){return Err(unavailable());}
    let metadata=tokio::fs::symlink_metadata(path).await.map_err(|_|unavailable())?;
    if !metadata.is_file()||metadata.file_type().is_symlink()||metadata.len()==0||metadata.len()>MAX_CASE_BYTES{return Err(unavailable());}
    let bytes=tokio::fs::read(path).await.map_err(|_|unavailable())?;
    if bytes.len() as u64>MAX_CASE_BYTES||format!("{:x}",Sha256::digest(&bytes))!=expected{return Err(unavailable());}
    Ok(bytes)
}
async fn load_configured_case()->ApiResult<VerifiedBootstrapCase>{
    let path=PathBuf::from(std::env::var(CASE_PATH_ENV).map_err(|_|unavailable())?);
    let sha256=std::env::var(CASE_HASH_ENV).map_err(|_|unavailable())?;
    let bytes=case_bytes(&path,&sha256).await?;
    let value=serde_json::from_slice(&bytes).map_err(|_|unavailable())?;
    parse_case(value,sha256,path)
}
fn case_scope(d:&Value,case:&VerifiedBootstrapCase)->ApiResult<()> {
    if case.bootstrap["companyId"]!=accounts::Profile::from_workspace(d)?.key()
        ||case.bootstrap["connectionBinding"]!=active_binding(d)?.to_json()
        ||case.bootstrap["expectedStorageGeneration"]!=working_generation::current(d)?
        ||owner_value(&case.owner)!=d["runtimeLifecycle"]["owner"]{return Err(unavailable());}
    runtime_lifecycle::require_admission(d,&case.owner,runtime_lifecycle::AdmissionClass::SocialDispatch)?;
    external_reconciliation::validate_value(d,&case.bootstrap["archiveFence"])?;
    Ok(())
}

/// Trusted transport provenance is supplied by read_protected(), not by JSON.
pub(crate) struct ProtectedAvailabilityRead {
    observation:connection_gate::AvailabilityObservation,observed:std::time::Instant,
}
fn parse_protected(value:Value)->ApiResult<connection_gate::AvailabilityObservation>{
    let allowed=["version","account","connectionBinding","state","generation","phase","reason","canonicalCaseId",
        "grantSpent","candidatePresent","scopeVerified","parentCount","sendGateOpen","retryOriginalOperation","socialRequests","receiptSha256"];
    if value.to_string().len()>8192||value.as_object().is_none_or(|fields|fields.keys().any(|key|!allowed.contains(&key.as_str())))
        ||value["version"]!=1||value["account"].as_str().is_none_or(str::is_empty)
        ||!matches!(value["state"].as_str(),Some("ready"|"blocked"|"recovering"|"needs_owner"))
        ||value["generation"].as_u64().is_none_or(|n|n>9_007_199_254_740_991)
        ||!matches!(value["phase"].as_str(),Some("registered"|"claimed"|"request_armed"|"unresolved"|"deposit_unconfirmed"
            |"candidate_deposited"|"returned_missing_candidate"|"verify_pending"|"scope_held"|"verified"|"cas_pending"
            |"cas_baseline"|"cas_conflict"|"committed"|"needs_owner"|"unavailable"))
        ||value["reason"].as_str().is_none_or(|s|s.is_empty()||s.len()>80||!s.bytes().all(|b|b.is_ascii_lowercase()||b==b'_'))
        ||!value["grantSpent"].is_boolean()||!value["candidatePresent"].is_boolean()||!value["scopeVerified"].is_boolean()
        ||value["parentCount"]!=2||value["sendGateOpen"]!=false||value["retryOriginalOperation"]!=false||value["socialRequests"]!=0
        ||value.get("canonicalCaseId").is_some_and(|id|id.as_str().is_none_or(|s|uuid::Uuid::parse_str(s).is_err()))
        ||!hash(&value["receiptSha256"]){return Err(unavailable());}
    ConnectorBinding::from_json(&value["connectionBinding"]).map_err(|_|unavailable())?;
    if value["state"]=="ready"&&(value["phase"]!="committed"||value["grantSpent"]!=true
        ||value["candidatePresent"]!=true||value["scopeVerified"]!=true){return Err(unavailable());}
    let receipt_sha256=value["receiptSha256"].as_str().unwrap().to_owned();
    let mut projection=value.clone();projection.as_object_mut().unwrap().remove("receiptSha256");
    // Explicit recursive ordering stays stable even if a later dependency
    // enables serde_json preserve_order. This DTO has no floating numbers.
    if format!("{:x}",Sha256::digest(canonical_json(&projection).as_bytes()))!=receipt_sha256{return Err(unavailable());}
    Ok(connection_gate::AvailabilityObservation{projection:value,protected_receipt_sha256:receipt_sha256})
}
async fn read_protected(app:&App)->ApiResult<ProtectedAvailabilityRead>{
    // Root bridge command opens a SERVER-configured hash-pinned issued case and
    // same protected registry. Caller args cannot choose path/grant/ready state.
    let value=app.bridge("owner_session_inspect",json!({"account":app.account.display()})).await?;
    let observation=parse_protected(value)?;
    if observation.projection["account"]!=app.account.key(){return Err(unavailable());}
    Ok(ProtectedAvailabilityRead{observation,observed:std::time::Instant::now()})
}
fn fresh(read:&ProtectedAvailabilityRead)->ApiResult<()> {
    if read.observed.elapsed()>MAX_PROJECTION_AGE{return Err(unavailable());}Ok(())
}
/// A failed trusted protected inspection revokes readiness without inventing
/// an auth generation or reclassifying any original external attempt. Root can
/// also call this for a positively classified shared auth barrier AFTER the
/// originating transport/conductor guard has been released.
pub(crate) async fn block_protected_unavailable(app:&App)->ApiResult<()> {
    dispatch_authority::hold_transport_failure(app);
    let deadline=tokio::time::Instant::now()+std::time::Duration::from_millis(connection_gate::MAX_DRAIN_MS);
    let metadata=tokio::time::timeout_at(deadline,app.db.read_metadata()).await.map_err(|_|unavailable())??;
    if metadata.get(connection_gate::FIELD).is_none(){return Ok(());}
    let existing=&metadata[connection_gate::FIELD]["closingIntent"];
    let intent=if existing.is_object(){existing.clone()}else{json!({"id":format!("protected-read-unavailable:{}",uuid::Uuid::new_v4()),"reason":"protected_read_unavailable"})};
    connection_gate::close_and_drain(app,required(&intent,"id")?,required(&intent,"reason")?).await?;
    Ok(())
}
async fn read_protected_or_block(app:&App)->ApiResult<ProtectedAvailabilityRead> {
    match read_protected(app).await {
        Ok(read)=>Ok(read),
        Err(error)=>{let _=block_protected_unavailable(app).await;Err(error)}
    }
}

pub(crate) async fn refresh_from_protected(app:&App)->ApiResult<Value>{
    let read=read_protected_or_block(app).await?;
    let intent={let _company=connection_gate::lock(app).await;
        let result=app.change_connection_gate(connection_gate::Scope::Control,|d|{
            fresh(&read)?;connection_gate::observe_availability(d,&read.observation)?;
            Ok(if d[connection_gate::FIELD]["state"]=="closing"{Some(d[connection_gate::FIELD]["closingIntent"].clone())}else{None})
        }).await;
        drop(_company);
        match result{Ok(intent)=>intent,Err(error)=>{let _=block_protected_unavailable(app).await;return Err(error);}}
    };
    if let Some(intent)=intent {
        connection_gate::close_and_drain(app,required(&intent,"id")?,required(&intent,"reason")?).await?;
    }
    Ok(json!({"status":"recorded","availability":read.observation.projection,"sendGateReopened":false}))
}

/// Registration happens before spawn. The HTTP receiver owns only its response,
/// not the committed admission or its post-commit native continuation.
pub(crate) async fn admit_configured_case(app:&App)->ApiResult<Value>{
    let owned=app.clone();
    continuation::spawn_owned(app,async move{admit_owned(&owned).await})?
        .await.map_err(|_|unavailable())?
}
async fn admit_owned(app:&App)->ApiResult<Value>{
    let case=load_configured_case().await?;
    // A repeated proof is inspected before requesting close: ordinary active
    // dispatch and its original lease must survive an exact fresh repeat.
    let initial=read_protected_or_block(app).await?;
    case_bytes(&case.path,&case.sha256).await?;
    let replay={let company=connection_gate::lock(app).await;
        let result=app.change_connection_gate(connection_gate::Scope::Control,|d|{
            continuation::replay(d,&case,&initial,dispatch_authority::require_unheld(app).is_ok())
        }).await;
        drop(company);result?};
    if let Some(result)=replay {
        // The durable marker is the only reason to reconcile a repeated proof.
        // A normal active child or a run without a marker receives no wake.
        if conductor::has_deferred_connection_work(&app.read().await?)? {
            conductor::wake_deferred(app).await?;
        }
        return Ok(result);
    }
    let intent={let _company=connection_gate::lock(app).await;
        app.change_connection_gate(connection_gate::Scope::Control,|d|{
            case_scope(d,&case)?;
            if d.get(connection_gate::FIELD).is_none(){connection_gate::initialize_closed(d,
                required(&case.bootstrap,"predecessorContainmentReceiptSha256")?,case.bootstrap["mutationPermitCap"].as_u64().unwrap() as usize,
                case.bootstrap["drainDeadlineMs"].as_u64().unwrap(),true)?;}
            if d[connection_gate::FIELD]["mutationPermitCap"]!=case.bootstrap["mutationPermitCap"]
                ||d[connection_gate::FIELD]["drainDeadlineMs"]!=case.bootstrap["drainDeadlineMs"]
                ||d[connection_gate::FIELD]["archiveFenceRequired"]!=true{return Err(unavailable());}
            if d[connection_gate::FIELD]["state"]=="closing"||d[connection_gate::FIELD]["state"]=="blocked"
                &&d[connection_gate::FIELD]["closingIntent"].is_object(){return Ok(d[connection_gate::FIELD]["closingIntent"].clone());}
            connection_gate::request_close(d,&format!("connection-case:{}",&case.sha256[..40]),"owner_recovery")
        }).await?};
    // No M/SQL/conductor lock while draining or reading the protected store.
    connection_gate::close_and_drain(app,required(&intent,"id")?,required(&intent,"reason")?).await?;
    let hold=dispatch_authority::capture_recovery_hold(app)?;
    let read=read_protected_or_block(app).await?;
    case_bytes(&case.path,&case.sha256).await?;
    let company=connection_gate::lock(app).await;
    let result=app.change_connection_gate(connection_gate::Scope::Control,|d|{
        case_scope(d,&case)?;fresh(&read)?;
        let epoch=d[connection_gate::FIELD]["gateEpoch"].as_u64().ok_or_else(unavailable)?;
        let archive=external_reconciliation::install(d,&case.bootstrap["archiveFence"],epoch)?;
        let availability=connection_gate::observe_availability(d,&read.observation)?;
        if availability["state"]!="ready"||availability["generation"]!=case.bootstrap["expectedProtectedGeneration"] {
            return Ok(json!({"status":"needs_owner","availability":availability,"archive":archive,"sendGateReopened":false}));
        }
        connection_gate::reopen(d,&json!({"expectedGateEpoch":epoch,"owner":owner_value(&case.owner),
            "connectionBinding":case.bootstrap["connectionBinding"],"availability":availability,
            "lifecycleReceiptSha256":case.bootstrap["lifecycleReceiptSha256"],"caseSha256":case.sha256,
            "archiveRestoreReceiptSha256":case.bootstrap["archiveRestoreReceiptSha256"]}))?;
        let proof=continuation::persist(d,&case,&read)?;
        Ok(json!({"status":"admitted","gate":d[connection_gate::FIELD],"archive":archive,"caseSha256":case.sha256,
            "admittedContinuationProof":proof,"replayed":false,"sendGateReopened":true}))
    }).await;
    drop(company);
    let result=result?;
    if result["status"]=="admitted" {
        dispatch_authority::clear_after_recovery_admission(app,&hold)?;
        conductor::wake_deferred(app).await?;
    }
    Ok(result)
}
fn owner_request(actor:&operator_auth::Actor,body:&Bytes)->ApiResult<()> {
    if actor.role!="owner"{return Err(ApiError(StatusCode::FORBIDDEN,"Connection recovery requires the company owner".into()));}
    if !body.is_empty(){let value:Value=serde_json::from_slice(body).map_err(|_|bad("Connection recovery accepts an empty object"))?;
        if !exact(&value,&[]){return Err(bad("Connection recovery accepts no authority or ready fields"));}}
    Ok(())
}
pub(crate) async fn refresh(State(app):State<App>,Extension(actor):Extension<operator_auth::Actor>,body:Bytes)->ApiResult<Json<Value>>{
    owner_request(&actor,&body)?;refresh_from_protected(&app).await.map(Json)
}
pub(crate) async fn admit(State(app):State<App>,Extension(actor):Extension<operator_auth::Actor>,body:Bytes)->ApiResult<Json<Value>>{
    owner_request(&actor,&body)?;admit_configured_case(&app).await.map(Json)
}

#[cfg(test)]
#[path="connection_recovery_tests.rs"]
mod tests;
#[cfg(test)]
#[path="connection_continuation_tests.rs"]
mod continuation_tests;
