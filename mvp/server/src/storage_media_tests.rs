use super::*;
use std::time::Instant;

const AT:&str="2026-09-23T08:00:00Z";

#[tokio::test]
async fn visual_proof_refresh_reads_only_current_heads_without_historical_closure(){
    let (app,_folder)=crate::tests::test_app().await;
    // Preserve the normalized full workspace collections owned by test_app.
    // Replacing it with empty() omits feedback and fails the storage contract.
    let mut d=app.read().await.unwrap();
    let post=json!({"id":"proof-post","postKey":"proof-post","title":"Synthetic proof source",
        "sourceUrl":"https://youtu.be/AbCdEf123_-","attachments":[{"type":"video"}]});
    let evidence=crate::media_fullframes::fixture_for_post("LikeAvto",&post);
    d["posts"]=json!([post]);
    d["materials"]=json!([{"id":"proof-material","kind":"visual_context","account":"LikeAvto",
        "postKey":"proof-post","sourceUrl":post["sourceUrl"],"mediaSha256":evidence["source"]["mediaSha256"],
        "text":"First source observation","visualEvidence":evidence}]);
    // Build genuine hash-bound catalog revisions and a normal older current
    // head. The same immutable evidence remains in both unpointed versions.
    crate::knowledge::sync_catalog(&mut d,AT).unwrap();
    d["materials"][0]["text"]=json!("Second source observation");
    crate::knowledge::sync_catalog(&mut d,AT).unwrap();
    let current=d["knowledge_entries"][0]["currentVersionId"].clone();
    d["materials"][0]["text"]=json!("Third source observation");
    crate::knowledge::sync_catalog(&mut d,AT).unwrap();
    d["knowledge_entries"][0]["currentVersionId"]=current;
    assert_eq!(d["knowledge_versions"].as_array().unwrap().len(),3);
    crate::knowledge::TranscriptLookup::new(&d,AT).unwrap();
    app.change(|stored|{*stored=d;Ok(())}).await.unwrap();
    assert_eq!(app.db.read_media_visual_evidence().await.unwrap(),vec![evidence]);
    app.db.close().await;
}

