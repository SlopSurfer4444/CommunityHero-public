//! Company-local acquisition policy. Fresh default work extracts full audio and
//! screen text; an explicit visual policy never becomes text-only evidence.
use crate::*;
use axum::{extract::{Path,State},Extension,Json};
use serde_json::{json,Value};
use sha2::{Digest,Sha256};
use std::collections::BTreeMap;

const DEFAULT:&str="full_audio_visual";
const AUDIO_ONLY:&str="full_audio_only";
const RETIRE_VISUAL:&str="default_full_audio_text";

fn threshold_seconds(d:&Value)->ApiResult<u64>{
    let Some(defaults)=d["settings"].get("mediaPolicyDefaults") else{return Ok(180)};
    let defaults=defaults.as_object().ok_or_else(||bad("Media policy defaults must be an object"))?;
    match defaults.get("audioOnlyAboveSeconds") {
        None=>Ok(180),
        Some(value)=>value.as_u64().filter(|n|*n>0&&*n<=86400)
            .ok_or_else(||bad("Audio-only duration threshold must be 1..86400 seconds")),
    }
}

/// The existing download checkpoint contains ffprobe's duration of the cached
/// file. Provider duration hints do not waive visual processing.
pub(crate) fn probed_duration(d:&Value,post:&Value)->Option<Value>{
    probed_duration_from(d,post,d["jobs"].as_array()?.iter())
}

/// Candidate references belong to one immutable database snapshot. Every
/// source, account and binding check still runs against the selected post.
pub(crate) struct ProbedDurationIndex<'a>{by_post:BTreeMap<&'a str,Vec<&'a Value>>}
impl<'a> ProbedDurationIndex<'a>{
    pub(crate) fn new(d:&'a Value)->Self{
        let mut by_post:BTreeMap<&str,Vec<&Value>>=BTreeMap::new();
        if let Some(jobs)=d["jobs"].as_array(){
            for job in jobs {
                if job["kind"]=="media"&&job["purpose"]=="auto_media"&&job["visualContractVersion"]==2 {
                    if let Some(id)=job["refId"].as_str(){by_post.entry(id).or_default().push(job);}
                }
            }
        }
        Self{by_post}
    }
    fn candidates(&self,post:&Value)->impl Iterator<Item=&'a Value> + '_{
        self.by_post.get(post["id"].as_str().unwrap_or("")).into_iter().flat_map(|jobs|jobs.iter().copied())
    }
}
fn probed_duration_from<'a>(d:&Value,post:&Value,jobs:impl Iterator<Item=&'a Value>)->Option<Value>{
    let binding=active_binding(d).ok()?.to_json();
    if !knowledge::in_account(post,text(d,"account"))
        || (!post["connectorBinding"].is_null()&&post["connectorBinding"]!=binding){return None;}
    let version=media_fullframes::source_version(post,text(d,"account"));
    let job=jobs.filter(|job|{
        let p=&job["result"]["visualProgress"];
        let phase=if p["phase"]=="held"{text(p,"resumePhase")}else{text(p,"phase")};
        job["kind"]=="media"&&job["purpose"]=="auto_media"&&job["visualContractVersion"]==2&&job["account"]==d["account"]
            &&job["connectorBinding"]==binding
            &&matches!(text(job,"status"),"queued"|"running"|"paused"|"failed"|"completed"|"interrupted")
            &&matches!(phase,"inventory"|"select"|"scan"|"finalize"|"complete")
            &&job["refId"]==post["id"]&&p["schemaVersion"]==2&&p["sourcePostId"]==post["id"]
            &&p["sourceVersion"]==version&&p["account"]==d["account"]&&p["connectorBinding"]==binding
            &&p["sourcePostKey"]==post["postKey"]&&p["sourceIdentity"]["postKey"]==post["postKey"]
            &&p["sourceIdentity"]["account"]==d["account"]&&p["sourceIdentity"]["mediaSha256"]==p["source"]["sha256"]
            &&media_fullframes::reference(&p["source"]).is_ok()
            &&p["sourceIdentity"]["durationMs"].as_u64().is_some_and(|n|n>0)
    }).max_by_key(|job|text(job,"createdAt"))?;
    let p=&job["result"]["visualProgress"];
    Some(json!({"durationMs":p["sourceIdentity"]["durationMs"],"sourceSha256":p["source"]["sha256"]}))
}

fn hash(value:&Value)->String{format!("{:x}",Sha256::digest(value.to_string().as_bytes()))}
fn text<'a>(d:&'a Value,key:&str)->&'a str{d[key].as_str().unwrap_or("")}
fn digest(value:&Value)->bool{value.as_str().is_some_and(|v|v.len()==64&&v.bytes().all(|b|b.is_ascii_hexdigit()))}

fn stored<'a>(d:&'a Value,post_id:&str)->ApiResult<Option<&'a Value>>{
    let Some(map)=d["settings"].get("postMediaPolicies") else{return Ok(None)};
    let map=map.as_object().ok_or_else(||internal("Post media policy catalog is invalid"))?;
    Ok(map.get(post_id))
}

fn active_record<'a>(d:&'a Value,post_id:&str,account:&str,binding:&Value,source_version:&str)->ApiResult<Option<&'a Value>>{
    Ok(stored(d,post_id)?.filter(|r|r["status"]=="active"&&r["account"]==account
        &&r["connectorBinding"]==*binding&&r["sourceVersion"]==source_version
        &&r["postId"]==post_id&&r["version"]==1&&r["revision"].as_u64().is_some_and(|revision|revision>0)
        &&matches!(r["mode"].as_str(),Some(DEFAULT|AUDIO_ONLY))))
}

