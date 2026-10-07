//! Versioned knowledge evidence. Importing and feedback never grant publication authority.
use chrono::{DateTime, Utc};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::cell::OnceCell;
#[cfg(test)]
thread_local! { static VALIDATIONS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) }; }
#[cfg(test)]
pub(crate) fn validation_count()->usize{VALIDATIONS.with(|count|count.get())}
#[path = "company_knowledge_import.rs"]
pub mod company_import;
pub mod rule_normalization;
pub mod rule_revision;
pub(crate) mod reply_url_policy;

fn rows<'a>(d: &'a Value, key: &str) -> &'a [Value] {
    d[key].as_array().map(Vec::as_slice).unwrap_or(&[])
}
fn text<'a>(v: &'a Value, key: &str) -> &'a str {
    v[key].as_str().unwrap_or("")
}
fn hash(v: &Value) -> String {
    sha(v.to_string().as_bytes())
}
fn sha(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn account(d: &Value) -> &str {
    d["connectorBinding"]["accountId"]
        .as_str()
        .or(d["account"].as_str())
        .unwrap_or("")
}
fn supported_account(d: &Value) -> Result<&str, &'static str> {
    let active=account(d);
    if d["account"].as_str()!=Some(active) || active.is_empty() {
        return Err("Knowledge account is not configured");
    }
    if let Some(value)=d.get("connectorBinding") {
        if let Ok(binding)=crate::connectors::ConnectorBinding::from_json(value) {
            binding.validate_scope("local-pilot",active)
                .map_err(|_|"Knowledge account is not configured")?;
            return Ok(active);
        }
        // Earlier single-company imports persisted only these three Angry.Space
        // fields. Keep them readable for the two legacy companies, without
        // admitting a partial or foreign binding for new company policy edits.
        let legacy_key=match active {"LikeAvto"=>"likeavto","BAW Russia"=>"baw-russia",
            _=>return Err("Knowledge account is not configured")};
        if value.as_object().is_some_and(|fields|fields.len()==3
            && fields.keys().all(|key|matches!(key.as_str(),"connector"|"accountId"|"providerAccountId")))
            && value["connector"]=="angryspace" && value["accountId"]==active
            && value["providerAccountId"]==legacy_key {
            return Ok(active);
        }
        return Err("Knowledge account is not configured");
    }
    // Existing unbound pilot fixtures and legacy LikeAvto data remain readable;
    // a newly onboarded company needs an explicit connector binding.
    match active {"LikeAvto"|"BAW Russia"=>Ok(active),_=>Err("Knowledge account is not configured")}
}
fn content(m: &Value) -> Value {
    let mut value = json!({"title":text(m,"title"),"text":text(m,"text"),"sourceUrl":text(m,"sourceUrl"),"postKey":text(m,"postKey"),"kind":text(m,"kind"),"sourceDate":m["sourceDate"]});
    if let Some(policy)=m.get("replyUrlPolicy") {value["replyUrlPolicy"]=policy.clone();}
    if let Some(availability)=m.get("sourceAvailability") {value["sourceAvailability"]=availability.clone();}
    copy_media_identity(m, &mut value);
    value
}

fn copy_media_identity(source: &Value, target: &mut Value) {
    if !matches!(text(source, "kind"), "transcript" | "ocr" | "visual_context") { return; }
    for key in ["canonicalMediaId", "contentSha256", "mediaSha256", "attachments", "account", "accountId", "connectorBinding", "transcription", "visualEvidence", "ocr"] {
        if let Some(value) = source.get(key) { target[key] = value.clone(); }
    }
    // Catalog scope is owned by admission. Preserve a source-declared scope
    // separately so that importing it cannot erase a foreign-account boundary.
    if let Some(scope) = source.get("sourceMediaScope").or_else(|| {
        if source.get("sourceMaterialId").is_none() { source.get("scope") } else { None }
    }) { target["sourceMediaScope"] = scope.clone(); }
}

// These identifiers are evidence, never guesses from titles, thumbnails or text links.
fn canonical_video_url(raw: &str) -> Option<String> {
    let (scheme, rest) = raw.trim().split_once("://")?;
    if !matches!(scheme.to_ascii_lowercase().as_str(), "http" | "https") { return None; }
    let (host, suffix) = rest.split_once('/').unwrap_or((rest, ""));
    let host = host.to_ascii_lowercase();
    // Exact host allowlists also reject credentials, ports and lookalike domains.
    let suffix = suffix.split('#').next()?;
    let (path, query) = suffix.split_once('?').unwrap_or((suffix, ""));
    let path = path.trim_end_matches('/');
    let id_chars = |id: &str| !id.is_empty() && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-');
    if matches!(host.as_str(), "youtube.com" | "www.youtube.com" | "m.youtube.com" | "music.youtube.com" | "youtu.be") {
        let id = if host == "youtu.be" {
            path
        } else if path == "watch" {
            let values: Vec<_> = query.split('&').filter_map(|part| part.strip_prefix("v=")).collect();
            if values.len() != 1 { return None; }
            values[0]
        } else {
            path.strip_prefix("shorts/").or_else(|| path.strip_prefix("embed/"))?
        };
        return (id.len() == 11 && id_chars(id)).then(|| format!("yt:{id}"));
    }
    if matches!(host.as_str(), "vk.com" | "www.vk.com" | "m.vk.com" | "vkvideo.ru" | "www.vkvideo.ru") {
        let id = path.strip_prefix("video")?;
        let (owner, video) = id.split_once('_')?;
        let owner_digits = owner.strip_prefix('-').unwrap_or(owner);
        if !owner_digits.is_empty() && owner_digits.bytes().all(|b| b.is_ascii_digit()) && !video.is_empty() && video.bytes().all(|b| b.is_ascii_digit()) {
            return Some(format!("vk:{id}"));
        }
    }
    if matches!(host.as_str(), "instagram.com" | "www.instagram.com") {
        let id = path.strip_prefix("reel/").or_else(|| path.strip_prefix("reels/")).or_else(|| path.strip_prefix("p/"))?;
        if id_chars(id) { return Some(format!("ig:{id}")); }
    }
    None
}

pub(crate) fn in_account(record: &Value, expected: &str) -> bool {
    [record.get("account"), record.get("accountId"), record["scope"].get("account"), record["sourceMediaScope"].get("account"), record["connectorBinding"].get("accountId")]
        .into_iter().flatten().all(|v| v.as_str().is_some_and(|s| s == expected))
}

fn video_attachment(value: &Value) -> bool {
    matches!(text(value, "type"), "video" | "reel" | "clip")
        || text(value, "mimeType").starts_with("video/")
}

/// One video predicate for grouping, reuse and preparation gating. A plain
/// Instagram /p link may be a photo and needs an explicit video attachment.
pub(crate) fn is_video_post(post: &Value) -> bool {
    if rows(post, "attachments").iter().any(video_attachment) { return true; }
    ["sourceUrl", "attachmentSourceUrl", "url"].iter().any(|key| {
        let raw = text(post, key).to_ascii_lowercase();
        let Some((scheme, rest)) = raw.split_once("://") else { return false; };
        if !matches!(scheme, "http" | "https") { return false; }
        let (host, path) = rest.split_once('/').unwrap_or((rest, ""));
        match host {
            "youtu.be" => !path.is_empty(),
            "youtube.com" | "www.youtube.com" | "m.youtube.com" | "music.youtube.com" =>
                path.starts_with("watch?") || path.starts_with("shorts/") || path.starts_with("embed/"),
            "vk.com" | "www.vk.com" | "m.vk.com" | "vk.ru" | "www.vk.ru" | "vkvideo.ru" | "www.vkvideo.ru" =>
                path.starts_with("video") || path.starts_with("clip"),
            "instagram.com" | "www.instagram.com" => path.starts_with("reel/") || path.starts_with("reels/"),
            "tiktok.com" | "www.tiktok.com" | "vm.tiktok.com" | "vt.tiktok.com" => !path.is_empty(),
            _ => false,
        }
    })
}

fn media_identities(record: &Value, expected_account: &str) -> BTreeSet<String> {
    let mut result = BTreeSet::new();
    if !in_account(record, expected_account) { return result; }
    let mut add = |value: &Value, attachment: bool| {
        if !in_account(value, expected_account) { return; }
        for key in if attachment { &["sourceUrl", "source_url", "url"][..] } else { &["sourceUrl"][..] } {
            if let Some(id) = canonical_video_url(text(value, key)) { result.insert(id); }
        }
        let canonical = text(value, "canonicalMediaId");
        if !canonical.trim().is_empty() { result.insert(format!("canonical:{canonical}")); }
        for key in ["contentSha256", "mediaSha256"] {
            let digest = text(value, key);
            if digest.len() == 64 && digest.bytes().all(|b| b.is_ascii_hexdigit()) { result.insert(format!("sha:{}", digest.to_ascii_lowercase())); }
        }
    };
    add(record, false);
    for attachment in rows(record, "attachments").iter().filter(|v| video_attachment(v)) { add(attachment, true); }
    result
}

fn media_duration_ms(value: &Value) -> Option<u64> {
    value["durationMs"].as_u64().filter(|n|*n>0).or_else(||
        value["durationSeconds"].as_f64().filter(|n|n.is_finite()&&*n>0.0)
            .map(|n|(n*1000.0).round() as u64))
}

// A locator observation never verifies a dealer/contact fact. No network read
// occurs here; the explicit operator report is versioned with its exact URL.
fn validate_source_availability(value:&Value,url:&str,at:Option<&str>)->Result<(),&'static str>{
    let fields=value.as_object().ok_or("Source availability must be an object")?;
    if fields.len()!=8 || fields.keys().any(|key|!["schemaVersion","sourceUrl","status","basis","checkedAt","reasonCode","httpStatus","implication"].contains(&key.as_str()))
        || value["schemaVersion"]!=1 || value["sourceUrl"]!=url || url.is_empty()
        || !matches!(text(value,"basis"),"owner_reported"|"operator_reported")
        || value["implication"]!="locator_only" {return Err("Invalid source availability contract");}
    locator_url(url)?;
    let checked=timestamp(text(value,"checkedAt"))?;
    if at.map(timestamp).transpose()?.is_some_and(|limit|checked>limit){return Err("Source availability observation is in the future");}
    let code=value["httpStatus"].as_u64();
    if !value["httpStatus"].is_null() && code.is_none(){return Err("Invalid locator HTTP status");}
    let valid=match text(value,"status") {
        "available"=>value["reasonCode"]=="public_locator_available" && code.is_none_or(|code|(200..400).contains(&code)),
        "unavailable"=>value["reasonCode"]=="public_locator_unavailable" && code.is_none_or(|code|(400..600).contains(&code)),
        "unknown"=>value["reasonCode"]=="public_locator_unverified" && code.is_none(),
        _=>false,
    };
    if !valid{return Err("Source availability status and reason differ");}
    Ok(())
}
fn locator_url(raw:&str)->Result<(),&'static str>{
    if raw.len()>2048 || raw.trim()!=raw || raw.chars().any(char::is_whitespace) || raw.contains('\\') || raw.contains('#') {
        return Err("Fact source URL must be a bounded public HTTPS locator");
    }
    let uri=raw.parse::<axum::http::Uri>().map_err(|_|"Fact source URL must be a bounded public HTTPS locator")?;
    let authority=uri.authority().ok_or("Fact source URL requires an authority")?;
    if uri.scheme_str()!=Some("https") || authority.as_str().contains('@') || uri.host().is_none_or(str::is_empty){
        return Err("Fact source URL must be a bounded public HTTPS locator");
    }
    Ok(())
}
fn duration_conflict(a:Option<u64>,b:Option<u64>)->bool {
    a.zip(b).is_some_and(|(a,b)|a.abs_diff(b)>1000)
}
pub(crate) fn media_conflict(a: &Value, b: &Value, scope: &str) -> bool {
    if duration_conflict(media_duration_ms(a),media_duration_ms(b)){return true;}
    let a = media_identities(a,scope);
    let b = media_identities(b,scope);
    ["sha:","canonical:"].iter().any(|prefix| {
        let x: BTreeSet<_> = a.iter().filter(|s| s.starts_with(prefix)).collect();
        let y: BTreeSet<_> = b.iter().filter(|s| s.starts_with(prefix)).collect();
        !x.is_empty() && !y.is_empty() && x.is_disjoint(&y)
    })
}
// Queue coverage consumers share this key, so it must establish source identity.
// Titles never establish evidence identity or suppress another source's work.
pub(crate) fn media_group_key(post: &Value, scope: &str) -> Option<String> {
    if !in_account(post,scope) { return None; }
    let identities = media_identities(post,scope);
    let video = is_video_post(post) || !identities.is_empty();
    if !video { return None; }
    identities.iter().find(|id|id.starts_with("sha:"))
        .or_else(||identities.iter().find(|id|id.starts_with("canonical:")))
        .or_else(||identities.iter().next()).map(|id| format!("{scope}:{id}"))
        .or_else(|| (!text(post,"postKey").is_empty()).then(|| format!("{scope}:post:{}",text(post,"postKey"))))
}

/// Provider video identity for bounding download attempts across duplicate post rows.
/// It deliberately does not collapse different platforms through a shared title.
pub(crate) fn media_source_key(post: &Value, scope: &str) -> Option<String> {
    if !in_account(post,scope) { return None; }
    for attachment in rows(post,"attachments").iter().filter(|a|video_attachment(a)) {
        for key in ["sourceUrl","source_url","url"] {
            if let Some(id)=canonical_video_url(text(attachment,key)) { return Some(id); }
        }
    }
    canonical_video_url(text(post,"sourceUrl")).or_else(|| (!text(post,"postKey").is_empty()).then(||format!("post:{}",text(post,"postKey"))))
}

pub(crate) fn post_has_transcript(d: &Value, post: &Value, at: &str) -> Result<bool, &'static str> {
    TranscriptLookup::new(d, at)?.has(post)
}

#[derive(Clone)]
struct MediaFacts {
    identities: BTreeSet<String>,
    in_account: bool,
    duration_ms: Option<u64>,
}
impl MediaFacts {
    fn new(post: &Value, scope: &str) -> Self {
        let identities = media_identities(post, scope);
        Self { in_account: in_account(post, scope), duration_ms:media_duration_ms(post), identities }
    }
}
fn facts_conflict(a:&MediaFacts,b:&MediaFacts)->bool {
    identity_conflict(&a.identities,&b.identities)||duration_conflict(a.duration_ms,b.duration_ms)
}
fn identity_conflict(a: &BTreeSet<String>, b: &BTreeSet<String>) -> bool {
    ["sha:", "canonical:"].iter().any(|prefix| {
        let x: BTreeSet<_> = a.iter().filter(|s| s.starts_with(prefix)).collect();
        let y: BTreeSet<_> = b.iter().filter(|s| s.starts_with(prefix)).collect();
        !x.is_empty() && !y.is_empty() && x.is_disjoint(&y)
    })
}

/// Compute queue groups once per snapshot, retaining the conservative split of
/// every member when a group contains conflicting explicit video identities.
pub(crate) fn media_groups(d: &Value, scope: &str) -> BTreeMap<String, String> {
    let mut peers: BTreeMap<String, Vec<(&Value, MediaFacts)>> = BTreeMap::new();
    for post in rows(d, "posts") {
        if let Some(key) = media_group_key(post, scope) { peers.entry(key).or_default().push((post, MediaFacts::new(post, scope))); }
    }
    let mut result = BTreeMap::new();
    for (key, group) in peers {
        let conflict = group.iter().enumerate().any(|(i, (_, a))| group.iter().skip(i + 1).any(|(_, b)| facts_conflict(a,b)));
        for (post, _) in group {
            let id = text(post, "id");
            result.insert(id.to_owned(), if conflict { format!("{key}:post:{id}") } else { key.clone() });
        }
    }
    result
}

/// Scheduling affinity only. Reuse the current complete transcript admission
/// relation, but never the title fallback. This grants no evidence to a sibling:
/// each post still has its own capture, images, research and dependency digest.
pub(crate) fn preparation_families(d:&Value,post_ids:&BTreeSet<String>,at:&str)->Result<BTreeMap<String,String>,&'static str>{
    Ok(preparation_family_evidence(d,post_ids,at)?.families)
}

/// The exact proof used by the existing affinity policy, without source text or
/// donor posts entering the answering selection. This is evidence, not a ledger.
pub(crate) struct PreparationFamilyEvidence {
    pub(crate) families:BTreeMap<String,String>,
    pub(crate) supports:BTreeMap<String,Value>,
}
pub(crate) fn preparation_family_evidence(d:&Value,post_ids:&BTreeSet<String>,at:&str)->Result<PreparationFamilyEvidence,&'static str>{
    fn support(v:&Value)->String { json!([v["entryId"],v["id"],v["hash"]]).to_string() }
    let scope=supported_account(d)?;let time=timestamp(at)?;
    let binding=super::active_binding(d).map_err(|_|"Preparation account unavailable")?.to_json();
    let posts:Vec<_>=rows(d,"posts").iter().filter(|p|post_ids.contains(text(p,"id"))
        &&in_account(p,scope)&&(p["connectorBinding"].is_null()||p["connectorBinding"]==binding)).collect();
    let mut supports:BTreeMap<String,BTreeSet<String>>=posts.iter().map(|p|(text(p,"id").to_owned(),BTreeSet::new())).collect();
    let mut proofs:BTreeMap<String,BTreeMap<String,Value>>=posts.iter().map(|p|(text(p,"id").to_owned(),BTreeMap::new())).collect();
    // The resolver checks current donor head/hash, source versions, company,
    // full coverage, active time and the owner assertion. An unselected donor
    // may connect two selected targets without entering their model context.
    for edge in crate::media_audio_equivalence::bindings(d,at)? {
        let anchor=json!([edge["transcript"]["entryId"],edge["transcript"]["versionId"],edge["transcript"]["hash"]]).to_string();
        for field in ["targetPostId","sourcePostId"] {
            if let Some(signatures)=supports.get_mut(text(&edge,field)){
                signatures.insert(anchor.clone());
                proofs.get_mut(text(&edge,field)).unwrap().insert(hash(&edge),edge.clone());
            }
        }
    }
    let media=MediaLookup::new(d,scope);
    let targets:Vec<_>=posts.iter().map(|p|(*p,MediaFacts::new(p,scope))).collect();
    let versions:BTreeMap<_,_>=rows(d,"knowledge_versions").iter().map(|v|(text(v,"id"),v)).collect();
    for entry in rows(d,"knowledge_entries") {
        let Some(v)=versions.get(text(entry,"currentVersionId")) else{continue;};
        if v["kind"]!="transcript"||v["status"]!="active"||text(v,"text").trim().is_empty()
            ||!matches!(text(v,"trust"),"source_only"|"verified")||v["scope"]["account"]!=scope
            ||v.get("companyImport").is_some()||!media.proven_full_audio_source(v,scope){continue;}
        let from=v["validFrom"].as_str().map(timestamp).transpose()?;
        let until=v["validUntil"].as_str().map(timestamp).transpose()?;
        if from.is_some_and(|t|t>time)||until.is_some_and(|t|t<=time){continue;}
        let allowed:BTreeSet<String>=rows(&v["scope"],"postKeys").iter().filter_map(|k|k.as_str().map(str::to_owned)).collect();
        let members:Vec<_>=targets.iter().filter(|(post,facts)| {
            let exact=v["postKey"]==post["postKey"] || media.shared_binding(scope,v,&[(*post,facts.clone())])
                .iter().any(|b|!rows(b,"identities").is_empty());
            exact&&transcript_matches(v,post,scope,&media,&allowed)
                &&media.full_audio_target_binding(v,post,scope,&allowed)
        }).collect();
        // Unknown donor identity must not bridge contradictory selected copies.
        if members.iter().enumerate().any(|(i,(_,a))|members.iter().skip(i+1).any(|(_,b)|facts_conflict(a,b))){continue;}
        let anchor=support(v);
        for (post,_) in members {
            let id=text(post,"id");supports.get_mut(id).unwrap().insert(anchor.clone());
            proofs.get_mut(id).unwrap().insert(anchor.clone(),json!({"kind":"complete_transcript",
                "entryId":v["entryId"],"versionId":v["id"],"hash":v["hash"],
                "entryHeadSha256":hash(entry),"versionSha256":hash(v),
                "validFrom":v["validFrom"],"validUntil":v["validUntil"]}));
        }
    }
    // Equal complete support signatures may share a scheduling unit. A post
    // with T+U does not transitively merge a T-only and a U-only publication.
    // Missing evidence remains an ordinary individual post, with no new hold.
    let families=supports.into_iter().map(|(id,anchors)|{
        let key=if anchors.is_empty(){format!("post:{id}")}else{format!("speech:{}",hash(&json!(anchors)))};
        (id,key)
    }).collect();
    Ok(PreparationFamilyEvidence{families,supports:proofs.into_iter().map(|(id,pins)|(id,json!(pins.into_values().collect::<Vec<_>>()))).collect()})
}

