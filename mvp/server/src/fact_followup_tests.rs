use super::*;
// These settled multi-post parents model paid single-pass captures from before
// strict-family selection. Fresh continuations below use the native scheduler.
// Keep their historical request explicit rather than downgrading a new capture.
fn historical_parent(d:&mut Value)->String{
    for post in list_mut(d,"posts"){if post.get("attachments").is_none(){post["attachments"]=json!([]);}}
    let ids=vec![json!("ready"),json!("media")];
    let mut bundle=crate::prepare_bundle::build_engine_capture(d,&ids,&[]).unwrap();
    bundle["request"]["purpose"]=json!("triage");
    for (key,value) in [("preparationMode","single_pass_v1"),("responseContract","compact_decisions_v1"),
        ("modelContextContract","shared_moderation_v1"),("researchPolicy","context_sufficient_v1"),
        ("researchLimitContract","uncapped_evidence_v1"),("recoveryEvidenceContract","held_candidates_v1"),
        ("factDependencyContract",CONTRACT)]{bundle["request"][key]=json!(value);}
    bundle["request"]["visualNeedContract"]=json!(crate::prepare_bundle::visual::CONTRACT);
    bundle["request"]["visualSelection"]=crate::prepare_bundle::visual::empty();
    crate::decision_media::attach_request(d,&mut bundle["request"]).unwrap();
    bundle["factSourceFingerprints"]=json!({});bundle["factFollowupManifest"]=json!([]);
    for id in &ids{bundle["factSourceFingerprints"][id.as_str().unwrap()]=json!(crate::prepare_bundle::review_fingerprint(d,id.as_str().unwrap()).unwrap());}
    bundle["digest"]=json!(hash(&bundle["request"]));
    let groups=crate::prepare_bundle::capture_groups(d,&bundle).unwrap();
    let run=crate::new_job(d,"assistant","engine_prepare").unwrap();
    let job=row_mut(d,"jobs",&run).unwrap();job["purpose"]=json!("engine_prepare");
    job["requestedItemIds"]=json!(ids);job["selectedItemIds"]=json!(ids);job["held"]=json!([]);
    job["prepareBundle"]=bundle;job["preparationStages"]=json!({"first":null,"review":null,"groupAdmission":groups});
    let reservation=crate::preparation_reservations::capture(d,&run).unwrap();
    row_mut(d,"jobs",&run).unwrap()["scopeReservation"]=reservation;run
}
fn fixture()->(Value,String){
    let mut d=crate::engine_prepare::tests::fixture(false);
    for key in ["knowledge_entries","knowledge_versions","feedback"]{if d.get(key).is_none(){d[key]=json!([]);}}
    let run=historical_parent(&mut d);
    let result=json!({"text":"Held one exact technical claim","sources":[],"proposals":[],
        "assessments":[{"itemId":"ready","outcome":"needs_attention","reason":"Exact technical fact missing","tags":["needs_fact"]},
            {"itemId":"media","outcome":"needs_attention","reason":"Private company order fact","tags":["needs_fact"]}],
        "factDependencies":[{"itemId":"ready","kind":"missing_public_fact","claimScope":"Exact public technical operating temperature","publicQuery":"manufacturer exact public technical operating temperature"},
            {"itemId":"media","kind":"private_company_fact","claimScope":"Private current company order state","publicQuery":null}]});
    let outcome=crate::engine_prepare::tests::complete_first_fixture(&mut d,&run,&result);
    assert_eq!(outcome["status"],"held");
    (d,run)
}
fn result(request:&Value)->Value{
    let input=format!("{{\"query\":{},\"factResearchContract\":\"{}\"}}",request["query"],SOURCE_CONTRACT);
    json!({"text":"Supported source-only claim","sources":[{"url":"https://manufacturer.example/specification","title":"Exact manufacturer source","claim":"Exact operating temperature is supported","claimKind":"source_statement","trust":"source_only"}],
        "runMetadata":{"version":1,"status":"completed","model":codex_model_policy::MODEL,"modelProfile":codex_model_policy::PROFILE,
            "reasoningEffort":"medium","promptVersion":"communityhero-discussion-public-research-v4-uncapped-evidence",
            "factResearchContract":SOURCE_CONTRACT,"webCallLimit":null,"webCalls":14,"completedAt":now(),"elapsedMs":5,
            "inputSha256":format!("{:x}",Sha256::digest(input.as_bytes())),"instructionSha256":"a".repeat(64),"cliSha256":codex_model_policy::CLI_SHA256}})
}
fn launch(d:&mut Value,parent:&str)->String{
    let token=fact_test_token(d);let actor=operator_auth::Actor::local_owner("csrf");
    let (_,ids)=schedule_research_admitted(d,parent,&[json!("ready")],Some(&actor),&token).unwrap();assert_eq!(ids.len(),1);
    reserve_research_admission(d,&token,&ids[0],&now()).unwrap();ids[0].clone()
}
fn fact_test_token(d:&mut Value)->crate::runtime_lifecycle::OwnerToken {
    if d.get("runtimeLifecycle").is_none(){fact_lifecycle(d)}
    else{crate::runtime_lifecycle::admission_token(d,crate::runtime_lifecycle::AdmissionClass::Preparation).unwrap()}
}
fn schedule_preparation_fixture_admitted(d:&mut Value,input:engine_prepare::Input)->ApiResult<engine_prepare::Scheduled> {
    let token=fact_test_token(d);
    let scheduled=engine_prepare::schedule(d,input)?;
    crate::preparation_review::record_initial_admission(d,&token,&scheduled.job_id,&now())?;
    Ok(scheduled)
}
fn complete(d:&mut Value,parent:&str,id:&str){
    let job=row(d,"jobs",id).unwrap().clone();
    let entries=rows(row(d,"jobs",parent).unwrap(),"factFollowups").iter().filter(|e|rows(&job,"factDependencyIds").contains(&e["id"])).cloned().collect::<Vec<_>>();
    let admitted=admit_sources(&result(&job["researchRequest"]),&job["researchRequest"]).unwrap();
    settle(d,id,&job,&entries,&admitted).unwrap();row_mut(d,"jobs",id).unwrap()["status"]=json!("completed");
}

