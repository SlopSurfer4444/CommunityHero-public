//! Offline migration tool. Does not link the HTTP server, agent, or social bridge.
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::{Connection, PgConnection, Row, SqliteConnection, sqlite::SqliteConnectOptions};
use std::{collections::HashSet, io::Write, path::Path, time::Duration};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;
const TABLES: [&str; 13] = [
    "posts",
    "branches",
    "items",
    "conversations",
    "proposals",
    "approvals",
    "operations",
    "materials",
    "jobs",
    "audit",
    "knowledge_entries",
    "knowledge_versions",
    "feedback",
];
const SCHEMA: &str = include_str!("../../migrations/0001_pilot.sql");
const LOCK: i64 = 438772115;

fn text<'a>(v: &'a Value, key: &str) -> Result<&'a str> {
    v[key]
        .as_str()
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| format!("Missing or invalid {key}").into())
}
fn rows<'a>(v: &'a Value, table: &str) -> Result<&'a Vec<Value>> {
    if v.get(table).is_none()
        && ["knowledge_entries", "knowledge_versions", "feedback"].contains(&table)
    {
        static EMPTY: Vec<Value> = Vec::new();
        return Ok(&EMPTY);
    }
    v[table]
        .as_array()
        .ok_or_else(|| format!("Missing or invalid collection {table}").into())
}
fn hash(v: &Value) -> String {
    // serde_json's default map is key-sorted. Object formatting is irrelevant;
    // array order, text, draft contents, and unknown fields remain significant.
    format!("{:x}", Sha256::digest(v.to_string().as_bytes()))
}
fn check_ref(v: &Value, key: &str, target: &HashSet<&str>, required: bool) -> Result<()> {
    if v[key].is_null() && !required {
        return Ok(());
    }
    if !target.contains(text(v, key)?) {
        return Err(format!("Dangling {key}").into());
    }
    Ok(())
}
fn validate(v: &Value) -> Result<Value> {
    if !v.is_object() {
        return Err("Workspace must be an object".into());
    }
    text(v, "account")?;
    let mut counts = json!({});
    let mut ids = std::collections::HashMap::new();
    for table in TABLES {
        let records = rows(v, table)?;
        if records.len() > i32::MAX as usize {
            return Err("Collection too large".into());
        }
        let mut seen = HashSet::new();
        for r in records {
            if !r.is_object() || !seen.insert(text(r, "id")?) {
                return Err(format!("Invalid or duplicate ID in {table}").into());
            }
            for (_, key) in projection(table) {
                if !r[*key].is_null() && !r[*key].is_string() {
                    return Err(format!("Invalid {table}.{key}").into());
                }
            }
        }
        counts[table] = json!(records.len());
        ids.insert(table, seen);
    }
    for r in rows(v, "branches")? {
        check_ref(r, "postId", &ids["posts"], false)?;
    }
    for r in rows(v, "items")? {
        check_ref(r, "postId", &ids["posts"], false)?;
        check_ref(r, "branchId", &ids["branches"], false)?;
    }
    for r in rows(v, "proposals")? {
        check_ref(r, "itemId", &ids["items"], true)?;
    }
    for r in rows(v, "operations")? {
        check_ref(r, "itemId", &ids["items"], false)?;
        check_ref(r, "proposalId", &ids["proposals"], false)?;
        check_ref(r, "approvalId", &ids["approvals"], false)?;
    }
    for r in rows(v, "conversations")? {
        if let Some(targets) = r.get("itemIds") {
            for id in targets.as_array().ok_or("Invalid conversation itemIds")? {
                if !id.as_str().is_some_and(|s| ids["items"].contains(s)) {
                    return Err("Dangling conversation itemId".into());
                }
            }
        }
    }
    for r in rows(v, "approvals")? {
        for p in r["proposals"]
            .as_array()
            .ok_or("Invalid approval proposals")?
        {
            check_ref(p, "id", &ids["proposals"], true)?;
        }
    }
    for r in rows(v, "knowledge_entries")? {
        check_ref(r, "sourceMaterialId", &ids["materials"], false)?;
        check_ref(r, "currentVersionId", &ids["knowledge_versions"], true)?;
        let version = rows(v, "knowledge_versions")?
            .iter()
            .find(|x| x["id"] == r["currentVersionId"])
            .unwrap();
        if version["entryId"] != r["id"] {
            return Err("Current version belongs to another entry".into());
        }
    }
    for r in rows(v, "knowledge_versions")? {
        check_ref(r, "sourceMaterialId", &ids["materials"], false)?;
        check_ref(r, "entryId", &ids["knowledge_entries"], true)?;
    }
    for r in rows(v, "feedback")? {
        check_ref(r, "itemId", &ids["items"], true)?;
    }
    let operations = rows(v, "operations")?;
    Ok(
        json!({"schemaVersion":1,"sourceSha256":hash(v),"counts":counts,"executionEnabled":false,
        "unresolvedOperations":operations.iter().filter(|r| matches!(r["status"].as_str(),Some("unknown"|"dispatching"))).count(),
        "operationsWithoutRoute":operations.iter().filter(|r| !r["target"]["connectorBinding"].is_object()).count(),
        "approvalsHeld":rows(v,"approvals")?.iter().filter(|r| r["status"]=="approved").count()}),
    )
}

