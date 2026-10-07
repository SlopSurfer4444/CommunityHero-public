//! Explicit same-recipient semantic revision of preserved generated text.
//! No generation, paid retry, approval, publishing or reservation release.
use crate::*;
use axum::Extension;
use std::collections::BTreeSet;

pub(crate) const CONTRACT:&str="communityhero-paid-source-rebind-v1";
pub(crate) const FIELD:&str="sourceRebind";
pub(crate) const ADMISSION:&str="proposal-source-rebind";
const TRANSITION:&str="retained_text_semantic_revalidation";
tokio::task_local!{
    // Only the dedicated authenticated endpoint supplies this non-persisted
    // writer permit; a generic writer cannot mint matching JSON authority.
    static ADMISSION_PERMIT:(operator_auth::Actor,Value);
}
fn hash(value:&Value)->String{editorial_review::hash_text(&value.to_string())}
fn exact(value:&Value,fields:&[&str])->ApiResult<()>{
    if value.as_object().is_none_or(|o|o.len()!=fields.len()||fields.iter().any(|key|!o.contains_key(*key))){return Err(bad("Invalid exact source rebind fields"));}Ok(())
}
fn authority(actor:&operator_auth::Actor)->ApiResult<()>{
    if actor.role!="owner"{return Err(ApiError(StatusCode::FORBIDDEN,"Source rebind requires current owner authority".into()));}
    dispatch_authority::admit(&json!({"approvedBy":actor.public_json(),"approvalAuthority":dispatch_authority::approval_binding(actor)}),actor)?;Ok(())
}
fn references(body:&Value)->ApiResult<&Vec<Value>>{
    if body["contract"]!=CONTRACT||body["admissionMode"]!="partial"{return Err(bad("Explicit partial source rebind contract required"));}
    let refs=body["proposals"].as_array().filter(|refs|!refs.is_empty()&&refs.len()<=100).ok_or_else(||bad("Choose 1 to 100 exact generated drafts"))?;
    let mut seen=BTreeSet::new();
    for r in refs{exact(r,&["id","revision","expectedItemRevision"])?;
        if r["revision"].as_u64().is_none_or(|n|n==0)||r["expectedItemRevision"].as_u64().is_none_or(|n|n==0)||!seen.insert(required(r,"id")?){return Err(bad("Invalid or duplicate source rebind reference"));}}
    Ok(refs)
}
fn historical(p:&Value)->Value{let mut saved=p.clone();saved.as_object_mut().unwrap().remove("history");saved}
fn source_rows<'a>(record:&'a Value,key:&str)->ApiResult<&'a [Value]>{
    record[key].as_array().map(Vec::as_slice).ok_or_else(||conflict("old_source_unavailable"))
}
fn optional_rows<'a>(record:&'a Value,key:&str,reason:&str)->ApiResult<&'a [Value]>{
    match record.get(key){
        None=>Ok(&[]),
        Some(value)=>value.as_array().map(Vec::as_slice).ok_or_else(||conflict(reason)),
    }
}
fn old_source(job:&Value,p:&Value)->ApiResult<Value>{
    let request=&job["prepareBundle"]["request"];let id=required(p,"itemId")?;
    let item=source_rows(request,"items")?.iter().find(|item|item["id"]==id).ok_or_else(||conflict("old_source_unavailable"))?;
    let branches=source_rows(request,"branches")?.iter().filter(|branch|branch["id"]==item["branchId"]).cloned().collect::<Vec<_>>();
    let post_ids=branches.iter().map(|branch|branch["postId"].clone()).chain(std::iter::once(item["postId"].clone())).collect::<Vec<_>>();
    let posts=source_rows(request,"posts")?.iter().filter(|post|post_ids.contains(&post["id"])).cloned().collect::<Vec<_>>();
    if item["postId"].as_str().is_none_or(str::is_empty)||posts.len()!=1||posts[0]["id"]!=item["postId"]
        ||item["branchId"].as_str().is_some_and(|id|!id.is_empty())&&(branches.len()!=1||branches[0]["postId"]!=item["postId"]){
        return Err(conflict("old_source_unavailable"));
    }
    let post_keys=posts.iter().map(|post|post["postKey"].clone()).collect::<Vec<_>>();
    let materials=source_rows(request,"materials")?.iter().filter(|material|material["postKey"].as_str().is_none_or(str::is_empty)
        ||post_keys.contains(&material["postKey"])).cloned().collect::<Vec<_>>();
    let mut manifest=vec![];
    for pin in source_rows(request,"knowledgeManifest")?{
        let keys=source_rows(&pin["scope"],"postKeys")?;
        if keys.iter().any(|key|key.as_str().is_none_or(str::is_empty)){return Err(conflict("old_source_unavailable"));}
        if keys.is_empty()||keys.iter().any(|key|post_keys.contains(key)){manifest.push(pin.clone());}
    }
    // Canonical preparation omits an empty customer-context section. Its
    // absence proves no captured section, never complete customer history.
    let cases=optional_rows(request,"customerCases","old_source_unavailable")?;
    Ok(json!({"account":request["account"],"connectorBinding":request["connectorBinding"],"items":[item],"branches":branches,"posts":posts,
        "materials":materials,"knowledgeManifest":manifest,"customerCases":cases.iter().filter(|case|case["itemId"]==id).collect::<Vec<_>>(),
        "originalModelMaterialReceipt":p["modelMaterialReceipt"],"generationBundleId":job["prepareBundle"]["id"],"generationBundleDigest":job["prepareBundle"]["digest"]}))
}
fn source_delta(old:&Value,current:&Value)->Value{
    let mut delta=json!({});
    for key in ["items","branches","posts","materials","knowledgeManifest","customerCases"]{
        delta[key]=json!({"changed":old[key]!=current[key],"oldSha256":hash(&old[key]),"currentSha256":hash(&current[key])});
    }
    delta
}
fn target(item:&Value)->Value{json!({"itemId":item["id"],"revision":item["revision"],"contextEvidenceDigest":item["contextEvidenceDigest"],
    "branchContextDigest":item["branchContextDigest"],"postId":item["postId"],
    "routeTarget":{"id":item["id"],"objectId":item["objectId"],"itemId":item["itemId"],"postKey":item["postKey"],
        "conversationKey":item["conversationKey"],"branchId":item["branchId"],"connectorBinding":item["connectorBinding"]}})}
