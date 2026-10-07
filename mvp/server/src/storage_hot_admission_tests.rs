use super::*;

#[test]
fn existing_proposal_admission_keeps_recursive_paid_fact_bundle_and_unknown_closure() {
    let mut full=crate::empty();normalize(&mut full);crate::accounts::initialize(&mut full,crate::accounts::Profile::BawRussia).unwrap();
    full["items"]=json!([{"id":"item"}]);
    full["proposals"]=json!([{"id":"proposal","itemId":"item","status":"draft","prepareRunId":"origin",
        "modelMaterialReceipt":{"nativeJobId":"native-result"}}]);
    full["operations"]=json!([{"id":"historical-unknown","itemId":"other","status":"unknown","receipt":{"retain":true}}]);
    full["jobs"]=json!([
        {"id":"bundle-first","kind":"assistant","purpose":"discussion","status":"completed","prepareBundle":{"id":"shared"},"saved":"first duplicate"},
        {"id":"origin","kind":"assistant","purpose":"discussion","status":"completed","prepareBundle":{"id":"origin-bundle","researchManifest":[{"archiveId":"pin"}]},
            "factDependencies":[{"pin":{"prepareJobId":"fact-parent"}}]},
        {"id":"fact-parent","kind":"assistant","purpose":"discussion","status":"completed","prepareBundle":{"id":"shared"},"nativeSourceOriginJobId":"native-parent"},
        {"id":"native-parent","kind":"material_acquisition","status":"completed","paidResultRef":"original-paid"},
        {"id":"native-result","kind":"material_acquisition","status":"completed","retainedEvidence":{"paidResultRef":"original-paid"}},
        {"id":"cold","kind":"assistant","purpose":"discussion","status":"completed","saved":"unrelated"}]);
    full["preparationResearch"]=json!([{"id":"cold-research","body":"unrelated"},{"id":"pin","body":"first"},{"id":"pin","body":"second"}]);
    let body=json!({"proposals":[{"id":"proposal","revision":1}]});
    let view=project(&full,AdmissionScope::Approval(&body)).unwrap();
    assert_eq!(view["jobs"].as_array().unwrap().iter().map(|job|job["id"].as_str().unwrap()).collect::<Vec<_>>(),
        vec!["bundle-first","origin","fact-parent","native-parent","native-result"]);
    assert_eq!(view["preparationResearch"],json!([{"id":"pin","body":"first"},{"id":"pin","body":"second"}]));
    assert_eq!(view["operations"],full["operations"]);assert_eq!(view["proposals"],full["proposals"]);
    let editorial=project(&full,AdmissionScope::OperatorEditorial(&body)).unwrap();assert_eq!(editorial["preparationResearch"],full["preparationResearch"],"new editorial may choose cached research");
    full["jobs"][1]["factDependencies"][0]["pin"]["prepareJobId"]=json!([]);
    let broad=project(&full,AdmissionScope::Approval(&body)).unwrap();assert_eq!(broad["jobs"],full["jobs"],"ambiguous exact lineage retains full bodies");
}

#[tokio::test]
async fn scoped_editorial_repair_commits_once_and_protects_all_other_history(){
    let (app,_temp)=crate::tests::test_app().await;
    let(mut fixture,actor,job,body)=crate::editorial_repair::tests::fixture();external_writer_history(&mut fixture);
    let unrelated=json!({"id":"unrelated-approval","status":"approved","history":"immutable"});
    crate::list_mut(&mut fixture,"approvals").push(unrelated.clone());
    // Historical data is isolated fixture seeding, then native initialization
    // runs under the complete lifecycle ledger lock before any App admission.
    app.db.change(|d|{*d=fixture.clone();Ok(())}).await.unwrap();
    crate::native_fixture_owner_repair::initialize_db(&app.db).await.unwrap();
    fixture=app.read().await.unwrap();
    let canonical=crate::editorial_repair::normalized(&job,&body).unwrap();let scope=AdmissionScope::EditorialRepair{job:&job,body:&canonical};
    let before=project(&fixture,scope).unwrap();assert!(crate::row(&before,"jobs",&job).is_ok());
    let(first,changed)=app.db.change_admission_observed(scope,|d|crate::editorial_repair::admit(d,&actor,&job,&body)).await.unwrap();assert!(changed);
    let after=app.read().await.unwrap();assert_eq!(crate::row(&after,"approvals","unrelated-approval").unwrap(),&unrelated);
    assert_eq!(after["jobs"],fixture["jobs"]);assert_eq!(after["operations"],fixture["operations"]);assert_eq!(after["feedback"],fixture["feedback"]);
    let(second,changed)=app.db.change_admission_observed(scope,|d|crate::editorial_repair::admit(d,&actor,&job,&body)).await.unwrap();assert!(!changed);assert_eq!(first["newRefs"],second["newRefs"]);
    let mut forged=project(&after,scope).unwrap();forged["proposals"][0]["history"][0]["text"]=json!("Rewritten old history");
    assert!(validate_delta(&before,&forged,scope).is_err());app.db.close().await;
}

fn external_writer_history(d:&mut Value){
    for job in [
        json!({"id":"other-active-execute","kind":"execute","status":"running","refId":"other-approval","dispatchProof":"immutable-active-writer"}),
        json!({"id":"other-queued-reconcile","kind":"reconcile","status":"queued","refId":"other-old-operation"}),
        json!({"id":"other-pending-execute","kind":"execute","status":"pending","refId":"other-approval"}),
        json!({"id":"other-terminal-reconcile","kind":"reconcile","status":"failed","refId":"other-old-operation","journal":{"readback":"retained-history"}}),
    ]{crate::list_mut(d,"jobs").push(job);}
}

#[test]
fn scoped_admission_retains_foreign_external_writers_before_claiming_quiescence(){
    let(mut full,refs)=crate::operator_editorial::tests::fixture();external_writer_history(&mut full);
    let body=json!({"proposals":refs});
    for scope in [AdmissionScope::Approval(&body),AdmissionScope::Execute{approval:"new-approval",body:&body},AdmissionScope::OperatorEditorial(&body)]{
        let view=project(&full,scope).unwrap();
        for id in ["other-active-execute","other-queued-reconcile","other-pending-execute"]{
            assert_eq!(crate::row(&view,"jobs",id).unwrap(),crate::row(&full,"jobs",id).unwrap(),"a foreign approval/operation writer cannot disappear from the selected scope");
        }
        assert!(crate::row(&view,"jobs","other-terminal-reconcile").is_err(),"a terminal historical journal is not a live writer");
    }
}

