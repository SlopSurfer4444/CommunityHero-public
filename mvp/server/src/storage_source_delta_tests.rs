use super::*;
use std::{cell::Cell,rc::Rc};

fn posts(count:usize,bytes:usize)->Vec<Value> {
    (0..count).map(|n|json!({"id":format!("p{n}"),"text":"x".repeat(bytes)})).collect()
}
#[test]
fn source_delta_borrows_only_changed_rows_and_keeps_global_ordinals() {
    let old=posts(260,4096);let mut new=old.clone();
    new[1]["text"]=json!("changed");new[259]["text"]=json!("last");
    new.push(json!({"id":"p260","text":"appended"}));
    let updates=DeltaRows::new(&old,&new,0,Mode::Update).collect::<Vec<_>>();
    assert_eq!(updates.iter().map(|(n,_)|*n).collect::<Vec<_>>(),vec![1,259]);
    assert!(std::ptr::eq(updates[0].1,&new[1]));assert!(std::ptr::eq(updates[1].1,&new[259]));
    let insert=Batches::new(DeltaRows::new(&old,&new,0,Mode::Insert)).next().unwrap().unwrap();
    assert_eq!(insert.ids,vec!["p260"]);assert_eq!(insert.ordinals,vec![260]);
    let changed=Batches::new(DeltaRows::new(&old,&new,0,Mode::Update)).next().unwrap().unwrap();
    assert_eq!(changed.ids,vec!["p1","p259"]);assert!(changed.bytes<200);
    assert!(Batches::new(DeltaRows::new(&old,&old,0,Mode::Update)).next().is_none());
    assert!(Batches::new(DeltaRows::new(&old,&old,0,Mode::Insert)).next().is_none());
}
#[test]
fn source_delta_row_cap_and_audit_append_offset_are_exact() {
    let new=posts(260,1);
    let batches=Batches::new(DeltaRows::new(&[],&new,37,Mode::Insert))
        .map(Result::unwrap).collect::<Vec<_>>();
    assert_eq!(batches.iter().map(Batch::len).collect::<Vec<_>>(),vec![128,128,4]);
    assert_eq!(batches.iter().flat_map(|b|b.ordinals.iter().copied()).collect::<Vec<_>>(),(37..297).collect::<Vec<_>>());
    assert!(batches.iter().all(|b|b.bytes<=MAX_BYTES));
}
#[test]
fn source_delta_byte_cap_and_single_oversized_row_are_bounded() {
    let mut new=posts(2,600*1024);new.push(json!({"id":"large","text":"y".repeat(MAX_BYTES*2)}));
    new.push(json!({"id":"small","text":"tail"}));
    let batches=Batches::new(DeltaRows::new(&[],&new,0,Mode::Insert))
        .map(Result::unwrap).collect::<Vec<_>>();
    assert_eq!(batches.iter().map(Batch::len).collect::<Vec<_>>(),vec![1,1,1,1]);
    assert_eq!(batches.iter().flat_map(|b|b.ids.iter().copied()).collect::<Vec<_>>(),vec!["p0","p1","large","small"]);
    assert!(batches.iter().all(|b|b.bytes<=MAX_BYTES||b.len()==1));
    assert!(batches[2].bytes>MAX_BYTES);
}
#[test]
fn source_delta_backpressure_never_serializes_a_full_change_set() {
    let values=posts(400,1);let visited=Rc::new(Cell::new(0));let seen=visited.clone();
    let rows=values.iter().enumerate().map(move |(n,value)|{seen.set(seen.get()+1);(n as i64,value)});
    let mut batches=Batches::new(rows);
    assert_eq!(batches.next().unwrap().unwrap().len(),128);assert_eq!(visited.get(),128);
    assert_eq!(batches.next().unwrap().unwrap().len(),128);assert_eq!(visited.get(),256);
    // Byte overflow permits exactly one serialized lookahead, reused next time.
    let values=posts(5,600*1024);let visited=Rc::new(Cell::new(0));let seen=visited.clone();
    let rows=values.iter().enumerate().map(move |(n,value)|{seen.set(seen.get()+1);(n as i64,value)});
    let mut batches=Batches::new(rows);
    assert_eq!(batches.next().unwrap().unwrap().len(),1);assert_eq!(visited.get(),2);
    assert_eq!(batches.next().unwrap().unwrap().len(),1);assert_eq!(visited.get(),3);
}
#[test]
fn source_delta_invalid_identity_and_ordinal_fail_without_retry() {
    for (ordinal,value) in [(-1,json!({"id":"p"})),(i64::from(i32::MAX)+1,json!({"id":"p"})),
        (0,json!({"id":7})),(0,json!({"text":"missing id"}))] {
        let mut batches=Batches::new(std::iter::once((ordinal,&value)));
        assert!(batches.next().unwrap().is_err());assert!(batches.next().is_none());
    }
    let values=posts(2,0);
    let mut batches=Batches::new(DeltaRows::new(&[],&values,i64::from(i32::MAX),Mode::Insert));
    assert!(batches.next().unwrap().is_err(),"audit append overflow aborts the caller transaction");
}
#[test]
fn source_delta_sql_uses_closed_tables_exact_update_guard_and_no_upsert() {
    assert_eq!(WRITE_TABLES.map(Table::name),["posts","branches","items","proposals","audit"]);
    for table in WRITE_TABLES {
        let insert=statement(table,Mode::Insert).unwrap();
        assert!(insert.contains("SELECT $1,d.id,d.ordinal,d.payload"));
        assert!(insert.contains("ORDER BY d.ordinal"));assert!(!insert.contains("ON CONFLICT"));
        if table==Table::Audit { assert!(statement(table,Mode::Update).is_err());continue; }
        let update=statement(table,Mode::Update).unwrap();
        assert!(update.contains("target.workspace_id=$1 AND target.id=d.id AND target.ordinal=d.ordinal"));
        assert!(!update.contains("INSERT"));
        for (_,key) in table.columns() {
            assert!(update.contains(&string_projection(key)));assert!(insert.contains(&string_projection(key)));
        }
    }
}

