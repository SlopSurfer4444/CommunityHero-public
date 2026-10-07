//! Atomic final admission for explicit engine preparation. Source and operation
//! evidence stay complete; conversation, approval, audit and feedback histories
//! and completed unrelated jobs never enter the PostgreSQL writer projection.
//! This is not an approval or dispatch scope. SQLite still stores one document.
use super::*;

pub(super) fn equal_except(a: &Value, b: &Value, keys: &[&str]) -> bool {
    match (a.as_object(), b.as_object()) {
        (Some(a), Some(b)) => a.iter().filter(|(k,_)| !keys.contains(&k.as_str()))
            .eq(b.iter().filter(|(k,_)| !keys.contains(&k.as_str()))),
        _ => false,
    }
}

fn validate_admission(before: &Value, after: &Value, job_id: &str) -> ApiResult<()> {
    if !equal_except(before, after, &["items", "proposals", "jobs", "preparationResearch"]) {
        return Err(internal("Preparation admission changed source or protected history"));
    }
    let old_job = crate::row(before, "jobs", job_id)?;
    let new_job = crate::row(after, "jobs", job_id)?;
    let old_groups=old_job["preparationStages"]["groupAdmission"].as_array();
    let new_groups=new_job["preparationStages"]["groupAdmission"].as_array();
    if old_groups.is_some()!=new_groups.is_some()||old_groups.zip(new_groups).is_some_and(|(a,b)|a.len()!=b.len()) {
        return Err(internal("Preparation admission group inventory changed"));
    }
    let mut newly_admitted=HashSet::new();
    for (old,new) in old_groups.into_iter().flatten().zip(new_groups.into_iter().flatten()){
        if !equal_except(old,new,&["status","admission"]){return Err(internal("Preparation group source binding changed"));}
        if old==new {continue;}
        if old["status"]!="pending"||!old["admission"].is_null()
            || !matches!(new["status"].as_str(),Some("admitted"|"stale"))
            || !new["admission"].is_object()
            || (new["status"]=="stale")!=(new["admission"]["status"]=="stale"){
            return Err(internal("Preparation group transition is invalid"));
        }
        let members=new["itemIds"].as_array().ok_or_else(||internal("Preparation group members missing"))?;
        for candidate in new["admission"]["candidates"].as_array().ok_or_else(||internal("Preparation group candidates missing"))? {
            let id=text(candidate,"itemId")?;
            if !members.iter().any(|member|member==id)
                ||!matches!(candidate["status"].as_str(),Some("review"|"rejected")){
                return Err(internal("Preparation group admitted a foreign recipient"));
            }
            if candidate["status"]=="review"&&!newly_admitted.insert(id.to_owned()){
                return Err(internal("Preparation group duplicated an admitted recipient"));
            }
        }
    }
    if old_job["kind"] != "assistant" || old_job["status"] != "running"
        || old_job["purpose"] != "engine_prepare"
        || !old_job["prepareOutcome"].is_null()
        || !old_job["preparationStages"]["review"].is_null() {
        return Err(internal("Preparation admission requires an unadmitted running engine job"));
    }
    let recipients = old_job["prepareBundle"]["itemIds"].as_array()
        .filter(|ids| !ids.is_empty() && ids.len() <= 100)
        .ok_or_else(|| internal("Preparation admission recipients missing"))?;
    let mut recipient_ids = HashSet::new();
    for recipient in recipients {
        let id = recipient.as_str().filter(|id| !id.is_empty())
            .ok_or_else(|| internal("Invalid preparation admission recipient"))?;
        if !recipient_ids.insert(id) { return Err(internal("Duplicate preparation admission recipient")); }
        crate::row(before, "items", id)?;
    }
    let old_items = rows(before, "items")?;
    let new_items = rows(after, "items")?;
    if old_items.len() != new_items.len() { return Err(internal("Preparation admission changed item inventory")); }
    for (old, new) in old_items.iter().zip(new_items) {
        if old == new { continue; }
        if !recipient_ids.contains(text(old, "id")?)
            || !equal_except(old, new, &["workflow", "revision"])
            || old["workflow"] != "attention" || new["workflow"] != "prepared"
            || old["revision"].as_u64().and_then(|n| n.checked_add(1)) != new["revision"].as_u64() {
            return Err(internal("Preparation admission changed protected item state"));
        }
    }
    let old_proposals = rows(before, "proposals")?;
    let new_proposals = rows(after, "proposals")?;
    if !new_proposals.starts_with(old_proposals) {
        return Err(internal("Preparation admission rewrote an existing proposal"));
    }
    let mut proposal_ids: HashSet<&str> = old_proposals.iter().map(|p| text(p, "id")).collect::<ApiResult<_>>()?;
    let mut proposed_recipients = HashSet::new();
    for proposal in &new_proposals[old_proposals.len()..] {
        let recipient = text(proposal, "itemId")?;
        let item = crate::row(after, "items", recipient)?;
        if !proposal_ids.insert(text(proposal, "id")?) || !recipient_ids.contains(recipient)
            || !proposed_recipients.insert(recipient)
            || proposal["status"] != "draft" || proposal["revision"] != 1
            || proposal["itemRevision"] != item["revision"]
            || proposal["prepareRunId"] != job_id
            || proposal["prepareBundleId"] != old_job["prepareBundle"]["id"]
            || proposal["prepareBundleDigest"] != old_job["prepareBundle"]["digest"]
            || !proposal["sourceContextDigest"].is_string()
            || proposal["sourceContextDigest"] != proposal["reviewContextDigest"] {
            return Err(internal("Preparation admission appended an invalid proposal"));
        }
        for (_, field) in projection("proposals") {
            if !proposal[*field].is_null() && !proposal[*field].is_string() {
                return Err(internal("Invalid preparation proposal projection"));
            }
        }
    }
    for (old, new) in old_items.iter().zip(new_items) {
        if old != new && !proposed_recipients.contains(text(old, "id")?) {
            return Err(internal("Preparation admission changed an item without a proposal"));
        }
    }
    if new_groups.is_some() && proposed_recipients!=newly_admitted.iter().map(String::as_str).collect() {
        return Err(internal("Preparation group candidate/proposal mismatch"));
    }
    let old_jobs = rows(before, "jobs")?;
    let new_jobs = rows(after, "jobs")?;
    if old_jobs.len() != new_jobs.len() || old_jobs.iter().zip(new_jobs)
        .any(|(a,b)| if a["id"] == job_id { b["id"] != job_id } else { a != b })
        || !equal_except(old_job, new_job, &["runMetadata", "prepareOutcome", "preparationStages"])
        || without_group_and_review(old_job)?["preparationStages"]
            != without_group_and_review(new_job)?["preparationStages"] {
        return Err(internal("Preparation admission changed protected job state"));
    }
    if !new_job["prepareOutcome"].is_null() && new_groups.into_iter().flatten().any(|g|g["status"]=="pending") {
        return Err(internal("Preparation outcome left an unsettled group"));
    }
    let old_archive = before.get("preparationResearch").map(|_| rows(before, "preparationResearch"))
        .transpose()?.map(Vec::as_slice).unwrap_or(&[]);
    let new_archive = after.get("preparationResearch").map(|_| rows(after, "preparationResearch"))
        .transpose()?.map(Vec::as_slice).unwrap_or(&[]);
    if !new_archive.starts_with(old_archive) || new_archive.len() > old_archive.len() + 1 {
        return Err(internal("Preparation admission rewrote research history"));
    }
    let review = &new_job["preparationStages"]["review"];
    let appended = new_archive.get(old_archive.len());
    if review.is_null() != appended.is_none() {
        return Err(internal("Preparation review and research archive must commit together"));
    }
    if let Some(archive) = appended {
        if old_job["preparationStages"]["first"]["reviewRequired"] != true
            || review["status"] != "completed" || !review["result"].is_object()
            || archive["id"] != format!("research:{job_id}") || archive["jobId"] != job_id
            || old_archive.iter().any(|a| a["id"] == archive["id"] || a["jobId"] == job_id)
            || archive["account"] != old_job["prepareBundle"]["request"]["account"]
            || archive["connectorBinding"] != old_job["prepareBundle"]["request"]["connectorBinding"]
            || archive["prepareBundleId"] != old_job["prepareBundle"]["id"]
            || archive["prepareBundleDigest"] != old_job["prepareBundle"]["digest"]
            || archive["trust"] != "source_only" || archive["activePolicy"] != false
            || archive["review"] != *review
            || archive["checksum"] != crate::research_cache::checksum(archive) {
            return Err(internal("Preparation admission appended invalid research evidence"));
        }
    }
    crate::db_guards::validate_change(before, after)
}

