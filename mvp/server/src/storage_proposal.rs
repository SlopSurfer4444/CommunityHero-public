//! Single local proposal creation, including immutable operator feedback, under
//! the existing writer and row lock. This scope never admits approvals/actions.
use super::*;
use super::admission::equal_except;

fn project_proposal(workspace: &Value) -> ApiResult<Value> {
    // No preparation job is requested, but all media/audio and active assistant
    // jobs remain visible to source/media validation.
    let mut view = projection_for(workspace, Some(""))?;
    view["feedback"] = workspace["feedback"].clone();
    Ok(view)
}

fn validate_proposal_change(before: &Value, after: &Value, target: &str) -> ApiResult<()> {
    validate_proposal_rows(before,after,&HashSet::from([target.to_owned()]),1,false)
}

fn validate_proposal_rows(before:&Value,after:&Value,targets:&HashSet<String>,maximum:usize,audit:bool)->ApiResult<()> {
    let mutable=if audit {&["items","proposals","feedback","audit"][..]} else {&["items","proposals","feedback"][..]};
    if !equal_except(before, after, mutable) {
        return Err(internal("Proposal creation changed source or authority"));
    }
    let old_items = rows(before,"items")?;
    let new_items = rows(after,"items")?;
    if old_items.len() != new_items.len() { return Err(internal("Proposal creation changed item inventory")); }
    for (old,new) in old_items.iter().zip(new_items) {
        if old == new { continue; }
        if !old["id"].as_str().is_some_and(|id|targets.contains(id)) || !equal_except(old,new,&["workflow","revision"])
            || old["workflow"] != "attention" || new["workflow"] != "prepared"
            || old["revision"].as_u64().and_then(|n|n.checked_add(1)) != new["revision"].as_u64() {
            return Err(internal("Proposal creation changed protected item state"));
        }
    }
    let old = rows(before,"proposals")?;
    let new = rows(after,"proposals")?;
    if !new.starts_with(old) || new.len() > old.len()+maximum {
        return Err(internal("Proposal creation rewrote proposal history"));
    }
    let appended=&new[old.len()..];
    let mut ids=old.iter().map(|p|text(p,"id")).collect::<ApiResult<HashSet<_>>>()?;
    for p in appended {
        let key = text(p,"id")?;
        let target=text(p,"itemId")?;
        let item = crate::row(after,"items",target)?;
        if !ids.insert(key) || !targets.contains(target)
            || p["status"] != "draft" || p["revision"] != 1 || p["itemRevision"] != item["revision"] {
            return Err(internal("Proposal creation appended an invalid proposal"));
        }
    }
    for (old,new) in old_items.iter().zip(new_items) {
        if old!=new && !appended.iter().any(|p|p["itemId"]==new["id"]) {
            return Err(internal("Proposal creation changed an item without a proposal"));
        }
    }
    let old_feedback = rows(before,"feedback")?;
    let new_feedback = rows(after,"feedback")?;
    if !new_feedback.starts_with(old_feedback) || new_feedback.len() > old_feedback.len()+maximum {
        return Err(internal("Proposal creation rewrote feedback history"));
    }
    let mut event_ids=old_feedback.iter().map(|v|text(v,"id")).collect::<ApiResult<HashSet<_>>>()?;
    for event in &new_feedback[old_feedback.len()..] {
        let key = text(event,"id")?;
        if !event_ids.insert(key) || !event["itemId"].as_str().is_some_and(|id|targets.contains(id))
            || event["kind"] != "action_selected" || event["accountId"] != before["account"]
            || !appended.iter().any(|p|event["proposalId"]==p["id"] && event["proposalRevision"]==p["revision"] && event["itemId"]==p["itemId"]) {
            return Err(internal("Proposal and feedback must commit together"));
        }
    }
    crate::db_guards::validate_change(before,after)
}

fn merge(workspace:&mut Value,before:&Value,after:&Value)->ApiResult<()> {
    merge_claim_delta(workspace,before,after)?;
    let old = rows(before,"feedback")?;
    for event in &rows(after,"feedback")?[old.len()..] {
        crate::list_mut(workspace,"feedback").push(event.clone());
    }
    Ok(())
}

