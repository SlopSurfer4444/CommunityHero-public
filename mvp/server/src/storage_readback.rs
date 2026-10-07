//! Bounded UNKNOWN enumeration and atomic readback-job admission. No mutation
//! action is created here, and operation/action/approval evidence is immutable.
use super::*;
use serde_json::json;

fn validate_metadata(value:&Value)->ApiResult<()> {
    if !value.is_object()||!value["account"].is_string()||TABLES.iter().any(|table|value.get(table).is_some()) {
        return Err(internal("Invalid readback workspace metadata"));
    }
    Ok(())
}

fn job_stats(jobs:&[Value],op:&Value)->(bool,u64,Option<String>) {
    let active=jobs.iter().any(|j|matches!(j["status"].as_str(),Some("running"|"queued"))
        &&(j["kind"]=="reconcile"||(j["kind"]=="execute"&&j["refId"]==op["approvalId"])));
    let previous:Vec<_>=jobs.iter().filter(|j|j["kind"]=="reconcile"&&j["refId"]==op["id"]&&j.get("automaticReadback").is_some()).collect();
    let last=previous.last().and_then(|j|j["finishedAt"].as_str().or_else(||j["createdAt"].as_str())).map(str::to_owned);
    (active,previous.len() as u64,last)
}

impl Database {
    /// At most sixteen identities; no comment/media/assistant history is read.
    pub(crate) async fn read_readback_candidates(&self,after:i64)->ApiResult<Vec<(i64,String)>> {
        match self {
            Self::Sqlite(pool)=>{
                let records=sqlx::query("SELECT CAST(o.key AS INTEGER) AS ordinal,json_extract(o.value,'$.id') AS id FROM workspace w,json_each(w.payload,'$.operations') o WHERE w.id=1 AND json_extract(o.value,'$.status')='unknown' AND CAST(o.key AS INTEGER)>? ORDER BY CAST(o.key AS INTEGER) LIMIT 16")
                    .bind(after).fetch_all(pool).await?;
                records.into_iter().map(|r|Ok((r.try_get("ordinal")?,r.try_get("id")?))).collect()
            }
            Self::Postgres{reader,..}=>{
                // Verify workspace ownership with the existing metadata guard;
                // admission below repeats it under the writer transaction.
                self.read_metadata().await?;
                let records=sqlx::query("SELECT id,ordinal,payload->>'id' AS payload_id,payload->>'status' AS payload_status FROM communityhero.operations WHERE workspace_id=$1 AND status='unknown' AND ordinal>$2 ORDER BY ordinal LIMIT 16")
                    .bind(WORKSPACE).bind(after).fetch_all(reader).await?;
                records.into_iter().map(|r|{
                    let key:String=r.try_get("id")?;
                    if r.try_get::<Option<String>,_>("payload_id")?.as_deref()!=Some(key.as_str())
                        ||r.try_get::<Option<String>,_>("payload_status")?.as_deref()!=Some("unknown") {return Err(internal("Readback candidate projection mismatch"));}
                    Ok((i64::from(r.try_get::<i32,_>("ordinal")?),key))
                }).collect()
            }
        }
    }

