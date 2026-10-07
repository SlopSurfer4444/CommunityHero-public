//! Durable native drain. All reducers run inside the workspace writer lock.
//! No reducer executes a provider call, terminates a process, or retries work.
use crate::{ApiResult, conflict};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct OwnerToken {
    pub account: String,
    pub runtime_id: String,
    pub release_sha256: String,
    pub epoch: u64,
}
/// Fixed at process startup from admitted native release/account identity.
/// Never refresh this identity from mutable workspace metadata.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RuntimeIdentity {
    pub account: String,
    pub runtime_id: String,
    pub release_sha256: String,
}
#[derive(Clone, Debug)]
pub(crate) struct AdmittedTarget {
    pub release_sha256: String,
    pub media_analysis_generation: u64,
    pub asr_disabled: bool,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AdmissionClass { SourceRead, Media, Preparation, SocialDispatch }
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Phase { Running, Draining, Drained, Stopped }
impl Phase {
    fn name(self) -> &'static str { match self {
        Self::Running=>"running",Self::Draining=>"draining",Self::Drained=>"drained",Self::Stopped=>"stopped"
    }}
}

// Created from actual native observers by the ROOT-owned runtime coordinator,
// NEVER deserialized from HTTP JSON or a caller-provided `quiet: true` flag.
#[derive(Debug)]
pub(crate) struct SettledNative {
    pub owner: OwnerToken,
    pub application_tasks: usize,
    pub provider_queued: usize,
    pub provider_dispatched: usize,
    pub provider_contained: bool,
    pub credential_writers: usize,
    pub unresolved_effects: usize,
}
impl SettledNative {
    fn is_settled(&self) -> bool {
        self.application_tasks == 0 && self.provider_queued == 0 && self.provider_dispatched == 0
            && self.provider_contained && self.credential_writers == 0 && self.unresolved_effects == 0
    }
}

