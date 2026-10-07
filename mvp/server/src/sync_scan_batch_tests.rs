use super::*;

fn state()->(Value,ConnectorBinding,String) {
    let mut d=empty();let binding=active_binding(&d).unwrap();
    let scan=prepare(&mut d,&binding.to_json());(d,binding,scan["id"].as_str().unwrap().to_owned())
}
fn capabilities(binding:&ConnectorBinding)->Value {
    let account=bridge_account(binding).unwrap();
    json!({"account":account,"local":{"read":{"version":1,"verified":true,"account":account,
        "binding":binding.to_json(),"objectScope":["11391"],"mode":"open","pageSizes":[10,100],
        "defaultPageSize":10,"preferredPageSize":100,"cursorContractVersion":2}}})
}
fn page(binding:&ConnectorBinding,key:&str,text:&str,next:Value)->Value {
    let item=bound_item(binding,&json!({"id":key,"objectId":"11391","itemId":key,"postKey":"11391:batch-post",
        "postId":"batch-post","branchId":"batch-branch","conversationKey":"11391:batch-branch",
        "providerStatus":"new","text":text,"draft":"","workflow":"attention",
        "contextObservedAt":"2026-10-07T00:00:00Z","providerStatusObservedAt":"2026-10-07T00:00:00Z"})).unwrap();
    json!({"posts":[{"id":"batch-post","text":"complete publication","attachments":[{"type":"photo","url":"https://fixture.invalid/photo"}]}],
        "branches":[{"id":"batch-branch","postId":"batch-post","messages":[{"id":key,"role":"customer","text":text}]}],
        "items":[item],"skipped":[],"cursor":next,"hasMore":!next.is_null(),
        "queueAccounting":{"version":1,"observations":[{"objectId":"11391","itemId":key,"contextRequired":true}],"duplicateQueueCount":0}})
}

#[test]
fn open_frontier_pins_page_contract_and_preserves_old_omitted_cursor() {
    let(mut d,binding,_)=state();let caps=capabilities(&binding);
    let policy=read_policy_from_caps(&binding,bridge_account(&binding).unwrap(),&caps);
    assert_eq!(policy["options"],json!({"pageSize":100}));
    d["sync"]["scan"]["open"].as_object_mut().unwrap().remove("readOptions");
    let before=d["sync"]["scan"]["open"].clone();
    prepare_with_read_policy(&mut d,&binding.to_json(),&policy);
    assert_eq!(d["sync"]["scan"]["open"],before,"an existing omitted contract is not explicit ten or hundred");
    let frontier=prepare_open_frontier_with_read_policy(&mut d,&binding,&policy);
    assert_eq!(read_options(&frontier).unwrap(),json!({"pageSize":100}));
    d["sync"]["openFrontier"]["cursor"]=json!("opaque-hundred");
    d["sync"]["openFrontier"]["accounting"] = json!({"version":1,"overflow":false,"unverifiedPages":0,
        "observations":0,"duplicateObservations":0,"excludedObservations":0,"untrackedObservations":0,
        "records":[],"trackedUnique":0,"importedUnique":0,"unresolvedUnique":0});
    let pinned=d["sync"]["openFrontier"].clone();
    assert_eq!(prepare_open_frontier_with_read_policy(&mut d,&binding,&omitted_read_policy()),pinned);
    for field in ["binding","account","verified","pageSizes"] {
        let mut hostile=caps.clone();hostile["local"]["read"][field]=Value::Null;
        assert_eq!(read_policy_from_caps(&binding,bridge_account(&binding).unwrap(),&hostile)["options"],json!({}));
    }
}

#[test]
fn ordered_open_batch_rejects_cycles_foreign_options_count_and_full_context_byte_overflow() {
    let(d,binding,_)=state();let lane=&d["sync"]["scan"]["open"];
    let first=page(&binding,"overlap","first observation",json!("next"));
    let second=page(&binding,"overlap","new observation",Value::Null);
    validate_open_batch(lane,&binding,bridge_account(&binding).unwrap(),&Value::Null,&[first.clone(),second.clone()]).unwrap();
    for change in ["cycle","options","company","count","bytes","terminal"] {
        let mut one=first.clone();let mut two=second.clone();
        match change {
            "cycle"=>{two["hasMore"]=json!(true);two["cursor"]=json!("next");},
            "options"=>two["readOptions"]=json!({"pageSize":10}),
            "company"=>two["accountBinding"]=json!({"accountKey":"baw-russia"}),
            "count"=>two["queueAccounting"]["observations"]=json!(vec![json!({});OPEN_BATCH_ROWS]),
            "bytes"=>two["branches"][0]["messages"][0]["text"]=json!("Ю".repeat(OPEN_BATCH_BYTES/2)),
            _=>{one["hasMore"]=json!(false);one["cursor"]=Value::Null;},
        }
        assert!(validate_open_batch(lane,&binding,bridge_account(&binding).unwrap(),&Value::Null,&[one,two]).is_err(),"{change}");
    }
}

