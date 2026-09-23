//! Cheap process-local freshness read; register behind normal API authentication.
use crate::{App,Json,State,Value,json};
use crate::operator_auth::Actor;
use axum::Extension;
pub(crate) async fn get(State(app):State<App>,Extension(actor):Extension<Actor>)->Json<Value> {
    app.observe_media_proofs();
    // Authentication can change without any workspace mutation. The caller must
    // not retain another actor's UI or skip refresh after a same-actor relogin.
    // CSRF is the current authenticated caller's token, as in /api/session.
    Json(json!({"workspaceVersion":app.bootstrap_cache.current_version(),"actorId":actor.id,"csrfToken":actor.csrf_token}))
}
