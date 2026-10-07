use crate::{accounts::Profile, storage::Database, *};
use std::{
    collections::HashSet,
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::sync::Barrier;

const WORKERS_PER_COMPANY: usize = 6;
const OPERATIONS_PER_WORKER: usize = 16;

#[tokio::test]
async fn sqlite_company_identity_guard_preserves_bootstrap_and_rejects_foreign_writes() {
    let temp = tempfile::tempdir().unwrap();
    let db = Database::Sqlite(
        crate::open_db(&temp.path().join("workspace.sqlite"))
            .await
            .unwrap(),
    );
    db.change(|data| crate::accounts::initialize(data, Profile::BawRussia))
        .await
        .unwrap();
    let original = db.read().await.unwrap();
    assert_eq!(original["account"], Profile::BawRussia.display());
    assert_eq!(original["connectorBinding"], Profile::BawRussia.binding());

    assert!(
        db.change(|data| {
            data["account"] = json!(Profile::LikeAvto.display());
            Ok(())
        })
        .await
        .is_err()
    );
    assert_eq!(db.read().await.unwrap(), original);
    assert!(
        db.change(|data| {
            data["connectorBinding"] = Profile::LikeAvto.binding();
            Ok(())
        })
        .await
        .is_err()
    );
    assert_eq!(db.read().await.unwrap(), original);

    // Same-company revision changes remain representable for future connector
    // replacement; the write path checks scope, not a permanently frozen route.
    db.change(|data| {
        data["connectorBinding"]["revision"] = json!(2);
        Ok(())
    })
    .await
    .unwrap();
    let revised = db.read().await.unwrap();
    assert_eq!(revised["account"], original["account"]);
    assert_eq!(revised["connectorBinding"]["revision"], 2);
    assert_eq!(
        revised["connectorBinding"]["accountId"],
        Profile::BawRussia.display()
    );
    db.close().await;
}

fn percentile(samples: &[Duration], percentage: usize) -> f64 {
    let mut sorted: Vec<_> = samples.iter().map(Duration::as_secs_f64).collect();
    sorted.sort_by(f64::total_cmp);
    let index = ((sorted.len() - 1) * percentage).div_ceil(100);
    sorted[index] * 1000.0
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn separate_company_sqlite_stores_retain_concurrent_work_and_ledgers() {
    let temp = tempfile::tempdir().unwrap();
    let mut stores = Vec::new();
    for profile in [Profile::LikeAvto, Profile::BawRussia] {
        let directory = temp.path().join(profile.key());
        std::fs::create_dir(&directory).unwrap();
        let path = directory.join("workspace.sqlite");
        let db = Database::Sqlite(crate::open_db(&path).await.unwrap());
        db.change(|data| crate::accounts::initialize(data, profile))
            .await
            .unwrap();
        stores.push((profile, path, db));
    }

    let barrier = Arc::new(Barrier::new(WORKERS_PER_COMPANY * stores.len()));
    let mut tasks = Vec::new();
    let wall_started = Instant::now();
    for (profile, _, db) in &stores {
        for worker in 0..WORKERS_PER_COMPANY {
            let profile = *profile;
            let db = db.clone();
            let barrier = barrier.clone();
            tasks.push(tokio::spawn(async move {
                let mut writes = Vec::with_capacity(OPERATIONS_PER_WORKER);
                let mut reads = Vec::with_capacity(OPERATIONS_PER_WORKER);
                barrier.wait().await;
                for sequence in 0..OPERATIONS_PER_WORKER {
                    // The same logical/external ids occur in both databases on purpose.
                    let operation_id = format!("operation-{worker}-{sequence}");
                    let marker = format!("{}-{worker}-{sequence}", profile.key());
                    let started = Instant::now();
                    db.change(|data| {
                        assert_eq!(data["account"], profile.display());
                        assert_eq!(data["connectorBinding"], profile.binding());
                        data["operations"].as_array_mut().unwrap().push(json!({
                            "id": operation_id, "status": "unknown",
                            "externalId": format!("external-{worker}-{sequence}"),
                            "target": {"connectorBinding": profile.binding()},
                            "localMarker": marker
                        }));
                        data["audit"].as_array_mut().unwrap().push(json!({
                            "id": format!("audit-{worker}-{sequence}"),
                            "action": "synthetic-load", "refId": operation_id,
                            "accountMarker": marker
                        }));
                        Ok(())
                    })
                    .await
                    .unwrap();
                    writes.push(started.elapsed());

                    let started = Instant::now();
                    let snapshot = db.read().await.unwrap();
                    assert_eq!(snapshot["account"], profile.display());
                    assert!(
                        snapshot["operations"]
                            .as_array()
                            .unwrap()
                            .iter()
                            .any(|row| {
                                row["id"] == operation_id && row["localMarker"] == marker
                            })
                    );
                    reads.push(started.elapsed());
                }
                (writes, reads)
            }));
        }
    }

    let bounded = tokio::time::timeout(Duration::from_secs(60), async {
        let mut writes = Vec::new();
        let mut reads = Vec::new();
        for task in &mut tasks {
            let (mut task_writes, mut task_reads) = task.await.unwrap();
            writes.append(&mut task_writes);
            reads.append(&mut task_reads);
        }
        (writes, reads)
    })
    .await;
    let samples = match bounded {
        Ok(samples) => samples,
        Err(_) => {
            for task in tasks {
                task.abort();
            }
            panic!("bounded local SQLite probe exceeded 60 seconds");
        }
    };
    let wall = wall_started.elapsed();
    let expected = WORKERS_PER_COMPANY * OPERATIONS_PER_WORKER;
    assert_eq!(samples.0.len(), expected * stores.len());
    assert_eq!(samples.1.len(), expected * stores.len());

    for (profile, path, db) in stores {
        let before = db.read().await.unwrap();
        assert_eq!(before["account"], profile.display());
        assert_eq!(before["connectorBinding"], profile.binding());
        assert_eq!(before["operations"].as_array().unwrap().len(), expected);
        assert_eq!(before["audit"].as_array().unwrap().len(), expected);
        let operation_ids: HashSet<_> = before["operations"]
            .as_array()
            .unwrap()
            .iter()
            .map(|row| row["id"].as_str().unwrap())
            .collect();
        let audit_ids: HashSet<_> = before["audit"]
            .as_array()
            .unwrap()
            .iter()
            .map(|row| row["id"].as_str().unwrap())
            .collect();
        assert_eq!(operation_ids.len(), expected);
        assert_eq!(audit_ids.len(), expected);
        for row in before["operations"].as_array().unwrap() {
            assert_eq!(row["status"], "unknown");
            assert_eq!(row["target"]["connectorBinding"], profile.binding());
            assert!(
                row["localMarker"]
                    .as_str()
                    .unwrap()
                    .starts_with(profile.key())
            );
        }
        for row in before["audit"].as_array().unwrap() {
            assert!(operation_ids.contains(row["refId"].as_str().unwrap()));
            assert!(
                row["accountMarker"]
                    .as_str()
                    .unwrap()
                    .starts_with(profile.key())
            );
        }
        let other = if profile == Profile::LikeAvto {
            Profile::BawRussia
        } else {
            Profile::LikeAvto
        };
        assert!(
            db.change(|data| crate::accounts::initialize(data, other))
                .await
                .is_err()
        );
        assert_eq!(db.read().await.unwrap(), before);
        db.close().await;
        let reopened = Database::Sqlite(crate::open_db(&path).await.unwrap());
        assert_eq!(reopened.read().await.unwrap(), before);
        reopened.close().await;
    }

    eprintln!(
        "company-load sqlite stores=2 workers/store={} operations/store={} writes={} reads={} wall_ms={:.1} write_ms[p50={:.1},p95={:.1},p99={:.1},max={:.1}] read_ms[p50={:.1},p95={:.1},p99={:.1},max={:.1}]",
        WORKERS_PER_COMPANY,
        expected,
        samples.0.len(),
        samples.1.len(),
        wall.as_secs_f64() * 1000.0,
        percentile(&samples.0, 50),
        percentile(&samples.0, 95),
        percentile(&samples.0, 99),
        percentile(&samples.0, 100),
        percentile(&samples.1, 50),
        percentile(&samples.1, 95),
        percentile(&samples.1, 99),
        percentile(&samples.1, 100)
    );
}
