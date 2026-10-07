//! Semantic review of exact final decisions. No publishing, text rewriting or
//! typography matching happens here; the model owns editorial judgment.
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use crate::prepare_bundle::EvidenceContext;

pub(crate) const CONTRACT: &str = "communityhero-editorial-v1";
pub(crate) const OPERATOR_EVIDENCE_CONTRACT:&str="communityhero-operator-decision-evidence-v1";
pub(crate) const MODEL_PROFILE: &str = "sol61_high_v2";
const LEGACY_MODEL_PROFILE: &str = "sol_high_v1";
const MAX_BYTES: usize = 500_000;
const MAX_REFS: usize = 100;

fn rows<'a>(v: &'a Value, k: &str) -> &'a [Value] {
    v[k].as_array().map(Vec::as_slice).unwrap_or(&[])
}
fn hash(v: &Value) -> String { hash_text(&v.to_string()) }
pub(crate) fn hash_text(text: &str) -> String {
    format!("{:x}", Sha256::digest(text.as_bytes()))
}
fn text<'a>(v: &'a Value, k: &str, max: usize) -> Result<&'a str, &'static str> {
    v[k].as_str().filter(|s| !s.is_empty() && s.len() <= max)
        .ok_or("Invalid editorial identity or text")
}
fn exact(v: &Value, fields: &[&str]) -> bool {
    v.as_object().is_some_and(|o| o.len() == fields.len() && fields.iter().all(|k| o.contains_key(*k)))
}
fn available(d: &Value, p: &Value) -> Result<(), &'static str> {
    available_recipient(d,p)?;
    crate::proposal_source_rebind::validate_origin_and_current_target(&EvidenceContext::new(d),p)
        .map_err(|_|"Source rebind origin/current recipient changed")?;
    crate::operator_close::assert_preparation(d,p).map_err(|_|"Editorial preparation scope is reserved")?;
    Ok(())
}
fn available_recipient(d: &Value, p: &Value) -> Result<(), &'static str> {
    if !matches!(p["status"].as_str(), Some("draft" | "approved")) {
        return Err("Editorial proposal is unavailable");
    }
    let binding=crate::active_binding(d).map_err(|_|"Editorial company binding invalid")?;
    let item=crate::row(d,"items",text(p,"itemId",256)?).map_err(|_|"Editorial recipient missing")?;
    let bound=crate::bound_item(&binding,item).map_err(|_|"Editorial recipient binding invalid")?;
    if rows(d,"operations").iter().any(|op|crate::recipient_operation_blocks(op,p,&bound)) {
        return Err("Editorial recipient has an admitted operation");
    }
    Ok(())
}
fn proposal<'a>(d: &'a Value, id: &str) -> Result<&'a Value, &'static str> {
    rows(d,"proposals").iter().find(|p| p["id"] == id).ok_or("Editorial proposal missing")
}
fn rule_manifest(evidence: &Value) -> Value {
    let mut rules: Vec<Value> = rows(evidence,"knowledgeManifest").iter()
        .filter(|p| matches!(p["kind"].as_str(),Some("rule"|"policy"))).cloned().collect();
    rules.sort_by_key(|p| p.to_string());
    json!(rules)
}
fn candidate(context: &EvidenceContext<'_>, p: &Value, frozen_pins: Option<&Value>) -> Result<(Value,Value), &'static str> {
    let id=text(p,"id",256)?;
    let item=text(p,"itemId",256)?;
    let revision=p["revision"].as_u64().filter(|r| *r > 0).ok_or("Invalid editorial proposal revision")?;
    let kind=text(p,"kind",40)?;
    if !["reply_and_close","close","hide","delete"].contains(&kind) {return Err("Invalid editorial action");}
    let body=p["text"].as_str().filter(|s|s.encode_utf16().count()<=12000).ok_or("Invalid editorial proposal text")?;
    if (kind=="reply_and_close" && body.trim().is_empty()) || (kind!="reply_and_close" && !body.is_empty()) {
        return Err("Editorial action and text disagree");
    }
    let mut evidence=context.evidence_for_item(item)?;
    let frames=rows(&p["modelMaterialReceipt"]["body"],"optionalFrameRefs").iter().filter(|reference|rows(&evidence,"posts").iter().any(|post|post["id"]==reference["postId"])).cloned().collect::<Vec<_>>();
    if !frames.is_empty(){evidence["optionalFrameRefs"]=json!(frames);}
    // Public research remains qualified source evidence, selected by the same
    // validated account/post cache as preparation; arbitrary p.sources is ignored.
    let manifest=if let Some(manifest)=frozen_pins {
        let mut ids=vec![json!(item)];
        for pin in manifest.as_array().into_iter().flatten() {for id in rows(pin,"itemIds") {if !ids.contains(id) {ids.push(id.clone());}}}
        // Pin validation uses original selection time; cache expiry or new
        // research never silently replaces the reviewed source.
        crate::research_cache::current(context.workspace(),manifest,&ids,&crate::now())?;
        manifest.clone()
    } else {
        let cached=crate::research_cache::select(context.workspace(),rows(&evidence,"items"),rows(&evidence,"posts"),&crate::now())?;
        evidence["materials"].as_array_mut().unwrap().extend_from_slice(rows(&cached,"materials"));
        cached["manifest"].clone()
    };
    evidence["editorialResearchManifest"]=manifest.clone();
    let mut selected=json!({"proposalId":id,"proposalRevision":revision,"itemId":item,"kind":kind,"text":body,
        "textSha256":hash_text(body),"contextDigest":hash(&json!({"sourceContextDigest":context.review_fingerprint(item)?,"researchManifest":manifest})),
        "rulesDigest":hash(&rule_manifest(&evidence))});
    if crate::decision_media::enabled(p){
        crate::decision_media::attach_request_with_context(context,&mut evidence)?;
        selected["decisionMediaContract"]=json!(crate::decision_media::CONTRACT);
        selected["decisionMediaEvidence"]=crate::decision_media::capture(context,crate::row(context.workspace(),"items",item).map_err(|_|"Editorial item missing")?,&evidence)?;
        selected["contextDigest"]=json!(hash(&json!({"sourceContextDigest":context.review_fingerprint(item)?,"researchManifest":manifest,
            "decisionMediaEvidence":selected["decisionMediaEvidence"]})));
    }
    Ok((selected,evidence))
}
fn empty_request(d: &Value, new_profile:bool) -> Value {
    let mut request=json!({"account":d["account"],"connectorBinding":d["connectorBinding"],"purpose":"editorial_review",
        "instruction":"Review these exact final decisions under their active company rules and supplied evidence. Never publish or silently rewrite them.",
        "items":[],"branches":[],"posts":[],"materials":[],"knowledgeManifest":[],"knowledgePolicyVersion":1,
        "customerCases":[],"editorialCandidates":[],"editorialResearchPins":[]});
    if new_profile {
        request["editorialModelProfile"]=json!(MODEL_PROFILE);
        request["visualNeedContract"]=json!(crate::prepare_bundle::visual::CONTRACT);
        request["visualSelection"]=crate::prepare_bundle::visual::empty();
    }
    request
}
fn merge_request(request: &mut Value, evidence: &Value, selected: &Value) -> Result<(), &'static str> {
    // A new requirement capture must never upgrade legacy evidence or receipts.
    // Keep each wire request on one contract, also when its posts are disjoint.
    if !rows(request,"editorialCandidates").is_empty()
        &&crate::decision_media::enabled(request)!=crate::decision_media::enabled(selected){
        return Err("Editorial decision media contracts differ");
    }
    for name in ["items","branches","posts","materials","knowledgeManifest","customerCases"] {
        for row in rows(evidence,name) {
            if name=="materials" && row["kind"]=="research" {
                // A qualified cached excerpt is shared only when every source,
                // scope and claim field agrees. Recipient aliases differ, so
                // combine their exact allowed recipients without duplicating it.
                let equivalent=|source:&Value| {
                    let mut key=source.clone();
                    if let Some(o)=key.as_object_mut() {o.remove("id");o.remove("itemIds");}
                    key
                };
                if let Some(index)=rows(request,name).iter().position(|old|old["kind"]=="research" && equivalent(old)==equivalent(row)) {
                    let old=&mut request[name][index];
                    for id in rows(row,"itemIds") {
                        if !rows(old,"itemIds").contains(id) {old["itemIds"].as_array_mut().unwrap().push(id.clone());}
                    }
                    continue;
                }
            }
            let identity=if name=="knowledgeManifest" {json!([row["entryId"],row["versionId"]])}
                else if name=="customerCases" {row["itemId"].clone()}
                else {row["id"].clone()};
            if let Some(old)=rows(request,name).iter().find(|old| {
                let key=if name=="knowledgeManifest" {json!([old["entryId"],old["versionId"]])}
                    else if name=="customerCases" {old["itemId"].clone()}
                    else {old["id"].clone()}; key==identity
            }) {if old!=row {return Err("Conflicting editorial context identities");}}
            else {request[name].as_array_mut().unwrap().push(row.clone());}
        }
    }
    if request.get("visualSelection").is_some() {
        request["visualSelection"]=crate::prepare_bundle::visual::merge(&request["visualSelection"],&evidence["editorialVisualSelection"])?;
        crate::prepare_bundle::visual::request_selection(request)?;
    }
    if evidence["optionalFrameRefs"].is_array(){
        if request.get("optionalFrameRefs").is_none(){request["optionalFrameRefs"]=json!([]);}
        for reference in rows(evidence,"optionalFrameRefs"){if !rows(request,"optionalFrameRefs").contains(reference){request["optionalFrameRefs"].as_array_mut().unwrap().push(reference.clone());}}
    }
    request["editorialCandidates"].as_array_mut().unwrap().push(selected.clone());
    if crate::decision_media::enabled(selected){request["decisionMediaContract"]=json!(crate::decision_media::CONTRACT);}
    request["editorialResearchPins"].as_array_mut().unwrap().push(json!({"proposalId":selected["proposalId"],"manifest":evidence["editorialResearchManifest"]}));
    Ok(())
}
fn batch(request: Value, index: usize) -> Value {
    json!({"version":1,"contract":CONTRACT,"id":format!("editorial-batch-{}",index),"digest":hash(&request),"request":request})
}
fn attach_new_materials(d:&Value,request:&mut Value)->Result<(),&'static str>{
    let ids=rows(request,"items").iter().map(|i|i["id"].clone()).collect::<Vec<_>>();
    request["strictGroupContract"]=json!(crate::preparation_unit::CONTRACT);
    request["strictGroup"]=crate::preparation_unit::capture(d,&ids,&crate::now())?;
    crate::preparation_materials::attach_request(d,request)?;
    crate::preparation_materials::require_request(d,request)
}

