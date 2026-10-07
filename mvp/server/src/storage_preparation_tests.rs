use super::*;
use serde_json::json;
use std::time::Instant;

const NOW: i64 = 1_765_000_000;

#[test]
fn preparation_material_and_repair_children_stay_full_after_terminal_settlement() {
    let mut full=crate::empty();normalize(&mut full);crate::accounts::initialize(&mut full,crate::accounts::Profile::BawRussia).unwrap();
    let fields=["modelMaterialReceipts","videoFrameNeeds","frameNeedOutcome","sourceOriginJobId","nativeSourceOriginJobId","sourceProofRef","repairPaidIntent"];
    for (index,field) in fields.iter().enumerate(){
        let mut job=json!({"id":format!("settled-{index}"),"kind":"assistant","purpose":"discussion","status":"completed",
            "retainedEvidence":{"paidResultRef":"immutable-paid"},"unknownFutureEvidence":{"keep":"whole"}});
        job[*field]=json!({"nativeJobId":"origin","evidence":"complete"});crate::list_mut(&mut full,"jobs").push(job);
    }
    crate::list_mut(&mut full,"jobs").push(json!({"id":"settled-repair-stage","kind":"assistant","status":"failed",
        "preparationStages":{"answeringRepairs":[{"paidResultRef":"original"}],"repairBudget":{"rounds":1}}}));
    let view=schedule_projection(&full).unwrap();assert_eq!(view["jobs"],full["jobs"],"settled repair history is full canonical evidence, not a control view");
    for job in crate::list(&view,"jobs"){assert!(retained_material_job(job));}
}

#[test]
fn first_stage_accepts_exact_native_frame_needs_and_rejects_rehashed_forgery() {
    let (full_before,full_after)=crate::video_frame_work::first_needs_fixture();
    let before=projection_for(&full_before,Some("root")).unwrap();let mut after=before.clone();
    *crate::row_mut(&mut after,"jobs","root").unwrap()=crate::row(&full_after,"jobs","root").unwrap().clone();
    validate_first_stage_change(&before,&after,"root").unwrap();
    assert_eq!(crate::row(&after,"jobs","root").unwrap()["videoFrameNeeds"].as_array().unwrap().len(),1);
    validate_first_stage_change(&after,&after,"root").unwrap();
    for field in ["companyId","member","asset","parentPaidResultRef","budget","createdAt"] {
        let mut forged=after.clone();let job=crate::row_mut(&mut forged,"jobs","root").unwrap();
        job["videoFrameNeeds"][0][field]=json!("forged");let need=&mut job["videoFrameNeeds"][0];
        need.as_object_mut().unwrap().remove("needSha256");need["needSha256"]=json!(crate::preparation_materials::hash(need));
        job["preparationStages"]["first"]["result"]["nativeVideoFrameNeeds"]=job["videoFrameNeeds"].clone();
        assert!(validate_first_stage_change(&before,&forged,"root").is_err(),"exact derived {field}");
    }
    let mut foreign=after.clone();foreign["posts"][0]["text"]=json!("different source");
    assert!(validate_first_stage_change(&before,&foreign,"root").is_err(),"first settlement cannot edit source");
    let mut rewrote=after.clone();crate::row_mut(&mut rewrote,"jobs","root").unwrap()["frameNeedOutcome"]["status"]=json!("complete");
    assert!(validate_first_stage_change(&after,&rewrote,"root").is_err(),"replay cannot rewrite a previously persisted need outcome");
}

fn initialize_claim_fixture_lifecycle(d:&mut Value) {
    // Complete only absent synthetic ledger fields, retaining existing owner
    // and every historical receipt before the native bootstrap reducer.
    crate::native_fixture_owner_repair::initialize_workspace(d).unwrap();
}
fn claim_revalidation_fixture_admitted(d:&mut Value,at:i64)->ApiResult<Option<(String,Value)>> {
    let token=crate::runtime_lifecycle::admission_token(d,crate::runtime_lifecycle::AdmissionClass::Preparation)?;
    let old_count=crate::list(d,"jobs").len();
    let claimed=crate::auto_prepare::claim(d,at)?;
    if let Some((run,_))=&claimed {
        // Match native commit_captured bookkeeping on this new revalidation only.
        assert_eq!(crate::list(d,"jobs").len(),old_count+1);
        assert_eq!(crate::list(d,"jobs").last().unwrap()["id"],*run);
        let job=crate::row_mut(d,"jobs",run)?;
        assert_eq!(job["purpose"],"auto_revalidate");
        assert!(job["preparationStages"].is_null());
        job["preparationStages"]=json!({"first":null,"review":null});
        let at=chrono::DateTime::<chrono::Utc>::from_timestamp(at,0).unwrap().to_rfc3339();
        crate::preparation_review::record_initial_admission(d,&token,run,&at)?;
    }
    Ok(claimed)
}

fn admission_job(id:&str)->Value {
    json!({"id":id,"kind":"assistant","refId":"engine_prepare","status":"running",
        "createdAt":"2026-09-26T12:00:00Z","purpose":"engine_prepare",
        "requestedItemIds":["i"],"selectedItemIds":[],"held":[],
        "preparationStages":{"first":null,"review":null}})
}
fn admission_receipt(key:&str,job:&str)->Value {
    json!({"id":crate::local_admission::receipt_id("prepare",key),
        "action":"local_admission.committed","kind":"prepare","refId":key,"requestId":key,
        "account":"likeavto","actorId":"owner","actorRole":"owner","requestHash":"a".repeat(64),"payloadHash":"b".repeat(64),
        "result":{"jobId":job,"requestId":key,"replayed":false},"createdAt":"2026-09-26T12:00:00Z"})
}
fn append_admission(d:&mut Value,key:&str,job:&str)->ApiResult<()> {
    crate::list_mut(d,"jobs").push(admission_job(job));
    crate::list_mut(d,"audit").push(admission_receipt(key,job));
    Ok(())
}

#[test]
fn local_admission_schedule_is_narrow_and_replay_does_not_load_finished_job() {
    let mut original=fixture();
    append_admission(&mut original,"prior","prior-job").unwrap();
    crate::row_mut(&mut original,"jobs","prior-job").unwrap()["status"]=json!("completed");
    crate::list_mut(&mut original,"audit").push(json!({"id":"approval-receipt","action":"local_admission.committed",
        "kind":"approval","result":{"jobId":"sync-old"}}));
    let before=schedule_projection(&original).unwrap();
    assert_eq!(before["audit"].as_array().unwrap().len(),1);
    assert!(before["jobs"].as_array().unwrap().is_empty());
    assert_eq!(crate::local_admission::find_receipt(&before,"prepare","prior").unwrap().unwrap()["result"]["jobId"],"prior-job");
    assert!(validate_schedule_change(&before,&before).is_ok());
    let mut unchanged=original.clone();
    merge_schedule_delta(&mut unchanged,&before,&before).unwrap();
    assert_eq!(unchanged,original);
    let mut after=before.clone();
    append_admission(&mut after,"next","next-job").unwrap();
    validate_schedule_change(&before,&after).unwrap();
    let mut merged=original.clone();
    merge_schedule_delta(&mut merged,&before,&after).unwrap();
    assert_eq!(merged["audit"].as_array().unwrap().len(),4);
    assert_eq!(merged["audit"][0],original["audit"][0]);
    assert_eq!(merged["audit"][2],original["audit"][2]);
    assert_eq!(crate::row(&merged,"jobs","prior-job").unwrap(),crate::row(&original,"jobs","prior-job").unwrap());
    for mutation in ["old_receipt","old_job","extra_receipt","receipt_only","wrong_job","wrong_kind",
        "wrong_account","wrong_identity","extra_field","wrong_result","source"] {
        let mut invalid=after.clone();
        match mutation {
            "old_receipt"=>invalid["audit"][0]["actorId"]=json!("other"),
            "old_job"=>invalid["jobs"][0]["status"]=json!("failed"),
            "extra_receipt"=>crate::list_mut(&mut invalid,"audit").push(admission_receipt("extra","next-job")),
            "receipt_only"=>{crate::list_mut(&mut invalid,"jobs").pop();},
            "wrong_job"=>invalid["audit"][1]["result"]["jobId"]=json!("prior-job"),
            "wrong_kind"=>invalid["audit"][1]["kind"]=json!("approval"),
            "wrong_account"=>invalid["audit"][1]["account"]=json!("baw-russia"),
            "wrong_identity"=>invalid["audit"][1]["id"]=json!("other"),
            "extra_field"=>invalid["audit"][1]["secret"]=json!("unrelated"),
            "wrong_result"=>invalid["audit"][1]["result"]["replayed"]=json!(true),
            _=>invalid["items"][0]["draft"]=json!("changed"),
        }
        assert!(validate_schedule_change(&before,&invalid).is_err(),"{mutation}");
    }
}

