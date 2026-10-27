//! The style kit view (the palette button in Merlin's header): pick the project's kit from the
//! library, or make one, and edit its palette, fonts, motion feel and brand notes. Merlin gets the
//! active kit with every message.

use std::sync::mpsc;

use egui::{Color32, Rect, RichText, Sense, Stroke, StrokeKind, pos2, vec2};
use merlin_agent::style::{Feel, StyleKit, Swatch, extract_palette, hex, parse_hex};
use serde_json::json;

use super::native;
use super::view::{Look, child, icon_btn, pill, section};
use crate::EffectcraftApp;
use crate::i18n::{tr, tr_args};
use crate::icons::Icon;
use crate::theme::Tokens;

/// Colours "From image…" adds.
const FROM_IMAGE: usize = 6;

enum Act {
    Select(String),
    Rename(String),
    New,
    Duplicate,
    Delete,
    AddColor,
    RemoveColor(usize),
    FromAttachment(String),
    PickImage,
    AddFont(String),
    RemoveFont(usize),
}

/// A row of a kit's colours as small dots (the header button, the composer chip).
pub(super) fn paint_dots(p: &egui::Painter, c: egui::Pos2, kit: Option<&StyleKit>, look: &Look, r: f32) {
    let cols: Vec<Color32> = match kit {
        Some(k) if !k.palette.is_empty() => k.palette.iter().take(3).filter_map(|s| parse_hex(&s.hex)).map(|[r, g, b]| Color32::from_rgb(r, g, b)).collect(),
        _ => vec![Color32::from_rgb(0xf2, 0x6b, 0x21), look.magic, Color32::from_rgb(0x38, 0xbd, 0xf8)],
    };
    let n = cols.len() as f32;
    for (i, col) in cols.into_iter().enumerate() {
        let x = c.x + (i as f32 - (n - 1.0) / 2.0) * r * 1.2;
        p.circle_filled(pos2(x, c.y), r, col);
        p.circle_stroke(pos2(x, c.y), r, Stroke::new(1.0, look.t.panel_bg));
    }
}

fn unique_name(kits: &[StyleKit], base: &str) -> String {
    if !kits.iter().any(|k| k.name == base) {
        return base.to_string();
    }
    (2..).map(|n| format!("{base} {n}")).find(|n| !kits.iter().any(|k| k.name == *n)).unwrap_or_else(|| base.to_string())
}