#[tokio::test]
async fn scoped_policy_pause_persists_and_resumes_without_changing_checkpoint_or_drafts(){
    let (app,_folder)=crate::tests::test_app().await;
    let mut d=fixture();
    let native_lifecycle=app.read().await.unwrap()["runtimeLifecycle"].clone();
    // This scenario resumes a captured visual job. Capture an explicit visual
    // policy first; changing a default text job into visual work is a new scope.
    let source=crate::media_fullframes::source_version(&d["posts"][0],"LikeAvto");
    d["settings"]["postMediaPolicies"]=json!({"post-one":{"version":1,"revision":1,"status":"active",
        "postId":"post-one","account":"LikeAvto","connectorBinding":d["connectorBinding"],
        "sourceVersion":source,"mode":"full_audio_visual"}});
    crate::storage::normalize(&mut d);
    let (id,post)=crate::media_queue::claim_when_ready(&mut d,AT,true).unwrap().unwrap();
    let binding=crate::active_binding(&d).unwrap().to_json();
    let version=crate::media_fullframes::source_version(&post,"LikeAvto");
    let job=crate::row_mut(&mut d,"jobs",&id).unwrap();
    job["status"]=json!("queued");job["result"]["visualProgress"]["leaseId"]=Value::Null;
    let checkpoint=job["result"]["visualProgress"].clone();
    d["settings"]["postMediaPolicies"]=json!({"post-one":{"version":1,"revision":1,"status":"active",
        "postId":"post-one","account":"LikeAvto","connectorBinding":binding,"sourceVersion":version,"mode":"full_audio_only"}});
    app.change(|stored|{
        // Synthetic content replaces fixture data, never the admitted App owner.
        d["runtimeLifecycle"]=stored["runtimeLifecycle"].clone();*stored=d;Ok(())
    }).await.unwrap();
    app.change_media(|stored|crate::media_queue::reconcile(stored,AT)).await.unwrap();
    let paused=app.read().await.unwrap();
    let j=crate::row(&paused,"jobs",&id).unwrap();
    assert_eq!(j["status"],"paused");assert_eq!(j["mediaPolicyPause"],true);
    assert_eq!(j["result"]["visualProgress"],checkpoint);
    assert_eq!(paused["items"][0]["draft"],"Keep this operator edit");
    assert_eq!(paused["runtimeLifecycle"],native_lifecycle);
    app.change(|stored|{stored["settings"]["postMediaPolicies"]["post-one"]["mode"]=json!("full_audio_visual");Ok(())}).await.unwrap();
    app.change_media(|stored|crate::media_queue::reconcile(stored,AT)).await.unwrap();
    let resumed=app.read().await.unwrap();let j=crate::row(&resumed,"jobs",&id).unwrap();
    assert_eq!(j["status"],"queued");assert!(j.get("mediaPolicyPause").is_none());
    assert_eq!(j["result"]["visualProgress"],checkpoint);
    assert_eq!(resumed["runtimeLifecycle"],native_lifecycle);app.db.close().await;
}

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
fn scoped_media_projection_reads_policy_without_exposing_other_settings(){
    let mut d=fixture();
    d["items"][0]["createdAt"]=json!("2026-09-23T07:59:59.999Z");
    d["settings"]["postMediaPolicies"]=json!({"post-one":{"mode":"full_audio_only","sourceVersion":"source-pin"}});
    d["settings"]["mediaAudioEquivalences"]=json!({"post-one":{"status":"active","sourcePostId":"source-post"}});
    d["settings"]["mediaPolicyDefaults"]=json!({"audioOnlyAboveSeconds":300});
    d["settings"]["privateSetting"]=json!("do not project");
    let before=media_projection(&d).unwrap();
    assert_eq!(before["items"][0]["createdAt"],d["items"][0]["createdAt"],
        "scoped media cutoff must use the exact observed comment creation time");
    assert_eq!(before["settings"]["postMediaPolicies"],d["settings"]["postMediaPolicies"]);
    assert_eq!(before["settings"]["mediaAudioEquivalences"],d["settings"]["mediaAudioEquivalences"]);
    assert_eq!(before["settings"]["mediaPolicyDefaults"],d["settings"]["mediaPolicyDefaults"]);
    assert!(before["settings"].get("privateSetting").is_none());
    let mut after=before.clone();
    after["settings"]["postMediaPolicies"]["post-one"]["mode"]=json!("full_audio_visual");
    assert!(validate_media_change(&before,&after).is_err());
    let mut after=before.clone();after["settings"]["mediaAudioEquivalences"]["post-one"]["status"]=json!("revoked");
    assert!(validate_media_change(&before,&after).is_err());
}
#[test]
fn scoped_media_retains_complete_readonly_audio_analysis_authority_and_rejects_every_foreign_write(){
    let mut full=fixture();
    for(kind,status)in [("media_audio","failed"),("media_audio","unknown"),("media_analysis","completed"),("media_analysis_applicability","completed")]{
        crate::list_mut(&mut full,"jobs").push(json!({"id":format!("{kind}-{status}"),"kind":kind,"status":status,"account":"LikeAvto","paidJournal":{"frozenRequest":null,"exact":"immutable"}}));
    }
    let before=media_projection(&full).unwrap();assert_eq!(before["jobs"].as_array().unwrap().len(),4);
    for row in crate::list(&before,"jobs"){assert_eq!(crate::row(&full,"jobs",row["id"].as_str().unwrap()).unwrap(),row);}
    validate_media_change(&before,&before).unwrap();
    for index in 0..4{for field in ["status","paidJournal","kind"]{
        let mut hostile=before.clone();hostile["jobs"][index][field]=json!("forged");assert!(validate_media_change(&before,&hostile).is_err());
    }}
    let mut appended=before.clone();crate::list_mut(&mut appended,"jobs").push(json!({"id":"new-analysis","kind":"media_analysis","status":"completed"}));assert!(validate_media_change(&before,&appended).is_err());
    // Structural storage rows are deliberately not native applicability proof.
    // Full and scoped queue paths must both reject the same malformed receipt.
    let mut full_probe=full.clone();let mut scoped_probe=before.clone();
    let full_error=crate::media_queue::claim_when_ready(&mut full_probe,AT,true).unwrap_err();
    let scoped_error=crate::media_queue::claim_when_ready(&mut scoped_probe,AT,true).unwrap_err();
    assert_eq!((full_error.0,full_error.1),(scoped_error.0,scoped_error.1));
    assert_eq!(full_probe,full);assert_eq!(scoped_probe,before);
    let clean_before=media_projection(&fixture()).unwrap();let mut claimed=clean_before.clone();
    crate::media_queue::claim_when_ready(&mut claimed,AT,true).unwrap();validate_media_change(&clean_before,&claimed).unwrap();
    assert!(crate::list(&claimed,"jobs").starts_with(crate::list(&clean_before,"jobs")));
}

async fn initialize_empty_baw_media_database(db:&Database){
    let pristine=db.read().await.unwrap();
    for table in ["posts","items","branches","conversations","proposals","approvals","operations","jobs","materials","knowledge_entries","knowledge_versions","audit","feedback"]{
        assert!(crate::list(&pristine,table).is_empty(),"BAW identity selection requires a pristine {table} collection");
    }
    // open_db starts with the unbound default account. Select this separate
    // pristine database BEFORE constructing/importing any immutable BAW ledger.
    // A same-write account switch plus paid rows must remain rejected.
    db.change(|d|crate::accounts::initialize(d,crate::accounts::Profile::BawRussia)).await.unwrap();
    let selected=db.read().await.unwrap();assert_eq!(selected["account"],"BAW Russia");
    assert_eq!(crate::active_binding(&selected).unwrap().account_id,"BAW Russia");
}