#[test]
fn typed_private_hold_never_dispatches_and_lost_ack_recovers_original_attempt(){
    let (mut d,parent)=fixture();let actor=operator_auth::Actor::local_owner("csrf");
    let (private,launches)=schedule(&mut d,&parent,&[json!("media")],&actor).unwrap();assert!(launches.is_empty());assert!(private["readyItemIds"].as_array().unwrap().is_empty());
    let id=launch(&mut d,&parent);let before=list(&d,"jobs").len();
    let (recovered,new)=schedule(&mut d,&parent,&[json!("ready")],&actor).unwrap();assert!(new.is_empty());assert_eq!(recovered["jobIds"],json!([id]));assert_eq!(list(&d,"jobs").len(),before);
    let mut public=row(&d,"jobs",&parent).unwrap().clone();conductor_authority::sanitize_job(&mut public);
    assert_eq!(public["factDependencies"][0]["lastResearchJobId"],id);assert!(public.get("factFollowups").is_none());
}

#[test]
fn interrupted_lookup_has_one_precise_retry_and_terminal_failure_does_not_restart(){
    let (mut d,parent)=fixture();let first=launch(&mut d,&parent);row_mut(&mut d,"jobs",&first).unwrap()["status"]=json!("interrupted");
    let second=launch(&mut d,&parent);assert_ne!(first,second);row_mut(&mut d,"jobs",&second).unwrap()["status"]=json!("interrupted");
    let (out,launches)=schedule(&mut d,&parent,&[json!("ready")],&operator_auth::Actor::local_owner("csrf")).unwrap();
    assert!(launches.is_empty());assert_eq!(out["held"].as_array().unwrap().len(),1);assert_eq!(rows(&row(&d,"jobs",&parent).unwrap()["factFollowups"][0],"attempts").len(),2);
}

#[test]
fn source_receipt_pins_survive_fresh_draft_workflow_revision_and_never_approve_old_work(){
    let (mut d,parent)=fixture();let id=launch(&mut d,&parent);complete(&mut d,&parent,&id);
    let selected=select(&d,&[json!("ready"),json!("media")],&now()).unwrap();assert_eq!(rows(&selected,"materials").len(),1);assert_eq!(selected["materials"][0]["itemIds"],json!(["ready"]));
    assert!(list(&d,"proposals").is_empty());assert!(list(&d,"approvals").is_empty());
    let fresh=crate::engine_prepare::schedule(&mut d,crate::engine_prepare::Input{item_ids:vec!["ready".into()],instruction:None}).unwrap();
    let bundle=row(&d,"jobs",&fresh.job_id).unwrap()["prepareBundle"].clone();
    assert_eq!(rows(&bundle,"factFollowupManifest").len(),1);assert!(rows(&select(&d,&[json!("ready")],&now()).unwrap(),"materials").is_empty());
    let before=crate::prepare_bundle::review_fingerprint(&d,"ready").unwrap();
    crate::create_generated_proposal(&mut d,&json!({"itemId":"ready","kind":"reply_and_close","text":"Supported fresh reply","expectedRevision":1,"sources":[]})).unwrap();
    assert_eq!(row(&d,"items","ready").unwrap()["workflow"],"prepared");assert_eq!(row(&d,"items","ready").unwrap()["revision"],2);
    assert_eq!(before,crate::prepare_bundle::review_fingerprint(&d,"ready").unwrap());
    current(&d,&bundle["factFollowupManifest"],&[json!("ready")],&now()).unwrap();
    assert!(list(&d,"approvals").is_empty());
    let mut changed=d.clone();row_mut(&mut changed,"posts","ready-post").unwrap()["text"]=json!("Changed public source context");
    assert!(current(&changed,&bundle["factFollowupManifest"],&[json!("ready")],&now()).is_err());
    assert!(current(&d,&bundle["factFollowupManifest"],&[json!("ready")],&(chrono::Utc::now()+chrono::Duration::days(2)).to_rfc3339()).is_err());
}

#[test]
fn unknown_late_source_and_company_rebinding_cannot_wake_dependency(){
    let (mut d,parent)=fixture();let id=launch(&mut d,&parent);
    let target=row(&d,"items","ready").unwrap().clone();
    list_mut(&mut d,"operations").push(json!({"id":"uncertain","itemId":"ready","status":"unknown","target":target}));
    complete(&mut d,&parent,&id);assert_eq!(row(&d,"jobs",&parent).unwrap()["factFollowups"][0]["status"],"stale");
    assert!(row(&d,"jobs",&id).unwrap()["researchResult"].is_object());assert!(rows(&select(&d,&[json!("ready")],&now()).unwrap(),"materials").is_empty());
    let (mut d,parent)=fixture();let id=launch(&mut d,&parent);complete(&mut d,&parent,&id);
    d["account"]=json!("BAW Russia");assert!(rows(&select(&d,&[json!("ready")],&now()).unwrap(),"materials").is_empty());
}

