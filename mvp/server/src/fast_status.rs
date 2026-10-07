//! Independent status and priority-context lanes; neither infers closure from absence.
use super::*;
use futures_util::{stream,StreamExt,FutureExt};
const STATUS_SECONDS:i64=30;
const STATUS_LIMIT:usize=100;
const CONTEXT_LIMIT:usize=4;
// The adapter reads at most two 100-row heads for each configured object.
const HEAD_OBJECT_LIMIT:usize=12;
const HEAD_ROWS_PER_OBJECT:usize=200;
const HEAD_LIMIT:usize=HEAD_OBJECT_LIMIT*HEAD_ROWS_PER_OBJECT;
const CONTEXT_QUEUE_LIMIT:usize=2000;
const CONTEXT_STORAGE_LIMIT:usize=CONTEXT_QUEUE_LIMIT+HEAD_LIMIT;
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
fn queue_counts(d:&Value)->(usize,usize,usize){
    let records=d["sync"]["pendingContext"].as_object();
    let active=records.into_iter().flatten().filter(|(_,v)|matches!(v["status"].as_str(),Some("queued"|"running"))).count();
    let deferred=records.into_iter().flatten().filter(|(_,v)|v["status"]=="deferred").count();
    (active,deferred,records.map_or(0,|v|v.len()))
}
fn matches_binding(v:&Value,binding:&Value)->bool{v["connectorBinding"].is_null()||v["connectorBinding"]==*binding}
fn unknown_count(d:&Value)->usize{d["sync"]["pendingContext"].as_object().into_iter().flatten().filter(|(_,v)|v["canonicalItemId"].is_null()).count()}
fn active_count(d:&Value,binding:&Value)->usize{d["sync"]["pendingContext"].as_object().into_iter().flatten().filter(|(_,v)|matches_binding(v,binding)&&matches!(v["status"].as_str(),Some("queued"|"running"))).count()}
fn periodic_count(d:&Value,binding:&Value)->usize{d["sync"]["pendingContext"].as_object().into_iter().flatten().filter(|(_,v)|
    matches_binding(v,binding)&&v["reason"]=="context_refresh"&&matches!(v["status"].as_str(),Some("queued"|"running"|"deferred"))).count()}
fn head_paused(d:&Value)->bool{
    let Ok(binding)=active_binding(d) else{return true};let binding=binding.to_json();
    let deferred=d["sync"]["pendingContext"].as_object().into_iter().flatten().any(|(_,v)|matches_binding(v,&binding)&&v["status"]=="deferred");
    deferred||unknown_count(d)>CONTEXT_QUEUE_LIMIT
}
fn fair_context(mut pending:Vec<(String,Value)>,limit:usize)->Vec<(String,Value)>{
    pending.sort_by_key(|(key,v)|(clock(&v["queuedAt"]).unwrap_or(0).max(clock(&v["retryAt"]).unwrap_or(0)),key.clone()));
    let (urgent,background):(Vec<_>,Vec<_>)=pending.into_iter().partition(|(_,v)|matches!(v["reason"].as_str(),Some("new_comment"|"status_changed")));
    let mut urgent=urgent.into_iter();let mut background=background.into_iter();let mut selected=Vec::new();
    while selected.len()<limit{
        for _ in 0..3{if selected.len()==limit{break}if let Some(v)=urgent.next().or_else(||background.next()){selected.push(v)}else{return selected}}
        if selected.len()<limit{if let Some(v)=background.next().or_else(||urgent.next()){selected.push(v)}else{break}}
    }
    selected
}
fn refill_context(d:&mut Value)->ApiResult<()> {
    ensure_queue(d);
    let binding=active_binding(d)?.to_json();
    let available=CONTEXT_QUEUE_LIMIT.saturating_sub(active_count(d,&binding));
    let mut deferred:Vec<_>=d["sync"]["pendingContext"].as_object().unwrap().iter()
        .filter(|(_,v)|v["status"]=="deferred"&&v["connectorBinding"]==binding)
        .map(|(key,v)|(key.clone(),v.clone())).collect();
    for (key,_) in fair_context(std::mem::take(&mut deferred),available){d["sync"]["pendingContext"][&key]["status"]=json!("queued");}
    let (active,deferred,total)=queue_counts(d);
    let unknown=unknown_count(d);let blocked=d["sync"]["pendingContext"].as_object().unwrap().values().filter(|v|!matches_binding(v,&binding)).count();
    d["sync"]["contextBackpressure"]=json!({"active":active,"deferred":deferred,"stored":total,"activeLimit":CONTEXT_QUEUE_LIMIT,"periodicRefreshActive":periodic_count(d,&binding),"periodicRefreshLimit":CONTEXT_LIMIT,"storedUnknown":unknown,"maxStoredUnknown":CONTEXT_STORAGE_LIMIT,"canonicalObligations":total-unknown,"blockedBindings":blocked,"headPaused":head_paused(d),"blockedReason":if blocked>0&&unknown>CONTEXT_QUEUE_LIMIT{json!("foreign_binding_obligations_require_reconciliation")}else{Value::Null}});
    Ok(())
}
#[cfg(test)]
fn queue_context(d:&mut Value,observation:&Value,reason:&str,at:&str)->ApiResult<()> {
    ensure_queue(d);let mut counts=queue_counts(d);let mut unknown=unknown_count(d);
    let binding=active_binding(d)?.to_json();
    counts.0=active_count(d,&binding);
    queue_context_budgeted(d,observation,reason,at,&binding,&mut counts,&mut unknown,None)
}
fn queue_context_budgeted(d:&mut Value,observation:&Value,reason:&str,at:&str,binding:&Value,counts:&mut (usize,usize,usize),unknown:&mut usize,canonical:Option<&str>)->ApiResult<()> {
    let key=route(observation)?;
    if let Some(canonical)=canonical{
        let old=&d["sync"]["pendingContext"][&key];
        if !old.is_null()&&matches_binding(old,binding)&&old["canonicalItemId"].is_null(){d["sync"]["pendingContext"][&key]["canonicalItemId"]=json!(canonical);*unknown=unknown.saturating_sub(1);}
    }
    let old=&d["sync"]["pendingContext"][&key];
    if !old["connectorBinding"].is_null() && &old["connectorBinding"]!=binding{return Err(conflict("Pending source context belongs to another connector"))}
    if matches!(old["status"].as_str(),Some("queued"|"running"|"deferred")) {
        // An explicit change must not remain behind a periodic refresh backlog.
        // Promote the same obligation without replacing its in-flight attempt.
        if matches!(reason,"new_comment"|"status_changed") && old["reason"]=="context_refresh" {
            d["sync"]["pendingContext"][&key]["reason"]=json!(reason);
            d["sync"]["pendingContext"][&key]["statusObservedAt"]=observation["observedAt"].clone();
        }
        return Ok(())
    }
    // A failed read gets a durable cooldown; repeated heads must not erase it.
    if old["status"]=="error" && clock(&old["retryAt"]).is_some_and(|until|until>clock(&json!(at)).unwrap_or(0)){return Ok(())}
    let (active,_,_)=*counts;
    if old.is_null()&&canonical.is_none()&&*unknown>=CONTEXT_STORAGE_LIMIT{return Err(conflict("Source context unknown-identity storage is full; observation was not admitted"))}
    if old.is_null(){counts.2+=1;if canonical.is_none(){*unknown+=1;}}
    if active>=CONTEXT_QUEUE_LIMIT{counts.1+=1;}else{counts.0+=1;}
    d["sync"]["pendingContext"][&key]=json!({"objectId":observation["objectId"],"itemId":observation["itemId"],"connectorBinding":binding,"canonicalItemId":canonical.map(|v|json!(v)).unwrap_or_else(||old["canonicalItemId"].clone()),"status":if active>=CONTEXT_QUEUE_LIMIT{"deferred"}else{"queued"},"reason":reason,"queuedAt":at,"statusObservedAt":observation["observedAt"],"attempts":old["attempts"].as_u64().unwrap_or(0)});
    Ok(())
}
fn needs_context(item:&Value,observation:&Value,at:i64)->bool{
    item["providerStatus"]!=observation["status"]||matches!(observation["status"].as_str(),Some("new"|"inprogress"))
        &&clock(&item["contextObservedAt"]).or_else(||clock(&item["providerObservedAt"])).is_none_or(|t|t<=at-480_000)
}
fn prune_fresh_refreshes(d:&mut Value,binding:&ConnectorBinding)->ApiResult<()> {
    let binding_json=binding.to_json();
    let fresh:std::collections::HashMap<_,_>=list(d,"items").iter().filter(|item|bound_item(binding,item).is_ok()).filter_map(|item|Some((route(item).ok()?,clock(&item["contextObservedAt"])?))).collect();
    ensure_queue(d);
    d["sync"]["pendingContext"].as_object_mut().unwrap().retain(|key,record|{
        !(record["status"]=="queued"&&record["reason"]=="context_refresh"&&record["connectorBinding"]==binding_json
            &&clock(&record["statusObservedAt"]).is_some_and(|queued|fresh.get(key).is_some_and(|observed|*observed>=queued)))
    });
    refill_context(d)
}

