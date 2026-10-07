//! Exact-recipient public fact dependencies. Research is source evidence only;
//! resolving a dependency requires a new ordinary preparation, never approval.
use crate::*;
use axum::Extension;
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;

pub(crate) const CONTRACT: &str = "targeted_public_v1";
const SOURCE_CONTRACT: &str = "scoped_source_v1";
const TTL_SECONDS: i64 = 24 * 60 * 60;
const MAX_ATTEMPTS: usize = 2;
#[path="fact_worker_scope.rs"]
mod worker_scope;
pub(crate) fn worker_scope_keys(d:&Value,job:&Value,families:&std::collections::BTreeMap<String,String>)->Option<BTreeSet<String>> {
    worker_scope::keys(d,job,families)
}
pub(crate) fn validate_automatic_setting(value:&Value)->ApiResult<()> {
    if value.as_object().is_none_or(|v|v.len()!=2)||value["version"]!=1||!value["enabled"].is_boolean(){
        return Err(bad("Invalid automatic public fact configuration"));
    }Ok(())
}
pub(crate) fn automatic_enabled(d:&Value)->bool {
    d["settings"]["autoPreparation"]["publicFactFollowup"]==json!({"version":1,"enabled":true})
}
/// Captured only by new native automatic parents; historical declarations alone
/// carry no authority for another model attempt.
pub(crate) fn capture_automatic_policy(d:&Value,job:&Value)->Option<Value>{
    let bundle=&job["prepareBundle"];
    if !automatic_enabled(d)||job["purpose"]!="auto_prepare"
        ||job.get("conductorRunId").is_some()||job.get("grantGeneration").is_some()
        ||bundle["request"]["preparationMode"]!="single_pass_v1"
        ||bundle["request"]["factDependencyContract"]!=CONTRACT{return None;}
    Some(json!({"version":1,"parentJobId":job["id"],"account":d["account"],
        "connectorBinding":bundle["request"]["connectorBinding"],"bundleDigest":bundle["digest"],
        "maxResearchAttempts":1,"maxContinuationsPerDependency":1}))
}
pub(crate) fn validate_automatic_policy(d:&Value,job:&Value)->ApiResult<()> {
    let expected=capture_automatic_policy(d,job);
    if job.get("automaticFactPolicy")!=expected.as_ref(){return Err(conflict("Automatic fact policy capture changed"));}
    Ok(())
}
fn automatic_parent(d:&Value,parent:&Value)->ApiResult<()> {
    if parent["status"]!="completed"||parent["preparationStages"]["first"]["status"]!="completed"
        ||parent.get("automaticFactPolicy").is_none()
        ||parent["prepareBundle"]["digest"]!=hash(&parent["prepareBundle"]["request"])
        ||parent["prepareBundle"]["request"]["account"]!=d["account"]{
        return Err(conflict("Automatic fact parent is not a settled captured owner"));
    }
    validate_automatic_policy(d,parent)
}
// Callers validate the immutable parent once per captured group, not once per
// recipient (which would repeatedly hash the entire shared model context).
fn automatic_eligible(d:&Value,parent:&Value,entry:&Value,at:&str)->bool {
    if entry["prepareJobId"]!=parent["id"]
        ||entry["bundleDigest"]!=parent["prepareBundle"]["digest"]||entry["kind"]!="missing_public_fact"
        ||current_dependency(d,entry).is_err(){return false;}
    let Ok(item)=row(d,"items",entry["itemId"].as_str().unwrap_or("")) else{return false;};
    let outcome=&parent["prepareOutcome"];
    let held=if outcome["itemId"]==entry["itemId"]{outcome["status"]=="needs_attention"}
        else{rows(outcome,"items").iter().filter(|v|v["itemId"]==entry["itemId"]&&v["status"]=="needs_attention").count()==1};
    held&&item["autoPreparation"]["jobId"]==parent["id"]&&item["autoPreparation"]["status"]=="needs_attention"
        &&timestamp(at).is_ok_and(|at|auto_prepare::eligible(d,item,at))
        &&rows(&parent["preparationStages"]["first"]["result"],"factDependencies").contains(&entry["binding"]["declaration"])
}
#[derive(Clone,Debug,PartialEq)]
struct AutomaticSelection { parent:String, ids:Vec<Value>, dependencies:Vec<Value>, resolved:bool }
impl AutomaticSelection {
    fn narrowed(&self,ids:&[Value])->Option<Self>{
        if ids.is_empty()||ids.len()>self.ids.len(){return None;}
        let mut selected=self.clone();selected.ids.clear();selected.dependencies.clear();
        for (id,dependency) in self.ids.iter().zip(&self.dependencies){if ids.contains(id){selected.ids.push(id.clone());selected.dependencies.push(dependency.clone());}}
        (selected.ids.len()==ids.len()).then_some(selected)
    }
}
fn automatic_selection(d:&Value,at:&str,width:usize)->Option<AutomaticSelection>{
    if !automatic_enabled(d)||conductor_authority::current_context().is_some(){return None;}
    // Consume useful evidence before creating another lookup. Keep original
    // parent/group order and reuse bounded reservation filtering for aliases.
    let admission=preparation_workers::AutomaticAdmission::capture(d,width);
    for resolved in [true,false] {for parent in list(d,"jobs") {
        if automatic_parent(d,parent).is_err(){continue;}
        let eligible:Vec<_>=rows(parent,"factFollowups").iter().filter(|entry|
            automatic_eligible(d,parent,entry,at)&&entry["consumedByJobId"].is_null()
            &&if resolved{entry["status"]=="resolved"&&evidence_current(entry,at).is_ok()
                &&row(d,"items",entry["itemId"].as_str().unwrap_or("")).is_ok_and(|item|admission.permits(d,item))}
                else{entry["status"]=="pending"&&rows(entry,"attempts").is_empty()}).collect();
        let ids:Vec<String>=eligible.iter().filter_map(|e|e["itemId"].as_str().map(str::to_owned)).collect();
        let mut available=Vec::new();auto_prepare::retain_available(&ids,&mut |ids|preparation_reservations::assert_available(d,ids,None).is_ok(),&mut available);
        let entries:Vec<_>=eligible.into_iter().filter(|e|e["itemId"].as_str().is_some_and(|id|available.iter().any(|v|v==id))).collect();
        let Some(seed)=entries.first() else{continue;};let key=group_key(seed);
        let entries:Vec<_>=entries.into_iter().filter(|e|resolved||group_key(e)==key).take(100).collect();
        return Some(AutomaticSelection{parent:parent["id"].as_str()?.to_owned(),resolved,
            ids:entries.iter().map(|e|e["itemId"].clone()).collect(),dependencies:entries.iter().map(|e|e["id"].clone()).collect()});
    }}None
}
fn research_turn(d:&Value)->bool {
    // One research group yields back to fresh preparation. No drain timer or
    // resettable scheduler cursor; the original durable jobs decide the turn.
    list(d,"jobs").iter().rev().find(|j|j["kind"]=="assistant"&&(
        matches!(j["purpose"].as_str(),Some("auto_prepare"|"auto_revalidate"))
        ||j.get("automaticFactContinuation").is_some()
        ||(j["purpose"]=="public_fact_followup"&&row(d,"jobs",j["parentPrepareJobId"].as_str().unwrap_or(""))
            .is_ok_and(|p|p.get("automaticFactPolicy").is_some()))))
        .is_none_or(|j|matches!(j["purpose"].as_str(),Some("auto_prepare"|"auto_revalidate")))
}
fn active_assistant(d:&Value)->bool {list(d,"jobs").iter().any(|j|j["kind"]=="assistant"&&j["purpose"]!="discussion"
    &&matches!(j["status"].as_str(),Some("queued"|"running")))}