/// Pure bounded planner. Common posts/rules/media are included once per batch.
/// Invalid individual references are held explicitly; company failure is global.
pub(crate) fn plan(d: &Value, references: &Value, _at: &str) -> Result<Value, &'static str> {
    plan_profile(d,references,false,false)
}
/// Only new endpoint admissions select Sol. Historical batches retain the
/// absence of this field, their digest and the original serialized lane.
pub(crate) fn plan_new(d:&Value,references:&Value,_at:&str)->Result<Value,&'static str>{
    plan_profile(d,references,true,false)
}
/// A captured independent invocation must never inherit another judgment.
pub(crate) fn plan_fresh(d:&Value,references:&Value,_at:&str)->Result<Value,&'static str>{
    plan_profile(d,references,true,true)
}
pub(crate) fn independent_lane(request:&Value)->Result<bool,&'static str>{
    match request.get("editorialModelProfile") {
        None=>Ok(false),
        Some(profile) if (profile==MODEL_PROFILE||profile==LEGACY_MODEL_PROFILE) && request["purpose"]=="editorial_review"=>Ok(true),
        _=>Err("Unsupported editorial model profile"),
    }
}
fn plan_profile(d:&Value,references:&Value,new_profile:bool,fresh:bool)->Result<Value,&'static str>{
    crate::active_binding(d).map_err(|_|"Editorial company binding invalid")?;
    let refs=references.as_array().filter(|r| !r.is_empty() && r.len()<=MAX_REFS)
        .ok_or("Choose 1 to 100 editorial references")?;
    let mut seen=BTreeSet::new();
    for r in refs {
        if !exact(r,&["id","revision"]) || r["revision"].as_u64().is_none_or(|n|n==0)
            || !seen.insert(text(r,"id",256)?.to_owned()) {return Err("Invalid or duplicate editorial reference");}
    }
    let context=EvidenceContext::new(d);
    let mut pending=empty_request(d,new_profile);let mut batches=Vec::new();let mut held=Vec::new();let mut reused=Vec::new();let mut not_required=Vec::new();
    for r in refs {
        let selected=(|| {
            let p=proposal(d,text(r,"id",256)?)?;
            if p["revision"]!=r["revision"] {return Err("Editorial proposal revision changed");}
            available(d,p)?;
            let item=crate::row(d,"items",text(p,"itemId",256)?).map_err(|_|"Editorial recipient missing")?;
            if item["revision"]!=p["itemRevision"] || item["contextEvidenceDigest"]!=p["contextEvidenceDigest"]
                || item["branchContextDigest"]!=p["branchContextDigest"] {
                return Err("Comment context changed; create a new proposal");
            }
            if item["workflow"]=="waiting" {return Err("Editorial recipient has an operator hold");}
            if fresh {
                current_repair_route(d,p,&context)?;
                return candidate(&context,p,None).map(Some);
            }
            // Current non-reply actions remain exempt. A stale action can be
            // reviewed against current evidence without regenerating it, but
            // never spend review work on an invalid route or operator hold.
            if p["kind"]!="reply_and_close" {
                if crate::proposal_current_with_context(p,&context).is_ok() {
                    candidate(&context,p,None)?;
                    return Ok(Some((Value::Null,Value::Null)));
                }
                let binding=crate::active_binding(d).map_err(|_|"Editorial company binding invalid")?;
                let bound=crate::bound_item(&binding,item).map_err(|_|"Editorial recipient binding invalid")?;
                crate::validate_route(p,&binding,&bound).map_err(|_|"Editorial action route changed")?;
                if matches!(item["workflow"].as_str(),Some("waiting"|"closed")) {
                    return Err("Editorial recipient has an operator hold or is closed");
                }
                if !crate::decision_media::enabled(p)&&!context.video_ready(item)? {return Err("Editorial recipient video evidence is incomplete");}
                return candidate(&context,p,None).map(Some);
            }
            if require_current(&context,p).is_ok() && crate::proposal_current_with_context(p,&context).is_ok() {return Ok(None);}
            candidate(&context,p,None).map(Some)
        })();
        let (c,mut e)=match selected {
            Ok(None)=>{reused.push(r.clone());continue;},
            Ok(Some((c,_))) if c.is_null()=>{not_required.push(r.clone());continue;},
            Ok(Some(v))=>v,
            Err(reason)=>{held.push(json!({"reference":r,"reason":reason}));continue;}
        };
        if new_profile {
            let p=proposal(d,text(r,"id",256)?)?;
            match crate::prepare_bundle::visual::editorial_selection(p,&e) {
                Ok(selection)=>e["editorialVisualSelection"]=selection,
                Err(reason)=>{held.push(json!({"reference":r,"reason":reason}));continue;}
            }
        }
        let mut merged=pending.clone();
        if merge_request(&mut merged,&e,&c).is_err() ||new_profile&&attach_new_materials(d,&mut merged).is_err() {
            // A shared source can have recipient-specific attribution. Split its
            // requests rather than erasing a binding or holding an unrelated item.
            if !rows(&pending,"editorialCandidates").is_empty() {batches.push(batch(pending,batches.len()+1));}
            pending=empty_request(d,new_profile);merged=pending.clone();merge_request(&mut merged,&e,&c)?;
            if new_profile{if let Err(reason)=attach_new_materials(d,&mut merged){held.push(json!({"reference":r,"reason":reason}));continue;}}
        }
        if merged.to_string().len()>MAX_BYTES || rows(&merged,"materials").len()>300
            || rows(&merged,"knowledgeManifest").len()>300 || rows(&merged,"items").len()>MAX_REFS
            || crate::engine_prepare::image_count(&merged)>crate::engine_prepare::MAX_REQUEST_IMAGES {
            if !rows(&pending,"editorialCandidates").is_empty() {batches.push(batch(pending,batches.len()+1));}
            pending=empty_request(d,new_profile);merge_request(&mut pending,&e,&c)?;
            if new_profile{if let Err(reason)=attach_new_materials(d,&mut pending){held.push(json!({"reference":r,"reason":reason}));pending=empty_request(d,new_profile);continue;}}
            if pending.to_string().len()>MAX_BYTES || rows(&pending,"materials").len()>300
                || rows(&pending,"knowledgeManifest").len()>300
                || crate::engine_prepare::image_count(&pending)>crate::engine_prepare::MAX_REQUEST_IMAGES {
                held.push(json!({"reference":r,"reason":"Single editorial context exceeds bounded input"}));pending=empty_request(d,new_profile);
            }
        } else {pending=merged;}
    }
    if !rows(&pending,"editorialCandidates").is_empty() {batches.push(batch(pending,batches.len()+1));}
    let mut plan=json!({"version":1,"contract":CONTRACT,"account":d["account"],"connectorBinding":d["connectorBinding"],
        "batches":batches,"held":held,"reused":reused,"notRequired":not_required});
    if fresh {plan["fresh"]=json!(true);}
    Ok(plan)
}

