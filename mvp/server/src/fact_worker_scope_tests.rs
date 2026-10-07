// Included beside the existing fact fixtures: every claim below exercises the
// real automatic reducer, not a standalone replacement admission algorithm.
pub(super) fn fact_tail(d:&mut Value,id:&str,post:&str) {
    let mut item=row(d,"items","media").unwrap().clone();
    item["id"]=json!(id);item["itemId"]=json!(format!("comment-{id}"));
    item["postId"]=json!(post);item["postKey"]=json!(post);
    item["branchId"]=json!(format!("branch-{id}"));item["conversationKey"]=json!(format!("thread-{id}"));
    item["draft"]=json!("");item["workflow"]=json!("attention");
    item["providerObservedAt"]=json!(now());item["createdAt"]=json!(now());
    item.as_object_mut().unwrap().remove("autoPreparation");
    list_mut(d,"items").push(item);
    if !list(d,"posts").iter().any(|p|p["id"]==post) {
        list_mut(d,"posts").push(json!({"id":post,"postKey":post,"platform":"VK","text":"Independent public post","attachments":[]}));
    }
    list_mut(d,"branches").push(json!({"id":format!("branch-{id}"),"postId":post,"contextComplete":true,
        "messages":[{"id":format!("comment-{id}"),"text":"A fresh public question"}]}));
}
pub(super) fn active_fact_fixture()->(Value,String,String) {
    let (mut d,parent)=automatic_fixture(false);
    // A native paid parent has its captured input digest. The old synthetic
    // fixture omits it; fill it before lookup so claim never rewrites its owner.
    let digests:Vec<_>=["ready","media"].into_iter().map(|id|(id,prepare_bundle::fingerprint(&d,id).unwrap())).collect();
    for (id,digest) in digests {row_mut(&mut d,"items",id).unwrap()["autoPreparation"]["inputDigest"]=json!(digest);}
    let lookup=automatic_launch(&mut d,&parent,"ready");
    assert!(row(&d,"jobs",&lookup).unwrap()["factWorkerScope"].is_object());
    (d,parent,lookup)
}

#[test]
fn active_exact_research_allows_independent_automatic_claim_at_width_one() {
    let (mut d,parent,lookup)=active_fact_fixture();fact_tail(&mut d,"tail","tail-post");
    let paid=row(&d,"jobs",&parent).unwrap().clone();let research=row(&d,"jobs",&lookup).unwrap().clone();
    let admission=preparation_workers::AutomaticAdmission::capture(&d,1);
    assert!(admission.available());assert!(!admission.permits(&d,row(&d,"items","ready").unwrap()));
    let (job,request)=auto_prepare::claim(&mut d,timestamp(&now()).unwrap()).unwrap().expect("chat research is outside the preparation pool");
    assert_eq!(request["items"][0]["id"],"tail");assert_eq!(request["items"].as_array().unwrap().len(),1);
    assert!(!preparation_workers::pending_conflict(&d,row(&d,"jobs",&job).unwrap(),true));
    assert_eq!(row(&d,"jobs",&parent).unwrap(),&paid);assert_eq!(row(&d,"jobs",&lookup).unwrap(),&research);
    let jobs=d["jobs"].clone();assert!(auto_prepare::claim(&mut d,timestamp(&now()).unwrap()).unwrap().is_none());
    assert_eq!(d["jobs"],jobs,"one ordinary slot remains bounded while research runs");
    assert!(list(&d,"proposals").is_empty());assert!(list(&d,"operations").is_empty());assert!(list(&d,"approvals").is_empty());
}

