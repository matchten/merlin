//! The tools the model can call. Each maps onto engine commands through [`Host`], with the same
//! semantics as the MCP server's tools (`crates/automation/src/tools.rs`), trimmed to what an
//! animation assistant needs.

use serde_json::{Map, Value, json};

use crate::conversation::{Image, ResultPart};
use crate::host::Host;
use crate::provider::ToolSpec;

/// What a tool call produced, for the model and for the panel.
#[derive(Clone, Debug, PartialEq)]
pub struct ToolOutput {
    pub content: Vec<ResultPart>,
    pub is_error: bool,
    /// Rendered frames as PNG (the panel shows them as thumbnails).
    pub frames: Vec<Vec<u8>>,
}

impl ToolOutput {
    fn json(v: Value) -> ToolOutput {
        ToolOutput { content: vec![ResultPart::Text { text: compact(&v) }], is_error: false, frames: vec![] }
    }

    fn error(msg: impl Into<String>) -> ToolOutput {
        ToolOutput { content: vec![ResultPart::Text { text: msg.into() }], is_error: true, frames: vec![] }
    }

    /// The text parts, joined (log lines, tests).
    pub fn text(&self) -> String {
        self.content
            .iter()
            .filter_map(|p| match p {
                ResultPart::Text { text } => Some(text.as_str()),
                ResultPart::Image { .. } => None,
            })
            .collect::<Vec<_>>()
            .join("\n")
    }
}

/// Cap on one tool result's text: big trees and listings are cut, with a note, so one call can't
/// flood the context window.
const MAX_RESULT_CHARS: usize = 60_000;

fn compact(v: &Value) -> String {
    let s = v.to_string();
    if s.len() <= MAX_RESULT_CHARS {
        return s;
    }
    let cut = s.char_indices().map(|(i, _)| i).take_while(|&i| i <= MAX_RESULT_CHARS).last().unwrap_or(0);
    format!("{}… [truncated: {} of {} characters shown; narrow the query]", s.get(..cut).unwrap_or(""), cut, s.len())
}

fn schema(props: Value, required: &[&str]) -> Value {
    // Kept to the subset every provider takes (Gemini's is the narrowest): no type unions, no
    // `additionalProperties`. Unknown keys are caught by the engine's own parameter check.
    json!({"type": "object", "properties": props, "required": required})
}
fn comp_s() -> Value {
    json!({"type": "string", "description": "Composition name or id (e.g. \"Main\" or \"12\"); omit for the active comp."})
}
fn layer_s() -> Value {
    json!({"type": "string", "description": "Layer name, \"#n\" (1-based index from the top) or id from get_comp (e.g. \"7\")."})
}
fn path_s() -> Value {
    json!({"type": "string", "description": "Property path from get_layer, e.g. `transform/position`, `transform/opacity`, `transform/scale`, `transform/rotation`, `effects/#1/blurriness`, `masks/#1/feather`, or `@uid`."})
}
fn value_s() -> Value {
    json!({"description": "Number for 1-D properties (50), array for vectors ([960,540]), \"#rrggbb\" or [r,g,b,a] (0-1) for colours, bool for checkboxes, string for text/menus."})
}