#[tokio::test]
async fn sqlite_local_admission_restart_replay_and_atomic_rollback() {
    let folder=tempfile::tempdir().unwrap();
    let path=folder.path().join("workspace.sqlite");
    let db=Database::Sqlite(crate::open_db(&path).await.unwrap());
    db.change(|d| { *d=fixture(); for table in ["knowledge_entries","knowledge_versions","feedback"] {
        d[table]=json!([]);
    } Ok(()) }).await.unwrap();
    let baseline=db.read().await.unwrap();
    assert!(db.read_local_admission_receipt("prepare","absent").await.unwrap().is_none());
    let (_,changed)=db.change_preparation_schedule_observed(|d|append_admission(d,"one","job-one")).await.unwrap();
    assert!(changed);
    let saved=db.read().await.unwrap();
    assert_eq!(saved["jobs"].as_array().unwrap().len(),baseline["jobs"].as_array().unwrap().len()+1);
    assert_eq!(saved["audit"].as_array().unwrap().len(),baseline["audit"].as_array().unwrap().len()+1);
    for table in ["posts","branches","items","proposals","operations","approvals","conversations"] {
        assert_eq!(saved[table],baseline[table],"{table}");
    }
    db.close().await;
    let db=Database::Sqlite(crate::open_db(&path).await.unwrap());
    assert_eq!(db.read_local_admission_receipt("prepare","one").await.unwrap(),Some(admission_receipt("one","job-one")));
    let (replayed,changed)=db.change_preparation_schedule_observed(|d| {
        // Replay is unchanged, but this owner is still running. Atomic capacity
        // admission must see it; completed history stays covered separately.
        assert_eq!(crate::row(d,"jobs","job-one").unwrap()["status"],"running");
        Ok(crate::local_admission::find_receipt(d,"prepare","one")?.unwrap()["result"].clone())
    }).await.unwrap();
    assert!(!changed); assert_eq!(replayed["jobId"],"job-one");
    assert_eq!(db.read().await.unwrap(),saved);
    let Database::Sqlite(pool)=&db else {unreachable!()};
    sqlx::query("CREATE TRIGGER admission_fail BEFORE UPDATE ON workspace BEGIN SELECT RAISE(ABORT,'offline test rejection'); END")
        .execute(pool).await.unwrap();
    assert!(db.change_preparation_schedule_observed(|d|append_admission(d,"two","job-two")).await.is_err());
    assert!(db.read_local_admission_receipt("prepare","two").await.unwrap().is_none());
    assert_eq!(db.read().await.unwrap(),saved,"storage failure must roll back job and receipt together");
    sqlx::query("DROP TRIGGER admission_fail").execute(pool).await.unwrap();
    assert!(db.change_preparation_schedule_observed(|d| {
        append_admission(d,"two","job-two")?;
        d["audit"][0]["result"]["jobId"]=json!("rewritten");
        Ok(())
    }).await.is_err());
    assert_eq!(db.read().await.unwrap(),saved);
    db.change(|d| {let mut corrupt=admission_receipt("corrupt","job-one");
        corrupt["kind"]=json!("approval");crate::list_mut(d,"audit").push(corrupt);Ok(())}).await.unwrap();
    assert!(db.read_local_admission_receipt("prepare","corrupt").await.is_err(),"wrong discriminator is not absence");
    db.close().await;
}

#[tokio::test]
async fn sqlite_local_admission_concurrent_same_key_has_one_commit() {
    let folder=tempfile::tempdir().unwrap();
    let db=Database::Sqlite(crate::open_db(&folder.path().join("workspace.sqlite")).await.unwrap());
    let admit=|d:&mut Value| {
        if let Some(receipt)=crate::local_admission::find_receipt(d,"prepare","race")? {
            return Ok(receipt["result"]["jobId"].clone());
        }
        append_admission(d,"race","race-job")?;
        Ok(json!("race-job"))
    };
    let (left,right)=tokio::join!(db.change_preparation_schedule_observed(admit),
        db.change_preparation_schedule_observed(admit));
    let (left,changed_left)=left.unwrap();let (right,changed_right)=right.unwrap();
    assert_eq!(left,right);assert_ne!(changed_left,changed_right);
    let saved=db.read().await.unwrap();
    assert_eq!(saved["jobs"].as_array().unwrap().len(),1);
    assert_eq!(saved["audit"].as_array().unwrap().len(),1);
    db.close().await;
}

fn fixture() -> Value {
    let at = chrono::DateTime::from_timestamp(NOW, 0).unwrap().to_rfc3339();
    let created = chrono::DateTime::from_timestamp(NOW - 60, 0).unwrap().to_rfc3339();
    let mut d = crate::empty();
    d["items"] = json!([{"id":"i","itemId":"c","objectId":"o","postKey":"p","conversationKey":"thread",
        "branchId":"b","postId":"post","revision":1,"draft":"","workflow":"attention",
        "providerStatus":"new","createdAt":created,"providerObservedAt":at}]);
    d["branches"] = json!([{"id":"b","postId":"post","messages":[{"id":"c","text":"Hi"}],"contextComplete":false}]);
    d["posts"] = json!([{"id":"post","postKey":"p","text":"Post"}]);
    let research_at = crate::now();
    let mut archive = json!({"id":"research:old","jobId":"paid-research","account":"LikeAvto",
        "connectorBinding":null,"createdAt":research_at,"trust":"source_only","activePolicy":false,
        "posts":[{"id":"post","postKey":"p","text":"Post"}],"bindings":[{"itemId":"i","postKey":"p"}],
        "review":{"status":"completed","research":{"version":1,"status":"completed","trust":"source_only",
            "webCalls":1,"completedAt":research_at,"sources":[{"itemId":"i","url":"https://manufacturer.example/specs",
                "title":"Specs","claim":"Source describes this model","trust":"source_only"}]}}});
    archive["checksum"] = json!(crate::research_cache::checksum(&archive));
    d["preparationResearch"] = json!([archive]);
    d["jobs"] = json!([
        {"id":"sync-old","kind":"sync","status":"completed","payload":{"bulk":"unrelated"}},
        {"id":"discussion-old","kind":"assistant","purpose":"discussion","status":"completed","toolResults":[{"private":"unrelated"}]}
    ]);
    d["conversations"] = json!([{"id":"private","messages":[{"text":"unrelated"}]}]);
    d["audit"] = json!([{"id":"audit-old","action":"other","refId":"i"}]);
    d["approvals"] = json!([{"id":"approval-old","status":"approved","proposals":[]}]);
    d["feedback"] = json!([{"id":"feedback-old","itemId":"i"}]);
    d
}

fn normalize_ids(value: &mut Value) {
    match value {
        Value::String(text) if uuid::Uuid::parse_str(text).is_ok() => *text = "<new-id>".into(),
        Value::Array(values) => values.iter_mut().for_each(normalize_ids),
        Value::Object(fields) => fields.values_mut().for_each(normalize_ids),
        _ => (),
    }
}

#[test]
fn explicit_schedule_projection_preserves_request_and_only_appends_one_job() {
    let mut original=fixture();
    original["jobs"].as_array_mut().unwrap().push(json!({"id":"media-history","kind":"media","status":"completed"}));
    let mut full=original.clone();
    let before=schedule_projection(&original).unwrap();
    let mut scoped=before.clone();
    let input=crate::engine_prepare::Input{item_ids:vec!["i".into()],instruction:Some("Use current evidence".into())};
    crate::engine_prepare::schedule(&mut full,input.clone()).unwrap();
    crate::engine_prepare::schedule(&mut scoped,input).unwrap();
    validate_schedule_change(&before,&scoped).unwrap();
    let full_job=full["jobs"].as_array().unwrap().last().unwrap();
    let scoped_job=scoped["jobs"].as_array().unwrap().last().unwrap();
    assert_eq!(full_job["selectedItemIds"],scoped_job["selectedItemIds"]);
    assert_eq!(full_job["held"],scoped_job["held"]);
    assert_eq!(full_job["prepareBundle"]["request"],scoped_job["prepareBundle"]["request"]);
    assert_eq!(full_job["prepareBundle"]["dependencyDigest"],scoped_job["prepareBundle"]["dependencyDigest"]);
    assert!(!scoped_job["prepareBundle"]["researchManifest"].as_array().unwrap().is_empty(),
        "scoped scheduling must retain cached research");
    assert!(crate::prepare_bundle::current(&scoped,&scoped_job["prepareBundle"]).is_ok());
    let mut merged=original.clone();
    merge_schedule_delta(&mut merged,&before,&scoped).unwrap();
    assert_eq!(merged["jobs"].as_array().unwrap().len(),original["jobs"].as_array().unwrap().len()+1);
    for table in ["items","posts","branches","materials","knowledge_entries","knowledge_versions",
        "preparationResearch","operations","proposals","conversations","audit","approvals","feedback"] {
        assert_eq!(merged[table],original[table],"{table}");
    }
    for mutation in ["item","post","metadata","prior_job","extra_job"] {
        let mut invalid=scoped.clone();
        match mutation {
            "item"=>invalid["items"][0]["text"]=json!("Changed"),
            "post"=>invalid["posts"][0]["text"]=json!("Changed"),
            "metadata"=>invalid["settings"]["provider"]=json!("changed"),
            "prior_job"=>invalid["jobs"][0]["status"]=json!("running"),
            _=>invalid["jobs"].as_array_mut().unwrap().push(json!({"id":"extra"})),
        }
        assert!(validate_schedule_change(&before,&invalid).is_err(),"{mutation}");
    }
}

