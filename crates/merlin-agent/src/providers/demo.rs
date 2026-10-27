//! A scripted provider for trying Merlin without a network or a key (and for UI tests). Each user
//! request plays the same three-step turn a real model would: say what it will do and run a script
//! that builds a drop-in title from the request's words, render frames to check it, then sum up.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use serde_json::json;

use crate::conversation::{Message, Part, Role};
use crate::provider::{ChatRequest, LlmProvider, Reply, StopReason, StreamEvent, Usage};
use crate::{Error, Result};

pub struct Demo {
    /// Pause between streamed chunks, so the panel's live updates are visible.
    pub chunk_delay: Duration,
}

impl Default for Demo {
    fn default() -> Self {
        Demo { chunk_delay: Duration::from_millis(25) }
    }
}

/// The file names of the images attached to message `at` (their labels start with the name).
fn attached(messages: &[Message], at: usize) -> Vec<String> {
    let Some(m) = messages.get(at) else { return vec![] };
    m.parts
        .iter()
        .filter_map(|p| match p {
            Part::Image { label, .. } => Some(label.as_deref().and_then(|l| l.split(" (").next()).unwrap_or("image").to_string()),
            _ => None,
        })
        .collect()
}

/// The text of the latest user request (not tool results, not the editor context).
fn request(messages: &[Message]) -> (usize, String) {
    for (i, m) in messages.iter().enumerate().rev() {
        if m.role != Role::User {
            continue;
        }
        let text: Vec<&str> = m
            .parts
            .iter()
            .filter_map(|p| match p {
                Part::Text { text } if !text.starts_with("<editor_context>") => Some(text.as_str()),
                _ => None,
            })
            .collect();
        if !text.is_empty() {
            return (i, text.join(" "));
        }
    }
    (0, String::new())
}

/// The demo's build script for a title saying `words`.
fn script(words: &str) -> String {
    let words = serde_json::to_string(words).unwrap_or_else(|_| "\"Merlin\"".into());
    format!(
        r#"var comp = app.project.activeItem;
if (!comp) {{ comp = app.project.items.addComp("Merlin Demo", 1920, 1080, 1, 5, 30); comp.openInViewer(); }}
var bg = comp.layers.addSolid([0.06, 0.07, 0.12], "Demo Background", comp.width, comp.height, 1);
bg.moveToEnd();
var t = comp.layers.addText({words});
t.name = "Demo Title";
var doc = t.property("ADBE Text Properties").property("ADBE Text Document").value;
doc.fontSize = Math.round(comp.height / 9); doc.fillColor = [1, 1, 1];
t.property("ADBE Text Properties").property("ADBE Text Document").setValue(doc);
var pos = t.property("ADBE Transform Group").property("ADBE Position");
var cx = comp.width / 2, cy = comp.height / 2;
pos.setValueAtTime(0, [cx, -comp.height * 0.2]);
pos.setValueAtTime(0.6, [cx, cy + comp.height * 0.04]);
pos.setValueAtTime(0.8, [cx, cy - comp.height * 0.015]);
pos.setValueAtTime(1.0, [cx, cy]);
var ease = new KeyframeEase(0, 60);
for (var k = 2; k <= pos.numKeys; k++) pos.setTemporalEaseAtKey(k, [ease], [ease]);
"built " + t.name;"#
    )
}

impl Demo {
    fn stream(&self, text: &str, on_event: &mut dyn FnMut(StreamEvent), cancel: &AtomicBool) -> Result<()> {
        let words: Vec<&str> = text.split_inclusive(' ').collect();
        for chunk in words.chunks(3) {
            if cancel.load(Ordering::Relaxed) {
                return Err(Error::Cancelled);
            }
            on_event(StreamEvent::Text(chunk.concat()));
            if !self.chunk_delay.is_zero() {
                std::thread::sleep(self.chunk_delay);
            }
        }
        Ok(())
    }
}

impl LlmProvider for Demo {
    fn name(&self) -> &'static str {
        "demo"
    }

    fn complete(&self, req: &ChatRequest, on_event: &mut dyn FnMut(StreamEvent), cancel: &AtomicBool) -> Result<Reply> {
        let (at, words) = request(req.messages);
        let step = req.messages.iter().skip(at.saturating_add(1)).filter(|m| m.role == Role::Assistant).count();
        let title: String = words.trim().chars().take(40).collect();
        let title = if title.is_empty() { "Merlin".to_string() } else { title };
        let id = |n: usize| format!("demo_{at}_{n}");
        let (text, call, stop) = match step {
            0 => (
                {
                    let files = attached(req.messages, at);
                    let seen = if files.is_empty() { String::new() } else { format!(" I can see {} attached image(s): {}.", files.len(), files.join(", ")) };
                    format!("Demo mode (no model is called).{seen} I'll build a title that says “{title}” and drop it in with a small bounce.")
                },
                Some((id(1), "run_script", json!({"code": script(&title)}))),
                StopReason::ToolUse,
            ),
            1 => ("Let me check the motion.".to_string(), Some((id(2), "render_frames", json!({"times": [0.2, 0.6, 1.2]}))), StopReason::ToolUse),
            _ => (
                "Done: the title drops in from above, overshoots slightly at 0.6 s and settles at 1 s. Keep it, or Revert to undo the whole turn. Pick a real provider in settings to work with a model.".to_string(),
                None,
                StopReason::EndTurn,
            ),
        };
        self.stream(&text, on_event, cancel)?;
        let mut parts = vec![Part::Text { text }];
        if let Some((id, name, input)) = call {
            on_event(StreamEvent::ToolCallStarted { id: id.clone(), name: name.to_string() });
            parts.push(Part::ToolCall { id, name: name.to_string(), input });
        }
        Ok(Reply { message: Message { role: Role::Assistant, parts, native: None }, stop, usage: Usage::default() })
    }
}
