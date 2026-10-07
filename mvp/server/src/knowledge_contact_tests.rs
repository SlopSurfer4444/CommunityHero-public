//! Pure native catalog fixtures; no URL fetch, model call or real company data.
use super::*;
const AT:&str="2026-10-07T10:00:00Z";
const LATER:&str="2026-10-07T11:00:00Z";
fn fixture()->Value{
    let mut d=crate::empty();crate::accounts::initialize(&mut d,crate::accounts::Profile::BawRussia).unwrap();
    d["materials"]=json!([{"id":"contact-fixture","title":"Contact fact","text":"Verified dealer contact exists.",
        "sourceUrl":"https://dealer.example/old-contact","postKey":"baw:contact-post","kind":"reference","revision":1}]);
    d["posts"]=json!([{"id":"contact-post","postKey":"baw:contact-post","title":"Contact question"},
        {"id":"unrelated-post","postKey":"baw:other-post","title":"Unrelated question"}]);
    sync_catalog(&mut d,AT).unwrap();
    let entry=d["knowledge_entries"][0].clone();
    revise(&mut d,text(&entry,"id"),&json!({"expectedVersionId":entry["currentVersionId"],"status":"active","trust":"verified","provenance":"Fixture operator verified the actual contact fact."}),AT).unwrap();d
}
fn availability(url:&str,status:&str,code:Value)->Value{
    json!({"schemaVersion":1,"sourceUrl":url,"status":status,"basis":"operator_reported","checkedAt":AT,
        "reasonCode":match status{"available"=>"public_locator_available","unavailable"=>"public_locator_unavailable",_=>"public_locator_unverified"},
        "httpStatus":code,"implication":"locator_only"})
}
fn request(d:&Value)->Value{
    json!({"expectedVersionId":d["knowledge_entries"][0]["currentVersionId"],"trust":"verified","status":"active",
        "text":"Verified contact moved to the new official address.","sourceUrl":"https://dealer.example/current-contact",
        "sourceAvailability":availability("https://dealer.example/current-contact","available",json!(200)),
        "provenance":"Fixture operator checked the original company contact statement independently of locator availability."})
}
fn selected(d:&Value,post:&str)->Value{select(d,&[json!({"postKey":post})],&[],LATER).unwrap()}

#[test]
fn corrected_fact_appends_immutable_version_updates_backing_material_and_only_selected_scope(){
    let mut d=fixture();let before=d.clone();let id=text(&d["knowledge_entries"][0],"id").to_owned();
    let previous=selected(&d,"baw:contact-post");let unrelated=selected(&d,"baw:other-post");let body=request(&d);
    let version=revise(&mut d,&id,&body,LATER).unwrap();
    assert_eq!(&rows(&d,"knowledge_versions")[..rows(&before,"knowledge_versions").len()],rows(&before,"knowledge_versions"));
    assert_eq!(version["scope"],before["knowledge_entries"][0]["scope"]);
    assert_eq!(version["supersedes"],before["knowledge_entries"][0]["currentVersionId"]);
    assert_eq!(d["materials"][0]["id"],before["materials"][0]["id"]);assert_eq!(d["materials"][0]["revision"],2);
    assert_eq!(version["sourceHash"],hash(&content(&d["materials"][0])));
    assert_eq!(d["materials"][0]["text"],body["text"]);assert_eq!(d["materials"][0]["sourceUrl"],body["sourceUrl"]);
    let current=selected(&d,"baw:contact-post");assert_ne!(hash(&current),hash(&previous));assert_eq!(selected(&d,"baw:other-post"),unrelated);
    assert_eq!(current["materials"][0]["sourceAvailability"],body["sourceAvailability"]);
    assert_eq!(current["manifest"][0]["sourceAvailability"],body["sourceAvailability"]);
    for key in ["items","jobs","proposals","approvals","operations","posts","branches"]{assert_eq!(d[key],before[key],"{key}");}
    let count=rows(&d,"knowledge_versions").len();sync_catalog(&mut d,LATER).unwrap();assert_eq!(rows(&d,"knowledge_versions").len(),count,"matching backing hash cannot append a downgrade on ordinary sync");
    let changed=d.clone();assert!(revise(&mut d,&id,&body,LATER).is_err());assert_eq!(d,changed,"stale CAS cannot overwrite the new head");
}

#[test]
fn unavailable_or_unknown_locator_never_erases_verified_contact_or_claims_a_negative_fact(){
    for (status,code) in [("unavailable",json!(404)),("unavailable",json!(503)),("unknown",Value::Null)]{
        let mut d=fixture();let id=text(&d["knowledge_entries"][0],"id").to_owned();let old_text=d["materials"][0]["text"].clone();
        let body=json!({"expectedVersionId":d["knowledge_entries"][0]["currentVersionId"],"trust":"verified","status":"active",
            "sourceAvailability":availability("https://dealer.example/old-contact",status,code),
            "provenance":"Fixture operator records locator availability; existing independently verified contact fact is retained."});
        let version=revise(&mut d,&id,&body,LATER).unwrap();assert_eq!(version["text"],old_text);
        let bundle=selected(&d,"baw:contact-post");assert_eq!(bundle["materials"][0]["text"],old_text);
        assert_eq!(bundle["materials"][0]["sourceAvailability"]["status"],status);
        assert_eq!(version["factCorrection"]["generationDispatched"],false);assert_eq!(version["factCorrection"]["externalActions"],0);
    }
}

