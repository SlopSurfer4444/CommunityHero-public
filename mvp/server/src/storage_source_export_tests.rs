//! ROOT-executable connected native tests. Authored offline; not run by R9 tree.
use super::*;
use axum::extract::{State,Query};

fn fixture(profile:crate::accounts::Profile)->Value {
    let mut d=crate::empty();
    // Match the complete persisted workspace shape used by full reads.
    d["knowledge_entries"]=json!([]);d["knowledge_versions"]=json!([]);d["feedback"]=json!([]);
    d["account"]=json!(profile.display());d["connectorBinding"]=profile.binding();
    d["sync"]=json!({"cursor":null,"coverageComplete":false,"pages":12,"future":{"arr":[true,1,null,"1"]}});
    d["posts"]=json!([{"id":"p","title":"Исходная публикация","attachments":[{"type":"video","url":"fixture"}]}]);
    d["branches"]=json!([{"id":"b","postId":"p","messages":[{"id":"m","text":"Контекст"}],"observedMessages":[{"id":"m","text":"Exact capture"}]}]);
    d["items"]=json!([{"id":"i","postId":"p","branchId":"b","text":"Вопрос","draft":{"text":"Paid draft","editedByHuman":true},"revision":7}]);
    d["jobs"]=json!([{"id":"cold","kind":"assistant","status":"completed","result":{"private":"OMITTED_PAID_HISTORY".repeat(50_000)}}]);
    d["audit"]=json!([{"id":"history","action":"fixture","private":"OMITTED_AUDIT"}]);
    d
}

fn legacy_projection(d:&Value)->Value {
    json!({"account":d["account"],"sync":d["sync"],"items":d["items"],"posts":d["posts"],"branches":d["branches"]})
}

async fn seed_sqlite(db:&Database,d:&Value) {
    let Database::Sqlite(pool)=db else { panic!("SQLite fixture required") };
    sqlx::query("UPDATE workspace SET payload=? WHERE id=1").bind(d.to_string()).execute(pool).await.unwrap();
}

#[tokio::test]
async fn source_export_handler_preserves_both_company_records_without_cold_materialization() {
    let (app,_folder)=crate::tests::test_app().await;
    for profile in [crate::accounts::Profile::LikeAvto,crate::accounts::Profile::BawRussia] {
        let d=fixture(profile);seed_sqlite(&app.db,&d).await;
        let before=app.read().await.unwrap();
        let source=app.db.read_source_export_context().await.unwrap();
        assert_eq!(source,legacy_projection(&before));
        let export=crate::engine_api::export(State(app.clone())).await.unwrap().0;
        assert_eq!(export["account"],profile.key());
        assert_eq!(export["coverage"],before["sync"]);
        for table in SOURCE_TABLES {assert_eq!(export[table],before[table]);}
        assert_eq!(export["containsExecutableActions"],false);
        assert!(!export.to_string().contains("OMITTED_PAID_HISTORY"));
        assert!(!export.to_string().contains("OMITTED_AUDIT"));
        assert!(source.to_string().len()*100<before.to_string().len());
        assert_eq!(app.read().await.unwrap(),before);
    }
    app.db.close().await;
}

#[tokio::test]
async fn source_export_preserves_arbitrary_sync_json_types_and_missing_null() {
    let (app,_folder)=crate::tests::test_app().await;
    for sync in [json!(true),json!(false),json!(18446744073709551615_u64),json!("looks like [1]"),
        json!([true,false,null,{"closed":false}]),Value::Null] {
        let mut d=fixture(crate::accounts::Profile::LikeAvto);d["sync"]=sync;
        seed_sqlite(&app.db,&d).await;
        assert_eq!(app.db.read_source_export_context().await.unwrap(),legacy_projection(&app.read().await.unwrap()));
    }
    let mut d=fixture(crate::accounts::Profile::LikeAvto);d.as_object_mut().unwrap().remove("sync");
    seed_sqlite(&app.db,&d).await;
    assert_eq!(app.db.read_source_export_context().await.unwrap()["sync"],Value::Null);
    app.db.close().await;
}

#[tokio::test]
async fn source_export_rejects_bad_selected_identity_instead_of_returning_partial_data() {
    let (app,_folder)=crate::tests::test_app().await;
    let d=fixture(crate::accounts::Profile::LikeAvto);
    for mutation in 0..5 {
        let mut bad=d.clone();
        match mutation {
            0=>{let duplicate=bad["items"][0].clone();bad["items"].as_array_mut().unwrap().push(duplicate);},
            1=>bad["items"][0]["branchId"]=json!("foreign-branch"),
            2=>bad["branches"][0]["postId"]=json!("foreign-post"),
            3=>bad["items"][0]["postId"]=json!(17),
            _=>bad["items"][0]["id"]=json!("   "),
        }
        seed_sqlite(&app.db,&bad).await;
        assert!(app.db.read_source_export_context().await.is_err());
        assert_eq!(app.read().await.unwrap(),bad);
    }
    app.db.close().await;
}

