//! Returned auto-preparation settlement and completed engine/auto recovery.
//! Typed reducers only; this scope never reserves, dispatches or adds jobs.
//! SQLite remains a full-document writer. PostgreSQL reuses the shared locked
//! source projection. Recovery audit is an exact insert-only output, separate
//! from the omitted historical audit collection and shared history validator.
use super::*;

enum Intent<'a>{
    Success{result:&'a Value,reviewed:bool,at:Option<&'a str>},
    Recovery{plan:&'a str,profile:&'a Value,at:&'a str},
    // Finite hostile fixture seam only: execute the real typed reducer, then
    // corrupt its existing paid reference before the SAME guards/persistence.
    #[cfg(test)]
    PaidFault{intent:Box<Intent<'a>>,rebind:bool},
}
impl Intent<'_>{
    fn recovery(&self)->bool{match self{
        Self::Recovery{..}=>true,Self::Success{..}=>false,
        #[cfg(test)] Self::PaidFault{intent,..}=>intent.recovery(),
    }}
    fn apply(&self,d:&mut Value,run:&str)->ApiResult<(Value,Option<Value>)>{
        match self{
            Self::Success{result,reviewed,at}=>{
                if crate::row(d,"jobs",run)?["purpose"]!="auto_prepare"{
                    return Err(crate::conflict("Bounded final settlement requires auto_prepare"));
                }
                crate::auto_prepare::settle_success(d,run,result,*reviewed,*at).map(|v|(v,None))
            },
            Self::Recovery{plan,profile,at}=>{
                let replay=!crate::row(d,"jobs",run)?["completedRecovery"].is_null();
                let outcome=crate::preparation_review::chunks::recover_in_unlogged(d,run,plan,profile,at)?;
                let receipt=(!replay).then(||json!({"id":crate::id(),"action":"preparation.completed_result_recovered",
                    "refId":run,"createdAt":crate::now()}));
                Ok((outcome,receipt))
            },
            #[cfg(test)]
            Self::PaidFault{intent,rebind}=>{
                let output=intent.apply(d,run)?;
                let job=crate::row_mut(d,"jobs",run)?;
                assert!(job["prepareOutcome"].is_object(),"fault follows actual final reducer");
                assert!(job["retainedEvidence"].as_array().is_some_and(|refs|!refs.is_empty()),"legitimate prior reference required");
                if *rebind{job["retainedEvidence"][0]["binding"]["nativeJobId"]=json!("foreign-paid-job");}
                else{job.as_object_mut().unwrap().remove("retainedEvidence");}
                Ok(output)
            },
        }
    }
}

fn final_projection(workspace:&Value,run:&str)->ApiResult<Value>{
    projection_for(workspace,Some(run))
}

