//! `@` mentions: pin the message to a layer, a frame, a time range, a marker or a note, the way a
//! coding agent's `@file` pins a file. Typing `@` opens a list above the composer (the current
//! frame, a typed time like `@1:12`, `@f42` or `@1:00-2:12`, the comp's layers, markers and notes);
//! the context chips pin the current frame or selected layers in one click. Layer and range pins
//! start locked: Merlin's edits must stay inside them (see `merlin_agent::guard`).

use effectcraft_engine::project::{ItemId, LayerId};
use effectcraft_engine::time::Tick;
use egui::{Align2, Color32, Rect, Sense, Stroke, StrokeKind, pos2, vec2};
use merlin_agent::PinBrief;
use merlin_agent::guard::Scope;
use merlin_agent::notes::time_label;

use super::native::display_time;
use super::view::{Look, lerp, short};
use super::{Pin, PinKind};
use crate::EffectcraftApp;
use crate::i18n::tr;
use crate::icons::{self, Icon};
use crate::theme::Tokens;

/// Entries in the mention list.
const MAX_CANDIDATES: usize = 8;
/// A pin chip's height.
pub(super) const PIN_H: f32 = 22.0;

/// The `@query` being typed at the end of the message: its byte offset and the query.
pub(super) fn query(input: &str) -> Option<(usize, &str)> {
    let at = input.rfind('@')?;
    let before = input.get(..at)?;
    if !(before.is_empty() || before.ends_with(char::is_whitespace)) {
        return None;
    }
    let q = input.get(at + 1..)?;
    if q.contains(char::is_whitespace) || q.chars().count() > 40 {
        return None;
    }
    Some((at, q))
}

/// A typed time (`1:12`, `112`, `f42`) or range (`1:00-2:12`) on the active comp.
fn typed_time(app: &EffectcraftApp, q: &str) -> Option<PinKind> {
    let comp = app.session.active_comp()?;
    let cur = comp.frame_rate.frame_at(app.session.time());
    let one = |s: &str| -> Option<Tick> {
        let s = s.trim();
        let frame = match s.strip_prefix(['f', 'F']) {
            Some(n) => n.parse::<i64>().ok()?,
            None if s.chars().next().is_some_and(|c| c.is_ascii_digit()) => {
                effectcraft_engine::commands::time::parse_display_time(&app.session, comp, s, cur).ok()?
            }
            None => return None,
        };
        let last = comp.frame_rate.frame_at(comp.duration).saturating_sub(1).max(0);
        Some(comp.frame_rate.tick_of(frame.clamp(0, last)))
    };
    match q.split_once(['-', '–']) {
        Some((a, b)) if !a.is_empty() && !b.is_empty() => {
            let (a, b) = (one(a)?, one(b)?);
            Some(PinKind::Range { start: a.min(b), end: a.max(b) })
        }
        _ => one(q).map(|at| PinKind::Frame { at }),
    }
}

/// The chip label of a pin of `kind` on the active comp.
fn label_of(app: &EffectcraftApp, comp: u64, kind: &PinKind) -> String {
    match kind {
        PinKind::Frame { at } => display_time(app, comp, *at),
        PinKind::Range { start, end } => format!("{}–{}", display_time(app, comp, *start), display_time(app, comp, *end)),
        PinKind::Layer { id } => app.session.project.comp(ItemId(comp)).and_then(|c| c.layer(LayerId(*id))).map(|l| l.name.clone()).unwrap_or_default(),
        PinKind::Note { id } => format!("Note {id}"),
    }
}

fn pin(app: &EffectcraftApp, comp: u64, kind: PinKind) -> Pin {
    let label = label_of(app, comp, &kind);
    Pin { comp, locked: kind.lockable(), kind, label }
}

