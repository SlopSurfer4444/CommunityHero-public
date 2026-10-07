//! Offline-ready VK wall reader. No HTTP, credentials, runtime routing or write authority.
//! Wire shape: VKCOM/vk-api-schema 333481bd082ad747d4873ef4a77f9247097eeef0, v5.199.
use crate::connectors::*;
use serde_json::{Value, json};
use std::collections::HashSet;

pub const API_VERSION: &str = "5.199";
pub const READ_MODE: &str = "wall-comments";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WallPost {
    pub owner_id: i64,
    pub post_id: u64,
}
impl WallPost {
    pub fn new(owner_id: i64, post_id: u64) -> Result<Self, BoundaryError> {
        if owner_id == 0 || owner_id == i64::MIN || post_id == 0 || post_id > i64::MAX as u64 {
            return Err(BoundaryError("Invalid native VK wall reference"));
        }
        Ok(Self { owner_id, post_id })
    }
}

/// Native fields are explicit; opaque ResourceRef values are never interpreted as Angry IDs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WallComment {
    pub post: WallPost,
    pub comment_id: u64,
}
impl WallComment {
    pub fn new(post: WallPost, comment_id: u64) -> Result<Self, BoundaryError> {
        WallPost::new(post.owner_id, post.post_id)?;
        if comment_id == 0 || comment_id > i64::MAX as u64 {
            return Err(BoundaryError("Invalid native VK comment reference"));
        }
        Ok(Self { post, comment_id })
    }
    pub fn resource(self, binding: &ConnectorBinding) -> ResourceRef {
        ResourceRef {
            binding: binding.clone(),
            object_id: format!("vk:wall:{}:{}", self.post.owner_id, self.post.post_id),
            item_id: format!("vk:comment:{}:{}", self.post.owner_id, self.comment_id),
            post_key: format!("vk:wall:{}:{}", self.post.owner_id, self.post.post_id),
            conversation_key: format!("vk:wall:{}:{}", self.post.owner_id, self.post.post_id),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ReadRequest {
    GetComments {
        post: WallPost,
        offset: u64,
        count: u16,
    },
    GetComment {
        target: WallComment,
    },
}
impl ReadRequest {
    pub fn method(&self) -> &'static str {
        match self {
            Self::GetComments { .. } => "wall.getComments",
            Self::GetComment { .. } => "wall.getComment",
        }
    }
    pub fn params(&self) -> Value {
        match self {
            Self::GetComments {
                post,
                offset,
                count,
            } => json!({"v":API_VERSION,"owner_id":post.owner_id,"post_id":post.post_id,
                "offset":offset,"count":count,"sort":"asc","preview_length":0,"thread_items_count":10,"extended":0}),
            Self::GetComment { target } => {
                json!({"v":API_VERSION,"owner_id":target.post.owner_id,"comment_id":target.comment_id,"extended":0})
            }
        }
    }
}
/// Implementors supply only read methods. Tokens and HTTP retry policy stay outside core.
pub trait ReadTransport: Send + Sync {
    fn request<'a>(
        &'a self,
        binding: &'a ConnectorBinding,
        request: ReadRequest,
    ) -> ConnectorFuture<'a, Value>;
}
pub struct VkConnector<T> {
    binding: ConnectorBinding,
    posts: Vec<WallPost>,
    page_size: u16,
    transport: T,
}
pub struct ReadSnapshot {
    pub page: ProviderPage,
    pub posts: Vec<Value>,
    pub branches: Vec<Value>,
    pub coverage: Value,
}
impl<T: ReadTransport> VkConnector<T> {
    pub fn new(
        binding: ConnectorBinding,
        posts: Vec<WallPost>,
        page_size: u16,
        transport: T,
    ) -> Result<Self, BoundaryError> {
        binding.validate_scope(&binding.workspace_id, &binding.account_id)?;
        if binding.connector != ConnectorKind::Vk
            || posts.is_empty()
            || posts.len() > 1000
            || !(1..=100).contains(&page_size)
        {
            return Err(BoundaryError("Invalid native VK reader configuration"));
        }
        let mut seen = HashSet::new();
        for post in &posts {
            WallPost::new(post.owner_id, post.post_id)?;
            if !seen.insert((post.owner_id, post.post_id)) {
                return Err(BoundaryError("Duplicate native VK source"));
            }
        }
        Ok(Self {
            binding,
            posts,
            page_size,
            transport,
        })
    }
    fn scope(&self, binding: &ConnectorBinding) -> Result<(), BoundaryError> {
        if binding != &self.binding {
            return Err(BoundaryError("Native VK binding mismatch"));
        }
        Ok(())
    }
    fn sources(&self) -> Value {
        json!(
            self.posts
                .iter()
                .map(|p| json!([p.owner_id, p.post_id]))
                .collect::<Vec<_>>()
        )
    }
    fn local_id(&self, kind: &str, native: Value) -> String {
        use sha2::{Digest, Sha256};
        // An external alias is scoped by stable connection identity, not configuration revision.
        let scope = json!([
            self.binding.workspace_id,
            self.binding.account_id,
            self.binding.id,
            self.binding.connector.as_str(),
            self.binding.provider_account_id,
            kind,
            native
        ]);
        format!(
            "native-vk-{kind}-{:x}",
            Sha256::digest(scope.to_string().as_bytes())
        )
    }
    fn target(&self, target: &ResourceRef) -> Result<WallComment, BoundaryError> {
        self.scope(&target.binding)?;
        for post in &self.posts {
            let prefix = format!("vk:comment:{}:", post.owner_id);
            if let Some(id) = target
                .item_id
                .strip_prefix(&prefix)
                .and_then(|s| s.parse::<u64>().ok())
            {
                let reference = WallComment::new(*post, id)?;
                if reference.resource(&self.binding) == *target {
                    return Ok(reference);
                }
            }
        }
        Err(BoundaryError(
            "Native VK target is outside configured source scope",
        ))
    }
    pub async fn read_snapshot(
        &self,
        binding: &ConnectorBinding,
        mode: &str,
        cursor: Option<&ProviderCursor>,
    ) -> Result<ReadSnapshot, BoundaryError> {
        self.scope(binding)?;
        if mode != READ_MODE {
            return Err(BoundaryError("Unsupported native VK read mode"));
        }
        let (index, offset, prior_incomplete, prior_total) = if let Some(cursor) = cursor {
            let payload: Value = serde_json::from_str(cursor.for_request(binding, mode)?)
                .map_err(|_| BoundaryError("Invalid native VK cursor"))?;
            if payload.as_object().is_none_or(|object| {
                object.len() != 7
                    || object.keys().any(|key| {
                        ![
                            "version",
                            "sources",
                            "pageSize",
                            "sourceIndex",
                            "offset",
                            "incomplete",
                            "levelTotal",
                        ]
                        .contains(&key.as_str())
                    })
            }) || (!payload["levelTotal"].is_null() && payload["levelTotal"].as_u64().is_none())
                || payload["version"] != 1
                || payload["sources"] != self.sources()
                || payload["pageSize"] != self.page_size
            {
                return Err(BoundaryError("Native VK cursor source scope changed"));
            }
            let index = payload["sourceIndex"]
                .as_u64()
                .filter(|i| *i < self.posts.len() as u64)
                .ok_or(BoundaryError("Invalid native VK cursor source"))?
                as usize;
            (
                index,
                payload["offset"]
                    .as_u64()
                    .filter(|n| *n <= i64::MAX as u64)
                    .ok_or(BoundaryError("Invalid native VK cursor offset"))?,
                payload["incomplete"]
                    .as_bool()
                    .ok_or(BoundaryError("Invalid native VK cursor completeness"))?,
                payload["levelTotal"].as_u64(),
            )
        } else {
            (0, 0, false, None)
        };
        let post = self.posts[index];
        let response = self
            .transport
            .request(
                binding,
                ReadRequest::GetComments {
                    post,
                    offset,
                    count: self.page_size,
                },
            )
            .await?;
        let response = response_body(&response)?;
        let total = response["count"]
            .as_u64()
            .ok_or(BoundaryError("Invalid native VK page count"))?;
        let level_total = optional_uint(response, "current_level_count")?;
        if prior_total.is_some() && prior_total != level_total {
            return Err(BoundaryError(
                "Native VK pagination changed; restart scoped read",
            ));
        }
        let rows = response["items"]
            .as_array()
            .ok_or(BoundaryError("Invalid native VK page items"))?;
        if rows.len() > self.page_size as usize
            || total < rows.len() as u64
            || level_total.is_some_and(|n| n > total || offset + rows.len() as u64 > n)
        {
            return Err(BoundaryError("Inconsistent native VK page coverage"));
        }
        let mut items = Vec::new();
        let mut branches = Vec::new();
        let mut seen = HashSet::new();
        let mut incomplete = prior_incomplete;
        for row in rows {
            let root = self.map_comment(post, row)?;
            let root_id = root["nativeVk"]["commentId"].as_u64().unwrap();
            if !seen.insert(root_id) {
                return Err(BoundaryError("Duplicate native VK comment"));
            }
            let mut messages = vec![root.clone()];
            if let Some(thread) = row.get("thread") {
                let count = thread["count"]
                    .as_u64()
                    .ok_or(BoundaryError("Invalid native VK thread count"))?;
                let empty = Vec::new();
                let replies = match thread.get("items") {
                    None => &empty,
                    Some(v) => v
                        .as_array()
                        .ok_or(BoundaryError("Invalid native VK thread items"))?,
                };
                if replies.len() > 10 || replies.len() as u64 > count {
                    return Err(BoundaryError("Inconsistent native VK thread coverage"));
                }
                incomplete |= replies.len() as u64 != count;
                for reply in replies {
                    let mapped = self.map_comment(post, reply)?;
                    let id = mapped["nativeVk"]["commentId"].as_u64().unwrap();
                    if mapped["nativeVk"]["parentsStack"]
                        .as_array()
                        .and_then(|parents| parents.first())
                        .is_some_and(|parent| parent.as_u64() != Some(root_id))
                        || (mapped["nativeVk"]["parentsStack"]
                            .as_array()
                            .is_some_and(Vec::is_empty)
                            && reply
                                .get("reply_to_comment")
                                .is_some_and(|parent| parent.as_u64() != Some(root_id)))
                    {
                        return Err(BoundaryError("Native VK preview belongs to another thread"));
                    }
                    if !seen.insert(id) {
                        return Err(BoundaryError("Duplicate native VK thread comment"));
                    }
                    messages.push(mapped.clone());
                    items.push(mapped);
                }
            } else {
                incomplete = true;
            }
            branches.push(json!({"id":root["branchId"],"postId":root["postId"],"connectorBinding":binding.to_json(),"messages":messages,
                "contextComplete":false,"contextTruncated":true,"unavailableReason":"Read observation only; full branch admission is not implemented."}));
            items.push(root);
        }
        let end = offset
            .checked_add(rows.len() as u64)
            .ok_or(BoundaryError("Native VK offset overflow"))?;
        let exhausted =
            level_total.is_some_and(|n| end == n) || rows.len() < self.page_size as usize;
        if !exhausted && rows.is_empty() {
            return Err(BoundaryError("Native VK pagination made no progress"));
        }
        // count may include replies; only current_level_count establishes root coverage.
        incomplete |= level_total.is_none() && total != 0;
        if exhausted && level_total.is_some_and(|n| end != n) {
            incomplete = true;
        }
        let next = if !exhausted {
            Some((index, end, level_total))
        } else if index + 1 < self.posts.len() {
            Some((index + 1, 0, None))
        } else {
            None
        };
        let cursor = next
            .map(|(i, o, t)| {
                ProviderCursor::new(
                    binding.clone(),
                    mode.into(),
                    json!({"version":1,"sources":self.sources(),"pageSize":self.page_size,
            "sourceIndex":i,"offset":o,"incomplete":incomplete,"levelTotal":t})
                    .to_string(),
                )
            })
            .transpose()?;
        let complete = cursor.is_none() && !incomplete;
        let post_key = format!("vk:wall:{}:{}", post.owner_id, post.post_id);
        let post_id = self.local_id("post", json!([post.owner_id, post.post_id]));
        Ok(ReadSnapshot {
            page: ProviderPage {
                items,
                cursor,
                complete,
            },
            posts: vec![
                json!({"id":post_id,"postKey":post_key,"connectorBinding":binding.to_json(),
            "sourceUrl":format!("https://vk.com/wall{}_{}",post.owner_id,post.post_id),"sourceContentObserved":false}),
            ],
            branches,
            coverage: json!({"scope":"configured-wall-posts","sourceIndex":index,"offset":offset,"observedRootCount":rows.len(),"reportedCount":total,
                "currentLevelCount":level_total,"complete":complete,"branchContextComplete":false,"snapshotStable":false,"readOnly":true}),
        })
    }
    fn map_comment(&self, post: WallPost, row: &Value) -> Result<Value, BoundaryError> {
        let id = row["id"]
            .as_u64()
            .ok_or(BoundaryError("Missing native VK comment id"))?;
        let target = WallComment::new(post, id)?;
        if row
            .get("owner_id")
            .is_some_and(|v| v.as_i64() != Some(post.owner_id))
            || row
                .get("post_id")
                .is_some_and(|v| v.as_u64() != Some(post.post_id))
        {
            return Err(BoundaryError("Native VK response source mismatch"));
        }
        let author = row["from_id"]
            .as_i64()
            .filter(|n| *n != 0 && *n != i64::MIN)
            .ok_or(BoundaryError("Missing native VK author"))?;
        let date = row["date"]
            .as_i64()
            .filter(|n| *n >= 0)
            .ok_or(BoundaryError("Invalid native VK comment date"))?;
        let text = row["text"]
            .as_str()
            .ok_or(BoundaryError("Missing native VK comment text"))?;
        let parents = if let Some(value) = row.get("parents_stack") {
            let rows = value
                .as_array()
                .ok_or(BoundaryError("Invalid native VK parents"))?;
            for parent in rows {
                if parent
                    .as_u64()
                    .is_none_or(|n| n == 0 || n > i64::MAX as u64 || n == id)
                {
                    return Err(BoundaryError("Invalid native VK parent id"));
                }
            }
            rows.clone()
        } else {
            vec![]
        };
        let parent = optional_uint(row, "reply_to_comment")?;
        if parent.is_some_and(|n| n == 0 || n > i64::MAX as u64 || n == id) {
            return Err(BoundaryError("Invalid native VK reply parent"));
        }
        let deleted = match row.get("deleted") {
            None => false,
            Some(v) => v
                .as_bool()
                .ok_or(BoundaryError("Invalid native VK deleted flag"))?,
        };
        let reference = target.resource(&self.binding);
        let local = self.local_id("item", json!([post.owner_id, post.post_id, id]));
        let root = parents
            .first()
            .and_then(Value::as_u64)
            .or(parent)
            .unwrap_or(id);
        Ok(
            json!({"id":local,"connectorBinding":self.binding.to_json(),"objectId":reference.object_id,"itemId":reference.item_id,
            "postKey":reference.post_key,"conversationKey":reference.conversation_key,"postId":self.local_id("post",json!([post.owner_id,post.post_id])),
            "branchId":self.local_id("branch",json!([post.owner_id,post.post_id,root])),"providerItemId":reference.item_id,"providerObjectId":reference.object_id,
            "replyToProviderItemId":parent.map(|n|format!("vk:comment:{}:{n}",post.owner_id)),"text":text,"authorId":format!("vk:author:{author}"),
            "createdAt":chrono::DateTime::from_timestamp(date,0).ok_or(BoundaryError("Invalid native VK date range"))?.to_rfc3339(),
            "platform":"VK","nativeObservation":{"deleted":deleted},"readOnly":true,"executionEligible":false,
            "externalAliases":[{"connectorBinding":self.binding.to_json(),"resourceKind":"wall-comment","ownerId":post.owner_id,"postId":post.post_id,"commentId":id}],
            "nativeVk":{"ownerId":post.owner_id,"postId":post.post_id,"commentId":id,"parentsStack":parents},
            "sourceUrl":format!("https://vk.com/wall{}_{}?reply={id}",post.owner_id,post.post_id)}),
        )
    }
}
fn response_body(value: &Value) -> Result<&Value, BoundaryError> {
    if value.get("error").is_some() {
        return Err(BoundaryError(
            "Native VK read rejected; no source absence inferred",
        ));
    }
    value
        .get("response")
        .filter(|v| v.is_object())
        .ok_or(BoundaryError("Invalid native VK response envelope"))
}
fn optional_uint(value: &Value, key: &str) -> Result<Option<u64>, BoundaryError> {
    value
        .get(key)
        .map(|v| {
            v.as_u64()
                .ok_or(BoundaryError("Invalid native VK integer field"))
        })
        .transpose()
}
impl<T: ReadTransport> Connector for VkConnector<T> {
    fn kind(&self) -> ConnectorKind {
        ConnectorKind::Vk
    }
    fn capabilities(&self) -> Capabilities {
        let mut caps = Capabilities::unknown(ConnectorKind::Vk);
        caps.close_work_item = Support::Unsupported;
        caps
    }
    fn read<'a>(
        &'a self,
        binding: &'a ConnectorBinding,
        mode: &'a str,
        cursor: Option<&'a ProviderCursor>,
    ) -> ConnectorFuture<'a, ProviderPage> {
        Box::pin(async move { Ok(self.read_snapshot(binding, mode, cursor).await?.page) })
    }
    fn context<'a>(&'a self, target: &'a ResourceRef) -> ConnectorFuture<'a, Value> {
        Box::pin(async move {
            let native = self.target(target)?;
            let response = self
                .transport
                .request(&self.binding, ReadRequest::GetComment { target: native })
                .await?;
            let response = response_body(&response)?;
            let rows = response["items"]
                .as_array()
                .filter(|rows| rows.len() == 1)
                .ok_or(BoundaryError("Native VK exact comment not observed"))?;
            let item = self.map_comment(native.post, &rows[0])?;
            if item["nativeVk"]["commentId"] != native.comment_id {
                return Err(BoundaryError("Native VK context recipient mismatch"));
            }
            Ok(
                json!({"connectorBinding":self.binding.to_json(),"item":item,"contextComplete":false,"readOnly":true,"executionEligible":false}),
            )
        })
    }
    fn execute<'a>(
        &'a self,
        operation_id: &'a str,
        route: &'a ApprovedRoute,
    ) -> ConnectorFuture<'a, ActionReceipt> {
        Box::pin(async move {
            self.target(&route.target)?;
            if operation_id.trim().is_empty() {
                return Err(BoundaryError("Missing native VK operation id"));
            }
            Ok(ActionReceipt {
                operation_id: operation_id.into(),
                outcome: ReceiptOutcome::Rejected,
                evidence: json!({"reason":"native_vk_writes_not_enabled","providerCallAttempted":false,"providerRetryAllowed":false}),
            })
        })
    }
    fn reconcile<'a>(
        &'a self,
        operation_id: &'a str,
        route: &'a ApprovedRoute,
    ) -> ConnectorFuture<'a, Readback> {
        Box::pin(async move {
            self.target(&route.target)?;
            if operation_id.trim().is_empty() {
                return Err(BoundaryError("Missing native VK operation id"));
            }
            Ok(Readback {
                operation_id: operation_id.into(),
                route: route.clone(),
                outcome: ReadbackOutcome::Unknown,
                evidence: json!({"reason":"native_vk_operation_readback_unavailable","providerCallAttempted":false,"providerRetryAllowed":false}),
            })
        })
    }
}
#[cfg(test)]
#[path = "native_vk_tests.rs"]
mod tests;
