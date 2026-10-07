use super::*;
use sha2::{Digest,Sha256};

fn result() -> Value {
    json!({"text":"Reviewed","sources":[],"assessments":[{"itemId":"i","outcome":"close","reason":"No reply needed","tags":[]}],
        "proposals":[{"itemId":"i","kind":"close","text":""}],
        "runMetadata":{"schemaVersion":1,"model":"fixture","reasoningEffort":"medium","promptVersion":"v1",
            "instructionSha256":"a".repeat(64),"inputSha256":"b".repeat(64),"cliSha256":"c".repeat(64),
            "elapsedMs":1,"completedAt":"2026-09-27T00:00:00Z",
            "research":{"version":1,"status":"no_sources","model":"fixture","reasoningEffort":"medium",
                "instructionSha256":"d".repeat(64),"inputSha256":"e".repeat(64),"elapsedMs":1,"webCalls":0,
                "sources":[],"completedAt":"2026-09-27T00:00:00Z"}}})
}

fn fixture() -> (Value, String) {
    let mut d = crate::empty();
    normalize(&mut d);
    crate::accounts::initialize(&mut d, crate::accounts::Profile::LikeAvto).unwrap();
    d["items"] = json!([{"id":"i","itemId":"c","objectId":"o","platform":"VK","postKey":"p",
        "conversationKey":"thread","branchId":"b","postId":"post","revision":1,"draft":"",
        "workflow":"attention","providerStatus":"new","providerObservedAt":crate::now()}]);
    d["branches"] = json!([{"id":"b","postId":"post","messages":[{"id":"c","text":"Thank you"}],"contextComplete":true}]);
    d["posts"] = json!([{"id":"post","postKey":"p","text":"Post"}]);
    let job = crate::engine_prepare::schedule(&mut d, crate::engine_prepare::Input {
        item_ids: vec!["i".into()], instruction: None,
    }).unwrap().job_id;
    // This fixture exercises the already-paid V71 full-batch contract. New
    // jobs carry a captured group plan and use incremental admission below.
    let saved=crate::row_mut(&mut d,"jobs",&job).unwrap();
    // V71 predates durable scope captures. Do not keep a new immutable capture
    // while deliberately rewriting its request into this historical fixture.
    saved.as_object_mut().unwrap().remove("scopeReservation");
    saved["preparationStages"].as_object_mut().unwrap().remove("groupAdmission");
    let bundle=&mut saved["prepareBundle"];
    // Genuine V71 capture predates the material/group/visual selectors. This
    // test-only conversion precedes persistence and any paid result; it must
    // never be applied to a newly saved mandatory-material capture.
    let request=bundle["request"].as_object_mut().unwrap();
    for field in ["preparationMode","responseContract","modelContextContract",
        "researchPolicy","researchLimitContract","recoveryEvidenceContract",
        "factDependencyContract","visualNeedContract","visualSelection",
        "strictGroupContract","strictGroup","mandatoryMaterialContract",
        "postContextBundle","materialReadiness"] {request.remove(field);}
    bundle["digest"]=json!(format!("{:x}",Sha256::digest(bundle["request"].to_string().as_bytes())));
    crate::preparation_review::record_first(&mut d, &job, &result(), "2026-09-27T00:00:00Z").unwrap();
    d["conversations"] = json!([{"id":"private","messages":[{"text":"unrelated".repeat(20_000)}]}]);
    d["approvals"] = json!([{"id":"approval-old","status":"approved","proposals":[],"authority":"unchanged"}]);
    d["audit"] = json!([{"id":"audit-old","action":"other","refId":"i"}]);
    d["feedback"] = json!([{"id":"feedback-old","itemId":"i"}]);
    crate::list_mut(&mut d, "jobs").push(json!({"id":"old-job","kind":"assistant","purpose":"discussion",
        "status":"completed","result":{"text":"unrelated".repeat(20_000)}}));
    crate::list_mut(&mut d, "jobs").push(json!({"id":"audio-evidence","kind":"media_audio",
        "refId":"another-post","status":"running"}));
    (d, job)
}

