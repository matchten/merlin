//! One user turn: send the conversation, stream the reply, run the tools it calls, send their
//! results back, and repeat until the model answers without tools (or the turn is stopped).
//!
//! The conversation stays valid for the next turn whatever happens: every tool call gets a result
//! (an error result when the turn was stopped before it ran), and history is only appended to.

use std::sync::atomic::{AtomicBool, Ordering};

use serde_json::{Value, json};

use crate::config::AgentConfig;
use crate::conversation::{Image, Message, Part, ResultPart};
use crate::host::Host;
use crate::notes::{self, NoteBrief};
use crate::provider::{ChatRequest, LlmProvider, StopReason, StreamEvent, Usage};
use crate::tools::ToolOutput;
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
    /// A note's frame as it looked when the note was sent.
    NoteFrame {
        id: u64,
        png: Vec<u8>,
    },
    /// The agent reported a note done (`addressed`) or declined it, with its frame as it looks now.
    NoteResolved {
        id: u64,
        addressed: bool,
        summary: String,
        frame: Option<Vec<u8>>,
    },
}

/// What the panel adds to a turn besides the user's message.
#[derive(Clone, Debug, Default)]
pub struct TurnContext {
    /// The current time as the app shows it (see [`notes::time_label`]).
    pub time: Option<String>,
    /// The notes this turn applies (`send`) and the strip's others (so `resolve_note` knows them).
    pub notes: Vec<NoteBrief>,
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
    run_turn_with(provider, host, cfg, conversation, user, &TurnContext::default(), on_event, cancel)
}

/// [`run_turn`] with the panel's extra context: the displayed time and the notes being applied.
#[allow(clippy::too_many_arguments)]
pub fn run_turn_with(
    provider: &dyn LlmProvider,
    host: &mut dyn Host,
    cfg: &AgentConfig,
    conversation: &mut Vec<Message>,
    user: Vec<Part>,
    ctx: &TurnContext,
    on_event: &mut dyn FnMut(AgentEvent),
    cancel: &AtomicBool,
) -> TurnOutcome {
    let mut parts = user;
    parts.extend(note_parts(host, ctx, cfg.frame_max_side(), on_event));
    parts.push(Part::Text { text: context::snapshot_at(host, ctx.time.as_deref()) });
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
                let out = if name == tools::RESOLVE_NOTE {
                    resolve_note(host, ctx, &input, cfg.frame_max_side(), on_event)
                } else {
                    tools::run(host, &name, &input, cfg.frame_max_side())
                };
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

/// Each note being sent as its frame (both ends of a range, region outlined), then the `<notes>`
/// block.
fn note_parts(host: &mut dyn Host, ctx: &TurnContext, max_side: u32, on_event: &mut dyn FnMut(AgentEvent)) -> Vec<Part> {
    let sent: Vec<&NoteBrief> = ctx.notes.iter().filter(|b| b.send).collect();
    if sent.is_empty() {
        return vec![];
    }
    let mut parts = vec![];
    for b in sent {
        let (start, end) = b.note.span();
        let ends: &[(bool, _)] = if b.until.is_some() && end > start { &[(false, start), (true, end)] } else { &[(false, start)] };
        for &(is_end, t) in ends {
            let png = host.render(Some(&json!(b.note.comp)), Some(t.seconds()), max_side).and_then(|f| notes::outline(&f.png, b.comp_size, b.note.region));
            match png {
                Ok(png) => {
                    if !is_end {
                        on_event(AgentEvent::NoteFrame { id: b.note.id, png: png.clone() });
                    }
                    parts.push(Part::Image { image: Image::png(&png), label: Some(notes::frame_label(b, is_end)) });
                }
                Err(e) => parts.push(Part::Text { text: format!("({} could not be rendered: {e})", notes::frame_label(b, is_end)) }),
            }
        }
    }
    parts.push(Part::Text { text: notes::block(&ctx.notes) });
    parts
}

/// The `resolve_note` tool: tell the panel, and show the note's frame as it looks now.
fn resolve_note(host: &mut dyn Host, ctx: &TurnContext, input: &Value, max_side: u32, on_event: &mut dyn FnMut(AgentEvent)) -> ToolOutput {
    let id = input.get("id").and_then(|v| v.as_u64().or_else(|| v.as_str().and_then(|s| s.trim().trim_start_matches('#').parse().ok())));
    let Some(b) = id.and_then(|id| ctx.notes.iter().find(|b| b.note.id == id)) else {
        let known: Vec<String> = ctx.notes.iter().map(|b| b.note.id.to_string()).collect();
        return ToolOutput::error(format!(
            "no note with id {}; the notes are: {}",
            input.get("id").unwrap_or(&Value::Null),
            if known.is_empty() { "none".into() } else { known.join(", ") }
        ));
    };
    let addressed = input.get("status").and_then(Value::as_str) != Some("wont_fix");
    let summary = input.get("summary").and_then(Value::as_str).unwrap_or_default().trim().to_string();
    let frame = host.render(Some(&json!(b.note.comp)), Some(b.note.start.seconds()), max_side).ok().map(|f| f.png);
    on_event(AgentEvent::NoteResolved { id: b.note.id, addressed, summary, frame: frame.clone() });
    let text = format!("Note {} marked {}.", b.note.id, if addressed { "addressed" } else { "won't fix" });
    ToolOutput { content: vec![ResultPart::Text { text }], is_error: false, frames: frame.into_iter().collect() }
}

/// The undo label for a turn: "Merlin: " and the start of the request.
pub fn turn_label(request: &str) -> String {
    let first = request.lines().find(|l| !l.trim().is_empty()).unwrap_or("").trim();
    let mut short: String = first.chars().take(48).collect();
    if first.chars().count() > 48 {
        short.push('…');
    }
    if short.is_empty() { "Merlin".into() } else { format!("Merlin: {short}") }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels() {
        assert_eq!(turn_label("\n  make it bounce\nmore"), "Merlin: make it bounce");
        assert_eq!(turn_label(""), "Merlin");
        assert!(turn_label(&"x".repeat(100)).ends_with('…'));
    }
}