/// The view, in `r`.
pub(super) fn show(app: &mut EffectcraftApp, ui: &mut egui::Ui, r: Rect, look: &Look) {
    let t = look.t;
    take_picks(app);
    if app.ui.merlin.rt.font_list.is_none() {
        let fams = app.session.execute("text.fonts", json!({})).ok();
        let list: Vec<String> = fams
            .and_then(|v| v["families"].as_array().map(|a| a.iter().filter_map(|f| f["family"].as_str().map(str::to_string)).collect()))
            .unwrap_or_default();
        app.ui.merlin.rt.font_list = Some(list);
    }
    let before = (app.ui.merlin.rt.kits.clone(), app.ui.merlin.active_style.clone());
    let attachments: Vec<(String, String)> =
        app.ui.merlin.attachments.iter().filter(|a| a.state == super::AttachState::Ready).map(|a| (a.path.clone(), a.name.clone())).collect();
    let mut acts = vec![];
    let mut c = child(ui, r.shrink2(vec2(14.0, 12.0)));
    let EffectcraftApp { ui: state, auto, .. } = app;
    let panel = &mut state.merlin;
    let active = panel.active_style.clone();
    let fonts = panel.rt.font_list.clone().unwrap_or_default();
    egui::ScrollArea::vertical().id_salt("merlin-style-scroll").auto_shrink([false, false]).show(&mut c, |ui| {
        ui.label(RichText::new(tr("Style kit")).font(Tokens::semibold(15.0)).color(t.text));
        ui.label(
            RichText::new(tr("Merlin follows the project's kit in every message, so you don't have to repeat your colours, fonts and taste."))
                .font(Tokens::ui(11.5))
                .color(t.text_faint),
        );
        ui.add_space(10.0);
        section(ui, tr("KIT"), look, |ui| {
            ui.horizontal_wrapped(|ui| {
                let shown = if active.is_empty() { tr("None").to_string() } else { active.clone() };
                let w = ui.available_width().clamp(80.0, 160.0);
                let resp = egui::ComboBox::from_id_salt("merlin-style-kit").selected_text(shown).width(w).show_ui(ui, |ui| {
                    if ui.selectable_label(active.is_empty(), tr("None")).clicked() {
                        acts.push(Act::Select(String::new()));
                    }
                    for k in &panel.rt.kits {
                        if ui.selectable_label(k.name == active, &k.name).clicked() {
                            acts.push(Act::Select(k.name.clone()));
                        }
                    }
                });
                auto.add_response("merlin.style.kit", &resp.response, "Style kit");
                let b = ui.button(tr("New"));
                auto.add_response("merlin.style.new", &b, "New kit");
                if b.clicked() {
                    acts.push(Act::New);
                }
                if !active.is_empty() {
                    let d = ui.button(tr("Duplicate"));
                    auto.add_response("merlin.style.duplicate", &d, "Duplicate kit");
                    if d.clicked() {
                        acts.push(Act::Duplicate);
                    }
                    let x = ui.button(tr("Delete"));
                    auto.add_response("merlin.style.delete", &x, "Delete kit");
                    if x.clicked() {
                        acts.push(Act::Delete);
                    }
                }
            });
        });
        let Some(kit) = panel.rt.kits.iter_mut().find(|k| k.name == active && !active.is_empty()) else {
            ui.label(
                RichText::new(tr("No kit: Merlin picks colours and type itself. Make a kit, or ask Merlin to remember your style."))
                    .font(Tokens::ui(11.5))
                    .color(t.text_faint),
            );
            return;
        };
        section(ui, tr("NAME"), look, |ui| {
            let mut name = kit.name.clone();
            let resp = ui.add(egui::TextEdit::singleline(&mut name).desired_width(ui.available_width()));
            auto.add_response("merlin.style.name", &resp, "Kit name");
            if resp.changed() && !name.trim().is_empty() {
                acts.push(Act::Rename(name));
            }
        });
        section(ui, tr("PALETTE"), look, |ui| {
            for (i, sw) in kit.palette.iter_mut().enumerate() {
                ui.horizontal(|ui| {
                    let mut rgb = parse_hex(&sw.hex).unwrap_or([128, 128, 128]);
                    let resp = egui::widgets::color_picker::color_edit_button_srgb(ui, &mut rgb);
                    auto.add_response(&format!("merlin.style.color.{i}"), &resp, &sw.hex);
                    if resp.changed() {
                        sw.hex = hex(rgb);
                    }
                    let n =
                        ui.add(egui::TextEdit::singleline(&mut sw.name).desired_width((ui.available_width() - 110.0).clamp(50.0, 140.0)).hint_text(tr("Name")));
                    auto.add_response(&format!("merlin.style.color.{i}.name"), &n, "Colour name");
                    ui.label(RichText::new(&sw.hex).font(Tokens::mono(11.0)).color(t.text_faint));
                    let (x, _) = ui.allocate_exact_size(vec2(20.0, 20.0), Sense::hover());
                    if icon_btn(ui, x, Icon::Close, false, true, look, &format!("style-color-x-{i}")).on_hover_text(tr("Remove")).clicked() {
                        acts.push(Act::RemoveColor(i));
                    }
                    auto.add(&format!("merlin.style.color.{i}.remove"), x, "Remove colour");
                });
            }
            ui.horizontal_wrapped(|ui| {
                let a = pill(ui, tr("Add colour"), false, true, look, "style-add-color");
                auto.add("merlin.style.addColor", a.rect, "Add colour");
                if a.clicked() {
                    acts.push(Act::AddColor);
                }
                let f = pill(ui, tr("From image…"), false, true, look, "style-from-image");
                auto.add("merlin.style.fromImage", f.rect, "Take colours from an image");
                if f.clicked() {
                    acts.push(Act::PickImage);
                }
                for (path, name) in &attachments {
                    let b = pill(ui, &tr_args("From {}", &[name]), false, true, look, &format!("style-from-{path}"));
                    if b.on_hover_text(tr("Take colours from this attached image")).clicked() {
                        acts.push(Act::FromAttachment(path.clone()));
                    }
                }
            });
        });
        section(ui, tr("FONTS"), look, |ui| {
            for (i, f) in kit.fonts.iter().enumerate() {
                ui.horizontal(|ui| {
                    ui.label(RichText::new(f).font(Tokens::ui(12.5)).color(t.text));
                    let (x, _) = ui.allocate_exact_size(vec2(20.0, 20.0), Sense::hover());
                    if icon_btn(ui, x, Icon::Close, false, true, look, &format!("style-font-x-{i}")).on_hover_text(tr("Remove")).clicked() {
                        acts.push(Act::RemoveFont(i));
                    }
                    auto.add(&format!("merlin.style.font.{i}.remove"), x, "Remove font");
                });
            }
            let resp = egui::ComboBox::from_id_salt("merlin-style-add-font")
                .selected_text(tr("Add a font…"))
                .width(ui.available_width().clamp(80.0, 220.0))
                .height(320.0)
                .show_ui(ui, |ui| {
                    for f in fonts.iter().filter(|f| !kit.fonts.contains(f)) {
                        if ui.selectable_label(false, f).clicked() {
                            acts.push(Act::AddFont(f.clone()));
                        }
                    }
                });
            auto.add_response("merlin.style.addFont", &resp.response, "Add a font");
        });
        section(ui, tr("MOTION"), look, |ui| {
            ui.horizontal_wrapped(|ui| {
                for f in Feel::ALL {
                    let on = kit.motion.feel == f;
                    let b = ui.selectable_label(on, tr(f.label()));
                    auto.add_response(&format!("merlin.style.feel.{}", f.label().to_lowercase()), &b, f.label());
                    if b.clicked() {
                        kit.motion.feel = f;
                    }
                }
            });
            ui.spacing_mut().slider_width = (ui.available_width() - 120.0).max(60.0);
            let mut pct = kit.motion.overshoot * 100.0;
            let resp = ui.add(egui::Slider::new(&mut pct, 0.0..=50.0).suffix("%").text(tr("Overshoot")));
            auto.add_response("merlin.style.overshoot", &resp, "Overshoot");
            if resp.changed() {
                kit.motion.overshoot = (pct / 100.0).clamp(0.0, 0.5);
            }
        });
        section(ui, tr("NOTES"), look, |ui| {
            let resp = ui.add(
                egui::TextEdit::multiline(&mut kit.notes)
                    .desired_rows(3)
                    .desired_width(ui.available_width())
                    .hint_text(RichText::new(tr("Brand rules and taste, e.g. “Never pure white; logo bottom right; type tracks wide”")).color(t.text_faint)),
            );
            auto.add_response("merlin.style.notes", &resp, "Brand notes");
        });
    });
    for a in acts {
        apply(app, ui.ctx(), a);
    }
    if (app.ui.merlin.rt.kits.clone(), app.ui.merlin.active_style.clone()) != before {
        native::save_kits(app);
    }
}

