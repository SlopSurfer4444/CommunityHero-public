//! Exercise the real router/middleware over isolated loopback TCP, never live data.
use super::*;
use sha2::{Digest, Sha256};

const ORIGIN: &str = "https://pilot.example.test";
const HOST: &str = "pilot.example.test";
const ALICE_TOKEN: &str = "alice-0123456789abcdef0123456789abcdef0123456789abcdef";
const BOB_TOKEN: &str = "bob-0123456789abcdef0123456789abcdef0123456789abcdef";

#[tokio::test]
async fn account_navigation_is_public_and_sessions_stay_with_their_company() {
    let entries=r#"[{"id":"baw-russia","label":"BAW Russia","url":"/baw/"},{"id":"likeavto","label":"LikeAvto","url":"/likeavto/"}]"#;
    let baw=Pilot::start_scoped(accounts::Profile::BawRussia,"/baw/",entries).await;
    let likeavto=Pilot::start_scoped(accounts::Profile::LikeAvto,"/likeavto/",entries).await;
    let navigation=baw.request("GET","/api/accounts",&[],None).await;
    assert_eq!(navigation.status,200);
    assert_eq!(navigation.body["account"],"BAW Russia");
    assert_eq!(navigation.body["basePath"],"/baw/");
    assert_eq!(navigation.body["accounts"][1]["url"],format!("{ORIGIN}/likeavto/"));
    assert_eq!(navigation.body.as_object().unwrap().len(),3);
    assert_eq!(baw.request("GET","/api/bootstrap",&[],None).await.status,401);
    let (baw_cookie,baw_csrf)=baw.login(ALICE_TOKEN).await;
    let (likeavto_cookie,_)=likeavto.login(ALICE_TOKEN).await;
    let combined=format!("{baw_cookie}; {likeavto_cookie}");
    for pilot in [&baw,&likeavto] {
        assert_eq!(pilot.request("GET","/api/session",&[("cookie",&combined)],None).await.status,200);
    }
    assert_eq!(baw.request("GET","/api/session",&[("cookie",&likeavto_cookie)],None).await.status,401);
    assert_eq!(baw.request("GET","/api/session",&[("cookie",&baw_cookie),("cookie",&baw_cookie)],None).await.status,401);
    let logout=baw.request("POST","/api/session/logout",&[("cookie",&combined),("origin",ORIGIN),("x-csrf-token",&baw_csrf)],Some(json!({}))).await;
    assert_eq!(logout.status,200);
    assert!(logout.headers["set-cookie"].starts_with("__Secure-communityhero_session_baw=; Path=/baw/;"));
    assert_eq!(baw.request("GET","/api/session",&[("cookie",&combined)],None).await.status,401);
    assert_eq!(likeavto.request("GET","/api/session",&[("cookie",&combined)],None).await.status,200);
}

#[tokio::test]
async fn audio_equivalence_http_requires_owner_csrf_and_exact_cas() {
    let pilot=Pilot::start_with_account(accounts::Profile::BawRussia).await;
    pilot.app.change(|d|{let fixture=media_audio_equivalence::tests::fixture();
        for key in ["posts","materials","knowledge_entries","knowledge_versions"]{d[key]=fixture[key].clone();}Ok(())}).await.unwrap();
    let path="/api/engine/posts/target/audio-equivalence";
    let host=format!("127.0.0.1:{}",pilot.port);let local=[("host",host.as_str())];
    let view=pilot.request("GET",&format!("{path}?sourcePostId=source"),&local,None).await;
    assert_eq!(view.status,200,"{}",view.body);
    let candidate=&view.body["source"]["transcriptCandidates"][0];
    let body=json!({"expectedHeadSha256":view.body["headSha256"],"expectedTargetSourceVersion":view.body["targetSourceVersion"],
        "sourcePostId":"source","expectedSourceVersion":view.body["source"]["sourceVersion"],
        "transcriptVersionId":candidate["versionId"],"transcriptHash":candidate["hash"],"reason":"Owner confirms same video"});
    assert_eq!(pilot.request("PUT",path,&local,Some(body.clone())).await.status,403);
    let (cookie,csrf)=pilot.login(ALICE_TOKEN).await;
    let remote=[("cookie",cookie.as_str()),("x-csrf-token",csrf.as_str()),("origin",ORIGIN)];
    assert_eq!(pilot.request("PUT",path,&remote,Some(body.clone())).await.status,403);
    let write=[("host",host.as_str()),("x-csrf-token","local-owner-csrf")];
    let accepted=pilot.request("PUT",path,&write,Some(body.clone())).await;
    assert_eq!(accepted.status,200,"{}",accepted.body);assert_eq!(accepted.body["usable"],true);
    assert_eq!(pilot.request("PUT",path,&write,Some(body)).await.status,409);
    let revoke=json!({"expectedHeadSha256":accepted.body["headSha256"],"reason":"Owner revokes"});
    assert_eq!(pilot.request("DELETE",path,&local,Some(revoke.clone())).await.status,403);
    let revoked=pilot.request("DELETE",path,&write,Some(revoke)).await;
    assert_eq!(revoked.status,200,"{}",revoked.body);assert_eq!(revoked.body["usable"],false);
}

