//! Exact, opt-in post-image selection. Source links are checked against the
//! saved request; selection is never evidence that pixels were observed.
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};

pub(crate) const CONTRACT: &str = "selected_post_images_v1";
const INVALID: &str = "Invalid exact visual selection";
fn rows<'a>(v:&'a Value,key:&str)->&'a [Value]{v[key].as_array().map(Vec::as_slice).unwrap_or(&[])}
fn exact(v:&Value,keys:&[&str])->bool{v.as_object().is_some_and(|o|o.len()==keys.len()&&keys.iter().all(|k|o.contains_key(*k)))}
fn bounded_id(v:&Value)->bool{v.as_str().is_some_and(|s|!s.is_empty()&&s.len()<=500)}
fn sha(v:&Value)->bool{v.as_str().is_some_and(|s|s.len()==64&&s.bytes().all(|b|b.is_ascii_hexdigit()))}
pub(crate) fn empty()->Value{json!({"version":1,"postImages":[]})}

pub(crate) fn sanitize(value:&Value)->Result<Value,&'static str>{
    if !exact(value,&["version","postImages"])||value["version"]!=1{return Err(INVALID);}
    let entries=value["postImages"].as_array().filter(|v|v.len()<=100).ok_or(INVALID)?;
    let mut seen=BTreeSet::new();let mut clean=Vec::new();
    for entry in entries {
        if !exact(entry,&["itemId","postId","attachmentIndices","reason"])
            ||!bounded_id(&entry["itemId"])||!bounded_id(&entry["postId"])
            ||!seen.insert(entry["itemId"].as_str().unwrap())
            ||entry["reason"].as_str().is_none_or(|s|s.trim().is_empty()||s.encode_utf16().count()>500||s.chars().any(|c|c<='\u{001f}'||c=='\u{007f}')) {
            return Err(INVALID);
        }
        let indices=entry["attachmentIndices"].as_array().filter(|v|!v.is_empty()&&v.len()<=20).ok_or(INVALID)?;
        let mut selected=BTreeSet::new();
        for index in indices {if !selected.insert(index.as_u64().filter(|n|*n<20).ok_or(INVALID)?){return Err(INVALID);}}
        let mut row=entry.clone();row["attachmentIndices"]=json!(selected);clean.push(row);
    }
    Ok(json!({"version":1,"postImages":clean}))
}

pub(crate) fn linked_post<'a>(item:&'a Value,request:&'a Value)->Result<&'a str,&'static str>{
    let branch=rows(request,"branches").iter().find(|b|b["id"]==item["branchId"]);
    let branch_post=branch.and_then(|b|b["postId"].as_str());
    let item_post=item["postId"].as_str();
    if item_post.is_some()&&branch_post.is_some()&&item_post!=branch_post{return Err(INVALID);}
    item_post.or(branch_post).filter(|s|!s.is_empty()).ok_or(INVALID)
}

pub(crate) fn validate(value:&Value,request:&Value)->Result<Value,&'static str>{
    let clean=sanitize(value)?;
    for entry in rows(&clean,"postImages") {
        let items:Vec<_>=rows(request,"items").iter().filter(|i|i["id"]==entry["itemId"]).collect();
        if items.len()!=1||linked_post(items[0],request)?!=entry["postId"].as_str().unwrap(){return Err(INVALID);}
        let posts:Vec<_>=rows(request,"posts").iter().filter(|p|p["id"]==entry["postId"]).collect();
        if posts.len()!=1{return Err(INVALID);}
        for index in rows(entry,"attachmentIndices") {
            let attachment=rows(posts[0],"attachments").get(index.as_u64().unwrap() as usize).ok_or(INVALID)?;
            if !matches!(attachment["type"].as_str(),Some("photo"|"image")){return Err(INVALID);}
        }
    }
    Ok(clean)
}

pub(crate) fn request_selection(request:&Value)->Result<Option<Value>,&'static str>{
    match (request.get("visualNeedContract"),request.get("visualSelection")) {
        (None,None)=>Ok(None),
        (Some(contract),Some(selection)) if contract==CONTRACT=>validate(selection,request).map(Some),
        _=>Err(INVALID),
    }
}