async fn assert_native_two_video_progression(db:&Database)->(Value,String){
    initialize_empty_baw_media_database(db).await;
    // BAW is selected by the native helper before its immutable donor ledger,
    // CAS evidence and applicability admission. Never retarget captured proof.
    let mut seed=crate::media_queue::cached_audio::tests::native_first_alias_fixture();
    // Applicability is admitted by native code now; an earlier historical
    // observation cannot see it. Keep original donor/capture clocks immutable.
    let observed_at=crate::now();let at=observed_at.as_str();
    crate::storage::normalize(&mut seed);
    db.change(|d|{*d=seed;Ok(())}).await.unwrap();
    let before=db.read().await.unwrap();
    assert!(before.get("mediaQueue").is_none(),"the native alias fixture must exercise an absent queue control");
    for kind in MEDIA_EVIDENCE_KINDS{
        assert!(crate::list(&before,"jobs").iter().any(|job|job["kind"]==*kind),"genuine native fixture must exercise {kind}");
    }
    let post=crate::row(&before,"posts","p").unwrap();
    let initial=crate::media_speech_assets::outcomes(&before,post,at).unwrap();
    assert_eq!(initial.len(),2);assert_eq!(initial[0]["status"],"ready");assert_eq!(initial[1]["status"],"pending");
    let second_pin=initial[1]["assetPin"].clone();
    let mut oracle=before.clone();
    let (oracle_id,oracle_post)=crate::media_queue::claim_when_ready(&mut oracle,at,true).unwrap().unwrap();
    let oracle_job=crate::row(&oracle,"jobs",&oracle_id).unwrap();
    assert_eq!(oracle_post["id"],"p");assert_eq!(oracle_job["videoSpeechAssetPin"],second_pin);
    let (claimed,changed)=db.change_media_observed(|view|{
        assert_eq!(*view,media_projection(&before).unwrap(),"actual SQL read must retain every native evidence body");
        assert_eq!(crate::media_speech_assets::outcomes(view,crate::row(view,"posts","p")?,at).unwrap(),initial);
        crate::media_queue::claim_when_ready(view,at,true)
    }).await.unwrap();
    assert!(changed);let (id,claimed_post)=claimed.unwrap();assert_eq!(claimed_post["id"],"p");
    let after=db.read().await.unwrap();let job=crate::row(&after,"jobs",&id).unwrap();
    assert_eq!(job["videoSpeechAssetPin"],second_pin);assert_eq!(job["status"],"running");
    assert_eq!(job["sourceAttempts"].as_array().unwrap().len(),1);
    assert_eq!(job["sourceAttempts"][0]["assetPin"],second_pin);
    assert_eq!(job["sourceAttempts"][0]["attemptNumber"],1);
    // Only this new unpaid download claim allocates these three UUIDs. Match
    // them for an exact whole-result comparison; captured proof is untouched.
    let mut comparable=job.clone();
    for pointer in ["/id","/sourceAttempts/0/id","/result/visualProgress/leaseId"]{
        uuid::Uuid::parse_str(job.pointer(pointer).unwrap().as_str().unwrap()).unwrap();
        *comparable.pointer_mut(pointer).unwrap()=oracle_job.pointer(pointer).unwrap().clone();
    }
    assert_eq!(&comparable,oracle_job,"all other claim fields must match the full native reducer");
    let mut whole=after.clone();*crate::row_mut(&mut whole,"jobs",&id).unwrap()=comparable;
    assert_eq!(whole,oracle,"full/scoped persisted semantics differ outside new unpaid allocation UUIDs");
    assert_eq!(after["jobs"].as_array().unwrap().len(),before["jobs"].as_array().unwrap().len()+1);
    assert!(after["jobs"].as_array().unwrap().starts_with(before["jobs"].as_array().unwrap()));
    for key in ["posts","items","materials","knowledge_entries","knowledge_versions","operations","approvals","feedback"]{assert_eq!(after[key],before[key],"native media claim changed {key}");}
    assert_eq!(crate::media_analysis::ledger_from_workspace(&after).unwrap(),crate::media_analysis::ledger_from_workspace(&before).unwrap());
    let scoped=media_projection(&after).unwrap();
    assert_eq!(crate::media_speech_assets::outcomes(&scoped,crate::row(&scoped,"posts","p").unwrap(),at).unwrap(),crate::media_speech_assets::outcomes(&after,crate::row(&after,"posts","p").unwrap(),at).unwrap());
    let (repeat,_)=db.change_media_observed(|view|crate::media_queue::claim_when_ready(view,at,true)).await.unwrap();
    assert!(repeat.is_none());let repeated=db.read().await.unwrap();
    assert_eq!(repeated["jobs"],after["jobs"],"second claim cannot duplicate or replay either video");
    (before,observed_at)
}

