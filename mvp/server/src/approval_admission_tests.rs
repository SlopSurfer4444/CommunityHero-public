use super::*;
use axum::Extension;

fn actor() -> operator_auth::Actor { operator_auth::Actor::local_owner("synthetic") }

async fn setup() -> (App, tempfile::TempDir, Value) { setup_with_alias(false).await }

async fn setup_with_alias(alias: bool) -> (App, tempfile::TempDir, Value) {
    setup_with_source(alias,false).await
}

async fn setup_with_source(alias: bool, complete_post: bool) -> (App, tempfile::TempDir, Value) {
    let (app, temp) = crate::tests::test_app().await;
    let refs = app.change(|d| {
        let mut second = d["items"][0].clone();
        second["id"] = json!("item-2"); second["itemId"] = json!(if alias {"comment-1"} else {"comment-2"});
        list_mut(d, "items").push(second);
        if complete_post {
            for item in ["item-1","item-2"] {crate::tests::create_post_fixture(d,item)?;}
        }
        let mut refs = vec![];
        for item in ["item-1", "item-2"] {
            let p = create_proposal(d, &json!({"itemId":item,"expectedRevision":1,"kind":"close"}))?;
            refs.push(json!({"id":p["id"],"revision":p["revision"]}));
        }
        Ok(json!(refs))
    }).await.unwrap();
    (app, temp, refs)
}

fn body(refs: &Value) -> Value {
    json!({"requestId":"partial-review-1","admissionMode":"partial","proposals":refs})
}

#[tokio::test]
async fn one_stale_branch_holds_exact_reference_and_freezes_the_admitted_subset() {
    let (app, _temp, refs) = setup().await;
    app.change(|d| { bump(&mut d["items"][1]); Ok(()) }).await.unwrap();
    let result = approval_new(State(app.clone()), Extension(actor()), Json(body(&refs))).await.unwrap().0;
    assert_eq!(result["accepted"], json!([refs[0]]));
    assert_eq!(result["held"][0]["reference"], refs[1]);
    assert_eq!(result["held"][0]["reason"], "context_or_evidence_changed");
    let d = app.read().await.unwrap();
    assert_eq!(list(&d,"approvals").len(), 1);
    assert_eq!(d["approvals"][0]["proposals"].as_array().unwrap().len(), 1);
    assert_eq!(d["approvals"][0]["proposals"][0]["proposal"]["kind"], "close");
    assert_eq!(d["approvals"][0]["admission"]["requested"], refs);
    assert_eq!(d["proposals"][1]["status"], "draft");
    assert!(list(&d,"operations").is_empty());
    app.db.close().await;
}

#[tokio::test]
async fn atomic_default_rejects_whole_batch_without_mutating_it() {
    let (app, _temp, refs) = setup().await;
    app.change(|d| { bump(&mut d["items"][1]); Ok(()) }).await.unwrap();
    let result = approval_new(State(app.clone()), Extension(actor()), Json(json!({"proposals":refs}))).await;
    assert_eq!(result.unwrap_err().0, StatusCode::CONFLICT);
    let d = app.read().await.unwrap();
    assert!(list(&d,"approvals").is_empty());
    assert!(list(&d,"proposals").iter().all(|p| p["status"] == "draft"));
    app.db.close().await;
}

#[tokio::test]
async fn shared_scope_failure_holds_every_reference_without_executable_approval() {
    let (app, _temp, refs) = setup().await;
    // Storage rejects invalid workspace bindings before persistence. Exercise
    // the admission guard directly against a hostile consistent snapshot.
    let mut d = app.read().await.unwrap();
    d["connectorBinding"] = json!({"accountId":"foreign"});
    let request = body(&refs);
    let identity = local_admission::request(&d,"approval",&request,&actor()).unwrap().unwrap();
    let mut result = create(&mut d,&actor(),&request).unwrap();
    local_admission::commit(&mut d,&identity,&mut result).unwrap();
    assert_eq!(result["status"], "held"); assert!(result["id"].is_null());
    assert!(result["accepted"].as_array().unwrap().is_empty());
    assert_eq!(result["held"].as_array().unwrap().len(), 2);
    assert!(result["held"].as_array().unwrap().iter().all(|r| r["reason"] == "shared_scope_or_authority_invalid"));
    let replay = local_admission::replay(&d,&identity,&actor()).unwrap().unwrap();
    assert_eq!(replay["held"],result["held"]); assert_eq!(replay["replayed"],true);
    assert!(list(&d,"approvals").is_empty()); assert!(list(&d,"operations").is_empty());
    app.db.close().await;
}