fn apply(app: &mut EffectcraftApp, ctx: &egui::Context, a: Act) {
    let active = app.ui.merlin.active_style.clone();
    match a {
        Act::Select(n) => native::set_active_kit(app, &n),
        Act::Rename(n) => {
            if !app.ui.merlin.rt.kits.iter().any(|k| k.name == n)
                && let Some(k) = app.ui.merlin.rt.kits.iter_mut().find(|k| k.name == active)
            {
                k.name = n.clone();
                native::set_active_kit(app, &n);
            }
        }
        Act::New => {
            let name = unique_name(&app.ui.merlin.rt.kits, tr("New kit"));
            app.ui.merlin.rt.kits.push(StyleKit { name: name.clone(), ..Default::default() });
            native::set_active_kit(app, &name);
        }
        Act::Duplicate => {
            if let Some(k) = app.ui.merlin.rt.kits.iter().find(|k| k.name == active).cloned() {
                let name = unique_name(&app.ui.merlin.rt.kits, &tr_args("{} copy", &[&k.name]));
                app.ui.merlin.rt.kits.push(StyleKit { name: name.clone(), ..k });
                native::set_active_kit(app, &name);
            }
        }
        Act::Delete => {
            app.ui.merlin.rt.kits.retain(|k| k.name != active);
            native::set_active_kit(app, "");
        }
        Act::AddColor => {
            if let Some(k) = kit_mut(app, &active) {
                k.palette.push(Swatch { name: String::new(), hex: "#808080".into() });
            }
        }
        Act::RemoveColor(i) => {
            if let Some(k) = kit_mut(app, &active)
                && i < k.palette.len()
            {
                k.palette.remove(i);
            }
        }
        Act::AddFont(f) => {
            if let Some(k) = kit_mut(app, &active) {
                k.fonts.push(f);
            }
        }
        Act::RemoveFont(i) => {
            if let Some(k) = kit_mut(app, &active)
                && i < k.fonts.len()
            {
                k.fonts.remove(i);
            }
        }
        Act::FromAttachment(path) => {
            let png = app.ui.merlin.rt.frames.get(&format!("att:{path}")).and_then(|v| v.first()).cloned();
            match png {
                Some(png) => add_from_image(app, &png),
                None => app.ui.status = tr("That attachment isn't ready yet.").to_string(),
            }
        }
        Act::PickImage => {
            let exts = merlin_agent::attachment::extensions();
            if let Some(spawn) = app.hooks.file_dialog_async.as_ref() {
                let tx = app.ui.merlin.rt.style_picks.get_or_insert_with(mpsc::channel).0.clone();
                let c = ctx.clone();
                spawn(
                    crate::FileDialog::Open { exts: exts.iter().map(|e| e.to_string()).collect(), many: false },
                    Box::new(move |paths| {
                        let _ = tx.send(paths);
                        c.request_repaint();
                    }),
                );
            } else if let Some(pick) = app.hooks.pick_files.as_ref() {
                let paths = pick(&exts);
                image_paths(app, paths);
            }
        }
    }
}