fn schedule_automatic_continuation(d:&mut Value,selection:&AutomaticSelection,width:usize,at:&str)->ApiResult<engine_prepare::Scheduled>{
    if !selection.resolved||automatic_selection(d,at,width).and_then(|current|current.narrowed(&selection.ids)).as_ref()!=Some(selection){return Err(conflict("Automatic fact selection changed"));}
    let admission=preparation_workers::AutomaticAdmission::capture(d,width);
    if !admission.available()||selection.ids.iter().any(|id|row(d,"items",id.as_str().unwrap_or("")).map_or(true,|i|!admission.permits(d,i))){
        return Err(conflict("Automatic fact continuation capacity unavailable"));
    }
    let parent=row(d,"jobs",&selection.parent)?;let policy=parent["automaticFactPolicy"].clone();
    let ids=selection.ids.iter().filter_map(|id|id.as_str().map(str::to_owned)).collect::<Vec<_>>();
    preparation_reservations::assert_available(d,&ids,None)?;
    let scheduled=engine_prepare::schedule_at(d,engine_prepare::Input{item_ids:ids,instruction:None},Some(at))?;
    let job=row_mut(d,"jobs",&scheduled.job_id)?;
    job["automaticFactContinuation"]=json!({"version":1,"parentPrepareJobId":selection.parent,
        "dependencyIds":selection.dependencies,"policy":policy,"admissionWidth":width});
    automatic_continuation_current(d,row(d,"jobs",&scheduled.job_id)?)?;
    Ok(scheduled)
}
fn schedule_continuation_admitted(d:&mut Value,selection:&AutomaticSelection,width:usize,at:&str,token:&crate::runtime_lifecycle::OwnerToken)->ApiResult<engine_prepare::Scheduled> {
    crate::runtime_lifecycle::require_admission(d,token,crate::runtime_lifecycle::AdmissionClass::Preparation)?;
    let scheduled=schedule_automatic_continuation(d,selection,width,at)?;
    crate::preparation_review::record_initial_admission(d,token,&scheduled.job_id,at)?;
    Ok(scheduled)
}
fn capacity_holds(d:&mut Value,selection:&AutomaticSelection,ids:&[Value],at:&str)->ApiResult<()> {
    let parent=row(d,"jobs",&selection.parent)?.clone();
    automatic_parent(d,&parent)?;
    for id in ids {
        let position=selection.ids.iter().position(|v|v==id).ok_or_else(||conflict("Foreign automatic capacity hold"))?;
        let dependency=&selection.dependencies[position];
        let entry=rows(&parent,"factFollowups").iter().find(|e|e["id"]==*dependency).ok_or_else(||conflict("Capacity dependency missing"))?;
        if !automatic_eligible(d,&parent,entry,at)||entry["status"]!="resolved"||!entry["consumedByJobId"].is_null()
            ||evidence_current(entry,&now()).is_err(){return Err(conflict("Automatic capacity hold source changed"));}
    }
    for entry in row_mut(d,"jobs",&selection.parent)?["factFollowups"].as_array_mut().ok_or_else(||conflict("Capacity dependencies missing"))? {
        if ids.contains(&entry["itemId"]) {entry["status"]=json!("held");entry["reason"]=json!("automatic_fact_capacity_exceeded");}
    }Ok(())
}
/// This controls only the existing tick's refill. It creates no second loop or
/// publisher. Proven research uses its existing chat lane; unrelated prepared
/// work may use the ordinary pool while exact dependencies remain held.
pub(crate) async fn automatic_tick(app:&App,state:&Value)->ApiResult<bool>{
    let snapshot=state.clone();let at=now();
    let width=app.preparation_workers.width();
    let Some(mut selection)=automatic_selection(&snapshot,&at,width) else{return Ok(false);};
    let token=app.lifecycle_admission_token(crate::runtime_lifecycle::AdmissionClass::Preparation).await?;
    if selection.resolved {
        let admission=preparation_workers::AutomaticAdmission::capture(&snapshot,width);
        if !admission.available(){return Ok(false);}
        let mut preview=crate::runtime_lifecycle_app::Capture::preview(app,&token,snapshot.clone(),|d|
            schedule_automatic_continuation(d,&selection,width,&at)).map(|(_,scheduled)|scheduled);
        let size=match &preview {Ok(scheduled)=>engine_prepare::capacity::require(app,scheduled.request()).await,Err(e)=>Err(ApiError(e.0,e.1.clone()))};
        if let Err(error)=size {
            if !prepare_plan::capacity_error(&error.1){return Err(error);}
            let plan=engine_prepare::capacity::refine(app,&snapshot,json!({"batches":[{"itemIds":selection.ids}],"held":[]}),None).await?;
            let held=rows(&plan,"held").iter().map(|v|v["itemId"].clone()).collect::<Vec<_>>();
            if !held.is_empty(){app.change(|d|capacity_holds(d,&selection,&held,&at)).await?;}
            let Some(ids)=rows(&plan,"batches").first().and_then(|b|b["itemIds"].as_array()) else{return Ok(false);};
            selection=selection.narrowed(ids).ok_or_else(||conflict("Capacity plan retargeted fact recipients"))?;
            let fresh=app.db.read_preparation_schedule().await?;
            preview=crate::runtime_lifecycle_app::Capture::preview(app,&token,fresh,|d|
                schedule_automatic_continuation(d,&selection,width,&at)).map(|(_,scheduled)|scheduled);
            engine_prepare::capacity::require(app,preview.as_ref().map_err(|e|ApiError(e.0,e.1.clone()))?.request()).await?;
        }
        let preview=preview?;
        let scheduled=app.change_preparation_schedule(|d|{
            let scheduled=schedule_continuation_admitted(d,&selection,width,&at,&token)?;
            engine_prepare::capacity::same_capture(preview.request(),scheduled.request())?;Ok(scheduled)
        }).await;
        let scheduled=match scheduled{Ok(job)=>job,Err(error) if error.0==StatusCode::CONFLICT=>return Ok(false),Err(error)=>return Err(error)};
        engine_prepare::spawn_preparation(app,scheduled);return Ok(false);
    }
    let (snapshot,ordinary_ready)=crate::runtime_lifecycle_app::Capture::preview(app,&token,snapshot,|d|
        if research_turn(d){Ok(false)}else{auto_prepare::ordinary_claim_ready(d,timestamp(&at).map_err(bad)?,width)})?;
    if ordinary_ready{return Ok(false);}
    if active_assistant(&snapshot){return Ok(!preparation_workers::AutomaticAdmission::capture(&snapshot,width).available());}
    let launches=app.change(|d|{
        if automatic_selection(d,&at,width).as_ref()!=Some(&selection)||active_assistant(d)
            ||(!research_turn(d)&&auto_prepare::ordinary_claim_ready(d,timestamp(&at).map_err(bad)?,app.preparation_workers.width())?){return Ok((vec![],false));}
        let (_,jobs)=schedule_research_admitted(d,&selection.parent,&selection.ids,None,&token)?;
        let exclusive=jobs.iter().any(|id|row(d,"jobs",id).map_or(true,|job|job.get("factWorkerScope").is_none()));
        Ok((jobs,exclusive))
    }).await?;
    for id in launches.0{let worker=app.clone();app.spawn(id.clone(),run(worker,id));}Ok(launches.1)
}
pub(crate) fn automatic_continuation_current(d:&Value,job:&Value)->ApiResult<()> {
    let Some(marker)=job.get("automaticFactContinuation") else{return Ok(());};
    let parent=row(d,"jobs",required(marker,"parentPrepareJobId")?)?;automatic_parent(d,parent)?;
    if marker.as_object().is_none_or(|o|o.len()!=5)||marker["version"]!=1||marker["policy"]!=parent["automaticFactPolicy"]
        ||job.get("conductorRunId").is_some()||job.get("grantGeneration").is_some()
        ||marker["admissionWidth"].as_u64().is_none_or(|v|v==0||v>preparation_workers::MAX_WORKERS as u64){return Err(conflict("Automatic fact continuation origin changed"));}
    let ids=job["selectedItemIds"].as_array().filter(|ids|!ids.is_empty()).ok_or_else(||conflict("Automatic fact recipients missing"))?;
    let wanted=marker["dependencyIds"].as_array().filter(|v|!v.is_empty()&&v.len()==ids.len()).ok_or_else(||conflict("Automatic fact dependencies missing"))?;
    let manifest=&job["prepareBundle"]["factFollowupManifest"];
    let first=&job["preparationStages"]["first"];
    if !first.is_null()&&(first["status"]!="completed"||!first["result"].is_object()||!first["reviewRequired"].is_boolean()
        ||preparation_review::plan_review_for_job(job).map_or(true,|plan|first["reviewRequired"]!=json!(plan.is_some()))){
        return Err(conflict("Automatic fact child first receipt is invalid"));
    }
    let mut dependencies=BTreeSet::new();let mut recipients=BTreeSet::new();
    for pin in manifest.as_array().ok_or_else(||conflict("Automatic fact manifest missing"))?{
        // Settled siblings keep immutable provenance, but only the existing
        // pending-group/review guards decide their remaining live source scope.
        current_pin(d,pin,ids,&now(),first.is_null()).map_err(conflict)?;
        if pin["prepareJobId"]!=parent["id"]||!wanted.contains(&pin["dependencyId"]){return Err(conflict("Foreign automatic fact evidence"));}
        dependencies.insert(required(pin,"dependencyId")?.to_owned());
        for id in rows(pin,"itemIds"){recipients.insert(id.as_str().ok_or_else(||conflict("Invalid automatic fact recipient"))?.to_owned());}
    }
    if dependencies.len()!=wanted.len()||recipients.len()!=ids.len()
        ||wanted.iter().any(|id|id.as_str().is_none_or(|id|!dependencies.contains(id)))
        ||ids.iter().any(|id|id.as_str().is_none_or(|id|!recipients.contains(id))){return Err(conflict("Automatic fact manifest must cover exact recipients"));}
    for dependency in wanted{
        let entry=rows(parent,"factFollowups").iter().find(|e|e["id"]==*dependency).ok_or_else(||conflict("Automatic fact dependency missing"))?;
        if entry["consumedByJobId"]!=job["id"]||(first.is_null()&&!automatic_eligible(d,parent,entry,&now())){
            return Err(conflict("Automatic fact continuation ownership changed"));
        }
    }
    Ok(())
}
pub(crate) fn validate_automatic_schedule(before:&Value,after:&Value,job:&Value)->ApiResult<()> {
    let Some(marker)=job.get("automaticFactContinuation") else{return Ok(());};
    let width=marker["admissionWidth"].as_u64().and_then(|v|usize::try_from(v).ok()).ok_or_else(||conflict("Automatic fact admission width missing"))?;
    let admission=preparation_workers::AutomaticAdmission::capture(before,width);
    if !admission.available()||rows(job,"selectedItemIds").iter().any(|id|row(before,"items",id.as_str().unwrap_or("")).map_or(true,|i|!admission.permits(before,i))){
        return Err(conflict("Automatic fact writer capacity changed"));
    }
    automatic_continuation_current(after,job)
}
fn hash(value: &Value) -> String {format!("{:x}",Sha256::digest(value.to_string().as_bytes()))}
fn work_key(binding:&Value)->String{
    let mut stable=binding.clone();stable.as_object_mut().unwrap().remove("itemRevision");hash(&stable)
}
fn rows<'a>(value:&'a Value,key:&str)->&'a [Value]{value[key].as_array().map(Vec::as_slice).unwrap_or(&[])}
fn text<'a>(value:&'a Value,key:&str,max:usize)->Result<&'a str,&'static str>{
    value[key].as_str().filter(|v|v.trim()==*v && v.encode_utf16().count()>=2 && v.encode_utf16().count()<=max && !v.chars().any(char::is_control)).ok_or("Invalid fact dependency text")
}
fn timestamp(at:&str)->Result<i64,&'static str>{chrono::DateTime::parse_from_rfc3339(at).map(|v|v.timestamp()).map_err(|_|"Invalid fact evidence time")}

