use super::{ApiResult, App, Value, audit, feedback, internal, list_mut, now, required};
use serde_json::json;
use std::{collections::HashSet, future::Future, time::Duration};
use tokio::task::{JoinError, JoinHandle};

/// The observed end of a registered worker. `Cancelled` is deliberately
/// separate from an application error: a task may have been explicitly
/// cancelled after its durable job state was changed, or it may have vanished
/// while external effects were still uncertain.
pub(crate) enum WorkerExit {
    Completed(ApiResult<Value>),
    Panicked,
    Cancelled,
    Skipped,
}

impl WorkerExit {
    fn abnormal(&self) -> Option<AbnormalExit> {
        match self {
            Self::Completed(_) | Self::Skipped => None,
            Self::Panicked => Some(AbnormalExit::Panicked),
            Self::Cancelled => Some(AbnormalExit::Cancelled),
        }
    }
}

#[derive(Clone, Copy)]
enum AbnormalExit {
    Panicked,
    Cancelled,
}

impl AbnormalExit {
    fn code(self) -> &'static str {
        match self {
            Self::Panicked => "worker_panicked",
            Self::Cancelled => "worker_cancelled",
        }
    }

    fn message(self) -> &'static str {
        match self {
            Self::Panicked => "Worker panicked before durable completion",
            Self::Cancelled => "Worker stopped before durable completion",
        }
    }
}

pub(crate) async fn run_if_active<F>(app: &App, job: &str, future: F) -> WorkerExit
where
    F: Future<Output = ApiResult<Value>>,
{
    match app.db.read_job(job).await {
        Ok(Some(stored)) if stored["status"] == "running" => WorkerExit::Completed(future.await),
        Ok(_) => WorkerExit::Skipped,
        Err(error) => WorkerExit::Completed(Err(error)),
    }
}

pub(crate) async fn observe(handle: JoinHandle<WorkerExit>) -> WorkerExit {
    match handle.await {
        Ok(exit) => exit,
        Err(error) if error.is_panic() => WorkerExit::Panicked,
        Err(_) => WorkerExit::Cancelled,
    }
}

/// Finalize a worker exactly once after its JoinHandle has been observed.
///
/// For abnormal exits, any still-dispatching operation admitted by this exact
/// execute job becomes UNKNOWN before the job is failed. This is deliberately
/// conservative: the provider call may have happened, and only readback may
/// resolve it. No worker or provider future is retried here.
pub(crate) async fn finalize(app: &App, job: &str, exit: WorkerExit) {
    if let Some(abnormal) = exit.abnormal() {
        let mut delay = Duration::from_secs(1);
        loop {
            let recorded = app
                .change(|data| record_abnormal_external_exit(data, job, abnormal))
                .await;
            match recorded {
                Ok(true) => break,
                Ok(false) => {
                    app.tasks.lock().await.remove(job);
                    return;
                }
                Err(_) => {}
            }
            eprintln!("worker_abnormal_reconciliation_pending; retrying durable state update");
            tokio::time::sleep(delay).await;
            delay = (delay * 2).min(Duration::from_secs(30));
        }
        app.finish(job, Err(internal(abnormal.message()))).await;
    } else {
        match exit {
            WorkerExit::Completed(result) => app.finish(job, result).await,
            WorkerExit::Skipped => {
                app.tasks.lock().await.remove(job);
            }
            WorkerExit::Panicked | WorkerExit::Cancelled => unreachable!(),
        }
    }
}

fn record_abnormal_external_exit(
    data: &mut Value,
    job_id: &str,
    exit: AbnormalExit,
) -> ApiResult<bool> {
    let Some(job) = super::list(data, "jobs")
        .iter()
        .find(|job| job["id"] == job_id)
        .cloned()
    else {
        return Ok(false);
    };
    if !matches!(job["status"].as_str(), Some("running" | "queued")) {
        // Explicit cancellation writes the terminal state before aborting the
        // task. It remains authoritative and must not be rewritten as failure.
        return Ok(true);
    }
    if job["kind"] != "execute" {
        return Ok(true);
    }

    let approval_id = required(&job, "refId")?;
    let mut affected = Vec::new();
    let mut proposal_ids = HashSet::new();
    for operation in list_mut(data, "operations") {
        if operation["approvalId"] != approval_id || operation["status"] != "dispatching" {
            continue;
        }
        let prior = operation["evidence"].clone();
        if !operation["evidence"].is_object() {
            operation["evidence"] = if prior.is_null() {
                json!({})
            } else {
                json!({"prior":prior})
            };
        }
        operation["status"] = json!("unknown");
        operation["updatedAt"] = json!(now());
        operation["evidence"]["workerExit"] = json!({
            "code": exit.code(),
            "outcome": "unknown",
            "requiresReadback": true,
            "providerRetryAllowed": false
        });
        if let Some(proposal_id) = operation["proposalId"].as_str() {
            proposal_ids.insert(proposal_id.to_owned());
        }
        affected.push(operation.clone());
    }
    for proposal in list_mut(data, "proposals") {
        if proposal["status"] == "dispatching"
            && proposal["id"]
                .as_str()
                .is_some_and(|id| proposal_ids.contains(id))
        {
            proposal["status"] = json!("unknown");
        }
    }
    for operation in affected {
        feedback::outcome(data, &operation, "unknown")?;
        audit(data, "operation.unknown", required(&operation, "id")?);
    }
    Ok(true)
}

