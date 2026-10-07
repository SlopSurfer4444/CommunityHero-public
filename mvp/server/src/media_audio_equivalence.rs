//! Owner-confirmed directional reuse of one complete source transcript.
//! This is an assertion of media equivalence, never an assertion of equal bytes.
use crate::{ApiResult,App,Value,json};
use axum::{extract::{Path,Query,State},Extension,Json};
use sha2::{Digest,Sha256};
use std::collections::BTreeMap;

fn rows<'a>(d:&'a Value,k:&str)->&'a [Value]{d[k].as_array().map(Vec::as_slice).unwrap_or(&[])}
fn text<'a>(d:&'a Value,k:&str)->&'a str{d[k].as_str().unwrap_or("")}
fn hash(v:&Value)->String{format!("{:x}",Sha256::digest(v.to_string().as_bytes()))}
fn records(d:&Value)->Result<Option<&serde_json::Map<String,Value>>,&'static str>{
    match d["settings"].get("mediaAudioEquivalences"){
        None=>Ok(None),Some(v)=>v.as_object().filter(|m|m.len()<=1000).map(Some).ok_or("Invalid audio equivalence catalog")
    }
}
fn post<'a>(d:&'a Value,id:&str)->ApiResult<&'a Value>{
    let p=crate::row(d,"posts",id)?;
    let binding=crate::active_binding(d)?.to_json();
    if !crate::knowledge::is_video_post(p) || text(p,"postKey").is_empty()
        || !crate::knowledge::in_account(p,text(d,"account"))
        || (!text(p,"account").is_empty()&&p["account"]!=d["account"])
        || (!p["connectorBinding"].is_null()&&p["connectorBinding"]!=binding){return Err(crate::conflict("Video post company binding differs"));}
    Ok(p)
}
fn source_version(d:&Value,p:&Value)->String{crate::media_fullframes::source_version(p,text(d,"account"))}
fn eligible(d:&Value,source:&Value,v:&Value,at:&str)->bool{
    let Ok(now)=chrono::DateTime::parse_from_rfc3339(at) else{return false;};
    let valid_time=|key:&str,from:bool|v[key].is_null()||v[key].as_str().and_then(|s|chrono::DateTime::parse_from_rfc3339(s).ok())
        .is_some_and(|t|if from{t<=now}else{t>now});
    v["kind"]=="transcript"&&v["status"]=="active"&&!text(v,"text").trim().is_empty()
        && matches!(text(v,"trust"),"source_only"|"verified")&&v["scope"]["account"]==d["account"]
        && crate::knowledge::in_account(v,text(d,"account"))
        && (v["connectorBinding"].is_null()||crate::active_binding(d).is_ok_and(|b|v["connectorBinding"]==b.to_json()))
        && v["postKey"]==source["postKey"]&&rows(&v["scope"],"postKeys").iter().any(|k|*k==source["postKey"])
        && crate::knowledge::proven_full_audio(&v["transcription"],&source_version(d,source))
        && valid_time("validFrom",true)&&valid_time("validUntil",false)
        && rows(d,"knowledge_entries").iter().any(|e|e["id"]==v["entryId"]&&e["currentVersionId"]==v["id"])
}
fn resolve(d:&Value,target_id:&str,r:&Value,at:&str)->Option<Value>{
    let target=post(d,target_id).ok()?;let source=post(d,text(r,"sourcePostId")).ok()?;
    let binding=crate::active_binding(d).ok()?.to_json();
    if r["schemaVersion"]!=1||r["status"]!="active"||r["account"]!=d["account"]||r["connectorBinding"]!=binding
        || r["targetPostId"]!=target_id||r["targetPostKey"]!=target["postKey"]||r["sourcePostKey"]!=source["postKey"]
        || source["id"]==target["id"]||r["targetSourceVersion"]!=source_version(d,target)||r["sourceVersion"]!=source_version(d,source){return None;}
    let all=records(d).ok()??;
    if all.get(text(source,"id")).is_some_and(|s|s["status"]=="active")
        ||all.values().any(|s|s["status"]=="active"&&s["sourcePostId"]==target_id){return None;}
    let v=rows(d,"knowledge_versions").iter().find(|v|v["id"]==r["transcript"]["versionId"])?;
    if v["hash"]!=r["transcript"]["hash"]||v["entryId"]!=r["transcript"]["entryId"]||!eligible(d,source,v,at){return None;}
    Some(json!({"match":"owner_confirmed_audio_equivalence","authorization":"owner_confirmed_same_video",
        "equivalenceSha256":hash(r),"equivalenceRevision":r["revision"],"targetPostId":target["id"],"postKey":target["postKey"],
        "targetSourceVersion":r["targetSourceVersion"],"sourcePostId":source["id"],"sourcePostKey":source["postKey"],
        "sourceVersion":r["sourceVersion"],"account":r["account"],"connectorBinding":r["connectorBinding"],
        "transcript":r["transcript"],"identities":[],"byteEqualityClaimed":false}))
}
/// Callers have already validated the catalog. No recursive selection or edges.
pub(crate) fn bindings(d:&Value,at:&str)->Result<Vec<Value>,&'static str>{
    Ok(records(d)?.into_iter().flat_map(|m|m.iter()).filter_map(|(id,r)|resolve(d,id,r,at)).collect())
}
fn head(d:&Value,target:&Value)->ApiResult<Value>{
    let record=records(d).map_err(crate::internal)?.and_then(|m|m.get(text(target,"id"))).cloned().unwrap_or(Value::Null);
    Ok(json!({"targetPostId":target["id"],"targetSourceVersion":source_version(d,target),
        "headSha256":hash(&json!([d["account"],crate::active_binding(d)?.to_json(),target["id"],record])),"record":record}))
}
#[cfg(test)]
pub(crate) fn preview_for_test(d:&Value,target_id:&str,source_id:Option<&str>,at:&str)->ApiResult<Value>{
    preview(d,target_id,source_id,at)
}
fn preview(d:&Value,target_id:&str,source_id:Option<&str>,at:&str)->ApiResult<Value>{
    crate::knowledge::validate_catalog(d).map_err(crate::conflict)?;
    let target=post(d,target_id)?;let mut value=head(d,target)?;
    value["usable"]=json!(resolve(d,target_id,&value["record"],at).is_some());
    if let Some(id)=source_id{
        let source=post(d,id)?;
        let candidates:Vec<_>=rows(d,"knowledge_versions").iter().filter(|v|eligible(d,source,v,at)).map(|v|
            json!({"entryId":v["entryId"],"versionId":v["id"],"hash":v["hash"],"sourceMaterialId":v["sourceMaterialId"],
                "sourceRevision":v["sourceRevision"],"mediaSha256":v["mediaSha256"],"transcription":v["transcription"]})).collect();
        value["source"]=json!({"postId":id,"sourceVersion":source_version(d,source),"transcriptCandidates":candidates});
    }Ok(value)
}
pub(crate) fn set(d:&mut Value,target_id:&str,body:&Value,actor:&crate::operator_auth::Actor,at:&str,revoke:bool)->ApiResult<Value>{
    if actor.role!="owner"{return Err(crate::ApiError(axum::http::StatusCode::FORBIDDEN,"Owner required".into()));}
    let fields=if revoke{vec!["expectedHeadSha256","reason"]}else{vec!["expectedHeadSha256","expectedTargetSourceVersion","sourcePostId","expectedSourceVersion","transcriptVersionId","transcriptHash","reason"]};
    if body.as_object().is_none_or(|m|m.len()!=fields.len()||fields.iter().any(|f|!m.contains_key(*f))){return Err(crate::bad("Exact audio equivalence pins and reason required"));}
    let reason=body["reason"].as_str().map(str::trim).filter(|s|!s.is_empty()&&s.len()<=500).ok_or_else(||crate::bad("Bounded owner confirmation reason required"))?;
    crate::knowledge::validate_catalog(d).map_err(crate::conflict)?;
    let target=post(d,target_id)?.clone();let old=head(d,&target)?;
    if old["headSha256"]!=body["expectedHeadSha256"]{return Err(crate::conflict("Audio equivalence head changed"));}
    let revision=old["record"]["revision"].as_u64().unwrap_or(0).checked_add(1).ok_or_else(||crate::conflict("Audio equivalence revision exhausted"))?;
    let mut record=if revoke{
        if old["record"].is_null(){return Err(crate::conflict("Audio equivalence does not exist"));}
        let mut r=old["record"].clone();r["status"]=json!("revoked");r
    }else{
        let source_id=crate::required(body,"sourcePostId")?;let source=post(d,source_id)?;
        if source_id==target_id||old["targetSourceVersion"]!=body["expectedTargetSourceVersion"]||source_version(d,source)!=body["expectedSourceVersion"]{
            return Err(crate::conflict("Audio equivalence source changed or self mapping requested"));
        }
        if records(d).map_err(crate::internal)?.is_some_and(|m|
            m.get(source_id).is_some_and(|r|r["status"]=="active")||m.values().any(|r|r["status"]=="active"&&r["sourcePostId"]==target_id)){
            return Err(crate::conflict("Audio equivalence must be direct; chains and cycles are not allowed"));
        }
        let v=rows(d,"knowledge_versions").iter().find(|v|v["id"]==body["transcriptVersionId"]&&v["hash"]==body["transcriptHash"])
            .filter(|v|eligible(d,source,v,at)).ok_or_else(||crate::conflict("Pinned current complete source transcript unavailable"))?;
        json!({"schemaVersion":1,"status":"active","account":d["account"],"connectorBinding":crate::active_binding(d)?.to_json(),
            "targetPostId":target_id,"targetPostKey":target["postKey"],"targetSourceVersion":body["expectedTargetSourceVersion"],
            "sourcePostId":source_id,"sourcePostKey":source["postKey"],"sourceVersion":body["expectedSourceVersion"],
            "transcript":{"entryId":v["entryId"],"versionId":v["id"],"hash":v["hash"]}})
    };
    record["revision"]=json!(revision);record["reason"]=json!(reason);record["updatedAt"]=json!(at);record["updatedBy"]=actor.public_json();
    if d["settings"]["mediaAudioEquivalences"].is_null(){d["settings"]["mediaAudioEquivalences"]=json!({});}
    let map=d["settings"]["mediaAudioEquivalences"].as_object_mut().ok_or_else(||crate::internal("Invalid audio equivalence catalog"))?;
    if map.len()>=1000&&!map.contains_key(target_id){return Err(crate::bad("Audio equivalence catalog full"));}
    map.insert(target_id.to_owned(),record.clone());
    crate::list_mut(d,"audit").push(json!({"id":crate::id(),"action":if revoke{"media.audio_equivalence_revoked"}else{"media.audio_equivalence_confirmed"},
        "refId":target_id,"createdAt":at,"actor":actor.public_json(),"record":record}));
    preview(d,target_id,None,at)
}
pub(crate) fn invalidate_source_change(d:&mut Value,before:&Value,after:&Value)->ApiResult<()>{
    if source_version(d,before)==source_version(d,after){return Ok(());}
    let changed_id=text(before,"id");
    let ids:Vec<_>=records(d).map_err(crate::internal)?.into_iter().flat_map(|m|m.iter())
        .filter(|(_,r)|r["status"]=="active"&&(r["targetPostId"]==changed_id||r["sourcePostId"]==changed_id)).map(|(id,_)|id.clone()).collect();
    for id in ids{
        let r=&mut d["settings"]["mediaAudioEquivalences"][&id];r["status"]=json!("superseded");r["supersededAt"]=json!(crate::now());
        crate::audit(d,"media.audio_equivalence_source_changed",&id);
    }Ok(())
}
pub(crate) async fn get(State(app):State<App>,Path(id):Path<String>,Query(query):Query<BTreeMap<String,String>>)->ApiResult<Json<Value>>{
    if query.keys().any(|k|k!="sourcePostId"){return Err(crate::bad("Unknown audio equivalence query"));}
    preview(&app.read().await?,&id,query.get("sourcePostId").map(String::as_str),&crate::now()).map(Json)
}
pub(crate) async fn put(State(app):State<App>,Path(id):Path<String>,Extension(actor):Extension<crate::operator_auth::Actor>,Json(body):Json<Value>)->ApiResult<Json<Value>>{
    app.change(|d|set(d,&id,&body,&actor,&crate::now(),false).map(Json)).await
}
pub(crate) async fn delete(State(app):State<App>,Path(id):Path<String>,Extension(actor):Extension<crate::operator_auth::Actor>,Json(body):Json<Value>)->ApiResult<Json<Value>>{
    app.change(|d|set(d,&id,&body,&actor,&crate::now(),true).map(Json)).await
}

