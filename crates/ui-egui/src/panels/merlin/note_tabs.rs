//! Notes as tabs. A note made on the Timeline (`@`, or Ask Merlin…) joins the stack above the
//! composer as a collapsed tab, `@0:00:01:12` and the start of its text; expanded, it shows the
//! frame it points at (both ends for a range) with the note underneath. The stack goes with the
//! next message; sent notes stay as tabs in that message's card, with Merlin's answer and the frame
//! before and after. Plan beats are the same tabs inside the plan card.

use effectcraft_engine::Session;
use effectcraft_engine::project::{ItemId, LayerId};
use effectcraft_engine::time::Tick;
use egui::{Align2, Color32, CornerRadius, Margin, Pos2, Rect, RichText, Sense, Stroke, StrokeKind, pos2, vec2};
use merlin_agent::notes::{Author, Note, NoteStatus};

use super::frames::{self, FrameKey, Pending};
use super::native::{self, Runtime};
use super::view::{Look, child, icon_btn, lerp, pill, short, textures};
use crate::EffectcraftApp;
use crate::i18n::{tr, tr_args};
use crate::icons::{self, Icon};
use crate::theme::Tokens;

/// Amber: the user's notes (on tabs and the Timeline ruler).
pub(super) const AMBER: Color32 = Color32::from_rgb(0xff, 0xb8, 0x00);
/// Green: notes Merlin addressed.
const DONE: Color32 = Color32::from_rgb(0x4a, 0xde, 0x80);
/// The stack takes at most this share of the panel's height before it scrolls.
const STACK_SHARE: f32 = 0.42;

/// A note's colour.
pub(super) fn color(n: &Note, look: &Look) -> Color32 {
    match (&n.status, n.author) {
        (NoteStatus::Addressed { .. }, _) => DONE,
        (NoteStatus::WontFix { .. }, _) => look.t.text_faint,
        (_, Author::Merlin) => look.magic,
        (_, Author::User) => AMBER,
    }
}

/// `@0:00:01:12`, or `@0:00:01:00–0:00:02:12` for a range, in the project's time display.
pub(super) fn label(s: &Session, n: &Note) -> String {
    let Some(comp) = s.project.comp(ItemId(n.comp)) else { return format!("@{:.2}s", n.start.seconds()) };
    let (a, b) = n.span();
    let at = |t: Tick| crate::panels::timecode(s, comp, t);
    if b > a { format!("@{}–{}", at(a), at(b)) } else { format!("@{}", at(a)) }
}

/// What a note is about: its layers and properties.
fn about(s: &Session, n: &Note) -> String {
    let comp = s.project.comp(ItemId(n.comp));
    let mut parts: Vec<String> = n.layers.iter().filter_map(|id| comp.and_then(|c| c.layer(LayerId(*id))).map(|l| l.name.clone())).collect();
    parts.extend(n.props.iter().cloned());
    parts.join(", ")
}

fn status_text(s: &NoteStatus) -> Option<&'static str> {
    match s {
        NoteStatus::Open | NoteStatus::Proposed => None,
        NoteStatus::Sent => Some("With Merlin…"),
        NoteStatus::Addressed { .. } => Some("Addressed"),
        NoteStatus::WontFix { .. } => Some("Won't fix"),
    }
}

/// What a click on a tab asks for (applied after drawing).
pub(super) enum NoteAct {
    Toggle(u64),
    Delete(u64),
    Mark(u64),
    StartHere(u64),
    EndHere(u64),
    /// Done with the note's text (Enter).
    Close(u64),
}

/// How a tab may be used.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Mode {
    /// In the stack or a proposed plan: write, retime, mark, delete.
    Edit,
    /// Sent: read its status and Merlin's answer.
    Read,
}

