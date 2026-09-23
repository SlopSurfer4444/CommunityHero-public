//! Account-bound, replay-safe admission of data-only company knowledge packages.
//! Source assertions and legacy identifiers never become verified facts or authority.
use super::*;

pub struct Package {
    records: Vec<Value>,
    manifest_sha: String,
    records_sha: String,
    generated_at: String,
    coverage_expected: Option<(String,u64)>,
    coverage: Option<Vec<Value>>,
}
fn digest(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|b| b.is_ascii_hexdigit())
}
fn display(company: &str) -> Result<&'static str, &'static str> {
    match company { "likeavto" => Ok("LikeAvto"), "baw-russia" => Ok("BAW Russia"), _ => Err("Unknown company") }
}
impl Package {
    pub fn from_bytes(manifest: &[u8], records: &[u8], expected_manifest: &str, expected_records: &str) -> Result<Self, &'static str> {
        if manifest.len() > 32*1024*1024 || records.len() > 32*1024*1024
            || !digest(expected_manifest) || !digest(expected_records)
            || sha(manifest) != expected_manifest.to_ascii_lowercase() || sha(records) != expected_records.to_ascii_lowercase() {
            return Err("Knowledge package digest or size mismatch");
        }
        let manifest: Value = serde_json::from_slice(manifest).map_err(|_| "Invalid knowledge manifest")?;
        if manifest["schemaVersion"] != "communityhero.company-knowledge.v1" || manifest["grantsExecutionAuthority"] != false
            || manifest["files"]["records.jsonl"]["sha256"] != sha(records)
            || manifest["files"]["records.jsonl"]["bytes"].as_u64() != Some(records.len() as u64) {
            return Err("Unsupported knowledge package contract");
        }
        timestamp(text(&manifest,"generatedAt"))?;
        let raw = std::str::from_utf8(records).map_err(|_| "Invalid knowledge records encoding")?;
        let mut parsed = Vec::new();
        let mut keys = BTreeSet::new();
        let mut legacy = BTreeSet::new();
        let mut counts = BTreeMap::<String,usize>::new();
        for line in raw.lines().filter(|line| !line.trim().is_empty()) {
            if parsed.len() >= 10000 { return Err("Knowledge record limit exceeded"); }
            let record: Value = serde_json::from_str(line).map_err(|_| "Invalid knowledge record")?;
            let company = text(&record,"companyKey"); display(company)?;
            if !record.is_object() || record.as_object().unwrap().keys().any(|key| !["companyKey","importKey","kind","metadata","scope","source","text","observedAt"].contains(&key.as_str()))
                || !matches!(text(&record,"kind"),"claim"|"rule"|"reference"|"transcript"|"ocr"|"customer_case")
                || record["scope"]["companyKey"] != company || record["metadata"]["grantsExecutionAuthority"] != false
                || !digest(text(&record["source"],"sha256")) || text(&record["source"],"origin").is_empty()
                || text(&record,"importKey").is_empty() || text(&record,"importKey").len()>512
                || !record["text"].is_string() || text(&record,"text").len()>200000
                || record["metadata"].get("account").is_some_and(|v|v!=company)
                || !keys.insert((company.to_owned(),text(&record,"importKey").to_owned())) {
                return Err("Invalid or conflicting company knowledge record");
            }
            if let Some(observed)=record.get("observedAt"){timestamp(observed.as_str().ok_or("Invalid observation time")?)?;}
            if record["metadata"].get("transcription").is_some_and(|v|!v.is_object()) || record["metadata"].get("coverage").is_some_and(|v|!v.is_object()) {return Err("Invalid transcript coverage metadata");}
            if let Some(id)=record["metadata"].get("legacyMaterialId") {
                let id=id.as_str().filter(|id|!id.is_empty()&&id.len()<=512).ok_or("Invalid legacy material alias")?;
                if !legacy.insert((company.to_owned(),id.to_owned())) {return Err("Duplicate legacy material alias");}
            }
            for field in ["postAliases","authorAliases"] {
                if let Some(aliases)=record["scope"].get(field) {
                    let aliases=aliases.as_array().filter(|values|values.len()<=12).ok_or("Invalid legacy aliases")?;
                    for alias in aliases {
                        let namespace=if field=="postAliases" {"commentops-fast.post-key"} else {"commentops-fast.author-id"};
                        if alias["namespace"]!=namespace||text(alias,"value").is_empty()||text(alias,"value").len()>1024 {return Err("Invalid legacy aliases");}
                    }
                }
            }
            *counts.entry(company.to_owned()).or_default()+=1;
            parsed.push(record);
        }
        if manifest["recordCount"].as_u64()!=Some(parsed.len() as u64)
            || counts.iter().any(|(company,count)|manifest["companies"][company]["recordCount"].as_u64()!=Some(*count as u64)) {
            return Err("Knowledge package count mismatch");
        }
        let coverage_expected=if let Some(file)=manifest["files"].get("media-coverage.json") {
            let digest=text(file,"sha256");if !self::digest(digest){return Err("Invalid media coverage digest");}
            Some((digest.to_owned(),file["bytes"].as_u64().ok_or("Invalid media coverage size")?))
        }else{None};
        Ok(Self{records:parsed,manifest_sha:expected_manifest.to_ascii_lowercase(),records_sha:expected_records.to_ascii_lowercase(),generated_at:text(&manifest,"generatedAt").to_owned(),coverage_expected,coverage:None})
    }
    pub fn with_coverage(mut self,bytes:&[u8])->Result<Self,&'static str> {
        let (expected,length)=self.coverage_expected.as_ref().ok_or("Media coverage is not in manifest")?;
        if bytes.len()>32*1024*1024||bytes.len() as u64!=*length||sha(bytes)!=*expected{return Err("Media coverage digest or size mismatch");}
        let coverage:Vec<Value>=serde_json::from_slice(bytes).map_err(|_|"Invalid media coverage")?;
        if coverage.len()>10000{return Err("Media coverage limit exceeded");}
        for row in &coverage {
            let company=text(&row["scope"],"companyKey");display(company)?;
            if row["evidence"].get("account").is_some_and(|value|value!=company)||text(&row["scope"],"postKey").is_empty()
                || !matches!(text(row,"transcription"),"present"|"pending"|"error") {return Err("Invalid media coverage scope");}
        }
        self.coverage=Some(coverage);Ok(self)
    }
}

