//! Capture post-processing — crop, scale, PNG encoding, retention, cleanup.
//!
//! ## Why cropping and scaling are part of the feature
//!
//! A full-screen PNG is expensive on every call — in transfer, and in tokens for whoever
//! reads it. What is actually needed is usually one line of the status bar. So the
//! region and the scale sit at the front of the API as everyday parameters rather than
//! options.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use image::codecs::png::{CompressionType, FilterType as PngFilter, PngEncoder};
use image::{ExtendedColorType, ImageEncoder, RgbaImage, imageops};

use crate::targets::Rect;
use crate::win::capture::Shot;

/// Magnification ceiling. Past this you are looking at blocks of pixels, not glyphs.
const MAX_MAGNIFY: f64 = 8.0;
/// Output pixel ceiling, so the response cannot explode regardless of the scale.
const MAX_OUTPUT_PIXELS: u64 = 4_000_000;

/// The finished image — cropped and scaled.
pub struct Frame {
    pub image: RgbaImage,
    /// The cropped region in client coordinates, before scaling.
    pub source_rect: Rect,
    pub scale: f64,
}

impl Frame {
    pub fn width(&self) -> u32 {
        self.image.width()
    }
    pub fn height(&self) -> u32 {
        self.image.height()
    }

    /// Client coordinates → pixel coordinates in this image.
    ///
    /// Overlays are drawn **after** scaling (drawn before, the text and 1px lines smear), so
    /// the source coordinates of buttons and grid lines have to be brought across.
    pub fn map(&self, cx: i32, cy: i32) -> (i32, i32) {
        (
            ((cx - self.source_rect[0]) as f64 * self.scale).round() as i32,
            ((cy - self.source_rect[1]) as f64 * self.scale).round() as i32,
        )
    }

    /// Convert a length (width or height).
    pub fn map_len(&self, n: i32) -> i32 {
        (n as f64 * self.scale).round() as i32
    }

    /// **Where and how much** two captures differ. `None` if they are not the same size (the
    /// window was resized in between, which is not a question pixel comparison can answer).
    ///
    /// This exists because a single boolean cannot settle it. Measured: pressing a key on an
    /// MDI screen reported `changed: true` when the only thing that had moved was **one digit
    /// of the on-screen clock**. Given where and how many pixels, the caller can tell those
    /// apart.
    ///
    /// `ignore` is a rectangle in **window client coordinates** — leave out a spot that is
    /// always moving, like a clock.
    pub fn diff(&self, other: &Frame, ignore: Option<Rect>) -> Option<Diff> {
        if self.image.dimensions() != other.image.dimensions() {
            return None;
        }
        let (w, h) = (self.image.width() as i32, self.image.height() as i32);
        // Bring `ignore` into this image's pixel coordinates (crop offset plus scale).
        let ig = ignore.map(|r| {
            let (x0, y0) = self.map(r[0], r[1]);
            (x0, y0, x0 + self.map_len(r[2]), y0 + self.map_len(r[3]))
        });
        let (a, b) = (self.image.as_raw(), other.image.as_raw());
        let stride = (w * 4) as usize;
        let mut pixels: u64 = 0;
        let (mut x0, mut y0, mut x1, mut y1) = (i32::MAX, i32::MAX, i32::MIN, i32::MIN);
        for y in 0..h {
            let row = y as usize * stride;
            // Skip identical rows wholesale. Most rows are identical, and this one line
            // makes the cost proportional to the number of changed rows rather than to the
            // number of pixels.
            if a[row..row + stride] == b[row..row + stride] {
                continue;
            }
            for x in 0..w {
                if let Some((ix0, iy0, ix1, iy1)) = ig
                    && x >= ix0 && x < ix1 && y >= iy0 && y < iy1
                {
                    continue;
                }
                let i = row + x as usize * 4;
                if a[i..i + 4] != b[i..i + 4] {
                    pixels += 1;
                    if x < x0 { x0 = x; }
                    if y < y0 { y0 = y; }
                    if x > x1 { x1 = x; }
                    if y > y1 { y1 = y; }
                }
            }
        }
        if pixels == 0 {
            return Some(Diff { pixels: 0, bbox: [0, 0, 0, 0] });
        }
        // Put the bbox back into **window client coordinates** — the only system the caller uses.
        let back = |v: i32, off: i32| off + (v as f64 / self.scale).round() as i32;
        let (bx, by) = (back(x0, self.source_rect[0]), back(y0, self.source_rect[1]));
        Some(Diff {
            pixels,
            bbox: [
                bx,
                by,
                back(x1, self.source_rect[0]) - bx + 1,
                back(y1, self.source_rect[1]) - by + 1,
            ],
        })
    }

