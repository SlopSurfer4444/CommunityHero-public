use super::*;
use crate::media_artifacts::ArtifactStore;
const AT:&str="2026-09-26T08:00:00Z";
fn fixture()->(Value,Value){
    let mut d=crate::empty();crate::accounts::initialize(&mut d,crate::accounts::Profile::LikeAvto).unwrap();
    for key in ["knowledge_entries","knowledge_versions","feedback"] {d[key]=json!([]);}
    d["posts"]=json!([{"id":"post-one","postKey":"11390:one","objectId":"11390","title":"Retained original test","channel":"YouTube","sourceUrl":"https://www.youtube.com/watch?v=AbCdEf123_-","durationMs":2000,"attachments":[{"type":"video"}]}]);
    d["items"]=json!([{"id":"item-one","itemId":"one","objectId":"11390","postId":"post-one","postKey":"11390:one","conversationKey":"11390:thread","providerStatus":"new","workflow":"attention","draft":"preserve"}]);
    // Historical visual-origin import has an explicit owner floor; new
    // default text jobs follow the separate source-to-text handoff contract.
    d["settings"]["postMediaPolicies"]=json!({"post-one":{"version":1,"revision":1,"status":"active",
        "postId":"post-one","account":d["account"],"connectorBinding":d["connectorBinding"],
        "sourceVersion":crate::media_fullframes::source_version(&d["posts"][0],text(&d,"account")),"mode":"full_audio_visual"}});
    let (_,post)=crate::media_queue::claim_when_ready(&mut d,AT,true).unwrap().unwrap();
    let job=&mut d["jobs"][0];job["status"]=json!("failed");job["error"]=json!("source_download_failed_auth");job["result"]=json!({});job["finishedAt"]=json!(AT);
    job["sourceAttempts"][0]["status"]=json!("failed");job["sourceAttempts"][0]["error"]=json!("source_download_failed_auth");job["sourceAttempts"][0]["finishedAt"]=json!(AT);
    let body=json!({"receiptId":uuid::Uuid::new_v4().to_string(),"artifactId":uuid::Uuid::new_v4().to_string(),"account":d["account"],"postId":post["id"],"postKey":post["postKey"],"jobId":d["jobs"][0]["id"],"attemptIndex":0,"sourceKey":"yt:AbCdEf123_-","sourceUrl":post["sourceUrl"],"sourceVersion":crate::media_fullframes::source_version(&post,text(&d,"account")),"connectorBinding":d["connectorBinding"],"expectedError":"source_download_failed_auth","bytes":5,"sha256":format!("{:x}",Sha256::digest(b"media")),"durationMs":2000,
        "provenance":{"acquiredAt":"2026-09-25T01:00:00Z","method":"operator_retained_original_review","evidenceSha256":"c".repeat(64),"note":"Fixture operator provenance, acquired before failure; no fresh download claim."}});
    (d,body)
}
fn proof()->Value{json!({"hasVideo":true,"hasAudio":true,"fullDecode":true,"durationMs":2000})}
fn reference(body:&Value)->ArtifactRef{ArtifactRef{sha256:text(body,"sha256").into(),bytes:body["bytes"].as_u64().unwrap()}}
fn prepare_ingress(scratch:&Path,body:&Value)->PathBuf{
    let root=scratch.join("retained-source-import").join(crate::media_fullframes::hash(&json!([body["account"],body["connectorBinding"]])));
    fs::create_dir_all(&root).unwrap();root
}
#[test]
fn source_import_preserves_failure_and_replays_without_ingress(){
    let (mut d,body)=fixture();let original=d["jobs"][0]["sourceAttempts"][0].clone();let expected=admit(&d,&body,AT).unwrap();
    let receipt=commit(&mut d,&body,"owner",&expected,&reference(&body),&proof(),AT).unwrap();
    assert_eq!(d["jobs"][0]["sourceAttempts"][0],original);
    assert_eq!(d["jobs"][0]["sourceAttempts"][1]["kind"],"retained_original_import");
    assert_eq!(d["jobs"][0]["result"]["visualProgress"]["phase"],"inventory");
    assert!(d["jobs"][0]["result"]["visualProgress"]["latestReceipt"].is_null());
    assert!(d["jobs"][0]["downloadRetry"].is_null());
    assert_eq!(d["items"][0]["draft"],"preserve");
    let committed=d.clone();assert_eq!(replay(&d,&body,"owner").unwrap(),Some(receipt.clone()));
    assert_eq!(commit(&mut d,&body,"owner",&expected,&reference(&body),&proof(),AT).unwrap(),receipt);assert_eq!(d,committed);
    assert!(replay(&d,&body,"other-owner").is_err());
    let mut changed=body.clone();changed["provenance"]["note"]=json!("Different provenance");assert!(replay(&d,&changed,"owner").is_err());
    let (job_id,_)=crate::media_queue::claim_when_ready(&mut d,AT,true).unwrap().unwrap();assert_eq!(job_id,body["jobId"]);
    assert_eq!(d["jobs"][0]["sourceAttempts"].as_array().unwrap().len(),2,"claim cannot redownload");
    let leased=d["jobs"][0]["result"]["visualProgress"].clone();
    crate::media_queue::recover(&mut d,AT).unwrap();assert_eq!(d["jobs"][0]["status"],"queued");
    let restored=&d["jobs"][0]["result"]["visualProgress"];assert_eq!(restored["source"],leased["source"]);assert_eq!(restored["phase"],"inventory");assert!(restored["leaseId"].is_null());
}
#[test]
fn source_import_hostile_binding_and_state_leave_workspace_unchanged(){
    for case in ["account","binding","source","post-key","version","post-id","ref-id","attempt","job-status","attempt-status","error","unknown-error","process-unknown","checkpoint","empty-checkpoint","pending-retry","old-import","active","unknown","parallel","source-url","duration","future","hash","oversize","path","unknown-field"] {
        let (mut d,mut body)=fixture();
        match case {
            "account"=>body["account"]=json!("BAW Russia"),"binding"=>body["connectorBinding"]["accountId"]=json!("Other"),
            "source"=>d["posts"][0]["sourceUrl"]=json!("https://www.youtube.com/watch?v=Different01"),"post-key"=>body["postKey"]=json!("other"),
            "version"=>body["sourceVersion"]=json!("b".repeat(64)),"post-id"=>body["postId"]=json!("absent"),"attempt"=>body["attemptIndex"]=json!(10),
            "ref-id"=>d["jobs"][0]["refId"]=json!("later-fallback-source"),
            "job-status"=>d["jobs"][0]["status"]=json!("queued"),"attempt-status"=>d["jobs"][0]["sourceAttempts"][0]["status"]=json!("running"),
            "error"=>body["expectedError"]=json!("source_download_failed"),
            "unknown-error"|"process-unknown"=>{let error=if case=="unknown-error"{"not_a_download"}else{"source_download_failed_process_unknown"};body["expectedError"]=json!(error);d["jobs"][0]["sourceAttempts"][0]["error"]=json!(error);},
            "checkpoint"=>d["jobs"][0]["result"]["visualProgress"]=json!({"schemaVersion":2,"phase":"inventory"}),
            "empty-checkpoint"=>d["jobs"][0]["result"]["visualProgress"]=json!({}),
            "pending-retry"=>d["jobs"][0]["sourceAttempts"][0]["retryPermit"]=json!({"id":"pending"}),"old-import"=>d["jobs"][0]["sourceImport"]=json!({"id":"old"}),
            "active"|"unknown"|"parallel"=>{let status=match case{"active"=>"running","unknown"=>"unknown",_=>"queued"};let group=d["jobs"][0]["groupKey"].clone();crate::list_mut(&mut d,"jobs").push(json!({"id":"other","kind":"media","status":status,"groupKey":group}));},
            "source-url"=>body["sourceUrl"]=json!("https://example.invalid/other.mp4"),"duration"=>body["durationMs"]=json!(4000),
            "future"=>body["provenance"]["acquiredAt"]=json!("2099-01-01T00:00:00Z"),"hash"=>body["sha256"]=json!("invalid"),"oversize"=>body["bytes"]=json!(MAX_BYTES+1),
            "path"=>body["artifactId"]=json!("../../secret"),_=>body["path"]=json!("C:/private")
        }
        let before=d.clone();assert!(admit(&d,&body,AT).is_err(),"{case}");assert_eq!(before,d,"{case}");
    }
}
#[test]
fn source_import_revalidates_transaction_and_verified_proof(){
    for case in ["job","source","material","account","binding","hash","bytes","audio","decode"] {
        let (mut d,body)=fixture();let expected=admit(&d,&body,AT).unwrap();let mut reference=reference(&body);let mut probe=proof();
        match case {
            "job"=>d["jobs"][0]["finishedAt"]=json!("changed"),"source"=>d["posts"][0]["title"]=json!("changed"),
            "material"=>crate::list_mut(&mut d,"knowledge_entries").push(json!({"id":"new","kind":"transcript","currentVersionId":"new","scope":{"postKeys":[body["postKey"]]}})),
            "account"=>d["account"]=json!("BAW Russia"),"binding"=>d["connectorBinding"]["accountId"]=json!("other"),
            "hash"=>reference.sha256="b".repeat(64),"bytes"=>reference.bytes+=1,"audio"=>probe["hasAudio"]=json!(false),_=>probe["fullDecode"]=json!(false)
        }
        let before=d.clone();assert!(commit(&mut d,&body,"owner",&expected,&reference,&probe,AT).is_err(),"{case}");assert_eq!(d,before,"{case}");
    }
}
#[test]
fn source_import_bounded_copy_hash_ingress_and_orphan_recovery(){
    let temp=tempfile::tempdir().unwrap();let (_,body)=fixture();let root=prepare_ingress(temp.path(),&body);let path=root.join(format!("{}.media",text(&body,"artifactId")));
    fs::write(&path,b"media").unwrap();assert_eq!(ingress(temp.path(),&body).unwrap(),root);
    let mut other=body.clone();other["account"]=json!("BAW Russia");assert!(ingress(temp.path(),&other).is_err());
    let store=ArtifactStore::open(&temp.path().join("cas")).unwrap();
    let staged=copy_bounded(&root,&body).unwrap();let staged_path=staged.0.clone();let original=store.put_file(&staged.0).unwrap();drop(staged);assert!(!staged_path.exists());
    // Crash after CAS persistence, before DB commit: exact restaging deduplicates
    // the orphan; nothing can turn it into a checkpoint without admit+commit.
    let staged=copy_bounded(&root,&body).unwrap();assert_eq!(store.put_file(&staged.0).unwrap(),original);
    for contents in [b"wrong".as_slice(),b"medi".as_slice(),b"media!".as_slice()] {fs::write(&path,contents).unwrap();assert!(copy_bounded(&root,&body).is_err());}
    fs::write(&path,b"media").unwrap();let mut wrong=body.clone();wrong["sha256"]=json!("b".repeat(64));assert!(copy_bounded(&root,&wrong).is_err());
    wrong=body.clone();wrong["bytes"]=json!(MAX_BYTES+1);assert!(copy_bounded(&root,&wrong).is_err());
    wrong=body.clone();wrong["artifactId"]=json!("../other");assert!(copy_bounded(&root,&wrong).is_err());
    // A failed/abandoned scratch copy does not collide with a UUID ingress file.
    fs::write(root.join(".verified-abandoned.media"),b"partial").unwrap();assert!(copy_bounded(&root,&body).is_ok());
    let object=store.path(&original).unwrap();fs::write(object,b"wrong").unwrap();assert!(store.put_file(&path).is_err(),"CAS collision must not overwrite evidence");
}
#[test]
fn source_import_rejects_file_symlink_and_ancestor_link(){
    let temp=tempfile::tempdir().unwrap();let (_,body)=fixture();let root=prepare_ingress(temp.path(),&body);let real=temp.path().join("real.media");fs::write(&real,b"media").unwrap();
    let input=root.join(format!("{}.media",text(&body,"artifactId")));
    #[cfg(unix)] std::os::unix::fs::symlink(&real,&input).unwrap();
    #[cfg(windows)] std::os::windows::fs::symlink_file(&real,&input).expect("symlink test requires developer mode or symlink privilege");
    assert!(copy_bounded(&root,&body).is_err());
    let linked=temp.path().join("linked");
    #[cfg(unix)] std::os::unix::fs::symlink(&root,&linked).unwrap();
    #[cfg(windows)] std::os::windows::fs::symlink_dir(&root,&linked).expect("symlink test requires developer mode or symlink privilege");
    assert!(copy_bounded(&linked,&body).is_err());
}

