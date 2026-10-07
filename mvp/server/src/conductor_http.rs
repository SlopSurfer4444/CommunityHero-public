//! Private, finite conductor RPC. This router is never mounted on public HTTP.
//! The capability delegates a captured grant, not a browser/owner session.
use crate::*;
use axum::{Extension, body::Bytes, extract::Query, http::HeaderMap};
use sha2::{Digest, Sha256};
use std::collections::HashSet;

const MAX_RPC_BYTES: usize = 2 * 1024 * 1024;
#[derive(Clone)]
struct RpcState {
    app: App,
    launch: Arc<conductor::Launch>,
    context: conductor_authority::Context,
    capability_hash: [u8; 32],
    observed_jobs: Arc<Mutex<HashSet<String>>>,
    proposal_hints: Arc<Mutex<ProposalHints>>,
    active: Arc<std::sync::atomic::AtomicBool>,
}
#[derive(Default)]
struct ProposalHints {
    seeded: bool,
    item_by_proposal: HashMap<String,String>,
    item_by_operation: HashMap<String,String>,
    items_by_post: HashMap<String,HashSet<String>>,
}
pub(crate) struct Server {
    pub rpc_url: String,
    task: tokio::task::JoinHandle<()>,
    active: Arc<std::sync::atomic::AtomicBool>,
}
impl Drop for Server { fn drop(&mut self) {
    self.active.store(false,std::sync::atomic::Ordering::Release);
    self.task.abort();
} }

pub(crate) async fn start(app: &App, launch: Arc<conductor::Launch>, capability: &str) -> ApiResult<Server> {
    let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).await
        .map_err(|_| internal("Conductor RPC unavailable"))?;
    let address = listener.local_addr().map_err(|_| internal("Conductor RPC address unavailable"))?;
    let context = conductor_authority::Context { run_id: launch.run_id.clone(),
        lease_generation: launch.lease_generation, actor: launch.actor.clone() };
    let active=Arc::new(std::sync::atomic::AtomicBool::new(true));
    let state = RpcState { app: app.clone(), launch, context,
        capability_hash: Sha256::digest(capability.as_bytes()).into(),
        observed_jobs: Arc::new(Mutex::new(HashSet::new())),proposal_hints:Arc::new(Mutex::new(ProposalHints::default())),active:active.clone() };
    let router = Router::new().route("/rpc", post(rpc))
        .layer(DefaultBodyLimit::max(MAX_RPC_BYTES)).with_state(state);
    let task = tokio::spawn(async move { let _ = axum::serve(listener, router).await; });
    Ok(Server { rpc_url: format!("http://{address}/rpc"), task,active })
}

