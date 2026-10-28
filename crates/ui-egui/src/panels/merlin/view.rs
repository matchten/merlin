//! Drawing the Merlin panel: a header (brand, model pill, New Chat, Settings), the conversation
//! (user cards, formatted replies, tool cards, Keep / Revert cards) and the composer, or the
//! settings in place of the conversation. Small motions: new items fade and slide in, the brand
//! mark pulses and a shimmer runs over "Thinking" while Merlin works.

use egui::text::{LayoutJob, TextFormat};
use egui::{Align, Align2, Color32, CornerRadius, Layout, Margin, Rect, RichText, Sense, Stroke, StrokeKind, Vec2, pos2, vec2};
use merlin_agent::ProviderKind;
use merlin_agent::keys::KeySource;
use serde_json::Value;

use super::markdown::{self, Block, Span};
use super::native::{self, Runtime};
use super::{AttachState, ChatItem, Decision, Mode, PlanState, ToolState};
use crate::EffectcraftApp;
use crate::i18n::{tr, tr_args};
use crate::icons::{self, Icon};
use crate::theme::Tokens;

/// Thumbnail width in the chat.
const THUMB_W: f32 = 112.0;
/// Height of one row of attachment chips.
pub(super) const CHIPS_H: f32 = 28.0;
/// How long a new item takes to fade and slide in (seconds).
const APPEAR_SECS: f64 = 0.28;
/// How long the empty conversation's mark bobs before it rests.
const EMPTY_BOB_SECS: f64 = 3.0;
/// The share of the panel's height the composer takes.
const COMPOSER_SHARE: f32 = 0.25;
/// Body text size.
const BODY: f32 = 13.0;

const SUGGESTIONS: [&str; 4] = [
    "A title that drops in with a bounce",
    "A lower third for “Ada Lovelace · Mathematician”",
    "Make the selected layer pulse gently",
    "Kinetic typography that says “Ship it”",
];

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
    Suggest(usize),
    ToggleStyle,
    Compare(u64),
    PinFrame,
    PinLayers,
    BuildPlan(usize),
    Note(super::note_tabs::NoteAct),
    Restore(usize),
    DiscardPlan(usize),
    SetMode(Mode),
}

/// The panel's colours, derived from the theme so light and dark both work.
#[derive(Clone, Copy)]
pub(super) struct Look {
    pub(super) t: Tokens,
    /// Merlin's mark and accents (a soft violet).
    pub(super) magic: Color32,
    pub(super) card: Color32,
    pub(super) border: Color32,
    code_bg: Color32,
}

impl Look {
    fn new(t: Tokens) -> Look {
        let magic = Color32::from_rgb(0xa7, 0x8b, 0xfa);
        Look { t, magic, card: t.field_bg, border: t.field_border.gamma_multiply(0.8), code_bg: t.app_bg }
    }
}

/// The panel.
pub fn show(app: &mut EffectcraftApp, ui: &mut egui::Ui, rect: Rect) {
    native::ensure_loaded(app);
    let ctx = ui.ctx().clone();
    let now = app.now();
    app.ui.merlin.rt.now = now;
    let look = Look::new(app.tokens);
    ui.painter().rect_filled(rect, 0.0, look.t.panel_bg);
    let header = Rect::from_min_size(rect.min, vec2(rect.width(), 38.0));
    let chips = if app.ui.merlin.attachments.is_empty() { 0.0 } else { app.ui.merlin.rt.chips_h.max(CHIPS_H) + 2.0 };
    let pins = if app.ui.merlin.pins.is_empty() { 0.0 } else { app.ui.merlin.rt.pins_h.max(super::mentions::PIN_H) + 4.0 };
    // The composer takes a quarter of the panel (more when chips need it); the notes for the next
    // message run down from the top, the conversation fills between.
    let footer_h = (rect.height() * COMPOSER_SHARE).max(148.0 + chips + pins).min(rect.height() * 0.6);
    let footer = Rect::from_min_max(pos2(rect.min.x, rect.max.y - footer_h), rect.max);
    let middle = Rect::from_min_max(pos2(rect.min.x, header.max.y), pos2(rect.max.x, footer.min.y));
    let stack_h = super::note_tabs::stack_height(app, rect.height());
    let stack = Rect::from_min_max(middle.min, pos2(rect.max.x, middle.min.y + stack_h));
    let body = Rect::from_min_max(pos2(rect.min.x, stack.max.y), middle.max);
    let mut acts = vec![];
    header_ui(app, ui, header, &look, now, &mut acts);
    if app.ui.merlin.settings_open {
        settings_ui(app, ui, Rect::from_min_max(middle.min, rect.max), &look, &mut acts);
    } else if app.ui.merlin.style_open {
        super::style_view::show(app, ui, Rect::from_min_max(middle.min, rect.max), &look);
    } else {
        if app.ui.merlin.marking_note.is_some() {
            super::note_tabs::mark_ui(app, ui, body, &look);
        } else {
            log_ui(app, ui, body, &look, now, &mut acts);
        }
        if stack_h > 0.0 {
            super::note_tabs::stack_ui(app, ui, stack, &look, now);
        }
        composer_ui(app, ui, footer, &look, now, &mut acts);
    }
    if std::mem::take(&mut app.ui.merlin.rt.drop_hover) {
        drop_hint(app, ui, rect, &look);
    }
    for a in acts {
        match a {
            Act::Send => native::start_turn(app, &ctx),
            Act::Stop => native::stop_turn(app, now),
            Act::NewChat => native::new_chat(app),
            Act::Attach => native::pick_attachments(app, &ctx),
            Act::Unattach(i) => native::unattach(app, i),
            Act::ToggleSettings => {
                let p = &mut app.ui.merlin;
                p.settings_open = !p.settings_open;
                p.style_open = false;
                p.rt.key_status = None;
            }
            Act::Compare(t) => {
                for it in &mut app.ui.merlin.items {
                    if let ChatItem::Turn { id, showing_before, .. } = it
                        && *id == t
                    {
                        *showing_before = !*showing_before;
                    }
                }
            }
            Act::ToggleStyle => {
                let p = &mut app.ui.merlin;
                p.style_open = !p.style_open;
                p.settings_open = false;
            }
            Act::Keep(t) => native::decide(app, &ctx, t, true),
            Act::Revert(t) => native::decide(app, &ctx, t, false),
            Act::Toggle(i) => {
                if let Some(ChatItem::Thinking { open, .. } | ChatItem::Tool { open, .. }) = app.ui.merlin.items.get_mut(i) {
                    *open = !*open;
                }
            }
            Act::Suggest(i) => {
                if let Some(s) = SUGGESTIONS.get(i) {
                    app.ui.merlin.input = tr(s).to_string();
                    ctx.memory_mut(|m| m.request_focus(input_id()));
                }
            }
            Act::SaveKey | Act::RemoveKey => native::save_or_remove_key(app, matches!(a, Act::SaveKey)),
            Act::BuildPlan(i) => native::build_plan(app, &ctx, i),
            Act::Note(a) => super::note_tabs::apply(app, vec![a]),
            Act::Restore(i) => native::restore(app, &ctx, i),
            Act::DiscardPlan(i) => native::discard_plan(app, i),
            Act::SetMode(m) => app.ui.merlin.mode = m,
            Act::PinFrame => super::mentions::pin_current_frame(app),
            Act::PinLayers => super::mentions::pin_selected_layers(app),
        }
    }
}

fn input_id() -> egui::Id {
    egui::Id::unique("merlin-input")
}

pub(super) fn child(ui: &mut egui::Ui, r: Rect) -> egui::Ui {
    ui.new_child(egui::UiBuilder::new().max_rect(r).layout(Layout::top_down(Align::Min)))
}

/// 0 → 1 over [`APPEAR_SECS`] from the first frame `key` was drawn (eased), repainting meanwhile.
fn appear(ctx: &egui::Context, rt: &mut Runtime, key: usize, now: f64) -> f32 {
    let start = *rt.appear.entry(key).or_insert(now);
    let k = ((now - start) / APPEAR_SECS).clamp(0.0, 1.0) as f32;
    if k < 1.0 {
        ctx.request_repaint();
    }
    1.0 - (1.0 - k).powi(3)
}

/// A soft repeating wave, 0..1.
fn wave(now: f64, speed: f64, phase: f64) -> f32 {
    (0.5 + 0.5 * (now * speed + phase).sin()) as f32
}

/// Merlin's mark: a sparkle with a soft glow (which breathes while a turn runs).
fn paint_mark(p: &egui::Painter, c: egui::Pos2, size: f32, look: &Look, now: f64, busy: bool) {
    let glow = if busy { 0.25 + 0.35 * wave(now, 4.0, 0.0) } else { 0.18 };
    p.circle_filled(c, size * 0.75, look.magic.gamma_multiply(glow * 0.5));
    let s = if busy { size * (0.92 + 0.12 * wave(now, 4.0, 0.0)) } else { size };
    icons::paint(p, Rect::from_center_size(c, Vec2::splat(s)), Icon::Sparkle, look.magic);
}

/// Text with a highlight sweeping across it (Thinking…, Working…).
fn shimmer(ui: &mut egui::Ui, text: &str, size: f32, look: &Look, now: f64) {
    let chars: Vec<char> = text.chars().collect();
    let n = chars.len().max(1) as f32;
    let sweep = ((now * 0.8).fract() as f32) * (n + 8.0) - 4.0;
    let mut job = LayoutJob::default();
    for (i, ch) in chars.iter().enumerate() {
        let d = (i as f32 - sweep).abs();
        let k = (1.0 - d / 4.0).clamp(0.0, 1.0);
        let color = lerp(look.t.text_faint, look.t.text, k);
        job.append(&ch.to_string(), 0.0, TextFormat { font_id: Tokens::medium(size), color, ..Default::default() });
    }
    ui.label(job);
    tick(ui.ctx());
}