#[test]
fn distinct_unknown_reply_close_checks_original_writer_in_scoped_admission(){
    let(mut full,refs)=crate::operator_editorial::tests::fixture();
    let item=crate::row(&full,"items","i0").unwrap().clone();
    crate::list_mut(&mut full,"operations").push(json!({"id":"old-reply","itemId":"i0","approvalId":"old-approval","status":"unknown",
        "action":{"action":"reply_and_close","actionId":"old-reply","objectId":item["objectId"],"itemId":item["itemId"],"conversationKey":item["conversationKey"]},
        "target":item,"executeReceipt":{"mutationOutcome":"uncertain"},"evidence":{"verificationPhase":"unconfirmed"}}));
    let body=json!({"proposals":refs});let requested=json!(["old-reply"]);
    let original=full["operations"].clone();
    assert!(crate::unknown_reply_close::capture(&full,&item,&requested).is_ok());
    for scope in [AdmissionScope::Approval(&body),AdmissionScope::Execute{approval:"new-approval",body:&body}]{
        let view=project(&full,scope).unwrap();
        assert_eq!(crate::unknown_reply_close::capture(&view,&item,&requested).unwrap(),crate::unknown_reply_close::capture(&full,&item,&requested).unwrap());
        let mut missing=view.clone();missing.as_object_mut().unwrap().remove("activeExternalJobs");
        assert!(crate::unknown_reply_close::capture(&missing,&item,&requested).is_err(),"partial view without job completeness is not quiescence");
    }
    for(kind,status,reference)in [("execute","running","old-approval"),("reconcile","queued","old-reply"),("execute","pending","old-approval")]{
        let mut active=full.clone();crate::list_mut(&mut active,"jobs").push(json!({"id":"original-worker","kind":kind,"status":status,"refId":reference}));
        for scope in [AdmissionScope::Approval(&body),AdmissionScope::Execute{approval:"new-approval",body:&body}]{
            let view=project(&active,scope).unwrap();
            assert!(crate::unknown_reply_close::capture(&view,&item,&requested).is_err(),"a late original writer must finish before the distinct close");
            assert_eq!(view["operations"],original,"inspection preserves the old UNKNOWN operation and its proof");
        }
    }
}

async fn seed_operator(app:&crate::App)->Value {
    let(source,refs)=crate::operator_editorial::tests::fixture();
    app.change(|d|{
        history(d);
        for table in ["posts","branches","items","proposals"] {
            for value in crate::list(&source,table){crate::list_mut(d,table).push(value.clone());}
        }
        Ok(())
    }).await.unwrap();
    refs
}

#[tokio::test]
async fn operator_editorial_scoped_preview_atomic_replay_and_history(){
    let(app,_temp)=crate::tests::test_app().await;let refs=seed_operator(&app).await;
    let original=app.read().await.unwrap();let preview_body=json!({"proposals":refs});
    let view=app.db.read_operator_editorial(&preview_body).await.unwrap();
    assert_eq!(view,project(&original,AdmissionScope::OperatorEditorial(&preview_body)).unwrap());
    assert_eq!(app.read().await.unwrap(),original,"preview writes no job, audit or receipt");
    assert_eq!(view["operations"],original["operations"]);
    assert!(view.to_string().len()*10<original.to_string().len());
    let actor=crate::operator_editorial::tests::actor();
    let body=crate::operator_editorial::tests::body(&view,&refs);
    let(result,changed)=app.db.change_admission_observed(AdmissionScope::OperatorEditorial(&body),|d|crate::operator_editorial::admit(d,&actor,&body)).await.unwrap();
    assert!(changed);let saved=app.read().await.unwrap();
    assert_eq!(crate::row(&saved,"jobs",result["jobId"].as_str().unwrap()).unwrap()["status"],"completed");
    let receipt=crate::row(&saved,"audit",&crate::local_admission::receipt_id("editorial",body["requestId"].as_str().unwrap())).unwrap();
    assert_eq!(receipt["account"],crate::accounts::Profile::from_workspace(&original).unwrap().key(),"local admission binds the canonical account key, while preview/receipt bind the workspace display account");
    for table in ["items","posts","branches","operations","approvals","feedback"]{assert_eq!(saved[table],original[table],"{table}");}
    assert_eq!(crate::row(&saved,"jobs","cold-job").unwrap(),crate::row(&original,"jobs","cold-job").unwrap());
    assert_eq!(crate::row(&saved,"conversations","cold-chat").unwrap(),crate::row(&original,"conversations","cold-chat").unwrap());
    let(replay,changed)=app.db.change_admission_observed(AdmissionScope::OperatorEditorial(&body),|d|crate::operator_editorial::admit(d,&actor,&body)).await.unwrap();
    assert!(!changed);assert_eq!(replay["jobId"],result["jobId"]);assert_eq!(replay["replayed"],true);assert_eq!(app.read().await.unwrap(),saved);
    let mut different=body.clone();different["operatorReview"]["entries"][0]["reason"]=json!("Changed request cannot reuse its admission identity");
    assert!(app.db.change_admission_observed(AdmissionScope::OperatorEditorial(&different),|d|crate::operator_editorial::admit(d,&actor,&different)).await.is_err());
    assert_eq!(app.read().await.unwrap(),saved);app.db.close().await;
}