fn validate_final(before:&Value,after:&Value,run:&str,recovery:bool)->ApiResult<()> {
    use admission::equal_except;
    // No-op recovery replay has already passed recover_in's plan/profile/receipt
    // check. It grants no second admission, audit append or notification.
    if before==after{return Ok(());}
    let allowed=&["items","proposals","jobs","preparationResearch"][..];
    if !equal_except(before,after,allowed){return Err(internal("Final settlement changed source or protected history"));}
    let old_job=crate::row(before,"jobs",run)?;let new_job=crate::row(after,"jobs",run)?;
    let automatic=old_job["purpose"]=="auto_prepare";
    if old_job["kind"]!="assistant"||(!automatic&&(!recovery||old_job["purpose"]!="engine_prepare"))
        ||(!recovery&&old_job["status"]!="running")
        ||(recovery&&!matches!(old_job["status"].as_str(),Some("failed"|"interrupted")))
        ||!old_job["prepareOutcome"].is_null()||!old_job["preparationStages"]["review"].is_null(){
        return Err(internal("Final settlement owner is not unadmitted returned work"));
    }
    let recipients=preparation_recipients(old_job).ok_or_else(||internal("Final recipients missing"))?;
    let old_items=rows(before,"items")?;let new_items=rows(after,"items")?;
    if old_items.len()!=new_items.len(){return Err(internal("Final settlement changed item inventory"));}
    for (old,new) in old_items.iter().zip(new_items){
        if old==new{continue;}
        let fields=if automatic {&["workflow","revision","decision","reason","triageTags","autoPreparation"][..]}
            else{&["workflow","revision"][..]};
        if !old["id"].as_str().is_some_and(|id|recipients.iter().any(|r|r==id))||!equal_except(old,new,fields){
            return Err(internal("Final settlement changed protected item state"));
        }
        if automatic {
            if old["autoPreparation"]["jobId"]!=run||new["autoPreparation"]["jobId"]!=run
                ||!equal_except(&old["autoPreparation"],&new["autoPreparation"],
                    &["status","reason","updatedAt","retryAt","reviewResumeRequired","requiresReview","reasonCode","reviewReason"]){
                return Err(internal("Final settlement changed automatic capture or paid attempt count"));
            }
        }else if old["workflow"]!="attention"||new["workflow"]!="prepared"
            ||old["revision"].as_u64().and_then(|n|n.checked_add(1))!=new["revision"].as_u64(){
            return Err(internal("Engine recovery changed protected item state"));
        }
    }
    let old_proposals=rows(before,"proposals")?;let new_proposals=rows(after,"proposals")?;
    if !new_proposals.starts_with(old_proposals){return Err(internal("Final settlement rewrote paid proposal history"));}
    let mut ids:HashSet<&str>=old_proposals.iter().map(|p|text(p,"id")).collect::<ApiResult<_>>()?;
    let mut proposed=HashSet::new();
    for proposal in &new_proposals[old_proposals.len()..]{
        let item_id=text(proposal,"itemId")?;let item=crate::row(after,"items",item_id)?;
        if !ids.insert(text(proposal,"id")?)||!proposed.insert(item_id)||!recipients.iter().any(|r|r==item_id)
            ||proposal["status"]!="draft"||proposal["revision"]!=1||proposal["itemRevision"]!=item["revision"]
            ||proposal["prepareRunId"]!=run||proposal["prepareBundleId"]!=old_job["prepareBundle"]["id"]
            ||proposal["prepareBundleDigest"]!=old_job["prepareBundle"]["digest"]
            ||!proposal["sourceContextDigest"].is_string()||proposal["sourceContextDigest"]!=proposal["reviewContextDigest"]{
            return Err(internal("Final settlement appended an invalid proposal"));
        }
        for (_,field) in projection("proposals"){
            if !proposal[*field].is_null()&&!proposal[*field].is_string(){return Err(internal("Invalid final proposal projection"));}
        }
    }
    let old_jobs=rows(before,"jobs")?;let new_jobs=rows(after,"jobs")?;
    let job_fields=if recovery {&["runMetadata","prepareOutcome","preparationStages","status","result","finishedAt","error","completedRecovery"][..]}
        else{&["runMetadata","prepareOutcome","preparationStages"][..]};
    if old_jobs.len()!=new_jobs.len()||old_jobs.iter().zip(new_jobs).any(|(a,b)|if a["id"]==run{b["id"]!=run}else{a!=b})
        ||!equal_except(old_job,new_job,job_fields)
        ||!equal_except(&old_job["preparationStages"],&new_job["preparationStages"],&["review","groupAdmission"]){
        return Err(internal("Final settlement changed immutable job capture or another job"));
    }
    let old_groups=old_job["preparationStages"]["groupAdmission"].as_array();
    let new_groups=new_job["preparationStages"]["groupAdmission"].as_array();
    if old_groups.is_some()!=new_groups.is_some()||old_groups.zip(new_groups).is_some_and(|(a,b)|a.len()!=b.len()){
        return Err(internal("Final settlement changed group inventory"));
    }
    for (old,new) in old_groups.into_iter().flatten().zip(new_groups.into_iter().flatten()){
        if old==new{continue;}
        if !equal_except(old,new,&["status","admission"])||old["status"]!="pending"||!old["admission"].is_null()
            ||!matches!(new["status"].as_str(),Some("admitted"|"stale"))||!new["admission"].is_object(){
            return Err(internal("Final settlement changed an immutable group outcome"));
        }
    }
    if !new_job["prepareOutcome"].is_object()||new_groups.into_iter().flatten().any(|g|g["status"]=="pending"){
        return Err(internal("Final settlement left an unsettled outcome"));
    }
    // Historical workspaces may have no archive until their first review.
    // Present archives remain complete ordered history; null/malformed values
    // and deletion of an existing collection never become an empty history.
    let old_archive=match before.get("preparationResearch"){
        None=>&[][..],Some(value)=>value.as_array().map(Vec::as_slice)
            .ok_or_else(||internal("Invalid prior preparation research archive"))?,
    };
    let new_archive=match after.get("preparationResearch"){
        None=>&[][..],Some(value)=>value.as_array().map(Vec::as_slice)
            .ok_or_else(||internal("Invalid final preparation research archive"))?,
    };
    if before.get("preparationResearch").is_some()&&after.get("preparationResearch").is_none()
        ||!new_archive.starts_with(old_archive)||new_archive.len()>old_archive.len()+1{
        return Err(internal("Final settlement rewrote research history"));
    }
    let review=&new_job["preparationStages"]["review"];let appended=new_archive.get(old_archive.len());
    if review.is_null()!=appended.is_none(){return Err(internal("Final review and archive must commit together"));}
    if let Some(archive)=appended{
        if old_job["preparationStages"]["first"]["reviewRequired"]!=true||review["status"]!="completed"
            ||!review["result"].is_object()||archive["id"]!=format!("research:{run}")||archive["jobId"]!=run
            ||old_archive.iter().any(|a|a["id"]==archive["id"]||a["jobId"]==run)
            ||archive["account"]!=old_job["prepareBundle"]["request"]["account"]
            ||archive["connectorBinding"]!=old_job["prepareBundle"]["request"]["connectorBinding"]
            ||archive["prepareBundleId"]!=old_job["prepareBundle"]["id"]||archive["prepareBundleDigest"]!=old_job["prepareBundle"]["digest"]
            ||archive["trust"]!="source_only"||archive["activePolicy"]!=false||archive["review"]!=*review
            ||archive["checksum"]!=crate::research_cache::checksum(archive){return Err(internal("Invalid final research archive"));}
    }
    if recovery{
        if new_job["status"]!="completed"
            ||new_job["result"]!=new_job["prepareOutcome"]||!new_job["error"].is_null()
            ||new_job["completedRecovery"]["previousStatus"]!=old_job["status"]{
            return Err(internal("Invalid atomic final recovery receipt"));
        }
    }
    crate::db_guards::validate_change(before,after)
}

