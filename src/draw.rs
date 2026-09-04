//! What gets drawn over a capture — the grid, the crosshair, button rectangles, the inset.
//!
//! ## Why the font is built in
//!
//! An unlabelled rectangle is no use for checking anything ("which box was that?"). But
//! drawing text needs a font rasteriser, and adding a crate brings a font file or a few
//! hundred kilobytes along with it — against this tool's premise that copying one exe is all
//! there is to it.
//!
//! So a 5x7 bitmap font lives here as a 320-byte constant. Uppercase only (lowercase folds up),
//! and anything outside ASCII is drawn as an **empty box glyph** — non-Latin text in a `note`
//! does not appear in the picture, but seeing that it did not appear beats it vanishing
//! silently. Checking that needs the real text is what `/editor` is for.

use image::{Rgba, RgbaImage};

pub type Color = [u8; 4];

pub const WHITE: Color = [255, 255, 255, 255];
pub const BLACK: Color = [0, 0, 0, 255];
/// An ordinary button — cyan. Operator panels are mostly grey and black, so it stands out.
pub const TARGET: Color = [0, 200, 255, 255];
/// A `confirm: true` button — red, so the eye goes to the dangerous ones first.
pub const DANGER: Color = [255, 60, 60, 255];
/// A capture region — yellow.
pub const REGION: Color = [255, 210, 0, 255];
/// The two grid colours. **Alternated to make a dashed line** — a single translucent colour
/// disappears entirely against one background or the other. Operator panels are often close to
/// black, so a black-only grid vanishes on exactly the screens that need it (the same reason
/// the crosshair is drawn as a double line).
pub const GRID_DARK: Color = [0, 0, 0, 120];
pub const GRID_LIGHT: Color = [255, 255, 255, 120];

/// Plot one alpha-blended pixel. Out of range is silently ignored — a rectangle straddling the
/// edge of the image is no reason for drawing to fail.
fn put(img: &mut RgbaImage, x: i32, y: i32, c: Color) {
    if x < 0 || y < 0 || x >= img.width() as i32 || y >= img.height() as i32 {
        return;
    }
    let (x, y) = (x as u32, y as u32);
    if c[3] == 255 {
        img.put_pixel(x, y, Rgba(c));
        return;
    }
    let a = c[3] as u32;
    let d = img.get_pixel(x, y).0;
    let mix = |s: u8, d: u8| ((s as u32 * a + d as u32 * (255 - a)) / 255) as u8;
    img.put_pixel(x, y, Rgba([mix(c[0], d[0]), mix(c[1], d[1]), mix(c[2], d[2]), 255]));
}

pub fn fill(img: &mut RgbaImage, x: i32, y: i32, w: i32, h: i32, c: Color) {
    for yy in y..y + h {
        for xx in x..x + w {
            put(img, xx, yy, c);
        }
    }
}

/// A rectangle outline. `t` is the thickness, drawn inwards.
pub fn rect_outline(img: &mut RgbaImage, x: i32, y: i32, w: i32, h: i32, t: i32, c: Color) {
    if w <= 0 || h <= 0 {
        return;
    }
    for i in 0..t {
        fill(img, x + i, y + i, w - 2 * i, 1, c);
        fill(img, x + i, y + h - 1 - i, w - 2 * i, 1, c);
        fill(img, x + i, y + i, 1, h - 2 * i, c);
        fill(img, x + w - 1 - i, y + i, 1, h - 2 * i, c);
    }
}

// ────────────────────────────── font ──────────────────────────────

pub const GLYPH_W: i32 = 5;
pub const GLYPH_H: i32 = 7;