pub fn is_managed_material(d:&Value,id:&str)->bool {
    rows(d,"knowledge_entries").iter().any(|entry|entry["sourceMaterialId"]==id&&(!rows(entry,"companyImportReceipts").is_empty()||super::rule_normalization::owns_entry(entry)))
}
pub fn has_authority(d:&Value)->bool {
    let marker=&d["companyKnowledgeAuthority"];
    marker["owner"]=="communityhero"&&marker["account"]==account(d)
        && display(text(marker,"companyKey")).ok()==Some(account(d))
}

/// Resolve legacy post aliases only inside their original connector namespace.
/// Native connections need their own explicit alias migration; no ID reinterpretation.
pub(super) fn post_keys(d:&Value,v:&Value)->BTreeSet<String> {
    if v.get("companyImport").is_none() {return rows(&v["scope"],"postKeys").iter().filter_map(|v|v.as_str().map(str::to_owned)).collect();}
    let import=&v["companyImport"];
    if d["connectorBinding"]["connector"]!="angryspace" || display(text(import,"companyKey")).ok()!=Some(account(d))
        || d["connectorBinding"]["accountId"]!=account(d) || d["connectorBinding"]["providerAccountId"]!=import["companyKey"] {return BTreeSet::new();}
    rows(&import["scope"],"postAliases").iter().filter(|alias|alias["namespace"]=="commentops-fast.post-key")
        .filter_map(|alias|alias["value"].as_str()).filter(|key|rows(d,"posts").iter().any(|post|post["postKey"]==*key&&in_account(post,account(d))))
        .map(str::to_owned).collect()
}

