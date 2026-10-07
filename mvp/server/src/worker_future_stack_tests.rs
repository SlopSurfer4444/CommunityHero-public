use super::*;
use std::{future::Future,sync::atomic::{AtomicUsize,Ordering}};

struct Dropped(Arc<AtomicUsize>);
impl Drop for Dropped {fn drop(&mut self){self.0.fetch_add(1,Ordering::SeqCst);}}

#[tokio::test]
async fn actual_boxed_large_worker_preserves_scope_completion_and_cancellation() {
    for cancel in [false,true] {
        let (app,_temp)=crate::tests::test_app().await;
        let job=app.job("assistant","stack-regression").await.unwrap();
        let expected_job=job.clone();
        let payload=std::hint::black_box([7u8;32*1024]);
        let dropped=Arc::new(AtomicUsize::new(0));
        let capture=Dropped(dropped.clone());
        let completed=Arc::new(AtomicUsize::new(0));
        let completion=completed.clone();
        let (started,ready)=tokio::sync::oneshot::channel();
        let (release,waiting)=tokio::sync::oneshot::channel();
        app.spawn_with_completion(job.clone(),async move {
            assert_eq!(runtime_lifecycle_app::current_job(),Some(expected_job));
            assert!(conductor_authority::current_context().is_none());
            let registry=runtime_owned_work::current()?;
            let work=registry.begin(runtime_owned_work::Kind::Preparation)?;
            let _=started.send(());
            waiting.await.map_err(|_|internal("fixture release lost"))?;
            let checksum=std::hint::black_box(&payload).iter().map(|v|*v as usize).sum::<usize>();
            drop(capture);
            work.settled();
            Ok(json!({"checksum":checksum}))
        },move||{completion.fetch_add(1,Ordering::SeqCst);});
        tokio::time::timeout(Duration::from_secs(5),ready).await.unwrap().unwrap();
        assert_eq!(app.lifecycle_task_count.load(Ordering::SeqCst),1);
        assert_eq!(app.lifecycle_work.snapshot().unwrap().active,1);
        if cancel {
            let task=app.tasks.lock().await.get(&job).unwrap().clone();
            task.abort();
        } else {release.send(()).unwrap();}
        tokio::time::timeout(Duration::from_secs(5),async {
            loop {
                if !app.tasks.lock().await.contains_key(&job) && app.lifecycle_task_count.load(Ordering::SeqCst)==0 {break;}
                tokio::task::yield_now().await;
            }
        }).await.expect("registered worker did not finalize");
        let stored=app.db.read_job(&job).await.unwrap().unwrap();
        assert_eq!(stored["status"],if cancel {"failed"}else{"completed"});
        if !cancel {assert_eq!(stored["result"]["checksum"],32*1024*7);}
        assert_eq!(dropped.load(Ordering::SeqCst),1);
        assert_eq!(completed.load(Ordering::SeqCst),1);
        assert_eq!(app.lifecycle_work.snapshot().unwrap().active,0);
        assert_eq!(app.lifecycle_work.snapshot().unwrap().unresolved,0);
    }
}

// Type-only helper never invokes its factory. The connected test above exercises
// actual App spawning; this complementary bound includes the complete inner
// worker and its optional Context, running-job guard, registry and JOB wrappers.
fn future_bytes<A,F:Future>(_:impl FnOnce(A)->F)->usize {std::mem::size_of::<F>()}
fn execution_worker(worker:App,operations:Vec<Value>,parallelism:usize)->impl Future<Output=ApiResult<Value>>+Send {
    async move {let _dispatch_guard=worker.execution_gate.lock().await;dispatch_wave::run(worker.clone(),operations,parallelism).await}
}
fn inner_shape<F:Future<Output=ApiResult<Value>>+Send+'static>(worker:App,key:String,conductor_context:Option<conductor_authority::Context>,f:F)->impl Future<Output=worker_supervision::WorkerExit>+Send {
    async move {
        let context=match conductor_context {
            Some(ctx)=>Some(ctx),
            None=>match conductor_authority::context_for_job(&worker,&key).await {
                Ok(context)=>context,
                Err(error)=>return worker_supervision::WorkerExit::Completed(Err(error)),
            },
        };
        let f=runtime_owned_work::with_registry(worker.lifecycle_work.clone(),runtime_lifecycle_app::with_job(key.clone(),f));
        match context {
            Some(ctx)=>conductor_authority::with_context(ctx,worker_supervision::run_if_active(&worker,&key,f)).await,
            None=>worker_supervision::run_if_active(&worker,&key,f).await,
        }
    }
}
#[test]
fn pinned_worker_complete_inner_layout_is_bounded() {
    let bytes=future_bytes(|(app,key,context):(App,String,Option<conductor_authority::Context>)| {
        let f=Box::pin(execution_worker(app.clone(),vec![],1));
        inner_shape(app,key,context,f)
    });
    println!("boxed_spawn_inner_bytes={bytes}");
    assert!(bytes<=32*1024,"complete worker wrapper regrew to {bytes} bytes");
}