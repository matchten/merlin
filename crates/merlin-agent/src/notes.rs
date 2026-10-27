//! Notes: requests pinned to a moment of a comp ("too slow here", "the logo should be bigger at
//! this frame"), like review comments on a cut. The user drops them on the panel's storyboard strip
//! and applies them as one turn; each travels as the frame it points at plus a line in a `<notes>`
//! block, and the agent reports each one done with the `resolve_note` tool. Plan mode's beats are
//! notes Merlin writes.

use effectcraft_time::Tick;
use serde::{Deserialize, Serialize};

/// Who wrote a note.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Author {
    #[default]
    User,
    Merlin,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "camelCase")]
pub enum NoteStatus {
    /// Waiting to be applied.
    #[default]
    Open,
    /// Sent to Merlin; not reported on yet.
    Sent,
    Addressed {
        summary: String,
    },
    WontFix {
        summary: String,
    },
    /// A beat of a plan Merlin proposed, waiting for Build.
    Proposed,
}

impl NoteStatus {
    pub fn is_done(&self) -> bool {
        matches!(self, NoteStatus::Addressed { .. } | NoteStatus::WontFix { .. })
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Note {
    pub id: u64,
    /// The comp (item id).
    pub comp: u64,
    /// Comp time of the frame it points at (the start, for a range).
    pub start: Tick,
    /// The end of a range (comp time), when it covers one.
    #[serde(default)]
    pub end: Option<Tick>,
    /// Layers it is about (ids).
    #[serde(default)]
    pub layers: Vec<u64>,
    /// Properties it is about, as shown (`Title ▸ Position`).
    #[serde(default)]
    pub props: Vec<String>,
    /// An area of the frame it is about: `[x, y, w, h]` in comp pixels.
    #[serde(default)]
    pub region: Option<[f32; 4]>,
    pub text: String,
    #[serde(default)]
    pub author: Author,
    #[serde(default)]
    pub status: NoteStatus,
}

impl Note {
    /// Its start and end (equal for a single frame), in order.
    pub fn span(&self) -> (Tick, Tick) {
        let end = self.end.unwrap_or(self.start);
        (self.start.min(end), self.start.max(end))
    }
}

/// A note as sent to the agent, with its times and layers spelled out the way the app shows them
/// (the panel fills these in: it knows the project's time display style and the layer names).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct NoteBrief {
    pub note: Note,
    /// The start, e.g. `0:00:01:12 (frame 42, 1.400 s)`.
    pub at: String,
    /// The end of a range, same form.
    pub until: Option<String>,
    /// The layers, e.g. `"Title" (id 7)`.
    pub layers: Vec<String>,
    /// The comp's pixel size (to place `region` on the rendered frame).
    pub comp_size: (u32, u32),
    /// Part of this turn's request (others are listed only so `resolve_note` knows them).
    pub send: bool,
}

/// The `<notes>` block for the notes being sent.
pub fn block(briefs: &[NoteBrief]) -> String {
    let mut out = String::from(
        "<notes>\nThe user left these notes on the timeline. Each note's frame is attached above it; treat the note as how that moment should look or move. Address every note, re-render its frame to check, then call resolve_note for it.\n",
    );
    for b in briefs.iter().filter(|b| b.send) {
        let n = &b.note;
        let when = match &b.until {
            Some(u) => format!("from {} to {}", b.at, u),
            None => format!("at {}", b.at),
        };
        let beat = if n.author == Author::Merlin { " (a beat of the plan the user approved)" } else { "" };
        out.push_str(&format!("- Note {}{beat} {when}", n.id));
        if !b.layers.is_empty() {
            out.push_str(&format!(", about {}", b.layers.join(", ")));
        }
        if !n.props.is_empty() {
            out.push_str(&format!(" (properties: {})", n.props.join(", ")));
        }
        if let Some([x, y, w, h]) = n.region {
            out.push_str(&format!(", the area x {x:.0}–{:.0}, y {y:.0}–{:.0} px (outlined in its frame)", x + w, y + h));
        }
        out.push_str(&format!(": {}\n", n.text.trim()));
    }
    out.push_str("</notes>");
    out
}

/// The label of a note's frame image.
pub fn frame_label(b: &NoteBrief, end: bool) -> String {
    match (&b.until, end) {
        (Some(u), true) => format!("Note {} end frame, at {u}", b.note.id),
        (Some(_), false) => format!("Note {} start frame, at {}", b.note.id, b.at),
        (None, _) => format!("Note {} frame, at {}", b.note.id, b.at),
    }
}

/// `png` with the note's region outlined (in the colour of the panel's note pins), or unchanged
/// when it has none. `comp_size` maps comp pixels onto the rendered frame.
pub fn outline(png: &[u8], comp_size: (u32, u32), region: Option<[f32; 4]>) -> Result<Vec<u8>, String> {
    let Some([x, y, w, h]) = region else { return Ok(png.to_vec()) };
    let mut img = image::load_from_memory(png).map_err(|e| e.to_string())?.to_rgba8();
    let (iw, ih) = img.dimensions();
    if comp_size.0 == 0 || comp_size.1 == 0 || iw == 0 || ih == 0 {
        return Ok(png.to_vec());
    }
    let sx = iw as f32 / comp_size.0 as f32;
    let sy = ih as f32 / comp_size.1 as f32;
    let clamp_x = |v: f32| (v * sx).round().clamp(0.0, (iw - 1) as f32) as u32;
    let clamp_y = |v: f32| (v * sy).round().clamp(0.0, (ih - 1) as f32) as u32;
    let (x0, x1) = (clamp_x(x.min(x + w)), clamp_x(x.max(x + w)));
    let (y0, y1) = (clamp_y(y.min(y + h)), clamp_y(y.max(y + h)));
    let thick = (iw.max(ih) / 256).max(2);
    let amber = image::Rgba([255u8, 184, 0, 255]);
    for py in y0..=y1 {
        for px in x0..=x1 {
            let edge = px < x0 + thick || px + thick > x1 || py < y0 + thick || py + thick > y1;
            if edge && let Some(p) = img.get_pixel_mut_checked(px, py) {
                *p = amber;
            }
        }
    }
    let mut out = Vec::new();
    img.write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png).map_err(|e| e.to_string())?;
    Ok(out)
}

