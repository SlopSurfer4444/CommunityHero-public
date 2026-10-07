//! Durable second-pass evidence. Research is source material, never active policy.
use serde_json::{json, Value};
use std::collections::BTreeSet;

#[path = "preparation_review_chunks.rs"]
pub(crate) mod chunks;

/// New-run evidence, committed with the job claim. Legacy jobs without this
/// receipt cannot start a first paid pass after upgrade or restart.
pub(super) fn record_initial_admission(d:&mut Value,token:&crate::runtime_lifecycle::OwnerToken,
    run:&str,at:&str)->super::ApiResult<()> {
    crate::runtime_lifecycle::require_admission(d,token,crate::runtime_lifecycle::AdmissionClass::Preparation)?;
    let job=super::row(d,"jobs",run)?;
    if job["kind"]!="assistant" || !matches!(job["purpose"].as_str(),Some("engine_prepare"|"auto_prepare"|"auto_revalidate"))
        ||job["status"]!="running"||!job["preparationStages"].is_object()
        ||!job["preparationStages"]["first"].is_null()
        ||job["preparationStages"].get("initialAdmission").is_some()
        ||job["preparationStages"].get("firstAdmission").is_some() {
        return Err(super::conflict("Initial preparation admission is not a new run"));
    }
    let request=&job["prepareBundle"]["request"];
    let request_hash=if request.is_null() {Value::Null} else {
        validate_first_request(d,job,request)?;json!(first_request_hash(request))
    };
    super::row_mut(d,"jobs",run)?["preparationStages"]["initialAdmission"]=json!({
        "version":1,"status":"scheduled","requestSha256":request_hash,
        "owner":first_owner(token),"admittedAt":at});
    Ok(())
}

fn first_owner(token:&crate::runtime_lifecycle::OwnerToken)->Value {
    json!({"account":token.account,"runtimeId":token.runtime_id,
        "releaseSha256":token.release_sha256,"epoch":token.epoch})
}
fn first_request_hash(request:&Value)->String {
    use sha2::{Digest,Sha256};format!("{:x}",Sha256::digest(request.to_string().as_bytes()))
}
fn validate_first_request(d:&Value,job:&Value,request:&Value)->super::ApiResult<()> {
    let binding=super::active_binding(d)?;
    super::bridge_account(&binding)?;
    if !request.is_object()||job["prepareBundle"]["version"]!=1
        ||job["prepareBundle"]["request"]!=*request
        ||job["prepareBundle"]["digest"]!=first_request_hash(request)
        ||request["account"]!=d["account"]||request["connectorBinding"]!=binding.to_json() {
        return Err(super::conflict("First-pass admission request ownership changed"));
    }
    Ok(())
}

/// Single-use paid admission. A failed or interrupted reservation is never
/// silently consumed again; completed output is settled separately during drain.
pub(super) fn reserve_first_admitted(d:&mut Value,token:&crate::runtime_lifecycle::OwnerToken,
    run:&str,request:&Value,at:&str)->super::ApiResult<()> {
    crate::runtime_lifecycle::require_admission(d,token,crate::runtime_lifecycle::AdmissionClass::Preparation)?;
    let job=super::row(d,"jobs",run)?;
    let initial=&job["preparationStages"]["initialAdmission"];
    if job["kind"]!="assistant"||!matches!(job["purpose"].as_str(),Some("engine_prepare"|"auto_prepare"|"auto_revalidate"))
        ||job["status"]!="running"||!job["preparationStages"]["first"].is_null()
        ||job["preparationStages"].get("firstAdmission").is_some()
        ||initial.as_object().is_none_or(|v|v.len()!=5||!["version","status","requestSha256","owner","admittedAt"].iter().all(|key|v.contains_key(*key)))
        ||initial["version"]!=1||initial["status"]!="scheduled"||initial["owner"]!=first_owner(token)
        ||initial["requestSha256"]!=first_request_hash(request)||!initial["admittedAt"].is_string() {
        return Err(super::conflict("First-pass admission absent, stale or already reserved; inspect durable run"));
    }
    validate_first_request(d,job,request)?;
    crate::manual_frame_request::require_no_pending(d,job).map_err(super::conflict)?;
    crate::preparation_unit::current_request(d,request,at).map_err(super::conflict)?;
    crate::preparation_materials::require_request(d,request).map_err(super::conflict)?;
    super::row_mut(d,"jobs",run)?["preparationStages"]["firstAdmission"]=json!({
        "version":1,"status":"reserved","requestSha256":first_request_hash(request),
        "owner":first_owner(token),"reservedAt":at});
    Ok(())
}
/// Only an unspent capture may gain newly acquired source pixels. Native task
/// owner/admission remain identical; no paid evidence or reservation is edited.
pub(crate) fn refresh_initial_capture(d:&mut Value,run:&str,old_request:&Value,new_request:&Value)->super::ApiResult<()> {
    let job=super::row(d,"jobs",run)?;
    let initial=&job["preparationStages"]["initialAdmission"];
    if job["status"]!="running"||!job["preparationStages"]["first"].is_null()
        ||job["preparationStages"].get("firstAdmission").is_some()
        ||initial["version"]!=1||initial["status"]!="scheduled"||!initial["owner"].is_object()
        ||initial["requestSha256"]!=first_request_hash(old_request){return Err(super::conflict("Paid or changed preparation capture cannot be refreshed"));}
    validate_first_request(d,job,new_request)?;
    crate::preparation_unit::current_request(d,new_request,&crate::now()).map_err(super::conflict)?;
    crate::preparation_materials::require_request(d,new_request).map_err(super::conflict)?;
    super::row_mut(d,"jobs",run)?["preparationStages"]["initialAdmission"]["requestSha256"]=json!(first_request_hash(new_request));Ok(())
}
/// Before attempting a first dispatch after restart, recover a uniquely bound
/// ORIGINAL paid response when its receipt/ACK was lost. Reads immutable CAS
/// only; returning None never clears an existing firstAdmission reservation.
pub(crate) async fn recover_first_if_retained(app:&crate::App,run:&str,request:&Value)->super::ApiResult<Option<Value>>{
    let data=app.db.read_preparation_context(run).await?;
    let job=super::row(&data,"jobs",run)?;
    if !job["preparationStages"]["first"].is_null(){return Ok(None);}
    if job["preparationStages"]["firstAdmission"].is_null(){return Ok(None);}
    let references=rows(job,"retainedEvidence").iter().filter(|r|r["binding"]["operation"]=="assistant"&&r["binding"]["nativeJobId"]==run).cloned().collect::<Vec<_>>();
    let mut matched=None;
    for paid in references{
        let owner=&job["preparationStages"]["firstAdmission"]["owner"];
        if ["account","runtimeId","releaseSha256"].iter().any(|key|paid["runtimeOwner"][*key]!=owner[*key]){continue;}
        let capture=crate::runtime_paid_result::resolve(app,run,"assistant",&paid).await?;
        if !first_capture_matches(app.account,request,&capture)?{continue;}
        if matched.is_some(){return Err(super::conflict("Multiple original paid first captures require exact reconciliation"));}
        matched=Some((paid,capture));
    }
    let Some((paid,capture))=matched else{return Ok(None)};
    let mut result=capture["response"].clone();
    if let Some(pointer)=crate::model_material_receipt::retain(app,&capture["request"],&result,&paid).await?{
        app.change_job(run,|d|crate::model_material_receipt::attach(d,run,&pointer)).await?;
        result["modelMaterialReceipt"]=pointer;
    }Ok(Some(result))
}
pub(crate) fn first_capture_matches(profile:crate::accounts::Profile,request:&Value,capture:&Value)->super::ApiResult<bool>{
    if capture["binding"]["operation"]!="assistant"||capture["company"]!=profile.key()||capture["account"]!=profile.display(){return Ok(false);}
    let mut direct=request.clone();profile.bind_request(&mut direct)?;direct["operation"]=json!("assistant");
    let wire=&capture["request"];
    if wire==&direct{return Ok(true);}
    // The explicitly supported wrapper shape binds transport only outside;
    // its inner original captured application request is never retargeted.
    Ok(wire.as_object().is_some_and(|o|o.len()==3&&["account","operation","request"].iter().all(|k|o.contains_key(*k)))
        &&wire["account"]==profile.key()&&wire["operation"]=="assistant"&&wire["request"]==*request)
}

