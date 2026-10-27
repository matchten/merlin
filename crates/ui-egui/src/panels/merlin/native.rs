//! The desktop Merlin panel: turns run on a worker thread; their tool calls come back to the UI
//! thread as control requests, handled between frames by [`tick`] exactly as the control channel
//! handles an external agent's (so edits are ordinary, undoable engine commands).

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::Duration;

use egui::{Align, Color32, Layout, Rect, RichText, Sense, pos2, vec2};
use merlin_agent::agent_loop::{AgentEvent, TurnEnd, TurnOutcome, turn_label};
use merlin_agent::attachment::{self, Attachment};
use merlin_agent::checkpoint::Checkpoint;
use merlin_agent::conversation::{Message, Part};
use merlin_agent::keys::{self, KeySource};
use merlin_agent::{AgentConfig, ProviderKind, RemoteHost, providers, run_turn, transport};
use serde_json::{Value, json};

use super::{AttachState, Attached, ChatItem, Decision, MerlinSettings, ToolState};
use crate::EffectcraftApp;
use crate::control::{self, ControlRequest, Outcome};
use crate::dock::PanelKind;
use crate::i18n::{tr, tr_args};

/// Settings store entry.
const SETTINGS_ENTRY: &str = "merlin.json";
/// How long a stopped turn may take to wind down before the panel stops waiting for it.
const STOP_GRACE_SECS: f64 = 20.0;
/// A tool request unanswered this long (the app is stuck) fails the call.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(120);
/// Characters of a tool input shown when it is expanded.
const INPUT_PREVIEW_CHARS: usize = 4000;
/// Finished turns whose checkpoints are kept for Revert.
const MAX_CHECKPOINTS: usize = 20;
/// Thumbnail width in the chat.
const THUMB_W: f32 = 112.0;
/// Height of the attachment chips row.
const CHIPS_H: f32 = 28.0;

/// Messages from a turn's worker thread.
enum WorkerMsg {
    Event(AgentEvent),
    Done { outcome: TurnOutcome, conversation: Vec<Message> },
}

/// Attachment decodes and attach-dialog answers, from background threads.
enum AttachMsg {
    Picked(Vec<String>),
    Loaded(String, Result<Attachment, String>),
}

struct Worker {
    turn: u64,
    request: String,
    checkpoint: Checkpoint,
    cancel: Arc<AtomicBool>,
    rx: Receiver<WorkerMsg>,
    stopping_since: Option<f64>,
    tool_calls: usize,
}

/// What the panel keeps outside its serialized state.
#[derive(Default)]
pub(crate) struct Runtime {
    loaded: bool,
    /// The model conversation (moved into the worker while a turn runs).
    conversation: Vec<Message>,
    worker: Option<Worker>,
    /// Tool requests from workers, handled on the UI thread.
    requests: Option<(Sender<ControlRequest>, Receiver<ControlRequest>)>,
    deferred: Vec<(ControlRequest, f64)>,
    next_turn: u64,
    checkpoints: Vec<(u64, Checkpoint)>,
    /// Images shown in the chat (PNG) and their textures: a tool call's rendered frames by call
    /// id, an attached file's picture by `att:<path>`.
    frames: HashMap<String, Vec<Vec<u8>>>,
    textures: HashMap<String, Vec<egui::TextureHandle>>,
    /// Key status for the settings view (provider, where it comes from, masked key).
    key_status: Option<(ProviderKind, KeySource, String)>,
    /// The key being typed in settings: never serialized, cleared once saved.
    key_draft: String,
    /// Decoded attachments of the message being written, by path.
    attachments: HashMap<String, Attachment>,
    attach_msgs: Option<(Sender<AttachMsg>, Receiver<AttachMsg>)>,
    /// Files are being dragged over the panel (set by the drop hooks, cleared once drawn).
    drop_hover: bool,
    /// The attachment chips' height last frame (they wrap onto more rows in a narrow panel).
    chips_h: f32,
}

impl Clone for Runtime {
    /// UI state snapshots don't carry the runtime (threads, channels, textures).
    fn clone(&self) -> Self {
        Runtime::default()
    }
}

impl std::fmt::Debug for Runtime {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Runtime").field("running", &self.worker.is_some()).field("conversation", &self.conversation.len()).finish()
    }
}

impl MerlinSettings {
    fn kind(&self) -> ProviderKind {
        ProviderKind::ALL.into_iter().find(|k| k.id() == self.provider).unwrap_or_default()
    }

    fn config(&self) -> AgentConfig {
        let mut c = AgentConfig { provider: self.kind(), model: self.model.clone(), base_url: self.base_url.clone(), ..Default::default() };
        if !self.effort.is_empty() {
            c.effort = self.effort.clone();
        }
        c
    }
}

/// Load the saved settings once.
fn ensure_loaded(app: &mut EffectcraftApp) {
    if app.ui.merlin.rt.loaded {
        return;
    }
    app.ui.merlin.rt.loaded = true;
    let saved = app.session.config.as_ref().and_then(|c| c.read(SETTINGS_ENTRY)).and_then(|s| serde_json::from_str::<MerlinSettings>(&s).ok());
    let mut s = saved.unwrap_or_default();
    if s.provider.is_empty() {
        s.provider = ProviderKind::default().id().into();
    }
    app.ui.merlin.settings = s;
}

fn save_settings(app: &mut EffectcraftApp) {
    let Some(store) = app.session.config.clone() else { return };
    match serde_json::to_string_pretty(&app.ui.merlin.settings) {
        Ok(text) => {
            if let Err(e) = store.write(SETTINGS_ENTRY, &text) {
                app.ui.status = format!("Merlin settings: {e}");
            }
        }
        Err(e) => app.ui.status = format!("Merlin settings: {e}"),
    }
}

// ------------------------------------------------------------------ turns

fn notice(app: &mut EffectcraftApp, text: impl Into<String>, error: bool) {
    app.ui.merlin.items.push(ChatItem::Notice { text: text.into(), error });
}