/// What `@q` could pin, best first: a typed time, the current frame, layers, markers, notes.
pub(super) fn candidates(app: &EffectcraftApp, q: &str) -> Vec<(Pin, String)> {
    let Some(cid) = app.session.active_comp_id() else { return vec![] };
    let Some(comp) = app.session.active_comp() else { return vec![] };
    let c = cid.0;
    let ql = q.to_lowercase();
    let mut out: Vec<(Pin, String)> = vec![];
    if let Some(kind) = typed_time(app, q) {
        let what = if matches!(kind, PinKind::Range { .. }) { tr("Time range") } else { tr("Frame") };
        out.push((pin(app, c, kind), what.to_string()));
    }
    if q.is_empty() || "frame".starts_with(&ql) || "now".starts_with(&ql) {
        out.push((pin(app, c, PinKind::Frame { at: app.session.time() }), tr("Current frame").to_string()));
    }
    for l in &comp.layers {
        if l.name.to_lowercase().contains(&ql) {
            out.push((pin(app, c, PinKind::Layer { id: l.id.0 }), tr("Layer").to_string()));
        }
    }
    for m in &comp.markers {
        let name = if m.comment.is_empty() { tr("Marker").to_string() } else { m.comment.clone() };
        if name.to_lowercase().contains(&ql) || ql.is_empty() {
            let kind = if m.duration > Tick::ZERO { PinKind::Range { start: m.time, end: m.time + m.duration } } else { PinKind::Frame { at: m.time } };
            let mut p = pin(app, c, kind);
            p.label = format!("{} ({})", short(&name, 20), p.label);
            out.push((p, tr("Marker").to_string()));
        }
    }
    for n in app.ui.merlin.notes.iter().filter(|n| n.comp == c) {
        if format!("note {}", n.id).contains(&ql) || n.text.to_lowercase().contains(&ql) {
            out.push((pin(app, c, PinKind::Note { id: n.id }), short(n.text.trim(), 28)));
        }
    }
    out.truncate(MAX_CANDIDATES);
    out
}

/// Pin `p` (once), replacing the `@query` being typed with its name; typing goes on after it.
pub(super) fn accept(app: &mut EffectcraftApp, ctx: &egui::Context, p: Pin) {
    let panel = &mut app.ui.merlin;
    if let Some((at, _)) = query(&panel.input) {
        panel.input.truncate(at);
        let token: String = p.label.chars().filter(|c| !c.is_whitespace()).take(24).collect();
        panel.input.push_str(&format!("@{token} "));
        let id = egui::Id::unique("merlin-input");
        let mut state = egui::TextEdit::load_state(ctx, id).unwrap_or_default();
        state.cursor.set_char_range(Some(egui::text::CCursorRange::one(egui::text::CCursor::new(panel.input.chars().count()))));
        state.store(ctx, id);
    }
    add(app, p);
}

/// Pin `p` unless an equal pin is there already.
pub(super) fn add(app: &mut EffectcraftApp, p: Pin) {
    let pins = &mut app.ui.merlin.pins;
    if !pins.iter().any(|x| x.comp == p.comp && x.kind == p.kind) {
        pins.push(p);
    }
}

pub(super) fn pin_current_frame(app: &mut EffectcraftApp) {
    let Some(c) = app.session.active_comp_id() else { return };
    let p = pin(app, c.0, PinKind::Frame { at: app.session.time() });
    add(app, p);
}

pub(super) fn pin_selected_layers(app: &mut EffectcraftApp) {
    let Some(c) = app.session.active_comp_id() else { return };
    let ids: Vec<u64> = app.session.state.selected_layers.iter().map(|l| l.0).collect();
    for id in ids {
        let p = pin(app, c.0, PinKind::Layer { id });
        add(app, p);
    }
}

