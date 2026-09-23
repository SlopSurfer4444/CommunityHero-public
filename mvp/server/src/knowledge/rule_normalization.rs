//! Reviewed, atomic normalization of already-active imported policy. No transport or approval API.
use super::*;
const ORIGIN:&str="communityhero.rule-normalization";

pub fn plan_hash(plan:&Value)->String {hash(plan)}
pub(super) fn owns_entry(entry:&Value)->bool {entry["ruleNormalizationOwner"]["origin"]==ORIGIN}
// Ordinary revise/restore copies provenance but sets changedBy=operator. Such
// descendants are not extra results of the original normalization transaction.
fn operation_version(v:&Value)->bool {v["ruleNormalization"]["origin"]==ORIGIN&&v["changedBy"]=="rule_normalization"}
fn object(value:&Value,keys:&[&str])->Result<(),&'static str>{
    if value.as_object().is_none_or(|o|o.keys().any(|k|!keys.contains(&k.as_str()))){return Err("Unsupported normalization field");} Ok(())
}
fn string<'a>(value:&'a Value,key:&str,max:usize)->Result<&'a str,&'static str>{
    value[key].as_str().filter(|s|!s.trim().is_empty()&&s.encode_utf16().count()<=max).ok_or("Invalid normalization string")
}
fn array<'a>(value:&'a Value,key:&str,max:usize)->Result<&'a [Value],&'static str>{
    value[key].as_array().filter(|a|a.len()<=max).map(Vec::as_slice).ok_or("Invalid normalization array")
}
fn scope(value:&Value,account:&str)->Result<(),&'static str>{
    object(value,&["account","postKeys"])?;
    if value["account"]!=account{return Err("Normalization scope account mismatch");}
    let mut seen=BTreeSet::new();
    for key in array(value,"postKeys",100)? {let key=key.as_str().filter(|s|!s.is_empty()&&s.len()<=512).ok_or("Invalid normalization post scope")?;if !seen.insert(key){return Err("Duplicate normalization post scope");}}
    Ok(())
}
fn head<'a>(d:&'a Value,reference:&Value)->Result<(&'a Value,&'a Value),&'static str>{
    let entry=rows(d,"knowledge_entries").iter().find(|e|e["id"]==reference["entryId"]).ok_or("Normalization entry not found")?;
    let version=rows(d,"knowledge_versions").iter().find(|v|v["id"]==entry["currentVersionId"]).ok_or("Normalization head not found")?;
    if entry["currentVersionId"]!=reference["versionId"]||version["hash"]!=reference["versionHash"]{return Err("Normalization head conflict");}
    Ok((entry,version))
}
fn receipt(d:&Value,request:&str,replayed:bool)->Value {
    let changes:Vec<_>=rows(d,"knowledge_versions").iter().filter(|v|operation_version(v)&&v["scope"]["account"]==account(d)&&v["ruleNormalization"]["requestId"]==request)
        .map(|v|json!({"entryId":v["entryId"],"versionId":v["id"],"versionHash":v["hash"],"role":v["ruleNormalization"]["role"]})).collect();
    let current=changes.iter().all(|v|rows(d,"knowledge_entries").iter().any(|e|e["id"]==v["entryId"]&&e["currentVersionId"]==v["versionId"]));
    json!({"requestId":request,"replayed":replayed,"headsCurrent":current,"changes":changes,"grantsExecutionAuthority":false})
}
fn replay(d:&Value,request:&str,request_hash:&str)->Result<Option<Value>,&'static str>{
    let matching:Vec<_>=rows(d,"knowledge_versions").iter().filter(|v|operation_version(v)&&v["scope"]["account"]==account(d)&&v["ruleNormalization"]["requestId"]==request).collect();
    if matching.is_empty(){return Ok(None)}
    if matching.iter().any(|v|v["ruleNormalization"]["requestHash"]!=request_hash){return Err("Normalization requestId already has different input");}
    Ok(Some(receipt(d,request,true)))
}
fn stamp(d:&mut Value,mut version:Value,entry:Value)->Result<(),&'static str>{
    version["changedBy"]=json!("rule_normalization");
    let digest=version_hash(&version);version["hash"]=json!(digest);version["id"]=json!(format!("knowledge-version-{digest}"));
    if rows(d,"knowledge_versions").iter().any(|v|v["id"]==version["id"]){return Err("Duplicate normalization version");}
    let mut entry=entry;entry["currentVersionId"]=version["id"].clone();entry["status"]=version["status"].clone();
    entry["ruleNormalizationOwner"]=json!({"origin":ORIGIN,"account":version["scope"]["account"]});
    let entries=d["knowledge_entries"].as_array_mut().ok_or("Missing knowledge entries")?;
    if let Some(old)=entries.iter_mut().find(|e|e["id"]==entry["id"]){*old=entry}else{entries.push(entry)}
    d["knowledge_versions"].as_array_mut().ok_or("Missing knowledge versions")?.push(version);Ok(())
}
fn begin(d:&Value,plan:&Value,reviewed_hash:&str,at:&str)->Result<(),&'static str>{
    validate(d)?;timestamp(at)?;
    if plan["schemaVersion"]!=1||plan["account"]!=supported_account(d)?||plan_hash(plan)!=reviewed_hash{return Err("Reviewed normalization plan mismatch");}
    if plan.to_string().len()>2*1024*1024{return Err("Normalization plan too large");}
    string(plan,"requestId",160)?;
    for key in ["materials","knowledge_entries","knowledge_versions"]{if !d[key].is_array(){return Err("Missing normalization collection");}}
    Ok(())
}

