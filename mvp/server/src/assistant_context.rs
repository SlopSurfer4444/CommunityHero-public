//! Read-only workspace retrieval and bounded client screen references.
use serde_json::{Value, json};
use sha2::{Digest,Sha256};
use std::collections::{BTreeSet,HashMap};

fn rows<'a>(d:&'a Value,key:&str)->&'a [Value]{d[key].as_array().map(Vec::as_slice).unwrap_or(&[])}
fn text(v:&Value,key:&str)->String{v[key].as_str().unwrap_or("").to_string()}
fn clipped(value:&str,max:usize)->String{value.chars().take(max).collect()}
fn indexed<'a>(records:&'a [Value])->HashMap<&'a str,&'a Value>{
    let mut index=HashMap::new();
    for record in records {if let Some(id)=record["id"].as_str(){index.entry(id).or_insert(record);}}
    index
}
fn post_for<'a>(branches:&HashMap<&str,&'a Value>,posts:&HashMap<&str,&'a Value>,item:&Value)->Option<&'a Value>{
    let branch=item["branchId"].as_str().and_then(|id|branches.get(id).copied());
    let post_id=item["postId"].as_str().or_else(||branch.and_then(|b|b["postId"].as_str()));
    post_id.and_then(|id|posts.get(id).copied())
}
fn author_for(branches:&HashMap<&str,&Value>,item:&Value)->String{
    if let Some(author)=item["author"].as_str().filter(|a|!a.is_empty()){return author.to_string();}
    item["branchId"].as_str().and_then(|id|branches.get(id).copied())
        .and_then(|b|rows(b,"messages").iter().find(|m|m["id"]==item["targetId"]))
        .map(|m|text(m,"author")).unwrap_or_default()
}

