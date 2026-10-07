use super::*;
use serde_json::json;

fn operation(id:&str, binding:Value, conversation:&str)->Value {
    json!({"id":id,"status":"unknown","target":{"connectorBinding":binding},
        "action":{"action":"reply_and_close","conversationKey":conversation}})
}
fn fixture(profile:crate::accounts::Profile, conversation:&str)->(Value,Value) {
    let binding=profile.binding();let target=operation("self",binding.clone(),conversation);
    let mut foreign=binding.clone();foreign["accountId"]=json!("another-account");
    let mut old_revision=binding.clone();old_revision["revision"]=json!(99);
    let mut closed=operation("closed",binding.clone(),conversation);closed["status"]=json!("succeeded");
    let mut close_action=operation("close-action",binding.clone(),conversation);close_action["action"]["action"]=json!("close");
    let mut first=operation("first",binding.clone(),conversation);
    first["executeReceipt"]=json!({"private":"DO_NOT_READ_RECEIPT".repeat(10000)});
    let data=json!({"account":profile.display(),"operations":[target.clone(),
        operation("foreign",foreign,conversation),operation("old-revision",old_revision,conversation),
        operation("another-thread",binding.clone(),"different"),closed,close_action,first,
        operation("second",binding,conversation)],
        "jobs":[{"private":"DO_NOT_READ_JOBS".repeat(10000)}],
        "conversations":[{"private":"DO_NOT_READ_CHAT".repeat(10000)}]});
    (data,target)
}
async fn sqlite()->(Database,tempfile::TempDir) {
    let temp=tempfile::tempdir().unwrap();let pool=crate::open_db(&temp.path().join("workspace.sqlite")).await.unwrap();
    (Database::Sqlite(pool),temp)
}
async fn seed(db:&Database,value:&Value) {
    let Database::Sqlite(pool)=db else {panic!("test sqlite only")};
    sqlx::query("UPDATE workspace SET payload=? WHERE id=1").bind(value.to_string()).execute(pool).await.unwrap();
}

#[tokio::test]
async fn reply_blocker_query_matches_domain_order_binding_self_and_conversation_without_bulk_payloads() {
    let (db,_temp)=sqlite().await;
    for profile in [crate::accounts::Profile::LikeAvto,crate::accounts::Profile::BawRussia] {
        for conversation in ["thread", "quote' OR 1=1 --", "ветка:😀"] {
            let (mut data,target)=fixture(profile,conversation);
            for _ in 0..3 {
                seed(&db,&data).await;
                assert_eq!(db.read_reply_conversation_blocker(&target).await.unwrap(),crate::dispatch_evidence::conversation_blocker(&data,&target));
                let Database::Sqlite(pool)=&db else {unreachable!()};
                let raw:String=sqlx::query_scalar(SQLITE_REPLY_BLOCKERS).bind(conversation).fetch_one(pool).await.unwrap();
                assert!(raw.len()<5000);assert!(!raw.contains("DO_NOT_READ"));
                if let Some(id)=crate::dispatch_evidence::conversation_blocker(&data,&target) {
                    data["operations"].as_array_mut().unwrap().iter_mut().find(|r|r["id"]==id).unwrap()["status"]=json!("succeeded");
                }
            }
        }
    }
    db.close().await;
}

#[tokio::test]
async fn reply_blocker_query_keeps_exact_json_binding_equality_including_legacy_null() {
    let (db,_temp)=sqlite().await;
    for binding in [Value::Null,json!({"connector":"native-future","accountId":"LikeAvto","revision":2}),json!({"revision":2,"extra":null})] {
        let target=operation("self",binding.clone(),"thread");
        let data=json!({"account":"LikeAvto","operations":[operation("legacy",binding,"thread")]});
        seed(&db,&data).await;
        assert_eq!(db.read_reply_conversation_blocker(&target).await.unwrap(),Some("legacy".into()));
        let mut other=target.clone();other["target"]["connectorBinding"]=json!({"revision":3});
        assert_eq!(db.read_reply_conversation_blocker(&other).await.unwrap(),None);
    }
    db.close().await;
}

#[tokio::test]
async fn reply_blocker_query_rejects_corrupt_selected_rows_source_and_unavailable_database() {
    let (db,_temp)=sqlite().await;let (baseline,target)=fixture(crate::accounts::Profile::LikeAvto,"thread");
    for mode in 0..6 {
        let mut data=baseline.clone();
        match mode {
            0=>data["operations"][6]["id"]=Value::Null,
            1=>data["operations"][7]["id"]=json!("first"),
            2=>data["operations"][6]["proposalId"]=json!({"not":"a string"}),
            3=>data["account"]=json!(7),
            4=>data["operations"]=json!({}),
            _=>data["operations"][6]["itemId"]=json!([]),
        }
        seed(&db,&data).await;
        assert!(db.read_reply_conversation_blocker(&target).await.is_err(),"mode {mode}");
    }
    let mut malformed=target.clone();malformed["action"]["conversationKey"]=Value::Null;
    assert!(db.read_reply_conversation_blocker(&malformed).await.is_err());
    db.close().await;
    assert!(db.read_reply_conversation_blocker(&target).await.is_err());
    let mut close=target;close["action"]["action"]=json!("close");
    assert_eq!(db.read_reply_conversation_blocker(&close).await.unwrap(),None);
}

