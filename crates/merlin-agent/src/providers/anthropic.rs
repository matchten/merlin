//! Anthropic's Messages API (`POST /v1/messages`, streamed).
//!
//! * Adaptive thinking with summarized display (shown in the panel) and an explicit effort.
//! * Assistant turns are replayed exactly as received (thinking blocks included), which the API
//!   requires; after a mid-output refusal fallback, blocks other than text before the last
//!   `fallback` marker are dropped, as the API documents.
//! * Automatic prompt caching over the stable prefix (tools, system, earlier turns).
//! * Refusal fallbacks (`fallbacks: "default"`) on the models that support them.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use serde_json::{Value, json};

use super::{parse_tool_input, with_retries};
use crate::config::AgentConfig;
use crate::conversation::{Message, Native, Part, ResultPart, Role};
use crate::provider::{ChatRequest, LlmProvider, Reply, StopReason, StreamEvent, Usage};
use crate::sse::Event;
use crate::transport::Transport;
use crate::{Error, Result};

const NAME: &str = "anthropic";
pub const API_VERSION: &str = "2026-10-21";
const FALLBACK_BETA: &str = "server-side-fallback-2026-07-01";
const MAX_TOKENS: u32 = 64_000;

pub struct Anthropic {
    transport: Arc<dyn Transport>,
    key: String,
    url: String,
    model: String,
    effort: String,
}

impl Anthropic {
    pub fn new(cfg: &AgentConfig, key: String, transport: Arc<dyn Transport>) -> Anthropic {
        Anthropic { transport, key, url: format!("{}/messages", cfg.base_url()), model: cfg.model().to_string(), effort: cfg.effort.clone() }
    }

    /// The request body (public for tests).
    pub fn body(&self, req: &ChatRequest) -> Value {
        let tools: Vec<Value> = req
            .tools
            .iter()
            .map(|t| json!({"name": t.name, "description": t.description, "input_schema": t.input_schema, "eager_input_streaming": true}))
            .collect();
        let mut body = json!({
            "model": self.model,
            "max_tokens": MAX_TOKENS,
            "stream": true,
            "system": [{"type": "text", "text": req.system}],
            "tools": tools,
            "messages": req.messages.iter().map(to_wire).collect::<Vec<_>>(),
            // Caches everything up to the last block: each request in a turn extends the last.
            "cache_control": {"type": "ephemeral"},
        });
        if adaptive(&self.model) {
            body["thinking"] = json!({"type": "adaptive", "display": "summarized"});
            let effort = match self.effort.as_str() {
                e @ ("low" | "medium" | "high" | "xhigh" | "max") => e,
                _ => "high",
            };
            body["output_config"] = json!({"effort": effort});
        }
        if fallbacks(&self.model) {
            body["fallbacks"] = json!("default");
        }
        body
    }
}

/// Models that take `thinking: {type: "adaptive"}` and `output_config.effort`.
fn adaptive(model: &str) -> bool {
    let m = model.strip_prefix("claude-").unwrap_or(model);
    ["opus-5", "sonnet-5", "haiku-5", "fable-5", "mythos-5", "opus-4-6", "opus-4-7", "opus-4-8", "sonnet-4-6"].iter().any(|p| m.starts_with(p))
}

/// Models with server-side refusal fallbacks (the `"default"` form).
fn fallbacks(model: &str) -> bool {
    ["claude-opus-5-5", "claude-opus-5", "claude-fable-5-1", "claude-sonnet-5-5"].contains(&model)
}

