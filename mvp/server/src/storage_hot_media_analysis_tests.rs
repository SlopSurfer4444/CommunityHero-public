//! Connected tests for immutable media evidence in bounded admission readers.
//! The structural fixture is used only for byte preservation. Consumer tests
//! use Media's real local CAS fixture, with exactly its own cache pin evicted.
use super::*;

const AT:&str="2026-10-04T08:00:00Z";

fn carrier_rows(d:&Value)->Vec<Value> {
    crate::list(d,"jobs").iter().filter(|j|matches!(j["kind"].as_str(),
        Some("media_analysis"|"media_analysis_applicability"))).cloned().collect()
}

#[test]
fn admission_media_carriers_preserve_unknown_and_proofs_without_unrelated_history() {
    for account in [crate::accounts::Profile::LikeAvto,crate::accounts::Profile::BawRussia] {
        let mut d=crate::empty();normalize(&mut d);crate::accounts::initialize(&mut d,account).unwrap();
        let media=crate::media_analysis::test_workspace_fixture(account.display());
        d["jobs"]=media["jobs"].clone();
        crate::list_mut(&mut d,"jobs").push(json!({"id":"cold-unrelated","kind":"sync","status":"completed",
            "result":{"oldPayload":"retained-history".repeat(1000)}}));
        let expected=carrier_rows(&d);assert_eq!(expected.len(),3);
        assert!(expected.iter().any(|job|job.to_string().contains("unknown")));
        let body=json!({"proposals":[],"expected":[]});
        for scope in [AdmissionScope::EditorialSchedule(&body),AdmissionScope::OperatorEditorial(&body),
            AdmissionScope::EditorialJob("editorial-root"),AdmissionScope::EditorialRepair{job:"editorial-root",body:&body},
            AdmissionScope::Approval(&body),AdmissionScope::Execute{approval:"approval-root",body:&body}] {
            let before=project(&d,scope).unwrap();
            assert_eq!(carrier_rows(&before),expected,"full immutable analysis and applicability payloads");
            assert!(crate::row(&before,"jobs","cold-unrelated").is_err());
            crate::media_analysis::ledger_from_workspace(&before).unwrap();
            for field in ["analysis","result","account","kind"] {
                let mut changed=before.clone();changed["jobs"][0][field]=json!("forged");
                assert!(validate_delta(&before,&changed,scope).is_err(),"readonly carrier field {field}");
            }
        }
        let receipts=project(&d,AdmissionScope::ExecutionReceipts("absent-execute")).unwrap();
        assert!(carrier_rows(&receipts).is_empty(),"receipt refresh has no knowledge consumer or source projection");
        assert_eq!(carrier_rows(&d),expected,"projection cannot settle UNKNOWN or rewrite a proof");
    }
}

fn assert_reused_audio(view:&Value,ready:bool) {
    let target=crate::row(view,"posts","target").unwrap();
    let strict=crate::knowledge::TranscriptLookup::new(view,AT).unwrap().strict_media_evidence(target).unwrap();
    assert_eq!(strict["audioReady"],ready);
    assert_eq!(strict["screenTextReady"],false,"donor OCR is not target screen evidence");
    assert_eq!(strict["visualReady"],false);
    let selected=crate::knowledge::select(view,&[],std::slice::from_ref(target),AT).unwrap();
    let reused=crate::list(&selected,"materials").iter().any(|material|
        material["postKey"]=="vk:donor"&&material["exactFileAnalysisReuse"].as_array().into_iter().flatten()
            .any(|proof|proof["targetPostId"]=="target"));
    assert_eq!(reused,ready,"knowledge consumes durable ledger plus target applicability, not a cloned transcript");
}

async fn exercise_cold_consumer(db:&Database)->Value {
    let mut initial=crate::media_analysis_reuse::test_cold_reused_workspace();normalize(&mut initial);
    crate::list_mut(&mut initial,"jobs").push(json!({"id":"cold-unrelated","kind":"sync","status":"completed",
        "result":{"oldPayload":"retained-history".repeat(1000)}}));
    db.change(|d|{*d=initial;Ok(())}).await.unwrap();
    let full=db.read().await.unwrap();let body=json!({"proposals":[]});
    let view=db.read_operator_editorial(&body).await.unwrap();
    assert_eq!(view,project(&full,AdmissionScope::OperatorEditorial(&body)).unwrap());
    assert_eq!(carrier_rows(&view),carrier_rows(&full));
    assert!(crate::row(&view,"jobs","cold-unrelated").is_err());
    assert_reused_audio(&view,false);
    crate::media_analysis_reuse::warm_workspace(&view).unwrap();
    assert_reused_audio(&view,true);
    assert_eq!(db.read().await.unwrap(),full,"reading and cache warming do not alter paid ledger or applicability");
    let key=carrier_rows(&view)[0]["id"].as_str().unwrap().to_owned();
    let mutation=db.change_admission_observed(AdmissionScope::Approval(&body),|d|{
        crate::row_mut(d,"jobs",&key)?["analysis"]=json!({"forged":true});Ok(())
    }).await;
    assert!(mutation.is_err(),"new readonly evidence cannot be edited by approval admission");
    assert_eq!(db.read().await.unwrap(),full,"failed admission rolls back all durable evidence");
    full
}

#[tokio::test]
async fn sqlite_operator_reader_cold_media_reuse_reaches_knowledge_and_preserves_ledger() {
    let (app,_temp)=crate::tests::test_app().await;
    exercise_cold_consumer(&app.db).await;
    app.db.close().await;
}

#[tokio::test]
#[ignore="requires a fresh isolated communityhero_writer_v51_test_ PostgreSQL fixture; run selector alone"]
async fn postgres_operator_reader_cold_media_reuse_and_corrupt_discriminators() {
    let db=super::super::preparation::writer_v51_fixture_db().await;
    let full=exercise_cold_consumer(&db).await;
    let body=json!({"proposals":[]});let expected=project(&full,AdmissionScope::OperatorEditorial(&body)).unwrap();
    let Database::Postgres{writer,..}=&db else {unreachable!()};
    let media=carrier_rows(&full);let id=media[0]["id"].as_str().unwrap();
    assert_eq!(media[0]["status"],"ledger","completed analysis is carried by a non-runnable ledger job");
    // Either physical or payload kind must retain this row so relational
    // corruption is rejected rather than silently becoming absent evidence.
    for query in [
        "UPDATE communityhero.jobs SET kind='sync' WHERE workspace_id=$1 AND id=$2",
        "UPDATE communityhero.jobs SET payload=jsonb_set(payload,'{kind}','\"sync\"'::jsonb) WHERE workspace_id=$1 AND id=$2",
        "UPDATE communityhero.jobs SET status='completed' WHERE workspace_id=$1 AND id=$2",
        "UPDATE communityhero.jobs SET payload=jsonb_set(payload,'{id}','\"forged-analysis-id\"'::jsonb) WHERE workspace_id=$1 AND id=$2",
    ] {
        let mut tx=writer.begin().await.unwrap();
        assert_eq!(sqlx::query(sqlx::AssertSqlSafe(query)).bind(WORKSPACE).bind(id)
            .execute(&mut *tx).await.unwrap().rows_affected(),1);
        assert!(load_scope(&mut tx,AdmissionScope::OperatorEditorial(&body),false).await.is_err());
        tx.rollback().await.unwrap();
        assert_eq!(db.read_operator_editorial(&body).await.unwrap(),expected,"corruption fixture rollback restores exact snapshot");
    }
    assert_eq!(db.read().await.unwrap(),full);
    db.close().await;
}
