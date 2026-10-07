use super::*;

fn fixture()->Value{
    let mut data=crate::empty();normalize(&mut data);
    data["branches"]=json!([{"id":"b","messages":[]}]);
    data["items"]=json!([{"id":"i","branchId":"b","revision":1,"platform":"VK","draft":"retained human draft"},
        {"id":"foreign","revision":1,"platform":"VK"}]);
    data["proposals"]=json!([{"id":"p","itemId":"i","revision":1,"kind":"reply_and_close","status":"draft","text":"Original",
        "custom":{"keep":true},"editorialReview":{"proof":"immutable"}},
        {"id":"other","itemId":"foreign","revision":1,"kind":"close","status":"unknown","text":"Unknown must remain"}]);
    data["feedback"]=json!([{"id":"foreign-event","itemId":"foreign","kind":"draft_saved","requestHash":"other-request","accountId":"LikeAvto"}]);
    data["jobs"]=json!([{"id":"cold","kind":"assistant","status":"completed","result":{"private":"C".repeat(100_000)}}]);
    data["conversations"]=json!([{"id":"private","messages":[{"text":"unrelated".repeat(10_000)}]}]);
    data["operations"]=json!([{"id":"unknown","status":"unknown","evidence":{"uncertain":"retained"}}]);
    data
}
fn request(revision:u64,event:&str)->Value{json!({"expectedRevision":revision,"text":"Saved synthetic edit","eventId":event})}
fn normalized(mut value:Value)->Value{
    fn walk(value:&mut Value){match value{Value::Object(fields)=>for(key,value)in fields{if key=="createdAt"{*value=json!("<time>")}else{walk(value)}},Value::Array(values)=>values.iter_mut().for_each(walk),_=>()}}
    walk(&mut value);value
}

#[test]
fn proposal_edit_scope_keeps_only_dependencies_and_rejects_authority_changes(){
    let full=fixture();let body=request(1,"saved-event");let before=project(&full,"p",&body).unwrap();
    assert!(before.get("jobs").is_none()&&before.get("operations").is_none());
    assert_eq!(before["items"].as_array().unwrap().len(),1);assert!(before["feedback"].as_array().unwrap().is_empty());
    assert!(before.to_string().len()*50<full.to_string().len());
    let mut after=before.clone();crate::edit_proposal(&mut after,"p",&body).unwrap();validate_edit(&before,&after,"p",&body).unwrap();
    for mutation in ["account","item","revision","protected","origin","history","feedback","extra"]{
        let mut forged=after.clone();match mutation{
            "account"=>forged["account"]=json!("BAW"),"item"=>forged["items"][0]["draft"]=json!("overwrite"),
            "revision"=>forged["proposals"][0]["revision"]=json!(3),"protected"=>forged["proposals"][0]["custom"]=json!({}),
            "origin"=>forged["proposals"][0]["origin"]["text"]=json!("rewritten"),"history"=>forged["proposals"][0]["history"]=json!([]),
            "feedback"=>forged["feedback"][0]["itemId"]=json!("foreign"),_=>forged["proposals"].as_array_mut().unwrap().push(json!({"id":"extra"})),
        }
        assert!(validate_edit(&before,&forged,"p",&body).is_err(),"{mutation}");
    }
    let collision=request(1,"foreign-event");let projected=project(&full,"p",&collision).unwrap();
    assert_eq!(projected["feedback"],full["feedback"],"global collision must remain visible");
}

