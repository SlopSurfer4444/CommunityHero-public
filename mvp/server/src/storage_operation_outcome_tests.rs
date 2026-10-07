use super::*;

// Only generated clocks/UUIDs differ between two invocations of the same
// domain transition. Existing history and every other byte remain comparable.
fn comparable(mut data: Value, before: &Value, op: &Value) -> Value {
    crate::row_mut(&mut data,"operations",op["id"].as_str().unwrap()).unwrap()["updatedAt"] = Value::Null;
    let audit_count = before["audit"].as_array().unwrap().len();
    for record in data["audit"].as_array_mut().unwrap().iter_mut().skip(audit_count) {
        record["id"] = Value::Null; record["createdAt"] = Value::Null;
    }
    let feedback_count = before["feedback"].as_array().unwrap().len();
    for record in data["feedback"].as_array_mut().unwrap().iter_mut().skip(feedback_count) { record["createdAt"] = Value::Null; }
    data
}

async fn fixture(action: &str, mode: &str) -> (Database, tempfile::TempDir, Value, Value) {
    let (app,temp) = crate::tests::test_app().await;
    let mut data = app.db.read().await.unwrap();
    let p = crate::create_proposal(&mut data,&json!({"itemId":"item-1","kind":"close","expectedRevision":1})).unwrap();
    let op = json!({"id":"outcome-operation","itemId":"item-1","proposalId":p["id"],"attemptId":"attempt",
        "status":"dispatching","target":p["routeTarget"],"dispatchAuthority":{"actor":"synthetic"},
        "action":{"actionId":"outcome-action","action":action,"itemId":"comment-1","contextEvidenceDigest":"digest"}});
    data["operations"] = json!([op]);
    data["jobs"] = json!([{"id":"unrelated-job","kind":"assistant","status":"completed","result":{"preserve":"history"}}]);
    data["audit"] = json!([{"id":"prior-audit","action":"synthetic","refId":"prior"}]);
    data["feedback"] = json!([{"id":"prior-feedback","itemId":"item-1","keep":{"history":true}}]);
    match mode {
        "waiting" => data["items"][0]["workflow"] = json!("waiting"),
        "rebound" => data["connectorBinding"]["revision"] = json!(99),
        _ => {}
    }
    if mode == "rebound" { assert!(!crate::outcome_matches_item(&data,&op)); }
    else { assert!(crate::outcome_matches_item(&data,&op)); }
    let Database::Sqlite(pool) = &app.db else { unreachable!() };
    sqlx::query("UPDATE workspace SET payload=? WHERE id=1").bind(data.to_string()).execute(pool).await.unwrap();
    (app.db.clone(),temp,data,op)
}

#[tokio::test]
async fn operation_outcome_matches_full_domain_for_statuses_actions_and_foreign_bindings() {
    for status in ["unknown","failed","stale","succeeded"] {
        for action in ["close","reply_and_close","hide","delete"] {
            for mode in ["normal","waiting","rebound"] {
                let (db,_temp,before,op) = fixture(action,mode).await;
                let evidence = json!({"synthetic":"outcome"});
                let mut expected = before.clone();
                crate::apply_operation_outcome(&mut expected,&op,status,evidence.clone()).unwrap();
                assert!(db.change_operation_outcome_observed(&op,status,|d| crate::apply_operation_outcome(d,&op,status,evidence)).await.unwrap().1);
                let actual = db.read().await.unwrap();
                assert_eq!(comparable(actual.clone(),&before,&op),comparable(expected,&before,&op),"{status}/{action}/{mode}");
                // Repeated receipt keeps the existing feedback event, while
                // retaining the historical domain behavior of another audit.
                let mut repeated = actual.clone();
                crate::apply_operation_outcome(&mut repeated,&op,status,json!({"synthetic":"again"})).unwrap();
                db.change_operation_outcome_observed(&op,status,|d| crate::apply_operation_outcome(d,&op,status,json!({"synthetic":"again"}))).await.unwrap();
                let final_data = db.read().await.unwrap();
                assert_eq!(comparable(final_data.clone(),&actual,&op),comparable(repeated,&actual,&op));
                assert_eq!(final_data["feedback"],actual["feedback"]);
                db.close().await;
            }
        }
    }
}

