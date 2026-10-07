use super::*;

fn oauth_fixture()->Value {json!({"version":1,"classification":"invalid_grant","responseShape":"json_object",
    "error":"invalid_grant","trigger":"after-401","generation":350,"observedAt":"2026-09-28T11:20:05.123Z"})}
fn oauth_error()->Value {json!({"code":"HTTP_ERROR","httpStatus":400,"transportStage":"read-auth-after-401-refresh-http",
    "adapterOperation":"context","oauthDiagnostic":oauth_fixture()})}

#[test]
fn oauth_failure_projection_is_closed_bounded_and_backward_compatible() {
    let mut error=oauth_error();error["oauthDiagnostic"]["secret"]=json!("private token");
    error["oauthDiagnostic"]["classification"]=json!("forged private category");
    let message=dispatch_evidence::adapter_failure(&error);assert!(message.len()<=512);assert!(!message.contains("private"));
    let failure=read_failure("fresh_context_read_failed",&internal(&message));
    assert_eq!(failure.evidence["diagnostic"]["oauthDiagnostic"],oauth_fixture());
    for bad in [message.replace("; oauthReason=invalid_grant","; oauthReason=revoked"),
        message.replace("; oauthBody=json_object","; oauthBody=json_object; oauthBody=unavailable"),
        message.replace("read-auth-after-401-refresh-http","read-http"),
        format!("{}{}",message,"x".repeat(600))] {
        assert!(read_failure("fresh_context_read_failed",&internal(&bad)).evidence["diagnostic"].get("oauthDiagnostic").is_none());
    }
    error.as_object_mut().unwrap().remove("oauthDiagnostic");
    let old=read_failure("fresh_context_read_failed",&internal(&dispatch_evidence::adapter_failure(&error)));
    assert!(old.evidence["diagnostic"].get("oauthDiagnostic").is_none());
    assert_eq!(old.evidence["providerRetryAllowed"],false);
}

#[tokio::test]
async fn oauth_failure_is_persisted_without_dispatch_or_retry() {
    let (app,_temp,op,log)=harness("context_oauth").await;
    assert_eq!(dispatch(app.clone(),op).await.unwrap(),Outcome::Failed);
    let data=app.read().await.unwrap();let evidence=&data["operations"][0]["evidence"];
    assert_eq!(evidence["diagnostic"]["oauthDiagnostic"],oauth_fixture());
    assert_eq!(evidence["providerRetryAllowed"],false);assert_eq!(evidence["providerCallAttempted"],false);
    assert_eq!(evidence["mutationOutcome"],"not-attempted");assert_eq!(calls(&log),["context"]);
    app.db.close().await;
}

#[test]
fn oauth_inline_receipt_retains_reason_without_retry_authority() {
    let op=json!({"action":{"actionId":"a","itemId":"i"}});
    let receipt=json!({"account":"likeavto","results":[{"actionId":"a","itemId":"i","status":"failed",
        "mutationOutcome":"not-attempted","phase":"catalogue","operation":"adapter-catalogue","oauthDiagnostic":oauth_fixture()}]});
    let evidence=known_failure_evidence(receipt,&op,"likeavto");
    assert_eq!(evidence["receipt"]["results"][0]["oauthDiagnostic"],oauth_fixture());
    assert_eq!(evidence["providerRetryAllowed"],false);assert_eq!(evidence["recovery"]["sameOperationRetryAllowed"],false);
}

#[tokio::test]
async fn oauth_failed_readback_retains_closed_error_and_verified_execute_without_retry() {
    let (app,_temp,op,log)=harness("readback_oauth").await;
    assert_eq!(dispatch(app.clone(),op).await.unwrap(),Outcome::Unknown);
    let data=app.read().await.unwrap();let saved=&data["operations"][0];
    assert_eq!(saved["executeReceipt"]["results"][0]["status"],"verified");
    assert_eq!(saved["dispatchPermit"]["phase"],"transport_settled");
    assert!(connection_gate::valid_permit(saved));
    assert_eq!(saved["evidence"]["executeObservation"]["status"],"verified");
    assert_eq!(saved["evidence"]["providerRetryAllowed"],false);
    let message=saved["evidence"]["error"].as_str().unwrap();
    assert!(!message.contains("private"));
    assert_eq!(read_failure("readback",&internal(message)).evidence["diagnostic"]["oauthDiagnostic"],oauth_fixture());
    assert_eq!(calls(&log),["context","execute","readback"]);
    app.db.close().await;
}

fn context()->Value {json!({"itemId":"c","objectId":"o","postKey":"o:p","conversationKey":"o:c","contextEvidenceDigest":"a".repeat(64)})}