fn err() -> crate::ApiError { conflict("Runtime lifecycle owner, epoch or phase mismatch; admission blocked") }
fn key(s: &str) -> bool { !s.is_empty() && s.len()<=80 && s.bytes().all(|c|c.is_ascii_alphanumeric()||matches!(c,b'-'|b'_')) }
fn account(s: &str) -> bool { matches!(s, "LikeAvto" | "BAW Russia") }
fn hash(s: &str) -> bool { s.len()==64 && s.bytes().all(|c|c.is_ascii_digit()||matches!(c,b'a'..=b'f')) }
fn exact(v:&Value, fields:&[&str])->bool {
    v.as_object().is_some_and(|o|o.len()==fields.len()&&fields.iter().all(|k|o.contains_key(*k)))
}
fn token_value(t:&OwnerToken)->Value { json!({"account":t.account,"runtimeId":t.runtime_id,"releaseSha256":t.release_sha256,"epoch":t.epoch}) }
pub(crate) fn parse_token(v:&Value)->ApiResult<OwnerToken> {
    if !exact(v,&["account","runtimeId","releaseSha256","epoch"]) {return Err(err());}
    let t=OwnerToken { account:v["account"].as_str().ok_or_else(err)?.into(),
        runtime_id:v["runtimeId"].as_str().ok_or_else(err)?.into(),
        release_sha256:v["releaseSha256"].as_str().ok_or_else(err)?.into(),epoch:v["epoch"].as_u64().ok_or_else(err)? };
    if !account(&t.account)||!key(&t.runtime_id)||!hash(&t.release_sha256)||t.epoch==0 {return Err(err());} Ok(t)
}
fn current(d:&Value)->ApiResult<(OwnerToken,Phase)> {
    let v=&d["runtimeLifecycle"];
    if !exact(v,&["schemaVersion","owner","phase","history","target","transfer","mediaAnalysisGeneration","queuedBacklog"])
        ||v["schemaVersion"]!=1||v["mediaAnalysisGeneration"]!=1||!v["history"].is_array() {return Err(err());}
    let t=parse_token(&v["owner"])?;
    if d["account"].as_str()!=Some(&t.account) {return Err(err());}
    let p=match v["phase"].as_str() {Some("running")=>Phase::Running,Some("draining")=>Phase::Draining,
        Some("drained")=>Phase::Drained,Some("stopped")=>Phase::Stopped,_=>return Err(err())};
    if p==Phase::Running && (!v["target"].is_null()||!v["transfer"].is_null()||!v["queuedBacklog"].is_null()) {return Err(err());}
    if p!=Phase::Running && (!exact(&v["target"],&["releaseSha256","attemptId","asrDisabled","mediaAnalysisGeneration"])
        ||!v["target"]["releaseSha256"].as_str().is_some_and(hash)
        ||!v["target"]["attemptId"].as_str().is_some_and(key)||!v["target"]["asrDisabled"].is_boolean()
        ||!v["target"]["mediaAnalysisGeneration"].is_u64()
        ||(v["target"]["mediaAnalysisGeneration"]==0&&v["target"]["asrDisabled"]!=true)) {return Err(err());}
    if matches!(p,Phase::Drained|Phase::Stopped) && (!exact(&v["transfer"],&["ledgerSha256","owner","target","nativeSettled","queuedBacklog"])
        ||!v["transfer"]["ledgerSha256"].as_str().is_some_and(hash)
        ||v["transfer"]["owner"]!=token_value(&t)||v["transfer"]["target"]!=v["target"]||v["transfer"]["nativeSettled"]!=true||v["transfer"]["queuedBacklog"]!=v["queuedBacklog"]) {return Err(err());}
    if p==Phase::Draining && !v["transfer"].is_null() {return Err(err());}
    Ok((t,p))
}
fn append(d:&mut Value,kind:&str,owner:&OwnerToken,evidence:Value) {
    d["runtimeLifecycle"]["history"].as_array_mut().unwrap().push(json!({"kind":kind,"owner":token_value(owner),"evidence":evidence}));
}
pub(crate) fn admission_token(d:&Value,_class:AdmissionClass)->ApiResult<OwnerToken> {
    let (t,p)=current(d)?;if p!=Phase::Running {return Err(err());} Ok(t)
}
pub(crate) fn bound_admission_token(d:&Value,runtime:&RuntimeIdentity,class:AdmissionClass)->ApiResult<OwnerToken> {
    let token=admission_token(d,class)?;
    if token.account!=runtime.account||token.runtime_id!=runtime.runtime_id||token.release_sha256!=runtime.release_sha256 {return Err(err());}
    Ok(token)
}
pub(crate) fn current_owner(d:&Value,runtime:&RuntimeIdentity)->ApiResult<OwnerToken> {
    let (t,_)=current(d)?;require_runtime_owner(&t,runtime)?;Ok(t)
}
pub(crate) fn require_runtime_owner(t:&OwnerToken,runtime:&RuntimeIdentity)->ApiResult<()> {
    if t.account!=runtime.account||t.runtime_id!=runtime.runtime_id||t.release_sha256!=runtime.release_sha256 {return Err(err());}Ok(())
}
pub(crate) fn require_admission(d:&Value,t:&OwnerToken,class:AdmissionClass)->ApiResult<()> {
    if &admission_token(d,class)?!=t {return Err(err());} Ok(())
}
pub(crate) fn status(d:&Value)->ApiResult<Value> { current(d)?;Ok(d["runtimeLifecycle"].clone()) }