fn admit(d:&mut Value,binding:&ConnectorBinding,captured:&Value,response:&Value,expected:Option<&[Value]>,at:&str)->ApiResult<Value>{
    if active_binding(d)?!=*binding{return Err(conflict("Connector changed during fast synchronization"))}
    if response["kind"]!=if expected.is_some(){"exact-status-refresh"}else{"open-status-head"}{return Err(bad("Invalid fast status response"))}
    let observations=response["items"].as_array().filter(|v|v.len()<=if expected.is_some(){STATUS_LIMIT}else{HEAD_LIMIT}).ok_or_else(||bad("Invalid status observations"))?;
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
    let mut head_objects=std::collections::HashMap::<String,usize>::new();
    if expected.is_none(){
        for observation in observations{let count=head_objects.entry(observation["objectId"].as_str().unwrap().to_owned()).or_default();*count+=1;if *count>HEAD_ROWS_PER_OBJECT{return Err(bad("Status head exceeds per-object read budget"))}}
    }
    let coverage:Vec<_>=response["coverage"].as_array().into_iter().flatten().map(|c|json!({"objectId":c["objectId"],"hasMore":c["hasMore"],"count":c["count"],"observedAt":c["observedAt"]})).collect();
    if coverage.len()>HEAD_OBJECT_LIMIT{return Err(bad("Status coverage exceeds object budget"))}
    let mut coverage_seen=std::collections::HashSet::new();
    for c in &coverage{
        let object=c["objectId"].as_str().filter(|s|!s.is_empty()&&s.len()<=256&&!s.chars().any(char::is_control)).ok_or_else(||bad("Invalid status coverage identity"))?;
        if !coverage_seen.insert(object)||!c["hasMore"].is_boolean()||(!c["count"].is_null()&&c["count"].as_u64().is_none())||clock(&c["observedAt"]).is_none_or(|t|t>current_time+60_000){return Err(bad("Invalid status coverage"))}
        head_objects.entry(object.to_owned()).or_default();
    }
    if expected.is_none(){for error in errors{let object=error["objectId"].as_str().filter(|s|!s.is_empty()&&s.len()<=256&&!s.chars().any(char::is_control)).ok_or_else(||bad("Invalid head error identity"))?;head_objects.entry(object.to_owned()).or_default();}}
    if head_objects.len()>HEAD_OBJECT_LIMIT{return Err(bad("Status head exceeds object budget"))}
    if let Some(expected)=expected {
        for error in errors{
            let key=route(error)?;
            if !seen.insert(key)||!expected.iter().any(|v|v["objectId"]==error["objectId"]&&v["itemId"]==error["itemId"]){return Err(bad("Duplicate or foreign status error target"))}
        }
        if seen.len()!=expected.len(){return Err(bad("Exact status response omitted requested targets"))}
    }
    let indexes:std::collections::HashMap<_,_>=list(d,"items").iter().enumerate().filter_map(|(i,item)|route(item).ok().map(|key|(key,i))).collect();
    // Unchanged status does not demand a company-wide context refresh every
    // eight minutes. Keep at most one worker batch of new periodic obligations;
    // retained legacy backlog drains normally. Explicit changes bypass this cap.
    let queue_binding=binding.to_json();
    let periodic=periodic_count(d,&queue_binding);
    let mut background:Vec<_>=admitted.iter().filter_map(|(key,observation,observed)|{
        let item=&list(d,"items")[*indexes.get(key)?];
        if !d["sync"]["pendingContext"][key].is_null() || bound_item(binding,item).is_err()
            || captured[key]!=token(item) || *observed<clock(&item["statusObservedAt"]).unwrap_or(0).max(clock(&item["providerObservedAt"]).unwrap_or(0))
            || item["providerStatus"]!=observation["status"] || !needs_context(item,observation,current_time){return None}
        Some((clock(&item["contextObservedAt"]).or_else(||clock(&item["providerObservedAt"])).unwrap_or(0),key.clone()))
    }).collect();
    background.sort();
    let background:std::collections::HashSet<_>=background.into_iter().take(CONTEXT_LIMIT.saturating_sub(periodic)).map(|(_,key)|key).collect();
    let additions=admitted.iter().filter(|(key,_,_)|{
        if !d["sync"]["pendingContext"][key].is_null(){return false}
        !indexes.contains_key(key)
    }).count();
    if additions>0&&unknown_count(d).saturating_add(additions)>CONTEXT_STORAGE_LIMIT{return Err(conflict("Source context unknown-identity storage is full; status response was not admitted"))}
    ensure_queue(d);let mut counts=queue_counts(d);let mut unknown=unknown_count(d);counts.0=active_count(d,&queue_binding);
    let mut changed=0;let mut stale=0;
    for (key,observation,observed) in admitted{
        if expected.is_some(){d["sync"]["fastStatusTargets"][&key]["error"]=Value::Null;d["sync"]["fastStatusTargets"][&key]["completedAt"]=json!(at);}
        let Some(&index)=indexes.get(&key) else{queue_context_budgeted(d,observation,"new_comment",at,&queue_binding,&mut counts,&mut unknown,None)?;continue;};
        let item=&list(d,"items")[index];
        bound_item(binding,item)?;
        let canonical=required(item,"id")?.to_owned();
        let latest=clock(&item["statusObservedAt"]).unwrap_or(0).max(clock(&item["providerObservedAt"]).unwrap_or(0));
        if captured[&key]!=token(item)||observed<latest||(observed==latest&&item["providerStatus"]!=observation["status"]){stale+=1;continue;}
        let status_changed=item["providerStatus"]!=observation["status"];
        let workflow_mismatch=(observation["status"]=="closed"&&!matches!(item["workflow"].as_str(),Some("closed"|"waiting")))
            ||(observation["status"]=="deleted"&&item["workflow"]!="deleted");
        let context_needed=needs_context(item,observation,current_time);
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
        if context_needed && (status_changed || background.contains(&key)
            || matches!(d["sync"]["pendingContext"][&key]["status"].as_str(),Some("queued"|"running"|"deferred"))) {
            queue_context_budgeted(d,observation,if status_changed{"status_changed"}else{"context_refresh"},at,&queue_binding,&mut counts,&mut unknown,Some(&canonical))?;
        }
    }
    if let Some(expected)=expected {
        for error in errors{
            let key=route(error)?;
            if !expected.iter().any(|v|v["objectId"]==error["objectId"]&&v["itemId"]==error["itemId"]){return Err(bad("Foreign status error target"))}
            let code=error["code"].as_str().unwrap_or("SOURCE_STATUS_ERROR").chars().filter(|c|c.is_ascii_alphanumeric()||*c=='_').take(80).collect::<String>();
            d["sync"]["fastStatusTargets"][&key]["error"]=json!(code);d["sync"]["fastStatusTargets"][&key]["completedAt"]=json!(at);
        }
    }
    refill_context(d)?;
    Ok(json!({"kind":response["kind"],"observed":observations.len(),"changed":changed,"staleResponses":stale,"errors":errors.len(),"coverage":coverage,"hasMore":response["hasMore"],"coverageIsSnapshot":false,"contextBackpressure":d["sync"]["contextBackpressure"]}))
}

