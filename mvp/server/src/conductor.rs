//! Engine-owned lifecycle for a fixed, company-scoped autonomous campaign.
//! The child owns workflow checkpoints, never the canonical effect ledger.
use crate::*;
use axum::Extension;
use sha2::{Digest, Sha256};
use std::collections::HashSet;

pub(crate) const MAX_CAMPAIGN_ITEMS: usize = 5000;

#[path="conductor_connection_continuation.rs"]
pub(crate) mod connection_continuation;
pub(crate) use connection_continuation::{execute_reevaluate, has_deferred_connection_work, wake_deferred};

pub(crate) struct Launch {
    pub(crate) run_id: String,
    pub(crate) lease_generation: u64,
    pub(crate) actor: operator_auth::Actor,
    pub(crate) account: String,
    pub(crate) base_url: String,
    pub(crate) checkpoint_path: PathBuf,
    pub(crate) scope_item_ids: Vec<String>,
    pub(crate) mode: String,
    pub(crate) max_repair_rounds: u64,
    pub(crate) max_cycles: u64,
    pub(crate) batch_size: u64,
    pub(crate) cutoff_utc: Option<String>,
    pub(crate) resume: bool,
    pub(crate) workspace_generation: Option<String>,
    pub(crate) connection_binding: Value,
    // The selected claim is retained across actual launch/cleanup. In
    // particular firstLaunch is not re-inferred from childEverStarted later.
    pub(crate) continuation_claim: Value,
}

fn bounded_integer(input: &Value, key: &str, default: u64, maximum: u64) -> ApiResult<u64> {
    match input.get(key) {
        None => Ok(default),
        Some(value) => value.as_u64().filter(|n| *n <= maximum && (*n > 0 || key == "maxRepairRounds"))
            .ok_or_else(|| bad("Invalid conductor limit")),
    }
}

pub(crate) fn parse(body: &Value) -> ApiResult<Value> {
    let fields = body.as_object().ok_or_else(|| bad("Conductor requires an object"))?;
    if fields.keys().any(|key| !matches!(key.as_str(), "requestId" | "scope" | "mode" | "actionKinds" | "limits" | "workspaceGeneration")) {
        return Err(bad("Conductor contains unsupported fields"));
    }
    let request = required(body, "requestId")?;
    if request.len() > 160 || !request.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_') {
        return Err(bad("Invalid conductor requestId"));
    }
    let scope = body["scope"].as_object().ok_or_else(|| bad("Conductor scope required"))?;
    if scope.keys().any(|key| !matches!(key.as_str(), "itemIds" | "cutoffUtc")) {
        return Err(bad("Conductor scope contains unsupported fields"));
    }
    let ids = body["scope"]["itemIds"].as_array().filter(|ids| !ids.is_empty() && ids.len() <= MAX_CAMPAIGN_ITEMS)
        .ok_or_else(|| bad("Conductor requires 1 to 5000 fixed item IDs"))?;
    let mut unique = HashSet::new();
    for value in ids {
        let key = value.as_str().filter(|s| !s.is_empty() && s.len() <= 128 && s.trim() == *s
            && !s.contains(',') && !s.chars().any(char::is_control)).ok_or_else(|| bad("Invalid conductor item ID"))?;
        if !unique.insert(key) { return Err(bad("Duplicate conductor item ID")); }
    }
    if let Some(value) = scope.get("cutoffUtc") {
        let raw = value.as_str().ok_or_else(|| bad("Invalid conductor cutoff"))?;
        let parsed = chrono::DateTime::parse_from_rfc3339(raw).map_err(|_| bad("Invalid conductor cutoff"))?;
        if raw.trim() != raw || parsed.timestamp_subsec_nanos() != 0 { return Err(bad("Invalid conductor cutoff")); }
    }
    if !matches!(body["mode"].as_str(), Some("prepare" | "execute")) { return Err(bad("Invalid conductor mode")); }
    if let Some(generation)=body.get("workspaceGeneration") {
        if !generation.is_null() {
            let mut scope=json!({});scope["storageGeneration"]=generation.clone();
            conductor_authority::workspace_generation(&scope).map_err(|_|bad("Invalid conductor workspaceGeneration"))?;
        }
    }
    let limits = body.get("limits").cloned().unwrap_or_else(|| json!({}));
    let fields = limits.as_object().ok_or_else(|| bad("Conductor limits require an object"))?;
    if fields.keys().any(|key| !matches!(key.as_str(), "maxRepairRounds" | "maxCycles" | "batchSize")) {
        return Err(bad("Conductor limits contain unsupported fields"));
    }
    let mut normalized = body.clone();
    if let Some(cutoff)=body["scope"]["cutoffUtc"].as_str() {
        let parsed=chrono::DateTime::parse_from_rfc3339(cutoff).map_err(|_|bad("Invalid conductor cutoff"))?;
        normalized["scope"]["cutoffUtc"]=json!(parsed.with_timezone(&chrono::Utc).to_rfc3339_opts(chrono::SecondsFormat::Secs,true));
    }
    normalized["limits"] = json!({
        "maxRepairRounds": bounded_integer(&limits, "maxRepairRounds", 2, 3)?,
        "maxCycles": bounded_integer(&limits, "maxCycles", 10_000, 50_000)?,
        "batchSize": bounded_integer(&limits, "batchSize", 60, 100)?,
    });
    Ok(normalized)
}

