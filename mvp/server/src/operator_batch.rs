//! Exact, replayable operator proposal creation. This never approves or dispatches.
use crate::{
    ApiResult, App, Json, Path, State, bad, conflict, create_proposal, internal, list, list_mut,
    now, operator_auth::Actor,
};
use axum::Extension;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::HashSet;

const MAX_PROPOSALS: usize = 100;
const MAX_REQUEST_BYTES: usize = 2_000_000;
const ACTION: &str = "proposal.batch_created";

pub(crate) fn receipt_id(key: &str) -> String {
    format!(
        "proposal-batch:{:x}",
        Sha256::digest(format!("{ACTION}\0{key}").as_bytes())
    )
}

fn request_id(value: &Value) -> ApiResult<&str> {
    value
        .as_str()
        .filter(|s| !s.is_empty() && s.len() <= 160 && !s.chars().any(char::is_control))
        .ok_or_else(|| bad("Invalid proposal batch requestId"))
}

fn request_hash(account: &Value, actor: &Actor, proposals: &Value) -> String {
    let canonical = json!({"account":account,"actorId":actor.id,"proposals":proposals});
    format!("{:x}", Sha256::digest(canonical.to_string().as_bytes()))
}

pub(crate) fn find_receipt<'a>(d: &'a Value, key: &str) -> ApiResult<Option<&'a Value>> {
    let receipt_id = receipt_id(key);
    let mut matches = list(d, "audit").iter().filter(|record| {
        record["id"] == receipt_id || (record["action"] == ACTION && record["refId"] == key)
    });
    let first = matches.next();
    if matches.next().is_some() {
        return Err(internal("Duplicate proposal batch receipt"));
    }
    if first.is_some_and(|record| record["action"] != ACTION || record["refId"] != key) {
        return Err(internal("Proposal batch receipt ID collision"));
    }
    Ok(first)
}

fn saved_result(receipt: &Value, actor: &Actor) -> ApiResult<Value> {
    if receipt["actorId"] != actor.id {
        return Err(conflict(
            "Proposal batch requestId belongs to another operator",
        ));
    }
    let mut response = receipt["result"].clone();
    if !response.is_object() {
        return Err(internal("Invalid proposal batch receipt"));
    }
    response["replayed"] = json!(true);
    Ok(response)
}

pub(crate) fn create_proposals(d: &mut Value, body: &Value, actor: &Actor) -> ApiResult<Value> {
    let object = body
        .as_object()
        .ok_or_else(|| bad("Proposal batch must be an object"))?;
    if object.len() != 2 || !object.contains_key("requestId") || !object.contains_key("proposals") {
        return Err(bad("Proposal batch requires only requestId and proposals"));
    }
    let key = request_id(&body["requestId"])?;
    let proposals = body["proposals"]
        .as_array()
        .filter(|entries| !entries.is_empty() && entries.len() <= MAX_PROPOSALS)
        .ok_or_else(|| bad("Proposal batch requires 1 to 100 proposals"))?;
    let bytes = serde_json::to_vec(body).map_err(|_| bad("Invalid proposal batch"))?;
    if bytes.len() > MAX_REQUEST_BYTES {
        return Err(bad("Proposal batch is too large"));
    }
    let hash = request_hash(&d["account"], actor, &body["proposals"]);
    if let Some(receipt) = find_receipt(d, key)? {
        if receipt["requestHash"] != hash {
            return Err(conflict(
                "Proposal batch requestId reused with different request",
            ));
        }
        return saved_result(receipt, actor);
    }

    let mut seen = HashSet::new();
    let mut results = Vec::with_capacity(proposals.len());
    let mut created = 0;
    let mut existing = 0;
    let mut rejected = 0;
    for (index, entry) in proposals.iter().enumerate() {
        let item_id = entry["itemId"].as_str().map(str::to_owned);
        let result = if !entry.is_object() {
            Err(bad("Proposal entry must be an object"))
        } else if item_id
            .as_ref()
            .is_some_and(|item| !seen.insert(item.clone()))
        {
            Err(bad("Duplicate itemId in proposal batch"))
        } else {
            // `create_proposal` can bump an item's workflow before a late feedback
            // validation error. Save only its mutation surfaces, not the workspace.
            let item_backup = item_id.as_ref().and_then(|key| {
                list(d, "items")
                    .iter()
                    .position(|item| item["id"] == *key)
                    .map(|at| (at, list(d, "items")[at].clone()))
            });
            let proposal_len = list(d, "proposals").len();
            let feedback_len = list(d, "feedback").len();
            let mut attributed = entry.clone();
            attributed["_verifiedActor"] = actor.public_json();
            match create_proposal(d, &attributed) {
                Ok(proposal) => {
                    let reused = list(d, "proposals").len() == proposal_len;
                    if reused {
                        existing += 1;
                    } else {
                        created += 1;
                    }
                    Ok(json!({"index":index,"itemId":proposal["itemId"],
                        "status":if reused {"existing"} else {"created"},
                        "proposalId":proposal["id"],"proposalRevision":proposal["revision"],
                        "itemRevision":proposal["itemRevision"]}))
                }
                Err(error) => {
                    if let Some((at, before)) = item_backup {
                        list_mut(d, "items")[at] = before;
                    }
                    list_mut(d, "proposals").truncate(proposal_len);
                    list_mut(d, "feedback").truncate(feedback_len);
                    Err(error)
                }
            }
        };
        match result {
            Ok(value) => results.push(value),
            Err(error) => {
                rejected += 1;
                results.push(json!({"index":index,"itemId":item_id,"status":"rejected",
                    "httpStatus":error.0.as_u16(),"error":error.1}));
            }
        }
    }
    let response = json!({"requestId":key,"created":created,"existing":existing,
        "rejected":rejected,"results":results,"replayed":false});
    list_mut(d, "audit").push(json!({"id":receipt_id(key),"action":ACTION,"refId":key,
        "actorId":actor.id,"requestHash":hash,"result":response,"createdAt":now()}));
    Ok(response)
}