/// Admit typed declarations only from a captured opt-in and held recipients.
pub(crate) fn declarations(request:&Value,result:&Value)->Result<Vec<Value>,&'static str>{
    if request.get("factDependencyContract").is_none(){
        if result.get("factDependencies").is_some_and(|v|v.as_array().is_none_or(|a|!a.is_empty())){return Err("Fact dependency has no captured contract")}
        return Ok(vec![]);
    }
    if request["factDependencyContract"]!=CONTRACT || request["preparationMode"]!="single_pass_v1" {return Err("Unsupported fact dependency contract")}
    // Omission carries no research authority. The current adapter's strict wire
    // requires explicit nullable declarations; old bridge fixtures remain holds.
    let Some(value)=result.get("factDependencies") else{return Ok(vec![])};
    let entries=value.as_array().filter(|v|v.len()<=rows(request,"items").len()).ok_or("Invalid typed fact dependencies")?;
    let mut seen=BTreeSet::new();let mut clean=Vec::new();
    for entry in entries {
        let fields=entry.as_object().ok_or("Invalid fact dependency")?;
        if fields.len()!=4 || fields.keys().any(|k|!matches!(k.as_str(),"itemId"|"kind"|"claimScope"|"publicQuery")){return Err("Invalid fact dependency fields")}
        let id=entry["itemId"].as_str().filter(|id|rows(request,"items").iter().any(|i|i["id"]==*id)).ok_or("Foreign fact recipient")?;
        if !seen.insert(id) || !rows(result,"assessments").iter().any(|a|a["itemId"]==id&&a["outcome"]=="needs_attention")
            || rows(result,"proposals").iter().any(|p|p["itemId"]==id){return Err("Fact dependency must bind a held recipient")}
        let kind=entry["kind"].as_str().filter(|k|matches!(*k,"missing_public_fact"|"private_company_fact"|"missing_media"|"owner_decision")).ok_or("Invalid fact dependency kind")?;
        text(entry,"claimScope",2000)?;
        if kind=="missing_public_fact"{text(entry,"publicQuery",1000)?;}else if !entry["publicQuery"].is_null(){return Err("Non-public dependency cannot have a query")}
        clean.push(entry.clone());
    }
    Ok(clean)
}

