//! ROOT-reviewed, whole-file pinned startup admission; no implicit Running.
use crate::{ApiResult, conflict};
use crate::runtime_lifecycle::{self, AdmissionClass, AdmittedTarget, OwnerToken, RuntimeIdentity};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{collections::HashSet, fs::File, io::Read, path::{Component, Path, PathBuf}};
use tokio::sync::RwLock;

struct HeldTarget { target:AdmittedTarget, reviewed_record:Value }
pub(crate) struct VerifiedTarget { held:HeldTarget, file_pin:Value }
fn require_compatible(targets:&[HeldTarget],next:&HeldTarget)->ApiResult<()> {
    if targets.iter().any(|old|old.target.release_sha256==next.target.release_sha256&&old.reviewed_record!=next.reviewed_record) {
        return Err(conflict("Conflicting admitted target receipt"));
    }Ok(())
}

pub(crate) struct Admission {
    identity: RuntimeIdentity,
    startup: Value,
    targets: RwLock<Vec<HeldTarget>>,
    trace_release: Option<TraceReleasePins>,
    predecessor: Option<crate::predecessor_recovery::VerifiedStartup>,
    predecessor_launch: Option<crate::predecessor_recovery::VerifiedLaunch>,
}
#[derive(Clone, Debug)]
pub(crate) struct TraceReleasePins {
    pub(crate) source_checkpoint_sha256: String,
    pub(crate) binary_sha256: String,
    pub(crate) core_sha256: String,
}
fn fail() -> crate::ApiError { conflict("ROOT-reviewed native lifecycle startup admission required") }
fn exact(v:&Value, keys:&[&str])->bool {
    v.as_object().is_some_and(|o|o.len()==keys.len()&&keys.iter().all(|k|o.contains_key(*k)))
}
fn hash(s:&str)->bool { s.len()==64&&s.bytes().all(|b|b.is_ascii_digit()||matches!(b,b'a'..=b'f')) }
fn text<'a>(v:&'a Value)->ApiResult<&'a str> { v.as_str().ok_or_else(fail) }
fn digest(v:&Value)->ApiResult<&str> { let s=text(v)?;if !hash(s){return Err(fail());}Ok(s) }
fn opaque(s:&str)->bool { !s.is_empty()&&s.len()<=80&&s.bytes().all(|b|b.is_ascii_alphanumeric()||matches!(b,b'-'|b'_')) }
fn safe_path(value:&str,base:&Path)->ApiResult<PathBuf> {
    if value.is_empty()||value.chars().any(|c|c.is_control()||matches!(c,'*'|'?'|'"'|'<'|'>'|'|')) {return Err(fail());}
    let normalized=value.replace('\\',"/");
    if normalized.starts_with("//") {return Err(fail());}
    for (i,part) in normalized.split('/').enumerate() {
        if part==".."||part.ends_with([' ','.'])||(part.contains(':')&&!(i==0&&part.len()==2&&part.as_bytes()[0].is_ascii_alphabetic()&&part.ends_with(':')))
            ||matches!(part.to_ascii_lowercase().as_str(),"private"|"secrets"|"credentials"|"auth.json"|"operators.json"|"owner-access-code.txt")
            ||part.to_ascii_lowercase().starts_with(".env") {return Err(fail());}
    }
    let path=Path::new(value);
    if path.components().any(|c|matches!(c,Component::ParentDir)) {return Err(fail());}
    Ok(if path.is_absolute(){path.to_path_buf()}else{base.join(path)})
}
pub(crate) fn read_pinned(path:&Path,sha:&str,limit:u64)->ApiResult<Vec<u8>> {
    if !path.is_absolute()||!hash(sha){return Err(fail());}
    for ancestor in path.ancestors() {
        let m=std::fs::symlink_metadata(ancestor).map_err(|_|fail())?;
        if m.file_type().is_symlink(){return Err(fail());}
        #[cfg(windows)] { use std::os::windows::fs::MetadataExt; if m.file_attributes()&0x400!=0{return Err(fail());} }
    }
    let file=File::open(path).map_err(|_|fail())?;
    if !file.metadata().map_err(|_|fail())?.is_file(){return Err(fail());}
    let mut bytes=Vec::new();file.take(limit+1).read_to_end(&mut bytes).map_err(|_|fail())?;
    if bytes.len() as u64>limit||format!("{:x}",Sha256::digest(&bytes))!=sha{return Err(fail());}Ok(bytes)
}
fn json(bytes:&[u8])->ApiResult<Value>{serde_json::from_slice(bytes).map_err(|_|fail())}
fn same_path(a:&Path,b:&Path)->bool {
    #[cfg(windows)] { a.to_string_lossy().eq_ignore_ascii_case(&b.to_string_lossy()) }
    #[cfg(not(windows))] { a==b }
}
/// ROOT supplies both env pins only after full closure/absence review. Neither
/// missing nor partial environment enables a fixture or legacy Running fallback.
pub(crate) fn from_environment(selected_account:&str)->ApiResult<Admission> {
    let path=std::env::var("COMMUNITYHERO_LIFECYCLE_ADMISSION_PATH").map_err(|_|fail())?;
    let sha=std::env::var("COMMUNITYHERO_LIFECYCLE_ADMISSION_SHA256").map_err(|_|fail())?;
    let path=safe_path(&path,Path::new(""))?;
    if !path.is_absolute(){return Err(fail());}
    let bytes=read_pinned(&path,&sha,65536)?;
    let value=json(&bytes)?;
    let mut admission=decode(&value,selected_account)?;
    let base=path.parent().ok_or_else(fail)?;
    let core=verify_core(&value["installedCore"],base,true)?;
    admission.trace_release=trace_release_from_environment(&value["installedCore"],&core,base)?;
    let admission_pin=serde_json::json!({"path":path.to_string_lossy(),"sha256":sha});
    admission.predecessor_launch=crate::predecessor_recovery::VerifiedLaunch::from_environment(&value,&admission_pin)?;
    if value["startup"]["kind"]=="same-owner-predecessor-recovery" {
        admission.predecessor=Some(crate::predecessor_recovery::VerifiedStartup::from_admission(&value,&admission_pin)?);
    }
    Ok(admission)
}
fn trace_release_pins(manifest:&Value,core_pin:&Value,core:&Value)->ApiResult<TraceReleasePins>{
    if manifest["schemaVersion"]!=2||manifest["kind"]!="communityhero-engine-release-manifest"
        ||manifest["coreArtifact"]["sha256"]!=core_pin["sha256"]
        ||!exact(&manifest["sourceCheckpoint"],&["path","sha256"])
        ||manifest["sourceCheckpoint"]!=core["sourceCheckpoint"]{return Err(fail());}
    Ok(TraceReleasePins{source_checkpoint_sha256:digest(&manifest["sourceCheckpoint"]["sha256"])?.into(),
        binary_sha256:digest(&core["binarySha256"])?.into(),core_sha256:digest(&core_pin["sha256"])?.into()})
}
fn trace_release_from_environment(core_pin:&Value,core:&Value,admission_base:&Path)->ApiResult<Option<TraceReleasePins>>{
    let manifest_path=std::env::var("COMMUNITYHERO_RELEASE_MANIFEST_PATH").ok();
    let manifest_sha=std::env::var("COMMUNITYHERO_RELEASE_MANIFEST_SHA256").ok();
    let (path,sha)=match(manifest_path,manifest_sha){(None,None)=>return Ok(None),(Some(path),Some(sha))=>(path,sha),_=>return Err(fail())};
    let path=safe_path(&path,Path::new(""))?;if !path.is_absolute(){return Err(fail());}
    let manifest=json(&read_pinned(&path,&sha,16777216)?)?;
    let pins=trace_release_pins(&manifest,core_pin,core)?;
    let base=path.parent().ok_or_else(fail)?;
    let manifest_core=safe_path(text(&manifest["coreArtifact"]["path"])?,base)?;
    let admitted_core=safe_path(text(&core_pin["path"])?,admission_base)?;
    if !same_path(&std::fs::canonicalize(manifest_core).map_err(|_|fail())?,
        &std::fs::canonicalize(admitted_core).map_err(|_|fail())?){return Err(fail());}
    let checkpoint=safe_path(text(&manifest["sourceCheckpoint"]["path"])?,base)?;
    let frozen=json(&read_pinned(&checkpoint,&pins.source_checkpoint_sha256,16777216)?)?;
    if frozen["kind"]!="unpromoted-source-checkpoint"||!frozen["files"].is_array(){return Err(fail());}
    Ok(Some(pins))
}
pub(crate) fn verify_core(core_pin:&Value,admission_base:&Path,require_current_binary:bool)->ApiResult<Value> {
    if !exact(core_pin,&["path","sha256"]){return Err(fail());}
    let core_path=safe_path(text(&core_pin["path"])?,admission_base)?;
    let core=json(&read_pinned(&core_path,digest(&core_pin["sha256"])?,16777216)?)?;
    if core["schemaVersion"]!=1||core["kind"]!="company-independent-immutable-core" {return Err(fail());}
    let base=core_path.parent().ok_or_else(fail)?;
    let binary=safe_path(text(&core["binary"])?,base)?;
    // Bind the actual executable; a valid old binary at another path cannot
    // authorize this process. ROOT's closure receipt separately binds sources.
    if require_current_binary {
        let actual=std::env::current_exe().map_err(|_|fail())?;
        if !same_path(&std::fs::canonicalize(&binary).map_err(|_|fail())?,&std::fs::canonicalize(actual).map_err(|_|fail())?){return Err(fail());}
    }
    read_pinned(&binary,digest(&core["binarySha256"])?,1073741824)?;
    let runtime=safe_path(text(&core["runtimeRoot"])?,base)?;
    let assets=core["assets"].as_array().filter(|a|!a.is_empty()).ok_or_else(fail)?;
    let mut seen=HashSet::new();
    for asset in assets {
        let name=text(&asset["path"])?;
        if !["adapters/","cli/","connectors/","web/"].iter().any(|p|name.starts_with(p))
            ||name.contains('\\')||name.split('/').any(|p|p.is_empty()||p=="."||p=="..")
            ||!seen.insert(name.to_ascii_lowercase()){return Err(fail());}
        read_pinned(&safe_path(name,&runtime)?,digest(&asset["sha256"])?,268435456)?;
    }
    Ok(core)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn trace_release_requires_exact_full_checkpoint_and_core_binding(){
        let core_pin=json!({"path":"C:/fixture/core.json","sha256":"a".repeat(64)});
        let checkpoint=json!({"path":"C:/fixture/source-manifest.json","sha256":"b".repeat(64)});
        let core=json!({"sourceCheckpoint":checkpoint,"sourcePins":{"sha256":"d".repeat(64)},"binarySha256":"c".repeat(64)});
        let manifest=json!({"schemaVersion":2,"kind":"communityhero-engine-release-manifest","coreArtifact":core_pin,"sourceCheckpoint":checkpoint});
        let pins=trace_release_pins(&manifest,&core_pin,&core).unwrap();
        assert_eq!(pins.source_checkpoint_sha256,"b".repeat(64));
        assert_eq!(pins.binary_sha256,"c".repeat(64));assert_eq!(pins.core_sha256,"a".repeat(64));
        let mut wrong=manifest.clone();wrong["sourceCheckpoint"]["sha256"]=core["sourcePins"]["sha256"].clone();
        assert!(trace_release_pins(&wrong,&core_pin,&core).is_err());
        let mut wrong=manifest.clone();wrong["coreArtifact"]["sha256"]=json!("f".repeat(64));
        assert!(trace_release_pins(&wrong,&core_pin,&core).is_err());
        let mut incomplete=core.clone();incomplete.as_object_mut().unwrap().remove("sourceCheckpoint");
        assert!(trace_release_pins(&manifest,&core_pin,&incomplete).is_err());
        assert!(Admission::fixture(crate::accounts::Profile::BawRussia).trace_release().is_none());
    }
    fn fixture()->Value {
        json!({"schemaVersion":1,"kind":"root-reviewed-native-runtime-lifecycle-startup",
            "fixedOwner":{"account":"LikeAvto","runtimeId":"native-owner","releaseSha256":"a".repeat(64)},
            "installedCore":{"path":"C:/public/core.json","sha256":"a".repeat(64)},
            "startup":{"kind":"bootstrap","receiptSha256":"b".repeat(64),"expectedLedgerSha256":"c".repeat(64)},
            "admittedTargets":[{"releaseSha256":"d".repeat(64),"mediaAnalysisGeneration":1,"asrDisabled":false,"admissionReceiptSha256":"e".repeat(64)}]})
    }
    #[test] fn company_core_unknown_schema_and_unadmitted_target_are_rejected() {
        let v=fixture();assert!(decode(&v,"BAW Russia").is_err());
        let a=decode(&v,"LikeAvto").unwrap();assert!(a.target(&"f".repeat(64)).is_err());
        assert!(a.target(&"d".repeat(64)).is_ok());
        let mut changed=v.clone();changed["fixedOwner"]["extra"]=json!(true);assert!(decode(&changed,"LikeAvto").is_err());
        let mut changed=v.clone();changed["installedCore"]["sha256"]=json!("f".repeat(64));assert!(decode(&changed,"LikeAvto").is_err());
        let mut changed=v.clone();changed["startup"]["kind"]=json!("automatic");assert!(decode(&changed,"LikeAvto").is_err());
        let mut changed=v.clone();changed["admittedTargets"][0]["mediaAnalysisGeneration"]=json!(0);assert!(decode(&changed,"LikeAvto").is_err());
        let mut changed=v.clone();changed["admittedTargets"].as_array_mut().unwrap().push(v["admittedTargets"][0].clone());assert!(decode(&changed,"LikeAvto").is_err());
    }
    #[test] fn explicit_recovery_does_not_initialize_missing_workspace() {
        let mut v=fixture();v["startup"]["kind"]=json!("same-owner-recovery");
        let mut d=json!({"account":"LikeAvto","connectorBinding":{},"jobs":[],"operations":[],"approvals":[],"audit":[],"materials":[],"knowledge_entries":[],"knowledge_versions":[]});
        v["startup"]["expectedLedgerSha256"]=json!(runtime_lifecycle::ledger_digest(&d).unwrap());
        let a=decode(&v,"LikeAvto").unwrap();
        assert!(a.initialize_workspace(&mut d).is_err());assert!(d.get("runtimeLifecycle").is_none());
        let t=OwnerToken{account:"LikeAvto".into(),runtime_id:"native-owner".into(),release_sha256:"a".repeat(64),epoch:1};
        let ledger=runtime_lifecycle::ledger_digest(&d).unwrap();
        runtime_lifecycle::initialize(&mut d,t.clone(),&"b".repeat(64),&ledger).unwrap();
        runtime_lifecycle::begin_drain_for_release(&mut d,&t,&a.target(&"d".repeat(64)).unwrap(),"attempt").unwrap();
        assert!(a.initialize_workspace(&mut d).is_err());assert_eq!(d["runtimeLifecycle"]["phase"],"draining");
    }
    #[test] fn held_targets_preserve_receipt_and_reject_conflicting_republication() {
        let a=decode(&fixture(),"LikeAvto").unwrap();let targets=a.targets.try_read().unwrap();
        let old=&targets[0];let record=old.reviewed_record.clone();
        let same=HeldTarget{target:old.target.clone(),reviewed_record:record.clone()};
        require_compatible(&targets,&same).unwrap();
        for field in ["admissionReceiptSha256","asrDisabled","mediaAnalysisGeneration"] {
            let mut changed=record.clone();changed[field]=serde_json::json!("conflict");
            let candidate=HeldTarget{target:old.target.clone(),reviewed_record:changed};
            assert!(require_compatible(&targets,&candidate).is_err());
        }
        assert_eq!(targets[0].reviewed_record,record);
    }
    #[tokio::test] async fn pending_registration_blocks_target_lookup() {
        let a=decode(&fixture(),"LikeAvto").unwrap();let guard=a.targets.write().await;
        assert!(a.target(&"d".repeat(64)).is_err());drop(guard);
        assert!(a.target(&"d".repeat(64)).is_ok());
    }
}
/// Pure decoder for source fixtures. Actual admission always uses the env file
/// whole SHA and native executable/assets checks above; never expose over HTTP.
fn decode(v:&Value,selected_account:&str)->ApiResult<Admission> {
    if !exact(v,&["schemaVersion","kind","fixedOwner","installedCore","startup","admittedTargets"])
        ||v["schemaVersion"]!=1||v["kind"]!="root-reviewed-native-runtime-lifecycle-startup"
        ||!exact(&v["fixedOwner"],&["account","runtimeId","releaseSha256"])
        ||!exact(&v["installedCore"],&["path","sha256"]){return Err(fail());}
    let owner=&v["fixedOwner"];
    let account=text(&owner["account"])?;
    if !matches!(account,"LikeAvto"|"BAW Russia")||account!=selected_account||!opaque(text(&owner["runtimeId"])?)
        ||digest(&v["installedCore"]["sha256"])?!=digest(&owner["releaseSha256"])? {return Err(fail());}
    text(&v["installedCore"]["path"])?;
    let startup=&v["startup"];
    let kind=text(&startup["kind"])?;
    let keys=match kind {
        "bootstrap"|"same-owner-recovery"=>vec!["kind","receiptSha256","expectedLedgerSha256"],
        "successor"=>vec!["kind","receiptSha256","expectedLedgerSha256","previousOwner","transfer"],
        "same-owner-predecessor-recovery"=>vec!["kind","receiptSha256","expectedLedgerSha256","importReceipt","nextStartIntent"],
        _=>return Err(fail()),
    };
    if !exact(startup,&keys){return Err(fail());}
    digest(&startup["receiptSha256"])?;digest(&startup["expectedLedgerSha256"])?;
    if kind=="same-owner-predecessor-recovery"{crate::predecessor_recovery::pin(&startup["importReceipt"])?;crate::predecessor_recovery::pin(&startup["nextStartIntent"])?;}
    if kind=="successor" { runtime_lifecycle::parse_token(&startup["previousOwner"])?;if !startup["transfer"].is_object(){return Err(fail());} }
    let mut targets=Vec::new();let mut seen=HashSet::new();
    for t in v["admittedTargets"].as_array().ok_or_else(fail)? {
        if !exact(t,&["releaseSha256","mediaAnalysisGeneration","asrDisabled","admissionReceiptSha256"]){return Err(fail());}
        let sha=digest(&t["releaseSha256"])?;
        let generation=t["mediaAnalysisGeneration"].as_u64().ok_or_else(fail)?;
        let disabled=t["asrDisabled"].as_bool().ok_or_else(fail)?;
        digest(&t["admissionReceiptSha256"])?;
        if (generation==0&&!disabled)||!seen.insert(sha.to_owned()){return Err(fail());}
        targets.push(HeldTarget{target:AdmittedTarget{release_sha256:sha.into(),media_analysis_generation:generation,asr_disabled:disabled},reviewed_record:t.clone()});
    }
    Ok(Admission{identity:RuntimeIdentity{account:account.into(),runtime_id:text(&owner["runtimeId"])?.into(),release_sha256:digest(&owner["releaseSha256"])?.into()},startup:startup.clone(),targets:RwLock::new(targets),trace_release:None,predecessor:None,predecessor_launch:None})
}
impl Admission {
    /// Explicit synthetic App constructor; completely absent from production.
    #[cfg(test)]
    pub(crate) fn fixture(profile:crate::accounts::Profile)->Self {
        Self{identity:RuntimeIdentity{account:profile.display().into(),runtime_id:format!("fixture-runtime-{}",profile.key()),release_sha256:"a".repeat(64)},
            startup:Value::Null,trace_release:None,predecessor:None,predecessor_launch:None,targets:RwLock::new(vec![HeldTarget{target:AdmittedTarget{release_sha256:"c".repeat(64),media_analysis_generation:1,asr_disabled:true},
                reviewed_record:serde_json::json!({"releaseSha256":"c".repeat(64),"mediaAnalysisGeneration":1,"asrDisabled":true,"admissionReceiptSha256":"b".repeat(64)})}])}
    }
    pub(crate) fn identity(&self)->&RuntimeIdentity{&self.identity}
    pub(crate) fn requires_predecessor_startup(&self)->bool{self.startup["kind"]=="same-owner-predecessor-recovery"}
    pub(crate) fn requires_verified_startup(&self)->bool{self.requires_predecessor_startup()||self.predecessor_launch.is_some()}
    pub(crate) async fn initialize_verified_startup(&self,db:&crate::Database)->ApiResult<OwnerToken>{
        let launch=self.predecessor_launch.as_ref().ok_or_else(fail)?;
        let token=db.initialize_verified_startup(self,self.predecessor.as_ref(),launch).await?;runtime_lifecycle::require_runtime_owner(&token,&self.identity)?;Ok(token)
    }
    pub(crate) async fn consume_predecessor_startup(&self,db:&crate::Database)->ApiResult<OwnerToken>{
        if !self.requires_predecessor_startup(){return Err(fail());}self.initialize_verified_startup(db).await
    }
    pub(crate) fn trace_release(&self)->Option<&TraceReleasePins>{self.trace_release.as_ref()}
    pub(crate) fn target(&self,sha:&str)->ApiResult<AdmittedTarget>{
        let targets=self.targets.try_read().map_err(|_|conflict("Target registration in progress"))?;
        targets.iter().find(|t|t.target.release_sha256==sha).map(|t|t.target.clone()).ok_or_else(fail)
    }
    /// Exact local ROOT receipt, never HTTP target capability flags. Future
    /// release binary/assets are checked, without equating them to this process.
    pub(crate) fn verify_target_file(&self,pin:&Value)->ApiResult<VerifiedTarget> {
        if !exact(pin,&["path","sha256"]){return Err(fail());}
        let path=safe_path(text(&pin["path"])?,Path::new(""))?;
        if !path.is_absolute(){return Err(fail());}
        let v=json(&read_pinned(&path,digest(&pin["sha256"])?,65536)?)?;
        if !exact(&v,&["schemaVersion","kind","account","installedCore","target","admissionReceiptSha256"])
            ||v["schemaVersion"]!=1||v["kind"]!="root-reviewed-native-runtime-target-admission"
            ||text(&v["account"])?!=self.identity.account
            ||!exact(&v["target"],&["releaseSha256","mediaAnalysisGeneration","asrDisabled"])
            ||digest(&v["installedCore"]["sha256"])?!=digest(&v["target"]["releaseSha256"])? {return Err(fail());}
        let target=&v["target"];
        let generation=target["mediaAnalysisGeneration"].as_u64().ok_or_else(fail)?;
        let disabled=target["asrDisabled"].as_bool().ok_or_else(fail)?;
        if generation==0&&!disabled{return Err(fail());}
        let receipt=digest(&v["admissionReceiptSha256"])?;
        verify_core(&v["installedCore"],path.parent().ok_or_else(fail)?,false)?;
        let held=HeldTarget{target:AdmittedTarget{release_sha256:digest(&target["releaseSha256"])?.into(),media_analysis_generation:generation,asr_disabled:disabled},
            reviewed_record:serde_json::json!({"releaseSha256":target["releaseSha256"],"mediaAnalysisGeneration":generation,"asrDisabled":disabled,"admissionReceiptSha256":receipt})};
        Ok(VerifiedTarget{held,file_pin:pin.clone()})
    }
    /// Hold the native registry writer through the durable owner-check commit.
    /// Pending entries are unobservable; commit failure never appends anything.
    pub(crate) async fn register_target(&self,app:&crate::App,expected:&OwnerToken,verified:VerifiedTarget)->ApiResult<Value> {
        runtime_lifecycle::require_runtime_owner(expected,&self.identity)?;
        let mut targets=self.targets.write().await;
        require_compatible(&targets,&verified.held)?;
        app.change_runtime_lifecycle(|d| {
            if runtime_lifecycle::current_owner(d,&self.identity)?!=*expected{return Err(conflict("Target registration owner epoch mismatch"));}
            Ok(())
        }).await?;
        let record=verified.held.reviewed_record.clone();
        if !targets.iter().any(|t|t.target.release_sha256==verified.held.target.release_sha256){targets.push(verified.held);}
        Ok(serde_json::json!({"registeredTarget":record,"admission":verified.file_pin,"stopAuthorized":false}))
    }
    /// ROOT calls under complete workspace writer lock BEFORE worker admission.
    /// No default initialize/resume exists; malformed/unfinished state is closed.
    pub(crate) fn initialize_workspace(&self,d:&mut Value)->ApiResult<OwnerToken> {
        crate::predecessor_recovery::guard_untracked_startup(d,&self.identity)?;
        if self.predecessor_launch.is_some(){return Err(fail());}
        self.initialize_workspace_admitted(d)
    }
    pub(crate) fn initialize_workspace_with_launch(&self,d:&mut Value,launch:&crate::predecessor_recovery::VerifiedLaunch)->ApiResult<OwnerToken> {
        launch.validate_workspace(d)?;
        if self.predecessor_launch.as_ref().is_none_or(|owned|!std::ptr::eq(owned,launch)){return Err(fail());}
        self.initialize_workspace_admitted(d)
    }
    fn initialize_workspace_admitted(&self,d:&mut Value)->ApiResult<OwnerToken> {
        crate::predecessor_recovery::guard_startup(d,false)?;
        let expected=digest(&self.startup["expectedLedgerSha256"])?;
        if runtime_lifecycle::ledger_digest(d)?!=expected{return Err(fail());}
        match text(&self.startup["kind"])? {
            "bootstrap"=> {
                let t=OwnerToken{account:self.identity.account.clone(),runtime_id:self.identity.runtime_id.clone(),release_sha256:self.identity.release_sha256.clone(),epoch:1};
                runtime_lifecycle::initialize(d,t.clone(),digest(&self.startup["receiptSha256"])?,expected)?;Ok(t)
            },
            "successor"=> {
                let previous=runtime_lifecycle::parse_token(&self.startup["previousOwner"])?;
                if previous.account!=self.identity.account{return Err(fail());}
                let transfer=&self.startup["transfer"];
                if transfer["ledgerSha256"].as_str()!=Some(expected){return Err(fail());}
                let disabled=transfer["target"]["asrDisabled"].as_bool().ok_or_else(fail)?;
                let t=runtime_lifecycle::accept_successor(d,&previous,transfer,&self.identity.runtime_id,&self.identity.release_sha256,digest(&self.startup["receiptSha256"])?,disabled)?;
                runtime_lifecycle::require_runtime_owner(&t,&self.identity)?;Ok(t)
            },
            "same-owner-recovery"=>runtime_lifecycle::bound_admission_token(d,&self.identity,AdmissionClass::SourceRead),
            _=>Err(fail()),
        }
    }
}