/// Add already observed media hashes/durations to an ephemeral identity view.
/// This never retargets or writes the original post. Two observed differing
/// copies split a formerly source-equivalent group before further reuse.
// A generated visual title retains the title used by the original immutable
// source fingerprint. Only an exact cryptographic reconstruction can associate
// that proof after a title-only projection change; reply fingerprints stay strict.
fn visual_source_associated(v:&Value,post:&Value,scope:&str)->bool {
    if !in_account(v,scope)||!in_account(post,scope)||v["postKey"]!=post["postKey"]
        ||media_conflict(v,post,scope){return false;}
    let evidence=&v["visualEvidence"];let source=&evidence["source"];
    if source["account"]!=scope||source["postKey"]!=v["postKey"]
        ||source["mediaSha256"]!=v["mediaSha256"]{return false;}
    let expected=text(evidence,"sourcePostVersion");
    if expected==super::media_fullframes::source_version(post,scope){return true;}
    if v["kind"]!="visual_context"||v["status"]!="active"||v["trust"]!="source_only"
        ||v.get("companyImport").is_some(){return false;}
    let Some(old_title)=text(v,"title").strip_prefix("Visual context: ") else{return false;};
    if old_title.is_empty(){return false;}
    let mut original=post.clone();original["title"]=json!(old_title);
    expected==super::media_fullframes::source_version(&original,scope)
}
fn observed_video_posts(d:&Value,scope:&str)->Vec<Value>{
    let mut posts=rows(d,"posts").to_vec();
    let current:BTreeSet<_>=rows(d,"knowledge_entries").iter().map(|e|text(e,"currentVersionId")).collect();
    for v in rows(d,"knowledge_versions").iter().filter(|v|current.contains(text(v,"id"))&&v["kind"]=="visual_context"&&v["status"]=="active"&&in_account(v,scope)) {
        if super::media_fullframes::validate_evidence(&v["visualEvidence"]).is_err(){continue;}
        for post in posts.iter_mut().filter(|p|p["postKey"]==v["postKey"]&&in_account(p,scope)) {
            if !visual_source_associated(v,post,scope){continue;}
            if !post["mediaSha256"].is_string()&&!post["contentSha256"].is_string(){post["mediaSha256"]=v["mediaSha256"].clone();}
            if post.get("durationMs").is_none()&&post.get("durationSeconds").is_none(){post["durationMs"]=v["visualEvidence"]["source"]["durationMs"].clone();}
        }
    }
    posts
}
pub(crate) fn visual_groups(d:&Value,scope:&str)->BTreeMap<String,String>{
    let posts=observed_video_posts(d,scope);
    let view=json!({"posts":posts});
    media_groups(&view,scope)
}

struct TranscriptHead {
    value: Value,
    scope: BTreeSet<String>,
    source_identities: BTreeSet<String>,
}
struct MediaLookup {
    posts: BTreeMap<String, Vec<MediaFacts>>,
    current_source_posts: BTreeMap<String, Vec<Value>>,
    source_versions: BTreeMap<String,BTreeSet<String>>,
    associated_visual_heads: BTreeSet<String>,
}
impl MediaLookup {
    fn new(d:&Value,scope:&str)->Self {
        let mut posts: BTreeMap<String, Vec<MediaFacts>> = BTreeMap::new();
        let mut current_source_posts: BTreeMap<String, Vec<Value>> = BTreeMap::new();
        for post in rows(d,"posts").iter().filter(|p|in_account(p,scope)) {
            current_source_posts.entry(text(post,"postKey").to_owned()).or_default().push(post.clone());
        }
        for post in &observed_video_posts(d,scope) {
            let facts = MediaFacts::new(post, scope);
            posts.entry(text(post,"postKey").to_owned()).or_default().push(facts);
        }
        let mut source_versions:BTreeMap<String,BTreeSet<String>>=BTreeMap::new();
        for post in rows(d,"posts").iter().filter(|p|in_account(p,scope)){source_versions.entry(text(post,"postKey").to_owned()).or_default().insert(super::media_fullframes::source_version(post,scope));}
        let current:BTreeSet<_>=rows(d,"knowledge_entries").iter().map(|e|text(e,"currentVersionId")).collect();
        let associated_visual_heads=rows(d,"knowledge_versions").iter()
            .filter(|v|current.contains(text(v,"id"))&&v["kind"]=="visual_context"
                &&rows(d,"posts").iter().any(|post|visual_source_associated(v,post,scope)))
            .map(|v|text(v,"id").to_owned()).collect();
        Self{posts,current_source_posts,source_versions,associated_visual_heads}
    }
    fn proven_full_audio_source(&self,v:&Value,scope:&str)->bool{
        let key=text(v,"postKey");
        let facts=MediaFacts::new(v,scope);
        let observed_duration=v["transcription"]["mediaDurationSeconds"].as_f64()
            .map(|n|(n*1000.0).round() as u64);
        if self.posts.get(key).is_some_and(|known|known.iter().any(|post|
            facts_conflict(&facts,post)||duration_conflict(observed_duration,post.duration_ms))){return false;}
        self.current_source_posts.get(key).is_some_and(|posts|
            posts.iter().any(|post|proven_full_audio_for_source(v,post,scope)))
    }
    fn full_audio_target_binding(&self,v:&Value,post:&Value,scope:&str,allowed:&BTreeSet<String>)->bool{
        let key=text(post,"postKey");
        let observed_duration=v["transcription"]["mediaDurationSeconds"].as_f64()
            .map(|n|(n*1000.0).round() as u64);
        if duration_conflict(observed_duration,media_duration_ms(post))
            ||self.posts.get(key).is_some_and(|known|known.iter().any(|p|
                duration_conflict(observed_duration,p.duration_ms))){return false;}
        v["postKey"]==post["postKey"] || allowed.contains(key)
            || !self.shared_binding(scope,v,&[(post,MediaFacts::new(post,scope))]).is_empty()
    }
    fn shared_binding(&self,scope:&str,material:&Value,targets:&[(&Value,MediaFacts)])->Vec<Value>{
        if !matches!(text(material,"kind"),"transcript"|"ocr"|"visual_context")||!in_account(material,scope){return vec![];}
        let facts=MediaFacts::new(material,scope);
        let source_key=text(material,"postKey");
        let sources=if source_key.is_empty(){&[][..]}else{self.posts.get(source_key).map(Vec::as_slice).unwrap_or(&[])};
        let mut identities=facts.identities.clone();
        for source in sources{identities.extend(source.identities.iter().cloned());}
        let mut bindings=Vec::new();
        for (target,target_facts) in targets{
            if !target_facts.in_account||facts_conflict(&facts,target_facts)||sources.iter().any(|source|facts_conflict(source,target_facts)){continue;}
            let matches:Vec<_>=identities.intersection(&target_facts.identities).cloned().collect();
            if !matches.is_empty(){bindings.push(json!({"postKey":target["postKey"],"sourcePostKey":source_key,"identities":matches}));}
        }
        bindings.sort_by_key(Value::to_string);bindings.dedup();bindings
    }
}
/// Immutable snapshot-local index. Validate every catalog hash/head once, then
/// answer many gates without building/sorting/canonicalizing whole bundles.
pub(crate) struct TranscriptLookup {
    account: String,
    at: DateTime<Utc>,
    media: MediaLookup,
    heads: Vec<TranscriptHead>,
    audio_equivalences: Vec<Value>,
    exact_analyses: Vec<Value>,
}
fn full_audio_coverage(transcription:&Value)->bool{
    if transcription["partial"]!=false{return false;}
    let media=transcription["mediaDurationSeconds"].as_f64().filter(|n|n.is_finite()&&*n>0.0);
    let Some(media)=media else{return false;};
    match text(transcription,"coverage"){
        "full_audio"=>transcription["audioDurationSeconds"].as_f64()
            .is_some_and(|audio|audio.is_finite()&&audio>0.0&&audio+0.25>=media),
        "no_audio_stream"=>transcription["audioStatus"]=="no_audio_stream"
            &&transcription["audioDurationSeconds"].is_null(),
        _=>false,
    }
}
pub(crate) fn proven_full_audio(transcription:&Value,source_version:&str)->bool{
    transcription["sourceVersion"]==source_version && full_audio_coverage(transcription)
}
/// Older local-ASR imports predate sourceVersion, but carried the SHA256 of the
/// exact source locator used by the media processor. Prove that locator against
/// the current donor post; the separate target-binding check still governs reuse.
fn proven_full_audio_for_source(v:&Value,source_post:&Value,scope:&str)->bool{
    if v["kind"]!="transcript" || v["postKey"]!=source_post["postKey"]
        || !in_account(v,scope) || !in_account(source_post,scope){return false;}
    let transcription=&v["transcription"];
    if !full_audio_coverage(transcription){return false;}
    let observed_duration=transcription["mediaDurationSeconds"].as_f64()
        .map(|n|(n*1000.0).round() as u64);
    if duration_conflict(observed_duration,media_duration_ms(source_post)){return false;}
    if let Some(version)=transcription.get("sourceVersion"){
        return version.as_str().is_some_and(|s|s==super::media_fullframes::source_version(source_post,scope));
    }
    let locator=text(v,"sourceUrl");
    if v["account"]!=scope || locator.is_empty()
        || transcription["sourcePostKey"]!=source_post["postKey"]
        || transcription["sourceLocatorSha256"]!=sha(locator.as_bytes()){return false;}
    if locator==text(source_post,"sourceUrl"){return true;}
    // A social post may link to a wall page while its actual media processor
    // source is the exact video URL in an attachment. Admit that historical
    // locator only when the current post still has one unambiguous video
    // identity; duplicate URL aliases do not create multiple sources.
    let Some(identity)=canonical_video_url(locator) else{return false;};
    let mut identities=BTreeSet::new();
    let mut locators=BTreeSet::new();
    if let Some(id)=canonical_video_url(text(source_post,"sourceUrl")){identities.insert(id);}
    for attachment in rows(source_post,"attachments").iter().filter(|a|video_attachment(a)&&in_account(a,scope)){
        for field in ["sourceUrl","source_url","url"] {
            let url=text(attachment,field);
            if let Some(id)=canonical_video_url(url){identities.insert(id);locators.insert(url);}
        }
    }
    identities.len()==1 && identities.contains(&identity) && locators.contains(locator)
}
/// Readiness and selected evidence use the same source admission relation.
/// Unknown legacy coverage remains compatible; declared provenance never retargets.
fn transcript_matches(v:&Value,post:&Value,scope:&str,media:&MediaLookup,allowed:&BTreeSet<String>)->bool {
    if !in_account(v,scope)||!in_account(post,scope){return false;}
    let key=text(post,"postKey");
    let direct=allowed.is_empty()||allowed.contains(key);
    let target=MediaFacts::new(post,scope);let facts=MediaFacts::new(v,scope);
    if facts_conflict(&facts,&target){return false;}
    if let Some(known)=media.posts.get(key){if known.iter().any(|p|facts_conflict(&facts,p)){return false;}}
    let declared=&v["transcription"]["sourceVersion"];
    if !declared.is_null() {
        let Some(version)=declared.as_str() else{return false;};
        if direct && version!=super::media_fullframes::source_version(post,scope){return false;}
        if !direct && !media.source_versions.get(text(v,"postKey")).is_some_and(|versions|versions.contains(version)){return false;}
    }
    if direct{return true;}
    if v.get("companyImport").is_some(){return false;}
    !media.shared_binding(scope,v,&[(post,target)]).is_empty()
}
fn safe_head_id(id:&str)->String {
    if !id.is_empty()&&id.len()<=128&&id.bytes().all(|c|c.is_ascii_alphanumeric()||matches!(c,b'-'|b'_'|b'.'|b':')){id.to_owned()}
    else{format!("head-sha256:{}",sha(id.as_bytes()))}
}
impl TranscriptLookup {
    pub(crate) fn new(d: &Value, at: &str) -> Result<Self, &'static str> {
        validate(d)?;
        Self::build(d,at)
    }
    pub(crate) fn from_catalog(catalog:&Catalog<'_>,at:&str)->Result<Self,&'static str>{Self::build(catalog.workspace(),at)}
    fn build(d:&Value,at:&str)->Result<Self,&'static str>{
        let scope = supported_account(d)?;
        let media=MediaLookup::new(d,scope);
        let versions: BTreeMap<_,_> = rows(d,"knowledge_versions").iter().map(|v|(text(v,"id"),v)).collect();
        let mut heads = Vec::new();
        for entry in rows(d,"knowledge_entries") {
            let v = versions.get(text(entry,"currentVersionId")).ok_or("Missing knowledge version")?;
            if !matches!(text(v,"kind"),"transcript"|"visual_context"|"ocr") || text(&v["scope"],"account") != scope || !in_account(v,scope) { continue; }
            let imported_scope=company_import::post_keys(d,v);
            if v.get("companyImport").is_some() && !rows(&v["companyImport"]["scope"],"postAliases").is_empty() && imported_scope.is_empty() { continue; }
            let facts = MediaFacts::new(v,scope);
            let source_facts = if text(v,"postKey").is_empty() { vec![] } else { media.posts.get(text(v,"postKey")).cloned().unwrap_or_default() };
            let mut source_identities = facts.identities.clone();
            for source in &source_facts { source_identities.extend(source.identities.iter().cloned()); }
            heads.push(TranscriptHead { value:(*v).clone(), scope:imported_scope, source_identities });
        }
        Ok(Self { account:scope.to_owned(), at:timestamp(at)?, media, heads,
            audio_equivalences:crate::media_audio_equivalence::bindings(d,at)?,
            exact_analyses:crate::media_analysis_reuse::bindings(d,at)
                .map_err(|_|"Media analysis applicability invalid")? })
    }
    pub(crate) fn has(&self, post: &Value) -> Result<bool, &'static str> {self.audio(post,false)}
    fn audio(&self, post: &Value, require_complete:bool) -> Result<bool, &'static str> {
        self.audio_with_proof(post,require_complete,false)
    }
    fn audio_with_proof(&self, post:&Value,require_complete:bool,require_proven_full:bool)->Result<bool,&'static str>{
        let source_version=require_proven_full.then(||super::media_fullframes::source_version(post,&self.account));
        if !in_account(post,&self.account){return Ok(false);}
        let mut covered=false;
        for head in &self.heads {
            if head.value["kind"]!="transcript" || !transcript_matches(&head.value,post,&self.account,&self.media,&head.scope){continue;}
            let v=&head.value;
            let from=v["validFrom"].as_str().map(timestamp).transpose()?;
            let until=v["validUntil"].as_str().map(timestamp).transpose()?;
            if v["status"]=="active" && !text(v,"text").trim().is_empty()
                && (!require_complete||v["transcription"]["partial"]!=true)
                && (!require_proven_full||(self.media.proven_full_audio_source(v,&self.account)
                    &&self.media.full_audio_target_binding(v,post,&self.account,&head.scope)))
                && !from.is_some_and(|t|t>self.at) && !until.is_some_and(|t|t<=self.at)
                && (v["trust"]=="verified" || (v["trust"]=="source_only" && !head.scope.is_empty())) { covered=true; }
        }
        Ok(covered || (require_proven_full&&self.audio_equivalences.iter().any(|edge|
            edge["targetPostId"]==post["id"] && edge["targetSourceVersion"].as_str()==source_version.as_deref()))
            || self.exact_analyses.iter().any(|binding|binding["edge"]["targetPostId"]==post["id"]
                && binding["edge"]["target"]["sourceVersion"]==super::media_fullframes::source_version(post,&self.account)))
    }
    pub(crate) fn has_visual(&self,post:&Value)->Result<bool,&'static str>{
        for head in &self.heads {
            let v=&head.value;
            if v["kind"]!="visual_context" || (v.get("companyImport").is_some()&&!head.scope.contains(text(post,"postKey"))) || !visual_matches(v,post,&self.account,&self.media)
                || super::media_fullframes::validate_evidence(&v["visualEvidence"]).is_err() {continue;}
            let from=v["validFrom"].as_str().map(timestamp).transpose()?;
            let until=v["validUntil"].as_str().map(timestamp).transpose()?;
            if v["status"]=="active" && !text(v,"text").trim().is_empty()
                && !from.is_some_and(|t|t>self.at) && !until.is_some_and(|t|t<=self.at)
                && v["trust"]=="source_only" && !head.scope.is_empty() {return Ok(true);}
        }
        Ok(false)
    }
    pub(crate) fn ready(&self,post:&Value)->Result<bool,&'static str>{Ok(self.audio(post,true)?&&self.has_visual(post)?)}
    /// Strict send floor: complete, admitted current-source audio. OCR is an
    /// honest optional outcome, never an invented observation or whole-video
    /// screen coverage claim. Pure audio uses this same proof relation.
    pub(crate) fn strict_media_evidence(&self,post:&Value)->Result<Value,&'static str>{
        let mut audio_ready=false;let mut audio_content=false;let mut screen_ready=false;let mut screen_content=false;
        let mut ocr_sources=Vec::new();
        let source_version=super::media_fullframes::source_version(post,&self.account);
        let mut ids=BTreeSet::new();
        if !in_account(post,&self.account){return Ok(json!({"audioReady":false,"audioHasContent":false,"screenTextReady":false,"screenTextHasContent":false,"visualReady":false,"headIds":[]}));}
        for head in &self.heads {
            let v=&head.value;
            if v["kind"]!="transcript"||!transcript_matches(v,post,&self.account,&self.media,&head.scope)
                ||!self.media.proven_full_audio_source(v,&self.account)
                ||!self.media.full_audio_target_binding(v,post,&self.account,&head.scope){continue;}
            // A title-only reuse policy is not evidence of this exact asset.
            let same_source=v["postKey"]==post["postKey"]
                ||!MediaFacts::new(v,&self.account).identities.is_disjoint(&MediaFacts::new(post,&self.account).identities);
            if !same_source{continue;}
            let from=v["validFrom"].as_str().map(timestamp).transpose()?;
            let until=v["validUntil"].as_str().map(timestamp).transpose()?;
            if v["status"]!="active"||text(v,"text").trim().is_empty()
                ||from.is_some_and(|t|t>self.at)||until.is_some_and(|t|t<=self.at)
                ||!(v["trust"]=="verified"||v["trust"]=="source_only"&&!head.scope.is_empty()){continue;}
            audio_ready=true;ids.insert(safe_head_id(text(v,"id")));
            let tr=&v["transcription"];
            audio_content|=tr["coverage"]=="full_audio"&&tr["audioStatus"]!="inspected_no_speech";
            let ocr=&tr["ocr"];
            if ocr["sourceVersion"]==source_version&&ocr["coverage"]=="sampled_frames"
                &&ocr["sampledFrames"].as_u64().is_some_and(|n|n>0&&n<=30)
                &&ocr["failedFrames"]==0&&matches!(text(ocr,"status"),"completed"|"no_text_found"){
                screen_ready=true;
                if !text(v,"mediaSha256").is_empty(){ocr_sources.push((v["mediaSha256"].clone(),ocr.clone()));}
            }
        }
        // A status label alone does not prove extracted text. Require the
        // current catalog's separately admitted, nonempty OCR material.
        for head in &self.heads {
            let v=&head.value;let ocr=&v["ocr"];
            if v["kind"]!="ocr"||v["postKey"]!=post["postKey"]||ocr["sourceVersion"]!=source_version
                ||!ocr_sources.iter().any(|(sha,receipt)|v["mediaSha256"]==*sha&&ocr==receipt)
                ||ocr["status"]!="completed"||ocr["coverage"]!="sampled_frames"||ocr["failedFrames"]!=0
                ||ocr["sampledFrames"].as_u64().is_none_or(|n|n==0||n>30)
                ||v["status"]!="active"||text(v,"text").trim().is_empty()
                ||!(v["trust"]=="verified"||v["trust"]=="source_only"&&!head.scope.is_empty()) {continue;}
            let from=v["validFrom"].as_str().map(timestamp).transpose()?;
            let until=v["validUntil"].as_str().map(timestamp).transpose()?;
            if !from.is_some_and(|t|t>self.at)&&!until.is_some_and(|t|t<=self.at){screen_content=true;}
        }
        // Reuse the existing explicitly admitted directional audio-equivalence
        // relation, pinned to this target source and an active complete donor.
        // Title similarity by itself grants no readiness.
        for edge in &self.audio_equivalences {
            if edge["targetPostId"]!=post["id"]||edge["targetSourceVersion"]!=source_version{continue;}
            let Some(head)=self.heads.iter().find(|h|h.value["id"]==edge["transcript"]["versionId"]) else{continue;};
            let v=&head.value;let tr=&v["transcription"];
            if v["kind"]!="transcript"||v["status"]!="active"||text(v,"text").trim().is_empty()
                ||!full_audio_coverage(tr)||!self.media.proven_full_audio_source(v,&self.account){continue;}
            let from=v["validFrom"].as_str().map(timestamp).transpose()?;
            let until=v["validUntil"].as_str().map(timestamp).transpose()?;
            if from.is_some_and(|t|t>self.at)||until.is_some_and(|t|t<=self.at){continue;}
            audio_ready=true;audio_content|=tr["coverage"]=="full_audio"&&tr["audioStatus"]!="inspected_no_speech";
            ids.insert(safe_head_id(text(v,"id")));
        }
        for binding in &self.exact_analyses {
            let edge=&binding["edge"];
            if edge["targetPostId"]!=post["id"]||edge["target"]["sourceVersion"]!=source_version {continue;}
            audio_ready=true;
            audio_content|=binding["material"]["transcription"]["coverage"]=="full_audio"
                &&binding["material"]["transcription"]["audioStatus"]!="inspected_no_speech";
            ids.insert(safe_head_id(text(&edge["transcript"],"versionId")));
            // A directly observed target OCR outcome is independently pinned
            // inside this applicability proof. no_text_found is useful sampled
            // coverage even when there is no nonempty OCR material to import.
            if !binding["currentScreenText"].is_null() {screen_ready=true;}
            // Original OCR/sourceVersion remains donor evidence. This exact
            // audio edge does not set target screen readiness or viewed pixels.
            for head in &self.heads {
                let v=&head.value;let ocr=&v["ocr"];
                if v["kind"]!="ocr"||v["postKey"]!=post["postKey"]||v["mediaSha256"]!=edge["verifiedFile"]["sha256"]
                    ||ocr["sourceVersion"]!=source_version||ocr["coverage"]!="sampled_frames"||ocr["exhaustive"]!=false
                    ||ocr["failedFrames"]!=0||ocr["sampledFrames"].as_u64().is_none_or(|n|n==0||n>30)
                    ||!matches!(text(ocr,"status"),"completed"|"no_text_found")||v["status"]!="active"
                    ||!(v["trust"]=="verified"||v["trust"]=="source_only"&&!head.scope.is_empty()) {continue;}
                let from=v["validFrom"].as_str().map(timestamp).transpose()?;
                let until=v["validUntil"].as_str().map(timestamp).transpose()?;
                if from.is_some_and(|t|t>self.at)||until.is_some_and(|t|t<=self.at) {continue;}
                screen_ready=true;
                screen_content|=ocr["status"]=="completed"&&!text(v,"text").trim().is_empty();
            }
        }
        Ok(json!({"audioReady":audio_ready,"audioHasContent":audio_content,"screenTextReady":screen_ready,
            "screenTextHasContent":screen_content,"visualReady":self.has_visual(post)?,"headIds":ids.into_iter().collect::<Vec<_>>()}))
    }
    fn readiness_prerequisites(&self,post:&Value,visual_required:bool)->Result<(bool,bool,bool,bool),&'static str>{
        let bound=in_account(post,&self.account);let required=is_video_post(post);
        if !required&&bound{return Ok((bound,required,true,true));}
        Ok((bound,required,bound&&self.audio_with_proof(post,true,!visual_required)?,
            bound&&(!visual_required||self.has_visual(post)?)))
    }
    pub(crate) fn ready_for_policy(&self,post:&Value,visual_required:bool)->Result<bool,&'static str>{
        let (_,_,audio_ready,visual_ready)=self.readiness_prerequisites(post,visual_required)?;
        Ok(audio_ready&&visual_ready)
    }
    /// Current-head prerequisite readback; no artifact I/O or catalog promotion.
    pub(crate) fn readiness_for_policy(&self,post:&Value,visual_required:bool)->Result<Value,&'static str>{
        let (bound,required,audio_ready,visual_ready)=self.readiness_prerequisites(post,visual_required)?;
        let source_version=super::media_fullframes::source_version(post,&self.account);
        if !required&&bound{return Ok(json!({"audioRequired":false,"visualRequired":false,"audioReady":true,"visualReady":true,"ready":true,"sourceVersion":source_version,"reasons":[],"currentHeadIds":{"audio":[],"visual":[]},"currentHeadCounts":{"audio":0,"visual":0},"headIdsTruncated":false}));}
        let target=MediaFacts::new(post,&self.account);let key=text(post,"postKey");
        let mut reasons=BTreeSet::new();let mut audio_ids=BTreeSet::new();let mut visual_ids=BTreeSet::new();
        let mut audio_seen=false;let mut visual_seen=false;
        for head in &self.heads {
            let v=&head.value;let kind=text(v,"kind");
            let direct=head.scope.is_empty()||head.scope.contains(key)||text(v,"postKey")==key;
            let shared=v.get("companyImport").is_none()&&!head.source_identities.is_disjoint(&target.identities);
            if !direct&&!shared{continue;}
            if kind=="transcript"{audio_seen=true;}else if kind=="visual_context"{visual_seen=true;}else{continue;}
            let from=v["validFrom"].as_str().map(timestamp).transpose()?;
            let until=v["validUntil"].as_str().map(timestamp).transpose()?;
            let active=v["status"]=="active"&&!text(v,"text").trim().is_empty()
                &&!from.is_some_and(|t|t>self.at)&&!until.is_some_and(|t|t<=self.at);
            if kind=="transcript" {
                if !bound||!transcript_matches(v,post,&self.account,&self.media,&head.scope){if !audio_ready{reasons.insert("audio_source_mismatch");}continue;}
                if !active||!(v["trust"]=="verified"||(v["trust"]=="source_only"&&!head.scope.is_empty())){if !audio_ready{reasons.insert("head_not_admitted");}continue;}
                if v["transcription"]["partial"]==true||(!visual_required
                    &&!(self.media.proven_full_audio_source(v,&self.account)
                        &&self.media.full_audio_target_binding(v,post,&self.account,&head.scope))){if !audio_ready{reasons.insert("audio_incomplete");}continue;}
                audio_ids.insert(safe_head_id(text(v,"id")));
                if !self.media.proven_full_audio_source(v,&self.account){reasons.insert("legacy_unknown_coverage");}
            }else if visual_required {
                if !bound||!visual_matches(v,post,&self.account,&self.media)||(v.get("companyImport").is_some()&&!head.scope.contains(key)){if !visual_ready{reasons.insert("visual_source_mismatch");}continue;}
                if !active||v["trust"]!="source_only"||head.scope.is_empty(){if !visual_ready{reasons.insert("head_not_admitted");}continue;}
                if super::media_fullframes::validate_evidence(&v["visualEvidence"]).is_err(){if !visual_ready{reasons.insert("visual_proof_unavailable");}continue;}
                visual_ids.insert(safe_head_id(text(v,"id")));
            }
        }
        if !bound{reasons.clear();reasons.insert("binding_conflict");}
        else {
            if !audio_ready&&!audio_seen{reasons.insert("audio_head_missing");}
            if visual_required&&!visual_ready&&!visual_seen{reasons.insert("visual_head_missing");}
        }
        if !visual_required&&audio_ready {
            for edge in &self.audio_equivalences{if edge["targetPostId"]==post["id"]&&edge["targetSourceVersion"]==source_version {
                if let Some(id)=edge["transcript"]["versionId"].as_str(){audio_ids.insert(safe_head_id(id));}
            }}
        }
        let audio_count=audio_ids.len();let visual_count=visual_ids.len();
        Ok(json!({"audioRequired":required,"visualRequired":required&&visual_required,"audioReady":audio_ready,"visualReady":visual_ready,"ready":audio_ready&&visual_ready,
            "sourceVersion":source_version,"reasons":reasons.into_iter().collect::<Vec<_>>(),"currentHeadCounts":{"audio":audio_count,"visual":visual_count},"headIdsTruncated":audio_count>20||visual_count>20,
            "currentHeadIds":{"audio":audio_ids.into_iter().take(20).collect::<Vec<_>>(),"visual":visual_ids.into_iter().take(20).collect::<Vec<_>>()}}))
    }

}