/// Wake for the next frame of a decorative animation, at about 30 fps (not the display's full
/// rate: real news from the worker repaints right away anyway).
pub(super) fn tick(ctx: &egui::Context) {
    ctx.request_repaint_after(std::time::Duration::from_millis(33));
}

/// A busy indicator: an arc turning in `r` (egui's spinner repaints at full rate).
pub(super) fn spinner(ui: &egui::Ui, r: Rect, color: Color32) {
    let now = ui.input(|i| i.time);
    let c = r.center();
    let rad = r.width().min(r.height()) / 2.0 - 1.0;
    let start = now * std::f64::consts::TAU * 0.9;
    let pts: Vec<egui::Pos2> = (0..=16)
        .map(|k| {
            let a = start + (k as f64 / 16.0) * std::f64::consts::PI * 1.4;
            c + rad * vec2(a.cos() as f32, a.sin() as f32)
        })
        .collect();
    ui.painter().add(egui::Shape::line(pts, Stroke::new(1.6, color)));
    tick(ui.ctx());
}

fn lerp_f(a: f32, b: f32, k: f32) -> f32 {
    a + (b - a) * k
}

pub(super) fn lerp(a: Color32, b: Color32, k: f32) -> Color32 {
    let m = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * k).round().clamp(0.0, 255.0) as u8;
    Color32::from_rgba_unmultiplied(m(a.r(), b.r()), m(a.g(), b.g()), m(a.b(), b.b()), m(a.a(), b.a()))
}

/// A rounded icon button; `on` draws it highlighted.
pub(super) fn icon_btn(ui: &mut egui::Ui, r: Rect, icon: Icon, on: bool, enabled: bool, look: &Look, id: &str) -> egui::Response {
    let resp = ui.interact(r, egui::Id::unique(("merlin-btn", id)), if enabled { Sense::click() } else { Sense::hover() });
    let t = &look.t;
    let hot = ui.ctx().animate_bool_with_time(resp.id, enabled && resp.hovered(), 0.12);
    if on {
        ui.painter().rect_filled(r, 6.0, look.magic.gamma_multiply(0.22));
    } else if hot > 0.0 {
        ui.painter().rect_filled(r, 6.0, t.hover.gamma_multiply(hot));
    }
    let col = if !enabled {
        t.text_faint.gamma_multiply(0.6)
    } else if on {
        look.magic
    } else {
        lerp(t.icon, t.icon_active, hot)
    };
    icons::paint(ui.painter(), r.shrink(r.width() * 0.25), icon, col);
    if enabled && resp.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    resp
}

/// A pill button: `primary` is filled with the accent, else outlined.
pub(super) fn pill(ui: &mut egui::Ui, label: &str, primary: bool, enabled: bool, look: &Look, id: &str) -> egui::Response {
    let t = &look.t;
    let galley = ui.painter().layout_no_wrap(label.to_string(), Tokens::medium(12.0), Color32::WHITE);
    let size = vec2(galley.size().x + 26.0, 26.0);
    let (r, resp) = ui.allocate_exact_size(size, if enabled { Sense::click() } else { Sense::hover() });
    let hot = ui.ctx().animate_bool_with_time(egui::Id::unique(("merlin-pill", id)), enabled && resp.hovered(), 0.12);
    let radius = r.height() / 2.0;
    let text_color = if primary {
        let fill = if enabled { lerp(t.accent, t.accent_hover, hot) } else { t.field_border };
        ui.painter().rect_filled(r, radius, fill);
        Color32::WHITE
    } else {
        if hot > 0.0 {
            ui.painter().rect_filled(r, radius, t.hover.gamma_multiply(hot));
        }
        ui.painter().rect_stroke(r, radius, Stroke::new(1.0, t.field_border), StrokeKind::Inside);
        if enabled { t.text } else { t.text_faint }
    };
    ui.painter().galley(r.center() - galley.size() / 2.0, galley, text_color);
    if enabled && resp.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    resp
}

// ------------------------------------------------------------------ header

fn header_ui(app: &mut EffectcraftApp, ui: &mut egui::Ui, r: Rect, look: &Look, now: f64, acts: &mut Vec<Act>) {
    let t = look.t;
    let p = ui.painter().clone();
    p.line_segment([r.left_bottom(), r.right_bottom()], Stroke::new(1.0, t.separator));
    let cy = r.center().y;
    let busy = app.ui.merlin.busy;
    paint_mark(&p, pos2(r.min.x + 20.0, cy), 15.0, look, now, busy);
    let name = p.text(pos2(r.min.x + 33.0, cy), Align2::LEFT_CENTER, "Merlin", Tokens::semibold(13.5), t.text);
    if busy {
        tick(ui.ctx());
    }

    // Right to left: Settings, New Chat; the model pill takes what is left.
    let settings = Rect::from_center_size(pos2(r.max.x - 20.0, cy), vec2(26.0, 26.0));
    let open = app.ui.merlin.settings_open;
    let resp = icon_btn(ui, settings, Icon::Gear, open, true, look, "settings");
    app.auto.add("merlin.settings", settings, "Settings");
    if resp.on_hover_text(if open { tr("Back to chat") } else { tr("Settings") }).clicked() {
        acts.push(Act::ToggleSettings);
    }
    let mut right = settings.min.x - 4.0;
    // Style kit: the kit's colours as dots.
    let style = Rect::from_center_size(pos2(right - 13.0, cy), vec2(26.0, 26.0));
    let style_on = app.ui.merlin.style_open;
    let resp = icon_btn(ui, style, Icon::Checker, style_on, true, look, "style");
    ui.painter().rect_filled(
        style.shrink(3.0),
        6.0,
        if style_on {
            look.magic.gamma_multiply(0.22)
        } else if resp.hovered() {
            t.hover
        } else {
            t.panel_bg
        },
    );
    super::style_view::paint_dots(ui.painter(), style.center(), super::native::active_kit(&app.ui.merlin), look, 3.6);
    app.auto.add("merlin.style", style, "Style kit");
    if resp.on_hover_text(if style_on { tr("Back to chat") } else { tr("Style kit: palette, fonts, motion") }).clicked() {
        acts.push(Act::ToggleStyle);
    }
    right = style.min.x - 2.0;
    let open = open || style_on;
    if !open {
        let new_chat = Rect::from_center_size(pos2(right - 13.0, cy), vec2(26.0, 26.0));
        let resp = icon_btn(ui, new_chat, Icon::Plus, false, !busy, look, "newChat");
        app.auto.add("merlin.newChat", new_chat, "New Chat");
        if resp.on_hover_text(tr("New chat")).clicked() {
            acts.push(Act::NewChat);
        }
        right = new_chat.min.x - 6.0;
    }

    let _ = (name, right);
}

// ------------------------------------------------------------------ conversation

