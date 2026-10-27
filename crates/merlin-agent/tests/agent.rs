//! End-to-end tests of the agent: provider adapters against recorded streams, and the loop driving
//! a real headless session (scripts, commands, renders, checkpoints).

use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use merlin_agent::agent_loop::{AgentEvent, TurnEnd, run_turn};
use merlin_agent::checkpoint::Checkpoint;
use merlin_agent::conversation::{Image, Message, Native, Part, ResultPart, Role};
use merlin_agent::provider::{ChatRequest, LlmProvider, StopReason};
use merlin_agent::providers::anthropic::Anthropic;
use merlin_agent::providers::openai_compat::OpenAiCompat;
use merlin_agent::transport::Recorded;
use merlin_agent::{AgentConfig, HeadlessHost, Host, ProviderKind, SYSTEM_PROMPT, tools};
use serde_json::{Value, json};

// ---------------------------------------------------------------- stream builders

fn sse(events: &[(&str, Value)]) -> String {
    events.iter().map(|(e, d)| format!("event: {e}\ndata: {d}\n\n")).collect()
}

/// An Anthropic reply: optional thinking, optional text, then tool calls `(id, name, input)`,
/// each tool input streamed in two fragments.
fn anthropic_reply(thinking: Option<&str>, text: Option<&str>, calls: &[(&str, &str, Value)]) -> String {
    let mut ev = vec![(
        "message_start",
        json!({"type": "message_start", "message": {"id": "msg_1", "role": "assistant", "content": [], "usage": {"input_tokens": 120, "cache_read_input_tokens": 3000, "cache_creation_input_tokens": 50, "output_tokens": 1}}}),
    )];
    let mut i = 0;
    if let Some(t) = thinking {
        ev.push((
            "content_block_start",
            json!({"type": "content_block_start", "index": i, "content_block": {"type": "thinking", "thinking": "", "signature": ""}}),
        ));
        ev.push(("content_block_delta", json!({"type": "content_block_delta", "index": i, "delta": {"type": "thinking_delta", "thinking": t}})));
        ev.push(("content_block_delta", json!({"type": "content_block_delta", "index": i, "delta": {"type": "signature_delta", "signature": "sig123"}})));
        ev.push(("content_block_stop", json!({"type": "content_block_stop", "index": i})));
        i += 1;
    }
    if let Some(t) = text {
        let (a, b) = t.split_at(t.len() / 2);
        ev.push(("content_block_start", json!({"type": "content_block_start", "index": i, "content_block": {"type": "text", "text": ""}})));
        ev.push(("content_block_delta", json!({"type": "content_block_delta", "index": i, "delta": {"type": "text_delta", "text": a}})));
        ev.push(("content_block_delta", json!({"type": "content_block_delta", "index": i, "delta": {"type": "text_delta", "text": b}})));
        ev.push(("content_block_stop", json!({"type": "content_block_stop", "index": i})));
        i += 1;
    }
    for (id, name, input) in calls {
        let raw = input.to_string();
        let (a, b) = raw.split_at(raw.len() / 2);
        ev.push((
            "content_block_start",
            json!({"type": "content_block_start", "index": i, "content_block": {"type": "tool_use", "id": id, "name": name, "input": {}}}),
        ));
        ev.push(("content_block_delta", json!({"type": "content_block_delta", "index": i, "delta": {"type": "input_json_delta", "partial_json": a}})));
        ev.push(("ping", json!({"type": "ping"})));
        ev.push(("content_block_delta", json!({"type": "content_block_delta", "index": i, "delta": {"type": "input_json_delta", "partial_json": b}})));
        ev.push(("content_block_stop", json!({"type": "content_block_stop", "index": i})));
        i += 1;
    }
    let stop = if calls.is_empty() { "end_turn" } else { "tool_use" };
    ev.push(("message_delta", json!({"type": "message_delta", "delta": {"stop_reason": stop}, "usage": {"output_tokens": 77}})));
    ev.push(("message_stop", json!({"type": "message_stop"})));
    sse(&ev)
}

fn anthropic(replies: Vec<String>) -> (Anthropic, Arc<Recorded>) {
    let t = Arc::new(Recorded { replies, ..Default::default() });
    let cfg = AgentConfig::default();
    (Anthropic::new(&cfg, "sk-test".into(), t.clone()), t)
}

fn sent(t: &Recorded) -> Vec<Value> {
    t.sent.lock().unwrap().iter().map(|(_, b)| b.clone()).collect()
}

fn host() -> HeadlessHost {
    HeadlessHost::new(effectcraft_host::session())
}

const BUILD: &str = r#"var c = app.project.items.addComp("Main", 640, 360, 1, 2, 30); c.openInViewer();
var t = c.layers.addText("Hello"); t.name = "Title";
t.property("ADBE Transform Group").property("ADBE Position").setValueAtTime(0, [320, 400]);
t.property("ADBE Transform Group").property("ADBE Position").setValueAtTime(1, [320, 180]);
c.numLayers;"#;