/// Visual reuse requires observed matching bytes or the same source binding.
/// Titles grant neither identity nor evidence; observed conflicts still win.
fn visual_matches(v:&Value,post:&Value,scope:&str,media:&MediaLookup)->bool {
    if !in_account(v,scope)||!in_account(post,scope)||media_conflict(v,post,scope){return false;}
    if !media.associated_visual_heads.contains(text(v,"id")){return false;}
    let evidence=&v["visualEvidence"]["source"];
    if evidence["account"]!=scope || evidence["postKey"]!=v["postKey"] || evidence["mediaSha256"]!=v["mediaSha256"] {return false;}
    let observed_duration=post["durationMs"].as_u64().or_else(||post["durationSeconds"].as_f64().filter(|v|v.is_finite()&&*v>0.0).map(|v|(v*1000.0).round() as u64));
    if observed_duration.zip(evidence["durationMs"].as_u64()).is_some_and(|(a,b)|a.abs_diff(b)>1000){return false;}
    let a=media_identities(v,scope);let mut target=MediaFacts::new(post,scope);
    if let Some(known)=media.posts.get(text(post,"postKey")){for facts in known{target.identities.extend(facts.identities.iter().cloned());}}
    if identity_conflict(&a,&target.identities){return false;}
    if a.intersection(&target.identities).any(|id|id.starts_with("sha:")){return true;}
    if !text(v,"postKey").is_empty() && v["postKey"]==post["postKey"]
        && media_source_key(v,scope)==media_source_key(post,scope) {return true;}
    false
}

/// Reverify only current visual heads matching this recipient's evidence posts.
/// This is local artifact I/O outside the database writer. It grants no source
/// authority: the unchanged proposal/readiness checks still run afterwards.
fn dispatch_visual_proofs(d:&Value,item_id:&str)->crate::ApiResult<Vec<Value>> {
        // Older non-video proposals need neither branch evidence nor a catalog.
        // Warming must not invent prerequisites for their existing local gate.
        if !rows(d,"knowledge_entries").iter().any(|entry|rows(d,"knowledge_versions").iter()
            .any(|v|v["id"]==entry["currentVersionId"]&&v["kind"]=="visual_context")) {return Ok(vec![]);}
        let binding=crate::active_binding(d)?;
        let item=crate::bound_item(&binding,crate::row(d,"items",item_id)?)?;
        let branch=item["branchId"].as_str().and_then(|id|rows(d,"branches").iter().find(|b|b["id"]==id));
        let targets:Vec<_>=rows(d,"posts").iter().filter(|p|p["id"].is_string()
            &&(p["id"]==item["postId"]||branch.is_some_and(|b|p["id"]==b["postId"]))).collect();
        if targets.is_empty(){return Ok(vec![]);}
        let catalog=Catalog::new(d).map_err(crate::conflict)?;
        let scope=supported_account(d).map_err(crate::conflict)?;
        let media=MediaLookup::new(d,scope);let at=Utc::now();
        let mut proofs=BTreeMap::new();
        for entry in rows(d,"knowledge_entries") {
            let v=catalog.version(text(entry,"currentVersionId")).ok_or_else(||crate::conflict("Missing knowledge version"))?;
            if v["kind"]!="visual_context"||v["status"]!="active"||v["trust"]!="source_only"
                ||text(v,"text").trim().is_empty()||text(&v["scope"],"account")!=scope||!in_account(v,scope)
                ||company_import::post_keys(d,v).is_empty(){continue;}
            let from=v["validFrom"].as_str().map(timestamp).transpose().map_err(crate::conflict)?;
            let until=v["validUntil"].as_str().map(timestamp).transpose().map_err(crate::conflict)?;
            if from.is_some_and(|t|t>at)||until.is_some_and(|t|t<=at){continue;}
            let allowed=company_import::post_keys(d,v);
            if targets.iter().any(|post|(v.get("companyImport").is_none()||allowed.contains(text(post,"postKey")))&&visual_matches(v,post,scope,&media)) {
                proofs.insert(crate::media_fullframes::hash(&v["visualEvidence"]),v["visualEvidence"].clone());
            }
        }
        Ok(proofs.into_values().collect())
}

#[cfg(test)]
#[path="knowledge_media_binding_tests.rs"]
mod media_binding_tests;
pub(crate) async fn refresh_dispatch_visual_proofs(d:&Value,item_id:&str)->crate::ApiResult<()> {
    let target=rows(d,"items").iter().find(|item|item["id"]==item_id).map(|item|item["postId"].clone());
    let pins=rows(d,"jobs").iter().filter(|job|job["kind"]=="media_analysis_applicability"&&job["status"]=="completed"
        &&job["account"]==d["account"]&&target.as_ref()==Some(&job["result"]["proof"]["target"]["postId"]))
        .map(|job|job["result"]["proof"].clone()).collect::<Vec<_>>();
    if !pins.is_empty() {
        // Narrow immutable CAS warming; the dispatch predicate below still
        // compares current source/head/result against its scoped snapshot.
        tokio::task::spawn_blocking(move||for pin in pins {let _=crate::media_analysis_reuse::warm_immutable_pin(&pin);})
            .await.map_err(|_|crate::internal("Dispatch analysis proof verification stopped"))?;
    }
    // Selection cannot grant authority. Let local_check retain its established
    // missing-record, revision, source and catalog failure classifications.
    let Ok(proofs)=dispatch_visual_proofs(d,item_id) else{return Ok(());};
    if proofs.is_empty(){return Ok(());}
    let _timing=crate::performance::Span::new("dispatch.visual_proof.reverify");
    tokio::task::spawn_blocking(move||for proof in proofs {
        // Failed verification invalidates cached proof; normal source/readiness
        // validation decides whether another admitted head still suffices.
        let _=crate::media_fullframes::verify_and_cache(&proof);
    }).await.map_err(|_|crate::internal("Dispatch media proof verification stopped"))?;
    Ok(())
}

