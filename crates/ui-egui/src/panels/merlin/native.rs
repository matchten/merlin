//! The desktop Merlin panel: turns run on a worker thread; their tool calls come back to the UI
//! thread as control requests, handled between frames by [`tick`] exactly as the control channel
//! handles an external agent's (so edits are ordinary, undoable engine commands).

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::Duration;

use effectcraft_engine::time::Tick;
use merlin_agent::agent_loop::{AgentEvent, TurnEnd, TurnOutcome, turn_label};
use merlin_agent::attachment::{self, Attachment};
use merlin_agent::checkpoint::Checkpoint;
use merlin_agent::conversation::{Message, Part};
use merlin_agent::guard::{self, Scope};
use merlin_agent::keys::{self, KeySource};
use merlin_agent::notes::{self as note_model, Note, NoteBrief, NoteStatus};
use merlin_agent::{AgentConfig, ProviderKind, RemoteHost, TurnContext, providers, run_turn_with, transport};
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

/// Messages from a turn's worker thread.
enum WorkerMsg {
    Event(AgentEvent),
    /// What the turn is waiting on, shown in the working line (`None`: back to normal).
    Status(Option<String>),
    Done {
        outcome: TurnOutcome,
        conversation: Vec<Message>,
    },
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
    status: Option<String>,
    /// The limits the message's locked pins set (each edit outside them is refused).
    scope: Option<Scope>,
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
    pub(super) checkpoints: Vec<(u64, Checkpoint)>,
    /// Images shown in the chat (PNG) and their textures: a tool call's rendered frames by call
    /// id, an attached file's picture by `att:<path>`.
    pub(super) frames: HashMap<String, Vec<Vec<u8>>>,
    pub(super) textures: HashMap<String, Vec<egui::TextureHandle>>,
    /// Key status for the settings view (provider, where it comes from, masked key).
    pub(super) key_status: Option<(ProviderKind, KeySource, String)>,
    /// The key being typed in settings: never serialized, cleared once saved.
    pub(super) key_draft: String,
    /// Decoded attachments of the message being written, by path.
    attachments: HashMap<String, Attachment>,
    attach_msgs: Option<(Sender<AttachMsg>, Receiver<AttachMsg>)>,
    /// Files are being dragged over the panel (set by the drop hooks, cleared once drawn).
    pub(super) drop_hover: bool,
    /// The attachment chips' height last frame (they wrap onto more rows in a narrow panel).
    pub(super) chips_h: f32,
    /// When each conversation item was first drawn (its fade-in), by index.
    pub(super) appear: HashMap<usize, f64>,
    /// When each attachment chip was first drawn, by path.
    pub(super) chip_appear: HashMap<String, f64>,
    /// The clock the chips' fade-in reads (the panel's last frame time).
    pub(super) now: f64,
    /// The storyboard's filmstrip and the render refreshing it.
    pub(super) film: Option<super::strip::Film>,
    pub(super) film_job: Option<super::strip::Pending>,
    pub(super) strip_drag: Option<super::strip::Drag>,
    /// The frame shown while marking a note's area, its render, and the box being dragged (comp px).
    pub(super) mark: Option<(super::strip::FilmKey, egui::TextureHandle)>,
    pub(super) mark_job: Option<super::strip::Pending>,
    pub(super) mark_drag: Option<egui::Pos2>,
    /// A note just added, whose text field takes the focus once.
    pub(super) focus_note: Option<u64>,
    /// The highlighted entry of the `@` list, and the `@` it was dismissed at (Escape).
    pub(super) mention_sel: usize,
    pub(super) mention_off: Option<usize>,
    /// The pin chips' height last frame (they wrap in a narrow panel).
    pub(super) pins_h: f32,
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
    pub(super) fn kind(&self) -> ProviderKind {
        ProviderKind::ALL.into_iter().find(|k| k.id() == self.provider).unwrap_or_default()
    }

    pub(super) fn config(&self) -> AgentConfig {
        let mut c = AgentConfig { provider: self.kind(), model: self.model.clone(), base_url: self.base_url.clone(), ..Default::default() };
        if !self.effort.is_empty() {
            c.effort = self.effort.clone();
        }
        c
    }
}

