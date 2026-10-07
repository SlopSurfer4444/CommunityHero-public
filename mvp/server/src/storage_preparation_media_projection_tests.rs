//! Connected storage projections preserve complete, read-only exact-file media evidence.
use super::*;
const AT: &str = "2026-10-04T08:00:00Z";

fn media_rows(d: &Value) -> Vec<Value> {
    crate::list(d,"jobs").iter().filter(|j| matches!(j["kind"].as_str(),
        Some("media_analysis" | "media_analysis_applicability"))).cloned().collect()
}
fn ordinary(d: &mut Value) -> String {
    let id=crate::engine_prepare::schedule(d,crate::engine_prepare::Input{
        item_ids:vec!["ready".into()],instruction:None}).unwrap().job_id;
    crate::list_mut(d,"jobs").push(json!({"id":"unrelated-queued-assistant","kind":"assistant","status":"queued"}));
    id
}
fn structural(profile: crate::accounts::Profile) -> (Value,String) {
    let template=crate::engine_prepare::tests::fixture(false);
    let mut d=crate::empty();normalize(&mut d);crate::accounts::initialize(&mut d,profile).unwrap();
    for table in ["items","branches","posts"] {d[table]=template[table].clone();}
    let binding=crate::active_binding(&d).unwrap().to_json();
    for item in crate::list_mut(&mut d,"items") {item["connectorBinding"]=binding.clone();}
    let carriers=crate::media_analysis::test_workspace_fixture(profile.display());
    d["jobs"]=carriers["jobs"].clone();
    crate::knowledge::sync_catalog(&mut d,AT).unwrap();
    let id=ordinary(&mut d);(d,id)
}
fn cold() -> (Value,String) {
    let mut d=crate::media_analysis_reuse::test_cold_reused_workspace();normalize(&mut d);
    let template=crate::engine_prepare::tests::fixture(false);
    for table in ["items","branches","posts"] {
        crate::list_mut(&mut d,table).extend(crate::list(&template,table).iter().cloned());
    }
    // Include an independent completed analysis and an UNKNOWN attempt, with all
    // original requests/segments/results/CAS references. They grant no readiness.
    let historical=crate::media_analysis::test_workspace_fixture("LikeAvto");
    crate::list_mut(&mut d,"jobs").extend(crate::list(&historical,"jobs").iter().cloned());
    crate::knowledge::sync_catalog(&mut d,AT).unwrap();
    let id=ordinary(&mut d);(d,id)
}
fn retained(full:&Value,view:&Value) {
    assert_eq!(media_rows(view),media_rows(full),"full carrier and applicability payloads");
    assert_eq!(crate::media_analysis::ledger_from_workspace(view).unwrap(),
        crate::media_analysis::ledger_from_workspace(full).unwrap());
    let unknown=media_rows(view).into_iter().find(|j|j["analysis"]["attempts"].as_array()
        .is_some_and(|a|a.iter().any(|x|x["status"]=="unknown"))).unwrap();
    assert!(!unknown["analysis"]["attempts"][0]["originalRequest"].is_null());
    assert!(media_rows(view).iter().any(|j|j["kind"]=="media_analysis_applicability"
        &&!j["result"]["proof"]["result"].is_null()));
    crate::media_analysis::validate_workspace_change(view,view).unwrap();
}
fn reused(view:&Value) {
    crate::media_analysis_reuse::warm_workspace(view).unwrap();
    let target=crate::row(view,"posts","target").unwrap();
    let strict=crate::knowledge::TranscriptLookup::new(view,AT).unwrap().strict_media_evidence(target).unwrap();
    assert_eq!(strict["audioReady"],true);
    assert_eq!(strict["screenTextReady"],false);assert_eq!(strict["visualReady"],false);
    let selected=crate::knowledge::select(view,&[],std::slice::from_ref(target),AT).unwrap();
    assert!(crate::list(&selected,"materials").iter().any(|m|m["postKey"]=="vk:donor"
        &&m["exactFileAnalysisReuse"][0]["targetPostId"]=="target"));
}
fn plan_parity(full:&Value,view:&Value,job:&str) {
    assert!(crate::list(view,"jobs").iter().all(|j|j["kind"]!="assistant"),
        "ordinary planner omits both running and queued assistant bodies");
    assert!(crate::list(view,"scopeOwners").iter().any(|owner|owner["id"]==job));
    let input=json!({"itemIds":["ready"]});
    let expected=crate::prepare_plan::build(full,&input).unwrap();
    assert_eq!(expected,crate::prepare_plan::build(view,&input).unwrap());
    assert_eq!(expected["held"][0]["reason"],"preparation_scope_reserved");
}
fn mutate(view:&mut Value,kind:&str) {
    let before=view.clone();
    let row=crate::list_mut(view,"jobs").iter_mut().find(|j|j["kind"]==kind
        &&(kind!="media_analysis"||j["analysis"]["attempts"][0]["status"]=="unknown")).unwrap();
    if kind=="media_analysis" {row["analysis"]["attempts"][0]["status"]=json!("owned");}
    else {row["result"]["proof"]["target"]["postId"]=json!("retargeted");}
    assert_ne!(*view,before,"the guard exercise must change retained evidence");
}

