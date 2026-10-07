use super::*;
use serde_json::json;

const NOW:i64=1_800_000_000;
fn stamp(at:i64)->String{chrono::DateTime::from_timestamp(at,0).unwrap().to_rfc3339()}

fn fixture()->Value{
    let mut d=crate::empty();
    d["settings"]["autoPreparation"]=json!({"revalidation":{"enabled":true,"debounceSeconds":9999},
        "publicFactFollowup":{"version":1,"enabled":true},"unrelated":"OMITTED_SETTINGS".repeat(1000)});
    d["items"]=json!([{"id":"prepared","workflow":"prepared","text":"OMITTED_ITEM"},
        {"id":"stale","workflow":"attention","autoPreparation":{"status":"stale"}},
        {"id":"attention","workflow":"attention"},{"id":"closed","workflow":"closed"},null,"prepared"]);
    let mut jobs=vec![];
    for (at,status,outcome) in [(NOW-1,"completed","prepared"),(NOW-2,"failed","needs_attention"),
        (NOW-3,"running","pending"),(NOW-86400,"completed","prepared"),(NOW+1,"queued","prepared")] {
        jobs.push(json!({"id":format!("job-{at}"),"purpose":"auto_revalidate","kind":"assistant",
            "claimedAt":stamp(at),"status":status,"prepareOutcome":{"status":outcome,"private":"OMITTED_OUTCOME"},
            "prepareBundle":{"request":{"private":"OMITTED_PAID_BUNDLE".repeat(10000)}}}));
    }
    for (kind,purpose,status) in [("assistant","auto_prepare","running"),("assistant","discussion","queued"),
        ("assistant","custom","running"),("assistant","custom","completed"),("media","auto_media","running")] {
        jobs.push(json!({"kind":kind,"purpose":purpose,"status":status,"result":"OMITTED_MEDIA".repeat(10000)}));
    }
    jobs.push(json!({"purpose":"auto_revalidate","claimedAt":"invalid-date","status":"failed"}));
    jobs.push(json!({"purpose":"auto_revalidate","kind":"media","claimedAt":stamp(NOW-4),"status":"failed"}));
    d["jobs"]=json!(jobs);
    for field in ["feedback","operations","conversations","materials","knowledge_versions"]{
        d[field]=json!([{"private":"OMITTED_HISTORY".repeat(1000)}]);
    }
    d
}

