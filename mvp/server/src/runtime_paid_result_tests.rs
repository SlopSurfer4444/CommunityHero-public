//! Authored native fixtures. UNRUN in the source-only R9 worker.
use super::*;

fn captured()->Value {
    let request=json!({"prompt":"complete original prompt", "context":{"history":["old observation"],"fullAudio":"speech", "ocr":"observed target overlay"}});
    let response=json!({"text":"paid reply", "usage":{"tokens":25},"unknownExtension":{"kept":true}});
    json!({"version":2,"kind":"retained-native-paid-stage-result","company":"likeavto","account":"LikeAvto",
        "runtimeOwner":{"account":"LikeAvto","runtimeId":"fixture-owner","releaseSha256":"a".repeat(64)},
        "binding":{"nativeJobId":"job","operation":"assistant","itemId":"item"},"requestSha256":digest(&request),
        "responseSha256":digest(&response),"request":request,"response":response,"retryAuthorized":false,"dispatchAuthorized":false})
}
fn fixture()->(Value,crate::runtime_lifecycle::RuntimeIdentity) {
    let identity=crate::runtime_lifecycle::RuntimeIdentity{account:"LikeAvto".into(),runtime_id:"fixture-owner".into(),release_sha256:"a".repeat(64)};
    let token=crate::runtime_lifecycle::OwnerToken{account:identity.account.clone(),runtime_id:identity.runtime_id.clone(),release_sha256:identity.release_sha256.clone(),epoch:1};
    let mut d=json!({"account":"LikeAvto","connectorBinding":{},"jobs":[{"id":"job","kind":"assistant","status":"running","result":{"oldPaid":"keep"}}],
        "operations":[{"id":"unknown","status":"unknown","attemptId":"existing"}],"approvals":[],"audit":[],"materials":[],"knowledge_entries":[],"knowledge_versions":[]});
    let digest=crate::runtime_lifecycle::ledger_digest(&d).unwrap();
    crate::runtime_lifecycle::initialize(&mut d,token,&"b".repeat(64),&digest).unwrap();
    (d,identity)
}
#[test]
fn restart_exact_capture_and_legacy_job_preservation() {
    let dir=tempfile::tempdir().unwrap();let root=dir.path().join("company-cas");
    let store=ArtifactStore::open(&root).unwrap();let original=captured();
    let artifact=store.put_bytes(original.to_string().as_bytes()).unwrap();let reference=pointer(&original,&artifact);
    let (mut d,identity)=fixture();let before=d.clone();
    attach(&mut d,"job",&reference,&identity).unwrap();
    attach(&mut d,"job",&reference,&identity).unwrap();
    assert_eq!(d["jobs"][0]["retainedEvidence"],json!([reference]));
    let mut old_reader=d.clone();old_reader["jobs"][0].as_object_mut().unwrap().remove("retainedEvidence");
    assert_eq!(old_reader,before,"additive field is the ONLY logical mutation");
    // Durable JSON roundtrip and reopening the store model restart, not a warm cache.
    let restarted:Value=serde_json::from_slice(&serde_json::to_vec(&d).unwrap()).unwrap();drop(store);
    let reopened=ArtifactStore::open(&root).unwrap();
    assert_eq!(resolve_from(&reopened,"likeavto","LikeAvto",Some("job"),"assistant",&restarted["jobs"][0]["retainedEvidence"][0]).unwrap(),original);
    assert_eq!(restarted["operations"][0]["status"],"unknown");
    assert!(!original["request"]["context"]["fullAudio"].is_null());
    assert!(!original["request"]["context"]["ocr"].is_null());
}
#[test]
fn reference_is_not_retargetable_or_permission() {
    let dir=tempfile::tempdir().unwrap();let store=ArtifactStore::open(&dir.path().join("cas")).unwrap();let original=captured();
    let artifact=store.put_bytes(original.to_string().as_bytes()).unwrap();let reference=pointer(&original,&artifact);
    for (company,account,job,operation) in [("baw-russia","BAW Russia","job","assistant"),("likeavto","LikeAvto","foreign","assistant"),("likeavto","LikeAvto","job","media")] {
        assert!(resolve_from(&store,company,account,Some(job),operation,&reference).is_err());
    }
    for pointer_path in ["/binding/itemId","/runtimeOwner/runtimeId","/requestSha256","/responseSha256","/artifact/bytes","/dispatchAuthorized","/retryAuthorized"] {
        let mut forged=reference.clone();*forged.pointer_mut(pointer_path).unwrap()=json!("changed");
        assert!(resolve_from(&store,"likeavto","LikeAvto",Some("job"),"assistant",&forged).is_err(),"{pointer_path}");
    }
    let (d,identity)=fixture();
    for pointer_path in ["/company","/account","/runtimeOwner/runtimeId","/binding/nativeJobId"] {
        let mut forged=reference.clone();*forged.pointer_mut(pointer_path).unwrap()=json!("foreign");let mut actual=d.clone();
        assert!(attach(&mut actual,"job",&forged,&identity).is_err());assert_eq!(actual,d);
    }
}
#[test]
fn research_capture_survives_restart_and_cannot_be_relabelled_as_answering() {
    let dir=tempfile::tempdir().unwrap();let root=dir.path().join("research-cas");
    let store=ArtifactStore::open(&root).unwrap();let mut original=captured();
    original["binding"]["operation"]=json!("assistant_research");
    let artifact=store.put_bytes(original.to_string().as_bytes()).unwrap();let reference=pointer(&original,&artifact);
    let (mut d,identity)=fixture();let before=d.clone();
    attach(&mut d,"job",&reference,&identity).unwrap();validate_change(&before,&d).unwrap();
    drop(store);let reopened=ArtifactStore::open(&root).unwrap();
    let saved=&d["jobs"][0]["retainedEvidence"][0];
    assert_eq!(resolve_from(&reopened,"likeavto","LikeAvto",Some("job"),"assistant_research",saved).unwrap(),original);
    assert!(resolve_from(&reopened,"likeavto","LikeAvto",Some("job"),"assistant",saved).is_err());
    assert_eq!(d["operations"],before["operations"]);
    assert_eq!(saved["dispatchAuthorized"],false);assert_eq!(saved["retryAuthorized"],false);
}
#[test]
fn missing_corrupt_and_manifest_hash_failure_never_recreate_paid_result() {
    let dir=tempfile::tempdir().unwrap();let store=ArtifactStore::open(&dir.path().join("cas")).unwrap();let original=captured();
    let artifact=store.put_bytes(original.to_string().as_bytes()).unwrap();let reference=pointer(&original,&artifact);
    let path=store.path(&artifact).unwrap();std::fs::write(&path,b"corrupt").unwrap();
    assert!(resolve_from(&store,"likeavto","LikeAvto",Some("job"),"assistant",&reference).is_err());
    std::fs::remove_file(&path).unwrap();
    assert!(resolve_from(&store,"likeavto","LikeAvto",Some("job"),"assistant",&reference).is_err());
    let mut forged=original.clone();forged["request"]["prompt"]=json!("new prompt");
    let artifact=store.put_bytes(forged.to_string().as_bytes()).unwrap();let reference=pointer(&forged,&artifact);
    assert!(resolve_from(&store,"likeavto","LikeAvto",Some("job"),"assistant",&reference).is_err());
    for field in ["request","response"] {
        let mut absent=original.clone();absent.as_object_mut().unwrap().remove(field);
        absent[format!("{field}Sha256")]=json!(digest(&Value::Null));
        let artifact=store.put_bytes(absent.to_string().as_bytes()).unwrap();let reference=pointer(&absent,&artifact);
        assert!(resolve_from(&store,"likeavto","LikeAvto",Some("job"),"assistant",&reference).is_err(),"absent {field} is not a captured null");
    }
    let old=json!({"version":1,"kind":"retained-native-paid-stage-result","response":{"text":"legacy paid reply"}});
    let old_ref=store.put_bytes(old.to_string().as_bytes()).unwrap();
    assert_eq!(serde_json::from_slice::<Value>(&store.read_bytes(&old_ref,1024).unwrap()).unwrap(),old);
    assert!(resolve_from(&store,"likeavto","LikeAvto",Some("job"),"assistant",&pointer(&old,&old_ref)).is_err(),"legacy hash-only request is not forged into full capture");
}
#[test]
fn malformed_existing_evidence_and_duplicate_job_hold_without_losing_old_evidence() {
    let dir=tempfile::tempdir().unwrap();let store=ArtifactStore::open(&dir.path().join("cas")).unwrap();let original=captured();
    let artifact=store.put_bytes(original.to_string().as_bytes()).unwrap();let reference=pointer(&original,&artifact);
    let (mut d,identity)=fixture();d["jobs"][0]["retainedEvidence"]=json!({"legacy":"keep"});let before=d.clone();
    assert!(attach(&mut d,"job",&reference,&identity).is_err());assert_eq!(d,before);
    let (mut d,identity)=fixture();let duplicate=d["jobs"][0].clone();d["jobs"].as_array_mut().unwrap().push(duplicate);let before=d.clone();
    assert!(attach(&mut d,"job",&reference,&identity).is_err());assert_eq!(d,before);
}
#[test]
fn credential_capture_rejected_without_normalizing_audio_into_visual_authority() {
    for value in [json!({"access_token":"secret"}),json!({"result":[{"Authorization":"secret"}]}),json!({"api-key":"secret"})] {assert!(credential_field(&value));}
    assert!(!credential_field(&captured()));
    let dir=tempfile::tempdir().unwrap();let store=ArtifactStore::open(&dir.path().join("cas")).unwrap();let original=captured();
    let artifact=store.put_bytes(original.to_string().as_bytes()).unwrap();let reference=pointer(&original,&artifact);
    assert!(reference["binding"].get("visualEvidence").is_none());
    assert_eq!(resolve_from(&store,"likeavto","LikeAvto",Some("job"),"assistant",&reference).unwrap()["request"],original["request"]);
}

