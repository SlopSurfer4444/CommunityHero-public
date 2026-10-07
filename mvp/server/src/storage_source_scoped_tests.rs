use super::*;

fn settled_job(id:&str,item:&str,bundle:&str,bytes:usize)->Value {
    json!({"id":id,"kind":"assistant","purpose":"auto_prepare","status":"completed","refId":item,
        "prepareBundle":{"version":1,"id":bundle,"digest":"a".repeat(64),"dependencyDigest":"b".repeat(64),
            "itemIds":[item],"request":{"items":[{"id":item,"text":"captured"}]}},
        "prepareOutcome":{"itemId":item,"status":"needs_attention","reason":"Exact retained decision"},
        "autoPreparationInputs":{(item):{"inputDigest":"saved-input"}},
        "preparationStages":{"first":{"status":"completed","result":{"text":"paid response","sources":[],"assessments":[],"proposals":[]}}},
        "scopeModelAttempt":{"status":"completed","paidResultRef":"immutable-paid-ref"},
        "syntheticColdEvidence":"x".repeat(bytes)})
}

fn fixture(bytes:usize)->Value {
    let mut d=crate::empty();normalize(&mut d);crate::accounts::initialize(&mut d,crate::accounts::Profile::BawRussia).unwrap();
    d["posts"]=json!([{"id":"post","text":"Before"},{"id":"other-post","text":"Other post","officialAuthorId":"brand"}]);
    d["branches"]=json!([{"id":"branch","postId":"post","messages":[]},{"id":"other-branch","postId":"other-post","messages":[]}]);
    d["items"]=json!([{"id":"item","itemId":"external-item","objectId":"post-object","postId":"post","branchId":"branch",
        "providerStatus":"new","workflow":"prepared","draft":"Keep operator text","draftEdited":true,"revision":3},
        {"id":"held","itemId":"external-held","objectId":"other-object","postId":"other-post","branchId":"other-branch",
        "providerStatus":"new","workflow":"attention","draft":"","autoPreparation":{"status":"queued"},"revision":1}]);
    d["proposals"]=json!([{"id":"approved","itemId":"item","status":"approved","text":"Exact saved approved text","generationMetadata":{"paidResultRef":"saved"}}]);
    d["approvals"]=json!([{"id":"consent","status":"approved","proposals":[{"id":"approved","revision":1,"proposal":{"text":"Exact consent"}}]}]);
    d["operations"]=json!([{"id":"unknown","itemId":"item","proposalId":"approved","approvalId":"consent","status":"unknown","receipt":{"old":"retain"}}]);
    d["jobs"]=json!([settled_job("cold","held","cold-bundle",bytes),settled_job("newest","held","newest-bundle",bytes)]);
    d["preparationResearch"]=json!([{"id":"unused","synthetic":"x".repeat(bytes)}]);
    d["audit"]=json!([{"id":"old-audit","action":"history","refId":"item"}]);validate(&d).unwrap();d
}

#[tokio::test]
async fn sqlite_source_completion_keeps_legacy_lifetime_and_rejects_invalid_batch_before_reducer() {
    let temp=tempfile::tempdir().unwrap();let db=Database::Sqlite(crate::open_db(&temp.path().join("completion.sqlite")).await.unwrap());
    db.change(|d|{*d=fixture(128);Ok(())}).await.unwrap();let before=db.read().await.unwrap();
    let mut called=false;let invalid=vec![json!({}),Value::Null];
    let completion=db.change_source_snapshot_scoped_completed(SourceReadIntent::Snapshots(&invalid),|_|{called=true;Ok(())}).await;
    assert!(!called);assert!(completion.outcome.is_err());assert!(!completion.cleanup.has_projections());completion.cleanup.dispose().await;
    assert_eq!(db.read().await.unwrap(),before);
    let completion=db.change_source_snapshot_scoped_completed(SourceReadIntent::Full{reason:"explicit_full_oracle"},|_|Ok(())).await;
    assert!(!completion.outcome.unwrap().1);assert!(!completion.cleanup.has_projections());completion.cleanup.dispose().await;
    let snapshot=snapshot();let completion=db.change_source_snapshot_scoped_completed(SourceReadIntent::Snapshot(&snapshot),|d|crate::merge_snapshot(d,&snapshot)).await;
    assert!(completion.outcome.unwrap().1);assert!(!completion.cleanup.has_projections(),"one-document SQLite cleanup remains internal; no claimed post-permit optimization");
    completion.cleanup.dispose().await;assert_eq!(db.read().await.unwrap()["operations"],before["operations"]);db.close().await;
}
fn snapshot()->Value {json!({"posts":[{"id":"post","text":"After"}],"branches":[{"id":"branch","postId":"post","messages":[{"id":"external-item","authorId":"same-author","role":"customer","text":"Fresh context"}]}],
    "items":[{"id":"item","itemId":"external-item","objectId":"post-object","postId":"post","branchId":"branch","providerStatus":"closed",
        "contextObservedAt":"2026-10-06T00:00:00Z","providerStatusObservedAt":"2026-10-06T00:00:00Z"}]})}
