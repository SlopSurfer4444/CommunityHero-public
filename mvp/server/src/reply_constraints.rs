//! Deterministic reply gates compiled only from selected canonical knowledge heads.
//! Imported provenance identifies a known legacy matcher; arbitrary rule prose never does.
use serde_json::Value;
use std::collections::BTreeSet;
use axum::http::Uri;

const INVALID: &str = "Invalid imported reply URL policy";
const FORBIDDEN: &str = "Reply URL is not allowed by current policy";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PolicySource { Typed, LegacyImport }

#[derive(Clone, Debug)]
pub(crate) struct PolicyHead {
    pub(crate) source: PolicySource,
    pub(crate) entry_id: String,
    pub(crate) version_id: String,
    pub(crate) values: BTreeSet<String>,
}

fn rows<'a>(value: &'a Value, key: &str) -> &'a [Value] {
    value[key].as_array().map(Vec::as_slice).unwrap_or(&[])
}
fn text<'a>(value: &'a Value, key: &str) -> &'a str {
    value[key].as_str().unwrap_or("")
}
fn digest(value: &Value) -> bool {
    value
        .as_str()
        .is_some_and(|s| s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit()))
}
fn bounded(value: &Value, max: usize) -> bool {
    value.as_str().is_some_and(|s| {
        !s.is_empty() && s.len() <= max && !s.bytes().any(|b| b < 0x20 || b == 0x7f)
    })
}
fn company(account: &str) -> Option<&'static str> {
    match account {
        "LikeAvto" => Some("likeavto"),
        "BAW Russia" => Some("baw-russia"),
        _ => None,
    }
}

fn imported_allowlist(
    context: &super::prepare_bundle::EvidenceContext<'_>,
    material: &Value,
    manifest: &[Value],
) -> Result<Option<BTreeSet<String>>, &'static str> {
    let imported = &material["companyImport"];
    let source = &imported["source"];
    let marker = &source["originalIds"];
    if source["origin"] != "commentops-fast.account-card" || marker["field"] != "allowed_reply_urls"
    {
        return Ok(None);
    }
    let account = text(context.workspace(), "account");
    let company = company(account).ok_or(INVALID)?;
    let expected_source = format!("configs/commentops-fast/{company}.json");
    let scope = &imported["scope"];
    let metadata = &imported["metadata"];
    if material["kind"] != "rule"
        || material["trust"] != "imported_policy"
        || imported["companyKey"] != company
        || scope["companyKey"] != company
        || scope
            .as_object()
            .is_none_or(|v| v.keys().any(|key| key != "companyKey"))
        || marker["source"] != expected_source
        || marker.as_object().is_none_or(|v| {
            v.keys()
                .any(|key| !matches!(key.as_str(), "field" | "source"))
        })
        || !bounded(&imported["importKey"], 512)
        || !digest(&imported["recordSha256"])
        || !digest(&source["sha256"])
        || metadata["category"] != "brand_policy"
        || metadata["grantsExecutionAuthority"] != false
        || metadata.get("legacyProvenance") != Some(marker)
        || metadata.get("legacyScope") != Some(scope)
    {
        return Err(INVALID);
    }
    let entry = text(material, "knowledgeEntryId");
    let version = text(material, "knowledgeVersionId");
    if entry.is_empty() || version.is_empty() {
        return Err(INVALID);
    }
    let matched: Vec<_> = manifest
        .iter()
        .filter(|row| row["entryId"] == entry && row["versionId"] == version)
        .collect();
    if matched.len() != 1 {
        return Err(INVALID);
    }
    let selected = matched[0];
    if selected["kind"] != "rule"
        || selected["trust"] != "imported_policy"
        || !digest(&selected["hash"])
        || selected["scope"]["account"] != account
        || !selected["scope"]["postKeys"].is_array()
        || selected["scope"].as_object().is_none_or(|v| {
            v.keys()
                .any(|key| !matches!(key.as_str(), "account" | "postKeys"))
        })
    {
        return Err(INVALID);
    }
    let canonical = context.knowledge_version(version)?;
    if canonical["entryId"] != entry
        || canonical["kind"] != "rule"
        || canonical["trust"] != "imported_policy"
        || canonical["scope"] != selected["scope"]
        || canonical.get("companyImport") != Some(imported)
        || canonical["text"] != material["text"]
        || canonical["hash"] != selected["hash"]
        || canonical["grantsExecutionAuthority"] != false
    {
        return Err(INVALID);
    }
    let values: Value = serde_json::from_str(text(material, "text")).map_err(|_| INVALID)?;
    let values = values
        .as_array()
        .filter(|v| v.len() <= 100)
        .ok_or(INVALID)?;
    if values.iter().any(|value| !bounded(value, 2048))
        || metadata["value"] != Value::Array(values.clone())
    {
        return Err(INVALID);
    }
    Ok(Some(
        values
            .iter()
            .map(|value| value.as_str().unwrap().to_owned())
            .collect(),
    ))
}