/// The pins as the agent reads them, and the limits the locked ones set on the active comp.
pub(super) fn briefs(app: &EffectcraftApp) -> (Vec<PinBrief>, Option<Scope>) {
    let agent_time = |comp: u64, t: Tick| {
        let frame = app.session.project.comp(ItemId(comp)).map(|c| c.frame_rate.frame_at(t)).unwrap_or(0);
        time_label(&display_time(app, comp, t), frame, t.seconds())
    };
    let active = app.session.active_comp_id().map(|c| c.0);
    let mut scope = Scope { comp: active.unwrap_or(0), ..Scope::default() };
    let mut out = vec![];
    for p in &app.ui.merlin.pins {
        let lock = if p.locked && p.kind.lockable() { ", locked: your edits must stay inside it" } else { "" };
        let in_active = Some(p.comp) == active;
        let b = match &p.kind {
            PinKind::Layer { id } => {
                if p.locked && in_active {
                    scope.layers.get_or_insert_default().insert(*id);
                }
                PinBrief { line: format!("layer \"{}\" (id {id}){lock}", p.label), frames: vec![] }
            }
            PinKind::Frame { at } => PinBrief {
                line: format!("the frame at {}", agent_time(p.comp, *at)),
                frames: vec![(p.comp, *at, format!("Pinned frame at {}", display_time(app, p.comp, *at)))],
            },
            PinKind::Range { start, end } => {
                if p.locked && in_active {
                    let r = scope.range.get_or_insert((*start, *end));
                    *r = (r.0.min(*start), r.1.max(*end));
                }
                PinBrief {
                    line: format!("the time range {} to {}{lock}", agent_time(p.comp, *start), agent_time(p.comp, *end)),
                    frames: vec![
                        (p.comp, *start, format!("Pinned range start, {}", display_time(app, p.comp, *start))),
                        (p.comp, *end, format!("Pinned range end, {}", display_time(app, p.comp, *end))),
                    ],
                }
            }
            PinKind::Note { id } => match app.ui.merlin.notes.iter().find(|n| n.id == *id) {
                Some(n) => PinBrief {
                    line: format!("note {id} at {}: \"{}\"", agent_time(n.comp, n.start), n.text.trim()),
                    frames: vec![(n.comp, n.start, format!("Pinned note {id}'s frame, {}", display_time(app, n.comp, n.start)))],
                },
                None => continue,
            },
        };
        out.push(b);
    }
    (out, if scope.is_empty() || active.is_none() { None } else { Some(scope) })
}

enum PinAct {
    Lock(usize),
    Remove(usize),
}