/// Send the typed message as a new turn.
fn start_turn(app: &mut EffectcraftApp, ctx: &egui::Context) {
    let text = app.ui.merlin.input.trim().to_string();
    if !can_send(&app.ui.merlin) || app.ui.merlin.rt.worker.is_some() {
        return;
    }
    let cfg = app.ui.merlin.settings.config();
    // Starting over implicitly keeps the previous turns.
    for it in &mut app.ui.merlin.items {
        if let ChatItem::Turn { decision: d @ Decision::Pending, .. } = it {
            *d = Decision::Kept;
        }
    }
    let checkpoint = Checkpoint::take(&app.session);
    let panel = &mut app.ui.merlin;
    panel.input.clear();
    // Pictures first, then the words (what the providers recommend); failed chips are dropped.
    let mut parts = vec![];
    let mut sent = vec![];
    for a in std::mem::take(&mut panel.attachments) {
        if let Some(decoded) = panel.rt.attachments.remove(&a.path) {
            parts.push(decoded.part());
            sent.push(a);
        }
    }
    panel.rt.attachments.clear();
    if !text.is_empty() {
        parts.push(Part::Text { text: text.clone() });
    }
    let request = if text.is_empty() { sent.iter().map(|a| a.name.as_str()).collect::<Vec<_>>().join(", ") } else { text.clone() };
    panel.items.push(ChatItem::User { text, attachments: sent.into_iter().map(|a| a.path).collect() });
    let rt = &mut panel.rt;
    rt.next_turn = rt.next_turn.saturating_add(1);
    let turn = rt.next_turn;
    let req_tx = rt.requests.get_or_insert_with(mpsc::channel).0.clone();
    let (ev_tx, ev_rx) = mpsc::channel();
    let cancel = Arc::new(AtomicBool::new(false));
    let conversation = std::mem::take(&mut rt.conversation);
    let (ctx_req, ctx_ev, cancel_req, cancel_turn) = (ctx.clone(), ctx.clone(), cancel.clone(), cancel.clone());
    let spawned = std::thread::Builder::new().name("merlin-turn".into()).spawn(move || {
        // The keychain is read here, off the UI thread (macOS may ask the user first).
        let key = if cfg.provider.needs_key() || cfg.provider == ProviderKind::Custom { keys::get(cfg.provider).0.unwrap_or_default() } else { String::new() };
        let provider = providers::build(&cfg, key, Arc::new(transport::Http::default()));
        let mut host = RemoteHost::new(move |method: &str, params: Value| {
            if cancel_req.load(Ordering::Relaxed) {
                return Err("not run: the user stopped the turn".into());
            }
            let (req, rx) = ControlRequest::new(method, params);
            req_tx.send(req).map_err(|_| "the Merlin panel is gone".to_string())?;
            ctx_req.request_repaint();
            rx.recv_timeout(REQUEST_TIMEOUT).map_err(|_| format!("{method}: the app didn't answer"))
        });
        let mut conversation = conversation;
        let outcome = run_turn(
            provider.as_ref(),
            &mut host,
            &cfg,
            &mut conversation,
            parts,
            &mut |e| {
                let _ = ev_tx.send(WorkerMsg::Event(e));
                ctx_ev.request_repaint();
            },
            &cancel_turn,
        );
        let _ = ev_tx.send(WorkerMsg::Done { outcome, conversation });
        ctx_ev.request_repaint();
    });
    match spawned {
        Ok(_) => {
            rt.worker = Some(Worker { turn, request, checkpoint, cancel, rx: ev_rx, stopping_since: None, tool_calls: 0 });
            panel.busy = true;
        }
        Err(e) => notice(app, tr_args("Merlin couldn't start: {}", &[&e]), true),
    }
}

/// Whether the message can be sent: words or ready attachments, and no attachment still loading.
fn can_send(panel: &super::MerlinPanel) -> bool {
    let loading = panel.attachments.iter().any(|a| a.state == AttachState::Loading);
    let ready = panel.attachments.iter().any(|a| a.state == AttachState::Ready);
    !loading && (ready || !panel.input.trim().is_empty())
}

// ------------------------------------------------------------------ attachments

/// Attach files to the next message. They are decoded on a background thread (a big PDF page can
/// take a moment); their chips show progress, and Send waits for them.
pub fn attach_paths(app: &mut EffectcraftApp, ctx: &egui::Context, paths: Vec<String>) {
    let panel = &mut app.ui.merlin;
    let mut fresh = vec![];
    let mut over = 0usize;
    for path in paths {
        if path.is_empty() || panel.attachments.iter().any(|a| a.path == path) {
            continue;
        }
        if panel.attachments.len() >= attachment::MAX_PER_MESSAGE {
            over += 1;
            continue;
        }
        let name = std::path::Path::new(&path).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| path.clone());
        panel.attachments.push(Attached { path: path.clone(), name, state: AttachState::Loading, info: String::new() });
        fresh.push(path);
    }
    if over > 0 {
        notice(app, tr_args("A message can carry up to {} files; {} more weren't attached.", &[&attachment::MAX_PER_MESSAGE, &over]), true);
    }
    if fresh.is_empty() {
        return;
    }
    app.ui.merlin.settings_open = false;
    let tx = app.ui.merlin.rt.attach_msgs.get_or_insert_with(mpsc::channel).0.clone();
    let c = ctx.clone();
    let todo = fresh.clone();
    let spawned = std::thread::Builder::new().name("merlin-attach".into()).spawn(move || {
        for path in todo {
            let r = attachment::load(&path);
            if tx.send(AttachMsg::Loaded(path, r)).is_err() {
                return;
            }
            c.request_repaint();
        }
    });
    if let Err(e) = spawned {
        for a in app.ui.merlin.attachments.iter_mut().filter(|a| fresh.contains(&a.path)) {
            a.state = AttachState::Failed;
            a.info = e.to_string();
        }
    }
}

/// `ui.set {"merlin": {...}}` from the control channel: `attach` (absolute paths) attaches files
/// as a drop would, for agents and scripts that can't drag.
pub fn apply_ui_set(app: &mut EffectcraftApp, ctx: &egui::Context, v: &Value) -> Result<(), String> {
    let Some(list) = v.get("attach") else { return Ok(()) };
    let paths: Vec<String> =
        list.as_array().ok_or("merlin.attach: expected an array of absolute paths")?.iter().filter_map(Value::as_str).map(str::to_string).collect();
    if let Some(bad) = paths.iter().find(|p| !std::path::Path::new(p).is_absolute()) {
        return Err(format!("merlin.attach: `{bad}` is not an absolute path"));
    }
    attach_paths(app, ctx, paths);
    Ok(())
}

