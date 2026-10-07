//! Explicit owner-requested fresh review. Never erases text or invokes a model.
use serde_json::{Value,json};
use sha2::{Digest,Sha256};
use std::collections::BTreeSet;
use crate::{ApiResult,list,row,row_mut};

pub async fn restart(
    axum::extract::State(app):axum::extract::State<crate::App>,
    axum::Json(body):axum::Json<Value>,
)->ApiResult<axum::Json<Value>> {
    let (run,apply,scope,fresh)=validate(&body)?;
    if apply {
        app.change(|d| plan_context(d,&run,true,chrono::Utc::now().timestamp(),scope.as_deref(),fresh).map(axum::Json)).await
    } else {
        let mut snapshot=app.read().await?;
        Ok(axum::Json(plan_context(&mut snapshot,&run,false,chrono::Utc::now().timestamp(),scope.as_deref(),fresh)?))
    }
}
fn validate(body:&Value)->ApiResult<(String,bool,Option<Vec<String>>,bool)> {
    let fields=body.as_object().ok_or_else(||crate::bad("Restart request must be an object"))?;
    if fields.keys().any(|k| !["runId","apply","itemIds","freshContext"].contains(&k.as_str())) {return Err(crate::bad("Unknown restart field"));}
    let run=body["runId"].as_str().filter(|s|!s.is_empty()&&s.len()<=100&&s.bytes().all(|c|c.is_ascii_alphanumeric()||b"_-".contains(&c)))
        .ok_or_else(||crate::bad("Invalid restart runId"))?;
    let apply=body["apply"].as_bool().ok_or_else(||crate::bad("apply must be boolean"))?;
    let scope=if let Some(value)=body.get("itemIds") {
        let ids=value.as_array().filter(|v|!v.is_empty()&&v.len()<=100).ok_or_else(||crate::bad("Restart requires 1 to 100 itemIds"))?;
        let mut selected=BTreeSet::new();
        for id in ids {
            let id=id.as_str().filter(|s|!s.trim().is_empty()&&s.encode_utf16().count()<=256).ok_or_else(||crate::bad("Invalid restart itemId"))?;
            if !selected.insert(id.to_owned()) {return Err(crate::bad("Restart itemIds must be unique"));}
        }
        Some(selected.into_iter().collect())
    } else {None};
    let fresh=body.get("freshContext").map(|v|v.as_bool().ok_or_else(||crate::bad("freshContext must be boolean"))).transpose()?.unwrap_or(false);
    if fresh&&scope.is_none() {return Err(crate::bad("Fresh-context restart requires explicit itemIds"));}
    Ok((run.to_owned(),apply,scope,fresh))
}
fn automatic_job<'a>(d:&'a Value,p:&Value)->Option<&'a Value> {
    let run=p["prepareRunId"].as_str().or_else(||p["recovery"]["prepareRunId"].as_str())?;
    let job=row(d,"jobs",run).ok()?;
    if !matches!(job["purpose"].as_str(),Some("auto_prepare"|"auto_revalidate"))||job["status"]!="completed" {return None;}
    let bundle=&job["prepareBundle"];
    let origin=if p["recovery"].is_object(){&p["recovery"]}else{p};
    if bundle["version"]!=1 || bundle["itemIds"]!=json!([p["itemId"]])
        || bundle["digest"]!=format!("{:x}",Sha256::digest(bundle["request"].to_string().as_bytes()))
        || origin["prepareBundleId"]!=bundle["id"] || origin["prepareBundleDigest"]!=bundle["digest"] {return None;}
    Some(job)
}
// Membership is a protection boundary, never evidence authorizing a result.
pub(crate) fn job_targets(job:&Value,item_id:&Value)->bool {
    job["refId"]==*item_id || job["prepareBundle"]["itemIds"].as_array().is_some_and(|ids|ids.contains(item_id))
}
pub(crate) fn grouped_previous(d:&Value,item:&Value)->bool {
    let belongs=|job:&Value|job["purpose"]=="auto_prepare"
        && job["prepareBundle"]["itemIds"].as_array().is_some_and(|ids|ids.len()>1&&ids.contains(&item["id"]));
    let proposals=list(d,"proposals");
    // Historical decisions are retained for audit. Only the currently selected
    // decision and current job can constrain this item's next preparation.
    let selected=proposals.iter().rev().find(|p|p["itemId"]==item["id"]&&p["status"]=="draft")
        .or_else(||item["autoPreparation"]["savedProposalId"].as_str()
            .and_then(|id|proposals.iter().find(|p|p["id"]==id&&p["itemId"]==item["id"])));
    item["autoPreparation"]["jobId"].as_str().and_then(|id|row(d,"jobs",id).ok()).is_some_and(belongs)
        || selected.and_then(|p|p["prepareRunId"].as_str().or_else(||p["recovery"]["prepareRunId"].as_str()))
            .and_then(|id|row(d,"jobs",id).ok()).is_some_and(belongs)
}
fn guard(d:&Value,item:&Value)->Result<(), &'static str> {
    if !matches!(item["workflow"].as_str(),Some("attention"|"prepared"))
        || !matches!(item["providerStatus"].as_str(),Some("new"|"inprogress")) {return Err("not_open");}
    if item["draftEdited"]==true || !item["draft"].as_str().unwrap_or("").trim().is_empty()
        || !item["autoPreparation"]["humanOverrideAt"].is_null() {return Err("operator_edit");}
    if list(d,"jobs").iter().any(|j|job_targets(j,&item["id"])&&matches!(j["status"].as_str(),Some("queued"|"running")))
        || list(d,"operations").iter().any(|o|o["itemId"]==item["id"]) {return Err("active_or_recorded_operation");}
    let proposals:Vec<&Value>=list(d,"proposals").iter().filter(|p|p["itemId"]==item["id"]).collect();
    if proposals.iter().any(|p| matches!(p["status"].as_str(),Some("approved"|"dispatching"|"unknown"|"succeeded"))
        || p["origin"].is_object() || p["history"].as_array().is_some_and(|h|!h.is_empty())) {return Err("protected_proposal");}
    // A fresh generation must not silently obtain a new per-item budget from
    // a previously spent grouped job. Same-job completed review recovery is separate.
    if grouped_previous(d,item) {return Err("group_review_required");}
    if proposals.iter().any(|p|p["status"]=="draft"&&automatic_job(d,p).is_none()) {return Err("manual_or_unverified_proposal");}
    Ok(())
}
fn candidate(d:&Value,item:&Value,scoped:bool)->Result<Option<String>, &'static str> {
    guard(d,item)?;
    let proposals:Vec<&Value>=list(d,"proposals").iter().filter(|p|p["itemId"]==item["id"]).collect();
    // Prefer the currently displayed proposal, then its saved predecessor.
    let saved=proposals.iter().rev().find(|p|p["status"]=="draft")
        .copied().or_else(||item["autoPreparation"]["savedProposalId"].as_str().and_then(|id|proposals.iter().find(|p|p["id"]==id).copied()));
    if let Some(p)=saved {
        if !matches!(p["status"].as_str(),Some("draft"|"stale"))||automatic_job(d,p).is_none()
            || p["text"].as_str().is_some_and(|t|t.encode_utf16().count()>12000) {return Err("unverified_provenance");}
        return Ok(p["id"].as_str().map(str::to_owned));
    }
    let Some(job)=item["autoPreparation"]["jobId"].as_str().and_then(|id|row(d,"jobs",id).ok()) else {return Err("initial_preparation");};
    let b=&job["prepareBundle"];
    if scoped && item["autoPreparation"]["status"]=="error"
        && matches!(job["purpose"].as_str(),Some("auto_prepare"|"auto_revalidate"))
        && matches!(job["status"].as_str(),Some("completed"|"failed"))
        && job["refId"]==item["id"] && b["itemIds"]==json!([item["id"]])
        && b["request"]["items"].as_array().is_some_and(|v|v.len()==1&&v[0]["id"]==item["id"])
        && crate::prepare_bundle::current(d,b).is_ok() {return Ok(None);}
    if !matches!(job["purpose"].as_str(),Some("auto_prepare"|"auto_revalidate")) || job["status"]!="completed"
        || job["prepareOutcome"]["status"]!="needs_attention" || b["version"]!=1 || b["itemIds"]!=json!([item["id"]])
        || b["digest"]!=format!("{:x}",Sha256::digest(b["request"].to_string().as_bytes())) {return Err("unverified_provenance");}
    Ok(None)
}
pub(crate) fn plan(d:&mut Value,run:&str,apply:bool,now:i64)->ApiResult<Value> {
    plan_scoped(d,run,apply,now,None)
}
pub(crate) fn plan_scoped(d:&mut Value,run:&str,apply:bool,now:i64,scope:Option<&[String]>)->ApiResult<Value> {
    plan_context(d,run,apply,now,scope,false)
}
pub(crate) fn plan_context(d:&mut Value,run:&str,apply:bool,now:i64,scope:Option<&[String]>,fresh:bool)->ApiResult<Value> {
    let mut body=json!({"runId":run,"apply":apply});
    if let Some(scope)=scope {body["itemIds"]=json!(scope);}
    body["freshContext"]=json!(fresh);
    let (_,_,scope,_)=validate(&body)?;
    let requested=json!(scope);
    if let Some(receipt)=d["preparationRuns"].as_array().and_then(|runs|runs.iter().find(|r|r["runId"]==run)) {
        if receipt["requestedItemIds"]!=requested || (receipt["freshContext"]==true)!=fresh {return Err(crate::conflict("Restart runId is already bound to a different item scope or mode"));}
        return Ok(receipt.clone());
    }
    if let Some(scope)=&scope {for id in scope {row(d,"items",id)?;}}
    let mut eligible=Vec::new();let mut skipped=Vec::new();
    let mut error_retries=Vec::new();
    let mut fresh_reanalyses=Vec::new();
    for item in list(d,"items") {
        if let Some(scope)=&scope {
            if !scope.iter().any(|id|item["id"]==id.as_str()) {continue;}
        } else if !matches!(item["workflow"].as_str(),Some("attention"|"prepared")){continue;}
        if fresh {
            let proof=guard(d,item).and_then(|_|fresh_error_job(d,item)).and_then(|job|fresh_proof(d,item,job));
            match proof {
                Ok(proof)=>{fresh_reanalyses.push(proof);eligible.push((item["id"].as_str().unwrap_or("").to_owned(),None));},
                Err(reason)=>skipped.push(json!({"itemId":item["id"],"reason":reason})),
            }
            continue;
        }
        match candidate(d,item,scope.is_some()) {
            Ok(saved)=>{
                if saved.is_none()&&item["autoPreparation"]["status"]=="error" {
                    let job=row(d,"jobs",item["autoPreparation"]["jobId"].as_str().unwrap())?;
                    error_retries.push(json!({"itemId":item["id"],"jobId":job["id"],"bundleId":job["prepareBundle"]["id"],"bundleDigest":job["prepareBundle"]["digest"]}));
                }
                eligible.push((item["id"].as_str().unwrap_or("").to_owned(),saved));
            },
            Err(reason)=>skipped.push(json!({"itemId":item["id"],"reason":reason})),
        }
    }
    let at=chrono::DateTime::from_timestamp(now,0).unwrap().to_rfc3339();
    let receipt=json!({"runId":run,"applied":apply,"createdAt":at,"eligibleCount":eligible.len(),
        "requestedItemIds":requested,"errorRetries":error_retries,"freshContext":fresh,"freshReanalyses":fresh_reanalyses,
        "itemIds":eligible.iter().map(|(id,_)|id).collect::<Vec<_>>(),"skipped":skipped});
    if !apply {return Ok(receipt);}
    for (id,saved) in eligible {
        for p in crate::list_mut(d,"proposals").iter_mut().filter(|p|p["itemId"]==id&&p["status"]=="draft") {
            p["status"]=json!("stale");p["staleAt"]=json!(at);p["staleReason"]=json!("Operator requested a fresh preparation run");crate::bump(p);
        }
        let item=row_mut(d,"items",&id)?;
        item["workflow"]=json!("attention");item["decision"]=json!("needs_attention");
        item["autoPreparation"]["status"]=json!(if saved.is_some(){"stale"}else{"needs_attention"});
        item["autoPreparation"]["requiresReview"]=json!(true);
        item["autoPreparation"]["savedProposalId"]=json!(saved);
        item["autoPreparation"]["retryAt"]=Value::Null;
        item["autoPreparation"]["reason"]=json!("Запрошена новая подготовка. Предыдущее решение сохранено для сравнения.");
        item["reason"]=item["autoPreparation"]["reason"].clone();
        item["autoRevalidation"]=json!({"status":"requested","restartRunId":run,"requestedAt":at});
        crate::bump(item);
    }
    if !d["preparationRuns"].is_array(){d["preparationRuns"]=json!([]);}
    d["preparationRuns"].as_array_mut().unwrap().push(receipt.clone());
    Ok(receipt)
}

