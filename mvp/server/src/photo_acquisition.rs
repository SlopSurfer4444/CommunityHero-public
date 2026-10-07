//! Source-owned acquisition observations. No model, editorial, or send authority.
//! One durable attempt per exact source pin; interrupted attempts never auto-retry.
use crate::*;
use crate::media_artifacts::{ArtifactRef, ArtifactStore};
use std::{collections::BTreeSet, io::Read, path::{Path as FsPath, PathBuf}};
#[path="photo_acquisition_comment.rs"]
mod comment;
pub(crate) use comment::{current_metadata as current_comment_metadata,preflight as comment_preflight,pin as comment_pin};

const MAX_IMAGE:u64=8*1024*1024;
#[cfg(test)]
tokio::task_local!{static FIXTURE_STORE:ArtifactStore;}
/// Opt-in isolated release corpus scope. Ordinary tests still use their private
/// TempDir; neither product binaries nor deployed environment select this path.
#[cfg(test)]
pub(crate) async fn with_fixture_store<T>(store:&ArtifactStore,future:impl std::future::Future<Output=T>)->T{
    FIXTURE_STORE.scope(store.clone(),future).await
}
fn store()->Result<ArtifactStore,String>{
    #[cfg(not(test))]
    {media_fullframes::store()}
    #[cfg(test)]
    {
        if let Ok(store)=FIXTURE_STORE.try_with(Clone::clone){return Ok(store);}
        // Destructive synthetic tests must NEVER honor a deployed evidence
        // environment. This module's complete test path owns a private store.
        static ROOT:std::sync::OnceLock<tempfile::TempDir>=std::sync::OnceLock::new();
        ArtifactStore::open(ROOT.get_or_init(||tempfile::tempdir().unwrap()).path()).map_err(|_|"photo_test_store_unavailable".to_owned())
    }
}
fn hash(v:&Value)->String{media_fullframes::hash(v)}
fn text<'a>(v:&'a Value,k:&str)->&'a str{v[k].as_str().unwrap_or("")}
fn rows<'a>(v:&'a Value,k:&str)->&'a [Value]{v[k].as_array().map(Vec::as_slice).unwrap_or(&[])}
fn sha(v:&Value)->bool{v.as_str().is_some_and(|s|s.len()==64&&s.bytes().all(|b|b.is_ascii_digit()||(b'a'..=b'f').contains(&b)))}
fn fields(v:&Value,keys:&[&str])->ApiResult<()>{
    if v.as_object().is_none_or(|o|o.len()!=keys.len()||o.keys().any(|k|!keys.contains(&k.as_str()))){return Err(bad("Unexpected photo acquisition fields"));}Ok(())
}
fn parse(body:&Value)->ApiResult<()>{
    if body.get("recipients").is_some(){
        fields(body,&["receiptId","postId","expectedSourceVersion","attachmentDigest","recipients"])?;
        let recipients=body["recipients"].as_array().filter(|r|!r.is_empty()&&r.len()<=100).ok_or_else(||bad("Choose 1 to 100 exact current photo recipients"))?;
        let mut seen=BTreeSet::new();
        for recipient in recipients{
            fields(recipient,&["itemId","expectedRevision"])?;let item_id=text(recipient,"itemId");
            if item_id.is_empty()||item_id.len()>128||item_id.trim()!=item_id||item_id.chars().any(|c|c.is_control()||c==',')
                ||!seen.insert(item_id)||recipient["expectedRevision"].as_u64().is_none_or(|n|n==0){return Err(bad("Distinct canonical photo recipients and positive revisions required"));}
        }
    }else{fields(body,&["receiptId","postId","expectedSourceVersion","attachmentDigest"])?;}
    let receipt=text(body,"receiptId");
    if uuid::Uuid::parse_str(receipt).is_err()||uuid::Uuid::parse_str(receipt).is_ok_and(|u|u.to_string()!=receipt)||receipt.len()!=36||text(body,"postId").is_empty()||text(body,"postId").len()>256
        ||!sha(&body["expectedSourceVersion"])||!sha(&body["attachmentDigest"]){return Err(bad("Exact photo source and UUID receipt required"));}Ok(())
}
fn photo(a:&Value)->bool{matches!(text(a,"type"),"photo"|"image")}
fn pin(d:&Value,post:&Value)->ApiResult<Value>{
    let binding=active_binding(d)?.to_json();let account=required(d,"account")?;
    if post["id"].as_str().is_none()||text(post,"postKey").is_empty()||(!post["connectorBinding"].is_null()&&post["connectorBinding"]!=binding)
        ||(!post["account"].is_null()&&post["account"]!=d["account"])||!post["attachments"].is_array(){return Err(conflict("Photo source binding unavailable"));}
    let slots:Vec<_>=rows(post,"attachments").iter().enumerate().filter(|(_,a)|photo(a)).collect();
    if rows(post,"attachments").len()>20||text(post,"postKey").len()>500||text(post,"postKey").chars().any(char::is_control)||slots.is_empty()||slots.len()>16
        ||rows(post,"attachments").iter().any(|a|!a.is_object()||["url","source_url","preview_url","title"].iter().any(|k|a[*k].as_str().is_some_and(|s|s.len()>8192)))
        ||slots.iter().any(|(_,a)|a["url"].as_str().is_none_or(|u|u.is_empty())){return Err(bad("Eligible bounded source photos required"));}
    Ok(json!({"version":1,"account":account,"connectorBinding":binding,"postId":post["id"],"postKey":post["postKey"],
        "sourceVersion":media_fullframes::source_version(post,account),"attachmentDigest":hash(&post["attachments"]),"attachments":post["attachments"]}))
}
fn source(d:&Value,body:&Value)->ApiResult<Value>{
    let post=row(d,"posts",required(body,"postId")?)?;let p=pin(d,post)?;
    if p["sourceVersion"]!=body["expectedSourceVersion"]||p["attachmentDigest"]!=body["attachmentDigest"]{return Err(conflict("Photo source changed"));}Ok(p)
}
fn owns(d:&Value,p:&Value,except:Option<&str>)->ApiResult<()>{
    if rows(d,"jobs").iter().any(|j|Some(text(j,"id"))!=except&&j["refId"]==p["postId"]
        &&matches!(text(j,"kind"),"photo_acquisition"|"media"|"media_audio")
        &&matches!(text(j,"status"),"running"|"queued"|"paused"|"unknown"|"dispatching"|"interrupted")){
        return Err(conflict("Photo source has an active or unresolved owner"));
    }
    let ids:Vec<_>=rows(d,"items").iter().filter(|i|i["postId"]==p["postId"]||i["postKey"]==p["postKey"]).map(|i|&i["id"]).collect();
    if rows(d,"operations").iter().any(|op|ids.contains(&&op["itemId"])&&matches!(text(op,"status"),"unknown"|"dispatching"|"running")){
        return Err(conflict("Photo recipient operation is active or UNKNOWN"));
    }Ok(())
}
#[derive(Clone)]
struct Claim{job:Value,pin:Value,head:Value,items:Vec<Value>}
enum Admission{Replay(Value),Fresh(Claim)}
fn explicit_selection(d:&Value,body:&Value,p:&Value)->ApiResult<(Vec<Value>,Vec<Value>)>{
    let binding=active_binding(d)?;let mut items=Vec::new();let mut pins=Vec::new();
    for recipient in rows(body,"recipients"){
        let item=row(d,"items",required(recipient,"itemId")?)?;check_revision(item,&recipient["expectedRevision"])?;
        // An explicit selection names canonical rows, never provider aliases.
        // Both source coordinates must match; legacy all-post matching is unchanged.
        if item["postId"]!=p["postId"]||item["postKey"]!=p["postKey"]
            ||!knowledge::in_account(item,required(d,"account")?){return Err(conflict("Photo recipient company or source changed"));}
        let target=bound_item(&binding,item)?;
        items.push(json!({"id":target["id"],"postId":target["postId"],"postKey":target["postKey"]}));
        pins.push(json!({"itemId":item["id"],"expectedRevision":item["revision"],"connectorBinding":target["connectorBinding"],
            "objectId":target["objectId"],"providerItemId":target["itemId"],"conversationKey":target["conversationKey"],"postId":target["postId"],"postKey":target["postKey"]}));
    }Ok((items,pins))
}
fn claim(d:&mut Value,body:&Value,actor:&str,at:&str)->ApiResult<Admission>{
    parse(body)?;let p=source(d,body)?;
    if let Some(job)=rows(d,"jobs").iter().find(|j|j["id"]==body["receiptId"]){
        if job["kind"]!="photo_acquisition"||job["request"]!=*body||job["authorizedBy"]!=actor||job["sourcePin"]!=p{return Err(conflict("Photo receipt already bound or stale"));}
        // Returned status is an observation; running/unknown is never dispatched again.
        if job["status"]=="completed" {validate_receipt(d,row(d,"posts",text(body,"postId"))?,&job["receipt"],&store().map_err(|_|conflict("Photo evidence store unavailable"))?)?;}
        return Ok(Admission::Replay(job.clone()));
    }
    if rows(d,"jobs").iter().any(|j|j["kind"]=="photo_acquisition"&&j["sourcePin"]==p){return Err(conflict("Exact photo source was already attempted; reconcile its receipt"));}
    owns(d,&p,None)?;
    let (items,recipient_pins)=if body.get("recipients").is_some(){let(items,pins)=explicit_selection(d,body,&p)?;(items,Some(pins))}else{
        (rows(d,"items").iter().filter(|i|i["postId"]==p["postId"]||i["postKey"]==p["postKey"]).map(|i|json!({"id":i["id"],"postId":p["postId"],"postKey":p["postKey"]})).collect::<Vec<_>>(),None)
    };
    if items.is_empty()||items.len()>100||items.iter().any(|i|text(i,"id").is_empty()||text(i,"id").len()>500||text(i,"id").chars().any(char::is_control)){return Err(bad("Bounded current photo recipients required"));}
    let head=row(d,"posts",text(body,"postId"))?["photoAcquisition"].clone();
    let mut job=json!({"id":body["receiptId"],"kind":"photo_acquisition","purpose":"photo_acquire_only","status":"running","refId":body["postId"],
        "account":p["account"],"connectorBinding":p["connectorBinding"],"sourcePin":p,"sourceDigest":hash(&p),"request":body,"authorizedBy":actor,"createdAt":at});
    if let Some(pins)=recipient_pins{job["recipientPins"]=json!(pins);}
    list_mut(d,"jobs").push(job.clone());audit(d,"photo.acquisition_claimed",text(body,"receiptId"));
    Ok(Admission::Fresh(Claim{job,pin:p,head,items}))
}
fn unsigned(receipt:&Value)->Value{let mut v=receipt.clone();if let Some(o)=v.as_object_mut(){o.remove("receiptSha256");}v}
fn image_meta(v:&Value)->ApiResult<ArtifactRef>{
    let r=ArtifactRef::from_json(&v["artifact"]).map_err(|_|conflict("Photo artifact reference invalid"))?;
    let width=v["width"].as_u64().filter(|n|*n>0&&*n<=12000).ok_or_else(||conflict("Photo width invalid"))?;
    let height=v["height"].as_u64().filter(|n|*n>0&&*n<=12000).ok_or_else(||conflict("Photo height invalid"))?;
    if r.bytes==0||r.bytes>MAX_IMAGE||v["sha256"]!=r.sha256||v["bytes"]!=r.bytes||width*height>24_000_000
        ||!matches!(text(v,"mime"),"image/jpeg"|"image/png"|"image/webp"){return Err(conflict("Photo pixels metadata invalid"));}Ok(r)
}
fn validate_receipt(d:&Value,post:&Value,r:&Value,store:&ArtifactStore)->ApiResult<()>{
    if r["version"]!=1||r["kind"]!="photo_acquisition"||r["purpose"]!="photo_acquire_only"||r["modelCalled"]!=false||r["semanticAcceptance"]!=false
        ||r["validator"]!="assistant-images-structural-v1"||r["sourcePin"]!=pin(d,post)?||r["sourceDigest"]!=hash(&r["sourcePin"])
        ||r["receiptSha256"]!=hash(&unsigned(r))||!r["images"].is_array()||!r["failures"].is_array(){return Err(conflict("Photo acquisition receipt invalid or stale"));}
    let mut slots=BTreeSet::new();let mut bytes=0u64;let mut pixels=0u64;
    for v in rows(r,"images"){
        let index=v["attachmentIndex"].as_u64().and_then(|i|usize::try_from(i).ok()).ok_or_else(||conflict("Photo slot invalid"))?;
        let a=rows(post,"attachments").get(index).filter(|a|photo(a)).ok_or_else(||conflict("Photo slot changed"))?;
        if !slots.insert(index)||v["postId"]!=post["id"]||v["attachmentSha256"]!=hash(a){return Err(conflict("Photo source slot mismatch"));}
        let reference=image_meta(v)?;bytes+=reference.bytes;pixels+=v["width"].as_u64().unwrap()*v["height"].as_u64().unwrap();
        // Intentionally no time/stamp cache: same-size retained-byte tamper fails every admission/dispatch.
        store.verify(&reference).map_err(|_|conflict("Photo artifact missing or changed"))?;
    }
    for f in rows(r,"failures"){
        let index=f["attachmentIndex"].as_u64().and_then(|i|usize::try_from(i).ok()).ok_or_else(||conflict("Photo failure slot invalid"))?;
        if !slots.insert(index)||rows(post,"attachments").get(index).is_none_or(|a|!photo(a))||f["postId"]!=post["id"]
            ||!matches!(text(f,"stage"),"acquisition"|"validation"|"not_started")||text(f,"category").is_empty(){return Err(conflict("Photo failure binding invalid"));}
    }
    if bytes>64*1024*1024||pixels>192_000_000||slots.len()!=rows(post,"attachments").iter().filter(|a|photo(a)).count(){return Err(conflict("Incomplete photo acquisition outcomes"));}Ok(())
}
pub(crate) fn current_metadata(d:&Value,post:&Value)->ApiResult<Value>{
    if post["photoAcquisition"].is_null(){return Err(conflict("Photo acquisition receipt missing"));}
    let r=&post["photoAcquisition"];let store=store().map_err(|_|conflict("Photo evidence store unavailable"))?;validate_receipt(d,post,r,&store)?;
    let images:Vec<_>=rows(r,"images").iter().map(|v|{let mut v=v.clone();v["acquisitionReceiptSha256"]=r["receiptSha256"].clone();v}).collect();
    Ok(json!({"imageEvidence":images,"imageFailures":r["failures"],"acquisitionReceiptSha256":r["receiptSha256"]}))
}
fn reject_links(path:&FsPath)->ApiResult<()>{
    if !path.is_absolute(){return Err(bad("Photo scratch must be absolute"));}let mut prefix=PathBuf::new();
    for component in path.components(){prefix.push(component);if matches!(component,std::path::Component::Prefix(_)){continue;}
        let m=std::fs::symlink_metadata(&prefix).map_err(|_|bad("Photo staging unavailable"))?;
        if m.file_type().is_symlink(){return Err(bad("Photo staging links rejected"));}
        #[cfg(windows)]{use std::os::windows::fs::MetadataExt;if m.file_attributes()&0x400!=0{return Err(bad("Photo staging reparse rejected"));}}
    }Ok(())
}
fn persist(result:&Value,c:&Claim,bridge_account:&str,scratch:&FsPath,store:&ArtifactStore)->ApiResult<Value>{
    fields(result,&["version","account","receiptId","sourceDigest","images","failures"])?;
    if result["version"]!=1||result["account"]!=bridge_account||result["receiptId"]!=c.job["id"]||result["sourceDigest"]!=c.job["sourceDigest"]
        ||!result["images"].is_array()||!result["failures"].is_array(){return Err(bad("Photo adapter binding mismatch"));}
    let root=scratch.join("photo-acquisition").join(bridge_account).join(text(&c.job,"id"));reject_links(&root)?;
    let is_comment=c.pin["sourceKind"]=="comment_attachment";
    let source_key=if is_comment{"itemId"}else{"postId"};
    let eligible=|attachment:&Value|if is_comment{comment::image(attachment)}else{photo(attachment)};
    let mut images=Vec::new();let mut seen=BTreeSet::new();let mut total=0u64;
    for v in rows(result,"images"){
        if is_comment{fields(v,&["attachmentIndex","itemId","sourceRole","sha256","bytes","mime","width","height"])?;}else{fields(v,&["attachmentIndex","postId","sha256","bytes","mime","width","height"])?;}
        let index=v["attachmentIndex"].as_u64().and_then(|i|usize::try_from(i).ok()).ok_or_else(||bad("Photo slot invalid"))?;
        let attachment=rows(&c.pin,"attachments").get(index).filter(|a|eligible(a)).ok_or_else(||bad("Unexpected photo slot"))?;
        if !seen.insert(index)||v[source_key]!=c.pin[source_key]||is_comment&&v["sourceRole"]!=c.pin["sourceRole"]||!sha(&v["sha256"]){return Err(bad("Photo slot binding mismatch"));}
        let path=root.join(format!("photo-{index}.image"));reject_links(&path)?;
        let mut options=std::fs::OpenOptions::new();options.read(true);
        #[cfg(windows)]{use std::os::windows::fs::OpenOptionsExt;options.custom_flags(0x00200000).share_mode(1);}
        #[cfg(target_os="linux")]{use std::os::unix::fs::OpenOptionsExt;options.custom_flags(0x20000);}
        let input=options.open(&path).map_err(|_|bad("Photo staging open failed"))?;let metadata=input.metadata().map_err(|_|bad("Photo staging metadata failed"))?;
        #[cfg(windows)]{use std::os::windows::fs::MetadataExt;if metadata.file_attributes()&0x400!=0{return Err(bad("Photo staging reparse rejected"));}}
        #[cfg(unix)]{use std::os::unix::fs::MetadataExt;let after=std::fs::symlink_metadata(&path).map_err(|_|bad("Photo staging changed"))?;
            if after.file_type().is_symlink()||metadata.nlink()!=1||after.ino()!=metadata.ino()||after.dev()!=metadata.dev(){return Err(bad("Photo staging identity changed"));}}
        if !metadata.is_file()||metadata.len()==0||metadata.len()>MAX_IMAGE||v["bytes"]!=metadata.len(){return Err(bad("Photo staging size mismatch"));}
        let mut bytes=Vec::new();input.take(MAX_IMAGE+1).read_to_end(&mut bytes).map_err(|_|bad("Photo staging read failed"))?;
        if bytes.len() as u64!=metadata.len(){return Err(bad("Photo staging changed"));}
        let signature=match text(v,"mime"){"image/jpeg"=>bytes.starts_with(&[0xff,0xd8])&&bytes.ends_with(&[0xff,0xd9]),"image/png"=>bytes.starts_with(b"\x89PNG\r\n\x1a\n"),"image/webp"=>bytes.starts_with(b"RIFF")&&bytes.get(8..12)==Some(b"WEBP"),_=>false};
        if !signature{return Err(bad("Photo signature mismatch"));}
        let reference=store.put_bytes(&bytes).map_err(|_|bad("Photo persistence failed"))?;
        if v["sha256"]!=reference.sha256||v["bytes"]!=reference.bytes{return Err(bad("Photo staging hash mismatch"));}
        let mut image=v.clone();image["artifact"]=reference.to_json();image["origin"]=json!(if is_comment{"comment_attachment"}else{"post_attachment"});image["attachmentSha256"]=json!(hash(attachment));image_meta(&image)?;
        total+=reference.bytes;if total>64*1024*1024{return Err(bad("Photo total byte limit"));}images.push(image);
    }
    let mut failures=Vec::new();
    for f in rows(result,"failures"){
        if is_comment{fields(f,&["attachmentIndex","itemId","sourceRole","category","stage"])?;}else{fields(f,&["attachmentIndex","postId","category","stage"])?;}
        let index=f["attachmentIndex"].as_u64().and_then(|i|usize::try_from(i).ok()).ok_or_else(||bad("Photo failure slot invalid"))?;
        if !seen.insert(index)||rows(&c.pin,"attachments").get(index).is_none_or(|a|!eligible(a))||f[source_key]!=c.pin[source_key]||is_comment&&f["sourceRole"]!=c.pin["sourceRole"]
            ||!matches!(text(f,"stage"),"acquisition"|"validation"|"not_started")||text(f,"category").is_empty()||text(f,"category").len()>80
            ||!text(f,"category").bytes().all(|b|b.is_ascii_lowercase()||b.is_ascii_digit()||b==b'_'){return Err(bad("Photo failure binding invalid"));}
        let mut f=f.clone();f["origin"]=json!(if is_comment{"comment_attachment"}else{"post_attachment"});failures.push(f);
    }
    if seen.len()!=rows(&c.pin,"attachments").iter().filter(|a|eligible(a)).count(){return Err(bad("Photo adapter omitted a source outcome"));}
    Ok(json!({"images":images,"failures":failures}))
}
fn commit(d:&mut Value,c:&Claim,outcome:&Value,at:&str,store:&ArtifactStore)->ApiResult<Value>{
    let job_id=text(&c.job,"id");if row(d,"jobs",job_id)?!=&c.job{return Err(conflict("Photo attempt ownership changed"));}
    let post=row(d,"posts",text(&c.pin,"postId"))?;
    if pin(d,post)?!=c.pin||post["photoAcquisition"]!=c.head{return Err(conflict("Photo source/head changed during acquisition"));}owns(d,&c.pin,Some(job_id))?;
    // These pins protect the current explicit routing request across network
    // work. The resulting photo receipt remains source-owned pixel evidence.
    if c.job["request"].get("recipients").is_some(){
        let(items,pins)=explicit_selection(d,&c.job["request"],&c.pin)?;
        if items!=c.items||json!(pins)!=c.job["recipientPins"]{return Err(conflict("Photo recipient selection changed during acquisition"));}
    }
    let mut receipt=json!({"id":c.job["id"],"version":1,"kind":"photo_acquisition","purpose":"photo_acquire_only","authorizedBy":c.job["authorizedBy"],"acquiredAt":at,
        "sourcePin":c.pin,"sourceDigest":c.job["sourceDigest"],"images":outcome["images"],"failures":outcome["failures"],"validator":"assistant-images-structural-v1","modelCalled":false,"semanticAcceptance":false});
    receipt["receiptSha256"]=json!(hash(&receipt));validate_receipt(d,post,&receipt,store)?;
    row_mut(d,"posts",text(&c.pin,"postId"))?["photoAcquisition"]=receipt.clone();
    let job=row_mut(d,"jobs",job_id)?;job["status"]=json!("completed");job["receipt"]=receipt.clone();job["finishedAt"]=json!(at);
    audit(d,"photo.acquisition_observed",job_id);Ok(receipt)
}
async fn unresolved(app:&App,c:&Claim,category:&str)->ApiResult<()>{
    app.change(|d|{let job=row_mut(d,"jobs",text(&c.job,"id"))?;
        if job==&c.job{job["status"]=json!("unknown");job["error"]=json!(category);job["finishedAt"]=json!(now());}Ok(())}).await
}
pub(crate) async fn acquire(State(app):State<App>,axum::Extension(actor):axum::Extension<operator_auth::Actor>,Json(body):Json<Value>)->ApiResult<Json<Value>>{
    if actor.role!="owner"{return Err(ApiError(StatusCode::FORBIDDEN,"Photo acquisition requires owner".into()));}if body.get("sourceKind").is_some(){comment::parse(&body)?;}else{parse(&body)?;}
    execute(&app,&body,&actor.id).await.map(Json)
}
async fn execute(app:&App,body:&Value,authority:&str)->ApiResult<Value>{
    let scratch=std::env::var_os("COMMUNITYHERO_MEDIA_SCRATCH_DIR").map(PathBuf::from).ok_or_else(||bad("Photo scratch unconfigured"))?;reject_links(&scratch)?;
    let store=store().map_err(|_|bad("Photo evidence store unavailable"))?;
    let is_comment=body["sourceKind"]=="comment_attachment";
    let admission=app.change(|d|if is_comment{comment::claim(d,body,authority,&now())}else{claim(d,body,authority,&now())}).await?;
    let c=match admission{Admission::Replay(v)=>return Ok(v),Admission::Fresh(c)=>c};
    let request=if is_comment{json!({"version":2,"purpose":"photo_acquire_only","receiptId":c.job["id"],"sourceDigest":c.job["sourceDigest"],
        "sourceComment":{"id":c.pin["itemId"],"sourceVersion":c.pin["sourceVersion"],"sourceRole":c.pin["sourceRole"],"attachments":c.pin["attachments"]}})}else{json!({"version":1,"purpose":"photo_acquire_only","receiptId":c.job["id"],"sourceDigest":c.job["sourceDigest"],
        "sourcePost":{"id":c.pin["postId"],"postKey":c.pin["postKey"],"sourceVersion":c.pin["sourceVersion"],"attachments":c.pin["attachments"]},
        "items":c.items,"branches":[]})};
    let result=match app.bridge("photo_acquire_only",json!({"request":request})).await{Ok(v)=>v,Err(e)=>{unresolved(&app,&c,"photo_bridge_outcome_unknown").await?;return Err(e);}};
    let outcome=match persist(&result,&c,app.account.key(),&scratch,&store){Ok(v)=>v,Err(e)=>{unresolved(&app,&c,"photo_persistence_not_admitted").await?;return Err(e);}};
    let receipt=match app.change(|d|if is_comment{comment::commit(d,&c,&outcome,&now(),&store)}else{commit(d,&c,&outcome,&now(),&store)}).await{Ok(v)=>v,Err(e)=>{unresolved(&app,&c,"photo_commit_not_admitted").await?;return Err(e);}};
    // Only deterministic files from this owned attempt are retired after CAS+head commit.
    let root=scratch.join("photo-acquisition").join(app.account.key()).join(text(&c.job,"id"));
    for image in rows(&outcome,"images"){if let Some(index)=image["attachmentIndex"].as_u64(){let _=std::fs::remove_file(root.join(format!("photo-{index}.image")));}}
    let _=std::fs::remove_dir(root);Ok(receipt)
}
/// Native preparation may acquire exact source photos through the existing
/// non-model operation. The owning task/job, not an invented personal actor,
/// is authority; an existing attempted/UNKNOWN source is never retried here.
pub(crate) async fn ensure_for_preparation(app:&App,owner_job:&str,item_ids:&[Value])->ApiResult<()> {
    if runtime_lifecycle_app::current_job().as_deref()!=Some(owner_job){return Err(conflict("Photo preparation lacks native task ownership"));}
    let requests=app.change(|d|{
        let owner=row(d,"jobs",owner_job)?;
        if owner["kind"]!="assistant"||owner["status"]!="running"{return Err(conflict("Photo preparation owner is not active"));}
        let mut posts=BTreeSet::new();let mut requests=Vec::new();
        for id in item_ids{
            let item=row(d,"items",id.as_str().ok_or_else(||bad("Exact photo recipient required"))?)?;
            if !knowledge::in_account(item,required(d,"account")?){return Err(conflict("Photo preparation recipient company changed"));}
            let post_id=required(item,"postId")?;
            if !posts.insert(post_id.to_owned()){continue;}
            let post=row(d,"posts",post_id)?;
            if !rows(post,"attachments").iter().any(photo){continue;}
            if current_metadata(d,post).is_ok_and(|m|rows(&m,"imageFailures").is_empty()){continue;}
            let p=pin(d,post)?;
            let recipients=item_ids.iter().filter_map(|id|id.as_str()).filter_map(|id|row(d,"items",id).ok())
                .filter(|i|i["postId"]==post["id"]).map(|i|json!({"itemId":i["id"],"expectedRevision":i["revision"]})).collect::<Vec<_>>();
            requests.push(json!({"receiptId":uuid::Uuid::new_v4().to_string(),"postId":post["id"],
                "expectedSourceVersion":p["sourceVersion"],"attachmentDigest":p["attachmentDigest"],"recipients":recipients}));
        }requests.extend(comment::preparation_requests(d,item_ids)?);Ok(requests)
    }).await?;
    for request in requests{
        let authority=format!("native-preparation:{owner_job}");
        let result=execute(app,&request,&authority).await?;
        if result["status"].as_str().is_some_and(|s|s!="completed")||!rows(&result,"failures").is_empty(){return Err(conflict("Mandatory photo acquisition held; inspect exact source receipt"));}
    }Ok(())
}
/// Exact native pins for the subsequent maintenance request. This route only
/// reads source/ownership metadata: no store.open, download, model or auth helper.
pub(crate) async fn preflight(State(app):State<App>,axum::Extension(actor):axum::Extension<operator_auth::Actor>,Path(post_id):Path<String>)->ApiResult<Json<Value>>{
    if actor.role!="owner"{return Err(ApiError(StatusCode::FORBIDDEN,"Photo preflight requires owner".into()));}
    if post_id.is_empty()||post_id.len()>256{return Err(bad("Bounded exact post ID required"));}
    let d=app.read().await?;let post=row(&d,"posts",&post_id)?;let p=pin(&d,post)?;
    let attempts:Vec<_>=rows(&d,"jobs").iter().filter(|j|j["kind"]=="photo_acquisition"&&j["refId"]==p["postId"]).map(|j|json!({"id":j["id"],"status":j["status"],"sourceDigest":j["sourceDigest"],"exactCurrentSource":j["sourcePin"]==p})).collect();
    let ownership=owns(&d,&p,None).err().map(|e|e.1);
    let already_attempted=rows(&d,"jobs").iter().any(|j|j["kind"]=="photo_acquisition"&&j["sourcePin"]==p);
    let recipient_count=rows(&d,"items").iter().filter(|i|i["postId"]==p["postId"]||i["postKey"]==p["postKey"]).count();
    let receipt=&post["photoAcquisition"];
    let slots:Vec<_>=rows(post,"attachments").iter().enumerate().filter(|(_,a)|photo(a)).map(|(i,a)|json!({"attachmentIndex":i,"attachmentSha256":hash(a)})).collect();
    Ok(Json(json!({"version":1,"account":p["account"],"connectorBinding":p["connectorBinding"],"postId":p["postId"],"postKey":p["postKey"],
        "expectedSourceVersion":p["sourceVersion"],"attachmentDigest":p["attachmentDigest"],"sourceDigest":hash(&p),"slots":slots,
        "currentReceipt":if receipt.is_null(){Value::Null}else{json!({"id":receipt["id"],"receiptSha256":receipt["receiptSha256"],"sourceCurrent":receipt["sourcePin"]==p,"metadataIntegrity":receipt["receiptSha256"]==hash(&unsigned(receipt)),"retainedBytesVerified":false})},
        "attempts":attempts,"acquisitionAllowed":ownership.is_none()&&!already_attempted,"ownershipBlock":ownership,
        "recipientCount":recipient_count,"explicitSelectionRequired":recipient_count>100})))
}
/// Backup consumer must include these immutable objects AND its native DB snapshot.
/// This is a reference closure, not a claim that the existing DB-only backup copied CAS.
pub(crate) fn artifact_refs(d:&Value)->ApiResult<Vec<ArtifactRef>>{
    let mut refs=Vec::new();let mut seen=BTreeSet::new();
    for receipt in rows(d,"posts").iter().map(|p|&p["photoAcquisition"]).chain(rows(d,"items").iter().map(|item|&item["commentPhotoAcquisition"]))
        .chain(rows(d,"jobs").iter().filter(|j|j["kind"]=="photo_acquisition").map(|j|&j["receipt"])){
        if receipt.is_null(){continue;}if receipt["receiptSha256"]!=hash(&unsigned(receipt)){return Err(conflict("Photo backup receipt integrity invalid"));}
        for image in rows(receipt,"images"){let r=image_meta(image)?;if seen.insert((r.sha256.clone(),r.bytes)){refs.push(r);}}
    }Ok(refs)
}