// Retired records stay inactive but retain a CAS head: retirement must not
// recreate the pre-override hash and permit an old default-policy write.
fn policy_revision(d:&Value,post_id:&str,account:&str,binding:&Value,source:&str)->ApiResult<u64>{
    Ok(stored(d,post_id)?.filter(|r|r["version"]==1&&r["postId"]==post_id
        &&r["account"]==account&&r["connectorBinding"]==*binding&&r["sourceVersion"]==source
        &&matches!(text(r,"status"),"active"|"retired")
        &&matches!(text(r,"mode"),DEFAULT|AUDIO_ONLY))
        .and_then(|r|r["revision"].as_u64()).unwrap_or(0))
}
fn retirement_idle(d:&Value,post:&Value)->ApiResult<()>{
    let source=knowledge::media_source_key(post,text(d,"account"));
    // Queue affinity is a conservative retirement block, never media reuse
    // authority. Include both historical media groups and current visual-v2
    // groups, which can incorporate already observed identity/duration facts.
    let media_groups=knowledge::media_groups(d,text(d,"account"));
    let visual_groups=knowledge::visual_groups(d,text(d,"account"));
    let post_id=required(post,"id")?;
    let media_group=media_groups.get(post_id);
    let visual_group=visual_groups.get(post_id);
    let same_group=|peer_id:&str|{
        let bound=list(d,"posts").iter().any(|p|p["id"]==peer_id&&knowledge::in_account(p,text(d,"account"))
            &&(p["connectorBinding"].is_null()||active_binding(d).is_ok_and(|b|p["connectorBinding"]==b.to_json())));
        bound&&(media_group.is_some_and(|g|media_groups.get(peer_id)==Some(g))
            ||visual_group.is_some_and(|g|visual_groups.get(peer_id)==Some(g)))
    };
    for job in list(d,"jobs").iter().filter(|j|matches!(text(j,"kind"),"media"|"media_audio")){
        if !job["account"].is_null()&&job["account"]!=d["account"]{continue;}
        if !job["connectorBinding"].is_null()&&job["connectorBinding"]!=active_binding(d)?.to_json(){continue;}
        let references=[&job["refId"],&job["result"]["visualProgress"]["sourcePostId"],
            &job["audioPin"]["progress"]["sourcePostId"]];
        let queue_key=text(job,"groupKey");
        let group_owned=media_group.is_some_and(|g|queue_key==g.as_str()||queue_key==format!("visual-v2:{g}"))
            ||visual_group.is_some_and(|g|queue_key==format!("visual-v2:{g}"));
        let affected=group_owned||references.iter().any(|id|**id==post["id"]||id.as_str().is_some_and(|id|same_group(id)||
            list(d,"posts").iter().any(|p|p["id"]==id&&source.as_ref().is_some_and(|source|
                knowledge::media_source_key(p,text(d,"account")).as_ref()==Some(source)))));
        if affected&&(matches!(text(job,"status"),"running"|"unknown"|"dispatching"|"interrupted")
            ||job["result"]["visualProgress"]["leaseId"].as_str().is_some_and(|s|!s.is_empty())){
            return Err(conflict("Affected media work must settle before visual policy retirement"));
        }
    }
    Ok(())
}
fn retire_visual(d:&mut Value,post:&Value,old:&Value,reason:&str,actor:&operator_auth::Actor)->ApiResult<Value>{
    let post_id=required(post,"id")?;
    let prior=active_record(d,post_id,required(d,"account")?,&active_binding(d)?.to_json(),
        required(old,"sourceVersion")?)?.filter(|r|r["mode"]==DEFAULT).cloned()
        .ok_or_else(||conflict("Exact active visual override required for retirement"))?;
    retirement_idle(d,post)?;
    let mut retired=prior.clone();
    retired["revision"]=json!(prior["revision"].as_u64().unwrap_or(0).checked_add(1)
        .ok_or_else(||internal("Post media policy revision exhausted"))?);
    retired["status"]=json!("retired");retired["retiredAt"]=json!(now());
    retired["retirementReason"]=json!(reason);retired["retiredBy"]=actor.public_json();
    retired["previousRecord"]=prior.clone();
    d["settings"]["postMediaPolicies"][post_id]=retired.clone();
    let new=effective(d,post)?;
    list_mut(d,"audit").push(json!({"id":id(),"action":"post_media_policy.visual_retired",
        "refId":post_id,"createdAt":now(),"actor":actor.public_json(),"reason":reason,
        "account":old["account"],"connectorBinding":old["connectorBinding"],"sourceVersion":old["sourceVersion"],
        "previousPolicySha256":old["policySha256"],"policySha256":new["policySha256"],
        "previousRecord":prior,"retiredRecord":retired}));
    Ok(new)
}
/// Preparation requires source-proven full audio by default. An exact active
/// owner override may additionally require visual evidence. Acquisition pins
/// continue to use `effective`, so changing the preparation default cannot
/// reinterpret an already admitted media job.
pub(crate) fn effective_for_preparation(d:&Value,post:&Value)->ApiResult<Value>{
    preparation_policy(d,post)
}
fn preparation_policy(d:&Value,post:&Value)->ApiResult<Value>{
    if !knowledge::is_video_post(post){return Err(bad("Post has no video"));}
    let account=text(d,"account");
    let binding=active_binding(d)?.to_json();
    bridge_account(&active_binding(d)?)?;
    let post_id=required(post,"id")?;
    let source_version=media_fullframes::source_version(post,account);
    let record=active_record(d,post_id,account,&binding,&source_version)?;
    let mode=record.and_then(|r|r["mode"].as_str()).unwrap_or(AUDIO_ONLY);
    let revision=policy_revision(d,post_id,account,&binding,&source_version)?;
    let mut value=json!({"version":1,"purpose":"preparation","mode":mode,
        "fullAudioRequired":true,"visualRequired":mode!=AUDIO_ONLY,
        "ownerAuthorizedAudioOnly":record.is_some()&&mode==AUDIO_ONLY,
        "decisionBasis":{"kind":if record.is_some(){"exact_owner_override"}else{"default_full_video_speech"}},
        "account":account,"connectorBinding":binding,"sourceVersion":source_version});
    value["policySha256"]=json!(hash(&json!([value.clone(),revision])));
    Ok(value)
}