#[tokio::test]
async fn preparation_summary_projection_matches_full_observed_counters_without_evidence(){
    let pool=sqlx::sqlite::SqlitePoolOptions::new().max_connections(1).connect("sqlite::memory:").await.unwrap();
    sqlx::query("CREATE TABLE workspace(id INTEGER PRIMARY KEY,payload TEXT NOT NULL)").execute(&pool).await.unwrap();
    let d=fixture();
    sqlx::query("INSERT INTO workspace VALUES(1,?)").bind(d.to_string()).execute(&pool).await.unwrap();
    let db=Database::Sqlite(pool);
    let projected=db.read_preparation_status_summary().await.unwrap();
    let mut full=crate::auto_prepare::status_view(&d,NOW,false);
    let mut summary=crate::auto_prepare::status_view(&projected,NOW,true);
    assert_eq!(summary["candidateCount"],Value::Null);
    assert_eq!(summary["groupReviewRequiredCount"],Value::Null);
    assert_eq!(summary["eligibility"],json!({"status":"not_evaluated","detailQuery":"eligibility=full"}));
    assert_eq!(summary["currentWorkflow"],json!({"prepared":1,"needsAttention":2,"stale":1}));
    assert_eq!(summary["activeJobs"],json!({"initialPreparation":1,"revalidation":2,"discussion":1,"otherPreparation":1}));
    assert_eq!(summary["revalidationLast24Hours"],json!({"claimed":5,"prepared":2,"needsAttention":1,"running":2,"failed":2}));
    assert_eq!(summary["configuration"],json!({"enabled":true,"debounceSeconds":3600}));
    assert_eq!(summary["publicFactFollowup"]["enabled"],true);
    // The cheap projection cannot attest the complete continuous backlog or
    // paid journal. Unknown remains null; the full fixture has observed zeros.
    assert_eq!(summary["continuousPreparation"]["backlog"],json!({"coverage":"unverified",
        "observedItems":null,"observedProposals":null,"openUnprepared":null}));
    assert_eq!(full["continuousPreparation"]["backlog"],json!({"coverage":"complete",
        "observedItems":6,"observedProposals":0,"openUnprepared":2}));
    for key in ["readyForOwnerApproval","held","waitingForOwnerApproval","issuedSlots","invocationPartition","observedTokens"] {
        assert_eq!(summary["continuousPreparation"][key],Value::Null,"partial {key} is unknown");
    }
    for key in ["readyForOwnerApproval","held"] {
        assert_eq!(full["continuousPreparation"][key],json!([]),"full {key} is observed empty");
    }
    for key in ["waitingForOwnerApproval","issuedSlots","observedTokens"] {
        assert_eq!(full["continuousPreparation"][key],json!(0),"full {key} is observed zero");
    }
    assert_eq!(full["continuousPreparation"]["invocationPartition"],json!({"unit":"issued_codex_process_slot",
        "reserved":0,"settled":0,"unknown":0,"observedNotInvoked":0,"refundAuthorized":false,"billableWireRequests":null}));
    assert_eq!(summary["continuousPreparation"]["tokenObservationIncomplete"],true);
    assert_eq!(full["continuousPreparation"]["tokenObservationIncomplete"],false);
    let mut summary_configuration=summary["continuousPreparation"].clone();
    let mut full_configuration=full["continuousPreparation"].clone();
    for key in ["backlog","readyForOwnerApproval","held","waitingForOwnerApproval","issuedSlots",
        "invocationPartition","observedTokens","tokenObservationIncomplete"] {
        summary_configuration.as_object_mut().unwrap().remove(key);
        full_configuration.as_object_mut().unwrap().remove(key);
    }
    assert_eq!(summary_configuration,full_configuration,"continuous configuration and dependency authority are coverage-independent");
    for key in ["candidateCount","groupReviewRequiredCount","eligibility","continuousPreparation"]{
        full.as_object_mut().unwrap().remove(key);summary.as_object_mut().unwrap().remove(key);
    }
    assert_eq!(summary,full,"all observed settings/counters/worker flags retain the existing contract");
    assert!(!projected.to_string().contains("OMITTED_"));
    assert!(projected.to_string().len()*100<d.to_string().len());
    for key in ["items","operations","proposals","posts","materials","feedback","knowledge_versions"]{assert!(projected.get(key).is_none());}
    db.close().await;
}

#[tokio::test]
async fn preparation_summary_preserves_effective_settings_defaults_and_invalid_claim_dates(){
    let pool=sqlx::sqlite::SqlitePoolOptions::new().max_connections(1).connect("sqlite::memory:").await.unwrap();
    sqlx::query("CREATE TABLE workspace(id INTEGER PRIMARY KEY,payload TEXT NOT NULL)").execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO workspace VALUES(1,'{}')").execute(&pool).await.unwrap();
    let db=Database::Sqlite(pool.clone());
    for settings in [Value::Null,json!({"autoPreparation":"legacy"}),json!({"autoPreparation":{"revalidation":false}}),
        json!({"autoPreparation":{"revalidation":{"enabled":"true","debounceSeconds":-2},"publicFactFollowup":{"version":1,"enabled":true,"extra":1}}})]{
        let mut d=crate::empty();
        d["settings"]=settings;d["items"]=json!([]);
        d["jobs"]=json!([{"purpose":"auto_revalidate","claimedAt":null,"status":"failed"}]);
        sqlx::query("UPDATE workspace SET payload=? WHERE id=1").bind(d.to_string()).execute(&pool).await.unwrap();
        let projected=db.read_preparation_status_summary().await.unwrap();
        let full=crate::auto_prepare::status_view(&d,NOW,false);
        let summary=crate::auto_prepare::status_view(&projected,NOW,true);
        for key in ["configuration","publicFactFollowup","activeJobs","claimedLast24Hours","revalidationLast24Hours","currentWorkflow"]{assert_eq!(summary[key],full[key],"{key}");}
    }
    db.close().await;
}

