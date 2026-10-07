//! Development-only matched source-writer workload. Root owns execution and pins.
//! Three selectors each require their own TWO pristine, explicitly named BAW DBs.
//! No worker/transport/model is started. Timings are warm, serial, debug/release
//! as built by root; they are never a replay of the historical R9 preparation run.
use super::*;
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, io::Read, sync::Arc, time::Instant};
use tokio::sync::{broadcast, Mutex};

const VERSION: &str = "source-writer-matched-v2";
const PAIRS: usize = 30;
const AT: &str = "2026-10-06T00:00:00Z";
const TARGET_POST: &str = "source-bench-post";
const TARGET_ITEM: &str = "source-bench-item";
const CAPTURE_ITEM: &str = "source-bench-current-item";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Cohort { Legacy, Ambiguous, MandatoryRepair }
impl Cohort {
    fn name(self) -> &'static str { match self {
        Self::Legacy => "A_legacy_unreferenced", Self::Ambiguous => "B_ambiguous_fallback",
        Self::MandatoryRepair => "C_current_unpaid_captures_plus_paid_repair",
    } }
}

fn sha(bytes: &[u8]) -> String { format!("{:x}", Sha256::digest(bytes)) }
fn digest(value: &Value) -> String { sha(value.to_string().as_bytes()) }
fn env(name: &str) -> String { std::env::var(name).unwrap_or_else(|_| panic!("explicit {name} required")) }
fn pin(name: &str) -> String {
    let value = env(name);
    assert!(value.len() == 64 && value.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)), "invalid {name}");
    value
}
fn bounded(name: &str, default: usize, min: usize, max: usize) -> usize {
    let value = std::env::var(name).map(|v| v.parse::<usize>().expect("integer benchmark bound")).unwrap_or(default);
    assert!((min..=max).contains(&value), "{name} outside finite benchmark bounds"); value
}
fn executable_sha() -> String {
    let mut file = std::fs::File::open(std::env::current_exe().unwrap()).unwrap();
    let mut hash = Sha256::new(); let mut buffer = [0u8; 65536];
    loop { let count = file.read(&mut buffer).unwrap(); if count == 0 { break; } hash.update(&buffer[..count]); }
    format!("{hash:x}", hash = hash.finalize())
}
fn emit(kind: &str, value: &Value) { println!("source_writer_benchmark_{kind} {value}"); }

// A real App whose account, storage, lifecycle and absent executables are BAW
// from birth; do not construct/relabel a populated LikeAvto test_app.
async fn app(db: Database) -> (crate::App, tempfile::TempDir) {
    let folder = tempfile::tempdir().unwrap();
    let admission = Arc::new(crate::runtime_lifecycle_startup::Admission::fixture(crate::accounts::Profile::BawRussia));
    let (events, _) = broadcast::channel(32);
    let app = crate::App {
        lifecycle_task_count: Default::default(), lifecycle_owner: Arc::new(admission.identity().clone()),
        lifecycle_admission: admission, lifecycle_provider_token: Default::default(), lifecycle_work: Default::default(),
        media_discovery: Default::default(), preparation_wake: Default::default(), provider_session: Default::default(),
        account: crate::accounts::Profile::BawRussia, navigation: crate::account_navigation::Navigation::root(), db,
        gate: Arc::new(crate::writer_gate::WriterGate::default()), execution_gate: Arc::new(Mutex::new(())),
        preparation_workers: Default::default(), editorial_gate: Default::default(), assistant_gate: Arc::new(Mutex::new(())),
        assistant_chat_gate: Arc::new(Mutex::new(())), events, csrf: "source-benchmark-fixture".into(),
        auth: None, public_origin: None, external_writes: false, port: 0, data: folder.path().to_owned(),
        bridge: folder.path().join("never-execute.mjs"), node: folder.path().join("no-runtime"),
        tasks: Arc::new(Mutex::new(HashMap::new())), bootstrap_cache: Arc::new(crate::bootstrap_cache::Cache::default()),
    };
    crate::runtime_lifecycle_app::initialize_app_fixture(&app).await.unwrap(); (app, folder)
}

// The connected completion test reuses this real native admission history,
// including its original UNKNOWN operation, instead of inventing a send row.
pub(super) async fn completion_fixture_app(db:Database)->(crate::App,tempfile::TempDir,Value) {
    let factory=fixture(Cohort::Legacy,2,1024).await;
    let (app,folder)=app(db).await;
    let lifecycle=app.db.read().await.unwrap()["runtimeLifecycle"].clone();
    for step in factory.seed_steps {
        app.db.change(|d|{*d=step.state.clone();d["runtimeLifecycle"]=lifecycle.clone();Ok(())}).await.unwrap();
    }
    app.change_source_snapshot_scoped(SourceReadIntent::Snapshot(&json!({})),|d|
        crate::merge_snapshot(d,&json!({"posts":[],"branches":[],"items":[]}))).await.unwrap();
    let state=app.db.read().await.unwrap();
    let operation=crate::list(&state,"operations").iter().find(|op|op["status"]=="unknown").unwrap().clone();
    (app,folder,operation)
}

fn observed_source(d: &Value, source: &Value, post: &Value, branch: &Value) -> Value {
    let binding = crate::active_binding(d).unwrap();
    let mut source = crate::bound_item(&binding, source).unwrap();
    // The source reducer owns the assembled branch digest, not the connector's
    // observed-fragment digest. Hash the exact local synthetic observation before
    // any bundle/proposal/approval captures it; retain its preimage for checking.
    let evidence = json!({"contract":"synthetic_source_benchmark_context_v1","account":d["account"],
        "connectorBinding":binding.to_json(),"post":post,"branch":branch,"item":source});
    source["contextEvidenceDigest"] = json!(digest(&evidence));
    source["fixtureContextEvidence"] = evidence;
    source
}