fn admit(d: &mut Value, job: &str) -> ApiResult<Value> {
    let bundle = crate::row(d, "jobs", job)?["prepareBundle"].clone();
    crate::prepare_bundle::current(d, &bundle).map_err(crate::conflict)?;
    crate::preparation_review::chunks::current(d, crate::row(d,"jobs",job)?)?;
    let outcome = crate::prepare_bundle::admit_to(d, job, None, &result())?;
    if outcome["candidates"].as_array().unwrap().iter().any(|r|r["status"] != "review") {
        return Err(crate::conflict("Admission failed; rollback entire batch"));
    }
    crate::preparation_review::record_review(d, job, Ok(&result()), "2026-09-27T00:00:00Z")?;
    Ok(outcome)
}

fn normalize_generated(v: &mut Value) {
    match v {
        Value::Array(a) => a.iter_mut().for_each(normalize_generated),
        Value::Object(o) => {
            for (key, value) in o.iter_mut() {
                if key == "createdAt" { *value = json!("<time>"); }
                else { normalize_generated(value); }
            }
        }
        Value::String(s) if uuid::Uuid::parse_str(s).is_ok() => *s = "<id>".into(),
        _ => (),
    }
}

#[test]
fn incremental_group_admission_is_append_only_and_does_not_finalize_job(){
    let (mut original,job)=fixture();
    let bundle=crate::row(&original,"jobs",&job).unwrap()["prepareBundle"].clone();
    let groups=crate::prepare_bundle::capture_groups(&original,&bundle).unwrap();
    crate::row_mut(&mut original,"jobs",&job).unwrap()["preparationStages"]["groupAdmission"]=groups;
    let before=projection_for(&original,Some(&job)).unwrap();
    let mut after=before.clone();
    let group=crate::row(&after,"jobs",&job).unwrap()["preparationStages"]["groupAdmission"][0].clone();
    let admission=crate::prepare_bundle::admit_group(&mut after,&job,&result(),&group).unwrap();
    let saved=&mut crate::row_mut(&mut after,"jobs",&job).unwrap()["preparationStages"]["groupAdmission"][0];
    saved["status"]=json!("admitted");saved["admission"]=admission;
    validate_admission(&before,&after,&job).unwrap();
    assert!(crate::row(&after,"jobs",&job).unwrap()["prepareOutcome"].is_null());
    assert_eq!(after["proposals"].as_array().unwrap().len(),1);
    let mut forged=after.clone();
    crate::row_mut(&mut forged,"jobs",&job).unwrap()["preparationStages"]["groupAdmission"][0]["fingerprints"]["i"]=json!("0".repeat(64));
    assert!(validate_admission(&before,&forged,&job).is_err());
}

#[test]
fn final_admission_projection_matches_full_transaction_and_omits_private_history() {
    let (original, job) = fixture();
    let before = projection_for(&original, Some(&job)).unwrap();
    for table in ["approvals", "audit", "feedback", "conversations"] {
        assert!(before.get(table).is_none(), "{table}");
    }
    assert!(crate::row(&before, "jobs", "old-job").is_err());
    assert_eq!(crate::row(&before,"jobs","audio-evidence").unwrap()["status"], "running");
    assert!(before.to_string().len() * 10 < original.to_string().len());
    let mut full = original.clone();
    admit(&mut full, &job).unwrap();
    let mut after = before.clone();
    admit(&mut after, &job).unwrap();
    validate_admission(&before, &after, &job).unwrap();
    let proposal = &after["proposals"][0];
    assert_eq!(proposal["sourceContextDigest"], proposal["reviewContextDigest"]);
    assert_eq!(proposal["sourceContextDigest"], crate::prepare_bundle::review_fingerprint(&before, "i").unwrap());
    let mut merged = original;
    merge_claim_delta(&mut merged, &before, &after).unwrap();
    merged["preparationResearch"] = after["preparationResearch"].clone();
    normalize_generated(&mut merged);
    normalize_generated(&mut full);
    assert_eq!(merged, full);
}