fn current_repair_route(d:&Value,p:&Value,context:&EvidenceContext<'_>)->Result<(),&'static str>{
    available(d,p)?;
    let binding=crate::active_binding(d).map_err(|_|"Editorial company binding invalid")?;
    let item=crate::bound_item(&binding,crate::row(d,"items",text(p,"itemId",256)?).map_err(|_|"Editorial recipient missing")?)
        .map_err(|_|"Editorial recipient binding invalid")?;
    if item["revision"]!=p["itemRevision"]||item["contextEvidenceDigest"]!=p["contextEvidenceDigest"]
        ||item["branchContextDigest"]!=p["branchContextDigest"]||matches!(item["workflow"].as_str(),Some("waiting"|"closed")) {
        return Err("Editorial recipient changed or held");
    }
    crate::validate_route(p,&binding,&item).map_err(|_|"Editorial action route changed")?;
    if !crate::decision_media::enabled(p)&&!context.video_ready(&item)? {return Err("Editorial recipient video evidence is incomplete");}
    Ok(())
}

/// Revalidate a durable revise receipt against the current exact old decision.
/// This accepts no new text or authority from the caller.
pub(crate) fn repair_candidate(d:&Value,p:&Value,saved:&Value)->Result<Value,&'static str>{
    let mut unsigned=saved.clone();unsigned.as_object_mut().ok_or("Editorial revise receipt missing")?.remove("receiptSha256");
    if saved["version"]!=1||saved["contract"]!=CONTRACT||saved["account"]!=d["account"]
        ||saved["connectorBinding"]!=d["connectorBinding"]||saved["receiptSha256"]!=hash(&unsigned)
        ||saved["decision"]!="revise"||saved["source"]["kind"]!="dedicated_model_review" {
        return Err("Editorial repair requires a dedicated exact revise receipt");
    }
    let context=EvidenceContext::new(d);current_repair_route(d,p,&context)?;
    let (current,_)=candidate(&context,p,Some(&saved["source"]["researchManifest"]))?;
    if saved["candidate"]!=current {return Err("Editorial repair context or candidate changed");}
    validate_decision(saved,current["kind"].as_str().unwrap_or(""),current["text"].as_str().unwrap_or(""))?;
    Ok(current)
}

