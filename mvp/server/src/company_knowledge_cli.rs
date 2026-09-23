//! One-shot company knowledge import. This path never starts the HTTP server.
use crate::{ApiResult, Database, bad, conflict, internal, knowledge, now};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use std::{ffi::OsString, fs, io::Read, path::{Path, PathBuf}, time::Duration};

const MAX_PACKAGE_FILE: u64 = 32 * 1024 * 1024;
const MAX_URL_FILE: u64 = 8192;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode { Check, Apply }

enum Source { Sqlite(PathBuf), PostgresUrlFile(PathBuf) }

struct Options {
    mode: Mode,
    package: PathBuf,
    company: String,
    manifest_sha256: String,
    records_sha256: String,
    expected_workspace_sha256: Option<String>,
    source: Source,
}

fn digest(bytes: &[u8]) -> String { format!("{:x}", Sha256::digest(bytes)) }
fn workspace_digest(value: &Value) -> String { digest(value.to_string().as_bytes()) }

fn parse_hash(value: OsString) -> Result<String, &'static str> {
    let value = value.into_string().map_err(|_| "Hash must be UTF-8 hexadecimal")?;
    if value.len() != 64 || !value.bytes().all(|c| c.is_ascii_hexdigit()) {
        return Err("Hash must be 64 hexadecimal characters");
    }
    Ok(value.to_ascii_lowercase())
}

fn absolute_path(value: OsString) -> Result<PathBuf, &'static str> {
    let path = PathBuf::from(value);
    if !path.is_absolute() { return Err("Path must be absolute"); }
    Ok(path)
}

fn set_once<T>(slot: &mut Option<T>, value: T) -> Result<(), &'static str> {
    if slot.replace(value).is_some() { return Err("Duplicate option"); }
    Ok(())
}

fn parse(args: impl IntoIterator<Item = OsString>) -> Result<Options, &'static str> {
    let mut args = args.into_iter();
    let mut mode = None;
    let mut package = None;
    let mut company = None;
    let mut manifest_sha256 = None;
    let mut records_sha256 = None;
    let mut expected_workspace_sha256 = None;
    let mut source = None;
    while let Some(flag) = args.next() {
        let flag = flag.to_str().ok_or("Unknown option")?;
        match flag {
            "--check" => set_once(&mut mode, Mode::Check)?,
            "--apply" => set_once(&mut mode, Mode::Apply)?,
            "--package" => set_once(&mut package, absolute_path(args.next().ok_or("Missing option value")?)?)?,
            "--company" => {
                let value = args.next().ok_or("Missing option value")?.into_string().map_err(|_| "Invalid company")?;
                if value != "likeavto" && value != "baw-russia" { return Err("Invalid company"); }
                set_once(&mut company, value)?;
            }
            "--manifest-sha256" => set_once(&mut manifest_sha256, parse_hash(args.next().ok_or("Missing option value")?)?)?,
            "--records-sha256" => set_once(&mut records_sha256, parse_hash(args.next().ok_or("Missing option value")?)?)?,
            "--expected-workspace-sha256" => set_once(&mut expected_workspace_sha256, parse_hash(args.next().ok_or("Missing option value")?)?)?,
            "--sqlite" => set_once(&mut source, Source::Sqlite(absolute_path(args.next().ok_or("Missing option value")?)?))?,
            "--postgres-url-file" => set_once(&mut source, Source::PostgresUrlFile(absolute_path(args.next().ok_or("Missing option value")?)?))?,
            _ => return Err("Unknown option"),
        }
    }
    let mode = mode.ok_or("Specify --check or --apply")?;
    if mode == Mode::Apply && expected_workspace_sha256.is_none() {
        return Err("--apply requires --expected-workspace-sha256 from --check");
    }
    if mode == Mode::Check && expected_workspace_sha256.is_some() {
        return Err("--expected-workspace-sha256 is only valid with --apply");
    }
    Ok(Options {
        mode,
        package: package.ok_or("Missing --package")?,
        company: company.ok_or("Missing --company")?,
        manifest_sha256: manifest_sha256.ok_or("Missing --manifest-sha256")?,
        records_sha256: records_sha256.ok_or("Missing --records-sha256")?,
        expected_workspace_sha256,
        source: source.ok_or("Specify --sqlite or --postgres-url-file")?,
    })
}

