//! Agent settings: which provider and model, and how hard it thinks. No secrets live here (keys are
//! in [`crate::keys`]), so the whole struct is safe to save with preferences and show to scripts.

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ProviderKind {
    /// Anthropic's Messages API (Claude).
    #[default]
    Anthropic,
    /// OpenAI's Chat Completions API.
    OpenAi,
    /// Google Gemini through its OpenAI-compatible endpoint.
    Gemini,
    /// Any other OpenAI-compatible endpoint (OpenRouter, Ollama, LM Studio…) at `base_url`.
    Custom,
}

impl ProviderKind {
    pub const ALL: [ProviderKind; 4] = [ProviderKind::Anthropic, ProviderKind::OpenAi, ProviderKind::Gemini, ProviderKind::Custom];

    /// Stable id (keychain entry name, [`crate::conversation::Native::provider`]).
    pub fn id(self) -> &'static str {
        match self {
            ProviderKind::Anthropic => "anthropic",
            ProviderKind::OpenAi => "openai",
            ProviderKind::Gemini => "gemini",
            ProviderKind::Custom => "custom",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            ProviderKind::Anthropic => "Anthropic (Claude)",
            ProviderKind::OpenAi => "OpenAI",
            ProviderKind::Gemini => "Google Gemini",
            ProviderKind::Custom => "OpenAI-compatible",
        }
    }

    /// Environment variable that overrides the stored key.
    pub fn env_var(self) -> &'static str {
        match self {
            ProviderKind::Anthropic => "ANTHROPIC_API_KEY",
            ProviderKind::OpenAi => "OPENAI_API_KEY",
            ProviderKind::Gemini => "GEMINI_API_KEY",
            ProviderKind::Custom => "EFFECTCRAFT_AGENT_API_KEY",
        }
    }

    /// API root used when the settings leave `base_url` empty.
    pub fn default_base_url(self) -> &'static str {
        match self {
            ProviderKind::Anthropic => "https://api.anthropic.com/v1",
            ProviderKind::OpenAi => "https://api.openai.com/v1",
            ProviderKind::Gemini => "https://generativelanguage.googleapis.com/v1beta/openai",
            ProviderKind::Custom => "http://localhost:11434/v1",
        }
    }

    /// Whether a key is required (local servers usually take none).
    pub fn needs_key(self) -> bool {
        self != ProviderKind::Custom
    }

    /// Suggested models, the default first. Any other model id can be typed in.
    pub fn presets(self) -> &'static [&'static str] {
        match self {
            ProviderKind::Anthropic => &["claude-opus-5-5", "claude-sonnet-5-5", "claude-haiku-5-5"],
            ProviderKind::OpenAi => &["gpt-5"],
            ProviderKind::Gemini => &["gemini-2.5-pro", "gemini-2.5-flash"],
            ProviderKind::Custom => &[],
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct AgentConfig {
    pub provider: ProviderKind,
    /// Model id; empty means the provider's first preset.
    pub model: String,
    /// API root; empty means [`ProviderKind::default_base_url`].
    pub base_url: String,
    /// Anthropic effort: `low`, `medium`, `high`, `xhigh` or `max`.
    pub effort: String,
    /// Safety stop for one user turn (model replies, each possibly with tool calls).
    pub max_steps: u32,
    /// Largest side of frames the agent renders to look at its work.
    pub frame_max_side: u32,
}

impl Default for AgentConfig {
    fn default() -> Self {
        AgentConfig {
            provider: ProviderKind::Anthropic,
            model: String::new(),
            base_url: String::new(),
            effort: "high".into(),
            max_steps: 40,
            frame_max_side: 768,
        }
    }
}

impl AgentConfig {
    pub fn model(&self) -> &str {
        let m = self.model.trim();
        if m.is_empty() { self.provider.presets().first().copied().unwrap_or("") } else { m }
    }

    pub fn base_url(&self) -> &str {
        let b = self.base_url.trim().trim_end_matches('/');
        if b.is_empty() { self.provider.default_base_url() } else { b }
    }

    /// Frame size clamped to what vision models handle well.
    pub fn frame_max_side(&self) -> u32 {
        self.frame_max_side.clamp(128, 1568)
    }

    pub fn max_steps(&self) -> u32 {
        self.max_steps.clamp(1, 200)
    }
}
