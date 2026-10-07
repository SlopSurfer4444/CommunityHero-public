use super::{ApiResult, App, Value, bad, dispatch, required, set_outcome};
use futures_util::{StreamExt, stream::FuturesUnordered};
use serde_json::json;
use crate::dispatch_diagnostics::Outcome;
use std::{collections::{HashMap, VecDeque}, future::Future};

// Keep an admitted wave below the PostgreSQL reader pool's four connections.
// An execution job holds the account-wide execution_gate, so this is also the
// maximum dispatch concurrency across jobs in one workspace.
pub(crate) const MAX_IN_FLIGHT: usize = 4;

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
    RunFuture: Future<Output = ApiResult<Outcome>>,
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
    let parallelism = configured_parallelism.min(MAX_IN_FLIGHT).min(total.max(1));
    let mut active = FuturesUnordered::new();
    let mut completed = VecDeque::new();
    let mut active_keys: HashMap<String, usize> = HashMap::new();
    let mut blocked_keys: HashMap<String, String> = HashMap::new();
    let mut results: Vec<Option<Value>> = (0..total).map(|_| None).collect();

    while !pending.is_empty() || !active.is_empty() || !completed.is_empty() {
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
            let record = settle_with_progress(settle(
                entry.operation.clone(),
                Settlement::Quarantined {
                    blocked_by: blocked_by.clone(),
                },
            ), &mut active, &mut completed)
            .await;
            results[entry.index] = Some(match record {
                Ok(()) => json!({
                    "operationId": entry.operation_id,
                    "status": "stale",
                    "quarantined": true,
                    "blockedByOperationId": blocked_by
                }),
                Err(error) => json!({
                    "operationId": entry.operation_id,
                    "status": "unknown",
                    "durableStatus": "dispatching",
                    "quarantined": true,
                    "blockedByOperationId": blocked_by,
                    "recordError": error.1
                }),
            });
        }

        // Completed futures keep their conflict keys/capacity until their
        // outcomes are processed below. Never replace an unprocessed slot.
        while active.len() + completed.len() < parallelism {
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

        let next = match completed.pop_front() {
            Some(completion) => Some(completion),
            None => active.next().await,
        };
        let Some((entry, outcome)) = next else {
            continue;
        };
        for key in &entry.keys {
            active_keys.remove(key);
        }

        match outcome {
            Ok(outcome) => {
                if outcome==Outcome::Unknown {for key in &entry.keys {
                    blocked_keys
                        .entry(key.clone())
                        .or_insert_with(|| entry.operation_id.clone());
                }}
                results[entry.index] = Some(json!({
                    "operationId": entry.operation_id,
                    "status": outcome.status()
                }));
            }
            Err(error) => {
                let message = error.1;
                for key in &entry.keys {
                    blocked_keys
                        .entry(key.clone())
                        .or_insert_with(|| entry.operation_id.clone());
                }
                let record = settle_with_progress(settle(
                    entry.operation.clone(),
                    Settlement::DispatchFailed {
                        error: message.clone(),
                    },
                ), &mut active, &mut completed)
                .await;
                results[entry.index] = Some(match record {
                    Ok(()) => json!({
                        "operationId": entry.operation_id,
                        "status": "unknown",
                        "dispatcherFailed": true,
                        "durableStatus": "unknown",
                        "error": message
                    }),
                    Err(record_error) => json!({
                        "operationId": entry.operation_id,
                        "status": "unknown",
                        "dispatcherFailed": true,
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
    Ok(json!({
        "total": total,
        "parallelism": parallelism.min(total),
        // Compatibility aggregate: terminal knowledge, never a success count.
        "known": count("succeeded")+count("failed")+count("stale"),
        "succeeded": count("succeeded"),
        "failed": count("failed"),
        "stale": count("stale"),
        "unknown": count("unknown"),
        "dispatcherFailed": results.iter().filter(|row|row["dispatcherFailed"]==true).count(),
        "quarantined": results.iter().filter(|row|row["quarantined"]==true).count(),
        "results": results
    }))
}

/// Keep already-started dispatch futures moving while a durable settlement
/// waits. Do not start new work or process buffered outcomes until settlement
/// returns; the scheduler retains their capacity and conflict keys meanwhile.
/// No child tasks are spawned, so cancellation still drops the whole wave.
async fn settle_with_progress<SettlementFuture, DispatchFuture, Completion>(
    settlement: SettlementFuture,
    active: &mut FuturesUnordered<DispatchFuture>,
    completed: &mut VecDeque<Completion>,
) -> SettlementFuture::Output
where
    SettlementFuture: Future,
    DispatchFuture: Future<Output = Completion>,
{
    tokio::pin!(settlement);
    loop {
        tokio::select! {
            biased;
            result = &mut settlement => return result,
            completion = active.next(), if !active.is_empty() => {
                if let Some(completion) = completion { completed.push_back(completion); }
            }
        }
    }
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
        atomic::{AtomicBool, AtomicUsize, Ordering},
    };
    use std::time::Duration;
    use tokio::sync::{Barrier, Notify};

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

    async fn settlement_keeps_active_dispatch_moving(first_fails: bool) {
        let ready = Arc::new(Notify::new());
        let settlement_started = Arc::new(Notify::new());
        let progressed = Arc::new(Notify::new());
        let settlement_done = Arc::new(AtomicBool::new(false));
        let called = Arc::new(Mutex::new(Vec::new()));
        let report = schedule_with(
            vec![
                operation("first", "a", "first-thread", true),
                operation("first-blocked", "b", "first-thread", true),
                operation("progress", "c", "progress-thread", true),
                operation("progress-blocked", "d", "progress-thread", true),
                operation("later", "e", "independent-thread", true),
            ], 2,
            {
                let ready = ready.clone();
                let settlement_started = settlement_started.clone();
                let progressed = progressed.clone();
                let settlement_done = settlement_done.clone();
                let called = called.clone();
                move |operation| {
                    let ready = ready.clone();
                    let settlement_started = settlement_started.clone();
                    let progressed = progressed.clone();
                    let settlement_done = settlement_done.clone();
                    let called = called.clone();
                    async move {
                        let id = operation["id"].as_str().unwrap().to_owned();
                        called.lock().unwrap().push(id.clone());
                        match id.as_str() {
                            "first" => {
                                ready.notified().await;
                                if first_fails { Err(internal("runner failure")) }
                                else { Ok(Outcome::Unknown) }
                            }
                            "progress" => {
                                ready.notify_one();
                                settlement_started.notified().await;
                                progressed.notify_one();
                                // This completion must retain its keys until
                                // processed; its sibling must never be started.
                                Ok(Outcome::Unknown)
                            }
                            "later" => {
                                assert!(settlement_done.load(Ordering::SeqCst));
                                Ok(Outcome::Succeeded)
                            }
                            _ => panic!("conflicting quarantined operation was dispatched"),
                        }
                    }
                }
            },
            {
                let settlement_started = settlement_started.clone();
                let progressed = progressed.clone();
                let settlement_done = settlement_done.clone();
                move |operation, settlement| {
                    let settlement_started = settlement_started.clone();
                    let progressed = progressed.clone();
                    let settlement_done = settlement_done.clone();
                    async move {
                        let waits = match settlement {
                            Settlement::DispatchFailed { .. } => operation["id"] == "first",
                            Settlement::Quarantined { .. } => !first_fails && operation["id"] == "first-blocked",
                        };
                        if waits {
                            settlement_started.notify_one();
                            progressed.notified().await;
                            settlement_done.store(true, Ordering::SeqCst);
                        }
                        Ok(())
                    }
                }
            },
        );
        // The original scheduler deadlocks here: progress needs polling while
        // the settlement is awaiting its notification.
        let report = tokio::time::timeout(Duration::from_secs(1), report).await.unwrap().unwrap();
        let called = called.lock().unwrap();
        assert_eq!(called.len(), 3);
        for id in ["first", "progress", "later"] { assert!(called.iter().any(|called| called == id)); }
        assert_eq!(report["unknown"], 2);
        assert_eq!(report["quarantined"], 2);
        assert_eq!(report["succeeded"], 1);
        assert_eq!(report["dispatcherFailed"], usize::from(first_fails));
        assert_eq!(report["results"][1]["blockedByOperationId"], "first");
        assert_eq!(report["results"][3]["blockedByOperationId"], "progress");
    }

    #[tokio::test]
    async fn quarantine_settlement_polls_existing_dispatch_without_starting_conflicts() {
        settlement_keeps_active_dispatch_moving(false).await;
    }

    #[tokio::test]
    async fn failure_settlement_polls_existing_dispatch_without_starting_conflicts() {
        settlement_keeps_active_dispatch_moving(true).await;
    }

    #[tokio::test]
    async fn summary_counts_terminal_outcomes_without_treating_known_as_success() {
        let report=schedule_with(vec![operation("success","a","a",false),operation("failed","b","b",false),
            operation("stale","c","c",false),operation("unknown","d","d",false)],4,
            |operation|async move {Ok(match operation["id"].as_str().unwrap() {
                "success"=>Outcome::Succeeded,"failed"=>Outcome::Failed,"stale"=>Outcome::Stale,_=>Outcome::Unknown
            })},|_,_|async{Ok(())}).await.unwrap();
        for field in ["succeeded","failed","stale","unknown"] {assert_eq!(report[field],1,"{field}");}
        assert_eq!(report["known"],3);assert_eq!(report["total"],4);
        assert!(report["results"].as_array().unwrap().iter().all(|result|result["status"]!="known"));
    }

    #[tokio::test]
    async fn large_wave_stays_bounded_and_settles_every_operation() {
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
        let report = tokio::spawn(schedule_with(
            operations,
            8,
            {
                let active = active.clone();
                let maximum = maximum.clone();
                move |_| {
                    let active = active.clone();
                    let maximum = maximum.clone();
                    async move {
                        let now = active.fetch_add(1, Ordering::SeqCst) + 1;
                        observe_max(&maximum, now);
                        tokio::time::sleep(Duration::from_millis(20)).await;
                        active.fetch_sub(1, Ordering::SeqCst);
                        Ok(Outcome::Succeeded)
                    }
                }
            },
            |_, _| async { Ok(()) },
        ));
        let report = report.await.unwrap().unwrap();
        assert_eq!(maximum.load(Ordering::SeqCst), MAX_IN_FLIGHT);
        assert_eq!(report["parallelism"], MAX_IN_FLIGHT);
        assert_eq!(report["known"], 8);
        assert_eq!(report["succeeded"], 8);
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
                        Ok(Outcome::Succeeded)
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
                        Ok(if id != "unknown" {Outcome::Succeeded}else{Outcome::Unknown})
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
        assert_eq!(report["known"], 2);
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
                    Ok(Outcome::Succeeded)
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
        assert_eq!(report["failed"], 0);
        assert_eq!(report["dispatcherFailed"], 1);
        assert_eq!(report["unknown"], 1);
        assert_eq!(
            settlements.lock().unwrap().as_slice(),
            &[("fails".into(), "isolated runner failure".into())]
        );
    }
}
