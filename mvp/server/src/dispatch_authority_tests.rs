use super::*;
use sha2::{Digest, Sha256};

const TOKEN: &str = "alice-test-access-key-0123456789abcdef0123456789abcdef";
pub(crate) struct Harness {
    pub(crate) app: App,
    pub(crate) actor: Actor,
    pub(crate) access: PathBuf,
    log: PathBuf,
    _temp: tempfile::TempDir,
}
impl Harness {
    pub(crate) async fn new(revoke_on: &str) -> Self {
        let temp = tempfile::tempdir().unwrap();
        let access = temp.path().join("access.json");
        std::fs::write(&access, access_config(TOKEN)).unwrap();
        let auth = operator_auth::Auth::open(temp.path(), access.clone())
            .await
            .unwrap();
        let actor = auth.login(TOKEN).await.unwrap().actor;
        let db = open_db(&temp.path().join("workspace.sqlite"))
            .await
            .unwrap();
        let (events, _) = broadcast::channel(8);
        let log = temp.path().join("calls.jsonl");
        let bridge = temp.path().join("isolated-fake.mjs");
    let script = r#"import {appendFile,writeFile,readFile} from 'node:fs/promises';
let input='';for await(const c of process.stdin)input+=c;const r=JSON.parse(input);
const itemId=r.itemId??r.actions[0].itemId;
await appendFile(__LOG__,JSON.stringify({operation:r.operation,itemId})+'\n');
if(r.operation==='execute'&&__REVOKE__==='gate_execute'){
 const until=Date.now()+10000;
 while(true){try{await readFile(__LOG__+'.release');break;}catch{if(Date.now()>until)throw new Error('isolated execute gate timed out');await new Promise(resolve=>setTimeout(resolve,10));}}
}
if(r.operation===__REVOKE__)await writeFile(__ACCESS__,'{"operators":[]}');
const result=r.operation==='context'?{itemId,objectId:'11391',postKey:'11391:post-1',conversationKey:'11391:'+itemId,contextEvidenceDigest:'a'.repeat(64)}:{account:r.account,results:r.actions.map(a=>({actionId:a.actionId,itemId:a.itemId,status:'verified'}))};
process.stdout.write(JSON.stringify({ok:true,result}));"#
            .replace("__LOG__", &json!(log.to_string_lossy()).to_string())
            .replace("__ACCESS__", &json!(access.to_string_lossy()).to_string())
            .replace("__REVOKE__", &json!(revoke_on).to_string());
        std::fs::write(&bridge, script).unwrap();
        let app = App {lifecycle_task_count: Default::default(), lifecycle_admission: Arc::new(crate::runtime_lifecycle_startup::Admission::fixture(crate::accounts::Profile::LikeAvto)), lifecycle_owner: Arc::new(crate::runtime_lifecycle_startup::Admission::fixture(crate::accounts::Profile::LikeAvto).identity().clone()), lifecycle_provider_token: Default::default(), lifecycle_work: Default::default(), media_discovery: Default::default(),preparation_wake: Default::default(),provider_session: Default::default(),
            account:crate::accounts::Profile::LikeAvto,navigation:crate::account_navigation::Navigation::root(),
            db: Database::Sqlite(db),
            gate: Arc::new(crate::writer_gate::WriterGate::default()),
            execution_gate: Arc::new(Mutex::new(())),
            preparation_workers: Default::default(),editorial_gate: Default::default(),assistant_gate: Arc::new(Mutex::new(())),assistant_chat_gate: Arc::new(Mutex::new(())),
            events,
            csrf: "owner-test".into(),
            auth: Some(auth),
            public_origin: None,
            // Only the isolated network-free script above; no live bridge or data.
            external_writes: true,
            port: 0,
            data: temp.path().to_owned(),
            bridge,
            node: PathBuf::from(
                "C:/Users/hello/AppData/Local/Microsoft/WinGet/Packages/OpenJS.NodeJS.LTS_Microsoft.Winget.Source_8wekyb3d8bbwe/node-v24.15.0-win-x64/node.exe",
            ),
            tasks: Arc::new(Mutex::new(HashMap::new())),
            bootstrap_cache: Arc::new(bootstrap_cache::Cache::default()),
        };
        crate::runtime_lifecycle_app::initialize_app_fixture(&app).await.unwrap();
        app.change(|d| {
            d["items"] = json!((1..=2).map(|n| json!({"id":format!("item-{n}"),"itemId":format!("comment-{n}"),"objectId":"11391","postKey":"11391:post-1","conversationKey":format!("11391:comment-{n}"),"contextEvidenceDigest":"a".repeat(64),"providerStatus":"new","revision":1,"workflow":"attention"})).collect::<Vec<_>>());
            connection_gate::fixture_open(d)?;
            Ok(())
        }).await.unwrap();
        Self {
            app,
            actor,
            access,
            log,
            _temp: temp,
        }
    }
    pub(crate) async fn approval(&self) -> String {
        self.approval_with_kind("close").await
    }
    async fn approval_with_kind(&self, kind: &str) -> String {
        let mut proposals = vec![];
        for n in 1..=2 {
            let mut request = json!({
                "itemId":format!("item-{n}"),
                "kind":kind,
                "expectedRevision":1
            });
            if kind == "reply_and_close" {
                request["text"] = json!(format!("Reviewed response {n}"));
            }
            let Json(p) = proposal_new(State(self.app.clone()), Json(request))
                .await
                .unwrap();
            proposals.push(json!({"id":p["id"],"revision":p["revision"]}));
        }
        let Json(a) = approval_new(
            State(self.app.clone()),
            axum::Extension(self.actor.clone()),
            Json(json!({"proposals":proposals})),
        )
        .await
        .unwrap();
        assert!(a.get("approvalAuthority").is_none());
        a["id"].as_str().unwrap().to_owned()
    }
    pub(crate) async fn enqueue(&self, approval: String) {
        let _ = execute(
            State(self.app.clone()),
            axum::Extension(self.actor.clone()),
            Path(approval),
        )
        .await
        .unwrap();
    }
    pub(crate) async fn finished(&self) -> Value {
        tokio::time::timeout(Duration::from_secs(15), async {
            loop {
                let data = self.app.read().await.unwrap();
                if list(&data, "operations").len() == 2
                    && list(&data, "operations")
                        .iter()
                        .all(|op| op["status"] != "dispatching")
                    && list(&data, "jobs")
                        .iter()
                        .filter(|job|job["kind"]!="conductor")
                        .all(|job| matches!(job["status"].as_str(), Some("completed" | "failed")))
                {
                    return data;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap()
    }
    pub(crate) fn calls(&self) -> Vec<Value> {
        std::fs::read_to_string(&self.log)
            .unwrap_or_default()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }
    pub(crate) fn release_execute(&self) {std::fs::write(self.log.with_extension("jsonl.release"),"").unwrap();}
}
fn access_config(token: &str) -> String {
    json!({"operators":[{"id":"alice","name":"Alice","tokenHash":format!("{:x}",Sha256::digest(token.as_bytes()))}]}).to_string()
}

#[tokio::test]
async fn revoked_rotated_or_unreadable_while_waiting_has_zero_bridge_calls() {
    for mode in ["removed", "rotated", "unreadable"] {
        let h = Harness::new("").await;
        let approval = h.approval().await;
        let guard = h.app.execution_gate.lock().await;
        h.enqueue(approval).await;
        match mode {
            "removed" => std::fs::write(&h.access, "{\"operators\":[]}").unwrap(),
            "rotated" => std::fs::write(&h.access, access_config("new-key-generation")).unwrap(),
            _ => std::fs::remove_file(&h.access).unwrap(),
        }
        drop(guard);
        let data = h.finished().await;
        assert!(h.calls().is_empty(), "{mode}");
        assert!(
            list(&data, "operations")
                .iter()
                .all(|op| op["status"] == "stale" && op["evidence"]["authorityDenied"] == true),
            "{mode}"
        );
    }
}

#[tokio::test]
async fn revoke_during_context_refresh_prevents_execute() {
    let h = Harness::new("context").await;
    h.enqueue(h.approval().await).await;
    let data = h.finished().await;
    let calls = h.calls();
    // Both independent operations may already be in their read-only context
    // refresh when either refresh revokes authority. Neither may cross the
    // second authority check into execute.
    assert!(!calls.is_empty());
    assert!(calls.iter().all(|call| call["operation"] == "context"));
    assert!(
        list(&data, "operations")
            .iter()
            .all(|op| op["status"] == "stale" && op["evidence"]["authorityDenied"] == true)
    );
}

#[tokio::test]
async fn revoke_mid_batch_keeps_effect_and_readback_but_stops_queued_conversation_reply() {
    let h = Harness::new("execute").await;
    h.app
        .change(|data| {
            let conversation = data["items"][0]["conversationKey"].clone();
            data["items"][1]["conversationKey"] = conversation;
            Ok(())
        })
        .await
        .unwrap();
    let refs=h.app.change(|d| {
        for n in 1..=2 {crate::tests::create_post_fixture(d,&format!("item-{n}"))?;}
        let mut refs=vec![];
        for n in 1..=2 {
            let p=create_proposal(d,&json!({"itemId":format!("item-{n}"),"kind":"reply_and_close","expectedRevision":1,
                "text":format!("Reviewed response {n}")}))?;
            editorial_review::fixture_accept(d,p["id"].as_str().unwrap()).map_err(bad)?;
            refs.push(json!({"id":p["id"],"revision":p["revision"]}));
        }
        Ok(json!(refs))
    }).await.unwrap();
    let approval=approval_new(State(h.app.clone()),axum::Extension(h.actor.clone()),Json(json!({"proposals":refs})))
        .await.unwrap().0["id"].as_str().unwrap().to_owned();
    h.enqueue(approval).await;
    let data = h.finished().await;
    let calls = h.calls();
    assert_eq!(
        calls
            .iter()
            .map(|v| v["operation"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["context", "execute", "readback"]
    );
    assert_eq!(data["operations"][0]["status"], "succeeded");
    assert_eq!(data["operations"][1]["status"], "stale");
    assert_eq!(data["operations"][1]["evidence"]["authorityDenied"], true);
    assert_eq!(data["items"][0]["providerStatus"], "closed");
    assert_eq!(data["items"][1]["providerStatus"], "new");
}

#[tokio::test]
async fn generation_change_requires_new_review_and_bindings_stay_out_of_bootstrap() {
    let h = Harness::new("").await;
    let key = h.approval().await;
    std::fs::write(
        &h.access,
        access_config("rotated-0123456789abcdef0123456789abcdef"),
    )
    .unwrap();
    let replacement = h
        .app
        .auth
        .as_ref()
        .unwrap()
        .login("rotated-0123456789abcdef0123456789abcdef")
        .await
        .unwrap()
        .actor;
    let data = h.app.read().await.unwrap();
    assert!(admit(&data["approvals"][0], &replacement).is_err());
    let view = bootstrap_view(data.clone(), "csrf");
    assert!(view["approvals"][0].get("approvalAuthority").is_none());
    assert!(
        !view
            .to_string()
            .contains(h.actor.authority_generation.as_ref().unwrap())
    );
    let mut legacy = data["approvals"][0].clone();
    legacy.as_object_mut().unwrap().remove("approvalAuthority");
    assert!(admit(&legacy, &replacement).is_err());
    assert!(admit(&legacy, &Actor::local_owner("csrf")).is_err());
    assert!(admit(&json!({"id":key}), &Actor::local_owner("csrf")).is_ok());
    let mut op = json!({"approvedBy":h.actor.public_json(),"executedBy":h.actor.public_json()});
    assert!(check(&h.app, &op).await.is_err());
    op["dispatchAuthority"] = json!({"approved":approval_binding(&h.actor),"executed":approval_binding(&Actor::local_owner("csrf"))});
    assert!(check(&h.app, &op).await.is_err());
}

#[tokio::test]
async fn local_owner_legacy_admission_survives_disabled_remote_auth() {
    let mut h = Harness::new("").await;
    h.app.auth = None;
    let owner = Actor::local_owner("csrf");
    for approved in [json!({}), json!({"approvedBy":owner.public_json()})] {
        let binding = admit(&approved, &owner).unwrap();
        let op = json!({"approvedBy":approved["approvedBy"],"executedBy":owner.public_json(),"dispatchAuthority":binding});
        assert!(check(&h.app, &op).await.is_ok());
        let mut data = empty();
        data["operations"] = json!([op]);
        let view = bootstrap_view(data, "csrf");
        assert!(view["operations"][0].get("dispatchAuthority").is_none());
    }
    assert!(admit(&json!({}), &h.actor).is_err());
    let missing = json!({"approvedBy":owner.public_json(),"executedBy":owner.public_json()});
    assert!(
        check(&h.app, &missing).await.is_err(),
        "Unbound old queued work never gains dispatch authority"
    );
}
#[tokio::test]
async fn local_retirement_failure_stops_new_permits_even_if_persisted_gate_still_looks_open() {
    let (app,_temp)=crate::tests::test_app().await;
    let (independent,_other_temp)=crate::tests::test_app().await;
    app.change(|d|connection_gate::fixture_open(d)).await.unwrap();
    require_unheld(&app).unwrap();hold_transport_failure(&app);
    assert!(require_unheld(&app.clone()).is_err());assert!(require_unheld(&independent).is_ok());
    assert!(begin(&app,&json!({"id":"new-operation","attemptId":"new-attempt"})).await.is_err());
    // A durable ready observation or direct fixture reopen cannot clear this
    // hold: production clears it only after verified root case admission.
    app.change(|d|connection_gate::fixture_open(d)).await.unwrap();
    assert!(require_unheld(&app).is_err());let d=app.read().await.unwrap();
    assert_eq!(d[connection_gate::FIELD]["state"],"open");assert!(list(&d,"operations").iter().all(|op|op.get(connection_gate::PERMIT_FIELD).is_none()));
}

#[tokio::test]
async fn recovery_cannot_clear_a_failure_observed_after_its_protected_read_started() {
    let (app,_temp)=crate::tests::test_app().await;
    hold_transport_failure(&app);
    let original=capture_recovery_hold(&app).unwrap();
    // The controlled ordering is the actual race: a newly classified failure
    // arrives while the earlier recovery is inspecting the protected state.
    hold_transport_failure(&app);
    assert!(clear_after_recovery_admission(&app,&original).is_err());
    assert!(require_unheld(&app).is_err());
    let current=capture_recovery_hold(&app).unwrap();
    clear_after_recovery_admission(&app,&current).unwrap();
    require_unheld(&app).unwrap();
}

#[tokio::test]
async fn no_hold_snapshot_and_previously_cleared_token_cannot_erase_a_later_failure() {
    let (app,_temp)=crate::tests::test_app().await;
    let initially_clear=capture_recovery_hold(&app).unwrap();
    hold_transport_failure(&app);
    assert!(clear_after_recovery_admission(&app,&initially_clear).is_err());
    let admitted=capture_recovery_hold(&app).unwrap();
    clear_after_recovery_admission(&app,&admitted).unwrap();
    hold_transport_failure(&app);
    assert!(clear_after_recovery_admission(&app,&admitted).is_err());
    assert!(require_unheld(&app).is_err());
}

#[tokio::test]
async fn recovery_hold_token_is_scoped_to_the_original_company_workspace() {
    let (app,_temp)=crate::tests::test_app().await;
    let (other,_other_temp)=crate::tests::test_app().await;
    hold_transport_failure(&app);hold_transport_failure(&other);
    let original=capture_recovery_hold(&app).unwrap();
    assert!(clear_after_recovery_admission(&other,&original).is_err());
    assert!(require_unheld(&other).is_err());
    clear_after_recovery_admission(&app,&original).unwrap();
    require_unheld(&app).unwrap();assert!(require_unheld(&other).is_err());
}

#[tokio::test]
async fn exhausted_failure_generation_cannot_accept_a_same_counter_token() {
    let (app,_temp)=crate::tests::test_app().await;
    TRANSPORT_FAILURE_HOLDS.get_or_init(Default::default).lock().unwrap().insert(
        hold_key(&app),HoldState{generation:u64::MAX,held:true,exhausted:false});
    let last=capture_recovery_hold(&app).unwrap();hold_transport_failure(&app);
    assert!(capture_recovery_hold(&app).is_err());
    assert!(clear_after_recovery_admission(&app,&last).is_err());
    assert!(require_unheld(&app).is_err());
}
