//! Bounded local repair of provider-proven author roles. Never calls a model or provider.
use super::*;
use std::collections::BTreeSet;

fn binding() -> Value {
    json!({"accountId":"LikeAvto","connector":"angryspace","id":"angryspace-likeavto-v1",
        "providerAccountId":"likeavto","revision":1,"workspaceId":"local-pilot"})
}
fn matching_binding(value: &Value) -> bool {
    binding().as_object().unwrap().iter().all(|(key, expected)| value.get(key) == Some(expected))
}
// Exact read-only provider observations, 2026-09-22. Each sample had official=1.
const AUTHORS: [(&str, &str, &str); 3] = [
    ("11341", "provider:vk_-135891342", "6aad6798e38a2a6b20e03d4d"),
    ("11389", "provider:youtube_UCSwrR_qTcXvjKVxgrO1v1pQ", "6aaeaacb6aa20d243282a55b"),
    ("11391", "provider:instagram_likeavto_import", "6aad69526aa20d2432aa00d7"),
];

fn repair_workspace(d: &mut Value) -> ApiResult<Value> {
    if d["account"] != "LikeAvto" || !matching_binding(&d["connectorBinding"]) {
        return Err(conflict("Brand repair workspace binding mismatch"));
    }
    let items = list(d,"items").to_vec();
    let default_binding = d["connectorBinding"].clone();
    let mut stamps = 0usize;
    let mut role_changes = 0usize;
    let mut comments = BTreeSet::new();
    let mut changed_branches = BTreeSet::new();
    for branch in list_mut(d,"branches") {
        let owners: Vec<_> = items.iter().filter(|item| item["branchId"] == branch["id"] && item["postId"] == branch["postId"]).collect();
        if owners.is_empty() || owners.iter().any(|item| !matching_binding(item.get("connectorBinding").unwrap_or(&default_binding))) { continue; }
        if owners.iter().any(|item| item.get("providerObjectId").or_else(||item.get("objectId")).and_then(Value::as_str).is_none()) { continue; }
        let objects: BTreeSet<_> = owners.iter().filter_map(|item| item.get("providerObjectId").or_else(||item.get("objectId")).and_then(Value::as_str)).collect();
        if objects.len()!=1 { continue; }
        let object = *objects.first().unwrap();
        let branch_id = branch["id"].as_str().unwrap_or("").to_owned();
        for field in ["messages", "observedMessages"] {
            let Some(messages) = branch[field].as_array_mut() else { continue; };
            for message in messages {
                if message["providerObjectId"] != object || message["providerItemId"].as_str().is_none_or(str::is_empty) { continue; }
                if !AUTHORS.iter().any(|(obj,author,_)| *obj==object && message["authorId"]==*author) { continue; }
                if message["role"]=="brand" && message["providerOfficial"]==true { continue; }
                if message["role"]!="brand" { role_changes+=1; }
                stamps+=1;
                comments.insert((object.to_owned(),message["providerItemId"].as_str().unwrap().to_owned()));
                changed_branches.insert(branch_id.clone());
                message["role"]=json!("brand");
                message["providerOfficial"]=json!(true);
                message["roleEvidence"]=json!("verified-provider-author");
            }
        }
    }
    if stamps>0 {
        // Reconcile real context changes through the ordinary versioning path.
        // Draft text is retained; stale preparation must not be silently reapproved.
        merge_snapshot(d,&json!({}))?;
    }
    Ok(json!({"roleChanges":role_changes,"evidenceStamps":stamps,"uniqueComments":comments.len(),
        "affectedBranches":changed_branches.len(),"manifest":"official-author-2026-09-22"}))
}

