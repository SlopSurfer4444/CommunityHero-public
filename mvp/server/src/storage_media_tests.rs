use super::*;
use std::time::Instant;

const AT:&str="2026-09-23T08:00:00Z";

fn fixture()->Value {
    let mut d=crate::empty();
    crate::accounts::initialize(&mut d,crate::accounts::Profile::LikeAvto).unwrap();
    d["posts"]=json!([{"id":"post-one","postKey":"11391:one","objectId":"11391",
        "title":"LikeAvto video","channel":"VK","sourceUrl":"https://vk.com/video-123_456",
        "attachments":[{"type":"video"}]}]);
    d["items"]=json!([{"id":"item-one","itemId":"one","objectId":"11391",
        "postId":"post-one","postKey":"11391:one","conversationKey":"11391:thread",
        "providerStatus":"new","workflow":"attention","draft":"Keep this operator edit",
        "contextEvidenceDigest":"large evidence excluded"}]);
    d["knowledge_entries"]=json!([]);
    d["knowledge_versions"]=json!([]);
    d["jobs"]=json!([{"id":"non-media-history","kind":"assistant","status":"completed",
        "prepareBundle":{"request":"do not clone this into media tick"}}]);
    d
}

#[test]
fn scoped_media_claim_matches_full_state_and_retains_history() {
    let mut full=fixture();
    crate::media_queue::reconcile(&mut full,AT).unwrap();
    let first_job=full["jobs"].as_array().unwrap().iter().find(|j|j["kind"]=="media").unwrap().clone();
    assert_eq!(first_job["status"],"queued");
    let baseline=full.clone();
    let mut scoped=media_projection(&baseline).unwrap();
    assert_eq!(scoped["mediaQueue"]["inputDigest"],baseline["mediaQueue"]["inputDigest"]);
    let claimed_full=crate::media_queue::claim_when_ready(&mut full,AT,true).unwrap().unwrap();
    let claimed_scoped=crate::media_queue::claim_when_ready(&mut scoped,AT,true).unwrap().unwrap();
    assert_eq!(claimed_full,claimed_scoped);
    validate_media_change(&media_projection(&baseline).unwrap(),&scoped).unwrap();
    let active=rows(&scoped,"jobs").unwrap().iter().find(|j|j["id"]==first_job["id"]).unwrap();
    assert_eq!(active["sourceAttempts"].as_array().unwrap().len(),1);
    assert_eq!(active["sourceAttempts"][0]["status"],"running");
    assert_eq!(scoped["mediaQueue"],full["mediaQueue"]);

    let before_media=rows(&scoped,"jobs").unwrap().len();
    assert!(crate::media_queue::claim_when_ready(&mut scoped,AT,true).unwrap().is_none());
    assert_eq!(rows(&scoped,"jobs").unwrap().len(),before_media);
    assert_eq!(scoped["jobs"].as_array().unwrap().iter().filter(|j|j["status"]=="running").count(),1);
    assert_eq!(baseline["jobs"][0]["id"],"non-media-history");
    assert_eq!(baseline["items"][0]["draft"],"Keep this operator edit");
}

#[test]
fn first_tick_claims_new_video_once_and_unready_noop_is_valid() {
    let full=fixture();
    let before=media_projection(&full).unwrap();
    let mut unready=before.clone();
    assert!(crate::media_queue::claim_when_ready(&mut unready,AT,false).unwrap().is_none());
    validate_media_change(&before,&unready).unwrap();
    assert_eq!(before,unready);
    let mut ready=before.clone();
    let first=crate::media_queue::claim_when_ready(&mut ready,AT,true).unwrap().unwrap();
    validate_media_change(&before,&ready).unwrap();
    assert_eq!(first.1["id"],"post-one");
    assert_eq!(ready["jobs"].as_array().unwrap().len(),1);
    assert_eq!(ready["jobs"][0]["status"],"running");
    assert_eq!(ready["jobs"][0]["sourceAttempts"].as_array().unwrap().len(),1);
    assert!(crate::media_queue::claim_when_ready(&mut ready,AT,true).unwrap().is_none());
    assert_eq!(ready["jobs"].as_array().unwrap().len(),1);
}