#[test]
fn research_holds_post_family_and_cross_post_recipient_conversation_aliases() {
    let (mut d,_parent,_lookup)=active_fact_fixture();
    for (id,post) in [("same-post","ready-post"),("recipient-alias","alias-post"),("conversation-alias","conversation-post"),("tail","tail-post")] {fact_tail(&mut d,id,post);}
    let original=row(&d,"items","ready").unwrap().clone();
    for (id,field) in [("recipient-alias","itemId"),("conversation-alias","conversationKey")] {
        let alias=row_mut(&mut d,"items",id).unwrap();alias["objectId"]=original["objectId"].clone();alias[field]=original[field].clone();
    }
    let admission=preparation_workers::AutomaticAdmission::capture(&d,1);assert!(admission.available());
    for id in ["same-post","recipient-alias","conversation-alias"] {assert!(!admission.permits(&d,row(&d,"items",id).unwrap()),"{id}");}
    assert!(admission.permits(&d,row(&d,"items","tail").unwrap()));
    let (_,request)=auto_prepare::claim(&mut d,timestamp(&now()).unwrap()).unwrap().unwrap();
    assert_eq!(request["items"][0]["id"],"tail");
    for id in ["same-post","recipient-alias","conversation-alias"] {
        let pending=&row(&d,"items",id).unwrap()["autoPreparation"];
        assert_eq!(pending["attempts"],0);assert!(pending["jobId"].is_null());
    }
}

#[test]
fn absent_or_tampered_active_fact_capture_is_exclusive_and_never_spends() {
    let (mut original,parent,lookup)=active_fact_fixture();fact_tail(&mut original,"tail","tail-post");
    for change in ["legacy","parent","dependencies","recipients","signatures","query","company","group","capture","attempt","source","binding"] {
        let mut d=original.clone();
        let mut scoped=original.clone();
        let ordinary=engine_prepare::schedule(&mut scoped,engine_prepare::Input{item_ids:vec!["tail".into()],instruction:None}).unwrap();
        for mut d in [&mut d,&mut scoped] {
        match change {
            "legacy"=>{row_mut(&mut d,"jobs",&lookup).unwrap().as_object_mut().unwrap().remove("factWorkerScope");},
            "parent"=>row_mut(&mut d,"jobs",&lookup).unwrap()["parentPrepareJobId"]=json!("missing"),
            "dependencies"=>row_mut(&mut d,"jobs",&lookup).unwrap()["factDependencyIds"]=json!([]),
            "recipients"=>row_mut(&mut d,"jobs",&lookup).unwrap()["requestedItemIds"]=json!(["tail"]),
            "signatures"=>row_mut(&mut d,"jobs",&lookup).unwrap()["factSignatures"]=json!([]),
            "query"=>row_mut(&mut d,"jobs",&lookup).unwrap()["researchRequest"]["query"]=json!("Another query"),
            "company"=>row_mut(&mut d,"jobs",&lookup).unwrap()["factWorkerScope"]["account"]=json!("BAW Russia"),
            "group"=>row_mut(&mut d,"jobs",&lookup).unwrap()["factGroupKey"]=json!("tampered"),
            "capture"=>row_mut(&mut d,"jobs",&lookup).unwrap()["factWorkerScope"]["recipientKeys"]=json!([]),
            "attempt"=>row_mut(&mut d,"jobs",&parent).unwrap()["factFollowups"][0]["attempts"][0]["jobId"]=json!("other"),
            "source"=>row_mut(&mut d,"posts","ready-post").unwrap()["text"]=json!("Changed public source"),
            _=>{let mut binding=active_binding(&d).unwrap().to_json();binding["revision"]=json!(binding["revision"].as_u64().unwrap_or(1)+1);d["connectorBinding"]=binding;},
        }
        }
        for width in [1,8] {assert!(!preparation_workers::AutomaticAdmission::capture(&d,width).available(),"{change} width {width}");}
        let before=d["jobs"].clone();assert!(auto_prepare::claim(&mut d,timestamp(&now()).unwrap()).unwrap().is_none(),"{change}");assert_eq!(d["jobs"],before);
        // Ordinary scoped execution must not revive the old unconditional
        // public-fact skip, even when automatic scheduling is bypassed.
        assert!(preparation_workers::pending_conflict(&scoped,row(&scoped,"jobs",&ordinary.job_id).unwrap(),true),"{change}");
    }
}

