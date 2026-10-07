//! Proposal-batch creation and immutable receipt readback under the existing
//! workspace writer lock. Shared source/authority remains complete; unrelated
//! discussion, approvals and historical feedback/receipts are not hydrated.
use super::{Database, WORKSPACE, parse, rows, text};
use crate::{ApiResult, internal, operator_batch};
use serde_json::{Value, json};
use sqlx::Row;
use std::collections::HashSet;
use sha2::Digest;

struct Inputs { targets:HashSet<String>, events:Vec<String>, origins:Vec<String>, count:usize }

// Invalid/ambiguous request discriminators retain the legacy full transaction.
// Domain validation, partial rejection and actor attribution remain unchanged.
fn inputs(body:&Value)->Option<Inputs> {
    let fields=body.as_object()?;
    if fields.len()!=2 {return None;}
    let key=body["requestId"].as_str()?;
    if key.is_empty()||key.len()>160||key.chars().any(char::is_control){return None;}
    let entries=body["proposals"].as_array().filter(|v|!v.is_empty()&&v.len()<=100)?;
    let mut result=Inputs{targets:HashSet::new(),events:vec![],origins:vec![],count:entries.len()};
    for entry in entries {
        entry.as_object()?;
        let target=entry["itemId"].as_str().filter(|s|!s.is_empty())?;
        result.targets.insert(target.to_owned());
        for (key,values) in [("eventId",&mut result.events),("sourceProposalId",&mut result.origins)] {
            if !entry[key].is_null() {
                let id=entry[key].as_str().filter(|s|!s.is_empty()&&s.len()<=160&&!s.chars().any(char::is_control))?;
                values.push(id.to_owned());
            }
        }
    }
    Some(result)
}

// A historical presentation or implicit draft origin may require a completed
// legacy job's full captured request, absent from the compact ownership view.
// Keep that existing full transaction until origin-job capture is proven.
fn needs_complete_origin(body:&Value,workspace:Option<&Value>)->bool {
    crate::list(body,"proposals").iter().any(|entry|!entry["sourceProposalId"].is_null()
        || workspace.is_some_and(|d|crate::list(d,"items").iter()
            .any(|item|item["id"]==entry["itemId"]&&!item["draftOrigin"].is_null())))
}

fn feedback_selected(event:&Value,input:&Inputs)->bool {
    event["id"].as_str().is_some_and(|id|input.events.iter().any(|key|key==id))
        || (event["itemId"].as_str().is_some_and(|id|input.targets.contains(id))
            && event["kind"]=="proposal_presented"
            && event["sourceProposalId"].as_str().is_some_and(|id|input.origins.iter().any(|key|key==id)))
}
fn audit_selected(record:&Value,key:&str)->bool {
    record["id"]==operator_batch::receipt_id(key)
        || (record["action"]=="proposal.batch_created"&&record["refId"]==key)
}
fn project(workspace:&Value,body:&Value,input:&Inputs)->ApiResult<Value> {
    let mut view=Database::operator_batch_projection(workspace)?;
    view["feedback"]=json!(rows(workspace,"feedback")?.iter().filter(|v|feedback_selected(v,input)).collect::<Vec<_>>());
    view["audit"]=json!(rows(workspace,"audit")?.iter().filter(|v|audit_selected(v,body["requestId"].as_str().unwrap())).collect::<Vec<_>>());
    Ok(view)
}

fn validate(before:&Value,after:&Value,body:&Value,actor:&crate::operator_auth::Actor,input:&Inputs,result:&Value)->ApiResult<()> {
    Database::validate_operator_batch_rows(before,after,&input.targets,input.count)?;
    let old=rows(before,"audit")?;let new=rows(after,"audit")?;
    if before==after {return Ok(());}
    if !new.starts_with(old)||new.len()!=old.len()+1 {return Err(internal("Proposal batch changed receipt history"));}
    let receipt=new.last().unwrap();
    let expected_hash=format!("{:x}",sha2::Sha256::digest(json!({"account":before["account"],"actorId":actor.id,"proposals":body["proposals"]}).to_string().as_bytes()));
    if receipt["id"]!=operator_batch::receipt_id(body["requestId"].as_str().unwrap())
        ||receipt["action"]!="proposal.batch_created"||receipt["refId"]!=body["requestId"]
        ||receipt["actorId"]!=actor.id||receipt["requestHash"]!=expected_hash||receipt["result"]!=*result {
        return Err(internal("Proposal batch receipt binding changed"));
    }
    Ok(())
}