fn visual_dispatch_fixture()->(Value,Value,Value) {
    let mut d=crate::empty();let profile=crate::accounts::Profile::LikeAvto;
    d["account"]=json!(profile.display());d["connectorBinding"]=profile.binding();
    let post_id=format!("proof-expiry-{}",uuid::Uuid::new_v4());
    d["posts"]=json!([{"id":post_id,"postKey":post_id,"title":post_id,"text":"Synthetic video","attachments":[{"type":"video"}]}]);
    d["branches"]=json!([{"id":"branch","postId":post_id,"messages":[{"id":"message","text":"Synthetic comment"}]}]);
    d["items"]=json!([{"id":"item","itemId":"external-item","objectId":"11391","postId":post_id,"branchId":"branch",
        "postKey":post_id,"conversationKey":"conversation","connectorBinding":profile.binding(),"platform":"instagram",
        "text":"Synthetic comment","revision":1,"workflow":"attention","providerStatus":"new",
        "contextEvidenceDigest":"a".repeat(64),"branchContextDigest":"b".repeat(64)}]);
    // Exercise the legacy generated visual dispatch path. New manual video
    // drafts instead require an exact semantic editorial receipt.
    let source_version=crate::media_fullframes::source_version(&d["posts"][0],profile.display());
    d["settings"]["postMediaPolicies"][post_id.as_str()]=json!({"version":1,"revision":1,
        "status":"active","postId":post_id,"account":profile.display(),
        "connectorBinding":profile.binding(),"sourceVersion":source_version,
        "mode":"full_audio_visual","reason":"Visual dispatch regression fixture"});
    let evidence=crate::media_fullframes::fixture_for_post(profile.display(),&d["posts"][0]);
    // Speech and visual evidence describe the same complete fixture source.
    let duration_seconds=evidence["source"]["durationMs"].as_u64().unwrap() as f64/1000.0;
    d["materials"]=json!([
        {"id":"speech","account":profile.display(),"postKey":post_id,"kind":"transcript","text":"Complete synthetic speech",
            "mediaSha256":evidence["source"]["mediaSha256"],
            "transcription":{"partial":false,"coverage":"full_audio","sourceVersion":source_version,
                "mediaDurationSeconds":duration_seconds,"audioDurationSeconds":duration_seconds}},
        {"id":"visual","account":profile.display(),"postKey":post_id,"kind":"visual_context","text":"Synthetic visual evidence",
            "mediaSha256":evidence["source"]["mediaSha256"],"visualEvidence":evidence}]);
    crate::knowledge::sync_catalog(&mut d,"2026-01-01T00:00:00Z").unwrap();
    let p=crate::create_generated_proposal(&mut d,&json!({"itemId":"item","kind":"close","expectedRevision":1})).unwrap();
    assert!(!crate::decision_media::enabled(&p));
    assert_eq!(d["settings"]["postMediaPolicies"][post_id.as_str()]["mode"],"full_audio_visual");
    let op=json!({"id":"operation","itemId":"item","proposalId":p["id"],"target":d["items"][0]});
    assert!(local_check(&d,&op).is_ok());(d,op,evidence)
}

#[test]
fn dispatch_visual_proof_expiry_changes_only_cache_and_reverify_restores_current_proposal() {
    let (d,op,evidence)=visual_dispatch_fixture();let unchanged=d.clone();
    crate::media_fullframes::expire_test_proof(&evidence);
    let failure=local_check(&d,&op).err().unwrap();
    assert_eq!(failure.evidence["code"],"local_precondition_changed");
    assert_eq!(failure.evidence["diagnostic"]["localPredicate"],"review_source_changed");
    assert_ne!(failure.evidence["diagnostic"]["expectedFingerprint"],failure.evidence["diagnostic"]["observedFingerprint"]);
    assert_eq!(failure.evidence["providerCallAttempted"],false);
    crate::media_fullframes::verify_and_cache(&evidence).unwrap();
    assert!(local_check(&d,&op).is_ok());assert_eq!(d,unchanged);
}

#[tokio::test]
async fn dispatch_visual_proof_refresh_recovers_expiry_without_warming_unrelated_head() {
    let (mut d,op,evidence)=visual_dispatch_fixture();
    let (other,_,unrelated)=visual_dispatch_fixture();
    d["posts"].as_array_mut().unwrap().push(other["posts"][0].clone());
    let mut material=other["materials"][1].clone();material["id"]=json!("unrelated-visual");
    d["materials"].as_array_mut().unwrap().push(material);
    crate::knowledge::sync_catalog(&mut d,"2026-01-01T00:00:00Z").unwrap();
    assert!(local_check(&d,&op).is_ok());let before=d.clone();
    for proof in [&evidence,&unrelated]{crate::media_fullframes::expire_test_proof(proof);}
    assert!(local_check(&d,&op).is_err());
    crate::knowledge::refresh_dispatch_visual_proofs(&d,"item").await.unwrap();
    assert!(local_check(&d,&op).is_ok());
    assert!(crate::media_fullframes::validate_evidence(&unrelated).is_err());assert_eq!(d,before);
}

#[tokio::test]
async fn dispatch_visual_proof_refresh_rejects_missing_or_changed_artifacts() {
    for missing in [true,false] {
        let (d,op,evidence)=visual_dispatch_fixture();let before=d.clone();
        let store=crate::media_fullframes::store().unwrap();
        let path=store.path(&crate::media_fullframes::reference(&evidence["finalEvidence"]).unwrap()).unwrap();
        if missing {std::fs::remove_file(path).unwrap();}else{std::fs::write(path,b"changed synthetic artifact").unwrap();}
        crate::knowledge::refresh_dispatch_visual_proofs(&d,"item").await.unwrap();
        assert!(crate::media_fullframes::validate_evidence(&evidence).is_err());
        let failure=local_check(&d,&op).err().unwrap();assert_eq!(failure.evidence["providerCallAttempted"],false);
        assert_eq!(d,before);
    }
}

