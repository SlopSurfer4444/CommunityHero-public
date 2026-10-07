use super::*;
use std::sync::{Arc, Mutex};

#[derive(Clone)]
struct Fixture {
    responses: Arc<Mutex<Vec<Value>>>,
    requests: Arc<Mutex<Vec<ReadRequest>>>,
}
impl Fixture {
    fn new(responses: Vec<Value>) -> Self {
        Self {
            responses: Arc::new(Mutex::new(responses)),
            requests: Arc::new(Mutex::new(vec![])),
        }
    }
    fn calls(&self) -> usize {
        self.requests.lock().unwrap().len()
    }
}
impl ReadTransport for Fixture {
    fn request<'a>(
        &'a self,
        _binding: &'a ConnectorBinding,
        request: ReadRequest,
    ) -> ConnectorFuture<'a, Value> {
        Box::pin(async move {
            self.requests.lock().unwrap().push(request);
            let mut responses = self.responses.lock().unwrap();
            if responses.is_empty() {
                return Err(BoundaryError("Fixture exhausted"));
            }
            Ok(responses.remove(0))
        })
    }
}
fn binding() -> ConnectorBinding {
    ConnectorBinding::from_json(
        &json!({"id":"native-vk-likeavto","workspaceId":"local-pilot",
    "accountId":"LikeAvto","connector":"vk","revision":1,"providerAccountId":"vk-community-123"}),
    )
    .unwrap()
}
fn post() -> WallPost {
    WallPost::new(-123, 7).unwrap()
}
fn fixture(name: &str) -> Value {
    serde_json::from_str(match name {
        "comments" => include_str!("../../tests/fixtures/native-vk/comments.json"),
        "partial" => include_str!("../../tests/fixtures/native-vk/partial-thread.json"),
        "comment" => include_str!("../../tests/fixtures/native-vk/comment.json"),
        _ => panic!("Unknown fixture"),
    })
    .unwrap()
}
fn connector(transport: Fixture) -> VkConnector<Fixture> {
    VkConnector::new(binding(), vec![post()], 100, transport).unwrap()
}
fn single(id: u64, total: u64) -> Value {
    json!({"response":{"count":total,"current_level_count":total,"items":[{"id":id,"from_id":500,"date":1750000000,"text":"Question","thread":{"count":0}}]}})
}

