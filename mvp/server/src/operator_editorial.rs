//! Explicit review assisted by an operator's delegate. This is a native,
//! authenticated semantic-review receipt, never a claimed model/human verdict.
use crate::*;
use axum::Extension;
use std::collections::BTreeSet;

pub(crate) const CONTRACT:&str="communityhero-operator-assisted-editorial-v1";
pub(crate) const METHOD:&str="assistant_on_operator_authority";

pub(crate) fn fields(value:&Value,names:&[&str])->ApiResult<()> {
    if value.as_object().is_none_or(|o|o.len()!=names.len()||names.iter().any(|k|!o.contains_key(*k))) {
        return Err(bad("Invalid operator-assisted review fields"));
    }
    Ok(())
}
pub(crate) fn authority(actor:&operator_auth::Actor)->ApiResult<()> {
    if actor.role!="owner" {return Err(ApiError(StatusCode::FORBIDDEN,"Explicit operator-assisted review requires owner authority".into()));}
    dispatch_authority::admit(&json!({"approvedBy":actor.public_json(),"approvalAuthority":dispatch_authority::approval_binding(actor)}),actor)?;
    Ok(())
}
pub(crate) fn refs(body:&Value)->ApiResult<&Vec<Value>> {
    body["proposals"].as_array().filter(|r|!r.is_empty()&&r.len()<=100).ok_or_else(||bad("Choose 1 to 100 exact draft replies"))
}
pub(crate) fn pending(d:&Value,p:&Value)->bool {
    // Terminal failed/completed reviews cannot admit a late result: the existing
    // worker checks status==running before capture, receipt and finalization.
    // Their immutable journals are history, not a new permanent semantic hold.
    list(d,"jobs").iter().any(|j| j["kind"]=="editorial_review"&&matches!(j["status"].as_str(),Some("running"|"queued"))&&(
        j["editorialReferences"].as_array().into_iter().flatten().any(|r|r["id"]==p["id"])
        ||j["editorialPlan"]["batches"].as_array().into_iter().flatten().any(|b|
            b["request"]["editorialCandidates"].as_array().into_iter().flatten().any(|c|c["itemId"]==p["itemId"]))))
}

