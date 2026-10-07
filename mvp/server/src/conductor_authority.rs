//! A bounded company campaign delegates existing admissions; it never becomes
//! another sender or substitutes a local-owner actor for missing provenance.
use crate::*;
use operator_auth::Actor;
use std::collections::HashSet;
use std::sync::{OnceLock, Weak};
use tokio::sync::{OwnedRwLockReadGuard, OwnedRwLockWriteGuard, RwLock};

#[derive(Clone, Debug)]
pub(crate) struct Context {
    pub run_id: String,
    pub lease_generation: u64,
    pub actor: Actor,
}
tokio::task_local! { static CONTEXT: Context; }

pub(crate) fn current_context() -> Option<Context> {
    CONTEXT.try_with(Clone::clone).ok()
}
pub(crate) async fn with_context<T>(ctx: Context, future: impl std::future::Future<Output=T>) -> T {
    CONTEXT.scope(ctx, future).await
}
fn denied() -> ApiError {
    ApiError(StatusCode::FORBIDDEN, "Conductor grant paused, revoked, changed or outside its admitted scope".into())
}

/// Missing metadata is the explicit legacy generation, never a fresh UUID.
/// Once a working generation exists, old grants cannot silently repin to it.
pub(crate) fn workspace_generation(d:&Value)->ApiResult<Value> {
    let Some(value)=d.get("storageGeneration") else{return Ok(Value::Null)};
    let valid=value.as_str().and_then(|raw|uuid::Uuid::parse_str(raw).ok().map(|uuid|(raw,uuid)))
        .is_some_and(|(raw,uuid)|uuid.get_version_num()==4&&uuid.to_string()==raw);
    if !valid{return Err(conflict("Working storage generation is missing or malformed"));}
    Ok(value.clone())
}
pub(crate) fn check_workspace_generation(d:&Value,job:&Value)->ApiResult<()> {
    if job["conductor"]["grant"]["workspaceGeneration"]!=workspace_generation(d)?{return Err(denied());}
    Ok(())
}

/// This is only the grant, not the campaign lifecycle/checkpoint. Capture the
/// exact authorizer now; browser expiry does not later revoke admitted work.
pub(crate) fn create_grant(d: &Value, actor: &Actor, input: &Value) -> ApiResult<Value> {
    let workspace_generation=workspace_generation(d)?;
    if input.get("workspaceGeneration").is_some_and(|expected|*expected!=workspace_generation){return Err(denied());}
    let connector=active_binding(d)?;
    let binding = dispatch_authority::approval_binding(actor);
    // Exercise the established actor-binding validator, with no compatibility
    // fallback for a grant missing its own explicit binding.
    dispatch_authority::admit(&json!({"approvedBy":actor.public_json(),"approvalAuthority":binding}),actor)?;
    let mode=input["mode"].as_str().ok_or_else(||bad("Conductor mode required"))?;
    if !matches!(mode,"prepare"|"execute") {return Err(bad("Invalid conductor mode"));}
    let ids=input["scope"]["itemIds"].as_array().filter(|ids|!ids.is_empty()&&ids.len()<=5000)
        .ok_or_else(||bad("Conductor requires a fixed manifest of 1 to 5000 items"))?;
    let mut unique=HashSet::new();
    for key in ids {
        let key=key.as_str().filter(|id|!id.is_empty()).ok_or_else(||bad("Invalid conductor item ID"))?;
        if !unique.insert(key) {return Err(bad("Duplicate conductor item ID"));}
        let item=row(d,"items",key)?;
        bound_item(&connector,item)?;
    }
    let kinds=input["actionKinds"].as_array().ok_or_else(||bad("Conductor actionKinds required"))?;
    let mut actions=HashSet::new();
    for kind in kinds {
        let kind=kind.as_str().ok_or_else(||bad("Invalid conductor action kind"))?;
        if !matches!(kind,"reply_and_close"|"close"|"hide"|"delete")||!actions.insert(kind) {
            return Err(bad("Invalid or duplicate conductor action kind"));
        }
    }
    if mode=="execute"&&kinds.is_empty(){return Err(bad("Execute campaign requires action kinds"));}
    if mode=="execute" {
        let targets=ids.iter().map(|key|row(d,"items",key.as_str().unwrap()).cloned()).collect::<ApiResult<Vec<_>>>()?;
        connection_gate::fence_admission(d,&targets)?;
    }
    Ok(json!({"actor":actor.public_json(),"authorityGeneration":binding,"actionKinds":kinds,
        "workspaceGeneration":workspace_generation}))
}

