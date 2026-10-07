use super::*;
use crate::conductor_authority::{Context,with_context};

fn fixture()->(Value,Context){
    let mut d=crate::engine_prepare::tests::fixture(false);normalize(&mut d);
    let actor=crate::operator_auth::Actor{id:"schedule-owner".into(),name:"Schedule Owner".into(),role:"operator".into(),csrf_token:"offline".into(),authority_generation:Some("a".repeat(64))};
    let input=json!({"mode":"prepare","scope":{"itemIds":["ready"]},"actionKinds":[]});
    let grant=crate::conductor_authority::create_grant(&d,&actor,&input).unwrap();
    let run=json!({"id":"schedule-run","kind":"conductor","status":"running","account":d["account"],"connectorBinding":d["connectorBinding"],
        "conductor":{"version":1,"desiredState":"running","leaseGeneration":1,"mode":"prepare","scope":input["scope"],"grant":grant}});
    let mut cold=run.clone();cold["id"]=json!("other-run");cold["status"]=json!("completed");
    cold["conductor"]["desiredState"]=json!("paused");cold["cold"]=json!("X".repeat(1_000_000));
    d["jobs"]=json!([run,cold]);
    d["feedback"]=json!([{"id":"retained-feedback","itemId":"ready","kind":"operator_note","text":"retained"}]);
    d["operations"]=json!([{"id":"uncertain-operation","status":"unknown","itemId":"ready","evidence":{"retain":true}}]);
    (d,Context{run_id:"schedule-run".into(),lease_generation:1,actor})
}

// This is the existing App.job conductor reducer, with the same ordering.
fn schedule(d:&mut Value)->ApiResult<String>{
    let ctx=crate::conductor_authority::fence_admission(d,"read",&[])?
        .ok_or_else(||internal("Conductor job authority missing"))?;
    let key=crate::new_job(d,"sync","")?;
    crate::conductor_authority::tag(&ctx,crate::row_mut(d,"jobs",&key)?);Ok(key)
}

#[tokio::test]
async fn conductor_schedule_projection_retains_exact_nonactive_grant_and_dedup_jobs(){
    let(full,ctx)=fixture();
    with_context(ctx.clone(),async{
        let scoped=project(&full,&Scope::Schedule).unwrap();
        assert_eq!(crate::list(&scoped,"jobs").len(),1);
        assert!(scoped.to_string().len()*20<full.to_string().len());
        for status in ["paused","failed","completed"]{
            let mut d=full.clone();crate::row_mut(&mut d,"jobs",&ctx.run_id).unwrap()["status"]=json!(status);
            let scoped=project(&d,&Scope::Schedule).unwrap();
            assert_eq!(crate::row(&scoped,"jobs",&ctx.run_id).unwrap()["status"],status,"exact grant cannot vanish behind active-job filtering");
        }
        let mut after=scoped.clone();schedule(&mut after).unwrap();validate_scope(&scoped,&after,&Scope::Schedule).unwrap();
        let mut forged=after;crate::row_mut(&mut forged,"jobs",&ctx.run_id).unwrap()["conductor"]["grant"]["actor"]["id"]=json!("another-actor");
        assert!(validate_scope(&scoped,&forged,&Scope::Schedule).is_err());
    }).await;
}