pub(crate) async fn create(
    State(app): State<App>,
    Extension(actor): Extension<Actor>,
    Json(body): Json<Value>,
) -> ApiResult<Json<Value>> {
    app.create_operator_batch(&body,&actor).await.map(Json)
}

pub(crate) async fn lookup(
    State(app): State<App>,
    Extension(actor): Extension<Actor>,
    Path(key): Path<String>,
) -> ApiResult<Json<Value>> {
    request_id(&json!(key))?;
    let receipt = app
        .db
        .read_operator_batch_receipt(&key)
        .await?
        .ok_or_else(|| {
            crate::ApiError(
                axum::http::StatusCode::NOT_FOUND,
                "Proposal batch receipt not found".into(),
            )
        })?;
    Ok(Json(saved_result(&receipt, &actor)?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{empty, row};

    fn actor() -> Actor {
        Actor::local_owner("test")
    }
    fn workspace() -> Value {
        let mut d = empty();
        d["feedback"] = json!([]);
        d["items"] = json!([
            {"id":"item-1","itemId":"comment-1","objectId":"11391","postKey":"11391:post-1",
                "conversationKey":"11391:comment-1","contextEvidenceDigest":"a".repeat(64),
                "providerStatus":"new","revision":1,"workflow":"attention","draft":"Human draft","platform":"vk"},
            {"id":"item-2","itemId":"comment-2","objectId":"11391","postKey":"11391:post-1",
                "conversationKey":"11391:comment-2","contextEvidenceDigest":"a".repeat(64),
                "providerStatus":"new","revision":1,"workflow":"attention","draft":"Human draft","platform":"vk"}
        ]);
        d
    }

    #[test]
    fn creates_valid_entries_and_replays_frozen_results_without_duplicate_drafts() {
        let mut d = workspace();
        let body = json!({"requestId":"batch-1","proposals":[
            {"itemId":"item-1","expectedRevision":1,"kind":"reply_and_close","text":"Reviewed reply"},
            {"itemId":"item-2","expectedRevision":3,"kind":"close"}
        ]});
        let first = create_proposals(&mut d, &body, &actor()).unwrap();
        assert_eq!(
            (first["created"].as_u64(), first["rejected"].as_u64()),
            (Some(1), Some(1))
        );
        assert_eq!(list(&d, "proposals").len(), 1);
        assert_eq!(list(&d, "audit").len(), 1);
        let proposal_id = first["results"][0]["proposalId"].as_str().unwrap();
        assert_eq!(
            row(&d, "proposals", proposal_id).unwrap()["text"],
            "Reviewed reply"
        );
        d["proposals"][0]["text"] = json!("Later edit");
        let second = create_proposals(&mut d, &body, &actor()).unwrap();
        assert_eq!(second["results"], first["results"]);
        assert_eq!(second["replayed"], true);
        assert_eq!(list(&d, "proposals").len(), 1);
        assert_eq!(list(&d, "audit").len(), 1);
        assert_eq!(d["items"][0]["revision"], 2);
        assert_eq!(d["items"][1]["revision"], 1);
    }

    #[test]
    fn reused_request_id_with_changed_intent_is_rejected() {
        let mut d = workspace();
        let mut body = json!({"requestId":"batch-2","proposals":[
            {"itemId":"item-1","expectedRevision":1,"kind":"close"}
        ]});
        create_proposals(&mut d, &body, &actor()).unwrap();
        body["proposals"][0]["kind"] = json!("hide");
        assert!(create_proposals(&mut d, &body, &actor()).is_err());
        assert_eq!(list(&d, "proposals").len(), 1);
    }

    #[test]
    fn late_feedback_error_rolls_back_only_failed_item_then_continues() {
        let mut d = workspace();
        let body = json!({"requestId":"batch-3","proposals":[
            {"itemId":"item-1","expectedRevision":1,"kind":"close","eventId":"e-1","sessionId":{},"_verifiedActor":{"id":"forged"}},
            {"itemId":"item-2","expectedRevision":1,"kind":"close"}
        ]});
        let result = create_proposals(&mut d, &body, &actor()).unwrap();
        assert_eq!(
            (result["created"].as_u64(), result["rejected"].as_u64()),
            (Some(1), Some(1))
        );
        assert_eq!(d["items"][0]["workflow"], "attention");
        assert_eq!(d["items"][0]["revision"], 1);
        assert_eq!(d["items"][1]["workflow"], "prepared");
        assert_eq!(list(&d, "proposals").len(), 1);
        assert_eq!(list(&d, "feedback").len(), 0);
    }

    #[test]
    fn duplicate_item_is_rejected_without_draft_duplication() {
        let mut d = workspace();
        let body = json!({"requestId":"batch-4","proposals":[
            {"itemId":"item-1","expectedRevision":1,"kind":"close"},
            {"itemId":"item-1","expectedRevision":2,"kind":"close"}
        ]});
        let result = create_proposals(&mut d, &body, &actor()).unwrap();
        assert_eq!(result["results"][1]["status"], "rejected");
        assert_eq!(list(&d, "proposals").len(), 1);
    }

    #[test]
    fn client_actor_field_cannot_forge_feedback_attribution() {
        let mut d = workspace();
        let body = json!({"requestId":"batch-5","proposals":[
            {"itemId":"item-1","expectedRevision":1,"kind":"close",
                "eventId":"e-2","_verifiedActor":{"id":"forged","role":"owner"}}
        ]});
        let result = create_proposals(&mut d, &body, &actor()).unwrap();
        assert_eq!(result["created"], 1);
        assert_eq!(d["feedback"][0]["actor"]["id"], "local-owner");
        assert_eq!(d["feedback"][0]["actorVerified"], true);
    }

    #[tokio::test]
    async fn endpoint_receipt_survives_database_readback() {
        let (app, _temp) = crate::tests::test_app().await;
        let body = json!({"requestId":"http-batch-1","proposals":[
            {"itemId":"item-1","expectedRevision":1,"kind":"close"}
        ]});
        let first = create(State(app.clone()), Extension(actor()), Json(body.clone()))
            .await
            .unwrap()
            .0;
        let readback = lookup(
            State(app.clone()),
            Extension(actor()),
            Path("http-batch-1".into()),
        )
        .await
        .unwrap()
        .0;
        assert_eq!(readback["results"], first["results"]);
        assert_eq!(readback["replayed"], true);
        let retry = create(State(app.clone()), Extension(actor()), Json(body))
            .await
            .unwrap()
            .0;
        assert_eq!(retry["results"], first["results"]);
        let state = app.read().await.unwrap();
        assert_eq!(list(&state, "proposals").len(), 1);
        assert_eq!(list(&state, "audit").len(), 1);
        assert!(list(&state, "approvals").is_empty());
        assert!(list(&state, "operations").is_empty());
    }
}