fn valid_exact_url(value:&str)->bool {
    if value.len()>2048 || !value.is_ascii() || value.bytes().any(|b|b<=0x20||b==0x7f)
        || value.contains(['#','<','>','"','\'','`','\\','*'])
        || value.chars().last().is_some_and(|ch|matches!(ch,'.'|','|'!'|'?'|';'|':'|')'|']'|'}'))
        || !(value.starts_with("https://")||value.starts_with("http://")) {return false;}
    let Ok(uri)=value.parse::<Uri>() else {return false};
    if !matches!(uri.scheme_str(),Some("https"|"http")){return false;}
    let Some(authority)=uri.authority() else {return false};
    let host=authority.host();
    if host.len()>253 || host.is_empty() || authority.as_str()!=host || host!=host.to_ascii_lowercase() {return false;}
    if host.split('.').any(|label|label.is_empty()||label.len()>63||label.starts_with('-')||label.ends_with('-')
        ||!label.bytes().all(|b|b.is_ascii_lowercase()||b.is_ascii_digit()||b==b'-')) {return false;}
    let bytes=value.as_bytes();
    for (i,b) in bytes.iter().enumerate() {
        if *b==b'%' && !(bytes.get(i+1).is_some_and(u8::is_ascii_hexdigit)
            && bytes.get(i+2).is_some_and(u8::is_ascii_hexdigit)) {return false;}
    }
    true
}

/// Versioned exact-byte matching. The explicit list grants no host/path/query
/// variants. This parser is deliberately independent of legacy import syntax.
pub(crate) fn typed_values(policy:&Value)->Result<BTreeSet<String>,&'static str>{
    let object=policy.as_object().ok_or("Invalid typed reply URL policy")?;
    if object.len()!=4 || object.keys().any(|key|!["schemaVersion","operator","matching","values"].contains(&key.as_str()))
        || policy["schemaVersion"]!=1 || policy["operator"]!="allowed_reply_urls"
        || policy["matching"]!="exact_url_v1" {return Err("Invalid typed reply URL policy");}
    let values=policy["values"].as_array().filter(|values|values.len()<=100)
        .ok_or("Invalid typed reply URL policy")?;
    let mut allowed=BTreeSet::new();
    for value in values {
        let url=value.as_str().filter(|url|valid_exact_url(url)).ok_or("Invalid typed reply URL policy")?;
        if !allowed.insert(url.to_owned()){return Err("Duplicate typed reply URL");}
    }
    Ok(allowed)
}

fn typed_allowlist(
    context:&super::prepare_bundle::EvidenceContext<'_>,
    material:&Value,
    manifest:&[Value],
)->Result<Option<BTreeSet<String>>,&'static str>{
    let version_id=text(material,"knowledgeVersionId");
    if version_id.is_empty(){return Ok(None);}
    let canonical=context.knowledge_version(version_id)?;
    let typed=canonical["category"]=="reply_url_policy" || canonical.get("replyUrlPolicy").is_some()
        || material["policyType"]=="reply_url_policy" || material.get("replyUrlPolicy").is_some();
    if !typed {return Ok(None);}
    let account=text(context.workspace(),"account");
    let entry=text(material,"knowledgeEntryId");
    let matched:Vec<_>=manifest.iter().filter(|row|row["entryId"]==entry&&row["versionId"]==version_id).collect();
    if matched.len()!=1{return Err("Invalid typed reply URL policy");}
    let selected=matched[0];
    if account.is_empty() || material["kind"]!="policy" || material["trust"]!="verified"
        || material["policyType"]!="reply_url_policy" || canonical["kind"]!="policy"
        || canonical["trust"]!="verified" || canonical["status"]!="active"
        || canonical["category"]!="reply_url_policy" || canonical["grantsExecutionAuthority"]!=false
        || canonical["scope"]["account"]!=account || canonical["scope"]["postKeys"]!=serde_json::json!([])
        || selected["scope"]!=canonical["scope"] || selected["kind"]!="policy"
        || canonical["scope"].as_object().is_none_or(|scope|scope.len()!=2
            ||!scope.contains_key("account")||!scope.contains_key("postKeys"))
        || selected["trust"]!="verified" || selected["hash"]!=canonical["hash"]
        || canonical["entryId"]!=entry || canonical["id"]!=version_id
        || canonical["replyUrlPolicy"]!=material["replyUrlPolicy"]
        || canonical["text"]!=material["text"]
    {return Err("Invalid typed reply URL policy");}
    typed_values(&canonical["replyUrlPolicy"]).map(Some)
}

