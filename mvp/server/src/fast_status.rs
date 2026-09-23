//! Independent status and priority-context lanes; neither infers closure from absence.
use super::*;
use futures_util::{stream,StreamExt};
const STATUS_SECONDS:i64=30;
const STATUS_LIMIT:usize=100;
const CONTEXT_LIMIT:usize=4;
fn clock(v:&Value)->Option<i64>{chrono::DateTime::parse_from_rfc3339(v.as_str()?).ok().map(|t|t.timestamp_millis())}
fn route(v:&Value)->ApiResult<String>{
    for field in ["objectId","itemId"]{if v[field].as_str().is_none_or(|s|s.is_empty()||s.len()>256||s.chars().any(char::is_control)){return Err(bad("Invalid source status identity"))}}
    Ok(json!([v["objectId"],v["itemId"]]).to_string())
}
fn token(item:&Value)->Value{json!([item["providerStatus"],item["statusObservedAt"],item["providerObservedAt"]])}
fn capture(d:&Value)->Value{
    let mut result=json!({});
    for item in list(d,"items"){if let Ok(key)=route(item){result[&key]=token(item);}}
    result
}
fn active(item:&Value)->bool{matches!(item["providerStatus"].as_str(),Some("new"|"inprogress"))||matches!(item["workflow"].as_str(),Some("attention"|"prepared"|"waiting"))}
fn targets(d:&Value,binding:&ConnectorBinding)->Vec<Value>{
    let mut items:Vec<_>=list(d,"items").iter().filter(|i|active(i)).filter_map(|i|bound_item(binding,i).ok()).collect();
    items.retain(|i|route(i).ok().is_some_and(|key|{
        let last=&d["sync"]["fastStatusTargets"][&key];last["error"].is_null()||clock(&last["attemptedAt"]).is_none_or(|t|t<=chrono::Utc::now().timestamp_millis()-300_000)
    }));
    items.sort_by_key(|i|(clock(&i["statusObservedAt"]).unwrap_or(0).max(route(i).ok().and_then(|key|clock(&d["sync"]["fastStatusTargets"][&key]["attemptedAt"])).unwrap_or(0)),i["id"].as_str().unwrap_or("").to_owned()));
    items.into_iter().take(STATUS_LIMIT).map(|i|json!({"objectId":i["objectId"],"itemId":i["itemId"]})).collect()
}
fn ensure_queue(d:&mut Value){if !d["sync"]["pendingContext"].is_object(){d["sync"]["pendingContext"]=json!({});}}
fn queue_context(d:&mut Value,observation:&Value,reason:&str,at:&str)->ApiResult<()> {
    let key=route(observation)?;ensure_queue(d);
    let old=&d["sync"]["pendingContext"][&key];
    if old["status"]=="queued"||old["status"]=="running" {return Ok(())}
    // A failed read gets a durable cooldown; repeated heads must not erase it.
    if old["status"]=="error" && clock(&old["retryAt"]).is_some_and(|until|until>clock(&json!(at)).unwrap_or(0)){return Ok(())}
    if old.is_null()&&d["sync"]["pendingContext"].as_object().unwrap().len()>=2000{return Ok(())}
    d["sync"]["pendingContext"][&key]=json!({"objectId":observation["objectId"],"itemId":observation["itemId"],"status":"queued","reason":reason,"queuedAt":at,"statusObservedAt":observation["observedAt"],"attempts":old["attempts"].as_u64().unwrap_or(0)});
    Ok(())
}