async fn read_source(path: &Path) -> Result<Value> {
    // A single SELECT reads one coherent SQLite snapshot, including committed WAL.
    // The source is never initialized, recovered, or written by this program.
    if path.extension().is_some_and(|s| s == "json") {
        return Ok(serde_json::from_slice(&std::fs::read(path)?)?);
    }
    let options = SqliteConnectOptions::new()
        .filename(path)
        .read_only(true)
        .create_if_missing(false)
        .busy_timeout(Duration::from_secs(5));
    let mut db = SqliteConnection::connect_with(&options).await?;
    let payload: String = sqlx::query_scalar("SELECT payload FROM workspace WHERE id=1")
        .fetch_one(&mut db)
        .await?;
    db.close().await?;
    Ok(serde_json::from_str(&payload)?)
}

fn projection(table: &str) -> &'static [(&'static str, &'static str)] {
    match table {
        "branches" => &[("post_id", "postId")],
        "items" => &[("post_id", "postId"), ("branch_id", "branchId")],
        "proposals" => &[("item_id", "itemId"), ("status", "status")],
        "approvals" => &[("status", "status")],
        "operations" => &[
            ("item_id", "itemId"),
            ("proposal_id", "proposalId"),
            ("approval_id", "approvalId"),
            ("status", "status"),
        ],
        "materials" => &[("kind", "kind")],
        "jobs" => &[("kind", "kind"), ("status", "status"), ("ref_id", "refId")],
        "audit" => &[("action", "action"), ("ref_id", "refId")],
        "knowledge_entries" => &[
            ("source_material_id", "sourceMaterialId"),
            ("current_version_id", "currentVersionId"),
        ],
        "knowledge_versions" => &[
            ("entry_id", "entryId"),
            ("source_material_id", "sourceMaterialId"),
        ],
        "feedback" => &[("item_id", "itemId")],
        _ => &[],
    }
}

async fn verify(db: &mut PgConnection, workspace: &str, source: &Value) -> Result<()> {
    let record = sqlx::query(
        "SELECT account,metadata::text,execution_enabled FROM communityhero.workspaces WHERE id=$1",
    )
    .bind(workspace)
    .fetch_one(&mut *db)
    .await?;
    if record.get::<bool, _>("execution_enabled")
        || record.get::<Option<String>, _>("account").as_deref() != source["account"].as_str()
    {
        return Err("Workspace identity/hold mismatch".into());
    }
    let mut restored: Value = serde_json::from_str(record.get::<&str, _>("metadata"))?;
    for table in TABLES {
        let query = format!(
            "SELECT id,ordinal,payload::text{} FROM communityhero.{table} WHERE workspace_id=$1 ORDER BY ordinal",
            projection(table)
                .iter()
                .map(|(col, _)| format!(",{col}"))
                .collect::<String>()
        );
        // Identifiers come exclusively from TABLES/projection, never source data.
        let records = sqlx::query(sqlx::AssertSqlSafe(query.as_str()))
            .bind(workspace)
            .fetch_all(&mut *db)
            .await?;
        let mut payloads = Vec::new();
        for (n, record) in records.iter().enumerate() {
            let payload: Value = serde_json::from_str(record.get::<&str, _>("payload"))?;
            if record.get::<i32, _>("ordinal") != n as i32
                || record.get::<&str, _>("id") != text(&payload, "id")?
            {
                return Err("ID/order projection mismatch".into());
            }
            for (col, key) in projection(table) {
                if record.get::<Option<String>, _>(*col).as_deref() != payload[*key].as_str() {
                    return Err("Relational projection mismatch".into());
                }
            }
            payloads.push(payload);
        }
        restored[table] = Value::Array(payloads);
    }
    let mut normalized = source.clone();
    for key in ["knowledge_entries", "knowledge_versions", "feedback"] {
        if normalized.get(key).is_none() {
            normalized[key] = json!([]);
        }
    }
    if restored != normalized {
        return Err("Round-trip verification failed; import rolled back".into());
    }
    Ok(())
}

