//! Process-local actor-neutral read cache with bounded snapshot history.
//! Tokens identify accepted data generations, never operator authority.
use crate::{ApiResult, internal};
use serde_json::Value;
use std::{collections::VecDeque, future::Future, sync::{Arc,Mutex}, time::{Duration,Instant}};
const HISTORY_GENERATIONS:usize=8;
const HISTORY_BYTES:usize=32*1024*1024;
struct Entry {epoch:u64,at:Instant,value:Arc<Value>,bytes:usize}
#[derive(Default)]
struct State {epoch:u64,observation:u64,external_epoch:u64,entry:Option<Entry>,history:VecDeque<Entry>,history_bytes:usize}
pub(crate) struct Cache {instance:String,state:Mutex<State>,loading:tokio::sync::Mutex<()>}
impl Default for Cache {
    fn default()->Self {Self {instance:uuid::Uuid::new_v4().to_string(),state:Mutex::new(State::default()),loading:tokio::sync::Mutex::new(())}}
}
impl Cache {
    pub fn current_version(&self)->String {let state=self.state.lock().unwrap();self.version(state.epoch)}
    fn version(&self,epoch:u64)->String {format!("{}:{epoch}",self.instance)}
    // Keep the last accepted value available as a delta base until its replacement
    // is admitted. It is never returned by hit() after this invalidation.
    pub fn invalidate(&self) {let mut state=self.state.lock().unwrap();state.epoch=state.epoch.wrapping_add(1);}
    // Verified artifact readiness can change without a database commit. Fold
    // that monotonic process-local generation into the same delta/SSE token.
    // Older concurrent observers must not move it backward or invalidate twice.
    pub fn observe_external_epoch(&self,external_epoch:u64)->bool {
        let mut state=self.state.lock().unwrap();
        if external_epoch<=state.external_epoch{return false;}
        state.external_epoch=external_epoch;
        state.epoch=state.epoch.wrapping_add(1);true
    }
    pub fn snapshot(&self,version:&str)->Option<Arc<Value>> {
        let state=self.state.lock().unwrap();
        state.entry.iter().chain(state.history.iter()).find(|entry|entry.value["workspaceVersion"].as_str()==Some(version)).map(|entry|entry.value.clone())
    }
    fn hit(&self)->Option<Value> {let state=self.state.lock().unwrap();state.entry.as_ref().filter(|entry|entry.epoch==state.epoch&&entry.at.elapsed()<Duration::from_secs(30)).map(|entry|(*entry.value).clone())}
    pub async fn get<F,Fut>(&self,load:F)->ApiResult<Value> where F:Fn()->Fut,Fut:Future<Output=ApiResult<Value>> {
        if let Some(value)=self.hit(){return Ok(value);}
        let _loading=self.loading.lock().await;
        if let Some(value)=self.hit(){return Ok(value);}
        let epoch=self.state.lock().unwrap().epoch;
        // The loader returns one coherent database snapshot. A commit racing
        // that read makes it potentially behind, not internally inconsistent.
        // Do not starve bootstrap by repeating costly reads until writers stop.
        let mut value=load().await?;
        let mut state=self.state.lock().unwrap();
        let raced=state.epoch!=epoch;
        if raced {
            state.observation=state.observation.wrapping_add(1);
            value["workspaceVersion"]=Value::String(format!("{}:observed:{}",self.version(state.epoch),state.observation));
        } else {
            value["workspaceVersion"]=Value::String(self.version(epoch));
            // TTL reloads can observe an unsupported external writer. A changed
            // payload must never reuse the previous token.
            if let Some(previous)=&state.entry {
                if previous.epoch==epoch&&*previous.value!=value {
                    state.epoch=state.epoch.wrapping_add(1);
                    value["workspaceVersion"]=Value::String(self.version(state.epoch));
                }
            }
        }
        if let Some(previous)=state.entry.take() {
            if previous.value["workspaceVersion"]!=value["workspaceVersion"]&&previous.bytes<=HISTORY_BYTES {
                state.history_bytes+=previous.bytes;
                state.history.push_back(previous);
            }
        }
        while state.history.len()>HISTORY_GENERATIONS||state.history_bytes>HISTORY_BYTES {
            if let Some(removed)=state.history.pop_front(){state.history_bytes-=removed.bytes;}
        }
        let bytes=serde_json::to_vec(&value).map_err(|_|internal("Cannot serialize bootstrap cache"))?.len();
        // A raced observation is a valid retained delta base, but its
        // start epoch deliberately prevents hit() from calling it current.
        let entry_epoch=if raced {epoch} else {state.epoch};
        state.entry=Some(Entry {epoch:entry_epoch,at:Instant::now(),value:Arc::new(value.clone()),bytes});
        Ok(value)
    }
}
#[cfg(test)]
mod tests {
 use super::*; use serde_json::json; use std::sync::atomic::{AtomicUsize,Ordering};
 #[tokio::test] async fn media_artifact_loss_updates_bootstrap_delta_and_event_without_rewriting_draft(){
    let (app,_folder)=crate::tests::test_app().await;
    let post=json!({"id":"proof-cache-post","postKey":"11391:proof-cache-post","title":"Video cache test","attachments":[{"type":"video"}]});
    app.change(|d|{
        d["posts"]=json!([post.clone()]);
        d["items"]=json!([{"id":"proof-cache-item","postId":post["id"],"postKey":post["postKey"],"objectId":"11391","itemId":"proof-cache-item","conversationKey":"11391:thread","providerStatus":"new","workflow":"prepared","draft":{"text":"Preserve operator text","editedByHuman":true}}]);
        d["materials"]=json!([]);d["knowledge_entries"]=json!([]);d["knowledge_versions"]=json!([]);Ok(())
    }).await.unwrap();
    let waiting=app.read_bootstrap().await.unwrap();
    assert_eq!(waiting["items"][0]["mediaReadiness"]["status"],"media_wait");
    let evidence=crate::media_fullframes::fixture_for_post("LikeAvto",&post);
    app.change(|d|{
        d["materials"]=json!([
            {"id":"proof-cache-audio","kind":"transcript","postKey":post["postKey"],"text":"Audio evidence"},
            {"id":"proof-cache-visual","account":"LikeAvto","kind":"visual_context","postKey":post["postKey"],"mediaSha256":evidence["source"]["mediaSha256"],"text":"Visual evidence","visualEvidence":evidence}
        ]);crate::knowledge::sync_catalog(d,&crate::now()).map_err(crate::bad)
    }).await.unwrap();
    let ready=app.read_bootstrap().await.unwrap();
    assert_eq!(ready["items"][0]["mediaReadiness"]["status"],"ready");
    assert_ne!(ready["workspaceVersion"],waiting["workspaceVersion"]);
    let mut events=app.events.subscribe();
    let store=crate::media_fullframes::store().unwrap();
    let proof_file=store.path(&crate::media_fullframes::reference(&evidence["finalEvidence"]).unwrap()).unwrap();
    std::fs::remove_file(proof_file).unwrap();
    crate::media_fullframes::refresh(&app).await.unwrap();app.observe_media_proofs();
    assert!(events.try_recv().is_ok());
    let held=app.read_bootstrap().await.unwrap();
    assert_ne!(held["items"][0]["mediaReadiness"]["status"],"ready");
    assert_eq!(held["items"][0]["draft"],ready["items"][0]["draft"]);
    assert_eq!(held["items"][0]["workflow"],"prepared");
    let delta=crate::workspace_delta::between(&ready,&held,"owner");
    assert_eq!(delta["kind"],"delta");assert_ne!(delta["workspaceVersion"],delta["baseVersion"]);
    assert_ne!(delta["collections"]["items"]["upsert"][0]["mediaReadiness"]["status"],"ready");
    assert!(held.get("mediaReadinessCatalog").is_none());app.db.close().await;
 }
 #[test] fn bootstrap_exposes_progress_without_private_download_locator_or_lease(){
    let mut data=crate::empty();
    data["jobs"]=json!([{"id":"visual","status":"queued","result":{"visualProgress":{
      "phase":"scan","completedSelectedFrames":4,"sourceProjection":{"sourceUrl":"PRIVATE_DOWNLOAD_LOCATOR"},
      "leaseId":"PRIVATE_LEASE","materialEpoch":"PRIVATE_EPOCH"}}}]);
    let view=crate::bootstrap_view(data.clone(),"csrf");
    assert_eq!(view["jobs"][0]["result"]["visualProgress"]["completedSelectedFrames"],4);
    for private in ["PRIVATE_DOWNLOAD_LOCATOR","PRIVATE_LEASE","PRIVATE_EPOCH"]{assert!(!view.to_string().contains(private));assert!(data.to_string().contains(private));}
 }
 #[tokio::test] async fn artifact_epoch_invalidates_cached_readiness_and_preserves_delta_base(){
    let c=Cache::default();assert!(c.observe_external_epoch(1));
    let old=c.get(||async{Ok(json!({"ready":false}))}).await.unwrap();
    assert!(!c.observe_external_epoch(1));assert!(!c.observe_external_epoch(0));
    assert!(c.hit().is_some());assert!(c.observe_external_epoch(2));assert!(c.hit().is_none());
    let next=c.get(||async{Ok(json!({"ready":true}))}).await.unwrap();
    assert_ne!(old["workspaceVersion"],next["workspaceVersion"]);
    assert_eq!(*c.snapshot(old["workspaceVersion"].as_str().unwrap()).unwrap(),old);
    assert!(!c.observe_external_epoch(1));assert_eq!(next["workspaceVersion"],c.current_version());
 }
 #[tokio::test] async fn history_is_bounded_and_restart_has_no_base(){
    let c=Cache::default();let mut first=String::new();
    for generation in 0..12 {c.invalidate();let v=c.get(||async {Ok(json!({"generation":generation}))}).await.unwrap();if generation==0{first=v["workspaceVersion"].as_str().unwrap().into();}}
    let state=c.state.lock().unwrap();assert_eq!(state.history.len(),HISTORY_GENERATIONS);assert!(state.history_bytes<=HISTORY_BYTES);drop(state);
    assert!(c.snapshot(&first).is_none());assert!(Cache::default().snapshot(&c.current_version()).is_none());
 }
 #[tokio::test] async fn ttl_changed_payload_receives_distinct_version(){
    let c=Cache::default();let first=c.get(||async {Ok(json!({"value":1}))}).await.unwrap();
    c.state.lock().unwrap().entry.as_mut().unwrap().at=Instant::now()-Duration::from_secs(31);
    let next=c.get(||async {Ok(json!({"value":2}))}).await.unwrap();assert_ne!(first["workspaceVersion"],next["workspaceVersion"]);assert!(c.snapshot(first["workspaceVersion"].as_str().unwrap()).is_some());
 }
 #[tokio::test] async fn oversized_prior_snapshot_is_not_retained_in_history(){
    let c=Cache::default();let first=c.get(||async {Ok(json!({"value":1}))}).await.unwrap();
    // Simulate measured serialized bytes without allocating a 32 MiB fixture.
    c.state.lock().unwrap().entry.as_mut().unwrap().bytes=HISTORY_BYTES+1;
    c.invalidate();assert!(c.snapshot(first["workspaceVersion"].as_str().unwrap()).is_some());
    c.get(||async {Ok(json!({"value":2}))}).await.unwrap();assert!(c.state.lock().unwrap().history.is_empty());
 }
 #[test] fn instance_tokens_change_after_restart_and_epoch_invalidation(){let a=Cache::default();let b=Cache::default();assert_ne!(a.current_version(),b.current_version());let before=a.current_version();a.invalidate();assert_ne!(before,a.current_version());}
 #[tokio::test] async fn racing_snapshot_is_retained_but_never_labeled_current(){
    let c=Cache::default();let initial=c.current_version();let n=AtomicUsize::new(0);
    let result=c.get(||{let count=n.fetch_add(1,Ordering::SeqCst);let cache=&c;async move {cache.invalidate();Ok(json!({"contentGeneration":count}))}}).await.unwrap();
    assert_eq!(n.load(Ordering::SeqCst),1);assert_eq!(result["contentGeneration"],0);
    assert_ne!(result["workspaceVersion"],initial);assert_ne!(result["workspaceVersion"],c.current_version());assert!(c.hit().is_none());
    let token=result["workspaceVersion"].as_str().unwrap();assert_eq!(*c.snapshot(token).unwrap(),result);
    let next=c.get(||async{Ok(json!({"contentGeneration":1}))}).await.unwrap();
    assert_eq!(next["workspaceVersion"],c.current_version());assert_eq!(*c.snapshot(token).unwrap(),result);
 }
 #[tokio::test] async fn cache_reuses_compact_value_and_invalidates(){let c=Cache::default();let n=AtomicUsize::new(0);for _ in 0..2 {c.get(||async{n.fetch_add(1,Ordering::SeqCst);Ok(json!({"value":1}))}).await.unwrap();}assert_eq!(n.load(Ordering::SeqCst),1);c.invalidate();c.get(||async{n.fetch_add(1,Ordering::SeqCst);Ok(json!({"value":2}))}).await.unwrap();assert_eq!(n.load(Ordering::SeqCst),2);}
 #[tokio::test] async fn continuous_database_commits_return_coherent_unique_snapshots_without_retry_starvation(){
    use axum::response::IntoResponse;
    let (app,_folder)=crate::tests::test_app().await;
    app.change(|d|{d["settings"]["counter"]=json!(0);d["settings"]["mirror"]=json!(0);Ok(())}).await.unwrap();
    let mut prior:Option<String>=None;let mut versions=std::collections::HashSet::new();
    for generation in 0..12 {
        let value=app.bootstrap_cache.get(||async{
            let coherent=app.db.read_bootstrap_source().await?;
            // A real committed mutation after the statement snapshot, on every
            // loader invocation. The former retry loop failed after four reads.
            app.change(|d|{let n=d["settings"]["counter"].as_u64().unwrap()+1;d["settings"]["counter"]=json!(n);d["settings"]["mirror"]=json!(n);Ok(())}).await?;
            Ok(coherent)
        }).await.unwrap();
        assert_eq!(axum::Json(value.clone()).into_response().status(),axum::http::StatusCode::OK);
        assert_eq!(value["settings"]["counter"],generation);assert_eq!(value["settings"]["counter"],value["settings"]["mirror"]);
        let token=value["workspaceVersion"].as_str().unwrap().to_owned();assert!(versions.insert(token.clone()));
        assert_ne!(token,app.bootstrap_cache.current_version());assert!(app.bootstrap_cache.hit().is_none());
        if let Some(previous)=prior {
            let base=app.bootstrap_cache.snapshot(&previous).unwrap();
            let delta=crate::workspace_delta::between(&base,&value,"owner");
            assert_eq!(delta["kind"],"delta");assert_eq!(delta["baseVersion"],previous);assert_eq!(delta["workspaceVersion"],token);
        }
        prior=Some(token);
    }
    let stable=app.read_bootstrap().await.unwrap();assert_eq!(stable["settings"]["counter"],12);assert_eq!(stable["workspaceVersion"],app.bootstrap_cache.current_version());
    assert!(app.bootstrap_cache.snapshot(&prior.unwrap()).is_some());app.db.close().await;
 }
 #[tokio::test] async fn cold_requests_coalesce(){let c=Cache::default();let n=AtomicUsize::new(0);let load=||async{n.fetch_add(1,Ordering::SeqCst);tokio::task::yield_now().await;Ok(json!({}))};let (a,b)=tokio::join!(c.get(load),c.get(load));assert!(a.is_ok()&&b.is_ok());assert_eq!(n.load(Ordering::SeqCst),1);}

 #[tokio::test(flavor="multi_thread",worker_threads=2)]
 async fn bootstrap_and_delta_handlers_stay_available_and_private_during_writes(){
    use crate::*;
    let (app,_folder)=crate::tests::test_app().await;
    app.change(|d|{
        d["settings"]["counter"]=json!(0);d["settings"]["mirror"]=json!(0);
        d["conversations"]=json!([{"id":"alice","operatorId":"alice","messages":[{"text":"alice-private"}]},{"id":"bob","operatorId":"bob","messages":[{"text":"bob-private"}]}]);
        d["jobs"]=json!([{"id":"alice-job","kind":"assistant","refId":"alice","status":"running","detail":"alice-private"},{"id":"bob-job","kind":"assistant","refId":"bob","status":"running","detail":"bob-private"}]);Ok(())
    }).await.unwrap();
    let writer_app=app.clone();
    let writer=tokio::spawn(async move {
        for n in 1..=80 {
            writer_app.change(|d|{d["settings"]["counter"]=json!(n);d["settings"]["mirror"]=json!(n);Ok(())}).await.unwrap();
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    });
    let mut counters=HashMap::new();let mut versions=HashMap::<String,String>::new();let mut observed=std::collections::HashSet::new();
    for _ in 0..40 {for who in ["alice","bob"] {
        let actor=operator_auth::Actor{id:who.into(),name:who.into(),role:"operator".into(),csrf_token:format!("csrf-{who}"),authority_generation:None};
        let response=operator_http::bootstrap(State(app.clone()),axum::Extension(actor.clone())).await.unwrap();
        assert_eq!(response.clone().into_response().status(),StatusCode::OK);
        let value=response.0;let counter=value["settings"]["counter"].as_u64().unwrap();
        observed.insert(counter);
        assert_eq!(value["settings"]["counter"],value["settings"]["mirror"]);
        assert!(counter>=*counters.get(who).unwrap_or(&0));counters.insert(who,counter);
        let other=if who=="alice"{"bob-private"}else{"alice-private"};
        assert!(!value.to_string().contains(other));assert_eq!(value["csrfToken"],format!("csrf-{who}"));
        assert_eq!(value["conversations"].as_array().unwrap().len(),1);assert_eq!(value["jobs"].as_array().unwrap().len(),1);
        let token=value["workspaceVersion"].as_str().unwrap().to_owned();
        if let Some(base)=versions.insert(who.into(),token){
            let delta=operator_http::bootstrap_delta(State(app.clone()),axum::Extension(actor),axum::extract::Query(HashMap::from([("since".into(),base.clone())]))).await.unwrap();
            assert_eq!(delta.clone().into_response().status(),StatusCode::OK);
            let delta=delta.0;assert!(!delta.to_string().contains(other));
            if delta["kind"]=="delta" {assert_eq!(delta["actorId"],who);assert_eq!(delta["baseVersion"],base);assert_eq!(delta["set"]["csrfToken"],format!("csrf-{who}"));}
            else {assert_eq!(delta["kind"],"full");assert_eq!(delta["snapshot"]["operator"]["id"],who);}
        }
    }tokio::time::sleep(Duration::from_millis(1)).await;}
    assert!(observed.len()>1,"handler reads must overlap committed writer progress");
    writer.await.unwrap();let latest=app.read_bootstrap().await.unwrap();
    assert_eq!(latest["settings"]["counter"],80);
    // Artifact verification in another test can invalidate the process-wide
    // proof epoch during this read, even after this workspace writer finishes.
    // A raced observation must remain an exact, coherent retained delta base;
    // it need not claim the independently advancing current-version token.
    let retained=app.bootstrap_cache.snapshot(latest["workspaceVersion"].as_str().unwrap()).unwrap();
    assert_eq!(*retained,latest);
    app.db.close().await;
 }
}