fn start_hash(body: &Value, actor: &operator_auth::Actor) -> String {
    format!("{:x}", Sha256::digest(json!({"body":body,"authority":dispatch_authority::approval_binding(actor)}).to_string().as_bytes()))
}

fn owned(job: &Value, actor: &operator_auth::Actor) -> ApiResult<()> {
    if job["kind"] != "conductor" || job["conductor"]["version"] != 1
        || (actor.role != "owner" && job["conductor"]["grant"]["actor"]["id"] != actor.id) {
        return Err(ApiError(StatusCode::NOT_FOUND, "Conductor run not found".into()));
    }
    Ok(())
}

fn public(job: &Value) -> Value {
    let mut value = job.clone();
    if let Some(grant) = value["conductor"]["grant"].as_object_mut() { grant.remove("authorityGeneration"); }
    if let Some(campaign) = value["conductor"].as_object_mut() { campaign.remove("startPayloadHash"); }
    value
}

pub(crate) async fn start(State(app): State<App>, Extension(actor): Extension<operator_auth::Actor>, Json(body): Json<Value>) -> ApiResult<Json<Value>> {
    let body = parse(&body)?;
    if body["mode"] == "execute" { app.check_execution()?; }
    let hash = start_hash(&body, &actor);
    let (run, replayed, running) = app.change_conductor_start(&body, &actor, &hash, |d| {
        let existing: Vec<_> = list(d, "jobs").iter().filter(|j| j["kind"] == "conductor" && j["refId"] == body["requestId"]).collect();
        if !existing.is_empty() {
            if existing.len() != 1 || existing[0]["conductor"]["startPayloadHash"] != hash { return Err(conflict("Conductor requestId was reused with another payload or actor")); }
            owned(existing[0], &actor)?;
            conductor_authority::check_workspace_generation(d,existing[0])?;
            return Ok((required(existing[0], "id")?.to_owned(), true, existing[0]["conductor"]["desiredState"] == "running"));
        }
        if list(d, "jobs").iter().any(|j| j["kind"] == "conductor"
            && matches!(j["conductor"]["desiredState"].as_str(), Some("running" | "pausing"))) {
            return Err(conflict("This company already has an active conductor"));
        }
        let grant = conductor_authority::create_grant(d, &actor, &body)?;
        if let Some(cutoff) = body["scope"]["cutoffUtc"].as_str() {
            let cutoff = chrono::DateTime::parse_from_rfc3339(cutoff).map_err(|_| bad("Invalid conductor cutoff"))?;
            for key in body["scope"]["itemIds"].as_array().unwrap() {
                let item = row(d, "items", key.as_str().unwrap())?;
                let created = chrono::DateTime::parse_from_rfc3339(required(item, "createdAt")?).map_err(|_| bad("Campaign item has no valid creation date"))?;
                if created > cutoff { return Err(conflict("Campaign manifest exceeds its cutoff")); }
            }
        }
        let binding = active_binding(d)?.to_json();
        let account = d["account"].clone();
        let run = new_job(d, "conductor", required(&body, "requestId")?)?;
        let job = row_mut(d, "jobs", &run)?;
        job["purpose"] = json!("autonomous_conductor"); job["account"] = account; job["connectorBinding"] = binding;
        job["conductor"] = json!({"version":1,"desiredState":"running","leaseGeneration":1,"mode":body["mode"],
            "scope":body["scope"],"limits":body["limits"],"grant":grant,"startPayloadHash":hash,
            "childEverStarted":false,"checkpoint":{"relativePath":format!("conductor/{run}/queue.json")},"progress":null,"itemHolds":[]});
        Ok((run, false, true))
    }).await?;
    if running { connection_continuation::continue_run(&app,&run,connection_continuation::Reason::Start).await?; }
    Ok(Json(json!({"runId":run,"requestId":body["requestId"],"replayed":replayed})))
}