// ---------------------------------------------------------------- Anthropic adapter

#[test]
fn anthropic_stream_becomes_a_reply_with_native_replay() {
    let (p, _) =
        anthropic(vec![anthropic_reply(Some("Plan: add text"), Some("Adding a title."), &[("toolu_1", "run_script", json!({"code": "1 + \"quoted\""}))])]);
    let mut events = vec![];
    let req = ChatRequest { system: "sys", tools: &[], messages: &[Message::user_text("hi")] };
    let r = p.complete(&req, &mut |e| events.push(e), &AtomicBool::new(false)).unwrap();
    assert_eq!(r.stop, StopReason::ToolUse);
    assert_eq!(r.usage.input_tokens, 120);
    assert_eq!(r.usage.cache_read_tokens, 3000);
    assert_eq!(r.usage.output_tokens, 77);
    assert_eq!(r.message.text(), "Adding a title.");
    let calls: Vec<_> = r.message.tool_calls().collect();
    assert_eq!(calls, vec![("toolu_1", "run_script", &json!({"code": "1 + \"quoted\""}))]);
    assert!(r.message.parts.contains(&Part::Thinking { text: "Plan: add text".into() }));
    // The native content keeps the thinking block exactly, signature included.
    let native = r.message.native.as_ref().unwrap();
    assert_eq!(native.provider, "anthropic");
    assert_eq!(native.content[0], json!({"type": "thinking", "thinking": "Plan: add text", "signature": "sig123"}));
    assert_eq!(native.content[2]["input"], json!({"code": "1 + \"quoted\""}));
    assert!(events.len() >= 4);
}

#[test]
fn anthropic_request_body() {
    let (p, _) = anthropic(vec![]);
    let png = Image::png(&[1, 2, 3]);
    let foreign = Message {
        role: Role::Assistant,
        parts: vec![Part::Text { text: "from gpt".into() }],
        native: Some(Native { provider: "openai".into(), content: json!({"bogus": true}) }),
    };
    let own = Message {
        role: Role::Assistant,
        parts: vec![],
        native: Some(Native { provider: "anthropic".into(), content: json!([{"type": "text", "text": "verbatim"}]) }),
    };
    let messages = vec![
        Message::user(vec![Part::Text { text: "make it".into() }, Part::Image { image: png.clone(), label: Some("logo.png".into()) }]),
        own,
        Message::user(vec![Part::ToolResult {
            call_id: "toolu_1".into(),
            content: vec![ResultPart::Text { text: "ok".into() }, ResultPart::Image { image: png }],
            is_error: false,
        }]),
        foreign,
    ];
    let specs = tools::specs();
    let b = p.body(&ChatRequest { system: SYSTEM_PROMPT, tools: &specs, messages: &messages });
    assert_eq!(b["model"], "claude-opus-5-5");
    assert_eq!(b["stream"], true);
    assert_eq!(b["thinking"], json!({"type": "adaptive", "display": "summarized"}));
    assert_eq!(b["output_config"]["effort"], "high");
    assert_eq!(b["fallbacks"], "default");
    assert_eq!(b["cache_control"]["type"], "ephemeral");
    assert_eq!(b["tools"][0]["eager_input_streaming"], true);
    assert!(b.get("temperature").is_none());
    let m = &b["messages"];
    assert_eq!(m[0]["content"][1]["text"], "Attached image: logo.png");
    assert_eq!(m[0]["content"][2]["source"]["type"], "base64");
    assert_eq!(m[1]["content"], json!([{"type": "text", "text": "verbatim"}]));
    assert_eq!(m[2]["content"][0]["type"], "tool_result");
    assert_eq!(m[2]["content"][0]["content"][1]["type"], "image");
    // Another provider's native encoding is ignored: rebuilt from the normalized parts.
    assert_eq!(m[3]["content"], json!([{"type": "text", "text": "from gpt"}]));
}

