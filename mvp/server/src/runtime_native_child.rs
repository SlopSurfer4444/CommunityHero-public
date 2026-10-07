//! Reusable native child lifetime. Caller cancellation retains a counted reaper
//! until observed process-tree absence; an unproved cleanup becomes Unresolved.
use crate::{ApiResult,internal};
use crate::runtime_owned_work::{Registry,Kind,Work};
use std::{process::ExitStatus,sync::Arc,time::Duration};
use tokio::process::{Child,Command};
#[cfg(unix)]
unsafe extern "C" {fn kill(pid:i32,signal:i32)->i32;}
#[cfg(unix)]
struct Group(u32);
#[cfg(unix)]
impl Group {
    async fn stop_and_wait(&self)->ApiResult<()> {
        let pid=i32::try_from(self.0).ok().filter(|p|*p>0).ok_or_else(||internal("Native group identity invalid"))?;
        let until=tokio::time::Instant::now()+Duration::from_secs(30);
        loop {
            if unsafe{kill(-pid,0)}==-1 {
                if std::io::Error::last_os_error().raw_os_error()==Some(3){return Ok(());}
                return Err(internal("Native group containment observation failed"));
            }
            unsafe{kill(-pid,9);}
            if tokio::time::Instant::now()>=until{return Err(internal("Native group containment unresolved"));}
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }
}
#[cfg(unix)]
impl Drop for Group {fn drop(&mut self){if let Ok(pid)=i32::try_from(self.0){if pid>0{unsafe{kill(-pid,9);}}}}}
pub(crate) struct OwnedChild {
    child:Option<Child>,work:Option<Work>,process_id:u32,
    #[cfg(windows)] tree:Arc<crate::ProcessTree>,
    #[cfg(unix)] tree:Arc<Group>,
}
/// Only native child exit plus observed tree absence can construct this value.
pub(crate) struct Cessation {process_id:u32}
impl Cessation {pub(crate) fn process_id(&self)->u32 {self.process_id}}
impl OwnedChild {
    pub(crate) async fn spawn(registry:&Registry,kind:Kind,command:&mut Command)->ApiResult<Self> {
        let work=registry.begin(kind)?;
        Self::spawn_admitted(work,command).await
    }
    /// A paid/native slot reserved BEFORE its writer admission stays counted
    /// across drain. It may finish its already committed one-shot admission.
    pub(crate) async fn spawn_admitted(work:Work,command:&mut Command)->ApiResult<Self> {
        Self::spawn_admitted_observed(work,command,&mut None).await
    }
    pub(crate) async fn spawn_admitted_observed(mut work:Work,command:&mut Command,os_error:&mut Option<i32>)->ApiResult<Self> {
        command.kill_on_drop(true);
        #[cfg(windows)] command.creation_flags(0x08000000);
        #[cfg(unix)] command.process_group(0);
        work.mark_started();
        let mut child=match command.spawn(){Ok(child)=>child,Err(error)=>{*os_error=error.raw_os_error();work.settled();return Err(internal("Native child not spawned"));}};
        #[cfg(windows)] let tree=match crate::ProcessTree::attach(&child){Ok(tree)=>Arc::new(tree),Err(error)=>{
            let _=child.kill().await;let _=child.wait().await;
            // No attached tree: descendant absence is NOT inferred from parent.
            drop(work);return Err(error);
        }};
        #[cfg(unix)] let tree=Arc::new(Group(child.id().ok_or_else(||internal("Native child PID unavailable"))?));
        #[cfg(not(any(windows,unix)))] {let _=child.kill().await;drop(work);return Err(internal("Native containment unsupported"));}
        #[cfg(any(windows,unix))] {
            let process_id=child.id().ok_or_else(||internal("Native child PID unavailable"))?;
            Ok(Self{child:Some(child),work:Some(work),process_id,tree})
        }
    }
    pub(crate) fn child_mut(&mut self)->&mut Child {self.child.as_mut().expect("owned child already settled")}
    pub(crate) async fn settle_observed(self)->ApiResult<(ExitStatus,Cessation)> {
        let process_id=self.process_id;
        let status=self.settle().await?;
        Ok((status,Cessation{process_id}))
    }
    pub(crate) async fn settle(mut self)->ApiResult<ExitStatus> {
        // Call after stdout completion/terminal error. Containment cleanup
        // terminates descendants and, if necessary, the failed parent.
        let result=tokio::time::timeout(Duration::from_secs(30),self.child.as_mut().ok_or_else(||internal("Native child already settled"))?.wait()).await
            .map_err(|_|internal("Native child exit unresolved"))?
            .map_err(|_|internal("Native child wait unresolved"))?;
        #[cfg(any(windows,unix))] self.tree.stop_and_wait().await?;
        self.child.take();
        if let Some(work)=self.work.take(){work.settled();}Ok(result)
    }
}
impl Drop for OwnedChild {
    fn drop(&mut self) {
        let Some(mut child)=self.child.take() else{return;};
        let Some(work)=self.work.take() else{return;};
        #[cfg(any(windows,unix))] let tree=self.tree.clone();
        // Runtime shutdown has no executor; keep unresolved, then kill-on-drop
        // and native tree Drop provide containment without claiming observation.
        if let Ok(handle)=tokio::runtime::Handle::try_current(){handle.spawn(async move {
            let _=child.start_kill();
            let waited=tokio::time::timeout(Duration::from_secs(30),child.wait()).await;
            // Reap the parent before group absence polling: an unreaped Unix
            // zombie must not make an otherwise contained group look active.
            #[cfg(any(windows,unix))] let contained=tree.stop_and_wait().await.is_ok();
            #[cfg(not(any(windows,unix)))] let contained=false;
            if contained&&matches!(waited,Ok(Ok(_))){work.settled();}
            // Otherwise Work::drop records exact unresolved ownership.
        });}
    }
}
#[cfg(all(test,any(windows,unix)))]
mod tests {
    use super::*;
    fn fixture(source:&str)->(tempfile::TempDir,Command) {
        let dir=tempfile::tempdir().unwrap();let path=dir.path().join("native-lifetime-fixture.mjs");
        std::fs::write(&path,source).unwrap();
        let node=std::env::var_os("COMMUNITYHERO_TEST_NODE").unwrap_or_else(||"node".into());
        let mut command=Command::new(node);command.arg(path).stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null());(dir,command)
    }
    #[tokio::test] async fn completed_child_counts_until_descendant_absence_is_observed() {
        let (_dir,mut command)=fixture("import {spawn} from 'node:child_process'; spawn(process.execPath,['-e','setInterval(()=>{},1000)'],{stdio:'ignore'}); setTimeout(()=>process.exit(0),100);");
        let registry=Registry::default();let child=OwnedChild::spawn(&registry,Kind::DirectBridge,&mut command).await.unwrap();
        assert_eq!(registry.snapshot().unwrap().active,1);registry.close().unwrap();
        assert!(child.settle().await.unwrap().success());let status=registry.snapshot().unwrap();assert_eq!(status.active,0);assert_eq!(status.unresolved,0);
    }
    #[tokio::test] async fn dropped_caller_keeps_counted_reaper_until_containment() {
        let (_dir,mut command)=fixture("setInterval(()=>{},1000);");let registry=Registry::default();
        let child=OwnedChild::spawn(&registry,Kind::CredentialWriter,&mut command).await.unwrap();let token=registry.close().unwrap();drop(child);
        tokio::time::timeout(Duration::from_secs(35),async {loop {let s=registry.snapshot().unwrap();if s.active==0 {assert_eq!(s.unresolved,0);break;}tokio::time::sleep(Duration::from_millis(10)).await;}}).await.unwrap();
        registry.resume(&token).unwrap();
    }
    #[tokio::test] async fn missing_executable_is_proven_not_spawned_and_closed_registry_never_spawns() {
        let registry=Registry::default();let mut command=Command::new("missing-native-lifecycle-fixture-executable");
        assert!(OwnedChild::spawn(&registry,Kind::MediaTool,&mut command).await.is_err());assert_eq!(registry.snapshot().unwrap().unresolved,0);
        registry.close().unwrap();assert!(OwnedChild::spawn(&registry,Kind::MediaTool,&mut command).await.is_err());assert_eq!(registry.snapshot().unwrap().active,0);
    }
    #[tokio::test] async fn cancellation_during_settle_keeps_owned_reaper() {
        let (_dir,mut command)=fixture("setInterval(()=>{},1000);");let registry=Registry::default();
        let child=OwnedChild::spawn(&registry,Kind::CredentialWriter,&mut command).await.unwrap();
        let token=registry.close().unwrap();let (started,ready)=tokio::sync::oneshot::channel();
        let settle=tokio::spawn(async move {let _=started.send(());child.settle().await});ready.await.unwrap();tokio::task::yield_now().await;
        settle.abort();let _=settle.await;
        tokio::time::timeout(Duration::from_secs(65),async {loop {let s=registry.snapshot().unwrap();if s.active==0 {assert_eq!(s.unresolved,0);break;}tokio::time::sleep(Duration::from_millis(10)).await;}}).await.unwrap();
        registry.resume(&token).unwrap();
    }
    #[tokio::test] async fn precommitted_native_ticket_settles_across_drain_without_new_admission() {
        let (_dir,mut command)=fixture("setTimeout(()=>process.exit(0),30);");let registry=Registry::default();
        let admitted=registry.begin(Kind::Preparation).unwrap();let token=registry.close().unwrap();
        assert!(registry.begin(Kind::Preparation).is_err());assert!(registry.resume(&token).is_err());
        let child=OwnedChild::spawn_admitted(admitted,&mut command).await.unwrap();child.settle().await.unwrap();registry.resume(&token).unwrap();
    }
}
