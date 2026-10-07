//! Connected scheduler/reducer checks. Authored offline; execution belongs to ROOT.
//! No external bridge is used: capacity answers are injected at the production seam.
use super::*;
use super::super::tests::{fixture, add_recipient, NOW};
use std::sync::{Arc, Mutex};
use tokio::sync::Notify;
use crate::engine_prepare::capacity::Size;

fn complete_captured(d:&mut Value,run:&str,result:&Value,at:i64)->crate::ApiResult<Value>{
    let request=crate::row(d,"jobs",run)?["prepareBundle"]["request"].clone();
    let mut result=if result["runMetadata"].is_object(){result.clone()}
        else{crate::engine_prepare::tests::single_pass_result(result.clone())};
    if result.get("factDependencies").is_none(){result["factDependencies"]=json!([]);}
    crate::model_material_receipt::fixture_result(d,run,&request,&mut result)?;
    if crate::row(d,"jobs",run)?["preparationStages"]["first"].is_null(){
        crate::preparation_review::settle_first(d,run,&request,&result,&super::super::stamp(at))?;
    }
    super::super::complete(d,run,&result,at)
}

#[tokio::test]
async fn rolling_graph_is_finite_and_keeps_original_split_recipients_excluded() {
    let mut d=source();add_recipient(&mut d,"same-family","post");let mut graph=Frontier::new(&d);
    let candidate=discover(&d,NOW,2).unwrap().remove(0);
    let original=candidate.item_ids.clone();let sequence=graph.launch(&candidate);
    assert!(original.len()>1,"the fixture must actually exercise a grouped capacity split");
    let split=preflight_with(&d,candidate,|requests|async move{Ok(requests.into_iter().map(|request|
        if request["items"].as_array().unwrap().len()>1{Size::Oversized}else{Size::Fits(100)}).collect())}).await.unwrap();
    assert_eq!(split.selected.len(),1);assert!(split.oversized.is_empty());
    graph.pending.remove(&sequence);
    let mut fresh=d.clone();independent(&mut fresh,"arrived","post-arrived");
    let excluded=graph.exclusions(&fresh);
    assert!(original.iter().all(|id|excluded.contains(id)));
    assert!(excluded.contains("arrived"),"a stream of arrivals cannot prolong one producer wake");
    assert!(!excluded.contains("j"));
    let candidates=discover_refill(&fresh,NOW,2,2,&excluded,&graph.pending).unwrap();
    assert_eq!(candidates[0].item_ids,vec!["j"]);
    assert!(candidates.iter().all(|c|!c.item_ids.contains(&"arrived".into())));
    assert_eq!(fresh["items"].as_array().unwrap().len(),5,"discovery does not remove deferred work");
}

#[test]
fn rolling_discovery_replays_pending_alias_and_family_fences_without_durable_claims() {
    let mut d=source();let mut graph=Frontier::new(&d);
    let pending=discover(&d,NOW,2).unwrap().remove(0);graph.launch(&pending);
    // A different local recipient becomes an alias of pending i after capture.
    let alias=crate::row(&d,"items","i").unwrap()["conversationKey"].clone();
    crate::row_mut(&mut d,"items","j").unwrap()["conversationKey"]=alias;
    let before=d.clone();let excluded=graph.exclusions(&d);
    let next=discover_refill(&d,NOW,2,1,&excluded,&graph.pending).unwrap();
    assert_eq!(next.len(),1);assert_eq!(next[0].item_ids,vec!["k"]);
    assert_eq!(d,before,"unpaid pending ownership remains private and rebuildable");
    no_external_intent(&d);
}

#[tokio::test]
async fn actual_rolling_refill_advances_third_family_before_hanging_first_probe_ends() {
    let (app,_temp)=app_fixture(2,false).await;
    let started=Arc::new(Notify::new());let release=Arc::new(Notify::new());let third=Arc::new(Notify::new());
    let probes=Arc::new(Mutex::new(Vec::new()));let admitted=Arc::new(Mutex::new(Vec::new()));
    let retained=Arc::new(Mutex::new(std::collections::BTreeMap::<usize,usize>::new()));
    struct SnapshotGuard{retained:Arc<Mutex<std::collections::BTreeMap<usize,usize>>>,key:usize}
    impl Drop for SnapshotGuard{fn drop(&mut self){let mut retained=self.retained.lock().unwrap();
        let count=retained.get_mut(&self.key).unwrap();*count-=1;if *count==0{retained.remove(&self.key);}}}
    let task=tokio::spawn({let app=app.clone();let started=started.clone();let release=release.clone();
        let third=third.clone();let probes=probes.clone();let admitted=admitted.clone();let retained=retained.clone();
        async move{fill_with(&app,move|snapshot,candidate|{
            let started=started.clone();let release=release.clone();let probes=probes.clone();let retained=retained.clone();
            async move{
                let key=Arc::as_ptr(&snapshot) as usize;
                {let mut retained=retained.lock().unwrap();*retained.entry(key).or_default()+=1;
                    assert!(retained.len()<=2,"retained snapshot generations stay within configured worker width");}
                let _snapshot_guard=SnapshotGuard{retained,key};
                let id=candidate.item_ids[0].clone();probes.lock().unwrap().push(id.clone());
                if id=="i"{started.notify_one();release.notified().await;}
                preflight_with(&snapshot,candidate,move|requests|{let id=id.clone();async move{
                    Ok(requests.into_iter().map(|_|if id=="j"{Size::Oversized}else{Size::Fits(100)}).collect())
                }}).await
            }
        },move|_job,request|{let id=request["items"][0]["id"].clone();
            admitted.lock().unwrap().push(id.clone());if id=="k"{third.notify_one();}}).await}
    });
    signal(&started,"first probe must hang").await;
    signal(&third,"third family must refill j's hold while i is unpaid and suspended").await;
    assert!(!task.is_finished());
    let state=app.read().await.unwrap();assert!(crate::row(&state,"items","i").unwrap()["autoPreparation"]["jobId"].is_null());
    assert_eq!(crate::row(&state,"items","j").unwrap()["autoPreparation"]["attempts"],0);
    release.notify_one();task.await.unwrap().unwrap();
    let mut seen=probes.lock().unwrap().clone();seen.sort();assert_eq!(seen,vec!["i","j","k"]);
    assert_eq!(admitted.lock().unwrap().len(),2,"each native ready job admits exactly once");
    assert!(retained.lock().unwrap().is_empty(),"settled futures release captured snapshot generations");
    no_external_intent(&app.read().await.unwrap());app.db.close().await;
}

#[tokio::test]
async fn actual_committed_completion_wake_refills_while_unpaid_probe_hangs() {
    let (app,_temp)=app_fixture(2,false).await;
    let started=Arc::new(Notify::new());let release=Arc::new(Notify::new());let fast=Arc::new(Notify::new());let third=Arc::new(Notify::new());
    let jobs=Arc::new(Mutex::new(Vec::<(String,String)>::new()));
    let task=tokio::spawn({let app=app.clone();let started=started.clone();let release=release.clone();
        let fast=fast.clone();let third=third.clone();let jobs=jobs.clone();
        async move{fill_with(&app,move|snapshot,candidate|{let started=started.clone();let release=release.clone();async move{
            if candidate.item_ids==vec!["i"]{started.notify_one();release.notified().await;}
            preflight_with(&snapshot,candidate,|requests|async move{Ok(requests.into_iter().map(|_|Size::Fits(100)).collect())}).await
        }},move|job,request|{let id=request["items"][0]["id"].as_str().unwrap().to_owned();
            jobs.lock().unwrap().push((job,id.clone()));if id=="j"{fast.notify_one();}if id=="k"{third.notify_one();}}).await}
    });
    signal(&started,"first hangs").await;signal(&fast,"second admits").await;
    let job=jobs.lock().unwrap().iter().find(|(_,id)|id=="j").unwrap().0.clone();
    app.change(|d|{let at=chrono::Utc::now().timestamp();
        complete_captured(d,&job,&json!({"text":"Local evidence retained","sources":[],
            "assessments":[{"itemId":"j","outcome":"needs_attention","reason":"Retained"}],"proposals":[]}),at)?;
        crate::row_mut(d,"jobs",&job)?["status"]=json!("completed");Ok(())}).await.unwrap();
    // Same notification seam used only after committed job finalization.
    app.preparation_wake.notify_one();
    signal(&third,"completion must refresh durable graph before first's deadline").await;
    assert!(!task.is_finished());release.notify_one();task.await.unwrap().unwrap();
    assert_eq!(jobs.lock().unwrap().len(),3);
    tokio::time::timeout(std::time::Duration::from_secs(1),app.preparation_wake.notified()).await
        .expect("consumed wake retained for ordinary fact/recovery tail");
    no_external_intent(&app.read().await.unwrap());app.db.close().await;
}