/// Take in decoded attachments and attach-dialog answers.
fn take_attachments(app: &mut EffectcraftApp, ctx: &egui::Context) {
    let Some((tx, rx)) = app.ui.merlin.rt.attach_msgs.take() else { return };
    let mut picked = vec![];
    while let Ok(m) = rx.try_recv() {
        match m {
            AttachMsg::Picked(paths) => picked.extend(paths),
            AttachMsg::Loaded(path, result) => {
                let panel = &mut app.ui.merlin;
                // A chip removed while loading takes its result with it.
                let Some(chip) = panel.attachments.iter_mut().find(|a| a.path == path) else { continue };
                match result {
                    Ok(a) => {
                        chip.state = AttachState::Ready;
                        chip.info = format!("{} · {}×{}{}", a.format, a.width, a.height, if a.video { tr(" · first frame") } else { "" });
                        panel.rt.textures.remove(&format!("att:{path}"));
                        panel.rt.frames.insert(format!("att:{path}"), vec![a.png.clone()]);
                        panel.rt.attachments.insert(path, a);
                    }
                    Err(e) => {
                        chip.state = AttachState::Failed;
                        chip.info = e;
                    }
                }
            }
        }
    }
    app.ui.merlin.rt.attach_msgs = Some((tx, rx));
    if !picked.is_empty() {
        attach_paths(app, ctx, picked);
    }
}

/// The Attach button: the host's file dialog (without blocking the frame where it can).
fn pick_attachments(app: &mut EffectcraftApp, ctx: &egui::Context) {
    let exts = attachment::extensions();
    if let Some(spawn) = app.hooks.file_dialog_async.as_ref() {
        let tx = app.ui.merlin.rt.attach_msgs.get_or_insert_with(mpsc::channel).0.clone();
        let c = ctx.clone();
        spawn(
            crate::FileDialog::Open { exts: exts.iter().map(|e| e.to_string()).collect(), many: true },
            Box::new(move |paths| {
                let _ = tx.send(AttachMsg::Picked(paths));
                c.request_repaint();
            }),
        );
    } else if let Some(pick) = app.hooks.pick_files.as_ref() {
        let paths = pick(&exts);
        attach_paths(app, ctx, paths);
    } else {
        notice(app, tr("No file dialog here: drag files onto the Merlin panel to attach them."), true);
    }
}

fn local_paths(files: &[egui::DroppedFileHandle]) -> Vec<String> {
    files.iter().filter(|f| f.path().is_absolute()).map(|f| f.path().to_string_lossy().into_owned()).collect()
}

/// Files dragged over or dropped on the Merlin panel docked in the main window become attachments
/// rather than imports. The panel under the pointer decides, else the focused panel: the window
/// system may send no pointer moves while files are dragged in (macOS). Returns whether Merlin
/// took them (the caller then neither imports nor shows its import hint).
pub fn take_main_window_files(app: &mut EffectcraftApp, ctx: &egui::Context) -> bool {
    let (hovering, dropped, pos) = ctx.input(|i| (!i.raw.hovered_files.is_empty(), local_paths(&i.raw.dropped_files), i.pointer.hover_pos()));
    let any_dropped = ctx.input(|i| !i.raw.dropped_files.is_empty());
    if !hovering && !any_dropped {
        return false;
    }
    let shown = app.dock_rects.iter().any(|(k, _)| *k == PanelKind::Merlin) && !app.in_panel_window(PanelKind::Merlin);
    if !shown || app.panel_at(pos) != PanelKind::Merlin {
        return false;
    }
    if hovering {
        app.ui.merlin.rt.drop_hover = true;
        ctx.request_repaint();
    }
    if !dropped.is_empty() {
        attach_paths(app, ctx, dropped);
    }
    true
}

/// Files dragged over or dropped on a floating window showing Merlin are its attachments; the rest
/// go on to import.
pub fn take_window_files(
    app: &mut EffectcraftApp,
    ctx: &egui::Context,
    files: Vec<craft_ui::docking::WindowFiles<PanelKind>>,
) -> Vec<craft_ui::docking::WindowFiles<PanelKind>> {
    let (mine, rest): (Vec<_>, Vec<_>) = files.into_iter().partition(|w| w.panel == PanelKind::Merlin);
    for w in mine {
        if !w.hovered.is_empty() {
            app.ui.merlin.rt.drop_hover = true;
            ctx.request_repaint();
        }
        let paths = local_paths(&w.dropped);
        if !paths.is_empty() {
            attach_paths(app, ctx, paths);
        }
    }
    rest
}

fn stop_turn(app: &mut EffectcraftApp, now: f64) {
    if let Some(w) = app.ui.merlin.rt.worker.as_mut() {
        w.cancel.store(true, Ordering::Relaxed);
        w.stopping_since.get_or_insert(now);
    }
}

/// Fold an event into the visible conversation.
fn apply_event(app: &mut EffectcraftApp, e: AgentEvent) {
    let panel = &mut app.ui.merlin;
    match e {
        AgentEvent::Text(t) => match panel.items.last_mut() {
            Some(ChatItem::Assistant { text }) => text.push_str(&t),
            _ => panel.items.push(ChatItem::Assistant { text: t }),
        },
        AgentEvent::Thinking(t) => match panel.items.last_mut() {
            Some(ChatItem::Thinking { text, .. }) => text.push_str(&t),
            _ => panel.items.push(ChatItem::Thinking { text: t, open: false }),
        },
        AgentEvent::ToolStarted { id, name } => {
            panel.items.push(ChatItem::Tool { id, name, input: String::new(), state: ToolState::Pending, summary: String::new(), frames: 0, open: false });
        }
        AgentEvent::ToolRunning { id, name, input } => {
            let shown = input.get("code").and_then(Value::as_str).map(str::to_string).unwrap_or_else(|| input.to_string());
            let shown: String = shown.chars().take(INPUT_PREVIEW_CHARS).collect();
            match tool_item(&mut panel.items, &id) {
                Some(ChatItem::Tool { input, state, .. }) => {
                    *input = shown;
                    *state = ToolState::Running;
                }
                _ => panel.items.push(ChatItem::Tool { id, name, input: shown, state: ToolState::Running, summary: String::new(), frames: 0, open: false }),
            }
        }
        AgentEvent::ToolFinished { id, is_error, summary, frames, .. } => {
            if let Some(w) = panel.rt.worker.as_mut() {
                w.tool_calls = w.tool_calls.saturating_add(1);
            }
            let n = frames.len();
            if n > 0 {
                panel.rt.frames.insert(id.clone(), frames);
            }
            if let Some(ChatItem::Tool { state, summary: s, frames: f, .. }) = tool_item(&mut panel.items, &id) {
                *state = if is_error { ToolState::Failed } else { ToolState::Done };
                *s = summary;
                *f = n;
            }
        }
        AgentEvent::Usage(_) => {}
    }
}

