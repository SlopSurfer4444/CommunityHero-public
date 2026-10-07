//! Item writers need current locked lifecycle context, without lifecycle write authority.
use super::*;
use serde_json::json;

fn document()->Value {
    json!({"account":"LikeAvto","items":[{"id":"one","postId":"p","branchId":"b","revision":1,"draft":"old"},
        {"id":"two","revision":7,"draft":"untouched"}],"operations":[],"proposals":[],"feedback":[],
        "privateMetadata":{"body":"cold history"},"jobs":[{"id":"private-job","result":{"body":"retained"}}]})
}
#[test]
fn item_lifecycle_projection_preserves_exact_presence_and_readonly_value() {
    for value in [None,Some(Value::Null),Some(json!({"owner":{"future":null},"history":[{"exact":"retained"}]}))] {
        let mut d=document();if let Some(value)=value {d["runtimeLifecycle"]=value;}
        let before=item_scope(&d,"one").unwrap();
        assert_eq!(before.get("runtimeLifecycle"),d.get("runtimeLifecycle"));
        assert_eq!(before.as_object().unwrap().len(),5+usize::from(d.get("runtimeLifecycle").is_some()));
        for omitted in ["privateMetadata","jobs"] {assert!(before.get(omitted).is_none());}
        validate_item_scope(&before,&before).unwrap();
        let mut edit=before.clone();edit["items"][0]["draft"]=json!("edited");validate_item_scope(&before,&edit).unwrap();
        for mode in 0..3 {
            let mut after=before.clone();match mode {
                0=>{after.as_object_mut().unwrap().remove("runtimeLifecycle");},
                1=>after["runtimeLifecycle"]=json!({"owner":"forged"}),
                _=>after["runtimeLifecycle"]=Value::Null,
            }
            if after!=before {assert!(validate_item_scope(&before,&after).is_err());}
        }
        let mut after=before.clone();after["unexpectedMetadata"]=Value::Null;
        assert!(validate_item_scope(&before,&after).is_err());
    }
}
async fn native_item_checks(app:&crate::App) {
    let before=app.read().await.unwrap();let key=before["items"][0]["id"].as_str().unwrap().to_owned();
    let lifecycle=before["runtimeLifecycle"].clone();
    app.change_item(&key,|view| {
        assert_eq!(view["runtimeLifecycle"],lifecycle);
        assert_eq!(*view,item_scope(&before,&key).unwrap());
        view["items"][0]["draft"]=json!("native item edit");
        crate::list_mut(view,"feedback").push(json!({"id":"native-item-feedback","itemId":key,"kind":"operator_edit"}));
        Ok(())
    }).await.unwrap();
    let edited=app.read().await.unwrap();assert_eq!(edited["runtimeLifecycle"],lifecycle);
    assert_eq!(edited["items"][0]["draft"],"native item edit");
    assert_eq!(edited["feedback"].as_array().unwrap().len(),before["feedback"].as_array().unwrap().len()+1);
    for mode in 0..5 {
        let result=app.db.change_item_observed(&key,|view| {
            match mode {
                0=>{view.as_object_mut().unwrap().remove("runtimeLifecycle");},
                1=>view["runtimeLifecycle"]=Value::Null,
                2=>view["runtimeLifecycle"]["history"].as_array_mut().unwrap().push(json!({"forged":true})),
                3=>view["items"][0]["id"]=json!("retargeted"),
                _=>view["feedback"][0]["kind"]=json!("history-rewritten"),
            }Ok(())
        }).await;
        assert!(result.is_err());assert_eq!(app.read().await.unwrap(),edited,"rejected callback must roll back");
    }
    let mut foreign=(*app).clone();foreign.lifecycle_owner=std::sync::Arc::new(crate::runtime_lifecycle::RuntimeIdentity{
        runtime_id:"foreign-fixed-owner".into(),..(*app.lifecycle_owner).clone()});
    assert!(foreign.change_item(&key,|view|{view["items"][0]["draft"]=json!("foreign");Ok(())}).await.is_err());
    assert_eq!(app.read().await.unwrap(),edited);

    let capture=crate::runtime_lifecycle_app::Capture::read(app).await;
    let owner=crate::runtime_lifecycle::current_owner(&edited,&app.lifecycle_owner).unwrap();
    let drained=app.db.change_runtime_lifecycle_with_ledger(|d|
        crate::runtime_lifecycle::begin_drain(d,&owner,&"c".repeat(64),"item-scope-drain",false)).await.unwrap();
    let current=app.read().await.unwrap();assert_eq!(current["runtimeLifecycle"]["phase"],"draining");
    // Same fixed owner may complete ordinary edits during drain. This projection
    // must not add a phase veto or a way to append new runnable work.
    app.change_item(&key,|view|{assert_eq!(view["runtimeLifecycle"],current["runtimeLifecycle"]);
        view["items"][0]["draft"]=json!("completed during drain");Ok(())}).await.unwrap();
    let completed=app.read().await.unwrap();
    assert!(app.change_item(&key,|view|{view["jobs"]=json!([{"id":"new","kind":"assistant","status":"running"}]);Ok(())}).await.is_err());
    assert_eq!(app.read().await.unwrap(),completed);
    // Advance through real protected reducers after Capture::read. The old
    // fixed identity must lose against metadata loaded under the item lock.
    let native=crate::runtime_lifecycle::SettledNative{owner:drained.clone(),application_tasks:0,
        provider_queued:0,provider_dispatched:0,provider_contained:true,credential_writers:0,unresolved_effects:0};
    let transfer=app.db.change_runtime_lifecycle_with_ledger(|d|crate::runtime_lifecycle::mark_drained(d,&drained,&native)).await.unwrap();
    app.db.change_runtime_lifecycle_with_ledger(|d|crate::runtime_lifecycle::commit_stop_checkpoint(d,&drained,&transfer)).await.unwrap();
    app.db.change_runtime_lifecycle_with_ledger(|d|crate::runtime_lifecycle::accept_successor(d,&drained,&transfer,
        "item-scope-successor",&"c".repeat(64),&"e".repeat(64),false)).await.unwrap();
    let successor=app.read().await.unwrap();
    assert!(app.db.change_item_observed(&key,|view|capture.with(view,|view|{
        view["items"][0]["draft"]=json!("stale captured owner");Ok(())})).await.is_err());
    assert_eq!(app.read().await.unwrap(),successor,"fresh locked owner overrides old Capture");
    assert_eq!(successor["feedback"],completed["feedback"]);
}

