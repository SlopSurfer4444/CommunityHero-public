//! Durable local editorial admission. Reviews exact drafts; never changes text,
//! approves an action or publishes. Lost replies recover the original job.
use crate::*;
use axum::Extension;

fn input(body:&Value)->ApiResult<()> {
    let fields=body.as_object().ok_or_else(||bad("Editorial review requires an object"))?;
    if !fields.contains_key("requestId") || !fields.contains_key("proposals")
        || fields.keys().any(|key|!["requestId","proposals","fresh"].contains(&key.as_str()))
        || fields.get("fresh").is_some_and(|value|!value.is_boolean()) {
        return Err(bad("Editorial review requires requestId and proposals"));
    }
    Ok(())
}

pub(crate) fn schedule(d:&mut Value,actor:&operator_auth::Actor,body:&Value)->ApiResult<(Value,Option<String>)> {
    input(body)?;
    let admission=local_admission::request(d,"editorial",body,actor)?
        .ok_or_else(||bad("Editorial review requires requestId"))?;
    if let Some(result)=local_admission::replay(d,&admission,actor)? {return Ok((result,None));}
    let plan=if body["fresh"]==true {editorial_review::plan_fresh(d,&body["proposals"],&now())}
        else {editorial_review::plan_new(d,&body["proposals"],&now())}.map_err(bad)?;
    let pending:Vec<_>=list(d,"jobs").iter().filter(|j|j["kind"]=="editorial_review"
        && matches!(j["status"].as_str(),Some("running"|"queued"))).collect();
    // Reused receipts and non-reply decisions require no model lane capacity.
    // They still get the same durable admission and duplicate-scope checks.
    if !plan["batches"].as_array().is_some_and(|batches|batches.is_empty()) && pending.len()>=10 {
        return Err(conflict("Editorial queue is full; recover existing reviews first"));
    }
    if pending.iter().any(|j|j["editorialReferences"].as_array().into_iter().flatten()
        .any(|old|body["proposals"].as_array().into_iter().flatten().any(|new|new==old))) {
        return Err(conflict("These exact proposals already have an editorial review in progress"));
    }
    if plan.to_string().len()>8*1024*1024 {return Err(bad("Editorial batch context exceeds 8 MiB; select fewer proposals"));}
    let job=new_job(d,"editorial_review",required(body,"requestId")?)?;
    let j=row_mut(d,"jobs",&job)?;
    j["purpose"]=json!("editorial_review");
    j["operatorId"]=json!(actor.id);
    j["editorialReferences"]=body["proposals"].clone();
    j["editorialPlan"]=plan;
    j["editorialBatches"]=json!([]);
    if body.get("fresh").is_some(){j["editorialFresh"]=body["fresh"].clone();}
    let mut result=json!({"jobId":job});
    local_admission::commit(d,&admission,&mut result)?;
    Ok((result,Some(job)))
}

pub(crate) async fn post(State(app):State<App>,Extension(actor):Extension<operator_auth::Actor>,Json(body):Json<Value>)->ApiResult<Json<Value>> {
    input(&body)?;
    if let Some(result)=local_admission::replay_committed(&app,"editorial",&body,&actor).await? {return Ok(Json(result));}
    let (result,job)=app.change_admission(storage::AdmissionScope::EditorialSchedule(&body),|d|schedule(d,&actor,&body)).await?;
    if let Some(job)=job {let worker=app.clone();app.spawn(job.clone(),run(worker,job));}
    Ok(Json(result))
}

pub(crate) async fn get(State(app):State<App>,Extension(actor):Extension<operator_auth::Actor>,Path(key):Path<String>)->ApiResult<Json<Value>> {
    let mut job=app.db.read_job(&key).await?.ok_or_else(||ApiError(StatusCode::NOT_FOUND,"Editorial job not found".into()))?;
    if job["kind"]!="editorial_review" || (actor.role!="owner" && job["operatorId"]!=actor.id) {
        return Err(ApiError(StatusCode::FORBIDDEN,"Editorial job belongs to another operator".into()));
    }
    // Read current material/recipient authority in one coherent native view.
    // The journal itself remains private; completed independent units are a
    // projection of the original immutable job, never a new paid partition.
    let snapshot=app.db.read_editorial_job_context(&key).await?;
    job=row(&snapshot,"jobs",&key)?.clone();
    if job["kind"]!="editorial_review" || (actor.role!="owner" && job["operatorId"]!=actor.id) {
        return Err(ApiError(StatusCode::FORBIDDEN,"Editorial job belongs to another operator".into()));
    }
    job["editorialProgress"]=progress(&snapshot,&job)?;
    sanitize_bootstrap_job(&mut job);
    Ok(Json(job))
}

fn outcome(refs:&Value,plan:&Value,batches:&[Value])->ApiResult<Value> {
    let mut accepted=Vec::new();let mut reused=Vec::new();let mut held=Vec::new();
    for r in refs.as_array().ok_or_else(||internal("Invalid editorial references"))? {
        let reused_here=plan["reused"].as_array().is_some_and(|rows|rows.contains(r));
        let not_required=plan["notRequired"].as_array().is_some_and(|rows|rows.contains(r));
        if reused_here||not_required {accepted.push(r.clone());if reused_here{reused.push(r.clone());}continue;}
        if let Some(h)=plan["held"].as_array().into_iter().flatten().find(|h|h["reference"]==*r) {
            held.push(json!({"reference":r,"decision":"hold","reason":h["reason"]}));continue;
        }
        let found:Vec<_>=batches.iter().flat_map(|b|b["outcomes"].as_array().into_iter().flatten())
            .filter(|v|v["proposalId"]==r["id"]).collect();
        if found.len()!=1 {return Err(internal("Editorial result coverage is incomplete or ambiguous"));}
        let v=found[0];
        if v["decision"]=="accept" {accepted.push(r.clone());}
        else {
            let mut h=json!({"reference":r,"decision":if v["decision"]=="revise"{"revise"}else{"hold"},"reason":v["reason"]});
            if v["proposedText"].is_string() {h["suggestedText"]=v["proposedText"].clone();}
            if v["decision"]=="revise" && v["receiptSha256"].is_string() {
                let candidates:Vec<_>=plan["batches"].as_array().into_iter().flatten()
                    .flat_map(|b|b["request"]["editorialCandidates"].as_array().into_iter().flatten())
                    .filter(|c|c["proposalId"]==r["id"]&&c["proposalRevision"]==r["revision"]).collect();
                if candidates.len()!=1{return Err(internal("Editorial repair candidate coverage differs"));}
                let c=candidates[0];
                h["repairExpected"]=json!({"proposalId":c["proposalId"],"proposalRevision":c["proposalRevision"],
                    "textSha256":c["textSha256"],"contextDigest":c["contextDigest"],"rulesDigest":c["rulesDigest"],"receiptSha256":v["receiptSha256"]});
            }
            held.push(h);
        }
    }
    Ok(json!({"accepted":accepted,"reused":reused,"held":held}))
}