#[test]
fn declaration_binding_uses_captured_source_and_utf16_bounds_without_prose_guessing(){
    let mut d=crate::engine_prepare::tests::fixture(false);let scheduled=crate::engine_prepare::schedule(&mut d,crate::engine_prepare::Input{item_ids:vec!["ready".into()],instruction:None}).unwrap();
    let request=row(&d,"jobs",&scheduled.job_id).unwrap()["prepareBundle"]["request"].clone();
    row_mut(&mut d,"posts","ready-post").unwrap()["text"]=json!("Changed during first paid pass");
    let held=json!({"assessments":[{"itemId":"ready","outcome":"needs_attention"}],"proposals":[],"factDependencies":[{"itemId":"ready","kind":"missing_public_fact","claimScope":"Captured technical claim","publicQuery":"я".repeat(600)}]});
    record(&mut d,&scheduled.job_id,&request,&held,&now()).unwrap();assert_eq!(row(&d,"jobs",&scheduled.job_id).unwrap()["factFollowups"][0]["status"],"stale");
    let mut tag=held.clone();tag.as_object_mut().unwrap().remove("factDependencies");assert!(declarations(&request,&tag).unwrap().is_empty());
    assert_eq!(declarations(&request,&held).unwrap().len(),1);
    let mut historical=request.clone();historical.as_object_mut().unwrap().remove("preparationMode");
    assert!(declarations(&historical,&held).is_err(),"known selector cannot authorize facts outside captured single pass");
    historical.as_object_mut().unwrap().remove("factDependencyContract");
    assert!(declarations(&historical,&tag).unwrap().is_empty(),"absent historical selector remains inert");
    historical["factDependencyContract"]=json!("unknown");
    assert!(declarations(&historical,&tag).is_err(),"unsupported present selector still rejects an empty declaration");
}

#[test]
fn consumed_lookup_identity_ignores_local_revision_bumps_on_same_public_claim(){
    let (mut d,parent)=fixture();let lookup=launch(&mut d,&parent);complete(&mut d,&parent,&lookup);
    let declaration=row(&d,"jobs",&parent).unwrap()["factFollowups"][0]["binding"]["declaration"].clone();
    row_mut(&mut d,"items","ready").unwrap()["revision"]=json!(2);
    let fresh=crate::engine_prepare::schedule(&mut d,crate::engine_prepare::Input{item_ids:vec!["ready".into()],instruction:None}).unwrap();
    let req=row(&d,"jobs",&fresh.job_id).unwrap()["prepareBundle"]["request"].clone();
    record(&mut d,&fresh.job_id,&req,&json!({"assessments":[{"itemId":"ready","outcome":"needs_attention"}],"proposals":[],"factDependencies":[declaration]}),&now()).unwrap();
    let repeated=&row(&d,"jobs",&fresh.job_id).unwrap()["factFollowups"][0];
    assert_eq!(repeated["status"],"held");assert_eq!(repeated["reason"],"public_fact_attempt_already_consumed");
}

#[tokio::test]
async fn pause_after_lookup_dispatch_preserves_paid_receipt_without_fresh_work(){
    let (mut d,parent)=fixture();let actor=operator_auth::Actor::local_owner("csrf");
    let input=json!({"mode":"prepare","scope":{"itemIds":["ready","media"]},"actionKinds":[]});
    let grant=conductor_authority::create_grant(&d,&actor,&input).unwrap();
    let campaign=new_job(&mut d,"conductor","pause-fixture").unwrap();let binding=active_binding(&d).unwrap().to_json();let account=d["account"].clone();
    let job=row_mut(&mut d,"jobs",&campaign).unwrap();job["account"]=account;job["connectorBinding"]=binding;
    job["conductor"]=json!({"version":1,"desiredState":"running","leaseGeneration":1,"mode":"prepare","scope":input["scope"],"grant":grant});
    let parent_job=row_mut(&mut d,"jobs",&parent).unwrap();parent_job["conductorRunId"]=json!(campaign);parent_job["grantGeneration"]=json!(1);
    let context=conductor_authority::Context{run_id:campaign.clone(),lease_generation:1,actor};
    conductor_authority::with_context(context,async {
        let id=launch(&mut d,&parent);let job=row(&d,"jobs",&id).unwrap().clone();
        let entries=rows(row(&d,"jobs",&parent).unwrap(),"factFollowups").iter().filter(|e|rows(&job,"factDependencyIds").contains(&e["id"])).cloned().collect::<Vec<_>>();
        conductor_authority::fence_job_capture(&d,&id,"prepare").unwrap();
        let source=admit_sources(&result(&job["researchRequest"]),&job["researchRequest"]).unwrap();
        row_mut(&mut d,"jobs",&campaign).unwrap()["conductor"]["desiredState"]=json!("paused");
        settle(&mut d,&id,&job,&entries,&source).unwrap();
        assert!(row(&d,"jobs",&id).unwrap()["researchResult"].is_object());
        assert_eq!(row(&d,"jobs",&parent).unwrap()["factFollowups"][0]["status"],"stale");
        assert!(list(&d,"approvals").is_empty());assert!(list(&d,"operations").is_empty());
        assert!(schedule(&mut d,&parent,&[json!("ready")],&operator_auth::Actor::local_owner("csrf")).is_err());
    }).await;
}

