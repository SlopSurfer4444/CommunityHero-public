use super::*;

pub(crate) fn actor() -> Actor { Actor::local_owner("offline-retained-recovery-fixture") }
pub(crate) fn fixture() -> (Value, InstalledCapture) {
    // Real native route/account/catalog/evidence/reservation fixture. The paid
    // failure has a valid native reservation and intentionally no first/result.
    let (mut d, _) = crate::operator_editorial::tests::fixture();
    d["proposals"] = json!([]); d["jobs"] = json!([]); d["audit"] = json!([]);
    d["preparationResearch"] = json!([]);
    let mut held = d["items"][0].clone(); held["id"] = json!("held0"); held["itemId"] = json!("held-external");
    held["conversationKey"] = json!("held-conversation"); held["branchId"] = json!("held-branch");
    list_mut(&mut d, "items").push(held);
    list_mut(&mut d, "branches").push(json!({"id":"held-branch","postId":"post","messages":[],"contextComplete":true}));
    let mut request = prepare_bundle::EvidenceContext::new(&d).evidence_for_item("i0").unwrap();
    request["items"] = d["items"].clone();
    let bundle = json!({"version":1,"id":"offline-retained-bundle","itemIds":["i0","held0"],"request":request,"digest":digest(&request)});
    let groups = prepare_bundle::capture_groups(&d, &bundle).unwrap();
    d["jobs"] = json!([{"id":"offline-retained-paid","kind":"assistant","purpose":"engine_prepare","status":"failed",
        "error":"Adapter failed (ASSISTANT_INVALID_RESPONSE)","prepareBundle":bundle,"selectedItemIds":["i0","held0"],
        "preparationStages":{"first":null,"review":null,"groupAdmission":groups}}]);
    d["jobs"][0]["scopeReservation"] = preparation_reservations::capture(&d,"offline-retained-paid").unwrap();
    // This capture deliberately retains malformed-output provenance; neither
    // its original editorial accept nor its raw bytes grant editorial authority.
    let raw = b"{\"retainedSuggestion\":\"Terse exact offline reply\"}\n";
    let packet = json!({"contract":CAPTURE_CONTRACT,"originalJob":d["jobs"][0],
        "historicalRuntimeSourceVerified":false,"historicalInstructionSourceVerified":false,"admitted":false,
        "evidence":{"responseSha256":hash(raw),"localTurnCompleted":true},"plan":{
        "originalJobId":"offline-retained-paid","bundleId":"offline-retained-bundle","bundleDigest":bundle["digest"],
        "originalMembership":["i0","held0"],"candidates":[{"proposal":{"itemId":"i0","kind":"reply_and_close","text":"Тоже интересный вариант 🙂"},
            "originalEditorialUntrusted":{"decision":"accept"},"evidence":[],"admitted":false,"nativeReceipt":false}],
        "held":[{"itemId":"held0","reason":"original_hold"}],"admitted":false,"nativeReceipt":false}});
    let bytes = packet.to_string().into_bytes();
    let installed = InstalledCapture::from_private_bytes(&bytes, &hash(&bytes), raw).unwrap();
    (d, installed)
}
pub(crate) fn body(d: &Value, installed: &InstalledCapture) -> Value {
    let preview = plan(d, &actor(), installed, &["i0".into()]).unwrap();
    assert!(preview["held"].as_array().unwrap().is_empty(), "fixture must exercise draft recovery rather than HOLD");
    json!({"requestId":"offline-retained-commit","planDigest":preview["planDigest"],"itemIds":["i0"],
        "checks":{"recipientIntent":"pass","companyRules":"pass","branchHistory":"pass","factualSupport":"pass","mediaDependency":"pass","exactText":"pass"},
        "reason":"Owner delegate checked exact retained text against the fresh branch, rules and applicable evidence; this is new draft admission only."})
}
pub(crate) fn recovered_fixture() -> (Value, Actor, InstalledCapture, String) {
    let (mut d, installed) = fixture(); let request = body(&d, &installed);
    let result = commit(&mut d, &actor(), &installed, &request).unwrap();
    let proposal_id = result["proposals"][0]["id"].as_str().unwrap().to_owned();
    (d, actor(), installed, proposal_id)
}

