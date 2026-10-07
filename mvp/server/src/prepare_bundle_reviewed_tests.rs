// Exercises the approval/dispatch freshness guard with genuinely admitted drafts.
fn batch_source() -> Value {
    let mut d=admitted_fixture();
    d["approvals"]=json!([]);
    d["feedback"]=json!([]);
    d["audit"]=json!([]);
    d["items"][0]["postId"]=json!("p");
    d["items"][0]["authorId"]=json!("author-i");
    d["items"][0]["platform"]=json!("vk");
    d["items"][0]["text"]=json!("First question");
    d["items"][0]["createdAt"]=json!("2026-01-01T00:00:00Z");
    let mut other=d["items"][0].clone();
    for (key,value) in [("id","j"),("branchId","other-branch"),("postId","other-post"),
        ("postKey","other-key"),("itemId","provider-j"),("objectId","provider-other-post"),
        ("authorId","author-j"),("text","Second question")] {other[key]=json!(value);}
    d["items"].as_array_mut().unwrap().push(other);
    d["branches"].as_array_mut().unwrap().push(json!({"id":"other-branch","postId":"other-post","messages":[{"id":"other-message","text":"Unrelated branch"}]}));
    d["posts"].as_array_mut().unwrap().push(json!({"id":"other-post","postKey":"other-key","text":"Other post","attachments":[]}));
    d
}
fn admit_batch(mut d:Value)->(Value,Value) {
    // Exercise the recipient-scoped downstream guard with an exact complete
    // generic bundle. Engine scheduling separately enforces strict family
    // selection; this fixture does not call or bypass that scheduler.
    let mut bundle=build(&d,&[json!("i"),json!("j")],&[]).unwrap();
    crate::preparation_materials::attach_request(&d,&mut bundle["request"]).unwrap();
    bundle["digest"]=json!(digest(&bundle["request"]));
    d["jobs"][0]["prepareBundle"]=bundle;
    let mut reviewed=result();
    reviewed["runMetadata"]=crate::editorial_review::fixture_metadata();
    reviewed["editorialEvidence"]=json!({"version":1,"contract":crate::editorial_review::CONTRACT,
        "entries":[{"itemId":"i","kind":"reply_and_close","textSha256":crate::editorial_review::hash_text("Спасибо!"),
            "decision":"accept","reason":"Explicit offline model review of this exact final response",
            "checks":{"companyRules":"pass","intent":"pass","factualScope":"pass"}}]});
    let request=d["jobs"][0]["prepareBundle"]["request"].clone();
    crate::model_material_receipt::fixture_result(&mut d,"run",&request,&mut reviewed).unwrap();
    let outcome=admit(&mut d,"run","chat",&reviewed).unwrap();
    assert_eq!(outcome["candidates"][0]["status"],"review","{outcome}");
    let p=d["proposals"][0].clone();
    assert!(crate::proposal_current(&d,&p).is_ok());
    (d,p)
}
fn close_other(d:&mut Value) {
    d["items"][1]["workflow"]=json!("closed");
    d["items"][1]["providerStatus"]=json!("processed");
    d["items"][1]["revision"]=json!(2);
}
#[test]
fn reviewed_batch_unrelated_completion_preserves_original_proof() {
    let (mut d,p)=admit_batch(batch_source());
    let original=d["jobs"][0]["prepareBundle"].clone();
    close_other(&mut d);
    assert!(current(&d,&original).is_err(),"generation must remain batch-scoped");
    assert!(crate::proposal_current(&d,&p).is_ok(),"unrelated completion must allow the reviewed recipient");
    assert_eq!(d["jobs"][0]["prepareBundle"],original);
    assert_eq!(d["proposals"][0],p,"never refresh accepted review evidence");
    let actor=crate::operator_auth::Actor::local_owner("reviewed-batch-test");
    let approval=crate::create_approval(&mut d,&actor,&json!({"proposals":[{"id":p["id"],"revision":p["revision"]}]})).unwrap();
    assert_eq!(approval["status"],"approved");
    assert_eq!(d["jobs"][0]["prepareBundle"],original);
    assert_eq!(d["proposals"][0]["reviewContextDigest"],p["reviewContextDigest"]);
}
#[test]
fn reviewed_batch_legacy_without_digest_keeps_global_freshness() {
    let (mut d,mut p)=admit_batch(batch_source());
    p.as_object_mut().unwrap().remove("reviewContextDigest");
    assert!(crate::proposal_current(&d,&p).is_ok());
    close_other(&mut d);
    assert!(crate::proposal_current(&d,&p).is_err());
}
#[test]
fn reviewed_batch_rejects_recipient_branch_post_media_and_rules_edits() {
    for change in ["target","status","closed","branch","new_message","parent","post","comment_media","branch_media","post_media","rule","transcript","revision","route","account"] {
        let (mut d,p)=admit_batch(batch_source());
        close_other(&mut d);
        match change {
            "target"=>d["items"][0]["text"]=json!("Changed question"),
            "status"=>d["items"][0]["providerStatus"]=json!("deleted"),
            "closed"=>d["items"][0]["workflow"]=json!("closed"),
            "branch"=>d["branches"][0]["messages"][0]["text"]=json!("Changed branch"),
            "new_message"=>d["branches"][0]["messages"].as_array_mut().unwrap().push(json!({"id":"new-message","text":"New branch evidence","role":"customer"})),
            "parent"=>d["branches"][0]["messages"][0]["parentId"]=json!("new-parent"),
            "post"=>d["posts"][0]["text"]=json!("Changed post"),
            "comment_media"=>d["items"][0]["attachments"]=json!([{"type":"photo"}]),
            "branch_media"=>d["branches"][0]["messages"][0]["attachments"]=json!([{"type":"photo"}]),
            "post_media"=>d["posts"][0]["attachments"]=json!([{"type":"video"}]),
            "rule"=>d["materials"][0]["text"]=json!("Changed global rule"),
            "transcript"=>d["materials"][1]["text"]=json!("Changed related transcript"),
            "revision"=>d["items"][0]["revision"]=json!(99),
            "route"=>d["items"][0]["itemId"]=json!("another-provider-recipient"),
            "account"=>d["account"]=json!("BAW"),
            _=>unreachable!(),
        }
        assert!(crate::proposal_current(&d,&p).is_err(),"must reject {change}");
    }
}
#[test]
fn reviewed_batch_same_author_cross_post_history_stays_pinned() {
    let mut source=batch_source();
    source["items"][1]["authorId"]=source["items"][0]["authorId"].clone();
    let (mut d,p)=admit_batch(source);
    assert!(!rows(&EvidenceContext::new(&d).evidence_for_item("i").unwrap(),"customerCases").is_empty());
    d["items"][1]["text"]=json!("Changed same-author case on another post");
    assert!(crate::proposal_current(&d,&p).is_err());
}
#[test]
fn reviewed_batch_rejects_tampering_and_wrong_membership() {
    for change in ["hash","version","id","proposal_digest","absent_target","duplicate_target","saved_missing","saved_duplicate","saved_other","review_digest"] {
        let (mut d,mut p)=admit_batch(batch_source());
        let b=&mut d["jobs"][0]["prepareBundle"];
        match change {
            "hash"=>b["request"]["instruction"]=json!("tampered"),
            "version"=>b["version"]=json!(2),
            "id"=>b["id"]=json!("other-bundle"),
            "proposal_digest"=>p["prepareBundleDigest"]=json!("0".repeat(64)),
            "absent_target"=>b["itemIds"]=json!(["j"]),
            "duplicate_target"=>b["itemIds"]=json!(["i","i","j"]),
            "saved_missing"=>b["request"]["items"]=json!([b["request"]["items"][1]]),
            "saved_duplicate"=>b["request"]["items"]=json!([b["request"]["items"][0],b["request"]["items"][0]]),
            "saved_other"=>b["request"]["items"][0]["id"]=json!("outsider"),
            "review_digest"=>p["reviewContextDigest"]=json!("0".repeat(64)),
            _=>unreachable!(),
        }
        if change.starts_with("saved_") {b["digest"]=json!(digest(&b["request"]));p["prepareBundleDigest"]=b["digest"].clone();}
        assert!(crate::proposal_current(&d,&p).is_err(),"must reject {change}");
    }
}
#[test]
fn reviewed_batch_knowledge_head_retirement_revision_expiry_reject() {
    for mode in ["retire","revision","expiry"] {
        let mut source=batch_source();
        let admitted_at=(chrono::Utc::now()-chrono::Duration::days(2)).to_rfc3339();
        crate::knowledge::sync_catalog(&mut source,&admitted_at).unwrap();
        let active=rows(&source,"knowledge_entries").iter().find(|e|e["sourceMaterialId"]=="global").unwrap().clone();
        crate::knowledge::revise(&mut source,active["id"].as_str().unwrap(),&json!({"expectedVersionId":active["currentVersionId"],
            "status":"active","trust":"verified","provenance":"Test operator verified source"}),&admitted_at).unwrap();
        let (mut d,p)=admit_batch(source);
        close_other(&mut d);
        let e=rows(&d,"knowledge_entries").iter().find(|e|e["sourceMaterialId"]=="global").unwrap().clone();
        let mut body=json!({"expectedVersionId":e["currentVersionId"]});
        match mode {
            "retire"=>body["status"]=json!("retired"),
            "expiry"=>body["validUntil"]=json!((chrono::Utc::now()-chrono::Duration::days(1)).to_rfc3339()),
            _=>body["validUntil"]=json!((chrono::Utc::now()+chrono::Duration::days(1)).to_rfc3339()),
        }
        crate::knowledge::revise(&mut d,e["id"].as_str().unwrap(),&body,&crate::now()).unwrap();
        assert!(crate::proposal_current(&d,&p).is_err(),"must reject {mode}");
    }
}
#[test]
fn reviewed_batch_keeps_all_original_research_pins() {
    let mut source=batch_source();
    let at=crate::now();
    let mut archive=json!({"id":"research:j1","jobId":"j1","account":"LikeAvto","connectorBinding":source["connectorBinding"],
        "createdAt":at,"trust":"source_only","activePolicy":false,"posts":[source["posts"][1]],
        "bindings":[{"itemId":"original","postKey":"other-key"}],
        "review":{"status":"completed","research":{"version":1,"status":"completed","trust":"source_only","webCalls":1,"completedAt":at,
            "sources":[{"itemId":"original","url":"https://manufacturer.example/specs","title":"Specs","claim":"Archived claim","trust":"source_only"}]}}});
    archive["checksum"]=json!(crate::research_cache::checksum(&archive));
    source["preparationResearch"]=json!([archive]);
    let (d,p)=admit_batch(source);
    assert_eq!(rows(&d["jobs"][0]["prepareBundle"],"researchManifest").len(),1);
    for change in ["claim","missing","post","binding"] {
        let mut changed=d.clone();
        close_other(&mut changed);
        assert!(crate::proposal_current(&changed,&p).is_ok());
        match change {
            "claim"=>changed["preparationResearch"][0]["review"]["research"]["sources"][0]["claim"]=json!("Changed claim"),
            "missing"=>changed["preparationResearch"]=json!([]),
            "post"=>changed["posts"][1]["text"]=json!("Different source post"),
            "binding"=>changed["preparationResearch"][0]["account"]=json!("BAW"),
            _=>unreachable!(),
        }
        // The target review digest alone does not include unrelated research;
        // the original full research manifest must still reject these changes.
        assert_eq!(review_fingerprint(&changed,"i").unwrap(),p["reviewContextDigest"]);
        assert!(crate::proposal_current(&changed,&p).is_err(),"must reject {change}");
    }
}
