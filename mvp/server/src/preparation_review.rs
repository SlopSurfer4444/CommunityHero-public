//! Durable second-pass evidence. Research is source material, never active policy.
use serde_json::{json, Value};
use std::collections::BTreeSet;

fn rows<'a>(v:&'a Value,key:&str)->&'a [Value]{v[key].as_array().map(Vec::as_slice).unwrap_or(&[])}
fn text(v:&Value,key:&str,max:usize)->Result<String,&'static str>{
    v[key].as_str().filter(|s|s.encode_utf16().count()<=max).map(str::to_owned).ok_or("Invalid preparation review text")
}
fn ids(request:&Value)->Result<BTreeSet<String>,&'static str>{
    let items=request["items"].as_array().filter(|v|!v.is_empty()&&v.len()<=100).ok_or("Review requires attached items")?;
    let mut ids=BTreeSet::new();
    for item in items {let id=text(item,"id",256)?;if id.is_empty()||!ids.insert(id){return Err("Invalid review recipients")}}
    Ok(ids)
}
/// Persist a useful category, not raw adapter diagnostics or private inputs.
/// Only established transport/runtime failures retain the bounded retry policy.
pub(super) fn failure_category(reason:&str)->(&'static str,bool){
    for code in ["ASSISTANT_INVALID_RESEARCH_UNOBSERVED_URL","ASSISTANT_INVALID_RESEARCH_MISSING_EVIDENCE",
        "ASSISTANT_INVALID_RESEARCH_RECIPIENT","ASSISTANT_INVALID_RESEARCH_FIELDS",
        "ASSISTANT_INVALID_RESEARCH_UNATTRIBUTED_REPLY","ASSISTANT_INVALID_RESEARCH_ACTIVITY_ID",
        "ASSISTANT_ISOLATION_FAILED","ASSISTANT_RESEARCH_LIMIT","ASSISTANT_INVALID_RESEARCH",
        "ASSISTANT_INVALID_RESPONSE","ASSISTANT_INVALID_REQUEST","ASSISTANT_CONTEXT_TOO_LARGE",
        "ASSISTANT_AUTH_UNAVAILABLE","ASSISTANT_UNAVAILABLE","CANCELLED"] {
        if reason.contains(code){return (code,false);}
    }
    for code in ["ASSISTANT_BUSY","ASSISTANT_FAILED","ADAPTER_TIMEOUT"] {
        if reason.contains(code){return (code,true);}
    }
    if reason.contains("Adapter timed out"){return ("ADAPTER_TIMEOUT",true);}
    if reason.contains("Adapter process failed"){return ("ADAPTER_PROCESS_FAILED",true);}
    if reason.contains("Preparation evidence changed")||reason.contains("changed before model call") {
        return ("PREPARATION_CONTEXT_CHANGED",false);
    }
    ("REVIEW_FAILED",false)
}
pub(super) fn failure_message(reason:&str)->String{
    let (code,_)=failure_category(reason);
    format!("Усиленная проверка не завершена ({code}). Первый разбор сохранён; непроверенное решение не принято.")
}
fn clean_result(result:&Value,allowed:&BTreeSet<String>)->Result<Value,&'static str>{
    if result["sources"].as_array().is_none_or(|v|!v.is_empty())||result["text"].as_str().is_none_or(|s|s.trim().is_empty()){return Err("Invalid preparation result")}
    let assessments=result["assessments"].as_array().filter(|a|a.len()==allowed.len()).ok_or("Review assessment coverage mismatch")?;
    let proposals=result["proposals"].as_array().filter(|a|a.len()<=allowed.len()).ok_or("Invalid review proposals")?;
    let mut seen=BTreeSet::new();let mut cleaned=Vec::new();let mut clean_proposals=Vec::new();
    for a in assessments {
        let item=text(a,"itemId",256)?;
        if !allowed.contains(&item)||!seen.insert(item.clone()){return Err("Foreign or duplicate review recipient")}
        let outcome=text(a,"outcome",30)?;
        if !["close","reply","needs_attention"].contains(&outcome.as_str()){return Err("Invalid review decision")}
        let reason=text(a,"reason",2000)?;if reason.trim().is_empty(){return Err("Review reason missing")}
        let empty_tags=Vec::new();
        let tags=match a.get("tags") {None=>&empty_tags,Some(v)=>v.as_array().filter(|v|v.len()<=3).ok_or("Invalid review tags")?};
        let mut tag_seen=BTreeSet::new();
        for tag in tags {let t=tag.as_str().ok_or("Invalid review tag")?;
            if !["complaint","needs_fact","moderation","missing_context","purchase","question","feedback"].contains(&t)||!tag_seen.insert(t){return Err("Invalid review tag")}}
        let target_proposals:Vec<_>=proposals.iter().filter(|p|p["itemId"]==item).collect();
        if outcome=="needs_attention" {if !target_proposals.is_empty(){return Err("Review decision disagrees with proposal")}}
        else {
            if target_proposals.len()!=1{return Err("Review decision requires one proposal")}
            let p=target_proposals[0];let kind=if outcome=="reply"{"reply_and_close"}else{"close"};
            if p["kind"]!=kind{return Err("Review proposal kind mismatch")}
            let body=text(p,"text",12000)?;
            if outcome=="reply"&&body.trim().is_empty(){return Err("Review reply missing")}
            if outcome=="close"&&!body.is_empty(){return Err("Review close must have empty text")}
            clean_proposals.push(json!({"itemId":item,"kind":kind,"text":body}));
        }
        cleaned.push(json!({"itemId":item,"outcome":outcome,"reason":reason,"tags":tags}));
    }
    if proposals.iter().any(|p|p["itemId"].as_str().is_none_or(|id|!allowed.contains(id))){return Err("Foreign review proposal")}
    Ok(json!({"text":text(result,"text",60000)?,"sources":[],"assessments":cleaned,"proposals":clean_proposals}))
}

