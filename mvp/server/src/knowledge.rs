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
    match account(d) {
        "LikeAvto" => Ok("LikeAvto"),
        "BAW Russia" => Ok("BAW Russia"),
        _ => Err("Knowledge account is not configured"),
    }
}
fn title_sharing_allowed(scope: &str) -> bool {
    matches!(scope, "LikeAvto" | "BAW Russia")
}
fn content(m: &Value) -> Value {
    let mut value = json!({"title":text(m,"title"),"text":text(m,"text"),"sourceUrl":text(m,"sourceUrl"),"postKey":text(m,"postKey"),"kind":text(m,"kind"),"sourceDate":m["sourceDate"]});
    copy_media_identity(m, &mut value);
    value
}

fn copy_media_identity(source: &Value, target: &mut Value) {
    if !matches!(text(source, "kind"), "transcript" | "ocr" | "visual_context") { return; }
    for key in ["canonicalMediaId", "contentSha256", "mediaSha256", "attachments", "account", "accountId", "connectorBinding", "transcription", "visualEvidence"] {
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

fn in_account(record: &Value, expected: &str) -> bool {
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

fn generic_media_title(value: &str) -> bool {
    matches!(value, "" | "none" | "null" | "video" | "видео" | "публикация" | "публикация likeavto" | "публикация baw russia")
        || value.starts_with("video by ") || value.starts_with("clip by ")
}
fn media_title(record: &Value) -> String {
    let mut title = text(record,"title").trim().to_lowercase();
    if generic_media_title(&title) {
        let body = if text(record,"text").is_empty() { text(record,"body") } else { text(record,"text") };
        let body = body.replace("<br>","\n").replace("<br/>","\n").replace("<br />","\n").replace("<BR>","\n");
        title = body.lines().find(|line| !line.trim().is_empty()).unwrap_or("").trim().to_lowercase();
    }
    let mut words: Vec<_> = title.split_whitespace().collect();
    while words.last().is_some_and(|word| word.starts_with('#')) { words.pop(); }
    let result = words.join(" ");
    if generic_media_title(&result) || !result.chars().any(char::is_alphanumeric) { String::new() } else { result }
}

pub(crate) fn media_conflict(a: &Value, b: &Value, scope: &str) -> bool {
    let a = media_identities(a,scope);
    let b = media_identities(b,scope);
    ["sha:","canonical:"].iter().any(|prefix| {
        let x: BTreeSet<_> = a.iter().filter(|s| s.starts_with(prefix)).collect();
        let y: BTreeSet<_> = b.iter().filter(|s| s.starts_with(prefix)).collect();
        !x.is_empty() && !y.is_empty() && x.is_disjoint(&y)
    })
}
fn same_media_title(a: &Value, b: &Value, scope: &str) -> Option<String> {
    if !title_sharing_allowed(scope) || !in_account(a,scope) || !in_account(b,scope) || media_conflict(a,b,scope) { return None; }
    let video = |v: &Value| is_video_post(v) || !media_identities(v,scope).is_empty();
    if !video(a) || !video(b) { return None; }
    let title = media_title(a);
    (!title.is_empty() && title == media_title(b)).then_some(title)
}

pub(crate) fn media_group_key(post: &Value, scope: &str) -> Option<String> {
    if !in_account(post,scope) { return None; }
    let identities = media_identities(post,scope);
    let video = is_video_post(post) || !identities.is_empty();
    if !video { return None; }
    let title = media_title(post);
    if title_sharing_allowed(scope) && !title.is_empty() { return Some(format!("{scope}:title:{}",sha(title.as_bytes()))); }
    identities.iter().next().map(|id| format!("{scope}:{id}"))
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
    title: String,
    video: bool,
    in_account: bool,
}
impl MediaFacts {
    fn new(post: &Value, scope: &str) -> Self {
        let identities = media_identities(post, scope);
        Self { title: media_title(post), video: is_video_post(post) || !identities.is_empty(), in_account: in_account(post, scope), identities }
    }
}
fn identity_conflict(a: &BTreeSet<String>, b: &BTreeSet<String>) -> bool {
    ["sha:", "canonical:"].iter().any(|prefix| {
        let x: BTreeSet<_> = a.iter().filter(|s| s.starts_with(prefix)).collect();
        let y: BTreeSet<_> = b.iter().filter(|s| s.starts_with(prefix)).collect();
        !x.is_empty() && !y.is_empty() && x.is_disjoint(&y)
    })
}

/// Compute queue groups once per snapshot, retaining the conservative split of
/// every member when a title contains conflicting explicit video identities.
pub(crate) fn media_groups(d: &Value, scope: &str) -> BTreeMap<String, String> {
    let mut peers: BTreeMap<String, Vec<(&Value, MediaFacts)>> = BTreeMap::new();
    for post in rows(d, "posts") {
        if let Some(key) = media_group_key(post, scope) { peers.entry(key).or_default().push((post, MediaFacts::new(post, scope))); }
    }
    let mut result = BTreeMap::new();
    for (key, group) in peers {
        let conflict = group.iter().enumerate().any(|(i, (_, a))| group.iter().skip(i + 1).any(|(_, b)| identity_conflict(&a.identities, &b.identities)));
        for (post, _) in group {
            let id = text(post, "id");
            result.insert(id.to_owned(), if conflict { format!("{key}:post:{id}") } else { key.clone() });
        }
    }
    result
}

/// Add already observed media hashes/durations to an ephemeral identity view.
/// This never retargets or writes the original post. Two observed differing
/// copies split a formerly title-equivalent group before further reuse.
fn observed_video_posts(d:&Value,scope:&str)->Vec<Value>{
    let mut posts=rows(d,"posts").to_vec();
    let current:BTreeSet<_>=rows(d,"knowledge_entries").iter().map(|e|text(e,"currentVersionId")).collect();
    for v in rows(d,"knowledge_versions").iter().filter(|v|current.contains(text(v,"id"))&&v["kind"]=="visual_context"&&v["status"]=="active"&&in_account(v,scope)) {
        if super::media_fullframes::validate_evidence(&v["visualEvidence"]).is_err(){continue;}
        for post in posts.iter_mut().filter(|p|p["postKey"]==v["postKey"]&&in_account(p,scope)) {
            if v["visualEvidence"]["sourcePostVersion"]!=super::media_fullframes::source_version(post,scope){continue;}
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
    facts: MediaFacts,
    source_facts: Vec<MediaFacts>,
    source_identities: BTreeSet<String>,
}
struct MediaLookup {
    posts: BTreeMap<String, Vec<MediaFacts>>,
    conflicting_titles: BTreeSet<String>,
    source_versions: BTreeMap<String,BTreeSet<String>>,
}
impl MediaLookup {
    fn new(d:&Value,scope:&str)->Self {
        let mut posts: BTreeMap<String, Vec<MediaFacts>> = BTreeMap::new();
        let mut titles: BTreeMap<String, Vec<MediaFacts>> = BTreeMap::new();
        for post in &observed_video_posts(d,scope) {
            let facts = MediaFacts::new(post, scope);
            if facts.in_account && !facts.title.is_empty() { titles.entry(facts.title.clone()).or_default().push(facts.clone()); }
            posts.entry(text(post,"postKey").to_owned()).or_default().push(facts);
        }
        let conflicting_titles = titles.into_iter().filter_map(|(title, peers)| {
            peers.iter().enumerate().any(|(i,a)| peers.iter().skip(i+1).any(|b| identity_conflict(&a.identities,&b.identities))).then_some(title)
        }).collect();
        let mut source_versions:BTreeMap<String,BTreeSet<String>>=BTreeMap::new();
        for post in rows(d,"posts").iter().filter(|p|in_account(p,scope)){source_versions.entry(text(post,"postKey").to_owned()).or_default().insert(super::media_fullframes::source_version(post,scope));}
        Self{posts,conflicting_titles,source_versions}
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
            let matches:Vec<_>=identities.intersection(&target_facts.identities).cloned().collect();
            if !matches.is_empty(){bindings.push(json!({"postKey":target["postKey"],"sourcePostKey":source_key,"identities":matches}));}
            else if !identity_conflict(&target_facts.identities,&facts.identities)
                && !self.conflicting_titles.contains(&target_facts.title) && title_sharing_allowed(scope)
                && target_facts.in_account && target_facts.video && !target_facts.title.is_empty()
                && sources.iter().any(|source|source.in_account&&source.video&&source.title==target_facts.title&&!identity_conflict(&source.identities,&target_facts.identities)) {
                bindings.push(json!({"postKey":target["postKey"],"sourcePostKey":source_key,"identities":[],"match":"exact_normalized_title","normalizedTitle":target_facts.title,"authorization":"account_scoped_exact_title_reuse"}));
            }
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
            if !matches!(text(v,"kind"),"transcript"|"visual_context") || text(&v["scope"],"account") != scope || !in_account(v,scope) { continue; }
            let imported_scope=company_import::post_keys(d,v);
            if v.get("companyImport").is_some() && !rows(&v["companyImport"]["scope"],"postAliases").is_empty() && imported_scope.is_empty() { continue; }
            let facts = MediaFacts::new(v,scope);
            let source_facts = if text(v,"postKey").is_empty() { vec![] } else { media.posts.get(text(v,"postKey")).cloned().unwrap_or_default() };
            let mut source_identities = facts.identities.clone();
            for source in &source_facts { source_identities.extend(source.identities.iter().cloned()); }
            heads.push(TranscriptHead { value:(*v).clone(), scope:imported_scope, facts, source_facts, source_identities });
        }
        Ok(Self { account:scope.to_owned(), at:timestamp(at)?, media, heads })
    }
    pub(crate) fn has(&self, post: &Value) -> Result<bool, &'static str> {self.audio(post,false)}
    fn audio(&self, post: &Value, require_complete:bool) -> Result<bool, &'static str> {
        let key = text(post,"postKey");
        let fallback;
        let targets = if let Some(known) = self.media.posts.get(key) { known.as_slice() } else { fallback=vec![MediaFacts::new(post,&self.account)]; fallback.as_slice() };
        let mut covered=false;
        for head in &self.heads {
            if head.value["kind"]!="transcript" {continue;}
            let direct = head.scope.is_empty() || head.scope.contains(key);
            let shared = !direct && targets.iter().filter(|p|p.in_account).any(|target| {
                !head.source_identities.is_disjoint(&target.identities)
                    || (!identity_conflict(&target.identities,&head.facts.identities)
                        && !self.media.conflicting_titles.contains(&target.title)
                        && title_sharing_allowed(&self.account) && target.video && !target.title.is_empty()
                        && head.source_facts.iter().any(|source| source.in_account && source.video && source.title == target.title
                            && !identity_conflict(&source.identities,&target.identities)))
            });
            if !direct && !shared { continue; }
            let v=&head.value;
            let from=v["validFrom"].as_str().map(timestamp).transpose()?;
            let until=v["validUntil"].as_str().map(timestamp).transpose()?;
            if v["status"]=="active" && !text(v,"text").trim().is_empty()
                && (!require_complete||v["transcription"]["partial"]!=true)
                && !from.is_some_and(|t|t>self.at) && !until.is_some_and(|t|t<=self.at)
                && (v["trust"]=="verified" || (v["trust"]=="source_only" && !head.scope.is_empty())) { covered=true; }
        }
        Ok(covered)
    }
    pub(crate) fn has_visual(&self,post:&Value)->Result<bool,&'static str>{
        for head in &self.heads {
            let v=&head.value;
            if v["kind"]!="visual_context" || !visual_matches(v,post,&self.account,&self.media)
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

}

/// Owner-authorized title equivalence remains a labelled reuse policy, not a
/// claim that different platform bytes are identical. Observed conflicts win.
fn visual_matches(v:&Value,post:&Value,scope:&str,media:&MediaLookup)->bool {
    if !in_account(v,scope)||!in_account(post,scope)||media_conflict(v,post,scope){return false;}
    if !media.source_versions.get(text(v,"postKey")).is_some_and(|versions|versions.contains(text(&v["visualEvidence"],"sourcePostVersion"))){return false;}
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
    title_sharing_allowed(scope) && target.video && !target.title.is_empty()
        && !media.conflicting_titles.contains(&target.title)
        && media.posts.get(text(v,"postKey")).is_some_and(|sources|sources.iter().any(|source|
            source.in_account && source.video && source.title==target.title
                && !identity_conflict(&source.identities,&target.identities)))
}

fn transcript_group(d: &Value, material: &Value, provenance: &Value) -> String {
    let key = text(material,"postKey");
    let source = rows(d,"posts").iter().find(|p| rows(provenance,"mediaBinding").iter().any(|binding|binding["postKey"]==p["postKey"]))
        .or_else(|| rows(d,"posts").iter().find(|p| !key.is_empty() && text(p,"postKey") == key));
    if let Some(post)=source {
        if let Some(group)=media_group_key(post,account(d)) {
            let peers:Vec<_>=rows(d,"posts").iter().filter(|p|media_group_key(p,account(d)).as_ref()==Some(&group)).collect();
            let title_conflict=peers.iter().enumerate().any(|(i,p)|peers.iter().skip(i+1).any(|q|media_conflict(p,q,account(d))));
            let identities=media_identities(post,account(d));
            let representative=rows(d,"posts").iter().filter(|p|in_account(p,account(d))&&(
                !identities.is_disjoint(&media_identities(p,account(d)))||(!title_conflict&&same_media_title(post,p,account(d)).is_some())
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
        } else if !media_conflict(target,material,account(d)) {
            let title=media_title(target);
            let peers:Vec<_>=rows(d,"posts").iter().filter(|p|in_account(p,account(d))&&!title.is_empty()&&media_title(p)==title).collect();
            if peers.iter().enumerate().any(|(i,p)|peers.iter().skip(i+1).any(|q|media_conflict(p,q,account(d)))) { continue; }
            for source in rows(d,"posts").iter().filter(|p| !source_key.is_empty() && text(p,"postKey") == source_key) {
                if let Some(title) = same_media_title(target,source,account(d)) {
                    bindings.push(json!({"postKey":target["postKey"],"sourcePostKey":source_key,"identities":[],"match":"exact_normalized_title","normalizedTitle":title,"authorization":"account_scoped_exact_title_reuse"}));
                }
            }
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
        select_catalog(self,items,posts,at)
    }
}

pub fn sync_catalog(d: &mut Value, at: &str) -> Result<(), &'static str> {
    let manifest: Value = serde_json::from_str(include_str!("knowledge-import-manifest.json"))
        .map_err(|_| "Invalid knowledge import manifest")?;
    sync_with_manifest(d, at, &manifest)
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
fn select_catalog(catalog:&Catalog<'_>,items:&[Value],posts:&[Value],at:&str)->Result<Value,&'static str>{
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
        let media_binding = if legacy_scoped { imported_keys.iter().filter(|key|keys.contains(key.as_str())).map(|key|json!({"postKey":key,"match":"legacy_connector_scoped_alias","namespace":"commentops-fast.post-key"})).collect() }
            else if direct || v.get("companyImport").is_some() { vec![] } else { media_index.shared_binding(account(d),v,&target_facts) };
        if !direct && media_binding.is_empty() {
            continue;
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
        copy_media_identity(v, &mut material);
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
    let selected_versions:BTreeSet<String>=canonical.values().map(|m|text(m,"knowledgeVersionId").to_owned()).collect();
    materials.retain(|m|m["kind"]!="transcript"||selected_versions.contains(text(m,"knowledgeVersionId")));
    manifest.retain(|m|m["kind"]!="transcript"||selected_versions.contains(text(m,"versionId")));
    manifest.sort_by_key(|v| text(v, "entryId").to_owned());
    excluded.sort_by_key(|v| text(v, "entryId").to_owned());
    Ok(json!({"materials":materials,"manifest":manifest,"excluded":excluded,"policyVersion":1}))
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
    const AT: &str = "2026-09-22T12:00:00Z";
    #[test]
    fn visual_title_policy_is_labelled_and_rejects_hash_duration_and_account_conflicts(){
        for account in ["LikeAvto","BAW Russia"] {
            let mut d=json!({"account":account,"posts":[
                {"id":"a","postKey":"a","title":"Specific owner accepted title","attachments":[{"type":"video"}]},
                {"id":"b","postKey":"b","title":"Specific owner accepted title #tag","attachments":[{"type":"video"}]}
            ],"materials":[{"id":"v","postKey":"a","account":account,"kind":"visual_context","mediaSha256":"b".repeat(64),"text":"Visual samples",
                "visualEvidence":super::super::media_fullframes::fixture(account,"a")}]});
            d["materials"][0]["visualEvidence"]=super::super::media_fullframes::fixture_for_post(account,&d["posts"][0]);
            d["materials"][0]["mediaSha256"]=d["materials"][0]["visualEvidence"]["source"]["mediaSha256"].clone();
            sync_catalog(&mut d,AT).unwrap();
            assert!(TranscriptLookup::new(&d,AT).unwrap().has_visual(&d["posts"][1]).unwrap());
            let selected=select(&d,&[json!({"postKey":"b"})],&[],AT).unwrap();
            assert_eq!(selected["manifest"][0]["mediaBinding"][0]["match"],"exact_normalized_title");
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
    fn owner_title_reuse_is_exact_scoped_and_preserves_partial_provenance() {
        let mut d=json!({"account":"LikeAvto","posts":[
            {"postKey":"failed-yt","title":"  Бюджетный кроссовер — Changan Q05 #лайкавто #импорт","sourceUrl":"https://youtu.be/AbCdEf123_-"},
            {"postKey":"success-ig","title":"Публикация LikeAvto","text":"Бюджетный   кроссовер — Changan Q05\nDetails","sourceUrl":"https://instagram.com/reel/ABC/"}
        ],"materials":[{"id":"existing","postKey":"success-ig","kind":"transcript","text":"Existing words","transcription":{"partial":true,"maxAudioSeconds":900}}]});
        sync_catalog(&mut d,AT).unwrap();
        let s=select(&d,&[json!({"postKey":"failed-yt"})],&[],AT).unwrap();
        assert_eq!(rows(&s,"materials").len(),1);
        assert_eq!(s["manifest"][0]["mediaBinding"][0]["match"],"exact_normalized_title");
        assert_eq!(s["materials"][0]["postKey"],"success-ig");
        assert_eq!(s["materials"][0]["transcription"]["partial"],true);
        assert!(post_has_transcript(&d,&d["posts"][0],AT).unwrap());
        d["posts"][0]["title"]=json!("Бюджетный кроссовер — Changan Q06");
        assert!(!post_has_transcript(&d,&d["posts"][0],AT).unwrap());
        d["posts"][0]["title"]=json!("Бюджетный кроссовер — Changan Q05");
        d["posts"][1]["accountId"]=json!("Other");
        assert!(!post_has_transcript(&d,&d["posts"][0],AT).unwrap());
    }
    #[test]
    fn exact_title_reuse_is_available_inside_each_account_and_never_crosses_accounts() {
        for active in ["LikeAvto", "BAW Russia"] {
            let mut d=json!({"account":active,"posts":[
                {"postKey":"source","account":active,"title":"Exact shared title","sourceUrl":"https://youtu.be/AbCdEf123_-"},
                {"postKey":"target","account":active,"title":"Exact shared title #tag","sourceUrl":"https://instagram.com/reel/ABC/"}
            ],"materials":[{"id":"transcript","account":active,"postKey":"source","kind":"transcript","text":"Account-local words"}]});
            sync_catalog(&mut d,AT).unwrap();
            assert!(post_has_transcript(&d,&d["posts"][1],AT).unwrap(),"{active}");
            let selected=select(&d,&[json!({"postKey":"target"})],&[],AT).unwrap();
            assert_eq!(selected["manifest"][0]["mediaBinding"][0]["match"],"exact_normalized_title");

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
    fn title_reuse_rejects_generic_names_and_conflicting_explicit_ids() {
        for title in [""," ","None","Video by likeavto_import","Clip by @likeavto_import","Публикация LikeAvto"] {
            let a=json!({"title":title,"sourceUrl":"https://youtu.be/AbCdEf123_-"});
            let b=json!({"title":title,"sourceUrl":"https://instagram.com/reel/ABC/"});
            assert_eq!(same_media_title(&a,&b,"LikeAvto"),None);
        }
        let a=json!({"title":"Same full title","mediaSha256":"a".repeat(64)});
        let b=json!({"title":"same full title","contentSha256":"b".repeat(64)});
        assert!(media_conflict(&a,&b,"LikeAvto"));
        assert_eq!(same_media_title(&a,&b,"LikeAvto"),None);
        let b=json!({"title":"same full title","sourceUrl":"https://instagram.com/reel/ABC/"});
        assert_eq!(same_media_title(&a,&b,"Other"),None);
    }
    #[test]
    fn one_canonical_transcript_per_video_group_preserves_all_source_history() {
        let mut d=json!({"account":"LikeAvto","posts":[
            {"postKey":"yt","title":"Suzuki Jimny","sourceUrl":"https://youtu.be/AbCdEf123_-"},
            {"postKey":"ig","title":"Suzuki Jimny #tag","sourceUrl":"https://instagram.com/reel/Jimny/"},
            {"postKey":"vk","title":"Suzuki Jimny","attachments":[{"type":"video"}]},
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
