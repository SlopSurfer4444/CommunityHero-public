use super::*;

fn actor()->crate::operator_auth::Actor {crate::operator_auth::Actor::local_owner("batch-storage-test")}
fn fixture()->Value {
    let mut d=crate::empty();super::super::normalize(&mut d);
    d["items"]=json!([{"id":"i","itemId":"c","objectId":"o","platform":"VK","postKey":"p",
        "conversationKey":"thread","branchId":"b","postId":"post","revision":1,"draft":"",
        "workflow":"attention","providerStatus":"new"}]);
    for (id,external) in [("other","c-other"),("paid","c-paid")] {
        let mut item=d["items"][0].clone();item["id"]=json!(id);item["itemId"]=json!(external);
        crate::list_mut(&mut d,"items").push(item);
    }
    d["branches"]=json!([{"id":"b","postId":"post","messages":[{"id":"c","text":"Thank you"}],"contextComplete":true}]);
    d["posts"]=json!([{"id":"post","postKey":"p","text":"Post"}]);
    d["conversations"]=json!([{"id":"private","messages":[{"text":"private history".repeat(20_000)}]}]);
    d["approvals"]=json!([{"id":"approval-old","status":"approved","proposals":[],"authority":"keep"}]);
    d["audit"]=json!([{"id":"audit-old","action":"other","refId":"i","context":"historical receipt".repeat(20_000)}]);
    d["feedback"]=json!([{"id":"foreign-event","itemId":"paid","kind":"action_selected","context":"historical feedback".repeat(20_000)}]);
    d["operations"]=json!((0..37).map(|n|json!({"id":format!("unknown-{n}"),"status":"unknown","evidence":{"receipt":"retain exact"}})).collect::<Vec<_>>());
    d["proposals"]=json!([{"id":"paid-proposal","itemId":"paid","revision":1,"itemRevision":1,"status":"draft",
        "kind":"reply_and_close","text":"Exact paid answer","prepareBundleId":"historical-bundle","prepareBundleDigest":"a".repeat(64),"prepareRunId":"old-paid-job"}]);
    d["jobs"]=json!([{"id":"old-paid-job","kind":"assistant","status":"completed","result":{"proposals":d["proposals"],"text":"paid history".repeat(20_000)}},
        {"id":"audio-evidence","kind":"media_audio","refId":"another-post","status":"running"}]);
    d
}
fn body()->Value {json!({"requestId":"batch-storage-1","proposals":[
    {"itemId":"i","kind":"close","text":"","expectedRevision":1,"eventId":"new-event"},
    {"itemId":"other","kind":"close","text":"","expectedRevision":1,"eventId":"late-event","sessionId":{}},
    {"itemId":"i","kind":"close","text":"","expectedRevision":2},
    {"itemId":"paid","kind":"reply_and_close","text":"Independent duplicate must be held","expectedRevision":1}
]})}
fn normalize_generated(value:&mut Value) {
    match value {
        Value::Array(values)=>values.iter_mut().for_each(normalize_generated),
        Value::Object(fields)=>for (key,v) in fields {
            if matches!(key.as_str(),"createdAt"|"selectedAt") {*v=json!("<time>");}
            else if key=="decisionSha256" {*v=json!("<time-dependent-decision-digest>");}
            else {normalize_generated(v);}
        },
        Value::String(s) if uuid::Uuid::parse_str(s).is_ok()=>*s="<generated-id>".into(),
        _=>(),
    }
}
fn assert_parity(actual:&Value,expected:&Value,baseline:&Value) {
    let mut left=actual.clone();let mut right=expected.clone();
    normalize_generated(&mut left);normalize_generated(&mut right);assert_eq!(left,right,"full reducer and scoped persistence parity");
    for table in ["jobs","operations","approvals","conversations"] {assert_eq!(actual[table],baseline[table],"exact retained {table}");}
    for table in ["proposals","feedback","audit"] {
        let old=crate::list(baseline,table);assert!(crate::list(actual,table).starts_with(old),"immutable historical {table}");
    }
}