fn normalize_clock(d:&mut Value){
    for branch in crate::list_mut(d,"branches"){branch["observedAt"]=json!("comparison-clock");}
    for item in crate::list_mut(d,"items"){if item["autoPreparation"].get("updatedAt").is_some(){item["autoPreparation"]["updatedAt"]=json!("comparison-clock");}}
    for proposal in crate::list_mut(d,"proposals"){if proposal.get("staleAt").is_some(){proposal["staleAt"]=json!("comparison-clock");}}
}
fn native_repaired_source()->(Value,String,String){
    let(mut d,origin,plan)=crate::answering_repair_plan::tests::fresh_revalidation();
    let child=crate::answering_repair_plan::tests::settle_revalidation_child(&mut d,&origin,&plan,true,false);crate::answering_repair_plan::tests::merge_revalidation(&mut d,&origin);
    crate::row_mut(&mut d,"jobs",&origin).unwrap()["status"]=json!("completed");normalize(&mut d);(d,origin,child)
}
#[test]
fn repaired_automatic_retirement_source_guard_matches_native_origin_and_rejects_human_foreign_paid_changes(){
    let(original,origin,child)=native_repaired_source();let view=project_scoped(&original).unwrap();
    assert_eq!(crate::answering_repair_plan::automatic_proposal_origin(&view,&view["proposals"][1]),Some(origin));
    let mut full=project(&original).unwrap();let mut scoped=view.clone();
    for d in [&mut full,&mut scoped]{d["branches"][0]["messages"][0]["text"]=json!("Current source changed after exact repair settlement");crate::auto_prepare::reconcile_stale(d,chrono::Utc::now().timestamp()+40);}
    assert_eq!(full["proposals"][1]["status"],"stale");assert_eq!(full["proposals"][1]["prepareRunId"],child);
    validate_change(&project(&original).unwrap(),&full).unwrap();validate_scoped_change(&view,&scoped).unwrap();
    normalize_clock(&mut full);normalize_clock(&mut scoped);assert_eq!(full["items"],scoped["items"]);assert_eq!(full["proposals"],scoped["proposals"]);
    for mutation in ["human_history","foreign_company","paid_owner","approved","unknown"]{
        let mut hostile=original.clone();match mutation{
            "human_history"=>hostile["proposals"][1]["history"]=json!([{"text":"human saved edit"}]),
            "foreign_company"=>hostile["proposals"][1]["account"]=json!("LikeAvto"),
            "paid_owner"=>crate::row_mut(&mut hostile,"jobs",&child).unwrap()["repairPaidIntent"]["owner"]["runtimeId"]=json!("foreign"),
            "approved"=>hostile["proposals"][1]["status"]=json!("approved"),_=>hostile["proposals"][1]["status"]=json!("unknown"),
        }
        let before=project_scoped(&hostile).unwrap();let mut after=before.clone();let p=&mut after["proposals"][1];p["status"]=json!("stale");p["revision"]=json!(p["revision"].as_u64().unwrap()+1);p["staleReason"]=json!("Forced retirement");p["staleAt"]=json!(crate::now());
        assert!(validate_scoped_change(&before,&after).is_err(),"{mutation}: purpose alone cannot grant automatic repair ownership");
    }
}
#[tokio::test]
async fn sqlite_real_scoped_source_retires_proven_repair_child_without_losing_paid_history(){
    let temp=tempfile::tempdir().unwrap();let db=Database::Sqlite(crate::open_db(&temp.path().join("repair-source.sqlite")).await.unwrap());
    // Paid reservations are created, captured and settled in their real native
    // order. A wholesale settled afterimage cannot stand in for that history.
    let(origin,plan)=crate::answering_repair_plan::tests::fresh_revalidation_sqlite(&db,false).await;
    let child=crate::answering_repair_plan::tests::settle_revalidation_child_sqlite(&db,&origin,&plan,true,false).await;
    db.change(|d|{
        crate::answering_repair_plan::tests::merge_revalidation(d,&origin);
        crate::row_mut(d,"jobs",&origin)?["status"]=json!("completed");Ok(())
    }).await.unwrap();let before=db.read().await.unwrap();
    let mut incoming=before["branches"][0].clone();incoming["messages"][0]["text"]=json!("New source details retire exact repaired automatic draft");let snapshot=json!({"posts":[],"items":[],"branches":[incoming]});
    let mut expected=before.clone();crate::merge_snapshot(&mut expected,&snapshot).unwrap();assert_eq!(expected["proposals"][1]["status"],"stale");
    assert!(db.change_source_snapshot_scoped_observed(SourceReadIntent::Snapshot(&snapshot),|d|crate::merge_snapshot(d,&snapshot)).await.unwrap().1);
    let mut actual=db.read().await.unwrap();assert_eq!(actual["proposals"][1]["prepareRunId"],child);assert_eq!(actual["jobs"],before["jobs"]);assert_eq!(actual["operations"],before["operations"]);assert_eq!(actual["approvals"],before["approvals"]);
    normalize_clock(&mut expected);normalize_clock(&mut actual);assert_eq!(actual,expected);db.close().await;
}

