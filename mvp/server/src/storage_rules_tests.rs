use super::*;
use sha2::{Digest, Sha256};
const AT: &str = "2026-09-23T12:00:00Z";

fn workspace() -> Value {
    json!({"account":"LikeAvto","connectorBinding":crate::legacy_binding(),"posts":[],"materials":[],"knowledge_entries":[],"knowledge_versions":[],"feedback":[]})
}
fn rehash(version: &mut Value) {
    let mut payload=version.clone();
    for key in ["id","hash","createdAt"] {payload.as_object_mut().unwrap().remove(key);}
    let hash=format!("{:x}",Sha256::digest(payload.to_string().as_bytes()));
    version["hash"]=json!(hash);version["id"]=json!(format!("knowledge-version-{hash}"));
}
fn rule(d:&mut Value,id:&str,scope:Value,extra:Value) {
    let mut v=json!({"entryId":id,"sourceMaterialId":format!("material-{id}"),"sourceRevision":1,"sourceHash":"source-fingerprint","title":id,"text":format!("Required {id}"),"sourceUrl":"","postKey":"","kind":"rule","scope":scope,"trust":"verified","status":"active","validFrom":"2026-09-01T00:00:00Z","validUntil":null,"createdAt":AT});
    for (key,value) in extra.as_object().unwrap(){v[key]=value.clone();}
    rehash(&mut v);
    d["knowledge_entries"].as_array_mut().unwrap().push(json!({"id":id,"sourceMaterialId":v["sourceMaterialId"],"currentVersionId":v["id"],"kind":v["kind"],"scope":v["scope"],"status":v["status"]}));
    d["knowledge_versions"].as_array_mut().unwrap().push(v);
}
async fn database(d:&Value)->(tempfile::TempDir,Database) {
    let temp=tempfile::tempdir().unwrap();
    let pool=crate::open_db(&temp.path().join("workspace.sqlite")).await.unwrap();
    sqlx::query("UPDATE workspace SET payload=? WHERE id=1").bind(d.to_string()).execute(&pool).await.unwrap();
    (temp,Database::Sqlite(pool))
}

#[tokio::test]
async fn current_rules_catalog_omits_history_facts_and_private_workspace_payloads() {
    let mut d=workspace();
    let first=crate::knowledge::save_instruction(&mut d,&json!({"requestId":"one","title":"Human rule","text":"OLD_HISTORY_CANARY"}),AT).unwrap();
    let latest=crate::knowledge::save_instruction(&mut d,&json!({"requestId":"two","title":"Human rule","text":"Latest human edit","entryId":first["entry"]["id"],"expectedVersionId":first["version"]["id"]}),AT).unwrap();
    rule(&mut d,"fact",json!({"account":"LikeAvto","postKeys":[]}),json!({"kind":"fact","text":"PRIVATE_FACT_CANARY"}));
    rule(&mut d,"foreign",json!({"account":"BAW Russia","postKeys":[]}),json!({"text":"FOREIGN_CANARY"}));
    d["jobs"]=json!([{"result":"PRIVATE_JOB_CANARY"}]);
    let (_temp,db)=database(&d).await;
    let result=db.read_instruction_catalog().await.unwrap();
    assert_eq!(result["entries"],json!([latest["entry"]]));
    assert_eq!(result["versions"],json!([latest["version"]]));
    for canary in ["OLD_HISTORY_CANARY","PRIVATE_FACT_CANARY","FOREIGN_CANARY","PRIVATE_JOB_CANARY"] {assert!(!result.to_string().contains(canary));}
    assert_eq!(db.read().await.unwrap(),d,"Projection must not mutate history or source fingerprints");
}

#[tokio::test]
async fn instruction_projection_observes_new_cas_head_and_preserves_immutable_history() {
    let mut d=workspace();
    let first=crate::knowledge::save_instruction(&mut d,&json!({"requestId":"cas-one","title":"Rule","text":"Original"}),AT).unwrap();
    let (_temp,db)=database(&d).await;
    let body=json!({"requestId":"cas-two","title":"Rule","text":"Edited","entryId":first["entry"]["id"],"expectedVersionId":first["version"]["id"]});
    db.change(|data|{crate::knowledge::save_instruction(data,&body,AT).map_err(internal)?;Ok(())}).await.unwrap();
    let current=db.read_instruction_catalog().await.unwrap();
    assert_eq!(current["versions"][0]["text"],"Edited");
    let stored=db.read().await.unwrap();
    assert_eq!(stored["knowledge_versions"][0],first["version"]);
    assert_eq!(stored["knowledge_versions"].as_array().unwrap().len(),2);
    assert_eq!(stored["materials"][0]["locallyEdited"],true);
    let mut stale=body.clone();stale["requestId"]=json!("cas-stale");stale["text"]=json!("Must not save");
    assert!(db.change(|data|{crate::knowledge::save_instruction(data,&stale,AT).map_err(internal)?;Ok(())}).await.is_err());
    assert_eq!(db.read().await.unwrap(),stored);
    assert_eq!(db.read_instruction_catalog().await.unwrap(),current);
}