#[test]
fn operator_batch_projection_matches_full_reducer_with_partial_holds_and_paid_unknown_history() {
    let baseline=fixture();let request=body();let input=inputs(&request).unwrap();
    let before=project(&baseline,&request,&input).unwrap();
    assert!(before.to_string().len()*10<baseline.to_string().len());
    assert_eq!(before["operations"],baseline["operations"]);assert_eq!(before["jobs"].as_array().unwrap().len(),1);
    assert_eq!(before["proposals"],baseline["proposals"]);
    let mut full=baseline.clone();let full_result=operator_batch::create_proposals(&mut full,&request,&actor()).unwrap();
    let mut after=before.clone();let result=operator_batch::create_proposals(&mut after,&request,&actor()).unwrap();
    assert_eq!(result["created"],1);assert_eq!(result["rejected"],3);
    let mut normalized_result=result.clone();let mut normalized_full=full_result;
    normalize_generated(&mut normalized_result);normalize_generated(&mut normalized_full);assert_eq!(normalized_result,normalized_full);
    validate(&before,&after,&request,&actor(),&input,&result).unwrap();
    let decision=after["proposals"].as_array().unwrap().last().unwrap();
    assert!(crate::operator_close::current(&after,decision,&after["items"][0]).unwrap(),"actual owner-close proof remains verifiable");
    let mut merged=baseline.clone();merge(&mut merged,&before,&after).unwrap();assert_parity(&merged,&full,&baseline);
    assert_eq!(merged["items"][1]["workflow"],"attention","late feedback failure rolls back only its item");
    eprintln!("OPERATOR_BATCH_SCOPE fixtureFullBytes={} fixtureProjectedBytes={}",baseline.to_string().len(),before.to_string().len());
    for table in ["operations","jobs","posts","scopeOwners"] {
        let mut corrupt=after.clone();corrupt[table]=json!({"protected":"changed"});
        assert!(validate(&before,&corrupt,&request,&actor(),&input,&result).is_err(),"protected {table}");
    }
}

#[test]
fn operator_batch_projection_preserves_observed_historical_origin() {
    let mut baseline=fixture();
    let historical=json!({"id":"observed-source","itemId":"other","revision":1,"itemRevision":0,
        "status":"draft","kind":"reply_and_close","text":"Historically observed answer"});
    let mut current=historical.clone();current["revision"]=json!(2);current["itemRevision"]=json!(1);
    current["status"]=json!("failed");current["text"]=json!("Later edited answer");
    crate::list_mut(&mut baseline,"proposals").push(current);
    crate::list_mut(&mut baseline,"feedback").push(json!({"id":"presentation-1","schemaVersion":2,"kind":"proposal_presented",
        "itemId":"other","sourceProposalId":"observed-source","sourceProposalRevision":1,"origin":historical}));
    let request=json!({"requestId":"batch-observed-origin","proposals":[{"itemId":"other","kind":"reply_and_close",
        "text":"Reviewed reuse","expectedRevision":1,"eventId":"observed-action","sourceProposalId":"observed-source","sourceProposalRevision":1}]});
    let input=inputs(&request).unwrap();let before=project(&baseline,&request,&input).unwrap();
    assert_eq!(before["feedback"].as_array().unwrap().len(),1);
    let mut full=baseline.clone();operator_batch::create_proposals(&mut full,&request,&actor()).unwrap();
    let mut after=before.clone();let result=operator_batch::create_proposals(&mut after,&request,&actor()).unwrap();
    assert_eq!(result["created"],1);validate(&before,&after,&request,&actor(),&input,&result).unwrap();
    assert_eq!(after["proposals"].as_array().unwrap().last().unwrap()["origin"]["text"],"Historically observed answer");
    let mut merged=baseline.clone();merge(&mut merged,&before,&after).unwrap();assert_parity(&merged,&full,&baseline);
}