#[test]
fn scoped_source_keeps_global_source_unknown_and_ordered_pointerless_maintenance() {
    let original=fixture(8192);let view=project_scoped(&original).unwrap();
    assert!(view["jobs"].as_array().unwrap().is_empty(),"settled unreferenced bodies are not source authority");
    assert_eq!(view["sourceJobControls"]["jobs"].as_array().unwrap().len(),2);
    for table in ["posts","branches","items","proposals","operations","materials","knowledge_entries","knowledge_versions"]{assert_eq!(view[table],original[table],"complete {table}");}
    let mut full=original.clone();let mut scoped=view.clone();
    crate::auto_prepare::reconcile_stale(&mut full,1_791_244_800);crate::auto_prepare::reconcile_stale(&mut scoped,1_791_244_800);
    assert_eq!(scoped["items"],full["items"]);assert_eq!(scoped["items"][1]["autoPreparation"]["jobId"],"newest");
    assert_eq!(scoped["items"][1]["autoPreparation"]["inputDigest"],"saved-input");
    validate_scoped_change(&view,&scoped).unwrap();
    for field in ["text","generationMetadata"]{assert_eq!(scoped["proposals"][0][field],original["proposals"][0][field]);}
}

#[test]
fn source_control_marker_and_selected_full_job_are_not_forgeable() {
    let mut original=fixture(0);original["jobs"][0]["modelMaterialReceipts"]=json!([{"nativeJobId":"native-result","paidResultRef":"paid"}]);
    crate::list_mut(&mut original,"jobs").push(json!({"id":"native-result","kind":"material_acquisition","status":"completed","retainedEvidence":{"paidResultRef":"paid"}}));
    let valid=project_scoped(&original).unwrap();assert_eq!(valid["jobs"].as_array().unwrap().len(),2);
    for mutation in 0..5 {
        let mut forged=valid.clone();match mutation {
            0=>forged["sourceJobControls"]["complete"]=json!(false),
            1=>forged["sourceJobControls"]["version"]=json!(2),
            2=>{let duplicate=forged["sourceJobControls"]["jobs"][0].clone();forged["sourceJobControls"]["jobs"].as_array_mut().unwrap().push(duplicate);},
            3=>forged["jobs"][0]["prepareOutcome"]["reason"]=json!("forged control"),
            _=>forged["sourceJobControls"]["jobs"][0]["id"]=json!("not-the-full-job"),
        }assert!(validate_source_controls(&forged).is_err(),"{mutation}");
    }
    original["sourceJobControls"]=valid["sourceJobControls"].clone();assert!(project_scoped(&original).is_err());assert!(project(&original).is_err());
}