/// `reviewed_hash` comes from the future authorized maintenance boundary, never
/// from a self-declared review field in the submitted plan. The caller persists
/// the result in ONE existing workspace transaction. All errors leave `d` intact.
pub fn apply(d:&mut Value,plan:&Value,reviewed_hash:&str,at:&str)->Result<Value,&'static str>{
    begin(d,plan,reviewed_hash,at)?;
    object(plan,&["schemaVersion","requestId","candidateId","account","sources","clauses","rules","coverage","unresolvedAmbiguityIds","deferredGroups","ownerDecisions"])?;
    let request=string(plan,"requestId",160)?;let candidate_id=string(plan,"candidateId",160)?;
    let ambiguities=array(plan,"unresolvedAmbiguityIds",100)?;
    let request_hash=hash(&json!({"action":"apply","plan":plan}));
    if let Some(receipt)=replay(d,request,&request_hash)? {return Ok(receipt)}
    let sources=array(plan,"sources",100)?;let clauses=array(plan,"clauses",1000)?;let rules=array(plan,"rules",200)?;
    if sources.is_empty()||rules.is_empty(){return Err("Normalization sources and rules are required");}
    let mut source_map=BTreeMap::new();let mut replaced=BTreeSet::new();
    for source in sources {
        object(source,&["entryId","versionId","versionHash","sourceHash","textSha256Utf8","scope","disposition"])?;
        let eid=string(source,"entryId",512)?;let (entry,version)=head(d,source)?;
        if source_map.insert(eid,(entry,version)).is_some(){return Err("Duplicate normalization source");}
        scope(&source["scope"],account(d))?;
        if source["scope"]!=version["scope"]||source["sourceHash"]!=version["sourceHash"]||source["textSha256Utf8"]!=sha(text(version,"text").as_bytes()) {return Err("Normalization source content mismatch");}
        let material=rows(d,"materials").iter().find(|m|m["id"]==entry["sourceMaterialId"]).ok_or("Normalization source material missing")?;
        if !in_account(material,account(d))||version["sourceHash"]!=hash(&content(material)){return Err("Normalization material conflict");}
        if version["status"]!="active"||version["trust"]!="imported_policy"||!matches!(text(version,"kind"),"rule"|"policy")
            || version["manualInstruction"]==true||entry["manualInstruction"]==true||material["manualInstruction"]==true||material["locallyEdited"]==true||version["changedBy"]=="operator"||owns_entry(entry)
            || (!version["validUntil"].is_null()&&!version["validUntil"].is_string())
            || timestamp(text(version,"validFrom"))?>timestamp(at)?||version["validUntil"].as_str().is_some_and(|until|timestamp(until).map_or(true,|until|until<=timestamp(at).unwrap())) {
            return Err("Normalization requires unmodified active imported policy");
        }
        match text(source,"disposition") {
            "retain"=>{},
            "replace"=>{
                if !rows(&version["companyImport"]["scope"],"postAliases").is_empty()||!rows(&version["companyImport"]["scope"],"authorAliases").is_empty(){return Err("Legacy scoped policy needs an explicit scope migration");}
                let field=text(&version["companyImport"]["source"]["originalIds"],"field");
                if ["forbidden_substrings","allowed_reply_urls","forbidden_reply_prefixes"].contains(&field)
                    || [entry,version,material].iter().any(|v|v["protected"]==true){return Err("Protected normalization source must be retained");}
                replaced.insert(eid);
            },
            _=>return Err("Invalid normalization source disposition"),
        }
    }
    let mut unresolved=BTreeSet::new();
    for id in ambiguities {let id=id.as_str().filter(|s|!s.is_empty()&&s.len()<=160).ok_or("Invalid ambiguity ID")?;if !unresolved.insert(id){return Err("Duplicate ambiguity ID");}}
    let mut decisions=BTreeMap::new();let mut resolved=BTreeSet::new();
    let owner_decisions=if plan.get("ownerDecisions").is_some(){array(plan,"ownerDecisions",20)?}else{&[]};
    for decision in owner_decisions {
        object(decision,&["id","sha256","resolvedAmbiguityIds"])?;let id=string(decision,"id",160)?;let digest=string(decision,"sha256",64)?;
        if digest.len()!=64||!digest.bytes().all(|v|v.is_ascii_hexdigit())||decisions.insert(id,decision).is_some(){return Err("Invalid owner decision reference");}
        let ids=array(decision,"resolvedAmbiguityIds",100)?;if ids.is_empty(){return Err("Owner decision must identify its clarification");}
        for id in ids {let id=id.as_str().filter(|s|!s.is_empty()&&s.len()<=160).ok_or("Invalid owner ambiguity ID")?;
            if unresolved.contains(id)||!resolved.insert(id){return Err("Owner ambiguity resolution conflicts");}}
    }
    let groups=if plan.get("deferredGroups").is_some(){array(plan,"deferredGroups",100)?}else{&[]};
    for group in groups {
        object(group,&["ambiguityId","retainedEntryIds"])?;
        if !unresolved.remove(string(group,"ambiguityId",160)?){return Err("Unknown or repeated deferred ambiguity");}
        let retained=array(group,"retainedEntryIds",100)?;if retained.is_empty(){return Err("Deferred ambiguity has no preserved source");}
        let mut seen=BTreeSet::new();for eid in retained {let eid=eid.as_str().ok_or("Invalid deferred source")?;
            if !seen.insert(eid)||!source_map.contains_key(eid)||replaced.contains(eid){return Err("Ambiguous normalization source must be retained");}}
    }
    if !unresolved.is_empty(){return Err("Normalization has unresolved ambiguities");}
    if replaced.is_empty(){return Err("Normalization has no replaceable sources");}
    let mut clause_map=BTreeMap::new();let mut ranges:BTreeMap<&str,Vec<(usize,usize)>>=BTreeMap::new();
    for clause in clauses {
        object(clause,&["id","entryId","versionId","startByte","endByte","textSha256Utf8"])?;
        let cid=string(clause,"id",160)?;let eid=string(clause,"entryId",512)?;
        let (_,version)=source_map.get(eid).ok_or("Foreign normalization clause")?;
        let start=clause["startByte"].as_u64().and_then(|v|usize::try_from(v).ok()).ok_or("Invalid clause byte range")?;
        let end=clause["endByte"].as_u64().and_then(|v|usize::try_from(v).ok()).ok_or("Invalid clause byte range")?;
        let slice=text(version,"text").get(start..end).filter(|s|!s.trim().is_empty()).ok_or("Invalid clause UTF8 range")?;
        if clause["versionId"]!=version["id"]||clause["textSha256Utf8"]!=sha(slice.as_bytes())||clause_map.insert(cid,clause).is_some(){return Err("Clause identity or hash mismatch");}
        ranges.entry(eid).or_default().push((start,end));
    }
    for eid in &replaced {
        let text=text(source_map[eid].1,"text");let mut end=0;
        let spans=ranges.get_mut(eid).ok_or("Replaced source has no clauses")?;spans.sort();
        for &(start,next) in spans.iter(){if start<end||!text.get(end..start).is_some_and(|s|s.trim().is_empty()){return Err("Clause coverage overlaps or omits source text");}end=next;}
        if !text[end..].trim().is_empty(){return Err("Clause coverage omits source text");}
    }
    let mut links:BTreeMap<&str,BTreeSet<&str>>=BTreeMap::new();let mut rule_ids=BTreeSet::new();let mut normalized_text=BTreeSet::new();let mut used_decisions=BTreeSet::new();
    for rule in rules {
        object(rule,&["id","title","text","category","scope","sourceClauseIds","kind","trust","ownerDecisionIds"])?;
        let rid=string(rule,"id",160)?;string(rule,"title",240)?;let body=string(rule,"text",24000)?;string(rule,"category",160)?;scope(&rule["scope"],account(d))?;
        if rule["kind"]!="rule"||rule["trust"]!="imported_policy"||!rule_ids.insert(rid)||!normalized_text.insert((hash(&rule["scope"]),body.split_whitespace().collect::<Vec<_>>().join(" "))){return Err("Invalid or duplicate normalized rule");}
        let owner_refs=if rule.get("ownerDecisionIds").is_some(){array(rule,"ownerDecisionIds",20)?}else{&[]};let mut own_seen=BTreeSet::new();
        for id in owner_refs {let id=id.as_str().ok_or("Invalid rule owner decision ID")?;
            if !decisions.contains_key(id)||!own_seen.insert(id){return Err("Unknown or repeated owner decision");}used_decisions.insert(id);}
        let refs=array(rule,"sourceClauseIds",1000)?;if refs.is_empty(){return Err("Normalized rule has no source clause");}
        let mut seen=BTreeSet::new();
        for cid in refs {
            let cid=cid.as_str().ok_or("Invalid normalized clause ID")?;let clause=clause_map.get(cid).ok_or("Missing normalized source clause")?;
            let eid=text(clause,"entryId");
            if !seen.insert(cid)||!replaced.contains(eid)||source_map[eid].1["scope"]!=rule["scope"]{return Err("Normalized scope expansion or duplicate clause");}
            links.entry(cid).or_default().insert(rid);
        }
    }
    if used_decisions.len()!=decisions.len(){return Err("Owner decision has no affected normalized rule");}
    let mut covered=BTreeSet::new();
    for coverage in array(plan,"coverage",1000)? {
        object(coverage,&["clauseId","ruleIds","retainedEntryId","archivedLegacy"])?;let cid=string(coverage,"clauseId",160)?;
        let clause=clause_map.get(cid).ok_or("Foreign clause coverage")?;if !covered.insert(cid){return Err("Duplicate clause coverage");}
        let eid=text(clause,"entryId");
        if replaced.contains(eid) {
            if coverage.get("retainedEntryId").is_some(){return Err("Replaced clause cannot be retained");}
            let archived=coverage.get("archivedLegacy");
            if let Some(archive)=archived {
                object(archive,&["reason","sourceContractIds","evidenceRefs"])?;string(archive,"reason",2000)?;
                for field in ["sourceContractIds","evidenceRefs"] {
                    let refs=array(archive,field,50)?;if refs.is_empty(){return Err("Archival provenance is required");}
                    let mut unique=BTreeSet::new();for value in refs {let value=value.as_str().filter(|v|!v.trim().is_empty()&&v.len()<=512).ok_or("Invalid archival reference")?;if !unique.insert(value){return Err("Duplicate archival reference");}}
                }
            }
            let values=if coverage.get("ruleIds").is_some(){array(coverage,"ruleIds",200)?}else{&[]};
            let mut targets=BTreeSet::new();for rid in values {let rid=rid.as_str().ok_or("Invalid coverage rule ID")?;if !targets.insert(rid){return Err("Duplicate coverage target");}}
            if (targets.is_empty()&&archived.is_none())||links.get(cid).cloned().unwrap_or_default()!=targets{return Err("Clause coverage disagrees with normalized rules");}
        } else if coverage["retainedEntryId"]!=eid||coverage.get("ruleIds").is_some()||coverage.get("archivedLegacy").is_some(){return Err("Retained clause coverage mismatch");}
    }
    if covered.len()!=clause_map.len(){return Err("Missing clause coverage");}
    let mut next=d.clone();
    for rule in rules {
        let source=format!("normalized-rule-{}",hash(&json!([account(d),candidate_id,rule["id"]])));let eid=format!("knowledge-{}",sha(source.as_bytes()));
        if rows(d,"materials").iter().any(|m|m["id"]==source)||rows(d,"knowledge_entries").iter().any(|e|e["id"]==eid){return Err("Normalized rule identity already exists");}
        let post_key=if rows(&rule["scope"],"postKeys").len()==1{rule["scope"]["postKeys"][0].clone()}else{json!("")};
        let material=json!({"id":source,"account":account(d),"title":rule["title"],"text":rule["text"],"kind":"rule","postKey":post_key,"sourceUrl":"","sourceDate":null,"revision":1,"updatedAt":at});
        next["materials"].as_array_mut().unwrap().push(material.clone());
        let mut until:Option<DateTime<Utc>>=None;let mut provenance=Vec::new();
        for cid in rows(rule,"sourceClauseIds") {
            let clause=clause_map[cid.as_str().unwrap()];let version=source_map[text(clause,"entryId")].1;
            if let Some(end)=version["validUntil"].as_str(){let end=timestamp(end)?;until=Some(until.map_or(end,|old|old.min(end)));}
            provenance.push(json!({"clause":clause,"sourceVersionHash":version["hash"]}));
        }
        let owner_refs:Vec<_>=rows(rule,"ownerDecisionIds").iter().map(|id|decisions[id.as_str().unwrap()].clone()).collect();
        let version=json!({"entryId":eid,"sourceMaterialId":source,"sourceRevision":1,"sourceHash":hash(&content(&material)),
            "title":rule["title"],"text":rule["text"],"sourceUrl":"","postKey":post_key,"sourceDate":null,"kind":"rule","category":rule["category"],
            "scope":rule["scope"],"trust":"imported_policy","status":"active","validFrom":at,"validUntil":until.map(|v|v.to_rfc3339()),"supersedes":null,"createdAt":at,
            "ruleNormalization":{"origin":ORIGIN,"action":"apply","role":"created","requestId":request,"requestHash":request_hash,"reviewedPlanHash":reviewed_hash,"candidateId":candidate_id,"normalizedRuleId":rule["id"],"sourceClauses":provenance,
                "changeType":if owner_refs.is_empty(){"normalization"}else{"owner_clarification"},"ownerDecisions":owner_refs},"grantsExecutionAuthority":false});
        let entry=json!({"id":eid,"sourceMaterialId":source,"kind":"rule","scope":rule["scope"]});stamp(&mut next,version,entry)?;
    }
    for eid in replaced {
        let (entry,old)=source_map[eid];let mut version=old.clone();version["status"]=json!("retired");version["supersedes"]=old["id"].clone();version["createdAt"]=json!(at);
        let coverage:Vec<_>=rows(plan,"coverage").iter().filter(|row|clause_map[text(row,"clauseId")]["entryId"]==eid)
            .map(|row|json!({"clause":clause_map[text(row,"clauseId")],"coverage":row})).collect();
        version["ruleNormalization"]=json!({"origin":ORIGIN,"action":"apply","role":"retired","requestId":request,"requestHash":request_hash,"reviewedPlanHash":reviewed_hash,"candidateId":candidate_id,"sourceVersionId":old["id"],"clauseCoverage":coverage});
        stamp(&mut next,version,entry.clone())?;
    }
    validate(&next)?;let result=receipt(&next,request,false);*d=next;Ok(result)
}

