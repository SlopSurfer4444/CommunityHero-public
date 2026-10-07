//! Borrowed, single-pass helpers for snapshot merge. No cache survives mutation.
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{collections::HashMap, io::{self, Write}};

/// `iter().find` selects the first duplicate. Keep that behavior even for legacy
/// rows; rows with non-string identities cannot equal a required string id.
pub(super) fn first_rows(rows: &[Value]) -> HashMap<String, usize> {
    let mut index = HashMap::with_capacity(rows.len());
    for (position, row) in rows.iter().enumerate() {
        if let Some(id) = row["id"].as_str() {
            index.entry(id.to_owned()).or_insert(position);
        }
    }
    index
}

/// Ordering examines identity before merge validates it. Preserve JSON equality
/// for malformed/missing IDs too: a stale malformed row may be dropped safely
/// before `required(id)` is reached. Never turn that into a new merge error.
pub(super) fn position(rows: &[Value], index: &HashMap<String, usize>, id: &Value) -> Option<usize> {
    match id.as_str() {
        Some(id) => index.get(id).copied(),
        None => rows.iter().position(|row| row["id"] == *id),
    }
}

struct HashWriter(Sha256);
impl Write for HashWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0.update(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> { Ok(()) }
}
impl HashWriter {
    fn raw(&mut self, bytes: &[u8]) {
        self.write_all(bytes).expect("SHA256 writer is infallible");
    }
    fn json(&mut self, value: &Value) {
        serde_json::to_writer(self, value).expect("JSON Value and SHA256 writer are infallible");
    }
    fn string(&mut self, value: &str) {
        serde_json::to_writer(self, value).expect("JSON string and SHA256 writer are infallible");
    }
}

/// Emit the exact former `json!({...}).to_string()` bytes directly into SHA256.
/// Preserve the current sorted serde_json Map order and strip only the four
/// top-level enrichment fields from object messages. Nested
/// fields, malformed/non-array messages and explicit nulls remain evidence.
/// Enabling serde_json preserve_order requires revalidating this parity: its
/// removal behavior can reorder keys, unlike the sorted map in this closure.
pub(super) fn branch_context_digest(branch: &Value) -> String {
    let mut writer = HashWriter(Sha256::new());
    // Construct the same small outer Map as the former json! object. The current
    // dependency closure uses sorted maps; key order remains part of the digest.
    let mut fields = serde_json::Map::new();
    for key in ["messages", "contextComplete", "missingParentIds", "contextTruncated"] {
        fields.insert(key.to_owned(), Value::Null);
    }
    writer.raw(b"{");
    for (ordinal, key) in fields.keys().enumerate() {
        if ordinal > 0 { writer.raw(b","); }
        writer.string(key);
        writer.raw(b":");
        if key != "messages" {
            writer.json(&branch[key]);
            continue;
        }
        let Some(messages) = branch["messages"].as_array() else {
            writer.json(&branch["messages"]);
            continue;
        };
        writer.raw(b"[");
        for (ordinal, message) in messages.iter().enumerate() {
            if ordinal > 0 { writer.raw(b","); }
            let Some(fields) = message.as_object() else {
                writer.json(message);
                continue;
            };
            writer.raw(b"{");
            let mut first = true;
            for (key, value) in fields {
                if ["authorId", "providerOfficial", "roleEvidence", "nativeUrl"].contains(&key.as_str()) { continue; }
                if !first { writer.raw(b","); }
                first = false;
                writer.string(key);
                writer.raw(b":");
                writer.json(value);
            }
            writer.raw(b"}");
        }
        writer.raw(b"]");
    }
    writer.raw(b"}");
    format!("{:x}", writer.0.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn legacy_digest(branch: &Value) -> String {
        let mut messages = branch["messages"].clone();
        if let Some(messages) = messages.as_array_mut() {
            for message in messages {
                if let Some(fields) = message.as_object_mut() {
                    for key in ["authorId", "providerOfficial", "roleEvidence", "nativeUrl"] { fields.remove(key); }
                }
            }
        }
        let evidence = json!({"messages":messages,"contextComplete":branch["contextComplete"],
            "missingParentIds":branch["missingParentIds"],"contextTruncated":branch["contextTruncated"]});
        format!("{:x}", Sha256::digest(evidence.to_string().as_bytes()))
    }

    #[test]
    fn branch_digest_is_exact_for_malformed_null_and_large_nested_messages() {
        for messages in [Value::Null, json!({"authorId":"retained_non_array"}), json!("quoted\n\"text"),
            json!([null, false, 4.700000000000001, "\u{0000}\n\\\"Привет", {},
                {"id":"target","authorId":null,"providerOfficial":false,"roleEvidence":"ignored","nativeUrl":"ignored",
                    "role":"customer","attachments":[{"authorId":"nested retained","nativeUrl":"nested evidence"}]}]),
            json!((0..300).map(|n|json!({"id":format!("message-{n}"),"text":"evidence".repeat(200),
                "authorId":format!("author-{n}"),"attachments":[{"type":"photo","url":format!("https://example.test/{n}")}]})).collect::<Vec<_>>())] {
            let branch = json!({"messages":messages,"contextComplete":false,"missingParentIds":["parent"],"contextTruncated":true});
            assert_eq!(branch_context_digest(&branch), legacy_digest(&branch));
        }
        assert_eq!(branch_context_digest(&json!({})), legacy_digest(&json!({})));
    }

    #[test]
    fn enrichment_changes_are_ignored_but_nested_evidence_and_content_changes_are_not() {
        let original = json!({"messages":[{"id":"a","text":"old","attachments":[{"authorId":"nested"}]}]});
        let mut enriched = original.clone();
        for field in ["authorId", "providerOfficial", "roleEvidence", "nativeUrl"] {
            enriched["messages"][0][field] = json!("enrichment");
        }
        assert_eq!(branch_context_digest(&original), branch_context_digest(&enriched));
        for pointer in ["/messages/0/text", "/messages/0/attachments/0/authorId"] {
            let mut changed = original.clone();
            *changed.pointer_mut(pointer).unwrap() = json!("changed");
            assert_ne!(branch_context_digest(&original), branch_context_digest(&changed));
        }
    }

    #[test]
    fn index_retains_first_duplicate_and_ignores_non_string_identity() {
        let rows = vec![json!({"id":null}), json!({"id":"same"}), json!({"id":"same"}), json!({"id":3})];
        let index = first_rows(&rows);
        assert_eq!(index.len(), 1);
        assert_eq!(index.get("same"), Some(&1));
    }
}
