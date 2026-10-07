use super::*;
const AT:&str="2026-10-04T08:00:00Z";
fn fixture()->Value{
    let mut d=crate::empty();crate::accounts::initialize(&mut d,crate::accounts::Profile::BawRussia).unwrap();
    let binding=crate::active_binding(&d).unwrap().to_json();
    d["posts"]=json!([{"id":"two-videos","postKey":"vk:two-videos","account":"BAW Russia","connectorBinding":binding,
        "title":"Exact text","attachments":[{"type":"video","url":"https://example.invalid/a"},{"type":"photo","url":"https://example.invalid/photo"},{"type":"video","url":"https://example.invalid/b"}]}]);d
}
#[test]
fn exact_asset_pin_has_distinct_indices_and_cannot_retarget_by_url_or_binding(){
    let d=fixture();let a=capture(&d,&d["posts"][0],0).unwrap();let b=capture(&d,&d["posts"][0],2).unwrap();
    assert_ne!(a,b);assert!(current(&d,&a).is_ok());assert!(current(&d,&b).is_ok());
    for change in ["reorder","replace","text","binding","company","foreign-index","extra"]{
        let mut next=d.clone();let mut pin=b.clone();
        match change{
            "reorder"=>next["posts"][0]["attachments"].as_array_mut().unwrap().swap(0,2),
            "replace"=>next["posts"][0]["attachments"][2]["url"]=json!("https://example.invalid/new"),
            "text"=>next["posts"][0]["title"]=json!("new version"),
            "binding"=>pin["connectorBinding"]["revision"]=json!(999),
            "company"=>pin["companyId"]=json!("LikeAvto"),
            "foreign-index"=>pin["attachmentIndex"]=json!(1),
            "extra"=>pin["override"]=json!(true),_=>unreachable!(),
        }assert!(current(&next,&pin).is_err(),"{change}");
    }
    let projection=json!({"account":"BAW Russia","postKey":"vk:two-videos","assetPin":a,"sourceUrl":"https://example.invalid/a"});
    assert!(require_projection(&b,&projection).is_err(),"Same post is not an asset selection");
}
#[test]
fn native_checkpoint_cannot_remove_add_or_retarget_an_asset_pin(){
    let d=fixture();let pin=capture(&d,&d["posts"][0],0).unwrap();
    let mut progress=crate::media_fullframes::initial("BAW Russia",&pin["connectorBinding"],&d["posts"][0],AT);
    progress["assetPin"]=pin.clone();crate::media_fullframes::claim(&mut progress,"asset-lease").unwrap();
    let original=json!({"id":"source-a","status":"running","result":{"visualProgress":progress}});
    for change in ["removed","null","sibling"]{
        let mut job=original.clone();let mut next=progress.clone();
        match change{"removed"=>{next.as_object_mut().unwrap().remove("assetPin");},"null"=>next["assetPin"]=Value::Null,
            _=>next["assetPin"]=capture(&d,&d["posts"][0],2).unwrap()};
        assert!(crate::media_fullframes::checkpoint(&mut job,"asset-lease",&progress,next).is_err(),"{change}");assert_eq!(job,original);
    }
    let mut legacy=progress.clone();legacy.as_object_mut().unwrap().remove("assetPin");
    let mut job=json!({"status":"running","result":{"visualProgress":legacy}});let mut retarget=legacy.clone();retarget["assetPin"]=pin;
    assert!(crate::media_fullframes::checkpoint(&mut job,"asset-lease",&legacy,retarget).is_err(),"An existing legacy cursor is not retargeted into a new asset admission");
}
#[test]
fn legacy_post_transcript_cannot_cover_two_assets_and_attempts_do_not_loop(){
    let mut d=fixture();let version=crate::media_fullframes::source_version(&d["posts"][0],"BAW Russia");
    d["materials"]=json!([{"id":"old-post-speech","kind":"transcript","account":"BAW Russia","postKey":"vk:two-videos","title":"Speech of one video","text":"Words of the first video only",
        "transcription":{"sourceVersion":version,"sourcePostKey":"vk:two-videos","partial":false,"coverage":"full_audio","audioStatus":"transcribed","mediaDurationSeconds":1.0,"audioDurationSeconds":1.0}}]);
    crate::knowledge::sync_catalog(&mut d,AT).unwrap();
    assert!(!all_ready(&d,&d["posts"][0],AT).unwrap());
    let a=next_unattempted(&d,&d["posts"][0],AT).unwrap().unwrap();assert_eq!(a["attachmentIndex"],0);
    crate::list_mut(&mut d,"jobs").push(json!({"id":"failed-a","kind":"media","status":"failed","videoSpeechAssetPin":a}));
    let b=next_unattempted(&d,&d["posts"][0],AT).unwrap().unwrap();assert_eq!(b["attachmentIndex"],2);
    crate::list_mut(&mut d,"jobs").push(json!({"id":"unknown-b","kind":"media_audio","status":"unknown","audioPin":{"progress":{"assetPin":b}}}));
    let states=outcomes(&d,&d["posts"][0],AT).unwrap();assert_eq!(states[0]["status"],"failed");assert_eq!(states[1]["status"],"unknown");
    assert!(states.iter().all(|s|s["speech"].is_null()),"Failure/UNKNOWN is not no_speech or no_audio");
    assert!(next_unattempted(&d,&d["posts"][0],AT).unwrap().is_none(),"FAILED/UNKNOWN do not imply automatic retry authority");
}
#[test]
fn same_retained_file_has_two_current_aliases_and_each_must_be_admitted(){
    for outcome in ["transcript","no_audio","no_speech"]{
        let (mut d,mut progress,old_receipt,ledger,_)=crate::media_analysis_reuse::tests::fixture_for_outcome(outcome);
        d["posts"][1]["attachments"].as_array_mut().unwrap().push(json!({"type":"video","url":"https://example.test/second.mp4"}));
        progress["sourceVersion"]=json!(crate::media_fullframes::source_version(&d["posts"][1],"LikeAvto"));
        assert!(!all_ready(&d,&d["posts"][1],AT).unwrap());
        for index in [0,1]{
            let asset=capture(&d,&d["posts"][1],index).unwrap();progress["assetPin"]=asset.clone();
            let request=json!({"verifiedFile":old_receipt["verifiedFile"],"originalAlias":{"assetPin":asset}});
            let receipt=crate::media_analysis_reuse::receipt_for_request(&d,&progress,&request).unwrap();
            let (_,pin)=crate::media_analysis_reuse::select_verified(&d,&progress,&ledger,&receipt,&"d".repeat(64)).unwrap().unwrap();
            crate::media_analysis_reuse::warm_pin(&d,&pin).unwrap();
            crate::media_analysis_reuse::admit(&mut d,&progress,&ledger,&receipt,&pin,AT).unwrap();
            assert!(ready(&d,&asset,AT).unwrap());
            assert_eq!(all_ready(&d,&d["posts"][1],AT).unwrap(),index==1,"First alias is insufficient for the second attachment");
            let assets=outcomes(&d,&d["posts"][1],AT).unwrap();assert_eq!(assets[index]["speech"]["outcome"],outcome);
            assert_eq!(crate::prepare_bundle::EvidenceContext::new(&d).decision_video_evidence(&d["posts"][1]).unwrap()["audioReady"],index==1);
        }
        assert_eq!(crate::media_analysis::ledger_from_workspace(&d).unwrap()["analyses"].as_array().unwrap().len(),1,"Aliases do not reserve a second ASR analysis");
    }
}