/// One note as a tab. `stretching`: the note is being stretched on the Timeline (it glows).
#[allow(clippy::too_many_arguments)]
pub(super) fn tab(
    ui: &mut egui::Ui,
    session: &Session,
    rt: &mut Runtime,
    auto: &mut crate::automation::Registry,
    look: &Look,
    note: &mut Note,
    mode: Mode,
    open: bool,
    now: f64,
    acts: &mut Vec<NoteAct>,
) {
    let t = look.t;
    let ctx = ui.ctx().clone();
    let id = note.id;
    let stretching = rt.stretching.is_some_and(|s| s.0 == id);
    let col = color(note, look);
    let label = label(session, note);
    let glow = if stretching { 0.55 + 0.45 * (0.5 + 0.5 * (now * 5.0).sin()) as f32 } else { 0.0 };
    let border = if stretching {
        lerp(look.border, col, glow)
    } else if open {
        col.gamma_multiply(0.7)
    } else {
        look.border
    };
    let frame = egui::Frame::new()
        .fill(look.card)
        .stroke(Stroke::new(if stretching { 1.5 } else { 1.0 }, border))
        .corner_radius(CornerRadius::same(8))
        .inner_margin(Margin::symmetric(8, 5));
    if stretching {
        ctx.request_repaint();
    }
    frame.show(ui, |ui| {
        ui.set_width(ui.available_width());
        // The title row toggles the tab (its buttons stay on top).
        let row = ui.scope_builder(egui::UiBuilder::new().sense(Sense::click()), |ui| {
            ui.horizontal(|ui| {
                let (dot, _) = ui.allocate_exact_size(vec2(8.0, 16.0), Sense::hover());
                ui.painter().circle_filled(dot.center(), 3.5, col);
                ui.label(RichText::new(&label).font(Tokens::semibold(11.5)).color(t.text));
                if note.author == Author::Merlin {
                    ui.label(RichText::new(tr("Beat")).font(Tokens::ui(10.5)).color(look.magic));
                }
                if let Some(s) = status_text(&note.status) {
                    ui.label(RichText::new(tr(s)).font(Tokens::ui(10.5)).color(t.text_faint));
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let (chev, _) = ui.allocate_exact_size(vec2(14.0, 16.0), Sense::hover());
                    icons::paint(ui.painter(), chev.shrink(2.0), if open { Icon::ChevronDown } else { Icon::ChevronRight }, t.text_faint);
                    if mode == Mode::Edit {
                        let (x, _) = ui.allocate_exact_size(vec2(18.0, 18.0), Sense::hover());
                        if icon_btn(ui, x, Icon::Close, false, true, look, &format!("note-del-{id}")).on_hover_text(tr("Remove note")).clicked() {
                            acts.push(NoteAct::Delete(id));
                        }
                        auto.add(&format!("merlin.note.{id}.delete"), x, "Remove note");
                    }
                    if !open {
                        let first = note.text.lines().find(|l| !l.trim().is_empty()).unwrap_or("").trim();
                        let (text, c) = if first.is_empty() {
                            (if stretching { tr("Drag or Shift+←/→ to stretch · Enter to write") } else { tr("Write a note…") }.to_string(), t.text_faint)
                        } else {
                            (short(first, 60), t.text_dim)
                        };
                        ui.add(egui::Label::new(RichText::new(text).font(Tokens::ui(11.5)).color(c)).truncate());
                    }
                });
            });
        });
        let resp = row.response;
        // The visible part (the stack scrolls).
        auto.add(&format!("merlin.note.{id}"), resp.rect.intersect(ui.clip_rect()), &label);
        if stretching {
            resp.scroll_to_me(None);
        }
        if resp.hovered() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        }
        if resp.clicked() {
            acts.push(NoteAct::Toggle(id));
        }
        if !open {
            return;
        }
        ui.add_space(4.0);
        // The frame(s): as written, or before / after once Merlin answered.
        let done = note.status.is_done();
        let [snap, snap_end] = frames::snap_keys(id);
        let pics: Vec<(String, String)> = if done && rt.frames.contains_key(&format!("note-after:{id}")) {
            vec![(format!("note-before:{id}"), tr("Before").to_string()), (format!("note-after:{id}"), tr("After").to_string())]
        } else if note.end.is_some() {
            vec![(snap, tr("Start").to_string()), (snap_end, tr("End").to_string())]
        } else {
            vec![(snap, String::new())]
        };
        let n = pics.len() as f32;
        let w = ((ui.available_width() - 6.0 * (n - 1.0)) / n).max(40.0);
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            for (key, cap) in &pics {
                ui.vertical(|ui| {
                    let tex = textures(&ctx, rt, key).first().cloned();
                    let size = match &tex {
                        Some(tx) => {
                            let [tw, th] = tx.size();
                            vec2(w, w * th.max(1) as f32 / tw.max(1) as f32)
                        }
                        None => vec2(w, w * 9.0 / 16.0),
                    };
                    let (r, resp) = ui.allocate_exact_size(size, if mode == Mode::Edit { Sense::click() } else { Sense::hover() });
                    match &tex {
                        Some(tx) => {
                            egui::Image::from_texture(egui::load::SizedTexture::new(tx.id(), size)).corner_radius(5.0).paint_at(ui, r);
                        }
                        None => {
                            ui.painter().rect_filled(r, 5.0, t.app_bg);
                            if frames::snapshot_pending(rt, id) || super::view::decoding(rt, key) {
                                super::view::spinner(ui, Rect::from_center_size(r.center(), vec2(14.0, 14.0)), look.magic);
                            }
                        }
                    }
                    if let (Some([x, y, rw, rh]), Some(comp)) = (note.region, session.project.comp(ItemId(note.comp))) {
                        let s = r.width() / comp.width.max(1) as f32;
                        let b = Rect::from_min_size(pos2(r.min.x + x * s, r.min.y + y * s), vec2(rw * s, rh * s)).intersect(r);
                        ui.painter().rect_stroke(b, 2.0, Stroke::new(1.5, AMBER), StrokeKind::Inside);
                    }
                    if mode == Mode::Edit {
                        auto.add(&format!("merlin.note.{id}.mark"), r, "Mark an area of the frame");
                        if resp.on_hover_text(tr("Click to mark the part of the frame this note is about")).clicked() {
                            acts.push(NoteAct::Mark(id));
                        }
                    }
                    if !cap.is_empty() {
                        ui.label(RichText::new(cap).font(Tokens::ui(10.0)).color(t.text_faint));
                    }
                });
            }
        });
        ui.add_space(2.0);
        match mode {
            Mode::Edit => {
                let te = egui::TextEdit::multiline(&mut note.text)
                    .id(egui::Id::unique(("merlin-note-text", id)))
                    .font(Tokens::ui(12.5))
                    .desired_rows(2)
                    .desired_width(ui.available_width())
                    .hint_text(RichText::new(tr("What should this moment look like?")).color(t.text_faint));
                let resp = ui.add(te);
                auto.add_response(&format!("merlin.note.{id}.text"), &resp, "Note text");
                if resp.has_focus() && ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Enter)) {
                    acts.push(NoteAct::Close(id));
                }
                if rt.focus_note == Some(id) {
                    resp.request_focus();
                    resp.scroll_to_me(Some(egui::Align::Center));
                    rt.focus_note = None;
                }
            }
            Mode::Read => {
                if !note.text.trim().is_empty() {
                    ui.add(egui::Label::new(RichText::new(note.text.trim()).font(Tokens::ui(12.5)).color(t.text)).wrap());
                }
                if let NoteStatus::Addressed { summary } | NoteStatus::WontFix { summary } = &note.status
                    && !summary.is_empty()
                {
                    ui.add(egui::Label::new(RichText::new(format!("✦ {summary}")).font(Tokens::ui(11.5)).color(look.magic)).wrap());
                }
            }
        }
        let about = about(session, note);
        let mut bits = vec![];
        if !about.is_empty() {
            bits.push(tr_args("About {}", &[&about]));
        }
        if note.region.is_some() {
            bits.push(tr("Area marked").to_string());
        }
        if !bits.is_empty() {
            ui.label(RichText::new(bits.join(" · ")).font(Tokens::ui(10.5)).color(t.text_faint));
        }
        if mode == Mode::Edit {
            ui.horizontal(|ui| {
                let s = pill(ui, tr("Start at playhead"), false, true, look, &format!("note-start-{id}"));
                auto.add(&format!("merlin.note.{id}.start"), s.rect, "Start at playhead");
                if s.clicked() {
                    acts.push(NoteAct::StartHere(id));
                }
                let e = pill(ui, tr("End at playhead"), false, true, look, &format!("note-end-{id}"));
                auto.add(&format!("merlin.note.{id}.end"), e.rect, "End at playhead");
                if e.clicked() {
                    acts.push(NoteAct::EndHere(id));
                }
            });
        }
    });
}

