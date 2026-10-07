//! Reviewed amendments to normalized rules. Pure, atomic reducer; no publication authority.
use super::*;
const ORIGIN: &str = "communityhero.rule-revision";

pub fn plan_hash(plan: &Value) -> String { hash(plan) }
fn object(value: &Value, keys: &[&str]) -> Result<(), &'static str> {
    if value.as_object().is_none_or(|o| o.keys().any(|k| !keys.contains(&k.as_str()))) {
        return Err("Unsupported rule revision field");
    }
    Ok(())
}
fn string<'a>(value: &'a Value, key: &str, max: usize) -> Result<&'a str, &'static str> {
    value[key].as_str().filter(|s| !s.trim().is_empty() && s.encode_utf16().count() <= max)
        .ok_or("Invalid rule revision string")
}
fn operation_version(v: &Value) -> bool {
    v["ruleRevision"]["origin"] == ORIGIN && v["changedBy"] == "rule_revision"
}
fn receipt(d: &Value, request: &str, replayed: bool) -> Value {
    let changes: Vec<_> = rows(d, "knowledge_versions").iter()
        .filter(|v| operation_version(v) && v["scope"]["account"] == account(d) && v["ruleRevision"]["requestId"] == request)
        .map(|v| json!({"entryId":v["entryId"],"versionId":v["id"],"versionHash":v["hash"]})).collect();
    let current = changes.iter().all(|v| rows(d,"knowledge_entries").iter()
        .any(|e| e["id"] == v["entryId"] && e["currentVersionId"] == v["versionId"]));
    json!({"requestId":request,"replayed":replayed,"headsCurrent":current,"changes":changes,"grantsExecutionAuthority":false})
}