async fn exercise_contract(db:&Database){
    db.change(|d|{*d=fixture();Ok(())}).await.unwrap();
    let runtime=crate::native_fixture_owner_repair::initialize_db(db).await.unwrap();
    let before=db.read().await.unwrap();
    let body=request(1,"saved-event");let mut expected=before.clone();crate::edit_proposal(&mut expected,"p",&body).unwrap();
    let(value,changed)=db.edit_proposal_observed("p",&body,&runtime).await.unwrap();assert!(changed);assert_eq!(value["revision"],2);
    let saved=db.read().await.unwrap();assert_eq!(normalized(saved.clone()),normalized(expected));
    let(replay,changed)=db.edit_proposal_observed("p",&body,&runtime).await.unwrap();assert!(!changed);assert_eq!(replay,value);assert_eq!(db.read().await.unwrap(),saved);
    for invalid in [request(1,"stale-event"),request(2,"foreign-event"),json!({"expectedRevision":2,"text":"","eventId":"empty"}),json!({"expectedRevision":2,"text":"Different","eventId":"saved-event"})]{
        assert!(db.edit_proposal_observed("p",&invalid,&runtime).await.is_err());assert_eq!(db.read().await.unwrap(),saved);
    }
    for status in ["dispatching","unknown","succeeded"]{
        db.change(|d|{crate::row_mut(d,"proposals","p")?["status"]=json!(status);Ok(())}).await.unwrap();
        let held=db.read().await.unwrap();assert!(db.edit_proposal_observed("p",&request(2,"blocked"),&runtime).await.is_err());assert_eq!(db.read().await.unwrap(),held);
    }
    db.change(|d|{crate::row_mut(d,"proposals","p")?["status"]=json!("draft");Ok(())}).await.unwrap();
    let one=request(2,"parallel-one");let two=request(2,"parallel-two");
    let(a,b)=tokio::join!(db.edit_proposal_observed("p",&one,&runtime),db.edit_proposal_observed("p",&two,&runtime));
    assert_eq!(usize::from(a.is_ok())+usize::from(b.is_ok()),1,"exactly one stale-revision contender may commit");
    assert_eq!(crate::row(&db.read().await.unwrap(),"proposals","p").unwrap()["revision"],3);
}

#[tokio::test]
async fn sqlite_proposal_edit_matches_complete_domain_and_preserves_replay_cas_unknown(){
    let folder=tempfile::tempdir().unwrap();let db=Database::Sqlite(crate::open_db(&folder.path().join("fixture.sqlite")).await.unwrap());
    exercise_contract(&db).await;db.close().await;
}

