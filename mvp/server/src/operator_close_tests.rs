use super::*;

fn fixture() -> Value {
    let (mut d, _) = operator_editorial::tests::fixture();
    d["proposals"] = json!([]);
    d["posts"][0]["sourceUrl"] = json!("https://www.youtube.com/watch?v=offline-fixture");
    d["jobs"] = json!([]);
    d
}
fn ready_fixture() -> Value {
    let mut d = fixture();
    let source = media_fullframes::source_version(&d["posts"][0], d["account"].as_str().unwrap());
    let transcription = json!({"partial":false,"coverage":"full_audio","audioStatus":"transcribed",
        "mediaDurationSeconds":120.0,"audioDurationSeconds":120.0,"sourceVersion":source});
    assert!(knowledge::proven_full_audio(&transcription, &source));
    d["materials"] = json!([{"id":"owner-close-source-speech","account":d["account"],
        "postKey":d["posts"][0]["postKey"],"kind":"transcript",
        "text":"Complete synthetic speech from this exact close decision source.",
        "transcription":transcription}]);
    let admitted_at = (chrono::Utc::now() - chrono::Duration::minutes(5)).to_rfc3339();
    knowledge::sync_catalog(&mut d, &admitted_at).unwrap();
    let evidence = prepare_bundle::EvidenceContext::new(&d).strict_media_evidence(&d["posts"][0]).unwrap();
    assert_eq!(evidence["audioReady"], true, "fixture must pass the current catalog selector");
    assert_eq!(evidence["audioHasContent"], true);
    d
}
fn body(d: &Value) -> Value {
    json!({"itemId":"i0","expectedRevision":d["items"][0]["revision"],
        "kind":"close","text":"","_verifiedActor":operator_auth::Actor::local_owner("fixture").public_json()})
}
fn refs(p: &Value) -> Value {
    json!([{"id":p["id"],"revision":p["revision"]}])
}
fn approve(d: &mut Value, p: &Value) -> Value {
    create_approval(
        d,
        &operator_auth::Actor::local_owner("fixture"),
        &json!({"requestId":"close-approval","admissionMode":"partial","proposals":refs(p)}),
    )
    .unwrap()
}

#[test]
fn authenticated_empty_close_preserves_owner_proof_but_cannot_bypass_strict_media_floor() {
    let mut d = fixture();
    let before = d.clone();
    let item = d["items"][0].clone();
    assert!(
        media_queue::preparation_state(&d, &item, &now())
            .unwrap()
            .is_some()
    );
    assert!(
        !prepare_bundle::EvidenceContext::new(&d)
            .video_ready(&item)
            .unwrap()
    );
    let p = create_proposal(&mut d, &body(&before)).unwrap();
    assert_eq!(
        p["operatorCloseDecision"]["kind"],
        "authenticated_operator_close"
    );
    assert_eq!(
        p["operatorCloseDecision"]["selectedBy"]["id"],
        "local-owner"
    );
    assert!(p.get("editorialReview").is_none());
    assert!(p.get("prepareRunId").is_none());
    assert!(operator_close::current(&d, &p, &d["items"][0]).unwrap());
    let strict = media_context_gate::inspect(&prepare_bundle::EvidenceContext::new(&d), &p).unwrap();
    assert_eq!(strict["status"], "missing");
    let error = proposal_current(&d, &p).unwrap_err();
    assert_eq!(error.1, "Applicable media context missing; acquire it or obtain an exact operator exception");
    assert_eq!(d["posts"], before["posts"]);
    assert_eq!(d["jobs"], before["jobs"]);
    assert_eq!(d["materials"], before["materials"]);
    assert_eq!(d["operations"], before["operations"]);
    assert_eq!(
        d["feedback"].as_array().unwrap().last().unwrap()["actorVerified"],
        true
    );
    let a = approve(&mut d, &p);
    assert!(a["accepted"].as_array().unwrap().is_empty());
    assert!(d["operations"].as_array().unwrap().is_empty());
}

