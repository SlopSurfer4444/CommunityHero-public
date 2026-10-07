use super::*;
use serde_json::json;
use sha2::{Digest,Sha256};

fn item(id: &str, author: &str, platform: &str, account: Option<&str>) -> Value {
    let mut item = json!({
        "id": id,
        "itemId": format!("comment-{id}"),
        "objectId": "11341",
        "postKey": format!("11341:post-{id}"),
        "conversationKey": format!("11341:thread-{id}"),
        "postId": format!("post-{id}"),
        "branchId": format!("branch-{id}"),
        "authorId": author,
        "author": "Customer",
        "platform": platform,
        "text": format!("Customer statement at {id}"),
        "createdAt": "2020-01-02T10:00:00Z",
        "providerObservedAt": "2020-01-02T10:00:00Z",
        "providerStatus": "new",
        "workflow": "attention",
        "revision": 1,
        "draft": "",
        "connectorBinding": crate::legacy_binding()
    });
    if let Some(account) = account { item["account"] = json!(account); }
    item
}

fn add_comment(d: &mut Value, id: &str, author: &str, platform: &str, account: Option<&str>) {
    crate::list_mut(d, "items").push(item(id, author, platform, account));
    crate::list_mut(d, "posts").push(json!({
        "id": format!("post-{id}"), "postKey": format!("11341:post-{id}"),
        "text": format!("Publication {id}"), "platform": platform
    }));
    crate::list_mut(d, "branches").push(json!({
        "id": format!("branch-{id}"), "postId": format!("post-{id}"),
        "contextComplete": true,
        "messages": [{"id": format!("comment-{id}"), "authorId": author,
            "role": "participant", "text": format!("Customer statement at {id}"),
            "createdAt": "2020-01-02T10:00:00Z"}]
    }));
}

fn fixture() -> Value {
    let mut d = crate::empty();
    normalize(&mut d);
    d["connectorBinding"] = crate::legacy_binding();
    add_comment(&mut d, "selected", "author-1", "VK", None);
    add_comment(&mut d, "prior", "author-1", "VK", None);
    let branch = crate::row_mut(&mut d, "branches", "branch-prior").unwrap();
    branch["messages"].as_array_mut().unwrap().push(json!({
        "id": "official-reply", "providerItemId": "provider-reply",
        "providerObjectId": "11341", "authorId": "brand-1", "role": "brand",
        "providerOfficial": true, "roleEvidence": "provider-official",
        "replyToProviderItemId": "comment-prior",
        "text": "Пришлите, пожалуйста, номер договора.",
        "createdAt": "2020-01-03T10:00:00Z"
    }));
    add_comment(&mut d, "other-platform", "author-1", "OK", None);
    add_comment(&mut d, "other-company", "author-1", "VK", Some("BAW"));
    add_comment(&mut d, "new-author", "author-2", "VK", None);
    d["operations"] = json!([
        {"id":"unrelated-unknown","itemId":"other-platform","status":"unknown","evidence":{"receipt":"unrelated"}}
    ]);
    d["proposals"] = json!([
        {"id":"selected-draft","itemId":"selected","status":"draft"},
        {"id":"unrelated-draft","itemId":"other-platform","status":"draft"}
    ]);
    d["jobs"] = json!([
        {"id":"media","kind":"media","status":"completed"},
        {"id":"active","kind":"assistant","status":"queued"},
        {"id":"finished","kind":"assistant","status":"completed","result":{"history":"irrelevant"}}
    ]);
    d["materials"] = json!([{"id":"global-material","kind":"policy","text":"Catalog source"}]);
    d["approvals"] = json!([{"id":"historic-approval","status":"approved"}]);
    d
}