fn log_ui(app: &mut EffectcraftApp, ui: &mut egui::Ui, r: Rect, look: &Look, now: f64, acts: &mut Vec<Act>) {
    let t = look.t;
    let ctx = ui.ctx().clone();
    app.auto.add("merlin.log", r, "Merlin conversation");
    if app.ui.merlin.items.is_empty() {
        empty_ui(app, ui, r, look, now, acts);
        return;
    }
    app.ui.merlin.rt.empty_since = None;
    let mut c = child(ui, r.shrink2(vec2(12.0, 8.0)));
    let EffectcraftApp { ui: state, auto, session, .. } = app;
    let panel = &mut state.merlin;
    let busy = panel.busy;
    let count = panel.items.len();
    egui::ScrollArea::vertical().id_salt("merlin-log").auto_shrink([false, false]).stick_to_bottom(true).show(&mut c, |ui| {
        ui.set_width(ui.available_width());
        ui.spacing_mut().item_spacing.y = 6.0;
        let latest: Vec<u64> = panel.rt.checkpoints.iter().filter(|(_, cp)| cp.is_latest(session)).map(|(id, _)| *id).collect();
        let mut after_user = false;
        let width = ui.available_width();
        let view = ui.clip_rect();
        panel.rt.drawn_items = 0;
        for (i, item) in panel.items.iter().enumerate() {
            let is_user = matches!(item, ChatItem::User { .. });
            let is_turn = matches!(item, ChatItem::Turn { .. });
            // Out of view: take the item's space without laying it out (the last item, which may be
            // growing, is always drawn).
            let top = ui.cursor().min.y;
            if i + 1 < count
                && let Some(&(w, h)) = panel.rt.item_h.get(&i)
                && (w - width).abs() < 0.5
                && (top + h < view.min.y || top > view.max.y)
            {
                ui.add_space(h);
                // Old items scrolled to later don't fade in.
                panel.rt.appear.entry(i).or_insert(now - APPEAR_SECS);
                after_user = is_user;
                continue;
            }
            panel.rt.drawn_items += 1;
            let k = appear(&ctx, &mut panel.rt, i, now);
            ui.scope(|ui| {
                ui.multiply_opacity(k);
                ui.add_space((1.0 - k) * 10.0);
                // "✦ Merlin" above the first part of each answer.
                if after_user && !is_user && !is_turn {
                    ui.add_space(4.0);
                    ui.horizontal(|ui| {
                        let (r, _) = ui.allocate_exact_size(vec2(14.0, 14.0), Sense::hover());
                        icons::paint(ui.painter(), r, Icon::Sparkle, look.magic);
                        ui.label(RichText::new("Merlin").font(Tokens::semibold(12.0)).color(t.text_dim));
                    });
                }
                match item {
                    ChatItem::User { text, attachments, notes, pins, checkpoint } => {
                        ui.add_space(4.0);
                        let card = egui::Frame::new()
                            .fill(look.card)
                            .stroke(Stroke::new(1.0, look.border))
                            .corner_radius(CornerRadius::same(10))
                            .inner_margin(Margin::symmetric(11, 9))
                            .show(ui, |ui| {
                                ui.set_width(ui.available_width());
                                if !attachments.is_empty() {
                                    let keys: Vec<String> = attachments.iter().map(|p| format!("att:{p}")).collect();
                                    thumbnails(ui, &ctx, &mut panel.rt, &keys, 72.0);
                                }
                                if !notes.is_empty() {
                                    ui.spacing_mut().item_spacing.y = 4.0;
                                    let mut note_acts = vec![];
                                    for id in notes {
                                        if let Some(note) = panel.notes.iter_mut().find(|n| n.id == *id) {
                                            let open = panel.open_notes.contains(id);
                                            super::note_tabs::tab(
                                                ui,
                                                session,
                                                &mut panel.rt,
                                                auto,
                                                look,
                                                note,
                                                super::note_tabs::Mode::Read,
                                                open,
                                                now,
                                                &mut note_acts,
                                            );
                                        }
                                    }
                                    acts.extend(note_acts.into_iter().map(Act::Note));
                                }
                                if !pins.is_empty() {
                                    ui.horizontal_wrapped(|ui| {
                                        for p in pins {
                                            ui.label(
                                                RichText::new(format!("@{p}"))
                                                    .font(Tokens::medium(11.5))
                                                    .color(look.magic)
                                                    .background_color(look.magic.gamma_multiply(0.12)),
                                            );
                                        }
                                    });
                                }
                                if !text.is_empty() {
                                    ui.add(egui::Label::new(RichText::new(text).font(Tokens::ui(BODY)).color(t.text)).wrap());
                                }
                            });
                        // Restore to here (not while a turn runs).
                        let r = card.response.rect;
                        if checkpoint.is_some() && !busy {
                            let br = Rect::from_min_size(pos2(r.max.x - 26.0, r.min.y + 4.0), vec2(22.0, 22.0));
                            let resp = icon_btn(ui, br, Icon::StepBack, false, true, look, &format!("restore-{i}"));
                            auto.add(&format!("merlin.item.{i}.restore"), br, "Restore to here");
                            if resp.on_hover_text(tr("Restore to here: put the project back as it was before this message")).clicked() {
                                acts.push(Act::Restore(i));
                            }
                        }
                    }
                    ChatItem::Assistant { text } => {
                        let fresh = panel.rt.md_cache.get(&i).is_some_and(|(n, _)| *n == text.len());
                        if !fresh {
                            panel.rt.md_cache.insert(i, (text.len(), markdown::parse(text)));
                        }
                        if let Some((_, blocks)) = panel.rt.md_cache.get(&i) {
                            markdown_ui(ui, blocks, look);
                        }
                    }
                    ChatItem::Thinking { text, open } => {
                        let live = busy && i + 1 == count;
                        let resp = disclosure(ui, *open, look, |ui| {
                            if live {
                                shimmer(ui, tr("Thinking…"), 12.0, look, now);
                            } else {
                                ui.label(RichText::new(tr("Thought")).font(Tokens::medium(12.0)).color(t.text_faint));
                            }
                        });
                        auto.add(&format!("merlin.item.{i}.toggle"), resp.rect, "Thinking");
                        if resp.clicked() {
                            acts.push(Act::Toggle(i));
                        }
                        if *open {
                            let (line_top, _) = (ui.cursor().min, ());
                            let inner = egui::Frame::new().inner_margin(Margin { left: 12, right: 0, top: 2, bottom: 2 }).show(ui, |ui| {
                                ui.add(egui::Label::new(RichText::new(text).font(Tokens::ui(12.0)).italics().color(t.text_dim)).wrap());
                            });
                            let x = line_top.x + 4.0;
                            ui.painter().line_segment([pos2(x, inner.response.rect.min.y), pos2(x, inner.response.rect.max.y)], Stroke::new(2.0, look.border));
                        }
                    }
                    ChatItem::Tool { id, name, input, state, summary, frames, open } => {
                        tool_card(ui, &ctx, &mut panel.rt, auto, acts, look, i, (id, name, input, *state, summary, *frames, *open));
                    }
                    ChatItem::Turn { id, label, outcome, decision, tool_calls, input_tokens, output_tokens, cost, showing_before } => {
                        ui.add_space(2.0);
                        turn_card(
                            ui,
                            &mut panel.rt,
                            auto,
                            acts,
                            look,
                            &latest,
                            (*id, label, outcome, *decision, *tool_calls, *input_tokens, *output_tokens, *cost),
                            *showing_before,
                        );
                        ui.add_space(6.0);
                    }
                    ChatItem::Notice { text, error } => notice_ui(ui, text, *error, look),
                    ChatItem::Plan { title, beats, state } => {
                        let any = beats.iter().any(|id| panel.notes.iter().any(|n| n.id == *id));
                        let mode = if *state == PlanState::Proposed { super::note_tabs::Mode::Edit } else { super::note_tabs::Mode::Read };
                        let mut note_acts = vec![];
                        let notes = &mut panel.notes;
                        let rt = &mut panel.rt;
                        let opened = &panel.open_notes;
                        plan_card(ui, auto, acts, look, i, title, any, *state, busy, &mut |ui, auto| {
                            ui.spacing_mut().item_spacing.y = 4.0;
                            for id in beats {
                                if let Some(note) = notes.iter_mut().find(|n| n.id == *id) {
                                    let open = opened.contains(id);
                                    super::note_tabs::tab(ui, session, rt, auto, look, note, mode, open, now, &mut note_acts);
                                }
                            }
                        });
                        acts.extend(note_acts.into_iter().map(Act::Note));
                    }
                }
            });
            // Its height, for when it scrolls out of view (without the fade-in's offset).
            panel.rt.item_h.insert(i, (width, ui.cursor().min.y - top - (1.0 - k) * 10.0));
            after_user = is_user;
        }
        if let Some(status) = native::working_status(&panel.rt) {
            ui.add_space(2.0);
            ui.horizontal(|ui| {
                let (r, _) = ui.allocate_exact_size(vec2(26.0, 16.0), Sense::hover());
                for k in 0..3 {
                    let y = r.center().y - 3.0 * wave(now, 7.0, -(k as f64) * 0.9);
                    let a = 0.45 + 0.55 * wave(now, 7.0, -(k as f64) * 0.9);
                    ui.painter().circle_filled(pos2(r.min.x + 5.0 + k as f32 * 8.0, y), 2.6, look.magic.gamma_multiply(a));
                }
                shimmer(ui, &status, 12.0, look, now);
            });
        }
    });
}

/// A clickable "▸ label" row; returns its response.
fn disclosure(ui: &mut egui::Ui, open: bool, look: &Look, label: impl FnOnce(&mut egui::Ui)) -> egui::Response {
    let inner = ui.horizontal(|ui| {
        let (r, _) = ui.allocate_exact_size(vec2(12.0, 14.0), Sense::hover());
        icons::paint(ui.painter(), r.shrink(1.5), if open { Icon::ChevronDown } else { Icon::ChevronRight }, look.t.text_faint);
        label(ui);
    });
    let resp = ui.interact(inner.response.rect, inner.response.id.with("click"), Sense::click());
    if resp.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    resp
}

/// The empty conversation: the mark, a line of welcome and a few things to try.
fn empty_ui(app: &mut EffectcraftApp, ui: &mut egui::Ui, r: Rect, look: &Look, now: f64, acts: &mut Vec<Act>) {
    let t = look.t;
    let mut c = child(ui, r.shrink2(vec2(18.0, 12.0)));
    c.set_clip_rect(r.intersect(ui.clip_rect()));
    c.add_space((r.height() * 0.16).max(8.0));
    c.vertical_centered(|ui| {
        let (mr, _) = ui.allocate_exact_size(vec2(56.0, 56.0), Sense::hover());
        // The mark bobs for a few seconds, then rests so an idle app stops repainting.
        let since = *app.ui.merlin.rt.empty_since.get_or_insert(now);
        let k = (1.0 - (now - since) / EMPTY_BOB_SECS).clamp(0.0, 1.0) as f32;
        let bob = 2.5 * (wave(now, 1.6, 0.0) - 0.5) * k;
        let center = mr.center() + vec2(0.0, bob);
        ui.painter().circle_filled(center, 26.0, look.magic.gamma_multiply(0.08 + 0.06 * lerp_f(0.5, wave(now, 1.6, 0.0), k)));
        ui.painter().circle_stroke(center, 26.0, Stroke::new(1.0, look.magic.gamma_multiply(0.35)));
        icons::paint(ui.painter(), Rect::from_center_size(center, vec2(26.0, 26.0)), Icon::Sparkle, look.magic);
        if k > 0.0 {
            tick(ui.ctx());
        }
        ui.add_space(10.0);
        ui.label(RichText::new(tr("What should we make?")).font(Tokens::semibold(16.0)).color(t.text));
        ui.add_space(2.0);
        ui.add(
            egui::Label::new(
                RichText::new(tr("Describe motion in plain words. Edits appear live, and each turn is one undo step you can keep or revert."))
                    .font(Tokens::ui(12.0))
                    .color(t.text_faint),
            )
            .wrap(),
        );
        ui.add_space(14.0);
    });
    let width = c.available_width().min(340.0);
    let x = c.max_rect().center().x - width / 2.0;
    for (n, s) in SUGGESTIONS.iter().enumerate() {
        // Only the suggestions that fit (the notes and the composer take room in a short panel).
        if c.cursor().min.y + 30.0 > r.max.y - 4.0 {
            break;
        }
        let (row, _) = c.allocate_exact_size(vec2(c.available_width(), 32.0), Sense::hover());
        let r = Rect::from_min_size(pos2(x, row.min.y), vec2(width, 30.0));
        let resp = c.interact(r, egui::Id::unique(("merlin-suggest", n)), Sense::click());
        let hot = c.ctx().animate_bool_with_time(resp.id, resp.hovered(), 0.15);
        c.painter().rect_filled(r, 8.0, lerp(look.card, t.hover, hot));
        c.painter().rect_stroke(r, 8.0, Stroke::new(1.0, lerp(look.border, look.magic.gamma_multiply(0.6), hot)), StrokeKind::Inside);
        icons::paint(
            c.painter(),
            Rect::from_center_size(pos2(r.min.x + 16.0, r.center().y), vec2(12.0, 12.0)),
            Icon::Sparkle,
            look.magic.gamma_multiply(0.6 + 0.4 * hot),
        );
        let mut job = LayoutJob::simple_singleline(tr(s).to_string(), Tokens::ui(12.0), t.text_dim);
        job.wrap = egui::text::TextWrapping::truncate_at_width(width - 40.0);
        let g = c.painter().layout_job(job);
        c.painter().galley(pos2(r.min.x + 30.0 + 2.0 * hot, r.center().y - g.size().y / 2.0), g, lerp(t.text_dim, t.text, hot));
        app.auto.add(&format!("merlin.suggestion.{n}"), r, s);
        if resp.hovered() {
            c.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        }
        if resp.clicked() {
            acts.push(Act::Suggest(n));
        }
    }
}