fn admit(d:&mut Value,binding:&ConnectorBinding,captured:&Value,response:&Value,expected:Option<&[Value]>,at:&str)->ApiResult<Value>{
    if active_binding(d)?!=*binding{return Err(conflict("Connector changed during fast synchronization"))}
    if response["kind"]!=if expected.is_some(){"exact-status-refresh"}else{"open-status-head"}{return Err(bad("Invalid fast status response"))}
    let observations=response["items"].as_array().filter(|v|v.len()<=800).ok_or_else(||bad("Invalid status observations"))?;
    let current_time=clock(&json!(at)).ok_or_else(||bad("Invalid status admission time"))?;
    let mut seen=std::collections::HashSet::new();let mut admitted=Vec::new();
    for observation in observations{
        let key=route(observation)?;
        if !seen.insert(key.clone()){return Err(bad("Duplicate status identity"))}
        if expected.is_some_and(|items|!items.iter().any(|v|v["objectId"]==observation["objectId"]&&v["itemId"]==observation["itemId"])){return Err(bad("Unrequested exact status identity"))}
        if !matches!(observation["status"].as_str(),Some("new"|"inprogress"|"closed"|"deleted")){return Err(bad("Unknown explicit provider status"))}
        let observed=clock(&observation["observedAt"]).filter(|t|*t<=current_time+60_000).ok_or_else(||bad("Invalid status observation time"))?;
        admitted.push((key,observation,observed));
    }
    let errors=response["errors"].as_array().filter(|v|v.len()<=100).ok_or_else(||bad("Invalid status errors"))?;
    if let Some(expected)=expected {
        for error in errors{
            let key=route(error)?;
            if !seen.insert(key)||!expected.iter().any(|v|v["objectId"]==error["objectId"]&&v["itemId"]==error["itemId"]){return Err(bad("Duplicate or foreign status error target"))}
        }
        if seen.len()!=expected.len(){return Err(bad("Exact status response omitted requested targets"))}
    }
    let mut changed=0;let mut stale=0;
    for (key,observation,observed) in admitted{
        if expected.is_some(){d["sync"]["fastStatusTargets"][&key]["error"]=Value::Null;d["sync"]["fastStatusTargets"][&key]["completedAt"]=json!(at);}
        let index=list(d,"items").iter().position(|i|i["objectId"]==observation["objectId"]&&i["itemId"]==observation["itemId"]);
        let Some(index)=index else{queue_context(d,observation,"new_comment",at)?;continue;};
        let item=&list(d,"items")[index];
        bound_item(binding,item)?;
        let latest=clock(&item["statusObservedAt"]).unwrap_or(0).max(clock(&item["providerObservedAt"]).unwrap_or(0));
        if captured[&key]!=token(item)||observed<latest||(observed==latest&&item["providerStatus"]!=observation["status"]){stale+=1;continue;}
        let status_changed=item["providerStatus"]!=observation["status"];
        let workflow_mismatch=(observation["status"]=="closed"&&!matches!(item["workflow"].as_str(),Some("closed"|"waiting")))
            ||(observation["status"]=="deleted"&&item["workflow"]!="deleted");
        let needs_context=status_changed || matches!(observation["status"].as_str(),Some("new"|"inprogress"))
            && clock(&item["contextObservedAt"]).or_else(||clock(&item["providerObservedAt"])).is_none_or(|t|t<=current_time-480_000);
        let item=&mut list_mut(d,"items")[index];
        item["statusObservedAt"]=observation["observedAt"].clone();
        if status_changed||workflow_mismatch {
            let previous=item["providerStatus"].clone();item["providerStatus"]=observation["status"].clone();
            match observation["status"].as_str().unwrap(){
                "deleted"=>item["workflow"]=json!("deleted"),
                "closed" if item["workflow"]!="waiting"=>item["workflow"]=json!("closed"),
                "new"|"inprogress" if matches!(previous.as_str(),Some("closed"|"deleted"))&&matches!(item["workflow"].as_str(),Some("closed"|"deleted"))=>item["workflow"]=json!("attention"),
                _=>{}
            }
            bump(item);changed+=1;
        }
        if needs_context{queue_context(d,observation,if status_changed{"status_changed"}else{"context_refresh"},at)?;}
    }
    if let Some(expected)=expected {
        for error in errors{
            let key=route(error)?;
            if !expected.iter().any(|v|v["objectId"]==error["objectId"]&&v["itemId"]==error["itemId"]){return Err(bad("Foreign status error target"))}
            let code=error["code"].as_str().unwrap_or("SOURCE_STATUS_ERROR").chars().filter(|c|c.is_ascii_alphanumeric()||*c=='_').take(80).collect::<String>();
            d["sync"]["fastStatusTargets"][&key]["error"]=json!(code);d["sync"]["fastStatusTargets"][&key]["completedAt"]=json!(at);
        }
    }
    let coverage:Vec<_>=response["coverage"].as_array().into_iter().flatten().take(8).map(|c|json!({"objectId":c["objectId"],"hasMore":c["hasMore"],"count":c["count"],"observedAt":c["observedAt"]})).collect();
    Ok(json!({"kind":response["kind"],"observed":observations.len(),"changed":changed,"staleResponses":stale,"errors":errors.len(),"coverage":coverage,"hasMore":response["hasMore"],"coverageIsSnapshot":false}))
}