#[tokio::test]
async fn operation_outcome_rolls_back_retargeting_callback_errors_and_scope_escapes() {
    let (db,_temp,before,op) = fixture("close","normal").await;
    for field in ["id","itemId","proposalId","attemptId","target","dispatchAuthority","action","editorialPolicyVersion"] {
        let mut wrong = op.clone(); wrong[field] = json!("different");
        assert!(db.change_operation_outcome_observed(&wrong,"unknown",|d| crate::apply_operation_outcome(d,&wrong,"unknown",json!({}))).await.is_err(),"{field}");
        assert_eq!(db.read().await.unwrap(),before);
    }
    for mode in 0..10 {
        let result = db.change_operation_outcome_observed(&op,"unknown",|d| {
            crate::apply_operation_outcome(d,&op,"unknown",json!({}))?;
            match mode {
                0 => d["account"] = json!("other"),
                1 => d["operations"][0]["action"]["itemId"] = json!("other"),
                2 => d["proposals"][0]["text"] = json!("changed"),
                3 => d["items"][0]["workflow"] = json!("closed"),
                4 => d["jobs"] = json!([{"id":"injected"}]),
                5 => d["feedback"][0]["itemId"] = json!("other"),
                6 => d["audit"][0]["refId"] = json!("other"),
                7 => d["audit"][0]["id"] = json!("prior-audit"),
                8 => d["operations"][0]["executeReceipt"] = json!("overwritten"),
                _ => return Err(internal("synthetic callback failure")),
            }
            Ok(())
        }).await;
        assert!(result.is_err(),"{mode}");
        assert_eq!(db.read().await.unwrap(),before,"{mode}");
    }
    db.close().await;
}

#[tokio::test]
async fn operation_outcome_readiness_preserves_only_current_ready_siblings() {
    for status in ["failed","stale","unknown"] {
        for mode in ["none","close","hide","delete","reply","blank_reply","old_revision",
            "old_context","old_branch","other_item","failed_sibling","unknown_kind"] {
            let (db,_temp,mut before,mut op)=fixture("close","normal").await;
            // create_proposal captures its legacy route before advancing an
            // attention item. This readiness fixture needs a current target;
            // the broad legacy fixture intentionally remains unchanged.
            op["target"]=crate::bound_item(&crate::active_binding(&before).unwrap(),
                crate::row(&before,"items","item-1").unwrap()).unwrap();
            db.change(|d| {
                crate::row_mut(d,"operations","outcome-operation")?["target"]=op["target"].clone();
                Ok(())
            }).await.unwrap();
            before=db.read().await.unwrap();
            if mode!="none" {
                let mut sibling=before["proposals"][0].clone();
                sibling["id"]=json!("unrelated-ready-sibling");sibling["status"]=json!("draft");
                match mode {
                    "hide"|"delete"=>sibling["kind"]=json!(mode),
                    "reply"=>{sibling["kind"]=json!("reply_and_close");sibling["text"]=json!("Ready reply");},
                    "blank_reply"=>{sibling["kind"]=json!("reply_and_close");sibling["text"]=json!("  \n");},
                    "old_revision"=>sibling["itemRevision"]=json!(0),
                    "old_context"=>sibling["contextEvidenceDigest"]=json!("old"),
                    "old_branch"=>sibling["branchContextDigest"]=json!("old"),
                    "other_item"=>sibling["itemId"]=json!("different-item"),
                    "failed_sibling"=>sibling["status"]=json!("failed"),
                    "unknown_kind"=>sibling["kind"]=json!("restore"),
                    _=>(),
                }
                db.change(|d| {crate::list_mut(d,"proposals").push(sibling);Ok(())}).await.unwrap();
                before=db.read().await.unwrap();
            }
            let ready=matches!(mode,"close"|"hide"|"delete"|"reply");
            let scoped=project(&before,&op,status).unwrap();
            assert_eq!(scoped["proposals"].as_array().unwrap().len(),1,"siblings must stay read-only");
            assert_eq!(scoped["operationOutcomeHasReadySibling"],ready,"{status}/{mode}");
            let mut expected=before.clone();
            crate::apply_operation_outcome(&mut expected,&op,status,json!({})).unwrap();
            db.change_operation_outcome_observed(&op,status,|d|crate::apply_operation_outcome(d,&op,status,json!({}))).await.unwrap();
            let actual=db.read().await.unwrap();
            assert_eq!(actual["items"][0]["workflow"],if ready {"prepared"} else {"attention"},"{status}/{mode}");
            assert_eq!(actual["items"][0]["revision"],before["items"][0]["revision"]);
            assert!(actual.get("operationOutcomeHasReadySibling").is_none(),"ephemeral hint cannot be stored");
            assert_eq!(comparable(actual,&before,&op),comparable(expected,&before,&op),"{status}/{mode}");
            db.close().await;
        }
    }
}

