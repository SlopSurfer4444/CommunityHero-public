//! Original-message source-only fixtures. No provider, model, ASR or real DB.
use super::*;
const AT:&str="2026-10-07T10:00:00Z";
fn fixture()->(Value,Value){
    let mut d=crate::empty();crate::accounts::initialize(&mut d,crate::accounts::Profile::BawRussia).unwrap();
    let attachments=json!([{"type":"photo","url":"https://cdn.example/source.png"},{"type":"sticker","url":"https://cdn.example/source.png"}]);
    d["posts"]=json!([{"id":"post","postKey":"12182:post","text":"Actual post","attachments":[],"attachmentsState":"none"}]);
    d["items"]=json!([{"id":"comment","itemId":"provider-comment","objectId":"12182","conversationKey":"thread","postId":"post","postKey":"12182:post","branchId":"branch","targetId":"message","platform":"VK","revision":1,
        "text":"Original comment","attachments":attachments,"attachmentsState":"present","connectorBinding":d["connectorBinding"]}]);
    d["branches"]=json!([{"id":"branch","postId":"post","messages":[{"id":"message","providerItemId":"provider-comment","providerObjectId":"12182","role":"customer","roleEvidence":"connector-observed","text":"Original comment","attachments":attachments}]}]);
    let p=pin(&d,&d["items"][0]).unwrap();let body=json!({"receiptId":crate::id(),"sourceKind":"comment_attachment","itemId":"comment","expectedRevision":1,"expectedSourceVersion":p["sourceVersion"],"attachmentDigest":p["attachmentDigest"]});(d,body)
}
fn fresh(d:&mut Value,body:&Value)->Claim{match claim(d,body,"owner",AT).unwrap(){Admission::Fresh(c)=>c,_=>panic!("fresh expected")}}
fn outcome(c:&Claim,store:&ArtifactStore)->Value{
    // Unique fixture bytes avoid collisions with destructive tests in the
    // shared private test store. Metadata is synthetic reducer evidence only.
    let r=store.put_bytes(format!("isolated original pixels {}",text(&c.job,"id")).as_bytes()).unwrap();
    json!({"images":rows(&c.pin,"attachments").iter().enumerate().map(|(index,a)|json!({"itemId":c.pin["itemId"],"attachmentIndex":index,"sourceRole":c.pin["sourceRole"],"origin":"comment_attachment","attachmentSha256":hash(a),"sha256":r.sha256,"bytes":r.bytes,"artifact":r.to_json(),"mime":"image/png","width":1,"height":1})).collect::<Vec<_>>(),"failures":[]})
}
#[test]
fn original_comment_receipt_preserves_message_role_slots_and_never_creates_post_or_delivery(){
    let(mut d,b)=fixture();let before=d.clone();let c=fresh(&mut d,&b);let s=store().unwrap();let receipt=commit(&mut d,&c,&outcome(&c,&s),AT,&s).unwrap();
    assert_eq!(receipt["version"],2);assert_eq!(receipt["modelCalled"],false);assert_eq!(receipt["semanticAcceptance"],false);
    assert_eq!(receipt["sourcePin"]["sourceRole"]["role"],"customer");assert_eq!(receipt["sourcePin"]["providerItemId"],"provider-comment");
    assert_eq!(rows(&receipt,"images").len(),2);for image in rows(&receipt,"images"){assert_eq!(image["itemId"],"comment");assert!(image.get("postId").is_none());assert!(image.get("imageNumber").is_none());}
    for key in ["posts","branches","proposals","approvals","operations"]{assert_eq!(d[key],before[key],"{key}");}
    let mut item=d["items"][0].clone();item.as_object_mut().unwrap().remove("commentPhotoAcquisition");assert_eq!(item,before["items"][0]);
    let metadata=current_metadata(&d,&d["items"][0]).unwrap();assert_eq!(metadata["modelCalled"],false);assert_eq!(metadata["imageEvidence"][1]["attachmentIndex"],1);
    let committed=d.clone();assert!(matches!(claim(&mut d,&b,"owner",AT).unwrap(),Admission::Replay(_)));assert_eq!(d,committed);
    let refs=super::super::artifact_refs(&d).unwrap();assert_eq!(refs.len(),1);assert_eq!(s.backup_declaration(&refs).unwrap().objects.len(),1);
}
#[test]
fn comment_acquisition_rechecks_revision_route_role_source_and_owner_before_commit(){
    for fault in ["revision","provider","object","binding","role","message","attachments","head","unknown","owner"]{
        let(mut d,b)=fixture();let c=fresh(&mut d,&b);let s=store().unwrap();let result=outcome(&c,&s);
        match fault{
            "revision"=>d["items"][0]["revision"]=json!(2),"provider"=>d["items"][0]["itemId"]=json!("another-provider-comment"),"object"=>d["items"][0]["objectId"]=json!("another-object"),
            "binding"=>d["items"][0]["connectorBinding"]=crate::accounts::Profile::LikeAvto.binding(),"role"=>d["branches"][0]["messages"][0]["role"]=json!("brand"),
            "message"=>d["branches"][0]["messages"][0]["id"]=json!("another-message"),"attachments"=>d["items"][0]["attachments"][0]["url"]=json!("https://cdn.example/changed.png"),
            "head"=>d["items"][0]["commentPhotoAcquisition"]=json!({"other":"head"}),"unknown"=>list_mut(&mut d,"operations").push(json!({"id":"unresolved","itemId":"comment","status":"unknown"})),_=>d["jobs"][0]["status"]=json!("unknown")
        }
        let before=d.clone();assert!(commit(&mut d,&c,&result,AT,&s).is_err(),"{fault}");assert_eq!(d,before,"{fault}");
    }
}
#[test]
fn comment_receipt_rejects_wrong_role_foreign_post_duplicate_slot_and_corrupt_retained_bytes(){
    let(mut d,b)=fixture();let c=fresh(&mut d,&b);let s=store().unwrap();let r=commit(&mut d,&c,&outcome(&c,&s),AT,&s).unwrap();
    for fault in ["role","post","duplicate","slot","source"]{
        let mut forged=r.clone();match fault{
            "role"=>forged["images"][0]["sourceRole"]["role"]=json!("brand"),"post"=>forged["images"][0]["postId"]=json!("post"),
            "duplicate"=>{forged["images"][1]=forged["images"][0].clone();},"slot"=>forged["images"][0]["attachmentIndex"]=json!(7),_=>forged["sourcePin"]["objectId"]=json!("foreign")
        }forged["sourceDigest"]=json!(hash(&forged["sourcePin"]));forged["receiptSha256"]=json!(hash(&unsigned(&forged)));
        assert!(validate_receipt(&d,&d["items"][0],&forged,&s).is_err(),"{fault}");
    }
    let reference=image_meta(&r["images"][0]).unwrap();std::fs::write(s.path(&reference).unwrap(),vec![b'x';reference.bytes as usize]).unwrap();
    assert!(current_metadata(&d,&d["items"][0]).is_err());assert_eq!(d["items"][0]["commentPhotoAcquisition"],r,"retained history survives a rejected lookup");
}
#[test]
fn missing_or_ambiguous_message_and_unknown_attempt_never_join_or_retry(){
    for fault in ["missing","duplicate","null","conflicting","unknown-metadata","bad-role-evidence"]{
        let(mut d,b)=fixture();match fault{
            "missing"=>d["branches"][0]["messages"]=json!([]),"duplicate"=>{let m=d["branches"][0]["messages"][0].clone();d["branches"][0]["messages"].as_array_mut().unwrap().push(m);},
            "null"=>{d["items"][0]["targetId"]=Value::Null;d["branches"][0]["messages"]=json!([{"role":"customer"}]);},
            "conflicting"=>d["items"][0]["commentAttachments"]=json!([]),"unknown-metadata"=>d["items"][0]["attachmentsState"]=json!("unknown"),_=>d["branches"][0]["messages"][0]["roleEvidence"]=json!({"invented":true})
        }let before=d.clone();assert!(claim(&mut d,&b,"owner",AT).is_err(),"{fault}");assert_eq!(d,before);
    }
    let(mut d,b)=fixture();let _=fresh(&mut d,&b);d["jobs"][0]["status"]=json!("unknown");let before=d.clone();assert!(matches!(claim(&mut d,&b,"owner",AT).unwrap(),Admission::Replay(_)));assert_eq!(d,before);
    let mut another=b.clone();another["receiptId"]=json!(crate::id());assert!(claim(&mut d,&another,"owner",AT).is_err());assert_eq!(d,before);
}

#[tokio::test]
async fn explicit_fixture_cas_is_task_local_and_does_not_change_default_store_or_environment(){
    let root=tempfile::tempdir().unwrap();let s=ArtifactStore::open(root.path()).unwrap();let(mut d,b)=fixture();let c=fresh(&mut d,&b);
    let receipt=commit(&mut d,&c,&outcome(&c,&s),AT,&s).unwrap();
    assert!(current_metadata(&d,&d["items"][0]).is_err(),"default private store does not acquire foreign objects");
    super::super::with_fixture_store(&s,async{
        assert_eq!(current_metadata(&d,&d["items"][0]).unwrap()["acquisitionReceiptSha256"],receipt["receiptSha256"]);
        assert!(tokio::spawn(async{store().unwrap().root().to_owned()}).await.unwrap()!=root.path(),"spawned tasks do not inherit fixture store authority");
    }).await;
    assert!(current_metadata(&d,&d["items"][0]).is_err(),"completed fixture scope restores ordinary private lookup");
}
