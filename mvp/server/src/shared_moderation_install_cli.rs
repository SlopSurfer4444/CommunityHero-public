//! One-shot installation of the owner-approved shared moderation source into
//! the stopped LikeAvto PostgreSQL workspace. No server/provider/worker starts.
use crate::{ApiResult, Database, accounts, auto_prepare, bad, conflict, knowledge, now};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{ffi::OsString, fs, io::{self, IsTerminal, Read}, path::PathBuf};

const REQUEST_ID: &str = "shared-abuse-moderation-20260928-likeavto-v1";
const ACCOUNT: &str = "LikeAvto";
const DATABASE: &str = "communityhero_snapshot_test";

fn digest(bytes: &[u8]) -> String { format!("{:x}", Sha256::digest(bytes)) }
fn workspace_digest(value: &Value) -> String { digest(value.to_string().as_bytes()) }

fn input(args: impl IntoIterator<Item=OsString>) -> Result<(bool,PathBuf,Option<String>,Option<String>), &'static str> {
    let mut args=args.into_iter();
    let apply=match args.next().and_then(|v|v.into_string().ok()).as_deref() {
        Some("--check")=>false,Some("--apply")=>true,_=>return Err("Expected --check or --apply")
    };
    let source=PathBuf::from(args.next().ok_or("Missing absolute source path")?);
    if !source.is_absolute(){return Err("Source path must be absolute");}
    if args.next().as_deref()!=Some(std::ffi::OsStr::new("--database-url-stdin")) {
        return Err("Expected --database-url-stdin");
    }
    let workspace=args.next().map(|v|v.into_string().map_err(|_|"Invalid workspace digest")).transpose()?;
    let source_hash=args.next().map(|v|v.into_string().map_err(|_|"Invalid source digest")).transpose()?;
    if args.next().is_some() || (apply && (!workspace.is_some() || !source_hash.is_some()))
        || (!apply && (workspace.is_some() || source_hash.is_some())) {
        return Err("Apply requires checked workspace and source digests");
    }
    if [workspace.as_ref(),source_hash.as_ref()].into_iter().flatten().any(|v|
        v.len()!=64||!v.bytes().all(|b|b.is_ascii_hexdigit())) {
        return Err("Invalid digest");
    }
    Ok((apply,source,workspace,source_hash))
}

fn read_database_url(reader: impl Read) -> Result<String, &'static str> {
    let mut bytes=Vec::new();
    reader.take(8193).read_to_end(&mut bytes).map_err(|_|"Cannot read database connection from stdin")?;
    if bytes.len()>8192{return Err("Database connection input too large");}
    let raw=String::from_utf8(bytes).map_err(|_|"Invalid database connection encoding")?;
    let url=raw.trim_end_matches(['\r','\n']);
    if url.contains(['\r','\n'])||!url.starts_with("postgresql://")||!url.ends_with(&format!("/{DATABASE}")) {
        return Err("Unexpected PostgreSQL connection on stdin");
    }
    Ok(url.to_owned())
}

fn request(source: &Value) -> ApiResult<Value> {
    if source["schemaVersion"]!=1 || source["ruleId"]!="SHARED-MODERATION-ABUSE-20260928"
        || source["accounts"]["likeavto"]["displayAccount"]!=ACCOUNT
        || source["accounts"]["likeavto"]["requestId"]!=REQUEST_ID
        || source["accounts"]["baw"]["displayAccount"]!="BAW Russia"
        || source["accounts"]["baw"]["requestId"]!="shared-abuse-moderation-20260928-baw-v1" {
        return Err(bad("Unexpected shared moderation source"));
    }
    let title=source["title"].as_str().ok_or_else(||bad("Missing rule title"))?;
    let text=source["text"].as_str().ok_or_else(||bad("Missing rule text"))?;
    if title.trim().is_empty()||text.trim().is_empty()||title.encode_utf16().count()>240
        ||text.encode_utf16().count()>24000 {
        return Err(bad("Invalid rule text"));
    }
    Ok(json!({"requestId":REQUEST_ID,"title":title,"text":text,"postKey":null}))
}