#[tokio::test]
async fn operation_outcome_readiness_hint_is_immutable_and_revision_is_protected() {
    let (db,_temp,before,op)=fixture("close","normal").await;
    for mode in ["hint","revision","unrelated_item"] {
        assert!(db.change_operation_outcome_observed(&op,"failed",|d| {
            crate::apply_operation_outcome(d,&op,"failed",json!({}))?;
            match mode {
                "hint"=>d["operationOutcomeHasReadySibling"]=json!(true),
                "revision"=>crate::bump(&mut d["items"][0]),
                _=>crate::list_mut(d,"items").push(json!({"id":"other","workflow":"attention"})),
            }
            Ok(())
        }).await.is_err(),"{mode}");
        assert_eq!(db.read().await.unwrap(),before);
    }
    db.close().await;
}

#[tokio::test]
#[ignore = "writes only to an explicitly bound isolated communityhero_dispatch_v12_test_ clone"]
async fn postgres_operation_evidence_and_outcome_clone_acceptance() {
    let url = std::env::var("COMMUNITYHERO_OPERATION_WRITE_TEST_URL").expect("explicit isolated clone URL");
    let expected_database = std::env::var("COMMUNITYHERO_OPERATION_WRITE_TEST_DATABASE").expect("explicit clone identity");
    assert!(expected_database.starts_with("communityhero_dispatch_v12_test_"),"refusing non-test database");
    let options = url.parse::<sqlx::postgres::PgConnectOptions>().unwrap_or_else(|_| panic!("invalid clone connection options"))
        .password(&std::env::var("PGPASSWORD").expect("transient password required"));
    let guard = PgPoolOptions::new().max_connections(1).connect_with(options).await.unwrap_or_else(|_|panic!("clone guard connection failed"));
    let actual_database: String = sqlx::query_scalar("SELECT current_database()").fetch_one(&guard).await.unwrap_or_else(|_|panic!("clone identity query failed"));
    assert!(actual_database==expected_database,"clone identity mismatch");
    guard.close().await;
    // Constructor only obtains a lease after the guarded, explicit clone check.
    // PgConnectOptions also honors transient PGPASSWORD in this URL constructor.
    let db = Database::postgres(&url).await.unwrap_or_else(|_|panic!("clone storage startup failed"));
    let runtime=crate::runtime_lifecycle_startup::initialize_db_fixture(&db).await.unwrap();
    let before = db.read().await.unwrap_or_else(|_|panic!("clone read failed"));
    let op = before["operations"].as_array().unwrap().iter().find(|op| {
        op["action"]["actionId"].as_str().is_some_and(|s|!s.is_empty())
            && op["itemId"].is_string() && op["proposalId"].is_string()
            && crate::row(&before,"items",op["itemId"].as_str().unwrap()).is_ok()
            && crate::row(&before,"proposals",op["proposalId"].as_str().unwrap()).is_ok()
    }).expect("clone needs a complete existing operation").clone();
    let key = op["id"].as_str().unwrap();
    let mut expected = before.clone();
    let receipt = json!({"syntheticCloneAcceptance":true});
    crate::row_mut(&mut expected,"operations",key).unwrap()["executeReceipt"] = receipt.clone();
    db.change_operation_evidence_observed(&op,OperationEvidenceUpdate::ExecuteReceipt(receipt), &runtime).await.unwrap_or_else(|_|panic!("clone execute receipt write failed"));
    assert!(db.read().await.unwrap()==expected,"execute receipt scope mismatch");
    let baseline = json!({"baselineReplyIds":["synthetic-clone-baseline"]});
    crate::row_mut(&mut expected,"operations",key).unwrap()["action"]["readbackEvidence"] = baseline.clone();
    db.change_operation_evidence_observed(&op,OperationEvidenceUpdate::Readback(baseline), &runtime).await.unwrap_or_else(|_|panic!("clone readback evidence write failed"));
    assert!(db.read().await.unwrap()==expected,"readback evidence scope mismatch");
    let evidence_before = expected.clone();
    let mut mismatched = op.clone(); mismatched["action"]["actionId"] = json!("mismatched");
    assert!(db.change_operation_evidence_observed(&mismatched,OperationEvidenceUpdate::ExecuteReceipt(json!({})), &runtime).await.is_err());
    assert!(db.read().await.unwrap()==evidence_before,"evidence mismatch failed to roll back");
    for status in ["unknown","failed","stale","succeeded"] {
        let before = db.read().await.unwrap();
        let mut expected = before.clone();
        crate::apply_operation_outcome(&mut expected,&op,status,json!({"syntheticCloneAcceptance":true})).unwrap();
        db.change_operation_outcome_observed(&op,status,|d| crate::apply_operation_outcome(d,&op,status,json!({"syntheticCloneAcceptance":true})))
            .await.unwrap_or_else(|_|panic!("clone outcome write failed"));
        let after = db.read().await.unwrap();
        assert!(comparable(after.clone(),&before,&op)==comparable(expected,&before,&op),"clone full-domain parity mismatch");
        assert!(db.change_operation_outcome_observed(&op,status,|d| {
            crate::apply_operation_outcome(d,&op,status,json!({}))?;
            d["items"][0]["text"] = json!("forbidden change"); Ok(())
        }).await.is_err());
        assert!(db.read().await.unwrap()==after,"outcome scope escape failed to roll back");
        assert!(db.change_operation_outcome_observed(&mismatched,status,|_| Ok(())).await.is_err());
        assert!(db.read().await.unwrap()==after,"outcome identity mismatch failed to roll back");
    }
    // Fail at the final SQL insert, after earlier UPDATE statements executed:
    // a pre-existing audit identity must roll back the entire outcome unit.
    let before_collision = db.read().await.unwrap();
    let existing_audit_id = before_collision["audit"][0]["id"].clone();
    assert!(existing_audit_id.is_string());
    assert!(db.change_operation_outcome_observed(&op,"unknown",|d| {
        crate::apply_operation_outcome(d,&op,"unknown",json!({"mustRollback":true}))?;
        d["audit"][0]["id"] = existing_audit_id; Ok(())
    }).await.is_err());
    assert!(db.read().await.unwrap()==before_collision,"final SQL insert failure did not roll back earlier writes");
    println!("OPERATION_CLONE_ACCEPTANCE evidence=true outcomeStatuses=4 fullDomainParity=true rollback=true providerCalls=0");
    db.close().await;
}
