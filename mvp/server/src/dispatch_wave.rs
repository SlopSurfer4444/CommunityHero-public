use super::{ApiResult, App, Value, bad, dispatch, required, set_outcome};
use futures_util::{StreamExt, stream::FuturesUnordered};
use serde_json::json;
use std::{collections::HashMap, future::Future};

#[derive(Clone)]
struct Entry {
    index: usize,
    operation: Value,
    operation_id: String,
    keys: Vec<String>,
}

enum Settlement {
    Quarantined { blocked_by: String },
    DispatchFailed { error: String },
}

/// Dispatch one exact admitted wave with bounded parallelism.
///
/// The caller retains the account-wide `execution_gate`. This module only
/// introduces parallelism inside that already-authorized batch. Operations on
/// the same item are serialized. Replies in the same provider conversation are
/// serialized as well, while unrelated operations are allowed to progress.
pub(crate) async fn run(
    app: App,
    operations: Vec<Value>,
    configured_parallelism: usize,
) -> ApiResult<Value> {
    let dispatch_app = app.clone();
    let settle_app = app;
    schedule_with(
        operations,
        configured_parallelism,
        move |operation| {
            let app = dispatch_app.clone();
            async move { dispatch(app, operation).await }
        },
        move |operation, settlement| {
            let app = settle_app.clone();
            async move {
                match settlement {
                    Settlement::Quarantined { blocked_by } => {
                        set_outcome(
                            &app,
                            &operation,
                            "stale",
                            json!({
                                "reason": "Conflicting earlier operation has an unknown external outcome; no dispatch attempted",
                                "blockedByOperationId": blocked_by,
                                "providerCallAttempted": false
                            }),
                        )
                        .await
                    }
                    Settlement::DispatchFailed { error } => {
                        set_outcome(
                            &app,
                            &operation,
                            "unknown",
                            json!({
                                "reason": "Dispatcher failed before it could return a conclusive outcome",
                                "error": error,
                                "requiresReadback": true,
                                "providerRetryAllowed": false
                            }),
                        )
                        .await
                    }
                }
            }
        },
    )
    .await
}

async fn schedule_with<Run, RunFuture, Settle, SettleFuture>(
    operations: Vec<Value>,
    configured_parallelism: usize,
    run_one: Run,
    settle: Settle,
) -> ApiResult<Value>
where
    Run: Fn(Value) -> RunFuture,
    RunFuture: Future<Output = ApiResult<bool>>,
    Settle: Fn(Value, Settlement) -> SettleFuture,
    SettleFuture: Future<Output = ApiResult<()>>,
{
    if configured_parallelism == 0 {
        return Err(bad("Dispatch parallelism must be positive"));
    }

    // Validate and bind every conflict key before the first provider future is
    // created. A malformed admitted wave therefore cannot be partly dispatched.
    let total = operations.len();
    let mut pending = operations
        .into_iter()
        .enumerate()
        .map(|(index, operation)| entry(index, operation))
        .collect::<ApiResult<Vec<_>>>()?;
    let parallelism = configured_parallelism.min(total.max(1));
    let mut active = FuturesUnordered::new();
    let mut active_keys: HashMap<String, usize> = HashMap::new();
    let mut blocked_keys: HashMap<String, String> = HashMap::new();
    let mut results: Vec<Option<Value>> = (0..total).map(|_| None).collect();

    while !pending.is_empty() || !active.is_empty() {
        // An UNKNOWN result quarantines only siblings which have not started.
        // Persist that no-attempt outcome before considering more dispatches.
        let mut cursor = 0;
        while cursor < pending.len() {
            let blocked_by = pending[cursor]
                .keys
                .iter()
                .find_map(|key| blocked_keys.get(key).cloned());
            let Some(blocked_by) = blocked_by else {
                cursor += 1;
                continue;
            };
            let entry = pending.remove(cursor);
            let record = settle(
                entry.operation.clone(),
                Settlement::Quarantined {
                    blocked_by: blocked_by.clone(),
                },
            )
            .await;
            results[entry.index] = Some(match record {
                Ok(()) => json!({
                    "operationId": entry.operation_id,
                    "status": "quarantined",
                    "blockedByOperationId": blocked_by
                }),
                Err(error) => json!({
                    "operationId": entry.operation_id,
                    "status": "quarantined",
                    "blockedByOperationId": blocked_by,
                    "recordError": error.1
                }),
            });
        }

        while active.len() < parallelism {
            let Some(position) = pending
                .iter()
                .position(|entry| entry.keys.iter().all(|key| !active_keys.contains_key(key)))
            else {
                break;
            };
            let entry = pending.remove(position);
            for key in &entry.keys {
                active_keys.insert(key.clone(), entry.index);
            }
            let future = run_one(entry.operation.clone());
            active.push(async move { (entry, future.await) });
        }

        let Some((entry, outcome)) = active.next().await else {
            continue;
        };
        for key in &entry.keys {
            active_keys.remove(key);
        }

        match outcome {
            Ok(true) => {
                results[entry.index] = Some(json!({
                    "operationId": entry.operation_id,
                    "status": "known"
                }));
            }
            Ok(false) => {
                for key in &entry.keys {
                    blocked_keys
                        .entry(key.clone())
                        .or_insert_with(|| entry.operation_id.clone());
                }
                results[entry.index] = Some(json!({
                    "operationId": entry.operation_id,
                    "status": "unknown"
                }));
            }
            Err(error) => {
                let message = error.1;
                for key in &entry.keys {
                    blocked_keys
                        .entry(key.clone())
                        .or_insert_with(|| entry.operation_id.clone());
                }
                let record = settle(
                    entry.operation.clone(),
                    Settlement::DispatchFailed {
                        error: message.clone(),
                    },
                )
                .await;
                results[entry.index] = Some(match record {
                    Ok(()) => json!({
                        "operationId": entry.operation_id,
                        "status": "failed",
                        "durableStatus": "unknown",
                        "error": message
                    }),
                    Err(record_error) => json!({
                        "operationId": entry.operation_id,
                        "status": "failed",
                        "durableStatus": "dispatching",
                        "error": message,
                        "recordError": record_error.1
                    }),
                });
            }
        }
    }

    let results = results.into_iter().flatten().collect::<Vec<_>>();
    let count = |status: &str| {
        results
            .iter()
            .filter(|result| result["status"] == status)
            .count()
    };
    let failed = count("failed");
    Ok(json!({
        "total": total,
        "parallelism": parallelism.min(total),
        "known": count("known"),
        // Failed dispatcher calls are durably UNKNOWN unless even the local
        // record failed; keep them in the legacy unknown aggregate as well.
        "unknown": count("unknown") + failed,
        "failed": failed,
        "quarantined": count("quarantined"),
        "results": results
    }))
}