/// Apply the tabs' clicks.
pub(super) fn apply(app: &mut EffectcraftApp, acts: Vec<NoteAct>) {
    for a in acts {
        match a {
            NoteAct::Toggle(id) => {
                let p = &mut app.ui.merlin;
                if p.rt.stretching.is_some_and(|s| s.0 == id) {
                    p.rt.stretching = None;
                }
                if !p.open_notes.remove(&id) {
                    p.open_notes.insert(id);
                    if let Some(n) = app.ui.merlin.notes.iter().find(|n| n.id == id) {
                        let start = n.start;
                        app.session.set_time(start);
                    }
                }
            }
            NoteAct::Delete(id) => native::remove_note(app, id),
            NoteAct::Mark(id) => {
                app.ui.merlin.marking_note = Some(id);
                app.ui.merlin.rt.mark = None;
                app.ui.merlin.rt.mark_job = None;
            }
            NoteAct::StartHere(id) | NoteAct::EndHere(id) => {
                let now = app.session.time();
                let start = matches!(a, NoteAct::StartHere(_));
                if let Some(n) = app.ui.merlin.notes.iter_mut().find(|n| n.id == id) {
                    let (s, e) = n.span();
                    let (s, e) = if start { (now, e.max(now)) } else { (s.min(now), now) };
                    n.start = s.min(e);
                    n.end = (e.max(s) > s.min(e)).then_some(e.max(s));
                }
                frames::request_snapshot(app, id);
            }
            NoteAct::Close(id) => {
                app.ui.merlin.open_notes.remove(&id);
            }
        }
    }
}

