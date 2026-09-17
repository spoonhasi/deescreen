//! The contact sheet — one cropped picture per button, with its name under it.
//!
//! ## Why this exists next to the overlay
//!
//! `buttons=full` draws every rectangle onto the screen itself, which answers "are the
//! coordinates right". It cannot answer "is this the right **name**", because the name has to
//! be printed next to its key and there is no room: 60 labels on one operator panel could not
//! find free spots and were drawn on top of each other, which is exactly the situation where
//! you were trying to read them.
//!
//! So this takes the same rectangles and lays them out as a **list** instead of an overlay.
//! Each cell is one button, cropped from the same single capture, with its name underneath and
//! nothing competing for the space. The cost is that the panel's layout is gone — which is why
//! this does not replace the overlay, and why the cells are in screen order by default, so the
//! sheet can still be read alongside the real panel a row at a time.
//!
//! Nothing here touches Windows. It takes a captured image and a list of rectangles, so every
//! decision in it is testable on any machine.

use image::{Rgba, RgbaImage, imageops};

use crate::draw;
use crate::targets::Rect;

/// The sheet's own background. Darker than any operator panel, so a crop's edge is visible
/// even where the key's own background is nearly black.
const BG: Rgba<u8> = Rgba([20, 24, 30, 255]);
/// A cell whose rectangle is not on the screen — drawn, not omitted.
const ABSENT: draw::Color = [120, 128, 140, 255];

/// Space between a cell's content and the cell's edge.
const GUTTER: i32 = 6;
/// Between the picture and the name under it.
const CAPTION_GAP: i32 = 4;

/// How large a cell's picture may get, before `scale`. A whole-panel "button" would otherwise
/// set the cell size for all 140 of them.
pub const DEFAULT_CELL: i32 = 120;
/// The sheet is laid out to fit this width, and the column count follows from it.
///
/// A constant rather than a parameter, along with the column count and the caption's glyph
/// scale. All three were once asked for in the query string, and the manual had nothing to say
/// about any of them beyond "when the default does not suit" - which is what a knob nobody
/// needs looks like from the outside. They are one edit away if a reason ever turns up.
const SHEET_WIDTH: i32 = 1600;
/// Glyph scale for the names under the cells.
const LABEL_SCALE: i32 = 1;
/// Same ceiling the ordinary captures use, for the same reason: a response cannot explode.
const MAX_SHEET_PIXELS: u64 = 64_000_000;

/// One button, already placed on the window it was captured from.
pub struct Cell {
    pub name: String,
    /// Where it is **now** — anchor offsets and coordinate scaling already applied.
    pub rect: Rect,
    /// Drawn in the danger colour, like the overlay does. Colour is identity here too: a
    /// confirm button has to look like one wherever it is shown.
    pub confirm: bool,
}

/// What the caller asked for. Every field is already clamped by the handler.
pub struct Options {
    /// Context pixels kept around each rectangle. A key pressed 16px off looks centred at
    /// `pad=0` and obviously off at `pad=8`, so the margin is part of what is being checked.
    pub pad: i32,
    /// The ceiling on a cell's picture, per side.
    pub cell: i32,
    /// Magnify (or shrink) each crop. Small softkeys are 32px and their legends want 2x.
    pub scale: f64,
    /// Drawn across the top. The sheet outlives the request that made it, so it says what it
    /// is a sheet of.
    pub heading: String,
}

/// The finished sheet.
pub struct Sheet {
    pub image: RgbaImage,
    pub cols: i32,
    pub rows: i32,
    /// `[w, h]` of one cell, for a caller that wants to point at a cell by number.
    pub cell: [i32; 2],
    /// Buttons whose rectangle has no pixels on this window. **Drawn as empty cells**, never
    /// skipped: a name silently missing from the sheet is the one nobody checks.
    pub absent: Vec<String>,
}