#[tokio::test]
async fn actual_rolling_invalidation_holds_only_stale_capture_without_reprobing_it() {
    let (app,_temp)=app_fixture(2,false).await;let probes=Arc::new(Mutex::new(Vec::new()));let admitted=Arc::new(Mutex::new(Vec::new()));
    let error=fill_with(&app,{let app=app.clone();let probes=probes.clone();move|snapshot,candidate|{
        let app=app.clone();let probes=probes.clone();async move{
            let id=candidate.item_ids[0].clone();probes.lock().unwrap().push(id.clone());
            let prepared=preflight_with(&snapshot,candidate,|requests|async move{Ok(requests.into_iter().map(|_|Size::Fits(100)).collect())}).await?;
            if id=="i"{tokio::spawn(async move{app.change(|d|{d["branches"][0]["messages"][0]["text"]=json!("Fresh source invalidates prior unpaid capture");Ok(())}).await}).await.unwrap()?;}
            Ok(prepared)
        }
    }},{let admitted=admitted.clone();move|_job,request|{admitted.lock().unwrap().push(request["items"][0]["id"].clone());}}).await.unwrap_err();
    assert!(error.1.contains("changed"));
    let mut seen=probes.lock().unwrap().clone();seen.sort();assert_eq!(seen,vec!["i","j","k"]);
    let mut selected=admitted.lock().unwrap().clone();selected.sort_by_key(Value::to_string);assert_eq!(selected,vec![json!("j"),json!("k")]);
    let state=app.read().await.unwrap();assert_eq!(crate::row(&state,"items","i").unwrap()["autoPreparation"]["attempts"],0);
    no_external_intent(&state);app.db.close().await;
}

#[tokio::test]
async fn actual_rolling_drain_discards_only_unpaid_pending_work_before_refill() {
    let (app,_temp)=app_fixture(2,false).await;let started=Arc::new(Notify::new());let release=Arc::new(Notify::new());
    let probes=Arc::new(Mutex::new(Vec::new()));let admitted=Arc::new(Mutex::new(Vec::new()));
    let task=tokio::spawn({let app=app.clone();let started=started.clone();let release=release.clone();let probes=probes.clone();let admitted=admitted.clone();
        async move{fill_with(&app,move|snapshot,candidate|{let started=started.clone();let release=release.clone();let probes=probes.clone();async move{
            probes.lock().unwrap().push(candidate.item_ids[0].clone());started.notify_one();release.notified().await;
            preflight_with(&snapshot,candidate,|requests|async move{Ok(requests.into_iter().map(|_|Size::Fits(100)).collect())}).await
        }},move|job,_request|{admitted.lock().unwrap().push(job);}).await}
    });
    signal(&started,"pending probe must actually run").await;
    let token=app.lifecycle_admission_token(crate::runtime_lifecycle::AdmissionClass::Preparation).await.unwrap();
    app.db.change(|d|{crate::runtime_lifecycle::begin_drain(d,&token,&"c".repeat(64),"rolling-drain",false)?;Ok(())}).await.unwrap();
    let frozen=app.read().await.unwrap();app.preparation_wake.notify_one();
    assert!(task.await.unwrap().is_err(),"original owner token must stop refill immediately");
    assert!(admitted.lock().unwrap().is_empty());assert_eq!(app.read().await.unwrap(),frozen);
    assert!(!probes.lock().unwrap().contains(&"k".to_owned()));
    no_external_intent(&frozen);app.db.close().await;
}

#[test]
fn pending_dependency_invalidation_never_turns_missing_fence_into_spare_capacity() {
    let mut d=source();let mut graph=Frontier::new(&d);
    let pending=discover(&d,NOW,2).unwrap().remove(0);graph.launch(&pending);
    crate::row_mut(&mut d,"items","i").unwrap()["draftEdited"]=json!(true);
    let changed=d.clone();let excluded=graph.exclusions(&d);
    assert!(discover_refill(&d,NOW,2,1,&excluded,&graph.pending).unwrap().is_empty());
    assert_eq!(d,changed,"unproven pending affinity never clears ownership or mutates source");
    graph.pending.clear();
    let next=discover_refill(&d,NOW,2,2,&excluded,&graph.pending).unwrap();
    assert_eq!(next.len(),2,"after unpaid settlement independent siblings advance");
    assert!(next.iter().all(|c|c.item_ids!=vec!["i"]));
}

#[tokio::test]
async fn actual_exclusive_revalidation_error_is_single_use_in_one_rolling_wake() {
    let (app,_temp)=app_fixture(2,false).await;let mut d=review_waiting();
    let at=chrono::Utc::now().timestamp();
    for item in d["items"].as_array_mut().unwrap(){
        item["createdAt"]=json!(super::super::stamp(at-60));
        item["providerObservedAt"]=json!(super::super::stamp(at));
    }
    crate::row_mut(&mut d,"items","i").unwrap()["autoRevalidation"]=Value::Null;
    super::super::revalidation::observe(&mut d,at-33).unwrap();
    assert!(super::super::revalidation::ready(&d,at).unwrap(),"live scheduler fixture must have a stable exclusive review");
    super::super::reconcile_claim_state(&mut d,at).unwrap();
    app.db.change(|state|{*state=d;Ok(())}).await.unwrap();
    let before=app.read().await.unwrap();let probes=Arc::new(Mutex::new(0usize));
    let error=fill_with(&app,{let probes=probes.clone();move|_snapshot,candidate|{let probes=probes.clone();async move{
        assert!(!candidate.initial);*probes.lock().unwrap()+=1;Err(crate::bad("offline exclusive review failure"))
    }}},|_job,_request|panic!("failed unpaid exclusive descriptor cannot spawn")).await.unwrap_err();
    assert!(error.1.contains("exclusive review failure"));assert_eq!(*probes.lock().unwrap(),1);
    assert_eq!(app.read().await.unwrap(),before);no_external_intent(&before);app.db.close().await;
}

#[tokio::test]
async fn actual_failed_frontier_has_finite_discovery_read_and_round_trace_counts() {
    let (app,_temp)=app_fixture(2,false).await;let before=app.read().await.unwrap();
    let probes=Arc::new(Mutex::new(Vec::new()));
    let (result,events)=crate::performance::capture(fill_with(&app,{let probes=probes.clone();move|_snapshot,candidate|{
        let probes=probes.clone();async move{probes.lock().unwrap().push(candidate.item_ids[0].clone());
            Err(crate::bad("Unpaid fixture failure"))}
    }},|_job,_request|panic!("failed frontier cannot spawn"))).await;
    assert!(result.is_err());let mut probes=probes.lock().unwrap().clone();probes.sort();assert_eq!(probes,vec!["i","j","k"]);
    let reads=events.iter().filter(|event|event["stage"]=="preparation.graph.discovery_read").count();
    let rounds=events.iter().filter(|event|event["stage"]=="preparation.graph.discovery").count();
    assert_eq!(reads,4,"one initial projection read plus one per settled probe");
    assert_eq!(rounds,4,"finite initial frontier plus terminal empty discovery");
    assert_eq!(app.read().await.unwrap(),before);no_external_intent(&before);app.db.close().await;
}