fn paid_origin(d:&Value,p:&Value,job:&Value)->Value{
    let root=job["originatingAnsweringAttemptId"].as_str().and_then(|key|list(d,"jobs").iter().find(|owner|owner["id"]==key));
    let root_budget=root.map(|root|json!({"id":root["id"],"prepareBundleDigest":root["prepareBundle"]["digest"],
        "repairBudget":root["preparationStages"]["repairBudget"],"answeringRepairs":root["preparationStages"]["answeringRepairs"],
        "repairPaidIntent":root["repairPaidIntent"],"repairMergeReceipt":root["repairMergeReceipt"]}));
    json!({"prepareRunId":p["prepareRunId"],"prepareBundleId":p["prepareBundleId"],"prepareBundleDigest":p["prepareBundleDigest"],
    "sourceContextDigest":p["sourceContextDigest"],"reviewContextDigest":p["reviewContextDigest"],"generationMetadata":p["generationMetadata"],
    "modelMaterialReceipt":p["modelMaterialReceipt"],"originatingAnsweringAttemptId":job["originatingAnsweringAttemptId"],
    "retainedEvidence":job["retainedEvidence"],"repairPaidIntent":job["repairPaidIntent"],"repairBudget":job["preparationStages"]["repairBudget"],
    "answeringRepairs":job["preparationStages"]["answeringRepairs"],"repairMergeReceipt":job["repairMergeReceipt"],"originalRootBudget":root_budget})}

pub(crate) fn original_proposal(p:&Value)->ApiResult<&Value>{
    let saved=&p[FIELD];let index=saved["historyIndex"].as_u64().and_then(|n|usize::try_from(n).ok()).ok_or_else(||conflict("Source rebind history unavailable"))?;
    p["history"].get(index).ok_or_else(||conflict("Source rebind old proposal unavailable"))
}

