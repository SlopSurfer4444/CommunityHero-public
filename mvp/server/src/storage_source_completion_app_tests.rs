//! ROOT execution only. Real PG App, original native UNKNOWN and a connected
//! queued durable Settlement must progress while owned projections still exist.
use super::*;
use std::{future::Future,pin::Pin,sync::{Arc,atomic::{AtomicUsize,Ordering}},task::{Context,Poll,Waker},time::Duration};
use tokio::sync::Mutex;
type Settlement=Pin<Box<dyn Future<Output=ApiResult<()>>+Send>>;

#[tokio::test]
#[ignore="ROOT only: one fresh isolated BAW PG fixture, original native operation history; no provider/model"]
async fn source_completion_releases_app_permit_after_pool_ack_before_cleanup() {
    let url=std::env::var("COMMUNITYHERO_WRITER_V51_TEST_URL").expect("explicit isolated fixture URL");
    let name=std::env::var("COMMUNITYHERO_WRITER_V51_TEST_DATABASE").expect("explicit isolated fixture name");
    let db=super::super::preparation::writer_v51_fixture_db_for_profile_with(&url,&name,crate::accounts::Profile::BawRussia).await;
    let (app,_folder,operation)=super::benchmark_tests::completion_fixture_app(db).await;
    for case in ["noop","mutation","reducer_rejection","late_metadata_rejection","commit_rejection"] {
        let before=app.db.read().await.unwrap();
        let Database::Postgres{writer,..}=&app.db else{unreachable!()};
        if case=="late_metadata_rejection" {
            sqlx::raw_sql("CREATE OR REPLACE FUNCTION pg_temp.completion_reject() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'source completion late metadata fixture'; END; $$; CREATE TRIGGER completion_reject BEFORE UPDATE ON communityhero.workspaces FOR EACH ROW EXECUTE FUNCTION pg_temp.completion_reject();")
                .execute(writer).await.unwrap();
        }
        if case=="commit_rejection" {
            sqlx::raw_sql("CREATE OR REPLACE FUNCTION pg_temp.completion_reject() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'source completion deferred COMMIT fixture'; END; $$; CREATE CONSTRAINT TRIGGER completion_reject AFTER UPDATE ON communityhero.workspaces DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION pg_temp.completion_reject();")
                .execute(writer).await.unwrap();
        }
        let pending:Arc<Mutex<Option<Settlement>>>=Arc::new(Mutex::new(None));
        let entered=Arc::new(AtomicUsize::new(0));let probe_pending=pending.clone();let probe_entered=entered.clone();
        let probe:CleanupProbe=Arc::new(move || {
            let pending=probe_pending.clone();let entered=probe_entered.clone();
            Box::pin(async move {
                let settlement=pending.lock().await.take().expect("actual App Settlement was registered during source reduction");
                tokio::time::timeout(Duration::from_secs(3),settlement).await
                    .expect("durable Settlement must complete before projection cleanup").unwrap();
                entered.fetch_add(1,Ordering::SeqCst);
            })
        });
        let snapshot=json!({"posts":[],"branches":[],"items":[]});
        let readback=json!({"fixture":"source-completion","case":case,"providerCallAttempted":false});
        let settlement_app=app.clone();let expected_operation=operation.clone();let expected_readback=readback.clone();
        let (outcome,events)=crate::performance::capture(with_cleanup_probe(probe,app.change_source_snapshot_scoped(
            SourceReadIntent::Snapshot(&snapshot),|d| {
                let mut settlement:Settlement=Box::pin(async move {
                    settlement_app.change_operation_evidence(&expected_operation,
                        crate::storage::OperationEvidenceUpdate::Readback(expected_readback)).await
                });
                let mut context=Context::from_waker(Waker::noop());
                assert!(matches!(settlement.as_mut().poll(&mut context),Poll::Pending));
                assert_eq!(app.gate.queued_count(crate::writer_gate::Class::Settlement),1,
                    "real App source holder must cause the exact Settlement registration");
                *pending.try_lock().unwrap()=Some(settlement);
                if case!="noop" {d["posts"][0]["text"]=json!(format!("complete source {case}"));}
                if case=="reducer_rejection" {return Err(crate::conflict("exact source completion rejection"));}
                if matches!(case,"late_metadata_rejection"|"commit_rejection") {d["sync"]["completionFault"]=json!(true);}
                Ok(())
            }))).await;
        if matches!(case,"late_metadata_rejection"|"commit_rejection") {
            sqlx::query("DROP TRIGGER completion_reject ON communityhero.workspaces").execute(writer).await.unwrap();
        }
        assert_eq!(entered.load(Ordering::SeqCst),1,"cleanup retained real projections through the connected probe");
        let position=|stage:&str|events.iter().position(|event|event["stage"]==stage).expect("actual stage event");
        let returned=position("pg.writer.return");let held=position("source.snapshot.writer.held");
        let settled=position("operation.evidence.writer.held");let cleanup=position("source.snapshot.drop");
        assert!(returned<held&&held<settled&&settled<cleanup,"pool ACK, App release, durable Settlement, cleanup order: {events:?}");
        assert_eq!(events[returned]["writerPool"]["returned"],1,"observed awaited pool return");
        let rejected=matches!(case,"reducer_rejection"|"late_metadata_rejection"|"commit_rejection");
        assert_eq!(events[returned]["writerPool"]["committed"],u64::from(!rejected));
        assert_eq!(events[returned]["writerPool"]["rollbackAcknowledged"],u64::from(matches!(case,"reducer_rejection"|"late_metadata_rejection")),
            "failed COMMIT does not become a fabricated rollback ACK or permission to retry");
        if case=="reducer_rejection" {assert_eq!(outcome.unwrap_err().1,"exact source completion rejection");}
        else if rejected {assert!(outcome.is_err());}else{outcome.unwrap();}
        let mut expected=before.clone();
        if case=="mutation" {expected["posts"][0]["text"]=json!(format!("complete source {case}"));}
        crate::row_mut(&mut expected,"operations",operation["id"].as_str().unwrap()).unwrap()["action"]["readbackEvidence"]=readback;
        let after=app.db.read().await.unwrap();assert_eq!(after,expected,"source rows and metadata rollback atomically; only exact queued evidence changes");
        assert_eq!(crate::row(&after,"operations",operation["id"].as_str().unwrap()).unwrap()["status"],"unknown");
    }
    app.db.close().await;
}