/// The stack's height for a panel whose middle is `avail` tall (0 without open notes).
pub(super) fn stack_height(app: &EffectcraftApp, avail: f32) -> f32 {
    if !app.ui.merlin.notes.iter().any(|n| n.status == NoteStatus::Open) {
        return 0.0;
    }
    app.ui.merlin.rt.stack_h.max(34.0).min(avail * STACK_SHARE)
}

/// The notes going with the next message, as a stack of tabs in `r`.
pub(super) fn stack_ui(app: &mut EffectcraftApp, ui: &mut egui::Ui, r: Rect, look: &Look, now: f64) {
    let t = look.t;
    if r.height() < 4.0 {
        return;
    }
    ui.painter().line_segment([r.left_top(), r.right_top()], Stroke::new(1.0, t.separator));
    let ids: Vec<u64> = app.ui.merlin.notes.iter().filter(|n| n.status == NoteStatus::Open).map(|n| n.id).collect();
    let inner = r.shrink2(vec2(10.0, 4.0));
    // Header: count and collapse-all.
    let head = Rect::from_min_size(inner.min, vec2(inner.width(), 20.0));
    ui.painter().text(
        head.left_center(),
        Align2::LEFT_CENTER,
        if ids.len() == 1 { tr("1 note for Merlin").to_string() } else { tr_args("{} notes for Merlin", &[&ids.len()]) },
        Tokens::semibold(11.0),
        t.text_dim,
    );
    if app.ui.merlin.open_notes.iter().any(|id| ids.contains(id)) {
        let g = ui.painter().layout_no_wrap(tr("Collapse all").to_string(), Tokens::ui(10.5), t.text_faint);
        let cr = Rect::from_min_size(pos2(head.max.x - g.size().x - 4.0, head.center().y - 8.0), vec2(g.size().x + 4.0, 16.0));
        let resp = ui.interact(cr, egui::Id::unique("merlin-notes-collapse"), Sense::click());
        ui.painter().galley(pos2(cr.min.x + 2.0, cr.center().y - g.size().y / 2.0), g, if resp.hovered() { t.text } else { t.text_faint });
        app.auto.add("merlin.notes.collapseAll", cr, "Collapse all notes");
        if resp.clicked() {
            app.ui.merlin.open_notes.retain(|id| !ids.contains(id));
        }
    }
    let list = Rect::from_min_max(pos2(inner.min.x, head.max.y + 2.0), inner.max);
    let mut acts = vec![];
    let mut c = child(ui, list);
    let EffectcraftApp { ui: state, auto, session, .. } = app;
    let panel = &mut state.merlin;
    let out = egui::ScrollArea::vertical().id_salt("merlin-note-stack").auto_shrink([false, true]).show(&mut c, |ui| {
        ui.set_width(ui.available_width());
        ui.spacing_mut().item_spacing.y = 4.0;
        for id in &ids {
            let Some(note) = panel.notes.iter_mut().find(|n| n.id == *id) else { continue };
            let open = panel.open_notes.contains(id);
            tab(ui, session, &mut panel.rt, auto, look, note, Mode::Edit, open, now, &mut acts);
        }
    });
    // Sized to its content next frame (up to its share of the panel).
    let want = out.content_size.y + 30.0;
    if (want - panel.rt.stack_h).abs() > 0.5 {
        panel.rt.stack_h = want;
        ui.ctx().request_repaint();
    }
    apply(app, acts);
}