fn scheduled() -> (Value, String) {
    let mut d = fixture();
    let input = crate::engine_prepare::Input { item_ids: vec!["selected".into()], instruction: None };
    crate::engine_prepare::schedule(&mut d, input).unwrap();
    let job = d["jobs"].as_array().unwrap().last().unwrap();
    assert_eq!(job["selectedItemIds"], json!(["selected"]));
    assert_eq!(job["prepareBundle"]["itemIds"], json!(["selected"]));
    let job_id = job["id"].as_str().unwrap().to_owned();
    // A newly uncertain operation after scheduling must remain visible to the
    // scoped reader and final admission. Pre-existing UNKNOWN correctly holds
    // this recipient before a model job can be scheduled.
    crate::list_mut(&mut d, "operations").insert(0,json!({
        "id":"selected-unknown","itemId":"selected","status":"unknown",
        "evidence":{"receipt":"uncertain"}
    }));
    (d, job_id)
}

#[test]
fn metadata_initialization_does_not_project_entities_or_drop_paid_controls() {
    let (full,job_id)=scheduled();
    let stored_metadata=metadata(&full);
    assert!(stored_metadata.get("jobs").is_none());
    assert!(stored_metadata.get("proposals").is_none());
    let initialized=claim_metadata(&stored_metadata).unwrap();
    assert!(initialized.get("scopeOwners").is_none(),"PG loads controls after entity arrays");
    let projected=schedule_projection(&full).unwrap();
    assert!(projected["scopeOwners"].as_array().unwrap().iter().any(|j|j["id"]==job_id),
        "pure full-workspace projection must retain actual paid owner");
}

fn ids(value: &Value, table: &str) -> Vec<String> {
    crate::list(value, table).iter()
        .map(|row| row["id"].as_str().unwrap().to_owned()).collect()
}

#[test]
fn selected_run_keeps_exact_author_history_and_shared_catalog() {
    let (full, job_id) = scheduled();
    let scoped = projection_for(&full, Some(&job_id)).unwrap();
    assert_eq!(ids(&scoped, "items"), vec!["selected", "prior"]);
    assert_eq!(ids(&scoped, "branches"), vec!["branch-selected", "branch-prior"]);
    assert_eq!(ids(&scoped, "proposals"), vec!["selected-draft"]);
    assert_eq!(ids(&scoped, "operations"), vec!["selected-unknown", "unrelated-unknown"],
        "all operation aliases are required for reservation currentness");
    assert_eq!(scoped["operations"][0], full["operations"][0], "UNKNOWN evidence must stay intact");
    for table in ["posts", "materials", "knowledge_entries", "knowledge_versions"] {
        assert_eq!(scoped[table], full[table], "{table} must remain complete");
    }
    assert!(ids(&scoped, "jobs").contains(&job_id));
    assert!(ids(&scoped, "jobs").contains(&"media".to_owned()));
    assert!(ids(&scoped, "jobs").contains(&"active".to_owned()));
    assert!(!ids(&scoped, "jobs").contains(&"finished".to_owned()));
    assert!(scoped.get("approvals").is_none(), "approval history is outside this projection");
    let bundle = &crate::row(&full, "jobs", &job_id).unwrap()["prepareBundle"];
    assert_eq!(crate::prepare_bundle::current(&full, bundle), crate::prepare_bundle::current(&scoped, bundle));
    assert!(crate::prepare_bundle::current(&scoped, bundle).is_ok());
    let full_job = crate::row(&full, "jobs", &job_id).unwrap();
    let scoped_job = crate::row(&scoped, "jobs", &job_id).unwrap();
    assert!(crate::preparation_review::chunks::current(&full, full_job).is_err(), "selected UNKNOWN operation protects the recipient");
    assert!(crate::preparation_review::chunks::current(&scoped, scoped_job).is_err(), "scoping must preserve the same UNKNOWN guard");
    let cases = &bundle["request"]["customerCases"];
    assert_eq!(cases[0]["messages"][0]["itemId"], "prior");
    assert_eq!(cases[0]["brandReplies"][0]["sourceItemId"], "prior");
    assert_eq!(cases[0]["priorContractRequests"][0]["sourceItemId"], "prior");
}