#[tokio::test]
async fn sqlite_operator_batch_preserves_completed_legacy_generated_origin_via_fallback() {
    let folder=tempfile::tempdir().unwrap();let db=Database::Sqlite(crate::open_db(&folder.path().join("workspace.sqlite")).await.unwrap());
    let mut baseline=fixture();let binding=crate::active_binding(&baseline).unwrap().to_json();
    let target=baseline["items"][0].clone();
    let request=json!({"account":baseline["account"],"connectorBinding":binding,"items":[target]});
    let digest=format!("{:x}",sha2::Sha256::digest(request.to_string().as_bytes()));
    crate::list_mut(&mut baseline,"jobs").push(json!({"id":"legacy-paid-origin","kind":"assistant","purpose":"engine_prepare","status":"completed",
        "prepareBundle":{"id":"legacy-bundle","version":1,"itemIds":["i"],"request":request,"digest":digest}}));
    let origin=json!({"id":"legacy-generated","itemId":"i","revision":1,"itemRevision":1,"status":"draft","kind":"reply_and_close",
        "text":"Exact prior paid text","prepareRunId":"legacy-paid-origin","routeTarget":baseline["items"][0]});
    crate::list_mut(&mut baseline,"proposals").push(origin.clone());baseline["items"][0]["draftOrigin"]=origin;
    assert!(crate::preparation_reservations::capture(&baseline,"legacy-paid-origin").is_ok(),"valid complete legacy bundle");
    let body=json!({"requestId":"legacy-origin-reuse","proposals":[{"itemId":"i","kind":"reply_and_close","text":"Exact prior paid text","expectedRevision":1,"eventId":"legacy-action"}]});
    let input=inputs(&body).unwrap();let narrow=project(&baseline,&body,&input).unwrap();
    assert!(!crate::list(&narrow,"jobs").iter().any(|j|j["id"]=="legacy-paid-origin"));
    assert!(needs_complete_origin(&body,Some(&narrow)),"implicit paid origin selects conservative full capture");

    db.change(|d|{*d=baseline.clone();Ok(())}).await.unwrap();
    let runtime=crate::native_fixture_owner_repair::initialize_db(&db).await.unwrap();
    let baseline=db.read().await.unwrap();
    let mut expected=baseline.clone();let expected_result=operator_batch::create_proposals(&mut expected,&body,&actor()).unwrap();assert_eq!(expected_result["created"],1);
    let (mut result,changed)=db.create_operator_batch_observed(&body,&actor(),&runtime).await.unwrap();assert!(changed);
    let mut expected_result=expected_result;normalize_generated(&mut result);normalize_generated(&mut expected_result);assert_eq!(result,expected_result);
    assert_parity(&db.read().await.unwrap(),&expected,&baseline);
    let mut explicit=body.clone();explicit["proposals"][0]["sourceProposalId"]=json!("legacy-generated");
    assert!(needs_complete_origin(&explicit,None),"explicit and historical presentation sources retain complete job evidence");db.close().await;
}

#[tokio::test]
async fn sqlite_operator_batch_replays_receipt_and_preserves_foreign_event_collision() {
    let folder=tempfile::tempdir().unwrap();let path=folder.path().join("workspace.sqlite");
    let db=Database::Sqlite(crate::open_db(&path).await.unwrap());let baseline=fixture();
    db.change(|d|{*d=baseline.clone();Ok(())}).await.unwrap();
    let runtime=crate::native_fixture_owner_repair::initialize_db(&db).await.unwrap();
    let baseline=db.read().await.unwrap();
    let mut collision=body();collision["proposals"][0]["eventId"]=json!("foreign-event");
    let mut expected=baseline.clone();let expected_result=operator_batch::create_proposals(&mut expected,&collision,&actor()).unwrap();
    let (result,changed)=db.create_operator_batch_observed(&collision,&actor(),&runtime).await.unwrap();assert!(changed);
    assert_eq!(result,expected_result);assert_eq!(result["created"],0);assert_eq!(result["rejected"],4);
    let saved=db.read().await.unwrap();assert_parity(&saved,&expected,&baseline);
    let (retry,changed)=db.create_operator_batch_observed(&collision,&actor(),&runtime).await.unwrap();
    assert!(!changed);assert_eq!(retry["results"],result["results"]);assert_eq!(retry["replayed"],true);assert_eq!(db.read().await.unwrap(),saved);
    let mut changed_request=collision.clone();changed_request["proposals"][0]["kind"]=json!("hide");
    assert!(db.create_operator_batch_observed(&changed_request,&actor(),&runtime).await.is_err());
    let mut foreign_actor=actor();foreign_actor.id="another-operator".into();
    assert!(db.create_operator_batch_observed(&collision,&foreign_actor,&runtime).await.is_err());
    assert_eq!(db.read().await.unwrap(),saved);db.close().await;
    let reopened=Database::Sqlite(crate::open_db(&path).await.unwrap());
    let (_,changed)=reopened.create_operator_batch_observed(&collision,&actor(),&runtime).await.unwrap();assert!(!changed);assert_eq!(reopened.read().await.unwrap(),saved);reopened.close().await;
}