#[test]
fn compatible_exact_dependencies_share_lookup_but_distinct_claim_scope_does_not(){
    let mut d=crate::engine_prepare::tests::fixture(false);
    row_mut(&mut d,"items","media").unwrap()["postId"]=json!("ready-post");row_mut(&mut d,"items","media").unwrap()["postKey"]=json!("ready-post");
    row_mut(&mut d,"branches","media-branch").unwrap()["postId"]=json!("ready-post");
    let prepared=crate::engine_prepare::schedule(&mut d,crate::engine_prepare::Input{item_ids:vec!["ready".into(),"media".into()],instruction:None}).unwrap();
    let request=row(&d,"jobs",&prepared.job_id).unwrap()["prepareBundle"]["request"].clone();
    let declaration=|id|json!({"itemId":id,"kind":"missing_public_fact","claimScope":"Exact public operating-temperature specification","publicQuery":"manufacturer public operating temperature specification"});
    record(&mut d,&prepared.job_id,&request,&json!({"assessments":[{"itemId":"ready","outcome":"needs_attention"},{"itemId":"media","outcome":"needs_attention"}],"proposals":[],"factDependencies":[declaration("ready"),declaration("media")]}),&now()).unwrap();
    row_mut(&mut d,"jobs",&prepared.job_id).unwrap()["status"]=json!("completed");
    let (_,launches)=schedule(&mut d,&prepared.job_id,&[json!("ready"),json!("media")],&operator_auth::Actor::local_owner("csrf")).unwrap();
    assert_eq!(launches.len(),1);assert_eq!(row(&d,"jobs",&launches[0]).unwrap()["requestedItemIds"],json!(["ready","media"]));
    let a=&row(&d,"jobs",&prepared.job_id).unwrap()["factFollowups"][0];let mut b=a.clone();b["binding"]["declaration"]["claimScope"]=json!("Different indispensable technical claim");
    assert_ne!(group_key(a),group_key(&b));
}

#[tokio::test]
async fn durable_attempt_receipt_and_consumption_survive_sqlite_reopen(){
    let (mut d,parent)=fixture();let lookup=launch(&mut d,&parent);complete(&mut d,&parent,&lookup);
    let folder=tempfile::tempdir().unwrap();let path=folder.path().join("facts.sqlite");let db=Database::Sqlite(open_db(&path).await.unwrap());
    db.change(|target|{*target=d.clone();Ok(())}).await.unwrap();db.close().await;
    let db=Database::Sqlite(open_db(&path).await.unwrap());
    let preview=db.read_preparation_schedule().await.unwrap();assert!(row(&preview,"jobs",&parent).is_ok());
    let scheduled=db.change_preparation_schedule_observed(|target|schedule_preparation_fixture_admitted(target,crate::engine_prepare::Input{item_ids:vec!["ready".into()],instruction:None})).await.unwrap().0;
    db.close().await;let db=Database::Sqlite(open_db(&path).await.unwrap());let restored=db.read().await.unwrap();
    assert_eq!(row(&restored,"jobs",&parent).unwrap()["factFollowups"][0]["consumedByJobId"],scheduled.job_id);
    assert!(row(&restored,"jobs",&lookup).unwrap()["researchResult"].is_object());
    assert!(rows(&select(&restored,&[json!("ready")],&now()).unwrap(),"materials").is_empty());db.close().await;
}

#[tokio::test]
#[ignore = "requires a pristine explicitly isolated communityhero_writer_v51_test_ PostgreSQL fixture"]
async fn postgres_fact_receipt_projection_and_consumption_survive_reopen(){
    let db=crate::storage::writer_v51_fixture_db().await;
    let (initial,parent)=fixture();
    db.change(|d|{*d=initial.clone();Ok(())}).await.unwrap();
    let lookup=db.change(|d|Ok(launch(d,&parent))).await.unwrap();
    db.change(|d|{complete(d,&parent,&lookup);Ok(())}).await.unwrap();
    let resolved=db.read().await.unwrap();
    let evidence=row(&resolved,"jobs",&parent).unwrap()["factFollowups"][0]["evidence"].clone();
    let lookup_result=row(&resolved,"jobs",&lookup).unwrap()["researchResult"].clone();
    let preview=db.read_preparation_schedule().await.unwrap();
    assert_eq!(row(&preview,"jobs",&parent).unwrap()["factFollowups"][0]["status"],"resolved");
    assert_eq!(rows(&select(&preview,&[json!("ready")],&now()).unwrap(),"materials").len(),1);
    let (scheduled,changed)=db.change_preparation_schedule_observed(|d|schedule_preparation_fixture_admitted(d,crate::engine_prepare::Input{item_ids:vec!["ready".into()],instruction:None})).await.unwrap();
    assert!(changed);
    let bound=db.read_preparation_context(&scheduled.job_id).await.unwrap();
    let bundle=row(&bound,"jobs",&scheduled.job_id).unwrap()["prepareBundle"].clone();
    assert_eq!(rows(&bundle,"factFollowupManifest").len(),1);
    assert_eq!(row(&bound,"jobs",&parent).unwrap()["factFollowups"][0]["consumedByJobId"],scheduled.job_id);
    current(&bound,&bundle["factFollowupManifest"],&[json!("ready")],&now()).unwrap();
    db.close().await;
    let url=std::env::var("COMMUNITYHERO_WRITER_V51_TEST_URL").expect("guarded isolated fixture URL");
    let reopened=Database::postgres(&url).await.unwrap();
    let restored=reopened.read().await.unwrap();
    let entry=&row(&restored,"jobs",&parent).unwrap()["factFollowups"][0];
    assert_eq!(entry["consumedByJobId"],scheduled.job_id);
    assert_eq!(entry["evidence"],evidence,"consumption must preserve immutable paid source evidence");
    assert_eq!(row(&restored,"jobs",&lookup).unwrap()["researchResult"],lookup_result);
    assert!(rows(&select(&restored,&[json!("ready")],&now()).unwrap(),"materials").is_empty());
    current(&restored,&bundle["factFollowupManifest"],&[json!("ready")],&now()).unwrap();
    assert!(list(&restored,"approvals").is_empty());assert!(list(&restored,"operations").is_empty());
    reopened.close().await;
}

