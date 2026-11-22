//! Renders icons/svg/<name>.svg (and <name>-small.svg for <= 24 px, if present)
//! into icons/<name>.ico with PNG-compressed entries at every common size.
//!
//!   cargo run --release --manifest-path tools/iconbake/Cargo.toml -- icons/svg icons
//!   cargo run --release --manifest-path tools/iconbake/Cargo.toml -- icons/ui icons/ui --ui

use resvg::{tiny_skia, usvg};
use std::path::Path;

const SIZES: &[u32] = &[16, 20, 24, 32, 40, 48, 64, 96, 128, 256];
/// In-app glyphs: 100 %-300 % of 16 px.
const UI_SIZES: &[u32] = &[16, 20, 24, 28, 32, 40, 48];

fn render(svg: &Path, px: u32) -> Vec<u8> {
    let data = std::fs::read(svg).unwrap_or_else(|e| panic!("{}: {e}", svg.display()));
    let tree = usvg::Tree::from_data(&data, &usvg::Options::default()).expect("parse svg");
    let mut pm = tiny_skia::Pixmap::new(px, px).unwrap();
    let s = px as f32 / tree.size().width();
    resvg::render(&tree, tiny_skia::Transform::from_scale(s, s), &mut pm.as_mut());
    pm.encode_png().expect("png")
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let (src, dst) = (Path::new(&args[1]), Path::new(&args[2]));
    let sizes = if args.iter().any(|a| a == "--ui") { UI_SIZES } else { SIZES };
    for entry in std::fs::read_dir(src).unwrap() {
        let p = entry.unwrap().path();
        let stem = p.file_stem().unwrap().to_string_lossy().to_string();
        if p.extension().map_or(true, |e| e != "svg") || stem.ends_with("-small") {
            continue;
        }
        let small = src.join(format!("{stem}-small.svg"));
        let pngs: Vec<(u32, Vec<u8>)> = sizes
            .iter()
            .map(|&px| (px, render(if px <= 24 && small.exists() { &small } else { &p }, px)))
            .collect();

        let mut ico = Vec::new();
        ico.extend_from_slice(&[0, 0, 1, 0]);
        ico.extend_from_slice(&(pngs.len() as u16).to_le_bytes());
        let mut offset = 6 + 16 * pngs.len() as u32;
        for (px, png) in &pngs {
            let b = if *px >= 256 { 0 } else { *px as u8 };
            ico.extend_from_slice(&[b, b, 0, 0]);
            ico.extend_from_slice(&1u16.to_le_bytes());
            ico.extend_from_slice(&32u16.to_le_bytes());
            ico.extend_from_slice(&(png.len() as u32).to_le_bytes());
            ico.extend_from_slice(&offset.to_le_bytes());
            offset += png.len() as u32;
        }
        for (_, png) in &pngs {
            ico.extend_from_slice(png);
        }
        let out = dst.join(format!("{stem}.ico"));
        std::fs::write(&out, &ico).unwrap();
        println!("{} ({} bytes)", out.display(), ico.len());
    }
}