pub(crate) fn actor_from_grant(job: &Value) -> ApiResult<Actor> {
    let grant=&job["conductor"]["grant"];
    let public=&grant["actor"];
    let authority=&grant["authorityGeneration"];
    let actor=Actor {id:required(public,"id")?.into(),name:required(public,"name")?.into(),
        role:required(public,"role")?.into(),csrf_token:String::new(),
        authority_generation:authority["generation"].as_str().map(str::to_owned)};
    if dispatch_authority::approval_binding(&actor)!=*authority {return Err(denied());}
    dispatch_authority::admit(&json!({"approvedBy":public,"approvalAuthority":authority}),&actor)?;
    Ok(actor)
}

fn validate(job: &Value, run: &str, generation: u64, action: &str, targets: &[Value]) -> ApiResult<Actor> {
    let campaign=&job["conductor"];
    if job["id"]!=run || job["kind"]!="conductor" || campaign["version"]!=1
        || campaign["desiredState"]!="running" || campaign["leaseGeneration"].as_u64()!=Some(generation)
        || generation==0 {return Err(denied());}
    let mode=campaign["mode"].as_str().ok_or_else(denied)?;
    if !matches!(mode,"prepare"|"execute"){return Err(denied());}
    let stage=matches!(action,"read"|"prepare"|"review"|"proposal");
    if (!stage && mode!="execute") || (!stage&&!matches!(action,"approval"|"execute")
        && !campaign["grant"]["actionKinds"].as_array().is_some_and(|kinds|kinds.iter().any(|kind|kind==action))) {
        return Err(denied());
    }
    let scope=campaign["scope"]["itemIds"].as_array().filter(|ids|!ids.is_empty()&&ids.len()<=5000).ok_or_else(denied)?;
    for target in targets {
        let key=if target.is_string(){target.as_str()}else{target["id"].as_str()}.ok_or_else(denied)?;
        if !scope.iter().any(|id|id==key){return Err(denied());}
        if let Some(binding)=target.get("connectorBinding") {
            if *binding!=job["connectorBinding"] {return Err(denied());}
        }
    }
    actor_from_grant(job)
}

fn same_actor(left: &Actor, right: &Actor) -> bool {
    dispatch_authority::approval_binding(left)==dispatch_authority::approval_binding(right)
        && left.id==right.id && left.role==right.role
}