#[tokio::test]
async fn captured_first_stage_and_typed_dependency_commit_in_one_narrow_storage_scope(){
    let folder=tempfile::tempdir().unwrap();let path=folder.path().join("first-facts.sqlite");let db=Database::Sqlite(open_db(&path).await.unwrap());
    let mut initial=crate::engine_prepare::tests::fixture(false);
    initial["posts"][0]["attachments"]=json!([]);
    for key in ["knowledge_entries","knowledge_versions","feedback"]{if initial.get(key).is_none(){initial[key]=json!([]);}}
    let prepared=crate::engine_prepare::schedule(&mut initial,crate::engine_prepare::Input{item_ids:vec!["ready".into()],instruction:None}).unwrap();
    let run=prepared.job_id;
    let mut response=json!({"text":"One missing public fact","sources":[],"proposals":[],
        "assessments":[{"itemId":"ready","outcome":"needs_attention","reason":"Exact technical specification missing","tags":["needs_fact"]}],
        "factDependencies":[{"itemId":"ready","kind":"missing_public_fact","claimScope":"Exact public technical specification","publicQuery":"manufacturer exact public technical specification"}],
        "editorialEvidence":{"version":1,"contract":crate::editorial_review::CONTRACT,"entries":[]},
        "runMetadata":{"schemaVersion":1,"model":codex_model_policy::MODEL,"modelProfile":codex_model_policy::PROFILE,"reasoningEffort":"high",
            "promptVersion":"communityhero-preparation-v1-single-pass","instructionSha256":"a".repeat(64),"inputSha256":"b".repeat(64),
            "cliSha256":codex_model_policy::CLI_SHA256,"elapsedMs":1,"completedAt":now(),"researchLimitContract":"uncapped_evidence_v1",
            "decisionDependencies":{"version":1,"entries":[{"itemId":"ready","dependsOnItemIds":[]}]}}});
    let request=row(&initial,"jobs",&run).unwrap()["prepareBundle"]["request"].clone();
    crate::model_material_receipt::fixture_result(&mut initial,&run,&request,&mut response).unwrap();
    db.change(|d|{*d=initial.clone();Ok(())}).await.unwrap();
    db.change_preparation_first_observed(&run,|d|crate::preparation_review::record_first(d,&run,&response,&now())).await.unwrap();
    db.close().await;let db=Database::Sqlite(open_db(&path).await.unwrap());let restored=db.read().await.unwrap();
    let job=row(&restored,"jobs",&run).unwrap();assert_eq!(job["preparationStages"]["first"]["status"],"completed");assert_eq!(job["factFollowups"][0]["kind"],"missing_public_fact");
    let before=restored.clone();assert!(db.change_preparation_first_observed(&run,|d|{row_mut(d,"jobs",&run)?["factFollowups"][0]["binding"]["declaration"]["publicQuery"]=json!("altered query");Ok(())}).await.is_err());
    assert_eq!(db.read().await.unwrap(),before);db.close().await;
}

#[test]
fn source_admission_preserves_quality_provenance_but_rejects_foreign_company_hold(){
    let request=json!({"account":"BAW Russia","query":"public technical query","factResearchContract":SOURCE_CONTRACT});
    let mut raw=result(&request);raw["runMetadata"]["unexpectedPrivateField"]=json!("discard");
    let admitted=admit_sources(&raw,&request).unwrap();assert!(admitted["runMetadata"].get("unexpectedPrivateField").is_none());
    assert_eq!(admitted["sources"][0]["claimKind"],"source_statement");
    raw["sources"]=json!([]);raw["runMetadata"]["status"]=json!("no_sources");
    raw["evidenceHolds"]=json!([{"version":1,"accountKey":"likeavto","itemId":"public-query","url":"https://manufacturer.example/specification",
        "reason":"incomplete_extraction","extraction":{"status":"access_challenge"},"renderedFallback":{"status":"access_challenge","attempts":0,"capability":"web.run_text_only"}}]);
    assert!(admit_sources(&raw,&request).is_err());raw["evidenceHolds"][0]["accountKey"]=json!("baw-russia");
    assert_eq!(admit_sources(&raw,&request).unwrap()["evidenceHolds"][0]["extraction"]["status"],"access_challenge");
}

