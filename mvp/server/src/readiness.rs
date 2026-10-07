//! Dependency readiness is separate from process liveness. Protected by normal
//! API authentication; no connection strings or internal errors leave the host.
use super::*;

pub(super) async fn ready(State(app):State<App>)->(StatusCode,Json<Value>){
    match app.db.readiness(Duration::from_secs(2)).await {
        Ok(()) => (StatusCode::OK,Json(json!({"status":"ready","database":"available"}))),
        Err(_) => (StatusCode::SERVICE_UNAVAILABLE,Json(json!({"status":"degraded","database":"unavailable"}))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn unavailable_database_degrades_readiness_but_liveness_survives(){
        let dir=tempfile::tempdir().unwrap();
        let db=open_db(&dir.path().join("workspace.sqlite")).await.unwrap();
        let (events,_)=broadcast::channel(8);
        let app=App{lifecycle_task_count: Default::default(), lifecycle_admission: Arc::new(crate::runtime_lifecycle_startup::Admission::fixture(crate::accounts::Profile::LikeAvto)), lifecycle_owner: Arc::new(crate::runtime_lifecycle_startup::Admission::fixture(crate::accounts::Profile::LikeAvto).identity().clone()), lifecycle_provider_token: Default::default(), lifecycle_work: Default::default(), media_discovery: Default::default(),preparation_wake: Default::default(),provider_session: Default::default(),account:crate::accounts::Profile::LikeAvto,navigation:crate::account_navigation::Navigation::root(),db:Database::Sqlite(db),gate:Arc::new(crate::writer_gate::WriterGate::default()),execution_gate:Arc::new(Mutex::new(())),preparation_workers: Default::default(),editorial_gate: Default::default(),assistant_gate:Arc::new(Mutex::new(())),assistant_chat_gate:Arc::new(Mutex::new(())),events,csrf:id(),auth:None,public_origin:None,external_writes:false,port:0,data:dir.path().to_owned(),bridge:dir.path().join("unused"),node:dir.path().join("unused"),tasks:Arc::new(Mutex::new(HashMap::new())),bootstrap_cache:Arc::new(bootstrap_cache::Cache::default())};
        crate::runtime_lifecycle_app::initialize_app_fixture(&app).await.unwrap();
        assert_eq!(ready(State(app.clone())).await.0,StatusCode::OK);
        app.db.close().await;
        let (status,Json(body))=ready(State(app)).await;
        assert_eq!(status,StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(body,json!({"status":"degraded","database":"unavailable"}));
        assert_eq!(health().await.0["status"],"ok");
    }
}
