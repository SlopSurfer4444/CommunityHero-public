//! Ignored, fresh-process acceptance for a separately restored SMART artifact tree.
//! Reads an exact root-run canary receipt; writes only disposable SQLite. No bridge,
//! decoder, model, provider, production database or artifact mutation is invoked.
use crate::*;
use sha2::{Digest, Sha256};

fn required_env(key: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| panic!("{key} required"))
}

fn assert_projection(value: &Value, draft: &Value, ready: bool) {
    let item = row(value, "items", "restored-smart-item").unwrap();
    assert_eq!(&item["draft"], draft);
    assert_eq!(item["workflow"], "prepared");
    assert_eq!(item["mediaReadiness"]["schemaVersion"], 2);
    assert_eq!(item["mediaReadiness"]["required"], true);
    assert_eq!(item["mediaReadiness"]["status"] == "ready", ready);
    assert!(value.get("mediaReadinessCatalog").is_none());
}

#[tokio::test]
#[ignore = "requires exact canary receipt and separate restored CAS; run alone in a fresh process"]
async fn local_restored_smart_evidence_acceptance() {
    let receipt_path = PathBuf::from(required_env("COMMUNITYHERO_MEDIA_RESTORE_RESULT"));
    let evidence_dir = PathBuf::from(required_env("COMMUNITYHERO_MEDIA_EVIDENCE_DIR"));
    assert!(receipt_path.is_absolute() && receipt_path.is_file());
    assert!(evidence_dir.is_absolute() && evidence_dir.join("objects").is_dir());
    assert!(std::fs::metadata(&receipt_path).unwrap().len() <= 16 * 1024 * 1024);
    let bytes = std::fs::read(&receipt_path).unwrap();
    assert_eq!(format!("{:x}", Sha256::digest(&bytes)),
        required_env("COMMUNITYHERO_MEDIA_RESTORE_RESULT_SHA256"));
    let receipt: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(receipt["status"], "passed");
    assert_eq!(receipt["sqliteReopenedAfterFirstChunk"], true);
    assert_eq!(receipt["firstReceiptRetained"], true);
    assert_eq!(receipt["modelFrames"], 2);
    assert_eq!(receipt["chunks"], 3);
    let materials = receipt["result"]["materials"].as_array().unwrap();
    assert_eq!(materials.iter().filter(|m| m["kind"] == "visual_context").count(), 1);
    assert!(materials.iter().any(|m| m["kind"] == "transcript"));
    let evidence = &materials.iter().find(|m| m["kind"] == "visual_context").unwrap()["visualEvidence"];
    assert_eq!(evidence["schemaVersion"], 2);
    assert!(crate::media_fullframes::validate_evidence(evidence).is_err(),
        "Run this exact ignored test alone in a fresh process: proof must start cold");

    // The hash cannot reconstruct a post. Rebuild the exact canary fixture from
    // the original explicit inputs, then prove its fingerprint before using it.
    let account = required_env("COMMUNITYHERO_MEDIA_SAMPLE_ACCOUNT");
    let post_key = required_env("COMMUNITYHERO_MEDIA_SAMPLE_POST_KEY");
    let post = json!({"id":"transient-local","postKey":post_key,
        "title":required_env("COMMUNITYHERO_MEDIA_SAMPLE_TITLE"),
        "sourceUrl":required_env("COMMUNITYHERO_MEDIA_SAMPLE_SOURCE_URL"),
        "attachments":[{"type":"video"}]});
    assert_eq!(account, "LikeAvto", "This acceptance uses the original LikeAvto canary fixture");
    assert_eq!(evidence["source"]["account"], account);
    assert_eq!(evidence["source"]["postKey"], post_key);
    assert_eq!(evidence["sourcePostVersion"], crate::media_fullframes::source_version(&post, &account));
    let object_id = post_key.split_once(':').expect("Scoped fixture post key required").0;
    let draft = json!({"text":"Preserve exact operator draft — восстановленный контекст", "editedByHuman":true});
    let (mut app, folder) = crate::tests::test_app().await;
    app.external_writes = false;
    // Recreate an already admitted DB snapshot, not a fresh import of untrusted
    // material while proof is cold. Real restores retain immutable active heads.
    crate::media_fullframes::verify_and_cache(evidence).unwrap();
    app.change(|d| {
        crate::accounts::initialize(d, crate::accounts::Profile::LikeAvto)?;
        d["posts"] = json!([post]);
        d["items"] = json!([{"id":"restored-smart-item","postId":"transient-local",
            "postKey":post_key,"objectId":object_id,"itemId":"restored-smart-item",
            "conversationKey":format!("{object_id}:restored-smart-thread"),
            "providerStatus":"new","workflow":"prepared","draft":draft}]);
        d["materials"] = json!(materials);
        d["knowledge_entries"] = json!([]);
        d["knowledge_versions"] = json!([]);
        crate::knowledge::sync_catalog(d, &crate::now()).map_err(crate::bad)
    }).await.unwrap();
    app.db.close().await;
    app.db = crate::Database::Sqlite(crate::open_db(&folder.path().join("workspace.sqlite")).await.unwrap());
    app.bootstrap_cache = Arc::new(crate::bootstrap_cache::Cache::default());
    crate::media_fullframes::forget_test_proof(evidence);
    assert!(crate::media_fullframes::validate_evidence(evidence).is_err());
    let persisted_before = app.read().await.unwrap();
    let cold = app.read_bootstrap().await.unwrap();
    assert_projection(&cold, &draft, false);

    // Verification walks the restored immutable closure outside any DB writer.
    crate::media_fullframes::verify_and_cache(evidence).unwrap();
    assert!(crate::media_fullframes::validate_evidence(evidence).is_ok());
    let ready = app.read_bootstrap().await.unwrap();
    assert_projection(&ready, &draft, true);
    assert_ne!(cold["workspaceVersion"], ready["workspaceVersion"]);
    assert_eq!(app.read().await.unwrap(), persisted_before, "Warming proof must not rewrite workspace data");

    app.db.close().await;
    app.db = crate::Database::Sqlite(crate::open_db(&folder.path().join("workspace.sqlite")).await.unwrap());
    app.bootstrap_cache = Arc::new(crate::bootstrap_cache::Cache::default());
    assert_projection(&app.read_bootstrap().await.unwrap(), &draft, true);
    assert_eq!(app.read().await.unwrap(), persisted_before);
    app.db.close().await;
}
