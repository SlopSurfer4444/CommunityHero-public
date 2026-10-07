//! Synchronous writer context, not a second lifecycle store or dispatch authority.
//! Capture before the writer lock; require against its CURRENT locked metadata.
use crate::{ApiResult,App,Value,conflict};
use crate::runtime_lifecycle::{self,AdmissionClass,OwnerToken,RuntimeIdentity};
use std::cell::RefCell;

#[derive(Clone)]
pub(crate) struct Capture { identity:RuntimeIdentity,token:Option<OwnerToken> }
thread_local! {static WRITER:RefCell<Option<Capture>>=const{RefCell::new(None)};}
// Test mode carries no owner/admission and never installs writer context. It
// only makes connected endpoint tests use the production missing-context rule.
#[cfg(test)]
tokio::task_local! {static STRICT_NEW_JOBS: bool;}
struct Restore(Option<Capture>);
impl Drop for Restore {fn drop(&mut self){WRITER.with(|v|{*v.borrow_mut()=self.0.take();});}}
impl Capture {
    // A unavailable/non-Running capture does not veto an ordinary completion.
    // Its first NEW reservation will fail closed inside the locked transaction.
    pub(crate) async fn read(app:&App)->Self {
        let token=crate::runtime_maintenance::admission_token(app,AdmissionClass::Preparation).await.ok();
        Self{identity:(*app.lifecycle_owner).clone(),token}
    }
    pub(crate) fn with<T>(&self,d:&mut Value,f:impl FnOnce(&mut Value)->ApiResult<T>)->ApiResult<T> {
        runtime_lifecycle::current_owner(d, &self.identity)?;
        let old=WRITER.with(|v|v.replace(Some(self.clone())));
        let _restore=Restore(old);
        let before: std::collections::BTreeMap<String,String> = crate::list(d,"jobs").iter()
            .filter_map(|job|Some((job["id"].as_str()?.to_owned(),job["status"].as_str()?.to_owned()))).collect();
        let result = f(d)?;
        let admitted_before=serde_json::json!({"jobs":before.keys().map(|id|serde_json::json!({"id":id})).collect::<Vec<_>>()});
        crate::continuous_preparation::stamp_admissions(&admitted_before,d)?;
        for job in crate::list(d,"jobs") {
            if matches!(job["status"].as_str(),Some("queued"|"running")) {
                let id=job["id"].as_str().ok_or_else(||conflict("Runnable job identity missing"))?;
                let status=job["status"].as_str().unwrap();
                if before.get(id).map(String::as_str)!=Some(status) {
                    require_new_job(d,job["kind"].as_str().ok_or_else(||conflict("Runnable job kind missing"))?)?;
                }
            }
        }
        runtime_lifecycle::current_owner(d, &self.identity)?;
        Ok(result)
    }
    /// Synchronous detached speculation only. Identity comes from the native
    /// App, never mutable workspace metadata; no writer context crosses await.
    pub(crate) fn preview<T>(app:&App,token:&OwnerToken,mut detached:Value,f:impl FnOnce(&mut Value)->ApiResult<T>)->ApiResult<(Value,T)> {
        let capture=Self{identity:(*app.lifecycle_owner).clone(),token:Some(token.clone())};
        runtime_lifecycle::require_runtime_owner(token,&capture.identity)?;
        runtime_lifecycle::require_admission(&detached,token,AdmissionClass::Preparation)?;
        let result=capture.with(&mut detached,f)?;
        Ok((detached,result))
    }
    /// For the item projection only: jobs are intentionally not loaded. Their
    /// omission grants no evidence about global history and no way to write jobs.
    pub(crate) fn with_jobless_scope<T>(&self,d:&mut Value,f:impl FnOnce(&mut Value)->ApiResult<T>)->ApiResult<T> {
        if d.get("jobs").is_some() {return Err(conflict("Jobless item scope must omit jobs"));}
        runtime_lifecycle::current_owner(d, &self.identity)?;
        let old=WRITER.with(|v|v.replace(Some(self.clone())));
        let _restore=Restore(old);
        let result=f(d)?;
        if d.get("jobs").is_some() {return Err(conflict("Jobless item scope cannot add jobs"));}
        runtime_lifecycle::current_owner(d, &self.identity)?;
        Ok(result)
    }
}
pub(crate) fn job_class(kind:&str)->AdmissionClass {
    match kind {
        "sync"|"status_sync"|"provider-scan"|"source-refresh"=>AdmissionClass::SourceRead,
        "execute"|"reconcile"=>AdmissionClass::SocialDispatch,
        "media"=>AdmissionClass::Media,
        _=>AdmissionClass::Preparation,
    }
}
pub(crate) fn require_new_job(d:&Value,kind:&str)->ApiResult<()> {
    // Standalone historical reducer fixtures retain their compatibility path.
    // Native App preview/writers always install a fixed Capture; production and
    // strict negative tests execute the exact same fail-closed implementation.
    #[cfg(test)] if !STRICT_NEW_JOBS.try_with(|strict|*strict).unwrap_or(false)
        && WRITER.with(|v|v.borrow().is_none()) {
        if d.get("runtimeLifecycle").is_none(){return Ok(());}
        return runtime_lifecycle::admission_token(d,job_class(kind)).map(|_|());
    }
    require_new_job_strict(d,kind)
}
fn require_new_job_strict(d:&Value,kind:&str)->ApiResult<()> {
    WRITER.with(|v|{
        let capture=v.borrow();
        let c=capture.as_ref().ok_or_else(||conflict("New work has no fixed native writer identity"))?;
        let token=c.token.as_ref().ok_or_else(||conflict("Runtime lifecycle is unavailable or closed; no new job admitted"))?;
        runtime_lifecycle::require_runtime_owner(token,&c.identity)?;
        runtime_lifecycle::require_admission(d,token,job_class(kind))
    })
}