#[test]
fn exact_native_audio_equivalence_survives_cas_without_whitelisting_headers() {
    let edge=json!({"match":"owner_confirmed_audio_equivalence","authorization":"owner_confirmed_same_video",
        "equivalenceSha256":"a".repeat(64),"equivalenceRevision":1,"targetPostId":"target","postKey":"vk:target",
        "targetSourceVersion":"b".repeat(64),"sourcePostId":"source","sourcePostKey":"yt:source","sourceVersion":"c".repeat(64),
        "account":"LikeAvto","connectorBinding":{"accountId":"likeavto","providerId":"fixture"},
        "transcript":{"entryId":"entry","versionId":"version","hash":"d".repeat(64)},"identities":[],"byteEqualityClaimed":false});
    let request=json!({"materials":[{"audioEquivalence":[edge]}],"strictGroup":{"familyProof":[{"postId":"target","supports":[edge]}]},
        "knowledgeManifest":[{"mediaBinding":[edge]}],
        "postContextBundle":{"members":[{"speech":{"audioEquivalence":edge}}],"knowledgeManifest":[{"mediaBinding":[edge]}]}});
    assert!(!credential_field(&request));
    for foreign in [json!({"authorization":"owner_confirmed_same_video"}),json!({"headers":edge}),
        json!({"audioEquivalence":[{"authorization":"owner_confirmed_same_video"}]})] {assert!(credential_field(&foreign));}
    for field in ["authorization","match","byteEqualityClaimed","equivalenceRevision"] {
        let mut forged=edge.clone();forged[field]=json!("not-native-or-a-secret");
        assert!(credential_field(&json!({"audioEquivalence":[forged]})),"{field}");
    }
    for field in ["Authorization","access_token","password","api-key","cookie"] {
        let mut secret=edge.clone();secret["connectorBinding"][field]=json!("secret");
        assert!(credential_field(&json!({"audioEquivalence":[secret]})),"nested {field}");
        assert!(credential_field(&json!({"knowledgeManifest":[{"mediaBinding":[secret]}]})),"manifest nested {field}");
    }
    let mut original=captured();original["request"]=request.clone();original["requestSha256"]=json!(digest(&request));
    let dir=tempfile::tempdir().unwrap();let store=ArtifactStore::open(&dir.path().join("cas")).unwrap();
    let artifact=store.put_bytes(original.to_string().as_bytes()).unwrap();let reference=pointer(&original,&artifact);
    let resolved=resolve_from(&store,"likeavto","LikeAvto",Some("job"),"assistant",&reference).unwrap();
    assert_eq!(resolved,original,"the retained original is never renamed or normalized");
    assert_eq!(reference["dispatchAuthorized"],false);assert_eq!(reference["retryAuthorized"],false);
}
