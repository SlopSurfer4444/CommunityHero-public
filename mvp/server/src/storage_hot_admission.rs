//! Bounded control/history projections under the existing company writer lock.
//! Source/knowledge and operation blockers deliberately remain complete in this
//! first slice. Filtered history is never passed to the full-workspace validator.
use super::*;
use serde_json::json;

#[derive(Clone, Copy)]
pub(crate) enum AdmissionScope<'a> {
    EditorialSchedule(&'a Value),
    OperatorEditorial(&'a Value),
    EditorialJob(&'a str),
    EditorialRepair { job: &'a str, body: &'a Value },
    Approval(&'a Value),
    Execute { approval: &'a str, body: &'a Value },
    ExecutionReceipts(&'a str),
}

fn request(scope: AdmissionScope<'_>) -> Option<(&str, &Value)> {
    match scope {
        AdmissionScope::EditorialSchedule(body) | AdmissionScope::OperatorEditorial(body) => Some(("editorial", body)),
        AdmissionScope::Approval(body) => Some(("approval", body)),
        AdmissionScope::Execute { body, .. } => Some(("execute", body)),
        AdmissionScope::EditorialRepair { body, .. } => Some(("editorial-repair", body)),
        _ => None,
    }
}
fn root_job(scope: AdmissionScope<'_>) -> Option<&str> {
    match scope { AdmissionScope::EditorialJob(job) | AdmissionScope::ExecutionReceipts(job) | AdmissionScope::EditorialRepair {job,..} => Some(job), _ => None }
}
fn references(scope: AdmissionScope<'_>, jobs: &[Value], approvals: &[Value]) -> HashSet<String> {
    if let AdmissionScope::EditorialRepair {body,..}=scope{return body["expected"].as_array().into_iter().flatten()
        .filter_map(|r|r["proposalId"].as_str().map(str::to_owned)).collect();}
    let refs = match scope {
        AdmissionScope::EditorialSchedule(body) | AdmissionScope::OperatorEditorial(body) | AdmissionScope::Approval(body) => &body["proposals"],
        AdmissionScope::EditorialJob(key) => jobs.iter().find(|j|j["id"]==key).map(|j|&j["editorialReferences"]).unwrap_or(&Value::Null),
        AdmissionScope::Execute { approval, .. } => approvals.iter().find(|a|a["id"]==approval).map(|a|&a["proposals"]).unwrap_or(&Value::Null),
        AdmissionScope::ExecutionReceipts(_) => &Value::Null,
        AdmissionScope::EditorialRepair {..} => unreachable!(),
    };
    refs.as_array().into_iter().flatten().filter_map(|r|r["id"].as_str().map(str::to_owned)).collect()
}
fn approval_key(scope: AdmissionScope<'_>, jobs: &[Value], operations: &[Value]) -> Option<String> {
    match scope {
        AdmissionScope::Execute { approval, .. } => Some(approval.to_owned()),
        AdmissionScope::ExecutionReceipts(key) => jobs.iter().find(|j|j["id"]==key).and_then(|job| {
            if job["kind"]=="execute" { job["refId"].as_str().map(str::to_owned) }
            else if job["kind"]=="reconcile" { operations.iter().find(|o|o["id"]==job["refId"]).and_then(|o|o["approvalId"].as_str()).map(str::to_owned) }
            else { None }
        }),
        _ => None,
    }
}
fn receipt_matches(row: &Value, scope: AdmissionScope<'_>) -> bool {
    request(scope).and_then(|(kind,body)|body["requestId"].as_str().map(|key|(kind,key)))
        .is_some_and(|(kind,key)|row["id"]==crate::local_admission::receipt_id(kind,key)
            ||(matches!(row["action"].as_str(),Some(crate::local_admission::ACTION|crate::local_admission::REJECTED_ACTION))
                &&row["kind"]==kind&&row["requestId"]==key)
            ||(kind=="execute"&&(1..=crate::local_admission::MAX_REJECTED_EVALUATIONS as u64)
                .any(|index|row["id"]==crate::local_admission::negative_id(kind,key,index))))
}
fn selected_retained_recovery(view:&Value)->bool {
    view["proposals"].as_array().into_iter().flatten()
        .any(|p|p.get(crate::retained_paid_recovery::FIELD).is_some())
}
fn generation_jobs(proposals: &[Value]) -> HashSet<String> {
    proposals.iter().flat_map(|p|[&p["prepareRunId"],&p["origin"]["prepareRunId"],&p["recovery"]["prepareRunId"]])
        .filter_map(|v|v.as_str().map(str::to_owned)).collect()
}
fn active_external_job(job: &Value) -> bool {
    matches!(job["kind"].as_str(),Some("execute"|"reconcile"))
        && matches!(job["status"].as_str(),Some("running"|"queued"|"pending"))
}
fn pinned_research_only(scope:AdmissionScope<'_>)->bool {
    // These scopes check an existing saved proposal. New editorial captures
    // may select cached research and therefore retain the complete archive.
    matches!(scope,AdmissionScope::Approval(_)|AdmissionScope::Execute{..})
}
fn retain_job(job: &Value, scope: AdmissionScope<'_>, refs: &HashSet<String>, approval: Option<&str>) -> bool {
    if super::preparation::retained_material_job(job){return true;}
    if crate::conductor_authority::current_context().is_some_and(|ctx|job["id"]==ctx.run_id){return true;}
    // A different approval/reconcile may still finish an operation retained
    // through recipient aliases. Absence from the selected approval is not
    // evidence that its external writer has stopped.
    if active_external_job(job) {return true;}
    if root_job(scope).is_some_and(|key|job["id"]==key) {return true;}
    if matches!(scope,AdmissionScope::ExecutionReceipts(_)) {
        return job["kind"]=="execute"&&approval.is_some_and(|key|job["refId"]==key);
    }
    refs.contains(job["id"].as_str().unwrap_or(""))
        ||matches!(job["kind"].as_str(),Some("media"|"media_audio"|"media_analysis"|"media_analysis_applicability"))
        ||(matches!(scope,AdmissionScope::EditorialSchedule(_)|AdmissionScope::OperatorEditorial(_))&&job["kind"]=="editorial_review"
            &&matches!(job["status"].as_str(),Some("running"|"queued")))
}
fn project(workspace: &Value, scope: AdmissionScope<'_>) -> ApiResult<Value> {
    let key=approval_key(scope,rows(workspace,"jobs")?,rows(workspace,"operations")?);
    let approvals:Vec<_>=rows(workspace,"approvals")?.iter().filter(|a|key.as_deref().is_some_and(|k|a["id"]==k)).cloned().collect();
    let selected=references(scope,rows(workspace,"jobs")?,&approvals);
    let proposals:Vec<_>=rows(workspace,"proposals")?.iter().filter(|p|selected.contains(p["id"].as_str().unwrap_or(""))).cloned().collect();
    let runs=generation_jobs(&proposals);
    let mut view=if pinned_research_only(scope) {
        Value::Object(workspace.as_object().ok_or_else(||internal("Invalid admission metadata"))?.iter()
            .filter(|(key,_)|!TABLES.contains(&key.as_str())&&key.as_str()!="preparationResearch")
            .map(|(key,value)|(key.clone(),value.clone())).collect())
    }else{metadata(workspace)};
    for table in TABLES {
        view[table]=match table {
            "proposals"=>json!(proposals),
            "approvals"=>json!(approvals),
            "jobs"=>json!(rows(workspace,table)?.iter().filter(|j|retain_job(j,scope,&runs,key.as_deref())).collect::<Vec<_>>()),
            "audit"=>json!(rows(workspace,table)?.iter().filter(|a|receipt_matches(a,scope)).collect::<Vec<_>>()),
            "feedback"=>json!([]),
            "conversations"=>json!(rows(workspace,table)?.iter().filter(|chat|matches!(scope,AdmissionScope::ExecutionReceipts(_))
                &&chat["actionReviews"].as_array().into_iter().flatten().any(|r|key.as_deref().is_some_and(|key|r["execution"]["approvalId"]==key))).collect::<Vec<_>>()),
            _ if matches!(scope,AdmissionScope::ExecutionReceipts(_))=>json!([]),
            _=>workspace[table].clone(),
        };
    }
    if !matches!(scope,AdmissionScope::ExecutionReceipts(_)) {
        loop {
            let (ids,bundles,full)=super::source_snapshot::scoped_job_dependencies(rows(&view,"jobs")?,rows(&view,"proposals")?);
            let next=rows(workspace,"jobs")?.iter().filter(|job|full||retain_job(job,scope,&runs,key.as_deref())
                ||job["id"].as_str().is_some_and(|id|ids.iter().any(|selected|selected==id))
                ||job["prepareBundle"]["id"].as_str().is_some_and(|id|bundles.iter().any(|selected|selected==id)))
                .cloned().collect::<Vec<_>>();
            let complete=next.as_slice()==rows(&view,"jobs")?.as_slice();view["jobs"]=json!(next);if complete{break;}
        }
    }
    // Receipt refresh only needs this approval's operations. Other admission
    // scopes retain every operation, including malformed legacy alias blockers.
    if matches!(scope,AdmissionScope::ExecutionReceipts(_)) {
        view["operations"]=json!(rows(workspace,"operations")?.iter().filter(|o|key.as_deref().is_some_and(|key|o["approvalId"]==key)).collect::<Vec<_>>());
    }
    if !matches!(scope,AdmissionScope::ExecutionReceipts(_)){super::scope_context::project(workspace,&mut view)?;}
    if pinned_research_only(scope){super::source_snapshot::project_pinned_research(workspace,&mut view);}
    Ok(view)
}
fn equal_except(a:&Value,b:&Value,fields:&[&str])->bool {
    match(a.as_object(),b.as_object()) {
        (Some(a),Some(b))=>a.iter().filter(|(k,_)|!fields.contains(&k.as_str())).eq(b.iter().filter(|(k,_)|!fields.contains(&k.as_str()))),
        _=>false,
    }
}
fn validate_editorial_journal(old:&Value,new:&Value)->ApiResult<()> {
    let a=old["editorialBatches"].as_array().ok_or_else(||internal("Editorial journal missing"))?;
    let b=new["editorialBatches"].as_array().ok_or_else(||internal("Editorial journal missing"))?;
    if b.len()<a.len()||b.len()>a.len()+1{return Err(internal("Editorial journal inventory changed"));}
    let mut changed=0;
    for(old,new)in a.iter().zip(b){if old==new{continue;}changed+=1;
        if !equal_except(old,new,&["state","result","resultDigest"])||old["state"]!="captured"||new["state"]!="settled"
            ||!new["result"].is_object()||new["resultDigest"]!=crate::editorial_review::hash_text(&new["result"].to_string()){
            return Err(internal("Editorial captured attempt changed"));
        }
    }
    if changed>1||(changed>0&&b.len()!=a.len()){return Err(internal("Editorial checkpoint changed multiple attempts"));}
    if let Some(entry)=b.get(a.len()){
        if entry["state"]!="captured"||a.iter().any(|old|old["batchId"]==entry["batchId"]){return Err(internal("Editorial duplicated a captured attempt"));}
        let parent=old["editorialPlan"]["batches"].as_array().into_iter().flatten().find(|p|p["id"]==entry["batchId"])
            .ok_or_else(||internal("Editorial capture parent missing"))?;
        if entry["digest"]!=parent["digest"]{return Err(internal("Editorial capture binding changed"));}
        crate::editorial_review::validate_dispatch_capture(parent,&entry["capture"]).map_err(internal)?;
    }
    Ok(())
}
fn validate_receipt_chat(before:&Value,after:&Value,approval:Option<&str>)->ApiResult<()> {
    let a=before["actionReviews"].as_array().ok_or_else(||internal("Receipt reviews missing"))?;
    let b=after["actionReviews"].as_array().ok_or_else(||internal("Receipt reviews missing"))?;
    if a.len()!=b.len(){return Err(internal("Receipt review inventory changed"));}
    let mut affected=HashSet::new();
    for(old,new)in a.iter().zip(b){
        if approval.is_some_and(|key|old["execution"]["approvalId"]==key){
            affected.insert(text(old,"id")?);
            if !equal_except(old,new,&["status","execution","updatedAt","outcome","resultMessageId"])
                ||!equal_except(&old["execution"],&new["execution"],&["jobId","externalOutcome","recoveryReason"]){return Err(internal("Receipt review authority changed"));}
        }else if old!=new{return Err(internal("Receipt changed another review"));}
    }
    let a=rows(before,"messages")?;let b=rows(after,"messages")?;
    if b.len()<a.len(){return Err(internal("Receipt deleted messages"));}
    for(old,new)in a.iter().zip(b){
        if old==new{continue;}
        if !affected.contains(old["actionExecution"]["reviewId"].as_str().unwrap_or(""))
            ||!equal_except(old,new,&["text","actionExecution","updatedAt"]){return Err(internal("Receipt changed private message evidence"));}
    }
    for message in &b[a.len()..]{if message["role"]!="assistant"||message["serverActionExecution"]!=true
        ||!affected.contains(message["actionExecution"]["reviewId"].as_str().unwrap_or("")){return Err(internal("Receipt appended unrelated message"));}}
    Ok(())
}
fn validate_operator_delta(before:&Value,after:&Value,body:&Value)->ApiResult<()> {
    if before==after{return Ok(());} // A durable request replay never appends.
    let refs=body["proposals"].as_array().ok_or_else(||internal("Operator review references missing"))?;
    let old_jobs=rows(before,"jobs")?;let jobs=rows(after,"jobs")?;
    if jobs.len()!=old_jobs.len()+1{return Err(internal("Operator review requires one completed job"));}
    let job=&jobs[old_jobs.len()];let preview=&job["operatorReviewPreview"];
    let entries=preview["entries"].as_array().ok_or_else(||internal("Operator preview evidence missing"))?;
    if !preview.is_object(){return Err(internal("Operator preview invalid"));}
    if refs.is_empty()||refs.len()>100||entries.len()!=refs.len()||rows(before,"proposals")?.len()!=refs.len()
        ||job["refId"]!=body["requestId"]||job["editorialReferences"]!=body["proposals"]
        ||preview["version"]!=1||preview["contract"]!=crate::operator_editorial::CONTRACT
        ||preview["method"]!=crate::operator_editorial::METHOD||preview["account"]!=before["account"]
        ||preview["connectorBinding"]!=before["connectorBinding"]||preview["proposals"]!=body["proposals"]
        ||!preview["reviewAuthorityDigest"].as_str().is_some_and(|digest|digest.len()==64&&digest.bytes().all(|b|b.is_ascii_digit()||(b'a'..=b'f').contains(&b)))
        ||preview["previewDigest"]!=body["previewDigest"]||preview["previewDigest"]!=crate::operator_editorial::preview_digest(preview)
        ||job["operatorId"].as_str().is_none()||job["operatorId"]!=preview["reviewedBy"]["id"]||job["finishedAt"].as_str().is_none()
        ||job["editorialOutcome"]!=json!({"accepted":refs,"reused":[],"held":[]})||job["result"]!=job["editorialOutcome"] {
        return Err(internal("Operator review job authority differs"));
    }
    let mut seen=HashSet::new();let mut receipt_hashes=Vec::new();
    let source_context=crate::prepare_bundle::EvidenceContext::new(before);
    for(index,(reference,entry))in refs.iter().zip(entries).enumerate(){
        let key=text(reference,"id")?;
        if !seen.insert(key){return Err(internal("Operator review duplicated a proposal"));}
        let a=crate::row(before,"proposals",key)?;let b=crate::row(after,"proposals",key)?;
        if entry["candidate"]["operatorEvidenceContract"]==crate::editorial_review::OPERATOR_EVIDENCE_CONTRACT {
            let actual=source_context.evidence_for_item(text(a,"itemId")?).map_err(internal)?;
            // Comparison may ignore acquisition churn for an independent
            // decision; the retained observation must still be exact and real.
            if entry["candidate"]["acquisitionMediaDigest"]!=crate::editorial_review::operator_acquisition_digest(&entry["evidence"])
                ||entry["candidate"]["acquisitionMediaDigest"]!=crate::editorial_review::operator_acquisition_digest(&actual){
                return Err(internal("Operator preview acquisition evidence differs from source"));
            }
        }
        if !a["editorialReviews"].is_null()&&!a["editorialReviews"].is_array(){return Err(internal("Operator review cannot replace malformed history"));}
        let old=a["editorialReviews"].as_array().map(Vec::as_slice).unwrap_or(&[]);
        let new=b["editorialReviews"].as_array().ok_or_else(||internal("Operator review receipt history missing"))?;
        let receipt=&b["editorialReview"];let source=&receipt["source"];
        let mut unhashed=receipt.clone();unhashed.as_object_mut().ok_or_else(||internal("Operator review receipt missing"))?.remove("receiptSha256");
        if new.len()!=old.len()+1||!new.starts_with(old)||new.last()!=Some(receipt)
            ||a["revision"]!=reference["revision"]||receipt["candidate"]!=entry["candidate"]
            ||receipt["candidate"]["proposalId"]!=a["id"]||receipt["candidate"]["proposalRevision"]!=a["revision"]
            ||receipt["candidate"]["itemId"]!=a["itemId"]||receipt["candidate"]["textSha256"]!=crate::editorial_review::hash_text(a["text"].as_str().unwrap_or(""))
            ||receipt["mediaDependency"]!=body["operatorReview"]["entries"][index]["mediaDependency"]
            ||!crate::editorial_review::operator_candidates_equal(&body["operatorReview"]["entries"][index]["candidate"],&entry["candidate"],&receipt["mediaDependency"])
            ||receipt["version"]!=1||receipt["contract"]!="communityhero-editorial-v1"||receipt["decision"]!="accept"||!receipt["proposedText"].is_null()
            ||receipt["account"]!=before["account"]||receipt["connectorBinding"]!=before["connectorBinding"]||receipt["reviewedAt"]!=job["finishedAt"]
            ||receipt["receiptSha256"]!=crate::editorial_review::hash_text(&unhashed.to_string())
            ||source["kind"]!="operator_assisted_review"||source["version"]!=1||source["contract"]!=crate::operator_editorial::CONTRACT
            ||source["method"]!=crate::operator_editorial::METHOD||source["requestId"]!=body["requestId"]||source["previewDigest"]!=body["previewDigest"]
            ||source["reviewedBy"]!=preview["reviewedBy"]||source["reviewAuthorityDigest"]!=preview["reviewAuthorityDigest"]
            ||source["researchManifest"]!=entry["evidence"]["editorialResearchManifest"] {
            return Err(internal("Operator review receipt changed protected authority/history"));
        }
        // The native proof is derived from the immutable BEFORE source and
        // already checked actor/preview/candidate/review receipt. Allowing its
        // field in the structural diff alone must never mint material proof.
        crate::preparation_materials::validate_operator_material_delta(before,a,b,preview,&entry["candidate"])
            .map_err(internal)?;
        receipt_hashes.push(receipt["receiptSha256"].clone());
    }
    let old_audit=rows(before,"audit")?;let audit=rows(after,"audit")?;
    if audit.len()!=old_audit.len()+2{return Err(internal("Operator review audit pair missing"));}
    let reviewed=&audit[old_audit.len()];let committed=&audit[old_audit.len()+1];
    if reviewed["action"]!="operator_editorial.reviewed"||reviewed["refId"]!=job["id"]||reviewed["actor"]!=preview["reviewedBy"]
        ||reviewed["method"]!=crate::operator_editorial::METHOD||reviewed["previewDigest"]!=body["previewDigest"]||reviewed["receiptSha256"]!=json!(receipt_hashes)
        ||committed["action"]!=crate::local_admission::ACTION||committed["kind"]!="editorial"||committed["requestId"]!=body["requestId"]
        ||committed["account"]!=crate::accounts::Profile::from_workspace(before)?.key()
        ||committed["id"]!=crate::local_admission::receipt_id("editorial",body["requestId"].as_str().unwrap_or(""))
        ||committed["actorId"]!=job["operatorId"]||committed["result"]["jobId"]!=job["id"] {
        return Err(internal("Operator review audit authority differs"));
    }
    Ok(())
}
fn validate_delta(before:&Value,after:&Value,scope:AdmissionScope<'_>)->ApiResult<()> {
    crate::continuous_preparation::validate_change(before,after)?;
    crate::connection_gate::validate_change(before,after)?;
    crate::external_reconciliation::validate_change(before,after)?;
    crate::preparation_reservations::validate_change(before,after)?;
    crate::retained_paid_recovery_registry::validate_change(before,after,false)?;
    if !equal_except(before,after,&["jobs","proposals","approvals","operations","feedback","audit","conversations"]) {
        return Err(internal("Admission changed protected source or company state"));
    }
    for table in ["jobs","proposals","approvals","operations","feedback","audit","conversations"] {
        let old=rows(before,table)?;let new=rows(after,table)?;
        let mut ids=HashSet::new();
        for row in new {
            if !row.is_object()||!ids.insert(text(row,"id")?) {return Err(internal("Invalid admission record identity"));}
            for (_,field) in projection(table) {if !row[*field].is_null()&&!row[*field].is_string(){return Err(internal("Invalid admission projection"));}}
        }
        if new.len()<old.len(){return Err(internal("Admission deleted history"));}
        for(a,b)in old.iter().zip(new) {
            if a["id"]!=b["id"] {return Err(internal("Admission reordered history"));}
            let allowed:&[&str]=match(scope,table) {
                (AdmissionScope::EditorialJob(key),"jobs") if a["id"]==key=>&["editorialBatches"],
                (AdmissionScope::EditorialJob(_),"proposals")=>&["editorialReview","editorialReviews","editorialModelMaterialReceipt"],
                (AdmissionScope::OperatorEditorial(_),"proposals")=>&["editorialReview","editorialReviews","operatorMaterialReceipt"],
                (AdmissionScope::EditorialRepair {..},"proposals")=>&["text","revision","status","history","editorialReview","editorialRepair"],
                (AdmissionScope::Approval(_)|AdmissionScope::Execute{..},"proposals")=>&["status"],
                (AdmissionScope::Execute{approval,..},"approvals") if a["id"]==approval=>&["status"],
                (AdmissionScope::ExecutionReceipts(_),"conversations")=>&["messages","actionReviews"],
                _=>&[],
            };
            if !equal_except(a,b,allowed){return Err(internal("Admission changed protected history"));}
            if a!=b {
                match(scope,table) {
                    (AdmissionScope::EditorialJob(_),"jobs")=>validate_editorial_journal(a,b)?,
                    (AdmissionScope::EditorialJob(_)|AdmissionScope::OperatorEditorial(_),"proposals")=>{
                        let old=a["editorialReviews"].as_array().map(Vec::as_slice).unwrap_or(&[]);
                        let new=b["editorialReviews"].as_array().ok_or_else(||internal("Editorial receipt history missing"))?;
                        if !new.starts_with(old)||!new.contains(&b["editorialReview"]){return Err(internal("Editorial receipt history changed"));}
                        if let AdmissionScope::EditorialJob(key)=scope {
                            crate::preparation_materials::validate_editorial_material_delta(before,key,a,b).map_err(internal)?;
                        }
                    },
                    (AdmissionScope::ExecutionReceipts(_),"conversations")=>validate_receipt_chat(a,b,approval_key(scope,rows(before,"jobs")?,rows(before,"operations")?).as_deref())?,
                    (AdmissionScope::Approval(_),"proposals") if b["status"]!="approved"=>return Err(internal("Invalid approval proposal transition")),
                    (AdmissionScope::Execute{..},"proposals") if a["status"]!="approved"||b["status"]!="dispatching"=>return Err(internal("Invalid execute proposal transition")),
                    (AdmissionScope::Execute{..},"approvals") if a["status"]!="approved"||b["status"]!="consumed"=>return Err(internal("Invalid approval consumption")),
                    _=>(),
                }
            }
        }
        let appended=&new[old.len()..];
        let permitted=match(scope,table) {
            (AdmissionScope::EditorialSchedule(_),"jobs")=>appended.len()<=1&&appended.iter().all(|j|j["kind"]=="editorial_review"&&j["status"]=="running"),
            (AdmissionScope::OperatorEditorial(_),"jobs")=>appended.len()<=1&&appended.iter().all(|j|j["kind"]=="editorial_review"&&j["status"]=="completed"&&j["purpose"]=="operator_assisted_review"),
            (AdmissionScope::Approval(_),"approvals")=>appended.len()<=1&&appended.iter().all(|a|a["status"]=="approved"),
            (AdmissionScope::Approval(_),"feedback")=>appended.iter().all(|e|e["kind"]=="review_confirmed"&&e["accountId"]==before["account"]
                &&rows(after,"approvals").is_ok_and(|a|a.iter().any(|a|a["id"]==e["approvalId"]&&a["proposals"].as_array().into_iter().flatten().any(|p|p["id"]==e["proposalId"]&&p["proposal"]["itemId"]==e["itemId"])))),
            (AdmissionScope::Execute{approval,..},"jobs")=>appended.len()<=1&&appended.iter().all(|j|j["kind"]=="execute"&&j["refId"]==approval&&j["status"]=="running"),
            (AdmissionScope::Execute{..},"operations")=>true, // checked against selected proposals below
            (AdmissionScope::Execute{..},"audit")=>appended.iter().all(|a|matches!(a["action"].as_str(),Some("local_admission.committed"|"local_admission.rejected"|"operation.admitted"))),
            (AdmissionScope::EditorialSchedule(_)|AdmissionScope::Approval(_),"audit")=>appended.iter().all(|a|matches!(a["action"].as_str(),Some("local_admission.committed"|"approval.created"|"operation.admitted"))),
            (AdmissionScope::OperatorEditorial(_),"audit")=>appended.iter().all(|a|matches!(a["action"].as_str(),Some("local_admission.committed"|"operator_editorial.reviewed"))),
            (AdmissionScope::EditorialRepair {..},"audit")=>appended.len()<=1&&appended.iter().all(|a|a["action"]==crate::local_admission::ACTION&&a["kind"]=="editorial-repair"),
            _=>appended.is_empty(),
        };
        // Operations append is checked against selected proposals; existing
        // blockers are immutable and remain visible to the reducer.
        let permitted=if let(AdmissionScope::Execute{approval,..},"operations")=(scope,table) {
            appended.len()<=100&&appended.iter().all(|o|o["approvalId"]==approval&&o["status"]=="dispatching"
                &&rows(before,"proposals").is_ok_and(|p|p.iter().any(|p|p["id"]==o["proposalId"]&&p["itemId"]==o["itemId"])))
        }else{permitted};
        if !permitted {return Err(internal("Admission appended unrelated history"));}
    }
    if let AdmissionScope::OperatorEditorial(body)=scope{validate_operator_delta(before,after,body)?;}
    if let AdmissionScope::EditorialRepair {body,..}=scope{crate::editorial_repair::validate_delta(before,after,body)?;}
    if let AdmissionScope::Execute {body,approval}=scope {
        if let Some(key)=body["requestId"].as_str() {
            let previous=crate::local_admission::find_rejection(before,"execute",key)?;
            let current=crate::local_admission::find_rejection(after,"execute",key)?;
            if current!=previous {
                let latest=current.ok_or_else(||internal("Admission rejection history disappeared"))?;
                if !equal_except(before,after,&["audit"])||rows(after,"audit")?.len()!=rows(before,"audit")?.len()+1
                    ||rows(after,"audit")?.last()!=Some(latest)||latest["approvalId"]!=approval
                    ||crate::local_admission::find_receipt(after,"execute",key)?.is_some() {
                    return Err(internal("Rejected local admission changed effects or appended unrelated history"));
                }
            }
        }
    }
    Ok(())
}

async fn capture_rows(connection:&mut PgConnection,table:&str,condition:&str,ids:&[String])->ApiResult<Vec<sqlx::postgres::PgRow>> {
    let statement=format!("SELECT id,payload::text{} FROM communityhero.{table} WHERE workspace_id=$1 AND {condition} ORDER BY ordinal",
        projection(table).iter().map(|(c,_)|format!(",{c}")).collect::<String>());
    Ok(sqlx::query(sqlx::AssertSqlSafe(statement.as_str())).bind(WORKSPACE).bind(ids).fetch_all(connection).await?)
}
fn decode_rows(table:&str,records:Vec<sqlx::postgres::PgRow>)->ApiResult<Vec<Value>> {
    records.into_iter().map(|row| {
        let value=parse(row.try_get::<&str,_>("payload")?)?;
        if row.try_get::<&str,_>("id")?!=text(&value,"id")?{return Err(internal("Admission record identity mismatch"));}
        for(column,field)in projection(table) {
            if(!value[*field].is_null()&&!value[*field].is_string())||row.try_get::<Option<String>,_>(*column)?.as_deref()!=value[*field].as_str(){return Err(internal("Admission record projection mismatch"));}
        }
        Ok(value)
    }).collect()
}
async fn load(connection:&mut PgConnection,table:&str,condition:&str,ids:&[String])->ApiResult<Vec<Value>> {
    decode_rows(table,capture_rows(connection,table,condition,ids).await?)
}

struct CapturedScope {
    value:Value,
    rows:Vec<(&'static str,Vec<sqlx::postgres::PgRow>)>,
    controls:Option<super::scope_context::CapturedControls>,
}
impl CapturedScope {
    fn decode(self)->ApiResult<Value> {
        let mut value=self.value;
        for(table,records)in self.rows {value[table]=json!(decode_rows(table,records)?);}
        if let Some(controls)=self.controls {super::scope_context::decode(controls,&mut value)?;}
        Ok(value)
    }
}
async fn save(connection:&mut PgConnection,table:&str,value:&Value,append:bool)->ApiResult<()> {
    let columns=projection(table);
    let statement=if append {
        format!("INSERT INTO communityhero.{table}(workspace_id,id,payload,ordinal{}) SELECT $1,$2,$3::jsonb,COALESCE(MAX(ordinal),-1)+1{} FROM communityhero.{table} WHERE workspace_id=$1",
            columns.iter().map(|(c,_)|format!(",{c}")).collect::<String>(),(0..columns.len()).map(|n|format!(",${}",n+4)).collect::<String>())
    }else{format!("UPDATE communityhero.{table} SET payload=$3::jsonb{} WHERE workspace_id=$1 AND id=$2",columns.iter().enumerate().map(|(n,(c,_))|format!(",{c}=${}",n+4)).collect::<String>())};
    let mut query=sqlx::query(sqlx::AssertSqlSafe(statement.as_str())).bind(WORKSPACE).bind(text(value,"id")?).bind(value.to_string());
    for(_,field)in columns{query=query.bind(value[*field].as_str());}
    if query.execute(connection).await?.rows_affected()!=1{return Err(internal("Admission record disappeared"));}Ok(())
}

async fn expand_dependency_jobs(connection:&mut PgConnection,view:&mut Value)->ApiResult<()> {
    let mut previous=None;
    loop {
        let (ids,bundles,full)=super::source_snapshot::scoped_job_dependencies(rows(view,"jobs")?,rows(view,"proposals")?);
        let key=(ids.clone(),bundles.clone(),full);
        if previous.as_ref()==Some(&key){break;}
        previous=Some(key);
        let mut fetch=crate::performance::Span::new("source.snapshot.load.jobs.bodies.fetch");
        let records=sqlx::query("SELECT id,kind,ref_id,status,payload::text FROM communityhero.jobs WHERE workspace_id=$1 AND ($4::boolean OR id=ANY($2::text[]) OR payload->>'id'=ANY($2::text[]) OR payload#>>'{prepareBundle,id}'=ANY($3::text[])) ORDER BY ordinal")
            .bind(WORKSPACE).bind(ids).bind(bundles).bind(full).fetch_all(&mut *connection).await?;
        let mut bytes=0u64;for record in &records{bytes+=record.try_get::<&str,_>("payload")?.len() as u64;}
        fetch.measurements(crate::performance::StorageMeasurements{payload_read:crate::performance::ReadMeasurements{
            rows:Some(records.len() as u64),bytes:Some(bytes),statements:Some(1)},
            fallback_reason:full.then_some("source_scope_legacy_fallback"),..Default::default()});drop(fetch);
        let decode=crate::performance::Span::new("source.snapshot.load.jobs.bodies.decode");
        let mut jobs=Vec::with_capacity(records.len());
        for record in records {
            let job=parse(record.try_get::<&str,_>("payload")?)?;
            if record.try_get::<&str,_>("id")?!=text(&job,"id")?{return Err(internal("Admission dependency job identity mismatch"));}
            for (column,field) in projection("jobs") {
                if record.try_get::<Option<String>,_>(*column)?.as_deref()!=job[*field].as_str(){return Err(internal("Admission dependency job projection mismatch"));}
            }
            jobs.push(job);
        }
        view["jobs"]=json!(jobs);drop(decode);
    }
    Ok(())
}

async fn capture_scope(connection:&mut PgConnection,scope:AdmissionScope<'_>,locked:bool,defer_preview:bool)->ApiResult<CapturedScope> {
    // Only OperatorEditorial resolves references from its body, independently
    // of operation/job contents. Other admission scopes keep their old loader.
    if defer_preview&&(locked||!matches!(scope,AdmissionScope::OperatorEditorial(_))){return Err(internal("Invalid deferred admission scope"));}
    let mut deferred=Vec::new();
    let metadata_column=if pinned_research_only(scope){"(metadata-'preparationResearch')"}else{"metadata"};
    let statement=format!("SELECT account,{metadata_column}::text AS metadata,execution_enabled FROM communityhero.workspaces WHERE id=$1{}",if locked{" FOR UPDATE"}else{""});
    let row=sqlx::query(sqlx::AssertSqlSafe(statement.as_str())).bind(WORKSPACE).fetch_one(&mut *connection).await?;
    let mut before=parse(row.try_get::<&str,_>("metadata")?)?;
    if row.try_get::<bool,_>("execution_enabled")?||!before.is_object()||before["account"].as_str().is_none()
        ||row.try_get::<Option<String>,_>("account")?.as_deref()!=before["account"].as_str()||TABLES.iter().any(|t|before.get(*t).is_some()){return Err(internal("Admission workspace identity mismatch"));}
    for table in TABLES{before[table]=json!([]);}
    let mut root:Vec<_>=root_job(scope).into_iter().map(str::to_owned).collect();
    if let Some(ctx)=crate::conductor_authority::current_context(){if !root.contains(&ctx.run_id){root.push(ctx.run_id);}}
    let initial_jobs=load(connection,"jobs","(id=ANY($2::text[]) OR payload->>'id'=ANY($2::text[]))",&root).await?;
    // All admission operations are retained for recipient alias
    // and corrupted legacy binding quarantine; payloads are small
    // relative to feedback/job/approval histories in measured data.
    if matches!(scope,AdmissionScope::ExecutionReceipts(_)) {
        let operation:Vec<_>=initial_jobs.iter().filter(|j|j["kind"]=="reconcile").filter_map(|j|j["refId"].as_str().map(str::to_owned)).collect();
        before["operations"]=json!(load(connection,"operations","(id=ANY($2::text[]) OR payload->>'id'=ANY($2::text[]))",&operation).await?);
    }else if defer_preview {deferred.push(("operations",capture_rows(connection,"operations","($2::text[] IS NOT NULL)",&[]).await?));}
    else{before["operations"]=json!(load(connection,"operations","($2::text[] IS NOT NULL)",&[]).await?);}
    let approval=approval_key(scope,&initial_jobs,rows(&before,"operations")?);
    before["approvals"]=json!(load(connection,"approvals","(id=ANY($2::text[]) OR payload->>'id'=ANY($2::text[]))",&approval.iter().cloned().collect::<Vec<_>>()).await?);
    let selected=references(scope,&initial_jobs,rows(&before,"approvals")?).into_iter().collect::<Vec<_>>();
    before["proposals"]=json!(load(connection,"proposals","(id=ANY($2::text[]) OR payload->>'id'=ANY($2::text[]))",&selected).await?);
    let mut runs=generation_jobs(rows(&before,"proposals")?);runs.extend(root);
    let predicate=if matches!(scope,AdmissionScope::ExecutionReceipts(_)) {"(id=ANY($2::text[]) OR payload->>'id'=ANY($2::text[]) OR ((kind='execute' OR payload->>'kind'='execute') AND (ref_id=ANY($2::text[]) OR payload->>'refId'=ANY($2::text[]))))"}
        else if matches!(scope,AdmissionScope::EditorialSchedule(_)|AdmissionScope::OperatorEditorial(_)){"(id=ANY($2::text[]) OR payload->>'id'=ANY($2::text[]) OR kind IN ('media','media_audio','media_analysis','media_analysis_applicability') OR payload->>'kind' IN ('media','media_audio','media_analysis','media_analysis_applicability') OR ((kind='editorial_review' OR payload->>'kind'='editorial_review') AND (status IN ('running','queued') OR payload->>'status' IN ('running','queued'))))"}
        else{"(id=ANY($2::text[]) OR payload->>'id'=ANY($2::text[]) OR kind IN ('media','media_audio','media_analysis','media_analysis_applicability') OR payload->>'kind' IN ('media','media_audio','media_analysis','media_analysis_applicability'))"};
    if matches!(scope,AdmissionScope::ExecutionReceipts(_)){runs.extend(approval.iter().cloned());}
    let predicate=format!("({predicate} OR {} OR ((kind IN ('execute','reconcile') OR payload->>'kind' IN ('execute','reconcile')) AND (status IN ('running','queued','pending') OR payload->>'status' IN ('running','queued','pending'))))",super::preparation::retained_material_job_sql());
    let runs=runs.into_iter().collect::<Vec<_>>();
    // Decode only selected full jobs inside this same RR snapshot so exact
    // parent/bundle closure cannot race a later reader transaction. Other
    // source/operation collections still decode after reader release.
    before["jobs"]=json!(load(connection,"jobs",&predicate,&runs).await?);
    if let Some((kind,body))=request(scope) {if let Some(key)=body["requestId"].as_str() {
        // Include secondary discriminator matches as well as PK so
        // duplicate/corrupt receipt identity cannot become absence.
        let mut identities=vec![crate::local_admission::receipt_id(kind,key)];
        if kind=="execute" {identities.extend((1..=crate::local_admission::MAX_REJECTED_EVALUATIONS as u64).map(|n|crate::local_admission::negative_id(kind,key,n)));}
        let statement="SELECT id,payload::text,action,ref_id FROM communityhero.audit WHERE workspace_id=$1 AND (id=ANY($2::text[]) OR payload->>'id'=ANY($2::text[]) OR ((action IN ('local_admission.committed','local_admission.rejected') OR payload->>'action' IN ('local_admission.committed','local_admission.rejected')) AND payload->>'kind'=$3 AND payload->>'requestId'=$4)) ORDER BY ordinal LIMIT 18";
        let records=sqlx::query(statement).bind(WORKSPACE).bind(identities).bind(kind).bind(key).fetch_all(&mut *connection).await?;
        if records.len()>crate::local_admission::MAX_REJECTED_EVALUATIONS+1{return Err(internal("Admission receipt history exceeds bounded projection"));}
        let mut audit=Vec::new();for row in records{let value=parse(row.try_get::<&str,_>("payload")?)?;
            if row.try_get::<&str,_>("id")?!=text(&value,"id")?||row.try_get::<Option<String>,_>("action")?.as_deref()!=value["action"].as_str()||row.try_get::<Option<String>,_>("ref_id")?.as_deref()!=value["refId"].as_str(){return Err(internal("Admission receipt projection mismatch"));}audit.push(value);}
        before["audit"]=json!(audit);
    }}
    if matches!(scope,AdmissionScope::ExecutionReceipts(_)) {
        let keys:Vec<_>=approval.iter().cloned().collect();
        before["conversations"]=json!(load(connection,"conversations","EXISTS(SELECT 1 FROM jsonb_array_elements(CASE WHEN jsonb_typeof(payload->'actionReviews')='array' THEN payload->'actionReviews' ELSE '[]'::jsonb END) r WHERE r#>>'{execution,approvalId}'=ANY($2::text[]))",&keys).await?);
        before["operations"]=json!(load(connection,"operations","(approval_id=ANY($2::text[]) OR payload->>'approvalId'=ANY($2::text[]))",&keys).await?);
    }else{for table in ["posts","branches","items","materials","knowledge_entries","knowledge_versions"]{
        if defer_preview {deferred.push((table,capture_rows(connection,table,"($2::text[] IS NOT NULL)",&[]).await?));}
        else {before[table]=json!(load(connection,table,"($2::text[] IS NOT NULL)",&[]).await?);}
    }}
    let controls=if defer_preview {Some(super::scope_context::capture(connection).await?)} else {
    if !matches!(scope,AdmissionScope::ExecutionReceipts(_)){super::scope_context::load(connection,&mut before).await?;}
        None
    };
    if !matches!(scope,AdmissionScope::ExecutionReceipts(_)){expand_dependency_jobs(connection,&mut before).await?;}
    if pinned_research_only(scope){super::source_snapshot::load_pinned_research(connection,&mut before).await?;}
    Ok(CapturedScope{value:before,rows:deferred,controls})
}
async fn load_scope(connection:&mut PgConnection,scope:AdmissionScope<'_>,locked:bool)->ApiResult<Value> {
    capture_scope(connection,scope,locked,false).await?.decode()
}
async fn capture_operator_preview(reader:&PgPool,body:&Value)->ApiResult<CapturedScope> {
    let waiting=crate::performance::Span::new("operator_editorial.scoped.pool_wait");
    let mut tx=reader.begin().await?;
    drop(waiting);
    let reading=crate::performance::Span::new("operator_editorial.scoped.capture");
    let mut occupancy=reading.child("operator_editorial.scoped.reader.held",crate::performance::SpanClass::Occupancy);
    let captured=occupancy.scope(async {
        sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ READ ONLY").execute(&mut *tx).await?;
        let captured=capture_scope(&mut tx,AdmissionScope::OperatorEditorial(body),false,true).await?;
        tx.commit().await?;
        Ok::<_,crate::ApiError>(captured)
    }).await;
    occupancy.finish(if captured.is_ok(){"completed"}else{"failed"},None);drop(occupancy);
    let captured=captured?;
    // Every raw row comes from the same repeatable-read snapshot. The return
    // value owns no connection, so observers can run before bulk decoding.
    drop(reading);
    Ok(captured)
}

impl Database {
    /// The original editorial job and its selected source authority come from
    /// one snapshot. Pending jobs are roots, not inferred from settled receipts.
    pub(crate) async fn read_editorial_job_context(&self,key:&str)->ApiResult<Value> {
        let _span=crate::performance::Span::new("editorial.job.scoped.read");
        let scope=AdmissionScope::EditorialJob(key);
        let view=match self {
            Self::Sqlite(_)=>{
                let full=self.read().await?;
                let view=project(&full,scope)?;
                if selected_retained_recovery(&view){return Ok(full);}
                view
            },
            Self::Postgres{reader,..}=>{
                let mut tx=reader.begin().await?;
                sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ READ ONLY").execute(&mut *tx).await?;
                let view=load_scope(&mut tx,scope,false).await?;
                tx.commit().await?;
                view
            }
        };
        // Authentic retained recovery needs its original complete ledger. A
        // missing/pending editorial job alone never selects this fallback.
        if selected_retained_recovery(&view){self.read().await}else{Ok(view)}
    }
    /// Current source and control authority for an explicit review preview.
    /// This reader acquires no writer gate, workspace lock or admission receipt.
    pub(crate) async fn read_operator_editorial(&self,body:&Value)->ApiResult<Value> {
        let _span=crate::performance::Span::new("operator_editorial.scoped.read");
        let scope=AdmissionScope::OperatorEditorial(body);
        match self {
            Self::Sqlite(_)=>{
                let full=self.read().await?;let view=project(&full,scope)?;
                if selected_retained_recovery(&view){Ok(full)}else{Ok(view)}
            },
            Self::Postgres{reader,..}=>{
                let captured=capture_operator_preview(reader,body).await?;
                let _decoding=crate::performance::Span::new("operator_editorial.scoped.decode");
                let view=captured.decode()?;
                // Recovery must authenticate its complete original owner, audit
                // and operation ledger. Re-read one full coherent snapshot.
                if selected_retained_recovery(&view){self.read().await}else{Ok(view)}
            }
        }
    }
    pub(crate) async fn change_admission_observed<T>(&self,scope:AdmissionScope<'_>,f:impl FnOnce(&mut Value)->ApiResult<T>)->ApiResult<(T,bool)> {
        let _total=crate::performance::Span::new("admission.scoped.total");
        match self {
            Self::Sqlite(_)=>self.change_observed(|workspace| {
                let before=project(workspace,scope)?;
                if selected_retained_recovery(&before){return f(workspace);}
                let mut after=before.clone();let result=f(&mut after)?;validate_delta(&before,&after,scope)?;
                for table in TABLES {let old=rows(&before,table)?;for(index,value)in rows(&after,table)?.iter().enumerate() {
                    if old.get(index)==Some(value){continue;}
                    if index>=old.len() {
                        if rows(workspace,table)?.iter().any(|v|v["id"]==value["id"]){return Err(internal("Admission append reused identity"));}
                        crate::list_mut(workspace,table).push(value.clone());
                    }else{*crate::row_mut(workspace,table,text(value,"id")?)?=value.clone();}
                }}Ok(result)
            }).await,
            Self::Postgres{writer,..}=> {
                let waiting=crate::performance::Span::new("admission.scoped.pool_wait");let mut tx=writer.begin().await?;drop(waiting);
                let loading=crate::performance::Span::new("admission.scoped.load");
                let before=load_scope(&mut tx,scope,true).await?;
                if selected_retained_recovery(&before){
                    // Never merge recovery changes from a filtered projection.
                    // Release this read and revalidate the unchanged request in
                    // the ordinary complete writer transaction before mutation.
                    tx.rollback().await?;drop(loading);
                    return self.change_observed(f).await;
                }
                drop(loading);let domain=crate::performance::Span::new("admission.scoped.clone_and_domain");let mut after=before.clone();let result=f(&mut after)?;drop(domain);
                validate_delta(&before,&after,scope)?;if before==after{tx.commit().await?;return Ok((result,false));}
                let _persist=crate::performance::Span::new("admission.scoped.persist_and_commit");
                for table in ["proposals","approvals","operations","jobs","feedback","audit","conversations"] {let old=rows(&before,table)?;for(index,value)in rows(&after,table)?.iter().enumerate(){if old.get(index)!=Some(value){save(&mut tx,table,value,index>=old.len()).await?;}}}
                tx.commit().await?;Ok((result,true))
            }
        }
    }
}

#[cfg(test)]
#[path="storage_hot_admission_tests.rs"]
pub(crate) mod tests;

#[cfg(test)]
#[path="storage_operator_preview_tests.rs"]
mod preview_tests;

#[cfg(test)]
#[path="retained_paid_recovery_downstream_tests.rs"]
mod retained_recovery_tests;

#[cfg(test)]
#[path="storage_hot_media_analysis_tests.rs"]
mod media_analysis_tests;
