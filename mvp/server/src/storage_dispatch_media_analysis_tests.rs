use super::*;
use serde_json::json;
const AT:&str="2026-10-04T08:00:00Z";

fn authority_workspace()->Value {
    let mut d=crate::empty();normalize(&mut d);
    d["jobs"]=crate::media_analysis::test_workspace_fixture("LikeAvto")["jobs"].clone();
    add_dispatch_target(&mut d,"target-post","target-key");
    add_legacy_jobs(&mut d);
    d
}
fn add_dispatch_target(d:&mut Value,post_id:&str,post_key:&str) {
    if !d["posts"].as_array().unwrap().iter().any(|p|p["id"]==post_id) {
        d["posts"].as_array_mut().unwrap().push(json!({"id":post_id,"postKey":post_key,"text":"target"}));
    }
    d["branches"]=json!([{"id":"dispatch-branch","postId":post_id,"messages":[]}]);
    d["items"]=json!([{"id":"dispatch-item","postId":post_id,"branchId":"dispatch-branch","text":"question","workflow":"attention","revision":1}]);
    // Neither retained media job is referenced by this prepare run.
    d["proposals"]=json!([{"id":"dispatch-proposal","itemId":"dispatch-item","status":"draft","prepareRunId":"referenced-prepare"}]);
}
fn add_legacy_jobs(d:&mut Value) {
    d["jobs"].as_array_mut().unwrap().extend([
        json!({"id":"referenced-prepare","kind":"assistant","status":"completed","result":{"privatePaidBody":"not dispatch evidence"},"prepareBundle":{"retained":"bundle"},"unrelated":"omit"}),
        json!({"id":"unrelated-paid","kind":"assistant","status":"completed","result":{"large":"x".repeat(128*1024)}}),
        json!({"id":"legacy-media","kind":"media","purpose":"auto_media","visualContractVersion":2,"status":"completed","result":{"visualProgress":{"phase":"inventory","sourcePostId":"target-post"},"paidBody":"omit"},"analysis":"omit"}),
    ]);
}
fn retained_rows(d:&Value)->Vec<Value> {
    rows(d,"jobs").unwrap().iter().filter(|j|retained_analysis_job(j)).cloned().collect()
}