/// Lay the cells out and draw them.
///
/// `full` is the whole client area at 1:1 — every cell is cropped from this one capture, so
/// the pictures cannot disagree with each other about when they were taken.
pub fn build(full: &RgbaImage, cells: &[Cell], opt: &Options) -> Result<Sheet, String> {
    if cells.is_empty() {
        return Err("this profile has no buttons to lay out".to_string());
    }
    let label = LABEL_SCALE;
    let cap = opt.cell.max(8);
    let (fw, fh) = (full.width() as i32, full.height() as i32);

    // ── crop first, because the crops decide how big a cell has to be ──
    let mut crops: Vec<Option<RgbaImage>> = Vec::with_capacity(cells.len());
    let mut absent: Vec<String> = Vec::new();
    for c in cells {
        let [rx, ry, rw, rh] = crate::targets::Targets::pad_rect(c.rect, opt.pad);
        // Both edges are cut, not slid — the same rule as an ordinary capture. A rectangle
        // half off the window has to come back as the half that is on it, in its own place,
        // or the picture is of somewhere the caller did not ask about.
        let x0 = rx.max(0);
        let y0 = ry.max(0);
        let w = rx.saturating_add(rw.max(0)).min(fw) - x0;
        let h = ry.saturating_add(rh.max(0)).min(fh) - y0;
        if w <= 0 || h <= 0 || x0 >= fw || y0 >= fh {
            absent.push(c.name.clone());
            crops.push(None);
            continue;
        }
        let cut = imageops::crop_imm(full, x0 as u32, y0 as u32, w as u32, h as u32).to_image();
        // Shrink to the cap, or magnify if asked — never both fighting each other, so the cap
        // is applied to the size the caller actually asked for.
        let f = opt.scale.min(cap as f64 / w as f64).min(cap as f64 / h as f64);
        let (nw, nh) = (((w as f64 * f).round() as i32).max(1), ((h as f64 * f).round() as i32).max(1));
        crops.push(Some(if nw == w && nh == h {
            cut
        } else {
            let filter =
                if f > 1.0 { imageops::FilterType::Nearest } else { imageops::FilterType::Triangle };
            imageops::resize(&cut, nw as u32, nh as u32, filter)
        }));
    }

    // ── the grid ──
    // The content box is as large as the largest crop rather than the cap: a panel of 44px
    // keys should not be laid out in 120px cells full of background.
    let content_w = crops.iter().flatten().map(|i| i.width() as i32).max().unwrap_or(cap / 3).max(16);
    let content_h = crops.iter().flatten().map(|i| i.height() as i32).max().unwrap_or(cap / 3).max(16);
    // Names are not truncated. A shortened name is a name that might match the picture and
    // might not, which is the question the sheet was made to answer — so the cell widens
    // instead, and the longest name in the profile sets the column width.
    let widest = cells.iter().map(|c| draw::text_width(&c.name, label)).max().unwrap_or(0);

    let cell_w = content_w.max(widest) + GUTTER * 2;
    let cell_h = content_h + CAPTION_GAP + draw::GLYPH_H * label + GUTTER * 2;
    let n = cells.len() as i32;
    let cols = (SHEET_WIDTH.max(cell_w) / cell_w).clamp(1, n);
    let rows = (n + cols - 1) / cols;

    let head_h = draw::GLYPH_H * label + GUTTER * 2;
    // The heading says which profile and which window this is a sheet of, and a sheet of six
    // buttons is narrower than that sentence. Widen the canvas rather than let the one line
    // that identifies the picture run off the edge of it.
    let absent_note = if absent.is_empty() { 0 } else { draw::text_width("999 NOT ON SCREEN", label) + GUTTER };
    let head_w = draw::text_width(&opt.heading, label) + absent_note + GUTTER * 2;
    let (w, h) = ((cols * cell_w).max(head_w), head_h + rows * cell_h);
    let px = (w as u64) * (h as u64);
    if px > MAX_SHEET_PIXELS {
        return Err(format!(
            "a {cols}x{rows} sheet of {cell_w}x{cell_h} cells is {w}x{h} ({:.2} megapixels), past \
             the {} megapixel ceiling. Ask for fewer buttons, a smaller cell, or a lower scale.",
            px as f64 / 1_000_000.0,
            MAX_SHEET_PIXELS / 1_000_000
        ));
    }

    // ── draw ──
    let mut img = RgbaImage::from_pixel(w as u32, h as u32, BG);
    draw::text(&mut img, GUTTER, GUTTER, &opt.heading, label, draw::WHITE, None);
    if !absent.is_empty() {
        let s = format!("{} NOT ON SCREEN", absent.len());
        let x = w - GUTTER - draw::text_width(&s, label);
        draw::text(&mut img, x, GUTTER, &s, label, draw::DANGER, None);
    }

    for (i, c) in cells.iter().enumerate() {
        let i = i as i32;
        let cx = (i % cols) * cell_w;
        let cy = head_h + (i / cols) * cell_h;
        let colour = if c.confirm { draw::DANGER } else { draw::TARGET };
        let bx = cx + (cell_w - content_w) / 2;

        match &crops[i as usize] {
            Some(crop) => {
                let (iw, ih) = (crop.width() as i32, crop.height() as i32);
                let (px, py) = (bx + (content_w - iw) / 2, cy + GUTTER + (content_h - ih) / 2);
                imageops::replace(&mut img, crop, px as i64, py as i64);
                // Only the picture is outlined, not the whole content box, so the frame says
                // exactly how much of the window this cell is showing.
                draw::rect_outline(&mut img, px - 1, py - 1, iw + 2, ih + 2, 1, colour);
            }
            None => {
                // Empty, and unmistakably so. Hollow alone would read as a dark key.
                let (bw, bh) = (content_w, content_h);
                let by = cy + GUTTER;
                draw::rect_outline(&mut img, bx, by, bw, bh, 1, ABSENT);
                cross(&mut img, bx, by, bw, bh, ABSENT);
            }
        }

        let ty = cy + GUTTER + content_h + CAPTION_GAP;
        let tx = cx + (cell_w - draw::text_width(&c.name, label)) / 2;
        let fg = if crops[i as usize].is_none() { ABSENT } else { colour };
        draw::text(&mut img, tx, ty, &c.name, label, fg, None);
    }

    Ok(Sheet { image: img, cols, rows, cell: [cell_w, cell_h], absent })
}

