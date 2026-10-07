//! Closed download diagnostics and recovery dispositions. These are observations,
//! not authority to install tools, use credentials, or bypass provider controls.
use serde_json::{Value, json};

// Only normalized timing survives the private diagnostic buffer. This is a
// downloader-reported header, not proof that yt-dlp exposes every provider's
// headers, and does not authorize an automatic retry or reset its deadline.
#[derive(Clone, Debug)]
pub(super) struct RetryAfter {
    observed_at: chrono::DateTime<chrono::Utc>,
    not_before: chrono::DateTime<chrono::Utc>,
}
impl RetryAfter {
    pub(super) fn metadata(&self) -> Value {
        json!({"source":"downloader_diagnostic_header",
            "observedAtUtc":self.observed_at.to_rfc3339_opts(chrono::SecondsFormat::Secs,true),
            "notBeforeUtc":self.not_before.to_rfc3339_opts(chrono::SecondsFormat::Secs,true),
            "delaySeconds":(self.not_before-self.observed_at).num_seconds()})
    }
}
pub(super) fn retry_after(stderr: &[u8], observed_at: chrono::DateTime<chrono::Utc>) -> Option<RetryAfter> {
    if classify(stderr)!="source_download_failed_rate_limited" {return None;}
    // Use whole seconds so persisted delay and absolute deadline agree exactly.
    let observed_at=chrono::DateTime::from_timestamp(observed_at.timestamp(),0)?;
    let mut not_before=None;
    for line in String::from_utf8_lossy(stderr).lines() {
        let Some((name,value))=line.trim().split_once(':') else {continue;};
        if !name.eq_ignore_ascii_case("retry-after") {continue;}
        let value=value.trim();
        let candidate=if !value.is_empty()&&value.bytes().all(|byte|byte.is_ascii_digit()) {
            value.parse::<i64>().ok().and_then(chrono::TimeDelta::try_seconds)
                .and_then(|delay|observed_at.checked_add_signed(delay))
        } else if value.len()==29&&value.ends_with(" GMT") {
            chrono::DateTime::parse_from_rfc2822(value).ok().map(|date|date.with_timezone(&chrono::Utc))
        } else {None};
        if let Some(candidate)=candidate {
            let candidate=candidate.max(observed_at);
            // Conflicting reported cooldowns must not shorten an existing hold.
            not_before=Some(not_before.map_or(candidate,|previous:chrono::DateTime<chrono::Utc>|previous.max(candidate)));
        }
    }
    not_before.map(|not_before|RetryAfter{observed_at,not_before})
}

pub(super) fn classify(stderr: &[u8]) -> &'static str {
    let text = String::from_utf8_lossy(stderr).to_ascii_lowercase();
    let has = |needles: &[&str]| needles.iter().any(|needle| text.contains(needle));
    // Specific access and local failures outrank generic HTTP/network symptoms.
    // A 403 alone says nothing about expiry, authentication, or an anti-bot gate.
    if has(&["no space left on device", "disk full", "not enough space on the disk", "errno 28"]) { "source_download_failed_disk_full" }
    else if has(&["permission denied", "access is denied", "read-only file system", "winerror 5"]) { "source_download_failed_permission" }
    else if has(&["http error 429", "too many requests", "rate limit"]) { "source_download_failed_rate_limited" }
    else if has(&["no supported javascript runtime", "javascript runtime is not available", "n challenge solving failed"]) { "source_download_failed_js_runtime" }
    else if has(&["not a bot", "captcha", "cloudflare challenge", "security challenge", "challenge solving failed"]) { "source_download_failed_challenge" }
    else if has(&["private video", "this video is private", "private account"]) { "source_download_failed_private" }
    else if has(&["not available in your country", "not available from your location", "geo-restricted", "geographic restriction"]) { "source_download_failed_geoblocked" }
    else if has(&["sign in", "login required", "log in", "authentication required", "cookies are required"]) { "source_download_failed_auth" }
    else if has(&["url has expired", "url is expired", "signature has expired", "request has expired", "expiredtoken"]) { "source_download_failed_url_expired" }
    else if has(&["certificate verify failed", "certificate_verify_failed", "sslcertverificationerror", "ssl: certificate"]) { "source_download_failed_tls" }
    else if has(&["no module named", "ffmpeg not found", "ffmpeg is not installed", "ffprobe not found"]) { "source_download_failed_dependency" }
    else if has(&["requested format is not available", "requested format not available", "no video formats found"]) { "source_download_failed_format_unavailable" }
    else if has(&["unsupported url"]) { "source_download_failed_unsupported_url" }
    else if has(&["http error 403", "403 forbidden"]) { "source_download_failed_http_forbidden" }
    else if has(&["video unavailable", "video is unavailable", "has been removed", "video does not exist", "video has been deleted"]) { "source_download_failed_unavailable" }
    else if has(&["timed out", "timeout", "name resolution", "getaddrinfo failed", "connection refused", "connection reset", "network is unreachable", "temporary failure in name resolution", "http error 500", "http error 502", "http error 503", "http error 504"]) { "source_download_failed_network" }
    else if has(&["no impersonate target is available", "impersonation target is not available"]) { "source_download_failed_impersonation" }
    else if has(&["unable to extract", "extractor error", "unexpected response from webpage request"]) { "source_download_failed_extractor" }
    else if has(&["did not get any data blocks", "content too short", "checksum mismatch", "invalid data found when processing input", "unable to download video fragments", "fragment not found"]) { "source_download_failed_integrity" }
    else { "source_download_failed" }
}