fn validate_audit_output(before:&Value,after:&Value,run:&str,recovery:bool,receipt:Option<&Value>)->ApiResult<()> {
    // An output is never exposed as a filtered/empty history to db_guards.
    if before.get("audit").is_some()||after.get("audit").is_some(){return Err(internal("Final audit history must remain omitted"));}
    let replay=!crate::row(before,"jobs",run)?["completedRecovery"].is_null();
    if !recovery||replay{
        if receipt.is_some(){return Err(internal("Unexpected final audit append"));}
        return Ok(());
    }
    let receipt=receipt.ok_or_else(||internal("Recovery audit append missing"))?;
    let object=receipt.as_object().ok_or_else(||internal("Invalid recovery audit append"))?;
    if object.len()!=4||!["id","action","refId","createdAt"].iter().all(|k|object.contains_key(*k))
        ||receipt["id"].as_str().is_none_or(|s|s.trim().is_empty())
        ||receipt["action"]!="preparation.completed_result_recovered"||receipt["refId"]!=run
        ||receipt["createdAt"].as_str().is_none_or(|at|chrono::DateTime::parse_from_rfc3339(at).is_err())
        ||before==after||crate::row(after,"jobs",run)?["completedRecovery"].is_null(){
        return Err(internal("Invalid exact recovery audit append"));
    }
    Ok(())
}