/// A reply, formatted.
fn markdown_ui(ui: &mut egui::Ui, blocks: &[Block], look: &Look) {
    let t = look.t;
    let width = ui.available_width();
    for block in blocks {
        match block {
            Block::Paragraph(spans) => {
                ui.label(spans_job(spans, BODY, t.text, look, width));
            }
            Block::Heading(level, spans) => {
                ui.add_space(4.0);
                let size = match *level {
                    1 => 16.0,
                    2 => 14.5,
                    _ => 13.5,
                };
                let spans: Vec<Span> = spans.iter().map(|s| Span { bold: true, ..s.clone() }).collect();
                ui.label(spans_job(&spans, size, t.text, look, width));
            }
            Block::Item { marker, depth, spans } => {
                ui.horizontal_top(|ui| {
                    ui.add_space(4.0 + *depth as f32 * 14.0);
                    let bullet = marker.clone();
                    let color = if bullet == "•" { look.magic } else { t.text_dim };
                    ui.label(RichText::new(bullet).font(Tokens::semibold(BODY)).color(color));
                    let w = ui.available_width();
                    ui.label(spans_job(spans, BODY, t.text, look, w));
                });
            }
            Block::Code { lang, text } => {
                egui::Frame::new()
                    .fill(look.code_bg)
                    .stroke(Stroke::new(1.0, look.border))
                    .corner_radius(CornerRadius::same(8))
                    .inner_margin(Margin::symmetric(10, 8))
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        if !lang.is_empty() {
                            ui.label(RichText::new(lang.as_str()).font(Tokens::ui(10.5)).color(t.text_faint));
                        }
                        ui.add(egui::Label::new(RichText::new(text.as_str()).font(Tokens::mono(11.5)).color(t.text_dim)).wrap());
                    });
            }
            Block::Rule => {
                let (r, _) = ui.allocate_exact_size(vec2(width, 9.0), Sense::hover());
                ui.painter().line_segment([r.left_center(), r.right_center()], Stroke::new(1.0, t.separator));
            }
        }
    }
}

fn spans_job(spans: &[Span], size: f32, color: Color32, look: &Look, width: f32) -> LayoutJob {
    let mut job = LayoutJob::default();
    job.wrap.max_width = width;
    for s in spans {
        let mut f = TextFormat { font_id: if s.bold { Tokens::semibold(size) } else { Tokens::ui(size) }, color, italics: s.italic, ..Default::default() };
        if s.code {
            f.font_id = Tokens::mono(size - 1.5);
            f.color = look.magic;
            f.background = look.code_bg;
        }
        job.append(&s.text, 0.0, f);
    }
    job
}

/// What a tool call did, in words, and its icon.
fn tool_title(name: &str, state: ToolState, input: &str, frames: usize) -> (Icon, String) {
    let running = matches!(state, ToolState::Pending | ToolState::Running);
    let arg = |k: &str| serde_json::from_str::<Value>(input).ok().and_then(|v| v.get(k).and_then(Value::as_str).map(str::to_string));
    let pick = |a: &str, b: String| if running { a.to_string() } else { b };
    match name {
        "run_script" => {
            (Icon::Code, pick(tr("Writing a script…"), if state == ToolState::Failed { tr("Script error").into() } else { tr("Ran a script").into() }))
        }
        "render_frames" => (Icon::Snapshot, pick(tr("Rendering frames…"), tr_args("Checked {} rendered frames", &[&frames]))),
        "execute_command" => {
            let cmd = arg("command").unwrap_or_default();
            (Icon::Fx, pick(&tr_args("Running {}…", &[&cmd]), tr_args("Ran {}", &[&cmd])))
        }
        "get_project" => (Icon::Search, pick(tr("Reading the project…"), tr("Read the project").into())),
        "get_comp" => (Icon::Comp, pick(tr("Reading the composition…"), tr("Read the composition").into())),
        "get_layer" => (Icon::Search, pick(tr("Reading a layer…"), tr("Read a layer").into())),
        "get_property" => (Icon::Search, pick(tr("Reading a property…"), tr_args("Read {}", &[&arg("path").unwrap_or_default()]))),
        "set_property" => (Icon::Keyframe, pick(tr("Setting a property…"), tr_args("Set {}", &[&arg("path").unwrap_or_default()]))),
        "add_keyframes" => (Icon::Keyframe, pick(tr("Adding keyframes…"), tr_args("Keyframed {}", &[&arg("path").unwrap_or_default()]))),
        "add_effect" => (Icon::Fx, pick(tr("Adding an effect…"), tr_args("Added {}", &[&arg("effect").unwrap_or_else(|| tr("an effect").into())]))),
        "list_effects" | "list_fonts" | "list_commands" | "describe_command" => (Icon::Search, pick(tr("Looking things up…"), tr("Looked things up").into())),
        "import_file" => (Icon::Image, pick(tr("Importing a file…"), tr("Imported a file").into())),
        "propose_plan" => (Icon::Flowchart, pick(tr("Writing a plan…"), tr("Proposed a plan").into())),
        "resolve_note" => {
            let id = serde_json::from_str::<Value>(input).ok().and_then(|v| v.get("id").and_then(Value::as_u64)).unwrap_or(0);
            let declined = input.contains("wont_fix");
            (Icon::Chat, pick(tr("Resolving a note…"), if declined { tr_args("Declined note {}", &[&id]) } else { tr_args("Resolved note {}", &[&id]) }))
        }
        other => (Icon::Gear, other.to_string()),
    }
}

#[allow(clippy::too_many_arguments)]
fn tool_card(
    ui: &mut egui::Ui,
    ctx: &egui::Context,
    rt: &mut Runtime,
    auto: &mut crate::automation::Registry,
    acts: &mut Vec<Act>,
    look: &Look,
    i: usize,
    (id, name, input, state, summary, frames, open): (&String, &String, &String, ToolState, &String, usize, bool),
) {
    let t = look.t;
    let (icon, title) = tool_title(name, state, input, frames);
    let failed = state == ToolState::Failed;
    let border = if failed { t.danger.gamma_multiply(0.6) } else { look.border };
    egui::Frame::new().fill(look.card).stroke(Stroke::new(1.0, border)).corner_radius(CornerRadius::same(8)).inner_margin(Margin::symmetric(9, 6)).show(
        ui,
        |ui| {
            ui.set_width(ui.available_width());
            let (row, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 20.0), Sense::click());
            let p = ui.painter();
            let cy = row.center().y;
            let status = Rect::from_center_size(pos2(row.min.x + 7.0, cy), vec2(14.0, 14.0));
            match state {
                ToolState::Pending | ToolState::Running => spinner(ui, status.shrink(1.0), look.magic),
                ToolState::Done => {
                    p.circle_filled(status.center(), 6.5, t.cache_green.gamma_multiply(0.9));
                    let c = status.center();
                    p.line_segment([c + vec2(-3.0, 0.2), c + vec2(-0.8, 2.4)], Stroke::new(1.6, Color32::WHITE));
                    p.line_segment([c + vec2(-0.8, 2.4), c + vec2(3.2, -2.2)], Stroke::new(1.6, Color32::WHITE));
                }
                ToolState::Failed => {
                    p.circle_filled(status.center(), 6.5, t.danger);
                    let c = status.center();
                    p.line_segment([c + vec2(-2.4, -2.4), c + vec2(2.4, 2.4)], Stroke::new(1.6, Color32::WHITE));
                    p.line_segment([c + vec2(-2.4, 2.4), c + vec2(2.4, -2.4)], Stroke::new(1.6, Color32::WHITE));
                }
            }
            icons::paint(p, Rect::from_center_size(pos2(row.min.x + 25.0, cy), vec2(13.0, 13.0)), icon, t.text_dim);
            let hot = ctx.animate_bool_with_time(resp.id, resp.hovered(), 0.12);
            let mut job = LayoutJob::simple_singleline(title, Tokens::medium(12.0), lerp(t.text_dim, t.text, hot));
            job.wrap = egui::text::TextWrapping::truncate_at_width((row.width() - 60.0).max(10.0));
            let g = p.layout_job(job);
            p.galley(pos2(row.min.x + 38.0, cy - g.size().y / 2.0), g, t.text);
            icons::paint(
                p,
                Rect::from_center_size(pos2(row.max.x - 7.0, cy), vec2(10.0, 10.0)),
                if open { Icon::ChevronDown } else { Icon::ChevronRight },
                t.text_faint,
            );
            auto.add(&format!("merlin.item.{i}.toggle"), row, name);
            if resp.hovered() {
                ctx.set_cursor_icon(egui::CursorIcon::PointingHand);
            }
            if resp.clicked() {
                acts.push(Act::Toggle(i));
            }
            if open {
                if !input.is_empty() {
                    code_block(ui, input, look);
                }
                if !summary.is_empty() {
                    ui.add(egui::Label::new(RichText::new(summary).font(Tokens::mono(11.0)).color(if failed { t.danger } else { t.text_faint })).wrap());
                }
            } else if failed && !summary.is_empty() {
                let first: String = summary.lines().next().unwrap_or("").chars().take(140).collect();
                ui.add(egui::Label::new(RichText::new(first).font(Tokens::ui(11.5)).color(t.danger)).wrap());
            }
            if frames > 0 {
                ui.add_space(2.0);
                thumbnails(ui, ctx, rt, std::slice::from_ref(id), THUMB_W);
            }
        },
    );
}

