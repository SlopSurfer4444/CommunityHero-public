//! Public navigation is a deployment allow-list, never a database selector.
use crate::*;
use std::collections::HashSet;

#[derive(Clone)]
pub(crate) struct Navigation {
    pub(crate) base_path: String,
    entries: Vec<Entry>,
}

#[derive(Clone)]
struct Entry { id: String, label: String, url: String }

pub(crate) fn valid_base_path(path: &str) -> bool {
    if path == "/" { return true; }
    let Some(slug) = path.strip_prefix('/').and_then(|v| v.strip_suffix('/')) else { return false; };
    !slug.is_empty() && slug.len() <= 64
        && slug.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        && slug.as_bytes()[0].is_ascii_alphanumeric()
        && slug.as_bytes()[slug.len()-1].is_ascii_alphanumeric()
}

impl Navigation {
    pub(crate) fn root() -> Self { Self { base_path: "/".into(), entries: Vec::new() } }

    pub(crate) fn load(profile: accounts::Profile, origin: Option<&str>) -> Result<Self, String> {
        let base = match std::env::var("COMMUNITYHERO_BASE_PATH") {
            Ok(value) => value,
            Err(std::env::VarError::NotPresent) => "/".into(),
            Err(_) => return Err("Invalid account base path".into()),
        };
        let entries = match std::env::var("COMMUNITYHERO_ACCOUNTS_JSON") {
            Ok(value) => value,
            Err(std::env::VarError::NotPresent) => "[]".into(),
            Err(_) => return Err("Invalid account navigation configuration".into()),
        };
        Self::parse(&base, &entries, profile.key(), origin)
    }

    pub(crate) fn parse(base: &str, raw: &str, current: &str, origin: Option<&str>) -> Result<Self, String> {
        let invalid = || "Invalid account navigation configuration".to_string();
        if !valid_base_path(base) || raw.len() > 32_768 { return Err(invalid()); }
        let value: Value = serde_json::from_str(raw).map_err(|_| invalid())?;
        let mut entries: Vec<Entry> = value.as_array().ok_or_else(invalid)?.iter().map(|entry| {
            let object=entry.as_object().ok_or_else(invalid)?;
            if object.len()!=3 { return Err(invalid()); }
            Ok(Entry { id:entry["id"].as_str().ok_or_else(invalid)?.into(),
                label:entry["label"].as_str().ok_or_else(invalid)?.into(),
                url:entry["url"].as_str().ok_or_else(invalid)?.into() })
        }).collect::<Result<_,String>>()?;
        if entries.len() > 32 { return Err(invalid()); }
        let mut ids = HashSet::new();
        let mut paths = HashSet::new();
        let mut current_present = false;
        for entry in &mut entries {
            if entry.id.is_empty() || entry.id.len() > 64
                || !entry.id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
                || entry.label.trim().is_empty() || entry.label.len() > 128
                || entry.label.chars().any(char::is_control) || !ids.insert(entry.id.clone()) {
                return Err(invalid());
            }
            // A configured public list requires a verified HTTPS origin. URLs
            // stay on that origin and address exactly one canonical account path.
            let origin = origin.ok_or_else(invalid)?;
            let path = if entry.url.starts_with('/') { entry.url.as_str() }
                else { entry.url.strip_prefix(origin).ok_or_else(invalid)? };
            if !valid_base_path(path) || !paths.insert(path.to_owned()) { return Err(invalid()); }
            if entry.id == current {
                if path != base { return Err(invalid()); }
                current_present = true;
            }
            entry.url = format!("{origin}{path}");
        }
        if !entries.is_empty() && !current_present { return Err(invalid()); }
        Ok(Self { base_path: base.into(), entries })
    }

    fn public_json(&self, profile: accounts::Profile) -> Value {
        json!({"account":profile.display(),"basePath":self.base_path,
            "accounts":self.entries.iter().map(|entry|json!({"id":entry.id,"label":entry.label,"url":entry.url})).collect::<Vec<_>>()})
    }
}

pub(crate) async fn get(State(app): State<App>) -> Json<Value> {
    Json(app.navigation.public_json(app.account))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn configured_navigation_is_generic_same_origin_and_identity_pinned() {
        let raw = r#"[{"id":"third-company","label":"Third Company","url":"/third/"},{"id":"likeavto","label":"LikeAvto","url":"https://communityhero.ru/likeavto/"}]"#;
        let config = Navigation::parse("/likeavto/",raw,"likeavto",Some("https://communityhero.ru")).unwrap();
        let result = config.public_json(accounts::Profile::LikeAvto);
        assert_eq!(result["account"],"LikeAvto");
        assert_eq!(result["basePath"],"/likeavto/");
        assert_eq!(result["accounts"][0]["url"],"https://communityhero.ru/third/");
        assert_eq!(Navigation::root().public_json(accounts::Profile::BawRussia)["accounts"],json!([]));
    }
    #[test]
    fn unsafe_or_ambiguous_navigation_fails_closed() {
        for base in ["/baw","//baw/","/../","/baw/x/","/BAW/","/baw%2f/","/baw/\r\n"] {
            assert!(Navigation::parse(base,"[]","baw-russia",None).is_err(),"{base}");
        }
        for url in ["https://evil.test/baw/","https://communityhero.ru.evil.test/baw/","//evil.test/","/baw/?x=1","/baw/#x","/baw/%2e/","/baw/../","javascript:alert(1)"] {
            let raw=json!([{"id":"baw-russia","label":"BAW","url":url}]).to_string();
            assert!(Navigation::parse("/baw/",&raw,"baw-russia",Some("https://communityhero.ru")).is_err(),"{url}");
        }
        for raw in [
            json!([{"id":"foreign","label":"Other","url":"/baw/"}]),
            json!([{"id":"baw-russia","label":"BAW","url":"/other/"}]),
            json!([{"id":"baw-russia","label":"BAW","url":"/baw/"},{"id":"baw-russia","label":"Other","url":"/other/"}]),
            json!([{"id":"baw-russia","label":"BAW","url":"/baw/"},{"id":"other","label":"Other","url":"/baw/"}]),
        ] { assert!(Navigation::parse("/baw/",&raw.to_string(),"baw-russia",Some("https://communityhero.ru")).is_err()); }
    }
}