#[test]
fn legacy_media_job_without_attempt_array_stays_opaque_on_noop_tick() {
    let mut full=fixture();
    full["items"][0]["workflow"]=json!("closed");
    full["jobs"].as_array_mut().unwrap().push(json!({"id":"old-post-one","kind":"media",
        "refId":"post-one","status":"failed"}));
    let before=media_projection(&full).unwrap();
    let legacy=before["jobs"][0].clone();
    let mut after=before.clone();
    assert!(crate::media_queue::claim_when_ready(&mut after,AT,true).unwrap().is_none());
    validate_media_change(&before,&after).unwrap();
    assert_eq!(after["jobs"].as_array().unwrap().len(),1);
    assert_eq!(after["jobs"][0],legacy);
    assert!(after["jobs"][0].get("sourceAttempts").is_none());
}

#[test]
fn legacy_media_history_does_not_block_fresh_distinct_post_claim() {
    let mut full=fixture();
    full["jobs"].as_array_mut().unwrap().push(json!({"id":"old-post-one","kind":"media",
        "refId":"post-one","status":"failed"}));
    full["posts"].as_array_mut().unwrap().push(json!({"id":"post-two","postKey":"11391:two",
        "objectId":"11391","title":"Different video","channel":"VK",
        "sourceUrl":"https://vk.com/video-123_789","attachments":[{"type":"video"}]}));
    full["items"].as_array_mut().unwrap().push(json!({"id":"item-two","itemId":"two",
        "objectId":"11391","postId":"post-two","postKey":"11391:two",
        "conversationKey":"11391:thread-two","providerStatus":"new","workflow":"attention"}));
    let before=media_projection(&full).unwrap();
    let legacy=before["jobs"][0].clone();
    let mut after=before.clone();
    let (_,post)=crate::media_queue::claim_when_ready(&mut after,AT,true).unwrap().unwrap();
    validate_media_change(&before,&after).unwrap();
    assert_eq!(post["id"],"post-two");
    // The old source is consumed. Reconciliation may add a failed group
    // placeholder, but only the distinct untried post can acquire an attempt.
    assert_eq!(after["jobs"].as_array().unwrap().len(),3);
    assert_eq!(after["jobs"][0],legacy);
    assert_eq!(after["jobs"].as_array().unwrap().iter().filter(|j|j["status"]=="running").count(),1);
    assert_eq!(after["jobs"].as_array().unwrap().iter().filter(|j|j["status"]=="failed" && j["purpose"]=="auto_media").count(),1);
    let running=after["jobs"].as_array().unwrap().iter().find(|j|j["status"]=="running").unwrap();
    assert_eq!(running["sourceAttempts"].as_array().unwrap().len(),1);
    assert!(crate::media_queue::claim_when_ready(&mut after,AT,true).unwrap().is_none());
    assert_eq!(after["jobs"].as_array().unwrap().len(),3);
}

#[test]
fn new_comment_and_reused_transcript_are_seen_by_projection() {
    let mut full=fixture();
    crate::media_queue::reconcile(&mut full,AT).unwrap();
    let original_job=full["jobs"].as_array().unwrap().iter().find(|j|j["kind"]=="media").unwrap()["id"].clone();
    // A new provider item arrives after the previous media digest was stored.
    full["items"].as_array_mut().unwrap().push(json!({"id":"item-two","itemId":"two",
        "objectId":"11391","postId":"post-one","postKey":"11391:one",
        "conversationKey":"11391:thread","providerStatus":"new","workflow":"attention"}));
    let mut scoped=media_projection(&full).unwrap();
    crate::media_queue::reconcile(&mut scoped,AT).unwrap();
    assert_eq!(rows(&scoped,"jobs").unwrap().len(),1);
    assert_eq!(scoped["jobs"][0]["id"],original_job);

    // A sibling import supplies the transcript. Reconciliation completes the
    // existing queue job without discarding its previous attempt evidence.
    full["jobs"].as_array_mut().unwrap().iter_mut().find(|j|j["id"]==original_job).unwrap()["sourceAttempts"]
        =json!([{"id":"attempt-1","postId":"post-one","status":"interrupted","finishedAt":AT}]);
    full["jobs"].as_array_mut().unwrap().iter_mut().find(|j|j["id"]==original_job).unwrap()["status"]=json!("interrupted");
    full["materials"]=json!([{"id":"transcript-one","kind":"transcript","postKey":"11391:one",
        "text":"Verified transcript from another source"}]);
    let evidence=crate::media_fullframes::fixture_for_post("LikeAvto",&full["posts"][0]);
    let visual=json!({"id":"visual-one","kind":"visual_context","account":"LikeAvto","postKey":"11391:one",
        "sourceUrl":full["posts"][0]["sourceUrl"],"mediaSha256":evidence["source"]["mediaSha256"],"text":"Visual selected context","visualEvidence":evidence});
    full["materials"].as_array_mut().unwrap().push(visual);
    crate::knowledge::sync_catalog(&mut full,AT).unwrap();
    let before=media_projection(&full).unwrap();
    let mut after=before.clone();
    crate::media_queue::reconcile(&mut after,AT).unwrap();
    validate_media_change(&before,&after).unwrap();
    assert_eq!(after["jobs"][0]["status"],"completed");
    assert_eq!(after["jobs"][0]["sourceAttempts"],before["jobs"][0]["sourceAttempts"]);
}