/// The catalogue, in the order the model sees it.
pub fn specs() -> Vec<ToolSpec> {
    vec![
        ToolSpec {
            name: "run_script",
            description: "Run JavaScript with the After Effects scripting object model (app.project, items.addComp, comp.layers.addText/addSolid/addShape/addNull, layer.property(\"ADBE Transform Group\").property(\"ADBE Position\").setValueAtTime(t, v), setTemporalEaseAtKey, property.expression = \"…\", Effects.addProperty(\"ADBE Gaussian Blur 2\"), markers, text documents, shape groups…). The main way to build and animate. Every edit is undoable. Returns {ok, result (the last expression's value), output ($.writeln / alert lines), error: {message, line, column} | null}; on an error fix the script and run it again.",
            input_schema: schema(json!({"code": {"type": "string", "description": "JavaScript source."}}), &["code"]),
        },
        ToolSpec {
            name: "execute_command",
            description: "Run any EffectCraft engine command by id with JSON params (layer.newText, comp.new, effect.apply, keys.easyEase, layer.timing, prop.set…). Unknown params are rejected with the accepted list. Use list_commands / describe_command to discover ids and params.",
            input_schema: schema(
                json!({"command": {"type": "string", "description": "Command id, e.g. `layer.newShape`."}, "params": {"type": "object", "description": "Command parameters."}}),
                &["command"],
            ),
        },
        ToolSpec {
            name: "list_commands",
            description: "Find engine commands: ids, labels and params docs, filtered by a substring of the id or label.",
            input_schema: schema(json!({"filter": {"type": "string", "description": "Substring, e.g. `keys.` or `shape`."}}), &["filter"]),
        },
        ToolSpec {
            name: "describe_command",
            description: "One command in full: params doc, JSON Schema, and whether it can run now (and why not).",
            input_schema: schema(json!({"command": {"type": "string"}}), &["command"]),
        },
        ToolSpec {
            name: "get_project",
            description: "Project overview: items (comps, footage, folders) with ids, the active comp, current time, selected layers.",
            input_schema: schema(json!({}), &[]),
        },
        ToolSpec {
            name: "get_comp",
            description: "A composition's settings (size, frame rate, duration, background, work area) and its layers (id, index, name, type, in/out, parent, switches).",
            input_schema: schema(json!({"comp": comp_s()}), &[]),
        },
        ToolSpec {
            name: "get_layer",
            description: "A layer's property tree with each property's path, value, keyframe count and expression. `depth` limits how many group levels expand (default all).",
            input_schema: schema(
                json!({"layer": layer_s(), "comp": comp_s(), "depth": {"type": "integer"}, "time": {"type": "number", "description": "Comp seconds to evaluate at."}}),
                &["layer"],
            ),
        },
        ToolSpec {
            name: "get_property",
            description: "One property: value at a time, keyframes (times are layer seconds) and expression.",
            input_schema: schema(json!({"layer": layer_s(), "path": path_s(), "comp": comp_s(), "time": {"type": "number"}}), &["layer", "path"]),
        },
        ToolSpec {
            name: "set_property",
            description: "Set a property's static value, or a keyframe when `time` is given (layer seconds unless timeBase is \"comp\"), and/or its expression (empty string removes it).",
            input_schema: schema(
                json!({"layer": layer_s(), "path": path_s(), "comp": comp_s(), "value": value_s(), "time": {"type": "number"}, "timeBase": {"type": "string", "enum": ["layer", "comp"]}, "expression": {"type": "string"}}),
                &["layer", "path"],
            ),
        },
        ToolSpec {
            name: "add_keyframes",
            description: "Add keyframes to a property in one call, then optionally set their interpolation (linear, bezier, hold, easyEase, easyEaseIn, easyEaseOut).",
            input_schema: schema(
                json!({
                    "layer": layer_s(), "path": path_s(), "comp": comp_s(),
                    "keys": {"type": "array", "items": {"type": "object", "properties": {"time": {"type": "number"}, "value": value_s()}, "required": ["time"]}},
                    "timeBase": {"type": "string", "enum": ["layer", "comp"]},
                    "interpolation": {"type": "string", "enum": ["linear", "bezier", "hold", "easyEase", "easyEaseIn", "easyEaseOut"]}
                }),
                &["layer", "path", "keys"],
            ),
        },
        ToolSpec {
            name: "add_effect",
            description: "Apply an effect to a layer and set its parameters by param id in one undo step; returns the instance path (effects/#n) and its parameter paths. Find effect ids with list_effects.",
            input_schema: schema(
                json!({"layer": layer_s(), "effect": {"type": "string", "description": "Effect id or name, e.g. `gaussianBlur`."}, "values": {"type": "object", "description": "Param id → value."}, "comp": comp_s()}),
                &["layer", "effect"],
            ),
        },
        ToolSpec {
            name: "list_effects",
            description: "Effect ids, names, categories and parameters, filtered by a substring.",
            input_schema: schema(json!({"filter": {"type": "string"}}), &["filter"]),
        },
        ToolSpec {
            name: "list_fonts",
            description: "Font families text layers can use, with their styles.",
            input_schema: schema(json!({"query": {"type": "string"}}), &[]),
        },
        ToolSpec {
            name: "render_frames",
            description: "Render frames of a comp and look at them, to check your work: give several times across an animation (e.g. start, middle, end, plus moments where things should happen). Returns the images.",
            input_schema: schema(
                json!({"times": {"type": "array", "items": {"type": "number"}, "description": "Comp seconds (at most 8)."}, "comp": comp_s()}),
                &["times"],
            ),
        },
        ToolSpec {
            name: "import_file",
            description: "Import a file (image, video, audio, .ai, .svg, .pdf, .psd) into the project, e.g. one the user attached; returns the new item. Pass `addToComp` to also place it as a layer in the active comp.",
            input_schema: schema(json!({"path": {"type": "string", "description": "Absolute path."}, "addToComp": {"type": "boolean"}}), &["path"]),
        },
    ]
}

