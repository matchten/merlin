//! Comp frames rendered off the UI thread for the panel: a note's snapshot (the frame as it was
//! when the note was written, kept as PNG so it saves with the chat) and the large frame shown while
//! an area is marked.

use std::sync::{Arc, Mutex};

use effectcraft_engine::project::ItemId;
use effectcraft_engine::time::Tick;

use crate::EffectcraftApp;

/// The longest side of a note snapshot (tabs show them at most 112 px wide).
const SNAP_SIDE: u32 = 240;

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

/// A note snapshot being rendered: the note, which end (0 start, 1 end), the time, its slot (one
/// PNG).
pub(crate) struct SnapJob {
    id: u64,
    which: usize,
    at: Tick,
    slot: Slot,
}

/// Render `times` of `comp` off the UI thread, each at most `max_side`, all at once (a thread
/// each). `png`: encode them there too (the pixels come back as PNG bytes).
pub(super) fn spawn_frames(app: &EffectcraftApp, comp: u64, times: &[Tick], max_side: u32) -> Option<Slot> {
    spawn(app, comp, times, max_side, false)
}

fn spawn(app: &EffectcraftApp, comp: u64, times: &[Tick], max_side: u32, png: bool) -> Option<Slot> {
    let jobs: Vec<_> = times.iter().filter_map(|t| app.session.thumbnail_job(ItemId(comp), *t, max_side)).collect();
    if jobs.len() != times.len() {
        return None;
    }
    let slot: Slot = Arc::default();
    let s = slot.clone();
    std::thread::Builder::new()
        .name("merlin-frames".into())
        .spawn(move || {
            let handles: Vec<_> = jobs
                .into_iter()
                .map(|j| {
                    std::thread::Builder::new().name("merlin-frame".into()).spawn(move || {
                        let f = j()?;
                        if png {
                            let (w, h, px) = f;
                            encode_png(w, h, &px).map(|b| (w, h, b))
                        } else {
                            Some(f)
                        }
                    })
                })
                .collect();
            let out: Vec<_> = handles.into_iter().map(|h| h.ok().and_then(|h| h.join().ok()).flatten()).collect();
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

/// Render note `id`'s snapshot in the background: its start frame, and its end frame for a range,
/// each on its own (so the start shows while the end renders) and only when its time changed.
pub(super) fn request_snapshot(app: &mut EffectcraftApp, id: u64) {
    let Some(n) = app.ui.merlin.notes.iter().find(|n| n.id == id) else { return };
    let (s, e) = n.span();
    let comp = n.comp;
    let keys = snap_keys(id);
    let wanted = if e > s { vec![(0, s), (1, e)] } else { vec![(0, s)] };
    if e <= s {
        // No longer a range: drop the end frame.
        let rt = &mut app.ui.merlin.rt;
        rt.snap_jobs.retain(|j| !(j.id == id && j.which == 1));
        rt.snap_at.remove(&(id, 1));
        if let Some(k) = keys.get(1) {
            rt.frames.remove(k);
            rt.textures.remove(k);
        }
    }
    for (which, at) in wanted {
        let rt = &app.ui.merlin.rt;
        let have = rt.snap_at.get(&(id, which)) == Some(&at) && keys.get(which).is_some_and(|k| rt.frames.contains_key(k));
        let pending = rt.snap_jobs.iter().any(|j| j.id == id && j.which == which && j.at == at);
        if have || pending {
            continue;
        }
        let Some(slot) = spawn(app, comp, &[at], SNAP_SIDE, true) else { continue };
        let rt = &mut app.ui.merlin.rt;
        rt.snap_jobs.retain(|j| !(j.id == id && j.which == which));
        rt.snap_jobs.push(SnapJob { id, which, at, slot });
    }
}

/// Drop the snapshots rendering for note `id` (all notes for `None`).
pub(super) fn forget_snapshots(rt: &mut super::native::Runtime, id: Option<u64>) {
    rt.snap_jobs.retain(|j| id.is_some_and(|id| j.id != id));
}

/// Whether a snapshot of note `id` is still rendering.
pub(super) fn snapshot_pending(rt: &super::native::Runtime, id: u64) -> bool {
    rt.snap_jobs.iter().any(|j| j.id == id)
}

/// Take in finished snapshots (per frame): PNG into the frame store, the texture rebuilt.
pub(super) fn take_snapshots(app: &mut EffectcraftApp, ctx: &egui::Context) {
    let rt = &mut app.ui.merlin.rt;
    if rt.snap_jobs.is_empty() {
        return;
    }
    let mut waiting = vec![];
    for job in std::mem::take(&mut rt.snap_jobs) {
        let Some(frames) = take(&job.slot) else {
            waiting.push(job);
            continue;
        };
        let Some(key) = snap_keys(job.id).get(job.which).cloned() else { continue };
        rt.textures.remove(&key);
        match frames.into_iter().next().flatten() {
            Some((_, _, png)) => {
                rt.frames.insert(key, vec![png]);
                rt.snap_at.insert((job.id, job.which), job.at);
            }
            None => {
                rt.frames.remove(&key);
            }
        }
    }
    if !waiting.is_empty() {
        ctx.request_repaint_after(std::time::Duration::from_millis(30));
    }
    rt.snap_jobs = waiting;
}

fn encode_png(w: u32, h: u32, rgba: &[u8]) -> Option<Vec<u8>> {
    use image::ImageEncoder as _;
    let mut out = Vec::new();
    image::codecs::png::PngEncoder::new(&mut out).write_image(rgba, w, h, image::ExtendedColorType::Rgba8).ok()?;
    Some(out)
}