#[tokio::test]
async fn unknown_and_dispatching_recipients_are_held_without_retry_permission() {
    for status in ["unknown", "dispatching"] {
        let (app, _temp, refs) = setup().await;
        app.change(|d| { list_mut(d,"operations").push(json!({"id":"prior-op","itemId":"item-1","status":status})); Ok(()) }).await.unwrap();
        let result = approval_new(State(app.clone()), Extension(actor()), Json(body(&refs))).await.unwrap().0;
        assert_eq!(result["accepted"],json!([refs[1]]));
        assert_eq!(result["held"][0]["reason"],"recipient_operation_blocked");
        let d=app.read().await.unwrap(); assert_eq!(list(&d,"operations").len(),1);
        assert_eq!(d["operations"][0]["status"],status); app.db.close().await;
    }
}

#[tokio::test]
async fn concurrent_duplicate_and_restart_replay_keep_one_immutable_receipt() {
    let (mut app, temp, refs) = setup().await;
    app.change(|d| { bump(&mut d["items"][1]); Ok(()) }).await.unwrap();
    let request=body(&refs);
    let (a,b)=tokio::join!(
        approval_new(State(app.clone()),Extension(actor()),Json(request.clone())),
        approval_new(State(app.clone()),Extension(actor()),Json(request.clone())));
    let first=a.unwrap().0; assert_eq!(first["id"],b.unwrap().0["id"]);
    app.db.close().await;
    app.db=Database::Sqlite(open_db(&temp.path().join("workspace.sqlite")).await.unwrap());
    app.change(|d| { recover(d).unwrap(); bump(&mut d["items"][0]); Ok(()) }).await.unwrap();
    let replay=approval_new(State(app.clone()),Extension(actor()),Json(request.clone())).await.unwrap().0;
    assert_eq!(replay["id"],first["id"]); assert_eq!(replay["accepted"],first["accepted"]);
    assert_eq!(replay["held"],first["held"]); assert_eq!(replay["replayed"],true);
    let mut changed=request; changed["proposals"][0]["revision"]=json!(99);
    assert_eq!(approval_new(State(app.clone()),Extension(actor()),Json(changed)).await.unwrap_err().0,StatusCode::CONFLICT);
    let d=app.read().await.unwrap(); assert_eq!(list(&d,"approvals").len(),1);
    assert_eq!(list(&d,"audit").iter().filter(|r|r["action"]==local_admission::ACTION).count(),1);
    app.db.close().await;
}

#[tokio::test]
async fn duplicate_references_are_all_held_and_partial_requires_request_identity() {
    let (app,_temp,refs)=setup().await;
    let request=body(&json!([refs[0],refs[0],refs[1]]));
    let result=approval_new(State(app.clone()),Extension(actor()),Json(request)).await.unwrap().0;
    assert_eq!(result["accepted"],json!([refs[1]]));
    assert_eq!(result["held"].as_array().unwrap().len(),2);
    assert!(result["held"].as_array().unwrap().iter().all(|r|r["reason"]=="duplicate_recipient"));
    assert!(partial(&json!({"admissionMode":"partial"})).is_err());
    assert!(partial(&json!({"admissionMode":"whatever","requestId":"x"})).is_err());
    app.db.close().await;
}