/// Carry native admission across the durable reservation and dispatch gap.
/// Writer rejection drops an unstarted ticket; dispatch never renews admission.
pub(super) async fn dispatch_first_admitted(app:&crate::App,run:&str,request:Value,
    token:&crate::runtime_lifecycle::OwnerToken,
    preflight:impl FnOnce(&Value,&str)->super::ApiResult<()> + Send)->super::ApiResult<Value> {
    let native=app.lifecycle_work.begin(crate::runtime_owned_work::Kind::Preparation)?;
    app.change_preparation_first(run,|d|{
        preflight(d,run)?;
        reserve_first_admitted(d,token,run,&request,&super::now())
    }).await?;
    app.bridge_admitted("assistant",request,native).await
}

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
pub(super) fn sanitize_decision_dependencies(value:&Value,allowed:&BTreeSet<String>)->Result<Value,&'static str>{
    let invalid="Invalid single-pass decision dependencies";
    let object=value.as_object().ok_or(invalid)?;
    if object.len()!=2||!object.contains_key("version")||!object.contains_key("entries")||value["version"]!=1 {
        return Err(invalid);
    }
    let entries=value["entries"].as_array().filter(|v|v.len()==allowed.len()&&v.len()<=100).ok_or(invalid)?;
    let mut seen=BTreeSet::new();let mut clean=Vec::new();
    for entry in entries {
        let object=entry.as_object().ok_or(invalid)?;
        if object.len()!=2||!object.contains_key("itemId")||!object.contains_key("dependsOnItemIds") {return Err(invalid)}
        let id=entry["itemId"].as_str().filter(|id|!id.is_empty()&&id.len()<=256).ok_or(invalid)?;
        if !allowed.contains(id)||!seen.insert(id.to_owned()){return Err(invalid)}
        let dependencies=entry["dependsOnItemIds"].as_array().filter(|v|v.len()<=100).ok_or(invalid)?;
        let mut dependency_seen=BTreeSet::new();
        for dependency in dependencies {
            let dependency=dependency.as_str().filter(|v|!v.is_empty()&&v.len()<=256).ok_or(invalid)?;
            if dependency==id||!allowed.contains(dependency)||!dependency_seen.insert(dependency){return Err(invalid)}
        }
        clean.push(json!({"itemId":id,"dependsOnItemIds":dependencies}));
    }
    Ok(json!({"version":1,"entries":clean}))
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
pub(super) fn clean_result(result:&Value,allowed:&BTreeSet<String>)->Result<Value,&'static str>{
    clean_result_mode(result,allowed,false)
}
fn clean_result_mode(result:&Value,allowed:&BTreeSet<String>,single_pass:bool)->Result<Value,&'static str>{
    if result["sources"].as_array().is_none_or(|v|!v.is_empty())||result["text"].as_str().is_none_or(|s|s.trim().is_empty()){return Err("Invalid preparation result")}
    let assessments=result["assessments"].as_array().filter(|a|a.len()==allowed.len()).ok_or("Review assessment coverage mismatch")?;
    let proposals=result["proposals"].as_array().filter(|a|a.len()<=allowed.len()).ok_or("Invalid review proposals")?;
    let mut seen=BTreeSet::new();let mut cleaned=Vec::new();let mut clean_proposals=Vec::new();
    for a in assessments {
        let item=text(a,"itemId",256)?;
        if !allowed.contains(&item)||!seen.insert(item.clone()){return Err("Foreign or duplicate review recipient")}
        let outcome=text(a,"outcome",30)?;
        if !["close","reply","needs_attention"].contains(&outcome.as_str())
            &&!(single_pass&&["hide","delete"].contains(&outcome.as_str())){return Err("Invalid review decision")}
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
            let p=target_proposals[0];let kind=if outcome=="reply"{"reply_and_close"}else{outcome.as_str()};
            if p["kind"]!=kind{return Err("Review proposal kind mismatch")}
            let body=text(p,"text",12000)?;
            if outcome=="reply"&&body.trim().is_empty(){return Err("Review reply missing")}
            if outcome!="reply"&&!body.is_empty(){return Err("Review non-reply must have empty text")}
            clean_proposals.push(json!({"itemId":item,"kind":kind,"text":body}));
        }
        cleaned.push(json!({"itemId":item,"outcome":outcome,"reason":reason,"tags":tags}));
    }
    if proposals.iter().any(|p|p["itemId"].as_str().is_none_or(|id|!allowed.contains(id))){return Err("Foreign review proposal")}
    let mut clean=json!({"text":text(result,"text",60000)?,"sources":[],"assessments":cleaned,"proposals":clean_proposals});
    // Bind optional new-contract proof to the exact cleaned final proposals.
    // Historical reviews without proof remain readable, never auto-approved.
    if let Some(proof)=crate::editorial_review::generation_evidence(result)? {
        clean["editorialEvidence"]=proof;
        crate::editorial_review::generation_evidence(&clean)?;
    }
    if let Some(proof)=moderation_evidence(result)?{clean["moderationEvidence"]=proof;}
    Ok(clean)
}

pub(super) fn moderation_evidence(result:&Value)->Result<Option<Value>,&'static str>{
    let proposals:Vec<_>=rows(result,"proposals").iter().filter(|p|matches!(p["kind"].as_str(),Some("hide"|"delete"))).collect();
    let Some(proof)=result.get("moderationEvidence") else {
        return if proposals.is_empty(){Ok(None)}else{Err("Moderation rule evidence missing")};
    };
    let invalid="Invalid moderation rule evidence";
    if proof.as_object().is_none_or(|o|o.len()!=2)||proof["version"]!=1{return Err(invalid)}
    let entries=proof["entries"].as_array().filter(|a|a.len()==proposals.len()&&a.len()<=100).ok_or(invalid)?;
    let mut seen=BTreeSet::new();
    for entry in entries{
        let id=text(entry,"itemId",256)?;
        if entry.as_object().is_none_or(|o|o.len()!=3)||!seen.insert(id)
            ||!proposals.iter().any(|p|p["itemId"]==entry["itemId"]&&p["kind"]==entry["kind"]){return Err(invalid)}
        let refs=entry["ruleRefs"].as_array().filter(|r|!r.is_empty()&&r.len()<=10).ok_or(invalid)?;
        let mut rule_ids=BTreeSet::new();
        for rule in refs{
            if rule.as_object().is_none_or(|o|o.len()!=3)||text(rule,"entryId",256)?.is_empty()
                ||text(rule,"versionId",256)?.is_empty()||!rule_ids.insert(text(rule,"entryId",256)?)
                ||rule["hash"].as_str().is_none_or(|h|h.len()!=64||!h.bytes().all(|b|b.is_ascii_hexdigit())){return Err(invalid)}
        }
    }
    Ok(Some(proof.clone()))
}

pub(super) fn validate_moderation(request:&Value,result:&Value)->Result<(),&'static str>{
    if request["preparationMode"]!="single_pass_v1" {
        return if result.get("moderationEvidence").is_some_and(|proof|!rows(proof,"entries").is_empty()){
            Err("Moderation preparation scope mismatch")
        }else{Ok(())};
    }
    let Some(proof)=moderation_evidence(result)? else{return Ok(())};
    if rows(&proof,"entries").is_empty(){return Ok(())}
    let context=&request["moderationContext"];
    if request["preparationMode"]!="single_pass_v1"||request["purpose"]!="triage"
        ||!crate::codex_model_policy::preparation_route(&result["runMetadata"])
        ||result["runMetadata"]["promptVersion"]!="communityhero-preparation-v1-single-pass"
        ||context["version"]!=1||context["account"]!=request["account"]
        ||context["connectorBinding"]!=request["connectorBinding"]||!context["connectorBinding"].is_object(){
        return Err("Moderation preparation scope mismatch");
    }
    let editorial=crate::editorial_review::generation_evidence(result)?.ok_or("Moderation editorial evidence missing")?;
    for entry in rows(&proof,"entries"){
        let item=rows(request,"items").iter().find(|item|item["id"]==entry["itemId"]).ok_or("Foreign moderation recipient")?;
        let kind=entry["kind"].as_str().ok_or("Invalid moderation action")?;
        if !rows(&editorial,"entries").iter().any(|e|e["itemId"]==entry["itemId"]&&e["kind"]==entry["kind"]&&e["decision"]=="accept"){
            return Err("Moderation editorial acceptance missing");
        }
        if item["moderationCapabilities"][kind]!="supported"{return Err("Moderation capability unavailable")}
        for rule in rows(entry,"ruleRefs"){
            if !rows(context,"ruleRefs").contains(rule)||!rows(request,"knowledgeManifest").iter().any(|m|
                m["kind"]=="rule"&&m["scope"]["account"]==request["account"]&&m["entryId"]==rule["entryId"]
                &&m["versionId"]==rule["versionId"]&&m["hash"]==rule["hash"]
                &&m["scope"]["postKeys"].as_array().is_some_and(|keys|keys.is_empty()||keys.contains(&item["postKey"])))
                ||!rows(request,"materials").iter().any(|m|m["kind"]=="rule"&&m["knowledgeEntryId"]==rule["entryId"]
                    &&m["knowledgeVersionId"]==rule["versionId"]&&m["text"].as_str().is_some_and(|s|!s.trim().is_empty())){
                return Err("Moderation active rule binding mismatch");
            }
        }
    }
    Ok(())
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

/// A branch is one decision dependency: sibling replies can change each other's
/// meaning, while a different branch does not inherit a difficult recipient's
/// second-pass requirement.
fn review_ids(request:&Value,clean:&Value)->BTreeSet<String>{
    let mut selected=BTreeSet::new();
    for item in rows(request,"items") {
        let id=item["id"].as_str().unwrap_or("");
        let assessment=rows(clean,"assessments").iter().find(|a|a["itemId"]==id);
        let risky=assessment.is_some_and(|a|a["outcome"]=="close"||a["outcome"]=="needs_attention"
            ||rows(a,"tags").iter().any(|t|matches!(t.as_str(),Some("complaint"|"needs_fact"|"moderation"|"question"|"purchase"))));
        if risky||price_question_or_comparison(item){selected.insert(id.to_owned());}
    }
    let branches:BTreeSet<String>=rows(request,"items").iter().filter(|i|selected.contains(i["id"].as_str().unwrap_or("")))
        .filter_map(|i|i["branchId"].as_str().filter(|s|!s.is_empty()).map(str::to_owned)).collect();
    for item in rows(request,"items") {
        if item["branchId"].as_str().is_some_and(|id|branches.contains(id)) {
            if let Some(id)=item["id"].as_str(){selected.insert(id.to_owned());}
        }
    }
    selected
}