fn add_source(d: &mut Value, suffix: &str) {
    let post = format!("source-bench-{suffix}post"); let branch = format!("source-bench-{suffix}branch");
    let item = format!("source-bench-{suffix}item");
    let source = json!({"id":item,"itemId":item,"objectId":"source-bench-object",
        "platform":"vk","postId":post,"postKey":post,"branchId":branch,"conversationKey":branch,
        "providerStatus":"new","text":"Synthetic local source question","authorId":format!("source-bench-{suffix}author"),
        "contextObservedAt":AT,"providerStatusObservedAt":AT});
    let observed_post = json!({"id":post,"postKey":post,"objectId":"source-bench-object",
        "text":"Synthetic source baseline","attachments":[]});
    let observed_branch = json!({"id":branch,"postId":post,"contextComplete":true,
        "messages":[{"id":item,"text":"Synthetic local source question","role":"customer"}]});
    let source = observed_source(d, &source, &observed_post, &observed_branch);
    crate::merge_snapshot(d, &json!({"posts":[observed_post],"branches":[observed_branch],"items":[source]})).unwrap();
    // Fixed source observation before any preparation/approval/paid capture.
    crate::row_mut(d, "branches", &branch).unwrap()["observedAt"] = json!(AT);
}

struct SeedStep { label: String, state: Value }
struct Fixture { state: Value, mandatory_jobs: Vec<String>, draft: String, seed_steps: Vec<SeedStep> }

async fn fixture_step<T>(db: &Database, steps: &mut Vec<SeedStep>, label: &str,
    change: impl FnOnce(&mut Value) -> ApiResult<T>) -> T {
    let result = db.change(change).await.unwrap_or_else(|error| panic!("native fixture {label}: {error:?}"));
    steps.push(SeedStep { label: label.into(), state: db.read().await.unwrap() });
    result
}

fn add_benchmark_sources(d: &mut Value, cohort: Cohort) {
    add_source(d, ""); add_source(d, "protected-");
    if cohort == Cohort::MandatoryRepair { add_source(d, "current-"); }
    if cohort != Cohort::MandatoryRepair {
        crate::list_mut(d, "materials").push(json!({"id":"source-bench-reference","account":"BAW Russia",
            "kind":"reference","revision":1,"title":"Synthetic retained reference","text":"Unverified fixture reference; no publication authority."}));
        crate::knowledge::sync_catalog(d, AT).unwrap();
    }
}

fn add_wide_sources(d:&mut Value,count:usize,text_bytes:usize) {
    if count==0{return;}
    let binding=crate::active_binding(d).unwrap();let text="w".repeat(text_bytes);
    let mut posts=Vec::with_capacity(count);let mut branches=Vec::with_capacity(count);let mut items=Vec::with_capacity(count);
    for n in 0..count {
        let post=format!("source-wide-post-{n}");let branch=format!("source-wide-branch-{n}");let item=format!("source-wide-item-{n}");
        posts.push(json!({"id":post,"text":text,"objectId":"source-bench-object","postKey":post,"attachments":[]}));
        branches.push(json!({"id":branch,"postId":post,"contextComplete":true,
            "messages":[{"id":item,"role":"customer","text":text}]}));
        items.push(crate::bound_item(&binding,&json!({"id":item,"itemId":item,"objectId":"source-bench-object",
            "postId":post,"postKey":post,"branchId":branch,"conversationKey":branch,"text":"Synthetic broad source observation",
            "providerStatus":"new","contextObservedAt":AT,"providerStatusObservedAt":AT})).unwrap());
    }
    crate::merge_snapshot(d,&json!({"posts":posts,"branches":branches,"items":items})).unwrap();
    for branch in crate::list_mut(d,"branches").iter_mut().filter(|row|row["id"].as_str().is_some_and(|id|id.starts_with("source-wide-"))) {
        branch["observedAt"]=json!(AT);
    }
}

fn append_source_history(d: &mut Value, cohort: Cohort, history: usize, body_bytes: usize) {
    for n in 0..history {
        let messages = json!([{"role":"user","text":"h".repeat(body_bytes)}]);
        if cohort == Cohort::MandatoryRepair {
            // Each row has an independently constructed CURRENT native request
            // on a source that this workload never mutates. These are unpaid
            // journals cancelled before dispatch, NOT invented paid outcomes.
            let mut bundle=crate::prepare_bundle::build(&d,&[json!(CAPTURE_ITEM)],messages.as_array().unwrap()).unwrap();
            crate::preparation_unit::attach(&d,&mut bundle,AT).unwrap();
            crate::decision_media::attach_request(&d,&mut bundle["request"]).unwrap();
            crate::preparation_materials::attach_request(&d,&mut bundle["request"]).unwrap();
            bundle["digest"]=json!(crate::preparation_materials::hash(&bundle["request"]));
            crate::prepare_bundle::current(&d,&bundle).unwrap();
            crate::preparation_materials::require_request(&d,&bundle["request"]).unwrap();
            let id=crate::new_job(d,"assistant",CAPTURE_ITEM).unwrap();
            let binding=d["connectorBinding"].clone();
            let job=crate::row_mut(d,"jobs",&id).unwrap();
            job["purpose"]=json!("discussion");job["account"]=json!("BAW Russia");job["connectorBinding"]=binding;
            job["mandatoryMaterialContract"]=bundle["request"]["mandatoryMaterialContract"].clone();
            job["postContextBundle"]=bundle["request"]["postContextBundle"].clone();
            job["materialReadiness"]=bundle["request"]["materialReadiness"].clone();
            job["prepareBundle"]=bundle;job["status"]=json!("cancelled");
            job["fixtureOrigin"]=json!("native-current-capture-cancelled-before-dispatch");
            job["retryAuthorized"]=json!(false);
            assert!(source_jobs::requires_full_material_job(job)&&source_jobs::source_control(job).1);
            assert!(job.get("retainedEvidence").is_none()&&job.get("result").is_none()&&job.get("preparationStages").is_none());
            continue;
        }
        // Native valid legacy request and dependency digests; deliberately no
        // paidResultRef, material receipt or fabricated successful paid journal.
        let mut bundle = crate::prepare_bundle::build(&d, &[json!(TARGET_ITEM)], messages.as_array().unwrap()).unwrap();
        bundle["id"] = json!(format!("source-bench-history-bundle-{n:05}"));
        crate::prepare_bundle::current(&d, &bundle).unwrap();
        let job = json!({"id":format!("source-bench-history-{n:05}"),"kind":"assistant","purpose":"discussion",
            "account":"BAW Russia","connectorBinding":d["connectorBinding"],"status":"completed","createdAt":AT,
            "request":bundle["request"],"prepareBundle":bundle,
            "preparationStages":{"first":{"status":"completed","result":{"text":"Synthetic legacy terminal body","sources":[],"assessments":[],"proposals":[]}}},
            "fixtureOrigin":"synthetic-legacy-no-paid-authority","retryAuthorized":false});
        assert!(!source_jobs::source_control(&job).1, "history must really be compactable");
        crate::list_mut(d, "jobs").push(job);
    }
}