/// Most frames one `render_frames` call returns (each is input tokens for the model).
pub const MAX_FRAMES: usize = 8;

/// Run tool `name` with `input` on `host`. Failures come back as an error result for the model to
/// read and recover from, never as a panic or an aborted turn.
pub fn run(host: &mut dyn Host, name: &str, input: &Value, frame_max_side: u32) -> ToolOutput {
    if let Some(raw) = input.get("_invalidInput") {
        return ToolOutput::error(format!("The tool input was not valid JSON (it may have been cut off): {raw}. Send the call again."));
    }
    let r = match name {
        "run_script" => run_script(host, input),
        "execute_command" => execute_command(host, input),
        "list_commands" => list_commands(host, input),
        "describe_command" => need_str(input, "command").and_then(|c| host.command("command.describe", json!({"command": c}))).map(ToolOutput::json),
        "get_project" => host.command("project.summary", json!({})).map(ToolOutput::json),
        "get_comp" => host.command("comp.info", obj(&[("comp", get(input, "comp"))])).map(ToolOutput::json),
        "get_layer" => host
            .command(
                "layer.tree",
                obj(&[("layer", get(input, "layer")), ("comp", get(input, "comp")), ("depth", get(input, "depth")), ("time", get(input, "time"))]),
            )
            .map(ToolOutput::json),
        "get_property" => host
            .command(
                "prop.get",
                obj(&[("layer", get(input, "layer")), ("path", get(input, "path")), ("comp", get(input, "comp")), ("time", get(input, "time"))]),
            )
            .map(ToolOutput::json),
        "set_property" => set_property(host, input),
        "add_keyframes" => add_keyframes(host, input),
        "add_effect" => add_effect(host, input),
        "list_effects" => host.command("effect.list", obj(&[("filter", get(input, "filter"))])).map(ToolOutput::json),
        "list_fonts" => host.command("text.fonts", obj(&[("query", get(input, "query"))])).map(ToolOutput::json),
        "render_frames" => render_frames(host, input, frame_max_side),
        "import_file" => import_file(host, input),
        other => Err(format!("unknown tool `{other}`")),
    };
    r.unwrap_or_else(ToolOutput::error)
}

type R = Result<ToolOutput, String>;

fn get<'a>(a: &'a Value, k: &str) -> Option<&'a Value> {
    a.get(k).filter(|v| !v.is_null())
}

fn need<'a>(a: &'a Value, k: &str) -> Result<&'a Value, String> {
    get(a, k).ok_or_else(|| format!("missing `{k}`"))
}

fn need_str<'a>(a: &'a Value, k: &str) -> Result<&'a str, String> {
    need(a, k)?.as_str().ok_or_else(|| format!("`{k}` must be a string"))
}

/// A params object from the present, non-null entries.
fn obj(entries: &[(&str, Option<&Value>)]) -> Value {
    let mut m = Map::new();
    for (k, v) in entries {
        if let Some(v) = v.filter(|v| !v.is_null()) {
            m.insert((*k).to_string(), v.clone());
        }
    }
    Value::Object(m)
}

fn run_script(host: &mut dyn Host, a: &Value) -> R {
    let code = need_str(a, "code")?;
    let r = host.command("script.run", json!({"code": code, "name": "Merlin"}))?;
    // A script error is the model's to fix: flag it so providers mark the result as an error.
    let failed = r.get("error").is_some_and(|e| !e.is_null());
    let mut out = ToolOutput::json(r);
    out.is_error = failed;
    Ok(out)
}

