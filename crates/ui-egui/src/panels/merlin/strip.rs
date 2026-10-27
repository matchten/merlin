//! The storyboard strip: a filmstrip of the active comp above the composer, where the user scrubs,
//! drops notes on frames or ranges ("too slow here", "bigger at this point"), and sees how Merlin
//! answered each. Notes are pins (a frame) or bars (a range); clicking one opens its editor, and an
//! area of its frame can be marked on an enlarged render that takes the conversation's place.

use std::sync::{Arc, Mutex};

use effectcraft_engine::project::ItemId;
use effectcraft_engine::time::Tick;
use egui::{Align2, Color32, CornerRadius, Margin, Pos2, Rect, RichText, Sense, Stroke, StrokeKind, pos2, vec2};
use merlin_agent::notes::{Author, Note, NoteStatus};

use super::native;
use super::view::{Look, child, icon_btn, lerp, pill, short, textures};
use crate::EffectcraftApp;
use crate::i18n::{tr, tr_args};
use crate::icons::{self, Icon};
use crate::theme::Tokens;

/// The strip's title row.
const HEAD_H: f32 = 26.0;
/// The filmstrip.
const FILM_H: f32 = 46.0;
/// A collapsed note card in the list.
const CARD_H: f32 = 38.0;
/// The note being edited.
const EDITOR_H: f32 = 104.0;
/// Note cards shown before the list scrolls.
const LIST_ROWS: usize = 3;
/// Most thumbnails across the filmstrip.
const MAX_THUMBS: usize = 24;
/// How close (points) a press must be to a pin to grab it.
const GRAB: f32 = 6.0;

/// Frames rendered off the UI thread: one result per job, in order.
type Slot = Arc<Mutex<Option<Vec<Option<(u32, u32, Vec<u8>)>>>>>;

/// A comp's filmstrip, keyed by what it shows.
pub(crate) struct Film {
    key: FilmKey,
    textures: Vec<egui::TextureHandle>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) struct FilmKey {
    comp: u64,
    revision: u64,
    /// Thumbnails across (a filmstrip), or 0 (a single frame).
    count: usize,
    /// The single frame's time.
    at: Tick,
}

/// A filmstrip or a marking frame being rendered.
pub(crate) struct Pending {
    key: FilmKey,
    slot: Slot,
}

/// A drag on the filmstrip.
#[derive(Clone, Copy)]
pub(crate) enum Drag {
    /// Moving the current time.
    Scrub,
    /// Shift-drag: a range for a new note, from this time.
    Range(Tick),
    /// Moving a note, grabbed this far from its start.
    Pin { id: u64, offset: Tick },
}

/// The strip's colour for a note.
fn note_color(n: &Note, look: &Look) -> Color32 {
    match (&n.status, n.author) {
        (NoteStatus::Addressed { .. }, _) => Color32::from_rgb(0x4a, 0xde, 0x80),
        (NoteStatus::WontFix { .. }, _) => look.t.text_faint,
        (_, Author::Merlin) => look.magic,
        (_, Author::User) => Color32::from_rgb(0xff, 0xb8, 0x00),
    }
}

fn status_text(s: &NoteStatus) -> &'static str {
    match s {
        NoteStatus::Open => "Open",
        NoteStatus::Sent => "With Merlin…",
        NoteStatus::Addressed { .. } => "Addressed",
        NoteStatus::WontFix { .. } => "Won't fix",
    }
}

/// The active comp's notes, in time order.
fn comp_notes(app: &EffectcraftApp) -> Vec<Note> {
    let Some(c) = app.session.active_comp_id() else { return vec![] };
    let mut v: Vec<Note> = app.ui.merlin.notes.iter().filter(|n| n.comp == c.0).cloned().collect();
    v.sort_by_key(|n| (n.start, n.id));
    v
}

/// The strip's height in a panel whose conversation area is `avail` tall.
pub(super) fn height(app: &EffectcraftApp, avail: f32) -> f32 {
    let p = &app.ui.merlin;
    if !p.strip_open || app.session.active_comp().is_none() {
        return HEAD_H;
    }
    let notes = comp_notes(app);
    let editing = p.editing_note.is_some_and(|id| notes.iter().any(|n| n.id == id));
    let rows = notes.len().min(LIST_ROWS) as f32;
    let list = if notes.is_empty() { 0.0 } else { rows * (CARD_H + 4.0) + if editing { EDITOR_H - CARD_H } else { 0.0 } + 4.0 };
    (HEAD_H + FILM_H + 10.0 + list).min((avail - 90.0).max(HEAD_H))
}