#[cfg(test)]mod tests {
    use super::*;use serde_json::json;
    fn fixture()->(Value,Capture){
        let identity=RuntimeIdentity{account:"LikeAvto".into(),runtime_id:"explicit-app-fixture".into(),release_sha256:"a".repeat(64)};
        let token=OwnerToken{account:identity.account.clone(),runtime_id:identity.runtime_id.clone(),release_sha256:identity.release_sha256.clone(),epoch:1};
        let mut d=json!({"account":"LikeAvto","connectorBinding":{},"jobs":[],"operations":[],"approvals":[],"audit":[],"materials":[],"knowledge_entries":[],"knowledge_versions":[]});
        let digest=runtime_lifecycle::ledger_digest(&d).unwrap();runtime_lifecycle::initialize(&mut d,token.clone(),&"b".repeat(64),&digest).unwrap();
        (d,Capture{identity,token:Some(token)})
    }
    #[test]fn missing_capture_and_nested_restore_fail_closed(){
        let(mut d,c)=fixture();assert!(require_new_job(&d,"assistant").is_ok());
        c.with(&mut d, |d|{require_new_job(&d,"assistant")?;let mut closed=c.clone();closed.token=None;
            assert!(closed.with(d, |d|require_new_job(&d,"sync")).is_err());require_new_job(&d,"assistant")}).unwrap();
        assert!(require_new_job(&d,"assistant").is_ok());
    }
    #[test]fn slow_capture_cannot_cross_drain_epoch_and_completion_still_possible(){
        let(mut d,c)=fixture();let t=c.token.as_ref().unwrap();let target=runtime_lifecycle::AdmittedTarget{release_sha256:"c".repeat(64),media_analysis_generation:1,asr_disabled:true};
        runtime_lifecycle::begin_drain_for_release(&mut d,t,&target,"fixture-drain").unwrap();
        for kind in ["sync","status_sync","assistant","media","execute","reconcile"]{assert!(c.with(&mut d, |d|require_new_job(&d,kind)).is_err());}
        c.with(&mut d, |d|{d["completedEvidence"]=json!("retained");Ok(())}).unwrap();assert_eq!(d["completedEvidence"],"retained");
    }
}

