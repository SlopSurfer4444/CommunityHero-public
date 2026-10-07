//! SOURCEONLY candidate rehearsal. Not run by this audit. ROOT may run only
//! against an explicitly provisioned disposable writer-v51 fixture.
use super::*;

#[tokio::test]
#[ignore = "requires ROOT-owned fresh disposable writer-v51 PostgreSQL fixture"]
async fn status_route_join_preserves_membership_and_rejects_identity_drift() {
    let db = super::super::writer_v51_fixture_db().await;
    let Database::Postgres { writer, .. } = &db else { unreachable!() };
    // Same provider alias may correspond to several retained canonical rows.
    // Preserve numeric JSON textualization from the old PG ->> comparison.
    for (ordinal, id, object_id, item_id) in [
        (0, "one", json!("o"), json!("42")),
        (1, "other-object", json!("other"), json!("42")),
        (2, "two", json!("o"), json!("42")),
        (3, "numeric-legacy", json!("o"), json!(42)),
    ] {
        let payload = json!({"id":id,"objectId":object_id,"itemId":item_id,"draft":"retained"});
        sqlx::query("INSERT INTO communityhero.items(workspace_id,id,ordinal,payload) VALUES($1,$2,$3,$4::jsonb)")
            .bind(WORKSPACE).bind(id).bind(ordinal).bind(payload.to_string())
            .execute(writer).await.unwrap();
    }
    let routes = [json!({"objectId":"o","itemId":"42"}), json!({"objectId":"o","itemId":"42"})];
    let (_, changed) = db.change_status_observed(&routes, |scope| {
        let ids = rows(scope,"items")?.iter().map(|item|item["id"].as_str().unwrap()).collect::<Vec<_>>();
        assert_eq!(ids, vec!["one","two","numeric-legacy"]);
        assert!(rows(scope,"items")?.iter().all(|item|item["draft"]=="retained"));
        Ok(())
    }).await.unwrap();
    assert!(!changed);
    assert!(!db.change_status_observed(&[], |scope| { assert!(rows(scope,"items")?.is_empty()); Ok(()) }).await.unwrap().1);

    // Physical id/payload id mismatch must still reject before the closure.
    sqlx::query("UPDATE communityhero.items SET payload=jsonb_set(payload,'{id}','\"forged\"'::jsonb) WHERE workspace_id=$1 AND id='one'")
        .bind(WORKSPACE).execute(writer).await.unwrap();
    let called = std::cell::Cell::new(false);
    assert!(db.change_status_observed(&routes, |_| { called.set(true); Ok(()) }).await.is_err());
    assert!(!called.get());
    sqlx::query("UPDATE communityhero.items SET payload=jsonb_set(payload,'{id}',to_jsonb(id)) WHERE workspace_id=$1 AND id='one'")
        .bind(WORKSPACE).execute(writer).await.unwrap();

    // Company mismatch remains rejected by the locked metadata guard.
    sqlx::query("UPDATE communityhero.workspaces SET metadata=metadata||jsonb_build_object('account','BAW') WHERE id=$1")
        .bind(WORKSPACE).execute(writer).await.unwrap();
    let called = std::cell::Cell::new(false);
    assert!(db.change_status_observed(&routes, |_| { called.set(true); Ok(()) }).await.is_err());
    assert!(!called.get());
    db.close().await;
}