fn execute_command(host: &mut dyn Host, a: &Value) -> R {
    let id = need_str(a, "command")?;
    let params = match get(a, "params") {
        None => json!({}),
        // Some models stringify nested objects.
        Some(Value::String(s)) if s.trim().is_empty() => json!({}),
        Some(Value::String(s)) => serde_json::from_str(s).map_err(|e| format!("`params` is not valid JSON: {e}"))?,
        Some(v) => v.clone(),
    };
    host.command(id, params).map(ToolOutput::json)
}

fn list_commands(host: &mut dyn Host, a: &Value) -> R {
    let r = host.command("command.list", obj(&[("filter", get(a, "filter"))]))?;
    let list: Vec<Value> = r
        .as_array()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .map(|mut c| {
            if let Some(o) = c.as_object_mut() {
                o.retain(|k, v| !(v.is_null() || v.as_array().is_some_and(Vec::is_empty) || v.as_str() == Some("{}") || k == "menu"));
            }
            c
        })
        .collect();
    Ok(ToolOutput::json(json!(list)))
}

fn set_property(host: &mut dyn Host, a: &Value) -> R {
    let (layer, path, comp, time) = (need(a, "layer")?, need(a, "path")?, get(a, "comp"), get(a, "time"));
    let (value, expr) = (get(a, "value"), a.get("expression").filter(|e| e.is_string()));
    if value.is_none() && expr.is_none() {
        return Err("give `value` and/or `expression`".into());
    }
    let base = [("layer", Some(layer)), ("path", Some(path)), ("comp", comp)];
    if let Some(v) = value {
        let mut p = obj(&base);
        p["value"] = v.clone();
        if let Some(t) = time {
            p["time"] = t.clone();
            if let Some(tb) = get(a, "timeBase") {
                p["timeBase"] = tb.clone();
            }
            host.command("prop.addKey", p)?;
        } else {
            host.command("prop.set", p)?;
        }
    }
    if let Some(e) = expr {
        let mut p = obj(&base);
        p["expression"] = e.clone();
        host.command("prop.setExpression", p)?;
    }
    host.command("prop.get", obj(&[("layer", Some(layer)), ("path", Some(path)), ("comp", comp), ("time", time)])).map(ToolOutput::json)
}

fn add_keyframes(host: &mut dyn Host, a: &Value) -> R {
    let (layer, path, comp) = (need(a, "layer")?, need(a, "path")?, get(a, "comp"));
    let keys = need(a, "keys")?.as_array().ok_or("`keys` must be an array of {time, value?}")?;
    if keys.is_empty() {
        return Err("no keys".into());
    }
    let base = [("layer", Some(layer)), ("path", Some(path)), ("comp", comp)];
    let tb = get(a, "timeBase");
    for k in keys {
        let mut p = obj(&base);
        p["time"] = need(k, "time")?.clone();
        if let Some(v) = get(k, "value") {
            p["value"] = v.clone();
        }
        if let Some(tb) = tb {
            p["timeBase"] = tb.clone();
        }
        host.command("prop.addKey", p)?;
    }
    let info = host.command("prop.get", obj(&base))?;
    let Some(interp) = get(a, "interpolation").and_then(Value::as_str) else { return Ok(ToolOutput::json(info)) };
    // Key selection works in the active comp.
    if let Some(c) = comp {
        host.command("comp.open", json!({"comp": c}))?;
    }
    let sel: Vec<Value> = keys.iter().map(|k| json!({"layer": info["layer"], "prop": info["uid"], "time": k["time"]})).collect();
    let mut select = json!({"keys": sel});
    if let Some(tb) = tb {
        select["timeBase"] = tb.clone();
    }
    host.command("keys.select", select)?;
    match interp.to_ascii_lowercase().as_str() {
        "easyease" => host.command("keys.easyEase", json!({}))?,
        "easyeasein" => host.command("keys.easyEase", json!({"which": "in"}))?,
        "easyeaseout" => host.command("keys.easyEase", json!({"which": "out"}))?,
        i @ ("linear" | "bezier" | "hold") => host.command("keys.interpolation", json!({"interpolation": i}))?,
        other => return Err(format!("unknown interpolation `{other}`")),
    };
    host.command("prop.get", obj(&base)).map(ToolOutput::json)
}