/// 5x7 glyphs for ASCII 0x20 (space) through 0x5F ('_'). Column-major, with bit 0 of each
/// byte being the top row.
#[rustfmt::skip]
const FONT: [[u8; 5]; 64] = [
    [0x00,0x00,0x00,0x00,0x00], // space
    [0x00,0x00,0x5F,0x00,0x00], // !
    [0x00,0x07,0x00,0x07,0x00], // "
    [0x14,0x7F,0x14,0x7F,0x14], // #
    [0x24,0x2A,0x7F,0x2A,0x12], // $
    [0x23,0x13,0x08,0x64,0x62], // %
    [0x36,0x49,0x55,0x22,0x50], // &
    [0x00,0x05,0x03,0x00,0x00], // '
    [0x00,0x1C,0x22,0x41,0x00], // (
    [0x00,0x41,0x22,0x1C,0x00], // )
    [0x14,0x08,0x3E,0x08,0x14], // *
    [0x08,0x08,0x3E,0x08,0x08], // +
    [0x00,0x50,0x30,0x00,0x00], // ,
    [0x08,0x08,0x08,0x08,0x08], // -
    [0x00,0x60,0x60,0x00,0x00], // .
    [0x20,0x10,0x08,0x04,0x02], // /
    [0x3E,0x51,0x49,0x45,0x3E], // 0
    [0x00,0x42,0x7F,0x40,0x00], // 1
    [0x42,0x61,0x51,0x49,0x46], // 2
    [0x21,0x41,0x45,0x4B,0x31], // 3
    [0x18,0x14,0x12,0x7F,0x10], // 4
    [0x27,0x45,0x45,0x45,0x39], // 5
    [0x3C,0x4A,0x49,0x49,0x30], // 6
    [0x01,0x71,0x09,0x05,0x03], // 7
    [0x36,0x49,0x49,0x49,0x36], // 8
    [0x06,0x49,0x49,0x29,0x1E], // 9
    [0x00,0x36,0x36,0x00,0x00], // :
    [0x00,0x56,0x36,0x00,0x00], // ;
    [0x00,0x08,0x14,0x22,0x41], // <
    [0x14,0x14,0x14,0x14,0x14], // =
    [0x41,0x22,0x14,0x08,0x00], // >
    [0x02,0x01,0x51,0x09,0x06], // ?
    [0x32,0x49,0x79,0x41,0x3E], // @
    [0x7E,0x11,0x11,0x11,0x7E], // A
    [0x7F,0x49,0x49,0x49,0x36], // B
    [0x3E,0x41,0x41,0x41,0x22], // C
    [0x7F,0x41,0x41,0x22,0x1C], // D
    [0x7F,0x49,0x49,0x49,0x41], // E
    [0x7F,0x09,0x09,0x01,0x01], // F
    [0x3E,0x41,0x41,0x51,0x32], // G
    [0x7F,0x08,0x08,0x08,0x7F], // H
    [0x00,0x41,0x7F,0x41,0x00], // I
    [0x20,0x40,0x41,0x3F,0x01], // J
    [0x7F,0x08,0x14,0x22,0x41], // K
    [0x7F,0x40,0x40,0x40,0x40], // L
    [0x7F,0x02,0x04,0x02,0x7F], // M
    [0x7F,0x04,0x08,0x10,0x7F], // N
    [0x3E,0x41,0x41,0x41,0x3E], // O
    [0x7F,0x09,0x09,0x09,0x06], // P
    [0x3E,0x41,0x51,0x21,0x5E], // Q
    [0x7F,0x09,0x19,0x29,0x46], // R
    [0x46,0x49,0x49,0x49,0x31], // S
    [0x01,0x01,0x7F,0x01,0x01], // T
    [0x3F,0x40,0x40,0x40,0x3F], // U
    [0x1F,0x20,0x40,0x20,0x1F], // V
    [0x7F,0x20,0x18,0x20,0x7F], // W
    [0x63,0x14,0x08,0x14,0x63], // X
    [0x03,0x04,0x78,0x04,0x03], // Y
    [0x61,0x51,0x49,0x45,0x43], // Z
    [0x00,0x00,0x7F,0x41,0x41], // [
    [0x02,0x04,0x08,0x10,0x20], // backslash
    [0x41,0x41,0x7F,0x00,0x00], // ]
    [0x04,0x02,0x01,0x02,0x04], // ^
    [0x40,0x40,0x40,0x40,0x40], // _
];

/// Characters not in the table — an empty box. Non-Latin text in a `note` ends up here.
const MISSING: [u8; 5] = [0x7F, 0x41, 0x41, 0x41, 0x7F];

fn glyph(c: char) -> [u8; 5] {
    let u = c.to_ascii_uppercase() as u32;
    if (0x20..=0x5F).contains(&u) {
        FONT[(u - 0x20) as usize]
    } else {
        MISSING
    }
}

/// The pixel width once drawn, excluding the gap after the last glyph.
pub fn text_width(s: &str, scale: i32) -> i32 {
    let n = s.chars().count() as i32;
    if n == 0 { 0 } else { (n * (GLYPH_W + 1) - 1) * scale }
}