#[test]
fn generated_media_gate_and_manual_draft_review_never_grant_owner_close_authority() {
    for variation in [
        "generated",
        "unattributed",
        "operator",
        "reply",
        "hide",
        "delete",
        "nonempty",
    ] {
        let mut d = fixture();
        assert_eq!(d["items"][0]["platform"], "VK");
        let mut input = body(&d);
        match variation {
            "unattributed" => {
                input.as_object_mut().unwrap().remove("_verifiedActor");
            }
            "operator" => input["_verifiedActor"]["role"] = json!("operator"),
            "reply" => {
                input["kind"] = json!("reply_and_close");
                input["text"] = json!("A reply");
            }
            "hide" | "delete" => input["kind"] = json!(variation),
            "nonempty" => input["text"] = json!(" "),
            _ => {}
        }
        let before = d.clone();
        let result = create_proposal_impl(&mut d, &input, variation == "generated");
        if variation == "generated" {
            assert!(result.is_err(), "unmarked legacy generation keeps strict media prerequisites");
            assert_eq!(d, before);
        } else if variation == "hide" {
            // The VK recipient cannot HIDE, independently of media readiness.
            let error = result.unwrap_err();
            assert_eq!(error.0.as_u16(), 400);
            assert_eq!(error.1, "Action is unavailable for this platform");
            assert_eq!(d, before);
        } else {
            let proposal = result.unwrap();
            assert!(decision_media::enabled(&proposal), "{variation}");
            assert_eq!(proposal["status"], "draft");
            assert!(proposal.get("operatorCloseDecision").is_none(), "{variation}");
            assert!(proposal.get("editorialReview").is_none(), "{variation}");
            assert!(proposal_current(&d, &proposal).is_err(), "{variation}");
            let staged = d.clone();
            assert!(create_approval(&mut d, &operator_auth::Actor::local_owner("fixture"),
                &json!({"proposals":refs(&proposal)})).is_err(), "{variation}");
            assert_eq!(d, staged, "failed admission must not mutate state: {variation}");
            for field in ["posts", "materials", "jobs", "approvals", "operations"] {
                assert_eq!(d[field], before[field], "{variation}: {field}");
            }
        }
    }
}

#[test]
fn close_never_bypasses_item_revision_route_current_context_or_rules() {
    let mut d = ready_fixture();
    let input = body(&d);
    let mut wrong = input.clone();
    wrong["expectedRevision"] = json!(999);
    let before = d.clone();
    assert!(create_proposal(&mut d, &wrong).is_err());
    assert_eq!(d, before);
    let p = create_proposal(&mut d, &input).unwrap();
    assert!(proposal_current(&d, &p).is_ok(), "unmodified source must pass before each hostile change");
    for variation in [
        "revision", "context", "branch", "post", "route", "company", "waiting", "closed", "rules",
    ] {
        let mut changed = d.clone();
        let mut proposal = p.clone();
        match variation {
            "revision" => bump(&mut changed["items"][0]),
            "context" => changed["items"][0]["contextEvidenceDigest"] = json!("changed"),
            "branch" => {
                changed["branches"][0]["messages"] = json!([{"id":"later","text":"new context"}])
            }
            "post" => changed["posts"][0]["text"] = json!("new post"),
            "route" => proposal["routeTarget"]["objectId"] = json!("different"),
            "company" => changed["connectorBinding"]["accountId"] = json!("BAW Russia"),
            "waiting" | "closed" => changed["items"][0]["workflow"] = json!(variation),
            _ => {
                knowledge::save_instruction(&mut changed,&json!({"requestId":"new-rule","title":"Close policy","text":"Current rule changed"}),"2026-09-30T12:00:00Z").unwrap();
            }
        }
        assert!(
            proposal_current(&changed, &proposal).is_err(),
            "{variation}"
        );
    }
}

