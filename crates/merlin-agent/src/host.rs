//! Where the agent's tools run. [`RemoteHost`] sends each call as a control-channel request (the
//! desktop app forwards them to its UI thread, between frames); [`HeadlessHost`] owns a session.

use effectcraft_engine::Session;
use effectcraft_engine::time::Tick;
use serde_json::{Value, json};

/// A rendered comp frame.
#[derive(Clone, Debug, PartialEq)]
pub struct Frame {
    pub png: Vec<u8>,
    pub comp: Value,
    /// Comp seconds.
    pub time: f64,
    pub width: u32,
    pub height: u32,
}

pub trait Host: Send {
    /// Run an engine command (`Session::execute_checked`): any command id, query or `script.run`.
    fn command(&mut self, id: &str, params: Value) -> Result<Value, String>;

    /// Render a frame of `comp` (default the active comp) at `time` comp seconds (default the
    /// current time), the longest side at most `max_side`.
    fn render(&mut self, comp: Option<&Value>, time: Option<f64>, max_side: u32) -> Result<Frame, String>;
}

/// A host over an owned session (tests, examples, a future CLI).
pub struct HeadlessHost {
    pub session: Session,
}

impl HeadlessHost {
    pub fn new(session: Session) -> HeadlessHost {
        HeadlessHost { session }
    }
}

impl Host for HeadlessHost {
    fn command(&mut self, id: &str, params: Value) -> Result<Value, String> {
        let r = self.session.execute_checked(id, params).map_err(|e| e.to_string());
        // Nothing listens for UI events headless; don't let them pile up.
        self.session.drain_events();
        r
    }

    fn render(&mut self, comp: Option<&Value>, time: Option<f64>, max_side: u32) -> Result<Frame, String> {
        let s = &self.session;
        let cid = s.resolve_comp(comp).map_err(|e| e.to_string())?;
        let t = time.map(Tick::from_seconds_f64).unwrap_or_else(|| s.time());
        let (w, h, rgba) = s.render_rgba8_alpha(cid, t, max_side, false).map_err(|e| e.to_string())?;
        let png = effectcraft_automation::encode_png(w, h, rgba, max_side).map_err(|e| e.to_string())?;
        Ok(Frame { png, comp: json!(cid.0), time: t.seconds(), width: w, height: h })
    }
}

/// A host that speaks the control-channel protocol (`docs/control-protocol.md`): `call(method,
/// params)` returns the reply object `{ok, result | error}`. The desktop app's call posts the request
/// to its UI thread; a TCP client to `--control` would work the same.
pub struct RemoteHost<F: FnMut(&str, Value) -> Result<Value, String> + Send> {
    call: F,
}

impl<F: FnMut(&str, Value) -> Result<Value, String> + Send> RemoteHost<F> {
    pub fn new(call: F) -> Self {
        RemoteHost { call }
    }

    fn request(&mut self, method: &str, params: Value) -> Result<Value, String> {
        let reply = (self.call)(method, params)?;
        if reply["ok"] == true {
            Ok(reply.get("result").cloned().unwrap_or(Value::Null))
        } else {
            Err(reply["error"].as_str().map(str::to_string).unwrap_or_else(|| format!("{method} failed: {reply}")))
        }
    }
}

impl<F: FnMut(&str, Value) -> Result<Value, String> + Send> Host for RemoteHost<F> {
    fn command(&mut self, id: &str, params: Value) -> Result<Value, String> {
        self.request("engine.execute", json!({"command": id, "params": params}))
    }

    fn render(&mut self, comp: Option<&Value>, time: Option<f64>, max_side: u32) -> Result<Frame, String> {
        let mut p = json!({"max_side": max_side, "base64": true});
        if let Some(c) = comp.filter(|c| !c.is_null()) {
            p["comp"] = c.clone();
        }
        if let Some(t) = time {
            p["time"] = json!(t);
        }
        let r = self.request("render.frame", p)?;
        let png = r["png"].as_str().and_then(effectcraft_automation::base64::decode).ok_or("render.frame returned no image")?;
        let dim = |k: &str| r[k].as_u64().and_then(|v| u32::try_from(v).ok()).unwrap_or(0);
        Ok(Frame { png, comp: r["comp"].clone(), time: r["time"].as_f64().unwrap_or(0.0), width: dim("width"), height: dim("height") })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remote_host_unwraps_replies_and_decodes_frames() {
        let mut sent = vec![];
        {
            let mut h = RemoteHost::new(|m: &str, p: Value| {
                sent.push((m.to_string(), p.clone()));
                Ok(match m {
                    "render.frame" => json!({"ok": true, "result": {"comp": 3, "time": 1.5, "width": 4, "height": 2, "png": "iVBO"}}),
                    _ if p["command"] == "bad" => json!({"ok": false, "error": "unknown command `bad`"}),
                    _ => json!({"ok": true, "result": {"layer": 2}}),
                })
            });
            assert_eq!(h.command("layer.newText", json!({"text": "hi"})).unwrap(), json!({"layer": 2}));
            assert_eq!(h.command("bad", json!({})).unwrap_err(), "unknown command `bad`");
            let f = h.render(None, Some(1.5), 64).unwrap();
            assert_eq!((f.width, f.height, f.time, f.png.as_slice()), (4, 2, 1.5, &[0x89, 0x50, 0x4e][..]));
        }
        assert_eq!(sent[0], ("engine.execute".into(), json!({"command": "layer.newText", "params": {"text": "hi"}})));
        assert_eq!(sent[2].1, json!({"max_side": 64, "base64": true, "time": 1.5}));
    }
}