#[tokio::test]
async fn dispatch_analysis_predicate_and_projection_match_actual_sqlite_types() {
    let pool=sqlx::sqlite::SqlitePoolOptions::new().max_connections(1).connect("sqlite::memory:").await.unwrap();
    let mut cases=vec![Value::Null,json!([]),json!({}),json!({"kind":null}),json!({"kind":3}),json!({"kind":true}),
        json!({"kind":["media_analysis"]}),json!({"kind":{"nested":"media_analysis"}}),json!({"kind":"assistant","result":{"private":true}})];
    for kind in ANALYSIS_JOBS {
        for status in ["completed","unknown","failed","queued"] {
            cases.push(json!({"id":"job","kind":kind,"status":status,"result":{"proof":{"complete":"exact"}},"analysis":{"attempts":[{"status":"unknown"}]},"future":{"keep":true}}));
        }
    }
    for value in cases {
        let predicate=retained_analysis_job_sql("?1",false);
        let sql=format!("SELECT COALESCE({predicate},0)");
        let selected:i64=sqlx::query_scalar(sqlx::AssertSqlSafe(sql.as_str())).bind(value.to_string()).fetch_one(&pool).await.unwrap();
        assert_eq!(selected!=0,retained_analysis_job(&value));
        let sql=format!("SELECT {}",compact_payload_sql("jobs","?1",false,0));
        let actual:String=sqlx::query_scalar(sqlx::AssertSqlSafe(sql.as_str())).bind(value.to_string()).fetch_one(&pool).await.unwrap();
        assert_eq!(parse(&actual).unwrap(),compact_context_record(&value,"jobs"));
        if retained_analysis_job(&value) {assert_eq!(parse(&actual).unwrap(),value);}
    }
    pool.close().await;
}
#[test]
fn dispatch_pg_jobs_predicate_expands_both_closed_kind_paths() {
    let actual=pg_dispatch_jobs_statement();
    assert!(!actual.contains("{analysis_jobs}"));
    assert!(actual.contains("kind IN ('media_analysis','media_analysis_applicability')"));
    assert!(actual.contains(&retained_analysis_job_sql("payload",true)));
    assert!(actual.starts_with('(')&&actual.ends_with(')'));
}
async fn assert_authority_read(db:&Database,d:&Value) {
    let saved=db.read().await.unwrap();let view=db.read_dispatch_context("dispatch-proposal").await.unwrap();
    assert_eq!(retained_rows(&view),retained_rows(d),"complete ledgers/proofs/order remain independent of prepareRun and job status");
    let full=crate::media_analysis::ledger_from_workspace(d).unwrap();
    assert_eq!(crate::media_analysis::ledger_from_workspace(&view).unwrap(),full,"UNKNOWN attempts roundtrip into actual reducer");
    assert!(full["analyses"].as_array().unwrap().iter().any(|a|a["attempts"].as_array().unwrap().iter().any(|a|a["status"]=="unknown")));
    assert!(rows(&view,"jobs").unwrap().iter().all(|j|j["id"]!="unrelated-paid"));
    let prepare=crate::row(&view,"jobs","referenced-prepare").unwrap();
    assert_eq!(prepare["prepareBundle"],d["jobs"].as_array().unwrap().iter().find(|j|j["id"]=="referenced-prepare").unwrap()["prepareBundle"]);
    assert_eq!(prepare["result"],json!({}));assert!(prepare.get("unrelated").is_none());
    let legacy=crate::row(&view,"jobs","legacy-media").unwrap();
    assert!(legacy["result"].get("paidBody").is_none());assert!(legacy.get("analysis").is_none());
    assert_eq!(db.read().await.unwrap(),saved,"dispatch view is readonly");
}
#[tokio::test]
async fn dispatch_sqlite_retains_full_analysis_unknown_and_applicability_without_paid_history_growth() {
    let folder=tempfile::tempdir().unwrap();let db=Database::Sqlite(crate::open_db(&folder.path().join("analysis-dispatch.sqlite")).await.unwrap());
    let d=authority_workspace();db.change(|w|{*w=d.clone();Ok(())}).await.unwrap();assert_authority_read(&db,&d).await;
    // Keep a foreign carrier visible so the existing company validator can fail
    // closed. Omitting it would turn a corrupt ledger into an apparently valid one.
    let mut foreign=d.clone();foreign["jobs"][0]["account"]=json!("BAW Russia");
    let Database::Sqlite(pool)=&db else {unreachable!()};
    sqlx::query("UPDATE workspace SET payload=? WHERE id=1").bind(foreign.to_string()).execute(pool).await.unwrap();
    let view=db.read_dispatch_context("dispatch-proposal").await.unwrap();
    assert_eq!(retained_rows(&view),retained_rows(&foreign));assert!(crate::media_analysis::ledger_from_workspace(&view).is_err());
    db.close().await;
}