#[tokio::test]
async fn dispatch_visual_proof_refresh_cannot_rewarm_changed_source_or_obsolete_head() {
    for changed_source in [true,false] {
        let (mut d,op,evidence)=visual_dispatch_fixture();
        crate::media_fullframes::expire_test_proof(&evidence);
        if changed_source {d["posts"][0]["text"]=json!("Different synthetic source");}
        else {
            d["materials"][1]["text"]=json!("Revised visual observation");
            let next=crate::media_fullframes::fixture_for_post("LikeAvto",&d["posts"][0]);
            // A current revision can reuse the same bytes; retire it to ensure
            // an old non-head proof never becomes dispatch authority again.
            d["materials"][1]["visualEvidence"]=next;
            crate::knowledge::sync_catalog(&mut d,"2026-01-01T00:00:00Z").unwrap();
            let entry=d["knowledge_entries"].as_array().unwrap().iter().find(|e|e["sourceMaterialId"]=="visual").unwrap().clone();
            crate::knowledge::revise(&mut d,entry["id"].as_str().unwrap(),&json!({"expectedVersionId":entry["currentVersionId"],"status":"retired"}),"2026-01-01T00:00:00Z").unwrap();
            crate::media_fullframes::expire_test_proof(&evidence);
        }
        let before=d.clone();crate::knowledge::refresh_dispatch_visual_proofs(&d,"item").await.unwrap();
        assert!(crate::media_fullframes::validate_evidence(&evidence).is_err());
        assert!(local_check(&d,&op).is_err());assert_eq!(d,before);
    }
}

#[tokio::test]
async fn dispatch_visual_proof_refresh_rejects_foreign_recipient_before_artifact_access() {
    let (mut d,op,evidence)=visual_dispatch_fixture();
    crate::media_fullframes::expire_test_proof(&evidence);
    d["items"][0]["connectorBinding"]=crate::accounts::Profile::BawRussia.binding();
    let before=d.clone();crate::knowledge::refresh_dispatch_visual_proofs(&d,"item").await.unwrap();
    assert!(local_check(&d,&op).is_err());
    assert!(crate::media_fullframes::validate_evidence(&evidence).is_err());assert_eq!(d,before);
}

#[tokio::test]
async fn dispatch_visual_proof_refresh_preserves_legacy_checks_without_applicable_evidence() {
    let (app,_temp,op,_log)=harness("success").await;
    let mut d=app.read().await.unwrap();
    assert!(d["items"][0].get("branchId").is_none());assert!(local_check(&d,&op).is_ok());
    for unrelated in [false,true] {
        if unrelated {
            let (other,_,_)=visual_dispatch_fixture();
            for field in ["posts","materials","knowledge_entries","knowledge_versions"] {d[field]=other[field].clone();}
        }
        let before=d.clone();crate::knowledge::refresh_dispatch_visual_proofs(&d,"item-1").await.unwrap();
        assert!(local_check(&d,&op).is_ok());assert_eq!(d,before);
        let mut stale=d.clone();bump(&mut stale["items"][0]);
        crate::knowledge::refresh_dispatch_visual_proofs(&stale,"item-1").await.unwrap();
        assert_eq!(local_check(&stale,&op).err().unwrap().evidence["code"],"local_target_revision_changed");
    }
    app.db.close().await;
}

#[test]
fn typed_context_causes_are_distinct_and_fingerprints_exclude_content() {
    let target=context();
    for (field,code,outcome) in [("itemId","fresh_context_identity_changed",Outcome::Stale),
        ("contextEvidenceDigest","fresh_context_digest_changed",Outcome::Stale)] {
        let mut fresh=target.clone();fresh[field]=json!("private-value");fresh["text"]=json!("private comment");
        let failure=context_check(&fresh,&target).err().unwrap();
        assert_eq!(failure.outcome,outcome);assert_eq!(failure.evidence["code"],code);
        assert_eq!(failure.evidence["mismatchedFields"],json!([field]));
        assert!(!failure.evidence.to_string().contains("private"));
        assert_eq!(failure.evidence["providerCallAttempted"],false);
    }
    let mut malformed=target.clone();malformed["itemId"]=Value::Null;
    assert_eq!(context_check(&malformed,&target).err().unwrap().evidence["code"],"fresh_context_schema_invalid");
    assert!(context_check(&target,&target).is_ok());
    let error=internal("Adapter failed (TRANSPORT_ERROR; operation=context)");
    let bridge=read_failure("fresh_context_read_failed",&error);
    assert_eq!(bridge.outcome,Outcome::Failed);assert_eq!(bridge.evidence["diagnostic"]["adapterCode"],"TRANSPORT_ERROR");
    let local=read_failure("local_context_read_failed",&internal("private connection detail"));
    assert_eq!(local.evidence["code"],"local_context_read_failed");
    assert!(!local.evidence.to_string().contains("private"));
}