// Select a stronger review, never decide the price or endorse the comparison.
// Inspect the selected comment only: a price-bearing post/branch must not turn
// every greeting or numeric joke beneath it into a researched reply.
fn price_question_or_comparison(item:&Value)->bool {
    let source=item["text"].as_str().or_else(||item["preview"].as_str()).unwrap_or("");
    let lower=source.to_lowercase().replace('ё',"е");
    let words:Vec<&str>=lower.split(|c:char|!c.is_alphanumeric()).filter(|s|!s.is_empty()).collect();
    let normalized=format!(" {} ",words.join(" "));
    let phrase=|value:&str|normalized.contains(&format!(" {value} "));
    let price=words.iter().any(|word|matches!(*word,
        "цена"|"цены"|"цену"|"цене"|"ценой"|"ценам"|"ценах"|
        "дорого"|"дорогой"|"дорогая"|"дорогие"|"дорогую")
        ||["ценник","стоимост","дешев","дороже","переплат","нацен"].iter().any(|stem|word.starts_with(stem)));
    let currency=source.contains(['₽','¥','$','€'])||words.iter().any(|word|{
        let word=word.trim_start_matches(|c:char|c.is_ascii_digit());
        word=="млн"||["рубл","юан","доллар","миллион"].iter().any(|stem|word.starts_with(stem))
    });
    let china=words.iter().any(|word|word.starts_with("кита")||*word=="кнр");
    let russia=words.iter().any(|word|word.starts_with("росси")||*word=="рф");
    let market_contrast=(china&&(russia||phrase("у нас")))
        ||(phrase("у них")&&phrase("у нас"));
    let objection=words.contains(&"почему")||words.contains(&"откуда")
        ||phrase("за что")||phrase("не верю")||phrase("не может стоить");
    let quote_question=words.contains(&"почем")
        ||(words.contains(&"сколько")&&words.iter().any(|word|matches!(*word,"стоит"|"стоят")));
    quote_question||((price||currency)&&(market_contrast||objection||source.contains('?')))
}

pub(super) fn plan_review(request:&Value,first:&Value)->Result<Option<Value>,&'static str>{
    let clean=clean_result(first,&ids(request)?)?;
    if deterministic_media_hold(request,first)? {return Ok(None);}
    let needed=rows(&clean,"assessments").iter().any(|a|a["outcome"]=="close"||a["outcome"]=="needs_attention"
        || rows(a,"tags").iter().any(|t|matches!(t.as_str(),Some("complaint"|"needs_fact"|"moderation"|"question"|"purchase"))))
        || rows(request,"items").iter().any(price_question_or_comparison);
    if !needed{return Ok(None)}
    let mut review=request.clone();review["purpose"]=json!("triage_review");review["firstPass"]=clean;
    review["firstPass"]["trust"]=json!("untrusted_model_output");
    Ok(Some(review))
}