fn merge_final(workspace:&mut Value,before:&Value,after:&Value,receipt:Option<&Value>)->ApiResult<()> {
    merge_claim_delta(workspace,before,after)?;
    if before.get("preparationResearch")!=after.get("preparationResearch"){
        workspace["preparationResearch"]=after["preparationResearch"].clone();
    }
    if let Some(receipt)=receipt{
        let audit=workspace["audit"].as_array_mut().ok_or_else(||internal("Audit history missing"))?;
        if audit.iter().any(|row|row["id"]==receipt["id"]){return Err(internal("Recovery audit identity collision"));}
        audit.push(receipt.clone());
    }
    Ok(())
}

fn observe_delta(before:&Value,after:&Value,run:&str,receipt:Option<&Value>)->ApiResult<()> {
    let mut span=crate::performance::Span::job("preparation.final.delta.serialized",run);
    let mut records=0;let mut bytes=0;let mut collections=0;
    for table in MUTABLE{
        let prior=rows(before,table)?;let mut touched=false;
        for (index,row) in rows(after,table)?.iter().enumerate(){if prior.get(index)!=Some(row){
            records+=1;bytes+=row.to_string().len();touched=true;
        }}
        if touched{collections+=1;}
    }
    if before.get("preparationResearch")!=after.get("preparationResearch"){
        // This is one metadata UPDATE with the complete new archive value,
        // not just the appended archive's bytes.
        records+=1;bytes+=after["preparationResearch"].to_string().len();collections+=1;
    }
    if let Some(receipt)=receipt{records+=1;bytes+=receipt.to_string().len();collections+=1;}
    // Logical JSON serialization and changed record/write-call counts. These
    // are not physical SQLite bytes, SQL duration or lock occupancy estimates.
    span.counts(records,bytes,collections);Ok(())
}