#[tokio::test]
async fn sqlite_native_item_projection_keeps_owner_and_feedback_without_lifecycle_authority() {
    let (app,_temp)=crate::tests::test_app().await;native_item_checks(&app).await;app.db.close().await;
}

#[tokio::test]
async fn sqlite_item_lifecycle_absent_null_and_addition_rollback() {
    let folder=tempfile::tempdir().unwrap();let pool=crate::open_db(&folder.path().join("scope.sqlite")).await.unwrap();
    let db=Database::Sqlite(pool.clone());
    for lifecycle in [None,Some(Value::Null)] {
        let mut d=crate::empty();normalize(&mut d);d["items"]=document()["items"].clone();
        if let Some(value)=lifecycle {d["runtimeLifecycle"]=value;}
        sqlx::query("UPDATE workspace SET payload=? WHERE id=1").bind(d.to_string()).execute(&pool).await.unwrap();
        let before=db.read().await.unwrap();
        let (seen,changed)=db.change_item_observed("one",|view|Ok(view.clone())).await.unwrap();
        assert!(!changed);assert_eq!(seen,item_scope(&before,"one").unwrap());
        assert!(db.change_item_observed("one",|view|{view["runtimeLifecycle"]=json!({"owner":"forged"});Ok(())}).await.is_err());
        assert_eq!(db.read().await.unwrap(),before);
    }
    db.close().await;
}

#[tokio::test]
#[ignore="requires fresh isolated writer_v51 PostgreSQL fixture; run alone"]
async fn postgres_native_item_projection_uses_locked_lifecycle_and_rolls_back() {
    let (mut app,_temp)=crate::tests::test_app().await;
    let db=writer_v51_fixture_db().await;let sqlite_state=app.read().await.unwrap();
    db.change(|d|{crate::accounts::initialize(d,app.account)?;d["items"]=sqlite_state["items"].clone();Ok(())}).await.unwrap();
    let before=db.read().await.unwrap();
    let (seen,changed)=db.change_item_observed(before["items"][0]["id"].as_str().unwrap(),|view|Ok(view.clone())).await.unwrap();
    assert!(!changed);assert!(seen.get("runtimeLifecycle").is_none());assert_eq!(seen,item_scope(&before,before["items"][0]["id"].as_str().unwrap()).unwrap());
    assert!(db.change_item_observed(before["items"][0]["id"].as_str().unwrap(),|view|{view["runtimeLifecycle"]=Value::Null;Ok(())}).await.is_err());
    assert_eq!(db.read().await.unwrap(),before);
    // Explicit JSON null is distinct from an absent JSONB key. Exercise the
    // scalar extraction in the guarded isolated fixture, then restore absence
    // before initializing through the real lifecycle writer.
    let Database::Postgres{writer,..}=&db else {unreachable!()};
    sqlx::query("UPDATE communityhero.workspaces SET metadata=jsonb_set(metadata,'{runtimeLifecycle}','null'::jsonb,true) WHERE id=$1")
        .bind(WORKSPACE).execute(writer).await.unwrap();
    let null_state=db.read().await.unwrap();let key=before["items"][0]["id"].as_str().unwrap();
    let (seen,changed)=db.change_item_observed(key,|view|Ok(view.clone())).await.unwrap();
    assert!(!changed);assert_eq!(seen.get("runtimeLifecycle"),Some(&Value::Null));assert_eq!(seen,item_scope(&null_state,key).unwrap());
    assert!(db.change_item_observed(key,|view|{view.as_object_mut().unwrap().remove("runtimeLifecycle");Ok(())}).await.is_err());
    assert_eq!(db.read().await.unwrap(),null_state);
    sqlx::query("UPDATE communityhero.workspaces SET metadata=metadata-'runtimeLifecycle' WHERE id=$1").bind(WORKSPACE).execute(writer).await.unwrap();
    db.change_runtime_lifecycle_with_ledger(|d|crate::runtime_lifecycle_startup::initialize_fixture(d,&app.lifecycle_owner).map(|_|())).await.unwrap();
    let sqlite=std::mem::replace(&mut app.db,db);sqlite.close().await;
    native_item_checks(&app).await;app.db.close().await;
}
