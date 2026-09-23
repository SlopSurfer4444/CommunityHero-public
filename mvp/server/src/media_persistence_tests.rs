//! Explicit offline acceptance using an existing real-ASR result and a cloned PG database.
use super::*;
use sha2::{Digest, Sha256};

fn protected_texts(d: &Value) -> Value {
    json!({"items":list(d,"items").iter().map(|i|json!([i["id"],i["draft"],i["draftEdited"]])).collect::<Vec<_>>(),
        "proposals":list(d,"proposals").iter().map(|p|json!([p["id"],p["text"]])).collect::<Vec<_>>(),
        "approvals":d["approvals"],"operations":d["operations"]})
}

#[tokio::test]
#[ignore = "requires explicit real-ASR result and isolated PostgreSQL clone"]
async fn actual_asr_survives_pg_restart_and_reuses_twin() {
    let file=std::env::var("COMMUNITYHERO_MEDIA_TEST_DB_URL_FILE").expect("explicit test DB URL file");
    let url=std::fs::read_to_string(file).unwrap();
    let url=url.trim();
    assert!(url.starts_with("postgresql://ch_migrate:"));
    assert!(url.ends_with("@127.0.0.1:55439/communityhero_media_test_20260923"),"only the isolated media clone is allowed");
    let result_path=std::env::var("COMMUNITYHERO_MEDIA_ACCEPTANCE_OUTPUT").expect("explicit real ASR output");
    let bytes=std::fs::read(&result_path).unwrap();
    let result:Value=serde_json::from_slice(&bytes).unwrap();
    let transcript=result["materials"][0].clone();
    assert_eq!(transcript["account"],"LikeAvto");
    assert_eq!(transcript["kind"],"transcript");
    assert!(transcript["text"].as_str().unwrap().chars().count()>30);
    let post_key=transcript["postKey"].as_str().unwrap();
    assert_eq!(post_key,"11390:media-acceptance-20260923");
    let title="CommunityHero native-media acceptance fixture 20260923";
    let original=json!({"id":"post-media-acceptance-20260923","postKey":post_key,"title":title,"sourceUrl":transcript["sourceUrl"],"objectId":"11390","channel":"YouTube","attachments":[{"type":"video"}]});
    let twin=json!({"id":"post-media-twin-acceptance-20260923","postKey":"11391:media-twin-acceptance-20260923","title":title,"sourceUrl":"https://www.instagram.com/reel/mediaAcceptance20260923/","objectId":"11391","channel":"Instagram","attachments":[{"type":"video"}]});
    let db=Database::postgres(url).await.unwrap();
    let before=db.read().await.unwrap();
    let protected=protected_texts(&before);
    assert!(!list(&before,"posts").iter().any(|p|p["id"]==original["id"]),"fresh acceptance clone required");
    db.change(|d| {
        list_mut(d,"posts").extend([original.clone(),twin.clone()]);
        merge_materials(d,&result)?;
        assert!(knowledge::post_has_transcript(d,&original,&now()).unwrap());
        assert!(knowledge::post_has_transcript(d,&twin,&now()).unwrap());
        Ok(())
    }).await.unwrap();
    let committed=db.read().await.unwrap();
    assert_eq!(protected_texts(&committed),protected,"operator texts/approvals/operations changed");
    let material_id=format!("import-{}",transcript["id"].as_str().unwrap());
    let stored=row(&committed,"materials",&material_id).unwrap().clone();
    assert_eq!(stored["text"],transcript["text"]);
    assert_eq!(stored["transcription"],transcript["transcription"]);
    let versions=committed["knowledge_versions"].clone();
    db.close().await;
    let reopened=Database::postgres(url).await.unwrap();
    let readback=reopened.read().await.unwrap();
    assert_eq!(row(&readback,"materials",&material_id).unwrap(),&stored);
    assert_eq!(readback["knowledge_versions"],versions);
    assert!(knowledge::post_has_transcript(&readback,&twin,&now()).unwrap());
    let (_,changed)=reopened.change_observed(|d|merge_materials(d,&result)).await.unwrap();
    assert!(!changed,"identical real ASR admission was not idempotent");
    let receipt=json!({"database":"communityhero_media_test_20260923","realAsrResultSha256":format!("{:x}",Sha256::digest(&bytes)),"materialId":material_id,"textChars":transcript["text"].as_str().unwrap().chars().count(),"restartReadback":true,"twinReuse":true,"replayUnchanged":true,"operatorTextsPreserved":true,"externalWrites":false});
    let receipt_path=std::env::var("COMMUNITYHERO_MEDIA_PERSISTENCE_RECEIPT").expect("explicit receipt output");
    std::fs::write(receipt_path,serde_json::to_vec_pretty(&receipt).unwrap()).unwrap();
    reopened.close().await;
}