#[test]
fn anthropic_mid_output_fallback_drops_blocks_before_the_marker() {
    let ev = vec![
        ("message_start", json!({"type": "message_start", "message": {"usage": {"input_tokens": 10}}})),
        ("content_block_start", json!({"type": "content_block_start", "index": 0, "content_block": {"type": "thinking", "thinking": "x", "signature": "s"}})),
        ("content_block_stop", json!({"type": "content_block_stop", "index": 0})),
        ("content_block_start", json!({"type": "content_block_start", "index": 1, "content_block": {"type": "text", "text": "partial "}})),
        ("content_block_stop", json!({"type": "content_block_stop", "index": 1})),
        (
            "content_block_start",
            json!({"type": "content_block_start", "index": 2, "content_block": {"type": "tool_use", "id": "t0", "name": "get_project", "input": {}}}),
        ),
        ("content_block_stop", json!({"type": "content_block_stop", "index": 2})),
        (
            "content_block_start",
            json!({"type": "content_block_start", "index": 3, "content_block": {"type": "fallback", "from": {"model": "a"}, "to": {"model": "b"}}}),
        ),
        ("content_block_stop", json!({"type": "content_block_stop", "index": 3})),
        ("content_block_start", json!({"type": "content_block_start", "index": 4, "content_block": {"type": "text", "text": "continued"}})),
        ("content_block_stop", json!({"type": "content_block_stop", "index": 4})),
        ("message_delta", json!({"type": "message_delta", "delta": {"stop_reason": "end_turn"}, "usage": {"output_tokens": 5}})),
        ("message_stop", json!({"type": "message_stop"})),
    ];
    let (p, _) = anthropic(vec![sse(&ev)]);
    let r = p.complete(&ChatRequest { system: "", tools: &[], messages: &[] }, &mut |_| {}, &AtomicBool::new(false)).unwrap();
    let types: Vec<&str> = r.message.native.as_ref().unwrap().content.as_array().unwrap().iter().map(|b| b["type"].as_str().unwrap()).collect();
    assert_eq!(types, vec!["text", "text"]);
    assert_eq!(r.message.tool_calls().count(), 0);
}

#[test]
fn anthropic_errors_and_missing_key() {
    let (p, _) = anthropic(vec![sse(&[("error", json!({"type": "error", "error": {"type": "invalid_request_error", "message": "bad thing"}}))])]);
    let e = p.complete(&ChatRequest { system: "", tools: &[], messages: &[] }, &mut |_| {}, &AtomicBool::new(false)).unwrap_err();
    assert!(e.to_string().contains("bad thing"), "{e}");
    let t = Arc::new(Recorded::default());
    let p = Anthropic::new(&AgentConfig::default(), " ".into(), t);
    let e = p.complete(&ChatRequest { system: "", tools: &[], messages: &[] }, &mut |_| {}, &AtomicBool::new(false)).unwrap_err();
    assert!(e.to_string().contains("ANTHROPIC_API_KEY"), "{e}");
    // A stream cut off before message_stop is an error, not a silently partial reply.
    let (p, _) = anthropic(vec![sse(&[("message_start", json!({"type": "message_start", "message": {"usage": {}}}))])]);
    assert!(p.complete(&ChatRequest { system: "", tools: &[], messages: &[] }, &mut |_| {}, &AtomicBool::new(false)).is_err());
}

// ---------------------------------------------------------------- OpenAI-compatible adapter

fn openai_chunks(chunks: &[Value]) -> String {
    let mut s: String = chunks.iter().map(|c| format!("data: {c}\n\n")).collect();
    s.push_str("data: [DONE]\n\n");
    s
}

