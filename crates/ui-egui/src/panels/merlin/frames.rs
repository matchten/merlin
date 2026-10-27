//! Comp frames rendered off the UI thread for the panel: a note's snapshot (the frame as it was
//! when the note was written, kept as PNG so it saves with the chat) and the large frame shown while
//! an area is marked.

use std::sync::{Arc, Mutex};

use effectcraft_engine::project::ItemId;
use effectcraft_engine::time::Tick;

use crate::EffectcraftApp;

/// The longest side of a note snapshot.
const SNAP_SIDE: u32 = 360;

/// Frames rendered off the UI thread: one result per job, in order.
pub(crate) type Slot = Arc<Mutex<Option<Vec<Option<(u32, u32, Vec<u8>)>>>>>;

/// What a rendered frame shows.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) struct FrameKey {
    pub(super) comp: u64,
    pub(super) revision: u64,
    pub(super) at: Tick,
}

/// A frame being rendered.
pub(crate) struct Pending {
    pub(super) key: FrameKey,
    pub(super) slot: Slot,
}

/// Note snapshots being rendered: the note and its slot (start frame, then end frame for a range).
pub(crate) type SnapJob = (u64, Slot);

/// Render `times` of `comp` off the UI thread, each at most `max_side`.
pub(super) fn spawn_frames(app: &EffectcraftApp, comp: u64, times: &[Tick], max_side: u32) -> Option<Slot> {
    let jobs: Vec<_> = times.iter().filter_map(|t| app.session.thumbnail_job(ItemId(comp), *t, max_side)).collect();
    if jobs.len() != times.len() {
        return None;
    }
    let slot: Slot = Arc::default();
    let s = slot.clone();
    std::thread::Builder::new()
        .name("merlin-frames".into())
        .spawn(move || {
            let out: Vec<_> = jobs.into_iter().map(|j| j()).collect();
            if let Ok(mut s) = s.lock() {
                *s = Some(out);
            }
        })
        .ok()?;
    Some(slot)
}

/// The finished frames of `slot`, or `None` while they render.
fn take(slot: &Slot) -> Option<Vec<Option<(u32, u32, Vec<u8>)>>> {
    slot.lock().ok().and_then(|mut s| s.take())
}

/// The finished frames of `pending` as textures, or `None` while it runs.
pub(super) fn take_textures(ctx: &egui::Context, pending: &Pending, name: &str) -> Option<Vec<egui::TextureHandle>> {
    Some(
        take(&pending.slot)?
            .into_iter()
            .enumerate()
            .filter_map(|(i, f)| {
                let (w, h, px) = f?;
                let ci = egui::ColorImage::from_rgba_unmultiplied([w as usize, h as usize], &px);
                Some(ctx.load_texture(format!("{name}-{i}"), ci, egui::TextureOptions::LINEAR))
            })
            .collect(),
    )
}

/// The frame keys of note `id`'s snapshot (start, end).
pub(super) fn snap_keys(id: u64) -> [String; 2] {
    [format!("note-snap:{id}"), format!("note-snap-end:{id}")]
}

/// Render note `id`'s snapshot (its start frame, and its end frame for a range) in the background.
pub(super) fn request_snapshot(app: &mut EffectcraftApp, id: u64) {
    let Some(n) = app.ui.merlin.notes.iter().find(|n| n.id == id) else { return };
    let (s, e) = n.span();
    let times = if e > s { vec![s, e] } else { vec![s] };
    let comp = n.comp;
    let Some(slot) = spawn_frames(app, comp, &times, SNAP_SIDE) else { return };
    let rt = &mut app.ui.merlin.rt;
    rt.snap_jobs.retain(|(n, _)| *n != id);
    rt.snap_jobs.push((id, slot));
}

/// Take in finished snapshots (per frame): PNG into the frame store, the textures rebuilt.
pub(super) fn take_snapshots(app: &mut EffectcraftApp, ctx: &egui::Context) {
    let rt = &mut app.ui.merlin.rt;
    if rt.snap_jobs.is_empty() {
        return;
    }
    let mut waiting = vec![];
    for (id, slot) in std::mem::take(&mut rt.snap_jobs) {
        let Some(frames) = take(&slot) else {
            waiting.push((id, slot));
            continue;
        };
        let keys = snap_keys(id);
        for k in &keys {
            rt.frames.remove(k);
            rt.textures.remove(k);
        }
        for (key, f) in keys.iter().zip(frames) {
            if let Some(png) = f.and_then(|(w, h, px)| encode_png(w, h, &px)) {
                rt.frames.insert(key.clone(), vec![png]);
            }
        }
    }
    if !waiting.is_empty() {
        ctx.request_repaint_after(std::time::Duration::from_millis(50));
    }
    rt.snap_jobs = waiting;
}

fn encode_png(w: u32, h: u32, rgba: &[u8]) -> Option<Vec<u8>> {
    use image::ImageEncoder as _;
    let mut out = Vec::new();
    image::codecs::png::PngEncoder::new(&mut out).write_image(rgba, w, h, image::ExtendedColorType::Rgba8).ok()?;
    Some(out)
}
