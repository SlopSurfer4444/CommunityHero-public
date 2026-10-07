//! Owner-reviewed, one-use admission of an exact post whose only media history is
//! an opaque completed job. This does not rewrite that history or grant retries.
use super::*;
use axum::{extract::{Query,State},Extension,Json};
use crate::operator_auth::Actor;
use std::collections::BTreeMap;

const REVIEW_SECONDS:i64=300;
const PREFIX:&str="legacy-exact-v2:";

pub(super) fn candidate_key(post_id:&str)->String{format!("{PREFIX}{post_id}")}
pub(super) fn is_permitted(job:&Value)->bool{job["legacyReprocessPermit"]["version"]==1}

fn exact_open_items(d:&Value,post:&Value)->Vec<Value>{
    let Ok(binding)=crate::active_binding(d) else{return vec![]};
    let mut items:Vec<_>=rows(d,"items").iter().filter(|item|
        item["postId"]==post["id"] && item["postKey"]==post["postKey"]
        && matches!(text(item,"providerStatus"),"new"|"inprogress"|"in_progress")
        && matches!(text(item,"workflow"),"attention"|"prepared"|"waiting"|"wait"|"active")
        && crate::bound_item(&binding,item).is_ok())
        .map(|item|json!({"id":item["id"],"postId":item["postId"],"postKey":item["postKey"],
            "providerStatus":item["providerStatus"],"workflow":item["workflow"]})).collect();
    items.sort_by(|a,b|text(a,"id").cmp(text(b,"id")));
    items
}
pub(super) fn has_exact_open_item(d:&Value,post:&Value)->bool{!exact_open_items(d,post).is_empty()}

fn same_source(d:&Value,job:&Value,post:&Value,source:&str)->bool{
    if job["kind"]!="media" {return false;}
    if job["refId"]==post["id"] {return true;}
    let Some(other)=rows(d,"posts").iter().find(|p|p["id"]==job["refId"]) else{return false};
    crate::knowledge::media_source_key(other,text(d,"account")).as_deref()==Some(source)
}

fn state(d:&Value,post_id:&str,legacy_id:&str,own_job:Option<&str>,at:&str)->Result<Value,String>{
    let account=account_scope(d).map_err(|_|"Current account or connector unavailable")?;
    let binding=crate::active_binding(d).map_err(|_|"Current connector unavailable")?.to_json();
    let post=crate::row(d,"posts",post_id).map_err(|_|"Exact post unavailable")?;
    if !video(post)||!crate::knowledge::in_account(post,account)
        ||(!post["connectorBinding"].is_null()&&post["connectorBinding"]!=binding)
        ||text(post,"postKey").is_empty(){return Err("Post is not an exact bound video".into());}
    let source=crate::knowledge::media_source_key(post,account)
        .filter(|s|!s.starts_with("post:"))
        .ok_or("Post has no normalized provider video source")?;
    let comments=exact_open_items(d,post);
    if comments.is_empty(){return Err("No bound open comment on the exact post".into());}
    let old=crate::row(d,"jobs",legacy_id).map_err(|_|"Legacy job unavailable")?;
    if old["kind"]!="media"||!old["purpose"].is_null()||old["status"]!="completed"
        ||old["refId"]!=post["id"]||old["visualContractVersion"]==2
        ||!rows(old,"sourceAttempts").is_empty()||old["result"]["visualProgress"]["schemaVersion"]==2
        ||(!old["account"].is_null()&&old["account"]!=account)
        ||(!old["connectorBinding"].is_null()&&old["connectorBinding"]!=binding){
        return Err("Only an exact opaque completed media job is eligible".into());
    }
    let opaque_count=rows(d,"jobs").iter().filter(|j|j["kind"]=="media"&&j["purpose"].is_null()
        &&j["status"]=="completed"&&j["refId"]==post["id"]).count();
    if opaque_count!=1{return Err("Ambiguous opaque media history".into());}
    for job in rows(d,"jobs").iter().filter(|j|text(j,"id")!=legacy_id&&Some(text(j,"id"))!=own_job){
        if is_permitted(job) {
            let permit=&job["legacyReprocessPermit"];
            if permit["postId"]==post["id"]||permit["sourceKey"]==source {
                return Err("Legacy reprocess permit already exists for this source".into());
            }
        }
        if !same_source(d,job,post,&source){continue;}
        if current_job(job) && (!rows(job,"sourceAttempts").is_empty()
            ||job["result"]["visualProgress"]["schemaVersion"]==2){
            return Err("A v2 source attempt or checkpoint already exists".into());
        }
        if matches!(text(job,"status"),"queued"|"running"|"interrupted"|"paused") {
            return Err("An unresolved media owner already exists".into());
        }
    }
    let policy=crate::post_media_policy::effective(d,post).map_err(|_|"Media policy unavailable")?;
    if policy["visualRequired"]!=true{return Err("Current policy does not require visual media".into());}
    if has_required_media(d,post,at).map_err(|_|"Media evidence unavailable")?{
        return Err("Required media evidence is already usable".into());
    }
    let mut materials:Vec<_>=rows(d,"materials").iter().filter(|m|m["postKey"]==post["postKey"]).cloned().collect();
    materials.sort_by(|a,b|text(a,"id").cmp(text(b,"id")));
    Ok(json!({"account":account,"connectorBinding":binding,"postId":post["id"],"postKey":post["postKey"],
        "sourceKey":source,"sourceVersion":crate::media_fullframes::source_version(post,account),
        "openComments":comments,"materialEpoch":material_epoch(d,post),
        "materialsSha256":crate::media_fullframes::hash(&json!(materials)),"policy":policy,
        "legacyJobId":legacy_id,"legacyJobSha256":crate::media_fullframes::hash(old)}))
}

