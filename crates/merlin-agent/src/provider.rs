//! The interface every model provider implements: stream one assistant reply for a conversation.

use std::sync::atomic::AtomicBool;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::Result;
use crate::conversation::Message;

/// A tool as the model sees it. `input_schema` sticks to the JSON Schema subset every provider
/// accepts (object, properties, required, types, enums, descriptions).
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ToolSpec {
    pub name: &'static str,
    pub description: &'static str,
    pub input_schema: Value,
}

pub struct ChatRequest<'a> {
    pub system: &'a str,
    pub tools: &'a [ToolSpec],
    pub messages: &'a [Message],
}

/// Incremental output, for showing the reply as it arrives.
#[derive(Clone, Debug, PartialEq)]
pub enum StreamEvent {
    Text(String),
    Thinking(String),
    /// The model started a tool call; its input is still streaming.
    ToolCallStarted {
        id: String,
        name: String,
    },
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Usage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_write_tokens: u64,
}

impl Usage {
    pub fn add(&mut self, o: Usage) {
        self.input_tokens = self.input_tokens.saturating_add(o.input_tokens);
        self.output_tokens = self.output_tokens.saturating_add(o.output_tokens);
        self.cache_read_tokens = self.cache_read_tokens.saturating_add(o.cache_read_tokens);
        self.cache_write_tokens = self.cache_write_tokens.saturating_add(o.cache_write_tokens);
    }

    /// Everything the model read this request (fresh, cached and newly cached input).
    pub fn total_input(&self) -> u64 {
        self.input_tokens.saturating_add(self.cache_read_tokens).saturating_add(self.cache_write_tokens)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StopReason {
    /// The reply is complete.
    EndTurn,
    /// The reply asks for tool calls.
    ToolUse,
    /// Cut off by the output limit.
    MaxTokens,
    /// A safety system declined (with the category when known).
    Refusal(Option<String>),
    Other(String),
}

/// One complete assistant reply.
#[derive(Clone, Debug, PartialEq)]
pub struct Reply {
    pub message: Message,
    pub stop: StopReason,
    pub usage: Usage,
}

pub trait LlmProvider: Send + Sync {
    /// Short name for errors ("anthropic").
    fn name(&self) -> &'static str;

    /// Stream one reply, reporting progress to `on_event`. Returns [`crate::Error::Cancelled`] soon
    /// after `cancel` is set.
    fn complete(&self, req: &ChatRequest, on_event: &mut dyn FnMut(StreamEvent), cancel: &AtomicBool) -> Result<Reply>;
}