/// Only the server-selected, digest-bound preparation route may admit one
/// high-effort generation as its own exact editorial review. The adapter's
/// route metadata and judgments are checked here; model-authored prose or a
/// model-supplied mode cannot choose the admission path.
fn single_pass_proof(request:&Value,result:&Value)->Result<bool,&'static str>{
    let Some(mode)=request.get("preparationMode") else {return Ok(false)};
    if mode!="single_pass_v1" {return Err("Unsupported preparation mode")}
    if request["purpose"]!="triage" {return Err("Single-pass preparation purpose mismatch")}
    validate_moderation(request,result)?;
    let metadata=super::prepare_bundle::generation_metadata(result)?
        .ok_or("Single-pass generation provenance missing")?;
    if request.get("researchLimitContract").is_some_and(|v|v!="uncapped_evidence_v1")
        ||metadata.get("researchLimitContract")!=request.get("researchLimitContract") {
        return Err("Single-pass research limit contract mismatch");
    }
    if metadata["schemaVersion"]!=1 || !crate::codex_model_policy::preparation_route(&metadata)
        || metadata["promptVersion"]!="communityhero-preparation-v1-single-pass" {
        return Err("Single-pass generation route mismatch");
    }
    if result["runMetadata"].get("researchRepair").is_some() {
        return Err("Single-pass research repair is unsupported");
    }
    let declared=result["runMetadata"].get("decisionDependencies")
        .ok_or("Single-pass decision dependencies missing")?;
    sanitize_decision_dependencies(declared,&ids(request)?)?;
    let proof=crate::editorial_review::generation_evidence(result)?
        .ok_or("Single-pass editorial proof missing")?;
    let proposals=rows(result,"proposals");
    let entries=rows(&proof,"entries");
    if entries.len()!=proposals.len() || entries.iter().any(|entry|entry["decision"]!="accept") {
        return Err("Single-pass editorial acceptance coverage mismatch");
    }
    // generation_evidence has already bound each unique entry to one exact
    // proposal kind and text hash and checked all three semantic passes.
    Ok(true)
}

pub(super) fn subset_request(request:&Value,allowed:&BTreeSet<String>)->Value{
    let mut out=request.clone();
    let items:Vec<Value>=rows(request,"items").iter().filter(|i|i["id"].as_str().is_some_and(|id|allowed.contains(id))).cloned().collect();
    let branch_ids:BTreeSet<&str>=items.iter().filter_map(|i|i["branchId"].as_str()).collect();
    out["items"]=json!(items);
    // The adapter validates complete media-equivalence edges before its
    // model-only projection. Retain all posts referenced by retained materials
    // and manifest pins; the JS projection later removes unrelated model input.
    // Unknown legacy source pointers likewise cannot prove a branch unrelated.
    if request["branches"].is_array(){out["branches"]=if items.iter().any(|i|i["branchId"].as_str().is_none_or(str::is_empty)){
        request["branches"].clone()
    }else{json!(rows(request,"branches").iter()
        .filter(|b|b["id"].as_str().is_some_and(|id|branch_ids.contains(id))).collect::<Vec<_>>())};}
    if request["customerCases"].is_array(){out["customerCases"]=json!(rows(request,"customerCases").iter()
        .filter(|c|c["itemId"].as_str().is_some_and(|id|allowed.contains(id))).collect::<Vec<_>>());}
    if request["visualSelection"]["postImages"].is_array(){
        out["visualSelection"]["postImages"]=json!(rows(&request["visualSelection"],"postImages").iter()
            .filter(|selection|selection["itemId"].as_str().is_some_and(|id|allowed.contains(id))).collect::<Vec<_>>());
    }
    if request["firstPass"].is_object(){for key in ["assessments","proposals"]{
        out["firstPass"][key]=json!(rows(&request["firstPass"],key).iter()
            .filter(|v|v["itemId"].as_str().is_some_and(|id|allowed.contains(id))).collect::<Vec<_>>());
    }}
    out
}

pub(super) fn plan_review(request:&Value,first:&Value)->Result<Option<Value>,&'static str>{
    let clean=clean_result_mode(first,&ids(request)?,request["preparationMode"]=="single_pass_v1")?;
    if deterministic_media_hold(request,first)? {return Ok(None);}
    if single_pass_proof(request,first)? {return Ok(None);}
    let selected=review_ids(request,&clean);
    if selected.is_empty(){return Ok(None)}
    let mut review=request.clone();review["purpose"]=json!("triage_review");review["firstPass"]=clean;
    review["firstPass"]["trust"]=json!("untrusted_model_output");
    Ok(Some(subset_request(&review,&selected)))
}

pub(crate) fn plan_review_for_job(job:&Value)->Result<Option<Value>,&'static str>{
    let request=&job["prepareBundle"]["request"];
    let first=&job["preparationStages"]["first"]["result"];
    if job["preparationStages"]["groupAdmission"].is_array()||request["preparationMode"]=="single_pass_v1"{
        return plan_review(request,first);
    }
    plan_review_legacy(request,first)
}
fn plan_review_legacy(request:&Value,first:&Value)->Result<Option<Value>,&'static str>{
    // An existing paid V71 checkpoint used the entire batch as its review
    // request. Keep its digest and recovery contract byte-for-byte.
    let clean=clean_result(first,&ids(request)?)?;
    if deterministic_media_hold(request,first)?{return Ok(None);}
    if review_ids(request,&clean).is_empty(){return Ok(None);}
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

fn evidence_scope(value:&Value)->Result<Value,&'static str>{
    let invalid="Invalid research specification scope";
    let object=value.as_object().filter(|o|o.len()<=5&&o.keys().all(|k|
        matches!(k.as_str(),"model"|"trim"|"market"|"modelYear"|"observedAt"))).ok_or(invalid)?;
    for value in object.values(){value.as_str().filter(|v|!v.trim().is_empty()&&v.chars().count()<=500).ok_or(invalid)?;}
    Ok(value.clone())
}
fn evidence_extraction(value:&Value)->Result<Value,&'static str>{
    let invalid="Invalid research extraction evidence";
    let object=value.as_object().filter(|o|o.len()<=5&&o.keys().all(|k|
        matches!(k.as_str(),"status"|"observedAt"|"rowLabels"|"columnLabels"|"values"))).ok_or(invalid)?;
    value["status"].as_str().filter(|v|matches!(*v,"complete"|"empty"|"missing_table"|"access_challenge"|"rendered_unavailable")).ok_or(invalid)?;
    if let Some(time)=object.get("observedAt"){time.as_str().filter(|v|!v.trim().is_empty()&&v.chars().count()<=500).ok_or(invalid)?;}
    for key in ["rowLabels","columnLabels","values"] {if let Some(values)=object.get(key){
        let values=values.as_array().filter(|v|v.len()<=20).ok_or(invalid)?;
        for value in values {value.as_str().filter(|v|!v.trim().is_empty()&&v.chars().count()<=500).ok_or(invalid)?;}
    }}
    Ok(value.clone())
}
fn complete_specification_scope(scope:&Value)->bool{
    ["model","trim","market"].iter().all(|key|scope[*key].as_str().is_some_and(|v|!v.trim().is_empty()))
        &&["modelYear","observedAt"].iter().any(|key|scope[*key].as_str().is_some_and(|v|!v.trim().is_empty()))
}
fn mismatched_specification_scope(scope:&Value,source:&Value)->bool{
    ["model","trim","market","modelYear","observedAt"].iter().any(|key|
        scope[*key].is_string()&&source[*key].is_string()&&scope[*key]!=source[*key])
}
pub(super) fn research_quality_fields(source:&Value)->Result<Value,&'static str>{
    let mut clean=json!({});
    if let Some(kind)=source.get("claimKind"){
        kind.as_str().filter(|v|matches!(*v,"source_statement"|"product_specification")).ok_or("Invalid research claim kind")?;
        clean["claimKind"]=kind.clone();
    }
    for key in ["scope","sourceScope"] {if let Some(scope)=source.get(key){clean[key]=evidence_scope(scope)?;}}
    if let Some(extraction)=source.get("extraction"){clean["extraction"]=evidence_extraction(extraction)?;}
    Ok(clean)
}
fn quality_public_url(value:&Value)->Result<String,&'static str>{
    let invalid="Invalid held research URL";let url=text(value,"url",2048)?;
    let uri=url.parse::<axum::http::Uri>().map_err(|_|invalid)?;
    let host=uri.host().ok_or(invalid)?.to_ascii_lowercase();
    if !matches!(uri.scheme_str(),Some("http"|"https"))||uri.authority().is_some_and(|a|a.as_str().contains('@'))
        ||url.chars().any(|c|c.is_control()||c.is_whitespace())||!host.contains('.')||host.ends_with(".local")
        ||host.starts_with('[')||host.starts_with("0."){return Err(invalid)}
    if let Ok(std::net::IpAddr::V4(ip))=host.parse::<std::net::IpAddr>(){
        if ip.is_loopback()||ip.is_private()||ip.is_link_local()||ip.is_unspecified(){return Err(invalid)}
    }
    Ok(url)
}
// Bind model declarations to the current engine account before any durable use.
// Aggregate chunk metadata receives the same check recursively.
pub(super) fn validate_evidence_quality_company(metadata:&Value,current_account:&str,decisions:&Value)->Result<(),&'static str>{
    if let Some(holds)=metadata["research"].get("evidenceHolds"){
        let expected=match current_account {"LikeAvto"=>"likeavto","BAW Russia"=>"baw-russia",_=>return Err("Unknown research evidence account")};
        for hold in holds.as_array().ok_or("Invalid research evidence holds")?{
            if hold["accountKey"]!=expected{return Err("Foreign research evidence account")}
            if !rows(decisions,"assessments").iter().any(|a|a["itemId"]==hold["itemId"]&&a["outcome"]=="needs_attention")
                ||rows(decisions,"proposals").iter().any(|p|p["itemId"]==hold["itemId"]){return Err("Research evidence hold has an executable decision")}
        }
    }
    if let Some(chunks)=metadata.get("chunks"){
        for chunk in chunks.as_array().ok_or("Invalid research evidence chunks")?{
            validate_evidence_quality_company(&chunk["metadata"],current_account,decisions)?;
        }
    }
    Ok(())
}