#[test]
fn explicit_schedule_projection_keeps_duration_pending_audio_and_owner_floor() {
    let mut original=fixture();
    original["posts"][0]["attachments"]=json!([{"type":"video"}]);
    let post=original["posts"][0].clone();
    let binding=crate::active_binding(&original).unwrap().to_json();
    // Default missing media is assessable under the new semantic contract;
    // an exact owner prerequisite still blocks admission without an attempt.
    original["settings"]["postMediaPolicies"]=json!({(post["id"].as_str().unwrap()):{
        "version":1,"revision":1,"status":"active","postId":post["id"],"mode":"full_audio_only",
        "account":original["account"],"connectorBinding":binding,
        "sourceVersion":crate::media_fullframes::source_version(&post,"LikeAvto")}});
    let mut progress=crate::media_fullframes::initial("LikeAvto",&binding,&post,"now");
    progress["phase"]=json!("inventory");
    progress["source"]=json!({"sha256":"a".repeat(64),"bytes":1024});
    progress["sourceIdentity"]=json!({"account":"LikeAvto","postKey":"p",
        "mediaSha256":"a".repeat(64),"durationMs":181000});
    original["jobs"].as_array_mut().unwrap().extend([
        json!({"id":"probed","kind":"media","purpose":"auto_media","status":"queued",
            "visualContractVersion":2,"account":"LikeAvto","connectorBinding":binding,"refId":"post",
            "result":{"visualProgress":progress}}),
        json!({"id":"pending-audio","kind":"media_audio","purpose":"explicit_cached_audio_only",
            "status":"queued","refId":"post"})]);
    let scoped=schedule_projection(&original).unwrap();
    assert_eq!(scoped["jobs"].as_array().unwrap().iter().map(|j|j["id"].as_str().unwrap()).collect::<Vec<_>>(),
        vec!["probed","pending-audio"]);
    let policy=crate::post_media_policy::effective(&scoped,&scoped["posts"][0]).unwrap();
    assert_eq!(policy["mode"],"full_audio_only");
    assert_eq!(crate::post_media_policy::probed_duration(&scoped,&scoped["posts"][0]).unwrap()["durationMs"],181000);
    assert_eq!(crate::post_media_policy::effective_for_preparation(&scoped,&scoped["posts"][0]).unwrap()["decisionBasis"]["kind"],"exact_owner_override");
    let input=crate::engine_prepare::Input{item_ids:vec!["i".into()],instruction:None};
    let mut full=original.clone();let mut projected=scoped.clone();
    crate::engine_prepare::schedule(&mut full,input.clone()).unwrap();
    crate::engine_prepare::schedule(&mut projected,input).unwrap();
    let full_job=full["jobs"].as_array().unwrap().last().unwrap();
    let scoped_job=projected["jobs"].as_array().unwrap().last().unwrap();
    assert_eq!(full_job["selectedItemIds"],json!([]));
    assert_eq!(full_job["held"],scoped_job["held"]);
    assert_eq!(full_job["held"][0]["reason"],"media_wait");
    validate_schedule_change(&scoped,&projected).unwrap();
}

#[tokio::test]
async fn sqlite_explicit_schedule_transaction_preserves_unrelated_history() {
    let folder=tempfile::tempdir().unwrap();
    let db=Database::Sqlite(crate::open_db(&folder.path().join("workspace.sqlite")).await.unwrap());
    let original=fixture();
    let Database::Sqlite(pool)=&db else{unreachable!()};
    sqlx::query("UPDATE workspace SET payload=? WHERE id=1")
        .bind(original.to_string()).execute(pool).await.unwrap();
    let ((),changed)=db.change_preparation_schedule_observed(|d|{
        crate::engine_prepare::schedule(d,crate::engine_prepare::Input{item_ids:vec!["i".into()],instruction:None})?;
        Ok(())
    }).await.unwrap();
    assert!(changed);
    let after=db.read().await.unwrap();
    assert_eq!(after["jobs"].as_array().unwrap().len(),original["jobs"].as_array().unwrap().len()+1);
    for table in ["items","posts","branches","conversations","audit","approvals","feedback","preparationResearch"] {
        assert_eq!(after[table],original[table],"{table}");
    }
    db.close().await;
}

#[test]
fn claim_matches_full_workspace_and_preserves_omitted_history() {
    let mut full = fixture();
    let original = full.clone();
    let before = projection_of(&full).unwrap();
    assert!(!crate::list(&before, "jobs").iter().any(|j| j["id"] == "sync-old" || j["id"] == "discussion-old"));
    for name in OMITTED { assert!(before.get(*name).is_none()); }
    let mut after = before.clone();
    let mut full_result = crate::auto_prepare::claim(&mut full, NOW).unwrap();
    let mut projected_result = crate::auto_prepare::claim(&mut after, NOW).unwrap();
    assert!(full_result.is_some(), "fixture must exercise an actual preparation claim");
    assert!(projected_result.is_some());
    assert!(after["jobs"].as_array().unwrap().last().unwrap()["prepareBundle"]["researchManifest"].as_array().is_some_and(|rows| !rows.is_empty()),
        "claim must pin reusable paid research");
    validate_claim_change(&before, &after).unwrap();
    let mut merged = original;
    merge_claim_delta(&mut merged, &before, &after).unwrap();
    // new_job timestamps at wall clock time, while the claim's durable
    // eligibility/reconciliation clock is the explicit NOW argument.
    full["jobs"].as_array_mut().unwrap().last_mut().unwrap()["createdAt"] = json!("<claim-time>");
    merged["jobs"].as_array_mut().unwrap().last_mut().unwrap()["createdAt"] = json!("<claim-time>");
    normalize_ids(&mut full);
    normalize_ids(&mut merged);
    normalize_ids(&mut full_result.as_mut().unwrap().1);
    normalize_ids(&mut projected_result.as_mut().unwrap().1);
    assert_eq!(full, merged);
    assert_eq!(full_result.as_ref().map(|(_, request)| request), projected_result.as_ref().map(|(_, request)| request));
    assert_eq!(merged["conversations"].as_array().unwrap().len(), 1);
    assert_eq!(merged["approvals"].as_array().unwrap().len(), 1);
}

#[test]
fn historical_auto_jobs_and_proposal_references_remain_visible() {
    let mut full = fixture();
    full["jobs"].as_array_mut().unwrap().extend([
        json!({"id":"old-prep","kind":"assistant","purpose":"auto_prepare","status":"completed","refId":"i"}),
        json!({"id":"old-media","kind":"media","status":"failed","refId":"post"}),
        json!({"id":"legacy-ref","kind":"custom","status":"completed"}),
    ]);
    full["proposals"] = json!([{"id":"p","itemId":"i","status":"stale","prepareRunId":"legacy-ref"}]);
    let projected = projection_of(&full).unwrap();
    assert_eq!(crate::list(&projected, "jobs").iter().map(|j| j["id"].as_str().unwrap()).collect::<Vec<_>>(),
        vec!["old-prep", "old-media", "legacy-ref"]);
}

#[test]
fn fresh_context_claim_keeps_receipt_and_failed_job_proof_in_scoped_storage() {
    let mut baseline=fixture();
    // Seed the synthetic archive's exact connector before computing its proof,
    // native bootstrap, any paid-result capture or restart receipt creation.
    crate::accounts::initialize(&mut baseline,crate::accounts::Profile::LikeAvto).unwrap();
    let binding=baseline["connectorBinding"].clone();
    let archive=&mut baseline["preparationResearch"][0];
    archive["connectorBinding"]=binding;
    archive["checksum"]=json!(crate::research_cache::checksum(archive));
    let retained_research=baseline["preparationResearch"].clone();
    // Pin the connector and complete ledger before the failed-job request and
    // restart receipt capture their binding; bootstrap must not change it later.
    initialize_claim_fixture_lifecycle(&mut baseline);
    let (initial,_)=crate::auto_prepare::claim(&mut baseline,NOW).unwrap().unwrap();
    let job=crate::row_mut(&mut baseline,"jobs",&initial).unwrap();
    // Construct the historical no-output failure before persistence; current
    // workers must use the failure reducer rather than rewrite a paid proof.
    job.as_object_mut().unwrap().remove("scopeReservation");
    job["status"]=json!("failed");job["error"]=json!("ASSISTANT_INVALID_RESEARCH_UNOBSERVED_URL");
    let mut latest=job.clone();latest["id"]=json!("latest-held");latest["purpose"]=json!("auto_revalidate");
    // This second historical failed attempt has its own identity and capture;
    // copying another job's durable reservation would forge ownership.
    latest.as_object_mut().unwrap().remove("scopeReservation");
    baseline["jobs"].as_array_mut().unwrap().push(latest);
    baseline["items"][0]["autoPreparation"]["status"]=json!("needs_attention");
    baseline["items"][0]["autoRevalidation"]=json!({"status":"held","jobId":"latest-held"});
    baseline["branches"][0]["messages"][0]["text"]=json!("Current source");
    baseline["settings"]["autoPreparation"]=json!({"revalidation":{"enabled":true,"debounceSeconds":30}});
    let receipt=crate::preparation_restart::plan_context(&mut baseline,"fresh-scoped",true,NOW+1,Some(&["i".into()]),true).unwrap();
    assert_eq!(receipt["eligibleCount"],1);
    let before=projection_of(&baseline).unwrap();
    assert_eq!(before["preparationRuns"],baseline["preparationRuns"]);
    assert_eq!(crate::row(&before,"jobs","latest-held").unwrap(),crate::row(&baseline,"jobs","latest-held").unwrap());
    assert!(crate::row(&before,"jobs",&initial).is_ok());
    assert!(crate::row(&before,"jobs","discussion-old").is_err());
    let mut full=baseline.clone();let mut after=before.clone();
    assert!(crate::auto_prepare::claim(&mut full,NOW+2).unwrap().is_none());
    assert!(crate::auto_prepare::claim(&mut after,NOW+2).unwrap().is_none());
    let capture_started=chrono::Utc::now().timestamp();
    let full_claim=claim_revalidation_fixture_admitted(&mut full,NOW+33).unwrap().unwrap();
    let scoped_claim=claim_revalidation_fixture_admitted(&mut after,NOW+33).unwrap().unwrap();
    let capture_finished=chrono::Utc::now().timestamp();
    assert_eq!(full_claim.1,scoped_claim.1);
    assert_eq!(scoped_claim.1["previousDecision"]["prepareRunId"],"latest-held");
    validate_claim_change(&before,&after).unwrap();
    let mut merged=baseline;merge_claim_delta(&mut merged,&before,&after).unwrap();
    for doc in [&mut full,&mut merged] {
        let new_job=doc["jobs"].as_array_mut().unwrap().last_mut().unwrap();
        new_job["createdAt"]=json!("<claim-time>");
        // The two independent captures select the same paid research with
        // real wall-clock seconds, even though eligibility uses explicit NOW.
        // Normalize only this new capture's selection time, after validating
        // its bounds; all historical proofs and selected content stay exact.
        let manifest=new_job["prepareBundle"]["researchManifest"].as_array_mut().unwrap();
        assert!(!manifest.is_empty(),"fixture must retain selected paid research");
        for pin in manifest {
            let selected=chrono::DateTime::parse_from_rfc3339(pin["selectedAt"].as_str().unwrap()).unwrap().timestamp();
            assert!((capture_started..=capture_finished).contains(&selected));
            pin["selectedAt"]=json!("<claim-time>");
        }
        normalize_ids(doc);
    }
    assert_eq!(full,merged);
    assert_eq!(merged["conversations"].as_array().unwrap().len(),1);
    assert_eq!(merged["preparationRuns"][0]["runId"],"fresh-scoped");
    assert_eq!(merged["preparationResearch"],retained_research,"claims must preserve the complete seeded paid archive and checksum");
}

