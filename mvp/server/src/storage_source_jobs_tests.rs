use super::*;
use serde_json::json;

fn result(bytes: usize) -> Value {
    json!({"text":"x".repeat(bytes),"sources":[{"title":"x".repeat(bytes)}],
        "assessments":[{"itemId":"item","reason":"x".repeat(bytes)}],
        "proposals":[{"itemId":"item","text":"x".repeat(bytes)}],
        "runMetadata":{"model":"saved-model","chargedWebCalls":3,"inputSha256":"c".repeat(64)},
        "editorialEvidence":{"version":1,"proof":"paid editorial evidence"},
        "factDependencies":[{"itemId":"item","dependencyId":"fact"}],
        "futureResultEvidence":{"retained":true}})
}
fn job(bytes: usize) -> Value {
    json!({"id":"settled","kind":"assistant","status":"completed","purpose":"auto_prepare","refId":"item",
        "prepareBundle":{"version":1,"id":"bundle","digest":"a".repeat(64),"dependencyDigest":"b".repeat(64),
            "itemIds":["item"],"request":{"items":[{"id":"item","text":"x".repeat(bytes)}]}},
        "prepareOutcome":{"itemId":"item","status":"needs_attention","reason":"Exact saved paid assessment"},
        "autoPreparationInputs":{"item":{"inputDigest":"exact-item-input"}},
        "scopeReservation":{"account":"LikeAvto","ownerJobId":"settled","opaqueGrant":"retained"},
        "scopeModelAttempt":{"status":"completed","attemptId":"paid-attempt","spent":true},
        "preparationStages":{"first":{"status":"completed","at":"saved","reviewRequired":true,"result":result(bytes)},
            "review":{"status":"completed","at":"saved","research":{"status":"completed"},"result":result(bytes)},
            "reviewChunks":{"version":2,"status":"completed","planDigest":"d".repeat(64),"usage":{"chargedWebCalls":3},
                "chunks":[{"id":"chunk-1","itemIds":["item"],"attempts":[{"id":"attempt-1","status":"completed", 
                    "resultDigest":"e".repeat(64),"observedWebCalls":3,"contract":{"grant":"exact grant"}}],"result":result(bytes)}]},
            "groupAdmission":[{"id":"group","status":"admitted","itemIds":["item"],"proposalIds":[]}]},
        "result":result(bytes),"futureRootControl":{"retained":true}})
}
fn cases() -> Vec<Value> {
    let base=job(16);
    let mut cases=vec![base.clone(),Value::Null,json!([]),json!({})];
    for (path,value) in [
        ("/status",json!("unknown")),("/status",json!("running")),("/status",json!("failed")),("/status",json!("cancelled")),
        ("/kind",json!("media")),("/id",json!("")),("/prepareBundle/version",json!(1.0)),
        ("/result",Value::Null),("/result",json!([])),("/result",json!("future-shape")),
        ("/prepareBundle/request",json!([])),("/prepareBundle/digest",json!("A".repeat(64))),
        ("/prepareBundle/dependencyDigest",Value::Null),("/prepareBundle/itemIds",json!({})),
        ("/recovery",json!({"jobId":"unknown-owner"})),("/scopeModelAttempt",json!([])),
        ("/scopeModelAttempt/status",json!("unknown")),("/scopeModelAttempt/status",json!("running")),
        ("/preparationStages",json!([])),("/preparationStages",json!({"futureStage":{"status":"completed","result":result(16)}})),
        ("/preparationStages/first",json!("future-shape")),("/preparationStages/first/status",json!("unknown")),
        ("/preparationStages/review/status",json!("failed")),("/preparationStages/first/result",json!([])),
        ("/preparationStages/first/result/text",json!([])),("/preparationStages/first/result/runMetadata",json!("malformed")),
        ("/preparationStages/reviewChunks/status",json!("held")),("/preparationStages/reviewChunks/chunks",json!([])),
        ("/preparationStages/reviewChunks/chunks",json!([null])),
        ("/preparationStages/reviewChunks/chunks/0/result/sources",json!({})),
        ("/preparationStages/reviewChunks/chunks/0/attempts",json!([])),
        ("/preparationStages/reviewChunks/chunks/0/attempts",json!([null])),
        ("/preparationStages/reviewChunks/chunks/0/attempts/0/status",json!("unknown")),
        ("/preparationStages/reviewChunks/chunks/0/attempts/0/resultDigest",json!("invalid")),
        ("/preparationStages/groupAdmission",json!([null])),
    ] {
        let mut value_job=base.clone();
        if path=="/recovery" {value_job["recovery"]=value;}
        else {*value_job.pointer_mut(path).unwrap()=value;}
        cases.push(value_job);
    }
    cases
}