impl Database {
    // Share the admitted single-proposal evidence capture without exposing the
    // preparation module's internals or changing its source/media contracts.
    pub(in crate::storage) fn operator_batch_projection(workspace:&Value)->ApiResult<Value> {
        projection_for(workspace,Some(""))
    }
    pub(in crate::storage) async fn load_operator_batch_projection(connection:&mut PgConnection)->ApiResult<Value> {
        load_pg_claim(connection,Some(""),false).await
    }
    pub(in crate::storage) fn validate_operator_batch_rows(before:&Value,after:&Value,targets:&HashSet<String>,maximum:usize)->ApiResult<()> {
        validate_proposal_rows(before,after,targets,maximum,true)
    }
    pub(in crate::storage) async fn persist_operator_batch_record(connection:&mut PgConnection,table:&str,value:&Value,append:bool)->ApiResult<()> {
        persist_claim_record(connection,table,value,append).await
    }
    pub(crate) async fn create_proposal_observed(&self, body:&Value,expected_runtime:&crate::runtime_lifecycle::RuntimeIdentity)->ApiResult<(Value,bool)> {
        let target = crate::required(body,"itemId")?;
        match self {
            Self::Sqlite(_) => self.change_observed(|workspace| {
                crate::runtime_lifecycle::current_owner(workspace,expected_runtime)?;
                let before = project_proposal(workspace)?;
                let mut after = before.clone();
                let domain = crate::performance::Span::new("proposal.clone_and_domain");
                let result = crate::create_proposal(&mut after,body)?;
                drop(domain);
                validate_proposal_change(&before,&after,target)?;
                merge(workspace,&before,&after)?;
                Ok(result)
            }).await,
            Self::Postgres { writer,.. } => {
                let waiting = crate::performance::Span::new("proposal.pool_wait");
                let mut tx = writer.begin().await?;
                drop(waiting);
                let load = crate::performance::Span::new("proposal.load");
                let mut before = load_pg_claim(&mut tx,Some(""),false).await?;
                crate::runtime_lifecycle::current_owner(&before,expected_runtime)?;
                // Exact event lookup stays workspace-wide, including foreign
                // items. Historical origin reconstruction retains every matching
                // presentation revision; unrelated feedback bodies stay cold.
                let records = sqlx::query("SELECT id,item_id,payload::text FROM communityhero.feedback WHERE workspace_id=$1 AND \
                    (id=$2 OR payload->>'id'=$2 OR ((item_id=$3 OR payload->>'itemId'=$3) \
                    AND payload->>'kind'='proposal_presented' AND payload->>'sourceProposalId'=$4)) ORDER BY ordinal")
                    .bind(WORKSPACE).bind(body["eventId"].as_str()).bind(target)
                    .bind(body["sourceProposalId"].as_str()).fetch_all(&mut *tx).await?;
                let mut feedback = Vec::with_capacity(records.len());
                for record in records {
                    let event = parse(record.try_get::<&str,_>("payload")?)?;
                    if record.try_get::<&str,_>("id")? != text(&event,"id")?
                        || (!event["itemId"].is_null() && !event["itemId"].is_string())
                        || record.try_get::<Option<String>,_>("item_id")?.as_deref() != event["itemId"].as_str() {
                        return Err(internal("Proposal feedback projection mismatch"));
                    }
                    feedback.push(event);
                }
                before["feedback"] = json!(feedback);
                drop(load);
                let domain = crate::performance::Span::new("proposal.clone_and_domain");
                let mut after = before.clone();
                let result = crate::create_proposal(&mut after,body)?;
                drop(domain);
                let validation = crate::performance::Span::new("proposal.validation");
                validate_proposal_change(&before,&after,target)?;
                drop(validation);
                if before == after { tx.commit().await?; return Ok((result,false)); }
                let persist = crate::performance::Span::new("proposal.persist_and_commit");
                for table in ["items","proposals","feedback"] {
                    let old = rows(&before,table)?;
                    for (index,value) in rows(&after,table)?.iter().enumerate() {
                        if old.get(index) != Some(value) {
                            persist_claim_record(&mut tx,table,value,index>=old.len()).await?;
                        }
                    }
                }
                tx.commit().await?;
                drop(persist);
                Ok((result,true))
            }
        }
    }
}

#[cfg(test)]
#[path = "storage_proposal_tests.rs"]
mod tests;
