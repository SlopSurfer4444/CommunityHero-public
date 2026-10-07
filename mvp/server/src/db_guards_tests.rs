use super::*;
use crate::storage::Database;
use serde_json::json;
use sqlx::{PgConnection, postgres::PgPoolOptions};

fn approval() -> Value {
    json!({"id":"guard-approval","status":"approved","createdAt":"2026-09-22T00:00:00Z",
        "approvedBy":{"id":"operator-a"},
        "approvalAuthority":{"operatorId":"operator-a","generation":"original-generation"},
        "proposals":[{"id":"proposal-a","revision":2,
            "proposal":{"text":"Approved text","contextEvidenceDigest":"original-context"},
            "item":{"id":"item-a","itemId":"provider-recipient","revision":3}}]})
}
fn history() -> Value {
    // This validator also checks current workspace contracts. Seed a complete
    // isolated workspace while retaining the exact historical payloads.
    let mut workspace=crate::empty();
    workspace["audit"]=json!([{"id":"guard-audit","action":"approval.created","refId":"guard-approval","createdAt":"2026-09-22T00:00:00Z"}]);
    workspace["approvals"]=json!([approval()]);
    workspace
}

#[test]
fn immutable_history_rejects_payload_identity_context_authority_and_deletion() {
    let before = history();
    for pointer in [
        "/audit/0/action",
        "/audit/0/createdAt",
        "/audit/0/id",
        "/audit/0/refId",
        "/approvals/0/id",
        "/approvals/0/createdAt",
        "/approvals/0/approvedBy/id",
        "/approvals/0/approvalAuthority/generation",
        "/approvals/0/proposals/0/revision",
        "/approvals/0/proposals/0/proposal/text",
        "/approvals/0/proposals/0/proposal/contextEvidenceDigest",
        "/approvals/0/proposals/0/item/itemId",
    ] {
        let mut changed = before.clone();
        *changed.pointer_mut(pointer).unwrap() = json!("malicious mutation");
        assert!(
            validate_change(&before, &changed).is_err(),
            "accepted {pointer}"
        );
    }
    for collection in ["audit", "approvals"] {
        let mut changed = before.clone();
        changed[collection] = json!([]);
        assert!(validate_change(&before, &changed).is_err());
        changed.as_object_mut().unwrap().remove(collection);
        assert!(validate_change(&before, &changed).is_err());
        changed = before.clone();
        let duplicate = changed[collection][0].clone();
        changed[collection].as_array_mut().unwrap().push(duplicate);
        assert!(validate_change(&before, &changed).is_err());
    }
}

#[test]
fn approval_status_lifecycle_and_legacy_shapes_remain_supported() {
    let mut before = history();
    for status in [
        json!("consumed"),
        json!("cancelled"),
        json!("legacy-unknown"),
        Value::Null,
    ] {
        let mut after = before.clone();
        after["approvals"][0]["status"] = status;
        assert!(validate_change(&before, &after).is_ok());
        before = after;
    }
    let mut after = before.clone();
    after["audit"]
        .as_array_mut()
        .unwrap()
        .push(json!({"id":"second-audit"}));
    after["approvals"]
        .as_array_mut()
        .unwrap()
        .push(json!({"id":"legacy-approval","proposals":[]}));
    assert!(validate_change(&before, &after).is_ok());
    // Only the two history collections are omitted from this legacy bounded
    // shape; unrelated workspace collections remain complete.
    let mut legacy=crate::empty();
    for collection in ["audit","approvals"] {
        legacy.as_object_mut().unwrap().remove(collection);
    }
    assert!(validate_change(&legacy, &legacy).is_ok());
    after["approvals"][0]["status"] = json!(4);
    assert!(validate_change(&before, &after).is_err());
    let mut after = before.clone();
    after["audit"][0]["newField"] = json!(true);
    assert!(validate_change(&before, &after).is_err());
    let mut after = before.clone();
    after["approvals"][0]["futureAuthority"] = json!("new-token");
    assert!(validate_change(&before, &after).is_err());
}