/// The strip, in `r` (from [`height`]).
pub(super) fn show(app: &mut EffectcraftApp, ui: &mut egui::Ui, r: Rect, look: &Look) {
    let t = look.t;
    let ctx = ui.ctx().clone();
    ui.painter().line_segment([r.left_top(), r.right_top()], Stroke::new(1.0, t.separator));
    let head = Rect::from_min_size(r.min, vec2(r.width(), HEAD_H));
    head_ui(app, ui, head.shrink2(vec2(10.0, 0.0)), look);
    if !app.ui.merlin.strip_open || app.session.active_comp().is_none() {
        return;
    }
    let film = Rect::from_min_size(pos2(r.min.x + 10.0, head.max.y + 2.0), vec2(r.width() - 20.0, FILM_H));
    film_ui(app, ui, &ctx, film, look);
    let list = Rect::from_min_max(pos2(r.min.x + 10.0, film.max.y + 8.0), pos2(r.max.x - 10.0, r.max.y - 2.0));
    if list.height() > 8.0 {
        list_ui(app, ui, &ctx, list, look);
    }
}

fn head_ui(app: &mut EffectcraftApp, ui: &mut egui::Ui, r: Rect, look: &Look) {
    let t = look.t;
    let cy = r.center().y;
    let open = app.ui.merlin.strip_open;
    let ready = native::ready_notes(app);
    let total = comp_notes(app).len();
    let has_comp = app.session.active_comp().is_some();

    // Right to left: + Note, Apply.
    let add = Rect::from_center_size(pos2(r.max.x - 11.0, cy), vec2(22.0, 22.0));
    let resp = icon_btn(ui, add, Icon::Plus, false, has_comp, look, "note-add");
    app.auto.add("merlin.note.add", add, "Add a note at the current time");
    if resp.on_hover_text(tr("Add a note at the current time")).clicked() {
        let now = app.session.time();
        native::add_note(app, now, None, "");
    }
    let mut right = add.min.x - 6.0;
    if ready > 0 {
        let label = tr_args("Apply {}", &[&ready]);
        let w = ui.painter().layout_no_wrap(label.clone(), Tokens::medium(12.0), Color32::WHITE).size().x + 26.0;
        let pr = Rect::from_min_size(pos2(right - w, cy - 13.0), vec2(w, 26.0));
        let busy = app.ui.merlin.busy;
        let mut c = child(ui, pr);
        let resp = pill(&mut c, &label, true, !busy, look, "notes-apply");
        app.auto.add("merlin.notes.apply", resp.rect, "Apply notes");
        if resp.on_hover_text(tr("Send the open notes to Merlin (with anything typed below)")).clicked() {
            native::start_turn(app, &ui.ctx().clone());
        }
        right = pr.min.x - 6.0;
    }

    let title = Rect::from_min_max(r.min, pos2(right, r.max.y));
    let resp = ui.interact(title, egui::Id::unique("merlin-strip-toggle"), Sense::click());
    let hot = ui.ctx().animate_bool_with_time(resp.id, resp.hovered(), 0.12);
    let col = lerp(t.text_dim, t.text, hot);
    icons::paint(
        ui.painter(),
        Rect::from_center_size(pos2(r.min.x + 6.0, cy), vec2(10.0, 10.0)),
        if open { Icon::ChevronDown } else { Icon::ChevronRight },
        col,
    );
    let g = ui.painter().text(pos2(r.min.x + 16.0, cy), Align2::LEFT_CENTER, tr("Storyboard"), Tokens::semibold(12.0), col);
    if total > 0 {
        let badge = format!("{total}");
        let bg = ui.painter().layout_no_wrap(badge, Tokens::medium(10.5), t.text_dim);
        let br = Rect::from_min_size(pos2(g.max.x + 6.0, cy - 8.0), vec2(bg.size().x + 10.0, 16.0));
        ui.painter().rect_filled(br, 8.0, t.hover);
        ui.painter().galley(br.center() - bg.size() / 2.0, bg, t.text_dim);
    } else if open && has_comp && title.width() > 200.0 {
        ui.painter().text(pos2(g.max.x + 8.0, cy), Align2::LEFT_CENTER, tr("Shift-drag for a range"), Tokens::ui(10.5), t.text_faint);
    }
    app.auto.add("merlin.strip.toggle", title, if open { "Hide the storyboard" } else { "Show the storyboard" });
    if resp.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    if resp.on_hover_text(tr("Frames of the comp, with your notes on them")).clicked() {
        app.ui.merlin.strip_open = !open;
        // The layout above was sized for the old state: draw again.
        ui.ctx().request_repaint();
    }
}