fn tool_item<'a>(items: &'a mut [ChatItem], call: &str) -> Option<&'a mut ChatItem> {
    items.iter_mut().rev().find(|i| matches!(i, ChatItem::Tool { id, .. } if id == call))
}

/// The turn ended (or was abandoned): one undo step, the Keep / Revert card, the conversation back.
fn finish_turn(app: &mut EffectcraftApp, w: Worker, outcome: Option<TurnOutcome>, conversation: Option<Vec<Message>>) {
    let label = turn_label(&w.request);
    let changed = w.checkpoint.collapse(&mut app.session, &label);
    let (text, error) = match outcome.as_ref().map(|o| &o.end) {
        Some(TurnEnd::Done) => (tr("Done"), None::<String>),
        Some(TurnEnd::Cancelled) => (tr("Stopped"), None),
        Some(TurnEnd::MaxSteps) => (tr("Stopped at the step limit"), None),
        Some(TurnEnd::Refused(_)) => (tr("Declined by the provider's safety system"), None),
        Some(TurnEnd::Truncated) => (tr("The reply was cut off at the output limit"), None),
        Some(TurnEnd::Error(e)) => (tr("Failed"), Some(e.clone())),
        None => (tr("Stopped (the model didn't respond in time)"), None),
    };
    if let Some(e) = error {
        notice(app, e, true);
    }
    if outcome.is_none() {
        notice(app, tr("Merlin stopped waiting; the next message starts a fresh conversation."), false);
    }
    let usage = outcome.as_ref().map(|o| o.usage).unwrap_or_default();
    let panel = &mut app.ui.merlin;
    panel.items.push(ChatItem::Turn {
        id: w.turn,
        label,
        outcome: text.to_string(),
        decision: if changed { Decision::Pending } else { Decision::NoChanges },
        tool_calls: outcome.as_ref().map_or(w.tool_calls, |o| o.tool_calls),
        input_tokens: usage.total_input(),
        output_tokens: usage.output_tokens,
    });
    if changed {
        panel.rt.checkpoints.push((w.turn, w.checkpoint));
        let extra = panel.rt.checkpoints.len().saturating_sub(MAX_CHECKPOINTS);
        panel.rt.checkpoints.drain(..extra);
    }
    panel.rt.conversation = conversation.unwrap_or_default();
    panel.busy = false;
}

fn decide(app: &mut EffectcraftApp, ctx: &egui::Context, turn: u64, keep: bool) {
    let mut decision = Decision::Kept;
    if !keep {
        let latest = app.ui.merlin.rt.checkpoints.iter().find(|(t, _)| *t == turn).is_some_and(|(_, c)| c.is_latest(&app.session));
        if !latest {
            notice(app, tr("Can't revert this turn: there are later edits. Use Window ▸ History to go back."), true);
            return;
        }
        if let Err(e) = crate::menus::invoke(app, ctx, "edit.undo", json!({})) {
            notice(app, e, true);
            return;
        }
        decision = Decision::Reverted;
    }
    for it in &mut app.ui.merlin.items {
        if let ChatItem::Turn { id, decision: d, .. } = it
            && *id == turn
        {
            *d = decision;
        }
    }
}

/// Per frame (whether or not the panel is shown): run workers' tool requests on the UI thread and
/// take in their progress.
pub fn tick(app: &mut EffectcraftApp, ctx: &egui::Context, now: f64) {
    ensure_loaded(app);
    take_attachments(app, ctx);
    if let Some((tx, rx)) = app.ui.merlin.rt.requests.take() {
        let mut reqs = std::mem::take(&mut app.ui.merlin.rt.deferred);
        while let Ok(r) = rx.try_recv() {
            reqs.push((r, now + 3.0));
        }
        for (req, deadline) in reqs {
            let reply = req.reply.clone();
            match control::handle(app, ctx, &req, now) {
                Outcome::Done(v) => {
                    let _ = reply.send(v);
                }
                Outcome::Retry(_) if now < deadline => {
                    app.ui.merlin.rt.deferred.push((req, deadline));
                    ctx.request_repaint();
                }
                Outcome::Retry(msg) => {
                    let _ = reply.send(json!({"ok": false, "error": msg}));
                }
                // Merlin only sends commands and renders; anything that would wait on input or jobs
                // is answered right away.
                _ => {
                    let _ = reply.send(json!({"ok": false, "error": "started in the background; check jobs.list for progress"}));
                }
            }
        }
        app.ui.merlin.rt.requests = Some((tx, rx));
    }
    let Some(w) = app.ui.merlin.rt.worker.take() else { return };
    let mut done = None;
    let mut events = vec![];
    while let Ok(m) = w.rx.try_recv() {
        match m {
            WorkerMsg::Event(e) => events.push(e),
            WorkerMsg::Done { outcome, conversation } => {
                done = Some((outcome, conversation));
                break;
            }
        }
    }
    app.ui.merlin.rt.worker = Some(w);
    for e in events {
        apply_event(app, e);
    }
    let abandon = app.ui.merlin.rt.worker.as_ref().and_then(|w| w.stopping_since).is_some_and(|t| now - t > STOP_GRACE_SECS);
    if done.is_some() || abandon {
        let Some(w) = app.ui.merlin.rt.worker.take() else { return };
        match done {
            Some((outcome, conversation)) => finish_turn(app, w, Some(outcome), Some(conversation)),
            None => finish_turn(app, w, None, None),
        }
    } else if app.ui.merlin.rt.worker.as_ref().is_some_and(|w| w.stopping_since.is_some()) {
        ctx.request_repaint_after(Duration::from_millis(250));
    }
}

// ------------------------------------------------------------------ view

enum Act {
    Send,
    Stop,
    NewChat,
    ToggleSettings,
    Keep(u64),
    Revert(u64),
    Toggle(usize),
    SaveKey,
    RemoveKey,
    Attach,
    Unattach(usize),
}

