use super::*;
const AT:&str="2026-10-01T10:00:00Z";

pub(crate) fn fixture()->(Value,operator_auth::Actor,String,Value){
    fixture_inner(false)
}
fn fixture_inner(stale_sibling:bool)->(Value,operator_auth::Actor,String,Value){
    let (mut d,mut refs)=crate::operator_editorial::tests::fixture();
    if stale_sibling {
        let mut item=d["items"][0].clone();item["id"]=json!("i1");item["itemId"]=json!("c1");
        list_mut(&mut d,"items").push(item.clone());
        let p=create_proposal(&mut d,&json!({"itemId":"i1","expectedRevision":item["revision"],"kind":"reply_and_close","text":"Sibling old text"})).unwrap();
        refs.as_array_mut().unwrap().push(json!({"id":p["id"],"revision":p["revision"]}));
    }
    let actor=operator_auth::Actor::local_owner("editor-repair-fixture");
    let (_,job)=editorial_endpoint::schedule(&mut d,&actor,&json!({"requestId":"repair-parent","proposals":refs,"fresh":true})).unwrap();
    let job=job.unwrap();let batch=row(&d,"jobs",&job).unwrap()["editorialPlan"]["batches"][0].clone();
    if stale_sibling {d["proposals"][1]["text"]=json!("Sibling changed before dispatch");}
    let capture=editorial_endpoint::capture_dispatch(&mut d,&job,&batch).unwrap();
    let c=&capture["batch"]["request"]["editorialCandidates"][0];
    let mut result=json!({"text":"Synthetic independent fixture; no model called","sources":[],"proposals":[],
        "editorial":[{"proposalId":c["proposalId"],"proposalRevision":c["proposalRevision"],"itemId":c["itemId"],
            "textSha256":c["textSha256"],"contextDigest":c["contextDigest"],"rulesDigest":c["rulesDigest"],
            "decision":"revise","reason":"Exact text needs a contextual revision","proposedText":"Для выездов на рыбалку 🙂",
            "checks":{"companyRules":"pass","intent":"fail","factualScope":"pass"}}],
        "runMetadata":{"schemaVersion":1,"model":"gpt-6.1-sol","modelProfile":"sol61_v1","reasoningEffort":"high",
            "promptVersion":"communityhero-editorial-v1","instructionSha256":"a".repeat(64),"inputSha256":"b".repeat(64),
            "cliSha256":"86e8ef1013f98df51fdeea446597f7e3ca32e454d1d4d8c0402a68b03c311d70","elapsedMs":1,"completedAt":AT,"imageEvidence":[]}});
    crate::model_material_receipt::fixture_result(&mut d,&job,&capture["batch"]["request"],&mut result).unwrap();
    let mut admitted=editorial_review::admit(&mut d,&capture["batch"],&result,AT).unwrap();
    admitted["outcomes"].as_array_mut().unwrap().extend(capture["held"].as_array().unwrap().iter().cloned());
    let receipt=row(&d,"proposals",c["proposalId"].as_str().unwrap()).unwrap()["editorialReview"].clone();
    let expected=json!({"proposalId":c["proposalId"],"proposalRevision":c["proposalRevision"],"textSha256":c["textSha256"],
        "contextDigest":c["contextDigest"],"rulesDigest":c["rulesDigest"],"receiptSha256":receipt["receiptSha256"]});
    let mut summary=json!({"accepted":[],"reused":[],"held":[{"reference":refs[0],"decision":"revise","reason":receipt["reason"],
        "suggestedText":receipt["proposedText"],"repairExpected":expected}]});
    if stale_sibling {
        let held=&capture["held"][0];
        summary["held"].as_array_mut().unwrap().push(json!({"reference":refs[1],"decision":"hold","reason":held["reason"]}));
    }
    let j=row_mut(&mut d,"jobs",&job).unwrap();let entry=&mut j["editorialBatches"][0];
    entry["state"]=json!("settled");entry["resultDigest"]=json!(editorial_review::hash_text(&admitted.to_string()));entry["result"]=admitted;
    j["status"]=json!("completed");j["finishedAt"]=json!(AT);j["editorialOutcome"]=summary.clone();j["result"]=summary;
    (d,actor,job,json!({"requestId":"repair-once","expected":[expected]}))
}

#[test]
fn filtered_dispatch_repair_preserves_stale_sibling_and_parent_digest(){
    let(mut d,actor,job,body)=fixture_inner(true);let before=d.clone();
    let j=row(&d,"jobs",&job).unwrap();
    assert_ne!(j["editorialPlan"]["batches"][0]["digest"],j["editorialBatches"][0]["capture"]["batch"]["digest"]);
    let result=admit(&mut d,&actor,&job,&body).unwrap();assert_eq!(result["newRefs"][0]["revision"],2);
    assert_eq!(d["proposals"][1],before["proposals"][1]);assert_eq!(d["jobs"],before["jobs"]);
}