/// Render `times` of `comp` off the UI thread, each at most `max_side`.
fn spawn_frames(app: &EffectcraftApp, comp: u64, times: &[Tick], max_side: u32) -> Option<Slot> {
    let jobs: Vec<_> = times.iter().filter_map(|t| app.session.thumbnail_job(ItemId(comp), *t, max_side)).collect();
    if jobs.len() != times.len() {
        return None;
    }
    let slot: Slot = Arc::default();
    let s = slot.clone();
    std::thread::Builder::new()
        .name("merlin-strip".into())
        .spawn(move || {
            let out: Vec<_> = jobs.into_iter().map(|j| j()).collect();
            if let Ok(mut s) = s.lock() {
                *s = Some(out);
            }
        })
        .ok()?;
    Some(slot)
}

/// The finished frames of `pending` as textures, or `None` while it runs.
fn take_frames(ctx: &egui::Context, pending: &Pending, name: &str) -> Option<Vec<egui::TextureHandle>> {
    let done = pending.slot.lock().ok().and_then(|mut s| s.take())?;
    Some(
        done.into_iter()
            .enumerate()
            .filter_map(|(i, f)| {
                let (w, h, px) = f?;
                let ci = egui::ColorImage::from_rgba_unmultiplied([w as usize, h as usize], &px);
                Some(ctx.load_texture(format!("{name}-{i}"), ci, egui::TextureOptions::LINEAR))
            })
            .collect(),
    )
}

/// The comp's filmstrip, kept up to date in the background (the old strip stays up meanwhile).
fn film_textures(app: &mut EffectcraftApp, ctx: &egui::Context, comp: u64, count: usize, duration: Tick, max_side: u32) -> Vec<egui::TextureHandle> {
    let key = FilmKey { comp, revision: app.session.revision, count, at: Tick::ZERO };
    if let Some(p) = app.ui.merlin.rt.film_job.as_ref() {
        match take_frames(ctx, p, "merlin-film") {
            Some(texs) => {
                let k = p.key;
                app.ui.merlin.rt.film = Some(Film { key: k, textures: texs });
                app.ui.merlin.rt.film_job = None;
            }
            None => ctx.request_repaint_after(std::time::Duration::from_millis(50)),
        }
    }
    let fresh = app.ui.merlin.rt.film.as_ref().is_some_and(|f| f.key == key);
    if !fresh && app.ui.merlin.rt.film_job.is_none() {
        let times: Vec<Tick> = (0..count).map(|i| Tick(((duration.0 as i128) * (2 * i as i128 + 1) / (2 * count.max(1) as i128)) as i64)).collect();
        if let Some(slot) = spawn_frames(app, comp, &times, max_side) {
            app.ui.merlin.rt.film_job = Some(Pending { key, slot });
            ctx.request_repaint();
        }
    }
    match app.ui.merlin.rt.film.as_ref() {
        Some(f) if f.key.comp == comp && f.key.count == count => f.textures.clone(),
        _ => vec![],
    }
}