pub(super) fn sanitize_research(value:&Value,allowed:&BTreeSet<String>)->Result<Value,&'static str>{
    if value.get("webCallLimit").is_some(){return Err("Uncapped research requires single-pass provenance")}
    sanitize_research_inner(value,allowed,false)
}

// Caller verifies the current outer review route and its exact chunk contract.
pub(super) fn sanitize_uncapped_review_research(value:&Value,allowed:&BTreeSet<String>)->Result<Value,&'static str>{
    if value.get("webCallLimit")!=Some(&Value::Null)||value["model"]!=crate::codex_model_policy::MODEL
        ||value["modelProfile"]!=crate::codex_model_policy::PROFILE||value["reasoningEffort"]!="medium"{
        return Err("Invalid uncapped review research provenance");
    }
    sanitize_research_inner(value,allowed,true)
}

// Caller binds the outer model/effort/prompt tuple. Old single-pass receipts
// without the explicit marker retain their original eight-call contract.
pub(super) fn sanitize_single_pass_research(value:&Value,allowed:&BTreeSet<String>)->Result<Value,&'static str>{
    if value.get("webCallLimit").is_none(){return sanitize_research(value,allowed)}
    if value.get("webCallLimit")!=Some(&Value::Null)||!crate::codex_model_policy::preparation_route(value){
        return Err("Invalid single-pass research budget")
    }
    sanitize_research_inner(value,allowed,true)
}

fn sanitize_research_inner(value:&Value,allowed:&BTreeSet<String>,uncapped:bool)->Result<Value,&'static str>{
    if value["version"]!=1||!matches!(value["status"].as_str(),Some("completed"|"no_sources")){return Err("Unsupported research provenance")}
    let mut clean=json!({"version":1,"status":value["status"],"trust":"source_only"});
    crate::codex_model_policy::validate_profile(value)?;
    if let Some(profile)=value.get("modelProfile"){clean["modelProfile"]=profile.clone();}
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
    let calls=value["webCalls"].as_u64().filter(|n|*n<=9_007_199_254_740_991&&(uncapped||*n<=8)).ok_or("Invalid research call count")?;
    clean["elapsedMs"]=json!(elapsed);clean["webCalls"]=json!(calls);
    if uncapped{clean["webCallLimit"]=Value::Null;}
    if value.to_string().len()>2*1024*1024{return Err("Research provenance exceeds byte budget");}
    let source_limit=if uncapped&&value["model"]==crate::codex_model_policy::MODEL{usize::MAX}else if uncapped{300}else{30};
    let sources=value["sources"].as_array().filter(|s|s.len()<=source_limit).ok_or("Invalid research sources")?;
    if (!sources.is_empty()&&calls==0)||(value["status"]=="completed"&&sources.is_empty())||(value["status"]=="no_sources"&&!sources.is_empty()){return Err("Research status disagrees with evidence")}
    let mut clean_sources=Vec::new();
    for source in sources {
        let item=text(source,"itemId",256)?;if !allowed.contains(&item){return Err("Foreign research recipient")}
        let url=text(source,"url",2048)?;
        let uri=url.parse::<axum::http::Uri>().map_err(|_|"Invalid research URL")?;
        if !matches!(uri.scheme_str(),Some("http"|"https"))||uri.host().is_none()||uri.authority().is_some_and(|a|a.as_str().contains('@'))||url.chars().any(|c|c.is_control()||c.is_whitespace()){return Err("Invalid research URL")}
        let quality=research_quality_fields(source)?;
        if quality.get("extraction").is_some_and(|v|v["status"]!="complete")
            ||(quality["claimKind"]=="product_specification"&&(!complete_specification_scope(&quality["scope"])
                ||mismatched_specification_scope(&quality["scope"],&quality["sourceScope"]))){return Err("Held evidence cannot substantiate research")}
        let mut clean_source=json!({"itemId":item,"url":url,"title":text(source,"title",500)?,"claim":text(source,"claim",6000)?,"trust":"source_only"});
        clean_source.as_object_mut().unwrap().extend(quality.as_object().unwrap().clone());clean_sources.push(clean_source);
    }
    clean["sources"]=json!(clean_sources);
    if let Some(holds)=value.get("evidenceHolds"){
        let invalid="Invalid research evidence holds";
        let holds=holds.as_array().filter(|v|!v.is_empty()&&v.len()<=source_limit).ok_or(invalid)?;let mut retained=Vec::new();
        for hold in holds {
            hold.as_object().filter(|o|o.keys().all(|k|matches!(k.as_str(),"version"|"accountKey"|"itemId"|"url"|"reason"|"scope"|"sourceScope"|"extraction"|"renderedFallback"))).ok_or(invalid)?;
            if hold["version"]!=1||!matches!(hold["accountKey"].as_str(),Some("likeavto"|"baw-russia")){return Err(invalid)}
            let item=text(hold,"itemId",256)?;if !allowed.contains(&item){return Err(invalid)}
            let url=quality_public_url(hold)?;let quality=research_quality_fields(hold)?;
            let reason=hold["reason"].as_str().ok_or(invalid)?;
            match reason {
                "incomplete_extraction"=>{
                    if quality.get("extraction").is_none_or(|v|v["status"]=="complete"){return Err(invalid)}
                    let expected=if quality["extraction"]["status"]=="access_challenge"{"access_challenge"}else{"unsupported"};
                    if hold["renderedFallback"]!=json!({"status":expected,"attempts":0,"capability":"web.run_text_only"}){return Err(invalid)}
                },
                "specification_scope_incomplete"=>{if complete_specification_scope(&quality["scope"])||hold.get("renderedFallback").is_some(){return Err(invalid)}},
                "specification_scope_mismatch"=>{if !mismatched_specification_scope(&quality["scope"],&quality["sourceScope"])||hold.get("renderedFallback").is_some(){return Err(invalid)}},
                _=>return Err(invalid)
            }
            let mut retained_hold=json!({"version":1,"accountKey":hold["accountKey"],"itemId":item,"url":url,"reason":reason});
            retained_hold.as_object_mut().unwrap().extend(quality.as_object().unwrap().clone());
            if let Some(fallback)=hold.get("renderedFallback"){retained_hold["renderedFallback"]=fallback.clone();}
            retained.push(retained_hold);
        }
        clean["evidenceHolds"]=json!(retained);
    }
    if let Some(rejected)=value.get("rejectedSources") {
        let invalid="Invalid rejected research provenance";
        rejected.as_object().filter(|o|o.len()==(if uncapped{8}else{6})
            && ["version","reason","webCallsUsed","webCallsLimit","openedUrlSha256","sources"].iter().all(|k|o.contains_key(*k))).ok_or(invalid)?;
        if uncapped&&["sourcesTruncated","openedUrlsTruncated"].iter().any(|k|!rejected[k].is_boolean()){return Err(invalid)}
        let reason=rejected["reason"].as_str().filter(|v|matches!(*v,"budget_exhausted"|"still_unobserved")).ok_or(invalid)?;
        let used=rejected["webCallsUsed"].as_u64().filter(|n|*n==calls).ok_or(invalid)?;
        let limit=if uncapped{
            if rejected.get("webCallsLimit")!=Some(&Value::Null)||reason!="still_unobserved"{return Err(invalid)}
            Value::Null
        }else{json!(rejected["webCallsLimit"].as_u64().filter(|n|*n>0&&*n<=8&&*n>=used).ok_or(invalid)?)};
        if rejected["version"]!=1{return Err(invalid)}
        let digest=|v:&Value|v.as_str().is_some_and(|s|s.len()==64&&s.bytes().all(|b|b.is_ascii_hexdigit()));
        let opened=rejected["openedUrlSha256"].as_array().filter(|v|v.len()<=8).ok_or(invalid)?;
        let mut opened_seen=BTreeSet::new();
        for hash in opened {if !digest(hash)||!opened_seen.insert(hash.as_str().unwrap()){return Err(invalid)}}
        let sources=rejected["sources"].as_array().filter(|v|!v.is_empty()&&v.len()<=30).ok_or(invalid)?;
        let mut source_seen=BTreeSet::new();let mut clean_sources=Vec::new();
        for source in sources {
            source.as_object().filter(|o|(o.len()==3||o.len()==4)
                && ["itemId","candidateUrlSha256","comparison"].iter().all(|k|o.contains_key(*k))
                && o.keys().all(|k|matches!(k.as_str(),"itemId"|"candidateUrlSha256"|"comparison"|"openedUrlSha256"))).ok_or(invalid)?;
            let item=text(source,"itemId",256)?;if !allowed.contains(&item)||!digest(&source["candidateUrlSha256"]){return Err(invalid)}
            let hash=source["candidateUrlSha256"].as_str().unwrap();
            let comparison=source["comparison"].as_str().filter(|v|matches!(*v,
                "no_completed_literal_open"|"scheme_variant"|"query_variant"|"path_variant"|"not_observed")).ok_or(invalid)?;
            let variant=matches!(comparison,"scheme_variant"|"query_variant"|"path_variant");
            let opened_hash=source.get("openedUrlSha256");
            if variant!=opened_hash.is_some()||opened_hash.is_some_and(|v|!digest(v)
                ||!opened_seen.contains(v.as_str().unwrap())){return Err(invalid)}
            if !source_seen.insert((item.clone(),hash.to_owned())){return Err(invalid)}
            let mut clean_source=json!({"itemId":item,"candidateUrlSha256":hash,"comparison":comparison});
            if let Some(opened_hash)=opened_hash {clean_source["openedUrlSha256"]=opened_hash.clone();}
            clean_sources.push(clean_source);
        }
        clean["rejectedSources"]=json!({"version":1,"reason":reason,"webCallsUsed":used,"webCallsLimit":limit,
            "openedUrlSha256":opened,"sources":clean_sources});
        if uncapped{for key in ["sourcesTruncated","openedUrlsTruncated"]{clean["rejectedSources"][key]=rejected[key].clone();}}
    }
    Ok(clean)
}

