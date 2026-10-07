//! Early native command: complete read-only ledger digest, without server startup.
use crate::{accounts::Profile, runtime_lifecycle, ApiResult, Value};
use std::ffi::OsString;

fn fail()->crate::ApiError { crate::conflict("Native read-only lifecycle ledger capture failed") }
fn parse_args(args:impl IntoIterator<Item=OsString>)->ApiResult<Profile> {
    let args:Vec<OsString>=args.into_iter().collect();
    if args.len()!=2 || args[0]!="--account" {return Err(fail());}
    let account=args[1].to_str().ok_or_else(fail)?;
    Profile::parse(account).map_err(|_|fail())
}
/// Hash exactly the native complete workspace; do not hash a SQL text aggregate,
/// filtered JSON view or JSON bytes reserialized by a different runtime.
pub(crate) fn snapshot_report(workspace:&Value,profile:Profile)->ApiResult<Value> {
    if Profile::from_workspace(workspace).map_err(|_|fail())?!=profile {return Err(fail());}
    let digest=runtime_lifecycle::ledger_digest(workspace).map_err(|_|fail())?;
    let mut counts=serde_json::Map::new();
    for table in ["posts","branches","items","conversations","proposals","approvals","operations","materials","jobs","audit","knowledge_entries","knowledge_versions","feedback"] {
        let rows=workspace[table].as_array().ok_or_else(fail)?;
        counts.insert(table.into(),serde_json::json!(rows.len()));
    }
    Ok(serde_json::json!({"schemaVersion":1,"kind":"native-read-only-runtime-lifecycle-ledger-digest",
        "status":"captured","account":profile.key(),"displayAccount":profile.display(),"workspaceId":"local-pilot",
        "expectedLedgerSha256":digest,"serialization":"native-serde-json-full-workspace-except-runtimeLifecycle",
        "isolation":"repeatable-read/read-only","collections":counts,
        "lifecyclePresent":workspace.get("runtimeLifecycle").is_some(),
        "stopAuthorized":false,"bootstrapAuthorized":false}))
}
pub(crate) async fn run(args:impl IntoIterator<Item=OsString>)->Result<(),Box<dyn std::error::Error>> {
    // Errors deliberately omit database URLs, SQL connection details and bodies.
    let profile=parse_args(args).map_err(|_|"Expected lifecycle-ledger-digest --account likeavto|baw-russia")?;
    let url=std::env::var("COMMUNITYHERO_DATABASE_URL").map_err(|_|"Native ledger capture requires COMMUNITYHERO_DATABASE_URL")?;
    let report=crate::storage::read_bootstrap_ledger_snapshot(&url,profile).await
        .map_err(|_|"Native read-only lifecycle ledger capture failed; no startup admission emitted")?;
    println!("{}",serde_json::to_string(&report).map_err(|_|"Native ledger report serialization failed")?);
    Ok(())
}

#[cfg(test)] mod tests {
    use super::*;
    fn fixture(profile:Profile)->Value {
        let mut d=crate::empty();crate::accounts::initialize(&mut d,profile).unwrap();
        for key in ["knowledge_entries","knowledge_versions","feedback"] {d[key]=serde_json::json!([]);}
        d["jobs"]=serde_json::json!([{"id":"paused-paid","kind":"assistant","status":"paused","checkpoint":{"paid":"retain"}}]);
        d["operations"]=serde_json::json!([{"id":"unknown","status":"unknown","payload":{"raw":"retain"}}]);
        d
    }
    #[test] fn exact_args_have_no_file_sql_or_implicit_account_mode() {
        for account in ["likeavto","baw-russia"] {assert!(parse_args([OsString::from("--account"),OsString::from(account)]).is_ok());}
        for args in [vec![],vec!["--account"],vec!["--account","foreign"],vec!["--account","likeavto","--apply"],vec!["--input","fixture.json"]] {
            assert!(parse_args(args.into_iter().map(OsString::from)).is_err());
        }
    }
    #[test] fn reports_native_digest_without_mutation_or_authority_for_both_companies() {
        for profile in [Profile::LikeAvto,Profile::BawRussia] {
            let d=fixture(profile);let before=d.clone();let expected=runtime_lifecycle::ledger_digest(&d).unwrap();
            let report=snapshot_report(&d,profile).unwrap();assert_eq!(d,before);
            assert_eq!(report["expectedLedgerSha256"],expected);assert_eq!(report["collections"]["jobs"],1);
            assert_eq!(report["collections"]["operations"],1);assert_eq!(report["lifecyclePresent"],false);
            assert_eq!(report["stopAuthorized"],false);assert_eq!(report["bootstrapAuthorized"],false);
            let text=report.to_string();for private in ["paused-paid","checkpoint","unknown\"","payload","retain"] {assert!(!text.contains(private));}
        }
    }
    #[test] fn full_native_value_preserves_float_and_unrelated_metadata_identity() {
        let mut d=fixture(Profile::LikeAvto);d["nativeFloatWitness"]=serde_json::from_str("0.0").unwrap();
        let float=snapshot_report(&d,Profile::LikeAvto).unwrap()["expectedLedgerSha256"].clone();
        d["nativeFloatWitness"]=serde_json::from_str("0").unwrap();
        assert_ne!(snapshot_report(&d,Profile::LikeAvto).unwrap()["expectedLedgerSha256"],float);
        let before=snapshot_report(&d,Profile::LikeAvto).unwrap()["expectedLedgerSha256"].clone();
        d["settings"]["unrelatedWitness"]=serde_json::json!("bound");
        assert_ne!(snapshot_report(&d,Profile::LikeAvto).unwrap()["expectedLedgerSha256"],before);
        assert!(snapshot_report(&d,Profile::BawRussia).is_err());
        d.as_object_mut().unwrap().remove("connectorBinding");assert!(snapshot_report(&d,Profile::LikeAvto).is_err());
    }
    #[test] fn native_bootstrap_metadata_is_excluded_but_old_ledger_history_is_not() {
        let mut d=fixture(Profile::LikeAvto);let initial=runtime_lifecycle::ledger_digest(&d).unwrap();
        let identity=crate::runtime_lifecycle_startup::Admission::fixture(Profile::LikeAvto).identity().clone();
        crate::runtime_lifecycle_startup::initialize_fixture(&mut d,&identity).unwrap();
        let report=snapshot_report(&d,Profile::LikeAvto).unwrap();assert_eq!(report["expectedLedgerSha256"],initial);
        assert_eq!(report["lifecyclePresent"],true);
        d["operations"][0]["payload"]["raw"]=serde_json::json!("changed");
        assert_ne!(snapshot_report(&d,Profile::LikeAvto).unwrap()["expectedLedgerSha256"],initial);
    }
}