/// Draw text. Given a `bg`, a background box with 2px padding goes down first — operator
/// panels use every background colour there is, and unbacked text vanishes on some of them.
pub fn text(img: &mut RgbaImage, x: i32, y: i32, s: &str, scale: i32, fg: Color, bg: Option<Color>) {
    let scale = scale.max(1);
    if let Some(b) = bg {
        fill(img, x - 2, y - 2, text_width(s, scale) + 4, GLYPH_H * scale + 4, b);
    }
    let mut cx = x;
    for ch in s.chars() {
        let g = glyph(ch);
        for (col, bits) in g.iter().enumerate() {
            for row in 0..GLYPH_H {
                if bits & (1 << row) != 0 {
                    fill(img, cx + col as i32 * scale, y + row * scale, scale, scale, fg);
                }
            }
        }
        cx += (GLYPH_W + 1) * scale;
    }
}

// ────────────────────────── overlay elements ──────────────────────────

/// The coordinate grid. `step` is in **source client coordinates** and is mapped into image
/// coordinates by `scale`. The labels carry the source values, so a number read off the picture
/// can be written straight into the profile file — true even in a shrunken capture.
pub fn grid(img: &mut RgbaImage, origin: (i32, i32), step: i32, scale: f64) {
    if step <= 0 {
        return;
    }
    let (ox, oy) = origin;
    let (w, h) = (img.width() as i32, img.height() as i32);

    // the first tick in source coordinates (the origin need not be a multiple of step)
    let first_x = ((ox + step - 1) / step) * step;
    let first_y = ((oy + step - 1) / step) * step;

    let mut gx = first_x;
    while ((gx - ox) as f64 * scale) < w as f64 {
        let px = ((gx - ox) as f64 * scale).round() as i32;
        // every fifth line heavier, so the eye can count positions
        let strong = (gx / step) % 5 == 0;
        dashed_v(img, px, h, strong);
        if strong {
            text(img, px + 2, 2, &gx.to_string(), 1, WHITE, Some([0, 0, 0, 200]));
        }
        gx += step;
    }
    let mut gy = first_y;
    while ((gy - oy) as f64 * scale) < h as f64 {
        let py = ((gy - oy) as f64 * scale).round() as i32;
        let strong = (gy / step) % 5 == 0;
        dashed_h(img, py, w, strong);
        if strong {
            text(img, 2, py + 2, &gy.to_string(), 1, WHITE, Some([0, 0, 0, 200]));
        }
        gy += step;
    }
}

/// A dashed line alternating light and dark every 4px. Against any background, half of it
/// is visible.
fn dash_color(i: i32, strong: bool) -> Color {
    let mut c = if (i / 4) % 2 == 0 { GRID_DARK } else { GRID_LIGHT };
    if strong {
        c[3] = 200;
    }
    c
}

fn dashed_v(img: &mut RgbaImage, x: i32, h: i32, strong: bool) {
    for y in 0..h {
        put(img, x, y, dash_color(y, strong));
    }
}

fn dashed_h(img: &mut RgbaImage, y: i32, w: i32, strong: bool) {
    for x in 0..w {
        put(img, x, y, dash_color(x, strong));
    }
}

/// The crosshair — **a triple band: 5px black, 3px white, 1px red.**
///
/// It started as a double line (white and red) until a measurement showed the white band
/// disappearing completely against a light background, leaving only the 1px red. The black
/// band was added then. Against any background at least one layer has contrast.
pub fn crosshair(img: &mut RgbaImage, x: i32, y: i32, arm: i32) {
    // If the crosshair covers the very pixel whose colour you came to check, the whole feature
    // is pointless. Painting over it with alpha 0 does not work (blending does nothing, so the
    // red already laid down stays) — the original colour is taken first and restored last.
    let keep = if x >= 0 && y >= 0 && x < img.width() as i32 && y < img.height() as i32 {
        Some(*img.get_pixel(x as u32, y as u32))
    } else {
        None
    };
    for (t, c) in [(2, BLACK), (1, WHITE), (0, DANGER)] {
        fill(img, x - arm, y - t, arm * 2 + 1, t * 2 + 1, c);
        fill(img, x - t, y - arm, t * 2 + 1, arm * 2 + 1, c);
    }
    if let Some(p) = keep {
        img.put_pixel(x as u32, y as u32, p);
    }
    rect_outline(img, x - 5, y - 5, 11, 11, 1, BLACK);
    rect_outline(img, x - 4, y - 4, 9, 9, 1, WHITE);
}