/// A message as Messages API JSON: the native encoding when this provider produced it, else built
/// from the normalized parts.
fn to_wire(m: &Message) -> Value {
    let role = match m.role {
        Role::User => "user",
        Role::Assistant => "assistant",
    };
    if let Some(n) = m.native.as_ref().filter(|n| n.provider == NAME) {
        return json!({"role": role, "content": n.content});
    }
    let mut content = vec![];
    for p in &m.parts {
        match p {
            Part::Text { text } if !text.trim().is_empty() => content.push(json!({"type": "text", "text": text})),
            Part::Text { .. } | Part::Thinking { .. } => {}
            Part::Image { image, label } => {
                if let Some(l) = label {
                    content.push(json!({"type": "text", "text": format!("Attached image: {l}")}));
                }
                content.push(image_block(&image.media_type, &image.data));
            }
            Part::ToolCall { id, name, input } => {
                let input = if input.get("_invalidInput").is_some() { json!({}) } else { input.clone() };
                content.push(json!({"type": "tool_use", "id": id, "name": name, "input": input}));
            }
            Part::ToolResult { call_id, content: parts, is_error } => {
                let inner: Vec<Value> = parts
                    .iter()
                    .map(|r| match r {
                        ResultPart::Text { text } => json!({"type": "text", "text": if text.is_empty() { "(empty)" } else { text }}),
                        ResultPart::Image { image } => image_block(&image.media_type, &image.data),
                    })
                    .collect();
                content.push(json!({"type": "tool_result", "tool_use_id": call_id, "content": inner, "is_error": is_error}));
            }
        }
    }
    if content.is_empty() {
        content.push(json!({"type": "text", "text": "(no content)"}));
    }
    json!({"role": role, "content": content})
}

fn image_block(media_type: &str, data: &str) -> Value {
    json!({"type": "image", "source": {"type": "base64", "media_type": media_type, "data": data}})
}

/// Accumulates one streamed reply.
#[derive(Default)]
struct Acc {
    blocks: BTreeMap<u64, Value>,
    tool_json: BTreeMap<u64, String>,
    stop: Option<String>,
    refusal_category: Option<String>,
    usage: Usage,
    done: bool,
}

impl Acc {
    fn event(&mut self, ev: &Event, on_event: &mut dyn FnMut(StreamEvent)) -> Result<()> {
        if ev.event == "ping" {
            return Ok(());
        }
        let v: Value = serde_json::from_str(&ev.data).map_err(|e| Error::Protocol { provider: NAME, message: format!("{e} in `{}`", ev.event) })?;
        let index = v["index"].as_u64().unwrap_or(0);
        match v["type"].as_str().unwrap_or(ev.event.as_str()) {
            "message_start" => self.usage_from(&v["message"]["usage"]),
            "content_block_start" => {
                let block = v["content_block"].clone();
                if block["type"] == "tool_use" {
                    on_event(StreamEvent::ToolCallStarted { id: str_of(&block["id"]), name: str_of(&block["name"]) });
                    self.tool_json.insert(index, String::new());
                }
                self.blocks.insert(index, block);
            }
            "content_block_delta" => {
                let d = &v["delta"];
                let Some(block) = self.blocks.get_mut(&index) else {
                    return Err(Error::Protocol { provider: NAME, message: format!("delta for unknown block {index}") });
                };
                match d["type"].as_str().unwrap_or("") {
                    "text_delta" => {
                        let t = d["text"].as_str().unwrap_or("");
                        append(block, "text", t);
                        on_event(StreamEvent::Text(t.to_string()));
                    }
                    "thinking_delta" => {
                        let t = d["thinking"].as_str().unwrap_or("");
                        append(block, "thinking", t);
                        on_event(StreamEvent::Thinking(t.to_string()));
                    }
                    "signature_delta" => append(block, "signature", d["signature"].as_str().unwrap_or("")),
                    "input_json_delta" => {
                        if let Some(buf) = self.tool_json.get_mut(&index) {
                            let part = d["partial_json"].as_str().unwrap_or("");
                            if buf.len().saturating_add(part.len()) > crate::sse::MAX_EVENT_BYTES {
                                return Err(Error::Protocol { provider: NAME, message: "tool input too large".into() });
                            }
                            buf.push_str(part);
                        }
                    }
                    // Citations and future delta types: keep the block as started.
                    _ => {}
                }
            }
            "content_block_stop" => {
                if let (Some(raw), Some(block)) = (self.tool_json.get(&index), self.blocks.get_mut(&index)) {
                    block["input"] = parse_tool_input(raw);
                }
            }
            "message_delta" => {
                if let Some(s) = v["delta"]["stop_reason"].as_str() {
                    self.stop = Some(s.to_string());
                }
                if let Some(c) = v["delta"]["stop_details"]["category"].as_str().or(v["stop_details"]["category"].as_str()) {
                    self.refusal_category = Some(c.to_string());
                }
                if let Some(o) = v["usage"]["output_tokens"].as_u64() {
                    self.usage.output_tokens = o;
                }
                // A mid-stream fallback restates input usage here.
                if v["usage"]["input_tokens"].is_u64() {
                    self.usage_from(&v["usage"]);
                }
            }
            "message_stop" => self.done = true,
            "error" => {
                let msg = v["error"]["message"].as_str().unwrap_or("stream error").to_string();
                return Err(Error::Api { provider: NAME, status: None, message: msg });
            }
            _ => {}
        }
        Ok(())
    }

