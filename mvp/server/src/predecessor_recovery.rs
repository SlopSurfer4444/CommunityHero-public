//! Offline, same-core PostgreSQL predecessor retirement. No provider work is
//! performed here. A durable UNKNOWN is never converted into an outcome.
use crate::{ApiResult, Value, json, conflict};
use std::{cell::RefCell, collections::HashSet, path::Path, time::{Duration, Instant}};

#[path="predecessor_recovery_evidence.rs"] mod evidence;
#[path="predecessor_recovery_io.rs"] mod io;
pub(crate) use io::{run_command, read_pin};

pub(crate) const WORKSPACE_BYTES:u64=536_870_912;
pub(crate) const COHORT_BYTES:u64=8_388_608;
pub(crate) const COHORT_ROWS:usize=4;
pub(crate) const IMPORT_PREFIX:&str="predecessor-import:";
pub(crate) const START_PREFIX:&str="predecessor-start:";
pub(crate) const LAUNCH_PREFIX:&str="predecessor-launch:";
pub(crate) fn fail()->crate::ApiError { conflict("Predecessor recovery evidence, episode, scope or immutable state mismatch") }
pub(crate) fn exact(v:&Value,keys:&[&str])->ApiResult<()> {
    if !v.as_object().is_some_and(|o|o.len()==keys.len()&&keys.iter().all(|k|o.contains_key(*k))) {return Err(fail());} Ok(())
}
pub(crate) fn text(v:&Value)->ApiResult<&str>{v.as_str().filter(|s|!s.is_empty()).ok_or_else(fail)}
pub(crate) fn hash(v:&Value)->ApiResult<&str>{let s=text(v)?;if s.len()!=64||!s.bytes().all(|b|b.is_ascii_digit()||(b'a'..=b'f').contains(&b)){return Err(fail());}Ok(s)}
pub(crate) fn digest(v:&Value)->String{crate::connection_gate::digest(v)}
pub(crate) fn pin(v:&Value)->ApiResult<()> {exact(v,&["path","sha256"])?;let s=text(&v["path"])?;if s.len()>4096||!Path::new(s).is_absolute(){return Err(fail());}hash(&v["sha256"])?;Ok(())}
fn opaque(v:&Value)->ApiResult<&str>{let s=text(v)?;if s.len()>80||!s.bytes().all(|b|b.is_ascii_alphanumeric()||matches!(b,b'_'|b'-')){return Err(fail());}Ok(s)}
fn stamp(v:&Value)->ApiResult<()> {chrono::DateTime::parse_from_rfc3339(text(v)?).map_err(|_|fail())?;Ok(())}
pub(crate) fn limits(v:&Value)->ApiResult<()> {
    let fields=[("totalMs",120000),("verificationMs",30000),("lockMs",5000),("databaseMs",60000),("cleanupReserveMs",10000),("inputBytes",1048576),("intentBytes",65536),("rawBytes",4194304),("recordBytes",1048576),("resultBytes",1048576),("workspaceBytes",WORKSPACE_BYTES),("cohortBytes",COHORT_BYTES),("cohortRows",4),("rawProcesses",2048),("rawEvents",4096)];
    exact(v,&fields.iter().map(|p|p.0).collect::<Vec<_>>())?;if fields.iter().any(|(k,n)|v[*k].as_u64()!=Some(*n)){return Err(fail());}Ok(())
}
pub(crate) fn scope(v:&Value)->ApiResult<()> {
    exact(v,&["companyId","account","workspaceId","connectorBinding","storage"])?;
    let profile=crate::accounts::Profile::parse(text(&v["companyId"])?)?;
    if v["account"]!=profile.display()||v["workspaceId"]!="local-pilot" {return Err(fail());}
    let binding=crate::ConnectorBinding::from_json(&v["connectorBinding"]).map_err(|_|fail())?;
    binding.validate_scope("local-pilot",profile.display()).map_err(|_|fail())?;
    if binding.to_json()!=v["connectorBinding"]{return Err(fail());}
    let s=&v["storage"];exact(s,&["kind","host","port","database","user","dataDir","storageGeneration"])?;
    if s["kind"]!="postgres"||!matches!(s["host"].as_str(),Some("127.0.0.1"|"localhost"|"::1"))||s["port"].as_u64().is_none_or(|n|n==0||n>65535){return Err(fail());}
    for k in ["database","user"]{let t=text(&s[k])?;if t.len()>63||!t.bytes().all(|b|b.is_ascii_alphanumeric()||b==b'_'){return Err(fail());}}
    let path=Path::new(text(&s["dataDir"])?);if !path.is_absolute(){return Err(fail());}
    let generation=text(&s["storageGeneration"])?;let id=uuid::Uuid::parse_str(generation).map_err(|_|fail())?;
    if id.get_version_num()!=4||id.to_string()!=generation{return Err(fail());}Ok(())
}
fn package(v:&Value,owner:&Value)->ApiResult<()> {
    exact(v,&["core","binary","sourceCheckpoint"])?;for k in ["core","binary","sourceCheckpoint"]{pin(&v[k])?;}
    let token=crate::runtime_lifecycle::parse_token(owner)?;if token.release_sha256!=text(&v["core"]["sha256"])?{return Err(fail());}Ok(())
}
fn common(v:&Value)->ApiResult<()> {
    if v["schemaVersion"]!=1 {return Err(fail());}opaque(&v["importId"])?;scope(&v["scope"])?;package(&v["package"],&v["owner"])?;
    if v["owner"]["account"]!=v["scope"]["account"]{return Err(fail());}Ok(())
}
pub(crate) fn validate_input(v:&Value)->ApiResult<()> {
    exact(v,&["schemaVersion","kind","importId","scope","owner","package","expectedState","predecessorProof","cohort","nextStartIntent","validator","receiptPath","limits"])?;
    common(v)?;if v["kind"]!="root-reviewed-predecessor-transport-import" {return Err(fail());}limits(&v["limits"])?;pin(&v["nextStartIntent"])?;
    if !Path::new(text(&v["receiptPath"])?).is_absolute(){return Err(fail());}
    exact(&v["expectedState"],&["ledgerSha256","lifecycleSha256","gateSha256","auditCount"])?;
    for k in ["ledgerSha256","lifecycleSha256","gateSha256"]{hash(&v["expectedState"][k])?;}
    if v["expectedState"]["auditCount"].as_u64().is_none(){return Err(fail());}
    exact(&v["predecessorProof"],&["launchRequest","launchIntent","preLaunchBaseline","subjectCohortCapture","lifecycleAdmission","identity","rawObservation","control","observer"])?;
    for k in ["launchRequest","launchIntent","preLaunchBaseline","subjectCohortCapture","lifecycleAdmission","identity","rawObservation"]{pin(&v["predecessorProof"][k])?;}
    if !v["predecessorProof"]["control"].is_null(){pin(&v["predecessorProof"]["control"])?;}
    exact(&v["predecessorProof"]["observer"],&["source","script","assembly","assemblyBuild","sourceReview","preflight"])?;
    for p in v["predecessorProof"]["observer"].as_object().unwrap().values(){pin(p)?;}
    exact(&v["validator"],&["binary","sourceCheckpoint","sourcePins","review"])?;
    if v["validator"]["binary"]!=v["package"]["binary"]||v["validator"]["sourceCheckpoint"]!=v["package"]["sourceCheckpoint"]{return Err(fail());}
    for k in ["binary","sourceCheckpoint","sourcePins","review"]{pin(&v["validator"][k])?;}
    cohort_schema(&v["cohort"],&v["owner"],false)
}
fn cohort_schema(v:&Value,owner:&Value,empty:bool)->ApiResult<()> {
    let rows=v.as_array().ok_or_else(fail)?;if rows.len()>COHORT_ROWS||(!empty&&rows.is_empty())||v.to_string().len() as u64>COHORT_BYTES{return Err(fail());}
    let mut ids=HashSet::new();let mut attempts=HashSet::new();let mut permits=HashSet::new();
    for row in rows {exact(row,&["operationId","attemptId","permitId","owner","operationSha256","permitSha256"])?;
        if row["owner"]!=*owner||!ids.insert(text(&row["operationId"])?)||!attempts.insert(text(&row["attemptId"])?)||!permits.insert(text(&row["permitId"])?) {return Err(fail());}
        hash(&row["operationSha256"])?;hash(&row["permitSha256"])?;
    }Ok(())
}
pub(crate) fn cohort(d:&Value,owner:&Value)->ApiResult<Value> {
    let mut rows=vec![];for op in d["operations"].as_array().ok_or_else(fail)? {
        if op.get(crate::connection_gate::PERMIT_FIELD).is_none(){continue;}
        if !crate::connection_gate::valid_permit(op){return Err(fail());}
        let p=&op[crate::connection_gate::PERMIT_FIELD];if p["phase"]!="dispatch_armed"{continue;}
        if p["owner"]!=*owner||p["connectionBinding"]!=d["connectorBinding"] {return Err(fail());}
        rows.push(json!({"operationId":op["id"],"attemptId":op["attemptId"],"permitId":p["id"],"owner":p["owner"],"operationSha256":digest(op),"permitSha256":digest(p)}));
    }let value=json!(rows);cohort_schema(&value,owner,true)?;Ok(value)
}
fn workspace_scope(d:&Value,s:&Value,owner:&Value)->ApiResult<()> {
    scope(s)?;if d["account"]!=s["account"]||d["connectorBinding"]!=s["connectorBinding"]||d["storageGeneration"]!=s["storage"]["storageGeneration"]||crate::runtime_lifecycle::status(d)?["owner"]!=*owner{return Err(fail());}
    if crate::runtime_lifecycle::admission_token(d,crate::runtime_lifecycle::AdmissionClass::SourceRead)?!=crate::runtime_lifecycle::parse_token(owner)?{return Err(fail());}
    crate::connection_gate::continuation_gate_observation(d)?;Ok(())
}
pub(crate) fn capture(d:&Value,s:&Value,owner:&Value,p:&Value)->ApiResult<Value> {
    workspace_scope(d,s,owner)?;package(p,owner)?;
    let out=json!({"schemaVersion":1,"kind":"native-predecessor-workspace-capture","scope":s,"owner":owner,"package":p,
        "ledgerSha256":crate::runtime_lifecycle::ledger_digest(d)?,"lifecycleSha256":digest(&d["runtimeLifecycle"]),"gateSha256":digest(&d[crate::connection_gate::FIELD]),
        "auditCount":d["audit"].as_array().ok_or_else(fail)?.len(),"cohort":cohort(d,owner)?,"workspace":d});
    if out.to_string().len() as u64>WORKSPACE_BYTES{return Err(fail());}Ok(out)
}
pub(crate) fn validate_capture(v:&Value,s:&Value,owner:&Value,p:&Value)->ApiResult<()> {
    exact(v,&["schemaVersion","kind","scope","owner","package","ledgerSha256","lifecycleSha256","gateSha256","auditCount","cohort","workspace"])?;
    if *v!=capture(&v["workspace"],s,owner,p)? {return Err(fail());}Ok(())
}
pub(crate) struct Budget{start:Instant}
impl Budget {
    pub(crate) fn new()->Self{Self{start:Instant::now()}}
    pub(crate) fn remaining(&self,cap_ms:u64)->ApiResult<Duration>{let left=Duration::from_millis(110000).checked_sub(self.start.elapsed()).ok_or_else(fail)?;if left.is_zero(){return Err(fail());}Ok(left.min(Duration::from_millis(cap_ms)))}
    pub(crate) fn verification(&self)->ApiResult<()>{if self.start.elapsed()>Duration::from_millis(30000){return Err(fail());}self.remaining(30000)?;Ok(())}
}
/// Constructed only after the complete pinned causal chain has been verified.
/// No Deserialize implementation and no public arbitrary JSON constructor.
pub(crate) struct VerifiedImport{input:Value,input_pin:Value,_pins:io::Pins}
impl VerifiedImport {
    pub(crate) fn from_file(pin:&Value,budget:&Budget)->ApiResult<Self>{let input=read_pin(pin,1048576)?;validate_input(&input)?;let held=evidence::verify(&input,pin,budget)?;Ok(Self{input,input_pin:pin.clone(),_pins:held})}
    pub(crate) fn input(&self)->&Value{&self.input}
    pub(crate) fn input_pin(&self)->&Value{&self.input_pin}
    pub(crate) fn permits(&self,d:&Value)->ApiResult<Vec<VerifiedPredecessorCessation>> {
        let expected=&self.input["expectedState"];workspace_scope(d,&self.input["scope"],&self.input["owner"])?;
        if crate::runtime_lifecycle::ledger_digest(d)?!=text(&expected["ledgerSha256"])?||digest(&d["runtimeLifecycle"])!=text(&expected["lifecycleSha256"])?||digest(&d[crate::connection_gate::FIELD])!=text(&expected["gateSha256"])?
            ||d["audit"].as_array().ok_or_else(fail)?.len() as u64!=expected["auditCount"].as_u64().ok_or_else(fail)?||cohort(d,&self.input["owner"])?!=self.input["cohort"] {return Err(fail());}
        Ok(self.input["cohort"].as_array().unwrap().iter().map(|row|VerifiedPredecessorCessation{row:row.clone(),evidence_sha256:self.input_pin["sha256"].as_str().unwrap().into()}).collect())
    }
    pub(crate) fn apply(&self,d:&mut Value)->ApiResult<Value> {
        guard_startup(d,false)?;
        let witnesses=self.permits(d)?;let (imports,starts)=reserved(d)?;
        if imports.iter().any(|r|r["refId"]==self.input["importId"])||starts.iter().any(|r|r["refId"]==self.input["importId"]){return Err(fail());}
        let created=crate::now();let mut settled=vec![];
        for witness in witnesses {let row=witness.row.clone();let op=crate::row(d,"operations",text(&row["operationId"])?)?.clone();
            let native=crate::connection_gate::TransportCessation::from_predecessor(&op,witness)?;
            let p=crate::connection_gate::settle(d,&native)?;
            settled.push(json!({"operationId":row["operationId"],"attemptId":row["attemptId"],"permitId":row["permitId"],"settledPermitSha256":digest(&p),"settledAt":p["settledAt"],"cessationEvidenceSha256":self.input_pin["sha256"]}));
        }
        crate::connection_gate::close_after_predecessor(d)?;
        let transition=crate::runtime_lifecycle::ledger_digest(d)?;
        let record=json!({"id":format!("{IMPORT_PREFIX}{}",text(&self.input["importId"])?),"action":"connection.predecessor_imported","refId":self.input["importId"],"createdAt":created,"owner":self.input["owner"],"evidence":{
            "schemaVersion":1,"importId":self.input["importId"],"input":self.input_pin,"nextStartIntent":self.input["nextStartIntent"],"scope":self.input["scope"],"package":self.input["package"],
            "preLedgerSha256":self.input["expectedState"]["ledgerSha256"],"preLifecycleSha256":self.input["expectedState"]["lifecycleSha256"],"preGateSha256":self.input["expectedState"]["gateSha256"],
            "auditOrdinal":self.input["expectedState"]["auditCount"],"cohortSha256":digest(&self.input["cohort"]),"predecessorProofSha256":digest(&self.input["predecessorProof"]),"transitionLedgerSha256":transition,"settledRows":settled,"gateAfterSha256":digest(&d[crate::connection_gate::FIELD])}});
        if record.to_string().len()>1048576{return Err(fail());}
        d["audit"].as_array_mut().ok_or_else(fail)?.push(record);self.reconcile(d)
    }
    pub(crate) fn reconcile(&self,d:&Value)->ApiResult<Value>{result(d,&self.input,&self.input_pin)}
    pub(crate) fn observe_reconcile(&self,d:&Value)->ApiResult<Value>{
        let (imports,_)=reserved(d)?;
        if !imports.iter().any(|r|r["refId"]==self.input["importId"]){self.permits(d)?;guard_startup(d,false)?;
            return Ok(json!({"schemaVersion":1,"kind":"native-predecessor-import-not-committed","importId":self.input["importId"],"input":self.input_pin,"ledgerSha256":self.input["expectedState"]["ledgerSha256"],"dispatchAuthorized":false,"retryAuthorized":false}));}
        self.reconcile(d)
    }
    pub(crate) fn transition(&self,before:&Value)->ApiResult<AuthorizedTransition>{
        let mut after=before.clone();let output=self.apply(&mut after)?;
        for (old,new) in before["operations"].as_array().ok_or_else(fail)?.iter().zip(after["operations"].as_array().ok_or_else(fail)?) {
            let mut expected=old.clone();
            if old.get(crate::connection_gate::PERMIT_FIELD).is_some(){expected[crate::connection_gate::PERMIT_FIELD]=new.get(crate::connection_gate::PERMIT_FIELD).ok_or_else(fail)?.clone();}
            if expected!=*new{return Err(fail());}
        }
        AuthorizedTransition::new(before,after,output)
    }
}
pub(crate) struct VerifiedPredecessorCessation{row:Value,evidence_sha256:String}
impl VerifiedPredecessorCessation{pub(crate) fn into_parts(self)->(Value,String){(self.row,self.evidence_sha256)}}