/// Must run in the transaction that admits the new durable rows. A preflight
/// read alone is insufficient: pause/generation changes may win the writer.
pub(crate) fn fence_admission(d: &Value, action: &str, targets: &[Value]) -> ApiResult<Option<Context>> {
    let Some(ctx)=current_context() else {return Ok(None)};
    let job=row(d,"jobs",&ctx.run_id)?;
    check_workspace_generation(d,job)?;
    let actor=validate(job,&ctx.run_id,ctx.lease_generation,action,targets)?;
    if job["account"]!=d["account"] || job["connectorBinding"]!=active_binding(d)?.to_json()
        || !same_actor(&ctx.actor,&actor) {return Err(denied());}
    Ok(Some(ctx))
}
pub(crate) fn fence_actor(ctx: Option<&Context>, actor: &Actor) -> ApiResult<()> {
    if ctx.is_some_and(|ctx|!same_actor(&ctx.actor,actor)){return Err(denied());}
    Ok(())
}
pub(crate) fn tag(ctx: &Context, value: &mut Value) {
    value["conductorRunId"]=json!(ctx.run_id);
    value["grantGeneration"]=json!(ctx.lease_generation);
}
/// Wrap a scheduling reducer with its initial jobs length. Source selection
/// stays in the established reducer; its exact newly admitted recipients are
/// then checked and attributed before the same transaction can commit.
pub(crate) fn fence_new_jobs(d: &mut Value, first: usize) -> ApiResult<()> {
    if current_context().is_none(){return Ok(());}
    let jobs=list(d,"jobs").get(first..).ok_or_else(denied)?.to_vec();
    for job in jobs {
        let (action,targets)=match job["kind"].as_str() {
            Some("assistant") if job["purpose"]=="public_fact_followup" => ("prepare",fact_targets(d,&job)?),
            Some("assistant")=> ("prepare",job["prepareBundle"]["itemIds"].as_array().cloned().ok_or_else(denied)?),
            Some("editorial_review")=> {
                let refs=job["editorialReferences"].as_array().ok_or_else(denied)?;
                let targets=refs.iter().map(|r|row(d,"proposals",required(r,"id")?).map(|p|p["itemId"].clone())).collect::<ApiResult<Vec<_>>>()?;
                ("review",targets)
            },
            Some("execute")=> {
                let approval=row(d,"approvals",required(&job,"refId")?)?;
                ("execute",approval["proposals"].as_array().ok_or_else(denied)?.iter().map(|r|r["item"]["id"].clone()).collect())
            },
            _=>return Err(denied()),
        };
        if targets.is_empty(){return Err(denied());}
        let ctx=fence_admission(d,action,&targets)?.ok_or_else(denied)?;
        tag(&ctx,row_mut(d,"jobs",required(&job,"id")?)?);
    }
    Ok(())
}
fn fact_targets(d:&Value,job:&Value)->ApiResult<Vec<Value>>{
    let parent=row(d,"jobs",required(job,"parentPrepareJobId")?)?;
    let ids=job["requestedItemIds"].as_array().filter(|ids|!ids.is_empty()&&ids.len()<=100).ok_or_else(denied)?;
    let dependencies=job["factDependencyIds"].as_array().filter(|deps|deps.len()==ids.len()).ok_or_else(denied)?;
    let ctx=current_context().ok_or_else(denied)?;
    if parent["purpose"]!="engine_prepare"||parent["conductorRunId"]!=ctx.run_id
        ||parent["grantGeneration"].as_u64().is_none_or(|g|g==0||g>ctx.lease_generation)
        ||job["researchRequest"]["account"]!=d["account"]||job["researchRequest"]["factResearchContract"]!="scoped_source_v1"{
        return Err(denied());
    }
    let mut seen=HashSet::new();
    for id in ids {
        let item=id.as_str().ok_or_else(denied)?;
        if !seen.insert(item)||!parent["factFollowups"].as_array().is_some_and(|entries|entries.iter().any(|entry|
            entry["itemId"]==*id&&entry["kind"]=="missing_public_fact"&&dependencies.contains(&entry["id"])
            &&entry["binding"]["connectorBinding"]==active_binding(d).map(|v|v.to_json()).unwrap_or(Value::Null)
            &&entry["binding"]["declaration"]["publicQuery"]==job["researchRequest"]["query"])){
            return Err(denied());
        }
    }
    Ok(ids.clone())
}
pub(crate) fn require_attribution(ctx: Option<&Context>, value: &Value) -> ApiResult<()> {
    if value.get("conductorRunId").is_none() && value.get("grantGeneration").is_none(){return Ok(());}
    if !ctx.is_some_and(|ctx|value["conductorRunId"]==ctx.run_id
        && value["grantGeneration"].as_u64()==Some(ctx.lease_generation)){return Err(denied());}
    Ok(())
}
/// Local immutable intent may survive a supervisor epoch. This never admits a
/// previous operation for retry: callers still validate current proof, actor,
/// scope and grant, and stamp any newly admitted operation with the new epoch.
pub(crate) fn require_prior_attribution(ctx: Option<&Context>, value: &Value) -> ApiResult<()> {
    if value.get("conductorRunId").is_none()&&value.get("grantGeneration").is_none(){return Ok(());}
    if !ctx.is_some_and(|ctx|value["conductorRunId"]==ctx.run_id
        && value["grantGeneration"].as_u64().is_some_and(|epoch|epoch>0&&epoch<=ctx.lease_generation)) {
        return Err(denied());
    }
    Ok(())
}
pub(crate) fn fence_job_capture(d: &Value, job_id: &str, action: &str) -> ApiResult<()> {
    let job=row(d,"jobs",job_id)?;
    let ctx=current_context();
    require_attribution(ctx.as_ref(),job)?;
    if ctx.is_none(){return Ok(());}
    // A campaign cannot borrow an unrelated manually admitted model job.
    if job.get("conductorRunId").is_none(){return Err(denied());}
    let targets=if job["kind"]=="assistant"&&job["purpose"]=="public_fact_followup"{fact_targets(d,job)?}
        else if let Some(ids)=job["prepareBundle"]["itemIds"].as_array(){ids.clone()}
        else if let Some(refs)=job["editorialReferences"].as_array(){
            refs.iter().map(|r|row(d,"proposals",required(r,"id")?).map(|p|p["itemId"].clone())).collect::<ApiResult<Vec<_>>>()?
        }else{return Err(denied());};
    if targets.is_empty(){return Err(denied());}
    fence_admission(d,action,&targets)?;Ok(())
}
/// Observation of the original uncertain operation is allowed across epochs;
/// this creates only a new readback job and never retags the operation/action.
pub(crate) fn fence_readback_claim(d: &Value, op: &Value, requested_by: Option<&Value>, job: &mut Value) -> ApiResult<()> {
    let Some(ctx)=current_context() else{return Ok(());};
    if op.get("conductorRunId").is_none()||op["target"]["id"]!=op["itemId"]{return Err(denied());}
    require_prior_attribution(Some(&ctx),op)?;
    fence_admission(d,"read",&[op["target"].clone()])?;
    let actor=ctx.actor.public_json();
    if requested_by.is_none_or(|by|by["id"]!=actor["id"]||by["role"]!=actor["role"])
        ||op["dispatchAuthority"]["approved"]!=dispatch_authority::approval_binding(&ctx.actor)
        ||op["dispatchAuthority"]["executed"]!=dispatch_authority::approval_binding(&ctx.actor)
        ||job["kind"]!="reconcile"||job["refId"]!=op["id"]||job["readbackOnly"]!=true {
        return Err(denied());
    }
    tag(&ctx,job);Ok(())
}

