use super::*;

#[tokio::test]
async fn failed_start_read_becomes_failed_job_after_storage_recovers_without_dispatch(){
    let dir=tempfile::tempdir().unwrap();
    let db=open_db(&dir.path().join("workspace.sqlite")).await.unwrap();
    let (events,_)=broadcast::channel(8);
    let app=App {account:crate::accounts::Profile::LikeAvto,db:Database::Sqlite(db),gate:Arc::new(crate::writer_gate::WriterGate::default()),execution_gate:Arc::new(Mutex::new(())),assistant_gate:Arc::new(Mutex::new(())),assistant_chat_gate:Arc::new(Mutex::new(())),events,csrf:id(),auth:None,public_origin:None,external_writes:false,port:0,data:dir.path().to_owned(),bridge:dir.path().join("never-execute"),node:dir.path().join("no-runtime"),tasks:Arc::new(Mutex::new(HashMap::new())),bootstrap_cache:Arc::new(bootstrap_cache::Cache::default())};
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
    let app=App {account:crate::accounts::Profile::LikeAvto,db:Database::Sqlite(db),gate:Arc::new(crate::writer_gate::WriterGate::default()),execution_gate:Arc::new(Mutex::new(())),assistant_gate:Arc::new(Mutex::new(())),assistant_chat_gate:Arc::new(Mutex::new(())),events,csrf:id(),auth:None,public_origin:None,external_writes:false,port:0,data:dir.path().to_owned(),bridge:dir.path().join("never-execute"),node:dir.path().join("no-runtime"),tasks:Arc::new(Mutex::new(HashMap::new())),bootstrap_cache:Arc::new(bootstrap_cache::Cache::default())};
    let key=app.job("test_finish", "fixture").await.unwrap();
    let Database::Sqlite(pool)=&app.db else {unreachable!()};
    sqlx::query("CREATE TRIGGER fail_finish BEFORE UPDATE ON workspace BEGIN SELECT RAISE(ABORT,'isolated test fault'); END").execute(pool).await.unwrap();
    let calls=Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let observed=calls.clone();
    app.spawn(key.clone(),async move {observed.fetch_add(1,std::sync::atomic::Ordering::SeqCst);Ok(json!({"completed_once":true}))});
    tokio::time::timeout(Duration::from_secs(2),async {loop {if calls.load(std::sync::atomic::Ordering::SeqCst)==1 {break;}tokio::time::sleep(Duration::from_millis(10)).await;}}).await.unwrap();
    tokio::time::sleep(Duration::from_millis(150)).await;
    assert!(app.tasks.lock().await.contains_key(&key));
    assert_eq!(row(&app.read().await.unwrap(),"jobs",&key).unwrap()["status"],"running");
    sqlx::query("DROP TRIGGER fail_finish").execute(pool).await.unwrap();
    tokio::time::timeout(Duration::from_secs(4),async {loop {if !app.tasks.lock().await.contains_key(&key){break;}tokio::time::sleep(Duration::from_millis(25)).await;}}).await.unwrap();
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst),1);
    let d=app.read().await.unwrap();
    assert_eq!(row(&d,"jobs",&key).unwrap()["status"],"completed");
    assert_eq!(row(&d,"jobs",&key).unwrap()["result"]["completed_once"],true);
}