pub(crate) async fn status(State(app): State<App>, Extension(actor): Extension<operator_auth::Actor>, Path(run): Path<String>) -> ApiResult<Json<Value>> {
    let job = app.db.read_job(&run).await?.ok_or_else(|| ApiError(StatusCode::NOT_FOUND, "Conductor run not found".into()))?;
    owned(&job, &actor)?; Ok(Json(public(&job)))
}

pub(crate) async fn pause(State(app): State<App>, Extension(actor): Extension<operator_auth::Actor>, Path(run): Path<String>) -> ApiResult<Json<Value>> {
    let generation = {let _company=connection_gate::lock(&app).await; app.change_job(&run, |d| {
        let job = row_mut(d, "jobs", &run)?; owned(job, &actor)?;
        if matches!(job["conductor"]["desiredState"].as_str(), Some("paused" | "revoked")) { return Ok(None); }
        job["conductor"]["desiredState"] = json!("pausing"); job["status"] = json!("pausing"); job["updatedAt"] = json!(now());
        Ok(job["conductor"]["leaseGeneration"].as_u64().map(|generation|(generation,job["conductor"]["mode"]=="execute")))
    }).await?};
    if let Some((generation,execute)) = generation {
        let worker = app.clone(); let key = run.clone();
        tokio::spawn(async move {
            if execute {
                let intent=format!("conductor-pause:{key}:{generation}");
                if connection_gate::close_and_drain(&worker,&intent,"sender_handoff").await.is_err(){return;}
            }
            let _barrier = conductor_authority::transition_guard(&worker, &key).await;
            let _ = worker.change_job(&key, |d| {
                let job = row_mut(d, "jobs", &key)?;
                if job["conductor"]["desiredState"] == "pausing" && job["conductor"]["leaseGeneration"].as_u64() == Some(generation) {
                    job["conductor"]["desiredState"] = json!("paused"); job["status"] = json!("paused");
                    job["conductor"]["leaseGeneration"] = json!(generation.checked_add(1).ok_or_else(|| internal("Conductor generation exhausted"))?);
                    job["updatedAt"] = json!(now());
                }
                Ok(())
            }).await;
        });
    }
    status(State(app), Extension(actor), Path(run)).await
}

pub(crate) async fn resume(State(app): State<App>, Extension(actor): Extension<operator_auth::Actor>, Path(run): Path<String>) -> ApiResult<Json<Value>> {
    let prior = app.db.read_job(&run).await?.ok_or_else(|| conflict("Conductor run missing"))?;
    owned(&prior, &actor)?;
    conductor_authority::check_control(&app, &prior).await?;
    if prior["conductor"]["mode"] == "execute" { app.check_execution()?; }
    if prior["status"] == "completed" { return Err(conflict("Conductor campaign is already complete")); }
    // The canonical queue quarantines each UNKNOWN recipient/conversation.
    // Resuming its immutable manifest does not authorize replaying that effect.
    if matches!(prior["conductor"]["desiredState"].as_str(),Some("pausing"|"revoked")) {
        return Err(conflict("Conductor cannot resume while pausing or revoked"));
    }
    if prior["conductor"]["desiredState"]!="running" {conductor_child::wait_stopped(&app,&run).await?;}
    connection_continuation::continue_run(&app,&run,connection_continuation::Reason::Resume).await?;
    status(State(app), Extension(actor), Path(run)).await
}