#[tokio::test]
async fn sqlite_scoped_source_full_domain_equivalence_and_hostile_rollback() {
    let temp=tempfile::tempdir().unwrap();let db=Database::Sqlite(crate::open_db(&temp.path().join("scoped.sqlite")).await.unwrap());
    let original=fixture(8192);db.change(|d|{*d=original.clone();Ok(())}).await.unwrap();let baseline=db.read().await.unwrap();
    assert!(!db.change_source_snapshot_scoped_observed(SourceReadIntent::Snapshot(&snapshot()),|_|Ok(())).await.unwrap().1);
    for mutation in 0..10 {
        let rejected:ApiResult<((),bool)>=db.change_source_snapshot_scoped_observed(SourceReadIntent::Snapshot(&snapshot()),|d|{
            crate::merge_snapshot(d,&snapshot())?;match mutation {
                0=>d["operations"][0]["receipt"]=json!("forged"),
                1=>d["proposals"][0]["text"]=json!("lost saved text"),
                2=>d["items"][0]["itemId"]=json!("wrong recipient"),
                3=>d["sourceJobControls"]["complete"]=json!(false),
                4=>d["account"]=json!("another-company"),
                5=>d["proposals"][0]["generationMetadata"]["paidResultRef"]=json!("lost"),
                6=>d["audit"]=json!([{"id":"old-audit","action":"overwrite"}]),
                7=>d["connectorBinding"]=crate::accounts::Profile::LikeAvto.binding(),
                8=>d["proposals"][0]["status"]=json!("draft"),
                _=>{d["proposals"][0]["status"]=json!("unknown");d["proposals"][0]["revision"]=json!(1);},
            }Ok(())
        }).await;assert!(rejected.is_err(),"hostile mutation {mutation}");assert_eq!(db.read().await.unwrap(),baseline,"atomic rejected {mutation}");
    }
    let mut expected=baseline.clone();crate::merge_snapshot(&mut expected,&snapshot()).unwrap();
    assert!(db.change_source_snapshot_scoped_observed(SourceReadIntent::Snapshot(&snapshot()),|d|crate::merge_snapshot(d,&snapshot())).await.unwrap().1);
    let mut actual=db.read().await.unwrap();normalize_clock(&mut actual);normalize_clock(&mut expected);assert_eq!(actual,expected,"full reducer and real scoped writer persisted parity");
    assert!(actual.get("sourceJobControls").is_none());
    for table in ["jobs","approvals","operations","preparationResearch"]{assert_eq!(actual[table],baseline[table],"canonical {table} retained");}
    db.close().await;
}

#[test]
fn full_and_scoped_source_retire_only_the_same_unapproved_automatic_draft() {
    let mut original=fixture(0);original["proposals"]=json!([]);original["approvals"]=json!([]);original["operations"]=json!([]);
    original["items"][0]["platform"]=json!("instagram");original["items"][0]["postKey"]=json!("post-key");
    original["items"][0]["conversationKey"]=json!("thread");original["items"][0]["draft"]=json!("");original["items"][0]["draftEdited"]=json!(false);
    let proposal=crate::create_generated_proposal(&mut original,&json!({"itemId":"item","kind":"close","text":"","expectedRevision":3})).unwrap();
    let bundle=crate::prepare_bundle::build(&original,&[json!("item")],&[]).unwrap();
    let mut job=settled_job("origin","item","current-bundle",0);job["prepareBundle"]=bundle.clone();crate::list_mut(&mut original,"jobs").push(job);
    let record=crate::row_mut(&mut original,"proposals",proposal["id"].as_str().unwrap()).unwrap();
    record["prepareRunId"]=json!("origin");record["prepareBundleId"]=bundle["id"].clone();record["prepareBundleDigest"]=bundle["digest"].clone();
    assert!(crate::proposal_current(&original,&original["proposals"][0]).is_ok(),"real saved bundle is current before the source change");
    let before=project_scoped(&original).unwrap();let mut full=original.clone();let mut scoped=before.clone();
    crate::merge_snapshot(&mut full,&snapshot()).unwrap();crate::merge_snapshot(&mut scoped,&snapshot()).unwrap();
    assert_eq!(scoped["proposals"][0]["status"],"stale");validate_scoped_change(&before,&scoped).unwrap();
    normalize_clock(&mut full);normalize_clock(&mut scoped);
    for table in ["posts","branches","items","proposals","operations"]{assert_eq!(scoped[table],full[table],"full/scoped real stale {table}");}
    for status in ["approved","dispatching","unknown"] {
        let mut protected=original.clone();protected["proposals"][0]["status"]=json!(status);let before=project_scoped(&protected).unwrap();
        let mut after=before.clone();after["proposals"][0]["status"]=json!("stale");after["proposals"][0]["revision"]=json!(2);
        after["proposals"][0]["staleAt"]=json!("2026-10-06T00:00:00Z");after["proposals"][0]["staleReason"]=json!("forged");
        assert!(validate_scoped_change(&before,&after).is_err(),"source cannot retire {status}");
    }
}

