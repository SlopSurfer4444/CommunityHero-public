use super::*;
use serde_json::json;

fn ids(d:&Value,table:&str)->Vec<String> {
    crate::list(d,table).iter().map(|row|row["id"].as_str().unwrap().to_owned()).collect()
}
fn marked_job(d:&Value,id:&str,parent:&str,requested:Value)->Value {
    json!({"id":id,"kind":"assistant","purpose":"public_fact_followup","status":"running",
        "parentPrepareJobId":parent,"requestedItemIds":requested,
        "factWorkerScope":{"version":1,"account":d["account"],"connectorBinding":crate::active_binding(d).unwrap().to_json(),
            "jobId":id,"parentJobId":parent,"itemIds":requested}})
}
fn author_fixture()->(Value,String) {
    let mut d=crate::engine_prepare::tests::fixture(false);normalize(&mut d);
    crate::row_mut(&mut d,"items","ready").unwrap()["authorId"]=json!("fact-author");
    crate::row_mut(&mut d,"items","media").unwrap()["authorId"]=json!("ordinary-author");
    for (id,author,account) in [("fact-prior","fact-author",None),("ordinary-prior","ordinary-author",None),
        ("unrelated","other-author",None),("foreign-prior","fact-author",Some("BAW Russia"))] {
        let mut item=crate::row(&d,"items","ready").unwrap().clone();
        item["id"]=json!(id);item["itemId"]=json!(format!("comment-{id}"));item["objectId"]=json!(format!("object-{id}"));
        item["authorId"]=json!(author);item["postId"]=json!(format!("post-{id}"));item["postKey"]=json!(format!("post-{id}"));
        item["branchId"]=json!(format!("branch-{id}"));item["conversationKey"]=json!(format!("thread-{id}"));
        if let Some(account)=account {item["account"]=json!(account);}
        crate::list_mut(&mut d,"items").push(item);
        crate::list_mut(&mut d,"posts").push(json!({"id":format!("post-{id}"),"postKey":format!("post-{id}"),"text":"Author history","platform":"VK"}));
        crate::list_mut(&mut d,"branches").push(json!({"id":format!("branch-{id}"),"postId":format!("post-{id}"),"contextComplete":true,
            "messages":[{"id":format!("comment-{id}"),"role":"participant","text":"Saved customer history"}]}));
    }
    let ordinary=crate::engine_prepare::schedule(&mut d,crate::engine_prepare::Input{item_ids:vec!["media".into()],instruction:None}).unwrap().job_id;
    let fact=marked_job(&d,"fact","retained-parent",json!(["ready"]));crate::list_mut(&mut d,"jobs").push(fact);
    (d,ordinary)
}

#[test]
fn fact_context_union_preserves_both_recipient_author_histories_and_malformed_broad_fallback() {
    let (original,ordinary)=author_fixture();let view=projection_for(&original,Some(&ordinary)).unwrap();
    assert_eq!(ids(&view,"items"),vec!["ready","media","fact-prior","ordinary-prior"]);
    assert_eq!(ids(&view,"branches"),vec!["ready-branch","media-branch","branch-fact-prior","branch-ordinary-prior"]);
    assert_eq!(view["posts"],original["posts"]);assert_eq!(view["jobs"],original["jobs"]);
    let proof=|d:&Value,id:&str|crate::prepare_bundle::review_fingerprint(d,id);
    assert_eq!(proof(&view,"ready"),proof(&original,"ready"),"fact author history is source fingerprint evidence");
    assert_eq!(proof(&view,"media"),proof(&original,"media"),"ordinary context retains its own complete author history");
    for change in ["null-marker","array-marker","version","job-id","parent-id","foreign-binding","foreign-company",
        "missing-ids","null-ids","array-type","empty-ids","duplicate-ids","marker-recipients"] {
        let mut d=original.clone();let fact=crate::row_mut(&mut d,"jobs","fact").unwrap();
        match change {
            "null-marker"=>fact["factWorkerScope"]=Value::Null,
            "array-marker"=>fact["factWorkerScope"]=json!([]),
            "version"=>fact["factWorkerScope"]["version"]=json!(1.0),
            "job-id"=>fact["factWorkerScope"]["jobId"]=json!("another-job"),
            "parent-id"=>fact["parentPrepareJobId"]=json!("another-parent"),
            "foreign-binding"=>fact["factWorkerScope"]["connectorBinding"]=crate::accounts::Profile::BawRussia.binding(),
            "foreign-company"=>fact["factWorkerScope"]["account"]=json!("BAW Russia"),
            "missing-ids"=>{fact.as_object_mut().unwrap().remove("requestedItemIds");},
            "null-ids"=>fact["requestedItemIds"]=Value::Null,
            "array-type"=>fact["requestedItemIds"]=json!({}),
            "empty-ids"=>fact["requestedItemIds"]=json!([]),
            "duplicate-ids"=>fact["requestedItemIds"]=json!(["ready","ready"]),
            _=>fact["factWorkerScope"]["itemIds"]=json!(["media"]),
        }
        let broad=projection_for(&d,Some(&ordinary)).unwrap();
        for table in ["items","branches","proposals","jobs"] {assert_eq!(broad[table],d[table],"{change}: {table} must not disappear");}
    }
    let mut legacy=original.clone();crate::row_mut(&mut legacy,"jobs","fact").unwrap().as_object_mut().unwrap().remove("factWorkerScope");
    let legacy_view=projection_for(&legacy,Some(&ordinary)).unwrap();
    assert!(crate::row(&legacy_view,"jobs","fact").is_ok(),"unmarked legacy holder stays visible to exclusive domain guard");
    assert_eq!(ids(&legacy_view,"items"),vec!["media","ordinary-prior"]);
}