fn film_ui(app: &mut EffectcraftApp, ui: &mut egui::Ui, ctx: &egui::Context, r: Rect, look: &Look) {
    let t = look.t;
    let Some(cid) = app.session.active_comp_id() else { return };
    let Some(comp) = app.session.active_comp() else { return };
    let (cw, ch, duration, rate) = (comp.width.max(1) as f32, comp.height.max(1) as f32, comp.duration.max(Tick(1)), comp.frame_rate);
    let thumb_w = (FILM_H * cw / ch).max(8.0);
    let count = ((r.width() / thumb_w).ceil() as usize).clamp(1, MAX_THUMBS);
    let px = (ctx.pixels_per_point() * FILM_H * cw.max(ch) / ch).ceil().clamp(32.0, 320.0) as u32;
    let texs = film_textures(app, ctx, cid.0, count, duration, px);

    let p = ui.painter().with_clip_rect(r.expand2(vec2(0.0, 4.0)).intersect(ui.clip_rect()));
    p.rect_filled(r, 6.0, t.app_bg);
    let slot_w = r.width() / count as f32;
    for (i, tex) in texs.iter().enumerate() {
        let cell = Rect::from_min_size(pos2(r.min.x + i as f32 * slot_w, r.min.y), vec2(slot_w, FILM_H));
        // Fill the cell, cropping the frame's sides.
        let uv_w = (slot_w / thumb_w).clamp(0.05, 1.0);
        let uv = Rect::from_min_max(pos2(0.5 - uv_w / 2.0, 0.0), pos2(0.5 + uv_w / 2.0, 1.0));
        p.image(tex.id(), cell, uv, Color32::WHITE);
        p.line_segment([cell.left_top(), cell.left_bottom()], Stroke::new(1.0, t.panel_bg.gamma_multiply(0.6)));
    }
    p.rect_stroke(r, 6.0, Stroke::new(1.0, look.border), StrokeKind::Inside);

    let x_of = |tk: Tick| r.min.x + (tk.0 as f64 / duration.0 as f64).clamp(0.0, 1.0) as f32 * r.width();
    let t_of = |x: f32| {
        let k = ((x - r.min.x) / r.width().max(1.0)).clamp(0.0, 1.0) as f64;
        let raw = Tick((duration.0 as f64 * k).round() as i64);
        rate.tick_of(rate.frame_at(raw).min(rate.frame_at(duration).saturating_sub(1)).max(0))
    };

    // Notes: ranges as bars along the bottom, every note as a pin at its start.
    let notes = comp_notes(app);
    let editing = app.ui.merlin.editing_note;
    for n in &notes {
        let col = note_color(n, look);
        let (s, e) = n.span();
        if e > s {
            let bar = Rect::from_min_max(pos2(x_of(s), r.max.y - 7.0), pos2(x_of(e).max(x_of(s) + 3.0), r.max.y - 2.0));
            p.rect_filled(Rect::from_min_max(pos2(bar.min.x, r.min.y), pos2(bar.max.x, r.max.y)), 0.0, col.gamma_multiply(0.12));
            p.rect_filled(bar, 2.5, col);
        }
        let x = x_of(s);
        let sel = editing == Some(n.id);
        p.line_segment([pos2(x, r.min.y + 6.0), pos2(x, r.max.y)], Stroke::new(if sel { 2.0 } else { 1.5 }, col));
        p.circle_filled(pos2(x, r.min.y + 6.0), if sel { 5.5 } else { 4.5 }, col);
        p.circle_stroke(pos2(x, r.min.y + 6.0), if sel { 5.5 } else { 4.5 }, Stroke::new(1.0, t.panel_bg));
    }
    // A range being dragged out.
    let drag = app.ui.merlin.rt.strip_drag;
    let pointer = ui.input(|i| i.pointer.interact_pos());
    if let (Some(Drag::Range(start)), Some(pos)) = (drag, pointer) {
        let (a, b) = (x_of(start), x_of(t_of(pos.x)));
        let amber = Color32::from_rgb(0xff, 0xb8, 0x00);
        p.rect_filled(Rect::from_min_max(pos2(a.min(b), r.min.y), pos2(a.max(b), r.max.y)), 0.0, amber.gamma_multiply(0.2));
        p.rect_stroke(Rect::from_min_max(pos2(a.min(b), r.min.y), pos2(a.max(b), r.max.y)), 0.0, Stroke::new(1.0, amber), StrokeKind::Inside);
    }
    // The current time.
    let now_x = x_of(app.session.time());
    p.line_segment([pos2(now_x, r.min.y - 2.0), pos2(now_x, r.max.y + 2.0)], Stroke::new(1.5, t.text));
    p.add(egui::Shape::convex_polygon(
        vec![pos2(now_x - 4.0, r.min.y - 3.0), pos2(now_x + 4.0, r.min.y - 3.0), pos2(now_x, r.min.y + 2.0)],
        t.text,
        Stroke::NONE,
    ));

    let resp = ui.interact(r, egui::Id::unique("merlin-strip"), Sense::click_and_drag());
    app.auto.add("merlin.strip", r, "Storyboard: click to set the time, shift-drag for a range note");
    let pin_at = |x: f32| {
        notes.iter().filter(|n| (x_of(n.start) - x).abs() <= GRAB).min_by(|a, b| (x_of(a.start) - x).abs().total_cmp(&(x_of(b.start) - x).abs())).map(|n| n.id)
    };
    let shift = ui.input(|i| i.modifiers.shift);
    // A drag counts from where the button went down (it is only recognised after a few points).
    if resp.drag_started()
        && let Some(pos) = ui.input(|i| i.pointer.press_origin()).or(resp.interact_pointer_pos())
    {
        app.ui.merlin.rt.strip_drag = Some(match pin_at(pos.x) {
            Some(id) if !shift => {
                let start = notes.iter().find(|n| n.id == id).map(|n| n.start).unwrap_or_default();
                Drag::Pin { id, offset: t_of(pos.x) - start }
            }
            _ if shift => Drag::Range(t_of(pos.x)),
            _ => Drag::Scrub,
        });
    }
    if let (Some(pos), true) = (resp.interact_pointer_pos(), resp.dragged()) {
        let at = t_of(pos.x);
        match app.ui.merlin.rt.strip_drag {
            Some(Drag::Scrub) => app.session.set_time(at),
            Some(Drag::Pin { id, offset }) => {
                if let Some(n) = app.ui.merlin.notes.iter_mut().find(|n| n.id == id) {
                    let len = n.end.map(|e| e - n.start);
                    let start = rate.tick_of(rate.frame_at(at - offset).max(0));
                    n.start = start;
                    n.end = len.map(|l| start + l);
                }
            }
            Some(Drag::Range(_)) | None => {}
        }
    }
    if resp.drag_stopped() {
        if let (Some(Drag::Range(start)), Some(pos)) = (app.ui.merlin.rt.strip_drag, resp.interact_pointer_pos().or(pointer)) {
            let end = t_of(pos.x);
            native::add_note(app, start, Some(end), "");
            app.session.set_time(start.min(end));
        }
        app.ui.merlin.rt.strip_drag = None;
    }
    if resp.clicked()
        && let Some(pos) = resp.interact_pointer_pos()
    {
        match pin_at(pos.x) {
            Some(id) => open_note(app, id),
            None => app.session.set_time(t_of(pos.x)),
        }
    }
    if resp.hovered() {
        let over_pin = pointer.and_then(|p| pin_at(p.x)).is_some();
        ui.ctx().set_cursor_icon(if over_pin { egui::CursorIcon::Grab } else { egui::CursorIcon::Text });
    }
    if let Some(pos) = resp.hover_pos() {
        resp.on_hover_text_at_pointer(native::display_time(app, cid.0, t_of(pos.x)));
    }
}