#[test]
fn unrelated_history_shrinks_without_truncating_current_author_history() {
    let (mut full, job_id) = scheduled();
    for n in 0..1195 {
        let id = format!("unrelated-{n}");
        add_comment(&mut full, &id, &format!("other-author-{n}"), "VK", None);
        crate::row_mut(&mut full, "items", &id).unwrap()["text"] = json!("x".repeat(2048));
    }
    let scoped = projection_for(&full, Some(&job_id)).unwrap();
    // Empty job ID deliberately takes the pre-change broad entity projection;
    // the running target job remains included through the active-job predicate.
    let previous=projection_for(&full,Some("")).unwrap();
    assert_eq!(ids(&scoped, "items"), vec!["selected", "prior"]);
    assert_eq!(ids(&scoped, "branches"), vec!["branch-selected", "branch-prior"]);
    let full_bytes = full.to_string().len();
    let previous_bytes=previous.to_string().len();
    let scoped_bytes = scoped.to_string().len();
    eprintln!("preparation scope synthetic: full_bytes={full_bytes}, previous_projection_bytes={previous_bytes}, scoped_bytes={scoped_bytes}, full_items={}, scoped_items={}, full_branches={}, scoped_branches={}",
        ids(&full, "items").len(), ids(&scoped, "items").len(), ids(&full, "branches").len(), ids(&scoped, "branches").len());
    assert!(scoped_bytes * 2 < previous_bytes, "irrelevant item and branch history should materially shrink beyond the previous projection");
    let bundle = &crate::row(&full, "jobs", &job_id).unwrap()["prepareBundle"];
    assert_eq!(crate::prepare_bundle::current(&full, bundle), crate::prepare_bundle::current(&scoped, bundle));
}

#[test]
fn scheduling_rejects_forged_group_fingerprints_and_stage_fields() {
    let full=fixture();
    let before=schedule_projection(&full).unwrap();
    let mut after=before.clone();
    let scheduled=crate::engine_prepare::schedule(&mut after,crate::engine_prepare::Input {
        item_ids:vec!["selected".into()],instruction:None,
    }).unwrap();
    validate_schedule_change(&before,&after).unwrap();
    for mutation in ["fingerprint","status","recipient","extra_stage"] {
        let mut invalid=after.clone();
        let stages=&mut crate::row_mut(&mut invalid,"jobs",&scheduled.job_id).unwrap()["preparationStages"];
        match mutation {
            "fingerprint"=>stages["groupAdmission"][0]["fingerprints"]["selected"]=json!("0".repeat(64)),
            "status"=>stages["groupAdmission"][0]["status"]=json!("admitted"),
            "recipient"=>stages["groupAdmission"][0]["itemIds"]=json!(["other-platform"]),
            _=>stages["unreviewedAuthority"]=json!(true),
        }
        assert!(validate_schedule_change(&before,&invalid).is_err(),"{mutation}");
    }
}

#[test]
fn claim_rejects_forged_group_capture() {
    let (mut full,job_id)=scheduled();
    // Validate the new claim before uncertainty appears; a new reservation may
    // never be admitted over an existing UNKNOWN operation.
    full["operations"].as_array_mut().unwrap().retain(|op|op["id"]!="selected-unknown");
    let mut job=crate::row(&full,"jobs",&job_id).unwrap().clone();
    full["jobs"].as_array_mut().unwrap().retain(|row|row["id"]!=job_id);
    let before=projection_of(&full).unwrap();
    job["purpose"]=json!("auto_prepare");
    let mut after=before.clone();
    crate::list_mut(&mut after,"jobs").push(job);
    validate_claim_change(&before,&after).unwrap();
    crate::row_mut(&mut after,"jobs",&job_id).unwrap()["preparationStages"]["groupAdmission"][0]["fingerprints"]["selected"]=json!("forged");
    assert!(validate_claim_change(&before,&after).unwrap_err().1.contains("group bindings"));
}