/// Invoke explicitly at the end of synthetic fixture seeding under its writer;
/// never used by from_environment or production startup/recovery.
#[cfg(test)]
pub(crate) fn initialize_fixture(workspace:&mut Value,identity:&RuntimeIdentity)->ApiResult<OwnerToken> {
    if workspace.get("runtimeLifecycle").is_some(){return Err(fail());}
    let token=OwnerToken{account:identity.account.clone(),runtime_id:identity.runtime_id.clone(),release_sha256:identity.release_sha256.clone(),epoch:1};
    let ledger=runtime_lifecycle::ledger_digest(workspace)?;
    runtime_lifecycle::initialize(workspace,token.clone(),&"b".repeat(64),&ledger)?;Ok(token)
}

/// Explicit isolated-test DB bootstrap only, after its fixture rows are seeded.
/// Existing lifecycle is checked against the fixture identity, never replaced.
#[cfg(test)]
pub(crate) async fn initialize_db_fixture(db:&crate::Database)->ApiResult<RuntimeIdentity> {
    let metadata=db.read_metadata().await?;
    let profile=crate::accounts::Profile::from_workspace(&metadata)?;
    let identity=Admission::fixture(profile).identity().clone();
    db.change_runtime_lifecycle_with_ledger(|d| {
        if d.get("runtimeLifecycle").is_none(){initialize_fixture(d,&identity)?;}
        else{runtime_lifecycle::current_owner(d,&identity)?;}
        Ok(())
    }).await?;
    Ok(identity)
}
#[cfg(test)]
#[path = "runtime_lifecycle_registration_gate_tests.rs"]
mod registration_gate_tests;
#[cfg(test)]
#[path = "runtime_lifecycle_fixture_producer.rs"]
pub(crate) mod fixture_producer;
