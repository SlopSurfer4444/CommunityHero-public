//! Authenticated, read-only current review for a bounded item selection.
use crate::operator_auth::Actor;
use crate::*;
use axum::{Extension, extract::Query};

/// GET /api/items/review-bundle?itemIds=<comma-separated canonical IDs>
///
/// This returns complete operation history for those items, including UNKNOWN
/// outcomes. It never treats this observation as an approval or a dispatch
/// preflight; mutation handlers still perform their own current-state checks.
pub(crate) async fn get(
    State(app): State<App>,
    Extension(_actor): Extension<Actor>,
    Query(query): Query<HashMap<String, String>>,
) -> ApiResult<Json<Value>> {
    if query.len() != 1 || !query.contains_key("itemIds") {
        return Err(bad("Specify only itemIds"));
    }
    let encoded = &query["itemIds"];
    if encoded.is_empty() || encoded.len() > 12_900 {
        return Err(bad("Select between 1 and 100 item IDs"));
    }
    let ids: Vec<String> = encoded.split(',').map(str::to_owned).collect();
    Ok(Json(
        app.db
            .read_bounded_review(&ids, app.account.display())
            .await?,
    ))
}