#[test]
fn late_unknown_alias_revokes_independence_and_keeps_the_paid_result() {
    for status in ["unknown","dispatching"] {
        let (mut d,parent,lookup)=active_fact_fixture();fact_tail(&mut d,"tail","tail-post");
        let target=row(&d,"items","ready").unwrap().clone();
        let unknown=json!({"id":"uncertain-alias","itemId":"removed-local-alias","target":target,"status":status});
        list_mut(&mut d,"operations").push(unknown.clone());
        assert!(!preparation_workers::AutomaticAdmission::capture(&d,8).available());
        let before=d["jobs"].clone();assert!(auto_prepare::claim(&mut d,timestamp(&now()).unwrap()).unwrap().is_none());assert_eq!(d["jobs"],before);
        complete(&mut d,&parent,&lookup);
        assert!(row(&d,"jobs",&lookup).unwrap()["researchResult"].is_object());
        let entry=&row(&d,"jobs",&parent).unwrap()["factFollowups"][0];
        assert_eq!(entry["status"],"stale");assert_eq!(rows(entry,"attempts").len(),1);
        assert!(rows(&select(&d,&[json!("ready")],&now()).unwrap(),"materials").is_empty());
        assert_eq!(list(&d,"operations"),&[unknown]);assert!(list(&d,"approvals").is_empty());
    }
}

#[test]
fn fact_scope_json_restart_and_real_recovery_never_create_another_attempt() {
    let (mut d,parent,lookup)=active_fact_fixture();fact_tail(&mut d,"tail","tail-post");
    let capture=row(&d,"jobs",&lookup).unwrap()["factWorkerScope"].clone();
    let mut restored:Value=serde_json::from_str(&d.to_string()).unwrap();
    assert_eq!(row(&restored,"jobs",&lookup).unwrap()["factWorkerScope"],capture);
    assert!(preparation_workers::AutomaticAdmission::capture(&restored,1).available());
    assert_eq!(auto_prepare::claim(&mut restored,timestamp(&now()).unwrap()).unwrap().unwrap().1["items"][0]["id"],"tail");
    let mut recovered:Value=serde_json::from_str(&d.to_string()).unwrap();crate::recover(&mut recovered);
    assert_eq!(row(&recovered,"jobs",&lookup).unwrap()["status"],"interrupted");
    let attempts=row(&recovered,"jobs",&parent).unwrap()["factFollowups"][0]["attempts"].clone();
    let jobs=recovered["jobs"].clone();assert!(schedule_inner(&mut recovered,&parent,&[json!("ready")],None).unwrap().1.is_empty());
    assert_eq!(recovered["jobs"],jobs);assert_eq!(row(&recovered,"jobs",&parent).unwrap()["factFollowups"][0]["attempts"],attempts);
    assert_eq!(auto_prepare::claim(&mut recovered,timestamp(&now()).unwrap()).unwrap().unwrap().1["items"][0]["id"],"tail");
}

#[test]
fn duplicate_active_research_is_conservative_and_initial_research_never_replays() {
    let (mut d,parent,lookup)=active_fact_fixture();fact_tail(&mut d,"tail","tail-post");
    let mut duplicate=row(&d,"jobs",&lookup).unwrap().clone();duplicate["id"]=json!("extra-research");list_mut(&mut d,"jobs").push(duplicate);
    assert!(!preparation_workers::AutomaticAdmission::capture(&d,8).available());
    let before=d.clone();assert!(schedule_inner(&mut d,&parent,&[json!("ready")],None).is_err());assert_eq!(d,before);
    assert!(auto_prepare::claim(&mut d,timestamp(&now()).unwrap()).unwrap().is_none());
    assert_eq!(rows(&row(&d,"jobs",&parent).unwrap()["factFollowups"][0],"attempts").len(),1);
}

#[test]
fn fact_scope_is_rechecked_after_ordinary_claim_preview_without_spending() {
    let (mut d,_parent,lookup)=active_fact_fixture();fact_tail(&mut d,"tail","tail-post");
    let mut preview=d.clone();let (_,request)=auto_prepare::claim(&mut preview,timestamp(&now()).unwrap()).unwrap().unwrap();
    assert_eq!(request["items"][0]["id"],"tail");
    row_mut(&mut d,"jobs",&lookup).unwrap()["factWorkerScope"]["bundleDigest"]=json!("changed-after-preview");
    let before=d.clone();assert!(auto_prepare::claim(&mut d,timestamp(&now()).unwrap()).unwrap().is_none());
    assert_eq!(d["jobs"],before["jobs"]);assert!(row(&d,"items","tail").unwrap()["autoPreparation"]["jobId"].is_null());
    assert!(row(&d,"items","tail").unwrap()["autoPreparation"]["attempts"].as_u64().is_none_or(|v|v==0));
}