#[cfg(test)]
#[path="photo_acquisition_tests.rs"]
mod tests;
#[cfg(test)]
pub(crate) fn fixture_commit_baw_photo(d:&mut Value,post_id:&str,at:&str)->ApiResult<Value>{
    if d["account"]!="BAW Russia"{return Err(bad("BAW isolated fixture only"));}
    fixture_commit_photo(d,post_id,at)
}
#[cfg(test)]
pub(crate) fn fixture_commit_photo(d:&mut Value,post_id:&str,at:&str)->ApiResult<Value>{
    let store=store().map_err(|e|internal(&e))?;
    fixture_commit_photo_in(d,post_id,at,&store)
}
/// Explicit isolated CAS seam for release/floor corpus construction. Only test
/// binaries expose this helper; ordinary claim, source and commit guards run.
#[cfg(test)]
pub(crate) fn fixture_commit_photo_in(d:&mut Value,post_id:&str,at:&str,store:&ArtifactStore)->ApiResult<Value>{
    let profile=crate::accounts::Profile::from_workspace(d)?;crate::active_binding(d)?;
    let post=row(d,"posts",post_id)?;let body=json!({"receiptId":crate::id(),"postId":post_id,"expectedSourceVersion":media_fullframes::source_version(post,profile.display()),"attachmentDigest":hash(&post["attachments"])});
    let claim=match claim(d,&body,"isolated-native-fixture",at)?{Admission::Fresh(c)=>c,_=>return Err(bad("Fresh isolated fixture expected"))};
    // A genuine tiny PNG, retained through the ordinary native CAS commit. This
    // fixture performs no download/bridge/model and never bypasses a guard.
    let pixels=fixture_pixels();
    let artifact=store.put_bytes(&pixels).map_err(|e|internal(&e.to_string()))?;let images=rows(&claim.pin,"attachments").iter().enumerate().filter(|(_,a)|photo(a)).map(|(index,a)|json!({"postId":post_id,"attachmentIndex":index,"origin":"post_attachment","attachmentSha256":hash(a),"sha256":artifact.sha256,"bytes":artifact.bytes,"mime":"image/png","width":1,"height":1,"artifact":artifact.to_json()})).collect::<Vec<_>>();
    commit(d,&claim,&json!({"images":images,"failures":[]}),at,store)
}
#[cfg(test)]
fn fixture_pixels()->[u8;69]{[137,80,78,71,13,10,26,10,0,0,0,13,73,72,68,82,0,0,0,1,0,0,0,1,8,2,0,0,0,144,119,83,222,0,0,0,12,73,68,65,84,120,156,99,248,207,192,0,0,3,1,1,0,201,254,146,239,0,0,0,0,73,69,78,68,174,66,96,130]}
#[cfg(test)]
pub(crate) fn fixture_commit_comment_photo(d:&mut Value,item_id:&str,at:&str)->ApiResult<Value>{
    fixture_commit_comment_photo_in(d,item_id,at,&store().map_err(|e|internal(&e))?)
}
#[cfg(test)]
pub(crate) fn fixture_commit_comment_photo_in(d:&mut Value,item_id:&str,at:&str,store:&ArtifactStore)->ApiResult<Value>{
    crate::accounts::Profile::from_workspace(d)?;let item=row(d,"items",item_id)?;let p=comment::pin(d,item)?;
    let body=json!({"receiptId":crate::id(),"sourceKind":"comment_attachment","itemId":item_id,"expectedRevision":item["revision"],"expectedSourceVersion":p["sourceVersion"],"attachmentDigest":p["attachmentDigest"]});
    let claim=match comment::claim(d,&body,"isolated-native-fixture",at)?{Admission::Fresh(claim)=>claim,_=>return Err(bad("Fresh isolated fixture expected"))};
    let artifact=store.put_bytes(&fixture_pixels()).map_err(|e|internal(&e.to_string()))?;
    let images=rows(&claim.pin,"attachments").iter().enumerate().filter(|(_,a)|comment::image(a)).map(|(index,attachment)|json!({"itemId":item_id,"sourceRole":claim.pin["sourceRole"],"attachmentIndex":index,"origin":"comment_attachment",
        "attachmentSha256":hash(attachment),"sha256":artifact.sha256,"bytes":artifact.bytes,"mime":"image/png","width":1,"height":1,"artifact":artifact.to_json()})).collect::<Vec<_>>();
    comment::commit(d,&claim,&json!({"images":images,"failures":[]}),at,store)
}