#[test]
fn installed_raw_and_packet_bytes_are_immutable_and_hash_bound() {
    let (_, installed) = fixture(); let packet = installed.packet_bytes.to_vec(); let raw = installed.raw_response_bytes.to_vec();
    assert!(InstalledCapture::from_private_bytes(&packet, installed.sha256(), &raw).is_ok());
    let mut changed_raw = raw.clone(); changed_raw.push(b' ');
    assert!(InstalledCapture::from_private_bytes(&packet, installed.sha256(), &changed_raw).is_err());
    let mut changed_packet = packet.clone(); changed_packet.push(b' ');
    assert!(InstalledCapture::from_private_bytes(&changed_packet, installed.sha256(), &raw).is_err());
}

#[test]
fn draft_recovery_preserves_failed_job_reservation_first_result_and_scope_fence() {
    let (mut d, installed) = fixture(); let before = d.clone(); let request = body(&d, &installed);
    let result = commit(&mut d, &actor(), &installed, &request).unwrap();
    assert_eq!(result["externalActions"],0); assert_eq!(d["jobs"],before["jobs"]);
    assert!(d["jobs"][0]["result"].is_null()); assert!(d["jobs"][0]["preparationStages"]["first"].is_null());
    assert_eq!(d["approvals"],before["approvals"]); assert_eq!(d["operations"],before["operations"]);
    let p = &d["proposals"][0]; assert_eq!(p["status"],"draft"); assert_eq!(p["revision"],1);
    assert_eq!(p["text"],installed.candidate("i0").unwrap()["proposal"]["text"]);
    for key in ["prepareRunId","prepareBundleId","prepareBundleDigest","generationMetadata","paidGeneration","recovery","origin","editorialReview","operatorCloseDecision"] {
        assert!(p.get(key).is_none_or(Value::is_null), "no synthetic generation or verdict: {key}");
    }
    assert_eq!(validate_proposal(&d,p,&installed,None).unwrap(),installed.job_id().unwrap());
    assert!(preparation_reservations::assert_available(&d,&["i0".into()],None).is_err(),"recovery must not unlock fresh spend");
    assert!(preparation_reservations::assert_available(&d,&["held0".into()],None).is_err(),"original hold stays reserved");
}

#[test]
fn original_holds_and_foreign_ids_cannot_become_retained_drafts() {
    let (d, installed) = fixture(); let before = d.clone();
    let preview = plan(&d,&actor(),&installed,&["held0".into()]).unwrap();
    assert!(preview["candidates"].as_array().unwrap().is_empty()); assert_eq!(preview["held"][0]["itemId"],"held0");
    assert!(plan(&d,&actor(),&installed,&["foreign".into()]).is_err()); assert_eq!(d,before);
    let mut next = d.clone(); let mut request = body(&d,&installed); request["itemIds"] = json!(["held0"]); request["planDigest"] = preview["planDigest"].clone();
    assert!(commit(&mut next,&actor(),&installed,&request).is_err()); assert_eq!(next,d);
}

#[test]
fn late_context_change_is_hold_and_atomic_commit_failure() {
    let (d, installed) = fixture(); let request = body(&d,&installed);
    for change in ["post","branch","item_text","item_revision","recipient","company","waiting"] {
        let mut next = d.clone(); match change {
            "post" => next["posts"][0]["text"] = json!("Changed post"),
            "branch" => next["branches"][0]["messages"] = json!([{"id":"new","text":"New unanswered complaint","role":"customer"}]),
            "item_text" => next["items"][0]["text"] = json!("Changed intent"),
            "item_revision" => next["items"][0]["revision"] = json!(99),
            "recipient" => next["items"][0]["itemId"] = json!("foreign-target"),
            "company" => next["connectorBinding"]["providerAccountId"] = json!("another-company"),
            _ => next["items"][0]["workflow"] = json!("waiting"),
        }
        let before = next.clone(); assert!(commit(&mut next,&actor(),&installed,&request).is_err(),"{change}"); assert_eq!(next,before,"{change}");
    }
}

