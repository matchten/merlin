//! Where the agent's tools run. The desktop app implements [`Host`] by forwarding each call to the
//! UI thread (between frames, like the control channel); [`HeadlessHost`] owns a session.

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