fn import_record(r:&Value)->ApiResult<()> {
    exact(r,&["id","action","refId","createdAt","owner","evidence"])?;let id=opaque(&r["refId"])?;
    if r["id"]!=format!("{IMPORT_PREFIX}{id}")||r["action"]!="connection.predecessor_imported"{return Err(fail());}stamp(&r["createdAt"])?;crate::runtime_lifecycle::parse_token(&r["owner"])?;
    let e=&r["evidence"];exact(e,&["schemaVersion","importId","input","nextStartIntent","scope","package","preLedgerSha256","preLifecycleSha256","preGateSha256","auditOrdinal","cohortSha256","predecessorProofSha256","transitionLedgerSha256","settledRows","gateAfterSha256"])?;
    if e["schemaVersion"]!=1||e["importId"]!=r["refId"]||e["scope"]["account"]!=r["owner"]["account"]||e["auditOrdinal"].as_u64().is_none(){return Err(fail());}
    scope(&e["scope"])?;package(&e["package"],&r["owner"])?;pin(&e["input"])?;pin(&e["nextStartIntent"])?;
    for k in ["preLedgerSha256","preLifecycleSha256","preGateSha256","cohortSha256","predecessorProofSha256","transitionLedgerSha256","gateAfterSha256"]{hash(&e[k])?;}
    let rows=e["settledRows"].as_array().ok_or_else(fail)?;if rows.is_empty()||rows.len()>COHORT_ROWS{return Err(fail());}
    let mut ids=HashSet::new();for row in rows {exact(row,&["operationId","attemptId","permitId","settledPermitSha256","settledAt","cessationEvidenceSha256"])?;
        if !ids.insert(text(&row["operationId"])?) {return Err(fail());}text(&row["attemptId"])?;text(&row["permitId"])?;stamp(&row["settledAt"])?;hash(&row["settledPermitSha256"])?;hash(&row["cessationEvidenceSha256"])?;
        if row["cessationEvidenceSha256"]!=e["input"]["sha256"]{return Err(fail());}}
    Ok(())
}
fn start_record(c:&Value)->ApiResult<()> {
    exact(c,&["id","action","refId","createdAt","owner","evidence"])?;let id=opaque(&c["refId"])?;
    if c["id"]!=format!("{START_PREFIX}{id}")||c["action"]!="connection.predecessor_start_consumed"{return Err(fail());}stamp(&c["createdAt"])?;crate::runtime_lifecycle::parse_token(&c["owner"])?;
    let e=&c["evidence"];exact(e,&["schemaVersion","importId","input","importRecordSha256","importReceipt","nextStartIntent","startupAdmission","preLedgerSha256","scope","package","actualProcess"])?;
    if e["schemaVersion"]!=1||e["importId"]!=c["refId"]{return Err(fail());}for k in ["input","importReceipt","nextStartIntent","startupAdmission"]{pin(&e[k])?;}hash(&e["importRecordSha256"])?;hash(&e["preLedgerSha256"])?;scope(&e["scope"])?;package(&e["package"],&c["owner"])?;
    exact(&e["actualProcess"],&["pid","birthFileTime"])?;if e["actualProcess"]["pid"].as_u64().is_none_or(|n|n==0||n>u32::MAX as u64)||text(&e["actualProcess"]["birthFileTime"])?.parse::<u64>().ok().is_none_or(|n|n==0){return Err(fail());}Ok(())
}
fn launch_record(l:&Value)->ApiResult<()> {
    exact(l,&["id","action","refId","createdAt","owner","evidence"])?;let id=opaque(&l["refId"])?;
    if l["id"]!=format!("{LAUNCH_PREFIX}{id}")||l["action"]!="connection.predecessor_launch_started"{return Err(fail());}stamp(&l["createdAt"])?;crate::runtime_lifecycle::parse_token(&l["owner"])?;
    let e=&l["evidence"];exact(e,&["schemaVersion","importId","scope","package","launchIntent","lifecycleAdmission","preLaunchBaseline","preLedgerSha256","actualProcess"])?;
    if e["schemaVersion"]!=1||e["importId"]!=l["refId"]||e["scope"]["account"]!=l["owner"]["account"]{return Err(fail());}
    scope(&e["scope"])?;package(&e["package"],&l["owner"])?;for k in ["launchIntent","lifecycleAdmission","preLaunchBaseline"]{pin(&e[k])?;}hash(&e["preLedgerSha256"])?;
    exact(&e["actualProcess"],&["pid","birthFileTime"])?;
    let birth=text(&e["actualProcess"]["birthFileTime"])?;let native=birth.parse::<u64>().map_err(|_|fail())?;
    if e["actualProcess"]["pid"].as_u64().is_none_or(|n|n==0||n>u32::MAX as u64)||native==0||native.to_string()!=birth{return Err(fail());}Ok(())
}
pub(super) fn launch_records(d:&Value)->ApiResult<Vec<&Value>> {
    let mut rows=vec![];let mut ids=HashSet::new();
    for row in d["audit"].as_array().ok_or_else(fail)? {if row["id"].as_str().is_some_and(|s|s.starts_with(LAUNCH_PREFIX))||row["action"]=="connection.predecessor_launch_started"{
        launch_record(row)?;if !ids.insert(text(&row["id"])?) {return Err(fail());}rows.push(row);
    }}Ok(rows)
}
pub(crate) fn guard_untracked_startup(d:&Value,identity:&crate::runtime_lifecycle::RuntimeIdentity)->ApiResult<()> {
    for l in launch_records(d)? {let owner=crate::runtime_lifecycle::parse_token(&l["owner"])?;
        if owner.account==identity.account&&owner.runtime_id==identity.runtime_id&&owner.release_sha256==identity.release_sha256{return Err(fail());}
    }Ok(())
}
fn reserved(d:&Value)->ApiResult<(Vec<&Value>,Vec<&Value>)> {
    launch_records(d)?;
    let mut imports=vec![];let mut starts=vec![];let mut seen=HashSet::new();
    for (ordinal,r) in d["audit"].as_array().ok_or_else(fail)?.iter().enumerate(){let id=r["id"].as_str().unwrap_or("");let action=r["action"].as_str().unwrap_or("");
        if id.starts_with(IMPORT_PREFIX)||action=="connection.predecessor_imported" {import_record(r)?;if r["evidence"]["auditOrdinal"].as_u64()!=Some(ordinal as u64)||!seen.insert(id){return Err(fail());}imports.push(r);}
        else if id.starts_with(START_PREFIX)||action=="connection.predecessor_start_consumed" {start_record(r)?;if !seen.insert(id){return Err(fail());}starts.push(r);}
    }
    for c in &starts {let r=imports.iter().find(|r|r["refId"]==c["refId"]).ok_or_else(fail)?;
        if c["owner"]!=r["owner"]||c["evidence"]["input"]!=r["evidence"]["input"]||c["evidence"]["nextStartIntent"]!=r["evidence"]["nextStartIntent"]||c["evidence"]["scope"]!=r["evidence"]["scope"]||c["evidence"]["package"]!=r["evidence"]["package"]||c["evidence"]["importRecordSha256"]!=digest(r){return Err(fail());}}
    Ok((imports,starts))
}
pub(crate) fn guard_startup(d:&Value,recovery:bool)->ApiResult<()> {
    let (imports,starts)=reserved(d)?;let pending:Vec<_>=imports.iter().filter(|r|!starts.iter().any(|c|c["refId"]==r["refId"])).collect();
    if pending.len()>1||(!recovery&&!pending.is_empty())||(recovery&&pending.len()!=1){return Err(fail());}Ok(())
}
fn result(d:&Value,m:&Value,input_pin:&Value)->ApiResult<Value> {
    guard_startup(d,true)?;
    let (imports,starts)=reserved(d)?;let r=*imports.iter().find(|r|r["refId"]==m["importId"]).ok_or_else(fail)?;
    if starts.iter().any(|c|c["refId"]==m["importId"]){return Err(fail());}
    let e=&r["evidence"];if r["owner"]!=m["owner"]||e["input"]!=*input_pin||e["scope"]!=m["scope"]||e["package"]!=m["package"]||e["nextStartIntent"]!=m["nextStartIntent"]||e["preLedgerSha256"]!=m["expectedState"]["ledgerSha256"]||e["preLifecycleSha256"]!=m["expectedState"]["lifecycleSha256"]||e["preGateSha256"]!=m["expectedState"]["gateSha256"]||e["auditOrdinal"]!=m["expectedState"]["auditCount"]||e["cohortSha256"]!=digest(&m["cohort"])||e["predecessorProofSha256"]!=digest(&m["predecessorProof"]){return Err(fail());}
    workspace_scope(d,&m["scope"],&m["owner"])?;
    if !cohort(d,&m["owner"])?.as_array().unwrap().is_empty()||digest(&d["runtimeLifecycle"])!=text(&e["preLifecycleSha256"])?||digest(&d[crate::connection_gate::FIELD])!=text(&e["gateAfterSha256"])?{return Err(fail());}
    let expected=m["cohort"].as_array().ok_or_else(fail)?;let settled=e["settledRows"].as_array().unwrap();if settled.len()!=expected.len(){return Err(fail());}
    for (frozen,original) in settled.iter().zip(expected) {for k in ["operationId","attemptId","permitId"]{if frozen[k]!=original[k]{return Err(fail());}}
        let op=crate::row(d,"operations",text(&frozen["operationId"])?)?;let p=&op[crate::connection_gate::PERMIT_FIELD];
        if !crate::connection_gate::valid_permit(op)||p["phase"]!="transport_settled"||p["cessation"]["kind"]!="contained"||p["cessation"]["evidenceSha256"]!=input_pin["sha256"]||p["settledAt"]!=frozen["settledAt"]||digest(p)!=text(&frozen["settledPermitSha256"])?{return Err(fail());}}
    let mut transition=d.clone();let ordinal=e["auditOrdinal"].as_u64().unwrap() as usize;
    let rows=transition["audit"].as_array_mut().ok_or_else(fail)?;if rows.get(ordinal)!=Some(r){return Err(fail());}rows.remove(ordinal);
    if crate::runtime_lifecycle::ledger_digest(&transition)?!=text(&e["transitionLedgerSha256"])?{return Err(fail());}
    Ok(json!({"schemaVersion":1,"kind":"native-predecessor-transport-import-result","importId":m["importId"],"input":input_pin,"nextStartIntent":m["nextStartIntent"],"scope":m["scope"],"owner":m["owner"],"package":m["package"],"importRecordSha256":digest(r),"postLedgerSha256":crate::runtime_lifecycle::ledger_digest(d)?,"postLifecycleSha256":digest(&d["runtimeLifecycle"]),"settledRows":e["settledRows"],"gateAfterSha256":e["gateAfterSha256"],"committedAt":r["createdAt"],"dispatchAuthorized":false,"retryAuthorized":false}))
}