/// Explicit synthetic native App bootstrap. Never compiled into production.
#[cfg(test)]
pub(crate) async fn initialize_app_fixture(app:&App)->ApiResult<()> {
    initialize_fixture_database(&app.db,app.account,&app.lifecycle_owner).await
}

// Seed company metadata in the isolated ordinary fixture writer. The guarded
// lifecycle writer below changes ONLY runtimeLifecycle, exactly as production.
#[cfg(test)]
async fn initialize_fixture_database(db:&crate::Database,profile:crate::accounts::Profile,identity:&RuntimeIdentity)->ApiResult<()> {
    if identity.account!=profile.display(){return Err(conflict("Fixture company identity mismatch"));}
    db.change(|d| {
        if d.get("runtimeLifecycle").is_some(){return Err(conflict("Fixture lifecycle already initialized"));}
        crate::accounts::initialize(d,profile)?;
        if d.get("knowledge_entries").is_none(){d["knowledge_entries"]=serde_json::json!([]);}
        if d.get("knowledge_versions").is_none(){d["knowledge_versions"]=serde_json::json!([]);}
        Ok(())
    }).await?;
    db.change_runtime_lifecycle_with_ledger(|d| {
        crate::runtime_lifecycle_startup::initialize_fixture(d,identity).map(|_|())
    }).await
}