fn code_block(ui: &mut egui::Ui, text: &str, look: &Look) {
    egui::Frame::new().fill(look.code_bg).corner_radius(CornerRadius::same(6)).inner_margin(Margin::symmetric(8, 6)).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.add(egui::Label::new(RichText::new(text).font(Tokens::mono(11.0)).color(look.t.text_dim)).wrap());
    });
}

#[allow(clippy::too_many_arguments)]
fn turn_card(
    ui: &mut egui::Ui,
    rt: &mut Runtime,
    auto: &mut crate::automation::Registry,
    acts: &mut Vec<Act>,
    look: &Look,
    latest: &[u64],
    (id, label, outcome, decision, tool_calls, input_tokens, output_tokens, cost): (u64, &String, &String, Decision, usize, u64, u64, Option<f64>),
    before: bool,
) {
    let t = look.t;
    let mut stats = tr_args("{} tool calls · {} in · {} out", &[&tool_calls, &tokens(input_tokens), &tokens(output_tokens)]);
    if let Some(c) = cost {
        stats.push_str(&format!(" · ≈ {}", merlin_agent::pricing::format(c)));
    }
    if decision != Decision::Pending {
        // Decided: one quiet line.
        let what = match decision {
            Decision::Kept => tr("Kept"),
            Decision::Reverted => tr("Reverted"),
            _ => tr("No changes"),
        };
        ui.horizontal(|ui| {
            let (r, _) = ui.allocate_exact_size(vec2(14.0, 14.0), Sense::hover());
            let c = r.center();
            let col = if decision == Decision::Reverted { t.text_faint } else { t.cache_green };
            ui.painter().circle_stroke(c, 5.5, Stroke::new(1.3, col));
            ui.label(RichText::new(format!("{outcome} · {what}")).font(Tokens::medium(11.5)).color(t.text_dim));
            ui.label(RichText::new(stats).font(Tokens::ui(11.0)).color(t.text_faint));
        });
        return;
    }
    let can = latest.contains(&id);
    egui::Frame::new()
        .fill(look.card)
        .stroke(Stroke::new(1.0, look.magic.gamma_multiply(0.45)))
        .corner_radius(CornerRadius::same(10))
        .inner_margin(Margin::symmetric(12, 10))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                let (r, _) = ui.allocate_exact_size(vec2(16.0, 16.0), Sense::hover());
                ui.painter().circle_filled(r.center(), 7.5, look.magic.gamma_multiply(0.25));
                icons::paint(ui.painter(), r.shrink(3.0), Icon::Sparkle, look.magic);
                ui.label(RichText::new(tr("Changes applied")).font(Tokens::semibold(12.5)).color(t.text));
                ui.label(RichText::new(format!("· {outcome}")).font(Tokens::ui(12.0)).color(t.text_dim));
            });
            let mut job = LayoutJob::simple_singleline(format!("{label} · {stats}"), Tokens::ui(11.0), t.text_faint);
            job.wrap = egui::text::TextWrapping::truncate_at_width(ui.available_width());
            ui.label(job);
            // Before / after: frames across the comp.
            let (bk, ak) = (format!("turn-before:{id}"), format!("turn-after:{id}"));
            if rt.frames.contains_key(&bk) && rt.frames.contains_key(&ak) {
                ui.add_space(6.0);
                let ctx = ui.ctx().clone();
                // Both sets decode up front so the toggle swaps without a gap; until one is ready, its
                // place is kept at the other's size.
                let (shown, other) = if before { (&bk, &ak) } else { (&ak, &bk) };
                let other: Vec<egui::TextureHandle> = textures(&ctx, rt, other).to_vec();
                let texs: Vec<egui::TextureHandle> = textures(&ctx, rt, shown).to_vec();
                let sizes: Vec<[usize; 2]> = if texs.is_empty() { other.iter().map(|t| t.size()).collect() } else { texs.iter().map(|t| t.size()).collect() };
                let n = sizes.len().max(1) as f32;
                let w = ((ui.available_width() - 3.0 * (n - 1.0)) / n).max(20.0);
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 3.0;
                    for (k, [tw, th]) in sizes.iter().enumerate() {
                        let size = vec2(w, w * (*th).max(1) as f32 / (*tw).max(1) as f32);
                        match texs.get(k) {
                            Some(tx) => {
                                ui.add(egui::Image::from_texture(egui::load::SizedTexture::new(tx.id(), size)).corner_radius(3.0));
                            }
                            None => {
                                let (r, _) = ui.allocate_exact_size(size, Sense::hover());
                                ui.painter().rect_filled(r, 3.0, t.app_bg);
                            }
                        }
                    }
                });
                ui.horizontal(|ui| {
                    for (label, on_before, key) in [(tr("After"), false, "after"), (tr("Before"), true, "before")] {
                        let on = before == on_before;
                        let b = ui.selectable_label(on, RichText::new(label).font(Tokens::medium(11.0)));
                        auto.add_response(&format!("merlin.turn.{id}.{key}"), &b, label);
                        if b.clicked() && !on {
                            acts.push(Act::Compare(id));
                        }
                    }
                    ui.label(RichText::new(tr("across the comp")).font(Tokens::ui(10.5)).color(t.text_faint));
                });
            }
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                let keep = pill(ui, tr("Keep"), true, true, look, &format!("keep{id}"));
                auto.add(&format!("merlin.turn.{id}.keep"), keep.rect, "Keep");
                if keep.on_hover_text(tr("Keep these changes")).clicked() {
                    acts.push(Act::Keep(id));
                }
                let revert = pill(ui, tr("Revert"), false, can, look, &format!("revert{id}"));
                auto.add(&format!("merlin.turn.{id}.revert"), revert.rect, "Revert");
                let revert = if can {
                    revert.on_hover_text(tr("Undo this whole turn"))
                } else {
                    revert.on_hover_text(tr("There are later edits: use Window ▸ History"))
                };
                if can && revert.clicked() {
                    acts.push(Act::Revert(id));
                }
            });
        });
}

fn notice_ui(ui: &mut egui::Ui, text: &str, error: bool, look: &Look) {
    let t = look.t;
    if !error {
        ui.add(egui::Label::new(RichText::new(text).font(Tokens::ui(11.5)).color(t.text_faint)).wrap());
        return;
    }
    egui::Frame::new()
        .fill(t.danger.gamma_multiply(0.12))
        .stroke(Stroke::new(1.0, t.danger.gamma_multiply(0.45)))
        .corner_radius(CornerRadius::same(8))
        .inner_margin(Margin::symmetric(10, 7))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.add(egui::Label::new(RichText::new(text).font(Tokens::ui(12.0)).color(t.text)).wrap());
        });
}

fn tokens(n: u64) -> String {
    if n >= 10_000 { format!("{}k", n / 1000) } else { n.to_string() }
}

/// The textures of the images under `key` in [`Runtime::frames`]: decoded (and scaled down to
/// thumbnail size) off the UI thread on first use, so empty until then.
pub(super) fn textures<'a>(ctx: &egui::Context, rt: &'a mut Runtime, key: &str) -> &'a [egui::TextureHandle] {
    if !rt.textures.contains_key(key) {
        decode(ctx, rt, key);
    }
    rt.textures.get(key).map(Vec::as_slice).unwrap_or_default()
}

/// The longest side images are decoded at for the chat (they show at most ~240 px wide).
const DECODE_SIDE: u32 = 480;

/// Start decoding `key`'s images, or take them in once decoded.
fn decode(ctx: &egui::Context, rt: &mut Runtime, key: &str) {
    let Some(pngs) = rt.frames.get(key) else { return };
    let sig = signature(pngs);
    if let Some((s, slot)) = rt.decoding.get(key) {
        let done = slot.lock().ok().and_then(|mut d| d.take());
        let fresh = *s == sig;
        match done {
            Some(images) if fresh => {
                let texs = images
                    .into_iter()
                    .enumerate()
                    .filter_map(|(i, ci)| Some(ctx.load_texture(format!("merlin-{key}-{i}"), ci?, egui::TextureOptions::LINEAR)))
                    .collect();
                rt.textures.insert(key.to_string(), texs);
                rt.decoding.remove(key);
                return;
            }
            None if fresh => {
                ctx.request_repaint_after(std::time::Duration::from_millis(16));
                return;
            }
            // The images changed while decoding: start over.
            _ => {}
        }
    }
    let pngs = pngs.clone();
    let slot: native::DecodeSlot = Default::default();
    let out = slot.clone();
    let spawned = std::thread::Builder::new().name("merlin-decode".into()).spawn(move || {
        let images = pngs
            .iter()
            .map(|png| {
                let img = image::load_from_memory(png).ok()?;
                let img = if img.width().max(img.height()) > DECODE_SIDE { img.thumbnail(DECODE_SIDE, DECODE_SIDE) } else { img };
                let img = img.to_rgba8();
                Some(egui::ColorImage::from_rgba_unmultiplied([img.width() as usize, img.height() as usize], img.as_raw()))
            })
            .collect();
        if let Ok(mut o) = out.lock() {
            *o = Some(images);
        }
    });
    if spawned.is_ok() {
        rt.decoding.insert(key.to_string(), (sig, slot));
        ctx.request_repaint_after(std::time::Duration::from_millis(16));
    } else {
        rt.textures.insert(key.to_string(), vec![]);
    }
}