#[test]
fn operator_editorial_scope_rejects_forged_receipt_and_ledger_changes(){
    let(workspace,refs)=crate::operator_editorial::tests::fixture();let actor=crate::operator_editorial::tests::actor();
    let body=crate::operator_editorial::tests::body(&workspace,&refs);let before=project(&workspace,AdmissionScope::OperatorEditorial(&body)).unwrap();
    let mut accepted=before.clone();crate::operator_editorial::admit(&mut accepted,&actor,&body).unwrap();
    validate_delta(&before,&accepted,AdmissionScope::OperatorEditorial(&body)).unwrap();
    for mutation in ["text","status","source","model_claim","authority","receipt","job","audit",
        "material_removed","material_company","material_paid","material_model","other_scope_material"] {
        let mut forged=accepted.clone();
        match mutation {
            "text"=>forged["proposals"][0]["text"]=json!("Different unreviewed text"),
            "status"=>forged["proposals"][0]["status"]=json!("approved"),
            "source"=>forged["items"][0]["text"]=json!("Replaced source"),
            "model_claim"=>forged["proposals"][0]["editorialReview"]["source"]["kind"]=json!("model_review"),
            "authority"=>forged["jobs"][0]["operatorReviewPreview"]["reviewAuthorityDigest"]=json!("0".repeat(64)),
            "receipt"=>{forged["proposals"][0]["editorialReviews"]=json!([]);},
            "job"=>forged["jobs"][0]["status"]=json!("running"),
            "material_removed"=>{forged["proposals"][0].as_object_mut().unwrap().remove("operatorMaterialReceipt");},
            "material_company"=>forged["proposals"][0]["operatorMaterialReceipt"]["companyId"]=json!("BAW Russia"),
            "material_paid"=>forged["proposals"][0]["operatorMaterialReceipt"]["paidResultRef"]=json!({"nativeJobId":"unrelated-paid"}),
            "material_model"=>forged["proposals"][0]["operatorMaterialReceipt"]["modelCalled"]=json!(true),
            "other_scope_material"=>forged["proposals"][0]["editorialModelMaterialReceipt"]=json!({"nativeJobId":"unrelated-paid"}),
            _=>forged["audit"][0]["actor"]["id"]=json!("another-owner"),
        }
        assert!(validate_delta(&before,&forged,AdmissionScope::OperatorEditorial(&body)).is_err(),"{mutation}");
    }
}

#[test]
fn operator_material_receipt_full_and_scoped_derivation_match_with_cold_history(){
    let(mut workspace,refs)=crate::operator_editorial::tests::fixture();
    crate::list_mut(&mut workspace,"jobs").push(json!({"id":"cold-operator-history","kind":"assistant","purpose":"discussion","status":"completed","result":{"private":"cold".repeat(8192)}}));
    let actor=crate::operator_editorial::tests::actor();let body=crate::operator_editorial::tests::body(&workspace,&refs);
    let before=project(&workspace,AdmissionScope::OperatorEditorial(&body)).unwrap();
    assert!(before["jobs"].as_array().unwrap().is_empty());
    let mut full=workspace.clone();crate::operator_editorial::admit(&mut full,&actor,&body).unwrap();
    let mut scoped=before.clone();crate::operator_editorial::admit(&mut scoped,&actor,&body).unwrap();
    validate_delta(&workspace,&full,AdmissionScope::OperatorEditorial(&body)).unwrap();
    validate_delta(&before,&scoped,AdmissionScope::OperatorEditorial(&body)).unwrap();
    for field in ["companyId","connectorBinding","proposalId","proposalRevision","textSha256","previewDigest","reviewedBy","reviewAuthorityDigest","candidate","body"]{
        assert_eq!(full["proposals"][0]["operatorMaterialReceipt"][field],scoped["proposals"][0]["operatorMaterialReceipt"][field],"exact native {field}");
    }
    // Both original, independently timestamped receipts were authenticated by
    // the native guard above. Compare their fixed material/source authority;
    // do not rewrite receipt hashes or clock fields to hide a bad derivation.
    let mut forged=scoped.clone();forged["proposals"][0]["operatorMaterialReceipt"]["body"]["companyId"]=json!("BAW Russia");
    let receipt=&mut forged["proposals"][0]["operatorMaterialReceipt"];receipt.as_object_mut().unwrap().remove("receiptSha256");
    receipt["receiptSha256"]=json!(crate::preparation_materials::hash(receipt));
    assert!(validate_delta(&before,&forged,AdmissionScope::OperatorEditorial(&body)).is_err(),"a self-consistent forged hash cannot replace native derivation");
    assert_eq!(full["jobs"][0],workspace["jobs"][0]);
}

pub(crate) fn native_editorial_material_fixture()->(Value,Value,Value,String){
    let(mut d,refs)=crate::operator_editorial::tests::fixture();
    let plan=crate::editorial_review::plan_new(&d,&refs,"2026-10-06T00:00:00Z").unwrap();
    assert!(plan["held"].as_array().unwrap().is_empty());assert_eq!(plan["batches"].as_array().unwrap().len(),1);
    let batch=plan["batches"][0].clone();
    let entries=batch["request"]["editorialCandidates"].as_array().unwrap().iter().map(|candidate|{
        let mut entry=json!({"proposalId":candidate["proposalId"],"proposalRevision":candidate["proposalRevision"],"itemId":candidate["itemId"],
            "textSha256":candidate["textSha256"],"contextDigest":candidate["contextDigest"],"rulesDigest":candidate["rulesDigest"],
            "decision":"accept","reason":"Exact synthetic native editorial proof","proposedText":null,
            "checks":{"companyRules":"pass","intent":"pass","factualScope":"pass"}});
        if crate::decision_media::enabled(candidate){entry["mediaDependency"]=json!({"audio":"independent","visual":"independent"});}entry
    }).collect::<Vec<_>>();
    let mut metadata=crate::editorial_review::fixture_metadata();metadata["model"]=json!(crate::codex_model_policy::MODEL);
    metadata["modelProfile"]=json!(crate::codex_model_policy::PROFILE);metadata["reasoningEffort"]=json!("high");metadata["cliSha256"]=json!(crate::codex_model_policy::CLI_SHA256);
    let mut result=json!({"text":"Offline native editorial delta fixture","sources":[],"proposals":[],"editorial":entries,"runMetadata":metadata});
    let job=crate::editorial_review::fixture_capture_result(&mut d,&batch,&mut result).unwrap();
    crate::list_mut(&mut d,"jobs").push(json!({"id":"cold-editorial-history","kind":"assistant","purpose":"discussion","status":"completed","result":{"private":"cold".repeat(8192)}}));
    (d,batch,result,job)
}