/// Restore only an untouched installed batch. Rollback itself appends versions;
/// replay cannot resurrect an old batch, and a newer head requires separate review.
pub fn rollback(d:&mut Value,plan:&Value,reviewed_hash:&str,at:&str)->Result<Value,&'static str>{
    begin(d,plan,reviewed_hash,at)?;object(plan,&["schemaVersion","requestId","account","appliedRequestId","expectedHeads"])?;
    let request=string(plan,"requestId",160)?;let applied=string(plan,"appliedRequestId",160)?;
    let request_hash=hash(&json!({"action":"rollback","plan":plan}));if let Some(result)=replay(d,request,&request_hash)?{return Ok(result)}
    let installed:Vec<_>=rows(d,"knowledge_versions").iter().filter(|v|operation_version(v)&&v["scope"]["account"]==account(d)&&v["ruleNormalization"]["action"]=="apply"&&v["ruleNormalization"]["requestId"]==applied).cloned().collect();
    let refs=array(plan,"expectedHeads",300)?;if installed.is_empty()||refs.len()!=installed.len(){return Err("Rollback batch coverage mismatch");}
    let mut seen=BTreeSet::new();let mut next=d.clone();
    for reference in refs {
        object(reference,&["entryId","versionId","versionHash"])?;let (entry,current)=head(d,reference)?;
        if !seen.insert(text(entry,"id"))||current["scope"]["account"]!=plan["account"]||!installed.iter().any(|v|v["id"]==current["id"]){return Err("Rollback head changed or foreign");}
        let mut restored=if current["ruleNormalization"]["role"]=="retired" {
            rows(d,"knowledge_versions").iter().find(|v|v["id"]==current["ruleNormalization"]["sourceVersionId"]&&v["entryId"]==entry["id"]).ok_or("Rollback source missing")?.clone()
        }else if current["ruleNormalization"]["role"]=="created"{let mut v=current.clone();v["status"]=json!("retired");v}else{return Err("Invalid rollback role")};
        restored["restoredFrom"]=if current["ruleNormalization"]["role"]=="retired"{restored["id"].clone()}else{Value::Null};
        restored["supersedes"]=current["id"].clone();restored["createdAt"]=json!(at);
        restored["ruleNormalization"]=json!({"origin":ORIGIN,"action":"rollback","role":"rollback","requestId":request,"requestHash":request_hash,"reviewedPlanHash":reviewed_hash,"appliedRequestId":applied});
        stamp(&mut next,restored,entry.clone())?;
    }
    validate(&next)?;let result=receipt(&next,request,false);*d=next;Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    const AT:&str="2026-09-23T12:00:00Z";
    const LATER:&str="2026-09-23T13:00:00Z";
    fn fixture()->Value {
        let mut d=json!({"account":"LikeAvto","materials":[],"knowledge_entries":[],"knowledge_versions":[],"posts":[],
            "items":[{"id":"untouched"}],"jobs":[{"id":"running","status":"running"}],"operations":[{"id":"unknown","status":"unknown"}],"approvals":[{"id":"approval"}]});
        let mut manifest=json!({"entries":{}});
        for (id,title,body) in [("rule-a","Clarity","Отвечай ясно."),("rule-b","Brevity","Keep replies brief."),("constraint","Literal constraint","[\"forbidden phrase\"]")] {
            d["materials"].as_array_mut().unwrap().push(json!({"id":id,"title":title,"text":body,"kind":"rule","revision":1,"account":"LikeAvto","postKey":"","sourceUrl":"","sourceDate":null}));
            manifest["entries"][id]=json!({"kind":"rule","category":"reply_guidance","trust":"imported_policy","status":"active","scope":{"account":"LikeAvto","postKeys":[]},"contentHash":sha(body.as_bytes())});
        }
        sync_with_manifest(&mut d,AT,&manifest).unwrap();
        edit_head(&mut d,"constraint",|v|v["companyImport"]=json!({"source":{"originalIds":{"field":"forbidden_substrings"}}}));
        d
    }
    fn edit_head(d:&mut Value,source:&str,change:impl FnOnce(&mut Value)) {
        let e=rows(d,"knowledge_entries").iter().position(|e|e["sourceMaterialId"]==source).unwrap();
        let v=rows(d,"knowledge_versions").iter().position(|v|v["id"]==d["knowledge_entries"][e]["currentVersionId"]).unwrap();
        change(&mut d["knowledge_versions"][v]);
        let digest=version_hash(&d["knowledge_versions"][v]);d["knowledge_versions"][v]["hash"]=json!(digest);d["knowledge_versions"][v]["id"]=json!(format!("knowledge-version-{digest}"));
        d["knowledge_entries"][e]["currentVersionId"]=d["knowledge_versions"][v]["id"].clone();
        for key in ["kind","status","scope"]{d["knowledge_entries"][e][key]=d["knowledge_versions"][v][key].clone();}
    }
    fn plan(d:&Value)->Value {
        let mut sources=Vec::new();let mut clauses=Vec::new();let mut coverage=Vec::new();let mut refs=Vec::new();
        for (index,e) in rows(d,"knowledge_entries").iter().enumerate() {
            let v=rows(d,"knowledge_versions").iter().find(|v|v["id"]==e["currentVersionId"]).unwrap();let retained=e["sourceMaterialId"]=="constraint";let cid=format!("c{index}");
            sources.push(json!({"entryId":e["id"],"versionId":v["id"],"versionHash":v["hash"],"sourceHash":v["sourceHash"],"textSha256Utf8":sha(text(v,"text").as_bytes()),"scope":v["scope"],"disposition":if retained{"retain"}else{"replace"}}));
            clauses.push(json!({"id":cid,"entryId":e["id"],"versionId":v["id"],"startByte":0,"endByte":text(v,"text").len(),"textSha256Utf8":sha(text(v,"text").as_bytes())}));
            coverage.push(if retained{json!({"clauseId":cid,"retainedEntryId":e["id"]})}else{refs.push(json!(cid));json!({"clauseId":cid,"ruleIds":["clear-brief"]})});
        }
        json!({"schemaVersion":1,"requestId":"normalize-1","candidateId":"reviewed-candidate-1","account":"LikeAvto","sources":sources,"clauses":clauses,"coverage":coverage,"unresolvedAmbiguityIds":[],
            "rules":[{"id":"clear-brief","title":"Clarity and brevity","text":"Write clear, brief replies.","kind":"rule","category":"writing_style","trust":"imported_policy","scope":{"account":"LikeAvto","postKeys":[]},"sourceClauseIds":refs}]})
    }
    fn rollback_plan(d:&Value)->Value {
        let expected=rows(&receipt(d,"normalize-1",false),"changes").iter().map(|v|json!({"entryId":v["entryId"],"versionId":v["versionId"],"versionHash":v["versionHash"]})).collect::<Vec<_>>();
        json!({"schemaVersion":1,"requestId":"rollback-1","account":"LikeAvto","appliedRequestId":"normalize-1","expectedHeads":expected})
    }
    #[test]
    fn atomic_install_keeps_history_constraints_trust_and_unrelated_state(){
        let mut d=fixture();let before=d.clone();let p=plan(&d);let result=apply(&mut d,&p,&plan_hash(&p),AT).unwrap();
        assert_eq!(rows(&result,"changes").len(),3);assert_eq!(result["headsCurrent"],true);assert_eq!(result["grantsExecutionAuthority"],false);
        assert!(rows(&d,"knowledge_versions").starts_with(rows(&before,"knowledge_versions")));assert_eq!(rows(&d,"knowledge_versions").len(),6);
        assert_eq!(rows(&d,"knowledge_entries").iter().filter(|e|e["status"]=="active").count(),2);
        assert_eq!(d["knowledge_entries"][2],before["knowledge_entries"][2]);assert_eq!(d["materials"][2],before["materials"][2]);
        for key in ["items","jobs","operations","approvals"]{assert_eq!(d[key],before[key]);}
        let created=rows(&d,"knowledge_versions").iter().find(|v|v["ruleNormalization"]["role"]=="created").unwrap();
        assert_eq!(created["trust"],"imported_policy");assert_eq!(created["category"],"writing_style");assert!(created.get("companyImport").is_none());assert!(created.get("manualInstruction").is_none());
        assert_eq!(rows(&select(&d,&[],&[],AT).unwrap(),"materials").len(),2);
    }
    #[test]
    fn replay_is_inert_and_same_key_different_content_fails_before_mutation(){
        let mut d=fixture();let p=plan(&d);apply(&mut d,&p,&plan_hash(&p),AT).unwrap();let saved=d.clone();
        assert_eq!(apply(&mut d,&p,&plan_hash(&p),LATER).unwrap()["replayed"],true);assert_eq!(d,saved);
        let mut changed=p.clone();changed["rules"][0]["text"]=json!("Changed reviewed text");
        assert_eq!(apply(&mut d,&changed,&plan_hash(&changed),LATER),Err("Normalization requestId already has different input"));assert_eq!(d,saved);
    }
    #[test]
    fn malformed_late_plan_parts_are_all_or_nothing(){
        let baseline=fixture();let original=plan(&baseline);
        let edits:Vec<Box<dyn Fn(&mut Value)>>=vec![
            Box::new(|p|p["coverage"].as_array_mut().unwrap().pop().map(|_|()).unwrap()),
            Box::new(|p|p["clauses"][0]["startByte"]=json!(1)),
            Box::new(|p|p["clauses"][0]["endByte"]=json!(0)),
            Box::new(|p|p["clauses"][0]["textSha256Utf8"]=json!("bad")),
            Box::new(|p|p["sources"][1]["versionHash"]=json!("bad")),
            Box::new(|p|p["sources"][1]["sourceHash"]=json!("bad")),
            Box::new(|p|p["rules"][0]["sourceClauseIds"].as_array_mut().unwrap().push(json!("missing"))),
            Box::new(|p|p["rules"][0]["trust"]=json!("verified")),
            Box::new(|p|p["rules"][0]["kind"]=json!("fact")),
            Box::new(|p|p["rules"][0]["scope"]["postKeys"]=json!(["foreign"])),
            Box::new(|p|p["rules"][0]["companyImport"]=json!({"forged":true})),
            Box::new(|p|p["unresolvedAmbiguityIds"]=json!(["ambiguous"])),
            Box::new(|p|p["account"]=json!("BAW Russia")),
        ];
        for edit in edits {let mut d=baseline.clone();let mut p=original.clone();edit(&mut p);assert!(apply(&mut d,&p,&plan_hash(&p),AT).is_err());assert_eq!(d,baseline);}
        let mut d=baseline.clone();assert_eq!(apply(&mut d,&original,"not-reviewed",AT),Err("Reviewed normalization plan mismatch"));assert_eq!(d,baseline);
        // A collision discovered after the first staged new version still cannot leak it.
        let mut p=original.clone();let mut second=p["rules"][0].clone();second["id"]=json!("second");second["text"]=json!("Other rule");p["rules"].as_array_mut().unwrap().push(second);
        for row in p["coverage"].as_array_mut().unwrap(){if row["ruleIds"].is_array(){row["ruleIds"].as_array_mut().unwrap().push(json!("second"));}}
        let source=format!("normalized-rule-{}",hash(&json!(["LikeAvto",p["candidateId"],"second"])));d["materials"].as_array_mut().unwrap().push(json!({"id":source}));let before=d.clone();
        assert_eq!(apply(&mut d,&p,&plan_hash(&p),AT),Err("Normalized rule identity already exists"));assert_eq!(d,before);
    }
    #[test]
    fn stale_manual_fact_pending_and_protected_heads_cannot_be_replaced(){
        for (key,value) in [("manualInstruction",json!(true)),("trust",json!("verified")),("trust",json!("source_only")),("kind",json!("reference")),("status",json!("pending_review")),("protected",json!(true)),("changedBy",json!("operator"))] {
            let mut d=fixture();edit_head(&mut d,"rule-a",|v|v[key]=value);let p=plan(&d);let before=d.clone();assert!(apply(&mut d,&p,&plan_hash(&p),AT).is_err(),"{key}");assert_eq!(d,before);
        }
        let mut d=fixture();let p=plan(&d);edit_head(&mut d,"rule-b",|v|v["category"]=json!("new category"));let before=d.clone();
        assert_eq!(apply(&mut d,&p,&plan_hash(&p),AT),Err("Normalization head conflict"));assert_eq!(d,before);
        let mut d=fixture();let mut p=plan(&d);p["sources"][2]["disposition"]=json!("replace");let before=d.clone();
        assert_eq!(apply(&mut d,&p,&plan_hash(&p),AT),Err("Protected normalization source must be retained"));assert_eq!(d,before);
    }
    #[test]
    fn safe_independent_groups_retain_shared_ambiguous_heads_exactly(){
        let mut d=fixture();let mut p=plan(&d);let retained=p["sources"][1]["entryId"].clone();p["sources"][1]["disposition"]=json!("retain");
        p["rules"][0]["sourceClauseIds"]=json!(["c0"]);p["coverage"][1]=json!({"clauseId":"c1","retainedEntryId":retained});
        p["unresolvedAmbiguityIds"]=json!(["CTA","REACTION"]);p["deferredGroups"]=json!([{"ambiguityId":"CTA","retainedEntryIds":[retained]},{"ambiguityId":"REACTION","retainedEntryIds":[retained]}]);
        let before=d.clone();apply(&mut d,&p,&plan_hash(&p),AT).unwrap();assert_eq!(d["knowledge_entries"][1],before["knowledge_entries"][1]);
        let mut invalid=p.clone();invalid["deferredGroups"][0]["retainedEntryIds"]=json!([p["sources"][0]["entryId"]]);let mut untouched=before.clone();
        assert_eq!(apply(&mut untouched,&invalid,&plan_hash(&invalid),AT),Err("Ambiguous normalization source must be retained"));assert_eq!(untouched,before);
    }
    #[test]
    fn upstream_sync_preserves_owned_heads_and_backing_edits_conflict(){
        let mut d=fixture();let p=plan(&d);apply(&mut d,&p,&plan_hash(&p),AT).unwrap();let saved=d.clone();sync_catalog(&mut d,LATER).unwrap();assert_eq!(d,saved);
        for entry in rows(&d,"knowledge_entries").iter().filter(|e|owns_entry(e)){assert!(company_import::is_managed_material(&d,text(entry,"sourceMaterialId")));}
        d["materials"][0]["text"]=json!("Upstream changed wording");let changed=d.clone();
        assert_eq!(sync_catalog(&mut d,LATER),Err("Normalized rules require a reviewed versioned change"));assert_eq!(d,changed);
    }
    #[test]
    fn rollback_appends_history_replays_inertly_and_never_reactivates_on_apply_replay(){
        let mut d=fixture();let before=d.clone();let p=plan(&d);apply(&mut d,&p,&plan_hash(&p),AT).unwrap();let installed=d.clone();let undo=rollback_plan(&d);
        rollback(&mut d,&undo,&plan_hash(&undo),LATER).unwrap();assert!(rows(&d,"knowledge_versions").starts_with(rows(&installed,"knowledge_versions")));assert_eq!(rows(&d,"knowledge_versions").len(),9);
        for entry in rows(&before,"knowledge_entries"){
            let now=rows(&d,"knowledge_entries").iter().find(|e|e["id"]==entry["id"]).unwrap();let v=rows(&d,"knowledge_versions").iter().find(|v|v["id"]==now["currentVersionId"]).unwrap();
            let old=rows(&before,"knowledge_versions").iter().find(|v|v["id"]==entry["currentVersionId"]).unwrap();assert_eq!(v["text"],old["text"]);assert_eq!(v["status"],old["status"]);assert_eq!(v["trust"],old["trust"]);
        }
        let saved=d.clone();assert_eq!(rollback(&mut d,&undo,&plan_hash(&undo),LATER).unwrap()["replayed"],true);assert_eq!(d,saved);
        let replay=apply(&mut d,&p,&plan_hash(&p),LATER).unwrap();assert_eq!(replay["headsCurrent"],false);assert_eq!(d,saved);sync_catalog(&mut d,LATER).unwrap();assert_eq!(d,saved);
    }
    #[test]
    fn rollback_rejects_partial_foreign_or_newer_heads_without_mutation(){
        let mut d=fixture();let p=plan(&d);apply(&mut d,&p,&plan_hash(&p),AT).unwrap();let mut undo=rollback_plan(&d);undo["expectedHeads"].as_array_mut().unwrap().pop();let before=d.clone();
        assert!(rollback(&mut d,&undo,&plan_hash(&undo),LATER).is_err());assert_eq!(d,before);
        let mut undo=rollback_plan(&d);let first=undo["expectedHeads"][0].clone();revise(&mut d,text(&first,"entryId"),&json!({"expectedVersionId":first["versionId"],"status":"pending_review"}),LATER).unwrap();
        let entry=rows(&d,"knowledge_entries").iter().find(|e|e["id"]==first["entryId"]).unwrap();let v=rows(&d,"knowledge_versions").iter().find(|v|v["id"]==entry["currentVersionId"]).unwrap();
        undo["expectedHeads"][0]=json!({"entryId":entry["id"],"versionId":v["id"],"versionHash":v["hash"]});let changed=d.clone();
        assert_eq!(rollback(&mut d,&undo,&plan_hash(&undo),LATER),Err("Rollback head changed or foreign"));assert_eq!(d,changed);
        let replay=apply(&mut d,&p,&plan_hash(&p),LATER).unwrap();assert_eq!(rows(&replay,"changes").len(),3);assert_eq!(replay["headsCurrent"],false);assert_eq!(d,changed);
    }
    #[test]
    fn owner_clarification_is_explicit_provenance_without_trust_elevation(){
        let mut d=fixture();let mut p=plan(&d);
        p["ownerDecisions"]=json!([{"id":"owner-clarification","sha256":"a".repeat(64),"resolvedAmbiguityIds":["REACTION"]}]);
        p["rules"][0]["ownerDecisionIds"]=json!(["owner-clarification"]);
        apply(&mut d,&p,&plan_hash(&p),AT).unwrap();let created=rows(&d,"knowledge_versions").iter().find(|v|v["ruleNormalization"]["role"]=="created").unwrap();
        assert_eq!(created["trust"],"imported_policy");assert_eq!(created["ruleNormalization"]["changeType"],"owner_clarification");assert_eq!(created["ruleNormalization"]["ownerDecisions"],p["ownerDecisions"]);
        let mut d=fixture();let before=d.clone();p["unresolvedAmbiguityIds"]=json!(["REACTION"]);assert!(apply(&mut d,&p,&plan_hash(&p),AT).is_err());assert_eq!(d,before);
    }
    #[test]
    fn archived_runtime_contracts_remain_immutable_history_not_active_brand_rules(){
        let mut d=fixture();let mut p=plan(&d);p["rules"][0]["sourceClauseIds"]=json!(["c0"]);
        let archive=json!({"reason":"Superseded output schema retained as history","sourceContractIds":["legacy-output"],"evidenceRefs":["mvp/adapters/assistant.mjs:reviewInstructions"]});
        p["coverage"][1]=json!({"clauseId":"c1","archivedLegacy":archive});let before=d.clone();
        apply(&mut d,&p,&plan_hash(&p),AT).unwrap();let retired=rows(&d,"knowledge_versions").iter().find(|v|v["sourceMaterialId"]=="rule-b"&&v["ruleNormalization"]["role"]=="retired").unwrap();
        assert_eq!(retired["ruleNormalization"]["clauseCoverage"][0]["coverage"]["archivedLegacy"],archive);
        assert!(rows(&d,"knowledge_versions").starts_with(rows(&before,"knowledge_versions")));
        let created=rows(&d,"knowledge_versions").iter().find(|v|v["ruleNormalization"]["role"]=="created").unwrap();assert_eq!(rows(&created["ruleNormalization"],"sourceClauses").len(),1);
        let mut invalid=p.clone();invalid["coverage"][1]["archivedLegacy"]["evidenceRefs"]=json!([]);let mut untouched=before.clone();assert!(apply(&mut untouched,&invalid,&plan_hash(&invalid),AT).is_err());assert_eq!(untouched,before);
    }
    #[test]
    fn validity_cannot_expand_and_legacy_alias_scope_is_never_silently_globalized(){
        let mut d=fixture();edit_head(&mut d,"rule-a",|v|v["validUntil"]=json!("2026-09-24T00:00:00Z"));
        edit_head(&mut d,"rule-b",|v|v["validUntil"]=json!("2026-09-25T00:00:00Z"));let p=plan(&d);apply(&mut d,&p,&plan_hash(&p),AT).unwrap();
        let created=rows(&d,"knowledge_versions").iter().find(|v|v["ruleNormalization"]["role"]=="created").unwrap();assert_eq!(timestamp(text(created,"validUntil")).unwrap(),timestamp("2026-09-24T00:00:00Z").unwrap());
        let mut d=fixture();edit_head(&mut d,"rule-a",|v|v["companyImport"]=json!({"scope":{"postAliases":[{"namespace":"legacy.example","value":"opaque-post"}]}}));let p=plan(&d);let before=d.clone();
        assert_eq!(apply(&mut d,&p,&plan_hash(&p),AT),Err("Legacy scoped policy needs an explicit scope migration"));assert_eq!(d,before);
    }
    #[test]
    fn later_company_import_only_appends_pending_candidates_and_preserves_normalized_heads(){
        let mut d=fixture();let p=plan(&d);apply(&mut d,&p,&plan_hash(&p),AT).unwrap();let heads=d["knowledge_entries"].clone();let history=d["knowledge_versions"].clone();
        let record=json!({"companyKey":"likeavto","importKey":"later-legacy-rule","kind":"rule","text":"Changed upstream rule",
            "scope":{"companyKey":"likeavto"},"source":{"origin":"synthetic-import","sha256":"a".repeat(64)},"metadata":{"grantsExecutionAuthority":false,"legacyMaterialId":"rule-a"}});
        let bytes=record.to_string().into_bytes();let manifest=json!({"schemaVersion":"communityhero.company-knowledge.v1","grantsExecutionAuthority":false,"generatedAt":AT,
            "recordCount":1,"companies":{"likeavto":{"recordCount":1}},"files":{"records.jsonl":{"bytes":bytes.len(),"sha256":sha(&bytes)}}}).to_string().into_bytes();
        let package=company_import::Package::from_bytes(&manifest,&bytes,&sha(&manifest),&sha(&bytes)).unwrap();company_import::apply(&mut d,&package,"likeavto",LATER).unwrap();
        assert!(rows(&d,"knowledge_versions").starts_with(history.as_array().unwrap()));
        for old in heads.as_array().unwrap(){let current=rows(&d,"knowledge_entries").iter().find(|e|e["id"]==old["id"]).unwrap();assert_eq!(current["currentVersionId"],old["currentVersionId"]);assert_eq!(current["ruleNormalizationOwner"],old["ruleNormalizationOwner"]);}
        assert_eq!(d["knowledge_versions"].as_array().unwrap().last().unwrap()["status"],"pending_review");let saved=d.clone();sync_catalog(&mut d,LATER).unwrap();assert_eq!(d,saved);
    }
}
