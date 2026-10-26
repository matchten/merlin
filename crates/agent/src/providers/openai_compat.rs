//! OpenAI-style Chat Completions (`POST {base}/chat/completions`, streamed): OpenAI itself, Gemini's
//! OpenAI-compatible endpoint, OpenRouter, Ollama, LM Studio and the like.
//!
//! * Tool results are `role: "tool"` messages (text only in this API); images a tool returns
//!   (rendered frames) follow in a user message right after them.
//! * Unknown fields on streamed tool calls (Gemini's `extra_content` thought signatures) are kept
//!   and replayed with the call to the same provider.
//! * `reasoning_content` / `reasoning` deltas (providers that expose reasoning) show as thinking.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use serde_json::{Map, Value, json};

use super::{parse_tool_input, with_retries};
use crate::config::{AgentConfig, ProviderKind};
use crate::conversation::{Message, Native, Part, ResultPart, Role};
use crate::provider::{ChatRequest, LlmProvider, Reply, StopReason, StreamEvent, Usage};
use crate::sse::Event;
use crate::transport::Transport;
use crate::{Error, Result};

const NAME: &str = "openai-compatible";

pub struct OpenAiCompat {
    kind: ProviderKind,
    transport: Arc<dyn Transport>,
    key: String,
    url: String,
    model: String,
}

impl OpenAiCompat {
    pub fn new(kind: ProviderKind, cfg: &AgentConfig, key: String, transport: Arc<dyn Transport>) -> OpenAiCompat {
        OpenAiCompat { kind, transport, key, url: format!("{}/chat/completions", cfg.base_url()), model: cfg.model().to_string() }
    }

    pub fn body(&self, req: &ChatRequest) -> Value {
        let mut messages = vec![json!({"role": "system", "content": req.system})];
        for m in req.messages {
            self.push_wire(m, &mut messages);
        }
        let tools: Vec<Value> = req
            .tools
            .iter()
            .map(|t| json!({"type": "function", "function": {"name": t.name, "description": t.description, "parameters": t.input_schema}}))
            .collect();
        json!({"model": self.model, "stream": true, "stream_options": {"include_usage": true}, "messages": messages, "tools": tools})
    }

    fn push_wire(&self, m: &Message, out: &mut Vec<Value>) {
        match m.role {
            Role::Assistant => {
                if let Some(n) = m.native.as_ref().filter(|n| n.provider == self.kind.id()) {
                    out.push(n.content.clone());
                    return;
                }
                let calls: Vec<Value> = m
                    .tool_calls()
                    .map(|(id, name, input)| json!({"id": id, "type": "function", "function": {"name": name, "arguments": arguments(input)}}))
                    .collect();
                out.push(assistant_json(&m.text(), calls));
            }
            Role::User => {
                // Tool results first: they must directly follow the assistant's tool calls.
                let mut images = vec![];
                for p in &m.parts {
                    if let Part::ToolResult { call_id, content, is_error } = p {
                        let mut text = String::new();
                        let mut n_images = 0;
                        for r in content {
                            match r {
                                ResultPart::Text { text: t } => {
                                    if !text.is_empty() {
                                        text.push('\n');
                                    }
                                    text.push_str(t);
                                }
                                ResultPart::Image { image } => {
                                    n_images += 1;
                                    images.push(json!({"type": "image_url", "image_url": {"url": image.data_url()}}));
                                }
                            }
                        }
                        if *is_error {
                            text = format!("Error: {text}");
                        }
                        if n_images > 0 {
                            text.push_str(&format!("\n[{n_images} image(s) from this call follow in the next message]"));
                        }
                        out.push(json!({"role": "tool", "tool_call_id": call_id, "content": text}));
                    }
                }
                let mut content = vec![];
                if !images.is_empty() {
                    content.push(json!({"type": "text", "text": "Images returned by the tool calls above:"}));
                    content.extend(images);
                }
                for p in &m.parts {
                    match p {
                        Part::Text { text } if !text.trim().is_empty() => content.push(json!({"type": "text", "text": text})),
                        Part::Image { image, label } => {
                            if let Some(l) = label {
                                content.push(json!({"type": "text", "text": format!("Attached image: {l}")}));
                            }
                            content.push(json!({"type": "image_url", "image_url": {"url": image.data_url()}}));
                        }
                        _ => {}
                    }
                }
                if !content.is_empty() {
                    out.push(json!({"role": "user", "content": content}));
                }
            }
        }
    }
}

fn arguments(input: &Value) -> String {
    match input.get("_invalidInput") {
        Some(_) => "{}".into(),
        None => input.to_string(),
    }
}

fn assistant_json(text: &str, calls: Vec<Value>) -> Value {
    let mut m = json!({"role": "assistant", "content": if text.is_empty() { Value::Null } else { json!(text) }});
    if !calls.is_empty() {
        m["tool_calls"] = json!(calls);
    }
    m
}

#[derive(Default)]
struct Call {
    id: String,
    name: String,
    arguments: String,
    /// Fields other than index/id/type/function (provider extensions).
    extra: Map<String, Value>,
    announced: bool,
}

#[derive(Default)]
struct Acc {
    text: String,
    thinking: String,
    calls: BTreeMap<u64, Call>,
    finish: Option<String>,
    usage: Usage,
    done: bool,
}