pub(crate) async fn prepare_child(app: &App, run: &str) -> ApiResult<Launch> {
    prepare_child_inner(app,run,None).await
}
pub(crate) async fn prepare_child_again(app:&App,run:&str,claim:&Value)->ApiResult<Launch> {
    prepare_child_inner(app,run,Some(claim)).await
}
async fn prepare_child_inner(app:&App,run:&str,retained_claim:Option<&Value>)->ApiResult<Launch> {
    let mut job = app.db.read_job(run).await?.ok_or_else(|| conflict("Conductor run missing"))?;
    if job["conductor"]["mode"]=="execute" {
        if let Some(claim)=retained_claim {
            if !job["conductor"]["connectionContinuation"].is_null()&&job["conductor"]["connectionContinuation"]!=*claim {
                return Err(conflict("Conductor continuation claim changed"));
            }
            job["conductor"]["connectionContinuation"]=claim.clone();
        }
    }
    let campaign = &job["conductor"];
    let generation = campaign["leaseGeneration"].as_u64().ok_or_else(|| internal("Conductor lease missing"))?;
    let actor = conductor_authority::authorize(app, run, generation, "read", &[]).await?;
    if campaign["mode"]=="execute" {connection_continuation::require_launch_ready(app,&job).await?;}
    if uuid::Uuid::parse_str(run).is_err() { return Err(internal("Conductor run has invalid path identity")); }
    let root = app.data.join("conductor"); tokio::fs::create_dir_all(&root).await.map_err(|_| internal("Conductor checkpoint directory unavailable"))?;
    let data_root = tokio::fs::canonicalize(&app.data).await.map_err(|_| internal("Conductor data directory unavailable"))?;
    let canonical_root = tokio::fs::canonicalize(&root).await.map_err(|_| internal("Conductor checkpoint directory unavailable"))?;
    if !canonical_root.starts_with(&data_root) { return Err(internal("Conductor checkpoint directory escapes company data")); }
    let folder = root.join(run); tokio::fs::create_dir_all(&folder).await.map_err(|_| internal("Conductor checkpoint directory unavailable"))?;
    let folder = tokio::fs::canonicalize(&folder).await.map_err(|_| internal("Conductor checkpoint directory unavailable"))?;
    if !folder.starts_with(&canonical_root) { return Err(internal("Conductor checkpoint path escapes company data")); }
    let checkpoint_path = folder.join("queue.json");
    let mut exists = tokio::fs::try_exists(&checkpoint_path).await.map_err(|_| internal("Conductor checkpoint unavailable"))?;
    if exists && tokio::fs::symlink_metadata(&checkpoint_path).await.map_err(|_| internal("Conductor checkpoint unavailable"))?.file_type().is_symlink() {
        return Err(internal("Conductor checkpoint must not be a link"));
    }
    if !exists && campaign["childEverStarted"] != false { return Err(conflict("Conductor checkpoint missing; recovery required")); }
    // Commit a resumable empty journal before any process may cross an
    // admission boundary. A crash before spawn can therefore resume unattended.
    if !exists {
        let repairs=if campaign["mode"]=="prepare"{json!(0)}else{campaign["limits"]["maxRepairRounds"].clone()};
        let mut seed=json!({"schemaVersion":1,"kind":"communityhero-queue","account":app.account.display(),
            "baseUrl":format!("http://127.0.0.1:{}",app.port),"phase":"starting","createdAt":now(),"updatedAt":now(),
            "conductorRunId":run,"cycle":0,"attemptedItemIds":[],"slices":[],"batchSize":campaign["limits"]["batchSize"],
            "maxCycles":campaign["limits"]["maxCycles"],"scopeItemIds":campaign["scope"]["itemIds"],
            "flowPolicy":{"freshEditorial":true,"maxRepairRounds":repairs,"continueHeld":true},
            "workflowGeneration":campaign["grant"]["workspaceGeneration"],"workflowId":run});
        if campaign["mode"]=="prepare"{seed["workflowMode"]=json!("prepare_review_only");}
        if let Some(cutoff)=campaign["scope"].get("cutoffUtc"){seed["cutoffUtc"]=cutoff.clone();}
        let temporary=folder.join(format!("seed-{}.tmp",uuid::Uuid::new_v4()));
        let mut file=tokio::fs::OpenOptions::new().create_new(true).write(true).open(&temporary).await
            .map_err(|_|internal("Conductor checkpoint seed unavailable"))?;
        file.write_all(format!("{seed}\n").as_bytes()).await.map_err(|_|internal("Conductor checkpoint seed unavailable"))?;
        file.sync_all().await.map_err(|_|internal("Conductor checkpoint seed unavailable"))?;drop(file);
        tokio::fs::rename(&temporary,&checkpoint_path).await.map_err(|_|internal("Conductor checkpoint seed unavailable"))?;
        exists=true;
    }
    validate_checkpoint(&checkpoint_path,&job,app.port).await?;
    let launch = Launch {run_id:run.into(),lease_generation:generation,actor,account:app.account.display().into(),
        base_url:format!("http://127.0.0.1:{}",app.port),checkpoint_path,
        scope_item_ids:campaign["scope"]["itemIds"].as_array().ok_or_else(|| internal("Conductor scope missing"))?.iter()
            .map(|v|v.as_str().map(str::to_owned).ok_or_else(||internal("Conductor scope invalid"))).collect::<ApiResult<_>>()?,
        mode:required(campaign,"mode")?.into(),max_repair_rounds:if campaign["mode"]=="prepare"{0}else{campaign["limits"]["maxRepairRounds"].as_u64().unwrap_or(0)},
        max_cycles:campaign["limits"]["maxCycles"].as_u64().unwrap_or(10_000),batch_size:campaign["limits"]["batchSize"].as_u64().unwrap_or(60),
        cutoff_utc:campaign["scope"]["cutoffUtc"].as_str().map(str::to_owned),resume:exists,
        workspace_generation:campaign["grant"]["workspaceGeneration"].as_str().map(str::to_owned),
        connection_binding:job["connectorBinding"].clone(),continuation_claim:campaign["connectionContinuation"].clone()};
    app.change_job(run, |d| {
        conductor_authority::check_workspace_generation(d,row(d,"jobs",run)?)?;
        if launch.mode=="execute" {
            let mut candidate=row(d,"jobs",run)?.clone();
            if let Some(claim)=retained_claim {
                if !candidate["conductor"]["connectionContinuation"].is_null()&&candidate["conductor"]["connectionContinuation"]!=*claim {
                    return Err(conflict("Conductor continuation claim changed"));
                }
                candidate["conductor"]["connectionContinuation"]=claim.clone();
            }
            connection_continuation::require_launch_projection(app,d,&candidate)?;
        }
        let job = row_mut(d,"jobs",run)?;
        if job["conductor"]["leaseGeneration"].as_u64()!=Some(generation) || job["conductor"]["desiredState"]!="running" {
            return Err(conflict("Conductor lease changed before launch"));
        }
        job["conductor"]["childEverStarted"]=json!(true);job["status"]=json!("running");job["error"]=Value::Null;
        job["conductor"].as_object_mut().ok_or_else(||internal("Conductor state invalid"))?.remove("connectionContinuation");Ok(())
    }).await?;
    Ok(launch)
}