#[test]
fn final_admission_scope_rejects_authority_history_and_source_changes() {
    let (original, job) = fixture();
    let before = projection_for(&original, Some(&job)).unwrap();
    let mut good = before.clone();
    admit(&mut good, &job).unwrap();
    for mutation in ["approval", "audit", "feedback", "source", "operation", "draft", "route",
        "first_pass", "job_status", "proposal_status", "archive_removed", "archive_altered", "review_removed"] {
        let mut changed = good.clone();
        match mutation {
            "approval" => changed["approvals"] = json!([{"id":"illegal"}]),
            "audit" => changed["audit"] = json!([]),
            "feedback" => changed["feedback"] = json!([]),
            "source" => changed["branches"][0]["messages"][0]["text"] = json!("Changed"),
            "operation" => changed["operations"] = json!([{"id":"new-operation","status":"dispatching"}]),
            "draft" => changed["items"][0]["draft"] = json!("Unapproved text"),
            "route" => changed["items"][0]["objectId"] = json!("Other company"),
            "first_pass" => crate::row_mut(&mut changed,"jobs",&job).unwrap()["preparationStages"]["first"]["result"] = json!({}),
            "job_status" => crate::row_mut(&mut changed,"jobs",&job).unwrap()["status"] = json!("completed"),
            "proposal_status" => changed["proposals"][0]["status"] = json!("approved"),
            "archive_removed" => changed["preparationResearch"] = json!([]),
            "archive_altered" => changed["preparationResearch"][0]["account"] = json!("BAW Russia"),
            _ => crate::row_mut(&mut changed,"jobs",&job).unwrap()["preparationStages"]["review"] = Value::Null,
        }
        assert!(validate_admission(&before,&changed,&job).is_err(), "{mutation}");
    }
}

#[tokio::test]
async fn sqlite_final_admission_rolls_back_proposals_review_archive_and_preserves_unknown() {
    let (mut initial, job) = fixture();
    initial["operations"] = json!([{"id":"unknown-old","status":"unknown","evidence":{"receipt":"must survive"}}]);
    let folder = tempfile::tempdir().unwrap();
    let path = folder.path().join("workspace.sqlite");
    let db = Database::Sqlite(crate::open_db(&path).await.unwrap());
    db.change(|d| { *d = initial; Ok(()) }).await.unwrap();
    let before = db.read().await.unwrap();
    let late_error = db.change_preparation_admission_observed(&job, |d| {
        admit(d, &job)?;
        Err::<(),_>(crate::conflict("Late failure after review archive"))
    }).await.unwrap_err();
    assert_eq!(late_error.1, "Late failure after review archive");
    assert_eq!(db.read().await.unwrap(), before);
    let scope_error = db.change_preparation_admission_observed(&job, |d| {
        admit(d, &job)?;
        d["items"][0]["draft"] = json!("Forbidden mutation");
        Ok(())
    }).await.unwrap_err();
    assert!(scope_error.1.contains("protected item state"), "{}", scope_error.1);
    assert_eq!(db.read().await.unwrap(), before);
    let (_, changed) = db.change_preparation_admission_observed(&job, |d| admit(d,&job)).await.unwrap();
    assert!(changed);
    let saved = db.read().await.unwrap();
    for table in ["operations", "approvals", "audit", "feedback", "conversations"] {
        assert_eq!(saved[table], before[table], "{table}");
    }
    assert_eq!(saved["proposals"].as_array().unwrap().len(), 1);
    assert_eq!(saved["preparationResearch"].as_array().unwrap().len(), 1);
    assert!(db.change_preparation_admission_observed(&job, |d| admit(d,&job)).await.is_err());
    assert_eq!(db.read().await.unwrap(), saved);
    db.close().await;
    let reopened = Database::Sqlite(crate::open_db(&path).await.unwrap());
    assert_eq!(reopened.read().await.unwrap(), saved);
    reopened.close().await;
}