#[test]
fn read_failure_keeps_upstream_http_status_separate_from_engine_status() {
    for upstream in [403,429,500] {
        let formatted=dispatch_evidence::adapter_failure(&json!({"code":"HTTP_ERROR",
            "httpStatus":upstream,"adapterOperation":"context","message":"private body","token":"private token"}));
        let failure=read_failure("fresh_context_read_failed",&internal(&formatted));
        assert_eq!(failure.evidence["diagnostic"],json!({"adapterCode":"HTTP_ERROR","httpStatus":500,"nativeHttpStatus":500,"upstreamHttpStatus":upstream}));
        assert_eq!(failure.outcome,Outcome::Failed);
        assert_eq!(failure.evidence["mutationOutcome"],"not-attempted");
        assert_eq!(failure.evidence["providerCallAttempted"],false);
        assert_eq!(failure.evidence["providerRetryAllowed"],false);
        assert!(!failure.evidence.to_string().contains("private"));
    }
}

#[test]
fn read_failure_does_not_invent_upstream_status_from_malformed_or_private_text() {
    // The typed auth-exchange formatter now has a 2048-byte envelope budget.
    // Keep this adversarial fixture above that budget, not the old 512 bytes.
    let oversized=format!("Adapter failed (HTTP_ERROR; {}; httpStatus=403)","private".repeat(300));
    assert!(oversized.len()>2048);
    for message in ["private upstream HTTP 403",
        "Adapter failed (HTTP_ERROR)",
        "Adapter failed (TRANSPORT_ERROR; httpStatus=403)",
        "Adapter failed (HTTP_ERROR; httpStatus=099)",
        "Adapter failed (HTTP_ERROR; httpStatus=600)",
        "Adapter failed (HTTP_ERROR; httpStatus=-1)",
        "Adapter failed (HTTP_ERROR; httpStatus=403.0)",
        "Adapter failed (HTTP_ERROR; httpStatus=0403)",
        "Adapter failed (HTTP_ERROR; httpStatus=private)",
        "Adapter failed (HTTP_ERROR; httpStatus=403; httpStatus=429)",
        "Adapter failed (HTTP_ERROR; httpStatus=403) private",
        oversized.as_str()] {
        let failure=read_failure("fresh_context_read_failed",&internal(message));
        assert_eq!(failure.evidence["diagnostic"]["httpStatus"],500);
        assert_eq!(failure.evidence["diagnostic"]["nativeHttpStatus"],500);
        assert!(failure.evidence["diagnostic"].get("upstreamHttpStatus").is_none(),"{message}");
        assert!(!failure.evidence.to_string().contains("private"));
    }
}

#[test]
fn read_failure_preserves_only_closed_transport_stage_independently_of_http_status() {
    for stage in ["read-fetch","read-json","read-auth","read-http","read-auth-proactive-token-state","read-auth-after-401-refresh-lock-timeout","read-auth-proactive-credential-helper-startup","read-auth-proactive-credential-helper-timeout-before-connect","read-auth-proactive-credential-helper-timeout-after-connect","read-auth-proactive-credential-helper-transport","read-auth-proactive-credential-helper-response-invalid","read-auth-proactive-credential-helper-failed","read-auth-proactive-credential-missing","read-auth-proactive-credential-value-invalid","read-auth-proactive-token-envelope-invalid","read-auth-proactive-token-freshness-boundary","read-auth-proactive-token-lifecycle-blocked","read-auth-after-401-credential-helper-startup","read-auth-after-401-credential-helper-timeout-before-connect","read-auth-after-401-credential-helper-timeout-after-connect","read-auth-after-401-credential-helper-transport","read-auth-after-401-credential-helper-response-invalid","read-auth-after-401-credential-helper-failed","read-auth-after-401-credential-missing","read-auth-after-401-credential-value-invalid","read-auth-after-401-token-envelope-invalid","read-auth-after-401-token-freshness-boundary","read-auth-after-401-token-lifecycle-blocked"] {
        let formatted=dispatch_evidence::adapter_failure(&json!({"code":"TRANSPORT_ERROR","transportStage":stage,"message":"private"}));
        let failure=read_failure("fresh_context_read_failed",&internal(&formatted));
        assert_eq!(failure.evidence["diagnostic"],json!({"adapterCode":"TRANSPORT_ERROR","httpStatus":500,"nativeHttpStatus":500,"transportStage":stage}));
        assert_eq!(failure.evidence["providerRetryAllowed"],false);
        assert_eq!(failure.evidence["providerCallAttempted"],false);
        assert!(!failure.evidence.to_string().contains("private"));
    }
    for upstream in [403,429,500] {
        let formatted=dispatch_evidence::adapter_failure(&json!({"code":"HTTP_ERROR","httpStatus":upstream,"transportStage":"read-auth-proactive-refresh-http"}));
        let failure=read_failure("fresh_context_read_failed",&internal(&formatted));
        assert_eq!(failure.evidence["diagnostic"],json!({"adapterCode":"HTTP_ERROR","httpStatus":500,"nativeHttpStatus":500,"upstreamHttpStatus":upstream,"transportStage":"read-auth-proactive-refresh-http"}));
    }
    for message in ["Adapter failed (TRANSPORT_ERROR; transportStage=private)",
        "Adapter failed (TRANSPORT_ERROR; transportStage=read-auth-unknown-token-state)",
        "Adapter failed (TRANSPORT_ERROR; transportStage=read-fetch; transportStage=read-json)",
        "Adapter failed (TRANSPORT_ERROR; transportStage=read-fetch) private",
        "Adapter failed (RESPONSE_SCHEMA_ERROR; transportStage=read-json)"] {
        let failure=read_failure("fresh_context_read_failed",&internal(message));
        assert!(failure.evidence["diagnostic"].get("transportStage").is_none());
        assert!(!failure.evidence.to_string().contains("private"));
    }
}