/// Bind the returned first pass to the captured request before retaining it.
/// Currentness belongs at the next model dispatch and at proposal admission.
pub(super) fn settle_first(d:&mut Value,job:&str,request:&Value,result:&Value,at:&str)->super::ApiResult<Option<Value>>{
    use sha2::{Digest,Sha256};
    let record=super::row(d,"jobs",job)?;
    let bundle=&record["prepareBundle"];
    let binding=super::active_binding(d)?;
    super::bridge_account(&binding)?;
    if record["kind"]!="assistant"||!["engine_prepare","auto_prepare","auto_revalidate"].contains(&record["purpose"].as_str().unwrap_or(""))
        ||bundle["version"]!=1||bundle["digest"]!=format!("{:x}",Sha256::digest(request.to_string().as_bytes()))
        ||bundle["request"]!=*request||request["account"]!=d["account"]||request["connectorBinding"]!=binding.to_json(){
        return Err(super::conflict("First-pass return ownership changed"));
    }
    record_first(d,job,result,at)
}

pub(super) fn record_first(d:&mut Value,job:&str,result:&Value,at:&str)->super::ApiResult<Option<Value>>{
    let record=super::row(d,"jobs",job)?.clone();
    if record["status"]!="running"{return Err(super::conflict("Preparation job no longer running"))}
    let request=&record["prepareBundle"]["request"];
    let material_receipt=crate::model_material_receipt::result_receipt(request,result).map_err(super::bad)?;
    let allowed=ids(request).map_err(super::bad)?;
    let mut clean=clean_result_mode(result,&allowed,request["preparationMode"]=="single_pass_v1").map_err(super::bad)?;
    let deterministic=deterministic_media_hold(request,result).map_err(super::bad)?;
    if deterministic {
        clean["decisionSource"]=json!("deterministic_media_source_gap");
    }
    if let Some(metadata)=super::prepare_bundle::generation_metadata(result).map_err(super::bad)?{
        validate_evidence_quality_company(&metadata,d["account"].as_str().unwrap_or(""),result).map_err(super::bad)?;
        super::prepare_bundle::validate_image_evidence_binding(&metadata,&record["prepareBundle"]).map_err(super::bad)?;
        clean["runMetadata"]=metadata;
    }
    if let Some(receipt)=material_receipt{clean["modelMaterialReceipt"]=receipt;}
    if result.get("videoFrameNeeds").is_some(){
        let needs=crate::video_frame_work::record_needs(d,job,request,result,at)?;
        clean["videoFrameNeeds"]=result["videoFrameNeeds"].clone();clean["nativeVideoFrameNeeds"]=needs;
    }
    let plan=if record["preparationStages"]["groupAdmission"].is_array()||request["preparationMode"]=="single_pass_v1"{plan_review(request,result)}
        else{plan_review_legacy(request,result)}.map_err(super::bad)?;
    if request.get("factDependencyContract").is_some(){clean["factDependencies"]=json!(super::fact_followup::declarations(request,result).map_err(super::bad)?);}
    let stage=json!({"status":"completed","at":at,"result":clean,"reviewRequired":plan.is_some(),"reason":if deterministic{"deterministic_media_source_gap"}else if request["preparationMode"]=="single_pass_v1"{"single_pass_editorial_proof"}else if plan.is_some(){"decision_or_substantive_question"}else{"routine_feedback_reply"},"trust":if deterministic{"source_evidence_only"}else{"untrusted_model_output"}});
    let old=&record["preparationStages"]["first"];
    if !old.is_null(){if old["result"]==stage["result"]{return Ok(plan)}return Err(super::conflict("First-pass evidence is immutable"))}
    super::fact_followup::record(d,job,request,result,at)?;
    super::row_mut(d,"jobs",job)?["preparationStages"]["first"]=stage;Ok(plan)
}

