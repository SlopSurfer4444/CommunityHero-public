//! Test-only child of operator_http_tests, reusing its real routes/Pilot harness.
//! Response loss is at the consumer delivery seam: the completed HTTP Reply is
//! discarded before its status/body can inform reconciliation. This is NOT a
//! socket-loss, process-restart, live-provider or installed-runtime receipt.
use super::*;

fn owner_wire(owner: &crate::runtime_lifecycle::OwnerToken) -> Value {
    json!({"account":owner.account,"runtimeId":owner.runtime_id,
        "releaseSha256":owner.release_sha256,"epoch":owner.epoch})
}

#[tokio::test]
async fn native_http_lost_checkpoint_and_stop_reconcile_exact_durable_attempt() {
    tokio::time::timeout(Duration::from_secs(30), async {
        let pilot = Pilot::start().await;
        let host = format!("127.0.0.1:{}", pilot.port);
        let headers = [("host", host.as_str()), ("x-csrf-token", "local-owner-csrf")];
        let attempt = "r9-http-lost-reply";
        let target_sha = "c".repeat(64); // Existing synthetic admitted target.
        pilot.app.change(|d| {
            crate::list_mut(d, "jobs").push(json!({
                "id":"lost-reply-paid-paused", "kind":"media", "purpose":"auto_media",
                "account":"LikeAvto", "refId":"paid-post", "status":"paused",
                "visualContractVersion":2,
                "sourceAttempts":[{"id":"already-paid-attempt","status":"completed",
                    "receipt":{"sha256":"f".repeat(64),"paid":true,"retryAuthorized":false}}],
                "result":{"visualProgress":{"schemaVersion":2,"leaseEpoch":7,"phase":"scan",
                    "committedCursor":3,"retainedArtifacts":[{"path":"synthetic/frame-3.png","sha256":"e".repeat(64)}]}},
                "operatorDecision":{"kind":"paused","reason":"keep exact paid checkpoint"},
                "future":{"raw":"preserve"}
            }));
            crate::list_mut(d, "operations").push(json!({
                "id":"lost-reply-unknown-social", "status":"unknown", "attemptId":"prior-external-attempt",
                "itemId":"uncertain-item", "action":{"actionId":"prior-action","text":"exact uncertain reply",
                    "target":{"id":"synthetic-external-one"}},
                "receipt":{"reason":"prior response lost","providerRetryAllowed":false},
                "future":{"raw":[1,2,3]}
            }));
            Ok(())
        }).await.unwrap();
        let before = pilot.app.db.read().await.unwrap();
        let jobs_bytes = serde_json::to_vec(&before["jobs"]).unwrap();
        let operations_bytes = serde_json::to_vec(&before["operations"]).unwrap();
        let initial = crate::runtime_lifecycle::current_owner(
            &pilot.app.db.read_metadata().await.unwrap(), &pilot.app.lifecycle_owner).unwrap();
        let begin_body = json!({"owner":owner_wire(&initial),"attemptId":attempt,"releaseSha256":target_sha});

        // Synthetic owner authorization still traverses production middleware
        // and handler CSRF checks; a missing token cannot mutate the native DB.
        let denied = pilot.request("POST", "/api/maintenance/runtime/begin",
            &[("host",host.as_str())], Some(begin_body.clone())).await;
        assert_eq!(denied.status,403);
        assert_eq!(pilot.app.db.read().await.unwrap(),before);
        let begun = pilot.request("POST", "/api/maintenance/runtime/begin", &headers, Some(begin_body.clone())).await;
        assert_eq!(begun.status,200,"{}",begun.body);
        let draining = pilot.app.db.read().await.unwrap();
        let drain = crate::runtime_lifecycle::current_owner(&draining, &pilot.app.lifecycle_owner).unwrap();
        assert_eq!(drain.epoch,initial.epoch + 1);
        assert_eq!(begun.body["owner"],owner_wire(&drain));
        let exact_target = json!({"releaseSha256":target_sha,"attemptId":attempt,
            "asrDisabled":true,"mediaAnalysisGeneration":1});
        assert_eq!(draining["runtimeLifecycle"]["phase"],"draining");
        assert_eq!(draining["runtimeLifecycle"]["target"],exact_target);

        // Native checkpoint commits before the Reply reaches this consumer.
        // Never inspect the discarded response to decide whether to retry.
        drop(pilot.request("POST", "/api/maintenance/runtime/checkpoint", &headers,
            Some(json!({"owner":owner_wire(&drain)}))).await);
        let checkpointed = pilot.app.db.read().await.unwrap();
        let lifecycle = crate::runtime_lifecycle::status(&checkpointed).unwrap();
        assert_eq!(lifecycle["phase"],"drained");
        assert_eq!(lifecycle["owner"],owner_wire(&drain));
        assert_eq!(lifecycle["target"],exact_target);
        let transfer = lifecycle["transfer"].clone();
        assert_eq!(transfer["owner"],owner_wire(&drain));
        assert_eq!(transfer["target"],exact_target);
        assert_eq!(transfer["nativeSettled"],true);
        assert_eq!(transfer["ledgerSha256"],crate::runtime_lifecycle::ledger_digest(&checkpointed).unwrap());
        let checkpoint_status = pilot.request("POST", "/api/maintenance/runtime/status", &headers,
            Some(json!({"owner":owner_wire(&drain)}))).await;
        assert_eq!(checkpoint_status.status,200,"{}",checkpoint_status.body);
        assert_eq!(checkpoint_status.body["lifecycle"],lifecycle);
        assert_eq!(checkpoint_status.body["stopAuthorized"],false);

        // Second lost delivery, now after the actual durable stop commit.
        let stop_body = json!({"owner":owner_wire(&drain),"transfer":transfer});
        drop(pilot.request("POST", "/api/maintenance/runtime/stop-checkpoint", &headers, Some(stop_body.clone())).await);
        let stopped = pilot.app.db.read().await.unwrap();
        let stopped_bytes = serde_json::to_vec(&stopped).unwrap();
        assert_eq!(crate::runtime_lifecycle::current_owner(&stopped,&pilot.app.lifecycle_owner).unwrap(),drain);
        assert_eq!(stopped["runtimeLifecycle"]["phase"],"stopped");
        assert_eq!(stopped["runtimeLifecycle"]["target"],exact_target);
        assert_eq!(stopped["runtimeLifecycle"]["transfer"],stop_body["transfer"]);
        assert_eq!(serde_json::to_vec(&stopped["jobs"]).unwrap(),jobs_bytes);
        assert_eq!(serde_json::to_vec(&stopped["operations"]).unwrap(),operations_bytes);
        let status = pilot.request("POST", "/api/maintenance/runtime/status", &headers,
            Some(json!({"owner":owner_wire(&drain)}))).await;
        assert_eq!(status.status,200,"{}",status.body);
        assert_eq!(status.body["lifecycle"],stopped["runtimeLifecycle"]);
        assert_eq!(status.body["owner"],owner_wire(&drain));
        assert_eq!(status.body["stopAuthorized"],false);

        // The same attempt/transfer is a rejected mutation, never another effect.
        // A caller must reconcile first; it cannot silently retarget the attempt.
        let replay = pilot.request("POST", "/api/maintenance/runtime/stop-checkpoint", &headers, Some(stop_body.clone())).await;
        assert_eq!(replay.status,409,"{}",replay.body);
        let repeated_begin = pilot.request("POST", "/api/maintenance/runtime/begin", &headers, Some(begin_body)).await;
        assert_eq!(repeated_begin.status,409,"{}",repeated_begin.body);
        let stale_status = pilot.request("POST", "/api/maintenance/runtime/status", &headers,
            Some(json!({"owner":owner_wire(&initial)}))).await;
        assert_eq!(stale_status.status,409,"{}",stale_status.body);
        let mut changed_attempt = stop_body;
        changed_attempt["transfer"]["target"]["attemptId"] = json!("different-attempt");
        let retarget = pilot.request("POST", "/api/maintenance/runtime/stop-checkpoint", &headers, Some(changed_attempt)).await;
        assert_eq!(retarget.status,409,"{}",retarget.body);
        assert_eq!(serde_json::to_vec(&pilot.app.db.read().await.unwrap()).unwrap(),stopped_bytes,
            "reconciliation and rejected replay must not append history or alter paid/UNKNOWN rows");
        assert!(pilot.app.tasks.lock().await.is_empty());
        assert_eq!(pilot.app.lifecycle_task_count.load(std::sync::atomic::Ordering::SeqCst),0);
        let native = pilot.app.lifecycle_work.snapshot().unwrap();
        assert!(native.closed);
        assert_eq!((native.active,native.unresolved,native.credential_writers),(0,0,0));
        let provider = pilot.app.provider_session.drain_status();
        assert_eq!(provider.phase,crate::provider_session::ProviderDrainPhase::Drained);
        assert_eq!((provider.queued,provider.dispatched),(0,0));
        assert!(provider.worker_retired && provider.containment && provider.worker_pid.is_none());

        // Reopen the real SQLite store, discarding its pool/read caches. This is
        // persistence across database reopen, not a process/startup-controller test.
        pilot.server.abort();
        pilot.app.db.close().await;
        let reopened = Database::Sqlite(open_db(&pilot._temp.path().join("workspace.sqlite")).await.unwrap());
        let durable = reopened.read().await.unwrap();
        assert_eq!(serde_json::to_vec(&durable).unwrap(),stopped_bytes);
        assert_eq!(crate::runtime_lifecycle::current_owner(&durable,&pilot.app.lifecycle_owner).unwrap(),drain);
        assert_eq!(reopened.read_runtime_lifecycle().await.unwrap(),stopped["runtimeLifecycle"]);
        for class in [crate::runtime_lifecycle::AdmissionClass::Media,
            crate::runtime_lifecycle::AdmissionClass::Preparation,
            crate::runtime_lifecycle::AdmissionClass::SocialDispatch] {
            assert!(crate::runtime_lifecycle::bound_admission_token(&durable,&pilot.app.lifecycle_owner,class).is_err());
        }
        assert_eq!(serde_json::to_vec(&durable["jobs"]).unwrap(),jobs_bytes);
        assert_eq!(serde_json::to_vec(&durable["operations"]).unwrap(),operations_bytes);
        reopened.close().await;
    }).await.expect("isolated native HTTP fixture exceeded its finite deadline");
}
