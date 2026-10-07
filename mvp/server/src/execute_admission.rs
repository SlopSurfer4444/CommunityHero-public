//! Execution admission is durable before worker dispatch. Replaying a receipt
//! only recovers the original job identity; it never resumes external work.
use crate::*;
use axum::{Extension, body::Bytes};

fn payload(approval_id: &str, body: &Value) -> ApiResult<Value> {
    let fields = body.as_object().ok_or_else(|| bad("Execute admission requires an object"))?;
    if fields.keys().any(|key| !matches!(key.as_str(),"requestId"|"reevaluate")) {
        return Err(bad("Execute admission only accepts requestId and exact reevaluation; approvalId comes from the path"));
    }
    let mut payload = json!({"approvalId":approval_id});
    if let Some(key) = fields.get("requestId") { payload["requestId"] = key.clone(); }
    if let Some(control)=fields.get("reevaluate") {
        if !fields.contains_key("requestId")||control.as_object().is_none_or(|o|o.len()!=2
            ||!o.contains_key("evaluationId")||!o.contains_key("receiptSha256"))
            ||control["evaluationId"].as_str().is_none_or(|s|s.is_empty()||s.len()>160)
            ||control["receiptSha256"].as_str().is_none_or(|s|s.len()!=64||!s.bytes().all(|b|b.is_ascii_hexdigit())) {
            return Err(bad("Reevaluation requires requestId and exact evaluationId/receiptSha256"));
        }
        payload["reevaluate"]=control.clone();
    }
    Ok(payload)
}

pub(crate) async fn post(State(app): State<App>, Extension(actor): Extension<operator_auth::Actor>,
    Path(key): Path<String>, body: Bytes) -> ApiResult<Json<Value>> {
    // Old clients send no body, with or without a JSON content-type header.
    let body = if body.is_empty() { json!({}) } else {
        serde_json::from_slice(&body).map_err(|_| bad("Invalid execute admission JSON"))?
    };
    run(app, actor, key, body).await
}

pub(crate) async fn run(app: App, actor: operator_auth::Actor, key: String, body: Value) -> ApiResult<Json<Value>> {
    let _total = performance::Span::new("execute.admission.total");
    let body = payload(&key, &body)?;
    if let Some(result) = local_admission::replay_committed(&app, "execute", &body, &actor).await? {
        return Ok(Json(result));
    }
    {
        let _refresh = performance::Span::new("execute.admission.media_refresh");
        media_fullframes::refresh(&app).await?;
    }
    app.check_execution()?;
    let parallelism = dispatch_parallelism()?;
    let evaluated = {
        let _admission = performance::Span::new("execute.admission.transaction");
        app.change_admission(storage::AdmissionScope::Execute{approval:&key,body:&body},|d| evaluate(d, &actor, &key, &body)).await?
    };
    // The negative branch was committed as a typed result. Returning Err inside
    // the storage callback would roll it back; arbitrary mutating Err is never
    // converted into no-attempt evidence.
    let (result,scheduled)=match evaluated {
        Evaluation::Admitted(result,scheduled)=>(result,scheduled),
        Evaluation::Rejected(_receipt,reason)=>return Err(conflict(&reason)),
    };
    if let Some((job, operations)) = scheduled {
        let worker = app.clone();
        app.spawn(job, async move {
            // Distinct reviewers share one provider account, without holding
            // the workspace transaction/UI gate during network I/O.
            let _dispatch_guard = worker.execution_gate.lock().await;
            dispatch_wave::run(worker.clone(), operations, parallelism).await
        });
    }
    Ok(Json(result))
}

// The caller commits this entire transition in one storage transaction. None
// means replay, never an empty dispatch that may be spawned or retried.
pub(crate) fn admit(d: &mut Value, actor: &operator_auth::Actor, key: &str, body: &Value)
    -> ApiResult<(Value, Option<(String, Vec<Value>)>)> {
    let request = local_admission::request(d, "execute", body, actor)?;
    if let Some(ref request) = request {
        if let Some(result) = local_admission::replay(d, request, actor)? { return Ok((result, None)); }
    }
    let plan=validate_plan(d,actor,key)?;
    commit_plan(d,key,request.as_ref(),plan)
}