impl Database{
    async fn change_preparation_final(&self,run:&str,intent:Intent<'_>,capture:Option<&crate::runtime_lifecycle_app::Capture>)->ApiResult<(Value,bool)>{
        let recovery=intent.recovery();
        let apply=|d:&mut Value|match capture {Some(c)=>c.with(d,|d|intent.apply(d,run)),None=>intent.apply(d,run)};
        match self{
            Self::Sqlite(_)=>self.change_observed(|workspace|{
                let before=final_projection(workspace,run)?;let mut after=before.clone();
                let (result,receipt)=apply(&mut after)?;validate_final(&before,&after,run,recovery)?;
                validate_audit_output(&before,&after,run,recovery,receipt.as_ref())?;
                observe_delta(&before,&after,run,receipt.as_ref())?;
                merge_final(workspace,&before,&after,receipt.as_ref())?;Ok(result)
            }).await,
            Self::Postgres{writer,..}=>{
                let wait=crate::performance::Span::job("preparation.final.pool_wait",run);
                let mut connection=writer.acquire().await?;drop(wait);
                let mut tx=sqlx::Connection::begin(&mut *connection).await?;
                // The source/delta and persistence span outlive the async body.
                // Even a reducer/SQL error explicitly settles and returns the
                // borrowed connection before large JSON is dropped or logged.
                let mut before=Value::Null;
                let mut after=Value::Null;
                let mut persist=None;
                let outcome:ApiResult<(Value,bool)>=async{
                    let load=crate::performance::Span::job("preparation.final.load",run);
                    // Existing loader acquires the universal workspace FOR UPDATE
                    // BEFORE any recipient/source/reservation/control capture.
                    before=load_pg_claim(&mut tx,Some(run),false).await?;
                    drop(load);
                    let domain=crate::performance::Span::job("preparation.final.clone_and_domain",run);
                    after=before.clone();let (result,receipt)=apply(&mut after)?;drop(domain);
                    let validation=crate::performance::Span::job("preparation.final.validation",run);
                    validate_final(&before,&after,run,recovery)?;drop(validation);
                    validate_audit_output(&before,&after,run,recovery,receipt.as_ref())?;
                    observe_delta(&before,&after,run,receipt.as_ref())?;
                    if before==after{return Ok((result,false));}
                    persist=Some(crate::performance::Span::job("preparation.final.persist_and_commit",run));
                    for table in MUTABLE{
                        let old=rows(&before,table)?;
                        for (index,value) in rows(&after,table)?.iter().enumerate(){
                            if old.get(index)!=Some(value){persist_claim_record(&mut tx,table,value,index>=old.len()).await?;}
                        }
                    }
                    if before.get("preparationResearch")!=after.get("preparationResearch"){
                        sqlx::query("UPDATE communityhero.workspaces SET metadata=jsonb_set(metadata,'{preparationResearch}',$2::jsonb,true) WHERE id=$1")
                            .bind(WORKSPACE).bind(after["preparationResearch"].to_string()).execute(&mut *tx).await?;
                        #[cfg(test)] crate::performance::r3_sql_write();
                    }
                    if let Some(value)=receipt{
                        // Insert-only output. The same universal writer lock
                        // protects MAX(ordinal); unique identity failures roll back
                        // item/proposal/job/archive writes in this transaction.
                        persist_claim_record(&mut tx,"audit",&value,true).await?;
                    }
                    Ok((result,true))
                }.await;
                let(outcome,completion)=super::super::pg_writer::settle(tx,outcome).await;
                super::super::pg_writer::release(&mut connection,writer,completion).await;
                drop(persist);
                drop(after);
                drop(before);
                outcome
            },
        }
    }
}

impl crate::App{
    pub(crate) async fn settle_preparation_success(&self,run:&str,result:&Value,reviewed:bool)->ApiResult<Value>{
        let job=self.db.read_job(run).await?.ok_or_else(||crate::conflict("Preparation job missing"))?;
        if job["purpose"]=="auto_revalidate"{
            // This reducer reads saved previous-decision/paid/restart jobs that
            // the job-bound initial projection does not retain. Keep its exact
            // existing full transaction until a separate contract covers it.
            return self.change(|d|{
                if crate::row(d,"jobs",run)?["purpose"]!="auto_revalidate"{return Err(crate::conflict("Revalidation owner changed"));}
                crate::auto_prepare::settle_success(d,run,result,reviewed,None)
            }).await;
        }
        let intent=Intent::Success{result,reviewed,at:None};
        self.preparation_final(run,intent).await.map(|(v,_)|v)
    }
    pub(crate) async fn recover_completed_preparation(&self,run:&str,plan:&str,profile:&Value,at:&str)->ApiResult<(Value,bool)>{
        self.preparation_final(run,Intent::Recovery{plan,profile,at}).await
    }
    async fn preparation_final(&self,run:&str,intent:Intent<'_>)->ApiResult<(Value,bool)>{
        let capture=crate::runtime_lifecycle_app::Capture::read(self).await;
        let _total=crate::performance::Span::job("preparation.final.total",run);
        let waiting=crate::performance::Span::job("preparation.final.writer.wait",run);
        let _guard=self.gate.acquire(crate::writer_gate::Class::Standard).await;drop(waiting);
        let _held=crate::performance::Span::job("preparation.final.writer.held",run);
        let (result,changed)=self.db.change_preparation_final(run,intent,Some(&capture)).await?;
        if changed{self.bootstrap_cache.invalidate();let _=self.events.send(());}
        Ok((result,changed))
    }
}

#[cfg(test)]
#[path="storage_preparation_final_tests.rs"]
mod tests;