fn denied() -> ApiError { ApiError(StatusCode::FORBIDDEN, "Conductor RPC authority denied".into()) }
fn capability_matches(expected: &[u8; 32], observed: &str) -> bool {
    let actual: [u8; 32] = Sha256::digest(observed.as_bytes()).into();
    expected.iter().zip(actual).fold(0u8, |difference, (a, b)| difference | (a ^ b)) == 0
}
fn authenticate(state: &RpcState, headers: &HeaderMap) -> ApiResult<()> {
    let text = |name:&str| headers.get(name).and_then(|v| v.to_str().ok());
    let capability = text("x-conductor-capability").filter(|s| s.len() <= 256).ok_or_else(denied)?;
    if !state.active.load(std::sync::atomic::Ordering::Acquire) || !capability_matches(&state.capability_hash, capability)
        || text("x-conductor-run") != Some(state.context.run_id.as_str())
        || text("x-conductor-generation").and_then(|s| s.parse::<u64>().ok()) != Some(state.context.lease_generation) {
        return Err(denied());
    }
    Ok(())
}
fn fields(value: &Value, allowed: &[&str], required: &[&str]) -> ApiResult<()> {
    let object = value.as_object().ok_or_else(|| bad("Conductor RPC object required"))?;
    if object.keys().any(|key| !allowed.contains(&key.as_str()))
        || required.iter().any(|key| !object.contains_key(*key)) {
        return Err(bad("Invalid conductor RPC fields"));
    }
    Ok(())
}
fn key<'a>(args: &'a Value, field: &str) -> ApiResult<&'a str> {
    args[field].as_str().filter(|s| !s.is_empty() && s.len() <= 256 && !s.chars().any(char::is_control))
        .ok_or_else(|| bad("Invalid conductor RPC identifier"))
}
fn ids(args: &Value) -> ApiResult<Vec<String>> {
    ids_limit(args, 100)
}
fn ids_limit(args: &Value, maximum: usize) -> ApiResult<Vec<String>> {
    let values = args["itemIds"].as_array().filter(|v| !v.is_empty() && v.len() <= maximum)
        .ok_or_else(|| bad("Conductor item selection exceeds its bound"))?;
    let mut seen = HashSet::new();
    values.iter().map(|value| {
        let id = value.as_str().filter(|s| !s.is_empty() && s.len() <= 128 && !s.contains(',') && !s.chars().any(char::is_control))
            .ok_or_else(|| bad("Invalid conductor item ID"))?;
        if !seen.insert(id) { return Err(bad("Duplicate conductor item ID")); }
        Ok(id.to_owned())
    }).collect()
}
fn in_scope(state: &RpcState, selected: &[String]) -> ApiResult<()> {
    if selected.iter().any(|id| !state.launch.scope_item_ids.contains(id)) { return Err(denied()); }
    Ok(())
}
async fn authorize_items(state: &RpcState, action: &str, selected: &[String]) -> ApiResult<()> {
    in_scope(state, selected)?;
    let targets: Vec<Value> = selected.iter().map(|id| json!(id)).collect();
    conductor_authority::authorize(&state.app, &state.context.run_id, state.context.lease_generation, action, &targets).await?;
    Ok(())
}
async fn scope_review(state: &RpcState, selected: &[String]) -> ApiResult<Value> {
    authorize_items(state, "read", selected).await?;
    let review=state.app.db.read_bounded_review(selected, state.app.account.display()).await?;
    remember_proposals(state,&review).await?;
    Ok(review)
}
async fn remember_proposals(state:&RpcState,review:&Value)->ApiResult<()> {
    let mut hints=state.proposal_hints.lock().await;
    for item in list(review,"items") {
        let id=required(item,"id")?;
        if !state.launch.scope_item_ids.iter().any(|selected|selected==id){return Err(denied());}
        if let Some(post)=item["postId"].as_str().filter(|id|!id.is_empty()) {
            hints.items_by_post.entry(post.to_owned()).or_default().insert(id.to_owned());
        }
    }
    for proposal in list(review,"proposals") {
        let id=required(proposal,"id")?; let item=required(proposal,"itemId")?;
        if !state.launch.scope_item_ids.iter().any(|id|id==item) { return Err(denied()); }
        if hints.item_by_proposal.get(id).is_some_and(|old|old!=item){return Err(denied());}
        hints.item_by_proposal.insert(id.to_owned(),item.to_owned());
    }
    for operation in list(review,"operations") {
        let Some(item)=operation["itemId"].as_str().filter(|item|state.launch.scope_item_ids.iter().any(|selected|selected==item)) else{continue;};
        let id=required(operation,"id")?;
        if hints.item_by_operation.get(id).is_some_and(|old|old!=item){return Err(denied());}
        hints.item_by_operation.insert(id.to_owned(),item.to_owned());
    }
    Ok(())
}
async fn authorize_post(state:&RpcState,post:&str,action:&str)->ApiResult<()> {
    let missing={let hints=state.proposal_hints.lock().await;!hints.items_by_post.contains_key(post)};
    if missing {
        {let mut hints=state.proposal_hints.lock().await;if hints.seeded{return Err(denied());}hints.seeded=true;}
        for chunk in state.launch.scope_item_ids.chunks(100){scope_review(state,chunk).await?;}
    }
    let hinted={let hints=state.proposal_hints.lock().await;
        hints.items_by_post.get(post).ok_or_else(denied)?.iter().cloned().collect::<Vec<_>>()};
    let mut selected=Vec::new();
    for chunk in hinted.chunks(100) {
        let review=scope_review(state,chunk).await?;
        selected.extend(list(&review,"items").iter().filter(|i|i["postId"]==post).filter_map(|i|i["id"].as_str().map(str::to_owned)));
    }
    if selected.is_empty(){return Err(denied());}
    authorize_items(state,action,&selected).await
}
async fn proposal_scope(state: &RpcState, refs: &Value, id_field: &str, action: &str) -> ApiResult<()> {
    let refs = refs.as_array().filter(|v| !v.is_empty() && v.len() <= 100)
        .ok_or_else(|| bad("Conductor requires exact proposal references"))?;
    let wanted: HashSet<&str> = refs.iter().map(|r| key(r, id_field)).collect::<ApiResult<_>>()?;
    if wanted.len() != refs.len() { return Err(bad("Duplicate conductor proposal reference")); }
    // Hints only choose a small current read; they never authorize references.
    // One initial cohort scan is allowed if no bootstrap has seeded the map.
    let unknown={let hints=state.proposal_hints.lock().await; wanted.iter().any(|id|!hints.item_by_proposal.contains_key(*id))};
    if unknown {
        {let mut hints=state.proposal_hints.lock().await; if hints.seeded{return Err(denied());} hints.seeded=true;}
        for chunk in state.launch.scope_item_ids.chunks(100) {scope_review(state,chunk).await?;}
    }
    let mut selected={let hints=state.proposal_hints.lock().await;
        wanted.iter().map(|id|hints.item_by_proposal.get(*id).cloned().ok_or_else(denied)).collect::<ApiResult<Vec<_>>>()?};
    selected.sort(); selected.dedup();
    let mut found = HashSet::new();
    for chunk in selected.chunks(100) {
        let review = scope_review(state,chunk).await?;
        for proposal in list(&review, "proposals") {
            if let Some(id) = proposal["id"].as_str().filter(|id| wanted.contains(id)) {
                if !found.insert(id.to_owned()) { return Err(denied()); }
            }
        }
    }
    if found.len() != wanted.len() { return Err(denied()); }
    selected.sort(); selected.dedup();
    authorize_items(state, action, &selected).await
}
fn owned(state: &RpcState, record: &Value) -> bool {
    // Resuming a campaign may inspect its older-generation results; only the
    // current generation can admit new work or dispatch under authority fences.
    record["conductorRunId"] == state.context.run_id && record["grantGeneration"].as_u64().is_some_and(|g| g > 0 && g <= state.context.lease_generation)
}
async fn original_operation(state:&RpcState,id:&str)->ApiResult<Value> {
    let missing={let hints=state.proposal_hints.lock().await;!hints.item_by_operation.contains_key(id)};
    if missing {
        {let mut hints=state.proposal_hints.lock().await;if hints.seeded{return Err(denied());}hints.seeded=true;}
        for chunk in state.launch.scope_item_ids.chunks(100){scope_review(state,chunk).await?;}
    }
    let item={let hints=state.proposal_hints.lock().await;hints.item_by_operation.get(id).cloned().ok_or_else(denied)?};
    let view=scope_review(state,std::slice::from_ref(&item)).await?;
    let rows:Vec<_>=list(&view,"operations").iter().filter(|operation|operation["id"]==id).collect();
    if rows.len()!=1 || rows[0]["itemId"]!=item || !owned(state,rows[0]) {return Err(denied());}
    // The existing readback claimant validates the original provider route and
    // action identity again; no current item/proposal is substituted here.
    Ok(rows[0].clone())
}
async fn owned_job(state: &RpcState, id: &str) -> ApiResult<Value> {
    let job = state.app.db.read_job_public(id).await?.ok_or_else(denied)?;
    if owned(state, &job) || id == state.context.run_id || state.observed_jobs.lock().await.contains(id) {
        return Ok(job);
    }
    // Media work is shared within this company; only evidence for a selected
    // post is observable, never arbitrary workspace jobs or another campaign.
    if job["kind"]=="target_refresh" && state.launch.scope_item_ids.iter().any(|id|job["refId"]==*id) {
        return Ok(job);
    }
    if job["kind"]=="media" {
        if let Some(post)=job["refId"].as_str() {authorize_post(state,post,"read").await?;return Ok(job);}
    }
    if job["kind"]=="reconcile" && job["readbackOnly"]==true {
        original_operation(state,key(&job,"refId")?).await?;return Ok(job);
    }
    Err(denied())
}
async fn bootstrap_scoped(state: &RpcState) -> ApiResult<Value> {
    // Preserve the CLI coverage/policy contract without transferring unrelated
    // drafts, author conversations, operation histories or model requests.
    let metadata = state.app.db.read_conductor_bootstrap_policy().await?;
    let mut view = json!({"account":state.app.account.display(), "connectorBinding":metadata["connectorBinding"],
        "sync":metadata["sync"], "companyKnowledgeAuthority":metadata["companyKnowledgeAuthority"],
        "items":[],"posts":[],"branches":[],"proposals":[],"operations":[],"jobs":[],
        "materials":metadata["materials"]});
    let mut seen:HashMap<&str,HashSet<String>>=HashMap::new();
    for chunk in state.launch.scope_item_ids.chunks(100) {
        let review = scope_review(state, chunk).await?;
        for collection in ["items","posts","branches","proposals","operations"] {
            for value in list(&review, collection) {
                if seen.entry(collection).or_default().insert(required(value,"id")?.to_owned()) {
                    list_mut(&mut view, collection).push(value.clone());
                }
            }
        }
    }
    state.proposal_hints.lock().await.seeded=true;
    let mut jobs = state.observed_jobs.lock().await.iter().cloned().collect::<Vec<_>>();
    jobs.push(state.context.run_id.clone());
    for id in jobs { if let Ok(mut job) = owned_job(state, &id).await { sanitize_bootstrap_job(&mut job); list_mut(&mut view,"jobs").push(job); } }
    dispatch_authority::sanitize_view(&mut view);
    Ok(view)
}