fn connected_fixture(same_family:bool)->(Value,String,String,String) {
    // Flow's cfg(test) helper runs native automatic parent/first-pass/fact
    // scheduling reducers and creates the actual immutable factWorkerScope.
    let (mut d,parent,fact)=crate::fact_followup::storage_active_fact_fixture();
    if same_family {
        let tail=crate::row_mut(&mut d,"items","tail").unwrap();tail["postId"]=json!("ready-post");tail["postKey"]=json!("ready-post");
        crate::row_mut(&mut d,"branches","branch-tail").unwrap()["postId"]=json!("ready-post");
    }
    let ordinary=crate::engine_prepare::schedule(&mut d,crate::engine_prepare::Input{item_ids:vec!["tail".into()],instruction:None}).unwrap().job_id;
    assert_eq!(crate::preparation_workers::pending_conflict(&d,crate::row(&d,"jobs",&ordinary).unwrap(),true),same_family,
        "full real domain fixture must establish its own disposition before storage projection");
    (d,parent,fact,ordinary)
}
fn perturb(d:&mut Value,fact:&str,change:&str) {
    match change {
        "revision"=>crate::row_mut(d,"items","ready").unwrap()["revision"]=json!(999),
        "fingerprint"=>crate::row_mut(d,"branches","ready-branch").unwrap()["messages"]=json!([{"id":"c-ready","text":"Changed exact source"}]),
        "unknown"=>{let target=crate::row(d,"items","ready").unwrap().clone();crate::list_mut(d,"operations").push(json!({
            "id":"late-unknown-alias","itemId":"removed-local-alias","status":"unknown","target":target}));},
        "bad-marker"=>crate::row_mut(d,"jobs",fact).unwrap()["factWorkerScope"]=Value::Null,
        "foreign-binding"=>crate::row_mut(d,"jobs",fact).unwrap()["factWorkerScope"]["connectorBinding"]=crate::accounts::Profile::BawRussia.binding(),
        "requested"=>crate::row_mut(d,"jobs",fact).unwrap()["requestedItemIds"]=json!(["tail"]),
        _=>{crate::row_mut(d,"jobs",fact).unwrap().as_object_mut().unwrap().remove("factWorkerScope");},
    }
}

