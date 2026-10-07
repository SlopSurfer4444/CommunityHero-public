//! ROOT-only synthetic E2E seed/digest emitter. Does not admit a runtime.
use serde_json::{json, Value};
#[cfg(test)]
#[path = "wave_paid_floor_producer_tests.rs"]
mod paid_floor_producer;
#[cfg(test)]
#[path = "floor_characterization_tests.rs"]
mod floor_characterization;

fn bootstrap_seed(profile: crate::accounts::Profile) -> Value {
    let mut workspace = crate::empty();
    crate::accounts::initialize(&mut workspace, profile).unwrap();
    // The native writer normalizes these absent collections before bootstrap.
    // Bind the emitted digest to that same complete workspace shape.
    for table in ["knowledge_entries", "knowledge_versions", "feedback"] {
        if workspace.get(table).is_none() { workspace[table] = json!([]); }
    }
    assert!(workspace.get("runtimeLifecycle").is_none());
    workspace
}

#[test]
#[ignore = "ROOT synthetic production-bootstrap fixture preparation only"]
fn emit_production_bootstrap_fixture() {
    if let Ok(file) = std::env::var("COMMUNITYHERO_WAVE_SYNTHETIC_LEDGER_FILE") {
        let path = std::path::Path::new(&file);
        assert!(path.is_absolute());
        assert_eq!(path.file_name().and_then(|s| s.to_str()), Some("synthetic-ledger.json"));
        let bytes = std::fs::read(path).unwrap();
        assert!(bytes.len() < 128 * 1024 * 1024);
        let value: Value = serde_json::from_slice(&bytes).unwrap();
        println!("\nWAVE_E2E_LEDGER_DIGEST={}", crate::runtime_lifecycle::ledger_digest(&value).unwrap());
        return;
    }
    let mut seeds = Vec::new();
    for profile in [crate::accounts::Profile::LikeAvto, crate::accounts::Profile::BawRussia] {
        let workspace = bootstrap_seed(profile);
        let digest = crate::runtime_lifecycle::ledger_digest(&workspace).unwrap();
        seeds.push(json!({"accountKey":profile.key(),"account":profile.display(),"workspace":workspace,"ledgerSha256":digest}));
    }
    println!("\nWAVE_E2E_BOOTSTRAP_SEEDS={}", serde_json::to_string(&seeds).unwrap());
}

#[tokio::test]
async fn emitted_bootstrap_seed_matches_actual_database_normalization_for_both_companies() {
    for profile in [crate::accounts::Profile::LikeAvto, crate::accounts::Profile::BawRussia] {
        let seed = bootstrap_seed(profile);
        let digest = crate::runtime_lifecycle::ledger_digest(&seed).unwrap();
        let pool = sqlx::sqlite::SqlitePoolOptions::new().max_connections(1)
            .connect("sqlite::memory:").await.unwrap();
        sqlx::query("CREATE TABLE workspace(id INTEGER PRIMARY KEY,payload TEXT NOT NULL)")
            .execute(&pool).await.unwrap();
        // Reproduce the old incomplete emitter through the real writer: its
        // pre-normalization digest must be refused without installing an owner.
        let mut legacy = seed.clone();
        legacy.as_object_mut().unwrap().remove("feedback");
        let legacy_digest = crate::runtime_lifecycle::ledger_digest(&legacy).unwrap();
        sqlx::query("INSERT INTO workspace(id,payload) VALUES(1,?)")
            .bind(legacy.to_string()).execute(&pool).await.unwrap();
        let db = crate::Database::Sqlite(pool.clone());
        let owner = crate::runtime_lifecycle::OwnerToken {
            account: profile.display().into(), runtime_id: format!("seed-parity-{}", profile.key()),
            release_sha256: "a".repeat(64), epoch: 1,
        };
        let receipt = "b".repeat(64);
        assert_ne!(legacy_digest, digest);
        assert!(db.change_runtime_lifecycle_with_ledger(|d| {
            crate::runtime_lifecycle::initialize(d, owner.clone(), &receipt, &legacy_digest)
        }).await.is_err());
        let raw: String = sqlx::query_scalar("SELECT payload FROM workspace WHERE id=1")
            .fetch_one(&pool).await.unwrap();
        assert_eq!(serde_json::from_str::<Value>(&raw).unwrap(), legacy);
        assert_eq!(db.read().await.unwrap(), seed);

        sqlx::query("UPDATE workspace SET payload=? WHERE id=1")
            .bind(seed.to_string()).execute(&pool).await.unwrap();
        db.change_runtime_lifecycle_with_ledger(|d| {
            assert_eq!(*d, seed);
            assert_eq!(crate::runtime_lifecycle::ledger_digest(d)?, digest);
            crate::runtime_lifecycle::initialize(d, owner.clone(), &receipt, &digest)
        }).await.unwrap();
        let mut settled = db.read().await.unwrap();
        assert_eq!(settled["runtimeLifecycle"]["owner"]["account"], profile.display());
        assert_eq!(settled["runtimeLifecycle"]["phase"], "running");
        assert_eq!(crate::runtime_lifecycle::ledger_digest(&settled).unwrap(), digest);
        settled.as_object_mut().unwrap().remove("runtimeLifecycle");
        assert_eq!(settled, seed);
        db.close().await;
    }
}