#[test]
fn stale_saved_proposal_claim_matches_full_and_holds_paid_result() {
    let mut full = fixture();
    full["items"][0]["workflow"] = json!("prepared");
    full["items"][0]["autoPreparation"] = json!({"status":"prepared","jobId":"old-prep"});
    full["jobs"].as_array_mut().unwrap().push(json!({"id":"old-prep","kind":"assistant",
        "purpose":"auto_prepare","status":"completed","refId":"i","prepareBundle":{"id":"old-bundle"}}));
    full["proposals"] = json!([{"id":"saved","itemId":"i","status":"draft","prepareRunId":"old-prep",
        "prepareBundleId":"other-bundle","revision":1}]);
    let original = full.clone();
    let before = projection_of(&full).unwrap();
    let mut after = before.clone();
    assert!(crate::auto_prepare::claim(&mut full, NOW).unwrap().is_none());
    assert!(crate::auto_prepare::claim(&mut after, NOW).unwrap().is_none());
    validate_claim_change(&before, &after).unwrap();
    let mut merged = original;
    merge_claim_delta(&mut merged, &before, &after).unwrap();
    assert_eq!(full, merged);
    assert_eq!(merged["proposals"][0]["status"], "stale");
    assert_eq!(merged["items"][0]["autoPreparation"]["savedProposalId"], "saved");
    assert_eq!(merged["items"][0]["autoPreparation"]["requiresReview"], true);
}

#[test]
fn revalidation_claim_matches_full_and_preserves_saved_proposal() {
    let mut baseline = fixture();
    let (original_job, _) = crate::auto_prepare::claim(&mut baseline, NOW).unwrap().unwrap();
    // This saved proposal predates the material/group contracts. Construct the
    // historical capture before persistence; no live paid request is rewritten.
    legacy_first_pass_job(&mut baseline, &original_job, true);
    let response = json!({"text":"Reviewed","sources":[],"assessments":[{"itemId":"i","outcome":"reply",
        "reason":"Evidence supports this reply"}],"proposals":[{"itemId":"i","kind":"reply_and_close","text":"Saved reply"}]});
    crate::auto_prepare::complete(&mut baseline, &original_job, &response, NOW).unwrap();
    crate::row_mut(&mut baseline, "jobs", &original_job).unwrap()["status"] = json!("completed");
    baseline["materials"].as_array_mut().unwrap().push(json!({"id":"new-evidence","kind":"transcript","postKey":"p","text":"New evidence"}));
    crate::auto_prepare::reconcile_stale(&mut baseline, NOW + 1);
    assert_eq!(baseline["proposals"][0]["status"], "stale");
    baseline["settings"]["autoPreparation"] = json!({"revalidation":{"enabled":true,"debounceSeconds":30}});
    let saved = baseline["proposals"][0].clone();
    initialize_claim_fixture_lifecycle(&mut baseline);
    let before = projection_of(&baseline).unwrap();
    let mut full = baseline.clone();
    let mut after = before.clone();
    assert!(crate::auto_prepare::claim(&mut full, NOW + 1).unwrap().is_none());
    assert!(crate::auto_prepare::claim(&mut after, NOW + 1).unwrap().is_none());
    assert_eq!(after["items"][0]["autoRevalidation"]["status"], "settling");
    let full_result = claim_revalidation_fixture_admitted(&mut full, NOW + 32).unwrap();
    let projected_result = claim_revalidation_fixture_admitted(&mut after, NOW + 32).unwrap();
    assert!(full_result.is_some() && projected_result.is_some());
    assert_eq!(full_result.as_ref().unwrap().1, projected_result.as_ref().unwrap().1);
    validate_claim_change(&before, &after).unwrap();
    let old_job_count = crate::list(&baseline, "jobs").len();
    let mut merged = baseline;
    merge_claim_delta(&mut merged, &before, &after).unwrap();
    assert_eq!(merged["proposals"][0], saved);
    for doc in [&mut full, &mut merged] {
        doc["jobs"].as_array_mut().unwrap()[old_job_count]["createdAt"] = json!("<claim-time>");
        normalize_ids(doc);
    }
    assert_eq!(full, merged);
    assert_eq!(merged["jobs"].as_array().unwrap().last().unwrap()["purpose"], "auto_revalidate");
}

#[test]
fn rejects_authority_source_and_unexpected_job_mutations() {
    let before = projection_of(&fixture()).unwrap();
    let mut after = before.clone();
    after["audit"] = json!([{"id":"new","action":"unexpected","refId":"i"}]);
    assert!(validate_claim_change(&before, &after).is_err());
    let mut after = before.clone();
    after["items"][0]["draft"] = json!("hidden edit");
    assert!(validate_claim_change(&before, &after).is_err());
    let mut after = before.clone();
    after["jobs"].as_array_mut().unwrap().push(json!({"id":"new","kind":"sync","status":"running"}));
    assert!(validate_claim_change(&before, &after).is_err());
}

fn first_pass_result() -> Value {
    json!({"text":"Selected comment needs an operator","sources":[],
        "assessments":[{"itemId":"i","outcome":"needs_attention","reason":"Source evidence is incomplete"}],
        "proposals":[]})
}

fn checkpoint_hash(value:&Value)->String {
    use sha2::{Digest,Sha256};
    format!("{:x}",Sha256::digest(value.to_string().as_bytes()))
}

fn legacy_first_pass_job(d:&mut Value,job:&str,full_batch:bool){
    let record=crate::row_mut(d,"jobs",job).unwrap();
    // Construct a pre-reservation historical job before fixture persistence.
    // A live saved paid reservation/capture must never be rewritten. These
    // sparse results exercise recovery of genuine pre-contract captures, not
    // admission of a fresh mandatory-material invocation without its receipt.
    record.as_object_mut().unwrap().remove("scopeReservation");
    if full_batch {record["preparationStages"].as_object_mut().unwrap().remove("groupAdmission");}
    let bundle=&mut record["prepareBundle"];
    let request=bundle["request"].as_object_mut().unwrap();
    for field in ["preparationMode","responseContract","modelContextContract",
        "researchPolicy","researchLimitContract","recoveryEvidenceContract",
        "factDependencyContract","visualNeedContract","visualSelection",
        "strictGroupContract","strictGroup","mandatoryMaterialContract",
        "postContextBundle","materialReadiness"] {request.remove(field);}
    bundle["digest"]=json!(checkpoint_hash(&bundle["request"]));
}

#[test]
fn fresh_first_capture_rejects_sparse_legacy_result_without_mutation(){
    let mut d=fixture();
    crate::accounts::initialize(&mut d,crate::accounts::Profile::LikeAvto).unwrap();
    let (job,_)=crate::auto_prepare::claim(&mut d,NOW).unwrap().unwrap();
    assert_eq!(crate::row(&d,"jobs",&job).unwrap()["prepareBundle"]["request"]["mandatoryMaterialContract"],
        crate::preparation_materials::CONTRACT);
    let before=d.clone();
    let err=crate::preparation_review::record_first(&mut d,&job,&first_pass_result(),"2026-09-24T12:00:00Z").unwrap_err();
    assert_eq!(err.1,"mandatory_material_invocation_missing_or_incomplete");
    assert_eq!(d,before,"a new capture requires its native material receipt before any stage changes");
}

