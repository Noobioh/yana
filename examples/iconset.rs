//! Renders the Yana logos into a macOS `.iconset` folder (see scripts/bundle-macos.sh).
//! Usage: cargo run --example iconset -- <out.iconset>

use resvg::{tiny_skia, usvg};

const LOGO: &str = include_str!("../assets/icons/yana-logo.svg");
const LOGO_SMALL: &str = include_str!("../assets/icons/yana-logo-small.svg");

fn main() {
    let out = std::path::PathBuf::from(std::env::args().nth(1).expect("usage: iconset <out.iconset>"));
    std::fs::create_dir_all(&out).unwrap();
    for (points, scale) in [16, 32, 128, 256, 512].into_iter().flat_map(|p| [(p, 1), (p, 2)]) {
        let px = points * scale;
        // the small logo is drawn for tiny sizes (no dots, thicker strokes)
        let svg = if px <= 32 { LOGO_SMALL } else { LOGO };
        let name = if scale == 1 { format!("icon_{points}x{points}.png") } else { format!("icon_{points}x{points}@2x.png") };
        render(svg, px).save_png(out.join(name)).unwrap();
    }
}

/// Logo inset on a transparent canvas following Apple's icon grid (824 of 1024).
fn render(svg: &str, px: u32) -> tiny_skia::Pixmap {
    let tree = usvg::Tree::from_str(svg, &usvg::Options::default()).unwrap();
    let mut pixmap = tiny_skia::Pixmap::new(px, px).unwrap();
    let body = px as f32 * 824.0 / 1024.0;
    let offset = (px as f32 - body) / 2.0;
    let scale = body / tree.size().width();
    let transform = tiny_skia::Transform::from_scale(scale, scale).post_translate(offset, offset);
    resvg::render(&tree, transform, &mut pixmap.as_mut());
    pixmap
}