pub(super) fn record_review(d:&mut Value,job:&str,outcome:Result<&Value,&str>,at:&str)->super::ApiResult<()> {
    let record=super::row(d,"jobs",job)?.clone();
    if record["preparationStages"]["first"]["reviewRequired"]!=true{return Err(super::conflict("Review has no persisted first pass"))}
    let request=&record["prepareBundle"]["request"];
    let plan=plan_review_for_job(&record).map_err(super::bad)?
        .ok_or_else(||super::conflict("Review plan is no longer required"))?;
    let allowed=ids(&plan).map_err(super::bad)?;
    let stage=match outcome {
        Err(reason)=>{
            let (code,retryable)=failure_category(reason);
            json!({"status":"failed","at":at,"error":"Stronger review failed; first pass retained","errorCode":code,"retryable":retryable})
        },
        Ok(result)=>{
            let mut clean=clean_result(result,&allowed).map_err(super::bad)?;
            let metadata=super::prepare_bundle::generation_metadata(result).map_err(super::bad)?.ok_or_else(||super::bad("Review generation metadata missing"))?;
            validate_evidence_quality_company(&metadata,d["account"].as_str().unwrap_or(""),result).map_err(super::bad)?;
            super::prepare_bundle::validate_image_evidence_binding(&metadata,&record["prepareBundle"]).map_err(super::bad)?;
            let research=if metadata["schemaVersion"]==2 {
                chunks::research_projection(&metadata).map_err(super::bad)?
            }else{sanitize_research(&metadata["research"],&allowed).map_err(super::bad)?};
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
    #[test]
    fn baw_original_first_capture_matches_real_bridge_operation_normalization_only(){
        let profile=crate::accounts::Profile::BawRussia;
        let request=json!({"account":"BAW Russia","purpose":"triage","posts":[{"id":"p","text":"Exact source"}]});
        let mut wire=request.clone();profile.bind_request(&mut wire).unwrap();wire["operation"]=json!("assistant");
        let capture=json!({"company":"baw-russia","account":"BAW Russia","binding":{"operation":"assistant"},"request":wire});
        assert!(first_capture_matches(profile,&request,&capture).unwrap());
        for path in ["/request/operation","/binding/operation"]{let mut bad=capture.clone();*bad.pointer_mut(path).unwrap()=json!("readback");assert!(!first_capture_matches(profile,&request,&bad).unwrap());}
        let mut bad=capture.clone();bad["request"]["posts"][0]["text"]=json!("Changed source");assert!(!first_capture_matches(profile,&request,&bad).unwrap());
        let wrapper=json!({"company":"baw-russia","account":"BAW Russia","binding":{"operation":"assistant"},"request":{"account":"baw-russia","operation":"assistant","request":request}});
        assert!(first_capture_matches(profile,&request,&wrapper).unwrap());
    }
    #[test]
    fn visual_selection_subset_keeps_exact_retained_recipients_and_legacy_absence(){
        let allowed=BTreeSet::from(["a".to_owned()]);
        let legacy=json!({"items":[{"id":"a"},{"id":"b"}]});
        assert!(subset_request(&legacy,&allowed).get("visualSelection").is_none());
        let mut request=legacy;
        let selected=json!({"itemId":"a","postId":"p","attachmentIndices":[1],"reason":"Read diagram"});
        request["visualSelection"]=json!({"version":1,"postImages":[selected.clone(),
            {"itemId":"b","postId":"p","attachmentIndices":[0],"reason":"Read figure"}]});
        assert_eq!(subset_request(&request,&allowed)["visualSelection"],json!({"version":1,"postImages":[selected]}));
        assert_eq!(request["visualSelection"]["postImages"].as_array().unwrap().len(),2,"immutable input survives projection");
        let empty=subset_request(&request,&BTreeSet::from(["missing".to_owned()]));
        assert_eq!(empty["visualSelection"],json!({"version":1,"postImages":[]}));
        request["visualSelection"]["postImages"]=Value::Null;
        assert_eq!(subset_request(&request,&allowed)["visualSelection"],request["visualSelection"],"invalid shape must reach validation unchanged");
    }
    const AT:&str="2026-09-22T10:00:00Z";
    fn request()->Value{json!({"purpose":"triage","account":"LikeAvto","items":[{"id":"i","postId":"p","postKey":"vk:p"}],"posts":[{"id":"p","postKey":"vk:p","title":"Video"}]})}
    fn result(outcome:&str,tags:Value)->Value{
        let proposals=if outcome=="needs_attention"{json!([])}else{json!([{"itemId":"i","kind":if outcome=="reply"{"reply_and_close"}else{"close"},"text":if outcome=="reply"{"Reply"}else{""}}])};
        json!({"text":"Explanation","sources":[],"assessments":[{"itemId":"i","outcome":outcome,"reason":"Reason","tags":tags}],"proposals":proposals})
    }
    fn research()->Value{json!({"version":1,"status":"completed","model":"m","reasoningEffort":"medium","instructionSha256":"a".repeat(64),"inputSha256":"b".repeat(64),"elapsedMs":8,"webCalls":2,"sources":[{"itemId":"i","url":"https://example.com/facts","title":"Page","claim":"Claim","secret":"remove"}],"completedAt":AT,"secret":"remove"})}
    #[test]
    fn current_uncapped_research_has_byte_bounds_without_arbitrary_source_counts(){
        let allowed=BTreeSet::from(["i".to_owned()]);let mut r=research();
        r["model"]=json!(crate::codex_model_policy::MODEL);r["modelProfile"]=json!(crate::codex_model_policy::PROFILE);
        r["webCallLimit"]=Value::Null;r["webCalls"]=json!(400);
        let source=r["sources"][0].clone();r["sources"]=json!((0..400).map(|n|{let mut s=source.clone();s["url"]=json!(format!("https://example.com/{n}"));s}).collect::<Vec<_>>());
        assert_eq!(sanitize_uncapped_review_research(&r,&allowed).unwrap()["sources"].as_array().unwrap().len(),400);
        r["reasoningEffort"]=json!("high");assert_eq!(sanitize_single_pass_research(&r,&allowed).unwrap()["sources"].as_array().unwrap().len(),400);
        assert!(sanitize_research(&r,&allowed).is_err());
        r["model"]=json!("gpt-6-astra");r.as_object_mut().unwrap().remove("modelProfile");assert!(sanitize_single_pass_research(&r,&allowed).is_err());
    }
    fn reviewed()->Value{
        let mut r=result("reply",json!(["needs_fact"]));
        r["runMetadata"]=json!({"schemaVersion":1,"model":"m","reasoningEffort":"medium","promptVersion":"v","instructionSha256":"a".repeat(64),"inputSha256":"b".repeat(64),"cliSha256":"c".repeat(64),"elapsedMs":8,"completedAt":AT,"research":research(),"secret":"remove"});r
    }
    #[test]
    fn single_pass_uncapped_research_keeps_observed_usage_and_legacy_budget() {
        let allowed=BTreeSet::from(["i".to_owned()]);let mut value=research();
        value["model"]=json!("gpt-6-astra");value["reasoningEffort"]=json!("high");
        value["webCallLimit"]=Value::Null;value["webCalls"]=json!(40);
        let clean=sanitize_single_pass_research(&value,&allowed).unwrap();
        assert_eq!(clean["webCalls"],40);assert_eq!(clean.get("webCallLimit"),Some(&Value::Null));
        assert_eq!(clean["sources"][0]["itemId"],"i");assert!(clean.get("secret").is_none());
        assert!(sanitize_research(&value,&allowed).is_err());
        for (key,bad) in [("model",json!("m")),("reasoningEffort",json!("medium")),("webCallLimit",json!(8)),
            ("webCalls",json!(-1)),("webCalls",json!(1.5)),("webCalls",json!(9_007_199_254_740_992u64))] {
            let mut invalid=value.clone();invalid[key]=bad;assert!(sanitize_single_pass_research(&invalid,&allowed).is_err());
        }
        let mut old=value.clone();old.as_object_mut().unwrap().remove("webCallLimit");
        assert!(sanitize_single_pass_research(&old,&allowed).is_err());old["webCalls"]=json!(8);
        assert!(sanitize_single_pass_research(&old,&allowed).is_ok());assert!(sanitize_research(&old,&allowed).is_ok());
        value["sources"][0]["itemId"]=json!("foreign");assert!(sanitize_single_pass_research(&value,&allowed).is_err());
    }
    #[test]
    fn single_pass_uncapped_rejected_sources_remain_exact_bounded_and_never_budget_exhausted() {
        let allowed=BTreeSet::from(["i".to_owned()]);let mut value=research();
        value["model"]=json!("gpt-6-astra");value["reasoningEffort"]=json!("high");
        value["webCallLimit"]=Value::Null;value["webCalls"]=json!(40);
        value["rejectedSources"]=json!({"version":1,"reason":"still_unobserved","webCallsUsed":40,"webCallsLimit":null,
            "sourcesTruncated":false,"openedUrlsTruncated":false,
            "openedUrlSha256":[],"sources":[{"itemId":"i","candidateUrlSha256":"a".repeat(64),"comparison":"no_completed_literal_open"}]});
        assert_eq!(sanitize_single_pass_research(&value,&allowed).unwrap()["rejectedSources"],value["rejectedSources"]);
        for (key,bad) in [("reason",json!("budget_exhausted")),("webCallsUsed",json!(8)),("webCallsLimit",json!(40))]{
            let mut invalid=value.clone();invalid["rejectedSources"][key]=bad;
            assert!(sanitize_single_pass_research(&invalid,&allowed).is_err());
        }
        value["rejectedSources"]["openedUrlSha256"]=json!((0..9).map(|n|format!("{n:064x}")).collect::<Vec<_>>());
        assert!(sanitize_single_pass_research(&value,&allowed).is_err());
    }
    #[test]
    fn single_pass_source_capacity_is_three_per_hundred_items_without_changing_legacy_thirty() {
        let mut value=research();value["model"]=json!("gpt-6-astra");value["reasoningEffort"]=json!("high");
        value["webCallLimit"]=Value::Null;value["webCalls"]=json!(300);
        let allowed:BTreeSet<String>=(0..100).map(|n|format!("item-{n}")).collect();
        let original=value["sources"][0].clone();
        value["sources"]=json!((0..300).map(|n|{let mut s=original.clone();s["itemId"]=json!(format!("item-{}",n/3));
            s["url"]=json!(format!("https://example.com/source-{n}"));s}).collect::<Vec<_>>());
        assert_eq!(sanitize_single_pass_research(&value,&allowed).unwrap()["sources"].as_array().unwrap().len(),300);
        let mut excess=value.clone();excess["sources"].as_array_mut().unwrap().push(original);
        assert!(sanitize_single_pass_research(&excess,&allowed).is_err());
        value.as_object_mut().unwrap().remove("webCallLimit");value["webCalls"]=json!(8);
        value["sources"].as_array_mut().unwrap().truncate(31);assert!(sanitize_research(&value,&allowed).is_err());
        value["sources"].as_array_mut().unwrap().truncate(30);assert!(sanitize_research(&value,&allowed).is_ok());
    }
    fn single_pass(mut value:Value)->Value{
        value["runMetadata"]=json!({"schemaVersion":1,"model":"gpt-6-astra","reasoningEffort":"high",
            "promptVersion":"communityhero-preparation-v1-single-pass","instructionSha256":"a".repeat(64),
            "inputSha256":"b".repeat(64),"cliSha256":"c".repeat(64),"elapsedMs":8,"completedAt":AT,
            "research":{"version":1,"status":"no_sources","model":"gpt-6-astra","reasoningEffort":"high",
                "instructionSha256":"a".repeat(64),"inputSha256":"b".repeat(64),"elapsedMs":8,"webCalls":0,
                "sources":[],"completedAt":AT,"untrustedExtra":"remove"}});
        value["runMetadata"]["decisionDependencies"]=json!({"version":1,"entries":
            rows(&value,"assessments").iter().map(|a|json!({"itemId":a["itemId"],"dependsOnItemIds":[]})).collect::<Vec<_>>()});
        value["editorialEvidence"]=json!({"version":1,"contract":crate::editorial_review::CONTRACT,"entries":
            rows(&value,"proposals").iter().map(|proposal|json!({"itemId":proposal["itemId"],"kind":proposal["kind"],
                "textSha256":crate::editorial_review::hash_text(proposal["text"].as_str().unwrap()),
                "decision":"accept","reason":"Exact final action is supported","checks":{"companyRules":"pass","intent":"pass","factualScope":"pass"}})).collect::<Vec<_>>()});
        value
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
    fn deterministic_hold_is_retained_immutable_evidence_and_admits_no_stale_proposal() {
        let now=1_790_000_000;
        let mut d=crate::empty();
        d["items"]=json!([{"id":"i","itemId":"c","objectId":"o","postKey":"p","conversationKey":"thread","branchId":"b","postId":"post","revision":1,"draft":"","workflow":"attention","providerStatus":"new","createdAt":chrono::DateTime::from_timestamp(now-60,0).unwrap().to_rfc3339(),"providerObservedAt":chrono::DateTime::from_timestamp(now,0).unwrap().to_rfc3339(),"attachments":[{"type":"unsupported"}]}]);
        d["branches"]=json!([{"id":"b","postId":"post","messages":[{"id":"c","text":"Comment"}],"contextComplete":true}]);
        d["posts"]=json!([{"id":"post","text":"Post"}]);
        let (job,_)=crate::auto_prepare::claim(&mut d,now).unwrap().unwrap();
        let mut changed=d.clone();changed["items"][0]["attachments"]=json!([{"type":"photo","url":"https://example.com/current.jpg"}]);
        assert!(record_first(&mut changed,&job,&media_hold(),AT).unwrap().is_none());
        assert!(crate::prepare_bundle::current(&changed,&crate::row(&changed,"jobs",&job).unwrap()["prepareBundle"]).is_err());
        assert!(changed["proposals"].as_array().unwrap().is_empty());
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
    fn single_pass_moderation_requires_current_company_rule_capability_and_editorial(){
        let mut req=request();req["preparationMode"]=json!("single_pass_v1");
        req["connectorBinding"]=crate::accounts::Profile::LikeAvto.binding();
        req["items"][0]["moderationCapabilities"]=json!({"delete":"supported","hide":"unsupported"});
        let rule=json!({"entryId":"rule","versionId":"rule-v1","hash":"d".repeat(64)});
        req["moderationContext"]=json!({"version":1,"account":"LikeAvto","connectorBinding":req["connectorBinding"],"ruleRefs":[rule.clone()]});
        req["knowledgeManifest"]=json!([{ "entryId":"rule","versionId":"rule-v1","hash":"d".repeat(64),"kind":"rule","scope":{"account":"LikeAvto","postKeys":[]}}]);
        req["materials"]=json!([{"kind":"rule","knowledgeEntryId":"rule","knowledgeVersionId":"rule-v1","text":"Delete targeted insults; preserve substantive criticism."}]);
        let mut r=result("close",json!(["moderation"]));r["assessments"][0]["outcome"]=json!("delete");r["proposals"][0]["kind"]=json!("delete");
        r=single_pass(r);r["moderationEvidence"]=json!({"version":1,"entries":[{"itemId":"i","kind":"delete","ruleRefs":[rule]}]});
        assert!(plan_review(&req,&r).unwrap().is_none());
        let mut d=document();d["jobs"][0]["prepareBundle"]["request"]=req.clone();
        assert!(record_first(&mut d,"j",&r,AT).unwrap().is_none());
        assert_eq!(d["jobs"][0]["preparationStages"]["first"]["result"]["moderationEvidence"],r["moderationEvidence"]);
        assert_eq!(crate::prepare_bundle::generation_metadata(&r).unwrap().unwrap()["moderationEvidence"],r["moderationEvidence"]);
        for (path,bad) in [("/items/0/moderationCapabilities/delete",json!("unknown")),("/items/0/moderationCapabilities/delete",json!("unsupported")),
            ("/knowledgeManifest/0/scope/account",json!("BAW Russia")),("/knowledgeManifest/0/scope/postKeys",json!(["foreign-post"])),
            ("/knowledgeManifest/0/versionId",json!("stale")),("/materials/0/text",json!("")),("/moderationContext/connectorBinding/revision",json!(2))]{
            let mut changed=req.clone();*changed.pointer_mut(path).unwrap()=bad;assert!(plan_review(&changed,&r).is_err(),"{path}");
        }
        let mut legacy=req.clone();legacy.as_object_mut().unwrap().remove("preparationMode");assert!(plan_review(&legacy,&r).is_err());
        for path in ["/moderationEvidence/entries/0/itemId","/moderationEvidence/entries/0/ruleRefs/0/hash","/editorialEvidence/entries/0/kind"]{
            let mut changed=r.clone();*changed.pointer_mut(path).unwrap()=json!("foreign");assert!(plan_review(&req,&changed).is_err(),"{path}");
        }
    }
    #[test]
    fn explicit_single_pass_accepts_exact_high_editorial_proof_without_blanket_review(){
        let mut req=request();req["preparationMode"]=json!("single_pass_v1");
        req["items"][0]["text"]=json!("Сколько стоит под ключ?");
        for (outcome,tags) in [("reply",json!(["purchase","question"])),("close",json!(["moderation"])),
            ("needs_attention",json!(["needs_fact"]))] {
            let first=single_pass(result(outcome,tags));
            assert!(plan_review(&req,&first).unwrap().is_none(),"{outcome}");
            let mut d=document();d["jobs"][0]["prepareBundle"]["request"]=req.clone();
            d["jobs"][0]["preparationStages"]=json!({"first":null,"review":null,"groupAdmission":[]});
            assert!(record_first(&mut d,"j",&first,AT).unwrap().is_none());
            let saved=&d["jobs"][0]["preparationStages"]["first"];
            assert_eq!(saved["reviewRequired"],false);
            assert_eq!(saved["reason"],"single_pass_editorial_proof");
            assert_eq!(saved["result"]["editorialEvidence"],first["editorialEvidence"]);
            assert_eq!(saved["result"]["runMetadata"]["research"]["trust"],"source_only");
            assert!(saved["result"]["runMetadata"]["research"].get("untrustedExtra").is_none());
            assert!(plan_review(&req,&saved["result"]).unwrap().is_none(),"saved result must use same route");
        }
        let legacy=single_pass(result("reply",json!(["question"])));
        assert!(plan_review(&request(),&legacy).unwrap().is_some(),"metadata alone cannot select single pass");
    }
    #[test]
    fn sol61_single_pass_retains_profile_research_and_legacy_receipts() {
        let mut req=request();req["preparationMode"]=json!("single_pass_v1");
        let legacy=single_pass(result("reply",json!(["question"])));
        assert!(plan_review(&req,&legacy).unwrap().is_none());
        let mut next=legacy.clone();
        for key in ["runMetadata","research"] {
            let target=if key=="runMetadata" {&mut next["runMetadata"]}else{&mut next["runMetadata"]["research"]};
            target["model"]=json!(crate::codex_model_policy::MODEL);
            target["modelProfile"]=json!(crate::codex_model_policy::PROFILE);
        }
        next["runMetadata"]["cliSha256"]=json!("86e8ef1013f98df51fdeea446597f7e3ca32e454d1d4d8c0402a68b03c311d70");
        assert!(plan_review(&req,&next).unwrap().is_none());
        let clean=crate::prepare_bundle::generation_metadata(&next).unwrap().unwrap();
        assert_eq!(clean["modelProfile"],crate::codex_model_policy::PROFILE);
        assert_eq!(clean["research"]["modelProfile"],crate::codex_model_policy::PROFILE);
        for pointer in ["/runMetadata/modelProfile","/runMetadata/research/modelProfile","/runMetadata/cliSha256"] {
            let mut invalid=next.clone();*invalid.pointer_mut(pointer).unwrap()=json!("wrong");
            assert!(plan_review(&req,&invalid).is_err(),"{pointer}");
        }
        assert_eq!(legacy["runMetadata"]["model"],"gpt-6-astra");
        assert!(legacy["runMetadata"].get("modelProfile").is_none());
    }
    #[test]
    fn explicit_single_pass_without_group_admission_does_not_reenter_legacy_review(){
        let mut d=document();d["jobs"][0]["prepareBundle"]["request"]["preparationMode"]=json!("single_pass_v1");
        let first=single_pass(result("close",json!(["moderation"])));
        assert!(record_first(&mut d,"j",&first,AT).unwrap().is_none());
        assert_eq!(d["jobs"][0]["preparationStages"]["first"]["reviewRequired"],false);
        assert!(plan_review_for_job(&d["jobs"][0]).unwrap().is_none());
        let mut legacy=document();
        assert!(record_first(&mut legacy,"j",&result("close",json!(["moderation"])),AT).unwrap().is_some());
    }
    #[test]
    fn single_pass_rejects_missing_wrong_or_incomplete_provenance_without_review_fallback(){
        let mut req=request();req["preparationMode"]=json!("single_pass_v1");
        let good=single_pass(result("reply",json!(["question"])));
        for pointer in ["/runMetadata/model","/runMetadata/reasoningEffort","/runMetadata/promptVersion"] {
            let mut bad=good.clone();*bad.pointer_mut(pointer).unwrap()=json!("other");
            assert!(plan_review(&req,&bad).is_err(),"{pointer}");
        }
        let mut bad=good.clone();bad.as_object_mut().unwrap().remove("runMetadata");
        assert!(plan_review(&req,&bad).is_err());
        let mut bad=good.clone();bad.as_object_mut().unwrap().remove("editorialEvidence");
        assert!(plan_review(&req,&bad).is_err());
        let mut bad=good.clone();bad["editorialEvidence"]["entries"]=json!([]);
        assert!(plan_review(&req,&bad).is_err());
        let mut bad=good.clone();bad["editorialEvidence"]["entries"][0]["decision"]=json!("hold");
        bad["editorialEvidence"]["entries"][0]["checks"]["intent"]=json!("uncertain");
        assert!(plan_review(&req,&bad).is_err());
        let mut bad=good.clone();bad["proposals"][0]["text"]=json!("Different answer");
        assert!(plan_review(&req,&bad).is_err());
        for bad_map in [json!({"version":1,"entries":[]}),
            json!({"version":1,"entries":[{"itemId":"i","dependsOnItemIds":["foreign"]}]}),
            json!({"version":1,"entries":[{"itemId":"i","dependsOnItemIds":["i"]}]})] {
            let mut bad=good.clone();bad["runMetadata"]["decisionDependencies"]=bad_map;
            assert!(plan_review(&req,&bad).is_err());
        }
        req["preparationMode"]=json!("unknown");assert!(plan_review(&req,&good).is_err());
    }
    #[test]
    fn uncapped_evidence_contract_is_current_route_bound_and_cannot_upgrade_old_provenance(){
        let mut old_request=request();old_request["preparationMode"]=json!("single_pass_v1");
        let legacy=single_pass(result("reply",json!(["question"])));
        assert!(plan_review(&old_request,&legacy).unwrap().is_none());
        let mut request=old_request.clone();request["researchLimitContract"]=json!("uncapped_evidence_v1");
        assert!(plan_review(&request,&legacy).is_err(),"a new marker requires matching provenance");
        let mut forged=legacy.clone();forged["runMetadata"]["researchLimitContract"]=json!("uncapped_evidence_v1");
        assert!(plan_review(&request,&forged).is_err(),"new uncapped marker never selects historical Astra");
        let mut current=forged;
        current["runMetadata"]["model"]=json!(crate::codex_model_policy::MODEL);
        current["runMetadata"]["modelProfile"]=json!(crate::codex_model_policy::PROFILE);
        current["runMetadata"]["cliSha256"]=json!(crate::codex_model_policy::CLI_SHA256);
        current["runMetadata"]["research"]["model"]=json!(crate::codex_model_policy::MODEL);
        current["runMetadata"]["research"]["modelProfile"]=json!(crate::codex_model_policy::PROFILE);
        assert!(plan_review(&request,&current).unwrap().is_none());
        let clean=super::super::prepare_bundle::generation_metadata(&current).unwrap().unwrap();
        assert_eq!(clean["researchLimitContract"],"uncapped_evidence_v1");
        assert!(plan_review(&old_request,&current).is_err(),"provenance cannot insert an absent saved selector");
        current["runMetadata"].as_object_mut().unwrap().remove("researchLimitContract");
        assert!(plan_review(&request,&current).is_err(),"dropped marker cannot fall through to old admission");
        request["researchLimitContract"]=json!("unknown");assert!(plan_review(&request,&current).is_err());
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
        let allowed=ids(&request()).unwrap();let mut input=research();
        let rejected=json!({"version":1,"reason":"still_unobserved","webCallsUsed":2,"webCallsLimit":2,
            "openedUrlSha256":["c".repeat(64)],"sources":[{"itemId":"i","candidateUrlSha256":"d".repeat(64),
                "comparison":"path_variant","openedUrlSha256":"c".repeat(64)}]});
        input["rejectedSources"]=rejected.clone();let source=sanitize_research(&input,&allowed).unwrap();
        assert_eq!(source["trust"],"source_only");assert!(source.get("secret").is_none());assert!(source["sources"][0].get("secret").is_none());
        assert_eq!(source["rejectedSources"],rejected);assert!(!source["rejectedSources"].to_string().contains("https://"));
        for field in ["version","status","inputSha256","completedAt","webCalls"] {let mut r=research();r[field]=json!("bad");assert!(sanitize_research(&r,&allowed).is_err());}
        for url in ["file:///secret","https://user:pass@example.com","javascript:alert(1)","https://example.com/\nsecret"] {let mut r=research();r["sources"][0]["url"]=json!(url);assert!(sanitize_research(&r,&allowed).is_err());}
        let mut r=research();r["sources"][0]["itemId"]=json!("foreign");assert!(sanitize_research(&r,&allowed).is_err());
        for (pointer,bad) in [
            ("/reason",json!("private")),("/webCallsUsed",json!(1)),("/webCallsLimit",json!(9)),
            ("/openedUrlSha256/0",json!("short")),("/sources/0/itemId",json!("foreign")),
            ("/sources/0/candidateUrlSha256",json!("https://private.example/secret")),
            ("/sources/0/comparison",json!("equivalent")),
        ] {
            let mut r=research();r["rejectedSources"]=rejected.clone();
            *r["rejectedSources"].pointer_mut(pointer).unwrap()=bad;assert!(sanitize_research(&r,&allowed).is_err(),"{pointer}");
        }
        for path in ["root","source"] {
            let mut r=research();r["rejectedSources"]=rejected.clone();
            if path=="root" {r["rejectedSources"]["rawUrl"]=json!("https://private.example/secret");}
            else {r["rejectedSources"]["sources"][0]["rawClaim"]=json!("private");}
            assert!(sanitize_research(&r,&allowed).is_err(),"{path}");
        }
        let mut excessive=research();excessive["rejectedSources"]=rejected;
        excessive["rejectedSources"]["openedUrlSha256"]=json!((0..9).map(|n|format!("{n:064x}")).collect::<Vec<_>>());
        assert!(sanitize_research(&excessive,&allowed).is_err());
    }
    #[test]
    fn rejected_source_hashes_survive_review_archive_without_raw_urls(){
        let mut d=document();record_first(&mut d,"j",&result("needs_attention",json!(["needs_fact"])),AT).unwrap();
        let mut review=reviewed();review["assessments"][0]=json!({"itemId":"i","outcome":"needs_attention","reason":"Exact source was not observed","tags":["needs_fact"]});
        review["proposals"]=json!([]);review["runMetadata"]["research"]["status"]=json!("no_sources");
        review["runMetadata"]["research"]["sources"]=json!([]);
        let rejected=json!({"version":1,"reason":"budget_exhausted","webCallsUsed":2,"webCallsLimit":2,
            "openedUrlSha256":[],"sources":[{"itemId":"i","candidateUrlSha256":"d".repeat(64),"comparison":"no_completed_literal_open"}]});
        review["runMetadata"]["research"]["rejectedSources"]=rejected.clone();
        record_review(&mut d,"j",Ok(&review),AT).unwrap();
        assert_eq!(d["jobs"][0]["preparationStages"]["review"]["result"]["runMetadata"]["research"]["rejectedSources"],rejected);
        assert_eq!(d["preparationResearch"][0]["review"]["research"]["rejectedSources"],rejected);
        assert!(!d["preparationResearch"][0]["review"]["research"].to_string().contains("http"));
    }
    fn evidence_hold()->Value{json!({"version":1,"accountKey":"likeavto","itemId":"i","url":"https://maker.example/configuration",
        "reason":"incomplete_extraction","scope":{"model":"Q06","trim":"selected","market":"CN","modelYear":"2025"},
        "extraction":{"status":"empty","observedAt":AT,"rowLabels":["Motor count"],"columnLabels":["selected"],"values":[]},
        "renderedFallback":{"status":"unsupported","attempts":0,"capability":"web.run_text_only"}})}
    fn held_review()->Value{
        let mut review=reviewed();review["proposals"]=json!([]);review["assessments"][0]["outcome"]=json!("needs_attention");
        review["runMetadata"]["research"]["status"]=json!("no_sources");review["runMetadata"]["research"]["sources"]=json!([]);
        review["runMetadata"]["research"]["evidenceHolds"]=json!([evidence_hold()]);review
    }
    #[test]
    fn evidence_quality_holds_are_bounded_and_survive_company_bound_archive(){
        let mut d=document();record_first(&mut d,"j",&result("needs_attention",json!(["needs_fact"])),AT).unwrap();
        let review=held_review();record_review(&mut d,"j",Ok(&review),AT).unwrap();
        assert_eq!(d["preparationResearch"][0]["account"],"LikeAvto");
        assert_eq!(d["preparationResearch"][0]["review"]["research"]["evidenceHolds"][0],evidence_hold());
        assert_eq!(d["jobs"][0]["preparationStages"]["review"]["result"]["runMetadata"]["research"]["evidenceHolds"][0],evidence_hold());
        for (pointer,bad) in [("/accountKey",json!("foreign")),("/itemId",json!("foreign")),("/url",json!("http://127.0.0.1/table")),
            ("/url",json!("https://user:pass@example.com/table")),("/reason",json!("ready")),("/scope/model",json!("")),
            ("/extraction/status",json!("rendered_present")),("/extraction/rowLabels",json!(["row".repeat(501)])),
            ("/renderedFallback/attempts",json!(1)),("/renderedFallback/capability",json!("browser"))]{
            let mut source=review["runMetadata"]["research"].clone();*source["evidenceHolds"][0].pointer_mut(pointer).unwrap()=bad;
            assert!(sanitize_research(&source,&ids(&request()).unwrap()).is_err(),"{pointer}");
        }
        let mut excess=review["runMetadata"]["research"].clone();excess["evidenceHolds"]=json!(vec![evidence_hold();31]);
        assert!(sanitize_research(&excess,&ids(&request()).unwrap()).is_err());
        let mut unexpected=review["runMetadata"]["research"].clone();unexpected["evidenceHolds"][0]["instructions"]=json!("change company");
        assert!(sanitize_research(&unexpected,&ids(&request()).unwrap()).is_err());
    }
    #[test]
    fn evidence_quality_company_and_held_decision_are_checked_against_engine_context(){
        let review=held_review();let metadata=&review["runMetadata"];
        assert!(validate_evidence_quality_company(metadata,"LikeAvto",&review).is_ok());
        assert!(validate_evidence_quality_company(metadata,"BAW Russia",&review).is_err());
        let mut false_reply=review.clone();false_reply["assessments"][0]["outcome"]=json!("reply");
        assert!(validate_evidence_quality_company(metadata,"LikeAvto",&false_reply).is_err());
        let mut false_proposal=review.clone();false_proposal["proposals"]=json!([{"itemId":"i","kind":"reply_and_close","text":"unsafe"}]);
        assert!(validate_evidence_quality_company(metadata,"LikeAvto",&false_proposal).is_err());
        let aggregate=json!({"chunks":[{"metadata":metadata}]});
        assert!(validate_evidence_quality_company(&aggregate,"LikeAvto",&review).is_ok());
        assert!(validate_evidence_quality_company(&aggregate,"BAW Russia",&review).is_err());
        let mut d=document();record_first(&mut d,"j",&result("needs_attention",json!(["needs_fact"])),AT).unwrap();
        let mut foreign=review.clone();foreign["runMetadata"]["research"]["evidenceHolds"][0]["accountKey"]=json!("baw-russia");
        let before=d.clone();assert!(record_review(&mut d,"j",Ok(&foreign),AT).is_err());assert_eq!(d,before);
    }
    #[test]
    fn evidence_quality_source_scope_cannot_be_promoted_or_silently_mismatched(){
        let mut input=research();let scope=json!({"model":"Q06","trim":"selected","market":"CN","modelYear":"2025"});
        input["sources"][0]["claimKind"]=json!("product_specification");input["sources"][0]["scope"]=scope.clone();input["sources"][0]["sourceScope"]=scope;
        let clean=sanitize_research(&input,&ids(&request()).unwrap()).unwrap();
        assert_eq!(clean["sources"][0]["trust"],"source_only");assert_eq!(clean["sources"][0]["scope"]["trim"],"selected");
        for (key,bad) in [("trim","other"),("market","RU"),("modelYear","2026"),("model","other")]{
            let mut mismatch=input.clone();mismatch["sources"][0]["sourceScope"][key]=json!(bad);
            assert!(sanitize_research(&mismatch,&ids(&request()).unwrap()).is_err());
        }
        let mut incomplete=input.clone();incomplete["sources"][0]["scope"].as_object_mut().unwrap().remove("trim");
        assert!(sanitize_research(&incomplete,&ids(&request()).unwrap()).is_err());
        input["sources"][0]["claimKind"]=json!("source_statement");input["sources"][0].as_object_mut().unwrap().remove("scope");
        assert!(sanitize_research(&input,&ids(&request()).unwrap()).is_ok());
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