fn verdict(value: &Value, c: &Value) -> Result<Value, &'static str> {
    let mut fields=vec!["proposalId","proposalRevision","itemId","textSha256","contextDigest","rulesDigest","decision","reason","proposedText","checks"];
    if value.get("mediaDependency").is_some(){fields.push("mediaDependency");}
    if !exact(value,&fields) {
        return Err("Invalid editorial verdict fields");
    }
    for k in ["proposalId","proposalRevision","itemId","textSha256","contextDigest","rulesDigest"] {
        if value[k]!=c[k] {return Err("Editorial verdict candidate binding mismatch");}
    }
    validate_decision(value,c["kind"].as_str().unwrap_or(""),c["text"].as_str().unwrap_or(""))?;
    if crate::decision_media::enabled(c){crate::decision_media::validate_dependency(&value["mediaDependency"])?;}
    Ok(value.clone())
}
fn validate_decision(value: &Value, kind: &str, body: &str) -> Result<(), &'static str> {
    if let Some(needs)=value.get("mediaDependency"){crate::decision_media::validate_dependency(needs)?;}
    let checks=&value["checks"];
    if !exact(checks,&["companyRules","intent","factualScope"]) || ["companyRules","intent","factualScope"].iter()
        .any(|k|!matches!(checks[k].as_str(),Some("pass"|"fail"|"uncertain"))) {return Err("Invalid editorial semantic checks");}
    if text(value,"reason",2000)?.trim().is_empty() {return Err("Editorial reason missing");}
    let pass=["companyRules","intent","factualScope"].iter().all(|k|checks[k]=="pass");
    match value["decision"].as_str() {
        Some("accept") if pass && value["proposedText"].is_null()=>Ok(()),
        Some("hold") if !pass && value["proposedText"].is_null()=>Ok(()),
        Some("revise") if !pass && kind=="reply_and_close" && value["proposedText"].as_str()
            .is_some_and(|t|!t.trim().is_empty() && t.encode_utf16().count()<=12000 && t!=body)=>Ok(()),
        _=>Err("Editorial decision and checks disagree")
    }
}

/// Validate adapter assertions against the exact admitted final generation text.
/// A generic model success, hash of another text or an editorial hold is never
/// silently converted into acceptance. Missing proof remains legacy/no proof.
pub(crate) fn generation_evidence(result: &Value) -> Result<Option<Value>, &'static str> {
    let Some(proof)=result.get("editorialEvidence") else {return Ok(None);};
    if !exact(proof,&["version","contract","entries"]) || proof["version"]!=1 || proof["contract"]!=CONTRACT {
        return Err("Invalid generation editorial contract");
    }
    let entries=proof["entries"].as_array().filter(|e|e.len()<=MAX_REFS).ok_or("Invalid generation editorial entries")?;
    let mut seen=BTreeSet::new();
    for entry in entries {
        let mut fields=vec!["itemId","kind","textSha256","decision","reason","checks"];
        if entry.get("mediaDependency").is_some(){fields.push("mediaDependency");}
        if !exact(entry,&fields) {
            return Err("Invalid generation editorial fields");
        }
        let item=text(entry,"itemId",256)?;
        if !seen.insert(item) {return Err("Duplicate generation editorial recipient");}
        let matches:Vec<_>=rows(result,"proposals").iter().filter(|p|p["itemId"]==item && p["kind"]==entry["kind"]
            && p["text"].as_str().is_some_and(|s|entry["textSha256"]==hash_text(s))).collect();
        if matches.len()!=1 {return Err("Generation editorial text binding mismatch");}
        let mut decision=entry.clone();decision["proposedText"]=Value::Null;
        // Non-passing generation judgments remain evidence but cannot be reused
        // as acceptance. A generation revision is advisory, with no final edit.
        if decision["decision"]=="revise" {decision["decision"]=json!("hold");}
        validate_decision(&decision, matches[0]["kind"].as_str().unwrap_or(""),matches[0]["text"].as_str().unwrap_or(""))?;
    }
    Ok(Some(proof.clone()))
}
fn receipt(d: &Value, c: &Value, judgment: &Value, source: &Value, at: &str) -> Value {
    let mut receipt=json!({"version":1,"contract":CONTRACT,"account":d["account"],"connectorBinding":d["connectorBinding"],
        "candidate":c,"decision":judgment["decision"],"reason":judgment["reason"],"proposedText":judgment["proposedText"],
        "checks":judgment["checks"],"source":source,"reviewedAt":at});
    if let Some(needs)=judgment.get("mediaDependency"){receipt["mediaDependency"]=needs.clone();}
    receipt["receiptSha256"]=json!(hash(&receipt));receipt
}
fn store(d: &mut Value, id: &str, receipt: Value) {
    let p=d["proposals"].as_array_mut().unwrap().iter_mut().find(|p|p["id"]==id).unwrap();
    if !p["editorialReviews"].is_array() {p["editorialReviews"]=json!([]);}
    if !rows(p,"editorialReviews").iter().any(|old|old["receiptSha256"]==receipt["receiptSha256"]) {
        p["editorialReviews"].as_array_mut().unwrap().push(receipt.clone());
    }
    p["editorialReview"]=receipt;
}

// Pure projection only: saved receipts must retain their frozen research pins
// and remain valid after approval without re-entering draft admission guards.
pub(crate) fn operator_acquisition_digest(evidence:&Value)->String{
    hash(&json!(rows(evidence,"posts").iter()
        .map(|post|json!({"postId":post["id"],"mediaPolicy":post["mediaPolicy"]})).collect::<Vec<_>>()))
}
fn operator_decision_candidate(context:&EvidenceContext<'_>,p:&Value,pins:Option<&Value>)->Result<(Value,Value),&'static str>{
    let (mut c,e)=candidate(context,p,pins)?;
    if crate::decision_media::enabled(p){
        c["operatorEvidenceContract"]=json!(OPERATOR_EVIDENCE_CONTRACT);
        c["acquisitionMediaDigest"]=json!(operator_acquisition_digest(&e));
        let source=context.evidence_for_item(text(p,"itemId",256)?)?;
        c["contextDigest"]=json!(hash(&json!({"sourceContextDigest":crate::prepare_bundle::operator_decision_source_fingerprint(&source),
            "researchManifest":e["editorialResearchManifest"],"decisionMediaEvidence":c["decisionMediaEvidence"]})));
    }
    Ok((c,e))
}
pub(crate) fn operator_candidates_equal(reviewed:&Value,current:&Value,needs:&Value)->bool{
    if reviewed["operatorEvidenceContract"]!=OPERATOR_EVIDENCE_CONTRACT
        ||current["operatorEvidenceContract"]!=OPERATOR_EVIDENCE_CONTRACT{return reviewed==current;}
    let valid=|v:&Value|v["acquisitionMediaDigest"].as_str().is_some_and(|s|s.len()==64&&s.bytes().all(|b|b.is_ascii_digit()||(b'a'..=b'f').contains(&b)));
    if !valid(reviewed)||!valid(current)||crate::decision_media::validate_dependency(needs).is_err(){return false;}
    if needs["audio"]!="independent"||needs["visual"]!="independent"{return reviewed==current;}
    let mut old=reviewed.clone();let mut now=current.clone();
    if let Some(o)=old.as_object_mut(){o.remove("acquisitionMediaDigest");}else{return false;}
    if let Some(o)=now.as_object_mut(){o.remove("acquisitionMediaDigest");}else{return false;}
    old==now
}
/// Explicit operator-assisted review captures actual source evidence; it never
/// claims that a model reviewed the draft or that a human read it personally.
pub(crate) fn operator_candidate(d:&Value,p:&Value)->Result<(Value,Value),&'static str>{
    operator_candidate_with_context(&EvidenceContext::new(d),p)
}