thread_local!{static RESERVED_AUTH:RefCell<Option<Vec<Value>>>=const{RefCell::new(None)};}
/// Reserved rows are authorized by exact bytes only while this synchronous
/// validation runs. No async gap, bool override, HTTP input or nested authority.
fn validate_private_transition(before:&Value,after:&Value)->ApiResult<()> {
    let audit=after["audit"].as_array().ok_or_else(fail)?;let old=before["audit"].as_array().ok_or_else(fail)?;
    if audit.len()<=old.len()||audit.len()>old.len()+2||&audit[..old.len()]!=old.as_slice(){return Err(fail());}
    let appended=&audit[old.len()..];
    if appended.len()==2 {let c=&appended[0];let l=&appended[1];start_record(c)?;launch_record(l)?;
        if c["owner"]!=l["owner"]||c["evidence"]["actualProcess"]!=l["evidence"]["actualProcess"]||c["evidence"]["startupAdmission"]!=l["evidence"]["lifecycleAdmission"]||c["evidence"]["preLedgerSha256"]!=l["evidence"]["preLedgerSha256"]{return Err(fail());}
    }else{let r=&appended[0];match r["action"].as_str(){Some("connection.predecessor_imported")=>import_record(r)?,Some("connection.predecessor_start_consumed")=>start_record(r)?,Some("connection.predecessor_launch_started")=>launch_record(r)?,_=>return Err(fail())}}
    struct Reset;impl Drop for Reset{fn drop(&mut self){RESERVED_AUTH.with(|r|*r.borrow_mut()=None);}}
    RESERVED_AUTH.with(|r|{let mut r=r.borrow_mut();if r.is_some(){return Err(fail());}*r=Some(appended.to_vec());Ok(())})?;let _reset=Reset;
    crate::db_guards::validate_change(before,after)
}
/// Only the verified importer/startup can produce this exact guarded change.
pub(crate) struct AuthorizedTransition{before_sha256:String,after:Value,output:Value}
impl AuthorizedTransition {
    fn new(before:&Value,after:Value,output:Value)->ApiResult<Self>{let change=Self{before_sha256:digest(before),after,output};change.validate(before)?;Ok(change)}
    pub(crate) fn validate(&self,before:&Value)->ApiResult<()> {if digest(before)!=self.before_sha256{return Err(fail());}validate_private_transition(before,&self.after) }
    pub(crate) fn workspace(&self)->&Value{&self.after}
    pub(crate) fn output(&self)->Value{self.output.clone()}
}