#[test]
fn exact_durable_repair_preserves_history_and_requires_new_review(){
    let(mut d,actor,job,body)=fixture();let before=d.clone();
    let repaired=admit(&mut d,&actor,&job,&body).unwrap();
    let old=&before["proposals"][0];let p=&d["proposals"][0];
    assert_eq!(p["revision"],2);assert_eq!(p["status"],"draft");assert_eq!(p["text"],old["editorialReview"]["proposedText"]);
    assert!(p["editorialReview"].is_null());assert_eq!(p["history"][0]["text"],old["text"]);
    assert_eq!(p["editorialReviews"],old["editorialReviews"]);assert_eq!(d["operations"],before["operations"]);assert_eq!(d["approvals"],before["approvals"]);
    assert!(editorial_review::require_current(&prepare_bundle::EvidenceContext::new(&d),p).is_err());
    let fresh=editorial_review::plan_fresh(&d,&repaired["newRefs"],AT).unwrap();
    assert_eq!(fresh["reused"],json!([]));assert_eq!(fresh["batches"][0]["request"]["editorialCandidates"][0]["text"],p["text"]);
    let after=d.clone();let replay=admit(&mut d,&actor,&job,&body).unwrap();assert_eq!(replay["replayed"],true);assert_eq!(replay["newRefs"],repaired["newRefs"]);assert_eq!(d,after);
    let canonical=normalized(&job,&body).unwrap();validate_delta(&before,&after,&canonical).unwrap();
}

#[test]
fn repair_rejects_stale_foreign_unsettled_and_invented_proof_without_mutation(){
    let(original,actor,job,body)=fixture();
    for change in ["revision","text","post","rules","waiting","unknown","unknown_operation","unsettled","foreign","foreign_parent","suggestion","caller_text","history"]{
        let mut d=original.clone();let mut request=body.clone();
        match change{
            "revision"=>d["proposals"][0]["revision"]=json!(9),
            "text"=>d["proposals"][0]["text"]=json!("Changed"),
            "post"=>d["posts"][0]["text"]=json!("Changed post"),
            "rules"=>{knowledge::save_instruction(&mut d,&json!({"requestId":"changed-rule","title":"Voice","text":"A new rule"}),&crate::now()).unwrap();},
            "waiting"=>d["items"][0]["workflow"]=json!("waiting"),
            "unknown"=>d["proposals"][0]["status"]=json!("unknown"),
            "unknown_operation"=>d["operations"]=json!([{"id":"prior","itemId":d["proposals"][0]["itemId"],"status":"unknown"}]),
            "unsettled"=>row_mut(&mut d,"jobs",&job).unwrap()["editorialBatches"][0]["state"]=json!("captured"),
            "foreign"=>d["account"]=json!("BAW Russia"),
            "foreign_parent"=>{let parent=row_mut(&mut d,"jobs",&job).unwrap();parent["conductorRunId"]=json!("foreign-run");parent["grantGeneration"]=json!(1);},
            "suggestion"=>d["proposals"][0]["editorialReview"]["proposedText"]=json!("An invented substitute"),
            "caller_text"=>request["text"]=json!("Caller substitute"),
            _=>d["proposals"][0]["history"]=json!({"malformed":true}),
        }
        let before=d.clone();assert!(admit(&mut d,&actor,&job,&request).is_err(),"{change}");assert_eq!(d,before,"{change}");
    }
}

#[tokio::test]
async fn repair_endpoint_lost_ack_restart_lookup_returns_one_revision(){
    let(mut app,temp)=crate::tests::test_app().await;let (mut fixture,actor,job,body)=fixture();
    let fixture_owner=crate::native_fixture_owner_repair::initialize_workspace(&mut fixture).unwrap();
    assert_eq!(&fixture_owner,app.lifecycle_owner.as_ref());
    app.change(|d|{*d=fixture;Ok(())}).await.unwrap();
    let first=post(State(app.clone()),Extension(actor.clone()),Path(job.clone()),Json(body.clone())).await.unwrap().0;
    let canonical=normalized(&job,&body).unwrap();let expected_payload=canonical.as_object().unwrap().iter().filter(|(k,_)|k.as_str()!="requestId")
        .map(|(k,v)|(k.clone(),v.clone())).collect::<serde_json::Map<_,_>>();
    let before=app.read().await.unwrap();app.db.close().await;
    app.db=Database::Sqlite(open_db(&temp.path().join("workspace.sqlite")).await.unwrap());
    let second=post(State(app.clone()),Extension(actor.clone()),Path(job.clone()),Json(body.clone())).await.unwrap().0;
    assert_eq!(first["newRefs"],second["newRefs"]);assert_eq!(second["replayed"],true);assert_eq!(app.read().await.unwrap(),before);
    let lookup=local_admission::lookup(State(app.clone()),Extension(actor.clone()),Path(("editorial-repair".into(),"repair-once".into()))).await.unwrap().0;
    assert_eq!(lookup["status"],"committed");assert_eq!(lookup["payloadHash"],editorial_review::hash_text(&Value::Object(expected_payload).to_string()));
    let mut changed=body;changed["expected"][0]["proposalRevision"]=json!(8);
    assert!(post(State(app.clone()),Extension(actor),Path(job),Json(changed)).await.is_err());app.db.close().await;
}