    fn usage_from(&mut self, u: &Value) {
        let n = |k: &str| u[k].as_u64().unwrap_or(0);
        self.usage.input_tokens = n("input_tokens");
        self.usage.cache_read_tokens = n("cache_read_input_tokens");
        self.usage.cache_write_tokens = n("cache_creation_input_tokens");
        if let Some(o) = u["output_tokens"].as_u64() {
            self.usage.output_tokens = o;
        }
    }

    fn finish(self) -> Result<Reply> {
        if !self.done {
            return Err(Error::Network("the reply stream ended early".into()));
        }
        let mut content: Vec<Value> = self.blocks.into_values().collect();
        // Mid-output fallback: before the last marker keep only text (the API's echo rule).
        if let Some(last) = content.iter().rposition(|b| b["type"] == "fallback") {
            let after = content.split_off(last.saturating_add(1));
            content.retain(|b| b["type"] == "text");
            content.extend(after);
        }
        let mut parts = vec![];
        for b in &content {
            match b["type"].as_str().unwrap_or("") {
                "text" => parts.push(Part::Text { text: str_of(&b["text"]) }),
                "thinking" => {
                    let t = str_of(&b["thinking"]);
                    if !t.is_empty() {
                        parts.push(Part::Thinking { text: t });
                    }
                }
                "tool_use" => parts.push(Part::ToolCall { id: str_of(&b["id"]), name: str_of(&b["name"]), input: b["input"].clone() }),
                _ => {}
            }
        }
        // Replay must be valid JSON input even when the stream's wasn't.
        for b in content.iter_mut().filter(|b| b["type"] == "tool_use") {
            if b["input"].get("_invalidInput").is_some() || !b["input"].is_object() {
                b["input"] = json!({});
            }
        }
        let stop = match self.stop.as_deref() {
            Some("end_turn") | Some("stop_sequence") => StopReason::EndTurn,
            Some("tool_use") => StopReason::ToolUse,
            Some("max_tokens") => StopReason::MaxTokens,
            Some("refusal") => StopReason::Refusal(self.refusal_category),
            Some(o) => StopReason::Other(o.to_string()),
            None => StopReason::Other("unknown".into()),
        };
        let message = Message { role: Role::Assistant, parts, native: Some(Native { provider: NAME.into(), content: Value::Array(content) }) };
        Ok(Reply { message, stop, usage: self.usage })
    }
}

fn str_of(v: &Value) -> String {
    v.as_str().unwrap_or("").to_string()
}

fn append(block: &mut Value, key: &str, s: &str) {
    if let Some(o) = block.as_object_mut() {
        let cur = o.entry(key.to_string()).or_insert_with(|| Value::String(String::new()));
        if let Some(c) = cur.as_str() {
            *cur = Value::String(format!("{c}{s}"));
        }
    }
}

impl LlmProvider for Anthropic {
    fn name(&self) -> &'static str {
        NAME
    }

    fn complete(&self, req: &ChatRequest, on_event: &mut dyn FnMut(StreamEvent), cancel: &AtomicBool) -> Result<Reply> {
        if self.key.trim().is_empty() {
            return Err(Error::NoKey("Anthropic", "ANTHROPIC_API_KEY"));
        }
        let body = serde_json::to_vec(&self.body(req)).map_err(|e| Error::Other(e.to_string()))?;
        let mut headers = vec![("x-api-key", self.key.clone()), ("anthropic-version", API_VERSION.to_string())];
        if fallbacks(&self.model) {
            headers.push(("anthropic-beta", FALLBACK_BETA.to_string()));
        }
        with_retries(cancel, |streamed| {
            let mut acc = Acc::default();
            self.transport.post_sse(
                NAME,
                &self.url,
                &headers,
                body.clone(),
                &mut |ev| {
                    *streamed = true;
                    acc.event(&ev, on_event)
                },
                cancel,
            )?;
            acc.finish()
        })
    }
}