#[tokio::test]
async fn new_manual_reply_approval_requires_exact_editorial_receipt_and_marks_only_new_approval() {
    let (app,_temp)=crate::tests::test_app_with_post().await;
    let r=app.change(|d| {
        let p=create_proposal(d,&json!({"itemId":"item-1","expectedRevision":1,"kind":"reply_and_close","text":"Спасибо 🙂"}))?;
        Ok(json!({"id":p["id"],"revision":p["revision"]}))
    }).await.unwrap();
    let mut d=app.read().await.unwrap();let before=d.clone();
    assert!(create(&mut d,&actor(),&json!({"proposals":[r]})).is_err());assert_eq!(d,before);
    crate::editorial_review::fixture_accept(&mut d,r["id"].as_str().unwrap()).unwrap();
    let approval=create(&mut d,&actor(),&json!({"proposals":[r]})).unwrap();
    assert_eq!(approval["editorialPolicyVersion"],1);
    assert_eq!(approval["proposals"][0]["proposal"]["editorialReview"]["decision"],"accept");
    let immutable=d["approvals"][0].clone();d["proposals"][0]["text"]=json!("Later edit");d["proposals"][0]["revision"]=json!(2);
    assert!(create(&mut d,&actor(),&json!({"proposals":[{"id":r["id"],"revision":2}]})).is_err());
    assert_eq!(d["approvals"][0],immutable);assert!(list(&d,"operations").is_empty());app.db.close().await;
}

#[tokio::test]
async fn partial_admission_holds_unreviewed_reply_but_keeps_valid_nonreply_without_new_model() {
    let (app,_temp,close_refs)=setup_with_source(false,true).await;
    let mut d=app.read().await.unwrap();
    let revision=d["items"][0]["revision"].clone();
    let reply=create_proposal(&mut d,&json!({"itemId":"item-1","expectedRevision":revision,"kind":"reply_and_close","text":"Manual reply"})).unwrap();
    let result=create(&mut d,&actor(),&json!({"requestId":"editorial-partial","admissionMode":"partial",
        "proposals":[{"id":reply["id"],"revision":reply["revision"]},close_refs[1]]})).unwrap();
    assert_eq!(result["accepted"],json!([close_refs[1]]));assert_eq!(result["held"][0]["reason"],"context_or_evidence_changed");
    assert_eq!(proposal_current(&d,&reply).unwrap_err().1,"legacy_material_contract_unmet");
    assert!(list(&d,"jobs").is_empty(),"HOLD must not invent a paid editorial capture");
    assert!(d["approvals"][0].get("editorialPolicyVersion").is_none());assert!(list(&d,"operations").is_empty());app.db.close().await;
}

#[tokio::test]
async fn legacy_reply_approval_without_material_proof_holds_without_dispatch() {
    let (app,_temp)=crate::tests::test_app().await;
    let key=app.change(|d| {
        // Reach the legacy material guard through an explicitly admitted
        // synthetic connection; the shared test_app gate stays closed.
        connection_gate::fixture_open(d)?;
        let mut p=create_proposal(d,&json!({"itemId":"item-1","expectedRevision":1,"kind":"reply_and_close","text":"Previously approved exact reply"}))?;
        // A persisted pre-policy approval: construct its historical exact
        // snapshot without calling the new admission gate or inventing proof.
        p.as_object_mut().unwrap().remove("nativeCreationOrigin");
        row_mut(d,"proposals",p["id"].as_str().unwrap())?.as_object_mut().unwrap().remove("nativeCreationOrigin");
        let item=row(d,"items","item-1")?.clone();
        let key=id();
        list_mut(d,"approvals").push(json!({"id":key,"status":"approved","createdAt":now(),
            "approvedBy":actor().public_json(),"approvalAuthority":dispatch_authority::approval_binding(&actor()),
            "proposals":[{"id":p["id"],"revision":p["revision"],"proposal":p,"item":item}]}));
        row_mut(d,"proposals",p["id"].as_str().unwrap())?["status"]=json!("approved");
        Ok(key)
    }).await.unwrap();
    let before=app.read().await.unwrap();
    assert!(before["approvals"][0].get("editorialPolicyVersion").is_none());
    assert!(before["proposals"][0].get("editorialReview").is_none());
    let mut foreign=actor();foreign.id="different-reviewer".into();
    assert_eq!(execute(State(app.clone()),Extension(foreign),Path(key.clone())).await.unwrap_err().0,StatusCode::FORBIDDEN);
    assert!(list(&app.read().await.unwrap(),"operations").is_empty());
    let held=execute(State(app.clone()),Extension(actor()),Path(key)).await.unwrap_err();
    assert_eq!(held.0,StatusCode::CONFLICT);assert_eq!(held.1,"legacy_material_contract_unmet");
    let after=app.read().await.unwrap();
    assert_eq!(after,before,"A legacy approval supplies no new mandatory proof or retry authority");
    assert!(list(&after,"operations").is_empty());assert!(app.tasks.lock().await.is_empty());
    app.db.close().await;
}

