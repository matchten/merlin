//! Attachments decode like footage: stills, transparent artwork, SVG, Photoshop and PDF /
//! Illustrator files become labelled PNGs within the size limit; folders, audio and junk are
//! refused with a reason.

use merlin_agent::attachment::{self, MAX_SIDE};
use merlin_agent::conversation::Part;

fn dir(name: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("merlin-attach-{}-{name}", std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn decoded(png: &[u8]) -> image::RgbaImage {
    image::load_from_memory(png).unwrap().to_rgba8()
}

#[test]
fn a_large_png_is_shrunk_and_labelled_with_its_path() {
    let d = dir("png");
    let path = d.join("reference.png");
    image::RgbaImage::from_pixel(3200, 1800, image::Rgba([200, 30, 30, 255])).save(&path).unwrap();
    let a = attachment::load(path.to_str().unwrap()).unwrap();
    assert_eq!((a.width, a.height), (3200, 1800));
    assert!(!a.video && !a.flattened);
    let img = decoded(&a.png);
    assert_eq!(img.width().max(img.height()), MAX_SIDE);
    let px = img.get_pixel(10, 10).0;
    assert!(px[0] > 180 && px[1] < 60, "{px:?}");
    let label = a.label();
    assert!(label.contains("reference.png") && label.contains("3200×1800") && label.contains(path.to_str().unwrap()), "{label}");
    assert!(matches!(a.part(), Part::Image { label: Some(l), .. } if l == label));
}

#[test]
fn transparent_artwork_is_flattened_on_grey() {
    let d = dir("alpha");
    let path = d.join("logo.png");
    let mut img = image::RgbaImage::from_pixel(64, 64, image::Rgba([0, 0, 0, 0]));
    for x in 16..48 {
        for y in 16..48 {
            img.put_pixel(x, y, image::Rgba([255, 255, 255, 255]));
        }
    }
    img.save(&path).unwrap();
    let a = attachment::load(path.to_str().unwrap()).unwrap();
    assert!(a.flattened);
    assert!(a.label().contains("mid-grey"));
    let out = decoded(&a.png);
    let corner = out.get_pixel(0, 0).0;
    assert_eq!(corner[3], 255);
    assert!((110..=150).contains(&corner[0]), "{corner:?}");
    assert!(out.get_pixel(32, 32).0[0] > 240);
}

#[test]
fn svg_psd_and_pdf_artwork_rasterize() {
    let d = dir("vector");
    let svg = d.join("badge.svg");
    std::fs::write(&svg, r##"<svg xmlns="http://www.w3.org/2000/svg" width="200" height="100"><rect width="200" height="100" fill="#00ff00"/></svg>"##)
        .unwrap();
    let a = attachment::load(svg.to_str().unwrap()).unwrap();
    assert_eq!((a.width, a.height), (200, 100));
    let px = decoded(&a.png).get_pixel(100, 50).0;
    assert!(px[1] > 200 && px[0] < 60, "{px:?}");

    use effectcraft_psd::Rect;
    use effectcraft_psd::write::{WDoc, WLayer};
    let mut doc = WDoc::new(120, 80);
    doc.layers.push(WLayer::solid("Blue", Rect::new(0, 0, 120, 80), [0.0, 0.0, 1.0, 1.0]));
    let psd = d.join("comp.psd");
    std::fs::write(&psd, effectcraft_psd::write::write(&doc)).unwrap();
    let a = attachment::load(psd.to_str().unwrap()).unwrap();
    assert_eq!(a.format, "PSD");
    assert_eq!((a.width, a.height), (120, 80));
    let px = decoded(&a.png).get_pixel(60, 40).0;
    assert!(px[2] > 200 && px[0] < 60, "{px:?}");

    // A one-page PDF with a red square; an Illustrator file is the same bytes named .ai.
    let content = "1 0 0 rg 0 0 100 100 re f";
    let objs = [
        "<< /Type /Catalog /Pages 2 0 R >>".to_string(),
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_string(),
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] /Contents 4 0 R >>".to_string(),
        format!("<< /Length {} >>\nstream\n{content}\nendstream", content.len()),
    ];
    let mut pdf = b"%PDF-1.4\n".to_vec();
    let mut offsets = vec![];
    for (i, o) in objs.iter().enumerate() {
        offsets.push(pdf.len());
        pdf.extend(format!("{} 0 obj\n{o}\nendobj\n", i + 1).bytes());
    }
    let xref = pdf.len();
    pdf.extend(format!("xref\n0 {}\n0000000000 65535 f \n", objs.len() + 1).bytes());
    for o in offsets {
        pdf.extend(format!("{o:010} 00000 n \n").bytes());
    }
    pdf.extend(format!("trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n", objs.len() + 1).bytes());
    for name in ["art.pdf", "logo.ai"] {
        let p = d.join(name);
        std::fs::write(&p, &pdf).unwrap();
        let a = attachment::load(p.to_str().unwrap()).unwrap_or_else(|e| panic!("{name}: {e}"));
        assert!(a.width > 0 && a.height > 0, "{name}: {a:?}");
        let img = decoded(&a.png);
        let px = img.get_pixel(img.width() / 2, img.height() / 2).0;
        assert!(px[0] > 200 && px[1] < 60, "{name}: {px:?}");
    }
}

#[test]
fn folders_missing_files_and_non_pictures_are_refused() {
    let d = dir("bad");
    let e = attachment::load(d.to_str().unwrap()).unwrap_err();
    assert!(e.contains("folder"), "{e}");
    assert!(attachment::load(d.join("nope.png").to_str().unwrap()).is_err());
    let junk = d.join("notes.png");
    std::fs::write(&junk, b"definitely not a picture").unwrap();
    assert!(attachment::load(junk.to_str().unwrap()).is_err());
    let wav = d.join("tone.wav");
    let mut w = Vec::new();
    let data = vec![0u8; 800];
    w.extend(b"RIFF");
    w.extend(((36 + data.len()) as u32).to_le_bytes());
    w.extend(b"WAVEfmt ");
    w.extend(16u32.to_le_bytes());
    w.extend(1u16.to_le_bytes());
    w.extend(1u16.to_le_bytes());
    w.extend(8000u32.to_le_bytes());
    w.extend(16000u32.to_le_bytes());
    w.extend(2u16.to_le_bytes());
    w.extend(16u16.to_le_bytes());
    w.extend(b"data");
    w.extend((data.len() as u32).to_le_bytes());
    w.extend(&data);
    std::fs::write(&wav, w).unwrap();
    let e = attachment::load(wav.to_str().unwrap()).unwrap_err();
    assert!(e.contains("audio") || e.contains("no picture"), "{e}");
}