#[tokio::test]
async fn source_import_sqlite_reopen_preserves_receipt_and_inventory_checkpoint(){
    let (app,temp)=crate::tests::test_app().await;let (fixture,body)=fixture();
    let native_lifecycle=app.read().await.unwrap()["runtimeLifecycle"].clone();
    // The synthetic import workspace shares this App's current native owner;
    // whole-workspace fixture replacement must preserve its admission fence.
    app.change(|d|{let lifecycle=d["runtimeLifecycle"].clone();*d=fixture.clone();d["runtimeLifecycle"]=lifecycle;Ok(())}).await.unwrap();
    assert_eq!(app.read().await.unwrap()["runtimeLifecycle"],native_lifecycle);
    let expected=admit(&app.read().await.unwrap(),&body,AT).unwrap();
    let receipt=app.change(|d|commit(d,&body,"owner",&expected,&reference(&body),&proof(),AT)).await.unwrap();
    app.db.close().await;drop(app);
    let reopened=crate::storage::Database::Sqlite(crate::open_db(&temp.path().join("workspace.sqlite")).await.unwrap());
    let mut reloaded=reopened.read().await.unwrap();assert_eq!(reloaded["runtimeLifecycle"],native_lifecycle);
    crate::media_queue::recover(&mut reloaded,AT).unwrap();
    assert_eq!(replay(&reloaded,&body,"owner").unwrap(),Some(receipt));
    assert_eq!(reloaded["jobs"][0]["result"]["visualProgress"]["phase"],"inventory");
    reopened.close().await;
}

