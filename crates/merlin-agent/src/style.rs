//! Style kits: what a designer would otherwise repeat in every request: the palette, the fonts,
//! how things move, brand rules. The active kit travels with each message as a `<style_kit>`
//! block; Merlin adds to it with the `remember_style` tool when asked to remember something.

use serde::{Deserialize, Serialize};

/// A named colour.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Swatch {
    pub name: String,
    /// `#rrggbb`.
    pub hex: String,
}

/// How things should move.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Feel {
    /// Gentle ease in and out.
    #[default]
    Smooth,
    /// Fast out, quick settle.
    Snappy,
    /// Overshoot and settle.
    Bouncy,
    /// Mechanical, constant speed.
    Linear,
}

impl Feel {
    pub const ALL: [Feel; 4] = [Feel::Smooth, Feel::Snappy, Feel::Bouncy, Feel::Linear];

    pub fn label(self) -> &'static str {
        match self {
            Feel::Smooth => "Smooth",
            Feel::Snappy => "Snappy",
            Feel::Bouncy => "Bouncy",
            Feel::Linear => "Linear",
        }
    }

    fn describe(self) -> &'static str {
        match self {
            Feel::Smooth => "smooth: gentle ease in and out (easy ease, ~70% influence)",
            Feel::Snappy => "snappy: fast starts and quick settles (strong ease out, short durations)",
            Feel::Bouncy => "bouncy: arrive with overshoot and settle",
            Feel::Linear => "linear: constant speed, mechanical",
        }
    }

    pub fn parse(s: &str) -> Option<Feel> {
        Feel::ALL.into_iter().find(|f| f.label().eq_ignore_ascii_case(s.trim()))
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Motion {
    pub feel: Feel,
    /// Overshoot as a share of the move (0.0–0.5).
    pub overshoot: f32,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct StyleKit {
    pub name: String,
    pub palette: Vec<Swatch>,
    pub fonts: Vec<String>,
    pub motion: Motion,
    /// Brand rules and taste, in the user's words.
    pub notes: String,
}

/// What `remember_style` adds to the kit.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct StyleUpdate {
    pub colors: Vec<Swatch>,
    pub fonts: Vec<String>,
    pub feel: Option<Feel>,
    pub overshoot: Option<f32>,
    pub notes: Option<String>,
}

/// `#RRGGBB`, `RRGGBB` or `#RGB` as bytes.
pub fn parse_hex(s: &str) -> Option<[u8; 3]> {
    let h = s.trim().trim_start_matches('#');
    let digits: Vec<u8> = h.chars().map(|c| c.to_digit(16).map(|d| d as u8)).collect::<Option<_>>()?;
    match digits.as_slice() {
        [r, g, b] => Some([r * 17, g * 17, b * 17]),
        [r1, r2, g1, g2, b1, b2] => Some([r1 * 16 + r2, g1 * 16 + g2, b1 * 16 + b2]),
        _ => None,
    }
}

pub fn hex(rgb: [u8; 3]) -> String {
    format!("#{:02X}{:02X}{:02X}", rgb[0], rgb[1], rgb[2])
}

/// The block sent with each message while the kit is active.
pub fn block(k: &StyleKit) -> String {
    let mut out = format!("<style_kit name=\"{}\">\nThe user's style kit: follow it unless they ask otherwise.\n", k.name.replace('"', "'"));
    if !k.palette.is_empty() {
        let colors: Vec<String> = k
            .palette
            .iter()
            .filter_map(|s| {
                let [r, g, b] = parse_hex(&s.hex)?;
                let name = if s.name.trim().is_empty() { String::new() } else { format!("{} ", s.name.trim()) };
                Some(format!("{name}{} = [{:.3}, {:.3}, {:.3}]", hex([r, g, b]), r as f64 / 255.0, g as f64 / 255.0, b as f64 / 255.0))
            })
            .collect();
        out.push_str(&format!("Palette (use only these colours): {}\n", colors.join("; ")));
    }
    if !k.fonts.is_empty() {
        out.push_str(&format!("Fonts: {}\n", k.fonts.join(", ")));
    }
    out.push_str(&format!("Motion: {}", k.motion.feel.describe()));
    if k.motion.overshoot > 0.001 {
        out.push_str(&format!(", overshoot about {:.0}% of the move", k.motion.overshoot * 100.0));
    }
    out.push_str(".\n");
    if !k.notes.trim().is_empty() {
        out.push_str(&format!("Notes: {}\n", k.notes.trim()));
    }
    out.push_str("</style_kit>");
    out
}

/// Merge `u` into `k`; returns what changed, in words (empty when nothing did).
pub fn merge(k: &mut StyleKit, u: &StyleUpdate) -> Vec<String> {
    let mut changed = vec![];
    let mut added = 0;
    for c in &u.colors {
        let Some(rgb) = parse_hex(&c.hex) else { continue };
        let hx = hex(rgb);
        if let Some(s) = k.palette.iter_mut().find(|s| parse_hex(&s.hex) == Some(rgb)) {
            if !c.name.trim().is_empty() && s.name != c.name.trim() {
                s.name = c.name.trim().to_string();
            }
            continue;
        }
        k.palette.push(Swatch { name: c.name.trim().to_string(), hex: hx });
        added += 1;
    }
    if added > 0 {
        changed.push(if added == 1 { "1 colour".to_string() } else { format!("{added} colours") });
    }
    let mut fonts = 0;
    for f in &u.fonts {
        let f = f.trim();
        if !f.is_empty() && !k.fonts.iter().any(|x| x.eq_ignore_ascii_case(f)) {
            k.fonts.push(f.to_string());
            fonts += 1;
        }
    }
    if fonts > 0 {
        changed.push(if fonts == 1 { "1 font".to_string() } else { format!("{fonts} fonts") });
    }
    if let Some(f) = u.feel.filter(|f| *f != k.motion.feel) {
        k.motion.feel = f;
        changed.push(format!("{} motion", f.label().to_lowercase()));
    }
    if let Some(o) = u.overshoot.filter(|o| o.is_finite()) {
        let o = o.clamp(0.0, 0.5);
        if (o - k.motion.overshoot).abs() > 0.001 {
            k.motion.overshoot = o;
            changed.push(format!("{:.0}% overshoot", o * 100.0));
        }
    }
    if let Some(n) = u.notes.as_deref().map(str::trim).filter(|n| !n.is_empty() && !k.notes.contains(*n)) {
        if !k.notes.trim().is_empty() {
            k.notes.push('\n');
        }
        k.notes.push_str(n);
        changed.push("a note".into());
    }
    changed
}

/// The `k` main colours of an image (k-means on a sample of its opaque pixels), most common first.
pub fn extract_palette(png: &[u8], k: usize) -> Result<Vec<[u8; 3]>, String> {
    let img = image::load_from_memory(png).map_err(|e| e.to_string())?.to_rgba8();
    let (w, h) = img.dimensions();
    let step = ((w as u64 * h as u64 / 20_000).max(1) as f64).sqrt().ceil() as u32;
    let mut px: Vec<[f32; 3]> = vec![];
    for y in (0..h).step_by(step.max(1) as usize) {
        for x in (0..w).step_by(step.max(1) as usize) {
            let p = img.get_pixel(x, y).0;
            if p[3] > 128 {
                px.push([p[0] as f32, p[1] as f32, p[2] as f32]);
            }
        }
    }
    let k = k.clamp(1, 12).min(px.len().max(1));
    if px.is_empty() {
        return Ok(vec![]);
    }
    // Seeds spread through the sample, then a few rounds of k-means.
    let mut centers: Vec<[f32; 3]> = (0..k).filter_map(|i| px.get(i * px.len() / k).copied()).collect();
    let mut counts = vec![0usize; centers.len()];
    for _ in 0..12 {
        let mut sums = vec![[0f32; 3]; centers.len()];
        counts = vec![0; centers.len()];
        for p in &px {
            let best = nearest(&centers, p);
            if let (Some(s), Some(c)) = (sums.get_mut(best), counts.get_mut(best)) {
                for i in 0..3 {
                    s[i] += p[i];
                }
                *c += 1;
            }
        }
        for ((c, s), n) in centers.iter_mut().zip(&sums).zip(&counts) {
            if *n > 0 {
                *c = [s[0] / *n as f32, s[1] / *n as f32, s[2] / *n as f32];
            }
        }
    }
    let mut out: Vec<(usize, [u8; 3])> =
        centers.iter().zip(&counts).filter(|(_, n)| **n > 0).map(|(c, n)| (*n, [c[0].round() as u8, c[1].round() as u8, c[2].round() as u8])).collect();
    out.sort_by_key(|b| std::cmp::Reverse(b.0));
    // Near-duplicates (seeds that met) count once.
    let mut uniq: Vec<[u8; 3]> = vec![];
    for (_, c) in out {
        if !uniq.iter().any(|u| dist(u, &c) < 24.0) {
            uniq.push(c);
        }
    }
    Ok(uniq)
}

fn nearest(centers: &[[f32; 3]], p: &[f32; 3]) -> usize {
    let d = |c: &[f32; 3]| (c[0] - p[0]).powi(2) + (c[1] - p[1]).powi(2) + (c[2] - p[2]).powi(2);
    centers.iter().enumerate().min_by(|a, b| d(a.1).total_cmp(&d(b.1))).map(|(i, _)| i).unwrap_or(0)
}

fn dist(a: &[u8; 3], b: &[u8; 3]) -> f32 {
    ((a[0] as f32 - b[0] as f32).powi(2) + (a[1] as f32 - b[1] as f32).powi(2) + (a[2] as f32 - b[2] as f32).powi(2)).sqrt()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_round_trips() {
        assert_eq!(parse_hex("#f26b21"), Some([0xF2, 0x6B, 0x21]));
        assert_eq!(parse_hex("FFF"), Some([255, 255, 255]));
        assert_eq!(parse_hex("#12345"), None);
        assert_eq!(parse_hex("zzzzzz"), None);
        assert_eq!(hex([1, 2, 255]), "#0102FF");
    }

    #[test]
    fn the_block_spells_out_the_kit() {
        let k = StyleKit {
            name: "Ember".into(),
            palette: vec![Swatch { name: "Ember".into(), hex: "#ff8000".into() }, Swatch { name: String::new(), hex: "nope".into() }],
            fonts: vec!["Inter".into()],
            motion: Motion { feel: Feel::Bouncy, overshoot: 0.15 },
            notes: "Never use pure white.".into(),
        };
        let b = block(&k);
        assert!(b.starts_with("<style_kit name=\"Ember\">") && b.ends_with("</style_kit>"));
        assert!(b.contains("Palette (use only these colours): Ember #FF8000 = [1.000, 0.502, 0.000]\n"), "{b}");
        assert!(b.contains("Fonts: Inter") && b.contains("bouncy") && b.contains("overshoot about 15%") && b.contains("Notes: Never use pure white."));
    }

    #[test]
    fn merging_adds_only_whats_new() {
        let mut k = StyleKit { palette: vec![Swatch { name: "Old".into(), hex: "#FF0000".into() }], fonts: vec!["Inter".into()], ..Default::default() };
        let u = StyleUpdate {
            colors: vec![
                Swatch { name: "Red".into(), hex: "ff0000".into() },
                Swatch { name: "Navy".into(), hex: "#001F3F".into() },
                Swatch { name: "x".into(), hex: "nope".into() },
            ],
            fonts: vec!["inter".into(), "Futura".into()],
            feel: Some(Feel::Snappy),
            overshoot: Some(9.0),
            notes: Some("Logo bottom right".into()),
        };
        let changed = merge(&mut k, &u);
        assert_eq!(changed, ["1 colour", "1 font", "snappy motion", "50% overshoot", "a note"]);
        assert_eq!(k.palette[0].name, "Red", "an existing colour is renamed, not duplicated");
        assert_eq!(k.palette[1].hex, "#001F3F");
        assert!(merge(&mut k, &u).is_empty(), "the same update again changes nothing");
    }

    #[test]
    fn palettes_come_out_of_images() {
        use image::ImageEncoder as _;
        let (w, h) = (40u32, 20u32);
        let mut px = vec![];
        for _y in 0..h {
            for x in 0..w {
                px.extend_from_slice(if x < 30 { &[240, 100, 30, 255] } else { &[20, 30, 60, 255] });
            }
        }
        let mut png = vec![];
        image::codecs::png::PngEncoder::new(&mut png).write_image(&px, w, h, image::ExtendedColorType::Rgba8).unwrap();
        let p = extract_palette(&png, 5).unwrap();
        assert_eq!(p, vec![[240, 100, 30], [20, 30, 60]], "two colours, the larger first");
        assert!(extract_palette(b"nope", 3).is_err());
    }
}