async fn apply(db: &mut PgConnection, workspace: &str, source: &Value) -> Result<Value> {
    let mut report = validate(source)?;
    if workspace.trim().is_empty() {
        return Err("Workspace ID required".into());
    }
    let digest = hash(source);
    let mut tx = db.begin().await?;
    sqlx::query("SELECT pg_advisory_xact_lock($1)")
        .bind(LOCK)
        .execute(&mut *tx)
        .await?;
    let exists: bool =
        sqlx::query_scalar("SELECT to_regclass('communityhero.migration_imports') IS NOT NULL")
            .fetch_one(&mut *tx)
            .await?;
    if !exists {
        sqlx::raw_sql(SCHEMA).execute(&mut *tx).await?;
    }
    let held: bool = sqlx::query_scalar("SELECT pg_try_advisory_xact_lock(438772116)")
        .fetch_one(&mut *tx)
        .await?;
    if !held {
        return Err("Stop the pilot before importing".into());
    }
    sqlx::query("CREATE TABLE IF NOT EXISTS communityhero.schema_migrations(version integer PRIMARY KEY, applied_at timestamptz NOT NULL DEFAULT now())").execute(&mut *tx).await?;
    let upgraded: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM communityhero.schema_migrations WHERE version=2)",
    )
    .fetch_one(&mut *tx)
    .await?;
    if !upgraded {
        sqlx::raw_sql(include_str!("../../migrations/0002_knowledge.sql"))
            .execute(&mut *tx)
            .await?;
    }
    sqlx::raw_sql(include_str!("../../migrations/0003_history_guards.sql"))
        .execute(&mut *tx)
        .await?;
    sqlx::raw_sql(include_str!("../../migrations/0004_feedback_payload_id.sql"))
        .execute(&mut *tx)
        .await?;
    let existing = sqlx::query("SELECT i.source_sha256,i.payload::text,i.schema_version FROM communityhero.workspaces w JOIN communityhero.migration_imports i ON i.id=w.import_id WHERE w.id=$1").bind(workspace).fetch_optional(&mut *tx).await?;
    if let Some(existing) = existing {
        let previous: Value = serde_json::from_str(existing.get::<&str, _>("payload"))?;
        if existing.get::<i32, _>("schema_version") != 1
            || existing.get::<&str, _>("source_sha256") != digest
            || previous != *source
        {
            return Err(
                "Target already contains another snapshot; use a fresh migration database".into(),
            );
        }
        verify(&mut tx, workspace, source).await?;
        report["alreadyImported"] = json!(true);
    } else {
        sqlx::query("INSERT INTO communityhero.migration_imports(id,source_sha256,payload) VALUES($1,$2,$3::jsonb)").bind(&digest).bind(&digest).bind(source.to_string()).execute(&mut *tx).await?;
        let mut metadata = source.clone();
        for table in TABLES {
            metadata.as_object_mut().unwrap().remove(table);
        }
        sqlx::query("INSERT INTO communityhero.workspaces(id,account,import_id,metadata) VALUES($1,$2,$3,$4::jsonb)").bind(workspace).bind(text(source,"account")?).bind(&digest).bind(metadata.to_string()).execute(&mut *tx).await?;
        for table in TABLES {
            let columns = projection(table);
            let statement = format!(
                "INSERT INTO communityhero.{table}(workspace_id,id,ordinal,payload{}) VALUES($1,$2,$3,$4::jsonb{})",
                columns
                    .iter()
                    .map(|(col, _)| format!(",{col}"))
                    .collect::<String>(),
                (0..columns.len())
                    .map(|n| format!(",${}", n + 5))
                    .collect::<String>()
            );
            for (n, payload) in rows(source, table)?.iter().enumerate() {
                // Only compile-time table/column identifiers are interpolated.
                let mut query = sqlx::query(sqlx::AssertSqlSafe(statement.as_str()))
                    .bind(workspace)
                    .bind(text(payload, "id")?)
                    .bind(n as i32)
                    .bind(payload.to_string());
                for (_, key) in columns {
                    query = query.bind(payload[*key].as_str());
                }
                query.execute(&mut *tx).await?;
            }
        }
        verify(&mut tx, workspace, source).await?;
        report["alreadyImported"] = json!(false);
    }
    tx.commit().await?;
    report["verified"] = json!(true);
    Ok(report)
}

