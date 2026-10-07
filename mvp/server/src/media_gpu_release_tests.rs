use super::*;
use crate::media_processing::full::{finish_vision_bridge,gpu_outcome::Outcome};

#[tokio::test]
async fn media_gpu_terminal_cloud_failure_releases_file_but_preserves_failure(){
    let temp=tempfile::tempdir().unwrap();let path=temp.path().join("gate");
    std::fs::write(&path,b"{\"version\":1,\"status\":\"clean\"}").unwrap();
    let gate=Lease::acquire_path(&path,"media_vision_chunk","fixture","",Duration::ZERO).await.unwrap();
    let error=crate::internal("Adapter failed (MEDIA_VISION_CODEX_FAILED)");
    let expected=format!("visual_backend_failed: {}",error.1);
    let result=finish_vision_bridge(Some(gate),Err(error),Outcome::UnusedAfterClosedCodex);
    assert_eq!(result.unwrap_err(),expected);
    let marker:Value=serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();assert!(clean(&marker));
    Lease::acquire_path(&path,"next","fixture","",Duration::ZERO).await.unwrap().finish().unwrap();
}
#[tokio::test]
async fn media_gpu_unknown_timeout_and_cancel_stay_dirty_and_block_next_owner(){
    for reason in ["Adapter timed out; action outcome may be unknown","Adapter failed (CANCELLED)","Adapter failed (MEDIA_VISION_TIMEOUT)","Adapter failed (MEDIA_VISION_CODEX_FAILED)"]{
        let temp=tempfile::tempdir().unwrap();let path=temp.path().join("gate");std::fs::write(&path,b"{\"version\":1,\"status\":\"clean\"}").unwrap();
        let gate=Lease::acquire_path(&path,"media_vision_chunk","fixture","",Duration::ZERO).await.unwrap();
        assert_eq!(finish_vision_bridge(Some(gate),Err(crate::internal(reason)),Outcome::Unknown).unwrap_err(),format!("visual_backend_failed: {reason}"));
        assert!(!clean(&serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap()));
        assert!(matches!(Lease::acquire_path(&path,"next","fixture","",Duration::ZERO).await,Err(e) if e=="gpu_gate_dirty_or_invalid"));
    }
}
#[tokio::test]
async fn media_gpu_success_and_absent_gate_keep_existing_result_semantics(){
    let temp=tempfile::tempdir().unwrap();let path=temp.path().join("gate");std::fs::write(&path,b"{\"version\":1,\"status\":\"clean\"}").unwrap();
    let gate=Lease::acquire_path(&path,"media_vision_chunk","fixture","",Duration::ZERO).await.unwrap();let output=json!({"frames":[]});
    assert_eq!(finish_vision_bridge(Some(gate),Ok(output.clone()),Outcome::Unknown).unwrap(),output);
    assert!(clean(&serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap()));
    assert_eq!(finish_vision_bridge(None,Err(crate::internal("same error")),Outcome::UnusedAfterClosedCodex).unwrap_err(),"visual_backend_failed: same error");
}