/// Recover context only from a child's durable attribution and the original
/// durable grant. Missing, paused or superseded provenance never gets an actor
/// synthesized by the supervisor.
pub(crate) async fn context_for_job(app: &App, job_id: &str) -> ApiResult<Option<Context>> {
    let child=app.db.read_job(job_id).await?.ok_or_else(denied)?;
    if child.get("conductorRunId").is_none()&&child.get("grantGeneration").is_none(){return Ok(None);}
    let run=required(&child,"conductorRunId")?.to_owned();
    let generation=child["grantGeneration"].as_u64().ok_or_else(denied)?;
    let actor=authorize(app,&run,generation,"read",&[]).await?;
    Ok(Some(Context{run_id:run,lease_generation:generation,actor}))
}

pub(crate) async fn authorize(app: &App, run: &str, generation: u64, action: &str, targets: &[Value]) -> ApiResult<Actor> {
    let job=app.db.read_job(run).await?.ok_or_else(denied)?;
    let actor=validate(&job,run,generation,action,targets)?;
    check_control(app,&job).await?;
    Ok(actor)
}
pub(crate) async fn check_control(app: &App, job: &Value) -> ApiResult<()> {
    if job["kind"]!="conductor"||job["conductor"]["version"]!=1||job["account"]!=app.account.display(){return Err(denied());}
    if job["conductor"]["grant"]["workspaceGeneration"]!=json!(app.db.read_working_generation().await?){return Err(denied());}
    let binding=ConnectorBinding::from_json(&job["connectorBinding"]).map_err(|_|denied())?;
    binding.validate_scope("local-pilot",app.account.display()).map_err(|_|denied())?;
    let actor=actor_from_grant(job)?;
    let authority=dispatch_authority::approval_binding(&actor);
    dispatch_authority::check(app,&json!({"approvedBy":actor.public_json(),"executedBy":actor.public_json(),
        "dispatchAuthority":{"approved":authority,"executed":authority}})).await?;
    Ok(())
}
pub(crate) async fn check_read(app: &App, ctx: &Context) -> ApiResult<()> {
    let actor=authorize(app,&ctx.run_id,ctx.lease_generation,"read",&[]).await?;
    if !same_actor(&actor,&ctx.actor){return Err(denied());} Ok(())
}
pub(crate) fn sanitize_job(job: &mut Value) {
    if job.get("factFollowups").is_some(){
        job["factDependencies"]=crate::fact_followup::summary(job);
        job.as_object_mut().unwrap().remove("factFollowups");
    }
    if job["purpose"]=="public_fact_followup" {
        for key in ["researchRequest","researchResult","factSignatures"]{job.as_object_mut().unwrap().remove(key);}
    }
    if let Some(grant)=job.get_mut("conductor").and_then(|c|c.get_mut("grant")).and_then(Value::as_object_mut){
        grant.remove("authorityGeneration");
    }
}
/// Epoch changes fence workers; they do not regrant another actor, company,
/// connector, recipient manifest or action budget under the original run ID.
pub(crate) fn validate_change(before: &Value, after: &Value) -> ApiResult<()> {
    for original in before["jobs"].as_array().into_iter().flatten().filter(|job|job["kind"]=="conductor") {
        let changed=after["jobs"].as_array().into_iter().flatten().find(|job|job["id"]==original["id"])
            .ok_or_else(||internal("Conductor grant history cannot be deleted"))?;
        if ["kind","purpose","account","connectorBinding"].iter().any(|field|original[*field]!=changed[*field])
            ||["version","mode","scope","limits","grant","startPayloadHash"].iter()
                .any(|field|original["conductor"][*field]!=changed["conductor"][*field]) {
            return Err(internal("Conductor grant identity and admitted scope are immutable"));
        }
        let a=original["conductor"]["leaseGeneration"].as_u64().ok_or_else(denied)?;
        let b=changed["conductor"]["leaseGeneration"].as_u64().ok_or_else(denied)?;
        if b<a||b==0{return Err(internal("Conductor generation cannot regress"));}
        if changed["conductor"]["desiredState"]=="running"&&!changed["conductor"]["connectionContinuation"].is_null() {
            conductor::connection_continuation::validate_marker(&changed["conductor"]["connectionContinuation"],changed)?;
        }
    }
    Ok(())
}
pub(crate) async fn check_dispatch(app: &App, op: &Value) -> ApiResult<()> {
    if op.get("conductorRunId").is_none() && op.get("grantGeneration").is_none(){return Ok(());}
    let run=op["conductorRunId"].as_str().ok_or_else(denied)?;
    let generation=op["grantGeneration"].as_u64().ok_or_else(denied)?;
    let action=op["action"]["action"].as_str().ok_or_else(denied)?;
    let actor=authorize(app,run,generation,action,&[op["target"].clone()]).await?;
    if op["dispatchAuthority"]["executed"]!=dispatch_authority::approval_binding(&actor){return Err(denied());}
    Ok(())
}