/// The panel: a header (provider · model, New Chat, Settings), the conversation and the input, or
/// the settings in place of the conversation.
pub fn show(app: &mut EffectcraftApp, ui: &mut egui::Ui, rect: Rect) {
    ensure_loaded(app);
    let ctx = ui.ctx().clone();
    let now = app.now();
    let header = Rect::from_min_size(rect.min, vec2(rect.width(), 30.0));
    let chips = if app.ui.merlin.attachments.is_empty() { 0.0 } else { app.ui.merlin.rt.chips_h.max(CHIPS_H) + 4.0 };
    let footer_h = (122.0 + chips).min(rect.height() * 0.5);
    let footer = Rect::from_min_max(pos2(rect.min.x, rect.max.y - footer_h), rect.max);
    let body = Rect::from_min_max(pos2(rect.min.x, header.max.y), pos2(rect.max.x, footer.min.y));
    let mut acts = vec![];
    header_ui(app, ui, header, &mut acts);
    if app.ui.merlin.settings_open {
        settings_ui(app, ui, Rect::from_min_max(body.min, rect.max), &mut acts);
    } else {
        log_ui(app, ui, body, &mut acts);
        footer_ui(app, ui, footer, &mut acts);
    }
    if std::mem::take(&mut app.ui.merlin.rt.drop_hover) {
        drop_hint(app, ui, rect);
    }
    for a in acts {
        match a {
            Act::Send => start_turn(app, &ctx),
            Act::Stop => stop_turn(app, now),
            Act::NewChat => {
                if app.ui.merlin.rt.worker.is_none() {
                    let p = &mut app.ui.merlin;
                    p.items.clear();
                    p.rt.conversation.clear();
                    p.rt.frames.retain(|k, _| p.attachments.iter().any(|a| *k == format!("att:{}", a.path)));
                    p.rt.textures.clear();
                }
            }
            Act::Attach => pick_attachments(app, &ctx),
            Act::Unattach(i) => {
                let p = &mut app.ui.merlin;
                if i < p.attachments.len() {
                    let a = p.attachments.remove(i);
                    p.rt.attachments.remove(&a.path);
                }
            }
            Act::ToggleSettings => {
                let p = &mut app.ui.merlin;
                p.settings_open = !p.settings_open;
                p.rt.key_status = None;
            }
            Act::Keep(t) => decide(app, &ctx, t, true),
            Act::Revert(t) => decide(app, &ctx, t, false),
            Act::Toggle(i) => {
                if let Some(ChatItem::Thinking { open, .. } | ChatItem::Tool { open, .. }) = app.ui.merlin.items.get_mut(i) {
                    *open = !*open;
                }
            }
            Act::SaveKey | Act::RemoveKey => {
                let p = &mut app.ui.merlin;
                let kind = p.settings.kind();
                let r = if matches!(a, Act::SaveKey) { keys::set(kind, &p.rt.key_draft) } else { keys::remove(kind) };
                p.rt.key_draft.clear();
                p.rt.key_status = None;
                if let Err(e) = r {
                    app.ui.status = e.to_string();
                }
            }
        }
    }
}

fn child(ui: &mut egui::Ui, r: Rect) -> egui::Ui {
    ui.new_child(egui::UiBuilder::new().max_rect(r).layout(Layout::top_down(Align::Min)))
}

fn header_ui(app: &mut EffectcraftApp, ui: &mut egui::Ui, r: Rect, acts: &mut Vec<Act>) {
    let t = app.tokens;
    ui.painter().rect_filled(r, 0.0, t.header_bg);
    let mut c = child(ui, r.shrink2(vec2(8.0, 4.0)));
    c.horizontal(|ui| {
        ui.label(RichText::new("Merlin").strong().color(t.text));
        // Right to left: the buttons take their room first, then the provider chip truncates into
        // what is left (narrow docks).
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            let label = if app.ui.merlin.settings_open { tr("Back to chat") } else { tr("Settings") };
            let resp = ui.small_button(label);
            app.auto.add("merlin.settings", resp.rect, "Settings");
            if resp.clicked() {
                acts.push(Act::ToggleSettings);
            }
            if !app.ui.merlin.settings_open {
                let resp = ui.add_enabled(!app.ui.merlin.busy, egui::Button::new(tr("New Chat")).small());
                app.auto.add("merlin.newChat", resp.rect, "New Chat");
                if resp.clicked() {
                    acts.push(Act::NewChat);
                }
            }
            ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                let cfg = app.ui.merlin.settings.config();
                let chip = format!("{} · {}", cfg.provider.label(), cfg.model());
                let resp = ui.add(egui::Button::new(RichText::new(chip).small().color(t.text_dim)).frame(false).truncate());
                app.auto.add("merlin.model", resp.rect, "Provider and model");
                if resp.on_hover_text(tr("Change the provider or model")).clicked() {
                    acts.push(Act::ToggleSettings);
                }
            });
        });
    });
}

