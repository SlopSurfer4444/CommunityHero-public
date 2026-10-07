//! Explicit subset admission, evaluated against one transaction snapshot.
//! The original references and all holds remain immutable in the admission receipt.
use crate::*;
use std::collections::HashMap;

fn recipient_key(item: &Value) -> ApiResult<String> {
    let binding = ConnectorBinding::from_json(&item["connectorBinding"]).map_err(|e| conflict(e.0))?;
    Ok(json!([binding.workspace_id, binding.account_id, binding.id,
        binding.connector.as_str(), binding.provider_account_id,
        required(item, "objectId")?, required(item, "itemId")?]).to_string())
}

pub(crate) fn partial(body: &Value) -> ApiResult<bool> {
    match body.get("admissionMode").and_then(Value::as_str) {
        None if body.get("admissionMode").is_none() => Ok(false),
        Some("atomic") => Ok(false),
        Some("partial") if body["requestId"].as_str().is_some_and(|s| !s.is_empty()) => Ok(true),
        Some("partial") => Err(bad("Partial admission requires a durable requestId")),
        _ => Err(bad("Invalid approval admissionMode")),
    }
}

pub(crate) fn create(d: &mut Value, actor: &operator_auth::Actor, body: &Value) -> ApiResult<Value> {
    let is_partial = partial(body)?;
    let refs = body["proposals"].as_array().filter(|v| !v.is_empty() && v.len() <= 100)
        .ok_or_else(|| bad("Choose 1 to 100 proposals"))?;
    // Detect every ambiguous recipient before admitting any entry. Choosing the
    // first competing action would silently change the operator's reviewed scope.
    let mut counts = HashMap::new();
    let mut recipient_counts = HashMap::new();
    let binding = active_binding(d).ok();
    for r in refs {
        if let Ok(p) = row(d, "proposals", r["id"].as_str().unwrap_or("")) {
            *counts.entry(p["itemId"].to_string()).or_insert(0usize) += 1;
            if let Some(binding) = &binding {
                if let Ok(item) = row(d,"items",p["itemId"].as_str().unwrap_or(""))
                    .and_then(|item| bound_item(binding,item)) {
                    if let Ok(key) = recipient_key(&item) {
                        *recipient_counts.entry(key).or_insert(0usize) += 1;
                    }
                }
            }
        }
    }
    let global_error = if is_partial {
        active_binding(d).map(|_| ()).and_then(|_| {
            dispatch_authority::admit(&json!({"approvedBy":actor.public_json(),
                "approvalAuthority":dispatch_authority::approval_binding(actor)}), actor).map(|_| ())
        }).err()
    } else { None };
    let context = prepare_bundle::EvidenceContext::new(d);
    let mut exact = vec![];
    let mut accepted = vec![];
    let mut held = vec![];
    for r in refs {
        let result = (|| -> Result<Value, (&str, ApiError)> {
            if let Some(error) = &global_error {
                return Err(("shared_scope_or_authority_invalid", ApiError(error.0, error.1.clone())));
            }
            let key = required(r, "id").map_err(|e| ("invalid_reference", e))?;
            let p = row(d, "proposals", key).map_err(|e| ("proposal_missing", e))?;
            check_revision(p, &r["revision"]).map_err(|e| ("proposal_revision_changed", e))?;
            if !["draft", "approved"].contains(&p["status"].as_str().unwrap_or("")) {
                return Err(("proposal_unavailable", conflict("Proposal is no longer available")));
            }
            if counts.get(&p["itemId"].to_string()).copied().unwrap_or(0) != 1 {
                return Err(("duplicate_recipient", bad("One action per recipient required")));
            }
            operator_close::assert_preparation(d,p)
                .map_err(|e| ("preparation_scope_reserved", e))?;
            operator_close::assert_actor(p,actor).map_err(|e| ("operator_close_authority_changed", e))?;
            retained_paid_recovery::assert_actor(p,actor).map_err(|e| ("retained_recovery_authority_changed", e))?;
            let item = proposal_current_with_context(p, &context).map_err(|e| ("context_or_evidence_changed", e))?;
            if p["kind"]=="reply_and_close" {
                editorial_review::require_current(&context,p)
                    .map_err(|reason|("editorial_review_required",conflict(reason)))?;
            }
            let recipient = recipient_key(&item).map_err(|e| ("context_or_evidence_changed", e))?;
            if recipient_counts.get(&recipient).copied().unwrap_or(0) != 1 {
                return Err(("duplicate_recipient", bad("One action per connector recipient required")));
            }
            if is_partial && list(d, "operations").iter().any(|op| recipient_operation_blocks_current(d, op, p, &item)) {
                return Err(("recipient_operation_blocked", conflict("Recipient already has an unresolved or completed operation")));
            }
            let photo_proof=media_context_gate::photo_proof(&context,p).map_err(|e|("photo_acquisition_changed",e))?;
            let mut exact=json!({"id":p["id"],"revision":p["revision"],"proposal":p,"item":item});
            if !photo_proof.is_null(){exact["approvedPhotoAcquisitionProof"]=photo_proof;}
            Ok(exact)
        })();
        match result {
            Ok(value) => { accepted.push(json!({"id":value["id"],"revision":value["revision"]})); exact.push(value); }
            Err((reason, error)) if is_partial => held.push(json!({"reference":r,"reason":reason,
                "message":error.1,"httpStatus":error.0.as_u16()})),
            Err((_, error)) => return Err(error),
        }
    }
    if exact.is_empty() {
        return Ok(json!({"id":null,"status":"held","admissionMode":"partial","accepted":accepted,"held":held}));
    }
    let targets:Vec<Value>=exact.iter().map(|entry|entry["item"].clone()).collect();
    let conductor=conductor_authority::fence_admission(d,"approval",&targets)?;
    conductor_authority::fence_actor(conductor.as_ref(),actor)?;
    if conductor.is_some() {
        for entry in &exact {
            conductor_authority::fence_admission(d,required(&entry["proposal"],"kind")?,&[entry["item"].clone()])?;
            conductor_authority::require_prior_attribution(conductor.as_ref(),&entry["proposal"])?;
        }
    }
    let key = id();
    let mut approval = json!({"id":key,"proposals":exact,"status":"approved","createdAt":now(),
        "approvedBy":actor.public_json(),"approvalAuthority":dispatch_authority::approval_binding(actor)});
    if let Some(ctx)=conductor.as_ref(){conductor_authority::tag(ctx,&mut approval);}
    // Preserve prior immutable approvals/operations; only this newly admitted
    // generation requires the editorial receipt again before dispatch.
    if exact.iter().any(|e|e["proposal"]["kind"]=="reply_and_close") {
        approval["editorialPolicyVersion"]=json!(1);
    }
    if is_partial {
        approval["admission"] = json!({"requestId":body["requestId"],"mode":"partial",
            "requested":refs,"accepted":accepted,"held":held});
    }
    for entry in &exact {
        row_mut(d, "proposals", required(entry, "id")?)?["status"] = json!("approved");
        let p = &entry["proposal"];
        let origin = p.get("origin").unwrap_or(p);
        feedback::append(d, &json!({"eventId":format!("review:{}:{}",key,required(p,"id")?),
            "draftSessionId":p["draftSessionId"],"_verifiedActor":actor.public_json()}), &entry["item"], origin,
            "review_confirmed", json!({"action":p["kind"],"text":p["text"],"proposalId":p["id"],
                "proposalRevision":p["revision"],"approvalId":key,"recipient":entry["item"]["itemId"]}))?;
    }
    list_mut(d, "approvals").push(approval.clone());
    audit(d, "approval.created", &key);
    if is_partial {
        Ok(json!({"id":key,"status":"approved","admissionMode":"partial","accepted":accepted,"held":held}))
    } else {
        approval.as_object_mut().unwrap().remove("approvalAuthority");
        Ok(approval)
    }
}

#[cfg(test)]
#[path = "approval_admission_tests.rs"]
mod tests;