#[tokio::test]
async fn typed_reply_url_policy_http_is_owner_scoped_and_csrf_protected() {
    let pilot=Pilot::start().await;
    pilot.app.change(|d|{d["connectorBinding"]=accounts::Profile::LikeAvto.binding();Ok(())}).await.unwrap();
    let path="/api/engine/reply-url-policy";
    let host=format!("127.0.0.1:{}",pilot.port);
    let local=[("host",host.as_str())];
    let absent=pilot.request("GET",path,&local,None).await;
    assert_eq!(absent.status,200,"{}",absent.body);
    assert_eq!(absent.body["configured"],false);
    assert!(absent.body["values"].is_null());
    let body=json!({"requestId":"http-create","expectedVersionId":null,
        "values":["https://likeavto.example/official"]});
    assert_eq!(pilot.request("PUT",path,&local,Some(body.clone())).await.status,403);
    let (cookie,csrf)=pilot.login(ALICE_TOKEN).await;
    let remote=[("cookie",cookie.as_str()),("x-csrf-token",csrf.as_str()),("origin",ORIGIN)];
    assert_eq!(pilot.request("PUT",path,&remote,Some(body.clone())).await.status,403);
    let local_write=[("host",host.as_str()),("x-csrf-token","local-owner-csrf")];
    let created=pilot.request("PUT",path,&local_write,Some(body)).await;
    assert_eq!(created.status,200,"{}",created.body);
    assert_eq!(created.body["source"],"typed");
    let material_id=pilot.app.read().await.unwrap()["knowledge_entries"][0]["sourceMaterialId"].as_str().unwrap().to_owned();
    let patch=pilot.request("PATCH",&format!("/api/materials/{material_id}"),&local_write,
        Some(json!({"expectedRevision":1,"title":"Overwrite","text":"No URL gate"}))).await;
    assert_eq!(patch.status,409,"{}",patch.body);
    let current=pilot.request("GET",path,&local,None).await;
    assert_eq!(current.status,200,"{}",current.body);
    assert_eq!(current.body["values"],json!(["https://likeavto.example/official"]));
    assert_eq!(current.body["currentVersionId"],created.body["currentVersionId"]);
    let foreign=json!({"requestId":"foreign","expectedVersionId":current.body["currentVersionId"],
        "values":[],"account":"BAW Russia"});
    assert_eq!(pilot.request("PUT",path,&local_write,Some(foreign)).await.status,409);
}

#[tokio::test]
async fn baw_instruction_catalog_http_is_available_to_its_authenticated_operator() {
    let pilot=Pilot::start_with_account(crate::accounts::Profile::BawRussia).await;
    assert_eq!(pilot.request("GET","/api/knowledge/instructions",&[],None).await.status,401);
    let (cookie,_)=pilot.login(ALICE_TOKEN).await;
    let result=pilot.request("GET","/api/knowledge/instructions",&[("cookie",&cookie)],None).await;
    assert_eq!(result.status,200,"{}",result.body);
    assert_eq!(result.body,json!({"entries":[],"versions":[]}));
}