fn busy(d:&Value,kind:&str)->bool{list(d,"jobs").iter().any(|j|j["kind"]==kind&&matches!(j["status"].as_str(),Some("running"|"queued")))}
fn status_due(d:&Value,at:i64)->bool{!busy(d,"status_sync")&&clock(&d["sync"]["fastStatus"]["nextRunAt"]).is_none_or(|next|next<=at)}
pub(super) async fn tick(app:&App,state:&Value)->ApiResult<()> {
    let at=chrono::Utc::now().timestamp_millis();
    if status_due(state,at){
        let claimed=app.change_source_claim(|d|{
            if !status_due(d,at){return Ok(None)}
            let binding=active_binding(d)?;let account=bridge_account(&binding)?.to_owned();
            let request=targets(d,&binding);let captured=capture(d);
            for target in &request{let key=route(target)?;d["sync"]["fastStatusTargets"][&key]=json!({"attemptedAt":now(),"error":null});}
            let job=new_job(d,"status_sync","")?;row_mut(d,"jobs",&job)?["purpose"]=json!("fast_source_status");
            d["sync"]["fastStatus"]["state"]=json!("running");
            Ok(Some((job,binding,account,request,captured)))
        }).await?;
        if let Some((job,binding,account,request,captured))=claimed{
            let worker=app.clone();app.spawn(job,async move{
                // Each adapter limits its provider requests. Keep the head and
                // exact batch sequential so their concurrency limits add no burst.
                let mut successes=Vec::new();let mut errors=0;
                if !request.is_empty(){
                    match worker.bridge("status",json!({"account":account,"targets":request})).await{
                        Ok(response)=>match worker.change_status(response["items"].as_array().map(Vec::as_slice).unwrap_or(&[]),|d|admit(d,&binding,&captured,&response,Some(&request),&now())).await{Ok(v)=>{if v["errors"].as_u64().unwrap_or(0)>0{errors+=1}if v["observed"].as_u64().unwrap_or(0)>0||v["errors"]==0{successes.push(v)}},Err(_)=>errors+=1},
                        Err(_)=>errors+=1,
                    }
                }
                // Capture afresh: the exact result is newer state than the first
                // capture; reusing its token would discard every subsequent head.
                let head_capture={let fresh=worker.db.read_source_status().await?;capture(&fresh)};
                match worker.bridge("head",json!({"account":account})).await{
                    Ok(response)=>match worker.change_status(response["items"].as_array().map(Vec::as_slice).unwrap_or(&[]),|d|admit(d,&binding,&head_capture,&response,None,&now())).await{Ok(v)=>{if v["errors"].as_u64().unwrap_or(0)>0{errors+=1}if v["observed"].as_u64().unwrap_or(0)>0||v["errors"]==0{successes.push(v)}},Err(_)=>errors+=1},
                    Err(_)=>errors+=1,
                }
                worker.change_schedule(|d|{
                    if active_binding(d)?!=binding{return Err(conflict("Connector changed during fast synchronization"))}
                    let consecutive=if errors==0{0}else{d["sync"]["fastStatus"]["consecutiveErrors"].as_u64().unwrap_or(0)+1};
                    let delay=if successes.is_empty(){(STATUS_SECONDS*(1_i64<<consecutive.min(4))).min(300)}else{STATUS_SECONDS};
                    d["sync"]["fastStatus"]=json!({"state":if errors==0{"monitoring"}else{"partial"},"lastFinishedAt":now(),"nextRunAt":(chrono::Utc::now()+chrono::Duration::seconds(delay)).to_rfc3339(),"consecutiveErrors":consecutive,"results":successes,"batchErrors":errors});Ok(())
                }).await?;
                if successes.is_empty(){Err(internal("Fast source status reads failed"))}else{Ok(json!({"refreshed":true,"results":successes,"batchErrors":errors}))}
            });
        }
    }
    context_tick(app,state).await
}