#[test]
fn read_failure_preserves_only_closed_fetch_cause_without_granting_retry() {
    for cause in ["dns","tcp","tls","connect_timeout","timeout","abort","unknown"] {
        let formatted=dispatch_evidence::adapter_failure(&json!({"code":"TRANSPORT_ERROR",
            "transportStage":"read-fetch","transportCause":cause,"message":"private token"}));
        let failure=read_failure("fresh_context_read_failed",&internal(&formatted));
        assert_eq!(failure.evidence["diagnostic"],json!({"adapterCode":"TRANSPORT_ERROR",
            "httpStatus":500,"nativeHttpStatus":500,"transportStage":"read-fetch","transportCause":cause}));
        assert_eq!(failure.evidence["providerRetryAllowed"],false);
        assert!(!failure.evidence.to_string().contains("private"));
    }
    for message in [
        "Adapter failed (TRANSPORT_ERROR; transportStage=read-fetch; transportCause=private)",
        "Adapter failed (TRANSPORT_ERROR; transportStage=read-json; transportCause=dns)",
        "Adapter failed (TRANSPORT_ERROR; transportStage=read-fetch; transportCause=dns; transportCause=tcp)",
        "Adapter failed (HTTP_ERROR; transportStage=read-http; transportCause=dns)"
    ] {
        let failure=read_failure("fresh_context_read_failed",&internal(message));
        assert!(failure.evidence["diagnostic"].get("transportCause").is_none());
        assert!(!failure.evidence.to_string().contains("private"));
    }
}

#[test]
fn execute_observation_requires_exact_provider_neutral_binding() {
    let op=json!({"target":{"connectorBinding":accounts::Profile::LikeAvto.binding(),"objectId":"11391","itemId":"i","postKey":"11391:p","conversationKey":"11391:i"},
        "action":{"actionId":"a","itemId":"i"},
        "executeReceipt":{"account":"likeavto","results":[{"actionId":"a","itemId":"i","status":"verified"}]}});
    let mut evidence=json!({"phase":"readback"});retain_execute_observation(&op,&mut evidence);
    assert_eq!(evidence["executeObservation"]["status"],"verified");
    for (path,value) in [("account",json!("baw-russia")),("account",Value::Null),("actionId",json!("other")),("itemId",json!("other")),("status",json!("unknown"))] {
        let mut changed=op.clone();
        if path=="account" {changed["executeReceipt"][path]=value;}else{changed["executeReceipt"]["results"][0][path]=value;}
        let mut evidence=json!({});retain_execute_observation(&changed,&mut evidence);
        assert!(evidence.get("executeObservation").is_none());
    }
    let mut duplicate=op.clone();let row=duplicate["executeReceipt"]["results"][0].clone();duplicate["executeReceipt"]["results"].as_array_mut().unwrap().push(row);
    let mut evidence=json!({});retain_execute_observation(&duplicate,&mut evidence);assert!(evidence.get("executeObservation").is_none());
    let mut forged=json!({"account":"foreign","executeObservation":{"status":"verified"}});
    retain_execute_observation(&duplicate,&mut forged);
    assert!(forged.get("executeObservation").is_none());
    for field in ["actionId","itemId"] {
        for invalid in [Value::Null,json!(""),json!(42)] {
            let mut changed=op.clone();changed["action"][field]=invalid.clone();changed["executeReceipt"]["results"][0][field]=invalid;
            assert!(!readback_confirmed(&changed["executeReceipt"],&changed["action"]));
            let mut evidence=json!({});retain_execute_observation(&changed,&mut evidence);
            assert!(evidence.get("executeObservation").is_none());
        }
        let mut missing=op.clone();missing["action"].as_object_mut().unwrap().remove(field);
        missing["executeReceipt"]["results"][0].as_object_mut().unwrap().remove(field);
        assert!(!readback_confirmed(&missing["executeReceipt"],&missing["action"]));
    }
}

#[test]
fn catalogue_recovery_classification_never_authorizes_replay() {
    let op=json!({"action":{"actionId":"a","itemId":"i"}});
    let receipt=json!({"account":"likeavto","results":[{"actionId":"a","itemId":"i","status":"failed",
        "mutationOutcome":"not-attempted","phase":"catalogue","operation":"adapter-catalogue"}]});
    let evidence=known_failure_evidence(receipt.clone(),&op,"likeavto");
    assert_eq!(evidence["providerRetryAllowed"],false);assert_eq!(evidence["recovery"]["newOperationOnly"],true);
    assert_eq!(evidence["recovery"]["sameOperationRetryAllowed"],false);
    for field in ["actionId","itemId","status","mutationOutcome","phase","operation"] {
        let mut changed=receipt.clone();changed["results"][0][field]=json!("other");
        assert!(known_failure_evidence(changed,&op,"likeavto").get("recovery").is_none(),"{field}");
    }
    assert!(known_failure_evidence(receipt,&op,"baw-russia").get("recovery").is_none());
}