/// Shrink the radius and factor until the inset fits inside the source image.
///
/// Without this calculation, a small image means the inset is **silently not drawn** — while
/// the response metadata says there is one. Measured: a 700x200 crop asked for radius 30 at 4x,
/// which needs a 244px inset, which does not fit in a height of 200, and nothing happened.
/// The factor comes down first, then the radius. Below 2x there is no reason to magnify at all,
/// so that returns `None` — and the response says so.
pub fn fit_inset(img_w: i32, img_h: i32, radius: i32, wanted: i32) -> Option<(i32, i32)> {
    let avail = img_w.min(img_h) - 8;
    if avail < 24 {
        return None;
    }
    for factor in (2..=wanted.clamp(2, 8)).rev() {
        let allowed_radius = (avail / factor - 1) / 2;
        if allowed_radius >= 8 {
            return Some((radius.min(allowed_radius), factor));
        }
    }
    None
}

/// An overlay rectangle. The label is placed **separately** — it has to go somewhere that does
/// not overlap a neighbour, and drawing it together with the rectangle leaves no way to choose.
pub fn marked_rect(img: &mut RgbaImage, x: i32, y: i32, w: i32, h: i32, c: Color) {
    rect_outline(img, x, y, w, h, 2, c);
    // translucent fill, so what the rectangle covers is visible at a glance
    fill(img, x + 2, y + 2, w - 4, h - 4, [c[0], c[1], c[2], 40]);
}

/// The box `text(.., Some(bg))` actually occupies — the unit of the overlap test.
///
/// It has to use **the same numbers** `text` uses to lay the background down, from
/// `(x-2, y-2)` across `width+4 x GLYPH_H+4`. Diverge here and it judges two labels not to
/// overlap and then draws them on top of each other.
pub fn label_box(x: i32, y: i32, s: &str) -> [i32; 4] {
    [x - 2, y - 2, text_width(s, 1) + 4, GLYPH_H + 4]
}

pub fn boxes_overlap(a: [i32; 4], b: [i32; 4]) -> bool {
    a[0] < b[0] + b[2] && b[0] < a[0] + a[2] && a[1] < b[1] + b[3] && b[1] < a[1] + a[3]
}

/// `radius` pixels around `(cx, cy)`, enlarged `factor` times.
/// Nearest-neighbour — the pixel boundaries have to stay sharp for "is this crosshair in the
/// middle of the button or at its edge" to be answerable.
pub fn inset(img: &RgbaImage, cx: i32, cy: i32, radius: i32, factor: i32) -> RgbaImage {
    let radius = radius.max(4);
    let factor = factor.clamp(1, 8);
    let side = radius * 2 + 1;
    let mut out = RgbaImage::from_pixel((side * factor) as u32, (side * factor) as u32, Rgba([24, 24, 24, 255]));
    for dy in 0..side {
        for dx in 0..side {
            let (sx, sy) = (cx - radius + dx, cy - radius + dy);
            if sx < 0 || sy < 0 || sx >= img.width() as i32 || sy >= img.height() as i32 {
                continue;
            }
            let p = *img.get_pixel(sx as u32, sy as u32);
            for fy in 0..factor {
                for fx in 0..factor {
                    out.put_pixel((dx * factor + fx) as u32, (dy * factor + fy) as u32, p);
                }
            }
        }
    }
    // The marker inside the inset has to be **a shape that does not cover anything**. Drawing
    // the crosshair again would put it over the exact spot the magnification was for — and the
    // purpose is to see whether that pixel is in the middle of the button or at its edge, so
    // that would defeat it entirely. Instead: (1) an outline around the one magnified source
    // pixel at the centre, and (2) short ticks pointing inwards from the four edges.
    let b = radius * factor; // top-left of the centre source pixel's block
    rect_outline(&mut out, b - 2, b - 2, factor + 4, factor + 4, 1, BLACK);
    rect_outline(&mut out, b - 1, b - 1, factor + 2, factor + 2, 1, DANGER);

    let side = side * factor;
    let tick = (side / 8).max(4);
    let mid = b + factor / 2;
    for (x, y, w, h) in [
        (0, mid - 1, tick, 3),                // left
        (side - tick, mid - 1, tick, 3),      // right
        (mid - 1, 0, 3, tick),                // top
        (mid - 1, side - tick, 3, tick),      // bottom
    ] {
        fill(&mut out, x, y, w, h, BLACK);
        fill(&mut out, x + 1, y + 1, (w - 2).max(1), (h - 2).max(1), DANGER);
    }
    out
}