#[tokio::test]
async fn scoped_rules_match_domain_selection_including_time_trust_and_legacy_aliases() {
    let mut d=workspace();
    let target="vk:literal_%'post";
    d["posts"]=json!([{"id":"p","postKey":target,"account":"LikeAvto","scope":{"account":"LikeAvto"},"attachments":[{"private":"NOT_NEEDED"}]}]);
    let global=json!({"account":"LikeAvto","postKeys":[]});
    for (id,extra) in [("global",json!({})),("pending",json!({"status":"pending_review"})),("expired",json!({"validUntil":AT})),("future",json!({"validFrom":"2026-09-24T00:00:00Z"})),("untrusted",json!({"trust":"unverified"}))] {rule(&mut d,id,global.clone(),extra);}
    rule(&mut d,"post",json!({"account":"LikeAvto","postKeys":[target]}),json!({}));
    rule(&mut d,"unrelated",json!({"account":"LikeAvto","postKeys":["other"]}),json!({}));
    rule(&mut d,"foreign",json!({"account":"BAW Russia","postKeys":[]}),json!({}));
    rule(&mut d,"alias",global.clone(),json!({"companyImport":{"companyKey":"likeavto","scope":{"postAliases":[{"namespace":"commentops-fast.post-key","value":target}]}}}));
    rule(&mut d,"unresolved",global,json!({"companyImport":{"companyKey":"likeavto","scope":{"postAliases":[{"namespace":"commentops-fast.post-key","value":"missing"}]}}}));
    let expected=crate::knowledge::select(&d,&[json!({"postKey":target})],&[],AT).unwrap();
    let (_temp,db)=database(&d).await;
    assert_eq!(db.read_rule_selection(&[target.into()],AT).await.unwrap(),expected);
    assert_eq!(expected["materials"].as_array().unwrap().len(),3);
    assert_eq!(expected["excluded"].as_array().unwrap().len(),4);
}

#[tokio::test]
async fn native_binding_does_not_reinterpret_legacy_alias_and_foreign_post_does_not_resolve_it() {
    for native in [false,true] {
        let mut d=workspace();
        d["posts"]=json!([{"postKey":"post","account":"BAW Russia","scope":{"account":"BAW Russia"}}]);
        if native {d["connectorBinding"]["connector"]=json!("vk");d["connectorBinding"]["id"]=json!("native-vk");}
        rule(&mut d,"global",json!({"account":"LikeAvto","postKeys":[]}),json!({}));
        rule(&mut d,"alias",json!({"account":"LikeAvto","postKeys":[]}),json!({"companyImport":{"companyKey":"likeavto","scope":{"postAliases":[{"namespace":"commentops-fast.post-key","value":"post"}]}}}));
        let expected=crate::knowledge::select(&d,&[json!({"postKey":"post"})],&[],AT).unwrap();
        let (_temp,db)=database(&d).await;
        assert_eq!(db.read_rule_selection(&["post".into()],AT).await.unwrap(),expected);
        assert_eq!(expected["materials"].as_array().unwrap().len(),1);
    }
}

#[tokio::test]
async fn rule_bounds_fail_closed_without_truncating_mandatory_rules() {
    let mut d=workspace();
    for n in 0..=MAX_RULES {rule(&mut d,&format!("rule-{n}"),json!({"account":"LikeAvto","postKeys":[]}),json!({}));}
    let (_temp,db)=database(&d).await;
    assert!(db.read_instruction_catalog().await.is_err());
    assert!(db.read_rule_selection(&[],AT).await.is_err());
    d["knowledge_entries"]=json!([]);d["knowledge_versions"]=json!([]);
    rule(&mut d,"huge",json!({"account":"LikeAvto","postKeys":[]}),json!({"text":"x".repeat(MAX_BYTES as usize)}));
    let (_temp,db)=database(&d).await;
    assert!(db.read_instruction_catalog().await.is_err());
    assert!(db.read_rule_selection(&vec!["x".into();MAX_POST_KEYS+1],AT).await.is_err());
}