/// The pinned chips above the context chips, wrapping onto more rows from `r`'s top; returns their
/// height (kept for the next frame's layout).
pub(super) fn pins_ui(app: &mut EffectcraftApp, ui: &mut egui::Ui, r: Rect, look: &Look) -> f32 {
    let t = look.t;
    let mut acts = vec![];
    let mut x = r.min.x;
    let mut cy = r.min.y + PIN_H / 2.0;
    let p = ui.painter().clone();
    for (i, pin) in app.ui.merlin.pins.iter().enumerate() {
        let icon = match pin.kind {
            PinKind::Layer { .. } => Icon::TextLayer,
            PinKind::Frame { .. } => Icon::Stopwatch,
            PinKind::Range { .. } => Icon::Region,
            PinKind::Note { .. } => Icon::Chat,
        };
        let g = p.layout_no_wrap(format!("@{}", short(&pin.label, 26)), Tokens::medium(11.0), look.magic);
        let lock_w = if pin.kind.lockable() { 16.0 } else { 0.0 };
        let w = (8.0 + 12.0 + 4.0 + g.size().x + 4.0 + lock_w + 14.0 + 4.0).min(r.width());
        if x + w > r.max.x && x > r.min.x {
            x = r.min.x;
            cy += PIN_H + 4.0;
        }
        let chip = Rect::from_min_size(pos2(x, cy - PIN_H / 2.0), vec2(w, PIN_H));
        x += w + 4.0;
        p.rect_filled(chip, PIN_H / 2.0, look.magic.gamma_multiply(0.14));
        p.rect_stroke(chip, PIN_H / 2.0, Stroke::new(1.0, look.magic.gamma_multiply(if pin.locked { 0.9 } else { 0.4 })), StrokeKind::Inside);
        icons::paint(&p, Rect::from_center_size(pos2(chip.min.x + 14.0, cy), vec2(11.0, 11.0)), icon, look.magic);
        let text_x = chip.min.x + 24.0;
        let gw = g.size().x;
        p.galley(pos2(text_x, cy - g.size().y / 2.0), g, look.magic);
        let mut bx = text_x + gw + 4.0;
        if pin.kind.lockable() {
            let lr = Rect::from_min_size(pos2(bx, cy - 8.0), vec2(16.0, 16.0));
            let resp = ui.interact(lr, egui::Id::unique(("merlin-pin-lock", i)), Sense::click());
            let col = if pin.locked { look.magic } else { lerp(t.text_faint, t.text, if resp.hovered() { 1.0 } else { 0.0 }) };
            icons::paint(&p, lr.shrink(2.5), Icon::Lock, col);
            app.auto.add(
                &format!("merlin.pin.{i}.lock"),
                lr,
                if pin.locked { "Unlock: let Merlin edit outside this" } else { "Lock: keep Merlin's edits inside this" },
            );
            let tip = if pin.locked {
                tr("Locked: Merlin's edits must stay inside this. Click to unlock.")
            } else {
                tr("Unlocked: context only. Click to lock Merlin's edits inside it.")
            };
            if resp.on_hover_text(tip).clicked() {
                acts.push(PinAct::Lock(i));
            }
            bx += 16.0;
        }
        let xr = Rect::from_min_size(pos2(bx, cy - 7.0), vec2(14.0, 14.0));
        let resp = ui.interact(xr, egui::Id::unique(("merlin-pin-x", i)), Sense::click());
        if resp.hovered() {
            p.circle_filled(xr.center(), 7.0, t.hover);
        }
        icons::paint(&p, xr.shrink(3.5), Icon::Close, if resp.hovered() { t.text } else { t.text_dim });
        app.auto.add(&format!("merlin.pin.{i}.remove"), xr, "Remove pin");
        app.auto.add(&format!("merlin.pin.{i}"), chip, &pin.label);
        if resp.on_hover_text(tr("Remove")).clicked() {
            acts.push(PinAct::Remove(i));
        }
    }
    let used = cy + PIN_H / 2.0 - r.min.y;
    app.ui.merlin.rt.pins_h = used;
    for a in acts {
        let pins = &mut app.ui.merlin.pins;
        match a {
            PinAct::Lock(i) => {
                if let Some(p) = pins.get_mut(i) {
                    p.locked = !p.locked;
                }
            }
            PinAct::Remove(i) => {
                if i < pins.len() {
                    pins.remove(i);
                }
            }
        }
    }
    used
}

/// While an `@query` is being typed: the arrow keys and Enter drive the list (taken before
/// the composer sees Enter). Returns whether the list is showing.
pub(super) fn keys(app: &mut EffectcraftApp, ui: &egui::Ui, focused: bool) -> bool {
    let Some((at, q)) = query(&app.ui.merlin.input) else { return false };
    if !focused || app.ui.merlin.rt.mention_off == Some(at) {
        return false;
    }
    let list = candidates(app, q);
    if list.is_empty() {
        return false;
    }
    let n = list.len();
    let rt = &mut app.ui.merlin.rt;
    let (up, down, pick, esc) = ui.input_mut(|i| {
        (
            i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowUp),
            i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowDown),
            i.consume_key(egui::Modifiers::NONE, egui::Key::Enter),
            i.consume_key(egui::Modifiers::NONE, egui::Key::Escape),
        )
    });
    rt.mention_sel = rt.mention_sel.min(n - 1);
    if up {
        rt.mention_sel = (rt.mention_sel + n - 1) % n;
    }
    if down {
        rt.mention_sel = (rt.mention_sel + 1) % n;
    }
    if esc {
        rt.mention_off = Some(at);
        return false;
    }
    if pick && let Some((p, _)) = list.into_iter().nth(rt.mention_sel) {
        rt.mention_sel = 0;
        accept(app, ui.ctx(), p);
        return false;
    }
    true
}

