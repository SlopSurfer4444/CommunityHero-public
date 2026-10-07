//! Connected R6 differential fixtures. Authored UNRUN; ROOT owns execution.
use super::*;
#[path="legacy_thread_graph.rs"] mod legacy_thread_graph;
include!("legacy_snapshot_order.rs");
include!("legacy_merge_snapshot.rs");

const AT: &str = "2026-10-04T08:00:00Z";
const EARLIER: &str = "2026-10-04T07:00:00Z";

#[test]
fn native_comment_photo_head_survives_refresh_and_connector_cannot_mint_one(){
    // New native field: assert owner requirements directly; the retained R6
    // oracle has no contract for it and is not rewritten to match this reducer.
    for existing in [None,Some(Value::Null),Some(json!({"receiptId":"native-saved","sourceKind":"comment_attachment","proof":"retained"}))] {
        let mut d=empty();let mut old=item("existing");old["revision"]=json!(2);old["draft"]=json!("saved owner text");
        if let Some(head)=&existing{old["commentPhotoAcquisition"]=head.clone();}
        d["items"]=json!([old]);d["operations"]=json!([{"id":"held","itemId":"existing","status":"unknown"}]);
        let operations=d["operations"].clone();
        let mut incoming=item("existing");incoming["commentPhotoAcquisition"]=json!({"receiptId":"connector-forgery"});
        let mut new=item("new");new["commentPhotoAcquisition"]=json!({"receiptId":"connector-new-forgery"});
        merge_snapshot(&mut d,&json!({"items":[incoming,new]})).unwrap();
        assert_eq!(d["items"][0].get("commentPhotoAcquisition"),existing.as_ref());
        assert!(d["items"][1].get("commentPhotoAcquisition").is_none());
        assert_eq!(d["items"][0]["draft"],"saved owner text");assert_eq!(d["operations"],operations);
    }
}

fn item(id: &str) -> Value {
    json!({"id":id,"itemId":format!("provider-{id}"),"objectId":"object","postId":"p","branchId":"b",
        "postKey":"object:post","conversationKey":format!("object:{id}"),"providerStatus":"new",
        "contextEvidenceDigest":"observed","contextObservedAt":AT,"providerStatusObservedAt":AT})
}
fn normalized(mut d: Value) -> Value {
    // Only branch observation wall time differs between sequential oracle calls.
    // Do not erase digest, revision, policy, paid evidence, UNKNOWN or audit data.
    for branch in list_mut(&mut d,"branches") {
        if branch.get("observedAt").is_some() { branch["observedAt"] = json!(AT); }
    }
    d
}
fn compare_merge(d: Value, snapshot: &Value) -> Value {
    let mut actual = d.clone();
    let mut expected = d;
    let result = merge_snapshot(&mut actual,snapshot).map_err(|e|(e.0,e.1));
    let oracle = legacy_merge_snapshot(&mut expected,snapshot).map_err(|e|(e.0,e.1));
    assert_eq!(result,oracle,"same connected error and success contract");
    assert_eq!(normalized(actual.clone()),normalized(expected),"same connected durable state");
    actual
}

#[test]
fn connected_first_duplicate_post_and_item_and_sequential_append_are_preserved() {
    for head in [Value::Null,json!({"id":"server-head","revision":4,"proof":"paid"})] {
        let mut d = empty();
        let mut first = json!({"id":"p","text":"old"});
        if !head.is_null() { first["photoAcquisition"] = head.clone(); }
        let second = json!({"id":"p","text":"second untouched","photoAcquisition":{"id":"wrong-head"}});
        d["posts"] = json!([first,second.clone()]);
        let mut old = item("same");
        old["draft"] = json!("paid/manual retained"); old["draftEdited"] = json!(true);
        old["revision"] = json!(7); old["workflow"] = json!("waiting");
        old["autoPreparation"] = json!({"humanOverrideAt":AT,"jobId":"paid-job"});
        let mut duplicate = old.clone(); duplicate["draft"] = json!("second untouched");
        d["items"] = json!([old,duplicate.clone()]);
        d["jobs"] = json!([{"id":"paid-job","status":"unknown","result":{"paid":true,"text":"keep"}}]);
        d["operations"] = json!([{"id":"op","itemId":"same","status":"unknown","receipt":"keep"}]);
        let before_jobs = d["jobs"].clone(); let before_ops = d["operations"].clone();
        let result = compare_merge(d,&json!({"posts":[{"id":"p","text":"incoming","photoAcquisition":{"id":"injected"}},
            {"id":"p","text":"latest","photoAcquisition":{"id":"injected-again"}}],
            "items":[item("same"),item("new"),item("new")]}));
        assert_eq!(result["posts"][0]["text"],"latest");
        assert_eq!(result["posts"][0]["photoAcquisition"],head);
        if head.is_null() { assert!(result["posts"][0].get("photoAcquisition").is_none()); }
        assert_eq!(result["posts"][1],second);
        assert_eq!(result["items"][0]["draft"],"paid/manual retained");
        assert_eq!(result["items"][1],duplicate);
        assert_eq!(list(&result,"items").len(),3,"new repeated id updates appended row");
        assert_eq!(result["jobs"],before_jobs); assert_eq!(result["operations"],before_ops);
    }
}