#[tokio::test]
#[ignore="requires a pristine explicitly isolated communityhero_writer_v51_test_ PostgreSQL fixture"]
async fn postgres_preparation_summary_parity_and_identity_guard(){
    // The existing harness checks loopback, exact named test database and absent
    // schema before migration. It never connects to an operational company DB.
    let db=crate::storage::writer_v51_fixture_db().await;
    // crate::empty() is a pre-storage document: migrated storage additionally
    // requires normalized knowledge collections. Preserve that pristine base.
    let mut d=db.read().await.unwrap();
    for (key,value) in fixture().as_object().unwrap(){d[key]=value.clone();}
    d["items"].as_array_mut().unwrap().retain(Value::is_object);
    for (n,job) in d["jobs"].as_array_mut().unwrap().iter_mut().enumerate(){job["id"]=json!(format!("job-{n}"));}
    for name in ["feedback","operations","conversations","materials","knowledge_versions"]{d[name]=json!([]);}
    d["operations"]=json!([{"id":"retained-unknown","itemId":"stale","status":"unknown",
        "evidence":{"providerCallAttempted":true,"mutationOutcome":"unknown","private":"OMITTED_UNKNOWN"}}]);
    d["feedback"]=json!([{"id":"retained-feedback","itemId":"stale","private":"OMITTED_FEEDBACK".repeat(1000)}]);
    crate::storage::validate(&d).expect("synthetic PG fixture must retain all normalized collections and references");
    db.change(|state|{*state=d;Ok(())}).await.unwrap();
    let before=db.read().await.unwrap();
    let projected=db.read_preparation_status_summary().await.unwrap();
    let mut expected=crate::auto_prepare::status_view(&before,NOW,false);
    let mut actual=crate::auto_prepare::status_view(&projected,NOW,true);
    assert!(actual["candidateCount"].is_null()&&actual["groupReviewRequiredCount"].is_null());
    assert_eq!(actual["eligibility"]["status"],"not_evaluated");
    for key in ["candidateCount","groupReviewRequiredCount","eligibility"]{
        expected.as_object_mut().unwrap().remove(key);actual.as_object_mut().unwrap().remove(key);
    }
    assert_eq!(actual,expected,"PostgreSQL must retain all observed response fields");
    assert!(!projected.to_string().contains("OMITTED_"));
    assert!(projected.to_string().len()*100<before.to_string().len());
    assert_eq!(db.read().await.unwrap(),before,"observation must not alter paid history or UNKNOWN");

    // Changed settings and a workflow edit appear in the next statement; public
    // fact opt-in with extra keys remains disabled under the exact predicate.
    db.change(|state|{
        state["settings"]["autoPreparation"]["revalidation"]=json!({"enabled":false,"debounceSeconds":30});
        state["settings"]["autoPreparation"]["publicFactFollowup"]=json!({"version":1,"enabled":true,"extra":1});
        state["items"][0]["workflow"]=json!("attention");Ok(())
    }).await.unwrap();
    let refreshed=db.read_preparation_status_summary().await.unwrap();
    let view=crate::auto_prepare::status_view(&refreshed,NOW,true);
    assert_eq!(view["configuration"],json!({"enabled":false,"debounceSeconds":30}));
    assert_eq!(view["publicFactFollowup"]["enabled"],false);
    assert_eq!(view["currentWorkflow"],json!({"prepared":0,"needsAttention":3,"stale":1}));
    let unchanged=db.read().await.unwrap();
    assert_eq!(unchanged["operations"],before["operations"]);assert_eq!(unchanged["jobs"],before["jobs"]);

    let Database::Postgres{writer,..}=&db else{unreachable!()};
    // A foreign company declaration cannot be accepted as the original account.
    // This tests the existing relational-account vs metadata-account guard,
    // not an invented cross-company permission supplied by the caller.
    sqlx::query("UPDATE communityhero.workspaces SET account='BAW Russia' WHERE id=$1")
        .bind(WORKSPACE).execute(writer).await.unwrap();
    let error=db.read_preparation_status_summary().await.unwrap_err();
    assert_eq!(error.1,"Workspace identity mismatch");
    sqlx::query("UPDATE communityhero.workspaces SET account='LikeAvto' WHERE id=$1")
        .bind(WORKSPACE).execute(writer).await.unwrap();
    assert_eq!(db.read_preparation_status_summary().await.unwrap(),refreshed);
    assert_eq!(db.read().await.unwrap(),unchanged,"guard observation must leave source/ledger unchanged");
    db.close().await;
}
