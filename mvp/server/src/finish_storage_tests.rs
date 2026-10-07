use super::*;

#[tokio::test]
async fn failed_start_read_becomes_failed_job_after_storage_recovers_without_dispatch(){
    let dir=tempfile::tempdir().unwrap();
    let db=open_db(&dir.path().join("workspace.sqlite")).await.unwrap();
    let (events,_)=broadcast::channel(8);
    let app=App {lifecycle_task_count: Default::default(), lifecycle_admission: Arc::new(crate::runtime_lifecycle_startup::Admission::fixture(crate::accounts::Profile::LikeAvto)), lifecycle_owner: Arc::new(crate::runtime_lifecycle_startup::Admission::fixture(crate::accounts::Profile::LikeAvto).identity().clone()), lifecycle_provider_token: Default::default(), lifecycle_work: Default::default(), media_discovery: Default::default(),preparation_wake: Default::default(),provider_session: Default::default(),account:crate::accounts::Profile::LikeAvto,navigation:crate::account_navigation::Navigation::root(),db:Database::Sqlite(db),gate:Arc::new(crate::writer_gate::WriterGate::default()),execution_gate:Arc::new(Mutex::new(())),preparation_workers: Default::default(),editorial_gate: Default::default(),assistant_gate:Arc::new(Mutex::new(())),assistant_chat_gate:Arc::new(Mutex::new(())),events,csrf:id(),auth:None,public_origin:None,external_writes:false,port:0,data:dir.path().to_owned(),bridge:dir.path().join("never-execute"),node:dir.path().join("no-runtime"),tasks:Arc::new(Mutex::new(HashMap::new())),bootstrap_cache:Arc::new(bootstrap_cache::Cache::default())};
        crate::runtime_lifecycle_app::initialize_app_fixture(&app).await.unwrap();
    let key=app.job("status_sync", "read-fault").await.unwrap();
    let Database::Sqlite(pool)=&app.db else{unreachable!()};
    let original:String=sqlx::query_scalar("SELECT payload FROM workspace WHERE id=1").fetch_one(pool).await.unwrap();
    sqlx::query("UPDATE workspace SET payload='temporarily unreadable' WHERE id=1").execute(pool).await.unwrap();
    let calls=Arc::new(std::sync::atomic::AtomicUsize::new(0));let observed=calls.clone();
    app.spawn(key.clone(),async move{observed.fetch_add(1,std::sync::atomic::Ordering::SeqCst);Ok(json!({}))});
    tokio::time::sleep(Duration::from_millis(150)).await;
    sqlx::query("UPDATE workspace SET payload=? WHERE id=1").bind(original).execute(pool).await.unwrap();
    tokio::time::timeout(Duration::from_secs(4),async{loop{
        let d=app.read().await.unwrap();if row(&d,"jobs",&key).unwrap()["status"]=="failed"&&!app.tasks.lock().await.contains_key(&key){break;}
        tokio::time::sleep(Duration::from_millis(25)).await;
    }}).await.unwrap();
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst),0);
    assert!(!app.tasks.lock().await.contains_key(&key));
}

#[tokio::test]
async fn failed_finalization_keeps_handle_and_retries_only_commit() {
    let dir=tempfile::tempdir().unwrap();
    let db=open_db(&dir.path().join("workspace.sqlite")).await.unwrap();
    let (events,_)=broadcast::channel(8);
    let app=App {lifecycle_task_count: Default::default(), lifecycle_admission: Arc::new(crate::runtime_lifecycle_startup::Admission::fixture(crate::accounts::Profile::LikeAvto)), lifecycle_owner: Arc::new(crate::runtime_lifecycle_startup::Admission::fixture(crate::accounts::Profile::LikeAvto).identity().clone()), lifecycle_provider_token: Default::default(), lifecycle_work: Default::default(), media_discovery: Default::default(),preparation_wake: Default::default(),provider_session: Default::default(),account:crate::accounts::Profile::LikeAvto,navigation:crate::account_navigation::Navigation::root(),db:Database::Sqlite(db),gate:Arc::new(crate::writer_gate::WriterGate::default()),execution_gate:Arc::new(Mutex::new(())),preparation_workers: Default::default(),editorial_gate: Default::default(),assistant_gate:Arc::new(Mutex::new(())),assistant_chat_gate:Arc::new(Mutex::new(())),events,csrf:id(),auth:None,public_origin:None,external_writes:false,port:0,data:dir.path().to_owned(),bridge:dir.path().join("never-execute"),node:dir.path().join("no-runtime"),tasks:Arc::new(Mutex::new(HashMap::new())),bootstrap_cache:Arc::new(bootstrap_cache::Cache::default())};
        crate::runtime_lifecycle_app::initialize_app_fixture(&app).await.unwrap();
    let key=app.job("assistant", "fixture").await.unwrap();
    let Database::Sqlite(pool)=&app.db else {unreachable!()};
    sqlx::query("CREATE TRIGGER fail_finish BEFORE UPDATE ON workspace BEGIN SELECT RAISE(ABORT,'isolated test fault'); END").execute(pool).await.unwrap();
    let calls=Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let observed=calls.clone();
    app.spawn(key.clone(),async move {observed.fetch_add(1,std::sync::atomic::Ordering::SeqCst);Ok(json!({"completed_once":true}))});
    tokio::time::timeout(Duration::from_secs(2),async {loop {if calls.load(std::sync::atomic::Ordering::SeqCst)==1 {break;}tokio::time::sleep(Duration::from_millis(10)).await;}}).await.unwrap();
    tokio::time::sleep(Duration::from_millis(150)).await;
    assert!(app.tasks.lock().await.contains_key(&key));
    assert_eq!(row(&app.read().await.unwrap(),"jobs",&key).unwrap()["status"],"running");
    assert!(tokio::time::timeout(Duration::from_millis(50),app.preparation_wake.notified()).await.is_err(),
        "uncommitted completion must not wake the next preparation");
    sqlx::query("DROP TRIGGER fail_finish").execute(pool).await.unwrap();
    tokio::time::timeout(Duration::from_secs(4),app.preparation_wake.notified()).await.unwrap();
    assert!(!app.tasks.lock().await.contains_key(&key),"wake follows task retirement");
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst),1);
    let d=app.read().await.unwrap();
    assert_eq!(row(&d,"jobs",&key).unwrap()["status"],"completed");
    assert_eq!(row(&d,"jobs",&key).unwrap()["result"]["completed_once"],true);
}

#[tokio::test]
async fn preparation_completion_wake_is_workspace_local_and_ignores_discussions() {
    let (app,_dir)=tests::test_app().await;
    let (other,_other_dir)=tests::test_app().await;
    let discussion=app.job("assistant","discussion-fixture").await.unwrap();
    app.change(|d|{row_mut(d,"jobs",&discussion)?["purpose"]=json!("discussion");Ok(())}).await.unwrap();
    app.finish(&discussion,Ok(json!({}))).await;
    assert!(tokio::time::timeout(Duration::from_millis(25),app.preparation_wake.notified()).await.is_err());
    let media=app.job("media","media-fixture").await.unwrap();
    app.finish(&media,Err(internal("fixture media hold"))).await;
    tokio::time::timeout(Duration::from_secs(1),app.preparation_wake.notified()).await.unwrap();
    assert_eq!(row(&app.read().await.unwrap(),"jobs",&media).unwrap()["status"],"failed");
    assert!(tokio::time::timeout(Duration::from_millis(25),other.preparation_wake.notified()).await.is_err());
}
