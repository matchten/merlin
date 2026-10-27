//! Files the user attaches to a message as visual context: a reference frame, a mood board, a logo
//! to use. Each is decoded the way footage is ([`effectcraft_media::probe`] and the pool's
//! thumbnail: PNG/JPEG/… stills, Photoshop documents, SVG, PDF, Illustrator and EPS files, and a
//! video's first frame), shrunk to fit the models' image limits and sent as a labelled image whose
//! label gives the file's path, so the agent can `import_file` the original.

use effectcraft_project::FootageKind;
use serde::{Deserialize, Serialize};

use crate::conversation::{Image, Part};

/// The longest side sent to a model (the providers downscale anything larger anyway).
pub const MAX_SIDE: u32 = 1568;
/// Attachments one message may carry.
pub const MAX_PER_MESSAGE: usize = 8;
/// File extensions offered by the attach dialog: stills, layered and vector files, movies.
pub fn extensions() -> Vec<&'static str> {
    effectcraft_media::STILL_EXTENSIONS.iter().chain(effectcraft_media::VIDEO_EXTENSIONS).copied().collect()
}
/// Files larger than this are refused before reading (movies are read in place, not whole).
pub const MAX_FILE_BYTES: u64 = 512 * 1024 * 1024;

/// A decoded attachment, ready to send.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Attachment {
    /// The original file (absolute).
    pub path: String,
    /// The file name, as shown.
    pub name: String,
    /// What the file is: `PNG`, `PSD`, `SVG`, `PDF`, `H.264`…
    pub format: String,
    /// The file's own pixel size.
    pub width: u32,
    pub height: u32,
    /// A movie (the image is its first frame).
    pub video: bool,
    /// Transparent areas were filled with mid-grey so light and dark artwork both stay visible.
    pub flattened: bool,
    /// The image sent (PNG, at most [`MAX_SIDE`] on its longest side).
    #[serde(skip)]
    pub png: Vec<u8>,
}

impl Attachment {
    /// How the model is told about it.
    pub fn label(&self) -> String {
        let what = if self.video { format!("{} video, first frame shown", self.format) } else { self.format.clone() };
        let mut s = format!("{} ({what}, {}×{}) at path {}", self.name, self.width, self.height, self.path);
        if self.flattened {
            s.push_str("; transparent areas are shown as mid-grey");
        }
        s
    }

    /// The message part for it.
    pub fn part(&self) -> Part {
        Part::Image { image: Image::png(&self.png), label: Some(self.label()) }
    }
}

/// Decode `path` into an attachment (blocking: run it off the UI thread).
pub fn load(path: &str) -> Result<Attachment, String> {
    let p = std::path::Path::new(path);
    let name = p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| path.to_string());
    let meta = std::fs::metadata(p).map_err(|e| format!("{name}: {e}"))?;
    if meta.is_dir() {
        return Err(format!("{name} is a folder: attach the image files in it"));
    }
    if meta.len() > MAX_FILE_BYTES && !effectcraft_media::VIDEO_EXTENSIONS.iter().any(|e| name.to_ascii_lowercase().ends_with(&format!(".{e}"))) {
        return Err(format!("{name} is too large to attach ({} MB)", meta.len() / (1024 * 1024)));
    }
    let footage = effectcraft_media::probe(p).map_err(|e| format!("{name}: {e}"))?;
    match footage.kind {
        FootageKind::Audio => return Err(format!("{name} is audio: Merlin can only look at pictures")),
        FootageKind::Model => return Err(format!("{name} is a 3D model: Merlin can't look at it yet (import it with File ▸ Import)")),
        FootageKind::Data => return Err(format!("{name} is a data file: Merlin can only look at pictures")),
        FootageKind::Video | FootageKind::Still | FootageKind::Sequence => {}
    }
    if !footage.has_video {
        return Err(format!("{name} has no picture"));
    }
    let pool = effectcraft_media::MediaPool::with_budget(64 * 1024 * 1024);
    let img = pool.thumbnail(&footage, MAX_SIDE).map_err(|e| format!("{name}: {e}"))?;
    if img.width == 0 || img.height == 0 {
        return Err(format!("{name}: the image is empty"));
    }
    let flattened = img.data.iter().any(|p| p[3] < 0.999);
    let rgba = if flattened { img.to_rgba8_over([0.5, 0.5, 0.5]) } else { img.to_rgba8() };
    let png = encode_png(&rgba, img.width, img.height).map_err(|e| format!("{name}: {e}"))?;
    let video = footage.kind == FootageKind::Video && footage.duration > effectcraft_time::Tick::ZERO;
    let format =
        if footage.codec.is_empty() { p.extension().map(|e| e.to_string_lossy().to_ascii_uppercase()).unwrap_or_default() } else { footage.codec.clone() };
    Ok(Attachment { path: path.to_string(), name, format, width: footage.width, height: footage.height, video, flattened, png })
}

fn encode_png(rgba: &[u8], w: u32, h: u32) -> Result<Vec<u8>, String> {
    use image::ImageEncoder as _;
    let mut out = Vec::new();
    image::codecs::png::PngEncoder::new(&mut out).write_image(rgba, w, h, image::ExtendedColorType::Rgba8).map_err(|e| e.to_string())?;
    Ok(out)
}