// ------------------------------------------------------------------ marking an area

/// In the conversation's place while an area of a note's frame is being marked: the frame, large;
/// drag a box over it.
pub(super) fn mark_ui(app: &mut EffectcraftApp, ui: &mut egui::Ui, r: Rect, look: &Look) {
    let t = look.t;
    let ctx = ui.ctx().clone();
    let Some(id) = app.ui.merlin.marking_note else { return };
    let Some(note) = app.ui.merlin.notes.iter().find(|n| n.id == id).cloned() else {
        app.ui.merlin.marking_note = None;
        return;
    };
    let Some(comp) = app.session.project.comp(ItemId(note.comp)) else {
        app.ui.merlin.marking_note = None;
        return;
    };
    let (cw, ch) = (comp.width.max(1) as f32, comp.height.max(1) as f32);
    ui.painter().rect_filled(r, 0.0, t.panel_bg);
    let inner = r.shrink2(vec2(12.0, 10.0));
    ui.painter().text(inner.min, Align2::LEFT_TOP, tr("Drag a box around what this note is about"), Tokens::semibold(12.5), t.text);
    ui.painter().text(inner.min + vec2(0.0, 18.0), Align2::LEFT_TOP, label(&app.session, &note), Tokens::ui(11.0), t.text_faint);

    // Buttons along the bottom.
    let bar = Rect::from_min_max(pos2(inner.min.x, inner.max.y - 28.0), inner.max);
    let mut b = child(ui, bar);
    let mut done = false;
    let mut clear = false;
    b.horizontal(|ui| {
        let d = pill(ui, tr("Done"), true, true, look, "mark-done");
        app.auto.add("merlin.mark.done", d.rect, "Done marking");
        done = d.clicked();
        if note.region.is_some() {
            let c = pill(ui, tr("Clear area"), false, true, look, "mark-clear");
            app.auto.add("merlin.mark.clear", c.rect, "Clear area");
            clear = c.clicked();
        }
    });

    // The frame, fitted.
    let area = Rect::from_min_max(pos2(inner.min.x, inner.min.y + 40.0), pos2(inner.max.x, bar.min.y - 8.0));
    if area.width() < 20.0 || area.height() < 20.0 {
        return;
    }
    let s = (area.width() / cw).min(area.height() / ch);
    let img = Rect::from_center_size(area.center(), vec2(cw * s, ch * s));
    let key = FrameKey { comp: note.comp, revision: app.session.revision, at: note.start };
    if let Some(p) = app.ui.merlin.rt.mark_job.as_ref() {
        match frames::take_textures(&ctx, p, "merlin-mark") {
            Some(mut texs) => {
                let k = p.key;
                let tex = texs.pop();
                // A frame that didn't render isn't asked for again (until the project changes).
                app.ui.merlin.rt.mark_failed = tex.is_none().then_some(k);
                app.ui.merlin.rt.mark = tex.map(|tx| (k, tx));
                app.ui.merlin.rt.mark_job = None;
            }
            None => ctx.request_repaint_after(std::time::Duration::from_millis(50)),
        }
    }
    let fresh = app.ui.merlin.rt.mark.as_ref().is_some_and(|(k, _)| *k == key);
    if !fresh && app.ui.merlin.rt.mark_job.is_none() && app.ui.merlin.rt.mark_failed != Some(key) {
        let px = (img.width().max(img.height()) * ctx.pixels_per_point()).clamp(64.0, 1600.0) as u32;
        if let Some(slot) = frames::spawn_frames(app, note.comp, &[note.start], px) {
            app.ui.merlin.rt.mark_job = Some(Pending { key, slot });
        }
    }
    ui.painter().rect_filled(img, 4.0, t.app_bg);
    match app.ui.merlin.rt.mark.as_ref() {
        Some((_, tex)) => {
            ui.painter().image(tex.id(), img, Rect::from_min_max(Pos2::ZERO, pos2(1.0, 1.0)), Color32::WHITE);
        }
        None => super::view::spinner(ui, Rect::from_center_size(img.center(), vec2(20.0, 20.0)), look.magic),
    }
    ui.painter().rect_stroke(img, 4.0, Stroke::new(1.0, look.border), StrokeKind::Inside);

    let to_comp = |p: Pos2| pos2(((p.x - img.min.x) / s).clamp(0.0, cw), ((p.y - img.min.y) / s).clamp(0.0, ch));
    let to_screen = |p: Pos2| pos2(img.min.x + p.x * s, img.min.y + p.y * s);
    let resp = ui.interact(img, egui::Id::unique("merlin-mark-area"), Sense::drag());
    app.auto.add("merlin.mark.area", img, "Frame: drag a box");
    if resp.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::Crosshair);
    }
    let mut region = note.region;
    if resp.drag_started() {
        app.ui.merlin.rt.mark_drag = ui.input(|i| i.pointer.press_origin()).or(resp.interact_pointer_pos()).map(to_comp);
    }
    if let (Some(a), Some(pos)) = (app.ui.merlin.rt.mark_drag, resp.interact_pointer_pos()) {
        let b = to_comp(pos);
        let (x0, y0, x1, y1) = (a.x.min(b.x), a.y.min(b.y), a.x.max(b.x), a.y.max(b.y));
        if x1 - x0 >= 2.0 && y1 - y0 >= 2.0 {
            region = Some([x0, y0, x1 - x0, y1 - y0]);
        }
    }
    if resp.drag_stopped() {
        app.ui.merlin.rt.mark_drag = None;
    }
    if clear {
        region = None;
    }
    if let Some([x, y, w, h]) = region {
        let box_r = Rect::from_min_max(to_screen(pos2(x, y)), to_screen(pos2(x + w, y + h)));
        ui.painter().rect_filled(box_r, 2.0, AMBER.gamma_multiply(0.12));
        ui.painter().rect_stroke(box_r, 2.0, Stroke::new(2.0, AMBER), StrokeKind::Inside);
    }
    if let Some(n) = app.ui.merlin.notes.iter_mut().find(|n| n.id == id) {
        n.region = region;
    }
    if done || ui.input(|i| i.key_pressed(egui::Key::Escape)) {
        app.ui.merlin.marking_note = None;
        app.ui.merlin.rt.mark_drag = None;
    }
}