#[test]
fn explicit_compatible_recipients_share_one_proven_research_scope_and_replay_the_same_job() {
    let mut d=engine_prepare::tests::fixture(false);
    row_mut(&mut d,"posts","ready-post").unwrap()["attachments"]=json!([]);
    for key in ["knowledge_entries","knowledge_versions","feedback"] {if d.get(key).is_none(){d[key]=json!([]);}}
    row_mut(&mut d,"items","media").unwrap()["postId"]=json!("ready-post");
    row_mut(&mut d,"items","media").unwrap()["postKey"]=json!("ready-post");
    row_mut(&mut d,"branches","media-branch").unwrap()["postId"]=json!("ready-post");
    let parent=engine_prepare::schedule(&mut d,engine_prepare::Input{item_ids:vec!["ready".into(),"media".into()],instruction:None}).unwrap().job_id;
    let declaration=|id|json!({"itemId":id,"kind":"missing_public_fact","claimScope":"Exact shared public specification","publicQuery":"manufacturer exact shared public specification"});
    engine_prepare::tests::complete_first_fixture(&mut d,&parent,&json!({"text":"Missing shared source","sources":[],"proposals":[],
        "assessments":[{"itemId":"ready","outcome":"needs_attention","reason":"Exact shared public specification source missing"},
            {"itemId":"media","outcome":"needs_attention","reason":"Exact shared public specification source missing"}],
        "factDependencies":[declaration("ready"),declaration("media")]}));
    let actor=operator_auth::Actor::local_owner("csrf");
    let (_,launches)=schedule(&mut d,&parent,&[json!("ready"),json!("media")],&actor).unwrap();assert_eq!(launches.len(),1);
    let lookup=launches[0].clone();let saved=row(&d,"jobs",&lookup).unwrap().clone();
    assert_eq!(saved["factWorkerScope"]["itemIds"],json!(["media","ready"]));
    fact_tail(&mut d,"tail","tail-post");
    let admission=preparation_workers::AutomaticAdmission::capture(&d,1);
    assert!(admission.available());assert!(admission.permits(&d,row(&d,"items","tail").unwrap()));
    let count=list(&d,"jobs").len();let (replayed,new)=schedule(&mut d,&parent,&[json!("ready"),json!("media")],&actor).unwrap();
    assert!(new.is_empty());assert!(replayed["jobIds"].as_array().unwrap().iter().all(|id|*id==json!(lookup)));
    assert_eq!(list(&d,"jobs").len(),count);assert_eq!(row(&d,"jobs",&lookup).unwrap(),&saved);
}

fn fact_lifecycle(d:&mut Value)->crate::runtime_lifecycle::OwnerToken {
    let token=crate::runtime_lifecycle::OwnerToken{account:d["account"].as_str().unwrap().into(),
        runtime_id:"fact-fixture-runtime".into(),release_sha256:"a".repeat(64),epoch:1};
    let ledger=crate::runtime_lifecycle::ledger_digest(d).unwrap();
    crate::runtime_lifecycle::initialize(d,token.clone(),&"b".repeat(64),&ledger).unwrap();token
}

#[test]
fn lifecycle_stale_epoch_or_drain_rejects_manual_and_automatic_lookup_before_attempt() {
    for automatic in [false,true] {for drain in [false,true] {
        let (mut d,parent)=if automatic{automatic_fixture(false)}else{fixture()};
        let token=fact_lifecycle(&mut d);
        if drain {crate::runtime_lifecycle::begin_drain(&mut d,&token,&"c".repeat(64),"fact-drain",false).unwrap();}
        else {d["runtimeLifecycle"]["owner"]["epoch"]=json!(2);}
        let actor=operator_auth::Actor::local_owner("csrf");let before=d.clone();
        assert!(schedule_research_admitted(&mut d,&parent,&[json!("ready")],if automatic{None}else{Some(&actor)},&token).is_err());
        assert_eq!(d,before,"no job, reservation or research attempt before current admission");
        assert!(rows(&row(&d,"jobs",&parent).unwrap()["factFollowups"][0],"attempts").is_empty());
    }}
}