pub(crate) fn sanitize_followup(value:&Value,metadata:&Value)->Result<Value,&'static str>{
    let invalid="Invalid visual followup provenance";
    if !exact(value,&["version","status","selection","itemIds","firstPass","retry"])
        ||value["version"]!=1||!matches!(value["status"].as_str(),Some("completed"|"held")){return Err(invalid);}
    let selection=sanitize(&value["selection"])?;
    if rows(&selection,"postImages").is_empty(){return Err(invalid);}
    let ids=value["itemIds"].as_array().filter(|v|!v.is_empty()&&v.len()<=100).ok_or(invalid)?;
    let mut claimed=BTreeSet::new();
    for id in ids {if !bounded_id(id)||!claimed.insert(id.as_str().unwrap()){return Err(invalid);}}
    let selected=rows(&selection,"postImages").iter().map(|r|r["itemId"].as_str().unwrap()).collect::<BTreeSet<_>>();
    if selected!=claimed{return Err(invalid);}
    let valid_pass=|pass:&Value|exact(pass,&["inputSha256","instructionSha256","resultSha256","traceSha256"])
        &&["inputSha256","instructionSha256","resultSha256","traceSha256"].iter().all(|k|sha(&pass[k]));
    if !valid_pass(&value["firstPass"])||value["firstPass"]["inputSha256"]!=metadata["inputSha256"]
        ||value["firstPass"]["instructionSha256"]!=metadata["instructionSha256"]
        ||(value["status"]=="completed"&&!valid_pass(&value["retry"]))
        ||(value["status"]=="held"&&!value["retry"].is_null()){return Err(invalid);}
    let mut clean=value.clone();clean["selection"]=selection;Ok(clean)
}

pub(crate) fn merge(left:&Value,right:&Value)->Result<Value,&'static str>{
    let mut merged:BTreeMap<String,Value>=BTreeMap::new();
    for selection in [left,right] {for row in rows(selection,"postImages") {
        let id=row["itemId"].as_str().ok_or(INVALID)?.to_owned();
        if let Some(old)=merged.get_mut(&id) {
            if old["postId"]!=row["postId"]{return Err(INVALID);}
            let indices=rows(old,"attachmentIndices").iter().chain(rows(row,"attachmentIndices")).filter_map(Value::as_u64).collect::<BTreeSet<_>>();
            old["attachmentIndices"]=json!(indices);
        } else {merged.insert(id,row.clone());}
    }}
    Ok(json!({"version":1,"postImages":merged.into_values().collect::<Vec<_>>()}))
}

pub(crate) fn effective(metadata:&Value,request:&Value)->Result<Option<Value>,&'static str>{
    let mut selection=request_selection(request)?;
    if let Some(contract)=metadata.get("visualNeedContract") {
        if contract!=CONTRACT||request.get("visualNeedContract")!=Some(contract){return Err(INVALID);}
    }
    if let Some(declared)=metadata.get("visualSelection") {
        if selection.as_ref()!=Some(&validate(declared,request)?){return Err(INVALID);}
    }
    if let Some(followup)=metadata.get("visualFollowup") {
        if request["visualNeedContract"]!=CONTRACT||metadata["visualNeedContract"]!=CONTRACT
            ||selection.as_ref().is_none_or(|s|!rows(s,"postImages").is_empty()){return Err(INVALID);}
        let followup=sanitize_followup(followup,metadata)?;
        let selected=validate(&followup["selection"],request)?;
        if followup["status"]=="completed" {
            // Reconstruct the admitted retry image scope from the immutable
            // request, rather than accepting a metadata claim of smaller work.
            let mut retry=request.clone();retry["visualSelection"]=selected.clone();
            retry["items"]=json!(rows(request,"items").iter().filter(|i|rows(&followup,"itemIds").contains(&i["id"])).collect::<Vec<_>>());
            if crate::engine_prepare::image_count(request)>crate::engine_prepare::MAX_REQUEST_IMAGES
                ||crate::engine_prepare::image_count(&retry)>crate::engine_prepare::MAX_REQUEST_IMAGES{return Err(INVALID);}
            selection=Some(merge(selection.as_ref().ok_or(INVALID)?,&selected)?);
        }
    }
    Ok(selection)
}

pub(crate) fn recipients(selection:&Value,post_id:&str,index:usize)->BTreeSet<String>{
    rows(selection,"postImages").iter().filter(|r|r["postId"]==post_id&&rows(r,"attachmentIndices").iter().any(|i|i.as_u64()==Some(index as u64)))
        .filter_map(|r|r["itemId"].as_str().map(str::to_owned)).collect()
}