pub(crate) async fn record_child_report(app: &App, launch: &Launch, report: &Value) -> ApiResult<()> {
    if report.to_string().len()>512_000 || !report.is_object() {return Err(bad("Invalid conductor progress"));}
    let allowed=["event","mode","summary","itemHolds","checkpointDigest"];
    if report.as_object().unwrap().keys().any(|key|!allowed.contains(&key.as_str())) {return Err(bad("Invalid conductor progress fields"));}
    if report.get("event").is_some_and(|event|event.as_str().is_none_or(|s|s.len()>80||!s.bytes().all(|b|b.is_ascii_alphanumeric()||b==b'.'||b==b'-'||b==b'_'))) {
        return Err(bad("Invalid conductor progress event"));
    }
    if let Some(summary)=report.get("summary") {
        let summary=summary.as_object().ok_or_else(||bad("Invalid conductor summary"))?;
        let counters=["verifiedReplies","verifiedNoReply","failed","unknown","stale","held","unresolvedTransport","sliceFailures","unresolvedItems","prepared","slices","total","verified","pending"];
        if summary.iter().any(|(key,value)|!counters.contains(&key.as_str())||value.as_u64().is_none_or(|n|n>50_000)) {
            return Err(bad("Invalid conductor summary fields"));
        }
    }
    let scope:HashSet<_>=launch.scope_item_ids.iter().map(String::as_str).collect();
    if let Some(holds)=report.get("itemHolds") {
        let holds=holds.as_array().filter(|holds|holds.len()<=MAX_CAMPAIGN_ITEMS).ok_or_else(||bad("Invalid conductor holds"))?;
        if holds.iter().any(|hold|hold["itemId"].as_str().is_none_or(|id|!scope.contains(id))
            || hold.as_object().is_none_or(|fields|fields.keys().any(|key|!matches!(key.as_str(),"itemId"|"reason"|"stage")))
            || hold["reason"].as_str().is_none_or(|s|s.is_empty()||s.len()>4000)
            || hold.get("stage").is_some_and(|stage|stage.as_str().is_none_or(|s|s.len()>80))) {return Err(bad("Invalid conductor item hold"));}
    }
    app.change_job(&launch.run_id,|d| {
        let job=row_mut(d,"jobs",&launch.run_id)?;
        if job["conductor"]["leaseGeneration"].as_u64()!=Some(launch.lease_generation) {return Err(conflict("Stale conductor child"));}
        job["conductor"]["progress"]=report.clone();
        if report["event"]=="holds-reset" {job["conductor"]["itemHolds"]=json!([]);}
        if let Some(holds)=report.get("itemHolds") {
            if report["event"]=="holds-page" {
                let current=job["conductor"]["itemHolds"].as_array_mut().ok_or_else(||internal("Conductor holds missing"))?;
                for hold in holds.as_array().unwrap() {
                    if let Some(existing)=current.iter_mut().find(|row|row["itemId"]==hold["itemId"]) {*existing=hold.clone();}
                    else {current.push(hold.clone());}
                }
            } else {job["conductor"]["itemHolds"]=holds.clone();}
        }
        job["updatedAt"]=json!(now());Ok(())
    }).await
}

