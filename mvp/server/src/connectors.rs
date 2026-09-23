//! Provider-neutral routing contract. This module performs no I/O and grants no live authority.
//! A binding identifies a connection, not the social platform shown on a comment.
use serde_json::{Value, json};
use std::{fmt, future::Future, pin::Pin};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConnectorKind {
    AngrySpace,
    Vk,
    Instagram,
    Youtube,
    TikTok,
}

impl ConnectorKind {
    pub fn parse(value: &str) -> Result<Self, BoundaryError> {
        match value {
            "angryspace" => Ok(Self::AngrySpace),
            "vk" => Ok(Self::Vk),
            "instagram" => Ok(Self::Instagram),
            "youtube" => Ok(Self::Youtube),
            "tiktok" => Ok(Self::TikTok),
            _ => Err(BoundaryError("Unknown connector")),
        }
    }
    pub fn as_str(self) -> &'static str {
        match self {
            Self::AngrySpace => "angryspace",
            Self::Vk => "vk",
            Self::Instagram => "instagram",
            Self::Youtube => "youtube",
            Self::TikTok => "tiktok",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BoundaryError(pub &'static str);
impl fmt::Display for BoundaryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.0)
    }
}
impl std::error::Error for BoundaryError {}
fn field(value: &Value, key: &str) -> Result<String, BoundaryError> {
    value[key]
        .as_str()
        .filter(|s| !s.trim().is_empty() && s.len() <= 4096)
        .map(str::to_owned)
        .ok_or(BoundaryError("Missing or invalid routing field"))
}

/// Increment revision when the server changes a connection's configuration or ownership.
/// Persist this entire snapshot with approval; never resolve an old approval by id alone.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConnectorBinding {
    pub id: String,
    pub workspace_id: String,
    pub account_id: String,
    pub connector: ConnectorKind,
    pub revision: u64,
    pub provider_account_id: String,
}
impl ConnectorBinding {
    pub fn from_json(value: &Value) -> Result<Self, BoundaryError> {
        let binding = Self {
            id: field(value, "id")?,
            workspace_id: field(value, "workspaceId")?,
            account_id: field(value, "accountId")?,
            connector: ConnectorKind::parse(&field(value, "connector")?)?,
            revision: value["revision"]
                .as_u64()
                .filter(|n| *n > 0)
                .ok_or(BoundaryError("Invalid binding revision"))?,
            provider_account_id: field(value, "providerAccountId")?,
        };
        Ok(binding)
    }
    pub fn to_json(&self) -> Value {
        json!({"id":self.id,"workspaceId":self.workspace_id,"accountId":self.account_id,
            "connector":self.connector.as_str(),"revision":self.revision,"providerAccountId":self.provider_account_id})
    }
    pub fn validate_scope(&self, workspace: &str, account: &str) -> Result<(), BoundaryError> {
        if self.workspace_id != workspace || self.account_id != account {
            return Err(BoundaryError("Connector workspace or account mismatch"));
        }
        // Also validate programmatically constructed bindings.
        Self::from_json(&self.to_json()).map(|_| ())
    }
}