#[tokio::test]
async fn old_graph_capture_rejects_fresh_approval_unknown_or_hidden_recipient_atomically() {
    for blocker in ["approved","unknown","hidden"] {
        let mut d=source();let snapshot=d.clone();
        let prepared=fits(&snapshot,discover(&snapshot,NOW,2).unwrap().remove(0)).await;
        match blocker {
            "approved"=>{d["proposals"].as_array_mut().unwrap().push(json!({"id":"owner-approved","itemId":"i","status":"approved",
                "kind":"reply_and_close","text":"Owner approval body changed after unpaid graph capture"}));
                d["approvals"].as_array_mut().unwrap().push(json!({"id":"owner-approval","proposalId":"owner-approved","status":"approved","requestSha256":"a".repeat(64)}));},
            "unknown"=>d["operations"].as_array_mut().unwrap().push(json!({"id":"owner-unknown","itemId":"i","status":"unknown","kind":"reply_and_close"})),
            _=>crate::row_mut(&mut d,"items","i").unwrap()["providerStatus"]=json!("hidden"),
        }
        let protected=d.clone();assert!(transaction(&mut d,NOW,2,&prepared).is_err(),"{blocker}");
        assert_eq!(d,protected,"{blocker}: graph cannot alter approval/UNKNOWN/body or admit another job");
        assert!(crate::row(&d,"items","i").unwrap()["autoPreparation"]["jobId"].is_null());
    }
}

#[tokio::test]
async fn old_graph_capture_rejects_changed_current_rule_without_new_attempt_or_intent() {
    let mut d=source();let snapshot=d.clone();let prepared=fits(&snapshot,discover(&snapshot,NOW,2).unwrap().remove(0)).await;
    crate::knowledge::save_instruction(&mut d,&json!({"requestId":"offline-graph-rule","title":"Current owner rule",
        "text":"Reply must follow this newly reviewed current company rule"}),&crate::now()).unwrap();
    let current=crate::engine_prepare::build_request(&d,&[json!("i")],None).unwrap()["request"].clone();
    assert_ne!(prepared.expected.as_ref(),Some(&current),"the new rule must be current selected evidence before testing capture rejection");
    let protected=d.clone();assert!(transaction(&mut d,NOW,2,&prepared).is_err());assert_eq!(d,protected);
    assert!(crate::row(&d,"items","i").unwrap()["autoPreparation"]["jobId"].is_null());
    assert_eq!(crate::row(&d,"items","i").unwrap()["autoPreparation"]["attempts"].as_u64().unwrap_or(0),0);no_external_intent(&d);
}

#[tokio::test]
async fn pending_feedback_body_remains_advisory_and_graph_creates_no_publication_intent() {
    let mut d=source();let snapshot=d.clone();let prepared=fits(&snapshot,discover(&snapshot,NOW,2).unwrap().remove(0)).await;
    crate::knowledge::feedback(&mut d,"i",&json!({"draft":"old"}),&json!({"draft":"new pending owner feedback body"}),&super::super::stamp(NOW));
    assert_eq!(d["feedback"][0]["status"],"pending_review");let feedback=d["feedback"].clone();
    let (run,_)=transaction(&mut d,NOW,2,&prepared).unwrap().unwrap();
    assert_eq!(d["feedback"],feedback,"graph never promotes pending feedback into a current rule");
    assert_eq!(crate::row(&d,"jobs",&run).unwrap()["status"],"running");no_external_intent(&d);
}

fn running(d: &mut Value) -> crate::runtime_lifecycle::OwnerToken {
    // Official lifecycle bootstrap reducer: no optional-token or missing-state
    // exemption. The isolated fixture owns its complete protected ledger.
    if d.get("connectorBinding").is_none() { d["connectorBinding"] = crate::active_binding(d).unwrap().to_json(); }
    for key in ["jobs", "operations", "approvals", "audit", "materials", "knowledge_entries", "knowledge_versions", "feedback"] {
        if d.get(key).is_none() { d[key] = json!([]); }
    }
    let token = crate::runtime_lifecycle::OwnerToken {
        account: d["account"].as_str().unwrap().into(), runtime_id: "offline-runtime".into(),
        release_sha256: "a".repeat(64), epoch: 1,
    };
    let ledger = crate::runtime_lifecycle::ledger_digest(d).unwrap();
    crate::runtime_lifecycle::initialize(d, token.clone(), &"b".repeat(64), &ledger).unwrap();
    token
}

fn independent(d: &mut Value, id: &str, post: &str) {
    add_recipient(d, id, post);
    crate::row_mut(d, "items", id).unwrap()["createdAt"] = json!(super::super::stamp(NOW));
}
fn source() -> Value {
    let mut d = fixture();
    independent(&mut d, "j", "post-j");
    independent(&mut d, "k", "post-k");
    running(&mut d);
    super::super::reconcile_claim_state(&mut d, NOW).unwrap();
    d
}
fn transaction(d: &mut Value, at: i64, width: usize, prepared: &Prepared)
    -> crate::ApiResult<Option<(String, Value)>> {
    // Same rollback boundary as App::change_preparation_claim: rejected reducers
    // cannot leave a speculative job, oversize hold or paid attempt behind.
    let mut staged = d.clone();
    let result = commit_captured(&mut staged, at, width, prepared)?;
    *d = staged;
    Ok(result)
}
async fn fits(snapshot: &Value, candidate: Candidate) -> Prepared {
    preflight_with(snapshot, candidate, |requests| async move {
        Ok(requests.into_iter().map(|_| Size::Fits(100)).collect())
    }).await.unwrap()
}
fn no_external_intent(d: &Value) {
    assert!(crate::list(d, "approvals").is_empty());
    assert!(crate::list(d, "operations").is_empty());
}

#[tokio::test]
async fn discovery_and_preflight_leave_durable_source_unclaimed_and_bound_window() {
    let d = source(); let before = d.clone();
    let candidates = discover(&d, NOW, 2).unwrap();
    assert_eq!(candidates.len(), 2, "do not speculatively claim the full backlog");
    assert_eq!(candidates[0].item_ids, vec!["i"]);
    assert_eq!(candidates[1].item_ids, vec!["j"]);
    for candidate in candidates { assert!(fits(&d, candidate).await.expected.is_some()); }
    assert_eq!(d, before, "unpaid discovery/preflight are not durable admission");
    assert!(crate::list(&d, "jobs").iter().all(|j| j["kind"] != "assistant"));
    for item in crate::list(&d, "items") {
        assert!(item["autoPreparation"]["jobId"].is_null());
        assert!(item["autoPreparation"]["attempts"].as_u64().unwrap_or(0) == 0);
    }
    no_external_intent(&d);
}

#[tokio::test]
async fn later_preflight_can_commit_first_then_earlier_capture_commits_exactly_once() {
    let mut d = source(); let snapshot = d.clone();
    let mut candidates = discover(&snapshot, NOW, 2).unwrap().into_iter();
    let first = fits(&snapshot, candidates.next().unwrap()).await;
    let later = fits(&snapshot, candidates.next().unwrap()).await;
    let (j, request) = transaction(&mut d, NOW, 2, &later).unwrap().unwrap();
    assert_eq!(request["items"][0]["id"], "j", "fresh commit must not pick global queue head i");
    let (i, request) = transaction(&mut d, NOW, 2, &first).unwrap().unwrap();
    assert_eq!(request["items"][0]["id"], "i"); assert_ne!(i, j);
    for id in ["i", "j"] { assert_eq!(crate::row(&d, "items", id).unwrap()["autoPreparation"]["attempts"], 1); }
    let committed = d.clone();
    assert!(transaction(&mut d, NOW, 2, &later).is_err());
    assert_eq!(d, committed, "the already-admitted descriptor must not replay");
    no_external_intent(&d);
}