pub(super) fn transient(code: &str) -> bool { code == "source_download_failed_network" }
pub(super) fn alternate_locator(code: &str) -> bool {
    matches!(code, "source_download_failed_format_unavailable" | "source_download_failed_unsupported_url"
        | "source_download_failed_http_forbidden" | "source_download_failed_unavailable"
        | "source_download_failed_url_expired" | "source_download_failed_extractor")
}
pub(super) fn known(code: &str) -> bool {
    matches!(code, "source_download_failed" | "source_download_failed_rate_limited" | "source_download_failed_challenge"
        | "source_download_failed_private" | "source_download_failed_geoblocked" | "source_download_failed_auth"
        | "source_download_failed_url_expired" | "source_download_failed_tls" | "source_download_failed_dependency"
        | "source_download_failed_impersonation" | "source_download_failed_js_runtime" | "source_download_failed_format_unavailable"
        | "source_download_failed_unsupported_url" | "source_download_failed_http_forbidden" | "source_download_failed_unavailable"
        | "source_download_failed_network" | "source_download_failed_extractor" | "source_download_failed_disk_full"
        | "source_download_failed_permission" | "source_download_failed_integrity" | "source_download_failed_timeout"
        | "source_download_failed_spawn_failed" | "source_download_failed_wait_failed" | "source_download_failed_unknown"
        | "source_download_failed_process_unknown" | "source_file_missing" | "source_file_too_large")
}
pub(super) fn policy(code: &str) -> Value {
    let disposition = match code {
        "completed" => "complete",
        "source_download_failed_network" => "bounded_transient_retry",
        "source_download_failed_rate_limited" => "wait_for_provider_cooldown",
        "source_download_failed_auth" | "source_download_failed_challenge" | "source_download_failed_private" => "operator_access_review",
        "source_download_failed_geoblocked" | "source_download_failed_unavailable" => "source_availability_review",
        "source_download_failed_url_expired" => "refresh_bound_locator",
        "source_download_failed_dependency" | "source_download_failed_impersonation" | "source_download_failed_js_runtime" => "repair_pinned_runtime",
        "source_download_failed_tls" => "review_tls_configuration",
        "source_download_failed_disk_full" | "source_download_failed_permission" => "repair_local_storage",
        "source_download_failed_format_unavailable" | "source_download_failed_unsupported_url" | "source_download_failed_extractor" => "review_format_or_extractor",
        "source_download_failed_integrity" | "source_file_missing" | "source_file_too_large" => "review_source_integrity",
        "source_download_failed_http_forbidden" => "review_unspecified_forbidden",
        "source_download_failed_timeout" => "deadline_exhausted",
        "source_download_failed_process_unknown" | "source_download_failed_wait_failed" => "reconcile_process_before_retry",
        "source_download_failed_spawn_failed" => "repair_pinned_runtime",
        _ => "inspect_unknown_failure",
    };
    json!({"schemaVersion":1,"disposition":disposition,"automaticRetryEligible":transient(code),
        "automaticRetryLimit":if transient(code){1}else{0},"requiresConfirmedChildCessation":true,"requiresConfirmedProcessTreeCessation":true,
        "alternateBoundLocatorEligible":alternate_locator(code)})
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn retry_after_headers_preserve_safe_timing_without_guessing_from_error_text() {
        let now=chrono::DateTime::parse_from_rfc3339("2026-09-27T12:00:00Z").unwrap().with_timezone(&chrono::Utc);
        for (header,seconds) in [("Retry-After: 120",120),("rEtRy-AfTeR: 0",0),
            ("Retry-After: Sun, 27 Sep 2026 12:02:00 GMT",120),("Retry-After: Sun, 27 Sep 2026 11:00:00 GMT",0),
            ("Retry-After: 120\nRetry-After: 30",120)] {
            let fixture=format!("HTTP Error 429: Too Many Requests\n{header}\nhttps://private.invalid/?token=SECRET Cookie: PRIVATE_COOKIE");
            let metadata=retry_after(fixture.as_bytes(),now).unwrap().metadata();
            assert_eq!(metadata["delaySeconds"],seconds,"{header}");
            let until=chrono::DateTime::parse_from_rfc3339(metadata["notBeforeUtc"].as_str().unwrap()).unwrap().with_timezone(&chrono::Utc);
            assert_eq!((until-now).num_seconds(),seconds);
            let rendered=metadata.to_string();assert!(!rendered.contains("SECRET")&&!rendered.contains("PRIVATE_COOKIE")&&!rendered.contains("https:"));
        }
        for header in ["", "Retry-After: -1", "Retry-After: 1.5", "Retry-After: 999999999999999999999999",
            "Retry-After: 9223372036854775807", "Retry-After: tomorrow", "Retry-After: 120 SECRET",
            "ERROR: Retry-After: 120", "https://private.invalid/Retry-After: 120", "Retry-After: Sun, 27 Sep 2026 12:02:00 +0000"] {
            assert!(retry_after(format!("HTTP Error 429\n{header}").as_bytes(),now).is_none(),"{header}");
        }
        assert!(retry_after(b"HTTP Error 403\nRetry-After: 120",now).is_none());
    }
    #[test]
    fn hostile_stderr_fixtures_have_closed_categories_and_recovery() {
        for (message, suffix) in [
            ("The extractor is attempting impersonation, but no impersonate target is available\nERROR: Unexpected response from webpage request", "impersonation"),
            ("ERROR: No module named 'curl_cffi'", "dependency"),
            ("ERROR: [SSL: CERTIFICATE_VERIFY_FAILED] certificate verify failed", "tls"),
            ("ERROR: TLS handshake timed out", "network"),
            ("ERROR: HTTP Error 403: Forbidden", "http_forbidden"),
            ("ERROR: HTTP Error 403: Signature has expired", "url_expired"),
            ("ERROR: HTTP Error 429: Too Many Requests", "rate_limited"),
            ("ERROR: Sign in to confirm you're not a bot", "challenge"),
            ("ERROR: Sign in to confirm your age", "auth"),
            ("ERROR: Private video. Sign in if you've been granted access", "private"),
            ("ERROR: This video is not available in your country", "geoblocked"),
            ("ERROR: Video unavailable. This video has been removed", "unavailable"),
            ("ERROR: Unable to extract initial state", "extractor"),
            ("ERROR: Requested format is not available", "format_unavailable"),
            ("ERROR: [Errno 28] No space left on device", "disk_full"),
            ("ERROR: [WinError 5] Access is denied", "permission"),
            ("ERROR: Content too short", "integrity"),
            ("ERROR: getaddrinfo failed", "network"),
        ] {
            let diagnostic = format!("{message}\nhttps://private.invalid/?token=SECRET Cookie: PRIVATE_COOKIE");
            let code = classify(diagnostic.as_bytes());
            assert_eq!(code, format!("source_download_failed_{suffix}"), "{message}");
            assert!(known(code));
            let safe = policy(code).to_string();
            assert!(!safe.contains("SECRET") && !safe.contains("PRIVATE_COOKIE") && !safe.contains("https:"));
            assert_eq!(policy(code)["automaticRetryEligible"], suffix == "network");
        }
        // A warning must not override an explicit access rejection.
        assert_eq!(classify(b"no impersonate target is available\nHTTP Error 403: Forbidden"), "source_download_failed_http_forbidden");
        for code in ["source_download_failed", "source_download_failed_unknown", "source_download_failed_timeout", "source_download_failed_process_unknown", "source_download_failed_wait_failed"] {
            assert!(!transient(code)); assert!(!alternate_locator(code));
        }
        for bytes in [b"\xff\xfeSECRET".as_slice(), b"", b"SECRET unexplained 403"] { assert_eq!(classify(bytes), "source_download_failed"); }
    }
}
