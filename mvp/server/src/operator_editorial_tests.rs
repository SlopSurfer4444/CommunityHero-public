use super::*;
const AT:&str="2026-09-29T10:00:00Z";

pub(crate) fn actor()->operator_auth::Actor {operator_auth::Actor::local_owner("offline-owner-fixture")}
pub(crate) fn fixture()->(Value,Value){
    let mut d=crate::empty();crate::accounts::initialize(&mut d,crate::accounts::Profile::LikeAvto).unwrap();
    d["feedback"]=json!([]);d["knowledge_entries"]=json!([]);d["knowledge_versions"]=json!([]);
    d["items"]=json!([{"id":"i0","itemId":"c0","objectId":"11341","platform":"VK","postKey":"post",
        "conversationKey":"thread0","branchId":"branch","postId":"post","revision":1,"draft":"","workflow":"attention",
        "providerStatus":"new","contextEvidenceDigest":"context-0","branchContextDigest":"branch-digest",
        "text":"Мечтаю о V8","connectorBinding":d["connectorBinding"]}]);
    d["posts"]=json!([{"id":"post","postKey":"post","objectId":"11341","platform":"VK","text":"Post","attachments":[]}]);
    d["branches"]=json!([{"id":"branch","postId":"post","messages":[],"contextComplete":true}]);
    let p=create_proposal(&mut d,&json!({"itemId":"i0","expectedRevision":1,"kind":"reply_and_close","text":"Тоже интересный вариант 🙂"})).unwrap();
    let refs=json!([{"id":p["id"],"revision":p["revision"]}]);(d,refs)
}
pub(crate) fn body(d:&Value,refs:&Value)->Value {
    let preview=capture(d,&actor(),&json!({"proposals":refs})).unwrap();
    json!({"requestId":"offline-operator-review","proposals":refs,"previewDigest":preview["previewDigest"],
        "operatorReview":{"version":1,"method":METHOD,"entries":preview["entries"].as_array().unwrap().iter().map(|e|
            json!({"candidate":e["candidate"],"checks":{"companyRules":"pass","intent":"pass","factualScope":"pass"},
                "reason":"Delegate reviewed exact reply against supplied current rules and source evidence"})).collect::<Vec<_>>()}})
}

async fn assert_transaction_owned_review_rollback_and_replay(db:Database) {
    let (mut baseline,first)=fixture();
    let mut other=baseline["items"][0].clone();
    other["id"]=json!("i1");other["itemId"]=json!("c1");
    other["conversationKey"]=json!("thread1");other["revision"]=json!(1);other["workflow"]=json!("attention");
    list_mut(&mut baseline,"items").push(other);
    let second=create_proposal(&mut baseline,&json!({"itemId":"i1","expectedRevision":1,"kind":"reply_and_close","text":"Another exact reply"})).unwrap();
    let refs=json!([first[0],{"id":second["id"],"revision":second["revision"]}]);
    let request=body(&baseline,&refs);
    let mut late_error=request.clone();
    late_error["operatorReview"]["entries"][1]["checks"]["factualScope"]=json!("uncertain");
    let mut uncommitted=baseline.clone();
    assert!(admit_inner(&mut uncommitted,&actor(),&late_error).is_err());
    assert_ne!(uncommitted["proposals"],baseline["proposals"],"fixture must fail after the first receipt was written");
    let mut pure=baseline.clone();
    assert!(admit(&mut pure,&actor(),&late_error).is_err());
    assert_eq!(pure,baseline,"pure reducer still owns its rollback");
    db.change(|d|{*d=baseline;Ok(())}).await.unwrap();
    let before=db.read().await.unwrap();
    assert!(db.change_admission_observed(storage::AdmissionScope::OperatorEditorial(&late_error),
        |d|admit_inner(d,&actor(),&late_error)).await.is_err());
    assert_eq!(db.read().await.unwrap(),before,"transaction rollback retains no partial receipt or history");
    let failed:ApiResult<(Value,bool)>=db.change_admission_observed(storage::AdmissionScope::OperatorEditorial(&request),|d| {
        admit_inner(d,&actor(),&request)?;
        Err(internal("synthetic failure after review admission"))
    }).await;
    assert!(failed.is_err());assert_eq!(db.read().await.unwrap(),before,"late failure rolls back job and durable receipt too");
    let (result,events)=performance::capture(db.change_admission_observed(storage::AdmissionScope::OperatorEditorial(&request),
        |d|admit_inner(d,&actor(),&request))).await;
    let (result,changed)=result.unwrap();assert!(changed);
    assert!(!events.iter().any(|event|event["stage"]=="operator.editorial.atomic_clone"));
    let saved=db.read().await.unwrap();
    assert_eq!(saved["items"],before["items"]);assert_eq!(saved["operations"],before["operations"]);
    for proposal in list(&saved,"proposals") {assert!(proposal_current(&saved,proposal).is_ok());}
    let (replay,changed)=db.change_admission_observed(storage::AdmissionScope::OperatorEditorial(&request),
        |d|admit_inner(d,&actor(),&request)).await.unwrap();
    assert!(!changed);assert_eq!(replay["jobId"],result["jobId"]);assert_eq!(replay["replayed"],true);
    assert_eq!(db.read().await.unwrap(),saved);db.close().await;
}