#[test]
fn malformed_unverified_foreign_stale_or_policy_corrections_fail_atomically(){
    let base=fixture();let id=text(&base["knowledge_entries"][0],"id").to_owned();let valid=request(&base);
    let mut invalid=Vec::new();
    for key in ["trust","provenance"]{let mut body=valid.clone();body.as_object_mut().unwrap().remove(key);invalid.push(body);}
    for (key,value) in [("scope",json!({"account":"foreign"})),("restoreVersionId",base["knowledge_entries"][0]["currentVersionId"].clone()),("text",json!(" ")),("sourceUrl",json!("https://user:secret@dealer.example/contact")),("sourceUrl",json!("http://dealer.example/contact"))] {
        let mut body=valid.clone();body[key]=value;invalid.push(body);
    }
    for (key,value) in [("sourceUrl",json!("https://dealer.example/foreign")),("basis",json!("model_inferred")),("implication",json!("dealer_absent")),("httpStatus",json!(404)),("checkedAt",json!("2027-01-01T00:00:00Z")),("unexpected",json!(true))] {
        let mut body=valid.clone();body["sourceAvailability"][key]=value;invalid.push(body);
    }
    for body in invalid{let mut d=base.clone();assert!(revise(&mut d,&id,&body,LATER).is_err(),"{body}");assert_eq!(d,base);}
    let mut source_changed=base.clone();source_changed["materials"][0]["text"]=json!("Unreviewed backing mutation");let before=source_changed.clone();
    assert!(revise(&mut source_changed,&id,&valid,LATER).is_err());assert_eq!(source_changed,before);
    let mut foreign=base.clone();foreign["materials"][0]["account"]=json!("foreign");let before=foreign.clone();
    assert!(revise(&mut foreign,&id,&valid,LATER).is_err());assert_eq!(foreign,before);
    let mut policy=base.clone();let head=policy["knowledge_versions"].as_array_mut().unwrap().last_mut().unwrap();head["kind"]=json!("rule");
    let digest=version_hash(head);head["hash"]=json!(digest);head["id"]=json!(format!("knowledge-version-{digest}"));let new_id=head["id"].clone();
    policy["knowledge_entries"][0]["currentVersionId"]=new_id.clone();policy["knowledge_entries"][0]["kind"]=json!("rule");
    let mut body=valid;body["expectedVersionId"]=new_id;let before=policy.clone();assert!(revise(&mut policy,&id,&body,LATER).is_err());assert_eq!(policy,before);
}

#[test]
fn changed_locator_clears_previous_availability_and_catalog_rejects_tampered_report(){
    let mut d=fixture();let id=text(&d["knowledge_entries"][0],"id").to_owned();let body=request(&d);revise(&mut d,&id,&body,LATER).unwrap();
    let body=json!({"expectedVersionId":d["knowledge_entries"][0]["currentVersionId"],"trust":"verified","sourceUrl":"https://dealer.example/next-contact",
        "provenance":"Fixture operator verified a changed locator without claiming it was fetched by CommunityHero."});
    let version=revise(&mut d,&id,&body,"2026-10-07T12:00:00Z").unwrap();assert!(version.get("sourceAvailability").is_none());assert!(d["materials"][0].get("sourceAvailability").is_none());
    let mut forged=d.clone();let head=forged["knowledge_versions"].as_array_mut().unwrap().last_mut().unwrap();
    head["sourceAvailability"]=availability("https://dealer.example/next-contact","unknown",Value::Null);head["sourceAvailability"]["implication"]=json!("dealer_absent");
    let digest=version_hash(head);head["hash"]=json!(digest);head["id"]=json!(format!("knowledge-version-{digest}"));let head_id=head["id"].clone();
    forged["knowledge_entries"][0]["currentVersionId"]=head_id;assert!(validate_catalog(&forged).is_err(),"a recomputed hash cannot bless a malformed availability contract");
}

#[test]
fn fact_availability_roundtrip_retains_version_source_hash_and_rejects_changed_locator_report(){
    let mut d=fixture();let id=text(&d["knowledge_entries"][0],"id").to_owned();let body=request(&d);revise(&mut d,&id,&body,LATER).unwrap();
    let stored:Value=serde_json::from_slice(&serde_json::to_vec(&d).unwrap()).unwrap();assert_eq!(stored,d);validate_catalog(&stored).unwrap();
    assert_eq!(selected(&stored,"baw:contact-post"),selected(&d,"baw:contact-post"));
    let head=rows(&stored,"knowledge_versions").last().unwrap();assert_eq!(head["sourceHash"],hash(&content(&stored["materials"][0])));
    for key in ["status","sourceUrl","checkedAt"]{
        let mut changed=stored.clone();let version=changed["knowledge_versions"].as_array_mut().unwrap().last_mut().unwrap();version["sourceAvailability"][key]=json!("tampered");
        assert!(validate_catalog(&changed).is_err(),"{key}");
    }
}