#[tokio::test]
async fn changed_first_source_rejects_only_its_capture_and_preserves_valid_sibling() {
    let mut d = source(); let snapshot = d.clone();
    let mut candidates = discover(&snapshot, NOW, 2).unwrap().into_iter();
    let first = fits(&snapshot, candidates.next().unwrap()).await;
    let later = fits(&snapshot, candidates.next().unwrap()).await;
    d["branches"][0]["messages"][0]["text"] = json!("Owner repaired the selected source");
    let repaired = d.clone();
    assert!(transaction(&mut d, NOW, 2, &first).is_err()); assert_eq!(d, repaired);
    let (_, request) = transaction(&mut d, NOW, 2, &later).unwrap().unwrap();
    assert_eq!(request["items"][0]["id"], "j");
    assert_eq!(d["branches"][0]["messages"][0]["text"], repaired["branches"][0]["messages"][0]["text"]);
    assert!(crate::row(&d, "items", "i").unwrap()["autoPreparation"]["jobId"].is_null());
    no_external_intent(&d);
}

#[tokio::test]
async fn changed_oversize_source_does_not_inherit_old_hold_or_consume_attempt() {
    let mut d = source(); let snapshot = d.clone();
    let candidate = discover(&snapshot, NOW, 1).unwrap().remove(0);
    let held = preflight_with(&snapshot, candidate, |requests| async move {
        Ok(requests.into_iter().map(|_| Size::Oversized).collect())
    }).await.unwrap();
    assert!(held.expected.is_none()); assert_eq!(held.oversized, vec!["i"]);
    assert_eq!(held.held_captures.len(), 1, "singleton size hold must bind its source");
    d["branches"][0]["messages"][0]["text"] = json!("Changed after size projection");
    let repaired = d.clone();
    assert!(transaction(&mut d, NOW, 2, &held).is_err()); assert_eq!(d, repaired);
    assert!(crate::row(&d, "items", "i").unwrap()["autoPreparation"]["attempts"].as_u64().unwrap_or(0) == 0);
    assert_ne!(crate::row(&d, "items", "i").unwrap()["reason"], "model_context_capacity_exceeded");
}

#[tokio::test]
async fn fresh_manual_admission_and_alias_reservation_fence_old_preflight() {
    for alias in [false, true] {
        let mut d = source(); let snapshot = d.clone();
        let candidate = discover(&snapshot, NOW, 1).unwrap().remove(0);
        let prepared = fits(&snapshot, candidate).await;
        let selected = if alias {
            crate::row_mut(&mut d, "items", "j").unwrap()["conversationKey"] = json!("thread");
            "j"
        } else { "i" };
        let owner = crate::engine_prepare::schedule(&mut d, crate::engine_prepare::Input {
            item_ids: vec![selected.into()], instruction: None,
        }).unwrap();
        let owned = d.clone();
        assert!(transaction(&mut d, NOW, 2, &prepared).is_err(), "alias={alias}");
        assert_eq!(d, owned, "manual paid owner cannot be replaced by an older capture");
        assert_eq!(crate::row(&d, "jobs", &owner.job_id).unwrap(), crate::row(&owned, "jobs", &owner.job_id).unwrap());
        no_external_intent(&d);
    }
}

#[tokio::test]
async fn lost_unpaid_window_reconstructs_without_replaying_paid_family() {
    let mut d = source(); let snapshot = d.clone();
    let mut window = discover(&snapshot, NOW, 2).unwrap().into_iter();
    let admitted = fits(&snapshot, window.next().unwrap()).await;
    let unpaid = fits(&snapshot, window.next().unwrap()).await;
    let (owner, _) = transaction(&mut d, NOW, 2, &admitted).unwrap().unwrap();
    drop(unpaid); drop(window);
    let restarted: Value = serde_json::from_str(&d.to_string()).unwrap();
    let reconstructed = discover(&restarted, NOW, 2).unwrap();
    assert_eq!(reconstructed.len(), 1);
    assert_eq!(reconstructed[0].item_ids, vec!["j"], "paid family remains owned after descriptor loss");
    assert_eq!(crate::row(&restarted, "items", "i").unwrap()["autoPreparation"]["jobId"], owner);
    assert_eq!(crate::row(&restarted, "items", "i").unwrap()["autoPreparation"]["attempts"], 1);
    assert_eq!(restarted, d, "reconstruction does not alter paid ownership");
}

#[tokio::test]
async fn draining_and_resumed_epoch_reject_old_preflight_before_any_claim_reducer() {
    let mut d = source(); let snapshot = d.clone();
    let candidate = discover(&snapshot, NOW, 1).unwrap().remove(0);
    let prepared = fits(&snapshot, candidate).await;
    let drain = crate::runtime_lifecycle::begin_drain(&mut d, &prepared.token,
        &"c".repeat(64), "offline-drain-attempt", false).unwrap();
    let frozen = d.clone();
    assert!(transaction(&mut d, NOW, 2, &prepared).is_err()); assert_eq!(d, frozen);
    assert!(discover(&d, NOW, 2).is_err(), "draining must block unpaid new captures as well");
    let native = crate::runtime_lifecycle::SettledNative {
        owner: drain.clone(), application_tasks: 0, provider_queued: 0, provider_dispatched: 0,
        provider_contained: true, credential_writers: 0, unresolved_effects: 0,
    };
    let resumed = crate::runtime_lifecycle::resume_same_owner(&mut d, &drain, &native).unwrap();
    assert!(resumed.epoch > prepared.token.epoch);
    let resumed_state = d.clone();
    assert!(transaction(&mut d, NOW, 2, &prepared).is_err()); assert_eq!(d, resumed_state);
    assert_eq!(discover(&d, NOW, 2).unwrap().len(), 2, "fresh captures may use the resumed epoch");
    for item in crate::list(&d, "items") { assert!(item["autoPreparation"]["attempts"].as_u64().unwrap_or(0) == 0); }
    no_external_intent(&d);
}

#[test]
fn missing_lifecycle_never_becomes_a_compatibility_admission_exemption() {
    let mut d = source(); d.as_object_mut().unwrap().remove("runtimeLifecycle");
    let before = d.clone(); assert!(discover(&d, NOW, 2).is_err()); assert_eq!(d, before);
}