#[tokio::test]
async fn sqlite_media_jobs_only_preserves_explicit_null_and_real_reconcile_persists_queue_digest(){
    let temp=tempfile::tempdir().unwrap();
    let db=Database::Sqlite(crate::open_db(&temp.path().join("native-null-queue.sqlite")).await.unwrap());
    initialize_empty_baw_media_database(&db).await;
    let mut seed=crate::media_queue::cached_audio::tests::native_first_alias_fixture();
    assert!(seed.get("mediaQueue").is_none());seed["mediaQueue"]=Value::Null;
    crate::storage::normalize(&mut seed);db.change(|d|{*d=seed;Ok(())}).await.unwrap();
    let before=db.read().await.unwrap();let at=crate::now();
    let (claim,changed)=db.change_media_observed(|view|crate::media_queue::claim_when_ready(view,&at,true)).await.unwrap();
    assert!(claim.is_some());assert!(changed);
    let claimed=db.read().await.unwrap();assert_eq!(claimed.get("mediaQueue"),Some(&Value::Null));
    assert!(crate::list(&claimed,"jobs").starts_with(crate::list(&before,"jobs")),"a jobs-only claim must preserve every original paid journal");
    assert_eq!(crate::media_analysis::ledger_from_workspace(&claimed).unwrap(),crate::media_analysis::ledger_from_workspace(&before).unwrap());
    let mut expected=claimed.clone();crate::media_queue::reconcile(&mut expected,&at).unwrap();
    let (_,changed)=db.change_media_observed(|view|crate::media_queue::reconcile(view,&at)).await.unwrap();
    assert!(changed,"the native reconciliation must persist a real queue-control update");
    let after=db.read().await.unwrap();let digest=after["mediaQueue"]["inputDigest"].as_str().unwrap();
    assert_eq!(digest.len(),64);assert!(digest.bytes().all(|byte|byte.is_ascii_hexdigit()));
    assert_eq!(after,expected,"native queue-control updates must match full-state reconciliation exactly");
    db.close().await;
}

#[tokio::test]
async fn native_two_video_first_alias_scoped_sqlite_progression_preserves_paid_history(){
    let temp=tempfile::tempdir().unwrap();
    let db=Database::Sqlite(crate::open_db(&temp.path().join("native-two-video.sqlite")).await.unwrap());
    let(before,observed_at)=assert_native_two_video_progression(&db).await;let at=observed_at.as_str();
    let second_pin=crate::media_speech_assets::capture(&before,crate::row(&before,"posts","p").unwrap(),1).unwrap();
    db.close().await;

    // These negative children are explicitly unexecuted structural states, not
    // fabricated paid authority. The first alias still uses genuine native CAS.
    // Missing these readonly rows used to reinterpret FAILED/UNKNOWN as pending.
    for status in ["failed","unknown"]{
        let mut state=before.clone();
        crate::list_mut(&mut state,"jobs").push(json!({"id":format!("unexecuted-second-{status}"),"kind":"media_audio","purpose":"required_video_speech","status":status,"account":before["account"],"connectorBinding":before["connectorBinding"],"refId":"p","audioPin":{"progress":{"assetPin":second_pin}},"error":format!("isolated_unexecuted_{status}")}));
        let path=temp.path().join(format!("negative-{status}.sqlite"));let negative=Database::Sqlite(crate::open_db(&path).await.unwrap());
        initialize_empty_baw_media_database(&negative).await;
        negative.change(|d|{*d=state;Ok(())}).await.unwrap();let original=negative.read().await.unwrap();
        let expected=crate::media_speech_assets::outcomes(&original,crate::row(&original,"posts","p").unwrap(),at).unwrap();
        assert_eq!(expected[0]["status"],"ready");assert_eq!(expected[1]["status"],status);
        let (_,changed)=negative.change_media_observed(|view|{
            assert_eq!(crate::media_speech_assets::outcomes(view,crate::row(view,"posts","p")?,at).unwrap(),expected);
            assert!(crate::media_speech_assets::next_unattempted(view,crate::row(view,"posts","p")?,at).unwrap().is_none());
            Ok(())
        }).await.unwrap();assert!(!changed);assert_eq!(negative.read().await.unwrap(),original);
        negative.close().await;
    }
}