#[test]
fn owner_close_receipt_tamper_edit_and_foreign_approval_actor_fail_closed() {
    let mut d = ready_fixture();
    let input = body(&d);
    let p = create_proposal(&mut d, &input).unwrap();
    assert!(proposal_current(&d, &p).is_ok(), "media readiness must not mask proof or actor failures");
    for variation in ["kind", "text", "revision", "actor", "marker", "account"] {
        let mut altered = p.clone();
        match variation {
            "kind" => altered["kind"] = json!("reply_and_close"),
            "text" => altered["text"] = json!("new text"),
            "revision" => altered["revision"] = json!(2),
            "actor" => altered["operatorCloseDecision"]["selectedBy"]["id"] = json!("foreign"),
            "marker" => altered["operatorCloseDecision"]["decisionSha256"] = json!("0".repeat(64)),
            _ => altered["operatorCloseDecision"]["account"] = json!("foreign"),
        };
        assert!(proposal_current(&d, &altered).is_err(), "{variation}");
    }
    let mut actor = operator_auth::Actor::local_owner("fixture");
    actor.role = "operator".into();
    actor.id = "remote".into();
    let result = create_approval(
        &mut d,
        &actor,
        &json!({"requestId":"foreign","admissionMode":"partial","proposals":refs(&p)}),
    )
    .unwrap();
    assert!(result["accepted"].as_array().unwrap().is_empty());
}

#[test]
fn approval_and_operation_capture_the_exact_owner_close_proof_without_touching_history() {
    let mut d = ready_fixture();
    let fixture_owner = crate::runtime_lifecycle_startup::Admission::fixture(crate::accounts::Profile::LikeAvto);
    crate::runtime_lifecycle_startup::initialize_fixture(&mut d, fixture_owner.identity()).unwrap();
    crate::connection_gate::fixture_open(&mut d).unwrap();
    let input = body(&d);
    let p = create_proposal(&mut d, &input).unwrap();
    let a = approve(&mut d, &p);
    let old = d.clone();
    let (result, scheduled) = execute_admission::admit(
        &mut d,
        &operator_auth::Actor::local_owner("fixture"),
        a["id"].as_str().unwrap(),
        &json!({"requestId":"close-execute"}),
    )
    .unwrap();
    let (_, operations) = scheduled.unwrap();
    assert_eq!(operations.len(), 1);
    let op = &operations[0];
    assert_eq!(
        op["approvedOperatorCloseDecisionSha256"],
        p["operatorCloseDecision"]["decisionSha256"]
    );
    assert_eq!(op["action"]["action"], "close");
    assert!(op["action"].get("reply").is_none());
    assert_eq!(result["approvalId"], a["id"]);
    assert_eq!(d["posts"], old["posts"]);
    assert!(dispatch_diagnostics::local_check(&d, op).is_ok());
    let mut changed = d.clone();
    changed["proposals"][0]["operatorCloseDecision"]["reason"] = json!("altered");
    let source = &mut changed["proposals"][0]["operatorCloseDecision"];
    source["decisionSha256"] = json!(digest(source));
    assert!(dispatch_diagnostics::local_check(&changed, op).is_err());
}

#[test]
fn source_proof_cannot_change_between_owner_approval_and_execute() {
    let mut d = ready_fixture();
    let input = body(&d);
    let p = create_proposal(&mut d, &input).unwrap();
    let a = approve(&mut d, &p);
    d["proposals"][0]["operatorCloseDecision"]["reason"] = json!("different decision");
    let source = &mut d["proposals"][0]["operatorCloseDecision"];
    source["decisionSha256"] = json!(digest(source));
    let before = d.clone();
    assert!(
        execute_admission::admit(
            &mut d,
            &operator_auth::Actor::local_owner("fixture"),
            a["id"].as_str().unwrap(),
            &json!({"requestId":"close-execute"})
        )
        .is_err()
    );
    assert_eq!(d, before);
}