fn installed(data: &Value, request: &Value) -> ApiResult<Option<Value>> {
    if accounts::Profile::from_workspace(data)?!=accounts::Profile::LikeAvto
        || crate::active_binding(data)?.account_id!=ACCOUNT {
        return Err(conflict("Database does not belong to LikeAvto"));
    }
    let versions=data["knowledge_versions"].as_array().ok_or_else(||conflict("Missing knowledge versions"))?;
    let found:Vec<&Value>=versions.iter().filter(|v|v["operatorRequestId"]==REQUEST_ID).collect();
    if found.len()>1{return Err(conflict("Duplicate moderation request ID"));}
    let Some(v)=found.first() else {return Ok(None)};
    if v["manualInstruction"]!=true||v["kind"]!="rule"||v["category"]!="operator_instruction"
        ||v["trust"]!="verified"||v["status"]!="active"||v["title"]!=request["title"]
        ||v["text"]!=request["text"]||v["postKey"]!=""
        ||v["scope"]!=json!({"account":ACCOUNT,"postKeys":[]})
        ||!data["knowledge_entries"].as_array().into_iter().flatten().any(|e|
            e["id"]==v["entryId"]&&e["currentVersionId"]==v["id"]) {
        return Err(conflict("Moderation request exists but is not this active rule head"));
    }
    Ok(Some((*v).clone()))
}

fn selected(data: &Value, version: &Value) -> ApiResult<()> {
    let result=knowledge::select(data,&[],&[],&now()).map_err(conflict)?;
    if !result["manifest"].as_array().into_iter().flatten().any(|pin|
        pin["entryId"]==version["entryId"]&&pin["versionId"]==version["id"]
        &&pin["hash"]==version["hash"]&&pin["scope"]==json!({"account":ACCOUNT,"postKeys":[]}))
        ||!result["materials"].as_array().into_iter().flatten().any(|m|
            m["knowledgeVersionId"]==version["id"]&&m["text"]==version["text"]) {
        return Err(conflict("Current global moderation rule is not selected into preparation"));
    }
    Ok(())
}