fn reviewed_head(state:&Value,reviewed_at:&str)->Result<Value,String>{
    let reviewed=chrono::DateTime::parse_from_rfc3339(reviewed_at).map_err(|_|"Invalid review time")?;
    let expires=reviewed+chrono::Duration::seconds(REVIEW_SECONDS);
    let mut head=json!({"state":state,"reviewedAt":reviewed_at,"expiresAt":expires.to_rfc3339()});
    head["headSha256"]=json!(crate::media_fullframes::hash(&head));
    Ok(head)
}
fn review_fresh(reviewed_at:&str,at:&str)->bool{
    let Ok(reviewed)=chrono::DateTime::parse_from_rfc3339(reviewed_at) else{return false};
    let Ok(now)=chrono::DateTime::parse_from_rfc3339(at) else{return false};
    now>=reviewed && now-reviewed<=chrono::Duration::seconds(REVIEW_SECONDS)
}
fn request_hash(body:&Value)->String{crate::media_fullframes::hash(body)}

pub(crate) async fn get(State(app):State<crate::App>,Extension(actor):Extension<Actor>,Query(query):Query<BTreeMap<String,String>>)->crate::ApiResult<Json<Value>>{
    if actor.role!="owner"{return Err(crate::ApiError(axum::http::StatusCode::FORBIDDEN,"Owner required".into()));}
    if query.len()!=1||!query.contains_key("postId")||query["postId"].is_empty(){return Err(crate::bad("Specify only postId"));}
    let d=app.read().await?;
    let post_id=&query["postId"];
    let old:Vec<_>=rows(&d,"jobs").iter().filter(|j|j["kind"]=="media"&&j["purpose"].is_null()
        &&j["status"]=="completed"&&j["refId"]==post_id.as_str()).collect();
    if old.len()!=1{return Ok(Json(json!({"eligible":false,"reason":"Expected one exact opaque completed media job"})));}
    let reviewed_at=crate::now();
    match state(&d,post_id,text(old[0],"id"),None,&reviewed_at){
        Ok(snapshot)=>Ok(Json(json!({"eligible":true,"head":reviewed_head(&snapshot,&reviewed_at).map_err(|_|crate::internal("Review time unavailable"))?}))),
        Err(reason)=>Ok(Json(json!({"eligible":false,"reason":reason}))),
    }
}