/// The complete immutable pin for media acquisition or cached-audio attempt.
/// Missing/stale records resolve to the conservative default.
pub(crate) fn effective(d:&Value,post:&Value)->ApiResult<Value>{
    effective_with_index(d,post,None)
}
pub(crate) fn effective_indexed(d:&Value,post:&Value,index:&ProbedDurationIndex<'_>)->ApiResult<Value>{
    effective_with_index(d,post,Some(index))
}
fn effective_with_index(d:&Value,post:&Value,index:Option<&ProbedDurationIndex<'_>>)->ApiResult<Value>{
    if !knowledge::is_video_post(post){return Err(bad("Post has no video"));}
    let account=text(d,"account");
    let binding=active_binding(d)?.to_json();
    bridge_account(&active_binding(d)?)?;
    let post_id=required(post,"id")?;
    let source_version=media_fullframes::source_version(post,account);
    let record=active_record(d,post_id,account,&binding,&source_version)?;
    let active=record.is_some();
    let threshold=if active{180}else{threshold_seconds(d)?};
    let duration=(!active).then(||match index{
        Some(index)=>probed_duration_from(d,post,index.candidates(post)),
        None=>probed_duration(d,post),
    }).flatten();
    let automatic=duration.as_ref().is_some_and(|p|p["durationMs"].as_u64().unwrap_or(0)>threshold*1000);
    let mode=if active{record.unwrap()["mode"].as_str().unwrap_or(DEFAULT)}else{AUDIO_ONLY};
    let explicit=active&&mode==AUDIO_ONLY;
    let revision=policy_revision(d,post_id,account,&binding,&source_version)?;
    let mut value=json!({"version":1,"mode":mode,"fullAudioRequired":true,
        "visualRequired":mode!=AUDIO_ONLY,"ownerAuthorizedAudioOnly":explicit,
        "account":account,"connectorBinding":binding,"sourceVersion":source_version});
    if let Some(duration)=duration{
        value["decisionBasis"]=json!({"kind":"probed_duration_threshold","audioOnlyAboveSeconds":threshold,
            "durationMs":duration["durationMs"],"sourceSha256":duration["sourceSha256"]});
    }
    if active{value["decisionBasis"]=json!({"kind":"exact_owner_override"});}
    else if !automatic{value["decisionBasis"]["kind"]=json!("default_full_video_speech");}
    let policy_digest=hash(&json!([value.clone(),revision]));
    value["policySha256"]=json!(policy_digest);
    Ok(value)
}

pub(crate) fn visual_required(d:&Value,post:&Value)->ApiResult<bool>{
    Ok(effective(d,post)?["visualRequired"]==true)
}

/// Snapshot replacement must permanently retire a source-bound exception.
/// Otherwise identical source bytes returning later could silently reactivate it.
pub(crate) fn invalidate_source_change(d:&mut Value,before:&Value,after:&Value)->ApiResult<()> {
    if before["id"]!=after["id"]{return Err(internal("Post source replacement changed identity"));}
    let account=required(d,"account")?.to_owned();
    let old_source=media_fullframes::source_version(before,&account);
    let new_source=media_fullframes::source_version(after,&account);
    if old_source==new_source{return Ok(());}
    let post_id=required(before,"id")?.to_owned();
    let Some(prior)=stored(d,&post_id)?.cloned() else{return Ok(());};
    if prior["status"]!="active"{return Ok(());}
    let policy=effective(d,before)?;
    let record=&mut d["settings"]["postMediaPolicies"][&post_id];
    record["status"]=json!("superseded");
    record["supersededAt"]=json!(now());
    record["supersededBySourceVersion"]=json!(new_source.clone());
    list_mut(d,"audit").push(json!({"id":id(),"action":"post_media_policy.source_changed",
        "refId":post_id,"createdAt":now(),"previousPolicySha256":policy["policySha256"],
        "previousSourceVersion":old_source,"sourceVersion":new_source}));
    Ok(())
}