#[tokio::test]
#[ignore="ROOT-owned pristine explicitly isolated BAW PostgreSQL fixture"]
async fn postgres_native_two_video_four_kind_scope_preserves_paid_proof_and_rejects_malformed_applicability(){
    let url=std::env::var("COMMUNITYHERO_WRITER_V51_TEST_URL").expect("explicit isolated fixture URL");
    let expected=std::env::var("COMMUNITYHERO_WRITER_V51_TEST_DATABASE").expect("explicit isolated fixture name");
    let db=crate::storage::preparation::writer_v51_fixture_db_for_profile_with(&url,&expected,crate::accounts::Profile::BawRussia).await;
    assert_eq!(db.read().await.unwrap()["account"],"BAW Russia","create the BAW database before any native capture");
    assert_native_two_video_progression(&db).await;
    let healthy=db.read().await.unwrap();
    // Explicit isolated persistence fault, with no paid receipt. Native guards
    // are not weakened to admit this malformed applicability through a writer.
    let invalid=json!({"id":"malformed-applicability-unexecuted","kind":"media_analysis_applicability","status":"completed","account":healthy["account"],"connectorBinding":healthy["connectorBinding"],"receipt":{"deliberatelyInvalid":true}});
    let Database::Postgres{writer,..}=&db else{unreachable!()};
    sqlx::query("INSERT INTO communityhero.jobs(workspace_id,id,ordinal,payload,kind,status,ref_id) SELECT $1,$2,COALESCE(MAX(ordinal),-1)+1,$3::jsonb,$4,$5,NULL FROM communityhero.jobs WHERE workspace_id=$1")
        .bind(WORKSPACE).bind(invalid["id"].as_str().unwrap()).bind(invalid.to_string()).bind("media_analysis_applicability").bind("completed").execute(writer).await.unwrap();
    let faulty=db.read().await.unwrap();
    assert!(faulty["jobs"].as_array().unwrap().starts_with(healthy["jobs"].as_array().unwrap()),"fault injection cannot rewrite existing paid evidence");
    let at=crate::now();let full_error=crate::media_speech_assets::outcomes(&faulty,crate::row(&faulty,"posts","p").unwrap(),&at).unwrap_err();
    let scoped_error=db.change_media_observed(|view|{
        assert_eq!(*view,media_projection(&faulty).unwrap(),"malformed authority must reach native validation, not disappear from SQL scope");
        crate::media_speech_assets::outcomes(view,crate::row(view,"posts","p")?,&at).map_err(|reason|crate::conflict(&reason))
    }).await.unwrap_err();
    assert_eq!(scoped_error.1,full_error);assert_eq!(scoped_error.0,axum::http::StatusCode::CONFLICT);
    assert_eq!(db.read().await.unwrap(),faulty,"rejected scope performs no persistent write or blind retry");
    for key in ["posts","items","materials","knowledge_entries","knowledge_versions","operations","approvals","feedback"]{assert_eq!(faulty[key],healthy[key],"fault scope changed {key}");}
    db.close().await;
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
    // This is the retained visual queue scenario. New default text extraction
    // additionally needs its own OCR receipt; selected visual proof is not OCR.
    let source=crate::media_fullframes::source_version(&full["posts"][0],"LikeAvto");
    full["settings"]["postMediaPolicies"]=json!({"post-one":{"version":1,"revision":1,"status":"active",
        "postId":"post-one","account":"LikeAvto","connectorBinding":full["connectorBinding"],
        "sourceVersion":source,"mode":"full_audio_visual"}});
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
    let evidence=crate::media_fullframes::fixture_for_post("LikeAvto",&full["posts"][0]);
    let source=crate::media_fullframes::source_version(&full["posts"][0],"LikeAvto");
    let seconds=evidence["source"]["durationMs"].as_f64().unwrap()/1000.0;
    full["materials"]=json!([{"id":"transcript-one","kind":"transcript","account":"LikeAvto","postKey":"11391:one",
        "text":"Verified full transcript for this source",
        "transcription":{"partial":false,"coverage":"full_audio","sourceVersion":source,
            "mediaDurationSeconds":seconds,"audioDurationSeconds":seconds}}]);
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
    assert_eq!(persisted["mediaQueue"],staged["mediaQueue"],
        "First appended claim leaves discovery dirty for the next persisted reconciliation");

    let (second,_)=db.change_media_observed(|scoped|
        crate::media_queue::claim_when_ready(scoped,&at,true)).await.unwrap();
    assert!(second.is_none());
    let again=db.read().await.unwrap();
    assert_eq!(again["jobs"].as_array().unwrap().len(),persisted["jobs"].as_array().unwrap().len());
    assert_eq!(crate::row(&again,"jobs",&job_id).unwrap()["sourceAttempts"],saved["sourceAttempts"]);
    eprintln!("media PG clone claim: durable job {job_id}, one source attempt, no duplicate on second tick");
    assert_legacy_binding_pg_claim(&db,&job_id,&at).await;
    db.close().await;
}