#[test]
fn unknown_completed_effects_and_active_workers_never_retry_or_replace() {
    let (d, installed) = fixture(); let request = body(&d,&installed);
    for change in ["unknown","succeeded","execute","reconcile","editorial_review","assistant","newer_proposal"] {
        let mut next = d.clone(); match change {
            "unknown" | "succeeded" => next["operations"] = json!([{"id":"prior-effect","itemId":"i0","target":next["items"][0],"status":change}]),
            "newer_proposal" => next["proposals"] = json!([{"id":"newer","itemId":"i0","status":"draft"}]),
            kind => next["jobs"].as_array_mut().unwrap().push(json!({"id":"active-worker","kind":kind,"status":"running"})),
        }
        let before = next.clone(); let preview = plan(&next,&actor(),&installed,&["i0".into()]);
        assert!(preview.is_err() || !preview.unwrap()["held"].as_array().unwrap().is_empty(),"{change}");
        assert!(commit(&mut next,&actor(),&installed,&request).is_err(),"{change}"); assert_eq!(next,before,"{change}");
    }
}

#[test]
fn authenticated_local_owner_is_required_even_for_plan_and_replay() {
    let (mut d, installed) = fixture(); let request = body(&d,&installed);
    for remote in [false,true] {
        let mut other = actor(); if remote { other.id="remote-owner".into(); } else { other.role="operator".into(); }
        let before=d.clone(); assert!(plan(&d,&other,&installed,&["i0".into()]).is_err());
        assert!(commit(&mut d,&other,&installed,&request).is_err()); assert_eq!(d,before);
    }
}

#[test]
fn exact_plan_review_check_request_identity_and_no_extra_client_proof() {
    let (d, installed) = fixture(); let request = body(&d,&installed);
    for change in ["plan","checks","reason","request_id","caller_proof","duplicate_ids"] {
        let mut body = request.clone(); match change {
            "plan"=>body["planDigest"]=json!("f".repeat(64)),
            "checks"=>body["checks"]["factualSupport"]=json!("hold"),
            "reason"=>body["reason"]=json!("x".repeat(2001)),
            "request_id"=>body["requestId"]=json!("../caller"),
            "caller_proof"=>body[FIELD]=json!({"verified":true,"prepareRunId":"offline-retained-paid"}),
            _=>body["itemIds"]=json!(["i0","i0"]),
        }
        let mut next=d.clone(); assert!(commit(&mut next,&actor(),&installed,&body).is_err(),"{change}"); assert_eq!(next,d);
    }
}

#[test]
fn idempotent_commit_acknowledges_same_storage_without_new_proposal_or_work() {
    let (mut d, installed)=fixture(); let request=body(&d,&installed);
    let first=commit(&mut d,&actor(),&installed,&request).unwrap(); let before=d.clone();
    let replay=commit(&mut d,&actor(),&installed,&request).unwrap(); assert_eq!(replay["replayed"],true); assert_eq!(first["proposals"],replay["proposals"]); assert_eq!(d,before);
    let mut changed=request.clone(); changed["reason"]=json!("Changed semantic review");
    assert!(commit(&mut d,&actor(),&installed,&changed).is_err()); assert_eq!(d,before);
}

#[test]
fn exact_proof_rejects_text_edits_route_revision_forged_lineage_and_marker_swaps() {
    let (d,_,installed,key)=recovered_fixture(); let original=row(&d,"proposals",&key).unwrap();
    for change in ["text","revision","route","lineage","marker","checks","artifact"] {
        let mut next=d.clone(); let p=row_mut(&mut next,"proposals",&key).unwrap(); match change {
            "text"=>p["text"]=json!("Today's different manually written reply"),
            "revision"=>p["revision"]=json!(2),
            "route"=>p["routeTarget"]["itemId"]=json!("other-recipient"),
            "lineage"=>p["prepareRunId"]=json!("offline-retained-paid"),
            "marker"=>p[FIELD]["proofSha256"]=json!("f".repeat(64)),
            "checks"=>p[FIELD]["checks"]["exactText"]=json!("hold"),
            _=>p[FIELD]["captureSha256"]=json!("f".repeat(64)),
        }
        assert!(validate_proposal(&next,row(&next,"proposals",&key).unwrap(),&installed,None).is_err(),"{change}");
    }
    assert_eq!(original["text"],installed.candidate("i0").unwrap()["proposal"]["text"]);
}