async fn harness(mode:&str)->(App,tempfile::TempDir,Value,PathBuf) {
    let (mut app,temp)=crate::tests::test_app().await;
    app.node=PathBuf::from("C:/Users/hello/AppData/Local/Microsoft/WinGet/Packages/OpenJS.NodeJS.LTS_Microsoft.Winget.Source_8wekyb3d8bbwe/node-v24.15.0-win-x64/node.exe");
    app.bridge=temp.path().join("dispatch-diagnostic-fixture.mjs");
    let op=app.change(|data| {
        connection_gate::fixture_open(data)?;
        let proposal=create_proposal(data,&json!({"itemId":"item-1","kind":"close","expectedRevision":1}))?;
        let target=proposal_current(data,&proposal)?;
        let actor=operator_auth::Actor::local_owner("test");
        let authority=dispatch_authority::approval_binding(&actor);
        let op=json!({"id":"operation","itemId":"item-1","proposalId":proposal["id"],"approvalId":"approval","attemptId":"attempt",
            "action":action_for(&proposal,&target,"operation")?,"target":target,"status":"dispatching","createdAt":now(),
            "approvedBy":actor.public_json(),"executedBy":actor.public_json(),"dispatchAuthority":{"approved":authority,"executed":authority}});
        list_mut(data,"operations").push(op.clone());Ok(op)
    }).await.unwrap();
    let log=temp.path().join("calls.jsonl");
    let script=r#"import {appendFile} from 'node:fs/promises';
let raw='';for await(const chunk of process.stdin)raw+=chunk;const r=JSON.parse(raw),mode=__MODE__,target=__TARGET__;
await appendFile(__LOG__,JSON.stringify({operation:r.operation})+'\n');let result;
if ((mode==='context_oauth'&&r.operation==='context')||(mode==='readback_oauth'&&r.operation==='readback')) {
 process.stdout.write(JSON.stringify({ok:false,error:{...__OAUTH_ERROR__,message:'private',body:'private'}}));process.exit(0);
}
if(r.operation==='context'){
 if(mode==='context_error'){process.stdout.write(JSON.stringify({ok:false,error:{code:'TRANSPORT_ERROR',message:'private'}}));process.exit(0);}
 if(mode==='context_transport_stage'){process.stdout.write(JSON.stringify({ok:false,error:{code:'TRANSPORT_ERROR',transportStage:'read-fetch',adapterOperation:'context',message:'private'}}));process.exit(0);}
 if(mode.startsWith('context_http_')){process.stdout.write(JSON.stringify({ok:false,error:{code:'HTTP_ERROR',httpStatus:Number(mode.slice('context_http_'.length)),adapterOperation:'context',message:'private body'}}));process.exit(0);}
 result=target;
 if(mode==='identity_changed')result.itemId='another';
 if(mode==='digest_changed')result.contextEvidenceDigest='b'.repeat(64);
 if(mode==='schema_invalid')delete result.itemId;
}else{
 const a=r.actions[0];
 if(r.operation==='readback'&&mode==='readback_error'){process.stdout.write(JSON.stringify({ok:false,error:{code:'TRANSPORT_ERROR'}}));process.exit(0);}
 result={account:mode==='readback_foreign'&&r.operation==='readback'?'baw-russia':r.account,results:[{actionId:a.actionId,itemId:a.itemId,status:'verified'}]};
 if(r.operation==='execute'&&mode==='known_failure')result.results[0]={actionId:a.actionId,itemId:a.itemId,status:'failed',mutationOutcome:'not-attempted'};
}
process.stdout.write(JSON.stringify({ok:true,result}));"#
        .replace("__OAUTH_ERROR__",&oauth_error().to_string())
        .replace("__MODE__",&json!(mode).to_string()).replace("__TARGET__",&op["target"].to_string())
        .replace("__LOG__",&json!(log.to_string_lossy()).to_string());
    std::fs::write(&app.bridge,script).unwrap();
    (app,temp,op,log)
}
fn calls(log:&PathBuf)->Vec<String> {std::fs::read_to_string(log).unwrap_or_default().lines()
    .map(|line|serde_json::from_str::<Value>(line).unwrap()["operation"].as_str().unwrap().to_owned()).collect()}

#[tokio::test]
async fn upstream_http_failures_cross_bridge_without_dispatching_mutation() {
    for upstream in [403,429,500] {
        let (app,_temp,op,log)=harness(&format!("context_http_{upstream}")).await;
        assert_eq!(dispatch(app.clone(),op).await.unwrap(),Outcome::Failed);
        let data=app.read().await.unwrap();let saved=&data["operations"][0];
        assert_eq!(saved["status"],"failed");
        assert_eq!(saved["evidence"]["diagnostic"],json!({"adapterCode":"HTTP_ERROR","httpStatus":500,"nativeHttpStatus":500,"upstreamHttpStatus":upstream}));
        assert_eq!(saved["evidence"]["providerCallAttempted"],false);
        assert_eq!(saved["evidence"]["providerRetryAllowed"],false);
        assert_eq!(saved["evidence"]["mutationOutcome"],"not-attempted");
        assert!(saved.get("executeReceipt").is_none());
        assert!(!saved["evidence"].to_string().contains("private"));
        assert_eq!(calls(&log),["context"]);
        app.db.close().await;
    }
}

