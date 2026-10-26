//! Provider adapters and the factory that picks one from [`AgentConfig`].

pub mod anthropic;
pub mod openai_compat;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use crate::config::{AgentConfig, ProviderKind};
use crate::provider::LlmProvider;
use crate::transport::Transport;
use crate::{Error, Result};

/// The provider for `cfg`, talking through `transport` with `key` (may be empty for local servers).
pub fn build(cfg: &AgentConfig, key: String, transport: Arc<dyn Transport>) -> Box<dyn LlmProvider> {
    match cfg.provider {
        ProviderKind::Anthropic => Box::new(anthropic::Anthropic::new(cfg, key, transport)),
        kind => Box::new(openai_compat::OpenAiCompat::new(kind, cfg, key, transport)),
    }
}

/// Run `attempt` with up to two retries for transient failures (rate limits, overload, 5xx, a broken
/// connection) that happen before any output streamed. `streamed` reports whether output arrived.
pub(crate) fn with_retries<T>(cancel: &AtomicBool, mut attempt: impl FnMut(&mut bool) -> Result<T>) -> Result<T> {
    let mut wait = Duration::from_secs(2);
    for n in 0..3 {
        let mut streamed = false;
        match attempt(&mut streamed) {
            Err(e) if n < 2 && !streamed && transient(&e) => {
                log::warn!("agent: retrying after {e}");
                // Sleep in slices so Stop stays responsive.
                let until = std::time::Instant::now() + wait;
                while std::time::Instant::now() < until {
                    if cancel.load(Ordering::Relaxed) {
                        return Err(Error::Cancelled);
                    }
                    std::thread::sleep(Duration::from_millis(100));
                }
                wait = wait.saturating_mul(3);
            }
            r => return r,
        }
    }
    Err(Error::Other("retries exhausted".into()))
}

fn transient(e: &Error) -> bool {
    match e {
        Error::Network(_) => true,
        Error::Api { status: Some(s), .. } => matches!(s, 408 | 409 | 429 | 500 | 502 | 503 | 504 | 529),
        // An in-stream error event with no status (Anthropic `overloaded_error`).
        Error::Api { status: None, message, .. } => message.to_ascii_lowercase().contains("overloaded"),
        _ => false,
    }
}

/// Parse a streamed tool input. Empty means no arguments; invalid JSON (a cut-off or malformed
/// stream) is kept as `{"_invalidInput": raw}` so the loop can tell the model instead of guessing.
pub(crate) fn parse_tool_input(raw: &str) -> serde_json::Value {
    let raw = raw.trim();
    if raw.is_empty() {
        return serde_json::json!({});
    }
    match serde_json::from_str::<serde_json::Value>(raw) {
        Ok(v @ serde_json::Value::Object(_)) => v,
        _ => serde_json::json!({ "_invalidInput": raw }),
    }
}
