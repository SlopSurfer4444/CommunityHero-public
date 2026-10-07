//! Read-only eligibility for exact operator review, never a semantic judgment
//! or publication permission. One immutable capture supplies every member.
use crate::*;
use axum::Extension;
use std::collections::BTreeSet;

pub(crate) const CONTRACT:&str="communityhero-operator-review-frontier-v1";

fn input(actor:&operator_auth::Actor,body:&Value)->ApiResult<()> {
    operator_editorial::fields(body,&["proposals"])?;
    operator_editorial::authority(actor)?;
    operator_editorial::refs(body)?;
    if body.to_string().len()>2*1024*1024{return Err(bad("Operator review frontier input exceeds 2 MiB"));}
    Ok(())
}

pub(crate) fn capture(d:&Value,actor:&operator_auth::Actor,body:&Value)->ApiResult<Value> {
    input(actor,body)?;
    // A legacy fallback binding is not an explicit source binding. Do not
    // fabricate one in either the envelope or native preview/digest.
    let explicit=ConnectorBinding::from_json(&d["connectorBinding"])
        .map_err(|_|conflict("Operator review frontier requires an explicit valid company binding"))?;
    let binding=active_binding(d)?;
    if explicit!=binding{return Err(conflict("Operator review frontier company binding differs from active company"));}
    let references=operator_editorial::refs(body)?;
    let mut seen=BTreeSet::new();let mut recipients=BTreeSet::new();
    // Validate the COMPLETE exact selection before considering readiness. An
    // earlier media hold must not conceal a later foreign, stale or duplicate
    // reference, nor produce a preview of an invalid requested selection.
    for reference in references {
        operator_editorial::fields(reference,&["id","revision"])?;
        let key=required(reference,"id")?;
        if !seen.insert(key.to_owned()){return Err(bad("Duplicate operator review reference"));}
        let p=row(d,"proposals",key)?;check_revision(p,&reference["revision"])?;
        if !recipients.insert(required(p,"itemId")?.to_owned()){return Err(bad("One operator review per recipient required"));}
    }
    let context=prepare_bundle::EvidenceContext::new(d);
    let mut ready=Vec::new();let mut held=Vec::new();let mut entries=Vec::new();
    for reference in references {
        let p=row(d,"proposals",required(reference,"id")?)?;
        if operator_editorial::pending(d,p) {
            held.push(json!({"reference":reference,"stage":"pending_editorial_review",
                "reason":"Recover the existing editorial review before operator-assisted review"}));
            continue;
        }
        match operator_editorial::entry_with_context(&context,p) {
            Ok(entry)=>{
                // Native candidate validation and mandatory material evidence
                // are shared with direct review, before either byte budget.
                // Check the exact singleton native envelope without cloning its
                // evidence. Aggregate limits retain the same native guard:
                // there is never an implicit truncated preview/selection.
                if !operator_editorial::entry_fits_preview(d,actor,reference,&entry) {
                    held.push(json!({"reference":reference,"stage":"evidence_budget",
                        "reason":"Operator review evidence exceeds 8 MiB; select fewer replies"}));
                } else {ready.push(reference.clone());entries.push(entry);}
            },
            Err(reason)=>held.push(json!({"reference":reference,"stage":"operator_candidate","reason":reason})),
        }
    }
    let ready=json!(ready);
    let preview=if entries.is_empty(){Value::Null}else{operator_editorial::preview_from_entries(d,actor,&ready,entries)?};
    Ok(json!({"version":1,"contract":CONTRACT,"account":d["account"],"connectorBinding":d["connectorBinding"],"requested":body["proposals"],
        "readyForOperatorReview":ready,"held":held,"preview":preview}))
}

pub(crate) async fn preview(State(app):State<App>,Extension(actor):Extension<operator_auth::Actor>,Json(body):Json<Value>)->ApiResult<Json<Value>> {
    input(&actor,&body)?;
    crate::media_fullframes::refresh(&app).await?;
    // One existing storage capture; no per-member read, acquisition, model,
    // admission, reservation, receipt, history or external-operation mutation.
    let d=app.db.read_operator_editorial(&body).await?;
    capture(&d,&actor,&body).map(Json)
}

#[cfg(test)]
#[path="operator_frontier_tests.rs"]
mod tests;