#[cfg(test)]
pub(crate) mod tests{
    use super::*;
    const AT:&str="2026-09-25T00:00:00Z";
    pub(crate) fn fixture()->Value{
        let mut d=crate::empty();crate::accounts::initialize(&mut d,crate::accounts::Profile::BawRussia).unwrap();
        d["posts"]=json!([
            {"id":"target","postKey":"12182:target","objectId":"12182","title":"VK title","sourceUrl":"https://vk.com/video-1_1","attachments":[{"type":"video"}]},
            {"id":"source","postKey":"12185:source","objectId":"12185","title":"Different YouTube title","sourceUrl":"https://www.youtube.com/watch?v=AbCdEf123_-","attachments":[{"type":"video"}]}]);
        d["items"]=json!([{"id":"i","itemId":"i","objectId":"12182","postId":"target","postKey":"12182:target","branchId":"b","revision":1,"workflow":"attention","providerStatus":"new","conversationKey":"12182:t"}]);
        d["branches"]=json!([{"id":"b","postId":"target","messages":[{"id":"i","text":"What happened in the video?"}],"contextComplete":false}]);
        d["materials"]=json!([{"id":"speech","account":d["account"],"postKey":"12185:source","title":"Full source audio","kind":"transcript","revision":1,
            "sourceUrl":d["posts"][1]["sourceUrl"],"mediaSha256":"a".repeat(64),"text":"Complete original source words",
            "transcription":{"sourceVersion":source_version(&d,&d["posts"][1]),"partial":false,"coverage":"full_audio","mediaDurationSeconds":1200.0,"audioDurationSeconds":1200.0}}]);
        crate::knowledge::sync_catalog(&mut d,AT).unwrap();d
    }
    fn body(d:&Value)->Value{
        let view=preview(d,"target",Some("source"),AT).unwrap();let candidate=&view["source"]["transcriptCandidates"][0];
        json!({"expectedHeadSha256":view["headSha256"],"expectedTargetSourceVersion":view["targetSourceVersion"],"sourcePostId":"source",
            "expectedSourceVersion":view["source"]["sourceVersion"],"transcriptVersionId":candidate["versionId"],"transcriptHash":candidate["hash"],"reason":"Owner confirmed these two posts contain the same complete video"})
    }
    fn confirm(d:&mut Value)->Value{let b=body(d);set(d,"target",&b,&crate::operator_auth::Actor::local_owner("test"),AT,false).unwrap()}
    fn audio_policy(d:&mut Value){
        d["settings"]["postMediaPolicies"]=json!({"target":{"version":1,"revision":1,"status":"active","postId":"target",
            "account":d["account"],"connectorBinding":crate::active_binding(d).unwrap().to_json(),"sourceVersion":source_version(d,&d["posts"][0]),"mode":"full_audio_only"}});
    }
    #[test]
    fn exact_owner_reuse_keeps_original_material_and_pins_bundle_provenance(){
        let mut d=fixture();audio_policy(&mut d);
        let lookup=crate::knowledge::TranscriptLookup::new(&d,AT).unwrap();assert!(!lookup.ready_for_policy(&d["posts"][0],false).unwrap());
        let before_materials=d["materials"].clone();let before_versions=d["knowledge_versions"].clone();
        let confirmed=confirm(&mut d);assert_eq!(confirmed["usable"],true);
        let lookup=crate::knowledge::TranscriptLookup::new(&d,AT).unwrap();assert!(lookup.ready_for_policy(&d["posts"][0],false).unwrap());assert!(!lookup.ready(&d["posts"][0]).unwrap());
        let b=crate::prepare_bundle::build(&d,&[json!("i")],&[]).unwrap();
        let material=rows(&b["request"],"materials").iter().find(|m|m["kind"]=="transcript").unwrap();
        assert_eq!(material["postKey"],"12185:source");assert_eq!(material["transcription"],d["materials"][0]["transcription"]);
        let edge=&material["audioEquivalence"][0];assert_eq!(edge["postKey"],"12182:target");assert_eq!(edge["byteEqualityClaimed"],false);
        assert_ne!(edge["targetSourceVersion"],edge["sourceVersion"]);
        assert!(rows(&b["request"],"knowledgeManifest").iter().any(|m|rows(m,"mediaBinding").contains(edge)));
        assert_eq!(d["materials"],before_materials);assert_eq!(d["knowledge_versions"],before_versions);
        assert_eq!(b["request"]["posts"][0]["visualContextStatus"],"missing");
        let revoked=set(&mut d,"target",&json!({"expectedHeadSha256":confirmed["headSha256"],"reason":"Owner revoked equivalence"}),&crate::operator_auth::Actor::local_owner("test"),AT,true).unwrap();
        assert_eq!(revoked["usable"],false);assert!(crate::prepare_bundle::current(&d,&b).is_err());
        assert!(!crate::knowledge::TranscriptLookup::new(&d,AT).unwrap().ready_for_policy(&d["posts"][0],false).unwrap());
    }
    #[test]
    fn missing_partial_unknown_and_wrong_source_audio_cannot_be_confirmed(){
        for case in ["missing","partial","coverage","duration","wrong_source","empty"]{
            let mut d=fixture();let b=body(&d);
            match case{
                "missing"=>d["materials"]=json!([]),
                "partial"=>d["materials"][0]["transcription"]["partial"]=json!(true),
                "coverage"=>{d["materials"][0]["transcription"].as_object_mut().unwrap().remove("coverage");},
                "duration"=>d["materials"][0]["transcription"]["audioDurationSeconds"]=json!(600),
                "wrong_source"=>d["materials"][0]["transcription"]["sourceVersion"]=json!("foreign"),
                _=>d["materials"][0]["text"]=json!(""),
            }
            if case=="missing"{d["knowledge_entries"]=json!([]);d["knowledge_versions"]=json!([]);}else{crate::knowledge::sync_catalog(&mut d,AT).unwrap();}
            assert!(preview(&d,"target",Some("source"),AT).unwrap()["source"]["transcriptCandidates"].as_array().unwrap().is_empty(),"{case}");
            assert!(set(&mut d,"target",&b,&crate::operator_auth::Actor::local_owner("test"),AT,false).is_err(),"{case}");
        }
    }
    #[test]
    fn transcript_foreign_company_or_connector_is_ineligible_before_selection(){
        let d=fixture();let original=d["knowledge_versions"][0].clone();
        assert!(eligible(&d,&d["posts"][1],&original,AT));
        for field in ["accountId","scope","sourceMediaScope","foreign_binding","other_connector"]{
            let mut version=original.clone();
            match field{
                "accountId"=>version[field]=json!("LikeAvto"),
                "scope"|"sourceMediaScope"=>version[field]["account"]=json!("LikeAvto"),
                "foreign_binding"=>version["connectorBinding"]=crate::accounts::Profile::LikeAvto.binding(),
                _=>{version["connectorBinding"]=crate::active_binding(&d).unwrap().to_json();version["connectorBinding"]["connectorId"]=json!("another-connector");}
            }
            assert!(!eligible(&d,&d["posts"][1],&version,AT),"{field}");
        }
    }
    #[test]
    fn owner_cas_and_company_scope_are_required(){
        for case in ["operator","head","target","source","version","hash","company","binding"]{
            let mut d=fixture();let mut b=body(&d);let mut actor=crate::operator_auth::Actor::local_owner("test");
            match case{
                "operator"=>actor.role="operator".into(),"head"=>b["expectedHeadSha256"]=json!("wrong"),
                "target"=>b["expectedTargetSourceVersion"]=json!("wrong"),"source"=>b["expectedSourceVersion"]=json!("wrong"),
                "version"=>b["transcriptVersionId"]=json!("wrong"),"hash"=>b["transcriptHash"]=json!("wrong"),
                "company"=>d["posts"][1]["account"]=json!("LikeAvto"),
                _=>d["posts"][1]["connectorBinding"]=crate::accounts::Profile::LikeAvto.binding(),
            }
            let before=d.clone();assert!(set(&mut d,"target",&b,&actor,AT,false).is_err(),"{case}");assert_eq!(d,before);
        }
        let mut d=fixture();let b=body(&d);confirm(&mut d);assert!(set(&mut d,"target",&b,&crate::operator_auth::Actor::local_owner("test"),AT,false).is_err());
        let mut foreign=fixture();crate::accounts::initialize(&mut foreign,crate::accounts::Profile::LikeAvto).unwrap_err();
        foreign["account"]=json!("LikeAvto");foreign["connectorBinding"]=crate::accounts::Profile::LikeAvto.binding();
        foreign["settings"]["mediaAudioEquivalences"]=d["settings"]["mediaAudioEquivalences"].clone();assert!(bindings(&foreign,AT).unwrap().is_empty());
    }
    #[test]
    fn changed_post_on_either_side_supersedes_and_returning_source_cannot_reactivate(){
        for index in [0,1]{
            let mut d=fixture();confirm(&mut d);let before=d["posts"][index].clone();let mut after=before.clone();after["title"]=json!("Changed video");
            d["posts"][index]=after.clone();assert!(bindings(&d,AT).unwrap().is_empty());
            invalidate_source_change(&mut d,&before,&after).unwrap();d["posts"][index]=before;
            assert!(bindings(&d,AT).unwrap().is_empty());assert_eq!(d["settings"]["mediaAudioEquivalences"]["target"]["status"],"superseded");
        }
    }
    #[test]
    fn foreign_post_account_aliases_cannot_confirm_or_satisfy_readiness(){
        for index in [0,1]{for field in ["accountId","scope","sourceMediaScope"]{
            let mut d=fixture();audio_policy(&mut d);confirm(&mut d);let b=body(&d);
            if field=="accountId"{d["posts"][index][field]=json!("LikeAvto");}
            else{d["posts"][index][field]=json!({"account":"LikeAvto"});}
            assert!(preview(&d,"target",Some("source"),AT).is_err(),"{index}/{field}");
            assert!(set(&mut d,"target",&b,&crate::operator_auth::Actor::local_owner("test"),AT,false).is_err(),"{index}/{field}");
            assert!(bindings(&d,AT).unwrap().is_empty());
            assert!(!crate::knowledge::TranscriptLookup::new(&d,AT).unwrap().ready_for_policy(&d["posts"][0],false).unwrap());
        }}
    }
    #[test]
    fn transcript_revision_drift_requires_a_new_exact_owner_confirmation(){
        let mut d=fixture();audio_policy(&mut d);let prior=confirm(&mut d);let bundle=crate::prepare_bundle::build(&d,&[json!("i")],&[]).unwrap();
        d["materials"][0]["text"]=json!("Corrected complete transcript");d["materials"][0]["revision"]=json!(2);crate::knowledge::sync_catalog(&mut d,AT).unwrap();
        assert!(bindings(&d,AT).unwrap().is_empty());assert!(crate::prepare_bundle::current(&d,&bundle).is_err());
        assert!(!crate::knowledge::TranscriptLookup::new(&d,AT).unwrap().ready_for_policy(&d["posts"][0],false).unwrap());
        let new=confirm(&mut d);assert_eq!(new["usable"],true);assert_ne!(prior["record"]["transcript"],new["record"]["transcript"]);
        assert!(d["knowledge_versions"].as_array().unwrap().len()>1);
    }
    #[test]
    fn canonical_ranking_cannot_drop_owner_pinned_source_revision(){
        let mut d=fixture();
        for post in d["posts"].as_array_mut().unwrap(){post["canonicalMediaId"]=json!("shared-canonical-video");}
        d["materials"][0]["transcription"]["sourceVersion"]=json!(source_version(&d,&d["posts"][1]));
        crate::knowledge::sync_catalog(&mut d,AT).unwrap();audio_policy(&mut d);confirm(&mut d);
        let pinned=d["settings"]["mediaAudioEquivalences"]["target"]["transcript"]["versionId"].clone();
        let mut competitor=d["materials"][0].clone();competitor["id"]=json!("aaa-preferred");
        competitor["text"]=json!("A competing transcript must not replace the attested revision");
        competitor["transcription"]["audioDurationSeconds"]=json!(1300.0);
        competitor["transcription"]["mediaDurationSeconds"]=json!(1300.0);
        competitor["transcription"]["maxAudioSeconds"]=json!(1300);
        d["materials"].as_array_mut().unwrap().push(competitor);
        crate::knowledge::sync_catalog(&mut d,AT).unwrap();
        let selection=crate::knowledge::select(&d,&[],&[d["posts"][0].clone(),d["posts"][1].clone()],AT).unwrap();
        assert!(rows(&selection,"materials").iter().any(|m|m["knowledgeVersionId"]==pinned&&m["audioEquivalence"].as_array().is_some_and(|a|!a.is_empty())));
        assert!(rows(&selection,"manifest").iter().any(|m|m["versionId"]==pinned));
        assert!(rows(&selection,"materials").iter().any(|m|m["id"]=="aaa-preferred"));
    }
    #[test]
    fn self_mapping_chains_and_cycles_are_rejected_without_transitive_proof(){
        let mut d=fixture();confirm(&mut d);
        let view=preview(&d,"source",Some("target"),AT).unwrap();
        let reverse=json!({"expectedHeadSha256":view["headSha256"],"expectedTargetSourceVersion":view["targetSourceVersion"],"sourcePostId":"target",
            "expectedSourceVersion":view["source"]["sourceVersion"],"transcriptVersionId":"anything","transcriptHash":"anything","reason":"cycle"});
        assert!(set(&mut d,"source",&reverse,&crate::operator_auth::Actor::local_owner("test"),AT,false).is_err());
        let mut self_link=body(&d);self_link["sourcePostId"]=json!("target");assert!(set(&mut d,"target",&self_link,&crate::operator_auth::Actor::local_owner("test"),AT,false).is_err());
        let mut third=d["posts"][0].clone();third["id"]=json!("third");third["postKey"]=json!("12182:third");d["posts"].as_array_mut().unwrap().push(third);
        let view=preview(&d,"third",None,AT).unwrap();let mut chain=reverse;
        chain["expectedHeadSha256"]=view["headSha256"].clone();chain["expectedTargetSourceVersion"]=view["targetSourceVersion"].clone();
        assert!(set(&mut d,"third",&chain,&crate::operator_auth::Actor::local_owner("test"),AT,false).is_err());
    }
}