/// Paste the inset into a corner of the source — **the corner furthest from the mark**. An
/// inset covering the very point being checked would be pointless.
pub fn paste_inset(img: &mut RgbaImage, ins: &RgbaImage, mark: (i32, i32)) {
    let (iw, ih) = (ins.width() as i32, ins.height() as i32);
    let (w, h) = (img.width() as i32, img.height() as i32);
    if iw + 8 > w || ih + 8 > h {
        return; // a source smaller than the inset has nowhere to paste it
    }
    let left = mark.0 > w / 2;
    let top = mark.1 > h / 2;
    let x = if left { 4 } else { w - iw - 4 };
    let y = if top { 4 } else { h - ih - 4 };
    for dy in 0..ih {
        for dx in 0..iw {
            put(img, x + dx, y + dy, ins.get_pixel(dx as u32, dy as u32).0);
        }
    }
    rect_outline(img, x - 2, y - 2, iw + 4, ih + 4, 2, WHITE);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn img(w: u32, h: u32) -> RgbaImage {
        RgbaImage::from_pixel(w, h, Rgba([0, 0, 0, 255]))
    }

    #[test]
    fn out_of_bounds_drawing_is_ignored_not_panicking() {
        // A rectangle straddling the edge is a normal situation — drawing must not die
        let mut i = img(20, 20);
        rect_outline(&mut i, -50, -50, 200, 200, 3, WHITE);
        marked_rect(&mut i, 18, 18, 40, 40, TARGET);
        text(&mut i, 20, 11, "EDGE", 1, BLACK, Some(TARGET));
        crosshair(&mut i, 0, 0, 30);
        text(&mut i, -10, -10, "OFFSCREEN", 2, WHITE, Some(BLACK));
    }

    #[test]
    fn label_box_matches_what_text_actually_paints() {
        // Diverge here and it judges two labels not to overlap and draws them on top of each
        // other — and overlapping labels read as one name, so even the fact that it is wrong
        // does not show in the picture.
        let b = label_box(10, 20, "AB");
        assert_eq!(b, [8, 18, text_width("AB", 1) + 4, GLYPH_H + 4]);

        // Check against the range actually painted, in pixels.
        let mut i = img(60, 40);
        text(&mut i, 10, 20, "AB", 1, WHITE, Some(TARGET));
        assert_eq!(i.get_pixel(b[0] as u32, b[1] as u32).0, TARGET, "top-left corner");
        assert_eq!(
            i.get_pixel((b[0] + b[2] - 1) as u32, (b[1] + b[3] - 1) as u32).0,
            TARGET,
            "bottom-right corner"
        );
        assert_ne!(i.get_pixel((b[0] - 1) as u32, b[1] as u32).0, TARGET, "outside the box");
    }

    #[test]
    fn overlap_is_exclusive_at_the_edges() {
        // Two boxes exactly touching do not overlap. One pixel wrong here and, in a crowded
        // area, every placeable spot is judged unavailable.
        assert!(!boxes_overlap([0, 0, 10, 10], [10, 0, 10, 10]));
        assert!(!boxes_overlap([0, 0, 10, 10], [0, 10, 10, 10]));
        assert!(boxes_overlap([0, 0, 10, 10], [9, 9, 10, 10]));
        assert!(boxes_overlap([0, 0, 10, 10], [-5, -5, 10, 10]));
    }

    #[test]
    fn text_renders_something_visible() {
        let mut i = img(60, 20);
        text(&mut i, 2, 2, "AB", 1, WHITE, None);
        let lit = i.pixels().filter(|p| p.0 == [255, 255, 255, 255]).count();
        assert!(lit > 10, "glyphs should light up pixels, got {lit}");
    }

    #[test]
    fn unknown_characters_render_as_a_visible_box() {
        // Non-Latin text cannot be drawn — that has to be visible, not silently missing
        assert_eq!(glyph('\u{ac00}'), MISSING); // a Hangul syllable, outside the font
        assert_ne!(glyph('A'), MISSING);
        // lowercase folds to uppercase
        assert_eq!(glyph('a'), glyph('A'));
    }

    #[test]
    fn text_width_matches_what_gets_drawn() {
        assert_eq!(text_width("", 1), 0);
        assert_eq!(text_width("A", 1), 5);
        assert_eq!(text_width("AB", 1), 11);
        assert_eq!(text_width("AB", 2), 22);
    }

    #[test]
    fn grid_lines_land_on_scaled_source_coordinates() {
        // Halved, the ticks still have to be in source coordinates — a number read off the
        // picture has to be writable straight into the profile file.
        // The source tick at 250 (the fifth, so a heavy line) has to land at image x=125.
        let mut i = img(200, 200);
        grid(&mut i, (0, 0), 50, 0.5);
        // Horizontal lines cross the full width, so "is any pixel painted" cannot separate
        // them — a column with a vertical line in it has overwhelmingly more painted pixels.
        let lit = |x: u32| (0..200).filter(|&y| i.get_pixel(x, y).0 != [0, 0, 0, 255]).count();
        assert!(
            lit(125) > lit(130) * 3,
            "vertical grid line expected at image x=125 (source 250): {} vs {}",
            lit(125),
            lit(130)
        );
    }

    #[test]
    fn grid_is_visible_on_both_dark_and_light_backgrounds() {
        // Operator panels are often black, where a single translucent colour disappears.
        for bg in [[0u8, 0, 0, 255], [255, 255, 255, 255]] {
            let mut i = RgbaImage::from_pixel(60, 60, Rgba(bg));
            grid(&mut i, (0, 0), 20, 1.0);
            let changed = (0..60).any(|y| i.get_pixel(20, y).0 != bg);
            assert!(changed, "grid must be visible on background {bg:?}");
        }
    }

    #[test]
    fn inset_shrinks_to_fit_instead_of_silently_vanishing() {
        // Measured regression: a 700x200 crop at radius 30 and 4x needs 244px, which does not
        // fit the height, and the old code drew nothing and said nothing.
        let (r, f) = fit_inset(700, 200, 30, 4).expect("should fit after shrinking");
        assert!((2 * r + 1) * f <= 200 - 8, "inset {}px must fit in 192px", (2 * r + 1) * f);
        assert!(f >= 2, "magnification below 2x is pointless");
        assert!(r <= 30);

        // with room to spare, exactly as asked
        assert_eq!(fit_inset(1000, 1000, 30, 4), Some((30, 4)));
        // A size that cannot fit at all gives None, so the caller can say "not drawn"
        assert_eq!(fit_inset(60, 20, 30, 4), None);
    }

    #[test]
    fn crosshair_is_visible_on_both_dark_and_light_backgrounds() {
        for bg in [[0u8, 0, 0, 255], [255, 255, 255, 255]] {
            let mut i = RgbaImage::from_pixel(60, 60, Rgba(bg));
            crosshair(&mut i, 30, 30, 20);
            // part-way along an arm there have to be several layers differing from the background
            let lit = (20..40).filter(|&y| i.get_pixel(30 - 15, y).0 != bg).count();
            assert!(lit >= 3, "crosshair arm needs contrast on {bg:?}, got {lit}");
        }
    }

    #[test]
    fn crosshair_leaves_the_marked_pixel_uncovered() {
        // the very pixel being checked must not be covered
        let mut i = img(40, 40);
        i.put_pixel(20, 20, Rgba([7, 8, 9, 255]));
        crosshair(&mut i, 20, 20, 10);
        assert_eq!(i.get_pixel(20, 20).0, [7, 8, 9, 255]);
    }

    #[test]
    fn inset_magnifies_by_the_requested_factor() {
        let src = img(100, 100);
        let out = inset(&src, 50, 50, 10, 3);
        assert_eq!(out.width(), 21 * 3);
        assert_eq!(out.height(), 21 * 3);
    }

    #[test]
    fn inset_goes_to_the_corner_away_from_the_mark() {
        // a mark at the top-left puts the inset bottom-right — it must not cover the point
        let mut i = img(200, 200);
        let ins = RgbaImage::from_pixel(40, 40, Rgba([9, 9, 9, 255]));
        paste_inset(&mut i, &ins, (10, 10));
        assert_eq!(i.get_pixel(180, 180).0, [9, 9, 9, 255]);
        assert_eq!(i.get_pixel(20, 20).0, [0, 0, 0, 255]);
    }
}