    /// Caller holds the App writer gate. Serializes with manual reconciliation,
    /// current execute jobs and every other workspace writer. Attempt budget is
    /// derived from durable jobs including interrupted jobs, never process memory.
    #[cfg(test)]
    pub(crate) async fn claim_readback_recovery(&self,key:&str,at:i64,requested_by:Option<&Value>)->ApiResult<Option<(String,Value)>> {
        let metadata = self.read_metadata().await?;
        let token = crate::runtime_lifecycle::admission_token(&metadata, crate::runtime_lifecycle::AdmissionClass::SocialDispatch)?;
        self.claim_readback_recovery_for_owner(key, at, requested_by, &token).await
    }
    pub(crate) async fn claim_readback_recovery_for_owner(&self,key:&str,at:i64,requested_by:Option<&Value>,expected:&crate::runtime_lifecycle::OwnerToken)->ApiResult<Option<(String,Value)>> {
        match self {
            Self::Sqlite(_)=>self.change_observed(|workspace|{
                crate::runtime_lifecycle::require_admission(workspace,expected,crate::runtime_lifecycle::AdmissionClass::SocialDispatch)?;
                if rows(workspace,"operations")?.iter().filter(|op|op["id"]==key).count()>1 {return Err(internal("Duplicate readback operation identity"));}
                let Some(op)=rows(workspace,"operations")?.iter().find(|op|op["id"]==key).cloned() else{return Ok(None);};
                let meta=metadata(workspace);validate_metadata(&meta)?;
                let (active,attempts,last)=job_stats(rows(workspace,"jobs")?,&op);
                let Some(mut job)=crate::readback_recovery::planned_job(&meta,&op,active,attempts,last.as_deref(),at,requested_by) else{return Ok(None);};
                crate::conductor_authority::fence_readback_claim(workspace,&op,requested_by,&mut job)?;
                let id=text(&job,"id")?.to_owned();
                workspace["jobs"].as_array_mut().ok_or_else(||internal("Invalid readback jobs"))?.push(job);
                Ok(Some((id,op)))
            }).await.map(|(value,_)|value),
            Self::Postgres{writer,..}=>{
                let mut tx=writer.begin().await?;
                let record=sqlx::query("SELECT account,metadata::text,execution_enabled FROM communityhero.workspaces WHERE id=$1 FOR UPDATE")
                    .bind(WORKSPACE).fetch_one(&mut *tx).await?;
                let meta=parse(record.try_get::<&str,_>("metadata")?)?;validate_metadata(&meta)?;
                crate::runtime_lifecycle::require_admission(&meta,expected,crate::runtime_lifecycle::AdmissionClass::SocialDispatch)?;
                if record.try_get::<bool,_>("execution_enabled")?||record.try_get::<Option<String>,_>("account")?.as_deref()!=meta["account"].as_str() {return Err(internal("Readback workspace identity mismatch"));}
                let Some(record)=sqlx::query("SELECT id,ordinal,payload::text,item_id,proposal_id,approval_id,status FROM communityhero.operations WHERE workspace_id=$1 AND id=$2 FOR UPDATE")
                    .bind(WORKSPACE).bind(key).fetch_optional(&mut *tx).await? else{tx.commit().await?;return Ok(None);};
                let op=parse(record.try_get::<&str,_>("payload")?)?;
                if text(&op,"id")?!=key||record.try_get::<&str,_>("id")?!=key||record.try_get::<i32,_>("ordinal")?<0 {return Err(internal("Readback operation identity mismatch"));}
                for (column,field) in projection("operations") {
                    if (!op[*field].is_null()&&!op[*field].is_string())||record.try_get::<Option<String>,_>(*column)?.as_deref()!=op[*field].as_str() {return Err(internal("Readback operation projection mismatch"));}
                }
                let stats=sqlx::query(r#"SELECT
 EXISTS(SELECT 1 FROM communityhero.jobs WHERE workspace_id=$1 AND status IN ('running','queued') AND (kind='reconcile' OR (kind='execute' AND ref_id=$3))) AS active,
 (SELECT count(*) FROM communityhero.jobs WHERE workspace_id=$1 AND kind='reconcile' AND ref_id=$2 AND payload ? 'automaticReadback') AS attempts,
 (SELECT COALESCE(payload->>'finishedAt',payload->>'createdAt') FROM communityhero.jobs WHERE workspace_id=$1 AND kind='reconcile' AND ref_id=$2 AND payload ? 'automaticReadback' ORDER BY ordinal DESC LIMIT 1) AS last_at
"#).bind(WORKSPACE).bind(key).bind(op["approvalId"].as_str()).fetch_one(&mut *tx).await?;
                let attempts=stats.try_get::<i64,_>("attempts")?;
                let last=stats.try_get::<Option<String>,_>("last_at")?;
                let Some(mut job)=crate::readback_recovery::planned_job(&meta,&op,stats.try_get("active")?,attempts as u64,last.as_deref(),at,requested_by) else{tx.commit().await?;return Ok(None);};
                if let Some(ctx)=crate::conductor_authority::current_context() {
                    let grant=sqlx::query("SELECT id,payload::text,kind,status,ref_id FROM communityhero.jobs WHERE workspace_id=$1 AND id=$2")
                        .bind(WORKSPACE).bind(&ctx.run_id).fetch_optional(&mut *tx).await?
                        .ok_or_else(||internal("Conductor readback grant missing"))?;
                    let value=parse(grant.try_get::<&str,_>("payload")?)?;
                    if grant.try_get::<&str,_>("id")?!=text(&value,"id")? {return Err(internal("Conductor readback grant identity mismatch"));}
                    for(column,field)in projection("jobs") {
                        if (!value[*field].is_null()&&!value[*field].is_string())
                            ||grant.try_get::<Option<String>,_>(*column)?.as_deref()!=value[*field].as_str() {
                            return Err(internal("Conductor readback grant projection mismatch"));
                        }
                    }
                    let mut fenced=meta.clone();fenced["jobs"]=json!([value]);
                    crate::conductor_authority::fence_readback_claim(&fenced,&op,requested_by,&mut job)?;
                }
                let id=text(&job,"id")?.to_owned();
                let inserted=sqlx::query("INSERT INTO communityhero.jobs(workspace_id,id,ordinal,payload,kind,status,ref_id) SELECT $1,$2,COALESCE(MAX(ordinal),-1)+1,$3::jsonb,'reconcile','running',$4 FROM communityhero.jobs WHERE workspace_id=$1")
                    .bind(WORKSPACE).bind(&id).bind(job.to_string()).bind(key).execute(&mut *tx).await?;
                if inserted.rows_affected()!=1 {return Err(internal("Readback claim was not persisted"));}
                tx.commit().await?;
                Ok(Some((id,op)))
            }
        }
    }
}

#[cfg(test)]
mod lifecycle_claim_tests {
    use super::*;
    #[tokio::test]
    async fn stale_social_claim_is_rejected_before_any_durable_write() {
        let (app, _temp) = crate::tests::test_app().await;
        let at = chrono::Utc::now().timestamp();
        let op = crate::readback_recovery::tests::operation(at - 60);
        app.change(|d| { d["operations"] = json!([op.clone()]); Ok(()) }).await.unwrap();
        let token = app.lifecycle_admission_token(crate::runtime_lifecycle::AdmissionClass::SocialDispatch).await.unwrap();
        let target = crate::runtime_lifecycle::AdmittedTarget { release_sha256: "c".repeat(64), media_analysis_generation: 1, asr_disabled: true };
        app.db.change_runtime_lifecycle_with_ledger(|d| crate::runtime_lifecycle::begin_drain_for_release(d, &token, &target, "readback-race")).await.unwrap();
        let before = app.db.read().await.unwrap();
        assert!(app.db.claim_readback_recovery_for_owner("op", at, None, &token).await.is_err());
        let after = app.db.read().await.unwrap();
        assert_eq!(after["operations"], before["operations"]);
        assert_eq!(after["jobs"], before["jobs"]);
        assert_eq!(after["runtimeLifecycle"], before["runtimeLifecycle"]);
    }
    #[tokio::test]
    async fn running_claim_keeps_unknown_evidence_and_fixed_owner() {
        let (app, _temp) = crate::tests::test_app().await;
        let at = chrono::Utc::now().timestamp();
        let op = crate::readback_recovery::tests::operation(at - 60);
        app.change(|d| { d["operations"] = json!([op.clone()]); Ok(()) }).await.unwrap();
        let token = app.lifecycle_admission_token(crate::runtime_lifecycle::AdmissionClass::SocialDispatch).await.unwrap();
        let (_, saved) = app.db.claim_readback_recovery_for_owner("op", at, None, &token).await.unwrap().unwrap();
        assert_eq!(saved, op);
        let after = app.db.read().await.unwrap();
        assert_eq!(after["operations"], json!([op]));
        crate::runtime_lifecycle::require_runtime_owner(&crate::runtime_lifecycle::current_owner(&after, &app.lifecycle_owner).unwrap(), &app.lifecycle_owner).unwrap();
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn durable_readback_claim_survives_restart_and_never_modifies_operation() {
        let (app,temp)=crate::tests::test_app().await;
        let at=chrono::Utc::now().timestamp();
        let op=crate::readback_recovery::tests::operation(at-60);
        app.change(|d|{d["operations"]=json!([op]);Ok(())}).await.unwrap();
        let (job,saved)=app.db.claim_readback_recovery("op",at,None).await.unwrap().unwrap();
        assert_eq!(saved,op);
        assert!(app.db.claim_readback_recovery("op",at,None).await.unwrap().is_none());
        assert!(app.db.claim_readback_recovery("op",at,Some(&json!({"id":"operator"}))).await.unwrap().is_none());
        app.change(|d|{crate::recover(d).unwrap();Ok(())}).await.unwrap();
        assert_eq!(crate::row(&app.db.read().await.unwrap(),"jobs",&job).unwrap()["status"],"interrupted");
        app.db.close().await;
        let db=Database::Sqlite(crate::open_db(&temp.path().join("workspace.sqlite")).await.unwrap());
        // The interrupted attempt still spends budget. Its recovery timestamp
        // controls delay; advancing test time does not reset the attempt number.
        let (second,_)=db.claim_readback_recovery("op",at+121,None).await.unwrap().unwrap();
        let saved=db.read().await.unwrap();
        assert_eq!(crate::row(&saved,"jobs",&second).unwrap()["automaticReadback"]["attempt"],2);
        assert_eq!(saved["operations"][0],op);
        db.close().await;
    }
    #[tokio::test]
    async fn candidate_page_is_bounded_and_excludes_resolved_operations() {
        let (app,_temp)=crate::tests::test_app().await;
        let at=chrono::Utc::now().timestamp();
        app.change(|d|{d["operations"]=json!((0..20).map(|i|{let mut op=crate::readback_recovery::tests::operation(at);op["id"]=json!(format!("op{i}"));op}).collect::<Vec<_>>());d["operations"][0]["status"]=json!("succeeded");Ok(())}).await.unwrap();
        let first=app.db.read_readback_candidates(-1).await.unwrap();assert_eq!(first.len(),16);assert_eq!(first[0].0,1);
        let tail=app.db.read_readback_candidates(first.last().unwrap().0).await.unwrap();assert_eq!(tail.len(),3);
    }
    #[test]
    fn manual_and_inline_execution_block_automatic_reconciliation() {
        let op=json!({"id":"op","approvalId":"approval"});
        for job in [json!({"kind":"reconcile","status":"queued","refId":"other"}),json!({"kind":"execute","status":"running","refId":"approval"})] {
            assert!(job_stats(&[job],&op).0);
        }
        assert!(!job_stats(&[json!({"kind":"execute","status":"running","refId":"other"})],&op).0);
    }
    #[tokio::test]
    #[ignore="requires a pristine explicitly isolated communityhero_writer_v51_test_ PostgreSQL fixture"]
    async fn postgres_conductor_readback_claim_preserves_original_operation_and_pause_is_atomic() {
        let db=super::super::preparation::writer_v51_fixture_db().await;
        let actor=crate::operator_auth::Actor::local_owner("isolated-pg-readback-authorizer");
        let at=chrono::Utc::now().timestamp();
        let mut op=crate::readback_recovery::tests::operation(at-60);
        let authority=crate::dispatch_authority::approval_binding(&actor);
        op["dispatchAuthority"]=json!({"approved":authority,"executed":authority});
        op["approvedBy"]=actor.public_json();op["executedBy"]=actor.public_json();
        let run=db.change(|d| {
            d["connectorBinding"]=crate::accounts::Profile::LikeAvto.binding();
            d["items"]=json!([op["target"]]);
            d["proposals"]=json!([{"id":"p","itemId":"item-1","kind":"close","status":"dispatching",
                "revision":1,"routeTarget":op["target"]}]);
            d["approvals"]=json!([{"id":"approval","status":"consumed","proposals":[{"id":"p","revision":1}],
                "approvedBy":actor.public_json(),"approvalAuthority":authority}]);
            let input=json!({"mode":"execute","scope":{"itemIds":["item-1"]},"actionKinds":["close"]});
            let grant=crate::conductor_authority::create_grant(d,&actor,&input)?;
            let account=d["account"].clone();let binding=d["connectorBinding"].clone();
            let run=crate::new_job(d,"conductor","isolated-pg-readback")?;
            let job=crate::row_mut(d,"jobs",&run)?;
            job["account"]=account;job["connectorBinding"]=binding;job["status"]=json!("paused");
            job["conductor"]=json!({"version":1,"desiredState":"paused","leaseGeneration":3,
                "mode":"execute","scope":input["scope"],"grant":grant});
            op["conductorRunId"]=json!(run);op["grantGeneration"]=json!(1);
            d["operations"]=json!([op]);Ok(run)
        }).await.unwrap();
        crate::native_fixture_owner_repair::initialize_db(&db).await.unwrap();
        let ctx=crate::conductor_authority::Context{run_id:run.clone(),lease_generation:3,actor:actor.clone()};
        crate::conductor_authority::with_context(ctx,async {
            let before=db.read().await.unwrap();
            assert!(db.claim_readback_recovery("op",at,Some(&actor.public_json())).await.is_err());
            assert_eq!(db.read().await.unwrap(),before,"Paused grant appends no job and spends no attempt");
        }).await;
        db.change_job_observed(&run,|d| {
            let job=crate::row_mut(d,"jobs",&run)?;job["status"]=json!("running");
            job["conductor"]["desiredState"]=json!("running");job["conductor"]["leaseGeneration"]=json!(4);Ok(())
        }).await.unwrap();
        let before=db.read().await.unwrap();let original=before["operations"][0].to_string();
        let ctx=crate::conductor_authority::Context{run_id:run.clone(),lease_generation:4,actor:actor.clone()};
        crate::conductor_authority::with_context(ctx,async {
            let (job,saved)=db.claim_readback_recovery("op",at,Some(&actor.public_json())).await.unwrap().unwrap();
            assert_eq!(saved.to_string(),original);
            let after=db.read().await.unwrap();let claimed=crate::row(&after,"jobs",&job).unwrap();
            assert_eq!(claimed["conductorRunId"],run);assert_eq!(claimed["grantGeneration"],4);
            assert_eq!(claimed["kind"],"reconcile");assert_eq!(claimed["readbackOnly"],true);
            assert_eq!(after["operations"][0].to_string(),original);
            assert_eq!(after["operations"][0]["grantGeneration"],1);
            assert_eq!(crate::list(&after,"jobs").len(),crate::list(&before,"jobs").len()+1);
            assert!(db.claim_readback_recovery("op",at,Some(&actor.public_json())).await.unwrap().is_none(),"Existing claim cannot be duplicated");
        }).await;
        db.close().await;
    }
}