#[test]
fn editorial_material_delta_full_scoped_exact_paid_capture_and_hostile_scope(){
    let(before,batch,result,job)=native_editorial_material_fixture();let scope=AdmissionScope::EditorialJob(&job);
    let projection=project(&before,scope).unwrap();
    assert_eq!(crate::row(&projection,"jobs",&job).unwrap(),crate::row(&before,"jobs",&job).unwrap(),"complete journal and plural paid proof history");
    assert!(crate::row(&projection,"jobs","cold-editorial-history").is_err());
    let mut full=before.clone();let mut scoped=projection.clone();
    let a=crate::editorial_review::admit(&mut full,&batch,&result,"2026-10-06T00:00:01Z").unwrap();
    let b=crate::editorial_review::admit(&mut scoped,&batch,&result,"2026-10-06T00:00:01Z").unwrap();
    assert_eq!(a,b);assert_eq!(full["proposals"],scoped["proposals"]);
    validate_delta(&before,&full,scope).unwrap();validate_delta(&projection,&scoped,scope).unwrap();
    assert!(scoped["proposals"][0]["editorialModelMaterialReceipt"].is_object());
    for mutation in ["native_job","paid_capture","company","invocation","candidate","operator_field","text","wrong_scope"]{
        let mut forged=scoped.clone();
        match mutation{
            "native_job"=>forged["proposals"][0]["editorialModelMaterialReceipt"]["nativeJobId"]=json!("cold-editorial-history"),
            "paid_capture"=>forged["proposals"][0]["editorialModelMaterialReceipt"]["paidResultRef"]["requestSha256"]=json!("0".repeat(64)),
            "company"=>forged["proposals"][0]["editorialModelMaterialReceipt"]["companyId"]=json!("BAW Russia"),
            "invocation"=>forged["proposals"][0]["editorialReview"]["source"]["runMetadata"]["materialInvocation"]["completenessStatus"]=json!("pending"),
            "candidate"=>forged["proposals"][0]["editorialReview"]["candidate"]["textSha256"]=json!("0".repeat(64)),
            "operator_field"=>forged["proposals"][0]["operatorMaterialReceipt"]=json!({"modelCalled":false}),
            "text"=>forged["proposals"][0]["text"]=json!("Different unreviewed text"),_=>(),
        }
        // Make pointer-local integrity valid where possible: native ownership,
        // exact retained attachment/candidate still have to reject the forgery.
        if matches!(mutation,"native_job"|"paid_capture"|"company"){
            let pointer=&mut forged["proposals"][0]["editorialModelMaterialReceipt"];
            pointer.as_object_mut().unwrap().remove("pointerSha256");pointer["pointerSha256"]=json!(crate::preparation_materials::hash(pointer));
        }
        let attempted=if mutation=="wrong_scope"{AdmissionScope::EditorialJob("cold-editorial-history")}else{scope};
        assert!(validate_delta(&projection,&forged,attempted).is_err(),"{mutation}");
    }
    assert_eq!(full["jobs"],before["jobs"],"no new paid call/history rewrite during verdict admission");
    let refs=json!({"proposals":[{"id":full["proposals"][0]["id"],"revision":full["proposals"][0]["revision"]}]});
    let approval_view=project(&full,AdmissionScope::Approval(&refs)).unwrap();
    assert_eq!(crate::row(&approval_view,"jobs",&job).unwrap(),crate::row(&full,"jobs",&job).unwrap(),"next hot scope retains the complete new native pointer owner");
    assert!(crate::proposal_current(&full,&full["proposals"][0]).is_ok());
    assert!(crate::proposal_current(&approval_view,&approval_view["proposals"][0]).is_ok());
    let mut missing=projection.clone();missing["jobs"]=json!([]);
    assert!(crate::preparation_materials::validate_editorial_material_delta(&missing,&job,&projection["proposals"][0],&scoped["proposals"][0]).is_err(),"omitted native job cannot certify the added proof");
}

/// Uses only a new loopback test database admitted by writer_v51_fixture_db.
#[tokio::test]
#[ignore = "requires a fresh isolated PostgreSQL fixture; run this selector alone"]
async fn postgres_operator_editorial_scope_parity(){
    std::env::var("COMMUNITYHERO_WRITER_V51_TEST_URL").expect("explicit isolated fixture URL");
    let(mut app,_temp)=crate::tests::test_app().await;let initial=app.read().await.unwrap();
    let db=super::super::preparation::writer_v51_fixture_db().await;app.db.close().await;app.db=db;
    app.db.change(|d|{*d=initial;Ok(())}).await.unwrap();let refs=seed_operator(&app).await;
    app.db.change(|d|{external_writer_history(d);Ok(())}).await.unwrap();
    let actor=crate::operator_editorial::tests::actor();let preview_body=json!({"proposals":refs});
    let full=app.read().await.unwrap();let view=app.db.read_operator_editorial(&preview_body).await.unwrap();
    assert_eq!(view,project(&full,AdmissionScope::OperatorEditorial(&preview_body)).unwrap());
    for scope in [AdmissionScope::Approval(&preview_body),AdmissionScope::Execute{approval:"new-approval",body:&preview_body}]{
        let(writer,changed)=app.db.change_admission_observed(scope,|d|Ok(d.clone())).await.unwrap();
        assert!(!changed);assert_eq!(writer,project(&full,scope).unwrap());
        assert!(crate::row(&writer,"jobs","other-active-execute").is_ok());
        assert!(crate::row(&writer,"jobs","other-queued-reconcile").is_ok());
        assert!(crate::row(&writer,"jobs","other-pending-execute").is_ok());
        assert!(crate::row(&writer,"jobs","other-terminal-reconcile").is_err());
    }
    assert_eq!(app.read().await.unwrap(),full,"quiescence inspection must not mutate or restart another operation");
    let body=crate::operator_editorial::tests::body(&view,&refs);
    let(writer_view,changed)=app.db.change_admission_observed(AdmissionScope::OperatorEditorial(&body),|d|Ok(d.clone())).await.unwrap();
    assert!(!changed);assert_eq!(writer_view,view,"read and writer dependency closure is identical");
    let(result,changed)=app.db.change_admission_observed(AdmissionScope::OperatorEditorial(&body),|d|crate::operator_editorial::admit(d,&actor,&body)).await.unwrap();
    assert!(changed);let saved=app.read().await.unwrap();
    let(replay,changed)=app.db.change_admission_observed(AdmissionScope::OperatorEditorial(&body),|d|crate::operator_editorial::admit(d,&actor,&body)).await.unwrap();
    assert!(!changed);assert_eq!(replay["jobId"],result["jobId"]);assert_eq!(app.read().await.unwrap(),saved);
    for table in ["items","posts","branches","operations","approvals","feedback"]{assert_eq!(saved[table],full[table],"{table}");}
    // Root policy: a terminal failed model journal is retained history, not
    // an active writer or a prohibition on an explicit current local review.
    app.db.change(|d|{crate::list_mut(d,"jobs").push(json!({"id":"failed-paid-editorial","kind":"editorial_review","status":"failed","refId":"old-review","editorialReferences":refs,
        "editorialBatches":[{"state":"captured","batchId":"old-attempt","capture":{"immutable":"paid ownership"}}]}));Ok(())}).await.unwrap();
    let terminal_full=app.read().await.unwrap();let terminal=app.db.read_operator_editorial(&preview_body).await.unwrap();
    assert_eq!(terminal,project(&terminal_full,AdmissionScope::OperatorEditorial(&preview_body)).unwrap());
    assert!(crate::operator_editorial::capture(&terminal,&actor,&preview_body).is_ok());
    assert_eq!(crate::row(&app.read().await.unwrap(),"jobs","failed-paid-editorial").unwrap(),crate::row(&terminal_full,"jobs","failed-paid-editorial").unwrap());
    app.db.change(|d|{crate::list_mut(d,"jobs").push(json!({"id":"active-paid-editorial","kind":"editorial_review","status":"running","refId":"active-review","editorialReferences":refs,
        "editorialBatches":[{"state":"captured","batchId":"active-attempt"}]}));Ok(())}).await.unwrap();
    let blocked_full=app.read().await.unwrap();let blocked=app.db.read_operator_editorial(&preview_body).await.unwrap();
    assert_eq!(blocked,project(&blocked_full,AdmissionScope::OperatorEditorial(&preview_body)).unwrap());
    assert!(crate::row(&blocked,"jobs","active-paid-editorial").is_ok());
    assert!(crate::operator_editorial::capture(&blocked,&actor,&preview_body).is_err());
    assert_eq!(app.read().await.unwrap(),blocked_full,"uncertain preview is still read-only");
    // All external recipient aliases remain, even outside selected local IDs.
    app.db.change(|d|{
        let target=crate::row(d,"items","i0")?.clone();
        // The old local alias remains a durable item; the operation's target
        // still resolves to the selected recipient through external identity.
        let mut historical=target.clone();
        historical["id"]=json!("historical-local-alias");
        crate::list_mut(d,"items").push(historical);
        crate::list_mut(d,"operations").push(json!({"id":"old-alias-unknown","status":"unknown","itemId":"historical-local-alias","target":target}));
        Ok(())
    }).await.unwrap();
    let alias_full=app.read().await.unwrap();let alias_view=app.db.read_operator_editorial(&preview_body).await.unwrap();
    assert_eq!(alias_view["operations"],alias_full["operations"]);assert!(crate::row(&alias_view,"operations","old-alias-unknown").is_ok());
    app.db.close().await;
}