// Integration dependency supplied by Media owner in refresh-r2: the shared real
// CAS fixture has an admitted target proof with its ephemeral cache pin evicted.
// Storage never constructs a second divergent media fixture or fakes a warm pin.
fn real_workspace()->Value {
    let mut d=crate::media_analysis_reuse::test_cold_reused_workspace();
    add_dispatch_target(&mut d,"target","ig:target");add_legacy_jobs(&mut d);normalize(&mut d);d
}
async fn assert_actual_reader_knowledge(db:&Database,d:&Value,proposal_id:&str) {
    let saved=db.read().await.unwrap();let view=db.read_dispatch_context(proposal_id).await.unwrap();
    assert_eq!(retained_rows(&view),retained_rows(d));
    assert_eq!(crate::media_analysis::ledger_from_workspace(&view).unwrap(),crate::media_analysis::ledger_from_workspace(d).unwrap());
    let cold=crate::knowledge::TranscriptLookup::new(&view,AT).unwrap().strict_media_evidence(crate::row(&view,"posts","target").unwrap()).unwrap();
    assert_eq!(cold["audioReady"],false,"retained proof is not accepted before cold CAS verification");
    crate::media_analysis_reuse::warm_workspace(&view).unwrap();
    let target=crate::row(&view,"posts","target").unwrap();
    let strict=crate::knowledge::TranscriptLookup::new(&view,AT).unwrap().strict_media_evidence(target).unwrap();
    assert_eq!(strict["audioReady"],true,"actual dispatch reader supplies current full-audio evidence");
    assert_eq!(strict["screenTextReady"],false);assert_eq!(strict["visualReady"],false);
    let actual=crate::knowledge::select(&view,&[],std::slice::from_ref(target),AT).unwrap();
    let expected=crate::knowledge::select(d,&[],std::slice::from_ref(crate::row(d,"posts","target").unwrap()),AT).unwrap();
    assert_eq!(actual,expected,"actual reader→knowledge exact-file proof selection parity");
    assert!(actual["materials"].as_array().unwrap().iter().any(|m|m["postKey"]=="vk:donor" && m["exactFileAnalysisReuse"][0]["targetPostId"]=="target"));
    assert_eq!(db.read().await.unwrap(),saved,"consumer proof checks never mutate retained jobs");
    let mut changed=view.clone();crate::row_mut(&mut changed,"posts","target").unwrap()["title"]=json!("new target metadata");
    let strict=crate::knowledge::TranscriptLookup::new(&changed,AT).unwrap().strict_media_evidence(crate::row(&changed,"posts","target").unwrap()).unwrap();
    assert_eq!(strict["audioReady"],false,"full retained proof does not authorize a changed alias");
}
#[tokio::test]
async fn dispatch_sqlite_real_cas_reader_to_knowledge_preserves_exact_audio_and_rejects_changed_alias() {
    let folder=tempfile::tempdir().unwrap();let db=Database::Sqlite(crate::open_db(&folder.path().join("analysis-cas-dispatch.sqlite")).await.unwrap());
    let d=real_workspace();db.change(|w|{*w=d.clone();Ok(())}).await.unwrap();assert_actual_reader_knowledge(&db,&d,"dispatch-proposal").await;db.close().await;
}
#[tokio::test]
#[ignore="ROOT only: pristine explicit isolated communityhero_writer_v51_test_ PostgreSQL fixture"]
async fn dispatch_postgres_full_analysis_and_actual_reader_to_knowledge_parity() {
    let db=crate::storage::preparation::writer_v51_fixture_db().await;
    let d=authority_workspace();db.change(|w|{*w=d.clone();Ok(())}).await.unwrap();assert_authority_read(&db,&d).await;
    let Database::Postgres{writer,..}=&db else {unreachable!()};
    for value in [json!({}),json!({"kind":null}),json!({"kind":true}),json!({"kind":[]}),
        json!({"kind":"media_analysis","analysis":{"attempts":[{"status":"unknown"}]}}),
        json!({"kind":"media_analysis_applicability","status":"completed","result":{"proof":{"future":"full"}}}),
        json!({"kind":"assistant","result":{"private":"omit"}})] {
        let sql=format!("SELECT COALESCE({},false) AS selected,({})::text AS projected",
            retained_analysis_job_sql("$1::jsonb",true),compact_payload_sql("jobs","$1::jsonb",true,0));
        let row=sqlx::query(sqlx::AssertSqlSafe(sql.as_str())).bind(value.to_string()).fetch_one(writer).await.unwrap();
        assert_eq!(row.try_get::<bool,_>("selected").unwrap(),retained_analysis_job(&value));
        assert_eq!(parse(row.try_get::<&str,_>("projected").unwrap()).unwrap(),compact_context_record(&value,"jobs"));
    }
    // Actual table-backed reader, with a committed corruption visible to its
    // separate readonly snapshot. Restore first, then assert the captured error,
    // so each fault is isolated and never becomes the next case's preimage.
    let saved=db.read().await.unwrap();
    let carrier=rows(&saved,"jobs").unwrap().iter().find(|j|j["kind"]=="media_analysis").unwrap();
    let carrier_id=carrier["id"].as_str().unwrap();
    for fault in ["payload_kind","physical_kind"] {
        if fault=="payload_kind" {
            // The old payload-only filter omitted this unrelated ledger entirely.
            sqlx::query("UPDATE communityhero.jobs SET payload=jsonb_set(payload,'{kind}','\"assistant\"'::jsonb) WHERE workspace_id=$1 AND id=$2")
                .bind(WORKSPACE).bind(carrier_id).execute(writer).await.unwrap();
        } else {
            sqlx::query("UPDATE communityhero.jobs SET kind='assistant' WHERE workspace_id=$1 AND id=$2")
                .bind(WORKSPACE).bind(carrier_id).execute(writer).await.unwrap();
        }
        let observed=db.read_dispatch_context("dispatch-proposal").await;
        sqlx::query("UPDATE communityhero.jobs SET kind=$3,payload=$4::jsonb WHERE workspace_id=$1 AND id=$2")
            .bind(WORKSPACE).bind(carrier_id).bind(carrier["kind"].as_str()).bind(carrier.to_string()).execute(writer).await.unwrap();
        let error=observed.unwrap_err();assert_eq!(error.1,"Record relational projection mismatch","corruption must fail closed: {fault}");
        assert_eq!(db.read().await.unwrap(),saved,"exact authority row restored after {fault}");
    }
    // Foreign workspace jobs, even with the same kind/id, remain outside the
    // actual jobs SQL selection. No company credential or live database is used.
    sqlx::query("INSERT INTO communityhero.migration_imports(id,source_sha256,payload) VALUES('analysis-foreign',$1,'{}')").bind("2".repeat(64)).execute(writer).await.unwrap();
    sqlx::query("INSERT INTO communityhero.workspaces(id,account,import_id,metadata) VALUES('analysis-foreign','BAW Russia','analysis-foreign','{}')").execute(writer).await.unwrap();
    sqlx::query("INSERT INTO communityhero.jobs(workspace_id,id,ordinal,kind,status,payload) VALUES('analysis-foreign','foreign-analysis',0,'media_analysis','unknown','{\"id\":\"foreign-analysis\",\"kind\":\"media_analysis\",\"status\":\"unknown\",\"account\":\"BAW Russia\"}')").execute(writer).await.unwrap();
    let view=db.read_dispatch_context("dispatch-proposal").await.unwrap();assert!(rows(&view,"jobs").unwrap().iter().all(|j|j["id"]!="foreign-analysis"));
    // Append the real-CAS scenario without deleting, reordering or retargeting
    // any structural scenario record. Shared legacy jobs must be exact repeats.
    let mut real=real_workspace();
    real["branches"][0]["id"]=json!("real-dispatch-branch");
    real["items"][0]["id"]=json!("real-dispatch-item");real["items"][0]["branchId"]=json!("real-dispatch-branch");
    real["proposals"][0]["id"]=json!("real-dispatch-proposal");real["proposals"][0]["itemId"]=json!("real-dispatch-item");
    let before_append=db.read().await.unwrap();let mut combined=before_append.clone();
    for key in ["posts","branches","items","proposals","jobs","materials","knowledge_entries","knowledge_versions"] {
        for row in real[key].as_array().unwrap() {
            if let Some(existing)=combined[key].as_array().unwrap().iter().find(|old|old["id"]==row["id"]) {
                assert_eq!(existing,row,"fixture ID collision must not change existing history: {key}");
            } else {combined[key].as_array_mut().unwrap().push(row.clone());}
        }
        assert!(combined[key].as_array().unwrap().starts_with(before_append[key].as_array().unwrap()),"existing ordered prefix retained: {key}");
    }
    db.change(|w|{*w=combined.clone();Ok(())}).await.unwrap();
    let appended=db.read().await.unwrap();
    for key in ["posts","branches","items","proposals","jobs","materials","knowledge_entries","knowledge_versions"] {
        assert!(appended[key].as_array().unwrap().starts_with(before_append[key].as_array().unwrap()),"persisted ordered prefix retained: {key}");
    }
    assert_actual_reader_knowledge(&db,&combined,"real-dispatch-proposal").await;db.close().await;
}
