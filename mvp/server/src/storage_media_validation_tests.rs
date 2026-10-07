use super::*;

fn document(cold:usize)->Value {
    let mut d=crate::media_analysis::test_workspace_fixture("LikeAvto");
    d["connectorBinding"]=crate::accounts::Profile::LikeAvto.binding();
    d["runtimeLifecycle"]=json!({"exact":"kept by projection"});
    d["posts"]=json!([{"id":"post","postKey":"target","text":"source bytes"},{"id":"cold-post","text":"unrelated"}]);
    d["knowledge_entries"]=json!([]);d["knowledge_versions"]=json!([]);
    crate::list_mut(&mut d,"jobs").push(json!({"id":"execution","kind":"media_audio","status":"running","audioPin":{"progress":{"schemaVersion":2,"opaque":"exact"}}}));
    for index in 0..cold {crate::list_mut(&mut d,"jobs").push(json!({"id":format!("cold-{index}"),"kind":"assistant","status":"completed","request":{"history":"x".repeat(4096)},"result":{"history":"y".repeat(4096)}}));}
    d["operations"]=json!([{"id":"uncertain","status":"unknown","evidence":{"original":"paid"}}]);
    d
}
fn scope(mode:MediaValidationMode)->MediaValidationScope<'static> {MediaValidationScope{job:"execution",post:"post",mode}}
#[test]
fn scoped_read_keeps_audio_job_and_all_ledger_but_not_unrelated_history() {
    let d=document(100);let before=d.clone();
    let execution=scope_projection(&d,scope(MediaValidationMode::Execution)).unwrap();
    assert_eq!(execution["jobs"],json!([crate::row(&d,"jobs","execution").unwrap()]));
    assert_eq!(execution["posts"],json!([crate::row(&d,"posts","post").unwrap()]));
    assert_eq!(execution.get("runtimeLifecycle"),d.get("runtimeLifecycle"));
    for absent in ["operations","items","materials","knowledge_versions"] {assert!(execution.get(absent).is_none());}
    let reuse=scope_projection(&d,scope(MediaValidationMode::Reuse{legacy_catalog:false})).unwrap();
    assert_eq!(crate::media_analysis::ledger_from_workspace(&reuse).unwrap(),crate::media_analysis::ledger_from_workspace(&d).unwrap());
    assert_eq!(reuse["jobs"].as_array().unwrap().len(),3,"two full ledger carriers plus execution");
    assert!(reuse.get("knowledge_entries").is_none());
    let legacy=scope_projection(&d,scope(MediaValidationMode::Reuse{legacy_catalog:true})).unwrap();
    assert_eq!(legacy["knowledge_entries"],d["knowledge_entries"]);assert_eq!(legacy["knowledge_versions"],d["knowledge_versions"]);
    assert_eq!(d,before);
}
#[test]
fn selected_duplicate_invalid_type_and_unrelated_bad_carrier_are_not_hidden() {
    let d=document(0);let s=scope(MediaValidationMode::Reuse{legacy_catalog:false});
    let mut bad=d.clone();let duplicate=crate::row(&bad,"jobs","execution").unwrap().clone();crate::list_mut(&mut bad,"jobs").push(duplicate);
    assert!(scope_projection(&bad,s).is_err());
    let mut bad=d.clone();crate::row_mut(&mut bad,"jobs","execution").unwrap()["refId"]=json!(17);assert!(scope_projection(&bad,s).is_err());
    let mut bad=d.clone();bad["jobs"][0]["status"]=json!("running");assert!(scope_projection(&bad,s).is_err());
    for mode in 0..3 {
        let mut bad=d.clone();match mode {0=>bad["jobs"][0]["account"]=json!("BAW Russia"),1=>bad["jobs"][0]["analysis"]["results"][0]["resultSha256"]=json!("0".repeat(64)),_=>bad["jobs"][1]["analysis"]["attempts"][0]["status"]=json!("invented")};
        let projected=scope_projection(&bad,s).unwrap();
        assert!(crate::media_analysis::ledger_from_workspace(&bad).is_err());
        assert!(crate::media_analysis::ledger_from_workspace(&projected).is_err());
    }
    let mut bad=d.clone();bad["knowledge_versions"]=json!([{"id":"bad-unrelated-version","hash":"forged"}]);
    let projected=scope_projection(&bad,scope(MediaValidationMode::Reuse{legacy_catalog:true})).unwrap();
    assert!(crate::knowledge::validate_catalog(&bad).is_err());assert!(crate::knowledge::validate_catalog(&projected).is_err());
}
#[test]
fn retained_assistant_volume_does_not_expand_media_validation_projection() {
    let small=document(0);let large=document(2000);
    for mode in [MediaValidationMode::Execution,MediaValidationMode::Reuse{legacy_catalog:false},MediaValidationMode::Reuse{legacy_catalog:true}] {
        let a=scope_projection(&small,scope(mode)).unwrap();let b=scope_projection(&large,scope(mode)).unwrap();
        assert_eq!(a,b);assert_eq!(a.to_string().len(),b.to_string().len());
    }
    assert!(large.to_string().len()>16_000_000);
}

