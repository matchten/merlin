//! Window ▸ Merlin: the agent panel runs a whole turn offline (the Demo provider), its tool calls go
//! through the UI thread like control requests, the turn becomes one undo step, and Keep / Revert
//! work (egui_kittest, UI logic only).

use effectcraft_ui_egui::EffectcraftApp;
use effectcraft_ui_egui::dock::PanelKind;
use effectcraft_ui_egui::panels::merlin::{ChatItem, Decision, ToolState};
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
    assert!(matches!(&items[0], ChatItem::User { text } if text == "Hello Merlin"));
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

/// Headless look at the panel after a demo turn (wgpu offscreen); run with
/// `MERLIN_SNAPSHOT=/abs/out.png cargo test -p effectcraft-ui-egui --test it ui_merlin::merlin_snapshot -- --ignored`.
#[test]
#[ignore]
fn merlin_snapshot() {
    let mut h = harness();
    h.state_mut().ui.merlin.settings.provider = "demo".into();
    h.state_mut().ui.merlin.input = "A title that says MERLIN".into();
    h.run_steps(1);
    click(&mut h, "merlin.send");
    wait_idle(&mut h);
    let tool = h.state().ui.merlin.items.iter().position(|i| matches!(i, ChatItem::Tool { .. })).unwrap();
    click(&mut h, &format!("merlin.item.{tool}.toggle"));
    h.state_mut().ui.merlin.input = "Make it glow".into();
    h.run_steps(4);
    let img = h.render().expect("render");
    let out = std::env::var("MERLIN_SNAPSHOT").unwrap_or_else(|_| concat!(env!("CARGO_MANIFEST_DIR"), "/../../target/test-out/merlin.png").into());
    if let Some(d) = std::path::Path::new(&out).parent() {
        std::fs::create_dir_all(d).unwrap();
    }
    img.save(&out).unwrap();
}