#[tokio::test]
async fn instruction_catalog_http_requires_actor_and_returns_only_current_shared_rules() {
    let pilot=Pilot::start().await;
    pilot.app.change(|d|{
        let first=knowledge::save_instruction(d,&json!({"requestId":"http-first","title":"Rule","text":"OLD_INSTRUCTION_CANARY"}),"2026-09-23T12:00:00Z").map_err(internal)?;
        knowledge::save_instruction(d,&json!({"requestId":"http-second","title":"Rule","text":"Current human rule","entryId":first["entry"]["id"],"expectedVersionId":first["version"]["id"]}),"2026-09-23T12:00:00Z").map_err(internal)?;
        Ok(())
    }).await.unwrap();
    assert_eq!(pilot.request("GET","/api/knowledge/instructions",&[],None).await.status,401);
    let (cookie,csrf)=pilot.login(ALICE_TOKEN).await;
    let result=pilot.request("GET","/api/knowledge/instructions",&[("cookie",&cookie)],None).await;
    assert_eq!(result.status,200,"{}",result.body);
    assert_eq!(result.body["entries"].as_array().unwrap().len(),1);
    assert_eq!(result.body["versions"].as_array().unwrap().len(),1);
    assert_eq!(result.body["versions"][0]["text"],"Current human rule");
    for canary in ["OLD_INSTRUCTION_CANARY","PRIVATE_OWNER_CANARY","PRIVATE_BOB_CANARY","ALICE_JOB_CANARY"] {assert!(!result.body.to_string().contains(canary));}
    assert_eq!(result.body.as_object().unwrap().len(),2);
    let history=pilot.request("GET","/api/knowledge",&[("cookie",&cookie)],None).await;
    assert_eq!(history.status,200);
    assert_eq!(history.body["versions"].as_array().unwrap().len(),2);
    let headers=[("cookie",cookie.as_str()),("x-csrf-token",csrf.as_str()),("origin",ORIGIN)];
    assert_eq!(pilot.request("POST","/api/knowledge/instructions",&headers,Some(json!({"requestId":"denied","title":"Unauthorized","text":"No"}))).await.status,403);
}

#[tokio::test]
async fn feedback_http_exports_real_edit_lineage_with_verified_actor_and_owner_privacy() {
    let pilot=Pilot::start().await;
    pilot.app.change(|d|{
        d["items"]=json!([{"id":"feedback-item","revision":1,"draft":"","workflow":"prepared","platform":"vk"}]);
        d["proposals"]=json!([{"id":"ai-origin","itemId":"feedback-item","revision":1,"itemRevision":1,"kind":"reply_and_close","text":"Original AI text","prepareRunId":"generated-run","prepareBundleId":"bundle-1","prepareBundleDigest":"digest-1","knowledgePolicyVersion":1,"knowledgeManifest":[{"kind":"rule","entryId":"rule-1","versionId":"rule-v1","hash":"hash-1"}]}]);
        Ok(())
    }).await.unwrap();
    let (cookie,csrf)=pilot.login(ALICE_TOKEN).await;
    let headers=[("cookie",cookie.as_str()),("x-csrf-token",csrf.as_str()),("origin",ORIGIN)];
    let body=json!({"expectedRevision":1,"draft":"Edited by operator","eventId":"edit-auth-1","draftSessionId":"review-session","sessionId":"client-opaque","sourceProposalId":"ai-origin","sourceProposalRevision":1,"actor":{"id":"bob"},"_verifiedActor":{"id":"local-owner","role":"owner"}});
    let edited=pilot.request("PATCH","/api/items/feedback-item",&headers,Some(body.clone())).await;
    assert_eq!(edited.status,200,"{}",edited.body);
    assert_eq!(pilot.request("PATCH","/api/items/feedback-item",&headers,Some(body)).await.status,200);
    let local=format!("127.0.0.1:{}",pilot.port);
    let export=pilot.request("GET","/api/knowledge",&[("host",&local)],None).await;
    assert_eq!(export.status,200);
    let events=export.body["feedback"].as_array().unwrap();
    assert_eq!(events.len(),1);
    let e=&events[0];
    assert_eq!(e["before"],"Original AI text"); assert_eq!(e["after"],"Edited by operator");
    assert_eq!(e["origin"]["text"],"Original AI text");
    assert_eq!(e["origin"]["knowledgeManifest"][0]["versionId"],"rule-v1");
    assert_eq!(e["prepareBundleId"],"bundle-1"); assert_eq!(e["prepareBundleDigest"],"digest-1");
    assert_eq!(e["actor"]["id"],"alice"); assert_eq!(e["actorVerified"],true);
    assert!(!e.to_string().contains(ALICE_TOKEN)); assert!(!e.to_string().contains(&csrf));
    let remote=pilot.request("GET","/api/knowledge",&[("cookie",&cookie)],None).await;
    assert_eq!(remote.status,200); assert!(remote.body.get("feedback").is_none());
    assert!(remote.body.get("entries").is_some()); // Shared knowledge stays accessible.
    assert_eq!(pilot.request("GET","/api/feedback/report",&[("cookie",&cookie)],None).await.status,403);
    assert_eq!(pilot.request("GET","/api/feedback/report",&[("host",&local)],None).await.status,200);
    let (bob,_)=pilot.login(BOB_TOKEN).await;
    assert_eq!(pilot.request("GET","/api/feedback/report",&[("cookie",&bob)],None).await.status,403);
}

