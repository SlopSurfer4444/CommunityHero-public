//! Numeric timing only: no prompts, credentials, SQL or entity payloads.
//! Slow stages remain observable in normal operation; opt-in tracing includes all.
use std::{sync::OnceLock, time::Instant};

fn enabled() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| std::env::var("COMMUNITYHERO_PERF_TRACE").as_deref() == Ok("1"))
}

pub(crate) struct Span {
    stage: &'static str,
    start: Instant,
}
impl Span {
    pub(crate) fn new(stage: &'static str) -> Self {
        Self { stage, start: Instant::now() }
    }
}
impl Drop for Span {
    fn drop(&mut self) {
        let elapsed_ms = self.start.elapsed().as_secs_f64()*1000.0;
        if enabled() || elapsed_ms >= 250.0 {
            eprintln!("performance {}", serde_json::json!({"stage":self.stage,"elapsedMs":elapsed_ms}));
        }
    }
}