fn without_group_and_review(job:&Value)->ApiResult<Value>{
    let mut value=without_stage(job,"review")?;
    value["preparationStages"].as_object_mut()
        .ok_or_else(||internal("Preparation stages missing"))?.remove("groupAdmission");
    Ok(value)
}

impl Database {
    pub(crate) async fn change_preparation_admission_observed<T>(
        &self, job_id: &str, f: impl FnOnce(&mut Value) -> ApiResult<T>,
    ) -> ApiResult<(T, bool)> {
        match self {
            Self::Sqlite(_) => self.change_observed(|workspace| {
                let before = projection_for(workspace, Some(job_id))?;
                let mut after = before.clone();
                let domain = crate::performance::Span::job("preparation.admission.domain", job_id);
                let result = f(&mut after)?;
                drop(domain);
                validate_admission(&before, &after, job_id)?;
                merge_claim_delta(workspace, &before, &after)?;
                if before.get("preparationResearch") != after.get("preparationResearch") {
                    workspace["preparationResearch"] = after["preparationResearch"].clone();
                }
                Ok(result)
            }).await,
            Self::Postgres { writer, .. } => {
                let waiting = crate::performance::Span::job("preparation.admission.pool_wait", job_id);
                let mut tx = writer.begin().await?;
                drop(waiting);
                let load = crate::performance::Span::job("preparation.admission.load", job_id);
                // load_pg_claim holds the same workspace FOR UPDATE lock used by
                // every writer. No evidence is captured before acquiring it.
                let before = load_pg_claim(&mut tx, Some(job_id), false).await?;
                drop(load);
                let domain = crate::performance::Span::job("preparation.admission.clone_and_domain", job_id);
                let mut after = before.clone();
                let result = f(&mut after)?;
                drop(domain);
                let validation = crate::performance::Span::job("preparation.admission.validation", job_id);
                validate_admission(&before, &after, job_id)?;
                drop(validation);
                if before == after { tx.commit().await?; return Ok((result, false)); }
                let persist = crate::performance::Span::job("preparation.admission.persist_and_commit", job_id);
                for table in MUTABLE {
                    let old = rows(&before, table)?;
                    for (index, value) in rows(&after, table)?.iter().enumerate() {
                        if old.get(index) != Some(value) {
                            persist_claim_record(&mut tx, table, value, index >= old.len()).await?;
                        }
                    }
                }
                if before.get("preparationResearch") != after.get("preparationResearch") {
                    sqlx::query("UPDATE communityhero.workspaces SET metadata=jsonb_set(metadata,'{preparationResearch}',$2::jsonb,true) WHERE id=$1")
                        .bind(WORKSPACE).bind(after["preparationResearch"].to_string()).execute(&mut *tx).await?;
                }
                tx.commit().await?;
                drop(persist);
                Ok((result, true))
            }
        }
    }
}

#[cfg(test)]
#[path = "storage_preparation_admission_tests.rs"]
mod tests;