#[test]
fn paid_research_settles_during_drain_but_no_continuation_can_start() {
    let (mut d,parent)=automatic_fixture(false);let token=fact_lifecycle(&mut d);
    let (_,launches)=schedule_research_admitted(&mut d,&parent,&[json!("ready")],None,&token).unwrap();assert_eq!(launches.len(),1);
    let lookup=launches[0].clone();let attempts=row(&d,"jobs",&parent).unwrap()["factFollowups"][0]["attempts"].clone();
    reserve_research_admission(&mut d,&token,&lookup,&now()).unwrap();
    crate::runtime_lifecycle::begin_drain(&mut d,&token,&"c".repeat(64),"fact-result-drain",false).unwrap();
    complete(&mut d,&parent,&lookup);
    assert!(row(&d,"jobs",&lookup).unwrap()["researchResult"].is_object());
    assert_eq!(row(&d,"jobs",&parent).unwrap()["factFollowups"][0]["status"],"resolved");
    assert_eq!(row(&d,"jobs",&parent).unwrap()["factFollowups"][0]["attempts"],attempts);
    let selection=automatic_selection(&d,&now(),1).unwrap();assert!(selection.resolved);
    let before=d.clone();assert!(schedule_continuation_admitted(&mut d,&selection,1,&now(),&token).is_err());
    assert_eq!(d,before);assert!(list(&d,"approvals").is_empty());assert!(list(&d,"operations").is_empty());
}

#[test]
fn fresh_continuation_captures_initial_paid_admission_in_its_guarded_writer() {
    let (mut d,parent)=automatic_resolved(false);let token=fact_test_token(&mut d);let at=now();
    let selection=automatic_selection(&d,&at,1).unwrap();assert!(selection.resolved);
    let paid=row(&d,"jobs",&parent).unwrap()["preparationStages"].clone();
    let child=schedule_continuation_admitted(&mut d,&selection,1,&at,&token).unwrap().job_id;
    let marker=row(&d,"jobs",&child).unwrap()["preparationStages"]["initialAdmission"].clone();
    assert!(marker.is_object(),"actual continuation writer records its first-stage reservation");
    assert_eq!(row(&d,"jobs",&parent).unwrap()["preparationStages"],paid);
    let before=d.clone();assert!(preparation_review::record_initial_admission(&mut d,&token,&child,&at).is_err());
    assert_eq!(d,before,"a repeated receipt cannot admit another paid stage or rewrite ownership");
    let request=row(&d,"jobs",&child).unwrap()["prepareBundle"]["request"].clone();
    preparation_review::reserve_first_admitted(&mut d,&token,&child,&request,&at).unwrap();
    assert!(row(&d,"jobs",&child).unwrap()["preparationStages"]["firstAdmission"].is_object());
    assert_eq!(row(&d,"jobs",&parent).unwrap()["factFollowups"][0]["consumedByJobId"],child);
    assert!(list(&d,"operations").is_empty());assert!(list(&d,"approvals").is_empty());
}

#[test]
fn research_stage_is_bound_to_initial_owner_and_can_be_reserved_only_once() {
    let (mut d,parent)=automatic_fixture(false);let token=fact_lifecycle(&mut d);
    let (_,launches)=schedule_research_admitted(&mut d,&parent,&[json!("ready")],None,&token).unwrap();let lookup=launches[0].clone();
    let initial=row(&d,"jobs",&lookup).unwrap()["factResearchInitialAdmission"].clone();assert!(initial.is_object());
    let reserved=reserve_research_admission(&mut d,&token,&lookup,&now()).unwrap();assert!(reserved["factResearchAdmission"].is_object());
    let before=d.clone();let error=reserve_research_admission(&mut d,&token,&lookup,&now()).unwrap_err();
    assert!(error.1.contains("model not dispatched"));assert_eq!(d,before);
    assert_eq!(row(&d,"jobs",&lookup).unwrap()["factResearchInitialAdmission"],initial);
    assert_eq!(rows(&row(&d,"jobs",&parent).unwrap()["factFollowups"][0],"attempts").len(),1);
    assert!(list(&d,"operations").is_empty());assert!(list(&d,"approvals").is_empty());
}