// R9 additions: UNRUN. The fixture helper refuses non-local/non-test databases
// and requires a fresh schema for each ignored test; ROOT owns execution.
#[tokio::test]
#[ignore = "requires ROOT-owned fresh disposable writer-v51 PostgreSQL fixture"]
async fn status_route_join_matches_old_query_for_scalar_text_routes_and_foreign_workspace() {
    let db = super::super::writer_v51_fixture_db().await;
    let Database::Postgres { writer, .. } = &db else { unreachable!() };
    sqlx::query("INSERT INTO communityhero.migration_imports(id,source_sha256,payload) VALUES('foreign-import',$1,'{}'::jsonb)")
        .bind("f".repeat(64)).execute(writer).await.unwrap();
    sqlx::query("INSERT INTO communityhero.workspaces(id,account,import_id,metadata) VALUES('foreign-company','BAW','foreign-import','{}'::jsonb)")
        .execute(writer).await.unwrap();
    let values = [Value::Null,json!("o"),json!(42),json!(true),json!([1,2]),json!({"x":1})];
    for ordinal in 0..96_i32 {
        let mut payload = json!({"id":format!("row-{ordinal}"),"draft":"retained"});
        if ordinal % 7 != 0 { payload["objectId"] = values[(ordinal as usize / 3) % values.len()].clone(); }
        if ordinal % 11 != 0 { payload["itemId"] = values[ordinal as usize % values.len()].clone(); }
        sqlx::query("INSERT INTO communityhero.items(workspace_id,id,ordinal,payload) VALUES($1,$2,$3,$4::jsonb)")
            .bind(WORKSPACE).bind(payload["id"].as_str().unwrap()).bind(ordinal).bind(payload.to_string())
            .execute(writer).await.unwrap();
        let mut foreign_payload = payload.clone();
        foreign_payload["id"] = json!(format!("foreign-{ordinal}"));
        sqlx::query("INSERT INTO communityhero.items(workspace_id,id,ordinal,payload) VALUES('foreign-company',$1,$2,$3::jsonb)")
            .bind(foreign_payload["id"].as_str().unwrap()).bind(ordinal).bind(foreign_payload.to_string())
            .execute(writer).await.unwrap();
    }
    // Use PG itself as the reference for ->> text extraction, including legacy
    // nonstring JSON values. No fixture-side stringification guesses authority.
    let payloads = sqlx::query("SELECT payload::text FROM communityhero.items WHERE workspace_id=$1 ORDER BY ordinal")
        .bind(WORKSPACE).fetch_all(writer).await.unwrap();
    let mut all_routes = Vec::new();
    for row in payloads {
        let payload: Value = serde_json::from_str(row.try_get::<&str,_>("payload").unwrap()).unwrap();
        all_routes.push(json!({"objectId":payload["objectId"],"itemId":payload["itemId"]}));
    }
    all_routes.extend(all_routes.clone());
    let cases = vec![json!([]),json!([{}]),json!([{"objectId":null,"itemId":null}]),
        json!([{"objectId":"o","itemId":"42"},{"objectId":"o","itemId":"42"}]),Value::Array(all_routes)];
    for routes in cases {
        let old = sqlx::query("SELECT id,payload::text FROM communityhero.items WHERE workspace_id=$1 AND EXISTS (SELECT 1 FROM jsonb_array_elements($2::jsonb) r WHERE payload->>'objectId'=r->>'objectId' AND payload->>'itemId'=r->>'itemId') ORDER BY ordinal LIMIT 2401")
            .bind(WORKSPACE).bind(routes.to_string()).fetch_all(writer).await.unwrap();
        let expected = old.iter().map(|row|serde_json::from_str::<Value>(row.try_get::<&str,_>("payload").unwrap()).unwrap()).collect::<Vec<_>>();
        let mut connection = writer.acquire().await.unwrap();
        let actual = load_records(&mut connection, "items", "unused", None, Some(&routes)).await.unwrap();
        assert_eq!(actual, expected, "old SQL and connected candidate loader must preserve ids, order, payload and workspace scope");
        assert!(actual.iter().all(|row|row["id"].as_str().unwrap().starts_with("row-")));
    }
    db.close().await;
}

#[tokio::test]
#[ignore = "requires ROOT-owned fresh disposable writer-v51 PostgreSQL fixture"]
async fn status_route_join_preserves_2400_limit_and_rejects_2401_before_closure() {
    let db = super::super::writer_v51_fixture_db().await;
    let Database::Postgres { writer, .. } = &db else { unreachable!() };
    sqlx::query("INSERT INTO communityhero.items(workspace_id,id,ordinal,payload) SELECT $1,'boundary-'||n,n,jsonb_build_object('id','boundary-'||n,'objectId','o','itemId','42','draft','retained') FROM generate_series(0,2399) n")
        .bind(WORKSPACE).execute(writer).await.unwrap();
    let routes = vec![json!({"objectId":"o","itemId":"42"});2400];
    let (_, changed) = db.change_status_observed(&routes, |scope| {
        assert_eq!(rows(scope,"items")?.len(),2400);
        assert_eq!(rows(scope,"items")?[0]["id"],"boundary-0");
        assert_eq!(rows(scope,"items")?[2399]["id"],"boundary-2399");
        Ok(())
    }).await.unwrap();
    assert!(!changed);
    let before_input = db.read().await.unwrap();
    let too_many_routes = vec![json!({"objectId":"o","itemId":"42"});2401];
    let called = std::cell::Cell::new(false);
    assert!(db.change_status_observed(&too_many_routes, |_| {called.set(true);Ok(())}).await.is_err());
    assert!(!called.get());
    assert_eq!(db.read().await.unwrap(),before_input,"2401 input routes must reject before closure without mutation");
    sqlx::query("INSERT INTO communityhero.items(workspace_id,id,ordinal,payload) VALUES($1,'boundary-2400',2400,'{\"id\":\"boundary-2400\",\"objectId\":\"o\",\"itemId\":\"42\",\"draft\":\"retained\"}'::jsonb)")
        .bind(WORKSPACE).execute(writer).await.unwrap();
    let before = db.read().await.unwrap();
    let called = std::cell::Cell::new(false);
    assert!(db.change_status_observed(&routes, |_| {called.set(true);Ok(())}).await.is_err());
    assert!(!called.get());
    assert_eq!(db.read().await.unwrap(),before,"over-limit failure must not write history or payloads");
    db.close().await;
}