#[tokio::test]
async fn native_wire_mapping_uses_scoped_ids_without_angry_status() {
    let transport = Fixture::new(vec![fixture("comments")]);
    let c = connector(transport.clone());
    let result = c.read_snapshot(&binding(), READ_MODE, None).await.unwrap();
    assert!(result.page.complete);
    assert!(result.page.cursor.is_none());
    assert_eq!(result.page.items.len(), 3);
    assert_eq!(result.posts.len(), 1);
    assert_eq!(result.branches.len(), 2);
    for item in &result.page.items {
        assert_eq!(item["connectorBinding"], binding().to_json());
        assert_eq!(item["executionEligible"], false);
        assert!(item.get("providerStatus").is_none());
        assert!(item.get("workflow").is_none());
        assert_eq!(item["nativeVk"]["ownerId"], -123);
        assert_eq!(item["externalAliases"][0]["resourceKind"], "wall-comment");
        ResourceRef::from_item(&binding(), item).unwrap();
    }
    let request = &transport.requests.lock().unwrap()[0];
    assert_eq!(request.method(), "wall.getComments");
    assert_eq!(
        request.params(),
        json!({"v":"5.199","owner_id":-123,"post_id":7,"offset":0,"count":100,"sort":"asc","preview_length":0,"thread_items_count":10,"extended":0})
    );
    assert_eq!(result.coverage["branchContextComplete"], false);
    assert_eq!(result.coverage["snapshotStable"], false);
}
#[tokio::test]
async fn partial_and_absent_thread_previews_are_not_complete() {
    for missing in [false, true] {
        let mut page = fixture("partial");
        if missing {
            page["response"]["items"][0]["thread"]
                .as_object_mut()
                .unwrap()
                .remove("items");
        }
        let c = connector(Fixture::new(vec![page]));
        let result = c.read_snapshot(&binding(), READ_MODE, None).await.unwrap();
        assert!(!result.page.complete);
        assert!(result.page.cursor.is_none());
        assert_eq!(result.branches[0]["contextComplete"], false);
    }
}
#[tokio::test]
async fn absent_level_count_does_not_promise_account_coverage() {
    let mut page = fixture("comments");
    page["response"]
        .as_object_mut()
        .unwrap()
        .remove("current_level_count");
    let result = connector(Fixture::new(vec![page]))
        .read(&binding(), READ_MODE, None)
        .await
        .unwrap();
    assert!(!result.complete);
}
#[tokio::test]
async fn bindings_and_modes_are_checked_before_transport() {
    let transport = Fixture::new(vec![]);
    let c = connector(transport.clone());
    for key in [
        "id",
        "workspaceId",
        "accountId",
        "providerAccountId",
        "connector",
        "revision",
    ] {
        let mut value = binding().to_json();
        value[key] = match key {
            "revision" => json!(2),
            "connector" => json!("angryspace"),
            _ => json!("foreign"),
        };
        let other = ConnectorBinding::from_json(&value).unwrap();
        assert!(c.read(&other, READ_MODE, None).await.is_err());
    }
    assert!(c.read(&binding(), "open", None).await.is_err());
    assert_eq!(transport.calls(), 0);
}
#[tokio::test]
async fn source_scoped_pagination_and_changed_totals_fail_closed() {
    let transport = Fixture::new(vec![single(41, 2), single(43, 2)]);
    let c = VkConnector::new(binding(), vec![post()], 1, transport.clone()).unwrap();
    let page = c.read(&binding(), READ_MODE, None).await.unwrap();
    assert!(!page.complete);
    let cursor = page.cursor.unwrap();
    let final_page = c.read(&binding(), READ_MODE, Some(&cursor)).await.unwrap();
    assert!(final_page.complete);
    assert_eq!(transport.requests.lock().unwrap()[1].params()["offset"], 1);
    let other = VkConnector::new(
        binding(),
        vec![WallPost::new(-123, 8).unwrap()],
        1,
        transport.clone(),
    )
    .unwrap();
    assert!(
        other
            .read(&binding(), READ_MODE, Some(&cursor))
            .await
            .is_err()
    );
    assert_eq!(transport.calls(), 2);
    let altered = Fixture::new(vec![single(41, 2), single(43, 3)]);
    let c = VkConnector::new(binding(), vec![post()], 1, altered).unwrap();
    let page = c.read(&binding(), READ_MODE, None).await.unwrap();
    assert!(
        c.read(&binding(), READ_MODE, page.cursor.as_ref())
            .await
            .is_err()
    );
}
#[tokio::test]
async fn cursor_cannot_cross_revision_company_mode_or_page_size() {
    let t = Fixture::new(vec![single(41, 2)]);
    let c = VkConnector::new(binding(), vec![post()], 1, t.clone()).unwrap();
    let page = c.read(&binding(), READ_MODE, None).await.unwrap();
    let cursor = page.cursor.unwrap();
    let other = connector(t.clone());
    assert!(
        other
            .read(&binding(), READ_MODE, Some(&cursor))
            .await
            .is_err()
    );
    for field in ["revision", "accountId"] {
        let mut v = binding().to_json();
        v[field] = if field == "revision" {
            json!(2)
        } else {
            json!("BAW Russia")
        };
        let b = ConnectorBinding::from_json(&v).unwrap();
        assert!(cursor.for_request(&b, READ_MODE).is_err());
    }
    assert!(cursor.for_request(&binding(), "open").is_err());
    assert_eq!(t.calls(), 1);
}