pub async fn run(args: impl IntoIterator<Item=OsString>) -> Result<(),Box<dyn std::error::Error>> {
    let (apply,path,expected_workspace,expected_source)=input(args)?;
    let bytes=fs::read(path)?;
    if bytes.len()>32*1024{return Err("Source too large".into());}
    let source_hash=digest(&bytes);
    if apply&&expected_source.as_deref()!=Some(source_hash.as_str()) {
        return Err("Source changed since check".into());
    }
    let request=request(&serde_json::from_slice::<Value>(&bytes)?).map_err(|e|e.1)?;
    let stdin=io::stdin();
    if stdin.is_terminal(){return Err("Pipe PostgreSQL URL through stdin; interactive secret entry is disabled".into());}
    let url=read_database_url(stdin.lock())?;
    let db=Database::postgres(&url).await.map_err(|_|"PostgreSQL connection failed (details withheld)")?;
    let result:ApiResult<Value>=async {
        let before=db.read().await?;
        let previous=installed(&before,&request)?;
        if let Some(v)=previous {
            selected(&before,&v)?;
            return Ok(json!({"status":"already_installed","sourceSha256":source_hash,
                "entryId":v["entryId"],"versionId":v["id"],"versionHash":v["hash"]}));
        }
        if before["knowledge_entries"].as_array().into_iter().flatten().any(|entry|
            before["knowledge_versions"].as_array().into_iter().flatten().any(|v|
                v["id"]==entry["currentVersionId"]&&v["kind"]=="rule"&&v["status"]=="active"
                &&v["scope"]["account"]==ACCOUNT
                &&(v["title"]==request["title"]||v["text"]==request["text"]))) {
            return Err(conflict("Equivalent active rule exists under another request ID"));
        }
        let workspace=workspace_digest(&before);
        let mut preview=before.clone();
        let receipt=knowledge::save_instruction(&mut preview,&request,&now()).map_err(conflict)?;
        selected(&preview,&receipt["version"])?;
        auto_prepare::reconcile_stale(&mut preview,chrono::Utc::now().timestamp());
        if !apply {
            return Ok(json!({"status":"checked","sourceSha256":source_hash,
                "workspaceDigest":workspace,"previewEntryId":receipt["entry"]["id"],
                "ruleSelected":true}));
        }
        if expected_workspace.as_deref()!=Some(workspace.as_str()) {
            return Err(conflict("Workspace changed since check"));
        }
        let checked=workspace.clone();
        let request_for_tx=request.clone();
        let receipt=db.change(|data| {
            if workspace_digest(data)!=checked||installed(data,&request_for_tx)?.is_some() {
                return Err(conflict("Workspace changed before installation"));
            }
            let receipt=knowledge::save_instruction(data,&request_for_tx,&now()).map_err(conflict)?;
            selected(data,&receipt["version"])?;
            auto_prepare::reconcile_stale(data,chrono::Utc::now().timestamp());
            crate::audit(data,"knowledge.instruction",receipt["entry"]["id"].as_str().unwrap());
            Ok(receipt)
        }).await?;
        let after=db.read().await?;
        let v=installed(&after,&request)?.ok_or_else(||conflict("Installed rule missing on readback"))?;
        selected(&after,&v)?;
        if v["id"]!=receipt["version"]["id"] {return Err(conflict("Installed version mismatch"));}
        Ok(json!({"status":"installed","sourceSha256":source_hash,
            "entryId":v["entryId"],"versionId":v["id"],"versionHash":v["hash"],
            "ruleSelected":true}))
    }.await;
    db.close().await;
    println!("{}",result.map_err(|e|e.1)?);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn apply_requires_digests() {
        let args=["--apply","C:/source.json","--database-url-stdin"];
        assert!(input(args.into_iter().map(OsString::from)).is_err());
    }
    #[test] fn url_is_exact_and_never_in_error() {
        let url="postgresql://user:fake-secret@127.0.0.1/communityhero_snapshot_test";
        assert_eq!(read_database_url(format!("{url}\n").as_bytes()).unwrap(),url);
        for invalid in [url.replace("snapshot_test","foreign"),format!("{url}\n{url}"),"x".repeat(8193)] {
            assert!(!read_database_url(invalid.as_bytes()).unwrap_err().contains("fake-secret"));
        }
    }
    #[test] fn source_request_is_exact() {
        let source:Value=serde_json::from_slice(include_bytes!("../../../project/company-moderation-2026-09-28/shared-abuse-rule.v1.json")).unwrap();
        let body=request(&source).unwrap();
        assert_eq!(body["requestId"],REQUEST_ID);
        assert_eq!(body["postKey"],Value::Null);
        assert!(body["text"].as_str().unwrap().contains("людей и групп"));
        let mut forged=source;
        forged["accounts"]["likeavto"]["requestId"]=json!("foreign");
        assert!(request(&forged).is_err());
    }
    #[test] fn versioned_rule_is_selected_only_for_its_company() {
        let source:Value=serde_json::from_slice(include_bytes!("../../../project/company-moderation-2026-09-28/shared-abuse-rule.v1.json")).unwrap();
        let body=request(&source).unwrap();
        let mut likeavto=crate::empty();
        accounts::initialize(&mut likeavto,accounts::Profile::LikeAvto).unwrap();
        let receipt=knowledge::save_instruction(&mut likeavto,&body,"2026-09-28T12:00:00Z").unwrap();
        let v=installed(&likeavto,&body).unwrap().unwrap();
        assert_eq!(v["id"],receipt["version"]["id"]);
        selected(&likeavto,&v).unwrap();
        let mut baw=crate::empty();
        accounts::initialize(&mut baw,accounts::Profile::BawRussia).unwrap();
        assert!(installed(&baw,&body).is_err());
        let empty=knowledge::select(&baw,&[],&[],"2026-09-28T12:00:00Z").unwrap();
        assert!(empty["manifest"].as_array().unwrap().iter().all(|pin|pin["versionId"]!=v["id"]));
    }
}
