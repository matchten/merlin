//! What the agent is told about the editor with every user message: the active comp, its layers,
//! the selection and the current time, so "make *this* bounce" resolves. Volatile, so it travels in
//! the user message (the system prompt stays byte-identical for caching).

use serde_json::{Value, json};

use crate::host::Host;

/// Layers listed per comp before the list is cut (the agent can call get_comp for the rest).
const MAX_LAYERS: usize = 40;

/// A short text block describing the editor state. Never fails: missing pieces are left out.
pub fn snapshot(host: &mut dyn Host) -> String {
    let summary = host.command("project.summary", json!({})).unwrap_or(Value::Null);
    let mut out = String::from("<editor_context>\n");
    let comps: Vec<String> = summary["items"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or_default()
        .iter()
        .filter(|i| i["type"] == "comp")
        .map(|i| format!("{} (id {})", i["name"].as_str().unwrap_or("?"), i["id"]))
        .collect();
    out.push_str(&format!("Project compositions: {}\n", if comps.is_empty() { "none".to_string() } else { comps.join(", ") }));
    if summary["activeComp"].is_null() {
        out.push_str("No composition is open.\n");
    } else if let Ok(c) = host.command("comp.info", json!({})) {
        out.push_str(&format!(
            "Active comp: \"{}\" (id {}), {}×{} px, {} fps, duration {} s. Current time: {} s.\n",
            c["name"].as_str().unwrap_or("?"),
            c["id"],
            c["width"],
            c["height"],
            c["frameRate"],
            c["duration"],
            summary["time"]
        ));
        let layers = c["layers"].as_array().map(Vec::as_slice).unwrap_or_default();
        if layers.is_empty() {
            out.push_str("It has no layers.\n");
        } else {
            out.push_str("Layers (top first):\n");
            for l in layers.iter().take(MAX_LAYERS) {
                out.push_str(&format!(
                    "  #{} \"{}\" (id {}, {}), in {} s, out {} s\n",
                    l["index"],
                    l["name"].as_str().unwrap_or("?"),
                    l["id"],
                    l["type"].as_str().unwrap_or("?"),
                    l["in"],
                    l["out"]
                ));
            }
            if layers.len() > MAX_LAYERS {
                out.push_str(&format!("  … and {} more (get_comp lists all)\n", layers.len() - MAX_LAYERS));
            }
        }
    }
    let selected: Vec<String> = summary["selectedLayers"].as_array().map(Vec::as_slice).unwrap_or_default().iter().map(Value::to_string).collect();
    if !selected.is_empty() {
        out.push_str(&format!("Selected layer ids: {}\n", selected.join(", ")));
    }
    out.push_str("</editor_context>");
    out
}