async fn exercise(db:&Database){
    let(full,ctx)=fixture();db.change(|d|{*d=full;Ok(())}).await.unwrap();let baseline=db.read().await.unwrap();
    let key=with_context(ctx.clone(),db.change_schedule_observed(schedule)).await.unwrap().0;
    let saved=db.read().await.unwrap();
    let mut expected=baseline.clone();let expected_key=with_context(ctx.clone(),async{schedule(&mut expected)}).await.unwrap();
    let expected_job=crate::row_mut(&mut expected,"jobs",&expected_key).unwrap();
    expected_job["id"]=json!(key);expected_job["createdAt"]=crate::row(&saved,"jobs",&key).unwrap()["createdAt"].clone();
    assert_eq!(saved,expected,"scoped admission matches the complete-workspace reducer including unrelated histories/UNKNOWN");
    assert!(with_context(ctx.clone(),db.change_schedule_observed(schedule)).await.is_err(),"active sync deduplication must survive projection");
    assert_eq!(db.read().await.unwrap(),saved);
    let mut wrong_run=ctx.clone();wrong_run.run_id="missing-run".into();
    let mut wrong_epoch=ctx.clone();wrong_epoch.lease_generation=2;
    let mut wrong_actor=ctx.clone();wrong_actor.actor.id="other-actor".into();
    for (wrong,status) in [(wrong_run,crate::StatusCode::NOT_FOUND),(wrong_epoch,crate::StatusCode::FORBIDDEN),(wrong_actor,crate::StatusCode::FORBIDDEN)]{
        let error=with_context(wrong,db.change_schedule_observed(schedule)).await.unwrap_err();
        assert_eq!(error.0,status,"denial must come from the grant fence, before sync deduplication");
        assert_eq!(db.read().await.unwrap(),saved,"wrong run/generation/actor cannot append work");
    }
    db.change_job_observed(&key,|d|{crate::row_mut(d,"jobs",&key)?["status"]=json!("completed");Ok(())}).await.unwrap();
    db.change_job_observed(&ctx.run_id,|d|{
        let run=crate::row_mut(d,"jobs",&ctx.run_id)?;run["status"]=json!("paused");run["conductor"]["desiredState"]=json!("paused");Ok(())
    }).await.unwrap();
    let paused=db.read().await.unwrap();
    assert!(with_context(ctx.clone(),db.change_schedule_observed(schedule)).await.is_err());
    assert_eq!(db.read().await.unwrap(),paused,"nonactive paused grant cannot schedule another sync");
    println!("CONDUCTOR_SCHEDULE_SCOPE {}",json!({"fullBytes":baseline.to_string().len(),"scopedBytes":with_context(ctx,async{project(&baseline,&Scope::Schedule).unwrap().to_string().len()}).await,"fullCollections":13,"loadedCollections":1,"unknownPreserved":true,"pausedDenied":true,"staleEpochDenied":true,"dedupPreserved":true}));
}

#[tokio::test]
async fn sqlite_conductor_sync_schedule_preserves_authority_history_and_dedup(){
    let folder=tempfile::tempdir().unwrap();let db=Database::Sqlite(crate::open_db(&folder.path().join("fixture.sqlite")).await.unwrap());
    exercise(&db).await;db.close().await;
}

#[tokio::test]
#[ignore="requires a pristine explicitly isolated communityhero_writer_v51_test_ PostgreSQL fixture"]
async fn postgres_conductor_sync_schedule_preserves_authority_history_and_dedup(){
    let db=super::super::preparation::writer_v51_fixture_db().await;
    exercise(&db).await;
    let Database::Postgres{writer,..}=&db else{unreachable!()};
    let(_,ctx)=fixture();
    db.change_job_observed(&ctx.run_id,|d|{
        let run=crate::row_mut(d,"jobs",&ctx.run_id)?;run["status"]=json!("running");run["conductor"]["desiredState"]=json!("running");Ok(())
    }).await.unwrap();
    let body=with_context(ctx.clone(),async{
        tokio::join!(db.change_schedule_observed(schedule),db.change_schedule_observed(schedule))
    }).await;
    assert_eq!(usize::from(body.0.is_ok())+usize::from(body.1.is_ok()),1,"two contenders cannot admit simultaneous sync jobs");
    let winning=body.0.or(body.1).unwrap().0;
    db.change_job_observed(&winning,|d|{crate::row_mut(d,"jobs",&winning)?["status"]=json!("completed");Ok(())}).await.unwrap();
    let original=db.read().await.unwrap();
    sqlx::query("CREATE FUNCTION pg_temp.schedule_reject() RETURNS trigger LANGUAGE plpgsql AS 'BEGIN RAISE EXCEPTION ''synthetic late job rejection''; END;'").execute(writer).await.unwrap();
    sqlx::query("CREATE TRIGGER schedule_reject BEFORE INSERT ON communityhero.jobs FOR EACH ROW EXECUTE FUNCTION pg_temp.schedule_reject()").execute(writer).await.unwrap();
    assert!(with_context(ctx.clone(),db.change_schedule_observed(schedule)).await.is_err());
    sqlx::query("DROP TRIGGER schedule_reject ON communityhero.jobs").execute(writer).await.unwrap();
    assert_eq!(db.read().await.unwrap(),original,"late SQL failure cannot persist metadata, job or history changes");
    db.change_job_observed(&ctx.run_id,|d|{
        let run=crate::row_mut(d,"jobs",&ctx.run_id)?;run["status"]=json!("paused");run["conductor"]["desiredState"]=json!("paused");Ok(())
    }).await.unwrap();
    let mut tx=writer.begin().await.unwrap();
    sqlx::query("UPDATE communityhero.jobs SET kind='sync' WHERE workspace_id=$1 AND id='schedule-run'").bind(WORKSPACE).execute(&mut *tx).await.unwrap();
    tx.commit().await.unwrap();
    assert!(with_context(ctx,db.change_schedule_observed(schedule)).await.is_err(),"exact nonactive grant relational corruption must also be rejected");
    db.close().await;
}
