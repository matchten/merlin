//! One user turn: send the conversation, stream the reply, run the tools it calls, send their
//! results back, and repeat until the model answers without tools (or the turn is stopped).
//!
//! The conversation stays valid for the next turn whatever happens: every tool call gets a result
//! (an error result when the turn was stopped before it ran), and history is only appended to.

use std::sync::atomic::{AtomicBool, Ordering};

use serde_json::Value;

use crate::config::AgentConfig;
use crate::conversation::{Message, Part, ResultPart};
use crate::host::Host;
use crate::provider::{ChatRequest, LlmProvider, StopReason, StreamEvent, Usage};
use crate::{Error, SYSTEM_PROMPT, context, tools};

/// Progress for the panel.
#[derive(Clone, Debug, PartialEq)]
pub enum AgentEvent {
    Text(String),
    Thinking(String),
    /// A tool call began streaming (its input isn't complete yet).
    ToolStarted {
        id: String,
        name: String,
    },
    /// A tool call is about to run with this input.
    ToolRunning {
        id: String,
        name: String,
        input: Value,
    },
    ToolFinished {
        id: String,
        name: String,
        is_error: bool,
        summary: String,
        frames: Vec<Vec<u8>>,
    },
    /// Tokens used by one model request.
    Usage(Usage),
}

/// How a turn ended.
#[derive(Clone, Debug, PartialEq)]
pub enum TurnEnd {
    Done,
    Cancelled,
    /// The step limit was reached with work possibly left.
    MaxSteps,
    /// The provider's safety system declined.
    Refused(Option<String>),
    /// The reply was cut off by the output limit.
    Truncated,
    Error(String),
}

#[derive(Clone, Debug, PartialEq)]
pub struct TurnOutcome {
    pub end: TurnEnd,
    pub usage: Usage,
    /// Tool calls run this turn.
    pub tool_calls: usize,
}

/// Characters of a tool result shown in the panel's summary line.
const SUMMARY_CHARS: usize = 240;

/// Run one turn for the user message `user`, appending to `conversation`.
pub fn run_turn(
    provider: &dyn LlmProvider,
    host: &mut dyn Host,
    cfg: &AgentConfig,
    conversation: &mut Vec<Message>,
    user: Vec<Part>,
    on_event: &mut dyn FnMut(AgentEvent),
    cancel: &AtomicBool,
) -> TurnOutcome {
    let mut parts = user;
    parts.push(Part::Text { text: context::snapshot(host) });
    conversation.push(Message::user(parts));

    let specs = tools::specs();
    let mut usage = Usage::default();
    let mut tool_calls = 0usize;
    let end = 'turn: {
        for _ in 0..cfg.max_steps() {
            if cancel.load(Ordering::Relaxed) {
                break 'turn TurnEnd::Cancelled;
            }
            let req = ChatRequest { system: SYSTEM_PROMPT, tools: &specs, messages: conversation };
            let mut forward = |ev: StreamEvent| {
                on_event(match ev {
                    StreamEvent::Text(t) => AgentEvent::Text(t),
                    StreamEvent::Thinking(t) => AgentEvent::Thinking(t),
                    StreamEvent::ToolCallStarted { id, name } => AgentEvent::ToolStarted { id, name },
                })
            };
            let reply = match provider.complete(&req, &mut forward, cancel) {
                Ok(r) => r,
                Err(Error::Cancelled) => break 'turn TurnEnd::Cancelled,
                Err(e) => break 'turn TurnEnd::Error(e.to_string()),
            };
            usage.add(reply.usage);
            on_event(AgentEvent::Usage(reply.usage));
            let calls: Vec<(String, String, Value)> = reply.message.tool_calls().map(|(i, n, v)| (i.to_string(), n.to_string(), v.clone())).collect();
            conversation.push(reply.message);

            if calls.is_empty() {
                break 'turn match reply.stop {
                    StopReason::Refusal(c) => TurnEnd::Refused(c),
                    StopReason::MaxTokens => TurnEnd::Truncated,
                    _ => TurnEnd::Done,
                };
            }
            let mut results = vec![];
            let mut stopped = false;
            for (id, name, input) in calls {
                if stopped || cancel.load(Ordering::Relaxed) {
                    stopped = true;
                    results.push(Part::ToolResult {
                        call_id: id,
                        content: vec![ResultPart::Text { text: "Not run: the user stopped the turn.".into() }],
                        is_error: true,
                    });
                    continue;
                }
                on_event(AgentEvent::ToolRunning { id: id.clone(), name: name.clone(), input: input.clone() });
                let out = tools::run(host, &name, &input, cfg.frame_max_side());
                tool_calls = tool_calls.saturating_add(1);
                let summary: String = out.text().chars().take(SUMMARY_CHARS).collect();
                on_event(AgentEvent::ToolFinished { id: id.clone(), name, is_error: out.is_error, summary, frames: out.frames });
                results.push(Part::ToolResult { call_id: id, content: out.content, is_error: out.is_error });
            }
            conversation.push(Message::user(results));
            if stopped {
                break 'turn TurnEnd::Cancelled;
            }
        }
        TurnEnd::MaxSteps
    };
    TurnOutcome { end, usage, tool_calls }
}

/// The undo label for a turn: "Agent: " and the start of the request.
pub fn turn_label(request: &str) -> String {
    let first = request.lines().find(|l| !l.trim().is_empty()).unwrap_or("").trim();
    let mut short: String = first.chars().take(48).collect();
    if first.chars().count() > 48 {
        short.push('…');
    }
    if short.is_empty() { "Agent".into() } else { format!("Agent: {short}") }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels() {
        assert_eq!(turn_label("\n  make it bounce\nmore"), "Agent: make it bounce");
        assert_eq!(turn_label(""), "Agent");
        assert!(turn_label(&"x".repeat(100)).ends_with('…'));
    }
}