fn review_waiting() -> Value {
    let mut d = fixture(); running(&mut d);
    let (job, _) = super::super::claim(&mut d, NOW).unwrap().unwrap();
    let saved_text = "Сохранённый ответ";
    // Exercise a genuinely admitted saved reply under the current exact media
    // review contract, rather than a legacy result whose proposal is rejected.
    let response = json!({"text":"Reviewed", "sources":[],
        "assessments":[{"itemId":"i","outcome":"reply","reason":"Evidence supports this reply"}],
        "proposals":[{"itemId":"i","kind":"reply_and_close","text":saved_text}],
        "editorialEvidence":{"version":1,"contract":crate::editorial_review::CONTRACT,"entries":[{
            "itemId":"i","kind":"reply_and_close","textSha256":crate::editorial_review::hash_text(saved_text),
            "decision":"accept","reason":"Exact reply checked against the supplied text-only source",
            "checks":{"companyRules":"pass","intent":"pass","factualScope":"pass"},
            "mediaDependency":{"audio":"independent","visual":"independent"}}]},
        "runMetadata":{"schemaVersion":1,"model":crate::codex_model_policy::MODEL,"modelProfile":crate::codex_model_policy::PROFILE,
            "reasoningEffort":"high","promptVersion":"communityhero-preparation-v1-single-pass",
            "instructionSha256":"a".repeat(64),"inputSha256":"b".repeat(64),"cliSha256":crate::codex_model_policy::CLI_SHA256,
            "elapsedMs":1,"completedAt":super::super::stamp(NOW),"decisionMediaContract":crate::decision_media::CONTRACT,
            "researchLimitContract":"uncapped_evidence_v1",
            "decisionDependencies":{"version":1,"entries":[{"itemId":"i","dependsOnItemIds":[]}]}}});
    let outcome = complete_captured(&mut d, &job, &response, NOW).unwrap();
    assert_eq!(outcome["status"], "prepared", "review fixture must contain an admitted saved reply: {outcome}");
    crate::row_mut(&mut d, "jobs", &job).unwrap()["status"] = json!("completed");
    let previous_bundle = crate::row(&d, "jobs", &job).unwrap()["prepareBundle"].clone();
    assert!(crate::prepare_bundle::current(&d, &previous_bundle).is_ok());
    // running() creates the real catalog arrays. An unimported raw material is
    // therefore excluded from preparation evidence; update this actual selected
    // text source instead. This helper is not a video/material-selection test.
    crate::row_mut(&mut d, "posts", "post").unwrap()["text"] = json!("Updated canonical post evidence");
    assert!(crate::prepare_bundle::current(&d, &previous_bundle).is_err());
    super::super::reconcile_stale(&mut d, NOW + 1);
    assert_eq!(d["proposals"][0]["status"], "stale");
    assert_eq!(crate::row(&d, "items", "i").unwrap()["autoPreparation"]["status"], "stale");
    d["settings"]["autoPreparation"] = json!({"revalidation":{"enabled":true,"debounceSeconds":30}});
    super::super::revalidation::observe(&mut d, NOW + 1).unwrap();
    assert_eq!(crate::row(&d, "items", "i").unwrap()["autoRevalidation"]["status"], "settling");
    assert!(!super::super::revalidation::ready(&d, NOW + 2).unwrap());
    assert!(super::super::revalidation::ready(&d, NOW + 33).unwrap());
    independent(&mut d, "j", "post-j"); independent(&mut d, "k", "post-k");
    d
}

#[tokio::test]
async fn stable_review_fences_speculative_refill_but_blocked_review_permits_it() {
    for blocked in [false, true] {
        let mut d = review_waiting();
        super::super::reconcile_claim_state(&mut d, NOW + 2).unwrap();
        let (active, _) = super::super::claim_reconciled(&mut d, NOW + 2, None, 2).unwrap().unwrap();
        assert_eq!(crate::row(&d, "jobs", &active).unwrap()["purpose"], "auto_prepare");
        let snapshot = d.clone();
        let candidate = discover(&snapshot, NOW + 2, 2).unwrap().remove(0);
        assert_eq!(candidate.item_ids, vec!["k"]);
        let prepared = fits(&snapshot, candidate).await;
        if blocked { crate::row_mut(&mut d, "items", "i").unwrap()["draft"] = json!("Owner changed the draft"); }
        assert_eq!(super::super::revalidation::ready(&d, NOW + 33).unwrap(), !blocked,
            "only the stable eligible saved review may fence independent refill");
        let current = d.clone();
        let result = transaction(&mut d, NOW + 33, 2, &prepared);
        if blocked {
            let (_, request) = result.unwrap().expect("ineligible review must not idle an independent free worker");
            assert_eq!(request["items"][0]["id"], "k");
            assert_eq!(crate::row(&d, "items", "i").unwrap()["draft"], "Owner changed the draft");
        } else {
            assert!(result.is_err(), "now stable review must drain owners instead of refilling with stale speculative permit");
            assert_eq!(d, current);
            assert!(crate::row(&d, "items", "k").unwrap()["autoPreparation"]["jobId"].is_null());
        }
        no_external_intent(&d);
    }
}

async fn app_fixture(width: usize, video: bool) -> (crate::App, tempfile::TempDir) {
    let (mut app, temp) = crate::tests::test_app().await;
    app.preparation_workers = Arc::new(crate::preparation_workers::Pool::new(width).unwrap());
    let mut initial = source();
    let at = chrono::Utc::now().timestamp();
    for item in initial["items"].as_array_mut().unwrap() {
        item["createdAt"] = json!(super::super::stamp(at - 60));
        item["providerObservedAt"] = json!(super::super::stamp(at));
    }
    if video { initial["posts"][0]["attachments"] = json!([{"type":"video","url":"https://example.invalid/no-network.mp4"}]); }
    for key in ["knowledge_entries", "knowledge_versions", "feedback"] { initial[key] = json!([]); }
    let token = crate::runtime_lifecycle::admission_token(&initial, crate::runtime_lifecycle::AdmissionClass::Preparation).unwrap();
    app.lifecycle_owner = Arc::new(crate::runtime_lifecycle::RuntimeIdentity {
        account: token.account.clone(), runtime_id: token.runtime_id.clone(), release_sha256: token.release_sha256.clone(),
    });
    app.db.change(|d| { *d = initial; Ok(()) }).await.unwrap();
    (app, temp)
}
async fn signal(wake: &Notify, label: &str) {
    tokio::time::timeout(std::time::Duration::from_secs(3), wake.notified()).await.expect(label);
}

#[tokio::test]
async fn completed_recovery_hold_uses_exact_job_and_fresh_plan_status() {
    for (status, plan, eligible) in [("failed", "plan", true), ("interrupted", "plan", true),
        ("completed", "plan", false), ("running", "plan", false), ("failed", "new-plan", false)] {
        let (app, _temp) = app_fixture(1, false).await;
        app.db.change(|d| {
            d["jobs"].as_array_mut().unwrap().extend([
                json!({"id":"hold-target","kind":"assistant","refId":"","status":status,
                    "result":{"paidEvidence":"retained"},"preparationStages":{"reviewChunks":{
                        "planDigest":plan,"chunks":[{"attempts":[{"status":"unknown","requestDigest":"original"}]}]}}}),
                json!({"id":"unrelated","kind":"assistant","refId":"","status":"failed","result":{"keep":true}}),
            ]);
            Ok(())
        }).await.unwrap();
        let before = app.read().await.unwrap();
        super::super::record_completed_recovery_hold(&app, "hold-target", "plan", "RECOVERY_PROFILE_CHANGED").await.unwrap();
        let mut after = app.read().await.unwrap();
        if eligible {
            let job = crate::row_mut(&mut after, "jobs", "hold-target").unwrap();
            let hold = job.as_object_mut().unwrap().remove("completedRecoveryHold").unwrap();
            assert_eq!(hold["expectedPlanDigest"], "plan");
            assert_eq!(hold["reason"], "RECOVERY_PROFILE_CHANGED");
            assert!(hold["heldAt"].as_str().is_some());
        }
        assert_eq!(after, before, "hold cannot retarget a changed plan/status or alter paid evidence and unrelated records");
        app.db.close().await;
    }
}

#[tokio::test]
async fn completed_recovery_hold_rejects_foreign_runtime_owner_without_mutation() {
    let (mut app, _temp) = app_fixture(1, false).await;
    app.db.change(|d| {
        d["jobs"].as_array_mut().unwrap().push(json!({"id":"hold-target","kind":"assistant","refId":"",
            "status":"failed","preparationStages":{"reviewChunks":{"planDigest":"plan"}}}));
        Ok(())
    }).await.unwrap();
    let before = app.read().await.unwrap();
    let mut identity = (*app.lifecycle_owner).clone();
    identity.runtime_id = "foreign-runtime".into();
    app.lifecycle_owner = Arc::new(identity);
    assert!(super::super::record_completed_recovery_hold(&app, "hold-target", "plan", "RECOVERY_PROFILE_CHANGED").await.is_err());
    assert_eq!(app.read().await.unwrap(), before);
    app.db.close().await;
}

