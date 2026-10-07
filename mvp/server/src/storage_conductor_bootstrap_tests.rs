use super::*;

fn materials() -> Value {
    json!([
        {"id":"rules","kind":"instruction","text":"Exact company rules","unknownFutureField":{"retain":true}},
        {"id":"source","kind":"transcript","text":"Exact complete media text","sourceUrl":"https://example.invalid/source"},
        {"id":"legacy","kind":null,"body":{"also":"retained"}}
    ])
}
fn envelopes(values: &Value) -> Value {
    json!(values.as_array().unwrap().iter().map(|v| json!({"id":v["id"],"kind":v["kind"],"payload":v})).collect::<Vec<_>>())
}

#[test]
fn conductor_policy_preserves_all_metadata_and_material_bytes() {
    let metadata = json!({"account":"LikeAvto","connectorBinding":{"exact":true},
        "sync":{"openCoverage":{"coverageComplete":false}},"companyKnowledgeAuthority":{"current":"v2"},"future":{"retain":true}});
    let rows = materials();
    let mut expected = metadata.clone(); expected["materials"] = rows.clone();
    assert_eq!(decode_policy(&metadata.to_string(), &envelopes(&rows).to_string()).unwrap(), expected);
    assert_eq!(decode_policy(&metadata.to_string(), "[]").unwrap()["materials"], json!([]));
}

#[test]
fn conductor_policy_rejects_material_identity_projection_and_duplicate_drift() {
    let metadata = r#"{"account":"LikeAvto"}"#;
    for mutation in ["id", "kind", "payload-kind", "duplicate", "shape"] {
        let mut rows = envelopes(&materials());
        match mutation {
            "id" => rows[0]["id"] = json!("another"),
            "kind" => rows[0]["kind"] = json!("another"),
            "payload-kind" => {rows[0]["kind"] = json!(7); rows[0]["payload"]["kind"] = json!(7);},
            "duplicate" => {let first=rows[0].clone(); rows.as_array_mut().unwrap().push(first);},
            _ => rows = json!({"not":"an array"}),
        }
        assert!(decode_policy(metadata, &rows.to_string()).is_err(), "{mutation}");
    }
}

#[test]
fn conductor_policy_rejects_missing_account_or_hidden_entity_collections() {
    for metadata in [json!(null), json!({}), json!({"account":7}),
        json!({"account":"LikeAvto","jobs":[]}), json!({"account":"LikeAvto","materials":[]})] {
        assert!(decode_policy(&metadata.to_string(), "[]").is_err());
    }
}

async fn sqlite_fixture(d: &Value) -> Database {
    let pool=sqlx::sqlite::SqlitePoolOptions::new().max_connections(1).connect("sqlite::memory:").await.unwrap();
    sqlx::query("CREATE TABLE workspace(id INTEGER PRIMARY KEY,payload TEXT NOT NULL)").execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO workspace VALUES(1,?)").bind(d.to_string()).execute(&pool).await.unwrap();
    Database::Sqlite(pool)
}

#[tokio::test]
async fn conductor_policy_sqlite_matches_previous_inputs_without_unrelated_histories() {
    let mut d=crate::empty(); d["materials"]=materials();
    d["sync"]=json!({"openCoverage":{"coverageComplete":false,"scanId":"retained"}});
    d["companyKnowledgeAuthority"]=json!({"current":"exact"});
    for table in TABLES {if table!="materials" {d[table]=json!([{"id":format!("unrelated-{table}"),"large":"OMITTED_PRIVATE_HISTORY".repeat(12000)}]);}}
    let before=d.to_string(); let db=sqlite_fixture(&d).await;
    let got=db.read_conductor_bootstrap_policy().await.unwrap();
    let mut expected=metadata(&d); expected["materials"]=d["materials"].clone();
    assert_eq!(got,expected,"same metadata and complete materials used by the old bootstrap path");
    assert!(!got.to_string().contains("OMITTED_PRIVATE_HISTORY"));
    assert!(got.to_string().len()*100 < before.len());
    if let Database::Sqlite(pool)=&db {
        let after:String=sqlx::query_scalar("SELECT payload FROM workspace WHERE id=1").fetch_one(pool).await.unwrap();
        assert_eq!(after,before,"read must not compact, rewrite or retire historical data");
    }
    db.close().await;
    assert!(db.read_conductor_bootstrap_policy().await.is_err());
}

#[tokio::test]
async fn conductor_policy_sqlite_failure_releases_connection_and_does_not_mutate() {
    let mut d=crate::empty(); d["materials"]=json!([{"id":"duplicate"},{"id":"duplicate"}]);
    let before=d.to_string(); let db=sqlite_fixture(&d).await;
    assert!(db.read_conductor_bootstrap_policy().await.is_err());
    if let Database::Sqlite(pool)=&db {
        let after:String=sqlx::query_scalar("SELECT payload FROM workspace WHERE id=1").fetch_one(pool).await.unwrap();
        assert_eq!(after,before);
        sqlx::query("UPDATE workspace SET payload=? WHERE id=1").bind(json!({"account":"LikeAvto"}).to_string()).execute(pool).await.unwrap();
    }
    assert_eq!(db.read_conductor_bootstrap_policy().await.unwrap()["materials"],json!([]));
    db.close().await;
}