struct Pilot {
    app: App,
    port: u16,
    server: tokio::task::JoinHandle<()>,
    _temp: tempfile::TempDir,
}
impl Drop for Pilot {
    fn drop(&mut self) {
        self.server.abort();
    }
}
struct Reply {
    status: u16,
    headers: HashMap<String, String>,
    body: Value,
}

impl Pilot {
    async fn start() -> Self {
        Self::start_with_account(crate::accounts::Profile::LikeAvto).await
    }
    async fn start_with_account(account:crate::accounts::Profile) -> Self {
        Self::start_scoped(account, "/", "[]").await
    }
    async fn start_scoped(account:crate::accounts::Profile, base_path:&str, account_json:&str) -> Self {
        let temp = tempfile::tempdir().unwrap();
        let access_file = temp.path().join("access.json");
        let operators = [("alice",ALICE_TOKEN),("bob",BOB_TOKEN)].map(|(id,token)|json!({"id":id,"name":id,"tokenHash":format!("{:x}",Sha256::digest(token.as_bytes()))}));
        tokio::fs::write(&access_file, json!({"operators":operators}).to_string())
            .await
            .unwrap();
        let auth = operator_auth::Auth::open_scoped(temp.path(), access_file, base_path)
            .await
            .unwrap();
        let db = open_db(&temp.path().join("workspace.sqlite"))
            .await
            .unwrap();
        if account != crate::accounts::Profile::LikeAvto {
            // Set the isolated fixture's company before constructing its App;
            // production correctly forbids switching a populated workspace.
            sqlx::query("UPDATE workspace SET payload=json_set(payload,'$.account',?,'$.connectorBinding',json(?)) WHERE id=1")
                .bind(account.display()).bind(account.binding().to_string()).execute(&db).await.unwrap();
        }
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let (events, _) = broadcast::channel(8);
        let app = App {lifecycle_task_count: Default::default(), lifecycle_admission: Arc::new(crate::runtime_lifecycle_startup::Admission::fixture(account)), lifecycle_owner: Arc::new(crate::runtime_lifecycle_startup::Admission::fixture(account).identity().clone()), lifecycle_provider_token: Default::default(), lifecycle_work: Default::default(), media_discovery: Default::default(),preparation_wake: Default::default(),provider_session: Default::default(),
            account,navigation:crate::account_navigation::Navigation::parse(base_path,account_json,account.key(),Some(ORIGIN)).unwrap(),
            db: Database::Sqlite(db),
            gate: Arc::new(crate::writer_gate::WriterGate::default()),
            execution_gate: Arc::new(Mutex::new(())),
        preparation_workers: Default::default(),editorial_gate: Default::default(),assistant_gate: Arc::new(Mutex::new(())),assistant_chat_gate: Arc::new(Mutex::new(())),
            events,
            csrf: "local-owner-csrf".into(),
            auth: Some(auth),
            public_origin: Some(ORIGIN.into()),
            external_writes: false,
            port,
            data: temp.path().to_owned(),
            bridge: temp.path().join("never-execute.mjs"),
            node: temp.path().join("no-runtime"),
            tasks: Arc::new(Mutex::new(HashMap::new())),bootstrap_cache:Arc::new(bootstrap_cache::Cache::default()),
        };
        crate::runtime_lifecycle_app::initialize_app_fixture(&app).await.unwrap();
        app.change(|d| {
            d["conversations"] = json!([
                {"id":"owner-chat","messages":[{"text":"PRIVATE_OWNER_CANARY"}]},
                {"id":"alice-chat","operatorId":"alice","messages":[{"text":"PRIVATE_ALICE_CANARY"}]},
                {"id":"bob-chat","operatorId":"bob","messages":[{"text":"PRIVATE_BOB_CANARY"}]}
            ]);
            d["jobs"] = json!([
                {"id":"owner-job","kind":"assistant","refId":"owner-chat","status":"completed","result":{"text":"OWNER_JOB_CANARY"}},
                {"id":"alice-job","kind":"assistant","refId":"alice-chat","status":"completed","result":{"text":"ALICE_JOB_CANARY"}},
                {"id":"bob-job","kind":"assistant","refId":"bob-chat","status":"completed","result":{"text":"BOB_JOB_CANARY"}},
                {"id":"shared-job","kind":"sync","status":"completed"}
            ]);
            Ok(())
        }).await.unwrap();
        let router = routes(app.clone(), temp.path().join("empty-web"));
        let server = tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        Self {
            app,
            port,
            server,
            _temp: temp,
        }
    }
    async fn request(
        &self,
        method: &str,
        path: &str,
        headers: &[(&str, &str)],
        body: Option<Value>,
    ) -> Reply {
        let body = body.map(|b| b.to_string()).unwrap_or_default();
        let host = headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case("host"))
            .map(|(_, v)| *v)
            .unwrap_or(HOST);
        let mut raw = format!(
            "{method} {path} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\nContent-Type: application/json\r\nContent-Length: {}\r\n",
            body.len()
        );
        for (key, value) in headers
            .iter()
            .filter(|(key, _)| !key.eq_ignore_ascii_case("host"))
        {
            raw.push_str(&format!("{key}: {value}\r\n"));
        }
        raw.push_str("\r\n");
        raw.push_str(&body);
        let mut stream = tokio::net::TcpStream::connect((std::net::Ipv4Addr::LOCALHOST, self.port))
            .await
            .unwrap();
        stream.write_all(raw.as_bytes()).await.unwrap();
        let mut bytes = vec![];
        tokio::time::timeout(Duration::from_secs(5), stream.read_to_end(&mut bytes))
            .await
            .unwrap()
            .unwrap();
        let raw = String::from_utf8(bytes).unwrap();
        let (head, body) = raw.split_once("\r\n\r\n").unwrap();
        let mut lines = head.lines();
        let status = lines
            .next()
            .unwrap()
            .split_whitespace()
            .nth(1)
            .unwrap()
            .parse()
            .unwrap();
        let headers = lines
            .filter_map(|line| line.split_once(':'))
            .map(|(key, value)| (key.to_ascii_lowercase(), value.trim().to_string()))
            .collect();
        Reply {
            status,
            headers,
            body: serde_json::from_str(body).unwrap_or_else(|_| json!(body)),
        }
    }
    async fn login(&self, token: &str) -> (String, String) {
        let response = self
            .request(
                "POST",
                "/api/session/login",
                &[("origin", ORIGIN)],
                Some(json!({"token":token})),
            )
            .await;
        assert_eq!(response.status, 200, "{}", response.body);
        let cookie = response.headers["set-cookie"]
            .split(';')
            .next()
            .unwrap()
            .to_string();
        let csrf = response.body["csrfToken"].as_str().unwrap().to_owned();
        (cookie, csrf)
    }
}

