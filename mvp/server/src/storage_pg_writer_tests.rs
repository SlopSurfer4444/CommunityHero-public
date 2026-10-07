//! ROOT-only isolated PG regression. This file is wired under storage::pg_writer.
//! current_thread prevents SQLx's Drop-spawned return from winning a race on
//! another executor thread before the synchronous pool availability assertion.
use crate::{ApiResult, Value};
use crate::storage::{Database, LEASE};
use serde_json::json;
use sqlx::{PgPool, Postgres, pool::PoolConnection};

fn immediately_reacquire(writer: &PgPool) -> PoolConnection<Postgres> {
    assert_eq!(writer.size(), 1, "the original sole writer pool is retained");
    assert_eq!(writer.num_idle(), 1, "writer must be idle before any caller await");
    writer.try_acquire().expect("return must complete before yielding to a Drop-spawned task")
}

fn returned_before(events: &[Value], committed: u64, rollback: u64, later: &str) {
    let returns: Vec<_> = events.iter().enumerate()
        .filter(|(_, event)| event["stage"] == "pg.writer.return").collect();
    assert_eq!(returns.len(), 1, "one return observation for one application transaction");
    let (position, event) = returns[0];
    assert_eq!(event["writerPool"]["committed"], committed);
    assert_eq!(event["writerPool"]["returned"], 1);
    assert_eq!(event["writerPool"]["rollbackAcknowledged"], rollback);
    assert_eq!(event["writerPool"]["numIdle"], 1);
    assert_eq!(event["writerPool"]["size"], 1);
    let later_position = events.iter().position(|event| event["stage"] == later)
        .expect("the production wrapper/persistence span must be connected");
    assert!(position < later_position, "awaited pool return precedes caller span destruction");
}