pub fn apply(d:&mut Value,package:&Package,company:&str,at:&str)->Result<Value,&'static str> {
    let expected=display(company)?;
    if supported_account(d)?!=expected || d["account"]!=expected {return Err("Knowledge workspace company mismatch");}
    if let Some(marker)=d.get("companyKnowledgeAuthority") {
        if marker["owner"]!="communityhero" || marker["companyKey"]!=company || marker["account"]!=expected
            || marker["grantsExecutionAuthority"]!=false || !digest(text(marker,"manifestSha256")) || !digest(text(marker,"recordsSha256")) {
            return Err("Invalid or foreign company knowledge authority");
        }
    }
    if !package.records.iter().any(|record|record["companyKey"]==company){return Err("Package has no selected company records");}
    timestamp(at)?;validate(d)?;
    if package.coverage_expected.is_some()&&package.coverage.is_none(){return Err("Media coverage has not been admitted");}
    // Atomic reducer: malformed later records cannot leave a partially applied workspace.
    let mut candidate=d.clone();
    for table in ["materials","knowledge_entries","knowledge_versions"] {
        if candidate.get(table).is_none(){candidate[table]=json!([]);}
        if !candidate[table].is_array(){return Err("Invalid knowledge collection");}
    }
    let mut receipts=Vec::new();let(mut imported,mut replayed,mut preserved,mut new_materials,mut filled)=(0,0,0,0,0);
    for record in package.records.iter().filter(|record|record["companyKey"]==company) {
        let key=text(record,"importKey");let record_sha=hash(record);
        let existing_key=rows(&candidate,"knowledge_entries").iter().position(|entry|rows(entry,"companyImportReceipts").iter().any(|receipt|receipt["companyKey"]==company&&receipt["importKey"]==key));
        if let Some(index)=existing_key {
            if rows(&candidate["knowledge_entries"][index],"companyImportReceipts").iter().any(|receipt|receipt["companyKey"]==company&&receipt["importKey"]==key&&receipt["recordSha256"]==record_sha) {
                let entry=&candidate["knowledge_entries"][index];
                replayed+=1;receipts.push(json!({"importKey":key,"recordSha256":record_sha,"entryId":entry["id"],"sourceMaterialId":entry["sourceMaterialId"],"currentVersionId":entry["currentVersionId"],"disposition":"replayed"}));continue;
            }
        }
        let legacy=text(&record["metadata"],"legacyMaterialId");
        let alias=if legacy.is_empty(){format!("company-knowledge-{}",hash(&json!([company,key])))}else{format!("import-{legacy}")};
        // Some early workspaces retained the adapter ID verbatim. Admit that exact
        // alias only when unambiguous, never by title/text similarity.
        let raw_existing=!legacy.is_empty()&&rows(&candidate,"materials").iter().any(|m|m["id"]==legacy);
        if raw_existing&&rows(&candidate,"materials").iter().any(|m|m["id"]==alias){return Err("Ambiguous legacy material mapping");}
        let source=if let Some(index)=existing_key {text(&candidate["knowledge_entries"][index],"sourceMaterialId").to_owned()}else if raw_existing{legacy.to_owned()}else{alias};
        let entry_index=rows(&candidate,"knowledge_entries").iter().position(|entry|entry["sourceMaterialId"]==source);
        if let Some(index)=entry_index {
            if candidate["knowledge_entries"][index]["scope"]["account"]!=expected {return Err("Legacy material belongs to another company");}
        }
        let old_material=rows(&candidate,"materials").iter().find(|m|m["id"]==source).cloned();
        if old_material.as_ref().is_some_and(|m|!in_account(m,expected)){return Err("Legacy material belongs to another company");}
        let old_head=entry_index.and_then(|i|rows(&candidate,"knowledge_versions").iter().find(|v|v["id"]==candidate["knowledge_entries"][i]["currentVersionId"])).cloned();
        let kind=match text(record,"kind"){"claim"=>"reference",kind=>kind};
        let meta=&record["metadata"];
        let observed=record["observedAt"].as_str().unwrap_or(&package.generated_at);
        let fill_empty=matches!(kind,"transcript"|"ocr")&&!text(record,"text").trim().is_empty()
            &&old_material.as_ref().is_some_and(|m|m["kind"]==kind&&text(m,"text").trim().is_empty()&&m["locallyEdited"]!=true&&m["manualInstruction"]!=true
                &&m["sourceDate"].as_str().or(m["updatedAt"].as_str()).and_then(|date|timestamp(date).ok()).is_none_or(|date|date<=timestamp(observed).unwrap()))
            &&old_head.as_ref().is_none_or(|v|v["changedBy"]!="operator"&&v["manualInstruction"]!=true&&v["trust"]!="verified"&&v["status"]!="retired"&&text(v,"text").trim().is_empty());
        let protect=(old_material.is_some()||old_head.is_some())&&!fill_empty;
        let mut material=json!({"id":source,"account":expected,"title":format!("Imported {} evidence",text(record,"kind")),"text":record["text"],"kind":kind,"postKey":"","sourceUrl":text(meta,"source_url"),"sourceDate":meta.get("updated_at").or_else(||meta.get("created_at")).or_else(||record.get("observedAt")).cloned().unwrap_or(Value::Null),"revision":old_material.as_ref().map_or(1,|v|v["revision"].as_u64().unwrap_or(0)+1),"updatedAt":at,"companyKnowledge":true});
        if matches!(kind,"transcript"|"ocr") {
            material["transcription"]=meta.get("transcription").cloned().or_else(||meta.get("coverage").filter(|v|v.is_object()).cloned()).unwrap_or_else(||json!({"coverage":"unknown"}));
            material["transcription"]["sourcePostKey"]=meta["post_key"].clone();
            if material["transcription"].get("coverage").is_none(){material["transcription"]["coverage"]=json!("unknown");}
            if let Some(model)=meta.get("model"){material["transcription"]["model"]=model.clone();}
        }
        let backing=if fill_empty{material.clone()}else{old_material.clone().unwrap_or_else(||material.clone())};
        if old_material.is_none(){new_materials+=1;candidate["materials"].as_array_mut().unwrap().push(material.clone());}
        else if fill_empty {filled+=1;let slot=candidate["materials"].as_array_mut().unwrap().iter_mut().find(|m|m["id"]==source).unwrap();*slot=material.clone();}
        let eid=entry_index.map(|i|text(&candidate["knowledge_entries"][i],"id").to_owned()).unwrap_or_else(||format!("knowledge-{}",sha(source.as_bytes())));
        let scope=json!({"account":expected,"postKeys":[]});
        let mut version=json!({"entryId":eid,"sourceMaterialId":source,"sourceRevision":backing["revision"],"sourceHash":hash(&content(&backing)),
            "title":material["title"],"text":record["text"],"sourceUrl":material["sourceUrl"],"postKey":"","kind":kind,"scope":scope,
            "category":meta["category"],"trust":if kind=="rule"{"imported_policy"}else{"source_only"},"status":if protect{"pending_review"}else{"active"},
            "validFrom":at,"validUntil":Value::Null,"sourceDate":material["sourceDate"],"supersedes":Value::Null,"createdAt":at,
            "companyImport":{"companyKey":company,"importKey":key,"recordSha256":record_sha,"source":record["source"],"scope":record["scope"],"metadata":meta,"observedAt":record["observedAt"]},
            "packageProvenance":{"manifestSha256":package.manifest_sha,"recordsSha256":package.records_sha,"generatedAt":package.generated_at},"grantsExecutionAuthority":false});
        copy_media_identity(&material,&mut version);
        let vhash=version_hash(&version);version["hash"]=json!(vhash);version["id"]=json!(format!("knowledge-version-{vhash}"));
        candidate["knowledge_versions"].as_array_mut().unwrap().push(version.clone());
        let index=if let Some(index)=entry_index {index}else{
            // An existing uncatalogued material stays as the head, preserving its
            // text/revision; the imported candidate remains reviewable separately.
            if protect {
                let mut old=version.clone();for field in ["title","text","sourceUrl","postKey","sourceDate"]{old[field]=backing[field].clone();}
                old["kind"]=json!(if matches!(text(&backing,"kind"),"transcript"|"ocr"){text(&backing,"kind")}else{"reference"});
                old["status"]=json!("pending_review");old["trust"]=json!("source_only");old.as_object_mut().unwrap().remove("companyImport");
                old["scope"]=json!({"account":expected,"postKeys":if text(&backing,"postKey").is_empty(){vec![]}else{vec![text(&backing,"postKey")]}});
                let digest=version_hash(&old);old["hash"]=json!(digest);old["id"]=json!(format!("knowledge-version-{digest}"));
                candidate["knowledge_versions"].as_array_mut().unwrap().push(old.clone());
                candidate["knowledge_entries"].as_array_mut().unwrap().push(json!({"id":eid,"sourceMaterialId":source,"currentVersionId":old["id"],"kind":old["kind"],"scope":old["scope"],"status":old["status"]}));
            }else{candidate["knowledge_entries"].as_array_mut().unwrap().push(json!({"id":eid,"sourceMaterialId":source,"currentVersionId":version["id"],"kind":kind,"scope":scope,"status":version["status"]}));}
            candidate["knowledge_entries"].as_array().unwrap().len()-1
        };
        if fill_empty {
            candidate["knowledge_entries"][index]["currentVersionId"]=version["id"].clone();candidate["knowledge_entries"][index]["status"]=version["status"].clone();
            candidate["knowledge_entries"][index]["kind"]=version["kind"].clone();candidate["knowledge_entries"][index]["scope"]=version["scope"].clone();
        }
        if !candidate["knowledge_entries"][index]["companyImportReceipts"].is_array(){candidate["knowledge_entries"][index]["companyImportReceipts"]=json!([]);}
        let disposition=if protect{preserved+=1;"preserved_existing"}else{imported+=1;"imported"};
        let receipt=json!({"companyKey":company,"importKey":key,"recordSha256":record_sha,"entryId":eid,"sourceMaterialId":source,"versionId":version["id"],"currentVersionId":candidate["knowledge_entries"][index]["currentVersionId"],"disposition":if fill_empty{"filled_empty_media"}else{disposition}});
        candidate["knowledge_entries"][index]["companyImportReceipts"].as_array_mut().unwrap().push(receipt.clone());receipts.push(receipt);
    }
    let coverage:Vec<_>=package.coverage.iter().flatten().filter(|row|row["scope"]["companyKey"]==company).cloned().collect();
    if !coverage.is_empty() {
        if candidate.get("companyKnowledgeCoverage").is_none(){candidate["companyKnowledgeCoverage"]=json!([]);}
        let snapshots=candidate["companyKnowledgeCoverage"].as_array_mut().ok_or("Invalid coverage history")?;
        let snapshot=json!({"companyKey":company,"manifestSha256":package.manifest_sha,"sourceSha256":package.coverage_expected.as_ref().unwrap().0,"rows":coverage,"grantsExecutionAuthority":false});
        if let Some(old)=snapshots.iter().find(|v|v["companyKey"]==company&&v["manifestSha256"]==package.manifest_sha) {
            if old!=&snapshot{return Err("Conflicting coverage history");}
        }else{snapshots.push(snapshot);}
    }
    // Authority records the first accepted cutover. Later packet provenance lives
    // in version receipts and coverage snapshots, so alternating replays stay inert.
    if candidate.get("companyKnowledgeAuthority").is_none() {
        candidate["companyKnowledgeAuthority"]=json!({"owner":"communityhero","companyKey":company,"account":expected,
            "manifestSha256":package.manifest_sha,"recordsSha256":package.records_sha,"grantsExecutionAuthority":false});
    }
    validate(&candidate)?;*d=candidate;
    Ok(json!({"companyKey":company,"manifestSha256":package.manifest_sha,"recordsSha256":package.records_sha,"recordCount":receipts.len(),"imported":imported,"newMaterials":new_materials,"filledEmptyMedia":filled,"replayed":replayed,"preservedExisting":preserved,"mediaCoverageCount":coverage.len(),"records":receipts,"grantsExecutionAuthority":false}))
}