/// Called in the transaction retaining first-pass evidence; no extra collection.
pub(crate) fn record(d:&mut Value,run:&str,request:&Value,result:&Value,at:&str)->ApiResult<()> {
    let entries=declarations(request,result).map_err(bad)?;
    if request.get("factDependencyContract").is_none(){return Ok(())}
    let bundle=row(d,"jobs",run)?["prepareBundle"].clone();
    let mut records=Vec::new();
    for declaration in entries {
        let id=required(&declaration,"itemId")?;
        let item=rows(request,"items").iter().find(|i|i["id"]==id).ok_or_else(||bad("Missing fact recipient"))?;
        let fingerprint=bundle["factSourceFingerprints"][id].as_str().ok_or_else(||bad("Missing captured fact source fingerprint"))?;
        let stale=prepare_bundle::review_fingerprint(d,id).map_or(true,|current|current!=fingerprint);
        let binding=json!({"account":d["account"],"connectorBinding":request["connectorBinding"],"itemId":id,
            "itemRevision":item["revision"],"sourceFingerprint":fingerprint,"postId":item["postId"],"declaration":declaration});
        let signature=hash(&binding);
        let work_key=work_key(&binding);
        let attempted=list(d,"jobs").iter().filter(|j|j["id"]!=run).flat_map(|j|rows(j,"factFollowups"))
            .any(|old|old["workKey"]==work_key && !rows(old,"attempts").is_empty());
        let public=declaration["kind"]=="missing_public_fact";
        records.push(json!({"version":1,"id":uuid::Uuid::new_v4().to_string(),"prepareJobId":run,
            "bundleId":bundle["id"],"bundleDigest":bundle["digest"],"binding":binding,"signature":signature,"workKey":work_key,
            "kind":declaration["kind"],"itemId":id,"status":if stale{"stale"}else if public&&!attempted{"pending"}else{"held"},
            "reason":if stale{"fact_source_changed_during_preparation"}else if attempted{"public_fact_attempt_already_consumed"}else if public{"public_fact_pending"}else{"non_public_fact_requires_context"},
            "createdAt":at,"attempts":[],"evidence":null,"consumedByJobId":null}));
    }
    let old=row(d,"jobs",run)?.get("factFollowups");
    if old.is_some(){return Ok(())} // First-pass immutability is checked by caller.
    row_mut(d,"jobs",run)?["factFollowups"]=json!(records);Ok(())
}
pub(crate) fn validate_created(before:&Value,job:&Value)->ApiResult<()> {
    let id=required(job,"id")?;let mut expected=before.clone();
    let stage=&job["preparationStages"]["first"];
    record(&mut expected,id,&job["prepareBundle"]["request"],&stage["result"],required(stage,"at")?)?;
    let mut actual=job["factFollowups"].clone();let mut wanted=row(&expected,"jobs",id)?["factFollowups"].clone();
    for entries in [&mut actual,&mut wanted]{
        if entries.is_null(){continue}
        let mut seen=BTreeSet::new();for entry in entries.as_array_mut().ok_or_else(||internal("Invalid initial fact dependencies"))?{
            let id=required(entry,"id")?;if uuid::Uuid::parse_str(id).is_err()||!seen.insert(id.to_owned()){return Err(internal("Invalid fact dependency identity"))}
            entry.as_object_mut().ok_or_else(||internal("Invalid fact dependency"))?.remove("id");
        }
    }
    if actual!=wanted{return Err(internal("Initial fact dependencies differ from captured first pass"))}
    Ok(())
}

fn current_dependency(d:&Value,entry:&Value)->Result<(),&'static str>{
    let binding=&entry["binding"];let id=entry["itemId"].as_str().ok_or("Invalid fact recipient")?;
    let item=row(d,"items",id).map_err(|_|"Fact recipient missing")?;
    if entry["version"]!=1 || binding["account"]!=d["account"] || binding["connectorBinding"]!=active_binding(d).map_err(|_|"Fact connector missing")?.to_json()
        || entry["signature"]!=hash(binding) || binding["itemId"]!=id || binding["itemRevision"]!=item["revision"]
        || binding["sourceFingerprint"]!=prepare_bundle::review_fingerprint(d,id)? {return Err("Fact dependency source changed")}
    if item["workflow"]!="attention" || !engine_prepare::operation_holds(d,&[id.to_owned()]).is_empty()
        ||list(d,"proposals").iter().any(|p|p["itemId"]==id&&matches!(p["status"].as_str(),Some("draft"|"approved"|"dispatching"|"unknown"|"succeeded"))){return Err("Fact recipient has current work or uncertain effect")}
    Ok(())
}
fn evidence_current(entry:&Value,at:&str)->Result<(),&'static str>{
    let evidence=&entry["evidence"];
    if evidence["version"]!=1 || evidence["trust"]!="source_only" || evidence["activePolicy"]!=false
        || evidence["checksum"]!=hash(&evidence["result"]) || rows(&evidence["result"],"sources").is_empty(){return Err("Fact evidence missing or changed")}
    let completed=timestamp(evidence["result"]["runMetadata"]["completedAt"].as_str().ok_or("Missing fact evidence time")?)?;
    let now=timestamp(at)?;
    if completed>now+60 || now-completed>TTL_SECONDS{return Err("Fact evidence expired")}
    Ok(())
}
pub(crate) fn summary(job:&Value)->Value {
    json!(rows(job,"factFollowups").iter().map(|e|json!({"id":e["id"],"itemId":e["itemId"],"kind":e["kind"],"status":e["status"],"reason":e["reason"],
        "lastResearchJobId":rows(e,"attempts").last().map(|a|a["jobId"].clone()).unwrap_or(Value::Null),"consumedByJobId":e["consumedByJobId"]})).collect::<Vec<_>>())
}