pub(crate) async fn finish_child(app: &App, launch: &Launch, result: ApiResult<Value>) -> ApiResult<()> {
    // A failed observation/validation is a terminal disposition too. It must
    // never leave an active durable run after its sole child has disappeared.
    let result=async {
        let result=result?;
        if !result.is_object() || result.as_object().unwrap().keys().any(|key|!matches!(key.as_str(),"mode"|"summary"|"itemHolds")) {
            return Err(bad("Invalid conductor child result"));
        }
        record_child_report(app,launch,&result).await?;
        canonical_result(app,launch,&result).await
    }.await;
    let result=result.ok();
    for attempt in 0..3 {
    let saved=app.change_job(&launch.run_id,|d| {
        let job=row_mut(d,"jobs",&launch.run_id)?;
        if job["conductor"]["leaseGeneration"].as_u64()!=Some(launch.lease_generation) {return Ok(());}
        if job["conductor"]["desiredState"]!="running" {return Ok(());}
        match &result {
            Some(result)=>{
                let completed=matches!(result["mode"].as_str(),Some("complete"|"complete-with-holds"|"prepared"|"approved"|"no-action"));
                job["status"]=json!(if completed{"completed"}else{"blocked"});
                job["result"]=result.clone();job["finishedAt"]=json!(now());job["conductor"]["desiredState"]=json!("paused");
            }
            None=>{
                job["status"]=json!("blocked");job["error"]=json!("Conductor stopped; resume existing checkpoints for inspection, never resend unknown work");
                job["conductor"]["desiredState"]=json!("paused");
            }
        } Ok(())
    }).await;
    match saved {
        Ok(())=>return Ok(()),
        Err(error) if attempt==2=>{
            eprintln!("conductor settlement failed: durable persistence unavailable");
            return Err(error);
        },
        Err(_)=>tokio::time::sleep(std::time::Duration::from_millis(40)).await,
    }
    } unreachable!()
}