async fn fixture(cohort: Cohort, history: usize, body_bytes: usize) -> Fixture {
    fixture_with_sources(cohort,history,body_bytes,0,0).await
}
async fn fixture_with_sources(cohort:Cohort,history:usize,body_bytes:usize,source_count:usize,source_text_bytes:usize)->Fixture {
    let folder = tempfile::tempdir().unwrap();
    let db = Database::Sqlite(crate::open_db(&folder.path().join("native-source-fixture.sqlite")).await.unwrap());
    let mut seed_steps = Vec::new();
    fixture_step(&db, &mut seed_steps, "select_pristine_baw", |d| {
        crate::accounts::initialize(d, crate::accounts::Profile::BawRussia)?; normalize(d); Ok(())
    }).await;
    let mandatory_jobs = if cohort == Cohort::MandatoryRepair {
        let (mut base, now) = crate::answering_repair_plan::tests::revalidation_base();
        let post = base["posts"][0].clone(); let branch = base["branches"][0].clone();
        let source = observed_source(&base, &base["items"][0], &post, &branch);
        // Normalize the native source BEFORE paid capture. Subsequent source
        // merges must not introduce branch evidence that stales the paid draft.
        crate::merge_snapshot(&mut base, &json!({"posts":[post],"branches":[branch],"items":[source]})).unwrap();
        let (origin, plan) = crate::answering_repair_plan::tests::fresh_revalidation_database(
            &db, base, now, false, &mut |label: &str, state| {
                seed_steps.push(SeedStep { label: label.into(), state });
            }).await;
        let child = crate::answering_repair_plan::tests::settle_revalidation_child_database(
            &db, &origin, &plan, true, false, &mut |label: &str, state| {
                seed_steps.push(SeedStep { label: label.into(), state });
            }).await;
        fixture_step(&db, &mut seed_steps, "merge_native_repair", |d| {
            crate::answering_repair_plan::tests::merge_revalidation(d, &origin);
            crate::row_mut(d, "jobs", &origin)?["status"] = json!("completed"); Ok(())
        }).await;
        let d = db.read().await.unwrap();
        assert!(crate::row(&d, "jobs", &child).unwrap()["modelMaterialReceipts"].as_array().is_some_and(|a| !a.is_empty()));
        crate::list(&d, "jobs").iter().map(|j| j["id"].as_str().unwrap().to_owned()).collect()
    } else { Vec::new() };
    fixture_step(&db, &mut seed_steps, "add_benchmark_sources", |d| {
        add_benchmark_sources(d, cohort);add_wide_sources(d,source_count,source_text_bytes);Ok(())
    }).await;
    fixture_step(&db, &mut seed_steps, "open_protected_operation_fixture_gate", |d| {
        // MandatoryRepair already owns its original native paid lifecycle.
        // Bootstrap only absent owners; never replace that retained identity.
        if d.get("runtimeLifecycle").is_none() {
            crate::native_fixture_owner_repair::initialize_workspace(d)?;
        }
        crate::connection_gate::fixture_open(d)
    }).await;
    // Full native local admission, followed by a local UNKNOWN observation.
    // The returned dispatch work is never spawned or sent to a connector.
    let actor = crate::operator_auth::Actor::local_owner("source-benchmark");
    let held = "source-bench-protected-item";
    let proposal = fixture_step(&db, &mut seed_steps, "create_protected_proposal", |d| {
        let revision = crate::row(d, "items", held)?["revision"].clone();
        crate::create_proposal(d, &json!({"itemId":held,"kind":"close","expectedRevision":revision}))
    }).await;
    let approval = fixture_step(&db, &mut seed_steps, "approve_protected_proposal", |d| {
        crate::create_approval(d, &actor, &json!({"proposals":[{"id":proposal["id"],"revision":proposal["revision"]}]}))
    }).await;
    let approval_id = approval["id"].as_str().unwrap();
    let (_, scheduled) = fixture_step(&db, &mut seed_steps, "admit_protected_operation", |d| {
        crate::execute_admission::admit(d, &actor, approval_id, &json!({"approvalId":approval_id}))
    }).await;
    let (execution, operations) = scheduled.unwrap(); assert_eq!(operations.len(), 1);
    assert_eq!(operations[0]["action"]["contextEvidenceDigest"], proposal["contextEvidenceDigest"]);
    assert_eq!(approval["proposals"][0]["item"]["contextEvidenceDigest"], proposal["contextEvidenceDigest"]);
    fixture_step(&db, &mut seed_steps, "observe_local_unknown", |d| {
        crate::apply_operation_outcome(d, &operations[0], "unknown", json!({"reason":"Synthetic uncertain observation; provider never called"}))?;
        crate::row_mut(d, "jobs", &execution)?["status"] = json!("completed"); Ok(())
    }).await;
    let draft = fixture_step(&db, &mut seed_steps, "save_operator_draft", |d| {
        let revision = crate::row(d, "items", TARGET_ITEM)?["revision"].clone();
        let draft = crate::create_proposal(d, &json!({"itemId":TARGET_ITEM,"kind":"close","expectedRevision":revision}))?;
        let draft = draft["id"].as_str().unwrap().to_owned();
        crate::row_mut(d, "items", TARGET_ITEM)?["draft"] = json!("Preserve operator draft byte for byte");
        crate::row_mut(d, "items", TARGET_ITEM)?["draftEdited"] = json!(true);
        if cohort == Cohort::Ambiguous {
            // A stored legacy lineage of ambiguous type, not an explicit Full
            // intent or a fabricated fallback measurement. Both loaders see it.
            crate::row_mut(d, "proposals", &draft)?["legacySourceLineage"] = json!({"jobId":{"ambiguousLegacyReference":true}});
        }
        Ok(draft)
    }).await;
    // Keep large synthetic history in the final checkpoint only. Every capture
    // is built against the final source state, after all operator edits.
    fixture_step(&db, &mut seed_steps, "append_unpaid_source_history", |d| {
        append_source_history(d, cohort, history, body_bytes); Ok(())
    }).await;
    let d = db.read().await.unwrap();
    validate(&d).unwrap(); crate::knowledge::validate_catalog(&d).unwrap();
    crate::db_guards::validate_change(&d, &d).unwrap();
    for item in crate::list(&d, "items").iter().filter(|i| i.get("fixtureContextEvidence").is_some()) {
        assert_eq!(item["contextEvidenceDigest"], digest(&item["fixtureContextEvidence"]), "original source observation digest");
    }
    for job in crate::list(&d, "jobs").iter().filter(|j|j["fixtureOrigin"]=="native-current-capture-cancelled-before-dispatch") {
        crate::prepare_bundle::current(&d, &job["prepareBundle"]).unwrap();
        crate::preparation_unit::current_bundle(&d, &job["prepareBundle"], AT).unwrap();
        crate::preparation_materials::require_request(&d, &job["prepareBundle"]["request"]).unwrap();
    }
    if cohort == Cohort::MandatoryRepair {
        let repaired = crate::list(&d, "proposals").iter().find(|p|p["status"]=="draft"&&p["kind"]=="reply_and_close")
            .expect("native repair must retain a current reply draft");
        crate::proposal_current(&d, repaired).expect("adding benchmark sources must not stale the original paid repair");
        let mut replay = d.clone();
        crate::merge_snapshot(&mut replay, &json!({"posts":[],"branches":[],"items":[]})).unwrap();
        assert_eq!(crate::row(&replay,"proposals",repaired["id"].as_str().unwrap()).unwrap(),repaired,
            "source maintenance must not revise or stale the original paid repair");
    }
    let scoped = project_scoped(&d).unwrap();
    if matches!(cohort,Cohort::Ambiguous|Cohort::MandatoryRepair) {
        assert_eq!(scoped["jobs"], d["jobs"], "actual closure must retain every ambiguous/current mandatory body");
    }
    else { assert!(crate::list(&scoped, "jobs").len() < crate::list(&d, "jobs").len()); }
    for id in &mandatory_jobs { assert_eq!(crate::row(&scoped, "jobs", id).unwrap(), crate::row(&d, "jobs", id).unwrap()); }
    db.close().await;
    Fixture { state: d, mandatory_jobs, draft, seed_steps }
}