/// Review admission proves origin/current target without requiring the review
/// that is about to be scheduled. Action admission adds a fresh exact receipt.
pub(crate) fn validate_origin_and_current_target(context:&prepare_bundle::EvidenceContext<'_>,p:&Value)->ApiResult<Option<String>>{
    let Some(proof)=p.get(FIELD)else{return Ok(None)};
    let old=original_proposal(p)?;let d=context.workspace();let mut unsigned=proof.clone();
    unsigned.as_object_mut().ok_or_else(||conflict("Source rebind proof invalid"))?.remove("proofSha256");
    let to=proof["toRevision"].as_u64().ok_or_else(||conflict("Source rebind revision unavailable"))?;
    if proof["version"]!=1||proof["contract"]!=CONTRACT||proof["transitionKind"]!=TRANSITION||proof["proofSha256"]!=hash(&unsigned)
        ||proof["account"]!=d["account"]||proof["connectorBinding"]!=d["connectorBinding"]
        ||proof["fromRevision"].as_u64().and_then(|n|n.checked_add(1))!=Some(to)||p["revision"].as_u64().is_none_or(|n|n<to)
        ||old["revision"]!=proof["fromRevision"]||old["id"]!=p["id"]||old["itemId"]!=p["itemId"]||hash(old)!=proof["priorProposalSha256"]
        ||old["nativeCreationOrigin"]!="model_generation_v1"||old.get(retained_paid_recovery::FIELD).is_some()
        ||old.get(FIELD).is_some()||old["kind"]!="reply_and_close"||p["kind"]!=old["kind"]{
        return Err(conflict("Source rebind immutable origin or revision changed"));
    }
    let transition=if p["revision"]==to{p}else{list(p,"history").iter().find(|prior|prior["revision"]==to)
        .ok_or_else(||conflict("Source rebind transition revision disappeared"))?};
    if transition["text"]!=old["text"]||transition[FIELD]!=*proof{
        return Err(conflict("Source rebind preserved text or transition changed"));
    }
    for key in ["nativeCreationOrigin","prepareRunId","prepareBundleId","prepareBundleDigest","sourceContextDigest","reviewContextDigest","generationMetadata","modelMaterialReceipt","knowledgeManifest","knowledgePolicyVersion"]{
        if p.get(key)!=old.get(key){return Err(conflict("Source rebind old answering provenance changed"));}
    }
    let item=bound_item(&active_binding(d)?,row(d,"items",required(p,"itemId")?)?)?;
    if target(&item)!=proof["currentTarget"]||p["itemRevision"]!=item["revision"]||p["contextEvidenceDigest"]!=item["contextEvidenceDigest"]
        ||p["branchContextDigest"]!=item["branchContextDigest"]||target(&p["routeTarget"])!=proof["currentTarget"]{
        return Err(conflict("Source rebind current recipient or semantic bindings changed"));
    }
    if context.review_fingerprint(required(p,"itemId")?).map_err(conflict)?!=proof["currentEvidenceDigest"] {
        return Err(conflict("Source rebind reviewed semantic source changed"));
    }
    let owner=required(old,"prepareRunId")?;let job=row(d,"jobs",owner)?;
    context.reviewed_bundle_provenance(&job["prepareBundle"],required(p,"itemId")?).map_err(conflict)?;
    if paid_origin(d,old,job)!=proof["paidOrigin"]{return Err(conflict("Source rebind paid owner/root/budget changed"));}
    let scope=preparation_reservations::source_rebind_scope(d,old)?;
    if scope!=proof["reservationScope"]{return Err(conflict("Source rebind old/current reservation keys changed"));}
    preparation_unit::current(d,&proof["currentUnit"],&[p["itemId"].clone()],&now()).map_err(conflict)?;
    validate_route(p,&active_binding(d)?,&item)?;
    Ok(Some(owner.to_owned()))
}
pub(crate) fn require_current(context:&prepare_bundle::EvidenceContext<'_>,p:&Value)->ApiResult<()>{
    if validate_origin_and_current_target(context,p)?.is_none(){return Ok(());}
    editorial_review::require_current(context,p).map_err(conflict)?;
    if p["editorialReview"]["source"]["kind"]!="dedicated_model_review"||!p["editorialModelMaterialReceipt"].is_object(){
        return Err(conflict("Source rebind requires its own complete current model editorial delivery"));
    }
    preparation_materials::require_proposal(context,p).map_err(conflict)
}