pub(crate) enum Evaluation {
    Admitted(Value,Option<(String,Vec<Value>)>),
    Rejected(Value,String),
}
struct Plan {operations:Vec<Value>,conductor:Option<conductor_authority::Context>}

pub(crate) fn evaluate(d:&mut Value,actor:&operator_auth::Actor,key:&str,body:&Value)->ApiResult<Evaluation> {
    let request=local_admission::request(d,"execute",body,actor)?;
    if let Some(ref request)=request {
        if let Some(result)=local_admission::replay(d,request,actor)? {return Ok(Evaluation::Admitted(result,None));}
        if let Some(receipt)=local_admission::check_reevaluation(d,request,actor,body)? {
            return Ok(Evaluation::Rejected(receipt,"Local admission remains rejected; inspect the exact durable receipt before reevaluation".into()));
        }
    }
    let plan=match validate_plan(d,actor,key) {
        Ok(plan)=>plan,
        Err(error) if matches!(error.0,StatusCode::CONFLICT|StatusCode::FORBIDDEN)=> {
            let Some(ref request)=request else{return Err(error);};
            let dependency=error.1=="External execution/readback must finish before an operator close"
                ||error.1=="Connection dispatch gate is closed, unverified or changed";
            let jobs=if dependency {
                let rows=if d.get("scopeOwners").is_some(){&d["activeExternalJobs"]["jobs"]}else{&d["jobs"]};
                rows.as_array().into_iter().flatten().filter(|job|
                    matches!(job["kind"].as_str(),Some("execute"|"reconcile"))
                        &&matches!(job["status"].as_str(),Some("queued"|"running"|"pending")))
                    .map(|job|job["id"].clone()).collect()
            }else{vec![]};
            let receipt=local_admission::reject_execute(d,request,actor,key,
                if dependency{"dependency"}else{"precondition_changed"},jobs)?;
            return Ok(Evaluation::Rejected(receipt,error.1));
        }
        Err(error)=>return Err(error),
    };
    // Any error after validation rolls back the entire mutation transaction.
    // It is not caught and cannot create a false negative after partial writes.
    let (result,scheduled)=commit_plan(d,key,request.as_ref(),plan)?;
    Ok(Evaluation::Admitted(result,scheduled))
}