// The selected target lacks exact-source text; its title family already has a
// genuine strict-ready sibling. Queue coverage must not allocate another ASR.
fn discovery_ready_family(mut d:Value,target_id:&str,title:&str)->Value{
    let mut target=d["posts"][0].clone();target["id"]=json!(target_id);
    target["postKey"]=json!(format!("11391:{target_id}"));target["title"]=json!(title);
    target["sourceUrl"]=json!("https://vk.com/video-123_999");target["durationSeconds"]=json!(1200.0);
    target["mediaSha256"]=json!("a".repeat(64));
    let mut item=d["items"][0].clone();item["id"]=json!(format!("item-{target_id}"));
    item["itemId"]=json!(target_id);item["postId"]=target["id"].clone();item["postKey"]=target["postKey"].clone();
    item["workflow"]=json!("attention");item["providerStatus"]=json!("new");
    item["conversationKey"]=json!(format!("11391:{target_id}"));
    let mut donor=target.clone();donor["id"]=json!("ready-donor");donor["postKey"]=json!("11390:ready-donor");
    donor["objectId"]=json!("11390");donor["channel"]=json!("YouTube");
    donor["sourceUrl"]=json!("https://youtu.be/AbCdEf123_-");
    donor["title"]=json!("Different donor publication title, identical verified bytes");
    let source=crate::media_fullframes::source_version(&donor,"LikeAvto");
    crate::list_mut(&mut d,"posts").extend([target,donor.clone()]);crate::list_mut(&mut d,"items").push(item);
    d["materials"]=json!([{"id":"paid-ready-audio","kind":"transcript","account":"LikeAvto",
        "postKey":donor["postKey"],"sourceUrl":donor["sourceUrl"],"mediaSha256":donor["mediaSha256"],"text":"Already paid full source words",
        "transcription":{"sourceVersion":source,"partial":false,"coverage":"full_audio",
            "mediaDurationSeconds":1200.0,"audioDurationSeconds":1200.0,
            "ocr":{"sourceVersion":source,"coverage":"sampled_frames","sampledFrames":1,"failedFrames":0,"status":"no_text_found"}}}]);
    crate::knowledge::sync_catalog(&mut d,AT).unwrap();d
}

#[test]
fn discovery_ready_speech_skips_new_intent_and_preserves_paid_source(){
    let mut full=fixture();full["items"][0]["workflow"]=json!("closed");
    full["items"][0]["providerStatus"]=json!("closed");
    full=discovery_ready_family(full,"ready-target","Verified title family");
    let target=crate::row(&full,"posts","ready-target").unwrap();
    let state=crate::knowledge::TranscriptLookup::new(&full,AT).unwrap().strict_media_evidence(target).unwrap();
    assert_eq!(state["audioReady"],true,"exact bytes prove audio reuse independently of publication title");
    assert_eq!(state["screenTextReady"],false,"donor OCR source-version receipt has not been rebound to this recipient");
    let paid=full["knowledge_versions"].clone();
    let mut expected=full.clone();crate::media_queue::reconcile(&mut expected,AT).unwrap();
    let before=media_projection(&full).unwrap();let mut first=before.clone();
    crate::media_queue::reconcile(&mut first,AT).unwrap();
    validate_media_change(&before,&first).unwrap();
    assert_eq!(first,media_projection(&expected).unwrap(),"scoped discovery matches full speech policy");
    assert!(first["jobs"].as_array().unwrap().is_empty(),"full speech requires no OCR/frame acquisition intent");
    let persisted=first.clone();crate::media_queue::reconcile(&mut first,AT).unwrap();
    validate_media_change(&persisted,&first).unwrap();
    assert_eq!(first,persisted,"ready source discovery is idempotent without a synthetic completion job");
    assert_eq!(first["knowledge_versions"],paid);
    assert!(crate::media_queue::claim_when_ready(&mut first,AT,true).unwrap().is_none());
}

#[test]
fn discovery_direct_claim_ready_speech_does_not_allocate_source_attempt(){
    let mut full=fixture();full["items"][0]["workflow"]=json!("closed");
    full["items"][0]["providerStatus"]=json!("closed");
    let full=discovery_ready_family(full,"ready-target","Verified title family");
    let before=media_projection(&full).unwrap();let mut first=before.clone();
    assert!(crate::media_queue::claim_when_ready(&mut first,AT,true).unwrap().is_none());
    validate_media_change(&before,&first).unwrap();
    assert!(first["jobs"].as_array().unwrap().is_empty(),"ready required speech must not spend another acquisition attempt");
    assert_eq!(first["knowledge_versions"],before["knowledge_versions"]);
    let persisted=first.clone();
    assert!(crate::media_queue::claim_when_ready(&mut first,AT,true).unwrap().is_none());
    validate_media_change(&persisted,&first).unwrap();assert_eq!(first,persisted);
}