#[tokio::test]
async fn local_aliases_of_one_external_recipient_cannot_admit_competing_actions() {
    let (app,_temp,refs)=setup_with_alias(true).await;
    let atomic=approval_new(State(app.clone()),Extension(actor()),Json(json!({"proposals":refs}))).await;
    assert_eq!(atomic.unwrap_err().0,StatusCode::BAD_REQUEST);
    let result=approval_new(State(app.clone()),Extension(actor()),Json(body(&refs))).await.unwrap().0;
    assert_eq!(result["status"],"held"); assert!(result["id"].is_null());
    assert_eq!(result["held"].as_array().unwrap().len(),2);
    assert!(result["held"].as_array().unwrap().iter().all(|r|r["reason"]=="duplicate_recipient"));
    let d=app.read().await.unwrap(); assert!(list(&d,"approvals").is_empty());
    assert!(list(&d,"operations").is_empty()); app.db.close().await;
}

#[test]
fn recipient_identity_is_scoped_to_company_connection_and_provider() {
    let first=json!({"connectorBinding":legacy_binding(),"objectId":"page","itemId":"comment"});
    let original=recipient_key(&first).unwrap();
    for (field,value) in [("accountId","BAW Russia"),("id","another-connection"),
        ("workspaceId","another-workspace"),("providerAccountId","another-provider-account")] {
        let mut other=first.clone(); other["connectorBinding"][field]=json!(value);
        assert_ne!(recipient_key(&other).unwrap(),original,"{field}");
    }
    let mut alias=first.clone(); alias["id"]=json!("another-local-row");
    assert_eq!(recipient_key(&alias).unwrap(),original);
}

#[tokio::test]
async fn unavailable_shared_authority_and_changed_global_evidence_hold_all() {
    let (app,_temp,refs)=setup().await;
    let mut invalid_actor=actor(); invalid_actor.id="remote-reviewer".into();
    invalid_actor.role="operator".into(); invalid_actor.authority_generation=None;
    let result=approval_new(State(app.clone()),Extension(invalid_actor),Json(body(&refs))).await.unwrap().0;
    assert_eq!(result["held"].as_array().unwrap().len(),2); assert!(result["id"].is_null());
    app.change(|d| {
        // Initialized workspaces select the versioned knowledge catalog, not
        // raw legacy materials. Change evidence through its actual owner.
        let rule=knowledge::save_instruction(d,&json!({"requestId":"shared-rule-v1",
            "title":"Shared rule","text":"Original shared rule"}),&now()).map_err(conflict)?;
        let fingerprints=["item-1","item-2"].iter().map(|key|
            prepare_bundle::review_fingerprint(d,key).map_err(conflict)).collect::<ApiResult<Vec<_>>>()?;
        for (index,fingerprint) in fingerprints.iter().enumerate() {
            d["proposals"][index]["reviewContextDigest"]=json!(fingerprint);
        }
        knowledge::save_instruction(d,&json!({"requestId":"shared-rule-v2",
            "title":"Shared rule","text":"Changed shared rule","entryId":rule["entry"]["id"],
            "expectedVersionId":rule["version"]["id"]}),&now()).map_err(conflict)?;
        for (index,key) in ["item-1","item-2"].iter().enumerate() {
            assert_ne!(prepare_bundle::review_fingerprint(d,key).map_err(conflict)?,fingerprints[index],
                "The fixture must actually change the selected canonical evidence");
        }
        Ok(())
    }).await.unwrap();
    let mut request=body(&refs); request["requestId"]=json!("changed-shared-evidence");
    let result=approval_new(State(app.clone()),Extension(actor()),Json(request)).await.unwrap().0;
    assert!(result["id"].is_null()); assert_eq!(result["held"].as_array().unwrap().len(),2);
    assert!(result["held"].as_array().unwrap().iter().all(|r|r["reason"]=="context_or_evidence_changed"));
    app.db.close().await;
}