// Verify the adapter's pre-model hold against raw immutable selected-comment
// evidence. Download failures, unsupported video capability and unknown absence
// never authorize this exception to second-pass review.
fn deterministic_media_hold(request:&Value,result:&Value)->Result<bool,&'static str>{
    let Some(marker)=result.get("decisionSource") else {return Ok(false);};
    let invalid="Invalid deterministic media source hold";
    if marker!="deterministic_media_source_gap" || request["purpose"]!="triage"
        || rows(request,"items").len()!=1 || result.get("runMetadata").is_some()
        || rows(result,"assessments").len()!=1 || !rows(result,"proposals").is_empty()
        || result["assessments"][0]["outcome"]!="needs_attention"
        || result["assessments"][0]["tags"]!=json!(["missing_context"])
        || result["assessments"][0]["itemId"]!=request["items"][0]["id"] {return Err(invalid);}
    let item=&request["items"][0];
    let present=item["attachmentsState"]=="present"||item["commentAttachmentsPresent"]==true;
    let raw=item.get("attachments").filter(|v|!v.is_null()).or_else(||item.get("commentAttachments").filter(|v|!v.is_null()));
    let Some(raw)=raw else {return if present {Ok(true)}else{Err(invalid)};};
    let attachments=raw.as_array().filter(|v|v.len()<=20).ok_or(invalid)?;
    let mut gap=present&&attachments.is_empty();
    for a in attachments {
        if !a.is_object(){return Err(invalid);}
        for key in ["url","source_url","preview_url","title"] {
            if a[key].as_str().is_some_and(|s|s.encode_utf16().count()>8192) {return Err(invalid);}
        }
        let has=|key:&str|a[key].as_str().is_some_and(|s|!s.is_empty());
        match a["type"].as_str() {
            Some("photo"|"image"|"sticker")=>gap|=!has("url"),
            Some("video")=>(),
            _=>gap|=!has("url")&&!has("source_url")&&!has("preview_url"),
        }
    }
    if gap {Ok(true)}else{Err(invalid)}
}

pub(super) fn sanitize_research(value:&Value,allowed:&BTreeSet<String>)->Result<Value,&'static str>{
    if value["version"]!=1||!matches!(value["status"].as_str(),Some("completed"|"no_sources")){return Err("Unsupported research provenance")}
    let mut clean=json!({"version":1,"status":value["status"],"trust":"source_only"});
    for key in ["model","reasoningEffort"] {let v=text(value,key,120)?;if v.is_empty(){return Err("Research provenance missing")};clean[key]=json!(v);}
    for key in ["instructionSha256","inputSha256"] {
        let v=text(value,key,64)?;if v.len()!=64||!v.bytes().all(|b|b.is_ascii_hexdigit()){return Err("Invalid research digest")};clean[key]=json!(v);
    }
    if let Some(profile)=value.get("toolsProfileSha256") {
        let hash=profile.as_str().filter(|v|v.len()==64&&v.bytes().all(|b|b.is_ascii_hexdigit())).ok_or("Invalid research tools profile")?;
        clean["toolsProfileSha256"]=json!(hash);
    }
    let completed=text(value,"completedAt",80)?;
    if chrono::DateTime::parse_from_rfc3339(&completed).is_err(){return Err("Invalid research completion time")}
    clean["completedAt"]=json!(completed);
    let elapsed=value["elapsedMs"].as_u64().ok_or("Invalid research duration")?;
    let calls=value["webCalls"].as_u64().filter(|n|*n<=8).ok_or("Invalid research call count")?;
    clean["elapsedMs"]=json!(elapsed);clean["webCalls"]=json!(calls);
    let sources=value["sources"].as_array().filter(|s|s.len()<=30).ok_or("Invalid research sources")?;
    if (!sources.is_empty()&&calls==0)||(value["status"]=="completed"&&sources.is_empty())||(value["status"]=="no_sources"&&!sources.is_empty()){return Err("Research status disagrees with evidence")}
    let mut clean_sources=Vec::new();
    for source in sources {
        let item=text(source,"itemId",256)?;if !allowed.contains(&item){return Err("Foreign research recipient")}
        let url=text(source,"url",2048)?;
        let uri=url.parse::<axum::http::Uri>().map_err(|_|"Invalid research URL")?;
        if !matches!(uri.scheme_str(),Some("http"|"https"))||uri.host().is_none()||uri.authority().is_some_and(|a|a.as_str().contains('@'))||url.chars().any(|c|c.is_control()||c.is_whitespace()){return Err("Invalid research URL")}
        clean_sources.push(json!({"itemId":item,"url":url,"title":text(source,"title",500)?,"claim":text(source,"claim",6000)?,"trust":"source_only"}));
    }
    clean["sources"]=json!(clean_sources);Ok(clean)
}

