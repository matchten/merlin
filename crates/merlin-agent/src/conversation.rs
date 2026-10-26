//! The provider-neutral conversation: what the user said, what the model answered and which tools
//! it called. Every provider adapter translates to and from this model.
//!
//! An assistant message may also carry its provider's own encoding ([`Native`]), replayed unchanged
//! to the same provider (Anthropic requires its thinking blocks back exactly as sent). After a
//! provider switch the native part is ignored and only the normalized parts are sent.

use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    User,
    Assistant,
}

/// An image as the models take it: base64 PNG or JPEG.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Image {
    /// `image/png` or `image/jpeg`.
    pub media_type: String,
    /// Standard base64 (no line breaks).
    pub data: String,
}

impl Image {
    pub fn png(bytes: &[u8]) -> Image {
        Image { media_type: "image/png".into(), data: effectcraft_automation::base64::encode(bytes) }
    }

    /// `data:` URL (OpenAI-compatible APIs).
    pub fn data_url(&self) -> String {
        format!("data:{};base64,{}", self.media_type, self.data)
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum Part {
    Text {
        text: String,
    },
    /// A picture the user attached (`label` names it for the model, e.g. the file name).
    Image {
        image: Image,
        #[serde(default)]
        label: Option<String>,
    },
    /// The model's (summarized) reasoning: shown in the panel, never sent back as normalized text.
    Thinking {
        text: String,
    },
    ToolCall {
        id: String,
        name: String,
        input: Value,
    },
    ToolResult {
        call_id: String,
        content: Vec<ResultPart>,
        #[serde(default)]
        is_error: bool,
    },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum ResultPart {
    Text { text: String },
    Image { image: Image },
}

/// A message in the provider's own wire format, replayed verbatim to that provider.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Native {
    /// [`crate::ProviderKind::id`] of the provider that produced it.
    pub provider: String,
    pub content: Value,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Message {
    pub role: Role,
    pub parts: Vec<Part>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub native: Option<Native>,
}

impl Message {
    pub fn user(parts: Vec<Part>) -> Message {
        Message { role: Role::User, parts, native: None }
    }

    pub fn user_text(text: impl Into<String>) -> Message {
        Message::user(vec![Part::Text { text: text.into() }])
    }

    /// The tool calls this message asks for, in order.
    pub fn tool_calls(&self) -> impl Iterator<Item = (&str, &str, &Value)> {
        self.parts.iter().filter_map(|p| match p {
            Part::ToolCall { id, name, input } => Some((id.as_str(), name.as_str(), input)),
            _ => None,
        })
    }

    /// The message's visible text, joined.
    pub fn text(&self) -> String {
        let mut out = String::new();
        for p in &self.parts {
            if let Part::Text { text } = p {
                if !out.is_empty() {
                    out.push('\n');
                }
                out.push_str(text);
            }
        }
        out
    }
}