fn read_limited(path: &Path, max: u64) -> Result<Vec<u8>, &'static str> {
    let file = fs::File::open(path).map_err(|_| "Input file unavailable")?;
    let metadata = file.metadata().map_err(|_| "Input file unavailable")?;
    if !metadata.is_file() || metadata.len() >= max { return Err("Input file missing or too large"); }
    let mut bytes = Vec::new();
    file.take(max).read_to_end(&mut bytes).map_err(|_| "Input file unreadable")?;
    if bytes.len() as u64 >= max { return Err("Input file too large"); }
    Ok(bytes)
}

async fn open_existing_sqlite(path: &Path, check: bool) -> ApiResult<Database> {
    if !path.is_file() { return Err(bad("SQLite database must already exist")); }
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(
            SqliteConnectOptions::new()
                .filename(path)
                .create_if_missing(false)
                .read_only(check)
                .busy_timeout(Duration::from_secs(5)),
        )
        .await?;
    let db = Database::Sqlite(pool);
    // A missing workspace is an error; the importer cannot seed or migrate it.
    if let Err(error) = db.read().await {
        db.close().await;
        return Err(error);
    }
    Ok(db)
}

async fn open_source(source: &Source, check: bool) -> ApiResult<(Database, Option<fs::File>)> {
    match source {
        Source::Sqlite(path) => {
            let parent = path.parent().ok_or_else(|| bad("SQLite path has no parent"))?;
            let lock = fs::OpenOptions::new().create(true).write(true).open(parent.join("server.lock"))
                .map_err(|_| internal("SQLite server lease unavailable"))?;
            lock.try_lock().map_err(|_| conflict("Another CommunityHero server owns this database"))?;
            let db = open_existing_sqlite(path, check).await?;
            Ok((db, Some(lock)))
        }
        Source::PostgresUrlFile(path) => {
            let bytes = read_limited(path, MAX_URL_FILE).map_err(bad)?;
            let text = std::str::from_utf8(&bytes).map_err(|_| bad("PostgreSQL URL file invalid"))?;
            let url = text.trim_end_matches(['\r', '\n']);
            if url.is_empty() || url.chars().any(char::is_whitespace) {
                return Err(bad("PostgreSQL URL file invalid"));
            }
            // Database::postgres obtains the same advisory lease as the server.
            Ok((Database::postgres(url).await?, None))
        }
    }
}

async fn execute(options: Options) -> ApiResult<Value> {
    if !options.package.is_dir() { return Err(bad("Package directory unavailable")); }
    let manifest = read_limited(&options.package.join("manifest.json"), MAX_PACKAGE_FILE).map_err(bad)?;
    let records = read_limited(&options.package.join("records.jsonl"), MAX_PACKAGE_FILE).map_err(bad)?;
    let coverage = read_limited(&options.package.join("media-coverage.json"), MAX_PACKAGE_FILE).map_err(bad)?;
    let package = knowledge::company_import::Package::from_bytes(
        &manifest, &records, &options.manifest_sha256, &options.records_sha256,
    ).and_then(|package| package.with_coverage(&coverage)).map_err(bad)?;
    let (db, _lease) = open_source(&options.source, options.mode == Mode::Check).await?;
    let result: ApiResult<Value> = async { match options.mode {
        Mode::Check => {
            let mut workspace = db.read().await?;
            let before = workspace_digest(&workspace);
            let receipt = knowledge::company_import::apply(&mut workspace, &package, &options.company, &now())
                .map_err(conflict)?;
            Ok(json!({
                "mode": "check",
                "workspaceSha256": before,
                "result": receipt,
            }))
        }
        Mode::Apply => {
            let expected = options.expected_workspace_sha256.as_deref().expect("checked by parser");
            let (receipt, changed) = db.change_observed(|workspace| {
                let before = workspace_digest(workspace);
                if before != expected { return Err(conflict("Workspace changed since --check")); }
                let result = knowledge::company_import::apply(workspace, &package, &options.company, &now())
                    .map_err(conflict)?;
                Ok(json!({
                    "workspaceSha256": before,
                    "workspaceSha256After": workspace_digest(workspace),
                    "result": result,
                }))
            }).await?;
            Ok(json!({"mode": "apply", "changed": changed, "receipt": receipt}))
        }
    }}.await;
    db.close().await;
    result
}

