//! One logical repair round per ORIGINAL answering attempt. Versions/needs/
//! leases never create another identity; UNKNOWN consumes the original slot.
use serde_json::{json,Value};
use crate::preparation_materials::{rows,hash};
pub(crate) const CONTRACT:&str="AnsweringRepairPlan.v1";
pub(crate) fn capture(d:&Value,origin:&str,needs:&[Value],results:&[Value],request:&Value,at:&str)->Result<Value,&'static str>{
    let original=crate::row(d,"jobs",origin).map_err(|_|"repair_origin_missing")?;
    if original.get("originatingAnsweringAttemptId").is_some()||original["preparationStages"]["first"]["status"]!="completed"
        ||needs.is_empty()||needs.len()>8||results.len()!=needs.len(){return Err("repair_origin_or_fanin_invalid");}
    if original["purpose"]=="auto_revalidate"{crate::auto_prepare::revalidation::repair_current(d,origin,&rows(request,"items").iter().map(|i|i["id"].clone()).collect::<Vec<_>>(),crate::auto_prepare::revalidation::RepairPhase::BeforeAdmission).map_err(|_|"repair_revalidation_currentness_changed")?;}
    crate::preparation_materials::require_request(d,request)?;crate::preparation_unit::current_request(d,request,at)?;
    let mut ordered=needs.to_vec();ordered.sort_by_key(|n|n["needId"].as_str().unwrap_or("").to_owned());
    let mut all=rows(original,"videoFrameNeeds").to_vec();all.sort_by_key(|n|n["needId"].as_str().unwrap_or("").to_owned());if ordered!=all{return Err("repair_exact_need_fanin_required");}
    let mut refs=Vec::new();let mut recipients=std::collections::BTreeSet::new();let mut seen=std::collections::BTreeSet::new();
    for need in &ordered{
        if need["originatingAnsweringAttemptId"]!=origin||!seen.insert(need["needId"].as_str().ok_or("repair_need_identity_missing")?)
            ||!rows(original,"videoFrameNeeds").contains(need){return Err("repair_need_lineage_invalid");}
        let matching=results.iter().filter(|r|r["needId"]==need["needId"]).collect::<Vec<_>>();
        if matching.len()!=1{return Err("repair_frame_fanin_incomplete");}
        crate::video_frame_work::validate_result(d,need,matching[0])?;
        refs.push(matching[0].clone());for id in rows(need,"affectedRecipientIds"){recipients.insert(id.as_str().ok_or("repair_recipient_invalid")?.to_owned());}
    }
    let supplied=rows(request,"items").iter().map(|i|i["id"].as_str().ok_or("repair_recipient_invalid").map(str::to_owned)).collect::<Result<std::collections::BTreeSet<_>,_>>()?;
    if supplied!=recipients||supplied.len()!=rows(request,"items").len(){return Err("repair_exact_affected_recipients_required");}
    let frame_refs=ordered.iter().flat_map(|need|crate::video_frame_work::frame_refs(refs.iter().find(|r|r["needId"]==need["needId"]).unwrap(),need)).collect::<Vec<_>>();
    if request["optionalFrameRefs"]!=json!(frame_refs){return Err("repair_exact_frame_refs_required");}
    let ids=recipients.iter().map(|id|json!(id)).collect::<Vec<_>>();let mut bundle=crate::prepare_bundle::build_engine_capture(d,&ids,&[])?;
    bundle["id"]=json!(format!("repair-{}",hash(&json!([origin,ordered]))));bundle["request"]=request.clone();bundle["digest"]=json!(hash(request));
    let mut plan=json!({"schemaVersion":1,"contract":CONTRACT,"companyId":d["account"],"originatingAnsweringAttemptId":origin,
        "roundOrdinal":1,"needSet":ordered,"needSetDigest":hash(&json!(ordered)),"affectedRecipientIds":recipients,
        "frameResults":refs,"baseContextSha256":request["postContextBundle"]["contentSha256"],"request":request,"prepareBundle":bundle,
        "admissionPins":settlement_pins(d,&ids).map_err(|_|"repair_recipient_pin_unavailable")?,"createdAt":at});
    plan["planSha256"]=json!(hash(&plan));Ok(plan)
}
fn validate(plan:&Value)->crate::ApiResult<()> {
    let mut unsigned=plan.clone();unsigned.as_object_mut().ok_or_else(||crate::bad("Repair plan required"))?.remove("planSha256");
    if plan["schemaVersion"]!=1||plan["contract"]!=CONTRACT||plan["roundOrdinal"]!=1||plan["planSha256"]!=hash(&unsigned)
        ||plan["needSetDigest"]!=hash(&plan["needSet"]){return Err(crate::bad("Frozen repair plan changed"));}Ok(())
}
/// A reservation exception is minted from the native frozen root/child claim,
/// never from a supplied HTTP owner/family. The original local outcome must be
/// durable before a repair can admit proposals against its current revisions.
pub(crate) fn reservation_origin(d:&Value,child_id:&str,item_ids:&[String])->crate::ApiResult<String>{
    let child=crate::row(d,"jobs",child_id)?;let plan=&child["answeringRepairPlan"];validate(plan)?;
    if child["scopeReservation"].is_null()||crate::preparation_reservations::capture(d,child_id)?!=child["scopeReservation"]{
        return Err(crate::conflict("Repair reservation requires its unchanged native child capture"));
    }
    let origin=child["originatingAnsweringAttemptId"].as_str().ok_or_else(||crate::conflict("Repair root missing"))?;let root=crate::row(d,"jobs",origin)?;
    if child["kind"]!="assistant"||child["purpose"]!="answering_repair"||!matches!(child["status"].as_str(),Some("running"|"unknown"|"interrupted"))||child["roundOrdinal"]!=1||plan["originatingAnsweringAttemptId"]!=origin
        ||child["prepareBundle"]!=plan["prepareBundle"]||child["repairPaidIntent"]["identity"]!=json!({"originatingAnsweringAttemptId":origin,"roundOrdinal":1})
        ||child["repairPaidIntent"]["planSha256"]!=plan["planSha256"]||child["repairPaidIntent"]["retryAuthorized"]!=false
        ||!matches!(child["repairPaidIntent"]["status"].as_str(),Some("reserved"|"dispatching"|"unknown"))||root.get("originatingAnsweringAttemptId").is_some()
        ||root["preparationStages"]["first"]["status"]!="completed"||!root["prepareOutcome"].is_object()
        ||rows(&root["preparationStages"],"groupAdmission").iter().any(|g|g["status"]=="pending"){return Err(crate::conflict("Repair reservation root is not durably settled"));}
    let Some((count,claims))=budget(root)? else{return Err(crate::conflict("Repair root budget missing"))};
    if count["consumedRounds"]!=1||claims.len()!=1||claims[0]["childJobId"]!=child_id||claims[0]["originatingAnsweringAttemptId"]!=origin||claims[0]["roundOrdinal"]!=1||claims[0]["planSha256"]!=plan["planSha256"]{
        return Err(crate::conflict("Repair reservation claim changed"));}
    if item_ids.iter().any(|id|!rows(plan,"affectedRecipientIds").contains(&json!(id))){return Err(crate::conflict("Repair reservation recipient outside frozen affected set"));}
    let expected=capture(d,origin,rows(plan,"needSet"),rows(plan,"frameResults"),&plan["request"],plan["createdAt"].as_str().unwrap_or("")).map_err(crate::conflict)?;
    if expected!=*plan{return Err(crate::conflict("Repair reservation preconditions changed"));}Ok(origin.to_owned())
}
pub(crate) fn claim(d:&mut Value,token:&crate::runtime_lifecycle::OwnerToken,plan:&Value,at:&str)->crate::ApiResult<Value>{
    crate::runtime_lifecycle::require_admission(d,token,crate::runtime_lifecycle::AdmissionClass::Preparation)?;
    claim_owned(d,plan,at,token)
}
fn claim_owned(d:&mut Value,plan:&Value,at:&str,owner:&crate::runtime_lifecycle::OwnerToken)->crate::ApiResult<Value>{
    validate(plan)?;let origin=plan["originatingAnsweringAttemptId"].as_str().ok_or_else(||crate::bad("Original answering attempt required"))?;
    if plan["companyId"]!=d["account"]{return Err(crate::conflict("Repair company changed"));}
    let original=crate::row(d,"jobs",origin)?;
    if original["purpose"]=="auto_revalidate"{crate::auto_prepare::revalidation::repair_current(d,origin,rows(plan,"affectedRecipientIds"),crate::auto_prepare::revalidation::RepairPhase::BeforeAdmission)?;}
    if original.get("originatingAnsweringAttemptId").is_some()||original["preparationStages"]["first"]["status"]!="completed"{return Err(crate::conflict("Repair cannot mint a new root"));}
    let spent=original["preparationStages"]["repairBudget"]["consumedRounds"].as_u64().unwrap_or(0);
    if spent>=1||!rows(&original["preparationStages"],"answeringRepairs").is_empty()
        ||rows(d,"jobs").iter().any(|j|j["originatingAnsweringAttemptId"]==origin){return Err(crate::conflict("Original answering repair budget is consumed or unresolved"));}
    let current=capture(d,origin,rows(plan,"needSet"),rows(plan,"frameResults"),&plan["request"],plan["createdAt"].as_str().unwrap_or("" )).map_err(crate::conflict)?;
    if current!=*plan{return Err(crate::conflict("Repair preconditions changed"));}
    if !original["prepareOutcome"].is_object()||rows(&original["preparationStages"],"groupAdmission").iter().any(|g|g["status"]=="pending"){return Err(crate::conflict("Repair requires original durable admission"));}
    let affected=rows(plan,"affectedRecipientIds").iter().filter_map(Value::as_str).map(str::to_owned).collect::<Vec<_>>();
    crate::preparation_reservations::assert_available(d,&affected,Some(origin))?;
    let child=crate::id();
    let intent=json!({"schemaVersion":1,"identity":{"originatingAnsweringAttemptId":origin,"roundOrdinal":1},
        "planSha256":plan["planSha256"],"status":"reserved","retryAuthorized":false,"createdAt":at,
        "owner":{"account":owner.account,"runtimeId":owner.runtime_id,"releaseSha256":owner.release_sha256,"epoch":owner.epoch}});
    let round=json!({"originatingAnsweringAttemptId":origin,"roundOrdinal":1,"childJobId":child,"planSha256":plan["planSha256"],"paidIntent":intent});
    let company=d["account"].clone();let binding=d["connectorBinding"].clone();
    let root=crate::row_mut(d,"jobs",origin)?;
    root["preparationStages"]["repairBudget"]=json!({"schemaVersion":1,"maxRounds":1,"consumedRounds":spent+1,"authority":"admitted_workflow_v1"});
    root["preparationStages"]["answeringRepairs"]=json!([round]);
    crate::list_mut(d,"jobs").push(json!({"id":child,"kind":"assistant","purpose":"answering_repair","status":"running","account":company,
        "connectorBinding":binding,"originatingAnsweringAttemptId":origin,"roundOrdinal":1,"answeringRepairPlan":plan,"repairPaidIntent":intent,"prepareBundle":plan["prepareBundle"],"createdAt":at}));
    // This scope capture belongs to the newly admitted native child and is
    // committed atomically with its original one-round claim. Never mint it on
    // a historical UNKNOWN child while resuming or projecting a saved result.
    let reservation=crate::preparation_reservations::capture(d,&child)?;
    crate::row_mut(d,"jobs",&child)?["scopeReservation"]=reservation;
    Ok(json!({"jobId":child,"plan":plan,"paidIntent":intent}))
}
#[cfg(test)]
fn claim_fenced(d:&mut Value,plan:&Value,at:&str)->crate::ApiResult<Value>{
    let owner=crate::runtime_lifecycle::OwnerToken{account:d["account"].as_str().unwrap_or("").to_owned(),runtime_id:"isolated-material-fixture".to_owned(),release_sha256:"e".repeat(64),epoch:1};
    claim_owned(d,plan,at,&owner)
}
pub(crate) fn validate_change(before:&Value,after:&Value)->crate::ApiResult<()> {
    let (old,new)=match (checked_jobs(before)?,checked_jobs(after)?){(None,None)=>return Ok(()),(Some(a),Some(b))=>(a,b),_=>return Err(crate::internal("Repair job scope changed"))};
    for job in old{
        if let Some((budget,claims))=budget(job)?{
            let matching=new.iter().filter(|j|j["id"]==job["id"]).collect::<Vec<_>>();
            if matching.len()!=1{return Err(crate::conflict("Repair origin deleted or duplicated"));}
            if !job["repairMergeReceipt"].is_null()&&matching[0]["repairMergeReceipt"]!=job["repairMergeReceipt"]{
                return Err(crate::conflict("Repair merge receipt cannot be rebound"));
            }
            let (next,next_claims)=self::budget(matching[0])?.ok_or_else(||crate::conflict("Repair budget cannot be removed"))?;
            if next["consumedRounds"].as_u64().unwrap()<budget["consumedRounds"].as_u64().unwrap()||!next_claims.starts_with(claims){return Err(crate::conflict("Original repair count/claims cannot be reset"));}
        }
        if job.get("originatingAnsweringAttemptId").is_some(){
            let matching=new.iter().filter(|j|j["id"]==job["id"]).collect::<Vec<_>>();
            if matching.len()!=1||["originatingAnsweringAttemptId","roundOrdinal","answeringRepairPlan"].iter().any(|k|matching[0][*k]!=job[*k])
                ||matching[0]["repairPaidIntent"]["identity"]!=job["repairPaidIntent"]["identity"]||matching[0]["repairPaidIntent"]["planSha256"]!=job["repairPaidIntent"]["planSha256"]||matching[0]["repairPaidIntent"]["owner"]!=job["repairPaidIntent"]["owner"]{return Err(crate::conflict("Repair child lineage cannot be removed or rebound"));}
            validate_job_change(&json!({"jobs":[job]}),&json!({"jobs":[matching[0]]}))?;
        }
    }
    let mut ids=std::collections::BTreeSet::new();
    for job in new{
        let id=job["id"].as_str().ok_or_else(||crate::bad("Repair job identity missing"))?;if !ids.insert(id){return Err(crate::bad("Repair job identity duplicated"));}
        if let Some((_,claims))=budget(job)?{for claim in claims{
            if claim["originatingAnsweringAttemptId"]!=id||claim["roundOrdinal"]!=1{return Err(crate::conflict("Repair claim identity changed"));}
            let children=new.iter().filter(|j|j["id"]==claim["childJobId"]).collect::<Vec<_>>();
            if children.len()!=1||children[0]["originatingAnsweringAttemptId"]!=id||children[0]["roundOrdinal"]!=1||children[0]["answeringRepairPlan"]["planSha256"]!=claim["planSha256"]{return Err(crate::conflict("Repair claim child closure missing or rebound"));}
        }}
        if job.get("originatingAnsweringAttemptId").is_some(){
            validate(&job["answeringRepairPlan"])?;
            if !old.iter().any(|previous|previous["id"]==job["id"])
                &&(job["scopeReservation"].is_null()||crate::preparation_reservations::capture(after,id)?!=job["scopeReservation"]){
                return Err(crate::conflict("New repair child must retain its atomic native scope capture"));
            }
            let parents=new.iter().filter(|parent|parent["id"]==job["originatingAnsweringAttemptId"]).collect::<Vec<_>>();
            if parents.len()!=1||rows(&parents[0]["preparationStages"],"answeringRepairs").iter().filter(|claim|claim["childJobId"]==id&&claim["roundOrdinal"]==1&&claim["planSha256"]==job["answeringRepairPlan"]["planSha256"]).count()!=1
                ||job["prepareBundle"]!=job["answeringRepairPlan"]["prepareBundle"]{return Err(crate::conflict("Repair child root closure missing or changed"));}
            if job["roundOrdinal"]!=1||job["repairPaidIntent"]["identity"]!=json!({"originatingAnsweringAttemptId":job["originatingAnsweringAttemptId"],"roundOrdinal":1})
                ||job["repairPaidIntent"]["planSha256"]!=job["answeringRepairPlan"]["planSha256"]||job["repairPaidIntent"]["retryAuthorized"]!=false{return Err(crate::conflict("Repair paid intent changed"));}
            let owner=crate::runtime_lifecycle::parse_token(&job["repairPaidIntent"]["owner"])?;if owner.account!=job["answeringRepairPlan"]["companyId"].as_str().unwrap_or(""){return Err(crate::conflict("Repair admission owner company changed"));}
        }
    }Ok(())
}
/// Storage explicitly declares a ONE-job projection. It may settle existing
/// child progress, but cannot create/reset/rebind root claims or lineage here.
/// Complete root/child existence is checked only by the full-domain validator.
pub(crate) fn validate_job_change(before:&Value,after:&Value)->crate::ApiResult<()> {
    let old=checked_jobs(before)?.ok_or_else(||crate::bad("Bounded repair job missing"))?;let next=checked_jobs(after)?.ok_or_else(||crate::bad("Bounded repair job missing"))?;
    if old.len()!=1||next.len()!=1||old[0]["id"]!=next[0]["id"]{return Err(crate::conflict("Bounded repair writer must retain one exact job"));}
    let (a,b)=(&old[0],&next[0]);budget(a)?;budget(b)?;
    if a["preparationStages"]["repairBudget"]!=b["preparationStages"]["repairBudget"]||a["preparationStages"]["answeringRepairs"]!=b["preparationStages"]["answeringRepairs"]
        ||a.get("originatingAnsweringAttemptId")!=b.get("originatingAnsweringAttemptId")||a.get("roundOrdinal")!=b.get("roundOrdinal")||a.get("answeringRepairPlan")!=b.get("answeringRepairPlan")||a.get("originatingAnsweringAttemptId").is_some()&&a["prepareBundle"]!=b["prepareBundle"]{return Err(crate::conflict("Bounded repair writer cannot create or edit claims/lineage"));}
    if !a["repairMergeReceipt"].is_null()&&a["repairMergeReceipt"]!=b["repairMergeReceipt"]{
        return Err(crate::conflict("Repair merge receipt cannot be rebound"));
    }
    if a.get("originatingAnsweringAttemptId").is_some(){validate(&b["answeringRepairPlan"])?;
        if a.get("scopeReservation")!=b.get("scopeReservation"){
            return Err(crate::conflict("Repair child scope cannot be removed, recaptured or minted on resume"));
        }
        if a["status"]=="completed"&&(a["result"]!=b["result"]||a["prepareOutcome"]!=b["prepareOutcome"]){
            return Err(crate::conflict("Completed repair paid result and admitted outcome cannot be replaced"));
        }
        if b["repairPaidIntent"]["identity"]!=a["repairPaidIntent"]["identity"]||b["repairPaidIntent"]["planSha256"]!=a["repairPaidIntent"]["planSha256"]||b["repairPaidIntent"]["owner"]!=a["repairPaidIntent"]["owner"]||b["repairPaidIntent"]["retryAuthorized"]!=false{return Err(crate::conflict("Bounded repair paid identity changed"));}
        let from=a["repairPaidIntent"]["status"].as_str().unwrap_or("");let to=b["repairPaidIntent"]["status"].as_str().unwrap_or("");
        if from!=to&&!matches!((from,to),("reserved","dispatching")|("dispatching","unknown")|("dispatching","completed")|("unknown","completed")){return Err(crate::conflict("Repair paid intent cannot reset"));}
    }else if a.get("repairPaidIntent")!=b.get("repairPaidIntent"){return Err(crate::conflict("Bounded writer cannot add repair paid authority"));}Ok(())
}
/// Schedule/source/status projections are declared incomplete inventories.
/// Protected jobs present in them are read-only and new repair/frame authority
/// is forbidden. Absence never proves that a root/child does not exist.
pub(crate) fn validate_readonly_projection_change(before:&Value,after:&Value)->crate::ApiResult<()> {
    let (old,new)=match (checked_jobs(before)?,checked_jobs(after)?){(None,None)=>return Ok(()),(Some(a),Some(b))=>(a,b),_=>return Err(crate::bad("Bounded jobs projection changed"))};
    for job in old.iter().chain(new){budget(job)?;}
    for job in old.iter().filter(|j|protected(j)){
        let matching=new.iter().filter(|j|j["id"]==job["id"]).collect::<Vec<_>>();if matching.len()!=1||matching[0]!=job{return Err(crate::conflict("Bounded projection cannot modify protected repair/frame job"));}
    }
    for job in new.iter().filter(|j|protected(j)){
        let matching=old.iter().filter(|j|j["id"]==job["id"]).collect::<Vec<_>>();if matching.len()!=1||matching[0]!=job{return Err(crate::conflict("Bounded projection cannot create repair/frame authority"));}
    }Ok(())
}
fn protected(job:&Value)->bool {job["purpose"]=="answering_repair"||job["purpose"]=="targeted_video_frames"
    ||["originatingAnsweringAttemptId","roundOrdinal","answeringRepairPlan","repairPaidIntent","frameNeed","framePlan","frameLease","frameResult"].iter().any(|key|job.get(*key).is_some())
    ||job["preparationStages"].get("repairBudget").is_some()||job["preparationStages"].get("answeringRepairs").is_some()}
