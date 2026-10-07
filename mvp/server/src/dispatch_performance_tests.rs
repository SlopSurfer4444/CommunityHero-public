//! Offline timing boundary tests. All storage is temporary SQLite.
use crate::*;
use std::time::Duration;

fn stages(events: &[Value]) -> Vec<&str> {
    events.iter().map(|event| event["stage"].as_str().unwrap()).collect()
}
fn privacy(events: &[Value]) {
    for event in events {
        assert!(event.as_object().unwrap().keys().all(|key|
            ["stage","elapsedMs","timestamp","processId","operationId","jobId"].contains(&key.as_str())));
        assert!(event["elapsedMs"].as_f64().is_some_and(|value| value.is_finite() && value >= 0.0));
    }
    assert!(!json!(events).to_string().contains("private-fixture"));
}

#[tokio::test]
async fn context_pool_wait_is_separate_from_sql_decode_and_validation() {
    let (app, _folder) = crate::tests::test_app().await;
    let Database::Sqlite(pool) = &app.db else { unreachable!() };
    assert_eq!(pool.options().get_max_connections(), 1, "contention fixture requires the sole pool lease");
    let held = pool.acquire().await.unwrap();
    let ((result, events), ()) = tokio::join!(
        performance::capture(performance::operation_scope("fixture-context", app.db.read_dispatch_context("missing"))),
        async { tokio::time::sleep(Duration::from_millis(30)).await; drop(held); }
    );
    result.unwrap();
    assert_eq!(stages(&events), ["dispatch.context.pool_wait", "dispatch.context.sql", "dispatch.context.decode", "dispatch.context.validate", "dispatch.context.read"]);
    assert!(events[0]["elapsedMs"].as_f64().unwrap() >= 20.0);
    assert!(events.iter().all(|event| event["operationId"] == "fixture-context"));
    privacy(&events);
}

#[tokio::test]
async fn context_query_failure_does_not_claim_decode_or_validation() {
    let (app, _folder) = crate::tests::test_app().await;
    let Database::Sqlite(pool) = &app.db else { unreachable!() };
    sqlx::query("DROP TABLE workspace").execute(pool).await.unwrap();
    let (result, events) = performance::capture(app.db.read_dispatch_context("private-fixture")).await;
    assert!(result.is_err());
    assert_eq!(stages(&events), ["dispatch.context.pool_wait", "dispatch.context.sql", "dispatch.context.read"]);
    privacy(&events);
}

#[tokio::test]
async fn granted_writer_error_emits_held_time_and_releases_the_permit() {
    let (app, _folder) = crate::tests::test_app().await;
    let held = app.gate.acquire(writer_gate::Class::Standard).await;
    let ((result, events), ()) = tokio::join!(
        performance::capture(app.change::<()>(|_| Err(conflict("private-fixture")))),
        async { tokio::time::sleep(Duration::from_millis(30)).await; drop(held); }
    );
    assert!(result.is_err());
    assert_eq!(stages(&events), ["workspace.writer.wait", "workspace.writer.held", "workspace.change.total"]);
    assert!(events[0]["elapsedMs"].as_f64().unwrap() >= 20.0);
    privacy(&events);
    tokio::time::timeout(Duration::from_secs(1), app.change(|_| Ok(()))).await.unwrap().unwrap();
}

#[tokio::test]
async fn cancelled_waiter_never_emits_a_held_interval() {
    let (app, _folder) = crate::tests::test_app().await;
    let held = app.gate.acquire(writer_gate::Class::Standard).await;
    let (result, events) = performance::capture(tokio::time::timeout(
        Duration::from_millis(10), app.change(|_| Ok(()))
    )).await;
    assert!(result.is_err());
    assert_eq!(stages(&events), ["workspace.writer.wait", "workspace.change.total"]);
    privacy(&events);
    drop(held);
    tokio::time::timeout(Duration::from_secs(1), app.change(|_| Ok(()))).await.unwrap().unwrap();
}

#[tokio::test]
async fn causal_writer_contention_keeps_wait_and_occupancy_under_explicit_parent() {
    let (app, _folder) = crate::tests::test_app().await;
    let held = app.gate.acquire(writer_gate::Class::Standard).await;
    let context = trace_context::TraceContext::root("baw-russia", "fixture-runtime", 7, &"a".repeat(64)).unwrap()
        .with_job("writer-contention").unwrap().with_operation("writer-operation", None).unwrap();
    let ((result, parent_id), events) = trace_context::capture(trace_context::scope(context, async {
        let mut parent = performance::Span::start("fixture.container", performance::SpanClass::Container, &trace_context::current().unwrap());
        let parent_id = parent.context().unwrap().to_json()["parentSpanId"].clone();
        let (result, ()) = tokio::join!(parent.scope(app.change::<()>(|_| Err(conflict("private-fixture")))),
            async { tokio::time::sleep(Duration::from_millis(30)).await; drop(held); });
        parent.finish("failed", None);
        (result, parent_id)
    })).await;
    assert!(result.is_err());
    assert!(events.iter().all(trace_context::valid_event));
    let terminal: Vec<_> = events.iter().filter(|event| event["eventType"] == "span_end").collect();
    let wait = terminal.iter().find(|event| event["stage"] == "workspace.writer.wait").unwrap();
    let occupied = terminal.iter().find(|event| event["stage"] == "workspace.writer.held").unwrap();
    assert_eq!(wait["spanClass"], "wait");assert_eq!(occupied["spanClass"], "occupancy");
    assert_eq!(wait["parentSpanId"], parent_id);assert_eq!(occupied["parentSpanId"], parent_id);
    assert!(wait["elapsedNs"].as_str().unwrap().parse::<u128>().unwrap() >= 20_000_000);
    assert!(wait["endNs"].as_str().unwrap().parse::<u128>().unwrap() <= occupied["startNs"].as_str().unwrap().parse::<u128>().unwrap());
    assert!(terminal.iter().all(|event| event["ids"]["logicalOperationId"] == "writer-operation"));
    assert!(!json!(events).to_string().contains("private-fixture"));
}