fn immutable(state: &Value) -> Value {
    let mut output = serde_json::Map::new();
    for key in ["account", "connectorBinding", "runtimeLifecycle", "jobs", "operations", "approvals", "materials",
        "knowledge_entries", "knowledge_versions", "settings", "feedback", "audit", "preparationResearch"] {
        output.insert(key.to_owned(), state.get(key).cloned().unwrap_or(Value::Null));
    }
    Value::Object(output)
}
fn sizing(state: &Value) -> Value {
    let collections: BTreeMap<_, _> = TABLES.iter().map(|table| (*table,
        json!({"rows":crate::list(state, table).len(),"serializedArrayBytes":state[*table].to_string().len()}))).collect();
    json!({"serializedWorkspaceBytes":state.to_string().len(),"collections":collections,
        "metadataBytes":metadata(state).to_string().len(),"canonicalSha256":digest(state),
        "preparationResearchBytes":state.get("preparationResearch").map(|v|v.to_string().len()),
        "basis":"UTF-8 serde_json logical bytes; not PG text/protocol, heap, TOAST, index, or disk bytes"})
}
fn distribution(state: &Value) -> Value {
    let mut kinds=BTreeMap::<String,usize>::new();let mut statuses=BTreeMap::<String,usize>::new();
    for job in crate::list(state,"jobs") {
        *kinds.entry(job["kind"].as_str().unwrap_or("missing").to_owned()).or_default()+=1;
        *statuses.entry(job["status"].as_str().unwrap_or("missing").to_owned()).or_default()+=1;
    }
    json!({"jobsByKind":kinds,"jobsByStatus":statuses,
        "jobsWithPaidReferences":crate::list(state,"jobs").iter().filter(|j|j["retainedEvidence"].as_array().is_some_and(|r|!r.is_empty())).count(),
        "currentUnpaidCaptures":crate::list(state,"jobs").iter().filter(|j|j["fixtureOrigin"]=="native-current-capture-cancelled-before-dispatch").count(),
        "jobsRequiringMaterialOrRepairBody":crate::list(state,"jobs").iter().filter(|j|source_jobs::requires_full_material_job(j)).count(),
        "unknownOperations":crate::list(state,"operations").iter().filter(|op|op["status"]=="unknown").count(),
        "proposalsByStatus":crate::list(state,"proposals").iter().fold(BTreeMap::<String,usize>::new(),|mut counts,p|{
            *counts.entry(p["status"].as_str().unwrap_or("missing").to_owned()).or_default()+=1;counts}),
        "activeRuleVersions":crate::list(state,"knowledge_versions").iter().filter(|v|v["status"]=="active"&&(v["kind"]=="rule"||v["kind"]=="policy")).count()})
}
async fn stored_sql_sizing(db: &Database) -> Value {
    let Database::Postgres { writer, .. } = db else { unreachable!() };
    let mut output = serde_json::Map::new();
    for table in TABLES {
        let sql = format!("SELECT count(*) AS rows,COALESCE(sum(octet_length(payload::text)),0)::bigint AS bytes FROM communityhero.{table} WHERE workspace_id=$1");
        let row = sqlx::query(sqlx::AssertSqlSafe(sql.as_str())).bind(WORKSPACE).fetch_one(writer).await.unwrap();
        output.insert((*table).to_owned(), json!({"rows":row.get::<i64,_>("rows"),"storedPayloadJsonTextBytes":row.get::<i64,_>("bytes")}));
    }
    let metadata_bytes: i32=sqlx::query_scalar("SELECT octet_length(metadata::text) FROM communityhero.workspaces WHERE id=$1")
        .bind(WORKSPACE).fetch_one(writer).await.unwrap();
    json!({"collections":output,"storedMetadataJsonTextBytes":metadata_bytes,"basis":"untimed SQL census of canonical payload::text, not measured writer transfer"})
}