pub(super) fn record_first(d:&mut Value,job:&str,result:&Value,at:&str)->super::ApiResult<Option<Value>>{
    let record=super::row(d,"jobs",job)?.clone();
    if record["status"]!="running"{return Err(super::conflict("Preparation job no longer running"))}
    let request=&record["prepareBundle"]["request"];
    let allowed=ids(request).map_err(super::bad)?;
    let mut clean=clean_result(result,&allowed).map_err(super::bad)?;
    let deterministic=deterministic_media_hold(request,result).map_err(super::bad)?;
    if deterministic {
        super::prepare_bundle::current(d,&record["prepareBundle"]).map_err(super::conflict)?;
        clean["decisionSource"]=json!("deterministic_media_source_gap");
    }
    if let Some(metadata)=super::prepare_bundle::generation_metadata(result).map_err(super::bad)?{clean["runMetadata"]=metadata;}
    let plan=plan_review(request,result).map_err(super::bad)?;
    let stage=json!({"status":"completed","at":at,"result":clean,"reviewRequired":plan.is_some(),"reason":if deterministic{"deterministic_media_source_gap"}else if plan.is_some(){"decision_or_substantive_question"}else{"routine_feedback_reply"},"trust":if deterministic{"source_evidence_only"}else{"untrusted_model_output"}});
    let old=&record["preparationStages"]["first"];
    if !old.is_null(){if old["result"]==stage["result"]{return Ok(plan)}return Err(super::conflict("First-pass evidence is immutable"))}
    super::row_mut(d,"jobs",job)?["preparationStages"]["first"]=stage;Ok(plan)
}