async fn exercise_reader(db:&Database,same_family:bool)->(String,String) {
    let (initial,parent,fact,ordinary)=connected_fixture(same_family);
    db.change(|d|{*d=initial;Ok(())}).await.unwrap();let full=db.read().await.unwrap();
    let view=db.read_preparation_context(&ordinary).await.unwrap();
    assert_eq!(view,projection_for(&full,Some(&ordinary)).unwrap(),"actual reader/pure union parity");
    assert_eq!(ids(&view,"items"),vec!["ready","tail"],"retain current fact recipient plus ordinary recipient");
    assert!(crate::row(&view,"branches","ready-branch").is_ok());assert!(crate::row(&view,"branches","branch-tail").is_ok());
    assert_eq!(crate::row(&view,"jobs",&parent).unwrap(),crate::row(&full,"jobs",&parent).unwrap(),"exact parent attempts/paid first result retained");
    assert_eq!(crate::row(&view,"jobs",&parent).unwrap()["prepareBundle"]["itemIds"],json!(["ready","media"]),
        "real multi-recipient paid parent remains complete while research selects only ready");
    assert_eq!(crate::row(&view,"jobs",&fact).unwrap()["requestedItemIds"],json!(["ready"]));
    assert!(crate::row(&view,"items","media").is_err(),"saved parent reservation uses complete saved request.items; unrelated current sibling stays scoped out");
    assert_eq!(crate::preparation_reservations::capture(&view,&parent).unwrap(),
        crate::preparation_reservations::capture(&full,&parent).unwrap(),"subset fact context preserves complete saved parent reservation");
    assert_eq!(crate::row(&view,"jobs",&fact).unwrap(),crate::row(&full,"jobs",&fact).unwrap(),"exact immutable research job retained");
    let blocked=crate::preparation_workers::pending_conflict(&view,crate::row(&view,"jobs",&ordinary).unwrap(),true);
    assert_eq!(blocked,same_family,"real worker consumes actual DB context; source omission must not invent conflict/independence");
    if !same_family {
        for change in ["revision","fingerprint","unknown","bad-marker","foreign-binding","requested","legacy"] {
            let mut current=view.clone();let mut full_current=full.clone();perturb(&mut current,&fact,change);perturb(&mut full_current,&fact,change);
            assert!(crate::preparation_workers::pending_conflict(&current,crate::row(&current,"jobs",&ordinary).unwrap(),true),"actual reader consumer must revalidate {change}");
            assert!(crate::preparation_workers::pending_conflict(&full_current,crate::row(&full_current,"jobs",&ordinary).unwrap(),true),"full-state guard independently holds {change}");
        }
    }
    assert_eq!(db.read().await.unwrap(),full,"context reads and domain preflights preserve source, attempts, grants and job bodies");
    (fact,ordinary)
}

#[tokio::test]
async fn sqlite_fact_context_reader_connected_worker_disjoint_and_same_family_parity() {
    for same_family in [false,true] {
        let folder=tempfile::tempdir().unwrap();let db=Database::Sqlite(crate::open_db(&folder.path().join("fact-context.sqlite")).await.unwrap());
        exercise_reader(&db,same_family).await;db.close().await;
    }
}

#[tokio::test]
#[ignore="requires a pristine explicitly isolated communityhero_writer_v51_test_ PostgreSQL fixture"]
async fn postgres_fact_context_reader_connected_worker_parity_and_corrupt_discriminators() {
    let db=writer_v51_fixture_db().await;let (fact,ordinary)=exercise_reader(&db,false).await;
    let Database::Postgres{writer,..}=&db else{panic!("isolated PostgreSQL fixture required")};
    // Corrupt only the disposable explicitly guarded fixture inside rollback
    // transactions; the OR physical/payload discriminator must expose the row
    // to identity/projection validation rather than silently hiding its holder.
    for (column,malformed) in [("kind","discussion"),("status","completed"),("id","corrupt-id")] {
        let mut tx=writer.begin().await.unwrap();
        let statement=format!("UPDATE communityhero.jobs SET {column}=$3 WHERE workspace_id=$1 AND id=$2");
        sqlx::query(sqlx::AssertSqlSafe(statement.as_str())).bind(WORKSPACE).bind(&fact).bind(malformed).execute(&mut *tx).await.unwrap();
        assert!(load_pg_preparation(&mut tx,Some(&ordinary),false,false).await.is_err(),"physical {column} mismatch cannot hide marked holder");
        tx.rollback().await.unwrap();
    }
    let view=db.read_preparation_context(&ordinary).await.unwrap();
    assert!(!crate::preparation_workers::pending_conflict(&view,crate::row(&view,"jobs",&ordinary).unwrap(),true));
    db.close().await;
}