fn context_candidates(d:&Value,at:i64)->Vec<(String,Value)>{
    let mut pending:Vec<_>=d["sync"]["pendingContext"].as_object().into_iter().flatten().filter(|(_,v)|{
        let abandoned=v["status"]=="running"&&v["jobId"].as_str().and_then(|id|row(d,"jobs",id).ok()).is_none_or(|j|!matches!(j["status"].as_str(),Some("running"|"queued")));
        (v["status"]=="queued"||v["status"]=="error"||abandoned)&&clock(&v["retryAt"]).is_none_or(|t|t<=at)
    }).map(|(k,v)|(k.clone(),v.clone())).collect();
    pending.sort_by_key(|(key,v)|(clock(&v["queuedAt"]).unwrap_or(0),key.clone()));pending.truncate(CONTEXT_LIMIT);pending
}
async fn context_tick(app:&App,state:&Value)->ApiResult<()> {
    if busy(state,"context_sync")||context_candidates(state,chrono::Utc::now().timestamp_millis()).is_empty(){return Ok(())}
    let claimed=app.change_schedule(|d|{
        if busy(d,"context_sync"){return Ok(None)}
        let pending=context_candidates(d,chrono::Utc::now().timestamp_millis());if pending.is_empty(){return Ok(None)}
        let binding=active_binding(d)?;let account=bridge_account(&binding)?.to_owned();let job=new_job(d,"context_sync","")?;
        row_mut(d,"jobs",&job)?["purpose"]=json!("priority_source_context");
        for (key,_) in &pending{let v=&mut d["sync"]["pendingContext"][key];v["status"]=json!("running");v["jobId"]=json!(job);v["attemptedAt"]=json!(now());v["attempts"]=json!(v["attempts"].as_u64().unwrap_or(0)+1);}
        Ok(Some((job,binding,account,pending)))
    }).await?;
    if let Some((job,binding,account,pending))=claimed{
        let worker=app.clone();let run=job.clone();app.spawn(job,async move{
            let results=stream::iter(pending.into_iter().map(|(key,target)|{
                let worker=worker.clone();let account=account.clone();
                async move{let result=worker.bridge("context",json!({"account":account,"objectId":target["objectId"],"itemId":target["itemId"],"snapshot":true})).await;(key,target,result)}
            })).buffer_unordered(2);
            futures_util::pin_mut!(results);
            let mut completed=0;let mut failed=0;
            while let Some((key,target,result))=results.next().await{
                let result=match result {Ok(snapshot)=>worker.change(|d|admit_context(d,&binding,&run,&key,&target,&snapshot)).await,Err(error)=>Err(error)};
                if result.is_ok(){completed+=1;}else{failed+=1;worker.change_schedule(|d|{
                    if active_binding(d)?!=binding{return Err(conflict("Connector changed during context synchronization"))}
                    let record=&mut d["sync"]["pendingContext"][&key];if record["jobId"]!=run{return Ok(())}
                    record["status"]=json!("error");record["error"]=json!("Source context read failed");record["completedAt"]=json!(now());
                    let delay=(30*(1_i64<<record["attempts"].as_u64().unwrap_or(1).min(5))).min(900);
                    record["retryAt"]=json!((chrono::Utc::now()+chrono::Duration::seconds(delay)).to_rfc3339());Ok(())
                }).await?;}
            }
            Ok(json!({"completed":completed,"failed":failed}))
        });
    }
    Ok(())
}
fn admit_context(d:&mut Value,binding:&ConnectorBinding,job:&str,key:&str,target:&Value,snapshot:&Value)->ApiResult<()> {
    if active_binding(d)?!=*binding||d["sync"]["pendingContext"][key]["jobId"]!=job{return Err(conflict("Priority context was superseded"))}
    let items=snapshot["items"].as_array().filter(|v|v.len()==1).ok_or_else(||bad("Exact context target missing"))?;
    let item=bound_item(binding,&items[0])?;
    if item["objectId"]!=target["objectId"]||item["itemId"]!=target["itemId"]{return Err(bad("Exact context returned another target"))}
    let mut bound=snapshot.clone();bound["items"]=json!([item]);
    let ordered=crate::snapshot_order::ordered(d,&bound)?;
    if ordered["items"].as_array().is_none_or(|v|v.len()!=1){return Err(conflict("Priority context arrived after newer source state"))}
    merge_snapshot(d,&ordered)?;
    // Remove only this attempt; another status change can queue fresh context
    // again on the next fast observation without accumulating completed entries.
    d["sync"]["pendingContext"].as_object_mut().unwrap().remove(key);
    Ok(())
}