/// Restart a panicked background scheduler with capped backoff. A clean return
/// is an intentional stop and is never restarted. The scheduler future runs in
/// a child task only so its panic is observable; dropping the supervisor aborts
/// that child, so a future drain/shutdown cannot leave a detached loop behind.
pub(crate) async fn supervise_background<F, Fut>(name: &'static str, mut factory: F)
where
    F: FnMut() -> Fut,
    Fut: Future<Output = ()> + Send + 'static,
{
    let mut delay = Duration::from_secs(1);
    loop {
        let mut task = AbortOnDrop::new(tokio::spawn(factory()));
        match task.join().await {
            Ok(()) => return,
            Err(error) if error.is_panic() => {
                eprintln!("background_scheduler_panicked name={name}; restarting after backoff");
            }
            Err(_) => return,
        }
        tokio::time::sleep(delay).await;
        delay = (delay * 2).min(Duration::from_secs(30));
    }
}

struct AbortOnDrop<T>(Option<JoinHandle<T>>);

impl<T> AbortOnDrop<T> {
    fn new(handle: JoinHandle<T>) -> Self {
        Self(Some(handle))
    }

    async fn join(&mut self) -> Result<T, JoinError> {
        self.0.as_mut().expect("join handle present").await
    }
}

impl<T> Drop for AbortOnDrop<T> {
    fn drop(&mut self) {
        if let Some(handle) = self.0.take() {
            handle.abort();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    #[tokio::test]
    async fn observer_distinguishes_result_panic_and_cancellation() {
        let completed = observe(tokio::spawn(async {
            WorkerExit::Completed(Ok(json!({"ok":true})))
        }))
        .await;
        assert!(matches!(completed, WorkerExit::Completed(Ok(_))));

        let panicked = observe(tokio::spawn(async {
            panic!("contained test panic");
            #[allow(unreachable_code)]
            WorkerExit::Completed(Ok(json!(null)))
        }))
        .await;
        assert!(matches!(panicked, WorkerExit::Panicked));

        let task = tokio::spawn(async {
            std::future::pending::<()>().await;
            WorkerExit::Completed(Ok(json!(null)))
        });
        task.abort();
        assert!(matches!(observe(task).await, WorkerExit::Cancelled));
    }

    #[tokio::test]
    async fn background_supervisor_restarts_panic_but_not_clean_stop() {
        let calls = Arc::new(AtomicUsize::new(0));
        let observed = calls.clone();
        let supervisor = tokio::spawn(supervise_background("test", move || {
            let observed = observed.clone();
            async move {
                if observed.fetch_add(1, Ordering::SeqCst) == 0 {
                    panic!("first scheduler generation");
                }
            }
        }));
        tokio::time::timeout(Duration::from_secs(3), supervisor)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn abnormal_execute_exit_marks_only_bound_inflight_operations_unknown() {
        let mut data = json!({
            "account":"LikeAvto",
            "jobs":[{"id":"job-a","kind":"execute","refId":"approval-a","status":"running"}],
            "operations":[
                {"id":"op-a","approvalId":"approval-a","proposalId":"proposal-a","itemId":"item-a","status":"dispatching","evidence":{"receipt":"kept"}},
                {"id":"op-done","approvalId":"approval-a","proposalId":"proposal-done","itemId":"item-done","status":"succeeded"},
                {"id":"op-b","approvalId":"approval-b","proposalId":"proposal-b","itemId":"item-b","status":"dispatching"}
            ],
            "proposals":[
                {"id":"proposal-a","itemId":"item-a","revision":1,"kind":"close","text":"","status":"dispatching"},
                {"id":"proposal-done","itemId":"item-done","revision":1,"kind":"close","text":"","status":"succeeded"},
                {"id":"proposal-b","itemId":"item-b","revision":1,"kind":"close","text":"","status":"dispatching"}
            ],
            "items":[
                {"id":"item-a","platform":"vk"},
                {"id":"item-done","platform":"vk"},
                {"id":"item-b","platform":"vk"}
            ],
            "feedback":[],"audit":[]
        });
        record_abnormal_external_exit(&mut data, "job-a", AbnormalExit::Panicked).unwrap();
        assert_eq!(data["operations"][0]["status"], "unknown");
        assert_eq!(data["operations"][0]["evidence"]["receipt"], "kept");
        assert_eq!(
            data["operations"][0]["evidence"]["workerExit"]["providerRetryAllowed"],
            false
        );
        assert_eq!(data["proposals"][0]["status"], "unknown");
        assert_eq!(data["operations"][1]["status"], "succeeded");
        assert_eq!(data["operations"][2]["status"], "dispatching");
        assert_eq!(data["proposals"][2]["status"], "dispatching");
        assert_eq!(data["feedback"].as_array().unwrap().len(), 1);
        assert_eq!(data["audit"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn durable_cancel_state_wins_over_abort_observation() {
        let mut data = json!({
            "jobs":[{"id":"job-a","kind":"assistant","refId":"","status":"cancelled"}],
            "operations":[],"proposals":[],"feedback":[],"audit":[]
        });
        let before = data.clone();
        record_abnormal_external_exit(&mut data, "job-a", AbnormalExit::Cancelled).unwrap();
        assert_eq!(data, before);
    }
}
