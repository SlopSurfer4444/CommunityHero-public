//! Authenticated native planning and atomic commit of retained suggestions.
use crate::*;
use axum::Extension;

fn item_ids(body: &Value) -> ApiResult<Vec<String>> {
    let o = body.as_object().ok_or_else(|| bad("Retained plan request must be an object"))?;
    if o.len() != 1 || !o.contains_key("itemIds") { return Err(bad("Retained plan accepts only itemIds")); }
    let ids = body["itemIds"].as_array().filter(|v| !v.is_empty() && v.len() <= 100)
        .ok_or_else(|| bad("Choose retained item IDs"))?;
    ids.iter().map(|v| v.as_str().map(str::to_owned).ok_or_else(|| bad("Invalid retained item ID"))).collect()
}
pub(crate) async fn plan(State(app): State<App>, Extension(actor): Extension<operator_auth::Actor>, Json(body): Json<Value>) -> ApiResult<Json<Value>> {
    let installed = retained_paid_recovery_registry::capture()?;
    let data = app.read().await?;
    retained_paid_recovery::plan(&data, &actor, &installed, &item_ids(&body)?).map(Json)
}
pub(crate) async fn commit(State(app): State<App>, Extension(actor): Extension<operator_auth::Actor>, Json(body): Json<Value>) -> ApiResult<Json<Value>> {
    let installed = retained_paid_recovery_registry::capture()?;
    // The reducer clones and validates the complete workspace. The storage
    // writer publishes it in one transaction; failures leave no partial drafts.
    app.commit_retained_paid_recovery(&actor, &installed, &body).await.map(Json)
}