/// Exact local query. Text matches every literal word; topic matches an existing tag.
/// No model-generated classification or provider completeness claim enters counts.
pub fn query(d:&Value,args:&Value,stats:bool)->Result<Value,&'static str>{
    let fields=args.as_object().ok_or("Query arguments must be an object")?;
    if fields.keys().any(|k|!["query","workflow","platform","postId","from","to","topic","limit","offset"].contains(&k.as_str())){return Err("Unsupported query filter");}
    for key in ["query","workflow","platform","postId","topic"] {
        if fields.get(key).is_some_and(|v|!v.is_string()||v.as_str().unwrap().chars().count()>300){return Err("Invalid query filter");}
    }
    if let Some(w)=args["workflow"].as_str().filter(|s|!s.is_empty()){
        if !["attention","prepared","waiting","closed","deleted"].contains(&w){return Err("Unknown workflow filter");}
    }
    let date=|key:&str|->Result<Option<chrono::DateTime<chrono::FixedOffset>>,&'static str>{
        match fields.get(key){None|Some(Value::Null)=>Ok(None),Some(Value::String(s))=>chrono::DateTime::parse_from_rfc3339(s).map(Some).map_err(|_|"Dates require RFC3339 timestamps"),_=>Err("Invalid date filter")}
    };
    let from=date("from")?;let to=date("to")?;
    if from.zip(to).is_some_and(|(a,b)|a>=b){return Err("Date range must have from before to");}
    for (key,max) in [("limit",20_u64),("offset",1_000_000_u64)]{
        if fields.get(key).is_some_and(|v|v.as_u64().is_none_or(|n|n>max||(key=="limit"&&n==0))){return Err("Invalid query pagination");}
    }
    let query=args["query"].as_str().unwrap_or("").trim();
    let words:Vec<String>=query.split_whitespace().map(str::to_lowercase).collect();
    let branches=indexed(rows(d,"branches"));let posts=indexed(rows(d,"posts"));
    let mut matches:Vec<(String,String,Value)>=Vec::new();
    let mut missing_dates=0;
    for item in rows(d,"items") {
        let post=post_for(&branches,&posts,item);
        let author=if !stats||!words.is_empty(){author_for(&branches,item)}else{String::new()};
        let body=item["text"].as_str().filter(|v|!v.is_empty()).or_else(||item["preview"].as_str()).unwrap_or("");
        let title=post.map(|p|text(p,"title")).unwrap_or_else(||text(item,"title"));
        let post_id=post.map(|p|p["id"].clone()).unwrap_or(Value::Null);
        if ["workflow","platform"].iter().any(|k|args[k].as_str().filter(|s|!s.is_empty()).is_some_and(|s|item[k]!=s)){continue;}
        if args["postId"].as_str().filter(|s|!s.is_empty()).is_some_and(|s|post_id!=s){continue;}
        if args["topic"].as_str().filter(|s|!s.is_empty()).is_some_and(|s|!rows(item,"triageTags").iter().any(|v|v==s)){continue;}
        if !words.is_empty(){
            let searchable=format!("{} {} {} {} {}",text(item,"id"),text(item,"itemId"),author,body,title).to_lowercase();
            if !words.iter().all(|w|searchable.contains(w)){continue;}
        }
        if from.is_some()||to.is_some(){
            let Some(created)=item["createdAt"].as_str().and_then(|s|chrono::DateTime::parse_from_rfc3339(s).ok())else{missing_dates+=1;continue;};
            if from.is_some_and(|f|created<f)||to.is_some_and(|t|created>=t){continue;}
        }
        matches.push((text(item,"createdAt"),text(item,"id"),json!({
            "id":item["id"],"revision":item["revision"],"author":author,"text":if stats{String::new()}else{clipped(body,800)},"title":clipped(&title,240),
            "workflow":item["workflow"],"platform":item["platform"],"createdAt":item["createdAt"],
            "postId":post_id,"triageTags":item["triageTags"]
        })));
    }
    if !stats{matches.sort_by(|a,b|b.0.cmp(&a.0).then_with(||a.1.cmp(&b.1)));}
    let total=matches.len();let limit=args["limit"].as_u64().unwrap_or(20) as usize;let offset=args["offset"].as_u64().unwrap_or(0) as usize;
    let mut result=json!({"query":query,"total":total,"scope":"local_workspace","matchMode":"literal_all_words","filters":args,"observedAt":crate::now(),"excludedMissingCreatedAt":missing_dates});
    if stats {
        let mut prepared_reply=0_usize;let mut prepared_close=0_usize;let mut open_total=0_usize;let mut prepared_other=0_usize;
        for (_,_,item) in &matches {
            if matches!(item["workflow"].as_str(),Some("attention"|"prepared"|"waiting")){open_total+=1;}
            if item["workflow"]=="prepared" {
                let proposal=rows(d,"proposals").iter().rev().find(|p|p["itemId"]==item["id"]&&matches!(p["status"].as_str(),Some("draft"|"approved"))&&crate::proposal_current(d,p).is_ok());
                match proposal.and_then(|p|p["kind"].as_str()){Some("reply_and_close")=>prepared_reply+=1,Some("close")=>prepared_close+=1,_=>prepared_other+=1}
            }
        }
        result["openTotal"]=json!(open_total);
        result["prepared_reply"]=json!(prepared_reply);result["prepared_close"]=json!(prepared_close);result["prepared_other_or_stale"]=json!(prepared_other);
        result["preparedReplyPercentOfOpen"]=if open_total>0{json!(100.0*prepared_reply as f64/open_total as f64)}else{Value::Null};
        result["percentageDenominator"]=json!("openTotal: matching attention + prepared + waiting comments");
        let synced=d["sync"]["lastSyncedAt"].as_str().and_then(|s|chrono::DateTime::parse_from_rfc3339(s).ok());
        result["syncFreshness"]=json!({"lastSyncedAt":d["sync"]["lastSyncedAt"],"status":d["sync"]["status"],
            "stale":synced.is_none_or(|t|chrono::Utc::now().signed_duration_since(t).num_minutes()>15)||d["sync"]["status"].as_str().is_some_and(|s|matches!(s,"failed"|"error"|"backoff")),
            "staleAfterMinutes":15,"providerCompleteness":"not_established","openCoverage":d["sync"]["open"]["coverage"],"closedCoverage":d["sync"]["closed"]["coverage"]});
        use std::collections::BTreeMap;
        let mut workflows:BTreeMap<String,usize>=BTreeMap::new();let mut platforms=BTreeMap::new();let mut topics=BTreeMap::new();let mut posts:BTreeMap<String,(String,usize)>=BTreeMap::new();
        for (_,_,item) in &matches {
            *workflows.entry(item["workflow"].as_str().unwrap_or("unknown").to_owned()).or_default()+=1;
            *platforms.entry(item["platform"].as_str().unwrap_or("unknown").to_owned()).or_insert(0_usize)+=1;
            let post=posts.entry(item["postId"].as_str().unwrap_or("unknown").to_owned()).or_insert((text(item,"title"),0));post.1+=1;
            let unique:BTreeSet<_>=rows(item,"triageTags").iter().filter_map(Value::as_str).collect();
            for topic in unique{*topics.entry(topic.to_owned()).or_insert(0_usize)+=1;}
        }
        result["byWorkflow"]=json!(workflows);result["byPlatform"]=json!(platforms);result["byTopic"]=json!(topics);
        result["byPost"]=json!(posts.into_iter().map(|(id,(title,count))|json!({"postId":id,"title":title,"count":count})).collect::<Vec<_>>());
        result["topicCountMeaning"]=json!("existing_tags_nonexclusive_not_semantic_classification");
    }else{
        result["items"]=json!(matches.into_iter().skip(offset).take(limit).map(|(_,_,v)|v).collect::<Vec<_>>());
        result["offset"]=json!(offset);result["limit"]=json!(limit);result["hasMore"]=json!(total>offset.saturating_add(limit));
    }
    Ok(result)
}