    pub fn to_png(&self) -> Result<Vec<u8>, String> {
        let mut buf = Vec::new();
        // Fast compression — screenshots compress well anyway, and this path sits inside a
        // round trip someone is waiting on.
        PngEncoder::new_with_quality(&mut buf, CompressionType::Fast, PngFilter::Adaptive)
            .write_image(
                self.image.as_raw(),
                self.image.width(),
                self.image.height(),
                ExtendedColorType::Rgba8,
            )
            .map_err(|e| format!("PNG encode failed: {e}"))?;
        Ok(buf)
    }
}

/// What changed between before and after a press — **where, and how much**.
pub struct Diff {
    /// Number of differing pixels. 0 means identical.
    pub pixels: u64,
    /// A rectangle around every differing pixel, in **window client coordinates**. Meaningless
    /// when `pixels == 0`.
    ///
    /// It is one rectangle, so two changes far apart enclose everything between them. Even so,
    /// "one clock digit (small, always in the same place)" and "half the screen changed" are
    /// distinguishable from this alone.
    pub bbox: Rect,
}

/// Crop a region out of a capture and apply the scale.
pub fn frame(shot: &Shot, rect: Rect, scale: Option<f64>, max_width: Option<u32>) -> Result<Frame, String> {
    let full = RgbaImage::from_raw(shot.width, shot.height, shot.rgba.clone())
        .ok_or_else(|| "capture buffer size does not match its dimensions".to_string())?;

    // Clamp anything that runs past the client area. A region defined slightly larger than
    // the window is no reason for the whole capture to fail.
    //
    // **Both edges are cut, not slid.** The right and bottom edges were always cut; the left
    // and top used to move to 0 while keeping their width, which slides the rectangle sideways
    // and returns a region the caller did not ask for. With `pad` on a button near an edge
    // that is the difference between "the button, off to one side of a wide picture" and
    // "the button, with as much margin as there was room for" — and only the second is what
    // the numbers say. `source_rect` reports what was actually taken either way, so nothing was
    // wrong about the coordinates; the picture was just of somewhere else.
    let [rx, ry, rw, rh] = rect;
    let x0 = rx.max(0) as u32;
    let y0 = ry.max(0) as u32;
    if x0 >= shot.width || y0 >= shot.height {
        return Err(format!(
            "region [{rx},{ry},{rw},{rh}] starts outside the {}x{} client area",
            shot.width, shot.height
        ));
    }
    // Work from the far edge so that whatever fell off the near side is subtracted, not kept.
    let right = rx.saturating_add(rw.max(0)).max(0) as u32;
    let bottom = ry.saturating_add(rh.max(0)).max(0) as u32;
    let w = right.min(shot.width).saturating_sub(x0);
    let h = bottom.min(shot.height).saturating_sub(y0);
    if w == 0 || h == 0 {
        return Err(format!("region [{rx},{ry},{rw},{rh}] is empty after clamping to the client area"));
    }

    let cropped = imageops::crop_imm(&full, x0, y0, w, h).to_image();

    // On a crop, **magnifying is allowed too.** Reading the legend on a small key needs it,
    // and this used to shrink only, so there was no way to do it (the inset that comes with
    // `mark` enlarges around one point; it is not a way to enlarge "this row of six keys").
    //
    // The whole screen is still refused — 1920x1080 at 4x is 33 megapixels, which is no use
    // to the reader either. A crop bounding the output size is what makes magnifying safe, so
    // that condition is used directly as the rule.
    let is_crop = w < shot.width || h < shot.height;
    let mut factor = scale.unwrap_or(1.0);
    // `!(factor > 0.0)` rather than `factor <= 0.0`, deliberately: it also catches NaN, and NaN
    // is reachable — a query string of `scale=nan` parses into one. Left through, every
    // comparison below is false and the arithmetic collapses to a 1x1 image with no complaint.
    #[allow(clippy::neg_cmp_op_on_partial_ord)]
    if !(factor > 0.0) {
        return Err(format!("scale must be greater than 0, got {factor}"));
    }
    if factor > 1.0 && !is_crop {
        return Err(format!(
            "scale {factor} would magnify the whole {}x{} client area. Magnifying is only \
             allowed on a crop — add region=NAME or rect=x,y,w,h and ask again.",
            shot.width, shot.height
        ));
    }
    if factor > MAX_MAGNIFY {
        return Err(format!("scale is capped at {MAX_MAGNIFY}x, got {factor}"));
    }
    // max_width bounds the output width in **both directions**. Being able to cap
    // magnification with it too means the caller never has to compute a scale factor.
    if let Some(mw) = max_width
        && mw > 0
    {
        factor = factor.min(mw as f64 / w as f64);
    }

    let nw = ((w as f64 * factor).round() as u32).max(1);
    let nh = ((h as f64 * factor).round() as u32).max(1);
    if (nw as u64) * (nh as u64) > MAX_OUTPUT_PIXELS {
        return Err(format!(
            "scale {factor} would produce a {nw}x{nh} image ({} megapixels); the cap is {} \
             megapixels. Crop tighter, lower the scale, or set max_width.",
            (nw as u64 * nh as u64) / 1_000_000,
            MAX_OUTPUT_PIXELS / 1_000_000
        ));
    }

    let image = if (factor - 1.0).abs() < f64::EPSILON {
        cropped
    } else {
        // Magnify with **no interpolation** (nearest). The point is to see the glyphs
        // larger, not to invent detail, so hard steps beat a smeared resample. Shrinking
        // wants the opposite.
        let filter = if factor > 1.0 {
            imageops::FilterType::Nearest
        } else {
            imageops::FilterType::Triangle
        };
        imageops::resize(&cropped, nw, nh, filter)
    };

    Ok(Frame { image, source_rect: [x0 as i32, y0 as i32, w as i32, h as i32], scale: factor })
}