#[test]
fn openai_stream_with_fragmented_tool_calls_and_extensions() {
    let stream = openai_chunks(&[
        json!({"choices": [{"index": 0, "delta": {"role": "assistant", "content": "Let me "}}]}),
        json!({"choices": [{"index": 0, "delta": {"content": "check.", "reasoning_content": "thinking…"}}]}),
        json!({"choices": [{"index": 0, "delta": {"tool_calls": [{"index": 0, "id": "call_a", "type": "function", "function": {"name": "get_comp", "arguments": "{\"co"}, "extra_content": {"google": {"thought_signature": "TS"}}}]}}]}),
        json!({"choices": [{"index": 0, "delta": {"tool_calls": [{"index": 0, "function": {"arguments": "mp\": 1}"}}]}}]}),
        json!({"choices": [{"index": 0, "delta": {"tool_calls": [{"index": 1, "id": "call_b", "function": {"name": "get_project", "arguments": ""}}]}}]}),
        json!({"choices": [{"index": 0, "delta": {}, "finish_reason": "tool_calls"}]}),
        json!({"choices": [], "usage": {"prompt_tokens": 500, "completion_tokens": 40, "prompt_tokens_details": {"cached_tokens": 300}}}),
    ]);
    let t = Arc::new(Recorded { replies: vec![stream], ..Default::default() });
    let cfg = AgentConfig { provider: ProviderKind::Gemini, ..Default::default() };
    let p = OpenAiCompat::new(ProviderKind::Gemini, &cfg, "k".into(), t.clone());
    let r = p.complete(&ChatRequest { system: "s", tools: &[], messages: &[Message::user_text("hi")] }, &mut |_| {}, &AtomicBool::new(false)).unwrap();
    assert_eq!(r.stop, StopReason::ToolUse);
    assert_eq!(r.message.text(), "Let me check.");
    let calls: Vec<_> = r.message.tool_calls().map(|(i, n, v)| (i.to_string(), n.to_string(), v.clone())).collect();
    assert_eq!(calls, vec![("call_a".into(), "get_comp".into(), json!({"comp": 1})), ("call_b".into(), "get_project".into(), json!({}))]);
    assert_eq!(r.usage.input_tokens, 200);
    assert_eq!(r.usage.cache_read_tokens, 300);
    // The thought signature travels back with the call to the same provider.
    let native = r.message.native.clone().unwrap();
    assert_eq!(native.provider, "gemini");
    assert_eq!(native.content["tool_calls"][0]["extra_content"]["google"]["thought_signature"], "TS");
    assert_eq!(sent(&t)[0]["model"], "gemini-2.5-pro");
    assert!(t.sent.lock().unwrap()[0].0.ends_with("/v1beta/openai/chat/completions"));

    let results = Message::user(vec![
        Part::ToolResult {
            call_id: "call_a".into(),
            content: vec![ResultPart::Text { text: "{}".into() }, ResultPart::Image { image: Image::png(&[9]) }],
            is_error: false,
        },
        Part::ToolResult { call_id: "call_b".into(), content: vec![ResultPart::Text { text: "boom".into() }], is_error: true },
    ]);
    let b = p.body(&ChatRequest { system: "s", tools: &tools::specs(), messages: &[Message::user_text("hi"), r.message.clone(), results] });
    let m = b["messages"].as_array().unwrap();
    assert_eq!(m[0]["role"], "system");
    assert_eq!(m[2]["tool_calls"][0]["extra_content"]["google"]["thought_signature"], "TS");
    assert_eq!(m[3]["role"], "tool");
    assert_eq!(m[3]["tool_call_id"], "call_a");
    assert_eq!(m[4]["content"], "Error: boom");
    // Tool images follow the tool messages as a user message.
    assert_eq!(m[5]["role"], "user");
    assert!(m[5]["content"][1]["image_url"]["url"].as_str().unwrap().starts_with("data:image/png;base64,"));
    assert_eq!(b["tools"][0]["type"], "function");

    // The same reply sent to OpenAI (another provider) is rebuilt without Gemini's extension.
    let other = OpenAiCompat::new(ProviderKind::OpenAi, &AgentConfig { provider: ProviderKind::OpenAi, ..Default::default() }, "k".into(), t);
    let b = other.body(&ChatRequest { system: "s", tools: &[], messages: &[r.message] });
    assert!(b["messages"][1]["tool_calls"][0].get("extra_content").is_none());
    assert_eq!(b["messages"][1]["tool_calls"][0]["function"]["arguments"], "{\"comp\":1}");
}

#[test]
fn openai_local_server_needs_no_key_and_bad_arguments_are_flagged() {
    let stream = openai_chunks(&[
        json!({"choices": [{"index": 0, "delta": {"tool_calls": [{"index": 0, "function": {"name": "run_script", "arguments": "{\"code\": \"unterminated"}}]}, "finish_reason": "stop"}]}),
    ]);
    let t = Arc::new(Recorded { replies: vec![stream], ..Default::default() });
    let cfg = AgentConfig { provider: ProviderKind::Custom, model: "llama".into(), ..Default::default() };
    let p = OpenAiCompat::new(ProviderKind::Custom, &cfg, String::new(), t);
    let r = p.complete(&ChatRequest { system: "s", tools: &[], messages: &[] }, &mut |_| {}, &AtomicBool::new(false)).unwrap();
    let (id, _, input) = r.message.tool_calls().next().unwrap();
    assert_eq!(id, "call_0");
    assert!(input.get("_invalidInput").is_some());
    let mut h = host();
    let out = tools::run(&mut h, "run_script", input, 256);
    assert!(out.is_error);
    assert!(out.text().contains("not valid JSON"));
}

// ---------------------------------------------------------------- the loop on a real session

