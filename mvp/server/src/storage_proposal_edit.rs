//! A local draft edit loads its proposal, recipient and exact replay event.
//! It retains the leased writer and workspace lock; no external action is admitted.
use super::*;
use serde_json::json;

fn project(workspace:&Value,key:&str,body:&Value)->ApiResult<Value>{
    let proposal=crate::row(workspace,"proposals",key)?.clone();
    let item=crate::row(workspace,"items",text(&proposal,"itemId")?)?.clone();
    let mut view=metadata(workspace);
    view["items"]=json!([item]);view["proposals"]=json!([proposal]);
    view["feedback"]=json!(crate::list(workspace,"feedback").iter()
        .filter(|e|body["eventId"].as_str().is_some_and(|id|e["id"]==id)).cloned().collect::<Vec<_>>());
    Ok(view)
}

fn equal_except(before:&Value,after:&Value,allowed:&[&str])->bool{
    let(Some(before),Some(after))=(before.as_object(),after.as_object())else{return false};
    before.iter().filter(|(key,_)|!allowed.contains(&key.as_str()))
        .eq(after.iter().filter(|(key,_)|!allowed.contains(&key.as_str())))
}

fn validate_edit(before:&Value,after:&Value,key:&str,body:&Value)->ApiResult<()>{
    if !equal_except(before,after,&["proposals","feedback"]){return Err(internal("Proposal edit changed read-only context"));}
    let old=rows(before,"proposals")?;let new=rows(after,"proposals")?;
    if old.len()!=1||new.len()!=1||old[0]["id"]!=key||new[0]["id"]!=key{
        return Err(internal("Proposal edit changed its target inventory"));
    }
    let old_events=rows(before,"feedback")?;let new_events=rows(after,"feedback")?;
    if !new_events.starts_with(old_events)||new_events.len()>old_events.len()+1{
        return Err(internal("Proposal edit rewrote feedback history"));
    }
    if before==after{return Ok(());}
    let p=&old[0];let edited=&new[0];
    if !equal_except(p,edited,&["origin","history","text","status","revision"])
        || matches!(p["status"].as_str(),Some("dispatching"|"unknown"|"succeeded"))
        || edited["status"]!="draft"||edited["text"]!=body["text"]
        || p["revision"].as_u64().and_then(|v|v.checked_add(1))!=edited["revision"].as_u64(){
        return Err(internal("Proposal edit changed protected state"));
    }
    let origin=p.get("origin").cloned().unwrap_or_else(||p.clone());
    if edited["origin"]!=origin{return Err(internal("Proposal edit changed captured origin"));}
    let mut history=p["history"].as_array().cloned().unwrap_or_default();
    let mut historical=p.clone();historical.as_object_mut().unwrap().remove("history");history.push(historical);
    if edited["history"]!=json!(history)||new_events.len()!=old_events.len()+1{
        return Err(internal("Proposal edit must retain history and append feedback"));
    }
    let event=new_events.last().unwrap();
    let event_id=text(event,"id")?;
    if old_events.iter().any(|old|old["id"]==event_id)
        || body["eventId"].as_str().is_some_and(|id|event_id!=id)
        || event["eventId"]!=event_id||event["itemId"]!=p["itemId"]
        || event["accountId"]!=before["account"]||event["kind"]!="draft_saved"
        || event["proposalId"]!=key||event["proposalRevision"]!=edited["revision"]
        || event["origin"]!=origin||event["before"]!=p["text"]||event["after"]!=edited["text"]{
        return Err(internal("Proposal edit appended invalid feedback"));
    }
    Ok(())
}