// Native automatic parents use the same durable receipts as explicit work.
// These fixtures opt in at capture, before any follow-up research exists.
fn automatic_fixture(two_public:bool)->(Value,String){
    let mut d=crate::engine_prepare::tests::fixture(false);
    for key in ["knowledge_entries","knowledge_versions","feedback"]{if d.get(key).is_none(){d[key]=json!([]);}}
    d["settings"]["autoPreparation"]["publicFactFollowup"]=json!({"version":1,"enabled":true});
    for item in list_mut(&mut d,"items"){item["providerObservedAt"]=json!(now());}
    let run=historical_parent(&mut d);
    let declaration=|id,public|json!({"itemId":id,"kind":if public{"missing_public_fact"}else{"private_company_fact"},
        "claimScope":"Exact public technical specification","publicQuery":if public{json!("manufacturer exact public technical specification")}else{Value::Null}});
    engine_prepare::tests::complete_first_fixture(&mut d,&run,&json!({"text":"Exact public facts needed","sources":[],"proposals":[],
        "assessments":[{"itemId":"ready","outcome":"needs_attention","reason":"Exact technical fact missing","tags":["needs_fact"]},
            {"itemId":"media","outcome":"needs_attention","reason":"Additional exact fact missing","tags":["needs_fact"]}],
        "factDependencies":[declaration("ready",true),declaration("media",two_public)]}));
    let parent=row_mut(&mut d,"jobs",&run).unwrap();parent["purpose"]=json!("auto_prepare");
    parent["prepareOutcome"]=json!({"items":[{"itemId":"ready","status":"needs_attention"},{"itemId":"media","status":"needs_attention"}]});
    let policy=capture_automatic_policy(&d,row(&d,"jobs",&run).unwrap()).unwrap();
    row_mut(&mut d,"jobs",&run).unwrap()["automaticFactPolicy"]=policy;
    for item in list_mut(&mut d,"items"){item["autoPreparation"]=json!({"jobId":run,"status":"needs_attention","attempts":1});}
    assert_eq!(automatic_selection(&d,&now(),2).unwrap().ids,vec![json!("ready")]);
    (d,run)
}
fn automatic_launch(d:&mut Value,parent:&str,id:&str)->String{
    let token=fact_test_token(d);let (_,jobs)=schedule_research_admitted(d,parent,&[json!(id)],None,&token).unwrap();assert_eq!(jobs.len(),1);
    reserve_research_admission(d,&token,&jobs[0],&now()).unwrap();jobs[0].clone()
}
fn automatic_resolved(two_public:bool)->(Value,String){
    let (mut d,parent)=automatic_fixture(two_public);
    for id in if two_public{vec!["ready","media"]}else{vec!["ready"]}{let run=automatic_launch(&mut d,&parent,id);complete(&mut d,&parent,&run);}
    (d,parent)
}

#[test]
fn automatic_facts_require_captured_native_parent_and_exact_current_recipient(){
    for change in ["opt_out","missing_policy","unfinished","conductor","account","binding","changed_source","human_draft","human_override","unknown"]{
        let (mut d,parent)=automatic_fixture(false);
        match change{
            "opt_out"=>d["settings"]["autoPreparation"]["publicFactFollowup"]["enabled"]=json!(false),
            "missing_policy"=>{row_mut(&mut d,"jobs",&parent).unwrap().as_object_mut().unwrap().remove("automaticFactPolicy");},
            "unfinished"=>row_mut(&mut d,"jobs",&parent).unwrap()["status"]=json!("interrupted"),
            "conductor"=>row_mut(&mut d,"jobs",&parent).unwrap()["conductorRunId"]=json!("foreign-owner"),
            "account"=>d["account"]=json!("BAW Russia"),
            "binding"=>d["connectorBinding"]=json!({"different":true}),
            "changed_source"=>row_mut(&mut d,"posts","ready-post").unwrap()["text"]=json!("A different public claim"),
            "human_draft"=>row_mut(&mut d,"items","ready").unwrap()["draft"]=json!("Owner draft"),
            "human_override"=>row_mut(&mut d,"items","ready").unwrap()["autoPreparation"]["humanOverrideAt"]=json!(now()),
            _=>{let target=row(&d,"items","ready").unwrap().clone();list_mut(&mut d,"operations").push(json!({"id":"old-unknown","itemId":"removed-alias","target":target,"status":"unknown"}));},
        }
        let before=d.clone();assert!(automatic_selection(&d,&now(),2).is_none(),"{change}");assert_eq!(d,before);
    }
}

#[test]
fn automatic_research_has_one_attempt_even_after_interruption_and_rejects_multigroup(){
    let (mut d,parent)=automatic_fixture(true);let before=d.clone();
    assert!(schedule_inner(&mut d,&parent,&[json!("ready"),json!("media")],None).is_err());assert_eq!(d,before);
    let first=automatic_launch(&mut d,&parent,"ready");
    for status in ["running","interrupted","cancelled","failed","completed"]{
        row_mut(&mut d,"jobs",&first).unwrap()["status"]=json!(status);
        let before=d.clone();
        if status=="running"{assert!(schedule_inner(&mut d,&parent,&[json!("ready")],None).is_err());}
        else{assert!(schedule_inner(&mut d,&parent,&[json!("ready")],None).unwrap().1.is_empty());}
        assert_eq!(d,before,"{status}");
        assert_eq!(rows(&row(&d,"jobs",&parent).unwrap()["factFollowups"][0],"attempts").len(),1);
    }
    assert_eq!(automatic_selection(&d,&now(),2).unwrap().ids,vec![json!("media")]);
}