async fn canonical_result(app:&App,launch:&Launch,reported:&Value)->ApiResult<Value> {
    let persisted=app.db.read_job(&launch.run_id).await?.ok_or_else(||conflict("Conductor run missing"))?;
    let reported_holds=reported.get("itemHolds").unwrap_or(&persisted["conductor"]["itemHolds"]);
    let mut counts=json!({"total":launch.scope_item_ids.len(),"verifiedReplies":0,"verifiedNoReply":0,
        "verified":0,"failed":0,"unknown":0,"stale":0,"held":0,"prepared":0,"pending":0});
    let mut holds=Vec::new();
    for chunk in launch.scope_item_ids.chunks(100) {
        let view=app.db.read_bounded_review(chunk,app.account.display()).await?;
        if view["coverage"]["operationsComplete"]!=true {return Err(internal("Conductor result operation coverage incomplete"));}
        for item_id in chunk {
            let item=row(&view,"items",item_id)?;
            let recipient=json!({"itemId":item_id});
            let operations:Vec<_>=list(&view,"operations").iter().filter(|op|op["itemId"]==*item_id
                || recipient_operation_blocks(op,&recipient,item)).collect();
            let owned:Vec<_>=operations.iter().copied().filter(|op|op["conductorRunId"]==launch.run_id).collect();
            let increment=|counts:&mut Value,key:&str| {counts[key]=json!(counts[key].as_u64().unwrap_or(0)+1);};
            if operations.iter().any(|op|op["status"]=="unknown") {
                increment(&mut counts,"unknown");holds.push(json!({"itemId":item_id,"reason":"Original operation outcome is UNKNOWN; observation and readback only","stage":"unknown"}));continue;
            }
            if let Some(operation)=owned.iter().find(|op|op["status"]=="succeeded") {
                increment(&mut counts,"verified");
                increment(&mut counts,if operation["action"]["action"]=="reply_and_close"{"verifiedReplies"}else{"verifiedNoReply"});continue;
            }
            if owned.iter().any(|op|matches!(op["status"].as_str(),Some("pending"|"queued"|"dispatching"))) {
                increment(&mut counts,"pending");holds.push(json!({"itemId":item_id,"reason":"Original admitted operation has no terminal outcome","stage":"pending"}));continue;
            }
            if owned.iter().any(|op|op["status"]=="failed") {
                increment(&mut counts,"failed");holds.push(json!({"itemId":item_id,"reason":"Original operation failed; no automatic resend","stage":"failed"}));continue;
            }
            if owned.iter().any(|op|op["status"]=="stale") {
                increment(&mut counts,"stale");holds.push(json!({"itemId":item_id,"reason":"Original operation proof is stale","stage":"stale"}));continue;
            }
            if launch.mode=="prepare" && list(&view,"proposals").iter().any(|proposal|proposal["itemId"]==*item_id
                && proposal["status"]=="draft" && proposal["conductorRunId"]==launch.run_id) {increment(&mut counts,"prepared");continue;}
            increment(&mut counts,"held");
            let reason=reported_holds.as_array().into_iter().flatten().find(|hold|hold["itemId"]==*item_id)
                .and_then(|hold|hold["reason"].as_str()).unwrap_or("Recipient did not reach a verified terminal operation");
            holds.push(json!({"itemId":item_id,"reason":reason,"stage":"campaign"}));
        }
    }
    let blocked=["unknown","pending","failed","stale"].iter().any(|key|counts[*key].as_u64().unwrap_or(0)>0);
    let reported_complete=matches!(reported["mode"].as_str(),Some("complete"|"complete-with-holds"|"prepared"|"approved"|"no-action"));
    let mode=if blocked||!reported_complete{"blocked"}else if counts["held"].as_u64().unwrap_or(0)>0{"complete-with-holds"}
        else if launch.mode=="prepare"{"prepared"}else{"complete"};
    Ok(json!({"mode":mode,"summary":counts,"itemHolds":holds}))
}

pub(crate) async fn fail_launch(app:&App,run:&str)->ApiResult<()> {
    app.change_job(run,|d| {
        let job=row_mut(d,"jobs",run)?;
        if job["conductor"]["desiredState"]=="running" {
            job["status"]=json!("recovery_required");job["conductor"]["desiredState"]=json!("paused");
            job["error"]=json!("Conductor could not recover its admitted checkpoint or runtime; no new work started");
        } Ok(())
    }).await
}