pub(crate) struct VerifiedStartup {import:VerifiedImport,q:Value,q_pin:Value,a_pin:Value}
impl VerifiedStartup {
    pub(crate) fn from_admission(a:&Value,a_pin:&Value)->ApiResult<Self> {
        pin(a_pin)?;let mut held=io::Pins::new();if held.json(a_pin,65536)?!=*a{return Err(fail());}
        let s=&a["startup"];exact(s,&["kind","receiptSha256","expectedLedgerSha256","importReceipt","nextStartIntent"])?;
        if s["kind"]!="same-owner-predecessor-recovery"||s["receiptSha256"]!=s["importReceipt"]["sha256"]{return Err(fail());}
        let q_pin=s["importReceipt"].clone();let q=held.json(&q_pin,1048576)?;
        exact(&q,&["schemaVersion","kind","importId","input","nextStartIntent","scope","owner","package","importRecordSha256","postLedgerSha256","postLifecycleSha256","settledRows","gateAfterSha256","committedAt","dispatchAuthorized","retryAuthorized"])?;
        if q["schemaVersion"]!=1||q["kind"]!="native-predecessor-transport-import-result"||q["dispatchAuthorized"]!=false||q["retryAuthorized"]!=false||q["nextStartIntent"]!=s["nextStartIntent"]||q["postLedgerSha256"]!=s["expectedLedgerSha256"]{return Err(fail());}
        let budget=Budget::new();let import=VerifiedImport::from_file(&q["input"],&budget)?;
        evidence::startup_fixed_owner(a,import.input())?;
        if q["importId"]!=import.input()["importId"]||q["scope"]!=import.input()["scope"]||q["owner"]!=import.input()["owner"]||q["package"]!=import.input()["package"]||q["nextStartIntent"]!=import.input()["nextStartIntent"]{return Err(fail());}
        // Move A/Q held files into the verified import's lifetime.
        let mut import=import;import._pins.append(held);
        Ok(Self{import,q,q_pin,a_pin:a_pin.clone()})
    }
    pub(crate) fn scope(&self)->&Value{&self.import.input()["scope"]}
    pub(crate) fn transition(&self,before:&Value)->ApiResult<AuthorizedTransition> {
        self.transition_for_process(before,io::actual_process()?)
    }
    fn transition_for_process(&self,before:&Value,actual_process:Value)->ApiResult<AuthorizedTransition> {
        guard_startup(before,true)?;let q=self.import.reconcile(before)?;if q!=self.q{return Err(fail());}
        if before[crate::connection_gate::FIELD]["state"]!="blocked"||before[crate::connection_gate::FIELD]["availability"]["state"]!="unverified"{return Err(fail());}
        let mut after=before.clone();let record=json!({"id":format!("{START_PREFIX}{}",text(&q["importId"])?),"action":"connection.predecessor_start_consumed","refId":q["importId"],"createdAt":crate::now(),"owner":q["owner"],"evidence":{
            "schemaVersion":1,"importId":q["importId"],"input":q["input"],"importRecordSha256":q["importRecordSha256"],"importReceipt":self.q_pin,"nextStartIntent":q["nextStartIntent"],"startupAdmission":self.a_pin,"preLedgerSha256":q["postLedgerSha256"],"scope":q["scope"],"package":q["package"],"actualProcess":actual_process}});
        after["audit"].as_array_mut().ok_or_else(fail)?.push(record);AuthorizedTransition::new(before,after,q["owner"].clone())
    }
}

