//! Decision evidence requirements, not a claim that an incomplete video is ready.
//! A semantic judgment belongs to the exact existing editorial receipt. This
//! module neither classifies comment text nor authorizes a provider operation.
use crate::prepare_bundle::EvidenceContext;
use serde_json::{json,Value};

pub(crate) const CONTRACT:&str="communityhero-decision-media-v1";
fn rows<'a>(v:&'a Value,k:&str)->&'a [Value]{v[k].as_array().map(Vec::as_slice).unwrap_or(&[])}
pub(crate) fn enabled(value:&Value)->bool{value["decisionMediaContract"]==CONTRACT}
/// Missing default media can be assessed, but an explicit stricter owner
/// prerequisite is not silently weakened and need not consume a model attempt.
pub(crate) fn may_assess(d:&Value,item:&Value)->Result<bool,&'static str>{
    let context=EvidenceContext::new(d);
    for post in rows(d,"posts").iter().filter(|p|p["id"]==item["postId"]
        ||item["postKey"].is_string()&&p["postKey"]==item["postKey"]){
        if !crate::knowledge::is_video_post(post){continue;}
        // The common default needs semantic assessment, not a catalog scan per
        // queued comment. Only explicit owner floors need readiness here.
        let policy=crate::post_media_policy::effective_for_preparation(d,post)
            .map_err(|_|"Post preparation media policy unavailable")?;
        if policy["decisionBasis"]["kind"]!="exact_owner_override"{continue;}
        let state=context.decision_video_evidence(post)?;
        if state["ownerAudioRequired"]==true&&state["audioReady"]!=true
            ||state["ownerVisualRequired"]==true&&state["visualReady"]!=true{return Ok(false);}
    }Ok(true)
}
pub(crate) fn validate_dependency(value:&Value)->Result<(),&'static str>{
    if value.as_object().is_none_or(|o|o.len()!=2)
        ||["audio","visual"].iter().any(|k|!matches!(value[*k].as_str(),Some("independent"|"required"|"unknown"))){
        return Err("Invalid exact decision media dependency");
    }Ok(())
}
fn provided_audio(evidence:&Value,post:&Value)->bool{
    rows(evidence,"materials").iter().any(|m|m["kind"]=="transcript"&&m["text"].as_str().is_some_and(|s|!s.trim().is_empty())&&(
        m["postKey"].is_string()&&m["postKey"]==post["postKey"]
        ||rows(m,"audioEquivalence").iter().any(|e|e["targetPostId"]==post["id"])
        ||rows(evidence,"knowledgeManifest").iter().any(|pin|m["knowledgeEntryId"].is_string()&&m["knowledgeVersionId"].is_string()
            &&pin["entryId"]==m["knowledgeEntryId"]&&pin["versionId"]==m["knowledgeVersionId"]
            &&rows(pin,"mediaBinding").iter().any(|e|e["targetPostId"]==post["id"]||e["postKey"].is_string()&&e["postKey"]==post["postKey"]))))
}
fn context_available(evidence:&Value,item:&Value)->bool{
    let Some(branch)=rows(evidence,"branches").iter().find(|b|b["id"]==item["branchId"]) else{return false};
    // Providers may not certify the entire discussion. Require the addressed
    // chain below, not unrelated siblings or a full-thread coverage flag.
    let messages=rows(branch,"messages");
    // Canonical item and message IDs are different namespaces. An explicit
    // targetId is authoritative; only old items without it use legacy aliases.
    let target=item["targetId"].as_str().filter(|s|!s.is_empty());
    let mut cursor=messages.iter().find(|m|target.map_or_else(
        ||["id","itemId"].iter().any(|k|item[*k].is_string()&&m["id"]==item[*k]),
        |id|m["id"]==id));
    if cursor.is_none(){return false;}
    let mut seen=std::collections::BTreeSet::new();
    while let Some(message)=cursor{
        // A blank/tombstoned necessary message is not readable text, even when
        // the provider reports attachments but cannot return their metadata.
        if message["unavailable"]==true||message["textUnavailable"]==true||message["deleted"]==true
            ||message["text"].as_str().unwrap_or("").trim().is_empty(){return false;}
        let Some(parent)=message["parentId"].as_str().filter(|s|!s.is_empty()) else{break};
        if !seen.insert(parent){return false;}
        let Some(found)=messages.iter().find(|m|m["id"]==parent) else{return false};
        cursor=Some(found);
    }
    true
}
fn post_evidence(context:&EvidenceContext<'_>,post:&Value,evidence:&Value)->Result<Value,&'static str>{
    let mut state=context.decision_video_evidence(post)?;
    state["audioProvided"]=json!(state["audioReady"]==true&&provided_audio(evidence,post));
    // Ordinary model context intentionally omits video-frame scene summaries.
    // Actually extracted OCR remains supplied text, never proof of seen pixels.
    state["visualProvided"]=json!(false);
    let frames=rows(evidence,"optionalFrameRefs").iter().filter(|reference|reference["postId"]==post["id"]).collect::<Vec<_>>();
    if !frames.is_empty(){
        for reference in &frames{
            let job=crate::row(context.workspace(),"jobs",reference["frameJobId"].as_str().ok_or("Targeted frame job missing")?).map_err(|_|"Targeted frame job missing")?;
            crate::video_frame_work::validate_result(context.workspace(),&job["frameNeed"],&job["frameResult"])?;
            if !crate::video_frame_work::frame_refs(&job["frameResult"],&job["frameNeed"]).contains(reference){return Err("Targeted frame source/ref changed");}
        }
        state["visualProvided"]=json!(true);state["targetedFramesProvided"]=json!(true);state["targetedFrameRefsSha256"]=json!(crate::preparation_materials::hash(&json!(frames)));
    }
    Ok(state)
}
pub(crate) fn capture(context:&EvidenceContext<'_>,item:&Value,evidence:&Value)->Result<Value,&'static str>{
    if !context_available(evidence,item){return Err("Decision requires unavailable parent or branch context");}
    let mut states=Vec::new();
    for post in rows(context.workspace(),"posts").iter().filter(|p|p["id"]==item["postId"]
        ||item["postKey"].is_string()&&p["postKey"]==item["postKey"]){
        if crate::knowledge::is_video_post(post){
            let mut state=post_evidence(context,post,evidence)?;state["postId"]=post["id"].clone();states.push(state);
        }
    }
    states.sort_by_key(|s|s["postId"].as_str().unwrap_or("").to_owned());Ok(json!(states))
}
pub(crate) fn attach_request(d:&Value,request:&mut Value)->Result<(),&'static str>{
    attach_request_with_context(&EvidenceContext::new(d),request)
}
pub(crate) fn attach_request_with_context(context:&EvidenceContext<'_>,request:&mut Value)->Result<(),&'static str>{
    let d=context.workspace();let mut writes=Vec::new();
    for post in rows(request,"posts"){
        let source=rows(d,"posts").iter().find(|p|p["id"]==post["id"]).ok_or("Decision media post missing")?;
        if crate::knowledge::is_video_post(source){writes.push((post["id"].clone(),post_evidence(context,source,request)?));}
    }
    if writes.is_empty(){return Ok(());}
    request["decisionMediaContract"]=json!(CONTRACT);
    for (id,state) in writes{if let Some(post)=request["posts"].as_array_mut().unwrap().iter_mut().find(|p|p["id"]==id){post["decisionMediaEvidence"]=state;}}
    Ok(())
}
pub(crate) fn validate_judgment(candidate:&Value,judgment:&Value)->Result<(),&'static str>{
    if !enabled(candidate){
        if let Some(needs)=judgment.get("mediaDependency"){validate_dependency(needs)?;}return Ok(());
    }
    let needs=&judgment["mediaDependency"];validate_dependency(needs)?;
    if judgment["decision"]!="accept"{return Ok(());}
    if ["audio","visual"].iter().any(|k|needs[*k]=="unknown"){return Err("Decision media dependency is unresolved");}
    let states=candidate["decisionMediaEvidence"].as_array().ok_or("Decision media capture missing")?;
    if states.is_empty()&&["audio","visual"].iter().any(|k|needs[*k]=="required"){
        return Err("Exact decision requires unavailable media evidence");
    }
    for state in states{
        if state["ownerAudioRequired"]==true&&state["audioReady"]!=true
            ||state["ownerVisualRequired"]==true&&state["visualReady"]!=true
            ||needs["audio"]=="required"&&(state["audioReady"]!=true||state["audioProvided"]!=true)
            ||needs["visual"]=="required"&&state["targetedFramesProvided"]!=true&&(state["visualReady"]!=true||state["visualProvided"]!=true){
            return Err("Exact decision requires unavailable media evidence");
        }
    }Ok(())
}
/// Validate a saved generation judgment before creating its proposal. No
/// untrusted caller can turn a missing judgment into a preparation exemption.
pub(crate) fn generation_ready(d:&Value,bundle:&Value,result:&Value,proposal:&Value)->Result<(),&'static str>{
    generation_ready_with_context(&EvidenceContext::new(d),bundle,result,proposal)
}
pub(crate) fn generation_ready_with_context(context:&EvidenceContext<'_>,bundle:&Value,result:&Value,proposal:&Value)->Result<(),&'static str>{
    if !enabled(&bundle["request"]){return Ok(());}
    if !enabled(&result["runMetadata"]){return Err("Decision media generation provenance missing");}
    let d=context.workspace();
    let item=crate::row(d,"items",proposal["itemId"].as_str().ok_or("Decision recipient missing")?).map_err(|_|"Decision recipient missing")?;
    let proof=crate::editorial_review::generation_evidence(result)?.ok_or("Decision media requires exact generation review")?;
    let entry=rows(&proof,"entries").iter().find(|e|e["itemId"]==proposal["itemId"]&&e["kind"]==proposal["kind"]
        &&e["textSha256"]==crate::editorial_review::hash_text(proposal["text"].as_str().unwrap_or(""))).ok_or("Decision media review missing")?;
    if entry["decision"]!="accept"{return Err("Decision media judgment requires review");}
    let current=capture(context,item,&bundle["request"])?;
    for state in current.as_array().ok_or("Decision media capture missing")?{
        let mut captured=rows(&bundle["request"],"posts").iter().find(|p|p["id"]==state["postId"])
            .ok_or("Decision media captured post missing")?["decisionMediaEvidence"].clone();
        captured["postId"]=state["postId"].clone();
        if captured!=*state{return Err("Decision media source changed during preparation");}
    }
    validate_judgment(&json!({"decisionMediaContract":CONTRACT,"decisionMediaEvidence":current}),entry)
}

#[cfg(test)]
#[path="decision_media_tests.rs"]
mod tests;