/// Two diagonals across a box. Only used to say "there is no picture here" — a shape, because
/// a colour would have to compete with whatever the crops happen to be.
fn cross(img: &mut RgbaImage, x: i32, y: i32, w: i32, h: i32, c: draw::Color) {
    if w <= 1 || h <= 1 {
        return;
    }
    let steps = w.max(h);
    for i in 0..=steps {
        let dx = x + i * (w - 1) / steps;
        let dy = y + i * (h - 1) / steps;
        draw::fill(img, dx, dy, 1, 1, c);
        draw::fill(img, x + w - 1 - (dx - x), dy, 1, 1, c);
    }
}

/// Cells in **reading order on the panel** — top to bottom, left to right.
///
/// `tol` is how far below a row's first rectangle another one may start and still belong to
/// that row. Keys in a row are rarely aligned to the pixel, and sorting on `y` alone turns one
/// row into a staircase — which is the one thing that would stop the sheet being readable next
/// to the real panel.
///
/// Rows are grown from the topmost rectangle rather than cut at fixed multiples of `tol`.
/// Fixed bands look equivalent and are not: two keys 2px apart that happen to straddle a band
/// boundary land in different rows, and which keys those are depends on the panel's distance
/// from the top of the window.
pub fn screen_order(cells: Vec<Cell>, tol: i32) -> Vec<Cell> {
    let tol = tol.max(1);
    let mut cells = cells;
    cells.sort_by_key(|c| (c.rect[1], c.rect[0]));

    let mut row = 0usize;
    let mut top: Option<i32> = None;
    let mut keyed: Vec<(usize, i32, i32, Cell)> = Vec::with_capacity(cells.len());
    for c in cells {
        match top {
            Some(t) if c.rect[1] - t > tol => {
                row += 1;
                top = Some(c.rect[1]);
            }
            None => top = Some(c.rect[1]),
            _ => {}
        }
        keyed.push((row, c.rect[0], c.rect[1], c));
    }
    keyed.sort_by_key(|(r, x, y, _)| (*r, *x, *y));
    keyed.into_iter().map(|(_, _, _, c)| c).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cell(name: &str, rect: Rect) -> Cell {
        Cell { name: name.to_string(), rect, confirm: false }
    }

    fn opts(heading: &str) -> Options {
        Options {
            pad: 0,
            cell: DEFAULT_CELL,
            scale: 1.0,
            heading: heading.to_string(),
        }
    }

    /// A row of keys is never aligned to the pixel. Sorting on `y` alone would read one row as
    /// a staircase and put the sheet's cells in an order that matches no panel.
    #[test]
    fn a_row_of_keys_stays_one_row_despite_a_few_pixels() {
        let cells = vec![
            cell("C", [200, 103, 40, 40]),
            cell("A", [100, 100, 40, 40]),
            cell("D", [100, 260, 40, 40]),
            cell("B", [150, 98, 40, 40]),
        ];
        let got: Vec<String> =
            screen_order(cells, 32).into_iter().map(|c| c.name).collect();
        assert_eq!(got, ["A", "B", "C", "D"], "reading order, not a staircase");
    }

    /// Rows grow from their own topmost key. Cutting at fixed multiples of the tolerance
    /// instead would split a row wherever it happened to straddle a band edge — and which row
    /// that is depends on nothing more meaningful than how far the panel sits from the top of
    /// the window.
    #[test]
    fn a_row_is_not_split_by_where_it_sits_on_the_window() {
        // Two keys 2px apart, placed so that a 32px band boundary falls between them.
        let cells = vec![cell("RIGHT", [200, 129, 40, 40]), cell("LEFT", [100, 127, 40, 40])];
        let got: Vec<String> =
            screen_order(cells, 32).into_iter().map(|c| c.name).collect();
        assert_eq!(got, ["LEFT", "RIGHT"], "one row, read left to right");
    }

    /// A rectangle that is not on this window gets a cell anyway. Leaving it out would make
    /// the sheet look complete while the one button worth checking is the one missing from it.
    #[test]
    fn a_button_off_the_window_still_gets_its_cell() {
        let full = RgbaImage::from_pixel(200, 200, Rgba([90, 90, 90, 255]));
        let cells = vec![cell("ON", [10, 10, 40, 40]), cell("GONE", [900, 900, 40, 40])];
        let out = build(&full, &cells, &opts("T")).expect("two cells");

        assert_eq!(out.absent, ["GONE"]);
        assert_eq!(out.cols, 2, "both cells are laid out, not one");
        assert_eq!(out.rows, 1);
        // Wide enough for two cells, so nothing was quietly dropped from the grid.
        assert!(out.image.width() as i32 >= out.cols * out.cell[0]);
    }

    /// Every name is drawn in full. Shrinking a cell by cutting the name off would leave a
    /// label that might be the button's name and might be a prefix of it.
    #[test]
    fn the_longest_name_sets_the_column_width() {
        let full = RgbaImage::from_pixel(200, 200, Rgba([90, 90, 90, 255]));
        let short = vec![cell("A", [0, 0, 20, 20]), cell("B", [30, 0, 20, 20])];
        let long = vec![
            cell("A", [0, 0, 20, 20]),
            cell("SOFTKEY_MENU_LEFT_LOWER", [30, 0, 20, 20]),
        ];
        let a = build(&full, &short, &opts("T")).expect("short");
        let b = build(&full, &long, &opts("T")).expect("long");

        assert!(b.cell[0] > a.cell[0], "{} should exceed {}", b.cell[0], a.cell[0]);
        assert!(
            b.cell[0] >= draw::text_width("SOFTKEY_MENU_LEFT_LOWER", 1),
            "the cell has to hold the whole name"
        );
    }

    /// One oversized rectangle must not set the cell size for the other hundred.
    #[test]
    fn a_whole_panel_rectangle_is_shrunk_to_the_cap() {
        let full = RgbaImage::from_pixel(1000, 1000, Rgba([90, 90, 90, 255]));
        let cells = vec![cell("KEY", [0, 0, 44, 44]), cell("PANEL", [0, 100, 700, 240])];
        let out = build(&full, &cells, &opts("T")).expect("two cells");
        assert!(
            out.cell[1] <= DEFAULT_CELL + draw::GLYPH_H + CAPTION_GAP + GUTTER * 2,
            "cell height {} ran past the cap",
            out.cell[1]
        );
    }

    /// A sheet of six buttons is narrower than the line saying which profile and which window
    /// it is of. The canvas widens for it — a picture that has outlived the request that made
    /// it and cannot say what it shows is not worth having kept.
    #[test]
    fn the_canvas_is_never_narrower_than_its_own_heading() {
        let full = RgbaImage::from_pixel(200, 200, Rgba([90, 90, 90, 255]));
        let cells = vec![cell("A", [0, 0, 20, 20]), cell("B", [30, 0, 20, 20])];
        let head = "NCTRAINER-MILL - 2 BUTTONS - CLIENT 1920X997 - SCREEN ORDER";
        let out = build(&full, &cells, &opts(head)).expect("two cells");

        assert!(out.image.width() as i32 > out.cols * out.cell[0], "the grid alone is narrower");
        assert!(out.image.width() as i32 >= draw::text_width(head, 1), "the heading is cut off");
    }

    /// A sheet of nothing is a request that did not make sense, said so rather than answered
    /// with a blank picture.
    #[test]
    fn an_empty_profile_is_refused_not_drawn() {
        let full = RgbaImage::from_pixel(50, 50, Rgba([0, 0, 0, 255]));
        assert!(build(&full, &[], &opts("T")).is_err());
    }
}
