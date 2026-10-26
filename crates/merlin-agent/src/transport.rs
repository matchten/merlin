//! HTTP for the providers: POST a JSON body and read the reply as server-sent events. Behind a trait
//! so tests replay recorded streams instead of calling the network.

use std::sync::atomic::{AtomicBool, Ordering};

use crate::sse::{Event, Parser};
use crate::{Error, Result};

/// Cap on an error body read into a message.
const MAX_ERROR_BODY: u64 = 64 << 10;

pub trait Transport: Send + Sync {
    /// POST `body` to `url` and pass each server-sent event to `on_event`. A non-2xx reply is
    /// [`Error::Api`] with the status and the provider's message. Stops with [`Error::Cancelled`]
    /// when `cancel` is set between events.
    fn post_sse(
        &self,
        provider: &'static str,
        url: &str,
        headers: &[(&str, String)],
        body: Vec<u8>,
        on_event: &mut dyn FnMut(Event) -> Result<()>,
        cancel: &AtomicBool,
    ) -> Result<()>;
}

/// The message inside a provider's JSON error body (`{"error": {"message": …}}` for Anthropic and
/// OpenAI-style APIs; Gemini wraps the same in a list), else the raw text.
pub fn error_message(body: &str) -> String {
    let v: serde_json::Value = serde_json::from_str(body).unwrap_or(serde_json::Value::Null);
    let err = if v.is_array() { &v[0]["error"] } else { &v["error"] };
    err["message"].as_str().or_else(|| err.as_str()).map(str::to_string).unwrap_or_else(|| body.trim().chars().take(500).collect())
}

/// Read a stream of lines into events, honouring `cancel` between lines.
pub fn pump(reader: &mut dyn std::io::BufRead, on_event: &mut dyn FnMut(Event) -> Result<()>, cancel: &AtomicBool, provider: &'static str) -> Result<()> {
    let mut parser = Parser::default();
    let mut line = String::new();
    loop {
        if cancel.load(Ordering::Relaxed) {
            return Err(Error::Cancelled);
        }
        line.clear();
        let n = reader.read_line(&mut line).map_err(|e| Error::Network(e.to_string()))?;
        if n == 0 {
            break;
        }
        let l = line.strip_suffix('\n').unwrap_or(&line);
        if let Some(ev) = parser.line(l).map_err(|message| Error::Protocol { provider, message })? {
            on_event(ev)?;
        }
    }
    if let Some(ev) = parser.finish() {
        on_event(ev)?;
    }
    Ok(())
}

/// Replays a recorded event stream (tests, offline examples) and records the request bodies sent.
#[derive(Default)]
pub struct Recorded {
    /// Stream text returned for each call, in order (the last one repeats).
    pub replies: Vec<String>,
    pub sent: std::sync::Mutex<Vec<(String, serde_json::Value)>>,
}

impl Transport for Recorded {
    fn post_sse(
        &self,
        provider: &'static str,
        url: &str,
        _headers: &[(&str, String)],
        body: Vec<u8>,
        on_event: &mut dyn FnMut(Event) -> Result<()>,
        cancel: &AtomicBool,
    ) -> Result<()> {
        let body: serde_json::Value = serde_json::from_slice(&body).map_err(|e| Error::Other(e.to_string()))?;
        let n = {
            let mut sent = self.sent.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            sent.push((url.to_string(), body));
            sent.len()
        };
        let reply = self.replies.get(n.saturating_sub(1)).or(self.replies.last()).cloned().unwrap_or_default();
        pump(&mut std::io::Cursor::new(reply.into_bytes()), on_event, cancel, provider)
    }
}

#[cfg(not(target_arch = "wasm32"))]
pub use http::Http;

#[cfg(not(target_arch = "wasm32"))]
mod http {
    use std::io::{BufReader, Read};
    use std::sync::atomic::AtomicBool;
    use std::time::Duration;

    use super::{MAX_ERROR_BODY, Transport, error_message, pump};
    use crate::sse::Event;
    use crate::{Error, Result};

    /// ureq over rustls with the OS certificate verifier (as craft-update's updater).
    pub struct Http {
        agent: ureq::Agent,
    }

    impl Default for Http {
        fn default() -> Self {
            let tls = ureq::tls::TlsConfig::builder().root_certs(ureq::tls::RootCerts::PlatformVerifier).build();
            let agent = ureq::Agent::config_builder()
                .tls_config(tls)
                .timeout_connect(Some(Duration::from_secs(20)))
                // Headers can wait on a declined attempt before a refusal fallback answers.
                .timeout_recv_response(Some(Duration::from_secs(300)))
                // A *total* budget for the stream (ureq has no idle timeout): long thinking replies
                // are normal, so this is only a backstop. Stop doesn't wait for it: the app drops a
                // stopped turn's thread rather than joining it.
                .timeout_recv_body(Some(Duration::from_secs(1800)))
                .http_status_as_error(false)
                .user_agent(concat!("Merlin/", env!("CARGO_PKG_VERSION")))
                .build()
                .new_agent();
            Http { agent }
        }
    }

    impl Transport for Http {
        fn post_sse(
            &self,
            provider: &'static str,
            url: &str,
            headers: &[(&str, String)],
            body: Vec<u8>,
            on_event: &mut dyn FnMut(Event) -> Result<()>,
            cancel: &AtomicBool,
        ) -> Result<()> {
            let mut req = self.agent.post(url).header("content-type", "application/json").header("accept", "text/event-stream");
            for (k, v) in headers {
                req = req.header(*k, v.as_str());
            }
            let mut resp = req.send(body).map_err(|e| Error::Network(e.to_string()))?;
            let status = resp.status().as_u16();
            if !(200..300).contains(&status) {
                let mut text = String::new();
                let _ = resp.body_mut().with_config().reader().take(MAX_ERROR_BODY).read_to_string(&mut text);
                return Err(Error::Api { provider, status: Some(status), message: error_message(&text) });
            }
            let mut reader = BufReader::new(resp.into_body().into_reader());
            pump(&mut reader, on_event, cancel, provider)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_messages() {
        assert_eq!(error_message(r#"{"type":"error","error":{"type":"overloaded_error","message":"Overloaded"}}"#), "Overloaded");
        assert_eq!(error_message(r#"[{"error":{"code":400,"message":"API key not valid"}}]"#), "API key not valid");
        assert_eq!(error_message("Bad Gateway"), "Bad Gateway");
    }

    #[test]
    fn recorded_stops_when_cancelled() {
        let t = Recorded { replies: vec!["data: 1\n\ndata: 2\n\n".into()], ..Default::default() };
        let cancel = AtomicBool::new(true);
        let r = t.post_sse("x", "u", &[], b"{}".to_vec(), &mut |_| Ok(()), &cancel);
        assert!(matches!(r, Err(Error::Cancelled)));
    }
}