fn checked_jobs(v:&Value)->crate::ApiResult<Option<&[Value]>>{match v.get("jobs"){None=>Ok(None),Some(v)=>v.as_array().map(|v|Some(v.as_slice())).ok_or_else(||crate::bad("Repair jobs must be an array"))}}
fn budget(job:&Value)->crate::ApiResult<Option<(&Value,&[Value])>>{
    let stage=&job["preparationStages"];match (stage.get("repairBudget"),stage.get("answeringRepairs")){
        (None,None)=>Ok(None),(Some(b),Some(c))=>{
            let claims=c.as_array().ok_or_else(||crate::bad("Repair claims must be an array"))?;
            let count=b["consumedRounds"].as_u64().filter(|n|*n<=1).ok_or_else(||crate::bad("Repair budget invalid"))?;
            if !b.is_object()||b["schemaVersion"]!=1||b["maxRounds"]!=1||b["authority"]!="admitted_workflow_v1"||claims.len()!=count as usize{return Err(crate::bad("Repair budget and claims disagree"));}Ok(Some((b,claims.as_slice())))
        },_=>Err(crate::bad("Repair budget and claims must both exist")),
    }
}
/// After original settlement, extract only explicitly requested frames and run
/// at most one native admitted answering repair. Original paid evidence stays
/// immutable; any uncertain child may only recover its original captured result.
pub(crate) async fn run_pending(app:&crate::App,origin:&str)->crate::ApiResult<Option<Value>>{
    let snapshot=app.db.read_preparation_context(origin).await?;let root=crate::row(&snapshot,"jobs",origin)?;
    if rows(root,"videoFrameNeeds").is_empty(){return Ok(None);}

    let children=rows(&snapshot,"jobs").iter().filter(|j|j["originatingAnsweringAttemptId"]==origin).cloned().collect::<Vec<_>>();
    if children.len()>1{return Err(crate::conflict("Repair child identity is ambiguous"));}
    if root["purpose"]=="auto_revalidate"{
        let affected=rows(root,"videoFrameNeeds").iter().flat_map(|n|rows(n,"affectedRecipientIds").iter().filter_map(Value::as_str)).collect::<std::collections::BTreeSet<_>>().into_iter().map(|id|json!(id)).collect::<Vec<_>>();
        let phase=if children.first().is_some_and(|c|c["status"]=="completed"){crate::auto_prepare::revalidation::RepairPhase::AfterAdmission}else{crate::auto_prepare::revalidation::RepairPhase::BeforeAdmission};
        crate::auto_prepare::revalidation::repair_current(&snapshot,origin,&affected,phase)?;
    }
    if let Some(child)=children.first(){return settle_retained_child(app,child).await;}
    drop(snapshot);
    let Some(results)=crate::video_frame_work::run_pending(app,origin).await? else{return Ok(None)};
    let snapshot=app.db.read_preparation_context(origin).await?;let root=crate::row(&snapshot,"jobs",origin)?;let needs=rows(root,"videoFrameNeeds").to_vec();
    let ids=needs.iter().flat_map(|n|rows(n,"affectedRecipientIds").iter().filter_map(|id|id.as_str().map(str::to_owned))).collect::<std::collections::BTreeSet<_>>().into_iter().map(|id|json!(id)).collect::<Vec<_>>();
    let mut bundle=crate::prepare_bundle::build_engine_capture(&snapshot,&ids,&[]).map_err(crate::conflict)?;
    let original=&root["prepareBundle"]["request"];
    for key in ["purpose","preparationMode","responseContract","modelContextContract","researchPolicy","researchLimitContract","sharedModerationContext","contextSufficiencyContract","recoveryEvidenceContract","factDependencyContract","visualNeedContract","previousDecision"]{if let Some(value)=original.get(key){bundle["request"][key]=value.clone();}}
    crate::decision_media::attach_request(&snapshot,&mut bundle["request"]).map_err(crate::conflict)?;
    crate::preparation_unit::attach(&snapshot,&mut bundle,&crate::now()).map_err(crate::conflict)?;
    crate::preparation_materials::attach_request(&snapshot,&mut bundle["request"]).map_err(crate::conflict)?;
    let mut ordered=needs.clone();ordered.sort_by_key(|n|n["needId"].as_str().unwrap_or("").to_owned());
    bundle["request"]["optionalFrameRefs"]=json!(ordered.iter().flat_map(|n|crate::video_frame_work::frame_refs(results.iter().find(|r|r["needId"]==n["needId"]).unwrap(),n)).collect::<Vec<_>>());
    crate::decision_media::attach_request(&snapshot,&mut bundle["request"]).map_err(crate::conflict)?;
    let plan=capture(&snapshot,origin,&needs,&results,&bundle["request"],&crate::now()).map_err(crate::conflict)?;drop(snapshot);
    let token=app.lifecycle_admission_token(crate::runtime_lifecycle::AdmissionClass::Preparation).await?;
    let work=app.lifecycle_work.begin(crate::runtime_owned_work::Kind::Preparation)?;
    let reserved=match app.change(|d|claim(d,&token,&plan,&crate::now())).await{Ok(value)=>value,Err(error)=>{work.settled();return Err(error)}};
    let child_id=match reserved["jobId"].as_str(){Some(id)=>id.to_owned(),None=>{work.settled();return Err(crate::internal("Repair child missing"))}};
    let preflight=app.change(|d|{if crate::row(d,"jobs",origin)?["purpose"]=="auto_revalidate"{crate::auto_prepare::revalidation::repair_current(d,origin,rows(&plan,"affectedRecipientIds"),crate::auto_prepare::revalidation::RepairPhase::BeforeAdmission)?;}
        crate::preparation_materials::require_request(d,&plan["request"]).map_err(crate::conflict)?;crate::preparation_unit::current_request(d,&plan["request"],&crate::now()).map_err(crate::conflict)?;
        let affected=rows(&plan,"affectedRecipientIds").iter().filter_map(Value::as_str).map(str::to_owned).collect::<Vec<_>>();crate::preparation_reservations::assert_available(d,&affected,Some(&child_id))?;
        let j=crate::row_mut(d,"jobs",&child_id)?;if j["repairPaidIntent"]["status"]!="reserved"||j["answeringRepairPlan"]!=plan{return Err(crate::conflict("Repair paid intent already used"));}j["repairPaidIntent"]["status"]=json!("dispatching");Ok(())}).await;
    if let Err(error)=preflight{work.settled();return Err(error)}
    let generated=crate::runtime_lifecycle_app::with_job(child_id.clone(),app.bridge_admitted("assistant",plan["request"].clone(),work)).await;
    match generated{Ok(result)=>app.change(|d|settle_child(d,&child_id,&result)).await.map(Some),Err(error)=>{
        app.change_job(&child_id,|d|{let j=crate::row_mut(d,"jobs",&child_id)?;j["status"]=json!("unknown");j["error"]=json!("repair_paid_outcome_unresolved");j["repairPaidIntent"]["status"]=json!("unknown");Ok(())}).await?;Err(error)
    }}
}
async fn settle_retained_child(app:&crate::App,child:&Value)->crate::ApiResult<Option<Value>>{
    if child["status"]=="completed"{return Ok(Some(child["prepareOutcome"].clone()));}
    let id=child["id"].as_str().ok_or_else(||crate::internal("Repair child identity missing"))?;let request=&child["answeringRepairPlan"]["request"];
    let mut found=None;for paid in rows(child,"retainedEvidence").iter().filter(|r|r["binding"]["operation"]=="assistant"){
        require_paid_owner(child,paid)?;
        let captured=crate::runtime_paid_result::resolve(app,id,"assistant",paid).await?;
        if crate::preparation_review::first_capture_matches(app.account,request,&captured)?{if found.is_some(){return Err(crate::conflict("Multiple repair paid captures require reconciliation"));}found=Some(paid.clone());}
    }
    let Some(paid)=found else{return Err(crate::conflict("Repair child unresolved; no new round or paid replay allowed"))};
    let result=crate::model_material_receipt::recover(app,id,&paid).await?;
    app.change(|d|settle_child(d,id,&result)).await.map(Some)
}
fn require_paid_owner(child:&Value,paid:&Value)->crate::ApiResult<()>{
    let owner=&child["repairPaidIntent"]["owner"];let profile=crate::accounts::Profile::from_workspace(&json!({"account":owner["account"]}))?;
    crate::runtime_lifecycle::parse_token(owner)?;
    if paid["company"]!=profile.key()||paid["account"]!=profile.display()||paid["runtimeOwner"]!=json!({"account":owner["account"],"runtimeId":owner["runtimeId"],"releaseSha256":owner["releaseSha256"]}){return Err(crate::conflict("Repair paid capture owner differs from original admitted owner"));}Ok(())
}
/// Authenticate paid evidence before minting a local generation-only permit.
/// FIRST is the native normalized settled result; the child response is still
/// the exact paid payload with only its native material pointer attached.
pub(crate) fn generation_paid_closure(d:&Value,child_id:&str,result:&Value)->crate::ApiResult<Value>{
    let child=crate::row(d,"jobs",child_id)?;
    let origin=crate::required(child,"originatingAnsweringAttemptId")?;
    let root=crate::row(d,"jobs",origin)?;
    let first=&root["preparationStages"]["first"];
    let profile=crate::accounts::Profile::from_workspace(d)?;
    if child["status"]!="running"||!matches!(child["repairPaidIntent"]["status"].as_str(),Some("dispatching"|"unknown"))
        ||first["status"]!="completed"||root["videoFrameNeeds"]!=first["result"]["nativeVideoFrameNeeds"]{
        return Err(crate::conflict("Repair generation requires its settled original FIRST and paid child"));
    }
    for (job,request,returned) in [(root,&root["prepareBundle"]["request"],&first["result"]),
        (child,&child["answeringRepairPlan"]["request"],result)]{
        let receipt=crate::model_material_receipt::result_receipt(request,returned).map_err(crate::conflict)?
            .ok_or_else(||crate::conflict("Repair generation paid material receipt missing"))?;
        let paid=&receipt["paidResultRef"];
        if receipt["nativeJobId"]!=job["id"]||!rows(job,"modelMaterialReceipts").contains(&receipt)
            ||!rows(job,"retainedEvidence").contains(paid)||paid["company"]!=profile.key()||paid["account"]!=profile.display()
            ||paid["binding"]["nativeJobId"]!=job["id"]||paid["binding"]["operation"]!="assistant"
            ||!repair_paid_request_matches(profile,request,paid)?{
            return Err(crate::conflict("Repair generation original paid closure changed"));
        }
    }
    let receipt=crate::model_material_receipt::result_receipt(&child["answeringRepairPlan"]["request"],result)
        .map_err(crate::conflict)?.ok_or_else(||crate::conflict("Repair generation child receipt missing"))?;
    require_paid_owner(child,&receipt["paidResultRef"])?;
    let mut paid_response=result.clone();paid_response.as_object_mut().ok_or_else(||crate::conflict("Repair paid response missing"))?.remove("modelMaterialReceipt");
    if receipt["paidResultRef"]["responseSha256"]!=hash(&paid_response){
        return Err(crate::conflict("Repair generation response differs from its exact paid capture"));
    }
    Ok(receipt)
}
fn repair_paid_request_matches(profile:crate::accounts::Profile,request:&Value,paid:&Value)->crate::ApiResult<bool>{
    let mut direct=request.clone();profile.bind_request(&mut direct)?;direct["operation"]=json!("assistant");
    let wrapper=json!({"account":profile.key(),"operation":"assistant","request":request});
    // Keep the original stored hash. Only the two exact wire forms accepted
    // by native retained-capture recovery may match; no arbitrary normalization.
    for wire in [direct,wrapper]{
        if paid["requestSha256"]==hash(&wire){
            let capture=json!({"binding":paid["binding"],"company":paid["company"],"account":paid["account"],"request":wire});
            return crate::preparation_review::first_capture_matches(profile,request,&capture);
        }
    }Ok(false)
}
fn settle_child(d:&mut Value,id:&str,result:&Value)->crate::ApiResult<Value>{
    let child=crate::row(d,"jobs",id)?.clone();if child["status"]=="completed"{return Ok(child["prepareOutcome"].clone());}
    if !matches!(child["status"].as_str(),Some("running"|"unknown"|"interrupted"))||child["repairPaidIntent"]["retryAuthorized"]!=false{return Err(crate::conflict("Repair settlement ownership changed"));}
    let origin=crate::required(&child,"originatingAnsweringAttemptId")?;if crate::row(d,"jobs",origin)?["purpose"]=="auto_revalidate"{crate::auto_prepare::revalidation::repair_current(d,origin,rows(&child["answeringRepairPlan"],"affectedRecipientIds"),crate::auto_prepare::revalidation::RepairPhase::BeforeAdmission)?;}
    if json!(settlement_pins(d,rows(&child["answeringRepairPlan"],"affectedRecipientIds"))?)!=child["answeringRepairPlan"]["admissionPins"]{return Err(crate::conflict("Repair recipient changed during paid attempt; saved result remains preserved"));}
    let request=&child["answeringRepairPlan"]["request"];crate::preparation_materials::require_request(d,request).map_err(crate::conflict)?;crate::preparation_unit::current_request(d,request,&crate::now()).map_err(crate::conflict)?;
    let affected=rows(&child["answeringRepairPlan"],"affectedRecipientIds").iter().filter_map(Value::as_str).map(str::to_owned).collect::<Vec<_>>();crate::preparation_reservations::assert_available(d,&affected,Some(id))?;
    crate::preparation_review::plan_review(&child["answeringRepairPlan"]["request"],result).map_err(crate::conflict)?;
    let material=crate::model_material_receipt::result_receipt(&child["answeringRepairPlan"]["request"],result).map_err(crate::conflict)?.ok_or_else(||crate::conflict("Repair paid receipt missing"))?;require_paid_owner(&child,&material["paidResultRef"])?;
    crate::row_mut(d,"jobs",id)?["status"]=json!("running");let mut outcome=crate::prepare_bundle::admit_to(d,id,None,result)?;
    outcome["affectedRecipientIds"]=child["answeringRepairPlan"]["affectedRecipientIds"].clone();outcome["repairJobId"]=json!(id);outcome["finalAssessments"]=result["assessments"].clone();
    outcome["settlementPins"]=json!(settlement_pins(d,rows(&outcome,"affectedRecipientIds"))?);
    outcome["proposalSettlementPins"]=json!(proposal_pins(d,rows(&outcome,"candidates"))?);
    let further=crate::video_frame_work::record_needs(d,id,&child["answeringRepairPlan"]["request"],result,&crate::now())?;
    if further.as_array().is_some_and(|needs|!needs.is_empty()){crate::row_mut(d,"jobs",id)?["frameNeedOutcome"]=json!({"status":"held","reasonCode":"answering_repair_limit_exhausted","retryAuthorized":false,"at":crate::now()});}
    let job=crate::row_mut(d,"jobs",id)?;job["result"]=result.clone();job["prepareOutcome"]=outcome.clone();job["status"]=json!("completed");job["repairPaidIntent"]["status"]=json!("completed");job["finishedAt"]=json!(crate::now());Ok(outcome)
}
/// Publish the separately admitted repair outcome in the ordinary local view.
/// This does not rewrite original first results, approvals or paid captures.
pub(crate) fn merge_outcome(d:&mut Value,origin:&str,mut outcome:Value)->crate::ApiResult<Value>{
    let children=rows(d,"jobs").iter().filter(|j|j["originatingAnsweringAttemptId"]==origin&&j["status"]=="completed").collect::<Vec<_>>();
    if children.is_empty(){return Ok(outcome);}if children.len()!=1{return Err(crate::conflict("Repair completion ambiguous"));}
    let repaired=children[0]["prepareOutcome"].clone();let affected=rows(&repaired,"affectedRecipientIds");
    if affected!=rows(&children[0]["answeringRepairPlan"],"affectedRecipientIds")||rows(&repaired,"candidates").iter().chain(rows(&repaired,"finalAssessments")).any(|c|!affected.contains(&c["itemId"])){return Err(crate::conflict("Repair outcome escaped its affected recipient set"));}
    if !matches!(repaired["status"].as_str(),Some("review"|"discussed"))||rows(&repaired,"candidates").iter().any(|c|c["status"]!="review"){return Err(crate::conflict("Stale or rejected repair cannot replace current decisions"));}
    crate::preparation_materials::require_request(d,&children[0]["answeringRepairPlan"]["request"]).map_err(crate::conflict)?;
    let revalidation=crate::row(d,"jobs",origin)?["purpose"]=="auto_revalidate";if revalidation{crate::auto_prepare::revalidation::repair_current(d,origin,affected,crate::auto_prepare::revalidation::RepairPhase::AfterAdmission)?;}
    let previous=&crate::row(d,"jobs",origin)?["repairMergeReceipt"];let replay=previous["repairJobId"]==repaired["repairJobId"]&&previous["repairOutcomeSha256"]==hash(&repaired);let pins=if replay{rows(previous,"settlementPins")}else{rows(&repaired,"settlementPins")};
    if pins.len()!=affected.len()||json!(settlement_pins(d,affected)?)!=json!(pins){return Err(crate::conflict("Repair recipient changed after settlement; saved result remains preserved"));}
    if json!(proposal_pins(d,rows(&repaired,"candidates"))?)!=repaired["proposalSettlementPins"]{return Err(crate::conflict("Repair proposal changed after settlement; saved operator candidate remains preserved"));}
    if replay{return Ok(crate::row(d,"jobs",origin)?["prepareOutcome"].clone());}
    let automatic=crate::row(d,"jobs",origin)?["purpose"]=="auto_prepare";
    if revalidation{outcome=crate::auto_prepare::revalidation::merge_repair_projection(d,origin,outcome,&repaired)?;}else if automatic{merge_auto_projection(d,origin,&mut outcome,&repaired)?;}else{merge_admission(&mut outcome,&repaired);}
    outcome["repairJobId"]=repaired["repairJobId"].clone();let pins=settlement_pins(d,affected)?;let root=crate::row_mut(d,"jobs",origin)?;root["prepareOutcome"]=outcome.clone();root["repairMergeReceipt"]=json!({"schemaVersion":1,"repairJobId":repaired["repairJobId"],"repairOutcomeSha256":hash(&repaired),"settlementPins":pins,"proposalSettlementPins":repaired["proposalSettlementPins"]});Ok(outcome)
}
/// Full native records may be carried read-only in protected scopeOwners.
/// A reduced Boolean control is never paid lineage or settlement authority.
fn proof_job<'a>(d:&'a Value,id:&str)->crate::ApiResult<&'a Value>{
    let mut found=None;
    for job in rows(d,"jobs").iter().chain(rows(d,"scopeOwners")).filter(|j|j["id"]==id){
        if job["scopeOwnerProjection"]["version"]==1{return Err(crate::conflict("Repair proof requires a full native job record"));}
        if found.is_some_and(|previous|previous!=job){return Err(crate::conflict("Repair native job controls disagree"));}
        found=Some(job);
    }
    found.ok_or_else(||crate::conflict("Repair native job proof missing"))
}
fn proof_children<'a>(d:&'a Value,origin:&str)->crate::ApiResult<Vec<&'a Value>>{
    let mut children=std::collections::BTreeMap::new();
    for job in rows(d,"jobs").iter().chain(rows(d,"scopeOwners")).filter(|j|j["originatingAnsweringAttemptId"]==origin){
        let id=crate::required(job,"id")?;
        let exact=proof_job(d,id)?;
        children.insert(id,exact);
    }
    Ok(children.into_values().collect())
}
/// A terminal child is proven by native frozen root/child lineage and its
/// retained paid material receipt. This does not grant current-source authority.
pub(crate) fn settled_child<'a>(d:&'a Value,origin:&str)->crate::ApiResult<Option<&'a Value>>{
    let root=proof_job(d,origin)?;
    let children=proof_children(d,origin)?;
    if children.is_empty(){if budget(root)?.is_some_and(|(b,_)|b["consumedRounds"]!=0){return Err(crate::conflict("Repair consumed root has no proven child"));}return Ok(None);}
    if children.len()!=1{return Err(crate::conflict("Repair terminal child ambiguous"));}let child=children[0];
    let plan=&child["answeringRepairPlan"];validate(plan)?;
    let Some((count,claims))=budget(root)? else{return Err(crate::conflict("Repair terminal root budget missing"))};
    let mut original_needs=rows(root,"videoFrameNeeds").to_vec();
    original_needs.sort_by_key(|need|need["needId"].as_str().unwrap_or("").to_owned());
    if original_needs.is_empty()||json!(original_needs)!=plan["needSet"]{
        return Err(crate::conflict("Repair terminal original need lineage changed"));
    }
    if root.get("originatingAnsweringAttemptId").is_some()||root["preparationStages"]["first"]["status"]!="completed"
        ||count["consumedRounds"]!=1||claims.len()!=1||claims[0]["childJobId"]!=child["id"]
        ||claims[0]["originatingAnsweringAttemptId"]!=origin||claims[0]["roundOrdinal"]!=1||claims[0]["planSha256"]!=plan["planSha256"]
        ||child["kind"]!="assistant"||child["purpose"]!="answering_repair"||child["status"]!="completed"||child["roundOrdinal"]!=1
        ||child["account"]!=d["account"]||child["connectorBinding"]!=crate::active_binding(d)?.to_json()
        ||plan["companyId"]!=d["account"]||plan["originatingAnsweringAttemptId"]!=origin
        ||child["prepareBundle"]!=plan["prepareBundle"]||plan["prepareBundle"]["digest"]!=hash(&plan["request"])
        ||plan["prepareBundle"]["request"]!=plan["request"]||plan["request"]["account"]!=d["account"]
        ||plan["request"]["connectorBinding"]!=crate::active_binding(d)?.to_json()
        ||child["repairPaidIntent"]["status"]!="completed"||child["repairPaidIntent"]["retryAuthorized"]!=false
        ||child["repairPaidIntent"]["identity"]!=json!({"originatingAnsweringAttemptId":origin,"roundOrdinal":1})
        ||child["repairPaidIntent"]["planSha256"]!=plan["planSha256"]||claims[0]["paidIntent"]["owner"]!=child["repairPaidIntent"]["owner"] {
        return Err(crate::conflict("Repair terminal lineage changed"));
    }
    if child["scopeReservation"].is_null()||crate::preparation_reservations::capture(d,crate::required(child,"id")?)?!=child["scopeReservation"]{
        return Err(crate::conflict("Repair terminal native scope capture changed"));
    }
    let outcome=&child["prepareOutcome"];
    if outcome["repairJobId"]!=child["id"]||outcome["affectedRecipientIds"]!=plan["affectedRecipientIds"]||outcome["finalAssessments"]!=child["result"]["assessments"]
        ||!matches!(outcome["status"].as_str(),Some("review"|"discussed"))
        ||rows(outcome,"candidates").iter().chain(rows(outcome,"finalAssessments")).any(|c|!rows(plan,"affectedRecipientIds").contains(&c["itemId"])) {
        return Err(crate::conflict("Repair terminal outcome changed"));
    }
    let receipt=crate::model_material_receipt::result_receipt(&plan["request"],&child["result"]).map_err(crate::conflict)?.ok_or_else(||crate::conflict("Repair terminal paid material receipt missing"))?;
    require_paid_owner(child,&receipt["paidResultRef"])?;
    if receipt["nativeJobId"]!=child["id"]||!rows(child,"retainedEvidence").contains(&receipt["paidResultRef"])
        ||!rows(child,"modelMaterialReceipts").contains(&receipt){return Err(crate::conflict("Repair terminal paid closure missing"));}
    Ok(Some(child))
}
/// Successful merge receipt connects workflow ownership to an unchanged paid
/// child. Consumers separately validate current material/source/proposal state.
pub(crate) fn merged_child<'a>(d:&'a Value,origin:&str)->crate::ApiResult<Option<&'a Value>>{
    let root=proof_job(d,origin)?;let receipt=&root["repairMergeReceipt"];
    if receipt.is_null(){return Ok(None);}
    let child=settled_child(d,origin)?.ok_or_else(||crate::conflict("Merged repair child missing"))?;
    if receipt["schemaVersion"]!=1||receipt["repairJobId"]!=child["id"]||receipt["repairOutcomeSha256"]!=hash(&child["prepareOutcome"])
        ||root["prepareOutcome"]["repairJobId"]!=child["id"]||receipt["proposalSettlementPins"]!=child["prepareOutcome"]["proposalSettlementPins"] {
        return Err(crate::conflict("Merged repair receipt changed"));
    }Ok(Some(child))
}
pub(crate) fn proposal_content_hash(proposal:&Value)->String{
    let mut content=proposal.clone();if let Some(fields)=content.as_object_mut(){for key in ["status","revision","updatedAt","staleReason","staleAt"]{fields.remove(key);}}hash(&content)
}
/// Resolve workflow ownership without relabeling a proposal's paid prepareRunId.
/// Lawful machine retirement may change status/revision; human content cannot.
pub(crate) fn automatic_proposal_origin(d:&Value,proposal:&Value)->Option<String>{
    let run=proposal["prepareRunId"].as_str().or_else(||proposal["recovery"]["prepareRunId"].as_str())?;
    let job=crate::row(d,"jobs",run).ok()?;
    if matches!(job["purpose"].as_str(),Some("auto_prepare"|"auto_revalidate")){return Some(run.to_owned());}
    if job["purpose"]!="answering_repair"||proposal["origin"].is_object()||rows(proposal,"history").iter().next().is_some(){return None;}
    let origin=job["originatingAnsweringAttemptId"].as_str()?;let root=crate::row(d,"jobs",origin).ok()?;
    if !matches!(root["purpose"].as_str(),Some("auto_prepare"|"auto_revalidate")){return None;}
    let child=merged_child(d,origin).ok()??;
    let candidate=rows(&child["prepareOutcome"],"candidates").iter().find(|c|c["proposalId"]==proposal["id"]&&c["itemId"]==proposal["itemId"]&&c["status"]=="review")?;
    let pin=rows(&child["prepareOutcome"],"proposalSettlementPins").iter().find(|p|p["proposalId"]==candidate["proposalId"])?;
    if proposal["prepareBundleId"]!=child["prepareBundle"]["id"]||proposal["prepareBundleDigest"]!=child["prepareBundle"]["digest"]
        ||pin["contentSha256"]!=proposal_content_hash(proposal)||pin["textSha256"]!=crate::editorial_review::hash_text(proposal["text"].as_str().unwrap_or("")) {
        return None;
    }Some(origin.to_owned())
}
fn settlement_pins(d:&Value,affected:&[Value])->crate::ApiResult<Vec<Value>>{
    let mut seen=std::collections::BTreeSet::new();let mut pins=Vec::new();for id in affected{
        let id=id.as_str().ok_or_else(||crate::conflict("Repair recipient identity invalid"))?;if !seen.insert(id){return Err(crate::conflict("Repair recipient duplicated"));}
        let item=crate::row(d,"items",id)?;let state=json!({"draft":item["draft"],"draftEdited":item["draftEdited"],"decision":item["decision"],"tags":item["triageTags"],"reason":item["reason"],"workflow":item["workflow"],"humanOverrideAt":item["autoPreparation"]["humanOverrideAt"]});
        pins.push(json!({"itemId":id,"itemRevision":item["revision"],"dependencyDigest":crate::prepare_bundle::fingerprint(d,id).map_err(crate::conflict)?,"decisionStateSha256":hash(&state)}));
    }Ok(pins)
}
fn proposal_pins(d:&Value,candidates:&[Value])->crate::ApiResult<Vec<Value>>{
    let mut seen=std::collections::BTreeSet::new();let mut pins=Vec::new();for candidate in candidates{
        let id=candidate["proposalId"].as_str().ok_or_else(||crate::conflict("Repair saved proposal identity missing"))?;
        if !seen.insert(id){return Err(crate::conflict("Repair saved proposal duplicated"));}let proposal=crate::row(d,"proposals",id)?;
        if proposal["status"]!="draft"||proposal["revision"].as_u64().is_none_or(|r|r==0)||proposal["itemId"]!=candidate["itemId"]{return Err(crate::conflict("Repair saved proposal no longer an exact admitted draft"));}
        pins.push(json!({"proposalId":id,"proposalRevision":proposal["revision"],"status":proposal["status"],"textSha256":crate::editorial_review::hash_text(proposal["text"].as_str().unwrap_or("")),"proposalSha256":hash(proposal),"contentSha256":proposal_content_hash(proposal)}));
    }Ok(pins)
}
pub(crate) fn merge_admission(outcome:&mut Value,repaired:&Value){
    let affected=rows(repaired,"affectedRecipientIds");let mut candidates=rows(outcome,"candidates").iter().filter(|c|!affected.contains(&c["itemId"])).cloned().collect::<Vec<_>>();candidates.extend(rows(repaired,"candidates").iter().cloned());outcome["candidates"]=json!(candidates);
    for key in ["held","needsAttention"]{let mut values=rows(outcome,key).iter().filter(|c|!affected.contains(&c["itemId"])).cloned().collect::<Vec<_>>();values.extend(rows(repaired,"finalAssessments").iter().filter(|a|a["outcome"]=="needs_attention").map(|a|json!({"itemId":a["itemId"],"reason":a["reason"]})));outcome[key]=json!(values);}
    outcome["preparedItemIds"]=json!(candidates.iter().filter(|c|c["status"]=="review").map(|c|c["itemId"].clone()).collect::<Vec<_>>());if candidates.iter().any(|c|c["status"]=="review"){outcome["status"]=json!("review");}
}
fn merge_auto_projection(d:&mut Value,origin:&str,outcome:&mut Value,repaired:&Value)->crate::ApiResult<()> {
    if !outcome["admission"].is_object(){return Err(crate::conflict("Automatic repair requires original durable admission"));}
    let mut updates=Vec::new();
    for id in rows(repaired,"affectedRecipientIds"){
        let item_id=id.as_str().ok_or_else(||crate::conflict("Repair recipient missing"))?;let item=crate::row(d,"items",item_id)?;
        if item["autoPreparation"]["jobId"]!=origin||!item["autoPreparation"]["humanOverrideAt"].is_null()||item["draftEdited"]==true{return Err(crate::conflict("Automatic repair item ownership changed"));}
        let assessments=rows(repaired,"finalAssessments").iter().filter(|a|a["itemId"]==*id).collect::<Vec<_>>();if assessments.len()!=1{return Err(crate::conflict("Automatic repair assessment coverage changed"));}
        let assessment=assessments[0];let candidate=rows(repaired,"candidates").iter().find(|c|c["itemId"]==*id&&c["status"]=="review");
        if let Some(candidate)=candidate{let proposal=crate::row(d,"proposals",candidate["proposalId"].as_str().ok_or_else(||crate::conflict("Repair proposal missing"))?)?;
            if proposal["prepareRunId"]!=repaired["repairJobId"]||proposal["itemId"]!=*id||item["revision"]!=proposal["itemRevision"]||item["workflow"]!="prepared"{return Err(crate::conflict("Automatic repair proposal no longer owns its saved draft"));}}
        let matching=if outcome["items"].is_array(){rows(outcome,"items").iter().filter(|row|row["itemId"]==*id).count()}else{usize::from(outcome["itemId"]==*id)};
        if matching!=1{return Err(crate::conflict("Automatic repair original outcome coverage changed"));}
        updates.push((item_id.to_owned(),assessment.clone(),candidate.is_some()));
    }
    merge_admission(&mut outcome["admission"],repaired);
    for (id,assessment,prepared) in updates{
        let status=if prepared{"prepared"}else{"needs_attention"};let item=crate::row_mut(d,"items",&id)?;
        item["autoPreparation"]["status"]=json!(status);item["autoPreparation"]["reason"]=assessment["reason"].clone();item["autoPreparation"]["updatedAt"]=json!(crate::now());item["autoPreparation"]["retryAt"]=Value::Null;item["autoPreparation"]["repairJobId"]=repaired["repairJobId"].clone();
        item["decision"]=assessment["outcome"].clone();item["triageTags"]=assessment["tags"].clone();item["reason"]=assessment["reason"].clone();if prepared{item["workflow"]=json!("prepared");}
        let row=if outcome["items"].is_array(){outcome["items"].as_array_mut().unwrap().iter_mut().find(|row|row["itemId"]==id).unwrap()}else{&mut *outcome};row["status"]=json!(status);row["reason"]=assessment["reason"].clone();row["repairJobId"]=repaired["repairJobId"].clone();
    }
    if outcome["items"].is_array(){outcome["status"]=json!(if rows(outcome,"items").iter().all(|row|row["status"]=="prepared"){"prepared"}else if rows(outcome,"items").iter().any(|row|row["status"]=="stale"){"stale"}else{"needs_attention"});}Ok(())
}
/// Annotate only the repair branch. Never replay the original automatic reducer
/// or persist provider errors/secrets after the original admission completed.
pub(crate) fn record_work_failure(d:&mut Value,origin:&str,_reason:&str)->crate::ApiResult<()> {
    let children=rows(d,"jobs").iter().filter(|j|j["originatingAnsweringAttemptId"]==origin).map(|j|json!({"jobId":j["id"],"status":j["status"],"paidIntentStatus":j["repairPaidIntent"]["status"]})).collect::<Vec<_>>();
    crate::row_mut(d,"jobs",origin)?["frameNeedOutcome"]=json!({"status":"held","reasonCode":"repair_work_unresolved","retryAuthorized":false,"children":children,"at":crate::now()});Ok(())
}
/// Explicit ordinary resume of this original, already admitted work. A child
/// with uncertain paid outcome is admitted only to original capture recovery;
/// run_pending never dispatches an existing child again or refunds its slot.
pub(crate) fn claim_pending_resume(d:&mut Value,token:&crate::runtime_lifecycle::OwnerToken,origin:&str,request_digest:&str,need_digest:&str,at:&str)->crate::ApiResult<String>{
    crate::runtime_lifecycle::require_admission(d,token,crate::runtime_lifecycle::AdmissionClass::Preparation)?;
    let root=crate::row(d,"jobs",origin)?.clone();let request=&root["prepareBundle"]["request"];

    if root["kind"]!="assistant"||!matches!(root["purpose"].as_str(),Some("engine_prepare"|"auto_prepare"|"auto_revalidate"))||root.get("originatingAnsweringAttemptId").is_some()
        ||!matches!(root["status"].as_str(),Some("completed"|"failed"|"interrupted"))||!root["prepareOutcome"].is_object()
        ||root["preparationStages"]["first"]["status"]!="completed"||root["prepareBundle"]["digest"]!=request_digest||hash(request)!=request_digest
        ||request["account"]!=d["account"]||request["connectorBinding"]!=crate::active_binding(d)?.to_json()
        ||rows(&root,"videoFrameNeeds").is_empty()||hash(&root["videoFrameNeeds"])!=need_digest
        ||rows(&root["preparationStages"],"groupAdmission").iter().any(|g|g["status"]=="pending"){return Err(crate::conflict("Pending repair resume does not match original settled work"));}
    for need in rows(&root,"videoFrameNeeds"){crate::video_frame_work::current_need(d,need).map_err(crate::conflict)?;}
    let children=rows(d,"jobs").iter().filter(|j|j["originatingAnsweringAttemptId"]==origin).collect::<Vec<_>>();
    if children.len()>1{return Err(crate::conflict("Pending repair child ambiguous"));}
    if root["purpose"]=="auto_revalidate"{
        let affected=rows(&root,"videoFrameNeeds").iter().flat_map(|n|rows(n,"affectedRecipientIds").iter().filter_map(Value::as_str)).collect::<std::collections::BTreeSet<_>>().into_iter().map(|id|json!(id)).collect::<Vec<_>>();
        let phase=if children.first().is_some_and(|c|c["status"]=="completed"){crate::auto_prepare::revalidation::RepairPhase::AfterAdmission}else{crate::auto_prepare::revalidation::RepairPhase::BeforeAdmission};
        crate::auto_prepare::revalidation::repair_current(d,origin,&affected,phase)?;
    }
    if let Some(child)=children.first(){validate(&child["answeringRepairPlan"])?;validate_change(d,d)?;
        let mut other=d.clone();other["jobs"]=json!(rows(d,"jobs").iter().filter(|j|j["id"]!=child["id"]).cloned().collect::<Vec<_>>());
        if crate::preparation_workers::pending_conflict(&other,&root,true){return Err(crate::conflict("Another assistant owns pending repair scope"));}
    }else{
        if budget(&root)?.is_some(){return Err(crate::conflict("Consumed repair budget has no proven child"));}
        let ids=rows(&root,"videoFrameNeeds").iter().flat_map(|n|rows(n,"affectedRecipientIds")).filter_map(Value::as_str).map(str::to_owned).collect::<std::collections::BTreeSet<_>>().into_iter().collect::<Vec<_>>();
        crate::preparation_reservations::assert_available(d,&ids,Some(origin))?;
        if crate::preparation_workers::pending_conflict(d,&root,true){return Err(crate::conflict("Another assistant owns pending repair scope"));}
    }
    let purpose=root["purpose"].as_str().unwrap().to_owned();let saved=crate::row_mut(d,"jobs",origin)?;saved["status"]=json!("running");saved["finishedAt"]=Value::Null;saved["error"]=Value::Null;
    saved["pendingWorkResume"]=json!({"schemaVersion":1,"requestDigest":request_digest,"needSetDigest":need_digest,"maxRepairRounds":1,"owner":{"account":token.account,"runtimeId":token.runtime_id,"releaseSha256":token.release_sha256,"epoch":token.epoch},"at":at});Ok(purpose)
}
#[cfg(test)]#[path="answering_repair_plan_tests.rs"]pub(crate) mod tests;