#[test]
fn automatic_continuation_consumes_exact_manifest_once_and_keeps_parent_receipt(){
    let (mut d,parent)=automatic_resolved(false);let at=now();let before=d.clone();
    let selection=automatic_selection(&d,&at,2).unwrap();assert!(selection.resolved);
    let mut preview=d.clone();let expected=schedule_automatic_continuation(&mut preview,&selection,2,&at).unwrap();
    let child=schedule_automatic_continuation(&mut d,&selection,2,&at).unwrap();
    engine_prepare::capacity::same_capture(expected.request(),child.request()).unwrap();
    let job=row(&d,"jobs",&child.job_id).unwrap();
    validate_automatic_schedule(&before,&d,job).unwrap();
    assert_eq!(job["selectedItemIds"],json!(["ready"]));assert_eq!(rows(&job["prepareBundle"],"factFollowupManifest").len(),1);
    assert_eq!(row(&d,"jobs",&parent).unwrap()["preparationStages"],row(&before,"jobs",&parent).unwrap()["preparationStages"]);
    assert_eq!(row(&d,"jobs",&parent).unwrap()["prepareOutcome"],row(&before,"jobs",&parent).unwrap()["prepareOutcome"]);
    let committed=d.clone();assert!(schedule_automatic_continuation(&mut d,&selection,2,&at).is_err());assert_eq!(d,committed);
    for change in ["empty_manifest","empty_ids","foreign_dependency","wrong_owner","wrong_width"]{
        let mut invalid=d.clone();let job=row_mut(&mut invalid,"jobs",&child.job_id).unwrap();
        match change{
            "empty_manifest"=>job["prepareBundle"]["factFollowupManifest"]=json!([]),
            "empty_ids"=>job["selectedItemIds"]=json!([]),
            "foreign_dependency"=>job["automaticFactContinuation"]["dependencyIds"]=json!(["foreign"]),
            "wrong_owner"=>job["automaticFactContinuation"]["parentPrepareJobId"]=json!(child.job_id),
            _=>job["automaticFactContinuation"]["admissionWidth"]=json!(9),
        }
        assert!(automatic_continuation_current(&invalid,row(&invalid,"jobs",&child.job_id).unwrap()).is_err(),"{change}");
    }
    assert!(list(&d,"approvals").is_empty());assert!(list(&d,"operations").is_empty());
}

#[test]
fn automatic_fact_capacity_narrowing_preserves_sibling_evidence_and_ownership(){
    let (mut d,parent)=automatic_resolved(true);let at=now();let selection=automatic_selection(&d,&at,2).unwrap();
    assert_eq!(selection.ids.len(),2);assert!(selection.narrowed(&[]).is_none());assert!(selection.narrowed(&[json!("foreign")]).is_none());
    assert!(selection.narrowed(&[json!("ready"),json!("ready")]).is_none());
    let evidence=row(&d,"jobs",&parent).unwrap()["factFollowups"].clone();
    capacity_holds(&mut d,&selection,&[json!("ready")],&at).unwrap();
    let narrowed=selection.narrowed(&[json!("media")]).unwrap();
    let child=schedule_automatic_continuation(&mut d,&narrowed,2,&at).unwrap();
    let facts=&row(&d,"jobs",&parent).unwrap()["factFollowups"];
    assert_eq!(facts[0]["status"],"held");assert!(facts[0]["consumedByJobId"].is_null());assert_eq!(facts[0]["evidence"],evidence[0]["evidence"]);
    assert_eq!(facts[1]["consumedByJobId"],child.job_id);assert_eq!(facts[1]["evidence"],evidence[1]["evidence"]);
}

#[tokio::test]
async fn automatic_fact_atomic_schedule_sees_other_active_jobs_and_survives_restart(){
    let (d,parent)=automatic_resolved(false);let folder=tempfile::tempdir().unwrap();let path=folder.path().join("automatic-facts.sqlite");
    let db=Database::Sqlite(open_db(&path).await.unwrap());db.change(|target|{*target=d.clone();Ok(())}).await.unwrap();
    let at=now();let selection=automatic_selection(&d,&at,1).unwrap();
    // A different ordinary owner wins after preview. The narrow Schedule must
    // see that full active job, not only its compact reservation owner.
    let other=db.change_preparation_schedule_observed(|target|schedule_preparation_fixture_admitted(target,engine_prepare::Input{item_ids:vec!["media".into()],instruction:None})).await.unwrap().0.job_id;
    assert_eq!(row(&db.read_preparation_schedule().await.unwrap(),"jobs",&other).unwrap()["purpose"],"engine_prepare");
    let before=db.read().await.unwrap();
    assert!(db.change_preparation_schedule_observed(|target|{let token=fact_test_token(target);schedule_continuation_admitted(target,&selection,1,&at,&token)}).await.is_err());
    assert_eq!(db.read().await.unwrap(),before,"full pool defers without consuming an identity");
    db.change(|target|{row_mut(target,"jobs",&other)?["status"]=json!("completed");Ok(())}).await.unwrap();
    let child=db.change_preparation_schedule_observed(|target|{let token=fact_test_token(target);schedule_continuation_admitted(target,&selection,1,&at,&token)}).await.unwrap().0.job_id;
    db.close().await;let db=Database::Sqlite(open_db(&path).await.unwrap());let restored=db.read().await.unwrap();
    assert_eq!(row(&restored,"jobs",&parent).unwrap()["factFollowups"][0]["consumedByJobId"],child);
    let before=restored.clone();assert!(db.change_preparation_schedule_observed(|target|{let token=fact_test_token(target);schedule_continuation_admitted(target,&selection,1,&at,&token)}).await.is_err());
    assert_eq!(db.read().await.unwrap(),before);db.close().await;
}