#[test]
fn distinct_close_preserves_unknown_reply_receipts_through_native_execute_and_dispatch_checks() {
    let mut d = ready_fixture();
    let fixture_owner = crate::runtime_lifecycle_startup::Admission::fixture(crate::accounts::Profile::LikeAvto);
    crate::runtime_lifecycle_startup::initialize_fixture(&mut d, fixture_owner.identity()).unwrap();
    crate::connection_gate::fixture_open(&mut d).unwrap();
    let item = d["items"][0].clone();
    let old = json!({"id":"original-reply","itemId":item["id"],"status":"unknown",
        "approvalId":"original-approval","proposalId":"original-proposal",
        "action":{"action":"reply_and_close","actionId":"original-reply","objectId":item["objectId"],
            "itemId":item["itemId"],"conversationKey":item["conversationKey"],"reply":"Original exact unknown reply"},
        "target":item,"executeReceipt":{"mutationOutcome":"uncertain"},"evidence":{"verificationPhase":"unconfirmed"}});
    d["operations"] = json!([old]);
    let mut input = body(&d);
    input["closePreserveUnknownReplies"] = json!(["original-reply"]);
    let p = create_proposal(&mut d, &input).unwrap();
    let original = d["operations"][0].clone();
    assert!(recipient_operation_blocks(&original, &p, &d["items"][0]));
    assert!(!recipient_operation_blocks_current(
        &d,
        &original,
        &p,
        &d["items"][0]
    ));
    let a = approve(&mut d, &p);
    assert_eq!(a["accepted"], refs(&p));
    let (_, scheduled) = execute_admission::admit(
        &mut d,
        &operator_auth::Actor::local_owner("fixture"),
        a["id"].as_str().unwrap(),
        &json!({"requestId":"distinct-close-execute"}),
    )
    .unwrap();
    let (_, operations) = scheduled.unwrap();
    assert_eq!(operations.len(), 1);
    let op = &operations[0];
    assert!(dispatch_diagnostics::local_check(&d, op).is_ok());
    assert_eq!(d["operations"][0], original);
    assert_eq!(d["operations"][0]["status"], "unknown");
    assert_ne!(op["id"], original["id"]);
    assert_ne!(op["approvalId"], original["approvalId"]);
    assert_eq!(op["action"]["action"], "close");
    assert!(op["action"].get("reply").is_none());
    assert!(
        execute_admission::admit(
            &mut d,
            &operator_auth::Actor::local_owner("fixture"),
            a["id"].as_str().unwrap(),
            &json!({"requestId":"different-second-execute"})
        )
        .is_err()
    );
}

#[test]
fn older_paid_draft_origin_is_preserved_as_history_not_new_close_generation_authority() {
    let mut d = ready_fixture();
    let historical = json!({"id":"old-paid-proposal","revision":1,"prepareRunId":"old-paid-run",
        "prepareBundleId":"old-bundle","prepareBundleDigest":"old-paid-digest","sourceContextDigest":"historical-context"});
    d["items"][0]["draftOrigin"] = historical.clone();
    let input = body(&d);
    let before = d.clone();
    let p = create_proposal(&mut d, &input).unwrap();
    assert_eq!(p["priorPreparationOrigin"], historical);
    assert!(p.get("origin").is_none());
    assert!(p.get("prepareRunId").is_none());
    assert_eq!(d["items"][0]["draftOrigin"], historical);
    assert_eq!(d["jobs"], before["jobs"]);
    assert_eq!(
        d["feedback"].as_array().unwrap().last().unwrap()["origin"],
        historical
    );
    assert!(proposal_current(&d, &p).is_ok());
    assert_eq!(approve(&mut d, &p)["accepted"], refs(&p));
    let mut altered = p.clone();
    altered["priorPreparationOrigin"]["prepareRunId"] = json!("different");
    assert!(proposal_current(&d, &altered).is_err());
}