#[tokio::test]
async fn transport_stage_reaches_durable_predispatch_receipt_without_mutation() {
    let (app,_temp,op,log)=harness("context_transport_stage").await;
    assert_eq!(dispatch(app.clone(),op).await.unwrap(),Outcome::Failed);
    let data=app.read().await.unwrap();let evidence=&data["operations"][0]["evidence"];
    assert_eq!(evidence["diagnostic"],json!({"adapterCode":"TRANSPORT_ERROR","httpStatus":500,"nativeHttpStatus":500,"transportStage":"read-fetch"}));
    assert_eq!(evidence["mutationOutcome"],"not-attempted");
    assert_eq!(evidence["providerCallAttempted"],false);
    assert_eq!(evidence["providerRetryAllowed"],false);
    assert_eq!(calls(&log),["context"]);
    assert!(!evidence.to_string().contains("private"));
    app.db.close().await;
}

#[tokio::test]
async fn dispatch_predispatch_causes_never_execute_and_keep_failed_distinct_from_stale() {
    for (mode,expected,code) in [("context_error",Outcome::Failed,"fresh_context_read_failed"),
        ("identity_changed",Outcome::Stale,"fresh_context_identity_changed"),("digest_changed",Outcome::Stale,"fresh_context_digest_changed"),
        ("schema_invalid",Outcome::Failed,"fresh_context_schema_invalid"),("local_revision",Outcome::Stale,"local_target_revision_changed")] {
        let (app,_temp,op,log)=harness(mode).await;
        if mode=="local_revision" {app.change(|d|{bump(&mut d["items"][0]);Ok(())}).await.unwrap();}
        assert_eq!(dispatch(app.clone(),op).await.unwrap(),expected,"{mode}");
        let data=app.read().await.unwrap();assert_eq!(data["operations"][0]["status"],expected.status());
        assert_eq!(data["operations"][0]["evidence"]["code"],code);
        assert_eq!(calls(&log),["context"]);
        app.db.close().await;
    }
}

#[tokio::test]
async fn verified_execute_then_failed_readback_retains_positive_evidence_without_claiming_success() {
    for mode in ["readback_error","readback_foreign","success","known_failure"] {
        let (app,_temp,op,log)=harness(mode).await;
        let expected=match mode {"success"=>Outcome::Succeeded,"known_failure"=>Outcome::Failed,_=>Outcome::Unknown};
        assert_eq!(dispatch(app.clone(),op.clone()).await.unwrap(),expected);
        let data=app.read().await.unwrap();let saved=&data["operations"][0];
        assert_eq!(saved["status"],expected.status());assert_eq!(calls(&log).iter().filter(|op|*op=="execute").count(),1);
        assert_eq!(saved["dispatchPermit"]["phase"],"transport_settled");
        assert!(connection_gate::valid_permit(saved));
        if mode=="known_failure" {assert_eq!(calls(&log),["context","execute"]);}
        else {assert_eq!(saved["executeReceipt"]["results"][0]["status"],"verified");
            assert_eq!(saved["evidence"]["executeObservation"]["status"],"verified");}
        if mode=="readback_error" {
            assert_eq!(saved["evidence"]["verificationPhase"],"unavailable");
            assert_eq!(saved["evidence"]["providerRetryAllowed"],false);
            assert_eq!(data["items"][0]["providerStatus"],"new");
            let receipt=saved["executeReceipt"].clone();
            let script=std::fs::read_to_string(&app.bridge).unwrap().replace("mode=\"readback_error\"","mode=\"success\"");
            std::fs::write(&app.bridge,script).unwrap();
            assert!(reconcile_one(&app,&op).await.unwrap());
            let after=app.read().await.unwrap();
            assert_eq!(after["operations"][0]["status"],"succeeded");
            assert_eq!(after["operations"][0]["executeReceipt"],receipt);
            assert_eq!(calls(&log).iter().filter(|op|*op=="execute").count(),1);
        }
        app.db.close().await;
    }
}

#[test]
fn local_predicate_keeps_first_failure_when_review_source_and_route_both_change() {
    let (mut d,op,_)=visual_dispatch_fixture();
    d["posts"][0]["text"]=json!("private changed source https://fixture.invalid/private");
    d["proposals"][0]["routeTarget"]["connectorBinding"]=accounts::Profile::BawRussia.binding();
    let before=d.clone();
    let failure=local_check(&d,&op).err().unwrap();
    assert_eq!(failure.evidence["diagnostic"]["localPredicate"],"review_source_changed");
    assert!(!failure.evidence.to_string().contains("private"));
    assert!(!failure.evidence.to_string().contains("https://"));
    assert_eq!(d,before);
    // Removing only the first failed condition exposes the next original check.
    d["proposals"][0]["reviewContextDigest"]=json!(prepare_bundle::review_fingerprint(&d,"item").unwrap());
    let failure=local_check(&d,&op).err().unwrap();
    assert_eq!(failure.evidence["diagnostic"]["localPredicate"],"route");
}