fn admit(d:&mut Value,body:&Value,actor:&Actor,at:&str)->crate::ApiResult<Value>{
    if actor.role!="owner"{return Err(crate::ApiError(axum::http::StatusCode::FORBIDDEN,"Owner required".into()));}
    let Some(map)=body.as_object() else{return Err(crate::bad("Legacy reprocess request must be an object"))};
    let fields=["postId","legacyJobId","expectedHeadSha256","reviewedAt","reason","requestId"];
    if map.len()!=fields.len()||fields.iter().any(|f|!map.contains_key(*f)){
        return Err(crate::bad("Legacy reprocess request has unexpected fields"));
    }
    let field=|key:&str,max:usize|body[key].as_str().filter(|s|!s.trim().is_empty()&&s.len()<=max)
        .ok_or_else(||crate::bad("Missing or oversized legacy reprocess field"));
    let post_id=field("postId",200)?;let legacy_id=field("legacyJobId",200)?;
    let expected=field("expectedHeadSha256",64)?;let reviewed_at=field("reviewedAt",64)?;
    let reason=field("reason",500)?;let request_id=field("requestId",160)?;
    if expected.len()!=64||!expected.bytes().all(|c|c.is_ascii_hexdigit())
        ||!request_id.bytes().all(|c|c.is_ascii_alphanumeric()||b"-_.:".contains(&c)){
        return Err(crate::bad("Invalid review digest or request ID"));
    }
    let digest=request_hash(body);
    if let Some(prior)=rows(d,"jobs").iter().find(|j|is_permitted(j)&&j["legacyReprocessPermit"]["requestId"]==request_id){
        if prior["legacyReprocessPermit"]["requestSha256"]!=digest{return Err(crate::conflict("Legacy reprocess request ID was reused"));}
        return Ok(json!({"jobId":prior["id"],"status":prior["status"],"idempotent":true}));
    }
    if !review_fresh(reviewed_at,at){return Err(crate::conflict("Legacy reprocess review expired"));}
    let snapshot=state(d,post_id,legacy_id,None,at).map_err(|e|crate::conflict(&e))?;
    let head=reviewed_head(&snapshot,reviewed_at).map_err(|e|crate::bad(&e))?;
    if head["headSha256"]!=expected{return Err(crate::conflict("Legacy reprocess review head changed"));}
    let id=crate::id();let permit_id=crate::id();
    let state_sha=crate::media_fullframes::hash(&snapshot);
    let permit=json!({"version":1,"id":permit_id,"postId":post_id,"postKey":snapshot["postKey"],
        "sourceKey":snapshot["sourceKey"],"sourceVersion":snapshot["sourceVersion"],
        "legacyJobId":legacy_id,"stateSha256":state_sha,"reviewHeadSha256":expected,
        "reviewedAt":reviewed_at,"admittedAt":at,"reason":reason,"requestId":request_id,
        "requestSha256":digest,"actor":actor.public_json()});
    let job=json!({"id":id,"kind":"media","purpose":PURPOSE,"visualContractVersion":2,
        "account":snapshot["account"],"connectorBinding":snapshot["connectorBinding"],
        "refId":post_id,"groupKey":candidate_key(post_id),"status":"queued","sourceAttempts":[],
        "fallbackAllowed":false,"manualRequested":false,"createdAt":at,"legacyReprocessPermit":permit});
    crate::list_mut(d,"jobs").push(job);
    crate::audit(d,"media.legacy_reprocess_admitted",&id);
    Ok(json!({"jobId":id,"status":"queued","idempotent":false}))
}
pub(crate) async fn post(State(app):State<crate::App>,Extension(actor):Extension<Actor>,Json(body):Json<Value>)->crate::ApiResult<Json<Value>>{
    let at=crate::now();
    let result=app.change(|d|admit(d,&body,&actor,&at)).await?;
    MEDIA_WAKE.notify_one();
    Ok(Json(result))
}

pub(super) fn claimable_source(d:&Value,job:&Value)->crate::ApiResult<Option<Value>>{
    if !is_permitted(job)||job["status"]!="running"||!rows(job,"sourceAttempts").is_empty()
        ||job["result"]["visualProgress"]["schemaVersion"]==2{return Ok(None);}
    let permit=&job["legacyReprocessPermit"];
    let post_id=text(permit,"postId");
    if text(job,"groupKey")!=candidate_key(post_id)||job["refId"]!=post_id
        ||job["account"]!=d["account"]||job["connectorBinding"]!=crate::active_binding(d)?.to_json(){return Ok(None);}
    let at=crate::now();
    let Ok(snapshot)=state(d,post_id,text(permit,"legacyJobId"),Some(text(job,"id")),&at) else{return Ok(None)};
    if crate::media_fullframes::hash(&snapshot)!=text(permit,"stateSha256")
        ||snapshot["sourceKey"]!=permit["sourceKey"]||snapshot["sourceVersion"]!=permit["sourceVersion"]{
        return Ok(None);
    }
    Ok(rows(d,"posts").iter().find(|p|p["id"]==post_id).cloned())
}