#[tokio::test]
async fn actual_fill_commits_ready_second_family_while_first_preflight_is_suspended() {
    let (app, _temp) = app_fixture(2, false).await;
    let slow_started = Arc::new(Notify::new()); let release = Arc::new(Notify::new());
    let fast_committed = Arc::new(Notify::new()); let recorded = Arc::new(Mutex::new(Vec::new()));
    let task = tokio::spawn({
        let app = app.clone(); let release = release.clone(); let slow_started = slow_started.clone();
        let fast_committed = fast_committed.clone(); let recorded = recorded.clone();
        async move { fill_with(&app, move |snapshot, candidate| {
            let release = release.clone(); let slow_started = slow_started.clone();
            async move {
                if candidate.item_ids == vec!["i"] { slow_started.notify_one(); release.notified().await; }
                preflight_with(&snapshot, candidate, |requests| async move {
                    Ok(requests.into_iter().map(|_| Size::Fits(100)).collect())
                }).await
            }
        }, move |job, request| {
            let id = request["items"][0]["id"].as_str().unwrap().to_owned();
            recorded.lock().unwrap().push((job, id.clone()));
            if id == "j" { fast_committed.notify_one(); }
        }).await }
    });
    signal(&slow_started, "first capacity future must actually be suspended").await;
    signal(&fast_committed, "ready sibling must commit before releasing slow preflight").await;
    let state = app.read().await.unwrap();
    assert_eq!(recorded.lock().unwrap().iter().map(|(_, id)| id.as_str()).collect::<Vec<_>>(), vec!["j"]);
    assert_eq!(crate::row(&state, "items", "j").unwrap()["autoPreparation"]["attempts"], 1);
    assert!(crate::row(&state, "items", "i").unwrap()["autoPreparation"]["jobId"].is_null());
    release.notify_one(); task.await.unwrap().unwrap();
    assert_eq!(recorded.lock().unwrap().len(), 2, "both exact captures admitted once");
    no_external_intent(&app.read().await.unwrap()); app.db.close().await;
}

#[tokio::test]
async fn actual_fill_drains_valid_sibling_after_earlier_preflight_error() {
    let (app, _temp) = app_fixture(2, false).await;
    let valid_started = Arc::new(Notify::new()); let release = Arc::new(Notify::new());
    let recorded = Arc::new(Mutex::new(Vec::new()));
    let task = tokio::spawn({
        let app = app.clone(); let valid_started = valid_started.clone(); let release = release.clone(); let recorded = recorded.clone();
        async move { fill_with(&app, move |snapshot, candidate| {
            let valid_started = valid_started.clone(); let release = release.clone();
            async move {
                if candidate.item_ids == vec!["i"] { return Err(crate::bad("hostile first preflight failure")); }
                if candidate.item_ids == vec!["j"] {valid_started.notify_one(); release.notified().await;}
                preflight_with(&snapshot, candidate, |requests| async move {
                    Ok(requests.into_iter().map(|_| Size::Fits(100)).collect())
                }).await
            }
        }, move |_job, request| { recorded.lock().unwrap().push(request["items"][0]["id"].clone()); }).await }
    });
    signal(&valid_started, "first failure must not drop unpolled sibling future").await;
    release.notify_one(); let error = task.await.unwrap().unwrap_err();
    assert!(error.1.contains("hostile first preflight failure"));
    let mut admitted=recorded.lock().unwrap().clone();admitted.sort_by_key(Value::to_string);
    assert_eq!(admitted, vec![json!("j"),json!("k")], "failed head cannot hide third ready family");
    let state = app.read().await.unwrap();
    assert!(crate::row(&state, "items", "i").unwrap()["autoPreparation"]["attempts"].as_u64().unwrap_or(0) == 0);
    assert_eq!(crate::row(&state, "items", "j").unwrap()["autoPreparation"]["attempts"], 1);
    no_external_intent(&state); app.db.close().await;
}

#[tokio::test]
async fn actual_fill_oversize_hold_does_not_stop_later_valid_family() {
    let (app, _temp) = app_fixture(2, false).await; let recorded = Arc::new(Mutex::new(Vec::new()));
    fill_with(&app, |snapshot, candidate| async move {
        let oversized = candidate.item_ids == vec!["i"];
        preflight_with(&snapshot, candidate, move |requests| async move {
            Ok(requests.into_iter().map(|_| if oversized { Size::Oversized } else { Size::Fits(100) }).collect())
        }).await
    }, { let recorded = recorded.clone(); move |_job, request| { recorded.lock().unwrap().push(request["items"][0]["id"].clone()); } }).await.unwrap();
    let mut admitted=recorded.lock().unwrap().clone();admitted.sort_by_key(Value::to_string);
    assert_eq!(admitted, vec![json!("j"),json!("k")], "capacity hold releases only its own dependency");
    let state = app.read().await.unwrap(); let held = crate::row(&state, "items", "i").unwrap();
    assert_eq!(held["autoPreparation"]["status"], "needs_attention"); assert_eq!(held["autoPreparation"]["attempts"], 0);
    assert!(held["autoPreparation"]["jobId"].is_null());
    assert_eq!(crate::row(&state, "items", "j").unwrap()["autoPreparation"]["attempts"], 1);
    no_external_intent(&state); app.db.close().await;
}

#[tokio::test]
async fn actual_fill_uses_reconciled_video_snapshot_without_capacity_capture_drift() {
    let (app, _temp) = app_fixture(2, true).await; let recorded = Arc::new(Mutex::new(Vec::new()));
    fill_with(&app, |snapshot, candidate| async move {
        preflight_with(&snapshot, candidate, |requests| async move {
            Ok(requests.into_iter().map(|_| Size::Fits(100)).collect())
        }).await
    }, { let recorded = recorded.clone(); move |_job, request| { recorded.lock().unwrap().push(request["items"][0]["id"].clone()); } }).await.unwrap();
    let observed = recorded.lock().unwrap().clone();
    assert!(observed.contains(&json!("i")), "default missing-media text assessment remains schedulable after source reducers");
    assert!(observed.contains(&json!("j")), "independent text family also advances");
    let state = app.read().await.unwrap();
    assert!(crate::list(&state, "jobs").iter().any(|job| job["kind"] == "assistant"));
    no_external_intent(&state); app.db.close().await;
}

#[tokio::test]
async fn actual_fill_has_only_worker_width_outstanding_probes_on_one_shared_snapshot() {
    let (app, _temp) = app_fixture(2, false).await;
    let entered = Arc::new(tokio::sync::Barrier::new(3));
    let release = Arc::new(tokio::sync::Barrier::new(3));
    let probes = Arc::new(Mutex::new(Vec::new()));
    let task = tokio::spawn({
        let app = app.clone(); let entered = entered.clone(); let release = release.clone(); let probes = probes.clone();
        async move { fill_with(&app, move |snapshot, candidate| {
            let entered = entered.clone(); let release = release.clone(); let probes = probes.clone();
            async move {
                probes.lock().unwrap().push((Arc::as_ptr(&snapshot) as usize, candidate.item_ids.clone()));
                entered.wait().await; release.wait().await;
                preflight_with(&snapshot, candidate, |requests| async move {
                    Ok(requests.into_iter().map(|_| Size::Fits(100)).collect())
                }).await
            }
        }, |_job, _request| {}).await }
    });
    tokio::time::timeout(std::time::Duration::from_secs(3), entered.wait()).await
        .expect("real producer must poll exactly its independent bounded window");
    {
        let observed = probes.lock().unwrap(); assert_eq!(observed.len(), 2);
        assert_eq!(observed[0].0, observed[1].0, "unpaid descriptors share one aggregate source snapshot");
        assert!(observed.iter().all(|(_, ids)| ids != &vec!["k".to_owned()]), "third ready family stays in durable queue");
    }
    let state = app.read().await.unwrap();
    assert!(crate::list(&state, "jobs").iter().all(|job| job["kind"] != "assistant"));
    release.wait().await; task.await.unwrap().unwrap();
    let state = app.read().await.unwrap();
    assert!(crate::row(&state, "items", "k").unwrap()["autoPreparation"]["jobId"].is_null());
    no_external_intent(&state); app.db.close().await;
}