#[tokio::test]
async fn transaction_owned_review_rolls_back_partial_receipt_and_replays_without_recloning() {
    let folder=tempfile::tempdir().unwrap();
    let db=Database::Sqlite(crate::open_db(&folder.path().join("review.sqlite")).await.unwrap());
    assert_transaction_owned_review_rollback_and_replay(db).await;
}

#[tokio::test]
#[ignore="requires a pristine explicitly isolated communityhero_writer_v51_test_ PostgreSQL fixture"]
async fn postgres_transaction_owned_review_rollback_and_replay() {
    assert_transaction_owned_review_rollback_and_replay(storage::writer_v51_fixture_db().await).await;
}

#[test]
fn explicit_assisted_review_is_truthful_durable_and_accepted_by_unchanged_native_approval(){
    let (mut d,refs)=fixture();let before=d.clone();let request=body(&d,&refs);assert_eq!(d,before,"preview is read-only");
    assert!(create_approval(&mut d,&actor(),&json!({"proposals":refs})).is_err(),"ordinary unreviewed manual draft remains gated");
    let result=admit(&mut d,&actor(),&request).unwrap();
    let p=&d["proposals"][0];assert_eq!(p["text"],before["proposals"][0]["text"]);assert_eq!(p["revision"],1);assert_eq!(p["status"],"draft");
    assert_eq!(p["editorialReview"]["source"]["kind"],"operator_assisted_review");
    assert_eq!(p["editorialReview"]["source"]["method"],METHOD);
    assert_eq!(p["editorialReview"]["source"]["reviewedBy"],actor().public_json());
    assert!(p["editorialReview"]["source"].get("runMetadata").is_none(),"no fake model provenance");
    assert!(editorial_review::require_current(&prepare_bundle::EvidenceContext::new(&d),p).is_ok());
    assert_eq!(d["jobs"][0]["status"],"completed");assert_eq!(d["jobs"][0]["id"],result["jobId"]);
    assert_eq!(d["jobs"][0]["purpose"],"operator_assisted_review");
    assert_eq!(d["jobs"][0]["operatorReviewPreview"]["entries"][0]["evidence"]["items"][0]["text"],"Мечтаю о V8");
    assert!(list(&d,"operations").is_empty());assert!(list(&d,"approvals").is_empty());
    assert!(list(&d,"audit").iter().any(|a|a["action"]=="operator_editorial.reviewed"));
    let approval=create_approval(&mut d,&actor(),&json!({"proposals":refs})).unwrap();
    assert_eq!(approval["editorialPolicyVersion"],1);assert_eq!(approval["proposals"][0]["proposal"]["editorialReview"],d["proposals"][0]["editorialReview"]);
}

#[test]
fn exact_preview_detects_text_revision_source_rules_tenant_and_route_changes_atomically(){
    let (d,refs)=fixture();let request=body(&d,&refs);
    for change in ["text","revision","item_revision","post","branch","rules","tenant","binding","route","hold"] {
        let mut changed=d.clone();
        match change {
            "text"=>changed["proposals"][0]["text"]=json!("Changed reply"),
            "revision"=>changed["proposals"][0]["revision"]=json!(2),
            "item_revision"=>changed["items"][0]["revision"]=json!(3),
            "post"=>changed["posts"][0]["text"]=json!("New source context"),
            "branch"=>changed["branches"][0]["messages"]=json!([{"id":"later","text":"New clarification"}]),
            "rules"=>{knowledge::save_instruction(&mut changed,&json!({"requestId":"new-rule","title":"Voice","text":"Use current company voice"}),AT).unwrap();},
            "tenant"=>{changed["account"]=json!("BAW Russia");changed["connectorBinding"]=accounts::Profile::BawRussia.binding();},
            "binding"=>changed["connectorBinding"]["providerAccountId"]=json!("different-account"),
            "route"=>changed["proposals"][0]["routeTarget"]["connectorBinding"]=json!({}),
            _=>changed["items"][0]["workflow"]=json!("waiting"),
        }
        let original=changed.clone();assert!(admit(&mut changed,&actor(),&request).is_err(),"{change}");assert_eq!(changed,original,"{change}: no partial receipt/ledger writes");
    }
}