// Locks are transient execution barriers, never a grant cache or durable
// lease. Shared permits retain independent dispatch parallelism. Transitions
// can publish pausing first, then exclusively wait for already-sent calls.
type Barriers=std::sync::Mutex<HashMap<String,Weak<RwLock<()>>>>;
static BARRIERS:OnceLock<Barriers>=OnceLock::new();
fn barrier(app: &App, run: &str) -> Arc<RwLock<()>> {
    let key=json!([app.data.to_string_lossy(),app.account.key(),run]).to_string();
    let mut barriers=BARRIERS.get_or_init(Default::default).lock().unwrap_or_else(|e|e.into_inner());
    barriers.retain(|_,value|value.strong_count()>0);
    if let Some(value)=barriers.get(&key).and_then(Weak::upgrade){return value;}
    let value=Arc::new(RwLock::new(()));barriers.insert(key,Arc::downgrade(&value));value
}
pub(crate) async fn dispatch_guard(app: &App, run: &str) -> OwnedRwLockReadGuard<()> {
    barrier(app,run).read_owned().await
}
pub(crate) struct TransitionGuard {
    _company: tokio::sync::OwnedMutexGuard<()>,
    _run: OwnedRwLockWriteGuard<()>,
}
pub(crate) async fn transition_guard(app: &App, run: &str) -> TransitionGuard {
    let barrier = barrier(app,run);
    loop {
        let company = connection_gate::lock(app).await;
        if let Ok(run) = barrier.clone().try_write_owned() {
            return TransitionGuard {_company:company,_run:run};
        }
        // Do not retain M while waiting for an existing provider call. The
        // temporary readiness guard is released BEFORE the next M acquisition.
        drop(company);
        let readiness = barrier.clone().write_owned().await;
        drop(readiness);
    }
}

#[cfg(test)]
#[path="conductor_authority_tests.rs"]
mod tests;