fn log_ui(app: &mut EffectcraftApp, ui: &mut egui::Ui, r: Rect, acts: &mut Vec<Act>) {
    let t = app.tokens;
    let ctx = ui.ctx().clone();
    app.auto.add("merlin.log", r, "Merlin conversation");
    if app.ui.merlin.items.is_empty() {
        let hint =
            tr("Ask Merlin to build or animate something, e.g. “a title that drops in with a bounce”. Every turn is one undo step you can keep or revert.");
        let mut c = child(ui, r.shrink(24.0));
        c.add_space((r.height() * 0.3).max(0.0));
        c.vertical_centered(|ui| ui.label(RichText::new(hint).color(t.text_faint)));
        return;
    }
    let mut c = child(ui, r.shrink2(vec2(8.0, 6.0)));
    let EffectcraftApp { ui: state, auto, session, .. } = app;
    let panel = &mut state.merlin;
    egui::ScrollArea::vertical().id_salt("merlin-log").auto_shrink([false, false]).stick_to_bottom(true).show(&mut c, |ui| {
        ui.set_width(ui.available_width());
        let latest: Vec<u64> = panel.rt.checkpoints.iter().filter(|(_, cp)| cp.is_latest(session)).map(|(id, _)| *id).collect();
        for (i, item) in panel.items.iter().enumerate() {
            match item {
                ChatItem::User { text, attachments } => {
                    egui::Frame::new().fill(t.field_bg).inner_margin(egui::Margin::same(8)).corner_radius(egui::CornerRadius::same(6)).show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        if !attachments.is_empty() {
                            let keys: Vec<String> = attachments.iter().map(|p| format!("att:{p}")).collect();
                            thumbnails(ui, &ctx, &mut panel.rt, &keys);
                        }
                        if !text.is_empty() {
                            ui.add(egui::Label::new(RichText::new(text).color(t.text)).wrap());
                        }
                    });
                }
                ChatItem::Assistant { text } => {
                    ui.add(egui::Label::new(RichText::new(text).color(t.text)).wrap());
                }
                ChatItem::Thinking { text, open } => {
                    let head = format!("{} {}", if *open { "▾" } else { "▸" }, tr("Thinking"));
                    let resp = ui.add(egui::Button::new(RichText::new(head).small().color(t.text_faint)).frame(false));
                    auto.add(&format!("merlin.item.{i}.toggle"), resp.rect, "Thinking");
                    if resp.clicked() {
                        acts.push(Act::Toggle(i));
                    }
                    if *open {
                        ui.add(egui::Label::new(RichText::new(text).small().italics().color(t.text_dim)).wrap());
                    }
                }
                ChatItem::Tool { id, name, input, state, summary, frames, open } => {
                    let (mark, color) = match state {
                        ToolState::Pending | ToolState::Running => ("…", t.text_dim),
                        ToolState::Done => ("✓", t.cache_green),
                        ToolState::Failed => ("✗", t.danger),
                    };
                    let first = summary.lines().next().unwrap_or("");
                    let first: String = first.chars().take(80).collect();
                    let head = format!("{} {mark} {name}  {first}", if *open { "▾" } else { "▸" });
                    let resp = ui.add(egui::Button::new(RichText::new(head).small().monospace().color(color)).frame(false).truncate());
                    auto.add(&format!("merlin.item.{i}.toggle"), resp.rect, name);
                    if resp.clicked() {
                        acts.push(Act::Toggle(i));
                    }
                    if *open {
                        egui::Frame::new().fill(t.field_bg).inner_margin(egui::Margin::same(6)).show(ui, |ui| {
                            ui.set_width(ui.available_width());
                            if !input.is_empty() {
                                ui.add(egui::Label::new(RichText::new(input).small().monospace().color(t.text_dim)).wrap());
                            }
                            if !summary.is_empty() {
                                ui.add(egui::Label::new(RichText::new(summary).small().monospace().color(t.text_faint)).wrap());
                            }
                        });
                    }
                    if *frames > 0 {
                        thumbnails(ui, &ctx, &mut panel.rt, std::slice::from_ref(id));
                    }
                }
                ChatItem::Turn { id, label, outcome, decision, tool_calls, input_tokens, output_tokens } => {
                    egui::Frame::new()
                        .stroke(egui::Stroke::new(1.0, t.field_border))
                        .inner_margin(egui::Margin::same(8))
                        .corner_radius(egui::CornerRadius::same(6))
                        .show(ui, |ui| {
                            ui.set_width(ui.available_width());
                            let what = match decision {
                                Decision::Pending => tr("Applied as one undo step"),
                                Decision::Kept => tr("Kept"),
                                Decision::Reverted => tr("Reverted"),
                                Decision::NoChanges => tr("No changes"),
                            };
                            ui.label(RichText::new(format!("{outcome} · {what}")).color(t.text));
                            let cost = tr_args("{} tool calls · {} tokens in · {} out", &[tool_calls, &tokens(*input_tokens), &tokens(*output_tokens)]);
                            ui.label(RichText::new(format!("“{label}” · {cost}")).small().color(t.text_faint));
                            if *decision == Decision::Pending {
                                ui.horizontal(|ui| {
                                    let keep = ui.button(tr("Keep"));
                                    auto.add(&format!("merlin.turn.{id}.keep"), keep.rect, "Keep");
                                    if keep.clicked() {
                                        acts.push(Act::Keep(*id));
                                    }
                                    let can = latest.contains(id);
                                    let revert = ui.add_enabled(can, egui::Button::new(tr("Revert")));
                                    auto.add(&format!("merlin.turn.{id}.revert"), revert.rect, "Revert");
                                    let revert = if can { revert } else { revert.on_disabled_hover_text(tr("There are later edits: use Window ▸ History")) };
                                    if revert.clicked() {
                                        acts.push(Act::Revert(*id));
                                    }
                                });
                            }
                        });
                }
                ChatItem::Notice { text, error } => {
                    ui.add(egui::Label::new(RichText::new(text).small().color(if *error { t.danger } else { t.text_dim })).wrap());
                }
            }
            ui.add_space(6.0);
        }
        if let Some(w) = panel.rt.worker.as_ref() {
            let what = if w.stopping_since.is_some() { tr("Stopping…") } else { tr("Merlin is working… edits appear live.") };
            ui.label(RichText::new(what).small().color(t.accent));
        }
    });
}

fn tokens(n: u64) -> String {
    if n >= 10_000 { format!("{}k", n / 1000) } else { n.to_string() }
}

/// The textures of the images under `key` in [`Runtime::frames`], made on first use.
fn textures<'a>(ctx: &egui::Context, rt: &'a mut Runtime, key: &str) -> &'a [egui::TextureHandle] {
    if !rt.textures.contains_key(key) {
        let texs: Vec<egui::TextureHandle> = rt
            .frames
            .get(key)
            .map(Vec::as_slice)
            .unwrap_or_default()
            .iter()
            .enumerate()
            .filter_map(|(i, png)| {
                let img = image::load_from_memory(png).ok()?.to_rgba8();
                let size = [img.width() as usize, img.height() as usize];
                let ci = egui::ColorImage::from_rgba_unmultiplied(size, img.as_raw());
                Some(ctx.load_texture(format!("merlin-{key}-{i}"), ci, egui::TextureOptions::LINEAR))
            })
            .collect();
        rt.textures.insert(key.to_string(), texs);
    }
    rt.textures.get(key).map(Vec::as_slice).unwrap_or_default()
}