fn stage(d:&Value,actor:&operator_auth::Actor,r:&Value)->ApiResult<Value>{
    let p=row(d,"proposals",required(r,"id")?)?;check_revision(p,&r["revision"])?;
    if p.get(retained_paid_recovery::FIELD).is_some(){return Err(conflict("retained_origin_rebind_unsupported"));}
    if p.get(FIELD).is_some(){return Err(conflict("An existing source rebind requires its current review or a separately admitted transition"));}
    if p["nativeCreationOrigin"]!="model_generation_v1"||p["kind"]!="reply_and_close"||!matches!(p["status"].as_str(),Some("draft"|"stale"))
        ||p["text"].as_str().is_none_or(|text|text.trim().is_empty())||p.get("recovery").is_some()||p.get("proposalRevalidation").is_some(){
        return Err(conflict("Source rebind supports only preserved native generated replies"));
    }
    if p["status"]=="stale"&&!matches!(p["staleReason"].as_str(),Some("Review source context changed"|"Preparation evidence changed; prepare again"|"Comment context changed; create a new proposal"|"Recovered preparation source changed")){
        return Err(conflict("Source rebind requires an authenticated native source-stale draft"));
    }
    let head=list(d,"proposals").iter().rev().find(|other|other["itemId"]==p["itemId"]);
    if head.is_none_or(|head|head["id"]!=p["id"]){return Err(conflict("Source rebind draft is not the current recipient head"));}
    let binding=active_binding(d)?;let item=bound_item(&binding,row(d,"items",required(p,"itemId")?)?)?;
    if item["revision"]!=r["expectedItemRevision"]||matches!(item["workflow"].as_str(),Some("waiting"|"closed"|"deleted"))||item["providerStatus"]=="deleted"{
        return Err(conflict("Source rebind recipient changed or is held"));
    }
    validate_route(p,&binding,&item)?;
    for op in list(d,"operations"){
        if op["proposalId"]==p["id"]||recipient_operation_blocks(op,p,&item){return Err(conflict("Source rebind recipient has an admitted operation"));}
    }
    let resource=ResourceRef::from_item(&binding,&item).map_err(|e|conflict(e.0))?;
    for other in list(d,"proposals").iter().filter(|other|other["id"]!=p["id"]&&matches!(other["status"].as_str(),Some("draft"|"approved"|"dispatching"|"unknown"))){
        let other_item=bound_item(&binding,row(d,"items",required(other,"itemId")?)?)?;
        if ResourceRef::from_item(&binding,&other_item).map_err(|e|conflict(e.0))?==resource{return Err(conflict("Source rebind recipient has a competing current draft"));}
    }
    for approval in list(d,"approvals").iter().filter(|a|a["status"]=="approved"){
        let entries=approval["proposals"].as_array().filter(|rows|!rows.is_empty()).ok_or_else(||conflict("Active approval scope unavailable"))?;
        for entry in entries{
            if entry["id"]==p["id"]||entry["proposal"]["itemId"]==p["itemId"]||entry["item"]["id"]==p["itemId"]{return Err(conflict("Source rebind recipient has an active approval"));}
            let target=if entry["item"].is_object(){entry["item"].clone()}else{row(d,"items",required(row(d,"proposals",required(entry,"id")?)?,"itemId")?)?.clone()};
            if ResourceRef::from_item(&binding,&bound_item(&binding,&target)?).map_err(|e|conflict(e.0))?==resource{return Err(conflict("Source rebind alias has an active approval"));}
        }
    }
    for job in list(d,"jobs").iter().filter(|j|j["kind"]=="editorial_review"&&matches!(j["status"].as_str(),Some("running"|"queued"))){
        let refs=job["editorialReferences"].as_array().ok_or_else(||conflict("Active editorial recipient scope unavailable"))?;
        for reference in refs{let prior=row(d,"proposals",required(reference,"id")?)?;let other=bound_item(&binding,row(d,"items",required(prior,"itemId")?)?)?;
            if ResourceRef::from_item(&binding,&other).map_err(|e|conflict(e.0))?==resource{return Err(conflict("Existing recipient editorial work must finish before source rebind"));}}
    }
    conductor_authority::fence_admission(d,"proposal",&[item.clone()])?;
    operator_close::assert_actor(p,actor)?;
    let owner=required(p,"prepareRunId")?;let job=row(d,"jobs",owner)?;let context=prepare_bundle::EvidenceContext::new(d);
    context.reviewed_bundle_provenance(&job["prepareBundle"],required(p,"itemId")?).map_err(conflict)?;
    if p["prepareBundleId"]!=job["prepareBundle"]["id"]||p["prepareBundleDigest"]!=job["prepareBundle"]["digest"]||!p["sourceContextDigest"].is_string(){
        return Err(conflict("old_source_unavailable"));
    }
    let current_digest=context.review_fingerprint(required(p,"itemId")?).map_err(conflict)?;
    if p["itemRevision"]==item["revision"]&&p["contextEvidenceDigest"]==item["contextEvidenceDigest"]
        &&p["branchContextDigest"]==item["branchContextDigest"]&&p["sourceContextDigest"]==current_digest{
        return Err(conflict("source_unchanged_use_existing_review"));
    }
    let revision=p["revision"].as_u64().and_then(|n|n.checked_add(1)).ok_or_else(||conflict("Source rebind revision exhausted"))?;
    let mut history=match p.get("history"){None=>vec![],Some(value)=>value.as_array().cloned().ok_or_else(||conflict("Source rebind history malformed"))?};
    let saved=historical(p);let index=history.len();history.push(saved.clone());let mut next=p.clone();
    next["history"]=json!(history);next["revision"]=json!(revision);next["status"]=json!("draft");
    next["itemRevision"]=item["revision"].clone();next["contextEvidenceDigest"]=item["contextEvidenceDigest"].clone();next["branchContextDigest"]=item["branchContextDigest"].clone();next["routeTarget"]=item.clone();
    next["decisionMediaContract"]=json!(decision_media::CONTRACT);
    for key in ["editorialReview","editorialReviews","editorialModelMaterialReceipt","operatorMaterialReceipt","mediaContextWaiver"]{next.as_object_mut().unwrap().remove(key);}
    let unit=preparation_unit::capture(d,&[p["itemId"].clone()],&now()).map_err(conflict)?;
    let scope=preparation_reservations::source_rebind_scope(d,p)?;
    // Paid capture attaches this array lazily. A legacy absence stays unknown;
    // malformed explicit evidence must not be downgraded to that legacy class.
    let paid_provenance_class=if optional_rows(job,"retainedEvidence","source_rebind_paid_evidence_unavailable")?.is_empty(){
        "legacy_saved_generated_text_delivery_unknown"
    }else{"native_retained_capture_references"};
    let mut proof=json!({"version":1,"contract":CONTRACT,"transitionKind":TRANSITION,"account":d["account"],"connectorBinding":d["connectorBinding"],
        "fromRevision":p["revision"],"toRevision":revision,"historyIndex":index,"priorProposalSha256":hash(&saved),"oldTarget":target(&p["routeTarget"]),
        "currentTarget":target(&item),"currentEvidenceDigest":current_digest,"currentUnit":unit,
        "reservationScope":scope,"paidOrigin":paid_origin(d,p,job),
        "paidProvenanceClass":paid_provenance_class,
        "adoptedBy":actor.public_json(),"authorityDigest":hash(&dispatch_authority::approval_binding(actor))});
    proof["proofSha256"]=json!(hash(&proof));next[FIELD]=proof;Ok(next)
}