/// One exact local post, one current provider source, one expected policy head.
/// The surrounding App transaction provides the compare-and-swap boundary.
fn set(d:&mut Value,post_id:&str,body:&Value,actor:&operator_auth::Actor)->ApiResult<Value>{
    if actor.role!="owner"{return Err(ApiError(StatusCode::FORBIDDEN,"Owner required".into()));}
    if body.as_object().is_none_or(|m|m.len()!=4||
        ["expectedPolicySha256","expectedSourceVersion","mode","reason"].iter().any(|k|!m.contains_key(*k))){
        return Err(bad("Expected policy hash, source version, mode and reason required"));
    }
    if !digest(&body["expectedPolicySha256"])||!digest(&body["expectedSourceVersion"]){
        return Err(bad("Invalid expected media policy digest"));
    }
    let mode=body["mode"].as_str().filter(|m|matches!(*m,DEFAULT|AUDIO_ONLY|RETIRE_VISUAL))
        .ok_or_else(||bad("Invalid post media policy mode"))?;
    let reason=body["reason"].as_str().map(str::trim).filter(|v|!v.is_empty()&&v.len()<=500)
        .ok_or_else(||bad("A bounded policy reason is required"))?;
    let post=row(d,"posts",post_id)?.clone();
    let old=effective(d,&post)?;
    if old["sourceVersion"]!=body["expectedSourceVersion"]||old["policySha256"]!=body["expectedPolicySha256"]{
        return Err(conflict("Post source or media policy changed"));
    }
    if mode==RETIRE_VISUAL{return retire_visual(d,&post,&old,reason,actor);}
    let previous=stored(d,post_id)?.cloned();
    let already_exact=previous.as_ref().is_some_and(|r|r["status"]=="active"
        &&r["account"]==old["account"]&&r["connectorBinding"]==old["connectorBinding"]
        &&r["sourceVersion"]==old["sourceVersion"]&&r["mode"]==mode)
        ;
    if already_exact{return Ok(old);}
    if mode==DEFAULT{return Err(bad("New full-scene video policy admissions are disabled"));}
    let account=required(d,"account")?.to_owned();
    let binding=active_binding(d)?.to_json();
    let revision=previous.as_ref().and_then(|r|r["revision"].as_u64()).unwrap_or(0)
        .checked_add(1).ok_or_else(||internal("Post media policy revision exhausted"))?;
    if d["settings"]["postMediaPolicies"].is_null(){d["settings"]["postMediaPolicies"]=json!({});}
    let map=d["settings"]["postMediaPolicies"].as_object_mut()
        .ok_or_else(||internal("Post media policy catalog is invalid"))?;
    if !map.contains_key(post_id)&&map.len()>=1000{return Err(bad("Post media policy catalog is full"));}
    map.insert(post_id.to_owned(),json!({"version":1,"revision":revision,"status":"active",
        "postId":post_id,"account":account,"connectorBinding":binding,"sourceVersion":old["sourceVersion"],
        "mode":mode,"reason":reason,"updatedAt":now(),"updatedBy":actor.public_json()}));
    let new=effective(d,&post)?;
    list_mut(d,"audit").push(json!({"id":id(),"action":"post_media_policy.changed","refId":post_id,
        "createdAt":now(),"actor":actor.public_json(),"sourceVersion":new["sourceVersion"],
        "previousPolicySha256":old["policySha256"],"policySha256":new["policySha256"],"mode":mode}));
    Ok(new)
}

pub(crate) async fn get(State(app):State<App>,Path(post_id):Path<String>)->ApiResult<Json<Value>>{
    let d=app.read().await?;
    Ok(Json(effective(&d,row(&d,"posts",&post_id)?)?))
}
pub(crate) async fn put(State(app):State<App>,Extension(actor):Extension<operator_auth::Actor>,
    Path(post_id):Path<String>,Json(body):Json<Value>)->ApiResult<Json<Value>>{
    if actor.role!="owner"{return Err(ApiError(StatusCode::FORBIDDEN,"Owner required".into()));}
    if body["mode"]==RETIRE_VISUAL{
        let d=app.read().await?;
        retirement_idle(&d,row(&d,"posts",&post_id)?)?;
    }
    // Drain any active visual chunk before committing a policy that excludes
    // further frames. The queued policy writer prevents a new periodic claim.
    let _media_guard=media_queue::wait_for_policy_change().await?;
    app.change(|d|set(d,&post_id,&body,&actor).map(Json)).await
}

pub(crate) async fn request_cached_audio(State(app):State<App>,Extension(actor):Extension<operator_auth::Actor>,
    Json(body):Json<Value>)->ApiResult<Json<Value>>{
    if actor.role!="owner"{return Err(ApiError(StatusCode::FORBIDDEN,"Owner required".into()));}
    Ok(Json(media_queue::cached_audio::request(&app,&body).await?))
}