fn paint_texture(ui: &egui::Ui, tex: &egui::TextureHandle, r: Rect) {
    ui.painter().image(tex.id(), r, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
}

/// A row of the images under `keys` (a tool call's rendered frames, a message's attachments).
fn thumbnails(ui: &mut egui::Ui, ctx: &egui::Context, rt: &mut Runtime, keys: &[String]) {
    ui.horizontal_wrapped(|ui| {
        for key in keys {
            for tex in textures(ctx, rt, key) {
                let [w, h] = tex.size();
                let aspect = if w > 0 { (h as f32 / w as f32).clamp(0.2, 2.0) } else { 0.5625 };
                let (r, _) = ui.allocate_exact_size(vec2(THUMB_W, THUMB_W * aspect), Sense::hover());
                paint_texture(ui, tex, r);
            }
        }
    });
}

/// Over the panel while files are dragged onto it.
fn drop_hint(app: &mut EffectcraftApp, ui: &egui::Ui, rect: Rect) {
    let t = app.tokens;
    let painter = ui.ctx().layer_painter(egui::LayerId::new(egui::Order::Foreground, egui::Id::unique("merlin-drop")));
    painter.rect_filled(rect.shrink(4.0), 6.0, t.panel_bg.gamma_multiply(0.85));
    painter.rect_stroke(rect.shrink(6.0), 6.0, egui::Stroke::new(2.0, t.accent), egui::StrokeKind::Inside);
    let label = tr("Drop to attach to your message to Merlin");
    painter.text(rect.center(), egui::Align2::CENTER_CENTER, label, crate::theme::Tokens::medium(14.0), t.text);
    app.auto.add("merlin.dropTarget", rect, label);
}

/// The active comp, selection and time, as the agent will see them.
fn context_line(app: &EffectcraftApp) -> String {
    let s = &app.session;
    let Some(cid) = s.active_comp_id() else { return tr("No composition open: Merlin will create one if needed.").to_string() };
    let name = s.project.item(cid).map(|i| i.name.clone()).unwrap_or_default();
    let sel = &s.state.selected_layers;
    let time = format!("{:.2}s", s.time().seconds());
    let names: Vec<String> = s.active_comp().map(|c| c.layers.iter().filter(|l| sel.contains(&l.id)).map(|l| l.name.clone()).collect()).unwrap_or_default();
    match names.as_slice() {
        [] => tr_args("Comp “{}” · at {}", &[&name, &time]),
        [one] => tr_args("Comp “{}” · “{}” selected · at {}", &[&name, one, &time]),
        [a, b] => tr_args("Comp “{}” · “{}”, “{}” selected · at {}", &[&name, a, b, &time]),
        _ => tr_args("Comp “{}” · {} layers selected · at {}", &[&name, &names.len(), &time]),
    }
}

fn footer_ui(app: &mut EffectcraftApp, ui: &mut egui::Ui, r: Rect, acts: &mut Vec<Act>) {
    let t = app.tokens;
    ui.painter().line_segment([r.left_top(), r.right_top()], egui::Stroke::new(1.0, t.separator));
    let line = context_line(app);
    let busy = app.ui.merlin.busy;
    let mut c = child(ui, r.shrink2(vec2(8.0, 6.0)));
    c.horizontal(|ui| {
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            let b = ui.small_button(tr("Attach…")).on_hover_text(tr("Attach images, Photoshop, SVG, PDF or Illustrator files (or drag them onto this panel)"));
            app.auto.add("merlin.attach", b.rect, "Attach files");
            if b.clicked() {
                acts.push(Act::Attach);
            }
            ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                ui.add(egui::Label::new(RichText::new(line).small().color(t.text_faint)).truncate());
            });
        });
    });
    if !app.ui.merlin.attachments.is_empty() {
        chips_ui(app, &mut c, acts);
    }
    let input_id = egui::Id::unique("merlin-input");
    // Enter sends, Shift+Enter starts a new line.
    if !busy && c.ctx().memory(|m| m.has_focus(input_id)) && c.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Enter)) {
        acts.push(Act::Send);
    }
    let width = c.available_width();
    c.horizontal(|ui| {
        let te = egui::TextEdit::multiline(&mut app.ui.merlin.input)
            .id(input_id)
            .desired_width(width - 64.0)
            .desired_rows(3)
            .hint_text(tr("Ask Merlin… (Enter sends, Shift+Enter for a new line)"));
        let resp = ui.add(te);
        app.auto.add_response("merlin.input", &resp, "Message Merlin");
        let (label, act) = if busy { (tr("Stop"), Act::Stop) } else { (tr("Send"), Act::Send) };
        let enabled = busy || can_send(&app.ui.merlin);
        let b = ui.add_enabled(enabled, egui::Button::new(label).min_size(vec2(52.0, 28.0)));
        app.auto.add("merlin.send", b.rect, if busy { "Stop" } else { "Send" });
        if b.clicked() {
            acts.push(act);
        }
    });
}

/// The attachments of the message being written: thumbnail, name, ×. Laid out by hand so the
/// chips wrap onto more rows in a narrow panel.
fn chips_ui(app: &mut EffectcraftApp, ui: &mut egui::Ui, acts: &mut Vec<Act>) {
    const PAD: f32 = 5.0;
    const THUMB: f32 = 18.0;
    const X_W: f32 = 12.0;
    const GAP: f32 = 4.0;
    let t = app.tokens;
    let ctx = ui.ctx().clone();
    let font = egui::TextStyle::Small.resolve(ui.style());
    let left = ui.cursor().min.x;
    let right = ui.max_rect().max.x;
    let top = ui.cursor().min.y;
    let row_h = CHIPS_H - GAP;
    let EffectcraftApp { ui: state, auto, .. } = app;
    let panel = &mut state.merlin;
    let (mut x, mut y) = (left, top);
    for i in 0..panel.attachments.len() {
        let Some(a) = panel.attachments.get(i).cloned() else { break };
        let name: String = a.name.chars().take(28).collect();
        let color = if a.state == AttachState::Failed { t.danger } else { t.text };
        let galley = ui.painter().layout_no_wrap(name, font.clone(), color);
        let w = (PAD + THUMB + GAP + galley.size().x + GAP + X_W + PAD).min(right - left);
        if x > left && x + w > right {
            x = left;
            y += CHIPS_H;
        }
        let chip = Rect::from_min_size(pos2(x, y), vec2(w, row_h));
        x += w + GAP;
        let stroke = if a.state == AttachState::Failed { t.danger } else { t.field_border };
        ui.painter().rect_filled(chip, 4.0, t.field_bg);
        ui.painter().rect_stroke(chip, 4.0, egui::Stroke::new(1.0, stroke), egui::StrokeKind::Inside);
        let slot = Rect::from_min_size(pos2(chip.min.x + PAD, chip.center().y - THUMB / 2.0), vec2(THUMB, THUMB));
        match a.state {
            AttachState::Loading => egui::Spinner::new().size(THUMB - 4.0).paint_at(ui, slot.shrink(2.0)),
            AttachState::Ready => {
                if let Some(tex) = textures(&ctx, &mut panel.rt, &format!("att:{}", a.path)).first() {
                    let [tw, th] = tex.size();
                    let (tw, th) = (tw.max(1) as f32, th.max(1) as f32);
                    let k = (THUMB / tw).min(THUMB / th);
                    paint_texture(ui, tex, Rect::from_center_size(slot.center(), vec2(tw * k, th * k)));
                }
            }
            AttachState::Failed => {
                ui.painter().text(slot.center(), egui::Align2::CENTER_CENTER, "!", font.clone(), t.danger);
            }
        }
        let name_pos = pos2(slot.max.x + GAP, chip.center().y - galley.size().y / 2.0);
        ui.painter().with_clip_rect(chip).galley(name_pos, galley, color);
        let resp = ui.interact(chip, egui::Id::unique(("merlin-chip", &a.path)), Sense::hover());
        auto.add(&format!("merlin.attach.{i}"), chip, &a.name);
        let tip = if a.state == AttachState::Loading { tr("Reading…").to_string() } else { format!("{}\n{}", a.path, a.info) };
        resp.on_hover_text(tip);
        let x_rect = Rect::from_min_size(pos2(chip.max.x - PAD - X_W, chip.min.y), vec2(X_W, row_h));
        let x_resp = ui.interact(x_rect, egui::Id::unique(("merlin-chip-x", &a.path)), Sense::click());
        let x_color = if x_resp.hovered() { t.text } else { t.text_dim };
        ui.painter().text(x_rect.center(), egui::Align2::CENTER_CENTER, "×", font.clone(), x_color);
        auto.add(&format!("merlin.attach.{i}.remove"), x_rect, "Remove attachment");
        if x_resp.on_hover_text(tr("Remove")).clicked() {
            acts.push(Act::Unattach(i));
        }
    }
    let used = Rect::from_min_max(pos2(left, top), pos2(right, y + CHIPS_H));
    ui.allocate_rect(used, Sense::hover());
    panel.rt.chips_h = used.height();
}

