//! Connected native regressions. Authoring only; ROOT runs these in acceptance.
use super::*;
use crate::runtime_lifecycle::{self,AdmissionClass,SettledNative};
use crate::writer_gate::Class;
use std::{future::Future,pin::Pin,task::Poll,sync::atomic::{AtomicUsize,Ordering},time::Duration};
use serde_json::json;
async fn pending<F:Future>(future:&mut Pin<Box<F>>) {
    std::future::poll_fn(|cx|{assert!(future.as_mut().poll(cx).is_pending());Poll::Ready(())}).await;
}
async fn owner(app:&crate::App)->runtime_lifecycle::OwnerToken {
    runtime_lifecycle::current_owner(&app.db.read_metadata().await.unwrap(),&app.lifecycle_owner).unwrap()
}
fn settled(owner:&runtime_lifecycle::OwnerToken)->SettledNative {
    SettledNative{owner:owner.clone(),application_tasks:0,provider_queued:0,provider_dispatched:0,
        provider_contained:true,credential_writers:0,unresolved_effects:0}
}
#[tokio::test]
async fn lifecycle_wrapper_cancellation_releases_exact_queued_and_delivered_tickets() {
    let (app,_folder)=crate::tests::test_app().await;let calls=AtomicUsize::new(0);
    let held=app.gate.acquire(Class::Standard).await;
    let mut queued=Box::pin(app.change_runtime_lifecycle(|_|{calls.fetch_add(1,Ordering::SeqCst);Ok(())}));
    pending(&mut queued).await;assert_eq!(app.gate.queued_count(Class::Interactive),1);
    drop(queued);assert_eq!(app.gate.queued_count(Class::Interactive),0);assert_eq!(calls.load(Ordering::SeqCst),0);
    let mut granted=Box::pin(app.change_runtime_lifecycle_with_ledger(|_|{calls.fetch_add(1,Ordering::SeqCst);Ok(())}));
    pending(&mut granted).await;assert_eq!(app.gate.queued_count(Class::Interactive),1);
    drop(held);assert_eq!(app.gate.queued_count(Class::Interactive),0);
    // The delivered reservation has not been observed by the wrapper yet.
    drop(granted);assert_eq!(calls.load(Ordering::SeqCst),0);
    let _next=tokio::time::timeout(Duration::from_secs(1),app.gate.acquire(Class::Standard)).await.unwrap();
    app.db.close().await;
}
#[tokio::test]
async fn lifecycle_interactive_preserves_dialogue_fifo_nonpreemption_and_source_standard_debts() {
    let (app,_folder)=crate::tests::test_app().await;let calls=AtomicUsize::new(0);
    let held=app.gate.acquire(Class::SourceSnapshot).await;
    let mut dialogue=Box::pin(app.gate.acquire(Class::Interactive));pending(&mut dialogue).await;
    let mut maintenance=Box::pin(app.change_runtime_lifecycle(|_|{calls.fetch_add(1,Ordering::SeqCst);Ok(())}));pending(&mut maintenance).await;
    let mut standard=Box::pin(app.gate.acquire(Class::Standard));pending(&mut standard).await;
    let mut source=Box::pin(app.gate.acquire(Class::SourceSnapshot));pending(&mut source).await;
    let mut i1=Box::pin(app.gate.acquire(Class::Interactive));pending(&mut i1).await;
    let mut i2=Box::pin(app.gate.acquire(Class::Interactive));pending(&mut i2).await;
    assert_eq!(app.gate.queued_count(Class::Interactive),4);assert_eq!(calls.load(Ordering::SeqCst),0);
    drop(held);let dialogue_permit=dialogue.await;pending(&mut maintenance).await;
    assert_eq!(calls.load(Ordering::SeqCst),0);drop(dialogue_permit);
    maintenance.await.unwrap();assert_eq!(calls.load(Ordering::SeqCst),1);
    // The third Interactive grant pays source debt before a fourth foreground
    // write; source does not erase the earlier debt to Standard.
    let permit=i1.await;pending(&mut standard).await;pending(&mut source).await;pending(&mut i2).await;drop(permit);
    let permit=source.await;pending(&mut standard).await;pending(&mut i2).await;drop(permit);
    let permit=standard.await;pending(&mut i2).await;drop(permit);
    drop(i2.await);app.db.close().await;
}
#[tokio::test]
async fn gated_drain_rejects_previously_captured_new_claim_and_preserves_full_transfer() {
    let (app,_folder)=crate::tests::test_app().await;
    let old=owner(&app).await;let capture=crate::runtime_lifecycle_app::Capture::read(&app).await;
    let before=app.db.read().await.unwrap();let original=runtime_lifecycle::ledger_digest(&before).unwrap();
    let target=app.lifecycle_admission.target(&"c".repeat(64)).unwrap();
    let drain=app.change_runtime_lifecycle_with_ledger(|d|runtime_lifecycle::begin_drain_for_release(d,&old,&target,"gated-drain")).await.unwrap();
    assert!(app.change(|d|{crate::list_mut(d,"jobs").push(json!({"id":"late-new","kind":"assistant","status":"queued"}));Ok(())}).await.is_err());
    assert!(app.change_runtime_lifecycle_with_ledger(|d|capture.with(d,|d|crate::runtime_lifecycle_app::require_new_job(d,"media"))).await.is_err());
    let after=app.db.read().await.unwrap();assert_eq!(runtime_lifecycle::ledger_digest(&after).unwrap(),original);
    assert!(runtime_lifecycle::require_admission(&after,&old,AdmissionClass::Preparation).is_err());
    let mut wrong=drain.clone();wrong.epoch+=1;
    assert!(app.change_runtime_lifecycle_with_ledger(|d|runtime_lifecycle::mark_drained(d,&wrong,&settled(&wrong))).await.is_err());
    assert_eq!(app.db.read().await.unwrap(),after);
    let transfer=app.change_runtime_lifecycle_with_ledger(|d|runtime_lifecycle::mark_drained(d,&drain,&settled(&drain))).await.unwrap();
    assert_eq!(transfer["ledgerSha256"],original);
    let mut forged=transfer.clone();forged["ledgerSha256"]=json!("f".repeat(64));
    assert!(app.change_runtime_lifecycle_with_ledger(|d|runtime_lifecycle::commit_stop_checkpoint(d,&drain,&forged)).await.is_err());
    app.change_runtime_lifecycle_with_ledger(|d|runtime_lifecycle::commit_stop_checkpoint(d,&drain,&transfer)).await.unwrap();
    assert_eq!(runtime_lifecycle::ledger_digest(&app.db.read().await.unwrap()).unwrap(),original);app.db.close().await;
}
#[tokio::test]
async fn gated_resume_changes_epoch_and_cache_once_but_protected_mutation_rolls_back() {
    let (app,_folder)=crate::tests::test_app().await;let old=owner(&app).await;
    let target=app.lifecycle_admission.target(&"c".repeat(64)).unwrap();
    let drain=app.change_runtime_lifecycle_with_ledger(|d|runtime_lifecycle::begin_drain_for_release(d,&old,&target,"resume-test")).await.unwrap();
    let before=app.db.read().await.unwrap();let old_version=app.bootstrap_cache.current_version();let mut events=app.events.subscribe();
    let resumed=app.change_runtime_lifecycle_with_ledger(|d|runtime_lifecycle::resume_same_owner(d,&drain,&settled(&drain))).await.unwrap();
    assert!(resumed.epoch>drain.epoch);assert_ne!(app.bootstrap_cache.current_version(),old_version);assert!(events.try_recv().is_ok());assert!(events.try_recv().is_err());
    assert_eq!(runtime_lifecycle::ledger_digest(&before).unwrap(),runtime_lifecycle::ledger_digest(&app.db.read().await.unwrap()).unwrap());
    let current=app.db.read().await.unwrap();let version=app.bootstrap_cache.current_version();
    assert!(app.change_runtime_lifecycle_with_ledger(|d|{d["settings"]["forged"]=json!(true);Ok(())}).await.is_err());
    assert_eq!(app.db.read().await.unwrap(),current);assert_eq!(app.bootstrap_cache.current_version(),version);assert!(events.try_recv().is_err());
    app.change_runtime_lifecycle(|_|Ok(())).await.unwrap();assert_eq!(app.bootstrap_cache.current_version(),version);assert!(events.try_recv().is_err());app.db.close().await;
}
#[tokio::test(flavor="multi_thread",worker_threads=4)]
#[ignore="requires pristine explicitly isolated communityhero_writer_v51_test_ PostgreSQL fixture"]
async fn postgres_lifecycle_waits_at_gate_beyond_pool_timeout_without_borrowing_writer() {
    let (mut app,_folder)=crate::tests::test_app().await;
    let db=crate::storage::writer_v51_fixture_db().await;
    db.change(|d|crate::accounts::initialize(d,crate::accounts::Profile::LikeAvto)).await.unwrap();
    crate::runtime_lifecycle_startup::initialize_db_fixture(&db).await.unwrap();
    let Database::Postgres{writer,reader}=db else{panic!("expected fixture PostgreSQL")};
    let options=(*writer.connect_options()).clone();writer.close().await;
    let writer=PgPoolOptions::new().max_connections(1).min_connections(1)
                .acquire_timeout(Duration::from_secs(1)).idle_timeout(None).max_lifetime(None)
        .after_connect(|connection,_|Box::pin(async move {
            let held:bool=sqlx::query_scalar("SELECT pg_try_advisory_lock($1)").bind(LEASE).fetch_one(connection).await?;
            if !held {return Err(sqlx::Error::Protocol("Fixture writer lease unavailable".into()));}Ok(())
        })).connect_with(options).await.unwrap();
    app.db=Database::Postgres{writer:writer.clone(),reader};
    let (entered,mut entered_rx)=tokio::sync::mpsc::unbounded_channel();let holder_app=app.clone();
    let (release,release_rx)=std::sync::mpsc::channel();
    let holder=tokio::spawn(async move{holder_app.change(move |d| {
        entered.send(()).unwrap();tokio::task::block_in_place(||release_rx.recv_timeout(Duration::from_secs(10))).unwrap();
        d["settings"]["heldWriterCommitted"]=json!(true);Ok(())
    }).await});
    entered_rx.recv().await.unwrap();assert_eq!(writer.num_idle(),0);
    let calls=AtomicUsize::new(0);
    let mut metadata=Box::pin(app.change_runtime_lifecycle(|d|{assert_eq!(d["settings"]["heldWriterCommitted"],true);calls.fetch_add(1,Ordering::SeqCst);Ok(())}));
    let mut ledger=Box::pin(app.change_runtime_lifecycle_with_ledger(|d|{runtime_lifecycle::ledger_digest(d)?;calls.fetch_add(1,Ordering::SeqCst);Ok(())}));
    pending(&mut metadata).await;pending(&mut ledger).await;
    assert_eq!(app.gate.queued_count(Class::Interactive),2);
    tokio::time::sleep(Duration::from_millis(1250)).await;
    pending(&mut metadata).await;pending(&mut ledger).await;
    assert_eq!(app.gate.queued_count(Class::Interactive),2);assert_eq!(calls.load(Ordering::SeqCst),0);
    assert_eq!(writer.num_idle(),0);assert_eq!(writer.size(),1);
    release.send(()).unwrap();holder.await.unwrap().unwrap();metadata.await.unwrap();ledger.await.unwrap();
    assert_eq!(calls.load(Ordering::SeqCst),2);assert_eq!(writer.num_idle(),1);app.db.close().await;
}