#[tokio::test]
#[ignore="requires a pristine explicitly isolated communityhero_writer_v51_test_ PostgreSQL BAW fixture"]
async fn postgres_scoped_source_controls_delta_and_late_metadata_rollback() {
    let url=std::env::var("COMMUNITYHERO_WRITER_V51_TEST_URL").expect("explicit isolated fixture URL");
    let name=std::env::var("COMMUNITYHERO_WRITER_V51_TEST_DATABASE").expect("explicit isolated fixture name");
    let db=super::super::preparation::writer_v51_fixture_db_for_profile_with(&url,&name,crate::accounts::Profile::BawRussia).await;
    let original=fixture(32*1024);db.change(|d|{*d=original.clone();Ok(())}).await.unwrap();let baseline=db.read().await.unwrap();
    let Database::Postgres{writer,..}=&db else{unreachable!()};
    let sql=format!("SELECT payload::text AS original,({})::text AS control FROM (SELECT $1::jsonb AS payload) input",source_jobs::source_control_sql());
    let mut cases=crate::list(&baseline,"jobs").to_vec();
    for field in ["modelMaterialReceipts","videoFrameNeeds","nativeSourceOriginJobId","repairPaidIntent"] {
        let mut changed=cases[0].clone();changed[field]=json!({"nativeJobId":"origin"});cases.push(changed);
    }
    for job in cases {
        let row=sqlx::query(sqlx::AssertSqlSafe(sql.as_str())).bind(job.to_string()).bind(Vec::<String>::new()).bind(true).fetch_one(writer).await.unwrap();
        let stored=parse(row.try_get::<&str,_>("original").unwrap()).unwrap();let actual=parse(row.try_get::<&str,_>("control").unwrap()).unwrap();
        let (expected,full)=source_jobs::source_control(&stored);assert_eq!(actual,json!({"fullRequired":full,"job":expected}),"stored SQL/Rust source-control parity");
    }
    let mut incoming=snapshot();crate::list_mut(&mut incoming,"posts").push(json!({"id":"appended-post","text":"New"}));
    let admit=|d:&mut Value|->ApiResult<()>{crate::merge_snapshot(d,&incoming)?;d["sync"]["scopedFixture"]=json!("committed");
        crate::list_mut(d,"audit").push(json!({"id":"scoped-audit","action":"source.fixture","refId":"item"}));Ok(())};
    sqlx::query("CREATE FUNCTION pg_temp.scoped_metadata_reject() RETURNS trigger LANGUAGE plpgsql AS 'BEGIN RAISE EXCEPTION ''synthetic late scoped metadata rejection''; END;'").execute(writer).await.unwrap();
    sqlx::query("CREATE TRIGGER scoped_metadata_reject BEFORE UPDATE ON communityhero.workspaces FOR EACH ROW EXECUTE FUNCTION pg_temp.scoped_metadata_reject()").execute(writer).await.unwrap();
    assert!(db.change_source_snapshot_scoped_observed(SourceReadIntent::Snapshot(&incoming),admit).await.is_err());
    sqlx::query("DROP TRIGGER scoped_metadata_reject ON communityhero.workspaces").execute(writer).await.unwrap();
    assert_eq!(db.read().await.unwrap(),baseline,"late metadata failure rolls back guarded row deltas, insert and audit");
    let mut expected=baseline.clone();admit(&mut expected).unwrap();
    assert!(db.change_source_snapshot_scoped_observed(SourceReadIntent::Snapshot(&incoming),|d|{
        assert_eq!(*d,project_scoped(&baseline).unwrap(),"actual scoped SQL loader equals in-memory closure");admit(d)
    }).await.unwrap().1);
    let mut actual=db.read().await.unwrap();normalize_clock(&mut expected);normalize_clock(&mut actual);assert_eq!(actual,expected,"actual scoped PG commit equals full reducer");
    assert!(!db.change_source_snapshot_scoped_observed(SourceReadIntent::Snapshot(&incoming),|_|Ok(())).await.unwrap().1);
    assert_eq!(db.read().await.unwrap()["jobs"],baseline["jobs"]);
    // An injected same-transaction conflict proves the delta checks the exact
    // captured old body as well as ID/physical ordinal. It must not commit an
    // earlier table's successful update when a later CAS row is rejected.
    let saved=db.read().await.unwrap();let mut tx=writer.begin().await.unwrap();
    let (before,ordinals)=load_scoped(&mut tx).await.unwrap();let mut after=before.clone();
    after["posts"][0]["text"]=json!("must roll back before later item CAS failure");after["items"][0]["reason"]=json!("domain source reason");
    validate_scoped_change(&before,&after).unwrap();
    sqlx::query("UPDATE communityhero.items SET payload=jsonb_set(payload,'{reason}','\"injected conflict\"'::jsonb) WHERE workspace_id=$1 AND id='item'")
        .bind(WORKSPACE).execute(&mut *tx).await.unwrap();
    assert!(guarded_delta::persist(&mut tx,&before,&after,&ordinals).await.is_err());tx.rollback().await.unwrap();
    assert_eq!(db.read().await.unwrap(),saved,"CAS rejection rolls back earlier source updates and injected fixture conflict");db.close().await;
}