fn duration(events: &[Value], stage: &str) -> Option<f64> {
    let ends: Vec<_> = events.iter().filter(|e| e["eventType"] == "span_end" && e["stage"] == stage).collect();
    if ends.len() != 1 { return None; }
    ends[0]["elapsedNs"].as_str()?.parse::<u128>().ok().map(|n| n as f64 / 1_000_000.0)
}
fn metrics(events: &[Value], elapsed_ms: f64) -> Value {
    let mut counters = serde_json::Map::new();
    for category in ["payloadRead", "controlRead", "headerRead", "discriminatorRead", "payloadWrite"] {
        let mut sums = serde_json::Map::new();
        for field in ["rows", "bytes", "statements"] {
            let observed: Vec<_> = events.iter().filter(|e| e["eventType"] == "span_end")
                .filter_map(|e| e["measurements"][category][field].as_u64()).collect();
            sums.insert(field.into(), if observed.is_empty() { Value::Null } else { json!(observed.iter().sum::<u64>()) });
        }
        counters.insert(category.into(), Value::Object(sums));
    }
    json!({"appCallMs":elapsed_ms,"writerWaitMs":duration(events,"source.snapshot.writer.wait"),
        "writerHeldMs":duration(events,"source.snapshot.writer.held"),"transactionMs":duration(events,"source.snapshot.transaction"),
        "loadMs":duration(events,"source.snapshot.load"),"cloneMs":duration(events,"source.snapshot.clone"),
        "domainMs":duration(events,"source.snapshot.domain"),"validationMs":duration(events,"source.snapshot.validation"),
        "persistEntitiesMs":duration(events,"source.snapshot.persist.entities"),"commitMs":duration(events,"source.snapshot.commit"),
        "poolReturnMs":duration(events,"pg.writer.return"),"cleanupMs":duration(events,"source.snapshot.drop"),"observedCounterSums":counters,
        "counterScope":"only numeric counters actually emitted by causal span_end; coverage may be partial; absence is null",
        "fallbackReasons":events.iter().filter(|e|e["eventType"]=="span_end")
            .filter_map(|e|e["measurements"]["fallbackReason"].as_str()).collect::<Vec<_>>()})
}

struct Sample { outcome: ApiResult<()>, metrics: Value }
async fn sample(app: &crate::App, snapshot: &Value, scoped: bool, cohort: Cohort, phase: &str,
    pair: usize, first: bool, source_pin: &str, binary_pin: &str) -> Sample {
    let context = crate::trace_context::TraceContext::root("baw-russia", &app.lifecycle_owner.runtime_id, 1, source_pin).unwrap()
        .with_lineage("binarySha256", binary_pin).unwrap()
        .with_operation(&format!("source-bench-{}-{phase}-{pair}-{}",cohort.name(),if scoped{"scoped"}else{"full"}),None).unwrap();
    let ((outcome, elapsed_ms), events) = crate::trace_context::capture(crate::trace_context::scope(context, async {
        let context = crate::trace_context::current().unwrap();
        let mut parent = crate::performance::Span::start("fixture.parent", crate::performance::SpanClass::Container, &context);
        let start = Instant::now();
        let outcome = parent.scope(async {
            if scoped { app.change_source_snapshot_scoped(SourceReadIntent::Snapshot(snapshot), |d| crate::merge_snapshot(d, snapshot)).await }
            else { app.change_source_snapshot(|d| crate::merge_snapshot(d, snapshot)).await }
        }).await;
        let elapsed_ms = start.elapsed().as_secs_f64() * 1000.0;
        parent.finish(if outcome.is_ok() { "completed" } else { "failed" }, None); (outcome, elapsed_ms)
    })).await;
    assert!(!events.is_empty() && events.len()<4096 && events.iter().all(crate::trace_context::valid_event), "valid finite causal capture required");
    assert!(duration(&events,"source.snapshot.writer.wait").is_some()&&duration(&events,"source.snapshot.writer.held").is_some(),"actual App writer boundaries required");
    let measurements = metrics(&events, elapsed_ms);
    emit("sample", &json!({"version":VERSION,"cohort":cohort.name(),"phase":phase,"pair":pair,"firstInPair":first,
        "variant":if scoped{"scoped"}else{"current_full"},"snapshotSha256":digest(snapshot),"measurements":measurements,
        "outcome":match &outcome {Ok(())=>json!({"status":"ok"}),Err(e)=>json!({"status":"error","httpStatus":e.0.as_u16(),"message":e.1})},"causalEvents":events}));
    Sample { outcome, metrics: measurements }
}

fn quantiles(values: &[f64]) -> Value {
    if values.is_empty() { return json!({"n":0,"p50Ms":null,"p95Ms":null}); }
    assert!(values.iter().all(|v| v.is_finite() && *v >= 0.0));
    let mut sorted = values.to_vec(); sorted.sort_by(f64::total_cmp);
    let rank = |p: usize| sorted[(sorted.len() * p).div_ceil(100) - 1];
    json!({"n":values.len(),"p50Ms":rank(50),"p95Ms":rank(95),"minMs":sorted[0],"maxMs":sorted[sorted.len()-1],"method":"nearest_rank"})
}