#[tokio::test]
async fn malformed_cursor_total_and_unknown_fields_fail_before_transport() {
    let t = Fixture::new(vec![single(41, 2)]);
    let c = VkConnector::new(binding(), vec![post()], 1, t.clone()).unwrap();
    let page = c.read(&binding(), READ_MODE, None).await.unwrap();
    let cursor = page.cursor.unwrap();
    let original: Value =
        serde_json::from_str(cursor.for_request(&binding(), READ_MODE).unwrap()).unwrap();
    for (field, value) in [
        ("levelTotal", json!("2")),
        ("extra", json!(true)),
        ("offset", json!(u64::MAX)),
    ] {
        let mut payload = original.clone();
        payload[field] = value;
        let invalid =
            ProviderCursor::new(binding(), READ_MODE.into(), payload.to_string()).unwrap();
        assert!(c.read(&binding(), READ_MODE, Some(&invalid)).await.is_err());
    }
    assert_eq!(t.calls(), 1);
}
#[tokio::test]
async fn read_errors_malformed_scope_duplicates_and_deleted_flags_do_not_infer_close() {
    let mut foreign = fixture("comments");
    foreign["response"]["items"][0]["owner_id"] = json!(-999);
    let mut duplicate = fixture("comments");
    duplicate["response"]["items"][1]["id"] = json!(41);
    let mut malformed = fixture("comments");
    malformed["response"]["items"][0]["date"] = json!("yesterday");
    let mut inconsistent = fixture("comments");
    inconsistent["response"]["current_level_count"] = json!(1);
    for page in [
        foreign,
        duplicate,
        malformed,
        inconsistent,
        json!({"error":{"error_code":15,"error_msg":"Access denied"}}),
    ] {
        assert!(
            connector(Fixture::new(vec![page]))
                .read(&binding(), READ_MODE, None)
                .await
                .is_err()
        );
    }
    let mut deleted = single(41, 1);
    deleted["response"]["items"][0]["deleted"] = json!(true);
    let page = connector(Fixture::new(vec![deleted]))
        .read(&binding(), READ_MODE, None)
        .await
        .unwrap();
    assert_eq!(page.items[0]["nativeObservation"]["deleted"], true);
    assert!(page.items[0].get("workflow").is_none());
}
#[tokio::test]
async fn exact_native_context_requires_requested_recipient_and_source() {
    let target = WallComment::new(post(), 41).unwrap().resource(&binding());
    let t = Fixture::new(vec![fixture("comment")]);
    let c = connector(t.clone());
    let context = c.context(&target).await.unwrap();
    assert_eq!(context["contextComplete"], false);
    assert_eq!(t.requests.lock().unwrap()[0].method(), "wall.getComment");
    assert_eq!(
        t.requests.lock().unwrap()[0].params(),
        json!({"v":"5.199","owner_id":-123,"comment_id":41,"extended":0})
    );
    for field in ["id", "post_id"] {
        let mut page = fixture("comment");
        page["response"]["items"][0][field] = json!(999);
        assert!(
            connector(Fixture::new(vec![page]))
                .context(&target)
                .await
                .is_err()
        );
    }
    let mut wrong = target.clone();
    wrong.item_id = "41".into();
    assert!(c.context(&wrong).await.is_err());
    assert_eq!(t.calls(), 1);
}
#[tokio::test]
async fn writes_are_unavailable_and_unknown_reconciliation_never_retries() {
    let t = Fixture::new(vec![]);
    let c = connector(t.clone());
    let target = WallComment::new(post(), 41).unwrap().resource(&binding());
    for action in [
        Action::PublishReply { text: "Hi".into() },
        Action::CloseWorkItem,
        Action::DeleteComment,
        Action::HideComment,
        Action::RestoreComment,
    ] {
        assert!(!c.capabilities().supports(action.kind()));
        let route = ApprovedRoute::new(target.clone(), vec![action]).unwrap();
        let receipt = c.execute("operation", &route).await.unwrap();
        assert_eq!(receipt.operation_id, "operation");
        assert_eq!(receipt.outcome, ReceiptOutcome::Rejected);
        assert!(!receipt.requires_reconciliation());
        assert_eq!(receipt.evidence["providerCallAttempted"], false);
        let readback = c.reconcile("operation", &route).await.unwrap();
        assert_eq!(readback.operation_id, "operation");
        assert_eq!(readback.outcome, ReadbackOutcome::Unknown);
        assert_eq!(readback.route, route);
        assert!(!readback.confirms("operation", &route));
        assert_eq!(readback.evidence["providerRetryAllowed"], false);
    }
    assert_eq!(t.calls(), 0);
}
#[tokio::test]
async fn local_aliases_separate_connections_companies_and_preserve_revision_identity() {
    let mut ids = HashSet::new();
    for field in [
        None,
        Some("id"),
        Some("accountId"),
        Some("providerAccountId"),
        Some("workspaceId"),
    ] {
        let mut v = binding().to_json();
        if let Some(field) = field {
            v[field] = json!("other");
        }
        let b = ConnectorBinding::from_json(&v).unwrap();
        let c = VkConnector::new(
            b.clone(),
            vec![post()],
            100,
            Fixture::new(vec![fixture("comments")]),
        )
        .unwrap();
        let page = c.read(&b, READ_MODE, None).await.unwrap();
        assert!(ids.insert(page.items[0]["id"].clone().to_string()));
    }
    let t = Fixture::new(vec![fixture("comments")]);
    let original = connector(t)
        .read(&binding(), READ_MODE, None)
        .await
        .unwrap()
        .items[0]["id"]
        .clone();
    let mut b = binding();
    b.revision = 2;
    let c = VkConnector::new(
        b.clone(),
        vec![post()],
        100,
        Fixture::new(vec![fixture("comments")]),
    )
    .unwrap();
    assert_eq!(
        original,
        c.read(&b, READ_MODE, None).await.unwrap().items[0]["id"]
    );
}
#[test]
fn invalid_native_configuration_is_rejected() {
    for (owner, post_id) in [(0, 7), (-123, 0), (i64::MIN, 7)] {
        assert!(WallPost::new(owner, post_id).is_err());
    }
    assert!(WallComment::new(post(), 0).is_err());
    for size in [0, 101] {
        assert!(VkConnector::new(binding(), vec![post()], size, Fixture::new(vec![])).is_err());
    }
    assert!(VkConnector::new(binding(), vec![post(), post()], 100, Fixture::new(vec![])).is_err());
}