#[tokio::test]
async fn remote_auth_rejects_anonymous_wrong_host_origin_and_forwarded_local_owner() {
    let pilot = Pilot::start().await;
    assert_eq!(
        pilot
            .request("GET", "/api/bootstrap", &[], None)
            .await
            .status,
        401
    );
    assert_eq!(
        pilot
            .request("GET", "/api/bootstrap", &[("host", "evil.example")], None)
            .await
            .status,
        403
    );
    for extra in [
        vec![],
        vec![("origin", "https://evil.example")],
        vec![("origin", ORIGIN), ("sec-fetch-site", "cross-site")],
    ] {
        assert_eq!(
            pilot
                .request(
                    "POST",
                    "/api/session/login",
                    &extra,
                    Some(json!({"token":ALICE_TOKEN}))
                )
                .await
                .status,
            403
        );
    }
    let local = format!("127.0.0.1:{}", pilot.port);
    for header in [
        "forwarded",
        "x-forwarded-for",
        "x-forwarded-host",
        "x-forwarded-proto",
        "cf-connecting-ip",
    ] {
        assert_eq!(
            pilot
                .request(
                    "GET",
                    "/api/bootstrap",
                    &[("host", &local), (header, "anything")],
                    None
                )
                .await
                .status,
            403,
            "{header}"
        );
    }
    let local = pilot
        .request("GET", "/api/bootstrap", &[("host", &local)], None)
        .await;
    assert_eq!(local.status, 200);
    assert_eq!(local.body["operator"]["id"], "local-owner");
    assert_eq!(local.body["conversations"].as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn editorial_jobs_bootstrap_is_private_to_creator_and_visible_to_owner() {
    let pilot=Pilot::start().await;
    pilot.app.change(|d| {
        for (key,operator,canary) in [("editorial-alice",Some("alice"),"EDITORIAL_ALICE_CANARY"),
            ("editorial-bob",Some("bob"),"EDITORIAL_BOB_CANARY"),
            ("editorial-owner",Some("local-owner"),"EDITORIAL_OWNER_CANARY"),
            ("editorial-unattributed",None,"EDITORIAL_UNATTRIBUTED_CANARY")] {
            let mut job=json!({"id":key,"kind":"editorial_review","purpose":"editorial_review","refId":format!("request-{key}"),
                "status":"completed","result":{"accepted":[],"reused":[],"held":[{"reason":canary}]}});
            if let Some(operator)=operator {job["operatorId"]=json!(operator);}
            list_mut(d,"jobs").push(job);
        }
        Ok(())
    }).await.unwrap();
    let (alice,_)=pilot.login(ALICE_TOKEN).await;let (bob,_)=pilot.login(BOB_TOKEN).await;
    for (cookie,who,own,foreign) in [(&alice,"alice","EDITORIAL_ALICE_CANARY","EDITORIAL_BOB_CANARY"),
        (&bob,"bob","EDITORIAL_BOB_CANARY","EDITORIAL_ALICE_CANARY")] {
        for path in ["/api/bootstrap","/api/bootstrap/delta?since=missing-generation"] {
            let response=pilot.request("GET",path,&[("cookie",cookie)],None).await;
            assert_eq!(response.status,200,"{}",response.body);
            let view=if path.contains("delta"){assert_eq!(response.body["kind"],"full");&response.body["snapshot"]}else{&response.body};
            assert_eq!(view["operator"]["id"],who);
            let editorial:Vec<_>=view["jobs"].as_array().unwrap().iter().filter(|j|j["kind"]=="editorial_review").collect();
            assert_eq!(editorial.len(),1);assert_eq!(editorial[0]["operatorId"],who);
            let text=view.to_string();assert!(text.contains(own));
            for hidden in [foreign,"EDITORIAL_OWNER_CANARY","EDITORIAL_UNATTRIBUTED_CANARY"] {assert!(!text.contains(hidden),"{who} received {hidden}");}
        }
    }
    let host=format!("127.0.0.1:{}",pilot.port);
    let owner=pilot.request("GET","/api/bootstrap",&[("host",&host)],None).await;
    assert_eq!(owner.status,200);
    assert_eq!(owner.body["jobs"].as_array().unwrap().iter().filter(|j|j["kind"]=="editorial_review").count(),4);
    for canary in ["EDITORIAL_ALICE_CANARY","EDITORIAL_BOB_CANARY","EDITORIAL_OWNER_CANARY","EDITORIAL_UNATTRIBUTED_CANARY"] {
        assert!(owner.body.to_string().contains(canary));
    }
}

#[tokio::test]
async fn editorial_jobs_delta_filters_both_actor_neutral_base_and_current_generation() {
    let pilot=Pilot::start().await;
    pilot.app.change(|d| {
        for operator in ["alice","bob"] {list_mut(d,"jobs").push(json!({"id":format!("editorial-{operator}"),"kind":"editorial_review",
            "purpose":"editorial_review","operatorId":operator,"refId":format!("request-{operator}"),"status":"running"}));}
        Ok(())
    }).await.unwrap();
    let (alice,_)=pilot.login(ALICE_TOKEN).await;let (bob,_)=pilot.login(BOB_TOKEN).await;
    // A version is actor-neutral. Borrowing Bob's version must not reveal his
    // deleted IDs or changed text through Alice's diff of the retained base.
    let base=pilot.request("GET","/api/bootstrap",&[("cookie",&bob)],None).await;
    assert_eq!(base.status,200);let version=base.body["workspaceVersion"].as_str().unwrap().to_owned();
    pilot.app.change(|d| {
        list_mut(d,"jobs").retain(|j|j["id"]!="editorial-bob");
        row_mut(d,"jobs","editorial-alice")?["result"]=json!({"accepted":[],"reused":[],"held":[{"reason":"ALICE_EDITORIAL_UPDATED_CANARY"}]});
        for operator in ["alice","bob"] {list_mut(d,"jobs").push(json!({"id":format!("editorial-new-{operator}"),"kind":"editorial_review",
            "purpose":"editorial_review","operatorId":operator,"refId":format!("new-request-{operator}"),"status":"completed",
            "result":{"accepted":[],"reused":[],"held":[{"reason":format!("{operator}_EDITORIAL_NEW_CANARY")}]}}));}
        Ok(())
    }).await.unwrap();
    let path=format!("/api/bootstrap/delta?since={version}");
    for (cookie,who,expected,hidden) in [(&alice,"alice",vec!["editorial-alice","editorial-new-alice"],"bob"),
        (&bob,"bob",vec!["editorial-new-bob"],"alice")] {
        let response=pilot.request("GET",&path,&[("cookie",cookie)],None).await;
        assert_eq!(response.status,200,"{}",response.body);assert_eq!(response.body["kind"],"delta","{}",response.body);
        assert_eq!(response.body["actorId"],who);
        let patch=&response.body["collections"]["jobs"];
        let upsert:Vec<_>=patch["upsert"].as_array().unwrap().iter().map(|j|j["id"].as_str().unwrap()).collect();
        assert_eq!(upsert,expected);
        assert_eq!(patch["remove"],if who=="bob"{json!(["editorial-bob"])}else{json!([])});
        let text=response.body.to_string();assert!(!text.contains(&format!("editorial-{hidden}")));
        assert!(!text.contains(&format!("editorial-new-{hidden}")));assert!(!text.contains(&format!("{hidden}_EDITORIAL_NEW_CANARY")));
        if who=="bob"{assert!(!text.contains("ALICE_EDITORIAL_UPDATED_CANARY"));}
    }
}

#[tokio::test]
async fn remote_sessions_isolate_bootstrap_jobs_csrf_and_conversation_write_ownership() {
    let pilot = Pilot::start().await;
    let (alice, alice_csrf) = pilot.login(ALICE_TOKEN).await;
    let (bob, bob_csrf) = pilot.login(BOB_TOKEN).await;
    assert_ne!(alice_csrf, bob_csrf);
    for (cookie, who, private, other) in [
        (
            &alice,
            "alice",
            "PRIVATE_ALICE_CANARY",
            "PRIVATE_BOB_CANARY",
        ),
        (&bob, "bob", "PRIVATE_BOB_CANARY", "PRIVATE_ALICE_CANARY"),
    ] {
        let response = pilot
            .request("GET", "/api/bootstrap", &[("cookie", cookie)], None)
            .await;
        assert_eq!(response.status, 200);
        assert_eq!(response.body["operator"]["id"], who);
        let text = response.body.to_string();
        assert!(text.contains(private));
        assert!(!text.contains(other));
        assert!(!text.contains("PRIVATE_OWNER_CANARY"));
        assert_eq!(response.body["conversations"].as_array().unwrap().len(), 1);
        assert_eq!(response.body["jobs"].as_array().unwrap().len(), 2);
        assert!(!text.contains("OWNER_JOB_CANARY"));
        assert!(!text.contains(if who == "alice" {
            "BOB_JOB_CANARY"
        } else {
            "ALICE_JOB_CANARY"
        }));
    }
    let before = pilot.app.read().await.unwrap();
    let forbidden = pilot
        .request(
            "POST",
            "/api/conversations/bob-chat/messages",
            &[
                ("cookie", &alice),
                ("origin", ORIGIN),
                ("x-csrf-token", &alice_csrf),
            ],
            Some(json!({"text":"IDOR attempt"})),
        )
        .await;
    assert_eq!(forbidden.status, 404);
    assert_eq!(pilot.app.read().await.unwrap(), before);
    let cross_csrf = pilot
        .request(
            "POST",
            "/api/conversations",
            &[
                ("cookie", &alice),
                ("origin", ORIGIN),
                ("x-csrf-token", &bob_csrf),
            ],
            Some(json!({"title":"must not exist"})),
        )
        .await;
    assert_eq!(cross_csrf.status, 403);
    let created = pilot
        .request(
            "POST",
            "/api/conversations",
            &[
                ("cookie", &alice),
                ("origin", ORIGIN),
                ("x-csrf-token", &alice_csrf),
            ],
            Some(json!({"title":"new","operatorId":"bob"})),
        )
        .await;
    assert_eq!(created.status, 200);
    assert_eq!(created.body["operatorId"], "alice");
    let bob_view = pilot
        .request("GET", "/api/bootstrap", &[("cookie", &bob)], None)
        .await;
    assert_eq!(bob_view.body["conversations"].as_array().unwrap().len(), 1);
    assert_eq!(bob_view.body["conversations"][0]["id"], "bob-chat");
}

#[tokio::test]
async fn remote_operator_cannot_run_maintenance_and_logout_revokes_cookie() {
    let pilot = Pilot::start().await;
    let (cookie, csrf) = pilot.login(ALICE_TOKEN).await;
    let headers = [
        ("cookie", cookie.as_str()),
        ("origin", ORIGIN),
        ("x-csrf-token", csrf.as_str()),
    ];
    for path in [
        "/api/sync",
        "/api/archive/import",
        "/api/backup",
        "/api/maintenance/recover-prepared",
        "/api/maintenance/knowledge-normalization",
        "/api/maintenance/knowledge-rule-revision",
        "/api/engine/prepare/plan",
        "/api/engine/prepare/families",
        "/api/engine/prepare/facts/resolve",
        "/api/materials/process",
        "/api/jobs/owner-job/cancel",
    ] {
        assert_eq!(
            pilot
                .request("POST", path, &headers, Some(json!({})))
                .await
                .status,
            403,
            "{path}"
        );
    }
    let logout = pilot
        .request("POST", "/api/session/logout", &headers, Some(json!({})))
        .await;
    assert_eq!(logout.status, 200);
    assert!(logout.headers["set-cookie"].contains("Max-Age=0"));
    assert_eq!(
        pilot
            .request("GET", "/api/bootstrap", &[("cookie", &cookie)], None)
            .await
            .status,
        401
    );
}

// R9 R3 isolated native HTTP lost-response fixture; parent module is cfg(test).
#[path = "runtime_lifecycle_http_lost_reply_tests.rs"]
mod lost_reply_tests;