// Gates are outside every measured sample. Preserve the original SQL failure
// and inspect canonical rows after rollback; never erase/reseed failed cohorts.
async fn sql_rollback_gates(app: &crate::App) {
    let before = app.db.read().await.unwrap();
    let Database::Postgres { writer, .. } = &app.db else { unreachable!() };
    sqlx::query("CREATE TEMP SEQUENCE source_benchmark_fault_hits").execute(writer).await.unwrap();
    sqlx::query("CREATE FUNCTION pg_temp.source_benchmark_reject() RETURNS trigger LANGUAGE plpgsql AS 'BEGIN PERFORM nextval(''pg_temp.source_benchmark_fault_hits''); RAISE EXCEPTION ''source benchmark late metadata failure''; END;'").execute(writer).await.unwrap();
    sqlx::query("CREATE TRIGGER source_benchmark_reject BEFORE UPDATE ON communityhero.workspaces FOR EACH ROW EXECUTE FUNCTION pg_temp.source_benchmark_reject()").execute(writer).await.unwrap();
    let snapshot = json!({"posts":[],"branches":[],"items":[]});
    let rejected: ApiResult<()> = app.change_source_snapshot_scoped(SourceReadIntent::Snapshot(&snapshot), |d| {
        crate::row_mut(d,"posts",TARGET_POST)?["text"] = json!("must roll back"); d["sync"]["benchmarkFault"] = json!(true); Ok(())
    }).await;
    sqlx::query("DROP TRIGGER source_benchmark_reject ON communityhero.workspaces").execute(writer).await.unwrap();
    let error = rejected.unwrap_err();
    let reached: bool=sqlx::query_scalar("SELECT is_called FROM pg_temp.source_benchmark_fault_hits").fetch_one(writer).await.unwrap();
    assert!(reached,"the actual late SQL trigger, not an earlier guard, rejected this attempt");
    emit("gate", &json!({"gate":"late_metadata_sql_rollback","httpStatus":error.0.as_u16(),"message":error.1}));
    assert_eq!(app.db.read().await.unwrap(), before);
    let mut tx = writer.begin().await.unwrap();
    let (captured, ordinals) = load_scoped(&mut tx).await.unwrap(); let mut after = captured.clone();
    crate::row_mut(&mut after,"posts",TARGET_POST).unwrap()["text"] = json!("earlier delta must roll back");
    crate::row_mut(&mut after,"items",TARGET_ITEM).unwrap()["reason"] = json!("expected source delta");
    validate_scoped_change(&captured,&after).unwrap();
    sqlx::query("UPDATE communityhero.items SET payload=jsonb_set(payload,'{reason}','\"injected benchmark CAS conflict\"'::jsonb) WHERE workspace_id=$1 AND id=$2")
        .bind(WORKSPACE).bind(TARGET_ITEM).execute(&mut *tx).await.unwrap();
    let error = guarded_delta::persist(&mut tx,&captured,&after,&ordinals).await.unwrap_err(); tx.rollback().await.unwrap();
    assert_eq!(error.1,"Source guarded delta lost expected row identity or payload");
    emit("gate", &json!({"gate":"old_payload_cas_rollback","httpStatus":error.0.as_u16(),"message":error.1}));
    assert_eq!(app.db.read().await.unwrap(),before);
}