fn review_checkpoint_fixture()->(Value,String,Value) {
    let mut d=fixture();
    crate::accounts::initialize(&mut d,crate::accounts::Profile::LikeAvto).unwrap();
    for table in ["knowledge_entries","knowledge_versions","feedback"] {d[table]=json!([]);}
    let (job,_)=crate::auto_prepare::claim(&mut d,NOW).unwrap().unwrap();
    legacy_first_pass_job(&mut d,&job,true);
    crate::preparation_review::record_first(&mut d,&job,&first_pass_result(),"2026-09-24T12:00:00Z").unwrap();
    let record=crate::row(&d,"jobs",&job).unwrap();
    let bundle=&record["prepareBundle"];
    let request=crate::preparation_review::plan_review(&bundle["request"],
        &record["preparationStages"]["first"]["result"]).unwrap().unwrap();
    let mut profile=json!({"version":1,"account":"likeavto","model":"gpt-6-astra","reasoningEffort":"medium",
        "promptVersion":"review-fixture","instructionSha256":"b".repeat(64),"toolsProfileSha256":"c".repeat(64),
        "runtimeSha256":"d".repeat(64),"cliSha256":"e".repeat(64)});
    profile["profileSha256"]=json!(checkpoint_hash(&profile));
    let ids:Vec<Value>=request["items"].as_array().unwrap().iter().map(|i|i["id"].clone()).collect();
    let mut state=json!({"version":1,"bundleId":bundle["id"],"bundleDigest":bundle["digest"],
        "requestDigest":checkpoint_hash(&request),"connectorBinding":bundle["request"]["connectorBinding"],
        "profile":profile,"chunks":[{"id":"chunk-1","itemIds":ids,"attempts":[],"result":null}],
        "maxWebCalls":8,"maxInputBytes":((request.to_string().len() as u64+2048)*4).min(2_400_000),
        "status":"pending","automaticResume":false});
    state["planDigest"]=json!(checkpoint_hash(&state));
    (d,job,state)
}

fn reserved_review_checkpoint(mut state:Value,limit:u64)->Value {
    let request_hash="f".repeat(64);
    let contract=json!({"version":1,"attemptId":"attempt-1","chunkId":"chunk-1",
        "profileSha256":state["profile"]["profileSha256"],"requestSha256":request_hash,"maxWebCalls":limit});
    state["chunks"][0]["attempts"]=json!([{"id":"attempt-1","status":"running",
        "contract":contract,"requestDigest":request_hash,"inputBytes":10,
        "reservedWebCalls":limit,"observedWebCalls":null}]);
    state["status"]=json!("running");
    state["usage"]=json!({"chargedWebCalls":limit,"observedCompletedWebCalls":0,
        "reservedUnconfirmedWebCalls":limit,"unknownAttemptCount":0,"inputBytes":10,
        "webBudget":state["maxWebCalls"],"actualTotalWebCallsKnown":false,
        "enforcement":"durable_reservation_and_observed_event_rejection"});
    state
}

#[test]
fn review_checkpoint_accepts_exact_legacy_and_adaptive_reservations(){
    for limit in [2,4,8] {
        let (full,job,state)=review_checkpoint_fixture();
        let before=projection_for(&full,Some(&job)).unwrap();
        let mut initialized=before.clone();
        crate::row_mut(&mut initialized,"jobs",&job).unwrap()["preparationStages"]["reviewChunks"]=state.clone();
        validate_review_checkpoint_change(&before,&initialized,&job).unwrap();
        let mut reserved=initialized.clone();
        crate::row_mut(&mut reserved,"jobs",&job).unwrap()["preparationStages"]["reviewChunks"]=
            reserved_review_checkpoint(state,limit);
        validate_review_checkpoint_change(&initialized,&reserved,&job).unwrap();
        let attempt=&crate::row(&reserved,"jobs",&job).unwrap()["preparationStages"]["reviewChunks"]["chunks"][0]["attempts"][0];
        assert_eq!(attempt["reservedWebCalls"],limit);
        assert_eq!(attempt["contract"]["maxWebCalls"],limit);
    }
}

#[test]
fn review_checkpoint_rejects_over_budget_or_mismatched_wire_grants(){
    let (full,job,state)=review_checkpoint_fixture();
    let mut before=projection_for(&full,Some(&job)).unwrap();
    crate::row_mut(&mut before,"jobs",&job).unwrap()["preparationStages"]["reviewChunks"]=state.clone();
    for limit in [0,9] {
        let mut after=before.clone();
        crate::row_mut(&mut after,"jobs",&job).unwrap()["preparationStages"]["reviewChunks"]=
            reserved_review_checkpoint(state.clone(),limit);
        assert_eq!(validate_review_checkpoint_change(&before,&after,&job).unwrap_err().1,
            "Review web reservation invalid","reserve {limit}");
    }
    let mut fractional=before.clone();
    let stage=&mut crate::row_mut(&mut fractional,"jobs",&job).unwrap()["preparationStages"]["reviewChunks"];
    *stage=reserved_review_checkpoint(state.clone(),4);stage["chunks"][0]["attempts"][0]["reservedWebCalls"]=json!(3.5);
    assert_eq!(validate_review_checkpoint_change(&before,&fractional,&job).unwrap_err().1,
        "Review web reservation invalid");
    for field in ["maxWebCalls","attemptId","chunkId","profileSha256","requestSha256"] {
        let mut after=before.clone();
        let stage=&mut crate::row_mut(&mut after,"jobs",&job).unwrap()["preparationStages"]["reviewChunks"];
        *stage=reserved_review_checkpoint(state.clone(),4);
        stage["chunks"][0]["attempts"][0]["contract"][field]=match field {
            "maxWebCalls"=>json!(2),"profileSha256"|"requestSha256"=>json!("0".repeat(64)),
            _=>json!("other"),
        };
        assert_eq!(validate_review_checkpoint_change(&before,&after,&job).unwrap_err().1,
            "Review web reservation differs from immutable contract","contract {field}");
    }
    let mut spent=before.clone();
    {
        let stage=&mut crate::row_mut(&mut spent,"jobs",&job).unwrap()["preparationStages"]["reviewChunks"];
        *stage=reserved_review_checkpoint(state,8);
        stage["chunks"][0]["attempts"][0]["status"]=json!("unknown");
        stage["chunks"][0]["attempts"][0]["finishedAt"]=json!("2026-09-24T12:01:00Z");
        stage["chunks"][0]["attempts"][0]["errorCode"]=json!("ADAPTER_TIMEOUT");
        stage["chunks"][0]["attempts"][0]["retryable"]=json!(true);
        stage["status"]=json!("held");stage["usage"]["unknownAttemptCount"]=json!(1);
    }
    validate_review_checkpoint_change(&before,&spent,&job).unwrap();
    let mut over=spent.clone();
    {
        let stage=&mut crate::row_mut(&mut over,"jobs",&job).unwrap()["preparationStages"]["reviewChunks"];
        let mut extra=stage["chunks"][0]["attempts"][0].clone();
        extra["id"]=json!("attempt-2");extra["contract"]["attemptId"]=json!("attempt-2");
        extra["status"]=json!("running");extra["reservedWebCalls"]=json!(1);extra["contract"]["maxWebCalls"]=json!(1);
        for key in ["finishedAt","errorCode","retryable"] {extra.as_object_mut().unwrap().remove(key);}
        stage["chunks"][0]["attempts"].as_array_mut().unwrap().push(extra);
        stage["status"]=json!("running");
        stage["usage"]["chargedWebCalls"]=json!(9);
        stage["usage"]["reservedUnconfirmedWebCalls"]=json!(9);
        stage["usage"]["inputBytes"]=json!(20);
    }
    assert_eq!(validate_review_checkpoint_change(&spent,&over,&job).unwrap_err().1,
        "Review checkpoint recipients or budget changed","aggregate nine must fail after a valid spent checkpoint");
}

#[test]
fn review_checkpoint_unknown_keeps_original_two_call_reservation(){
    let (full,job,state)=review_checkpoint_fixture();
    let mut before=projection_for(&full,Some(&job)).unwrap();
    crate::row_mut(&mut before,"jobs",&job).unwrap()["preparationStages"]["reviewChunks"]=
        reserved_review_checkpoint(state,2);
    let original=crate::row(&before,"jobs",&job).unwrap()["preparationStages"]["reviewChunks"]["chunks"][0]["attempts"][0]["contract"].clone();
    let mut after=before.clone();
    {
        let stage=&mut crate::row_mut(&mut after,"jobs",&job).unwrap()["preparationStages"]["reviewChunks"];
        stage["chunks"][0]["attempts"][0]["status"]=json!("unknown");
        stage["chunks"][0]["attempts"][0]["finishedAt"]=json!("2026-09-24T12:01:00Z");
        stage["chunks"][0]["attempts"][0]["errorCode"]=json!("ADAPTER_TIMEOUT");
        stage["chunks"][0]["attempts"][0]["retryable"]=json!(true);
        stage["status"]=json!("held");stage["usage"]["unknownAttemptCount"]=json!(1);
    }
    validate_review_checkpoint_change(&before,&after,&job).unwrap();
    let stage=&crate::row(&after,"jobs",&job).unwrap()["preparationStages"]["reviewChunks"];
    assert_eq!(stage["chunks"][0]["attempts"][0]["contract"],original);
    assert_eq!(stage["usage"]["chargedWebCalls"],2);
}

