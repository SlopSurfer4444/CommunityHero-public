use super::*;
use crate::conductor_authority::{Context,with_context};

fn fixture()->(Value,Context){
    let mut d=crate::engine_prepare::tests::fixture(false);normalize(&mut d);
    let actor=crate::operator_auth::Actor{id:"scope-owner".into(),name:"Scope Owner".into(),role:"operator".into(),csrf_token:"offline".into(),authority_generation:Some("a".repeat(64))};
    let input=json!({"mode":"prepare","scope":{"itemIds":["ready"]},"actionKinds":[]});
    let grant=crate::conductor_authority::create_grant(&d,&actor,&input).unwrap();
    let run=json!({"id":"conductor-run","kind":"conductor","status":"running","account":d["account"],"connectorBinding":d["connectorBinding"],
        "conductor":{"version":1,"desiredState":"running","leaseGeneration":1,"mode":"prepare","scope":input["scope"],"grant":grant}});
    // Unrelated history is cold, but must still satisfy the global SQLite
    // conductor history guard. A grant-less dummy is corrupt, not a valid
    // second company run to preserve while merging the scoped delta.
    let mut other=run.clone();other["id"]=json!("other-conductor");
    other["conductor"]["desiredState"]=json!("paused");other["cold"]=json!("X".repeat(100_000));
    d["jobs"]=json!([run,other]);
    (d,Context{run_id:"conductor-run".into(),lease_generation:1,actor})
}

fn schedule(d:&mut Value)->ApiResult<String>{
    let first=crate::list(d,"jobs").len();
    let scheduled=crate::engine_prepare::schedule(d,crate::engine_prepare::parse(&json!({"itemIds":["ready"]}))?)?;
    crate::conductor_authority::fence_new_jobs(d,first)?;
    Ok(scheduled.job_id)
}

#[tokio::test]
async fn preparation_projections_retain_only_current_grant_and_reject_grant_edits(){
    let(full,ctx)=fixture();let ordinary=schedule_projection(&full).unwrap();
    assert!(crate::row(&ordinary,"jobs",&ctx.run_id).is_err(),"default path keeps conductor history cold");
    with_context(ctx.clone(),async{
        for view in [schedule_projection(&full).unwrap(),projection_for(&full,Some("bound-child")).unwrap(),projection_of(&full).unwrap()]{
            assert_eq!(crate::row(&view,"jobs",&ctx.run_id).unwrap(),crate::row(&full,"jobs",&ctx.run_id).unwrap());
            assert!(crate::row(&view,"jobs","other-conductor").is_err());
        }
        let before=schedule_projection(&full).unwrap();let mut after=before.clone();let child=schedule(&mut after).unwrap();
        validate_schedule_change(&before,&after).unwrap();
        assert_eq!(crate::row(&after,"jobs",&child).unwrap()["conductorRunId"],ctx.run_id);
        for field in ["desiredState","leaseGeneration"]{
            let mut forged=after.clone();crate::row_mut(&mut forged,"jobs",&ctx.run_id).unwrap()["conductor"][field]=json!("changed");
            assert!(validate_schedule_change(&before,&forged).is_err());
        }
        let mut forged=after.clone();crate::row_mut(&mut forged,"jobs",&child).unwrap()["grantGeneration"]=json!(2);
        assert!(validate_schedule_change(&before,&forged).is_err());
    }).await;
}

async fn exercise(db:&Database){
    let(full,ctx)=fixture();db.change(|d|{*d=full;Ok(())}).await.unwrap();let baseline=db.read().await.unwrap();
    let child=with_context(ctx.clone(),db.change_preparation_schedule_observed(schedule)).await.unwrap().0;
    let saved=db.read().await.unwrap();assert_eq!(crate::row(&saved,"jobs",&ctx.run_id).unwrap(),crate::row(&baseline,"jobs",&ctx.run_id).unwrap());
    with_context(ctx.clone(),async{
        let view=db.read_preparation_context(&child).await.unwrap();
        assert_eq!(crate::row(&view,"jobs",&ctx.run_id).unwrap(),crate::row(&saved,"jobs",&ctx.run_id).unwrap());
        assert!(crate::row(&view,"jobs","other-conductor").is_err());
        db.change_preparation_first_observed(&child,|d|{
            assert!(crate::row(d,"jobs",&ctx.run_id).is_ok());
            crate::row_mut(d,"jobs",&child)?["preparationStages"]["first"]=json!({"status":"completed","result":{}});Ok(())
        }).await.unwrap();
    }).await;
    db.change_job_observed(&ctx.run_id,|d|{crate::row_mut(d,"jobs",&ctx.run_id)?["conductor"]["desiredState"]=json!("paused");Ok(())}).await.unwrap();
    let paused=db.read().await.unwrap();
    assert!(with_context(ctx.clone(),db.change_preparation_schedule_observed(schedule)).await.is_err());
    assert_eq!(db.read().await.unwrap(),paused,"paused grant cannot append a new paid preparation job");
    with_context(ctx.clone(),async{
        let view=db.read_preparation_context(&child).await.unwrap();
        assert_eq!(crate::row(&view,"jobs",&ctx.run_id).unwrap()["conductor"]["desiredState"],"paused");
        // A scope no-op is still readable/settleable after pause; loaders do not
        // impose a running-grant check on existing in-flight evidence.
        let(_,changed)=db.change_preparation_first_observed(&child,|_|Ok(())).await.unwrap();assert!(!changed);
    }).await;
    assert_eq!(db.read().await.unwrap(),paused);
}

#[tokio::test]
async fn sqlite_preparation_conductor_scope_admission_and_pause_are_atomic(){
    let folder=tempfile::tempdir().unwrap();let db=Database::Sqlite(crate::open_db(&folder.path().join("fixture.sqlite")).await.unwrap());
    exercise(&db).await;db.close().await;
}

#[tokio::test]
#[ignore="requires a pristine explicitly isolated communityhero_writer_v51_test_ PostgreSQL fixture"]
async fn postgres_preparation_conductor_scope_admission_and_pause_are_atomic(){
    let db=writer_v51_fixture_db().await;exercise(&db).await;db.close().await;
}
