//! Exact post-attachment speech identity. One immutable ASR ledger owns a file;
//! each video has its own current applicability. No IO or paid work here.
use serde_json::{json,Value};
pub(crate) const CONTRACT:&str="VideoSpeechAssetPin.v1";
fn rows<'a>(v:&'a Value,k:&str)->&'a [Value]{v[k].as_array().map(Vec::as_slice).unwrap_or(&[])}
fn video(a:&Value)->bool{matches!(a["type"].as_str(),Some("video"|"clip"|"reel"))}
fn sha(v:&Value)->bool{v.as_str().is_some_and(|s|s.len()==64&&s.bytes().all(|b|b.is_ascii_digit()||(b'a'..=b'f').contains(&b)))}
pub(crate) fn validate_shape(pin:&Value)->Result<(),String>{
    let fields=["schemaVersion","contract","companyId","connectorBinding","postId","postKey","sourceVersion","attachmentIndex","attachmentIdentity"];
    if pin.as_object().is_none_or(|o|o.len()!=fields.len()||fields.iter().any(|k|!o.contains_key(*k)))
        ||pin["schemaVersion"]!=1||pin["contract"]!=CONTRACT
        ||!matches!(pin["companyId"].as_str(),Some("BAW Russia"|"LikeAvto"))
        ||!pin["connectorBinding"].is_object()||!sha(&pin["sourceVersion"])||!sha(&pin["attachmentIdentity"])
        ||["postId","postKey"].iter().any(|k|pin[*k].as_str().is_none_or(|s|s.is_empty()||s.len()>256))
        ||pin["attachmentIndex"].as_u64().is_none_or(|n|n>10000){return Err("video_speech_asset_pin_invalid".into());}
    Ok(())
}
pub(crate) fn capture(d:&Value,post:&Value,index:usize)->Result<Value,String>{
    let company=crate::accounts::Profile::from_workspace(d).map_err(|_|"video_speech_company_invalid")?;
    let active=crate::active_binding(d).map_err(|_|"video_speech_binding_invalid")?;
    let binding=if post["connectorBinding"].is_null(){active.clone()}else{crate::ConnectorBinding::from_json(&post["connectorBinding"]).map_err(|_|"video_speech_binding_invalid")?};
    binding.validate_scope(&active.workspace_id,&active.account_id).map_err(|_|"video_speech_foreign_company")?;
    if !crate::knowledge::in_account(post,company.display())||post["attachmentsState"]=="unknown"{return Err("video_speech_attachment_metadata_unknown".into());}
    let attachment=post["attachments"].as_array().and_then(|a|a.get(index)).ok_or("video_speech_attachment_missing")?;
    if !video(attachment)||!crate::knowledge::in_account(attachment,company.display()){return Err("video_speech_attachment_invalid".into());}
    let pin=json!({"schemaVersion":1,"contract":CONTRACT,"companyId":company.display(),"connectorBinding":binding.to_json(),
        "postId":post["id"],"postKey":post["postKey"],"sourceVersion":crate::media_fullframes::source_version(post,company.display()),
        "attachmentIndex":index,"attachmentIdentity":crate::media_analysis_reuse::attachment_identity(attachment)});
    validate_shape(&pin)?;Ok(pin)
}
pub(crate) fn current(d:&Value,pin:&Value)->Result<(),String>{
    validate_shape(pin)?;
    let matches=rows(d,"posts").iter().filter(|p|p["id"]==pin["postId"]).collect::<Vec<_>>();
    if matches.len()!=1{return Err("video_speech_post_missing_or_ambiguous".into());}
    let index=usize::try_from(pin["attachmentIndex"].as_u64().ok_or("video_speech_asset_pin_invalid")?).map_err(|_|"video_speech_asset_pin_invalid")?;
    if capture(d,matches[0],index)?!=*pin{return Err("video_speech_asset_changed".into());}Ok(())
}
pub(crate) fn require_progress(d:&Value,progress:&Value)->Result<(),String>{
    let Some(pin)=progress.get("assetPin") else{return Ok(())};
    current(d,pin)?;
    if progress["account"]!=pin["companyId"]||progress["connectorBinding"]!=pin["connectorBinding"]
        ||progress["sourcePostId"]!=pin["postId"]||progress["sourcePostKey"]!=pin["postKey"]||progress["sourceVersion"]!=pin["sourceVersion"]
        ||progress["sourceProjection"].is_object()&&progress["sourceProjection"]["assetPin"]!=*pin{return Err("video_speech_progress_changed".into());}Ok(())
}
/// Adapter echo is exact, including binding/index; URL equality is insufficient.
pub(crate) fn require_projection(pin:&Value,projection:&Value)->Result<(),String>{
    validate_shape(pin)?;
    if projection["assetPin"]!=*pin||projection["account"]!=pin["companyId"]||projection["postKey"]!=pin["postKey"]{
        return Err("video_speech_selected_source_changed".into());}Ok(())
}
/// All material selection uses the ordinary catalog and warmed file aliases.
/// A failure of one video does not prevent admitting another video's proof.
pub(crate) fn outcomes(d:&Value,post:&Value,at:&str)->Result<Vec<Value>,String>{
    let selected=crate::knowledge::select(d,&[],std::slice::from_ref(post),at).map_err(str::to_owned)?;
    let ledger=if d.get("jobs").is_some(){crate::media_analysis::ledger_from_workspace(d)?}else{Value::Null};
    let context=crate::prepare_bundle::EvidenceContext::new(d);
    let count=rows(post,"attachments").iter().filter(|a|video(a)).count();
    let mut result=Vec::new();
    for (index,attachment) in rows(post,"attachments").iter().enumerate().filter(|(_,a)|video(a)){
        let pin=capture(d,post,index)?;
        let speech=crate::preparation_materials::speech(post,index,pin["sourceVersion"].as_str().unwrap(),&selected,&context,count);
        let jobs=rows(d,"jobs").iter().filter(|j|j["videoSpeechAssetPin"]==pin||j["result"]["visualProgress"]["assetPin"]==pin||j["audioPin"]["progress"]["assetPin"]==pin).collect::<Vec<_>>();
        let paid_unknown=rows(&ledger,"analyses").iter().any(|analysis|{
            let attempt=rows(analysis,"attempts").last();
            attempt.is_some_and(|attempt|attempt["status"]=="unknown"&&(attempt["originalRequest"]["originalAlias"]["assetPin"]==pin
                ||jobs.iter().any(|j|j["result"]["visualProgress"]["source"]["sha256"]==analysis["key"]["sha256"]||j["audioPin"]["progress"]["source"]["sha256"]==analysis["key"]["sha256"])))
        });
        let unknown=paid_unknown||jobs.iter().any(|j|matches!(j["status"].as_str(),Some("unknown"|"dispatching"))
            ||matches!(j["error"].as_str(),Some("source_download_failed_process_unknown"|"source_download_failed_wait_failed")));
        let active=jobs.iter().any(|j|matches!(j["status"].as_str(),Some("running"|"queued")));
        let failed=!active&&jobs.iter().any(|j|matches!(j["status"].as_str(),Some("failed"|"interrupted")));
        let held=!active&&jobs.iter().any(|j|j["status"]=="paused"&&j["error"].is_string());
        let status=if speech.is_some(){"ready"}else if unknown{"unknown"}else if failed{"failed"}else if held{"held"}else{"pending"};
        result.push(json!({"assetPin":pin,"attachmentIndex":index,"attachmentIdentity":crate::media_analysis_reuse::attachment_identity(attachment),
            "status":status,"reasonCode":match status{"ready"=>"full_video_speech_outcome","unknown"=>"video_speech_work_reconciliation_required","failed"=>"video_speech_work_failed","held"=>"video_speech_work_held",_=>"video_speech_unproven"},"speech":speech}));
    }Ok(result)
}
pub(crate) fn all_ready(d:&Value,post:&Value,at:&str)->Result<bool,String>{
    if !post["attachments"].is_array()||post["attachmentsState"]=="unknown"{return Ok(false);}
    let assets=outcomes(d,post,at)?;
    if assets.is_empty()&&crate::knowledge::is_video_post(post){return Ok(false);}
    Ok(assets.iter().all(|a|a["status"]=="ready"))
}
pub(crate) fn ready(d:&Value,pin:&Value,at:&str)->Result<bool,String>{
    current(d,pin)?;let post=rows(d,"posts").iter().find(|p|p["id"]==pin["postId"]).ok_or("video_speech_post_missing")?;
    Ok(outcomes(d,post,at)?.iter().any(|a|a["assetPin"]==*pin&&a["status"]=="ready"))
}
/// Native queue can create an exact asset job only once. Failed/UNKNOWN work
/// remains actionable; a new URL/version cannot release a same-file paid fence.
pub(crate) fn next_unattempted(d:&Value,post:&Value,at:&str)->Result<Option<Value>,String>{
    for asset in outcomes(d,post,at)?.into_iter().filter(|a|a["status"]!="ready"){
        let pin=&asset["assetPin"];
        if !rows(d,"jobs").iter().any(|j|j["videoSpeechAssetPin"]==*pin||j["result"]["visualProgress"]["assetPin"]==*pin||j["audioPin"]["progress"]["assetPin"]==*pin){return Ok(Some(pin.clone()));}
    }Ok(None)
}
#[cfg(test)]
#[path="media_speech_assets_tests.rs"]
mod tests;