fn settings_ui(app: &mut EffectcraftApp, ui: &mut egui::Ui, r: Rect, acts: &mut Vec<Act>) {
    let t = app.tokens;
    let before = app.ui.merlin.settings.clone();
    let kind = before.kind();
    if app.ui.merlin.rt.key_status.as_ref().is_none_or(|(k, _, _)| *k != kind) {
        let (key, source) = if kind == ProviderKind::Demo { (None, KeySource::Missing) } else { keys::get(kind) };
        app.ui.merlin.rt.key_status = Some((kind, source, key.as_deref().map(keys::masked).unwrap_or_default()));
    }
    let mut c = child(ui, r.shrink(12.0));
    let EffectcraftApp { ui: state, auto, .. } = app;
    let panel = &mut state.merlin;
    let s = &mut panel.settings;
    egui::Grid::new("merlin-settings").num_columns(2).spacing(vec2(10.0, 8.0)).show(&mut c, |ui| {
        ui.label(RichText::new(tr("Provider")).color(t.text_dim));
        let resp = egui::ComboBox::from_id_salt("merlin-provider").selected_text(kind.label()).width(220.0).show_ui(ui, |ui| {
            for k in ProviderKind::ALL {
                if ui.selectable_label(k == kind, k.label()).clicked() {
                    s.provider = k.id().into();
                    s.model.clear();
                    s.base_url.clear();
                }
            }
        });
        auto.add("merlin.settings.provider", resp.response.rect, "Provider");
        ui.end_row();

        if kind != ProviderKind::Demo {
            ui.label(RichText::new(tr("Model")).color(t.text_dim));
            ui.horizontal(|ui| {
                let default = kind.presets().first().copied().unwrap_or("");
                let resp = ui.add(egui::TextEdit::singleline(&mut s.model).hint_text(default).desired_width(170.0));
                auto.add_response("merlin.settings.model", &resp, "Model");
                if !kind.presets().is_empty() {
                    let resp = egui::ComboBox::from_id_salt("merlin-presets").selected_text(tr("Presets")).width(90.0).show_ui(ui, |ui| {
                        for m in kind.presets() {
                            if ui.selectable_label(s.model == *m, *m).clicked() {
                                s.model = (*m).to_string();
                            }
                        }
                    });
                    auto.add("merlin.settings.presets", resp.response.rect, "Model presets");
                }
            });
            ui.end_row();

            ui.label(RichText::new(tr("API base URL")).color(t.text_dim));
            let resp = ui.add(egui::TextEdit::singleline(&mut s.base_url).hint_text(kind.default_base_url()).desired_width(260.0));
            auto.add_response("merlin.settings.baseUrl", &resp, "API base URL");
            ui.end_row();
        }

        if kind == ProviderKind::Anthropic {
            ui.label(RichText::new(tr("Effort")).color(t.text_dim));
            let shown = if s.effort.is_empty() { "high".to_string() } else { s.effort.clone() };
            let resp = egui::ComboBox::from_id_salt("merlin-effort").selected_text(&shown).width(120.0).show_ui(ui, |ui| {
                for e in ["low", "medium", "high", "xhigh", "max"] {
                    if ui.selectable_label(shown == e, e).clicked() {
                        s.effort = e.into();
                    }
                }
            });
            auto.add("merlin.settings.effort", resp.response.rect, "Effort");
            ui.end_row();
        }

        if kind != ProviderKind::Demo {
            ui.label(RichText::new(tr("API key")).color(t.text_dim));
            let status = match &panel.rt.key_status {
                Some((_, KeySource::Env, m)) => tr_args("From {} ({})", &[&kind.env_var(), m]),
                Some((_, KeySource::Keychain, m)) => tr_args("Stored in the keychain ({})", &[m]),
                _ if kind.needs_key() => tr("Not set").to_string(),
                _ => tr("Not needed for most local servers").to_string(),
            };
            ui.label(RichText::new(status).color(t.text));
            ui.end_row();

            ui.label("");
            ui.horizontal(|ui| {
                let resp = ui.add(egui::TextEdit::singleline(&mut panel.rt.key_draft).password(true).hint_text(tr("Paste a key")).desired_width(170.0));
                auto.add_response("merlin.settings.key", &resp, "API key");
                let save = ui.add_enabled(!panel.rt.key_draft.trim().is_empty(), egui::Button::new(tr("Save")));
                auto.add("merlin.settings.saveKey", save.rect, "Save key");
                if save.clicked() {
                    acts.push(Act::SaveKey);
                }
                let has = matches!(panel.rt.key_status, Some((_, KeySource::Keychain, _)));
                let remove = ui.add_enabled(has, egui::Button::new(tr("Remove")));
                auto.add("merlin.settings.removeKey", remove.rect, "Remove key");
                if remove.clicked() {
                    acts.push(Act::RemoveKey);
                }
            });
            ui.end_row();
        }
    });
    c.add_space(10.0);
    let note = if kind == ProviderKind::Demo {
        tr("Demo plays a scripted turn without calling any model: a quick way to see how Merlin works.")
    } else {
        tr("Keys are kept in the OS keychain, never in settings or project files. An environment variable overrides the stored key.")
    };
    c.add(egui::Label::new(RichText::new(note).small().color(t.text_faint)).wrap());
    if app.ui.merlin.settings != before {
        save_settings(app);
    }
}