#[tokio::test]
#[ignore="ROOT only: separate fresh isolated BAW PG fixture; real precommit trigger/cancellation/lease recovery"]
async fn source_completion_cancellation_preserves_lease_and_cursor_atomicity() {
    let url=std::env::var("COMMUNITYHERO_WRITER_V51_TEST_URL").expect("explicit isolated fixture URL");
    let name=std::env::var("COMMUNITYHERO_WRITER_V51_TEST_DATABASE").expect("explicit isolated fixture name");
    let db=super::super::preparation::writer_v51_fixture_db_for_profile_with(&url,&name,crate::accounts::Profile::BawRussia).await;
    let (app,_folder,operation)=super::benchmark_tests::completion_fixture_app(db).await;
    app.change_schedule(|d| {d["sync"]["scan"]=json!({"id":"source-cancellation-fixture","binding":crate::active_binding(d)?.to_json(),
        "open":{"cursor":"original-checkpoint"}});Ok(())}).await.unwrap();
    let before=app.db.read().await.unwrap();
    let Database::Postgres{writer,reader}=&app.db else{unreachable!()};
    let mut connection=writer.acquire().await.unwrap();
    let pid:i32=sqlx::query_scalar("SELECT pg_backend_pid()").fetch_one(&mut *connection).await.unwrap();
    sqlx::raw_sql("CREATE FUNCTION communityhero.source_completion_pause() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN PERFORM pg_sleep(2); RETURN NEW; END; $$; CREATE TRIGGER completion_pause BEFORE UPDATE ON communityhero.workspaces FOR EACH ROW EXECUTE FUNCTION communityhero.source_completion_pause();")
        .execute(&mut *connection).await.unwrap();connection.return_to_pool().await;
    let pending_app=app.clone();
    let pending=tokio::spawn(async move {
        let snapshot=json!({"posts":[],"branches":[],"items":[]});
        pending_app.change_source_snapshot_scoped(SourceReadIntent::Snapshot(&snapshot),|d| {
            crate::list_mut(d,"posts").push(json!({"id":"cancelled-source-row","text":"uncommitted complete observation"}));
            d["sync"]["scan"]["open"]["cursor"]=json!("uncommitted-checkpoint");Ok(())
        }).await
    });
    let reached=tokio::time::timeout(Duration::from_secs(5),async {
        loop {
            let sleeping:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM pg_stat_activity WHERE pid=$1 AND datname=current_database() AND state='active' AND wait_event='PgSleep' AND query LIKE 'UPDATE communityhero.workspaces%')")
                .bind(pid).fetch_one(reader).await.unwrap();
            if sleeping {break;}
            assert!(!pending.is_finished(),"actual source transaction finished before cancellation barrier");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }).await;
    pending.abort();assert!(pending.await.unwrap_err().is_cancelled());assert!(reached.is_ok(),"observe actual fixture precommit SQL, not a fabricated marker");
    let mut recovered=writer.acquire().await.unwrap();
    sqlx::raw_sql("DROP TRIGGER completion_pause ON communityhero.workspaces; DROP FUNCTION communityhero.source_completion_pause();").execute(&mut *recovered).await.unwrap();
    recovered.return_to_pool().await;
    assert_eq!(app.db.read().await.unwrap(),before,"cancelled row and cursor rollback together, original UNKNOWN/paid/lifecycle retained");
    app.change_source_snapshot_scoped(SourceReadIntent::Snapshot(&json!({})),|_|Ok(())).await.unwrap();
    assert_eq!(crate::row(&app.db.read().await.unwrap(),"operations",operation["id"].as_str().unwrap()).unwrap(),&operation);
    app.db.close().await;
}