// Only an applied, explicitly scoped receipt authorizes the failed source job.
pub(crate) fn authorized_error_retry(d:&Value,item:&Value,job:&Value)->bool {
    let Some(run)=item["autoRevalidation"]["restartRunId"].as_str() else {return false;};
    list(d,"preparationRuns").iter().any(|r|r["runId"]==run&&r["applied"]==true
        && r["requestedItemIds"].as_array().is_some_and(|ids|ids.contains(&item["id"]))
        && r["itemIds"].as_array().is_some_and(|ids|ids.contains(&item["id"]))
        && r["errorRetries"].as_array().is_some_and(|rows|rows.iter().any(|e|e["itemId"]==item["id"]
            && e["jobId"]==job["id"]&&e["bundleId"]==job["prepareBundle"]["id"]&&e["bundleDigest"]==job["prepareBundle"]["digest"])))
}

fn fresh_error_job<'a>(d:&'a Value,item:&Value)->Result<&'a Value,&'static str> {
    // A held revalidation is the latest failure; never fall back to the older
    // initial job when that pointer exists but cannot be authenticated.
    let id=if item["autoRevalidation"]["status"]=="held" {
        item["autoRevalidation"]["jobId"].as_str()
    } else if item["autoPreparation"]["status"]=="error" {
        item["autoPreparation"]["jobId"].as_str()
    } else {None}.ok_or("no_failed_preparation")?;
    let job=row(d,"jobs",id).map_err(|_|"failed_job_missing")?;
    if list(d,"jobs").iter().rev().find(|j|job_targets(j,&item["id"])
        && matches!(j["purpose"].as_str(),Some("auto_prepare"|"auto_revalidate"))).is_none_or(|j|j["id"]!=id) {return Err("failed_job_superseded");}
    Ok(job)
}
fn fresh_proof(d:&Value,item:&Value,job:&Value)->Result<Value,&'static str> {
    let bundle=&job["prepareBundle"];
    if job["status"]!="failed" || !matches!(job["purpose"].as_str(),Some("auto_prepare"|"auto_revalidate"))
        || job["refId"]!=item["id"] || !job["error"].as_str().is_some_and(|s|!s.is_empty())
        || bundle["version"]!=1 || bundle["itemIds"]!=json!([item["id"]])
        || bundle["digest"]!=format!("{:x}",Sha256::digest(bundle["request"].to_string().as_bytes())) {
        return Err("unverified_failed_bundle");
    }
    let old=bundle["request"]["items"].as_array().filter(|items|items.len()==1&&items[0]["id"]==item["id"])
        .and_then(|items|items.first()).ok_or("failed_recipient_mismatch")?;
    let binding=crate::active_binding(d).map_err(|_|"current_binding_unavailable")?;
    crate::bridge_account(&binding).map_err(|_|"current_binding_unavailable")?;
    if bundle["request"]["account"]!=d["account"] || bundle["request"]["connectorBinding"]!=d["connectorBinding"] {
        return Err("failed_account_binding_changed");
    }
    let current=crate::bound_item(&binding,item).map_err(|_|"current_target_unavailable")?;
    let old=crate::bound_item(&binding,old).map_err(|_|"failed_target_binding_changed")?;
    let fields=["objectId","itemId","postKey","conversationKey","platform","connectorBinding"];
    if fields.iter().any(|key|old[*key]!=current[*key]) {return Err("failed_target_binding_changed");}
    let target:serde_json::Map<String,Value>=fields.iter().map(|key|((*key).to_owned(),current[*key].clone())).collect();
    // Reanalyse current evidence. Authentic old evidence is lineage only and is
    // intentionally not accepted as a current bundle or as a model result.
    let fresh=crate::prepare_bundle::triage(d,item["id"].as_str().ok_or("invalid_recipient")?).map_err(|_|"current_evidence_unavailable")?;
    crate::prepare_bundle::current(d,&fresh).map_err(|_|"current_evidence_unavailable")?;
    let fingerprint=crate::prepare_bundle::review_fingerprint(d,item["id"].as_str().unwrap()).map_err(|_|"current_evidence_unavailable")?;
    Ok(json!({"itemId":item["id"],"jobId":job["id"],"bundleId":bundle["id"],"bundleDigest":bundle["digest"],
        "currentFingerprint":fingerprint,"account":d["account"],"binding":binding.to_json(),"target":target}))
}
pub(crate) fn fresh_context_requested(d:&Value,item:&Value)->bool {
    d["preparationRuns"].as_array().into_iter().flatten().any(|r|r["runId"]==item["autoRevalidation"]["restartRunId"]&&r["freshContext"]==true)
}
pub(crate) fn fresh_context_previous(d:&Value,item:&Value)->Option<(Value,Value)> {
    let run=item["autoRevalidation"]["restartRunId"].as_str()?;
    let receipt=list(d,"preparationRuns").iter().find(|r|r["runId"]==run&&r["freshContext"]==true&&r["applied"]==true
        &&r["requestedItemIds"].as_array().is_some_and(|ids|ids.contains(&item["id"]))
        &&r["itemIds"].as_array().is_some_and(|ids|ids.contains(&item["id"])))?;
    let proof=receipt["freshReanalyses"].as_array()?.iter().find(|p|p["itemId"]==item["id"])?;
    let job=row(d,"jobs",proof["jobId"].as_str()?).ok()?;
    guard(d,item).ok()?;
    if fresh_proof(d,item,job).ok()?!=*proof {return None;}
    if list(d,"jobs").iter().rev().find(|j|job_targets(j,&item["id"])
        &&matches!(j["purpose"].as_str(),Some("auto_prepare"|"auto_revalidate"))).is_none_or(|j|j["id"]!=job["id"]) {return None;}
    Some((job["prepareBundle"].clone(),json!({"itemId":item["id"],"prepareRunId":job["id"],"proposalId":null,
        "outcome":"needs_attention","text":"","reason":job["error"],"freshContext":true})))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn scope_validation_is_bounded_unique_and_order_independent() {
        assert_eq!(validate(&json!({"runId":"r","apply":false,"itemIds":["b","a"]})).unwrap().2,Some(vec!["a".into(),"b".into()]));
        assert!(validate(&json!({"runId":"r","apply":false})).unwrap().2.is_none());
        for scope in [json!(null),json!([]),json!([" "]),json!([1]),json!(["a","a"]),json!(["x".repeat(257)]),json!((0..101).map(|i|i.to_string()).collect::<Vec<_>>())] {
            assert!(validate(&json!({"runId":"r","apply":false,"itemIds":scope})).is_err());
        }
        assert!(validate(&json!({"runId":"r","apply":true,"itemIds":(0..100).map(|i|i.to_string()).collect::<Vec<_>>() })).is_ok());
        assert!(validate(&json!({"runId":"r","apply":false,"freshContext":true})).is_err());
        assert!(validate(&json!({"runId":"r","apply":false,"itemIds":["i"],"freshContext":"yes"})).is_err());
    }
}