#[test]
fn legacy_lookup_and_changed_research_origin_cannot_start_a_paid_stage() {
    for legacy in [false,true] {
        let (mut d,parent)=automatic_fixture(false);let token=fact_lifecycle(&mut d);
        let lookup=if legacy{schedule_inner(&mut d,&parent,&[json!("ready")],None).unwrap().1[0].clone()}
            else{schedule_research_admitted(&mut d,&parent,&[json!("ready")],None,&token).unwrap().1[0].clone()};
        if !legacy {row_mut(&mut d,"jobs",&lookup).unwrap()["researchRequest"]["query"]=json!("changed after scheduling");}
        let before=d.clone();assert!(reserve_research_admission(&mut d,&token,&lookup,&now()).is_err());assert_eq!(d,before);
        assert!(row(&d,"jobs",&lookup).unwrap().get("factResearchAdmission").is_none());
        assert_eq!(rows(&row(&d,"jobs",&parent).unwrap()["factFollowups"][0],"attempts").len(),1);
    }
}

#[test]
fn resumed_new_epoch_cannot_resurrect_an_old_research_worker() {
    let (mut d,parent)=automatic_fixture(false);let token=fact_lifecycle(&mut d);
    let lookup=schedule_research_admitted(&mut d,&parent,&[json!("ready")],None,&token).unwrap().1[0].clone();
    let drain=crate::runtime_lifecycle::begin_drain(&mut d,&token,&"c".repeat(64),"waiting-research-drain",false).unwrap();
    let settled=crate::runtime_lifecycle::SettledNative{owner:drain.clone(),application_tasks:0,provider_queued:0,
        provider_dispatched:0,provider_contained:true,credential_writers:0,unresolved_effects:0};
    let resumed=crate::runtime_lifecycle::resume_same_owner(&mut d,&drain,&settled).unwrap();
    assert!(resumed.epoch>token.epoch);
    let before=d.clone();assert!(reserve_research_admission(&mut d,&resumed,&lookup,&now()).is_err());assert_eq!(d,before);
    assert!(row(&d,"jobs",&lookup).unwrap().get("factResearchAdmission").is_none());
    assert_eq!(rows(&row(&d,"jobs",&parent).unwrap()["factFollowups"][0],"attempts").len(),1);
}

#[tokio::test]
async fn drain_while_research_waits_for_chat_gate_never_dispatches_model() {
    let (app,_folder)=crate::tests::test_app().await;let (mut d,parent)=automatic_fixture(false);
    d.as_object_mut().unwrap().remove("runtimeLifecycle");
    let token=crate::runtime_lifecycle::OwnerToken{account:app.lifecycle_owner.account.clone(),runtime_id:app.lifecycle_owner.runtime_id.clone(),
        release_sha256:app.lifecycle_owner.release_sha256.clone(),epoch:1};
    let ledger=crate::runtime_lifecycle::ledger_digest(&d).unwrap();
    crate::runtime_lifecycle::initialize(&mut d,token.clone(),&"b".repeat(64),&ledger).unwrap();
    let lookup=schedule_research_admitted(&mut d,&parent,&[json!("ready")],None,&token).unwrap().1[0].clone();
    let initial=row(&d,"jobs",&lookup).unwrap()["factResearchInitialAdmission"].clone();
    app.change(|target|{*target=d.clone();Ok(())}).await.unwrap();
    assert_eq!(app.lifecycle_admission_token(crate::runtime_lifecycle::AdmissionClass::Preparation).await.unwrap(),token);
    let gate=app.assistant_chat_gate.lock().await;let worker=app.clone();let id=lookup.clone();
    let task=tokio::spawn(async move{run(worker,id).await});tokio::task::yield_now().await;
    app.change(|target|{crate::runtime_lifecycle::begin_drain(target,&token,&"c".repeat(64),"gate-wait-drain",false)?;Ok(())}).await.unwrap();
    drop(gate);let error=task.await.unwrap().unwrap_err();assert_eq!(error.0,StatusCode::CONFLICT);
    let native=app.lifecycle_work.snapshot().unwrap();assert_eq!(native.active,0);assert_eq!(native.unresolved,0);
    assert!(error.1.contains("model not dispatched"));
    let state=app.read().await.unwrap();let job=row(&state,"jobs",&lookup).unwrap();
    assert!(job.get("factResearchAdmission").is_none());assert!(job.get("researchResult").is_none());
    assert_eq!(job["factResearchInitialAdmission"],initial);
    assert_eq!(rows(&row(&state,"jobs",&parent).unwrap()["factFollowups"][0],"attempts").len(),1);
    assert!(list(&state,"operations").is_empty());assert!(list(&state,"approvals").is_empty());
}