fn selected_head(
    context:&super::prepare_bundle::EvidenceContext<'_>,
    materials:&[Value],manifest:&[Value]
)->Result<Option<PolicyHead>,&'static str>{
    let mut heads=Vec::new();
    for material in materials {
        if let Some(values)=typed_allowlist(context,material,manifest)? {
            heads.push(PolicyHead{source:PolicySource::Typed,entry_id:text(material,"knowledgeEntryId").to_owned(),
                version_id:text(material,"knowledgeVersionId").to_owned(),values});
        }
        if let Some(values)=imported_allowlist(context,material,manifest)? {
            heads.push(PolicyHead{source:PolicySource::LegacyImport,entry_id:text(material,"knowledgeEntryId").to_owned(),
                version_id:text(material,"knowledgeVersionId").to_owned(),values});
        }
    }
    match heads.len(){0=>Ok(None),1=>Ok(heads.pop()),_=>Err("Conflicting reply URL policies")}
}

pub(crate) fn company_head(d:&Value,at:&str)->Result<Option<PolicyHead>,&'static str>{
    let selected=super::knowledge::select(d,&[],&[],at)?;
    let context=super::prepare_bundle::EvidenceContext::new(d);
    selected_head(&context,rows(&selected,"materials"),rows(&selected,"manifest"))
}

fn urls(reply: &str) -> Vec<&str> {
    let mut result = Vec::new();
    let mut cursor = 0;
    while cursor < reply.len() {
        let tail = &reply[cursor..];
        let http = tail.find("http://");
        let https = tail.find("https://");
        let Some(relative) = (match (http, https) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (Some(a), None) | (None, Some(a)) => Some(a),
            (None, None) => None,
        }) else {
            break;
        };
        let start = cursor + relative;
        let candidate = &reply[start..];
        let end = candidate
            .char_indices()
            .find_map(|(index, ch)| {
                (index > 0 && (ch.is_whitespace() || matches!(ch, '<' | '>'))).then_some(index)
            })
            .unwrap_or(candidate.len());
        let raw = &candidate[..end];
        let trimmed = raw.trim_end_matches(|ch| {
            matches!(ch, '.' | ',' | '!' | '?' | ';' | ':' | ')' | ']' | '}')
        });
        let prefix = if raw.starts_with("https://") { 8 } else { 7 };
        if raw.len() > prefix {
            result.push(trimmed);
        }
        cursor = start + end;
    }
    result
}