#[test]
fn original_member_recovery_is_once_even_after_terminal_draft_status_or_receipt_only_history() {
    let (baseline, installed)=fixture(); let request=body(&baseline,&installed);
    let mut admitted=baseline.clone(); let result=commit(&mut admitted,&actor(),&installed,&request).unwrap();
    let key=result["proposals"][0]["id"].as_str().unwrap();
    assert_eq!(admitted["audit"].as_array().unwrap().last().unwrap()["itemIds"],json!(["i0"]));
    for change in ["stale","cancelled","failed","superseded","proof_only","receipt_only","legacy_receipt","unresolved_legacy_receipt"] {
        let mut next=admitted.clone();
        // Restore the exact original evidence to ensure this exercises the
        // permanent admission fence, not an unrelated stale-context hold.
        next["items"]=baseline["items"].clone();
        match change {
            "proof_only"=>next["audit"]=json!([]),
            "receipt_only"=>next["proposals"]=json!([]),
            "legacy_receipt"=>{
                row_mut(&mut next,"proposals",key).unwrap()["status"]=json!("stale");
                row_mut(&mut next,"proposals",key).unwrap().as_object_mut().unwrap().remove(FIELD);
                next["audit"].as_array_mut().unwrap().last_mut().unwrap().as_object_mut().unwrap().remove("itemIds");
            },
            "unresolved_legacy_receipt"=>{
                next["proposals"]=json!([]);
                next["audit"].as_array_mut().unwrap().last_mut().unwrap().as_object_mut().unwrap().remove("itemIds");
            },
            status=>row_mut(&mut next,"proposals",key).unwrap()["status"]=json!(status),
        }
        original_group_current(&next,&installed,"i0").unwrap();
        let fresh=plan(&next,&actor(),&installed,&["i0".into()]).unwrap();
        assert!(fresh["candidates"].as_array().unwrap().is_empty(),"{change}");
        assert_eq!(fresh["held"][0]["itemId"],"i0","{change}");
        let mut different=request.clone(); different["requestId"]=json!(format!("different-{change}"));
        different["planDigest"]=fresh["planDigest"].clone();
        let before=next.clone(); assert!(commit(&mut next,&actor(),&installed,&different).is_err(),"{change}");
        assert_eq!(next,before,"different request must not duplicate or mutate history: {change}");
        let replay=commit(&mut next,&actor(),&installed,&request);
        if change!="proof_only" {
            assert_eq!(replay.unwrap()["replayed"],true,"{change}");
            assert_eq!(next,before,"same receipt replay only acknowledges storage: {change}");
        } else { assert!(replay.is_err()); }
    }
}

#[test]
fn storage_hook_preserves_original_failure_and_native_proof_audit_history() {
    let (d,_,installed,key)=recovered_fixture();
    for change in ["job","reservation","first","proof_remove","proof_rewrite","audit_remove"] {
        let mut next=d.clone(); match change {
            "job"=>next["jobs"][0]["status"]=json!("completed"),
            "reservation"=>{next["jobs"][0].as_object_mut().unwrap().remove("scopeReservation");},
            "first"=>next["jobs"][0]["preparationStages"]["first"]=json!({"status":"completed"}),
            "proof_remove"=>{row_mut(&mut next,"proposals",&key).unwrap().as_object_mut().unwrap().remove(FIELD);},
            "proof_rewrite"=>row_mut(&mut next,"proposals",&key).unwrap()[FIELD]["reason"]=json!("Forged review"),
            _=>next["audit"]=json!([]),
        }
        assert!(validate_change(&d,&next,&installed).is_err(),"{change}");
    }
    // Honest operator edit retains source proof as history but fails recovery
    // exemption later; a separate reviewed-edit contract is deliberately absent.
    let mut edited=d.clone(); row_mut(&mut edited,"proposals",&key).unwrap()["text"]=json!("Edited operator text");
    row_mut(&mut edited,"proposals",&key).unwrap()["revision"]=json!(2);
    validate_change(&d,&edited,&installed).unwrap();
    assert!(validate_proposal(&edited,row(&edited,"proposals",&key).unwrap(),&installed,None).is_err());
}

