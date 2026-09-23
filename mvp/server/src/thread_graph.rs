//! Enrich only the connected context actually observed in this database.
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet, VecDeque};

const MAX_MESSAGES: usize = 300;

fn text(v: &Value, key: &str) -> Option<String> {
    v[key].as_str().filter(|s| !s.is_empty()).map(str::to_owned)
}

fn provider_key(object: &str, item: &str) -> String {
    // JSON tuples are unambiguous even when provider identifiers contain hyphens.
    format!("provider:{}", json!([object, item]))
}

fn key(message: &Value, object: &str) -> Option<String> {
    if let Some(item) = text(message, "providerItemId") {
        let obj = text(message, "providerObjectId").unwrap_or_else(|| object.into());
        if !object.is_empty() && obj != object {
            return None;
        }
        Some(provider_key(&obj, &item))
    } else {
        if !object.is_empty() && text(message, "providerObjectId").is_some_and(|o| o != object) {
            return None;
        }
        text(message, "id").map(|id| format!("legacy:{id}"))
    }
}

#[derive(Clone)]
struct Observation {
    branch: usize,
    message: Value,
    timestamp: Option<i64>,
}

fn timestamp(branch: &Value) -> Option<i64> {
    branch["observedAt"]
        .as_str()
        .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
        .map(|v| v.timestamp_millis())
        .or_else(|| branch["observedAt"].as_i64())
}