pub(crate) fn typed_urls(reply:&str)->Result<Vec<&str>,&'static str>{
    let mut result=Vec::new();
    let bytes=reply.as_bytes();
    let mut covered=vec![false;bytes.len()];
    let mut cursor=0;
    while cursor<bytes.len() {
        let Some(start)=(cursor..bytes.len()).find(|&i|{
            bytes.get(i..i+7).is_some_and(|part|part.eq_ignore_ascii_case(b"http://"))
                || bytes.get(i..i+8).is_some_and(|part|part.eq_ignore_ascii_case(b"https://"))
        }) else {break};
        let candidate=&reply[start..];
        let end=candidate.char_indices().find_map(|(i,ch)|
            (i>0&&(ch.is_whitespace()||matches!(ch,'<'|'>'))).then_some(i)).unwrap_or(candidate.len());
        let raw=&candidate[..end];
        let url=raw.trim_end_matches(|ch|matches!(ch,'.'|','|'!'|'?'|';'|':'|')'|']'|'}'));
        if !valid_exact_url(url){return Err(FORBIDDEN);}
        result.push(url);
        covered[start..start+end].fill(true);
        cursor=start+end;
    }
    let lower=reply.to_ascii_lowercase();
    let unsupported=["www.","http:","https:","mailto:","ftp:","javascript:",
        "data:","file:","tel:","ws:","wss:","blob:"];
    if unsupported.iter().any(|needle|lower.match_indices(needle).any(|(i,_)|
        !covered[i] && (i==0 || !matches!(bytes[i-1],b'a'..=b'z'|b'A'..=b'Z'|b'0'..=b'9'|b'+'|b'.'|b'-'))))
        || bytes.windows(3).enumerate().any(|(i,part)|part==b"://"&&!covered[i])
        || bytes.windows(2).enumerate().any(|(i,pair)|pair==b"//"&&!covered[i]&&(i==0||bytes[i-1]!=b':')) {
        return Err(FORBIDDEN);
    }
    Ok(result)
}