#[tokio::test]
async fn search_rejects_invalid_unicode_input_before_reader_acquisition() {
    let (app,_folder)=crate::tests::test_app().await;
    // A connected database read would fail with a storage error after close.
    // Invalid q must still yield the pre-existing BAD_REQUEST contract.
    app.db.close().await;
    for q in ["".to_owned()," x ".to_owned(),"я".repeat(301)," ".repeat(1000)] {
        let error=crate::operator_http::search(State(app.clone()),Query(HashMap::from([("q".into(),q)]))).await.unwrap_err();
        assert_eq!(error.0,axum::http::StatusCode::BAD_REQUEST);
        assert_eq!(error.1,"Search query must contain 2 to 300 characters");
    }
    assert!(crate::assistant_context::validate_search_query(&"я".repeat(300)).is_ok());
    assert!(crate::operator_http::search(State(app.clone()),Query(HashMap::from([("q".into(),"  привет  ".into())]))).await.is_err());
}

#[tokio::test]
async fn source_export_traces_numeric_selected_work_and_separate_wait_sql_decode() {
    let (app,_folder)=crate::tests::test_app().await;
    let d=fixture(crate::accounts::Profile::LikeAvto);seed_sqlite(&app.db,&d).await;
    let (value,events)=crate::performance::capture(app.db.read_source_export_context()).await;
    let value=value.unwrap();
    let total=events.iter().find(|event|event["stage"]=="source.export.read.total").unwrap();
    assert_eq!(total["work"]["rows"],3);assert_eq!(total["work"]["collections"],3);
    assert!(total["work"]["bytes"].as_u64().unwrap()<10_000);
    for stage in ["source.export.reader.wait","source.export.sql","source.export.decode","source.export.validation"] {
        assert!(events.iter().any(|event|event["stage"]==stage));
    }
    assert!(!json!(events).to_string().contains("Paid draft"));
    assert_eq!(value,legacy_projection(&d));app.db.close().await;
}

#[tokio::test]
#[ignore="ROOT-only fresh isolated writer_v51 PostgreSQL fixture; run alone"]
async fn postgres_source_export_parity_and_relational_corruption_fail_closed() {
    let db=crate::storage::writer_v51_fixture_db().await;
    let d=fixture(crate::accounts::Profile::LikeAvto);
    db.change(|workspace|{for key in ["sync","items","posts","branches","jobs","audit"] {
        workspace[key]=d[key].clone();
    }Ok(())}).await.unwrap();
    let before=db.read().await.unwrap();
    assert_eq!(db.read_source_export_context().await.unwrap(),legacy_projection(&before));
    let Database::Postgres{writer,..}=&db else {panic!("PostgreSQL fixture required")};
    sqlx::query("UPDATE communityhero.items SET branch_id=NULL WHERE workspace_id=$1 AND id='i'")
        .bind(WORKSPACE).execute(writer).await.unwrap();
    assert_eq!(db.read_source_export_context().await.unwrap_err().1,
        "Export relational identity or order mismatch");
    sqlx::query("UPDATE communityhero.items SET branch_id='b' WHERE workspace_id=$1 AND id='i'")
        .bind(WORKSPACE).execute(writer).await.unwrap();
    sqlx::query("UPDATE communityhero.items SET ordinal=7 WHERE workspace_id=$1 AND id='i'")
        .bind(WORKSPACE).execute(writer).await.unwrap();
    assert!(db.read_source_export_context().await.is_err());
    sqlx::query("UPDATE communityhero.items SET ordinal=0 WHERE workspace_id=$1 AND id='i'")
        .bind(WORKSPACE).execute(writer).await.unwrap();
    sqlx::query("UPDATE communityhero.workspaces SET metadata=jsonb_set(metadata,'{account}','\"BAW Russia\"'::jsonb) WHERE id=$1")
        .bind(WORKSPACE).execute(writer).await.unwrap();
    assert!(db.read_source_export_context().await.is_err());
    sqlx::query("UPDATE communityhero.workspaces SET metadata=jsonb_set(metadata,'{account}','\"LikeAvto\"'::jsonb) WHERE id=$1")
        .bind(WORKSPACE).execute(writer).await.unwrap();
    assert_eq!(db.read().await.unwrap(),before);
    db.close().await;
}
