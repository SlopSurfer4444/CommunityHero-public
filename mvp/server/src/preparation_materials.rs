//! New-capture mandatory materials. Source applicability is native authority;
//! image bytes are verified/staged before any model invocation, never summarized.
use serde_json::{json, Value};
use std::collections::BTreeSet;
pub(crate) const CONTRACT:&str="mandatory_post_materials_v1";
pub(crate) const POLICY:&str="post_text_all_photos_video_speech_v1";
pub(crate) const OPERATOR_CONTRACT:&str="OperatorMaterialReceipt.v1";
pub(crate) fn rows<'a>(v:&'a Value,k:&str)->&'a [Value]{v[k].as_array().map(Vec::as_slice).unwrap_or(&[])}
pub(crate) fn hash(v:&Value)->String{crate::media_fullframes::hash(v)}
pub(crate) fn enabled(request:&Value)->bool{request["mandatoryMaterialContract"]==CONTRACT}
fn photo(a:&Value)->bool{matches!(a["type"].as_str(),Some("photo"|"image"))}
fn video(a:&Value)->bool{matches!(a["type"].as_str(),Some("video"|"clip"|"reel"))}
fn sha(v:&Value)->bool{v.as_str().is_some_and(|s|s.len()==64&&s.bytes().all(|b|b.is_ascii_digit()||(b'a'..=b'f').contains(&b)))}
fn binding(d:&Value,post:&Value)->Result<Value,&'static str>{
    let active=crate::active_binding(d).map_err(|_|"mandatory_material_company_binding_invalid")?;
    let bound=if post["connectorBinding"].is_null(){active.clone()}else{
        crate::ConnectorBinding::from_json(&post["connectorBinding"]).map_err(|_|"mandatory_material_member_binding_invalid")?
    };
    bound.validate_scope(&active.workspace_id,&active.account_id).map_err(|_|"mandatory_material_foreign_company")?;
    if !crate::knowledge::in_account(post,d["account"].as_str().unwrap_or("")){return Err("mandatory_material_foreign_company");}
    Ok(bound.to_json())
}
fn photo_receipt(post:&Value,company:&Value,member_binding:&Value,version:&str)->Option<Value>{
    let r=&post["photoAcquisition"];let pin=&r["sourcePin"];
    let mut unsigned=r.clone();unsigned.as_object_mut()?.remove("receiptSha256");
    if r["version"]!=1||r["kind"]!="photo_acquisition"||r["purpose"]!="photo_acquire_only"
        ||r["modelCalled"]!=false||r["semanticAcceptance"]!=false||r["receiptSha256"]!=hash(&unsigned)
        ||r["sourceDigest"]!=hash(pin)||pin["account"]!=*company||pin["connectorBinding"]!=*member_binding
        ||pin["postId"]!=post["id"]||pin["postKey"]!=post["postKey"]||pin["sourceVersion"]!=version
        ||pin["attachments"]!=post["attachments"]||pin["attachmentDigest"]!=hash(&post["attachments"]){return None;}
    Some(r.clone())
}
pub(crate) fn speech(post:&Value,index:usize,version:&str,request:&Value,context:&crate::prepare_bundle::EvidenceContext<'_>,count:usize)->Option<Value>{
    let attachment=rows(post,"attachments").get(index)?;
    let identity=crate::media_analysis_reuse::attachment_identity(attachment);
    let single_ready=count==1&&context.strict_media_evidence(post).is_ok_and(|e|e["audioReady"]==true);
    for material in rows(request,"materials").iter().filter(|m|m["kind"]=="transcript"){
        let tr=&material["transcription"];
        let equivalence=if count==1{rows(material,"audioEquivalence").iter().find(|edge|
            edge["authorization"]=="owner_confirmed_same_video"&&edge["targetPostId"]==post["id"]&&edge["targetSourceVersion"]==version
            &&crate::media_audio_equivalence::bindings(context.workspace(),&crate::now()).is_ok_and(|current|current.contains(edge)))}else{None};
        let edge=rows(material,"exactFileAnalysisReuse").iter().find(|edge|edge["screenReuse"]==false
            &&edge["target"]["postId"]==post["id"]&&edge["target"]["sourceVersion"]==version
            &&edge["target"]["attachmentIndex"]==index&&edge["target"]["attachmentIdentity"]==identity);
        let legacy=single_ready&&material["postKey"]==post["postKey"]&&crate::knowledge::proven_full_audio(tr,version);
        if edge.is_none()&&equivalence.is_none()&&!legacy{continue;}
        let proven=if let Some(edge)=edge{crate::knowledge::proven_full_audio(tr,edge["originalSourceVersion"].as_str().unwrap_or(""))}
            else if let Some(edge)=equivalence{crate::knowledge::proven_full_audio(tr,edge["sourceVersion"].as_str().unwrap_or(""))}else{legacy};
        if !proven||material["text"].as_str().is_none_or(|t|t.trim().is_empty()){continue;}
        let outcome=match tr["audioStatus"].as_str(){Some("no_audio_stream")=>"no_audio",Some("inspected_no_speech")=>"no_speech",Some("transcribed")=>"transcript",None if tr["coverage"]=="full_audio"=>"transcript",_=>continue};
        return Some(json!({"materialId":material["id"],"materialSha256":hash(material),"text":material["text"],
            "knowledgeEntryId":material["knowledgeEntryId"],"knowledgeVersionId":material["knowledgeVersionId"],
            "transcription":tr,"outcome":outcome,"coverage":tr["coverage"],"durationSeconds":tr["mediaDurationSeconds"],
            "timestampGranularity":"extraction_chunk","applicability":edge,"audioEquivalence":equivalence,"screenReuse":false}));
    }None
}
pub(crate) fn attach_request(d:&Value,request:&mut Value)->Result<(),&'static str>{
    if request["account"]!=d["account"]{return Err("mandatory_material_foreign_company");}
    // New captures make the native permission binding explicit even when the
    // generic historical evidence reader omitted its legacy default binding.
    let permission=crate::active_binding(d).map_err(|_|"mandatory_material_company_binding_invalid")?.to_json();
    if request["connectorBinding"].is_null(){request["connectorBinding"]=permission;}else if request["connectorBinding"]!=permission{return Err("mandatory_material_permission_binding_changed");}
    let context=crate::prepare_bundle::EvidenceContext::new(d);
    let ids:BTreeSet<_>=rows(request,"items").iter().filter_map(|i|i["postId"].as_str()
        .or_else(||rows(request,"branches").iter().find(|b|b["id"]==i["branchId"]).and_then(|b|b["postId"].as_str()))).collect();
    if ids.is_empty(){return Err("mandatory_material_post_missing");}
    let mut members=Vec::new();let mut requirements=Vec::new();let mut selections=Vec::new();let mut bindings=Vec::new();
    for id in ids {
        let post=crate::row(d,"posts",id).map_err(|_|"mandatory_material_post_missing")?;
        let member_binding=binding(d,post)?;if !bindings.contains(&member_binding){bindings.push(member_binding.clone());}
        let version=crate::media_fullframes::source_version(post,d["account"].as_str().unwrap_or(""));
        let known=post["attachments"].is_array()&&post["attachmentsState"]!="unknown";
        let text_ready=post["text"].is_string()||post["body"].is_string()||post["title"].is_string();
        requirements.push(json!({"kind":"post_text","postId":id,"sourceVersion":version,"status":if text_ready{"ready"}else{"unavailable"},"reasonCode":if text_ready{"exact_post_text"}else{"post_text_unavailable"}}));
        if !known{requirements.push(json!({"kind":"attachment_metadata","postId":id,"sourceVersion":version,"status":"unavailable","reasonCode":"post_attachment_metadata_unknown"}));}
        let receipt=photo_receipt(post,&d["account"],&member_binding,&version);
        let video_count=rows(post,"attachments").iter().filter(|a|video(a)).count();
        let mut assets=Vec::new();let mut photo_indices=Vec::new();
        for (index,a) in rows(post,"attachments").iter().enumerate(){
            let identity=crate::media_analysis_reuse::attachment_identity(a);
            if photo(a){
                photo_indices.push(index);
                let retained=receipt.as_ref().and_then(|r|rows(r,"images").iter().find(|im|im["attachmentIndex"]==index
                    &&im["postId"]==id&&im["attachmentSha256"]==hash(a)&&sha(&im["sha256"])
                    &&im["artifact"]["sha256"]==im["sha256"]&&im["artifact"]["bytes"].as_u64().is_some_and(|n|n>0)
                    &&matches!(im["mime"].as_str(),Some("image/png"|"image/jpeg"|"image/webp"))&&im["width"].as_u64().is_some_and(|n|n>0)&&im["height"].as_u64().is_some_and(|n|n>0)));
                let overflow=retained.and_then(|im|if im["artifact"]["bytes"].as_u64().unwrap_or(0)>8*1024*1024{Some("photo_bytes_exceed_transport")}
                    else if im["width"].as_u64().unwrap_or(0)>12000||im["height"].as_u64().unwrap_or(0)>12000{Some("photo_dimensions_exceed_transport")}
                    else if im["width"].as_u64().unwrap_or(0).saturating_mul(im["height"].as_u64().unwrap_or(0))>24000000{Some("photo_pixels_exceed_transport")}else{None});
                let image=retained.filter(|_|overflow.is_none());
                requirements.push(json!({"kind":"post_photo","postId":id,"attachmentIndex":index,"attachmentIdentity":identity,
                    "sourceVersion":version,"status":if image.is_some(){"ready"}else if overflow.is_some(){"unavailable"}else{"pending"},
                    "reasonCode":overflow.unwrap_or(if image.is_some(){"verified_photo_acquisition"}else{"photo_acquisition_required"}),
                    "retainedBytes":retained.map(|im|im["artifact"]["bytes"].clone()),"retainedWidth":retained.map(|im|im["width"].clone()),"retainedHeight":retained.map(|im|im["height"].clone())}));
                assets.push(json!({"modality":"photo","attachmentIndex":index,"attachmentIdentity":identity,
                    "sourceVersion":version,"photo":image,"acquisitionReceiptSha256":receipt.as_ref().map(|r|r["receiptSha256"].clone())}));
            }else if video(a){
                let result=speech(post,index,&version,request,&context,video_count);
                requirements.push(json!({"kind":"video_speech","postId":id,"attachmentIndex":index,"attachmentIdentity":identity,
                    "sourceVersion":version,"status":if result.is_some(){"ready"}else{"pending"},"reasonCode":if result.is_some(){"full_video_speech_outcome"}else{"video_speech_unproven"}}));
                assets.push(json!({"modality":"video","attachmentIndex":index,"attachmentIdentity":identity,"sourceVersion":version,"speech":result}));
            }
        }
        if crate::knowledge::is_video_post(post)&&video_count==0{requirements.push(json!({"kind":"video_speech","postId":id,"sourceVersion":version,"status":"unavailable","reasonCode":"video_attachment_identity_missing"}));}
        for item in rows(request,"items").iter().filter(|i|i["postId"]==id){if !photo_indices.is_empty(){
            selections.push(json!({"itemId":item["id"],"postId":id,"attachmentIndices":photo_indices,"reason":"All photos of the exact post are mandatory"}));
        }}
        members.push(json!({"canonicalPostId":id,"connectorBinding":member_binding,"postKey":post["postKey"],"postSourceVersion":version,
            "fields":{"title":post["title"],"text":post["text"],"body":post["body"],"caption":post["caption"],"attachments":post["attachments"]},"assets":assets}));
    }
    let mut comment_photos=Vec::new();
    for selected in rows(request,"items") {
        let item=crate::row(d,"items",selected["id"].as_str().ok_or("comment_photo_recipient_missing")?).map_err(|_|"comment_photo_recipient_missing")?;
        let raw=item.get("attachments").or_else(||item.get("commentAttachments")).and_then(Value::as_array);
        if item["attachmentsState"]=="unknown"||raw.is_none()&&(item.get("attachments").is_some()||item.get("commentAttachments").is_some()
            ||item["attachmentsState"]=="present"||item["commentAttachmentsPresent"]==true)
            ||raw.is_some_and(Vec::is_empty)&&(item["attachmentsState"]=="present"||item["commentAttachmentsPresent"]==true)
            ||item.get("attachments").is_some()&&item.get("commentAttachments").is_some()&&item["attachments"]!=item["commentAttachments"]{
            requirements.push(json!({"kind":"comment_attachment_metadata","itemId":item["id"],"status":"unavailable","reasonCode":"comment_attachment_metadata_unknown_or_conflicting"}));
        }
        let Some(raw)=raw else{continue};
        if !raw.iter().any(|a|matches!(a["type"].as_str(),Some("photo"|"image"|"sticker"))){continue;}
        let pin=crate::photo_acquisition::comment_pin(d,item).ok();
        let metadata=crate::photo_acquisition::current_comment_metadata(d,item).ok();
        for (index,attachment) in raw.iter().enumerate().filter(|(_,a)|matches!(a["type"].as_str(),Some("photo"|"image"|"sticker"))) {
            let retained=metadata.as_ref().and_then(|m|rows(m,"imageEvidence").iter().find(|image|image["attachmentIndex"]==index)).cloned();
            let ready=pin.is_some()&&retained.is_some();
            let source=json!({"itemId":item["id"],"attachmentIndex":index,"attachmentIdentity":crate::media_analysis_reuse::attachment_identity(attachment),
                "sourceRole":pin.as_ref().map(|p|p["sourceRole"].clone()),"sourceVersion":pin.as_ref().map(|p|p["sourceVersion"].clone()),
                "photo":retained,"acquisitionReceiptSha256":metadata.as_ref().map(|m|m["acquisitionReceiptSha256"].clone())});
            requirements.push(json!({"kind":"comment_photo","itemId":item["id"],"attachmentIndex":index,"status":if ready{"ready"}else{"unavailable"},
                "reasonCode":if ready{"exact_original_comment_pixels"}else{"comment_photo_source_or_receipt_unavailable"}}));
            comment_photos.push(source);
        }
    }
    comment_photos.sort_by_key(|source|(source["itemId"].as_str().unwrap_or("").to_owned(),source["attachmentIndex"].as_u64().unwrap_or(0)));
    bindings.sort_by_key(Value::to_string);members.sort_by_key(|m|m["canonicalPostId"].as_str().unwrap_or("").to_owned());
    let photos=members.iter().flat_map(|m|rows(m,"assets")).filter(|a|a["modality"]=="photo").collect::<Vec<_>>();
    let all_photos=photos.iter().copied().chain(comment_photos.iter()).collect::<Vec<_>>();
    let bytes=all_photos.iter().map(|a|a["photo"]["artifact"]["bytes"].as_u64().unwrap_or(0)).sum::<u64>();let pixels=all_photos.iter().map(|a|a["photo"]["width"].as_u64().unwrap_or(0).saturating_mul(a["photo"]["height"].as_u64().unwrap_or(0))).sum::<u64>();
    if all_photos.len()>16||bytes>32*1024*1024||pixels>64000000{requirements.push(json!({"kind":"post_photo_transport","status":"unavailable","reasonCode":if all_photos.len()>16{"post_photo_count_exceeds_transport"}else if bytes>32*1024*1024{"post_photo_bytes_exceed_transport"}else{"post_photo_pixels_exceed_transport"},"photoCount":all_photos.len(),"retainedBytes":bytes,"retainedPixels":pixels}));}
    let ready=requirements.iter().all(|r|r["status"]=="ready");
    let group=&request["strictGroup"];
    // Common content does not acquire a different version for every recipient
    // or workflow bump. Exact recipient admission remains in strictGroup.
    let family=json!({"kind":group["kind"],"familyKey":group["familyKey"],"familyProof":group["familyProof"],"copies":group["copies"]});
    let mut bundle=json!({"schemaVersion":1,"companyId":d["account"],"connectionBindings":bindings,"members":members,
        "family":family,"knowledgeManifest":request["knowledgeManifest"],"materialPolicy":{"version":POLICY,"noSilentTruncation":true,"screenReuse":false},
        "readiness":{"status":if ready{"ready"}else{"pending"},"requirements":requirements}});
    if !comment_photos.is_empty(){bundle["commentPhotos"]=json!(comment_photos);request["commentPhotoSources"]=bundle["commentPhotos"].clone();}
    else{request.as_object_mut().ok_or("mandatory_material_request_invalid")?.remove("commentPhotoSources");}
    bundle["contentSha256"]=json!(hash(&bundle));
    request["mandatoryMaterialContract"]=json!(CONTRACT);request["postContextBundle"]=bundle.clone();request["materialReadiness"]=bundle["readiness"].clone();
    request["visualSelection"]=json!({"version":1,"postImages":selections});
    request["visualNeedContract"]=json!(crate::prepare_bundle::visual::CONTRACT);
    crate::manual_frame_request::attach_request(d,request)?;
    Ok(())
}
pub(crate) fn require_request(d:&Value,request:&Value)->Result<(),&'static str>{
    if !enabled(request){return Err("legacy_material_contract_unmet");}
    let mut current=request.clone();
    // Earlier paid captures predate manual selection. Their absent selection
    // means empty; a later manual request never broadens the frozen capture.
    if current.get("manualFrameRequestIds").is_none()
        &&rows(&current,"optionalFrameRefs").iter().all(|r|r["origin"]!="manual_before_generation"){
        current["manualFrameRequestIds"]=json!([]);
    }
    attach_request(d,&mut current)?;
    if current["postContextBundle"]!=request["postContextBundle"]{return Err("mandatory_material_source_changed");}
    if request["materialReadiness"]["status"]!="ready"{return Err("mandatory_material_not_ready");}
    if current.get("optionalFrameRefs").cloned().unwrap_or_else(||json!([]))!=request.get("optionalFrameRefs").cloned().unwrap_or_else(||json!([])){
        return Err("manual_frame_captured_selection_changed");
    }
    crate::manual_frame_request::require_refs(d,&current)?;Ok(())
}
/// Resolve only a native saved request with the exact retained paid wire hash.
/// Editorial requests live in their immutable dispatch journal, not prepareBundle.
fn receipt_request<'a>(d:&Value,job:&'a Value,receipt:&Value)->Result<&'a Value,&'static str>{
    let profile=crate::accounts::Profile::from_workspace(d).map_err(|_|"mandatory_material_company_invalid")?;
    let matches=|request:&Value|{
        let mut wire=request.clone();
        if profile.bind_request(&mut wire).is_err(){return false;}
        wire["operation"]=json!("assistant");
        receipt["paidResultRef"]["requestSha256"]==hash(&wire)
            &&crate::model_material_receipt::expected(request)==crate::model_material_receipt::observed_expected(&receipt["body"])
            &&request.get("optionalFrameRefs").cloned().unwrap_or_else(||json!([]))==receipt["body"]["optionalFrameRefs"]
    };
    let mut requests=Vec::new();
    if job["prepareBundle"]["request"].is_object()&&matches(&job["prepareBundle"]["request"]){requests.push(&job["prepareBundle"]["request"]);}
    for entry in rows(job,"editorialBatches"){
        let request=&entry["capture"]["batch"]["request"];
        if request.is_object()&&matches(request){requests.push(request);}
    }
    if requests.len()!=1{return Err("mandatory_material_request_capture_missing_or_ambiguous");}
    Ok(requests[0])
}
pub(crate) fn proposal_receipt<'a>(context:&crate::prepare_bundle::EvidenceContext<'_>,proposal:&'a Value)->Result<&'a Value,&'static str>{
    let editorial=proposal["editorialModelMaterialReceipt"].is_object();
    let receipt=if editorial{&proposal["editorialModelMaterialReceipt"]}else{&proposal["modelMaterialReceipt"]};
    checked_proposal_receipt(context,proposal,receipt,editorial)
}
fn checked_proposal_receipt<'a>(context:&crate::prepare_bundle::EvidenceContext<'_>,proposal:&Value,receipt:&'a Value,editorial:bool)->Result<&'a Value,&'static str>{
    if receipt["contract"]!=CONTRACT||receipt["body"]["completenessStatus"]!="complete"{return Err("legacy_material_contract_unmet");}
    crate::model_material_receipt::validate_pointer(receipt)?;
    let d=context.workspace();
    for pin in rows(&receipt["body"],"memberPins"){
        let post=crate::row(d,"posts",pin["postId"].as_str().ok_or("mandatory_material_post_missing")?).map_err(|_|"mandatory_material_post_missing")?;
        let fields=json!({"title":post["title"],"text":post["text"],"body":post["body"],"caption":post["caption"],"attachments":post["attachments"]});
        if binding(d,post)?!=pin["connectorBinding"]||crate::media_fullframes::source_version(post,d["account"].as_str().unwrap_or(""))!=pin["sourceVersion"]||hash(&fields)!=pin["postFieldsSha256"]{return Err("mandatory_material_source_changed");}
    }
    let job=crate::row(d,"jobs",receipt["nativeJobId"].as_str().ok_or("mandatory_material_origin_missing")?).map_err(|_|"mandatory_material_origin_missing")?;
    if !rows(job,"modelMaterialReceipts").contains(receipt)||!rows(job,"retainedEvidence").contains(&receipt["paidResultRef"]){return Err("mandatory_material_paid_receipt_unattached");}
    let request=receipt_request(d,job,receipt)?;
    require_request(d,request)?;
    if editorial{
        if job["kind"]!="editorial_review"||request["purpose"]!="editorial_review"
            {return Err("mandatory_editorial_material_candidate_changed");}
        crate::editorial_review::material_capture_current(context,proposal,request,&receipt["body"])?;
    }
    Ok(receipt)
}
/// Metadata-only recovery preserves the ORIGINAL answering delivery proof.
/// A later stale editorial candidate cannot replace or retarget that capture.
pub(crate) fn require_original_generation(context:&crate::prepare_bundle::EvidenceContext<'_>,proposal:&Value)->Result<(),&'static str>{
    checked_proposal_receipt(context,proposal,&proposal["modelMaterialReceipt"],false).map(|_|())
}
pub(crate) fn require_proposal(context:&crate::prepare_bundle::EvidenceContext<'_>,proposal:&Value)->Result<(),&'static str>{
    // A new semantic source revision preserves the old answering observation;
    // it cannot relabel that observation as delivery of the new current unit.
    if proposal.get(crate::proposal_source_rebind::FIELD).is_some()&&!proposal["editorialModelMaterialReceipt"].is_object(){
        return Err("source_rebind_current_editorial_material_receipt_required");
    }
    if proposal["editorialModelMaterialReceipt"].is_object()||proposal["modelMaterialReceipt"].is_object(){return proposal_receipt(context,proposal).map(|_|());}
    require_operator_proposal(context,proposal)
}
/// A server-created manual reply is distinct from a model suggestion or its
/// copied/recovered descendant. Absence of legacy metadata proves nothing.
pub(crate) fn genuine_manual(proposal:&Value)->bool{
    fn model_marker(value:&Value,depth:usize)->bool{
        if depth>32{return true;}
        match value{
            Value::Object(fields)=>fields.iter().any(|(key,value)|
                (key=="nativeCreationOrigin"&&value!="operator_manual_v1")
                ||(["priorPreparationOrigin","generationMetadata","prepareRunId","prepareBundleId","prepareBundleDigest","sourceProposalId","sourceProposalRevision",crate::retained_paid_recovery::FIELD].contains(&key.as_str())&&!value.is_null())
                ||model_marker(value,depth+1)),
            Value::Array(rows)=>rows.iter().any(|v|model_marker(v,depth+1)),_=>false,
        }
    }
    fn clean(row:&Value)->bool{
        row["nativeCreationOrigin"]=="operator_manual_v1"
            &&["priorPreparationOrigin","generationMetadata","prepareRunId","prepareBundleId","prepareBundleDigest","sourceProposalId","sourceProposalRevision",crate::retained_paid_recovery::FIELD]
                .iter().all(|key|row.get(*key).is_none_or(Value::is_null))
    }
    if !clean(proposal)||model_marker(proposal,0){return false;}
    let origin=&proposal["origin"];if origin.is_null(){return true;}
    let history=match proposal["history"].as_array(){Some(history)if!history.is_empty()=>history,_=>return false};
    let mut original=origin.clone();let Some(fields)=original.as_object_mut()else{return false};fields.remove("history");
    if original!=history[0]||!clean(origin)||!origin["origin"].is_null()
        ||origin["id"]!=proposal["id"]||origin["itemId"]!=proposal["itemId"]||origin["kind"]!=proposal["kind"]
        ||origin["routeTarget"]!=proposal["routeTarget"]||origin["revision"].as_u64().is_none_or(|n|n>=proposal["revision"].as_u64().unwrap_or(0)){return false;}
    history.iter().all(|row|clean(row)&&row["id"]==proposal["id"]&&row["itemId"]==proposal["itemId"]&&row["kind"]==proposal["kind"]
        &&row["routeTarget"]==proposal["routeTarget"]&&row["revision"].as_u64().is_some_and(|n|n<proposal["revision"].as_u64().unwrap_or(0))
        &&(row["origin"].is_null()||row["origin"]==*origin))
}
pub(crate) fn operator_request(context:&crate::prepare_bundle::EvidenceContext<'_>,proposal:&Value)->Result<Value,&'static str>{
    if !genuine_manual(proposal){return Err("operator_material_manual_origin_unproven");}
    let mut request=context.evidence_for_item(proposal["itemId"].as_str().ok_or("mandatory_material_recipient_missing")?)?;
    request["purpose"]=json!("operator_material_review");attach_request(context.workspace(),&mut request)?;
    require_request(context.workspace(),&request)?;Ok(request)
}
pub(crate) fn operator_receipt(context:&crate::prepare_bundle::EvidenceContext<'_>,proposal:&Value,preview:&Value,candidate:&Value,review:&Value)->Result<Value,&'static str>{
    let request=operator_request(context,proposal)?;
    if preview["reviewedBy"]["role"]!="owner"||preview["reviewAuthorityDigest"].as_str().is_none_or(|s|s.len()!=64)
        ||review["source"]["kind"]!="operator_assisted_review"||review["source"]["previewDigest"]!=preview["previewDigest"]
        ||review["candidate"]!=*candidate||candidate["proposalId"]!=proposal["id"]||candidate["proposalRevision"]!=proposal["revision"]{return Err("operator_material_review_binding_invalid");}
    let mut receipt=json!({"schemaVersion":1,"contract":OPERATOR_CONTRACT,"policy":POLICY,"kind":"native_operator_material_review",
        "modelCalled":false,"companyId":context.workspace()["account"],"connectorBinding":request["connectorBinding"],
        "proposalId":proposal["id"],"proposalRevision":proposal["revision"],"textSha256":crate::editorial_review::hash_text(proposal["text"].as_str().unwrap_or("")),
        "previewDigest":preview["previewDigest"],"reviewReceiptSha256":review["receiptSha256"],"reviewedBy":preview["reviewedBy"],
        "reviewAuthorityDigest":preview["reviewAuthorityDigest"],"candidate":candidate,"body":crate::model_material_receipt::expected(&request)});
    receipt["receiptSha256"]=json!(hash(&receipt));Ok(receipt)
}
fn require_operator_proposal(context:&crate::prepare_bundle::EvidenceContext<'_>,proposal:&Value)->Result<(),&'static str>{
    let receipt=&proposal["operatorMaterialReceipt"];let review=&proposal["editorialReview"];
    let mut unsigned=receipt.clone();unsigned.as_object_mut().ok_or("legacy_material_contract_unmet")?.remove("receiptSha256");
    if receipt["schemaVersion"]!=1||receipt["contract"]!=OPERATOR_CONTRACT||receipt["policy"]!=POLICY||receipt["kind"]!="native_operator_material_review"
        ||receipt["modelCalled"]!=false||receipt.get("paidResultRef").is_some()||receipt.get("materialInvocation").is_some()
        ||receipt["receiptSha256"]!=hash(&unsigned)||receipt["companyId"]!=context.workspace()["account"]
        ||receipt["proposalId"]!=proposal["id"]||receipt["proposalRevision"]!=proposal["revision"]
        ||receipt["textSha256"]!=crate::editorial_review::hash_text(proposal["text"].as_str().unwrap_or(""))
        ||review["source"]["kind"]!="operator_assisted_review"||receipt["reviewReceiptSha256"]!=review["receiptSha256"]
        ||receipt["previewDigest"]!=review["source"]["previewDigest"]||receipt["reviewedBy"]!=review["source"]["reviewedBy"]
        ||receipt["reviewAuthorityDigest"]!=review["source"]["reviewAuthorityDigest"]||receipt["candidate"]!=review["candidate"]{return Err("operator_material_receipt_invalid");}
    crate::editorial_review::require_current(context,proposal)?;
    let request=operator_request(context,proposal)?;
    if receipt["connectorBinding"]!=request["connectorBinding"]||receipt["body"]!=crate::model_material_receipt::expected(&request){return Err("mandatory_material_source_changed");}
    Ok(())
}
/// Scoped storage may allow only this exact native-derived added operator
/// proof after its ordinary preview/actor/receipt/history validation.
pub(crate) fn validate_operator_material_delta(d:&Value,old:&Value,new:&Value,preview:&Value,candidate:&Value)->Result<(),&'static str>{
    if old["kind"]!="reply_and_close"||old["modelMaterialReceipt"].is_object()||old["editorialModelMaterialReceipt"].is_object(){
        return if old["operatorMaterialReceipt"]==new["operatorMaterialReceipt"]{Ok(())}else{Err("operator_material_delta_unexpected")};
    }
    let expected=operator_receipt(&crate::prepare_bundle::EvidenceContext::new(d),old,preview,candidate,&new["editorialReview"])?;
    if expected!=new["operatorMaterialReceipt"]{return Err("operator_material_delta_changed");}Ok(())
}
/// The matching pointer must already belong to this exact native editorial
/// job and paid capture. It is derived from the same new saved model verdict.
pub(crate) fn validate_editorial_material_delta(d:&Value,job_id:&str,old:&Value,new:&Value)->Result<(),&'static str>{
    if old["editorialModelMaterialReceipt"]==new["editorialModelMaterialReceipt"]{return Ok(());}
    let receipt=&new["editorialModelMaterialReceipt"];
    if receipt["nativeJobId"]!=job_id||new["editorialReview"]["source"]["kind"]!="dedicated_model_review"
        ||new["editorialReview"]["source"]["runMetadata"]["materialInvocation"]!=receipt["body"]{return Err("mandatory_editorial_material_delta_changed");}
    let context=crate::prepare_bundle::EvidenceContext::new(d);checked_proposal_receipt(&context,new,receipt,true)?;
    let job=crate::row(d,"jobs",job_id).map_err(|_|"mandatory_material_origin_missing")?;let request=receipt_request(d,job,receipt)?;
    if new["editorialReview"]["source"]["batchDigest"]!=hash(request)||!rows(request,"editorialCandidates").contains(&new["editorialReview"]["candidate"]){return Err("mandatory_editorial_material_delta_changed");}
    Ok(())
}
#[cfg(test)] #[path="preparation_materials_tests.rs"] mod tests;
