//! Versioned provenance admission; legacy evidence never selects a future route.
use serde_json::Value;
pub(crate) const MODEL: &str = "gpt-6.1-sol";
pub(crate) const PROFILE: &str = "sol61_v1";
pub(crate) const CLI_SHA256: &str = "86e8ef1013f98df51fdeea446597f7e3ca32e454d1d4d8c0402a68b03c311d70";
pub(crate) fn preparation_route(value:&Value)->bool {
    value["reasoningEffort"]=="high" && (
        value["model"]=="gpt-6-astra" && value.get("modelProfile").is_none()
        || value["model"]==MODEL && value["modelProfile"]==PROFILE)
}
pub(crate) fn validate_profile(value:&Value)->Result<(),&'static str> {
    if value["model"]==MODEL && value["modelProfile"]!=PROFILE
        || value.get("modelProfile").is_some() && (value["model"]!=MODEL || value["modelProfile"]!=PROFILE) {
        return Err("Unsupported Codex model provenance profile");
    }
    if value["model"]==MODEL && value.get("cliSha256").is_some() && value["cliSha256"]!=CLI_SHA256 {
        return Err("Codex model runtime provenance differs from admitted CLI");
    }
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn legacy_and_versioned_provenance_are_distinct() {
        assert!(preparation_route(&json!({"model":"gpt-6-astra","reasoningEffort":"high"})));
        assert!(preparation_route(&json!({"model":MODEL,"modelProfile":PROFILE,"reasoningEffort":"high"})));
        for model in ["gpt-6-sol","gpt-6-luna",MODEL] {
            assert!(!preparation_route(&json!({"model":model,"reasoningEffort":"high"})));
        }
        assert!(!preparation_route(&json!({"model":"gpt-6-astra","modelProfile":PROFILE,"reasoningEffort":"high"})));
    }
}
