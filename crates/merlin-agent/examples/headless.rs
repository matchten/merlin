//! Run the agent without the app: one request against a fresh project, printing the reply and tool
//! calls as they stream, then saving the project and the frames the agent looked at.
//!
//! ```sh
//! ANTHROPIC_API_KEY=… cargo run -p merlin-agent --example headless -- "a title that drops in with a bounce"
//! cargo run -p merlin-agent --example headless -- --provider openai --model gpt-5 "…"
//! cargo run -p merlin-agent --example headless -- --provider custom --base-url http://localhost:11434/v1 --model qwen3 "…"
//! ```
//!
//! `--setup <js>` runs a script on the fresh project first; `--note "1.2:text"` or
//! `--note "1-2.5:text"` (comp seconds) adds a timeline note to apply, as the panel's storyboard does.
//!
//! `--plan` runs the turn in plan mode (read-only; the answer is a plan of timed beats).
//! `--lock-range 1-2` locks the turn's edits to that comp-time range, as a locked `@` range pin does.
//!
//! Output goes to `--out` (default `agent-out/`): `project.ecproj`, `frame-N.png`, `conversation.json`.

use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use merlin_agent::agent_loop::{AgentEvent, TurnContext, run_turn_with, turn_label};
use merlin_agent::checkpoint::Checkpoint;
use merlin_agent::conversation::Part;
use merlin_agent::notes::{Note, NoteBrief, time_label};
use merlin_agent::{AgentConfig, HeadlessHost, Host, ProviderKind, keys, providers, transport};