#[test]
fn loop_builds_animation_renders_and_collapses_into_one_undo_step() {
    let (p, t) = anthropic(vec![
        anthropic_reply(None, Some("Building."), &[("toolu_1", "run_script", json!({"code": BUILD}))]),
        anthropic_reply(None, None, &[("toolu_2", "render_frames", json!({"times": [0, 1.0]})), ("toolu_3", "get_comp", json!({}))]),
        anthropic_reply(Some("Looks right."), Some("Added a sliding title."), &[]),
    ]);
    let mut h = host();
    let cp = Checkpoint::take(&h.session);
    let undo_before = h.session.history.undo.len();
    let mut conv = vec![];
    let mut events = vec![];
    let cfg = AgentConfig { frame_max_side: 256, ..Default::default() };
    let out =
        run_turn(&p, &mut h, &cfg, &mut conv, vec![Part::Text { text: "make a title slide up".into() }], &mut |e| events.push(e), &AtomicBool::new(false));
    assert_eq!(out.end, TurnEnd::Done, "{events:?}");
    assert_eq!(out.tool_calls, 3);
    assert_eq!(out.usage.output_tokens, 77 * 3);

    // The project really changed: a comp with the keyed title layer.
    let info = h.command("comp.info", json!({"comp": "Main"})).unwrap();
    assert_eq!(info["layers"][0]["name"], "Title");
    let pos = h.command("prop.get", json!({"comp": "Main", "layer": "Title", "path": "transform/position"})).unwrap();
    assert_eq!(pos["keys"].as_array().unwrap().len(), 2);

    // Frames came back to the panel and the model.
    let frames: usize = events.iter().map(|e| if let AgentEvent::ToolFinished { frames, .. } = e { frames.len() } else { 0 }).sum();
    assert_eq!(frames, 2);
    let finished_ok = events.iter().all(|e| !matches!(e, AgentEvent::ToolFinished { is_error: true, .. }));
    assert!(finished_ok, "{events:?}");

    // Conversation: user, assistant, tool results, assistant, tool results, assistant.
    let roles: Vec<Role> = conv.iter().map(|m| m.role).collect();
    assert_eq!(roles, vec![Role::User, Role::Assistant, Role::User, Role::Assistant, Role::User, Role::Assistant]);
    assert!(conv[0].text().contains("<editor_context>"));
    // The third request replays the earlier assistant turns verbatim and carries the frames.
    let third = &sent(&t)[2]["messages"];
    assert_eq!(third[1]["content"][1]["name"], "run_script");
    assert_eq!(third[4]["content"][0]["content"][1]["type"], "image");

    // Checkpoint: the turn's many steps become one undo entry, and Revert restores the project.
    assert!(cp.collapse(&mut h.session, "Merlin: make a title slide up"));
    assert_eq!(h.session.history.undo.len(), undo_before + 1);
    assert_eq!(h.session.history.undo.last().unwrap().0, "Merlin: make a title slide up");
    assert!(cp.revert(&mut h.session));
    assert!(!cp.changed(&h.session));
    assert!(h.command("comp.info", json!({"comp": "Main"})).is_err());
}

#[test]
fn script_errors_reach_the_model_as_error_results() {
    let (p, t) = anthropic(vec![
        anthropic_reply(None, None, &[("toolu_1", "run_script", json!({"code": "app.project.nope();"}))]),
        anthropic_reply(None, Some("Fixed."), &[]),
    ]);
    let mut h = host();
    let out = run_turn(&p, &mut h, &AgentConfig::default(), &mut vec![], vec![Part::Text { text: "x".into() }], &mut |_| {}, &AtomicBool::new(false));
    assert_eq!(out.end, TurnEnd::Done);
    let second = &sent(&t)[1]["messages"];
    let result = &second[2]["content"][0];
    assert_eq!(result["type"], "tool_result");
    assert_eq!(result["is_error"], true);
    assert!(result["content"][0]["text"].as_str().unwrap().contains("error"));
}

#[test]
fn a_stopped_turn_leaves_a_valid_conversation() {
    let (p, _) = anthropic(vec![anthropic_reply(None, None, &[("toolu_1", "get_project", json!({}))])]);
    let mut conv = vec![];
    let out = run_turn(&p, &mut host(), &AgentConfig::default(), &mut conv, vec![Part::Text { text: "x".into() }], &mut |_| {}, &AtomicBool::new(true));
    assert_eq!(out.end, TurnEnd::Cancelled);
    assert_eq!(conv.len(), 1);
}

#[test]
fn step_limit_stops_a_runaway_turn() {
    let (p, _) = anthropic(vec![anthropic_reply(None, None, &[("toolu_1", "get_project", json!({}))])]);
    let cfg = AgentConfig { max_steps: 3, ..Default::default() };
    let mut conv = vec![];
    let out = run_turn(&p, &mut host(), &cfg, &mut conv, vec![Part::Text { text: "x".into() }], &mut |_| {}, &AtomicBool::new(false));
    assert_eq!(out.end, TurnEnd::MaxSteps);
    assert_eq!(out.tool_calls, 3);
    // Every tool call got its result.
    assert_eq!(conv.last().unwrap().role, Role::User);
}

#[test]
fn the_system_prompt_example_script_runs() {
    let code = SYSTEM_PROMPT.split("```js").nth(1).and_then(|s| s.split("```").next()).unwrap();
    let mut h = host();
    h.command("script.run", json!({"code": "app.project.items.addComp(\"Ctx\", 1920, 1080, 1, 5, 30).openInViewer();"})).unwrap();
    let r = h.command("script.run", json!({"code": code})).unwrap();
    assert!(r["error"].is_null(), "the prompt's example fails: {r}");
}