/// Compatibility endpoint retains the original required-query contract.
/// This validation is independent of workspace state, so callers can reject
/// invalid requests before acquiring a database reader or materializing rows.
pub(crate) fn validate_search_query(query_text:&str)->Result<&str,&'static str>{
    let q=query_text.trim();
    let characters=q.chars().take(301).count();
    if !(2..=300).contains(&characters){return Err("Search query must contain 2 to 300 characters");}
    Ok(q)
}
pub fn search(d:&Value,query_text:&str,limit:usize)->Result<Value,&'static str>{
    let q=validate_search_query(query_text)?;
    query(d,&json!({"query":q,"limit":limit.clamp(1,20)}),false)
}

/// The client describes the visible screen, but all attached references must exist.
/// Client counts/labels remain explicitly UI hints rather than verified statistics.
pub fn screen(d:&Value,input:&Value,attached:&[Value])->Result<Value,&'static str>{
    if attached.len()>20{return Err("Attach at most 20 screen comments");}
    let mut seen=BTreeSet::new();
    for id in attached{
        let id=id.as_str().filter(|s|!s.is_empty()).ok_or("Invalid screen comment ID")?;
        if !seen.insert(id)||!rows(d,"items").iter().any(|i|i["id"]==id){return Err("Screen comment missing or repeated");}
    }
    let kind=input["kind"].as_str().unwrap_or(if attached.len()==1{"comment"}else{"queue"});
    if !["comment","queue","topic","post","analytics","history","discussions"].contains(&kind){return Err("Invalid assistant screen kind");}
    let mut result=json!({"kind":kind,"itemIds":attached,"contextSource":"client_screen_refs_validated","clientHints":{}});
    for key in ["key","label","query","topicKey","order"]{
        if let Some(value)=input[key].as_str(){
            if value.chars().count()>300{return Err("Screen descriptor too long");}
            result["clientHints"][key]=json!(value);
        }
    }
    if let Some(id)=input["selectedItemId"].as_str().filter(|s|!s.is_empty()){
        if !attached.iter().any(|v|v==id){return Err("Selected comment must be attached");}
        result["selectedItemId"]=json!(id);
    }
    if let Some(id)=input["postId"].as_str().filter(|s|!s.is_empty()){
        let post=rows(d,"posts").iter().find(|p|p["id"]==id).ok_or("Screen publication missing")?;
        result["post"]=json!({"id":id,"title":post["title"],"platform":post["platform"]});
    }
    let mut filters=json!({});
    for key in ["channel","postId","period","dateField","from","to","outcome","workflow"]{
        if let Some(value)=input["filters"][key].as_str(){
            if value.chars().count()>240{return Err("Screen filter too long");}filters[key]=json!(value);
        }
    }
    result["clientHints"]["filters"]=filters;
    for key in ["totalCount","visibleItemCount"]{if let Some(value)=input[key].as_u64(){result["clientHints"][key]=json!(value.min(10_000_000));}}
    result["attachedCount"]=json!(attached.len());
    result["partial"]=json!(input["truncated"]==true||input["totalCount"].as_u64().is_some_and(|n|n>attached.len() as u64));
    Ok(result)
}