#[test]
fn research_selection_time_alone_is_stable_but_claim_scope_and_expiry_are_bound() {
    let a=json!({"materials":[{"id":"material","text":"Scoped source","expiresAt":"2030-01-01T00:00:00Z"}],
        "manifest":[{"materialId":"material","materialHash":"exact","selectedAt":"2026-10-04T10:00:00Z","expiresAt":"2030-01-01T00:00:00Z"}]});
    let mut b=a.clone(); b["manifest"][0]["selectedAt"]=json!("2026-10-04T10:00:15Z");
    assert_eq!(stable_research(a.clone()),stable_research(b.clone()));
    b["materials"][0]["text"]=json!("Different claim scope"); assert_ne!(stable_research(a.clone()),stable_research(b.clone()));
    b=a.clone(); b["manifest"][0]["expiresAt"]=json!("2026-10-04T10:00:16Z"); assert_ne!(stable_research(a),stable_research(b));
}

fn add_current_research(d: &mut Value) {
    let at=(chrono::Utc::now()-chrono::Duration::minutes(1)).to_rfc3339();
    let mut archive=json!({"id":"offline-research","jobId":"offline-research-job","account":d["account"],
        "connectorBinding":d["connectorBinding"],"createdAt":at,"trust":"source_only","activePolicy":false,
        "posts":[d["posts"][0]],"bindings":[{"itemId":"i0","postKey":d["items"][0]["postKey"]}],
        "review":{"status":"completed","research":{"version":1,"status":"completed","trust":"source_only","webCalls":1,"completedAt":at,
            "sources":[{"itemId":"i0","url":"https://manufacturer.example/exact-version","title":"Scoped specification","claim":"Scoped factual support","trust":"source_only"}]}}});
    archive["checksum"]=json!(research_cache::checksum(&archive)); d["preparationResearch"]=json!([archive]);
    let evidence=reviewed_evidence(d,"i0").unwrap();
    assert_eq!(evidence["retainedCurrentResearch"]["manifest"].as_array().unwrap().len(),1,"fixture must exercise actual native nonempty research selection");
}

#[test]
fn actual_cached_research_removal_expiry_or_changed_claim_invalidates_recovered_proof() {
    let (mut d,installed)=fixture(); add_current_research(&mut d);
    let request=body(&d,&installed); let result=commit(&mut d,&actor(),&installed,&request).unwrap();
    let key=result["proposals"][0]["id"].as_str().unwrap(); let p=row(&d,"proposals",key).unwrap().clone();
    validate_proposal(&d,&p,&installed,None).unwrap();
    for change in ["removed","expired","changed_claim"] {
        let mut next=d.clone(); match change {
            "removed"=>next["preparationResearch"]=json!([]),
            "expired"=>{let at=(chrono::Utc::now()-chrono::Duration::days(2)).to_rfc3339();
                next["preparationResearch"][0]["createdAt"]=json!(at);next["preparationResearch"][0]["review"]["research"]["completedAt"]=json!(at);
                next["preparationResearch"][0]["checksum"]=json!(research_cache::checksum(&next["preparationResearch"][0]));},
            _=>{next["preparationResearch"][0]["review"]["research"]["sources"][0]["claim"]=json!("Different market/version support");
                next["preparationResearch"][0]["checksum"]=json!(research_cache::checksum(&next["preparationResearch"][0]));},
        }
        assert!(validate_proposal(&next,&p,&installed,None).is_err(),"{change}");
    }
}

#[test]
fn delayed_plan_commit_with_actual_nonempty_research_keeps_identical_semantics() {
    let (mut d,installed)=fixture(); add_current_research(&mut d); let request=body(&d,&installed);
    let first=research_cache::select(&d,d["items"].as_array().unwrap(),d["posts"].as_array().unwrap(),&now()).unwrap();
    std::thread::sleep(std::time::Duration::from_millis(1100));
    let later=research_cache::select(&d,d["items"].as_array().unwrap(),d["posts"].as_array().unwrap(),&now()).unwrap();
    assert_ne!(first["manifest"][0]["selectedAt"],later["manifest"][0]["selectedAt"],"the actual native timestamp must advance");
    assert_eq!(stable_research(first),stable_research(later));
    let result=commit(&mut d,&actor(),&installed,&request).unwrap(); assert_eq!(result["replayed"],false);
}

