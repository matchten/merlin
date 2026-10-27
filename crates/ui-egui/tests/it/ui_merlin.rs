//! Window ▸ Merlin: the agent panel runs a whole turn offline (the Demo provider), its tool calls go
//! through the UI thread like control requests, the turn becomes one undo step, and Keep / Revert
//! work (egui_kittest, UI logic only).

use effectcraft_ui_egui::EffectcraftApp;
use effectcraft_ui_egui::dock::PanelKind;
use effectcraft_ui_egui::panels::merlin::{AttachState, ChatItem, Decision, ToolState};
use egui_kittest::Harness;

fn click(h: &mut Harness<'_, EffectcraftApp>, id: &str) {
    let e = h.state().auto.find(id).unwrap_or_else(|| panic!("no {id}")).clone();
    let c = egui::pos2(e.rect[0] + e.rect[2] / 2.0, e.rect[1] + e.rect[3] / 2.0);
    h.input_mut().events.push(egui::Event::PointerMoved(c));
    for pressed in [true, false] {
        h.input_mut().events.push(egui::Event::PointerButton { pos: c, button: egui::PointerButton::Primary, pressed, modifiers: Default::default() });
    }
    h.run_steps(2);
}

/// Step until the running turn ends (the worker thread streams with small pauses).
fn wait_idle(h: &mut Harness<'_, EffectcraftApp>) {
    for _ in 0..2000 {
        h.run_steps(1);
        if !h.state().ui.merlin.busy {
            h.run_steps(2);
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    panic!("the turn didn't finish: {:#?}", h.state().ui.merlin.items);
}

fn harness() -> Harness<'static, EffectcraftApp> {
    let s = effectcraft_host::session();
    let mut h = Harness::builder().with_size(egui::vec2(1400.0, 900.0)).build_eframe(|_| EffectcraftApp::new(s));
    h.state_mut().show_panel(PanelKind::Merlin);
    h.run_steps(3);
    h
}

fn has_comp(h: &Harness<'_, EffectcraftApp>, name: &str) -> bool {
    h.state().session.project.items.values().any(|i| i.name == name)
}

#[test]
fn merlin_runs_a_demo_turn_and_reverts_it() {
    let mut h = harness();
    for id in ["merlin.model", "merlin.settings", "merlin.newChat", "merlin.log", "merlin.input", "merlin.send"] {
        assert!(h.state().auto.find(id).is_some(), "missing {id}");
    }
    h.state_mut().ui.merlin.settings.provider = "demo".into();
    let undo_before = h.state().session.history.undo.len();
    h.state_mut().ui.merlin.input = "Hello Merlin".into();
    h.run_steps(1);
    click(&mut h, "merlin.send");
    assert!(h.state().ui.merlin.busy, "the turn started");
    assert!(h.state().ui.merlin.input.is_empty());
    wait_idle(&mut h);

    // The demo built its comp through the UI thread, live, and looked at three frames.
    assert!(has_comp(&h, "Merlin Demo"));
    let items = h.state().ui.merlin.items.clone();
    assert!(matches!(&items[0], ChatItem::User { text, .. } if text == "Hello Merlin"));
    let tools: Vec<(&str, ToolState, usize)> =
        items.iter().filter_map(|i| if let ChatItem::Tool { name, state, frames, .. } = i { Some((name.as_str(), *state, *frames)) } else { None }).collect();
    assert_eq!(tools, vec![("run_script", ToolState::Done, 0), ("render_frames", ToolState::Done, 3)], "{items:#?}");
    assert!(items.iter().any(|i| matches!(i, ChatItem::Assistant { text } if text.contains("Hello Merlin"))));
    let Some(ChatItem::Turn { id, decision, label, tool_calls, .. }) = items.last().cloned() else { panic!("no turn card: {items:#?}") };
    assert_eq!(decision, Decision::Pending);
    assert_eq!(tool_calls, 2);
    assert_eq!(label, "Merlin: Hello Merlin");

    // The whole turn is one undo step.
    let undo = &h.state().session.history.undo;
    assert_eq!(undo.len(), undo_before + 1);
    assert_eq!(undo.last().unwrap().0, "Merlin: Hello Merlin");

    // Revert undoes the turn in one go.
    click(&mut h, &format!("merlin.turn.{id}.revert"));
    assert!(!has_comp(&h, "Merlin Demo"));
    assert!(matches!(h.state().ui.merlin.items.last(), Some(ChatItem::Turn { decision: Decision::Reverted, .. })));

    // A second turn, kept.
    h.state_mut().ui.merlin.input = "Again".into();
    h.run_steps(1);
    click(&mut h, "merlin.send");
    wait_idle(&mut h);
    let Some(ChatItem::Turn { id, .. }) = h.state().ui.merlin.items.last().cloned() else { panic!("no turn card") };
    click(&mut h, &format!("merlin.turn.{id}.keep"));
    assert!(matches!(h.state().ui.merlin.items.last(), Some(ChatItem::Turn { decision: Decision::Kept, .. })));
    assert!(has_comp(&h, "Merlin Demo"));

    // Tool rows expand to show the script.
    let tool = h.state().ui.merlin.items.iter().rposition(|i| matches!(i, ChatItem::Tool { name, .. } if name == "run_script")).unwrap();
    click(&mut h, &format!("merlin.item.{tool}.toggle"));
    assert!(matches!(&h.state().ui.merlin.items[tool], ChatItem::Tool { open: true, input, .. } if input.contains("addText")));

    click(&mut h, "merlin.newChat");
    assert!(h.state().ui.merlin.items.is_empty());
}

#[test]
fn merlin_stop_ends_the_turn_and_keeps_the_panel_usable() {
    let mut h = harness();
    h.state_mut().ui.merlin.settings.provider = "demo".into();
    h.state_mut().ui.merlin.input = "Stop me".into();
    h.run_steps(1);
    click(&mut h, "merlin.send");
    click(&mut h, "merlin.send"); // now the Stop button
    wait_idle(&mut h);
    let items = &h.state().ui.merlin.items;
    assert!(matches!(items.last(), Some(ChatItem::Turn { outcome, .. }) if outcome == "Stopped"), "{items:#?}");
    // The conversation stays valid: another turn runs to the end.
    h.state_mut().ui.merlin.input = "Go".into();
    h.run_steps(1);
    click(&mut h, "merlin.send");
    wait_idle(&mut h);
    assert!(matches!(h.state().ui.merlin.items.last(), Some(ChatItem::Turn { outcome, .. }) if outcome == "Done"));
}

#[test]
fn merlin_without_a_key_explains_what_to_do() {
    let mut h = harness();
    // Gemini, with no key in the environment or keychain on test machines; the endpoint is a closed
    // local port, so even a stray key couldn't reach a real service.
    h.state_mut().ui.merlin.settings.provider = "gemini".into();
    h.state_mut().ui.merlin.settings.base_url = "http://127.0.0.1:9".into();
    h.state_mut().ui.merlin.input = "hi".into();
    h.run_steps(1);
    click(&mut h, "merlin.send");
    wait_idle(&mut h);
    let items = &h.state().ui.merlin.items;
    assert!(
        items.iter().any(|i| matches!(i, ChatItem::Notice { error: true, text } if text.contains("GEMINI_API_KEY") || text.contains("network"))),
        "{items:#?}"
    );
    assert!(matches!(items.last(), Some(ChatItem::Turn { decision: Decision::NoChanges, .. })));
}

#[test]
fn merlin_settings_switch_providers() {
    let mut h = harness();
    click(&mut h, "merlin.settings");
    assert!(h.state().ui.merlin.settings_open);
    for id in [
        "merlin.settings.provider",
        "merlin.settings.model",
        "merlin.settings.baseUrl",
        "merlin.settings.effort",
        "merlin.settings.key",
        "merlin.settings.saveKey",
    ] {
        assert!(h.state().auto.find(id).is_some(), "missing {id}");
    }
    h.state_mut().ui.merlin.settings.provider = "demo".into();
    h.run_steps(2);
    assert!(h.state().auto.find("merlin.settings.key").is_none(), "Demo needs no key");
    click(&mut h, "merlin.settings");
    assert!(!h.state().ui.merlin.settings_open);
}

fn temp_dir(name: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("merlin-ui-{}-{name}", std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// Step until no attachment is still loading (they decode on another thread).
fn wait_attached(h: &mut Harness<'_, EffectcraftApp>) {
    for _ in 0..2000 {
        h.run_steps(1);
        if !h.state().ui.merlin.attachments.iter().any(|a| a.state == AttachState::Loading) {
            h.run_steps(1);
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    panic!("attachments didn't load: {:#?}", h.state().ui.merlin.attachments);
}

#[test]
fn merlin_sends_attached_images_with_the_message() {
    let mut h = harness();
    h.state_mut().ui.merlin.settings.provider = "demo".into();
    assert!(h.state().auto.find("merlin.attach").is_some());
    let d = temp_dir("send");
    let png = d.join("moodboard.png");
    image::RgbaImage::from_pixel(64, 36, image::Rgba([20, 40, 200, 255])).save(&png).unwrap();
    let svg = d.join("logo.svg");
    std::fs::write(&svg, r##"<svg xmlns="http://www.w3.org/2000/svg" width="40" height="40"><circle cx="20" cy="20" r="18" fill="#fff"/></svg>"##).unwrap();
    let junk = d.join("broken.png");
    std::fs::write(&junk, b"not a picture").unwrap();
    let ctx = h.ctx.clone();
    let paths: Vec<String> = [&png, &svg, &junk].iter().map(|p| p.to_string_lossy().into_owned()).collect();
    effectcraft_ui_egui::panels::merlin::attach_paths(h.state_mut(), &ctx, paths.clone());
    // The same file twice is attached once.
    effectcraft_ui_egui::panels::merlin::attach_paths(h.state_mut(), &ctx, vec![paths[0].clone()]);
    wait_attached(&mut h);
    let chips = h.state().ui.merlin.attachments.clone();
    assert_eq!(chips.len(), 3, "{chips:#?}");
    assert_eq!((chips[0].state, chips[1].state, chips[2].state), (AttachState::Ready, AttachState::Ready, AttachState::Failed), "{chips:#?}");
    assert!(chips[0].info.contains("64×36"), "{chips:#?}");
    assert!(!chips[2].info.is_empty());
    for id in ["merlin.attach.0", "merlin.attach.1", "merlin.attach.2.remove"] {
        assert!(h.state().auto.find(id).is_some(), "missing {id}");
    }
    // The broken file can be removed; pictures alone are a message.
    click(&mut h, "merlin.attach.2.remove");
    assert_eq!(h.state().ui.merlin.attachments.len(), 2);
    h.run_steps(2); // the footer shrinks to the remaining row of chips
    click(&mut h, "merlin.send");
    assert!(h.state().ui.merlin.attachments.is_empty(), "sent with the message");
    wait_idle(&mut h);
    let items = h.state().ui.merlin.items.clone();
    let Some(ChatItem::User { text, attachments, .. }) = items.first() else { panic!("{items:#?}") };
    assert!(text.is_empty());
    assert_eq!(attachments, &paths[..2]);
    assert!(
        items.iter().any(|i| matches!(i, ChatItem::Assistant { text } if text.contains("I can see 2 attached image(s): moodboard.png, logo.svg"))),
        "the model got both pictures: {items:#?}"
    );
    assert!(matches!(items.last(), Some(ChatItem::Turn { label, .. }) if label == "Merlin: moodboard.png, logo.svg"), "{items:#?}");
}

#[test]
fn files_dropped_on_merlin_attach_and_elsewhere_import() {
    let mut h = harness();
    let d = temp_dir("drop");
    let png = d.join("ref.png");
    image::RgbaImage::from_pixel(32, 32, image::Rgba([255, 0, 0, 255])).save(&png).unwrap();
    let log = h.state().auto.find("merlin.log").unwrap().clone();
    let over = egui::pos2(log.rect[0] + log.rect[2] / 2.0, log.rect[1] + log.rect[3] / 2.0);
    h.input_mut().events.push(egui::Event::PointerMoved(over));
    h.run_steps(1);

    // Hovering shows Merlin's own hint, not the import one.
    h.input_mut().hovered_files.push(egui::HoveredFile { path: Some(png.clone()), mime: String::new() });
    h.run_steps(2);
    assert!(h.state().auto.find("merlin.dropTarget").is_some());
    assert!(h.state().auto.find("file.dropTarget").is_none());
    h.input_mut().hovered_files.clear();

    let items_before = h.state().session.project.items.len();
    h.input_mut().dropped_files.push(std::sync::Arc::new(Dropped(png.clone())));
    h.run_steps(2);
    wait_attached(&mut h);
    let chips = &h.state().ui.merlin.attachments;
    assert_eq!(chips.len(), 1);
    assert_eq!(chips[0].state, AttachState::Ready);
    assert_eq!(h.state().session.project.items.len(), items_before, "not imported");

    // Over another panel the drop imports as before.
    let timeline = h.state().auto.find("panel.Timeline").unwrap().clone();
    let elsewhere = egui::pos2(timeline.rect[0] + timeline.rect[2] / 2.0, timeline.rect[1] + timeline.rect[3] / 2.0);
    h.input_mut().events.push(egui::Event::PointerMoved(elsewhere));
    h.run_steps(1);
    h.input_mut().dropped_files.push(std::sync::Arc::new(Dropped(png.clone())));
    h.run_steps(2);
    for _ in 0..1000 {
        if h.state().session.project.items.len() > items_before {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
        h.run_steps(1);
    }
    assert!(h.state().session.project.items.values().any(|i| i.name == "ref.png"), "imported");
    assert_eq!(h.state().ui.merlin.attachments.len(), 1, "and not attached again");
}

/// Agents can't drag files: `ui.set {"merlin": {"attach": [...]}}` attaches them instead.
#[test]
fn the_control_channel_attaches_files() {
    let mut h = harness();
    let d = temp_dir("control");
    let png = d.join("still.png");
    image::RgbaImage::from_pixel(8, 8, image::Rgba([9, 9, 9, 255])).save(&png).unwrap();
    let ctx = h.ctx.clone();
    let set = effectcraft_ui_egui::panels::merlin::apply_ui_set;
    assert!(set(h.state_mut(), &ctx, &serde_json::json!({"attach": ["relative.png"]})).unwrap_err().contains("absolute"));
    assert!(set(h.state_mut(), &ctx, &serde_json::json!({"attach": "x"})).is_err());
    set(h.state_mut(), &ctx, &serde_json::json!({"attach": [png.to_string_lossy()]})).unwrap();
    wait_attached(&mut h);
    assert_eq!(h.state().ui.merlin.attachments.len(), 1);
    assert_eq!(h.state().ui.merlin.attachments[0].state, AttachState::Ready);
}

/// A floating window showing Merlin keeps the files dropped on it; other windows' go to import.
#[test]
fn files_dropped_on_a_floating_merlin_window_attach() {
    let mut h = harness();
    let d = temp_dir("window");
    let png = d.join("frame.png");
    image::RgbaImage::from_pixel(16, 16, image::Rgba([0, 255, 0, 255])).save(&png).unwrap();
    let window = |panel: PanelKind| craft_ui::docking::WindowFiles {
        window: 1,
        viewport: egui::ViewportId::ROOT,
        panel,
        pos: None,
        hovered: vec![],
        dropped: vec![std::sync::Arc::new(Dropped(png.clone())) as egui::DroppedFileHandle],
    };
    let ctx = h.ctx.clone();
    let rest = effectcraft_ui_egui::panels::merlin::take_window_files(h.state_mut(), &ctx, vec![window(PanelKind::Merlin), window(PanelKind::Timeline)]);
    assert_eq!(rest.len(), 1);
    assert_eq!(rest[0].panel, PanelKind::Timeline);
    wait_attached(&mut h);
    assert_eq!(h.state().ui.merlin.attachments.len(), 1);
    assert_eq!(h.state().ui.merlin.attachments[0].state, AttachState::Ready);
}

/// A file dropped on the window (egui hands it over as a dropped file).
#[derive(Debug)]
struct Dropped(std::path::PathBuf);

impl egui::DroppedFile for Dropped {
    fn path(&self) -> &std::path::Path {
        &self.0
    }
    fn bytes(&self) -> Result<Vec<u8>, String> {
        Err("not read".into())
    }
}

/// Headless look at the panel after a demo turn (wgpu offscreen); run with
/// `MERLIN_SNAPSHOT=/abs/out.png cargo test -p effectcraft-ui-egui --test it ui_merlin::merlin_snapshot -- --ignored`.
#[test]
#[ignore]
fn merlin_snapshot() {
    let mut h = harness();
    let out = std::env::var("MERLIN_SNAPSHOT").unwrap_or_else(|_| concat!(env!("CARGO_MANIFEST_DIR"), "/../../target/test-out/merlin.png").into());
    if let Some(d) = std::path::Path::new(&out).parent() {
        std::fs::create_dir_all(d).unwrap();
    }
    // The empty panel first.
    h.run_steps(4);
    h.render().expect("render").save(out.replace(".png", "-empty.png")).unwrap();
    h.state_mut().ui.merlin.settings.provider = "demo".into();
    let d = temp_dir("snapshot");
    let board = d.join("moodboard.png");
    let mut img = image::RgbaImage::new(160, 90);
    for (x, _, p) in img.enumerate_pixels_mut() {
        *p = image::Rgba([(x * 255 / 160) as u8, 40, 160, 255]);
    }
    img.save(&board).unwrap();
    let logo = d.join("studio-logo.svg");
    std::fs::write(&logo, r##"<svg xmlns="http://www.w3.org/2000/svg" width="60" height="60"><circle cx="30" cy="30" r="26" fill="#f5c542"/></svg>"##).unwrap();
    let ctx = h.ctx.clone();
    effectcraft_ui_egui::panels::merlin::attach_paths(h.state_mut(), &ctx, vec![board.to_string_lossy().into_owned()]);
    wait_attached(&mut h);
    h.state_mut().ui.merlin.input = "A title that says MERLIN in these colours".into();
    h.run_steps(2);
    click(&mut h, "merlin.send");
    wait_idle(&mut h);
    let tool = h.state().ui.merlin.items.iter().position(|i| matches!(i, ChatItem::Tool { .. })).unwrap();
    click(&mut h, &format!("merlin.item.{tool}.toggle"));
    let junk = d.join("notes.png");
    std::fs::write(&junk, b"nope").unwrap();
    effectcraft_ui_egui::panels::merlin::attach_paths(h.state_mut(), &ctx, vec![logo.to_string_lossy().into_owned(), junk.to_string_lossy().into_owned()]);
    wait_attached(&mut h);
    h.state_mut().ui.merlin.input = "Add the logo bottom right".into();
    h.run_steps(4);
    let img = h.render().expect("render");
    img.save(&out).unwrap();
}

/// Drag from `a` to `b` in a few moves, holding `modifiers` (pressed once the drag is under way,
/// as the other drag tests do).
fn drag(h: &mut Harness<'_, EffectcraftApp>, a: egui::Pos2, b: egui::Pos2, modifiers: egui::Modifiers) {
    h.event(egui::Event::PointerMoved(a));
    h.step();
    h.event(egui::Event::PointerButton { pos: a, button: egui::PointerButton::Primary, pressed: true, modifiers: egui::Modifiers::NONE });
    h.step();
    h.event(egui::Event::ModifiersChanged(modifiers));
    for k in 1..=6 {
        h.event(egui::Event::PointerMoved(a + (b - a) * (k as f32 / 6.0)));
        h.step();
    }
    h.event(egui::Event::PointerButton { pos: b, button: egui::PointerButton::Primary, pressed: false, modifiers });
    h.run_steps(3);
    h.event(egui::Event::ModifiersChanged(egui::Modifiers::NONE));
    h.step();
}

fn rect_of(h: &Harness<'_, EffectcraftApp>, id: &str) -> egui::Rect {
    let e = h.state().auto.find(id).unwrap_or_else(|| panic!("no {id}")).clone();
    egui::Rect::from_min_size(egui::pos2(e.rect[0], e.rect[1]), egui::vec2(e.rect[2], e.rect[3]))
}

fn key(h: &mut Harness<'_, EffectcraftApp>, key: egui::Key, modifiers: egui::Modifiers) {
    h.event(egui::Event::Key { key, physical_key: Some(key), pressed: true, repeat: false, modifiers });
    h.event(egui::Event::Key { key, physical_key: Some(key), pressed: false, repeat: false, modifiers });
    h.run_steps(2);
}

/// `@` with the Timeline focused drops a note at the playhead into the Merlin panel; Shift+→
/// stretches it into a range (not nudging the selected layer), Enter opens it for writing, and it
/// shows on the Timeline's ruler; Send carries it, and the answer shows on its tab in the message.
#[test]
fn at_on_the_timeline_makes_a_note_that_stretches_into_a_range() {
    use effectcraft_ui_egui::dock::PanelKind;
    use merlin_agent::notes::NoteStatus;
    let mut h = harness();
    board(&mut h);
    h.state_mut().session.execute("layer.select", serde_json::json!({"layers": ["#1"]})).unwrap();
    let pos = |h: &Harness<'_, EffectcraftApp>| h.state().session.active_comp().unwrap().layers[0].props.prop("transform/position").unwrap().value.clone();
    let before = pos(&h);
    h.state_mut().session.set_time(effectcraft_engine::time::Tick::from_seconds_f64(1.0));
    h.state_mut().ui.focused = PanelKind::Timeline;
    h.run_steps(2);
    h.event(egui::Event::Text("@".into()));
    h.run_steps(2);
    let notes = h.state().ui.merlin.notes.clone();
    assert_eq!(notes.len(), 1, "a note at the playhead");
    assert_eq!(notes[0].start.seconds(), 1.0);
    assert!(notes[0].end.is_none() && !h.state().ui.merlin.open_notes.contains(&notes[0].id), "collapsed while it is stretched");
    let id = notes[0].id;
    // Shift+→ ×3: three frames later, the layer untouched.
    h.state_mut().ui.focused = PanelKind::Timeline;
    for _ in 0..3 {
        key(&mut h, egui::Key::ArrowRight, egui::Modifiers::SHIFT);
    }
    let n = h.state().ui.merlin.notes[0].clone();
    assert_eq!(n.end.map(|e| e.seconds()), Some(1.1), "30 fps: 3 frames = 0.1 s");
    assert_eq!(pos(&h), before, "Shift+→ stretched the note instead of nudging the layer");
    // Moving the playhead any other way (a drag) keeps stretching it; back before the anchor flips it.
    h.state_mut().session.set_time(effectcraft_engine::time::Tick::from_seconds_f64(0.5));
    h.run_steps(2);
    let n = h.state().ui.merlin.notes[0].clone();
    assert_eq!((n.start.seconds(), n.end.map(|e| e.seconds())), (0.5, Some(1.0)));
    // It shows on the ruler.
    assert!(h.state().auto.find(&format!("merlin.ruler.note.{id}")).is_some());
    // Enter: done stretching, the tab opens for writing.
    key(&mut h, egui::Key::Enter, egui::Modifiers::NONE);
    assert!(h.state().ui.merlin.open_notes.contains(&id));
    h.event(egui::Event::Text("Hold longer here".into()));
    h.run_steps(2);
    assert_eq!(h.state().ui.merlin.notes[0].text, "Hold longer here");
    // Shift+→ nudges layers again once the note is done.
    h.state_mut().ui.focused = PanelKind::Timeline;
    h.state_mut().ui.merlin.input.clear();
    h.ctx.memory_mut(|m| m.surrender_focus(egui::Id::unique(("merlin-note-text", id))));
    h.run_steps(2);
    key(&mut h, egui::Key::ArrowRight, egui::Modifiers::SHIFT);
    assert_ne!(pos(&h), before, "layer nudging is back");
    h.state_mut().session.execute("edit.undo", serde_json::json!({})).unwrap();

    // A second note: Escape on an empty one drops it.
    h.state_mut().ui.focused = PanelKind::Timeline;
    h.run_steps(1);
    h.event(egui::Event::Text("@".into()));
    h.run_steps(2);
    assert_eq!(h.state().ui.merlin.notes.len(), 2);
    key(&mut h, egui::Key::Escape, egui::Modifiers::NONE);
    assert_eq!(h.state().ui.merlin.notes.len(), 1);

    // Tabs: the stack shows the note; it collapses and expands, with a snapshot to mark.
    h.run_steps(4);
    click(&mut h, &format!("merlin.note.{id}"));
    assert!(!h.state().ui.merlin.open_notes.contains(&id));
    click(&mut h, &format!("merlin.note.{id}"));
    assert!(h.state().ui.merlin.open_notes.contains(&id));
    h.run_steps(2);
    assert!(h.state().auto.find(&format!("merlin.note.{id}.mark")).is_some());
    // Collapsed from the ruler, a click on its pin opens it again.
    click(&mut h, "merlin.notes.collapseAll");
    assert!(h.state().ui.merlin.open_notes.is_empty());
    h.run_steps(2);
    click(&mut h, &format!("merlin.ruler.note.{id}"));
    assert!(h.state().ui.merlin.open_notes.contains(&id));

    // Send: the note goes with the message, and its tab in the message shows Merlin's answer.
    h.state_mut().ui.merlin.input = "Please".into();
    h.run_steps(2);
    click(&mut h, "merlin.send");
    wait_idle(&mut h);
    let items = h.state().ui.merlin.items.clone();
    assert!(matches!(&items[0], ChatItem::User { notes, .. } if notes == &vec![id]), "{items:#?}");
    assert!(matches!(h.state().ui.merlin.notes[0].status, NoteStatus::Addressed { .. }));
    assert!(h.state().auto.find("merlin.notes.collapseAll").is_none(), "the stack is empty again");
}

/// `@` while typing goes into the text; Ask Merlin… on a layer starts a note about it.
#[test]
fn typed_at_signs_stay_text_and_ask_merlin_starts_a_note() {
    use effectcraft_ui_egui::dock::PanelKind;
    let mut h = harness();
    board(&mut h);
    click(&mut h, "merlin.input");
    h.state_mut().ui.focused = PanelKind::Timeline;
    h.event(egui::Event::Text("mail@".into()));
    h.run_steps(2);
    assert!(h.state().ui.merlin.notes.is_empty());
    assert_eq!(h.state().ui.merlin.input, "mail@");
    let layer = h.state().session.active_comp().unwrap().layers[1].id.0;
    let ctx = h.ctx.clone();
    effectcraft_ui_egui::menus::invoke(h.state_mut(), &ctx, "merlin.ask", serde_json::json!({"layers": [layer]})).unwrap();
    h.run_steps(2);
    let n = h.state().ui.merlin.notes[0].clone();
    assert_eq!(n.layers, vec![layer]);
    assert!(h.state().ui.merlin.open_notes.contains(&n.id), "open for typing");
    // Its snapshot marks an area: the frame takes the chat's place; drag a box; Done.
    h.run_steps(4);
    click(&mut h, &format!("merlin.note.{}.mark", n.id));
    assert_eq!(h.state().ui.merlin.marking_note, Some(n.id));
    let area = rect_of(&h, "merlin.mark.area");
    drag(&mut h, area.lerp_inside(egui::vec2(0.25, 0.25)), area.lerp_inside(egui::vec2(0.5, 0.75)), egui::Modifiers::NONE);
    let [x, y, w, hh] = h.state().ui.merlin.notes[0].region.expect("an area was marked");
    assert!((x - 160.0).abs() < 8.0 && (y - 90.0).abs() < 8.0 && (w - 160.0).abs() < 8.0 && (hh - 180.0).abs() < 8.0, "{:?}", [x, y, w, hh]);
    click(&mut h, "merlin.mark.done");
    assert!(h.state().ui.merlin.marking_note.is_none());
    // From a keyframe: its time and property.
    let pos_uid = h.state().session.active_comp().unwrap().layers[1].props.prop("transform/position").unwrap().uid;
    effectcraft_ui_egui::menus::invoke(h.state_mut(), &ctx, "merlin.ask", serde_json::json!({"layer": layer, "prop": pos_uid, "time": 2.0})).unwrap();
    let n = h.state().ui.merlin.notes[1].clone();
    assert_eq!(n.start.seconds(), 2.0);
    assert_eq!(n.props, vec!["Blue ▸ Position".to_string()]);
}

/// Agents and scripts add notes and apply them through `ui.set {"merlin": …}`.
#[test]
fn the_control_channel_adds_and_applies_notes() {
    let mut h = harness();
    h.state_mut().ui.merlin.settings.provider = "demo".into();
    let ctx = h.ctx.clone();
    let set = effectcraft_ui_egui::panels::merlin::apply_ui_set;
    assert!(set(h.state_mut(), &ctx, &serde_json::json!({"note": {"time": 1, "text": "x"}})).unwrap_err().contains("no composition"));
    h.state_mut().session.execute("script.run", serde_json::json!({"code": "app.project.items.addComp('C', 320, 180, 1, 3, 25).openInViewer();"})).unwrap();
    assert!(set(h.state_mut(), &ctx, &serde_json::json!({"note": {"text": "x"}})).unwrap_err().contains("time"));
    assert!(set(h.state_mut(), &ctx, &serde_json::json!({"applyNotes": true})).unwrap_err().contains("nothing to send"));
    set(h.state_mut(), &ctx, &serde_json::json!({"note": {"time": 2.0, "end": 0.5, "text": "fade out here", "region": [1, 2, 3, 4], "layers": [99]}})).unwrap();
    let n = h.state().ui.merlin.notes[0].clone();
    assert_eq!((n.start.seconds(), n.end.map(|e| e.seconds())), (0.5, Some(2.0)), "a reversed range is put in order");
    assert_eq!((n.region, n.layers.clone()), (Some([1.0, 2.0, 3.0, 4.0]), vec![99]));
    set(h.state_mut(), &ctx, &serde_json::json!({"applyNotes": true})).unwrap();
    wait_idle(&mut h);
    assert!(matches!(h.state().ui.merlin.notes[0].status, merlin_agent::notes::NoteStatus::Addressed { .. }));
    set(h.state_mut(), &ctx, &serde_json::json!({"removeNote": n.id})).unwrap();
    assert!(h.state().ui.merlin.notes.is_empty());
}

fn board(h: &mut Harness<'_, EffectcraftApp>) {
    h.state_mut().ui.merlin.settings.provider = "demo".into();
    h.state_mut()
        .session
        .execute("script.run", serde_json::json!({"code": "var c = app.project.items.addComp('Board', 640, 360, 1, 4, 30); c.openInViewer(); c.layers.addSolid([0.2,0.4,0.9], 'Blue', 640, 360, 1); c.layers.addSolid([0.9,0.4,0.2], 'Orange', 100, 100, 1);"}))
        .unwrap();
    h.run_steps(2);
}

/// `@` opens a list of what can be pinned; Enter pins the highlighted entry, and the context chips
/// pin the current frame or selected layers in one click.
#[test]
fn at_mentions_and_context_chips_pin_the_message() {
    use effectcraft_ui_egui::panels::merlin::PinKind;
    let mut h = harness();
    board(&mut h);
    h.state_mut().session.set_time(effectcraft_engine::time::Tick::from_seconds_f64(1.0));
    click(&mut h, "merlin.input");
    h.event(egui::Event::Text("make @Ora".into()));
    h.run_steps(2);
    assert!(h.state().auto.find("merlin.mention.0").is_some(), "the list shows");
    assert!(h.state().auto.find("merlin.mention.1").is_none(), "only Orange matches");
    h.event(egui::Event::Key { key: egui::Key::Enter, physical_key: None, pressed: true, repeat: false, modifiers: Default::default() });
    h.run_steps(2);
    let pins = h.state().ui.merlin.pins.clone();
    assert_eq!(pins.len(), 1, "{pins:?}");
    assert!(matches!(pins[0].kind, PinKind::Layer { .. }) && pins[0].label == "Orange" && pins[0].locked);
    assert_eq!(h.state().ui.merlin.input, "make @Orange ", "Enter picked, not sent");
    assert!(!h.state().ui.merlin.busy);

    // A typed range.
    h.event(egui::Event::Text("@1:00-2:00".into()));
    h.run_steps(2);
    h.event(egui::Event::Key { key: egui::Key::Enter, physical_key: None, pressed: true, repeat: false, modifiers: Default::default() });
    h.run_steps(2);
    let range = h.state().ui.merlin.pins.last().cloned().unwrap();
    let PinKind::Range { start, end } = range.kind else { panic!("{range:?}") };
    assert_eq!((start.seconds(), end.seconds()), (1.0, 2.0));
    assert!(range.locked);

    // The time chip pins the current frame; its lock toggles; × removes a pin.
    h.run_steps(2);
    click(&mut h, "merlin.context.time");
    assert!(matches!(h.state().ui.merlin.pins.last().map(|p| &p.kind), Some(PinKind::Frame { at }) if (at.seconds() - 1.0).abs() < 1e-9));
    assert!(h.state().auto.find("merlin.pin.2.lock").is_none(), "a frame isn't lockable");
    h.run_steps(2);
    click(&mut h, "merlin.pin.0.lock");
    assert!(!h.state().ui.merlin.pins[0].locked);
    click(&mut h, "merlin.pin.2.remove");
    assert_eq!(h.state().ui.merlin.pins.len(), 2);
}

/// A locked range keeps Merlin inside it: the demo's script adds layers spanning the whole comp,
/// so the edit is refused, undone, and the model is told why; the pins go with the message.
#[test]
fn locked_pins_refuse_edits_outside_them() {
    let mut h = harness();
    board(&mut h);
    let ctx = h.ctx.clone();
    let set = effectcraft_ui_egui::panels::merlin::apply_ui_set;
    assert!(set(h.state_mut(), &ctx, &serde_json::json!({"pin": {}})).is_err());
    set(h.state_mut(), &ctx, &serde_json::json!({"pin": {"range": [2.0, 1.0]}})).unwrap();
    let before = h.state().session.project.clone();
    let undo = h.state().session.history.undo.len();
    h.state_mut().ui.merlin.input = "a title".into();
    h.run_steps(1);
    click(&mut h, "merlin.send");
    assert!(h.state().ui.merlin.pins.is_empty(), "pins go with the message");
    wait_idle(&mut h);
    let items = h.state().ui.merlin.items.clone();
    assert!(matches!(&items[0], ChatItem::User { pins, .. } if pins.len() == 1 && pins[0].starts_with("🔒")), "{items:#?}");
    let script = items
        .iter()
        .find_map(|i| if let ChatItem::Tool { name, state, summary, .. } = i { (name == "run_script").then_some((*state, summary.clone())) } else { None });
    let (state, summary) = script.expect("the demo ran its script");
    assert_eq!(state, ToolState::Failed);
    assert!(summary.starts_with("Refused") && summary.contains("Demo Title"), "{summary}");
    assert!(std::sync::Arc::ptr_eq(&h.state().session.project, &before), "nothing was applied");
    assert_eq!(h.state().session.history.undo.len(), undo);
    assert!(matches!(items.last(), Some(ChatItem::Turn { decision: Decision::NoChanges, .. })), "{items:#?}");

    // Unlocked, the same request goes through.
    set(h.state_mut(), &ctx, &serde_json::json!({"pin": {"range": [1.0, 2.0], "locked": false}})).unwrap();
    h.state_mut().ui.merlin.input = "a title".into();
    h.run_steps(1);
    click(&mut h, "merlin.send");
    wait_idle(&mut h);
    assert!(matches!(h.state().ui.merlin.items.last(), Some(ChatItem::Turn { decision: Decision::Pending, .. })));
}

/// Plan mode: Merlin only looks and answers with beats (note tabs in the plan card); Build it turns them into
/// notes and builds them in one turn; a plan can be revised or discarded.
#[test]
fn plan_mode_proposes_beats_and_builds_them() {
    use effectcraft_ui_egui::panels::merlin::{Mode, PlanState};
    use merlin_agent::notes::{Author, NoteStatus};
    let mut h = harness();
    board(&mut h);
    click(&mut h, "merlin.mode.plan");
    assert_eq!(h.state().ui.merlin.mode, Mode::Plan);
    let before = h.state().session.project.clone();
    h.state_mut().ui.merlin.input = "Big Launch".into();
    h.run_steps(1);
    click(&mut h, "merlin.send");
    wait_idle(&mut h);
    assert!(std::sync::Arc::ptr_eq(&h.state().session.project, &before), "plan mode builds nothing");
    let items = h.state().ui.merlin.items.clone();
    let plan = items.iter().position(|i| matches!(i, ChatItem::Plan { .. })).expect("a plan card");
    let ChatItem::Plan { beats, state, title } = &items[plan] else { unreachable!() };
    assert_eq!((title.as_str(), *state, beats.len()), ("Big Launch", PlanState::Proposed, 3));
    let notes = h.state().ui.merlin.notes.clone();
    assert!(notes.iter().all(|n| n.author == Author::Merlin && n.status == NoteStatus::Proposed && n.end.is_some()), "{notes:#?}");
    assert!(h.state().auto.find("merlin.notes.collapseAll").is_none(), "beats aren't notes for the next message");

    // Asking again revises: the new plan replaces the old beats.
    h.state_mut().ui.merlin.input = "Make it faster".into();
    h.run_steps(1);
    click(&mut h, "merlin.send");
    wait_idle(&mut h);
    let items = h.state().ui.merlin.items.clone();
    assert!(matches!(&items[plan], ChatItem::Plan { state: PlanState::Revised, .. }));
    let latest = items.iter().rposition(|i| matches!(i, ChatItem::Plan { .. })).unwrap();
    assert_eq!(h.state().ui.merlin.notes.len(), 3, "old beats replaced");

    // Build it: Act mode, one turn that resolves every beat.
    h.run_steps(2);
    click(&mut h, &format!("merlin.plan.{latest}.build"));
    assert_eq!(h.state().ui.merlin.mode, Mode::Act);
    wait_idle(&mut h);
    let notes = h.state().ui.merlin.notes.clone();
    assert!(notes.iter().all(|n| matches!(n.status, NoteStatus::Addressed { .. })), "{notes:#?}");
    assert!(matches!(&h.state().ui.merlin.items[latest], ChatItem::Plan { state: PlanState::Built, .. }));
}

#[test]
fn a_discarded_plan_drops_its_beats() {
    let mut h = harness();
    board(&mut h);
    h.state_mut().ui.merlin.mode = effectcraft_ui_egui::panels::merlin::Mode::Plan;
    h.state_mut().ui.merlin.input = "Intro".into();
    h.run_steps(1);
    click(&mut h, "merlin.send");
    wait_idle(&mut h);
    let plan = h.state().ui.merlin.items.iter().position(|i| matches!(i, ChatItem::Plan { .. })).unwrap();
    h.run_steps(2);
    click(&mut h, &format!("merlin.plan.{plan}.discard"));
    assert!(h.state().ui.merlin.notes.is_empty());
}

/// Writes `target/test-out/merlin-plan.png` (a plan card with its beats):
/// `cargo test -p effectcraft-ui-egui --test it merlin_plan_snapshot -- --ignored`.
#[test]
#[ignore]
fn merlin_plan_snapshot() {
    let mut h = harness();
    board(&mut h);
    let out = concat!(env!("CARGO_MANIFEST_DIR"), "/../../target/test-out/merlin-plan.png");
    std::fs::create_dir_all(std::path::Path::new(out).parent().unwrap()).unwrap();
    click(&mut h, "merlin.mode.plan");
    h.state_mut().ui.merlin.input = "A launch title".into();
    h.run_steps(1);
    click(&mut h, "merlin.send");
    wait_idle(&mut h);
    let ctx = h.ctx.clone();
    effectcraft_ui_egui::panels::merlin::apply_ui_set(h.state_mut(), &ctx, &serde_json::json!({"pin": {"range": [0.6, 1.2]}})).unwrap();
    h.state_mut().ui.merlin.input = "Make beat 2 snappier".into();
    h.run_steps(6);
    h.render().expect("render").save(out).unwrap();
}

/// Restore to here puts the project back as it was before a message (a history jump, so the later
/// work stays reachable).
#[test]
fn restore_to_here_returns_to_before_a_message() {
    let mut h = harness();
    board(&mut h);
    h.state_mut().ui.merlin.input = "First".into();
    h.run_steps(1);
    click(&mut h, "merlin.send");
    wait_idle(&mut h);
    assert!(has_comp(&h, "Board"));
    let layers = |h: &Harness<'_, EffectcraftApp>| h.state().session.active_comp().map(|c| c.layers.len()).unwrap_or(0);
    let after_first = layers(&h);
    h.state_mut().ui.merlin.input = "Second".into();
    h.run_steps(1);
    click(&mut h, "merlin.send");
    wait_idle(&mut h);
    assert!(layers(&h) > after_first);
    let first = h.state().ui.merlin.items.iter().position(|i| matches!(i, ChatItem::User { text, .. } if text == "First")).unwrap();
    // Scroll the conversation back to the top.
    let log = rect_of(&h, "merlin.log").center();
    h.input_mut().events.push(egui::Event::PointerMoved(log));
    for _ in 0..10 {
        h.input_mut().events.push(egui::Event::MouseWheel {
            unit: egui::MouseWheelUnit::Point,
            delta: egui::vec2(0.0, 400.0),
            modifiers: Default::default(),
            phase: egui::TouchPhase::Move,
            source: egui::MouseWheelSource::Wheel,
        });
        h.run_steps(2);
    }
    h.run_steps(10);
    click(&mut h, &format!("merlin.item.{first}.restore"));
    assert_eq!(layers(&h), 2, "back to the board's two layers");
    assert!(matches!(h.state().ui.merlin.items.last(), Some(ChatItem::Notice { error: false, .. })));
    // The later state is still in the history tree.
    assert!(h.state().session.history_tree().len() >= 3);
}

/// The chat, notes and conversation are saved next to the project and come back when it is
/// opened again; another project starts with a fresh chat.
#[test]
fn the_chat_is_saved_with_the_project() {
    let mut h = harness();
    board(&mut h);
    let d = temp_dir("persist");
    let proj = d.join("Board.ecproj");
    let chat = d.join("Board.merlin.json");
    let _ = std::fs::remove_file(&chat);
    h.state_mut().session.execute("file.saveAs", serde_json::json!({"path": proj.to_string_lossy()})).unwrap();
    let ctx = h.ctx.clone();
    effectcraft_ui_egui::panels::merlin::apply_ui_set(h.state_mut(), &ctx, &serde_json::json!({"note": {"time": 1.0, "text": "warmer"}})).unwrap();
    h.state_mut().ui.merlin.input = "Hello".into();
    h.run_steps(1);
    click(&mut h, "merlin.send");
    wait_idle(&mut h);
    assert!(!h.state().ui.merlin.items.is_empty(), "Save As kept the chat");
    for _ in 0..600 {
        h.run_steps(1);
        if chat.exists() && std::fs::read_to_string(&chat).is_ok_and(|t| t.contains("resolve_note")) {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    let text = std::fs::read_to_string(&chat).expect("saved next to the project");
    assert!(text.contains("\"warmer\"") && text.contains("\"conversation\""));
    let items = h.state().ui.merlin.items.len();

    // Another project: a fresh chat. Opening this one again brings it back.
    h.state_mut().session.execute("file.saveAs", serde_json::json!({"path": d.join("Other.ecproj").to_string_lossy()})).unwrap();
    h.state_mut().session.execute("file.open", serde_json::json!({"path": d.join("Other.ecproj").to_string_lossy()})).unwrap();
    h.run_steps(3);
    assert!(h.state().ui.merlin.items.is_empty() && h.state().ui.merlin.notes.is_empty(), "{:#?}", h.state().ui.merlin.items);
    h.state_mut().session.execute("file.open", serde_json::json!({"path": proj.to_string_lossy()})).unwrap();
    h.run_steps(3);
    assert_eq!(h.state().ui.merlin.items.len(), items);
    assert_eq!(h.state().ui.merlin.notes.len(), 1);
    assert!(matches!(h.state().ui.merlin.notes[0].status, merlin_agent::notes::NoteStatus::Addressed { .. }));
    assert!(h.state().ui.merlin.items.iter().all(|i| !matches!(i, ChatItem::Turn { decision: Decision::Pending, .. })));
}

/// Style kits: make one in the view, give it colours; Merlin gets it with each message, adds to it
/// when asked to remember, and the project keeps its choice.
#[test]
fn style_kits_are_made_followed_and_remembered() {
    let mut h = harness();
    board(&mut h);
    click(&mut h, "merlin.style");
    assert!(h.state().ui.merlin.style_open);
    h.run_steps(2);
    click(&mut h, "merlin.style.new");
    assert_eq!(h.state().ui.merlin.active_style, "New kit");
    h.run_steps(2);
    click(&mut h, "merlin.style.addColor");
    h.run_steps(2);
    click(&mut h, "merlin.style.feel.bouncy");
    h.run_steps(2);
    // Agents set whole kits.
    let ctx = h.ctx.clone();
    let set = effectcraft_ui_egui::panels::merlin::apply_ui_set;
    assert!(set(h.state_mut(), &ctx, &serde_json::json!({"style": "Nope"})).is_err());
    set(
        h.state_mut(),
        &ctx,
        &serde_json::json!({"styleKit": {"name": "Ember", "palette": [{"name": "Ember", "hex": "#F26B21"}], "fonts": ["Inter"], "notes": "No pure white"}}),
    )
    .unwrap();
    assert_eq!(h.state().ui.merlin.active_style, "Ember");
    assert_eq!(h.state().ui.merlin.settings.style, "Ember", "the default for other projects too");
    click(&mut h, "merlin.style");
    assert!(!h.state().ui.merlin.style_open, "back to chat");
    h.run_steps(2);
    assert!(h.state().auto.find("merlin.context.style").is_some(), "the kit shows in the composer");

    // Save the project, then ask Merlin to remember: the kit grows and the chat file has the block.
    let d = temp_dir("style");
    let proj = d.join("Styled.ecproj");
    let chat = d.join("Styled.merlin.json");
    let _ = std::fs::remove_file(&chat);
    h.state_mut().session.execute("file.saveAs", serde_json::json!({"path": proj.to_string_lossy()})).unwrap();
    h.state_mut().ui.merlin.input = "remember to keep it warm".into();
    h.run_steps(1);
    click(&mut h, "merlin.send");
    wait_idle(&mut h);
    let ember = h.state().ui.merlin.items.iter().any(|i| matches!(i, ChatItem::Notice { text, .. } if text.contains("Ember") && text.contains("1 colour")));
    assert!(ember, "{:#?}", h.state().ui.merlin.items);
    for _ in 0..600 {
        h.run_steps(1);
        if std::fs::read_to_string(&chat).is_ok_and(|t| t.contains("style_kit") && t.contains("\"style\":\"Ember\"")) {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    let text = std::fs::read_to_string(&chat).unwrap();
    assert!(text.contains("<style_kit name=\\\"Ember\\\">"), "Merlin got the kit");
    assert!(text.contains("\"style\":\"Ember\""), "the project keeps its kit");

    // Another project starts with the last-used kit and can choose none; this one comes back with Ember.
    h.state_mut().session.execute("file.saveAs", serde_json::json!({"path": d.join("Plain.ecproj").to_string_lossy()})).unwrap();
    h.state_mut().session.execute("file.open", serde_json::json!({"path": d.join("Plain.ecproj").to_string_lossy()})).unwrap();
    h.run_steps(3);
    assert_eq!(h.state().ui.merlin.active_style, "Ember");
    set(h.state_mut(), &ctx, &serde_json::json!({"style": ""})).unwrap();
    assert_eq!(h.state().ui.merlin.active_style, "");
    h.state_mut().session.execute("file.open", serde_json::json!({"path": proj.to_string_lossy()})).unwrap();
    h.run_steps(3);
    assert_eq!(h.state().ui.merlin.active_style, "Ember");
}

/// Writes `target/test-out/merlin-style.png` (the style kit view):
/// `cargo test -p effectcraft-ui-egui --test it merlin_style_snapshot -- --ignored`.
#[test]
#[ignore]
fn merlin_style_snapshot() {
    let mut h = harness();
    board(&mut h);
    let ctx = h.ctx.clone();
    effectcraft_ui_egui::panels::merlin::apply_ui_set(
        h.state_mut(),
        &ctx,
        &serde_json::json!({"styleKit": {"name": "Ember", "palette": [{"name": "Ember", "hex": "#F26B21"}, {"name": "Charcoal", "hex": "#1E1E1E"}, {"name": "Cream", "hex": "#F5EBDD"}], "fonts": ["Inter"], "motion": {"feel": "snappy", "overshoot": 0.12}, "notes": "Never pure white; logo bottom right."}}),
    )
    .unwrap();
    click(&mut h, "merlin.style");
    h.run_steps(6);
    let out = concat!(env!("CARGO_MANIFEST_DIR"), "/../../target/test-out/merlin-style.png");
    std::fs::create_dir_all(std::path::Path::new(out).parent().unwrap()).unwrap();
    h.render().expect("render").save(out).unwrap();
}