#[cfg(test)]
mod tests{
    use super::*;
    const AT:&str="2026-09-22T12:00:00Z";
    fn fixture()->Value{let mut d=empty();d["items"]=json!([{"id":"item-11391-42","objectId":"11391","itemId":"42","postKey":"11391:p","conversationKey":"11391:t","providerStatus":"new","workflow":"prepared","draft":"Human draft","draftEdited":true,"draftOrigin":{"proposalId":"saved"},"revision":3,"providerObservedAt":"2026-09-22T10:00:00Z"}]);d}
    fn observation(status:&str)->Value{json!({"objectId":"11391","itemId":"42","status":status,"observedAt":AT})}
    fn reply(items:Value)->Value{json!({"kind":"exact-status-refresh","observedAt":AT,"items":items,"errors":[]})}
    fn apply(d:&mut Value,status:&str)->Value{let b=active_binding(d).unwrap();let c=capture(d);admit(d,&b,&c,&reply(json!([observation(status)])),Some(&[json!({"objectId":"11391","itemId":"42"})]),AT).unwrap()}
    #[test]
    fn explicit_close_updates_work_and_preserves_draft_without_fake_context_freshness(){
        let mut d=fixture();let before=d["items"][0].clone();apply(&mut d,"closed");let item=&d["items"][0];
        assert_eq!(item["workflow"],"closed");assert_eq!(item["providerStatus"],"closed");assert_eq!(item["revision"],4);
        for field in ["draft","draftEdited","draftOrigin","providerObservedAt"]{assert_eq!(item[field],before[field]);}
        assert_eq!(item["statusObservedAt"],AT);assert!(item["contextObservedAt"].is_null());
        assert_eq!(d["sync"]["pendingContext"].as_object().unwrap().len(),1);
        let mut waiting=fixture();waiting["items"][0]["workflow"]=json!("waiting");waiting["items"][0]["waitingReason"]=json!("Owner reminder");apply(&mut waiting,"closed");
        assert_eq!(waiting["items"][0]["workflow"],"waiting");assert_eq!(waiting["items"][0]["waitingReason"],"Owner reminder");
    }
    #[test]
    fn missing_or_failed_reads_never_close_and_unknown_head_queues_context_only(){
        let mut d=fixture();let b=active_binding(&d).unwrap();let captured=capture(&d);let before=d["items"].clone();
        let response=json!({"kind":"open-status-head","items":[],"errors":[],"observedAt":AT});admit(&mut d,&b,&captured,&response,None,AT).unwrap();assert_eq!(d["items"],before);
        let error=json!({"kind":"exact-status-refresh","items":[],"errors":[{"objectId":"11391","itemId":"42","code":"NOT_FOUND"}]});
        admit(&mut d,&b,&captured,&error,Some(&[json!({"objectId":"11391","itemId":"42"})]),AT).unwrap();assert_eq!(d["items"],before);
        let head=json!({"kind":"open-status-head","items":[{"objectId":"11391","itemId":"73","status":"new","observedAt":AT}],"errors":[]});
        admit(&mut d,&b,&captured,&head,None,AT).unwrap();assert_eq!(d["items"],before);assert_eq!(d["sync"]["pendingContext"].as_object().unwrap().len(),1);
    }
    #[test]
    fn late_status_cannot_undo_a_more_recent_closure_or_context_read(){
        let mut d=fixture();let b=active_binding(&d).unwrap();let old=capture(&d);apply(&mut d,"closed");
        let mut older=observation("new");older["observedAt"]=json!("2026-09-22T11:59:59Z");
        let result=admit(&mut d,&b,&old,&reply(json!([older])),Some(&[json!({"objectId":"11391","itemId":"42"})]),AT).unwrap();assert_eq!(result["staleResponses"],1);assert_eq!(d["items"][0]["workflow"],"closed");
        let mut d=fixture();let old=capture(&d);d["items"][0]["providerObservedAt"]=json!(AT);let result=admit(&mut d,&b,&old,&reply(json!([observation("closed")])),Some(&[json!({"objectId":"11391","itemId":"42"})]),AT).unwrap();assert_eq!(result["staleResponses"],1);assert_eq!(d["items"][0]["providerStatus"],"new");
    }
    #[test]
    fn rejects_foreign_targets_and_binding_without_touching_items(){
        let mut d=fixture();let b=active_binding(&d).unwrap();let c=capture(&d);let before=d.clone();
        let mut wrong=observation("closed");wrong["itemId"]=json!("foreign");assert!(admit(&mut d,&b,&c,&reply(json!([wrong])),Some(&[json!({"objectId":"11391","itemId":"42"})]),AT).is_err());assert_eq!(d,before);
        let mut other=b.clone();other.revision+=1;assert!(admit(&mut d,&other,&c,&reply(json!([observation("closed")])),None,AT).is_err());assert_eq!(d,before);
    }
    #[test]
    fn exact_response_must_partition_targets_without_silent_omissions_or_duplicate_errors(){
        let targets=[json!({"objectId":"11391","itemId":"42"})];
        for mode in ["omitted","both","duplicate_error"]{
            let mut d=fixture();let before=d.clone();let binding=active_binding(&d).unwrap();let captured=capture(&d);
            let error=json!({"objectId":"11391","itemId":"42","code":"NOT_FOUND"});
            let response=match mode{"omitted"=>reply(json!([])),"both"=>json!({"kind":"exact-status-refresh","items":[observation("closed")],"errors":[error]}),_=>json!({"kind":"exact-status-refresh","items":[],"errors":[error.clone(),error]})};
            assert!(admit(&mut d,&binding,&captured,&response,Some(&targets),AT).is_err(),"{mode}");assert_eq!(d,before);
        }
    }
    #[test]
    fn late_conflicting_context_keeps_pending_retry_instead_of_claiming_it_completed(){
        let mut d=fixture();let mut old=d["items"][0].clone();apply(&mut d,"closed");
        let key=route(&observation("closed")).unwrap();let target=d["sync"]["pendingContext"][&key].clone();
        d["sync"]["pendingContext"][&key]["jobId"]=json!("context-job");d["sync"]["pendingContext"][&key]["status"]=json!("running");
        old["providerStatusObservedAt"]=json!("2026-09-22T11:59:00Z");old["contextObservedAt"]=json!("2026-09-22T11:59:00Z");
        let binding=active_binding(&d).unwrap();let before=d.clone();assert!(admit_context(&mut d,&binding,"context-job",&key,&target,&json!({"items":[old]})).is_err());
        assert_eq!(d,before);assert_eq!(d["items"][0]["providerStatus"],"closed");
    }
    #[test]
    fn stale_context_and_interrupted_priority_reads_resume_with_backoff(){
        let mut d=fixture();apply(&mut d,"new");let key=route(&observation("new")).unwrap();let at=clock(&json!(AT)).unwrap();assert_eq!(context_candidates(&d,at).len(),1);
        d["sync"]["pendingContext"][&key]["status"]=json!("running");d["sync"]["pendingContext"][&key]["jobId"]=json!("interrupted");
        d["jobs"]=json!([{"id":"interrupted","kind":"context_sync","status":"interrupted"}]);assert_eq!(context_candidates(&d,at).len(),1);
        d["sync"]["pendingContext"][&key]["retryAt"]=json!("2026-09-22T12:01:00Z");assert!(context_candidates(&d,at).is_empty());
        let before=d["sync"]["pendingContext"][&key].clone();d["sync"]["pendingContext"][&key]["status"]=json!("error");queue_context(&mut d,&observation("new"),"context_refresh",AT).unwrap();assert_eq!(d["sync"]["pendingContext"][&key]["retryAt"],before["retryAt"]);
    }
    #[tokio::test]
    async fn projected_schedule_preserves_retry_and_abandoned_job_decisions(){
        let temp=tempfile::tempdir().unwrap();
        let db=Database::Sqlite(open_db(&temp.path().join("workspace.sqlite")).await.unwrap());
        db.change(|d|{
            d["sync"]["fastStatus"]=json!({"nextRunAt":AT});
            d["jobs"]=json!([
                {"id":"active","kind":"context_sync","status":"running"},
                {"id":"old","kind":"status_sync","status":"completed","largeEvidence":"excluded"}
            ]);
            d["sync"]["pendingContext"]=json!({
                "queued":{"status":"queued","queuedAt":AT},
                "active":{"status":"running","jobId":"active","queuedAt":AT},
                "abandoned":{"status":"running","jobId":"old","queuedAt":AT},
                "failed":{"status":"error","retryAt":"2026-09-22T12:01:00Z","queuedAt":AT},
                "done":{"status":"completed","queuedAt":AT}
            });Ok(())
        }).await.unwrap();
        let full=db.read().await.unwrap();let compact=db.read_schedule().await.unwrap();
        for at in ["2026-09-22T11:59:59Z",AT,"2026-09-22T12:01:00Z"]{
            let at=clock(&json!(at)).unwrap();
            assert_eq!(status_due(&full,at),status_due(&compact,at));
            assert_eq!(busy(&full,"context_sync"),busy(&compact,"context_sync"));
            let keys=|d:&Value|context_candidates(d,at).into_iter().map(|(k,_)|k).collect::<Vec<_>>();
            assert_eq!(keys(&full),keys(&compact));
        }
        db.close().await;
    }
    #[tokio::test]
    async fn fast_status_and_new_comment_context_progress_while_archive_is_stalled(){
        let temp=tempfile::tempdir().unwrap();let db=open_db(&temp.path().join("workspace.sqlite")).await.unwrap();let (events,_)=broadcast::channel(8);
        let bridge=temp.path().join("fast-fixture.mjs");std::fs::write(&bridge,r#"let raw='';for await(const c of process.stdin)raw+=c;const r=JSON.parse(raw),at=new Date().toISOString();let result;
if(r.operation==='status')result={kind:'exact-status-refresh',observedAt:at,items:r.targets.map(t=>({...t,status:'closed',observedAt:at})),errors:[]};
else if(r.operation==='head')result={kind:'open-status-head',observedAt:at,items:[{objectId:'11391',itemId:'73',status:'new',observedAt:at}],errors:[]};
else if(r.operation==='context')result={items:[{id:'item-11391-'+r.itemId,objectId:r.objectId,itemId:r.itemId,postKey:'11391:p',conversationKey:'11391:t',providerStatus:r.itemId==='42'?'closed':'new',providerStatusObservedAt:at,contextObservedAt:at,text:'Source comment'}]};
else throw Error('Unexpected provider/model call');process.stdout.write(JSON.stringify({ok:true,result}));"#).unwrap();
        let app=App{account:crate::accounts::Profile::LikeAvto,db:Database::Sqlite(db),gate:Arc::new(crate::writer_gate::WriterGate::default()),execution_gate:Arc::new(Mutex::new(())),assistant_gate:Arc::new(Mutex::new(())),assistant_chat_gate:Arc::new(Mutex::new(())),events,csrf:id(),auth:None,public_origin:None,external_writes:false,port:0,data:temp.path().to_owned(),bridge,node:PathBuf::from("C:/Users/hello/AppData/Local/Microsoft/WinGet/Packages/OpenJS.NodeJS.LTS_Microsoft.Winget.Source_8wekyb3d8bbwe/node-v24.15.0-win-x64/node.exe"),tasks:Arc::new(Mutex::new(HashMap::new())),bootstrap_cache:Arc::new(bootstrap_cache::Cache::default())};
        app.change(|d|{let snapshot=fixture();d["items"]=snapshot["items"].clone();d["jobs"]=json!([{"id":"slow-archive","kind":"sync","status":"running"}]);Ok(())}).await.unwrap();
        tokio::time::timeout(Duration::from_secs(15),async{loop{
            crate::sync_scan::tick(&app).await.unwrap();let d=app.read().await.unwrap();
            if list(&d,"items").iter().any(|i|i["itemId"]=="73")&&!busy(&d,"context_sync"){break}
            tokio::time::sleep(Duration::from_millis(30)).await;
        }}).await.unwrap();
        let d=app.read().await.unwrap();assert_eq!(row(&d,"jobs","slow-archive").unwrap()["status"],"running");
        let old=row(&d,"items","item-11391-42").unwrap();assert_eq!(old["workflow"],"closed");assert_eq!(old["draft"],"Human draft");
        assert_eq!(row(&d,"items","item-11391-73").unwrap()["providerStatus"],"new");assert!(list(&d,"operations").is_empty());app.db.close().await;
    }
}