#[test]
fn reply_blocker_candidate_projection_rejects_relational_drift_and_keeps_status_semantics() {
    let target=operation("self",Value::Null,"thread");
    let mut prior=operation("prior",Value::Null,"thread");prior["relationalValid"]=json!(false);
    assert!(reply_blocker_from_candidates(json!([prior.clone()]),&target).is_err());
    prior["relationalValid"]=json!(true);prior["status"]=json!("dispatching");
    assert_eq!(reply_blocker_from_candidates(json!([prior]),&target).unwrap(),None);
}

// Uses only CTE fixtures in one read-only SELECT: no owner lease, schema creation,
// live rows, temp tables or model/provider calls. Root may run on its clone.
#[tokio::test]
#[ignore="requires explicitly isolated PostgreSQL URL for read-only SQL fixture"]
async fn postgres_reply_blocker_sql_fixture_parity_and_projection_corruption() {
    let url=std::env::var("COMMUNITYHERO_DISPATCH_READONLY_URL").expect("explicit isolated URL required");
    let expected=std::env::var("COMMUNITYHERO_DISPATCH_READONLY_DATABASE").expect("explicit database required");
    let options=url.parse::<sqlx::postgres::PgConnectOptions>().unwrap()
        .password(&std::env::var("PGPASSWORD").expect("transient password required"));
    let pool=PgPoolOptions::new().max_connections(1).after_connect(|c,_|Box::pin(async move{
        sqlx::query("SET default_transaction_read_only=on").execute(&mut *c).await?;
        sqlx::query("SET statement_timeout='5s'").execute(c).await?;Ok(())
    })).connect_with(options).await.unwrap_or_else(|_|panic!("isolated read connection failed"));
    let (database,readonly):(String,String)=sqlx::query_as("SELECT current_database(),current_setting('default_transaction_read_only')").fetch_one(&pool).await.unwrap();
    assert_eq!(database,expected);assert_eq!(readonly,"on");
    let sql=format!(r#"WITH test_workspaces AS (
      SELECT id,metadata,account,execution_enabled FROM jsonb_to_recordset($3::jsonb->'workspaces')
      AS w(id text,metadata jsonb,account text,execution_enabled boolean)),
      test_operations AS (SELECT workspace_id,id,ordinal,payload,status,item_id,proposal_id,approval_id
      FROM jsonb_to_recordset($3::jsonb->'operations') AS o(workspace_id text,id text,ordinal integer,payload jsonb,status text,item_id text,proposal_id text,approval_id text))
      {}"#,PG_REPLY_BLOCKERS.replace("communityhero.workspaces","test_workspaces").replace("communityhero.operations","test_operations"));
    let (data,target)=fixture(crate::accounts::Profile::BawRussia,"quote' OR 1=1 --");
    let operations:Vec<_>=data["operations"].as_array().unwrap().iter().enumerate().map(|(n,p)|json!({"workspace_id":WORKSPACE,"id":p["id"],"ordinal":n,"payload":p,"status":p["status"]})).collect();
    let baseline=json!({"workspaces":[{"id":WORKSPACE,"metadata":{"account":"BAW Russia"},"account":"BAW Russia","execution_enabled":false}],"operations":operations});
    for mode in 0..7 {
        let mut fixture=baseline.clone();
        match mode {
            1=>fixture["operations"][6]["status"]=json!("succeeded"),
            2=>fixture["operations"][6]["payload"]["status"]=json!("succeeded"),
            3=>fixture["operations"][6]["payload"]["id"]=json!("changed"),
            4=>fixture["workspaces"][0]["account"]=json!("LikeAvto"),
            5=>fixture["workspaces"][0]["execution_enabled"]=json!(true),
            6=>{for row in fixture["operations"].as_array_mut().unwrap(){row["workspace_id"]=json!("other-workspace");}},
            _=>{}
        }
        let record=sqlx::query(sqlx::AssertSqlSafe(sql.as_str())).bind(WORKSPACE).bind(target["action"]["conversationKey"].as_str().unwrap()).bind(fixture.to_string()).fetch_one(&pool).await.unwrap();
        let result=postgres_guard(&record).and_then(|()|reply_blocker_from_candidates(parse(record.try_get::<&str,_>("operations")?)?,&target));
        if mode==0 {assert_eq!(result.unwrap(),crate::dispatch_evidence::conversation_blocker(&data,&target));}
        else if mode==6 {assert_eq!(result.unwrap(),None);}
        else {assert!(result.is_err(),"mode {mode}");}
    }
    pool.close().await;
}