pub(super) fn record_review(d:&mut Value,job:&str,outcome:Result<&Value,&str>,at:&str)->super::ApiResult<()> {
    let record=super::row(d,"jobs",job)?.clone();
    if record["preparationStages"]["first"]["reviewRequired"]!=true{return Err(super::conflict("Review has no persisted first pass"))}
    let request=&record["prepareBundle"]["request"];
    let allowed=ids(request).map_err(super::bad)?;
    let stage=match outcome {
        Err(reason)=>{
            let (code,retryable)=failure_category(reason);
            json!({"status":"failed","at":at,"error":"Stronger review failed; first pass retained","errorCode":code,"retryable":retryable})
        },
        Ok(result)=>{
            let mut clean=clean_result(result,&allowed).map_err(super::bad)?;
            let metadata=super::prepare_bundle::generation_metadata(result).map_err(super::bad)?.ok_or_else(||super::bad("Review generation metadata missing"))?;
            let research=sanitize_research(&metadata["research"],&allowed).map_err(super::bad)?;
            clean["runMetadata"]=metadata;
            json!({"status":"completed","at":at,"result":clean,"research":research})
        }
    };
    let old=&record["preparationStages"]["review"];
    if !old.is_null(){if old["status"]==stage["status"]&&old["result"]==stage["result"]{return Ok(())}return Err(super::conflict("Review evidence is immutable"))}
    let bindings:Vec<_>=rows(request,"items").iter().map(|i|json!({"itemId":i["id"],"postId":i["postId"],"postKey":i["postKey"],"objectId":i["objectId"],"providerItemId":i["itemId"]})).collect();
    let posts:Vec<_>=rows(request,"posts").iter().map(|p|{
        let mut projected=serde_json::Map::new();
        for key in ["id","postKey","title","text","channel","canonicalMediaId","contentSha256","mediaSha256","sourceUrl","account"] {if let Some(v)=p.get(key){projected.insert(key.into(),v.clone());}}
        if let Some(original)=rows(d,"posts").iter().find(|original|original["id"]==p["id"]){projected.insert("isVideo".into(),json!(super::knowledge::is_video_post(original)));}
        Value::Object(projected)
    }).collect();
    let mut archive=json!({"id":format!("research:{job}"),"jobId":job,"account":request["account"],"connectorBinding":request["connectorBinding"],"prepareBundleId":record["prepareBundle"]["id"],"prepareBundleDigest":record["prepareBundle"]["digest"],"bindings":bindings,"posts":posts,"trust":"source_only","activePolicy":false,"createdAt":at,"review":stage});
    archive["checksum"]=json!(super::research_cache::checksum(&archive));
    if d.get("preparationResearch").is_none(){d["preparationResearch"]=json!([])}
    let archive_rows=d["preparationResearch"].as_array_mut().ok_or_else(||super::bad("Invalid preparation research archive"))?;
    if archive_rows.iter().any(|v|v["jobId"]==job){return Err(super::conflict("Research archive already exists"))}
    archive_rows.push(archive);
    super::row_mut(d,"jobs",job)?["preparationStages"]["review"]=stage;Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    const AT:&str="2026-09-22T10:00:00Z";
    fn request()->Value{json!({"purpose":"triage","account":"LikeAvto","items":[{"id":"i","postId":"p","postKey":"vk:p"}],"posts":[{"id":"p","postKey":"vk:p","title":"Video"}]})}
    fn result(outcome:&str,tags:Value)->Value{
        let proposals=if outcome=="needs_attention"{json!([])}else{json!([{"itemId":"i","kind":if outcome=="reply"{"reply_and_close"}else{"close"},"text":if outcome=="reply"{"Reply"}else{""}}])};
        json!({"text":"Explanation","sources":[],"assessments":[{"itemId":"i","outcome":outcome,"reason":"Reason","tags":tags}],"proposals":proposals})
    }
    fn research()->Value{json!({"version":1,"status":"completed","model":"m","reasoningEffort":"medium","instructionSha256":"a".repeat(64),"inputSha256":"b".repeat(64),"elapsedMs":8,"webCalls":2,"sources":[{"itemId":"i","url":"https://example.com/facts","title":"Page","claim":"Claim","secret":"remove"}],"completedAt":AT,"secret":"remove"})}
    fn reviewed()->Value{
        let mut r=result("reply",json!(["needs_fact"]));
        r["runMetadata"]=json!({"schemaVersion":1,"model":"m","reasoningEffort":"medium","promptVersion":"v","instructionSha256":"a".repeat(64),"inputSha256":"b".repeat(64),"cliSha256":"c".repeat(64),"elapsedMs":8,"completedAt":AT,"research":research(),"secret":"remove"});r
    }
    fn document()->Value{json!({"account":"LikeAvto","audit":[],"approvals":[],"knowledge_entries":[],"knowledge_versions":[],"feedback":[],"jobs":[{"id":"j","status":"running","prepareBundle":{"id":"b","digest":"d","request":request()}}],"posts":[{"id":"p","postKey":"vk:p","attachments":[{"type":"video"}]}]})}
    fn media_hold()->Value {let mut r=result("needs_attention",json!(["missing_context"]));r["decisionSource"]=json!("deterministic_media_source_gap");r}
    #[test]
    fn deterministic_media_hold_requires_selected_raw_source_gap() {
        for media in [json!({"attachments":[{"type":"unsupported"}]}),json!({"commentAttachments":[{"type":"photo"}]}),
            json!({"attachments":[],"attachmentsState":"present"}),json!({"commentAttachmentsPresent":true}),
            json!({"attachments":null,"commentAttachments":[{"type":"sticker","preview_url":"https://example.com/thumb"}]})] {
            let mut req=request();req["items"][0].as_object_mut().unwrap().extend(media.as_object().unwrap().clone());
            assert!(plan_review(&req,&media_hold()).unwrap().is_none());
        }
        for media in [json!({}),json!({"attachmentsState":"unknown"}),json!({"attachments":[],"attachmentsState":"none"}),
            json!({"attachments":[{"type":"video","url":"https://example.com/video"}]}),json!({"attachments":[{"type":"video"}]}),
            json!({"attachments":[{"type":"photo","url":"https://example.com/unavailable.jpg"}]}),
            json!({"attachments":[{"type":"unsupported","source_url":"https://example.com/media"}]}),
            json!({"attachmentStatus":"unavailable"})] {
            let mut req=request();req["items"][0].as_object_mut().unwrap().extend(media.as_object().unwrap().clone());
            req["posts"][0]["attachments"]=json!([{"type":"unsupported"}]);
            assert!(plan_review(&req,&media_hold()).is_err(),"{media}");
            assert!(plan_review(&req,&result("needs_attention",json!(["missing_context"]))).unwrap().is_some());
        }
    }
    #[test]
    fn deterministic_media_marker_cannot_bypass_review_or_forge_model_provenance() {
        let mut req=request();req["items"][0]["attachments"]=json!([{"type":"unsupported"}]);
        for change in ["metadata","reply","recipient","tag","flag","purpose","multiple"] {
            let mut request=req.clone();let mut r=media_hold();
            match change {
                "metadata"=>r["runMetadata"]=json!(null),
                "reply"=>{r=result("reply",json!(["feedback"]));r["decisionSource"]=json!("deterministic_media_source_gap");},
                "recipient"=>r["assessments"][0]["itemId"]=json!("other"),
                "tag"=>r["assessments"][0]["tags"]=json!(["feedback"]),
                "flag"=>r["decisionSource"]=json!("skip_review"),
                "purpose"=>request["purpose"]=json!("triage_review"),
                _=>request["items"].as_array_mut().unwrap().push(json!({"id":"other"})),
            }
            assert!(plan_review(&request,&r).is_err(),"{change}");
        }
    }
    #[test]
    fn deterministic_hold_is_current_immutable_evidence_and_admits_no_proposal() {
        let now=1_790_000_000;
        let mut d=crate::empty();
        d["items"]=json!([{"id":"i","itemId":"c","objectId":"o","postKey":"p","conversationKey":"thread","branchId":"b","postId":"post","revision":1,"draft":"","workflow":"attention","providerStatus":"new","createdAt":chrono::DateTime::from_timestamp(now-60,0).unwrap().to_rfc3339(),"providerObservedAt":chrono::DateTime::from_timestamp(now,0).unwrap().to_rfc3339(),"attachments":[{"type":"unsupported"}]}]);
        d["branches"]=json!([{"id":"b","postId":"post","messages":[{"id":"c","text":"Comment"}],"contextComplete":true}]);
        d["posts"]=json!([{"id":"post","text":"Post"}]);
        let (job,_)=crate::auto_prepare::claim(&mut d,now).unwrap().unwrap();
        let mut changed=d.clone();changed["items"][0]["attachments"]=json!([{"type":"photo","url":"https://example.com/current.jpg"}]);
        assert!(record_first(&mut changed,&job,&media_hold(),AT).is_err());
        assert!(record_first(&mut d,&job,&media_hold(),AT).unwrap().is_none());
        let stage=crate::row(&d,"jobs",&job).unwrap()["preparationStages"]["first"].clone();
        assert_eq!(stage["trust"],"source_evidence_only");assert_eq!(stage["reviewRequired"],false);
        assert_eq!(stage["result"]["decisionSource"],"deterministic_media_source_gap");assert!(stage["result"].get("runMetadata").is_none());
        record_first(&mut d,&job,&media_hold(),AT).unwrap();
        let outcome=crate::auto_prepare::complete(&mut d,&job,&media_hold(),now+1).unwrap();
        assert_eq!(outcome["status"],"needs_attention");assert_eq!(d["items"][0]["workflow"],"attention");
        assert!(d["proposals"].as_array().unwrap().is_empty());assert!(d["approvals"].as_array().unwrap().is_empty());
        assert!(d["operations"].as_array().unwrap().is_empty());
    }
    #[test]
    fn reviews_decisions_and_substantive_replies_not_routine_feedback(){
        for outcome in ["close","needs_attention"]{assert!(plan_review(&request(),&result(outcome,json!([]))).unwrap().is_some());}
        for tag in ["complaint","needs_fact","moderation","question","purchase"]{assert!(plan_review(&request(),&result("reply",json!([tag]))).unwrap().is_some());}
        assert!(plan_review(&request(),&result("reply",json!(["feedback"]))).unwrap().is_none());
        let plan=plan_review(&request(),&result("close",json!([]))).unwrap().unwrap();
        assert_eq!(plan["purpose"],"triage_review");assert_eq!(plan["firstPass"]["trust"],"untrusted_model_output");
    }
    #[test]
    fn price_objection_cannot_skip_review_by_being_mistagged_feedback(){
        // Sanitized regression: preserves the observed cheaper/abroad versus
        // dearer/here structure, without customer IDs or the disparaging label.
        let mut req=request();
        req["items"][0]["text"]=json!("Такой дешевле другого автомобиля у них, у нас он будет дороже 😁");
        let mut first=result("reply",json!(["feedback"]));
        first["proposals"][0]["text"]=json!("Тут честнее сравнивать обе машины по итоговой цене в России 🙂");
        let review=plan_review(&req,&first).unwrap().unwrap();
        assert_eq!(review["purpose"],"triage_review");
        assert_eq!(review["firstPass"]["assessments"][0]["tags"],json!(["feedback"]),"selection does not rewrite the model's evidence");
        assert_eq!(review["items"],req["items"]);
        let mut d=document();d["jobs"][0]["prepareBundle"]["request"]=req;
        assert!(record_first(&mut d,"j",&first,AT).unwrap().is_some());
        assert_eq!(d["jobs"][0]["preparationStages"]["first"]["reviewRequired"],true);
    }
    #[test]
    fn price_questions_and_market_comparisons_use_selected_comment_evidence(){
        for comment in [
            "В Китае 1,4 млн, а в России 5 млн", "Почему такая цена?", "За что такая наценка?",
            "Не верю этой цене", "Сколько стоит под ключ", "Почём?", "Почему 500000 рублей?",
            "У них дешевле, у нас дороже", "У\u{a0}них дешевле, У НАС дороже",
        ] {
            let mut req=request();req["items"][0]["text"]=json!(comment);
            assert!(plan_review(&req,&result("reply",json!(["feedback"]))).unwrap().is_some(),"{comment}");
        }
        let mut req=request();req["items"][0]["preview"]=json!("Почему такая цена?");
        assert!(plan_review(&req,&result("reply",json!(["feedback"]))).unwrap().is_some());
        req["items"][0]["text"]=json!("Спасибо!");
        assert!(plan_review(&req,&result("reply",json!(["feedback"]))).unwrap().is_none(),"current text overrides a stale preview");
    }
    #[test]
    fn routine_replies_and_unrelated_numeric_banter_keep_fast_path(){
        for comment in [
            "Привет!", "Спасибо, всё понятно!", "Можно написать хоть 10000 л. с. 😂",
            "Сколько будет 2 + 2? 😁", "У них 10000 лошадей, у нас 10001 😂",
            "У них дороги лучше, у нас хуже", "Слёзы тех, кто услышал цену 😅", "Ценю ваш юмор!",
        ] {
            let mut req=request();req["items"][0]["text"]=json!(comment);
            req["posts"][0]["text"]=json!("Почему в Китае цена ниже, чем в России?");
            req["branches"]=json!([{"messages":[{"text":"У них дешевле, у нас дороже"}]}]);
            assert!(plan_review(&req,&result("reply",json!(["feedback"]))).unwrap().is_none(),"{comment}");
        }
    }
    #[test]
    fn refuses_foreign_duplicate_targets_and_nonempty_close(){
        let mut r=result("close",json!([]));r["assessments"][0]["itemId"]=json!("foreign");assert!(plan_review(&request(),&r).is_err());
        let mut r=result("close",json!([]));r["proposals"][0]["text"]=json!("must not send");assert!(plan_review(&request(),&r).is_err());
        let mut r=result("close",json!([]));r["assessments"][0].as_object_mut().unwrap().remove("tags");assert!(plan_review(&request(),&r).unwrap().is_some());
    }
    #[test]
    fn research_is_allowlisted_bound_and_not_trusted_policy(){
        let allowed=ids(&request()).unwrap();let source=sanitize_research(&research(),&allowed).unwrap();
        assert_eq!(source["trust"],"source_only");assert!(source.get("secret").is_none());assert!(source["sources"][0].get("secret").is_none());
        for field in ["version","status","inputSha256","completedAt","webCalls"] {let mut r=research();r[field]=json!("bad");assert!(sanitize_research(&r,&allowed).is_err());}
        for url in ["file:///secret","https://user:pass@example.com","javascript:alert(1)","https://example.com/\nsecret"] {let mut r=research();r["sources"][0]["url"]=json!(url);assert!(sanitize_research(&r,&allowed).is_err());}
        let mut r=research();r["sources"][0]["itemId"]=json!("foreign");assert!(sanitize_research(&r,&allowed).is_err());
    }
    #[test]
    fn stages_and_archive_are_immutable_and_retry_safe(){
        let mut d=document();let first=result("close",json!([]));
        record_first(&mut d,"j",&first,AT).unwrap();record_first(&mut d,"j",&first,AT).unwrap();
        assert!(record_first(&mut d,"j",&result("needs_attention",json!([])),AT).is_err());
        let review=reviewed();record_review(&mut d,"j",Ok(&review),AT).unwrap();record_review(&mut d,"j",Ok(&review),AT).unwrap();
        assert_eq!(rows(&d,"preparationResearch").len(),1);let a=&d["preparationResearch"][0];
        assert_eq!(a["activePolicy"],false);assert_eq!(a["posts"][0]["isVideo"],true);assert_eq!(a["bindings"][0]["itemId"],"i");
        assert_eq!(a["checksum"],super::super::research_cache::checksum(a));assert!(!a.to_string().contains("remove"));
        let before=d.clone();assert!(record_review(&mut d,"j",Err("token=secret"),AT).is_err());assert_eq!(d,before);
    }
    #[test]
    fn failed_review_retains_first_pass_without_persisting_provider_secrets(){
        let mut d=document();record_first(&mut d,"j",&result("needs_attention",json!([])),AT).unwrap();
        record_review(&mut d,"j",Err("Bearer secret-token; private request"),AT).unwrap();
        assert_eq!(d["jobs"][0]["preparationStages"]["first"]["status"],"completed");
        assert_eq!(d["preparationResearch"][0]["review"]["status"],"failed");assert!(!d.to_string().contains("secret-token"));
    }
    #[test]
    fn review_failures_preserve_safe_category_and_not_secret_diagnostics(){
        for (input,code,retryable) in [
            ("Adapter failed (ADAPTER_TIMEOUT); Bearer secret-token","ADAPTER_TIMEOUT",true),
            ("Adapter process failed; private request","ADAPTER_PROCESS_FAILED",true),
            ("Adapter failed (ASSISTANT_BUSY)","ASSISTANT_BUSY",true),
            ("Adapter failed (ASSISTANT_INVALID_RESEARCH)","ASSISTANT_INVALID_RESEARCH",false),
            ("Adapter failed (ASSISTANT_INVALID_RESEARCH_UNOBSERVED_URL); Bearer secret-token","ASSISTANT_INVALID_RESEARCH_UNOBSERVED_URL",false),
            ("Adapter failed (ASSISTANT_INVALID_RESEARCH_MISSING_EVIDENCE)","ASSISTANT_INVALID_RESEARCH_MISSING_EVIDENCE",false),
            ("Adapter failed (ASSISTANT_INVALID_RESEARCH_RECIPIENT)","ASSISTANT_INVALID_RESEARCH_RECIPIENT",false),
            ("Adapter failed (ASSISTANT_INVALID_RESEARCH_FIELDS)","ASSISTANT_INVALID_RESEARCH_FIELDS",false),
            ("Adapter failed (ASSISTANT_INVALID_RESEARCH_UNATTRIBUTED_REPLY)","ASSISTANT_INVALID_RESEARCH_UNATTRIBUTED_REPLY",false),
            ("Adapter failed (ASSISTANT_INVALID_RESEARCH_ACTIVITY_ID)","ASSISTANT_INVALID_RESEARCH_ACTIVITY_ID",false),
            ("Adapter failed (ASSISTANT_INVALID_RESEARCH_SECRET_TOKEN)","ASSISTANT_INVALID_RESEARCH",false),
            ("Adapter failed (ASSISTANT_RESEARCH_LIMIT)","ASSISTANT_RESEARCH_LIMIT",false),
            ("Adapter failed (ASSISTANT_ISOLATION_FAILED)","ASSISTANT_ISOLATION_FAILED",false),
            ("private request failed","REVIEW_FAILED",false),
        ] {
            let mut d=document();record_first(&mut d,"j",&result("needs_attention",json!([])),AT).unwrap();
            record_review(&mut d,"j",Err(input),AT).unwrap();
            let review=&d["jobs"][0]["preparationStages"]["review"];
            assert_eq!(review["errorCode"],code);assert_eq!(review["retryable"],retryable);
            assert!(!d.to_string().contains("secret-token"));assert!(!d.to_string().contains("private request"));
            assert!(failure_message(input).contains(code));assert!(!failure_message(input).contains("secret-token"));
        }
    }
    #[tokio::test]
    async fn first_and_research_evidence_survive_database_reopen(){
        let dir=tempfile::tempdir().unwrap();let path=dir.path().join("review.sqlite");
        let db=super::super::Database::Sqlite(super::super::open_db(&path).await.unwrap());
        db.change(|d|{for (key,value) in document().as_object().unwrap(){d[key]=value.clone();}record_first(d,"j",&result("close",json!([])),AT)?;Ok(())}).await.unwrap();
        db.close().await;
        let db=super::super::Database::Sqlite(super::super::open_db(&path).await.unwrap());
        assert_eq!(db.read().await.unwrap()["jobs"][0]["preparationStages"]["first"]["status"],"completed");
        let mut review=reviewed();
        let repair=json!({"version":1,"attempts":1,"inputSha256":"d".repeat(64),"instructionSha256":"e".repeat(64),
            "originalInstructionSha256":"f".repeat(64),"candidateSha256":"0".repeat(64),"verifiedEvidenceIndices":[0],"webCalls":1});
        review["runMetadata"]["researchRepair"]=repair.clone();
        db.change(|d|record_review(d,"j",Ok(&review),AT)).await.unwrap();db.close().await;
        let db=super::super::Database::Sqlite(super::super::open_db(&path).await.unwrap());
        let state=db.read().await.unwrap();let archive=&state["preparationResearch"][0];
        assert_eq!(archive["review"]["research"]["sources"][0]["claim"],"Claim");
        assert_eq!(archive["review"]["result"]["runMetadata"]["researchRepair"],repair);
        assert_eq!(state["jobs"][0]["preparationStages"]["review"]["result"]["runMetadata"]["researchRepair"],repair);
        assert_eq!(archive["review"]["result"]["runMetadata"]["inputSha256"],"b".repeat(64));
        assert_eq!(archive["checksum"],super::super::research_cache::checksum(archive));db.close().await;
    }
}