pub fn attach_screen(bundle:&mut Value,screen:Value)->Result<(),&'static str>{
    if !bundle["request"].is_object(){return Err("Preparation request missing");}
    bundle["request"]["screen"]=screen;
    let input=bundle["request"].to_string();
    if input.len()>550_000{return Err("Selected assistant evidence exceeds 550000 bytes");}
    bundle["digest"]=json!(format!("{:x}",Sha256::digest(input.as_bytes())));
    Ok(())
}

/// Bind the exact editor text that was visible at submission, not an arbitrary
/// newest proposal. Discussion-only: creating a proposal must not invalidate its
/// own automatic preparation source fingerprint.
pub fn attach_displayed_draft(d:&Value,bundle:&mut Value,selection:&Value)->Result<(),&'static str>{
    if selection.is_null(){return Ok(());}
    let item_id=selection["itemId"].as_str().ok_or("Displayed draft recipient required")?;
    let text=selection["text"].as_str().ok_or("Displayed draft text required")?;
    if text.encode_utf16().count()>24_000{return Err("Displayed draft too long");}
    let source=rows(d,"items").iter().find(|i|i["id"]==item_id).ok_or("Displayed draft recipient missing")?;
    let manual=source["draft"].as_str().unwrap_or("");
    let mut provenance=json!({"kind":"manual","requiresReview":false});
    if source["draftEdited"]==true || !manual.is_empty() {
        if text!=manual{return Err("Displayed draft changed before submission; review and send again");}
    }else if !text.is_empty(){
        let proposal_id=selection["proposalId"].as_str().ok_or("Displayed proposal identity required")?;
        let proposal=rows(d,"proposals").iter().find(|p|p["id"]==proposal_id).ok_or("Displayed proposal missing")?;
        if proposal["itemId"]!=item_id||proposal["revision"]!=selection["proposalRevision"]||proposal["text"]!=text||proposal["kind"]!="reply_and_close"
            ||!matches!(proposal["status"].as_str(),Some("draft"|"stale"|"approved"|"dispatching")){
            return Err("Displayed proposal changed; review and send again");
        }
        let current=proposal["status"]=="draft"&&proposal["itemRevision"]==source["revision"]
            &&proposal["contextEvidenceDigest"]==source["contextEvidenceDigest"]&&proposal["branchContextDigest"]==source["branchContextDigest"];
        provenance=json!({"kind":if current{"candidate"}else{"historical_candidate"},"proposalId":proposal_id,"proposalRevision":proposal["revision"],"requiresReview":!current});
    }else{provenance=json!({"kind":"empty","requiresReview":false});}
    let item=bundle["request"]["items"].as_array_mut().ok_or("Preparation items missing")?.iter_mut().find(|i|i["id"]==item_id).ok_or("Displayed draft recipient is not attached")?;
    item["draft"]=json!(text);item["draftContext"]=provenance;
    let input=bundle["request"].to_string();
    if input.len()>550_000{return Err("Selected assistant evidence exceeds 550000 bytes");}
    bundle["digest"]=json!(format!("{:x}",Sha256::digest(input.as_bytes())));
    Ok(())
}

