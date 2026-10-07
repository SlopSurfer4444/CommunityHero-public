//! Ignored test: only the existing pristine loopback synthetic PG guard may seed it.
use crate::*;

#[tokio::test]
#[ignore="requires a pristine explicitly isolated communityhero_writer_v51_test_ PostgreSQL fixture"]
async fn postgres_post_network_receipt_survives_late_outcome_sql_failure_and_reopen() {
    let url=std::env::var("COMMUNITYHERO_WRITER_V51_TEST_URL").expect("explicit isolated fixture URL");
    let db=super::preparation::writer_v51_fixture_db().await;
    let (mut app,_temp,baseline,op)=crate::post_network_transition_tests::fixture().await;
    app.db.close().await;app.db=db;
    app.db.change(|data|{*data=baseline.clone();Ok(())}).await.unwrap();
    let before=app.db.read().await.unwrap();
    let Database::Postgres{writer,..}=&app.db else {unreachable!()};
    // Outcome saves operation/proposal/item/feedback before its final audit INSERT.
    // This trigger is installed solely in the freshly guarded synthetic DB.
    // nextval is not rolled back: its sole caller is this final-audit trigger.
    // ApiError correctly sanitizes database errors, so assert this explicit
    // synthetic witness rather than mistaking an earlier guard error for it.
    sqlx::query("CREATE SEQUENCE communityhero.post_network_late_fault_seen").execute(writer).await.unwrap();
    sqlx::query("CREATE FUNCTION pg_temp.post_network_reject_audit() RETURNS trigger LANGUAGE plpgsql AS 'BEGIN PERFORM nextval(''communityhero.post_network_late_fault_seen''); RAISE EXCEPTION ''synthetic late outcome audit failure''; END;'")
        .execute(writer).await.unwrap();
    sqlx::query("CREATE TRIGGER post_network_reject_audit BEFORE INSERT ON communityhero.audit FOR EACH ROW EXECUTE FUNCTION pg_temp.post_network_reject_audit()")
        .execute(writer).await.unwrap();
    let receipt=json!({"synthetic":"provider-returned-before-outcome-failure"});
    let mut notifications=app.events.subscribe();let version=app.bootstrap_cache.current_version();
    let result=app.change_execute_transition(&op,receipt.clone(),"unknown",|data|{
        assert_eq!(data["operations"][0]["executeReceipt"],receipt);
        assert_ne!(app.bootstrap_cache.current_version(),version);
        assert!(notifications.try_recv().is_ok());
        apply_operation_outcome(data,&op,"unknown",json!({"providerRetryAllowed":false}))
    }).await;
    assert!(result.is_err());assert!(notifications.try_recv().is_err());
    let called:bool=sqlx::query_scalar("SELECT is_called FROM communityhero.post_network_late_fault_seen").fetch_one(writer).await.unwrap();
    let visits:i64=sqlx::query_scalar("SELECT last_value FROM communityhero.post_network_late_fault_seen").fetch_one(writer).await.unwrap();
    assert!(called,"the late audit trigger must actually execute; earlier errors are not equivalent");
    assert_eq!(visits,1,"exactly one final audit insertion was attempted");
    let permit=app.gate.acquire(writer_gate::Class::Standard).await;drop(permit);
    app.db.close().await;
    // Reopen this exact already-guarded synthetic fixture; never reseed/reuse another DB.
    let reopened=Database::postgres(&url).await.unwrap_or_else(|_|panic!("synthetic fixture reopen failed"));
    let mut expected=before;expected["operations"][0]["executeReceipt"]=receipt;
    assert_eq!(reopened.read().await.unwrap(),expected,"receipt persists; all partial outcome writes/audit/feedback roll back");
    reopened.close().await;
    println!("post_network_pg_late_failure_receipt_retained_after_reopen_passed");
}