/// The caller replaces observedMessages whenever a fresh provider branch arrives.
pub fn enrich(database: &mut Value) {
    let items = database["items"].as_array().cloned().unwrap_or_default();
    // Resolve exact branch/post membership once. A single refreshed context
    // still enriches the workspace, but must not scan all items for each branch.
    let mut scoped_items: BTreeMap<(&str, &str), Vec<&Value>> = BTreeMap::new();
    let mut branch_targets: BTreeMap<&str, BTreeSet<String>> = BTreeMap::new();
    for item in &items {
        let Some(branch) = item["branchId"].as_str() else { continue; };
        if let Some(post) = item["postId"].as_str() {
            scoped_items.entry((branch, post)).or_default().push(item);
        }
        if let Some(target) = text(item, "targetId") {
            branch_targets.entry(branch).or_default().insert(target);
        }
    }
    let default_binding = database["connectorBinding"].clone();
    let Some(branches) = database["branches"].as_array_mut() else {
        return;
    };
    for branch in branches.iter_mut() {
        if !branch["observedMessages"].is_array() {
            branch["observedMessages"] =
                json!(branch["messages"].as_array().cloned().unwrap_or_default());
        }
        branch["contextComplete"] = json!(false);
    }
    let mut scopes: BTreeMap<String, (String, Vec<usize>)> = BTreeMap::new();
    for (index, branch) in branches.iter().enumerate() {
        let (Some(id), Some(post)) = (text(branch, "id"), text(branch, "postId")) else {
            continue;
        };
        let mut candidates = BTreeSet::new();
        let mut object = None;
        for item in scoped_items.get(&(id.as_str(), post.as_str())).into_iter().flatten() {
            let Some(obj) = text(item, "objectId") else {
                continue;
            };
            let binding = item.get("connectorBinding").unwrap_or(&default_binding);
            candidates.insert(json!([obj, binding, post]).to_string());
            object = Some(obj);
        }
        // Ambiguous or absent routing evidence must never join a different branch.
        if candidates.len() == 1 {
            scopes
                .entry(candidates.into_iter().next().unwrap())
                .or_insert_with(|| (object.unwrap(), Vec::new()))
                .1
                .push(index);
        } else {
            scopes.insert(format!("isolated:{index}"), (String::new(), vec![index]));
        }
    }
    // Ownership is account/object scoped, not post scoped. Learn only from
    // explicit provider evidence; an author mentioning a brand is not evidence.
    let ownership_scope = |scope: &str| -> Option<String> {
        let parts: Value = serde_json::from_str(scope).ok()?;
        (parts.as_array()?.len() == 3).then(|| json!([parts[0], parts[1]]).to_string())
    };
    let mut official_authors: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for (scope, (object, indices)) in &scopes {
        let Some(account_scope) = ownership_scope(scope) else { continue; };
        for &index in indices {
            for message in branches[index]["observedMessages"].as_array().unwrap() {
                if text(message,"providerObjectId").as_ref() == Some(object)
                    && (message["providerOfficial"] == true || message["roleEvidence"] == "provider-official-replies") {
                    if let Some(author) = text(message,"authorId") {
                        official_authors.entry(account_scope.clone()).or_default().insert(author);
                    }
                }
            }
        }
    }
    for (scope, (object, indices)) in scopes {
        let scoped_authors = ownership_scope(&scope).and_then(|s| official_authors.get(&s));
        let mut aliases: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        for &index in &indices {
            for message in branches[index]["observedMessages"].as_array().unwrap() {
                if let (Some(id), Some(k)) = (text(message, "id"), key(message, &object)) {
                    if k.starts_with("provider:") {
                        aliases.entry(id).or_default().insert(k);
                    }
                }
            }
        }
        let resolve = |id: &str| -> String {
            aliases
                .get(id)
                .filter(|keys| keys.len() == 1)
                .and_then(|keys| keys.first())
                .cloned()
                .unwrap_or_else(|| format!("legacy:{id}"))
        };
        let canonical = |m: &Value| -> Option<String> {
            let k = key(m, &object)?;
            if k.starts_with("legacy:") {
                text(m, "id").map(|id| resolve(&id))
            } else {
                Some(k)
            }
        };
        let mut nodes: BTreeMap<String, Vec<Observation>> = BTreeMap::new();
        let mut edges: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        let mut parents: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        let mut parent_labels = BTreeMap::new();
        let mut seeds: BTreeMap<usize, Vec<String>> = BTreeMap::new();
        for &index in &indices {
            for message in branches[index]["observedMessages"].as_array().unwrap() {
                let Some(k) = canonical(message) else {
                    continue;
                };
                seeds.entry(index).or_default().push(k.clone());
                nodes.entry(k.clone()).or_default().push(Observation {
                    branch: index,
                    message: message.clone(),
                    timestamp: timestamp(&branches[index]),
                });
                let parent = if let Some(item) = text(message, "replyToProviderItemId") {
                    Some((
                        provider_key(&object, &item),
                        text(message, "parentId").unwrap_or(item),
                    ))
                } else {
                    text(message, "parentId").map(|id| (resolve(&id), id))
                };
                if let Some((p, label)) = parent {
                    if p == k {
                        continue;
                    }
                    edges.entry(k.clone()).or_default().insert(p.clone());
                    edges.entry(p.clone()).or_default().insert(k.clone());
                    parents.entry(k).or_default().insert(p.clone());
                    parent_labels.entry(p).or_insert(label);
                }
            }
        }
        for &index in &indices {
            let original = seeds.get(&index).cloned().unwrap_or_default();
            let mut queue: VecDeque<String> = original.iter().cloned().collect();
            let mut visited = BTreeSet::new();
            while let Some(k) = queue.pop_front() {
                if !visited.insert(k.clone()) {
                    continue;
                }
                if let Some(neighbors) = edges.get(&k) {
                    queue.extend(neighbors.iter().cloned());
                }
            }
            let count = visited.iter().filter(|k| nodes.contains_key(*k)).count();
            // Own observations lead, retaining target nodes when the context is bounded.
            let mut ordered = Vec::new();
            let mut included = BTreeSet::new();
            for k in original.into_iter().chain(visited.iter().cloned()) {
                if nodes.contains_key(&k) && included.insert(k.clone()) {
                    ordered.push(k);
                }
            }
            if ordered.len() > MAX_MESSAGES {
                let target_ids = branches[index]["id"].as_str()
                    .and_then(|branch| branch_targets.get(branch));
                ordered.sort_by_key(|k| {
                    !nodes[k].iter().any(|v| {
                        v.branch == index
                            && text(&v.message, "id").is_some_and(|id| target_ids.is_some_and(|ids| ids.contains(&id)))
                    })
                });
            }
            ordered.truncate(MAX_MESSAGES);
            let mut selected: BTreeMap<String, Value> = BTreeMap::new();
            for k in &ordered {
                let variants = &nodes[k];
                let chosen = variants
                    .iter()
                    .max_by_key(|v| (v.timestamp, v.branch == index, std::cmp::Reverse(v.branch)))
                    .unwrap();
                let mut message = chosen.message.clone();
                // Author identity is immutable for a provider comment. An older
                // explicit official observation must survive a legacy projection
                // which merely called every parent "participant".
                if variants.iter().any(|v| v.message["providerOfficial"] == true
                    || v.message["roleEvidence"] == "provider-official-replies")
                    || (text(&message,"providerObjectId").as_ref() == Some(&object)
                        && text(&message,"authorId").is_some_and(|a| scoped_authors.is_some_and(|authors| authors.contains(&a)))) {
                    message["role"] = json!("brand");
                    message["roleEvidence"] = json!("provider-official");
                    message["providerOfficial"] = json!(true);
                }
                if let Some(own) = variants.iter().find(|v| v.branch == index) {
                    message["id"] = own.message["id"].clone();
                }
                selected.insert(k.clone(), message);
            }
            let ids: BTreeMap<String, Value> = selected
                .iter()
                .map(|(k, v)| (k.clone(), v["id"].clone()))
                .collect();
            for (k, message) in &mut selected {
                if let Some(ps) = parents.get(k).filter(|ps| ps.len() == 1) {
                    if let Some(id) = ps.first().and_then(|p| ids.get(p)) {
                        message["parentId"] = id.clone();
                    }
                }
            }
            let missing: BTreeSet<String> = visited
                .iter()
                .filter(|k| !nodes.contains_key(*k))
                .filter_map(|k| parent_labels.get(k).cloned())
                .collect();
            let branch = &mut branches[index];
            let mut messages: Vec<Value> = ordered
                    .iter()
                    .filter_map(|k| selected.remove(k))
                    .collect();
            // Provider observations arrive as parent, target, official replies, not
            // conversation chronology. Keep unknown dates in their original slots;
            // stably order dated messages without inventing different parent edges.
            let dated_slots: Vec<(usize, i64)> = messages.iter().enumerate()
                .filter_map(|(i, message)| message["createdAt"].as_str()
                    .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
                    .map(|date| (i, date.timestamp_millis())))
                .collect();
            let mut dated: Vec<(i64, Value)> = dated_slots.iter()
                .map(|(i, date)| (*date, messages[*i].clone())).collect();
            dated.sort_by_key(|(date, _)| *date);
            for ((slot, _), (_, message)) in dated_slots.into_iter().zip(dated) {
                messages[slot] = message;
            }
            branch["messages"] = json!(messages);
            branch["missingParentIds"] = json!(missing);
            branch["knownMessageCount"] = json!(count);
            branch["contextTruncated"] = json!(count > MAX_MESSAGES);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> Value {
        json!({"items":[
            {"branchId":"a","postId":"post","objectId":"object"},
            {"branchId":"b","postId":"post","objectId":"object"},
            {"branchId":"c","postId":"post","objectId":"object"}],
            "branches":[
                {"id":"a","postId":"post","messages":[{"id":"a","parentId":"missing"}]},
                {"id":"b","postId":"post","messages":[{"id":"b","parentId":"missing"}]},
                {"id":"c","postId":"post","messages":[{"id":"c","parentId":null}]}]})
    }
    #[test]
    fn connects_siblings_but_not_unrelated_roots_and_stays_unknown() {
        let mut d = fixture();
        enrich(&mut d);
        assert_eq!(d["branches"][0]["messages"].as_array().unwrap().len(), 2);
        assert_eq!(d["branches"][2]["knownMessageCount"], 1);
        assert_eq!(d["branches"][0]["missingParentIds"], json!(["missing"]));
        assert_eq!(d["branches"][0]["contextComplete"], false);
        let first = d.clone();
        enrich(&mut d);
        assert_eq!(d, first);
    }
    #[test]
    fn isolates_object_binding_and_post() {
        for field in ["objectId", "connectorBinding", "postId"] {
            let mut d = fixture();
            d["items"][1][field] = json!("different");
            enrich(&mut d);
            assert_eq!(d["branches"][0]["knownMessageCount"], 1);
        }
    }
    #[test]
    fn explicit_identity_uses_fresh_content_without_renaming_target() {
        let mut d = fixture();
        d["branches"][0]["messages"] = json!([{"id":"comment-object-part-item-part","providerObjectId":"object","providerItemId":"x-y","text":"old"}]);
        d["branches"][1]["messages"] = json!([{"id":"official-other-name","providerObjectId":"object","providerItemId":"x-y","text":"new"}]);
        d["branches"][0]["observedAt"] = json!("2026-09-20T01:00:00Z");
        d["branches"][1]["observedAt"] = json!("2026-09-21T01:00:00Z");
        enrich(&mut d);
        assert_eq!(
            d["branches"][0]["messages"][0]["id"],
            "comment-object-part-item-part"
        );
        assert_eq!(d["branches"][0]["messages"][0]["text"], "new");
        assert_eq!(d["branches"][0]["knownMessageCount"], 1);
    }
    #[test]
    fn no_amplification_after_source_replacement() {
        let mut d = fixture();
        enrich(&mut d);
        d["branches"][1]["observedMessages"] = json!([{"id":"b","parentId":null}]);
        enrich(&mut d);
        assert_eq!(d["branches"][0]["knownMessageCount"], 1);
    }
    #[test]
    fn hyphenated_legacy_ids_are_not_guessed_as_provider_identity() {
        let mut d = fixture();
        d["branches"][0]["messages"] = json!([{"id":"comment-object-a-b","parentId":null}]);
        d["branches"][1]["messages"] = json!([{"id":"official-object-a-b","parentId":null}]);
        enrich(&mut d);
        assert_eq!(d["branches"][0]["knownMessageCount"], 1);
        assert_eq!(d["branches"][1]["knownMessageCount"], 1);
    }
    #[test]
    fn exact_legacy_reference_can_find_explicit_parent() {
        let mut d = fixture();
        d["branches"][0]["messages"] = json!([{"id":"a","parentId":"parent"}]);
        d["branches"][1]["messages"] =
            json!([{"id":"parent","providerObjectId":"object","providerItemId":"p"}]);
        enrich(&mut d);
        assert_eq!(d["branches"][0]["knownMessageCount"], 2);
        assert_eq!(d["branches"][0]["missingParentIds"], json!([]));
        assert_eq!(d["branches"][0]["contextComplete"], false);
    }
    #[test]
    fn bounded_context_retains_original_target() {
        let mut d = fixture();
        d["branches"][1]["messages"] = json!(
            (0..350)
                .map(|n| json!({"id":format!("sibling-{n}"),"parentId":"missing"}))
                .collect::<Vec<_>>()
        );
        enrich(&mut d);
        assert_eq!(d["branches"][0]["messages"].as_array().unwrap().len(), 300);
        assert_eq!(d["branches"][0]["messages"][0]["id"], "a");
        assert_eq!(d["branches"][0]["knownMessageCount"], 351);
        assert_eq!(d["branches"][0]["contextTruncated"], true);
    }
    #[test]
    fn exact_membership_ignores_other_posts_and_retains_late_target_when_truncated() {
        let mut d = fixture();
        d["items"][0]["targetId"] = json!("sibling-349");
        // Same branch label in another post is not routing evidence for this
        // branch; accepting it would make the account scope ambiguous.
        d["items"].as_array_mut().unwrap().push(json!({
            "branchId":"a", "postId":"other-post", "objectId":"other-object"
        }));
        d["items"].as_array_mut().unwrap().push(json!({"branchId":null,"postId":"post"}));
        d["branches"][0]["messages"] = json!((0..350)
            .map(|n| json!({"id":format!("sibling-{n}"),"parentId":"missing"}))
            .collect::<Vec<_>>());
        enrich(&mut d);
        let branch = &d["branches"][0];
        assert_eq!(branch["knownMessageCount"], 351);
        assert_eq!(branch["contextTruncated"], true);
        assert_eq!(branch["messages"].as_array().unwrap().len(), MAX_MESSAGES);
        assert_eq!(branch["messages"][0]["id"], "sibling-349");
        // A conflicting route within the exact branch/post must still isolate.
        d["items"].as_array_mut().unwrap().push(json!({
            "branchId":"a", "postId":"post", "objectId":"other-object"
        }));
        enrich(&mut d);
        assert_eq!(d["branches"][0]["knownMessageCount"], 350);
    }
    #[test]
    fn dated_replies_are_chronological_without_inventing_nested_parents() {
        let mut d = fixture();
        let observations = json!([
            {"id":"root","parentId":null,"createdAt":"2026-09-21T15:34:40Z"},
            {"id":"user","parentId":"root","createdAt":"2026-09-21T16:20:11Z"},
            {"id":"unknown","parentId":"root","createdAt":"invalid"},
            {"id":"brand","parentId":"root","createdAt":"2026-09-21T18:41:09+03:00"},
            {"id":"same-time","parentId":"root","createdAt":"2026-09-21T16:20:11Z"}
        ]);
        d["branches"][0]["messages"] = observations.clone();
        enrich(&mut d);
        let messages = d["branches"][0]["messages"].as_array().unwrap();
        let ids: Vec<&str> = messages.iter().map(|m| m["id"].as_str().unwrap()).collect();
        assert_eq!(ids, vec!["root", "brand", "unknown", "user", "same-time"]);
        assert_eq!(messages[3]["parentId"], "root");
        assert_eq!(d["branches"][0]["observedMessages"], observations);
        let first = d.clone();
        enrich(&mut d);
        assert_eq!(d, first);
    }
    #[test]
    fn official_identity_evidence_survives_newer_legacy_parent_projection() {
        let mut d = fixture();
        d["branches"][0]["messages"] = json!([{"id":"comment-brand","providerObjectId":"object","providerItemId":"same","role":"participant"}]);
        d["branches"][0]["observedAt"] = json!("2026-09-22T00:00:00Z");
        d["branches"][1]["messages"] = json!([{"id":"official-brand","providerObjectId":"object","providerItemId":"same","role":"brand","providerOfficial":true}]);
        d["branches"][1]["observedAt"] = json!("2026-09-21T00:00:00Z");
        enrich(&mut d);
        assert_eq!(d["branches"][0]["messages"][0]["role"], "brand");
        assert_eq!(d["branches"][0]["messages"][0]["id"], "comment-brand");
        assert_eq!(d["branches"][0]["observedMessages"][0]["role"], "participant");
        let mut isolated = fixture();
        isolated["branches"][0]["messages"] = json!([{"id":"fake-brand-name","author":"LikeAvto","role":"participant"}]);
        enrich(&mut isolated);
        assert_eq!(isolated["branches"][0]["messages"][0]["role"], "participant");
    }
    #[test]
    fn official_author_evidence_crosses_posts_but_not_account_or_object_scopes() {
        for isolate in ["none", "object", "binding"] {
            let mut d = fixture();
            d["items"][1]["postId"] = json!("other-post");
            d["branches"][1]["postId"] = json!("other-post");
            d["branches"][0]["messages"] = json!([{"id":"a","providerObjectId":"object","providerItemId":"a","authorId":"provider:brand","role":"customer"}]);
            d["branches"][1]["messages"] = json!([{"id":"b","providerObjectId":"object","providerItemId":"b","authorId":"provider:brand","role":"brand","providerOfficial":true}]);
            if isolate == "object" {
                d["items"][1]["objectId"] = json!("other");
                d["branches"][1]["messages"][0]["providerObjectId"] = json!("other");
            } else if isolate == "binding" {
                d["items"][1]["connectorBinding"] = json!({"account":"other"});
            }
            enrich(&mut d);
            assert_eq!(d["branches"][0]["messages"][0]["role"], if isolate == "none" { "brand" } else { "customer" });
        }
    }
}
