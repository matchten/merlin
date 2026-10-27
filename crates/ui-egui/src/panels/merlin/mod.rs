//! Window ▸ Merlin: the in-app AI agent. A chat where the user asks for motion work in plain
//! language; the agent ([`merlin_agent`]) writes After Effects-style scripts and runs engine commands,
//! every edit live, and each turn ends as one undo step with Keep / Revert.
//!
//! The panel state below is plain serde data (agent-readable through `ui.inspect`); the running
//! turn, the model conversation and frame thumbnails live in a runtime that is never serialized.
//! The agent is desktop-only for now: the web build shows a notice.

use serde::{Deserialize, Serialize};

#[cfg(not(target_arch = "wasm32"))]
mod native;
#[cfg(not(target_arch = "wasm32"))]
pub(crate) use native::Runtime;
#[cfg(not(target_arch = "wasm32"))]
pub use native::{apply_ui_set, attach_paths, show, take_main_window_files, take_window_files, tick};

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct MerlinPanel {
    /// The message being typed.
    pub input: String,
    /// The conversation as shown.
    pub items: Vec<ChatItem>,
    /// The settings view replaces the chat.
    pub settings_open: bool,
    /// Provider and model (no secrets: keys live in the OS keychain).
    pub settings: MerlinSettings,
    /// Files attached to the next message.
    pub attachments: Vec<Attached>,
    /// A turn is running.
    pub busy: bool,
    #[serde(skip)]
    pub(crate) rt: Runtime,
}

/// Saved in the settings store as `merlin.json`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct MerlinSettings {
    /// Provider id: `anthropic`, `openai`, `gemini`, `custom` or `demo`.
    pub provider: String,
    /// Model id; empty means the provider's default.
    pub model: String,
    /// API root; empty means the provider's default.
    pub base_url: String,
    /// Anthropic effort: `low` … `max`; empty means `high`.
    pub effort: String,
}

/// A file attached to the message being written (decoded off the UI thread).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Attached {
    pub path: String,
    /// The file name.
    pub name: String,
    pub state: AttachState,
    /// `PSD · 1200×800` once decoded, or why it can't be attached.
    pub info: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum AttachState {
    Loading,
    Ready,
    Failed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ToolState {
    /// The model is still writing the call.
    Pending,
    Running,
    Done,
    Failed,
}

/// What became of a finished turn's edits.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Decision {
    /// The turn changed the project and awaits Keep / Revert.
    Pending,
    Kept,
    Reverted,
    /// The turn made no edits.
    NoChanges,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum ChatItem {
    User {
        text: String,
        /// Paths of the files sent with it (thumbnails are kept by the runtime).
        #[serde(default)]
        attachments: Vec<String>,
    },
    Assistant {
        text: String,
    },
    /// The model's reasoning summary (collapsed by default).
    Thinking {
        text: String,
        #[serde(default)]
        open: bool,
    },
    Tool {
        id: String,
        name: String,
        /// The call's input as shown (a script's source, else compact JSON).
        input: String,
        state: ToolState,
        /// The start of the result.
        summary: String,
        /// Frames the call rendered (thumbnails are kept by the runtime).
        #[serde(default)]
        frames: usize,
        #[serde(default)]
        open: bool,
    },
    /// The end of a turn: how it ended, what it cost, and Keep / Revert.
    Turn {
        id: u64,
        /// The undo step's name.
        label: String,
        outcome: String,
        decision: Decision,
        tool_calls: usize,
        input_tokens: u64,
        output_tokens: u64,
    },
    Notice {
        text: String,
        error: bool,
    },
}

#[cfg(target_arch = "wasm32")]
#[derive(Clone, Debug, Default)]
pub(crate) struct Runtime;

/// The web build has no agent yet: say so.
#[cfg(target_arch = "wasm32")]
pub fn show(app: &mut crate::EffectcraftApp, ui: &mut egui::Ui, rect: egui::Rect) {
    let t = app.tokens;
    ui.painter().text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        crate::i18n::tr("Merlin runs in the desktop app (not yet in the browser)."),
        crate::theme::Tokens::ui(12.0),
        t.text_faint,
    );
}

#[cfg(target_arch = "wasm32")]
pub fn tick(_: &mut crate::EffectcraftApp, _: &egui::Context, _: f64) {}

#[cfg(target_arch = "wasm32")]
pub fn apply_ui_set(_: &mut crate::EffectcraftApp, _: &egui::Context, _: &serde_json::Value) -> Result<(), String> {
    Err("Merlin runs in the desktop app".into())
}

/// The web build has no agent: floating windows' files all go to import.
#[cfg(target_arch = "wasm32")]
pub fn take_window_files<P>(
    _: &mut crate::EffectcraftApp,
    _: &egui::Context,
    files: Vec<craft_ui::docking::WindowFiles<P>>,
) -> Vec<craft_ui::docking::WindowFiles<P>> {
    files
}