#[tokio::test]
async fn selected_head_corruption_missing_version_and_foreign_binding_are_rejected() {
    for fault in ["hash","missing","binding","account"] {
        let mut d=workspace();rule(&mut d,"global",json!({"account":"LikeAvto","postKeys":[]}),json!({}));
        match fault {"hash"=>d["knowledge_versions"][0]["text"]=json!("tampered"),"missing"=>d["knowledge_versions"]=json!([]),"binding"=>d["connectorBinding"]["accountId"]=json!("BAW Russia"),_=>d["account"]=json!("BAW Russia")};
        let (_temp,db)=database(&d).await;
        assert!(db.read_instruction_catalog().await.is_err(),"{fault}");
    }
}

/// Opt-in SELECT-only parity on a separately provisioned, quiescent clone.
#[tokio::test]
async fn scoped_rule_disagreement_cannot_hide_before_validation() {
    for fault in ["scope","status","kind","source"] {
        let mut d=workspace();
        rule(&mut d,"rule",json!({"account":"LikeAvto","postKeys":["unrelated"]}),json!({}));
        if fault=="scope" {d["knowledge_entries"][0]["scope"]["postKeys"]=json!([]);}
        let v=&mut d["knowledge_versions"][0];
        match fault {"status"=>v["status"]=json!("pending_review"),"kind"=>v["kind"]=json!("reference"),"source"=>v["sourceMaterialId"]=json!("other"),_=>{}}
        rehash(v);let current=v["id"].clone();
        d["knowledge_entries"][0]["currentVersionId"]=current;
        let (_temp,db)=database(&d).await;
        assert!(db.read_rule_selection(&["target".into()],AT).await.is_err(),"{fault}");
        assert_eq!(db.read().await.unwrap(),d);
    }
}

/// Opt-in SELECT-only parity on a separately provisioned, quiescent clone.
/// Does not call Database::postgres (migrations/owner setup), seed or clean data.
#[tokio::test]
#[ignore = "requires COMMUNITYHERO_RULE_READ_TEST_URL for an isolated rule_read_test clone"]
async fn postgres_current_rule_clone_readonly_parity() {
    let url=std::env::var("COMMUNITYHERO_RULE_READ_TEST_URL").expect("explicit isolated clone URL");
    let pool=PgPoolOptions::new().max_connections(1).after_connect(|connection,_|Box::pin(async move {
        sqlx::query("SET default_transaction_read_only=on").execute(connection).await?;Ok(())
    })).connect(&url).await.unwrap();
    let database:String=sqlx::query_scalar("SELECT current_database()").fetch_one(&pool).await.unwrap();
    assert!(database.contains("rule_read_test"),"refusing non-test database");
    let db=Database::Postgres{writer:pool.clone(),reader:pool};
    let full=db.read().await.unwrap();
    let mut expected=full.clone();
    let entries:Vec<Value>=full["knowledge_entries"].as_array().unwrap().iter().filter(|e|e["scope"]["account"]=="LikeAvto"&&matches!(e["kind"].as_str(),Some("rule"|"policy"))).cloned().collect();
    let versions:Vec<Value>=entries.iter().map(|e|full["knowledge_versions"].as_array().unwrap().iter().find(|v|v["id"]==e["currentVersionId"]).unwrap().clone()).collect();
    expected["knowledge_entries"]=json!(entries);expected["knowledge_versions"]=json!(versions);
    let started=std::time::Instant::now();
    let catalog=db.read_instruction_catalog().await.unwrap();
    let catalog_ms=started.elapsed().as_secs_f64()*1000.0;
    assert_eq!(catalog,json!({"entries":entries,"versions":versions}));
    let keys:Vec<String>=full["posts"].as_array().unwrap().iter().filter_map(|p|p["postKey"].as_str().map(str::to_owned)).take(MAX_POST_KEYS).collect();
    let at=crate::now();
    for key in &keys {
        assert_eq!(db.read_rule_selection(&[key.clone()],&at).await.unwrap(),crate::knowledge::select(&expected,&[json!({"postKey":key})],&[],&at).unwrap());
    }
    println!("RULE_READ_PROBE {}",json!({"currentRuleHeads":entries.len(),"historyVersions":full["knowledge_versions"].as_array().unwrap().len(),"catalogBytes":catalog.to_string().len(),"fullBytes":full.to_string().len(),"catalogMs":catalog_ms,"postScopesCompared":keys.len()}));
    db.close().await;
}