#[cfg(test)]
mod tests {
    use super::*;
    const AT:&str="2026-09-22T22:00:00Z";
    fn record(kind:&str,key:&str)->Value {json!({"companyKey":"likeavto","importKey":key,"kind":kind,"text":"Source assertion",
        "scope":{"companyKey":"likeavto"},"source":{"origin":"commentops-fast.account-card","sha256":"a".repeat(64),"originalIds":{"source":"synthetic"}},
        "metadata":{"grantsExecutionAuthority":false,"category":"brand_policy"}})}
    fn package(records:Vec<Value>,coverage:Vec<Value>)->Package {
        let bytes=records.iter().map(Value::to_string).collect::<Vec<_>>().join("\n").into_bytes();
        let coverage=serde_json::to_vec(&coverage).unwrap();
        let mut companies=json!({});for r in &records{let key=text(r,"companyKey");let count=companies[key]["recordCount"].as_u64().unwrap_or(0);companies[key]=json!({"recordCount":count+1});}
        let manifest=json!({"schemaVersion":"communityhero.company-knowledge.v1","grantsExecutionAuthority":false,"generatedAt":AT,"recordCount":records.len(),"companies":companies,
            "files":{"records.jsonl":{"bytes":bytes.len(),"sha256":sha(&bytes)},"media-coverage.json":{"bytes":coverage.len(),"sha256":sha(&coverage)}}});
        let manifest=serde_json::to_vec(&manifest).unwrap();
        Package::from_bytes(&manifest,&bytes,&sha(&manifest),&sha(&bytes)).unwrap().with_coverage(&coverage).unwrap()
    }
    fn workspace()->Value {json!({"account":"LikeAvto","connectorBinding":{"connector":"angryspace","accountId":"LikeAvto","providerAccountId":"likeavto"},"materials":[],"knowledge_entries":[],"knowledge_versions":[],"posts":[],"operations":[{"id":"unknown","status":"unknown"}],"approvals":[{"id":"approved"}],"items":[]})}
    #[test]
    fn import_replay_keeps_one_material_head_and_leaves_authority_records_untouched() {
        let p=package(vec![record("claim","claim-1"),record("rule","rule-1")],vec![]);let mut d=workspace();let before=d.clone();
        let receipt=apply(&mut d,&p,"likeavto",AT).unwrap();assert_eq!(receipt["imported"],2);assert!(has_authority(&d));
        assert_eq!(d["operations"],before["operations"]);assert_eq!(d["approvals"],before["approvals"]);
        assert!(rows(&d,"knowledge_versions").iter().all(|v|v["trust"]!="verified"));
        let saved=d.clone();let receipt=apply(&mut d,&p,"likeavto","2026-09-23T00:00:00Z").unwrap();assert_eq!(receipt["replayed"],2);assert_eq!(d,saved);
        sync_catalog(&mut d,AT).unwrap();assert_eq!(d,saved);
    }
    #[test]
    fn alternating_package_replay_preserves_first_cutover_and_exact_workspace() {
        let base=package(vec![record("claim","base")],vec![]);
        let addendum=package(vec![record("claim","addendum")],vec![]);
        let mut d=workspace();apply(&mut d,&base,"likeavto",AT).unwrap();let marker=d["companyKnowledgeAuthority"].clone();
        apply(&mut d,&addendum,"likeavto",AT).unwrap();assert_eq!(d["companyKnowledgeAuthority"],marker);
        let saved=d.clone();
        for packet in [&base,&addendum] {
            assert_eq!(apply(&mut d,packet,"likeavto","2026-09-23T00:00:00Z").unwrap()["replayed"],1);
            assert_eq!(d,saved);
        }
    }
    #[test]
    fn malformed_or_foreign_cutover_marker_is_rejected_without_repair() {
        let p=package(vec![record("claim","base")],vec![]);let mut accepted=workspace();apply(&mut accepted,&p,"likeavto",AT).unwrap();
        let good=accepted["companyKnowledgeAuthority"].clone();
        let mut invalid=vec![Value::Null,json!({}),json!("legacy")];
        for (field,value) in [("owner",json!("other")),("companyKey",json!("baw-russia")),("account",json!("BAW Russia")),("grantsExecutionAuthority",json!(true)),("manifestSha256",json!("invalid")),("recordsSha256",Value::Null)] {
            let mut marker=good.clone();marker[field]=value;invalid.push(marker);
        }
        for marker in invalid {
            let mut d=accepted.clone();d["companyKnowledgeAuthority"]=marker;let before=d.clone();
            assert_eq!(apply(&mut d,&p,"likeavto",AT).unwrap_err(),"Invalid or foreign company knowledge authority");assert_eq!(d,before);
        }
    }
    #[test]
    fn legacy_alias_maps_exact_material_and_preserves_manual_head_and_newer_content() {
        let mut d=workspace();d["materials"]=json!([{"id":"import-import:knowledge:legacy","title":"Existing","text":"New manual wording","kind":"knowledge","postKey":"","revision":9,"locallyEdited":true}]);
        sync_catalog(&mut d,AT).unwrap();let previous=d["knowledge_versions"][0].clone();let head=d["knowledge_entries"][0]["currentVersionId"].clone();
        let mut r=record("claim","same");r["metadata"]["legacyMaterialId"]=json!("import:knowledge:legacy");let p=package(vec![r],vec![]);
        let receipt=apply(&mut d,&p,"likeavto",AT).unwrap();assert_eq!(receipt["preservedExisting"],1);assert_eq!(rows(&d,"materials").len(),1);assert_eq!(rows(&d,"knowledge_entries").len(),1);
        assert_eq!(d["materials"][0]["text"],"New manual wording");assert_eq!(d["knowledge_entries"][0]["currentVersionId"],head);assert_eq!(d["knowledge_versions"][0],previous);
        assert_eq!(d["knowledge_versions"][1]["status"],"pending_review");assert!(is_managed_material(&d,"import-import:knowledge:legacy"));
    }
    #[test]
    fn changed_import_is_review_candidate_not_silent_overwrite() {
        let mut d=workspace();let first=record("rule","key");apply(&mut d,&package(vec![first.clone()],vec![]),"likeavto",AT).unwrap();let head=d["knowledge_entries"][0]["currentVersionId"].clone();
        let mut changed=first;changed["text"]=json!("Different rule");let p=package(vec![changed],vec![]);let receipt=apply(&mut d,&p,"likeavto",AT).unwrap();
        assert_eq!(receipt["preservedExisting"],1);assert_eq!(d["knowledge_entries"][0]["currentVersionId"],head);assert_eq!(d["materials"][0]["text"],"Source assertion");
        let saved=d.clone();assert_eq!(apply(&mut d,&p,"likeavto",AT).unwrap()["replayed"],1);assert_eq!(d,saved);
    }
    #[test]
    fn local_editor_appends_reviewable_version_after_authority_cutover() {
        let mut d=workspace();let p=package(vec![record("rule","local-edit")],vec![]);apply(&mut d,&p,"likeavto",AT).unwrap();let original=d["knowledge_versions"][0].clone();
        d["materials"][0]["text"]=json!("New operator wording");d["materials"][0]["locallyEdited"]=json!(true);d["materials"][0]["revision"]=json!(2);
        sync_catalog(&mut d,AT).unwrap();assert_eq!(d["knowledge_versions"][0],original);assert_eq!(d["knowledge_versions"][1]["text"],"New operator wording");
        assert_eq!(d["knowledge_versions"][1]["kind"],"rule");assert_eq!(d["knowledge_versions"][1]["status"],"pending_review");assert_eq!(d["knowledge_versions"][1]["changedBy"],"operator");
        let edited=d.clone();apply(&mut d,&p,"likeavto",AT).unwrap();assert_eq!(d,edited);
    }
    #[test]
    fn empty_media_can_be_filled_but_manual_and_newer_heads_are_preserved() {
        let mut r=record("transcript","observed");r["metadata"]["legacyMaterialId"]=json!("old-media");r["observedAt"]=json!(AT);
        r["scope"]["postAliases"]=json!([{"namespace":"commentops-fast.post-key","value":"legacy:post"}]);
        r["metadata"]["coverage"]=json!({"maxAudioSeconds":900,"actualProcessedDurationSeconds":Value::Null,"fullSourceCoverage":"not_measured"});
        let p=package(vec![r],vec![]);
        for mode in ["empty","manual","newer","nonempty"] {
            let mut d=workspace();d["materials"]=json!([{"id":"import-old-media","kind":"transcript","text":if mode=="nonempty"{"new source"}else{""},"postKey":"legacy:post","revision":1,"locallyEdited":mode=="manual","updatedAt":if mode=="newer"{"2026-09-23T00:00:00Z"}else{"2026-09-20T00:00:00Z"}}]);
            sync_catalog(&mut d,AT).unwrap();let old=d["knowledge_versions"][0].clone();let receipt=apply(&mut d,&p,"likeavto",AT).unwrap();assert_eq!(rows(&d,"materials").len(),1);assert_eq!(rows(&d,"knowledge_entries").len(),1);assert_eq!(d["knowledge_versions"][0],old);
            if mode=="empty" {assert_eq!(receipt["filledEmptyMedia"],1);assert_eq!(d["materials"][0]["text"],"Source assertion");assert_eq!(d["materials"][0]["transcription"]["maxAudioSeconds"],900);assert_eq!(d["materials"][0]["transcription"]["actualProcessedDurationSeconds"],Value::Null);}
            else {assert_eq!(receipt["preservedExisting"],1);assert_eq!(d["knowledge_entries"][0]["currentVersionId"],old["id"]);}
        }
    }
    #[test]
    fn native_routes_do_not_reinterpret_legacy_post_aliases() {
        let mut r=record("transcript","media");r["scope"]["postAliases"]=json!([{"namespace":"commentops-fast.post-key","value":"legacy:post"}]);r["metadata"]["post_key"]=json!("legacy:post");
        let mut d=workspace();d["posts"]=json!([{"id":"canonical","postKey":"legacy:post"}]);apply(&mut d,&package(vec![r],vec![]),"likeavto",AT).unwrap();
        let selected=select(&d,&[],&[json!({"postKey":"legacy:post"})],AT).unwrap();assert_eq!(rows(&selected,"materials").len(),1);
        assert_eq!(selected["materials"][0]["transcription"]["coverage"],"unknown");assert_eq!(selected["materials"][0]["postKey"],"legacy:post");assert_eq!(d["knowledge_versions"][0]["postKey"],"");
        assert!(TranscriptLookup::new(&d,AT).unwrap().has(&d["posts"][0]).unwrap());
        d["connectorBinding"]["connector"]=json!("vk");assert!(rows(&select(&d,&[],&[json!({"postKey":"legacy:post"})],AT).unwrap(),"materials").is_empty());
        assert!(!TranscriptLookup::new(&d,AT).unwrap().has(&d["posts"][0]).unwrap());
    }
    #[test]
    fn customer_cases_never_become_global_knowledge_and_claims_stay_source_only() {
        let mut d=workspace();let p=package(vec![record("customer_case","private"),record("claim","assertion"),record("rule","rule")],vec![]);apply(&mut d,&p,"likeavto",AT).unwrap();
        let selected=select(&d,&[],&[],AT).unwrap();assert_eq!(rows(&selected,"materials").len(),2);
        assert!(rows(&selected,"materials").iter().all(|v|v["kind"]!="customer_case"&&v["trust"]!="verified"));
    }
    #[test]
    fn separate_legacy_media_sources_do_not_collapse_into_one_empty_post_group() {
        let mut records=vec![];let mut d=workspace();
        for key in ["legacy:one","legacy:two"] {
            let mut r=record("transcript",key);r["scope"]["postAliases"]=json!([{"namespace":"commentops-fast.post-key","value":key}]);records.push(r);
            d["posts"].as_array_mut().unwrap().push(json!({"id":key,"postKey":key}));
        }
        apply(&mut d,&package(records,vec![]),"likeavto",AT).unwrap();let selected=select(&d,&[],rows(&d,"posts"),AT).unwrap();
        assert_eq!(rows(&selected,"materials").len(),2);assert_ne!(selected["materials"][0]["postKey"],selected["materials"][1]["postKey"]);
    }
    #[test]
    fn coverage_preserves_pending_errors_without_manufacturing_transcripts() {
        let coverage=vec![json!({"scope":{"companyKey":"likeavto","postKey":"legacy:one"},"evidence":{"account":"likeavto","error_code":"download_failed"},"transcription":"error"}),json!({"scope":{"companyKey":"likeavto","postKey":"legacy:two"},"evidence":{"account":"likeavto"},"transcription":"pending"})];
        let p=package(vec![record("rule","one")],coverage.clone());let mut d=workspace();let receipt=apply(&mut d,&p,"likeavto",AT).unwrap();assert_eq!(receipt["mediaCoverageCount"],2);
        assert_eq!(d["companyKnowledgeCoverage"][0]["rows"],json!(coverage));assert_eq!(rows(&d,"materials").len(),1);assert_eq!(d["materials"][0]["kind"],"rule");
    }
    #[test]
    fn mismatched_company_or_later_foreign_alias_leaves_workspace_unchanged() {
        let p=package(vec![record("rule","one")],vec![]);let mut d=workspace();let before=d.clone();assert!(apply(&mut d,&p,"baw-russia",AT).is_err());assert_eq!(d,before);
        let mut r=record("claim","two");r["metadata"]["legacyMaterialId"]=json!("legacy");let p=package(vec![record("rule","one"),r],vec![]);
        d["materials"]=json!([{"id":"import-legacy","account":"BAW Russia","text":"Foreign"}]);let before=d.clone();assert!(apply(&mut d,&p,"likeavto",AT).is_err());assert_eq!(d,before);
        assert!(Package::from_bytes(b"{}",b"{}",&"a".repeat(64),&"b".repeat(64)).is_err());
    }
    #[test]
    #[ignore = "Private package admission: set COMMUNITYHERO_KNOWLEDGE_TEST_PACKAGE explicitly; prints counts only"]
    fn actual_private_package_parity_and_replay() {
        let root=std::path::PathBuf::from(std::env::var_os("COMMUNITYHERO_KNOWLEDGE_TEST_PACKAGE").expect("Explicit package path required"));
        let p=Package::from_bytes(&std::fs::read(root.join("manifest.json")).unwrap(),&std::fs::read(root.join("records.jsonl")).unwrap(),"e3eb430f25a4000ef60ef7698e265a20ce3c4902dccf2dde968cbdc0bf4dc6b4","fc0185662ed38d681ffc29f3ddfb6c9106617c84f6906501637289e06efea539").unwrap().with_coverage(&std::fs::read(root.join("media-coverage.json")).unwrap()).unwrap();
        for (company,count,coverage) in [("likeavto",212,130),("baw-russia",345,86)] {
            let mut d=workspace();d["account"]=json!(display(company).unwrap());d["connectorBinding"]["accountId"]=d["account"].clone();d["connectorBinding"]["providerAccountId"]=json!(company);
            let receipt=apply(&mut d,&p,company,AT).unwrap();assert_eq!(receipt["recordCount"],count);assert_eq!(receipt["imported"],count);assert_eq!(receipt["mediaCoverageCount"],coverage);
            let saved=d.clone();let replay=apply(&mut d,&p,company,AT).unwrap();assert_eq!(replay["replayed"],count);assert!(d==saved,"private package replay changed state");
            assert!(rows(&d,"knowledge_versions").iter().all(|v|v["trust"]!="verified"));
        }
        let mut legacy=workspace();
        for record in p.records.iter().filter(|r|r["companyKey"]=="likeavto"&&!text(&r["metadata"],"legacyMaterialId").is_empty()) {
            legacy["materials"].as_array_mut().unwrap().push(json!({"id":format!("import-{}",text(&record["metadata"],"legacyMaterialId")),"account":"LikeAvto","title":"Current local material","text":record["text"],"kind":if matches!(text(record,"kind"),"transcript"|"ocr"){text(record,"kind")}else{"knowledge"},"revision":7,"locallyEdited":true,"postKey":text(&record["metadata"],"post_key")}));
        }
        sync_catalog(&mut legacy,AT).unwrap();let before=legacy.clone();let receipt=apply(&mut legacy,&p,"likeavto",AT).unwrap();
        assert_eq!(receipt["preservedExisting"],170);assert_eq!(receipt["imported"],42);assert_eq!(receipt["recordCount"],212);
        assert!(&rows(&legacy,"materials")[..170]==rows(&before,"materials"),"legacy materials changed");
        assert!(&rows(&legacy,"knowledge_versions")[..170]==rows(&before,"knowledge_versions"),"legacy history changed");
        if let Some(addendum)=std::env::var_os("COMMUNITYHERO_KNOWLEDGE_ADDENDUM") {
            let base=&p;
            let root=std::path::PathBuf::from(addendum);
            let p=Package::from_bytes(&std::fs::read(root.join("manifest.json")).unwrap(),&std::fs::read(root.join("records.jsonl")).unwrap(),"125846734ee7035544a03e5c1fff3127e7d5cf05434e84c6b0aff88a5f17c515","5e82d2b46e0f9ce02170bd1c9e5d7fb1c4d4a46bda88501b9d1457704eb5a091").unwrap().with_coverage(&std::fs::read(root.join("media-coverage.json")).unwrap()).unwrap();
            let source=format!("import-{}",text(&p.records[0]["metadata"],"legacyMaterialId"));
            legacy["materials"].as_array_mut().unwrap().push(json!({"id":source,"account":"LikeAvto","kind":"transcript","postKey":p.records[0]["metadata"]["post_key"],"text":"","revision":1}));sync_catalog(&mut legacy,AT).unwrap();
            let before_len=rows(&legacy,"materials").len();let receipt=apply(&mut legacy,&p,"likeavto",AT).unwrap();assert_eq!(receipt["recordCount"],1);assert_eq!(receipt["filledEmptyMedia"],1);assert_eq!(rows(&legacy,"materials").len(),before_len);
            let material=rows(&legacy,"materials").iter().find(|m|m["id"]==source).unwrap();assert_eq!(material["transcription"]["maxAudioSeconds"],900);assert_eq!(material["transcription"]["fullSourceCoverage"],"not_measured");assert!(material["transcription"]["actualProcessedDurationSeconds"].is_null());
            let saved=legacy.clone();assert_eq!(apply(&mut legacy,base,"likeavto",AT).unwrap()["replayed"],212);assert!(legacy==saved,"private base replay after addendum changed state");
            assert_eq!(apply(&mut legacy,&p,"likeavto",AT).unwrap()["replayed"],1);assert!(legacy==saved,"private addendum replay changed state");
        }
    }
}
