//! One-shot installation through the same versioned domain and repository as HTTP.
//! No server recovery, workers, provider or external-action route is started.
use crate::{ApiResult, Database, accounts, auto_prepare, bad, conflict, knowledge, now};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{ffi::OsString, fs, io::{self, IsTerminal, Read}, path::PathBuf};

fn digest(value: &Value) -> String {
    format!("{:x}", Sha256::digest(value.to_string().as_bytes()))
}

fn input(args: impl IntoIterator<Item = OsString>) -> Result<(bool, PathBuf, Option<String>, Option<String>), &'static str> {
    let mut args = args.into_iter();
    let mode = args.next().ok_or("Expected --check or --apply")?;
    let apply = match mode.to_str() { Some("--check") => false, Some("--apply") => true, _ => return Err("Expected --check or --apply") };
    let candidate = PathBuf::from(args.next().ok_or("Missing candidate path")?);
    if args.next().as_deref() != Some(std::ffi::OsStr::new("--database-url-stdin")) {
        return Err("Expected --database-url-stdin; connection files and URL arguments are not accepted");
    }
    if !candidate.is_absolute() { return Err("Candidate path must be absolute"); }
    let expected = args.next().map(|v| v.into_string().map_err(|_| "Invalid workspace digest")).transpose()?;
    let candidate_hash = args.next().map(|v| v.into_string().map_err(|_| "Invalid candidate digest")).transpose()?;
    if args.next().is_some() || (apply && (expected.is_none() || candidate_hash.is_none()))
        || (!apply && (expected.is_some() || candidate_hash.is_some())) {
        return Err("Apply requires checked workspace and candidate digests");
    }
    if [expected.as_ref(),candidate_hash.as_ref()].into_iter().flatten().any(|v|
        v.len()!=64 || !v.bytes().all(|b| b.is_ascii_hexdigit())) {
        return Err("Invalid digest");
    }
    Ok((apply, candidate, expected, candidate_hash))
}

// Bounded, single-line secret input. Never include the input or underlying
// decoding error in diagnostics, and never prompt with terminal echo enabled.
fn read_database_url(reader: impl Read) -> Result<String, &'static str> {
    let mut bytes = Vec::new();
    reader.take(8193).read_to_end(&mut bytes).map_err(|_| "Cannot read database connection from stdin")?;
    if bytes.len() > 8192 { return Err("Database connection input too large"); }
    let raw = String::from_utf8(bytes).map_err(|_| "Invalid database connection encoding")?;
    let url = raw.trim_end_matches(['\r', '\n']);
    if url.contains(['\r', '\n']) || !url.starts_with("postgresql://")
        || !url.ends_with("/communityhero_snapshot_test") {
        return Err("Unexpected PostgreSQL connection on stdin");
    }
    Ok(url.to_owned())
}

fn active_heads(data: &Value) -> Value {
    let mut heads: Vec<Value> = data["knowledge_entries"].as_array().into_iter().flatten().filter_map(|entry| {
        let version = data["knowledge_versions"].as_array()?.iter().find(|v| v["id"] == entry["currentVersionId"])?;
        if version["kind"] != "rule" || version["status"] != "active" { return None; }
        Some(json!({"entryId":entry["id"],"currentVersionId":version["id"],
            "versionHash":version["hash"],"normalizedRuleId":version["ruleNormalization"]["normalizedRuleId"]}))
    }).collect();
    heads.sort_by(|a,b| a["entryId"].as_str().cmp(&b["entryId"].as_str()));
    Value::Array(heads)
}

fn counts(data: &Value) -> Value {
    let count = |rows: &str, key: &str, status: &str| data[rows].as_array().into_iter().flatten()
        .filter(|v| v[key] == status).count();
    json!({"activeRules":active_heads(data).as_array().unwrap().len(),
        "draftProposals":count("proposals","status","draft"),
        "staleProposals":count("proposals","status","stale"),
        "preparedItems":count("items","workflow","prepared"),
        "attentionItems":count("items","workflow","attention")})
}