/// Open a note's editor and move the current time to it.
fn open_note(app: &mut EffectcraftApp, id: u64) {
    let Some(n) = app.ui.merlin.notes.iter().find(|n| n.id == id) else { return };
    let start = n.start;
    app.ui.merlin.editing_note = Some(id);
    app.ui.merlin.rt.focus_note = Some(id);
    app.session.set_time(start);
}

enum ListAct {
    Open(u64),
    Close,
    Delete(u64),
    Mark(u64),
}

fn list_ui(app: &mut EffectcraftApp, ui: &mut egui::Ui, ctx: &egui::Context, r: Rect, look: &Look) {
    let t = look.t;
    let notes = comp_notes(app);
    if notes.is_empty() {
        return;
    }
    let labels: Vec<String> = notes
        .iter()
        .map(|n| {
            let (s, e) = n.span();
            if e > s {
                format!("{} – {}", native::display_time(app, n.comp, s), native::display_time(app, n.comp, e))
            } else {
                native::display_time(app, n.comp, s)
            }
        })
        .collect();
    let layer_names: Vec<String> = notes
        .iter()
        .map(|n| {
            let comp = app.session.project.comp(ItemId(n.comp));
            let names: Vec<String> =
                n.layers.iter().filter_map(|id| comp.and_then(|c| c.layer(effectcraft_engine::project::LayerId(*id))).map(|l| l.name.clone())).collect();
            names.join(", ")
        })
        .collect();
    let mut acts = vec![];
    let mut c = child(ui, r);
    let EffectcraftApp { ui: state, auto, .. } = app;
    let panel = &mut state.merlin;
    let editing = panel.editing_note;
    egui::ScrollArea::vertical().id_salt("merlin-notes").auto_shrink([false, false]).show(&mut c, |ui| {
        ui.set_width(ui.available_width());
        ui.spacing_mut().item_spacing.y = 4.0;
        for (i, n) in notes.iter().enumerate() {
            let col = note_color(n, look);
            let open = editing == Some(n.id);
            let label = labels.get(i).cloned().unwrap_or_default();
            let frame = egui::Frame::new()
                .fill(look.card)
                .stroke(Stroke::new(1.0, if open { col.gamma_multiply(0.8) } else { look.border }))
                .corner_radius(CornerRadius::same(8))
                .inner_margin(Margin::symmetric(8, 6));
            // The card senses clicks behind its buttons (they stay on top).
            let card = ui.scope_builder(egui::UiBuilder::new().sense(Sense::click()), |ui| {
                frame.show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.horizontal(|ui| {
                        let (dot, _) = ui.allocate_exact_size(vec2(8.0, 14.0), Sense::hover());
                        ui.painter().circle_filled(dot.center(), 3.5, col);
                        ui.label(RichText::new(&label).font(Tokens::semibold(11.5)).color(t.text));
                        if n.author == Author::Merlin {
                            ui.label(RichText::new(tr("Merlin")).font(Tokens::ui(10.5)).color(look.magic));
                        }
                        ui.label(RichText::new(tr(status_text(&n.status))).font(Tokens::ui(10.5)).color(t.text_faint));
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            let (x, _) = ui.allocate_exact_size(vec2(20.0, 20.0), Sense::hover());
                            if icon_btn(ui, x, Icon::Trash, false, true, look, &format!("note-del-{}", n.id)).on_hover_text(tr("Delete note")).clicked() {
                                acts.push(ListAct::Delete(n.id));
                            }
                            auto.add(&format!("merlin.note.{}.delete", n.id), x, "Delete note");
                            if open {
                                let (m, _) = ui.allocate_exact_size(vec2(20.0, 20.0), Sense::hover());
                                if icon_btn(ui, m, Icon::Region, n.region.is_some(), true, look, &format!("note-mark-{}", n.id))
                                    .on_hover_text(tr("Mark an area of the frame"))
                                    .clicked()
                                {
                                    acts.push(ListAct::Mark(n.id));
                                }
                                auto.add("merlin.note.mark", m, "Mark an area of the frame");
                            }
                        });
                    });
                    if open {
                        let Some(note) = panel.notes.iter_mut().find(|x| x.id == n.id) else { return };
                        let te = egui::TextEdit::multiline(&mut note.text)
                            .id(egui::Id::unique(("merlin-note-text", n.id)))
                            .font(Tokens::ui(12.5))
                            .desired_rows(2)
                            .desired_width(ui.available_width())
                            .hint_text(RichText::new(tr("What should this moment look like?")).color(t.text_faint));
                        let resp = ui.add(te);
                        auto.add_response("merlin.note.text", &resp, "Note text");
                        if resp.has_focus() && ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Enter)) {
                            acts.push(ListAct::Close);
                        }
                        if panel.rt.focus_note == Some(n.id) {
                            resp.request_focus();
                            resp.scroll_to_me(Some(egui::Align::Center));
                            panel.rt.focus_note = None;
                        }
                        let mut about = vec![];
                        if let Some(names) = layer_names.get(i).filter(|s| !s.is_empty()) {
                            about.push(tr_args("About {}", &[names]));
                        }
                        if note.region.is_some() {
                            about.push(tr("Area marked").to_string());
                        }
                        if !about.is_empty() {
                            ui.label(RichText::new(about.join(" · ")).font(Tokens::ui(10.5)).color(t.text_faint));
                        }
                    } else {
                        let text = if n.text.trim().is_empty() { tr("Click to write this note").to_string() } else { short(n.text.trim(), 80) };
                        ui.label(RichText::new(text).font(Tokens::ui(12.0)).color(if n.text.trim().is_empty() { t.text_faint } else { t.text_dim }));
                        if let NoteStatus::Addressed { summary } | NoteStatus::WontFix { summary } = &n.status {
                            if !summary.is_empty() {
                                ui.label(RichText::new(format!("✦ {summary}")).font(Tokens::ui(11.0)).color(look.magic));
                            }
                            // Before → after.
                            let before = textures(ctx, &mut panel.rt, &format!("note-before:{}", n.id)).first().cloned();
                            let after = textures(ctx, &mut panel.rt, &format!("note-after:{}", n.id)).first().cloned();
                            if before.is_some() || after.is_some() {
                                ui.horizontal(|ui| {
                                    for (tex, cap) in [(before, tr("Before")), (after, tr("After"))] {
                                        let Some(tex) = tex else { continue };
                                        let [w, h] = tex.size();
                                        let size = vec2(72.0, 72.0 * h.max(1) as f32 / w.max(1) as f32);
                                        ui.vertical(|ui| {
                                            ui.add(egui::Image::from_texture(egui::load::SizedTexture::new(tex.id(), size)).corner_radius(4.0));
                                            ui.label(RichText::new(cap).font(Tokens::ui(10.0)).color(t.text_faint));
                                        });
                                    }
                                });
                            }
                        }
                    }
                })
            });
            let resp = card.response;
            auto.add(&format!("merlin.note.{}", n.id), resp.rect, &label);
            if resp.clicked() && !open {
                acts.push(ListAct::Open(n.id));
            }
        }
    });
    for a in acts {
        match a {
            ListAct::Open(id) => open_note(app, id),
            ListAct::Close => app.ui.merlin.editing_note = None,
            ListAct::Delete(id) => native::remove_note(app, id),
            ListAct::Mark(id) => {
                app.ui.merlin.marking_note = Some(id);
                app.ui.merlin.rt.mark = None;
                app.ui.merlin.rt.mark_job = None;
            }
        }
    }
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
    ui.painter().text(inner.min + vec2(0.0, 18.0), Align2::LEFT_TOP, native::display_time(app, note.comp, note.start), Tokens::ui(11.0), t.text_faint);

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
    let key = FilmKey { comp: note.comp, revision: app.session.revision, count: 0, at: note.start };
    if let Some(p) = app.ui.merlin.rt.mark_job.as_ref() {
        match take_frames(&ctx, p, "merlin-mark") {
            Some(mut texs) => {
                let k = p.key;
                app.ui.merlin.rt.mark = texs.pop().map(|tx| (k, tx));
                app.ui.merlin.rt.mark_job = None;
            }
            None => ctx.request_repaint_after(std::time::Duration::from_millis(50)),
        }
    }
    let fresh = app.ui.merlin.rt.mark.as_ref().is_some_and(|(k, _)| *k == key);
    if !fresh && app.ui.merlin.rt.mark_job.is_none() {
        let px = (img.width().max(img.height()) * ctx.pixels_per_point()).clamp(64.0, 1600.0) as u32;
        if let Some(slot) = spawn_frames(app, note.comp, &[note.start], px) {
            app.ui.merlin.rt.mark_job = Some(Pending { key, slot });
        }
    }
    ui.painter().rect_filled(img, 4.0, t.app_bg);
    match app.ui.merlin.rt.mark.as_ref() {
        Some((_, tex)) => {
            ui.painter().image(tex.id(), img, Rect::from_min_max(Pos2::ZERO, pos2(1.0, 1.0)), Color32::WHITE);
        }
        None => egui::Spinner::new().size(20.0).color(look.magic).paint_at(ui, Rect::from_center_size(img.center(), vec2(20.0, 20.0))),
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
        app.ui.merlin.rt.mark_drag = resp.interact_pointer_pos().map(to_comp);
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
        let amber = Color32::from_rgb(0xff, 0xb8, 0x00);
        let box_r = Rect::from_min_max(to_screen(pos2(x, y)), to_screen(pos2(x + w, y + h)));
        ui.painter().rect_filled(box_r, 2.0, amber.gamma_multiply(0.12));
        ui.painter().rect_stroke(box_r, 2.0, Stroke::new(2.0, amber), StrokeKind::Inside);
    }
    if let Some(n) = app.ui.merlin.notes.iter_mut().find(|n| n.id == id) {
        n.region = region;
    }
    if done || ui.input(|i| i.key_pressed(egui::Key::Escape)) {
        app.ui.merlin.marking_note = None;
        app.ui.merlin.rt.mark_drag = None;
    }
}