/// Provider resource identifiers are opaque; no VK/Instagram identifiers are derived
/// from Angry.Space ids. Native adapters must supply their own references.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResourceRef {
    pub binding: ConnectorBinding,
    pub object_id: String,
    pub item_id: String,
    pub post_key: String,
    pub conversation_key: String,
}
impl ResourceRef {
    pub fn from_item(binding: &ConnectorBinding, item: &Value) -> Result<Self, BoundaryError> {
        binding.validate_scope(&binding.workspace_id, &binding.account_id)?;
        // Imported records must carry an explicit server-owned binding snapshot.
        if ConnectorBinding::from_json(&item["connectorBinding"])? != *binding {
            return Err(BoundaryError("Resource binding mismatch"));
        }
        Ok(Self {
            binding: binding.clone(),
            object_id: field(item, "objectId")?,
            item_id: field(item, "itemId")?,
            post_key: field(item, "postKey")?,
            conversation_key: field(item, "conversationKey")?,
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ActionKind {
    PublishReply,
    CloseWorkItem,
    HideComment,
    DeleteComment,
    RestoreComment,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Action {
    PublishReply { text: String },
    CloseWorkItem,
    HideComment,
    DeleteComment,
    RestoreComment,
}
impl Action {
    pub fn kind(&self) -> ActionKind {
        match self {
            Self::PublishReply { .. } => ActionKind::PublishReply,
            Self::CloseWorkItem => ActionKind::CloseWorkItem,
            Self::HideComment => ActionKind::HideComment,
            Self::DeleteComment => ActionKind::DeleteComment,
            Self::RestoreComment => ActionKind::RestoreComment,
        }
    }
}
/// Unsupported and unknown both fail closed. Restore is always explicit, never inferred
/// from delete support. Capabilities are supplied by the adapter, never by an HTTP caller.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Support {
    Supported,
    Unsupported,
    Unknown,
}
#[derive(Clone, Debug)]
pub struct Capabilities {
    pub connector: ConnectorKind,
    pub publish_reply: Support,
    pub close_work_item: Support,
    pub hide_comment: Support,
    pub delete_comment: Support,
    pub restore_comment: Support,
}
impl Capabilities {
    pub fn unknown(connector: ConnectorKind) -> Self {
        Self {
            connector,
            publish_reply: Support::Unknown,
            close_work_item: Support::Unknown,
            hide_comment: Support::Unknown,
            delete_comment: Support::Unknown,
            restore_comment: Support::Unknown,
        }
    }
    /// Contract of the existing bridge, not a claim about all Angry.Space API operations.
    pub fn angryspace_bridge() -> Self {
        Self {
            connector: ConnectorKind::AngrySpace,
            publish_reply: Support::Supported,
            close_work_item: Support::Supported,
            hide_comment: Support::Unsupported,
            delete_comment: Support::Unsupported,
            restore_comment: Support::Unsupported,
        }
    }
    pub fn supports(&self, action: ActionKind) -> bool {
        (match action {
            ActionKind::PublishReply => self.publish_reply,
            ActionKind::CloseWorkItem => self.close_work_item,
            ActionKind::HideComment => self.hide_comment,
            ActionKind::DeleteComment => self.delete_comment,
            ActionKind::RestoreComment => self.restore_comment,
        }) == Support::Supported
    }
    /// Verified compact-provider operations, additionally gated by the source
    /// platform. Restore/unhide are not inferred from delete/hide support.
    pub fn angryspace_for_platform(platform: &str) -> Self {
        let mut caps=Self::angryspace_bridge();
        match platform.to_ascii_lowercase().as_str() {
            "tiktok" => caps.hide_comment=Support::Supported,
            "vk"|"vkontakte"|"instagram"|"youtube" => caps.delete_comment=Support::Supported,
            _ => (),
        }
        caps
    }
}

/// The old bridge's combined operation is explicitly two effects; closing a work item
/// never means deleting the social comment. Native connectors need not support close.
pub fn legacy_actions(kind: &str, text: &str) -> Result<Vec<Action>, BoundaryError> {
    match kind {
        "close" => Ok(vec![Action::CloseWorkItem]),
        "delete" => Ok(vec![Action::DeleteComment]),
        "hide" => Ok(vec![Action::HideComment]),
        "reply_and_close" if !text.trim().is_empty() && text.len() <= 20000 => Ok(vec![
            Action::PublishReply { text: text.into() },
            Action::CloseWorkItem,
        ]),
        _ => Err(BoundaryError("Unsupported or invalid legacy action")),
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ApprovedRoute {
    pub target: ResourceRef,
    pub actions: Vec<Action>,
}
impl ApprovedRoute {
    pub fn new(target: ResourceRef, actions: Vec<Action>) -> Result<Self, BoundaryError> {
        if actions.is_empty() || actions.iter().any(|a| matches!(a, Action::PublishReply { text } if text.trim().is_empty() || text.len() > 20000)) {
            return Err(BoundaryError("Empty or invalid action plan"));
        }
        Ok(Self { target, actions })
    }
    pub fn validate_dispatch(
        &self,
        current_binding: &ConnectorBinding,
        current_target: &ResourceRef,
        capabilities: &Capabilities,
    ) -> Result<(), BoundaryError> {
        // Public structs can also be assembled by adapters; validate the plan at use.
        Self::new(self.target.clone(), self.actions.clone())?;
        if self.target.binding != *current_binding || self.target != *current_target {
            return Err(BoundaryError(
                "Approved connector route changed; new approval required",
            ));
        }
        if capabilities.connector != current_binding.connector
            || self
                .actions
                .iter()
                .any(|a| !capabilities.supports(a.kind()))
        {
            return Err(BoundaryError("Connector action capability unavailable"));
        }
        current_binding.validate_scope(
            &self.target.binding.workspace_id,
            &self.target.binding.account_id,
        )
    }
}

/// Payload is deliberately opaque to orchestration, and may only be passed back to
/// the same connection revision and queue mode. Do not put tokens in cursor payloads.
#[derive(Clone, PartialEq, Eq)]
pub struct ProviderCursor {
    binding: ConnectorBinding,
    mode: String,
    opaque: String,
}
impl ProviderCursor {
    pub fn new(
        binding: ConnectorBinding,
        mode: String,
        opaque: String,
    ) -> Result<Self, BoundaryError> {
        binding.validate_scope(&binding.workspace_id, &binding.account_id)?;
        if mode.is_empty() || opaque.is_empty() || opaque.len() > 16000 {
            return Err(BoundaryError("Invalid cursor"));
        }
        Ok(Self {
            binding,
            mode,
            opaque,
        })
    }
    pub fn for_request(
        &self,
        binding: &ConnectorBinding,
        mode: &str,
    ) -> Result<&str, BoundaryError> {
        if self.binding != *binding || self.mode != mode {
            return Err(BoundaryError("Cursor scope mismatch"));
        }
        Ok(&self.opaque)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReceiptOutcome {
    Accepted,
    Rejected,
    Unknown,
}
pub struct ActionReceipt {
    pub operation_id: String,
    pub outcome: ReceiptOutcome,
    pub evidence: Value,
}
impl ActionReceipt {
    /// Even Accepted is only a receipt. Only adapter readback may verify the effects.
    pub fn requires_reconciliation(&self) -> bool {
        self.outcome != ReceiptOutcome::Rejected
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReadbackOutcome {
    Verified,
    NotApplied,
    Unknown,
}
pub struct Readback {
    pub operation_id: String,
    pub route: ApprovedRoute,
    pub outcome: ReadbackOutcome,
    pub evidence: Value,
}
impl Readback {
    pub fn confirms(&self, operation_id: &str, route: &ApprovedRoute) -> bool {
        self.operation_id == operation_id
            && self.route == *route
            && self.outcome == ReadbackOutcome::Verified
    }
}
pub struct ProviderPage {
    pub items: Vec<Value>,
    pub cursor: Option<ProviderCursor>,
    pub complete: bool,
}
pub type ConnectorFuture<'a, T> =
    Pin<Box<dyn Future<Output = Result<T, BoundaryError>> + Send + 'a>>;

/// Native implementations and a transitional bridge can implement the same interface.
/// Transport uncertainty after dispatch MUST produce ReceiptOutcome::Unknown, not a
/// retryable pre-dispatch error. There is intentionally no automatic retry method.
pub trait Connector: Send + Sync {
    fn kind(&self) -> ConnectorKind;
    fn capabilities(&self) -> Capabilities;
    fn read<'a>(
        &'a self,
        binding: &'a ConnectorBinding,
        mode: &'a str,
        cursor: Option<&'a ProviderCursor>,
    ) -> ConnectorFuture<'a, ProviderPage>;
    fn context<'a>(&'a self, target: &'a ResourceRef) -> ConnectorFuture<'a, Value>;
    fn execute<'a>(
        &'a self,
        operation_id: &'a str,
        route: &'a ApprovedRoute,
    ) -> ConnectorFuture<'a, ActionReceipt>;
    fn reconcile<'a>(
        &'a self,
        operation_id: &'a str,
        route: &'a ApprovedRoute,
    ) -> ConnectorFuture<'a, Readback>;
}

#[cfg(test)]
mod tests {
    use super::*;
    fn binding() -> ConnectorBinding {
        ConnectorBinding::from_json(&json!({"id":"connection-1","workspaceId":"workspace-1","accountId":"likeavto","connector":"angryspace","revision":1,"providerAccountId":"provider-account-1"})).unwrap()
    }
    fn target(binding: &ConnectorBinding) -> ResourceRef {
        ResourceRef::from_item(binding, &json!({"connectorBinding":binding.to_json(),"objectId":"11391","itemId":"123","postKey":"11391:post","conversationKey":"11391:thread"})).unwrap()
    }
    fn route() -> ApprovedRoute {
        ApprovedRoute::new(
            target(&binding()),
            legacy_actions("reply_and_close", "hello").unwrap(),
        )
        .unwrap()
    }
    #[test]
    fn account_and_provider_mix_is_rejected() {
        let binding = binding();
        assert!(binding.validate_scope("workspace-2", "likeavto").is_err());
        assert!(binding.validate_scope("workspace-1", "baw").is_err());
        let mut item = json!({"connectorBinding":binding.to_json(),"objectId":"11391","itemId":"123","postKey":"post","conversationKey":"thread"});
        item["connectorBinding"]["connector"] = json!("vk");
        assert!(ResourceRef::from_item(&binding, &item).is_err());
    }
    #[test]
    fn changes_to_binding_invalidate_approved_route() {
        let approved = route();
        for changed in [
            "connector",
            "revision",
            "providerAccountId",
            "id",
            "accountId",
            "workspaceId",
        ] {
            let mut value = binding().to_json();
            value[changed] = match changed {
                "connector" => json!("vk"),
                "revision" => json!(2),
                _ => json!("different"),
            };
            let current = ConnectorBinding::from_json(&value).unwrap();
            assert!(
                approved
                    .validate_dispatch(
                        &current,
                        &target(&current),
                        &Capabilities::angryspace_bridge()
                    )
                    .is_err(),
                "{changed}"
            );
        }
    }
    #[test]
    fn unsupported_moderation_is_not_workflow_close() {
        let b = binding();
        let caps = Capabilities::angryspace_bridge();
        assert!(route().validate_dispatch(&b, &target(&b), &caps).is_ok());
        for action in [
            Action::HideComment,
            Action::DeleteComment,
            Action::RestoreComment,
        ] {
            let approved = ApprovedRoute::new(target(&b), vec![action]).unwrap();
            assert!(approved.validate_dispatch(&b, &target(&b), &caps).is_err());
        }
        let mut native = Capabilities::unknown(ConnectorKind::Vk);
        native.delete_comment = Support::Supported;
        assert!(!native.supports(ActionKind::RestoreComment));
    }
    #[test]
    fn cursors_cannot_cross_account_connector_or_mode() {
        let b = binding();
        let cursor = ProviderCursor::new(b.clone(), "open".into(), "opaque/value".into()).unwrap();
        assert_eq!(cursor.for_request(&b, "open").unwrap(), "opaque/value");
        assert!(cursor.for_request(&b, "closed").is_err());
        let mut other = b.clone();
        other.account_id = "baw".into();
        assert!(cursor.for_request(&other, "open").is_err());
        other = b;
        other.connector = ConnectorKind::Vk;
        assert!(cursor.for_request(&other, "open").is_err());
    }
    #[test]
    fn unknown_and_receipt_acceptance_do_not_confirm_success() {
        for outcome in [ReceiptOutcome::Unknown, ReceiptOutcome::Accepted] {
            assert!(
                ActionReceipt {
                    operation_id: "op".into(),
                    outcome,
                    evidence: Value::Null
                }
                .requires_reconciliation()
            );
        }
        let mut readback = Readback {
            operation_id: "op".into(),
            route: route(),
            outcome: ReadbackOutcome::Unknown,
            evidence: Value::Null,
        };
        assert!(!readback.confirms("op", &route()));
        readback.outcome = ReadbackOutcome::Verified;
        assert!(readback.confirms("op", &route()));
        assert!(!readback.confirms("other", &route()));
        let mut other_route = route();
        other_route.actions = vec![Action::CloseWorkItem];
        assert!(!readback.confirms("op", &other_route));
    }
}