fn verify(data: &Value, candidate: &Value) -> ApiResult<Option<Value>> {
    if candidate["schemaVersion"] != 1 || candidate["candidateId"] != "likeavto-moderation-2026-09-24-v1"
        || candidate["account"] != "LikeAvto" || candidate["database"] != "communityhero_snapshot_test"
        || candidate["newRule"]["id"] != "LA-M01" {
        return Err(bad("Unexpected LikeAvto candidate"));
    }
    if accounts::Profile::from_workspace(data)? != accounts::Profile::LikeAvto
        || crate::active_binding(data)?.account_id != "LikeAvto" {
        return Err(conflict("Database does not belong to LikeAvto"));
    }
    let request = &candidate["newRule"]["request"];
    if !request.is_object() || request["requestId"] != "likeavto-baseless-personal-taunt-20260924-v1"
        || request["postKey"] != Value::Null || request["entryId"] != Value::Null
        || request["expectedVersionId"] != Value::Null {
        return Err(bad("Unexpected new instruction request"));
    }
    let versions = data["knowledge_versions"].as_array().ok_or_else(|| conflict("Missing knowledge versions"))?;
    if let Some(prior) = versions.iter().find(|v| v["manualInstruction"] == true && v["operatorRequestId"] == request["requestId"]) {
        if prior["title"] != request["title"] || prior["text"] != request["text"] || prior["scope"]["account"] != "LikeAvto" {
            return Err(conflict("Instruction request ID is already bound to different content"));
        }
        if prior["status"] != "active" || !data["knowledge_entries"].as_array().into_iter().flatten().any(|entry|
            entry["id"] == prior["entryId"] && entry["currentVersionId"] == prior["id"]) {
            return Err(conflict("Instruction exists only as a historical version"));
        }
        return Ok(Some(prior.clone()));
    }
    let heads = active_heads(data);
    if heads != candidate["currentHeads"] || heads.as_array().unwrap().len() != 44 {
        return Err(conflict("LikeAvto rule heads changed since candidate review"));
    }
    if data["knowledge_entries"].as_array().into_iter().flatten().any(|entry| {
        versions.iter().find(|v| v["id"] == entry["currentVersionId"]).is_some_and(|v|
            v["status"] == "active" && v["kind"] == "rule" && (v["title"] == request["title"] || v["text"] == request["text"]))
    }) { return Err(conflict("An active matching instruction already exists")); }
    Ok(None)
}