#[test]
fn source_import_consumed_retry_history_is_preserved_but_pending_or_unknown_is_blocked(){
    let (mut d,mut body)=fixture();let original=d["jobs"][0]["sourceAttempts"][0].clone();
    let permit=json!({"id":"old-consumed-permit","sourceVersion":body["sourceVersion"],"connectorBinding":body["connectorBinding"]});
    d["jobs"][0]["sourceAttempts"][0]["retryPermit"]=permit.clone();d["jobs"][0]["downloadRetry"]=permit;
    let mut retried=original.clone();retried["id"]=json!("retry-attempt");retried["retryOf"]=json!({"jobId":body["jobId"],"attemptIndex":0,"permitId":"old-consumed-permit"});
    d["jobs"][0]["sourceAttempts"].as_array_mut().unwrap().push(retried);body["attemptIndex"]=json!(1);
    let expected=admit(&d,&body,AT).unwrap();let history=d["jobs"][0]["sourceAttempts"].clone();
    let mut accepted=d.clone();commit(&mut accepted,&body,"owner",&expected,&reference(&body),&proof(),AT).unwrap();
    assert_eq!(&accepted["jobs"][0]["sourceAttempts"].as_array().unwrap()[..2],history.as_array().unwrap());
    for case in ["wrong-consumer","wrong-source","process-unknown","wait-failed"] {
        let mut changed=d.clone();match case {
            "wrong-consumer"=>changed["jobs"][0]["sourceAttempts"][1]["retryOf"]["jobId"]=json!("other"),
            "wrong-source"=>changed["jobs"][0]["sourceAttempts"][1]["sourceVersion"]=json!("other"),
            "process-unknown"=>changed["jobs"][0]["sourceAttempts"][0]["error"]=json!("source_download_failed_process_unknown"),
            _=>changed["jobs"][0]["sourceAttempts"][0]["error"]=json!("source_download_failed_wait_failed")
        };assert!(admit(&changed,&body,AT).is_err(),"{case}");
    }
}

