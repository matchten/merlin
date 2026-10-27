//! # merlin-agent (L5)
//!
//! Merlin, the in-app AI agent for EffectCraft: a chat model that animates by writing After Effects-style JavaScript and
//! calling EffectCraft's engine commands, with every edit live and undoable.
//!
//! * [`provider`]: the [`LlmProvider`] interface over a normalized [`conversation`], with two
//!   adapters: [`providers::anthropic`] (Messages API) and [`providers::openai_compat`] (Chat
//!   Completions: OpenAI, Gemini's OpenAI-compatible endpoint, OpenRouter, Ollama, LM Studio…).
//! * [`host`]: where tools run. The desktop app forwards them to the UI thread through its control
//!   queue; [`host::HeadlessHost`] owns a `Session` (tests, examples).
//! * [`tools`]: the tool catalogue the model sees, each tool a thin mapping onto engine commands.
//! * [`agent_loop`]: one user turn: stream a reply, run its tool calls, feed results back, repeat.
//! * [`checkpoint`]: folds a turn's edits into one undo step and reverts it.
//! * [`attachment`]: files the user attaches (images, PSD, SVG, PDF, AI…) decoded into labelled images.
//! * [`notes`]: requests pinned to a frame or range of a comp, applied as one turn.
//! * [`guard`]: keeps edits inside the layers and time range the user pinned.
//! * [`keys`]: API keys from environment variables or the OS keychain, never from preferences.
//!
//! Layering: depends on `effectcraft-engine`, `effectcraft-automation` (PNG encoding) and
//! `effectcraft-media` (attachments), never on egui. Network and keychain code are desktop-only.

#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

pub mod agent_loop;
pub mod attachment;
pub mod checkpoint;
pub mod config;
pub mod context;
pub mod conversation;
pub mod guard;
pub mod host;
pub mod keys;
pub mod notes;
pub mod provider;
pub mod providers;
pub mod sse;
pub mod tools;
pub mod transport;

pub use agent_loop::{AgentEvent, PinBrief, TurnContext, TurnOutcome, run_turn, run_turn_with};
pub use config::{AgentConfig, ProviderKind};
pub use conversation::{Message, Part, ResultPart, Role};
pub use host::{HeadlessHost, Host, RemoteHost};
pub use provider::LlmProvider;

/// The system prompt: how EffectCraft works and how to animate it (stable, so providers cache it).
pub const SYSTEM_PROMPT: &str = include_str!("../prompts/system.md");

#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The provider answered with an HTTP error or an error event.
    #[error("{provider} error{}: {message}", status.map(|s| format!(" (HTTP {s})")).unwrap_or_default())]
    Api { provider: &'static str, status: Option<u16>, message: String },
    /// The connection failed or broke off.
    #[error("network: {0}")]
    Network(String),
    /// The provider sent something this client can't read.
    #[error("unexpected reply from {provider}: {message}")]
    Protocol { provider: &'static str, message: String },
    /// No API key for the chosen provider.
    #[error("no API key for {0}: save one in the Merlin panel's Settings, or set the {1} environment variable")]
    NoKey(&'static str, &'static str),
    /// The OS keychain refused or is unavailable.
    #[error("keychain: {0}")]
    Keychain(String),
    /// The user pressed Stop.
    #[error("stopped")]
    Cancelled,
    #[error("{0}")]
    Other(String),
}

pub type Result<T> = std::result::Result<T, Error>;