#[cfg(test)]mod fixture_database_tests {
    use super::*;use serde_json::json;
    async fn database()->(crate::Database,tempfile::TempDir){
        let temp=tempfile::tempdir().unwrap();
        let pool=crate::open_db(&temp.path().join("bootstrap.sqlite")).await.unwrap();
        (crate::Database::Sqlite(pool),temp)
    }
    #[tokio::test]async fn empty_fixture_bootstraps_through_protected_writer_for_both_accounts(){
        for profile in [crate::accounts::Profile::LikeAvto,crate::accounts::Profile::BawRussia]{
            let(db,_temp)=database().await;
            let identity=crate::runtime_lifecycle_startup::Admission::fixture(profile).identity().clone();
            initialize_fixture_database(&db,profile,&identity).await.unwrap();
            let after=db.read().await.unwrap();
            runtime_lifecycle::current_owner(&after,&identity).unwrap();
            assert_eq!(after["account"],profile.display());
            let before=after.clone();
            assert!(initialize_fixture_database(&db,profile,&identity).await.is_err());
            assert_eq!(db.read().await.unwrap(),before);
        }
    }
    #[tokio::test]async fn seeded_fixture_keeps_membership_and_foreign_company_is_rejected(){
        let(db,_temp)=database().await;
        db.change(|d|{
            crate::accounts::initialize(d,crate::accounts::Profile::BawRussia)?;
            d["items"]=json!([{"id":"existing-comment","revision":7}]);
            d["operations"]=json!([{"id":"uncertain","status":"unknown","payload":{"kept":true}}]);
            d["audit"]=json!([{"id":"history","kind":"fixture-evidence"}]);
            d["knowledge_entries"]=json!([]);d["knowledge_versions"]=json!([]);Ok(())
        }).await.unwrap();
        let before=db.read().await.unwrap();
        let foreign=crate::runtime_lifecycle_startup::Admission::fixture(crate::accounts::Profile::LikeAvto).identity().clone();
        assert!(initialize_fixture_database(&db,crate::accounts::Profile::LikeAvto,&foreign).await.is_err());
        assert_eq!(db.read().await.unwrap(),before);
        let identity=crate::runtime_lifecycle_startup::Admission::fixture(crate::accounts::Profile::BawRussia).identity().clone();
        initialize_fixture_database(&db,crate::accounts::Profile::BawRussia,&identity).await.unwrap();
        let mut after=db.read().await.unwrap();after.as_object_mut().unwrap().remove("runtimeLifecycle");
        assert_eq!(after,before);
    }
}
#[cfg(test)]mod fixed_owner_tests {
    use super::*;
    #[test]fn another_runtime_cannot_save_completion_even_when_running() {
        let identity=RuntimeIdentity{account:"LikeAvto".into(),runtime_id:"fixture-owner".into(),release_sha256:"a".repeat(64)};
        let mut d=serde_json::json!({"account":"LikeAvto","connectorBinding":{},"jobs":[],"operations":[],"approvals":[],"audit":[],"materials":[],"knowledge_entries":[],"knowledge_versions":[]});
        crate::runtime_lifecycle_startup::initialize_fixture(&mut d,&identity).unwrap();
        let before=d.clone();
        let other=Capture{identity:RuntimeIdentity{runtime_id:"obsolete-owner".into(),..identity},token:None};
        assert!(other.with(&mut d,|d|{d["mutation"]=serde_json::json!("forbidden");Ok(())}).is_err());
        assert_eq!(d,before);
    }
}
// Count synchronously before async spawn insertion; retain through finalization.
pub(crate) struct TaskCount(std::sync::Arc<std::sync::atomic::AtomicUsize>);
impl TaskCount {
    pub(crate) fn begin(counter:std::sync::Arc<std::sync::atomic::AtomicUsize>)->Self {
        counter.fetch_update(std::sync::atomic::Ordering::SeqCst,std::sync::atomic::Ordering::SeqCst,|count|count.checked_add(1)).expect("native task lifetime exhausted");
        Self(counter)
    }
}
impl Drop for TaskCount {fn drop(&mut self){let old=self.0.fetch_sub(1,std::sync::atomic::Ordering::SeqCst);assert!(old>0,"native task lifetime underflow");}}
tokio::task_local! {static JOB: String;}
pub(crate) async fn with_job<T>(job:String,future:impl std::future::Future<Output=T>)->T {JOB.scope(job,future).await}
pub(crate) fn current_job()->Option<String>{JOB.try_with(Clone::clone).ok()}
#[cfg(test)]mod task_count_tests {
    use super::*;
    #[test]fn lifetime_counts_before_poll_and_releases_on_drop(){
        let count=std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let work=TaskCount::begin(count.clone());assert_eq!(count.load(std::sync::atomic::Ordering::SeqCst),1);
        drop(work);assert_eq!(count.load(std::sync::atomic::Ordering::SeqCst),0);
    }
}
/// Establish task-local registry ownership only; never grants admission/work.
pub(crate) async fn native_context(
    axum::extract::State(app):axum::extract::State<App>,
    request:axum::http::Request<axum::body::Body>,
    next:axum::middleware::Next,
)->axum::response::Response {
    crate::runtime_owned_work::with_registry(app.lifecycle_work.clone(), next.run(request)).await
}
/// Actual startup recovery seam: preserve exact proven unstarted history rows.
/// Running rows and all ambiguous rows keep normal recovery/blocker behavior.
pub(crate) fn startup_recovery(d:&mut Value)->ApiResult<bool> {
    if !crate::runtime_lifecycle_backlog::recovery_allowed(d)? {return Err(conflict("Native startup recovery is not Running"));}
    let mut retained=false;
    for job in d["jobs"].as_array().ok_or_else(||conflict("Startup recovery jobs are malformed"))? {
        retained |= crate::runtime_lifecycle_backlog::preserved_at_recovery(d,job)?;
    }
    crate::recover(d)?;
    crate::assistant_action_review::recover_execution_receipts(d)?;
    if !retained {crate::media_queue::recover(d,&crate::now())?;}
    Ok(retained)
}

#[cfg(test)]
#[path="runtime_lifecycle_app_jobless_scope_tests.rs"]
mod jobless_scope_tests;

#[cfg(test)]
#[path="runtime_lifecycle_app_preparation_preview_tests.rs"]
mod preparation_preview_tests;