#[test]
fn review_checkpoint_projection_updates_only_target_and_preserves_other_jobs() {
    let (mut full,job,state)=review_checkpoint_fixture();
    full["jobs"].as_array_mut().unwrap().push(json!({"id":"other-active","kind":"assistant",
        "purpose":"engine_prepare","status":"queued","private":"keep"}));
    let original=full.clone();
    let before=projection_for(&full,Some(&job)).unwrap();
    let mut after=before.clone();
    crate::row_mut(&mut after,"jobs",&job).unwrap()["preparationStages"]["reviewChunks"]=state;
    validate_review_checkpoint_change(&before,&after,&job).unwrap();
    let mut merged=original;
    merge_claim_delta(&mut merged,&before,&after).unwrap();
    crate::row_mut(&mut full,"jobs",&job).unwrap()["preparationStages"]["reviewChunks"]=
        crate::row(&after,"jobs",&job).unwrap()["preparationStages"]["reviewChunks"].clone();
    assert_eq!(merged,full);
    assert_eq!(crate::row(&merged,"jobs","other-active").unwrap()["private"],"keep");
    let mut foreign=after.clone();
    crate::row_mut(&mut foreign,"jobs","other-active").unwrap()["status"]=json!("failed");
    assert!(validate_review_checkpoint_change(&before,&foreign,&job).is_err());
    let mut budget=after.clone();
    crate::row_mut(&mut budget,"jobs",&job).unwrap()["preparationStages"]["reviewChunks"]["maxWebCalls"]=json!(9);
    assert!(validate_review_checkpoint_change(&before,&budget,&job).is_err());
    let mut rewritten=after;
    crate::row_mut(&mut rewritten,"jobs",&job).unwrap()["preparationStages"]["first"]["result"]["text"]=json!("changed");
    assert!(validate_review_checkpoint_change(&before,&rewritten,&job).is_err());
}

#[tokio::test]
async fn sqlite_review_checkpoint_replay_and_stale_source_roll_back() {
    let (initial,job,state)=review_checkpoint_fixture();
    let folder=tempfile::tempdir().unwrap();
    let db=Database::Sqlite(crate::open_db(&folder.path().join("workspace.sqlite")).await.unwrap());
    db.change(|d|{*d=initial;Ok(())}).await.unwrap();
    let (_,changed)=db.change_preparation_review_checkpoint_observed(&job,|d| {
        crate::preparation_review::chunks::current(d,crate::row(d,"jobs",&job)?)?;
        crate::row_mut(d,"jobs",&job)?["preparationStages"]["reviewChunks"]=state.clone();
        Ok(())
    }).await.unwrap();
    assert!(changed);
    let saved=db.read().await.unwrap();
    let (_,changed)=db.change_preparation_review_checkpoint_observed(&job,|d| {
        assert_eq!(crate::row(d,"jobs",&job)?["preparationStages"]["reviewChunks"],state);
        Ok(())
    }).await.unwrap();
    assert!(!changed);assert_eq!(db.read().await.unwrap(),saved);
    db.change(|d|{d["branches"][0]["messages"][0]["text"]=json!("Changed source");Ok(())}).await.unwrap();
    let before=db.read().await.unwrap();
    assert!(db.change_preparation_review_checkpoint_observed(&job,|d| {
        crate::preparation_review::chunks::current(d,crate::row(d,"jobs",&job)?)?;
        crate::row_mut(d,"jobs",&job)?["preparationStages"]["reviewChunks"]["status"]=json!("running");
        Ok(())
    }).await.is_err());
    assert_eq!(db.read().await.unwrap(),before);
    db.close().await;
}

#[tokio::test]
async fn sqlite_review_checkpoint_persists_two_four_and_eight_call_contracts(){
    for limit in [2,4,8] {
        let (initial,job,state)=review_checkpoint_fixture();
        let folder=tempfile::tempdir().unwrap();
        let db=Database::Sqlite(crate::open_db(&folder.path().join("review-budget.sqlite")).await.unwrap());
        db.change(|d|{*d=initial;Ok(())}).await.unwrap();
        db.change_preparation_review_checkpoint_observed(&job,|d| {
            crate::row_mut(d,"jobs",&job)?["preparationStages"]["reviewChunks"]=state.clone();Ok(())
        }).await.unwrap();
        let reserved=reserved_review_checkpoint(state,limit);
        let (_,changed)=db.change_preparation_review_checkpoint_observed(&job,|d| {
            crate::row_mut(d,"jobs",&job)?["preparationStages"]["reviewChunks"]=reserved.clone();Ok(())
        }).await.unwrap();
        assert!(changed);
        let saved=db.read().await.unwrap();
        assert_eq!(crate::row(&saved,"jobs",&job).unwrap()["preparationStages"]["reviewChunks"],reserved);
        let rejection=db.change_preparation_review_checkpoint_observed(&job,|d| {
            let stage=&mut crate::row_mut(d,"jobs",&job)?["preparationStages"]["reviewChunks"];
            stage["chunks"][0]["attempts"][0]["status"]=json!("completed");
            stage["chunks"][0]["attempts"][0]["observedWebCalls"]=json!(limit+1);
            stage["usage"]["chargedWebCalls"]=json!(limit+1);
            stage["usage"]["observedCompletedWebCalls"]=json!(limit+1);
            stage["usage"]["reservedUnconfirmedWebCalls"]=json!(0);
            stage["usage"]["actualTotalWebCallsKnown"]=json!(true);
            Ok(())
        }).await.unwrap_err();
        assert_eq!(rejection.1,"Review observed web use invalid",
            "observed use cannot exceed immutable {limit}-call grant");
        assert_eq!(db.read().await.unwrap(),saved,"rejected checkpoint remains unchanged");
        db.close().await;
    }
}

#[test]
fn first_pass_projection_matches_full_write_and_preserves_unrelated_history() {
    let mut original = fixture();
    let (job, _) = crate::auto_prepare::claim(&mut original, NOW).unwrap().unwrap();
    legacy_first_pass_job(&mut original,&job,false);
    original["jobs"].as_array_mut().unwrap().push(json!({"id":"historical-auto","kind":"assistant",
        "purpose":"auto_prepare","status":"completed","result":{"largePrivateHistory":"x".repeat(1000)}}));
    original["jobs"].as_array_mut().unwrap().push(json!({"id":"active-other","kind":"assistant",
        "purpose":"engine_prepare","status":"queued"}));
    original["jobs"].as_array_mut().unwrap().push(json!({"id":"media-other","kind":"media","status":"completed"}));
    let before = projection_for(&original,Some(&job)).unwrap();
    assert!(crate::row(&before,"jobs","historical-auto").is_err());
    assert!(crate::row(&before,"jobs","active-other").is_ok());
    assert!(crate::row(&before,"jobs","media-other").is_ok());
    let mut full = original.clone();
    let mut scoped = before.clone();
    let result = first_pass_result();
    let full_plan = crate::preparation_review::record_first(&mut full, &job, &result, "2026-09-24T12:00:00Z").unwrap();
    let scoped_plan = crate::preparation_review::record_first(&mut scoped, &job, &result, "2026-09-24T12:00:00Z").unwrap();
    assert_eq!(full_plan, scoped_plan);
    validate_first_stage_change(&before, &scoped, &job).unwrap();
    let mut merged = original;
    merge_claim_delta(&mut merged, &before, &scoped).unwrap();
    assert_eq!(merged, full);
    for table in OMITTED { assert_eq!(merged.get(*table), full.get(*table)); }
    let replay = scoped.clone();
    crate::preparation_review::record_first(&mut scoped, &job, &result, "2026-09-24T12:00:01Z").unwrap();
    assert_eq!(scoped, replay, "identical first pass is idempotent");
    validate_first_stage_change(&replay, &scoped, &job).unwrap();
}

#[test]
fn first_pass_scope_rejects_other_jobs_source_and_rewrites() {
    let mut workspace = fixture();
    let (job, _) = crate::auto_prepare::claim(&mut workspace, NOW).unwrap().unwrap();
    legacy_first_pass_job(&mut workspace,&job,false);
    let before = projection_for(&workspace,Some(&job)).unwrap();
    let mut after = before.clone();
    crate::preparation_review::record_first(&mut after, &job, &first_pass_result(), "2026-09-24T12:00:00Z").unwrap();
    for change in ["other_job", "item", "research", "status", "review"] {
        let mut invalid = after.clone();
        match change {
            "other_job" => invalid["jobs"][0]["status"] = json!("failed"),
            "item" => invalid["items"][0]["draft"] = json!("unapproved edit"),
            "research" => invalid["preparationResearch"][0]["trust"] = json!("verified"),
            "status" => crate::row_mut(&mut invalid, "jobs", &job).unwrap()["status"] = json!("completed"),
            _ => crate::row_mut(&mut invalid, "jobs", &job).unwrap()["preparationStages"]["review"] = json!({"status":"completed"}),
        }
        assert!(validate_first_stage_change(&before, &invalid, &job).is_err(), "{change}");
    }
    assert!(validate_first_stage_change(&after, &before, &job).is_err(), "first evidence cannot be removed");
}

#[tokio::test]
async fn sqlite_first_pass_stale_source_rolls_back_and_replay_is_idempotent() {
    let folder = tempfile::tempdir().unwrap();
    let db = Database::Sqlite(crate::open_db(&folder.path().join("workspace.sqlite")).await.unwrap());
    db.change(|d| {
        *d = fixture();
        for table in ["knowledge_entries", "knowledge_versions", "feedback"] {
            d[table] = json!([]);
        }
        Ok(())
    }).await.unwrap();
    let (job, _) = db.change(|d| {
        let claimed=crate::auto_prepare::claim(d,NOW)?;
        if let Some((job,_))=&claimed {legacy_first_pass_job(d,job,false);}
        Ok(claimed)
    }).await.unwrap().unwrap();
    let result = first_pass_result();
    let (_, changed) = db.change_preparation_first_observed(&job, |d| {
        let bundle = crate::row(d, "jobs", &job)?["prepareBundle"].clone();
        crate::prepare_bundle::current(d, &bundle).map_err(crate::conflict)?;
        crate::preparation_review::record_first(d, &job, &result, "2026-09-24T12:00:00Z")
    }).await.unwrap();
    assert!(changed);
    let (_, changed) = db.change_preparation_first_observed(&job, |d| {
        crate::preparation_review::record_first(d, &job, &result, "2026-09-24T12:00:01Z")
    }).await.unwrap();
    assert!(!changed);
    db.change(|d| { d["branches"][0]["messages"][0]["text"] = json!("Changed source"); Ok(()) }).await.unwrap();
    let before = db.read().await.unwrap();
    assert!(db.change_preparation_first_observed(&job, |d| {
        let bundle = crate::row(d, "jobs", &job)?["prepareBundle"].clone();
        crate::prepare_bundle::current(d, &bundle).map_err(crate::conflict)?;
        crate::preparation_review::record_first(d, &job, &result, "2026-09-24T12:00:02Z")
    }).await.is_err());
    assert_eq!(db.read().await.unwrap(), before);
    db.close().await;
}

