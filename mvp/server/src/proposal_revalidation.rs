//! Explicit same-ID adoption of current decision review for legacy drafts.
//! This never generates text, claims media readiness, approves or dispatches.
use crate::*;
use axum::Extension;
use std::collections::BTreeSet;

pub(crate) const CONTRACT:&str="communityhero-legacy-proposal-revalidation-v1";
const ADMISSION:&str="proposal-revalidate";
fn exact(v:&Value,keys:&[&str])->ApiResult<()> {
    if v.as_object().is_none_or(|o|o.len()!=keys.len()||keys.iter().any(|key|!o.contains_key(*key))){
        return Err(bad("Invalid exact proposal revalidation fields"));
    }Ok(())
}
fn authority(actor:&operator_auth::Actor)->ApiResult<()> {
    if actor.role!="owner"{return Err(ApiError(StatusCode::FORBIDDEN,"Proposal revalidation requires current owner authority".into()));}
    dispatch_authority::admit(&json!({"approvedBy":actor.public_json(),"approvalAuthority":dispatch_authority::approval_binding(actor)}),actor)?;
    Ok(())
}
fn references(body:&Value)->ApiResult<&Vec<Value>> {
    if body["contract"]!=CONTRACT||body["admissionMode"]!="partial"{return Err(bad("Explicit legacy revalidation and partial admission contract required"));}
    let refs=body["proposals"].as_array().filter(|refs|!refs.is_empty()&&refs.len()<=100).ok_or_else(||bad("Choose 1 to 100 exact legacy drafts"))?;
    let mut ids=BTreeSet::new();
    for r in refs {exact(r,&["id","revision"])?;if r["revision"].as_u64().is_none_or(|v|v==0)||!ids.insert(required(r,"id")?){return Err(bad("Invalid or duplicate revalidation reference"));}}
    Ok(refs)
}
fn historical(p:&Value)->Value {let mut saved=p.clone();saved.as_object_mut().unwrap().remove("history");saved}
fn hash(value:&Value)->String {editorial_review::hash_text(&value.to_string())}
fn recipient_key(item:&Value)->ApiResult<String>{
    let binding=ConnectorBinding::from_json(&item["connectorBinding"]).map_err(|e|conflict(e.0))?;
    Ok(json!([binding.workspace_id,binding.account_id,binding.id,binding.connector.as_str(),binding.provider_account_id,
        required(item,"objectId")?,required(item,"itemId")?]).to_string())
}
fn stage(d:&Value,actor:&operator_auth::Actor,r:&Value)->ApiResult<Value> {
    let p=row(d,"proposals",required(r,"id")?)?;check_revision(p,&r["revision"])?;
    if p["status"]!="draft"||p.get("decisionMediaContract").is_some()||p.get("proposalRevalidation").is_some(){return Err(conflict("Only an exact unapproved legacy draft may adopt the current contract"));}
    if !matches!(p["kind"].as_str(),Some("reply_and_close"|"close"|"delete")){return Err(conflict("Legacy decision kind is outside revalidation"));}
    let revision=p["revision"].as_u64().and_then(|v|v.checked_add(1)).ok_or_else(||conflict("Proposal revision exhausted"))?;
    let binding=active_binding(d)?;let item=bound_item(&binding,row(d,"items",required(p,"itemId")?)?)?;
    if item["revision"]!=p["itemRevision"]||item["contextEvidenceDigest"]!=p["contextEvidenceDigest"]||item["branchContextDigest"]!=p["branchContextDigest"]
        ||matches!(item["workflow"].as_str(),Some("waiting"|"closed"|"deleted"))||item["providerStatus"]=="deleted" {
        return Err(conflict("Legacy recipient context changed or is held"));
    }
    validate_route(p,&binding,&item)?;
    let head=list(d,"proposals").iter().rev().find(|other|other["itemId"]==p["itemId"]);
    if head.is_none_or(|head|head["id"]!=p["id"]){return Err(conflict("Legacy draft is not the current recipient head"));}
    let recipient=recipient_key(&item)?;
    for other in list(d,"proposals").iter().filter(|other|other["id"]!=p["id"]&&matches!(other["status"].as_str(),Some("draft"|"approved"|"dispatching"|"unknown"))){
        if other["itemId"]==p["itemId"]{
            if other["status"]!="draft"{return Err(conflict("Recipient has a competing approved or dispatched proposal"));}
            continue;
        }
        let other_item=bound_item(&binding,row(d,"items",required(other,"itemId")?)?)?;
        if recipient_key(&other_item)?==recipient{return Err(conflict("Alias recipient has another current proposal"));}
    }
    if list(d,"operations").iter().any(|op|op["proposalId"]==p["id"]||recipient_operation_blocks(op,p,&item)){
        return Err(conflict("Legacy proposal or recipient has an admitted operation"));
    }
    for approval in list(d,"approvals").iter().filter(|approval|approval["status"]=="approved"){
        let entries=approval["proposals"].as_array().filter(|entries|!entries.is_empty()).ok_or_else(||conflict("Active approval recipient scope is unavailable"))?;
        for entry in entries {
            if entry["id"]==p["id"]||entry["proposal"]["itemId"]==p["itemId"]||entry["item"]["id"]==p["itemId"]{
                return Err(conflict("Legacy recipient belongs to an active approval"));
            }
            let target=if entry["item"].is_object(){entry["item"].clone()}else{
                let prior=row(d,"proposals",required(entry,"id")?)?;
                row(d,"items",required(prior,"itemId")?)?.clone()
            };
            if recipient_key(&bound_item(&binding,&target)?)?==recipient{return Err(conflict("Alias recipient belongs to an active approval"));}
        }
    }
    if operator_editorial::pending(d,p){return Err(conflict("Existing editorial work must finish before revalidation"));}
    for job in list(d,"jobs").iter().filter(|job|job["kind"]=="editorial_review"&&matches!(job["status"].as_str(),Some("queued"|"running"))){
        // A queued review may still reference an older same-recipient draft and
        // have no captured plan. It remains paid work, never implicit takeover.
        for reference in list(job,"editorialReferences"){
            let prior=row(d,"proposals",required(reference,"id")?)?;
            let target=bound_item(&binding,row(d,"items",required(prior,"itemId")?)?)?;
            if recipient_key(&target)?==recipient{return Err(conflict("Existing recipient editorial work must finish before revalidation"));}
        }
        if !job["editorialReferences"].is_array()&&!job["editorialPlan"]["batches"].is_array(){return Err(conflict("Active editorial recipient scope is unavailable"));}
    }
    if list(d,"jobs").iter().any(|job|matches!(job["status"].as_str(),Some("queued"|"running"))&&job["id"]==p["prepareRunId"]){
        return Err(conflict("Existing generation must finish before revalidation"));
    }
    conductor_authority::fence_admission(d,"proposal",&[item.clone()])?;
    operator_close::assert_actor(p,actor)?;operator_close::assert_preparation(d,p)?;
    let context=prepare_bundle::EvidenceContext::new(d);
    if let Some(run)=p["prepareRunId"].as_str(){
        let bundle=&row(d,"jobs",run)?["prepareBundle"];
        if p["prepareBundleId"]!=bundle["id"]||p["prepareBundleDigest"]!=bundle["digest"]{return Err(conflict("Legacy preparation provenance changed"));}
        context.reviewed_bundle_provenance(bundle,required(p,"itemId")?).map_err(conflict)?;
        // Missing historical review metadata must not become a shortcut past
        // the unchanged legacy preparation-source guard at approval.
        if !p["reviewContextDigest"].is_string(){context.current(bundle).map_err(conflict)?;}
    }
    let mut next=p.clone();let mut history=match p.get("history"){
        None=>vec![],Some(value)=>value.as_array().cloned().ok_or_else(||conflict("Legacy proposal history is malformed"))?};
    let history_index=history.len();let saved=historical(p);history.push(saved.clone());
    next["history"]=json!(history);next["revision"]=json!(revision);next["decisionMediaContract"]=json!(decision_media::CONTRACT);
    if p.get("operatorCloseDecision").is_some(){next["operatorCloseDecision"]=operator_close::rebind_existing_for_revalidation(d,p,&item,actor,revision)?;}
    next["proposalRevalidation"]=json!({"version":1,"contract":CONTRACT,"method":"preserve_existing_decision",
        "fromRevision":p["revision"],"adoptedRevision":revision,"historyIndex":history_index,"priorProposalSha256":hash(&saved),
        "adoptedBy":actor.public_json(),"authorityDigest":hash(&dispatch_authority::approval_binding(actor))});
    Ok(next)
}