fn offline_tools()->(PathBuf,PathBuf){
    let probe=PathBuf::from(std::env::var_os("COMMUNITYHERO_IMPORT_TEST_FFPROBE").expect("explicit existing ffprobe path"));
    let ffmpeg=PathBuf::from(std::env::var_os("COMMUNITYHERO_IMPORT_TEST_FFMPEG").expect("explicit existing ffmpeg path"));(probe,ffmpeg)
}
#[tokio::test]
#[ignore="explicit existing pinned ffmpeg/ffprobe; offline CPU only"]
async fn source_import_real_full_decode_rejects_silent_truncated_and_short_video(){
    let (probe,ffmpeg)=offline_tools();let temp=tempfile::tempdir().unwrap();
    for (kind,video_secs,audio_secs) in [("full",2,2),("mp4",2,2),("short-video",1,3),("silent",2,0)] {
        let output=temp.path().join(format!("{kind}.{}",if kind=="mp4"{"mp4"}else{"mkv"}));
        let mut command=std::process::Command::new(&ffmpeg);
        command.args(["-nostdin","-v","error","-f","lavfi","-i",&format!("color=c=blue:s=32x32:r=24:d={video_secs}")]);
        if audio_secs>0 {command.args(["-f","lavfi","-i",&format!("sine=frequency=440:duration={audio_secs}")]);}
        command.args(["-c:v",if kind=="mp4"{"mpeg4"}else{"ffv1"}]);if audio_secs>0{command.args(["-c:a",if kind=="mp4"{"aac"}else{"pcm_s16le"}]);}command.arg(&output);
        #[cfg(windows)] {use std::os::windows::process::CommandExt;command.creation_flags(0x08000000);}
        assert!(command.status().unwrap().success());
        let result=crate::media_processing::full::probe_retained_with_tools(&probe,&ffmpeg,&output,if kind=="short-video"{3000}else{2000}).await;
        if kind=="full" {
            assert!(result.is_ok(),"{result:?}");fs::copy(&output,temp.path().join("healthy.mkv")).unwrap();let bytes=fs::read(&output).unwrap();fs::write(&output,&bytes[..bytes.len()/2]).unwrap();
            assert!(crate::media_processing::full::probe_retained_with_tools(&probe,&ffmpeg,&output,2000).await.is_err());
        }else if kind=="mp4"{assert!(result.is_ok(),"{kind}: {result:?}");}else{assert!(result.is_err(),"{kind}: {result:?}");}
    }
    // Autodetected playlists could otherwise dereference files outside ingress
    // while only the small playlist itself was hashed. Neither demuxer is allowed.
    for (name,contents) in [("local.m3u8","#EXTM3U\n#EXT-X-TARGETDURATION:2\n#EXTINF:2.0,\nhealthy.mkv\n#EXT-X-ENDLIST\n"),("local.ffconcat","ffconcat version 1.0\nfile 'healthy.mkv'\nduration 2.0\n")] {
        let path=temp.path().join(name);fs::write(&path,contents).unwrap();
        if name=="local.ffconcat" {
            let mut control=std::process::Command::new(&probe);control.args(["-v","error","-show_entries","format=duration:stream=codec_type","-of","json"]).arg(&path);
            #[cfg(windows)] {use std::os::windows::process::CommandExt;control.creation_flags(0x08000000);}
            let result=control.output().unwrap();assert!(result.status.success(),"unsafe control must actually follow external reference");
            let followed:Value=serde_json::from_slice(&result.stdout).unwrap();assert_eq!(followed["format"]["duration"],"2.000000");assert_eq!(followed["streams"].as_array().unwrap().len(),2);
        }
        let result=crate::media_processing::full::probe_retained_with_tools(&probe,&ffmpeg,&path,2000).await;
        assert_eq!(result.unwrap_err(),"import_container_unsupported","{name}");
    }
}

