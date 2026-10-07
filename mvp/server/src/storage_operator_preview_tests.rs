use super::*;
use std::time::Duration;

/// The existing fixture guard permits only a new, explicitly named loopback
/// database. No provider/model work occurs. One reader makes retained pool
/// ownership observable without a timing benchmark or a huge JSON allocation.
#[tokio::test]
#[ignore = "requires a fresh isolated PostgreSQL fixture; run this selector alone"]
async fn postgres_operator_preview_releases_reader_before_decode_and_keeps_snapshot() {
    let fixture=super::super::preparation::writer_v51_fixture_db().await;
    let Database::Postgres{writer,reader:original_reader}=fixture else {unreachable!()};
    let options=(*original_reader.connect_options()).clone();
    original_reader.close().await;
    let reader=PgPoolOptions::new().max_connections(1).min_connections(1)
        .after_connect(|connection,_|Box::pin(async move {
            sqlx::query("SET default_transaction_read_only = on").execute(connection).await?;Ok(())
        })).connect_with(options).await.unwrap();
    let db=Database::Postgres{writer,reader};
    let (mut initial,refs)=crate::operator_editorial::tests::fixture();
    normalize(&mut initial);
    crate::list_mut(&mut initial,"jobs").push(json!({"id":"observer-job","kind":"sync","refId":"","status":"completed",
        "result":{"observations":59,"immutable":"exact durable status"}}));
    db.change(|d|{*d=initial;Ok(())}).await.unwrap();
    let baseline=db.read().await.unwrap();let body=json!({"proposals":refs});
    let expected=project(&baseline,AdmissionScope::OperatorEditorial(&body)).unwrap();
    let Database::Postgres{reader,..}=&db else {unreachable!()};
    // This is the same production capture stage. Delay its decode indefinitely
    // by retaining the owned rows: a status observation must still acquire the
    // sole reader, rather than depend on the preview's JSON/provenance work.
    let captured=capture_operator_preview(reader,&body).await.unwrap();
    let status=tokio::time::timeout(Duration::from_secs(5),db.read_job_public("observer-job"))
        .await.expect("preview capture retained the sole reader").unwrap().unwrap();
    assert_eq!(status["status"],"completed");assert_eq!(status["result"]["observations"],59);
    // A later source/control commit cannot mix with previously captured rows.
    db.change(|d|{
        crate::row_mut(d,"posts","post")?["text"]=json!("Changed after preview capture");
        let target=crate::row(d,"items","i0")?.clone();
        crate::list_mut(d,"operations").push(json!({"id":"late-unknown","itemId":"i0","status":"unknown","target":target}));
        Ok(())
    }).await.unwrap();
    assert_eq!(captured.decode().unwrap(),expected,"captured snapshot remains coherent after release");
    let current=db.read_operator_editorial(&body).await.unwrap();
    assert_eq!(current["posts"][0]["text"],"Changed after preview capture");
    assert!(crate::row(&current,"operations","late-unknown").is_ok());
    assert!(crate::operator_editorial::capture(&current,&crate::operator_editorial::tests::actor(),&body).is_err(),
        "the current UNKNOWN still blocks actual review");
    // Decode-time identity errors must not become an empty/allowed scope.
    let Database::Postgres{writer,..}=&db else {unreachable!()};
    sqlx::query("UPDATE communityhero.posts SET payload=jsonb_set(payload,'{id}','\"forged-post\"'::jsonb) WHERE workspace_id=$1 AND id='post'")
        .bind(WORKSPACE).execute(writer).await.unwrap();
    let captured=capture_operator_preview(reader,&body).await.unwrap();
    assert!(captured.decode().is_err(),"corrupted captured identity is rejected after pool release");
    let status=tokio::time::timeout(Duration::from_secs(5),db.read_job_public("observer-job"))
        .await.expect("failed decode retained the sole reader").unwrap().unwrap();
    assert_eq!(status["status"],"completed");
    db.close().await;
}