#[test]
fn media_carriers_are_full_readonly_in_every_preparation_projection_for_both_companies() {
    for profile in [crate::accounts::Profile::LikeAvto,crate::accounts::Profile::BawRussia] {
        let (d,id)=structural(profile);let before=d.clone();
        let queue=super::super::projection_of(&d).unwrap();
        let schedule=super::super::schedule_projection(&d).unwrap();
        let bound=super::super::projection_for(&d,Some(&id)).unwrap();
        let plan=project(&d,&["ready".into()]).unwrap();
        for view in [&queue,&schedule,&bound,&plan] {retained(&d,view);}
        plan_parity(&d,&plan,&id);
        for kind in ["media_analysis","media_analysis_applicability"] {
            let mut changed=queue.clone();mutate(&mut changed,kind);
            assert!(super::super::validate_claim_change(&queue,&changed).is_err());
            let mut changed=schedule.clone();mutate(&mut changed,kind);
            assert!(super::super::validate_schedule_change(&schedule,&changed).is_err());
            let mut changed=bound.clone();mutate(&mut changed,kind);
            assert!(super::super::validate_first_stage_change(&bound,&changed,&id).is_err());
        }
        let mut foreign=plan.clone();foreign["account"]=json!(if profile==crate::accounts::Profile::LikeAvto {"BAW Russia"}else{"LikeAvto"});
        assert!(crate::media_analysis::validate_workspace_change(&plan,&foreign).is_err());
        assert_eq!(d,before);
    }
}
async fn actual_readers(db:&Database,job:&str) {
    let before=db.read().await.unwrap();
    let (queue,changed)=db.change_preparation_claim_observed(|view|Ok(view.clone())).await.unwrap();
    assert!(!changed);assert_eq!(queue,super::super::projection_of(&before).unwrap());
    let schedule=db.read_preparation_schedule().await.unwrap();
    assert_eq!(schedule,super::super::schedule_projection(&before).unwrap());
    let bound=db.read_preparation_context(job).await.unwrap();
    assert_eq!(bound,super::super::projection_for(&before,Some(job)).unwrap());
    let plan=db.read_preparation_plan(&["ready".into()]).await.unwrap();
    assert_eq!(plan,project(&before,&["ready".into()]).unwrap());
    for view in [&queue,&schedule,&bound,&plan] {retained(&before,view);reused(view);}
    plan_parity(&before,&plan,job);
    for kind in ["media_analysis","media_analysis_applicability"] {
        assert!(db.change_preparation_claim_observed(|view|{mutate(view,kind);Ok(())}).await.is_err());
        assert!(db.change_preparation_schedule_observed(|view|{mutate(view,kind);Ok(())}).await.is_err());
        assert!(db.change_preparation_first_observed(job,|view|{mutate(view,kind);Ok(())}).await.is_err());
        assert_eq!(db.read().await.unwrap(),before,"rejected media mutation rolls back");
    }
    assert_eq!(db.read().await.unwrap(),before,"warming changes only the exact proof cache");
}

#[tokio::test]
async fn sqlite_media_evidence_reaches_real_preparation_readers_and_knowledge_consumer() {
    let folder=tempfile::tempdir().unwrap();let pool=crate::open_db(&folder.path().join("workspace.sqlite")).await.unwrap();
    let db=Database::Sqlite(pool.clone());let (d,id)=cold();
    sqlx::query("UPDATE workspace SET payload=? WHERE id=1").bind(d.to_string()).execute(&pool).await.unwrap();
    actual_readers(&db,&id).await;db.close().await;
}

#[tokio::test]
#[ignore = "requires fresh isolated writer_v51 PostgreSQL fixture; run alone"]
async fn postgres_media_evidence_reaches_real_preparation_readers_with_physical_projection_gates() {
    let db=super::super::writer_v51_fixture_db().await;let (d,id)=cold();
    db.change(|state|{*state=d.clone();Ok(())}).await.unwrap();
    actual_readers(&db,&id).await;
    let stored=db.read().await.unwrap();
    let carrier=media_rows(&stored).into_iter().find(|j|j["kind"]=="media_analysis").unwrap();
    let carrier_id=carrier["id"].as_str().unwrap();
    let Database::Postgres{writer,..}=&db else {panic!("PostgreSQL fixture required")};
    for update in ["kind='assistant'","status='running'","id=id||'-physical-mismatch'"] {
        let mut tx=writer.begin().await.unwrap();
        let statement=format!("UPDATE communityhero.jobs SET {update} WHERE workspace_id=$1 AND id=$2");
        sqlx::query(sqlx::AssertSqlSafe(statement.as_str())).bind(WORKSPACE).bind(carrier_id).execute(&mut *tx).await.unwrap();
        assert!(load(&mut tx,&["ready".into()],false).await.is_err(),"plan: {update}");
        assert!(super::super::load_pg_preparation(&mut tx,None,false,false).await.is_err(),"queue: {update}");
        assert!(super::super::load_pg_preparation(&mut tx,None,true,false).await.is_err(),"schedule: {update}");
        assert!(super::super::load_pg_preparation(&mut tx,Some(&id),false,false).await.is_err(),"bound: {update}");
        tx.rollback().await.unwrap();
    }
    assert_eq!(db.read().await.unwrap(),stored);db.close().await;
}