fn merge(workspace:&mut Value,before:&Value,after:&Value)->ApiResult<()> {
    // These evidence arrays are complete, unlike the selected history arrays.
    workspace["items"]=after["items"].clone();workspace["proposals"]=after["proposals"].clone();
    for table in ["feedback","audit"] {
        let old=rows(before,table)?;
        workspace[table].as_array_mut().ok_or_else(||internal("Invalid durable batch history"))?
            .extend(rows(after,table)?[old.len()..].iter().cloned());
    }
    Ok(())
}

async fn load(connection:&mut sqlx::PgConnection,body:&Value,input:&Inputs)->ApiResult<Value> {
    let mut view=Database::load_operator_batch_projection(connection).await?;
    let targets=input.targets.iter().cloned().collect::<Vec<_>>();
    let feedback=sqlx::query("SELECT id,item_id,payload::text FROM communityhero.feedback WHERE workspace_id=$1 AND \
        (id=ANY($2::text[]) OR payload->>'id'=ANY($2::text[]) OR ((item_id=ANY($3::text[]) OR payload->>'itemId'=ANY($3::text[])) \
        AND payload->>'kind'='proposal_presented' AND payload->>'sourceProposalId'=ANY($4::text[]))) ORDER BY ordinal")
        .bind(WORKSPACE).bind(&input.events).bind(&targets).bind(&input.origins).fetch_all(&mut *connection).await?;
    let mut events=vec![];
    for row in feedback {
        let event=parse(row.try_get::<&str,_>("payload")?)?;
        if row.try_get::<&str,_>("id")?!=text(&event,"id")?
            ||(!event["itemId"].is_null()&&!event["itemId"].is_string())
            ||row.try_get::<Option<String>,_>("item_id")?.as_deref()!=event["itemId"].as_str(){return Err(internal("Proposal batch feedback projection mismatch"));}
        events.push(event);
    }
    view["feedback"]=json!(events);
    let key=body["requestId"].as_str().unwrap();
    let records=sqlx::query("SELECT id,action,ref_id,payload::text FROM communityhero.audit WHERE workspace_id=$1 AND \
        (id=$2 OR payload->>'id'=$2 OR ((action='proposal.batch_created' OR payload->>'action'='proposal.batch_created') \
        AND (ref_id=$3 OR payload->>'refId'=$3))) ORDER BY ordinal")
        .bind(WORKSPACE).bind(operator_batch::receipt_id(key)).bind(key).fetch_all(&mut *connection).await?;
    let mut audit=vec![];
    for row in records {
        let value=parse(row.try_get::<&str,_>("payload")?)?;
        if row.try_get::<&str,_>("id")?!=text(&value,"id")?
            ||["action","refId"].iter().any(|field|!value[*field].is_null()&&!value[*field].is_string())
            ||row.try_get::<Option<String>,_>("action")?.as_deref()!=value["action"].as_str()
            ||row.try_get::<Option<String>,_>("ref_id")?.as_deref()!=value["refId"].as_str(){return Err(internal("Proposal batch audit projection mismatch"));}
        audit.push(value);
    }
    view["audit"]=json!(audit);Ok(view)
}

#[cfg(test)]
#[path = "storage_operator_batch_tests.rs"]
mod tests;

