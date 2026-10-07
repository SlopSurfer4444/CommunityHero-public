use super::*;
use serde_json::json;
fn fixture()->(Value,Value){let hash="a".repeat(64);(json!({"manifestSha256":hash}),json!({"code":"MEDIA_VISION_CODEX_FAILED","mediaGpuResource":{"version":1,"disposition":"unused_local_gpu","child":"closed_normal_exit","requestSha256":hash,"exitCode":1}}))}
#[test]
fn media_gpu_proof_requires_exact_request_operation_and_containment(){
    let (request,error)=fixture();let mut outcome=Outcome::Unknown;
    outcome.observe("media_vision_chunk",&request,&error,true);assert!(outcome.permits_release());
    outcome.observe("media_vision_chunk",&request,&error,false);assert!(!outcome.permits_release());
    for op in ["media_vision","assistant","execute"]{outcome.observe(op,&request,&error,true);assert!(!outcome.permits_release());}
    for hash in ["b".repeat(64),"A".repeat(64),"private".into()]{outcome.observe("media_vision_chunk",&json!({"manifestSha256":hash}),&error,true);assert!(!outcome.permits_release());}
}
#[test]
fn media_gpu_proof_rejects_timeout_kill_local_request_and_malformed_evidence(){
    let (request,error)=fixture();
    for code in ["ADAPTER_TIMEOUT","CANCELLED","MEDIA_VISION_TIMEOUT","MEDIA_VISION_OUTPUT_INVALID"]{
        let mut bad=error.clone();bad["code"]=json!(code);let mut outcome=Outcome::Unknown;outcome.observe("media_vision_chunk",&request,&bad,true);assert!(!outcome.permits_release());
    }
    for (field,value) in [("version",json!(2)),("version",json!("1")),("disposition",json!("local_request_unknown")),("child",json!("killed")),("exitCode",json!(0)),("exitCode",json!(-1)),("exitCode",json!(256)),("exitCode",json!("1")),("signal",json!("SIGTERM"))]{
        let mut bad=error.clone();bad["mediaGpuResource"][field]=value;let mut outcome=Outcome::Unknown;outcome.observe("media_vision_chunk",&request,&bad,true);assert!(!outcome.permits_release());
    }
    for key in ["version","disposition","child","requestSha256","exitCode"]{let mut bad=error.clone();bad["mediaGpuResource"].as_object_mut().unwrap().remove(key);let mut outcome=Outcome::Unknown;outcome.observe("media_vision_chunk",&request,&bad,true);assert!(!outcome.permits_release());}
}

#[cfg(windows)]
#[tokio::test]
async fn media_gpu_actual_bridge_keeps_error_and_only_observes_contained_valid_response(){
    let (mut app,temp)=crate::tests::test_app().await;
    app.node=std::path::PathBuf::from("C:/Users/hello/AppData/Local/Microsoft/WinGet/Packages/OpenJS.NodeJS.LTS_Microsoft.Winget.Source_8wekyb3d8bbwe/node-v24.15.0-win-x64/node.exe");
    app.bridge=temp.path().join("isolated-media-resource.mjs");
    let (request,error)=fixture();
    let script=format!("let input='';for await(const chunk of process.stdin)input+=chunk;process.stdout.write(JSON.stringify({}));",json!({"ok":false,"error":error}));
    std::fs::write(&app.bridge,script).unwrap();
    let ordinary=app.bridge("media_vision_chunk",request.clone()).await.unwrap_err();
    let mut outcome=Outcome::Unknown;
    let observed=app.bridge_observed("media_vision_chunk",request.clone(),Some(&mut outcome)).await.unwrap_err();
    assert_eq!(observed.0,ordinary.0);assert_eq!(observed.1,ordinary.1);assert!(outcome.permits_release());
    assert_eq!(observed.1,"Adapter failed (MEDIA_VISION_CODEX_FAILED)");
    std::fs::write(&app.bridge,"let input='';for await(const chunk of process.stdin)input+=chunk;process.stdout.write('invalid');").unwrap();
    assert!(app.bridge_observed("media_vision_chunk",request.clone(),Some(&mut outcome)).await.is_err());assert!(!outcome.permits_release());
    std::fs::write(&app.bridge,"let input='';for await(const chunk of process.stdin)input+=chunk;process.exitCode=1;").unwrap();
    outcome=Outcome::UnusedAfterClosedCodex;
    assert!(app.bridge_observed("media_vision_chunk",request,Some(&mut outcome)).await.is_err());assert!(!outcome.permits_release());
}