/// Ordered complete protected history digest; never accepts a filtered SQL view.
/// Full rows preserve paused checkpoints, paid receipts, UNKNOWN payloads and audit.
pub(crate) fn ledger_digest(d:&Value)->ApiResult<String> {
    let mut material=d.as_object().ok_or_else(err)?.clone();
    material.remove("runtimeLifecycle");
    for name in ["account","connectorBinding","jobs","operations","approvals","audit","materials","knowledge_entries","knowledge_versions"] {
        let v=d.get(name).ok_or_else(err)?;
        if name=="account" {if !v.as_str().is_some_and(account) {return Err(err());}}
        else if name=="connectorBinding" {if !v.is_object() {return Err(err());}}
        else if !v.is_array() {return Err(err());}
    }
    let bytes=serde_json::to_vec(&Value::Object(material)).map_err(|_|err())?;
    Ok(format!("{:x}",Sha256::digest(bytes)))
}

/// One-time bootstrap is an internal, explicit ROOT integration input. It does
/// not claim that the predecessor exposed native drain, nor observe cessation.
pub(crate) fn initialize(d:&mut Value,t:OwnerToken,bootstrap_receipt_sha256:&str,expected_ledger:&str)->ApiResult<()> {
    if d.get("runtimeLifecycle").is_some()||parse_token(&token_value(&t))?!=t||t.epoch!=1
        ||d["account"].as_str()!=Some(&t.account)||!hash(bootstrap_receipt_sha256)||ledger_digest(d)?!=expected_ledger {return Err(err());}
    d["runtimeLifecycle"]=json!({"schemaVersion":1,"owner":token_value(&t),"phase":"running","history":[],
        "target":null,"transfer":null,"mediaAnalysisGeneration":1,"queuedBacklog":null});
    append(d,"bootstrap",&t,json!({"receiptSha256":bootstrap_receipt_sha256,"ledgerSha256":expected_ledger}));Ok(())
}
#[cfg(test)]
pub(crate) fn begin_drain(d:&mut Value,expected:&OwnerToken,target_release:&str,attempt_id:&str,asr_disabled:bool)->ApiResult<OwnerToken> {
    begin_drain_for_release(d,expected,&AdmittedTarget{release_sha256:target_release.into(),media_analysis_generation:1,asr_disabled},attempt_id)
}
pub(crate) fn begin_drain_for_release(d:&mut Value,expected:&OwnerToken,target:&AdmittedTarget,attempt_id:&str)->ApiResult<OwnerToken> {
    let (mut t,p)=current(d)?;
    let target_release=&target.release_sha256;
    if &t!=expected||p!=Phase::Running||!hash(target_release)||!key(attempt_id)
        ||(target.media_analysis_generation==0&&!target.asr_disabled) {return Err(err());}
    t.epoch=t.epoch.checked_add(1).ok_or_else(err)?;
    d["runtimeLifecycle"]["owner"]=token_value(&t);
    d["runtimeLifecycle"]["phase"]=json!(Phase::Draining.name());
    d["runtimeLifecycle"]["target"]=json!({"releaseSha256":target_release,"attemptId":attempt_id,"asrDisabled":target.asr_disabled,"mediaAnalysisGeneration":target.media_analysis_generation});
    d["runtimeLifecycle"]["queuedBacklog"]=crate::runtime_lifecycle_backlog::capture(d,&t)?;
    let queued_backlog=d["runtimeLifecycle"]["queuedBacklog"].clone();
    append(d,"drain-begun",&t,json!({"targetReleaseSha256":target_release,"attemptId":attempt_id,
        "queuedBacklog":queued_backlog}));Ok(t)
}
pub(crate) fn mark_drained(d:&mut Value,expected:&OwnerToken,native:&SettledNative)->ApiResult<Value> {
    let (t,p)=current(d)?;
    if &t!=expected||p!=Phase::Draining||native.owner!=t||!native.is_settled() {return Err(err());}
    crate::runtime_lifecycle_backlog::validate(d)?;
    for j in d["jobs"].as_array().ok_or_else(err)? {
        if j["status"]=="running" || (j["status"]=="queued"
            && !crate::runtime_lifecycle_backlog::retained_queued(d,j)?) {
            return Err(conflict("Native drain still has durable active work"));
        }
    }
    for op in d["operations"].as_array().ok_or_else(err)? {
        if op["status"]=="dispatching" {return Err(conflict("Native drain still has dispatching operations"));}
    }
    let transfer=json!({"ledgerSha256":ledger_digest(d)?,"owner":token_value(&t),"target":d["runtimeLifecycle"]["target"],"nativeSettled":true,"queuedBacklog":d["runtimeLifecycle"]["queuedBacklog"]});
    d["runtimeLifecycle"]["transfer"]=transfer.clone();d["runtimeLifecycle"]["phase"]=json!(Phase::Drained.name());
    append(d,"drained",&t,transfer.clone());Ok(transfer)
}
/// Persist before external controller cessation. This is a release checkpoint,
/// NOT a receipt claiming that the server process is absent.
pub(crate) fn commit_stop_checkpoint(d:&mut Value,expected:&OwnerToken,transfer:&Value)->ApiResult<()> {
    let (t,p)=current(d)?;
    if &t!=expected||p!=Phase::Drained||d["runtimeLifecycle"]["transfer"]!=*transfer
        ||ledger_digest(d)?!=transfer["ledgerSha256"].as_str().ok_or_else(err)? {return Err(err());}
    crate::runtime_lifecycle_backlog::validate(d)?;
    d["runtimeLifecycle"]["phase"]=json!(Phase::Stopped.name());append(d,"stop-checkpoint",&t,transfer.clone());Ok(())
}
/// The actual startup controller independently verifies predecessor process/
/// backend absence and the admitted full release closure before invoking this.
pub(crate) fn accept_successor(d:&mut Value,previous:&OwnerToken,transfer:&Value,new_runtime_id:&str,
    installed_release:&str,native_absence_receipt:&str,asr_disabled:bool)->ApiResult<OwnerToken> {
    let (mut t,p)=current(d)?;
    if &t!=previous||p!=Phase::Stopped||d["runtimeLifecycle"]["transfer"]!=*transfer
        ||ledger_digest(d)?!=transfer["ledgerSha256"].as_str().ok_or_else(err)?||!key(new_runtime_id)||new_runtime_id==t.runtime_id
        ||!hash(installed_release)||installed_release!=d["runtimeLifecycle"]["target"]["releaseSha256"].as_str().ok_or_else(err)?
        ||!hash(native_absence_receipt)||asr_disabled!=d["runtimeLifecycle"]["target"]["asrDisabled"].as_bool().ok_or_else(err)? {return Err(err());}
    crate::runtime_lifecycle_backlog::validate(d)?;
    let queued_backlog=d["runtimeLifecycle"]["queuedBacklog"].clone();
    t.runtime_id=new_runtime_id.into();t.release_sha256=installed_release.into();t.epoch=t.epoch.checked_add(1).ok_or_else(err)?;
    append(d,"successor-accepted",&t,json!({"transfer":transfer,"nativeAbsenceReceiptSha256":native_absence_receipt,
        "queuedBacklog":queued_backlog}));
    d["runtimeLifecycle"]["owner"]=token_value(&t);d["runtimeLifecycle"]["phase"]=json!(Phase::Running.name());
    d["runtimeLifecycle"]["target"]=Value::Null;d["runtimeLifecycle"]["transfer"]=Value::Null;d["runtimeLifecycle"]["queuedBacklog"]=Value::Null;Ok(t)
}
/// Cancel maintenance on the same native owner only AFTER its provider workers
/// have retired. Older epoch preflights remain invalid after resumption.
pub(crate) fn resume_same_owner(d:&mut Value,expected:&OwnerToken,native:&SettledNative)->ApiResult<OwnerToken> {
    let (mut t,p)=current(d)?;
    if &t!=expected||!matches!(p,Phase::Draining|Phase::Drained)||native.owner!=t||!native.is_settled() {return Err(err());}
    if p==Phase::Drained && ledger_digest(d)?!=d["runtimeLifecycle"]["transfer"]["ledgerSha256"].as_str().ok_or_else(err)? {return Err(err());}
    crate::runtime_lifecycle_backlog::validate(d)?;
    let queued_backlog=d["runtimeLifecycle"]["queuedBacklog"].clone();
    t.epoch=t.epoch.checked_add(1).ok_or_else(err)?;append(d,"maintenance-resumed",&t,json!({"previousEpoch":expected.epoch,
        "queuedBacklog":queued_backlog}));
    d["runtimeLifecycle"]["owner"]=token_value(&t);d["runtimeLifecycle"]["phase"]=json!(Phase::Running.name());
    d["runtimeLifecycle"]["target"]=Value::Null;d["runtimeLifecycle"]["transfer"]=Value::Null;d["runtimeLifecycle"]["queuedBacklog"]=Value::Null;Ok(t)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture()->(Value,OwnerToken) {
        let mut d=json!({"account":"LikeAvto","connectorBinding":{"id":"native-scoped"},"jobs":[],"operations":[{"id":"old","status":"unknown","action":{"reply":"unchanged"}}],"approvals":[],"audit":[],"materials":[],"knowledge_entries":[],"knowledge_versions":[]});
        let t=OwnerToken{account:"LikeAvto".into(),runtime_id:"runtime-one".into(),release_sha256:"a".repeat(64),epoch:1};
        let digest=ledger_digest(&d).unwrap();initialize(&mut d,t.clone(),&"b".repeat(64),&digest).unwrap();(d,t)
    }
    fn settled(t:&OwnerToken)->SettledNative {SettledNative{owner:t.clone(),application_tasks:0,provider_queued:0,provider_dispatched:0,provider_contained:true,credential_writers:0,unresolved_effects:0}}
    #[test] fn missing_or_malformed_state_never_admits() {
        let (mut d,t)=fixture();d.as_object_mut().unwrap().remove("runtimeLifecycle");assert!(require_admission(&d,&t,AdmissionClass::Media).is_err());
        let (mut d,t)=fixture();d["runtimeLifecycle"]["owner"]["epoch"]=json!(0);assert!(require_admission(&d,&t,AdmissionClass::Media).is_err());
    }
    #[test] fn drain_rejects_every_new_admission_and_stale_preflight() {
        let (mut d,t)=fixture();let drain=begin_drain(&mut d,&t,&"c".repeat(64),"release-one",false).unwrap();
        for class in [AdmissionClass::SourceRead,AdmissionClass::Media,AdmissionClass::Preparation,AdmissionClass::SocialDispatch] {assert!(require_admission(&d,&t,class).is_err());assert!(require_admission(&d,&drain,class).is_err());}
        let resumed=resume_same_owner(&mut d,&drain,&settled(&drain)).unwrap();assert!(require_admission(&d,&t,AdmissionClass::Preparation).is_err());require_admission(&d,&resumed,AdmissionClass::Media).unwrap();
    }
    #[test] fn pending_refresh_containment_and_unresolved_each_block() {
        let (mut d,t)=fixture();let drain=begin_drain(&mut d,&t,&"c".repeat(64),"attempt",false).unwrap();
        for field in 0..7 {let mut proof=settled(&drain);match field {0=>proof.application_tasks=1,1=>proof.provider_queued=1,2=>proof.provider_dispatched=1,3=>proof.provider_contained=false,4=>proof.credential_writers=1,5=>proof.unresolved_effects=1,_=>proof.owner.epoch+=1};assert!(mark_drained(&mut d,&drain,&proof).is_err());assert_eq!(d["runtimeLifecycle"]["phase"],"draining");}
    }
    #[test] fn active_paid_and_dispatch_hold_while_paused_unknown_are_preserved() {
        let (mut d,t)=fixture();d["jobs"]=json!([{"id":"paid","status":"running","receipt":{"source":"exact"}}]);let drain=begin_drain(&mut d,&t,&"c".repeat(64),"attempt",false).unwrap();
        assert!(mark_drained(&mut d,&drain,&settled(&drain)).is_err());d["jobs"][0]["status"]=json!("paused");d["operations"][0]["status"]=json!("dispatching");assert!(mark_drained(&mut d,&drain,&settled(&drain)).is_err());d["operations"][0]["status"]=json!("unknown");let old=d["operations"].clone();mark_drained(&mut d,&drain,&settled(&drain)).unwrap();assert_eq!(d["operations"],old);assert_eq!(d["jobs"][0]["receipt"]["source"],"exact");
    }
    #[test] fn whole_ledger_drift_after_freeze_blocks_stop_and_successor() {
        let (mut d,t)=fixture();let drain=begin_drain(&mut d,&t,&"c".repeat(64),"attempt",false).unwrap();let transfer=mark_drained(&mut d,&drain,&settled(&drain)).unwrap();d["jobs"]=json!([{"id":"late","status":"completed"}]);assert!(commit_stop_checkpoint(&mut d,&drain,&transfer).is_err());
    }
    #[test] fn complete_transfer_preserves_unknown_and_fences_old_owner() {
        let (mut d,t)=fixture();let original=ledger_digest(&d).unwrap();let drain=begin_drain(&mut d,&t,&"c".repeat(64),"attempt",false).unwrap();let transfer=mark_drained(&mut d,&drain,&settled(&drain)).unwrap();commit_stop_checkpoint(&mut d,&drain,&transfer).unwrap();assert!(accept_successor(&mut d,&drain,&transfer,"runtime-two",&"d".repeat(64),&"e".repeat(64),false).is_err());let next=accept_successor(&mut d,&drain,&transfer,"runtime-two",&"c".repeat(64),&"e".repeat(64),false).unwrap();assert_eq!(ledger_digest(&d).unwrap(),original);assert!(require_admission(&d,&t,AdmissionClass::Media).is_err());require_admission(&d,&next,AdmissionClass::Media).unwrap();assert_eq!(d["runtimeLifecycle"]["history"].as_array().unwrap().len(),5);
    }
    #[test] fn foreign_owner_and_rebootstrap_are_rejected() {
        let (mut d,t)=fixture();let mut foreign=t.clone();foreign.account="BAW Russia".into();assert!(require_admission(&d,&foreign,AdmissionClass::SourceRead).is_err());let ledger=ledger_digest(&d).unwrap();assert!(initialize(&mut d,t,&"b".repeat(64),&ledger).is_err());
    }
    #[test] fn real_company_display_and_fixed_native_owner_bindings() {
        for company in ["LikeAvto","BAW Russia"] {
            let (mut d,_)=fixture();d.as_object_mut().unwrap().remove("runtimeLifecycle");d["account"]=json!(company);
            let t=OwnerToken{account:company.into(),runtime_id:"actual-owner".into(),release_sha256:"a".repeat(64),epoch:1};
            let digest=ledger_digest(&d).unwrap();initialize(&mut d,t.clone(),&"b".repeat(64),&digest).unwrap();
            let mut native=RuntimeIdentity{account:company.into(),runtime_id:"actual-owner".into(),release_sha256:"a".repeat(64)};
            assert_eq!(bound_admission_token(&d,&native,AdmissionClass::Media).unwrap(),t);
            native.runtime_id="old-owner".into();assert!(bound_admission_token(&d,&native,AdmissionClass::Media).is_err());
        }
    }
    #[test] fn incompatible_media_rollback_requires_asr_disabled() {
        let (mut d,t)=fixture();let mut target=AdmittedTarget{release_sha256:"c".repeat(64),media_analysis_generation:0,asr_disabled:false};
        assert!(begin_drain_for_release(&mut d,&t,&target,"rollback").is_err());target.asr_disabled=true;
        begin_drain_for_release(&mut d,&t,&target,"rollback").unwrap();assert_eq!(d["runtimeLifecycle"]["target"]["asrDisabled"],true);
    }
}
