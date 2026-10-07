//! Process-local lifetime registry for non-pooled native/model/media/credential
//! work. A dropped future is unresolved until actual native containment proves
//! its owned work ceased. Registration is not durable dispatch authority.
use crate::{ApiResult, conflict};
use std::{collections::BTreeMap,sync::{Arc,Mutex}};
#[derive(Clone,Copy,Debug,PartialEq,Eq)]
pub(crate) enum Kind { DirectBridge, MediaTool, Preparation, CredentialWriter }
#[derive(Clone,Debug,PartialEq,Eq)]
pub(crate) struct DrainToken(u64);
#[derive(Default)]
struct State { epoch:u64, closed:bool, next:u64, active:BTreeMap<u64,Kind>, unresolved:BTreeMap<u64,Kind> }
#[derive(Clone,Default)]
pub(crate) struct Registry(Arc<Mutex<State>>);
#[derive(Clone,Debug)]
pub(crate) struct Snapshot { pub active:usize,pub unresolved:usize,pub credential_writers:usize,pub closed:bool }
pub(crate) struct Work { registry:Registry,id:u64,finished:bool,started:bool }
tokio::task_local! { static CURRENT: Registry; }
tokio::task_local! { static ADMITTED: (Registry,u64); }
pub(crate) async fn with_registry<T>(registry:Registry,future:impl std::future::Future<Output=T>)->T {
    CURRENT.scope(registry,future).await
}
pub(crate) fn current()->ApiResult<Registry> {
    CURRENT.try_with(Clone::clone).map_err(|_|conflict("Native work has no runtime owner registry; not dispatched"))
}
/// A counted writer-admitted stage may finish its native substeps after close.
/// The scope is minted only from an owned Work; child admission verifies its
/// still-active exact parent in the same registry, never a caller boolean.
pub(crate) async fn with_admitted<T>(work:Work,future:impl std::future::Future<Output=T>)->T {
    let binding=(work.registry.clone(),work.id);
    let result=ADMITTED.scope(binding,future).await;
    drop(work);result
}
impl Registry {
    pub(crate) fn begin(&self,kind:Kind)->ApiResult<Work> {
        let mut s=self.0.lock().map_err(|_|conflict("Native owned-work registry unavailable"))?;
        if s.closed {return Err(conflict("Runtime drain rejects new native work; not dispatched"));}
        s.next=s.next.checked_add(1).ok_or_else(||conflict("Native work identity exhausted"))?;
        let id=s.next;s.active.insert(id,kind);Ok(Work{registry:self.clone(),id,finished:false,started:false})
    }
    pub(crate) fn begin_child(&self,kind:Kind)->ApiResult<Work> {
        let parent=ADMITTED.try_with(Clone::clone).ok();
        let mut s=self.0.lock().map_err(|_|conflict("Native owned-work registry unavailable"))?;
        if s.closed && !parent.as_ref().is_some_and(|(r,id)|Arc::ptr_eq(&self.0,&r.0)&&s.active.contains_key(id)) {
            return Err(conflict("Runtime drain rejects unowned native substep; not dispatched"));
        }
        s.next=s.next.checked_add(1).ok_or_else(||conflict("Native work identity exhausted"))?;
        let id=s.next;s.active.insert(id,kind);Ok(Work{registry:self.clone(),id,finished:false,started:false})
    }
    pub(crate) fn close(&self)->ApiResult<DrainToken> {
        let mut s=self.0.lock().map_err(|_|conflict("Native owned-work registry unavailable"))?;
        if !s.closed {s.epoch=s.epoch.checked_add(1).ok_or_else(||conflict("Native work epoch exhausted"))?;s.closed=true;}
        Ok(DrainToken(s.epoch))
    }
    pub(crate) fn snapshot(&self)->ApiResult<Snapshot> {
        let s=self.0.lock().map_err(|_|conflict("Native owned-work registry unavailable"))?;
        Ok(Snapshot{active:s.active.len(),unresolved:s.unresolved.len(),credential_writers:s.active.values().chain(s.unresolved.values()).filter(|v|**v==Kind::CredentialWriter).count(),closed:s.closed})
    }
    /// Actual complete native tree observation, never job status or timeout,
    /// supplies the exact unresolved native work ID. Remote UNKNOWN remains in
    /// the durable ledger even when its physical process has ceased.
    pub(crate) fn observe_cessation(&self,token:&DrainToken,id:u64)->ApiResult<()> {
        let mut s=self.0.lock().map_err(|_|conflict("Native owned-work registry unavailable"))?;
        if !s.closed||s.epoch!=token.0||s.unresolved.remove(&id).is_none(){return Err(conflict("Native cessation observation identity mismatch"));}Ok(())
    }
    pub(crate) fn unresolved_ids(&self)->ApiResult<Vec<u64>> {
        Ok(self.0.lock().map_err(|_|conflict("Native owned-work registry unavailable"))?.unresolved.keys().copied().collect())
    }
    pub(crate) fn resume(&self,token:&DrainToken)->ApiResult<()> {
        let mut s=self.0.lock().map_err(|_|conflict("Native owned-work registry unavailable"))?;
        if !s.closed||s.epoch!=token.0||!s.active.is_empty()||!s.unresolved.is_empty(){return Err(conflict("Native work unresolved; resume blocked"));}
        s.closed=false;Ok(())
    }
}
impl Work {
    pub(crate) fn id(&self)->u64 {self.id}
    pub(crate) fn mark_started(&mut self){self.started=true;}
    /// Call ONLY after success/failure before spawn or actual tree containment.
    /// Success envelope alone is insufficient for media/credential descendants.
    pub(crate) fn settled(mut self) {
        if let Ok(mut s)=self.registry.0.lock(){s.active.remove(&self.id);self.finished=true;}
    }
}
impl Drop for Work {
    fn drop(&mut self) {if !self.finished {if let Ok(mut s)=self.registry.0.lock(){if let Some(kind)=s.active.remove(&self.id){if self.started{s.unresolved.insert(self.id,kind);}}}}}
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn canceled_native_write_remains_unresolved_and_blocks_resume() {
        let r=Registry::default();let mut work=r.begin(Kind::CredentialWriter).unwrap();work.mark_started();let id=work.id();let token=r.close().unwrap();drop(work);
        assert!(r.begin(Kind::MediaTool).is_err());assert_eq!(r.snapshot().unwrap().credential_writers,1);assert!(r.resume(&token).is_err());
        r.observe_cessation(&token,id).unwrap();r.resume(&token).unwrap();assert!(r.begin(Kind::Preparation).is_ok());
    }
    #[test] fn admitted_work_settles_after_close_and_epochs_prevent_stale_reopen() {
        let r=Registry::default();let work=r.begin(Kind::DirectBridge).unwrap();let first=r.close().unwrap();assert_eq!(r.close().unwrap(),first);assert!(r.resume(&first).is_err());work.settled();r.resume(&first).unwrap();let second=r.close().unwrap();assert_ne!(first,second);assert!(r.resume(&first).is_err());r.resume(&second).unwrap();
    }
    #[tokio::test] async fn closed_children_require_live_exact_owned_stage_scope() {
        let r=Registry::default();let stage=r.begin(Kind::MediaTool).unwrap();let token=r.close().unwrap();
        assert!(r.begin_child(Kind::MediaTool).is_err());
        let foreign=Registry::default();foreign.close().unwrap();
        with_admitted(stage,async {assert!(foreign.begin_child(Kind::MediaTool).is_err());let child=r.begin_child(Kind::MediaTool).unwrap();assert_eq!(r.snapshot().unwrap().active,2);child.settled();}).await;
        assert!(r.begin_child(Kind::MediaTool).is_err());r.resume(&token).unwrap();
    }
}