#[test]
fn local_predicates_distinguish_recovery_bundle_video_and_item_preconditions() {
    for expected in ["recovered_preparation","preparation_provenance","preparation_sources","video_evidence",
        "proposal_item_context","item_already_closed","connector_binding","target_binding","reply_constraints"] {
        let (mut d,op,evidence)=visual_dispatch_fixture();
        // Legacy proposals without review fingerprints still use every later
        // guard. This is a fixture choice, never a production guard bypass.
        d["proposals"][0].as_object_mut().unwrap().remove("reviewContextDigest");
        match expected {
            "recovered_preparation"=>d["proposals"][0]["recovery"]=json!({"kind":"recovered_context_metadata"}),
            "preparation_provenance"=>d["proposals"][0]["prepareRunId"]=json!("missing-fixture-job"),
            "preparation_sources"=>{
                d["jobs"].as_array_mut().unwrap().push(json!({"id":"fixture-job","prepareBundle":{"id":"bundle","digest":"digest"}}));
                d["proposals"][0]["prepareRunId"]=json!("fixture-job");
                d["proposals"][0]["prepareBundleId"]=json!("bundle");
                d["proposals"][0]["prepareBundleDigest"]=json!("digest");
            },
            "video_evidence"=>crate::media_fullframes::expire_test_proof(&evidence),
            "proposal_item_context"=>d["proposals"][0]["branchContextDigest"]=json!("changed"),
            "item_already_closed"=>d["items"][0]["workflow"]=json!("closed"),
            "connector_binding"=>d["connectorBinding"]=json!({}),
            "target_binding"=>d["items"][0]["connectorBinding"]=accounts::Profile::BawRussia.binding(),
            "reply_constraints"=>{
                d["proposals"][0]["kind"]=json!("reply_and_close");
                d["proposals"][0]["text"]=Value::Null;
            },
            _=>unreachable!(),
        }
        let before=d.clone();let failure=local_check(&d,&op).err().unwrap();
        assert_eq!(failure.outcome,Outcome::Stale,"{expected}");
        assert_eq!(failure.evidence["diagnostic"],json!({"localPredicate":expected}));
        assert_eq!(failure.evidence["code"],"local_precondition_changed");
        assert_eq!(failure.evidence["mutationOutcome"],"not-attempted");
        assert_eq!(failure.evidence["providerCallAttempted"],false);
        assert_eq!(failure.evidence["providerRetryAllowed"],false);assert_eq!(d,before);
        let wrapped=crate::proposal_current(&d,&d["proposals"][0]).err().unwrap();
        let typed=crate::proposal_current_checked(&d["proposals"][0],&prepare_bundle::EvidenceContext::new(&d)).err().unwrap();
        assert_eq!(wrapped.0,typed.error.0);assert_eq!(wrapped.1,typed.error.1);
    }
}

#[test]
fn local_review_unavailable_has_no_invented_fingerprint_or_later_route_reason() {
    let (mut d,op,_)=visual_dispatch_fixture();
    d["branches"]=json!([]);
    d["proposals"][0]["routeTarget"]["connectorBinding"]=accounts::Profile::BawRussia.binding();
    let failure=local_check(&d,&op).err().unwrap();
    assert_eq!(failure.outcome,Outcome::Stale);
    assert_eq!(failure.evidence["diagnostic"],json!({"localPredicate":"review_source_unavailable"}));
    assert_eq!(failure.evidence["providerCallAttempted"],false);
    assert_eq!(failure.evidence["providerRetryAllowed"],false);
}

#[test]
fn local_predicate_diagnostic_rejects_raw_values_and_preserves_server_failure_classification() {
    let private="private token https://fixture.invalid/private";
    assert!(safe_review_fingerprints(private,&"a".repeat(64)).is_none());
    assert!(safe_review_fingerprints(&"g".repeat(64),&"a".repeat(64)).is_none());
    let failure=LocalPreconditionFailure {error:conflict(private),predicate:LocalPredicate::ReviewSourceChanged,
        review_fingerprints:Some((private.into(),private.into()))}.into_stop();
    assert_eq!(failure.evidence["diagnostic"],json!({"localPredicate":"review_source_changed"}));
    assert!(!failure.evidence.to_string().contains("private"));
    let failure=LocalPreconditionFailure {error:internal(private),predicate:LocalPredicate::ReviewSourceUnavailable,
        review_fingerprints:None}.into_stop();
    assert_eq!(failure.outcome,Outcome::Failed);
    assert_eq!(failure.evidence["code"],"local_validation_unavailable");
    assert_eq!(failure.evidence["diagnostic"],json!({"httpStatus":500,"nativeHttpStatus":500}));
    assert_eq!(failure.evidence["providerCallAttempted"],false);
    assert_eq!(failure.evidence["providerRetryAllowed"],false);
    assert_eq!(failure.evidence["mutationOutcome"],"not-attempted");
    assert!(!failure.evidence.to_string().contains("private"));
}