/// Reuse only within one immutable capture. Never retain this context across
/// receipt writes or later admission, which must reconstruct current evidence.
pub(crate) fn operator_candidate_with_context(context:&EvidenceContext<'_>,p:&Value)->Result<(Value,Value),&'static str>{
    let d=context.workspace();
    available(d,p)?;
    if p["status"]!="draft" || p["kind"]!="reply_and_close"&&!crate::decision_media::enabled(p) {return Err("Operator review requires an exact draft reply");}
    let binding=crate::active_binding(d).map_err(|_|"Operator review company binding invalid")?;
    let item=crate::bound_item(&binding,crate::row(d,"items",text(p,"itemId",256)?).map_err(|_|"Operator review recipient missing")?)
        .map_err(|_|"Operator review recipient binding invalid")?;
    if item["revision"]!=p["itemRevision"] || item["contextEvidenceDigest"]!=p["contextEvidenceDigest"]
        || item["branchContextDigest"]!=p["branchContextDigest"] || matches!(item["workflow"].as_str(),Some("waiting"|"closed")) {
        return Err("Operator review recipient changed or held");
    }
    crate::validate_route(p,&binding,&item).map_err(|_|"Operator review action route changed")?;
    if p["kind"]=="reply_and_close"{crate::reply_constraints::validate_reply(context,&item,text(p,"text",50000)?)?;}
    if !crate::decision_media::enabled(p)&&!context.video_ready(&item)? {return Err("Operator review requires complete video evidence");}
    operator_decision_candidate(context,p,None)
}

pub(crate) fn store_operator_receipt(d:&mut Value,c:&Value,judgment:&Value,source:&Value,at:&str)->Result<Value,&'static str>{
    verdict(judgment,c)?;
    crate::decision_media::validate_judgment(c,judgment)?;
    let saved=receipt(d,c,judgment,source,at);
    store(d,text(c,"proposalId",256)?,saved.clone());
    Ok(saved)
}

/// Rechecks captured recipients immediately before spending model capacity.
/// The caller persists this exact derived dispatch before invoking the bridge.
pub(crate) fn before_call(d:&Value,captured:&Value)->Result<Value,&'static str>{
    let request=&captured["request"];
    if captured["version"]!=1 || captured["contract"]!=CONTRACT || captured["digest"]!=hash(request)
        ||request["purpose"]!="editorial_review"||request["account"]!=d["account"]
        ||request["connectorBinding"]!=d["connectorBinding"] {return Err("Editorial batch binding changed");}
    independent_lane(request)?;
    crate::prepare_bundle::visual::request_selection(request)?;
    let candidates=rows(request,"editorialCandidates");let pins=rows(request,"editorialResearchPins");
    if candidates.is_empty()||candidates.len()>MAX_REFS||pins.len()!=candidates.len(){return Err("Editorial pre-call coverage mismatch");}
    let mut identities=BTreeSet::new();
    for candidate in candidates{let id=text(candidate,"proposalId",256)?;
        if !identities.insert(id)||pins.iter().filter(|p|p["proposalId"]==id&&p["manifest"].is_array()).count()!=1{return Err("Editorial research pin coverage mismatch");}
    }
    if crate::preparation_unit::current_request(d,request,&crate::now()).is_err()
        ||crate::preparation_materials::require_request(d,request).is_err(){
        // Changed native source/group proof is a finite pre-paid hold. The
        // immutable request is never refreshed underneath an existing capture.
        let held=candidates.iter().map(|c|json!({"proposalId":c["proposalId"],"decision":"hold","reason":"Editorial captured context changed before model call","stale":true})).collect::<Vec<_>>();
        let mut capture=json!({"parentBatchId":captured["id"],"parentDigest":captured["digest"],"batch":null,"held":held});
        capture["dispatchDigest"]=json!(hash(&capture));return Ok(capture);
    }
    let context=EvidenceContext::new(d);let mut seen=BTreeSet::new();let mut current=Vec::new();let mut held=Vec::new();
    for c in candidates {
        let id=text(c,"proposalId",256)?;
        if !seen.insert(id){return Err("Duplicate editorial candidate");}
        let matching:Vec<_>=pins.iter().filter(|pin|pin["proposalId"]==id).collect();
        if matching.len()!=1||!matching[0]["manifest"].is_array(){return Err("Editorial research pin coverage mismatch");}
        let manifest=&matching[0]["manifest"];
        let valid=(||{
            crate::research_cache::current(d,manifest,&[c["itemId"].clone()],&crate::now())?;
            let p=proposal(d,id)?;available(d,p)?;
            let item=crate::row(d,"items",text(p,"itemId",256)?).map_err(|_|"Editorial recipient missing")?;
            if item["revision"]!=p["itemRevision"]||item["contextEvidenceDigest"]!=p["contextEvidenceDigest"]
                ||item["branchContextDigest"]!=p["branchContextDigest"]||item["workflow"]=="waiting" {
                return Err("Editorial recipient changed or held");
            }
            let (binding,_)=candidate(&context,p,Some(manifest))?;
            if binding!=*c{return Err("Editorial candidate changed");}Ok(())
        })();
        if valid.is_ok(){current.push(c.clone());}
        else{held.push(json!({"proposalId":id,"decision":"hold","reason":"Editorial candidate changed before model call","stale":true}));}
    }
    // Keep the captured evidence bytes. Filtering decisions never refreshes or
    // invents source material; current recipients passed their exact fingerprints.
    let dispatch=if current.is_empty(){Value::Null}else if held.is_empty(){captured.clone()}else{
        let mut selected=captured.clone();
        selected["request"]["editorialResearchPins"]=json!(pins.iter().filter(|pin|current.iter().any(|c|c["proposalId"]==pin["proposalId"])).collect::<Vec<_>>());
        if request.get("visualSelection").is_some() {
            selected["request"]["visualSelection"]["postImages"]=json!(rows(&request["visualSelection"],"postImages").iter()
                .filter(|row|current.iter().any(|c|c["itemId"]==row["itemId"])).collect::<Vec<_>>());
        }
        selected["request"]["editorialCandidates"]=json!(current);
        selected["digest"]=json!(hash(&selected["request"]));selected
    };
    let mut capture=json!({"parentBatchId":captured["id"],"parentDigest":captured["digest"],"batch":dispatch,"held":held});
    capture["dispatchDigest"]=json!(hash(&capture));Ok(capture)
}