pub(crate) fn capture(d:&Value,actor:&operator_auth::Actor,body:&Value)->ApiResult<Value>{
    authority(actor)?;let refs=references(body)?;
    for key in ["items","proposals","jobs","operations","approvals","audit","posts","branches","knowledge_entries","knowledge_versions"]{
        if !d[key].is_array(){return Err(conflict("Complete source rebind ledger unavailable"));}
    }
    let mut current=d.clone();let mut entries=vec![];let mut held=vec![];let mut recipients=BTreeSet::new();
    for reference in refs{
        let staged=(||->ApiResult<Value>{
            let next=stage(d,actor,reference)?;let key=required(&next,"id")?;
            let resource=ResourceRef::from_item(&active_binding(d)?,&next["routeTarget"]).map_err(|e|conflict(e.0))?;
            let recipient=json!([resource.binding.to_json(),resource.object_id,resource.item_id,resource.post_key,resource.conversation_key]).to_string();
            if !recipients.insert(recipient){return Err(conflict("Duplicate source rebind recipient"));}
            *row_mut(&mut current,"proposals",key)?=next.clone();
            let new_refs=json!([{"id":next["id"],"revision":next["revision"]}]);
            let plan=editorial_review::plan_fresh(&current,&new_refs,&now()).map_err(conflict)?;
            if let Some(reason)=list(&plan,"held").first(){return Err(conflict(reason["reason"].as_str().unwrap_or("Current source materials unavailable")));}
            if list(&plan,"batches").len()!=1{return Err(conflict("Source rebind requires one exact current editorial unit"));}
            let old=original_proposal(&next)?;let original=old_source(row(d,"jobs",required(old,"prepareRunId")?)?,old)?;
            let delta=source_delta(&original,&plan["batches"][0]["request"]);
            Ok(json!({"reference":reference,"proposal":next,"oldSource":original,"sourceDelta":delta,"currentReviewPlan":plan}))
        })();
        match staged{Ok(entry)=>entries.push(entry),Err(error)=>{
            if let Ok(old)=row(d,"proposals",required(reference,"id")?){*row_mut(&mut current,"proposals",required(reference,"id")?)?=old.clone();}
            held.push(json!({"reference":reference,"reason":error.1,"httpStatus":error.0.as_u16()}));
        }}
    }
    let mut preview=json!({"version":1,"contract":CONTRACT,"admissionMode":"partial","account":d["account"],"connectorBinding":d["connectorBinding"],
        "reviewedBy":actor.public_json(),"authorityDigest":hash(&dispatch_authority::approval_binding(actor)),"proposals":body["proposals"],"entries":entries,"held":held});
    if preview.to_string().len()>8*1024*1024{return Err(bad("Source rebind preview exceeds 8 MiB; select fewer drafts"));}
    preview["previewDigest"]=json!(hash(&preview));Ok(preview)
}
pub(crate) fn admit(d:&mut Value,actor:&operator_auth::Actor,body:&Value)->ApiResult<Value>{let mut next=d.clone();let result=admit_inner(&mut next,actor,body)?;*d=next;Ok(result)}
fn admit_inner(d:&mut Value,actor:&operator_auth::Actor,body:&Value)->ApiResult<Value>{
    exact(body,&["requestId","contract","admissionMode","proposals","previewDigest"])?;authority(actor)?;references(body)?;
    let request=local_admission::request(d,ADMISSION,body,actor)?.ok_or_else(||bad("Source rebind requestId required"))?;
    if let Some(result)=local_admission::replay(d,&request,actor)?{return Ok(result);}
    let preview=capture(d,actor,body)?;
    if preview["previewDigest"]!=body["previewDigest"]{return Err(conflict("Source rebind preview changed; review current evidence again"));}
    let mut old_refs=vec![];let mut new_refs=vec![];
    for entry in list(&preview,"entries"){
        let p=&entry["proposal"];old_refs.push(json!({"id":p["id"],"revision":p[FIELD]["fromRevision"]}));new_refs.push(json!({"id":p["id"],"revision":p["revision"]}));
        *row_mut(d,"proposals",required(p,"id")?)?=p.clone();
    }
    let mut result=json!({"contract":CONTRACT,"status":if new_refs.is_empty(){"held"}else{"rebound"},"oldRefs":old_refs,"newRefs":new_refs,"held":preview["held"],
        "approvalRequired":true,"editorialReviewRequired":true,"generationDispatched":false,"externalActions":0,"retryAllowed":false});
    list_mut(d,"audit").push(json!({"id":id(),"action":"proposal.source_rebound","refId":body["requestId"],"at":now(),"contract":CONTRACT,
        "actor":actor.public_json(),"previewDigest":preview["previewDigest"],"oldRefs":old_refs,"newRefs":new_refs,
        "proofs":list(&preview,"entries").iter().map(|entry|entry["proposal"][FIELD].clone()).collect::<Vec<_>>() }));
    local_admission::commit(d,&request,&mut result)?;Ok(result)
}
pub(crate) async fn preview(State(app):State<App>,Extension(actor):Extension<operator_auth::Actor>,Json(body):Json<Value>)->ApiResult<Json<Value>>{
    exact(&body,&["contract","admissionMode","proposals"])?;capture(&app.read().await?,&actor,&body).map(Json)
}
pub(crate) async fn post(State(app):State<App>,Extension(actor):Extension<operator_auth::Actor>,Json(body):Json<Value>)->ApiResult<Json<Value>>{
    exact(&body,&["requestId","contract","admissionMode","proposals","previewDigest"])?;authority(&actor)?;references(&body)?;
    if let Some(result)=local_admission::replay_committed(&app,ADMISSION,&body,&actor).await?{return Ok(Json(result));}
    ADMISSION_PERMIT.scope((actor.clone(),body.clone()),app.change(|d|admit_inner(d,&actor,&body))).await.map(Json)
}