fn transcript_group(d: &Value, material: &Value, provenance: &Value) -> String {
    let key = text(material,"postKey");
    let locators=|record:&Value|media_identities(record,account(d)).into_iter()
        .filter(|id|!id.starts_with("sha:")&&!id.starts_with("canonical:")).collect::<BTreeSet<_>>();
    let original=rows(d,"posts").iter().find(|p|!key.is_empty()&&text(p,"postKey")==key);
    let original_locators=original.map(&locators).unwrap_or_default();
    // A selected post may contain several distinct videos. Using that bridge
    // as the representative for every bound transcript would silently drop
    // one video's speech, even when the only available identities are URLs.
    // Keep exact single-video locators separate at this boundary; this is a
    // ranking key, never a new media-equivalence assertion.
    let multivideo_boundary=rows(d,"posts").iter().any(|post| {
        let identities=locators(post);
        identities.len()>1&&(!identities.is_disjoint(&original_locators)
            ||rows(provenance,"mediaBinding").iter().any(|binding|binding["postKey"]==post["postKey"]))
    });
    if multivideo_boundary {
        let mut identities=locators(material);
        if identities.is_empty(){identities=original_locators;}
        let peers:Vec<_>=rows(d,"posts").iter().filter(|post|!locators(post).is_disjoint(&identities)).collect();
        if peers.iter().enumerate().any(|(i,post)|peers.iter().skip(i+1).any(|other|media_conflict(post,other,account(d)))) {
            return format!("post:{key}");
        }
        return if identities.len()==1 {format!("video:{}",identities.iter().next().unwrap())}else{format!("post:{key}")};
    }
    let source = rows(d,"posts").iter().find(|p| rows(provenance,"mediaBinding").iter().any(|binding|binding["postKey"]==p["postKey"]))
        .or_else(|| rows(d,"posts").iter().find(|p| !key.is_empty() && text(p,"postKey") == key));
    if let Some(post)=source {
        if let Some(group)=media_group_key(post,account(d)) {
            let peers:Vec<_>=rows(d,"posts").iter().filter(|p|media_group_key(p,account(d)).as_ref()==Some(&group)).collect();
            let title_conflict=peers.iter().enumerate().any(|(i,p)|peers.iter().skip(i+1).any(|q|media_conflict(p,q,account(d))));
            let identities=media_identities(post,account(d));
            let identity_peers:Vec<_>=rows(d,"posts").iter().filter(|p|in_account(p,account(d))
                && !identities.is_disjoint(&media_identities(p,account(d)))).collect();
            // An unknown-hash sibling must not bridge two explicitly conflicting
            // copies through the same provider locator and collapse their speech.
            if title_conflict||identity_peers.iter().enumerate().any(|(i,p)|identity_peers.iter().skip(i+1).any(|q|media_conflict(p,q,account(d)))) {
                return format!("post:{key}");
            }
            let representative=rows(d,"posts").iter().filter(|p|in_account(p,account(d))&&(
                !media_conflict(post,p,account(d))&&!identities.is_disjoint(&media_identities(p,account(d)))
            )).map(|p|text(p,"postKey")).filter(|k|!k.is_empty()).min();
            if let Some(representative)=representative {return format!("post:{representative}");}
        }
    }
    format!("post:{key}")
}
fn transcript_rank(d: &Value, material: &Value) -> (bool, u64, i64) {
    let raw=rows(d,"materials").iter().find(|m|m["id"]==material["id"]);
    let updated=raw.and_then(|m|m["updatedAt"].as_str().or(m["sourceDate"].as_str()).or(m["createdAt"].as_str()))
        .and_then(|s|DateTime::parse_from_rfc3339(s).ok()).map(|t|t.timestamp_millis()).unwrap_or(0);
    (material["transcription"]["partial"]==false,material["transcription"]["maxAudioSeconds"].as_u64().unwrap_or(0),updated)
}

#[cfg(test)]
fn shared_media_binding(d: &Value, material: &Value, targets: &[&Value]) -> Vec<Value> {
    if !matches!(text(material, "kind"), "transcript" | "ocr" | "visual_context") || !in_account(material, account(d)) { return vec![]; }
    let mut identities = media_identities(material, account(d));
    let source_key = text(material, "postKey");
    for source in rows(d, "posts").iter().filter(|p| !source_key.is_empty() && text(p, "postKey") == source_key) {
        identities.extend(media_identities(source, account(d)));
    }
    let mut bindings = Vec::new();
    for target in targets {
        let matches: Vec<_> = identities.intersection(&media_identities(target, account(d))).cloned().collect();
        if !matches.is_empty() {
            bindings.push(json!({"postKey":target["postKey"],"sourcePostKey":source_key,"identities":matches}));
        }
    }
    bindings.sort_by_key(Value::to_string);
    bindings.dedup();
    bindings
}
fn version_hash(v: &Value) -> String {
    let mut payload = v.clone();
    if let Some(o) = payload.as_object_mut() {
        for k in ["id", "hash", "createdAt"] {
            o.remove(k);
        }
    }
    hash(&payload)
}
fn timestamp(s: &str) -> Result<DateTime<Utc>, &'static str> {
    DateTime::parse_from_rfc3339(s)
        .map(|x| x.with_timezone(&Utc))
        .map_err(|_| "Invalid knowledge timestamp")
}
fn validate(d: &Value) -> Result<(), &'static str> {
    validate_indexed(d).map(|_|())
}
fn validate_indexed(d: &Value) -> Result<BTreeMap<&str,&Value>, &'static str> {
    #[cfg(test)]
    VALIDATIONS.with(|count|count.set(count.get()+1));
    let mut versions = BTreeMap::new();
    for v in rows(d, "knowledge_versions") {
        let id = text(v, "id");
        if id.is_empty() || versions.insert(id, v).is_some() {
            return Err("Duplicate or missing knowledge version ID");
        }
        if text(v, "hash") != version_hash(v) {
            return Err("Knowledge version integrity mismatch");
        }
        if let Some(availability)=v.get("sourceAvailability") {validate_source_availability(availability,text(v,"sourceUrl"),None)?;}
        if text(&v["scope"], "account").is_empty()
            || !v["scope"]["postKeys"].is_array()
            || rows(&v["scope"], "postKeys")
                .iter()
                .any(|key| key.as_str().is_none_or(str::is_empty))
        {
            return Err("Invalid knowledge scope");
        }
    }
    let mut ids = BTreeSet::new();
    let mut sources = BTreeSet::new();
    for e in rows(d, "knowledge_entries") {
        if text(e, "id").is_empty()
            || !ids.insert(text(e, "id"))
            || !sources.insert(text(e, "sourceMaterialId"))
        {
            return Err("Conflicting knowledge heads");
        }
        let v = versions
            .get(text(e, "currentVersionId"))
            .ok_or("Missing knowledge head version")?;
        if v["entryId"] != e["id"]
            || v["sourceMaterialId"] != e["sourceMaterialId"]
            || v["scope"] != e["scope"]
            || v["status"] != e["status"]
            || v["kind"] != e["kind"]
        {
            return Err("Knowledge head metadata mismatch");
        }
    }
    Ok(versions)
}
pub(crate) fn validate_catalog(d:&Value)->Result<(),&'static str>{validate(d)}

/// An integrity-checked catalog tied to one immutable workspace borrow. It has
/// no cross-transaction lifetime and cannot survive mutation of its source.
pub(crate) struct Catalog<'a> {
    workspace:&'a Value,
    versions:BTreeMap<&'a str,&'a Value>,
    media:OnceCell<MediaLookup>,
}
impl<'a> Catalog<'a> {
    pub(crate) fn new(workspace:&'a Value)->Result<Self,&'static str>{
        Ok(Self{workspace,versions:validate_indexed(workspace)?,media:OnceCell::new()})
    }
    pub(crate) fn workspace(&self)->&'a Value{self.workspace}
    pub(crate) fn version(&self,id:&str)->Option<&'a Value>{self.versions.get(id).copied()}
    pub(crate) fn select(&self,items:&[Value],posts:&[Value],at:&str)->Result<Value,&'static str>{
        select_catalog(self,items,posts,at,false)
    }
    pub(crate) fn select_for_preparation(&self,items:&[Value],posts:&[Value],at:&str)->Result<Value,&'static str>{
        select_catalog(self,items,posts,at,true)
    }
}

pub fn sync_catalog(d: &mut Value, at: &str) -> Result<(), &'static str> {
    let manifest: Value = serde_json::from_str(include_str!("knowledge-import-manifest.json"))
        .map_err(|_| "Invalid knowledge import manifest")?;
    sync_with_manifest(d, at, &manifest)
}
pub(crate) fn protects_reply_url_material(d:&Value,source:&str)->bool {
    rows(d,"knowledge_entries").iter().filter(|entry|entry["sourceMaterialId"]==source)
        .filter_map(|entry|rows(d,"knowledge_versions").iter().find(|v|v["id"]==entry["currentVersionId"]))
        .any(|version|version["category"]=="reply_url_policy")
}
fn sync_with_manifest(d: &mut Value, at: &str, manifest: &Value) -> Result<(), &'static str> {
    timestamp(at)?;
    validate(d)?;
    let active_account = supported_account(d)?;
    let mut entries = rows(d, "knowledge_entries").to_vec();
    let mut versions = rows(d, "knowledge_versions").to_vec();
    let mut source_ids = BTreeSet::new();
    for m in rows(d, "materials") {
        // Legacy/manual records may be unlabelled inside an already bound
        // account store. An explicitly foreign label is never imported.
        if !in_account(m, active_account) { continue; }
        let source = text(m, "id");
        if source.is_empty() || !source_ids.insert(source) {
            return Err("Duplicate or missing material ID");
        }
        let eid = format!("knowledge-{}", sha(source.as_bytes()));
        let old = entries.iter().find(|e| text(e, "id") == eid);
        if old.and_then(|entry|versions.iter().find(|v|v["id"]==entry["currentVersionId"]))
            .is_some_and(|version|version["category"]=="reply_url_policy") {
            if old.and_then(|entry|versions.iter().find(|v|v["id"]==entry["currentVersionId"]))
                .is_some_and(|version|version["sourceHash"]!=hash(&content(m))) {
                return Err("Reply URL policy requires a versioned edit");
            }
            continue;
        }
        if old.is_some_and(rule_normalization::owns_entry) {
            let current=old.and_then(|e|versions.iter().find(|v|v["id"]==e["currentVersionId"])).ok_or("Missing normalized knowledge head")?;
            if current["sourceHash"]!=hash(&content(m)) {return Err("Normalized rules require a reviewed versioned change");}
            continue;
        }
        // Once admitted into CommunityHero, legacy refresh is no longer its owner.
        let managed=old.is_some_and(|entry|!rows(entry,"companyImportReceipts").is_empty());
        if managed && m["locallyEdited"]!=true {continue;}
        let previous = old.and_then(|e| versions.iter().find(|v| v["id"] == e["currentVersionId"]));
        let source_hash = hash(&content(m));
        if previous.is_some_and(|v| text(v, "sourceHash") == source_hash) {
            continue;
        }
        if managed {
            let previous=previous.ok_or("Missing managed knowledge head")?;
            let mut v=previous.clone();
            for key in ["title","text","sourceUrl","sourceDate"] {v[key]=m[key].clone();}
            v.as_object_mut().unwrap().remove("sourceAvailability");
            if let Some(availability)=m.get("sourceAvailability") {validate_source_availability(availability,text(m,"sourceUrl"),Some(at))?;v["sourceAvailability"]=availability.clone();}
            v["sourceHash"]=json!(source_hash);v["sourceRevision"]=m["revision"].clone();
            v["status"]=json!("pending_review");v["changedBy"]=json!("operator");
            v["supersedes"]=previous["id"].clone();v["createdAt"]=json!(at);
            let digest=version_hash(&v);v["hash"]=json!(digest);v["id"]=json!(format!("knowledge-version-{digest}"));
            let index=entries.iter().position(|e|e["id"]==eid).unwrap();
            entries[index]["currentVersionId"]=v["id"].clone();entries[index]["status"]=v["status"].clone();
            versions.push(v);continue;
        }
        // The bundled manifest is an account-bound allowlist. A source ID from
        // another account must never inherit its kind, trust, policy or scope.
        let candidate = &manifest["entries"][source];
        let mapped = (candidate.is_object()
            && text(&candidate["scope"], "account") == active_account).then_some(candidate);
        let media = matches!(text(m, "kind"), "transcript" | "ocr" | "visual_context");
        let kind = if let Some(mapped) = mapped {
            text(mapped, "kind")
        } else if media {
            text(m, "kind")
        } else {
            "reference"
        };
        let post_key = text(m, "postKey");
        let scope = json!({"account":active_account,"postKeys":if post_key.is_empty(){vec![]}else{vec![post_key]}});
        let bound = mapped.is_some_and(|mapped| text(mapped, "contentHash") == sha(text(m, "text").as_bytes())
            && mapped["scope"] == scope);
        let evidence = matches!(kind, "transcript" | "ocr" | "visual_context") && !post_key.is_empty()
            && (kind!="visual_context" || super::media_fullframes::validate_evidence(&m["visualEvidence"]).is_ok());
        let initial_policy = previous.is_none()
            && bound
            && matches!(kind, "rule" | "policy")
            && mapped.is_some_and(|mapped| mapped["trust"] == "imported_policy"
                && mapped["status"] == "active");
        let (trust, status) = if evidence {
            ("source_only", "active")
        } else if initial_policy {
            ("imported_policy", "active")
        } else {
            ("unverified", "pending_review")
        };
        let mut v = json!({"entryId":eid,"sourceMaterialId":source,"sourceRevision":m["revision"],"sourceHash":source_hash,
            "title":text(m,"title"),"text":text(m,"text"),"sourceUrl":text(m,"sourceUrl"),"postKey":post_key,
            "kind":kind,"category":mapped.map_or(Value::Null,|value|value["category"].clone()),"scope":scope,"trust":trust,"status":status,
            "validFrom":at,"validUntil":mapped.map_or(Value::Null,|value|value["validUntil"].clone()),"sourceDate":m["sourceDate"],
            "supersedes":previous.map(|v|v["id"].clone()),"createdAt":at});
        copy_media_identity(m, &mut v);
        if let Some(availability)=m.get("sourceAvailability") {validate_source_availability(availability,text(m,"sourceUrl"),Some(at))?;v["sourceAvailability"]=availability.clone();}
        let digest = version_hash(&v);
        v["hash"] = json!(digest);
        v["id"] = json!(format!("knowledge-version-{digest}"));
        let e = json!({"id":eid,"sourceMaterialId":source,"currentVersionId":v["id"],"kind":kind,"scope":scope,"status":status});
        if let Some(index) = entries.iter().position(|e| text(e, "id") == eid) {
            entries[index] = e;
        } else {
            entries.push(e);
        }
        versions.push(v);
    }
    d["knowledge_entries"] = json!(entries);
    d["knowledge_versions"] = json!(versions);
    Ok(())
}