fn actor()->crate::operator_auth::Actor{crate::operator_auth::Actor::local_owner("synthetic")}
fn history(d:&mut Value){
    crate::list_mut(d,"jobs").push(json!({"id":"cold-job","kind":"assistant","status":"completed","result":{"text":"cold".repeat(30_000)}}));
    crate::list_mut(d,"approvals").push(json!({"id":"cold-approval","status":"approved","proposals":[],"largeEvidence":"cold".repeat(30_000)}));
    crate::list_mut(d,"feedback").push(json!({"id":"cold-feedback","itemId":"item-1","origin":{"text":"cold".repeat(30_000)}}));
    crate::list_mut(d,"audit").push(json!({"id":"cold-audit","action":"other","refId":"item-1"}));
    crate::list_mut(d,"conversations").push(json!({"id":"cold-chat","messages":[{"id":"private","role":"user","text":"cold".repeat(30_000)}]}));
}

#[tokio::test]
async fn admission_projection_keeps_alias_unknown_and_exact_generation_without_cold_history(){
    let(app,_temp)=crate::tests::test_app().await;
    let mut d=app.read().await.unwrap();history(&mut d);
    let p=crate::create_proposal(&mut d,&json!({"itemId":"item-1","kind":"close","expectedRevision":1})).unwrap();
    crate::row_mut(&mut d,"proposals",p["id"].as_str().unwrap()).unwrap()["prepareRunId"]=json!("generation");
    crate::list_mut(&mut d,"jobs").push(json!({"id":"generation","kind":"assistant","status":"completed","prepareBundle":{"proof":"keep"}}));
    crate::list_mut(&mut d,"jobs").push(json!({"id":"media-proof","kind":"media_audio","status":"completed","refId":"post-1"}));
    let target=json!({"objectId":d["items"][0]["objectId"],"itemId":d["items"][0]["itemId"]});
    crate::list_mut(&mut d,"operations").push(json!({"id":"other-local-unknown","itemId":"historic-local-alias","status":"unknown","target":target}));
    let body=json!({"proposals":[{"id":p["id"],"revision":p["revision"]}],"requestId":"scope"});
    let view=project(&d,AdmissionScope::Approval(&body)).unwrap();
    for table in ["feedback","approvals","audit","conversations"]{assert!(rows(&view,table).unwrap().is_empty(),"{table}");}
    assert_eq!(view["operations"],d["operations"],"all aliases and malformed legacy blockers stay visible");
    assert!(crate::row(&view,"jobs","generation").is_ok());assert!(crate::row(&view,"jobs","media-proof").is_ok());
    assert!(crate::row(&view,"jobs","cold-job").is_err());
    assert!(view.to_string().len()*10<d.to_string().len());
    app.db.close().await;
}

#[tokio::test]
async fn production_approval_and_execute_keep_history_replay_and_atomic_ledger(){
    let(app,_temp)=crate::tests::test_app().await;
    app.change(|d|crate::connection_gate::fixture_open(d)).await.unwrap();
    app.change(|d|{history(d);Ok(())}).await.unwrap();
    let p=app.change(|d|crate::create_proposal(d,&json!({"itemId":"item-1","kind":"close","expectedRevision":1}))).await.unwrap();
    let body=json!({"proposals":[{"id":p["id"],"revision":p["revision"]}],"requestId":"approval-scoped"});
    let a=crate::approval_new(crate::State(app.clone()),axum::Extension(actor()),crate::Json(body.clone())).await.unwrap().0;
    let saved=app.read().await.unwrap();
    assert_eq!(saved["feedback"].as_array().unwrap().len(),2);assert_eq!(saved["feedback"][1]["approvalId"],a["id"]);
    assert_eq!(crate::row(&saved,"approvals","cold-approval").unwrap()["largeEvidence"],"cold".repeat(30_000));
    let replay=crate::approval_new(crate::State(app.clone()),axum::Extension(actor()),crate::Json(body)).await.unwrap().0;
    assert_eq!(replay["id"],a["id"]);assert_eq!(replay["replayed"],true);assert_eq!(app.read().await.unwrap(),saved);
    let key=a["id"].as_str().unwrap();let body=json!({"approvalId":key,"requestId":"execute-scoped"});
    let(outcome,scheduled)=app.change_admission(AdmissionScope::Execute{approval:key,body:&body},|d|crate::execute_admission::admit(d,&actor(),key,&body)).await.unwrap();
    assert!(scheduled.is_some());
    let executed=app.read().await.unwrap();
    assert_eq!(crate::row(&executed,"approvals",key).unwrap()["status"],"consumed");
    assert_eq!(executed["operations"].as_array().unwrap().len(),1);
    let(again,scheduled)=app.change_admission(AdmissionScope::Execute{approval:key,body:&body},|d|crate::execute_admission::admit(d,&actor(),key,&body)).await.unwrap();
    assert!(scheduled.is_none());assert_eq!(again["jobId"],outcome["jobId"]);assert_eq!(again["replayed"],true);
    assert_eq!(app.read().await.unwrap(),executed,"replay must not append another operation or worker");
    let mut foreign=actor();foreign.id="other".into();
    assert!(app.change_admission(AdmissionScope::Execute{approval:key,body:&body},|d|crate::execute_admission::admit(d,&foreign,key,&body)).await.is_err());
    assert_eq!(app.read().await.unwrap(),executed);app.db.close().await;
}