/// Immutable prevalidation: no job/operation insert, approval consumption or
/// provider dispatch. The plan commits under this SAME writer snapshot.
fn validate_plan(d:&Value,actor:&operator_auth::Actor,key:&str)->ApiResult<Plan> {
    let approval = row(d, "approvals", key)?.clone();
    let targets:Vec<Value>=approval["proposals"].as_array().into_iter().flatten().map(|entry|entry["item"].clone()).collect();
    let conductor=conductor_authority::fence_admission(d,"execute",&targets)?;
    conductor_authority::fence_actor(conductor.as_ref(),actor)?;
    conductor_authority::require_prior_attribution(conductor.as_ref(),&approval)?;
    check_approval_actor(&approval, actor)?;
    let authority = dispatch_authority::admit(&approval, actor)?;
    if approval["status"] != "approved" {
        return Err(conflict("Approval already consumed; reconcile unresolved operations"));
    }
    connection_gate::fence_admission(d,&targets)?;
    let mut operations = vec![];
    for r in approval["proposals"].as_array().ok_or_else(|| internal("Invalid approval proposals"))? {
        let p = row(d, "proposals", required(r, "id")?)?;
        check_revision(p, &r["revision"])?;
        if p["status"] != "approved" { return Err(conflict("Approval invalidated")); }
        operator_close::assert_preparation(d,p)?;
        // The approval captures the exact editorial receipt that its operator
        // saw. A later source review needs a new approval, even for the same
        // proposal revision and reply text.
        if r["proposal"]["editorialReview"] != p["editorialReview"] {
            return Err(conflict("Editorial review changed after approval"));
        }
        if r["proposal"]["operatorCloseDecision"] != p["operatorCloseDecision"] {
            return Err(conflict("Operator close decision changed after approval"));
        }
        if r["proposal"]["mediaContextWaiver"] != p["mediaContextWaiver"] {
            return Err(conflict("Media context exception changed after approval"));
        }
        if r["approvedPhotoAcquisitionProof"]!=media_context_gate::photo_proof(&prepare_bundle::EvidenceContext::new(d),p)?{
            return Err(conflict("Photo acquisition proof changed after approval"));
        }
        if r["proposal"][retained_paid_recovery::FIELD] != p[retained_paid_recovery::FIELD] {
            return Err(conflict("Retained recovery proof changed after approval"));
        }
        retained_paid_recovery::assert_actor(p,actor)?;
        operator_close::assert_actor(p,actor)?;
        let item = proposal_current(d, p)?;
        if conductor.is_some() {
            conductor_authority::fence_admission(d,required(p,"kind")?,&[item.clone()])?;
        }
        if approval["editorialPolicyVersion"]==1 && p["kind"]=="reply_and_close" {
            editorial_review::require_current(&prepare_bundle::EvidenceContext::new(d),p).map_err(conflict)?;
        }
        if list(d, "operations").iter().any(|o| recipient_operation_blocks_current(d, o, p, &item)) {
            return Err(conflict("Recipient already has an unresolved or completed operation"));
        }
        let op = id();
        let action = action_for(p, &item, &op)?;
        let mut operation = json!({"id":op,"approvalId":key,"proposalId":p["id"],"itemId":p["itemId"],
            "action":action,"target":item,"status":"dispatching","attemptId":id(),"createdAt":now(),
            "approvedBy":approval["approvedBy"],"executedBy":actor.public_json(),"dispatchAuthority":authority,
            "editorialPolicyVersion":approval["editorialPolicyVersion"],
            "approvedEditorialReceiptSha256":r["proposal"]["editorialReview"]["receiptSha256"]});
        if let Some(waiver)=p.get("mediaContextWaiver") {
            operation["approvedMediaContextWaiver"]=waiver.clone();
        }
        if let Some(proof)=r.get("approvedPhotoAcquisitionProof"){
            operation["approvedPhotoAcquisitionProof"]=proof.clone();
        }
        if let Some(marker) = p["operatorCloseDecision"]["decisionSha256"].as_str() {
            operation["approvedOperatorCloseDecisionSha256"] = json!(marker);
        }
        if let Some(marker)=p[retained_paid_recovery::FIELD]["proofSha256"].as_str() {
            operation["approvedRetainedPaidRecoverySha256"]=json!(marker);
        }
        if let Some(ctx)=conductor.as_ref(){conductor_authority::tag(ctx,&mut operation);}
        operations.push(operation);
    }
    Ok(Plan{operations,conductor})
}

fn commit_plan(d:&mut Value,key:&str,request:Option<&local_admission::Request>,plan:Plan)
    ->ApiResult<(Value,Option<(String,Vec<Value>)>)> {
    let Plan{operations,conductor}=plan;
    let job = new_job(d, "execute", key)?;
    if let Some(ctx)=conductor.as_ref(){conductor_authority::tag(ctx,row_mut(d,"jobs",&job)?);}
    row_mut(d, "approvals", key)?["status"] = json!("consumed");
    for op in &operations {
        row_mut(d, "proposals", required(op, "proposalId")?)?["status"] = json!("dispatching");
        list_mut(d, "operations").push(op.clone());
        audit(d, "operation.admitted", required(op, "id")?);
    }
    let mut result = json!({"jobId":job});
    if let Some(request) = request {
        result["approvalId"] = json!(key);
        local_admission::commit(d, request, &mut result)?;
    }
    Ok((result, Some((job, operations))))
}

#[cfg(test)]
#[path = "execute_admission_tests.rs"]
mod tests;
