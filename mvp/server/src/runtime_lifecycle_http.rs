//! ROOT splices these POST handlers into owner maintenance routes.
//! No handler grants process-stop authority or accepts target capability flags.
use crate::{App, ApiResult, bad, conflict};
use crate::operator_auth::Actor;
use crate::{runtime_lifecycle, runtime_maintenance};
use axum::{Extension, Json, extract::State, http::{HeaderMap, StatusCode}, response::{IntoResponse, Response}};
use serde_json::{Value, json};

fn key(value: &str) -> bool {
    !value.is_empty() && value.len() <= 80
        && value.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
}
fn hash(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|b| b.is_ascii_digit() || matches!(b, b'a'..=b'f'))
}
fn exact(value: &Value, fields: &[&str]) -> bool {
    value.as_object().is_some_and(|object| object.len() == fields.len()
        && fields.iter().all(|field| object.contains_key(*field)))
}
fn decode_owner(input: &Value) -> ApiResult<runtime_lifecycle::OwnerToken> {
    if !exact(input, &["account", "runtimeId", "releaseSha256", "epoch"]) {
        return Err(bad("Malformed runtime lifecycle owner token"));
    }
    let account = input["account"].as_str().ok_or_else(|| bad("Malformed owner account"))?;
    let runtime_id = input["runtimeId"].as_str().ok_or_else(|| bad("Malformed owner runtimeId"))?;
    let release_sha256 = input["releaseSha256"].as_str().ok_or_else(|| bad("Malformed owner releaseSha256"))?;
    let epoch = input["epoch"].as_u64().ok_or_else(|| bad("Malformed owner epoch"))?;
    if !matches!(account, "LikeAvto" | "BAW Russia") || !key(runtime_id) || !hash(release_sha256) || epoch == 0 {
        return Err(bad("Malformed runtime lifecycle owner token"));
    }
    Ok(runtime_lifecycle::OwnerToken { account: account.into(), runtime_id: runtime_id.into(),
        release_sha256: release_sha256.into(), epoch })
}
fn request_shape(body: &Value, fields: &[&str]) -> ApiResult<()> {
    if !exact(body, fields) { return Err(bad("Malformed runtime lifecycle request")); }
    Ok(())
}
fn owner_json(owner: &runtime_lifecycle::OwnerToken) -> Value {
    json!({"account":owner.account,"runtimeId":owner.runtime_id,
        "releaseSha256":owner.release_sha256,"epoch":owner.epoch})
}
fn bound_owner(app: &App, input: &Value) -> ApiResult<runtime_lifecycle::OwnerToken> {
    let owner = decode_owner(input)?;
    runtime_lifecycle::require_runtime_owner(&owner, app.lifecycle_admission.identity())?;
    Ok(owner)
}
fn authorize(actor: &Actor, headers: &HeaderMap) -> Result<(), Response> {
    if actor.role != "owner" {
        return Err((StatusCode::FORBIDDEN, Json(json!({"error":"Owner access required","stopAuthorized":false}))).into_response());
    }
    // Reject duplicate fields, invalid bytes and empty tokens explicitly, even
    // when middleware already checked this request.
    let mut values = headers.get_all("x-csrf-token").iter();
    let supplied = values.next().and_then(|value| value.to_str().ok());
    if values.next().is_some() || !supplied.is_some_and(|token| !token.is_empty() && actor.valid_csrf(token)) {
        return Err((StatusCode::FORBIDDEN, Json(json!({"error":"CSRF token required","stopAuthorized":false}))).into_response());
    }
    Ok(())
}
fn reply(result: ApiResult<Value>) -> Response {
    match result { Ok(value) => Json(value).into_response(), Err(error) => error.into_response() }
}
fn returned_owner(value: &Value) -> ApiResult<runtime_lifecycle::OwnerToken> { decode_owner(value) }
fn validate_transfer(transfer: &Value, owner: &runtime_lifecycle::OwnerToken) -> ApiResult<()> {
    let target = &transfer["target"];
    if !exact(transfer, &["ledgerSha256", "owner", "target", "nativeSettled", "queuedBacklog"])
        || !transfer["ledgerSha256"].as_str().is_some_and(hash)
        || transfer["owner"] != owner_json(owner) || transfer["nativeSettled"] != true
        || !(transfer["queuedBacklog"].is_null() || transfer["queuedBacklog"].is_object())
        || !exact(target, &["releaseSha256", "attemptId", "asrDisabled", "mediaAnalysisGeneration"])
        || !target["releaseSha256"].as_str().is_some_and(hash)
        || !target["attemptId"].as_str().is_some_and(key)
        || !target["asrDisabled"].is_boolean() || !target["mediaAnalysisGeneration"].is_u64()
        || (target["mediaAnalysisGeneration"] == 0 && target["asrDisabled"] != true) {
        return Err(bad("Malformed runtime lifecycle transfer"));
    }
    // This is only wire-shape validation. The coordinator compares the complete
    // transfer to the frozen record and ledger under the durable writer lock,
    // including the backlog inventory, owner and complete retained row hashes.
    Ok(())
}