#[tokio::test]
#[ignore="requires a pristine explicitly isolated communityhero_writer_v51_test_ PostgreSQL fixture"]
async fn postgres_proposal_edit_atomic_replay_cas_and_projection_guards(){
    let db=super::super::preparation::writer_v51_fixture_db().await;
    // The optional performance upgrade must retain the full reducer contract.
    let Database::Postgres{writer,..}=&db else{unreachable!()};
    for _ in 0..2{
        sqlx::raw_sql(include_str!("../migrations/0004_feedback_payload_id.sql")).execute(writer).await.unwrap();
    }
    let migration_count:i64=sqlx::query_scalar("SELECT count(*) FROM communityhero.schema_migrations WHERE version=4").fetch_one(writer).await.unwrap();
    assert_eq!(migration_count,1,"explicit reapplication remains idempotent");
    let index_valid:bool=sqlx::query_scalar("SELECT indisvalid AND NOT indisunique FROM pg_index WHERE indexrelid='communityhero.feedback_payload_id_idx'::regclass").fetch_one(writer).await.unwrap();
    assert!(index_valid,"aliases must remain visible through a nonunique index");
    exercise_contract(&db).await;
    let runtime=crate::runtime_lifecycle_startup::initialize_db_fixture(&db).await.unwrap();
    let original=db.read().await.unwrap();
    let Database::Postgres{writer,..}=&db else{unreachable!()};
    sqlx::query("CREATE FUNCTION pg_temp.edit_reject() RETURNS trigger LANGUAGE plpgsql AS 'BEGIN RAISE EXCEPTION ''synthetic late feedback rejection''; END;'").execute(writer).await.unwrap();
    sqlx::query("CREATE TRIGGER edit_reject BEFORE INSERT ON communityhero.feedback FOR EACH ROW EXECUTE FUNCTION pg_temp.edit_reject()").execute(writer).await.unwrap();
    assert!(db.edit_proposal_observed("p",&request(3,"late-failure"),&runtime).await.is_err());
    sqlx::query("DROP TRIGGER edit_reject ON communityhero.feedback").execute(writer).await.unwrap();
    assert_eq!(db.read().await.unwrap(),original,"feedback failure rolls back target proposal and history");
    sqlx::query("UPDATE communityhero.proposals SET item_id='foreign' WHERE id='p'").execute(writer).await.unwrap();
    assert!(db.edit_proposal_observed("p",&request(3,"bad-projection"),&runtime).await.is_err());
    sqlx::query("UPDATE communityhero.proposals SET item_id='i' WHERE id='p'").execute(writer).await.unwrap();
    sqlx::query("UPDATE communityhero.workspaces SET account='BAW'").execute(writer).await.unwrap();
    assert!(db.edit_proposal_observed("p",&request(3,"bad-account"),&runtime).await.is_err());
    sqlx::query("UPDATE communityhero.workspaces SET account='LikeAvto'").execute(writer).await.unwrap();
    assert_eq!(db.read().await.unwrap(),original);
    let mut tx=writer.begin().await.unwrap();let before=load(&mut tx,"p",&request(3,"cost-probe")).await.unwrap();tx.rollback().await.unwrap();
    println!("PROPOSAL_EDIT_PG {}",json!({"atomicLateFeedbackRollback":true,"replay":true,"crossItemCollision":true,"cas":true,"unknownProtected":true,"relationalMismatchRejected":true,"workspaceMismatchRejected":true,"fullBytes":original.to_string().len(),"scopedBytes":before.to_string().len(),"loadStatements":3}));
    // Hostile persisted alias: searching only the primary key would hide this
    // corrupt foreign-item event and incorrectly permit a new save. The index
    // must keep the existing OR lookup/refusal, even alongside a valid replay.
    for (relational_id,payload_id) in [("payload-alias-new","alias-event"),("payload-alias-replay","saved-event")]{
        sqlx::query("INSERT INTO communityhero.feedback(workspace_id,id,item_id,ordinal,payload) SELECT $1,$2,'foreign',COALESCE(MAX(ordinal),-1)+1,jsonb_build_object('id',$3::text,'itemId','foreign','kind','draft_saved') FROM communityhero.feedback WHERE workspace_id=$1")
            .bind(WORKSPACE).bind(relational_id).bind(payload_id).execute(writer).await.unwrap();
    }
    let snapshot=sqlx::query_scalar::<_,String>("SELECT jsonb_build_object('proposal',(SELECT payload FROM communityhero.proposals WHERE workspace_id=$1 AND id='p'),'feedback',(SELECT jsonb_agg(to_jsonb(f) ORDER BY ordinal) FROM communityhero.feedback f WHERE workspace_id=$1))::text")
        .bind(WORKSPACE).fetch_one(writer).await.unwrap();
    for body in [request(3,"alias-event"),request(1,"saved-event")]{
        let error=db.edit_proposal_observed("p",&body,&runtime).await.unwrap_err();
        assert_eq!(error.1,"Proposal feedback identity mismatch","corrupt aliases must reject before save or replay");
        let unchanged=sqlx::query_scalar::<_,String>("SELECT jsonb_build_object('proposal',(SELECT payload FROM communityhero.proposals WHERE workspace_id=$1 AND id='p'),'feedback',(SELECT jsonb_agg(to_jsonb(f) ORDER BY ordinal) FROM communityhero.feedback f WHERE workspace_id=$1))::text")
            .bind(WORKSPACE).fetch_one(writer).await.unwrap();
        assert_eq!(unchanged,snapshot,"alias refusal preserves proposal revision/history and every feedback row");
    }
    db.close().await;
}