#[tokio::test]
async fn sqlite_final_admission_rechecks_changed_source_inside_transaction() {
    let (initial, job) = fixture();
    let folder = tempfile::tempdir().unwrap();
    let db = Database::Sqlite(crate::open_db(&folder.path().join("workspace.sqlite")).await.unwrap());
    db.change(|d| { *d=initial; Ok(()) }).await.unwrap();
    db.change(|d| { d["branches"][0]["messages"][0]["text"] = json!("Changed after model"); Ok(()) }).await.unwrap();
    let before = db.read().await.unwrap();
    let error = db.change_preparation_admission_observed(&job, |d| admit(d,&job)).await.unwrap_err();
    assert!(error.1.contains("Preparation evidence changed"), "{}", error.1);
    assert_eq!(db.read().await.unwrap(), before);
    db.close().await;
}

#[tokio::test]
async fn sqlite_final_admission_sees_protected_operations_before_admitting() {
    let (initial, job) = fixture();
    let folder = tempfile::tempdir().unwrap();
    let db = Database::Sqlite(crate::open_db(&folder.path().join("workspace.sqlite")).await.unwrap());
    db.change(|d| { *d=initial; Ok(()) }).await.unwrap();
    for status in ["unknown", "dispatching", "succeeded"] {
        db.change(|d| { d["operations"] = json!([{"id":"protected","itemId":"i","status":status}]); Ok(()) }).await.unwrap();
        let before = db.read().await.unwrap();
        let error = db.change_preparation_admission_observed(&job, |d| admit(d,&job)).await.unwrap_err();
        assert!(error.1.contains("protected operation"), "{status}: {}", error.1);
        assert_eq!(db.read().await.unwrap(), before);
    }
    db.close().await;
}

#[tokio::test]
#[ignore = "requires a pristine explicitly isolated communityhero_writer_v51_test_ PostgreSQL fixture"]
async fn postgres_final_admission_parity_and_late_sql_rollback() {
    let db = writer_v51_fixture_db().await;
    let (initial,job) = fixture();
    db.change(|d| { *d=initial; Ok(()) }).await.unwrap();
    let baseline = db.read().await.unwrap();
    // Fail the final metadata write, after item/proposal/job SQL has executed.
    // DDL is confined to the guarded disposable fixture; no provider is called.
    let Database::Postgres { writer,.. } = &db else { unreachable!() };
    sqlx::query("CREATE FUNCTION pg_temp.writer_v51_reject() RETURNS trigger LANGUAGE plpgsql AS 'BEGIN RAISE EXCEPTION ''synthetic final write rejection''; END;'")
        .execute(writer).await.unwrap();
    sqlx::query("CREATE TRIGGER writer_v51_reject BEFORE UPDATE ON communityhero.workspaces FOR EACH ROW EXECUTE FUNCTION pg_temp.writer_v51_reject()")
        .execute(writer).await.unwrap();
    let failed = db.change_preparation_admission_observed(&job,|d|admit(d,&job)).await;
    sqlx::query("DROP TRIGGER writer_v51_reject ON communityhero.workspaces").execute(writer).await.unwrap();
    assert!(failed.is_err());
    assert_eq!(db.read().await.unwrap(),baseline,"late SQL failure must roll back all prior writes");
    let mut expected = baseline.clone();
    admit(&mut expected,&job).unwrap();
    let started = std::time::Instant::now();
    let (_,changed) = db.change_preparation_admission_observed(&job,|d| {
        assert_eq!(*d,projection_for(&baseline,Some(&job)).unwrap(),"SQL projection parity");
        admit(d,&job)
    }).await.unwrap();
    assert!(changed);
    let elapsed = started.elapsed().as_secs_f64()*1000.0;
    let mut actual = db.read().await.unwrap();
    normalize_generated(&mut expected); normalize_generated(&mut actual);
    assert_eq!(actual,expected,"full-domain persisted parity");
    eprintln!("WRITER_V51_PG admission=true lateSqlRollback=true fullDomainParity=true scopedMs={elapsed:.2}");
    db.close().await;
}
