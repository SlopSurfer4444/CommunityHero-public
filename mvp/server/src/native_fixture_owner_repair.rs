//! Explicit test-only fixture setup. Production startup never imports this module.
use crate::*;
use crate::runtime_lifecycle::RuntimeIdentity;

/// Supply absent mandatory ledger collections in synthetic fixture seed data.
/// Existing values, raw jobs/UNKNOWN/paid histories and connector identity are retained.
/// This is ordinary test seeding, never a lifecycle-only transition callback.
fn complete_ledger(d:&mut Value)->ApiResult<()> {
    let profile=crate::accounts::Profile::from_workspace(d)?;
    let object=d.as_object_mut().ok_or_else(||bad("Fixture workspace must be an object"))?;
    if !object.contains_key("connectorBinding") {object.insert("connectorBinding".into(),profile.binding());}
    if !object["connectorBinding"].is_object(){return Err(bad("Fixture connector binding malformed"));}
    for key in ["jobs","operations","approvals","audit","materials","knowledge_entries","knowledge_versions"] {
        if !object.contains_key(key){object.insert(key.into(),json!([]));}
        if !object[key].is_array(){return Err(bad("Fixture ledger collection malformed"));}
    }
    Ok(())
}

pub(crate) fn initialize_workspace(d:&mut Value)->ApiResult<RuntimeIdentity> {
    let profile=crate::accounts::Profile::from_workspace(d)?;
    let identity=crate::runtime_lifecycle_startup::Admission::fixture(profile).identity().clone();
    if d.get("runtimeLifecycle").is_some() {
        // Never overwrite, resume, retarget or repair existing rejection fixtures.
        crate::runtime_lifecycle::bound_admission_token(d,&identity,crate::runtime_lifecycle::AdmissionClass::SourceRead)?;
    } else {
        complete_ledger(d)?;
        crate::runtime_lifecycle_startup::initialize_fixture(d,&identity)?;
    }
    Ok(identity)
}

pub(crate) async fn initialize_db(db:&crate::Database)->ApiResult<RuntimeIdentity> {
    // Ledger seed completion is an ordinary isolated fixture writer. Native
    // initialize runs separately under the complete lifecycle ledger lock.
    let metadata=db.read_metadata().await?;
    let profile=crate::accounts::Profile::from_workspace(&metadata)?;
    let identity=crate::runtime_lifecycle_startup::Admission::fixture(profile).identity().clone();
    db.change(|d| {
        if d.get("runtimeLifecycle").is_some() {
            crate::runtime_lifecycle::bound_admission_token(d,&identity,crate::runtime_lifecycle::AdmissionClass::SourceRead)?;
        } else {complete_ledger(d)?;}
        Ok(())
    }).await?;
    db.change_runtime_lifecycle_with_ledger(|d| {
        if d.get("runtimeLifecycle").is_none() {
            crate::runtime_lifecycle_startup::initialize_fixture(d,&identity)?;
        } else {
            crate::runtime_lifecycle::bound_admission_token(d,&identity,crate::runtime_lifecycle::AdmissionClass::SourceRead)?;
        }
        Ok(())
    }).await?;
    Ok(identity)
}

#[test]
fn fixture_bootstrap_keeps_raw_unknown_and_paid_history_and_exact_account() {
    for profile in [crate::accounts::Profile::LikeAvto,crate::accounts::Profile::BawRussia] {
        let mut d=crate::empty();crate::accounts::initialize(&mut d,profile).unwrap();
        d["jobs"]=json!([{"id":"paid","kind":"assistant","status":"paused","result":{"checkpoint":"retained"}}]);
        d["operations"]=json!([{"id":"unknown","status":"unknown","payload":{"raw":"same"}}]);
        let jobs=d["jobs"].clone();let ops=d["operations"].clone();
        let identity=initialize_workspace(&mut d).unwrap();
        let owner=crate::runtime_lifecycle::current_owner(&d,&identity).unwrap();
        assert_eq!(owner.account,profile.display());assert_eq!(owner.epoch,1);
        assert_eq!(d["jobs"],jobs);assert_eq!(d["operations"],ops);
        let baseline=d.clone();initialize_workspace(&mut d).unwrap();assert_eq!(d,baseline);
    }
}
#[test]
fn fixture_setup_never_repairs_foreign_stale_or_closed_owner() {
    let mut seed=crate::empty();initialize_workspace(&mut seed).unwrap();
    for variant in ["foreign","stale","closed","malformed"] {
        let mut d=seed.clone();
        match variant {
            "foreign"=>d["runtimeLifecycle"]["owner"]["runtimeId"]=json!("foreign-runtime"),
            "stale"=>d["runtimeLifecycle"]["owner"]["epoch"]=json!(0),
            "closed"=>d["runtimeLifecycle"]["phase"]=json!("draining"),
            _=>{d["runtimeLifecycle"].as_object_mut().unwrap().remove("queuedBacklog");},
        }
        let before=d.clone();assert!(initialize_workspace(&mut d).is_err(),"{variant}");assert_eq!(d,before);
    }
}

#[tokio::test]
async fn db_fixture_rejects_foreign_or_closed_owner_before_any_seed_write() {
    for variant in ["foreign","closed"] {
        let folder=tempfile::tempdir().unwrap();
        let db=crate::Database::Sqlite(crate::open_db(&folder.path().join("hostile.sqlite")).await.unwrap());
        let mut d=crate::empty();let identity=initialize_workspace(&mut d).unwrap();
        if variant=="foreign" {d["runtimeLifecycle"]["owner"]["runtimeId"]=json!("foreign-runtime");}
        else {
            let owner=crate::runtime_lifecycle::current_owner(&d,&identity).unwrap();
            crate::runtime_lifecycle::begin_drain(&mut d,&owner,&"c".repeat(64),"fixture-drain",false).unwrap();
        }
        // A hostile lifecycle owner must reach initialize_db through a valid
        // storage fixture. Retain its company connector and supply the ordinary
        // feedback collection required by SQLite history validation.
        d["feedback"]=json!([]);
        db.change(|stored|{*stored=d.clone();Ok(())}).await.unwrap();
        let before=db.read().await.unwrap();
        let rejected=initialize_db(&db).await;
        assert!(rejected.is_err(),"{variant}");
        let error=rejected.err().unwrap();
        assert_eq!(error.0,axum::http::StatusCode::CONFLICT,"{variant}: fixture must reach the native owner rejection");
        assert!(error.1.contains("Runtime lifecycle"),"{variant}: {error:?}");
        assert_eq!(db.read().await.unwrap(),before,"rejection cannot add or rewrite fixture ledger fields");
        db.close().await;
    }
}