pub fn select(
    d: &Value,
    items: &[Value],
    posts: &[Value],
    at: &str,
) -> Result<Value, &'static str> {
    Catalog::new(d)?.select(items,posts,at)
}
fn select_catalog(catalog:&Catalog<'_>,items:&[Value],posts:&[Value],at:&str,preparation:bool)->Result<Value,&'static str>{
    let d=catalog.workspace();
    supported_account(d)?;
    let now = timestamp(at)?;
    let keys: BTreeSet<&str> = items
        .iter()
        .chain(posts)
        .filter_map(|v| v["postKey"].as_str())
        .filter(|s| !s.is_empty())
        .collect();
    let targets: Vec<&Value> = rows(d, "posts").iter().chain(posts.iter().filter(|candidate| !rows(d, "posts").iter().any(|p| p["postKey"] == candidate["postKey"])))
        .filter(|p| keys.contains(text(p, "postKey")) && in_account(p, account(d)))
        .collect();
    let mut audio_only_sources:BTreeMap<String,Value>=BTreeMap::new();
    for post in &targets {
        if !is_video_post(post){continue;}
        // Assistant preparation must project the same owner-confirmed audio
        // equivalence that its full-audio admission gate accepted. Generic
        // catalog selection retains its historical acquisition policy.
        let policy=if preparation {
            crate::post_media_policy::effective_for_preparation(d,post)
                .map_err(|_|"Post preparation media policy unavailable")?
        }else{
            if d["settings"]["postMediaPolicies"].get(text(post,"id")).is_none()
                &&crate::post_media_policy::probed_duration(d,post).is_none(){continue;}
            crate::post_media_policy::effective(d,post).map_err(|_|"Post media policy unavailable")?
        };
        if policy["mode"]=="full_audio_only" {audio_only_sources.insert(text(post,"postKey").to_owned(),(**post).clone());}
    }
    let audio_equivalences=crate::media_audio_equivalence::bindings(d,at)?.into_iter()
        .filter(|e|audio_only_sources.contains_key(text(e,"postKey"))).collect::<Vec<_>>();
    let media_index=catalog.media.get_or_init(||MediaLookup::new(d,account(d)));
    let target_facts:Vec<_>=targets.iter().map(|post|(*post,MediaFacts::new(post,account(d)))).collect();
    let mut materials = Vec::new();
    let mut manifest = Vec::new();
    let mut excluded = Vec::new();
    for e in rows(d, "knowledge_entries") {
        let v = catalog.version(text(e,"currentVersionId"))
            .ok_or("Missing knowledge version")?;
        // Unrelated records are not part of either the evidence or its exclusion digest.
        if text(&v["scope"], "account") != account(d) {
            continue;
        }
        // Customer evidence has a separate exact author/platform selector.
        if v["kind"]=="customer_case" {continue;}
        if matches!(text(v, "kind"), "transcript" | "ocr" | "visual_context") && !in_account(v, account(d)) {
            continue;
        }
        let scoped = rows(&v["scope"], "postKeys");
        let legacy_scoped=!rows(&v["companyImport"]["scope"],"postAliases").is_empty();
        let imported_keys=company_import::post_keys(d,v);
        let direct = if legacy_scoped {imported_keys.iter().any(|key|keys.contains(key.as_str()))} else {scoped.is_empty() || scoped
                .iter()
                .any(|k| k.as_str().is_some_and(|k| keys.contains(k)))};
        if v["kind"]=="visual_context" && (!targets.iter().any(|post|visual_matches(v,post,account(d),media_index))
            || super::media_fullframes::validate_evidence(&v["visualEvidence"]).is_err()) {continue;}
        let exact_equivalences:Vec<_>=audio_equivalences.iter().filter(|e|e["transcript"]["versionId"]==v["id"]&&e["transcript"]["hash"]==v["hash"]).cloned().collect();
        if v["kind"]=="transcript" && !targets.is_empty()
            && !targets.iter().any(|post|transcript_matches(v,post,account(d),media_index,&imported_keys)
                ||exact_equivalences.iter().any(|edge|edge["targetPostId"]==post["id"])){continue;}
        let mut media_binding = if legacy_scoped { imported_keys.iter().filter(|key|keys.contains(key.as_str())).map(|key|json!({"postKey":key,"match":"legacy_connector_scoped_alias","namespace":"commentops-fast.post-key"})).collect() }
            else if direct || v.get("companyImport").is_some() { vec![] } else { media_index.shared_binding(account(d),v,&target_facts) };
        media_binding.extend(exact_equivalences.clone());
        if !direct && media_binding.is_empty() {
            continue;
        }
        // An older transcript with the same provider post key can outrank the
        // newly admitted full transcript. Exclude it before canonical ranking
        // so the model receives the exact current source, never stale speech.
        if v["kind"]=="transcript" && !audio_only_sources.is_empty() {
            let matched=audio_only_sources.iter().filter(|(key,_)|
                v["postKey"].as_str()==Some(key.as_str()) || scoped.iter().any(|scope|scope.as_str()==Some(key.as_str()))
                    ||media_binding.iter().any(|binding|binding["postKey"].as_str()==Some(key.as_str())))
                .collect::<Vec<_>>();
            let mut bound_keys=imported_keys.clone();
            bound_keys.extend(scoped.iter().filter_map(|scope|scope.as_str().map(str::to_owned)));
            let independent_source=targets.iter().any(|post|post["postKey"]==v["postKey"]);
            if matched.iter().any(|(key,target)|{
                let approved=exact_equivalences.iter().any(|e|e["postKey"].as_str()==Some(key.as_str()));
                !approved && (!media_index.proven_full_audio_source(v,account(d))
                    ||!media_index.full_audio_target_binding(v,target,account(d),&bound_keys)
                    ||audio_equivalences.iter().any(|e|e["postKey"].as_str()==Some(key.as_str())))
            })
                ||(matched.is_empty()&&!(independent_source&&media_index.proven_full_audio_source(v,account(d)))) {
                continue;
            }
        }
        let valid_from = v["validFrom"].as_str().map(timestamp).transpose()?;
        let valid_until = v["validUntil"].as_str().map(timestamp).transpose()?;
        let reason = if v["status"] != "active" {
            Some("pending_review")
        } else if v["kind"] == "transcript" && text(v,"text").trim().is_empty() {
            Some("empty_transcript")
        } else if valid_from.is_some_and(|t| t > now) {
            Some("not_yet_valid")
        } else if valid_until.is_some_and(|t| t <= now) {
            Some("expired")
        } else if matches!(text(v, "kind"), "rule" | "policy")
            && !matches!(text(v, "trust"), "imported_policy" | "verified")
        {
            Some("untrusted_policy")
        } else if !matches!(text(v, "kind"), "rule" | "policy")
            && v["trust"] != "verified"
            && !(matches!(text(v, "kind"), "transcript" | "ocr" | "visual_context")
                && v["trust"] == "source_only"
                && (!scoped.is_empty() || (legacy_scoped&&!imported_keys.is_empty())))
            && !(v.get("companyImport").is_some() && v["trust"]=="source_only"
                && v["kind"]=="reference" && v["category"]!="transport_capability_reference")
        {
            Some("unverified_fact")
        } else {
            None
        };
        if let Some(reason) = reason {
            excluded.push(json!({"entryId":e["id"],"reason":reason}));
            continue;
        }
        let mut material = json!({"id":v["sourceMaterialId"],"title":v["title"],"text":v["text"],"kind":v["kind"],"revision":v["sourceRevision"],"postKey":v["postKey"],"sourceUrl":v["sourceUrl"],"knowledgeEntryId":e["id"],"knowledgeVersionId":v["id"],"trust":v["trust"]});
        if v["category"]=="reply_url_policy" || v.get("replyUrlPolicy").is_some() {
            material["policyType"]=json!("reply_url_policy");
            material["replyUrlPolicy"]=v["replyUrlPolicy"].clone();
        }
        copy_media_identity(v, &mut material);
        if let Some(availability)=v.get("sourceAvailability") {material["sourceAvailability"]=availability.clone();}
        if !exact_equivalences.is_empty(){material["audioEquivalence"]=json!(exact_equivalences);}
        if v.get("companyImport").is_some() {
            material["sourceAssertion"]=json!(true);
            material["companyImport"]=v["companyImport"].clone();
            if legacy_scoped {
                material["legacyPostAliases"]=v["companyImport"]["scope"]["postAliases"].clone();
                // This is the matched existing workspace post, not a newly minted
                // native ID; the catalog continues storing only scoped aliases.
                if let Some(binding)=media_binding.first(){material["postKey"]=binding["postKey"].clone();}
            }
        }
        materials.push(material);
        let mut provenance = json!({"entryId":e["id"],"versionId":v["id"],"hash":v["hash"],"kind":v["kind"],"scope":v["scope"],"trust":v["trust"]});
        if let Some(availability)=v.get("sourceAvailability") {provenance["sourceAvailability"]=availability.clone();}
        if !media_binding.is_empty() { provenance["mediaBinding"] = json!(media_binding); }
        manifest.push(provenance);
    }
    materials.sort_by_key(|v| text(v, "id").to_owned());
    // Preserve every source/version in the catalog, but admit only one canonical
    // transcript per selected video group. Explicit full coverage outranks a
    // capped/unknown source; otherwise use documented coverage, date, stable ID.
    let mut canonical:BTreeMap<String,Value>=BTreeMap::new();
    for material in materials.iter().filter(|m|m["kind"]=="transcript") {
        let provenance=manifest.iter().find(|p|p["versionId"]==material["knowledgeVersionId"]).unwrap();
        let group=transcript_group(d,material,provenance);
        let replace=canonical.get(&group).is_none_or(|old|transcript_rank(d,material)>transcript_rank(d,old)
            || (transcript_rank(d,material)==transcript_rank(d,old)&&text(material,"id")<text(old,"id")));
        if replace {canonical.insert(group,material.clone());}
    }
    let mut selected_versions:BTreeSet<String>=canonical.values().map(|m|text(m,"knowledgeVersionId").to_owned()).collect();
    // An owner attestation pins a particular source revision. Generic ranking
    // cannot substitute another transcript for that directional evidence.
    selected_versions.extend(materials.iter().filter(|m|m["audioEquivalence"].as_array().is_some_and(|a|!a.is_empty()))
        .map(|m|text(m,"knowledgeVersionId").to_owned()));
    materials.retain(|m|m["kind"]!="transcript"||selected_versions.contains(text(m,"knowledgeVersionId")));
    manifest.retain(|m|m["kind"]!="transcript"||selected_versions.contains(text(m,"versionId")));
    manifest.sort_by_key(|v| text(v, "entryId").to_owned());
    excluded.sort_by_key(|v| text(v, "entryId").to_owned());
    let mut bundle=json!({"materials":materials,"manifest":manifest,"excluded":excluded,"policyVersion":1});
    let exact=crate::media_analysis_reuse::bindings(d,at).map_err(|_|"Media analysis applicability invalid")?;
    crate::media_analysis_reuse::append_selected(&mut bundle,&exact,&targets)
        .map_err(|_|"Media analysis selection invalid")?;
    Ok(bundle)
}

/// Manual instructions are the operator's explicit rules, never promoted feedback or
/// imported text. The backing material and catalog version are written together.
pub fn save_instruction(d: &mut Value, body: &Value, at: &str) -> Result<Value, &'static str> {
    validate(d)?;
    timestamp(at)?;
    let object = body.as_object().ok_or("Instruction must be an object")?;
    if object.keys().any(|k| !["requestId", "title", "text", "postKey", "entryId", "expectedVersionId"].contains(&k.as_str())) {
        return Err("Unsupported instruction field");
    }
    for (key, limit) in [("requestId", 160), ("title", 240), ("text", 24000)] {
        let value = body[key].as_str().ok_or("Instruction requestId, title and text are required")?;
        if value.trim().is_empty() || value.encode_utf16().count() > limit {
            return Err("Instruction field is empty or too long");
        }
    }
    for key in ["postKey", "entryId", "expectedVersionId"] {
        if !body[key].is_null() && body[key].as_str().is_none_or(|s| s.trim().is_empty() || s.len() > 512) {
            return Err("Invalid instruction identifier");
        }
    }
    let binding = super::active_binding(d).map_err(|_| "Instruction account is not configured")?.to_json();
    let post_key = text(body, "postKey");
    let scope = json!({"account":account(d),"postKeys":if post_key.is_empty(){vec![]}else{vec![post_key]}});
    let payload = json!({"requestId":body["requestId"],"title":text(body,"title").trim(),"text":text(body,"text").trim(),"scope":scope,"entryId":body["entryId"],"expectedVersionId":body["expectedVersionId"]});
    let request_hash = hash(&payload);
    // Resolve retries before CAS: the original write may already have advanced the head.
    if let Some(v) = rows(d, "knowledge_versions").iter().find(|v| v["manualInstruction"] == true && v["operatorRequestId"] == body["requestId"] && v["scope"]["account"] == scope["account"]) {
        if v["operatorRequestHash"] != request_hash {
            return Err("Instruction requestId was already used with different input");
        }
        return Ok(instruction_receipt(v, true));
    }
    if !post_key.is_empty() {
        let matches: Vec<_> = rows(d, "posts").iter().filter(|p| p["postKey"] == post_key).collect();
        if matches.len() != 1 { return Err("Instruction postKey must identify one existing post"); }
        let post = matches[0];
        if post.get("connectorBinding").is_some_and(|b| b != &binding)
            || ["account", "accountId"].iter().any(|k| post.get(*k).is_some_and(|a| a != &scope["account"])) {
            return Err("Instruction post belongs to another account");
        }
    }
    let previous = if let Some(eid) = body["entryId"].as_str() {
        let e = rows(d, "knowledge_entries").iter().find(|e| e["id"] == eid).ok_or("Instruction entry not found")?;
        if body["expectedVersionId"] != e["currentVersionId"] { return Err("Knowledge version conflict"); }
        let v = rows(d, "knowledge_versions").iter().find(|v| v["id"] == e["currentVersionId"]).ok_or("Missing knowledge head")?;
        if v["manualInstruction"] != true || v["scope"] != scope { return Err("Instruction scope cannot change or target imported knowledge"); }
        Some(v.clone())
    } else {
        if !body["expectedVersionId"].is_null() { return Err("Instruction entryId is required for an update"); }
        None
    };
    let source = previous.as_ref().map(|v| text(v,"sourceMaterialId").to_owned()).unwrap_or_else(|| format!("operator-instruction-{}", uuid::Uuid::new_v4()));
    let eid = format!("knowledge-{}", sha(source.as_bytes()));
    let revision = if previous.is_some() {
        let m = rows(d,"materials").iter().find(|m| m["id"] == source).ok_or("Instruction backing material missing")?;
        m["revision"].as_u64().unwrap_or(0) + 1
    } else { 1 };
    let material = json!({"id":source,"title":payload["title"],"text":payload["text"],"kind":"rule","postKey":post_key,"sourceUrl":"","revision":revision,"updatedAt":at,"manualInstruction":true,"locallyEdited":true});
    let mut v = json!({"entryId":eid,"sourceMaterialId":source,"sourceRevision":revision,"sourceHash":hash(&content(&material)),
        "title":material["title"],"text":material["text"],"sourceUrl":"","postKey":post_key,"kind":"rule","category":"operator_instruction",
        "scope":scope,"trust":"verified","status":"active","validFrom":at,"validUntil":null,"sourceDate":null,
        "supersedes":previous.as_ref().map(|v|v["id"].clone()),"createdAt":at,"changedBy":"operator","manualInstruction":true,
        "verificationProvenance":"Explicit operator instruction saved through the manual instruction form",
        "operatorRequestId":body["requestId"],"operatorRequestHash":request_hash});
    let digest = version_hash(&v);
    v["hash"] = json!(digest);
    v["id"] = json!(format!("knowledge-version-{digest}"));
    let receipt = instruction_receipt(&v, false);
    for (collection, record) in [("materials", material), ("knowledge_entries", receipt["entry"].clone())] {
        if !d[collection].is_array() { d[collection] = json!([]); }
        let target = d[collection].as_array_mut().unwrap();
        if let Some(old) = target.iter_mut().find(|r| r["id"] == record["id"]) { *old = record; } else { target.push(record); }
    }
    if !d["knowledge_versions"].is_array() { d["knowledge_versions"] = json!([]); }
    d["knowledge_versions"].as_array_mut().unwrap().push(v);
    Ok(receipt)
}

fn instruction_receipt(v: &Value, replayed: bool) -> Value {
    json!({"entry":{"id":v["entryId"],"sourceMaterialId":v["sourceMaterialId"],"currentVersionId":v["id"],"kind":v["kind"],"scope":v["scope"],"status":v["status"]},
        "version":v,"material":{"id":v["sourceMaterialId"],"title":v["title"],"text":v["text"],"kind":"rule","postKey":v["postKey"],"sourceUrl":"","revision":v["sourceRevision"],"updatedAt":v["createdAt"],"manualInstruction":true,"locallyEdited":true},"replayed":replayed})
}

/// Explicit operator changes append versions; they never rewrite historical evidence.
pub fn revise(
    d: &mut Value,
    entry_id: &str,
    body: &Value,
    at: &str,
) -> Result<Value, &'static str> {
    validate(d)?;
    timestamp(at)?;
    let fields=body.as_object().ok_or("Knowledge revision must be an object")?;
    if fields.keys().any(|key|!["expectedVersionId","restoreVersionId","status","validFrom","validUntil","trust","provenance","title","text","sourceUrl","sourceAvailability"].contains(&key.as_str())) {
        return Err("Unsupported knowledge revision field");
    }
    let index = rows(d, "knowledge_entries")
        .iter()
        .position(|e| e["id"] == entry_id)
        .ok_or("Knowledge entry not found")?;
    let e = &d["knowledge_entries"][index];
    if !body["expectedVersionId"].is_string() || body["expectedVersionId"] != e["currentVersionId"]
    {
        return Err("Knowledge version conflict");
    }
    let current = rows(d, "knowledge_versions")
        .iter()
        .find(|v| v["id"] == e["currentVersionId"])
        .ok_or("Missing knowledge head")?;
    let mut v = if let Some(id) = body["restoreVersionId"].as_str() {
        rows(d, "knowledge_versions")
            .iter()
            .find(|v| v["id"] == id && v["entryId"] == entry_id)
            .ok_or("Restore version does not belong to entry")?
            .clone()
    } else {
        current.clone()
    };
    let correction=["title","text","sourceUrl","sourceAvailability"].iter().any(|key|body.get(*key).is_some());
    let mut corrected_material=None;
    if correction {
        if !matches!(text(current,"kind"),"fact"|"reference") || current["category"]=="reply_url_policy"
            || body.get("restoreVersionId").is_some() || body["trust"]!="verified" {
            return Err("Fact/reference correction requires explicit verification and cannot restore or edit policy");
        }
        let provenance=text(body,"provenance");
        if provenance.trim().is_empty() || provenance.encode_utf16().count()>4000 {return Err("Fact correction requires bounded verification provenance");}
        if !in_account(current,supported_account(d)?) {return Err("Fact correction belongs to another account");}
        let matches:Vec<_>=rows(d,"materials").iter().enumerate().filter(|(_,m)|m["id"]==current["sourceMaterialId"]).collect();
        if matches.len()!=1 {return Err("Fact correction requires one backing material");}
        let (material_index,material)=matches[0];
        if !in_account(material,account(d)) || current["sourceHash"]!=hash(&content(material)) {
            return Err("Fact backing source changed; review its current version first");
        }
        let mut material=material.clone();
        // A historical restore may intentionally differ from the last observed
        // upstream material. A new explicit correction makes its backing row
        // agree with the corrected version rather than retaining mixed text.
        for key in ["title","text","sourceUrl"]{material[key]=v[key].clone();}
        material.as_object_mut().unwrap().remove("sourceAvailability");
        if let Some(availability)=v.get("sourceAvailability"){material["sourceAvailability"]=availability.clone();}
        for (key,limit) in [("title",240),("text",24000)] {
            if let Some(value)=body.get(key) {
                let text=value.as_str().ok_or("Fact title/text must be strings")?;
                if text.trim().is_empty() || text.encode_utf16().count()>limit {return Err("Fact correction text is empty or too long");}
                v[key]=json!(text.trim());material[key]=v[key].clone();
            }
        }
        if let Some(url)=body.get("sourceUrl") {
            let url=url.as_str().ok_or("Fact source URL must be a string")?;
            if !url.is_empty(){locator_url(url)?;}
            if v["sourceUrl"]!=url {
                v.as_object_mut().unwrap().remove("sourceAvailability");material.as_object_mut().unwrap().remove("sourceAvailability");
            }
            v["sourceUrl"]=json!(url);material["sourceUrl"]=json!(url);
        }
        if let Some(availability)=body.get("sourceAvailability") {
            validate_source_availability(availability,text(&v,"sourceUrl"),Some(at))?;
            v["sourceAvailability"]=availability.clone();material["sourceAvailability"]=availability.clone();
        }
        let revision=material["revision"].as_u64().filter(|revision|*revision>0).ok_or("Fact backing material revision is missing")?
            .checked_add(1).ok_or("Fact material revision overflow")?;
        material["revision"]=json!(revision);material["updatedAt"]=json!(at);material["locallyEdited"]=json!(true);
        v["sourceRevision"]=json!(revision);v["sourceHash"]=json!(hash(&content(&material)));
        v["factCorrection"]=json!({"schemaVersion":1,"basis":"explicit_operator_verification","previousVersionId":current["id"],"previousSourceHash":current["sourceHash"],"sourceMaterialId":current["sourceMaterialId"],"generationDispatched":false,"externalActions":0});
        corrected_material=Some((material_index,material));
    }
    if body["restoreVersionId"].is_string() {
        // The restored text is intentional. Remember the latest observed source so
        // an unchanged upstream does not immediately undo the operator's rollback.
        v["restoredSourceHash"] = v["sourceHash"].clone();
        v["sourceHash"] = current["sourceHash"].clone();
        v["observedSourceRevision"] = current
            .get("observedSourceRevision")
            .unwrap_or(&current["sourceRevision"])
            .clone();
    }
    for key in ["status", "validFrom", "validUntil"] {
        if let Some(value) = body.get(key) {
            v[key] = value.clone();
        }
    }
    if !matches!(text(&v, "status"), "active" | "pending_review" | "retired") {
        return Err("Invalid knowledge status");
    }
    let from = v["validFrom"]
        .as_str()
        .ok_or("validFrom is required")
        .and_then(timestamp)?;
    let until = if v["validUntil"].is_null() {
        None
    } else {
        Some(timestamp(
            v["validUntil"].as_str().ok_or("Invalid validUntil")?,
        )?)
    };
    if until.is_some_and(|t| t <= from) {
        return Err("Knowledge validity interval must be positive");
    }
    if let Some(trust) = body.get("trust") {
        if trust != "verified"
            || !matches!(text(&v, "kind"), "fact" | "rule" | "policy" | "reference")
        {
            return Err("Unsupported knowledge trust transition");
        }
        if text(&v, "sourceUrl").is_empty() && text(body, "provenance").trim().is_empty() {
            return Err("Verified knowledge requires source provenance");
        }
        v["trust"] = trust.clone();
        v["verificationProvenance"] = body["provenance"].clone();
    }
    if v["status"] == "active"
        && !matches!(
            text(&v, "trust"),
            "verified" | "imported_policy" | "source_only"
        )
    {
        return Err("Activation requires explicit verification");
    }
    v["supersedes"] = current["id"].clone();
    v["createdAt"] = json!(at);
    v["changedBy"] = json!("operator");
    v["restoredFrom"] = body["restoreVersionId"].clone();
    let digest = version_hash(&v);
    v["hash"] = json!(digest);
    v["id"] = json!(format!("knowledge-version-{digest}"));
    if rows(d, "knowledge_versions")
        .iter()
        .any(|old| old["id"] == v["id"])
    {
        return Err("Duplicate knowledge revision");
    }
    if !d["knowledge_versions"].is_array(){return Err("Missing version collection");}
    if let Some((material_index,material))=corrected_material {d["materials"][material_index]=material;}
    d["knowledge_entries"][index]["currentVersionId"] = v["id"].clone();
    d["knowledge_entries"][index]["status"] = v["status"].clone();
    d["knowledge_entries"][index]["scope"] = v["scope"].clone();
    d["knowledge_entries"][index]["kind"] = v["kind"].clone();
    d["knowledge_versions"]
        .as_array_mut()
        .ok_or("Missing version collection")?
        .push(v.clone());
    Ok(v)
}