async fn load(connection:&mut PgConnection,key:&str,body:&Value)->ApiResult<Value>{
    let record=sqlx::query("SELECT account,metadata::text,execution_enabled FROM communityhero.workspaces WHERE id=$1 FOR UPDATE")
        .bind(WORKSPACE).fetch_one(&mut *connection).await?;
    if record.try_get::<bool,_>("execution_enabled")?{return Err(internal("PostgreSQL pilot execution must remain disabled"));}
    let mut view=parse(record.try_get::<&str,_>("metadata")?)?;
    if !view.is_object()||view["account"].as_str().is_none()
        ||record.try_get::<Option<String>,_>("account")?.as_deref()!=view["account"].as_str(){
        return Err(internal("Workspace identity mismatch"));
    }
    for table in TABLES{if view.get(table).is_some(){return Err(internal("Workspace metadata contains entity collections"));}}
    let record=sqlx::query("SELECT p.id,p.item_id,p.status,p.payload::text AS proposal,i.id AS recipient_id,i.post_id,i.branch_id,i.payload::text AS recipient FROM communityhero.proposals p LEFT JOIN communityhero.items i ON i.workspace_id=p.workspace_id AND i.id=p.item_id WHERE p.workspace_id=$1 AND p.id=$2")
        .bind(WORKSPACE).bind(key).fetch_optional(&mut *connection).await?;
    let Some(record)=record else{return Err(crate::ApiError(crate::StatusCode::NOT_FOUND,"proposals record not found".into()));};
    let proposal=parse(record.try_get::<&str,_>("proposal")?)?;
    if record.try_get::<&str,_>("id")?!=text(&proposal,"id")?{return Err(internal("Proposal identity mismatch"));}
    for(column,field)in projection("proposals"){
        if (!proposal[*field].is_null()&&!proposal[*field].is_string())
            ||record.try_get::<Option<String>,_>(*column)?.as_deref()!=proposal[*field].as_str(){return Err(internal("Proposal relational projection mismatch"));}
    }
    let item=parse(record.try_get::<Option<&str>,_>("recipient")?.ok_or_else(||internal("Proposal recipient missing"))?)?;
    if record.try_get::<Option<&str>,_>("recipient_id")?!=Some(text(&item,"id")?)||proposal["itemId"]!=item["id"]{return Err(internal("Proposal recipient identity mismatch"));}
    for(column,field)in projection("items"){
        if (!item[*field].is_null()&&!item[*field].is_string())
            ||record.try_get::<Option<String>,_>(*column)?.as_deref()!=item[*field].as_str(){return Err(internal("Recipient relational projection mismatch"));}
    }
    view["proposals"]=json!([proposal]);view["items"]=json!([item]);
    // Global exact identity lookup preserves cross-item collision rejection.
    // Including payload identity also refuses corrupt aliases instead of hiding them.
    let records=sqlx::query("SELECT id,item_id,payload::text FROM communityhero.feedback WHERE workspace_id=$1 AND (id=$2 OR payload->>'id'=$2) ORDER BY ordinal")
        .bind(WORKSPACE).bind(body["eventId"].as_str()).fetch_all(connection).await?;
    let mut events=Vec::with_capacity(records.len());
    for record in records{
        let event=parse(record.try_get::<&str,_>("payload")?)?;
        if record.try_get::<&str,_>("id")?!=text(&event,"id")?
            ||record.try_get::<Option<String>,_>("item_id")?.as_deref()!=Some(text(&event,"itemId")?){return Err(internal("Proposal feedback identity mismatch"));}
        events.push(event);
    }
    view["feedback"]=json!(events);Ok(view)
}

impl Database{
    pub(crate) async fn edit_proposal_observed(&self,key:&str,body:&Value,expected_runtime:&crate::runtime_lifecycle::RuntimeIdentity)->ApiResult<(Value,bool)>{
        match self{
            Self::Sqlite(_)=>self.change_observed(|workspace|{
                crate::runtime_lifecycle::current_owner(workspace,expected_runtime)?;
                let before=project(workspace,key,body)?;let mut after=before.clone();
                let result=crate::edit_proposal(&mut after,key,body)?;validate_edit(&before,&after,key,body)?;
                *crate::row_mut(workspace,"proposals",key)?=after["proposals"][0].clone();
                for event in &rows(&after,"feedback")?[rows(&before,"feedback")?.len()..]{crate::list_mut(workspace,"feedback").push(event.clone());}
                Ok(result)
            }).await,
            Self::Postgres{writer,..}=>{
                let wait=crate::performance::Span::new("proposal.edit.pool_wait");let mut tx=writer.begin().await?;drop(wait);
                let loading=crate::performance::Span::new("proposal.edit.load");let before=load(&mut tx,key,body).await?;drop(loading);
                crate::runtime_lifecycle::current_owner(&before,expected_runtime)?;
                let domain=crate::performance::Span::new("proposal.edit.clone_and_domain");let mut after=before.clone();
                let result=crate::edit_proposal(&mut after,key,body)?;drop(domain);
                let validation=crate::performance::Span::new("proposal.edit.validation");validate_edit(&before,&after,key,body)?;drop(validation);
                if before==after{tx.commit().await?;return Ok((result,false));}
                let persist=crate::performance::Span::new("proposal.edit.persist_and_commit");
                let proposal=&after["proposals"][0];
                if sqlx::query("UPDATE communityhero.proposals SET payload=$3::jsonb,status=$4 WHERE workspace_id=$1 AND id=$2 AND payload->'revision'=$5::jsonb")
                    .bind(WORKSPACE).bind(key).bind(proposal.to_string()).bind(proposal["status"].as_str())
                    .bind(before["proposals"][0]["revision"].to_string()).execute(&mut *tx).await?.rows_affected()!=1{
                    return Err(crate::conflict("Proposal revision changed"));
                }
                for event in &rows(&after,"feedback")?[rows(&before,"feedback")?.len()..]{
                    sqlx::query("INSERT INTO communityhero.feedback(workspace_id,id,item_id,payload,ordinal) SELECT $1,$2,$3,$4::jsonb,COALESCE(MAX(ordinal),-1)+1 FROM communityhero.feedback WHERE workspace_id=$1")
                        .bind(WORKSPACE).bind(text(event,"id")?).bind(text(event,"itemId")?).bind(event.to_string()).execute(&mut *tx).await?;
                }
                tx.commit().await?;drop(persist);Ok((result,true))
            }
        }
    }
}

#[cfg(test)]
#[path="storage_proposal_edit_tests.rs"]
mod tests;
