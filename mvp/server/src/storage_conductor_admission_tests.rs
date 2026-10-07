use super::*;

fn actor()->Actor {Actor::local_owner("conductor-scoped-test")}
fn body(request:&str)->Value {crate::conductor::parse(&json!({"requestId":request,"mode":"execute",
    "scope":{"itemIds":["item-1"]},"actionKinds":["close"]})).unwrap()}
fn fixture()->Value {
    let mut d=crate::empty();normalize(&mut d);d["connectorBinding"]=crate::legacy_binding();
    d["items"]=json!([{"id":"item-1","itemId":"comment-1","objectId":"11391","postKey":"11391:post-1",
        "conversationKey":"11391:comment-1","revision":1,"createdAt":"2026-09-30T12:00:00Z",
        "connectorBinding":crate::legacy_binding()}]);
    for n in 0..20 {crate::list_mut(&mut d,"jobs").push(json!({"id":format!("cold-{n}"),"kind":"assistant","status":"completed","refId":"not-a-campaign","bundle":"cold-paid-history".repeat(1000)}));}
    crate::list_mut(&mut d,"operations").push(json!({"id":"protected-unknown","status":"unknown","requiresReadback":true,"providerRetryAllowed":false}));
    // These tests exercise a positive execute-grant admission. Its synthetic
    // lifecycle and send gate are explicit; the shared App fixture stays closed.
    crate::native_fixture_owner_repair::initialize_workspace(&mut d).unwrap();
    crate::connection_gate::fixture_open(&mut d).unwrap();
    d
}
fn append(d:&mut Value,body:&Value,actor:&Actor,hash:&str)->ApiResult<String> {
    let grant=crate::conductor_authority::create_grant(d,actor,body)?;
    let account=d["account"].clone();let binding=crate::active_binding(d)?.to_json();
    let key=crate::new_job(d,"conductor",crate::required(body,"requestId")?)?;
    let job=crate::row_mut(d,"jobs",&key)?;job["purpose"]=json!("autonomous_conductor");job["account"]=account;job["connectorBinding"]=binding;
    job["conductor"]=json!({"version":1,"desiredState":"running","leaseGeneration":1,"mode":body["mode"],
        "scope":body["scope"],"limits":body["limits"],"grant":grant,"startPayloadHash":hash,
        "childEverStarted":false,"checkpoint":{"relativePath":format!("conductor/{key}/queue.json")},"progress":null,"itemHolds":[]});
    Ok(key)
}

#[test]
fn conductor_projection_omits_cold_work_and_operations_but_keeps_all_grant_history(){
    let mut full=fixture();let first=body("prior");let run=append(&mut full,&first,&actor(),"prior-hash").unwrap();
    crate::row_mut(&mut full,"jobs",&run).unwrap()["conductor"]["desiredState"]=json!("paused");
    let view=project(&full,&body("next")).unwrap();
    assert_eq!(view["jobs"].as_array().unwrap().len(),1);
    assert_eq!(view["jobs"][0],crate::row(&full,"jobs",&run).unwrap().clone());
    assert!(view["operations"].as_array().unwrap().is_empty());
    assert_eq!(view["items"],full["items"]);assert_eq!(metadata(&view),metadata(&full));
    assert!(view.to_string().len()<full.to_string().len()/20);
}

#[test]
fn start_delta_is_append_only_exact_scope_actor_and_cutoff(){
    let request=body("next");let before=project(&fixture(),&request).unwrap();let actor=actor();
    let mut after=before.clone();append(&mut after,&request,&actor,"hash").unwrap();
    validate_delta(&before,&after,&request,&actor,"hash").unwrap();
    for field in ["scope","grant","startPayloadHash","leaseGeneration","checkpoint","mode"] {
        let mut forged=after.clone();forged["jobs"][0]["conductor"][field]=json!("forged");
        assert!(validate_delta(&before,&forged,&request,&actor,"hash").is_err(),"{field}");
    }
    for table in ["items","operations","audit","approvals"] {
        let mut forged=after.clone();forged[table]=json!([{"id":"forged"}]);
        assert!(validate_delta(&before,&forged,&request,&actor,"hash").is_err(),"{table}");
    }
    // local_owner's argument is CSRF, not an authority generation.
    let new_session=Actor::local_owner("different-csrf");
    validate_delta(&before,&after,&request,&new_session,"hash").unwrap();
    let remote=Actor{id:"remote-owner".into(),name:"Remote owner".into(),role:"owner".into(),
        csrf_token:"test-session".into(),authority_generation:Some("a".repeat(64))};
    assert_ne!(crate::dispatch_authority::approval_binding(&actor),crate::dispatch_authority::approval_binding(&remote));
    assert!(validate_delta(&before,&after,&request,&remote,"hash").is_err());
    let mut remote_after=before.clone();append(&mut remote_after,&request,&remote,"hash").unwrap();
    validate_delta(&before,&remote_after,&request,&remote,"hash").unwrap();
    let mut rotated=remote.clone();rotated.authority_generation=Some("b".repeat(64));
    assert_ne!(crate::dispatch_authority::approval_binding(&remote),crate::dispatch_authority::approval_binding(&rotated));
    assert!(validate_delta(&before,&remote_after,&request,&rotated,"hash").is_err());
    let mut foreign=before.clone();foreign["items"][0]["connectorBinding"]=crate::accounts::Profile::BawRussia.binding();
    assert!(append(&mut foreign,&request,&actor,"hash").is_err());
    let mut missing=before.clone();missing["items"]=json!([]);assert!(append(&mut missing,&request,&actor,"hash").is_err());
    let mut cutoff=request.clone();cutoff["scope"]["cutoffUtc"]=json!("2026-09-30T11:59:59Z");
    let mut invalid=before.clone();append(&mut invalid,&cutoff,&actor,"hash").unwrap();
    assert!(validate_delta(&before,&invalid,&cutoff,&actor,"hash").is_err());
    cutoff["scope"]["cutoffUtc"]=json!("2026-09-30T12:00:00Z");
    let mut exact=before.clone();append(&mut exact,&cutoff,&actor,"hash").unwrap();
    validate_delta(&before,&exact,&cutoff,&actor,"hash").unwrap();
}