/// Reject a reply only from an applicable canonical imported exact-URL allowlist.
/// The imported account card has one field. Duplicate applicable heads are an
/// unresolved policy conflict, never an invented union or intersection.
pub(crate) fn validate_reply(
    context: &super::prepare_bundle::EvidenceContext<'_>,
    item: &Value,
    reply: &str,
) -> Result<(), &'static str> {
    let item_id = text(item, "id");
    if item_id.is_empty() {
        return Err("Reply item is missing");
    }
    let selected = context.evidence_for_item(item_id)?;
    let Some(policy)=selected_head(context,rows(&selected,"materials"),rows(&selected,"knowledgeManifest"))? else {return Ok(())};
    let found=match policy.source {PolicySource::Typed=>typed_urls(reply)?,PolicySource::LegacyImport=>urls(reply)};
    if found.into_iter().all(|url| policy.values.contains(url)) {
        Ok(())
    } else {
        Err(FORBIDDEN)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};
    use sha2::{Digest, Sha256};

    fn hash(value: &Value) -> String {
        format!("{:x}", Sha256::digest(value.to_string().as_bytes()))
    }
    fn version_hash(value: &Value) -> String {
        let mut copy = value.clone();
        for key in ["id", "hash", "createdAt"] {
            copy.as_object_mut().unwrap().remove(key);
        }
        hash(&copy)
    }
    fn add_rule(
        d: &mut Value,
        id: &str,
        account: &str,
        company: &str,
        urls: Vec<&str>,
        scope: Vec<&str>,
        from: &str,
        until: Value,
    ) {
        let entry = format!("entry-{id}");
        let source = format!("source-{id}");
        let marker = json!({"field":"allowed_reply_urls","source":format!("configs/commentops-fast/{company}.json")});
        let import_scope = json!({"companyKey":company});
        let values = json!(urls);
        let imported = json!({"companyKey":company,"importKey":id,"recordSha256":"a".repeat(64),"source":{"origin":"commentops-fast.account-card","sha256":"b".repeat(64),"originalIds":marker},"scope":import_scope,"metadata":{"category":"brand_policy","grantsExecutionAuthority":false,"value":values,"legacyProvenance":marker,"legacyScope":import_scope}});
        let mut version = json!({"entryId":entry,"sourceMaterialId":source,"sourceRevision":1,"sourceHash":"c".repeat(64),"title":"Imported rule evidence","text":serde_json::to_string_pretty(&values).unwrap(),"sourceUrl":"","postKey":"","kind":"rule","scope":{"account":account,"postKeys":scope},"category":"brand_policy","trust":"imported_policy","status":"active","validFrom":from,"validUntil":until,"sourceDate":null,"supersedes":null,"createdAt":"2026-09-24T00:00:00Z","companyImport":imported,"grantsExecutionAuthority":false});
        let digest = version_hash(&version);
        version["hash"] = json!(digest);
        version["id"] = json!(format!("version-{id}"));
        d["knowledge_entries"].as_array_mut().unwrap().push(json!({"id":entry,"sourceMaterialId":source,"currentVersionId":version["id"],"kind":"rule","scope":version["scope"],"status":"active"}));
        d["knowledge_versions"]
            .as_array_mut()
            .unwrap()
            .push(version);
    }
    fn workspace() -> Value {
        let mut value = json!({"account":"BAW Russia","items":[{"id":"item","postKey":"post"}],"posts":[],"branches":[],"materials":[],"knowledge_entries":[],"knowledge_versions":[]});
        value["connectorBinding"] = crate::accounts::Profile::BawRussia.binding();
        value
    }
    fn check(d: &Value, reply: &str) -> Result<(), &'static str> {
        let context = super::super::prepare_bundle::EvidenceContext::new(d);
        validate_reply(&context, &d["items"][0], reply)
    }

    #[test]
    fn exact_allowed_url_passes_and_forbidden_manufacturer_url_fails() {
        let mut d = workspace();
        add_rule(
            &mut d,
            "allowed",
            "BAW Russia",
            "baw-russia",
            vec!["https://bawrussia.ru/", "https://bawrussia.ru/dealers"],
            vec![],
            "2026-09-01T00:00:00Z",
            Value::Null,
        );
        assert_eq!(check(&d, "Список: https://bawrussia.ru/dealers."), Ok(()));
        assert_eq!(
            check(&d, "Источник https://manufacturer.example/specs"),
            Err(FORBIDDEN)
        );
        assert_eq!(check(&d, "Без ссылки"), Ok(()));
    }

    #[test]
    fn company_and_post_scope_use_canonical_selection() {
        let mut d = workspace();
        add_rule(
            &mut d,
            "foreign",
            "LikeAvto",
            "likeavto",
            vec![],
            vec![],
            "2026-09-01T00:00:00Z",
            Value::Null,
        );
        add_rule(
            &mut d,
            "other-post",
            "BAW Russia",
            "baw-russia",
            vec![],
            vec!["other"],
            "2026-09-01T00:00:00Z",
            Value::Null,
        );
        assert_eq!(check(&d, "https://manufacturer.example/specs"), Ok(()));
        add_rule(
            &mut d,
            "this-post",
            "BAW Russia",
            "baw-russia",
            vec!["https://allowed.example/exact"],
            vec!["post"],
            "2026-09-01T00:00:00Z",
            Value::Null,
        );
        assert_eq!(
            check(&d, "https://manufacturer.example/specs"),
            Err(FORBIDDEN)
        );
    }

    #[test]
    fn only_current_active_head_constrains_reply() {
        let mut d = workspace();
        add_rule(
            &mut d,
            "old",
            "BAW Russia",
            "baw-russia",
            vec![],
            vec![],
            "2026-09-01T00:00:00Z",
            Value::Null,
        );
        let old = d["knowledge_versions"][0].clone();
        let mut replacement = old.clone();
        replacement["companyImport"]["source"]["originalIds"]["field"] = json!("reply_playbook/4");
        replacement["text"] = json!("Use evidence.");
        replacement["trust"] = json!("imported_policy");
        replacement["supersedes"] = old["id"].clone();
        replacement["id"] = Value::Null;
        replacement["hash"] = Value::Null;
        let digest = version_hash(&replacement);
        replacement["hash"] = json!(digest);
        replacement["id"] = json!("version-replacement");
        d["knowledge_entries"][0]["currentVersionId"] = replacement["id"].clone();
        d["knowledge_versions"]
            .as_array_mut()
            .unwrap()
            .push(replacement);
        assert_eq!(check(&d, "https://manufacturer.example/specs"), Ok(()));
    }

    #[test]
    fn absent_and_empty_allowlists_are_distinct_and_duplicate_heads_conflict() {
        let d = workspace();
        assert_eq!(check(&d, "https://manufacturer.example/specs"), Ok(()));
        let mut empty = workspace();
        add_rule(
            &mut empty,
            "empty",
            "BAW Russia",
            "baw-russia",
            vec![],
            vec![],
            "2026-09-01T00:00:00Z",
            Value::Null,
        );
        assert_eq!(check(&empty, "plain text"), Ok(()));
        assert_eq!(check(&empty, "https://bawrussia.ru/"), Err(FORBIDDEN));
        let mut intersection = workspace();
        add_rule(
            &mut intersection,
            "one",
            "BAW Russia",
            "baw-russia",
            vec!["https://one.example", "https://shared.example"],
            vec![],
            "2026-09-01T00:00:00Z",
            Value::Null,
        );
        add_rule(
            &mut intersection,
            "two",
            "BAW Russia",
            "baw-russia",
            vec!["https://shared.example", "https://two.example"],
            vec![],
            "2026-09-01T00:00:00Z",
            Value::Null,
        );
        assert_eq!(check(&intersection, "plain text"), Err("Conflicting reply URL policies"));
    }

    #[test]
    fn imported_head_can_be_replaced_by_typed_successor_without_combining_lists() {
        let mut d=workspace();
        add_rule(&mut d,"allowed","BAW Russia","baw-russia",
            vec!["https://old.example/"],vec![],"2026-09-01T00:00:00Z",Value::Null);
        d["materials"]=json!([{"id":"source-allowed","revision":1,"kind":"rule",
            "text":"[\"https://old.example/\"]"}]);
        let old=d["knowledge_entries"][0]["currentVersionId"].clone();
        let entry=d["knowledge_entries"][0]["id"].clone();
        d["knowledge_entries"][0]["companyImportReceipts"]=json!([{"importKey":"legacy-allowed","recordSha256":"historical"}]);
        assert_eq!(check(&d,"https://old.example/"),Ok(()));
        let saved=crate::knowledge::reply_url_policy::save(&mut d,&json!({"requestId":"migrate",
            "expectedVersionId":old,"values":["https://new.example/"]}),"2026-09-24T12:00:00Z").unwrap();
        assert_eq!(saved["entryId"],entry);
        assert_eq!(d["knowledge_entries"][0]["companyImportReceipts"],json!([{"importKey":"legacy-allowed","recordSha256":"historical"}]));
        assert_eq!(d["knowledge_entries"].as_array().unwrap().len(),1);
        assert_eq!(d["knowledge_versions"].as_array().unwrap().len(),2);
        assert_eq!(d["knowledge_versions"][0]["trust"],"imported_policy");
        assert_eq!(d["knowledge_versions"][1]["supersedes"],old);
        assert_eq!(check(&d,"https://old.example/"),Err(FORBIDDEN));
        assert_eq!(check(&d,"https://new.example/"),Ok(()));
    }

    #[test]
    fn punctuation_query_and_case_match_the_legacy_literal_extractor() {
        let mut d = workspace();
        add_rule(
            &mut d,
            "allowed",
            "BAW Russia",
            "baw-russia",
            vec!["https://Example.com/A?x=1", "https://example.com/end"],
            vec![],
            "2026-09-01T00:00:00Z",
            Value::Null,
        );
        assert_eq!(
            check(&d, "(https://Example.com/A?x=1), https://example.com/end]}"),
            Ok(())
        );
        for reply in [
            "https://example.com/A?x=1",
            "https://Example.com/A?x=2",
            "https://Example.com/A?x=1/extra",
        ] {
            assert_eq!(check(&d, reply), Err(FORBIDDEN), "{reply}");
        }
        assert_eq!(check(&d, "HTTPS://manufacturer.example/specs"), Ok(()));
    }

    #[test]
    fn malformed_recognized_rule_fails_closed_while_expired_rule_is_not_selected() {
        let mut malformed = workspace();
        add_rule(
            &mut malformed,
            "bad",
            "BAW Russia",
            "baw-russia",
            vec!["https://bawrussia.ru/"],
            vec![],
            "2026-09-01T00:00:00Z",
            Value::Null,
        );
        malformed["knowledge_versions"][0]["companyImport"]["metadata"]["value"] =
            json!(["changed"]);
        let digest = version_hash(&malformed["knowledge_versions"][0]);
        malformed["knowledge_versions"][0]["hash"] = json!(digest);
        assert_eq!(check(&malformed, "plain text"), Err(INVALID));
        let mut expired = workspace();
        add_rule(
            &mut expired,
            "expired",
            "BAW Russia",
            "baw-russia",
            vec![],
            vec![],
            "2026-09-01T00:00:00Z",
            json!("2026-09-02T00:00:00Z"),
        );
        assert_eq!(
            check(&expired, "https://manufacturer.example/specs"),
            Ok(())
        );
    }
}
