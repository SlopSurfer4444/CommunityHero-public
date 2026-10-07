use super::*;
fn fixture(profile:crate::accounts::Profile)->(Value,Value){
    let mut request=json!({"account":profile.display(),"connectorBinding":profile.binding(),"mandatoryMaterialContract":CONTRACT,
        "materialReadiness":{"status":"ready"},"postContextBundle":{"companyId":profile.display(),"contentSha256":"a".repeat(64),
        "members":[{"canonicalPostId":"p","connectorBinding":profile.binding(),"postSourceVersion":"b".repeat(64),"assets":[]}]}});
    profile.bind_request(&mut request).unwrap();
    let mut body=expected(&request);body["schemaVersion"]=json!(1);body["contract"]=json!(CONTRACT);body["completenessStatus"]=json!("complete");
    for field in ["actualTextInputSha256","instructionSha256","schemaSha256","cliSha256"]{body[field]=json!("c".repeat(64));}
    body["stagedPhotos"]=json!([]);body["deliveredPhotos"]=json!([]);body["optionalFrameRefs"]=json!([]);
    let result=json!({"runMetadata":{"materialInvocation":body,"inputSha256":"c".repeat(64),"instructionSha256":"c".repeat(64),"cliSha256":"c".repeat(64)}});(request,result)
}
#[test]fn actual_bridge_normalization_preserves_canonical_company_for_both_profiles(){
    for profile in [crate::accounts::Profile::BawRussia,crate::accounts::Profile::LikeAvto]{let(request,result)=fixture(profile);assert_eq!(validate_result(&request,&result).unwrap()["companyId"],profile.display());}
}
#[test]fn foreign_transport_and_delivery_omission_are_rejected(){
    let(mut request,mut result)=fixture(crate::accounts::Profile::BawRussia);request["account"]=json!("likeavto");assert_eq!(validate_result(&request,&result),Err("mandatory_material_company_binding_mismatch"));
    let(request,original)=fixture(crate::accounts::Profile::BawRussia);result=original;result["runMetadata"]["materialInvocation"]["deliveredPhotos"]=json!([{"postId":"other"}]);assert_eq!(validate_result(&request,&result),Err("mandatory_photo_delivery_incomplete"));
}
#[test]fn malformed_present_receipt_history_and_omitted_job_side_are_rejected(){
    assert!(validate_change(&json!({"jobs":null}),&json!({"jobs":[]})).is_err());
    assert!(validate_change(&json!({}),&json!({"jobs":[]})).is_err());
    let malformed=json!({"jobs":[{"id":"j","modelMaterialReceipts":{}}]});assert!(validate_change(&malformed,&malformed).is_err());
    assert!(artifact_refs(&malformed).is_err());
}

fn comment_fixture()->(Value,Value){
    let(mut request,mut result)=fixture(crate::accounts::Profile::BawRussia);
    let role=json!({"role":"customer","messageId":"original-message","roleEvidence":"connector-observed"});
    let artifact=json!({"sha256":"d".repeat(64),"bytes":69});
    let photo=json!({"origin":"comment_attachment","itemId":"i","attachmentIndex":0,"sourceRole":role,"sourceVersion":"e".repeat(64),"acquisitionReceiptSha256":"f".repeat(64),"artifact":artifact,"sha256":"d".repeat(64),"bytes":69,"mime":"image/png","width":1,"height":1});
    let source=json!({"itemId":"i","attachmentIndex":0,"attachmentIdentity":"9".repeat(64),"sourceRole":role,"sourceVersion":"e".repeat(64),"acquisitionReceiptSha256":"f".repeat(64),"photo":photo});
    request["postContextBundle"]["commentPhotos"]=json!([source.clone()]);request["commentPhotoSources"]=json!([source]);
    let expected=expected(&request);let body=&mut result["runMetadata"]["materialInvocation"];for(key,value)in expected.as_object().unwrap(){body[key]=value.clone();}
    let mut delivered=photo;delivered.as_object_mut().unwrap().remove("artifact");delivered["imageNumber"]=json!(1);
    body["stagedCommentPhotos"]=json!([delivered.clone()]);body["deliveredCommentPhotos"]=json!([delivered.clone()]);result["runMetadata"]["imageEvidence"]=json!([delivered]);(request,result)
}
#[test]fn original_comment_delivery_requires_exact_current_source_role_dimensions_and_unique_observation(){
    let(request,result)=comment_fixture();let body=validate_result(&request,&result).unwrap();assert_eq!(body["requiredCommentPhotos"].as_array().unwrap().len(),1);
    for fault in ["role","source","receipt","slot","item","post","bytes","width","mime","missing","duplicate","observation"]{
        let mut changed=result.clone();match fault{
            "role"=>changed["runMetadata"]["materialInvocation"]["deliveredCommentPhotos"][0]["sourceRole"]["role"]=json!("brand"),
            "source"=>changed["runMetadata"]["materialInvocation"]["deliveredCommentPhotos"][0]["sourceVersion"]=json!("0".repeat(64)),
            "receipt"=>changed["runMetadata"]["materialInvocation"]["deliveredCommentPhotos"][0]["acquisitionReceiptSha256"]=json!("0".repeat(64)),
            "slot"=>changed["runMetadata"]["materialInvocation"]["deliveredCommentPhotos"][0]["attachmentIndex"]=json!(1),
            "item"=>changed["runMetadata"]["materialInvocation"]["deliveredCommentPhotos"][0]["itemId"]=json!("foreign"),
            "post"=>changed["runMetadata"]["materialInvocation"]["deliveredCommentPhotos"][0]["postId"]=json!("fake-post"),
            "bytes"=>changed["runMetadata"]["materialInvocation"]["deliveredCommentPhotos"][0]["bytes"]=json!(68),
            "width"=>changed["runMetadata"]["materialInvocation"]["deliveredCommentPhotos"][0]["width"]=json!(2),
            "mime"=>changed["runMetadata"]["materialInvocation"]["deliveredCommentPhotos"][0]["mime"]=json!("image/jpeg"),
            "missing"=>changed["runMetadata"]["materialInvocation"]["deliveredCommentPhotos"]=json!([]),
            "duplicate"=>{let row=changed["runMetadata"]["materialInvocation"]["deliveredCommentPhotos"][0].clone();changed["runMetadata"]["materialInvocation"]["deliveredCommentPhotos"].as_array_mut().unwrap().push(row);},
            _=>changed["runMetadata"]["imageEvidence"]=json!([])
        }
        // Keep staged/delivered and raw provenance consistent so the failing
        // assertion exercises native source binding, rather than a JSON diff.
        if fault!="observation"{changed["runMetadata"]["materialInvocation"]["stagedCommentPhotos"]=changed["runMetadata"]["materialInvocation"]["deliveredCommentPhotos"].clone();changed["runMetadata"]["imageEvidence"]=changed["runMetadata"]["materialInvocation"]["deliveredCommentPhotos"].clone();}
        assert!(validate_result(&request,&changed).is_err(),"{fault}");
    }
    let mut no_requirement=request.clone();no_requirement["postContextBundle"].as_object_mut().unwrap().remove("commentPhotos");assert!(validate_result(&no_requirement,&result).is_err());
}
