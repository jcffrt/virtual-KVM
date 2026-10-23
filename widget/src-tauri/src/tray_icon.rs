//! Tray icon for Windows. The macOS menu bar shows the active agent's number
//! as text next to a template icon (`set_title`); the Windows notification
//! area has no text and no template icons, so the number is drawn into the
//! icon: a blue tile (readable on light and dark taskbars) with the white
//! glyph of the menu bar icon, or with the number when an agent is active.

/// Side of the generated icon, in pixels (Windows scales it).
pub const SIZE: usize = 32;

const TILE: [u8; 4] = [0x1f, 0x6f, 0xeb, 0xff];
const INK: [u8; 4] = [0xff, 0xff, 0xff, 0xff];
const RADIUS: usize = 6;

/// 5×7 digits, one row per byte, bit 4 = leftmost column.
const DIGITS: [[u8; 7]; 10] = [
    [0x0E, 0x11, 0x13, 0x15, 0x19, 0x11, 0x0E],
    [0x04, 0x0C, 0x04, 0x04, 0x04, 0x04, 0x0E],
    [0x0E, 0x11, 0x01, 0x02, 0x04, 0x08, 0x1F],
    [0x1F, 0x02, 0x04, 0x02, 0x01, 0x11, 0x0E],
    [0x02, 0x06, 0x0A, 0x12, 0x1F, 0x02, 0x02],
    [0x1F, 0x10, 0x1E, 0x01, 0x01, 0x11, 0x0E],
    [0x06, 0x08, 0x10, 0x1E, 0x11, 0x11, 0x0E],
    [0x1F, 0x01, 0x02, 0x04, 0x08, 0x08, 0x08],
    [0x0E, 0x11, 0x11, 0x0E, 0x11, 0x11, 0x0E],
    [0x0E, 0x11, 0x11, 0x0F, 0x01, 0x02, 0x0C],
];

/// RGBA pixels of the icon: `glyph` (RGBA, `glyph_w`×`glyph_h`, only its
/// alpha is used) when `digit` is `None`, else the digit.
pub fn render(glyph: &[u8], glyph_w: usize, glyph_h: usize, digit: Option<u8>) -> Vec<u8> {
    let mut px = vec![0u8; SIZE * SIZE * 4];
    let mut put = |x: usize, y: usize, c: [u8; 4]| {
        let i = (y * SIZE + x) * 4;
        px[i..i + 4].copy_from_slice(&c);
    };
    for y in 0..SIZE {
        for x in 0..SIZE {
            if in_rounded_square(x, y) {
                put(x, y, TILE);
            }
        }
    }
    match digit.filter(|d| *d < 10) {
        Some(d) => {
            // 5×7 scaled ×3: 15×21, centred.
            const SCALE: usize = 3;
            let (x0, y0) = ((SIZE - 5 * SCALE) / 2, (SIZE - 7 * SCALE) / 2);
            for (row, bits) in DIGITS[d as usize].iter().enumerate() {
                for col in 0..5 {
                    if bits & (0x10 >> col) == 0 {
                        continue;
                    }
                    for dy in 0..SCALE {
                        for dx in 0..SCALE {
                            put(x0 + col * SCALE + dx, y0 + row * SCALE + dy, INK);
                        }
                    }
                }
            }
        }
        None if glyph_w > 0 && glyph_h > 0 => {
            // The menu bar glyph, nearest-neighbour into the inner 24×24.
            const INNER: usize = 24;
            let off = (SIZE - INNER) / 2;
            for y in 0..INNER {
                for x in 0..INNER {
                    let (gx, gy) = (x * glyph_w / INNER, y * glyph_h / INNER);
                    let a = glyph.get((gy * glyph_w + gx) * 4 + 3).copied().unwrap_or(0);
                    if a >= 0x80 {
                        put(off + x, off + y, INK);
                    }
                }
            }
        }
        None => {}
    }
    px
}

fn in_rounded_square(x: usize, y: usize) -> bool {
    let r = RADIUS;
    let near = |v: usize| {
        if v < r {
            Some(r - v)
        } else if v >= SIZE - r {
            Some(v + 1 - (SIZE - r))
        } else {
            None
        }
    };
    match (near(x), near(y)) {
        (Some(dx), Some(dy)) => dx * dx + dy * dy <= r * r,
        _ => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(px: &[u8], x: usize, y: usize) -> [u8; 4] {
        let i = (y * SIZE + x) * 4;
        px[i..i + 4].try_into().unwrap()
    }

    #[test]
    fn tile_digit_and_glyph() {
        let px = render(&[], 0, 0, Some(1));
        assert_eq!(px.len(), SIZE * SIZE * 4);
        assert_eq!(at(&px, 0, 0)[3], 0, "rounded corner is transparent");
        assert_eq!(at(&px, 2, 16), TILE);
        // The stem of the "1": column 2 of the font, scaled ×3 from x0 = 8.
        assert_eq!(at(&px, 8 + 2 * 3 + 1, 16), INK);
        assert_ne!(render(&[], 0, 0, Some(2)), px);

        // A fully opaque 4×4 glyph fills the inner square.
        let glyph = [0u8, 0, 0, 0xff].repeat(16);
        let g = render(&glyph, 4, 4, None);
        assert_eq!(at(&g, 16, 16), INK);
        assert_eq!(at(&g, 2, 16), TILE);
    }
}