impl Acc {
    fn event(&mut self, ev: &Event, on_event: &mut dyn FnMut(StreamEvent)) -> Result<()> {
        let data = ev.data.trim();
        if data == "[DONE]" {
            self.done = true;
            return Ok(());
        }
        let v: Value = serde_json::from_str(data).map_err(|e| Error::Protocol { provider: NAME, message: e.to_string() })?;
        if let Some(err) = v.get("error").filter(|e| !e.is_null()) {
            let msg = err["message"].as_str().map(str::to_string).unwrap_or_else(|| err.to_string());
            return Err(Error::Api { provider: NAME, status: err["code"].as_u64().and_then(|c| u16::try_from(c).ok()), message: msg });
        }
        if let Some(u) = v.get("usage").filter(|u| u.is_object()) {
            let prompt = u["prompt_tokens"].as_u64().unwrap_or(0);
            let cached = u["prompt_tokens_details"]["cached_tokens"].as_u64().unwrap_or(0);
            self.usage = Usage {
                input_tokens: prompt.saturating_sub(cached),
                output_tokens: u["completion_tokens"].as_u64().unwrap_or(0),
                cache_read_tokens: cached,
                cache_write_tokens: 0,
            };
        }
        let Some(choice) = v["choices"].as_array().and_then(|c| c.first()) else { return Ok(()) };
        let d = &choice["delta"];
        if let Some(t) = d["content"].as_str().filter(|t| !t.is_empty()) {
            self.text.push_str(t);
            on_event(StreamEvent::Text(t.to_string()));
        }
        if let Some(t) = d["reasoning_content"].as_str().or(d["reasoning"].as_str()).filter(|t| !t.is_empty()) {
            self.thinking.push_str(t);
            on_event(StreamEvent::Thinking(t.to_string()));
        }
        for tc in d["tool_calls"].as_array().map(Vec::as_slice).unwrap_or_default() {
            let index = tc["index"].as_u64().unwrap_or(0);
            let call = self.calls.entry(index).or_default();
            if let Some(id) = tc["id"].as_str().filter(|s| !s.is_empty()) {
                call.id = id.to_string();
            }
            if let Some(n) = tc["function"]["name"].as_str() {
                call.name.push_str(n);
            }
            if let Some(a) = tc["function"]["arguments"].as_str() {
                if call.arguments.len().saturating_add(a.len()) > crate::sse::MAX_EVENT_BYTES {
                    return Err(Error::Protocol { provider: NAME, message: "tool input too large".into() });
                }
                call.arguments.push_str(a);
            }
            if let Some(o) = tc.as_object() {
                for (k, val) in o {
                    if !matches!(k.as_str(), "index" | "id" | "type" | "function") {
                        call.extra.insert(k.clone(), val.clone());
                    }
                }
            }
            if !call.announced && !call.name.is_empty() {
                call.announced = true;
                on_event(StreamEvent::ToolCallStarted { id: call.id.clone(), name: call.name.clone() });
            }
        }
        if let Some(f) = choice["finish_reason"].as_str() {
            self.finish = Some(f.to_string());
        }
        Ok(())
    }

    fn finish(self, kind: ProviderKind) -> Result<Reply> {
        // Some servers end without `[DONE]`; a finish reason is as good.
        if !self.done && self.finish.is_none() {
            return Err(Error::Network("the reply stream ended early".into()));
        }
        let mut parts = vec![];
        if !self.thinking.is_empty() {
            parts.push(Part::Thinking { text: self.thinking });
        }
        if !self.text.is_empty() {
            parts.push(Part::Text { text: self.text.clone() });
        }
        let mut wire_calls = vec![];
        for (i, c) in self.calls.into_values().enumerate() {
            // A server that omits ids still needs them to pair results with calls.
            let id = if c.id.is_empty() { format!("call_{i}") } else { c.id };
            let input = parse_tool_input(&c.arguments);
            let mut w = json!({"id": id, "type": "function", "function": {"name": c.name, "arguments": arguments(&input)}});
            if let Some(o) = w.as_object_mut() {
                o.extend(c.extra);
            }
            wire_calls.push(w);
            parts.push(Part::ToolCall { id, name: c.name, input });
        }
        let has_calls = !wire_calls.is_empty();
        let stop = match self.finish.as_deref() {
            _ if has_calls => StopReason::ToolUse,
            Some("stop") | None => StopReason::EndTurn,
            Some("length") => StopReason::MaxTokens,
            Some("content_filter") => StopReason::Refusal(None),
            Some(o) => StopReason::Other(o.to_string()),
        };
        let native = Native { provider: kind.id().into(), content: assistant_json(&self.text, wire_calls) };
        Ok(Reply { message: Message { role: Role::Assistant, parts, native: Some(native) }, stop, usage: self.usage })
    }
}

impl LlmProvider for OpenAiCompat {
    fn name(&self) -> &'static str {
        self.kind.id()
    }

    fn complete(&self, req: &ChatRequest, on_event: &mut dyn FnMut(StreamEvent), cancel: &AtomicBool) -> Result<Reply> {
        let key = self.key.trim();
        if key.is_empty() && self.kind.needs_key() {
            return Err(Error::NoKey(self.kind.label(), self.kind.env_var()));
        }
        let body = serde_json::to_vec(&self.body(req)).map_err(|e| Error::Other(e.to_string()))?;
        let headers: Vec<(&str, String)> = if key.is_empty() { vec![] } else { vec![("authorization", format!("Bearer {key}"))] };
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
            acc.finish(self.kind)
        })
    }
}