#[tokio::test]
#[ignore="ROOT-only fresh isolated writer_v51 PostgreSQL fixture; run alone"]
async fn postgres_validation_uses_target_or_payload_identity_and_rejects_relational_damage() {
    let db=crate::storage::writer_v51_fixture_db().await;
    db.change(|d|{d["posts"]=json!([{"id":"post","postKey":"target"}]);d["jobs"]=json!([
        {"id":"execution","kind":"media_audio","status":"running","audioPin":{"progress":{"schemaVersion":2}}},
        {"id":"other","kind":"assistant","status":"completed","result":{"cold":"not loaded"}}]);Ok(())}).await.unwrap();
    let before=db.read().await.unwrap();
    let observed=db.check_media_validation(scope(MediaValidationMode::Execution),|view|Ok(view.clone())).await.unwrap();
    assert_eq!(observed,scope_projection(&before,scope(MediaValidationMode::Execution)).unwrap());
    assert_eq!(db.read().await.unwrap(),before);
    let Database::Postgres{writer,..}=&db else {unreachable!()};
    sqlx::query("UPDATE communityhero.jobs SET status='failed' WHERE workspace_id=$1 AND id='execution'").bind(WORKSPACE).execute(writer).await.unwrap();
    assert!(db.check_media_validation(scope(MediaValidationMode::Execution),|_|Ok(())).await.is_err());
    sqlx::query("UPDATE communityhero.jobs SET status='running' WHERE workspace_id=$1 AND id='execution'").bind(WORKSPACE).execute(writer).await.unwrap();
    // A row claiming the requested payload identity must not be filtered out.
    sqlx::query("UPDATE communityhero.jobs SET payload=jsonb_set(payload,'{id}','\"execution\"'::jsonb) WHERE workspace_id=$1 AND id='other'").bind(WORKSPACE).execute(writer).await.unwrap();
    assert!(db.check_media_validation(scope(MediaValidationMode::Execution),|_|Ok(())).await.is_err());
    sqlx::query("UPDATE communityhero.jobs SET payload=jsonb_set(payload,'{id}','\"other\"'::jsonb),kind='media_analysis' WHERE workspace_id=$1 AND id='other'").bind(WORKSPACE).execute(writer).await.unwrap();
    assert!(db.check_media_validation(scope(MediaValidationMode::Reuse{legacy_catalog:false}),|_|Ok(())).await.is_err());
    sqlx::query("UPDATE communityhero.jobs SET kind='assistant' WHERE workspace_id=$1 AND id='other'").bind(WORKSPACE).execute(writer).await.unwrap();
    assert_eq!(db.read().await.unwrap(),before);
    db.close().await;
}

#[test]
#[ignore="offline synthetic timings only; no wall-time assertion"]
fn benchmark_old_full_clone_and_media_validation_projection() {
    let d=document(2000);let start=std::time::Instant::now();let old=d.clone();let clone_us=start.elapsed().as_micros();
    let start=std::time::Instant::now();let view=scope_projection(&d,scope(MediaValidationMode::Reuse{legacy_catalog:false})).unwrap();let scope_us=start.elapsed().as_micros();
    assert_eq!(crate::media_analysis::ledger_from_workspace(&old).unwrap(),crate::media_analysis::ledger_from_workspace(&view).unwrap());
    println!("media_validation_synthetic retained_jobs=2000 old_bytes={} selected_bytes={} old_clone_us={} selected_projection_us={}",old.to_string().len(),view.to_string().len(),clone_us,scope_us);
}