/// A local timestamp for file names (`2026-08-12T14-05-33.482`).
/// Colons are not allowed in Windows file names, hence the hyphens.
fn stamp() -> String {
    let now = time::OffsetDateTime::now_local().unwrap_or_else(|_| time::OffsetDateTime::now_utc());
    format!(
        "{:04}-{:02}-{:02}T{:02}-{:02}-{:02}.{:03}",
        now.year(),
        now.month() as u8,
        now.day(),
        now.hour(),
        now.minute(),
        now.second(),
        now.millisecond(),
    )
}

/// Fold a name into something usable in a file name.
fn slug(s: &str) -> String {
    let out: String = s
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' })
        .collect();
    let trimmed = out.trim_matches('_').to_string();
    if trimmed.is_empty() { "capture".to_string() } else { trimmed }
}

/// Write a PNG into the retention folder and return the path.
pub fn save(dir: &Path, label: &str, png: &[u8]) -> Result<PathBuf, String> {
    std::fs::create_dir_all(dir)
        .map_err(|e| format!("failed to create capture dir {}: {e}", dir.display()))?;
    let name = format!("{}_{}.png", stamp(), slug(label));
    let path = dir.join(&name);
    std::fs::write(&path, png).map_err(|e| format!("failed to write {}: {e}", path.display()))?;
    Ok(path)
}

/// Remove captures that are too old or over the count. Called after every save.
/// Failures only warn — being unable to tidy up is no reason to fail a capture request.
pub fn cleanup(dir: &Path, keep: usize, max_age_minutes: u64) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut files: Vec<(SystemTime, PathBuf)> = entries
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().is_some_and(|x| x == "png"))
        .filter_map(|e| {
            let m = e.metadata().ok()?;
            Some((m.modified().ok()?, e.path()))
        })
        .collect();

    if max_age_minutes > 0 {
        let cutoff = SystemTime::now() - Duration::from_secs(max_age_minutes * 60);
        files.retain(|(t, p)| {
            if *t < cutoff {
                let _ = std::fs::remove_file(p);
                false
            } else {
                true
            }
        });
    }

    if keep > 0 && files.len() > keep {
        files.sort_by_key(|(t, _)| *t); // oldest first
        for (_, p) in files.iter().take(files.len() - keep) {
            let _ = std::fs::remove_file(p);
        }
    }
}