#[tokio::test]
async fn source_import_preserves_duration_policy_and_routes_long_source_to_cached_audio(){
    // Keep the production preflight and scheduler path: this fixture needs a
    // configured audio runtime, but never launches it. A child process isolates
    // synthetic paths from other parallel tests and the developer's real tools.
    const CHILD:&str="COMMUNITYHERO_TEST_IMPORTED_AUDIO_RUNTIME";
    if std::env::var(CHILD).as_deref()!=Ok("1"){
        let temp=tempfile::tempdir().unwrap();let tool=temp.path().join("inert-audio-tool.fixture");
        fs::write(&tool,b"This fixture must never execute").unwrap();
        let mut command=std::process::Command::new(std::env::current_exe().unwrap());
        command.args(["--exact","media_source_import::tests::source_import_preserves_duration_policy_and_routes_long_source_to_cached_audio","--nocapture","--test-threads=1"])
            .env(CHILD,"1").env("COMMUNITYHERO_MEDIA_SCRATCH_DIR",temp.path())
            .env("COMMUNITYHERO_MEDIA_EVIDENCE_DIR",temp.path().join("evidence"));
        for key in ["COMMUNITYHERO_MEDIA_FFMPEG","COMMUNITYHERO_MEDIA_FFPROBE","COMMUNITYHERO_MEDIA_WHISPER_CLI","COMMUNITYHERO_MEDIA_WHISPER_MODEL"]{command.env(key,&tool);}
        for key in ["COMMUNITYHERO_GPU_GATE_FILE","COMMUNITYHERO_GPU_GATE_FILE_ID","COMMUNITYHERO_MEDIA_YTDLP","COMMUNITYHERO_MEDIA_YTDLP_PYTHON","COMMUNITYHERO_MEDIA_YTDLP_NODE","COMMUNITYHERO_MEDIA_VISION_BACKEND","COMMUNITYHERO_MEDIA_VISION_LOCAL_ENDPOINT","COMMUNITYHERO_MEDIA_VISION_LOCAL_MODEL","COMMUNITYHERO_MEDIA_VISION_LOCAL_DIGEST","COMMUNITYHERO_MEDIA_VISION_DATA_DIR","COMMUNITYHERO_MEDIA_TESSERACT","COMMUNITYHERO_MEDIA_TESSDATA_PREFIX"]{command.env_remove(key);}
        #[cfg(windows)] {use std::os::windows::process::CommandExt;command.creation_flags(0x08000000);}
        let output=command.output().unwrap();
        assert!(output.status.success(),"isolated import-to-audio fixture failed: {}\n{}",String::from_utf8_lossy(&output.stdout),String::from_utf8_lossy(&output.stderr));
        assert!(String::from_utf8_lossy(&output.stdout).contains("imported_audio_fixture_claimed"),"exact child selector must execute the reservation assertions");
        return;
    }
    crate::media_processing::preflight_phase("audio").expect("synthetic audio prerequisites must be admitted");
    assert!(crate::media_processing::preflight_phase("download").is_err(),"cached audio cannot require the downloader");
    assert!(crate::media_processing::preflight_phase("scan").is_err(),"cached audio cannot require vision");
    let (mut d,mut body)=fixture();d["settings"]["postMediaPolicies"]=json!({});
    d["posts"][0]["durationMs"]=json!(240000);body["durationMs"]=json!(240000);
    body["sourceVersion"]=json!(crate::media_fullframes::source_version(&d["posts"][0],text(&d,"account")));d["jobs"][0]["sourceAttempts"][0]["sourceVersion"]=body["sourceVersion"].clone();
    let expected=admit(&d,&body,AT).unwrap();let mut probe=proof();probe["durationMs"]=json!(240000);
    commit(&mut d,&body,"owner",&expected,&reference(&body),&probe,AT).unwrap();
    assert_eq!(crate::post_media_policy::effective(&d,&d["posts"][0]).unwrap()["mode"],"full_audio_only");
    assert!(crate::media_queue::claim_when_ready(&mut d,AT,true).unwrap().is_none(),"must not claim visual work for duration policy");
    assert_eq!(d["jobs"][0]["result"]["visualProgress"]["phase"],"inventory","retain source for cached audio");
    crate::media_fullframes::store().unwrap().put_bytes(b"media").unwrap();
    let (app,_temp)=crate::tests::test_app().await;
    app.db.change(|stored|{*stored=d.clone();crate::native_fixture_owner_repair::initialize_workspace(stored)?;Ok(())}).await.unwrap();
    let (id,pin)=crate::media_queue::imported_audio_candidate(&app).await.unwrap().expect("normal scheduler must claim cached audio");
    assert_eq!(pin["progress"]["source"],reference(&body).to_json());
    let readback=app.read().await.unwrap();let audio_job=crate::row(&readback,"jobs",&id).unwrap();
    assert_eq!(audio_job["kind"],"media_audio");assert_eq!(audio_job["status"],"running");
    assert!(audio_job["result"].is_null(),"reservation cannot invent an ASR result");
    assert_eq!(readback["jobs"][0]["sourceAttempts"],d["jobs"][0]["sourceAttempts"],"cached audio cannot redownload imported source");
    println!("imported_audio_fixture_claimed");
}
#[tokio::test]
#[ignore="explicit retained source; read-only source and temporary isolated CAS, no live DB"]
async fn source_import_verify_actual_retained_original(){
    let (probe,ffmpeg)=offline_tools();let path=PathBuf::from(std::env::var_os("COMMUNITYHERO_IMPORT_TEST_RETAINED").expect("explicit retained original"));
    let expected_sha=std::env::var("COMMUNITYHERO_IMPORT_TEST_SHA256").unwrap();let expected_bytes=std::env::var("COMMUNITYHERO_IMPORT_TEST_BYTES").unwrap().parse::<u64>().unwrap();let expected_ms=std::env::var("COMMUNITYHERO_IMPORT_TEST_DURATION_MS").unwrap().parse::<u64>().unwrap();
    let temp=tempfile::tempdir().unwrap();let (_,mut body)=fixture();body["sha256"]=json!(expected_sha);body["bytes"]=json!(expected_bytes);
    let root=prepare_ingress(temp.path(),&body);let ingress_file=root.join(format!("{}.media",text(&body,"artifactId")));fs::copy(&path,&ingress_file).unwrap();
    let staged=copy_bounded(&root,&body).unwrap();let verified=crate::media_processing::full::probe_retained_with_tools(&probe,&ffmpeg,&staged.0,expected_ms).await.unwrap();
    let store=ArtifactStore::open(&temp.path().join("isolated-cas")).unwrap();let source=store.put_file(&staged.0).unwrap();assert_eq!(source.sha256,expected_sha);assert_eq!(source.bytes,expected_bytes);
    println!("retained_source_verified {}",json!({"source":source.to_json(),"probe":verified,"liveDatabaseMutation":false,"remoteDownload":false}));
}
