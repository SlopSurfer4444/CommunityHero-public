//! Delivery proof bound AFTER immutable paid capture. Never a retry entitlement.
use serde_json::{json,Value};
use crate::media_artifacts::ArtifactRef;
use crate::preparation_materials::{rows,hash,CONTRACT};
fn sha(v:&Value)->bool{v.as_str().is_some_and(|s|s.len()==64&&s.bytes().all(|b|b.is_ascii_digit()||(b'a'..=b'f').contains(&b)))}
pub(crate) fn payload(v:&Value)->&Value{if v["request"].is_object(){&v["request"]}else{v}}
pub(crate) fn expected(request:&Value)->Value{
    let bundle=&request["postContextBundle"];
    let pins:Vec<_>=rows(bundle,"members").iter().map(|m|json!({"postId":m["canonicalPostId"],"connectorBinding":m["connectorBinding"],"sourceVersion":m["postSourceVersion"],"postFieldsSha256":hash(&m["fields"])})).collect();
    let photos:Vec<_>=rows(bundle,"members").iter().flat_map(|m|rows(m,"assets").iter().filter(|a|a["modality"]=="photo").map(move|a|json!({
        "postId":m["canonicalPostId"],"attachmentIndex":a["attachmentIndex"],"attachmentIdentity":a["attachmentIdentity"],"sourceVersion":a["sourceVersion"],
        "artifact":a["photo"]["artifact"],"acquisitionReceiptSha256":a["acquisitionReceiptSha256"]}))).collect();
    let speech:Vec<_>=rows(bundle,"members").iter().flat_map(|m|rows(m,"assets").iter().filter(|a|a["modality"]=="video").map(move|a|json!({
        "postId":m["canonicalPostId"],"attachmentIndex":a["attachmentIndex"],"attachmentIdentity":a["attachmentIdentity"],"sourceVersion":a["sourceVersion"],
        "materialId":a["speech"]["materialId"],"materialSha256":a["speech"]["materialSha256"],"outcome":a["speech"]["outcome"],"coverage":a["speech"]["coverage"]}))).collect();
    let mut expected=json!({"companyId":bundle["companyId"],"postContextBundleSha256":bundle["contentSha256"],"memberPins":pins,"requiredPhotos":photos,"suppliedSpeech":speech});
    if !rows(bundle,"commentPhotos").is_empty(){expected["requiredCommentPhotos"]=json!(rows(bundle,"commentPhotos").iter().map(|source|json!({"itemId":source["itemId"],"attachmentIndex":source["attachmentIndex"],
        "attachmentIdentity":source["attachmentIdentity"],"sourceRole":source["sourceRole"],"sourceVersion":source["sourceVersion"],"artifact":source["photo"]["artifact"],"acquisitionReceiptSha256":source["acquisitionReceiptSha256"]})).collect::<Vec<_>>());}expected
}
pub(crate) fn observed_expected(body:&Value)->Value{
    let mut observed=json!({"companyId":body["companyId"],"postContextBundleSha256":body["postContextBundleSha256"],"memberPins":body["memberPins"],"requiredPhotos":body["requiredPhotos"],"suppliedSpeech":body["suppliedSpeech"]});
    if let Some(comment)=body.get("requiredCommentPhotos"){observed["requiredCommentPhotos"]=comment.clone();}observed
}
pub(crate) fn validate_result(request:&Value,result:&Value)->Result<Value,&'static str>{
    let request=payload(request);
    if !crate::preparation_materials::enabled(request){return Ok(Value::Null);}
    if result["decisionSource"]=="deterministic_media_source_gap"&&result["proposals"]==json!([])&&result.get("runMetadata").is_none(){return Ok(Value::Null);}
    let company=&request["postContextBundle"]["companyId"];
    let profile=crate::accounts::Profile::from_workspace(&json!({"account":company})).map_err(|_|"mandatory_material_company_invalid")?;
    if request["account"]!=profile.key()&&request["account"]!=profile.display()
        ||request["connectorBinding"]["accountId"]!=*company||rows(&request["postContextBundle"],"members").iter().any(|m|m["connectorBinding"]["accountId"]!=*company){return Err("mandatory_material_company_binding_mismatch");}
    let body=&result["runMetadata"]["materialInvocation"];
    if body["schemaVersion"]!=1||body["contract"]!=CONTRACT||body["completenessStatus"]!="complete"
        ||body.get("paidResultRef").is_some()||body.get("receiptArtifactRef").is_some()
        ||request["materialReadiness"]["status"]!="ready"{return Err("mandatory_material_invocation_missing_or_incomplete");}
    let expected=expected(request);
    for key in ["companyId","postContextBundleSha256","memberPins","requiredPhotos","suppliedSpeech"]{
        if body[key]!=expected[key]{return Err("mandatory_material_invocation_binding_mismatch");}
    }
    if expected.get("requiredCommentPhotos")!=body.get("requiredCommentPhotos"){return Err("mandatory_comment_photo_invocation_binding_mismatch");}
    if let Some(required)=expected.get("requiredCommentPhotos"){
        let delivered=rows(body,"deliveredCommentPhotos");
        if !body["deliveredCommentPhotos"].is_array()||body["stagedCommentPhotos"]!=body["deliveredCommentPhotos"]||delivered.len()!=required.as_array().ok_or("mandatory_comment_photo_expected_invalid")?.len(){return Err("mandatory_comment_photo_delivery_incomplete");}
        for source in required.as_array().unwrap(){
            let matching=delivered.iter().filter(|image|image["origin"]=="comment_attachment"&&image["itemId"]==source["itemId"]&&image["attachmentIndex"]==source["attachmentIndex"]).collect::<Vec<_>>();
            let captured=rows(request,"commentPhotoSources").iter().filter(|photo|photo["itemId"]==source["itemId"]&&photo["attachmentIndex"]==source["attachmentIndex"]).collect::<Vec<_>>();
            if matching.len()!=1||matching[0]["sha256"]!=source["artifact"]["sha256"]||matching[0]["bytes"]!=source["artifact"]["bytes"]
                ||matching[0]["sourceRole"]!=source["sourceRole"]||matching[0]["acquisitionReceiptSha256"]!=source["acquisitionReceiptSha256"]||matching[0]["sourceVersion"]!=source["sourceVersion"]
                ||matching[0].get("postId").is_some()||matching[0].get("itemIds").is_some()
                ||matching[0]["imageNumber"].as_u64().is_none_or(|n|n==0||n>16)||captured.len()!=1
                ||["mime","width","height"].iter().any(|key|matching[0][*key]!=captured[0]["photo"][*key])
                ||rows(&result["runMetadata"],"imageEvidence").iter().filter(|image|image==&matching[0]).count()!=1{return Err("mandatory_comment_photo_delivery_mismatch");}
        }
    }else if body.get("stagedCommentPhotos").is_some()||body.get("deliveredCommentPhotos").is_some(){return Err("mandatory_comment_photo_invocation_binding_mismatch");
    }
    for key in ["actualTextInputSha256","instructionSha256","schemaSha256","cliSha256"]{if !sha(&body[key]){return Err("mandatory_material_invocation_digest_invalid");}}
    if body["actualTextInputSha256"]!=result["runMetadata"]["inputSha256"]||body["instructionSha256"]!=result["runMetadata"]["instructionSha256"]
        ||body["cliSha256"]!=result["runMetadata"]["cliSha256"]{return Err("mandatory_material_actual_input_mismatch");}
    let delivered=rows(body,"deliveredPhotos");
    let staged=rows(body,"stagedPhotos");
    if !body["deliveredPhotos"].is_array()||staged!=delivered||delivered.len()!=rows(body,"requiredPhotos").len(){return Err("mandatory_photo_delivery_incomplete");}
    for required in rows(body,"requiredPhotos"){
        let matches:Vec<_>=delivered.iter().filter(|im|im["postId"]==required["postId"]&&im["attachmentIndex"]==required["attachmentIndex"]).collect();
        if matches.len()!=1||matches[0]["sha256"]!=required["artifact"]["sha256"]||matches[0]["bytes"]!=required["artifact"]["bytes"]
            ||matches[0]["width"].as_u64().is_none_or(|n|n==0)||matches[0]["height"].as_u64().is_none_or(|n|n==0){return Err("mandatory_photo_delivery_mismatch");}
    }
    if body["optionalFrameRefs"]!=request.get("optionalFrameRefs").cloned().unwrap_or_else(||json!([])){
        return Err("optional_frame_delivery_mismatch");
    }
    let requested=rows(request,"optionalFrameRefs");let frames=rows(body,"deliveredFrames");
    if !requested.is_empty()&&(!body["deliveredFrames"].is_array()||body["stagedFrames"]!=body["deliveredFrames"]||frames.len()!=requested.len()) {return Err("optional_frame_delivery_incomplete");}
    for reference in requested {
        let matching=frames.iter().filter(|frame|frame["refSha256"]==hash(reference)).collect::<Vec<_>>();
        if matching.len()!=1||["sha256","mime","width","height","requestedTimestampMs","actualPts","timeBase"].iter().any(|key|matching[0][*key]!=reference[*key])
            ||matching[0]["bytes"]!=reference["artifact"]["bytes"]{return Err("optional_frame_delivery_mismatch");}
    }
    Ok(body.clone())
}
fn unsigned(pointer:&Value)->Value{let mut p=pointer.clone();if let Some(o)=p.as_object_mut(){o.remove("pointerSha256");}p}
pub(crate) fn validate_pointer(pointer:&Value)->Result<(),&'static str>{
    if pointer["schemaVersion"]!=1||pointer["contract"]!=CONTRACT||pointer["pointerSha256"]!=hash(&unsigned(pointer))
        ||pointer["bodySha256"]!=hash(&pointer["body"])||pointer["companyId"]!=pointer["body"]["companyId"]
        ||pointer["nativeJobId"].as_str().is_none_or(str::is_empty)||pointer["paidResultRef"]["binding"]["nativeJobId"]!=pointer["nativeJobId"]
        ||pointer["paidResultRef"]["account"]!=pointer["companyId"]||pointer["paidResultRef"]["retryAuthorized"]!=false||pointer["paidResultRef"]["dispatchAuthorized"]!=false
        ||ArtifactRef::from_json(&pointer["artifact"]).is_err(){return Err("mandatory_material_receipt_invalid");}Ok(())
}
pub(crate) async fn retain(app:&crate::App,request:&Value,result:&Value,paid:&Value)->crate::ApiResult<Option<Value>>{
    let body=validate_result(request,result).map_err(crate::bad)?;
    if body.is_null(){return Ok(None);}
    if paid["responseSha256"]!=hash(result)||paid["requestSha256"]!=hash(request)
        ||paid["binding"]["nativeJobId"].as_str().is_none_or(str::is_empty){return Err(crate::conflict("Paid material receipt capture binding changed"));}
    let store=crate::media_fullframes::store().map_err(|_|crate::internal("Material receipt store unavailable"))?;
    let record=json!({"schemaVersion":1,"contract":CONTRACT,"request":request,"body":body,"paidResultRef":paid});
    let artifact=tokio::task::spawn_blocking(move||store.put_bytes(record.to_string().as_bytes())).await
        .map_err(|_|crate::internal("Material receipt writer interrupted"))?.map_err(|_|crate::internal("Material receipt retention failed"))?;
    let mut pointer=json!({"schemaVersion":1,"contract":CONTRACT,"companyId":body["companyId"],"nativeJobId":paid["binding"]["nativeJobId"],
        "bodySha256":hash(&body),"body":body,"paidResultRef":paid,"artifact":artifact.to_json()});
    pointer["pointerSha256"]=json!(hash(&pointer));validate_pointer(&pointer).map_err(crate::bad)?;
    Ok(Some(pointer))
}
pub(crate) fn attach(d:&mut Value,job:&str,pointer:&Value)->crate::ApiResult<()>{
    validate_pointer(pointer).map_err(crate::bad)?;
    if pointer["nativeJobId"]!=job||pointer["companyId"]!=d["account"]{return Err(crate::conflict("Material receipt job/company changed"));}
    let record=crate::row_mut(d,"jobs",job)?;
    if !rows(record,"retainedEvidence").contains(&pointer["paidResultRef"]){return Err(crate::conflict("Material receipt lacks attached original paid result"));}
    if record.get("modelMaterialReceipts").is_none(){record["modelMaterialReceipts"]=json!([]);}
    let receipts=record["modelMaterialReceipts"].as_array_mut().ok_or_else(||crate::conflict("Invalid material receipt history"))?;
    if receipts.contains(pointer){return Ok(());}
    if receipts.iter().any(|p|p["paidResultRef"]==pointer["paidResultRef"]){return Err(crate::conflict("Material receipt immutable capture changed"));}
    receipts.push(pointer.clone());record["modelMaterialReceipt"]=pointer.clone();Ok(())
}
pub(crate) fn result_receipt(request:&Value,result:&Value)->Result<Option<Value>,&'static str>{
    if !crate::preparation_materials::enabled(payload(request)){return Ok(None);}
    if result["decisionSource"]=="deterministic_media_source_gap"&&result["proposals"]==json!([])&&result.get("runMetadata").is_none(){return Ok(None);}
    let body=validate_result(request,result)?;let pointer=&result["modelMaterialReceipt"];validate_pointer(pointer)?;
    if pointer["body"]!=body{return Err("mandatory_material_receipt_body_changed");}Ok(Some(pointer.clone()))
}
/// Lost receipt ACK repairs evidence from the ORIGINAL immutable paid capture.
/// Never invokes a model, resets a paid intent, or changes original capture bytes.
pub(crate) async fn recover(app:&crate::App,job:&str,paid:&Value)->crate::ApiResult<Value>{
    let captured=crate::runtime_paid_result::resolve(app,job,"assistant",paid).await?;
    let mut result=captured["response"].clone();
    let pointer=retain(app,&captured["request"],&result,paid).await?.ok_or_else(||crate::conflict("Legacy capture has no mandatory material invocation"))?;
    app.change_job(job,|d|attach(d,job,&pointer)).await?;
    result["modelMaterialReceipt"]=pointer;Ok(result)
}
pub(crate) fn validate_change(before:&Value,after:&Value)->crate::ApiResult<()> {
    let (old,new)=match (checked_jobs(before)?,checked_jobs(after)?){(None,None)=>return Ok(()),(Some(a),Some(b))=>(a,b),_=>return Err(crate::internal("Material receipt job scope changed"))};
    for job in old{let Some(receipts)=checked_receipts(job)? else{continue;};
        let matching=new.iter().filter(|j|j["id"]==job["id"]).collect::<Vec<_>>();
        if matching.len()!=1||checked_receipts(matching[0])?.is_none_or(|next|!next.starts_with(receipts)){return Err(crate::conflict("Material receipt history cannot be deleted or rebound"));}
    }
    let mut jobs=std::collections::BTreeSet::new();
    for job in new{let id=job["id"].as_str().ok_or_else(||crate::bad("Material receipt job identity missing"))?;
        if !jobs.insert(id){return Err(crate::bad("Material receipt job identity duplicated"));}
        let Some(receipts)=checked_receipts(job)? else{continue;};
        let retained=job["retainedEvidence"].as_array().ok_or_else(||crate::bad("Material receipt paid history invalid"))?;
        let mut paid=std::collections::BTreeSet::new();
        for pointer in receipts{validate_pointer(pointer).map_err(crate::bad)?;
            if pointer["nativeJobId"]!=id||pointer["companyId"]!=after["account"]||!retained.contains(&pointer["paidResultRef"])
                ||!paid.insert(hash(&pointer["paidResultRef"])){return Err(crate::conflict("Material receipt durable ownership changed or duplicated"));}
        }
        if let Some(last)=receipts.last(){if job["modelMaterialReceipt"]!=*last{return Err(crate::conflict("Material receipt latest pointer changed"));}}
    }Ok(())
}
fn checked_jobs(v:&Value)->crate::ApiResult<Option<&[Value]>>{match v.get("jobs"){None=>Ok(None),Some(v)=>v.as_array().map(|v|Some(v.as_slice())).ok_or_else(||crate::bad("Material receipt jobs must be an array"))}}
fn checked_receipts(job:&Value)->crate::ApiResult<Option<&[Value]>>{match job.get("modelMaterialReceipts"){None=>Ok(None),Some(v)=>v.as_array().map(|v|Some(v.as_slice())).ok_or_else(||crate::bad("Material receipts must be an array"))}}
pub(crate) fn artifact_refs(d:&Value)->crate::ApiResult<Vec<ArtifactRef>> {
    let mut refs=Vec::new();let mut seen=std::collections::BTreeMap::new();
    for job in checked_jobs(d)?.unwrap_or(&[]){for pointer in checked_receipts(job)?.unwrap_or(&[]){
        validate_pointer(pointer).map_err(crate::bad)?;let reference=ArtifactRef::from_json(&pointer["artifact"]).map_err(|_|crate::bad("Material receipt artifact invalid"))?;
        match seen.get(&reference.sha256){Some(bytes)if *bytes!=reference.bytes=>return Err(crate::conflict("Material receipt artifact size conflict")),Some(_)=>{},None=>{seen.insert(reference.sha256.clone(),reference.bytes);refs.push(reference);}}
    }}Ok(refs)
}
#[cfg(test)] #[path="model_material_receipt_tests.rs"] mod tests;
/// Pure native tests use real PRIVATE CAS objects and the normal validators.
/// Synthetic SHA/input observations are fixture data, never live delivery proof.
/// This helper is not compiled into product binaries. Company identity derives
/// from a validated isolated workspace; no deployed state/environment is read.
#[cfg(test)]
pub(crate) fn fixture_result(d:&mut Value,job:&str,request:&Value,result:&mut Value)->crate::ApiResult<()> {
    if !crate::preparation_materials::enabled(payload(request)){return Ok(());}
    let profile=crate::accounts::Profile::from_workspace(d)?;let binding=crate::active_binding(d)?.to_json();let native=payload(request);
    if native["postContextBundle"]["companyId"]!=profile.display()||native["connectorBinding"]!=binding
        ||native["account"]!=profile.display()&&native["account"]!=profile.key(){return Err(crate::bad("Material fixture request/company/binding mismatch"));}
    static ROOT:std::sync::OnceLock<tempfile::TempDir>=std::sync::OnceLock::new();
    let store=crate::media_artifacts::ArtifactStore::open(ROOT.get_or_init(||tempfile::tempdir().unwrap()).path()).map_err(|_|crate::internal("Fixture CAS unavailable"))?;
    let mut body=expected(payload(request));body["schemaVersion"]=json!(1);body["contract"]=json!(CONTRACT);body["completenessStatus"]=json!("complete");
    body["actualTextInputSha256"]=result["runMetadata"]["inputSha256"].clone();body["instructionSha256"]=result["runMetadata"]["instructionSha256"].clone();
    body["cliSha256"]=result["runMetadata"]["cliSha256"].clone();body["schemaSha256"]=json!("f".repeat(64));
    let photos=rows(&body,"requiredPhotos").iter().map(|r|{
        let photo=rows(&payload(request)["postContextBundle"],"members").iter().find(|m|m["canonicalPostId"]==r["postId"]).and_then(|m|rows(m,"assets").iter().find(|a|a["attachmentIndex"]==r["attachmentIndex"]))
            .map(|a|&a["photo"]).unwrap();
        json!({"postId":r["postId"],"attachmentIndex":r["attachmentIndex"],"sha256":r["artifact"]["sha256"],"bytes":r["artifact"]["bytes"],"width":photo["width"],"height":photo["height"]})
    }).collect::<Vec<_>>();
    body["stagedPhotos"]=json!(photos);body["deliveredPhotos"]=body["stagedPhotos"].clone();body["optionalFrameRefs"]=payload(request).get("optionalFrameRefs").cloned().unwrap_or(json!([]));
    if body.get("requiredCommentPhotos").is_some(){
        let comments=rows(payload(request),"commentPhotoSources").iter().enumerate().map(|(index,source)|json!({"imageNumber":index+1,"origin":"comment_attachment",
            "itemId":source["itemId"],"attachmentIndex":source["attachmentIndex"],"sha256":source["photo"]["artifact"]["sha256"],"bytes":source["photo"]["artifact"]["bytes"],
            "mime":source["photo"]["mime"],"width":source["photo"]["width"],"height":source["photo"]["height"],"sourceRole":source["sourceRole"],
            "sourceVersion":source["sourceVersion"],"acquisitionReceiptSha256":source["acquisitionReceiptSha256"]})).collect::<Vec<_>>();
        body["stagedCommentPhotos"]=json!(comments);body["deliveredCommentPhotos"]=body["stagedCommentPhotos"].clone();result["runMetadata"]["imageEvidence"]=body["stagedCommentPhotos"].clone();
    }
    body["stagedFrames"]=json!(rows(payload(request),"optionalFrameRefs").iter().map(|r|json!({"refSha256":hash(r),"sha256":r["sha256"],"mime":r["mime"],"width":r["width"],"height":r["height"],"bytes":r["artifact"]["bytes"],"requestedTimestampMs":r["requestedTimestampMs"],"actualPts":r["actualPts"],"timeBase":r["timeBase"]})).collect::<Vec<_>>());body["deliveredFrames"]=body["stagedFrames"].clone();
    result["runMetadata"]["materialInvocation"]=body.clone();validate_result(request,result).map_err(crate::bad)?;
    let owner=json!({"account":profile.display(),"runtimeId":"isolated-material-fixture","releaseSha256":"e".repeat(64)});let mut wire=request.clone();profile.bind_request(&mut wire)?;wire["operation"]=json!("assistant");
    let record=json!({"version":2,"kind":"retained-native-paid-stage-result","company":profile.key(),"account":profile.display(),"runtimeOwner":owner,"binding":{"nativeJobId":job,"operation":"assistant"},
        "requestSha256":hash(&wire),"responseSha256":hash(result),"request":wire,"response":result,"retryAuthorized":false,"dispatchAuthorized":false});
    let artifact=store.put_bytes(record.to_string().as_bytes()).map_err(|_|crate::internal("Fixture paid CAS failed"))?;
    let paid=json!({"version":1,"kind":"native-paid-capture-ref","company":profile.key(),"account":profile.display(),"binding":record["binding"],"runtimeOwner":owner,
        "requestSha256":record["requestSha256"],"responseSha256":record["responseSha256"],"artifact":artifact.to_json(),"retryAuthorized":false,"dispatchAuthorized":false});
    if let Err(error)=crate::runtime_paid_result::resolve_from(&store,profile.key(),profile.display(),Some(job),"assistant",&paid){
        // Only isolated test diagnostics: never dump request/response fields.
        // Compare the exact serialized roundtrip so float/hash drift can be
        // separated from the product's credential/identity checks.
        let parsed:Value=serde_json::from_slice(&store.read_bytes(&artifact,128*1024*1024).map_err(|_|crate::internal("Fixture paid CAS readback failed"))?).map_err(|_|crate::internal("Fixture paid CAS parse failed"))?;
        return Err(crate::ApiError(error.0,format!("{}; fixture requestStable={} responseStable={} ownerStable={} recordStable={}",error.1,
            hash(&parsed["request"])==record["requestSha256"],hash(&parsed["response"])==record["responseSha256"],parsed["runtimeOwner"]==owner,parsed==record)));
    }
    let retained=crate::row_mut(d,"jobs",job)?;
    if retained.get("retainedEvidence").is_none(){retained["retainedEvidence"]=json!([]);}
    retained["retainedEvidence"].as_array_mut().ok_or_else(||crate::bad("Fixture paid history malformed"))?.push(paid.clone());
    let receipt_record=json!({"schemaVersion":1,"contract":CONTRACT,"request":wire,"body":body,"paidResultRef":paid});
    let receipt_artifact=store.put_bytes(receipt_record.to_string().as_bytes()).map_err(|_|crate::internal("Fixture material CAS failed"))?;
    let mut pointer=json!({"schemaVersion":1,"contract":CONTRACT,"companyId":profile.display(),"nativeJobId":job,"bodySha256":hash(&body),"body":body,"paidResultRef":paid,"artifact":receipt_artifact.to_json()});
    pointer["pointerSha256"]=json!(hash(&pointer));attach(d,job,&pointer)?;result["modelMaterialReceipt"]=pointer;Ok(())
}