#[tokio::test]
async fn actual_fill_deadline_retires_only_hanging_unpaid_descriptor_and_allows_rediscovery() {
    let (app, _temp) = app_fixture(2, false).await;
    let recorded = Arc::new(Mutex::new(Vec::new()));
    let error = tokio::time::timeout(std::time::Duration::from_secs(3), fill_with_deadline(&app,
        |snapshot, candidate| async move {
            if candidate.item_ids == vec!["i"] {
                // Real producer callback remains unpaid forever; its deadline
                // must retire this future rather than stall all future wakes.
                std::future::pending::<()>().await;
            }
            preflight_with(&snapshot, candidate, |requests| async move {
                Ok(requests.into_iter().map(|_| Size::Fits(100)).collect())
            }).await
        }, { let recorded = recorded.clone(); move |_job, request| {
            recorded.lock().unwrap().push(request["items"][0]["id"].clone());
        } }, std::time::Duration::from_millis(20))).await
        .expect("bounded unpaid descriptor lifetime must let producer return").unwrap_err();
    assert!(!error.1.is_empty(), "expiry is a visible local preflight error");
    let mut admitted=recorded.lock().unwrap().clone();admitted.sort_by_key(Value::to_string);
    assert_eq!(admitted, vec![json!("j"),json!("k")]);
    let mut state = app.read().await.unwrap();
    let unattempted = crate::row(&state, "items", "i").unwrap();
    assert!(unattempted["autoPreparation"]["jobId"].is_null());
    assert!(unattempted["autoPreparation"]["attempts"].as_u64().unwrap_or(0) == 0);
    assert_eq!(crate::row(&state, "items", "j").unwrap()["autoPreparation"]["attempts"], 1);
    let at = chrono::Utc::now().timestamp(); super::super::reconcile_claim_state(&mut state, at).unwrap();
    assert!(discover(&state, at, 2).unwrap().is_empty(), "both admitted sibling slots remain owned");
    for id in ["j","k"] {
        let job=crate::row(&state,"items",id).unwrap()["autoPreparation"]["jobId"].as_str().unwrap().to_owned();
        complete_captured(&mut state,&job,&json!({"text":"Retained local assessment","sources":[],
            "assessments":[{"itemId":id,"outcome":"needs_attention","reason":"Retained local evidence"}],"proposals":[]}),at).unwrap();
        crate::row_mut(&mut state,"jobs",&job).unwrap()["status"]=json!("completed");
    }
    let next = discover(&state, at, 2).unwrap();
    assert_eq!(next.len(), 1); assert_eq!(next[0].item_ids, vec!["i"], "unpaid expiry grants no paid retry or family release");
    no_external_intent(&state); app.db.close().await;
}

#[tokio::test]
async fn actual_family_split_rounds_bound_callback_payloads_and_preserve_unclaimed_tail() {
    let mut d = fixture();
    for number in 0..32 { add_recipient(&mut d, &format!("family-{number:02}"), "post"); }
    running(&mut d); super::super::reconcile_claim_state(&mut d, NOW).unwrap();
    let snapshot = d.clone();
    let mut candidates = discover(&snapshot, NOW, 2).unwrap();
    assert_eq!(candidates.len(), 1, "one same-post family is one bounded speculative capture");
    let candidate = candidates.remove(0);
    assert_eq!(candidate.item_ids.len(), 33, "native grouping must exercise an expanded split round");
    let original_ids = candidate.item_ids.clone();
    let observed = Arc::new(Mutex::new(Vec::new())); let log = observed.clone();
    let prepared = preflight_with(&snapshot, candidate, move |requests| {
        let log = log.clone();
        async move {
            let raw_bytes: usize = requests.iter().map(|request| serde_json::to_vec(request).unwrap().len()).sum();
            assert!(requests.len() <= 16, "expanded split round must flush before retaining request 17");
            assert!(raw_bytes <= 8 * 1024 * 1024, "capacity callback payload must stay bounded");
            log.lock().unwrap().push((requests.len(), raw_bytes,
                requests.iter().map(|request| request["items"].as_array().unwrap().len()).collect::<Vec<_>>()));
            Ok(requests.into_iter().map(|request| {
                if request["items"].as_array().unwrap().len() > 1 { Size::Oversized } else { Size::Fits(100) }
            }).collect())
        }
    }).await.unwrap();
    assert_eq!(d, snapshot, "all split rounds remain unpaid before commit");
    assert!(observed.lock().unwrap().iter().any(|(count, _, _)| *count == 16), "fixture reaches the callback-count boundary");
    assert_eq!(prepared.selected, vec![original_ids[0].clone()], "first fitted leaf preserves captured recipient order");
    assert!(prepared.oversized.is_empty()); assert!(prepared.held_captures.is_empty());
    let (job, request) = transaction(&mut d, NOW, 2, &prepared).unwrap().unwrap();
    assert_eq!(request["items"][0]["id"], original_ids[0]);
    for id in &original_ids[1..] {
        let item = crate::row(&d, "items", id).unwrap();
        assert_eq!(item["autoPreparation"]["status"], "queued");
        assert_eq!(item["autoPreparation"]["attempts"], 0); assert!(item["autoPreparation"]["jobId"].is_null());
    }
    assert!(discover(&d, NOW, 2).unwrap().is_empty(), "same-family tail must wait for the committed owner");
    let answer = json!({"text":"Reviewed singleton", "sources":[],
        "assessments":[{"itemId":original_ids[0],"outcome":"needs_attention","reason":"Keep reviewed evidence"}],"proposals":[]});
    complete_captured(&mut d, &job, &answer, NOW).unwrap();
    crate::row_mut(&mut d, "jobs", &job).unwrap()["status"] = json!("completed");
    let restarted: Value = serde_json::from_str(&d.to_string()).unwrap();
    let next = discover(&restarted, NOW, 2).unwrap();
    assert_eq!(next.len(), 1);
    assert_eq!(next[0].item_ids, original_ids[1..], "split tail is rediscovered in original order without replaying paid leaf");
    no_external_intent(&d);
}

// Generation 2: exact initial-admission / first-call boundary. Both pure and
// App checks call the reducer used before the real worker's bridge invocation.
fn first_transaction(d: &mut Value, token: &crate::runtime_lifecycle::OwnerToken,
    run: &str, request: &Value, keys: Option<&std::collections::BTreeSet<String>>)
    -> crate::ApiResult<()> {
    let mut staged = d.clone();
    super::super::reserve_initial_call_captured(&mut staged, token, run, request, keys, "offline-first-call")?;
    *d = staged; Ok(())
}
async fn initial_run() -> (Value, crate::runtime_lifecycle::OwnerToken, String, Value) {
    let mut d = source(); let snapshot = d.clone();
    let prepared = fits(&snapshot, discover(&snapshot, NOW, 1).unwrap().remove(0)).await;
    let token = prepared.token.clone();
    let (job, request) = transaction(&mut d, NOW, 1, &prepared).unwrap().unwrap();
    (d, token, job, request)
}
fn current_keys(d: &Value, run: &str) -> Option<std::collections::BTreeSet<String>> {
    crate::preparation_workers::keys(d, crate::row(d, "jobs", run).unwrap())
}

