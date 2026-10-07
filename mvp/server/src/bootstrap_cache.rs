//! Process-local actor-neutral read cache with bounded snapshot history.
//! Tokens identify accepted data generations, never operator authority.
use crate::{ApiResult, internal, performance::Span};
use serde_json::Value;
use std::{collections::VecDeque, future::Future, sync::{Arc,Mutex}, time::{Duration,Instant}};
const HISTORY_GENERATIONS:usize=8;
const HISTORY_BYTES:usize=32*1024*1024;
#[derive(Clone)]
struct Entry {epoch:u64,at:Instant,version:String,value:Arc<Value>,bytes:usize}
// Measure compact JSON without allocating a second full workspace byte vector.
#[derive(Default)]
struct SerializedBytes(usize);
impl std::io::Write for SerializedBytes {
    fn write(&mut self,bytes:&[u8])->std::io::Result<usize> {
        self.0=self.0.checked_add(bytes.len()).ok_or_else(||std::io::Error::new(std::io::ErrorKind::Other,"bootstrap size overflow"))?;
        Ok(bytes.len())
    }
    fn flush(&mut self)->std::io::Result<()> {Ok(())}
}
fn serialized_size(value:&Value)->ApiResult<usize> {
    let mut sink=SerializedBytes::default();
    serde_json::to_writer(&mut sink,value).map_err(|_|internal("Cannot serialize bootstrap cache"))?;Ok(sink.0)
}
#[derive(Default)]
struct State {epoch:u64,observation:u64,external_epoch:u64,entry:Option<Entry>,history:VecDeque<Entry>,history_bytes:usize}
pub(crate) struct Cache {instance:String,state:Mutex<State>,loading:tokio::sync::Mutex<()>}
impl Default for Cache {
    fn default()->Self {Self {instance:uuid::Uuid::new_v4().to_string(),state:Mutex::new(State::default()),loading:tokio::sync::Mutex::new(())}}
}
impl Cache {
    pub fn current_version(&self)->String {let epoch={self.state.lock().unwrap().epoch};self.version(epoch)}
    fn version(&self,epoch:u64)->String {format!("{}:{epoch}",self.instance)}
    // Invalidation keeps the last accepted value as a delta base, never a hit.
    pub fn invalidate(&self) {let _span=Span::new("bootstrap_cache.state.invalidate");let mut state=self.state.lock().unwrap();state.epoch=state.epoch.wrapping_add(1);}
    pub fn observe_external_epoch(&self,external_epoch:u64)->bool {
        let _span=Span::new("bootstrap_cache.state.external_epoch");
        let mut state=self.state.lock().unwrap();
        if external_epoch<=state.external_epoch{return false;}
        state.external_epoch=external_epoch;state.epoch=state.epoch.wrapping_add(1);true
    }
    pub fn snapshot(&self,version:&str)->Option<Arc<Value>> {
        let _span=Span::new("bootstrap_cache.state.snapshot");
        let state=self.state.lock().unwrap();
        state.entry.iter().chain(state.history.iter()).find(|entry|entry.version==version).map(|entry|Arc::clone(&entry.value))
    }
    fn hit_arc(&self)->Option<Arc<Value>> {
        let _span=Span::new("bootstrap_cache.state.hit");
        let state=self.state.lock().unwrap();
        state.entry.as_ref().filter(|entry|entry.epoch==state.epoch&&entry.at.elapsed()<Duration::from_secs(30)).map(|entry|Arc::clone(&entry.value))
    }
    fn copy(value:&Arc<Value>)->Value {let _span=Span::new("bootstrap_cache.copy");(**value).clone()}
    fn hit(&self)->Option<Value> {self.hit_arc().map(|value|Self::copy(&value))}
    pub async fn get<F,Fut>(&self,load:F)->ApiResult<Value> where F:Fn()->Fut,Fut:Future<Output=ApiResult<Value>> {
        self.get_measured(load,|value| {
            let _span=Span::new("bootstrap_cache.serialize");
            serialized_size(value)
        }).await
    }
    // The measuring seam is private; tests exercise the real admission path,
    // including serialization failure and invalidation during preparation.
    async fn get_measured<F,Fut,M>(&self,load:F,measure:M)->ApiResult<Value>
    where F:Fn()->Fut,Fut:Future<Output=ApiResult<Value>>,M:Fn(&Value)->ApiResult<usize> {
        if let Some(value)=self.hit(){return Ok(value);}
        let _loading=self.loading.lock().await;
        if let Some(value)=self.hit(){return Ok(value);}
        let (epoch,previous)={
            let _span=Span::new("bootstrap_cache.state.capture");
            let state=self.state.lock().unwrap();(state.epoch,state.entry.clone())
        };
        // One coherent load; continuous writers never force another DB read.
        let mut value={let _span=Span::new("bootstrap_cache.load");load().await?};
        value["workspaceVersion"]=Value::String(self.version(epoch));
        let changed={
            let _span=Span::new("bootstrap_cache.compare");
            previous.as_ref().is_some_and(|prior|prior.epoch==epoch&&*prior.value!=value)
        };
        let (admission_epoch,raced,observation)={
            let _span=Span::new("bootstrap_cache.state.reserve");
            let mut state=self.state.lock().unwrap();
            let raced=state.epoch!=epoch;
            let observation=if raced {state.observation=state.observation.wrapping_add(1);Some(state.observation)}else{None};
            (state.epoch,raced,observation)
        };
        let target_epoch=if !raced&&changed {admission_epoch.wrapping_add(1)}else{admission_epoch};
        let mut version=match observation {Some(n)=>format!("{}:observed:{n}",self.version(admission_epoch)),None=>self.version(target_epoch)};
        value["workspaceVersion"]=Value::String(version.clone());
        let mut bytes=measure(&value)?;
        let mut value=Arc::new(value);
        // All candidate serialization succeeds before accepted state is touched.
        // If invalidated during preparation, convert once to an observed token.
        // The final observed publish need not chase a moving epoch: it is stale
        // by construction and its unique token never denotes current data.
        let mut retired=Vec::with_capacity(HISTORY_GENERATIONS+2);
        let late_race={
            let _span=Span::new("bootstrap_cache.state.admit");
            let mut state=self.state.lock().unwrap();
            if state.epoch!=admission_epoch {
                state.observation=state.observation.wrapping_add(1);
                Some((state.epoch,state.observation))
            }else{
                if !raced {state.epoch=target_epoch;}
                let entry_epoch=if raced {epoch}else{target_epoch};
                Self::publish(&mut state,Entry {epoch:entry_epoch,at:Instant::now(),version:version.clone(),value:Arc::clone(&value),bytes},&mut retired);
                None
            }
        };
        if let Some((latest,n))=late_race {
            version=format!("{}:observed:{n}",self.version(latest));
            // Only this task owns the candidate; unwrap avoids another deep copy.
            let mut candidate=Arc::try_unwrap(value).unwrap_or_else(|_|panic!("unpublished bootstrap candidate has one owner"));
            candidate["workspaceVersion"]=Value::String(version.clone());
            bytes=measure(&candidate)?;
            value=Arc::new(candidate);
            {
                let _span=Span::new("bootstrap_cache.state.admit_observed");
                let mut state=self.state.lock().unwrap();
                Self::publish(&mut state,Entry {epoch,at:Instant::now(),version,value:Arc::clone(&value),bytes},&mut retired);
            }
        }
        // Retired large trees and the captured previous snapshot drop unlocked.
        {let _span=Span::new("bootstrap_cache.retire");drop(retired);drop(previous);}
        Ok(Self::copy(&value))
    }
    // Called under the state mutex: metadata and Arc moves only. Every removed
    // entry leaves via retired, so last-reference payload destruction is outside.
    fn publish(state:&mut State,entry:Entry,retired:&mut Vec<Entry>) {
        if let Some(previous)=state.entry.take() {
            if previous.version!=entry.version&&previous.bytes<=HISTORY_BYTES {
                state.history_bytes+=previous.bytes;state.history.push_back(previous);
            }else{retired.push(previous);}
        }
        while state.history.len()>HISTORY_GENERATIONS||state.history_bytes>HISTORY_BYTES {
            if let Some(removed)=state.history.pop_front(){state.history_bytes-=removed.bytes;retired.push(removed);}
        }
        state.entry=Some(entry);
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
        // This regression tracks loss of a visual artifact, so pin the exact
        // owner visual requirement instead of relying on the audio-text default.
        let source_version=crate::media_fullframes::source_version(&post,"LikeAvto");
        d["settings"]["postMediaPolicies"]["proof-cache-post"]=json!({"version":1,"revision":1,
            "status":"active","postId":"proof-cache-post","account":"LikeAvto",
            "connectorBinding":crate::active_binding(d)?.to_json(),"sourceVersion":source_version,
            "mode":"full_audio_visual","reason":"Visual artifact cache regression fixture"});
        d["items"]=json!([{"id":"proof-cache-item","postId":post["id"],"postKey":post["postKey"],"objectId":"11391","itemId":"proof-cache-item","conversationKey":"11391:thread","providerStatus":"new","workflow":"prepared","draft":{"text":"Preserve operator text","editedByHuman":true}}]);
        d["materials"]=json!([]);d["knowledge_entries"]=json!([]);d["knowledge_versions"]=json!([]);Ok(())
    }).await.unwrap();
    let waiting=app.read_bootstrap().await.unwrap();
    assert_eq!(waiting["items"][0]["mediaReadiness"]["status"],"media_wait");
    let evidence=crate::media_fullframes::fixture_for_post("LikeAvto",&post);
    let source_seconds=evidence["source"]["durationMs"].as_f64().unwrap()/1000.0;
    app.change(|d|{
        let source_version=crate::media_fullframes::source_version(&post,"LikeAvto");
        d["materials"]=json!([
            {"id":"proof-cache-audio","account":"LikeAvto","kind":"transcript","postKey":post["postKey"],
                "mediaSha256":evidence["source"]["mediaSha256"],"text":"Audio evidence",
                "transcription":{"partial":false,"coverage":"full_audio","sourceVersion":source_version,
                    "mediaDurationSeconds":source_seconds,"audioDurationSeconds":source_seconds}},
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
 #[tokio::test] async fn sync_open_coverage_checkpoint_replaces_cached_bootstrap_and_delta(){
    use crate::*;
    let (app,_folder)=crate::tests::test_app().await;
    let actor=operator_auth::Actor{id:"local-owner".into(),name:"Owner".into(),role:"owner".into(),csrf_token:"csrf-owner".into(),authority_generation:None};
    app.change(|d|{
        d["sync"]["openCoverage"]=json!({"scope":"all-open","pages":8,"done":false,"coverageComplete":false});
        d["sync"]["openFrontier"]=json!({"scope":"all-open","pages":8,"done":false});
        d["jobs"]=json!([{"id":"old-sync","kind":"sync","status":"completed","result":{"openCoverage":{"pages":8}}}]);
        Ok(())
    }).await.unwrap();
    let before=operator_http::bootstrap(axum::extract::State(app.clone()),axum::Extension(actor.clone())).await.unwrap().0;
    assert_eq!(before["sync"]["openCoverage"]["pages"],8);
    let base=before["workspaceVersion"].as_str().unwrap().to_owned();
    app.change(|d|{
        d["sync"]["openCoverage"]["pages"]=json!(21);
        d["sync"]["openFrontier"]["pages"]=json!(21);
        Ok(())
    }).await.unwrap();
    let after=operator_http::bootstrap(axum::extract::State(app.clone()),axum::Extension(actor.clone())).await.unwrap().0;
    assert_eq!(after["sync"]["openCoverage"]["pages"],21);
    assert_eq!(after["sync"]["openFrontier"]["pages"],21);
    assert_ne!(after["workspaceVersion"],before["workspaceVersion"]);
    assert_eq!(after["jobs"][0]["result"]["openCoverage"]["pages"],8);
    let delta=operator_http::bootstrap_delta(axum::extract::State(app.clone()),axum::Extension(actor),axum::extract::Query(HashMap::from([("since".into(),base)]))).await.unwrap().0;
    assert_eq!(delta["kind"],"delta");
    assert_eq!(delta["set"]["sync"]["openCoverage"]["pages"],21);
    app.db.close().await;
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
    let result=c.get(||{let count=n.fetch_add(1,Ordering::SeqCst);let cache=&c;async move {cache.invalidate();Ok(json!({"contentGeneration":count,"sync":{"openCoverage":{"pages":8}}}))}}).await.unwrap();
    assert_eq!(n.load(Ordering::SeqCst),1);assert_eq!(result["contentGeneration"],0);
    assert_eq!(result["sync"]["openCoverage"]["pages"],8);
    assert_ne!(result["workspaceVersion"],initial);assert_ne!(result["workspaceVersion"],c.current_version());assert!(c.hit().is_none());
    let token=result["workspaceVersion"].as_str().unwrap();assert_eq!(*c.snapshot(token).unwrap(),result);
    let next=c.get(||async{Ok(json!({"contentGeneration":1,"sync":{"openCoverage":{"pages":21}}}))}).await.unwrap();
    assert_eq!(next["workspaceVersion"],c.current_version());assert_eq!(*c.snapshot(token).unwrap(),result);
    let delta=crate::workspace_delta::between(&result,&next,"owner");
    assert_eq!(delta["kind"],"delta");assert_eq!(delta["set"]["sync"]["openCoverage"]["pages"],21);
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
    let app=App {lifecycle_task_count: Default::default(), lifecycle_admission: Arc::new(crate::runtime_lifecycle_startup::Admission::fixture(crate::accounts::Profile::LikeAvto)), lifecycle_owner: Arc::new(crate::runtime_lifecycle_startup::Admission::fixture(crate::accounts::Profile::LikeAvto).identity().clone()), lifecycle_provider_token: Default::default(), lifecycle_work: Default::default(), media_discovery: Default::default(),preparation_wake: Default::default(),provider_session: Default::default(),account:crate::accounts::Profile::LikeAvto,navigation:crate::account_navigation::Navigation::root(),db:Database::Sqlite(db),gate:Arc::new(crate::writer_gate::WriterGate::default()),execution_gate:Arc::new(tokio::sync::Mutex::new(())),preparation_workers: Default::default(),editorial_gate: Default::default(),assistant_gate:Arc::new(tokio::sync::Mutex::new(())),assistant_chat_gate:Arc::new(tokio::sync::Mutex::new(())),events,csrf:"owner-secret".into(),auth:None,public_origin:None,external_writes:false,port:0,data:dir.path().to_owned(),bridge:dir.path().join("none"),node:dir.path().join("none"),tasks:Arc::new(tokio::sync::Mutex::new(HashMap::new())),bootstrap_cache:Arc::new(Cache::default())};
        crate::runtime_lifecycle_app::initialize_app_fixture(&app).await.unwrap();
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

#[cfg(test)]
mod r9_tests {
    use super::*;
    use serde_json::json;
    use std::sync::atomic::{AtomicUsize,Ordering};
    fn bytes(value:&Value)->ApiResult<usize> {serialized_size(value)}
    #[test]
    fn counting_sink_measures_exact_compact_json_without_buffering_payload() {
        let value=json!({"unicode":"Привет 🎯","escaped":"\n\t\"\\","nested":[null,true,0,1.5,{"x":"data"}]});
        assert_eq!(serialized_size(&value).unwrap(),serde_json::to_vec(&value).unwrap().len());
        let mut sink=SerializedBytes(usize::MAX);
        assert!(std::io::Write::write(&mut sink,b"x").is_err());assert_eq!(sink.0,usize::MAX);
    }
    fn expire(cache:&Cache) {cache.state.lock().unwrap().entry.as_mut().unwrap().at=Instant::now()-Duration::from_secs(31);}

    #[tokio::test]
    async fn ttl_same_payload_reuses_token_and_public_values_are_independent() {
        let cache=Cache::default();let mut first=cache.get(||async{Ok(json!({"nested":{"text":"kept"}}))}).await.unwrap();
        let version=first["workspaceVersion"].clone();first["nested"]["text"]=json!("caller edit");
        expire(&cache);
        let next=cache.get(||async{Ok(json!({"nested":{"text":"kept"}}))}).await.unwrap();
        assert_eq!(next["workspaceVersion"],version);assert_eq!(next["nested"]["text"],"kept");
        assert!(cache.state.lock().unwrap().history.is_empty());
        assert_eq!(cache.hit().unwrap(),next);
    }

    #[tokio::test]
    async fn serialization_failure_preserves_entry_history_and_current_generation() {
        let cache=Cache::default();let first=cache.get(||async{Ok(json!({"value":1}))}).await.unwrap();
        expire(&cache);let before=cache.current_version();
        let result=cache.get_measured(||async{Ok(json!({"value":2}))},|_| {
            assert!(cache.state.try_lock().is_ok());Err(internal("fixture serialization failure"))
        }).await;
        assert!(result.is_err());assert_eq!(cache.current_version(),before);
        assert_eq!(*cache.snapshot(first["workspaceVersion"].as_str().unwrap()).unwrap(),first);
        assert!(cache.state.lock().unwrap().history.is_empty());
        let next=cache.get(||async{Ok(json!({"value":2}))}).await.unwrap();
        assert_ne!(first["workspaceVersion"],next["workspaceVersion"]);
    }

    #[tokio::test]
    async fn invalidation_during_both_serializations_loads_once_and_keeps_unique_observed_base() {
        let cache=Cache::default();let first=cache.get(||async{Ok(json!({"value":0}))}).await.unwrap();cache.invalidate();
        let loads=AtomicUsize::new(0);let measures=AtomicUsize::new(0);
        let observed=cache.get_measured(||async{loads.fetch_add(1,Ordering::SeqCst);Ok(json!({"value":1}))},|value| {
            assert!(cache.state.try_lock().is_ok());measures.fetch_add(1,Ordering::SeqCst);cache.invalidate();bytes(value)
        }).await.unwrap();
        assert_eq!(loads.load(Ordering::SeqCst),1);assert_eq!(measures.load(Ordering::SeqCst),2);
        assert!(observed["workspaceVersion"].as_str().unwrap().contains(":observed:"));
        assert_ne!(observed["workspaceVersion"],cache.current_version());assert!(cache.hit().is_none());
        assert_eq!(*cache.snapshot(observed["workspaceVersion"].as_str().unwrap()).unwrap(),observed);
        assert_eq!(*cache.snapshot(first["workspaceVersion"].as_str().unwrap()).unwrap(),first);
        let next=cache.get(||async{Ok(json!({"value":2}))}).await.unwrap();
        assert_ne!(observed["workspaceVersion"],next["workspaceVersion"]);assert_eq!(next["workspaceVersion"],cache.current_version());
        let delta=crate::workspace_delta::between(&observed,&next,"owner");assert_eq!(delta["set"]["value"],2);
    }

    #[tokio::test]
    async fn second_serialization_failure_does_not_replace_accepted_history() {
        let cache=Cache::default();let first=cache.get(||async{Ok(json!({"value":0}))}).await.unwrap();expire(&cache);
        let calls=AtomicUsize::new(0);
        let result=cache.get_measured(||async{Ok(json!({"value":1}))},|value| {
            if calls.fetch_add(1,Ordering::SeqCst)==0 {cache.invalidate();bytes(value)}else{Err(internal("second serialization failed"))}
        }).await;
        assert!(result.is_err());assert_eq!(calls.load(Ordering::SeqCst),2);
        assert_eq!(*cache.snapshot(first["workspaceVersion"].as_str().unwrap()).unwrap(),first);
        assert!(cache.state.lock().unwrap().history.is_empty());assert!(cache.hit().is_none());
    }

    #[tokio::test]
    async fn proof_invalidation_during_preparation_and_after_acceptance_cannot_return_current_hit() {
        let cache=Cache::default();let old=cache.get(||async{Ok(json!({"ready":true}))}).await.unwrap();cache.invalidate();
        let measures=AtomicUsize::new(0);
        let observed=cache.get_measured(||async{Ok(json!({"ready":false}))},|value| {
            if measures.fetch_add(1,Ordering::SeqCst)==0 {assert!(cache.observe_external_epoch(7));}bytes(value)
        }).await.unwrap();
        assert!(cache.hit().is_none());assert_eq!(observed["ready"],false);
        assert!(!cache.observe_external_epoch(6));assert!(cache.observe_external_epoch(8));assert!(cache.hit().is_none());
        assert_eq!(*cache.snapshot(old["workspaceVersion"].as_str().unwrap()).unwrap(),old);
        let current=cache.get(||async{Ok(json!({"ready":true}))}).await.unwrap();
        assert_eq!(current["workspaceVersion"],cache.current_version());cache.invalidate();assert!(cache.hit().is_none());
        assert_eq!(*cache.snapshot(current["workspaceVersion"].as_str().unwrap()).unwrap(),current);
    }

    #[tokio::test]
    async fn cache_trace_uses_connected_copy_compare_serialize_and_state_without_payload() {
        let cache=Cache::default();let (_,events)=crate::performance::capture(async {
            cache.get(||async{Ok(json!({"text":"PRIVATE_CACHE_PAYLOAD"}))}).await.unwrap();
            expire(&cache);cache.get(||async{Ok(json!({"text":"PRIVATE_CACHE_PAYLOAD"}))}).await.unwrap();
            cache.hit().unwrap();
        }).await;
        for stage in ["bootstrap_cache.copy","bootstrap_cache.compare","bootstrap_cache.serialize","bootstrap_cache.state.admit"] {
            assert!(events.iter().any(|event|event["stage"]==stage),"missing {stage}");
        }
        assert!(!json!(events).to_string().contains("PRIVATE_CACHE_PAYLOAD"));
    }
}