#[test]
fn discovery_same_title_different_bytes_acquires_target_and_preserves_paid_donor(){
    let mut full=fixture();full["items"][0]["workflow"]=json!("closed");
    full["items"][0]["providerStatus"]=json!("closed");
    let mut full=discovery_ready_family(full,"missing-target","A repeated campaign title");
    let donor=crate::row(&full,"posts","ready-donor").unwrap().clone();
    let target=crate::row_mut(&mut full,"posts","missing-target").unwrap();
    target["title"]=donor["title"].clone();target["mediaSha256"]=json!("b".repeat(64));
    assert_ne!(target["sourceUrl"],donor["sourceUrl"]);
    let target=target.clone();let retained=full.clone();
    let lookup=crate::knowledge::TranscriptLookup::new(&full,AT).unwrap();
    assert_eq!(lookup.strict_media_evidence(&donor).unwrap()["audioReady"],true);
    assert_eq!(lookup.strict_media_evidence(&target).unwrap()["audioReady"],false);
    let before=media_projection(&full).unwrap();let mut queued=before.clone();
    crate::media_queue::reconcile(&mut queued,AT).unwrap();validate_media_change(&before,&queued).unwrap();
    assert_eq!(queued["jobs"].as_array().unwrap().len(),1);assert_eq!(queued["jobs"][0]["status"],"queued");
    let persisted=queued.clone();crate::media_queue::reconcile(&mut queued,AT).unwrap();
    validate_media_change(&persisted,&queued).unwrap();assert_eq!(queued["jobs"][0]["status"],"queued");
    let persisted=queued.clone();
    let (_,selected)=crate::media_queue::claim_when_ready(&mut queued,AT,true).unwrap().unwrap();
    validate_media_change(&persisted,&queued).unwrap();assert_eq!(selected["id"],target["id"]);
    assert_eq!(queued["jobs"][0]["sourceAttempts"][0]["postId"],target["id"]);
    assert_eq!(queued["knowledge_versions"],before["knowledge_versions"],"paid donor evidence stays immutable");
    assert_eq!(full,retained,"projection scheduling cannot rewrite the original paid workspace");
}

#[test]
fn discovery_new_ready_sibling_does_not_starve_existing_queued_source(){
    let mut full=fixture();crate::media_queue::reconcile(&mut full,AT).unwrap();
    let old=full["jobs"].as_array().unwrap().iter().find(|j|j["kind"]=="media").unwrap()["id"].clone();
    let full=discovery_ready_family(full,"ready-target","Verified distinct title family");
    let before=media_projection(&full).unwrap();let mut after=before.clone();
    let (claimed,post)=crate::media_queue::claim_when_ready(&mut after,AT,true).unwrap().unwrap();
    validate_media_change(&before,&after).unwrap();assert_eq!(claimed,old.as_str().unwrap());assert_eq!(post["id"],"post-one");
    assert!(after["jobs"].as_array().unwrap().iter().all(|j|j["refId"]!="ready-target"),
        "ready required speech does not compete with the existing missing source");
    assert_eq!(after["knowledge_versions"],before["knowledge_versions"]);
    assert_eq!(after["items"],before["items"]);
}

#[test]
fn discovery_persisted_new_intent_can_pause_with_strict_new_status_guard_unchanged(){
    let full=fixture();let before=media_projection(&full).unwrap();let mut queued=before.clone();
    crate::media_queue::reconcile(&mut queued,AT).unwrap();validate_media_change(&before,&queued).unwrap();
    for status in ["completed","paused","cancelled"]{
        let mut forged=queued.clone();forged["jobs"][0]["status"]=json!(status);
        assert!(validate_media_change(&before,&forged).is_err(),"Appended {status} remains forbidden");
    }
    queued["items"][0]["workflow"]=json!("closed");queued["items"][0]["providerStatus"]=json!("closed");
    let persisted=queued.clone();crate::media_queue::reconcile(&mut queued,AT).unwrap();
    validate_media_change(&persisted,&queued).unwrap();assert_eq!(queued["jobs"][0]["status"],"paused");
    assert_eq!(queued["jobs"][0]["sourceAttempts"],json!([]));
    assert!(crate::media_queue::claim_when_ready(&mut queued,AT,true).unwrap().is_none());
}

