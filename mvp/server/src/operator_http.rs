use crate::*;
use axum::{Extension, extract::Query, http::HeaderMap};
use crate::operator_auth::{Actor, AuthError};

pub fn configured_origin(auth:bool)->Result<Option<String>,String>{
    let Ok(origin)=std::env::var("COMMUNITYHERO_PUBLIC_ORIGIN") else{return Ok(None)};
    let authority=origin.strip_prefix("https://").ok_or("Public origin must use HTTPS")?;
    if !auth||authority.is_empty()||authority.contains(['/', '?', '#', '@', ':'])||!authority.bytes().all(|b|b.is_ascii_alphanumeric()||b==b'.'||b==b'-'){
        return Err("Invalid public origin or missing access configuration".into());
    }
    Ok(Some(origin))
}
fn denied(status:StatusCode,message:&str)->Response{(status,Json(json!({"error":message}))).into_response()}
fn cookie(headers:&HeaderMap)->String{
    // HTTP/2 can carry several Cookie fields. Combine them before checking for
    // duplicate names, so the first header cannot hide an ambiguous session.
    let mut result=String::new();
    for value in headers.get_all("cookie").iter(){
        let Ok(value)=value.to_str() else{return String::new()};
        if result.len()+value.len()+2>8192{return String::new()}
        if !result.is_empty(){result.push_str("; ");}result.push_str(value);
    }
    result
}
fn auth_error(error:AuthError)->Response{
    match error{
        AuthError::InvalidCredentials=>denied(StatusCode::UNAUTHORIZED,"Неверный код входа или сессия завершена"),
        AuthError::RateLimited=>{let mut r=denied(StatusCode::TOO_MANY_REQUESTS,"Слишком много попыток. Повторите через 5 минут");r.headers_mut().insert("retry-after","300".parse().unwrap());r},
        AuthError::Unavailable=>denied(StatusCode::SERVICE_UNAVAILABLE,"Вход временно недоступен"),
    }
}
pub async fn security(app:App,mut req:Request,next:Next)->Response{
    let host=req.headers().get("host").and_then(|v|v.to_str().ok()).unwrap_or("");
    let local=[format!("127.0.0.1:{}",app.port),format!("localhost:{}",app.port),format!("[::1]:{}",app.port)].iter().any(|h|h==host);
    let remote=app.public_origin.as_deref().is_some_and(|o|o.strip_prefix("https://")==Some(host));
    if !local&&!remote{return denied(StatusCode::FORBIDDEN,"Host not allowed")}
    if local&&["forwarded","x-forwarded-for","x-forwarded-host","x-forwarded-proto","cf-connecting-ip"].iter().any(|h|req.headers().contains_key(*h)){
        return denied(StatusCode::FORBIDDEN,"Forwarded requests cannot use local owner access")
    }
    let mutation=![Method::GET,Method::HEAD,Method::OPTIONS].contains(req.method());
    let expected=if local{format!("http://{host}")}else{app.public_origin.clone().unwrap()};
    let origin=req.headers().get("origin").and_then(|v|v.to_str().ok());
    if origin.is_some_and(|o|o!=expected)||(remote&&mutation&&origin!=Some(expected.as_str())){
        return denied(StatusCode::FORBIDDEN,"Same-origin request required")
    }
    if remote&&req.headers().get("sec-fetch-site").is_some_and(|v|v!="same-origin"&&v!="none"){
        return denied(StatusCode::FORBIDDEN,"Cross-site request rejected")
    }
    let path=req.uri().path();
    let public=!path.starts_with("/api/")||path=="/api/session/login"||path=="/api/health"||path=="/api/accounts";
    let actor=if local{Some(Actor::local_owner(&app.csrf))}else if path=="/api/accounts"{None}else{
        match &app.auth{
            Some(auth)=>match auth.authenticate(&cookie(req.headers())).await{Ok(actor)=>actor,Err(e)=>return auth_error(e)},
            None=>None,
        }
    };
    if !public&&actor.is_none(){return denied(StatusCode::UNAUTHORIZED,"Войдите по личному коду доступа")}
    if (path=="/api/feedback/report" || path.starts_with("/api/maintenance/") || path.starts_with("/api/engine/")) && !actor.as_ref().is_some_and(|a|a.role=="owner") {
        return denied(StatusCode::FORBIDDEN,"Экспорт обратной связи доступен только владельцу")
    }
    if mutation&&path!="/api/session/login"{
        let supplied=req.headers().get("x-csrf-token").and_then(|v|v.to_str().ok()).unwrap_or("");
        if !actor.as_ref().is_some_and(|a|a.valid_csrf(supplied)){return denied(StatusCode::FORBIDDEN,"CSRF token required")}
        if remote&&!operator_mutation(path){return denied(StatusCode::FORBIDDEN,"Действие доступно только владельцу")}
    }
    let generation=if path.starts_with("/api/") {
        match app.db.read_working_generation().await {
            Ok(value)=>value,
            Err(error)=>return error.into_response(),
        }
    }else{None};
    if path.starts_with("/api/") {
        if let Err(error)=working_generation::check_request(generation.as_deref(),req.headers(),mutation,
            matches!(path,"/api/session/login"|"/api/session/logout")){return error.into_response();}
    }
    if let Some(actor)=actor{req.extensions_mut().insert(actor);}
    let mut response=next.run(req).await;
    if let Some(generation)=generation {response.headers_mut().insert(working_generation::HEADER,generation.parse().unwrap());}
    for(key,value)in[("cache-control","no-store"),("x-content-type-options","nosniff"),("referrer-policy","same-origin"),("content-security-policy","default-src 'self'; style-src 'self' 'unsafe-inline'; img-src 'self' data: https:; frame-ancestors 'none'; base-uri 'none'; object-src 'none'")]{response.headers_mut().insert(axum::http::HeaderName::from_static(key),value.parse().unwrap());}
    response
}
fn operator_mutation(path:&str)->bool{
    path=="/api/session/logout"||path=="/api/conversations"||path.starts_with("/api/conversations/")||path.starts_with("/api/items/")||path=="/api/proposals"||path.starts_with("/api/proposals/")||path=="/api/approvals"||path.starts_with("/api/approvals/")||path.starts_with("/api/operations/")||path=="/api/feedback/events"||path=="/api/engine/reply-url-policy"
        ||(path.starts_with("/api/engine/posts/")&&path.ends_with("/media-policy"))
        ||(path.starts_with("/api/engine/posts/")&&path.ends_with("/audio-equivalence"))
        ||path=="/api/maintenance/media/cached-audio"
}
async fn media_exception_permission(app:&App,actor:&Actor)->bool{
    let (Some(auth),Some(generation))=(&app.auth,&actor.authority_generation) else{return false;};
    auth.can_override_missing_media(&actor.id,generation).await.unwrap_or(false)
}
pub async fn session(State(app):State<App>,Extension(actor):Extension<Actor>)->ApiResult<Json<Value>>{
    let mut v=actor.public_json();v["csrfToken"]=json!(actor.csrf_token);
    v["storageGeneration"]=json!(app.db.read_working_generation().await?);
    v["capabilities"]=json!({"canOverrideMissingMedia":media_exception_permission(&app,&actor).await});Ok(Json(v))
}
// Replace, never merge, any client attribution before calling domain handlers.
fn attributed(mut body:Value,actor:&Actor)->Value {
    if body.is_object(){body["_verifiedActor"]=actor.public_json();}
    body
}
pub async fn item_patch(State(app):State<App>,Extension(actor):Extension<Actor>,Path(key):Path<String>,Json(body):Json<Value>)->ApiResult<Json<Value>> {
    crate::item_patch(State(app),Path(key),Json(attributed(body,&actor))).await
}
pub async fn proposal_new(State(app):State<App>,Extension(actor):Extension<Actor>,Json(body):Json<Value>)->ApiResult<Json<Value>> {
    crate::proposal_new(State(app),Json(attributed(body,&actor))).await
}
pub async fn proposal_patch(State(app):State<App>,Extension(actor):Extension<Actor>,Path(key):Path<String>,Json(body):Json<Value>)->ApiResult<Json<Value>> {
    crate::proposal_patch(State(app),Path(key),Json(attributed(body,&actor))).await
}
pub async fn feedback_event(State(app):State<App>,Extension(actor):Extension<Actor>,Json(body):Json<Value>)->ApiResult<Json<Value>> {
    crate::feedback_event(State(app),Json(attributed(body,&actor))).await
}
pub async fn knowledge_catalog(State(app):State<App>,Extension(actor):Extension<Actor>)->ApiResult<Json<Value>> {
    Ok(Json(app.db.read_knowledge_catalog(actor.role=="owner").await?))
}
pub async fn knowledge_heads(State(app):State<App>,Query(query):Query<storage::KnowledgeHeadsQuery>)->ApiResult<Json<Value>> {
    Ok(Json(app.db.read_knowledge_heads(&query).await?))
}
#[derive(serde::Deserialize)]
#[serde(rename_all="camelCase",deny_unknown_fields)]
pub(crate) struct KnowledgeEntryQuery { expected_version_id:Option<String> }
pub async fn knowledge_entry(State(app):State<App>,Path(key):Path<String>,Query(query):Query<KnowledgeEntryQuery>)->ApiResult<Json<Value>> {
    Ok(Json(app.db.read_knowledge_entry(&key,query.expected_version_id.as_deref()).await?))
}
pub async fn knowledge_version(State(app):State<App>,Path(key):Path<String>)->ApiResult<Json<Value>> {
    Ok(Json(app.db.read_knowledge_version(&key).await?))
}
pub async fn instruction_catalog(State(app):State<App>,Extension(_actor):Extension<Actor>)->ApiResult<Json<Value>> {
    // Middleware establishes access to this single-account workspace; neither
    // query parameters nor browser attribution can select another account.
    Ok(Json(app.db.read_instruction_catalog().await?))
}
pub async fn search(State(app):State<App>,axum::extract::Query(query):axum::extract::Query<HashMap<String,String>>)->ApiResult<Json<Value>>{
    let query_text=assistant_context::validate_search_query(query.get("q").map(String::as_str).unwrap_or("")).map_err(bad)?;
    let data=app.db.read_search_context().await?;
    Ok(Json(assistant_context::search(&data,query_text,20).map_err(bad)?))
}
pub async fn login(State(app):State<App>,Json(body):Json<Value>)->Response{
    let Some(auth)=app.auth else{return denied(StatusCode::FORBIDDEN,"Remote access is disabled")};
    let token=body["token"].as_str().unwrap_or("");
    if token.len()>256{return auth_error(AuthError::InvalidCredentials)}
    match auth.login(token).await{Ok(result)=>{let mut value=result.actor.public_json();value["csrfToken"]=json!(result.actor.csrf_token);let mut response=Json(value).into_response();response.headers_mut().insert("set-cookie",result.set_cookie.parse().unwrap());response},Err(e)=>auth_error(e)}
}
pub async fn logout(State(app):State<App>,headers:HeaderMap)->Response{
    let mut response=Json(json!({"ok":true})).into_response();
    if let Some(auth)=app.auth{if let Err(e)=auth.logout(&cookie(&headers)).await{return auth_error(e)}response.headers_mut().insert("set-cookie",auth.clear_cookie().parse().unwrap());}
    response
}
pub fn owned(value:&Value,actor:&Actor)->bool{value["operatorId"].as_str().unwrap_or("local-owner")==actor.id}
fn actor_bootstrap(mut data:Value,actor:&Actor,external_writes:bool)->Value {
    list_mut(&mut data,"conversations").retain(|c|owned(c,actor));
    let conversations:std::collections::HashSet<String>=list(&data,"conversations").iter().filter_map(|c|c["id"].as_str().map(str::to_owned)).collect();
    list_mut(&mut data,"jobs").retain(|j|match j["kind"].as_str() {
        Some("assistant")=>conversations.contains(j["refId"].as_str().unwrap_or("")),
        Some("editorial_review")=>actor.role=="owner"||j["operatorId"]==actor.id,
        _=>true,
    });
    let mut view=bootstrap_view(data,&actor.csrf_token);
    view["operator"]=actor.public_json();
    view["settings"]["privateAssistant"]=json!(true);
    view["settings"]["externalWritesEnabled"]=json!(external_writes);
    view
}
pub async fn bootstrap(State(app):State<App>,Extension(actor):Extension<Actor>)->ApiResult<Json<Value>>{
    let mut view=actor_bootstrap(app.read_bootstrap().await?,&actor,app.external_writes);
    view["operator"]["capabilities"]=json!({"canOverrideMissingMedia":media_exception_permission(&app,&actor).await});
    Ok(Json(view))
}
pub async fn bootstrap_delta(State(app):State<App>,Extension(actor):Extension<Actor>,axum::extract::Query(query):axum::extract::Query<HashMap<String,String>>)->ApiResult<Json<Value>> {
    // Capture before loading: the last accepted snapshot remains available even
    // if it exceeds the history byte budget and is about to be replaced.
    let base=query.get("since").filter(|value|value.len()<=128).and_then(|version|app.bootstrap_cache.snapshot(version));
    let capabilities=json!({"canOverrideMissingMedia":media_exception_permission(&app,&actor).await});
    let mut current=actor_bootstrap(app.read_bootstrap().await?,&actor,app.external_writes);
    current["operator"]["capabilities"]=capabilities.clone();
    let mut result=match base {
        Some(base)=>crate::workspace_delta::between(&actor_bootstrap((*base).clone(),&actor,app.external_writes),&current,&actor.id),
        None=>json!({"kind":"full","snapshot":current}),
    };
    // Session tokens belong to this response, never the shared snapshot. Always
    // refresh them even when the workspace generation and operator ID match.
    if result["kind"]=="delta" {
        result["set"]["csrfToken"]=json!(actor.csrf_token);
        result["set"]["operator"]=actor.public_json();
        result["set"]["operator"]["capabilities"]=capabilities;
    }
    Ok(Json(result))
}