#[tokio::test]
#[ignore = "requires the explicitly isolated assistant scope PostgreSQL clone"]
async fn postgres_preparation_scope_clone_probe() {
    let url = std::env::var("COMMUNITYHERO_ASSISTANT_SCOPE_TEST_URL")
        .expect("explicit isolated PostgreSQL clone URL");
    assert!(url.starts_with("postgresql://") && url.contains("@127.0.0.1:"));
    assert!(url.contains("/communityhero_assistant_scope_test_remediation_20260923"));
    let db = Database::postgres(&url).await.unwrap();
    if let Database::Postgres { writer, .. } = &db {
        let database: String = sqlx::query_scalar("SELECT current_database()")
            .fetch_one(writer).await.unwrap();
        assert_eq!(database, "communityhero_assistant_scope_test_remediation_20260923");
    }
    let started = Instant::now();
    let full = db.read().await.unwrap();
    let full_ms = started.elapsed().as_secs_f64() * 1000.0;
    let expected = projection_of(&full).unwrap();
    let started = Instant::now();
    let (_, changed) = db.change_preparation_claim_observed(|scoped| {
        if *scoped != expected { return Err(internal("Preparation SQL projection differs from full workspace")); }
        Ok(())
    }).await.unwrap();
    let scoped_ms = started.elapsed().as_secs_f64() * 1000.0;
    assert!(!changed);
    eprintln!("preparation PG clone no-op: full read {full_ms:.2} ms; scoped transaction {scoped_ms:.2} ms; full bytes {}; scoped bytes {}",
        full.to_string().len(), expected.to_string().len());
    // This clone is disposable. Stage one fresh, non-video item after the
    // read-only probe. Do not alter the source/production database or launch
    // a model; the closure below calls claim only and has no worker spawn.
    let probe = format!("preparation-probe-{}", uuid::Uuid::new_v4());
    let post_id = format!("{probe}-post");
    let branch_id = format!("{probe}-branch");
    let item_id = format!("{probe}-item");
    let now = chrono::Utc::now().timestamp();
    let observed = chrono::DateTime::from_timestamp(now, 0).unwrap().to_rfc3339();
    db.change(|workspace| {
        if workspace["account"] != "LikeAvto" { return Err(internal("Probe clone is not LikeAvto")); }
        for job in crate::list_mut(workspace, "jobs") {
            if job["kind"] == "assistant" && job["purpose"] != "discussion"
                && matches!(job["status"].as_str(), Some("running" | "queued")) {
                job["status"] = json!("interrupted");
            }
        }
        crate::list_mut(workspace, "posts").push(json!({"id":post_id,"postKey":probe,"text":"Synthetic preparation probe"}));
        crate::list_mut(workspace, "branches").push(json!({"id":branch_id,"postId":post_id,
            "messages":[{"id":format!("{probe}-comment"),"text":"Synthetic question"}],"contextComplete":false}));
        crate::list_mut(workspace, "items").push(json!({"id":item_id,"itemId":format!("{probe}-comment"),
            "objectId":format!("{probe}-object"),"postKey":probe,"conversationKey":probe,
            "branchId":branch_id,"postId":post_id,"revision":1,"draft":"","workflow":"attention",
            "providerStatus":"new","createdAt":"2000-01-01T00:00:00Z","providerObservedAt":observed}));
        crate::media_queue::reconcile(workspace, &observed)?;
        Ok(())
    }).await.unwrap();
    let baseline = db.read().await.unwrap();
    let mut expected_after = baseline.clone();
    let expected_result = crate::auto_prepare::claim(&mut expected_after, now).unwrap();
    assert_eq!(expected_result.as_ref().map(|(_, request)| request["items"][0]["id"].as_str()),
        Some(Some(item_id.as_str())));
    let started = Instant::now();
    let (actual_result, changed) = db.change_preparation_claim_observed(|scoped| {
        crate::auto_prepare::claim(scoped, now)
    }).await.unwrap();
    let write_ms = started.elapsed().as_secs_f64() * 1000.0;
    assert!(changed);
    assert_eq!(actual_result.as_ref().map(|(_, request)| request["items"][0]["id"].as_str()),
        Some(Some(item_id.as_str())));
    let mut actual_after = db.read().await.unwrap();
    for table in OMITTED {
        assert!(actual_after[*table] == baseline[*table], "Excluded history changed: {table}");
    }
    let old_job_count = crate::list(&baseline, "jobs").len();
    for doc in [&mut expected_after, &mut actual_after] {
        for job in &mut doc["jobs"].as_array_mut().unwrap()[old_job_count..] {
            job["createdAt"] = json!("<claim-time>");
        }
        normalize_ids(doc);
    }
    assert!(actual_after == expected_after, "Persisted claim differs from full-document claim");
    assert!(crate::list(&actual_after, "jobs").iter().any(|j| j["id"] == "sync-old")
        || crate::list(&actual_after, "jobs").len() >= old_job_count + 1);
    eprintln!("preparation PG clone real claim: scoped write {write_ms:.2} ms; appended assistant job and updated item; excluded history unchanged");
    db.close().await;
}

#[tokio::test]
#[ignore = "requires a pristine explicitly isolated communityhero_writer_v51_test_ PostgreSQL fixture"]
async fn postgres_stale_settlement_parity_and_forged_refund_rollback() {
    let db=writer_v51_fixture_db().await;
    crate::preparation_review::chunks::exercise_stale_settlement_storage(&db).await;
    eprintln!("W1_SETTLEMENT_PG staleResultRetained=true forgedRefundRollback=true fullDomainParity=true");
    db.close().await;
}

#[tokio::test]
#[ignore = "requires a pristine explicitly isolated communityhero_writer_v51_test_ PostgreSQL fixture"]
async fn postgres_completed_recovery_atomic_admission_and_replay() {
    let db=writer_v51_fixture_db().await;
    crate::preparation_review::chunks::exercise_completed_recovery_storage(&db,true).await;
    db.close().await;
}

#[tokio::test]
#[ignore = "requires a pristine explicitly isolated communityhero_writer_v51_test_ PostgreSQL fixture"]
async fn postgres_completed_engine_recovery_atomic_admission_and_replay() {
    let db=writer_v51_fixture_db().await;
    crate::preparation_review::chunks::exercise_completed_recovery_storage(&db,false).await;
    db.close().await;
}

#[tokio::test]
#[ignore = "requires the explicitly isolated assistant scope PostgreSQL clone"]
async fn postgres_first_pass_scope_clone_probe() {
    first_pass_clone_probe(true).await;
}

#[tokio::test]
#[ignore = "requires the explicitly isolated assistant scope PostgreSQL clone"]
async fn postgres_full_first_pass_clone_probe() {
    first_pass_clone_probe(false).await;
}