#[test]
fn operator_batch_capture_retains_receipt_collisions_and_legacy_fallback() {
    let request=body();let input=inputs(&request).unwrap();let mut baseline=fixture();
    crate::list_mut(&mut baseline,"audit").push(json!({"id":operator_batch::receipt_id("batch-storage-1"),"action":"unrelated","refId":"another"}));
    let mut view=project(&baseline,&request,&input).unwrap();
    assert!(operator_batch::create_proposals(&mut view,&request,&actor()).is_err());
    baseline["audit"][1]=json!({"id":"noncanonical-receipt","action":"proposal.batch_created","refId":"batch-storage-1"});
    let view=project(&baseline,&request,&input).unwrap();assert_eq!(view["audit"].as_array().unwrap().len(),1);
    crate::list_mut(&mut baseline,"audit").push(json!({"id":operator_batch::receipt_id("batch-storage-1"),"action":"proposal.batch_created","refId":"batch-storage-1"}));
    let mut view=project(&baseline,&request,&input).unwrap();
    assert!(operator_batch::create_proposals(&mut view,&request,&actor()).is_err(),"duplicate canonical/legacy receipt cannot disappear from the scope");
    for invalid in [json!({"requestId":"x","proposals":[null]}),json!({"requestId":"x","proposals":[{"itemId":"i","eventId":{}}]}),json!({"requestId":"x","proposals":[{"kind":"close"}]})] {assert!(inputs(&invalid).is_none());}
}

#[tokio::test]
#[ignore="requires a pristine explicitly isolated communityhero_writer_v51_test_ PostgreSQL fixture"]
async fn postgres_operator_batch_parity_receipt_replay_and_late_sql_rollback() {
    let db=super::super::writer_v51_fixture_db().await;let baseline=fixture();
    db.change(|d|{*d=baseline.clone();Ok(())}).await.unwrap();
    let runtime=crate::runtime_lifecycle_startup::initialize_db_fixture(&db).await.unwrap();
    let baseline=db.read().await.unwrap();
    let Database::Postgres{writer,..}=&db else {unreachable!()};
    sqlx::query("CREATE FUNCTION pg_temp.batch_reject() RETURNS trigger LANGUAGE plpgsql AS 'BEGIN RAISE EXCEPTION ''synthetic final batch receipt rejection''; END;'").execute(writer).await.unwrap();
    sqlx::query("CREATE TRIGGER batch_reject BEFORE INSERT ON communityhero.audit FOR EACH ROW EXECUTE FUNCTION pg_temp.batch_reject()").execute(writer).await.unwrap();
    let failed=db.create_operator_batch_observed(&body(),&actor(),&runtime).await;
    sqlx::query("DROP TRIGGER batch_reject ON communityhero.audit").execute(writer).await.unwrap();
    assert!(failed.is_err());assert_eq!(db.read().await.unwrap(),baseline,"final receipt failure rolls back items, proposals and feedback");
    let mut expected=baseline.clone();operator_batch::create_proposals(&mut expected,&body(),&actor()).unwrap();
    let (result,changed)=db.create_operator_batch_observed(&body(),&actor(),&runtime).await.unwrap();assert!(changed);assert_eq!(result["created"],1);assert_eq!(result["rejected"],3);
    let saved=db.read().await.unwrap();assert_parity(&saved,&expected,&baseline);
    let receipt=db.read_operator_batch_receipt("batch-storage-1").await.unwrap().unwrap();assert_eq!(receipt["result"],result);
    let (retry,changed)=db.create_operator_batch_observed(&body(),&actor(),&runtime).await.unwrap();assert!(!changed);assert_eq!(retry["results"],result["results"]);assert_eq!(db.read().await.unwrap(),saved);
    let mut collision=body();collision["requestId"]=json!("batch-foreign-event");collision["proposals"][0]["itemId"]=json!("other");collision["proposals"][0]["eventId"]=json!("foreign-event");
    let mut expected=saved.clone();let expected_result=operator_batch::create_proposals(&mut expected,&collision,&actor()).unwrap();
    let (mut result,_)=db.create_operator_batch_observed(&collision,&actor(),&runtime).await.unwrap();let mut expected_result=expected_result;
    normalize_generated(&mut result);normalize_generated(&mut expected_result);assert_eq!(result,expected_result);assert_parity(&db.read().await.unwrap(),&expected,&saved);
    db.close().await;
}