/// The `effects` group of a `layer.tree` reply.
fn effects_group(tree: &Value) -> Option<&Value> {
    tree["properties"]["children"].as_array()?.iter().find(|c| c["path"] == "effects")
}

fn effect_count(tree: &Value) -> usize {
    effects_group(tree).and_then(|g| g.get("children")).and_then(Value::as_array).map_or(0, Vec::len)
}

fn add_effect(host: &mut dyn Host, a: &Value) -> R {
    let (layer, effect, comp) = (need(a, "layer")?, need(a, "effect")?, get(a, "comp"));
    let before = host.command("layer.tree", obj(&[("layer", Some(layer)), ("comp", comp)]))?;
    let index = effect_count(&before).saturating_add(1);
    let mut p = obj(&[("effect", Some(effect)), ("comp", comp)]);
    p["layers"] = json!([layer]);
    let mut steps = vec![json!({"command": "effect.apply", "params": p})];
    let prefix = format!("effects/#{index}");
    if let Some(vals) = get(a, "values").and_then(Value::as_object) {
        for (k, v) in vals {
            let path = json!(format!("{prefix}/{k}"));
            steps.push(json!({"command": "prop.set", "params": obj(&[("layer", Some(layer)), ("comp", comp), ("path", Some(&path)), ("value", Some(v))])}));
        }
    }
    // One batch: one undo step, and a failing value leaves nothing applied.
    host.command("engine.batch", json!({"steps": steps}))?;
    let tree = host.command("layer.tree", obj(&[("layer", Some(layer)), ("comp", comp)]))?;
    let inst = effects_group(&tree).and_then(|g| g.get("children")).and_then(Value::as_array).and_then(|c| c.get(index.saturating_sub(1))).cloned();
    Ok(ToolOutput::json(json!({"layer": tree["id"], "path": prefix, "effect": inst})))
}

fn render_frames(host: &mut dyn Host, a: &Value, max_side: u32) -> R {
    let times: Vec<f64> =
        need(a, "times")?.as_array().ok_or("`times` must be an array of seconds")?.iter().filter_map(Value::as_f64).filter(|t| t.is_finite()).collect();
    if times.is_empty() {
        return Err("give at least one time".into());
    }
    let mut out = ToolOutput { content: vec![], is_error: false, frames: vec![] };
    let mut notes = vec![];
    if times.len() > MAX_FRAMES {
        notes.push(format!("Only the first {MAX_FRAMES} of {} times were rendered.", times.len()));
    }
    for t in times.iter().take(MAX_FRAMES) {
        let f = host.render(get(a, "comp"), Some(t.max(0.0)), max_side)?;
        out.content.push(ResultPart::Text { text: format!("Frame at {:.3}s ({}×{}):", f.time, f.width, f.height) });
        out.content.push(ResultPart::Image { image: Image::png(&f.png) });
        out.frames.push(f.png);
    }
    if !notes.is_empty() {
        out.content.push(ResultPart::Text { text: notes.join(" ") });
    }
    Ok(out)
}

fn import_file(host: &mut dyn Host, a: &Value) -> R {
    let path = need_str(a, "path")?;
    let mut p = json!({"paths": [path]});
    if get(a, "addToComp").and_then(Value::as_bool) == Some(true) {
        p["addToComp"] = json!(true);
    }
    host.command("file.import", p).map(ToolOutput::json)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_names_are_unique_and_schemas_are_objects() {
        let specs = specs();
        let mut names: Vec<_> = specs.iter().map(|s| s.name).collect();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), specs.len());
        for s in &specs {
            assert_eq!(s.input_schema["type"], "object", "{}", s.name);
            assert!(!s.input_schema.to_string().contains("additionalProperties"), "{}", s.name);
            assert!(!s.input_schema.to_string().contains("\"type\":["), "{}: type unions are not portable", s.name);
        }
    }

    #[test]
    fn long_results_are_cut_at_a_char_boundary() {
        let s = compact(&json!("é".repeat(MAX_RESULT_CHARS)));
        assert!(s.contains("[truncated"));
        assert!(s.len() < MAX_RESULT_CHARS + 200);
    }
}