#[cfg(test)]
pub(super) fn retire_visual_for_test(d:&mut Value,post_id:&str)->ApiResult<Value>{
    let old=effective(d,row(d,"posts",post_id)?)?;
    set(d,post_id,&json!({"expectedPolicySha256":old["policySha256"],
        "expectedSourceVersion":old["sourceVersion"],"mode":RETIRE_VISUAL,"reason":"Retire historical fixture"}),
        &operator_auth::Actor::local_owner("test"))
}
#[cfg(test)]
mod tests{
    use super::*;
    fn fixture(account:accounts::Profile)->Value{
        let mut d=empty();accounts::initialize(&mut d,account).unwrap();
        d["posts"]=json!([{"id":"p","postKey":"provider:p","title":"Video","attachments":[{"type":"video"}]}]);
        d
    }
    fn update(d:&mut Value,mode:&str)->ApiResult<Value>{
        let old=effective(d,&d["posts"][0])?;
        set(d,"p",&json!({"expectedPolicySha256":old["policySha256"],"expectedSourceVersion":old["sourceVersion"],
            "mode":mode,"reason":"Explicit source-scoped owner decision"}),&operator_auth::Actor::local_owner("test"))
    }
    fn historical_visual(d:&mut Value)->Value{
        let post=d["posts"][0].clone();let old=effective(d,&post).unwrap();
        let revision=stored(d,"p").unwrap().and_then(|r|r["revision"].as_u64()).unwrap_or(0)+1;
        d["settings"]["postMediaPolicies"]["p"]=json!({"version":1,"revision":revision,"status":"active",
            "postId":"p","account":old["account"],"connectorBinding":old["connectorBinding"],
            "sourceVersion":old["sourceVersion"],"mode":DEFAULT,"reason":"Historical fixture"});
        effective(d,&post).unwrap()
    }
    #[test]
    fn default_is_video_speech_acquisition_and_exact_company_visual_override_remains_explicit(){
        for account in [accounts::Profile::LikeAvto,accounts::Profile::BawRussia]{
            let mut d=fixture(account);
            let default=effective(&d,&d["posts"][0]).unwrap();
            assert_eq!(default["mode"],AUDIO_ONLY);assert_eq!(default["visualRequired"],false);
            assert_eq!(default["decisionBasis"]["kind"],"default_full_video_speech");
            assert_eq!(default["fullAudioRequired"],true);
            assert_eq!(default["ownerAuthorizedAudioOnly"],false);
            let selected=update(&mut d,AUDIO_ONLY).unwrap();
            assert_eq!(selected["mode"],AUDIO_ONLY);assert_eq!(selected["visualRequired"],false);
            assert_ne!(selected["policySha256"],default["policySha256"]);
            assert_eq!(d["audit"].as_array().unwrap().len(),1);
            let reverted=historical_visual(&mut d);
            assert_eq!(reverted["visualRequired"],true);assert_eq!(reverted["ownerAuthorizedAudioOnly"],false);
            assert_ne!(reverted["policySha256"],selected["policySha256"]);
            assert_eq!(d["audit"].as_array().unwrap().len(),1);
        }
    }
    #[test]
    fn preparation_and_fresh_acquisition_default_to_video_speech_while_visual_override_is_exact(){
        for account in [accounts::Profile::LikeAvto,accounts::Profile::BawRussia]{
            let mut d=fixture(account);
            let acquisition=effective(&d,&d["posts"][0]).unwrap();
            let preparation=effective_for_preparation(&d,&d["posts"][0]).unwrap();
            assert_eq!(acquisition["mode"],AUDIO_ONLY);
            assert_eq!(acquisition["visualRequired"],false);
            assert_eq!(preparation["mode"],AUDIO_ONLY);
            assert_eq!(preparation["fullAudioRequired"],true);
            assert_eq!(preparation["visualRequired"],false);
            assert_eq!(preparation["purpose"],"preparation");
            assert_ne!(preparation["policySha256"],acquisition["policySha256"]);
            let visual=historical_visual(&mut d);
            let exact=effective_for_preparation(&d,&d["posts"][0]).unwrap();
            assert_eq!(exact["mode"],DEFAULT);
            assert_eq!(exact["visualRequired"],true);
            assert_eq!(exact["decisionBasis"]["kind"],"exact_owner_override");
            assert_eq!(effective(&d,&d["posts"][0]).unwrap(),visual);
            d["posts"][0]["title"]=json!("Different source");
            let drift=effective_for_preparation(&d,&d["posts"][0]).unwrap();
            assert_eq!(drift["mode"],AUDIO_ONLY);
            assert_ne!(drift["policySha256"],exact["policySha256"]);
        }
    }
    #[test]
    fn zero_revision_record_never_activates_an_owner_override(){
        let mut d=fixture(accounts::Profile::LikeAvto);
        let default_acquisition=effective(&d,&d["posts"][0]).unwrap();
        let default_preparation=effective_for_preparation(&d,&d["posts"][0]).unwrap();
        historical_visual(&mut d);
        d["settings"]["postMediaPolicies"]["p"]["revision"]=json!(0);
        assert_eq!(effective(&d,&d["posts"][0]).unwrap(),default_acquisition);
        assert_eq!(effective_for_preparation(&d,&d["posts"][0]).unwrap(),default_preparation);
    }
    #[test]
    fn source_drift_fails_closed_and_old_compare_and_swap_rejects(){
        let mut d=fixture(accounts::Profile::LikeAvto);
        let selected=update(&mut d,AUDIO_ONLY).unwrap();
        d["posts"][0]["title"]=json!("Replaced video source");
        let current=effective(&d,&d["posts"][0]).unwrap();
        assert_eq!(current["mode"],AUDIO_ONLY);assert_eq!(current["visualRequired"],false);
        assert_eq!(current["ownerAuthorizedAudioOnly"],false);
        assert_eq!(current["decisionBasis"]["kind"],"default_full_video_speech");
        assert_ne!(current["policySha256"],selected["policySha256"]);
        assert!(set(&mut d,"p",&json!({"expectedPolicySha256":selected["policySha256"],
            "expectedSourceVersion":selected["sourceVersion"],"mode":AUDIO_ONLY,"reason":"stale"}),
            &operator_auth::Actor::local_owner("test")).is_err());
    }
    #[test]
    fn source_reversion_does_not_reactivate_retired_exception(){
        let mut d=fixture(accounts::Profile::LikeAvto);
        update(&mut d,AUDIO_ONLY).unwrap();
        let old=d["posts"][0].clone();
        let mut changed=old.clone();changed["title"]=json!("Replaced video source");
        invalidate_source_change(&mut d,&old,&changed).unwrap();
        d["posts"][0]=changed;
        assert_eq!(effective(&d,&d["posts"][0]).unwrap()["ownerAuthorizedAudioOnly"],false);
        d["posts"][0]=old;
        assert_eq!(effective(&d,&d["posts"][0]).unwrap()["ownerAuthorizedAudioOnly"],false);
        assert_eq!(d["settings"]["postMediaPolicies"]["p"]["status"],"superseded");
    }
    #[test]
    fn foreign_company_record_cannot_authorize_an_override(){
        let mut like=fixture(accounts::Profile::LikeAvto);
        update(&mut like,AUDIO_ONLY).unwrap();
        let mut baw=fixture(accounts::Profile::BawRussia);
        baw["settings"]["postMediaPolicies"]=like["settings"]["postMediaPolicies"].clone();
        let policy=effective(&baw,&baw["posts"][0]).unwrap();
        assert_eq!(policy["mode"],AUDIO_ONLY);
        assert_eq!(policy["ownerAuthorizedAudioOnly"],false);
        assert_eq!(policy["decisionBasis"]["kind"],"default_full_video_speech");
    }
    #[test]
    fn retirement_preserves_history_and_cas_head_and_owner_authority(){
        for account in [accounts::Profile::LikeAvto,accounts::Profile::BawRussia]{
            let mut d=fixture(account);let initial=effective(&d,&d["posts"][0]).unwrap();
            let visual=historical_visual(&mut d);let prior=d["settings"]["postMediaPolicies"]["p"].clone();
            let body=json!({"expectedPolicySha256":visual["policySha256"],"expectedSourceVersion":visual["sourceVersion"],
                "mode":RETIRE_VISUAL,"reason":"Owner wants full video speech without automatic screen text"});
            let mut actor=operator_auth::Actor::local_owner("test");actor.role="operator".into();
            let before=d.clone();assert!(set(&mut d,"p",&body,&actor).is_err());assert_eq!(d,before);
            let retired=set(&mut d,"p",&body,&operator_auth::Actor::local_owner("test")).unwrap();
            assert_eq!(retired["mode"],AUDIO_ONLY);assert_eq!(retired["ownerAuthorizedAudioOnly"],false);
            assert_eq!(retired["visualRequired"],false);assert_eq!(retired["decisionBasis"]["kind"],"default_full_video_speech");
            assert_ne!(retired["policySha256"],initial["policySha256"]);
            assert_eq!(d["settings"]["postMediaPolicies"]["p"]["previousRecord"],prior);
            assert_eq!(d["audit"][0]["previousRecord"],prior);
            assert_eq!(d["audit"][0]["retiredRecord"],d["settings"]["postMediaPolicies"]["p"]);
            let before=d.clone();assert!(set(&mut d,"p",&body,&operator_auth::Actor::local_owner("test")).is_err());assert_eq!(d,before);
        }
    }
    #[test]
    fn retirement_rejects_policy_source_drift_and_nonvisual_record_without_mutation(){
        for case in ["policy","source","replacement","audio-only"]{
            let mut d=fixture(accounts::Profile::BawRussia);let visual=historical_visual(&mut d);
            let mut body=json!({"expectedPolicySha256":visual["policySha256"],"expectedSourceVersion":visual["sourceVersion"],"mode":RETIRE_VISUAL,"reason":"test"});
            match case{"policy"=>body["expectedPolicySha256"]=json!("a".repeat(64)),
                "source"=>body["expectedSourceVersion"]=json!("b".repeat(64)),
                "replacement"=>d["posts"][0]["title"]=json!("Changed source"),
                _=>{update(&mut d,AUDIO_ONLY).unwrap();let current=effective(&d,&d["posts"][0]).unwrap();
                    body["expectedPolicySha256"]=current["policySha256"].clone();}}
            let before=d.clone();assert!(set(&mut d,"p",&body,&operator_auth::Actor::local_owner("test")).is_err(),"{case}");assert_eq!(d,before);
        }
    }
    #[test]
    fn retirement_refuses_unsettled_exact_media_but_not_independent_media(){
        for status in ["running","unknown","dispatching","interrupted","queued-with-lease"]{
            let mut d=fixture(accounts::Profile::LikeAvto);historical_visual(&mut d);
            d["jobs"]=json!([{"id":"active","kind":"media_audio","account":d["account"],"refId":"p",
                "status":if status=="queued-with-lease"{"queued"}else{status}}]);
            if status=="queued-with-lease"{d["jobs"][0]["result"]["visualProgress"]["leaseId"]=json!("owned");}
            let before=d.clone();assert!(retire_visual_for_test(&mut d,"p").is_err(),"{status}");assert_eq!(d,before);
        }
        let mut d=fixture(accounts::Profile::LikeAvto);historical_visual(&mut d);
        d["jobs"]=json!([{"id":"other","kind":"media","refId":"different-post","status":"running"}]);
        retire_visual_for_test(&mut d,"p").unwrap();
    }
    #[test]
    fn retirement_blocks_unsettled_group_peer_with_different_provider_source(){
        for status in ["unknown","interrupted","running","dispatching"]{
            for group_only in [false,true]{
                let mut d=fixture(accounts::Profile::LikeAvto);
                d["posts"][0]["title"]=json!("Exact shared retired policy review");
                d["posts"][0]["sourceUrl"]=json!("https://youtu.be/AbCdEf123_-");
                d["posts"][0]["durationMs"]=json!(44000);
                d["posts"][0]["canonicalMediaId"]=json!("verified-retirement-source");
                let mut peer=d["posts"][0].clone();peer["id"]=json!("peer");peer["postKey"]=json!("provider:peer");
                peer["sourceUrl"]=json!("https://instagram.com/reel/ABC/");peer["durationMs"]=json!(44800);
                list_mut(&mut d,"posts").push(peer.clone());historical_visual(&mut d);
                let scope=text(&d,"account");let groups=knowledge::visual_groups(&d,scope);
                assert_eq!(groups.get("p"),groups.get("peer"));assert!(groups.contains_key("p"));
                assert_ne!(knowledge::media_source_key(&d["posts"][0],scope),knowledge::media_source_key(&peer,scope));
                d["jobs"]=json!([{"id":"group-peer","kind":"media","account":d["account"],
                    "connectorBinding":active_binding(&d).unwrap().to_json(),"status":status,
                    "refId":if group_only{"missing-reference"}else{"peer"},
                    "groupKey":if group_only{format!("visual-v2:{}",groups.get("p").unwrap())}else{String::new()}}]);
                let before=d.clone();assert!(retire_visual_for_test(&mut d,"p").is_err(),"{status}/{group_only}");
                assert_eq!(d,before);
            }
        }
    }
    #[test]
    fn new_scene_override_rejected_but_historical_policy_remains_readable(){
        let mut d=fixture(accounts::Profile::BawRussia);let before=d.clone();
        assert!(update(&mut d,DEFAULT).is_err());assert_eq!(d,before);
        let historical=historical_visual(&mut d);let before=d.clone();
        assert_eq!(update(&mut d,DEFAULT).unwrap(),historical);assert_eq!(d,before);
    }
    fn probe(d:&mut Value,duration:u64){
        let post=&d["posts"][0];let binding=active_binding(d).unwrap().to_json();
        let mut p=media_fullframes::initial(text(d,"account"),&binding,post,"now");
        p["source"]=json!({"sha256":"a".repeat(64),"bytes":1024});p["phase"]=json!("inventory");
        p["sourceIdentity"]=json!({"account":d["account"],"postKey":post["postKey"],"mediaSha256":"a".repeat(64),"durationMs":duration});
        d["jobs"]=json!([{"id":"probed","kind":"media","purpose":"auto_media","status":"queued","connectorBinding":binding,"visualContractVersion":2,"account":d["account"],"refId":"p","result":{"visualProgress":p}}]);
    }
    #[test]
    fn speech_default_retains_exact_legacy_duration_boundary_without_requiring_scenes(){
        for (duration,audio_only) in [(179999,false),(180000,false),(180001,true),(3600000,true)]{
            let mut d=fixture(accounts::Profile::BawRussia);probe(&mut d,duration);
            let p=effective(&d,&d["posts"][0]).unwrap();
            assert_eq!(p["mode"],AUDIO_ONLY);assert_eq!(p["fullAudioRequired"],true);
            assert_eq!(p["visualRequired"],false);assert_eq!(p["ownerAuthorizedAudioOnly"],false);
            assert_eq!(p["decisionBasis"]["kind"],if audio_only{"probed_duration_threshold"}else{"default_full_video_speech"});
            assert_eq!(p["decisionBasis"]["audioOnlyAboveSeconds"],180);
        }
        let mut d=fixture(accounts::Profile::BawRussia);probe(&mut d,180001);
        let old=effective(&d,&d["posts"][0]).unwrap();
        d["settings"]["mediaPolicyDefaults"]=json!({"audioOnlyAboveSeconds":300});
        let longer=effective(&d,&d["posts"][0]).unwrap();assert_eq!(longer["mode"],AUDIO_ONLY);
        assert_eq!(longer["decisionBasis"]["kind"],"default_full_video_speech");
        assert_ne!(longer["policySha256"],old["policySha256"]);
        for invalid in [json!(0),json!(-1),json!(1.5),json!("180"),json!(86401),Value::Null]{
            d["settings"]["mediaPolicyDefaults"]["audioOnlyAboveSeconds"]=invalid;
            assert!(effective(&d,&d["posts"][0]).is_err());
        }
        d["settings"]["mediaPolicyDefaults"]=json!("malformed");assert!(effective(&d,&d["posts"][0]).is_err());
    }
    #[test]
    fn explicit_visual_policy_wins_and_stale_probe_never_becomes_duration_authority(){
        let mut d=fixture(accounts::Profile::BawRussia);probe(&mut d,3600000);
        assert_eq!(historical_visual(&mut d)["mode"],DEFAULT);
        d["settings"]["mediaPolicyDefaults"]=json!({"audioOnlyAboveSeconds":1});
        assert_eq!(effective(&d,&d["posts"][0]).unwrap()["visualRequired"],true);
        assert_eq!(update(&mut d,AUDIO_ONLY).unwrap()["ownerAuthorizedAudioOnly"],true);
        for kind in ["unknown","zero","source","account","binding","hash","origin_binding","post_account","cancelled","download","held_download","purpose"]{
            let mut d=fixture(accounts::Profile::BawRussia);probe(&mut d,3600000);
            match kind{
                "unknown"=>d["jobs"]=json!([]),
                "zero"=>d["jobs"][0]["result"]["visualProgress"]["sourceIdentity"]["durationMs"]=json!(0),
                "source"=>d["posts"][0]["title"]=json!("New source"),
                "account"=>d["jobs"][0]["account"]=json!("LikeAvto"),
                "binding"=>d["jobs"][0]["result"]["visualProgress"]["connectorBinding"]=accounts::Profile::LikeAvto.binding(),
                "origin_binding"=>d["jobs"][0]["connectorBinding"]=accounts::Profile::LikeAvto.binding(),
                "post_account"=>d["posts"][0]["accountId"]=json!("LikeAvto"),
                "cancelled"=>d["jobs"][0]["status"]=json!("cancelled"),
                "download"=>d["jobs"][0]["result"]["visualProgress"]["phase"]=json!("download"),
                "held_download"=>{d["jobs"][0]["result"]["visualProgress"]["phase"]=json!("held");d["jobs"][0]["result"]["visualProgress"]["resumePhase"]=json!("download");},
                "purpose"=>d["jobs"][0]["purpose"]=json!("other"),
                _=>d["jobs"][0]["result"]["visualProgress"]["sourceIdentity"]["mediaSha256"]=json!("b".repeat(64)),
            }
            let result=effective(&d,&d["posts"][0]).unwrap();
            assert_eq!(result["decisionBasis"]["kind"],"default_full_video_speech","{kind}");
            assert_eq!(result["ownerAuthorizedAudioOnly"],false,"{kind}");
        }
        let mut d=fixture(accounts::Profile::BawRussia);d["posts"][0]["durationSeconds"]=json!(3600);
        assert_eq!(effective(&d,&d["posts"][0]).unwrap()["decisionBasis"]["kind"],"default_full_video_speech");
    }
    #[test]
    fn snapshot_index_preserves_latest_valid_probe_and_fail_closed_policy(){
        let mut d=fixture(accounts::Profile::BawRussia);probe(&mut d,3600000);
        let original=d["jobs"][0].clone();
        d["jobs"][0]["createdAt"]=json!("2026-09-25T01:00:00Z");
        let mut latest=original.clone();latest["createdAt"]=json!("2026-09-25T02:00:00Z");
        latest["result"]["visualProgress"]["sourceIdentity"]["durationMs"]=json!(60000);
        let mut stale=original.clone();stale["createdAt"]=json!("2026-09-25T03:00:00Z");
        stale["result"]["visualProgress"]["sourceVersion"]=json!("stale");
        let mut other=original.clone();other["refId"]=json!("other");
        d["jobs"].as_array_mut().unwrap().extend([latest,stale,other]);
        let indexed=ProbedDurationIndex::new(&d);
        let post=&d["posts"][0];
        assert_eq!(effective_indexed(&d,post,&indexed).unwrap(),effective(&d,post).unwrap());
        assert_eq!(effective_indexed(&d,post,&indexed).unwrap()["mode"],AUDIO_ONLY);
        assert_eq!(effective_indexed(&d,post,&indexed).unwrap()["decisionBasis"]["durationMs"],60000);

        let mut changed=d.clone();changed["posts"][0]["title"]=json!("Changed source");
        let indexed=ProbedDurationIndex::new(&changed);
        let post=&changed["posts"][0];
        assert_eq!(effective_indexed(&changed,post,&indexed).unwrap(),effective(&changed,post).unwrap());
        assert_eq!(effective_indexed(&changed,post,&indexed).unwrap()["decisionBasis"]["kind"],"default_full_video_speech");
    }
    #[test]
    #[ignore = "manual throughput comparison for a representative queue snapshot"]
    fn benchmark_snapshot_index_many_posts_and_jobs(){
        use std::time::Instant;
        let mut d=fixture(accounts::Profile::LikeAvto);
        d["posts"]=(0..294).map(|i|json!({"id":format!("p{i:03}"),"postKey":format!("provider:p{i:03}"),
            "title":format!("Video {i}"),"attachments":[{"type":"video"}]})).collect();
        let account=d["account"].clone();let binding=active_binding(&d).unwrap().to_json();
        d["jobs"]=(0..9458).map(|i|json!({"id":format!("j{i}"),"kind":"media","purpose":"auto_media",
            "visualContractVersion":2,"account":account,"connectorBinding":binding,"status":"failed",
            "refId":format!("p{:03}",i%294)})).collect();
        let start=Instant::now();
        let indexed=ProbedDurationIndex::new(&d);
        let fast:Vec<_>=d["posts"].as_array().unwrap().iter().map(|post|effective_indexed(&d,post,&indexed).unwrap()).collect();
        let indexed_time=start.elapsed();
        let start=Instant::now();
        let direct:Vec<_>=d["posts"].as_array().unwrap().iter().map(|post|effective(&d,post).unwrap()).collect();
        let direct_time=start.elapsed();
        assert_eq!(fast,direct);
        eprintln!("294 posts / 9458 jobs: indexed {indexed_time:?}, direct {direct_time:?}");
    }
}