/// Tells one set of images from another (to drop a decode whose images were replaced meanwhile).
fn signature(pngs: &[Vec<u8>]) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    pngs.hash(&mut h);
    h.finish()
}

/// Whether `key`'s images are still being decoded.
pub(super) fn decoding(rt: &Runtime, key: &str) -> bool {
    rt.decoding.contains_key(key)
}

/// A row of the images under `keys` (a tool call's rendered frames, a message's attachments).
fn thumbnails(ui: &mut egui::Ui, ctx: &egui::Context, rt: &mut Runtime, keys: &[String], width: f32) {
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = vec2(6.0, 6.0);
        for key in keys {
            for tex in textures(ctx, rt, key) {
                let [w, h] = tex.size();
                let aspect = if w > 0 { (h as f32 / w as f32).clamp(0.2, 2.0) } else { 0.5625 };
                let size = vec2(width, width * aspect);
                ui.add(egui::Image::from_texture(egui::load::SizedTexture::new(tex.id(), size)).corner_radius(6.0));
            }
        }
    });
}

/// Over the panel while files are dragged onto it.
fn drop_hint(app: &mut EffectcraftApp, ui: &egui::Ui, rect: Rect, look: &Look) {
    let t = look.t;
    let painter = ui.ctx().layer_painter(egui::LayerId::new(egui::Order::Foreground, egui::Id::unique("merlin-drop")));
    let r = rect.shrink(8.0);
    painter.rect_filled(r, 12.0, t.panel_bg.gamma_multiply(0.9));
    painter.rect_stroke(r, 12.0, Stroke::new(2.0, look.magic), StrokeKind::Inside);
    let c = r.center();
    painter.circle_filled(c - vec2(0.0, 18.0), 20.0, look.magic.gamma_multiply(0.18));
    icons::paint(&painter, Rect::from_center_size(c - vec2(0.0, 18.0), vec2(20.0, 20.0)), Icon::Image, look.magic);
    let label = tr("Drop to attach to your message");
    painter.text(c + vec2(0.0, 16.0), Align2::CENTER_CENTER, label, Tokens::semibold(13.0), t.text);
    painter.text(c + vec2(0.0, 34.0), Align2::CENTER_CENTER, tr("Images, PSD, SVG, PDF, AI"), Tokens::ui(11.5), t.text_faint);
    app.auto.add("merlin.dropTarget", rect, label);
}

/// A proposed plan: its beats (time, what happens) and Build / Discard.
#[allow(clippy::too_many_arguments)]
fn plan_card(
    ui: &mut egui::Ui,
    auto: &mut crate::automation::Registry,
    acts: &mut Vec<Act>,
    look: &Look,
    i: usize,
    title: &str,
    any_beats: bool,
    state: PlanState,
    busy: bool,
    beats: &mut dyn FnMut(&mut egui::Ui, &mut crate::automation::Registry),
) {
    let t = look.t;
    let live = state == PlanState::Proposed;
    ui.add_space(4.0);
    egui::Frame::new()
        .fill(look.card)
        .stroke(Stroke::new(1.0, if live { look.magic.gamma_multiply(0.7) } else { look.border }))
        .corner_radius(CornerRadius::same(10))
        .inner_margin(Margin::symmetric(12, 10))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                let (r, _) = ui.allocate_exact_size(vec2(14.0, 14.0), Sense::hover());
                icons::paint(ui.painter(), r, Icon::Flowchart, look.magic);
                ui.label(RichText::new(tr("Plan")).font(Tokens::semibold(11.0)).color(look.magic));
                ui.label(RichText::new(title).font(Tokens::semibold(13.0)).color(t.text));
            });
            ui.add_space(4.0);
            if !any_beats {
                ui.label(RichText::new(tr("Its beats were removed.")).font(Tokens::ui(11.5)).color(t.text_faint));
            }
            beats(ui, auto);
            ui.add_space(6.0);
            match state {
                PlanState::Proposed => {
                    ui.label(RichText::new(tr("Open a beat to edit or retime it, ask for changes, or build it.")).font(Tokens::ui(11.0)).color(t.text_faint));
                    ui.add_space(4.0);
                    ui.horizontal(|ui| {
                        let b = pill(ui, tr("Build it"), true, !busy, look, &format!("plan-build-{i}"));
                        auto.add(&format!("merlin.plan.{i}.build"), b.rect, "Build it");
                        if b.clicked() {
                            acts.push(Act::BuildPlan(i));
                        }
                        let d = pill(ui, tr("Discard"), false, !busy, look, &format!("plan-discard-{i}"));
                        auto.add(&format!("merlin.plan.{i}.discard"), d.rect, "Discard plan");
                        if d.clicked() {
                            acts.push(Act::DiscardPlan(i));
                        }
                    });
                }
                PlanState::Building => {
                    ui.label(RichText::new(tr("Building…")).font(Tokens::medium(11.5)).color(look.magic));
                }
                PlanState::Built => {
                    ui.label(RichText::new(tr("✓ Built")).font(Tokens::medium(11.5)).color(t.text_dim));
                }
                PlanState::Discarded => {
                    ui.label(RichText::new(tr("Discarded")).font(Tokens::medium(11.5)).color(t.text_faint));
                }
                PlanState::Revised => {
                    ui.label(RichText::new(tr("Replaced by a newer plan")).font(Tokens::medium(11.5)).color(t.text_faint));
                }
            }
        });
    ui.add_space(4.0);
}

// ------------------------------------------------------------------ composer