#[test]
fn source_stage_projection_retains_exact_paid_lineage_control_and_extensions() {
    let original=job(256*1024);let ids=HashSet::new();
    let view=project_assistant(&original,Some(&ids)).unwrap();
    for field in ["id","kind","status","purpose","refId","prepareOutcome","autoPreparationInputs",
        "scopeReservation","scopeModelAttempt","futureRootControl"] {assert_eq!(view[field],original[field],"{field}");}
    for path in ["/preparationStages/first/result","/preparationStages/review/result","/preparationStages/reviewChunks/chunks/0/result"] {
        for field in ["runMetadata","editorialEvidence","factDependencies","futureResultEvidence"] {
            assert_eq!(view.pointer(path).unwrap()[field],original.pointer(path).unwrap()[field],"{path}/{field}");
        }
        for field in BODY_FIELDS {assert!(view.pointer(path).unwrap().get(*field).is_none());}
    }
    assert_eq!(view["preparationStages"]["reviewChunks"]["chunks"][0]["attempts"],
        original["preparationStages"]["reviewChunks"]["chunks"][0]["attempts"]);
    assert_eq!(view["preparationStages"]["reviewChunks"]["usage"],original["preparationStages"]["reviewChunks"]["usage"]);
    assert_eq!(view["preparationStages"]["groupAdmission"],original["preparationStages"]["groupAdmission"]);
    assert!(view["prepareBundle"].get("request").is_none());assert!(view.get("result").is_none());
    assert!(view.to_string().len()*100<original.to_string().len(),"deterministic synthetic wire/decode/clone reduction");
    assert!(original["preparationStages"]["first"]["result"]["text"].is_string(),"input evidence remains intact");
}

#[test]
fn source_stage_projection_unknown_malformed_future_legacy_and_bound_fallback() {
    let ids=HashSet::new();let cases=cases();assert!(project_assistant(&cases[0],Some(&ids)).is_some());
    for job in &cases[1..] {assert!(project_assistant(job,Some(&ids)).is_none(),"fallback must preserve original checkpoints: {job}");}
    let bound=HashSet::from(["settled".to_owned()]);
    assert!(project_assistant(&cases[0],Some(&bound)).is_none());assert!(project_assistant(&cases[0],None).is_none());
    let mut legacy=cases[0].clone();legacy["prepareBundle"].as_object_mut().unwrap().remove("request");
    assert!(project_assistant(&legacy,Some(&ids)).is_none());
}

#[test]
fn source_candidate_controls_retain_pointerless_assessment_and_force_new_materials_full() {
    let original=job(4096);
    let (control,full)=source_control(&original);
    assert!(!full);
    for field in ["id","kind","purpose","status","prepareOutcome","autoPreparationInputs"] {
        assert_eq!(control[field],original[field]);
    }
    assert_eq!(control["prepareBundle"]["itemIds"],original["prepareBundle"]["itemIds"]);
    assert!(control["prepareBundle"].get("request").is_none());
    assert!(control.get("preparationStages").is_none(),"control is not a model or recovery job");
    for field in MATERIAL_REPAIR_FIELDS {
        let mut changed=original.clone();changed[*field]=json!({"version":1,"status":"unknown"});
        assert!(source_control(&changed).1,"full original material/repair field: {field}");
    }
    let mut uncertain=original.clone();uncertain["scopeModelAttempt"]["status"]=json!("unknown");
    assert!(source_control(&uncertain).1);
    let mut future=original.clone();future["preparationStages"]["materialAcquisition"]=json!({"status":"completed"});
    assert!(source_control(&future).1,"new stage never inherits old checkpoint compaction authority");
    assert!(original["prepareBundle"]["request"].is_object());
}