pub(super) async fn run(args: impl IntoIterator<Item = OsString>) -> Result<(), Box<dyn std::error::Error>> {
    let options = parse(args)?;
    let receipt = execute(options).await.map_err(|error| error.1)?;
    println!("{receipt}");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{Json, extract::State};

    #[test]
    fn parser_requires_all_bounded_inputs_and_apply_precondition() {
        let args = ["--check", "--package", "C:/package", "--company", "likeavto",
            "--manifest-sha256", &"a".repeat(64), "--records-sha256", &"b".repeat(64),
            "--sqlite", "C:/workspace.sqlite"];
        assert!(parse(args.map(OsString::from)).is_ok());
        let args = ["--apply", "--package", "C:/package", "--company", "likeavto",
            "--manifest-sha256", &"a".repeat(64), "--records-sha256", &"b".repeat(64),
            "--sqlite", "C:/workspace.sqlite"];
        assert!(parse(args.map(OsString::from)).is_err());
    }

    #[tokio::test]
    async fn acquired_authority_suppresses_legacy_bridge_before_job_creation() {
        let (mut app, _temp) = crate::tests::test_app().await;
        app.bridge = PathBuf::from("C:/nonexistent-company-knowledge-bridge.mjs");
        app.change(|d| {
            d["companyKnowledgeAuthority"] = json!({"owner":"communityhero","account":"LikeAvto","companyKey":"likeavto"});
            Ok(())
        }).await.unwrap();
        let jobs_before = app.read().await.unwrap()["jobs"].as_array().unwrap().len();
        let Json(response) = crate::materials_import(State(app.clone())).await.unwrap();
        assert_eq!(response["legacyImportSuppressed"], true);
        assert_eq!(app.read().await.unwrap()["jobs"].as_array().unwrap().len(), jobs_before);
        app.change(|d| crate::merge_materials(d, &json!({"materials":[
            {"id":"ordinary","kind":"knowledge","title":"Old","text":"Not admitted"},
            {"id":"clip","kind":"transcript","title":"Clip","text":"Media remains available"}
        ]}))).await.unwrap();
        let workspace = app.read().await.unwrap();
        assert!(!workspace["materials"].as_array().unwrap().iter().any(|row| row["id"] == "import-ordinary"));
        assert!(workspace["materials"].as_array().unwrap().iter().any(|row| row["id"] == "import-clip"));
    }

    #[tokio::test]
    async fn sqlite_check_apply_precondition_and_replay_use_existing_transaction_boundary() {
        let (app,temp)=crate::tests::test_app().await;
        let before=app.db.read().await.unwrap();app.db.close().await;
        let package=temp.path().join("package");fs::create_dir(&package).unwrap();
        let records=serde_json::to_vec(&json!({"companyKey":"likeavto","importKey":"synthetic-rule","kind":"rule","text":"Company guidance",
            "scope":{"companyKey":"likeavto"},"source":{"origin":"synthetic","sha256":"a".repeat(64)},"metadata":{"grantsExecutionAuthority":false,"category":"brand_policy"}})).unwrap();
        let coverage=b"[]";
        let manifest=serde_json::to_vec(&json!({"schemaVersion":"communityhero.company-knowledge.v1","generatedAt":now(),"recordCount":1,"grantsExecutionAuthority":false,
            "companies":{"likeavto":{"recordCount":1}},"files":{"records.jsonl":{"bytes":records.len(),"sha256":digest(&records)},"media-coverage.json":{"bytes":coverage.len(),"sha256":digest(coverage)}}})).unwrap();
        fs::write(package.join("manifest.json"),&manifest).unwrap();fs::write(package.join("records.jsonl"),&records).unwrap();fs::write(package.join("media-coverage.json"),coverage).unwrap();
        let options=|mode,expected|Options{mode,package:package.clone(),company:"likeavto".into(),manifest_sha256:digest(&manifest),records_sha256:digest(&records),expected_workspace_sha256:expected,source:Source::Sqlite(temp.path().join("workspace.sqlite"))};
        let check=execute(options(Mode::Check,None)).await.unwrap();assert_eq!(check["result"]["imported"],1);
        let db=open_existing_sqlite(&temp.path().join("workspace.sqlite"),true).await.unwrap();assert_eq!(db.read().await.unwrap(),before);db.close().await;
        let expected=check["workspaceSha256"].as_str().unwrap().to_owned();
        let applied=execute(options(Mode::Apply,Some(expected.clone()))).await.unwrap();assert_eq!(applied["changed"],true);
        assert!(execute(options(Mode::Apply,Some(expected))).await.is_err());
        let check=execute(options(Mode::Check,None)).await.unwrap();assert_eq!(check["result"]["replayed"],1);
        let replay=execute(options(Mode::Apply,Some(check["workspaceSha256"].as_str().unwrap().into()))).await.unwrap();assert_eq!(replay["changed"],false);
    }
}