pub(crate) fn validate_dispatch_capture(parent:&Value,capture:&Value)->Result<(),&'static str>{
    let mut unsigned=capture.clone();unsigned.as_object_mut().ok_or("Invalid editorial dispatch capture")?.remove("dispatchDigest");
    if capture["parentBatchId"]!=parent["id"]||capture["parentDigest"]!=parent["digest"]
        ||parent["digest"]!=hash(&parent["request"])||capture["dispatchDigest"]!=hash(&unsigned){return Err("Editorial dispatch binding changed");}
    let candidates=rows(&parent["request"],"editorialCandidates");let held=rows(capture,"held");
    if held.iter().any(|h|h["decision"]!="hold"||h["stale"]!=true||!candidates.iter().any(|c|c["proposalId"]==h["proposalId"])){
        return Err("Editorial pre-call hold binding changed");
    }
    let selected:Vec<_>=candidates.iter().filter(|c|!held.iter().any(|h|h["proposalId"]==c["proposalId"])).cloned().collect();
    if selected.len()+held.len()!=candidates.len(){return Err("Editorial dispatch coverage changed");}
    let expected=if selected.is_empty(){Value::Null}else if held.is_empty(){parent.clone()}else{
        let mut batch=parent.clone();
        batch["request"]["editorialResearchPins"]=json!(rows(&parent["request"],"editorialResearchPins").iter()
            .filter(|pin|selected.iter().any(|c|c["proposalId"]==pin["proposalId"])).collect::<Vec<_>>());
        if parent["request"].get("visualSelection").is_some() {
            batch["request"]["visualSelection"]["postImages"]=json!(rows(&parent["request"]["visualSelection"],"postImages").iter()
                .filter(|row|selected.iter().any(|c|c["itemId"]==row["itemId"])).collect::<Vec<_>>());
        }
        batch["request"]["editorialCandidates"]=json!(selected);batch["digest"]=json!(hash(&batch["request"]));batch
    };
    if capture["batch"]!=expected{return Err("Editorial dispatch differs from captured subset");}Ok(())
}

/// Validates the complete model result before writing receipts. Stale recipients
/// are explicit holds; other independent recipients may retain their reviews.
/// Never changes proposal text, action, revision, approval or operation history.
pub(crate) fn admit(d: &mut Value, batch: &Value, result: &Value, at: &str) -> Result<Value, &'static str> {
    chrono::DateTime::parse_from_rfc3339(at).map_err(|_|"Invalid editorial review timestamp")?;
    if batch["version"]!=1 || batch["contract"]!=CONTRACT || batch["digest"]!=hash(&batch["request"])
        || batch["request"]["purpose"]!="editorial_review" || batch["request"]["account"]!=d["account"]
        || batch["request"]["connectorBinding"]!=d["connectorBinding"] {return Err("Editorial batch binding changed");}
    let candidates=rows(&batch["request"],"editorialCandidates");
    let values=result["editorial"].as_array().filter(|r|r.len()==candidates.len() && !r.is_empty() && r.len()<=MAX_REFS)
        .ok_or("Editorial verdict coverage mismatch")?;
    if result["sources"]!=json!([]) || result["proposals"]!=json!([]) || result["text"].as_str().is_none_or(|s|s.trim().is_empty()) {
        return Err("Invalid editorial model result");
    }
    let metadata=crate::prepare_bundle::generation_metadata(result)?.ok_or("Editorial generation provenance missing")?;
    let material_receipt=crate::model_material_receipt::result_receipt(&batch["request"],result)?;
    let expected_model=if batch["request"]["editorialModelProfile"]==MODEL_PROFILE {crate::codex_model_policy::MODEL}else{"gpt-6-sol"};
    if independent_lane(&batch["request"])? && (metadata["model"]!=expected_model
        ||metadata["reasoningEffort"]!="high"||metadata["promptVersion"]!=CONTRACT) {
        return Err("Editorial model provenance differs from captured profile");
    }
    crate::prepare_bundle::validate_image_evidence_binding(&metadata,&json!({"request":batch["request"]}))?;
    let mut seen=BTreeSet::new();let mut judged=Vec::new();
    for value in values {
        let id=text(value,"proposalId",256)?;
        if !seen.insert(id) {return Err("Duplicate editorial verdict");}
        let c=candidates.iter().find(|c|c["proposalId"]==id).ok_or("Foreign editorial verdict")?;
        judged.push((c.clone(),verdict(value,c)?));
    }
    let context=EvidenceContext::new(d);let mut writes=Vec::new();let mut outcomes=Vec::new();
    for (c,j) in judged {
        let id=c["proposalId"].as_str().unwrap();
        let pins:Vec<_>=rows(&batch["request"],"editorialResearchPins").iter().filter(|pin|pin["proposalId"]==id).collect();
        if pins.len()!=1 {return Err("Editorial research pin coverage mismatch");}
        let manifest=&pins[0]["manifest"];
        if crate::research_cache::current(d,manifest,&[c["itemId"].clone()],&crate::now()).is_err() {
            outcomes.push(json!({"proposalId":id,"decision":"hold","reason":"Pinned editorial research changed","stale":true}));continue;
        }
        let current=proposal(d,id).and_then(|p|{available(d,p)?;candidate(&context,p,Some(manifest)).map(|(binding,_)|binding)});
        if current.as_ref().is_err() || current.as_ref().is_ok_and(|now|now!=&c) {
            outcomes.push(json!({"proposalId":id,"decision":"hold","reason":"Editorial candidate changed or admitted operation exists","stale":true}));
            continue;
        }
        if let Err(reason)=crate::decision_media::validate_judgment(&c,&j){
            outcomes.push(json!({"proposalId":id,"decision":"hold","reason":reason}));continue;
        }
        let saved=receipt(d,&c,&j,&json!({"kind":"dedicated_model_review","batchId":batch["id"],"batchDigest":batch["digest"],
            "resultSha256":hash(result),"runMetadata":metadata,"researchManifest":manifest}),at);
        outcomes.push(json!({"proposalId":id,"decision":j["decision"],"reason":j["reason"],"proposedText":j["proposedText"],"receiptSha256":saved["receiptSha256"]}));
        writes.push((id.to_owned(),saved));
    }
    drop(context);
    for (id,saved) in writes {store(d,&id,saved);if let Some(receipt)=&material_receipt{crate::row_mut(d,"proposals",&id).map_err(|_|"Editorial proposal missing")?["editorialModelMaterialReceipt"]=receipt.clone();}}
    Ok(json!({"version":1,"contract":CONTRACT,"outcomes":outcomes}))
}