#[test]
fn media_change_rejects_attempt_rewrite_or_unrelated_mutation() {
    let mut full=fixture();
    crate::media_queue::reconcile(&mut full,AT).unwrap();
    let before=media_projection(&full).unwrap();
    let mut changed=before.clone();
    changed["items"][0]["workflow"]=json!("closed");
    assert!(validate_media_change(&before,&changed).is_err());
    changed=before.clone();
    changed["jobs"][0]["sourceAttempts"]=json!([{"id":"attempt","status":"completed"}]);
    assert!(validate_media_change(&before,&changed).is_err());
    changed=before.clone();
    changed["jobs"][0]["kind"]=json!("assistant");
    assert!(validate_media_change(&before,&changed).is_err());
}

#[test]
fn offline_large_state_media_projection_clone_benchmark() {
    let mut full=fixture();
    full["branches"]=json!([{"id":"branch-large","observedMessages":"x".repeat(32*1024*1024)}]);
    let start=Instant::now();
    let whole=full.clone();
    let whole_ms=start.elapsed().as_secs_f64()*1000.0;
    let start=Instant::now();
    let projected=media_projection(&full).unwrap();
    let scoped_ms=start.elapsed().as_secs_f64()*1000.0;
    let whole_bytes=whole.to_string().len();
    let scoped_bytes=projected.to_string().len();
    eprintln!("media clone offline: whole={whole_bytes} bytes {whole_ms:.2} ms; projection={scoped_bytes} bytes {scoped_ms:.2} ms");
    assert!(scoped_bytes*20<whole_bytes);
    assert_eq!(projected["items"][0]["draft"],Value::Null);
    assert_eq!(projected["jobs"].as_array().unwrap().len(),0);
}

