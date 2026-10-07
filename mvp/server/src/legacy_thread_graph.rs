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
                // The connector's customer/participant labels describe a
                // comment's position in that fragment, not changing authorship.
                // Only recognize this convention when every observation agrees
                // with its own explicit target. Other/conflicting roles retain
                // the newest observation and remain revision-bearing evidence.
                let relative_roles = variants.iter().all(|v| {
                    let Some(targets) = branches[v.branch]["id"].as_str()
                        .and_then(|id| branch_targets.get(id)) else { return false; };
                    let Some(id) = text(&v.message,"id") else { return false; };
                    v.message["role"] == if targets.contains(&id) { "customer" } else { "participant" }
                });
                if relative_roles {
                    if let Some(targets) = branches[index]["id"].as_str()
                        .and_then(|id| branch_targets.get(id)) {
                        let is_target = variants.iter().any(|v| v.branch == index
                            && text(&v.message,"id").is_some_and(|id| targets.contains(&id)));
                        message["role"] = json!(if is_target { "customer" } else { "participant" });
                    }
                }
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