#[test]
fn connected_second_new_id_with_changed_provider_identity_keeps_conflict_and_prefix_mutation() {
    let first = item("new"); let mut second = first.clone(); second["itemId"] = json!("retargeted");
    let actual = compare_merge(empty(),&json!({"items":[first,second]}));
    assert_eq!(list(&actual,"items").len(),1);
    assert_eq!(actual["items"][0]["itemId"],"provider-new");
}

#[test]
fn connected_ordering_preserves_malformed_stale_identity_and_timestamp_error_precedence() {
    for identity in [Value::Null,json!(7),json!({"legacy":"id"}),json!(["legacy"])] {
        let mut d = empty();
        d["items"] = json!([{"id":identity,"contextObservedAt":AT}]);
        let snapshot = json!({"items":[{"id":identity,"branchId":"b","postId":"p","contextObservedAt":EARLIER}],
            "posts":[{"id":"p","text":"must drop"}],"branches":[{"id":"b","messages":[]}]});
        let result = compare_merge(d.clone(),&snapshot);
        assert_eq!(result,d,"stale malformed id was filtered before required(id)");
    }
    let mut d = empty(); d["items"] = json!([{"contextObservedAt":AT}]);
    compare_merge(d,&json!({"items":[{"contextObservedAt":EARLIER,"branchId":"b","postId":"p"}],
        "posts":[{"id":"p"}],"branches":[{"id":"b","messages":[]}]}));
    let mut d = empty(); let before = d.clone();
    let snapshot = json!({"posts":[{"id":null}],"items":[{"id":"a","contextObservedAt":"invalid"}]});
    let error = merge_snapshot(&mut d,&snapshot).unwrap_err();
    assert_eq!(error.1,"Invalid provider observation timestamp"); assert_eq!(d,before);
    compare_merge(before,&snapshot);
}

#[test]
fn ordering_keeps_validation_before_touching_malformed_existing_arrays() {
    let malformed = json!({"items":null});
    let empty = json!({"items":[],"posts":[],"branches":[]});
    assert_eq!(snapshot_order::ordered(&malformed,&empty).unwrap(),legacy_ordered(&malformed,&empty).unwrap());
    let invalid = json!({"items":[{"id":"a","contextObservedAt":"invalid"}]});
    let actual = snapshot_order::ordered(&malformed,&invalid).unwrap_err();
    let expected = legacy_ordered(&malformed,&invalid).unwrap_err();
    assert_eq!((actual.0,actual.1),(expected.0,expected.1));
}

#[test]
fn connected_duplicate_branch_digest_retains_last_row_without_phantom_revision_change() {
    let mut d = empty();
    d["branches"] = json!([{"id":"b","postId":"p","messages":[{"id":"first","text":"first"}]},
        {"id":"b","postId":"p","messages":[{"id":"last","text":"last"}]}]);
    let result = compare_merge(d,&json!({"items":[item("a")]}));
    assert_eq!(result["items"][0]["branchContextDigest"],snapshot_domain::branch_context_digest(&result["branches"][1]));
    let refreshed = compare_merge(result.clone(),&json!({}));
    assert_eq!(refreshed["items"][0]["revision"],result["items"][0]["revision"]);
}