async fn dispatch(state: &RpcState, operation: &str, args: Value) -> ApiResult<Value> {
    let app = &state.app;
    let actor = &state.context.actor;
    let value = match operation {
        "health" => { fields(&args,&[],&[])?; json!({"status":"ok"}) },
        "engineStatus" => {
            fields(&args,&[],&[])?;
            let mut value=accounts::status(State(app.clone())).await?.0;
            value["connectionDependency"]=conductor::connection_continuation::dependency_dto(app,&state.launch,Value::Null,false).await?;value
        },
        "connectionDependency" => {
            fields(&args,&["approvalId","requestId"],&["approvalId","requestId"])?;
            let canonical=app.read().await?;let approval=row(&canonical,"approvals",key(&args,"approvalId")?)?;
            if !owned(state,approval){return Err(denied());}
            proposal_scope(state,&approval["proposals"],"id","read").await?;
            conductor::connection_continuation::dependency_for_rejection(app,&state.launch,key(&args,"approvalId")?,key(&args,"requestId")?).await?
        },
        "bootstrap" => { fields(&args,&[],&[])?; bootstrap_scoped(state).await? },
        "reviewItems" => { fields(&args,&["itemIds"],&["itemIds"])?; scope_review(state,&ids(&args)?).await? },
        "selectPrepareFamilies" => {
            fields(&args,&["itemIds","batchSize","maxBatches"],&["itemIds","batchSize","maxBatches"])?;
            let selected=ids_limit(&args,5000)?;
            authorize_items(state,"prepare",&selected).await?;
            prepare_plan::conductor_family_windows(State(app.clone()),Json(args)).await?.0
        },
        "resolvePublicFacts" => {
            fields(&args,&["prepareJobId","itemIds"],&["prepareJobId","itemIds"])?;
            let selected=ids(&args)?;
            authorize_items(state,"prepare",&selected).await?;
            owned_job(state,key(&args,"prepareJobId")?).await?;
            fact_followup::resolve(State(app.clone()),Extension(actor.clone()),Json(args)).await?.0
        },
        "planPrepare"|"prepare" => {
            fields(&args,&["itemIds","instruction","requestId"],&["itemIds"])?;
            let selected = ids(&args)?; authorize_items(state,"prepare",&selected).await?;
            if operation == "prepare" {
                key(&args,"requestId")?;
                engine_prepare::prepare(State(app.clone()),Extension(actor.clone()),Json(args)).await?.0
            } else { prepare_plan::conductor_plan(State(app.clone()),Json(args)).await?.0 }
        },
        "sync" => {
            fields(&args,&[],&[])?;
            authorize_items(state,"prepare",&[]).await?;
            sync(State(app.clone()),Json(json!({}))).await?.0
        },
        "importMaterials" => {
            fields(&args,&[],&[])?; authorize_items(state,"prepare",&[]).await?;
            materials_import(State(app.clone())).await?.0
        },
        "localAdmission" => {
            fields(&args,&["kind","requestId"],&["kind","requestId"])?;
            let kind=key(&args,"kind")?; let request=key(&args,"requestId")?;
            if !matches!(kind,"prepare"|"approval"|"execute"|"editorial"|"editorial-repair") { return Err(bad("Invalid conductor admission kind")); }
            if let Some(receipt) = app.db.read_local_admission_receipt(kind,request).await? {
                if !owned(state,&receipt) { return Err(denied()); }
            }
            local_admission::lookup(State(app.clone()),Extension(actor.clone()),Path((kind.to_owned(),request.to_owned()))).await?.0
        },
        "editorialReview" => {
            fields(&args,&["proposals","requestId","fresh"],&["proposals","requestId","fresh"])?;
            if args["fresh"] != true { return Err(bad("Conductor editorial must be fresh")); }
            proposal_scope(state,&args["proposals"],"id","review").await?;
            editorial_endpoint::post(State(app.clone()),Extension(actor.clone()),Json(args)).await?.0
        },
        "editorialRepair" => {
            fields(&args,&["reviewJobId","expected","requestId"],&["reviewJobId","expected","requestId"])?;
            let job=key(&args,"reviewJobId")?.to_owned(); owned_job(state,&job).await?;
            proposal_scope(state,&args["expected"],"proposalId","proposal").await?;
            let body=json!({"expected":args["expected"],"requestId":args["requestId"]});
            editorial_repair::post(State(app.clone()),Extension(actor.clone()),Path(job),Json(body)).await?.0
        },
        "approval" => {
            fields(&args,&["proposals","requestId"],&["proposals","requestId"])?;
            proposal_scope(state,&args["proposals"],"id","approval").await?;
            approval_new(State(app.clone()),Extension(actor.clone()),Json(args)).await?.0
        },
        "execute" => {
            fields(&args,&["approvalId","requestId"],&["approvalId","requestId"])?;
            let id=key(&args,"approvalId")?.to_owned();
            // Existing admission repeats scope and attribution in its writer.
            let canonical=app.read().await?; let approval=row(&canonical,"approvals",&id)?;
            if !owned(state,approval) { return Err(denied()); }
            proposal_scope(state,&approval["proposals"],"id","execute").await?;
            execute_admission::run(app.clone(),actor.clone(),id,json!({"requestId":args["requestId"]})).await?.0
        },
        "job" => {
            fields(&args,&["jobId"],&["jobId"])?;
            let id=key(&args,"jobId")?; owned_job(state,id).await?;
            let mut value=engine_api::job(State(app.clone()),Path(id.to_owned())).await?.0;
            if value.get("conductor").is_some(){conductor_authority::sanitize_job(&mut value);}
            value
        },
        "reconcile" => {
            fields(&args,&["operationId"],&["operationId"])?;
            let operation=key(&args,"operationId")?;
            original_operation(state,operation).await?;
            // Only the established observation-only handler. It creates a
            // readback job for this original operation, never a send operation.
            reconcile(State(app.clone()),Extension(actor.clone()),Path(operation.to_owned())).await?.0
        },
        "refreshContext" => {
            fields(&args,&["itemId"],&["itemId"])?;
            let id=key(&args,"itemId")?.to_owned(); authorize_items(state,"prepare",std::slice::from_ref(&id)).await?;
            target_refresh::start(State(app.clone()),Path(id),Bytes::new()).await?.0
        },
        "media" => {
            fields(&args,&["postId"],&["postId"])?; let post=key(&args,"postId")?;
            authorize_post(state,post,"prepare").await?;
            media_queue::request(app,post,actor).await?
        },
        "mediaStatus" => {
            fields(&args,&["postId"],&["postId"])?;let post=key(&args,"postId")?;
            authorize_post(state,post,"read").await?;
            media_status::get(State(app.clone()),Extension(actor.clone()),Path(post.to_owned()),Query(HashMap::new())).await?.0
        },
        "report" => {
            fields(&args,&["report"],&["report"])?;
            conductor::record_child_report(app,&state.launch,&args["report"]).await?; json!({"recorded":true})
        },
        _ => return Err(bad("Unsupported conductor RPC operation")),
    };
    if let Some(id)=value["jobId"].as_str() { state.observed_jobs.lock().await.insert(id.to_owned()); }
    Ok(value)
}
async fn rpc(State(state): State<RpcState>, headers: HeaderMap, Json(body): Json<Value>) -> Json<Value> {
    // HTTP 200 carries the original handler status in an explicit envelope.
    // A lost envelope is still an unknown mutation to the existing CLI client.
    let outcome=async {
        authenticate(&state,&headers)?;
        fields(&body,&["operation","args"],&["operation","args"])?;
        conductor_authority::check_read(&state.app,&state.context).await?;
        let operation=key(&body,"operation")?;
        conductor_authority::with_context(state.context.clone(),dispatch(&state,operation,body["args"].clone())).await
    }.await;
    match outcome {
        Ok(body) => Json(json!({"status":200,"body":body})),
        Err(ApiError(status,message)) => Json(json!({"status":status.as_u16(),"body":{"error":message}})),
    }
}

#[cfg(test)]
#[path="conductor_transport_tests.rs"]
mod tests;