pub async fn run(args: impl IntoIterator<Item = OsString>) -> Result<(), Box<dyn std::error::Error>> {
    let (apply, candidate_path, expected, expected_candidate) = input(args)?;
    let candidate_bytes = fs::read(candidate_path)?;
    if candidate_bytes.len() > 128*1024 { return Err("Candidate file too large".into()); }
    let candidate: Value = serde_json::from_slice(&candidate_bytes)?;
    let candidate_sha256 = format!("{:x}", Sha256::digest(&candidate_bytes));
    if apply && expected_candidate.as_deref() != Some(candidate_sha256.as_str()) {
        return Err("Candidate changed since check".into());
    }
    let stdin = io::stdin();
    if stdin.is_terminal() { return Err("Pipe the database connection through stdin; interactive secret entry is disabled".into()); }
    let url = read_database_url(stdin.lock())?;
    let db = Database::postgres(&url).await.map_err(|_| "PostgreSQL connection failed (connection details withheld)")?;
    let result: ApiResult<Value> = async {
        let before = db.read().await?;
        let prior = verify(&before, &candidate)?;
        if prior.is_some() {
            return Ok(json!({"status":"already_installed","candidateSha256":candidate_sha256,
                "entryId":prior.as_ref().unwrap()["entryId"],"versionId":prior.as_ref().unwrap()["id"]}));
        }
        let before_counts = counts(&before);
        let workspace_digest = digest(&before);
        let mut preview = before.clone();
        let receipt = knowledge::save_instruction(&mut preview, &candidate["newRule"]["request"], &now()).map_err(conflict)?;
        auto_prepare::reconcile_stale(&mut preview, chrono::Utc::now().timestamp());
        let preview_counts = counts(&preview);
        if !apply {
            return Ok(json!({"status":"checked","candidateSha256":candidate_sha256,
                "workspaceDigest":workspace_digest,"before":before_counts,"previewAfter":preview_counts,
                "previewVersionId":receipt["version"]["id"]}));
        }
        if expected.as_deref() != Some(workspace_digest.as_str()) {
            return Err(conflict("Workspace changed since check"));
        }
        let request = candidate["newRule"]["request"].clone();
        let expected_digest = workspace_digest.clone();
        let candidate_for_tx = candidate.clone();
        let receipt = db.change(|data| {
            if digest(data) != expected_digest || verify(data, &candidate_for_tx)?.is_some() {
                return Err(conflict("Workspace changed before installation"));
            }
            let result = knowledge::save_instruction(data, &request, &now()).map_err(conflict)?;
            auto_prepare::reconcile_stale(data, chrono::Utc::now().timestamp());
            crate::audit(data, "knowledge.instruction", result["entry"]["id"].as_str().unwrap());
            Ok(result)
        }).await?;
        let after = db.read().await?;
        let installed = verify(&after, &candidate)?.ok_or_else(|| conflict("Installed rule missing on readback"))?;
        if installed["id"] != receipt["version"]["id"] || installed["text"] != request["text"] {
            return Err(conflict("Installed rule differs on readback"));
        }
        Ok(json!({"status":"installed","candidateSha256":candidate_sha256,"before":before_counts,
            "after":counts(&after),"entryId":installed["entryId"],"versionId":installed["id"],
            "replayed":receipt["replayed"]}))
    }.await;
    db.close().await;
    println!("{}", result.map_err(|e| e.1)?.to_string());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn apply_needs_both_checked_digests() {
        let args = ["--apply", "C:/candidate.json", "--database-url-stdin", &"a".repeat(64)];
        assert!(input(args.into_iter().map(OsString::from)).is_err());
    }
    #[test]
    fn stdin_connection_contract() {
        let url = "postgresql://user:fake-secret@127.0.0.1/communityhero_snapshot_test";
        assert_eq!(read_database_url(format!("{url}\r\n").as_bytes()).unwrap(), url);
        for invalid in [String::new(), format!("{url}\n{url}"), url.replace("snapshot_test", "other"), "x".repeat(8193)] {
            let error = read_database_url(invalid.as_bytes()).unwrap_err();
            assert!(!error.contains("fake-secret"));
        }
        assert!(read_database_url(&[255_u8][..]).is_err());
    }
    #[test]
    fn credential_file_argument_is_rejected() {
        let args = ["--check", "C:/candidate.json", "C:/url.txt"];
        assert!(input(args.into_iter().map(OsString::from)).is_err());
        let args = ["--check", "C:/candidate.json", "--database-url-stdin"];
        assert!(input(args.into_iter().map(OsString::from)).is_ok());
    }
    #[test]
    fn historical_request_is_not_treated_as_installed() {
        let mut data = crate::empty();
        accounts::initialize(&mut data, accounts::Profile::LikeAvto).unwrap();
        data["knowledge_versions"] = json!([{"id":"old","entryId":"rule","manualInstruction":true,
            "operatorRequestId":"likeavto-baseless-personal-taunt-20260924-v1",
            "title":"Личные выпады без содержания","text":"text","status":"active",
            "scope":{"account":"LikeAvto"}}]);
        data["knowledge_entries"] = json!([{"id":"rule","currentVersionId":"new"}]);
        let candidate = json!({"schemaVersion":1,"candidateId":"likeavto-moderation-2026-09-24-v1",
            "account":"LikeAvto","database":"communityhero_snapshot_test","newRule":{"id":"LA-M01",
            "request":{"requestId":"likeavto-baseless-personal-taunt-20260924-v1",
                "title":"Личные выпады без содержания","text":"text","postKey":null,
                "entryId":null,"expectedVersionId":null}}});
        assert!(verify(&data, &candidate).is_err());
    }
}