/// The mention list, above `anchor` (the composer box).
pub(super) fn popup_ui(app: &mut EffectcraftApp, ui: &mut egui::Ui, anchor: Rect, look: &Look) {
    let t = look.t;
    let Some((_, q)) = query(&app.ui.merlin.input) else { return };
    let list = candidates(app, q);
    if list.is_empty() {
        return;
    }
    let row_h = 26.0;
    let h = list.len() as f32 * row_h + 8.0;
    let r = Rect::from_min_size(pos2(anchor.min.x, anchor.min.y - h - 4.0), vec2(anchor.width(), h));
    let layer = egui::LayerId::new(egui::Order::Foreground, egui::Id::unique("merlin-mentions"));
    let p = ui.ctx().layer_painter(layer);
    p.rect_filled(r.expand(1.0), 10.0, Color32::from_black_alpha(40));
    p.rect_filled(r, 9.0, t.panel_bg);
    p.rect_stroke(r, 9.0, Stroke::new(1.0, look.border), StrokeKind::Inside);
    let sel = app.ui.merlin.rt.mention_sel.min(list.len().saturating_sub(1));
    let mut chosen = None;
    for (i, (pin, what)) in list.iter().enumerate() {
        let row = Rect::from_min_size(pos2(r.min.x + 4.0, r.min.y + 4.0 + i as f32 * row_h), vec2(r.width() - 8.0, row_h));
        let resp = ui.interact(row, egui::Id::unique(("merlin-mention", i)), Sense::click()).on_hover_cursor(egui::CursorIcon::PointingHand);
        if resp.hovered() {
            app.ui.merlin.rt.mention_sel = i;
        }
        if i == sel {
            p.rect_filled(row, 6.0, look.magic.gamma_multiply(0.18));
        }
        let icon = match pin.kind {
            PinKind::Layer { .. } => Icon::TextLayer,
            PinKind::Frame { .. } => Icon::Stopwatch,
            PinKind::Range { .. } => Icon::Region,
            PinKind::Note { .. } => Icon::Chat,
        };
        icons::paint(
            &p,
            Rect::from_center_size(pos2(row.min.x + 12.0, row.center().y), vec2(12.0, 12.0)),
            icon,
            if i == sel { look.magic } else { t.text_dim },
        );
        let name = p.text(pos2(row.min.x + 26.0, row.center().y), Align2::LEFT_CENTER, short(&pin.label, 34), Tokens::medium(12.0), t.text);
        p.text(pos2(name.max.x + 8.0, row.center().y), Align2::LEFT_CENTER, what, Tokens::ui(11.0), t.text_faint);
        app.auto.add(&format!("merlin.mention.{i}"), row, &pin.label);
        if resp.clicked() {
            chosen = Some(pin.clone());
        }
    }
    if let Some(pin) = chosen {
        accept(app, ui.ctx(), pin);
        ui.ctx().memory_mut(|m| m.request_focus(egui::Id::unique("merlin-input")));
    }
}

#[cfg(test)]
mod tests {
    use super::query;

    #[test]
    fn the_query_is_the_last_at_word() {
        assert_eq!(query("make @Ti"), Some((5, "Ti")));
        assert_eq!(query("@"), Some((0, "")));
        assert_eq!(query("@1:00-2:12"), Some((0, "1:00-2:12")));
        assert_eq!(query("mail me@home"), None, "an @ inside a word isn't a mention");
        assert_eq!(query("@Title and"), None, "finished: a space follows");
        assert_eq!(query("no mention"), None);
    }
}