#[cfg(test)]
mod tests {
    use super::*;
    const AT:&str="2026-09-25T10:00:00Z";
    fn fixture()->Value{
        let mut d=crate::empty();crate::accounts::initialize(&mut d,crate::accounts::Profile::LikeAvto).unwrap();
        d["posts"]=json!([{"id":"post-one","postKey":"one","title":"Shared title","sourceUrl":"https://youtube.com/watch?v=AbCdEf123_-","attachments":[{"type":"video"}]}]);
        d["items"]=json!([{"id":"comment-one","postId":"post-one","postKey":"one","providerStatus":"new","workflow":"attention"}]);
        d["jobs"]=json!([{"id":"opaque-one","kind":"media","refId":"post-one","status":"completed","result":{"materials":[{"id":"old"}]}}]);
        d
    }
    fn request(d:&Value)->Value{
        let s=state(d,"post-one","opaque-one",None,AT).unwrap();let head=reviewed_head(&s,AT).unwrap();
        json!({"postId":"post-one","legacyJobId":"opaque-one","expectedHeadSha256":head["headSha256"],
            "reviewedAt":AT,"reason":"Review old opaque completion","requestId":"legacy-1"})
    }
    #[test]
    fn opaque_admission_is_exact_idempotent_and_preserves_old_evidence(){
        let mut d=fixture();let old=d["jobs"][0].clone();let body=request(&d);
        let actor=Actor::local_owner("test");let accepted=admit(&mut d,&body,&actor,AT).unwrap();
        assert_eq!(d["jobs"][0],old);assert_eq!(accepted["status"],"queued");
        let replay=admit(&mut d,&body,&actor,"2026-09-25T10:10:00Z").unwrap();
        assert_eq!(replay["jobId"],accepted["jobId"]);assert_eq!(d["jobs"].as_array().unwrap().len(),2);
        let mut changed=body.clone();changed["reason"]=json!("different");
        let before=d.clone();assert!(admit(&mut d,&changed,&actor,AT).is_err());assert_eq!(d,before);
        let job=&d["jobs"][1];assert_eq!(job["groupKey"],candidate_key("post-one"));
        assert!(claimable_source(&d,job).is_none()); // not running before the scheduler lease
    }
    #[test]
    fn stale_comments_source_material_and_policy_reject_without_mutation(){
        for change in ["comment","source","material","policy"] {
            let mut d=fixture();let body=request(&d);
            match change{
                "comment"=>d["items"][0]["workflow"]=json!("closed"),
                "source"=>d["posts"][0]["sourceUrl"]=json!("https://youtube.com/watch?v=Another123_"),
                "material"=>d["materials"]=json!([{"id":"new","postKey":"one","text":"new"}]),
                _=>d["settings"]["mediaPolicyDefaults"]=json!({"audioOnlyAboveSeconds":120}),
            }
            let before=d.clone();assert!(admit(&mut d,&body,&Actor::local_owner("test"),AT).is_err(),"{change}");assert_eq!(d,before);
        }
    }
    #[test]
    fn pre_v2_auto_media_and_alias_prior_permit_reject(){
        let mut d=fixture();d["jobs"][0]["purpose"]=json!("auto_media");
        assert!(state(&d,"post-one","opaque-one",None,AT).is_err());
        let mut d=fixture();let body=request(&d);let actor=Actor::local_owner("test");
        admit(&mut d,&body,&actor,AT).unwrap();
        d["posts"].as_array_mut().unwrap().push(json!({"id":"post-twin","postKey":"twin","title":"Shared title",
            "sourceUrl":"https://youtu.be/AbCdEf123_-","attachments":[{"type":"video"}]}));
        d["items"].as_array_mut().unwrap().push(json!({"id":"comment-twin","postId":"post-twin","postKey":"twin","providerStatus":"new","workflow":"attention"}));
        d["jobs"].as_array_mut().unwrap().push(json!({"id":"opaque-twin","kind":"media","refId":"post-twin","status":"completed"}));
        assert!(state(&d,"post-twin","opaque-twin",None,AT).is_err());
    }
}