#[test]
fn all_current_campaigns_fence_new_start_and_replay_cannot_append(){
    for state in ["running","pausing"] {
        let mut full=fixture();let run=append(&mut full,&body("prior"),&actor(),"prior-hash").unwrap();
        crate::row_mut(&mut full,"jobs",&run).unwrap()["conductor"]["desiredState"]=json!(state);
        let request=body("next");let before=project(&full,&request).unwrap();let mut after=before.clone();append(&mut after,&request,&actor(),"hash").unwrap();
        assert!(validate_delta(&before,&after,&request,&actor(),"hash").is_err(),"{state}");
    }
    let request=body("prior");let mut full=fixture();let run=append(&mut full,&request,&actor(),"hash").unwrap();
    crate::row_mut(&mut full,"jobs",&run).unwrap()["conductor"]["desiredState"]=json!("paused");
    let before=project(&full,&request).unwrap();validate_delta(&before,&before,&request,&actor(),"hash").unwrap();
    let mut after=before.clone();append(&mut after,&request,&actor(),"new-hash").unwrap();
    assert!(validate_delta(&before,&after,&request,&actor(),"new-hash").is_err());
    let mut modified=before.clone();modified["jobs"][0]["conductor"]["leaseGeneration"]=json!(2);
    assert!(validate_delta(&before,&modified,&request,&actor(),"hash").is_err());
}

async fn assert_transaction_parity(db:&Database){
    let original=fixture();db.change(|d|{*d=original.clone();Ok(())}).await.unwrap();
    let request=body("new-request");let actor=actor();
    let(run,changed)=db.change_conductor_start_observed(&request,&actor,"hash",|d|append(d,&request,&actor,"hash")).await.unwrap();assert!(changed);
    let admitted=db.read().await.unwrap();let mut expected=original.clone();crate::list_mut(&mut expected,"jobs").push(crate::row(&admitted,"jobs",&run).unwrap().clone());
    assert_eq!(admitted,expected,"only the new conductor job may be added");
    let(_,changed)=db.change_conductor_start_observed(&request,&actor,"hash",|d|Ok(crate::row(d,"jobs",&run)?.clone())).await.unwrap();assert!(!changed);
    let conflicting=body("conflicting-request");
    assert!(db.change_conductor_start_observed(&conflicting,&actor,"other-hash",|d|append(d,&conflicting,&actor,"other-hash")).await.is_err());
    assert_eq!(db.read().await.unwrap(),admitted,"active admission failure must roll back");
    assert!(db.change_conductor_start_observed(&request,&actor,"hash",|d|{d["operations"]=json!([]);d["account"]=json!("BAW Russia");Ok(())}).await.is_err());
    assert_eq!(db.read().await.unwrap(),admitted,"company/context tampering must roll back");
}
#[tokio::test]
async fn sqlite_conductor_start_atomic_delta_preserves_unknown_and_paid_history(){
    let(app,_temp)=crate::tests::test_app().await;assert_transaction_parity(&app.db).await;app.db.close().await;
}
#[tokio::test]
#[ignore="requires a fresh isolated PostgreSQL fixture; run this selector alone"]
async fn postgres_conductor_start_scoped_global_ordinal_and_atomic_history(){
    let db=super::super::preparation::writer_v51_fixture_db().await;assert_transaction_parity(&db).await;
    let Database::Postgres{reader,..}=&db else{panic!("PostgreSQL fixture required")};
    let records=sqlx::query("SELECT kind,ordinal FROM communityhero.jobs WHERE workspace_id=$1 ORDER BY ordinal").bind(WORKSPACE).fetch_all(reader).await.unwrap();
    assert_eq!(records.len(),21);
    for(index,record)in records.iter().enumerate(){assert_eq!(record.try_get::<i32,_>("ordinal").unwrap(),index as i32);}
    assert_eq!(records.last().unwrap().try_get::<&str,_>("kind").unwrap(),"conductor");db.close().await;
}