fn workspace() -> Value {workspace_with_bytes(128*1024)}
fn workspace_with_bytes(bytes:usize) -> Value {
    let mut d=crate::empty();super::super::normalize(&mut d);
    d["posts"]=json!([{"id":"post","text":"Before"}]);
    d["branches"]=json!([{"id":"branch","postId":"post","messages":[]}]);
    d["items"]=json!([{"id":"item","itemId":"provider-item","objectId":"11391","postId":"post","branchId":"branch",
        "workflow":"attention","providerStatus":"new","draft":"","revision":3,"autoPreparation":{"status":"queued"}}]);
    let mut captured=job(bytes);captured.as_object_mut().unwrap().remove("scopeReservation");
    captured["prepareBundle"]["request"]["account"]=d["account"].clone();
    use sha2::{Digest,Sha256};
    captured["prepareBundle"]["digest"]=json!(format!("{:x}",Sha256::digest(captured["prepareBundle"]["request"].to_string().as_bytes())));
    d["jobs"]=json!([captured]);
    d["jobs"][0]["scopeReservation"]=crate::preparation_reservations::capture(&d,"settled").unwrap();
    d
}
fn projection(d:&Value)->Value {
    let mut view=super::super::project(d).unwrap();
    let ids=super::super::protected_jobs(d);
    view["jobs"]=Value::Array(d["jobs"].as_array().unwrap().iter().map(|job|
        project_assistant(job,ids.as_ref()).unwrap_or_else(||super::super::source_job(job,ids.as_ref()))).collect());
    view
}
fn snapshot()->Value {json!({"posts":[{"id":"post","text":"After"}],"branches":[{"id":"branch","postId":"post","messages":[]}],
    "items":[{"id":"item","itemId":"provider-item","objectId":"11391","postId":"post","branchId":"branch","providerStatus":"closed",
        "contextObservedAt":"2026-10-04T08:00:00Z","providerStatusObservedAt":"2026-10-04T08:00:00Z"}]})}
fn normalize_clock(d:&mut Value){for branch in d["branches"].as_array_mut().unwrap(){branch["observedAt"]=json!("fixed-clock");}}

#[test]
fn source_stage_projection_pointerless_paid_recovery_merge_and_readonly_guards_match_full() {
    let original=workspace();let before=projection(&original);
    assert!(before["jobs"][0]["preparationStages"]["first"]["result"].get("text").is_none());
    let mut full=original.clone();let mut after=before.clone();
    crate::auto_prepare::reconcile_stale(&mut full,1_790_000_000);crate::auto_prepare::reconcile_stale(&mut after,1_790_000_000);
    assert_eq!(after["items"],full["items"]);assert_eq!(after["items"][0]["autoPreparation"]["jobId"],"settled");
    assert_eq!(after["items"][0]["autoPreparation"]["inputDigest"],"exact-item-input");
    // Same projected transaction installs legacy pointers before a second merge.
    crate::merge_snapshot(&mut full,&snapshot()).unwrap();crate::merge_snapshot(&mut after,&snapshot()).unwrap();
    let mut actual=original.clone();super::super::apply(&mut actual,&before,&after).unwrap();
    normalize_clock(&mut full);normalize_clock(&mut actual);assert_eq!(actual,full);
    assert_eq!(actual["jobs"],original["jobs"],"source persistence preserves all paid bodies and grants");
    for path in ["/jobs/0/status","/jobs/0/scopeModelAttempt/status","/jobs/0/scopeReservation/keysDigest",
        "/jobs/0/preparationStages/reviewChunks/chunks/0/attempts/0/resultDigest"] {
        let mut illegal=before.clone();*illegal.pointer_mut(path).unwrap()=json!("forged");
        let mut durable=original.clone();assert!(super::super::apply(&mut durable,&before,&illegal).is_err(),"readonly/paid guard: {path}");
        assert_eq!(durable,original,"failed admission leaves durable evidence intact");
    }
}

#[test]
fn source_stage_projection_nested_proposal_lineage_and_unknown_operation_preserve_full_jobs() {
    let mut d=workspace();d["proposals"]=json!([{"id":"proposal","itemId":"item","status":"unknown",
        "history":[{"origin":{"recovery":{"prepareRunId":"settled"}}}]}]);
    d["operations"]=json!([{"id":"operation","itemId":"item","proposalId":"proposal","status":"unknown"}]);
    // Use the Media reducer's valid completed/UNKNOWN/applicability closure so
    // the integrated global ledger guard remains strict at this storage seam.
    let media=crate::media_analysis::test_workspace_fixture(d["account"].as_str().unwrap());
    d["jobs"].as_array_mut().unwrap().extend(media["jobs"].as_array().unwrap().iter().cloned());
    d["jobs"].as_array_mut().unwrap().extend([
        json!({"id":"fact-worker","kind":"assistant","status":"unknown","refId":"item",
            "factWorkerScope":{"version":1,"company":"LikeAvto","parentJobId":"settled","itemIds":["item"],
                "dependencyIds":["fact"],"attemptId":"unknown-paid-fact","providerRetryAllowed":false},
            "preparationStages":{"first":{"status":"unknown","result":{"exactPaidWitness":"retained"}}}}),
    ]);
    let before=projection(&d);assert_eq!(before["jobs"],d["jobs"]);
    let mut full=d.clone();let mut after=before.clone();crate::merge_snapshot(&mut full,&snapshot()).unwrap();crate::merge_snapshot(&mut after,&snapshot()).unwrap();
    let mut actual=d.clone();super::super::apply(&mut actual,&before,&after).unwrap();normalize_clock(&mut full);normalize_clock(&mut actual);
    assert_eq!(actual,full);assert_eq!(actual["operations"][0]["status"],"unknown");assert_eq!(actual["jobs"],d["jobs"]);
}