/// Only exact current dependencies enter the new request, never all same-post items.
pub(crate) fn select(d:&Value,ids:&[Value],at:&str)->Result<Value,&'static str>{
    let mut materials=Vec::new();let mut manifest=Vec::new();
    for job in list(d,"jobs") {for entry in rows(job,"factFollowups") {
        if entry["status"]!="resolved" || !entry["consumedByJobId"].is_null() || !ids.contains(&entry["itemId"])
            || current_dependency(d,entry).is_err() || evidence_current(entry,at).is_err(){continue}
        for (index,source) in rows(&entry["evidence"]["result"],"sources").iter().enumerate(){
            let material=material(entry,source,index,at);
            manifest.push(json!({"prepareJobId":job["id"],"dependencyId":entry["id"],"signature":entry["signature"],
                "evidenceChecksum":entry["evidence"]["checksum"],"sourceIndex":index,"itemIds":[entry["itemId"]],
                "selectedAt":at,"materialId":material["id"],"materialHash":hash(&material)}));
            materials.push(material);
        }
    }}
    Ok(json!({"materials":materials,"manifest":manifest}))
}
fn material(entry:&Value,source:&Value,index:usize,at:&str)->Value{
    let mut value=json!({"id":format!("fact-{}-{index}",entry["id"].as_str().unwrap_or("")),"account":entry["binding"]["account"],
        "kind":"research","title":source["title"],"text":source["claim"],"sourceUrl":source["url"],"trust":"source_only","activePolicy":false,
        "itemIds":[entry["itemId"]],"sourceItemId":entry["itemId"],"researchRecordId":entry["id"],"researchJobId":entry["prepareJobId"],
        "fetchedAt":entry["evidence"]["result"]["runMetadata"]["completedAt"],"retrievedAt":at,
        "usage":"Exact fact follow-up evidence. Evaluate claim scope and freshness; never policy or publication approval."});
    for key in ["claimKind","scope","sourceScope","extraction"] {if let Some(field)=source.get(key){value[key]=field.clone();}}
    value
}
pub(crate) fn current(d:&Value,manifest:&Value,ids:&[Value],at:&str)->Result<(),&'static str>{
    for pin in manifest.as_array().ok_or("Invalid fact evidence manifest")? {
        current_pin(d,pin,ids,at,true)?;
    }Ok(())
}
fn current_pin(d:&Value,pin:&Value,ids:&[Value],at:&str,source_current:bool)->Result<(),&'static str>{
        let parent=row(d,"jobs",pin["prepareJobId"].as_str().ok_or("Missing fact parent")?).map_err(|_|"Missing fact parent")?;
        let entry=rows(parent,"factFollowups").iter().find(|e|e["id"]==pin["dependencyId"]).ok_or("Missing fact dependency")?;
        // Consumption is a cursor, not a source mutation. Later new evidence has no effect on old pins.
        let binding=&entry["binding"];let id=entry["itemId"].as_str().ok_or("Missing fact recipient")?;
        if binding["account"]!=d["account"] || binding["connectorBinding"]!=active_binding(d).map_err(|_|"Missing fact connector")?.to_json()
            ||pin["signature"]!=entry["signature"]||pin["evidenceChecksum"]!=entry["evidence"]["checksum"]||!ids.contains(&entry["itemId"])
            ||pin["itemIds"]!=json!([entry["itemId"]])||entry["signature"]!=hash(binding)
            ||(source_current&&binding["sourceFingerprint"]!=prepare_bundle::review_fingerprint(d,id)?){return Err("Fact evidence binding changed")}
        if source_current{evidence_current(entry,at)?;}else if entry["evidence"]["checksum"]!=hash(&entry["evidence"]["result"]){return Err("Retained fact evidence changed");}
        let index=pin["sourceIndex"].as_u64().and_then(|n|usize::try_from(n).ok()).ok_or("Invalid fact source index")?;
        let source=rows(&entry["evidence"]["result"],"sources").get(index).ok_or("Missing fact source")?;
        let material=material(entry,source,index,pin["selectedAt"].as_str().ok_or("Missing fact selection time")?);
        if pin["materialHash"]!=hash(&material)||pin["materialId"]!=material["id"]{return Err("Pinned fact evidence changed")}
    Ok(())
}
pub(crate) fn consume(d:&mut Value,job_id:&str,manifest:&Value)->ApiResult<()> {
    for pin in manifest.as_array().ok_or_else(||bad("Invalid fact manifest"))? {
        let parent=required(pin,"prepareJobId")?;
        let entry=row_mut(d,"jobs",parent)?["factFollowups"].as_array_mut().and_then(|v|v.iter_mut().find(|e|e["id"]==pin["dependencyId"]))
            .ok_or_else(||conflict("Fact dependency disappeared"))?;
        if entry["consumedByJobId"].is_null(){entry["consumedByJobId"]=json!(job_id);}
        else if entry["consumedByJobId"]!=job_id {return Err(conflict("Fact evidence already consumed"))}
    }
    Ok(())
}

fn admit_sources(result:&Value,request:&Value)->Result<Value,&'static str>{
    let metadata=&result["runMetadata"];
    if result["text"].as_str().is_none_or(|s|s.trim().is_empty()||s.encode_utf16().count()>12000){return Err("Invalid fact research text")}
    if metadata["factResearchContract"]!=SOURCE_CONTRACT || metadata["model"]!=codex_model_policy::MODEL
        || metadata["modelProfile"]!=codex_model_policy::PROFILE || metadata["reasoningEffort"]!="medium"
        || metadata["promptVersion"]!="communityhero-discussion-public-research-v4-uncapped-evidence" || !metadata["webCallLimit"].is_null()
        || metadata["inputSha256"]!=format!("{:x}",Sha256::digest(format!("{{\"query\":{},\"factResearchContract\":\"{}\"}}",request["query"],SOURCE_CONTRACT).as_bytes())) {return Err("Invalid fact research provenance")}
    for key in ["inputSha256","instructionSha256","cliSha256"]{if !metadata[key].as_str().is_some_and(|s|s.len()==64&&s.bytes().all(|b|b.is_ascii_hexdigit())){return Err("Invalid fact research digest")}}
    timestamp(metadata["completedAt"].as_str().ok_or("Missing fact completion time")?)?;
    if metadata["webCalls"].as_u64().is_none()||metadata["elapsedMs"].as_u64().is_none()||result.to_string().len()>100_000{return Err("Invalid fact research receipt")}
    let sources=result["sources"].as_array().ok_or("Missing public fact sources")?;
    let mut research=metadata.clone();
    research["sources"]=json!(sources.iter().map(|s|{let mut s=s.clone();s["itemId"]=json!("public-query");s}).collect::<Vec<_>>());
    if let Some(holds)=result.get("evidenceHolds").filter(|v|v.as_array().is_some_and(|v|!v.is_empty())){research["evidenceHolds"]=holds.clone();}
    let clean=preparation_review::sanitize_uncapped_review_research(&research,&BTreeSet::from(["public-query".to_owned()]))?;
    let account=match request["account"].as_str(){Some("LikeAvto")=>"likeavto",Some("BAW Russia")=>"baw-russia",_=>return Err("Invalid fact research company")};
    if rows(&clean,"evidenceHolds").iter().any(|h|h["accountKey"]!=account){return Err("Foreign company fact evidence hold")}
    let mut urls=BTreeSet::new();
    let mut sources=Vec::new();for source in rows(&clean,"sources"){
        let mut source=source.clone();if !urls.insert(source["url"].as_str().ok_or("Missing source URL")?.to_owned()){return Err("Duplicate fact source")}
        source.as_object_mut().unwrap().remove("itemId");sources.push(source);
    }
    let mut retained=metadata.clone();
    retained.as_object_mut().ok_or("Invalid fact research metadata")?.retain(|key,_|matches!(key.as_str(),
        "version"|"status"|"model"|"modelProfile"|"reasoningEffort"|"promptVersion"|"instructionSha256"|"inputSha256"|"cliSha256"|
        "elapsedMs"|"completedAt"|"webCalls"|"webCallLimit"|"factResearchContract"|"toolsProfileSha256"));
    let mut admitted=json!({"text":result["text"],"sources":sources,"runMetadata":retained});
    if let Some(holds)=clean.get("evidenceHolds"){admitted["evidenceHolds"]=holds.clone();}
    Ok(admitted)
}

fn group_key(entry:&Value)->String{hash(&json!({"account":entry["binding"]["account"],"connectorBinding":entry["binding"]["connectorBinding"],
    "bundleDigest":entry["bundleDigest"],"postId":entry["binding"]["postId"],"publicQuery":entry["binding"]["declaration"]["publicQuery"],
    "claimScope":entry["binding"]["declaration"]["claimScope"]}))}

/// Canonical async research jobs carry finite attempt identities; polling never
/// repeats completed reads. Interrupted read-only lookup may get one new attempt.
pub(crate) async fn resolve(State(app):State<App>,Extension(actor):Extension<operator_auth::Actor>,Json(body):Json<Value>)->ApiResult<Json<Value>>{
    let parent=required(&body,"prepareJobId")?.to_owned();
    let ids=body["itemIds"].as_array().filter(|v|!v.is_empty()&&v.len()<=100).ok_or_else(||bad("Select exact public fact recipients"))?.clone();
    let mut unique=BTreeSet::new();for id in &ids {let id=id.as_str().filter(|s|!s.is_empty()).ok_or_else(||bad("Invalid fact recipient"))?;if !unique.insert(id){return Err(bad("Duplicate fact recipient"))}}
    let token=app.lifecycle_admission_token(crate::runtime_lifecycle::AdmissionClass::Preparation).await?;
    let (output,launches)=app.change(|d|{
        schedule_research_admitted(d,&parent,&ids,Some(&actor),&token)
    }).await?;
    for id in launches {let worker=app.clone();app.spawn(id.clone(),run(worker,id));}
    Ok(Json(output))
}