/// The authorized boundary supplies the independently reviewed digest, persists
/// this result once, and reconciles stale preparation in that same transaction.
/// All failures leave the caller's workspace unchanged.
pub fn apply(d: &mut Value, plan: &Value, reviewed_hash: &str, at: &str) -> Result<Value, &'static str> {
    validate(d)?;
    let now = timestamp(at)?;
    object(plan, &["schemaVersion", "requestId", "account", "edits"])?;
    if plan["schemaVersion"] != 1 || plan["account"] != supported_account(d)? || plan_hash(plan) != reviewed_hash {
        return Err("Reviewed rule revision plan mismatch");
    }
    if plan.to_string().len() > 2 * 1024 * 1024 { return Err("Rule revision plan too large"); }
    let request = string(plan,"requestId",160)?;
    let edits = plan["edits"].as_array().filter(|e| !e.is_empty() && e.len() <= 200)
        .ok_or("Invalid rule revision edits")?;
    let request_hash = hash(&json!({"action":"revise","plan":plan}));
    let previous: Vec<_> = rows(d,"knowledge_versions").iter().filter(|v|
        operation_version(v) && v["scope"]["account"] == account(d) && v["ruleRevision"]["requestId"] == request).collect();
    if !previous.is_empty() {
        if previous.iter().any(|v| v["ruleRevision"]["requestHash"] != request_hash) {
            return Err("Rule revision requestId already has different input");
        }
        return Ok(receipt(d,request,true));
    }
    let mut next = d.clone();
    let mut seen = BTreeSet::new();
    for edit in edits {
        object(edit,&["entryId","versionId","versionHash","title","text","reason"])?;
        let eid = string(edit,"entryId",512)?;
        string(edit,"versionId",512)?; string(edit,"versionHash",64)?;
        string(edit,"title",240)?; string(edit,"text",24000)?; string(edit,"reason",2000)?;
        if !seen.insert(eid) { return Err("Duplicate rule revision entry"); }
        let index = rows(d,"knowledge_entries").iter().position(|e| e["id"] == eid).ok_or("Rule revision entry not found")?;
        let entry = &d["knowledge_entries"][index];
        let current = rows(d,"knowledge_versions").iter().find(|v| v["id"] == entry["currentVersionId"])
            .ok_or("Rule revision head missing")?;
        if entry["currentVersionId"] != edit["versionId"] || current["hash"] != edit["versionHash"] {
            return Err("Rule revision head conflict");
        }
        if !rule_normalization::owns_entry(entry) || entry["ruleNormalizationOwner"]["account"] != plan["account"]
            || current["scope"]["account"] != plan["account"] || current["kind"] != "rule"
            || current["status"] != "active" || current["trust"] != "imported_policy"
            || current["ruleNormalization"]["role"] != "created" || current["ruleNormalization"]["action"] != "apply"
            || current["manualInstruction"] == true || entry["manualInstruction"] == true
            || timestamp(text(current,"validFrom"))? > now
            || (!current["validUntil"].is_null() && current["validUntil"].as_str().is_none_or(|v| timestamp(v).map_or(true,|t| t <= now))) {
            return Err("Rule revision requires active normalized policy in the exact account");
        }
        let material_index = rows(d,"materials").iter().position(|m| m["id"] == entry["sourceMaterialId"])
            .ok_or("Rule revision backing material missing")?;
        let original = &d["materials"][material_index];
        if !in_account(original,account(d)) || current["sourceHash"] != hash(&content(original)) {
            return Err("Rule revision backing material conflict");
        }
        if [entry,current,original].iter().any(|v| v["protected"] == true || v["manualInstruction"] == true) {
            return Err("Protected rule cannot be revised");
        }
        if current["title"] == edit["title"] && current["text"] == edit["text"] { return Err("Rule revision has no content change"); }
        let material = &mut next["materials"][material_index];
        material["title"] = edit["title"].clone(); material["text"] = edit["text"].clone();
        material["revision"] = json!(original["revision"].as_u64().and_then(|n| n.checked_add(1)).ok_or("Invalid rule material revision")?);
        material["locallyEdited"] = json!(true); material["updatedAt"] = json!(at);
        let mut version = current.clone();
        version["title"] = edit["title"].clone(); version["text"] = edit["text"].clone();
        version["sourceRevision"] = material["revision"].clone(); version["sourceHash"] = json!(hash(&content(material)));
        version["supersedes"] = current["id"].clone(); version["createdAt"] = json!(at);
        // Keep source clauses as lineage, while explicitly identifying the new
        // body as a reviewed operator amendment rather than imported wording.
        version["changedBy"] = json!("rule_revision"); version["grantsExecutionAuthority"] = json!(false);
        version["ruleRevision"] = json!({"origin":ORIGIN,"changeType":"reviewed_operator_amendment","requestId":request,
            "requestHash":request_hash,"reviewedPlanHash":reviewed_hash,"reason":edit["reason"],
            "sourceVersionId":current["id"],"sourceVersionHash":current["hash"],"sourceMaterialHash":current["sourceHash"]});
        let digest = version_hash(&version); version["hash"] = json!(digest); version["id"] = json!(format!("knowledge-version-{digest}"));
        if rows(&next,"knowledge_versions").iter().any(|v| v["id"] == version["id"]) { return Err("Duplicate rule revision version"); }
        next["knowledge_entries"][index]["currentVersionId"] = version["id"].clone();
        next["knowledge_versions"].as_array_mut().ok_or("Missing knowledge versions")?.push(version);
    }
    validate(&next)?;
    let result = receipt(&next,request,false);
    *d = next;
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    const AT: &str = "2026-09-24T06:00:00Z";
    const LATER: &str = "2026-09-24T07:00:00Z";
    fn fixture() -> Value {
        let mut d = json!({"account":"LikeAvto","materials":[],"knowledge_entries":[],"knowledge_versions":[],"posts":[],"items":[],
            "jobs":[{"id":"running","status":"running"}],"operations":[{"id":"unknown","status":"unknown"}],"approvals":[{"id":"approval"}]});
        let mut manifest = json!({"entries":{}});
        for id in ["first","second"] {
            d["materials"].as_array_mut().unwrap().push(json!({"id":id,"account":"LikeAvto","title":id,"text":"Original policy.","kind":"rule","revision":1,"postKey":"","sourceUrl":"","sourceDate":null}));
            manifest["entries"][id] = json!({"kind":"rule","category":"reply_guidance","trust":"imported_policy","status":"active","scope":{"account":"LikeAvto","postKeys":[]},"contentHash":sha(b"Original policy.")});
        }
        sync_with_manifest(&mut d,AT,&manifest).unwrap();
        for index in 0..2 {
            d["knowledge_entries"][index]["ruleNormalizationOwner"] = json!({"origin":"communityhero.rule-normalization","account":"LikeAvto"});
            let v = &mut d["knowledge_versions"][index];
            v["ruleNormalization"] = json!({"origin":"communityhero.rule-normalization","role":"created","action":"apply","sourceClauses":[{"sourceVersionHash":"original-import-proof"}]});
            v["changedBy"] = json!("rule_normalization");
            let digest = version_hash(v); v["hash"] = json!(digest); v["id"] = json!(format!("knowledge-version-{digest}"));
            d["knowledge_entries"][index]["currentVersionId"] = d["knowledge_versions"][index]["id"].clone();
        }
        validate(&d).unwrap(); d
    }
    fn plan(d:&Value) -> Value {
        let edits:Vec<_> = rows(d,"knowledge_entries").iter().map(|e| {
            let v=rows(d,"knowledge_versions").iter().find(|v|v["id"]==e["currentVersionId"]).unwrap();
            json!({"entryId":e["id"],"versionId":v["id"],"versionHash":v["hash"],"title":v["title"],"text":"Reviewed replacement.","reason":"Owner reviewed this wording."})
        }).collect();
        json!({"schemaVersion":1,"requestId":"revise-1","account":"LikeAvto","edits":edits})
    }
    #[test]
    fn atomic_revision_preserves_history_provenance_scope_and_invalidates_manifest() {
        let mut d=fixture(); let before=d.clone(); let p=plan(&d);
        let old=select(&d,&[],&[],AT).unwrap();
        let result=apply(&mut d,&p,&plan_hash(&p),LATER).unwrap();
        assert_eq!(rows(&result,"changes").len(),2);
        assert!(rows(&d,"knowledge_versions").starts_with(rows(&before,"knowledge_versions")));
        assert_ne!(old["manifest"],select(&d,&[],&[],LATER).unwrap()["manifest"]);
        for key in ["jobs","operations","approvals","items"] {assert_eq!(d[key],before[key]);}
        for i in 0..2 {
            let v=&d["knowledge_versions"][i+2]; let prior=&before["knowledge_versions"][i];
            for key in ["scope","trust","validFrom","validUntil","ruleNormalization"] {assert_eq!(v[key],prior[key]);}
            assert_eq!(v["supersedes"],prior["id"]); assert_eq!(v["ruleRevision"]["sourceVersionHash"],prior["hash"]);
            assert_eq!(v["ruleRevision"]["changeType"],"reviewed_operator_amendment");
            assert_eq!(d["materials"][i]["locallyEdited"],true);
            assert_eq!(v["sourceHash"],hash(&content(&d["materials"][i])));
        }
        let saved=d.clone(); sync_catalog(&mut d,LATER).unwrap(); assert_eq!(d,saved,"import cannot replace owned heads");
    }
    #[test]
    fn replay_does_not_restore_newer_heads_and_reused_request_rejects_changed_input() {
        let mut d=fixture(); let p=plan(&d); apply(&mut d,&p,&plan_hash(&p),LATER).unwrap(); let saved=d.clone();
        assert_eq!(apply(&mut d,&p,&plan_hash(&p),LATER).unwrap()["replayed"],true); assert_eq!(d,saved);
        let mut changed=p.clone(); changed["edits"][0]["text"]=json!("Different.");
        assert!(apply(&mut d,&changed,&plan_hash(&changed),LATER).is_err()); assert_eq!(d,saved);
        let e=d["knowledge_entries"][0].clone(); revise(&mut d,text(&e,"id"),&json!({"expectedVersionId":e["currentVersionId"],"status":"retired"}),LATER).unwrap();
        let retired=d.clone(); assert_eq!(apply(&mut d,&p,&plan_hash(&p),LATER).unwrap()["headsCurrent"],false); assert_eq!(d,retired);
    }
    #[test]
    fn invalid_late_edit_unknown_fields_foreign_account_and_stale_heads_are_atomic() {
        let baseline=fixture(); let original=plan(&baseline);
        for mode in ["unknown_root","unknown_edit","wrong_account","wrong_hash","stale","empty","duplicate","no_change","scope","status","trust"] {
            let mut d=baseline.clone(); let mut p=original.clone();
            match mode {
                "unknown_root"=>p["reviewed"]=json!(true), "unknown_edit"=>p["edits"][1]["typo"]=json!("bad"),
                "wrong_account"=>p["account"]=json!("BAW Russia"), "wrong_hash"=>p["edits"][1]["versionHash"]=json!("wrong"),
                "stale"=>p["edits"][1]["versionId"]=json!("stale"), "empty"=>p["edits"][1]["text"]=json!(" "),
                "duplicate"=>p["edits"][1]=p["edits"][0].clone(), "no_change"=>p["edits"][1]["text"]=json!("Original policy."),
                key=>p["edits"][1][key]=json!("unsupported"),
            }
            assert!(apply(&mut d,&p,&plan_hash(&p),LATER).is_err(),"{mode}"); assert_eq!(d,baseline,"{mode}");
        }
        let mut d=baseline.clone(); assert!(apply(&mut d,&original,"not-reviewed",LATER).is_err()); assert_eq!(d,baseline);
    }
    #[test]
    fn unowned_protected_and_changed_backing_material_cannot_be_amended() {
        for mode in ["unowned","protected","source"] {
            let mut d=fixture(); let p=plan(&d);
            match mode {"unowned"=>d["knowledge_entries"][1]["ruleNormalizationOwner"]=Value::Null,
                "protected"=>d["materials"][1]["protected"]=json!(true),_=>d["materials"][1]["text"]=json!("Unreviewed edit.")};
            let before=d.clone(); assert!(apply(&mut d,&p,&plan_hash(&p),LATER).is_err()); assert_eq!(d,before);
        }
    }
    // Public snapshot excludes actual_baw_candidate_replays_in_memory: private workspace and BAW activation-plan inputs are not published.
    // This excluded test is not counted as PASS; see VALIDATION.md.
}