fn entry(index: usize, operation: Value) -> ApiResult<Entry> {
    let operation_id = required(&operation, "id")?.to_owned();
    let mut keys = vec![format!("item:{}", required(&operation, "itemId")?)];
    if operation["action"]["action"] == "reply_and_close" {
        keys.push(format!(
            "reply-conversation:{}",
            required(&operation["action"], "conversationKey")?
        ));
    }
    Ok(Entry {
        index,
        operation,
        operation_id,
        keys,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::internal;
    use std::sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    };
    use std::time::Duration;
    use tokio::sync::Barrier;

    fn operation(id: &str, item: &str, conversation: &str, reply: bool) -> Value {
        json!({
            "id": id,
            "itemId": item,
            "action": {
                "action": if reply { "reply_and_close" } else { "close" },
                "conversationKey": conversation
            }
        })
    }

    fn observe_max(maximum: &AtomicUsize, current: usize) {
        let mut seen = maximum.load(Ordering::SeqCst);
        while current > seen {
            match maximum.compare_exchange(seen, current, Ordering::SeqCst, Ordering::SeqCst) {
                Ok(_) => break,
                Err(actual) => seen = actual,
            }
        }
    }

    #[tokio::test]
    async fn configured_parallelism_exceeds_the_retired_five_action_cap() {
        let operations = (0..8)
            .map(|n| {
                operation(
                    &format!("op-{n}"),
                    &format!("item-{n}"),
                    &format!("c-{n}"),
                    false,
                )
            })
            .collect();
        let active = Arc::new(AtomicUsize::new(0));
        let maximum = Arc::new(AtomicUsize::new(0));
        let barrier = Arc::new(Barrier::new(9));
        let report = tokio::spawn(schedule_with(
            operations,
            8,
            {
                let active = active.clone();
                let maximum = maximum.clone();
                let barrier = barrier.clone();
                move |_| {
                    let active = active.clone();
                    let maximum = maximum.clone();
                    let barrier = barrier.clone();
                    async move {
                        let now = active.fetch_add(1, Ordering::SeqCst) + 1;
                        observe_max(&maximum, now);
                        barrier.wait().await;
                        active.fetch_sub(1, Ordering::SeqCst);
                        Ok(true)
                    }
                }
            },
            |_, _| async { Ok(()) },
        ));
        tokio::time::timeout(Duration::from_secs(1), barrier.wait())
            .await
            .unwrap();
        let report = report.await.unwrap().unwrap();
        assert_eq!(maximum.load(Ordering::SeqCst), 8);
        assert_eq!(report["parallelism"], 8);
        assert_eq!(report["known"], 8);
    }

    #[tokio::test]
    async fn same_item_and_reply_conversation_are_serial_but_independent_work_progresses() {
        let operations = vec![
            operation("item-first", "same-item", "a", false),
            operation("item-second", "same-item", "b", false),
            operation("reply-first", "reply-a", "same-conversation", true),
            operation("reply-second", "reply-b", "same-conversation", true),
            operation("independent", "free", "free", false),
        ];
        let item_active = Arc::new(AtomicUsize::new(0));
        let reply_active = Arc::new(AtomicUsize::new(0));
        let first_wave = Arc::new(Barrier::new(4));
        let report = tokio::spawn(schedule_with(
            operations,
            5,
            {
                let item_active = item_active.clone();
                let reply_active = reply_active.clone();
                let first_wave = first_wave.clone();
                move |operation| {
                    let item_active = item_active.clone();
                    let reply_active = reply_active.clone();
                    let first_wave = first_wave.clone();
                    async move {
                        let id = operation["id"].as_str().unwrap();
                        let counter = if id.starts_with("item-") {
                            Some(item_active.as_ref())
                        } else if id.starts_with("reply-") {
                            Some(reply_active.as_ref())
                        } else {
                            None
                        };
                        if let Some(counter) = counter {
                            assert_eq!(counter.fetch_add(1, Ordering::SeqCst), 0);
                        }
                        if matches!(id, "item-first" | "reply-first" | "independent") {
                            first_wave.wait().await;
                        }
                        if let Some(counter) = counter {
                            counter.fetch_sub(1, Ordering::SeqCst);
                        }
                        Ok(true)
                    }
                }
            },
            |_, _| async { Ok(()) },
        ));
        tokio::time::timeout(Duration::from_secs(1), first_wave.wait())
            .await
            .unwrap();
        let report = report.await.unwrap().unwrap();
        assert_eq!(report["known"], 5);
    }

    #[tokio::test]
    async fn unknown_quarantines_only_not_yet_started_conflicting_siblings() {
        let called = Arc::new(Mutex::new(Vec::new()));
        let settled = Arc::new(Mutex::new(Vec::new()));
        let report = schedule_with(
            vec![
                operation("unknown", "a", "thread", true),
                operation("blocked", "b", "thread", true),
                operation("free", "c", "other", true),
            ],
            3,
            {
                let called = called.clone();
                move |operation| {
                    let called = called.clone();
                    async move {
                        let id = operation["id"].as_str().unwrap().to_owned();
                        called.lock().unwrap().push(id.clone());
                        Ok(id != "unknown")
                    }
                }
            },
            {
                let settled = settled.clone();
                move |operation, settlement| {
                    let settled = settled.clone();
                    async move {
                        if let Settlement::Quarantined { blocked_by } = settlement {
                            settled
                                .lock()
                                .unwrap()
                                .push((operation["id"].as_str().unwrap().to_owned(), blocked_by));
                        }
                        Ok(())
                    }
                }
            },
        )
        .await
        .unwrap();
        let called = called.lock().unwrap().clone();
        assert!(called.contains(&"unknown".to_owned()));
        assert!(called.contains(&"free".to_owned()));
        assert!(!called.contains(&"blocked".to_owned()));
        assert_eq!(
            settled.lock().unwrap().as_slice(),
            &[("blocked".into(), "unknown".into())]
        );
        assert_eq!(report["unknown"], 1);
        assert_eq!(report["quarantined"], 1);
        assert_eq!(report["known"], 1);
    }

    #[tokio::test]
    async fn one_dispatch_failure_is_contained_and_known_siblings_are_preserved() {
        let settlements = Arc::new(Mutex::new(Vec::new()));
        let report = schedule_with(
            vec![
                operation("ok-a", "a", "a", false),
                operation("fails", "b", "b", false),
                operation("ok-c", "c", "c", false),
            ],
            3,
            |operation| async move {
                if operation["id"] == "fails" {
                    Err(internal("isolated runner failure"))
                } else {
                    Ok(true)
                }
            },
            {
                let settlements = settlements.clone();
                move |operation, settlement| {
                    let settlements = settlements.clone();
                    async move {
                        if let Settlement::DispatchFailed { error } = settlement {
                            settlements
                                .lock()
                                .unwrap()
                                .push((operation["id"].as_str().unwrap().to_owned(), error));
                        }
                        Ok(())
                    }
                }
            },
        )
        .await
        .unwrap();
        assert_eq!(report["known"], 2);
        assert_eq!(report["failed"], 1);
        assert_eq!(report["unknown"], 1);
        assert_eq!(
            settlements.lock().unwrap().as_slice(),
            &[("fails".into(), "isolated runner failure".into())]
        );
    }
}