fn fixture()->Value {
    let mut d=crate::empty();normalize(&mut d);
    d["posts"]=Value::Array(posts(260,8));
    d["branches"]=json!([{"id":"b0","postId":"p0","messages":[]}]);
    d["items"]=json!([{"id":"i0","postId":"p0","branchId":"b0","draft":"operator","draftEdited":true,"providerStatus":"new"}]);
    d["proposals"]=json!([{"id":"q0","itemId":"i0","status":"approved","text":"approved retained"}]);
    d["approvals"]=json!([{"id":"a0","status":"approved","proposals":[{"id":"q0","proposal":{"text":"immutable approved"},"item":{"draft":"operator"}}]}]);
    d["operations"]=json!([{"id":"o0","itemId":"i0","proposalId":"q0","approvalId":"a0","status":"unknown","receipt":{"keep":true}}]);
    d["jobs"]=json!([{"id":"paid","kind":"assistant","status":"completed","result":{"paid":"x".repeat(256*1024)}}]);
    d["conversations"]=json!([{"id":"c0","itemIds":["i0"],"messages":[{"text":"private"}]}]);
    d["feedback"]=json!([{"id":"f0","itemId":"i0","text":"private feedback"}]);
    d["audit"]=json!([{"id":"audit0","action":"history","refId":"i0"},{"id":"audit1","action":"history","refId":null}]);
    validate(&d).unwrap();d
}
fn changes(before:&Value)->Value {
    let mut after=before.clone();
    for post in after["posts"].as_array_mut().unwrap() {post["text"]=json!("source changed");}
    after["posts"].as_array_mut().unwrap().extend((0..130).map(|n|json!({"id":format!("new-p{n}"),"text":"new"})));
    after["branches"][0]["messages"]=json!([{"text":"new context"}]);
    after["branches"].as_array_mut().unwrap().push(json!({"id":"b1","postId":"new-p0","messages":[]}));
    after["items"][0]["providerStatus"]=json!("closed");
    after["items"].as_array_mut().unwrap().push(json!({"id":"i1","postId":"new-p0","branchId":"b1"}));
    after["proposals"][0]["sourceObserved"]=json!(true);
    after["proposals"].as_array_mut().unwrap().push(json!({"id":"q1","itemId":"i1","status":null,"text":"new proposal"}));
    after["audit"]=json!([{"id":"audit2","action":"source.observed","refId":"i1"},{"id":"audit3","action":null}]);
    after["sync"]["sourceDeltaFixture"]=json!("admitted");
    validate_change(before,&after).unwrap();after
}