pub(crate) async fn run(app:App,job:String)->ApiResult<Value> {
    let _timing=performance::Span::job("editorial.total",&job);
    let stored=app.db.read_job(&job).await?.ok_or_else(||internal("Editorial job missing"))?;
    if stored["status"]!="running"{return Err(conflict("Editorial review cancelled"));}
    let plan=stored["editorialPlan"].clone();let refs=stored["editorialReferences"].clone();
    let mut completed=Vec::new();
    for batch in plan["batches"].as_array().ok_or_else(||internal("Editorial plan missing"))? {
        if let Some(result)=settled_result(&stored,batch)? {completed.push(result);continue;}
        // New captured Sol work has independent resource capacity. Old paid
        // batches without a selector retain their original preparation lane.
        // Neither wait holds the database writer or changes send authority.
        let (dispatch,model)={
            let independent=editorial_review::independent_lane(&batch["request"]).map_err(bad)?;
            let lane=if independent {&app.editorial_gate}else{&app.assistant_gate};
            let _lane=lane.lock().await;
            let dispatch=app.change_admission(storage::AdmissionScope::EditorialJob(&job),|d|capture_dispatch(d,&job,batch)).await?;
            let model=if dispatch["batch"].is_null(){None}else{Some(app.bridge("assistant",dispatch["batch"]["request"].clone()).await)};
            (dispatch,model)
        };
        let admitted=app.change_admission(storage::AdmissionScope::EditorialJob(&job),|d| {
            if row(d,"jobs",&job)?["status"]!="running" {return Err(conflict("Editorial review cancelled"));}
            // The reducer validates the entire batch before any receipt writes.
            // A malformed or unavailable model result cannot authorize a reply,
            // but it also must not discard independently completed batches.
            let mut result=match &model {
                Some(Ok(result))=>editorial_review::admit(d,&dispatch["batch"],result,&now())
                    .unwrap_or_else(|_|held_batch(&dispatch["batch"],"Editorial model evidence could not be validated")),
                Some(Err(_))=>held_batch(&dispatch["batch"],"Editorial model review unavailable"),
                None=>json!({"outcomes":[]}),
            };
            result["outcomes"].as_array_mut().unwrap().extend(dispatch["held"].as_array().unwrap().iter().cloned());
            let entry=row_mut(d,"jobs",&job)?["editorialBatches"].as_array_mut().unwrap().iter_mut()
                .find(|entry|entry["capture"]["dispatchDigest"]==dispatch["dispatchDigest"]).ok_or_else(||internal("Editorial dispatch capture missing"))?;
            entry["state"]=json!("settled");
            entry["result"]=result.clone();
            entry["resultDigest"]=json!(editorial_review::hash_text(&result.to_string()));
            Ok(result)
        }).await?;
        completed.push(admitted);
    }
    let summary=outcome(&refs,&plan,&completed)?;
    app.change_job(&job,|d| {
        let current=row_mut(d,"jobs",&job)?;
        if current["status"]!="running"{return Err(conflict("Editorial review cancelled"));}
        current["editorialOutcome"]=summary.clone();Ok(())
    }).await?;
    Ok(summary)
}

pub(crate) const PROGRESS_CONTRACT:&str="communityhero-editorial-progress-v1";

/// Exact current semantic/material readiness, reusable by CLI/continuous
/// projections regardless of which local workflow scheduled the review.
/// This grants no approval, operation, dispatch or retry authority.
pub(crate) fn current_ready(d:&Value,refs:&Value)->ApiResult<Value>{
    let refs=refs.as_array().ok_or_else(||bad("Exact editorial references required"))?;
    if refs.len()>100{return Err(bad("Choose at most 100 editorial references"));}
    let context=prepare_bundle::EvidenceContext::new(d);
    let mut ready=Vec::new();let mut held=Vec::new();let mut seen=std::collections::BTreeSet::new();
    for reference in refs{
        let checked=(||->ApiResult<Value>{
            if reference.as_object().is_none_or(|o|o.len()!=2||!o.contains_key("id")||!o.contains_key("revision"))
                ||reference["revision"].as_u64().is_none_or(|v|v==0)||!seen.insert(required(reference,"id")?.to_owned()){
                return Err(bad("Invalid or duplicate exact editorial reference"));
            }
            let p=row(d,"proposals",required(reference,"id")?)?;check_revision(p,&reference["revision"])?;
            if p["status"]!="draft"{return Err(conflict("Editorial ready proposal is not an unapproved draft"));}
            proposal_current_with_context(p,&context)?;
            if p["kind"]=="reply_and_close"{
                editorial_review::require_current(&context,p).map_err(conflict)?;
                if p["editorialReview"]["source"]["kind"]!="dedicated_model_review"{
                    return Err(conflict("Ready reply requires its current dedicated editorial receipt"));
                }
                preparation_materials::require_proposal(&context,p).map_err(conflict)?;
            }
            Ok(json!({"id":p["id"],"revision":p["revision"],"itemId":p["itemId"],
                "textSha256":editorial_review::hash_text(p["text"].as_str().unwrap_or("")),
                "receiptSha256":p["editorialReview"]["receiptSha256"]}))
        })();
        match checked{Ok(value)=>ready.push(value),Err(error)=>held.push(json!({"reference":reference,"reason":error.1}))}
    }
    Ok(json!({"readyForOwnerApproval":ready,"held":held,"approvalRequired":true,"dispatchAuthorized":false,"retryAllowed":false}))
}