#[test]
fn context_snapshot_describes_the_editor() {
    let mut h = host();
    let s = merlin_agent::context::snapshot(&mut h);
    assert!(s.contains("No composition is open"), "{s}");
    h.command("script.run", json!({"code": BUILD})).unwrap();
    let s = merlin_agent::context::snapshot(&mut h);
    assert!(s.contains("Active comp: \"Main\""), "{s}");
    assert!(s.contains("\"Title\""), "{s}");
}

#[test]
fn settings_hold_no_secrets() {
    let s = serde_json::to_string(&AgentConfig::default()).unwrap();
    assert!(!s.to_ascii_lowercase().contains("key"), "{s}");
}

#[test]
fn demo_provider_plays_a_full_turn_offline() {
    let p = merlin_agent::providers::demo::Demo { chunk_delay: std::time::Duration::ZERO };
    let mut h = host();
    let mut events = vec![];
    let cfg = AgentConfig { provider: ProviderKind::Demo, frame_max_side: 128, ..Default::default() };
    let out = run_turn(&p, &mut h, &cfg, &mut vec![], vec![Part::Text { text: "Hello \"world\"".into() }], &mut |e| events.push(e), &AtomicBool::new(false));
    assert_eq!(out.end, TurnEnd::Done);
    assert_eq!(out.tool_calls, 2);
    assert!(events.iter().all(|e| !matches!(e, AgentEvent::ToolFinished { is_error: true, .. })), "{events:?}");
    let info = h.command("comp.info", json!({})).unwrap();
    assert_eq!(info["name"], "Merlin Demo");
    assert_eq!(info["layers"][0]["name"], "Demo Title");
    assert_eq!(info["layers"][1]["name"], "Demo Background");
}

#[test]
fn notes_travel_as_frames_and_come_back_resolved() {
    use merlin_agent::notes::{Note, NoteBrief, time_label};
    use merlin_agent::{TurnContext, run_turn_with};
    let p = merlin_agent::providers::demo::Demo { chunk_delay: std::time::Duration::ZERO };
    let mut h = host();
    h.command(
        "script.run",
        json!({"code": "var c = app.project.items.addComp('Notes', 640, 360, 1, 4, 30); c.openInViewer(); c.layers.addSolid([1,0,0], 'Red', 640, 360, 1);"}),
    )
    .unwrap();
    let comp = h.command("comp.info", json!({})).unwrap()["id"].as_u64().unwrap();
    let brief = |id: u64, secs: f64, end: Option<f64>, send: bool| NoteBrief {
        note: Note {
            id,
            comp,
            start: effectcraft_time::Tick::from_seconds_f64(secs),
            end: end.map(effectcraft_time::Tick::from_seconds_f64),
            region: Some([10.0, 10.0, 100.0, 50.0]),
            text: format!("fix {id}"),
            ..Note::default()
        },
        at: time_label("t", 0, secs),
        until: end.map(|e| time_label("e", 0, e)),
        layers: vec![],
        comp_size: (640, 360),
        send,
    };
    let ctx = TurnContext {
        time: Some(time_label("0:00:00:00", 0, 0.0)),
        notes: vec![brief(1, 0.5, None, true), brief(2, 1.0, Some(2.0), true), brief(9, 3.0, None, false)],
        ..TurnContext::default()
    };
    let mut conv = vec![];
    let mut events = vec![];
    let cfg = AgentConfig { provider: ProviderKind::Demo, frame_max_side: 128, ..Default::default() };
    let out = run_turn_with(&p, &mut h, &cfg, &mut conv, vec![], &ctx, &mut |e| events.push(e), &AtomicBool::new(false));
    assert_eq!(out.end, TurnEnd::Done, "{events:?}");
    // The request carries 3 frames (one, then a range's two ends), the block and the context.
    let first = &conv[0].parts;
    let labels: Vec<&str> = first.iter().filter_map(|p| if let Part::Image { label, .. } = p { label.as_deref() } else { None }).collect();
    assert_eq!(labels, ["Note 1 frame, at t (frame 0, 0.500 s)", "Note 2 start frame, at t (frame 0, 1.000 s)", "Note 2 end frame, at e (frame 0, 2.000 s)"]);
    assert!(first.iter().any(|p| matches!(p, Part::Text { text } if text.starts_with("<notes>") && text.contains("Note 2 from") && !text.contains("Note 9"))));
    assert!(first.iter().any(|p| matches!(p, Part::Text { text } if text.contains("Current time: 0:00:00:00 (frame 0, 0.000 s)."))));
    let before: Vec<u64> = events.iter().filter_map(|e| if let AgentEvent::NoteFrame { id, .. } = e { Some(*id) } else { None }).collect();
    assert_eq!(before, [1, 2]);
    let resolved: Vec<(u64, bool, bool)> = events
        .iter()
        .filter_map(|e| if let AgentEvent::NoteResolved { id, addressed, frame, .. } = e { Some((*id, *addressed, frame.is_some())) } else { None })
        .collect();
    assert_eq!(resolved, [(1, true, true), (2, true, true)]);
    // Resolving a note the turn doesn't know is an error the model can read.
    let mut stray = vec![];
    let bad = merlin_agent::agent_loop::run_turn_with(
        &Fixed2("resolve_note", json!({"id": 77, "status": "addressed", "summary": "x"})),
        &mut h,
        &cfg,
        &mut vec![],
        vec![Part::Text { text: "x".into() }],
        &ctx,
        &mut |e| stray.push(e),
        &AtomicBool::new(false),
    );
    assert_eq!(bad.end, TurnEnd::Done);
    assert!(
        stray.iter().any(
            |e| matches!(e, AgentEvent::ToolFinished { is_error: true, summary, .. } if summary.contains("no note with id 77") && summary.contains("1, 2, 9"))
        ),
        "{stray:?}"
    );
}