#[test]
fn latest_identity_and_source_changes_keep_full_and_scoped_currentness_equal() {
    let (mut full, job_id) = scheduled();
    let bundle = crate::row(&full, "jobs", &job_id).unwrap()["prepareBundle"].clone();
    crate::row_mut(&mut full, "items", "selected").unwrap()["authorId"] = json!("author-2");
    let changed_author = projection_for(&full, Some(&job_id)).unwrap();
    assert_eq!(ids(&changed_author, "items"), vec!["selected", "new-author"]);
    assert_eq!(crate::prepare_bundle::current(&full, &bundle), crate::prepare_bundle::current(&changed_author, &bundle));
    assert!(crate::prepare_bundle::current(&changed_author, &bundle).is_err(), "new same-author history changes source evidence");

    crate::row_mut(&mut full, "items", "selected").unwrap()["authorId"] = json!("author-1");
    crate::row_mut(&mut full, "branches", "branch-selected").unwrap()["messages"][0]["text"] = json!("Changed source");
    let changed_source = projection_for(&full, Some(&job_id)).unwrap();
    assert_eq!(crate::prepare_bundle::current(&full, &bundle), crate::prepare_bundle::current(&changed_source, &bundle));
    assert!(crate::prepare_bundle::current(&changed_source, &bundle).is_err(), "stale branch source must be rejected");
}

#[test]
fn absent_or_malformed_scope_falls_back_to_complete_history() {
    let (mut full, job_id) = scheduled();
    // This fallback contract predates durable scope proofs. A forged NEW proof
    // remains an admission error, rather than becoming a legacy full read.
    crate::row_mut(&mut full,"jobs",&job_id).unwrap().as_object_mut().unwrap().remove("scopeReservation");
    crate::row_mut(&mut full,"jobs",&job_id).unwrap()["status"]=json!("completed");
    for selected_job in [None, Some(""), Some("missing-job")] {
        let projected = projection_for(&full, selected_job).unwrap();
        for table in ["items", "branches", "proposals", "operations"] {
            assert_eq!(projected[table], full[table], "{table} must remain complete for {selected_job:?}");
        }
    }
    for bad_ids in [json!([]), json!(["selected", "selected"]), json!("selected")] {
        let mut malformed = full.clone();
        crate::row_mut(&mut malformed, "jobs", &job_id).unwrap()["prepareBundle"]["itemIds"] = bad_ids;
        let projected = projection_for(&malformed, Some(&job_id)).unwrap();
        for table in ["items", "branches", "proposals", "operations"] {
            assert_eq!(projected[table], malformed[table], "{table} must remain complete for malformed bundle");
        }
    }
    let mut missing_bundle = full.clone();
    crate::row_mut(&mut missing_bundle, "jobs", &job_id).unwrap().as_object_mut().unwrap().remove("prepareBundle");
    let projected = projection_for(&missing_bundle, Some(&job_id)).unwrap();
    assert_eq!(projected["items"], missing_bundle["items"]);

    let mut absent_recipient = full.clone();
    crate::row_mut(&mut absent_recipient, "jobs", &job_id).unwrap()["prepareBundle"]["itemIds"] = json!(["missing"]);
    let projected = projection_for(&absent_recipient, Some(&job_id)).unwrap();
    assert!(ids(&projected, "items").is_empty());
    let bundle = &crate::row(&absent_recipient, "jobs", &job_id).unwrap()["prepareBundle"];
    assert!(crate::prepare_bundle::current(&absent_recipient, bundle).is_err());
    assert!(crate::prepare_bundle::current(&projected, bundle).is_err());
}