/// Only exact individually settled batches contribute refs. Pending/captured
/// work keeps its original job, membership, admission and paid budget intact.
pub(crate) fn progress(d:&Value,job:&Value)->ApiResult<Value>{
    if job["kind"]!="editorial_review"{return Err(bad("Editorial progress requires an editorial job"));}
    let refs=job["editorialReferences"].as_array().ok_or_else(||conflict("Editorial references missing"))?;
    let plan=&job["editorialPlan"];
    let batches=plan["batches"].as_array().ok_or_else(||conflict("Editorial plan missing"))?;
    let mut completed=Vec::new();let mut settled_refs=Vec::new();let mut units=Vec::new();
    for reference in refs{
        if list(plan,"reused").contains(reference)||list(plan,"notRequired").contains(reference)
            ||list(plan,"held").iter().any(|h|h["reference"]==*reference){settled_refs.push(reference.clone());}
    }
    for batch in batches{
        let unit_refs=refs.iter().filter(|r|list(&batch["request"],"editorialCandidates").iter()
            .any(|c|c["proposalId"]==r["id"]&&c["proposalRevision"]==r["revision"])).cloned().collect::<Vec<_>>();
        let captured=list(job,"editorialBatches").iter().any(|e|e["batchId"]==batch["id"]);
        let settled=list(job,"editorialBatches").iter().any(|e|e["batchId"]==batch["id"]&&e["state"]=="settled");
        if settled{
            let result=settled_result(job,batch)?.ok_or_else(||conflict("Editorial settled unit missing"))?;
            completed.push(result);settled_refs.extend(unit_refs.iter().cloned());
        }
        units.push(json!({"unitId":batch["id"],"references":unit_refs,
            "state":if settled{"settled"}else if captured{"captured"}else{"pending"}}));
    }
    if settled_refs.iter().any(|r|settled_refs.iter().filter(|other|*other==r).count()!=1){
        return Err(conflict("Editorial progress coverage is ambiguous"));
    }
    let summary=outcome(&json!(settled_refs),plan,&completed)?;
    let current=current_ready(d,&summary["accepted"])?;
    let ready=list(&current,"readyForOwnerApproval");
    let accepted=list(&summary,"accepted").iter().filter(|r|ready.iter().any(|p|p["id"]==r["id"]&&p["revision"]==r["revision"])).cloned().collect::<Vec<_>>();
    let reused=list(&summary,"reused").iter().filter(|r|accepted.contains(r)).cloned().collect::<Vec<_>>();
    let mut held=list(&summary,"held").to_vec();
    held.extend(list(&current,"held").iter().map(|h|json!({"reference":h["reference"],"decision":"hold","reason":h["reason"],"stale":true})));
    let pending=refs.iter().filter(|r|!settled_refs.contains(r)).cloned().collect::<Vec<_>>();
    Ok(json!({"version":1,"contract":PROGRESS_CONTRACT,"accepted":accepted,"reused":reused,"held":held,"pending":pending,
        "readyForOwnerApproval":current["readyForOwnerApproval"],"units":units,"completedUnits":completed.len(),"totalUnits":batches.len(),
        "complete":pending.is_empty(),"approvalRequired":true,"dispatchAuthorized":false,"retryAllowed":false}))
}

pub(crate) fn settled_result(job:&Value,batch:&Value)->ApiResult<Option<Value>>{
    let entries:Vec<_>=job["editorialBatches"].as_array().into_iter().flatten().filter(|entry|entry["batchId"]==batch["id"]).collect();
    if entries.is_empty(){return Ok(None);}
    if entries.len()!=1{return Err(conflict("Editorial checkpoint is ambiguous"));}
    let entry=entries[0];
    if entry["state"]!="settled"{return Err(conflict("Editorial dispatch already captured; recover the original outcome without another model call"));}
    if entry["digest"]!=batch["digest"]||entry["resultDigest"]!=editorial_review::hash_text(&entry["result"].to_string()){
        return Err(conflict("Editorial checkpoint binding changed"));
    }
    editorial_review::validate_dispatch_capture(batch,&entry["capture"]).map_err(conflict)?;
    let candidates=batch["request"]["editorialCandidates"].as_array().ok_or_else(||internal("Editorial candidates missing"))?;
    let outcomes=entry["result"]["outcomes"].as_array().ok_or_else(||conflict("Editorial checkpoint outcomes missing"))?;
    if outcomes.len()!=candidates.len()||candidates.iter().any(|candidate|outcomes.iter().filter(|o|o["proposalId"]==candidate["proposalId"]).count()!=1){
        return Err(conflict("Editorial checkpoint coverage changed"));
    }
    Ok(Some(entry["result"].clone()))
}

pub(crate) fn capture_dispatch(d:&mut Value,job:&str,batch:&Value)->ApiResult<Value>{
    crate::conductor_authority::fence_job_capture(d,job,"review")?;
    let current=row(d,"jobs",job)?;
    if current["status"]!="running"{return Err(conflict("Editorial review cancelled"));}
    if !current["editorialPlan"]["batches"].as_array().is_some_and(|rows|rows.contains(batch)){
        return Err(conflict("Editorial captured batch changed"));
    }
    if current["editorialBatches"].as_array().into_iter().flatten().any(|entry|entry["batchId"]==batch["id"]){
        return Err(conflict("Editorial dispatch already captured; recover the original outcome without another model call"));
    }
    let dispatch=editorial_review::before_call(d,batch).map_err(bad)?;
    let current=row_mut(d,"jobs",job)?;
    // This established private journal is removed by every public job projection.
    current["editorialBatches"].as_array_mut().ok_or_else(||internal("Editorial dispatch journal invalid"))?
        .push(json!({"batchId":batch["id"],"digest":batch["digest"],"capture":dispatch,"state":"captured"}));
    Ok(dispatch)
}

fn held_batch(batch:&Value,reason:&str)->Value {
    json!({"outcomes":batch["request"]["editorialCandidates"].as_array().into_iter().flatten()
        .map(|c|json!({"proposalId":c["proposalId"],"decision":"hold","reason":reason})).collect::<Vec<_>>()})
}