#[test]
fn discard_leaves_no_undo_or_redo_step() {
    let mut h = host();
    h.command("script.run", json!({"code": BUILD})).unwrap();
    let undo = h.session.history.undo.len();
    let before = h.session.project.clone();
    let cp = Checkpoint::take(&h.session);
    h.command("script.run", json!({"code": "app.project.activeItem.layer('Title').name = 'Renamed'; app.project.activeItem.layers.addNull();"})).unwrap();
    h.command("layer.select", json!({"layers": ["#1"]})).unwrap();
    assert!(cp.changed(&h.session));
    assert!(cp.discard(&mut h.session));
    assert!(Arc::ptr_eq(&h.session.project, &before));
    assert_eq!((h.session.history.undo.len(), h.session.history.redo.len()), (undo, 0));
    assert!(cp.discard(&mut h.session), "nothing to discard is fine");
}

#[test]
fn pins_travel_as_frames_and_a_block() {
    use merlin_agent::{PinBrief, TurnContext, run_turn_with};
    let p = merlin_agent::providers::demo::Demo { chunk_delay: std::time::Duration::ZERO };
    let mut h = host();
    h.command("script.run", json!({"code": BUILD})).unwrap();
    let comp = h.command("comp.info", json!({})).unwrap()["id"].as_u64().unwrap();
    let t = effectcraft_time::Tick::from_seconds_f64(0.5);
    let ctx = TurnContext {
        pins: vec![
            PinBrief { line: "layer \"Title\" (id 2), locked".into(), frames: vec![] },
            PinBrief { line: "frame 0:00:00:15".into(), frames: vec![(comp, t, "Pinned frame 0:00:00:15".into())] },
        ],
        ..TurnContext::default()
    };
    let mut conv = vec![];
    let cfg = AgentConfig { provider: ProviderKind::Demo, frame_max_side: 64, ..Default::default() };
    run_turn_with(&p, &mut h, &cfg, &mut conv, vec![Part::Text { text: "make @Title pop".into() }], &ctx, &mut |_| {}, &AtomicBool::new(false));
    let first = &conv[0].parts;
    assert!(matches!(&first[1], Part::Image { label: Some(l), .. } if l == "Pinned frame 0:00:00:15"), "{first:?}");
    assert!(matches!(&first[2], Part::Text { text } if text.starts_with("<pins>") && text.contains("- layer \"Title\" (id 2), locked\n- frame 0:00:00:15\n")));
}