#[tokio::test]
async fn actual_research_worker_without_initial_marker_or_with_reserved_stage_never_dispatches() {
    for legacy in [false,true] {
        let (app,_folder)=crate::tests::test_app().await;let (mut d,parent)=automatic_fixture(false);
        d.as_object_mut().unwrap().remove("runtimeLifecycle");
        let token=crate::runtime_lifecycle::OwnerToken{account:app.lifecycle_owner.account.clone(),runtime_id:app.lifecycle_owner.runtime_id.clone(),
            release_sha256:app.lifecycle_owner.release_sha256.clone(),epoch:1};
        let ledger=crate::runtime_lifecycle::ledger_digest(&d).unwrap();
        crate::runtime_lifecycle::initialize(&mut d,token.clone(),&"b".repeat(64),&ledger).unwrap();
        let lookup=if legacy{schedule_inner(&mut d,&parent,&[json!("ready")],None).unwrap().1[0].clone()}
            else{schedule_research_admitted(&mut d,&parent,&[json!("ready")],None,&token).unwrap().1[0].clone()};
        if !legacy {reserve_research_admission(&mut d,&token,&lookup,&now()).unwrap();}
        let saved=row(&d,"jobs",&lookup).unwrap().clone();app.change(|target|{*target=d.clone();Ok(())}).await.unwrap();
        assert_eq!(app.lifecycle_admission_token(crate::runtime_lifecycle::AdmissionClass::Preparation).await.unwrap(),token);
        let error=run(app.clone(),lookup.clone()).await.unwrap_err();assert_eq!(error.0,StatusCode::CONFLICT);
        let native=app.lifecycle_work.snapshot().unwrap();assert_eq!(native.active,0);assert_eq!(native.unresolved,0);
        assert!(error.1.contains("model not dispatched"),"legacy {legacy}: {}",error.1);
        let state=app.read().await.unwrap();assert_eq!(row(&state,"jobs",&lookup).unwrap(),&saved);
        assert!(row(&state,"jobs",&lookup).unwrap().get("researchResult").is_none());
        assert_eq!(rows(&row(&state,"jobs",&parent).unwrap()["factFollowups"][0],"attempts").len(),1);
        assert!(list(&state,"operations").is_empty());assert!(list(&state,"approvals").is_empty());
    }
}