async fn assert_legacy_binding_pg_claim(db:&Database,job_id:&str,at:&str){
    let initial=db.read().await.unwrap();let binding=crate::active_binding(&initial).unwrap();
    // Exercise the same legacy binding admission through the normalized PG
    // writer. Only this test-created job is changed; no worker is dispatched.
    for missing in [true,false] {
        db.change(|workspace|{
            let job=crate::row_mut(workspace,"jobs",&job_id)?;job["status"]=json!("queued");
            if missing{job.as_object_mut().unwrap().remove("connectorBinding");}else{job["connectorBinding"]=Value::Null;}
            let p=&mut job["result"]["visualProgress"];p["phase"]=json!("scan");p["leaseId"]=Value::Null;
            p["source"]=json!({"sha256":"a".repeat(64),"bytes":12});
            p["inventoryDescriptor"]=json!({"sha256":"b".repeat(64),"bytes":34});
            p["selectionDescriptor"]=json!({"sha256":"c".repeat(64),"bytes":56});
            p["latestReceipt"]=json!({"sha256":"d".repeat(64),"bytes":78});p["nextSelectionIndex"]=json!(24);p["completedSelectedFrames"]=json!(24);
            p["sourceIdentity"]=json!({"account":p["account"],"postKey":p["sourcePostKey"],"mediaSha256":"a".repeat(64),"durationMs":120000});Ok(())
        }).await.unwrap();
        let before=db.read().await.unwrap();let prior=crate::row(&before,"jobs",&job_id).unwrap();
        let expected=media_projection(&before).unwrap();
        let (_,changed)=db.change_media_observed(|scoped|{assert_eq!(*scoped,expected,"PG scoped read model must match domain projection before claim");Ok(())}).await.unwrap();
        assert!(!changed);
        assert!(db.change_media_observed(|scoped|{crate::row_mut(scoped,"jobs",&job_id)?["connectorBinding"]=binding.to_json();Ok(())}).await.is_err(),"unverified standalone binding stamp must roll back in PG");
        assert_eq!(db.read().await.unwrap(),before,"rejected standalone stamp preserves entire normalized workspace");
        let (claimed,changed)=db.change_media_observed(|scoped|crate::media_queue::claim_when_ready(scoped,&at,true)).await.unwrap();
        assert!(changed);assert_eq!(claimed.unwrap().0,job_id);
        let after=db.read().await.unwrap();let admitted=crate::row(&after,"jobs",&job_id).unwrap();
        let expected=media_projection(&after).unwrap();
        let (_,changed)=db.change_media_observed(|scoped|{assert_eq!(*scoped,expected,"PG scoped read model must match after admitted claim");Ok(())}).await.unwrap();
        assert!(!changed);
        assert_eq!(admitted["connectorBinding"],binding.to_json());assert_eq!(admitted["sourceAttempts"],prior["sourceAttempts"]);
        for field in ["source","sourceIdentity","inventoryDescriptor","selectionDescriptor","latestReceipt","nextSelectionIndex"]{assert_eq!(admitted["result"]["visualProgress"][field],prior["result"]["visualProgress"][field]);}
    }
    for hostile in [json!({"foreign":true}),json!("malformed")] {
        db.change(|workspace|{let job=crate::row_mut(workspace,"jobs",&job_id)?;job["status"]=json!("queued");job["connectorBinding"]=hostile.clone();job["result"]["visualProgress"]["leaseId"]=Value::Null;Ok(())}).await.unwrap();
        let before=db.read().await.unwrap();
        assert!(db.change_media_observed(|scoped|{let job=crate::row_mut(scoped,"jobs",&job_id)?;job["connectorBinding"]=binding.to_json();job["status"]=json!("running");Ok(())}).await.is_err(),"explicit hostile binding never becomes mutable");
        assert_eq!(db.read().await.unwrap(),before,"rejected hostile stamp preserves entire normalized workspace");
    }
    // Preserve the previous fixture's terminal posture for later clone probes.
    db.change(|workspace|{let job=crate::row_mut(workspace,"jobs",&job_id)?;job["status"]=json!("running");job["connectorBinding"]=binding.to_json();Ok(())}).await.unwrap();
}

#[tokio::test]
#[ignore="requires a pristine explicitly isolated communityhero_writer_v51_test_ PostgreSQL fixture"]
async fn postgres_legacy_media_binding_claim_fresh_fixture(){
    // Guard requires a named loopback test database with no existing product
    // schema. The fixture contains no company data and launches no workers.
    let db=crate::storage::preparation::writer_v51_fixture_db().await;
    let mut seed=fixture();crate::storage::normalize(&mut seed);
    db.change(|workspace|{*workspace=seed;Ok(())}).await.unwrap();
    let (claimed,changed)=db.change_media_observed(|scoped|crate::media_queue::claim_when_ready(scoped,AT,true)).await.unwrap();
    assert!(changed);let (job_id,post)=claimed.unwrap();assert_eq!(post["id"],"post-one");
    assert_legacy_binding_pg_claim(&db,&job_id,AT).await;
    db.close().await;
    println!("legacy_media_binding_pg_fresh_fixture_passed");
}