#[cfg(test)]
mod tests{
    use super::*;
    fn fixture()->Value{json!({"items":[
        {"id":"a","branchId":"branch","targetId":"message","text":"Полный привод","createdAt":"2026-09-22T09:00:00Z","workflow":"attention"},
        {"id":"b","branchId":"branch","text":"Ответ про привод","author":"Олег","createdAt":"2026-09-21T09:00:00Z","workflow":"closed"}],
        "branches":[{"id":"branch","postId":"post","messages":[{"id":"message","author":"Олег"}]}],
        "posts":[{"id":"post","title":"Changan A06","platform":"vk"}]})}
    #[test]fn search_uses_author_text_post_and_includes_closed(){
        let d=fixture();let r=search(&d,"ОЛЕГ привод",1).unwrap();
        assert_eq!(r["items"][0]["id"],"a");assert_eq!(r["total"],2);assert_eq!(r["hasMore"],true);
        assert_eq!(search(&d,"Changan",20).unwrap()["items"].as_array().unwrap().len(),2);
        assert!(search(&d," ",20).is_err());assert_eq!(search(&d,"несуществующее",20).unwrap()["total"],0);
    }
    #[test]fn screen_rejects_unknown_or_unattached_references(){
        let d=fixture();assert!(screen(&d,&json!({}),&[json!("unknown")]).is_err());
        assert!(screen(&d,&json!({"selectedItemId":"b"}),&[json!("a")]).is_err());
        assert!(screen(&d,&json!({"postId":"unknown"}),&[]).is_err());
    }
    #[test]fn descriptor_is_bounded_and_does_not_accept_injected_evidence(){
        let r=screen(&fixture(),&json!({"kind":"queue","items":[{"id":"fake"}],"totalCount":200,"label":"List","filters":{"secret":"leak","period":"week"}}),&[json!("a")]).unwrap();
        assert_eq!(r["partial"],true);assert_eq!(r["attachedCount"],1);assert!(r.get("items").is_none());
        assert!(r["clientHints"]["filters"].get("secret").is_none());
    }
    #[test]fn screen_change_is_covered_by_bundle_integrity(){
        let mut bundle=json!({"request":{"items":[]},"dependencyDigest":"source-only"});
        attach_screen(&mut bundle,json!({"kind":"analytics"})).unwrap();let first=bundle["digest"].clone();
        attach_screen(&mut bundle,json!({"kind":"history"})).unwrap();assert_ne!(first,bundle["digest"]);
        assert_eq!(bundle["dependencyDigest"],"source-only");
    }
    #[test]fn displayed_generated_draft_reaches_discussion_without_mutating_item(){
        let d=json!({"items":[{"id":"a","revision":3,"draft":"","contextEvidenceDigest":"c","branchContextDigest":"b"}],
            "proposals":[{"id":"p","revision":1,"itemId":"a","itemRevision":3,"text":"Shown answer","status":"draft","kind":"reply_and_close","contextEvidenceDigest":"c","branchContextDigest":"b"}]});
        let before=d.clone();let mut bundle=json!({"request":{"items":[{"id":"a","draft":""}]},"dependencyDigest":"source-only"});
        attach_displayed_draft(&d,&mut bundle,&json!({"itemId":"a","text":"Shown answer","proposalId":"p","proposalRevision":1})).unwrap();
        assert_eq!(bundle["request"]["items"][0]["draft"],"Shown answer");assert_eq!(bundle["request"]["items"][0]["draftContext"]["kind"],"candidate");
        assert_eq!(d,before);assert_eq!(bundle["dependencyDigest"],"source-only");
    }
    #[test]fn wrong_displayed_text_is_rejected_and_stale_text_is_marked_historical(){
        let d=json!({"items":[{"id":"a","revision":4,"draft":""}],"proposals":[{"id":"p","revision":1,"itemId":"a","itemRevision":3,"text":"Old answer","status":"stale","kind":"reply_and_close"}]});
        let mut bundle=json!({"request":{"items":[{"id":"a","draft":""}]}});
        assert!(attach_displayed_draft(&d,&mut bundle,&json!({"itemId":"a","text":"Invented","proposalId":"p","proposalRevision":1})).is_err());
        attach_displayed_draft(&d,&mut bundle,&json!({"itemId":"a","text":"Old answer","proposalId":"p","proposalRevision":1})).unwrap();
        assert_eq!(bundle["request"]["items"][0]["draftContext"]["requiresReview"],true);
    }
}