fn busy(d:&Value,kind:&str)->bool{list(d,"jobs").iter().any(|j|j["kind"]==kind&&matches!(j["status"].as_str(),Some("running"|"queued")))}
fn status_due(d:&Value,at:i64)->bool{!busy(d,"status_sync")&&clock(&d["sync"]["fastStatus"]["nextRunAt"]).is_none_or(|next|next<=at)}
pub(super) async fn tick(app:&App,state:&Value)->ApiResult<()> {
    let at=chrono::Utc::now().timestamp_millis();
    if status_due(state,at){
        let claimed=app.change_source_claim(|d|{
            if !status_due(d,at){return Ok(None)}
            let binding=active_binding(d)?;let account=bridge_account(&binding)?.to_owned();
            prune_fresh_refreshes(d,&binding)?;
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
                let (head_capture,paused)={let fresh=worker.db.read_source_status().await?;(capture(&fresh),head_paused(&fresh))};
                if paused{successes.push(json!({"kind":"open-status-head","deferred":true,"reason":"context_backpressure","coverageIsSnapshot":false}));}
                else{match worker.bridge("head",json!({"account":account})).await{
                    Ok(response)=>match worker.change_status(response["items"].as_array().map(Vec::as_slice).unwrap_or(&[]),|d|admit(d,&binding,&head_capture,&response,None,&now())).await{Ok(v)=>{if v["errors"].as_u64().unwrap_or(0)>0{errors+=1}if v["observed"].as_u64().unwrap_or(0)>0||v["errors"]==0{successes.push(v)}},Err(_)=>errors+=1},
                    Err(_)=>errors+=1,
                }}
                worker.change_schedule(|d|{
                    if active_binding(d)?!=binding{return Err(conflict("Connector changed during fast synchronization"))}
                    let consecutive=if errors==0{0}else{d["sync"]["fastStatus"]["consecutiveErrors"].as_u64().unwrap_or(0)+1};
                    let delay=if successes.is_empty(){(STATUS_SECONDS*(1_i64<<consecutive.min(4))).min(300)}else{STATUS_SECONDS};
                    d["sync"]["fastStatus"]=json!({"state":if errors==0&&!paused{"monitoring"}else{"partial"},"lastFinishedAt":now(),"nextRunAt":(chrono::Utc::now()+chrono::Duration::seconds(delay)).to_rfc3339(),"consecutiveErrors":consecutive,"results":successes,"batchErrors":errors});Ok(())
                }).await?;
                if successes.is_empty(){Err(internal("Fast source status reads failed"))}else{Ok(json!({"refreshed":successes.iter().any(|v|v["deferred"]!=true),"results":successes,"batchErrors":errors}))}
            });
        }
    }
    context_tick(app,state).await
}