fn kit_mut<'a>(app: &'a mut EffectcraftApp, name: &str) -> Option<&'a mut StyleKit> {
    app.ui.merlin.rt.kits.iter_mut().find(|k| k.name == name)
}

/// Images picked with "From image…".
fn take_picks(app: &mut EffectcraftApp) {
    let Some((tx, rx)) = app.ui.merlin.rt.style_picks.take() else { return };
    let mut paths = vec![];
    while let Ok(p) = rx.try_recv() {
        paths.extend(p);
    }
    app.ui.merlin.rt.style_picks = Some((tx, rx));
    if !paths.is_empty() {
        image_paths(app, paths);
    }
}

fn image_paths(app: &mut EffectcraftApp, paths: Vec<String>) {
    let Some(path) = paths.into_iter().next() else { return };
    match merlin_agent::attachment::load(&path) {
        Ok(a) => add_from_image(app, &a.png),
        Err(e) => app.ui.status = e,
    }
}

/// Add an image's main colours to the active kit.
pub(super) fn add_from_image(app: &mut EffectcraftApp, png: &[u8]) {
    let active = app.ui.merlin.active_style.clone();
    match extract_palette(png, FROM_IMAGE) {
        Ok(cols) => {
            let u = merlin_agent::style::StyleUpdate {
                colors: cols.into_iter().map(|c| Swatch { name: String::new(), hex: hex(c) }).collect(),
                ..Default::default()
            };
            if let Some(k) = app.ui.merlin.rt.kits.iter_mut().find(|k| k.name == active) {
                merlin_agent::style::merge(k, &u);
            }
            native::save_kits(app);
        }
        Err(e) => app.ui.status = e,
    }
}

/// The composer chip: the kit's colours and (room permitting, `room` wide) its name; opens the view.
pub(super) fn chip(app: &mut EffectcraftApp, ui: &mut egui::Ui, at: egui::Pos2, room: f32, look: &Look) -> f32 {
    let t = look.t;
    let kit = native::active_kit(&app.ui.merlin).cloned();
    let name = kit.as_ref().map(|k| k.name.clone()).unwrap_or_else(|| tr("No style").to_string());
    let mut g = ui.painter().layout_no_wrap(super::view::short(&name, 18), Tokens::ui(11.0), t.text_faint);
    let mut w = g.size().x + 44.0;
    if w > room {
        g = ui.painter().layout_no_wrap(String::new(), Tokens::ui(11.0), t.text_faint);
        w = 30.0;
    }
    let cr = Rect::from_min_size(pos2(at.x, at.y - 9.0), vec2(w, 18.0));
    let resp = ui.interact(cr, egui::Id::unique("merlin-style-chip"), Sense::click()).on_hover_cursor(egui::CursorIcon::PointingHand);
    let p = ui.painter();
    p.rect_filled(cr, 9.0, if resp.hovered() { t.hover } else { t.hover.gamma_multiply(0.6) });
    paint_dots(p, pos2(cr.min.x + 14.0, cr.center().y), kit.as_ref(), look, 3.5);
    p.galley(pos2(cr.min.x + 26.0, cr.center().y - g.size().y / 2.0), g, t.text_faint);
    p.rect_stroke(cr, 9.0, Stroke::new(0.0, Color32::TRANSPARENT), StrokeKind::Inside);
    app.auto.add("merlin.context.style", cr, "Style kit");
    if resp.on_hover_text(tr("The style kit Merlin follows (click to edit)")).clicked() {
        app.ui.merlin.style_open = true;
        app.ui.merlin.settings_open = false;
    }
    w
}