async fn same_backend_and_lease(mut connection: PoolConnection<Postgres>, reader: &PgPool,
    writer: &PgPool, expected_pid: i32) {
    let actual: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut *connection).await.unwrap();
    assert_eq!(actual, expected_pid, "healthy commit/rollback must reuse the original writer session");
    // A bigint advisory key uses classid=high32, objid=low32, objsubid=1.
    let exact_locks: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM pg_locks WHERE pid=$1 AND locktype='advisory' AND granted AND classid=0::oid AND objid=($2::bigint)::oid AND objsubid=1")
        .bind(actual).bind(LEASE).fetch_one(&mut *connection).await.unwrap();
    assert_eq!(exact_locks, 1, "the exact engine lease remains attached to the same backend");
    let mut competitor = reader.acquire().await.unwrap();
    let competitor_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut *competitor).await.unwrap();
    assert_ne!(actual, competitor_pid);
    let acquired: bool = sqlx::query_scalar("SELECT pg_try_advisory_lock($1)")
        .bind(LEASE).fetch_one(&mut *competitor).await.unwrap();
    if acquired {
        // Clean an unexpectedly acquired lock before reporting the regression.
        let released: bool = sqlx::query_scalar("SELECT pg_advisory_unlock($1)")
            .bind(LEASE).fetch_one(&mut *competitor).await.unwrap();
        assert!(released);
    }
    competitor.return_to_pool().await;
    assert!(!acquired, "a transient competing session cannot take the writer lease");
    connection.return_to_pool().await;
    assert_eq!(writer.num_idle(), 1);
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "ROOT only: fresh isolated communityhero_writer_v51_test_ PG fixture; run this selector alone"]
async fn postgres_r5_app_commit_noop_rollback_returns_writer_before_caller_work_and_retains_lease() {
    let (mut app, _folder) = crate::tests::test_app().await;
    let seeded = app.read().await.unwrap();
    let db = crate::storage::writer_v51_fixture_db().await;
    // Same fixture identity/owner as test_app; no new pool or lease configuration.
    db.change(|data| { *data = seeded; Ok(()) }).await.unwrap();
    crate::runtime_lifecycle_startup::initialize_db_fixture(&db).await.unwrap();
    app.db.close().await;
    app.db = db;
    let Database::Postgres { writer, reader } = &app.db else { panic!("PG fixture required") };
    let writer = writer.clone();
    let reader = reader.clone();
    let mut baseline_connection = writer.acquire().await.unwrap();
    let baseline_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut *baseline_connection).await.unwrap();
    baseline_connection.return_to_pool().await;

    // About 12 MiB of nested JSON makes the real before/after projection drops
    // meaningful while bounding fixture size. No busy loop or artificial sleep.
    let cold = Value::Array((0..8192).map(|n| json!({"ordinal":n,"body":"x".repeat(1536)})).collect());
    let ((large_reply, connection), events) = crate::performance::capture(async {
        let reply = app.change(|data| {
            data["settings"]["r5ColdPayload"] = cold;
            data["settings"]["r5Committed"] = json!(true);
            Ok(data["settings"]["r5ColdPayload"].clone())
        }).await.unwrap();
        // No await, yield, drop(reply), or capture exit between completion and
        // reacquire. The reply remains alive until after the lease readbacks.
        let connection = immediately_reacquire(&writer);
        (reply, connection)
    }).await;
    returned_before(&events, 1, 0, "workspace.change.persist_and_commit");
    returned_before(&events, 1, 0, "workspace.writer.held");
    assert_eq!(large_reply.as_array().unwrap().len(), 8192);
    same_backend_and_lease(connection, &reader, &writer, baseline_pid).await;
    assert_eq!(app.read().await.unwrap()["settings"]["r5Committed"], true);
    drop(large_reply);

    let (connection, events) = crate::performance::capture(async {
        app.change(|_| Ok(())).await.unwrap();
        immediately_reacquire(&writer)
    }).await;
    returned_before(&events, 1, 0, "workspace.writer.held");
    same_backend_and_lease(connection, &reader, &writer, baseline_pid).await;

    // Exercise the actual metadata writer, both no-op and a valid lifecycle
    // history append. Protected account/settings/ledger fields stay untouched.
    let (connection, events) = crate::performance::capture(async {
        app.change_runtime_lifecycle(|_| Ok(())).await.unwrap();
        immediately_reacquire(&writer)
    }).await;
    returned_before(&events, 1, 0, "runtime.lifecycle.writer.held");
    same_backend_and_lease(connection, &reader, &writer, baseline_pid).await;
    let lifecycle_before = app.db.read_runtime_lifecycle().await.unwrap();
    let (connection, events) = crate::performance::capture(async {
        app.change_runtime_lifecycle(|data| {
            data["runtimeLifecycle"]["history"].as_array_mut().unwrap()
                .push(json!({"kind":"r5-isolated-fixture","owner":lifecycle_before["owner"]}));
            crate::runtime_lifecycle::status(data)?;
            Ok(())
        }).await.unwrap();
        immediately_reacquire(&writer)
    }).await;
    returned_before(&events, 1, 0, "runtime.lifecycle.writer.held");
    same_backend_and_lease(connection, &reader, &writer, baseline_pid).await;
    let lifecycle_after = app.db.read_runtime_lifecycle().await.unwrap();
    assert_eq!(lifecycle_after["history"].as_array().unwrap().len(),
        lifecycle_before["history"].as_array().unwrap().len() + 1);
    assert_eq!(lifecycle_after["owner"], lifecycle_before["owner"]);
    assert_eq!(lifecycle_after["phase"], lifecycle_before["phase"]);

    let (connection, events) = crate::performance::capture(async {
        app.change_runtime_lifecycle_with_ledger(|data| {
            crate::runtime_lifecycle::ledger_digest(data)?; Ok(())
        }).await.unwrap();
        immediately_reacquire(&writer)
    }).await;
    returned_before(&events, 1, 0, "runtime.lifecycle.writer.held");
    same_backend_and_lease(connection, &reader, &writer, baseline_pid).await;

    let before_rejection = app.read().await.unwrap();
    let ((error, connection), events) = crate::performance::capture(async {
        let result: ApiResult<()> = app.change(|data| {
            data["settings"]["r5Uncommitted"] = json!(true);
            data["items"][0]["draft"] = json!("must never become durable");
            Err(crate::conflict("r5 exact original reducer rejection"))
        }).await;
        let error = result.unwrap_err();
        (error, immediately_reacquire(&writer))
    }).await;
    assert_eq!(error.0, axum::http::StatusCode::CONFLICT);
    assert_eq!(error.1, "r5 exact original reducer rejection");
    returned_before(&events, 0, 1, "workspace.writer.held");
    same_backend_and_lease(connection, &reader, &writer, baseline_pid).await;
    assert_eq!(app.read().await.unwrap(), before_rejection, "complete workspace rollback");

    // A late workspace UPDATE failure occurs AFTER an item INSERT. This tests
    // an actual SQL-aborted transaction rather than only rejected in-memory JSON.
    let mut setup = immediately_reacquire(&writer);
    sqlx::raw_sql("CREATE FUNCTION communityhero.r5_reject_workspace() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'r5 late workspace write rejected'; END; $$; CREATE TRIGGER r5_reject_workspace BEFORE UPDATE ON communityhero.workspaces FOR EACH ROW EXECUTE FUNCTION communityhero.r5_reject_workspace();")
        .execute(&mut *setup).await.unwrap();
    setup.return_to_pool().await;
    let ((error, connection), events) = crate::performance::capture(async {
        let result: ApiResult<()> = app.change(|data| {
            let mut item = data["items"][0].clone();
            item["id"] = json!("r5-rollback-insert");
            crate::list_mut(data, "items").push(item);
            data["settings"]["r5LateSqlWrite"] = json!(true);
            Ok(())
        }).await;
        let error = result.unwrap_err();
        (error, immediately_reacquire(&writer))
    }).await;
    assert_eq!(error.0, axum::http::StatusCode::INTERNAL_SERVER_ERROR);
    returned_before(&events, 0, 1, "workspace.change.persist_and_commit");
    same_backend_and_lease(connection, &reader, &writer, baseline_pid).await;
    assert_eq!(app.read().await.unwrap(), before_rejection, "earlier item INSERT and final metadata UPDATE roll back atomically");
    let inserted: i64 = sqlx::query_scalar("SELECT count(*) FROM communityhero.items WHERE id='r5-rollback-insert'")
        .fetch_one(&reader).await.unwrap();
    assert_eq!(inserted, 0);
    let mut cleanup = immediately_reacquire(&writer);
    sqlx::raw_sql("DROP TRIGGER r5_reject_workspace ON communityhero.workspaces; DROP FUNCTION communityhero.r5_reject_workspace();")
        .execute(&mut *cleanup).await.unwrap();
    sqlx::raw_sql("CREATE FUNCTION communityhero.r5_pause_workspace() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN PERFORM pg_sleep(2); RETURN NEW; END; $$; CREATE TRIGGER r5_pause_workspace BEFORE UPDATE ON communityhero.workspaces FOR EACH ROW EXECUTE FUNCTION communityhero.r5_pause_workspace();")
        .execute(&mut *cleanup).await.unwrap();
    cleanup.return_to_pool().await;

    // Cancel the real App writer while its transaction has inserted an item
    // but is still before COMMIT. Observe only this fresh fixture's known PID.
    let cancelling_app = app.clone();
    let pending = tokio::spawn(async move {
        cancelling_app.change(|data| {
            let mut item = data["items"][0].clone();
            item["id"] = json!("r5-cancelled-insert");
            crate::list_mut(data, "items").push(item);
            data["settings"]["r5CancelledSqlWrite"] = json!(true);
            Ok(())
        }).await
    });
    let barrier = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let sleeping: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM pg_stat_activity WHERE pid=$1 AND datname=current_database() AND state='active' AND wait_event='PgSleep' AND query LIKE 'UPDATE communityhero.workspaces%')")
                .bind(baseline_pid).fetch_one(&reader).await.unwrap();
            if sleeping { break; }
            assert!(!pending.is_finished(), "writer completed before the precommit cancellation barrier");
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    }).await;
    // Abort even on a failed observation, so the fixture owns no stray writer.
    pending.abort();
    let cancelled = pending.await.unwrap_err();
    assert!(cancelled.is_cancelled());
    assert!(barrier.is_ok(), "own fixture backend must enter the finite pg_sleep barrier");
    let mut recovered = writer.acquire().await.unwrap();
    let recovery_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut *recovered).await.unwrap();
    // Cancellation can close/replenish a connection; do not require baseline PID.
    // The replacement, if any, still passes the existing after_connect lease hook.
    recovered.return_to_pool().await;
    let connection = immediately_reacquire(&writer);
    same_backend_and_lease(connection, &reader, &writer, recovery_pid).await;
    assert_eq!(app.read().await.unwrap(), before_rejection, "precommit abort rolls back inserted row and metadata");
    let cancelled_rows: i64 = sqlx::query_scalar("SELECT count(*) FROM communityhero.items WHERE id='r5-cancelled-insert'")
        .fetch_one(&reader).await.unwrap();
    assert_eq!(cancelled_rows, 0);
    let mut cleanup = immediately_reacquire(&writer);
    sqlx::raw_sql("DROP TRIGGER r5_pause_workspace ON communityhero.workspaces; DROP FUNCTION communityhero.r5_pause_workspace();")
        .execute(&mut *cleanup).await.unwrap();
    cleanup.return_to_pool().await;
    app.change(|_| Ok(())).await.unwrap();
    let connection = immediately_reacquire(&writer);
    same_backend_and_lease(connection, &reader, &writer, recovery_pid).await;
    app.db.close().await;
}