/// A new editorial request rereads selected pixels. No prior observation is
/// represented as current image evidence, and no other recipient is inherited.
pub(crate) fn editorial_selection(proposal:&Value,request:&Value)->Result<Value,&'static str>{
    let metadata=&proposal["generationMetadata"];let item=&proposal["itemId"];
    let mut selection=empty();
    for source in [metadata.get("visualSelection"),metadata.get("visualFollowup").filter(|f|f["status"]=="completed").map(|f|&f["selection"])] {
        if let Some(source)=source {
            let clean=sanitize(source)?;
            let selected=json!({"version":1,"postImages":rows(&clean,"postImages").iter().filter(|r|r["itemId"]==*item).cloned().collect::<Vec<_>>()});
            selection=merge(&selection,&validate(&selected,request)?)?;
        }
    }
    // Legacy generation staged every carousel. For a reply those pixels may
    // support factual text, so preserve their review. Text-only moderation of
    // older proposals does not request unrelated post carousels again.
    if metadata.get("visualNeedContract").is_none()&&proposal["kind"]=="reply_and_close" {
        let mut by_post:BTreeMap<String,BTreeSet<u64>>=BTreeMap::new();
        for image in rows(metadata,"imageEvidence") {
            if image["origin"]=="post_attachment"&&(image["itemId"]==*item||rows(image,"itemIds").contains(item)) {
                let post=image["postId"].as_str().ok_or(INVALID)?.to_owned();
                by_post.entry(post).or_default().insert(image["attachmentIndex"].as_u64().ok_or(INVALID)?);
            }
        }
        for (post,indices) in by_post {
            let selected=json!({"version":1,"postImages":[{"itemId":item,"postId":post,"attachmentIndices":indices,
                "reason":"Recheck post pixels observed during the original reply preparation"}]});
            selection=merge(&selection,&validate(&selected,request)?)?;
        }
    }
    // Strict publication review must actually receive every current post photo,
    // even when the text-first model judged its decision media-independent.
    // This is the existing bounded image pipeline, never video-frame scanning.
    if matches!(proposal["kind"].as_str(),Some("reply_and_close"|"close"))
        && rows(request,"posts").iter().any(|post|rows(post,"attachments").iter().any(|a|
            matches!(a["type"].as_str(),Some("photo"|"image")))) {
        let recipient=rows(request,"items").iter().find(|i|i["id"]==*item).ok_or(INVALID)?;
        let post_id=linked_post(recipient,request)?;
        let post=rows(request,"posts").iter().find(|p|p["id"]==post_id).ok_or(INVALID)?;
        let indices:Vec<_>=rows(post,"attachments").iter().enumerate()
            .filter(|(_,a)|matches!(a["type"].as_str(),Some("photo"|"image")))
            .map(|(index,_)|json!(index)).collect();
        if !indices.is_empty() {
            let required=json!({"version":1,"postImages":[{"itemId":item,"postId":post_id,
                "attachmentIndices":indices,"reason":"Observe current post photos before an exact reply or CLOSE"}]});
            selection=merge(&selection,&validate(&required,request)?)?;
        }
    }
    Ok(selection)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn request()->Value{json!({"visualNeedContract":CONTRACT,"visualSelection":empty(),"items":[
        {"id":"a","postId":"p","branchId":"b"},{"id":"b","postId":"p","branchId":"b"},
        {"id":"c","postId":"q","branchId":"q"}],"branches":[{"id":"b","postId":"p"},{"id":"q","postId":"q"}],
        "posts":[{"id":"p","attachments":[{"type":"photo","url":"https://example.com/a.jpg"},{"type":"image","url":"https://example.com/b.jpg"},{"type":"video"}]},
            {"id":"q","attachments":[{"type":"photo","url":"https://example.com/a.jpg"}]}]})}
    fn selected(item:&str,post:&str,indices:Value)->Value{json!({"version":1,"postImages":[{"itemId":item,"postId":post,
        "attachmentIndices":indices,"reason":"Read the exact displayed price"}]})}
    fn metadata(status:&str)->Value{
        let first=json!({"inputSha256":"a".repeat(64),"instructionSha256":"b".repeat(64),"resultSha256":"c".repeat(64),"traceSha256":"d".repeat(64)});
        json!({"inputSha256":first["inputSha256"],"instructionSha256":first["instructionSha256"],"visualNeedContract":CONTRACT,
            "visualSelection":empty(),"visualFollowup":{"version":1,"status":status,"itemIds":["a"],"selection":selected("a","p",json!([1])),
                "firstPass":first,"retry":if status=="completed"{first}else{Value::Null}}})
    }
    #[test]
    fn absent_requests_keep_legacy_while_empty_selector_is_explicit(){
        let mut r=request();assert_eq!(request_selection(&r).unwrap(),Some(empty()));
        r.as_object_mut().unwrap().remove("visualSelection");assert!(request_selection(&r).is_err());
        r.as_object_mut().unwrap().remove("visualNeedContract");assert_eq!(request_selection(&r).unwrap(),None);
        r["visualSelection"]=empty();assert!(request_selection(&r).is_err());
    }
    #[test]
    fn selector_binds_exact_comment_post_and_attachment_without_url_borrowing(){
        let r=request();let valid=selected("a","p",json!([1,0]));
        assert_eq!(validate(&valid,&r).unwrap()["postImages"][0]["attachmentIndices"],json!([0,1]));
        for selection in [selected("missing","p",json!([0])),selected("a","q",json!([0])),selected("a","p",json!([2])),
            selected("a","p",json!([20])),selected("a","p",json!([0,0])),selected("a","p",json!([]))] {
            assert!(validate(&selection,&r).is_err(),"{selection}");
        }
        let mut mismatch=r.clone();mismatch["items"][0]["branchId"]=json!("q");assert!(validate(&valid,&mismatch).is_err());
        let mut duplicate=valid.clone();let entry=duplicate["postImages"][0].clone();duplicate["postImages"].as_array_mut().unwrap().push(entry);
        assert!(validate(&duplicate,&r).is_err());
        let mut extra=valid.clone();extra["postImages"][0]["url"]=json!("private");assert!(validate(&extra,&r).is_err());
    }
    #[test]
    fn successful_followup_adds_only_exact_selected_sources_and_held_adds_none(){
        let r=request();let complete=metadata("completed");
        assert_eq!(effective(&complete,&r).unwrap(),Some(selected("a","p",json!([1]))));
        assert_eq!(effective(&metadata("held"),&r).unwrap(),Some(empty()));
        let mut initial=r.clone();initial["visualSelection"]=selected("b","p",json!([1]));
        let mut output=complete.clone();output["visualSelection"]=initial["visualSelection"].clone();
        assert!(effective(&output,&initial).is_err(),"explicit initial selection must not start another automatic followup");
        let merged=merge(&initial["visualSelection"],&complete["visualFollowup"]["selection"]).unwrap();
        assert_eq!(recipients(&merged,"p",1),BTreeSet::from(["a".into(),"b".into()]));
        assert!(recipients(&merged,"p",0).is_empty());
        for (pointer,value) in [("/visualFollowup/firstPass/inputSha256",json!("0".repeat(64))),
            ("/visualFollowup/firstPass/instructionSha256",json!("0".repeat(64))),
            ("/visualFollowup/retry/traceSha256",json!("not-hashed")),("/visualFollowup/itemIds",json!(["b"])),
            ("/visualFollowup/selection/postImages/0/postId",json!("q")),("/visualFollowup/retry",Value::Null)] {
            let mut bad=complete.clone();*bad.pointer_mut(pointer).unwrap()=value;assert!(effective(&bad,&r).is_err(),"{pointer}");
        }
        let mut held=metadata("held");held["visualFollowup"]["retry"]=complete["visualFollowup"]["retry"].clone();assert!(effective(&held,&r).is_err());
        let mut legacy=r.clone();legacy.as_object_mut().unwrap().remove("visualSelection");legacy.as_object_mut().unwrap().remove("visualNeedContract");
        assert!(effective(&complete,&legacy).is_err());
        let mut crowded=r.clone();crowded["items"][0]["attachments"]=json!((0..16).map(|_|json!({"type":"photo"})).collect::<Vec<_>>());
        assert!(effective(&complete,&crowded).is_err(),"retry cannot hide mandatory own-comment images to fit selected post pixels");
    }
    #[test]
    fn text_only_final_review_needs_no_invented_post_binding(){
        let request=json!({"items":[{"id":"a"}],"posts":[],"branches":[]});
        for kind in ["reply_and_close","close"] {
            assert_eq!(editorial_selection(&json!({"itemId":"a","kind":kind}),&request).unwrap(),empty());
        }
    }
    #[test]
    fn strict_final_review_selects_current_photos_for_reply_and_close_only(){
        let r=request();let mut p=json!({"itemId":"a","kind":"reply_and_close","generationMetadata":metadata("completed")});
        assert_eq!(editorial_selection(&p,&r).unwrap()["postImages"][0]["attachmentIndices"],json!([0,1]));
        p["itemId"]=json!("b");
        let observed=editorial_selection(&p,&r).unwrap();assert_eq!(observed["postImages"][0]["itemId"],"b");
        assert_eq!(observed["postImages"][0]["attachmentIndices"],json!([0,1]));
        p["generationMetadata"]=json!({"imageEvidence":[{"origin":"post_attachment","postId":"p","itemId":"a","itemIds":["a","b"],"attachmentIndex":0}]});
        assert_eq!(editorial_selection(&p,&r).unwrap()["postImages"][0]["attachmentIndices"],json!([0,1]));
        p["kind"]=json!("close");assert_eq!(editorial_selection(&p,&r).unwrap()["postImages"][0]["attachmentIndices"],json!([0,1]));
        p["kind"]=json!("hide");assert_eq!(editorial_selection(&p,&r).unwrap(),empty());
        let mut large=r;large["posts"][0]["attachments"]=json!((0..21).map(|_|json!({"type":"photo"})).collect::<Vec<_>>());
        p["kind"]=json!("close");assert!(editorial_selection(&p,&large).is_err(),"Never silently truncate a carousel");
    }
}