#[cfg(test)]
#[path="knowledge_contact_tests.rs"]
mod contact_tests;

pub fn feedback(d: &mut Value, item_id: &str, before: &Value, after: &Value, at: &str) {
    let draft = |v: &Value| {
        v.as_str()
            .or(v["draft"].as_str())
            .unwrap_or("")
            .trim()
            .to_owned()
    };
    let old = draft(before);
    let new = draft(after);
    if old == new {
        return;
    }
    let item = rows(d, "items").iter().find(|i| i["id"] == item_id);
    let proposals: Vec<Value> = rows(d, "proposals")
        .iter()
        .filter(|p| p["itemId"] == item_id)
        .map(|p| p["id"].clone())
        .collect();
    let v = json!({"id":uuid::Uuid::new_v4().to_string(),"itemId":item_id,"before":old,"after":new,"itemRevision":item.map(|i|i["revision"].clone()),"proposalIds":proposals,"status":"pending_review","kind":"draft_edit","createdAt":at});
    if !d["feedback"].is_array() {
        d["feedback"] = json!([]);
    }
    d["feedback"].as_array_mut().unwrap().push(v);
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn audio_only_requires_explicit_full_duration_proof(){
        let full=json!({"partial":false,"coverage":"full_audio","mediaDurationSeconds":1029.261,
            "audioDurationSeconds":1029.261,"sourceVersion":"source-a"});
        assert!(proven_full_audio(&full,"source-a"));
        assert!(!proven_full_audio(&full,"source-b"));
        for bad in [json!({"partial":false}),
            json!({"partial":false,"coverage":"full_audio","mediaDurationSeconds":1029.261,"audioDurationSeconds":900.0}),
            json!({"partial":true,"coverage":"full_audio","mediaDurationSeconds":1029.261,"audioDurationSeconds":1029.261})]{
            assert!(!proven_full_audio(&bad,"source-a"));
        }
        assert!(proven_full_audio(&json!({"partial":false,"coverage":"no_audio_stream",
            "mediaDurationSeconds":120.0,"audioStatus":"no_audio_stream","audioDurationSeconds":null,
            "sourceVersion":"source-a"}),"source-a"));
    }
    #[test]
    fn default_preparation_projects_only_current_owner_confirmed_audio_equivalence(){
        let mut d=crate::empty();crate::accounts::initialize(&mut d,crate::accounts::Profile::LikeAvto).unwrap();
        d["posts"]=json!([{"id":"source","postKey":"provider:source","title":"Original video",
            "sourceUrl":"https://www.youtube.com/watch?v=AbCdEf123_-","attachments":[{"type":"video"}]},
            {"id":"target","postKey":"provider:target","title":"Another video",
            "sourceUrl":"https://vk.com/video-1_1","attachments":[{"type":"video"}]}]);
        let source_version=crate::media_fullframes::source_version(&d["posts"][0],"LikeAvto");
        let target_version=crate::media_fullframes::source_version(&d["posts"][1],"LikeAvto");
        d["materials"]=json!([{"id":"source-speech","account":"LikeAvto","kind":"transcript",
            "postKey":"provider:source","sourceUrl":d["posts"][0]["sourceUrl"],"text":"Exact full audio",
            "transcription":{"partial":false,"coverage":"full_audio","sourceVersion":source_version,
                "mediaDurationSeconds":120.0,"audioDurationSeconds":120.0}},
            {"id":"stale-target","account":"LikeAvto","kind":"transcript",
                "postKey":"provider:target","text":"Unproven old audio","transcription":{"partial":false}}]);
        sync_catalog(&mut d,AT).unwrap();
        let source_head=rows(&d,"knowledge_versions").iter().find(|v|v["sourceMaterialId"]=="source-speech").unwrap();
        let binding=crate::active_binding(&d).unwrap().to_json();
        d["settings"]["mediaAudioEquivalences"]=json!({"target":{"schemaVersion":1,"status":"active",
            "revision":1,"account":"LikeAvto","connectorBinding":binding,
            "targetPostId":"target","targetPostKey":"provider:target","targetSourceVersion":target_version,
            "sourcePostId":"source","sourcePostKey":"provider:source","sourceVersion":source_version,
            "transcript":{"entryId":source_head["entryId"],"versionId":source_head["id"],"hash":source_head["hash"]}}});
        let default_policy=crate::post_media_policy::effective(&d,&d["posts"][1]).unwrap();
        assert_eq!(default_policy["mode"],"full_audio_only");
        assert_eq!(default_policy["decisionBasis"]["kind"],"default_full_video_speech");
        assert_eq!(default_policy["ownerAuthorizedAudioOnly"],false);
        assert!(TranscriptLookup::new(&d,AT).unwrap().ready_for_policy(&d["posts"][1],false).unwrap());
        let selected=Catalog::new(&d).unwrap().select_for_preparation(&[json!({"postKey":"provider:target"})],&[],AT).unwrap();
        assert_eq!(rows(&selected,"materials").len(),1);
        assert_eq!(selected["materials"][0]["text"],"Exact full audio");
        assert_eq!(selected["materials"][0]["audioEquivalence"][0]["authorization"],"owner_confirmed_same_video");
        d["posts"][1]["title"]=json!("Changed target source");
        assert!(!TranscriptLookup::new(&d,AT).unwrap().ready_for_policy(&d["posts"][1],false).unwrap());
        let stale=Catalog::new(&d).unwrap().select_for_preparation(&[json!({"postKey":"provider:target"})],&[],AT).unwrap();
        assert!(rows(&stale,"materials").is_empty(),"stale target words and retired equivalence cannot enter model context");
    }
    fn historical_locator_asr_fixture()->Value{
        let mut d=crate::empty();crate::accounts::initialize(&mut d,crate::accounts::Profile::LikeAvto).unwrap();
        let source_url="https://www.instagram.com/reel/ExactSource/";
        d["posts"]=json!([{"id":"donor","postKey":"ig:donor","title":"Same exact car","mediaSha256":"a".repeat(64),
            "sourceUrl":source_url,"attachments":[{"type":"video"}]},
            {"id":"target","postKey":"yt:target","title":"Same exact car","mediaSha256":"a".repeat(64),
                "sourceUrl":"https://www.youtube.com/watch?v=ExactTarget","attachments":[{"type":"video"}]}]);
        d["materials"]=json!([{"id":"historical-asr","account":"LikeAvto","kind":"transcript","mediaSha256":"a".repeat(64),
            "postKey":"ig:donor","sourceUrl":source_url,"text":"Full observed audio",
            "transcription":{"partial":false,"coverage":"full_audio","sourcePostKey":"ig:donor",
                "sourceLocatorSha256":sha(source_url.as_bytes()),"mediaDurationSeconds":74.116,
                "audioDurationSeconds":73.97775}}]);
        d
    }
    #[test]
    fn historical_full_audio_locator_proves_current_donor_and_existing_shared_binding(){
        let mut d=historical_locator_asr_fixture();sync_catalog(&mut d,AT).unwrap();let target=&d["posts"][1];
        assert!(TranscriptLookup::new(&d,AT).unwrap().ready_for_policy(target,false).unwrap());
        let selected=Catalog::new(&d).unwrap().select_for_preparation(&[json!({"postKey":"yt:target"})],&[],AT).unwrap();
        assert!(selected["materials"].as_array().unwrap().iter().any(|m|m["id"]=="historical-asr"));
        assert!(selected["manifest"].as_array().unwrap().iter().any(|m|
            m["mediaBinding"].as_array().is_some_and(|bindings|bindings.iter().any(|b|
                b["postKey"]=="yt:target"&&b["identities"]==json!([format!("sha:{}","a".repeat(64))])))));
    }
    #[test]
    fn preparation_family_does_not_merge_disjoint_transcripts_through_multivideo_post(){
        let mut d=crate::empty();crate::accounts::initialize(&mut d,crate::accounts::Profile::LikeAvto).unwrap();
        let first="https://www.youtube.com/watch?v=AbCdEf123_-";let second="https://www.youtube.com/watch?v=ZyXwVu987_-";
        d["posts"]=json!([
            {"id":"first","postKey":"yt:first","sourceUrl":first,"attachments":[{"type":"video","url":first}]},
            {"id":"second","postKey":"yt:second","sourceUrl":second,"attachments":[{"type":"video","url":second}]},
            {"id":"bridge","postKey":"vk:bridge","attachments":[{"type":"video","url":first},{"type":"video","url":second}]},
            {"id":"bridge-copy","postKey":"ok:bridge","attachments":[{"type":"video","url":first},{"type":"video","url":second}]}
        ]);
        d["materials"]=json!([]);
        for n in 0..2 {
            let post=d["posts"][n].clone();let version=crate::media_fullframes::source_version(&post,"LikeAvto");
            crate::list_mut(&mut d,"materials").push(json!({"id":format!("speech-{n}"),"account":"LikeAvto","kind":"transcript",
                "postKey":post["postKey"],"sourceUrl":post["sourceUrl"],"text":format!("Exact complete speech {n}"),
                "transcription":{"partial":false,"coverage":"full_audio","sourceVersion":version,
                    "mediaDurationSeconds":120.0,"audioDurationSeconds":120.0}}));
        }
        sync_catalog(&mut d,AT).unwrap();
        let ids=["first","second","bridge","bridge-copy"].into_iter().map(str::to_owned).collect();
        let families=preparation_families(&d,&ids,AT).unwrap();
        assert_ne!(families["first"],families["second"]);
        assert_ne!(families["first"],families["bridge"]);assert_ne!(families["second"],families["bridge"]);
        assert_eq!(families["bridge"],families["bridge-copy"]);
        let catalog=Catalog::new(&d).unwrap();
        let selected=catalog.select_for_preparation(&[json!({"postKey":"vk:bridge"})],&[],AT).unwrap();
        assert_eq!(rows(&selected,"materials").iter().filter(|v|v["kind"]=="transcript").count(),2,"both transcript supports remain relevant evidence");
        let all=catalog.select_for_preparation(&rows(&d,"posts").to_vec(),&[],AT).unwrap();
        assert_eq!(rows(&all,"materials").iter().filter(|v|v["kind"]=="transcript").count(),2,"selecting donors too cannot reintroduce bridge collapse");
        assert!(rows(&selected,"manifest").iter().all(|entry|rows(entry,"mediaBinding").iter().all(|binding|binding["postKey"]=="vk:bridge")));
    }

    #[test]
    fn preparation_family_requires_exact_current_complete_media_admission(){
        let mut d=historical_locator_asr_fixture();
        d["posts"][1]["mediaSha256"]=json!("a".repeat(64));
        sync_catalog(&mut d,AT).unwrap();
        let ids=["donor".to_owned(),"target".to_owned()].into_iter().collect();
        let families=preparation_families(&d,&ids,AT).unwrap();
        assert_eq!(families["donor"],families["target"]);
        for variant in ["title_only","contradictory_hash","duration","stale_source","partial","expired"] {
            let mut bad=d.clone();
            match variant {
                "title_only"=>{bad["posts"][1].as_object_mut().unwrap().remove("mediaSha256");},
                "contradictory_hash"=>bad["posts"][1]["mediaSha256"]=json!("b".repeat(64)),
                "duration"=>bad["posts"][1]["durationSeconds"]=json!(130.0),
                "stale_source"=>bad["posts"][0]["sourceUrl"]=json!("https://www.instagram.com/reel/ChangedSource/"),
                "partial"=>bad["knowledge_versions"][0]["transcription"]["partial"]=json!(true),
                _=>bad["knowledge_versions"][0]["validUntil"]=json!(AT)
            }
            let families=preparation_families(&bad,&ids,AT).unwrap();
            assert_ne!(families["donor"],families["target"],"{variant}");
        }
    }
    fn historical_vk_attachment_locator_fixture()->Value{
        let mut d=historical_locator_asr_fixture();
        let video="https://vk.com/video-135891342_456248788";
        d["posts"][0]["sourceUrl"]=json!("https://vk.ru/wall-135891342_59733");
        d["posts"][0]["attachments"]=json!([{"type":"video","source_url":video,"url":video}]);
        d["materials"][0]["sourceUrl"]=json!(video);
        d["materials"][0]["transcription"]["sourceLocatorSha256"]=json!(sha(video.as_bytes()));
        d
    }
    #[test]
    fn historical_vk_wall_post_proves_exact_video_attachment_locator(){
        let mut d=historical_vk_attachment_locator_fixture();sync_catalog(&mut d,AT).unwrap();
        let target=&d["posts"][1];
        assert!(TranscriptLookup::new(&d,AT).unwrap().ready_for_policy(target,false).unwrap());
        let selected=Catalog::new(&d).unwrap().select_for_preparation(&[json!({"postKey":"yt:target"})],&[],AT).unwrap();
        assert!(rows(&selected,"materials").iter().any(|m|m["id"]=="historical-asr"));
        assert!(rows(&selected,"manifest").iter().any(|m|rows(m,"mediaBinding").iter().any(|b|
            b["postKey"]=="yt:target"&&b["identities"]==json!([format!("sha:{}","a".repeat(64))]))));
    }
    #[test]
    fn historical_vk_attachment_locator_rejects_ambiguous_or_stale_sources(){
        for changed in ["foreign_attachment","foreign_attachment_scope","ambiguous_attachment","ambiguous_top_level","nonvideo_attachment","bad_locator_hash","wrong_source_post","foreign_account","explicit_stale_version","unrelated_target"] {
            let mut d=historical_vk_attachment_locator_fixture();
            match changed {
                "foreign_attachment"=>{let foreign=json!("https://vk.com/video-135891342_456248789");d["posts"][0]["attachments"][0]["source_url"]=foreign.clone();d["posts"][0]["attachments"][0]["url"]=foreign;},
                "foreign_attachment_scope"=>d["posts"][0]["attachments"][0]["account"]=json!("BAW Russia"),
                "ambiguous_attachment"=>d["posts"][0]["attachments"].as_array_mut().unwrap().push(json!({"type":"video","source_url":"https://vk.com/video-135891342_456248789"})),
                "ambiguous_top_level"=>d["posts"][0]["sourceUrl"]=json!("https://vk.com/video-135891342_456248789"),
                "nonvideo_attachment"=>d["posts"][0]["attachments"][0]["type"]=json!("photo"),
                "bad_locator_hash"=>d["materials"][0]["transcription"]["sourceLocatorSha256"]=json!("b".repeat(64)),
                "wrong_source_post"=>d["materials"][0]["transcription"]["sourcePostKey"]=json!("vk:other"),
                "foreign_account"=>d["materials"][0]["account"]=json!("BAW Russia"),
                "explicit_stale_version"=>d["materials"][0]["transcription"]["sourceVersion"]=json!("b".repeat(64)),
                "unrelated_target"=>d["posts"][1]["mediaSha256"]=json!("b".repeat(64)),
                _=>unreachable!(),
            }
            sync_catalog(&mut d,AT).unwrap();
            assert!(!TranscriptLookup::new(&d,AT).unwrap().ready_for_policy(&d["posts"][1],false).unwrap(),"{changed}");
            let selected=Catalog::new(&d).unwrap().select_for_preparation(&[json!({"postKey":"yt:target"})],&[],AT).unwrap();
            assert!(!rows(&selected,"materials").iter().any(|m|m["id"]=="historical-asr"),"{changed}");
        }
    }
    #[test]
    fn historical_full_audio_locator_fails_closed_on_source_or_coverage_drift(){
        for changed in ["url","locator","source_key","account","source_version","null_version","partial","truncated","donor_media_sha","target_duration","observed_target_duration","unrelated_target"]{
            let mut d=historical_locator_asr_fixture();
            match changed{
                "url"=>d["posts"][0]["sourceUrl"]=json!("https://www.instagram.com/reel/Replaced/"),
                "locator"=>d["materials"][0]["transcription"]["sourceLocatorSha256"]=json!("a".repeat(64)),
                "source_key"=>d["materials"][0]["transcription"]["sourcePostKey"]=json!("ig:other"),
                "account"=>d["materials"][0]["account"]=json!("BAW Russia"),
                "source_version"=>d["materials"][0]["transcription"]["sourceVersion"]=json!("b".repeat(64)),
                "null_version"=>d["materials"][0]["transcription"]["sourceVersion"]=Value::Null,
                "partial"=>d["materials"][0]["transcription"]["partial"]=json!(true),
                "truncated"=>d["materials"][0]["transcription"]["audioDurationSeconds"]=json!(20.0),
                "donor_media_sha"=>d["posts"][0]["mediaSha256"]=json!("b".repeat(64)),
                "target_duration"=>d["posts"][1]["durationMs"]=json!(120000),
                "observed_target_duration"=>{
                    d["posts"][0]["mediaSha256"]=json!("b".repeat(64));
                    d["materials"][0]["mediaSha256"]=json!("b".repeat(64));
                    let evidence=crate::media_fullframes::fixture_for_post("LikeAvto",&d["posts"][1]);
                    let target_url=d["posts"][1]["sourceUrl"].clone();
                    d["materials"].as_array_mut().unwrap().push(json!({"id":"target-visual","account":"LikeAvto",
                        "kind":"visual_context","postKey":"yt:target","sourceUrl":target_url,
                        "mediaSha256":evidence["source"]["mediaSha256"],"text":"Observed frames","visualEvidence":evidence}));
                },
                "unrelated_target"=>d["posts"][1]["mediaSha256"]=json!("b".repeat(64)),
                _=>unreachable!(),
            }
            sync_catalog(&mut d,AT).unwrap();
            assert!(!TranscriptLookup::new(&d,AT).unwrap().ready_for_policy(&d["posts"][1],false).unwrap(),"{changed}");
            let selected=Catalog::new(&d).unwrap().select_for_preparation(&[json!({"postKey":"yt:target"})],&[],AT).unwrap();
            assert!(!selected["materials"].as_array().unwrap().iter().any(|m|m["id"]=="historical-asr"),"{changed}");
        }
    }
    #[test]
    fn version_hash_survives_fractional_media_metadata_json_roundtrip() {
        // Observed OCR interval: the default fast JSON parser previously read
        // this serialized value one ULP lower, invalidating immutable history.
        let mut version=json!({"kind":"transcript","text":"preserved speech",
            "transcription":{"ocr":{"intervalSeconds":110.981_f64/30.0}}});
        assert_eq!(version["transcription"]["ocr"]["intervalSeconds"].to_string(),"3.6993666666666667");
        let digest=version_hash(&version);
        version["hash"]=json!(digest);
        version["id"]=json!(format!("knowledge-version-{digest}"));
        let restored:Value=serde_json::from_str(&version.to_string()).unwrap();
        assert_eq!(restored,version);
        assert_eq!(version_hash(&restored),digest);
        let mut changed=restored;
        changed["text"]=json!("changed speech");
        assert_ne!(version_hash(&changed),digest,"integrity validation must still reject content changes");
    }
    const AT: &str = "2026-09-22T12:00:00Z";
    #[test]
    fn visual_exact_hash_reuse_rejects_duration_hash_and_account_conflicts(){
        for account in ["LikeAvto","BAW Russia"] {
            let mut d=json!({"account":account,"posts":[
                {"id":"a","postKey":"a","title":"Specific owner accepted title","attachments":[{"type":"video"}]},
                {"id":"b","postKey":"b","title":"Specific owner accepted title #tag","attachments":[{"type":"video"}]}
            ],"materials":[{"id":"v","postKey":"a","account":account,"kind":"visual_context","mediaSha256":"b".repeat(64),"text":"Visual samples",
                "visualEvidence":super::super::media_fullframes::fixture(account,"a")}]});
            d["materials"][0]["visualEvidence"]=super::super::media_fullframes::fixture_for_post(account,&d["posts"][0]);
            d["materials"][0]["mediaSha256"]=d["materials"][0]["visualEvidence"]["source"]["mediaSha256"].clone();
            d["posts"][1]["mediaSha256"]=d["materials"][0]["mediaSha256"].clone();
            sync_catalog(&mut d,AT).unwrap();
            assert!(TranscriptLookup::new(&d,AT).unwrap().has_visual(&d["posts"][1]).unwrap());
            let selected=select(&d,&[json!({"postKey":"b"})],&[],AT).unwrap();
            assert_eq!(selected["manifest"][0]["mediaBinding"][0]["identities"],json!([format!("sha:{}",text(&d["materials"][0],"mediaSha256"))]));
            assert_eq!(selected["materials"][0]["visualEvidence"],d["materials"][0]["visualEvidence"]);
            d["posts"][1]["durationMs"]=json!(22000);
            assert!(!TranscriptLookup::new(&d,AT).unwrap().has_visual(&d["posts"][1]).unwrap());
            d["posts"][1].as_object_mut().unwrap().remove("durationMs");
            d["posts"][1]["mediaSha256"]=json!("d".repeat(64));
            assert!(!TranscriptLookup::new(&d,AT).unwrap().has_visual(&d["posts"][1]).unwrap());
            assert!(select(&d,&[json!({"postKey":"b"})],&[],AT).unwrap()["materials"].as_array().unwrap().is_empty());
            d["posts"][1]["mediaSha256"]=json!("b".repeat(64));d["posts"][1]["account"]=json!("Other");
            assert!(!TranscriptLookup::new(&d,AT).unwrap().has_visual(&d["posts"][1]).unwrap());
        }
    }
    #[test]
    fn completed_visual_proof_never_survives_source_post_version_drift(){
      for field in ["sourceUrl","title","durationMs","mediaSha256"]{
        let post=json!({"id":"source","postKey":"p","title":"Original title","sourceUrl":"https://youtu.be/AbCdEf123_-","attachments":[{"type":"video"}]});let evidence=super::super::media_fullframes::fixture_for_post("LikeAvto",&post);
        let mut d=json!({"account":"LikeAvto","posts":[post],"materials":[{"id":"visual","kind":"visual_context","account":"LikeAvto","postKey":"p","sourceUrl":post["sourceUrl"],"mediaSha256":evidence["source"]["mediaSha256"],"text":"Observed context","visualEvidence":evidence}]});sync_catalog(&mut d,AT).unwrap();assert!(TranscriptLookup::new(&d,AT).unwrap().has_visual(&d["posts"][0]).unwrap());
        d["posts"][0][field]=if field=="durationMs"{json!(22000)}else{json!("changed")};assert!(!TranscriptLookup::new(&d,AT).unwrap().has_visual(&d["posts"][0]).unwrap(),"{field}");assert!(select(&d,&[json!({"postKey":"p"})],&[],AT).unwrap()["materials"].as_array().unwrap().is_empty());
      }
    }
    #[test]
    fn explicitly_partial_audio_does_not_release_video(){
        let mut d=json!({"account":"LikeAvto","posts":[{"postKey":"p","attachments":[{"type":"video"}]}],"materials":[{"id":"a","kind":"transcript","postKey":"p","text":"First900seconds","transcription":{"partial":true}}]});
        sync_catalog(&mut d,AT).unwrap();assert!(!TranscriptLookup::new(&d,AT).unwrap().audio(&d["posts"][0],true).unwrap());
    }
    #[test]
    fn catalog_index_does_not_cache_time_dependent_selection(){
        let mut d=json!({"account":"LikeAvto","posts":[],"materials":[{"id":"m","postKey":"p","kind":"transcript","text":"Current transcript"}]});
        sync_catalog(&mut d,AT).unwrap();
        d["knowledge_versions"][0]["validUntil"]=json!("2026-09-22T13:00:00Z");
        d["knowledge_versions"][0]["hash"]=json!(version_hash(&d["knowledge_versions"][0]));
        let catalog=Catalog::new(&d).unwrap();let items=vec![json!({"postKey":"p"})];
        let before=catalog.select(&items,&[],AT).unwrap();
        let after=catalog.select(&items,&[],"2026-09-22T13:00:00Z").unwrap();
        assert_eq!(rows(&before,"materials").len(),1);assert!(rows(&after,"materials").is_empty());
        assert_eq!(after["excluded"][0]["reason"],"expired");
        assert_eq!(before,select(&d,&items,&[],AT).unwrap());
        assert_eq!(after,select(&d,&items,&[],"2026-09-22T13:00:00Z").unwrap());
    }
    #[test]
    fn indexed_shared_binding_preserves_reference_semantics() {
        let initial=json!({"account":"LikeAvto","posts":[
            {"postKey":"source","title":"Видео про автомобиль","sourceUrl":"https://youtu.be/AbCdEf123_-"},
            {"postKey":"twin","title":"ВИДЕО про автомобиль #tag","attachments":[{"type":"video"}]},
            {"postKey":"unrelated","title":"Другое видео","sourceUrl":"https://instagram.com/reel/other/"}
        ]});
        for variant in 0..6 {
            let mut d=initial.clone();
            match variant {
                1=>{d["posts"][0]["canonicalMediaId"]=json!("a");d["posts"][1]["canonicalMediaId"]=json!("b");},
                2=>{d["posts"][2]["account"]=json!("Other");d["posts"][2]["sourceUrl"]=d["posts"][0]["sourceUrl"].clone();},
                3=>{d["posts"][1]["title"]=json!("Публикация LikeAvto");},
                4=>{d["posts"][2]["title"]=d["posts"][1]["title"].clone();d["posts"][0]["canonicalMediaId"]=json!("a");d["posts"][2]["canonicalMediaId"]=json!("b");},
                5=>{d["posts"][1]["sourceUrl"]=d["posts"][0]["sourceUrl"].clone();d["posts"][1]["title"]=json!("Different title same source");},
                _=>{}
            }
            let targets:Vec<_>=rows(&d,"posts").iter().collect();
            let facts:Vec<_>=targets.iter().map(|p|(*p,MediaFacts::new(p,"LikeAvto"))).collect();
            let index=MediaLookup::new(&d,"LikeAvto");
            for kind in ["transcript","ocr","rule"] {
                let material=json!({"kind":kind,"postKey":"source","text":"Evidence"});
                assert_eq!(index.shared_binding("LikeAvto",&material,&facts),shared_media_binding(&d,&material,&targets),"variant {variant}, kind {kind}");
            }
        }
    }
    #[test]
    #[ignore = "requires private offline snapshot; no live database or provider calls"]
    fn profile_transcript_snapshot_equivalence() {
        let path=std::env::var("COMMUNITYHERO_PROFILE_SNAPSHOT").expect("snapshot path");
        let d:Value=serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
        let at=chrono::Utc::now().to_rfc3339();
        let started=std::time::Instant::now();
        let lookup=TranscriptLookup::new(&d,&at).unwrap();
        eprintln!("transcript_lookup_build_ms={}",started.elapsed().as_millis());
        let keys:BTreeSet<_>=rows(&d,"items").iter().filter(|i|i["workflow"]=="attention").map(|i|text(i,"postKey")).collect();
        let mut missing=Vec::new();let mut covered=0;
        let started=std::time::Instant::now();
        for post in rows(&d,"posts").iter().filter(|p|keys.contains(text(p,"postKey"))&&is_video_post(p)){
            let has=lookup.has(post).unwrap();
            let selected=select(&d,&[],std::slice::from_ref(post),&at).unwrap();
            let expected=rows(&selected,"materials").iter().any(|m|m["kind"]=="transcript"&&!text(m,"text").trim().is_empty());
            assert_eq!(has,expected,"{}",text(post,"postKey"));
            if has{covered+=1;}else{missing.push(json!({"postKey":post["postKey"],"title":post["title"]}));}
        }
        eprintln!("transcript_compare_ms={} covered_posts={covered} missing={}",started.elapsed().as_millis(),json!(missing));
    }
    fn fixture() -> (Value, Value) {
        let d = json!({"account":"LikeAvto","materials":[{"id":"policy","title":"Style","text":"Be concise","revision":1},{"id":"video","kind":"transcript","text":"Video evidence","postKey":"post-a","revision":1},{"id":"other","kind":"ocr","text":"Other post","postKey":"post-b"},{"id":"unknown","text":"Unverified claim"}]});
        let m = json!({"entries":{"policy":{"kind":"rule","trust":"imported_policy","status":"active","scope":{"account":"LikeAvto","postKeys":[]},"contentHash":sha(b"Be concise")}}});
        (d, m)
    }
    #[test]
    fn canonical_video_urls_require_exact_provider_identity() {
        for url in ["https://youtube.com/watch?v=AbCdEf123_-&t=2", "https://youtu.be/AbCdEf123_-", "https://www.youtube.com/shorts/AbCdEf123_-/", "https://music.youtube.com/embed/AbCdEf123_-"] {
            assert_eq!(canonical_video_url(url).as_deref(), Some("yt:AbCdEf123_-"));
        }
        for url in ["https://vk.com/video-123_456", "https://vkvideo.ru/video-123_456?list=ignored"] {
            assert_eq!(canonical_video_url(url).as_deref(), Some("vk:-123_456"));
        }
        assert_eq!(canonical_video_url("https://instagram.com/reel/Ab_C-12/").as_deref(), Some("ig:Ab_C-12"));
        for url in ["https://youtube.com.evil/watch?v=AbCdEf123_-", "https://evil@youtube.com/watch?v=AbCdEf123_-", "file://youtube.com/watch?v=AbCdEf123_-", "https://youtu.be/AbCdEf123_-/other", "https://youtube.com/watch?v=AbCdEf123_-&v=XbCdEf123_-", "https://vk.com/wall-123_456", "https://youtube.com/watch?v=short"] {
            assert_eq!(canonical_video_url(url), None, "{url}");
        }
    }
    #[test]
    fn exact_source_duration_tolerance_matches_audio_selection_and_queue_groups() {
        for active in ["LikeAvto","BAW Russia"] {
            let mut d=json!({"account":active,"posts":[
                {"id":"a","postKey":"source","account":active,"title":"Exact shared title","sourceUrl":"https://youtu.be/AbCdEf123_-","durationMs":44000},
                {"id":"b","postKey":"target","account":active,"title":"Exact shared title","sourceUrl":"https://youtube.com/watch?v=AbCdEf123_-","durationSeconds":44.8}
            ],"materials":[{"id":"transcript","account":active,"postKey":"source","kind":"transcript","text":"Account-local words","transcription":{"partial":false}}]});
            sync_catalog(&mut d,AT).unwrap();
            assert!(post_has_transcript(&d,&d["posts"][1],AT).unwrap());
            let groups=media_groups(&d,active);assert_eq!(groups["a"],groups["b"]);
            assert_eq!(rows(&select(&d,&[json!({"postKey":"target"})],&[],AT).unwrap(),"materials").len(),1);
            d["posts"][1]["durationSeconds"]=json!(47.0);
            assert!(!post_has_transcript(&d,&d["posts"][1],AT).unwrap());
            let groups=media_groups(&d,active);assert_ne!(groups["a"],groups["b"]);
            assert!(rows(&select(&d,&[json!({"postKey":"target"})],&[],AT).unwrap(),"materials").is_empty());
        }
    }
    #[test]
    fn same_title_different_source_never_reuses_partial_audio() {
        let mut d=json!({"account":"LikeAvto","posts":[
            {"postKey":"failed-yt","title":"  Бюджетный кроссовер — Changan Q05 #лайкавто #импорт","sourceUrl":"https://youtu.be/AbCdEf123_-"},
            {"postKey":"success-ig","title":"Публикация LikeAvto","text":"Бюджетный   кроссовер — Changan Q05\nDetails","sourceUrl":"https://instagram.com/reel/ABC/"}
        ],"materials":[{"id":"existing","postKey":"success-ig","kind":"transcript","text":"Existing words","transcription":{"partial":true,"maxAudioSeconds":900}}]});
        sync_catalog(&mut d,AT).unwrap();
        let s=select(&d,&[json!({"postKey":"failed-yt"})],&[],AT).unwrap();
        assert!(rows(&s,"materials").is_empty());
        assert!(rows(&s,"manifest").is_empty());
        assert!(!post_has_transcript(&d,&d["posts"][0],AT).unwrap());
        assert_eq!(d["materials"][0]["transcription"]["partial"],true,"source history remains intact");
        d["posts"][0]["title"]=json!("Бюджетный кроссовер — Changan Q06");
        assert!(!post_has_transcript(&d,&d["posts"][0],AT).unwrap());
        d["posts"][0]["title"]=json!("Бюджетный кроссовер — Changan Q05");
        d["posts"][1]["accountId"]=json!("Other");
        assert!(!post_has_transcript(&d,&d["posts"][0],AT).unwrap());
    }
    #[test]
    fn exact_identity_reuse_is_available_inside_each_account_and_never_crosses_accounts() {
        for active in ["LikeAvto", "BAW Russia"] {
            let mut d=json!({"account":active,"posts":[
                {"postKey":"source","account":active,"title":"Exact shared title","sourceUrl":"https://youtu.be/AbCdEf123_-","canonicalMediaId":"exact-video"},
                {"postKey":"target","account":active,"title":"Different title","sourceUrl":"https://instagram.com/reel/ABC/","canonicalMediaId":"exact-video"}
            ],"materials":[{"id":"transcript","account":active,"postKey":"source","kind":"transcript","text":"Account-local words"}]});
            sync_catalog(&mut d,AT).unwrap();
            assert!(post_has_transcript(&d,&d["posts"][1],AT).unwrap(),"{active}");
            let selected=select(&d,&[json!({"postKey":"target"})],&[],AT).unwrap();
            assert_eq!(selected["manifest"][0]["mediaBinding"][0]["identities"],json!(["canonical:exact-video"]));

            let foreign=if active == "LikeAvto" { "BAW Russia" } else { "LikeAvto" };
            let mut isolated=json!({"account":active,"posts":d["posts"],"materials":[
                {"id":"foreign","account":foreign,"postKey":"source","kind":"transcript","text":"Foreign-account words"}
            ]});
            sync_catalog(&mut isolated,AT).unwrap();
            assert!(rows(&isolated,"knowledge_entries").is_empty());
            assert!(!post_has_transcript(&isolated,&isolated["posts"][1],AT).unwrap(),"{active}");
        }
    }
    #[test]
    fn bundled_likeavto_manifest_never_promotes_baw_materials() {
        let manifest=json!({"entries":{"policy":{"kind":"rule","trust":"imported_policy","status":"active","scope":{"account":"LikeAvto","postKeys":[]},"contentHash":sha(b"Be concise")}}});
        let mut d=json!({"account":"BAW Russia","materials":[{"id":"policy","account":"BAW Russia","title":"Style","text":"Be concise","revision":1}]});
        sync_with_manifest(&mut d,AT,&manifest).unwrap();
        assert_eq!(d["knowledge_versions"][0]["kind"],"reference");
        assert_eq!(d["knowledge_versions"][0]["trust"],"unverified");
        assert_eq!(d["knowledge_versions"][0]["status"],"pending_review");
        assert_eq!(d["knowledge_versions"][0]["scope"]["account"],"BAW Russia");
    }
    #[test]
    fn titles_never_grant_identity_and_explicit_conflicts_still_veto() {
        for title in [""," ","None","Video by likeavto_import","Clip by @likeavto_import","Публикация LikeAvto","Same full title"] {
            let a=json!({"title":title,"sourceUrl":"https://youtu.be/AbCdEf123_-"});
            let b=json!({"title":title,"sourceUrl":"https://instagram.com/reel/ABC/"});
            let d=json!({"account":"LikeAvto","posts":[]});
            assert!(MediaLookup::new(&d,"LikeAvto").shared_binding("LikeAvto",&json!({"kind":"transcript","sourceUrl":a["sourceUrl"],"title":title}),&[(&b,MediaFacts::new(&b,"LikeAvto"))]).is_empty());
        }
        let a=json!({"title":"Same full title","mediaSha256":"a".repeat(64)});
        let b=json!({"title":"same full title","contentSha256":"b".repeat(64)});
        assert!(media_conflict(&a,&b,"LikeAvto"));
    }
    #[test]
    fn one_canonical_transcript_per_video_group_preserves_all_source_history() {
        let mut d=json!({"account":"LikeAvto","posts":[
            {"postKey":"yt","title":"Suzuki Jimny","sourceUrl":"https://youtu.be/AbCdEf123_-","canonicalMediaId":"same-jimny-video"},
            {"postKey":"ig","title":"Suzuki Jimny #tag","sourceUrl":"https://instagram.com/reel/Jimny/","canonicalMediaId":"same-jimny-video"},
            {"postKey":"vk","title":"Suzuki Jimny","attachments":[{"type":"video"}],"canonicalMediaId":"same-jimny-video"},
            {"postKey":"other","title":"Different video","attachments":[{"type":"video"}]}
        ],"materials":[
            {"id":"t0","postKey":"yt","kind":"transcript","text":"First ASR","updatedAt":"2026-09-22T00:00:00Z","transcription":{"partial":true,"maxAudioSeconds":900}},
            {"id":"t1","postKey":"ig","kind":"transcript","text":"Full ASR","updatedAt":"2026-09-22T00:00:00Z","transcription":{"partial":false,"maxAudioSeconds":1200}},
            {"id":"t2","postKey":"vk","kind":"transcript","text":"Recent ASR","updatedAt":"2026-09-23T00:00:00Z","transcription":{"partial":true,"maxAudioSeconds":900}},
            {"id":"t3","postKey":"other","kind":"transcript","text":"Other video"}
        ]});
        sync_catalog(&mut d,AT).unwrap();
        let history=d.clone();
        for key in ["yt","ig","vk"] {
            let selected=select(&d,&[json!({"postKey":key})],&[],AT).unwrap();
            assert_eq!(rows(&selected,"materials").len(),1);
            assert_eq!(selected["materials"][0]["id"],"t1");
        }
        let all=select(&d,&[json!({"postKey":"yt"}),json!({"postKey":"other"})],&[],AT).unwrap();
        assert_eq!(rows(&all,"materials").len(),2);
        assert_eq!(d,history);
        d["materials"][1].as_object_mut().unwrap().remove("transcription");
        sync_catalog(&mut d,AT).unwrap();
        let selected=select(&d,&[json!({"postKey":"yt"})],&[],AT).unwrap();
        assert_eq!(selected["materials"][0]["id"],"t2");
    }
    #[test]
    fn shared_video_source_url_survives_missing_origin_without_broadening_rules() {
        let mut d = json!({"account":"LikeAvto","posts":[{"postKey":"vk-post","platform":"vk","sourceUrl":"https://vk.com/wall-123_4","attachments":[{"type":"video","url":"https://youtu.be/AbCdEf123_-"}]}],"materials":[
            {"id":"transcript","kind":"transcript","postKey":"yt-unloaded","sourceUrl":"https://youtube.com/watch?v=AbCdEf123_-","text":"Confirmed words"},
            {"id":"similar","kind":"transcript","postKey":"different","sourceUrl":"https://youtube.com/watch?v=XbCdEf123_-","title":"Same title","text":"Unrelated words"}
        ]});
        sync_catalog(&mut d, AT).unwrap();
        let rule = save_instruction(&mut d, &json!({"requestId":"r","title":"Only local","text":"Rule text","postKey":"vk-post"}), AT).unwrap();
        // Rebind the rule as a valid historical rule for the transcript's origin.
        let last = d["knowledge_versions"].as_array().unwrap().len() - 1;
        d["knowledge_versions"][last]["scope"]["postKeys"] = json!(["yt-unloaded"]);
        d["knowledge_versions"][last]["postKey"] = json!("yt-unloaded");
        d["knowledge_versions"][last]["sourceUrl"] = json!("https://youtu.be/AbCdEf123_-");
        let digest = version_hash(&d["knowledge_versions"][last]);
        d["knowledge_versions"][last]["hash"] = json!(digest);
        let entry = d["knowledge_entries"].as_array_mut().unwrap().iter_mut().find(|e| e["id"] == rule["entry"]["id"]).unwrap();
        entry["scope"]["postKeys"] = json!(["yt-unloaded"]);
        let result = select(&d, &[json!({"postKey":"vk-post"})], &[], AT).unwrap();
        assert_eq!(rows(&result,"materials").len(), 1);
        assert_eq!(result["materials"][0]["id"], "transcript");
        assert_eq!(result["materials"][0]["postKey"], "yt-unloaded");
        assert_eq!(result["manifest"][0]["scope"]["postKeys"], json!(["yt-unloaded"]));
        assert_eq!(result["manifest"][0]["mediaBinding"][0]["identities"], json!(["yt:AbCdEf123_-"]));
        d["posts"][0]["attachments"][0]["url"] = json!("https://youtu.be/ZbCdEf123_-");
        assert_eq!(select(&d, &[json!({"postKey":"vk-post"})], &[], AT).unwrap()["materials"], json!([]));
    }
    #[test]
    fn shared_media_uses_explicit_hashes_and_accounts_without_transitive_links() {
        let digest = "a".repeat(64);
        let mut d = json!({"account":"LikeAvto","posts":[
            {"postKey":"target","mediaSha256":digest,"canonicalMediaId":"same-video"},
            {"postKey":"origin","attachments":[{"type":"video","contentSha256":digest.to_uppercase()}]},
            {"postKey":"bridge","canonicalMediaId":"same-video","sourceUrl":"https://youtu.be/AbCdEf123_-"}
        ],"materials":[
            {"id":"shared","kind":"ocr","postKey":"origin","text":"Shared OCR"},
            {"id":"transitive","kind":"transcript","postKey":"unloaded","sourceUrl":"https://youtu.be/AbCdEf123_-","text":"Do not infer this alias"},
            {"id":"foreign","kind":"transcript","postKey":"foreign","canonicalMediaId":"same-video","accountId":"Other","text":"Foreign evidence"},
            {"id":"foreign-scope","kind":"transcript","postKey":"foreign","canonicalMediaId":"same-video","scope":{"account":"Other"},"text":"Foreign scoped evidence"},
            {"id":"thumbnail","kind":"transcript","postKey":"thumbnail","attachments":[{"type":"image","contentSha256":digest}],"text":"Not a video"}
        ]});
        sync_catalog(&mut d, AT).unwrap();
        let selected = select(&d, &[json!({"postKey":"target"})], &[], AT).unwrap();
        assert_eq!(rows(&selected,"materials").len(), 1);
        assert_eq!(selected["materials"][0]["id"], "shared");
        assert_eq!(selected["manifest"][0]["mediaBinding"][0]["identities"], json!([format!("sha:{digest}")]));
        d["posts"][1]["accountId"] = json!("Other");
        assert_eq!(select(&d, &[json!({"postKey":"target"})], &[], AT).unwrap()["materials"], json!([]));
    }
    #[test]
    fn shared_identity_changes_are_versioned_and_retirement_still_applies() {
        let mut d = json!({"account":"LikeAvto","posts":[{"postKey":"target","canonicalMediaId":"first"}],"materials":[{"id":"t","kind":"transcript","postKey":"origin","canonicalMediaId":"first","text":"Original words"}]});
        sync_catalog(&mut d, AT).unwrap();
        let first = select(&d, &[json!({"postKey":"target"})], &[], AT).unwrap();
        assert_eq!(rows(&first,"materials").len(),1);
        let previous = d["knowledge_versions"][0].clone();
        d["materials"][0]["canonicalMediaId"] = json!("second");
        sync_catalog(&mut d, AT).unwrap();
        assert_eq!(d["knowledge_versions"][0],previous);
        assert_eq!(rows(&d,"knowledge_versions").len(),2);
        assert_eq!(select(&d, &[json!({"postKey":"target"})], &[], AT).unwrap()["materials"],json!([]));
        d["posts"][0]["canonicalMediaId"] = json!("second");
        let second = select(&d, &[json!({"postKey":"target"})], &[], AT).unwrap();
        assert_ne!(hash(&first["manifest"]),hash(&second["manifest"]));
        let e = d["knowledge_entries"][0].clone();
        revise(&mut d,text(&e,"id"),&json!({"expectedVersionId":e["currentVersionId"],"status":"retired"}),AT).unwrap();
        assert_eq!(select(&d, &[json!({"postKey":"target"})], &[], AT).unwrap()["materials"],json!([]));
    }
    #[test]
    fn manual_instructions_are_scoped_and_survive_catalog_sync() {
        let (mut d, m) = fixture();
        d["posts"] = json!([{"id":"a","postKey":"post-a"},{"id":"b","postKey":"post-b"}]);
        sync_with_manifest(&mut d, AT, &m).unwrap();
        let original = d["knowledge_versions"].clone();
        let local = save_instruction(&mut d, &json!({"requestId":"local","title":"Post guidance","text":"Answer only about this post","postKey":"post-a"}), AT).unwrap();
        let global = save_instruction(&mut d, &json!({"requestId":"global","title":"Global guidance","text":"Use clear sentences"}), AT).unwrap();
        let before = d.clone();
        sync_catalog(&mut d, AT).unwrap();
        assert_eq!(d, before);
        assert_eq!(&d["knowledge_versions"].as_array().unwrap()[..original.as_array().unwrap().len()], original.as_array().unwrap());
        let selected = select(&d, &[json!({"postKey":"post-a"})], &[], AT).unwrap();
        for saved in [&local, &global] {
            assert!(rows(&selected,"manifest").iter().any(|v| v["versionId"] == saved["version"]["id"]));
        }
        let other = select(&d, &[json!({"postKey":"post-b"})], &[], AT).unwrap();
        assert!(!rows(&other,"manifest").iter().any(|v| v["versionId"] == local["version"]["id"]));
        assert!(rows(&other,"manifest").iter().any(|v| v["versionId"] == global["version"]["id"]));
        d["account"] = json!("DifferentAccount");
        assert_eq!(select(&d, &[], &[], AT), Err("Knowledge account is not configured"));
    }
    #[test]
    fn manual_instruction_replay_cas_scope_and_provenance_fail_closed() {
        let (mut d, _) = fixture();
        d["posts"] = json!([{"id":"a","postKey":"post-a"},{"id":"foreign","postKey":"foreign","accountId":"Another"}]);
        let body = json!({"requestId":"create","title":"Title","text":"Original","postKey":"post-a"});
        let first = save_instruction(&mut d, &body, AT).unwrap();
        let before = d.clone();
        let replay = save_instruction(&mut d, &body, AT).unwrap();
        assert_eq!(replay["version"], first["version"]);
        assert_eq!(replay["replayed"], true);
        assert_eq!(d, before);
        let mut change = json!({"requestId":"update","title":"Edited","text":"Changed","postKey":"post-a","entryId":first["entry"]["id"],"expectedVersionId":first["version"]["id"]});
        let next = save_instruction(&mut d, &change, AT).unwrap();
        assert_eq!(d["knowledge_versions"][0], first["version"]);
        assert_eq!(save_instruction(&mut d, &change, AT).unwrap()["version"], next["version"]);
        change["requestId"] = json!("stale");
        assert_eq!(save_instruction(&mut d, &change, AT), Err("Knowledge version conflict"));
        change["expectedVersionId"] = next["version"]["id"].clone();
        change["postKey"] = Value::Null;
        assert!(save_instruction(&mut d, &change, AT).is_err());
        for post in ["missing", "foreign"] {
            assert!(save_instruction(&mut d, &json!({"requestId":post,"title":"Title","text":"Text","postKey":post}), AT).is_err());
        }
        let mut forged = body.clone();
        forged["text"] = json!("different retry");
        assert!(save_instruction(&mut d, &forged, AT).is_err());
        forged["requestId"] = json!("forged");
        forged["trust"] = json!("verified");
        assert!(save_instruction(&mut d, &forged, AT).is_err());
        let before_sync = d.clone();
        sync_catalog(&mut d, AT).unwrap();
        assert_eq!(d["knowledge_entries"].as_array().unwrap().iter().find(|e|e["id"]==next["entry"]["id"]).unwrap(), &next["entry"]);
        assert_eq!(d["knowledge_versions"][0], before_sync["knowledge_versions"][0]);
        // Replay remains the same receipt even after a newer accepted edit.
        assert_eq!(save_instruction(&mut d, &body, AT).unwrap()["version"], first["version"]);
    }
    #[test]
    fn immutable_idempotent_and_scoped() {
        let (mut d, m) = fixture();
        sync_with_manifest(&mut d, AT, &m).unwrap();
        let initial = d.clone();
        sync_with_manifest(&mut d, "2026-09-23T12:00:00Z", &m).unwrap();
        assert_eq!(initial, d);
        let s = select(&d, &[json!({"postKey":"post-a"})], &[], AT).unwrap();
        assert_eq!(s["materials"].as_array().unwrap().len(), 2);
        assert!(!s.to_string().contains("Other post"));
        d["materials"][0]["text"] = json!("Changed policy");
        sync_with_manifest(&mut d, AT, &m).unwrap();
        assert_eq!(d["knowledge_versions"][0], initial["knowledge_versions"][0]);
        assert_eq!(d["knowledge_versions"].as_array().unwrap().len(), 5);
        assert_eq!(select(&d, &[], &[], AT).unwrap()["materials"], json!([]));
    }
    #[test]
    fn expiry_and_tampering_fail_closed() {
        let (mut d, mut m) = fixture();
        m["entries"]["policy"]["validUntil"] = json!(AT);
        sync_with_manifest(&mut d, AT, &m).unwrap();
        assert_eq!(select(&d, &[], &[], AT).unwrap()["materials"], json!([]));
        d["knowledge_versions"][0]["text"] = json!("tampered");
        assert!(select(&d, &[], &[], AT).is_err());
    }
    #[test]
    fn wrong_account_and_duplicate_heads_rejected() {
        let (mut d, m) = fixture();
        sync_with_manifest(&mut d, AT, &m).unwrap();
        d["account"] = json!("Other");
        assert_eq!(select(&d, &[], &[], AT), Err("Knowledge account is not configured"));
        let e = d["knowledge_entries"][0].clone();
        d["knowledge_entries"].as_array_mut().unwrap().push(e);
        assert!(select(&d, &[], &[], AT).is_err());
    }
    #[test]
    fn feedback_is_only_candidate() {
        let (mut d, m) = fixture();
        sync_with_manifest(&mut d, AT, &m).unwrap();
        let entries = d["knowledge_entries"].clone();
        feedback(
            &mut d,
            "x",
            &json!({"draft":"old"}),
            &json!({"draft":"new"}),
            AT,
        );
        feedback(&mut d, "x", &json!("new"), &json!(" new "), AT);
        assert_eq!(d["feedback"].as_array().unwrap().len(), 1);
        assert_eq!(d["feedback"][0]["status"], "pending_review");
        assert_eq!(d["knowledge_entries"], entries);
    }
    #[test]
    fn operator_revision_survives_sync_and_restores_as_new_version() {
        let (mut d, m) = fixture();
        sync_with_manifest(&mut d, AT, &m).unwrap();
        let e = d["knowledge_entries"][0].clone();
        let original = d["knowledge_versions"][0].clone();
        let changed = revise(
            &mut d,
            text(&e, "id"),
            &json!({"expectedVersionId":e["currentVersionId"],"status":"retired"}),
            AT,
        )
        .unwrap();
        sync_with_manifest(&mut d, AT, &m).unwrap();
        assert_eq!(d["knowledge_entries"][0]["currentVersionId"], changed["id"]);
        assert_eq!(d["knowledge_versions"][0], original);
        assert!(
            revise(
                &mut d,
                text(&e, "id"),
                &json!({"expectedVersionId":original["id"],"status":"active"}),
                AT
            )
            .is_err()
        );
        let restored = revise(
            &mut d,
            text(&e, "id"),
            &json!({"expectedVersionId":changed["id"],"restoreVersionId":original["id"]}),
            AT,
        )
        .unwrap();
        assert_ne!(restored["id"], original["id"]);
        assert_eq!(restored["status"], "active");
    }
    #[test]
    fn changed_import_and_unknown_fact_require_explicit_verification() {
        let (mut d, m) = fixture();
        d["materials"][0]["text"] = json!("Different policy under the same ID");
        sync_with_manifest(&mut d, AT, &m).unwrap();
        assert_eq!(d["knowledge_entries"][0]["status"], "pending_review");
        let e = d["knowledge_entries"][3].clone();
        assert!(
            revise(
                &mut d,
                text(&e, "id"),
                &json!({"expectedVersionId":e["currentVersionId"],"status":"active"}),
                AT
            )
            .is_err()
        );
        assert!(revise(&mut d, text(&e,"id"), &json!({"expectedVersionId":e["currentVersionId"],"status":"active","trust":"verified"}), AT).is_err());
        revise(&mut d, text(&e,"id"), &json!({"expectedVersionId":e["currentVersionId"],"status":"active","trust":"verified","provenance":"Operator checked the original specification on 2026-09-22"}), AT).unwrap();
        let selected = select(&d, &[], &[], AT).unwrap();
        assert_eq!(selected["materials"].as_array().unwrap().len(), 1);
        assert_eq!(selected["materials"][0]["id"], "unknown");
    }
    #[test]
    fn rollback_survives_unchanged_newer_source_but_next_change_requires_review() {
        let (mut d, m) = fixture();
        sync_with_manifest(&mut d, AT, &m).unwrap();
        let original = d["knowledge_versions"][0].clone();
        let entry = d["knowledge_entries"][0].clone();
        d["materials"][0]["text"] = json!("New upstream policy");
        d["materials"][0]["revision"] = json!(2);
        sync_with_manifest(&mut d, AT, &m).unwrap();
        let current = d["knowledge_entries"][0]["currentVersionId"].clone();
        let restored = revise(
            &mut d,
            text(&entry, "id"),
            &json!({"expectedVersionId":current,"restoreVersionId":original["id"]}),
            AT,
        )
        .unwrap();
        assert_eq!(restored["text"], original["text"]);
        assert_eq!(restored["observedSourceRevision"], 2);
        assert_eq!(restored["restoredSourceHash"], original["sourceHash"]);
        assert_ne!(restored["sourceHash"], original["sourceHash"]);
        let history = d["knowledge_versions"].clone();
        sync_with_manifest(&mut d, AT, &m).unwrap();
        assert_eq!(d["knowledge_versions"], history);
        assert_eq!(
            d["knowledge_entries"][0]["currentVersionId"],
            restored["id"]
        );
        assert_eq!(
            select(&d, &[], &[], AT).unwrap()["materials"][0]["text"],
            original["text"]
        );
        d["materials"][0]["text"] = json!("Another upstream policy");
        d["materials"][0]["revision"] = json!(3);
        sync_with_manifest(&mut d, AT, &m).unwrap();
        assert_eq!(d["knowledge_entries"][0]["status"], "pending_review");
        assert_ne!(
            d["knowledge_entries"][0]["currentVersionId"],
            restored["id"]
        );
        assert_eq!(d["knowledge_versions"][0], original);
    }
}