#[test]
fn dispatched_paid_result_survives_cancellation_and_superseded_dependencies() {
    for change in ["cancelled","missing_dependency","superseded_attempt","dependency_signature"] {
        let (mut d,parent,lookup)=active_fact_fixture();let job=row(&d,"jobs",&lookup).unwrap().clone();
        let entries=rows(row(&d,"jobs",&parent).unwrap(),"factFollowups").iter()
            .filter(|e|rows(&job,"factDependencyIds").contains(&e["id"])).cloned().collect::<Vec<_>>();
        let paid=admit_sources(&result(&job["researchRequest"]),&job["researchRequest"]).unwrap();
        match change {
            "cancelled"=>row_mut(&mut d,"jobs",&lookup).unwrap()["status"]=json!("cancelled"),
            "missing_dependency"=>{row_mut(&mut d,"jobs",&parent).unwrap()["factFollowups"].as_array_mut().unwrap().retain(|e|e["id"]!=entries[0]["id"]);},
            "superseded_attempt"=>row_mut(&mut d,"jobs",&parent).unwrap()["factFollowups"][0]["attempts"].as_array_mut().unwrap()
                .push(json!({"jobId":"newer-owned-attempt","attempt":2,"createdAt":now()})),
            _=>row_mut(&mut d,"jobs",&parent).unwrap()["factFollowups"][0]["signature"]=json!("newer-signature"),
        }
        let newer=row(&d,"jobs",&parent).unwrap()["factFollowups"].clone();
        let outcome=settle(&mut d,&lookup,&job,&entries,&paid).unwrap();
        assert_eq!(outcome["dependencies"][0]["status"],"stale","{change}");
        assert_eq!(row(&d,"jobs",&lookup).unwrap()["researchResult"],paid);
        assert_eq!(row(&d,"jobs",&lookup).unwrap()["researchResultBinding"]["researchAdmission"],job["factResearchAdmission"]);
        if matches!(change,"missing_dependency"|"superseded_attempt"|"dependency_signature") {
            assert_eq!(row(&d,"jobs",&parent).unwrap()["factFollowups"],newer,"newer/missing dependency admission is untouched");
        }
        assert!(automatic_selection(&d,&now(),1).is_none());assert!(list(&d,"operations").is_empty());assert!(list(&d,"approvals").is_empty());
        let before=d.clone();assert_eq!(settle(&mut d,&lookup,&job,&entries,&paid).unwrap(),outcome);assert_eq!(d,before);
        let mut replacement=paid.clone();replacement["text"]=json!("Different paid result cannot replace the retained response");
        assert!(settle(&mut d,&lookup,&job,&entries,&replacement).is_err());assert_eq!(d,before);
    }
}

#[test]
fn unreserved_research_cannot_masquerade_as_a_dispatched_paid_result() {
    let (mut d,parent)=automatic_fixture(false);let token=fact_lifecycle(&mut d);
    let lookup=schedule_research_admitted(&mut d,&parent,&[json!("ready")],None,&token).unwrap().1[0].clone();
    let job=row(&d,"jobs",&lookup).unwrap().clone();let entries=rows(row(&d,"jobs",&parent).unwrap(),"factFollowups").iter()
        .filter(|e|rows(&job,"factDependencyIds").contains(&e["id"])).cloned().collect::<Vec<_>>();
    let result=admit_sources(&result(&job["researchRequest"]),&job["researchRequest"]).unwrap();let before=d.clone();
    assert!(settle(&mut d,&lookup,&job,&entries,&result).is_err());assert_eq!(d,before);
    assert!(row(&d,"jobs",&lookup).unwrap().get("researchResult").is_none());
}

#[test]
fn corrupted_immutable_execution_identity_cannot_absorb_another_captured_paid_result() {
    for field in ["kind","purpose","researchRequest","parentPrepareJobId","factDependencyIds","requestedItemIds","factSignatures","factResearchInitialAdmission","factResearchAdmission"] {
        let (mut d,parent,lookup)=active_fact_fixture();let job=row(&d,"jobs",&lookup).unwrap().clone();
        let entries=rows(row(&d,"jobs",&parent).unwrap(),"factFollowups").iter()
            .filter(|e|rows(&job,"factDependencyIds").contains(&e["id"])).cloned().collect::<Vec<_>>();
        let paid=admit_sources(&result(&job["researchRequest"]),&job["researchRequest"]).unwrap();
        row_mut(&mut d,"jobs",&lookup).unwrap()[field]=json!({"corruptedExecutionIdentity":true});
        let before=d.clone();let error=settle(&mut d,&lookup,&job,&entries,&paid).unwrap_err();
        assert!(error.1.contains("paid result not retargeted"),"{field}");assert_eq!(d,before);
        assert!(row(&d,"jobs",&lookup).unwrap().get("researchResult").is_none());
    }
}