#[tokio::test]
async fn late_invalid_recipient_rolls_back_approval_and_preserves_unknown(){
    let(app,_temp)=crate::tests::test_app().await;
    let p=app.change(|d|crate::create_proposal(d,&json!({"itemId":"item-1","kind":"close","expectedRevision":1}))).await.unwrap();
    let before=app.read().await.unwrap();
    let body=json!({"requestId":"late-error","proposals":[{"id":p["id"],"revision":p["revision"]},{"id":"missing","revision":1}]});
    assert!(crate::approval_new(crate::State(app.clone()),axum::Extension(actor()),crate::Json(body)).await.is_err());
    assert_eq!(app.read().await.unwrap(),before);app.db.close().await;
}

#[test]
fn scope_rejects_source_history_and_foreign_mutations(){
    let mut before=crate::empty();normalize(&mut before);
    before["jobs"]=json!([{"id":"editorial","kind":"editorial_review","status":"running","editorialReferences":[],"editorialPlan":{"batches":[]},"editorialBatches":[]}]);
    before["operations"]=json!([{"id":"unknown","status":"unknown"}]);
    let scope=AdmissionScope::EditorialJob("editorial");
    for(table,field,value)in [("operations","status",json!("failed")),("jobs","status",json!("completed"))]{
        let mut after=before.clone();after[table][0][field]=value;assert!(validate_delta(&before,&after,scope).is_err());
    }
    let mut after=before.clone();after["settings"]["enabled"]=json!(true);assert!(validate_delta(&before,&after,scope).is_err());
    let mut after=before.clone();after["feedback"]=json!([{"id":"injected","itemId":"foreign"}]);assert!(validate_delta(&before,&after,scope).is_err());
}

#[test]
fn settled_editorial_attempt_is_immutable_and_new_capture_needs_parent_proof(){
    let old=json!({"id":"j","editorialPlan":{"batches":[]},"editorialBatches":[{"batchId":"b","state":"settled","result":{"outcomes":[]},"resultDigest":"original","capture":{"dispatchDigest":"immutable"}}]});
    let mut changed=old.clone();changed["editorialBatches"][0]["resultDigest"]=json!("replaced");assert!(validate_editorial_journal(&old,&changed).is_err());
    let mut changed=old.clone();changed["editorialBatches"].as_array_mut().unwrap().push(json!({"batchId":"new","state":"captured","capture":{}}));
    assert!(validate_editorial_journal(&old,&changed).is_err());
}

#[test]
fn receipt_refresh_preserves_other_reviews_private_messages_and_attempt_identity(){
    let chat=json!({"id":"chat","messages":[{"id":"user","role":"user","text":"confirmation"}],"actionReviews":[
        {"id":"review","status":"admitted","execution":{"approvalId":"approval","attemptId":"attempt","jobId":"job"}},
        {"id":"other-review","status":"admitted","execution":{"approvalId":"other"}}]});
    let mut changed=chat.clone();changed["actionReviews"][0]["execution"]["attemptId"]=json!("retarget");assert!(validate_receipt_chat(&chat,&changed,Some("approval")).is_err());
    let mut changed=chat.clone();changed["messages"][0]["text"]=json!("rewrite");assert!(validate_receipt_chat(&chat,&changed,Some("approval")).is_err());
    let mut changed=chat.clone();changed["actionReviews"][1]["status"]=json!("failed");assert!(validate_receipt_chat(&chat,&changed,Some("approval")).is_err());
    let mut changed=chat.clone();changed["actionReviews"][0]["outcome"]=json!({"counts":{"unknown":1}});
    changed["messages"].as_array_mut().unwrap().push(json!({"id":"execution:review","role":"assistant","serverActionExecution":true,"actionExecution":{"reviewId":"review","counts":{"unknown":1}}}));
    validate_receipt_chat(&chat,&changed,Some("approval")).unwrap();
}

#[tokio::test]
async fn schedule_preview_projection_is_identical_to_writer_source_scope(){
    let(app,_temp)=crate::tests::test_app().await;
    app.change(|d|{history(d);Ok(())}).await.unwrap();
    let preview=app.db.read_preparation_schedule().await.unwrap();
    let(inside,_)=app.db.change_preparation_schedule_observed(|d|Ok(d.clone())).await.unwrap();
    assert_eq!(preview,inside);assert!(preview.get("feedback").is_none());
    assert!(crate::row(&preview,"jobs","cold-job").is_err());app.db.close().await;
}