fn schedule(d:&mut Value,parent:&str,ids:&[Value],actor:&operator_auth::Actor)->ApiResult<(Value,Vec<String>)>{
    schedule_inner(d,parent,ids,Some(actor))
}
fn schedule_research_admitted(d:&mut Value,parent:&str,ids:&[Value],actor:Option<&operator_auth::Actor>,token:&crate::runtime_lifecycle::OwnerToken)->ApiResult<(Value,Vec<String>)> {
    crate::runtime_lifecycle::require_admission(d,token,crate::runtime_lifecycle::AdmissionClass::Preparation)?;
    let scheduled=schedule_inner(d,parent,ids,actor)?;
    for id in &scheduled.1 {record_research_initial_admission(d,token,id,&now())?;}
    Ok(scheduled)
}
fn research_not_dispatched()->ApiError {conflict("Fact research lifecycle admission changed or already reserved; model not dispatched")}
fn research_origin(d:&Value,job:&Value)->ApiResult<Value> {
    if job["kind"]!="assistant"||job["purpose"]!="public_fact_followup"
        ||!matches!(job["status"].as_str(),Some("queued"|"running")){return Err(research_not_dispatched());}
    let id=required(job,"id")?;let parent=row(d,"jobs",required(job,"parentPrepareJobId")?)?;
    if parent["prepareBundle"]["digest"]!=hash(&parent["prepareBundle"]["request"])
        ||parent["prepareBundle"]["request"]["account"]!=d["account"]
        ||parent["prepareBundle"]["request"]["connectorBinding"]!=active_binding(d)?.to_json(){return Err(research_not_dispatched());}
    let wanted=job["factDependencyIds"].as_array().filter(|v|!v.is_empty()&&v.len()<=100).ok_or_else(research_not_dispatched)?;
    let ids=job["requestedItemIds"].as_array().filter(|v|v.len()==wanted.len()).ok_or_else(research_not_dispatched)?;
    let signatures=job["factSignatures"].as_array().filter(|v|v.len()==wanted.len()).ok_or_else(research_not_dispatched)?;
    let mut dependencies=BTreeSet::new();let mut recipients=BTreeSet::new();let mut attempts=Vec::new();
    for dependency in wanted {
        let dependency=dependency.as_str().filter(|s|!s.is_empty()).ok_or_else(research_not_dispatched)?;
        if !dependencies.insert(dependency){return Err(research_not_dispatched());}
        let entries:Vec<_>=rows(parent,"factFollowups").iter().filter(|e|e["id"]==dependency).collect();
        if entries.len()!=1{return Err(research_not_dispatched());}let entry=entries[0];
        let recipient=required(entry,"itemId")?;
        if !recipients.insert(recipient.to_owned())||!ids.contains(&entry["itemId"])
            ||entry["prepareJobId"]!=parent["id"]||entry["bundleDigest"]!=parent["prepareBundle"]["digest"]
            ||entry["kind"]!="missing_public_fact"||entry["status"]!="researching"
            ||!signatures.contains(&json!({"id":entry["id"],"signature":entry["signature"]}))
            ||rows(entry,"attempts").last().is_none_or(|a|a["jobId"]!=id)
            ||job["factGroupKey"]!=group_key(entry)
            ||job["researchRequest"]!=json!({"account":d["account"],"query":entry["binding"]["declaration"]["publicQuery"],"factResearchContract":SOURCE_CONTRACT}){
            return Err(research_not_dispatched());
        }
        current_dependency(d,entry).map_err(|_|research_not_dispatched())?;
        if parent["purpose"]=="auto_prepare" {
            automatic_parent(d,parent)?;
            if !automatic_eligible(d,parent,entry,&now())||rows(entry,"attempts").len()!=1{return Err(research_not_dispatched());}
        }
        attempts.push(json!({"dependencyId":entry["id"],"signature":entry["signature"],"attempts":entry["attempts"]}));
    }
    preparation_reservations::assert_available(d,&recipients.into_iter().collect::<Vec<_>>(),None)?;
    conductor_authority::fence_job_capture(d,id,"prepare")?;
    if job.get("factWorkerScope").is_some()&&worker_scope::capture(d,job).as_ref()!=job.get("factWorkerScope"){return Err(research_not_dispatched());}
    Ok(json!({"parentPrepareJobId":parent["id"],"parentBundleDigest":parent["prepareBundle"]["digest"],
        "factDependencyIds":wanted,"requestedItemIds":ids,"factSignatures":signatures,"attempts":attempts}))
}
fn research_admission_receipt(d:&Value,job:&Value,token:&crate::runtime_lifecycle::OwnerToken,at:&str,stage:&str)->ApiResult<Value> {
    timestamp(at).map_err(|_|research_not_dispatched())?;
    Ok(json!({"version":1,"class":"preparation","stage":stage,"jobId":job["id"],"at":at,
        "owner":{"account":token.account,"runtimeId":token.runtime_id,"releaseSha256":token.release_sha256,"epoch":token.epoch},
        "requestSha256":hash(&job["researchRequest"]),"originSha256":hash(&research_origin(d,job)?)}))
}
fn record_research_initial_admission(d:&mut Value,token:&crate::runtime_lifecycle::OwnerToken,id:&str,at:&str)->ApiResult<()> {
    crate::runtime_lifecycle::require_admission(d,token,crate::runtime_lifecycle::AdmissionClass::Preparation)?;
    let job=row(d,"jobs",id)?;
    if job.get("factResearchInitialAdmission").is_some()||job.get("factResearchAdmission").is_some(){return Err(research_not_dispatched());}
    let receipt=research_admission_receipt(d,job,token,at,"initial")?;
    row_mut(d,"jobs",id)?["factResearchInitialAdmission"]=receipt;Ok(())
}
fn reserve_research_admission(d:&mut Value,token:&crate::runtime_lifecycle::OwnerToken,id:&str,at:&str)->ApiResult<Value> {
    crate::runtime_lifecycle::require_admission(d,token,crate::runtime_lifecycle::AdmissionClass::Preparation).map_err(|_|research_not_dispatched())?;
    let job=row(d,"jobs",id)?;let initial=job.get("factResearchInitialAdmission").ok_or_else(research_not_dispatched)?;
    if job["status"]!="running"||job.get("factResearchAdmission").is_some()
        ||*initial!=research_admission_receipt(d,job,token,required(initial,"at")?,"initial")? {return Err(research_not_dispatched());}
    let receipt=research_admission_receipt(d,job,token,at,"research")?;
    let job=row_mut(d,"jobs",id)?;job["factResearchAdmission"]=receipt;Ok(job.clone())
}
fn schedule_inner(d:&mut Value,parent:&str,ids:&[Value],actor:Option<&operator_auth::Actor>)->ApiResult<(Value,Vec<String>)>{
        let first=list(d,"jobs").len();
        let context=conductor_authority::fence_admission(d,"prepare",&ids)?;
        if let Some(actor)=actor{conductor_authority::fence_actor(context.as_ref(),actor)?;}
        let original=row(d,"jobs",&parent)?.clone();
        let automatic=actor.is_none();
        if automatic {
            automatic_parent(d,&original)?;
            if context.is_some()||active_assistant(d){return Err(conflict("Automatic public research lane unavailable"));}
            let selected:Vec<String>=ids.iter().filter_map(|v|v.as_str().map(str::to_owned)).collect();
            if ids.is_empty()||ids.len()>100||selected.len()!=ids.len()
                ||selected.iter().collect::<BTreeSet<_>>().len()!=ids.len()
                ||rows(&original,"factFollowups").iter().filter(|e|ids.contains(&e["itemId"]))
                    .map(group_key).collect::<BTreeSet<_>>().len()!=1{
                return Err(conflict("Automatic public research requires one exact group"));
            }
            preparation_reservations::assert_available(d,&selected,None)?;
        }
        if (!automatic&&original["purpose"]!="engine_prepare") || original["prepareBundle"]["request"]["account"]!=d["account"]
            ||ids.iter().any(|id|!rows(&original,"factFollowups").iter().any(|e|e["itemId"]==*id)){return Err(conflict("Fact dependency parent/scope mismatch"))}
        let mut job_ids=Vec::new();let mut ready=Vec::new();let mut held=Vec::new();let mut launches=Vec::new();let mut grouped=BTreeSet::new();
        for entry in rows(&original,"factFollowups").iter().filter(|e|ids.contains(&e["itemId"])) {
            if automatic&&(!automatic_eligible(d,&original,entry,&now())||!rows(entry,"attempts").is_empty()){
                held.push(json!({"itemId":entry["itemId"],"reason":"automatic_fact_attempt_owned_or_ineligible"}));continue;
            }
            if entry["kind"]!="missing_public_fact" {held.push(json!({"itemId":entry["itemId"],"reason":"non_public_fact_requires_context"}));continue}
            if let Err(reason)=current_dependency(d,entry){held.push(json!({"itemId":entry["itemId"],"reason":reason}));continue}
            if entry["status"]=="resolved" {
                if entry["consumedByJobId"].is_null()&&evidence_current(entry,&now()).is_ok(){ready.push(entry["itemId"].clone());}
                else{held.push(json!({"itemId":entry["itemId"],"reason":"fact_evidence_consumed_or_expired"}));}continue;
            }
            if entry["status"]=="held"||entry["status"]=="stale"{held.push(json!({"itemId":entry["itemId"],"reason":entry["reason"]}));continue}
            let attempts=rows(entry,"attempts");
            if let Some(last)=attempts.last(){
                let old=row(d,"jobs",required(last,"jobId")?)?;
                if matches!(old["status"].as_str(),Some("queued"|"running")){job_ids.push(old["id"].clone());continue}
                if old["status"]!="interrupted" || attempts.len()>=MAX_ATTEMPTS {held.push(json!({"itemId":entry["itemId"],"reason":"public_fact_lookup_failed"}));continue}
            }
            if !grouped.insert(group_key(entry)){continue}
            let job_id=new_job(d,"assistant","public_fact_followup")?;
            let request=json!({"account":d["account"],"query":entry["binding"]["declaration"]["publicQuery"],"factResearchContract":SOURCE_CONTRACT});
            let compatible:Vec<_>=rows(&original,"factFollowups").iter().enumerate().filter(|(_,other)|
                ids.contains(&other["itemId"])&&other["kind"]=="missing_public_fact"&&group_key(other)==group_key(entry)
                &&current_dependency(d,other).is_ok()&&matches!(other["status"].as_str(),Some("pending"|"researching"))
                &&(!automatic||(automatic_eligible(d,&original,other,&now())&&rows(other,"attempts").is_empty()))
                &&(rows(other,"attempts").is_empty()||rows(other,"attempts").last().is_some_and(|a|
                    rows(other,"attempts").len()<MAX_ATTEMPTS&&row(d,"jobs",a["jobId"].as_str().unwrap_or("")).is_ok_and(|j|j["status"]=="interrupted"))))
                .map(|(index,other)|(index,other["id"].clone(),other["itemId"].clone(),other["signature"].clone())).collect();
            if compatible.is_empty(){return Err(conflict("Compatible public fact scope missing"))}
            let job=row_mut(d,"jobs",&job_id)?;
            job["purpose"]=json!("public_fact_followup");job["parentPrepareJobId"]=json!(parent);
            job["factDependencyIds"]=json!(compatible.iter().map(|(_,id,_,_)|id).collect::<Vec<_>>());
            job["requestedItemIds"]=json!(compatible.iter().map(|(_,_,id,_)|id).collect::<Vec<_>>());
            job["factSignatures"]=json!(compatible.iter().map(|(_,id,_,signature)|json!({"id":id,"signature":signature})).collect::<Vec<_>>());
            job["researchRequest"]=request;job["factGroupKey"]=json!(group_key(entry));
            for (index,_,_,_) in compatible {
                let target=&mut row_mut(d,"jobs",&parent)?["factFollowups"][index];target["status"]=json!("researching");
                let attempt=rows(target,"attempts").len()+1;
                target["attempts"].as_array_mut().unwrap().push(json!({"jobId":job_id,"attempt":attempt,"createdAt":now()}));
            }
            job_ids.push(json!(job_id));launches.push(job_id);
        }
        conductor_authority::fence_new_jobs(d,first)?;
        // Capture after attribution fencing, and only for newly created jobs.
        // Existing paid/legacy attempts are never upgraded by polling/replay.
        for id in &launches {
            if let Some(scope)=worker_scope::capture(d,row(d,"jobs",id)?) {
                row_mut(d,"jobs",id)?["factWorkerScope"]=scope;
            }
        }
        Ok((json!({"jobIds":job_ids,"readyItemIds":ready,"held":held}),launches))

}
async fn run(app:App,id:String)->ApiResult<Value>{
    // Read-only public lookup uses the adapter's existing interactive lane;
    // awaiting preparation here would serialize otherwise independent work.
    let _guard=app.assistant_chat_gate.lock().await;
    let state=app.read().await?;let job=row(&state,"jobs",&id)?.clone();
    let parent=required(&job,"parentPrepareJobId")?.to_owned();
    let entries:Vec<_>=rows(row(&state,"jobs",&parent)?,"factFollowups").iter().filter(|e|rows(&job,"factDependencyIds").contains(&e["id"])).cloned().collect();
    if job["status"]!="running"||entries.len()!=rows(&job,"factDependencyIds").len()||entries.is_empty()
        ||entries.iter().any(|entry|rows(entry,"attempts").last().is_none_or(|attempt|attempt["jobId"]!=id)
            ||!rows(&job,"factSignatures").contains(&json!({"id":entry["id"],"signature":entry["signature"]}))){
        return Err(conflict("Fact dependency attempt is no longer active"));
    }
    for entry in &entries{current_dependency(&state,entry).map_err(conflict)?;}
    if job.get("factWorkerScope").is_some() && worker_scope::capture(&state,&job).as_ref()!=job.get("factWorkerScope") {
        return Err(conflict("Fact research worker scope changed"));
    }
    let original=row(&state,"jobs",&parent)?;
    if original["purpose"]=="auto_prepare" {
        automatic_parent(&state,original)?;
        if entries.iter().any(|entry|!automatic_eligible(&state,original,entry,&now())){
            return Err(conflict("Automatic fact research authority changed"));
        }
        let ids=entries.iter().filter_map(|e|e["itemId"].as_str().map(str::to_owned)).collect::<Vec<_>>();
        preparation_reservations::assert_available(&state,&ids,None)?;
    }
    conductor_authority::fence_job_capture(&state,&id,"prepare")?;
    if let Some(run)=job["conductorRunId"].as_str(){
        conductor_authority::authorize(&app,run,job["grantGeneration"].as_u64().ok_or_else(||conflict("Fact research generation missing"))?,"prepare",rows(&job,"requestedItemIds")).await?;
    }
    // Scheduling owns a durable attempt, but only a current, one-shot stage
    // reservation may start its paid model after waiting for the chat lane.
    let native=app.lifecycle_work.begin(crate::runtime_owned_work::Kind::Preparation)?;
    let token=match app.lifecycle_admission_token(crate::runtime_lifecycle::AdmissionClass::Preparation).await {
        Ok(token)=>token,Err(_)=>{native.settled();return Err(research_not_dispatched());}
    };
    let job=match app.change(|d|reserve_research_admission(d,&token,&id,&now())).await {
        Ok(job)=>job,Err(error)=>{native.settled();return Err(error);}
    };
    let raw=app.bridge_admitted("assistant_research",job["researchRequest"].clone(),native).await?;
    let result=admit_sources(&raw,&job["researchRequest"]).map_err(bad)?;
    app.change(|d|settle(d,&id,&job,&entries,&result)).await
}