#[tokio::test]
async fn sqlite_context_read_and_first_save_preserve_unloaded_history() {
    let folder = tempfile::tempdir().unwrap();
    let db = Database::Sqlite(crate::open_db(&folder.path().join("scope.sqlite")).await.unwrap());
    let (mut full, job_id) = scheduled();
    // The saved first-pass fixture is a pre-single-pass job; retain its exact
    // old request shape and rebind the immutable request digest.
    crate::row_mut(&mut full,"jobs",&job_id).unwrap().as_object_mut().unwrap().remove("scopeReservation");
    let bundle=&mut crate::row_mut(&mut full,"jobs",&job_id).unwrap()["prepareBundle"];
    // Construct the genuine historical request before saving this fixture;
    // absent new selectors do not authorize future fresh model/dispatch work.
    let request=bundle["request"].as_object_mut().unwrap();
    for field in ["preparationMode","responseContract","modelContextContract",
        "researchPolicy","researchLimitContract","recoveryEvidenceContract",
        "factDependencyContract","visualNeedContract","visualSelection",
        "strictGroupContract","strictGroup","mandatoryMaterialContract",
        "postContextBundle","materialReadiness"] {request.remove(field);}
    bundle["digest"]=json!(format!("{:x}",Sha256::digest(bundle["request"].to_string().as_bytes())));
    // Keep this persistence fixture within one company; the pure projection
    // test above separately exercises cross-company identity evidence.
    for table in ["items", "posts", "branches"] {
        full[table].as_array_mut().unwrap().retain(|row| !row["id"].as_str().unwrap().contains("other-company"));
    }
    db.change(|d| { *d = full.clone(); Ok(()) }).await.unwrap();
    let projected = db.read_preparation_context(&job_id).await.unwrap();
    assert_eq!(projected, projection_for(&full, Some(&job_id)).unwrap());
    assert_eq!(ids(&projected, "items"), vec!["selected", "prior"]);
    assert_eq!(projected["operations"][0]["status"], "unknown");

    let result = json!({"text":"Selected comment needs review","sources":[],
        "assessments":[{"itemId":"selected","outcome":"needs_attention","reason":"Synthetic evidence"}],
        "proposals":[]});
    let (_, changed) = db.change_preparation_first_observed(&job_id, |view| {
        let bundle = crate::row(view, "jobs", &job_id)?["prepareBundle"].clone();
        crate::prepare_bundle::current(view, &bundle).map_err(crate::conflict)?;
        crate::preparation_review::record_first(view, &job_id, &result, "2026-09-28T12:00:00Z")
    }).await.unwrap();
    assert!(changed);
    let saved = db.read().await.unwrap();
    assert_eq!(saved["items"], full["items"]);
    assert_eq!(saved["branches"], full["branches"]);
    assert_eq!(saved["operations"], full["operations"], "UNKNOWN operations remain durable");
    assert_eq!(saved["proposals"], full["proposals"]);
    assert_eq!(crate::row(&saved, "jobs", &job_id).unwrap()["preparationStages"]["first"]["status"], "completed");

    assert!(db.change_preparation_first_observed(&job_id, |view| {
        crate::row_mut(view, "items", "selected")?["draft"] = json!("unapproved edit");
        Ok(())
    }).await.is_err());
    assert_eq!(db.read().await.unwrap(), saved, "rejected scoped write must roll back");
    db.close().await;
}