/// Explicit isolated PostgreSQL fixture only. The helper rejects remote hosts,
/// non-test names and databases that already contain a CommunityHero schema.
/// Run this selector alone when providing the fixture variables.
#[tokio::test]
#[ignore = "requires a fresh isolated PostgreSQL fixture; run this selector alone"]
async fn postgres_hot_admission_atomic_replay_and_history_parity(){
    std::env::var("COMMUNITYHERO_WRITER_V51_TEST_URL").expect("explicit isolated fixture URL");
    let(mut app,_temp)=crate::tests::test_app().await;
    let mut initial=app.read().await.unwrap();history(&mut initial);
    let db=super::super::preparation::writer_v51_fixture_db().await;
    app.db.close().await;app.db=db;
    app.db.change(|d|{*d=initial;Ok(())}).await.unwrap();
    let selected=app.db.create_proposal_observed(&json!({"itemId":"item-1","kind":"close","expectedRevision":1,"eventId":"global-event"}),&app.lifecycle_owner).await.unwrap().0;
    let body=json!({"requestId":"pg-approval","proposals":[{"id":selected["id"],"revision":selected["revision"]}]});
    let full=app.read().await.unwrap();
    let(scope,changed)=app.db.change_admission_observed(AdmissionScope::Approval(&body),|d|Ok(d.clone())).await.unwrap();
    assert!(!changed);assert_eq!(scope,project(&full,AdmissionScope::Approval(&body)).unwrap(),"PostgreSQL query projection must match complete-state dependency selection");
    let full_bytes=serde_json::to_vec(&full).unwrap().len();let scoped_bytes=serde_json::to_vec(&scope).unwrap().len();
    assert!(scoped_bytes*10<full_bytes,"unrelated retained history must not enter the admission tree");
    eprintln!("hot-admission-fixture full_json_bytes={full_bytes} scoped_json_bytes={scoped_bytes} full_feedback_rows={} scoped_feedback_rows={} full_approval_rows={} scoped_approval_rows={} full_job_rows={} scoped_job_rows={}",
        rows(&full,"feedback").unwrap().len(),rows(&scope,"feedback").unwrap().len(),rows(&full,"approvals").unwrap().len(),rows(&scope,"approvals").unwrap().len(),rows(&full,"jobs").unwrap().len(),rows(&scope,"jobs").unwrap().len());
    let approval=crate::approval_new(crate::State(app.clone()),axum::Extension(actor()),crate::Json(body.clone())).await.unwrap().0;
    let before=app.read().await.unwrap();
    let replay=crate::approval_new(crate::State(app.clone()),axum::Extension(actor()),crate::Json(body)).await.unwrap().0;
    assert_eq!(replay["id"],approval["id"]);assert_eq!(app.read().await.unwrap(),before);
    // The same global event on a different item cannot be hidden by an item
    // filter. Check retry directly inside the production proposal adapter.
    let original_item=crate::row(&before,"items","item-1").unwrap();
    let mut foreign=original_item.clone();foreign["id"]=json!("foreign-item");foreign["itemId"]=json!("foreign-comment");
    foreign["workflow"]=json!("attention");foreign["revision"]=json!(1);
    app.db.change(|d|{crate::list_mut(d,"items").push(foreign);Ok(())}).await.unwrap();
    let unchanged=app.read().await.unwrap();
    assert!(app.db.create_proposal_observed(&json!({"itemId":"foreign-item","kind":"close","expectedRevision":1,"eventId":"global-event"}),&app.lifecycle_owner).await.is_err());
    assert_eq!(app.read().await.unwrap(),unchanged);
    let key=approval["id"].as_str().unwrap();let body=json!({"approvalId":key,"requestId":"pg-execute"});
    let(result,scheduled)=app.change_admission(AdmissionScope::Execute{approval:key,body:&body},|d|crate::execute_admission::admit(d,&actor(),key,&body)).await.unwrap();
    assert!(scheduled.is_some());let executed=app.read().await.unwrap();
    let(replay,scheduled)=app.change_admission(AdmissionScope::Execute{approval:key,body:&body},|d|crate::execute_admission::admit(d,&actor(),key,&body)).await.unwrap();
    assert!(scheduled.is_none());assert_eq!(replay["jobId"],result["jobId"]);assert_eq!(app.read().await.unwrap(),executed);
    let job=result["jobId"].as_str().unwrap();
    app.change_admission(AdmissionScope::ExecutionReceipts(job),|d|crate::assistant_action_review::refresh_execution_receipts(d,job)).await.unwrap();
    assert_eq!(app.read().await.unwrap(),executed,"no affected chats produces no write");
    let(scope,_) = app.db.change_admission_observed(AdmissionScope::ExecutionReceipts(job),|d|Ok(d.clone())).await.unwrap();
    assert!(rows(&scope,"conversations").unwrap().is_empty());assert_eq!(rows(&scope,"approvals").unwrap().len(),1);
    assert_eq!(rows(&scope,"operations").unwrap().len(),1);assert!(crate::row(&scope,"jobs","cold-job").is_err());
    let invalid=app.db.change_admission_observed(AdmissionScope::ExecutionReceipts(job),|d|{d["operations"][0]["status"]=json!("succeeded");Ok(())}).await;
    assert!(invalid.is_err());assert_eq!(app.read().await.unwrap(),executed);
    for table in ["approvals","feedback","jobs","conversations"]{
        let key=match table{"approvals"=>"cold-approval","feedback"=>"cold-feedback","jobs"=>"cold-job",_=>"cold-chat"};
        assert_eq!(crate::row(&executed,table,key).unwrap(),crate::row(&unchanged,table,key).unwrap());
    }
    // An affected legacy receipt and an unrelated private chat coexist. Compare
    // the production refresh reducer on full state with its scoped transaction.
    let exact=crate::row(&executed,"approvals",key).unwrap()["proposals"].clone();
    app.db.change(|d|{
        crate::list_mut(d,"conversations").push(json!({"id":"affected-chat","operatorId":"local-owner","messages":[],"actionReviews":[
            {"id":"legacy-review","status":"admitted","proposals":exact,"execution":{"approvalId":key,"jobId":job}},
            {"id":"unrelated-review","status":"presented","execution":{"approvalId":"other-approval"}}]}));Ok(())
    }).await.unwrap();
    let with_chat=app.read().await.unwrap();let mut expected=with_chat.clone();
    crate::assistant_action_review::refresh_execution_receipts(&mut expected,job).unwrap();
    app.change_admission(AdmissionScope::ExecutionReceipts(job),|d|crate::assistant_action_review::refresh_execution_receipts(d,job)).await.unwrap();
    let mut actual=app.read().await.unwrap();
    fn times(v:&mut Value){match v{Value::Array(a)=>a.iter_mut().for_each(times),Value::Object(o)=>for(k,v)in o{if matches!(k.as_str(),"createdAt"|"updatedAt"){*v=json!("<time>");}else{times(v);}},_=>()}}
    times(&mut actual);times(&mut expected);assert_eq!(actual,expected);
    assert_eq!(crate::row(&actual,"conversations","affected-chat").unwrap()["actionReviews"][1],with_chat["conversations"].as_array().unwrap().iter().find(|c|c["id"]=="affected-chat").unwrap()["actionReviews"][1]);
    // Real scoped editorial scheduling and captured paid-attempt journaling,
    // with deterministic unavailable-result settlement; no model is invoked.
    let reply=app.db.create_proposal_observed(&json!({"itemId":"foreign-item","kind":"reply_and_close","text":"Exact final response","expectedRevision":1}),&app.lifecycle_owner).await.unwrap().0;
    let body=json!({"requestId":"pg-editorial","proposals":[{"id":reply["id"],"revision":reply["revision"]}]});
    let(_,editorial)=app.change_admission(AdmissionScope::EditorialSchedule(&body),|d|crate::editorial_endpoint::schedule(d,&actor(),&body)).await.unwrap();
    let editorial=editorial.unwrap();let stored=app.db.read_job(&editorial).await.unwrap().unwrap();
    let batch=stored["editorialPlan"]["batches"][0].clone();assert!(batch.is_object());
    let capture=app.change_admission(AdmissionScope::EditorialJob(&editorial),|d|crate::editorial_endpoint::capture_dispatch(d,&editorial,&batch)).await.unwrap();
    assert!(capture["batch"].is_object());
    app.change_admission(AdmissionScope::EditorialJob(&editorial),|d|{
        let entry=&mut crate::row_mut(d,"jobs",&editorial)?["editorialBatches"][0];
        let result=json!({"outcomes":[{"proposalId":reply["id"],"decision":"hold","reason":"Deterministic unavailable fixture"}]});
        entry["state"]=json!("settled");entry["resultDigest"]=json!(crate::editorial_review::hash_text(&result.to_string()));entry["result"]=result;Ok(())
    }).await.unwrap();
    let before_replay=app.read().await.unwrap();
    assert!(app.change_admission(AdmissionScope::EditorialJob(&editorial),|d|crate::editorial_endpoint::capture_dispatch(d,&editorial,&batch)).await.is_err());
    assert_eq!(app.read().await.unwrap(),before_replay,"captured attempt cannot become another model call");
    // Real PostgreSQL preparation ownership and the exact CLI receipt proof.
    // No worker is spawned: scheduling itself is the paid-call admission seam.
    let sources=crate::engine_prepare::tests::fixture(false);
    app.db.change(|d|{for table in ["items","posts","branches"] {for row in crate::list(&sources,table){crate::list_mut(d,table).push(row.clone());}}Ok(())}).await.unwrap();
    // Mirror the production schedule_admitted boundary without spawning a worker.
    fn schedule_native_preparation(d:&mut Value,input:crate::engine_prepare::Input,
        identity:&crate::runtime_lifecycle::RuntimeIdentity)->crate::ApiResult<crate::engine_prepare::Scheduled> {
        let owner=crate::runtime_lifecycle::bound_admission_token(d,identity,crate::runtime_lifecycle::AdmissionClass::Preparation)?;
        crate::runtime_lifecycle::require_admission(d,&owner,crate::runtime_lifecycle::AdmissionClass::Preparation)?;
        let scheduled=crate::engine_prepare::schedule(d,input)?;
        crate::preparation_review::record_initial_admission(d,&owner,&scheduled.job_id,&crate::now())?;
        Ok(scheduled)
    }
    let prepare=json!({"itemIds":["ready"],"requestId":"pg-prepare-reserved"});
    let (prepared,_) = app.db.change_preparation_schedule_observed(|d|{
        let request=crate::local_admission::request(d,"prepare",&prepare,&actor())?.unwrap();
        let scheduled=schedule_native_preparation(d,crate::engine_prepare::parse(&prepare)?,&app.lifecycle_owner)?;
        let reservation=&crate::row(d,"jobs",&scheduled.job_id)?["scopeReservation"];
        let mut result=json!({"jobId":scheduled.job_id,"scopeReservation":{"version":1,"ownerJobId":reservation["ownerJobId"],"keysDigest":reservation["keysDigest"]}});
        crate::local_admission::commit(d,&request,&mut result)?;Ok(result)
    }).await.unwrap();
    let reserved_job=prepared["jobId"].as_str().unwrap();
    let stored=app.db.read_job(reserved_job).await.unwrap().unwrap();
    assert_eq!(prepared["scopeReservation"]["ownerJobId"],stored["id"]);
    assert_eq!(prepared["scopeReservation"]["keysDigest"],stored["scopeReservation"]["keysDigest"]);
    assert_eq!(prepared["scopeReservation"]["version"],1);
    let digest=prepared["scopeReservation"]["keysDigest"].as_str().unwrap();
    assert_eq!(digest.len(),64);assert!(digest.bytes().all(|b|b.is_ascii_digit()||(b'a'..=b'f').contains(&b)));
    let reserved=app.read().await.unwrap();
    let preview=app.db.read_preparation_schedule().await.unwrap();
    let(inside,changed)=app.db.change_preparation_schedule_observed(|d|Ok(d.clone())).await.unwrap();
    assert!(!changed);assert_eq!(preview,inside,"reader and writer ownership control snapshots match");
    assert_eq!(preview["scopeOwners"][0]["scopeReservation"],stored["scopeReservation"]);
    let replay=app.db.change_preparation_schedule_observed(|d|{
        let request=crate::local_admission::request(d,"prepare",&prepare,&actor())?.unwrap();
        crate::local_admission::replay(d,&request,&actor())?.ok_or_else(||internal("Fixture receipt missing"))
    }).await.unwrap().0;
    assert_eq!(replay["jobId"],prepared["jobId"]);assert_eq!(replay["scopeReservation"],prepared["scopeReservation"]);
    assert_eq!(app.read().await.unwrap(),reserved,"lost response recovers proof and never schedules again");
    let conflict=app.db.change_preparation_schedule_observed(|d|schedule_native_preparation(d,crate::engine_prepare::parse(&json!({"itemIds":["ready"]}))?,&app.lifecycle_owner).map(|s|s.job_id)).await;
    assert!(conflict.is_err());assert_eq!(app.read().await.unwrap(),reserved);
    assert!(app.db.create_proposal_observed(&json!({"itemId":"ready","kind":"close","expectedRevision":1}),&app.lifecycle_owner).await.is_err(),"same branch manual admission cannot escape paid owner");
    let independent=app.db.change_preparation_schedule_observed(|d|schedule_native_preparation(d,crate::engine_prepare::parse(&json!({"itemIds":["media"]}))?,&app.lifecycle_owner).map(|s|s.job_id)).await.unwrap().0;
    assert_ne!(independent,reserved_job,"independent next preparation overlaps safely");
    let bound=app.db.read_preparation_context(reserved_job).await.unwrap();
    assert_eq!(bound["scopeOwners"].as_array().unwrap().len(),2);
    assert!(bound["scopeOwners"].to_string().len()<reserved["jobs"].to_string().len(),"cold result/request history stays outside ownership controls");
    app.db.close().await;
}
