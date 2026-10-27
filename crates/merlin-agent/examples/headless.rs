//! Run the agent without the app: one request against a fresh project, printing the reply and tool
//! calls as they stream, then saving the project and the frames the agent looked at.
//!
//! ```sh
//! ANTHROPIC_API_KEY=… cargo run -p merlin-agent --example headless -- "a title that drops in with a bounce"
//! cargo run -p merlin-agent --example headless -- --provider openai --model gpt-5 "…"
//! cargo run -p merlin-agent --example headless -- --provider custom --base-url http://localhost:11434/v1 --model qwen3 "…"
//! ```
//!
//! Output goes to `--out` (default `agent-out/`): `project.ecproj`, `frame-N.png`, `conversation.json`.

use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use merlin_agent::agent_loop::{AgentEvent, run_turn, turn_label};
use merlin_agent::checkpoint::Checkpoint;
use merlin_agent::conversation::Part;
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
            _ => prompt.push(a),
        }
    }
    let prompt = prompt.join(" ");
    if prompt.trim().is_empty() {
        return Err("give a request, e.g. \"a title that drops in with a bounce\"".into());
    }
    std::fs::create_dir_all(&out).map_err(|e| e.to_string())?;
    let (key, source) = keys::get(cfg.provider);
    eprintln!("provider {} · model {} · key {:?}", cfg.provider.label(), cfg.model(), source);
    let provider = providers::build(&cfg, key.unwrap_or_default(), Arc::new(transport::Http::default()));

    let mut host = HeadlessHost::new(effectcraft_host::session());
    // Start like a new document in the app: one empty 1080p comp, open.
    host.command("comp.new", serde_json::json!({"name": "Main", "width": 1920, "height": 1080, "frameRate": 30, "duration": 5}))?;
    let checkpoint = Checkpoint::take(&host.session);

    let mut conversation = vec![];
    let mut frame_n = 0;
    let outcome = run_turn(
        provider.as_ref(),
        &mut host,
        &cfg,
        &mut conversation,
        vec![Part::Text { text: prompt.clone() }],
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
            AgentEvent::NoteResolved { id, addressed, summary, .. } => {
                eprintln!("  note {id} {}: {summary}", if addressed { "addressed" } else { "won't fix" })
            }
        },
        &AtomicBool::new(false),
    );
    eprintln!("\n— {:?}, {} tool calls, {} output tokens", outcome.end, outcome.tool_calls, outcome.usage.output_tokens);
    checkpoint.collapse(&mut host.session, &turn_label(&prompt));

    let path = out.join("project.ecproj");
    host.command("file.saveAs", serde_json::json!({"path": path.to_string_lossy()}))?;
    let conv = serde_json::to_string_pretty(&conversation).map_err(|e| e.to_string())?;
    std::fs::write(out.join("conversation.json"), conv).map_err(|e| e.to_string())?;
    eprintln!("saved {} (open it in EffectCraft) and {frame_n} frame(s)", path.display());
    Ok(())
}