// Keep full source evidence visible and journaled. Only the versioned digest
// projection omits acquisition observations; required judgments retain their
// separate exact acquisition digest at candidate admission and revalidation.
pub(crate) fn preview_digest(preview:&Value)->String{
    let mut value=preview.clone();
    if let Some(fields)=value.as_object_mut(){fields.remove("previewDigest");}
    for entry in value["entries"].as_array_mut().into_iter().flatten(){
        if entry["candidate"]["operatorEvidenceContract"]!=editorial_review::OPERATOR_EVIDENCE_CONTRACT{continue;}
        if let Some(fields)=entry["candidate"].as_object_mut(){fields.remove("acquisitionMediaDigest");}
        for post in entry["evidence"]["posts"].as_array_mut().into_iter().flatten(){
            if let Some(fields)=post.as_object_mut(){fields.remove("mediaPolicy");}
        }
    }
    editorial_review::hash_text(&value.to_string())
}
// Every entry path, including the frontier, carries the same native mandatory
// material observation before preview byte limits and candidate admission.
pub(crate) fn entry_with_context(context:&prepare_bundle::EvidenceContext<'_>,p:&Value)->Result<Value,&'static str>{
    let (candidate,mut evidence)=editorial_review::operator_candidate_with_context(context,p)?;
    if p["kind"]=="reply_and_close"&&!p["modelMaterialReceipt"].is_object()&&!p["editorialModelMaterialReceipt"].is_object(){
        evidence["operatorMandatoryMaterials"]=preparation_materials::operator_request(context,p)?["postContextBundle"].clone();
    }
    Ok(json!({"candidate":candidate,"evidence":evidence}))
}
pub(crate) fn capture(d:&Value,actor:&operator_auth::Actor,body:&Value)->ApiResult<Value> {
    authority(actor)?;
    let context=prepare_bundle::EvidenceContext::new(d);
    let mut seen=BTreeSet::new();let mut recipients=BTreeSet::new();let mut entries=vec![];
    for reference in refs(body)? {
        fields(reference,&["id","revision"])?;
        let key=required(reference,"id")?;
        if !seen.insert(key.to_owned()){return Err(bad("Duplicate operator review reference"));}
        let p=row(d,"proposals",key)?;check_revision(p,&reference["revision"])?;
        if !recipients.insert(required(p,"itemId")?.to_owned()){return Err(bad("One operator review per recipient required"));}
        if pending(d,p){return Err(conflict("Recover the existing editorial review before operator-assisted review"));}
        entries.push(entry_with_context(&context,p).map_err(conflict)?);
    }
    preview_from_entries(d,actor,&body["proposals"],entries)
}

pub(crate) const MAX_PREVIEW_BYTES:usize=8*1024*1024;

fn preview_envelope(d:&Value,actor:&operator_auth::Actor,references:&Value)->Value {
    json!({"version":1,"contract":CONTRACT,"account":d["account"],"connectorBinding":d["connectorBinding"],
        "reviewedBy":actor.public_json(),"reviewAuthorityDigest":editorial_review::hash_text(&dispatch_authority::approval_binding(actor).to_string()),"method":METHOD,
        "proposals":references,"entries":[]})
}

// Inserting one entry into [] adds exactly the entry's serialized byte count.
// Measure the SAME native envelope without cloning a large evidence member.
pub(crate) fn entry_fits_preview(d:&Value,actor:&operator_auth::Actor,reference:&Value,entry:&Value)->bool {
    preview_envelope(d,actor,&json!([reference])).to_string().len()+entry.to_string().len()<=MAX_PREVIEW_BYTES
}

// Shared assembly keeps the frontier subset byte-for-byte on the ordinary
// native review contract. This helper does not perform candidate admission.
pub(crate) fn preview_from_entries(d:&Value,actor:&operator_auth::Actor,references:&Value,entries:Vec<Value>)->ApiResult<Value> {
    let mut preview=preview_envelope(d,actor,references);
    preview["entries"]=Value::Array(entries);
    if preview.to_string().len()>MAX_PREVIEW_BYTES{return Err(bad("Operator review evidence exceeds 8 MiB; select fewer replies"));}
    preview["previewDigest"]=json!(preview_digest(&preview));
    Ok(preview)
}

pub(crate) fn admit(d:&mut Value,actor:&operator_auth::Actor,body:&Value)->ApiResult<Value> {
    // Also atomic for pure reducer callers: no partially appended review history.
    let mut next={let _clone=performance::Span::new("operator.editorial.atomic_clone");d.clone()};
    let result=admit_inner(&mut next,actor,body)?;*d=next;Ok(result)
}
fn input(body:&Value)->ApiResult<()> {
    fields(body,&["requestId","proposals","previewDigest","operatorReview"])?;
    fields(&body["operatorReview"],&["version","method","entries"])?;
    if body["operatorReview"]["version"]!=1||body["operatorReview"]["method"]!=METHOD{return Err(bad("Explicit operator-assisted review method required"));}
    Ok(())
}
fn admit_inner(d:&mut Value,actor:&operator_auth::Actor,body:&Value)->ApiResult<Value> {
    input(body)?;authority(actor)?;
    let request=local_admission::request(d,"editorial",body,actor)?.ok_or_else(||bad("Operator review requires requestId"))?;
    if let Some(result)=local_admission::replay(d,&request,actor)?{return Ok(result);}
    let preview=capture(d,actor,body)?;
    if body["previewDigest"]!=preview["previewDigest"]{return Err(conflict("Operator review preview changed; review current rules and evidence again"));}
    let entries=body["operatorReview"]["entries"].as_array().ok_or_else(||bad("Operator review judgments required"))?;
    let captured=preview["entries"].as_array().unwrap();
    if entries.len()!=captured.len(){return Err(bad("Operator review coverage differs from preview"));}
    let at=now();let mut saved=vec![];
    for (entry,selected) in entries.iter().zip(captured) {
        let mut names=vec!["candidate","checks","reason"];
        if entry.get("mediaDependency").is_some(){names.push("mediaDependency");}
        fields(entry,&names)?;
        if !editorial_review::operator_candidates_equal(&entry["candidate"],&selected["candidate"],&entry["mediaDependency"]){return Err(conflict("Operator review exact candidate differs from preview"));}
        let c=&selected["candidate"];
        let mut judgment=json!({"proposalId":c["proposalId"],"proposalRevision":c["proposalRevision"],"itemId":c["itemId"],
            "textSha256":c["textSha256"],"contextDigest":c["contextDigest"],"rulesDigest":c["rulesDigest"],
            "decision":"accept","reason":entry["reason"],"proposedText":null,"checks":entry["checks"]});
        if let Some(needs)=entry.get("mediaDependency"){judgment["mediaDependency"]=needs.clone();}
        let source=json!({"kind":"operator_assisted_review","version":1,"contract":CONTRACT,"method":METHOD,
            "reviewedBy":actor.public_json(),"reviewAuthorityDigest":preview["reviewAuthorityDigest"],
            "requestId":body["requestId"],"previewDigest":preview["previewDigest"],
            "researchManifest":selected["evidence"]["editorialResearchManifest"]});
        let receipt=editorial_review::store_operator_receipt(d,c,&judgment,&source,&at).map_err(bad)?;
        let proposal_id=required(c,"proposalId")?;
        let p=row(d,"proposals",proposal_id)?;
        if p["kind"]=="reply_and_close"&&!p["modelMaterialReceipt"].is_object()&&!p["editorialModelMaterialReceipt"].is_object(){
            let materials=preparation_materials::operator_receipt(&prepare_bundle::EvidenceContext::new(d),p,&preview,c,&receipt).map_err(conflict)?;
            row_mut(d,"proposals",proposal_id)?["operatorMaterialReceipt"]=materials;
        }
        saved.push(receipt["receiptSha256"].clone());
    }
    // Ordinary proposal, capability, source-fact, media and route guards remain
    // ultimate authority after the honestly dedicated current review is stored.
    for reference in refs(body)? {proposal_current(d,row(d,"proposals",required(reference,"id")?)?)?;}
    let job=new_job(d,"editorial_review",required(body,"requestId")?)?;
    let outcome=json!({"accepted":body["proposals"],"reused":[],"held":[]});
    let stored=row_mut(d,"jobs",&job)?;
    stored["purpose"]=json!("operator_assisted_review");stored["operatorId"]=json!(actor.id);
    stored["editorialReferences"]=body["proposals"].clone();stored["operatorReviewPreview"]=preview.clone();
    stored["editorialOutcome"]=outcome.clone();stored["result"]=outcome;stored["status"]=json!("completed");stored["finishedAt"]=json!(at);
    list_mut(d,"audit").push(json!({"id":id(),"action":"operator_editorial.reviewed","refId":job,"at":at,
        "actor":actor.public_json(),"method":METHOD,"previewDigest":preview["previewDigest"],"receiptSha256":saved}));
    let mut result=json!({"jobId":job,"accepted":body["proposals"],"held":[]});
    local_admission::commit(d,&request,&mut result)?;Ok(result)
}

pub(crate) async fn preview(State(app):State<App>,Extension(actor):Extension<operator_auth::Actor>,Json(body):Json<Value>)->ApiResult<Json<Value>> {
    fields(&body,&["proposals"])?;authority(&actor)?;refs(&body)?;
    let d=app.db.read_operator_editorial(&body).await?;
    capture(&d,&actor,&body).map(Json)
}
pub(crate) async fn post(State(app):State<App>,Extension(actor):Extension<operator_auth::Actor>,Json(body):Json<Value>)->ApiResult<Json<Value>> {
    input(&body)?;authority(&actor)?;
    if let Some(result)=local_admission::replay_committed(&app,"editorial",&body,&actor).await?{return Ok(Json(result));}
    // The storage transaction already supplies an isolated mutable candidate
    // and commits it only after all domain/storage checks pass. Avoid cloning
    // that complete evidence/history projection again for pure-call atomicity.
    app.change_admission(storage::AdmissionScope::OperatorEditorial(&body),|d|admit_inner(d,&actor,&body)).await.map(Json)
}

#[cfg(test)]
#[path="operator_editorial_tests.rs"]
pub(crate) mod tests;