#[test]
fn plan_mode_reads_only_and_answers_with_beats() {
    use merlin_agent::{TurnContext, run_turn_with};
    let p = merlin_agent::providers::demo::Demo { chunk_delay: std::time::Duration::ZERO };
    let mut h = host();
    let before = h.session.project.clone();
    let ctx = TurnContext { plan_mode: true, current_plan: Some("- 0–1 s: old beat".into()), ..TurnContext::default() };
    let mut conv = vec![];
    let mut events = vec![];
    let cfg = AgentConfig { provider: ProviderKind::Demo, ..Default::default() };
    let out =
        run_turn_with(&p, &mut h, &cfg, &mut conv, vec![Part::Text { text: "Big Launch".into() }], &ctx, &mut |e| events.push(e), &AtomicBool::new(false));
    assert_eq!(out.end, TurnEnd::Done);
    assert!(Arc::ptr_eq(&h.session.project, &before), "nothing was built");
    let plan = events.iter().find_map(|e| if let AgentEvent::PlanProposed { title, beats } = e { Some((title.clone(), beats.clone())) } else { None });
    let (title, beats) = plan.expect("a plan");
    assert_eq!(title, "Big Launch");
    assert_eq!(beats.len(), 3);
    assert!(beats.windows(2).all(|w| w[0].start <= w[1].start));
    assert!(conv[0].parts.iter().any(|p| matches!(p, Part::Text { text } if text.contains("<current_plan>\n- 0–1 s: old beat"))));
    // Plan mode offers no editing tools, and refuses them if called anyway.
    let names: Vec<&str> = tools::plan_specs().iter().map(|t| t.name).collect();
    assert!(names.contains(&"render_frames") && names.contains(&"propose_plan") && !names.contains(&"run_script") && !names.contains(&"execute_command"));
    let mut ev = vec![];
    run_turn_with(
        &Fixed2("run_script", json!({"code": "app.project.items.addComp('X', 10, 10, 1, 1, 1);"})),
        &mut h,
        &cfg,
        &mut vec![],
        vec![Part::Text { text: "x".into() }],
        &ctx,
        &mut |e| ev.push(e),
        &AtomicBool::new(false),
    );
    assert!(ev.iter().any(|e| matches!(e, AgentEvent::ToolFinished { is_error: true, summary, .. } if summary.contains("off in plan mode"))), "{ev:?}");
    assert!(Arc::ptr_eq(&h.session.project, &before));
    // Outside plan mode propose_plan is refused.
    let mut ev = vec![];
    run_turn_with(
        &Fixed2("propose_plan", json!({"title": "t", "beats": []})),
        &mut h,
        &cfg,
        &mut vec![],
        vec![Part::Text { text: "x".into() }],
        &TurnContext::default(),
        &mut |e| ev.push(e),
        &AtomicBool::new(false),
    );
    assert!(ev.iter().any(|e| matches!(e, AgentEvent::ToolFinished { is_error: true, .. })));
}

/// Calls one tool once with a fixed input, then finishes.
struct Fixed2(&'static str, Value);

impl LlmProvider for Fixed2 {
    fn name(&self) -> &'static str {
        "fixed"
    }

    fn complete(
        &self,
        req: &ChatRequest,
        _: &mut dyn FnMut(merlin_agent::provider::StreamEvent),
        _: &AtomicBool,
    ) -> merlin_agent::Result<merlin_agent::provider::Reply> {
        let first = req.messages.last().is_some_and(|m| m.parts.iter().all(|p| !matches!(p, Part::ToolResult { .. })));
        let parts =
            if first { vec![Part::ToolCall { id: "c1".into(), name: self.0.into(), input: self.1.clone() }] } else { vec![Part::Text { text: "ok".into() }] };
        Ok(merlin_agent::provider::Reply {
            message: Message { role: Role::Assistant, parts, native: None },
            stop: if first { StopReason::ToolUse } else { StopReason::EndTurn },
            usage: Default::default(),
        })
    }
}

#[test]
fn the_style_kit_travels_and_remember_style_reports_additions() {
    use merlin_agent::style::{Feel, StyleKit, Swatch, block};
    use merlin_agent::{TurnContext, run_turn_with};
    let p = merlin_agent::providers::demo::Demo { chunk_delay: std::time::Duration::ZERO };
    let mut h = host();
    let kit = StyleKit { name: "Ember".into(), palette: vec![Swatch { name: "Ember".into(), hex: "#F26B21".into() }], ..Default::default() };
    let ctx = TurnContext { style: Some(block(&kit)), ..TurnContext::default() };
    let mut conv = vec![];
    let mut events = vec![];
    let cfg = AgentConfig { provider: ProviderKind::Demo, ..Default::default() };
    let out = run_turn_with(
        &p,
        &mut h,
        &cfg,
        &mut conv,
        vec![Part::Text { text: "remember: logo bottom right".into() }],
        &ctx,
        &mut |e| events.push(e),
        &AtomicBool::new(false),
    );
    assert_eq!(out.end, TurnEnd::Done);
    assert!(conv[0].parts.iter().any(|p| matches!(p, Part::Text { text } if text.starts_with("<style_kit name=\"Ember\">"))));
    let u = events.iter().find_map(|e| if let AgentEvent::StyleRemembered(u) = e { Some(u.clone()) } else { None }).expect("remembered");
    assert_eq!(u.colors[0].hex, "#F5C542");
    assert_eq!(u.feel, Some(Feel::Snappy));
    // Nothing to remember is an error the model can read; bad colours are dropped.
    let mut ev = vec![];
    run_turn_with(
        &Fixed2("remember_style", json!({"colors": [{"hex": "nope"}]})),
        &mut h,
        &cfg,
        &mut vec![],
        vec![Part::Text { text: "x".into() }],
        &ctx,
        &mut |e| ev.push(e),
        &AtomicBool::new(false),
    );
    assert!(ev.iter().any(|e| matches!(e, AgentEvent::ToolFinished { is_error: true, summary, .. } if summary.contains("nothing to remember"))));
    assert!(tools::plan_specs().iter().any(|t| t.name == "remember_style"), "plan mode can remember too");
}
