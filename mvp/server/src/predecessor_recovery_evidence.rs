use super::*;
use std::{collections::{HashMap,HashSet},path::{Path,PathBuf}};
use super::io::Pins;

fn path(v:&Value)->ApiResult<&Path>{Ok(Path::new(text(v)?))}
fn resolve(value:&Value,base:&Path)->ApiResult<PathBuf>{let p=path(value)?;if p.components().any(|c|matches!(c,std::path::Component::ParentDir)){return Err(fail());}Ok(if p.is_absolute(){p.to_owned()}else{base.join(p)})}
pub(super) fn verify_package(package:&Value,pins:&mut Pins)->ApiResult<Value> {
    let core=crate::runtime_lifecycle_startup::verify_core(&package["core"],Path::new(""),true)?;
    pins.verify(&package["core"],16777216)?;pins.verify(&package["binary"],1073741824)?;
    let base=path(&package["core"]["path"])?.parent().ok_or_else(fail)?;
    if !io::same_path(&resolve(&core["binary"],base)?,path(&package["binary"]["path"])?)||core["binarySha256"]!=package["binary"]["sha256"]||core["sourceCheckpoint"]!=package["sourceCheckpoint"]{return Err(fail());}
    let runtime=resolve(&core["runtimeRoot"],base)?;
    for asset in core["assets"].as_array().ok_or_else(fail)? {pins.verify(&json!({"path":resolve(&asset["path"],&runtime)?.to_string_lossy(),"sha256":asset["sha256"]}),268435456)?;}
    let checkpoint=pins.json(&package["sourceCheckpoint"],16777216)?;
    if checkpoint["kind"]!="unpromoted-source-checkpoint"||checkpoint["fileCount"].as_u64()!=checkpoint["files"].as_array().map(|a|a.len() as u64)||checkpoint["releaseAdmitted"]!=false||checkpoint["deployed"]!=false{return Err(fail());}
    let root=path(&checkpoint["root"])?;if !root.is_absolute(){return Err(fail());}
    let files=checkpoint["files"].as_array().ok_or_else(fail)?;if files.is_empty()||files.len()>8192{return Err(fail());}
    let mut seen=HashSet::new();for f in files {exact(f,&["path","sha256","bytes"])?;let name=text(&f["path"])?;
        if !seen.insert(name.to_ascii_lowercase())||Path::new(name).is_absolute()||name.contains('\\')||name.split('/').any(|p|p.is_empty()||p=="."||p=="..")||!name.starts_with("mvp/")&&!name.starts_with("project/"){return Err(fail());}
        let size=f["bytes"].as_u64().filter(|n|*n<=268435456).ok_or_else(fail)?;let source=json!({"path":root.join(name).to_string_lossy(),"sha256":f["sha256"]});pins.verify(&source,size)?;
    }Ok(checkpoint)
}
fn verify_validator(m:&Value,pins:&mut Pins,cp:&Value)->ApiResult<()> {
    let v=&m["validator"];let source=pins.json(&v["sourcePins"],16777216)?;
    exact(&source,&["schemaVersion","kind","files"])?;
    if source["schemaVersion"]!=1||source["kind"]!="native-predecessor-validator-source-pins"{return Err(fail());}
    let compiled=cp["compiledSources"].as_array().filter(|a|!a.is_empty()).ok_or_else(fail)?;let rows=source["files"].as_array().ok_or_else(fail)?;
    if rows.len()!=compiled.len(){return Err(fail());}
    let root=path(&cp["root"])?;for (name,row) in compiled.iter().zip(rows){pin(row)?;let name=text(name)?;let f=cp["files"].as_array().unwrap().iter().find(|f|f["path"]==name).ok_or_else(fail)?;
        if row["sha256"]!=f["sha256"]||!io::same_path(path(&row["path"])?,&root.join(name)){return Err(fail());}pins.verify(row,268435456)?;}
    let review=pins.json(&v["review"],1048576)?;exact(&review,&["schemaVersion","kind","status","blockingFindings","binary","sourceCheckpoint","sourcePins","contractSha256"])?;
    if review["schemaVersion"]!=1||review["kind"]!="root-reviewed-predecessor-validator"||review["status"]!="passed"||review["blockingFindings"]!=0||review["binary"]!=v["binary"]||review["sourceCheckpoint"]!=v["sourceCheckpoint"]||review["sourcePins"]!=v["sourcePins"]||review["contractSha256"]!="a1d3ebaf5e84b1e112ec376ab217600a5923d5cbb78cf290cf45d31b12942f09"{return Err(fail());}Ok(())
}
fn fixed_owner(admission:&Value,m:&Value)->ApiResult<()> {
    let owner=crate::runtime_lifecycle::parse_token(&m["owner"])?;
    if admission["schemaVersion"]!=1||admission["kind"]!="root-reviewed-native-runtime-lifecycle-startup"||admission["installedCore"]!=m["package"]["core"]||admission["fixedOwner"]!=json!({"account":owner.account,"runtimeId":owner.runtime_id,"releaseSha256":owner.release_sha256}) {return Err(fail());}Ok(())
}
pub(super) fn startup_fixed_owner(admission:&Value,m:&Value)->ApiResult<()> {fixed_owner(admission,m)}
pub(super) fn verify(m:&Value,input_pin:&Value,budget:&Budget)->ApiResult<Pins> {
    let mut pins=Pins::new();if pins.json(input_pin,1048576)?!=*m{return Err(fail());}let cp=verify_package(&m["package"],&mut pins)?;budget.verification()?;verify_validator(m,&mut pins,&cp)?;
    let f=pins.json(&m["nextStartIntent"],65536)?;exact(&f,&["schemaVersion","kind","importId","scope","owner","package"])?;common(&f)?;
    if f["kind"]!="root-reviewed-predecessor-next-start-intent"||["importId","scope","owner","package"].iter().any(|k|f[*k]!=m[*k]){return Err(fail());}
    let proof=&m["predecessorProof"];let baseline=pins.json(&proof["preLaunchBaseline"],WORKSPACE_BYTES)?;let stopped=pins.json(&proof["subjectCohortCapture"],WORKSPACE_BYTES)?;
    verify_captures(m,&baseline,&stopped)?;
    let admission=pins.json(&proof["lifecycleAdmission"],65536)?;fixed_owner(&admission,m)?;
    if admission["startup"]["expectedLedgerSha256"]!=baseline["ledgerSha256"] {return Err(fail());}
    let intent=pins.json(&proof["launchIntent"],65536)?;exact(&intent,&["schemaVersion","kind","importId","scope","owner","package","preLaunchBaseline","lifecycleAdmission"])?;
    common(&intent)?;if intent["kind"]!="root-reviewed-native-predecessor-launch-intent"||["importId","scope","owner","package"].iter().any(|k|intent[*k]!=m[*k])||intent["preLaunchBaseline"]!=proof["preLaunchBaseline"]||intent["lifecycleAdmission"]!=proof["lifecycleAdmission"]{return Err(fail());}
    let request=pins.json(&proof["launchRequest"],1048576)?;
    exact(&request,&["kind","schemaVersion","role","caseInput","executable","args","workingDirectory","environment","outputPrefix","controlPath","timeoutMs","outputCapBytes","observer"])?;
    verify_artifact_paths(proof,&request)?;
    if request["kind"]!="root-owned-baw-e2e-process-input"||request["schemaVersion"]!=1||request["role"]!="server"||request["executable"]!=m["package"]["binary"]||request["args"]!=json!([])||request["outputCapBytes"]!=1048576||request["timeoutMs"].as_u64().is_none_or(|n|n<1000||n>1560000){return Err(fail());}
    let env=request["environment"].as_object().ok_or_else(fail)?;
    for (key,p) in [("COMMUNITYHERO_LIFECYCLE_ADMISSION",&proof["lifecycleAdmission"]),("COMMUNITYHERO_PREDECESSOR_LAUNCH_INTENT",&proof["launchIntent"])] {
        if env.get(&format!("{key}_PATH"))!=Some(&p["path"])||env.get(&format!("{key}_SHA256"))!=Some(&p["sha256"]){return Err(fail());}}
    if request["environment"]["COMMUNITYHERO_ACCOUNT"]!=m["scope"]["companyId"]||request["environment"]["COMMUNITYHERO_DATA_DIR"]!=m["scope"]["storage"]["dataDir"]{return Err(fail());}
    let expected=&proof["observer"];exact(&request["observer"],&["host","script","assembly","assemblyBuild","sourceReview","preflight"])?;
    let canonical_observer=cp["files"].as_array().unwrap().iter().find(|f|f["path"]=="project/scripts/engine-release/e1-owned-process-observer.cs").ok_or_else(fail)?;
    for key in ["script","assembly","assemblyBuild","sourceReview","preflight"]{if request["observer"][key]!=expected[key]{return Err(fail());}}
    pins.verify(&request["observer"]["host"],268435456)?;
    let case=pins.json(&request["caseInput"],1048576)?;
    if case["kind"]!="root-bounded-baw-e2e-case"||case["account"]!=m["scope"]["companyId"]||case["binary"]["path"]!=m["package"]["binary"]["path"]||case["binary"]["sha256"]!=m["package"]["binary"]["sha256"]{return Err(fail());}
    for key in ["source","script","assembly"]{pins.verify(&expected[key],268435456)?;}
    let build=pins.json(&expected["assemblyBuild"],1048576)?;let review=pins.json(&expected["sourceReview"],1048576)?;let preflight=pins.json(&expected["preflight"],1048576)?;
    if build["kind"]!="e1-observer-assembly-build"||build["status"]!="passed"||build["exitCode"]!=0||build["assembly"]!=expected["assembly"]||build["source"]!=expected["source"]||review["kind"]!="e1-observer-source-review"||review["status"]!="passed"||review["blockingFindings"]!=0||review["source"]!=expected["source"]||review["controlledScript"]!=expected["script"]||preflight["kind"]!="e1-observer-preflight-assertion"||preflight["status"]!="passed"||preflight["negativeChildDetected"]!=true||preflight["helper"]!=expected["assembly"]||preflight["assemblyBuild"]!=expected["assemblyBuild"]||preflight["review"]!=expected["sourceReview"]{return Err(fail());}
    verify_observer_generation_pins(canonical_observer,&build["baseSource"],&build["source"])?;
    let base=pins.bytes(&build["baseSource"],4194304,true)?;let generated=pins.bytes(&build["source"],4194304,true)?;verify_deadline_delta(&base,&generated)?;
    for key in ["compiler","stdout","stderr","bootstrapScript","node","builderInput","review"]{pins.verify(&build[key],268435456)?;}
    for key in ["references","runtimeToolPins","toolSourcePins"]{let rows=build[key].as_array().filter(|a|!a.is_empty()&&a.len()<=256).ok_or_else(fail)?;for p in rows{pins.verify(p,268435456)?;}}
    let compiler_raw=pins.json(&build["observation"],4194304)?;let compiler_request=pins.json(&build["request"],1048576)?;
    if compiler_raw["kind"]!="root-owned-native-job-outcome"||compiler_raw["status"]!="completed"||!compiler_raw["failure"].is_null()||compiler_raw["exitCode"]!=0||compiler_raw["rootExited"]!=true||compiler_raw["assignedBeforeResume"]!=true||compiler_raw["resumed"]!=true||compiler_raw["cleanupComplete"]!=true||compiler_raw["timedOut"]!=false||compiler_raw["outputCapExceeded"]!=false||compiler_raw["descendantsTerminated"]!=false||compiler_raw["final"]["activeProcesses"]!=0||compiler_raw["executableSha256"]!=build["compiler"]["sha256"]||compiler_request["executableSha256"]!=build["compiler"]["sha256"]{return Err(fail());}
    if !io::same_path(path(&compiler_raw["executable"])?,path(&build["compiler"]["path"])?)||!io::same_path(path(&compiler_raw["actualImage"])?,path(&build["compiler"]["path"])?)||!io::same_path(path(&compiler_request["executable"])?,path(&build["compiler"]["path"])?){return Err(fail());}
    let mut compiler_args=vec![json!("/nologo"),json!("/noconfig"),json!("/nostdlib+"),json!("/target:library"),json!("/optimize+"),json!(format!("/out:{}",text(&expected["assembly"]["path"])?))];
    for p in build["references"].as_array().unwrap(){compiler_args.push(json!(format!("/reference:{}",text(&p["path"])?)));}compiler_args.push(build["source"]["path"].clone());
    if compiler_request["args"]!=json!(compiler_args){return Err(fail());}
    // The actual preflight raw receipt must remain available; a summary alone
    // cannot establish that the reviewed observer detects a short-lived child.
    let pf=pins.json(&preflight["rawObservation"],4194304)?;
    if pf["kind"]!="e1-owned-process-observer-raw"||pf["status"]!="captured"||pf["mode"]!="preflight"||pf["helper"]!=expected["assembly"]||pf["assemblyBuild"]!=expected["assemblyBuild"]||pf["review"]!=expected["sourceReview"]{return Err(fail());}
    if review["script"]!=pf["observerScript"]{return Err(fail());}pins.verify(&pf["observerScript"],4194304)?;
    let pfr=pf["records"].as_array().filter(|a|a.len()==2).ok_or_else(fail)?;
    for (n,row) in pfr.iter().enumerate(){pins.verify(&row["executable"],268435456)?;let raw=pins.json(&row["observation"],4194304)?;verify_tree(&raw)?;
        if raw["final"]["totalProcesses"].as_u64()!=Some(if n==0{1}else{2})||raw["exitCode"]!=0||raw["executableSha256"]!=row["executable"]["sha256"]||raw["status"]!=if n==0{"passed"}else{"failed"}||n==1&&raw["failureCode"]!="unexpected_descendant"{return Err(fail());}}
    let identity=pins.json(&proof["identity"],65536)?;
    let raw=pins.json(&proof["rawObservation"],4194304)?;
    let control=if proof["control"].is_null(){None}else{Some(pins.json(&proof["control"],65536)?)};
    // First contract supports the reviewed controlled inner observer only.
    let control=control.as_ref().ok_or_else(fail)?;
    verify_subject(&raw,&identity,control,&proof["control"],&request,&m["package"]["binary"])?;
    verify_occurrence(m,&baseline,&stopped,&raw)?;
    budget.verification()?;Ok(pins)
}
// Task130 nullable stop-parent / DETACHED generation: earlier receipts are not reusable.
pub(super) fn verify_observer_generation_pins(canonical:&Value,base:&Value,generated:&Value)->ApiResult<()> {
    if base["sha256"]!=canonical["sha256"]||canonical["sha256"]!="4934332b205253c89bd4236fa90a26a2bfbbf59c7df57ea635c7c23102e86328"||generated["sha256"]!="1ae13f1e753abf7dee01bdc3f1605720fc765e17db614a234357a334000c6d49"{return Err(fail());}Ok(())
}
pub(super) fn verify_deadline_delta(base:&[u8],generated:&[u8])->ApiResult<()> {
    let base=std::str::from_utf8(base).map_err(|_|fail())?;let generated=std::str::from_utf8(generated).map_err(|_|fail())?;
    let from="timeoutMs <= 300000 && outputCap";let to="timeoutMs <= (allowDescendants ? 1560000 : 300000) && outputCap";
    if base.matches(from).count()!=1||base.replace(from,to)!=generated{return Err(fail());}Ok(())
}
pub(super) fn verify_artifact_paths(proof:&Value,request:&Value)->ApiResult<()> {
    let prefix=text(&request["outputPrefix"])?;if prefix.len()>4096||!Path::new(prefix).is_absolute(){return Err(fail());}
    for (key,suffix) in [("launchRequest",".input.json"),("identity",".identity.json"),("rawObservation",".observation.json"),("control",".stop.json")] {
        let expected=format!("{prefix}{suffix}");if !io::same_path(path(&proof[key]["path"])?,Path::new(&expected)){return Err(fail());}
    }
    if !io::same_path(path(&request["controlPath"])?,path(&proof["control"]["path"])?){return Err(fail());}Ok(())
}
pub(super) fn verify_occurrence(m:&Value,baseline:&Value,stopped:&Value,raw:&Value)->ApiResult<()> {
    let d=&stopped["workspace"];let rows=launch_records(d)?;
    let l=*rows.iter().rev().find(|l|l["owner"]==m["owner"]).ok_or_else(fail)?;let e=&l["evidence"];let proof=&m["predecessorProof"];
    if l["refId"]!=m["importId"]||e["scope"]!=m["scope"]||e["package"]!=m["package"]||e["launchIntent"]!=proof["launchIntent"]||e["lifecycleAdmission"]!=proof["lifecycleAdmission"]||e["preLaunchBaseline"]!=proof["preLaunchBaseline"]||e["preLedgerSha256"]!=baseline["ledgerSha256"]||e["actualProcess"]["pid"]!=raw["pid"]||filetime(&e["actualProcess"]["birthFileTime"])?!=filetime(&raw["creationFileTime"])?{return Err(fail());}
    let audit=d["audit"].as_array().ok_or_else(fail)?;let ordinal=audit.iter().position(|r|r==l).ok_or_else(fail)?;let base=baseline["auditCount"].as_u64().ok_or_else(fail)? as usize;
    if ordinal==base{return Ok(());}if ordinal!=base+1{return Err(fail());}
    let c=audit.get(base).ok_or_else(fail)?;start_record(c)?;
    if c["owner"]!=l["owner"]||c["evidence"]["actualProcess"]!=e["actualProcess"]||c["evidence"]["startupAdmission"]!=e["lifecycleAdmission"]||c["evidence"]["preLedgerSha256"]!=e["preLedgerSha256"]{return Err(fail());}Ok(())
}
pub(super) fn verify_captures(m:&Value,baseline:&Value,stopped:&Value)->ApiResult<()> {
    for capture in [baseline,stopped]{validate_capture(capture,&m["scope"],&m["owner"],&m["package"])?;}
    if !baseline["cohort"].as_array().unwrap().is_empty()||stopped["cohort"]!=m["cohort"]||stopped["ledgerSha256"]!=m["expectedState"]["ledgerSha256"]||stopped["lifecycleSha256"]!=m["expectedState"]["lifecycleSha256"]||stopped["gateSha256"]!=m["expectedState"]["gateSha256"]||stopped["auditCount"]!=m["expectedState"]["auditCount"]{return Err(fail());}
    // A previous durable permit cannot be reset into this process episode.
    for row in m["cohort"].as_array().ok_or_else(fail)?{if baseline["workspace"]["operations"].as_array().unwrap().iter().any(|op|(op["id"]==row["operationId"]||op["attemptId"]==row["attemptId"])&&op.get(crate::connection_gate::PERMIT_FIELD).is_some()){return Err(fail());}}
    let old=baseline["workspace"]["audit"].as_array().ok_or_else(fail)?;let new=stopped["workspace"]["audit"].as_array().ok_or_else(fail)?;
    if !new.starts_with(old){return Err(fail());}Ok(())
}
fn integer(v:&Value)->ApiResult<u64>{v.as_u64().filter(|n|*n>0).ok_or_else(fail)}
// Preserve raw stop-parent availability; only a complete tree may correlate it.
fn event_parent(event:&Value)->ApiResult<Option<u32>> {
    let value=&event["parentPid"];
    if event["kind"]=="stop"&&value.is_null(){return Ok(None);}
    let n=value.as_u64().filter(|n|*n<=u32::MAX as u64).ok_or_else(fail)?;
    if event["kind"]=="start"&&n==0{return Err(fail());}Ok(Some(n as u32))
}
fn filetime(v:&Value)->ApiResult<u64>{if let Some(n)=v.as_u64(){return Ok(n);}text(v)?.parse::<u64>().map_err(|_|fail())}
pub(super) fn verify_tree(raw:&Value)->ApiResult<()> {
    exact(raw,&["status","failureCode","executable","executableSha256","actualImage","pid","observerPid","exitCode","creationFileTime","elapsedMs","observationCutoffFileTime","pipeReadersClosed","assignedBeforeResume","imageVerified","cleanupComplete","wmiComplete","controlledStop","controlSha256","before","suspended","final","events","wmiErrors","stdoutBytes","stderrBytes","outputCapExceeded"])?;
    for key in ["before","suspended","final"]{exact(&raw[key],&["totalProcesses","activeProcesses","totalTerminatedProcesses"])?;}
    if raw["suspended"]["totalTerminatedProcesses"]!=0||raw["exitCode"].as_u64().is_none_or(|n|n>u32::MAX as u64)||raw["elapsedMs"].as_u64().is_none() {return Err(fail());}
    for key in ["stdoutBytes","stderrBytes"]{if raw[key].as_u64().is_none_or(|n|n>1048576){return Err(fail());}}
    let total=integer(&raw["final"]["totalProcesses"])?;if total>2048||raw["final"]["activeProcesses"]!=0||raw["final"]["totalTerminatedProcesses"].as_u64().is_none_or(|n|n>total)||raw["before"]!=json!({"totalProcesses":0,"activeProcesses":0,"totalTerminatedProcesses":0})||raw["suspended"]["totalProcesses"]!=1||raw["suspended"]["activeProcesses"]!=1{return Err(fail());}
    for key in ["assignedBeforeResume","imageVerified","cleanupComplete","pipeReadersClosed","wmiComplete"]{if raw[key]!=true{return Err(fail());}}
    if raw["wmiErrors"]!=json!([])||raw["outputCapExceeded"]!=false{return Err(fail());}
    let root=integer(&raw["pid"])?;let observer=integer(&raw["observerPid"])?;if root==observer||root>u32::MAX as u64||observer>u32::MAX as u64{return Err(fail());}
    let birth=filetime(&raw["creationFileTime"])?;let cutoff=filetime(&raw["observationCutoffFileTime"])?;if birth==0||cutoff<birth{return Err(fail());}
    let events=raw["events"].as_array().ok_or_else(fail)?;if events.len()!=total as usize*2||events.len()>4096{return Err(fail());}
    let mut starts=HashMap::new();let mut stops=HashMap::new();
    for event in events {exact(event,&["kind","image","pid","parentPid","exitCode","timeCreated"])?;let pid=integer(&event["pid"])?;event_parent(event)?;text(&event["image"])?;if event["kind"]=="start"&&event["exitCode"]!=0{return Err(fail());}if pid>u32::MAX as u64||event["exitCode"].as_u64().is_none_or(|n|n>u32::MAX as u64){return Err(fail());}let at=filetime(&event["timeCreated"])?;if at<birth||at>cutoff{return Err(fail());}
        let map=match event["kind"].as_str(){Some("start")=>&mut starts,Some("stop")=>&mut stops,_=>return Err(fail())};if map.insert(pid,event).is_some(){return Err(fail());}}
    if starts.len()!=total as usize||stops.len()!=starts.len()||!starts.contains_key(&root){return Err(fail());}
    for (pid,start) in &starts {let stop=stops.get(pid).ok_or_else(fail)?;
        // A nonzero reported stop parent must agree. Null/zero are not filled
        // from start; unique PID/image/time, Job totals and the start tree bind them.
        let start_parent=event_parent(start)?;
        if event_parent(stop)?.is_some_and(|parent|parent!=0&&Some(parent)!=start_parent){return Err(fail());}
        if !text(&stop["image"])?.eq_ignore_ascii_case(text(&start["image"])?)||filetime(&stop["timeCreated"])?<filetime(&start["timeCreated"])?{return Err(fail());}
        if *pid==root {let image=text(&raw["actualImage"])?.rsplit(['\\','/']).next().ok_or_else(fail)?;
            if start["parentPid"].as_u64()!=Some(observer)||!image.eq_ignore_ascii_case(text(&start["image"])?)||stop["exitCode"]!=raw["exitCode"]{return Err(fail());}}
        else {let parent=integer(&start["parentPid"])?;let parent_start=starts.get(&parent).ok_or_else(fail)?;let parent_stop=stops.get(&parent).ok_or_else(fail)?;let at=filetime(&start["timeCreated"])?;
            if at<filetime(&parent_start["timeCreated"])?||at>filetime(&parent_stop["timeCreated"])?{return Err(fail());}}}
    let mut connected=HashSet::from([root]);loop{let count=connected.len();for (pid,start) in &starts{if start["parentPid"].as_u64().is_some_and(|n|connected.contains(&n)){connected.insert(*pid);}}if count==connected.len(){break;}}
    if connected.len()!=starts.len(){return Err(fail());}Ok(())
}
pub(super) fn verify_subject(raw:&Value,identity:&Value,control:&Value,control_pin:&Value,request:&Value,binary:&Value)->ApiResult<()> {
    exact(identity,&["kind","pid","birthFileTime","executable","executableSha256","assignedBeforeResume","imageVerified"])?;
    exact(control,&["kind","pid","birthFileTime"])?;verify_tree(raw)?;
    if raw["status"]!="passed"||!raw["failureCode"].is_null()||raw["controlledStop"]!=true||raw["exitCode"]!=0xe103||raw["controlSha256"]!=control_pin["sha256"]||identity["kind"]!="owned-suspended-process-identity"||control["kind"]!="stop-owned-process"
        ||identity["pid"]!=raw["pid"]||control["pid"]!=raw["pid"]||filetime(&identity["birthFileTime"])?!=filetime(&raw["creationFileTime"])?||control["birthFileTime"]!=identity["birthFileTime"]||identity["assignedBeforeResume"]!=true||identity["imageVerified"]!=true{return Err(fail());}
    for field in ["executable","actualImage"]{if !io::same_path(path(&raw[field])?,path(&binary["path"])?){return Err(fail());}}
    if identity["executable"]!=binary["path"]||identity["executableSha256"]!=binary["sha256"]||raw["executableSha256"]!=binary["sha256"]||control_pin["path"]!=request["controlPath"]||raw["elapsedMs"].as_u64().is_none_or(|n|n>request["timeoutMs"].as_u64().unwrap()+10000){return Err(fail());}Ok(())
}