#[test]
fn source_stage_projection_new_media_analysis_and_applicability_contracts_stay_full() {
    let ids=HashSet::new();
    for kind in ["media_analysis","media_analysis_applicability"] {
        let mut value=job(128);value["kind"]=json!(kind);
        value["analysis"]=json!({"company":"LikeAvto","fileSha256":"f".repeat(64),"stage":"asr",
            "attempts":[{"id":"uncertain","status":"unknown"}],"segments":[{"startMs":0,"endMs":1000}],
            "resultRef":{"sha256":"f".repeat(64),"bytes":123},"casRef":{"sha256":"e".repeat(64),"bytes":456}});
        value["result"]=json!({"schemaVersion":1,"proof":{"target":"target","donor":"donor","result":"immutable-result", 
            "casRef":{"sha256":"e".repeat(64),"bytes":456}}});
        assert!(project_assistant(&value,Some(&ids)).is_none());
        assert_eq!(super::super::source_job(&value,Some(&ids)),value,"future media ownership/proof rows remain whole");
    }
}

// Exact assistant branch from canonical storage_source_snapshot.rs preimage
// SHA256 198365b1234bb8e326f7f0c4cf7d7d09a30432d548448d3f40079e91679b11de.
// Fixture kinds are assistant, so the separate media branch is unreachable.
fn pinned_original_source_job(job:&Value,protected:Option<&HashSet<String>>)->Value {
    let compactable=protected.is_some_and(|protected| {
        job.is_object() && job["kind"] == "assistant"
            && matches!(job["status"].as_str(), Some("completed" | "failed" | "cancelled"))
            && job["id"].as_str().is_some_and(|id| !id.is_empty() && !protected.contains(id))
            && job["prepareBundle"].is_object() && job["prepareBundle"]["request"].is_object()
            && job["prepareBundle"]["version"] == 1
            && hash(&job["prepareBundle"]["digest"]) && hash(&job["prepareBundle"]["dependencyDigest"])
            && job["prepareBundle"]["itemIds"].is_array()
    });
    let mut view=job.clone();
    if compactable {
        view["prepareBundle"].as_object_mut().unwrap().remove("request");
        if job["status"]=="completed" && job["result"].is_object() {view.as_object_mut().unwrap().remove("result");}
    }
    view
}

#[test]
fn source_stage_projection_thousand_jobs_legacy_clone_then_omit_vs_selective_projection() {
    let mut d=workspace_with_bytes(1024);let sample=d["jobs"][0].clone();
    d["jobs"]=Value::Array((0..1200).map(|n| {
        let mut job=sample.clone();let id=format!("job-{n}");job["id"]=json!(id);
        job["scopeReservation"]["ownerJobId"]=job["id"].clone();job
    }).collect());
    d["items"][0]["history"]=Value::Array((0..200).map(|n|json!({"jobId":format!("job-{n}")})).collect());
    let protected=super::super::protected_jobs(&d).unwrap();assert_eq!(protected.len(),200);
    let jobs=d["jobs"].as_array().unwrap();
    let old_start=std::time::Instant::now();let old:Vec<_>=jobs.iter()
        .map(|job|pinned_original_source_job(job,Some(&protected))).collect();let old_us=old_start.elapsed().as_micros();
    let new_start=std::time::Instant::now();let new:Vec<_>=jobs.iter().map(|job|
        project_assistant(job,Some(&protected)).unwrap_or_else(||pinned_original_source_job(job,Some(&protected))))
        .collect();let new_us=new_start.elapsed().as_micros();
    for n in 0..200 {assert_eq!(new[n],jobs[n],"protected input/result/provenance remains full");}
    for n in 200..1200 {
        for field in ["id","status","scopeReservation","scopeModelAttempt","prepareOutcome","autoPreparationInputs"] {
            assert_eq!(new[n][field],jobs[n][field]);
        }
    }
    let mut before=super::super::project(&d).unwrap();before["jobs"]=json!(new);
    let mut after=before.clone();let mut full=d.clone();
    crate::merge_snapshot(&mut full,&json!({})).unwrap();crate::merge_snapshot(&mut after,&json!({})).unwrap();
    let mut actual=d.clone();super::super::apply(&mut actual,&before,&after).unwrap();
    normalize_clock(&mut full);normalize_clock(&mut actual);assert_eq!(actual,full,"benchmark semantic admission precedes metric report");
    let full_bytes=d["jobs"].to_string().len();let old_bytes=serde_json::to_string(&old).unwrap().len();
    let new_bytes=before["jobs"].to_string().len();assert!(new_bytes<old_bytes);
    eprintln!("source_stage_projection_benchmark {}",json!({"jobs":1200,"protectedJobs":200,"fullJobBytes":full_bytes,
        "originalProjectedBytes":old_bytes,"selectiveProjectedBytes":new_bytes,"originalCloneThenOmitUs":old_us,
        "selectiveProjectionUs":new_us,"baselineSourceSha256":"198365b1234bb8e326f7f0c4cf7d7d09a30432d548448d3f40079e91679b11de",
        "note":"synthetic identical source_job selector fixtures; excludes SQL/lock/live costs; no elapsed assertion"}));
}