/// Apply at NEW approval admission, and dispatch of that marked policy generation.
/// Legacy already-admitted approvals and UNKNOWN reconciliation are not upgraded.
fn current_receipt<'a>(context: &EvidenceContext<'_>, p: &'a Value) -> Result<&'a Value, &'static str> {
    let r=&p["editorialReview"];let d=context.workspace();
    if r["version"]!=1 || r["contract"]!=CONTRACT || r["account"]!=d["account"]
        || r["connectorBinding"]!=d["connectorBinding"] {
        return Err("Exact final decision requires editorial review");
    }
    let mut original=r.clone();original.as_object_mut().ok_or("Invalid editorial receipt")?.remove("receiptSha256");
    if r["receiptSha256"]!=hash(&original) {return Err("Editorial receipt integrity mismatch");}
    let manifest=r["source"].get("researchManifest").ok_or("Editorial research manifest missing")?;
    let operator=r["source"]["kind"]=="operator_assisted_review"
        &&r["candidate"]["operatorEvidenceContract"]==OPERATOR_EVIDENCE_CONTRACT;
    let (current,_)=if operator{operator_decision_candidate(context,p,Some(manifest))?}else{candidate(context,p,Some(manifest))?};
    let same=if operator{operator_candidates_equal(&r["candidate"],&current,&r["mediaDependency"])}else{r["candidate"]==current};
    if !same {return Err("Editorial review is stale after text, context or rule change");}
    validate_decision(r,current["kind"].as_str().unwrap(),current["text"].as_str().unwrap())?;
    Ok(r)
}

/// An independent later operator judgment may replace the active semantic
/// verdict without erasing a real current model delivery observation. Verify
/// that observation against its immutable exact candidate and saved receipt;
/// HOLD is delivery evidence, never semantic permission to publish.
pub(crate) fn material_capture_current(context:&EvidenceContext<'_>,p:&Value,request:&Value,body:&Value)->Result<(),&'static str>{
    for review in std::iter::once(&p["editorialReview"]).chain(rows(p,"editorialReviews")){
        if review["source"]["kind"]!="dedicated_model_review"||review["source"]["batchDigest"]!=hash(request)
            ||review["source"]["runMetadata"]["materialInvocation"]!=*body||!rows(request,"editorialCandidates").contains(&review["candidate"]){continue;}
        let mut captured=p.clone();captured["editorialReview"]=review.clone();
        if current_receipt(context,&captured).is_ok(){return Ok(());}
    }Err("mandatory_editorial_material_candidate_changed")
}

/// Actual acquired images and failed acquisitions remain context evidence even
/// when the exact semantic decision is HOLD. This conveys no acceptance.
/// Admission already validated this metadata against its immutable batch;
/// receipt integrity and the current candidate retain that provenance.
pub(crate) fn current_acquisition_metadata<'a>(context:&EvidenceContext<'_>,p:&'a Value)->Result<&'a Value,&'static str>{
    Ok(&current_receipt(context,p)?["source"]["runMetadata"])
}

pub(crate) fn require_current(context: &EvidenceContext<'_>, p: &Value) -> Result<(), &'static str> {
    if p["editorialReview"]["decision"]!="accept" {return Err("Exact final decision requires editorial review");}
    let r=current_receipt(context,p)?;
    let current=&r["candidate"];
    crate::decision_media::validate_judgment(&current,r)
}

/// All new media-dependent actions, including moderation, retain the same exact
/// semantic receipt at approval and dispatch. Legacy snapshots stay strict.
pub(crate) fn decision_media_current(context:&EvidenceContext<'_>,p:&Value,item:&Value)->Result<bool,&'static str>{
    if crate::decision_media::enabled(p){require_current(context,p)?;Ok(true)}else{context.video_ready(item)}
}

/// A later, dedicated review may authorize the same exact decision against
/// current source evidence. Generation-time judgments retain their original
/// preparation binding and cannot silently refresh it.
pub(crate) fn dedicated_current(context: &EvidenceContext<'_>, p: &Value) -> Result<(), &'static str> {
    require_current(context,p)?;
    if !matches!(p["editorialReview"]["source"]["kind"].as_str(),Some("dedicated_model_review"|"operator_assisted_review")) {
        return Err("Current source requires a dedicated editorial review");
    }
    Ok(())
}

/// Reuse the existing second-pass model's explicit editorial judgments. Older
/// generic review success is not editorial proof. Bind only the reviewed final
/// text; edits cannot inherit this receipt. Caller has admitted generation data.
pub(crate) fn reuse_generation(d: &mut Value, proposal_id: &str, bundle: &Value, result: &Value, at: &str) -> Result<bool, &'static str> {
    Ok(reuse_generation_batch(d,&[proposal_id.to_owned()],bundle,result,at)?.contains(&proposal_id.to_owned()))
}

/// One immutable context/catalog validation for the whole admitted generation,
/// with receipt writes deferred until all candidate checks have completed.
pub(crate) fn reuse_generation_batch(d: &mut Value, proposal_ids: &[String], bundle: &Value, result: &Value, at: &str) -> Result<Vec<String>, &'static str> {
    reuse_generation_batch_in(d,proposal_ids,bundle,result,None,None,at)
}

/// Consume a native capability in this one synchronous batch. It authorizes
/// only reuse of this already paid verdict, never ordinary review or dispatch.
pub(crate) fn reuse_repair_generation(d:&mut Value,proposal_ids:&[String],bundle:&Value,result:&Value,
    permit:crate::preparation_reservations::RepairGenerationPermit,at:&str)->Result<Vec<String>,&'static str>{
    reuse_generation_batch_in(d,proposal_ids,bundle,result,None,Some(permit),at)
}

/// The caller has checked this captured branch group immediately before
/// creating its proposals in the same transaction. Recheck the exact saved
/// dependency vector after local workflow bumps; no other branch may supply
/// editorial authority to these recipients.
pub(crate) fn reuse_generation_scoped(d:&mut Value,proposal_ids:&[String],bundle:&Value,result:&Value,group:&Value,at:&str)->Result<Vec<String>,&'static str>{
    reuse_generation_batch_in(d,proposal_ids,bundle,result,Some(group),None,at)
}