fn composer_ui(app: &mut EffectcraftApp, ui: &mut egui::Ui, r: Rect, look: &Look, now: f64, acts: &mut Vec<Act>) {
    let t = look.t;
    let busy = app.ui.merlin.busy;
    let mut inner = r.shrink2(vec2(10.0, 8.0));

    // What the message is pinned to, then the context Merlin will see, as quiet chips.
    if !app.ui.merlin.pins.is_empty() {
        let h = super::mentions::pins_ui(app, ui, Rect::from_min_size(inner.min, vec2(inner.width(), super::mentions::PIN_H)), look);
        inner.min.y += h + 4.0;
    }
    context_chips(app, ui, Rect::from_min_size(inner.min, vec2(inner.width(), 18.0)), look, acts);

    let boxr = Rect::from_min_max(pos2(inner.min.x, inner.min.y + 24.0), inner.max);
    let focused = ui.ctx().memory(|m| m.has_focus(input_id()));
    // The @ list takes the arrow keys and Enter while it shows.
    let mentioning = super::mentions::keys(app, ui, focused);
    let f = ui.ctx().animate_bool_with_time(egui::Id::unique("merlin-composer-focus"), focused, 0.18);
    ui.painter().rect_filled(boxr, 12.0, look.card);
    ui.painter().rect_stroke(boxr, 12.0, Stroke::new(1.0 + 0.5 * f, lerp(look.border, look.magic.gamma_multiply(0.8), f)), StrokeKind::Inside);

    let mut top = boxr.min.y + 8.0;
    if !app.ui.merlin.attachments.is_empty() {
        let h = chips_ui(app, ui, boxr.min.x + 8.0, boxr.max.x - 8.0, top, look, acts);
        top += h + 2.0;
    }
    let bar_h = 30.0;
    let text_r = Rect::from_min_max(pos2(boxr.min.x + 10.0, top), pos2(boxr.max.x - 10.0, boxr.max.y - bar_h - 2.0));

    // Enter sends, Shift+Enter starts a new line.
    if !busy && focused && !mentioning && ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Enter)) {
        acts.push(Act::Send);
    }
    let mut c = child(ui, text_r);
    c.set_clip_rect(text_r.intersect(ui.clip_rect()));
    let te = egui::TextEdit::multiline(&mut app.ui.merlin.input)
        .id(input_id())
        .frame(egui::Frame::NONE)
        .font(Tokens::ui(BODY))
        .desired_width(text_r.width())
        .desired_rows(((text_r.height() / 17.0).floor() as usize).max(2))
        .hint_text(RichText::new(tr("Ask Merlin… type @ to pin a layer or time")).color(t.text_faint));
    let resp = c.add(te);
    app.auto.add_response("merlin.input", &resp, "Message Merlin");
    if mentioning {
        super::mentions::popup_ui(app, ui, boxr, look);
    }

    // Bottom bar: attach, a hint, send / stop.
    let bar = Rect::from_min_max(pos2(boxr.min.x + 6.0, boxr.max.y - bar_h - 2.0), pos2(boxr.max.x - 6.0, boxr.max.y - 4.0));
    let attach = Rect::from_center_size(pos2(bar.min.x + 14.0, bar.center().y), vec2(26.0, 26.0));
    let a = icon_btn(ui, attach, Icon::Image, false, true, look, "attach");
    app.auto.add("merlin.attach", attach, "Attach files");
    if a.on_hover_text(tr("Attach images, Photoshop, SVG, PDF or Illustrator files (or drag them onto this panel)")).clicked() {
        acts.push(Act::Attach);
    }
    // Act | Plan.
    let mode = app.ui.merlin.mode;
    let seg_w = 40.0;
    let seg = Rect::from_min_size(pos2(attach.max.x + 6.0, bar.center().y - 11.0), vec2(seg_w * 2.0, 22.0));
    ui.painter().rect_filled(seg, 11.0, t.app_bg);
    for (k, (m, label, id, tip)) in [
        (Mode::Act, tr("Act"), "merlin.mode.act", tr("Act: Merlin builds right away")),
        (Mode::Plan, tr("Plan"), "merlin.mode.plan", tr("Plan: Merlin only looks, then proposes timed beats for you to edit and build")),
    ]
    .into_iter()
    .enumerate()
    {
        let r = Rect::from_min_size(pos2(seg.min.x + k as f32 * seg_w, seg.min.y), vec2(seg_w, 22.0));
        let resp = ui.interact(r, egui::Id::unique(("merlin-mode", k)), if busy { Sense::hover() } else { Sense::click() });
        let on = mode == m;
        if on {
            ui.painter().rect_filled(r.shrink(2.0), 9.0, if m == Mode::Plan { look.magic.gamma_multiply(0.3) } else { t.hover });
        }
        let col = if on { if m == Mode::Plan { look.magic } else { t.text } } else { t.text_faint };
        ui.painter().text(r.center(), Align2::CENTER_CENTER, label, Tokens::medium(11.0), col);
        app.auto.add(id, r, tip);
        if resp.on_hover_text(tip).clicked() {
            acts.push(Act::SetMode(m));
        }
    }
    // The model, then a hint when it fits before the send button.
    let model_r = model_pill(app, ui, pos2(seg.max.x + 6.0, bar.center().y), bar.max.x - 34.0 - seg.max.x - 6.0, look, acts);
    let hint = if mode == Mode::Plan { tr("Merlin plans; nothing is built yet") } else { tr("Enter to send · Shift+Enter for a new line") };
    let g = ui.painter().layout_no_wrap(hint.to_string(), Tokens::ui(10.5), t.text_faint);
    if model_r.max.x + 8.0 + g.size().x < bar.max.x - 34.0 {
        ui.painter().galley(pos2(model_r.max.x + 8.0, bar.center().y - g.size().y / 2.0), g, t.text_faint);
    }
    let send = Rect::from_center_size(pos2(bar.max.x - 14.0, bar.center().y), vec2(26.0, 26.0));
    let enabled = busy || native::can_send(app);
    let sresp = ui.interact(send, egui::Id::unique("merlin-send"), if enabled { Sense::click() } else { Sense::hover() });
    let hot = ui.ctx().animate_bool_with_time(sresp.id, enabled && sresp.hovered(), 0.12);
    let p = ui.painter();
    let c0 = send.center();
    if busy {
        // Stop: a square in a pulsing ring.
        let ring = 13.0 + 3.0 * wave(now, 3.0, 0.0);
        p.circle_filled(c0, ring, look.magic.gamma_multiply(0.15));
        p.circle_filled(c0, 12.0 + hot, t.text);
        p.rect_filled(Rect::from_center_size(c0, vec2(8.0, 8.0)), 2.0, t.panel_bg);
        tick(ui.ctx());
    } else {
        let fill = if enabled { lerp(t.accent, t.accent_hover, hot) } else { t.field_border };
        p.circle_filled(c0, 12.0 + hot, fill);
        let col = if enabled { Color32::WHITE } else { t.text_faint };
        let s = Stroke::new(1.8, col);
        p.line_segment([c0 + vec2(0.0, 5.0), c0 + vec2(0.0, -5.0)], s);
        p.line_segment([c0 + vec2(-4.0, -1.0), c0 + vec2(0.0, -5.0)], s);
        p.line_segment([c0 + vec2(4.0, -1.0), c0 + vec2(0.0, -5.0)], s);
    }
    app.auto.add("merlin.send", send, if busy { "Stop" } else { "Send" });
    if enabled && sresp.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    if sresp.on_hover_text(if busy { tr("Stop") } else { tr("Send") }).clicked() {
        acts.push(if busy { Act::Stop } else { Act::Send });
    }
}

/// The model in use as a quiet pill starting at `left_center` (at most `max_w` wide); a click opens
/// Settings. Returns its rect (empty when it doesn't fit).
fn model_pill(app: &mut EffectcraftApp, ui: &mut egui::Ui, left_center: egui::Pos2, max_w: f32, look: &Look, acts: &mut Vec<Act>) -> Rect {
    let t = look.t;
    let cy = left_center.y;
    if max_w < 50.0 {
        return Rect::from_min_size(left_center, Vec2::ZERO);
    }
    let cfg = app.ui.merlin.settings.config();
    let mut job = LayoutJob::simple_singleline(cfg.model().to_string(), Tokens::ui(11.0), t.text_dim);
    job.wrap = egui::text::TextWrapping::truncate_at_width((max_w - 26.0).min(150.0));
    let galley = ui.painter().layout_job(job);
    let r = Rect::from_min_size(pos2(left_center.x, cy - 11.0), vec2(galley.size().x + 26.0, 22.0));
    let resp = ui.interact(r, egui::Id::unique("merlin-model-pill"), Sense::click());
    let hot = ui.ctx().animate_bool_with_time(resp.id, resp.hovered(), 0.12);
    let p = ui.painter();
    if hot > 0.0 {
        p.rect_filled(r, 11.0, t.hover.gamma_multiply(hot));
    }
    p.galley(pos2(r.min.x + 9.0, cy - galley.size().y / 2.0), galley, lerp(t.text_dim, t.text, hot));
    icons::paint(p, Rect::from_center_size(pos2(r.max.x - 10.0, cy), vec2(9.0, 9.0)), Icon::ChevronDown, t.text_faint);
    app.auto.add("merlin.model", r, "Provider and model");
    if resp.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    let (tin, tout, cost) = native::chat_cost(&app.ui.merlin);
    let mut tip = format!("{} · {}\n{}", cfg.provider.label(), cfg.model(), tr("Change the provider or model"));
    if tin + tout > 0 {
        tip.push_str(&format!("\n{}", tr_args("This chat: {} in · {} out", &[&tokens(tin), &tokens(tout)])));
        if let Some(c) = cost {
            tip.push_str(&format!(" · ≈ {}", merlin_agent::pricing::format(c)));
        }
    }
    if resp.on_hover_text(tip).clicked() {
        acts.push(Act::ToggleSettings);
    }
    r
}

/// The active comp, selected layers and time, as small chips; clicking the layers or the time pins
/// them to the message.
fn context_chips(app: &mut EffectcraftApp, ui: &mut egui::Ui, r: Rect, look: &Look, acts: &mut Vec<Act>) {
    let t = look.t;
    let p = ui.painter().with_clip_rect(r.intersect(ui.clip_rect()));
    let mut x = r.min.x;
    let cy = r.center().y;
    let chip = |icon: Icon, text: String, x: &mut f32, hot: bool| {
        let col = if hot { t.text } else { t.text_faint };
        let g = p.layout_no_wrap(text, Tokens::ui(11.0), col);
        let w = g.size().x + 26.0;
        let cr = Rect::from_min_size(pos2(*x, cy - 9.0), vec2(w, 18.0));
        p.rect_filled(cr, 9.0, if hot { t.hover } else { t.hover.gamma_multiply(0.6) });
        icons::paint(&p, Rect::from_center_size(pos2(cr.min.x + 10.0, cy), vec2(10.0, 10.0)), icon, col);
        p.galley(pos2(cr.min.x + 18.0, cy - g.size().y / 2.0), g, col);
        *x += w + 4.0;
        cr
    };
    let hovered = |id: &str| ui.ctx().read_response(egui::Id::unique(("merlin-ctx", id))).is_some_and(|r| r.hovered());
    let (hot_layers, hot_time) = (hovered("layers"), hovered("time"));
    let s = &app.session;
    let mut clickable = vec![];
    match s.active_comp() {
        None => {
            chip(Icon::Comp, tr("No composition").to_string(), &mut x, false);
        }
        Some(comp) => {
            let name = s.active_comp_id().and_then(|id| s.project.item(id)).map(|i| i.name.clone()).unwrap_or_default();
            chip(Icon::Comp, short(&name, 22), &mut x, false);
            let sel = &s.state.selected_layers;
            let names: Vec<String> = comp.layers.iter().filter(|l| sel.contains(&l.id)).map(|l| l.name.clone()).collect();
            let layers = match names.as_slice() {
                [] => None,
                [one] => Some(chip(Icon::TextLayer, short(one, 22), &mut x, hot_layers)),
                _ => Some(chip(Icon::TextLayer, tr_args("{} layers", &[&names.len()]), &mut x, hot_layers)),
            };
            if let Some(cr) = layers {
                clickable.push(("layers", cr, tr("Pin the selected layers to your message (locked: Merlin edits only them)"), Act::PinLayers));
            }
            let cr = chip(Icon::Stopwatch, crate::panels::timecode(s, comp, s.time()), &mut x, hot_time);
            clickable.push(("time", cr, tr("Pin this frame to your message (or type @)"), Act::PinFrame));
        }
    }
    // The style kit, last.
    if r.max.x - x >= 30.0 {
        super::style_view::chip(app, ui, pos2(x, cy), r.max.x - x, look);
    }
    for (id, cr, tip, act) in clickable {
        let resp = ui.interact(cr, egui::Id::unique(("merlin-ctx", id)), Sense::click()).on_hover_cursor(egui::CursorIcon::PointingHand);
        app.auto.add(&format!("merlin.context.{id}"), cr, tip);
        if resp.on_hover_text(tip).clicked() {
            acts.push(act);
        }
    }
}