impl Database {
    pub(crate) async fn create_operator_batch_observed(&self,body:&Value,actor:&crate::operator_auth::Actor,expected_runtime:&crate::runtime_lifecycle::RuntimeIdentity)->ApiResult<(Value,bool)> {
        let Some(input)=inputs(body) else {
            return self.change_observed(|d|{crate::runtime_lifecycle::current_owner(d,expected_runtime)?;operator_batch::create_proposals(d,body,actor)}).await;
        };
        if needs_complete_origin(body,None) {
            return self.change_observed(|d|{crate::runtime_lifecycle::current_owner(d,expected_runtime)?;operator_batch::create_proposals(d,body,actor)}).await;
        }
        match self {
            Self::Sqlite(_)=>self.change_observed(|workspace|{
                crate::runtime_lifecycle::current_owner(workspace,expected_runtime)?;
                if needs_complete_origin(body,Some(workspace)) {
                    return operator_batch::create_proposals(workspace,body,actor);
                }
                let before=project(workspace,body,&input)?;let mut after=before.clone();
                let result=operator_batch::create_proposals(&mut after,body,actor)?;
                validate(&before,&after,body,actor,&input,&result)?;
                if before!=after {merge(workspace,&before,&after)?;}
                Ok(result)
            }).await,
            Self::Postgres{writer,..}=>{
                let _total=crate::performance::Span::new("operator.proposal_batch.transaction");
                let mut tx=writer.begin().await?;
                let capture=crate::performance::Span::new("operator.proposal_batch.load");
                let before=load(&mut tx,body,&input).await?;drop(capture);
                crate::runtime_lifecycle::current_owner(&before,expected_runtime)?;
                if needs_complete_origin(body,Some(&before)) {
                    // No reducer or write ran. Release this read capture and
                    // recapture full truth in the original writer transaction.
                    tx.rollback().await?;drop(before);
                    return self.change_observed(|d|{crate::runtime_lifecycle::current_owner(d,expected_runtime)?;operator_batch::create_proposals(d,body,actor)}).await;
                }
                let domain=crate::performance::Span::new("operator.proposal_batch.clone_and_domain");
                let mut after=before.clone();let result=operator_batch::create_proposals(&mut after,body,actor)?;drop(domain);
                validate(&before,&after,body,actor,&input,&result)?;
                if before==after {tx.commit().await?;return Ok((result,false));}
                let persist=crate::performance::Span::new("operator.proposal_batch.persist_and_commit");
                for table in ["items","proposals","feedback","audit"] {
                    let old=rows(&before,table)?;
                    for (index,value) in rows(&after,table)?.iter().enumerate() {
                        if old.get(index)!=Some(value) {Database::persist_operator_batch_record(&mut tx,table,value,index>=old.len()).await?;}
                    }
                }
                tx.commit().await?;drop(persist);
                let teardown=crate::performance::Span::new("operator.proposal_batch.teardown");
                drop(after);drop(before);drop(teardown);
                Ok((result,true))
            }
        }
    }
    pub(crate) async fn read_operator_batch_receipt(
        &self,
        request_id: &str,
    ) -> ApiResult<Option<Value>> {
        match self {
            Self::Sqlite(_) => {
                Ok(operator_batch::find_receipt(&self.read().await?, request_id)?.cloned())
            }
            Self::Postgres { reader, .. } => {
                // The deterministic receipt ID uses audit's existing
                // (workspace_id,id) primary key, so timeout lookup sends one
                // compact row rather than the full workspace over the wire.
                let rows = sqlx::query(
                    "SELECT payload::text AS payload FROM communityhero.audit \
                    WHERE workspace_id=$1 AND id=$2",
                )
                .bind(WORKSPACE)
                .bind(operator_batch::receipt_id(request_id))
                .fetch_optional(reader)
                .await?;
                let receipt = rows
                    .map(|row| parse(row.try_get::<&str, _>("payload")?))
                    .transpose()?;
                if receipt.as_ref().is_some_and(|record| {
                    record["action"] != "proposal.batch_created" || record["refId"] != request_id
                }) {
                    return Err(internal("Proposal batch receipt ID collision"));
                }
                Ok(receipt)
            }
        }
    }
}