#[tokio::test]
#[ignore = "requires the explicitly isolated assistant scope PostgreSQL clone"]
async fn postgres_media_scope_clone_probe() {
    let url=std::env::var("COMMUNITYHERO_ASSISTANT_SCOPE_TEST_URL").expect("explicit isolated PostgreSQL clone URL");
    assert!(url.starts_with("postgresql://"));
    assert!(url.contains("@127.0.0.1:"));
    let configured_database=url.split('?').next().unwrap().rsplit('/').next().unwrap();
    assert_eq!(configured_database,"communityhero_assistant_scope_test_remediation_20260923");
    let db=Database::postgres(&url).await.unwrap();
    if let Database::Postgres{writer,..}=&db {
        let database:String=sqlx::query_scalar("SELECT current_database()")
            .fetch_one(writer).await.unwrap();
        assert_eq!(database,"communityhero_assistant_scope_test_remediation_20260923");
    }
    let started=Instant::now();
    let full=db.read().await.unwrap();
    let full_ms=started.elapsed().as_secs_f64()*1000.0;
    let expected=media_projection(&full).unwrap();
    let started=Instant::now();
    let (_,changed)=db.change_media_observed(|scoped| {
        if *scoped!=expected {return Err(internal("Media SQL projection differs from full workspace"));}
        Ok(())
    }).await.unwrap();
    let scoped_ms=started.elapsed().as_secs_f64()*1000.0;
    assert!(!changed);
    eprintln!("media PG clone no-op: full read {full_ms:.2} ms; scoped transaction {scoped_ms:.2} ms; full bytes {}; scoped bytes {}",
        full.to_string().len(),expected.to_string().len());

    // Refuse to claim unrelated queued work in the disposable clone. The dry
    // run is purely in memory and precedes every write below.
    let at=crate::now();
    let mut preflight=full.clone();
    assert!(crate::media_queue::claim_when_ready(&mut preflight,&at,true).unwrap().is_none(),
        "disposable clone already has claimable media work");
    assert!(preflight["jobs"]==full["jobs"],
        "disposable clone has unrelated media reconciliation to perform");

    let nonce=uuid::Uuid::new_v4().simple().to_string();
    let binding=crate::active_binding(&full).unwrap();
    let post_id=format!("media-scope-probe-post-{nonce}");
    let item_id=format!("media-scope-probe-item-{nonce}");
    let post_key=format!("11391:media-scope-probe-{nonce}");
    let post=json!({"id":post_id,"postKey":post_key,"objectId":"11391",
        "title":format!("Media scope probe {nonce}"),"channel":"YouTube",
        "sourceUrl":format!("https://youtu.be/{}",&nonce[..11]),"attachments":[{"type":"video"}]});
    let item=json!({"id":item_id,"itemId":nonce,"objectId":"11391",
        "postId":post_id,"postKey":post_key,"conversationKey":format!("11391:probe-{nonce}"),
        "connectorBinding":binding.to_json(),"providerStatus":"new","workflow":"attention",
        "draft":"Synthetic operator draft remains local"});
    let mut predicted=full.clone();
    predicted["posts"].as_array_mut().unwrap().push(post.clone());
    predicted["items"].as_array_mut().unwrap().push(item.clone());
    let preview=crate::media_queue::claim_when_ready(&mut predicted,&at,true).unwrap().unwrap();
    assert_eq!(preview.1["id"],post["id"],"fixture must be the only claimable media source");

    db.change(|workspace| {
        workspace["posts"].as_array_mut().unwrap().push(post.clone());
        workspace["items"].as_array_mut().unwrap().push(item.clone());
        Ok(())
    }).await.unwrap();
    let staged=db.read().await.unwrap();
    let (claimed,changed)=db.change_media_observed(|scoped|
        crate::media_queue::claim_when_ready(scoped,&at,true)).await.unwrap();
    assert!(changed);
    let (job_id,claimed_post)=claimed.expect("synthetic source should be claimed");
    assert_eq!(claimed_post["id"],post["id"]);
    let persisted=db.read().await.unwrap();
    for key in ["posts","items","branches","materials","knowledge_entries","knowledge_versions",
        "proposals","approvals","operations","audit","feedback","conversations"] {
        assert!(persisted[key]==staged[key],"media claim changed excluded {key}");
    }
    assert_eq!(persisted["jobs"].as_array().unwrap().len(),staged["jobs"].as_array().unwrap().len()+1);
    assert!(persisted["jobs"].as_array().unwrap().starts_with(staged["jobs"].as_array().unwrap()));
    let saved=crate::row(&persisted,"jobs",&job_id).unwrap();
    assert_eq!(saved["status"],"running");
    assert_eq!(saved["refId"],post["id"]);
    assert_eq!(saved["sourceAttempts"].as_array().unwrap().len(),1);
    assert_eq!(saved["sourceAttempts"][0]["status"],"running");
    assert_eq!(saved["sourceAttempts"][0]["postId"],post["id"]);
    assert!(persisted["mediaQueue"]["inputDigest"].is_string());

    let (second,_)=db.change_media_observed(|scoped|
        crate::media_queue::claim_when_ready(scoped,&at,true)).await.unwrap();
    assert!(second.is_none());
    let again=db.read().await.unwrap();
    assert_eq!(again["jobs"].as_array().unwrap().len(),persisted["jobs"].as_array().unwrap().len());
    assert_eq!(crate::row(&again,"jobs",&job_id).unwrap()["sourceAttempts"],saved["sourceAttempts"]);
    eprintln!("media PG clone claim: durable job {job_id}, one source attempt, no duplicate on second tick");
    db.close().await;
}
