//! Publication floor with exact current decision-dependency evidence.
//! Only a named, explicitly enabled website operator can waive one exact case.
use crate::*;
use sha2::{Digest, Sha256};

fn hash(value:&Value)->String {format!("{:x}",Sha256::digest(value.to_string().as_bytes()))}
fn rows<'a>(v:&'a Value,key:&str)->&'a [Value]{v[key].as_array().map(Vec::as_slice).unwrap_or(&[])}
fn text<'a>(v:&'a Value,key:&str)->&'a str{v[key].as_str().unwrap_or("")}
fn applies(p:&Value)->bool{matches!(text(p,"kind"),"reply_and_close"|"close")}
fn image(a:&Value)->bool{matches!(text(a,"type"),"photo"|"image"|"sticker")||text(a,"mime").starts_with("image/")}
fn audio(a:&Value)->bool{matches!(text(a,"type"),"audio"|"voice")||text(a,"mime").starts_with("audio/")}
fn video(a:&Value)->bool{matches!(text(a,"type"),"video"|"reel"|"clip")||text(a,"mime").starts_with("video/")}
fn metadata_unknown(v:&Value,attachments:&[Value])->bool {
    v.get("attachments").is_some_and(|a|!a.is_array())
        ||v.get("commentAttachments").is_some_and(|a|!a.is_array())
        ||v["commentAttachmentsPresent"]==true&&attachments.is_empty()
        ||match v.get("attachmentsState") {
            None=>false,
            Some(state) if state=="none"=>!attachments.is_empty(),
            Some(state) if state=="present"=>attachments.is_empty(),
            _=>true,
        }
}
fn add_unknown_metadata(requirements:&mut Vec<Value>,v:&Value,attachments:&[Value]) {
    if metadata_unknown(v,attachments){requirements.push(json!({"kind":"unknown_media","sourceId":hash(&json!({"id":v["id"],"attachmentsState":v["attachmentsState"],"commentAttachmentsPresent":v["commentAttachmentsPresent"]})),
        "ready":false,"reason":"attachment_metadata_unavailable"}));}
}
fn strict(d:&Value)->ApiResult<()> {
    // Missing old configuration upgrades to strict. There is deliberately no
    // global automation bypass: a malformed/false switch fails closed.
    match d["settings"].get("mediaContextStrict") {
        None|Some(Value::Bool(true))=>Ok(()),
        _=>Err(conflict("Strict media context configuration required")),
    }
}
fn usable_image(v:&Value)->bool {
    v["sha256"].as_str().is_some_and(|s|s.len()==64&&s.bytes().all(|b|b.is_ascii_hexdigit()))
        && matches!(text(v,"mime"),"image/png"|"image/jpeg"|"image/webp")
        && v["width"].as_u64().is_some_and(|n|n>0&&n<=12000)
        && v["height"].as_u64().is_some_and(|n|n>0&&n<=12000)
}
fn image_records<'a>(context:&prepare_bundle::EvidenceContext<'_>,p:&'a Value)->Vec<&'a Value>{
    let mut records=Vec::new();
    // Only server-admitted metadata whose immutable bundle is still current.
    if let Some(run)=p["prepareRunId"].as_str() {
        if let Ok(job)=row(context.workspace(),"jobs",run) {
            if p["prepareBundleDigest"]==job["prepareBundle"]["digest"]
                && context.current(&job["prepareBundle"]).is_ok()
                && prepare_bundle::validate_image_evidence_binding(&p["generationMetadata"],&job["prepareBundle"]).is_ok(){
                records.push(&p["generationMetadata"]);
            }
        }
    }
    // Acquired image bytes and failures are observations, not semantic approval.
    // A current HOLD may contain two real images and a failed third image; all
    // three observations are needed for an honest case-specific exception.
    if let Ok(metadata)=editorial_review::current_acquisition_metadata(context,p){records.push(metadata);}
    records
}
fn image_for(v:&Value,item:&Value,post:Option<&Value>,index:usize)->bool {
    if v["attachmentIndex"].as_u64()!=Some(index as u64){return false;}
    match post {
        Some(post)=>v["origin"]=="post_attachment"&&v["postId"]==post["id"]
            && (v["itemId"]==item["id"]||rows(v,"itemIds").contains(&item["id"])),
        None=>v["origin"]=="comment_attachment"&&v["itemId"]==item["id"],
    }
}
fn add_images(requirements:&mut Vec<Value>,attempts:&mut Vec<Value>,metadata:&[&Value],item:&Value,post:Option<&Value>,attachments:&[Value],native:Option<&Value>) {
    for (index,a) in attachments.iter().enumerate().filter(|(_,a)|image(a)) {
        let source=json!({"origin":if post.is_some(){"post_attachment"}else{"comment_attachment"},
            "id":post.unwrap_or(item)["id"],"attachmentIndex":index,"attachment":a});
        let ready=metadata.iter().any(|m|rows(m,"imageEvidence").iter().any(|v|image_for(v,item,post,index)&&usable_image(v)));
        requirements.push(json!({"kind":"image","sourceId":hash(&source),"ready":ready,"reason":if ready{"image_bytes_observed"}else{"image_evidence_missing"}}));
        // Only current_metadata can emit a native acquisition digest here. Bind
        // both receipt and exact retained pixels into the approval context.
        if let Some(v)=native.into_iter().flat_map(|m|rows(m,"imageEvidence")).find(|v|image_for(v,item,post,index)&&usable_image(v)){
            let requirement=requirements.last_mut().unwrap();
            requirement["acquisitionReceiptSha256"]=v["acquisitionReceiptSha256"].clone();requirement["artifact"]=v["artifact"].clone();
        }
        for failure in metadata.iter().flat_map(|m|rows(m,"imageFailures")).filter(|v|image_for(v,item,post,index)&&matches!(text(v,"stage"),"acquisition"|"validation")) {
            attempts.push(json!({"sourceId":hash(&source),"status":"failed","phase":failure["stage"],"failureClass":failure["category"]}));
        }
    }
}
fn current_attempts(d:&Value,post:&Value,source_version:&str)->Vec<Value>{
    let binding=active_binding(d).ok().map(|b|b.to_json()).unwrap_or(Value::Null);
    let mut attempts:Vec<_>=rows(d,"jobs").iter().filter(|j|matches!(text(j,"kind"),"media"|"media_audio")
        &&j["account"]==d["account"]&&j["connectorBinding"]==binding)
        .flat_map(|j|rows(j,"sourceAttempts").iter().filter(move|a|a["postId"]==post["id"]&&a["sourceVersion"]==source_version)
            .map(move|a|json!({"jobId":j["id"],"attemptId":a["id"],"sourceId":source_version,"status":a["status"],"phase":a["phase"],"failureClass":a["error"]})))
        .collect();
    // Cached audio is an existing separate job whose immutable audioPin is
    // the source proof; it never creates a sourceAttempts entry. Admit only a
    // terminal failed job for this exact current company/source, not a running
    // request or a failure from an earlier source checkpoint.
    for job in rows(d,"jobs") {
        let pin=&job["audioPin"];let progress=&pin["progress"];let policy=&pin["policy"];
        if job["kind"]!="media_audio"||job["status"]!="failed"||job["refId"]!=post["id"]
            ||job["account"]!=d["account"]||job["connectorBinding"]!=binding
            ||progress["account"]!=d["account"]||progress["connectorBinding"]!=binding
            ||progress["sourcePostId"]!=post["id"]||progress["sourcePostKey"]!=post["postKey"]
            ||progress["sourceVersion"]!=source_version||policy["sourceVersion"]!=source_version
            ||policy["account"]!=d["account"]||policy["connectorBinding"]!=binding
            ||progress["sourceIdentity"]["account"]!=d["account"]||progress["sourceIdentity"]["postKey"]!=post["postKey"]
            ||progress["sourceIdentity"]["mediaSha256"]!=progress["source"]["sha256"]
            ||progress["source"]["sha256"].as_str().is_none_or(|s|s.len()!=64||!s.bytes().all(|b|b.is_ascii_hexdigit())){continue;}
        attempts.push(json!({"jobId":job["id"],"attemptId":job["id"],"sourceId":source_version,
            "status":"failed","phase":"cached_audio","failureClass":job["error"],"acquisitionPinSha256":hash(pin)}));
    }
    attempts
}
fn binding(d:&Value,p:&Value,item:&Value,posts:&[Value],requirements:&[Value])->Value {
    json!({"version":1,"account":d["account"],"connectorBinding":d["connectorBinding"],
        "proposalId":p["id"],"proposalRevision":p["revision"],"kind":p["kind"],"textSha256":hash(&p["text"]),"routeTarget":p["routeTarget"],
        "itemId":item["id"],"itemRevision":item["revision"],"contextEvidenceDigest":item["contextEvidenceDigest"],
        "branchContextDigest":item["branchContextDigest"],"attachments":item["attachments"],"posts":posts,
        "requirements":requirements})
}
fn valid_waiver(p:&Value,digest:&str)->bool {
    let w=&p["mediaContextWaiver"];
    let mut unsigned=w.clone();if let Some(m)=unsigned.as_object_mut(){m.remove("waiverSha256");}
    w["version"]==1&&w["contextDigest"]==digest&&w["proposalId"]==p["id"]&&w["proposalRevision"]==p["revision"]
        && w["actor"]["id"].as_str().is_some_and(|id|!id.is_empty()&&id!="local-owner")
        && w["authorityGeneration"].as_str().is_some_and(|g|g.len()==64)
        && w["waiverSha256"]==hash(&unsigned)
}
// The owner's 2026-10-03 policy permits an exact reviewed decision independent
// of video audio AND visuals to proceed without pretending the asset is ready.
// Proposal fields alone, old receipts and owner CLOSE intent are not this proof.
fn independent_video_receipt<'a>(context:&prepare_bundle::EvidenceContext<'_>,p:&'a Value)->Option<&'a Value>{
    if !decision_media::enabled(p)
        ||p["editorialReview"]["mediaDependency"]["audio"]!="independent"
        ||p["editorialReview"]["mediaDependency"]["visual"]!="independent"
        ||editorial_review::require_current(context,p).is_err(){return None;}
    Some(&p["editorialReview"])
}
pub(crate) fn inspect(context:&prepare_bundle::EvidenceContext<'_>,p:&Value)->ApiResult<Value>{
    let d=context.workspace();if applies(p){strict(d)?;}
    let item=row(d,"items",required(p,"itemId")?)?;
    let mut requirements=Vec::new();let mut attempts=Vec::new();let mut sources=Vec::new();
    if applies(p){
        let independent=independent_video_receipt(context,p);
        let metadata=image_records(context,p);
        let attachments=if item["attachments"].is_array(){rows(item,"attachments")}else{rows(item,"commentAttachments")};
        add_unknown_metadata(&mut requirements,item,attachments);
        add_images(&mut requirements,&mut attempts,&metadata,item,None,attachments,None);
        for post in rows(d,"posts").iter().filter(|post|post["id"]==item["postId"]
            ||item["postKey"].is_string()&&post["postKey"]==item["postKey"]) {
            let source_version=media_fullframes::source_version(post,required(d,"account")?);
            sources.push(json!({"id":post["id"],"sourceVersion":source_version}));
            add_unknown_metadata(&mut requirements,post,rows(post,"attachments"));
            let mut acquired=photo_acquisition::current_metadata(d,post).unwrap_or(Value::Null);
            // A source-owned observation can serve the currently selected
            // recipient; it never rewrites historical model recipient lists.
            if !acquired.is_null(){
                if let Some(images)=acquired["imageEvidence"].as_array_mut(){for image in images{image["itemId"]=item["id"].clone();}}
                if let Some(failures)=acquired["imageFailures"].as_array_mut(){for failure in failures{failure["itemId"]=item["id"].clone();}}
            }
            let mut post_metadata=metadata.clone();if !acquired.is_null(){post_metadata.push(&acquired);}
            add_images(&mut requirements,&mut attempts,&post_metadata,item,Some(post),rows(post,"attachments"),if acquired.is_null(){None}else{Some(&acquired)});
            let videos=rows(post,"attachments").iter().filter(|a|video(a)).count();
            let audio_assets=rows(post,"attachments").iter().filter(|a|audio(a)).count();
            let has_video=knowledge::is_video_post(post);
            let has_audio=rows(post,"attachments").iter().any(audio);
            if has_video||has_audio {
                // The receipt must capture THIS current video. A standalone
                // audio attachment, sibling or unrelated post cannot inherit it.
                if let Some(receipt)=independent.filter(|r|has_video&&audio_assets==0&&videos<=1&&rows(&r["candidate"],"decisionMediaEvidence").iter()
                    .any(|s|s["postId"]==post["id"]&&s["sourceVersion"]==source_version)){
                    requirements.push(json!({"kind":"media_context","sourceId":source_version,"ready":true,
                        "reason":"exact_review_media_independent","dependencyReceiptSha256":receipt["receiptSha256"]}));
                }else{
                    let evidence=context.strict_media_evidence(post).map_err(conflict)?;
                    let material_body=crate::preparation_materials::proposal_receipt(context,p).ok().map(|r|&r["body"])
                        .or_else(||(crate::preparation_materials::require_proposal(context,p).is_ok()&&p["operatorMaterialReceipt"].is_object()).then_some(&p["operatorMaterialReceipt"]["body"]));
                    let mandatory=material_body.is_some()
                        &&audio_assets==0&&videos>0&&rows(material_body.unwrap_or(&Value::Null),"suppliedSpeech").iter()
                            .filter(|s|s["postId"]==post["id"]&&s["sourceVersion"]==source_version
                                &&matches!(text(s,"outcome"),"transcript"|"no_speech"|"no_audio")).count()==videos;
                    let ready=mandatory||videos+audio_assets<=1&&evidence["audioReady"]==true
                        && (evidence["audioHasContent"]==true||has_video&&(evidence["screenTextHasContent"]==true||evidence["visualReady"]==true));
                    requirements.push(json!({"kind":"media_context","sourceId":source_version,"ready":ready,
                        "reason":if videos+audio_assets>1{"multi_asset_audio_coverage_unproven"}else if ready{"complete_source_audio_observed"}else{"complete_source_audio_missing"}}));
                }
                // Full ASR is legitimate general context. Optional unavailable
                // OCR is retained honestly; existing exact semantic dependencies
                // still block decisions requiring unseen screen/visual evidence.
                attempts.extend(current_attempts(d,post,&source_version));
            }
            for a in rows(post,"attachments").iter().filter(|a|!image(a)&&!audio(a)&&!video(a)) {
                requirements.push(json!({"kind":"unknown_media","sourceId":hash(a),"ready":false,"reason":"attachment_modality_unverified"}));
            }
        }
        for a in attachments.iter().filter(|a|!image(a)) {
            // Comment-level audio/video currently lacks an independently bound
            // canonical transcript; a post transcript cannot stand in for it.
            requirements.push(json!({"kind":"comment_media","sourceId":hash(a),"ready":false,"reason":"comment_media_context_unproven"}));
        }
    }
    sources.sort_by_key(Value::to_string);requirements.sort_by_key(Value::to_string);attempts.sort_by_key(Value::to_string);attempts.dedup();
    // Failed acquisition evidence is archived in the signed exception itself.
    // Worker/history counters are not decision context and compact dispatch
    // views intentionally omit them. Source/requirements remain live pins.
    let digest=hash(&binding(d,p,item,&sources,&requirements));
    let missing=requirements.iter().any(|r|r["ready"]!=true);
    let waived=missing&&valid_waiver(p,&digest);
    Ok(json!({"version":1,"strict":true,"proposalId":p["id"],"proposalRevision":p["revision"],"contextDigest":digest,
        "status":if requirements.is_empty(){"not-required"}else if !missing{"ready"}else if waived{"waived"}else{"missing"},
        "requirements":requirements,"attempts":attempts,"waiver":if waived{p["mediaContextWaiver"].clone()}else{Value::Null}}))
}
pub(crate) fn require(context:&prepare_bundle::EvidenceContext<'_>,p:&Value,op:Option<&Value>)->ApiResult<()> {
    // Recovery observes the original dispatched operation. New policy cannot
    // authorize a resend or force reauthoring while settling historical truth.
    if op.is_some_and(|op|matches!(text(op,"status"),"dispatching"|"unknown"|"succeeded")){return Ok(());}
    if p["kind"]=="reply_and_close"{crate::preparation_materials::require_proposal(context,p).map_err(conflict)?;}
    let state=inspect(context,p)?;
    if state["status"]=="missing"{return Err(conflict("Applicable media context missing; acquire it or obtain an exact operator exception"));}
    // A manual context exception never overrides an actual current semantic
    // HOLD. Empty owner CLOSE has no blanket editorial requirement, but an
    // existing exact rejected judgment must be resolved by fresh review.
    if p["kind"]=="close"&&matches!(text(&p["editorialReview"],"decision"),"hold"|"revise")
        &&editorial_review::current_acquisition_metadata(context,p).is_ok(){
        return Err(conflict("Current editorial HOLD requires a fresh exact review before closing"));
    }
    if let Some(op)=op {
        if op["approvedMediaContextWaiver"]!=p["mediaContextWaiver"]{return Err(conflict("Media context exception changed after approval"));}
        if op["approvedPhotoAcquisitionProof"]!=photo_proof_from_state(&state){return Err(conflict("Photo acquisition proof changed after approval"));}
    }
    Ok(())
}
fn photo_proof_from_state(state:&Value)->Value{
    let proofs:Vec<_>=rows(state,"requirements").iter().filter(|r|r["acquisitionReceiptSha256"].is_string()).map(|r|json!({"sourceId":r["sourceId"],"receiptSha256":r["acquisitionReceiptSha256"],"artifact":r["artifact"]})).collect();
    if proofs.is_empty(){Value::Null}else{json!(proofs)}
}
pub(crate) fn photo_proof(context:&prepare_bundle::EvidenceContext<'_>,p:&Value)->ApiResult<Value>{
    inspect(context,p).map(|state|photo_proof_from_state(&state))
}
/// Existing editorial/source integrity remains mandatory. This only exempts a
/// missing default media prerequisite, never a rule, UNKNOWN or factual claim.
pub(crate) fn has_current_waiver(context:&prepare_bundle::EvidenceContext<'_>,p:&Value)->bool {
    inspect(context,p).is_ok_and(|state|state["status"]=="waived")
}
fn grant(d:&mut Value,proposal_id:&str,body:&Value,actor:&operator_auth::Actor,authorized:bool)->ApiResult<Value>{
    if !authorized||actor.id=="local-owner"||actor.authority_generation.is_none()
        ||conductor_authority::current_context().is_some(){return Err(ApiError(StatusCode::FORBIDDEN,"Personal media exception permission required".into()));}
    if body.as_object().is_none_or(|m|m.len()!=3||["expectedProposalRevision","expectedContextDigest","reason"].iter().any(|k|!m.contains_key(*k))){return Err(bad("Exact proposal revision, context digest and reason required"));}
    let reason=body["reason"].as_str().map(str::trim).filter(|s|!s.is_empty()&&s.len()<=1000&&!s.chars().any(char::is_control)).ok_or_else(||bad("A bounded exception reason required"))?;
    let p=row(d,"proposals",proposal_id)?;
    if p["revision"]!=body["expectedProposalRevision"]||!matches!(text(p,"status"),"draft"|"ready"){return Err(conflict("Proposal changed or already admitted"));}
    let item=row(d,"items",required(p,"itemId")?)?;
    if rows(d,"operations").iter().any(|op|recipient_operation_blocks_current(d,op,p,item)){return Err(conflict("Recipient operation already admitted; exception cannot authorize a replay"));}
    let state=inspect(&prepare_bundle::EvidenceContext::new(d),p)?;
    if state["contextDigest"]!=body["expectedContextDigest"]||state["status"]!="missing"{return Err(conflict("Media context changed or exception is unnecessary"));}
    if !rows(&state,"requirements").iter().filter(|r|r["ready"]!=true).all(|r|
        rows(&state,"attempts").iter().any(|a|a["status"]=="failed"&&a["sourceId"]==r["sourceId"])){return Err(conflict("Attempt acquisition for each missing source before granting a manual exception"));}
    let mut waiver=json!({"version":1,"proposalId":p["id"],"proposalRevision":p["revision"],"contextDigest":state["contextDigest"],
        "reason":reason,"attempts":state["attempts"],"actor":actor.public_json(),"authorityGeneration":actor.authority_generation,"createdAt":now()});
    waiver["waiverSha256"]=json!(hash(&waiver));
    row_mut(d,"proposals",proposal_id)?["mediaContextWaiver"]=waiver.clone();
    list_mut(d,"audit").push(json!({"id":id(),"action":"proposal.media_context_waived","refId":proposal_id,"actor":actor.public_json(),"createdAt":now(),"waiver":waiver}));
    inspect(&prepare_bundle::EvidenceContext::new(d),row(d,"proposals",proposal_id)?)
}
async fn permission(app:&App,actor:&operator_auth::Actor)->bool {
    let (Some(auth),Some(generation))=(&app.auth,&actor.authority_generation) else{return false;};
    auth.can_override_missing_media(&actor.id,generation).await.unwrap_or(false)
}
pub(crate) async fn get(State(app):State<App>,axum::Extension(actor):axum::Extension<operator_auth::Actor>,Path(proposal_id):Path<String>)->ApiResult<Json<Value>>{
    let allowed=permission(&app,&actor).await;
    let d=app.db.read_media_gate_context(&proposal_id,crate::storage::MediaGateReadBudget::default()).await?;let p=row(&d,"proposals",&proposal_id)?;
    let mut state=inspect(&prepare_bundle::EvidenceContext::new(&d),p)?;
    state["canOverrideMissingMedia"]=json!(allowed);
    if !allowed{state["overrideUnavailableReason"]=json!("Требуется личная website session с разрешением на исключения");}
    else if !rows(&state,"attempts").iter().any(|a|a["status"]=="failed"){state["overrideUnavailableReason"]=json!("Сначала необходима попытка получения медиаконтекста");}
    Ok(Json(state))
}
pub(crate) async fn put(State(app):State<App>,axum::Extension(actor):axum::Extension<operator_auth::Actor>,Path(proposal_id):Path<String>,Json(body):Json<Value>)->ApiResult<Json<Value>>{
    let allowed=permission(&app,&actor).await;
    app.change(|d|grant(d,&proposal_id,&body,&actor,allowed).map(Json)).await
}
pub(crate) async fn check_dispatch_authority(app:&App,op:&Value)->ApiResult<()> {
    let w=&op["approvedMediaContextWaiver"];
    if w.is_null(){return Ok(());}
    let (Some(auth),Some(actor),Some(generation))=(&app.auth,w["actor"]["id"].as_str(),w["authorityGeneration"].as_str()) else{return Err(conflict("Media exception authority unavailable"));};
    if auth.can_override_missing_media(actor,generation).await.unwrap_or(false){Ok(())}else{Err(conflict("Media exception permission revoked or changed"))}
}

#[cfg(test)]
#[path="media_context_gate_tests.rs"]
pub(crate) mod tests;