/// Load the saved settings once.
pub(super) fn ensure_loaded(app: &mut EffectcraftApp) {
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

pub(super) fn save_settings(app: &mut EffectcraftApp) {
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

/// A comp time as the panel shows it: the project's display style (`0:00:01:12`, or frames).
pub(super) fn display_time(app: &EffectcraftApp, comp: u64, t: Tick) -> String {
    match app.session.project.comp(effectcraft_engine::project::ItemId(comp)) {
        Some(c) => crate::panels::timecode(&app.session, c, t),
        None => format!("{:.2}s", t.seconds()),
    }
}

/// A comp time as the agent reads it: the display, the frame and the seconds.
fn agent_time(app: &EffectcraftApp, comp: u64, t: Tick) -> String {
    let frame = app.session.project.comp(effectcraft_engine::project::ItemId(comp)).map(|c| c.frame_rate.frame_at(t)).unwrap_or(0);
    note_model::time_label(&display_time(app, comp, t), frame, t.seconds())
}

/// Notes ready to apply: open, written, and on a comp that still exists.
pub(super) fn ready_notes(app: &EffectcraftApp) -> usize {
    app.ui.merlin.notes.iter().filter(|n| is_ready(app, n)).count()
}

fn is_ready(app: &EffectcraftApp, n: &Note) -> bool {
    n.status == NoteStatus::Open && !n.text.trim().is_empty() && app.session.project.comp(effectcraft_engine::project::ItemId(n.comp)).is_some()
}

/// Every note on an existing comp, spelled out for the agent; the ready ones are sent.
fn note_briefs(app: &EffectcraftApp) -> Vec<NoteBrief> {
    app.ui
        .merlin
        .notes
        .iter()
        .filter_map(|n| {
            let comp = app.session.project.comp(effectcraft_engine::project::ItemId(n.comp))?;
            let layers =
                n.layers.iter().filter_map(|id| comp.layer(effectcraft_engine::project::LayerId(*id)).map(|l| format!("\"{}\" (id {id})", l.name))).collect();
            Some(NoteBrief {
                note: n.clone(),
                at: agent_time(app, n.comp, n.start),
                until: n.end.filter(|e| *e != n.start).map(|e| agent_time(app, n.comp, e)),
                layers,
                comp_size: (comp.width, comp.height),
                send: is_ready(app, n),
            })
        })
        .collect()
}

/// A new note at `start` (a range to `end`) on the active comp, about the selected layers; its
/// editor opens. `None` without a comp.
pub(super) fn add_note(app: &mut EffectcraftApp, start: Tick, end: Option<Tick>, text: &str) -> Option<u64> {
    let comp = app.session.active_comp_id()?;
    let panel = &mut app.ui.merlin;
    let id = panel.notes.iter().map(|n| n.id).max().unwrap_or(0).saturating_add(1);
    let end = end.filter(|e| *e != start);
    let (start, end) = match end {
        Some(e) if e < start => (e, Some(start)),
        _ => (start, end),
    };
    let layers = app.session.state.selected_layers.iter().map(|l| l.0).collect();
    panel.notes.push(Note { id, comp: comp.0, start, end, layers, text: text.to_string(), ..Note::default() });
    panel.strip_open = true;
    panel.editing_note = Some(id);
    panel.rt.focus_note = Some(id);
    panel.settings_open = false;
    Some(id)
}

pub(super) fn remove_note(app: &mut EffectcraftApp, id: u64) {
    let p = &mut app.ui.merlin;
    p.notes.retain(|n| n.id != id);
    if p.editing_note == Some(id) {
        p.editing_note = None;
    }
    if p.marking_note == Some(id) {
        p.marking_note = None;
    }
    for k in [format!("note-before:{id}"), format!("note-after:{id}")] {
        p.rt.frames.remove(&k);
        p.rt.textures.remove(&k);
    }
}

/// Send the typed message, with the notes ready to apply, as a new turn.
pub(super) fn start_turn(app: &mut EffectcraftApp, ctx: &egui::Context) {
    let text = app.ui.merlin.input.trim().to_string();
    if !can_send(app) || app.ui.merlin.rt.worker.is_some() {
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
    let briefs = note_briefs(app);
    let note_ids: Vec<u64> = briefs.iter().filter(|b| b.send).map(|b| b.note.id).collect();
    let time = app.session.active_comp_id().map(|c| agent_time(app, c.0, app.session.time()));
    let (pins, scope) = super::mentions::briefs(app);
    let turn_ctx = TurnContext { time, notes: briefs, pins };
    let panel = &mut app.ui.merlin;
    let pin_labels: Vec<String> = std::mem::take(&mut panel.pins).into_iter().map(|p| if p.locked { format!("🔒 {}", p.label) } else { p.label }).collect();
    panel.rt.mention_off = None;
    for n in panel.notes.iter_mut().filter(|n| note_ids.contains(&n.id)) {
        n.status = NoteStatus::Sent;
    }
    if note_ids.contains(&panel.editing_note.unwrap_or(0)) {
        panel.editing_note = None;
    }
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
    let request = if !text.is_empty() {
        text.clone()
    } else if !note_ids.is_empty() {
        tr_args("Apply {} notes", &[&note_ids.len()])
    } else {
        sent.iter().map(|a| a.name.as_str()).collect::<Vec<_>>().join(", ")
    };
    panel.items.push(ChatItem::User { text, attachments: sent.into_iter().map(|a| a.path).collect(), notes: note_ids, pins: pin_labels });
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
        let key = if cfg.provider.needs_key() || cfg.provider == ProviderKind::Custom {
            // macOS may show a keychain permission dialog first (again after each app update).
            let _ = ev_tx.send(WorkerMsg::Status(Some(tr("Reading the API key from the keychain… if macOS asks, allow access.").to_string())));
            ctx_ev.request_repaint();
            let key = keys::get(cfg.provider).0.unwrap_or_default();
            let _ = ev_tx.send(WorkerMsg::Status(None));
            key
        } else {
            String::new()
        };
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
        let outcome = run_turn_with(
            provider.as_ref(),
            &mut host,
            &cfg,
            &mut conversation,
            parts,
            &turn_ctx,
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
            rt.worker = Some(Worker { turn, request, checkpoint, cancel, rx: ev_rx, stopping_since: None, tool_calls: 0, status: None, scope });
            panel.busy = true;
        }
        Err(e) => notice(app, tr_args("Merlin couldn't start: {}", &[&e]), true),
    }
}

/// Whether the message can be sent: words, ready attachments or notes to apply, and no attachment
/// still loading.
pub(super) fn can_send(app: &EffectcraftApp) -> bool {
    let panel = &app.ui.merlin;
    let loading = panel.attachments.iter().any(|a| a.state == AttachState::Loading);
    let ready = panel.attachments.iter().any(|a| a.state == AttachState::Ready);
    !loading && (ready || !panel.input.trim().is_empty() || ready_notes(app) > 0)
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

/// `ui.set {"merlin": {...}}` from the control channel, for agents and scripts that can't drag or
/// click:
/// * `attach`: absolute paths, attached as a drop would;
/// * `note`: `{time, end?, text, layers?, region?}` (comp seconds, layer ids, `[x, y, w, h]` comp
///   pixels) adds a note on the active comp, the selected layers unless `layers` is given;
/// * `removeNote`: a note id;
/// * `pin`: `{layer | frame | range | note, locked?}` (layer id, comp seconds, `[s, s]`, note id)
///   pins the next message, as an `@` mention does;
/// * `applyNotes`: `true` sends the typed message and the notes ready to apply.
pub fn apply_ui_set(app: &mut EffectcraftApp, ctx: &egui::Context, v: &Value) -> Result<(), String> {
    if let Some(n) = v.get("note") {
        let secs = |k: &str| n.get(k).and_then(Value::as_f64).map(Tick::from_seconds_f64);
        let start = secs("time").ok_or("merlin.note: `time` (comp seconds) is required")?;
        let text = n.get("text").and_then(Value::as_str).unwrap_or_default();
        let id = add_note(app, start, secs("end"), text).ok_or("merlin.note: no composition is open")?;
        if let Some(note) = app.ui.merlin.notes.iter_mut().find(|x| x.id == id) {
            if let Some(ls) = n.get("layers").and_then(Value::as_array) {
                note.layers = ls.iter().filter_map(Value::as_u64).collect();
            }
            if let Some([x, y, w, h]) = n
                .get("region")
                .and_then(Value::as_array)
                .and_then(|r| r.iter().map(|v| v.as_f64().map(|f| f as f32)).collect::<Option<Vec<_>>>())
                .and_then(|r| <[f32; 4]>::try_from(r).ok())
            {
                note.region = Some([x, y, w, h]);
            }
        }
    }
    if let Some(p) = v.get("pin") {
        let c = app.session.active_comp_id().ok_or("merlin.pin: no composition is open")?.0;
        let secs = |v: &Value| v.as_f64().map(Tick::from_seconds_f64);
        let kind = if let Some(id) = p.get("layer").and_then(Value::as_u64) {
            super::PinKind::Layer { id }
        } else if let Some(t) = p.get("frame").and_then(secs) {
            super::PinKind::Frame { at: t }
        } else if let Some([a, b]) = p.get("range").and_then(Value::as_array).map(Vec::as_slice) {
            let (a, b) = (secs(a).ok_or("merlin.pin.range: seconds")?, secs(b).ok_or("merlin.pin.range: seconds")?);
            super::PinKind::Range { start: a.min(b), end: a.max(b) }
        } else if let Some(id) = p.get("note").and_then(Value::as_u64) {
            super::PinKind::Note { id }
        } else {
            return Err("merlin.pin: give layer (id), frame (s), range ([s, s]) or note (id)".into());
        };
        let label = match &kind {
            super::PinKind::Layer { id } => app
                .session
                .active_comp()
                .and_then(|cp| cp.layer(effectcraft_engine::project::LayerId(*id)))
                .map(|l| l.name.clone())
                .ok_or("merlin.pin: no such layer")?,
            super::PinKind::Frame { at } => display_time(app, c, *at),
            super::PinKind::Range { start, end } => format!("{}–{}", display_time(app, c, *start), display_time(app, c, *end)),
            super::PinKind::Note { id } => format!("Note {id}"),
        };
        let locked = p.get("locked").and_then(Value::as_bool).unwrap_or(kind.lockable());
        super::mentions::add(app, super::Pin { comp: c, kind, label, locked });
    }
    if let Some(id) = v.get("removeNote").and_then(Value::as_u64) {
        remove_note(app, id);
    }
    if v.get("applyNotes") == Some(&Value::Bool(true)) {
        if app.ui.merlin.rt.worker.is_some() {
            return Err("merlin.applyNotes: a turn is already running".into());
        }
        if !can_send(app) {
            return Err("merlin.applyNotes: nothing to send (no written open notes or message)".into());
        }
        start_turn(app, ctx);
    }
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
pub(super) fn pick_attachments(app: &mut EffectcraftApp, ctx: &egui::Context) {
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

pub(super) fn stop_turn(app: &mut EffectcraftApp, now: f64) {
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
        AgentEvent::NoteFrame { id, png } => {
            let key = format!("note-before:{id}");
            panel.rt.textures.remove(&key);
            panel.rt.frames.insert(key, vec![png]);
        }
        AgentEvent::NoteResolved { id, addressed, summary, frame } => {
            if let Some(n) = panel.notes.iter_mut().find(|n| n.id == id) {
                n.status = if addressed { NoteStatus::Addressed { summary } } else { NoteStatus::WontFix { summary } };
            }
            let key = format!("note-after:{id}");
            panel.rt.textures.remove(&key);
            if let Some(png) = frame {
                panel.rt.frames.insert(key, vec![png]);
            }
        }
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
    // Notes the turn never reported on can be applied again.
    for n in panel.notes.iter_mut().filter(|n| n.status == NoteStatus::Sent) {
        n.status = NoteStatus::Open;
    }
}

pub(super) fn decide(app: &mut EffectcraftApp, ctx: &egui::Context, turn: u64, keep: bool) {
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

/// After an edit while pins are locked: undo it and answer with an error naming what went outside,
/// or pass the reply through when it stayed inside.
fn enforce(app: &mut EffectcraftApp, scope: &Scope, cp: &Checkpoint, reply: Value) -> Value {
    let comp = scope.comp;
    let when = |t: Tick| display_time(app, comp, t);
    let out = guard::check(cp.snapshot(), &app.session.project, scope, &when);
    if out.is_empty() {
        return reply;
    }
    let undone = cp.discard(&mut app.session);
    let mut msg = String::from("Refused: this edit went outside what the user pinned and locked, so ");
    msg.push_str(if undone { "none of it was applied" } else { "it could not be undone automatically; tell the user" });
    msg.push_str(":\n");
    for o in &out {
        msg.push_str(&format!("- {o}\n"));
    }
    msg.push_str("Make the change within the locked pins, or tell the user it needs them unlocked.");
    json!({"ok": false, "error": msg})
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
        let scope = app.ui.merlin.rt.worker.as_ref().and_then(|w| w.scope.clone());
        for (req, deadline) in reqs {
            let reply = req.reply.clone();
            let guarded = scope.as_ref().filter(|_| req.method == "engine.execute").map(|sc| (sc, Checkpoint::take(&app.session)));
            match control::handle(app, ctx, &req, now) {
                Outcome::Done(v) => {
                    let v = match guarded {
                        Some((sc, cp)) if cp.changed(&app.session) => enforce(app, sc, &cp, v),
                        _ => v,
                    };
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
    let Some(mut w) = app.ui.merlin.rt.worker.take() else { return };
    let mut done = None;
    let mut events = vec![];
    while let Ok(m) = w.rx.try_recv() {
        match m {
            WorkerMsg::Event(e) => events.push(e),
            WorkerMsg::Status(s) => w.status = s,
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

// ------------------------------------------------------------------ view support

/// Start over (not while a turn runs): the attachments being written stay.
pub(super) fn new_chat(app: &mut EffectcraftApp) {
    if app.ui.merlin.rt.worker.is_some() {
        return;
    }
    let p = &mut app.ui.merlin;
    p.items.clear();
    p.rt.conversation.clear();
    p.rt.appear.clear();
    p.rt.frames.retain(|k, _| p.attachments.iter().any(|a| *k == format!("att:{}", a.path)));
    p.rt.textures.clear();
}

pub(super) fn unattach(app: &mut EffectcraftApp, i: usize) {
    let p = &mut app.ui.merlin;
    if i < p.attachments.len() {
        let a = p.attachments.remove(i);
        p.rt.attachments.remove(&a.path);
        p.rt.chip_appear.remove(&a.path);
    }
}

pub(super) fn save_or_remove_key(app: &mut EffectcraftApp, save: bool) {
    let p = &mut app.ui.merlin;
    let kind = p.settings.kind();
    let r = if save { keys::set(kind, &p.rt.key_draft) } else { keys::remove(kind) };
    p.rt.key_draft.clear();
    p.rt.key_status = None;
    if let Err(e) = r {
        app.ui.status = e.to_string();
    }
}

/// Read where the key for `kind` comes from (once per provider shown).
pub(super) fn refresh_key_status(app: &mut EffectcraftApp, kind: ProviderKind) {
    if app.ui.merlin.rt.key_status.as_ref().is_none_or(|(k, _, _)| *k != kind) {
        let (key, source) = if kind == ProviderKind::Demo { (None, KeySource::Missing) } else { keys::get(kind) };
        app.ui.merlin.rt.key_status = Some((kind, source, key.as_deref().map(keys::masked).unwrap_or_default()));
    }
}

/// What the running turn is doing, for the working line (`None`: no turn).
pub(super) fn working_status(rt: &Runtime) -> Option<String> {
    let w = rt.worker.as_ref()?;
    Some(match (&w.stopping_since, &w.status) {
        (Some(_), _) => tr("Stopping…").to_string(),
        (None, Some(s)) => s.clone(),
        (None, None) => tr("Working… edits appear live").to_string(),
    })
}

/// A chip's fade-in, 0 → 1 over a fifth of a second from when it was first drawn.
pub(super) fn chip_appear(ctx: &egui::Context, rt: &mut Runtime, path: &str) -> f32 {
    let now = rt.now;
    let start = *rt.chip_appear.entry(path.to_string()).or_insert(now);
    let k = ((now - start) / 0.2).clamp(0.0, 1.0) as f32;
    if k < 1.0 {
        ctx.request_repaint();
    }
    k
}