#[test]
fn verified_open_page_rejects_foreign_contract_publication_and_message_scope() {
    let(_d,binding,_)=state();let caps=capabilities(&binding);let account=bridge_account(&binding).unwrap();
    let policy=read_policy_from_caps(&binding,account,&caps);let mut lane=fresh_lane(true);pin_read_policy(&mut lane,&policy);
    let mut snapshot=page(&binding,"selected","complete context",Value::Null);
    snapshot["readOptions"]=policy["options"].clone();snapshot["readCapabilities"]=policy["capabilities"].clone();
    snapshot["accountBinding"]=json!({"accountKey":account});
    validate_open_page(&lane,&binding,account,&Value::Null,&[],&snapshot).unwrap();
    for change in ["omitted_options","changed_capability","company","item","publication","message"] {
        let mut hostile=snapshot.clone();
        match change {
            "omitted_options"=>{hostile.as_object_mut().unwrap().remove("readOptions");},
            "changed_capability"=>hostile["readCapabilities"]["binding"]["revision"]=json!(2),
            "company"=>hostile["accountBinding"]["accountKey"]=json!("baw-russia"),
            "item"=>hostile["items"][0]["objectId"]=json!("foreign"),
            "publication"=>hostile["posts"][0]["objectId"]=json!("foreign"),
            _=>hostile["branches"][0]["messages"][0]["providerObjectId"]=json!("foreign"),
        }
        assert!(validate_open_page(&lane,&binding,account,&Value::Null,&[],&hostile).is_err(),"{change}");
    }
    let mut wide=caps;wide["local"]["read"]["objectScope"]=json!((0..12).map(|n|format!("object-{n}")).collect::<Vec<_>>());
    assert_eq!(read_policy_from_caps(&binding,account,&wide)["options"],json!({"pageSize":10}),
        "only a declared supported option fitting the logical batch budget is selected for a new wide connection");
}

#[tokio::test]
async fn open_ordered_batch_sqlite_matches_sequential_rows_and_rolls_back_bad_second_page_cursor() {
    let (app,_folder)=crate::tests::test_app().await;
    let (binding,scan_id)=app.change_schedule(|d| {
        let binding=active_binding(d)?;let scan=prepare(d,&binding.to_json());
        Ok((binding,scan["id"].as_str().unwrap().to_owned()))
    }).await.unwrap();
    let before=app.db.read().await.unwrap();let options=read_options(&before["sync"]["scan"]["open"]).unwrap();
    let capabilities=before["sync"]["scan"]["open"]["readCapabilities"].clone();
    let pages=vec![page(&binding,"overlap","first observation",json!("next")),page(&binding,"overlap","new observation",Value::Null)];
    let mut bad=pages.clone();bad[1]["queueAccounting"]["observations"][0]["itemId"]=json!("not-returned");
    assert!(app.change_source_snapshot_scoped(storage::SourceReadIntent::Snapshots(&bad),|d|
        admit_open_batch(d,&scan_id,&binding,None,&Value::Null,&options,&capabilities,&bad)).await.is_err());
    assert_eq!(app.db.read().await.unwrap(),before,"first rows/cursor/receipt rolled back with second accounting failure");
    let mut sequential=before.clone();
    admit(&mut sequential,&scan_id,&binding,"open",&Value::Null,&pages[0]).unwrap();
    admit(&mut sequential,&scan_id,&binding,"open",&json!("next"),&pages[1]).unwrap();
    app.change_source_snapshot_scoped(storage::SourceReadIntent::Snapshots(&pages),|d|
        admit_open_batch(d,&scan_id,&binding,None,&Value::Null,&options,&capabilities,&pages)).await.unwrap();
    let after=app.db.read().await.unwrap();
    for key in ["items","posts","operations","approvals","jobs","materials"] {assert_eq!(after[key],sequential[key],"ordered {key}");}
    for key in ["cursor","done","pages","seenCursors","accounting","coverageComplete"] {
        assert_eq!(after["sync"]["scan"]["open"][key],sequential["sync"]["scan"]["open"][key],"ordered checkpoint {key}");
    }
    assert_eq!(row(&after,"items","overlap").unwrap()["text"],"new observation");
    let receipt=list(&after,"audit").iter().find(|entry|entry["id"]==after["sync"]["lastOpenBatchReceiptId"]).unwrap();
    assert_eq!(receipt["pages"][0]["cursor"],Value::Null);assert_eq!(receipt["pages"][1]["cursor"],"next");
    assert_eq!(receipt["pages"][1]["nextCursor"],Value::Null);assert_eq!(receipt["pages"][1]["ordinal"],1);
    assert_ne!(receipt["pages"][0]["snapshotSha256"],receipt["pages"][1]["snapshotSha256"]);
    app.db.close().await;
}