fn reuse_generation_batch_in(d: &mut Value, proposal_ids: &[String], bundle: &Value, result: &Value, group:Option<&Value>,
    repair_permit:Option<crate::preparation_reservations::RepairGenerationPermit>,at: &str) -> Result<Vec<String>, &'static str> {
    let repair_generation=if let Some(permit)=repair_permit{
        permit.validate_created(d,proposal_ids,bundle,result).map_err(|_|"Editorial repair generation permit changed")?;true
    }else{false};
    let Some(proof)=generation_evidence(result)? else {return Ok(vec![]);};
    let metadata=crate::prepare_bundle::generation_metadata(result)?.ok_or("Editorial generation provenance missing")?;
    let context=EvidenceContext::new(d);
    if let Some(group)=group{crate::prepare_bundle::current_group(d,bundle,group)?;}
    else{context.current(bundle)?;}
    let pins=if let Some(group)=group{
        let ids=group["itemIds"].as_array().ok_or("Editorial group recipients missing")?;
        json!(rows(bundle,"researchManifest").iter().filter(|pin|rows(pin,"itemIds").iter().any(|id|ids.contains(id))).collect::<Vec<_>>())
    }else{bundle.get("researchManifest").cloned().unwrap_or(json!([]))};
    let mut writes=Vec::new();let mut accepted=Vec::new();
    for proposal_id in proposal_ids {
    let p=proposal(d,proposal_id)?;
    if repair_generation{available_recipient(d,p)?;}else{available(d,p)?;}
    if p["prepareBundleId"]!=bundle["id"] || p["prepareBundleDigest"]!=bundle["digest"] {return Err("Editorial generation provenance mismatch");}
    let (c,e)=candidate(&context,p,Some(&pins))?;
    let actual=rows(result,"proposals").iter().filter(|v|v["itemId"]==c["itemId"] && v["kind"]==c["kind"] && v["text"]==c["text"]).count();
    let matched:Vec<_>=rows(&proof,"entries").iter().filter(|v|v["itemId"]==c["itemId"] && v["kind"]==c["kind"] && v["textSha256"]==c["textSha256"]).collect();
    if actual!=1 || matched.len()!=1 {continue;}
    // All applicable current policy versions must have been in the reviewed input.
    if rows(&e,"knowledgeManifest").iter().filter(|v|matches!(v["kind"].as_str(),Some("rule"|"policy")))
        .any(|v|!rows(&bundle["request"],"knowledgeManifest").contains(v)) {return Err("Editorial generation rule binding mismatch");}
    let mut j=matched[0].clone();j["proposedText"]=Value::Null;
    if j["decision"]!="accept" {continue;}
    if crate::decision_media::enabled(p){
        if !crate::decision_media::enabled(&bundle["request"]){return Err("Decision media generation capture missing");}
        crate::decision_media::generation_ready_with_context(&context,bundle,result,&json!({"itemId":c["itemId"],"kind":c["kind"],"text":c["text"]}))?;
    }
    validate_decision(&j,c["kind"].as_str().unwrap(),c["text"].as_str().unwrap())?;
    crate::decision_media::validate_judgment(&c,&j)?;
    let saved=receipt(d,&c,&j,&json!({"kind":"reused_generation_review","prepareBundleId":bundle["id"],"prepareBundleDigest":bundle["digest"],
        "resultSha256":hash(result),"runMetadata":metadata,"researchManifest":pins}),at);
    writes.push((proposal_id.clone(),saved));accepted.push(proposal_id.clone());
    }
    drop(context);for (id,saved) in writes {store(d,&id,saved);}Ok(accepted)
}

/// Offline test fixture: submits an explicit synthetic model verdict through
/// the production planner/admission reducer. It does not bypass the gate.
#[cfg(test)]
pub(crate) fn fixture_accept(d: &mut Value, proposal_id: &str) -> Result<(), &'static str> {
    let p=proposal(d,proposal_id)?;
    let planned=plan_new(d,&json!([{"id":p["id"],"revision":p["revision"]}]),"2026-09-27T01:00:00Z")?;
    for b in rows(&planned,"batches") {
        let entries:Vec<_>=rows(&b["request"],"editorialCandidates").iter().map(|c|{let mut entry=json!({
            "proposalId":c["proposalId"],"proposalRevision":c["proposalRevision"],"itemId":c["itemId"],"textSha256":c["textSha256"],
            "contextDigest":c["contextDigest"],"rulesDigest":c["rulesDigest"],"decision":"accept","reason":"Explicit offline synthetic model fixture",
            "proposedText":null,"checks":{"companyRules":"pass","intent":"pass","factualScope":"pass"}});
            if crate::decision_media::enabled(c){entry["mediaDependency"]=json!({"audio":"independent","visual":"independent"});}
            entry}).collect();
        let mut metadata=fixture_metadata();metadata["model"]=json!(crate::codex_model_policy::MODEL);
        metadata["modelProfile"]=json!(crate::codex_model_policy::PROFILE);metadata["reasoningEffort"]=json!("high");metadata["cliSha256"]=json!(crate::codex_model_policy::CLI_SHA256);
        let mut result=json!({"text":"Offline fixture review","sources":[],"proposals":[],"editorial":entries,"runMetadata":metadata});
        fixture_capture_result(d,b,&mut result)?;
        admit(d,b,&result,"2026-09-27T01:00:00Z")?;
    }
    if !rows(&planned,"held").is_empty() {return Err("Editorial fixture cannot waive held references");}
    Ok(())
}

/// Isolated native fixtures retain an actual immutable paid document and use
/// the normal exact dispatch journal/material validator. No model is invoked.
/// Call before a negative-case snapshot; retention is an independent stage.
#[cfg(test)]
pub(crate) fn fixture_capture_result(d:&mut Value,batch:&Value,result:&mut Value)->Result<String,&'static str>{
    if !crate::preparation_materials::enabled(&batch["request"]){return Ok(String::new());}
    let first=crate::list(d,"jobs").len();
    let job=crate::new_job(d,"editorial_review","isolated-editorial-material-fixture").map_err(|_|"Editorial fixture job failed")?;
    let references=rows(&batch["request"],"editorialCandidates").iter().map(|c|json!({"id":c["proposalId"],"revision":c["proposalRevision"]})).collect::<Vec<_>>();
    let stored=crate::row_mut(d,"jobs",&job).map_err(|_|"Editorial fixture job missing")?;
    stored["purpose"]=json!("editorial_review");stored["editorialReferences"]=json!(references);
    stored["editorialPlan"]=json!({"batches":[batch]});stored["editorialBatches"]=json!([]);
    crate::conductor_authority::fence_new_jobs(d,first).map_err(|_|"Editorial fixture conductor authority failed")?;
    let capture=crate::editorial_endpoint::capture_dispatch(d,&job,batch).map_err(|_|"Editorial fixture dispatch failed")?;
    if capture["batch"]!=*batch{return Err("Editorial fixture needs the exact current batch");}
    crate::model_material_receipt::fixture_result(d,&job,&batch["request"],result).map_err(|_|"Editorial fixture paid material capture failed")?;
    let stored=crate::row_mut(d,"jobs",&job).map_err(|_|"Editorial fixture job missing")?;
    stored["status"]=json!("completed");stored["editorialBatches"][0]["state"]=json!("settled");
    Ok(job)
}

#[cfg(test)]
pub(crate) fn fixture_metadata() -> Value {
    json!({"schemaVersion":1,"model":"synthetic-offline-model","reasoningEffort":"medium","promptVersion":CONTRACT,
        "instructionSha256":"a".repeat(64),"inputSha256":"b".repeat(64),"cliSha256":"c".repeat(64),"elapsedMs":1,
        "completedAt":"2026-09-27T01:00:00Z","imageEvidence":[]})
}

#[cfg(test)]
#[path="editorial_review_tests.rs"]
mod tests;