/// Validation for the name `GET /captures/<name>` accepts.
/// Path separators and traversal are blocked outright — this server is not a file server.
pub fn safe_capture_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 128
        && name.ends_with(".png")
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))
        && !name.contains("..")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shot(w: u32, h: u32, fill: u8) -> Shot {
        Shot {
            width: w,
            height: h,
            rgba: vec![fill; (w * h * 4) as usize],
            method: "test",
            black: false,
        }
    }

    #[test]
    fn crops_to_the_requested_region() {
        let f = frame(&shot(100, 80, 200), [10, 20, 30, 40], None, None).expect("frame");
        assert_eq!((f.width(), f.height()), (30, 40));
        assert_eq!(f.source_rect, [10, 20, 30, 40]);
    }

    /// A crop that starts outside the window loses the part that is outside — it does not
    /// slide inwards keeping its width.
    ///
    /// This is what `pad` does at an edge. Padding a button at x=22 by 200 asks for x=-178
    /// w=422; sliding that to x=0 w=422 hands back 178 columns from the far side that nobody
    /// asked for, and leaves the button off to one side of a picture meant to be centred on
    /// it. The right and bottom edges were always cut this way — the left and top were the
    /// odd ones out.
    #[test]
    fn a_crop_outside_the_window_is_cut_not_slid() {
        let shot = shot(1000, 800, 200);

        // Off the left: 178 columns are outside, so 178 columns are lost.
        let f = frame(&shot, [-178, 273, 422, 444], None, None).expect("crops");
        assert_eq!(f.source_rect, [0, 273, 244, 444], "width shrank by what fell off");
        assert_eq!(f.width(), 244);

        // The example printed in /help and the README, asserted end to end: a button 22 wide
        // sitting 22 from the left edge, asked for with pad=200. Every number in that
        // paragraph is one of these, because a sentence doing pixel arithmetic in prose is
        // worth exactly as much as the arithmetic.
        let button = [22, 300, 22, 40];
        let asked = crate::targets::Targets::pad_rect(button, 200);
        assert_eq!(asked, [-178, 100, 422, 440], "22 - 200, and 22 + 2x200");
        let f = frame(&shot, asked, None, None).expect("crops");
        assert_eq!(f.source_rect, [0, 100, 244, 440]);
        //          left margin 22 + button 22 + right margin 200 = 244
        assert_eq!(f.source_rect[2], 22 + button[2] + 200);

        // Off the top, same rule.
        let f = frame(&shot, [10, -30, 50, 100], None, None).expect("crops");
        assert_eq!(f.source_rect, [10, 0, 50, 70]);

        // Off both near edges at once.
        let f = frame(&shot, [-5, -5, 20, 20], None, None).expect("crops");
        assert_eq!(f.source_rect, [0, 0, 15, 15]);

        // The far edges behaved this way already, and still do.
        let f = frame(&shot, [980, 780, 100, 100], None, None).expect("crops");
        assert_eq!(f.source_rect, [980, 780, 20, 20]);

        // Entirely off the near side is empty, not a silently relocated picture.
        assert!(frame(&shot, [-50, 10, 40, 40], None, None).is_err());
    }

    #[test]
    fn region_larger_than_client_is_clamped_not_rejected() {
        let f = frame(&shot(100, 80, 200), [90, 70, 999, 999], None, None).expect("frame");
        assert_eq!((f.width(), f.height()), (10, 10));
    }

    #[test]
    fn region_starting_outside_is_an_error() {
        assert!(frame(&shot(100, 80, 0), [200, 0, 10, 10], None, None).is_err());
    }

    #[test]
    fn max_width_wins_over_scale_when_smaller() {
        let f = frame(&shot(400, 200, 128), [0, 0, 400, 200], Some(1.0), Some(100)).expect("frame");
        assert_eq!(f.width(), 100);
        assert_eq!(f.height(), 50);
    }

    #[test]
    fn scale_must_be_in_range() {
        assert!(frame(&shot(10, 10, 0), [0, 0, 10, 10], Some(0.0), None).is_err());
        assert!(frame(&shot(10, 10, 0), [0, 0, 10, 10], Some(-1.0), None).is_err());
        // `scale=nan` in a query string parses into a real NaN. Let it through and every
        // comparison downstream is false, so it silently produces a 1x1 image.
        assert!(frame(&shot(10, 10, 0), [0, 0, 10, 10], Some(f64::NAN), None).is_err());
        assert!("nan".parse::<f64>().is_ok(), "the query path really can produce NaN");
    }

    #[test]
    fn magnifying_is_allowed_on_a_crop_and_refused_on_the_whole_screen() {
        // Magnifying the whole screen is refused, and the message has to be the next step.
        let Err(e) = frame(&shot(10, 10, 0), [0, 0, 10, 10], Some(2.0), None) else {
            panic!("magnifying the whole screen must be refused");
        };
        assert!(e.contains("region=") && e.contains("rect="), "{e}");

        // A crop magnifies.
        let f = frame(&shot(100, 100, 0), [10, 10, 20, 10], Some(4.0), None).expect("crop");
        assert_eq!((f.width(), f.height()), (80, 40));
        // Coordinate mapping has to follow the scale, or a coordinate read off the
        // magnified image is wrong.
        assert_eq!(f.map(10, 10), (0, 0));
        assert_eq!(f.map(15, 10), (20, 0));

        // the scale ceiling
        assert!(frame(&shot(100, 100, 0), [0, 0, 20, 10], Some(9.0), None).is_err());
        // the output pixel ceiling — within 8x, but still too large a result is refused
        let Err(e) = frame(&shot(4000, 4000, 0), [0, 0, 1500, 1500], Some(2.0), None) else {
            panic!("too many output pixels must be refused");
        };
        assert!(e.contains("megapixels"), "{e}");
    }

    #[test]
    fn max_width_also_caps_magnification() {
        // max_width bounds the output width even while magnifying — a cap in both
        // directions, so the caller never has to compute a scale factor.
        let f = frame(&shot(100, 100, 0), [0, 0, 20, 10], Some(8.0), Some(60)).expect("crop");
        assert_eq!(f.width(), 60);
    }

    #[test]
    fn map_translates_and_scales_source_coordinates() {
        // In an image cropped at [10,20] and halved, where does source (110, 220) land
        let f = frame(&shot(400, 400, 50), [10, 20, 200, 200], Some(0.5), None).expect("frame");
        assert_eq!(f.map(110, 220), (50, 100));
        assert_eq!(f.map_len(40), 20);
        // the crop origin is the image's (0,0)
        assert_eq!(f.map(10, 20), (0, 0));
    }

    /// A frame with exactly one pixel changed.
    fn poke(f: &Frame, x: u32, y: u32) -> Frame {
        let mut img = f.image.clone();
        let p = img.get_pixel_mut(x, y);
        p.0 = [255, 0, 0, 255];
        Frame { image: img, source_rect: f.source_rect, scale: f.scale }
    }

    #[test]
    fn diff_reports_where_and_how_much_in_window_coordinates() {
        let a = frame(&shot(100, 80, 200), [10, 20, 40, 30], None, None).expect("frame");
        let b = poke(&a, 5, 6);

        let d = a.diff(&b, None).expect("same size");
        assert_eq!(d.pixels, 1);
        // The crop origin has to be added back so this comes out in **window coordinates**,
        // which is what makes it usable as a click straight away.
        assert_eq!(d.bbox, [15, 26, 1, 1]);

        // Identical images give 0. The bbox is meaningless then, so it is not checked.
        assert_eq!(a.diff(&a, None).expect("same size").pixels, 0);
    }

    #[test]
    fn ignore_drops_that_rectangle_from_the_comparison() {
        // Excluding a spot that always moves, like a clock, has to produce "no change".
        let a = frame(&shot(100, 80, 200), [10, 20, 40, 30], None, None).expect("frame");
        let b = poke(&a, 5, 6);
        assert_eq!(a.diff(&b, Some([15, 26, 1, 1])).expect("same size").pixels, 0);
        // Excluding one pixel over still catches it — proof `ignore` is not ignoring everything.
        assert_eq!(a.diff(&b, Some([16, 26, 1, 1])).expect("same size").pixels, 1);
    }

    #[test]
    fn diff_refuses_frames_of_different_size() {
        // The window was resized in between, which pixel comparison cannot answer.
        let a = frame(&shot(100, 80, 200), [0, 0, 40, 30], None, None).expect("frame");
        let b = frame(&shot(100, 80, 200), [0, 0, 50, 30], None, None).expect("frame");
        assert!(a.diff(&b, None).is_none());
    }

    #[test]
    fn encodes_a_real_png() {
        let png = frame(&shot(4, 4, 10), [0, 0, 4, 4], None, None).expect("frame").to_png().expect("png");
        assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n");
    }

    #[test]
    fn capture_names_cannot_escape_the_folder() {
        assert!(safe_capture_name("2026-08-12T10-00-00.000_status_bar.png"));
        assert!(!safe_capture_name("../config.json"));
        assert!(!safe_capture_name("..\\..\\secret.png"));
        assert!(!safe_capture_name("sub/dir.png"));
        assert!(!safe_capture_name("notapng.txt"));
    }
}
