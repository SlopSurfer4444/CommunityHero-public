//! Connected admission/settlement tests; ROOT executes with the combined source.
use super::*;
use crate::runtime_lifecycle::{self,AdmissionClass,OwnerToken};

fn lifecycle_fixture()->(Value,Scheduled,OwnerToken) {
    let mut d=super::tests::baw_fixture(false);
    let scheduled=schedule(&mut d,Input{item_ids:vec!["ready".into()],instruction:None}).unwrap();
    crate::native_fixture_owner_repair::initialize_workspace(&mut d).unwrap();
    let token=runtime_lifecycle::admission_token(&d,AdmissionClass::Preparation).unwrap();
    crate::preparation_review::record_initial_admission(&mut d,&token,&scheduled.job_id,&crate::now()).unwrap();
    (d,scheduled,token)
}

#[test]
fn first_pass_reservation_is_single_use_and_preserves_owned_request() {
    let (mut d,scheduled,token)=lifecycle_fixture();let request=scheduled.request.unwrap();
    crate::preparation_review::reserve_first_admitted(&mut d,&token,&scheduled.job_id,&request,&crate::now()).unwrap();
    let receipt=crate::row(&d,"jobs",&scheduled.job_id).unwrap()["preparationStages"]["firstAdmission"].clone();
    assert_eq!(receipt["owner"]["epoch"],1);assert_eq!(receipt["status"],"reserved");
    let before=d.clone();
    assert!(crate::preparation_review::reserve_first_admitted(&mut d,&token,&scheduled.job_id,&request,&crate::now()).is_err());
    assert_eq!(d,before,"reserved first paid work cannot be replayed");
}

#[test]
fn legacy_stale_foreign_and_retargeted_first_passes_are_quarantined() {
    for variant in ["legacy","epoch","owner","initial_shape","initial_hash","request","bundle","cancelled","purpose","settled"] {
        let (mut d,scheduled,token)=lifecycle_fixture();let mut request=scheduled.request.unwrap();
        let job=crate::row_mut(&mut d,"jobs",&scheduled.job_id).unwrap();
        match variant {
            "legacy"=>{job["preparationStages"].as_object_mut().unwrap().remove("initialAdmission");},
            "epoch"=>job["preparationStages"]["initialAdmission"]["owner"]["epoch"]=json!(2),
            "owner"=>job["preparationStages"]["initialAdmission"]["owner"]["runtimeId"]=json!("foreign-runtime"),
            "initial_shape"=>job["preparationStages"]["initialAdmission"]["extra"]=json!(true),
            "initial_hash"=>job["preparationStages"]["initialAdmission"]["requestSha256"]=json!("0".repeat(64)),
            "request"=>request["items"][0]["text"]=json!("Changed input"),
            "bundle"=>job["prepareBundle"]["digest"]=json!("0".repeat(64)),
            "cancelled"=>job["status"]=json!("cancelled"),
            "purpose"=>job["purpose"]=json!("discussion"),
            _=>job["preparationStages"]["first"]=json!({"status":"completed"}),
        }
        let before=d.clone();
        assert!(crate::preparation_review::reserve_first_admitted(&mut d,&token,&scheduled.job_id,&request,&crate::now()).is_err(),"{variant}");
        assert_eq!(d,before,"{variant} must not acquire paid admission");
    }
}

async fn install_new_fixture(app:&crate::App) {
    let mut d=super::tests::baw_fixture(false);
    for key in ["knowledge_entries","knowledge_versions","feedback"] {d[key]=json!([]);}
    d["runtimeLifecycle"]=app.db.read().await.unwrap()["runtimeLifecycle"].clone();
    app.db.change(|state|{*state=d;Ok(())}).await.unwrap();
}

#[tokio::test]
async fn drain_winning_writer_race_prevents_new_first_paid_callback() {
    let (app,_temp)=super::first_capture_recovery_tests::baw_app().await;install_new_fixture(&app).await;
    let token=app.lifecycle_admission_token(AdmissionClass::Preparation).await.unwrap();
    let scheduled=app.change_preparation_schedule(|d|schedule_admitted(d,Input{item_ids:vec!["ready".into()],instruction:None},&token)).await.unwrap();
    app.change(|d|runtime_lifecycle::begin_drain(d,&token,&"b".repeat(64),"test-drain",false)).await.unwrap();
    let before=app.db.read().await.unwrap();
    let reserved=crate::preparation_review::dispatch_first_admitted(&app,&scheduled.job_id,
        scheduled.request.clone().unwrap(),&token,preflight).await;
    assert!(reserved.is_err());
    let native=app.lifecycle_work.snapshot().unwrap();
    assert_eq!(native.active,0);assert_eq!(native.unresolved,0,"writer rejection clears only the unstarted ticket");
    assert_eq!(app.db.read().await.unwrap(),before,"drain-first rejection rolls back whole scoped write");
    app.db.close().await;
}

#[tokio::test]
async fn native_close_before_first_writer_prevents_durable_reservation() {
    let (app,_temp)=super::first_capture_recovery_tests::baw_app().await;install_new_fixture(&app).await;
    let token=app.lifecycle_admission_token(AdmissionClass::Preparation).await.unwrap();
    let scheduled=app.change_preparation_schedule(|d|schedule_admitted(d,Input{item_ids:vec!["ready".into()],instruction:None},&token)).await.unwrap();
    app.lifecycle_work.close().unwrap();let before=app.db.read().await.unwrap();
    assert!(crate::preparation_review::dispatch_first_admitted(&app,&scheduled.job_id,
        scheduled.request.clone().unwrap(),&token,preflight).await.is_err());
    assert_eq!(app.db.read().await.unwrap(),before);
    let native=app.lifecycle_work.snapshot().unwrap();assert_eq!(native.active,0);assert_eq!(native.unresolved,0);
    app.db.close().await;
}

