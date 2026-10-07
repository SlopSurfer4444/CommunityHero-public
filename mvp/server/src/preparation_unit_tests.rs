use super::*;
const AT:&str="2026-09-25T00:00:00Z";
fn fixture()->Value {
    let mut d=crate::empty();crate::accounts::initialize(&mut d,crate::accounts::Profile::BawRussia).unwrap();
    d["posts"]=json!([{"id":"p","postKey":"12182:p","objectId":"12182","title":"Publication","text":"Exact post text","attachments":[]}]);
    d["branches"]=json!([{"id":"b","postId":"p","messages":[]},{"id":"c","postId":"p","messages":[]}]);
    d["items"]=json!([{"id":"a","itemId":"a","objectId":"12182","platform":"VK","postId":"p","postKey":"12182:p","conversationKey":"12182:a","branchId":"b","revision":1},
        {"id":"z","itemId":"z","objectId":"12182","platform":"VK","postId":"p","postKey":"12182:p","conversationKey":"12182:z","branchId":"c","revision":2}]);d
}
fn family()->Value {
    let mut d=crate::media_audio_equivalence::tests::fixture();
    d["items"][0]["platform"]=json!("VK");
    d["items"].as_array_mut().unwrap().push(json!({"id":"j","itemId":"j","objectId":"12185","platform":"VK","postId":"source","postKey":"12185:source","conversationKey":"12185:j","branchId":"s","revision":1}));
    d["branches"].as_array_mut().unwrap().push(json!({"id":"s","postId":"source","messages":[]}));
    let view=crate::media_audio_equivalence::preview_for_test(&d,"target",Some("source"),AT).unwrap();
    let transcript=&view["source"]["transcriptCandidates"][0];
    let body=json!({"expectedHeadSha256":view["headSha256"],"expectedTargetSourceVersion":view["targetSourceVersion"],"sourcePostId":"source",
        "expectedSourceVersion":view["source"]["sourceVersion"],"transcriptVersionId":transcript["versionId"],"transcriptHash":transcript["hash"],"reason":"Synthetic BAW owner confirmed complete speech equivalence"});
    crate::media_audio_equivalence::set(&mut d,"target",&body,&crate::operator_auth::Actor::local_owner("test"),AT,false).unwrap();d
}
#[test]fn same_post_is_one_deterministic_unit_without_mutation_or_paid_capture_cycle(){
    let d=fixture();let before=d.clone();let u=capture(&d,&[json!("z"),json!("a")],AT).unwrap();
    assert_eq!(u,capture(&d,&[json!("a"),json!("z")],AT).unwrap());assert_eq!(u["kind"],"post");
    assert_eq!(u["itemIds"],json!(["a","z"]));assert_eq!(u["copies"].as_array().unwrap().len(),1);
    let mut unhashed=u.clone();unhashed.as_object_mut().unwrap().remove("unitSha256");assert_eq!(u["unitSha256"],hash(&unhashed));
    assert!(!u.to_string().contains("paidCapture"));assert_eq!(d,before);assert!(current(&d,&u,&[json!("a"),json!("z")],AT).is_ok());
}
#[test]fn unrelated_posts_never_merge_on_title_url_or_transport_capacity(){
    let mut d=fixture();let mut other=d["posts"][0].clone();other["id"]=json!("q");other["postKey"]=json!("12182:q");
    d["posts"].as_array_mut().unwrap().push(other);d["items"][1]["postId"]=json!("q");d["branches"][1]["postId"]=json!("q");
    let before=d.clone();assert_eq!(capture(&d,&[json!("a"),json!("z")],AT).unwrap_err(),MIXED);
    assert!(capture(&d,&[json!("a")],AT).is_ok());assert!(capture(&d,&[json!("z")],AT).is_ok());assert_eq!(d,before);
}
#[test]fn exact_recipients_company_branch_and_per_copy_source_are_fenced(){
    let d=fixture();let ids=[json!("a"),json!("z")];let u=capture(&d,&ids,AT).unwrap();
    for change in ["revision","post","branch","company","binding","text","photo","url"] {
        let mut changed=d.clone();match change {
            "revision"=>changed["items"][1]["revision"]=json!(3),
            "post"=>changed["items"][1]["postId"]=json!("absent"),
            "branch"=>changed["branches"][1]["postId"]=json!("foreign"),
            "company"=>changed["items"][1]["account"]=json!("Other company"),
            "binding"=>changed["posts"][0]["connectorBinding"]=json!({"accountId":"other"}),
            "text"=>changed["posts"][0]["text"]=json!("Changed exact text"),
            "photo"=>changed["posts"][0]["attachments"]=json!([{"type":"photo","url":"https://fixture.invalid/changed.jpg"}]),
            _=>changed["posts"][0]["sourceUrl"]=json!("https://fixture.invalid/changed"),
        };assert!(current(&changed,&u,&ids,AT).is_err(),"{change}");
    }
    assert!(current(&d,&u,&[json!("a")],AT).is_err());
    for ids in [vec![],vec![json!("a"),json!("a")],vec![Value::Null],vec![json!("missing")],(0..101).map(|i|json!(format!("i{i}"))).collect()] {
        assert!(capture(&d,&ids,AT).is_err());
    }
}
#[test]fn proven_family_keeps_distinct_copies_and_exact_current_evidence(){
    let d=family();let ids=[json!("i"),json!("j")];let before=d.clone();let u=capture(&d,&ids,AT).unwrap();
    assert_eq!(u["kind"],"family");assert_eq!(u["copies"].as_array().unwrap().len(),2);
    assert_ne!(u["copies"][0]["sourceVersion"],u["copies"][1]["sourceVersion"]);
    assert_eq!(u["familyProof"].as_array().unwrap().len(),2);assert_eq!(d,before);
    for change in ["revoked","source","head","expired"] {
        let mut changed=d.clone();match change {
            "revoked"=>changed["settings"]["mediaAudioEquivalences"]["target"]["status"]=json!("revoked"),
            "source"=>changed["posts"][0]["text"]=json!("Changed copy text"),
            "head"=>changed["knowledge_entries"][0]["currentVersionId"]=json!("missing"),
            _=>changed["knowledge_versions"][0]["validUntil"]=json!(AT),
        };assert!(current(&changed,&u,&ids,AT).is_err(),"{change}");
    }
}
#[test]fn saved_request_cannot_forge_group_contract_selection_or_digest(){
    let d=fixture();let mut b=json!({"itemIds":["a","z"],"request":{"items":[d["items"][0].clone(),d["items"][1].clone()]}});
    attach(&d,&mut b,AT).unwrap();assert!(current_bundle(&d,&b,AT).is_ok());
    for change in ["digest","version","selection","unit","contract"] {
        let mut bad=b.clone();match change {
            "digest"=>bad["digest"]=json!("forged"),
            "version"=>bad["request"]["strictGroup"]["version"]=json!(2),
            "selection"=>bad["request"]["items"].as_array_mut().unwrap().pop().map(|_|()).unwrap(),
            "unit"=>bad["request"]["strictGroup"]["copies"][0]["sourceVersion"]=json!("forged"),
            _=>bad["request"]["strictGroupContract"]=json!("future_v2"),
        };if change!="digest"{bad["digest"]=json!(hash(&bad["request"]));}
        assert!(current_bundle(&d,&bad,AT).is_err(),"{change}");
    }
}
