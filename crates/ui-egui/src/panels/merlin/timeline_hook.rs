//! Notes from the Timeline. With the Timeline or the Composition viewer focused, `@` drops a note at
//! the playhead into the Merlin panel. While it glows, moving the playhead (dragging it, or
//! Shift+←/→ a frame at a time) stretches it into a range; Enter starts writing it, Escape drops an
//! empty one. Notes show on the Timeline's ruler, and "Ask Merlin…" in the layer and keyframe menus
//! starts a note about them.

use effectcraft_engine::project::{Comp, LayerId};
use effectcraft_engine::time::Tick;
use egui::{Rect, Sense, Stroke, pos2, vec2};
use merlin_agent::notes::NoteStatus;
use serde_json::Value;

use super::frames;
use super::native;
use super::note_tabs;
use crate::EffectcraftApp;
use crate::dock::PanelKind;
use crate::i18n::tr_args;
use crate::panels::timeline::TMap;

/// Before the shortcuts run (each frame): `@` starts a note; while one is being stretched, Shift+←/→
/// step the playhead (instead of nudging layers), Enter and Escape finish it.
pub fn before_shortcuts(app: &mut EffectcraftApp, ctx: &egui::Context) {
    stretch(app, ctx);
    if ctx.egui_wants_keyboard_input() || crate::widgets::any_menu_open(ctx) || app.dialog.is_some() {
        return;
    }
    if !matches!(app.ui.focused, PanelKind::Timeline | PanelKind::Composition) {
        return;
    }
    let at = ctx.input_mut(|i| {
        let n = i.events.len();
        i.events.retain(|e| !matches!(e, egui::Event::Text(t) if t == "@"));
        n != i.events.len()
    });
    if at {
        start(app);
    }
}

/// Start a note at the playhead, glowing until it is finished.
pub(super) fn start(app: &mut EffectcraftApp) -> Option<u64> {
    let now = app.session.time();
    let id = native::add_note(app, now, None, "")?;
    // Writing waits for Enter: the arrow keys stretch it first.
    app.ui.merlin.rt.stretching = Some((id, now, now));
    app.show_panel(PanelKind::Merlin);
    Some(id)
}

/// While a note is being stretched: follow the playhead, take Shift+←/→, Enter and Escape.
fn stretch(app: &mut EffectcraftApp, ctx: &egui::Context) {
    let Some((id, anchor, last)) = app.ui.merlin.rt.stretching else { return };
    if !app.ui.merlin.notes.iter().any(|n| n.id == id && n.status == NoteStatus::Open) {
        app.ui.merlin.rt.stretching = None;
        return;
    }
    let typing = ctx.egui_wants_keyboard_input();
    let (left, right, enter, esc) = if typing {
        (false, false, false, false)
    } else {
        ctx.input_mut(|i| {
            (
                i.consume_key(egui::Modifiers::SHIFT, egui::Key::ArrowLeft),
                i.consume_key(egui::Modifiers::SHIFT, egui::Key::ArrowRight),
                i.consume_key(egui::Modifiers::NONE, egui::Key::Enter),
                i.consume_key(egui::Modifiers::NONE, egui::Key::Escape),
            )
        })
    };
    if (left || right)
        && let Some(c) = app.session.active_comp()
    {
        let fr = c.frame_rate;
        let f = fr.frame_at(app.session.time()) + if right { 1 } else { -1 };
        let last_frame = fr.frame_at(c.duration).saturating_sub(1).max(0);
        app.session.set_time(fr.tick_of(f.clamp(0, last_frame)));
    }
    let now = app.session.time();
    let clock = app.now();
    if now != last {
        if let Some(n) = app.ui.merlin.notes.iter_mut().find(|n| n.id == id) {
            n.start = anchor.min(now);
            n.end = (now != anchor).then_some(anchor.max(now));
        }
        app.ui.merlin.rt.stretching = Some((id, anchor, now));
        app.ui.merlin.rt.stretch_moved = clock;
    } else if clock - app.ui.merlin.rt.stretch_moved >= SNAP_REST_SECS {
        // The playhead rested: render the frame(s) now, so they're there when the note opens.
        frames::request_snapshot(app, id);
    }
    if enter {
        finish(app, id, true);
    } else if esc {
        let empty = app.ui.merlin.notes.iter().any(|n| n.id == id && n.text.trim().is_empty() && n.end.is_none());
        app.ui.merlin.rt.stretching = None;
        if empty {
            native::remove_note(app, id);
        } else {
            frames::request_snapshot(app, id);
        }
    }
    super::view::tick(ctx);
}

/// How long the playhead rests while stretching before the note's frames render.
const SNAP_REST_SECS: f64 = 0.15;

/// Done stretching: snapshot the frame(s) and (`write`) open the note for typing.
fn finish(app: &mut EffectcraftApp, id: u64, write: bool) {
    app.ui.merlin.rt.stretching = None;
    frames::request_snapshot(app, id);
    if write {
        app.ui.merlin.open_notes.insert(id);
        app.ui.merlin.rt.focus_note = Some(id);
    }
}