fn install_mock_paid_bridge(app:&mut crate::App,root:&std::path::Path){
    app.node=std::env::var_os("COMMUNITYHERO_TEST_NODE").map(std::path::PathBuf::from).unwrap_or_else(||"node".into());
    app.bridge=root.join("native-admitted-first.mjs");
    let source=r#"import {materialInvocation} from __MATERIAL_MODULE__;
let input='';for await(const part of process.stdin)input+=part;
const request=JSON.parse(input);if(request.operation!=='assistant')throw new Error('Unexpected mock operation');
if(request.postContextBundle.members.some(m=>m.assets.some(a=>a.modality==='photo')))throw new Error('Unexpected fixture image');
const result=__RESULT__;result.mockPaidPass=true;
const invocation=materialInvocation({payload:request,input},{manifest:[]},{instructions:'Synthetic BAW lifecycle fixture',schema:'{}',cliSha256:result.runMetadata.cliSha256,stdin:input});
Object.assign(result.runMetadata,{inputSha256:invocation.actualTextInputSha256,instructionSha256:invocation.instructionSha256,materialInvocation:invocation,visualNeedContract:request.visualNeedContract,visualSelection:request.visualSelection});
process.stdout.write(JSON.stringify({ok:true,result}));"#
        .replace("__MATERIAL_MODULE__",&json!(format!("file:///{}",std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../adapters/assistant-materials.mjs").to_string_lossy().replace('\\',"/"))).to_string())
        .replace("__RESULT__",&super::tests::lifecycle_first_fixture().to_string());
    std::fs::write(&app.bridge,source).unwrap();
}

#[tokio::test]
async fn native_ticket_admitted_before_close_is_carried_through_mock_dispatch() {
    let (mut app,temp)=super::first_capture_recovery_tests::baw_app().await;install_new_fixture(&app).await;
    let token=app.lifecycle_admission_token(AdmissionClass::Preparation).await.unwrap();
    let scheduled=app.change_preparation_schedule(|d|schedule_admitted(d,Input{item_ids:vec!["ready".into()],instruction:None},&token)).await.unwrap();
    install_mock_paid_bridge(&mut app,temp.path());
    let output=crate::runtime_lifecycle_app::with_job(scheduled.job_id.clone(),crate::preparation_review::dispatch_first_admitted(&app,&scheduled.job_id,
        scheduled.request.clone().unwrap(),&token,|d,run|{
            assert_eq!(app.lifecycle_work.snapshot()?.active,1,"native ticket exists before the reserve writer");
            app.lifecycle_work.close()?;preflight(d,run)
        })).await.unwrap();
    assert_eq!(output["mockPaidPass"],true);
    assert_eq!(output["modelMaterialReceipt"]["nativeJobId"],scheduled.job_id);
    let native=app.lifecycle_work.snapshot().unwrap();
    assert!(native.closed);assert_eq!(native.active,0);assert_eq!(native.unresolved,0);
    let saved=app.db.read().await.unwrap();
    let job=crate::row(&saved,"jobs",&scheduled.job_id).unwrap();
    assert_eq!(job["preparationStages"]["firstAdmission"]["status"],"reserved");
    assert_eq!(job["retainedEvidence"].as_array().unwrap().len(),1);
    assert_eq!(job["modelMaterialReceipts"].as_array().unwrap().len(),1);
    assert!(app.lifecycle_work.begin(crate::runtime_owned_work::Kind::Preparation).is_err(),"fresh work remains closed");
    app.db.close().await;
}

#[tokio::test]
async fn admitted_first_output_settles_immutably_while_draining() {
    let (mut app,temp)=super::first_capture_recovery_tests::baw_app().await;install_new_fixture(&app).await;
    let token=app.lifecycle_admission_token(AdmissionClass::Preparation).await.unwrap();
    let scheduled=app.change_preparation_schedule(|d|schedule_admitted(d,Input{item_ids:vec!["ready".into()],instruction:None},&token)).await.unwrap();
    let request=scheduled.request.unwrap();
    install_mock_paid_bridge(&mut app,temp.path());
    let result=crate::runtime_lifecycle_app::with_job(scheduled.job_id.clone(),crate::preparation_review::dispatch_first_admitted(
        &app,&scheduled.job_id,request.clone(),&token,preflight)).await.unwrap();
    let admitted=app.db.read().await.unwrap();
    let admission=crate::row(&admitted,"jobs",&scheduled.job_id).unwrap()["preparationStages"]["firstAdmission"].clone();
    app.change(|d|runtime_lifecycle::begin_drain(d,&token,&"b".repeat(64),"test-drain",false)).await.unwrap();
    app.change_preparation_first(&scheduled.job_id,|d|crate::preparation_review::settle_first(d,&scheduled.job_id,&request,&result,&crate::now())).await.unwrap();
    let saved=app.db.read().await.unwrap();let job=crate::row(&saved,"jobs",&scheduled.job_id).unwrap();
    assert_eq!(saved["runtimeLifecycle"]["phase"],"draining");
    assert_eq!(job["preparationStages"]["first"]["status"],"completed");
    assert_eq!(job["preparationStages"]["firstAdmission"],admission);
    assert_eq!(job["preparationStages"]["first"]["result"]["proposals"],result["proposals"]);
    assert_eq!(job["retainedEvidence"],crate::row(&admitted,"jobs",&scheduled.job_id).unwrap()["retainedEvidence"]);
    assert_eq!(job["modelMaterialReceipts"],crate::row(&admitted,"jobs",&scheduled.job_id).unwrap()["modelMaterialReceipts"]);
    assert!(app.lifecycle_admission_token(AdmissionClass::Preparation).await.is_err());
    app.db.close().await;
}