async fn run() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.len() < 2 {
        return Err("Usage: migrate-workspace <inspect|snapshot|apply> SOURCE [DESTINATION]; apply requires COMMUNITYHERO_MIGRATION_DATABASE_URL and uses local-pilot workspace".into());
    }
    let source = read_source(Path::new(&args[1])).await?;
    let report = validate(&source)?;
    match args[0].as_str() {
        "inspect" if args.len() == 2 => println!("{report}"),
        "snapshot" if args.len() == 3 => {
            // create_new makes it impossible to overwrite source or an existing backup.
            let mut file = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&args[2])?;
            file.write_all(source.to_string().as_bytes())?;
            file.sync_all()?;
            println!("{report}");
        }
        "apply" if args.len() == 2 => {
            let url = std::env::var("COMMUNITYHERO_MIGRATION_DATABASE_URL")
                .map_err(|_| "Migration database URL environment variable required")?;
            let mut db = PgConnection::connect(&url)
                .await
                .map_err(|_| "PostgreSQL connection failed (URL withheld)")?;
            let report = apply(&mut db, "local-pilot", &source).await?;
            db.close().await?;
            println!("{report}");
        }
        _ => return Err("Invalid command or argument count".into()),
    }
    Ok(())
}

#[tokio::main]
async fn main() {
    if let Err(error) = run().await {
        // Database detail may contain user replies or URLs; keep SQL errors private.
        if error.downcast_ref::<sqlx::Error>().is_some() {
            eprintln!("Migration failed at database validation; no partial import committed");
        } else {
            eprintln!("{error}");
        }
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> Value {
        let mut v = json!({"account":"LikeAvto","settings":{"unknown":"keep"}});
        for table in TABLES {
            v[table] = json!([]);
        }
        v["posts"] = json!([{"id":"post"}]);
        v["branches"] = json!([{"id":"branch","postId":"post"}]);
        v["items"] =
            json!([{"id":"item","postId":"post","branchId":"branch","draft":"Ручной ответ\n🙂"}]);
        v["proposals"] = json!([{"id":"p","itemId":"item","status":"approved"}]);
        v["approvals"] =
            json!([{"id":"a","status":"approved","proposals":[{"id":"p","revision":1}]}]);
        v["operations"] = json!([{"id":"o","itemId":"item","proposalId":"p","approvalId":"a","status":"unknown"}]);
        v
    }
    #[test]
    fn legacy_unknown_is_held_without_rewriting_original() {
        let mut v = fixture();
        v["operations"][0]["action"] =
            json!({"action":"reply_and_close","reply":"Exact reply","expectedStatuses":["new"]});
        let copy = v.clone();
        let report = validate(&v).unwrap();
        assert_eq!(report["unresolvedOperations"], 1);
        assert_eq!(report["operationsWithoutRoute"], 1);
        assert_eq!(report["executionEnabled"], false);
        assert_eq!(v, copy);
    }
    #[test]
    fn legacy_snapshot_without_knowledge_arrays_is_valid_without_rewriting_archive() {
        let mut v=fixture();
        for key in ["knowledge_entries","knowledge_versions","feedback"] {v.as_object_mut().unwrap().remove(key);}
        let original=v.clone();
        assert!(validate(&v).is_ok());
        assert_eq!(v,original);
        v["feedback"]=json!(null);
        assert!(validate(&v).is_err());
    }
    #[test]
    fn reject_duplicate_and_dangling_relationships() {
        let mut v = fixture();
        let duplicate = v["items"][0].clone();
        v["items"].as_array_mut().unwrap().push(duplicate);
        assert!(validate(&v).is_err());
        let mut v = fixture();
        v["proposals"][0]["itemId"] = json!("gone");
        assert!(validate(&v).is_err());
        let mut v = fixture();
        v["approvals"][0]["proposals"][0]["id"] = json!("gone");
        assert!(validate(&v).is_err());
    }
    #[tokio::test]
    async fn readonly_source_includes_committed_wal_and_never_initializes_missing_db() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("source.sqlite");
        let options = SqliteConnectOptions::new()
            .filename(&path)
            .create_if_missing(true)
            .journal_mode(sqlx::sqlite::SqliteJournalMode::Wal);
        let mut writer = SqliteConnection::connect_with(&options).await.unwrap();
        sqlx::raw_sql("CREATE TABLE workspace(id INTEGER PRIMARY KEY,payload TEXT NOT NULL)")
            .execute(&mut writer)
            .await
            .unwrap();
        let v = fixture();
        sqlx::query("INSERT INTO workspace VALUES(1,?)")
            .bind(v.to_string())
            .execute(&mut writer)
            .await
            .unwrap();
        assert_eq!(read_source(&path).await.unwrap(), v);
        let missing = dir.path().join("missing.sqlite");
        assert!(read_source(&missing).await.is_err());
        assert!(!missing.exists());
    }
    #[tokio::test]
    #[ignore = "requires isolated PostgreSQL via COMMUNITYHERO_MIGRATION_DATABASE_URL"]
    async fn postgres_roundtrip_idempotency_hold_and_rollback() {
        let url = std::env::var("COMMUNITYHERO_MIGRATION_DATABASE_URL").unwrap();
        let mut db = PgConnection::connect(&url).await.unwrap();
        let mut v = fixture();
        v["materials"] = json!([{"id":"material"}]);
        v["knowledge_entries"] =
            json!([{"id":"entry","currentVersionId":"version","sourceMaterialId":"material"}]);
        v["knowledge_versions"] = json!([{"id":"version","entryId":"entry","sourceMaterialId":"material","text":"unchanged fact"}]);
        v["feedback"] = json!([{"id":"feedback","itemId":"item","status":"pending_review"}]);
        let r = apply(&mut db, "migration-test", &v).await.unwrap();
        assert_eq!(r["verified"], true);
        assert_eq!(
            apply(&mut db, "migration-test", &v).await.unwrap()["alreadyImported"],
            true
        );
        assert!(sqlx::query("UPDATE communityhero.workspaces SET execution_enabled=true WHERE id='migration-test'").execute(&mut db).await.is_err());
        let mut changed = v.clone();
        changed["items"][0]["draft"] = json!("different");
        assert!(apply(&mut db, "migration-test", &changed).await.is_err());
        verify(&mut db, "migration-test", &v).await.unwrap();
        assert!(sqlx::query("UPDATE communityhero.knowledge_versions SET payload='{}' WHERE workspace_id='migration-test'").execute(&mut db).await.is_err());
        assert!(
            sqlx::query(
                "DELETE FROM communityhero.knowledge_versions WHERE workspace_id='migration-test'"
            )
            .execute(&mut db)
            .await
            .is_err()
        );
        assert!(
            sqlx::query(
                "UPDATE communityhero.feedback SET payload='{}' WHERE workspace_id='migration-test'"
            )
            .execute(&mut db)
            .await
            .is_err()
        );
        verify(&mut db, "migration-test", &v).await.unwrap();
        // Reject the last collection after earlier inserts succeeded, proving
        // that the snapshot, workspace and all preceding entities roll back.
        sqlx::query("ALTER TABLE communityhero.audit ADD CONSTRAINT test_late_failure CHECK (workspace_id <> 'rollback-test')").execute(&mut db).await.unwrap();
        let mut invalid = v.clone();
        invalid["audit"] = json!([{"id":"late-record","action":"test"}]);
        assert!(apply(&mut db, "rollback-test", &invalid).await.is_err());
        let n: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM communityhero.workspaces WHERE id='rollback-test'",
        )
        .fetch_one(&mut db)
        .await
        .unwrap();
        assert_eq!(n, 0);
        let n: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM communityhero.migration_imports WHERE source_sha256=$1",
        )
        .bind(hash(&invalid))
        .fetch_one(&mut db)
        .await
        .unwrap();
        assert_eq!(n, 0);
        sqlx::query("ALTER TABLE communityhero.audit DROP CONSTRAINT test_late_failure")
            .execute(&mut db)
            .await
            .unwrap();
    }
}