pub(crate) fn capture(d:&Value,actor:&operator_auth::Actor,body:&Value)->ApiResult<Value> {
    authority(actor)?;let refs=references(body)?;
    // Production callers use full Database::read/change, never a bootstrap or
    // scoped projection. Complete histories remain visible to every guard.
    for key in ["items","proposals","jobs","operations","approvals","audit","posts","branches"]{
        if !d[key].is_array(){return Err(conflict("Complete revalidation ledger is unavailable"));}
    }
    let mut current=d.clone();let mut entries=vec![];let mut held=vec![];let mut recipients=BTreeSet::new();
    for r in refs {
        let staged=(||->ApiResult<Value>{
            let next=stage(d,actor,r)?;
            let item=bound_item(&active_binding(d)?,row(d,"items",required(&next,"itemId")?)?)?;
            let recipient=recipient_key(&item)?;
            if recipients.contains(&recipient){return Err(conflict("Duplicate revalidation recipient"));}
            *row_mut(&mut current,"proposals",required(r,"id")?)?=next.clone();
            let (candidate,evidence)=editorial_review::operator_candidate(&current,&next).map_err(conflict)?;
            recipients.insert(recipient);
            Ok(json!({"reference":r,"proposal":next,"candidate":candidate,"evidence":evidence}))
        })();
        match staged {Ok(entry)=>entries.push(entry),Err(error)=>{
            // A held temporary proposal cannot influence a later recipient.
            if let Ok(original)=row(d,"proposals",required(r,"id")?){*row_mut(&mut current,"proposals",required(r,"id")?)?=original.clone();}
            held.push(json!({"reference":r,"reason":error.1,"httpStatus":error.0.as_u16()}));
        }}
    }
    let mut preview=json!({"version":1,"contract":CONTRACT,"admissionMode":"partial","account":d["account"],"connectorBinding":d["connectorBinding"],
        "reviewedBy":actor.public_json(),"authorityDigest":hash(&dispatch_authority::approval_binding(actor)),"proposals":body["proposals"],"entries":entries,"held":held});
    if preview.to_string().len()>8*1024*1024{return Err(bad("Revalidation preview exceeds 8 MiB; select fewer drafts"));}
    preview["previewDigest"]=json!(hash(&preview));Ok(preview)
}