#[test]
fn old_source_can_be_explicitly_reviewed_again_without_rewriting_generation_provenance(){
    for change in ["post","rules"] {
        let (mut d,refs)=fixture();let historical=d["proposals"][0]["reviewContextDigest"].clone();
        if change=="post" {d["posts"][0]["text"]=json!("Updated post before explicit current review");}
        else {knowledge::save_instruction(&mut d,&json!({"requestId":"current-rule","title":"Voice","text":"Use current company voice"}),AT).unwrap();}
        assert!(proposal_current(&d,&d["proposals"][0]).is_err());
        let request=body(&d,&refs);admit(&mut d,&actor(),&request).unwrap();
        assert_eq!(d["proposals"][0]["reviewContextDigest"],historical);assert!(proposal_current(&d,&d["proposals"][0]).is_ok());
        d["posts"][0]["text"]=json!("Later source change");
        assert!(editorial_review::require_current(&prepare_bundle::EvidenceContext::new(&d),&d["proposals"][0]).is_err());
        assert!(create_approval(&mut d,&actor(),&json!({"proposals":refs})).is_err());
    }
}

#[test]
fn pending_paid_review_and_unknown_operation_never_get_taken_over(){
    let (d,refs)=fixture();let request=body(&d,&refs);
    for state in ["running","queued","unknown"] {
        let mut changed=d.clone();
        if state=="unknown" {changed["operations"]=json!([{"id":"old-unknown","itemId":"i0","proposalId":refs[0]["id"],"target":changed["items"][0],"status":"unknown"}]);}
        else {changed["jobs"]=json!([{"id":"paid-review","kind":"editorial_review","status":state,"editorialReferences":refs}]);}
        let before=changed.clone();assert!(admit(&mut changed,&actor(),&request).is_err(),"{state}");assert_eq!(changed,before);
    }
    let mut changed=d.clone();changed["jobs"]=json!([{"id":"old-revision-paid","kind":"editorial_review","status":"running",
        "editorialReferences":[{"id":refs[0]["id"],"revision":999}]}]);
    assert!(capture(&changed,&actor(),&json!({"proposals":refs})).is_err(),"another revision does not imply a settled paid outcome");
    for terminal in ["failed","completed"] {
        let mut ended=d.clone();ended["jobs"]=json!([{"id":"terminal-paid","kind":"editorial_review","status":terminal,
            "editorialReferences":refs,"editorialBatches":[{"state":"captured","capture":{"batch":{"id":"paid"}}}]}]);
        let historical=ended["jobs"][0].clone();let next=body(&ended,&refs);admit(&mut ended,&actor(),&next).unwrap();
        assert_eq!(ended["jobs"][0],historical,"explicit new semantic review never retries or edits terminal old journal");
    }
}

#[test]
fn replay_recovers_original_receipt_without_reviewing_changed_source_or_new_job(){
    let (mut d,refs)=fixture();let request=body(&d,&refs);let result=admit(&mut d,&actor(),&request).unwrap();
    d["posts"][0]["text"]=json!("Changed after committed receipt");let before=d.clone();
    let replay=admit(&mut d,&actor(),&request).unwrap();assert_eq!(replay["jobId"],result["jobId"]);assert_eq!(replay["replayed"],true);assert_eq!(d,before);
    let mut different=request.clone();different["operatorReview"]["entries"][0]["reason"]=json!("Changed request body");
    assert!(admit(&mut d,&actor(),&different).is_err());assert_eq!(d,before);
}