async fn validate_checkpoint(path:&std::path::Path,job:&Value,port:u16)->ApiResult<Value> {
    let metadata=tokio::fs::symlink_metadata(path).await.map_err(|_|conflict("Conductor checkpoint missing; recovery required"))?;
    if !metadata.is_file() || metadata.file_type().is_symlink() || metadata.len()>32*1024*1024 {
        return Err(conflict("Conductor checkpoint invalid; recovery required"));
    }
    let bytes=tokio::fs::read(path).await.map_err(|_|conflict("Conductor checkpoint unavailable; recovery required"))?;
    let checkpoint:Value=serde_json::from_slice(&bytes).map_err(|_|conflict("Conductor checkpoint invalid; recovery required"))?;
    let campaign=&job["conductor"];
    let repairs=if campaign["mode"]=="prepare"{json!(0)}else{campaign["limits"]["maxRepairRounds"].clone()};
    if checkpoint["schemaVersion"]!=1 || checkpoint["kind"]!="communityhero-queue"
        || checkpoint["conductorRunId"]!=job["id"] || checkpoint["account"]!=job["account"]
        || checkpoint["baseUrl"]!=format!("http://127.0.0.1:{port}")
        || checkpoint["scopeItemIds"]!=campaign["scope"]["itemIds"]
        || checkpoint["workflowGeneration"]!=campaign["grant"]["workspaceGeneration"]
        || checkpoint.get("cutoffUtc")!=campaign["scope"].get("cutoffUtc")
        || checkpoint["flowPolicy"]["freshEditorial"]!=true || checkpoint["flowPolicy"]["continueHeld"]!=true
        || checkpoint["flowPolicy"]["maxRepairRounds"]!=repairs
        || checkpoint["workflowId"]!=job["id"]
        || campaign["mode"]=="prepare"&&checkpoint["workflowMode"]!="prepare_review_only" {
        return Err(conflict("Conductor checkpoint binding changed; recovery required"));
    }
    Ok(checkpoint)
}

pub(crate) async fn restart_allowed(app:&App,launch:&Launch)->ApiResult<bool> {
    let job=app.db.read_job(&launch.run_id).await?.ok_or_else(||conflict("Conductor missing"))?;
    if job["conductor"]["desiredState"]!="running" || job["conductor"]["leaseGeneration"].as_u64()!=Some(launch.lease_generation) {return Ok(false);}
    conductor_authority::check_control(app,&job).await?;
    validate_checkpoint(&launch.checkpoint_path,&job,app.port).await?;
    app.change_job(&launch.run_id,|d| {
        let job=row_mut(d,"jobs",&launch.run_id)?;
        if job["conductor"]["desiredState"]!="running" || job["conductor"]["leaseGeneration"].as_u64()!=Some(launch.lease_generation) {return Ok(false);}
        let attempts=job["conductor"]["childRestarts"].as_u64().unwrap_or(0);
        if attempts>=2{return Ok(false);}
        job["conductor"]["childRestarts"]=json!(attempts+1);Ok(true)
    }).await
}

/// Called only after canonical interrupted-job and UNKNOWN recovery. Durable
/// desiredState decides admission; process presence is never an authorization.
pub(crate) async fn recover(app: &App) -> ApiResult<()> {
    let snapshot=app.read().await?;
    has_deferred_connection_work(&snapshot)?;
    let pausing:Vec<String>=list(&snapshot,"jobs").iter().filter(|job|job["kind"]=="conductor"
        && job["conductor"]["desiredState"]=="pausing").filter_map(|job|job["id"].as_str().map(str::to_owned)).collect();
    for run in pausing {
        let job=row(&snapshot,"jobs",&run)?;
        if job["conductor"]["mode"]=="execute" {
            let generation=job["conductor"]["leaseGeneration"].as_u64().ok_or_else(||internal("Conductor generation missing"))?;
            let intent=format!("conductor-pause:{run}:{generation}");
            connection_gate::close_and_drain(app,&intent,"sender_handoff").await?;
        }
        let _barrier=conductor_authority::transition_guard(app,&run).await;
        app.change_job(&run,|d| {
            let job=row_mut(d,"jobs",&run)?;
            let generation=job["conductor"]["leaseGeneration"].as_u64().ok_or_else(||internal("Conductor generation missing"))?;
            job["conductor"]["leaseGeneration"]=json!(generation.checked_add(1).ok_or_else(||internal("Conductor generation exhausted"))?);
            job["conductor"]["desiredState"]=json!("paused");job["status"]=json!("paused");Ok(())
        }).await?;
    }
    let runs:Vec<String>=list(&snapshot,"jobs").iter().filter(|job|job["kind"]=="conductor"
        && job["conductor"]["desiredState"]=="running").filter_map(|job|job["id"].as_str().map(str::to_owned)).collect();
    for run in runs {
        connection_continuation::continue_run(app,&run,connection_continuation::Reason::Recover).await?;
    } Ok(())
}

#[cfg(test)]
#[path="conductor_tests.rs"]
mod tests;