async fn run(cohort: Cohort) {
    let source_pin = pin("COMMUNITYHERO_SOURCE_BENCHMARK_SOURCE_PIN");
    let binary_pin = pin("COMMUNITYHERO_SOURCE_BENCHMARK_BINARY_SHA256");
    assert_eq!(executable_sha(),binary_pin,"root must pin the exact running test artifact");
    let history = bounded("COMMUNITYHERO_SOURCE_BENCHMARK_HISTORY_JOBS",100,1,4000);
    let body_bytes = bounded("COMMUNITYHERO_SOURCE_BENCHMARK_BODY_BYTES",4096,0,65536);
    let source_count=bounded("COMMUNITYHERO_SOURCE_BENCHMARK_SOURCE_CONTEXTS",0,0,10_000);
    let source_text_bytes=bounded("COMMUNITYHERO_SOURCE_BENCHMARK_SOURCE_TEXT_BYTES",2048,0,65536);
    assert!(history.checked_mul(body_bytes).unwrap() <= 64 * 1024 * 1024,"bounded raw history request text budget");
    assert!(source_count.checked_mul(source_text_bytes).unwrap()<=64*1024*1024,"bounded broad source text budget");
    let full_url=env("COMMUNITYHERO_SOURCE_BENCHMARK_FULL_URL"); let scoped_url=env("COMMUNITYHERO_SOURCE_BENCHMARK_SCOPED_URL");
    let full_name=env("COMMUNITYHERO_SOURCE_BENCHMARK_FULL_DATABASE"); let scoped_name=env("COMMUNITYHERO_SOURCE_BENCHMARK_SCOPED_DATABASE");
    assert_ne!(full_name,scoped_name,"a cohort requires two distinct fresh databases");
    let factory = fixture_with_sources(cohort,history,body_bytes,source_count,source_text_bytes).await;
    let full_db=super::super::preparation::writer_v51_fixture_db_for_profile_with(&full_url,&full_name,crate::accounts::Profile::BawRussia).await;
    let scoped_db=super::super::preparation::writer_v51_fixture_db_for_profile_with(&scoped_url,&scoped_name,crate::accounts::Profile::BawRussia).await;
    let (full,_full_folder)=app(full_db).await; let (scoped,_scoped_folder)=app(scoped_db).await;
    for app in [&full,&scoped] {
        let lifecycle = app.db.read().await.unwrap()["runtimeLifecycle"].clone();
        for (ordinal, step) in factory.seed_steps.iter().enumerate() {
            // Replay the single native fixture's committed history in order.
            // The PG App retains its own active lifecycle; original paid/FIRST/
            // repair owners inside immutable job evidence are never rebound.
            app.db.change(|d| {
                assert_eq!(d["runtimeLifecycle"],lifecycle);
                *d=step.state.clone(); d["runtimeLifecycle"]=lifecycle.clone(); Ok(())
            }).await.unwrap_or_else(|error|panic!("guarded PG fixture replay {ordinal} {}: {error:?}",step.label));
            let mut expected=step.state.clone(); expected["runtimeLifecycle"]=lifecycle.clone();
            assert_eq!(app.db.read().await.unwrap(),expected,"exact PG fixture checkpoint {ordinal} {}",step.label);
        }
    }
    let mut left=full.db.read().await.unwrap(); let mut right=scoped.db.read().await.unwrap();
    assert_eq!(left,right,"one exact native fixture and deterministic lifecycle, no paid/UUID/clock normalization");
    let full_view=full.change_source_snapshot(|d|Ok(d.clone())).await.unwrap();
    let scoped_view=scoped.change_source_snapshot_scoped(SourceReadIntent::Snapshot(&json!({})),|d|Ok(d.clone())).await.unwrap();
    assert_eq!(full_view,project(&left).unwrap(),"actual full SQL loader parity");
    assert_eq!(scoped_view,project_scoped(&right).unwrap(),"actual scoped SQL loader parity");
    let fixture_sha=digest(&factory.state);
    emit("manifest",&json!({"version":VERSION,"cohort":cohort.name(),"company":"baw-russia","sourcePin":source_pin,
        "binarySha256":binary_pin,"generatorSha256":sha(include_str!("storage_source_benchmark_tests.rs").as_bytes()),
        "fixtureSha256":fixture_sha,"historyJobs":history,"historyRequestTextBytesEach":body_bytes,"warmPairsPerPhase":PAIRS,
        "additionalSourceContexts":source_count,"sourceTextBytesPerPostAndMessage":source_text_bytes,
        "sourceDimension":"complete independent post/branch/item rows, no paid outcomes invented; counts/serialized bytes measured below",
        "fixtureSeedSteps":factory.seed_steps.iter().enumerate().map(|(ordinal,step)|json!({"ordinal":ordinal,
            "label":step.label,"canonicalSha256":digest(&step.state)})).collect::<Vec<_>>(),
        "fixtureReplay":"one guarded native SQLite construction, each committed state replayed in order through full guarded PG writers; no repeated paid execution",
        "fixtureProducerLifecycleSha256":digest(&factory.state["runtimeLifecycle"]),"activePgLifecycleSha256":digest(&left["runtimeLifecycle"]),
        "historyClass":if cohort==Cohort::MandatoryRepair{"current_native_unpaid_mandatory_captures_cancelled_before_dispatch"}else{"settled_unreferenced_synthetic_legacy_discussion"},
        "fixtureIdentity":"one native history; protected records copied unchanged to pair; destination lifecycle remains local; IDs/clocks can vary between runs",
        "canonical":sizing(&left),"distribution":distribution(&left),"currentFullProjection":sizing(&full_view),"scopedProjection":sizing(&scoped_view),
        "scopedControlRows":scoped_view["sourceJobControls"]["jobs"].as_array().unwrap().len(),
        "scopedControlLogicalBytes":scoped_view["sourceJobControls"].to_string().len(),"storedSqlCensus":stored_sql_sizing(&full.db).await,
        "mandatoryJobIds":factory.mandatory_jobs,"protectedDraftId":factory.draft,"nativePaidFixture":"C only: real private CAS records from native factory; media frame pins remain explicitly synthetic",
        "r9SizeComparison":"not_matched: 241855984 bytes / 24524 rows was a preparation projection maximum",
        "cold":"not_measured","contention":"not_measured","releaseAdmitted":false}));
    sql_rollback_gates(&scoped).await;
    let no_op=json!({"posts":[],"branches":[],"items":[]});
    // Settle native maintenance once, then require a genuinely unchanged replay.
    for _ in 0..2 {
        full.change_source_snapshot(|d|crate::merge_snapshot(d,&no_op)).await.unwrap();
        scoped.change_source_snapshot_scoped(SourceReadIntent::Snapshot(&no_op),|d|crate::merge_snapshot(d,&no_op)).await.unwrap();
    }
    left=full.db.read().await.unwrap(); right=scoped.db.read().await.unwrap(); assert_eq!(left,right);
    let protected_left=immutable(&left); let protected_right=immutable(&right);
    let source=crate::row(&left,"posts",TARGET_POST).unwrap().clone();
    let snapshots: Vec<_>=(0..PAIRS).map(|n| {let mut post=source.clone();post["text"]=json!(format!("Synthetic source observation {:03}",n+1));
        json!({"posts":[post],"branches":[],"items":[]})}).collect();
    let mut summaries=Vec::new();
    for phase in ["noop","mutation"] {
        let mut samples: [Vec<Value>;2]=[Vec::new(),Vec::new()];
        for pair in 0..PAIRS {
            let snapshot=if phase=="noop"{&no_op}else{&snapshots[pair]};
            let mut expected_left=left.clone(); let mut expected_right=right.clone();
            crate::merge_snapshot(&mut expected_left,snapshot).unwrap(); crate::merge_snapshot(&mut expected_right,snapshot).unwrap();
            if phase=="noop" {assert_eq!(expected_left,left,"noop must really have no canonical mutation");assert_eq!(expected_right,right);}
            else {assert_ne!(expected_left,left,"every mutation is effective");assert_ne!(expected_right,right);}
            let (a,b)=if pair%2==0 {
                let a=sample(&full,snapshot,false,cohort,phase,pair,true,&source_pin,&binary_pin).await;
                let b=sample(&scoped,snapshot,true,cohort,phase,pair,false,&source_pin,&binary_pin).await; (a,b)
            } else {
                let b=sample(&scoped,snapshot,true,cohort,phase,pair,true,&source_pin,&binary_pin).await;
                let a=sample(&full,snapshot,false,cohort,phase,pair,false,&source_pin,&binary_pin).await; (a,b)
            };
            // Readback/hashing/parity and trace serialization are outside timing.
            left=full.db.read().await.unwrap(); right=scoped.db.read().await.unwrap();
            if a.outcome.is_err()||b.outcome.is_err() {
                emit("incomplete",&json!({"cohort":cohort.name(),"phase":phase,"pair":pair,"fullStateSha256":digest(&left),
                    "scopedStateSha256":digest(&right),"reason":"original sample errors retained; no retry/reseed or success-only quantiles"}));
            }
            a.outcome.unwrap(); b.outcome.unwrap();
            assert_eq!(left,expected_left,"full canonical reducer parity");assert_eq!(right,expected_right,"scoped canonical reducer parity");
            assert_eq!(left,right);
            assert_eq!(immutable(&left),protected_left,"full protected state byte/logical equality");
            assert_eq!(immutable(&right),protected_right,"scoped protected state byte/logical equality");
            if cohort==Cohort::Ambiguous {assert!(b.metrics["fallbackReasons"].as_array().unwrap().iter().any(|r|r=="source_scope_legacy_fallback"),"fallback must be observed in actual loader trace");}
            samples[0].push(a.metrics); samples[1].push(b.metrics);
            emit("parity",&json!({"cohort":cohort.name(),"phase":phase,"pair":pair,
                "fullCanonicalSha256":digest(&left),"scopedCanonicalSha256":digest(&right),"status":"equal"}));
        }
        for (variant, values) in ["current_full","scoped"].into_iter().zip(samples) {
            let mut result=serde_json::Map::new();
            for key in ["appCallMs","writerWaitMs","writerHeldMs","transactionMs","loadMs","cloneMs","domainMs","validationMs","persistEntitiesMs","commitMs","poolReturnMs","cleanupMs"] {
                let observed=values.iter().filter_map(|v|v[key].as_f64()).collect::<Vec<_>>(); result.insert(key.into(),quantiles(&observed));
            }
            let mut fallbacks=BTreeMap::<String,usize>::new();
            for sample in &values {for reason in sample["fallbackReasons"].as_array().unwrap(){
                *fallbacks.entry(reason.as_str().unwrap().to_owned()).or_default()+=1;
            }}
            summaries.push(json!({"phase":phase,"variant":variant,"quantiles":result,"fallbackEventCounts":fallbacks}));
        }
    }
    full.db.close().await; scoped.db.close().await;
    let reopened_full=Database::postgres(&full_url).await.unwrap();let reopened_scoped=Database::postgres(&scoped_url).await.unwrap();
    assert_eq!(reopened_full.read().await.unwrap(),left);assert_eq!(reopened_scoped.read().await.unwrap(),right);
    reopened_full.close().await;reopened_scoped.close().await;
    emit("summary",&json!({"version":VERSION,"cohort":cohort.name(),"fixtureSha256":fixture_sha,"sourcePin":source_pin,
        "binarySha256":binary_pin,"completedPairs":2*PAIRS,"summaries":summaries,"reopenParity":true,"releaseAdmitted":false,
        "limitations":["warm uncontended local App only","C media pins are synthetic; no live delivery proof",
            "no cold/cache reset or contention cohort","full writer SQL counters/return span may be uninstrumented",
            "mutation changes benchmark text source; mandatory repair source remains current",
            "native fixture CAS was verified during construction; no independent CAS reopen in this test"]}));
}

