//! Private native handoff from durable pre-arm to exactly one bridge request.
//! Dropping this scope grants neither retirement nor replay authority.
use crate::*;
tokio::task_local! {static REQUEST:Value;}

pub(crate) fn require(args:&Value)->ApiResult<()> {
    REQUEST.try_with(|expected| {
        if args==expected {Ok(())}else{Err(conflict("Execute transport differs from its native pre-arm"))}
    }).unwrap_or_else(|_|Err(conflict("Execute transport requires its native durable dispatch permit")))
}
pub(crate) struct Execution {
    pub(crate) result:ApiResult<Value>,
    pub(crate) local_error:Option<ApiError>,
}
// Preserve the returned provider value across failures of dependent commits.
async fn persist_and_retire<P,PF,S,SF>(result:ApiResult<Value>,persist:P,retire:S)->Execution
where P:FnOnce(Value)->PF,PF:std::future::Future<Output=ApiResult<()>>,
      S:FnOnce()->SF,SF:std::future::Future<Output=ApiResult<()>> {
    let receipt=match &result {Ok(value)=>value.clone(),Err(error)=>json!({"error":error.1})};
    let local_error=match persist(receipt).await {Err(error)=>Some(error),Ok(())=>retire().await.err()};
    Execution{result,local_error}
}
pub(crate) async fn execute(app:&App,op:&Value,permit:dispatch_authority::Permit)->Execution {
    let identity=permit.identity().clone();
    if identity["operationId"]!=op["id"]||identity["attemptId"]!=op["attemptId"]||identity["phase"]!="dispatch_armed" {
        return Execution{result:Err(conflict("Execute transport permit belongs to another operation")),local_error:None};
    }
    let account=match operation_account(op){Ok(account)=>account,Err(error)=>return Execution{result:Err(error),local_error:None}};
    let request=json!({"account":account,"actions":[op["action"]]});
    let (result,observation)=REQUEST.scope(request.clone(),
        provider_session::observe_transport(app.bridge("execute",request))).await;
    let identity=permit.release(); // OLD conductor guard released before M.
    if let Some(observation)=observation {
        // Retiring a transport permit is a dependent write. Keep the exact
        // received result first, even if retirement storage subsequently fails.
        let witness=match connection_gate::TransportCessation::from_observation(op,&identity,observation) {
            Ok(witness)=>witness,Err(error)=>{
                dispatch_authority::hold_transport_failure(app);
                return Execution{result,local_error:Some(error)};
            }
        };
        let executed=persist_and_retire(result,
            |receipt|app.change_operation_evidence(op,storage::OperationEvidenceUpdate::ExecuteReceipt(receipt)),
            ||async {
                let _company=connection_gate::lock(app).await;
                app.change_connection_gate(connection_gate::Scope::Operation(op),|d|connection_gate::settle(d,&witness)).await.map(|_|())
            }).await;
        if executed.local_error.is_some(){dispatch_authority::hold_transport_failure(app);}
        return executed;
    }
    // If no positive witness was observed, keep the durable permit armed. The
    // caller still stores UNKNOWN and may independently read back this attempt.
    Execution{result,local_error:None}
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn local_commit_errors_never_replace_the_exact_transport_result() {
        let raw=json!({"results":[{"actionId":"a","itemId":"i","status":"sent","providerReplyId":"reply-1"}]});
        let written=std::sync::Mutex::new(None);
        let executed=persist_and_retire(Ok(raw.clone()),|receipt|async { *written.lock().unwrap()=Some(receipt);Ok(()) },
            ||async {Err(internal("retirement commit failed"))}).await;
        assert_eq!(executed.result.unwrap(),raw);assert!(executed.local_error.is_some());
        assert_eq!(*written.lock().unwrap(),Some(raw.clone()));
        let retired=std::sync::atomic::AtomicBool::new(false);
        let executed=persist_and_retire(Ok(raw.clone()),|_|async {Err(internal("receipt commit failed"))},
            ||async {retired.store(true,std::sync::atomic::Ordering::SeqCst);Ok(())}).await;
        assert_eq!(executed.result.unwrap(),raw);assert!(executed.local_error.is_some());
        assert!(!retired.load(std::sync::atomic::Ordering::SeqCst));
    }
}