#[tokio::test]
async fn incomplete_coverage_survives_advancing_to_another_source() {
    let transport = Fixture::new(vec![
        fixture("partial"),
        json!({"response":{"count":0,"current_level_count":0,"items":[]}}),
    ]);
    let c = VkConnector::new(
        binding(),
        vec![post(), WallPost::new(-123, 8).unwrap()],
        100,
        transport.clone(),
    )
    .unwrap();
    let page = c.read(&binding(), READ_MODE, None).await.unwrap();
    let cursor = page.cursor.unwrap();
    let last = c.read(&binding(), READ_MODE, Some(&cursor)).await.unwrap();
    assert!(last.cursor.is_none());
    assert!(!last.complete);
    assert_eq!(transport.requests.lock().unwrap()[1].params()["post_id"], 8);
}

#[tokio::test]
async fn foreign_thread_and_legacy_angry_targets_fail_before_writes() {
    let mut page = fixture("comments");
    page["response"]["items"][0]["thread"]["items"][0]["parents_stack"] = json!([999]);
    assert!(
        connector(Fixture::new(vec![page]))
            .read(&binding(), READ_MODE, None)
            .await
            .is_err()
    );
    let t = Fixture::new(vec![]);
    let c = connector(t.clone());
    let mut foreign = WallComment::new(post(), 41).unwrap().resource(&binding());
    foreign.binding.connector = ConnectorKind::AngrySpace;
    let route =
        ApprovedRoute::new(foreign, vec![Action::PublishReply { text: "Hi".into() }]).unwrap();
    assert!(c.execute("old-unknown", &route).await.is_err());
    assert!(c.reconcile("old-unknown", &route).await.is_err());
    assert_eq!(t.calls(), 0);
}
