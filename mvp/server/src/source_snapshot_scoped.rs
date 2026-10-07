//! Shared source reducer. Extracted unchanged from main.rs at Wave2 E0 admission.
//! The storage selector owns completeness; this reducer never invents missing evidence.
use crate::*;

pub(crate) fn merge_snapshot(d: &mut Value, snapshot: &Value) -> ApiResult<()> {
    let ordered_snapshot=snapshot_order::ordered(d,snapshot)?;
    let snapshot=&ordered_snapshot;
    for collection in ["posts", "branches"] {
        if let Some(rows) = snapshot[collection].as_array() {
            let mut row_index = None;
            for source in rows {
                let mut observed = source.clone();
                if collection == "posts" {
                    // Native acquisition heads are server-owned. A connector
                    // refresh may update the source, never invent/drop its proof.
                    if let Some(object)=observed.as_object_mut(){object.remove("photoAcquisition");}
                    let positions = row_index.get_or_insert_with(||snapshot_domain::first_rows(list(d,collection)));
                    if let Some(head)=snapshot_domain::position(list(d,"posts"), positions, &source["id"]).and_then(|position|list(d,"posts")[position].get("photoAcquisition")){
                        observed["photoAcquisition"]=head.clone();
                    }
                }
                if collection == "branches" {
                    observed["observedAt"] = json!(now());
                    observed["observedMessages"] = source["messages"].clone();
                }
                let value = &observed;
                let key = required(value, "id")?;
                let row_index = row_index.get_or_insert_with(||snapshot_domain::first_rows(list(d,collection)));
                let previous = {
                    let rows = list_mut(d, collection);
                    if let Some(position) = row_index.get(key).copied() {
                        let previous = std::mem::replace(&mut rows[position], value.clone());
                        Some(previous)
                    } else {
                        row_index.insert(key.to_owned(), rows.len());
                        rows.push(value.clone());
                        None
                    }
                };
                if collection=="posts" {
                    if let Some(previous)=previous {
                        post_media_policy::invalidate_source_change(d,&previous,value)?;
                        media_audio_equivalence::invalidate_source_change(d,&previous,value)?;
                    }
                }
            }
        }
    }
    if let Some(items) = snapshot["items"].as_array() {
        let mut item_index = None;
        for value in items {
            let key = required(value, "id")?;
            let item_index = item_index.get_or_insert_with(||snapshot_domain::first_rows(list(d,"items")));
            let items = list_mut(d, "items");
            if let Some(position) = item_index.get(key).copied() {
                let old = &mut items[position];
                for identity in ["itemId", "objectId"] {
                    if old[identity] != value[identity] {
                        return Err(conflict("Provider identity changed for an existing record"));
                    }
                }
                if old.get("connectorBinding").is_some()
                    && old["connectorBinding"] != value["connectorBinding"]
                {
                    return Err(conflict("Imported record belongs to another connector"));
                }
                let mut incoming = value.clone();
                incoming.as_object_mut().unwrap().remove("commentPhotoAcquisition");
                if let Some(head)=old.get("commentPhotoAcquisition"){incoming["commentPhotoAcquisition"]=head.clone();}
                for k in [
                    "draft",
                    "draftEdited",
                    "draftOrigin",
                    "draftSessionId",
                    "workflow",
                    "waitingReason",
                    "dueAt",
                    "revision",
                    "branchContextDigest",
                    "autoPreparation",
                    "autoRevalidation",
                    "reason",
                    "decision",
                    "triageTags",
                ] {
                    incoming[k] = old[k].clone();
                }
                if old["contextEvidenceDigest"] != incoming["contextEvidenceDigest"]
                    || old["providerStatus"] != incoming["providerStatus"]
                    || old["postKey"] != incoming["postKey"]
                    || old["conversationKey"] != incoming["conversationKey"]
                {
                    bump(&mut incoming);
                }
                if value["providerStatus"] == "deleted" {
                    incoming["workflow"] = json!("deleted");
                } else if value["providerStatus"] == "closed" && old["workflow"] != "waiting" {
                    incoming["workflow"] = json!("closed");
                } else if ["closed", "deleted"].contains(&old["providerStatus"].as_str().unwrap_or(""))
                    && ["closed", "deleted"].contains(&old["workflow"].as_str().unwrap_or(""))
                    && ["new", "inprogress"].contains(&value["providerStatus"].as_str().unwrap_or(""))
                {
                    incoming["workflow"] = json!("attention");
                }
                incoming["providerObservedAt"] = incoming["contextObservedAt"].as_str().map(|s|json!(s)).unwrap_or_else(||json!(now()));
                *old = incoming;
            } else {
                let mut incoming = value.clone();
                incoming.as_object_mut().unwrap().remove("commentPhotoAcquisition");
                incoming["revision"] = json!(1);
                incoming["draft"] = json!("");
                // Provider snapshots cannot create operator edits or feedback lineage.
                incoming["draftEdited"] = json!(false);
                incoming["draftOrigin"] = Value::Null;
                incoming["draftSessionId"] = Value::Null;
                incoming["waitingReason"] = json!("");
                incoming["dueAt"] = Value::Null;
                incoming["workflow"] = json!(match value["providerStatus"].as_str() {
                    Some("closed") => "closed",
                    Some("deleted") => "deleted",
                    _ => "attention",
                });
                incoming["providerObservedAt"] = incoming["contextObservedAt"].as_str().map(|s|json!(s)).unwrap_or_else(||json!(now()));
                item_index.insert(key.to_owned(), items.len());
                items.push(incoming);
            }
        }
    }
    thread_graph::enrich(d);
    // Provider digest covers its own fragment. Our assembled thread may gain a
    // sibling independently, so version that context separately for approvals.
    let branch_digests: HashMap<String,String> = list(d,"branches").iter().filter_map(|branch| {
        let key=branch["id"].as_str()?;
        Some((key.to_owned(), snapshot_domain::branch_context_digest(branch)))
    }).collect();
    for item in list_mut(d, "items") {
        if let Some(digest) = item["branchId"]
            .as_str()
            .and_then(|key| branch_digests.get(key))
        {
            if item["branchContextDigest"]
                .as_str()
                .is_some_and(|old| old != digest)
            {
                bump(item);
            }
            item["branchContextDigest"] = json!(digest);
        }
    }
    auto_prepare::reconcile_stale(d,chrono::Utc::now().timestamp());
    Ok(())
}