#[tokio::test]
#[ignore="matched warm source benchmark A; root must pin binary/source and provide TWO fresh isolated BAW PostgreSQL databases"]
async fn postgres_source_writer_matched_legacy_warm_pair() { run(Cohort::Legacy).await; }
#[tokio::test]
#[ignore="matched warm source benchmark B; root must pin binary/source and provide TWO fresh isolated BAW PostgreSQL databases"]
async fn postgres_source_writer_matched_ambiguous_warm_pair() { run(Cohort::Ambiguous).await; }
#[tokio::test]
#[ignore="matched warm source benchmark C; root must pin binary/source and provide TWO fresh isolated BAW PostgreSQL databases"]
async fn postgres_source_writer_matched_mandatory_repair_warm_pair() { run(Cohort::MandatoryRepair).await; }

#[test]
fn matched_source_benchmark_quantiles_keep_small_n_and_missing_metrics_explicit() {
    assert_eq!(quantiles(&[])["p95Ms"],Value::Null);
    let values=(1..=30).map(f64::from).collect::<Vec<_>>();let q=quantiles(&values);
    assert_eq!(q["n"],30);assert_eq!(q["p50Ms"],15.0);assert_eq!(q["p95Ms"],29.0);
    assert_eq!(metrics(&[],1.0)["observedCounterSums"]["payloadRead"]["bytes"],Value::Null);
    assert_eq!(metrics(&[],1.0)["cleanupMs"],Value::Null);
}

#[test]
fn matched_source_benchmark_wide_dimension_retains_complete_source_text() {
    let mut d=crate::empty();crate::accounts::initialize(&mut d,crate::accounts::Profile::BawRussia).unwrap();normalize(&mut d);
    let before=d.clone();add_wide_sources(&mut d,10,1024);let scoped=project_scoped(&d).unwrap();
    for table in ["posts","branches","items"] {
        assert_eq!(crate::list(&d,table).len(),crate::list(&before,table).len()+10);
        assert_eq!(scoped[table],d[table],"wide mandatory source rows are never compacted");
    }
    let post=crate::row(&d,"posts","source-wide-post-9").unwrap();
    let branch=crate::row(&d,"branches","source-wide-branch-9").unwrap();
    assert_eq!(post["text"].as_str().unwrap().len(),1024);assert_eq!(branch["messages"][0]["text"],post["text"]);
    assert!(sizing(&d)["serializedWorkspaceBytes"].as_u64().unwrap()>sizing(&before)["serializedWorkspaceBytes"].as_u64().unwrap()+20*1024);
    assert_eq!(d["jobs"],before["jobs"]);assert_eq!(d["operations"],before["operations"]);
}

#[tokio::test]
async fn matched_source_benchmark_native_cohorts_have_expected_retention() {
    for cohort in [Cohort::Legacy,Cohort::Ambiguous,Cohort::MandatoryRepair] {
        let f=fixture(cohort,3,128).await;
        assert_eq!(f.state["account"],"BAW Russia");
        assert_eq!(distribution(&f.state)["unknownOperations"],1);
        assert_eq!(crate::row(&f.state,"items",TARGET_ITEM).unwrap()["draft"],"Preserve operator draft byte for byte");
        if cohort==Cohort::MandatoryRepair {
            assert_eq!(distribution(&f.state)["currentUnpaidCaptures"],3);
            assert!(distribution(&f.state)["jobsWithPaidReferences"].as_u64().unwrap()>0);
            assert_eq!(project_scoped(&f.state).unwrap()["jobs"],f.state["jobs"]);
        } else {
            assert_eq!(distribution(&f.state)["currentUnpaidCaptures"],0);
            assert_eq!(distribution(&f.state)["jobsWithPaidReferences"],0);
        }
    }
}