#[tokio::test]
async fn native_claim_records_exact_initial_marker_then_first_reserve_is_single_use() {
    let (mut d, token, run, request) = initial_run().await;
    let marker = crate::row(&d, "jobs", &run).unwrap()["preparationStages"]["initialAdmission"].clone();
    assert_eq!(marker["version"], 1); assert_eq!(marker["status"], "scheduled");
    assert_eq!(marker["owner"]["account"], token.account);
    assert_eq!(marker["owner"]["epoch"], token.epoch);
    assert_eq!(marker["requestSha256"], crate::editorial_review::hash_text(&request.to_string()));
    assert!(crate::row(&d, "jobs", &run).unwrap()["preparationStages"]["firstAdmission"].is_null());
    let keys = current_keys(&d, &run);
    first_transaction(&mut d, &token, &run, &request, keys.as_ref()).unwrap();
    let reserved = d.clone();
    assert_eq!(crate::row(&d, "jobs", &run).unwrap()["preparationStages"]["firstAdmission"]["status"], "reserved");
    assert_eq!(crate::row(&d, "items", "i").unwrap()["autoPreparation"]["attempts"], 1);
    assert!(first_transaction(&mut d, &token, &run, &request, keys.as_ref()).is_err());
    assert_eq!(d, reserved, "repeating reservation cannot become an implicit first-pass retry");
    assert_eq!(crate::row(&d, "jobs", &run).unwrap()["preparationStages"]["initialAdmission"], marker);
    no_external_intent(&d);
}

#[tokio::test]
async fn native_cancellation_after_claim_blocks_first_call_without_changing_paid_owner() {
    let (mut d, token, run, request) = initial_run().await; let keys = current_keys(&d, &run);
    crate::row_mut(&mut d, "jobs", &run).unwrap()["status"] = json!("cancelled");
    let cancelled = d.clone();
    assert!(first_transaction(&mut d, &token, &run, &request, keys.as_ref()).is_err());
    assert_eq!(d, cancelled, "first-call rejection preserves the cancellation and claim evidence");
    assert!(crate::row(&d, "jobs", &run).unwrap()["preparationStages"]["firstAdmission"].is_null());
    assert_eq!(crate::row(&d, "items", "i").unwrap()["autoPreparation"]["attempts"], 1);
    no_external_intent(&d);
}

#[tokio::test]
async fn current_draining_token_and_resumed_epoch_cannot_start_old_claim_first_call() {
    let (mut d, token, run, request) = initial_run().await; let keys = current_keys(&d, &run);
    let drain = crate::runtime_lifecycle::begin_drain(&mut d, &token, &"c".repeat(64), "g2-drain", false).unwrap();
    let frozen = d.clone();
    for owner in [&token, &drain] {
        assert!(first_transaction(&mut d, owner, &run, &request, keys.as_ref()).is_err());
        assert_eq!(d, frozen, "both stale and current Draining tokens reject unpaid first call");
    }
    let native = crate::runtime_lifecycle::SettledNative {
        owner: drain.clone(), application_tasks: 0, provider_queued: 0, provider_dispatched: 0,
        provider_contained: true, credential_writers: 0, unresolved_effects: 0,
    };
    let current = crate::runtime_lifecycle::resume_same_owner(&mut d, &drain, &native).unwrap();
    let resumed = d.clone();
    assert!(first_transaction(&mut d, &current, &run, &request, keys.as_ref()).is_err(),
        "a current Running token must not adopt initial admission from a prior epoch");
    assert_eq!(d, resumed);
    assert!(crate::row(&d, "jobs", &run).unwrap()["preparationStages"]["firstAdmission"].is_null());
    no_external_intent(&d);
}

#[tokio::test]
async fn legacy_missing_marker_or_changed_request_never_bootstraps_a_first_paid_retry() {
    for fault in ["missing_marker", "legacy_stages", "changed_request", "prior_epoch_marker"] {
        let (mut d, token, run, mut request) = initial_run().await;
        match fault {
            "missing_marker" => { crate::row_mut(&mut d, "jobs", &run).unwrap()["preparationStages"]
                .as_object_mut().unwrap().remove("initialAdmission"); },
            "legacy_stages" => { crate::row_mut(&mut d, "jobs", &run).unwrap().as_object_mut().unwrap().remove("preparationStages"); },
            "changed_request" => request["items"][0]["text"] = json!("Altered after claim"),
            _ => crate::row_mut(&mut d, "jobs", &run).unwrap()["preparationStages"]["initialAdmission"]["owner"]["epoch"] = json!(token.epoch + 1),
        }
        let quarantined = d.clone(); let keys = current_keys(&d, &run);
        assert!(first_transaction(&mut d, &token, &run, &request, keys.as_ref()).is_err(), "{fault}");
        assert_eq!(d, quarantined, "{fault}: rejection cannot repair/adopt marker or issue a fresh attempt");
        assert!(crate::row(&d, "jobs", &run).unwrap()["preparationStages"]["firstAdmission"].is_null());
        assert_eq!(crate::row(&d, "items", "i").unwrap()["autoPreparation"]["attempts"], 1);
        no_external_intent(&d);
    }
}

#[tokio::test]
async fn new_native_auto_revalidate_run_gets_explicit_marker_and_single_first_admission() {
    let mut d = review_waiting();
    super::super::reconcile_claim_state(&mut d, NOW + 33).unwrap();
    assert!(super::super::revalidation::ready(&d, NOW + 33).unwrap(), "native saved review must actually be ready");
    let snapshot = d.clone();
    let mut candidates = discover(&snapshot, NOW + 33, 2).unwrap();
    assert_eq!(candidates.len(), 1); assert!(!candidates[0].initial, "exclusive revalidation uses its native captured request");
    let prepared = preflight_with(&snapshot, candidates.remove(0), |_requests| async move {
        panic!("historical review must not enter new-generation capacity splitting")
    }).await.unwrap();
    let token = prepared.token.clone();
    let (run, request) = transaction(&mut d, NOW + 33, 2, &prepared).unwrap().unwrap();
    let job = crate::row(&d, "jobs", &run).unwrap(); assert_eq!(job["purpose"], "auto_revalidate");
    assert!(job["preparationStages"].is_object(), "native revalidation lacked stages before the fresh claim marker seam");
    assert_eq!(job["preparationStages"]["initialAdmission"]["requestSha256"], crate::editorial_review::hash_text(&request.to_string()));
    first_transaction(&mut d, &token, &run, &request, None).unwrap();
    let admitted = d.clone(); assert!(first_transaction(&mut d, &token, &run, &request, None).is_err());
    assert_eq!(d, admitted, "auto_revalidate is explicit and single-use, not an initial-run bypass");
    no_external_intent(&d);
}

#[tokio::test]
async fn actual_app_first_call_writer_rejects_cancelled_durable_job_before_dispatch() {
    let (app, _temp) = app_fixture(1, false).await;
    let mut admitted = None;
    fill_with(&app, |snapshot, candidate| async move {
        preflight_with(&snapshot, candidate, |requests| async move {
            Ok(requests.into_iter().map(|_| Size::Fits(100)).collect())
        }).await
    }, |run, request| { admitted = Some((run, request)); }).await.unwrap();
    let (run, request) = admitted.expect("actual producer must commit a native marker-bearing job");
    let state = app.read().await.unwrap(); let keys = current_keys(&state, &run);
    assert_eq!(crate::row(&state, "jobs", &run).unwrap()["preparationStages"]["initialAdmission"]["status"], "scheduled");
    app.change_job(&run, |d| { crate::row_mut(d, "jobs", &run)?["status"] = json!("cancelled"); Ok(()) }).await.unwrap();
    let cancelled = app.read().await.unwrap();
    let result = super::super::reserve_initial_call(&app, &run, &request, keys.as_ref()).await;
    assert!(result.is_err(), "production App writer reducer must reject native cancellation before bridge dispatch");
    assert_eq!(app.read().await.unwrap(), cancelled);
    assert!(crate::row(&cancelled, "jobs", &run).unwrap()["preparationStages"]["firstAdmission"].is_null());
    no_external_intent(&cancelled); app.db.close().await;
}