pub(crate) async fn begin(State(app): State<App>, Extension(actor): Extension<Actor>, headers: HeaderMap,
    Json(body): Json<Value>) -> Response {
    if let Err(response) = authorize(&actor, &headers) { return response; }
    reply(async {
        request_shape(&body, &["owner", "attemptId", "releaseSha256"])?;
        let owner = bound_owner(&app, &body["owner"])?;
        if !body["attemptId"].as_str().is_some_and(key) { return Err(bad("Malformed lifecycle attemptId")); }
        if !body["releaseSha256"].as_str().is_some_and(hash) { return Err(bad("Malformed lifecycle releaseSha256")); }
        // The hash selects an already admitted immutable full-closure target;
        // browser JSON never supplies its generation or ASR policy.
        let target = app.lifecycle_admission.target(body["releaseSha256"].as_str().unwrap())?;
        let mut value = runtime_maintenance::begin(&app, &owner, &target, body["attemptId"].as_str().unwrap()).await?;
        let returned = returned_owner(&value["lifecycle"]["owner"])?;
        value["owner"] = owner_json(&returned);
        value["stopAuthorized"] = json!(false);
        Ok(value)
    }.await)
}
/// Register a ROOT-reviewed local file pin, never caller target capability flags.
/// This changes only held native target admission; it grants no stop authority.
pub(crate) async fn register_target(State(app):State<App>,Extension(actor):Extension<Actor>,headers:HeaderMap,
    Json(body):Json<Value>)->Response {
    if let Err(response)=authorize(&actor,&headers){return response;}
    reply(async {
        request_shape(&body,&["owner","admission"])?;
        let owner=bound_owner(&app,&body["owner"])?;
        // Public artifact IO is native verification only. The exact current
        // durable owner/epoch is rechecked after that IO under the writer lock.
        let admission=app.lifecycle_admission.clone();
        let pin=body["admission"].clone();
        let verified=tokio::task::spawn_blocking(move||admission.verify_target_file(&pin)).await
            .map_err(|_|conflict("Native target admission verification failed"))??;
        let mut result=app.lifecycle_admission.register_target(&app,&owner,verified).await?;
        result["owner"]=owner_json(&owner);
        Ok(result)
    }.await)
}
pub(crate) async fn status(State(app): State<App>, Extension(actor): Extension<Actor>, headers: HeaderMap,
    Json(body): Json<Value>) -> Response {
    if let Err(response) = authorize(&actor, &headers) { return response; }
    reply(async {
        request_shape(&body, &["owner"])?;
        let owner = bound_owner(&app, &body["owner"])?;
        let mut value = runtime_maintenance::status(&app).await?;
        // Bind this exact observation, with no second database read. Status is
        // advisory and never grants later mutation/admission authority.
        if value["lifecycle"]["owner"] != owner_json(&owner) {
            return Err(conflict("Runtime lifecycle status owner or epoch mismatch"));
        }
        value["owner"] = owner_json(&owner);
        value["stopAuthorized"] = json!(false);
        Ok(value)
    }.await)
}
pub(crate) async fn checkpoint(State(app): State<App>, Extension(actor): Extension<Actor>, headers: HeaderMap,
    Json(body): Json<Value>) -> Response {
    if let Err(response) = authorize(&actor, &headers) { return response; }
    reply(async {
        request_shape(&body, &["owner"])?;
        let owner = bound_owner(&app, &body["owner"])?;
        let transfer = runtime_maintenance::checkpoint(&app, &owner).await?;
        validate_transfer(&transfer, &owner)?;
        Ok(json!({"owner":owner_json(&owner),"transfer":transfer,"stopAuthorized":false}))
    }.await)
}
pub(crate) async fn commit_stop(State(app): State<App>, Extension(actor): Extension<Actor>, headers: HeaderMap,
    Json(body): Json<Value>) -> Response {
    if let Err(response) = authorize(&actor, &headers) { return response; }
    reply(async {
        request_shape(&body, &["owner", "transfer"])?;
        let owner = bound_owner(&app, &body["owner"])?;
        validate_transfer(&body["transfer"], &owner)?;
        runtime_maintenance::commit_stop_checkpoint(&app, &owner, &body["transfer"]).await?;
        Ok(json!({"owner":owner_json(&owner),"phase":"stopped","checkpointCommitted":true,"stopAuthorized":false}))
    }.await)
}
pub(crate) async fn resume(State(app): State<App>, Extension(actor): Extension<Actor>, headers: HeaderMap,
    Json(body): Json<Value>) -> Response {
    if let Err(response) = authorize(&actor, &headers) { return response; }
    reply(async {
        request_shape(&body, &["owner"])?;
        let owner = bound_owner(&app, &body["owner"])?;
        let returned = runtime_maintenance::resume(&app, &owner).await?;
        Ok(json!({"owner":owner_json(&returned),"phase":"running","stopAuthorized":false}))
    }.await)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn token() -> Value {
        json!({"account":"LikeAvto","runtimeId":"native-one","releaseSha256":"a".repeat(64),"epoch":1})
    }
    #[test]
    fn exact_owner_rejects_unknown_fields_and_invalid_epochs() {
        for epoch in [json!(0), json!(-1), json!(1.5), json!("1"), Value::Null] {
            let mut value = token(); value["epoch"] = epoch;
            assert!(decode_owner(&value).is_err());
        }
        let mut value = token(); value["quiet"] = json!(true);
        assert!(decode_owner(&value).is_err());
        assert!(decode_owner(&token()).is_ok());
    }
    #[test]
    fn body_does_not_accept_caller_target_flags() {
        assert!(request_shape(&json!({"owner":token(),"attemptId":"one","releaseSha256":"b".repeat(64),"asrDisabled":true}), &["owner","attemptId","releaseSha256"]).is_err());
        assert!(request_shape(&json!({"owner":token(),"quiet":true}), &["owner"]).is_err());
        assert!(request_shape(&json!({"owner":token(),"admission":{"path":"C:/public/target.json","sha256":"b".repeat(64)},"asrDisabled":true}), &["owner","admission"]).is_err());
    }
    #[test]
    fn explicit_auth_requires_owner_and_one_nonempty_csrf_header() {
        let mut actor = Actor::local_owner("valid-token");
        let mut headers = HeaderMap::new();
        assert!(authorize(&actor, &headers).is_err());
        headers.insert("x-csrf-token", "valid-token".parse().unwrap());
        assert!(authorize(&actor, &headers).is_ok());
        actor.role = "operator".into();
        assert!(authorize(&actor, &headers).is_err());
        actor.role = "owner".into();
        headers.append("x-csrf-token", "valid-token".parse().unwrap());
        assert!(authorize(&actor, &headers).is_err());
    }

    // Exercise the actual producer and durable consumer through this HTTP
    // validator, rather than constructing a transfer that omits reducer fields.
    fn drained_transfer(account: &str, null_overlay: bool) -> (Value, runtime_lifecycle::OwnerToken, Value) {
        let mut workspace = json!({"account":account,"connectorBinding":{"id":"fixture-scoped"},
            "jobs":[{"id":"queued-media","kind":"media","purpose":"auto_media","status":"queued",
                "account":account,"visualContractVersion":2,"sourceAttempts":[],
                "refId":"fixture-post","createdAt":"fixture-time","finishedAt":null}],
            "operations":[{"id":"unknown-op","status":"unknown","action":{"reply":"preserve"}}],
            "approvals":[],"audit":[],"materials":[],"knowledge_entries":[],"knowledge_versions":[]});
        let owner = runtime_lifecycle::OwnerToken { account:account.into(),runtime_id:"native-one".into(),
            release_sha256:"a".repeat(64),epoch:1 };
        if null_overlay { workspace["jobs"] = json!([]); }
        let ledger = runtime_lifecycle::ledger_digest(&workspace).unwrap();
        runtime_lifecycle::initialize(&mut workspace, owner.clone(), &"b".repeat(64), &ledger).unwrap();
        let target = runtime_lifecycle::AdmittedTarget { release_sha256:"c".repeat(64),
            media_analysis_generation:1,asr_disabled:false };
        let drained_owner = runtime_lifecycle::begin_drain_for_release(&mut workspace, &owner, &target, "http-transfer").unwrap();
        // Existing reducer validation admits legacy/no-overlay drains with
        // explicit null; preserve that wire compatibility as well as capture.
        if null_overlay { workspace["runtimeLifecycle"]["queuedBacklog"] = Value::Null; }
        // Pure reducer fixture witness; this test never observes native state
        // or claims that a real process/provider has settled.
        let native = runtime_lifecycle::SettledNative { owner:drained_owner.clone(),application_tasks:0,
            provider_queued:0,provider_dispatched:0,provider_contained:true,credential_writers:0,unresolved_effects:0 };
        let transfer = runtime_lifecycle::mark_drained(&mut workspace, &drained_owner, &native).unwrap();
        (workspace, drained_owner, transfer)
    }
    #[test]
    fn reducer_transfer_passes_http_validation_and_durable_stop_with_retained_backlog() {
        for account in ["LikeAvto", "BAW Russia"] {
            let (mut workspace, owner, transfer) = drained_transfer(account, false);
            let jobs = workspace["jobs"].clone();
            let operations = workspace["operations"].clone();
            assert_eq!(transfer.as_object().unwrap().len(), 5);
            assert_eq!(transfer["queuedBacklog"]["jobs"].as_array().unwrap().len(), 1);
            validate_transfer(&transfer, &owner).unwrap();
            runtime_lifecycle::commit_stop_checkpoint(&mut workspace, &owner, &transfer).unwrap();
            assert_eq!(workspace["runtimeLifecycle"]["phase"], "stopped");
            assert_eq!(workspace["jobs"], jobs);
            assert_eq!(workspace["operations"], operations);
        }
    }
    #[test]
    fn reducer_null_overlay_transfer_remains_wire_compatible_and_committable() {
        let (mut workspace, owner, transfer) = drained_transfer("LikeAvto", true);
        assert!(transfer["queuedBacklog"].is_null());
        validate_transfer(&transfer, &owner).unwrap();
        runtime_lifecycle::commit_stop_checkpoint(&mut workspace, &owner, &transfer).unwrap();
        assert_eq!(workspace["runtimeLifecycle"]["phase"], "stopped");
    }
    #[test]
    fn transfer_wire_rejects_missing_invalid_type_and_extra_backlog_fields() {
        let (_, owner, transfer) = drained_transfer("LikeAvto", false);
        let mut missing = transfer.clone();
        missing.as_object_mut().unwrap().remove("queuedBacklog");
        assert!(validate_transfer(&missing, &owner).is_err());
        for invalid in [json!(false), json!(1), json!("backlog"), json!([])] {
            let mut changed = transfer.clone(); changed["queuedBacklog"] = invalid;
            assert!(validate_transfer(&changed, &owner).is_err());
        }
        let mut extra = transfer.clone(); extra["extraBacklog"] = json!({});
        assert!(validate_transfer(&extra, &owner).is_err());
    }
    #[test]
    fn valid_shaped_backlog_and_ledger_changes_never_pass_durable_stop() {
        let (workspace, owner, transfer) = drained_transfer("LikeAvto", false);
        for field in 0..7 {
            let mut changed = transfer.clone();
            match field {
                0 => changed["queuedBacklog"]["jobs"] = json!([]),
                1 => changed["queuedBacklog"]["version"] = json!(2),
                2 => changed["queuedBacklog"]["extra"] = json!(true),
                3 => changed["queuedBacklog"]["owner"]["account"] = json!("BAW Russia"),
                4 => changed["queuedBacklog"]["jobs"][0]["rowSha256"] = json!("d".repeat(64)),
                5 => changed["queuedBacklog"] = Value::Null,
                _ => changed["ledgerSha256"] = json!("d".repeat(64)),
            }
            // Wire syntax alone does not prove the frozen durable binding.
            validate_transfer(&changed, &owner).unwrap();
            let mut attempt = workspace.clone();
            assert!(runtime_lifecycle::commit_stop_checkpoint(&mut attempt, &owner, &changed).is_err());
            assert_eq!(attempt, workspace);
        }
    }
    #[test]
    fn frozen_overlay_tampering_and_raw_row_drift_still_fail_backlog_validation() {
        let (workspace, owner, transfer) = drained_transfer("LikeAvto", false);
        for field in 0..4 {
            let mut changed_workspace = workspace.clone();
            let mut changed_transfer = transfer.clone();
            match field {
                0 => changed_transfer["queuedBacklog"]["version"] = json!(2),
                1 => changed_transfer["queuedBacklog"]["extra"] = json!(true),
                2 => changed_transfer["queuedBacklog"]["owner"]["account"] = json!("BAW Russia"),
                _ => changed_transfer["queuedBacklog"]["jobs"][0]["rowSha256"] = json!("d".repeat(64)),
            }
            changed_workspace["runtimeLifecycle"]["queuedBacklog"] = changed_transfer["queuedBacklog"].clone();
            changed_workspace["runtimeLifecycle"]["transfer"] = changed_transfer.clone();
            validate_transfer(&changed_transfer, &owner).unwrap();
            let before = changed_workspace.clone();
            assert!(runtime_lifecycle::commit_stop_checkpoint(&mut changed_workspace, &owner, &changed_transfer).is_err());
            assert_eq!(changed_workspace, before);
        }
        let mut changed_workspace = workspace.clone();
        changed_workspace["jobs"][0]["refId"] = json!("changed-post");
        let mut changed_transfer = transfer.clone();
        changed_transfer["ledgerSha256"] = json!(runtime_lifecycle::ledger_digest(&changed_workspace).unwrap());
        changed_workspace["runtimeLifecycle"]["transfer"] = changed_transfer.clone();
        validate_transfer(&changed_transfer, &owner).unwrap();
        let before = changed_workspace.clone();
        assert!(runtime_lifecycle::commit_stop_checkpoint(&mut changed_workspace, &owner, &changed_transfer).is_err());
        assert_eq!(changed_workspace, before);
    }
    #[test]
    fn transfer_owner_and_digest_guards_remain_strict() {
        let (workspace, owner, transfer) = drained_transfer("LikeAvto", false);
        for field in ["account", "runtimeId", "releaseSha256", "epoch"] {
            let mut changed = transfer.clone();
            changed["owner"][field] = match field {
                "account" => json!("BAW Russia"), "runtimeId" => json!("foreign-runtime"),
                "releaseSha256" => json!("d".repeat(64)), _ => json!(owner.epoch + 1),
            };
            assert!(validate_transfer(&changed, &owner).is_err());
        }
        for digest in [Value::Null, json!("short"), json!("A".repeat(64)), json!(0)] {
            let mut changed = transfer.clone(); changed["ledgerSha256"] = digest;
            assert!(validate_transfer(&changed, &owner).is_err());
        }
        let mut stale = owner.clone(); stale.epoch -= 1;
        assert!(validate_transfer(&transfer, &stale).is_err());
        let mut attempt = workspace.clone();
        assert!(runtime_lifecycle::commit_stop_checkpoint(&mut attempt, &stale, &transfer).is_err());
        assert_eq!(attempt, workspace);
    }
}
