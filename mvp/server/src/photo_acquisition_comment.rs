//! Typed original-comment pixels. No canonical post is invented or rewritten.
use super::*;
pub(super) fn image(a:&Value)->bool{matches!(text(a,"type"),"photo"|"image"|"sticker")}
fn attachments(item:&Value)->ApiResult<&Vec<Value>>{
    if item.get("attachments").is_some()&&item.get("commentAttachments").is_some()&&item["attachments"]!=item["commentAttachments"]{
        return Err(conflict("Comment attachment projections differ"));
    }
    item.get("attachments").or_else(||item.get("commentAttachments")).and_then(Value::as_array).ok_or_else(||conflict("Comment attachment metadata unavailable"))
}
pub(super) fn parse(body:&Value)->ApiResult<()>{
    fields(body,&["receiptId","sourceKind","itemId","expectedRevision","expectedSourceVersion","attachmentDigest"])?;
    let receipt=text(body,"receiptId");let item=text(body,"itemId");
    if body["sourceKind"]!="comment_attachment"||uuid::Uuid::parse_str(receipt).is_err()
        ||uuid::Uuid::parse_str(receipt).is_ok_and(|value|value.to_string()!=receipt)||item.is_empty()||item.len()>128
        ||item.trim()!=item||item.chars().any(char::is_control)||body["expectedRevision"].as_u64().is_none_or(|n|n==0)
        ||!sha(&body["expectedSourceVersion"])||!sha(&body["attachmentDigest"]){return Err(bad("Exact canonical comment source and UUID receipt required"));}Ok(())
}
pub(crate) fn pin(d:&Value,item:&Value)->ApiResult<Value>{
    let binding=active_binding(d)?;let target=bound_item(&binding,item)?;let raw=attachments(item)?;
    if !knowledge::in_account(item,required(d,"account")?)||item["attachmentsState"]=="unknown"||raw.len()>20
        ||raw.iter().any(|a|!a.is_object()||["url","source_url","preview_url","title"].iter().any(|key|a[*key].as_str().is_some_and(|v|v.len()>8192))){return Err(conflict("Comment photo source binding or metadata unavailable"));}
    let slots=raw.iter().filter(|a|image(a)).collect::<Vec<_>>();
    if slots.is_empty()||slots.len()>16||slots.iter().any(|a|a["url"].as_str().is_none_or(str::is_empty)){return Err(bad("Eligible bounded original comment images required"));}
    let branch=row(d,"branches",required(item,"branchId")?)?;
    if branch["postId"]!=item["postId"]||!knowledge::in_account(branch,required(d,"account")?){return Err(conflict("Original comment branch changed"));}
    fn exact_id(value:&Value)->Option<&str>{value.as_str().filter(|id|!id.is_empty())}
    let target_id=exact_id(&item["targetId"]);let provider_id=exact_id(&item["itemId"]);
    let messages=rows(branch,"messages").iter().filter(|m|{
        let message_id=exact_id(&m["id"]);let message_provider=exact_id(&m["providerItemId"]);
        message_id.is_some()&&(target_id.is_some()&&message_id==target_id||provider_id.is_some()&&message_id==provider_id
            ||provider_id.is_some()&&message_provider==provider_id)
    }).collect::<Vec<_>>();
    if messages.len()!=1{return Err(conflict("Original comment role requires one exact source message"));}
    let message=messages[0];
    if message["id"].as_str().is_none_or(|id|id.len()>500||id.trim()!=id||id.chars().any(char::is_control))
        ||!message["roleEvidence"].is_null()&&message["roleEvidence"].as_str().is_none_or(|e|e.len()>160||e.chars().any(char::is_control)){
        return Err(conflict("Original comment role evidence invalid"));
    }
    if message["providerObjectId"].as_str().is_some_and(|id|id!=text(item,"objectId"))
        ||message.get("attachments").is_some_and(|value|value!=&json!(raw)) {return Err(conflict("Original comment attachment or provider projection differs"));}
    let role=match text(message,"role"){"customer"=>"customer","brand"=>"brand",_=>"unknown"};
    let mut p=json!({"version":2,"sourceKind":"comment_attachment","account":d["account"],"connectorBinding":binding.to_json(),
        "itemId":item["id"],"providerItemId":target["itemId"],"objectId":target["objectId"],"conversationKey":target["conversationKey"],
        "branchId":item["branchId"],"postId":item["postId"],"postKey":item["postKey"],
        "sourceRole":{"role":role,"messageId":message["id"],"roleEvidence":message["roleEvidence"]},
        "attachmentDigest":hash(&json!(raw)),"attachments":raw});
    p["sourceVersion"]=json!(hash(&p));Ok(p)
}
fn source(d:&Value,body:&Value)->ApiResult<Value>{
    let item=row(d,"items",required(body,"itemId")?)?;check_revision(item,&body["expectedRevision"])?;let p=pin(d,item)?;
    if p["sourceVersion"]!=body["expectedSourceVersion"]||p["attachmentDigest"]!=body["attachmentDigest"]{return Err(conflict("Original comment source changed"));}Ok(p)
}
fn owns(d:&Value,p:&Value,except:Option<&str>)->ApiResult<()>{
    if rows(d,"jobs").iter().any(|job|Some(text(job,"id"))!=except&&job["kind"]=="photo_acquisition"
        &&job["sourcePin"]["sourceKind"]=="comment_attachment"&&job["sourcePin"]["itemId"]==p["itemId"]
        &&matches!(text(job,"status"),"running"|"queued"|"paused"|"unknown"|"dispatching"|"interrupted")){return Err(conflict("Comment source has an active or unresolved acquisition owner"));}
    let target=bound_item(&active_binding(d)?,row(d,"items",text(p,"itemId"))?)?;
    if rows(d,"operations").iter().any(|op|matches!(text(op,"status"),"unknown"|"dispatching"|"running")
        &&crate::recipient_operation_blocks_current(d,op,&json!({"itemId":p["itemId"]}),&target)){return Err(conflict("Comment recipient operation is active or UNKNOWN"));}Ok(())
}
pub(super) fn claim(d:&mut Value,body:&Value,actor:&str,at:&str)->ApiResult<Admission>{
    parse(body)?;let p=source(d,body)?;
    if let Some(job)=rows(d,"jobs").iter().find(|j|j["id"]==body["receiptId"]){
        if job["kind"]!="photo_acquisition"||job["request"]!=*body||job["authorizedBy"]!=actor||job["sourcePin"]!=p{return Err(conflict("Comment receipt already bound or stale"));}
        if job["status"]=="completed"{validate_receipt(d,row(d,"items",text(body,"itemId"))?,&job["receipt"],&store().map_err(|_|conflict("Comment evidence store unavailable"))?)?;}
        return Ok(Admission::Replay(job.clone()));
    }
    if rows(d,"jobs").iter().any(|job|job["kind"]=="photo_acquisition"&&job["sourcePin"]==p){return Err(conflict("Exact comment image source was already attempted; reconcile its receipt"));}
    owns(d,&p,None)?;let item=row(d,"items",text(body,"itemId"))?;let head=item["commentPhotoAcquisition"].clone();
    let job=json!({"id":body["receiptId"],"kind":"photo_acquisition","purpose":"photo_acquire_only","status":"running","refId":body["itemId"],
        "sourceKind":"comment_attachment","account":p["account"],"connectorBinding":p["connectorBinding"],"sourcePin":p,"sourceDigest":hash(&p),
        "request":body,"authorizedBy":actor,"createdAt":at});
    let items=vec![json!({"id":item["id"],"attachments":p["attachments"],"sourceRole":p["sourceRole"]})];
    list_mut(d,"jobs").push(job.clone());audit(d,"comment_photo.acquisition_claimed",text(body,"receiptId"));Ok(Admission::Fresh(Claim{job,pin:p,head,items}))
}
pub(super) fn validate_receipt(d:&Value,item:&Value,r:&Value,store:&ArtifactStore)->ApiResult<()>{
    let p=pin(d,item)?;
    if r["version"]!=2||r["kind"]!="photo_acquisition"||r["sourceKind"]!="comment_attachment"||r["purpose"]!="photo_acquire_only"
        ||r["modelCalled"]!=false||r["semanticAcceptance"]!=false||r["validator"]!="assistant-images-structural-v1"
        ||r["sourcePin"]!=p||r["sourceDigest"]!=hash(&p)||r["receiptSha256"]!=hash(&unsigned(r))||!r["images"].is_array()||!r["failures"].is_array(){return Err(conflict("Original comment acquisition receipt invalid or stale"));}
    let mut slots=BTreeSet::new();let mut bytes=0;let mut pixels=0;
    for v in rows(r,"images"){
        let index=v["attachmentIndex"].as_u64().and_then(|n|usize::try_from(n).ok()).ok_or_else(||conflict("Comment photo slot invalid"))?;
        let attachment=rows(&p,"attachments").get(index).filter(|a|image(a)).ok_or_else(||conflict("Comment source slot changed"))?;
        if !slots.insert(index)||v["origin"]!="comment_attachment"||v["itemId"]!=p["itemId"]||v.get("postId").is_some()
            ||v["sourceRole"]!=p["sourceRole"]||v["attachmentSha256"]!=hash(attachment){return Err(conflict("Original comment role or source slot mismatch"));}
        let reference=image_meta(v)?;bytes+=reference.bytes;pixels+=v["width"].as_u64().unwrap()*v["height"].as_u64().unwrap();store.verify(&reference).map_err(|_|conflict("Comment artifact missing or changed"))?;
    }
    for f in rows(r,"failures"){
        let index=f["attachmentIndex"].as_u64().and_then(|n|usize::try_from(n).ok()).ok_or_else(||conflict("Comment failure slot invalid"))?;
        if !slots.insert(index)||rows(&p,"attachments").get(index).is_none_or(|a|!image(a))||f["itemId"]!=p["itemId"]||f["origin"]!="comment_attachment"
            ||f["sourceRole"]!=p["sourceRole"]||f.get("postId").is_some()||!matches!(text(f,"stage"),"acquisition"|"validation"|"not_started")||text(f,"category").is_empty(){return Err(conflict("Comment failure role/source mismatch"));}
    }
    if bytes>64*1024*1024||pixels>192_000_000||slots.len()!=rows(&p,"attachments").iter().filter(|a|image(a)).count(){return Err(conflict("Incomplete original comment image outcomes"));}Ok(())
}
pub(super) fn commit(d:&mut Value,c:&Claim,outcome:&Value,at:&str,store:&ArtifactStore)->ApiResult<Value>{
    if row(d,"jobs",text(&c.job,"id"))?!=&c.job{return Err(conflict("Comment acquisition ownership changed"));}
    let item=row(d,"items",text(&c.pin,"itemId"))?;
    if source(d,&c.job["request"])?!=c.pin||item["commentPhotoAcquisition"]!=c.head{return Err(conflict("Comment source/head changed during acquisition"));}owns(d,&c.pin,Some(text(&c.job,"id")))?;
    let mut r=json!({"id":c.job["id"],"version":2,"kind":"photo_acquisition","sourceKind":"comment_attachment","purpose":"photo_acquire_only","authorizedBy":c.job["authorizedBy"],"acquiredAt":at,
        "sourcePin":c.pin,"sourceDigest":c.job["sourceDigest"],"images":outcome["images"],"failures":outcome["failures"],"validator":"assistant-images-structural-v1","modelCalled":false,"semanticAcceptance":false});
    r["receiptSha256"]=json!(hash(&r));validate_receipt(d,item,&r,store)?;
    row_mut(d,"items",text(&c.pin,"itemId"))?["commentPhotoAcquisition"]=r.clone();let job=row_mut(d,"jobs",text(&c.job,"id"))?;
    job["status"]=json!("completed");job["receipt"]=r.clone();job["finishedAt"]=json!(at);audit(d,"comment_photo.acquisition_observed",text(&c.job,"id"));Ok(r)
}
pub(crate) fn current_metadata(d:&Value,item:&Value)->ApiResult<Value>{
    let r=&item["commentPhotoAcquisition"];validate_receipt(d,item,r,&store().map_err(|_|conflict("Comment evidence store unavailable"))?)?;
    let images=rows(r,"images").iter().map(|image|{let mut image=image.clone();image["acquisitionReceiptSha256"]=r["receiptSha256"].clone();image["sourceVersion"]=r["sourcePin"]["sourceVersion"].clone();image}).collect::<Vec<_>>();
    Ok(json!({"imageEvidence":images,"imageFailures":r["failures"],"sourcePin":r["sourcePin"],"acquisitionReceiptSha256":r["receiptSha256"],"modelCalled":false,"semanticAcceptance":false}))
}
pub(crate) async fn preflight(State(app):State<App>,axum::Extension(actor):axum::Extension<operator_auth::Actor>,Path(item_id):Path<String>)->ApiResult<Json<Value>>{
    if actor.role!="owner"{return Err(ApiError(StatusCode::FORBIDDEN,"Comment photo acquisition requires owner".into()));}
    if item_id.is_empty()||item_id.len()>128||item_id.trim()!=item_id||item_id.chars().any(char::is_control){return Err(bad("Bounded exact comment ID required"));}
    let d=app.read().await?;let item=row(&d,"items",&item_id)?;let p=pin(&d,item)?;let ownership=owns(&d,&p,None).err().map(|error|error.1);
    let attempts=rows(&d,"jobs").iter().filter(|job|job["kind"]=="photo_acquisition"&&job["sourcePin"]["sourceKind"]=="comment_attachment"&&job["sourcePin"]["itemId"]==item_id)
        .map(|job|json!({"id":job["id"],"status":job["status"],"sourceDigest":job["sourceDigest"],"exactCurrentSource":job["sourcePin"]==p})).collect::<Vec<_>>();
    let attempted=rows(&d,"jobs").iter().any(|job|job["kind"]=="photo_acquisition"&&job["sourcePin"]==p);
    Ok(Json(json!({"version":2,"sourceKind":"comment_attachment","itemId":item_id,"expectedRevision":item["revision"],"account":p["account"],"connectorBinding":p["connectorBinding"],
        "sourceRole":p["sourceRole"],"expectedSourceVersion":p["sourceVersion"],"attachmentDigest":p["attachmentDigest"],"sourceDigest":hash(&p),"attempts":attempts,
        "currentReceipt":item["commentPhotoAcquisition"],"retainedBytesVerified":false,"modelCalled":false,"semanticAcceptance":false,"acquisitionAllowed":ownership.is_none()&&!attempted,"ownershipBlock":ownership})))
}

#[cfg(test)]
#[path="photo_acquisition_comment_tests.rs"]
mod tests;
pub(super) fn preparation_requests(d:&Value,ids:&[Value])->ApiResult<Vec<Value>>{
    let mut requests=Vec::new();let mut seen=BTreeSet::new();
    for id in ids{let id=id.as_str().ok_or_else(||bad("Exact comment photo recipient required"))?;if !seen.insert(id){continue;}
        let item=row(d,"items",id)?;let Some(raw)=item.get("attachments").or_else(||item.get("commentAttachments")).and_then(Value::as_array)else{continue};
        if !raw.iter().any(image){continue;}if current_metadata(d,item).is_ok_and(|metadata|rows(&metadata,"imageFailures").is_empty()){continue;}
        let p=pin(d,item)?;requests.push(json!({"receiptId":crate::id(),"sourceKind":"comment_attachment","itemId":id,"expectedRevision":item["revision"],"expectedSourceVersion":p["sourceVersion"],"attachmentDigest":p["attachmentDigest"]}));
    }Ok(requests)
}