/// Notes on the Timeline's ruler (`strip`: the marker strip along its bottom): a numbered pin in
/// each note's colour at its frame and a thin bar over a range, washed over only while the note is
/// stretched, open or under the pointer; a click opens the note in the panel.
pub(crate) fn ruler_notes(app: &mut EffectcraftApp, ui: &mut egui::Ui, tm: &TMap, strip: Rect) {
    let Some(cid) = app.session.active_comp_id() else { return };
    let panel = &app.ui.merlin;
    if !panel.notes.iter().any(|n| n.comp == cid.0 && !n.status.is_done()) {
        return;
    }
    let magic = egui::Color32::from_rgb(0xa7, 0x8b, 0xfa);
    let stretching = panel.rt.stretching.map(|s| s.0);
    let clip = strip.intersect(ui.clip_rect());
    let p = ui.painter().with_clip_rect(clip);
    let now = app.now();
    let mut clicked = None;
    let mut hovered = None;
    for n in panel.notes.iter().filter(|n| n.comp == cid.0 && !n.status.is_done()) {
        let base = if n.author == merlin_agent::notes::Author::Merlin { magic } else { note_tabs::hue(n.id) };
        let glow = if stretching == Some(n.id) { 0.6 + 0.4 * (0.5 + 0.5 * (now * 5.0).sin()) as f32 } else { 1.0 };
        let sent = n.status == NoteStatus::Sent;
        let col = base.gamma_multiply(glow * if sent { 0.5 } else { 1.0 });
        let lit = stretching == Some(n.id) || panel.open_notes.contains(&n.id) || panel.rt.hover_note == Some(n.id);
        let (s, e) = n.span();
        let (x0, x1) = (tm.x(s.seconds()), tm.x(e.seconds()));
        if e > s {
            if lit {
                p.rect_filled(Rect::from_min_max(pos2(x0, strip.min.y), pos2(x1, strip.max.y)), 0.0, base.gamma_multiply(0.14));
            }
            p.rect_filled(Rect::from_min_max(pos2(x0, strip.max.y - 3.0), pos2(x1.max(x0 + 2.0), strip.max.y)), 1.5, col);
        }
        let r = (strip.height() * 0.32).clamp(4.5, 6.5);
        let head = pos2(x0, strip.min.y + r + 0.5);
        p.line_segment([head, pos2(x0, strip.max.y)], Stroke::new(if lit { 1.5 } else { 1.0 }, col));
        note_tabs::badge(&p, head, n.id, col, r);
        let hit = Rect::from_center_size(pos2(x0, strip.center().y), vec2(2.0 * r + 4.0, strip.height().max(10.0)));
        let resp = ui.interact(hit, egui::Id::unique(("merlin-ruler-note", n.id)), Sense::click());
        app.auto.add(&format!("merlin.ruler.note.{}", n.id), hit, &n.text);
        if resp.hovered() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
            hovered = Some(n.id);
        }
        let first = n.text.lines().find(|l| !l.trim().is_empty()).map(str::trim).unwrap_or("");
        let tip = if first.is_empty() { tr_args("Note {} (empty)", &[&n.id]) } else { format!("{} · {first}", n.id) };
        if resp.on_hover_text(tip).clicked() {
            clicked = Some(n.id);
        }
    }
    if hovered.is_some() {
        app.ui.merlin.rt.hover_next = hovered;
    }
    if stretching.is_some() {
        super::view::tick(ui.ctx());
    }
    if let Some(id) = clicked {
        open(app, id);
    }
}

/// Open note `id` in the panel (expanded, at its time).
pub(super) fn open(app: &mut EffectcraftApp, id: u64) {
    let Some(n) = app.ui.merlin.notes.iter().find(|n| n.id == id) else { return };
    let start = n.start;
    app.ui.merlin.open_notes.insert(id);
    app.ui.merlin.settings_open = false;
    app.session.set_time(start);
    app.show_panel(PanelKind::Merlin);
}

/// `merlin.ask {layers?, prop?, time?}` (Ask Merlin… in the layer and keyframe menus): a note about
/// those layers and that property, at the keyframe's time or the playhead, open for typing.
pub fn ask(app: &mut EffectcraftApp, params: &Value) -> Result<Value, String> {
    let comp: Comp = app.session.active_comp().cloned().ok_or("no composition is open")?;
    let mut layers: Vec<u64> = params.get("layers").and_then(Value::as_array).map(|a| a.iter().filter_map(Value::as_u64).collect()).unwrap_or_default();
    if let Some(l) = params.get("layer").and_then(Value::as_u64) {
        layers.push(l);
    }
    layers.dedup();
    // A keyframe's time is layer time.
    let at = match (params.get("time").and_then(Value::as_f64), layers.first().and_then(|l| comp.layer(LayerId(*l)))) {
        (Some(secs), Some(layer)) => layer.comp_time(Tick::from_seconds_f64(secs)),
        _ => app.session.time(),
    };
    let props: Vec<String> = match (params.get("prop").and_then(Value::as_u64), layers.first().and_then(|l| comp.layer(LayerId(*l)))) {
        (Some(uid), Some(layer)) => layer.props.find(uid).map(|p| format!("{} ▸ {}", layer.name, p.name)).into_iter().collect(),
        _ => vec![],
    };
    let id = native::add_note(app, at, None, "").ok_or("no composition is open")?;
    if let Some(n) = app.ui.merlin.notes.iter_mut().find(|n| n.id == id) {
        if !layers.is_empty() {
            n.layers = layers;
        }
        n.props = props;
    }
    app.ui.merlin.open_notes.insert(id);
    app.ui.merlin.rt.focus_note = Some(id);
    frames::request_snapshot(app, id);
    app.show_panel(PanelKind::Merlin);
    Ok(serde_json::json!({"note": id}))
}
