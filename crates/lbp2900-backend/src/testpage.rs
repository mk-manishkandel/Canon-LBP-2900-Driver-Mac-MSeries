//! Built-in test page, printed by `lbp2900 diag testpage` without going through CUPS.

use capt::halftone::{Halftoner, Method};
use capt::page::Paper;

/// 5×7 glyphs for the few characters the test page uses.
fn glyph(c: char) -> [u8; 7] {
    match c {
        'L' => [0x10, 0x10, 0x10, 0x10, 0x10, 0x10, 0x1F],
        'B' => [0x1E, 0x11, 0x11, 0x1E, 0x11, 0x11, 0x1E],
        'P' => [0x1E, 0x11, 0x11, 0x1E, 0x10, 0x10, 0x10],
        'R' => [0x1E, 0x11, 0x11, 0x1E, 0x14, 0x12, 0x11],
        'U' => [0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x0E],
        'S' => [0x0F, 0x10, 0x10, 0x0E, 0x01, 0x01, 0x1E],
        'T' => [0x1F, 0x04, 0x04, 0x04, 0x04, 0x04, 0x04],
        'O' => [0x0E, 0x11, 0x11, 0x11, 0x11, 0x11, 0x0E],
        'K' => [0x11, 0x12, 0x14, 0x18, 0x14, 0x12, 0x11],
        '0' => [0x0E, 0x11, 0x13, 0x15, 0x19, 0x11, 0x0E],
        '2' => [0x0E, 0x11, 0x01, 0x02, 0x04, 0x08, 0x1F],
        '9' => [0x0E, 0x11, 0x11, 0x0F, 0x01, 0x02, 0x0C],
        _ => [0; 7],
    }
}

/// Render the test page as a 1-bit bitmap sized for `paper`'s printable area.
pub fn render(paper: &Paper) -> (Vec<u8>, usize, usize) {
    let w = paper.image_width_px() as usize;
    let h = paper.image_height_px() as usize;
    let lb = paper.image_line_bytes();
    let mut bitmap = vec![0u8; lb * h];
    let mut grey = vec![255u8; w];
    let mut ht = Halftoner::new(Method::Ordered, 0, w);
    let text = "LBP2900 RUST OK";
    let scale = 24; // 5x7 glyph → 120x168 px
    let (tx, ty) = (300usize, 300usize);

    for y in 0..h {
        for (x, g) in grey.iter_mut().enumerate() {
            let mut ink = false;
            // 1 mm border around the printable area.
            ink |= x < 24 || y < 24 || x >= w - 24 || y >= h - 24;
            // Diagonals of a centred square.
            let (cx, cy, half) = (w / 2, h / 2, w / 4);
            if x.abs_diff(cx) < half && y.abs_diff(cy) < half {
                let (dx, dy) = (x as i64 - (cx - half) as i64, y as i64 - (cy - half) as i64);
                ink |= (dx - dy).abs() < 6 || (dx + dy - 2 * half as i64).abs() < 6;
            }
            // Text.
            if y >= ty && y < ty + 7 * scale && x >= tx {
                let col = (x - tx) / scale;
                let (ch, cx) = (col / 6, col % 6);
                if let Some(c) = text.chars().nth(ch) {
                    let row = glyph(c)[(y - ty) / scale];
                    ink |= cx < 5 && row & (0x10 >> cx) != 0;
                }
            }
            *g = if ink { 0 } else { 255 };
            // Grey ramp along the bottom: 10 steps.
            if y > h - 700 && y < h - 300 && x > 300 && x < w - 300 && !ink {
                let step = (x - 300) * 10 / (w - 600);
                *g = 255 - (step * 255 / 9) as u8;
            }
        }
        ht.line(&grey, &mut bitmap[y * lb..(y + 1) * lb]);
    }
    (bitmap, lb, h)
}