#[test]
fn owner_close_explicit_media_contract_admits_only_current_semantic_independence() {
    let mut d=fixture();d["items"][0]["targetId"]=json!("message");
    d["branches"][0]["messages"]=json!([{"id":"message","text":"A self-contained text-only comment"}]);
    d["items"][0]["draftOrigin"]=json!({"id":"historical-paid","revision":1,"prepareRunId":"old-reserved","sourceContextDigest":"old"});
    let target=d["items"][0].clone();
    let request=json!({"account":d["account"],"connectorBinding":d["connectorBinding"],"items":[target]});
    d["jobs"]=json!([{"id":"old-reserved","kind":"assistant","purpose":"engine_prepare","status":"running",
      "selectedItemIds":["i0"],"prepareBundle":{"id":"old-bundle","version":1,"itemIds":["i0"],"request":request,
        "digest":editorial_review::hash_text(&request.to_string())},"preparationStages":{"first":null,"review":null,"groupAdmission":[]}}]);
    let reservation=preparation_reservations::capture(&d,"old-reserved").unwrap();
    d["jobs"][0]["scopeReservation"]=reservation;d["jobs"][0]["status"]=json!("completed");
    d["jobs"][0]["scopeModelAttempt"]=json!({"status":"unknown"});
    d["proposals"]=json!([{"id":"historical-paid","itemId":"i0","prepareRunId":"old-reserved","status":"draft","routeTarget":target}]);
    // The original exact paid-owner proposal has its own saved membership
    // exemption. An unrelated manual choice and new generation have none.
    assert!(preparation_reservations::assert_proposal(&d,&d["proposals"][0]).is_ok(),"exact original paid-owner membership remains valid");
    let ordinary=json!({"id":"unowned-manual","itemId":"i0","kind":"close","text":"","routeTarget":target});
    assert!(preparation_reservations::assert_proposal(&d,&ordinary).is_err(),"ownerless ordinary choice remains held");
    assert!(preparation_reservations::assert_available(&d,&["i0".to_owned()],None).is_err(),"new generation remains held by UNKNOWN paid work");
    let old=d.clone();let mut input=body(&d);input["decisionMediaContract"]=json!(decision_media::CONTRACT);
    for blocker in ["queued","running","social-unknown"] {
        let mut held=d.clone();
        if blocker=="social-unknown" {
            held["operations"]=json!([{"id":"held-social-unknown","itemId":"i0","status":"unknown","target":target,
              "action":{"actionId":"held-social-unknown","action":"reply_and_close","itemId":target["itemId"],"objectId":target["objectId"],
                "conversationKey":target["conversationKey"],"reply":"Exact unresolved earlier reply"}}]);
        }else{held["jobs"][0]["status"]=json!(blocker);}
        let before=held.clone();assert!(create_proposal(&mut held,&input).is_err(),"{blocker}: semantic opt-in cannot bypass live/UNKNOWN work");
        assert_eq!(held,before,"{blocker}: no workflow, paid history or operation writes");
    }
    let p=create_proposal(&mut d,&input).unwrap();assert!(decision_media::enabled(&p));
    assert_eq!(p["priorPreparationOrigin"],old["items"][0]["draftOrigin"]);
    let mut review=operator_editorial::tests::body(&d,&refs(&p));
    review["operatorReview"]["entries"][0]["mediaDependency"]=json!({"audio":"independent","visual":"independent"});
    for change in ["missing","required","unknown"] {
        let mut request=review.clone();
        if change=="missing" {request["operatorReview"]["entries"][0].as_object_mut().unwrap().remove("mediaDependency");}
        else {request["operatorReview"]["entries"][0]["mediaDependency"]["audio"]=json!(change);}
        let mut next=d.clone();assert!(operator_editorial::admit(&mut next,&operator_editorial::tests::actor(),&request).is_err(),"{change}");assert_eq!(next,d);
    }
    operator_editorial::admit(&mut d,&operator_editorial::tests::actor(),&review).unwrap();
    let reviewed=row(&d,"proposals",p["id"].as_str().unwrap()).unwrap().clone();
    assert!(proposal_current(&d,&reviewed).is_ok());assert_eq!(approve(&mut d,&reviewed)["accepted"],refs(&p));
    assert_eq!(d["posts"],old["posts"]);assert_eq!(d["materials"],old["materials"]);assert_eq!(d["operations"],old["operations"]);
    assert_eq!(d["jobs"][0],old["jobs"][0]);assert_eq!(d["proposals"][0],old["proposals"][0]);
    assert!(preparation_reservations::assert_proposal(&d,&ordinary).is_err(),"new owner review never releases old paid ownership");
    assert!(preparation_reservations::assert_available(&d,&["i0".to_owned()],None).is_err(),"accepted close never retries UNKNOWN model work");
    d["posts"][0]["text"]=json!("Changed after exact review");assert!(proposal_current(&d,&reviewed).is_err());
}