#[cfg(test)]
mod tests {
    use super::*;
    fn complete_text_fixture(d:&mut Value){
        let post_key=d["items"][0]["postKey"].clone();
        d["posts"]=json!([{"id":"isolated-editorial-post","postKey":post_key,"text":"Complete synthetic publication text","attachments":[]}]);
        for item in d["items"].as_array_mut().unwrap(){item["postId"]=json!("isolated-editorial-post");}
    }
    #[test]
    fn settled_independent_unit_is_current_ready_while_original_job_and_other_unit_remain_pending(){
        let(mut d,_)=crate::operator_editorial::tests::fixture();
        // Each source occurs in the post and its material contract. A single
        // unit must fit500k while the combined independent sources exceed it.
        d["proposals"]=json!([]);d["posts"][0]["text"]=json!("a".repeat(180_000));
        let first_revision=row(&d,"items","i0").unwrap()["revision"].clone();
        let first=create_proposal(&mut d,&json!({"itemId":"i0","expectedRevision":first_revision,"kind":"reply_and_close","text":"Первый точный синтетический ответ."})).unwrap();
        let mut second_post=d["posts"][0].clone();second_post["id"]=json!("independent-post");second_post["postKey"]=json!("independent-post");second_post["text"]=json!("b".repeat(180_000));list_mut(&mut d,"posts").push(second_post);
        let mut second_branch=d["branches"][0].clone();second_branch["id"]=json!("independent-branch");second_branch["postId"]=json!("independent-post");list_mut(&mut d,"branches").push(second_branch);
        let mut second_item=d["items"][0].clone();second_item["id"]=json!("independent-item");second_item["itemId"]=json!("independent-comment");
        second_item["postId"]=json!("independent-post");second_item["postKey"]=json!("independent-post");second_item["branchId"]=json!("independent-branch");
        second_item["conversationKey"]=json!("independent-thread");second_item["workflow"]=json!("attention");second_item["revision"]=json!(1);
        list_mut(&mut d,"items").push(second_item);
        let second=create_proposal(&mut d,&json!({"itemId":"independent-item","expectedRevision":1,"kind":"reply_and_close","text":"Другой точный синтетический ответ."})).unwrap();
        let second_ref=json!({"id":second["id"],"revision":second["revision"]});let refs=json!([{"id":first["id"],"revision":first["revision"]},second_ref]);
        // One real native schedule creates two bounded independent units because
        // their complete contexts exceed the aggregate batch byte budget. The
        // original immutable plan is never partitioned after paid capture.
        let actor=operator_auth::Actor::local_owner("offline-progress");
        let(_,job)=schedule(&mut d,&actor,&json!({"requestId":"offline-progress","proposals":refs,"fresh":true})).unwrap();let job=job.unwrap();
        assert_eq!(row(&d,"jobs",&job).unwrap()["editorialPlan"]["held"],json!([]));
        assert_eq!(row(&d,"jobs",&job).unwrap()["editorialPlan"]["batches"].as_array().unwrap().len(),2);
        let sizes:Vec<_>=list(&row(&d,"jobs",&job).unwrap()["editorialPlan"],"batches").iter()
            .map(|batch|batch["request"].to_string().len()).collect();
        assert!(sizes.iter().all(|bytes|*bytes<=500_000),"each independently admissible context fits");
        assert!(sizes.iter().sum::<usize>()>500_000,"the two independent units require a split");
        let first_batch=row(&d,"jobs",&job).unwrap()["editorialPlan"]["batches"][0].clone();let plan=row(&d,"jobs",&job).unwrap()["editorialPlan"].clone();
        let dispatch=capture_dispatch(&mut d,&job,&first_batch).unwrap();assert_eq!(dispatch["batch"],first_batch);
        let mut metadata=editorial_review::fixture_metadata();metadata["model"]=json!(codex_model_policy::MODEL);metadata["modelProfile"]=json!(codex_model_policy::PROFILE);
        metadata["reasoningEffort"]=json!("high");metadata["cliSha256"]=json!(codex_model_policy::CLI_SHA256);
        let mut result=json!({"text":"Offline independent exact review","sources":[],"proposals":[],"runMetadata":metadata,
            "editorial":list(&first_batch["request"],"editorialCandidates").iter().map(|c|json!({"proposalId":c["proposalId"],"proposalRevision":c["proposalRevision"],"itemId":c["itemId"],
                "textSha256":c["textSha256"],"contextDigest":c["contextDigest"],"rulesDigest":c["rulesDigest"],"decision":"accept","reason":"Exact isolated native progress proof","proposedText":null,
                "checks":{"companyRules":"pass","intent":"pass","factualScope":"pass"}})).collect::<Vec<_>>()});
        model_material_receipt::fixture_result(&mut d,&job,&first_batch["request"],&mut result).unwrap();
        let admitted=editorial_review::admit(&mut d,&first_batch,&result,&now()).unwrap();
        let entry=&mut row_mut(&mut d,"jobs",&job).unwrap()["editorialBatches"][0];entry["state"]=json!("settled");entry["result"]=admitted.clone();entry["resultDigest"]=json!(editorial_review::hash_text(&admitted.to_string()));
        let original=row(&d,"jobs",&job).unwrap().clone();let view=progress(&d,&original).unwrap();
        assert_eq!(view["accepted"],json!([refs[0]]));assert_eq!(view["pending"],json!([refs[1]]));assert_eq!(view["completedUnits"],1);assert_eq!(view["totalUnits"],2);assert_eq!(view["complete"],false);
        assert_eq!(view["readyForOwnerApproval"][0]["id"],refs[0]["id"]);assert_eq!(original["status"],"running");assert_eq!(original["editorialPlan"],plan);
        preparation_materials::require_proposal(&prepare_bundle::EvidenceContext::new(&d),row(&d,"proposals",refs[0]["id"].as_str().unwrap()).unwrap()).unwrap();
        assert_eq!(original["editorialBatches"].as_array().unwrap().len(),1);assert!(d["approvals"].as_array().unwrap().is_empty());assert!(d["operations"].as_array().unwrap().is_empty());
        d["posts"][0]["text"]=json!("Changed current source after first unit settled");let stale=progress(&d,&original).unwrap();
        assert_eq!(stale["accepted"],json!([]));assert_eq!(stale["pending"],json!([refs[1]]));assert_eq!(stale["held"][0]["reference"],refs[0]);assert_eq!(stale["held"][0]["stale"],true);
        assert_eq!(row(&d,"jobs",&job).unwrap(),&original,"stale progress does not rewrite or retry original paid work");
    }
    #[test]
    fn fresh_is_durable_idempotent_input_and_legacy_absence_is_preserved(){
        let (mut d,refs)=crate::operator_editorial::tests::fixture();
        let actor=operator_auth::Actor::local_owner("fresh-input-fixture");
        let body=json!({"requestId":"fresh-input","proposals":refs,"fresh":true});
        let (first,job)=schedule(&mut d,&actor,&body).unwrap();let job=job.unwrap();
        assert_eq!(row(&d,"jobs",&job).unwrap()["editorialFresh"],true);
        assert_eq!(row(&d,"jobs",&job).unwrap()["editorialPlan"]["fresh"],true);
        let before=d.clone();let (replay,next)=schedule(&mut d,&actor,&body).unwrap();
        assert!(next.is_none());assert_eq!(replay["jobId"],first["jobId"]);assert_eq!(replay["replayed"],true);assert_eq!(d,before);
        let mut omitted=body;omitted.as_object_mut().unwrap().remove("fresh");
        assert!(schedule(&mut d,&actor,&omitted).is_err());assert_eq!(d,before);
        let (mut legacy,refs)=crate::operator_editorial::tests::fixture();
        let (_,job)=schedule(&mut legacy,&actor,&json!({"requestId":"legacy-input","proposals":refs})).unwrap();
        let j=row(&legacy,"jobs",&job.unwrap()).unwrap();
        assert!(j.get("editorialFresh").is_none());assert!(j["editorialPlan"].get("fresh").is_none());
    }
    async fn model_fixture()->(App,tempfile::TempDir,String,Value){
        model_fixture_count(1).await
    }
    async fn model_fixture_count(count:usize)->(App,tempfile::TempDir,String,Value){
        let (mut app,temp)=crate::tests::test_app().await;
        app.node=std::env::var_os("COMMUNITYHERO_TEST_NODE").map(PathBuf::from).unwrap_or_else(||PathBuf::from("node"));
        app.bridge=temp.path().join("editorial-only.mjs");
        let material_module=PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../adapters/assistant-materials.mjs");
        let material_url=format!("file:///{}",material_module.to_string_lossy().replace('\\',"/"));
        std::fs::write(&app.bridge,r#"import fs from 'node:fs/promises';import {materialInvocation,validateMandatoryMaterials} from __MATERIAL_MODULE__;let raw='';for await(const part of process.stdin)raw+=part;
const req=JSON.parse(raw);if(req.operation!=='assistant'||req.purpose!=='editorial_review')throw Error('unexpected operation');
await fs.appendFile(new URL('calls',import.meta.url),req.editorialModelProfile??'legacy');
await fs.writeFile(new URL('request.json',import.meta.url),JSON.stringify(req));
await new Promise(resolve=>setTimeout(resolve,150));
const editorial=req.editorialCandidates.map(c=>({proposalId:c.proposalId,proposalRevision:c.proposalRevision,itemId:c.itemId,textSha256:c.textSha256,contextDigest:c.contextDigest,rulesDigest:c.rulesDigest,decision:'accept',reason:'Synthetic exact decision',proposedText:null,checks:{companyRules:'pass',intent:'pass',factualScope:'pass'}}));
const runMetadata={schemaVersion:1,model:'gpt-6.1-sol',modelProfile:'sol61_v1',reasoningEffort:'high',promptVersion:'communityhero-editorial-v1',instructionSha256:'a'.repeat(64),inputSha256:'b'.repeat(64),cliSha256:'86e8ef1013f98df51fdeea446597f7e3ca32e454d1d4d8c0402a68b03c311d70',elapsedMs:1,completedAt:'2026-09-29T00:00:00Z',imageEvidence:[]};
validateMandatoryMaterials(req);const material=materialInvocation({payload:req,input:JSON.stringify(req)},{manifest:[],frameManifest:[]},{instructions:'Synthetic offline fixture instruction',schema:{},cliSha256:runMetadata.cliSha256,stdin:JSON.stringify(req)});runMetadata.inputSha256=material.actualTextInputSha256;runMetadata.instructionSha256=material.instructionSha256;runMetadata.materialInvocation=material;
process.stdout.write(JSON.stringify({ok:true,result:{text:'Synthetic review',sources:[],proposals:[],editorial,runMetadata}}));"#.replace("__MATERIAL_MODULE__",&json!(material_url).to_string())).unwrap();
        let actor=operator_auth::Actor::local_owner("r11-offline");
        let (job,refs)=app.change(|d|{
            complete_text_fixture(d);
            if count==2 {let mut item=row(d,"items","item-1")?.clone();item["id"]=json!("item-2");item["itemId"]=json!("external-2");d["items"].as_array_mut().unwrap().push(item);}
            let mut refs=Vec::new();
            for n in 1..=count {
                let p=create_proposal(d,&json!({"itemId":format!("item-{n}"),"kind":"reply_and_close","text":"Exact final response","expectedRevision":1}))?;
                refs.push(json!({"id":p["id"],"revision":p["revision"]}));
            }
            let refs=json!(refs);
            let (_,job)=schedule(d,&actor,&json!({"requestId":"r11-offline-review","proposals":refs}))?;
            Ok((job.unwrap(),refs))
        }).await.unwrap();
        (app,temp,job,refs)
    }
    async fn run_fixture(app:App,job:String)->ApiResult<Value>{
        crate::runtime_lifecycle_app::with_job(job.clone(),run(app,job)).await
    }
    #[tokio::test]
    async fn queued_revision_or_source_change_holds_without_model_call(){
        for change in ["revision","source"] {
            let (app,temp,job,refs)=model_fixture().await;
            let lane=app.editorial_gate.lock().await;let worker=app.clone();let key=job.clone();
            let waiting=tokio::spawn(async move{run_fixture(worker,key).await});
            tokio::time::sleep(Duration::from_millis(50)).await;
            app.change(|d|{
                if change=="revision"{row_mut(d,"proposals",refs[0]["id"].as_str().unwrap())?["revision"]=json!(2);}
                else{row_mut(d,"items","item-1")?["text"]=json!("Changed source while waiting");}
                Ok(())
            }).await.unwrap();drop(lane);
            let result=waiting.await.unwrap().unwrap();assert!(result["accepted"].as_array().unwrap().is_empty());
            assert_eq!(result["held"][0]["reference"],refs[0]);assert!(!temp.path().join("calls").exists(),"{change}");
            let stored=app.db.read_job(&job).await.unwrap().unwrap();
            assert!(stored["editorialBatches"][0]["capture"]["batch"].is_null());
            assert_eq!(stored["editorialBatches"][0]["state"],"settled");app.db.close().await;
        }
    }
    #[tokio::test]
    async fn queued_mixed_batch_sends_only_current_candidate_and_preserves_original_capture(){
        let (app,temp,job,refs)=model_fixture_count(2).await;
        let original=app.db.read_job(&job).await.unwrap().unwrap()["editorialPlan"].clone();
        let lane=app.editorial_gate.lock().await;let worker=app.clone();let key=job.clone();
        let waiting=tokio::spawn(async move{run_fixture(worker,key).await});tokio::time::sleep(Duration::from_millis(50)).await;
        app.change(|d|{row_mut(d,"proposals",refs[0]["id"].as_str().unwrap())?["revision"]=json!(2);Ok(())}).await.unwrap();drop(lane);
        let result=waiting.await.unwrap().unwrap();assert_eq!(result["accepted"],json!([refs[1]]));assert_eq!(result["held"][0]["reference"],refs[0]);
        let sent:Value=serde_json::from_slice(&std::fs::read(temp.path().join("request.json")).unwrap()).unwrap();
        assert_eq!(sent["editorialCandidates"].as_array().unwrap().len(),1);assert_eq!(sent["editorialCandidates"][0]["proposalId"],refs[1]["id"]);
        let stored=app.db.read_job(&job).await.unwrap().unwrap();assert_eq!(stored["editorialPlan"],original);
        let capture=&stored["editorialBatches"][0]["capture"];
        assert_eq!(capture["parentDigest"],original["batches"][0]["digest"]);assert_ne!(capture["batch"]["digest"],capture["parentDigest"]);
        assert_eq!(stored["editorialBatches"][0]["result"]["outcomes"].as_array().unwrap().len(),2);
        assert_eq!(run_fixture(app.clone(),job.clone()).await.unwrap(),result);
        assert_eq!(std::fs::read_to_string(temp.path().join("calls")).unwrap(),"sol61_high_v2");
        app.change_job(&job,|d|{let j=row_mut(d,"jobs",&job)?;j["status"]=json!("cancelled");j.as_object_mut().unwrap().remove("editorialOutcome");Ok(())}).await.unwrap();
        assert!(run_fixture(app.clone(),job.clone()).await.unwrap_err().1.contains("cancelled"));
        let cancelled=app.db.read_job(&job).await.unwrap().unwrap();assert_eq!(cancelled["status"],"cancelled");assert!(cancelled.get("editorialOutcome").is_none());
        assert_eq!(std::fs::read_to_string(temp.path().join("calls")).unwrap(),"sol61_high_v2");app.db.close().await;
    }
    #[tokio::test]
    async fn interrupted_after_settlement_resumes_only_remaining_batch_without_duplicate_call(){
        let (app,temp,job,refs)=model_fixture_count(2).await;
        app.change_job(&job,|d|{
            let planned=&mut row_mut(d,"jobs",&job)?["editorialPlan"];
            let original=planned["batches"][0].clone();let mut batches=Vec::new();
            for n in 0..2 {
                let mut b=original.clone();b["id"]=json!(format!("editorial-batch-{n}"));
                b["request"]["editorialCandidates"]=json!([original["request"]["editorialCandidates"][n]]);
                b["request"]["editorialResearchPins"]=json!([original["request"]["editorialResearchPins"][n]]);
                b["digest"]=json!(editorial_review::hash_text(&b["request"].to_string()));batches.push(b);
            }
            planned["batches"]=json!(batches);Ok(())
        }).await.unwrap();
        let worker=app.clone();let key=job.clone();let running=tokio::spawn(async move{run_fixture(worker,key).await});
        let deadline=tokio::time::Instant::now()+Duration::from_secs(5);
        while !temp.path().join("calls").exists(){assert!(tokio::time::Instant::now()<deadline);tokio::time::sleep(Duration::from_millis(5)).await;}
        let lane=app.editorial_gate.lock().await;
        loop {
            let stored=app.db.read_job(&job).await.unwrap().unwrap();
            if stored["editorialBatches"][0]["state"]=="settled"{break;}
            assert!(tokio::time::Instant::now()<deadline);tokio::time::sleep(Duration::from_millis(5)).await;
        }
        running.abort();assert!(running.await.unwrap_err().is_cancelled());drop(lane);
        let after_first=app.db.read_job(&job).await.unwrap().unwrap();assert_eq!(after_first["editorialBatches"].as_array().unwrap().len(),1);
        let first_receipt=app.read().await.unwrap()["proposals"][0]["editorialReview"].clone();
        let result=run_fixture(app.clone(),job.clone()).await.unwrap();assert_eq!(result["accepted"],refs);assert!(result["held"].as_array().unwrap().is_empty());
        assert_eq!(std::fs::read_to_string(temp.path().join("calls")).unwrap(),"sol61_high_v2sol61_high_v2");
        let stored=app.db.read_job(&job).await.unwrap().unwrap();assert_eq!(stored["editorialBatches"].as_array().unwrap().len(),2);
        assert_eq!(stored["editorialBatches"][0],after_first["editorialBatches"][0]);
        assert_eq!(app.read().await.unwrap()["proposals"][0]["editorialReview"],first_receipt);app.db.close().await;
    }
    #[tokio::test]
    async fn interrupted_durable_dispatch_capture_never_rederives_or_repeats_model_call(){
        let (app,temp,job,refs)=model_fixture().await;
        let batch=app.db.read_job(&job).await.unwrap().unwrap()["editorialPlan"]["batches"][0].clone();
        let captured=app.change(|d|capture_dispatch(d,&job,&batch)).await.unwrap();
        assert!(captured["batch"]["request"]["editorialCandidates"].is_array());
        let Json(public)=get(State(app.clone()),Extension(operator_auth::Actor::local_owner("r11-offline")),Path(job.clone())).await.unwrap();
        assert!(public.get("editorialBatches").is_none());assert!(public.get("editorialPlan").is_none());
        let Json(headless)=crate::engine_api::job(State(app.clone()),Path(job.clone())).await.unwrap();
        assert!(headless.get("editorialBatches").is_none());assert!(headless.get("editorialPlan").is_none());
        let bootstrap=app.read_bootstrap().await.unwrap();
        let visible=list(&bootstrap,"jobs").iter().find(|entry|entry["id"]==job).unwrap();
        assert!(visible.get("editorialBatches").is_none());assert!(visible.get("editorialPlan").is_none());
        app.change(|d|{row_mut(d,"proposals",refs[0]["id"].as_str().unwrap())?["revision"]=json!(2);Ok(())}).await.unwrap();
        assert!(run_fixture(app.clone(),job.clone()).await.unwrap_err().1.contains("already captured"));
        assert!(!temp.path().join("calls").exists());
        let stored=app.db.read_job(&job).await.unwrap().unwrap();assert_eq!(stored["editorialBatches"][0]["capture"],captured);
        assert_eq!(stored["editorialBatches"].as_array().unwrap().len(),1);assert_eq!(stored["editorialBatches"][0]["state"],"captured");
        app.db.close().await;
    }
    #[tokio::test]
    async fn sol_editorial_completes_while_generation_and_sender_gates_are_occupied(){
        let (app,temp,job,refs)=model_fixture().await;
        let _generation=app.assistant_gate.lock().await;let _sender=app.execution_gate.lock().await;
        let outcome=tokio::time::timeout(Duration::from_secs(5),run_fixture(app.clone(),job.clone())).await
            .expect("Sol editorial must not wait for independent generation/send").unwrap();
        assert_eq!(outcome["accepted"],refs);assert!(temp.path().join("calls").exists());
        let d=app.read().await.unwrap();assert!(list(&d,"operations").is_empty());assert!(list(&d,"approvals").is_empty());
        assert_eq!(d["proposals"][0]["text"],"Exact final response");app.db.close().await;
    }
    #[tokio::test]
    async fn sol_lane_is_bounded_and_cancelled_waiter_never_starts_bridge(){
        let (app,temp,job,_)=model_fixture().await;
        let lane=app.editorial_gate.lock().await;let worker=app.clone();let key=job.clone();
        let waiting=tokio::spawn(async move{run(worker,key).await});
        tokio::time::sleep(Duration::from_millis(80)).await;assert!(!temp.path().join("calls").exists());
        app.change_job(&job,|d|{row_mut(d,"jobs",&job)?["status"]=json!("cancelled");Ok(())}).await.unwrap();
        drop(lane);assert!(waiting.await.unwrap().unwrap_err().1.contains("cancelled"));
        assert!(!temp.path().join("calls").exists());app.db.close().await;
    }
    #[tokio::test]
    async fn legacy_editorial_keeps_generation_lane_and_captured_plan_on_cancel(){
        let (app,temp,job,_)=model_fixture().await;
        app.change_job(&job,|d|{
            for batch in row_mut(d,"jobs",&job)?["editorialPlan"]["batches"].as_array_mut().unwrap(){
                batch["request"].as_object_mut().unwrap().remove("editorialModelProfile");
                batch["digest"]=json!(editorial_review::hash_text(&batch["request"].to_string()));
            }Ok(())
        }).await.unwrap();
        let before=app.db.read_job(&job).await.unwrap().unwrap()["editorialPlan"].clone();
        let lane=app.assistant_gate.lock().await;let worker=app.clone();let key=job.clone();
        let waiting=tokio::spawn(async move{run(worker,key).await});
        tokio::time::sleep(Duration::from_millis(80)).await;assert!(!temp.path().join("calls").exists());
        waiting.abort();let _=waiting.await;drop(lane);
        assert_eq!(app.db.read_job(&job).await.unwrap().unwrap()["editorialPlan"],before);
        assert!(!temp.path().join("calls").exists());app.db.close().await;
    }
    // Real routes/middleware over isolated loopback TCP. The fixture's Node and
    // bridge paths do not exist, so completion also proves the zero-model path.
    async fn http(port:u16,method:&str,path:&str,cookie:&str,csrf:&str,body:Option<Value>)->(u16,Value) {
        use tokio::io::{AsyncReadExt,AsyncWriteExt};
        let body=body.map(|value|value.to_string()).unwrap_or_default();
        let request=format!("{method} {path} HTTP/1.1\r\nHost: editorial.example.test\r\nOrigin: https://editorial.example.test\r\nCookie: {cookie}\r\nX-CSRF-Token: {csrf}\r\nConnection: close\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",body.len());
        let mut stream=tokio::net::TcpStream::connect((std::net::Ipv4Addr::LOCALHOST,port)).await.unwrap();
        stream.write_all(request.as_bytes()).await.unwrap();let mut bytes=vec![];
        tokio::time::timeout(Duration::from_secs(5),stream.read_to_end(&mut bytes)).await.unwrap().unwrap();
        let raw=String::from_utf8(bytes).unwrap();let (head,body)=raw.split_once("\r\n\r\n").unwrap();
        let status=head.lines().next().unwrap().split_whitespace().nth(1).unwrap().parse().unwrap();
        (status,serde_json::from_str(body).unwrap())
    }
    struct HttpServer(tokio::task::JoinHandle<()>);
    impl Drop for HttpServer {fn drop(&mut self){self.0.abort();}}

    #[tokio::test]
    async fn http_editorial_admission_replay_and_private_readback_complete_without_model_or_actions() {
        use sha2::{Digest,Sha256};
        let (mut app,temp)=crate::tests::test_app().await;
        let access=temp.path().join("editorial-access.json");
        let alice_token=format!("alice-{}","offline-test-key".repeat(5));let bob_token=format!("bob-{}","offline-test-key".repeat(5));
        let operators=[("alice",&alice_token),("bob",&bob_token)].map(|(who,token)|json!({"id":who,"name":who,"tokenHash":format!("{:x}",Sha256::digest(token.as_bytes()))}));
        std::fs::write(&access,json!({"operators":operators}).to_string()).unwrap();
        let auth=operator_auth::Auth::open(temp.path(),access).await.unwrap();
        let alice=auth.login(&alice_token).await.unwrap();let bob=auth.login(&bob_token).await.unwrap();
        let alice_cookie=alice.set_cookie.split(';').next().unwrap().to_owned();let bob_cookie=bob.set_cookie.split(';').next().unwrap().to_owned();
        app.auth=Some(auth);app.public_origin=Some("https://editorial.example.test".into());
        let listener=tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();app.port=listener.local_addr().unwrap().port();
        let port=app.port;
        let refs=app.change(|d| {
            complete_text_fixture(d);
            let mut second=d["items"][0].clone();second["id"]=json!("item-2");second["itemId"]=json!("comment-2");
            list_mut(d,"items").push(second);
            let close=create_proposal(d,&json!({"itemId":"item-1","kind":"close","expectedRevision":1}))?;
            let reply=create_proposal(d,&json!({"itemId":"item-2","kind":"reply_and_close","text":"Exact offline final reply","expectedRevision":1}))?;
            editorial_review::fixture_accept(d,reply["id"].as_str().unwrap()).map_err(internal)?;
            Ok(json!([{"id":close["id"],"revision":close["revision"]},{"id":reply["id"],"revision":reply["revision"]}]))
        }).await.unwrap();
        assert!(!app.node.exists());assert!(!app.bridge.exists());
        let router=routes(app.clone(),temp.path().join("empty-web"));
        let _server=HttpServer(tokio::spawn(async move {axum::serve(listener,router).await.unwrap();}));
        let body=json!({"requestId":"http-editorial-lost-ack","proposals":refs});
        let (status,first)=http(port,"POST","/api/proposals/editorial-review",&alice_cookie,&alice.actor.csrf_token,Some(body.clone())).await;
        assert_eq!(status,200,"{first}");assert_eq!(first["requestId"],body["requestId"]);assert_eq!(first["replayed"],false);
        let key=first["jobId"].as_str().unwrap();
        let stored=app.db.read_job(key).await.unwrap().unwrap();assert_eq!(stored["editorialReferences"],refs);
        assert_eq!(stored["operatorId"],"alice");assert_eq!(stored["purpose"],"editorial_review");assert_eq!(stored["refId"],body["requestId"]);
        let (status,replayed)=http(port,"POST","/api/proposals/editorial-review",&alice_cookie,&alice.actor.csrf_token,Some(body.clone())).await;
        assert_eq!(status,200,"{replayed}");assert_eq!(replayed["jobId"],first["jobId"]);assert_eq!(replayed["replayed"],true);
        let mut changed=body.clone();changed["proposals"][0]["revision"]=json!(999);
        assert_eq!(http(port,"POST","/api/proposals/editorial-review",&alice_cookie,&alice.actor.csrf_token,Some(changed)).await.0,409);
        assert_eq!(http(port,"POST","/api/proposals/editorial-review",&bob_cookie,&bob.actor.csrf_token,Some(body.clone())).await.0,409);
        let lookup_path=format!("/api/local-admissions/editorial/{}",body["requestId"].as_str().unwrap());
        let (status,lookup)=http(port,"GET",&lookup_path,&alice_cookie,"",None).await;
        assert_eq!(status,200,"{lookup}");assert_eq!(lookup["status"],"committed");assert_eq!(lookup["result"]["jobId"],first["jobId"]);
        let path=format!("/api/jobs/{key}");let deadline=tokio::time::Instant::now()+Duration::from_secs(5);
        let completed=loop {
            let (status,job)=http(port,"GET",&path,&alice_cookie,"",None).await;assert_eq!(status,200,"{job}");
            if job["status"]=="completed"{break job;}
            assert_eq!(job["status"],"running","{job}");assert!(tokio::time::Instant::now()<deadline,"Editorial zero-model job did not complete");
            tokio::time::sleep(Duration::from_millis(10)).await;
        };
        assert_eq!(completed["id"],first["jobId"]);assert_eq!(completed["kind"],"editorial_review");
        assert_eq!(completed["result"]["accepted"],refs);assert_eq!(completed["result"]["reused"],json!([refs[1]]));
        assert_eq!(completed["result"]["held"],json!([]));
        // Public sanitization preserves the absence of unrelated visual data.
        assert!(completed["result"].get("visualProgress").is_none());
        assert_eq!(completed["editorialOutcome"],completed["result"]);
        assert!(completed.get("editorialPlan").is_none());assert!(completed.get("editorialBatches").is_none());
        assert_eq!(http(port,"GET",&path,&bob_cookie,"",None).await.0,403);
        assert_eq!(http(port,"GET",&lookup_path,&bob_cookie,"",None).await.0,409);
        let d=app.read().await.unwrap();assert_eq!(list(&d,"jobs").iter().filter(|j|j["refId"]==body["requestId"]).count(),1);
        assert!(list(&d,"approvals").is_empty());assert!(list(&d,"operations").is_empty());
        assert_eq!(d["proposals"][1]["text"],"Exact offline final reply");assert_eq!(d["proposals"][1]["status"],"draft");
        app.db.close().await;
    }
    #[tokio::test]
    async fn replay_preserves_original_job_without_spawning_or_second_model_call() {
        let (app,_temp)=crate::tests::test_app().await;
        let actor=operator_auth::Actor::local_owner("synthetic");
        let p=app.change(|d|create_proposal(d,&json!({"itemId":"item-1","kind":"close","expectedRevision":1}))).await.unwrap();
        let body=json!({"requestId":"editorial-ack-loss","proposals":[{"id":p["id"],"revision":p["revision"]}]});
        let first=app.change(|d|schedule(d,&actor,&body)).await.unwrap();
        assert!(first.1.is_some());
        let again=app.change(|d|schedule(d,&actor,&body)).await.unwrap();
        assert!(again.1.is_none());assert_eq!(again.0["jobId"],first.0["jobId"]);
        let mut duplicate=body.clone();duplicate["requestId"]=json!("different-key-same-proposal");
        assert!(app.change(|d|schedule(d,&actor,&duplicate)).await.is_err());
        let lookup=local_admission::lookup(State(app.clone()),Extension(actor.clone()),Path(("editorial".into(),"editorial-ack-loss".into()))).await.unwrap().0;
        assert_eq!(lookup["status"],"committed");
        let mut changed=body.clone();changed["proposals"][0]["revision"]=json!(999);
        assert!(app.change(|d|schedule(d,&actor,&changed)).await.is_err());
        let mut other=actor;other.id="other".into();
        assert!(app.change(|d|schedule(d,&other,&body)).await.is_err());
        let d=app.read().await.unwrap();assert_eq!(list(&d,"jobs").len(),1);assert!(list(&d,"approvals").is_empty());assert!(list(&d,"operations").is_empty());
        app.db.close().await;
    }
    #[tokio::test]
    async fn full_editorial_queue_admits_zero_model_scope_with_durable_replay() {
        let (app,_temp)=crate::tests::test_app().await;
        let actor=operator_auth::Actor::local_owner("synthetic");
        let (refs,unreviewed)=app.change(|d| {
            complete_text_fixture(d);
            for key in ["item-2","item-3"] {
                let mut item=d["items"][0].clone();item["id"]=json!(key);item["itemId"]=json!(key);
                list_mut(d,"items").push(item);
            }
            let close=create_proposal(d,&json!({"itemId":"item-1","kind":"close","expectedRevision":1}))?;
            let reused=create_proposal(d,&json!({"itemId":"item-2","kind":"reply_and_close","text":"Exact previously reviewed reply","expectedRevision":1}))?;
            let unreviewed=create_proposal(d,&json!({"itemId":"item-3","kind":"reply_and_close","text":"Reply still requiring review","expectedRevision":1}))?;
            editorial_review::fixture_accept(d,reused["id"].as_str().unwrap()).map_err(internal)?;
            for index in 0..10 {new_job(d,"editorial_review",&format!("occupied-{index}"))?;}
            Ok((json!([{"id":close["id"],"revision":close["revision"]},{"id":reused["id"],"revision":reused["revision"]}]),
                json!([{"id":unreviewed["id"],"revision":unreviewed["revision"]}])))
        }).await.unwrap();
        let needs_model=json!({"requestId":"queue-full-needs-model","proposals":unreviewed});
        let error=app.change(|d|schedule(d,&actor,&needs_model)).await.unwrap_err();
        assert_eq!(error.0,StatusCode::CONFLICT);assert!(error.1.contains("queue is full"));
        let jobs_before=app.read().await.unwrap()["jobs"].as_array().unwrap().len();
        let body=json!({"requestId":"queue-full-zero-model","proposals":refs});
        let first=app.change(|d|schedule(d,&actor,&body)).await.unwrap();
        let job=first.1.unwrap();
        let stored=app.db.read_job(&job).await.unwrap().unwrap();
        assert_eq!(stored["editorialPlan"]["batches"],json!([]));
        assert_eq!(stored["editorialPlan"]["notRequired"],json!([refs[0]]));
        assert_eq!(stored["editorialPlan"]["reused"],json!([refs[1]]));
        let replay=app.change(|d|schedule(d,&actor,&body)).await.unwrap();
        assert!(replay.1.is_none());assert_eq!(replay.0["jobId"],job);assert_eq!(replay.0["replayed"],true);
        let duplicate=json!({"requestId":"queue-full-duplicate","proposals":refs});
        let error=app.change(|d|schedule(d,&actor,&duplicate)).await.unwrap_err();
        assert_eq!(error.0,StatusCode::CONFLICT);assert!(error.1.contains("already have an editorial review"));
        assert!(!app.node.exists());assert!(!app.bridge.exists());
        let completed=run(app.clone(),job.clone()).await.unwrap();
        assert_eq!(completed["accepted"],refs);assert_eq!(completed["reused"],json!([refs[1]]));
        assert_eq!(completed["held"],json!([]));
        let receipt=local_admission::lookup(State(app.clone()),Extension(actor),Path(("editorial".into(),"queue-full-zero-model".into()))).await.unwrap().0;
        assert_eq!(receipt["status"],"committed");assert_eq!(receipt["result"]["jobId"],job);
        let d=app.read().await.unwrap();assert_eq!(list(&d,"jobs").len(),jobs_before+1,"Only the zero-model admission is new; prior native paid-proof fixture journals remain history");
        assert!(list(&d,"approvals").is_empty());assert!(list(&d,"operations").is_empty());
        app.db.close().await;
    }
    #[test]
    fn output_partitions_exact_scope_and_never_silently_accepts_missing_results() {
        let refs=json!([{"id":"a","revision":1},{"id":"b","revision":2},{"id":"c","revision":3}]);
        let plan=json!({"reused":[refs[0]],"held":[{"reference":refs[1],"reason":"Changed"}]});
        assert!(outcome(&refs,&plan,&[]).is_err());
        let result=json!({"outcomes":[{"proposalId":"c","decision":"revise","reason":"Intent","proposedText":"Suggestion"}]});
        let value=outcome(&refs,&plan,&[result]).unwrap();
        assert_eq!(value["accepted"],json!([refs[0]]));assert_eq!(value["held"].as_array().unwrap().len(),2);
        assert_eq!(value["held"][1]["suggestedText"],"Suggestion");
        let model_hold=json!({"outcomes":[{"proposalId":"c","decision":"hold","reason":"Insufficient evidence","proposedText":null}]});
        let held=outcome(&refs,&plan,&[model_hold]).unwrap();
        assert!(held["held"][1].get("suggestedText").is_none());
        assert_eq!(held["held"][1]["decision"],"hold");
        let failed_batch=json!({"request":{"editorialCandidates":[{"proposalId":"c"}]}});
        let preserved=outcome(&refs,&plan,&[held_batch(&failed_batch,"Editorial model review unavailable")]).unwrap();
        assert_eq!(preserved["accepted"],json!([refs[0]]));
        assert_eq!(preserved["held"][1]["reference"],refs[2]);
        assert_eq!(preserved["held"][1]["decision"],"hold");
    }
}