#[test]
fn explicit_method_owner_and_complete_semantic_checks_are_required(){
    let (d,refs)=fixture();let request=body(&d,&refs);
    for change in ["method","uncertain","missing_check","missing_entry","different_candidate","blank_reason","extra_field","digest"] {
        let mut body=request.clone();
        match change {
            "method"=>body["operatorReview"]["method"]=json!("human_personally_read"),
            "uncertain"=>body["operatorReview"]["entries"][0]["checks"]["factualScope"]=json!("uncertain"),
            "missing_check"=>{body["operatorReview"]["entries"][0]["checks"].as_object_mut().unwrap().remove("companyRules");},
            "missing_entry"=>body["operatorReview"]["entries"]=json!([]),
            "different_candidate"=>body["operatorReview"]["entries"][0]["candidate"]["textSha256"]=json!("different"),
            "blank_reason"=>body["operatorReview"]["entries"][0]["reason"]=json!(" "),
            "extra_field"=>body["skipEditorial"]=json!(true),
            _=>body["previewDigest"]=json!("forged"),
        }
        let mut next=d.clone();assert!(admit(&mut next,&actor(),&body).is_err(),"{change}");assert_eq!(next,d);
    }
    let mut other=actor();other.role="operator".into();let mut next=d.clone();assert!(admit(&mut next,&other,&request).is_err());assert_eq!(next,d);
}

#[test]
fn duplicate_recipients_and_nonreply_actions_remain_outside_this_explicit_route(){
    let (mut d,refs)=fixture();assert!(capture(&d,&actor(),&json!({"proposals":[refs[0],refs[0]]})).is_err());
    let expected=d["items"][0]["revision"].clone();
    let p=create_proposal(&mut d,&json!({"itemId":"i0","expectedRevision":expected,"kind":"reply_and_close","text":"Another exact text"})).unwrap();
    assert!(capture(&d,&actor(),&json!({"proposals":[refs[0],{"id":p["id"],"revision":p["revision"]}]})).is_err());
    d["proposals"][0]["kind"]=json!("close");d["proposals"][0]["text"]=json!("");assert!(capture(&d,&actor(),&json!({"proposals":refs})).is_err());
}

#[test]
fn alias_unknown_video_and_current_url_policy_cannot_be_reviewed_into_permission(){
    let (d,refs)=fixture();
    let mut alias=d.clone();alias["operations"]=json!([{"id":"alias-unknown","itemId":"other-local-row","status":"unknown","target":alias["items"][0]}]);
    assert!(capture(&alias,&actor(),&json!({"proposals":refs})).is_err(),"UNKNOWN on another canonical alias remains blocked");
    let mut video=d.clone();video["posts"][0]["sourceUrl"]=json!("https://www.youtube.com/watch?v=offline-fixture");
    assert!(capture(&video,&actor(),&json!({"proposals":refs})).is_err(),"operator judgments cannot replace mandatory video evidence");
    let mut url=d.clone();knowledge::reply_url_policy::save(&mut url,&json!({"requestId":"typed-no-links","expectedVersionId":null,"values":[]}),AT).unwrap();
    url["proposals"][0]["text"]=json!("Подробнее https://forbidden.example/");
    assert!(capture(&url,&actor(),&json!({"proposals":refs})).is_err(),"current deterministic rule remains authoritative");
    // Display platform is not a capability switch: the selected connector's
    // verified reply capability is authoritative. Change the actual route.
    let mut retargeted=d.clone();retargeted["proposals"][0]["routeTarget"]["objectId"]=json!("different-provider-object");
    assert!(capture(&retargeted,&actor(),&json!({"proposals":refs})).is_err(),"actual connector target route remains authoritative");
}

#[test]
fn preview_identity_is_bound_to_authenticated_actor_and_credential_generation(){
    let (mut d,refs)=fixture();let request=body(&d,&refs);
    let mut other=actor();other.id="another-owner".into();other.authority_generation=Some("a".repeat(64));
    let before=d.clone();assert!(admit(&mut d,&other,&request).is_err());assert_eq!(d,before);
    let previous=capture(&d,&other,&json!({"proposals":refs})).unwrap();
    other.authority_generation=Some("b".repeat(64));let rotated=capture(&d,&other,&json!({"proposals":refs})).unwrap();
    assert_ne!(previous["previewDigest"],rotated["previewDigest"]);
    assert!(previous.get("reviewAuthority").is_none(),"credential-generation binding is never exposed as raw authority");
    assert_ne!(previous["reviewAuthorityDigest"],rotated["reviewAuthorityDigest"]);
}