#[tokio::test]
async fn sqlite_history_mutation_rolls_back_entire_workspace_transaction() {
    let temp = tempfile::tempdir().unwrap();
    let pool = crate::open_db(&temp.path().join("guard.sqlite"))
        .await
        .unwrap();
    let db = Database::Sqlite(pool);
    db.change(|workspace| {
        workspace["audit"] = history()["audit"].clone();
        workspace["approvals"] = history()["approvals"].clone();
        Ok(())
    })
    .await
    .unwrap();
    let before = db.read().await.unwrap();
    for collection in ["audit", "approvals"] {
        assert!(
            db.change(|workspace| {
                workspace["settings"]["transactionCanary"] = json!("must roll back");
                workspace[collection][0]["createdAt"] = json!("forged");
                Ok(())
            })
            .await
            .is_err()
        );
        assert_eq!(db.read().await.unwrap(), before);
        assert!(
            db.change(|workspace| {
                workspace[collection] = json!([]);
                Ok(())
            })
            .await
            .is_err()
        );
        assert_eq!(db.read().await.unwrap(), before);
    }
    db.change(|workspace| {
        workspace["approvals"][0]["status"] = json!("consumed");
        crate::audit(workspace, "operation.admitted", "fake-operation");
        Ok(())
    })
    .await
    .unwrap();
    let after = db.read().await.unwrap();
    assert_eq!(after["approvals"][0]["status"], "consumed");
    assert_eq!(after["audit"].as_array().unwrap().len(), 2);
    assert_eq!(after["audit"][0], before["audit"][0]);
    db.close().await;
}

async fn rejected(connection: &mut PgConnection, statement: &str) {
    sqlx::query("SAVEPOINT hostile_statement")
        .execute(&mut *connection)
        .await
        .unwrap();
    let error = sqlx::query(sqlx::AssertSqlSafe(statement))
        .execute(&mut *connection)
        .await
        .unwrap_err();
    // SQL-level triggers/constraints, not malformed SQL or unavailable schemas.
    let code = error.as_database_error().and_then(|e| e.code()).unwrap();
    assert!(
        matches!(code.as_ref(), "P0001" | "23514"),
        "unexpected error {error}"
    );
    sqlx::query("ROLLBACK TO SAVEPOINT hostile_statement")
        .execute(&mut *connection)
        .await
        .unwrap();
    sqlx::query("RELEASE SAVEPOINT hostile_statement")
        .execute(&mut *connection)
        .await
        .unwrap();
}