#[tokio::test]
async fn compact_cache_never_shares_actor_csrf_or_private_chats() {
    use crate::*;
    let dir=tempfile::tempdir().unwrap();
    let db=open_db(&dir.path().join("workspace.sqlite")).await.unwrap();
    let (events,_)=broadcast::channel(8);
    let app=App {account:crate::accounts::Profile::LikeAvto,db:Database::Sqlite(db),gate:Arc::new(crate::writer_gate::WriterGate::default()),execution_gate:Arc::new(tokio::sync::Mutex::new(())),assistant_gate:Arc::new(tokio::sync::Mutex::new(())),assistant_chat_gate:Arc::new(tokio::sync::Mutex::new(())),events,csrf:"owner-secret".into(),auth:None,public_origin:None,external_writes:false,port:0,data:dir.path().to_owned(),bridge:dir.path().join("none"),node:dir.path().join("none"),tasks:Arc::new(tokio::sync::Mutex::new(HashMap::new())),bootstrap_cache:Arc::new(Cache::default())};
    app.change(|d|{d["conversations"]=json!([{"id":"a","operatorId":"a","messages":[]},{"id":"b","operatorId":"b","messages":[]}]);Ok(())}).await.unwrap();
    for who in ["a","b"] {
        let actor=operator_auth::Actor{id:who.into(),name:who.into(),role:"operator".into(),csrf_token:format!("csrf-{who}"),authority_generation:None};
        let version=workspace_version::get(State(app.clone()),axum::Extension(actor.clone())).await.0;
        assert_eq!(version["actorId"],who);assert_eq!(version["csrfToken"],format!("csrf-{who}"));assert_eq!(version["workspaceVersion"],app.bootstrap_cache.current_version());
        let view=operator_http::bootstrap(State(app.clone()),axum::Extension(actor)).await.unwrap().0;
        assert_eq!(view["conversations"].as_array().unwrap().len(),1);
        assert_eq!(view["conversations"][0]["id"],who);
        assert_eq!(view["csrfToken"],format!("csrf-{who}"));
    }
    let cached=app.read_bootstrap().await.unwrap();
    assert!(cached.get("csrfToken").is_none() && cached.get("operator").is_none());
    let prior_version=cached["workspaceVersion"].clone();
    app.change(|d|{d["settings"]["updated"]=json!(true);Ok(())}).await.unwrap();
    let updated=app.read_bootstrap().await.unwrap();
    assert_eq!(updated["settings"]["updated"],true);
    assert_ne!(updated["workspaceVersion"],prior_version);
    assert_eq!(updated["workspaceVersion"],app.bootstrap_cache.current_version());
    let base_version=updated["workspaceVersion"].as_str().unwrap().to_owned();
    app.change(|d|{d["conversations"][0]["messages"]=json!([{"text":"alice private changed"}]);d["conversations"][1]["messages"]=json!([{"text":"bob private changed"}]);d["jobs"]=json!([{"id":"alice-job","kind":"assistant","refId":"a","status":"running","detail":"alice private job"},{"id":"bob-job","kind":"assistant","refId":"b","status":"running","detail":"bob private job"}]);Ok(())}).await.unwrap();
    for who in ["a","b"] {
        let actor=operator_auth::Actor{id:who.into(),name:who.into(),role:"operator".into(),csrf_token:format!("csrf-{who}"),authority_generation:None};
        let query=HashMap::from([("since".to_owned(),base_version.clone())]);
        let delta=operator_http::bootstrap_delta(State(app.clone()),axum::Extension(actor.clone()),axum::extract::Query(query)).await.unwrap().0;
        assert_eq!(delta["kind"],"delta");assert_eq!(delta["actorId"],who);
        assert_eq!(delta["collections"]["conversations"]["upsert"][0]["id"],who);
        let other=if who=="a"{"bob private"}else{"alice private"};assert!(!delta.to_string().contains(other));assert!(!delta.to_string().contains("owner-secret"));
        let full=operator_http::bootstrap_delta(State(app.clone()),axum::Extension(actor),axum::extract::Query(HashMap::from([("since".into(),"restarted-process:1".into())]))).await.unwrap().0;
        assert_eq!(full["kind"],"full");assert_eq!(full["snapshot"]["operator"]["id"],who);assert_eq!(full["snapshot"]["csrfToken"],format!("csrf-{who}"));assert!(!full.to_string().contains(other));
    }
}