async fn first_pass_clone_probe(scoped_write:bool) {
    let url=std::env::var("COMMUNITYHERO_ASSISTANT_SCOPE_TEST_URL")
        .expect("explicit isolated PostgreSQL clone URL");
    assert!(url.starts_with("postgresql://") && url.contains("@127.0.0.1:"));
    assert!(url.contains("/communityhero_assistant_scope_test_remediation_20260923"));
    let db=Database::postgres(&url).await.unwrap();
    if let Database::Postgres{writer,..}=&db {
        let database:String=sqlx::query_scalar("SELECT current_database()").fetch_one(writer).await.unwrap();
        assert_eq!(database,"communityhero_assistant_scope_test_remediation_20260923");
    }
    let probe=format!("first-pass-probe-{}",uuid::Uuid::new_v4());
    let post_id=format!("{probe}-post");
    let branch_id=format!("{probe}-branch");
    let item_id=format!("{probe}-item");
    let now=chrono::Utc::now().timestamp();
    let observed=chrono::DateTime::from_timestamp(now,0).unwrap().to_rfc3339();
    let job_id=db.change(|d| {
        if d["account"]!="LikeAvto" {return Err(internal("Probe clone is not LikeAvto"));}
        for job in crate::list_mut(d,"jobs") {
            if job["kind"]=="assistant" && job["purpose"]!="discussion"
                && matches!(job["status"].as_str(),Some("running"|"queued")) {
                job["status"]=json!("interrupted");
            }
        }
        crate::list_mut(d,"posts").push(json!({"id":post_id,"postKey":probe,"text":"Synthetic first-pass probe"}));
        crate::list_mut(d,"branches").push(json!({"id":branch_id,"postId":post_id,
            "messages":[{"id":format!("{probe}-comment"),"text":"Synthetic question"}],"contextComplete":false}));
        crate::list_mut(d,"items").push(json!({"id":item_id,"itemId":format!("{probe}-comment"),
            "objectId":format!("{probe}-object"),"postKey":probe,"conversationKey":probe,
            "branchId":branch_id,"postId":post_id,"revision":1,"draft":"","workflow":"attention",
            "providerStatus":"new","createdAt":"2000-01-01T00:00:00Z","providerObservedAt":observed}));
        crate::media_queue::reconcile(d,&observed)?;
        let (job,_)=crate::auto_prepare::claim(d,now)?.ok_or_else(||internal("Synthetic first-pass claim unavailable"))?;
        legacy_first_pass_job(d,&job,false);
        if crate::row(d,"jobs",&job)?["refId"]!=item_id {return Err(internal("First-pass probe claimed another item"));}
        Ok(job)
    }).await.unwrap();
    let before=db.read().await.unwrap();
    let projection=projection_for(&before,Some(&job_id)).unwrap();
    assert!(crate::row(&projection,"jobs",&job_id).is_ok());
    assert!(crate::list(&projection,"jobs").iter().all(|j|
        j["kind"]=="media" || j["id"]==job_id
            || (j["kind"]=="assistant"&&matches!(j["status"].as_str(),Some("running"|"queued")))));
    let full_bytes=before.to_string().len();let scoped_bytes=projection.to_string().len();
    assert!(scoped_bytes<full_bytes,"historical jobs must be excluded from first-pass SQL projection");

    let started=Instant::now();
    let (_,changed)=if scoped_write {
        db.change_preparation_first_observed(&job_id,|d| {
            if *d!=projection {return Err(internal("First-pass SQL projection differs from full workspace"));}
            Ok(())
        }).await.unwrap()
    }else{
        db.change_observed(|d| {
            if *d!=before {return Err(internal("Full first-pass probe fixture changed"));}
            Ok(())
        }).await.unwrap()
    };
    let no_op_ms=started.elapsed().as_secs_f64()*1000.0;
    assert!(!changed);
    let result=json!({"text":"Operator review needed","sources":[],
        "assessments":[{"itemId":item_id,"outcome":"needs_attention","reason":"Synthetic question needs review"}],
        "proposals":[]});
    let at="2026-09-24T12:00:00Z";
    let mut expected=before.clone();
    let bundle=crate::row(&expected,"jobs",&job_id).unwrap()["prepareBundle"].clone();
    crate::prepare_bundle::current(&expected,&bundle).unwrap();
    let expected_plan=crate::preparation_review::record_first(&mut expected,&job_id,&result,at).unwrap();
    let started=Instant::now();
    let write_first=|d:&mut Value| {
        let bundle=crate::row(d,"jobs",&job_id)?["prepareBundle"].clone();
        crate::prepare_bundle::current(d,&bundle).map_err(crate::conflict)?;
        crate::preparation_review::record_first(d,&job_id,&result,at)
    };
    let (actual_plan,changed)=if scoped_write {
        db.change_preparation_first_observed(&job_id,write_first).await.unwrap()
    }else{
        db.change_observed(write_first).await.unwrap()
    };
    let write_ms=started.elapsed().as_secs_f64()*1000.0;
    assert!(changed);assert_eq!(actual_plan,expected_plan);
    assert_eq!(db.read().await.unwrap(),expected,"first-pass SQL write must match full-workspace result");
    if scoped_write {
        let started=Instant::now();
        let (_,changed)=db.change_preparation_first_observed(&job_id,|d|
            crate::preparation_review::record_first(d,&job_id,&result,at)).await.unwrap();
        let replay_ms=started.elapsed().as_secs_f64()*1000.0;
        assert!(!changed,"replayed first pass cannot write a second stage");
        db.change(|d| {
            crate::row_mut(d,"branches",&branch_id)?["messages"][0]["text"]=json!("Changed synthetic source");
            Ok(())
        }).await.unwrap();
        let stale_before=db.read().await.unwrap();
        assert!(db.change_preparation_first_observed(&job_id,|d| {
            let bundle=crate::row(d,"jobs",&job_id)?["prepareBundle"].clone();
            crate::prepare_bundle::current(d,&bundle).map_err(crate::conflict)?;
            crate::preparation_review::record_first(d,&job_id,&result,at)
        }).await.is_err());
        assert_eq!(db.read().await.unwrap(),stale_before,"stale source rejection must roll back without touching history");
        eprintln!("first-pass PG clone scoped: full bytes {full_bytes}, scoped bytes {scoped_bytes}; no-op {no_op_ms:.2} ms, write {write_ms:.2} ms, replay {replay_ms:.2} ms; parity and rollback verified");
    }else{
        eprintln!("first-pass PG clone full: full bytes {full_bytes}, scoped reference bytes {scoped_bytes}; no-op {no_op_ms:.2} ms, write {write_ms:.2} ms; parity verified");
    }
    db.close().await;
}

#[tokio::test]
async fn sqlite_rolling_discovery_reuses_exact_claim_projection_and_full_operation_quarantine() {
    let folder=tempfile::tempdir().unwrap();
    let db=Database::Sqlite(crate::open_db(&folder.path().join("workspace.sqlite")).await.unwrap());
    let mut d=fixture();
    d["operations"]=json!([
        {"id":"unknown-hidden","itemId":"i","status":"unknown","target":{"connectorBinding":d["connectorBinding"]},
            "action":{"action":"reply_and_close","conversationKey":"hidden:thread"},"privateReceipt":{"observations":[null,true,"exact"]}},
        {"id":"retained-done","itemId":"i","status":"completed","history":{"raw":"retain"}}]);
    let Database::Sqlite(pool)=&db else {unreachable!()};
    sqlx::query("UPDATE workspace SET payload=? WHERE id=1").bind(d.to_string()).execute(pool).await.unwrap();
    let before=db.read().await.unwrap();
    let actual=db.read_preparation_discovery().await.unwrap();
    assert_eq!(actual,projection_of(&before).unwrap());
    assert_eq!(actual["operations"],before["operations"]);
    assert_eq!(actual["proposals"],before["proposals"]);
    assert_eq!(actual["materials"],before["materials"]);
    for name in OMITTED {assert!(actual.get(*name).is_none());}
    assert_eq!(db.read().await.unwrap(),before);
    db.close().await;
}

#[tokio::test]
async fn stale_discovery_cannot_hide_new_unknown_or_approved_proposal_at_native_claim() {
    for state in ["unknown","approved"] {
        let folder=tempfile::tempdir().unwrap();
        let db=Database::Sqlite(crate::open_db(&folder.path().join("workspace.sqlite")).await.unwrap());
        let d=fixture();
        let Database::Sqlite(pool)=&db else {unreachable!()};
        sqlx::query("UPDATE workspace SET payload=? WHERE id=1").bind(d.to_string()).execute(pool).await.unwrap();
        let mut old=db.read_preparation_discovery().await.unwrap();
        assert!(crate::auto_prepare::claim(&mut old,NOW).unwrap().is_some(),"must capture actually ready work before invalidation");
        let mut current=d;
        current["feedback"][0]["text"]=json!("Changed owner feedback after preview");
        current["approvals"][0]["ownerNote"]=json!("Changed private consent body after preview");
        if state=="unknown" {
            current["operations"]=json!([{"id":"new-unknown","itemId":"i","status":"unknown"}]);
        } else {
            current["proposals"]=json!([{"id":"new-approved","itemId":"i","status":"approved"}]);
        }
        sqlx::query("UPDATE workspace SET payload=? WHERE id=1").bind(current.to_string()).execute(pool).await.unwrap();
        let before=db.read().await.unwrap();
        let result=db.change_preparation_claim_observed(|fresh|crate::auto_prepare::claim(fresh,NOW)).await.unwrap();
        assert!(result.0.is_none(),"native current claim must see new quarantine/approval state");
        let after=db.read().await.unwrap();
        for name in ["approvals","feedback","operations","proposals"] {assert_eq!(after[name],before[name]);}
        assert_eq!(after["jobs"].as_array().unwrap().len(),before["jobs"].as_array().unwrap().len());
        // Discovery/claim schedules preparation only. It cannot introduce any
        // executable operation or treat omitted private consent as authority.
        assert_eq!(after["operations"],current["operations"]);
        db.close().await;
    }
}

#[tokio::test]
#[ignore="ROOT-only fresh isolated writer_v51 PostgreSQL fixture; run alone"]
async fn postgres_rolling_discovery_matches_exact_claim_and_counts_selected_materialization() {
    let db=writer_v51_fixture_db().await;
    db.change(|d|{
        *d=fixture();
        for table in ["knowledge_entries","knowledge_versions","feedback"] {d[table]=json!([]);}
        Ok(())
    }).await.unwrap();
    let before=db.read().await.unwrap();
    let (view,events)=crate::performance::capture(db.read_preparation_discovery()).await;
    let view=view.unwrap();
    assert_eq!(view,projection_of(&before).unwrap());
    assert_eq!(view["operations"],before["operations"]);
    let work=events.iter().find(|event|event["stage"]=="preparation.projection.materialized").unwrap();
    let expected_rows=TABLES.iter().filter(|table|!OMITTED.contains(table))
        .map(|table|crate::list(&view,table).len()).sum::<usize>();
    assert_eq!(work["work"]["rows"],expected_rows);
    assert_eq!(work["work"]["collections"],TABLES.len()-OMITTED.len());
    assert!(work["work"]["bytes"].as_u64().unwrap()>0);
    assert_eq!(db.read().await.unwrap(),before);
    db.close().await;
}