/// Every data mutation and DDL change rolls back. An explicit clone-only URL is
/// required, and its actual database name is checked before the first mutation.
#[tokio::test]
#[ignore = "requires COMMUNITYHERO_GUARDS_TEST_URL for an isolated guards_test clone"]
async fn postgres_history_guards_clone_probe() {
    let url = std::env::var("COMMUNITYHERO_GUARDS_TEST_URL").expect("explicit isolated clone URL");
    let pool = PgPoolOptions::new()
        .max_connections(1)
        .connect(&url)
        .await
        .unwrap();
    let database: String = sqlx::query_scalar("SELECT current_database()")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert!(
        database.contains("guards_test"),
        "refusing non-test database"
    );
    let baseline_version: i32 =
        sqlx::query_scalar("SELECT max(version) FROM communityhero.schema_migrations")
            .fetch_one(&pool)
            .await
            .unwrap();
    if baseline_version < REQUIRED_SCHEMA {
        assert!(require_schema(&pool).await.is_err());
    } else {
        require_schema(&pool).await.unwrap();
    }
    let audit_before: String = sqlx::query_scalar("SELECT md5(coalesce(jsonb_agg(to_jsonb(a) ORDER BY workspace_id,ordinal)::text,'')) FROM communityhero.audit a").fetch_one(&pool).await.unwrap();
    let approvals_before: String = sqlx::query_scalar("SELECT md5(coalesce(jsonb_agg(to_jsonb(a) ORDER BY workspace_id,ordinal)::text,'')) FROM communityhero.approvals a").fetch_one(&pool).await.unwrap();
    // A v2 clone with an incompatible existing row must fail closed without
    // silently repairing history or recording a partially-installed upgrade.
    if baseline_version == 2 {
        let mut failed = pool.begin().await.unwrap();
        for lease in [438772115_i64, 438772116_i64] {
            assert!(
                sqlx::query_scalar::<_, bool>("SELECT pg_try_advisory_xact_lock($1)")
                    .bind(lease)
                    .fetch_one(&mut *failed)
                    .await
                    .unwrap()
            );
        }
        sqlx::query("INSERT INTO communityhero.audit(workspace_id,id,ordinal,payload) SELECT 'local-pilot','guard-bad-existing',coalesce(max(ordinal),-1)+1,'{}'::jsonb FROM communityhero.audit WHERE workspace_id='local-pilot'")
            .execute(&mut *failed).await.unwrap();
        let error = sqlx::raw_sql(include_str!("../migrations/0003_history_guards.sql"))
            .execute(&mut *failed)
            .await
            .unwrap_err();
        assert_eq!(
            error.as_database_error().unwrap().code().unwrap().as_ref(),
            "23514"
        );
        failed.rollback().await.unwrap();
        let upgraded: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM communityhero.schema_migrations WHERE version=3)",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        let constraint: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM pg_constraint WHERE conrelid='communityhero.audit'::regclass AND conname='audit_payload_projection')").fetch_one(&pool).await.unwrap();
        assert!(!upgraded && !constraint);
    }
    let mut tx = pool.begin().await.unwrap();
    for lease in [438772115_i64, 438772116_i64] {
        assert!(
            sqlx::query_scalar::<_, bool>("SELECT pg_try_advisory_xact_lock($1)")
                .bind(lease)
                .fetch_one(&mut *tx)
                .await
                .unwrap()
        );
    }
    for _ in 0..2 {
        sqlx::raw_sql(include_str!("../migrations/0003_history_guards.sql"))
            .execute(&mut *tx)
            .await
            .unwrap();
    }
    let versions: i64 =
        sqlx::query_scalar("SELECT count(*) FROM communityhero.schema_migrations WHERE version=3")
            .fetch_one(&mut *tx)
            .await
            .unwrap();
    assert_eq!(versions, 1);
    sqlx::query("INSERT INTO communityhero.audit(workspace_id,id,ordinal,action,ref_id,payload) SELECT 'local-pilot','guard-audit',coalesce(max(ordinal),-1)+1,'approval.created','guard-approval',$1::jsonb FROM communityhero.audit WHERE workspace_id='local-pilot'")
        .bind(history()["audit"][0].to_string()).execute(&mut *tx).await.unwrap();
    sqlx::query("INSERT INTO communityhero.approvals(workspace_id,id,ordinal,status,payload) SELECT 'local-pilot','guard-approval',coalesce(max(ordinal),-1)+1,'approved',$1::jsonb FROM communityhero.approvals WHERE workspace_id='local-pilot'")
        .bind(approval().to_string()).execute(&mut *tx).await.unwrap();
    for statement in [
        "UPDATE communityhero.audit SET payload=jsonb_set(payload,'{createdAt}','\"forged\"') WHERE id='guard-audit'",
        "UPDATE communityhero.audit SET action='forged' WHERE id='guard-audit'",
        "DELETE FROM communityhero.audit WHERE id='guard-audit'",
        "TRUNCATE communityhero.audit",
        "DELETE FROM communityhero.approvals WHERE id='guard-approval'",
        "TRUNCATE communityhero.approvals CASCADE",
        "UPDATE communityhero.approvals SET ordinal=ordinal+1 WHERE id='guard-approval'",
        "UPDATE communityhero.approvals SET payload=jsonb_set(payload,'{id}','\"forged\"') WHERE id='guard-approval'",
        "UPDATE communityhero.approvals SET payload=jsonb_set(payload,'{approvedBy,id}','\"forged\"') WHERE id='guard-approval'",
        "UPDATE communityhero.approvals SET payload=jsonb_set(payload,'{approvalAuthority,generation}','\"forged\"') WHERE id='guard-approval'",
        "UPDATE communityhero.approvals SET payload=jsonb_set(payload,'{proposals,0,proposal,contextEvidenceDigest}','\"forged\"') WHERE id='guard-approval'",
        "UPDATE communityhero.approvals SET payload=payload-'approvalAuthority' WHERE id='guard-approval'",
        "UPDATE communityhero.approvals SET status='consumed' WHERE id='guard-approval'",
        "UPDATE communityhero.approvals SET payload=jsonb_set(payload,'{status}','\"consumed\"') WHERE id='guard-approval'",
        "INSERT INTO communityhero.approvals(workspace_id,id,ordinal,status,payload) VALUES('local-pilot','bad-projection',2147483647,'approved','{\"id\":\"bad-projection\",\"status\":\"consumed\"}')",
        "INSERT INTO communityhero.audit(workspace_id,id,ordinal,action,payload) VALUES('local-pilot','bad-projection',2147483647,'real','{\"id\":\"bad-projection\",\"action\":\"forged\"}')",
        "INSERT INTO communityhero.audit(workspace_id,id,ordinal,payload) VALUES('local-pilot','bad-projection',2147483647,'{}')",
        "INSERT INTO communityhero.audit(workspace_id,id,ordinal,ref_id,payload) VALUES('local-pilot','bad-projection',2147483647,'real','{\"id\":\"bad-projection\"}')",
    ] {
        rejected(&mut tx, statement).await;
    }
    for status in [
        Some("consumed"),
        Some("cancelled"),
        Some("legacy-unknown"),
        None,
    ] {
        sqlx::query("UPDATE communityhero.approvals SET status=$1,payload=jsonb_set(payload,'{status}',coalesce(to_jsonb($1::text),'null'::jsonb)) WHERE id='guard-approval'")
            .bind(status).execute(&mut *tx).await.unwrap();
    }
    let bound: String = sqlx::query_scalar(
        "SELECT (payload-'status')::text FROM communityhero.approvals WHERE id='guard-approval'",
    )
    .fetch_one(&mut *tx)
    .await
    .unwrap();
    let mut expected = approval();
    expected.as_object_mut().unwrap().remove("status");
    assert_eq!(serde_json::from_str::<Value>(&bound).unwrap(), expected);
    // One failed guard rolls back otherwise-valid metadata changes in that unit.
    let metadata: String = sqlx::query_scalar(
        "SELECT metadata::text FROM communityhero.workspaces WHERE id='local-pilot'",
    )
    .fetch_one(&mut *tx)
    .await
    .unwrap();
    sqlx::query("SAVEPOINT atomic_unit")
        .execute(&mut *tx)
        .await
        .unwrap();
    sqlx::query("UPDATE communityhero.workspaces SET metadata=metadata||'{\"guardCanary\":true}' WHERE id='local-pilot'").execute(&mut *tx).await.unwrap();
    assert!(
        sqlx::query("DELETE FROM communityhero.audit WHERE id='guard-audit'")
            .execute(&mut *tx)
            .await
            .is_err()
    );
    sqlx::query("ROLLBACK TO SAVEPOINT atomic_unit")
        .execute(&mut *tx)
        .await
        .unwrap();
    let restored: String = sqlx::query_scalar(
        "SELECT metadata::text FROM communityhero.workspaces WHERE id='local-pilot'",
    )
    .fetch_one(&mut *tx)
    .await
    .unwrap();
    assert_eq!(metadata, restored);
    tx.rollback().await.unwrap();
    let audit_after: String = sqlx::query_scalar("SELECT md5(coalesce(jsonb_agg(to_jsonb(a) ORDER BY workspace_id,ordinal)::text,'')) FROM communityhero.audit a").fetch_one(&pool).await.unwrap();
    let approvals_after: String = sqlx::query_scalar("SELECT md5(coalesce(jsonb_agg(to_jsonb(a) ORDER BY workspace_id,ordinal)::text,'')) FROM communityhero.approvals a").fetch_one(&pool).await.unwrap();
    assert_eq!(audit_before, audit_after);
    assert_eq!(approvals_before, approvals_after);
    let version_after: i32 =
        sqlx::query_scalar("SELECT max(version) FROM communityhero.schema_migrations")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(baseline_version, version_after);
    println!(
        "HISTORY_GUARDS_PROBE: idempotent migration, 18 hostile SQL mutations rejected, lifecycle accepted, transactional rollback and original history verified"
    );
    pool.close().await;
}