fn context_candidates(d:&Value,at:i64)->Vec<(String,Value)>{
    context_candidates_bound(d,at,None)
}
fn context_candidates_bound(d:&Value,at:i64,binding:Option<&Value>)->Vec<(String,Value)>{
    let retry_slots=CONTEXT_QUEUE_LIMIT.saturating_sub(binding.map_or_else(||queue_counts(d).0,|binding|active_count(d,binding)));
    let mut pending:Vec<_>=d["sync"]["pendingContext"].as_object().into_iter().flatten().filter(|(_,v)|{
        if binding.is_some_and(|binding|!matches_binding(v,binding)){return false}
        let abandoned=v["status"]=="running"&&v["jobId"].as_str().and_then(|id|row(d,"jobs",id).ok()).is_none_or(|j|!matches!(j["status"].as_str(),Some("running"|"queued")));
        (v["status"]=="queued"||(v["status"]=="error"&&retry_slots>0)||abandoned)&&clock(&v["retryAt"]).is_none_or(|t|t<=at)
    }).map(|(k,v)|(k.clone(),v.clone())).collect();
    // Retry cooldown moves a failure behind untouched work, rather than letting
    // the oldest permanently failing target monopolize every context batch.
    pending.sort_by_key(|(key,v)|(clock(&v["queuedAt"]).unwrap_or(0).max(clock(&v["retryAt"]).unwrap_or(0)),key.clone()));
    let mut retries=0;pending.retain(|(_,v)|if v["status"]=="error"{retries+=1;retries<=retry_slots}else{true});
    fair_context(pending,CONTEXT_LIMIT)
}
fn ready_context_batch<S:futures_util::Stream+Unpin>(first:S::Item,results:&mut S)->Vec<S::Item>{
    let mut ready=vec![first];
    while let Some(Some(next))=results.next().now_or_never(){ready.push(next);}
    ready
}
async fn context_tick(app:&App,state:&Value)->ApiResult<()> {
    let at=chrono::Utc::now().timestamp_millis();
    // The compact projection intentionally omits binding snapshots. A due
    // failure must reach the bound claim even if historical foreign records
    // fill its conservative active count.
    let due_error=state["sync"]["pendingContext"].as_object().into_iter().flatten().any(|(_,v)|v["status"]=="error"&&clock(&v["retryAt"]).is_none_or(|retry|retry<=at));
    if busy(state,"context_sync")||(!due_error&&context_candidates(state,at).is_empty()&&(queue_counts(state).1==0||queue_counts(state).0>=CONTEXT_QUEUE_LIMIT)){return Ok(())}
    let claimed=app.change_schedule(|d|{
        if busy(d,"context_sync"){return Ok(None)}
        refill_context(d)?;
        let binding=active_binding(d)?;
        let pending=context_candidates_bound(d,chrono::Utc::now().timestamp_millis(),Some(&binding.to_json()));if pending.is_empty(){return Ok(None)}
        let account=bridge_account(&binding)?.to_owned();let job=new_job(d,"context_sync","")?;
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
            while let Some(first)=results.next().await {
            // Drain only results ready now: a slow read must never hold a
            // completed peer until its timeout. The bounded stream owns every
            // still-running read; dropping this next() poll does not drop them.
            let ready=ready_context_batch(first,&mut results);
            let mut snapshots=Vec::new();let mut failures=Vec::new();
            for (key,target,result) in ready {
                match result {Ok(snapshot)=>snapshots.push((key,target,snapshot)),Err(_)=>failures.push(key)}
            }
            if !snapshots.is_empty(){
                let admitted=worker.change_source_snapshot(|d|{
                    for (key,target,snapshot) in &snapshots {admit_context(d,&binding,&run,key,target,snapshot)?;}
                    Ok(())
                }).await;
                if admitted.is_ok(){completed+=snapshots.len();}
                else {
                    // A rejected batch transaction persisted nothing. Retry
                    // only local admission individually, never provider reads
                    // or mutations, so one stale target cannot lose its peers.
                    for (key,target,snapshot) in snapshots {
                        if worker.change_source_snapshot(|d|admit_context(d,&binding,&run,&key,&target,&snapshot)).await.is_ok(){completed+=1;}
                        else{failures.push(key);}
                    }
                }
            }
            for key in failures {
                failed+=1;worker.change_schedule(|d|{
                    if active_binding(d)?!=binding{return Err(conflict("Connector changed during context synchronization"))}
                    let record=&mut d["sync"]["pendingContext"][&key];if record["jobId"]!=run{return Ok(())}
                    record["status"]=json!("error");record["error"]=json!("Source context read failed");record["completedAt"]=json!(now());
                    let delay=(30*(1_i64<<record["attempts"].as_u64().unwrap_or(1).min(5))).min(900);
                    record["retryAt"]=json!((chrono::Utc::now()+chrono::Duration::seconds(delay)).to_rfc3339());refill_context(d)?;Ok(())
                }).await?;
            }
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
    refill_context(d)?;
    Ok(())
}

#[cfg(test)]
mod tests{
    use super::*;
    const AT:&str="2026-09-22T12:00:00Z";
    #[tokio::test]
    async fn ready_context_batch_coalesces_ready_reads_without_waiting_or_losing_slow_peer(){
        let (send,receive)=tokio::sync::oneshot::channel();
        let reads=vec![futures_util::future::ready(1).boxed(),futures_util::future::ready(2).boxed(),async move{receive.await.unwrap()}.boxed()];
        let mut results=stream::iter(reads).buffer_unordered(2);
        let first=results.next().await.unwrap();
        assert_eq!(ready_context_batch(first,&mut results),vec![1,2]);
        send.send(3).unwrap();assert_eq!(results.next().await,Some(3));assert_eq!(results.next().await,None);
    }
    fn fixture()->Value{let mut d=empty();d["items"]=json!([{"id":"item-11391-42","objectId":"11391","itemId":"42","postKey":"11391:p","conversationKey":"11391:t","providerStatus":"new","workflow":"prepared","draft":"Human draft","draftEdited":true,"draftOrigin":{"proposalId":"saved"},"revision":3,"providerObservedAt":"2026-09-22T10:00:00Z"}]);d}
    fn observation(status:&str)->Value{json!({"objectId":"11391","itemId":"42","status":status,"observedAt":AT})}
    fn reply(items:Value)->Value{json!({"kind":"exact-status-refresh","observedAt":AT,"items":items,"errors":[]})}
    fn apply(d:&mut Value,status:&str)->Value{let b=active_binding(d).unwrap();let c=capture(d);admit(d,&b,&c,&reply(json!([observation(status)])),Some(&[json!({"objectId":"11391","itemId":"42"})]),AT).unwrap()}
    fn full_head()->Value{
        let items:Vec<_>=(0..HEAD_OBJECT_LIMIT).flat_map(|object|(0..HEAD_ROWS_PER_OBJECT).map(move|item|json!({"objectId":format!("object-{object}"),"itemId":format!("head-{item}"),"status":"new","observedAt":AT}))).collect();
        let coverage:Vec<_>=(0..HEAD_OBJECT_LIMIT).map(|object|json!({"objectId":format!("object-{object}"),"hasMore":true,"count":3000,"observedAt":AT})).collect();
        json!({"kind":"open-status-head","items":items,"errors":[],"coverage":coverage,"hasMore":true})
    }
    fn full_queue(d:&mut Value,count:usize){
        let binding=active_binding(d).unwrap().to_json();let mut entries=serde_json::Map::new();
        for i in 0..count{let target=json!({"objectId":"11391","itemId":format!("old-{i}"),"connectorBinding":binding,"status":"queued","reason":"context_refresh","queuedAt":"2026-09-22T11:00:00Z","attempts":0});entries.insert(route(&target).unwrap(),target);}
        d["sync"]["pendingContext"]=Value::Object(entries);
    }
    #[test]
    fn periodic_window_preserves_legacy_backlog_applies_status_and_promotes_explicit_changes(){
        for status in ["queued","running","deferred"] {
            let mut d=fixture();full_queue(&mut d,1000);
            let mut unchanged=d["items"][0].clone();unchanged["id"]=json!("item-11391-43");unchanged["itemId"]=json!("43");
            d["items"].as_array_mut().unwrap().push(unchanged);
            queue_context(&mut d,&observation("new"),"context_refresh",AT).unwrap();
            let key=route(&observation("new")).unwrap();
            d["sync"]["pendingContext"][&key]["status"]=json!(status);
            d["sync"]["pendingContext"][&key]["jobId"]=json!("retained-attempt");
            d["sync"]["pendingContext"][&key]["attempts"]=json!(7);
            if status=="running" {d["jobs"]=json!([{"id":"retained-attempt","kind":"context_sync","status":"running"}]);}
            let pending=d["sync"]["pendingContext"].clone();
            let binding=active_binding(&d).unwrap();let captured=capture(&d);
            let response=json!({"kind":"open-status-head","items":[observation("closed"),
                {"objectId":"11391","itemId":"43","status":"new","observedAt":AT},
                {"objectId":"11391","itemId":"arrival","status":"new","observedAt":AT}],"errors":[]});
            admit(&mut d,&binding,&captured,&response,None,AT).unwrap();
            assert_eq!(d["items"][0]["providerStatus"],"closed");assert_eq!(d["items"][0]["draft"],"Human draft");
            assert_eq!(d["items"][1]["statusObservedAt"],AT);
            let unchanged_key=route(&json!({"objectId":"11391","itemId":"43"})).unwrap();
            assert!(d["sync"]["pendingContext"][unchanged_key].is_null());
            for i in 0..1000 {let old=route(&json!({"objectId":"11391","itemId":format!("old-{i}")})).unwrap();assert_eq!(d["sync"]["pendingContext"][&old],pending[&old]);}
            let promoted=&d["sync"]["pendingContext"][&key];
            assert_eq!(promoted["reason"],"status_changed");assert_eq!(promoted["jobId"],"retained-attempt");
            assert_eq!(promoted["queuedAt"],pending[&key]["queuedAt"]);assert_eq!(promoted["attempts"],7);
            assert_eq!(promoted["status"],if status=="deferred"{"queued"}else{status});
            let arrival=route(&json!({"objectId":"11391","itemId":"arrival"})).unwrap();
            assert_eq!(d["sync"]["pendingContext"][arrival]["reason"],"new_comment");
            let selected=context_candidates(&d,clock(&json!(AT)).unwrap());
            assert!(selected.iter().any(|(_,v)|v["itemId"]=="arrival"));
            assert_eq!(selected.iter().any(|(_,v)|v["itemId"]=="42"),status!="running");
        }
    }

    #[test]
    fn periodic_window_chooses_oldest_contexts_and_refills_after_exact_completion(){
        let mut d=fixture();let prototype=d["items"][0].clone();
        d["items"]=json!((0..9).map(|n|{let mut item=prototype.clone();item["id"]=json!(format!("item-11391-{n}"));item["itemId"]=json!(n.to_string());item["contextObservedAt"]=json!(format!("2026-09-22T10:{n:02}:00Z"));item}).collect::<Vec<_>>());
        let binding=active_binding(&d).unwrap();
        let response=json!({"kind":"open-status-head","items":(0..9).rev().map(|n|json!({"objectId":"11391","itemId":n.to_string(),"status":"new","observedAt":AT})).collect::<Vec<_>>(),"errors":[]});
        let captured=capture(&d);admit(&mut d,&binding,&captured,&response,None,AT).unwrap();
        assert_eq!(queue_counts(&d).0,CONTEXT_LIMIT);
        for n in 0..9 {let key=route(&json!({"objectId":"11391","itemId":n.to_string()})).unwrap();assert_eq!(!d["sync"]["pendingContext"][key].is_null(),n<CONTEXT_LIMIT);}
        let pending=d["sync"]["pendingContext"].clone();
        let captured=capture(&d);admit(&mut d,&binding,&captured,&response,None,AT).unwrap();
        assert_eq!(d["sync"]["pendingContext"],pending);
        for n in 0..CONTEXT_LIMIT {
            let key=route(&json!({"objectId":"11391","itemId":n.to_string()})).unwrap();
            d["sync"]["pendingContext"][&key]["jobId"]=json!("completed-refresh");
            let target=d["sync"]["pendingContext"][&key].clone();
            let mut item=d["items"][n].clone();item["contextObservedAt"]=json!(AT);item["providerStatusObservedAt"]=json!(AT);
            admit_context(&mut d,&binding,"completed-refresh",&key,&target,&json!({"items":[item]})).unwrap();
        }
        assert_eq!(queue_counts(&d).0,0);
        let captured=capture(&d);admit(&mut d,&binding,&captured,&response,None,AT).unwrap();
        assert_eq!(queue_counts(&d).0,CONTEXT_LIMIT);
        for n in 0..9 {let key=route(&json!({"objectId":"11391","itemId":n.to_string()})).unwrap();assert_eq!(!d["sync"]["pendingContext"][key].is_null(),(CONTEXT_LIMIT..2*CONTEXT_LIMIT).contains(&n));}
    }
    #[test]
    fn configured_twelve_object_heads_preserve_all_coverage_and_durable_identities(){
        let mut d=fixture();let original=d["items"].clone();let binding=active_binding(&d).unwrap();let captured=capture(&d);let head=full_head();
        let result=admit(&mut d,&binding,&captured,&head,None,AT).unwrap();
        assert_eq!(result["observed"],HEAD_LIMIT);assert_eq!(result["coverage"].as_array().unwrap().len(),HEAD_OBJECT_LIMIT);
        assert_eq!(result["coverageIsSnapshot"],false);assert_eq!(result["hasMore"],true);assert_eq!(d["items"],original);
        assert_eq!(queue_counts(&d),(CONTEXT_QUEUE_LIMIT,HEAD_LIMIT-CONTEXT_QUEUE_LIMIT,HEAD_LIMIT));assert!(head_paused(&d));
        let pending=d["sync"]["pendingContext"].clone();admit(&mut d,&binding,&captured,&head,None,AT).unwrap();assert_eq!(d["sync"]["pendingContext"],pending);
        for item in head["items"].as_array().unwrap(){let entry=&d["sync"]["pendingContext"][route(item).unwrap()];assert_eq!(entry["connectorBinding"],binding.to_json());assert!(!entry.is_null());}
    }
    #[test]
    fn head_backpressure_preserves_known_exact_status_and_connector_identity(){
        let mut d=fixture();full_queue(&mut d,CONTEXT_QUEUE_LIMIT);
        // A new head identity waits while known exact status remains eligible.
        queue_context(&mut d,&json!({"objectId":"11391","itemId":"new-head","observedAt":AT}),"new_comment",AT).unwrap();
        refill_context(&mut d).unwrap();assert!(head_paused(&d));assert_eq!(targets(&d,&active_binding(&d).unwrap()).len(),1);
        apply(&mut d,"closed");assert_eq!(d["items"][0]["providerStatus"],"closed");assert_eq!(d["items"][0]["draft"],"Human draft");
        assert_eq!(d["sync"]["pendingContext"][route(&observation("closed")).unwrap()]["status"],"deferred");
        let key=route(&json!({"objectId":"11391","itemId":"new-head"})).unwrap();
        d["sync"]["pendingContext"][&key]["connectorBinding"]["revision"]=json!(999);
        let before=d.clone();assert!(queue_context(&mut d,&json!({"objectId":"11391","itemId":"new-head","observedAt":AT}),"new_comment",AT).is_err());assert_eq!(d,before);
        d["sync"]["pendingContext"].as_object_mut().unwrap().retain(|k,_|k==&key);refill_context(&mut d).unwrap();assert_eq!(d["sync"]["pendingContext"][&key]["status"],"deferred");
    }
    #[tokio::test]
    async fn new_comments_progress_before_refresh_backlog_with_one_background_slot(){
        let temp=tempfile::tempdir().unwrap();let db=Database::Sqlite(open_db(&temp.path().join("priority.sqlite")).await.unwrap());
        db.change(|d|{full_queue(d,1000);for i in 0..5{queue_context(d,&json!({"objectId":"11391","itemId":format!("urgent-{i}"),"observedAt":AT}),"new_comment",AT)?;}Ok(())}).await.unwrap();
        let full=db.read().await.unwrap();let compact=db.read_schedule().await.unwrap();let at=clock(&json!(AT)).unwrap();
        for d in [&full,&compact]{let selected=context_candidates(d,at);assert_eq!(selected.len(),4);assert_eq!(selected.iter().filter(|(_,v)|v["reason"]=="new_comment").count(),3);assert_eq!(selected[3].1["reason"],"context_refresh");}
        let keys=|d:&Value|context_candidates(d,at).into_iter().map(|(key,_)|key).collect::<Vec<_>>();assert_eq!(keys(&full),keys(&compact));db.close().await;
    }
    #[test]
    fn historical_binding_records_never_starve_or_retarget_current_context_claims(){
        let mut d=fixture();full_queue(&mut d,4);let binding=active_binding(&d).unwrap().to_json();
        for v in d["sync"]["pendingContext"].as_object_mut().unwrap().values_mut(){v["connectorBinding"]["revision"]=json!(999);}
        queue_context(&mut d,&json!({"objectId":"11391","itemId":"current","observedAt":AT}),"context_refresh",AT).unwrap();
        let selected=context_candidates_bound(&d,clock(&json!(AT)).unwrap(),Some(&binding));assert_eq!(selected.len(),1);assert_eq!(selected[0].1["itemId"],"current");
        d["sync"]["pendingContext"].as_object_mut().unwrap().retain(|_,v|v["itemId"]!="current");
        for v in d["sync"]["pendingContext"].as_object_mut().unwrap().values_mut(){v["status"]=json!("deferred");}
        refill_context(&mut d).unwrap();assert!(!head_paused(&d));assert_eq!(d["sync"]["contextBackpressure"]["blockedBindings"],4);
        assert!(d["sync"]["pendingContext"].as_object().unwrap().values().all(|v|v["connectorBinding"]["revision"]==999&&v["status"]=="deferred"));
    }
    #[test]
    fn fresh_context_prunes_only_bound_queued_periodic_refresh_obligations(){
        for mode in ["fresh","new_comment","running","error","foreign","older_context","no_context"]{
            let mut d=fixture();d["items"][0]["contextObservedAt"]=json!(AT);let binding=active_binding(&d).unwrap();
            queue_context(&mut d,&observation("new"),"context_refresh",AT).unwrap();let key=route(&observation("new")).unwrap();
            match mode{"new_comment"=>d["sync"]["pendingContext"][&key]["reason"]=json!("new_comment"),"running"|"error"=>d["sync"]["pendingContext"][&key]["status"]=json!(mode),"foreign"=>d["sync"]["pendingContext"][&key]["connectorBinding"]["revision"]=json!(999),"older_context"=>d["items"][0]["contextObservedAt"]=json!("2026-09-22T11:59:59Z"),"no_context"=>d["items"][0]["contextObservedAt"]=Value::Null,_=>{}}
            let before=d["items"].clone();prune_fresh_refreshes(&mut d,&binding).unwrap();assert_eq!(d["items"],before);assert_eq!(d["sync"]["pendingContext"][&key].is_null(),mode=="fresh","{mode}");
        }
    }
    #[test]
    fn full_queue_defers_head_and_failed_slots_refill_without_retry_starvation(){
        let mut d=fixture();full_queue(&mut d,CONTEXT_QUEUE_LIMIT);let binding=active_binding(&d).unwrap();let captured=capture(&d);let head=full_head();
        admit(&mut d,&binding,&captured,&head,None,AT).unwrap();assert_eq!(queue_counts(&d),(CONTEXT_QUEUE_LIMIT,HEAD_LIMIT,CONTEXT_STORAGE_LIMIT));
        let pending=d["sync"]["pendingContext"].clone();admit(&mut d,&binding,&captured,&head,None,AT).unwrap();assert_eq!(d["sync"]["pendingContext"],pending);
        // A failed attempt retains evidence but releases its active slot even
        // when it will keep failing. The newest untouched head can then run.
        let old=route(&json!({"objectId":"11391","itemId":"old-0"})).unwrap();
        d["sync"]["pendingContext"][&old]["status"]=json!("error");d["sync"]["pendingContext"][&old]["retryAt"]=json!("2026-09-22T12:01:00Z");
        refill_context(&mut d).unwrap();assert_eq!(queue_counts(&d),(CONTEXT_QUEUE_LIMIT,HEAD_LIMIT-1,CONTEXT_STORAGE_LIMIT));
        assert_eq!(d["sync"]["pendingContext"][&old]["status"],"error");
        assert!(context_candidates(&d,clock(&json!("2026-09-22T12:02:00Z")).unwrap()).iter().all(|(key,_)|key!=&old));
        let promoted=d["sync"]["pendingContext"].as_object().unwrap().iter().find(|(_,v)|v["status"]=="queued"&&v["objectId"]!="11391").unwrap().0.clone();
        d["sync"]["pendingContext"].as_object_mut().unwrap().remove(&promoted);refill_context(&mut d).unwrap();
        assert_eq!(queue_counts(&d),(CONTEXT_QUEUE_LIMIT,HEAD_LIMIT-2,CONTEXT_STORAGE_LIMIT-1));
        // Once active work clears, all remaining deferred identities refill;
        // head reads resume after the backlog has room again.
        d["sync"]["pendingContext"].as_object_mut().unwrap().retain(|_,v|v["status"]=="deferred");
        refill_context(&mut d).unwrap();assert_eq!(queue_counts(&d).0,CONTEXT_QUEUE_LIMIT);
        let keys:Vec<_>=context_candidates(&d,clock(&json!(AT)).unwrap()).into_iter().map(|(key,_)|key).collect();assert_eq!(keys.len(),CONTEXT_LIMIT);
        d["sync"]["pendingContext"].as_object_mut().unwrap().retain(|_,v|v["status"]=="deferred");refill_context(&mut d).unwrap();assert!(!head_paused(&d));
    }
    #[test]
    fn oversized_head_or_storage_rejects_without_silent_loss_and_keeps_legacy_overfull_queue(){
        for mode in ["rows","objects","coverage","storage"]{
            let mut d=fixture();let binding=active_binding(&d).unwrap();let captured=capture(&d);let mut head=full_head();
            match mode{
                "rows"=>head["items"].as_array_mut().unwrap().push(json!({"objectId":"object-0","itemId":"extra","status":"new","observedAt":AT})),
                "objects"=>{head["items"][0]["objectId"]=json!("object-extra");},
                "coverage"=>head["coverage"].as_array_mut().unwrap().push(json!({"objectId":"object-extra","hasMore":true,"count":1,"observedAt":AT})),
                _=>full_queue(&mut d,CONTEXT_STORAGE_LIMIT+1),
            }
            let before=d.clone();assert!(admit(&mut d,&binding,&captured,&head,None,AT).is_err(),"{mode}");assert_eq!(d,before,"{mode}");
        }
        let mut d=fixture();full_queue(&mut d,CONTEXT_STORAGE_LIMIT+1);refill_context(&mut d).unwrap();assert_eq!(queue_counts(&d).2,CONTEXT_STORAGE_LIMIT+1);
    }
    #[tokio::test]
    async fn deferred_queue_survives_restart_projection_and_recovers_after_context_completion(){
        let temp=tempfile::tempdir().unwrap();let path=temp.path().join("deferred.sqlite");
        let db=Database::Sqlite(open_db(&path).await.unwrap());
        db.change(|d|{full_queue(d,CONTEXT_QUEUE_LIMIT);let binding=active_binding(d)?;let captured=capture(d);admit(d,&binding,&captured,&full_head(),None,AT)?;Ok(())}).await.unwrap();db.close().await;
        let db=Database::Sqlite(open_db(&path).await.unwrap());let restored=db.read().await.unwrap();let compact=db.read_schedule().await.unwrap();
        assert_eq!(queue_counts(&restored),queue_counts(&compact));assert!(head_paused(&compact));
        let keys=|d:&Value|context_candidates(d,clock(&json!(AT)).unwrap()).into_iter().map(|(key,_)|key).collect::<Vec<_>>();assert_eq!(keys(&restored),keys(&compact));
        db.change(|d|{
            let key=route(&json!({"objectId":"11391","itemId":"old-0"}))?;let target=d["sync"]["pendingContext"][&key].clone();d["sync"]["pendingContext"][&key]["jobId"]=json!("resume");
            let snapshot=json!({"items":[{"id":"item-11391-old-0","objectId":"11391","itemId":"old-0","postKey":"11391:p","conversationKey":"11391:t","providerStatus":"new","providerStatusObservedAt":AT,"contextObservedAt":AT,"text":"Synthetic recovered context"}]});
            let binding=active_binding(d)?;admit_context(d,&binding,"resume",&key,&target,&snapshot)
        }).await.unwrap();
        let recovered=db.read().await.unwrap();assert_eq!(queue_counts(&recovered),(CONTEXT_QUEUE_LIMIT,HEAD_LIMIT-1,CONTEXT_STORAGE_LIMIT-1));assert_eq!(keys(&recovered),keys(&db.read_schedule().await.unwrap()));db.close().await;
    }
    async fn exercise_complete_head_storage(db:&Database){
        let head=full_head();
        db.change(|d|{
            let mut items=fixture()["items"].as_array().unwrap().clone();
            for observation in head["items"].as_array().unwrap(){items.push(json!({"id":format!("canonical-{}-{}",observation["objectId"].as_str().unwrap(),observation["itemId"].as_str().unwrap()),"objectId":observation["objectId"],"itemId":observation["itemId"],"postKey":"synthetic:p","conversationKey":"synthetic:t","providerStatus":"new","workflow":"attention","providerObservedAt":"2026-09-22T10:00:00Z","draft":"preserved draft","revision":1}));}
            d["items"]=json!(items);Ok(())
        }).await.unwrap();
        let before=db.read().await.unwrap();let binding=active_binding(&before).unwrap();let captured=capture(&before);
        let (result,changed)=db.change_status_observed(head["items"].as_array().unwrap(),|d|admit(d,&binding,&captured,&head,None,AT)).await.unwrap();
        assert!(changed);assert_eq!(result["observed"],HEAD_LIMIT);assert_eq!(result["coverage"].as_array().unwrap().len(),HEAD_OBJECT_LIMIT);
        let periodic=db.read().await.unwrap();assert_eq!(periodic["items"][0],before["items"][0]);
        assert_eq!(queue_counts(&periodic).2,CONTEXT_LIMIT);assert_eq!(periodic["sync"]["contextBackpressure"]["canonicalObligations"],CONTEXT_LIMIT);assert_eq!(unknown_count(&periodic),0);
        assert_eq!(periodic["sync"]["contextBackpressure"]["periodicRefreshLimit"],CONTEXT_LIMIT);
        for item in periodic["items"].as_array().unwrap().iter().skip(1){assert_eq!(item["statusObservedAt"],AT);assert_eq!(item["draft"],"preserved draft");}
        // The periodic window must not reduce the full head's durable explicit
        // change coverage, including promotion of the already pending batch.
        let mut changed_head=head.clone();
        for observation in changed_head["items"].as_array_mut().unwrap(){observation["status"]=json!("inprogress");observation["observedAt"]=json!("2026-09-22T12:01:00Z");}
        let captured=capture(&periodic);
        let (result,changed)=db.change_status_observed(changed_head["items"].as_array().unwrap(),|d|admit(d,&binding,&captured,&changed_head,None,"2026-09-22T12:01:00Z")).await.unwrap();
        assert!(changed);assert_eq!(result["observed"],HEAD_LIMIT);assert_eq!(result["changed"],HEAD_LIMIT);assert_eq!(result["coverage"].as_array().unwrap().len(),HEAD_OBJECT_LIMIT);
        let saved=db.read().await.unwrap();assert_eq!(saved["items"][0],before["items"][0]);
        assert_eq!(queue_counts(&saved),(CONTEXT_QUEUE_LIMIT,HEAD_LIMIT-CONTEXT_QUEUE_LIMIT,HEAD_LIMIT));
        assert_eq!(saved["sync"]["contextBackpressure"]["canonicalObligations"],HEAD_LIMIT);assert_eq!(unknown_count(&saved),0);
        for item in saved["items"].as_array().unwrap().iter().skip(1){
            assert_eq!(item["statusObservedAt"],"2026-09-22T12:01:00Z");assert_eq!(item["providerStatus"],"inprogress");assert_eq!(item["draft"],"preserved draft");
            let key=route(item).unwrap();let obligation=&saved["sync"]["pendingContext"][&key];
            assert_eq!(obligation["reason"],"status_changed");assert_eq!(obligation["canonicalItemId"],item["id"]);
            if !periodic["sync"]["pendingContext"][&key].is_null(){assert_eq!(obligation["queuedAt"],periodic["sync"]["pendingContext"][&key]["queuedAt"]);}
        }
        let mut oversized=head["items"].as_array().unwrap().clone();oversized.push(json!({"objectId":"extra","itemId":"extra"}));
        let mut invoked=false;assert!(db.change_status_observed(&oversized,|_|{invoked=true;Ok(())}).await.is_err());assert!(!invoked);assert_eq!(db.read().await.unwrap(),saved);
    }
    #[tokio::test]
    async fn complete_twelve_object_head_uses_real_scoped_status_storage(){
        let temp=tempfile::tempdir().unwrap();let db=Database::Sqlite(open_db(&temp.path().join("head.sqlite")).await.unwrap());exercise_complete_head_storage(&db).await;db.close().await;
    }
    #[tokio::test]
    #[ignore="requires an explicitly provisioned disposable synthetic PostgreSQL database"]
    async fn complete_twelve_object_head_postgres_scoped_storage(){
        let url=std::env::var("COMMUNITYHERO_FAST_STATUS_TEST_URL").expect("explicit disposable test database");
        assert_eq!(url,"postgresql://ch_migrate@127.0.0.1:55439/communityhero_fast_status_test_20260926","refusing non-test URL");
        let check=sqlx::postgres::PgPoolOptions::new().max_connections(1).connect(&url).await.unwrap();let name:String=sqlx::query_scalar("SELECT current_database()").fetch_one(&check).await.unwrap();assert_eq!(name,"communityhero_fast_status_test_20260926");check.close().await;
        let db=Database::postgres(&url).await.unwrap();exercise_complete_head_storage(&db).await;db.close().await;
    }
    #[tokio::test]
    async fn exact_closure_at_full_unknown_budget_persists_truth_and_canonical_obligation(){
        let temp=tempfile::tempdir().unwrap();let db=Database::Sqlite(open_db(&temp.path().join("closed.sqlite")).await.unwrap());
        db.change(|d|{d["items"]=fixture()["items"].clone();d["operations"]=json!([{"id":"uncertain","status":"unknown","itemId":"item-11391-42"}]);full_queue(d,CONTEXT_STORAGE_LIMIT);Ok(())}).await.unwrap();
        let before=db.read().await.unwrap();let binding=active_binding(&before).unwrap();let captured=capture(&before);let targets=[json!({"objectId":"11391","itemId":"42"})];
        db.change_status_observed(&targets,|d|admit(d,&binding,&captured,&reply(json!([observation("closed")])),Some(&targets),AT)).await.unwrap();
        let saved=db.read().await.unwrap();assert_eq!(saved["items"][0]["providerStatus"],"closed");assert_eq!(saved["items"][0]["draft"],"Human draft");assert_eq!(saved["operations"],before["operations"]);
        assert_eq!(unknown_count(&saved),CONTEXT_STORAGE_LIMIT);assert_eq!(queue_counts(&saved).2,CONTEXT_STORAGE_LIMIT+1);assert_eq!(saved["sync"]["contextBackpressure"]["canonicalObligations"],1);
        let entry=&saved["sync"]["pendingContext"][route(&observation("closed")).unwrap()];assert_eq!(entry["canonicalItemId"],"item-11391-42");assert_eq!(entry["status"],"deferred");assert_eq!(entry["connectorBinding"],binding.to_json());assert!(head_paused(&saved));
        assert!(db.change_status_observed(&[json!({"objectId":"foreign","itemId":"unseen"})],|d|admit(d,&binding,&capture(d),&json!({"kind":"open-status-head","items":[{"objectId":"foreign","itemId":"unseen","status":"new","observedAt":AT}],"errors":[]}),None,AT)).await.is_err());assert_eq!(db.read().await.unwrap(),saved);db.close().await;
    }
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
        let app=App{lifecycle_task_count: Default::default(),lifecycle_admission: Arc::new(crate::runtime_lifecycle_startup::Admission::fixture(crate::accounts::Profile::LikeAvto)),lifecycle_owner: Arc::new(crate::runtime_lifecycle_startup::Admission::fixture(crate::accounts::Profile::LikeAvto).identity().clone()),lifecycle_provider_token: Default::default(),lifecycle_work: Default::default(),media_discovery: Default::default(),preparation_wake: Default::default(),provider_session: Default::default(),account:crate::accounts::Profile::LikeAvto,navigation:crate::account_navigation::Navigation::root(),db:Database::Sqlite(db),gate:Arc::new(crate::writer_gate::WriterGate::default()),execution_gate:Arc::new(Mutex::new(())),preparation_workers: Default::default(),editorial_gate: Default::default(),assistant_gate:Arc::new(Mutex::new(())),assistant_chat_gate:Arc::new(Mutex::new(())),events,csrf:id(),auth:None,public_origin:None,external_writes:false,port:0,data:temp.path().to_owned(),bridge,node:PathBuf::from("C:/Users/hello/AppData/Local/Microsoft/WinGet/Packages/OpenJS.NodeJS.LTS_Microsoft.Winget.Source_8wekyb3d8bbwe/node-v24.15.0-win-x64/node.exe"),tasks:Arc::new(Mutex::new(HashMap::new())),bootstrap_cache:Arc::new(bootstrap_cache::Cache::default())};
        app.db.change(|d|crate::accounts::initialize(d,crate::accounts::Profile::LikeAvto)).await.unwrap();
        crate::runtime_lifecycle_app::initialize_app_fixture(&app).await.unwrap();
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
