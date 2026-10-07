//! Actual full native writer with a BAW CAS photo; root runs these tests.
use super::*;
use crate::*;
async fn acquired()->(App,tempfile::TempDir,Scheduled,Value){
    let(app,temp)=super::first_capture_recovery_tests::baw_app().await;
    app.change(|d|{row_mut(d,"posts","ready-post")?["attachments"]=json!([{"type":"photo","url":"https://fixture.invalid/mandatory.png"}]);Ok(())}).await.unwrap();
    let token=app.lifecycle_admission_token(runtime_lifecycle::AdmissionClass::Preparation).await.unwrap();
    let scheduled=app.change_preparation_schedule(|d|schedule_admitted(d,Input{item_ids:vec!["ready".into()],instruction:None},&token)).await.unwrap();
    assert_eq!(scheduled.request.as_ref().unwrap()["materialReadiness"]["status"],"pending");
    app.change(|d|photo_acquisition::fixture_commit_baw_photo(d,"ready-post",&now()).map(|_|())).await.unwrap();
    let before=app.db.read().await.unwrap();(app,temp,scheduled,before)
}
#[tokio::test]
async fn verified_photo_refines_only_unpaid_materials_and_exact_scope_digest_in_the_full_writer(){
    let(app,_temp,scheduled,before)=acquired().await;let run=&scheduled.job_id;let original=scheduled.request.as_ref().unwrap();
    assert!(preflight_capture(&before,run).is_ok());assert!(preflight(&before,run).is_err());
    let request=app.change(|d|refresh_unpaid_materials(d,run,original)).await.unwrap();assert_eq!(request["materialReadiness"]["status"],"ready");
    let after=app.db.read().await.unwrap();let old=row(&before,"jobs",run).unwrap();let new=row(&after,"jobs",run).unwrap();
    assert_ne!(new["prepareBundle"]["digest"],old["prepareBundle"]["digest"]);assert_eq!(new["prepareBundle"]["id"],old["prepareBundle"]["id"]);
    assert_eq!(new["prepareBundle"]["request"]["strictGroup"],old["prepareBundle"]["request"]["strictGroup"]);
    let mut scope=new["scopeReservation"].clone();scope["prepareBundleDigest"]=old["scopeReservation"]["prepareBundleDigest"].clone();assert_eq!(scope,old["scopeReservation"]);
    assert_eq!(new["preparationStages"]["initialAdmission"]["owner"],old["preparationStages"]["initialAdmission"]["owner"]);
    assert_eq!(new["preparationStages"]["initialAdmission"]["admittedAt"],old["preparationStages"]["initialAdmission"]["admittedAt"]);
    assert!(new["preparationStages"]["first"].is_null());assert!(new["preparationStages"]["firstAdmission"].is_null());
    preparation_reservations::validate_change(&before,&after).unwrap();preflight(&after,run).unwrap();
    let token=app.lifecycle_admission_token(runtime_lifecycle::AdmissionClass::Preparation).await.unwrap();
    app.change_preparation_first(run,|d|preparation_review::reserve_first_admitted(d,&token,run,&request,&now())).await.unwrap();
    let reserved=app.db.read().await.unwrap();assert!(app.change(|d|refresh_unpaid_materials(d,run,&request)).await.is_err());assert_eq!(app.db.read().await.unwrap(),reserved);
    assert!(!app.node.exists());assert!(!app.bridge.exists());app.db.close().await;
}
#[tokio::test]
async fn forged_refinement_cannot_change_source_recipient_family_instruction_or_owned_history(){
    let(app,_temp,scheduled,before)=acquired().await;let run=&scheduled.job_id;
    app.change(|d|refresh_unpaid_materials(d,run,scheduled.request.as_ref().unwrap())).await.unwrap();let valid=app.db.read().await.unwrap();
    for fault in ["source","recipient","family","instruction","group","owner","keys","stage","paid","history"]{
        let mut forged=valid.clone();
        match fault{
            "source"=>row_mut(&mut forged,"posts","ready-post").unwrap()["text"]=json!("Retargeted source"),
            "recipient"=>row_mut(&mut forged,"items","ready").unwrap()["revision"]=json!(99),
            _=>{let job=row_mut(&mut forged,"jobs",run).unwrap();match fault{
                "family"=>job["prepareBundle"]["request"]["strictGroup"]["familyKey"]=json!("invented-family"),
                "instruction"=>job["prepareBundle"]["request"]["instruction"]=json!("Injected operator instructions"),
                "group"=>job["preparationStages"]["groupAdmission"][0]["status"]=json!("admitted"),
                "owner"=>job["preparationStages"]["initialAdmission"]["owner"]["epoch"]=json!(99),
                "keys"=>job["scopeReservation"]["keys"]=json!([]),
                "stage"=>job["preparationStages"]["first"]=json!({"status":"completed"}),
                "paid"=>job["preparationStages"]["firstAdmission"]=json!({"status":"reserved"}),
                _=>job["retainedEvidence"]=json!([{"kind":"forged-paid"}]),
            }}
        }
        // Rehashing all public capture digests must not turn the edit into
        // authorized mandatory-material enrichment.
        let job=row_mut(&mut forged,"jobs",run).unwrap();rehash(&mut job["prepareBundle"]);
        job["scopeReservation"]["prepareBundleDigest"]=job["prepareBundle"]["digest"].clone();
        job["preparationStages"]["initialAdmission"]["requestSha256"]=job["prepareBundle"]["digest"].clone();
        assert!(preparation_reservations::validate_change(&before,&forged).is_err(),"{fault}");
    }
    assert_eq!(app.db.read().await.unwrap(),valid);app.db.close().await;
}
#[tokio::test]
async fn invalid_photo_refinement_rolls_back_source_and_scope_transactionally(){
    let(app,_temp,scheduled,_before)=acquired().await;let run=&scheduled.job_id;
    let valid=app.db.read().await.unwrap();assert!(app.change(|d|{
        row_mut(d,"posts","ready-post")?["photoAcquisition"]["receiptSha256"]=json!("0".repeat(64));
        refresh_unpaid_materials(d,run,scheduled.request.as_ref().unwrap())
    }).await.is_err());
    assert_eq!(app.db.read().await.unwrap(),valid);assert!(!app.node.exists());assert!(!app.bridge.exists());app.db.close().await;
}