/// Native launch origin. The env pins are checked before startup writes, and
/// the complete pinned zero-armed baseline is rechecked inside its writer.
pub(crate) struct VerifiedLaunch{intent:Value,intent_pin:Value,baseline:Value,_pins:io::Pins}
impl VerifiedLaunch {
    pub(crate) fn from_environment(a:&Value,a_pin:&Value)->ApiResult<Option<Self>> {
        let path=std::env::var("COMMUNITYHERO_PREDECESSOR_LAUNCH_INTENT_PATH").ok();let sha=std::env::var("COMMUNITYHERO_PREDECESSOR_LAUNCH_INTENT_SHA256").ok();
        let (path,sha)=match (path,sha){(None,None)=>return Ok(None),(Some(p),Some(s))=>(p,s),_=>return Err(fail())};
        let mut held=io::Pins::new();let p=json!({"path":path,"sha256":sha});let intent=held.json(&p,65536)?;
        exact(&intent,&["schemaVersion","kind","importId","scope","owner","package","preLaunchBaseline","lifecycleAdmission"])?;common(&intent)?;
        if intent["kind"]!="root-reviewed-native-predecessor-launch-intent"||intent["lifecycleAdmission"]!=*a_pin{return Err(fail());}
        if !matches!(a["startup"]["kind"].as_str(),Some("same-owner-recovery"|"same-owner-predecessor-recovery")){return Err(fail());}
        evidence::startup_fixed_owner(a,&intent)?;
        let baseline=held.json(&intent["preLaunchBaseline"],WORKSPACE_BYTES)?;validate_capture(&baseline,&intent["scope"],&intent["owner"],&intent["package"])?;
        if !baseline["cohort"].as_array().unwrap().is_empty()||a["startup"]["expectedLedgerSha256"]!=baseline["ledgerSha256"]{return Err(fail());}
        let url=std::env::var("COMMUNITYHERO_DATABASE_URL").map_err(|_|fail())?;
        use std::str::FromStr;let options=sqlx::postgres::PgConnectOptions::from_str(&url).map_err(|_|fail())?;let s=&intent["scope"]["storage"];let host=match options.get_host(){"[::1]"=>"::1",h=>h};
        if s["host"]!=host||s["port"].as_u64()!=Some(options.get_port() as u64)||s["database"].as_str()!=options.get_database()||s["user"]!=options.get_username()||std::env::var("COMMUNITYHERO_ACCOUNT").ok().as_deref()!=intent["scope"]["companyId"].as_str()||std::env::var("COMMUNITYHERO_DATA_DIR").ok().as_deref()!=s["dataDir"].as_str(){return Err(fail());}
        Ok(Some(Self{intent,intent_pin:p,baseline,_pins:held}))
    }
    pub(crate) fn scope(&self)->&Value{&self.intent["scope"]}
    pub(crate) fn validate_workspace(&self,d:&Value)->ApiResult<()> {
        if capture(d,&self.intent["scope"],&self.intent["owner"],&self.intent["package"])?!=self.baseline{return Err(fail());}Ok(())
    }
    fn append_for_process(&self,before:&Value,after:&mut Value,actual_process:Value)->ApiResult<()> {
        self.validate_workspace(before)?;let id=text(&self.intent["importId"])?;
        if launch_records(before)?.iter().any(|r|r["refId"]==id)||crate::runtime_lifecycle::status(after)?["owner"]!=self.intent["owner"]{return Err(fail());}
        let row=json!({"id":format!("{LAUNCH_PREFIX}{id}"),"action":"connection.predecessor_launch_started","refId":id,"createdAt":crate::now(),"owner":self.intent["owner"],"evidence":{
            "schemaVersion":1,"importId":id,"scope":self.intent["scope"],"package":self.intent["package"],"launchIntent":self.intent_pin,"lifecycleAdmission":self.intent["lifecycleAdmission"],"preLaunchBaseline":self.intent["preLaunchBaseline"],"preLedgerSha256":self.baseline["ledgerSha256"],"actualProcess":actual_process}});
        launch_record(&row)?;if row.to_string().len()>1048576{return Err(fail());}after["audit"].as_array_mut().ok_or_else(fail)?.push(row);Ok(())
    }
    pub(crate) fn transition(&self,before:&Value,admission:&crate::runtime_lifecycle_startup::Admission,predecessor:Option<&VerifiedStartup>)->ApiResult<AuthorizedTransition> {
        self.validate_workspace(before)?;let actual=io::actual_process()?;
        let (mut after,owner)=match predecessor {Some(proof)=>{let change=proof.transition_for_process(before,actual.clone())?;(change.workspace().clone(),change.output())},None=>{
            let mut after=before.clone();let owner=admission.initialize_workspace_with_launch(&mut after,self)?;
            let owner=json!({"account":owner.account,"runtimeId":owner.runtime_id,"releaseSha256":owner.release_sha256,"epoch":owner.epoch});(after,owner)
        }};
        self.append_for_process(before,&mut after,actual)?;AuthorizedTransition::new(before,after,owner)
    }
}
pub(crate) fn validate_reserved_change(before:&Value,after:&Value)->ApiResult<()> {
    if before.get("audit").is_none()&&after.get("audit").is_none(){return Ok(());}
    let (old_imports,old_starts)=reserved(before)?;let (new_imports,new_starts)=reserved(after)?;
    let old:Vec<_>=old_imports.into_iter().chain(old_starts).chain(launch_records(before)?).collect();let new:Vec<_>=new_imports.into_iter().chain(new_starts).chain(launch_records(after)?).collect();
    for row in &old {if !new.contains(row){return Err(fail());}}
    let additions:Vec<_>=after["audit"].as_array().ok_or_else(fail)?.iter().filter(|r|new.contains(r)&&!old.contains(r)).cloned().collect();
    if additions.is_empty(){return Ok(());}if additions.len()>2||!RESERVED_AUTH.with(|r|r.borrow().as_ref()==Some(&additions)){return Err(fail());}Ok(())
}

#[cfg(test)] #[path="predecessor_recovery_tests.rs"] mod tests;