pub(super) async fn repair(State(app): State<App>, Json(body): Json<Value>) -> ApiResult<Json<Value>> {
    let apply=match body.get("apply") { None=>false, Some(value)=>value.as_bool().ok_or_else(||bad("apply must be boolean"))? };
    let mut report = if apply {
        app.change(|d| {
            let report=repair_workspace(d)?;
            if report["evidenceStamps"].as_u64().unwrap_or(0)>0 {
                audit(d,"brand_roles_repaired",&report.to_string());
            }
            Ok(report)
        }).await?
    } else {
        let mut snapshot=app.read().await?;
        repair_workspace(&mut snapshot)?
    };
    report["applied"]=json!(apply);
    Ok(Json(report))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn data() -> Value {
        let mut d=empty();d["connectorBinding"]=binding();
        for field in ["knowledge_entries","knowledge_versions","feedback"] { d[field]=json!([]); }
        d["items"]=json!([{"id":"i","branchId":"b","postId":"p","objectId":"11341","draft":"Keep human text","draftEdited":true,"workflow":"attention","revision":1}]);
        let msg=json!({"id":"brand","providerItemId":"source","providerObjectId":"11341","authorId":"provider:vk_-135891342","role":"participant","text":"Brand text"});
        d["branches"]=json!([{"id":"b","postId":"p","messages":[msg.clone()],"observedMessages":[msg]}]);d
    }
    #[test]
    fn repairs_roles_preserves_human_draft_and_is_idempotent() {
        let mut d=data();let report=repair_workspace(&mut d).unwrap();
        assert_eq!(report["roleChanges"],2);assert_eq!(d["branches"][0]["messages"][0]["role"],"brand");
        assert_eq!(d["items"][0]["draft"],"Keep human text");assert_eq!(d["items"][0]["draftEdited"],true);
        let before=d.clone();assert_eq!(repair_workspace(&mut d).unwrap()["evidenceStamps"],0);assert_eq!(d,before);
        assert!(list(&d,"jobs").is_empty());
    }
    #[test]
    fn rejects_wrong_workspace_and_ignores_spoofed_names_other_objects_and_bindings() {
        let mut d=data();d["connectorBinding"]["revision"]=json!(2);assert!(repair_workspace(&mut d).is_err());
        for mode in ["name", "object", "binding"] {
            let mut d=data();
            if mode=="binding" { d["items"][0]["connectorBinding"]=json!({"id":"other"}); }
            else { for field in ["messages","observedMessages"] {
                if mode=="name" { d["branches"][0][field][0]["authorId"]=json!("attacker");d["branches"][0][field][0]["author"]=json!("LikeAvto"); }
                else { d["branches"][0][field][0]["providerObjectId"]=json!("11391"); }
            } }
            let before=d.clone();assert_eq!(repair_workspace(&mut d).unwrap()["evidenceStamps"],0);assert_eq!(d,before);
        }
    }
    #[tokio::test]
    async fn handler_defaults_to_read_only_and_apply_is_audited_without_jobs() {
        let temp=tempfile::tempdir().unwrap();
        let db=open_db(&temp.path().join("workspace.sqlite")).await.unwrap();
        let (events,_)=broadcast::channel(8);
        let app=App{lifecycle_task_count: Default::default(),lifecycle_admission: Arc::new(crate::runtime_lifecycle_startup::Admission::fixture(crate::accounts::Profile::LikeAvto)),lifecycle_owner: Arc::new(crate::runtime_lifecycle_startup::Admission::fixture(crate::accounts::Profile::LikeAvto).identity().clone()),lifecycle_provider_token: Default::default(),lifecycle_work: Default::default(),media_discovery: Default::default(),preparation_wake: Default::default(),provider_session: Default::default(),account:crate::accounts::Profile::LikeAvto,navigation:crate::account_navigation::Navigation::root(),db:Database::Sqlite(db),gate:Arc::new(crate::writer_gate::WriterGate::default()),execution_gate:Arc::new(Mutex::new(())),
        preparation_workers: Default::default(),editorial_gate: Default::default(),assistant_gate: Arc::new(Mutex::new(())),assistant_chat_gate: Arc::new(Mutex::new(())),events,csrf:id(),auth:None,public_origin:None,external_writes:false,port:4186,
            data:temp.path().to_owned(),bridge:temp.path().join("never-execute"),node:temp.path().join("no-runtime"),tasks:Arc::new(Mutex::new(HashMap::new())),bootstrap_cache:Arc::new(bootstrap_cache::Cache::default())};
        // Seed the isolated fixture before its explicit native ownership bootstrap.
        app.db.change(|d|{*d=data();Ok(())}).await.unwrap();
        crate::runtime_lifecycle_app::initialize_app_fixture(&app).await.unwrap();
        let before=app.read().await.unwrap();
        let Json(report)=repair(State(app.clone()),Json(json!({}))).await.unwrap();
        assert_eq!(report["applied"],false);assert_eq!(report["roleChanges"],2);
        assert_eq!(app.read().await.unwrap(),before);
        let Json(report)=repair(State(app.clone()),Json(json!({"apply":true}))).await.unwrap();
        assert_eq!(report["applied"],true);
        let after=app.read().await.unwrap();
        assert_eq!(after["items"][0]["draft"],before["items"][0]["draft"]);
        assert_eq!(after["audit"].as_array().unwrap().last().unwrap()["action"],"brand_roles_repaired");
        assert!(list(&after,"jobs").is_empty());
        let Json(second)=repair(State(app.clone()),Json(json!({"apply":true}))).await.unwrap();
        assert_eq!(second["evidenceStamps"],0);assert_eq!(app.read().await.unwrap(),after);
        app.db.close().await;
    }
}