#[tokio::test]
#[ignore="requires a pristine explicitly isolated communityhero_writer_v51_test_ PostgreSQL fixture"]
async fn postgres_source_stage_projection_shape_parity_and_actual_loader_retention() {
    use sqlx::Row;
    let db=crate::storage::preparation::writer_v51_fixture_db().await;
    let crate::storage::Database::Postgres{writer,..}=&db else{panic!("isolated PostgreSQL fixture required")};
    let statement=format!("WITH fixture(payload) AS (SELECT $1::jsonb) SELECT payload::text AS original, ({})::text AS payload FROM fixture",payload_sql("payload"));
    let mut inputs=cases();inputs.extend([job(0),job(256*1024)]);
    for field in ["first","review","reviewChunks","groupAdmission"] {
        let mut value=job(16);value["preparationStages"].as_object_mut().unwrap().remove(field);inputs.push(value.clone());
        value["preparationStages"][field]=Value::Null;inputs.push(value);
    }
    for value in inputs {
        for (ids,valid) in [(Vec::<String>::new(),true),(vec!["settled".to_owned()],true),(Vec::new(),false)] {
            let record=sqlx::query(sqlx::AssertSqlSafe(statement.as_str())).bind(value.to_string()).bind(ids.clone()).bind(valid)
                .fetch_one(writer).await.unwrap();
            let actual:Value=serde_json::from_str(record.try_get::<&str,_>("payload").unwrap()).unwrap();
            let stored:Value=serde_json::from_str(record.try_get::<&str,_>("original").unwrap()).unwrap();
            let benchmark_wire=valid && ids.is_empty() && stored["preparationStages"]["first"]["result"]["text"]
                .as_str().is_some_and(|text| text.is_empty() || text.len()==256*1024);
            let original_wire_bytes=record.try_get::<&str,_>("original").unwrap().len();
            let projected_wire_bytes=record.try_get::<&str,_>("payload").unwrap().len();
            let protected=valid.then(||ids.into_iter().collect::<HashSet<_>>());
            let expected=project_assistant(&stored,protected.as_ref()).unwrap_or(stored);
            assert_eq!(actual,expected,"Rust/SQL must agree on shape, absent/null, protected IDs and uncertain checkpoints");
            if benchmark_wire {eprintln!("source_stage_sql_fixture_bytes {}",json!({"originalWireBytes":original_wire_bytes,
                "projectedWireBytes":projected_wire_bytes,"note":"actual isolated PostgreSQL expression return bytes after parity; synthetic source job"}));}
        }
    }
    let original=workspace();db.change(|d|{*d=original.clone();Ok(())}).await.unwrap();
    let expected=projection(&db.read().await.unwrap());
    // Connected splice gate: fails if helper was authored but not wired into load.
    let (loaded,changed)=db.change_source_snapshot_observed(|d|Ok(d.clone())).await.unwrap();
    assert!(!changed);assert_eq!(loaded,expected,"actual PostgreSQL source loader must use the new expression");
    let durable=db.read().await.unwrap();assert_eq!(durable["jobs"],original["jobs"]);
    db.close().await;
}