#[test]
fn automatic_resolved_tail_skips_busy_family_and_recovers_independent_sibling(){
    let (mut d,parent)=automatic_resolved(true);
    let mut sibling=row(&d,"items","ready").unwrap().clone();
    sibling["id"]=json!("busy-sibling");sibling["itemId"]=json!("c-sibling");
    sibling["branchId"]=json!("sibling-branch");sibling["conversationKey"]=json!("sibling-thread");
    sibling.as_object_mut().unwrap().remove("autoPreparation");list_mut(&mut d,"items").push(sibling);
    list_mut(&mut d,"branches").push(json!({"id":"sibling-branch","postId":"ready-post","messages":[{"id":"c-sibling","text":"Another question"}],"contextComplete":true}));
    let busy=engine_prepare::schedule(&mut d,engine_prepare::Input{item_ids:vec!["busy-sibling".into()],instruction:None}).unwrap().job_id;
    let selection=automatic_selection(&d,&now(),2).unwrap();assert_eq!(selection.ids,vec![json!("media")]);
    let child=schedule_automatic_continuation(&mut d,&selection,2,&now()).unwrap().job_id;
    assert_eq!(row(&d,"jobs",&parent).unwrap()["factFollowups"][1]["consumedByJobId"],child);
    assert!(row(&d,"jobs",&parent).unwrap()["factFollowups"][0]["consumedByJobId"].is_null());
    assert_eq!(row(&d,"jobs",&busy).unwrap()["status"],"running");
}

#[test]
fn automatic_retained_first_receipt_keeps_static_evidence_without_reopening_paid_first(){
    let (mut d,_parent)=automatic_resolved(false);let at=now();let selection=automatic_selection(&d,&at,2).unwrap();
    let child=schedule_automatic_continuation(&mut d,&selection,2,&at).unwrap().job_id;
    engine_prepare::tests::complete_first_fixture(&mut d,&child,&json!({"text":"Supported reply","sources":[],
        "assessments":[{"itemId":"ready","outcome":"reply","reason":"Supported source"}],
        "proposals":[{"itemId":"ready","kind":"reply_and_close","text":"Спасибо за вопрос!"}]}));
    assert!(automatic_continuation_current(&d,row(&d,"jobs",&child).unwrap()).is_ok());
    row_mut(&mut d,"posts","ready-post").unwrap()["text"]=json!("New post text after admission");
    assert!(automatic_continuation_current(&d,row(&d,"jobs",&child).unwrap()).is_ok(),"completed first uses immutable pins, normal proposal guards still own publication");
    let mut forged=d.clone();row_mut(&mut forged,"jobs",&child).unwrap()["preparationStages"]["first"]["reviewRequired"]=json!(true);
    assert!(automatic_continuation_current(&forged,row(&forged,"jobs",&child).unwrap()).is_err());
    let parent=row(&d,"jobs",&child).unwrap()["automaticFactContinuation"]["parentPrepareJobId"].as_str().unwrap().to_owned();
    row_mut(&mut d,"jobs",&parent).unwrap()["factFollowups"][0]["evidence"]["result"]["text"]=json!("Tampered retained source");
    assert!(automatic_continuation_current(&d,row(&d,"jobs",&child).unwrap()).is_err());
}

#[tokio::test]
async fn cancelled_automatic_lookup_after_gate_wait_never_dispatches_bridge(){
    let (app,_folder)=crate::tests::test_app().await;let (mut d,parent)=automatic_fixture(false);
    let token=app.lifecycle_admission_token(crate::runtime_lifecycle::AdmissionClass::Preparation).await.unwrap();
    let ledger=crate::runtime_lifecycle::ledger_digest(&d).unwrap();
    crate::runtime_lifecycle::initialize(&mut d,token.clone(),&"b".repeat(64),&ledger).unwrap();
    let lookup=schedule_research_admitted(&mut d,&parent,&[json!("ready")],None,&token).unwrap().1[0].clone();
    let initial=row(&d,"jobs",&lookup).unwrap()["factResearchInitialAdmission"].clone();
    assert!(row(&d,"jobs",&lookup).unwrap().get("factResearchAdmission").is_none());
    app.change(|target|{*target=d.clone();Ok(())}).await.unwrap();
    assert_eq!(app.lifecycle_admission_token(crate::runtime_lifecycle::AdmissionClass::Preparation).await.unwrap(),token);
    let gate=app.assistant_chat_gate.lock().await;
    let mut task=Box::pin(run(app.clone(),lookup.clone()));
    // run's first await is this held gate; the actual worker must wait here.
    std::future::poll_fn(|cx|{
        assert!(matches!(std::future::Future::poll(task.as_mut(),cx),std::task::Poll::Pending));
        std::task::Poll::Ready(())
    }).await;
    app.change(|target|{row_mut(target,"jobs",&lookup)?["status"]=json!("cancelled");Ok(())}).await.unwrap();
    drop(gate);let error=task.await.unwrap_err();
    assert_eq!(error.0,StatusCode::CONFLICT);assert!(error.1.contains("no longer active"));
    let state=app.read().await.unwrap();assert!(row(&state,"jobs",&lookup).unwrap()["researchResult"].is_null());
    assert_eq!(row(&state,"jobs",&lookup).unwrap()["factResearchInitialAdmission"],initial);
    assert!(row(&state,"jobs",&lookup).unwrap().get("factResearchAdmission").is_none());
    assert_eq!(rows(&row(&state,"jobs",&parent).unwrap()["factFollowups"][0],"attempts").len(),1);
    assert_eq!(state["operations"],d["operations"]);assert_eq!(state["approvals"],d["approvals"]);
    let native=app.lifecycle_work.snapshot().unwrap();assert_eq!(native.active,0);assert_eq!(native.unresolved,0);
    // test_app's nonexistent runtime makes an accidental bridge call fail with
    // a different error, so the exact guard receipt proves no dispatch.
}
