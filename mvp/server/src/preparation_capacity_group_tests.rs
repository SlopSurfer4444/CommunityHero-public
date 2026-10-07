use super::*;
fn fixture()->Value {
    let mut d=crate::engine_prepare::tests::baw_fixture(false);
    d["items"]=json!([d["items"][0].clone()]);d["branches"]=json!([d["branches"][0].clone()]);d["posts"]=json!([d["posts"][0].clone()]);
    for n in 1..4 {let mut item=d["items"][0].clone();item["id"]=json!(format!("i{n}"));item["itemId"]=item["id"].clone();d["items"].as_array_mut().unwrap().push(item);}d
}
#[tokio::test]async fn every_adapter_split_retains_an_exact_native_strict_group(){
    let d=fixture();let before=d.clone();let ids=json!(["ready","i1","i2","i3"]);
    let plan=json!({"strictGroupContract":crate::preparation_unit::CONTRACT,"batches":[{"itemIds":ids}],"held":[]});
    let refined=refine_with(&d,plan,None,|requests|async move{Ok(requests.into_iter().map(|r|
        if r["items"].as_array().unwrap().len()>1{Size::Oversized}else{Size::Fits(100)}).collect())}).await.unwrap();
    assert_eq!(refined["batches"].as_array().unwrap().len(),4);
    for batch in refined["batches"].as_array().unwrap(){
        crate::preparation_unit::current(&d,&batch["strictGroup"],batch["itemIds"].as_array().unwrap(),&crate::now()).unwrap();
        assert_eq!(batch["strictGroup"]["version"],1);assert_eq!(batch["strictGroup"]["copies"].as_array().unwrap().len(),1);
    }
    assert_eq!(d,before);assert!(crate::list(&d,"jobs").is_empty());
}
#[tokio::test]async fn an_impossible_common_photo_base_is_held_once_without_adapter_or_recipient_split(){
    let mut d=fixture();d["posts"][0]["attachments"]=json!((0..17).map(|i|json!({"type":"photo","url":format!("https://fixture.invalid/{i}.png")})).collect::<Vec<_>>());
    let before=d.clone();let plan=json!({"batches":[{"itemIds":["ready","i1","i2","i3"]}],"held":[]});
    let refined=refine_with(&d,plan,None,|_|async{panic!("impossible mandatory source base must not invoke an adapter");#[allow(unreachable_code)]Ok(Vec::new())}).await.unwrap();
    assert_eq!(refined["batches"],json!([]));assert_eq!(refined["held"].as_array().unwrap().len(),4);
    assert!(refined["held"].as_array().unwrap().iter().all(|h|h["reason"]=="mandatory_post_material_capacity_exceeded"));
    assert_eq!(d,before);assert_eq!(d["posts"][0]["attachments"].as_array().unwrap().len(),17);
}
#[tokio::test]async fn family_capacity_splits_whole_copies_before_any_recipient_tail(){
    let at=crate::now();let mut d=crate::media_audio_equivalence::tests::fixture();
    let mut other=d["items"][0].clone();other["id"]=json!("j");other["itemId"]=json!("j");other["objectId"]=json!("12185");
    other["postId"]=json!("source");other["postKey"]=json!("12185:source");other["branchId"]=json!("s");
    let mut same=d["items"][0].clone();same["id"]=json!("k");same["itemId"]=json!("k");d["items"].as_array_mut().unwrap().extend([other,same]);
    d["branches"].as_array_mut().unwrap().push(json!({"id":"s","postId":"source","messages":[]}));
    let view=crate::media_audio_equivalence::preview_for_test(&d,"target",Some("source"),&at).unwrap();let candidate=&view["source"]["transcriptCandidates"][0];
    let body=json!({"expectedHeadSha256":view["headSha256"],"expectedTargetSourceVersion":view["targetSourceVersion"],"sourcePostId":"source",
        "expectedSourceVersion":view["source"]["sourceVersion"],"transcriptVersionId":candidate["versionId"],"transcriptHash":candidate["hash"],"reason":"Synthetic BAW same complete speech"});
    crate::media_audio_equivalence::set(&mut d,"target",&body,&crate::operator_auth::Actor::local_owner("test"),&at,false).unwrap();
    let before=d.clone();let plan=json!({"batches":[{"itemIds":["i","j","k"]}],"held":[]});
    let refined=refine_with(&d,plan,None,|requests|async move{Ok(requests.into_iter().map(|r|
        if r["strictGroup"]["copies"].as_array().unwrap().len()>1{Size::Oversized}else{Size::Fits(100)}).collect())}).await.unwrap();
    assert_eq!(refined["batches"].as_array().unwrap().len(),2);
    assert_eq!(refined["batches"][0]["itemIds"],json!(["i","k"]));assert_eq!(refined["batches"][1]["itemIds"],json!(["j"]));
    assert!(refined["held"].as_array().unwrap().is_empty());assert_eq!(d,before);
    for batch in refined["batches"].as_array().unwrap(){assert_eq!(batch["strictGroup"]["copies"].as_array().unwrap().len(),1);}
}