fn dispatched_research_binding(id:&str,job:&Value)->ApiResult<Value> {
    if job["id"]!=id||job["kind"]!="assistant"||job["purpose"]!="public_fact_followup"{return Err(research_not_dispatched());}
    let initial=job.get("factResearchInitialAdmission").ok_or_else(research_not_dispatched)?;
    let paid=job.get("factResearchAdmission").ok_or_else(research_not_dispatched)?;
    let digest=|v:&Value|v.as_str().is_some_and(|s|s.len()==64&&s.bytes().all(|b|b.is_ascii_hexdigit()));
    for (receipt,stage) in [(initial,"initial"),(paid,"research")] {
        let owner=&receipt["owner"];
        if receipt.as_object().is_none_or(|v|v.len()!=8)||receipt["version"]!=1||receipt["class"]!="preparation"
            ||receipt["stage"]!=stage||receipt["jobId"]!=id||job["id"]!=id
            ||timestamp(required(receipt,"at")?).is_err()||receipt["requestSha256"]!=hash(&job["researchRequest"])
            ||!digest(&receipt["originSha256"])||owner.as_object().is_none_or(|v|v.len()!=4)
            ||owner["account"]!=job["researchRequest"]["account"]
            ||!matches!(owner["account"].as_str(),Some("LikeAvto"|"BAW Russia"))
            ||owner["runtimeId"].as_str().is_none_or(|s|s.is_empty()||s.len()>80||!s.bytes().all(|b|b.is_ascii_alphanumeric()||matches!(b,b'-'|b'_')))
            ||!digest(&owner["releaseSha256"])||owner["epoch"].as_u64().is_none_or(|v|v==0){return Err(research_not_dispatched());}
    }
    if initial["owner"]!=paid["owner"]||initial["originSha256"]!=paid["originSha256"]{return Err(research_not_dispatched());}
    Ok(json!({"version":1,"jobId":id,"kind":job["kind"],"purpose":job["purpose"],"parentPrepareJobId":job["parentPrepareJobId"],"researchRequest":job["researchRequest"],
        "factDependencyIds":job["factDependencyIds"],"requestedItemIds":job["requestedItemIds"],"factSignatures":job["factSignatures"],
        "initialAdmission":initial,"researchAdmission":paid}))
}
fn settle(d:&mut Value,id:&str,job:&Value,entries:&[Value],result:&Value)->ApiResult<Value>{
    let binding=dispatched_research_binding(id,job)?;
    let parent=required(job,"parentPrepareJobId")?;let original=row(d,"jobs",id)?.clone();
    // A cancelled owner still owns its dispatched response. A corrupted or
    // reassigned immutable execution tuple does not; never retarget that result.
    if ["id","kind","purpose","researchRequest","parentPrepareJobId","factDependencyIds","requestedItemIds","factSignatures",
        "factResearchInitialAdmission","factResearchAdmission"].iter().any(|field|original.get(*field)!=job.get(*field)) {
        return Err(conflict("Dispatched fact research identity changed; paid result not retargeted"));
    }
    if let Some(retained)=original.get("researchResult").filter(|v|!v.is_null()) {
        if retained!=result||original.get("researchResultBinding")!=Some(&binding){return Err(conflict("Immutable paid fact result already retained; replacement refused"));}
        return Ok(original.get("researchOutcome").cloned().unwrap_or_else(||json!({"dependencies":[]})));
    }
    // Retention is independent of dependency admission. A later cancellation
    // or newer attempt must not roll back the dispatched receipt or borrow it.
    let authority_changed=original["status"]!="running"
        ||conductor_authority::fence_job_capture(d,id,"prepare").is_err()
        ||row(d,"jobs",parent).is_ok_and(|p|p["purpose"]=="auto_prepare"&&automatic_parent(d,p).is_err())
        ||original.get("factWorkerScope")!=job.get("factWorkerScope")
        ||(original.get("factWorkerScope").is_some()&&worker_scope::capture(d,&original).as_ref()!=original.get("factWorkerScope"));
    let target=row_mut(d,"jobs",id)?;target["researchResult"]=result.clone();target["researchResultBinding"]=binding;
    let mut outcomes=Vec::new();
    for captured in entries {
        let found:Vec<Value>=row(d,"jobs",parent).ok().map(|p|rows(p,"factFollowups").iter().filter(|e|e["id"]==captured["id"]).cloned().collect()).unwrap_or_default();
        let current=found.first();
        if found.len()!=1||current.is_none_or(|e|rows(e,"attempts").last().is_none_or(|a|a["jobId"]!=id)||e["signature"]!=captured["signature"]) {
            outcomes.push(json!({"itemId":captured["itemId"],"dependencyId":captured["id"],"status":"stale","reason":"fact_dependency_attempt_changed"}));
            continue;
        }
        let entry=current.unwrap();let stale=if authority_changed{Some("fact_research_authority_changed")}else{current_dependency(d,entry).err()};
        let target=row_mut(d,"jobs",parent).expect("parent verified in this writer")["factFollowups"].as_array_mut().expect("dependency array verified")
            .iter_mut().find(|e|e["id"]==entry["id"]).expect("exact dependency verified");
        target["evidence"]=json!({"version":1,"trust":"source_only","activePolicy":false,"result":result,"checksum":hash(result)});
        target["status"]=json!(if stale.is_some(){"stale"}else if rows(result,"sources").is_empty(){"held"}else{"resolved"});
        target["reason"]=json!(stale.unwrap_or(if rows(result,"sources").is_empty(){"public_fact_no_useful_sources"}else{"public_fact_source_available"}));
        outcomes.push(json!({"itemId":entry["itemId"],"status":target["status"],"dependencyId":entry["id"]}));
    }
    let outcome=json!({"dependencies":outcomes});row_mut(d,"jobs",id)?["researchOutcome"]=outcome.clone();Ok(outcome)
}

#[cfg(test)]
pub(crate) fn storage_active_fact_fixture()->(Value,String,String) {
    let (mut d,parent,lookup)=tests::active_fact_fixture();tests::fact_tail(&mut d,"tail","tail-post");
    (d,parent,lookup)
}
#[cfg(test)]
mod tests {
    include!("fact_followup_tests.rs");
    include!("fact_worker_scope_tests.rs");
}
