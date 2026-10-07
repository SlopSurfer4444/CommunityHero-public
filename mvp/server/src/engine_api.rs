//! Headless control shares the website's state and dispatch authority.
use crate::*;

pub(crate) async fn job(State(app): State<App>, Path(key): Path<String>) -> ApiResult<Json<Value>> {
    let mut job=app.db.read_job_public(&key).await?
        .ok_or_else(||ApiError(StatusCode::NOT_FOUND,"jobs record not found".into()))?;
    if job["kind"]=="editorial_review" {
        let snapshot=app.db.read_editorial_job_context(&key).await?;
        job=row(&snapshot,"jobs",&key)?.clone();
        job["editorialProgress"]=editorial_endpoint::progress(&snapshot,&job)?;
        sanitize_bootstrap_job(&mut job);
    }
    // Return durable status even after bootstrap's display-history limit.
    if let Some(bundle)=job["prepareBundle"].as_object_mut() { bundle.remove("request"); }
    if let Some(fields)=job.as_object_mut(){fields.remove("editorialPlan");fields.remove("editorialBatches");}
    sanitize_bootstrap_job(&mut job);
    Ok(Json(job))
}

pub(crate) async fn provider_capabilities(State(app): State<App>) -> ApiResult<Json<Value>> {
    let binding=active_binding(&app.db.read_metadata().await?)?;
    let account=bridge_account(&binding)?;
    Ok(Json(app.bridge("caps",json!({"account":account})).await?))
}

/// Resumable provider export. Source results are read-only and never admitted
/// as reviewed proposals or executable actions merely because they were read.
pub(crate) async fn scan(State(app): State<App>, Json(body): Json<Value>) -> ApiResult<Json<Value>> {
    let object=body.as_object().ok_or_else(|| bad("Scan request must be an object"))?;
    if object.keys().any(|key| !["statuses","pageSize","maxPages","maxItems","maxElapsedMs","resume"].contains(&key.as_str())) {
        return Err(bad("Unknown scan field; account is owned by the engine"));
    }
    let data=app.db.read_metadata().await?;
    let binding=active_binding(&data)?;
    let account=bridge_account(&binding)?.to_string();
    let mut request=body;
    request["account"]=json!(account);
    let job=app.change(|d|new_job(d,"provider-scan","history")).await?;
    let worker=app.clone();
    app.spawn(job.clone(),async move {
        let result=worker.bridge("scan",request).await?;
        if result["account"].as_str()!=Some(account.as_str()) {
            return Err(conflict("Provider scan account mismatch"));
        }
        Ok(result)
    });
    Ok(Json(json!({"jobId":job})))
}

pub(crate) async fn export(State(app): State<App>) -> ApiResult<Json<Value>> {
    let data=app.db.read_source_export_context().await?;
    let profile=accounts::Profile::from_workspace(&data)?;
    // Export source/comment state, never auth sessions, approval authority or
    // executable outbox records. This endpoint cannot restore/dispatch anything.
    Ok(Json(json!({"schemaVersion":1,"kind":"communityhero-source-export","account":profile.key(),
        "exportedAt":now(),"items":data["items"],"posts":data["posts"],"branches":data["branches"],
        "coverage":data["sync"],"containsExecutableActions":false})))
}