#[tokio::test]
#[ignore = "requires a pristine explicitly isolated communityhero_writer_v51_test_ PostgreSQL fixture"]
async fn postgres_addressed_scope_parity_rollback_and_corrupt_discriminators() {
    let db=writer_v51_fixture_db().await;
    let (mut initial,job_id)=scheduled();
    // The pure fixture's intentionally minimal legacy approval is not a valid
    // PostgreSQL import shape. This fixture preserves history in the other rows.
    initial["approvals"]=json!([]);
    for table in ["items","posts","branches"] {
        initial[table].as_array_mut().unwrap().retain(|row|!row["id"].as_str().unwrap().contains("other-company"));
    }
    db.change(|d|{*d=initial;Ok(())}).await.unwrap();
    let baseline=db.read().await.unwrap();
    let started=std::time::Instant::now();
    let scoped=db.read_preparation_context(&job_id).await.unwrap();
    let read_ms=started.elapsed().as_secs_f64()*1000.0;
    assert_eq!(scoped,projection_for(&baseline,Some(&job_id)).unwrap(),"SQL/pure addressed read parity");
    assert_eq!(ids(&scoped,"items"),vec!["selected","prior"]);
    assert_eq!(scoped["operations"][0]["status"],"unknown");
    assert!(crate::preparation_review::chunks::current(&scoped,crate::row(&scoped,"jobs",&job_id).unwrap()).is_err(),"selected UNKNOWN must still block admission");
    for id in ["","missing-job"] {
        assert_eq!(db.read_preparation_context(id).await.unwrap(),projection_for(&baseline,Some(id)).unwrap(),"legacy broad fallback SQL parity");
    }
    let result=json!({"text":"Selected comment needs review","sources":[],
        "assessments":[{"itemId":"selected","outcome":"needs_attention","reason":"Synthetic evidence"}],"proposals":[]});
    let mut expected=baseline.clone();
    crate::preparation_review::record_first(&mut expected,&job_id,&result,"2026-09-28T12:00:00Z").unwrap();
    let Database::Postgres{writer,..}=&db else{unreachable!()};
    sqlx::query("CREATE FUNCTION pg_temp.scope_reject() RETURNS trigger LANGUAGE plpgsql AS 'BEGIN RAISE EXCEPTION ''synthetic scope final write rejection''; END;'")
        .execute(writer).await.unwrap();
    sqlx::query("CREATE TRIGGER scope_reject AFTER UPDATE ON communityhero.jobs FOR EACH ROW EXECUTE FUNCTION pg_temp.scope_reject()")
        .execute(writer).await.unwrap();
    let rejected=db.change_preparation_first_observed(&job_id,|d|{
        assert_eq!(*d,scoped,"SQL/pure writer projection parity");
        crate::preparation_review::record_first(d,&job_id,&result,"2026-09-28T12:00:00Z")
    }).await;
    sqlx::query("DROP TRIGGER scope_reject ON communityhero.jobs").execute(writer).await.unwrap();
    assert!(rejected.is_err());
    assert_eq!(db.read().await.unwrap(),baseline,"late SQL failure rolls back scoped job save");
    db.change_preparation_first_observed(&job_id,|d|crate::preparation_review::record_first(d,&job_id,&result,"2026-09-28T12:00:00Z")).await.unwrap();
    assert_eq!(db.read().await.unwrap(),expected,"scoped commit preserves omitted history");

    // These inconsistencies are legal under the schema. Both representations
    // must select the row, then the loader must reject its projection mismatch.
    for table in ["operations","proposals"] {
        let id=if table=="operations"{"selected-unknown"}else{"selected-draft"};
        let replacement=if table=="operations"{"NULL"}else{"'other-platform'"};
        let statement=format!("UPDATE communityhero.{table} SET item_id={replacement} WHERE workspace_id=$1 AND id=$2");
        sqlx::query(sqlx::AssertSqlSafe(statement.as_str())).bind(WORKSPACE).bind(id).execute(writer).await.unwrap();
        assert!(db.read_preparation_context(&job_id).await.unwrap_err().1.contains("projection mismatch"));
        let statement=format!("UPDATE communityhero.{table} SET item_id='selected' WHERE workspace_id=$1 AND id=$2");
        sqlx::query(sqlx::AssertSqlSafe(statement.as_str())).bind(WORKSPACE).bind(id).execute(writer).await.unwrap();
    }
    for id in ["media","active"] {
        sqlx::query("UPDATE communityhero.jobs SET kind='other',status='completed' WHERE workspace_id=$1 AND id=$2")
            .bind(WORKSPACE).bind(id).execute(writer).await.unwrap();
        assert!(db.read_preparation_context(&job_id).await.unwrap_err().1.contains("projection mismatch"));
        sqlx::query("UPDATE communityhero.jobs SET kind=payload->>'kind',status=payload->>'status' WHERE workspace_id=$1 AND id=$2")
            .bind(WORKSPACE).bind(id).execute(writer).await.unwrap();
    }
    assert_eq!(db.read().await.unwrap(),expected,"corruption probes restore original fixture");
    eprintln!("PG_ADDRESSED_SCOPE readParity=true writeParity=true lateRollback=true unknownGuard=true corruptDiscriminatorsRejected=true readMs={read_ms:.2}");
    db.close().await;
}