#[tokio::test]
#[ignore="requires a pristine explicitly isolated communityhero_writer_v51_test_ PostgreSQL fixture"]
async fn postgres_conductor_policy_parity_and_rejects_corrupt_inputs() {
    let db=crate::storage::preparation::writer_v51_fixture_db().await;
    // Retain migrated collections absent from the pre-storage empty document.
    let mut d=db.read().await.unwrap(); d["materials"]=materials();
    d["sync"]=json!({"openCoverage":{"coverageComplete":false,"scanId":"retained"}});
    d["companyKnowledgeAuthority"]=json!({"owner":"communityhero","account":"LikeAvto","companyKey":"likeavto"});
    d["unknownFutureField"]=json!({"nested":[null,true,"retain Unicode правила"]});
    d["posts"]=json!([{"id":"unrelated-post","text":"OMITTED_PRIVATE_HISTORY".repeat(12000)}]);
    d["jobs"]=json!([{"id":"unrelated-paid-history","kind":"assistant","status":"completed",
        "result":{"evidence":"OMITTED_PRIVATE_HISTORY".repeat(12000)}}]);
    crate::storage::validate(&d).expect("PG policy fixture retains normalized collections and references");
    db.change(|stored|{*stored=d.clone();Ok(())}).await.unwrap();
    let baseline=db.read().await.unwrap();
    let mut expected=metadata(&baseline); expected["materials"]=baseline["materials"].clone();
    let got=db.read_conductor_bootstrap_policy().await.unwrap();
    assert_eq!(got,expected,"actual PostgreSQL metadata and every ordered material equal the full reader");
    assert!(!got.to_string().contains("OMITTED_PRIVATE_HISTORY"));
    assert!(got.to_string().len()*100 < baseline.to_string().len());
    assert_eq!(db.read().await.unwrap(),baseline,"successful scoped read cannot rewrite full history");

    let Database::Postgres{writer,..}=&db else{unreachable!()};
    // Same material ID in another workspace must neither leak nor look like a
    // duplicate local record. This synthetic row is outside the selected account.
    sqlx::query("INSERT INTO communityhero.migration_imports(id,source_sha256,payload) VALUES('foreign-policy-fixture',$1,'{}'::jsonb)")
        .bind("1".repeat(64)).execute(writer).await.unwrap();
    sqlx::query("INSERT INTO communityhero.workspaces(id,account,import_id,metadata) VALUES('foreign-policy','BAW Russia','foreign-policy-fixture',$1::jsonb)")
        .bind(json!({"account":"BAW Russia"}).to_string()).execute(writer).await.unwrap();
    let foreign=json!({"id":"rules","kind":"rule","account":"BAW Russia","text":"FOREIGN_POLICY_MUST_NOT_LEAK"});
    sqlx::query("INSERT INTO communityhero.materials(workspace_id,id,ordinal,kind,payload) VALUES('foreign-policy','rules',0,'rule',$1::jsonb)")
        .bind(foreign.to_string()).execute(writer).await.unwrap();
    assert_eq!(db.read_conductor_bootstrap_policy().await.unwrap(),expected);
    let foreign_after:String=sqlx::query_scalar("SELECT payload::text FROM communityhero.materials WHERE workspace_id='foreign-policy' AND id='rules'")
        .fetch_one(writer).await.unwrap();
    assert_eq!(parse(&foreign_after).unwrap(),foreign);

    let original=baseline["materials"][0].clone();
    for mutation in ["identity","kind","kind-type"] {
        let mut broken=original.clone();
        match mutation {
            "identity"=>broken["id"]=json!("retargeted"),
            "kind"=>broken["kind"]=json!("rule"),
            _=>broken["kind"]=json!(7),
        }
        sqlx::query("UPDATE communityhero.materials SET payload=$1::jsonb WHERE workspace_id=$2 AND id='rules'")
            .bind(broken.to_string()).bind(WORKSPACE).execute(writer).await.unwrap();
        let before:(String,Option<String>)=sqlx::query_as("SELECT payload::text,kind FROM communityhero.materials WHERE workspace_id=$1 AND id='rules'")
            .bind(WORKSPACE).fetch_one(writer).await.unwrap();
        assert!(db.read_conductor_bootstrap_policy().await.is_err(),"reject {mutation}");
        let after:(String,Option<String>)=sqlx::query_as("SELECT payload::text,kind FROM communityhero.materials WHERE workspace_id=$1 AND id='rules'")
            .bind(WORKSPACE).fetch_one(writer).await.unwrap();
        assert_eq!(after,before,"failed read cannot repair or replace malformed material");
    }
    sqlx::query("UPDATE communityhero.materials SET payload=$1::jsonb WHERE workspace_id=$2 AND id='rules'")
        .bind(original.to_string()).bind(WORKSPACE).execute(writer).await.unwrap();
    sqlx::query("UPDATE communityhero.workspaces SET account='BAW Russia' WHERE id=$1")
        .bind(WORKSPACE).execute(writer).await.unwrap();
    assert!(db.read_conductor_bootstrap_policy().await.is_err(),"relational account must match metadata");
    let account:String=sqlx::query_scalar("SELECT account FROM communityhero.workspaces WHERE id=$1")
        .bind(WORKSPACE).fetch_one(writer).await.unwrap();
    assert_eq!(account,"BAW Russia","read cannot repair account drift");
    sqlx::query("UPDATE communityhero.workspaces SET account='LikeAvto' WHERE id=$1")
        .bind(WORKSPACE).execute(writer).await.unwrap();
    assert_eq!(db.read_conductor_bootstrap_policy().await.unwrap(),expected,"failure releases its connection and recovery reads exact original evidence");
    assert_eq!(db.read().await.unwrap(),baseline,"all selected workspace history remains unchanged");
    db.close().await;
}