/// A time as the model reads it: the app's display, the frame and the seconds.
pub fn time_label(display: &str, frame: i64, seconds: f64) -> String {
    format!("{display} (frame {frame}, {seconds:.3} s)")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn brief(id: u64, send: bool) -> NoteBrief {
        NoteBrief {
            note: Note { id, comp: 3, start: Tick::from_seconds_f64(1.4), text: format!("  note {id} text "), ..Note::default() },
            at: time_label("0:00:01:12", 42, 1.4),
            until: None,
            layers: vec![],
            comp_size: (1920, 1080),
            send,
        }
    }

    #[test]
    fn the_block_lists_only_the_notes_sent() {
        let mut ranged = brief(2, true);
        ranged.until = Some(time_label("0:00:02:00", 60, 2.0));
        ranged.layers = vec!["\"Title\" (id 7)".into()];
        ranged.note.region = Some([100.0, 50.0, 200.0, 80.0]);
        ranged.note.props = vec!["Title ▸ Position".into()];
        let s = block(&[brief(1, true), ranged, brief(3, false)]);
        assert!(s.starts_with("<notes>\n") && s.ends_with("</notes>"));
        assert!(s.contains("- Note 1 at 0:00:01:12 (frame 42, 1.400 s): note 1 text\n"), "{s}");
        assert!(
            s.contains(
                "- Note 2 from 0:00:01:12 (frame 42, 1.400 s) to 0:00:02:00 (frame 60, 2.000 s), about \"Title\" (id 7) (properties: Title ▸ Position), the area x 100–300, y 50–130 px"
            ),
            "{s}"
        );
        assert!(!s.contains("Note 3"));
        assert_eq!(frame_label(&brief(1, true), false), "Note 1 frame, at 0:00:01:12 (frame 42, 1.400 s)");
    }

    #[test]
    fn spans_are_ordered() {
        let n = Note { start: Tick(10), end: Some(Tick(4)), ..Note::default() };
        assert_eq!(n.span(), (Tick(4), Tick(10)));
        assert_eq!(Note { start: Tick(7), ..Note::default() }.span(), (Tick(7), Tick(7)));
    }

    #[test]
    fn outline_draws_the_region_scaled_to_the_frame() {
        use image::ImageEncoder as _;
        let (w, h) = (96u32, 54u32);
        let mut png = Vec::new();
        image::codecs::png::PngEncoder::new(&mut png).write_image(&vec![0u8; (w * h * 4) as usize], w, h, image::ExtendedColorType::Rgba8).unwrap();
        assert_eq!(outline(&png, (1920, 1080), None).unwrap(), png);
        // A comp-space box at 400..800 × 200..600 lands at 20..40 × 10..30 on a 1/20 frame.
        let out = outline(&png, (1920, 1080), Some([400.0, 200.0, 400.0, 400.0])).unwrap();
        let img = image::load_from_memory(&out).unwrap().to_rgba8();
        assert_eq!(img.get_pixel(20, 20).0, [255, 184, 0, 255]);
        assert_eq!(img.get_pixel(30, 10).0, [255, 184, 0, 255]);
        assert_eq!(img.get_pixel(30, 20).0[0], 0, "the inside stays clear");
        assert_eq!(img.get_pixel(60, 20).0[0], 0, "outside stays clear");
        // Out-of-frame regions are clamped, not a panic.
        assert!(outline(&png, (1920, 1080), Some([-500.0, 5000.0, 99999.0, -9.0])).is_ok());
        assert!(outline(b"not a png", (1920, 1080), Some([0.0, 0.0, 1.0, 1.0])).is_err());
    }
}