#[tokio::test]
#[ignore="ROOT only: requires pristine explicit isolated communityhero_writer_v51_test_ PostgreSQL fixture"]
async fn postgres_source_delta_connected_parity_bound_and_late_failures() {
    // The standard loopback/name/pristine-schema guard is the only DB entrance.
    let db=crate::storage::preparation::writer_v51_fixture_db().await;
    let initial=fixture();db.change(|d|{*d=initial.clone();Ok(())}).await.unwrap();
    let baseline=db.read().await.unwrap();let before=project(&baseline).unwrap();let after=changes(&before);
    let Database::Postgres{writer,..}=&db else {unreachable!()};
    // Exact expression called by both generated INSERT and UPDATE statements.
    for value in [json!({}),json!({"status":null}),json!({"status":true}),json!({"status":123}),
        json!({"status":[]}),json!({"status":{"nested":1}}),json!({"status":""}),json!({"status":"approved"})] {
        let sql=format!("SELECT {} AS projected FROM (SELECT $1::jsonb AS payload) d",string_projection("status"));
        let actual:Option<String>=sqlx::query_scalar(sqlx::AssertSqlSafe(sql.as_str())).bind(value.to_string()).fetch_one(writer).await.unwrap();
        assert_eq!(actual.as_deref(),value["status"].as_str(),"Rust as_str SQL parity");
    }
    // Every physical DML statement is counted, not inferred from helper calls.
    sqlx::query("CREATE TEMP TABLE source_delta_trace(table_name text,op text)").execute(writer).await.unwrap();
    sqlx::query("CREATE FUNCTION pg_temp.source_delta_count() RETURNS trigger LANGUAGE plpgsql AS 'BEGIN INSERT INTO pg_temp.source_delta_trace VALUES(TG_TABLE_NAME,TG_OP); RETURN NULL; END;'").execute(writer).await.unwrap();
    for table in WRITE_TABLES {
        let sql=format!("CREATE TRIGGER source_delta_count AFTER INSERT OR UPDATE ON communityhero.{} FOR EACH STATEMENT EXECUTE FUNCTION pg_temp.source_delta_count()",table.name());
        sqlx::query(sqlx::AssertSqlSafe(sql.as_str())).execute(writer).await.unwrap();
    }
    // An identical external ID in another company must never be updated.
    sqlx::query("INSERT INTO communityhero.migration_imports(id,source_sha256,payload) VALUES('delta-foreign',$1,'{}')").bind("1".repeat(64)).execute(writer).await.unwrap();
    sqlx::query("INSERT INTO communityhero.workspaces(id,account,import_id,metadata) VALUES('delta-foreign','BAW','delta-foreign','{}')").execute(writer).await.unwrap();
    sqlx::query("INSERT INTO communityhero.posts(workspace_id,id,ordinal,payload) VALUES('delta-foreign','p0',0,'{\"id\":\"p0\",\"text\":\"foreign retained\"}')").execute(writer).await.unwrap();
    sqlx::query("TRUNCATE pg_temp.source_delta_trace").execute(writer).await.unwrap();
    // Public writer loads with FOR UPDATE, validates, calls this module and then
    // writes metadata. A late metadata rejection must unwind earlier batches.
    sqlx::query("CREATE FUNCTION pg_temp.source_delta_reject() RETURNS trigger LANGUAGE plpgsql AS 'BEGIN RAISE EXCEPTION ''synthetic late source delta rejection''; END;'").execute(writer).await.unwrap();
    sqlx::query("CREATE TRIGGER source_delta_reject BEFORE UPDATE ON communityhero.workspaces FOR EACH ROW EXECUTE FUNCTION pg_temp.source_delta_reject()").execute(writer).await.unwrap();
    assert!(db.change_source_snapshot_observed(|d|{*d=after.clone();Ok(())}).await.is_err());
    sqlx::query("DROP TRIGGER source_delta_reject ON communityhero.workspaces").execute(writer).await.unwrap();
    assert_eq!(db.read().await.unwrap(),baseline,"late metadata failure rolls back all 12 entity/audit batches");
    // Physical wrong/missing rows after a valid load cannot become inserts.
    // Mutate in one isolated transaction; then force a mismatch in the third
    // posts UPDATE batch, after the first two batches have written 256 rows.
    for fault in ["missing","ordinal","foreign-only"] {
        let mut tx=writer.begin().await.unwrap();let loaded=load(&mut tx).await.unwrap();assert_eq!(loaded,before);
        match fault {
            "ordinal"=>{sqlx::query("UPDATE communityhero.posts SET ordinal=1000000 WHERE workspace_id=$1 AND id='p259'").bind(WORKSPACE).execute(&mut *tx).await.unwrap();},
            _=>{sqlx::query("DELETE FROM communityhero.posts WHERE workspace_id=$1 AND id='p259'").bind(WORKSPACE).execute(&mut *tx).await.unwrap();},
        }
        if fault=="foreign-only" {
            sqlx::query("INSERT INTO communityhero.posts(workspace_id,id,ordinal,payload) VALUES('delta-foreign','p259',259,'{\"id\":\"p259\",\"text\":\"foreign only\"}')").execute(&mut *tx).await.unwrap();
        }
        let failure=persist(&mut tx,&loaded,&after).await.unwrap_err();
        assert_eq!(failure.1,"Source admission batch row count mismatch","strict update failure: {fault}");
        tx.rollback().await.unwrap();assert_eq!(db.read().await.unwrap(),baseline,"partial prior batches roll back: {fault}");
    }
    // Even if a caller bypasses validation, duplicate UPDATE input identities
    // cannot silently reduce the number of updated physical rows.
    let mut duplicate_updates=after.clone();duplicate_updates["posts"][1]["id"]=json!("p0");
    let mut tx=writer.begin().await.unwrap();let loaded=load(&mut tx).await.unwrap();
    let failure=persist(&mut tx,&loaded,&duplicate_updates).await.unwrap_err();
    assert_eq!(failure.1,"Source admission batch row count mismatch");tx.rollback().await.unwrap();
    assert_eq!(db.read().await.unwrap(),baseline,"duplicate UPDATE identities fail and roll back");
    // Full source projection hides retained audit, so reused audit IDs pass the
    // source validator but MUST conflict at append insertion, never be UPSERTed.
    let mut duplicate=after.clone();duplicate["audit"][0]["id"]=json!("audit0");
    validate_change(&before,&duplicate).unwrap();
    assert!(db.change_source_snapshot_observed(|d|{*d=duplicate;Ok(())}).await.is_err());
    assert_eq!(db.read().await.unwrap(),baseline,"late audit duplicate rolls back preceding entity batches");
    // Missing relational targets fail on insertion, after successful updates.
    let mut dangling=after.clone();dangling["branches"][1]["postId"]=json!("not-present");
    let mut tx=writer.begin().await.unwrap();let loaded=load(&mut tx).await.unwrap();
    assert!(persist(&mut tx,&loaded,&dangling).await.is_err());tx.rollback().await.unwrap();
    assert_eq!(db.read().await.unwrap(),baseline,"late FK error rolls back prior batches");
    // Public readonly guards still precede the delta writer.
    for table in ["jobs","operations","approvals"] {
        assert!(db.change_source_snapshot_observed(|d|{d[table][0]["illegalSourceChange"]=json!(true);Ok(())}).await.is_err());
        assert_eq!(db.read().await.unwrap(),baseline);
    }
    sqlx::query("TRUNCATE pg_temp.source_delta_trace").execute(writer).await.unwrap();
    assert!(db.change_source_snapshot_observed(|d|{*d=after.clone();Ok(())}).await.unwrap().1);
    let mut expected=baseline.clone();apply(&mut expected,&before,&after).unwrap();
    assert_eq!(db.read().await.unwrap(),expected,"connected full persisted domain parity");
    let count:i64=sqlx::query_scalar("SELECT count(*) FROM pg_temp.source_delta_trace").fetch_one(writer).await.unwrap();
    assert_eq!(count,12,"posts 3 UPDATE+2 INSERT, branches/items/proposals each 1+1, audit 1 INSERT");
    let ordinals:Vec<i32>=sqlx::query_scalar("SELECT ordinal FROM communityhero.audit WHERE workspace_id=$1 ORDER BY ordinal").bind(WORKSPACE).fetch_all(writer).await.unwrap();
    assert_eq!(ordinals,vec![0,1,2,3]);
    let foreign:String=sqlx::query_scalar("SELECT payload->>'text' FROM communityhero.posts WHERE workspace_id='delta-foreign' AND id='p0'").fetch_one(writer).await.unwrap();
    assert_eq!(foreign,"foreign retained");
    let before_noop=count;
    assert!(!db.change_source_snapshot_observed(|_|Ok(())).await.unwrap().1);
    let count:i64=sqlx::query_scalar("SELECT count(*) FROM pg_temp.source_delta_trace").fetch_one(writer).await.unwrap();
    assert_eq!(count,before_noop,"noop performs no entity DML");
    for table in ["jobs","operations","approvals","conversations","feedback"] {assert_eq!(expected[table],baseline[table],"retained {table}");}
    // Count is an exact bounded synthetic statement observation, no timing claim.
    db.close().await;
}