/// A migrated owner CLOSE retains its action-only exception, but this new
/// generation always needs fresh semantic review before approval/dispatch.
pub(crate) fn require_current(context:&prepare_bundle::EvidenceContext<'_>,p:&Value)->ApiResult<()> {
    let Some(saved)=p.get("proposalRevalidation") else{return Ok(())};
    let from=saved["fromRevision"].as_u64();let adopted=saved["adoptedRevision"].as_u64();
    let index=saved["historyIndex"].as_u64().and_then(|n|usize::try_from(n).ok());
    if saved["version"]!=1||saved["contract"]!=CONTRACT||saved["method"]!="preserve_existing_decision"||!decision_media::enabled(p)
        ||from.and_then(|n|n.checked_add(1))!=adopted||adopted.is_none()
        ||p["revision"].as_u64().zip(adopted).is_none_or(|(now,adopted)|now<adopted)
        ||index.and_then(|n|p["history"].get(n)).is_none_or(|old|old["id"]!=p["id"]||old["revision"]!=saved["fromRevision"]||hash(old)!=saved["priorProposalSha256"]){
        return Err(conflict("Proposal revalidation history changed"));
    }
    editorial_review::require_current(context,p).map_err(conflict)
}

pub(crate) fn admit(d:&mut Value,actor:&operator_auth::Actor,body:&Value)->ApiResult<Value>{
    let mut next=d.clone();let result=admit_inner(&mut next,actor,body)?;*d=next;Ok(result)
}
fn admit_inner(d:&mut Value,actor:&operator_auth::Actor,body:&Value)->ApiResult<Value>{
    exact(body,&["requestId","contract","admissionMode","proposals","previewDigest"])?;authority(actor)?;references(body)?;
    let request=local_admission::request(d,ADMISSION,body,actor)?.ok_or_else(||bad("Revalidation requestId required"))?;
    if let Some(result)=local_admission::replay(d,&request,actor)?{return Ok(result);}
    let preview=capture(d,actor,body)?;
    if preview["previewDigest"]!=body["previewDigest"]{return Err(conflict("Legacy revalidation preview changed; review current evidence again"));}
    let mut old_refs=vec![];let mut new_refs=vec![];let mut transitions=vec![];
    for entry in list(&preview,"entries"){
        let next=&entry["proposal"];let key=required(next,"id")?;
        old_refs.push(entry["reference"].clone());new_refs.push(json!({"id":key,"revision":next["revision"]}));
        let old=row(d,"proposals",key)?;
        transitions.push(json!({"id":key,"fromRevision":old["revision"],"toRevision":next["revision"],
            "priorProposalSha256":next["proposalRevalidation"]["priorProposalSha256"],
            "priorOperatorCloseDecisionSha256":old["operatorCloseDecision"]["decisionSha256"],
            "operatorCloseDecisionSha256":next["operatorCloseDecision"]["decisionSha256"],"editorialReviewRequired":true}));
        *row_mut(d,"proposals",key)?=next.clone();
    }
    let mut result=json!({"contract":CONTRACT,"status":if new_refs.is_empty(){"held"}else{"revalidated"},
        "oldRefs":old_refs,"newRefs":new_refs,"held":preview["held"],"approvalRequired":true,"editorialReviewRequired":true,"retryAllowed":false});
    list_mut(d,"audit").push(json!({"id":id(),"action":"proposal.revalidated","refId":body["requestId"],"at":now(),
        "contract":CONTRACT,"actor":actor.public_json(),"previewDigest":preview["previewDigest"],"oldRefs":old_refs,"newRefs":new_refs,"transitions":transitions}));
    local_admission::commit(d,&request,&mut result)?;Ok(result)
}
pub(crate) async fn preview(State(app):State<App>,Extension(actor):Extension<operator_auth::Actor>,Json(body):Json<Value>)->ApiResult<Json<Value>>{
    exact(&body,&["contract","admissionMode","proposals"])?;
    authority(&actor)?;references(&body)?;
    capture(&app.read().await?,&actor,&body).map(Json)
}
pub(crate) async fn post(State(app):State<App>,Extension(actor):Extension<operator_auth::Actor>,Json(body):Json<Value>)->ApiResult<Json<Value>>{
    exact(&body,&["requestId","contract","admissionMode","proposals","previewDigest"])?;authority(&actor)?;references(&body)?;
    if let Some(result)=local_admission::replay_committed(&app,ADMISSION,&body,&actor).await?{return Ok(Json(result));}
    // Existing full workspace writer/row lock supplies complete source/history
    // and atomic rollback. Do not extend the narrow text-edit storage exception.
    app.change(|d|admit_inner(d,&actor,&body)).await.map(Json)
}

#[cfg(test)]
#[path="proposal_revalidation_tests.rs"]
mod tests;
