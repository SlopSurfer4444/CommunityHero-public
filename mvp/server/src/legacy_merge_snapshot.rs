fn legacy_merge_snapshot(d: &mut Value, snapshot: &Value) -> ApiResult<()> {
    let ordered_snapshot=legacy_ordered(d,snapshot)?;
    let snapshot=&ordered_snapshot;
    for collection in ["posts", "branches"] {
        if let Some(rows) = snapshot[collection].as_array() {
            for source in rows {
                let mut observed = source.clone();
                if collection == "posts" {
                    // Native acquisition heads are server-owned. A connector
                    // refresh may update the source, never invent/drop its proof.
                    if let Some(object)=observed.as_object_mut(){object.remove("photoAcquisition");}
                    if let Some(head)=list(d,"posts").iter().find(|post|post["id"]==source["id"]).and_then(|post|post.get("photoAcquisition")){
                        observed["photoAcquisition"]=head.clone();
                    }
                }
                if collection == "branches" {
                    observed["observedAt"] = json!(now());
                    observed["observedMessages"] = source["messages"].clone();
                }
                let value = &observed;
                let key = required(value, "id")?;
                let previous = {
                    let rows = list_mut(d, collection);
                    if let Some(old) = rows.iter_mut().find(|r| r["id"] == key) {
                        let previous=old.clone();
                        *old = value.clone();
                        Some(previous)
                    } else {
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
        for value in items {
            let key = required(value, "id")?;
            let items = list_mut(d, "items");
            if let Some(old) = items.iter_mut().find(|r| r["id"] == key) {
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
                items.push(incoming);
            }
        }
    }
    legacy_thread_graph::enrich(d);
    // Provider digest covers its own fragment. Our assembled thread may gain a
    // sibling independently, so version that context separately for approvals.
    use sha2::{Digest, Sha256};
    let branch_digests: HashMap<String,String> = list(d,"branches").iter().filter_map(|branch| {
        let key=branch["id"].as_str()?;
        // Author-history identity is enrichment, not preparation evidence. Keep
        // the pre-enrichment digest stable, including when an adapter adds null.
        // Native locators aid operator navigation, not reply content or identity.
        // All other message fields remain in the approval context fingerprint.
        let mut messages=branch["messages"].clone();
        if let Some(messages)=messages.as_array_mut() {
            for message in messages {
                if let Some(fields)=message.as_object_mut() {
                    for key in ["authorId", "providerOfficial", "roleEvidence", "nativeUrl"] { fields.remove(key); }
                }
            }
        }
        let evidence=json!({"messages":messages,"contextComplete":branch["contextComplete"],"missingParentIds":branch["missingParentIds"],"contextTruncated":branch["contextTruncated"]});
        Some((key.to_string(),format!("{:x}",Sha256::digest(evidence.to_string().as_bytes()))))
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