#[test]
fn storage_rejects_retained_proof_attached_to_an_existing_ordinary_proposal() {
    let (after,_,installed,key)=recovered_fixture(); let mut before=after.clone();
    row_mut(&mut before,"proposals",&key).unwrap().as_object_mut().unwrap().remove(FIELD);
    assert!(validate_change(&before,&after,&installed).is_err());
}

#[test]
fn native_capture_keeps_digest_guard_even_when_json_numeric_lexemes_differ() {
    let (mut d,_) = fixture(); let request=&mut d["jobs"][0]["prepareBundle"]["request"];
    request["numericSource"] = json!(1.0);
    // This explicitly changed witness is only a mechanism fixture, not a claim
    // that production packet discrepancy was proven to be numeric serialization.
    let native = digest(request); let lexical = request.to_string().replace("\"numericSource\":1.0", "\"numericSource\":1e0");
    assert_ne!(native,hash(lexical.as_bytes()));
    d["jobs"][0]["prepareBundle"]["digest"] = json!(hash(lexical.as_bytes()));
    assert!(preparation_reservations::capture(&d,"offline-retained-paid").is_err(),"real native guard must not be bypassed by retained recovery");
}

#[test]
#[ignore="root only: requires explicitly supplied immutable retained packet path; offline native diagnostic"]
fn production_retained_packet_native_capture_digest_diagnostic() {
    let path=std::env::var_os("COMMUNITYHERO_RETAINED_PAID_DIAGNOSTIC_PACKET").expect("Root must supply exact retained packet path");
    let bytes=std::fs::read(path).unwrap(); let packet:Value=serde_json::from_slice(&bytes).unwrap(); let job=packet["originalJob"].clone();
    let native_request_digest=digest(&job["prepareBundle"]["request"]);
    let persisted_request_digest=job["prepareBundle"]["digest"].clone();
    let mut d=crate::empty(); d["account"]=job["prepareBundle"]["request"]["account"].clone();
    d["connectorBinding"]=job["scopeReservation"]["connectorBinding"].clone();
    d["items"]=job["prepareBundle"]["request"]["items"].clone(); d["jobs"]=json!([job]);
    let checked=preparation_reservations::capture(&d,job["id"].as_str().unwrap());
    println!("{}",json!({"packetSha256":hash(&bytes),"jobId":job["id"],"nativeRequestDigest":native_request_digest,
        "persistedRequestDigest":persisted_request_digest,"nativeCaptureAccepted":checked.is_ok(),
        "nativeCaptureError":checked.err().map(|e|e.1),"numericLexemeRootCauseProven":false}));
    // Diagnoses actual serialized value without bypassing mismatch or pretending
    // that lexical reconstruction established the historical native runtime.
    assert_eq!(native_request_digest,persisted_request_digest.as_str().unwrap_or(""));
}

#[test]
fn exact_own_dispatch_operation_exemption_never_exempts_a_sibling_unknown() {
    let (mut d,_,installed,key)=recovered_fixture(); let p=row(&d,"proposals",&key).unwrap().clone(); let target=d["items"][0].clone();
    let authority=dispatch_authority::approval_binding(&actor()); let action=action_for(&p,&target,"dispatch0").unwrap();
    d["approvals"]=json!([{"id":"approval0","status":"consumed","proposals":[{"id":key,"revision":1,"proposal":p,"item":target}],"approvalAuthority":authority}]);
    let op=json!({"id":"dispatch0","proposalId":key,"itemId":"i0","approvalId":"approval0","status":"dispatching","target":target,
        "action":action,"approvedRetainedPaidRecoverySha256":p[FIELD]["proofSha256"],"dispatchAuthority":{"approved":authority,"executed":authority}});
    d["operations"]=json!([op]);
    assert!(validate_proposal(&d,&p,&installed,None).is_err(),"ordinary check cannot omit an in-flight effect");
    validate_proposal(&d,&p,&installed,Some(&op)).unwrap();
    let mut forged=op.clone(); forged["approvedRetainedPaidRecoverySha256"]=json!("f".repeat(64));
    assert!(validate_proposal(&d,&p,&installed,Some(&forged)).is_err());
    list_mut(&mut d,"operations").push(json!({"id":"other-effect","itemId":"i0","target":target,"status":"unknown"}));
    assert!(validate_proposal(&d,&p,&installed,Some(&op)).is_err(),"own-operation never authorizes UNKNOWN retry");
}