#[test]
fn borrowed_graph_matches_r6_oracle_with_large_paid_fields_and_malformed_layouts() {
    for case in ["joined","foreign","ambiguous","empty_items","malformed_items","no_binding","malformed_branches"] {
        let mut d = empty();
        d["items"] = json!((0..12).map(|n|json!({"id":format!("i{n}"),"branchId":format!("b{n}"),"postId":"p",
            "objectId":"object","targetId":format!("message{n}"),"draft":"paid evidence".repeat(2000),
            "autoPreparation":{"result":"never copy for graph".repeat(2000)}})).collect::<Vec<_>>());
        d["branches"] = json!((0..12).map(|n|json!({"id":format!("b{n}"),"postId":"p",
            "messages":[{"id":format!("message{n}"),"parentId":"missing","text":format!("text{n}")}]})).collect::<Vec<_>>());
        match case {
            "foreign" => d["items"][0]["connectorBinding"] = json!({"account":"foreign"}),
            "ambiguous" => { let mut item = d["items"][0].clone(); item["objectId"] = json!("foreign"); list_mut(&mut d,"items").push(item); },
            "empty_items" => d["items"] = json!([]),
            "malformed_items" => d["items"] = json!({}),
            "no_binding" => { d.as_object_mut().unwrap().remove("connectorBinding"); },
            "malformed_branches" => d["branches"] = json!({}),
            _ => {}
        }
        let mut expected = d.clone(); legacy_thread_graph::enrich(&mut expected);
        thread_graph::enrich(&mut d); assert_eq!(d,expected,"{case}");
    }
}

/// Informational timing, no threshold and no performance assertion. ROOT runs
/// this against the exact integrated candidate; clock normalization is separate.
#[test]
#[ignore = "ROOT-owned representative domain benchmark"]
fn representative_source_merge_r6_r7_parity_and_cost() {
    let mut d = empty();
    d["jobs"] = json!((0..2000).map(|n|json!({"id":format!("retained-{n}"),"kind":"prepare","status":"completed",
        "purpose":"auto_prepare","result":{"text":"immutable paid context".repeat(200)}})).collect::<Vec<_>>());
    d["items"] = json!((0..600).map(|n|{
        let mut row = item(&format!("i{n}")); row["postId"] = json!(format!("p{}",n/5));
        row["branchId"] = json!(format!("b{}",n/5)); row["workflow"] = json!("prepared");
        row["draft"] = json!("retained draft".repeat(200)); row["revision"] = json!(3); row
    }).collect::<Vec<_>>());
    d["posts"] = json!((0..120).map(|n|json!({"id":format!("p{n}"),"text":"post"})).collect::<Vec<_>>());
    d["branches"] = json!((0..120).map(|n|json!({"id":format!("b{n}"),"postId":format!("p{n}"),"observedAt":AT,
        "messages":(0..8).map(|m|json!({"id":format!("message-{n}-{m}"),"parentId":format!("parent-{n}"),
            "text":"context evidence".repeat(200),"authorId":"enrichment","attachments":[{"type":"photo","url":"https://example.test/p"}]})).collect::<Vec<_>>()})).collect::<Vec<_>>());
    let snapshot = json!({"items":d["items"],"posts":d["posts"],"branches":d["branches"]});
    let workspace_bytes = d.to_string().len(); let item_bytes = d["items"].to_string().len();
    let mut before = d.clone(); let mut after = d;
    let r6_started = std::time::Instant::now(); legacy_merge_snapshot(&mut before,&snapshot).unwrap(); let r6_ms = r6_started.elapsed().as_secs_f64()*1000.0;
    let r7_started = std::time::Instant::now(); merge_snapshot(&mut after,&snapshot).unwrap(); let r7_ms = r7_started.elapsed().as_secs_f64()*1000.0;
    assert_eq!(normalized(before),normalized(after));
    eprintln!("domain benchmark R6-main SHA256=BD174BB6AE49B5DB8A52547F94F0A21C6EC7390E9CC7C2A4E2CD3F4C9C73DED3 R6-graph SHA256=AF3000C2094FE37185F6E66439C6C2A06BB3FE2ABC8E762BE457A36DB74902A1 items=600 branches=120 posts=120 retained_jobs=2000 workspace_json_bytes={workspace_bytes} avoided_graph_item_clone_json_bytes={item_bytes} r6_ms={r6_ms:.3} r7_ms={r7_ms:.3}");
}