pub(super) fn short(s: &str, n: usize) -> String {
    if s.chars().count() > n { format!("{}…", s.chars().take(n.saturating_sub(1)).collect::<String>()) } else { s.to_string() }
}

/// The attachments of the message being written: thumbnail, name, ×. Laid out by hand so the
/// chips wrap onto more rows in a narrow panel; returns their height.
fn chips_ui(app: &mut EffectcraftApp, ui: &mut egui::Ui, left: f32, right: f32, top: f32, look: &Look, acts: &mut Vec<Act>) -> f32 {
    const PAD: f32 = 5.0;
    const THUMB: f32 = 18.0;
    const X_W: f32 = 12.0;
    const GAP: f32 = 4.0;
    let t = look.t;
    let ctx = ui.ctx().clone();
    let font = Tokens::ui(11.5);
    let row_h = CHIPS_H - GAP;
    let EffectcraftApp { ui: state, auto, .. } = app;
    let panel = &mut state.merlin;
    let (mut x, mut y) = (left, top);
    for i in 0..panel.attachments.len() {
        let Some(a) = panel.attachments.get(i).cloned() else { break };
        let name = short(&a.name, 26);
        let color = if a.state == AttachState::Failed { t.danger } else { t.text };
        let galley = ui.painter().layout_no_wrap(name, font.clone(), color);
        let w = (PAD + THUMB + GAP + galley.size().x + GAP + X_W + PAD).min(right - left);
        if x > left && x + w > right {
            x = left;
            y += CHIPS_H;
        }
        let chip = Rect::from_min_size(pos2(x, y), vec2(w, row_h));
        x += w + GAP;
        let k = native::chip_appear(&ctx, &mut panel.rt, &a.path);
        let stroke = if a.state == AttachState::Failed { t.danger } else { look.border };
        ui.painter().rect_filled(chip, 7.0, t.panel_bg.gamma_multiply(k));
        ui.painter().rect_stroke(chip, 7.0, Stroke::new(1.0, stroke.gamma_multiply(k)), StrokeKind::Inside);
        let slot = Rect::from_min_size(pos2(chip.min.x + PAD, chip.center().y - THUMB / 2.0), vec2(THUMB, THUMB));
        match a.state {
            AttachState::Loading => spinner(ui, slot.shrink(2.0), look.magic),
            AttachState::Ready => {
                if let Some(tex) = textures(&ctx, &mut panel.rt, &format!("att:{}", a.path)).first() {
                    let [tw, th] = tex.size();
                    let (tw, th) = (tw.max(1) as f32, th.max(1) as f32);
                    let s = (THUMB / tw).min(THUMB / th);
                    let ir = Rect::from_center_size(slot.center(), vec2(tw * s, th * s));
                    egui::Image::from_texture(egui::load::SizedTexture::new(tex.id(), ir.size())).corner_radius(3.0).paint_at(ui, ir);
                }
            }
            AttachState::Failed => {
                ui.painter().text(slot.center(), Align2::CENTER_CENTER, "!", Tokens::semibold(12.0), t.danger);
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
        if x_resp.hovered() {
            ui.painter().circle_filled(x_rect.center(), 6.5, t.hover);
        }
        icons::paint(ui.painter(), Rect::from_center_size(x_rect.center(), vec2(8.0, 8.0)), Icon::Close, if x_resp.hovered() { t.text } else { t.text_dim });
        auto.add(&format!("merlin.attach.{i}.remove"), x_rect, "Remove attachment");
        if x_resp.on_hover_text(tr("Remove")).clicked() {
            acts.push(Act::Unattach(i));
        }
    }
    let used = y + CHIPS_H - top;
    panel.rt.chips_h = used;
    used
}

// ------------------------------------------------------------------ settings

pub(super) fn section(ui: &mut egui::Ui, title: &str, look: &Look, body: impl FnOnce(&mut egui::Ui)) {
    ui.label(RichText::new(title).font(Tokens::semibold(11.5)).color(look.t.text_faint));
    ui.add_space(2.0);
    egui::Frame::new().fill(look.card).stroke(Stroke::new(1.0, look.border)).corner_radius(CornerRadius::same(10)).inner_margin(Margin::same(12)).show(
        ui,
        |ui| {
            ui.set_width(ui.available_width());
            body(ui);
        },
    );
    ui.add_space(12.0);
}

fn settings_ui(app: &mut EffectcraftApp, ui: &mut egui::Ui, r: Rect, look: &Look, acts: &mut Vec<Act>) {
    let t = look.t;
    let before = app.ui.merlin.settings.clone();
    let kind = before.kind();
    native::refresh_key_status(app, kind);
    let mut c = child(ui, r.shrink2(vec2(14.0, 12.0)));
    let EffectcraftApp { ui: state, auto, .. } = app;
    let panel = &mut state.merlin;
    egui::ScrollArea::vertical().id_salt("merlin-settings-scroll").auto_shrink([false, false]).show(&mut c, |ui| {
        ui.label(RichText::new(tr("Settings")).font(Tokens::semibold(15.0)).color(t.text));
        ui.add_space(10.0);
        let s = &mut panel.settings;
        section(ui, tr("MODEL"), look, |ui| {
            egui::Grid::new("merlin-settings").num_columns(2).spacing(vec2(10.0, 10.0)).show(ui, |ui| {
                ui.label(RichText::new(tr("Provider")).color(t.text_dim));
                let resp = egui::ComboBox::from_id_salt("merlin-provider").selected_text(kind.label()).width(200.0).show_ui(ui, |ui| {
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
                        let resp = ui.add(egui::TextEdit::singleline(&mut s.model).hint_text(default).desired_width(150.0));
                        auto.add_response("merlin.settings.model", &resp, "Model");
                        if !kind.presets().is_empty() {
                            let resp = egui::ComboBox::from_id_salt("merlin-presets").selected_text(tr("Presets")).width(80.0).show_ui(ui, |ui| {
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
                    let resp = ui.add(egui::TextEdit::singleline(&mut s.base_url).hint_text(kind.default_base_url()).desired_width(240.0));
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
            });
            if kind == ProviderKind::Demo {
                ui.add_space(6.0);
                ui.add(
                    egui::Label::new(
                        RichText::new(tr("Demo plays a scripted turn without calling any model: a quick way to see how Merlin works."))
                            .font(Tokens::ui(11.5))
                            .color(t.text_faint),
                    )
                    .wrap(),
                );
            }
        });

        if kind != ProviderKind::Demo {
            section(ui, tr("API KEY"), look, |ui| {
                let checking = native::key_checking(&panel.rt);
                let (status, ok) = match &panel.rt.key_status {
                    _ if checking => (tr("Checking…").to_string(), true),
                    Some((_, KeySource::Env, m)) => (tr_args("From {} ({})", &[&kind.env_var(), m]), true),
                    Some((_, KeySource::Keychain, m)) => (tr_args("Stored in the keychain ({})", &[m]), true),
                    _ if kind.needs_key() => (tr("Not set").to_string(), false),
                    _ => (tr("Not needed for most local servers").to_string(), true),
                };
                ui.horizontal(|ui| {
                    let (r, _) = ui.allocate_exact_size(vec2(10.0, 14.0), Sense::hover());
                    ui.painter().circle_filled(r.center(), 4.0, if ok { t.cache_green } else { t.warning });
                    ui.label(RichText::new(status).color(t.text));
                });
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    let resp = ui.add(egui::TextEdit::singleline(&mut panel.rt.key_draft).password(true).hint_text(tr("Paste a key")).desired_width(160.0));
                    auto.add_response("merlin.settings.key", &resp, "API key");
                    let save = pill(ui, tr("Save"), true, !panel.rt.key_draft.trim().is_empty(), look, "saveKey");
                    auto.add("merlin.settings.saveKey", save.rect, "Save key");
                    if save.clicked() && !panel.rt.key_draft.trim().is_empty() {
                        acts.push(Act::SaveKey);
                    }
                    let has = matches!(panel.rt.key_status, Some((_, KeySource::Keychain, _)));
                    let remove = pill(ui, tr("Remove"), false, has, look, "removeKey");
                    auto.add("merlin.settings.removeKey", remove.rect, "Remove key");
                    if has && remove.clicked() {
                        acts.push(Act::RemoveKey);
                    }
                });
                ui.add_space(6.0);
                ui.add(
                    egui::Label::new(
                        RichText::new(tr(
                            "Keys are kept in the OS keychain, never in settings or project files. An environment variable overrides the stored key.",
                        ))
                        .font(Tokens::ui(11.0))
                        .color(t.text_faint),
                    )
                    .wrap(),
                );
            });
        }
    });
    if app.ui.merlin.settings != before {
        let now = app.now();
        app.ui.merlin.rt.settings_dirty = Some(now);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_titles_read_as_sentences() {
        assert_eq!(tool_title("run_script", ToolState::Running, "", 0).1, "Writing a script…");
        assert_eq!(tool_title("run_script", ToolState::Done, "", 0).1, "Ran a script");
        assert_eq!(tool_title("render_frames", ToolState::Done, "", 4).1, "Checked 4 rendered frames");
        assert_eq!(tool_title("execute_command", ToolState::Done, r#"{"command":"layer.newText"}"#, 0).1, "Ran layer.newText");
        assert_eq!(tool_title("set_property", ToolState::Done, "not json", 0).1, "Set ");
        assert_eq!(tool_title("mystery", ToolState::Done, "", 0).1, "mystery");
        assert_eq!(short("abcdef", 4), "abc…");
        assert_eq!(short("abc", 4), "abc");
        assert_eq!(tokens(25_398), "25k");
    }
}