fn main() {
    if let Err(e) = run() {
        eprintln!("error: {e}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), String> {
    let mut cfg = AgentConfig::default();
    let mut out = std::path::PathBuf::from("agent-out");
    let mut prompt = vec![];
    let mut setup = None;
    let mut note_args = vec![];
    let mut lock = None;
    let mut plan = false;
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--provider" => {
                let p = args.next().unwrap_or_default();
                cfg.provider =
                    ProviderKind::ALL.into_iter().find(|k| k.id() == p).ok_or(format!("unknown provider `{p}` (anthropic, openai, gemini, custom)"))?;
            }
            "--model" => cfg.model = args.next().unwrap_or_default(),
            "--base-url" => cfg.base_url = args.next().unwrap_or_default(),
            "--effort" => cfg.effort = args.next().unwrap_or_default(),
            "--out" => out = args.next().unwrap_or_default().into(),
            "--setup" => setup = args.next(),
            "--note" => note_args.push(args.next().unwrap_or_default()),
            "--lock-range" => lock = args.next(),
            "--plan" => plan = true,
            _ => prompt.push(a),
        }
    }
    let prompt = prompt.join(" ");
    if prompt.trim().is_empty() && note_args.is_empty() {
        return Err("give a request, e.g. \"a title that drops in with a bounce\"".into());
    }
    std::fs::create_dir_all(&out).map_err(|e| e.to_string())?;
    let (key, source) = keys::get(cfg.provider);
    eprintln!("provider {} · model {} · key {:?}", cfg.provider.label(), cfg.model(), source);
    let provider = providers::build(&cfg, key.unwrap_or_default(), Arc::new(transport::Http::default()));

    let mut host = HeadlessHost::new(effectcraft_host::session());
    // Start like a new document in the app: one empty 1080p comp, open.
    host.command("comp.new", serde_json::json!({"name": "Main", "width": 1920, "height": 1080, "frameRate": 30, "duration": 5}))?;
    if let Some(js) = setup {
        let r = host.command("script.run", serde_json::json!({"code": js}))?;
        eprintln!("setup: {r}");
    }
    let checkpoint = Checkpoint::take(&host.session);
    let info = host.command("comp.info", serde_json::json!({}))?;
    let (comp, fps) = (info["id"].as_u64().unwrap_or(0), info["frameRate"].as_f64().unwrap_or(30.0));
    let size = (info["width"].as_u64().unwrap_or(0) as u32, info["height"].as_u64().unwrap_or(0) as u32);
    let label = |s: f64| time_label(&format!("{s:.2}s"), (s * fps).floor() as i64, s);
    let mut notes = vec![];
    for (i, n) in note_args.iter().enumerate() {
        let (when, text) = n.split_once(':').ok_or(format!("--note `{n}`: expected `secs:text` or `start-end:text`"))?;
        let (a, b) = match when.split_once('-') {
            Some((a, b)) => (a.trim().parse::<f64>().map_err(|e| e.to_string())?, Some(b.trim().parse::<f64>().map_err(|e| e.to_string())?)),
            None => (when.trim().parse::<f64>().map_err(|e| e.to_string())?, None),
        };
        let note = Note {
            id: i as u64 + 1,
            comp,
            start: effectcraft_time::Tick::from_seconds_f64(a),
            end: b.map(effectcraft_time::Tick::from_seconds_f64),
            text: text.trim().to_string(),
            ..Note::default()
        };
        notes.push(NoteBrief { note, at: label(a), until: b.map(label), layers: vec![], comp_size: size, send: true });
    }
    let mut ctx = TurnContext { time: Some(label(0.0)), notes, ..TurnContext::default() };
    ctx.plan_mode = plan;
    let mut scope = None;
    if let Some(r) = lock {
        let (a, b) = r.split_once('-').ok_or("--lock-range: expected `start-end` in seconds")?;
        let (a, b) = (a.trim().parse::<f64>().map_err(|e| e.to_string())?, b.trim().parse::<f64>().map_err(|e| e.to_string())?);
        let (ta, tb) = (effectcraft_time::Tick::from_seconds_f64(a), effectcraft_time::Tick::from_seconds_f64(b));
        ctx.pins.push(merlin_agent::PinBrief {
            line: format!("the time range {} to {}, locked: your edits must stay inside it", label(a), label(b)),
            frames: vec![(comp, ta, format!("Pinned range start, {a:.2}s")), (comp, tb, format!("Pinned range end, {b:.2}s"))],
        });
        scope = Some(merlin_agent::guard::Scope { comp, layers: None, range: Some((ta, tb)) });
    }
    let mut host = Guarded { inner: host, scope };

    let mut conversation = vec![];
    let mut frame_n = 0;
    let user = if prompt.trim().is_empty() { vec![] } else { vec![Part::Text { text: prompt.clone() }] };
    let outcome = run_turn_with(
        provider.as_ref(),
        &mut host,
        &cfg,
        &mut conversation,
        user,
        &ctx,
        &mut |e| match e {
            AgentEvent::Text(t) => eprint!("{t}"),
            AgentEvent::Thinking(_) => {}
            AgentEvent::ToolStarted { name, .. } => eprintln!("\n▸ {name}…"),
            AgentEvent::ToolRunning { .. } => {}
            AgentEvent::ToolFinished { name, is_error, summary, frames, .. } => {
                eprintln!("  {} {name}: {}", if is_error { "✗" } else { "✓" }, summary.replace('\n', " "));
                for png in frames {
                    frame_n += 1;
                    let _ = std::fs::write(out.join(format!("frame-{frame_n}.png")), png);
                }
            }
            AgentEvent::Usage(u) => eprintln!("  [{} in ({} cached) · {} out]", u.total_input(), u.cache_read_tokens, u.output_tokens),
            AgentEvent::NoteFrame { .. } => {}
            AgentEvent::StyleRemembered(u) => eprintln!("  remembered for the style kit: {u:?}"),
            AgentEvent::PlanProposed { title, beats } => {
                eprintln!("  plan “{title}”:");
                for b in beats {
                    eprintln!("    {:.2}–{:.2}s {}  [{}]", b.start, b.end, b.text, b.layers.join(", "));
                }
            }
            AgentEvent::NoteResolved { id, addressed, summary, .. } => {
                eprintln!("  note {id} {}: {summary}", if addressed { "addressed" } else { "won't fix" })
            }
        },
        &AtomicBool::new(false),
    );
    eprintln!("\n— {:?}, {} tool calls, {} output tokens", outcome.end, outcome.tool_calls, outcome.usage.output_tokens);
    let mut host = host.inner;
    checkpoint.collapse(&mut host.session, &turn_label(&prompt));

    let path = out.join("project.ecproj");
    host.command("file.saveAs", serde_json::json!({"path": path.to_string_lossy()}))?;
    let conv = serde_json::to_string_pretty(&conversation).map_err(|e| e.to_string())?;
    std::fs::write(out.join("conversation.json"), conv).map_err(|e| e.to_string())?;
    eprintln!("saved {} (open it in Merlin) and {frame_n} frame(s)", path.display());
    Ok(())
}

/// The headless host with the panel's pin guard: an edit outside the locked range is undone and
/// refused.
struct Guarded {
    inner: HeadlessHost,
    scope: Option<merlin_agent::guard::Scope>,
}

impl Host for Guarded {
    fn command(&mut self, id: &str, params: serde_json::Value) -> Result<serde_json::Value, String> {
        let Some(scope) = self.scope.clone() else { return self.inner.command(id, params) };
        let cp = Checkpoint::take(&self.inner.session);
        let r = self.inner.command(id, params)?;
        let out = merlin_agent::guard::check(cp.snapshot(), &self.inner.session.project, &scope, &|t| format!("{:.2}s", t.seconds()));
        if out.is_empty() {
            return Ok(r);
        }
        cp.discard(&mut self.inner.session);
        eprintln!("  ⛔ refused: {}", out.join("; "));
        Err(format!(
            "Refused: this edit went outside what the user pinned and locked, so none of it was applied:\n- {}\nMake the change within the locked pins, or tell the user it needs them unlocked.",
            out.join("\n- ")
        ))
    }

    fn render(&mut self, comp: Option<&serde_json::Value>, time: Option<f64>, max_side: u32) -> Result<merlin_agent::host::Frame, String> {
        self.inner.render(comp, time, max_side)
    }
}