/// Dedicated native writer permits only the reducer's exact source transition.
/// Generic writers preserve proof/history; they cannot mint or retarget it.
pub(crate) fn validate_change(before:&Value,after:&Value)->ApiResult<()>{
    for old in list(before,"proposals"){
        let next=list(after,"proposals").iter().find(|p|p["id"]==old["id"]);
        if let Some(proof)=old.get(FIELD){
            let next=next.ok_or_else(||conflict("Source rebind proposal cannot disappear"))?;
            if next.get(FIELD)!=Some(proof)||!list(next,"history").starts_with(list(old,"history")){return Err(conflict("Source rebind proof/history is immutable"));}
            for key in ["nativeCreationOrigin","prepareRunId","prepareBundleId","prepareBundleDigest","sourceContextDigest","reviewContextDigest","generationMetadata","modelMaterialReceipt","knowledgeManifest","knowledgePolicyVersion"]{
                if old.get(key)!=next.get(key){return Err(conflict("Source rebind original paid provenance is immutable"));}
            }
        }
    }
    for next in list(after,"proposals").iter().filter(|p|p.get(FIELD).is_some()){
        let old=list(before,"proposals").iter().find(|p|p["id"]==next["id"]).ok_or_else(||conflict("Source rebind cannot create a new proposal identity"))?;
        if old.get(FIELD).is_some(){continue;}
        let proofs=list(after,"audit").iter().skip(list(before,"audit").len()).filter(|event|event["action"]=="proposal.source_rebound"
            &&list(event,"proofs").contains(&next[FIELD])).collect::<Vec<_>>();
        if proofs.len()!=1||original_proposal(next)?!=&historical(old){return Err(conflict("Source rebind requires its atomic native transition receipt"));}
        let event=proofs[0];
        let (actor,body)=ADMISSION_PERMIT.try_with(Clone::clone).map_err(|_|conflict("Source rebind requires its dedicated native writer permit"))?;
        let expected=capture(before,&actor,&body)?;
        if expected["previewDigest"]!=body["previewDigest"]||event["refId"]!=body["requestId"]
            ||list(&expected,"entries").iter().find(|entry|entry["proposal"]["id"]==next["id"]).is_none_or(|entry|entry["proposal"]!=*next)
            ||event["actor"]!=actor.public_json()||event["actor"]!=next[FIELD]["adoptedBy"]
            ||next[FIELD]["authorityDigest"]!=hash(&dispatch_authority::approval_binding(&actor)){
            return Err(conflict("Source rebind exact native transition or authority changed"));
        }
        validate_origin_and_current_target(&prepare_bundle::EvidenceContext::new(after),next)?;
        let receipt=local_admission::find_receipt(after,ADMISSION,required(event,"refId")?)?.ok_or_else(||conflict("Source rebind local admission receipt missing"))?;
        if receipt["actorId"]!=event["actor"]["id"]||receipt["result"]["newRefs"].as_array().is_none_or(|refs|!refs.contains(&json!({"id":next["id"],"revision":next["revision"]}))){
            return Err(conflict("Source rebind local receipt target changed"));
        }
        let request=local_admission::request(after,ADMISSION,&body,&actor)?.ok_or_else(||conflict("Source rebind native request missing"))?;
        local_admission::replay(after,&request,&actor)?.ok_or_else(||conflict("Source rebind exact native request receipt missing"))?;
    }
    Ok(())
}

#[cfg(test)]
#[path="proposal_source_rebind_tests.rs"]
mod tests;
