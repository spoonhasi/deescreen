//! The HTTP handlers.
//!
//! ## Why there are separate `.png` variants
//!
//! A file path is the easiest thing for an AI to act on — when the tool and the AI are on
//! **the same PC**. This tool runs on
//! the simulator PC while the AI is on a development PC, so a path on that disk is not
//! something this end can open.
//!
//! So one behaviour gets two surfaces:
//! - `/capture` and `/click` — JSON, with the server-side path and the metadata. For when the
//!   tool and its consumer share a PC.
//! - `/capture.png` and `/click.png` — the PNG bytes themselves. One `curl -o shot.png` puts a
//!   file on the development PC for the AI to open. One round trip, done.
//!
//! The metadata (how it was captured, whether it was black, whether anything changed) follows
//! the `.png` surface too, in the `X-Deescreen-Meta` header.

use std::collections::HashMap;

use axum::body::Bytes;
use axum::extract::{Path as UrlPath, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::captures::{self, Frame};
use crate::config::Config;
use crate::draw;
use crate::sheet;
use crate::state::SharedState;
use crate::targets::{AnchorDef, ButtonDef, Rect, Targets};
use crate::web::{ApiError, json_ok, png_response};
use crate::win::capture::{self as wincap, BLACK_HINT};
use crate::win::input::{self, Button};
use crate::win::window::{self, FindError, WindowInfo};

// ────────────────────────────── request shapes ──────────────────────────────

// serde cannot combine `flatten` with `deny_unknown_fields` (the outer struct sees the
// flattened fields as unknown keys). Capturing is read-only, so ignoring an unknown key here
// is harmless and this one is the exception — the control requests (ClickReq, KeyReq) stay
// strict.
#[derive(Deserialize, Default)]
pub struct CaptureReq {
    /// Set by `from_query`; a parsed body leaves it at its default. Only the refusal text
    /// depends on it.
    #[serde(skip)]
    pub params: Params,
    /// Which profile, and therefore which window. Omitted, the default profile.
    #[serde(default)]
    pub profile: Option<String>,
    /// A named region. Absent, the whole client area (`"client"`).
    #[serde(default)]
    pub region: Option<String>,
    /// An ad-hoc region `[x, y, w, h]`, taking precedence over `region`. **Read-only, so there
    /// is no whitelist** — the worst a bad crop does is produce a bad picture; nothing is
    /// pressed.
    #[serde(default)]
    pub rect: Option<Rect>,
    #[serde(default)]
    pub scale: Option<f64>,
    #[serde(default)]
    pub max_width: Option<u32>,
    /// Whether to also keep a file in the captures folder. Defaults to `true`.
    #[serde(default)]
    pub save: Option<bool>,
    #[serde(default, flatten)]
    pub overlay: Overlay,
    /// Pixels to grow the capture rectangle by on every side.
    #[serde(default)]
    pub pad: Option<i32>,
}

/// What gets drawn over a capture. All **read-only** — none of it presses anything.
#[derive(Deserialize, Default, Clone)]
pub struct Overlay {
    /// Grid spacing in client pixels. The labels carry source coordinates, so even in a
    /// shrunken capture a number read off the picture goes straight into the profile.
    #[serde(default)]
    pub grid: Option<i32>,
    /// Draw a crosshair at this point — for checking a guess **before** pressing.
    #[serde(default)]
    pub mark: Option<[i32; 2]>,
    /// Magnification around the mark (1–8, default 3). A small button is barely a dot in the
    /// whole picture, and without the magnified inset "middle or edge" is unanswerable.
    #[serde(default)]
    pub inset: Option<i32>,
    /// The radius to magnify, in client pixels (default 40).
    #[serde(default)]
    pub inset_radius: Option<i32>,
    /// Also draw the saved buttons and regions. `true`/`false`, or a mode name.
    #[serde(default)]
    pub buttons: Option<ButtonsOpt>,
}

/// How `buttons` gets drawn.
///
/// A mode rather than on/off, because the crosshair marks the **click point**, and that is
/// usually the middle of a key, which is where the key's legend is. So naming buttons — a job
/// that needs "the rectangle and its name" and "the lettering underneath" at the same time —
/// meant capturing the same spot twice. The crosshair cannot simply go: on a button with an
/// explicit `point`, the click point differs from the rectangle's centre and that mark is the
/// only thing that shows it. Being able to turn it off is enough.
#[derive(Deserialize, Clone, Copy, PartialEq, Debug)]
#[serde(rename_all = "lowercase")]
pub enum ButtonMode {
    /// Outline, name and crosshair. What `buttons=1` means — older calls keep working.
    Full,
    /// Outline and name. No crosshair.
    Box,
    /// Outline and **a number**, for when names are packed too tightly and cover each other.
    /// The number is the 1-based position in `GET /buttons` (sorted by name), so no separate
    /// legend table is needed.
    Num,
}

/// Accepts `true`/`false` as well as `"box"`, `"num"` and `"full"`.
#[derive(Deserialize, Clone, Copy, Debug)]
#[serde(untagged)]
pub enum ButtonsOpt {
    On(bool),
    Mode(ButtonMode),
}

impl ButtonsOpt {
    fn mode(self) -> Option<ButtonMode> {
        match self {
            ButtonsOpt::On(true) => Some(ButtonMode::Full),
            ButtonsOpt::On(false) => None,
            ButtonsOpt::Mode(m) => Some(m),
        }
    }
}

impl Overlay {
    fn from_query(q: &HashMap<String, String>) -> Result<Overlay, ApiError> {
        let mark = match q_get(q, "mark") {
            None => None,
            Some(v) => {
                let p: Result<Vec<i32>, _> = v.split(',').map(|s| s.trim().parse::<i32>()).collect();
                match p {
                    Ok(p) if p.len() == 2 => Some([p[0], p[1]]),
                    _ => return Err(ApiError::bad_request(format!("mark must be 'x,y', got '{v}'"))),
                }
            }
        };
        Ok(Overlay {
            grid: q_num(q, "grid")?,
            mark,
            inset: q_num(q, "inset")?,
            inset_radius: q_num(q, "inset_radius")?,
            buttons: q_buttons(q)?,
        })
    }

    fn is_empty(&self) -> bool {
        self.grid.is_none() && self.mark.is_none() && self.mode().is_none()
    }

    fn mode(&self) -> Option<ButtonMode> {
        self.buttons.and_then(ButtonsOpt::mode)
    }
}

/// `buttons=` takes a boolean or a mode name. What `1` means does not change — older calls
/// have to keep working.
fn q_buttons(q: &HashMap<String, String>) -> Result<Option<ButtonsOpt>, ApiError> {
    let Some(v) = q_get(q, "buttons") else { return Ok(None) };
    Ok(Some(match v.to_lowercase().as_str() {
        "1" | "true" | "yes" | "on" | "full" => ButtonsOpt::On(true),
        "0" | "false" | "no" | "off" => ButtonsOpt::On(false),
        "box" => ButtonsOpt::Mode(ButtonMode::Box),
        "num" => ButtonsOpt::Mode(ButtonMode::Num),
        other => {
            return Err(ApiError::bad_request(format!(
                "buttons must be 1/0 or one of full, box, num — got '{other}'"
            ))
            .with_detail(json!({
                "full": "outline + name + crosshair (same as buttons=1)",
                "box": "outline + name, no crosshair — the crosshair sits on the key legend",
                "num": "outline + number; numbers are 1-based positions in GET /buttons",
            })));
        }
    }))
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct ClickReq {
    /// Which profile, and therefore which window. Omitted, the default profile.
    #[serde(default)]
    pub profile: Option<String>,
    /// Press several saved buttons in order, in one request.
    ///
    /// Entering data on an MDI keypad is one click per character, and a partial string left
    /// in the machine is worse than no string at all — press CYCLE START after it and an
    /// unintended block runs. So this is not merely a round-trip saving: the whole array is
    /// resolved and checked BEFORE anything is pressed, and if a press fails the sequence
    /// stops there and the reply says which index it was.
    ///
    /// Mutually exclusive with `button` / `rect` / `point`.
    #[serde(default)]
    pub buttons: Option<Vec<String>>,
    /// Milliseconds to wait between the presses of `buttons`.
    #[serde(default)]
    pub gap_ms: Option<u64>,
    /// Pixels to grow the capture rectangle by on every side (client pixels, in the
    /// profile's own coordinates). A toggle's lamp is usually just outside its button.
    #[serde(default)]
    pub pad: Option<i32>,
    /// The name of the button to press — a key of `buttons` in the profile file.
    ///
    /// No alias for this field, `target` least of all: a request carrying both `target` and
    /// `button` reads naturally as a button name plus a mouse button, and one of the two would
    /// have to lose. `deny_unknown_fields` refuses the whole request instead.
    #[serde(default)]
    pub button: Option<String>,
    /// **Press an unsaved area on the spot** — `[x, y, w, h]`, pressed at its centre.
    ///
    /// Controls that appear only on certain screens cannot be on the named list. To press one,
    /// read the rectangle off a capture and send it. Nothing is stored. Requires
    /// `allow_raw_clicks`.
    #[serde(default)]
    pub rect: Option<Rect>,
    /// The point form of `rect`, for when the size is unknown.
    #[serde(default)]
    pub point: Option<[i32; 2]>,
    /// Which mouse button to press with (`left` | `right` | `middle`).
    /// Overrides the button definition's own `click_button`.
    #[serde(default)]
    pub click_button: Option<String>,
    /// Set by `from_query`; a parsed body leaves it at its default. Only the refusal text
    /// depends on it.
    #[serde(skip)]
    pub params: Params,
    #[serde(default)]
    pub double: Option<bool>,
    /// How long to hold the press (ms), overriding the button's own value and the default.
    #[serde(default)]
    pub hold_ms: Option<u64>,
    /// Pressing a `confirm: true` button requires this on the request as well.
    #[serde(default)]
    pub confirm: bool,
    #[serde(default)]
    pub settle_ms: Option<u64>,
    /// **Spell a string on the keypad** — expanded into `buttons` before anything is
    /// pressed, using the keys' own `types`/`shift_types` legends.
    ///
    /// Not called `text`: `POST /key` has a `text` that types on the **PC keyboard**, and one
    /// word for two different routes into the machine is exactly the confusion that made
    /// `keys` and `chord` separate names.
    #[serde(default)]
    pub spell: Option<String>,
    /// The region to capture after the press. Absent, nothing is captured.
    #[serde(default)]
    pub capture: Option<String>,
    /// Photograph between presses, so each press's own change is reported with it.
    ///
    /// A sequence otherwise reports only the screen after the last press, and a key silently
    /// ignored halfway through leaves an end screen that looks like a working one minus a
    /// character. Costs one capture per press, so it is asked for rather than assumed.
    #[serde(default)]
    pub per_press: Option<bool>,
    /// Measure how long the screen takes to settle, instead of waiting a fixed `settle_ms`.
    ///
    /// Costs a capture every 50ms until it holds still, so it is opt-in — but it answers
    /// the question `settle_ms` otherwise leaves to repetition, and the reply carries the
    /// number to write into the profile.
    #[serde(default)]
    pub measure: Option<bool>,
    /// How long the screen has to hold still before `measure` calls it settled. Raise it for an
    /// application that repaints in stages.
    #[serde(default)]
    pub quiet_ms: Option<u64>,
    /// Rectangle to **exclude** from the change comparison (window client coordinates).
    /// Put a clock in here and it stops making `changed` true on its own.
    #[serde(default)]
    pub ignore: Option<Rect>,
    #[serde(default)]
    pub scale: Option<f64>,
    #[serde(default)]
    pub max_width: Option<u32>,
}

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct KeyReq {
    /// Which profile, and therefore which window. Omitted, the default profile.
    #[serde(default)]
    pub profile: Option<String>,
    /// A name from the profile's `keys`.
    #[serde(default)]
    pub key: Option<String>,
    /// An unnamed chord — modifiers plus one key, struck together (`"f1"`, `"ctrl+alt+f1"`).
    /// Requires `allow_raw_keys`.
    ///
    /// Deliberately not a spelling of `key`. That field looks a definition up in the profile;
    /// this one bypasses the profile altogether, which is why it is gated. Opposite acts do not
    /// get near-identical names — a slip of one letter must not be how you reach the gated one.
    #[serde(default)]
    pub chord: Option<String>,
    /// Type a string. Also requires `allow_raw_keys`.
    #[serde(default)]
    pub text: Option<String>,
    #[serde(default)]
    pub settle_ms: Option<u64>,
    #[serde(default)]
    pub capture: Option<String>,
    /// Pixels to grow the capture rectangle by on every side.
    #[serde(default)]
    pub pad: Option<i32>,
    /// Measure how long the screen takes to settle, instead of waiting a fixed `settle_ms`.
    ///
    /// Costs a capture every 50ms until it holds still, so it is opt-in — but it answers
    /// the question `settle_ms` otherwise leaves to repetition, and the reply carries the
    /// number to write into the profile.
    #[serde(default)]
    pub measure: Option<bool>,
    /// How long the screen has to hold still before `measure` calls it settled. Raise it for an
    /// application that repaints in stages.
    #[serde(default)]
    pub quiet_ms: Option<u64>,
    /// Rectangle to **exclude** from the change comparison (window client coordinates).
    /// Put a clock in here and it stops making `changed` true on its own.
    #[serde(default)]
    pub ignore: Option<Rect>,
    #[serde(default)]
    pub scale: Option<f64>,
    #[serde(default)]
    pub max_width: Option<u32>,
}

fn parse_body<T: serde::de::DeserializeOwned + Default>(body: &Bytes) -> Result<T, ApiError> {
    if body.is_empty() {
        return Ok(T::default());
    }
    serde_json::from_slice(body).map_err(|e| ApiError::bad_request(format!("invalid JSON body: {e}")))
}

// Query-string parsing — the `.png` variants take their parameters in the query rather than
// a body, so one line of curl is enough.

fn q_get(q: &HashMap<String, String>, k: &str) -> Option<String> {
    q.get(k).map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
}

fn q_num<T: std::str::FromStr>(q: &HashMap<String, String>, k: &str) -> Result<Option<T>, ApiError> {
    match q_get(q, k) {
        None => Ok(None),
        Some(v) => v
            .parse::<T>()
            .map(Some)
            .map_err(|_| ApiError::bad_request(format!("query parameter '{k}' is not a number: {v}"))),
    }
}

fn q_bool(q: &HashMap<String, String>, k: &str) -> Result<Option<bool>, ApiError> {
    match q_get(q, k) {
        None => Ok(None),
        Some(v) => match v.to_lowercase().as_str() {
            "1" | "true" | "yes" | "on" => Ok(Some(true)),
            "0" | "false" | "no" | "off" => Ok(Some(false)),
            other => Err(ApiError::bad_request(format!("query parameter '{k}' is not a boolean: {other}"))),
        },
    }
}

fn q_rect_named(q: &HashMap<String, String>, key: &str) -> Result<Option<Rect>, ApiError> {
    let Some(v) = q_get(q, key) else { return Ok(None) };
    let parts: Result<Vec<i32>, _> = v.split(',').map(|p| p.trim().parse::<i32>()).collect();
    match parts {
        Ok(p) if p.len() == 4 => Ok(Some([p[0], p[1], p[2], p[3]])),
        _ => Err(ApiError::bad_request(format!("{key} must be 'x,y,w,h', got '{v}'"))),
    }
}

fn q_rect(q: &HashMap<String, String>) -> Result<Option<Rect>, ApiError> {
    q_rect_named(q, "rect")
}

impl CaptureReq {
    fn from_query(q: &HashMap<String, String>) -> Result<CaptureReq, ApiError> {
        Ok(CaptureReq {
            params: Params::Query,
            profile: q_get(q, "profile"),
            region: q_get(q, "region"),
            rect: q_rect(q)?,
            scale: q_num(q, "scale")?,
            max_width: q_num(q, "max_width")?,
            save: q_bool(q, "save")?,
            overlay: Overlay::from_query(q)?,
            pad: q_num(q, "pad")?,
        })
    }
}

impl ClickReq {
    fn from_query(q: &HashMap<String, String>) -> Result<ClickReq, ApiError> {
        let point = match q_get(q, "point") {
            None => None,
            Some(v) => {
                let parts: Result<Vec<i32>, _> = v.split(',').map(|p| p.trim().parse::<i32>()).collect();
                match parts {
                    Ok(p) if p.len() == 2 => Some([p[0], p[1]]),
                    _ => return Err(ApiError::bad_request(format!("point must be 'x,y', got '{v}'"))),
                }
            }
        };
        // buttons=A,B,C — the .png variant has no body, so the sequence has to fit in a
        // query string as well.
        let buttons = q_get(q, "buttons").map(|v| {
            v.split(',').map(|b| b.trim().to_string()).filter(|b| !b.is_empty()).collect()
        });
        Ok(ClickReq {
            params: Params::Query,
            profile: q_get(q, "profile"),
            buttons,
            gap_ms: q_num(q, "gap_ms")?,
            button: q_get(q, "button"),
            rect: q_rect(q)?,
            point,
            click_button: q_get(q, "click_button"),
            double: q_bool(q, "double")?,
            hold_ms: q_num(q, "hold_ms")?,
            confirm: q_bool(q, "confirm")?.unwrap_or(false),
            settle_ms: q_num(q, "settle_ms")?,
            spell: q_get(q, "spell"),
            per_press: q_bool(q, "per_press")?,
            measure: q_bool(q, "measure")?,
            quiet_ms: q_num(q, "quiet_ms")?,
            capture: q_get(q, "capture"),
            pad: q_num(q, "pad")?,
            ignore: q_rect_named(q, "ignore")?,
            scale: q_num(q, "scale")?,
            max_width: q_num(q, "max_width")?,
        })
    }
}

// ────────────────────────── shared work (blocking) ──────────────────────────

/// Where a request carried its parameters.
///
/// The only thing that decides whether "put it in the query" or "put it in the body" is true
/// advice, and endpoints differ: the `.png` variants read a query string, their JSON siblings
/// read a body, and none of them read both. Saying both was wrong half the time, on the half
/// the caller was standing in.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Params {
    /// A body parsed as JSON. The default because that is what deserializing produces, and the
    /// query path sets it explicitly.
    #[default]
    Body,
    /// A query string.
    Query,
}

impl Params {
    /// How to name a profile on this endpoint.
    fn profile_hint(self) -> &'static str {
        match self {
            Params::Query => "pass ?profile=NAME in the query string — this endpoint takes its \
                              parameters there and does not read a body",
            Params::Body => "put \"profile\": \"NAME\" in the JSON body — this endpoint takes \
                             its parameters there and does not read the query string",
        }
    }
}

/// The profile a request selected. An unknown name comes back as 404 with the known list —
/// **nothing is picked for you.** Clicking while it is unclear which window is being driven is
/// the class of accident this tool was built to prevent.
///
/// `from` is only there so the refusal can say where to put the name, and is the one thing
/// `pick` cannot work out for itself — see [`Params`].
fn pick(
    state: &SharedState,
    name: Option<&str>,
    from: Params,
) -> Result<std::sync::Arc<crate::state::Profile>, ApiError> {
    state.profile(name).map_err(|e| {
        ApiError::not_found(e).with_detail(json!({
            "known_profiles": state.profile_names(),
            "default": state.effective_default(),
            "hint": from.profile_hint(),
        }))
    })
}

struct Placed {
    /// How many labels had nowhere to go and were replaced by their number.
    collided: usize,
    /// The boxes of labels that **found** a spot. These never overlap each other — a promise
    /// a test asserts directly (`labels_do_not_overlap_each_other`).
    #[cfg_attr(not(test), allow(dead_code))]
    placed: Vec<[i32; 4]>,
}

/// Place labels so they **do not overlap each other**.
///
/// A name wider than its rectangle runs into its neighbour and reads as one thing — measured,
/// that produced `"MDI_CASE_TMDI_Z"` (MDI_CASE_TOGGLE plus MDI_Z). From the picture there is no
/// telling whether that is two names or one, so this is not an aesthetic problem: **an
/// overlapping label is wrong information**.
///
/// Four spots are tried per rectangle — above, below, inside-top, inside-bottom — and if all
/// are blocked the number is drawn instead of the name (the `GET /buttons` position). A number
/// is one glyph wide, so it almost always fits.
fn place_labels(
    img: &mut image::RgbaImage,
    labels: &[(i32, i32, i32, i32, String, draw::Color)],
) -> Placed {
    let mut taken: Vec<[i32; 4]> = Vec::with_capacity(labels.len());
    let mut placed: Vec<[i32; 4]> = Vec::new();
    let mut collided = 0usize;

    for (i, (x, y, _w, h, text, color)) in labels.iter().enumerate() {
        // Above → below → inside-top → inside-bottom. Above comes first because that was the
        // old behaviour, and because most labels are done at the first spot.
        let spots = [
            (x + 2, y - draw::GLYPH_H - 4),
            (x + 2, y + h + 4),
            (x + 2, y + 3),
            (x + 2, y + h - draw::GLYPH_H - 5),
        ];
        let mut done = false;
        for (lx, ly) in spots {
            if ly < 0 {
                continue;
            }
            let b = draw::label_box(lx, ly, text);
            if taken.iter().any(|t| draw::boxes_overlap(b, *t)) {
                continue;
            }
            draw::text(img, lx, ly, text, 1, draw::BLACK, Some(*color));
            taken.push(b);
            placed.push(b);
            done = true;
            break;
        }
        if !done {
            // The name did not fit anywhere. Fall back to the number, and draw it even if that
            // overlaps: something overlapping beats nothing at all, and the count is reported.
            collided += 1;
            let num = format!("{}", i + 1);
            let (lx, ly) = (x + 2, (y - draw::GLYPH_H - 4).max(y + 3));
            let b = draw::label_box(lx, ly, &num);
            draw::text(img, lx, ly, &num, 1, draw::BLACK, Some(*color));
            taken.push(b);
        }
    }
    Placed { collided, placed }
}

/// Compare the before and after captures and put `changed` — and the evidence for it — into
/// the metadata.
///
/// A single boolean cannot answer this. `changed` mashes together **two independent
/// questions**: (a) did the click land, and (b) did the screen change as a result. All four
/// combinations are real:
///
/// | | screen changed | screen identical |
/// |---|---|---|
/// | landed | it worked | blank key · toggle already in that state · ignored in this mode |
/// | did not land | a clock or animation moved on its own | the coordinate hit background |
///
/// So (a) is answered by `hit` — a window lookup, independent of pixels — and (b) is answered
/// here, carrying **how much and where**, so the caller can tell one clock digit from a real
/// change.
fn apply_change(
    meta: &mut Value,
    before: &crate::captures::Frame,
    after: &crate::captures::Frame,
    ignore: Option<Rect>,
    hit: Option<&crate::win::window::ControlHit>,
) {
    let Some(d) = before.diff(after, ignore) else {
        // Different sizes = the window was resized in between, which pixel comparison
        // cannot answer.
        meta["changed"] = json!(true);
        meta["changed_hint"] =
            json!("the capture changed size between the two shots — the window was resized");
        return;
    };
    let changed = d.pixels > 0;
    meta["changed"] = json!(changed);
    let total = (after.width() as u64) * (after.height() as u64);
    meta["change"] = json!({
        "pixels": d.pixels,
        "fraction": if total == 0 { 0.0 } else { d.pixels as f64 / total as f64 },
        "bbox": if changed { json!(d.bbox) } else { Value::Null },
        "ignored": ignore.map(|r| json!(r)).unwrap_or(Value::Null),
        "note": "bbox is in window client coordinates and is ONE rectangle around every \
                 changed pixel, so two far-apart changes enclose everything between them. A \
                 small bbox that sits in the same place every time is usually a clock or a \
                 blinking cursor — pass ignore=x,y,w,h to drop it from the comparison.",
    });
    if !changed {
        // This may be entirely correct (a toggle already in that state, a blank key, a
        // disabled control). But UIPI blocking looks exactly the same, and what separates the
        // two is `hit` plus `input.uipi_risk` in /health.
        meta["changed_hint"] = json!(match hit {
            Some(h) if h.is_window_itself =>
                "nothing changed, and no control sits under that point — the coordinate most likely landed on panel background. Capture with ?mark=x,y&inset=4 and look.",
            Some(h) if !h.enabled =>
                "nothing changed, and the control under that point is disabled. This is a correct no-change: the click arrived and the control ignored it.",
            Some(_) =>
                "nothing changed, but a control does sit under that point, so the click was aimed at something real. If /health reports input.uipi_risk false, this is simply a control that does not repaint — a blank key, a toggle already in that state, or one ignored in the current mode.",
            None =>
                "the captured region looks identical before and after. That is normal for a control already in the requested state, but it is also what a UIPI-blocked input looks like — check /health input.uipi_risk.",
        });
    }
}

/// What was under a coordinate, shaped for the response.
///
/// Whether a click **arrived** cannot be answered with pixels, because buttons that change
/// nothing when pressed legitimately exist (blank keys, a toggle already in that state, one
/// ignored in the current mode). So the control that was under the point comes back as-is, and
/// with it, arrival is settled independently of pixels.
fn hit_json(h: &crate::win::window::ControlHit) -> Value {
    json!({
        "hwnd": h.hwnd,
        "class": h.class,
        "text": h.text,
        "id": h.id,
        "rect": h.rect,
        "visible": h.visible,
        "enabled": h.enabled,
        "depth": h.depth,
        "is_window_itself": h.is_window_itself,
        "note": if h.is_window_itself {
            "no child control sits at this point — either the coordinate landed on panel \
             background, or this application does not split its controls into windows \
             (WPF, a single-bitmap HMI). GET /controls returning an empty list means the latter."
        } else if !h.enabled {
            "the control is disabled — a click reaches it and nothing happens. That is a \
             correct no-change, not a lost click."
        } else if !h.visible {
            "the control is not visible — the click may land on whatever is drawn over it."
        } else {
            "a control sits at this point, so the coordinate is aimed at something real. \
             Whether it reacts is a separate question."
        },
    })
}

/// Find the one configured window. Nothing found, or several, fails with **what to do next**
/// attached.
fn find_window(t: &Targets) -> Result<WindowInfo, ApiError> {
    let info = window::find(&t.window).map_err(|e| match e {
        FindError::NotFound => ApiError::not_found(format!(
            "no visible window matches {:?}",
            t.window.title
        ))
        .with_detail(json!({
            "spec": t.window,
            "hint": "call GET /windows to list visible window titles, then fix the profile's window spec",
        })),
        FindError::Ambiguous(list) => ApiError::conflict(format!(
            "{} windows match {:?} — refusing to guess which one to drive",
            list.len(),
            t.window.title
        ))
        .with_detail(json!({
            "matches": list,
            "hint": "narrow it with window.title_exact or window.class in the profile",
        })),
    })?;

    if info.minimized {
        return Err(ApiError::conflict("the target window is minimized").with_detail(json!({
            "hint": "POST /window/focus restores and raises it",
        })));
    }
    Ok(info)
}

/// For the control paths — find the window and **verify the coordinate system is still valid**.
/// The returned factor is only ever other than 1 under `on_size_mismatch: "scale"`.
fn bind_window(t: &Targets) -> Result<(WindowInfo, (f64, f64)), ApiError> {
    let info = find_window(t)?;
    let scale = t
        .coordinate_scale(info.client_size)
        .map_err(|e| ApiError::conflict(e).with_detail(json!({
            "client": [info.client_size.0, info.client_size.1],
            "reference_client": t.reference_client,
        })))?;
    Ok((info, scale))
}

/// The lenient version, used by the view-only paths (`/capture*`, `/preview.png`, `/window`).
///
/// A size mismatch does **not** refuse to show you a picture. A mismatch is exactly when you
/// need to look — seeing what drifted and how is what decides whether to call `/window/fit`.
/// Looking presses nothing, so there is no risk; the warning rides along in the response
/// instead. The control path (`bind_window`) stays strict.
/// The window, the coordinate factor, and a size warning if there was one.
type ViewBinding = (WindowInfo, (f64, f64), Option<String>);

fn bind_window_view(t: &Targets) -> Result<ViewBinding, ApiError> {
    let info = find_window(t)?;
    match t.coordinate_scale(info.client_size) {
        Ok(s) => Ok((info, s, None)),
        Err(_) => {
            let (rw, rh) = t.reference_client.map(|r| (r[0], r[1])).unwrap_or((0, 0));
            let warning = format!(
                "client area is {}x{} but the buttons were measured at {rw}x{rh}, so any overlaid boxes are drawn unscaled and will not line up. Nothing is blocked for viewing, but /click and /key will refuse until the two agree. If the buttons match the window as it looks now, set reference_client to {}x{} (the editor has a button); if the window drifted from a good measurement, POST /window/fit to put it back to {rw}x{rh}.",
                info.client_size.0, info.client_size.1, info.client_size.0, info.client_size.1
            );
            Ok((info, (1.0, 1.0), Some(warning)))
        }
    }
}

/// Capture the window and crop out the requested region.
fn shoot(
    info: &WindowInfo,
    rect: Rect,
    scale: Option<f64>,
    max_width: Option<u32>,
) -> Result<(Frame, &'static str, bool), ApiError> {
    let shot = wincap::capture_client(info).map_err(ApiError::internal)?;
    let (method, black) = (shot.method, shot.black);
    let frame = captures::frame(&shot, rect, scale, max_width).map_err(ApiError::bad_request)?;
    Ok((frame, method, black))
}

/// How often to look while measuring. Fine enough that 50ms of resolution is not the limiting
/// factor on a number in the hundreds, coarse enough that the looking does not become the
/// thing being measured.
const WATCH_POLL_MS: u64 = 50;
/// How long the screen has to hold still before it counts as settled.
const DEFAULT_QUIET_MS: u64 = 300;

/// What watching the screen after a press saw.
///
/// **"Settled" here means "held still for `quiet_ms`", which is a definition and not an
/// observation.** An application that pauses longer than that between two repaints is called
/// settled during the pause, and nothing outside the process can tell the difference. So the
/// definition is reported alongside the answer, and raising `quiet_ms` is what to do when a
/// screen is known to arrive in stages.
struct Watch {
    /// Milliseconds after the press when the picture last differed from the one before it.
    /// Zero means it never changed at all — which `changed` reports separately.
    last_change_ms: u64,
    first_change_ms: Option<u64>,
    /// How long it had been still when watching stopped.
    quiet_for_ms: u64,
    /// Total time spent watching, which is what the caller actually waited.
    elapsed_ms: u64,
    samples: usize,
    /// The gap actually achieved between shots — the resolution of every number above. A
    /// capture is not free, so this is larger than the interval asked for, and saying so beats
    /// implying a precision the method does not have.
    resolution_ms: u64,
    /// False when the ceiling arrived first: the screen never held still.
    settled: bool,
    /// The last picture taken. Reused as the request's capture, so measuring costs no extra
    /// shot and the picture is of the moment the screen was declared settled.
    last: (Frame, &'static str, bool),
}

/// Watch one region until it stops changing.
fn watch_until_still(
    info: &WindowInfo,
    rect: Rect,
    scale: Option<f64>,
    max_width: Option<u32>,
    ignore: Option<Rect>,
    quiet_ms: u64,
    ceiling_ms: u64,
) -> Result<Watch, ApiError> {
    let started = std::time::Instant::now();
    let mut prev = shoot(info, rect, scale, max_width)?;
    let mut samples = 1usize;
    // The press itself is a change, at t=0. Starting from "never changed" would let a screen
    // that has not repainted yet be called settled before it began.
    let mut last_change_ms = 0u64;
    let mut first_change_ms: Option<u64> = None;
    let mut settled = false;

    loop {
        let now = started.elapsed().as_millis() as u64;
        if now.saturating_sub(last_change_ms) >= quiet_ms {
            settled = true;
            break;
        }
        if now >= ceiling_ms {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(WATCH_POLL_MS));
        let cur = shoot(info, rect, scale, max_width)?;
        samples += 1;
        let at = started.elapsed().as_millis() as u64;
        // A frame that changed size means the window was resized while being watched. That is
        // a change, and a larger one than any number of pixels.
        let moved = prev.0.diff(&cur.0, ignore).map(|d| d.pixels > 0).unwrap_or(true);
        if moved {
            first_change_ms.get_or_insert(at);
            last_change_ms = at;
        }
        prev = cur;
    }

    let elapsed_ms = started.elapsed().as_millis() as u64;
    Ok(Watch {
        last_change_ms,
        first_change_ms,
        quiet_for_ms: elapsed_ms.saturating_sub(last_change_ms),
        elapsed_ms,
        samples,
        resolution_ms: elapsed_ms / (samples.saturating_sub(1).max(1)) as u64,
        settled,
        last: prev,
    })
}

/// What one press changed, for the record of that press.
///
/// Deliberately the same shape as the request-level `changed`, and deliberately without a
/// verdict: **nothing changing is not the same as nothing happening.** A toggle already in
/// that state, a key with no legend to repaint, a key ignored in this mode and a key that
/// never arrived all look identical here. What this adds is *which* press it was, which the
/// screen at the end of a sequence cannot say.
fn press_change(prev: &Frame, now: &Frame, ignore: Option<Rect>) -> Value {
    match prev.diff(now, ignore) {
        None => json!({
            "changed": true,
            "note": "the capture changed size between these two presses — the window was resized",
        }),
        Some(d) => json!({
            "changed": d.pixels > 0,
            "pixels": d.pixels,
            "bbox": if d.pixels > 0 { json!(d.bbox) } else { Value::Null },
        }),
    }
}

/// How long to require stillness, and how long to wait for it.
///
/// The ceiling is the server's own `max_settle_ms`, because a measurement is a wait and the
/// operator's limit on waiting does not stop applying because the wait is being measured.
/// `quiet_ms` is held below half of it: at or above the ceiling nothing could ever be called
/// settled, and a knob whose extreme setting silently guarantees failure is a trap.
fn watch_bounds(cfg: &Config, quiet_ms: Option<u64>) -> (u64, u64) {
    let ceiling = cfg.max_settle_ms.max(WATCH_POLL_MS * 4);
    let quiet = quiet_ms
        .unwrap_or(DEFAULT_QUIET_MS)
        .clamp(WATCH_POLL_MS, (ceiling / 2).max(WATCH_POLL_MS));
    (quiet, ceiling)
}

/// The number to write into the profile.
///
/// A quarter more than the longest wait seen, rounded up to 50ms, and never shorter than two
/// sampling intervals. Measured once on a quiet machine, this has to survive a busy one — and
/// a settle that is too long costs a wait, while one that is too short returns the screen from
/// before the press and calls it the result.
fn suggest_settle(last_change_ms: u64, resolution_ms: u64) -> u64 {
    let n = (last_change_ms + last_change_ms / 4).max(resolution_ms * 2);
    n.div_ceil(50) * 50
}

/// The measurement, as the reply carries it.
fn watch_json(w: &Watch, quiet_ms: u64, ceiling_ms: u64) -> Value {
    let suggest = suggest_settle(w.last_change_ms, w.resolution_ms);
    json!({
        "measured": true,
        "settled": w.settled,
        "last_change_ms": w.last_change_ms,
        "first_change_ms": w.first_change_ms,
        "quiet_for_ms": w.quiet_for_ms,
        // How long the watching took is `settle_ms` at the top of the reply. Repeating it
        // here as a second name for one number is how two fields start disagreeing.
        "samples": w.samples,
        "resolution_ms": w.resolution_ms,
        "quiet_ms": quiet_ms,
        "max_settle_ms": ceiling_ms,
        "suggest_settle_ms": suggest,
        "note": if w.settled {
            format!(
                "the screen last changed {}ms after the press and then held still. Put \
                 settle_ms: {suggest} on this button in the profile, so the number is measured \
                 once here rather than guessed by every caller. 'Settled' means 'held still for \
                 {quiet_ms}ms' - an application that pauses longer than that between repaints \
                 is called settled during the pause, so raise quiet_ms where a screen is known \
                 to arrive in stages.",
                w.last_change_ms
            )
        } else {
            format!(
                "the screen never held still for {quiet_ms}ms within {ceiling_ms}ms, so this is \
                 NOT a settle time - something on it is animating. A blinking cursor, a clock or \
                 a spinner does this; pass ignore=x,y,w,h to drop that rectangle from the \
                 comparison and measure again. last_change_ms is where watching stopped, not \
                 where the screen stopped."
            )
        },
    })
}

/// Encode a frame as PNG and, if asked, keep a file too. Returns the metadata JSON with it.
fn deliver(
    state: &SharedState,
    frame: &Frame,
    method: &str,
    black: bool,
    region_name: &str,
    save: bool,
) -> Result<(Vec<u8>, Value), ApiError> {
    let png = frame.to_png().map_err(ApiError::internal)?;
    let mut meta = json!({
        "region": region_name,
        "rect": frame.source_rect,
        "width": frame.width(),
        "height": frame.height(),
        "scale": frame.scale,
        "method": method,
        "black": black,
        "bytes": png.len(),
    });
    if black {
        meta["hint"] = json!(BLACK_HINT);
    }
    if save {
        let path = captures::save(&state.captures_dir, region_name, &png).map_err(ApiError::internal)?;
        captures::cleanup(
            &state.captures_dir,
            state.config.captures.keep,
            state.config.captures.max_age_minutes,
        );
        let name = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
        meta["path"] = json!(path.to_string_lossy());
        meta["url"] = json!(format!("/captures/{name}"));
    }
    Ok((png, meta))
}

/// Draw the overlays, **onto the already-scaled image** — drawn first and then shrunk, the 1px
/// grid lines and the text smear.
///
/// Drawing order is stacking order: grid (bottom) → regions → buttons → mark and inset (top).
/// A crosshair drawn to check something, covered by something else, defeats the feature.
fn apply_overlay(
    frame: &mut Frame,
    ov: &Overlay,
    defs: Option<&Targets>,
    coord_scale: (f64, f64),
    client: (i32, i32),
    offsets: &AnchorOffsets,
) -> Value {
    let mut drawn = json!({});

    if let Some(step) = ov.grid
        && step > 0
    {
        draw::grid(
            &mut frame.image,
            (frame.source_rect[0], frame.source_rect[1]),
            step,
            frame.scale,
        );
        drawn["grid"] = json!(step);
    }

    if let Some(mode) = ov.mode()
        && let Some(t) = defs
    {
        let mut outside: Vec<String> = Vec::new();
        // Draw **all** the rectangles first and place the labels afterwards, so a label sits
        // on top of other rectangles and can avoid the labels already placed.
        let mut labels: Vec<(i32, i32, i32, i32, String, draw::Color)> = Vec::new();

        for (name, r) in &t.regions {
            let s = Targets::scale_rect(r.rect, coord_scale);
            // Finish the coordinate conversion first — calling `frame.map_len` while holding
            // `&mut frame.image` would borrow the same value mutably and immutably at once.
            let (x, y) = frame.map(s[0], s[1]);
            let (w, h) = (frame.map_len(s[2]), frame.map_len(s[3]));
            draw::marked_rect(&mut frame.image, x, y, w, h, draw::REGION);
            labels.push((x, y, w, h, name.clone(), draw::REGION));
        }

        // The number is the 1-based position in **`GET /buttons` order** (sorted by name).
        // Both walk the same BTreeMap, so that correspondence holds by itself — which is why
        // no legend table has to be sent, and why these two iterations diverging would be
        // quietly wrong.
        for (i, (name, tg)) in t.buttons.iter().enumerate() {
            let by = Targets::offset_for(offsets, &tg.anchor);
            let s = Targets::shift_rect(Targets::scale_rect(tg.rect, coord_scale), by);
            let (px, py) = Targets::click_point(tg, coord_scale, by);
            let off = px < 0 || py < 0 || px >= client.0 || py >= client.1;
            if off {
                outside.push(name.clone());
            }
            // Colour is what this is. Whether it can be reached is the fill: a rectangle whose
            // press point is off the window is drawn hollow. One channel each, so a confirm
            // button that has drifted off the window still reads as a confirm button — which
            // is the thing a single "something is wrong" colour used to take away.
            let color = if tg.confirm { draw::DANGER } else { draw::TARGET };
            let (x, y) = frame.map(s[0], s[1]);
            let (w, h) = (frame.map_len(s[2]), frame.map_len(s[3]));
            if off {
                draw::rect_outline(&mut frame.image, x, y, w, h, 2, color);
            } else {
                draw::marked_rect(&mut frame.image, x, y, w, h, color);
            }
            if mode == ButtonMode::Full {
                // Where the press actually lands — not necessarily the rectangle's centre (an
                // explicit `point`). Not drawn in box/num: this crosshair sits exactly on the
                // key's legend.
                let (cx, cy) = frame.map(px, py);
                draw::crosshair(&mut frame.image, cx, cy, 6);
            }
            let label = match mode {
                ButtonMode::Num => format!("{}", i + 1),
                _ if tg.confirm => format!("{name} (CONFIRM)"),
                _ => name.clone(),
            };
            labels.push((x, y, w, h, label, color));
        }

        let placed = place_labels(&mut frame.image, &labels);

        drawn["buttons"] = json!(t.buttons.len());
        drawn["regions"] = json!(t.regions.len());
        drawn["buttons_mode"] = json!(match mode {
            ButtonMode::Full => "full",
            ButtonMode::Box => "box",
            ButtonMode::Num => "num",
        });
        if mode == ButtonMode::Num {
            drawn["numbering"] = json!(
                "each number is the 1-based position of that button in GET /buttons \
                 (which is sorted by name), so no legend is needed"
            );
        }
        if placed.collided > 0 {
            // Do not leave overlaps silent. An overlapping label cannot be read, and the fact
            // that it cannot be read does not show in the picture — two names simply look like
            // one.
            drawn["labels_crowded"] = json!(placed.collided);
            drawn["labels_hint"] = json!(format!(
                "{} label(s) had no free spot and were drawn as their number instead. Ask for \
                 buttons=num to number them all, or capture a smaller region with scale>1.",
                placed.collided
            ));
        }
        if !outside.is_empty() {
            drawn["outside_client"] = json!(outside);
        }
    }

    if let Some([mx, my]) = ov.mark {
        let (x, y) = frame.map(mx, my);

        // Take the magnified inset **before** drawing the crosshair. The other way round, what
        // you see magnified is not the underlying picture but the 5px crosshair blown up 4x
        // (measured).
        let wanted = ov.inset.unwrap_or(3);
        let prepared = if wanted > 1 {
            let want_radius = frame.map_len(ov.inset_radius.unwrap_or(40)).max(8);
            let (iw, ih) = (frame.width() as i32, frame.height() as i32);
            match draw::fit_inset(iw, ih, want_radius, wanted) {
                Some((radius, factor)) => {
                    Some((draw::inset(&frame.image, x, y, radius, factor), radius, factor, want_radius, iw, ih))
                }
                None => {
                    let (iw, ih) = (frame.width() as i32, frame.height() as i32);
                    // Never omit it silently — if there is none, say there is none.
                    drawn["inset"] = json!(false);
                    drawn["inset_skipped"] = json!(format!(
                        "the {iw}x{ih} capture is too small to hold a magnified inset — \
                         capture a larger region, or drop `scale`"
                    ));
                    None
                }
            }
        } else {
            None
        };

        draw::crosshair(&mut frame.image, x, y, 24);
        draw::text(
            &mut frame.image,
            x + 10,
            y + 10,
            &format!("{mx},{my}"),
            1,
            draw::WHITE,
            Some([0, 0, 0, 200]),
        );

        if let Some((ins, radius, factor, want_radius, iw, ih)) = prepared {
            draw::paste_inset(&mut frame.image, &ins, (x, y));
            drawn["inset"] = json!({"factor": factor, "radius": radius});
            if factor < wanted || radius < want_radius {
                drawn["inset_adjusted"] = json!(format!(
                    "asked for {wanted}x at radius {want_radius}, drew {factor}x at radius \
                     {radius} to fit the {iw}x{ih} image"
                ));
            }
        }
        drawn["mark"] = json!([mx, my]);
    }

    drawn
}

fn window_json(info: &WindowInfo) -> Value {
    json!({
        "title": info.title,
        "class": info.class,
        "pid": info.pid,
        "client": [info.client_size.0, info.client_size.1],
        "client_origin": [info.client_origin.0, info.client_origin.1],
        "window_rect": info.window_rect,
        "dpi": info.dpi,
        "minimized": info.minimized,
        "foreground": window::is_foreground(info.handle),
    })
}

// ────────────────────────────── handlers ──────────────────────────────

/// Answers only whether this is alive. Whitelist-exempt — it carries no information.
pub async fn ping() -> Response {
    json_ok(json!({"ok": true, "service": "deescreen", "version": env!("CARGO_PKG_VERSION"), "help": "/help"}))
}

/// Diagnose in one call whether this is in a state where it can act.
///
/// The failure modes that matter here — DPI scaling, UIPI, a locked session — all **work
/// wrongly without raising an error**. So rather than noticing after the
/// fact, this asks in advance.
pub async fn health(State(state): State<SharedState>) -> Response {
    let st = state.clone();
    let body = tokio::task::spawn_blocking(move || {
        let mut problems: Vec<String> = Vec::new();

        let interactive = crate::win::is_interactive_session();
        if !interactive {
            problems.push(
                "not running in an interactive desktop session — capture will be black and input \
                 will be ignored. Run deescreen as the logged-in user, not as a service."
                    .into(),
            );
        }
        if !st.dpi_aware {
            problems.push(
                "per-monitor DPI awareness was not applied by this process. If the display scale \
                 is not 100%, capture pixels and click coordinates may disagree."
                    .into(),
            );
        }

        let our = crate::win::own_elevation();

        // Diagnosed per profile — one can be fine while another's window is closed, and
        // blurring that together leaves "why does only that one fail" unanswerable.
        let mut per_profile = serde_json::Map::new();
        let snapshot = st.profiles.load();
        if snapshot.is_empty() {
            problems.push(
                "no profiles exist yet. A profile is one window plus its buttons and regions; \
                 nothing can be captured or pressed until one exists. Open /editor and use \
                 [＋ Profile] to create one."
                    .into(),
            );
        }
        for (name, prof) in snapshot.iter() {
            let t = prof.targets();
            let mut entry = json!({
                "description": t.description,
                // Give a count without saying where the names come from and the reader starts
                // inventing them. What to call next goes right here.
                "names": format!("/profiles?profile={name}"),
                "path": prof.path.to_string_lossy(),
                "buttons": t.buttons.len(),
                "regions": t.regions.len(),
                "keys": t.keys.len(),
                "reference_client": t.reference_client,
                "on_size_mismatch": t.on_size_mismatch,
                "window_spec": t.window,
                "anchors": t.anchors.len(),
            });
            if size_check_is_inert(&t) {
                problems.push(format!(
                    "profile '{name}': on_size_mismatch is \"reject\" but reference_client is not set, so nothing is checking the window size and every coordinate is used at whatever size the window happens to be. Open the window, read client_size from GET /window?profile={name}, and save it back as reference_client."
                ));
            }
            match bind_window(&t) {
                Err(e) => {
                    problems.push(format!("profile '{name}': {}", e.message));
                    entry["status"] = json!("unusable");
                    entry["error"] = json!(e.message);
                }
                Ok((info, _scale)) => {
                    let theirs = crate::win::process_elevation(info.pid);
                    let uipi_risk = our != crate::win::Elevation::Elevated
                        && theirs != crate::win::Elevation::Normal;
                    if uipi_risk {
                        problems.push(format!(
                            "profile '{name}': the target process looks {} while deescreen is {} \
                             — Windows UIPI will silently discard mouse and keyboard input.",
                            theirs.as_str(),
                            our.as_str()
                        ));
                    }
                    entry["window"] = window_json(&info);
                    if !t.anchors.is_empty() {
                        let found = crate::win::window::enumerate_controls(info.handle);
                        entry["anchors"] = match t.anchor_offsets(&found.items) {
                            Ok(offs) => {
                                let moved: Vec<String> = offs
                                    .iter()
                                    .filter(|(_, (dx, dy))| *dx != 0 || *dy != 0)
                                    .map(|(k, (dx, dy))| format!("{k} {dx:+},{dy:+}"))
                                    .collect();
                                if !moved.is_empty() {
                                    // Not a problem: it is being corrected. But a layout that
                                    // moves is worth saying out loud, because the same shift
                                    // is silently wrong for anything NOT anchored.
                                    log::info!(
                                        "profile '{name}': layout moved, corrected by anchor — {}",
                                        moved.join(", ")
                                    );
                                }
                                json!({
                                    "resolved": true,
                                    "offsets": offs.iter().map(|(k, (dx, dy))| (k.clone(), json!([dx, dy]))).collect::<serde_json::Map<_, _>>(),
                                    "moved": moved,
                                })
                            }
                            Err(e) => {
                                problems.push(format!("profile '{name}': {e}"));
                                json!({"resolved": false, "error": e})
                            }
                        };
                    }
                    entry["input"] = json!({
                        "our_elevation": our.as_str(),
                        "target_elevation": theirs.as_str(),
                        "uipi_risk": uipi_risk,
                    });
                    match wincap::capture_client(&info) {
                        Err(e) => {
                            problems.push(format!("profile '{name}': capture failed: {e}"));
                            entry["capture"] = json!({"ok": false, "error": e});
                            entry["status"] = json!("degraded");
                        }
                        Ok(shot) => {
                            if shot.black {
                                problems.push(format!("profile '{name}': {BLACK_HINT}"));
                            }
                            entry["capture"] = json!({
                                "ok": !shot.black,
                                "method": shot.method,
                                "black": shot.black,
                                "size": [shot.width, shot.height],
                            });
                            entry["status"] = json!(if shot.black { "degraded" } else { "ok" });
                        }
                    }
                }
            }
            per_profile.insert(name.clone(), entry);
        }

        json!({
            "status": if problems.is_empty() { "ok" } else { "degraded" },
            "problems": problems,
            "session": {"interactive": interactive},
            "home": {
                "dir": crate::config::home().dir.to_string_lossy(),
                "why": crate::config::home().kind.as_str(),
            },
            "dpi_aware": st.dpi_aware,
            "our_elevation": our.as_str(),
            "default_profile": st.effective_default(),
            "profiles": per_profile,
            "policy": {
                "allow_raw_clicks": st.config.allow_raw_clicks,
                "allow_raw_keys": st.config.allow_raw_keys,
                "allow_menus": st.config.allow_menus,
                "allow_profile_editing": st.config.allow_profile_editing,
                "admin_code_required": !st.config.admin_code.is_empty(),
                "default_settle_ms": st.config.default_settle_ms,
                "max_settle_ms": st.config.max_settle_ms,
                "default_hold_ms": st.config.default_hold_ms,
                "max_hold_ms": st.config.max_hold_ms,
            },
        })
    })
    .await
    .unwrap_or_else(|e| json!({"status": "degraded", "problems": [format!("health check panicked: {e}")]}));

    json_ok(body)
}

/// Visible top-level windows — **where you find the target window's title.**
pub async fn windows() -> Result<Response, ApiError> {
    let list = tokio::task::spawn_blocking(window::enumerate)
        .await
        .map_err(|e| ApiError::internal(format!("enumerate failed: {e}")))?;
    let items: Vec<Value> = list
        .iter()
        .map(|w| {
            json!({
                "title": w.title,
                "class": w.class,
                "pid": w.pid,
                "client": [w.client_size.0, w.client_size.1],
                "window_rect": w.window_rect,
                "dpi": w.dpi,
                "minimized": w.minimized,
            })
        })
        .collect();
    Ok(json_ok(json!({"count": items.len(), "windows": items})))
}

/// The configured window's current state.
pub async fn window_info(
    State(state): State<SharedState>,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Response, ApiError> {
    let prof = pick(&state, q_get(&q, "profile").as_deref(), Params::Query)?;
    let t = prof.targets();
    let (info, scale, warning) = tokio::task::spawn_blocking(move || bind_window_view(&t))
        .await
        .map_err(|e| ApiError::internal(format!("lookup failed: {e}")))??;
    let mut body = json!({
        "profile": prof.name,
        "window": window_json(&info),
        "coordinate_scale": [scale.0, scale.1],
    });
    if let Some(w) = warning {
        body["size_mismatch"] = json!(w);
    }
    // Whether this profile's coordinates can be placed on the window as it stands. The editor
    // asks here before it draws, because it draws its boxes from its own copy of the document
    // and would otherwise put them at uncorrected places over a perfectly real screenshot.
    if let Err(e) = anchor_offsets(&prof.targets(), &info) {
        body["anchor_unresolved"] = json!({"error": e.message, "detail": e.detail});
    }
    Ok(json_ok(body))
}

/// One profile's definition as JSON. Shared by `/profiles`, `/buttons` and `/regions` — build
/// the same thing in three places and one day the three disagree.
fn profile_view(prof: &crate::state::Profile) -> Value {
    defs_view(&prof.targets())
}

/// The definition itself, split out from the profile that holds it so the shape can be
/// asserted without a live profile — it is the shape, not the lookup, that consumers depend on.
fn defs_view(t: &Targets) -> Value {
    let buttons: Vec<Value> = t
        .buttons
        .iter()
        .map(|(name, b)| {
            json!({
                "name": name,
                "rect": b.rect,
                "point": b.point,
                "click_button": b.click_button,
                "double": b.double,
                "confirm": b.confirm,
                "settle_ms": b.settle_ms,
                "hold_ms": b.hold_ms,
                "anchor": b.anchor,
                "types": b.types,
                "shift_types": b.shift_types,
                "note": b.note,
            })
        })
        .collect();
    let regions: Vec<Value> = t
        .regions
        .iter()
        // Flat: name, rectangle, and the fields beside it. Writing the struct under "rect"
        // nests a rect inside a rect, which is what broke every reader when regions stopped
        // being bare arrays.
        .map(|(name, r)| json!({"name": name, "rect": r.rect, "anchor": r.anchor, "note": r.note}))
        .collect();
    let keys: Vec<Value> = t
        .keys
        .iter()
        .map(|(name, spec)| json!({"name": name, "keys": spec}))
        .collect();

    json!({
        "description": t.description,
        "window": t.window,
        "reference_client": t.reference_client,
        "on_size_mismatch": t.on_size_mismatch,
        "buttons": buttons,
        "regions": regions,
        // Which key reaches a button's second legend, and whether it latches. A caller that
        // wants to spell a string itself needs this as much as it needs the legends.
        "shift": t.shift,
        "keys": keys,
    })
}

/// `on_size_mismatch: "reject"` with no `reference_client` asks for a check that cannot run:
/// there is no measured size to compare the window against, so every coordinate is reused at
/// any window size in silence. That is the default state of a profile nobody pinned, and the
/// setting reads as protection, so it has to be said out loud rather than inferred from a null.
fn size_check_is_inert(t: &Targets) -> bool {
    t.reference_client.is_none() && t.on_size_mismatch == crate::targets::SizeMismatch::Reject
}

/// Server-wide policy — it does not vary per profile.
fn policy_json(state: &SharedState) -> Value {
    json!({
        "allow_raw_clicks": state.config.allow_raw_clicks,
        "allow_raw_keys": state.config.allow_raw_keys,
        "allow_menus": state.config.allow_menus,
        "allow_profile_editing": state.config.allow_profile_editing,
        "admin_code_required": !state.config.admin_code.is_empty(),
    })
}

/// **Every profile's definition at once.** With this, neither `/buttons` nor `/regions` is
/// needed.
///
/// `?profile=NAME` returns just that one. The editor reads its whole document through here.
pub async fn profiles(
    State(state): State<SharedState>,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Response, ApiError> {
    let wanted = q_get(&q, "profile");
    let mut out = serde_json::Map::new();
    match &wanted {
        Some(name) => {
            let prof = pick(&state, Some(name), Params::Query)?;
            out.insert(prof.name.clone(), profile_view(&prof));
        }
        None => {
            for (name, prof) in state.profiles.load().iter() {
                out.insert(name.clone(), profile_view(prof));
            }
        }
    }
    let mut body = json!({
        "default_profile": state.effective_default(),
        "profiles": out,
        "policy": policy_json(&state),
    });
    if let Some(name) = wanted {
        body["profile"] = json!(name);
    }
    Ok(json_ok(body))
}

/// What can be pressed — this list is the ceiling on capability.
/// (A subset of `/profiles`; smaller when only the buttons are needed.)
pub async fn list_buttons(
    State(state): State<SharedState>,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Response, ApiError> {
    let prof = pick(&state, q_get(&q, "profile").as_deref(), Params::Query)?;
    let view = profile_view(&prof);
    Ok(json_ok(json!({
        "profile": prof.name,
        "description": view["description"],
        "buttons": view["buttons"],
        "profiles": state.profile_names(),
        "default_profile": state.effective_default(),
    })))
}

/// The named regions that can be captured. The coordinates are **absolute to the window**, so
/// something spotted inside one can be used as a reference directly (see the cropped-coordinate
/// section of `/help`).
pub async fn list_regions(
    State(state): State<SharedState>,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Response, ApiError> {
    let prof = pick(&state, q_get(&q, "profile").as_deref(), Params::Query)?;
    let view = profile_view(&prof);
    Ok(json_ok(json!({
        "profile": prof.name,
        "description": view["description"],
        "regions": view["regions"],
        "note": "rect is [x, y, w, h] in window client coordinates; 'client' is reserved and \
                 means the whole client area",
        "profiles": state.profile_names(),
        "default_profile": state.effective_default(),
    })))
}

/// Capture — JSON response, including the server-side path.
pub async fn capture_json(State(state): State<SharedState>, body: Bytes) -> Result<Response, ApiError> {
    let req: CaptureReq = parse_body(&body)?;
    let (_png, meta, window) = do_capture(&state, req).await?;
    Ok(json_ok(json!({"capture": meta, "window": window})))
}

/// Capture — PNG bytes. `curl -o shot.png "…/capture.png?region=status_bar&scale=0.5"`
pub async fn capture_png(
    State(state): State<SharedState>,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Response, ApiError> {
    let req = CaptureReq::from_query(&q)?;
    let (png, meta, _window) = do_capture(&state, req).await?;
    Ok(png_response(png, &meta))
}

async fn do_capture(state: &SharedState, req: CaptureReq) -> Result<(Vec<u8>, Value, Value), ApiError> {
    let prof = pick(state, req.profile.as_deref(), req.params)?;
    let st = state.clone();
    tokio::task::spawn_blocking(move || {
        let t = prof.targets();
        capture_with(&st, &prof.name, &t, req, None)
    })
    .await
    .map_err(|e| ApiError::internal(format!("capture task failed: {e}")))?
}

/// The shared path that produces one capture — `/capture*` and `/preview.png` both run it.
///
/// `defs` is the definition to draw as an overlay. `None` uses whatever is loaded;
/// `/preview.png` passes **an unsaved candidate**. That is what keeps checking ahead of
/// applying, always.
fn capture_with(
    st: &SharedState,
    profile: &str,
    live: &Targets,
    req: CaptureReq,
    defs: Option<&Targets>,
) -> Result<(Vec<u8>, Value, Value), ApiError> {
    let defs_for_lookup = defs.unwrap_or(live);
    let (info, coord_scale, size_warning) = bind_window_view(defs_for_lookup)?;
    // Resolved, but not insisted on yet. Which of the two things below actually needs it
    // decides whether a failure here is fatal to this request.
    let resolved = anchor_offsets(defs_for_lookup, &info);
    let no_offsets = AnchorOffsets::new();

    let region_name = req.region.clone().unwrap_or_else(|| "@client".to_string());
    // `pad` grows the region on every side. A toggle's lamp usually sits just outside its
    // button, so `region=button:NAME&pad=25` is what you actually want to look at — and doing
    // that arithmetic in the caller means doing it against numbers the server already holds.
    let pad = req.pad.unwrap_or(0).max(0);
    let rect = match req.rect {
        Some(r) => Targets::pad_rect(r, pad),
        None => {
            // A named region is placed by its anchor, so without one its rectangle is a guess.
            // `@client` is the window itself and needs nothing.
            let offsets = if region_name == "@client" {
                &no_offsets
            } else {
                resolved.as_ref().map_err(Clone::clone)?
            };
            Targets::scale_rect(
                Targets::pad_rect(
                    defs_for_lookup.region(&region_name, info.client_size, offsets).map_err(|e| {
                        ApiError::bad_request(e)
                            .with_detail(json!({"known_regions": defs_for_lookup.region_names()}))
                    })?,
                    pad,
                ),
                coord_scale,
            )
        }
    };

    let (mut frame, method, black) = shoot(&info, rect, req.scale, req.max_width)?;

    let ov_mark = req.overlay.mark;
    let mut overlay = req.overlay.clone();
    // /preview.png is called in order to draw — do not make every caller add buttons=1.
    if defs.is_some() && overlay.buttons.is_none() {
        overlay.buttons = Some(ButtonsOpt::On(true));
    }
    let drawn = if overlay.is_empty() {
        Value::Null
    } else {
        // Boxes drawn at uncorrected coordinates would sit on the wrong keys and look
        // authoritative doing it, so this is the half that still refuses.
        let offsets = resolved.as_ref().map_err(Clone::clone)?;
        apply_overlay(&mut frame, &overlay, Some(defs_for_lookup), coord_scale, info.client_size, offsets)
    };

    // ?mark=x,y means "let me check this coordinate before pressing". If so, what sits under
    // that point should be answered too — more certain than picking the picture apart by eye,
    // and obtainable without pressing anything.
    let mark_hit = ov_mark.and_then(|[mx, my]| window::control_at(info.handle, mx, my));

    let what = if defs.is_some() {
        "preview"
    } else if req.rect.is_some() {
        "rect"
    } else {
        region_name.as_str()
    };
    // With several profiles, the file name has to say which window a capture belongs to.
    let label = format!("{profile}_{}", safe_label(what));
    let (png, mut meta) = deliver(st, &frame, method, black, &label, req.save.unwrap_or(true))?;
    meta["profile"] = json!(profile);
    if !drawn.is_null() {
        meta["overlay"] = drawn;
    }
    if let Some(h) = &mark_hit {
        meta["hit"] = hit_json(h);
    }
    if let Some(w) = size_warning {
        meta["size_mismatch"] = json!(w);
    }
    // The picture was allowed through without it, so the picture has to carry the fact. A
    // capture that looks identical to a healthy one, while every saved coordinate for this
    // profile is unusable, is the quiet wrongness anchors exist to remove.
    if let Err(e) = &resolved {
        meta["anchor_unresolved"] = json!({
            "error": e.message,
            "detail": e.detail,
            "note": "the screen is real, but nothing placed by an anchor is. Saved regions and \
                     button rectangles cannot be located on this window, so do not press by \
                     name and do not trust an overlay drawn from these coordinates. Often this \
                     means a different build of the application is running than the one this \
                     profile was measured against.",
        });
    }
    Ok((png, meta, window_json(&info)))
}

/// Draw a candidate definition over the current screen and return it, **saving nothing**.
///
/// The essential ordering of checking lives here: render after saving and it went live before
/// anyone checked. Taking the candidate in the body lets a person look at the picture and then
/// decide whether to apply it. It stores nothing and presses nothing, so it is a read path.
pub async fn preview_png(
    State(state): State<SharedState>,
    Query(q): Query<HashMap<String, String>>,
    body: Bytes,
) -> Result<Response, ApiError> {
    if body.is_empty() {
        return Err(ApiError::bad_request(
            "POST /preview.png needs a candidate profile document as its body",
        )
        .with_detail(json!({
            "hint": "send the same shape as a profile file; nothing is saved",
        })));
    }
    let candidate: Targets = serde_json::from_str(
        std::str::from_utf8(&body).map_err(|_| ApiError::bad_request("body must be UTF-8 JSON"))?,
    )
    .map_err(|e| ApiError::new(StatusCode::UNPROCESSABLE_ENTITY, format!("invalid profile document: {e}")))?;
    // Validation happens here too, so an agent filters itself out before involving a person.
    candidate
        .validate()
        .map_err(|e| ApiError::new(StatusCode::UNPROCESSABLE_ENTITY, e))?;

    let mut req = CaptureReq::from_query(&q)?;
    // A preview is a picture for checking, not a record — it does not save by default.
    if req.save.is_none() {
        req.save = Some(false);
    }

    let prof = pick(&state, req.profile.clone().as_deref(), Params::Query)?;
    let st = state.clone();
    let (png, meta, _win) = tokio::task::spawn_blocking(move || {
        let live = prof.targets();
        capture_with(&st, &prof.name, &live, req, Some(&candidate))
    })
    .await
    .map_err(|e| ApiError::internal(format!("preview task failed: {e}")))??;
    Ok(png_response(png, &meta))
}

/// Turn a spelled sequence into the shape the reply carries.
fn spelled_json(t: &Targets, seq: &[crate::targets::Spelled], text: &str) -> Value {
    let folded: Vec<&str> =
        seq.iter().filter(|s| s.case_folded).map(|s| s.enters.as_str()).collect();
    let mut v = json!({
        "text": text,
        "buttons": seq.iter().map(|s| s.button.clone()).collect::<Vec<_>>(),
        "presses": seq.len(),
        "steps": seq.iter().map(|s| json!({
            "button": s.button,
            "enters": s.enters,
            "shift": s.is_shift,
        })).collect::<Vec<_>>(),
    });
    if let Some(s) = &t.shift {
        v["shift"] = json!({
            "button": s.button,
            "mode": match s.mode {
                crate::targets::ShiftMode::Oneshot => "oneshot",
                crate::targets::ShiftMode::Toggle => "toggle",
            },
            "presses": seq.iter().filter(|p| p.is_shift).count(),
        });
    }
    if !folded.is_empty() {
        // The string that goes in is not character-for-character the string that was sent, and
        // that is worth one line rather than a surprise in the input display.
        v["case_folded"] = json!(folded);
        v["case_folded_note"] = json!(
            "these characters were matched on a key of the other case — a keypad is upper \
             case, so 'g91' is spelled with the G key. The characters entered are the key's, \
             not the ones you sent."
        );
    }
    v
}

/// Refusal for a string this keypad cannot enter — **every** character, not the first.
///
/// One at a time would mean a round trip per character, and the round trips in between are
/// presses on a machine. It also names what the keypad *can* enter, because the usual cause
/// is a character that simply is not on this panel.
fn unspellable(t: &Targets, text: &str, bad: &[crate::targets::Unspellable]) -> ApiError {
    let mut legends: Vec<String> = t
        .buttons
        .values()
        .flat_map(|b| [b.types.clone(), b.shift_types.clone()])
        .filter(|s| !s.is_empty())
        .collect();
    legends.sort();
    ApiError::bad_request(format!(
        "{} character(s) of {text:?} have no key on this keypad",
        bad.len()
    ))
    .with_detail(json!({
        "text": text,
        "unspellable": bad.iter().map(|u| json!({"at": u.at, "character": u.character})).collect::<Vec<_>>(),
        "can_enter": legends,
        "note": if legends.is_empty() {
            "this profile records no key legends at all. Put \"types\" on the keys that enter \
             characters (and \"shift_types\" for the second legend), or press them by name with \
             \"buttons\": [...]."
        } else {
            "nothing was pressed. A partial entry left in the machine is worse than none — \
             press CYCLE START after one and an unintended block runs — so the whole string is \
             refused rather than the part of it that could be spelled."
        },
    }))
}

/// **How this keypad would spell a string — pressing nothing.**
///
/// The point of it being a separate read is that a string can be checked before a machine
/// receives it. `POST /click` with `"spell"` presses the same sequence.
pub async fn spell(
    State(state): State<SharedState>,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Response, ApiError> {
    let prof = pick(&state, q_get(&q, "profile").as_deref(), Params::Query)?;
    let text = q_get(&q, "text").ok_or_else(|| {
        ApiError::bad_request("text is required — GET /spell?text=G91X0").with_detail(json!({
            "note": "this presses nothing; it answers which keys that string would press",
        }))
    })?;
    let t = prof.targets();
    match t.spell(&text) {
        Ok(seq) => {
            let mut v = spelled_json(&t, &seq, &text);
            v["profile"] = json!(prof.name);
            v["pressed"] = json!(false);
            v["note"] = json!(
                "nothing was pressed. Send the same string as POST /click {\"spell\": \"…\"} to \
                 press it, or the 'buttons' list above if you want to change it first."
            );
            Ok(json_ok(v))
        }
        Err(bad) => Err(unspellable(&t, &text, &bad)),
    }
}

// ────────────────────────── the contact sheet ──────────────────────────

/// Which order the cells come in.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum SheetOrder {
    /// As they sit on the panel. The sheet becomes a map of it and can be read a row at a time
    /// against the real thing.
    Screen,
    /// As `GET /buttons` lists them, which is by name. A family like `MDI_0`..`MDI_9` ends up
    /// adjacent, so the odd picture out is obvious without knowing the panel.
    Name,
}

impl SheetOrder {
    fn parse(s: Option<&str>) -> Result<SheetOrder, ApiError> {
        match s {
            None | Some("screen") => Ok(SheetOrder::Screen),
            Some("name") => Ok(SheetOrder::Name),
            Some(other) => Err(ApiError::bad_request(format!(
                "order must be 'screen' or 'name', got '{other}'"
            ))),
        }
    }
    fn as_str(self) -> &'static str {
        match self {
            SheetOrder::Screen => "screen",
            SheetOrder::Name => "name",
        }
    }
}

/// **One cropped picture per button, with its name under it.**
///
/// The overlay answers "are these coordinates right". This answers "is this the right *name*",
/// which the overlay cannot: printing 60 names next to 60 keys on one operator panel leaves no
/// room, the labels overlap, and two names on top of each other look like one.
///
/// The response is the sheet's shape and, importantly, which buttons had **no picture to show**
/// — those get a crossed-out cell rather than being left out, because a name missing from the
/// sheet is the one nobody checks.
pub async fn sheet_json(
    State(state): State<SharedState>,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Response, ApiError> {
    let (_png, meta) = do_sheet(&state, q).await?;
    Ok(json_ok(meta))
}

/// The sheet itself. `curl -o sheet.png "…/sheet.png?profile=NAME&region=operator_panel"`
pub async fn sheet_png(
    State(state): State<SharedState>,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Response, ApiError> {
    let (png, meta) = do_sheet(&state, q).await?;
    Ok(png_response(png, &meta))
}

async fn do_sheet(state: &SharedState, q: HashMap<String, String>) -> Result<(Vec<u8>, Value), ApiError> {
    let prof = pick(state, q_get(&q, "profile").as_deref(), Params::Query)?;
    let order = SheetOrder::parse(q_get(&q, "order").as_deref())?;
    let wanted: Option<Vec<String>> = q_get(&q, "buttons").map(|v| {
        v.split(',').map(|b| b.trim().to_string()).filter(|b| !b.is_empty()).collect()
    });
    let inside = q_get(&q, "region");
    let opt = sheet::Options {
        // Enough margin to see that a key is centred in its rectangle, which is how a 16px
        // drift shows up at a glance. At 0 an off-centre rectangle still looks like a key.
        pad: q_num(&q, "pad")?.unwrap_or(8).clamp(0, 200),
        cell: q_num(&q, "cell")?.unwrap_or(sheet::DEFAULT_CELL).clamp(16, 400),
        scale: q_num(&q, "scale")?.unwrap_or(1.0),
        heading: String::new(),
    };
    #[allow(clippy::neg_cmp_op_on_partial_ord)] // also catches NaN, which `scale=nan` produces
    if !(opt.scale > 0.0) || opt.scale > 8.0 {
        return Err(ApiError::bad_request(format!(
            "scale must be greater than 0 and at most 8, got {}",
            opt.scale
        )));
    }
    let save = q_bool(&q, "save")?.unwrap_or(true);

    let st = state.clone();
    tokio::task::spawn_blocking(move || sheet_now(&st, &prof, wanted, inside, order, opt, save))
        .await
        .map_err(|e| ApiError::internal(format!("sheet task failed: {e}")))?
}

fn sheet_now(
    state: &SharedState,
    prof: &crate::state::Profile,
    wanted: Option<Vec<String>>,
    inside: Option<String>,
    order: SheetOrder,
    mut opt: sheet::Options,
    save: bool,
) -> Result<(Vec<u8>, Value), ApiError> {
    let t = prof.targets();
    let (info, coord_scale, size_warning) = bind_window_view(&t)?;

    // Every cell on this sheet **is** an anchored rectangle, so unlike an ordinary capture
    // there is no part of the answer that survives the anchor not being found. Refusing is the
    // honest outcome: a grid of crossed-out boxes would look like 140 separate problems.
    let offsets = anchor_offsets(&t, &info)?;

    // Only the buttons asked for, and a name that is not there is said so rather than quietly
    // producing a shorter sheet.
    let mut chosen: Vec<(&String, &ButtonDef)> = match &wanted {
        None => t.buttons.iter().collect(),
        Some(names) => {
            let mut v = Vec::with_capacity(names.len());
            for n in names {
                match t.buttons.get_key_value(n) {
                    Some(kv) => v.push(kv),
                    None => {
                        return Err(ApiError::not_found(format!("no button named '{n}'"))
                            .with_detail(near_names(n, t.buttons.keys().cloned(), "GET /buttons")));
                    }
                }
            }
            v
        }
    };

    // `region=NAME` narrows the sheet to one part of the panel — which is the usual way to
    // look at 140 buttons, since they are checked a panel at a time.
    let mut within: Option<Rect> = None;
    if let Some(name) = &inside {
        let r = t.region(name, info.client_size, &offsets).map_err(|e| {
            ApiError::bad_request(e).with_detail(json!({"known_regions": t.region_names()}))
        })?;
        let r = Targets::scale_rect(r, coord_scale);
        within = Some(r);
        chosen.retain(|(_, b)| {
            let s = Targets::shift_rect(
                Targets::scale_rect(b.rect, coord_scale),
                Targets::offset_for(&offsets, &b.anchor),
            );
            // By the centre, not by containment: a key on a panel's edge is part of that panel
            // even where its rectangle, padded or not, runs a pixel past it.
            let (cx, cy) = (s[0] + s[2] / 2, s[1] + s[3] / 2);
            cx >= r[0] && cy >= r[1] && cx < r[0] + r[2] && cy < r[1] + r[3]
        });
        if chosen.is_empty() {
            return Err(ApiError::bad_request(format!(
                "no button's centre falls inside region '{name}' {r:?}"
            ))
            .with_detail(json!({
                "known_regions": t.region_names(),
                "note": "the region resolved and the buttons are placed; they simply do not \
                         overlap. GET /capture.png?region=NAME&buttons=box shows both.",
            })));
        }
    }

    let cells: Vec<sheet::Cell> = chosen
        .iter()
        .map(|(name, b)| sheet::Cell {
            name: (*name).clone(),
            rect: Targets::shift_rect(
                Targets::scale_rect(b.rect, coord_scale),
                Targets::offset_for(&offsets, &b.anchor),
            ),
            confirm: b.confirm,
        })
        .collect();
    let cells = match order {
        // Half the median key height: keys within a row differ by a few pixels, and rows are
        // at least a key apart. Derived rather than asked for, because a caller guessing it is
        // a caller guessing at the panel this tool has already measured.
        SheetOrder::Screen => {
            let mut hs: Vec<i32> = cells.iter().map(|c| c.rect[3]).collect();
            hs.sort_unstable();
            sheet::screen_order(cells, (hs[hs.len() / 2] / 2).max(4))
        }
        SheetOrder::Name => cells,
    };

    let shot = wincap::capture_client(&info).map_err(ApiError::internal)?;
    let full = captures::full_image(&shot).map_err(ApiError::internal)?;

    opt.heading = format!(
        "{} - {} BUTTONS - CLIENT {}X{} - {} ORDER",
        prof.name.to_ascii_uppercase(),
        cells.len(),
        info.client_size.0,
        info.client_size.1,
        order.as_str().to_ascii_uppercase(),
    );
    let built = sheet::build(&full, &cells, &opt).map_err(ApiError::bad_request)?;
    let png = captures::encode_png(&built.image).map_err(ApiError::internal)?;

    let mut meta = json!({
        "profile": prof.name,
        "buttons": cells.len(),
        "order": order.as_str(),
        "cols": built.cols,
        "rows": built.rows,
        "cell": built.cell,
        "pad": opt.pad,
        "scale": opt.scale,
        "width": built.image.width(),
        "height": built.image.height(),
        "bytes": png.len(),
        "method": shot.method,
        "black": shot.black,
        "note": "one cell per button, all cropped from a single capture. The name under a cell \
                 is what /click expects; the picture is what is actually there. A cell drawn as \
                 an empty crossed box has a rectangle with no pixels on this window.",
    });
    if let Some(r) = within {
        meta["region"] = json!(inside);
        meta["region_rect"] = json!(r);
    }
    if shot.black {
        meta["hint"] = json!(BLACK_HINT);
    }
    if !built.absent.is_empty() {
        meta["not_on_screen"] = json!(built.absent);
        meta["not_on_screen_hint"] = json!(
            "these rectangles are off the window entirely. Either the window is smaller than \
             the one they were measured on (POST /window/fit), or the profile is measured \
             against a different build (GET /controls, then re-measure in /editor)."
        );
    }
    if let Some(w) = size_warning {
        meta["size_mismatch"] = json!(w);
    }
    if save {
        let path = captures::save(&state.captures_dir, &format!("{}_sheet", safe_label(&prof.name)), &png)
            .map_err(ApiError::internal)?;
        captures::cleanup(
            &state.captures_dir,
            state.config.captures.keep,
            state.config.captures.max_age_minutes,
        );
        let name = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
        meta["path"] = json!(path.to_string_lossy());
        meta["url"] = json!(format!("/captures/{name}"));
    }
    Ok((png, meta))
}

/// The child controls inside the window — where this works, nothing is measured by eye.
///
/// **An empty list is not a failure but an answer**: that application does not split its
/// controls into windows, and the way forward is drawing them by hand in `/editor`. The
/// response says exactly that.
pub async fn controls(State(state): State<SharedState>, Query(q): Query<HashMap<String, String>>) -> Result<Response, ApiError> {
    let prof = pick(&state, q_get(&q, "profile").as_deref(), Params::Query)?;
    tokio::task::spawn_blocking(move || {
        let t = prof.targets();
        let info = find_window(&t)?;
        let list = window::enumerate_controls(info.handle);
        let items: Vec<Value> = list
            .items
            .iter()
            .map(|c| {
                json!({
                    "class": c.class,
                    "text": c.text,
                    "id": c.id,
                    "rect": c.rect,
                    "depth": c.depth,
                    "visible": c.visible,
                })
            })
            .collect();
        let note = if items.is_empty() {
            "no child windows — this application draws its controls itself (WPF/WinUI, or a \
             custom-painted operator panel). Measure the buttons by hand in /editor; that is \
             the expected path, not a failure."
        } else {
            "rect is already in client coordinates and is the reliable part. 'text' is often \
             empty even on a real button, so this list tells you WHERE the controls are, not \
             what they do — read the legend off the screen at each rect \
             (capture.png?rect=x,y,w,h&scale=4) before naming it. Many entries are layout \
             containers rather than buttons: keep the ones that are visible, at least 6x6, and \
             under 40% of the client area, then check the survivors with buttons=1."
        };
        let mut body = json!({
            "count": items.len(),
            "note": note,
            "window": window_json(&info),
            "controls": items,
        });
        // Never leave a truncation silent. Build a profile from a cut list and buttons are
        // missing, which surfaces later only as "you told me to press it and there is no such
        // button".
        if list.total > items.len() {
            body["truncated"] = json!({
                "found": list.total,
                "returned": items.len(),
                "limit": window::CONTROL_LIMIT,
                "note": "this window has more child windows than one response carries. The ones \
                         you did not get are mostly deep inside lists and trees, but do not \
                         assume that — if a button you expect is missing, it may simply be past \
                         the cut. Measure that one by hand in /editor.",
            });
        }
        Ok(json_ok(body))
    })
    .await
    .map_err(|e| ApiError::internal(format!("controls task failed: {e}")))?
}

/// Return the profile document **exactly as `POST /admin/profile` accepts it**.
///
/// ## Why `/profiles` will not do
///
/// `/profiles` flattens into arrays for reading and iterating (`buttons: [{name, rect, ...}]`).
/// The saved shape is a map (`buttons: {name: {rect, ...}}`). So read-edit-write meant every
/// client hand-writing a converter between the two, and the rules for that (drop a field when
/// it holds the default, and so on) lived only inside the editor's JavaScript.
///
/// The real problem is ahead of us. **Add one field to a button and a hand-written converter
/// saves without it.** It disappears on the next save and nothing errors. Being able to send
/// back exactly what you received means there is no converter, and therefore nothing to lose.
///
/// `/profiles` stays as it is — this is not a replacement but one more endpoint, for round
/// trips.
pub async fn admin_get_profile(
    State(state): State<SharedState>,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Response, ApiError> {
    let prof = pick(&state, q_get(&q, "profile").as_deref(), Params::Query)?;
    let doc = serde_json::to_value(&*prof.targets())
        .map_err(|e| ApiError::internal(format!("could not serialize the profile: {e}")))?;
    Ok(json_ok(doc))
}

/// Buttons that carried `confirm` before and do not carry it after — whether the flag was
/// cleared or the whole button was removed. Both are the same act from a caller's side: the
/// name stops being protected, and so does any raw coordinate that lands inside it.
fn confirm_flags_lost(before: &Targets, after: &Targets) -> Vec<String> {
    before
        .buttons
        .iter()
        .filter(|(_, b)| b.confirm)
        .filter(|(name, _)| after.buttons.get(*name).is_none_or(|now| !now.confirm))
        .map(|(name, _)| name.clone())
        .collect()
}

/// Where each anchor's contents have to move, for this window as it is right now.
///
/// Enumerating controls costs a few milliseconds, so a profile with no anchors never pays it —
/// which is every profile that existed before anchors did.
fn anchor_offsets(t: &Targets, info: &crate::win::window::WindowInfo) -> Result<AnchorOffsets, ApiError> {
    if t.anchors.is_empty() {
        return Ok(AnchorOffsets::new());
    }
    let found = crate::win::window::enumerate_controls(info.handle);
    t.anchor_offsets(&found.items).map_err(|e| {
        ApiError::new(StatusCode::CONFLICT, e).with_detail(json!({
            "why": "this profile's coordinates are recorded relative to a control, and that \
                   control could not be identified on the window as it is now. Using the saved \
                   numbers uncorrected is what the anchor exists to prevent, so nothing was \
                   pressed or captured.",
            "hint": "GET /controls lists what is actually there, with text and size. Update the \
                     anchor's rect if the application changed, or anchor a container whose text \
                     and size are unique.",
            "controls_seen": found.items.len(),
        }))
    })
}

type AnchorOffsets = std::collections::HashMap<String, (i32, i32)>;

/// Whether the control under the press sits where the profile says it does.
///
/// Only a same-sized control in a different place says anything. That is a translation, which
/// is what a layout shift looks like: the application moved its panel and the coordinates in
/// the file are all off by one constant. A different size means the saved rectangle was drawn
/// around something rather than copied from it — common in a hand-made profile — so there is
/// no claim to check and this returns nothing rather than crying wolf.
fn aim_json(saved: Option<Rect>, hit: Option<&crate::win::window::ControlHit>) -> Option<Value> {
    let (saved, hit) = (saved?, hit?);
    if hit.is_window_itself {
        return None;
    }
    let live = hit.rect;
    if live[2] != saved[2] || live[3] != saved[3] {
        return None;
    }
    let (dx, dy) = (live[0] - saved[0], live[1] - saved[1]);
    if dx == 0 && dy == 0 {
        return Some(json!({"matches": true}));
    }
    Some(json!({
        "matches": false,
        "delta": [dx, dy],
        "saved": saved,
        "found": live,
        "note": "the control here is the same size as the saved rectangle but not in the same \
                 place, so this application has moved its layout without changing its window \
                 size. Every coordinate in this profile is off by that delta. Wide keys still \
                 take the press; narrow ones give it to a neighbour, which is why this can look \
                 like it works.",
    }))
}

/// What to say when a caller names something that is not there.
///
/// Listing every name was the obvious answer and it stops scaling: on a 140-button panel it is
/// 1.4 kB of response for a one-character typo, in a reply the caller has to read on every
/// mistake. And it does not actually help — the answer is somewhere inside that wall.
///
/// A wrong name is nearly always a near miss: a typo, or the wrong word for the right idea
/// (`MDI_DOT` for `MDI_PERIOD`). So the useful reply is the handful of names that are close,
/// with a count and where to get the whole list on the rare occasion it is really wanted.
fn near_names(wanted: &str, all: impl Iterator<Item = String>, list_at: &str) -> Value {
    let all: Vec<String> = all.collect();
    let lower = wanted.to_lowercase();

    let mut scored: Vec<(usize, &String)> = all
        .iter()
        .map(|n| {
            let nl = n.to_lowercase();
            // Case is the cheapest possible mistake, so it wins outright. Next, one name
            // containing the other beats a mere edit distance — MDI_9 against MDI_90 is a
            // better guess than any distance says — but only when the two are close in length.
            // Without that guard a one-letter name matches nearly everything: `Y` is inside
            // `cYcle_start`, and it kept turning up as a suggestion for it.
            let (short, long) = if nl.len() < lower.len() { (&nl, &lower) } else { (&lower, &nl) };
            let comparable = short.chars().count() >= 3 && short.len() * 2 >= long.len();
            let score = if nl == lower {
                0
            } else if comparable && long.contains(short.as_str()) {
                1
            } else {
                2 + edit_distance(&lower, &nl)
            };
            (score, n)
        })
        .filter(|(score, n)| *score <= 2 + (n.chars().count() / 2).max(2))
        .collect();
    scored.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(b.1)));

    let near: Vec<&String> = scored.iter().take(5).map(|(_, n)| *n).collect();
    let mut out = json!({ "known": all.len(), "all": list_at });
    if !near.is_empty() {
        out["did_you_mean"] = json!(near);
    }

    // Some misses are not typos at all — the right family, the wrong word for the thing
    // (`MDI_DOT` for `MDI_PERIOD`). No string distance finds that, but the prefix does: saying
    // how many names share it turns a dead end into one filtered look at the list.
    if let Some((prefix, _)) = wanted.rsplit_once('_') {
        let group = format!("{prefix}_");
        let n = all.iter().filter(|k| k.starts_with(&group)).count();
        if n > 0 && !near.iter().any(|k| k.eq_ignore_ascii_case(wanted)) {
            out["same_prefix"] = json!({ "prefix": group, "count": n });
        }
    }
    out
}

/// Levenshtein distance, one row at a time. These names are short, so the plain version is the
/// right amount of machinery.
fn edit_distance(a: &str, b: &str) -> usize {
    let b: Vec<char> = b.chars().collect();
    let mut row: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.chars().enumerate() {
        let mut prev = row[0];
        row[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let cost = usize::from(ca != *cb);
            let next = (row[j] + 1).min(row[j + 1] + 1).min(prev + cost);
            prev = row[j + 1];
            row[j + 1] = next;
        }
    }
    row[b.len()]
}

/// The gate that guards the gate.
///
/// A confirm flag is one person's judgement that pressing this needs a human. An edit endpoint
/// that can remove it silently makes that judgement advisory: refused at `/click`, patch the
/// flag off, press. Three calls and nobody was asked. So taking a flag away asks for exactly
/// what pressing the button would have asked for.
///
/// It catches the accidental case too, which is the likelier one — a round-trip POST that
/// re-types 140 buttons and drops a `true` somewhere would otherwise save and answer success.
fn allow_losing_confirm(
    q: &HashMap<String, String>,
    before: &Targets,
    after: &Targets,
) -> Result<Vec<String>, ApiError> {
    let lost = confirm_flags_lost(before, after);
    if lost.is_empty() || q_bool(q, "confirm")?.unwrap_or(false) {
        return Ok(lost);
    }
    Err(ApiError::forbidden(format!(
        "this would take the confirm flag off {}: {}. Nothing was written.",
        if lost.len() == 1 { "a button" } else { "buttons" },
        lost.join(", ")
    ))
    .with_detail(json!({
        "buttons": lost,
 "why": "a person marked those as needing a human before they are pressed. Removing the flag is the same decision as pressing one, so it asks the same way.",
 "hint": "resend with &confirm=true if removing it is part of the work you were asked to do. If you are here because a press was refused, this is not the way around that — the press itself takes \"confirm\": true, and leaves the flag where it is for whoever comes next.",
 "note": "dropping a button that had the flag counts too — a raw coordinate inside it stops being protected the moment the button is gone",
    })))
}

/// JSON Merge Patch, RFC 7386, applied to the saved document.
///
/// `null` means **remove this key** — the whole of the specification's delete syntax. Anything
/// else replaces the value at that key and leaves its siblings alone, recursively.
///
/// Merge patch is usually described as unable to *store* a null, which would make an optional
/// field unclearable. That does not apply here, because a profile never stores one: an unset
/// option is written by leaving the key out, so removing the key and clearing the value are
/// the same act. `removing_a_key_is_how_a_patch_clears_a_value` in targets.rs holds that.
fn merge_patch(target: &mut Value, patch: &Value) {
    let Value::Object(fields) = patch else {
        *target = patch.clone();
        return;
    };
    if !target.is_object() {
        *target = Value::Object(serde_json::Map::new());
    }
    let Some(obj) = target.as_object_mut() else { return };
    for (k, v) in fields {
        if v.is_null() {
            obj.remove(k);
        } else {
            merge_patch(obj.entry(k.clone()).or_insert(Value::Null), v);
        }
    }
}

/// Which names in one of the by-name maps came, went, or changed. A patch that turns out to
/// match what was already there has to be reported as the nothing it was, rather than as a
/// save — otherwise "it worked" and "it was already like that" look identical from here.
fn map_diff(before: Option<&Value>, after: Option<&Value>) -> Value {
    let empty = serde_json::Map::new();
    let b = before.and_then(Value::as_object).unwrap_or(&empty);
    let a = after.and_then(Value::as_object).unwrap_or(&empty);
    json!({
        "added": a.keys().filter(|k| !b.contains_key(*k)).cloned().collect::<Vec<_>>(),
        "removed": b.keys().filter(|k| !a.contains_key(*k)).cloned().collect::<Vec<_>>(),
        "modified": a.iter()
            .filter(|(k, v)| b.get(*k).is_some_and(|old| old != *v))
            .map(|(k, _)| k.clone())
            .collect::<Vec<_>>(),
    })
}

/// **Change one corner of a profile without resending it.** A 140-button document is around
/// 6,000 tokens; read-modify-write costs that twice to add a single button, and every one of
/// those re-typed rectangles is a chance to move a coordinate by a digit in a way that
/// validates, saves, and answers success. A patch touches only what it names.
///
/// Merge patch semantics (RFC 7386): `{"buttons": {"NEW": {...}, "OLD": null}}` adds one,
/// deletes one, and leaves every other button exactly as it was.
///
/// It does not create profiles — an unknown name is a 404, because `POST` is where creating
/// happens and a mistyped name must not quietly become a new empty profile.
pub async fn admin_patch_profile(
    State(state): State<SharedState>,
    Query(q): Query<HashMap<String, String>>,
    body: Bytes,
) -> Result<Response, ApiError> {
    editing_allowed(&state)?;
    let text = std::str::from_utf8(&body)
        .map_err(|_| ApiError::bad_request("body must be UTF-8 JSON"))?;
    let patch: Value = serde_json::from_str(text)
        .map_err(|e| ApiError::bad_request(format!("invalid patch: {e}")))?;
    let Some(fields) = patch.as_object() else {
        return Err(ApiError::bad_request(
            "a merge patch has to be a JSON object naming the parts to change",
        )
        .with_detail(json!({
            "example": {"buttons": {"NEW_KEY": {"rect": [10, 20, 30, 40]}, "GONE": Value::Null}},
        })));
    };
    if fields.is_empty() {
        return Err(ApiError::bad_request("the patch is empty — nothing would change"));
    }

    let prof = pick(&state, q_get(&q, "profile").as_deref(), Params::Query)?;
    let before = prof.targets();
    let before_doc = serde_json::to_value(&*before)
        .map_err(|e| ApiError::internal(format!("could not serialize the profile: {e}")))?;

    let mut doc = before_doc.clone();
    merge_patch(&mut doc, &patch);

    // The merged document goes through the same door a whole POST does: unknown fields are
    // refused, so a typo inside the patch cannot delete the field it meant to set.
    let fresh: Targets = serde_json::from_value(doc).map_err(|e| {
        ApiError::new(StatusCode::UNPROCESSABLE_ENTITY, format!("the patched profile is not valid: {e}"))
            .with_detail(json!({"note": "nothing was written — the profile on disk is unchanged"}))
    })?;
    fresh.validate().map_err(|e| {
        ApiError::new(StatusCode::UNPROCESSABLE_ENTITY, e)
            .with_detail(json!({"note": "nothing was written — the profile on disk is unchanged"}))
    })?;
    let after_doc = serde_json::to_value(&fresh)
        .map_err(|e| ApiError::internal(format!("could not serialize the patched profile: {e}")))?;

    // A patch that asks for what is already there must not be written. Saving would rotate a
    // good .bak away for no reason, and "saved" would be the wrong answer to "did anything
    // change".
    if after_doc == before_doc {
        return Ok(json_ok(json!({
            "saved": false,
            "changed": false,
            "profile": prof.name,
            "note": "the patch matches what the profile already says — nothing was written, and the backup file is untouched",
        })));
    }

    let lost_confirm = allow_losing_confirm(&q, &before, &fresh)?;
    let scalars: Vec<String> = ["description", "window", "reference_client", "on_size_mismatch"]
        .iter()
        .filter(|f| before_doc.get(**f) != after_doc.get(**f))
        .map(|f| (*f).to_string())
        .collect();
    let changed = json!({
        "buttons": map_diff(before_doc.get("buttons"), after_doc.get("buttons")),
        "regions": map_diff(before_doc.get("regions"), after_doc.get("regions")),
        "keys": map_diff(before_doc.get("keys"), after_doc.get("keys")),
        "fields": scalars,
    });

    let path = prof.path.clone();
    let saved = tokio::task::spawn_blocking(move || fresh.save(&path).map(|()| fresh))
        .await
        .map_err(|e| ApiError::internal(format!("save task failed: {e}")))?
        .map_err(|e| ApiError::new(StatusCode::UNPROCESSABLE_ENTITY, e))?;

    log::warn!(
        "profile '{}' PATCHED over HTTP ({}) — now {} buttons, {} regions, {} keys",
        prof.name,
        prof.path.display(),
        saved.buttons.len(),
        saved.regions.len(),
        saved.keys.len()
    );
    if !lost_confirm.is_empty() {
        log::warn!(
            "profile '{}': confirm flag REMOVED from {} — acknowledged with confirm=true",
            prof.name,
            lost_confirm.join(", ")
        );
    }
    let summary = json!({
        "saved": true,
        "changed": changed,
        "confirm_removed": lost_confirm,
        "profile": prof.name,
        "path": prof.path.to_string_lossy(),
        "backup": prof.path.with_extension("json.bak").to_string_lossy(),
        "buttons": saved.buttons.len(),
        "regions": saved.regions.len(),
        "keys": saved.keys.len(),
        "confirm_buttons": saved.buttons.values().filter(|t| t.confirm).count(),
        "size_check": if size_check_is_inert(&saved) {
            json!({
                "active": false,
                "why": "reference_client is not set, so on_size_mismatch \"reject\" has nothing to compare against and the window size is never checked",
                "fix": format!("read client_size from GET /window?profile={} and save it back as reference_client", prof.name),
            })
        } else {
            json!({"active": saved.reference_client.is_some(), "measured_at": saved.reference_client})
        },
    });
    prof.targets.store(std::sync::Arc::new(saved));
    Ok(json_ok(summary))
}

/// The editor writing a profile file. Requires `allow_profile_editing: true`, plus a matching
/// `admin_code` if one is configured (none configured means none is demanded — it started out
/// required and was removed once real use showed it produced only friction, 2026-08-26).
///
/// This endpoint being open means **the permission boundary has moved from file permissions to
/// HTTP reachability**. So it is closed by default, and the refusal says exactly that.
pub async fn admin_save_profile(
    State(state): State<SharedState>,
    Query(q): Query<HashMap<String, String>>,
    body: Bytes,
) -> Result<Response, ApiError> {
    editing_allowed(&state)?;
    // Parse and validate the body **first**. This used to create the profile file and then
    // look at the body, so a broken body left an unusable file behind (measured). Side effects
    // come after validation.
    let text = std::str::from_utf8(&body)
        .map_err(|_| ApiError::bad_request("body must be UTF-8 JSON"))?
        .to_string();
    let fresh: Targets = serde_json::from_str(&text).map_err(|e| {
        ApiError::new(StatusCode::UNPROCESSABLE_ENTITY, format!("invalid profile document: {e}"))
    })?;
    fresh
        .validate()
        .map_err(|e| ApiError::new(StatusCode::UNPROCESSABLE_ENTITY, e))?;

    // Saving under an unknown name **creates a profile** — requiring a config edit and a
    // restart to add one application would break the premise that this tool is not specific to
    // any one program. It widens no access (the whitelist is untouched).
    let asked = q_get(&q, "profile");
    let (prof, created) = match &asked {
        Some(name) if state.profile(Some(name)).is_err() => {
            if !crate::config::is_safe_profile_name(name) {
                return Err(ApiError::bad_request(format!(
                    "'{name}' is not a valid profile name — letters, digits, '-' and '_' only"
                )));
            }
            let path = crate::config::profile_path(name);
            if path.exists() {
                return Err(ApiError::conflict(format!(
                    "{} already exists but is not a loaded profile — POST /admin/reload to rescan",
                    path.display()
                )));
            }
            // The file is created by save() below. Not writing it here is the same rule: a
            // failed save has to leave no trace at all.
            let p = std::sync::Arc::new(crate::state::Profile {
                name: name.clone(),
                path,
                targets: arc_swap::ArcSwap::from(std::sync::Arc::new(fresh.clone())),
            });
            (p, true)
        }
        other => (pick(&state, other.as_deref(), Params::Query)?, false),
    };

    // A whole-document POST is where a confirm flag goes missing by accident: 140 buttons
    // re-typed, one `true` dropped, saved, "success". Same gate as the patch.
    let lost_confirm =
        if created { Vec::new() } else { allow_losing_confirm(&q, &prof.targets(), &fresh)? };
    if !lost_confirm.is_empty() {
        log::warn!(
            "profile '{}': confirm flag REMOVED from {} — acknowledged with confirm=true",
            prof.name,
            lost_confirm.join(", ")
        );
    }

    let path = prof.path.clone();
    let saved = tokio::task::spawn_blocking(move || fresh.save(&path).map(|()| fresh))
        .await
        .map_err(|e| ApiError::internal(format!("save task failed: {e}")))?
        .map_err(|e| ApiError::new(StatusCode::UNPROCESSABLE_ENTITY, e))?;

    log::warn!(
        "profile '{}' REWRITTEN over HTTP ({}) — {} buttons, {} regions, {} keys",
        prof.name,
        prof.path.display(),
        saved.buttons.len(),
        saved.regions.len(),
        saved.keys.len()
    );
    let summary = json!({
        "saved": true,
        "created": created,
        "profile": prof.name,
        "path": prof.path.to_string_lossy(),
        "backup": prof.path.with_extension("json.bak").to_string_lossy(),
        "buttons": saved.buttons.len(),
        "regions": saved.regions.len(),
        "keys": saved.keys.len(),
        "confirm_buttons": saved.buttons.values().filter(|t| t.confirm).count(),
        "confirm_removed": lost_confirm,
        "size_check": if size_check_is_inert(&saved) {
            json!({
                "active": false,
                "why": "reference_client is not set, so on_size_mismatch \"reject\" has nothing to compare against and the window size is never checked",
                "fix": format!("read client_size from GET /window?profile={} and save it back as reference_client", prof.name),
            })
        } else {
            json!({"active": saved.reference_client.is_some(), "measured_at": saved.reference_client})
        },
    });
    prof.targets.store(std::sync::Arc::new(saved));
    if created {
        // Only add it to the list once the file really exists.
        let mut map = (**state.profiles.load()).clone();
        map.insert(prof.name.clone(), prof.clone());
        state.profiles.store(std::sync::Arc::new(map));
        log::warn!("new profile '{}' created at {}", prof.name, prof.path.display());
    }
    Ok(json_ok(summary))
}

/// The manual for agents — **pure documentation. It carries no server state.**
///
/// This originally included the current window, the button list and the policy. Reading once
/// and acting immediately was convenient, but it made the document different on every call,
/// which left it neither a manual nor a status report. The roles are now split:
///
/// - `/help`    — **how** to use it. It does not change
/// - `/health`  — **what exists right now and whether it works**: profiles, window state, policy
/// - `/buttons` — one profile's button, region and key names
///
/// Which is why this text has to end by saying plainly what to call next. Without that, the
/// reader starts inventing endpoint names.
pub async fn help(headers: axum::http::HeaderMap) -> Response {
    let base = headers
        .get(axum::http::header::HOST)
        .and_then(|v| v.to_str().ok())
        .map(|h| format!("http://{h}"))
        .unwrap_or_else(|| "http://HOST:PORT".to_string());

    let mut hdrs = axum::http::HeaderMap::new();
    hdrs.insert(
        axum::http::header::CONTENT_TYPE,
        axum::http::HeaderValue::from_static("text/plain; charset=utf-8"),
    );
    (StatusCode::OK, hdrs, build_help(&base)).into_response()
}

fn build_help(base: &str) -> String {
    format!(
        r#"deescreen v{version} - drive a Windows GUI window over HTTP.

This page is a manual and does not change. For what exists on this server right now,
call:
  GET {base}/health      which profiles exist, which windows, is it working
  GET {base}/profiles    every profile's full definition - buttons, regions, keys, window
                         (?profile=NAME for just one)

COORDINATES
  Every coordinate is a pixel inside the TARGET WINDOW's client area - not the screen,
  not the monitor. (0,0) is the top-left of the window's content, below the title bar
  and any menu bar. Captures use the same coordinate system, so a pixel measured in a
  capture PNG can be sent straight back as a click.
  Moving the window or changing screen resolution does not shift these. Resizing the
  window does - that is what the size check below is for.

PROFILES
  One profile = one window plus its own coordinates, buttons, regions and keys. A
  different application is a different coordinate universe, so profiles do not share
  anything. Every window-facing call takes ?profile=NAME (or "profile" in the body).
  GET /health lists them with a human-written description each, and GET /profiles
  gives their full definitions. If a person names an application rather than a
  profile, match what they said against those descriptions and window titles - do
  not guess.

  Omitting profile works only when it is unambiguous: when the operator set a default,
  or when exactly one profile exists. With two or more and no default you get a 404
  listing the names - name one. Nothing picks alphabetically on your behalf, because
  then adding a profile would silently re-aim every call that left it out.

  If there are no profiles at all, nothing can be captured or pressed yet. That is not
  a fault: a person has to create one in the editor, where the window and its buttons
  are pointed at by hand. Say so and stop - there is no coordinate you can send that
  would work.

PRESS A SAVED BUTTON - click, wait for it to settle, re-capture, one round trip
  curl -s -X POST -o shot.png "{base}/click.png?button=NAME&capture=REGION&settle_ms=500"
  Then look at shot.png. Metadata rides back in the X-Deescreen-Meta header: how it was
  captured, whether the screen actually changed, byte size.

  JSON variant - no image in the body, a server-side path and /captures URL instead:
  curl -s -X POST -H "Content-Type: application/json" \
    -d '{{"button":"NAME","capture":"REGION","settle_ms":500}}' {base}/click

  settle_ms is how long to wait after the press before re-capturing. Left out it uses
  the server's default (500ms); ask for more than the server's ceiling and it is CLAMPED,
  not refused. Both numbers are in /health under "policy", and every reply reports the
  settle_ms actually used - so read that rather than assuming you got what you asked.

  Do not confuse it with hold_ms below. They are different halves of the same press:
    hold_ms    how long the key stays DOWN. Too short and the machine never sees the press.
    settle_ms  how long to wait AFTER, before looking. Too short and you photograph the
               screen from before it caught up, and read a stale screen as the result.
  Both fail the same way - quietly, returning something that looks like an answer.
  HAVE THE SERVER MEASURE settle_ms FOR YOU - "measure": true
  Finding the right settle_ms by pressing, guessing and looking is slow and the answer you
  reach is the smallest one that happened to work once. The server is on the same side of
  the screen and can simply watch it: it photographs the region repeatedly, compares each
  shot with the one before it, and reports when the changing stopped.
    curl -s -X POST -H "Content-Type: application/json" \
      -d '{{"button":"MONITOR","measure":true}}' {base}/click
  The reply gains a "settle" object. The field to read is suggest_settle_ms - that is the
  number to put on this button in the profile, so it is measured once here rather than
  guessed by every caller afterwards:
    "settle": {{"measured": true, "settled": true, "last_change_ms": 850,
                "quiet_for_ms": 310, "samples": 22,
                "resolution_ms": 53, "quiet_ms": 300, "suggest_settle_ms": 1100}}
    PATCH {{"buttons": {{"MONITOR": {{"settle_ms": 1100}}}}}}

  measure REPLACES the fixed wait - it does not happen after one. The request waits exactly
  as long as the watching took, and "settle_ms" in the reply is that real number. With no
  capture named it watches the whole client area, since a measurement is a comparison of
  pictures and needs one to compare.

  "SETTLED" MEANS "HELD STILL FOR quiet_ms" (300 by default). That is a definition, not an
  observation: an application that pauses longer than that between two repaints is called
  settled during the pause, and nothing outside the process can tell the difference. Where a
  screen is known to arrive in stages, raise it - "quiet_ms": 600.

  "settled": false means the screen NEVER held still, and then the numbers are not a settle
  time at all. Something is animating - a blinking cursor, a clock, a spinner. Pass
  ignore=x,y,w,h to drop that rectangle from the comparison and measure again. The reply
  says this outright rather than handing back the ceiling as though it were an answer.

  Measuring costs a capture every 50ms until the screen is still, which is why it is opt-in.
  Do it once per button that needs it, write the number down, and never do it again.


  HOW LONG THE KEY IS HELD DOWN - "hold_ms"
  A press is three things: the button goes down, time passes, the button comes up. hold_ms is
  that middle part, in milliseconds, and it decides whether a machine panel notices at all.

  An ordinary Win32 button reacts to any press however brief, because it latches on the way
  down. A simulated machine key does not: something reads that contact on a cycle, and a press
  that begins and ends between two reads never happened as far as the machine is concerned.
  Nothing moves. No alarm. No error. The reply looks exactly like a successful press, because
  from this side it was one - the input really was delivered.

  So when a key does nothing AND "hit" says you reached a real, enabled control, the press
  length is the first thing to change, not the coordinate:
    curl -s -X POST -H "Content-Type: application/json" \
      -d '{{"button":"CYCLE_START","hold_ms":300,"capture":"nc_display"}}' {base}/click
  Try the default, then 200, then 500. Every reply states the hold_ms it actually used, so
  compare that against what you asked for rather than assuming.

  Three places set it, nearest wins:
    "hold_ms" in the request       this one press - what to use while finding the number
    "hold_ms" on the button        that key alone, in the profile
    default_hold_ms in config.json the whole application - usually the right home, because
                                   the scan rate belongs to the program, not to one key
  /health reports the default and the ceiling under "policy". The ceiling exists because the
  press holds a lock on that window for its whole length.

  Once you find the number that works, say so - it belongs in the profile or the config, not
  in every future request. A caller that has to remember a magic number is a caller that will
  one day forget it.

  A LEFT SINGLE CLICK IS THE DEFAULT. "click_button": "right" (or "middle") and
  "double": true change that, on a saved button and on a raw coordinate alike. A button
  can carry either as its own default in the profile, so a context-menu button works
  without the caller having to know; passing it in the request overrides that.

PRESS SEVERAL BUTTONS IN ORDER - for keypads, where a half-entry is worse than none
  Entering data on an MDI keypad is one press per character. Send the sequence instead:

  curl -s -X POST -H "Content-Type: application/json" -d '{{
    "buttons": ["MDI_G","MDI_9","MDI_1","MDI_X","MDI_0","MDI_EOB","MDI_INSERT"],
    "gap_ms": 150, "capture": "hmi_display" }}' {base}/click

  This is not mainly about saving round trips. A partial string left in the machine is
  worse than no string at all - press CYCLE START after it and an unintended block runs.
  So:
    - Every name is resolved and checked BEFORE anything is pressed. A typo in element
      eight costs nothing instead of leaving seven characters in the machine.
    - A press that fails STOPS the sequence. "sequence.failed" gives the index and the
      button, and "pressed" lists what did go in, each with its own hit. Read a partial
      sequence as an unfinished entry: look at the screen before doing anything else.
    - A confirm button inside the array follows the same rule as a single press - refused
      unless the request carries "confirm": true. An array is not a way around it. Even
      then, prefer pressing that one on its own.
    - capture, ignore and settle_ms apply ONCE, after the last press. gap_ms is the pause
      between presses; it defaults to 500ms because a panel that drops input when pressed
      too fast fails silently, which is the failure this endpoint exists to prevent. Lower
      it once you have measured yours.
    - THE WAIT BEFORE THE CAPTURE IS LONGER FOR A SEQUENCE - 800ms rather than the server's
      single-press default. The last press of a sequence is nearly always the commit
      (INSERT, INPUT, CYCLE START) and a commit does more than a keystroke; capture too
      early and you get the screen from just before it, which still shows the entry sitting
      in the input line and reads exactly like a sequence that failed. Every reply says the
      settle_ms it actually used, so check that rather than assuming. If you know the real
      number for a commit button, put settle_ms on THAT BUTTON in the profile - measured
      once, right every time after, and better than any default here.
  "buttons" cannot be combined with "button", "rect" or "point". At most 200 per request.
  In the query form (/click.png) it is a comma-separated list: buttons=MDI_G,MDI_9.

  SPELL A STRING INSTEAD OF NAMING EVERY KEY - "spell"
  Working out that G91X0 is MDI_G, MDI_9, MDI_1, MDI_X, MDI_0 is work the server can do,
  and the one fact it needs - which key carries which character - is in the profile:
    "MDI_F": {{"rect": [...], "types": "F", "shift_types": "E"}}
  types is the legend printed on the key. shift_types is the SECOND legend, the small one
  above it, reached through the shift key. That E is on the F key used to live in an
  English sentence in "note", where nothing could read it and nobody could check it.

  Look first, pressing nothing:
    curl -s "{base}/spell?profile=NAME&text=G91X0"
  Then press it:
    curl -s -X POST -H "Content-Type: application/json" \
      -d '{{"spell":"G91X0","capture":"hmi_display"}}' {base}/click

  IT BECOMES AN ORDINARY SEQUENCE, so everything above applies unchanged: every key is
  resolved before anything is pressed, a failed press stops the rest, gap_ms spaces them,
  per_press says which one did nothing, and a confirm button inside still needs
  "confirm": true. The reply's "spelled" says what the string turned into, including the
  shift presses, which enter nothing and would otherwise look like stray keys.

  A CHARACTER WITH NO KEY REFUSES THE WHOLE STRING, and names EVERY such character rather
  than the first - fixing them one per round trip means pressing keys in between. Nothing
  is entered: a partial entry is worse than none.

  SHIFT IS DECLARED, NEVER ASSUMED. In the profile:
    "shift": {{"button": "MDI_SHIFT", "mode": "oneshot" | "toggle"}}
    oneshot  reaches the second legend for ONE key, then falls back by itself
    toggle   stays on until pressed again
  They are not interchangeable and the wrong one types a different string with no error -
  on a one-shot panel, a latch model spells "EE" as E then f. A profile that records a
  shifted legend without declaring this does not load. With toggle, spelling always turns
  it back off at the end, so a sequence never leaves the panel in a state the next caller
  did not ask for.

  Lower case is spelled on the upper-case key - a keypad is upper case, so "g91" works -
  and the reply says which characters were folded, because what went into the machine is
  then the key's character and not the one you sent.

  GET /buttons carries types and shift_types, and GET /profiles carries "shift", so a
  caller that would rather build the sequence itself has everything to do it with.

  WHICH PRESS WAS IGNORED - "per_press": true
  A sequence reports the screen after the LAST press, so a key the application quietly
  dropped halfway through is invisible: the end screen looks like a working one minus a
  character nobody counted. Sending the input succeeded, so nothing errors.
    curl -s -X POST -H "Content-Type: application/json" -d '{{
      "buttons": ["MDI_G","MDI_9","CURSOR_RIGHT","MDI_1"],
      "per_press": true }}' {base}/click
  Each entry in "pressed" gains its own "change", read between that press and the next:
    {{"index": 2, "button": "CURSOR_RIGHT", "hit": {{...}},
     "change": {{"changed": false, "pixels": 0, "bbox": null}}}}
  and "sequence.unchanged" lists the names outright, which is the answer you wanted.

  A PRESS THAT CHANGED NOTHING IS NOT A FAILED PRESS. A toggle already in that state, a key
  with no legend to repaint, a key ignored in the current mode and a key that never arrived
  all look identical from here, so "change" states what moved and passes no verdict. It
  tells you WHERE to look. Then: if that press's "hit" says a real, enabled control was
  reached, the press length is the next thing to change - see hold_ms above.

  It costs one capture per press and adds that time to the sequence, so it is opt-in. With
  no capture named it watches the whole client area. ignore=x,y,w,h applies here too - a
  blinking cursor otherwise makes every press look like it did something.

BUTTONS MARKED CONFIRM - the one thing in here that is about judgement
  Some buttons carry "confirm": true. Pressing one needs "confirm": true in the request
  as well, and without it you get 403.

  THIS IS NOT A REQUEST FOR PERMISSION. Nothing here contacts anybody and nothing waits
  for a reply. Resending with "confirm": true is the entire mechanism and it is yours to
  send - you are expected to run alone. What the flag buys is that the press cannot
  happen by ACCIDENT: not from a rectangle you computed, not swept up inside a sequence,
  not as a reflex after a refusal. Only as a second request that names the button on
  purpose.

  So spend the gap on the thing it is there for. GET /buttons gives "confirm" and "note"
  for every button before you press anything; the note is a person telling you what that
  button does. Read it, then answer one question:

      Is this press part of the work I was actually asked to do?

    yes - press it. Say afterwards that you did, and why.
    no, it is my own idea of a shortcut to something else - this is the case the flag
        exists for. Do not press it. Find another route, or come back and say what you
        found and what you think it needs.

  The test is whether the button is inside your task, not whether somebody is watching.
  And the note is better information than the screen: it was written by someone standing
  at a machine you cannot see.

  Raw coordinates do not get you around it. A rect or point that lands inside a saved
  confirm button is refused the same way, because the alternative is that the flag
  quietly stops meaning anything the moment you compute coordinates instead of using a
  name.

PRESS SOMETHING THAT IS NOT A SAVED BUTTON
  Some controls only exist on certain screens, so they cannot be in the saved list.
  Send the rectangle itself; the click lands in its centre, exactly as a saved button
  would, and nothing is stored.

  ASK BEFORE YOU MEASURE. GET /controls often hands you the rectangle outright, with
  the control's caption next to it - no pixels involved, so no misreading:
    curl -s "{base}/controls?profile=NAME"
  It works where controls are separate windows (Win32, MFC, WinForms) and returns an
  empty list where they are not (WPF, a single-bitmap HMI). An empty list is an answer,
  not a failure: it means this application must be read from the screen. Captions are
  often blank even when the rectangle is right, so treat position as the reliable part.

  Otherwise capture and work out where the control is.

  curl -s -X POST -H "Content-Type: application/json" \
    -d '{{"rect":[820,640,60,40]}}' {base}/click
  curl -s -X POST -o shot.png "{base}/click.png?rect=820,640,60,40&capture=@client"

  `point` works too when you do not know the size: {{"point":[850,660]}}.
  This needs allow_raw_clicks in config.json - /health reports whether it is on, and
  the refusal says so plainly.

  CHECK YOUR READING FIRST. This draws a crosshair and a magnified inset at a
  coordinate without touching anything:
  curl -s -o check.png "{base}/capture.png?mark=850,660&inset=4"
  A misread rectangle presses whatever is actually there, and that is silent.
  The X-Deescreen-Meta of that response carries "hit" - the control sitting at that
  exact point (class, text, its own rect). If hit.is_window_itself is true there is no
  control there and you are about to press background. hit.rect is the control's real
  rectangle, so it is also the answer when your coordinate is close but off-centre.

LOOK AT SOMETHING
  curl -s -o shot.png "{base}/capture.png?region=NAME"
  curl -s -o shot.png "{base}/capture.png?rect=0,940,1280,60"     ad-hoc rectangle
  curl -s -o shot.png "{base}/capture.png"                        whole client area
  Overlays for measuring: &grid=50 &mark=x,y &inset=4 &inset_radius=40 &buttons=1
  (inset_radius is how many client pixels around the mark get magnified; default 40)

  READING SMALL TEXT - scale magnifies, but only on a crop:
    curl -s -o shot.png "{base}/capture.png?rect=820,600,300,80&scale=4"
  scale<1 shrinks (any capture), scale>1 magnifies (needs region= or rect=, capped at
  8x and 4 megapixels, nearest-neighbour so the strokes stay crisp). Magnifying the
  whole client area is refused - crop first. max_width caps the output width either
  way, so &scale=8&max_width=1200 means "as big as fits in 1200px".

  &buttons= draws the saved buttons and regions over the capture, three ways:
    buttons=1     outline + name + crosshair at the click point
    buttons=box   outline + name, NO crosshair - the crosshair sits exactly on the key
                  legend, so use this when you are reading what a key says
    buttons=num   outline + a number. The number is that button's 1-based position in
                  GET /buttons (which is sorted by name), so no legend is needed. Use
                  this where buttons are packed too tightly for names to fit.
  Names that cannot be placed without covering a neighbour are drawn as their number
  instead, and the metadata says how many ("labels_crowded").
  CHECK THE NAMES, NOT JUST THE COORDINATES - the contact sheet
  The overlay above answers "are these rectangles on the right keys". It cannot answer
  "is this the right NAME for this key": 60 names printed beside 60 keys do not fit, and
  two names drawn on top of each other look like one. The sheet is the same rectangles
  laid out as a LIST instead - one cell per button, its picture cropped from a single
  capture, its name underneath with nothing competing for the space.
    curl -s -o sheet.png "{base}/sheet.png?profile=NAME"
    curl -s -o sheet.png "{base}/sheet.png?profile=NAME&region=operator_panel&scale=2"
  Read it against the real panel a row at a time. A cell whose picture is not the key its
  name claims is a naming error - that is the only question this picture answers, and it
  answers it for every button at once.

    order=screen   default. Reading order on the panel, top to bottom and left to right,
                   so the sheet is a map of it. Rows are worked out from the buttons' own
                   heights, so a row of keys that are not pixel-aligned stays one row.
    order=name     the GET /buttons order instead. Puts MDI_0..MDI_9 side by side, so the
                   odd picture out shows even if you have never seen the panel.
    buttons=A,B,C  only these. An unknown name is a 404 with the near ones, not a quietly
                   shorter sheet.
    region=NAME    only the buttons whose centre falls inside that region. This is the
                   usual way to look at a panel of 140.
    pad=8          context pixels kept around each rectangle (default 8). This is what
                   makes a drift visible: at pad=0 a rectangle sitting 16px off its key
                   still looks like a picture of a key.
    scale=2        magnify each crop - for softkeys whose legend is only 32px tall.
    cell=120       ceiling on one cell's picture. A whole-panel rectangle is shrunk to it
                   rather than setting the cell size for the other 139.
    save=false     do not keep a copy on the server.

  A BUTTON WITH NO PICTURE IS STILL A CELL - drawn as an empty crossed box and listed in
  "not_on_screen". It is never left out, because a name missing from the sheet is exactly
  the one nobody checks. If the ANCHOR cannot be resolved the sheet is refused instead:
  then every cell would be empty, and 140 empty cells look like 140 separate problems
  rather than the one they are.

  GET /sheet is the same sheet as JSON - its shape, where it was saved, and that list -
  for when you want the failures without reading them off a picture.

  CAPTURE A BUTTON, WITH A MARGIN. A toggle's state is usually NOT inside its button - on
  an operator panel the lamp sits just above the key. Do not read the rect from
  GET /buttons and add a margin yourself; name the button and say how much:
    curl -s -o shot.png "{base}/capture.png?region=button:OPT_STOP&pad=25"
    curl -s -X POST -o shot.png "{base}/click.png?button=OPT_STOP&capture=button&pad=25"
  pad grows the rectangle on every side, in the same client pixels as the numbers in the
  profile. Where that runs past the window edge the picture is CUT there, not slid across:
  you get the margin there was room for on that side, and the full margin on the others.
  Measured, on a button 22 wide sitting 22 from the left edge, with pad=200:
    asked for   [-178, y-200, 422, h+400]      22 - 200 = -178, and 22 + 2x200 = 422
    came back   [   0, y-200, 244, h+400]      178 columns were outside, so 244 remain
                                               left margin 22 + button 22 + right margin 200
  So near an edge the button is NOT in the middle of the image. Work out where it is from
  the "rect" in the metadata, which is the crop's own origin, against the button's rect from
  GET /buttons - do not assume it is centred, and do not predict the width without
  subtracting what falls outside. On a click,
  `capture=button` with no name means the button you just pressed (the last one, for a
  sequence), so you do not have to write it twice.

  NAMES NEVER START WITH "@". That prefix is reserved for values the server defines, so a
  name can never collide with one: "@client" is the whole client area, "@fixed" is an element
  that does not move with an anchor. Reserving the prefix rather than individual words means
  the next such value costs nobody a rename.

  Region names and their absolute rects come from GET /regions (or /profiles).
  "@client" is reserved and means the whole client area. A capture attached to a click
  costs two renders (before and after), so ask for one when you intend to look.

  A CROPPED CAPTURE STARTS AT ITS OWN (0,0), NOT THE WINDOW'S.
  If you read a pixel off a region capture and send it back as a click, it is wrong by
  the crop offset - silently. Two ways to be right:

  1. Ask for a grid. Its labels are WINDOW coordinates even on a crop, so the number
     you read is the number you send. No arithmetic:
       curl -s -o shot.png "{base}/capture.png?region=status_bar&grid=20"

  2. Or convert, using the X-Deescreen-Meta header of that same response, which
     carries the crop rect and the scale that were applied:
       window_x = rect[0] + png_x / scale
       window_y = rect[1] + png_y / scale

  Capturing "@client" (or omitting the region) needs no conversion at all - that image
  already is the window's coordinate system.

ENDPOINTS
  GET  /  ·  /help     this page. It never changes; /health is what changes
  GET  /health         is it operable right now - check this first when anything fails.
                       "policy" also carries the timing defaults and ceilings this server
                       runs with - default_settle_ms, max_settle_ms, default_hold_ms,
                       max_hold_ms. A setting absent from that PC's config.json is written
                       into it at startup with the value in force, so the file always lists
                       everything there is to tune; only allowed_ips_read and
                       allowed_ips_write have no default, because who may reach and who may
                       control is not something this program will guess.
                       For a profile with anchors, "anchors" says whether they resolve on
                       the window as it is now and how far each has moved - the answer to
                       "are the coordinates in this profile usable right now", before
                       anything is pressed. An anchor that cannot be found is listed in
                       "problems" as well.
                       "home" is where config.json, profiles/, captures/ and logs/ live on
                       that PC, and why that directory was chosen - the answer when a person
                       asks where their settings are. Moving them is theirs to do, not
                       yours: the DEESCREEN_HOME environment variable, then a restart.
                       "status" is ok or degraded and "problems" lists what is wrong, in
                       plain language - a locked session, a profile that failed to parse,
                       a window that is not open. "policy" says which of the gated things
                       this server allows, including whether an admin code is required
  GET  /profiles       every profile's full definition; one call tells you everything
  GET  /buttons        just the button list of one profile (subset of /profiles)
  GET  /regions        just the region list of one profile (subset of /profiles)
  GET  /sheet.png      one cropped picture per button with its name under it. The way to
                       check that a name belongs to the key it is on, which an overlay
                       cannot show for a whole panel. See "the contact sheet" above
  GET  /sheet          the same sheet as JSON - its shape, where it was saved, and which
                       buttons had no picture to show
  GET  /menus          the window's own menu bar - paths, command ids, enabled/checked.
                       Presses nothing. An empty list is an answer
  POST /menu           {{path}} - pick one item from it. The ONLY name here that does not
                       come from the profile, so it needs allow_menus as well
  GET  /spell          ?text=G91X0 - which keys that string would press on this keypad.
                       Presses NOTHING; POST /click {{"spell": "..."}} presses it
  GET  /windows        every visible top-level window (to find a title)
  GET  /controls       every child control inside the target window, with its rectangle
                       in window coordinates and its caption. Rectangles you can read
                       instead of measure - see below.
  GET  /window         the configured window's current state
  GET  /capture.png    ?region= &rect=x,y,w,h &pad= &scale= &max_width= plus the overlays
                       above. region=button:NAME captures that button's own rectangle.
                       With &mark=x,y the metadata also carries "hit" for that point -
                       verify a coordinate without pressing anything.
  POST /capture        the same as /capture.png but the reply is JSON - the metadata plus
                       the server-side path and /captures URL, and no image in the body.
                       Use it when you want the picture kept and referred to rather than
                       read right now. Parameters go in the body
  POST /click          {{button | buttons[] | spell | rect | point, confirm, click_button,
                       double, hold_ms, measure, quiet_ms, per_press,
                       settle_ms, gap_ms, capture, pad, ignore}} - the reply carries "hit"
                       (what was under the point) and "change" (pixels + bbox). With
                       "buttons" it presses them in order and stops at the first failure
  POST /click.png      same, returns PNG bytes; parameters go in the query string
  POST /key            {{key}} - a saved key; the names are in GET /profiles under "keys"
                       (NOT in /buttons - that endpoint is buttons only). {{chord}} presses an
                       unnamed combination ("f1", "ctrl+alt+f1") and {{text}} types a string;
                       both need allow_raw_keys.
                       Takes {{capture, ignore, settle_ms, measure, quiet_ms, scale,
                       max_width}} too, so one call presses and shows you the result -
                       and "measure": true times this key's settle exactly as it does for
                       a click. There is no "hit" here -
                       a key has no coordinate, so "did it arrive" has no cheap answer;
                       check /health input.uipi_risk instead.
  POST /window/focus   raise it / un-minimize it
  POST /window/fit     restore the client area to the size the buttons were measured at
  POST /preview.png    draw a candidate profile document over the live screen, saving nothing
  GET  /admin/profile  the profile as a document, in EXACTLY the shape POST takes
  POST /admin/profile  replace that document (needs allow_profile_editing)
  PATCH /admin/profile change PART of it - send only what differs. See below
  DEL  /admin/profile  ?profile=NAME&confirm=true - delete it. The file is moved aside,
                       not erased, but only a person on that PC can put it back
  POST /admin/profile/rename ?profile=OLD&to=NEW - rename in place
  POST /admin/profile/refit  ?profile=NAME - re-seat every coordinate onto the window as
                       it is now, when a container CHANGED SIZE and an anchor's
                       translation is no longer enough. A proposal unless apply=true,
                       and it checks each moved button against a real control first
  POST /admin/reload   re-read the profile files from disk. POST /admin/profile already
                       takes effect immediately; this is for when a PERSON edited a file
                       on that PC or dropped a new one in. Without ?profile= it rescans
                       the folder, so a new profile appears without a restart.
  GET  /captures/{{name}} fetch a capture the server kept - but only for a while: the
                       oldest are deleted as new ones arrive, so a URL you set aside can
                       404 later. Download it when you get it. The JSON click/capture replies
                       give you the name and the URL; PNG replies you already have.
  GET  /ping           alive? The one endpoint no IP whitelist applies to. If everything
                       else returns 403 and this does not, you are simply not on the list.

THE /admin ENDPOINTS MAY ASK FOR A CODE
  Everything under /admin passes one more check when the operator set an admin_code.
  It travels as a header, on every /admin call:
    curl -s -H "X-Admin-Code: THECODE" "{base}/admin/profile?profile=NAME"
  Check "policy.admin_code_required" in GET /health before you start, rather than
  finding out at the save. Without the header you get 401 and the reply names it. Where
  the operator left admin_code empty nothing is demanded, and the IP whitelist is then
  the only thing in front of these endpoints. The code is not yours to guess or to
  brute force: if you do not have it, say so and ask the person who runs that PC.

CREATE A PROFILE FROM SCRATCH
  A person usually does this in /editor by drawing rectangles. You can do it too, if
  allow_profile_editing is on. POST with a ?profile= name that does not exist CREATES
  it - there is nothing to read first, so this is the one case where you do not start
  with GET.

  1. Find the window. GET /windows lists every visible top-level window with its title,
     class and client size. Match on what the person called the application, and keep
     the SHORTEST title fragment that still picks exactly one - titles often carry the
     open file name, so an exact match on today's title stops matching tomorrow.
     Check your fragment: how many windows contain it? If more than one, add "class".

  2. Create it. The name is yours to choose: letters, digits, '-' and '_' only, because
     it travels in URLs and capture file names.
       curl -s -X POST -H "Content-Type: application/json" \
         -d '{{"description": "the Mitsubishi CNC simulator the operator calls NC plus",
              "window": {{"title": "NC Trainer2", "title_exact": false, "class": ""}}}}' \
         "{base}/admin/profile?profile=nctrainer"
     WRITE THE DESCRIPTION IN PLAIN LANGUAGE. It is the only thing connecting what a
     person says ("use the NC simulator") to a profile named nctrainer. Leaving it
     empty means the next agent has to guess.

  3. Pin the size. GET /window?profile=nctrainer gives the client size right now; put it
     in reference_client and save again. Without it, coordinates measured today are
     reused at any other window size with no complaint.

  4. Get the rectangles. GET /controls?profile=nctrainer. On a WinForms panel this
     returns hundreds of entries and most are not buttons. What the editor keeps:
       visible, and                          - invisible ones cannot be clicked
       width >= 6 and height >= 6, and       - separators and hairlines
       area <= 40% of the client area        - those are panels and containers
     Expect to throw most of them away.

     THIS LIST TELLS YOU WHERE, NOT WHAT. "text" is empty on most custom-drawn buttons
     (21 of 128 keys on one FANUC panel are blank keys with no legend at all), so you
     cannot name a button from this response alone. Read the legend off the screen at
     that rectangle before you name it:
       curl -s -o key.png "{base}/capture.png?profile=nctrainer&rect=820,600,60,40&scale=6"
     If a "truncated" block comes back, the window had more child windows than one
     response carries and a button you expect may simply be past the cut.

  5. Name them and save the whole document back. Two names that are the same are
     refused, so pick carefully where an application repeats a legend - the MDI letter
     keys X/Y/Z and the axis-select buttons X/Y/Z are the same word for different
     things. Mark anything a person would have to undo by hand with "confirm": true.

     ADDING A CONFIRM FLAG IS YOURS TO DECIDE. It costs a caller one field and it is the
     right answer when you are unsure - a button you keep aiming at by mistake, or one
     whose legend you could not read. Say why in that button's "note", because nothing
     here records who set a flag: a month from now neither you nor a person can tell your
     caution apart from someone's hard requirement, and the note is the only place that
     difference can live. Adding one is not free either - taking it off later asks.

     RECORD WHAT A KEY ENTERS, IF IT ENTERS ANYTHING. On a keypad, put the character in
     "types" - the legend printed on the key:
       "MDI_G": {{"rect": [...], "types": "G"}}
     Where a key carries a SECOND legend reached through shift, that goes in
     "shift_types", and the document says which key reaches it - "shift" is a field of
     the PROFILE, beside "buttons", not of a button:
       "buttons": {{"MDI_F": {{"rect": [...], "types": "F", "shift_types": "E"}},
                   "MDI_SHIFT": {{"rect": [...]}}}},
       "shift":   {{"button": "MDI_SHIFT", "mode": "oneshot"}}
     This is the difference between "E is on the F key" being a sentence in a note that
     only a person can read, and being a fact the server can act on - with it, a whole
     string is entered by POST /click {{"spell": "G91X0"}} instead of by naming five keys.
     Two keys may not claim the same legend, and a shifted legend without "shift"
     declared will not load, so a half-recorded keypad is refused rather than surprising
     somebody later. See SPELL A STRING above for what mode means and why it is not
     guessed.

     A MENU ITEM CAN BE MARKED THE SAME WAY a button is. The menu is read off the window
     rather than written here, so the profile's say in it is which paths need a second
     look:
       "confirm_menus": ["File", "Tool/Set Machine Parameters"]
     Matched on whole path segments. See THE WINDOW'S MENU BAR below.

     TAKING ONE OFF IS A HEAVIER CALL. That flag is somebody's judgement about a machine
     you cannot see, and removing it changes this server for everyone who uses it after
     you, not just for the press in front of you. &confirm=true is yours to send - the
     gate is there so it cannot happen by accident, and the server logs it - but it
     should follow from the work you were asked to do, not from a refusal you would
     rather not have had. Flag looks wrong? Leaving it and saying so costs nothing.

  6. Verify before you trust it. You can look at a document BEFORE saving it - the same
     overlay, drawn from the body you send, storing nothing and touching no file:
       curl -s -X POST -H "Content-Type: application/json" --data-binary @p.json \
         -o check.png "{base}/preview.png?profile=nctrainer&buttons=1"
     It takes the capture parameters too (region, rect, scale, pad), and it validates the
     document, so a rectangle off the window is refused here rather than saved. After
     saving, the same picture comes from a plain capture:
       curl -s -o check.png "{base}/capture.png?profile=nctrainer&buttons=1"
     Every rectangle should sit on the control you named it after. Metadata reports
     outside_client for any that fell off the window.

EDITING A PROFILE FROM A PROGRAM
  Read it, change it, send it back - the same shape both ways:
    curl -s "{base}/admin/profile?profile=NAME" > p.json
    ...edit p.json...
    curl -s -X POST -H "Content-Type: application/json" \
      --data-binary @p.json "{base}/admin/profile?profile=NAME"

  Use THIS, not GET /profiles, when you intend to write back. /profiles flattens the
  document into arrays for reading (buttons: [{{name, rect, ...}}]); the saved shape is a
  map keyed by name (buttons: {{name: {{rect, ...}}}}). Converting by hand works today and
  silently drops whatever field this server gains tomorrow. Round-tripping the document
  has no converter, so it has nothing to lose.

  POST REPLACES THE WHOLE DOCUMENT. Anything you leave out is gone. Send back what you
  read, with your edits applied - not a fragment.

  CHANGING ONE THING? USE PATCH, NOT POST. A 140-button profile is around 6,000 tokens, so
  read-modify-write costs that twice to add a single button - and re-typing 140 rectangles
  is 140 chances to move a coordinate by one digit in a way that validates, saves, and
  answers success. PATCH sends only the difference:

    curl -s -X PATCH -H "Content-Type: application/json" \
      -d '{{"buttons": {{"NEW_KEY": {{"rect": [820,640,60,40]}}, "OLD_KEY": null}}}}' \
      "{base}/admin/profile?profile=NAME"

  It is a JSON merge patch (RFC 7386), and there are only three rules:
    - a value REPLACES what is at that key
    - an object MERGES into what is at that key, leaving its other fields alone. Patching a
      button's rect keeps that button's confirm and note
    - null PUTS THE KEY BACK THE WAY IT WAS BEFORE ANYONE SET IT. Nothing in a profile is
      ever stored as a literal null - an option that is not set is simply absent - so
      removing the key and clearing the value are the same act here:
        "reference_client": null   the size check goes back to unpinned
        "settle_ms": null          that button waits the server default again
        "point": null              back to pressing the centre of the rect
        "on_size_mismatch": null   back to "reject"
        "regions": null            empties that whole collection
  Everything not named is untouched. Fixing a size and adding a button are one line each:
    -d '{{"reference_client": [1280,1000]}}'
    -d '{{"regions": {{"alarm_bar": [0,940,1280,60]}}}}'

  To empty a collection send null, not {{}}. An empty object merges nothing, so
  {{"buttons": {{}}}} asks for no change at all - and the reply will say "changed": false
  rather than pretending it emptied anything.

  The reply says what actually changed - added, removed and modified names per collection.
  A patch that matches what the profile already said writes nothing and tells you so
  ("changed": false), so "it worked" and "it was already like that" never look the same.
  A typo inside the patch is refused like any other unknown field, and nothing is written.
  PATCH does not create profiles; an unknown name is a 404. POST is where creating happens.

  Profile files are strict JSON with no comments, so there is nothing in one that a
  round trip can quietly drop. Anything a person needs to record about a button goes in
  "description" or that button's "note", which are real fields and come back to you.

  Removing one button is just leaving it out of the document you send back. Removing
  the PROFILE is a different endpoint, and it asks first:
    curl -s -X DELETE "{base}/admin/profile?profile=NAME&confirm=true"
  Without confirm=true it refuses and tells you so. The file is moved aside as
  deescreen.<name>.json.deleted-<timestamp>, which nothing here ever overwrites or
  cleans up - but putting it back is a person's job on that PC, so treat delete as
  one-way from here.

  To rename, use the rename endpoint, NOT "save under a new name":
    curl -s -X POST "{base}/admin/profile/rename?profile=OLD&to=NEW"
  Saving under a new name COPIES. You would leave two profiles pointing at one window,
  and omitting ?profile= would stop working the moment there are two of them. Rename
  moves it, so neither happens. The old name then 404s with the known list - nothing is
  silently redirected.

  Neither one touches the profile that config.json names as default_profile: losing it
  would break every request that omits ?profile=, and only a config edit plus a restart
  could undo that. Both refuse with 409 and say so.

  A FIELD NAME THIS SERVER DOES NOT KNOW STOPS THE SAVE. Nothing is dropped quietly: the
  reply names the field and lists the ones that were expected, and no file is touched. The
  field most worth getting right is "confirm" - spelled wrong it would store the emergency
  stop with no confirmation required and answer "saved". Same for the config file, which
  refuses to start rather than run with a setting it did not understand.

  WHEN AN APPLICATION MOVES ITS OWN LAYOUT - ANCHORS
  Some programs put their panel in a slightly different place each time they start. The
  window is the same size, every control is the same size, and only the origin differs, so
  no check based on the window can see it. Measured on NC Trainer2 plus: every container
  moves 16px sideways between runs. A 44px key still takes the press; a 32px softkey hands
  it to its neighbour. Mostly it works, which is what makes it dangerous.

  An anchor records a control that the coordinates around it were measured from:

    "anchors": {{
      "screen": {{"text": "NC DISPLAY", "rect": [54, 92, 1104, 818]}}
    }},
    "regions": {{"nc_display": {{"rect": [54,92,1104,818], "anchor": "screen"}}}},
    "buttons": {{"SOFTKEY_01": {{"rect": [...], "anchor": "screen"}},
                "HEADER_TAB":  {{"rect": [...], "anchor": "@fixed"}}}}

  Before anything is pressed or captured, that control is found on the window as it is now
  and everything belonging to it moves by the difference. The file is never rewritten; only
  the reading of it changes.

  Matched on TEXT AND SIZE TOGETHER. Not the class - an MFC window carries its module's load
  address in it, so it differs every run. Not text alone - this panel has two containers
  called "OPERATION PANEL". Their sizes differ, and size is exactly what a translation leaves
  alone. And not "the nearest one to where it used to be", which uses the stale rectangle to
  find the thing that would prove it stale, and fails hardest when the drift is largest.

  DECLARING ONE ANCHOR MAKES THE WHOLE PROFILE ANSWER. Every button and region must then name
  an anchor or say "@fixed". There is no third state: an element that says nothing would stay
  behind while its neighbours move, and the ones that still work would hide it. A profile
  with no anchors at all is unaffected and needs none of this.

  If an anchor cannot be found, or two controls match its text and size, the request is
  refused - only for the elements belonging to that anchor. Using the saved numbers
  uncorrected is exactly what the anchor was added to prevent, so it is not a fallback.

  WRITING ONE. Both numbers come straight from GET /controls - copy the "text" and the
  "rect" of the container, unchanged. Pick one whose text and size are unique in that list;
  if the only candidates share both, there is nothing here that can tell them apart and the
  anchor will be refused as ambiguous rather than guessed at.

    curl -s "{base}/controls?profile=NAME" | ... find the container

  ADDING THEM TO A PROFILE THAT ALREADY HAS BUTTONS. Every element has to answer at once -
  a save with some of them still blank is refused - so this is one PATCH, not many:

    curl -s -X PATCH -H "Content-Type: application/json" -d '{{
      "anchors": {{"screen": {{"text": "NC DISPLAY", "rect": [54,92,1104,818]}}}},
      "buttons": {{"SOFTKEY_01": {{"anchor":"screen"}}, "SOFTKEY_02": {{"anchor":"screen"}}}},
      "regions": {{"nc_display": {{"anchor":"screen"}}}} }}' \
      "{base}/admin/profile?profile=NAME"

  Merge patch only touches what it names, so every rect and note stays as it was. Miss one
  button and the whole patch is refused with a count and the first few names - which is the
  point: a half-anchored profile is the one that goes wrong quietly.

  CHECK IT WITHOUT PRESSING ANYTHING. The overlay is drawn through the same correction the
  presses use, so if the boxes sit on the keys, the anchor is working:

    curl -s -o check.png "{base}/capture.png?profile=NAME&buttons=box"

  Do it once, then restart the application so the layout moves, and do it again. Only both
  together prove anything; one on its own may just be the layout that was measured.

  TAKING A CONFIRM FLAG OFF ASKS THE SAME WAY PRESSING WOULD. A save or patch that leaves
  a button without a "confirm" it used to have - flag cleared, or the whole button removed -
  is refused unless the request carries &confirm=true, and the refusal names the buttons.
  Nothing is written either way.

  Read that refusal carefully if you got here from a 403 at /click. Removing the flag and
  then pressing is three calls in which nobody was asked, and it leaves the button unprotected
  for everyone after you. It is not the way around a confirm - saying what you are about to
  press and letting a person answer is. When a person HAS asked for the flag to go, resend
  with &confirm=true; the reply lists what was removed and the server logs it.

  Two names that are the same are refused at parse time, not merged, so you can never
  send 128 buttons and save 127. Names travel in ?button=NAME, so characters that would
  be cut there (& = # ? % + / \ whitespace) are refused when saving. Letters, digits,
  '_', '-', '.' are always safe; non-ASCII letters are fine too.

TRAPS - these fail quietly or confusingly. Read once, save yourself an hour.
  403 confirm      A button marked CONFIRM needs "confirm": true in the request too, and
                   so does a raw coordinate that lands inside one. Not a fault and not a
                   permission slip - resending is yours to do. It is here so the press
                   cannot happen by accident. Read the button's note first; see BUTTONS
                   MARKED CONFIRM above.
  404 unknown name A button, region or key name that is not in the profile. The reply does
                   not list every name - on a big panel that is a wall of text for a typo -
                   it gives the closest few as "did_you_mean", how many exist, and where the
                   full list is.
                   Those guesses are spelling, so they find a slip and not a synonym: ask
                   for MDI_DOT when the panel calls it MDI_PERIOD and no distance connects
                   the two, so it will not be in the list. That case is what "same_prefix"
                   is for - it says how many names share MDI_ - and then one filtered read
                   of GET /buttons settles it.
  401 admin code   An /admin call on a server whose operator set an admin_code. Resend
                   with the "X-Admin-Code" header. You cannot obtain that value from
                   here - ask the person who runs that PC.
  409 size         The window is no longer the size the buttons were measured at, so
                   every coordinate would be off. POST /window/fit. If the window is the
                   one that is right and reference_client is the stale number, update
                   that field instead. A panel that genuinely stretches with its window
                   can set "on_size_mismatch": "scale", or "ignore" where the panel stays
                   pinned to the top-left; the default, "reject", is this 409.
  409 minimized    POST /window/focus.
  black capture    The console session is locked or RDP is disconnected. Nothing will
                   work until a human unlocks it. /health says so explicitly.
  layout moved      "aim" in the reply says matches:false with a delta. The control at that
                   point is the same SIZE as the rectangle in the profile but not in the same
                   PLACE, so the application has moved its layout without changing its window
                   size - and every coordinate in that profile is off by the same amount.
                   Nothing else notices this: the size check compares the window, which did
                   not change, and "hit" only asks whether a control is there. Wide keys still
                   take the press, narrow ones give it to a neighbour, so it looks like it
                   works right up until it does not. Fix it with anchors - see
                   WHEN AN APPLICATION MOVES ITS OWN LAYOUT above - or by re-measuring. A
                   profile with no "aim" in its reply has a rectangle that was
                   drawn by hand rather than copied from a control, so there is nothing to
                   compare and this stays silent.
  press too short   The click reached a real, enabled control - "hit" proves that - and the
                   application did nothing at all. A panel key is read by a scan, so a press
                   shorter than one scan interval never happened as far as the machine is
                   concerned. Resend with a longer "hold_ms" (try 200, then 500). This is not
                   the same as changed=false below: there the press was seen and ignored,
                   here it was never seen.
  a person at that PC
                   Input goes into the one real input stream that PC has: the pointer moves,
                   the window is brought to the front, and focus is taken. The press itself
                   cannot be knocked off target, and requests never overlap each other, but
                   nothing stops a person clicking between two of your presses. If someone is
                   working at that machine, a keypad sequence can come out with something
                   spliced into it. Reads (/health, /capture.png) are always safe.
  changed          DO NOT read this as "did my click work". It answers one question -
                   did these pixels move - and that is not the same question. Two
                   independent things are in play, and all four combinations happen:

                                     screen changed        screen identical
                     hit a control   it worked             blank key / toggle already
                                                           in that state / ignored in
                                                           this mode - ALL NORMAL
                     hit nothing     a clock or animation  the coordinate landed on
                                     moved on its own      panel background

                   So use "hit", not the pixels, to answer "did the click land":
                     hit present, is_window_itself false -> aimed at a real control
                     hit.is_window_itself true           -> you pressed background
                     hit.enabled false                   -> it arrived and was ignored
                   With a hit and /health input.uipi_risk false, changed=false is
                   simply a control that does not repaint. Nothing is wrong.

                   For the other half, "change" carries pixels + bbox (window
                   coordinates), so a one-cell clock tick is distinguishable from a
                   real repaint. If a clock keeps forcing changed=true, exclude it:
                     ignore=x,y,w,h   (query, or "ignore":[x,y,w,h] in the body)

                   Check a coordinate WITHOUT pressing anything - this reports hit too:
                     curl -s -o check.png ".../capture.png?mark=850,660&inset=4"

                   Caveat: hit only works where controls are separate windows (Win32,
                   MFC, WinForms). If GET /controls comes back empty, this application
                   draws its own controls and every point reports is_window_itself.

  WHEN THE CONTAINER ITSELF CHANGED SIZE - POST /admin/profile/refit
  An anchor corrects a TRANSLATION, and that is exactly why it can be trusted. A new build
  whose operator panel grew from 708x238 to 746x251 is past it: every rectangle inside is
  wrong by an amount that depends on how far it sits from the container's own origin, and
  no single offset can say that.
    curl -s -X POST -H "X-Admin-Code: THECODE" \
      "{base}/admin/profile/refit?profile=NAME"
  Nothing is written. The reply is the proposal: where each anchor was and is now, how many
  buttons and regions would move, and - the part to read - "verify".

  IT CHECKS ITSELF, AND YOU SHOULD READ THE CHECK. Each moved button's new click point is
  looked up on the live window. "landed" only means a control is there; "worst_offset" is
  the number that matters, because a point half a key off still lands, on the NEIGHBOUR.
  More than a few pixels means the layout did not merely scale - it re-flowed - and this
  endpoint is not the answer for that application.

  Then save it:
    curl -s -X POST -H "X-Admin-Code: THECODE" \
      "{base}/admin/profile/refit?profile=NAME&apply=true"
  A save is REFUSED while any moved button lands on nothing, and says which ones. force=true
  overrides that, for keys the application genuinely has no control for. The previous file is
  kept as a backup either way, and reference_client is set to the window as it is now -
  otherwise on_size_mismatch would refuse every click against coordinates that are correct.

  AN AMBIGUOUS ANCHOR IS REFUSED, NOT GUESSED. Anchors are found by text AND size, and a
  refit is for when the size changed - so where two controls carry the same text and neither
  is still the saved size, there is nothing left to tell them apart. The reply lists the
  candidates; name the right one:
    -d '{{"anchors": {{"OPERATION PANEL": [1142,384,746,251]}}, "apply": true}}'

  ELEMENTS MARKED "@fixed" ARE NOT MOVED. Somebody stated they do not travel with a
  container, and a refit does not overrule that. They are listed under "untouched".

  A profile with no anchors is refused - there is no container to re-seat against. For a
  window that merely changed size, POST /window/fit or reference_client is the answer.

THE WINDOW'S MENU BAR - the one thing not written in the profile
  A menu item has no stable rectangle. It exists only while the menu is open, it moves with
  the length of the items above it, and opening a menu in order to click inside it leaves
  the application open if the click then fails. So the menu is reached by its own identity
  instead: read the tree, name the path.
    curl -s "{base}/menus?profile=NAME"
    curl -s -X POST -H "Content-Type: application/json" \
      -d '{{"path":"Tool/Set Machine Parameters","capture":"@client"}}' {base}/menu

  'path' is the full path as GET /menus prints it, separated by '/'. The '&' that marks the
  underlined letter and the accelerator column (Ctrl+S) are already stripped there - do not
  type them. Matching ignores case. A 'submenu' entry is a place, not an action; its
  children are the actions, and naming one lists them.

  THIS IS THE ONE PLACE WHERE A NAME IS NOT FROM THE PROFILE. Everything else here can only
  press what a person wrote in the profile file. A menu is read off the window, so this
  endpoint reaches whatever the application's menu reaches - which is why it needs
  "allow_menus": true in config.json, on top of the control whitelist. GET /menus is not
  gated: knowing what is there presses nothing.
  A profile can name paths that need a second look, and they behave like a confirm button:
    "confirm_menus": ["File", "Tool/Set Machine Parameters"]
  Matched on whole path segments, so "File" covers the whole File menu and does NOT cover
  "Filename Options". Those paths refuse unless the request carries "confirm": true.

  A DISABLED ITEM IS REFUSED, NOT ATTEMPTED. The command is delivered as WM_COMMAND, which
  is what an application receives AFTER it has decided an item is enabled - so posting a
  greyed-out item's command may be acted on anyway. "enabled" in GET /menus is the state the
  menu carries right now; an application that greys items out as the menu OPENS will report
  everything enabled, because nothing opens it.

  THE COMMAND IS POSTED, NOT SENT. A menu item that opens a modal dialog would otherwise
  hold the request open for as long as the dialog is on screen. So the reply means the
  application received it, not that it did anything - capture to see. And a dialog that
  opened is a NEW window: this profile still points at the old one, so GET /windows is how
  you find it.

  An empty list is an answer. Plenty of applications have no menu Windows can see, and some
  draw their own (a ribbon, a WPF menu, a custom title bar). A drawn menu is pixels, so it
  is reached by clicking like anything else.

HOW TO VERIFY WHAT YOU DID
  Prefer the application's own API over the screen wherever one exists. Use the screen
  to ACT and to see what only the screen shows; read the resulting state back from the
  API. That loop is far more robust than reading pixels.
"#,
        version = env!("CARGO_PKG_VERSION"),
        base = base,
    )
}

/// The button editor — drag rectangles onto a live capture.
/// The tray icon, encoded as a PNG, as the page's favicon.
///
/// Encoded per request rather than at build time: it is a 32x32 image, the encode is
/// microseconds, and keeping one copy of the drawing beats keeping a copy and a cache of it.
pub async fn favicon() -> Response {
    use image::ImageEncoder;
    let rgba = crate::icon::icon_rgba();
    let mut png = Vec::new();
    if image::codecs::png::PngEncoder::new(&mut png)
        .write_image(&rgba, 32, 32, image::ExtendedColorType::Rgba8)
        .is_err()
    {
        return (StatusCode::INTERNAL_SERVER_ERROR, "could not encode the icon").into_response();
    }
    let mut headers = axum::http::HeaderMap::new();
    headers.insert(axum::http::header::CONTENT_TYPE, axum::http::HeaderValue::from_static("image/png"));
    // It changes only when the exe does, and a browser asks for it on every visit.
    headers.insert(
        axum::http::header::CACHE_CONTROL,
        axum::http::HeaderValue::from_static("public, max-age=86400"),
    );
    (StatusCode::OK, headers, png).into_response()
}

pub async fn editor() -> Response {
    let mut headers = axum::http::HeaderMap::new();
    headers.insert(
        axum::http::header::CONTENT_TYPE,
        axum::http::HeaderValue::from_static("text/html; charset=utf-8"),
    );
    headers.insert(axum::http::header::CACHE_CONTROL, axum::http::HeaderValue::from_static("no-store"));
    (StatusCode::OK, headers, include_str!("editor.html")).into_response()
}

/// Click — JSON response.
pub async fn click_json(State(state): State<SharedState>, body: Bytes) -> Result<Response, ApiError> {
    let req: ClickReq = parse_body(&body)?;
    let (_png, result) = do_click(&state, req).await?;
    Ok(json_ok(result))
}

/// Click, wait to settle and re-capture **in one round trip**, returning the PNG itself.
/// With no capture region given, the whole client area is captured.
pub async fn click_png(
    State(state): State<SharedState>,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Response, ApiError> {
    let mut req = ClickReq::from_query(&q)?;
    if req.capture.is_none() {
        req.capture = Some("@client".to_string());
    }
    let (png, result) = do_click(&state, req).await?;
    match png {
        Some(bytes) => Ok(png_response(bytes, &result)),
        None => Err(ApiError::internal("click succeeded but produced no image")),
    }
}

/// Capture file names are built from the region name, and `button:NAME` carries a colon,
/// which Windows will not accept in a path. Fold anything unusual into '_' rather than
/// letting the save fail for a reason that has nothing to do with the capture.
fn safe_label(region: &str) -> String {
    region
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' })
        .collect()
}

/// One press, worked out completely before anything moves.
///
/// The sequence path resolves **every** press up front and only then starts clicking. A name
/// typo in element eight must not leave seven characters sitting in the machine.
struct Press {
    name: String,
    /// The rectangle the profile holds for this button, kept so the control actually found at
    /// the point can be checked against it. `None` for coordinates sent by the request, where
    /// there is no saved claim to check.
    saved_rect: Option<Rect>,
    point: (i32, i32),
    button: Button,
    double: bool,
    hold_ms: u64,
    settle_ms: Option<u64>,
    /// Whether the saved definition is marked confirm (for the audit log).
    was_confirm: bool,
    /// Not a saved name — coordinates sent for this one request only.
    adhoc: bool,
}

/// Everything about a request that can be judged without the window.
///
/// Kept separate so a bad request is answered as a bad request. `bind_window` fails whenever
/// the application is not running, and if that ran first, every typo and every mis-shaped body
/// would come back as "no visible window matches" — pointing at the one thing that is not
/// wrong.
/// The `/key` counterpart of [`precheck`]. Same reason: a request that is wrong on its own
/// terms must say so, whether or not the window happens to be open. Everything here needs only
/// the profile, so it can run before the window is bound.
fn precheck_key(state: &SharedState, t: &Targets, req: &KeyReq) -> Result<(), ApiError> {
    match (&req.key, &req.chord, &req.text) {
        (Some(name), _, _) => {
            if !t.keys.contains_key(name) {
                return Err(ApiError::not_found(format!("unknown key '{name}'")).with_detail(
                    json!({
                        "keys": near_names(name, t.keys.keys().cloned(), "GET /profiles"),
                        "note": "'key' takes a name from the profile's keys. An unnamed chord goes in 'chord' instead, which needs allow_raw_keys",
                    }),
                ));
            }
        }
        (None, Some(_), _) | (None, None, Some(_)) => {
            if !state.config.allow_raw_keys {
                return Err(raw_keys_denied(t));
            }
        }
        (None, None, None) => {
            return Err(ApiError::bad_request("one of 'key', 'chord' or 'text' is required")
                .with_detail(json!({
                    "known_keys": t.keys.keys().cloned().collect::<Vec<_>>(),
                })));
        }
    }
    Ok(())
}

fn precheck(t: &Targets, req: &ClickReq) -> Result<(), ApiError> {
    // One name is checkable without the window, so check it here rather than after binding.
    // Otherwise a typo answers "no visible window matches", which sends the caller to look at
    // the window spec when the wrong thing was in their own request.
    if let Some(name) = &req.button
        && req.buttons.is_none()
    {
        let Some(tg) = t.buttons.get(name) else {
            return Err(ApiError::not_found(format!("unknown button '{name}'")).with_detail(
                json!({"buttons": near_names(name, t.buttons.keys().cloned(), "GET /buttons")}),
            ));
        };
        if tg.confirm && !req.confirm {
            return Err(ApiError::forbidden(format!(
                "button '{name}' is marked confirm — resend with \"confirm\": true"
            ))
            .with_detail(json!({"note": tg.note})));
        }
    }
    let Some(names) = &req.buttons else { return Ok(()) };
    if req.button.is_some() || req.rect.is_some() || req.point.is_some() {
        return Err(ApiError::bad_request(
            "'buttons' cannot be combined with 'button', 'rect' or 'point' — send one sequence \
             or one press",
        ));
    }
    if names.is_empty() {
        return Err(ApiError::bad_request("'buttons' is empty — nothing to press"));
    }
    if names.len() > MAX_SEQUENCE {
        return Err(ApiError::bad_request(format!(
            "'buttons' has {} entries; the limit is {MAX_SEQUENCE}",
            names.len()
        )));
    }
    // Check every name and every confirm flag now. A typo in element eight has to cost
    // nothing, not seven characters already sitting in the machine.
    for (i, n) in names.iter().enumerate() {
        let tg = t.buttons.get(n).ok_or_else(|| {
            ApiError::not_found(format!("unknown button '{n}' at index {i} of 'buttons'"))
                .with_detail(json!({
                    "index": i,
                    "buttons": near_names(n, t.buttons.keys().cloned(), "GET /buttons"),
                    "note": "nothing was pressed — the whole sequence is checked before any of it runs",
                }))
        })?;
        if tg.confirm && !req.confirm {
            return Err(ApiError::forbidden(format!(
                "'{n}' at index {i} of 'buttons' is marked confirm — a sequence is not a way \
                 around that. Resend with \"confirm\": true, or better, press it on its own"
            ))
            .with_detail(json!({"index": i, "button": n, "note": tg.note})));
        }
    }
    Ok(())
}

/// Resolve one saved button. Everything that can refuse, refuses here.
fn resolve_saved(
    cfg: &Config,
    t: &Targets,
    name: &str,
    req: &ClickReq,
    scale: (f64, f64),
    offsets: &AnchorOffsets,
) -> Result<Press, ApiError> {
    let tg = t.buttons.get(name).ok_or_else(|| {
        ApiError::not_found(format!("unknown button '{name}'"))
            .with_detail(json!({"buttons": near_names(name, t.buttons.keys().cloned(), "GET /buttons")}))
    })?;
    if tg.confirm && !req.confirm {
        return Err(ApiError::forbidden(format!(
            "button '{name}' is marked confirm — resend with \"confirm\": true"
        ))
        .with_detail(json!({"note": tg.note})));
    }
    Ok(Press {
        name: name.to_string(),
        // Unscaled: the check compares against what the file holds, not a derived number.
        saved_rect: Some(tg.rect),
        point: Targets::click_point(tg, scale, Targets::offset_for(offsets, &tg.anchor)),
        button: Button::parse(req.click_button.as_deref().unwrap_or(&tg.click_button))
            .map_err(ApiError::bad_request)?,
        double: req.double.unwrap_or(tg.double),
        hold_ms: cfg.clamp_hold(req.hold_ms.or(tg.hold_ms)),
        settle_ms: tg.settle_ms,
        was_confirm: tg.confirm,
        adhoc: false,
    })
}

/// Default pause between the presses of a `buttons` sequence.
///
/// Deliberately unhurried. An operator panel that drops input when pressed too fast fails
/// silently — you get a half-typed block and no error — and this endpoint exists precisely to
/// stop half-typed blocks. Callers who have measured their panel can lower it.
const DEFAULT_GAP_MS: u64 = 500;

/// How long a **sequence** waits before its capture, when the request and the last button both
/// leave it open. Higher than the server's single-press default for the same reason `gap_ms` is
/// slow: the last press of a sequence is almost always the commit — INSERT, INPUT, CYCLE START
/// — and a commit does more than a keystroke. Too short and the capture is of the screen just
/// before it, which shows an entry still sitting in the input line and looks exactly like a
/// sequence that failed. Silently reading the previous screen is the failure this endpoint
/// exists to prevent, so the default errs long.
///
/// The precise answer is a `settle_ms` on the commit button itself, measured once and recorded
/// in the profile. This is only the fallback for when nobody has.
const DEFAULT_SEQUENCE_SETTLE_MS: u64 = 800;
/// Ceiling on how many presses one request may carry.
const MAX_SEQUENCE: usize = 200;

async fn do_click(state: &SharedState, mut req: ClickReq) -> Result<(Option<Vec<u8>>, Value), ApiError> {
    let prof = pick(state, req.profile.as_deref(), req.params)?;
    // One input at a time, across every profile. There is one mouse and one foreground on a
    // PC, so driving two windows at once would have them stealing focus from each other.
    let _guard = state.input_lock.lock().await;
    let st = state.clone();
    tokio::task::spawn_blocking(move || {
        let t = prof.targets();

        // A spelled string becomes an ordinary sequence here, before anything else reads the
        // request. `buttons` is then the only shape the rest of this function knows about, so
        // gap_ms, "stop at the first failure", per_press and the confirm rule apply to a
        // spelled string exactly as they do to a hand-written array — which is the whole
        // reason this expands rather than being a second way to press things.
        let spelled = match &req.spell {
            None => None,
            Some(text) => {
                if req.buttons.is_some()
                    || req.button.is_some()
                    || req.rect.is_some()
                    || req.point.is_some()
                {
                    return Err(ApiError::bad_request(
                        "'spell' cannot be combined with 'button', 'buttons', 'rect' or 'point' \
                         — spelling IS the sequence",
                    ));
                }
                let seq = t.spell(text).map_err(|bad| unspellable(&t, text, &bad))?;
                if seq.is_empty() {
                    return Err(ApiError::bad_request("'spell' is empty — nothing to press")
                        .with_detail(json!({
                            "note": "a string with no characters presses nothing, which is \
                                     not something to do quietly",
                        })));
                }
                let v = spelled_json(&t, &seq, text);
                req.buttons = Some(seq.iter().map(|s| s.button.clone()).collect());
                Some(v)
            }
        };

        // Answer request-shape problems BEFORE looking up the window. A typo in a button name
        // does not depend on the window being there, and reporting it as "no visible window
        // matches" sends the caller off to fix the wrong thing entirely.
        precheck(&t, &req)?;

        let (info, scale) = bind_window(&t)?;
        // Once per request, before any coordinate is used. An anchor that cannot be found
        // stops everything here rather than letting some presses land corrected and others not.
        let offsets = anchor_offsets(&t, &info)?;

        // ── work out what to press ── (this is the safety boundary: a name, or a refusal)
        let sequence = req.buttons.is_some();
        let presses: Vec<Press> = match (&req.buttons, &req.button, req.rect, req.point) {
            // `precheck` already accepted the names, the confirm flags and the shape of the
            // request; this only turns them into coordinates.
            (Some(names), _, _, _) => names
                .iter()
                .map(|n| resolve_saved(&st.config, &t, n, &req, scale, &offsets))
                .collect::<Result<Vec<_>, _>>()?,
            (None, Some(name), _, _) => vec![resolve_saved(&st.config, &t, name, &req, scale, &offsets)?],

            // ── a rectangle that was never saved ──
            // Controls that appear only on some screens cannot be on the named list. Read the
            // capture, send the rectangle, and its centre gets pressed — the same rule a saved
            // button follows, so this also previews how it would behave once saved.
            (None, None, Some(r), _) => {
                if !st.config.allow_raw_clicks {
                    return Err(adhoc_denied(&t));
                }
                if r[2] <= 0 || r[3] <= 0 {
                    return Err(ApiError::bad_request(format!(
                        "rect [{},{},{},{}] must have positive width and height",
                        r[0], r[1], r[2], r[3]
                    )));
                }
                let [rx, ry, rw, rh] = Targets::scale_rect(r, scale);
                vec![Press {
                    name: format!("(rect {},{},{},{})", r[0], r[1], r[2], r[3]),
                    saved_rect: None,
                    point: (rx + rw / 2, ry + rh / 2),
                    button: Button::parse(req.click_button.as_deref().unwrap_or("left"))
                        .map_err(ApiError::bad_request)?,
                    double: req.double.unwrap_or(false),
                    hold_ms: st.config.clamp_hold(req.hold_ms),
                    settle_ms: None,
                    was_confirm: false,
                    adhoc: true,
                }]
            }

            (None, None, None, Some([x, y])) => {
                if !st.config.allow_raw_clicks {
                    return Err(adhoc_denied(&t));
                }
                vec![Press {
                    name: format!("(point {x},{y})"),
                    saved_rect: None,
                    point: (
                        (x as f64 * scale.0).round() as i32,
                        (y as f64 * scale.1).round() as i32,
                    ),
                    button: Button::parse(req.click_button.as_deref().unwrap_or("left"))
                        .map_err(ApiError::bad_request)?,
                    double: req.double.unwrap_or(false),
                    hold_ms: st.config.clamp_hold(req.hold_ms),
                    settle_ms: None,
                    was_confirm: false,
                    adhoc: true,
                }]
            }

            (None, None, None, None) => {
                return Err(ApiError::bad_request(
                    "one of 'button', 'buttons', 'rect' or 'point' is required",
                )
                .with_detail(json!({
                    "known_buttons": t.button_names(),
                    "hint": "a saved name presses a known control; 'buttons' presses several in \
                             order; rect/point press whatever is at those client coordinates now",
                })));
            }
        };

        // ── checks that apply to every press, still before any input ──
        let (cw, ch) = info.client_size;
        for p in &presses {
            // Raw coordinates must not slip past confirm.
            //
            // The confirm check above only covers named buttons — no name, no flag. But a
            // rectangle computed from a capture can happen to land on the emergency stop, and
            // then a person's "think twice about this one" quietly is not there. So an
            // unnamed press that falls inside a saved confirm button clears the same bar.
            // Deliberate presses pass with confirm: true; accidental ones are caught.
            if p.adhoc
                && !req.confirm
                && let Some((guarded, tg)) = confirm_button_at(&t, p.point, scale)
            {
                return Err(ApiError::forbidden(format!(
                    "that point is inside '{guarded}', which is marked confirm — resend with \
                     \"confirm\": true if you meant it"
                ))
                .with_detail(json!({
                    "button": guarded,
                    "note": tg.note,
                    "rect": tg.rect,
                    "why": "a person marked this control as needing a second thought. Sending raw \
                            coordinates does not bypass that, on purpose.",
                })));
            }
            // Pressing outside the client area is a configuration mistake — stop before
            // clicking somewhere on the desktop.
            if p.point.0 < 0 || p.point.1 < 0 || p.point.0 >= cw || p.point.1 >= ch {
                return Err(ApiError::bad_request(format!(
                    "click point ({}, {}) for '{}' is outside the {cw}x{ch} client area",
                    p.point.0, p.point.1, p.name
                )));
            }
        }

        // ── choose the capture region (grab the "before" frame first) ──
        // Only the request decides what to look at. Buttons used to be able to carry a default
        // capture region, and since most clicks need no confirmation it fired on every one of
        // them (twice, for change detection) with no way for the caller to turn it off.
        // Buttons have coordinates, so whoever needs to look can choose then.
        //
        // `capture=button` with no name means "the button just pressed" — for a sequence, the
        // last one. Pressing and then looking at that same control is the common case, and
        // repeating the name there is noise.
        // Measuring **is** comparing pictures, so `measure` without a region to compare would
        // be a request with no way to answer it. The whole client area is the honest default:
        // whatever the press moved is somewhere in it.
        let measure = req.measure.unwrap_or(false);
        let per_press = req.per_press.unwrap_or(false);
        let region_name = req
            .capture
            .clone()
            .filter(|r| !r.is_empty())
            .or_else(|| (measure || per_press).then(|| "@client".to_string()))
            .map(|r| {
                if r == "button" {
                    format!("button:{}", presses.last().map(|p| p.name.as_str()).unwrap_or(""))
                } else {
                    r
                }
            });
        let pad = req.pad.unwrap_or(0).max(0);
        let crop = match &region_name {
            None => None,
            Some(r) => Some(Targets::scale_rect(
                Targets::pad_rect(
                    t.region(r, info.client_size, &offsets).map_err(|e| {
                        ApiError::bad_request(e)
                            .with_detail(json!({"known_regions": t.region_names()}))
                    })?,
                    pad,
                ),
                scale,
            )),
        };
        // Hold the whole frame, not a digest. A digest is enough for a boolean, but reporting
        // where and how much changed needs the pixels.
        let before = match crop {
            Some(rect) => shoot(&info, rect, req.scale, req.max_width).ok().map(|(f, _, _)| f),
            None => None,
        };

        // ── the actual input ──
        window::focus(info.handle).map_err(|e| ApiError::conflict(e).with_detail(json!({
            "hint": "another window is holding the foreground, or the session is locked",
        })))?;
        // The window may have just moved, so measure the origin again.
        let mut info = window::describe(info.handle)
            .ok_or_else(|| ApiError::conflict("the target window disappeared while focusing it"))?;

        // The rolling comparison is its own frame. `before` has to stay the shot from before
        // the first press, because "did this request change anything" is still a question and
        // is not the sum of the per-press answers. Taken here, after focusing, so that raising
        // the window is not counted as the first press's doing.
        let mut prev = match (per_press, crop) {
            (true, Some(rect)) => shoot(&info, rect, req.scale, req.max_width).ok().map(|(f, _, _)| f),
            _ => None,
        };

        let gap = st.config.clamp_settle(Some(req.gap_ms.unwrap_or(DEFAULT_GAP_MS)));
        let mut done: Vec<Value> = Vec::with_capacity(presses.len());
        let mut failure: Option<Value> = None;
        // The last press is the one the "nothing changed" hint has to reason about.
        let mut last_hit: Option<crate::win::window::ControlHit> = None;

        for (i, pr) in presses.iter().enumerate() {
            if i > 0 {
                std::thread::sleep(std::time::Duration::from_millis(gap));
                // The previous press's own change, read after its gap and **before** the next
                // press can add to it. Attribution is the whole point, so the shot has to sit
                // between the two.
                if let (Some(rect), Some(p)) = (crop, prev.as_ref())
                    && let Ok((f, _, _)) = shoot(&info, rect, req.scale, req.max_width)
                {
                    if let Some(entry) = done.last_mut() {
                        entry["change"] = press_change(p, &f, req.ignore);
                    }
                    prev = Some(f);
                }
                // The application may have closed or replaced the window mid-sequence.
                match window::describe(info.handle) {
                    Some(w) => info = w,
                    None => {
                        failure = Some(json!({
                            "index": i,
                            "button": pr.name,
                            "error": "the target window disappeared part-way through the sequence",
                        }));
                        break;
                    }
                }
            }
            let (sx, sy) = window::client_to_screen(&info, pr.point.0, pr.point.1);
            // Look under the point **immediately before** pressing. Afterwards the application
            // may have destroyed and recreated controls, so you would be seeing what is left
            // rather than what was aimed at.
            let hit = window::control_at(info.handle, pr.point.0, pr.point.1);

            if let Err(e) = input::click(sx, sy, pr.button, pr.double, pr.hold_ms) {
                // Stop here. Carrying on would finish a string nobody asked for, and that is
                // the worst kind of quietly wrong result.
                log::error!(
                    "CLICK profile={} target={} FAILED at {}/{} — {e}",
                    prof.name, pr.name, i + 1, presses.len()
                );
                failure = Some(json!({"index": i, "button": pr.name, "error": e}));
                break;
            }

            // Audit line, written **immediately after the press**. Even if settling or
            // re-capturing then fails, the fact that it was pressed is already recorded. This
            // tool can press an emergency stop; without a record of what was pressed and when,
            // there is nowhere to look afterwards.
            //
            // confirm targets and unnamed coordinates are WARN — the two kinds that have to
            // stand out when skimming. Everything else is INFO.
            let mut line = format!(
                "CLICK profile={} target={} button={} double={} hold={}ms client=({},{}) screen=({sx},{sy}) window={:?} pid={}",
                prof.name,
                pr.name,
                pr.button.as_str(),
                pr.double,
                pr.hold_ms,
                pr.point.0,
                pr.point.1,
                info.title,
                info.pid
            );
            if sequence {
                line = format!("{line} seq={}/{}", i + 1, presses.len());
            }
            // Record what was aimed at too — asked later why a press did nothing, coordinates
            // alone cannot answer.
            line = match &hit {
                Some(h) if !h.is_window_itself => format!(
                    "{line} hit={:?}/{:?}{}",
                    h.class,
                    h.text,
                    if h.enabled { "" } else { " DISABLED" }
                ),
                _ => format!("{line} hit=none(window background)"),
            };
            if pr.was_confirm || pr.adhoc {
                log::warn!("{line} confirm={}", pr.was_confirm);
            } else {
                log::info!("{line}");
            }

            if let Some(a) = aim_json(pr.saved_rect, hit.as_ref())
                && a["matches"] == json!(false)
            {
                log::warn!(
                    "CLICK profile={} target={} LAYOUT DRIFT — saved {:?}, control found at {:?}, delta {}",
                    prof.name, pr.name, a["saved"], a["found"], a["delta"]
                );
            }
            last_hit = hit.clone();
            done.push(json!({
                "index": i,
                "button": pr.name,
                // How long the contact was actually closed. A panel key that needs longer than
                // this to be scanned does nothing at all, so the number has to be visible.
                "hold_ms": pr.hold_ms,
                "point": {"client": [pr.point.0, pr.point.1], "screen": [sx, sy]},
                // Answers "did the press land on something", independent of pixels.
                "hit": hit.as_ref().map(hit_json),
                // Answers "is it still where the file says", which the hit alone cannot.
                "aim": aim_json(pr.saved_rect, hit.as_ref()),
            }));
        }

        // settle_ms belongs to the whole request, not to each press — the sequence has its own
        // spacing in gap_ms, and what the caller waits for is the state after the last one.
        let settle = st.config.clamp_settle(Some(
            req.settle_ms
                .or_else(|| presses.last().and_then(|p| p.settle_ms))
                .unwrap_or(if sequence { DEFAULT_SEQUENCE_SETTLE_MS } else { st.config.default_settle_ms }),
        ));
        // Either wait the agreed time, or watch until the screen stops moving and report how
        // long that took. Never both: the wait and the measurement are two answers to the same
        // question, and doing the fixed wait first would put it inside the number.
        let (settle, measured, mut watched) = match (measure, crop) {
            (true, Some(rect)) => {
                let (quiet, ceiling) = watch_bounds(&st.config, req.quiet_ms);
                let w = watch_until_still(&info, rect, req.scale, req.max_width, req.ignore, quiet, ceiling)?;
                (w.elapsed_ms, watch_json(&w, quiet, ceiling), Some(w.last))
            }
            _ => {
                std::thread::sleep(std::time::Duration::from_millis(settle));
                (settle, Value::Null, None)
            }
        };

        // The last press has no gap after it — its change is read from the settled screen,
        // which is the very picture the capture below returns. One shot, used twice.
        if let (Some(rect), Some(p)) = (crop, prev.as_ref()) {
            let f = match watched.take() {
                Some(f) => f,
                None => shoot(&info, rect, req.scale, req.max_width)?,
            };
            if let Some(entry) = done.last_mut() {
                entry["change"] = press_change(p, &f.0, req.ignore);
            }
            watched = Some(f);
        }

        // ── report ──
        let last = done.last().cloned().unwrap_or(Value::Null);
        let mut result = json!({
            "profile": prof.name,
            "settle_ms": settle,
            "window": window_json(&info),
        });
        if !measured.is_null() {
            result["settle"] = measured;
        }
        // What the string turned into. Without it the reply lists button names and leaves the
        // caller to work out which character each one was, including the shift presses that
        // enter nothing.
        if let Some(v) = spelled {
            result["spelled"] = v;
        }
        if sequence {
            result["pressed"] = json!(done);
            // The list the whole feature exists for: not "something is wrong somewhere in
            // these twelve presses", but which ones moved nothing.
            let quiet: Vec<Value> = done
                .iter()
                .filter(|e| e["change"]["changed"] == json!(false))
                .map(|e| e["button"].clone())
                .collect();
            result["sequence"] = json!({
                "requested": presses.len(),
                "pressed": done.len(),
                "gap_ms": gap,
                "per_press": per_press,
                "unchanged": if per_press { json!(quiet) } else { Value::Null },
                "unchanged_hint": if per_press && !quiet.is_empty() {
                    json!("these presses moved nothing on the screen. That is not by itself a \
                           failure - a toggle already in that state, a key with no legend to \
                           repaint, and a key ignored in the current mode all look the same \
                           here. It is where to look: check each one's 'hit', and if the hit \
                           says a real enabled control was reached, the press length is the \
                           next thing to change (hold_ms).")
                } else {
                    Value::Null
                },
                "complete": failure.is_none(),
                "failed": failure.clone(),
                "note": "presses stop at the first failure. Anything already pressed is in \
                         'pressed'; treat a partial sequence as an unfinished entry and look at \
                         the screen before doing anything else.",
            });
        } else {
            result["clicked"] = last["button"].clone();
            result["button"] = json!(presses[0].button.as_str());
            result["double"] = json!(presses[0].double);
            // Lifted out of the per-press record, which only a sequence publishes. A single
            // press is the commonest call and the one where a key that needs a longer hold
            // shows up first, so leaving the number out here made the manual's promise that
            // "every reply reports the hold that was actually used" false where it mattered.
            result["hold_ms"] = json!(presses[0].hold_ms);
            result["aim"] = last["aim"].clone();
            result["point"] = last["point"].clone();
            result["hit"] = last["hit"].clone();
            // per_press is for sequences — with one press there is nothing to attribute. It is
            // still published rather than discarded: a caller who asked for it and got a reply
            // with no trace of it has no way to tell "not applicable" from "ignored".
            if per_press {
                result["change"] = last["change"].clone();
            }
        }

        let mut png_out = None;
        if let (Some(rect), Some(region)) = (crop, region_name.clone()) {
            // Watching already ended on a picture of the settled screen. Taking another here
            // would be a picture of a slightly later moment than the one that was measured.
            let (frame, method, black) = match watched {
                Some(f) => f,
                None => shoot(&info, rect, req.scale, req.max_width)?,
            };
            let label = format!("{}_{}", prof.name, safe_label(&region));
            let (png, mut meta) = deliver(&st, &frame, method, black, &label, true)?;
            if let Some(b) = before {
                apply_change(&mut meta, &b, &frame, req.ignore, last_hit.as_ref());
                if meta["changed"] == json!(false) {
                    log::warn!(
                        "CLICK target={} produced no visible change in region {region}",
                        presses.last().map(|p| p.name.as_str()).unwrap_or("")
                    );
                }
            }
            if pad > 0 {
                meta["pad"] = json!(pad);
            }
            result["capture"] = meta;
            png_out = Some(png);
        }
        // A failed sequence is still a 200: input really was sent, and the caller has to know
        // exactly how far it got. An error status with no body would leave them guessing.
        Ok((png_out, result))
    })
    .await
    .map_err(|e| ApiError::internal(format!("click task failed: {e}")))?
}

/// Key input — on an application that maps panel keys to the PC keyboard, this is often more
/// reliable than clicking.
pub async fn key(State(state): State<SharedState>, body: Bytes) -> Result<Response, ApiError> {
    let req: KeyReq = parse_body(&body)?;
    let prof = pick(&state, req.profile.as_deref(), Params::Body)?;
    let _guard = state.input_lock.lock().await;
    let st = state.clone();
    tokio::task::spawn_blocking(move || {
        let t = prof.targets();
        // Answer request-shape and policy problems BEFORE binding the window, so a typo in a
        // key name does not come back as "no visible window matches".
        precheck_key(&st, &t, &req)?;
        let (info, scale) = bind_window(&t)?;
        // Once per request, before any coordinate is used. An anchor that cannot be found
        // stops everything here rather than letting some presses land corrected and others not.
        let offsets = anchor_offsets(&t, &info)?;

        enum Action {
            Chord(input::Chord),
            Text(String),
        }
        let (label, action) = match (&req.key, &req.chord, &req.text) {
            (Some(name), _, _) => {
                let spec = t.keys.get(name).ok_or_else(|| {
                    ApiError::not_found(format!("unknown key '{name}'")).with_detail(json!({
                        "known_keys": t.keys.keys().cloned().collect::<Vec<_>>(),
                    }))
                })?;
                (
                    format!("{name} ({spec})"),
                    Action::Chord(input::parse_chord(spec).map_err(ApiError::internal)?),
                )
            }
            (None, Some(spec), _) => {
                if !st.config.allow_raw_keys {
                    return Err(raw_keys_denied(&t));
                }
                (
                    spec.clone(),
                    Action::Chord(input::parse_chord(spec).map_err(ApiError::bad_request)?),
                )
            }
            (None, None, Some(text)) => {
                if !st.config.allow_raw_keys {
                    return Err(raw_keys_denied(&t));
                }
                if text.chars().count() > 512 {
                    return Err(ApiError::bad_request("text is limited to 512 characters"));
                }
                (format!("text[{} chars]", text.chars().count()), Action::Text(text.clone()))
            }
            (None, None, None) => {
                return Err(ApiError::bad_request("one of 'key', 'chord' or 'text' is required")
                    .with_detail(json!({
                        "known_keys": t.keys.keys().cloned().collect::<Vec<_>>(),
                    })));
            }
        };

        // Same rule as a click: measuring is comparing pictures, so it needs one to compare.
        let measure = req.measure.unwrap_or(false);
        let region_name =
            req.capture.clone().or_else(|| measure.then(|| "@client".to_string()));
        let pad = req.pad.unwrap_or(0).max(0);
        let crop = match &region_name {
            None => None,
            Some(r) => Some(Targets::scale_rect(
                Targets::pad_rect(
                    t.region(r, info.client_size, &offsets).map_err(|e| {
                        ApiError::bad_request(e)
                            .with_detail(json!({"known_regions": t.region_names()}))
                    })?,
                    pad,
                ),
                scale,
            )),
        };
        let before = match crop {
            Some(rect) => shoot(&info, rect, req.scale, req.max_width).ok().map(|(f, _, _)| f),
            None => None,
        };

        window::focus(info.handle).map_err(ApiError::conflict)?;
        log::info!("KEY profile={} sent={label} window={:?} pid={}", prof.name, info.title, info.pid);
        match &action {
            Action::Chord(c) => input::send_chord(c).map_err(ApiError::internal)?,
            Action::Text(s) => input::send_text(s).map_err(ApiError::internal)?,
        }

        let settle = st.config.clamp_settle(req.settle_ms);
        let (settle, measured, watched) = match (measure, crop) {
            (true, Some(rect)) => {
                let (quiet, ceiling) = watch_bounds(&st.config, req.quiet_ms);
                let w = watch_until_still(&info, rect, req.scale, req.max_width, req.ignore, quiet, ceiling)?;
                (w.elapsed_ms, watch_json(&w, quiet, ceiling), Some(w.last))
            }
            _ => {
                std::thread::sleep(std::time::Duration::from_millis(settle));
                (settle, Value::Null, None)
            }
        };

        let info = window::describe(info.handle)
            .ok_or_else(|| ApiError::conflict("the target window disappeared"))?;
        let mut result = json!({
            "profile": prof.name,
            "sent": label,
            "settle_ms": settle,
            "window": window_json(&info),
        });
        if !measured.is_null() {
            result["settle"] = measured;
        }
        if let (Some(rect), Some(region)) = (crop, region_name) {
            let (frame, method, black) = match watched {
                Some(f) => f,
                None => shoot(&info, rect, req.scale, req.max_width)?,
            };

            let (_png, mut meta) =
                deliver(&st, &frame, method, black, &format!("{}_{}", prof.name, safe_label(&region)), true)?;
            if let Some(b) = before {
                apply_change(&mut meta, &b, &frame, req.ignore, None);
            }
            result["capture"] = meta;
        }
        Ok(json_ok(result))
    })
    .await
    .map_err(|e| ApiError::internal(format!("key task failed: {e}")))?
}

/// An attempt to press an unsaved area while the policy is closed.
fn adhoc_denied(t: &Targets) -> ApiError {
    ApiError::forbidden("ad-hoc clicks are disabled — only saved buttons can be pressed").with_detail(
        json!({
            "known_buttons": t.button_names(),
            "hint": "set \"allow_raw_clicks\": true in config.json and restart. That is what lets a caller press a control that only appears on some screens and therefore cannot be in the saved list.",
        }),
    )
}

fn raw_keys_denied(t: &Targets) -> ApiError {
    ApiError::forbidden("raw key input is disabled — use a named key from the profile").with_detail(
        json!({
            "known_keys": t.keys.keys().cloned().collect::<Vec<_>>(),
            "hint": "set \"allow_raw_keys\": true in config.json and restart to allow 'chord' and 'text'",
        }),
    )
}

/// Bring the window to the front. Done automatically before a click, but also useful when a
/// person wants to look at the screen.
pub async fn window_focus(State(state): State<SharedState>, Query(q): Query<HashMap<String, String>>) -> Result<Response, ApiError> {
    let prof = pick(&state, q_get(&q, "profile").as_deref(), Params::Query)?;
    let _guard = state.input_lock.lock().await;
    tokio::task::spawn_blocking(move || {
        let t = prof.targets();
        // Not `find_window` — that one refuses a minimised window, and undoing minimisation is
        // precisely this endpoint's job. (`/window/fit` bypasses the size check for the same
        // reason.) A recovery must not be blocked by the state it recovers from.
        let before = window::find(&t.window).map_err(|_| {
            ApiError::not_found("target window not found")
                .with_detail(json!({"hint": "GET /windows lists visible window titles"}))
        })?;
        let minimized_before = before.minimized;
        window::focus(before.handle).map_err(ApiError::conflict)?;
        let info = window::describe(before.handle)
            .ok_or_else(|| ApiError::conflict("the target window disappeared"))?;
        Ok(json_ok(json!({
            "focused": true,
            "restored": minimized_before,
            "window": window_json(&info),
        })))
    })
    .await
    .map_err(|e| ApiError::internal(format!("focus task failed: {e}")))?
}

/// Restore the client area to `reference_client` — the one move that recovers from a window
/// size drifting and taking every coordinate with it.
pub async fn window_fit(State(state): State<SharedState>, Query(q): Query<HashMap<String, String>>) -> Result<Response, ApiError> {
    let prof = pick(&state, q_get(&q, "profile").as_deref(), Params::Query)?;
    let _guard = state.input_lock.lock().await;
    tokio::task::spawn_blocking(move || {
        let t = prof.targets();
        let Some([rw, rh]) = t.reference_client else {
            return Err(ApiError::bad_request(
                "this profile has no reference_client, so there is no size to fit to",
            ));
        };
        // A size mismatch is the reason this endpoint gets called, so it bypasses
        // bind_window's check.
        let info = window::find(&t.window)
            .map_err(|_| ApiError::not_found("target window not found"))?;
        let before = info.client_size;
        let after = window::fit_client(info.handle, rw, rh).map_err(ApiError::internal)?;
        let info = window::describe(info.handle)
            .ok_or_else(|| ApiError::conflict("the target window disappeared"))?;
        if after != (rw, rh) {
            return Err(ApiError::conflict(format!(
                "asked for a {rw}x{rh} client area but the window settled at {}x{} — \
                 it may have a minimum size or a fixed layout",
                after.0, after.1
            ))
            .with_detail(json!({"before": [before.0, before.1], "after": [after.0, after.1]})));
        }
        Ok(json_ok(json!({
            "fitted": true,
            "before": [before.0, before.1],
            "after": [after.0, after.1],
            "window": window_json(&info),
        })))
    })
    .await
    .map_err(|e| ApiError::internal(format!("fit task failed: {e}")))?
}

/// Serve a stored capture — GET the `url` from a `/click` response as-is.
pub async fn capture_file(
    State(state): State<SharedState>,
    UrlPath(name): UrlPath<String>,
) -> Result<Response, ApiError> {
    if !captures::safe_capture_name(&name) {
        return Err(ApiError::bad_request("invalid capture name"));
    }
    let path = state.captures_dir.join(&name);
    let bytes = std::fs::read(&path).map_err(|_| ApiError::not_found(format!("no such capture: {name}")))?;
    Ok(png_response(bytes, &json!({"name": name})))
}

/// Whether that coordinate is **inside a button marked confirm**.
///
/// The test that stops an unnamed click from bypassing confirm. Pulled out on its own because
/// the path it lives on needs a window to reach — this way the rule itself is testable without
/// one.
fn confirm_button_at(
    t: &Targets,
    point: (i32, i32),
    scale: (f64, f64),
) -> Option<(&String, &ButtonDef)> {
    t.buttons.iter().find(|(_, tg)| {
        if !tg.confirm {
            return false;
        }
        let [x, y, w, h] = Targets::scale_rect(tg.rect, scale);
        point.0 >= x && point.0 < x + w && point.1 >= y && point.1 < y + h
    })
}

/// Whether the editing endpoints are open. Several handlers have to refuse with one sentence.
fn editing_allowed(state: &SharedState) -> Result<(), ApiError> {
    if state.config.allow_profile_editing {
        return Ok(());
    }
    Err(ApiError::forbidden(
        "profile editing over HTTP is disabled — edit the files under profiles/ on the target PC",
    )
    .with_detail(json!({
        "why": "with this off, the permission boundary is filesystem access to the profile files. \
                Turning it on moves that boundary to HTTP reachability.",
        "hint": "set \"allow_profile_editing\": true in config.json and restart",
    })))
}

/// The default profile the config **named** can be neither deleted nor renamed.
///
/// Losing it kills every request that omits `profile`, and the only way back is editing
/// `config.json` and restarting — which means a person, physically at that PC. One HTTP call
/// should not be able to create a state that requires that.
fn not_the_configured_default(state: &SharedState, name: &str) -> Result<(), ApiError> {
    if state.config.default_profile == name {
        return Err(ApiError::conflict(format!(
            "'{name}' is the default_profile in config.json — removing or renaming it would \
             break every request that omits ?profile=, and only a config edit plus a restart \
             could fix that"
        ))
        .with_detail(json!({
            "hint": "change default_profile in config.json and restart, then try again",
        })));
    }
    Ok(())
}

/// **Delete** a profile. The file is moved aside under a timestamped name.
///
/// `confirm=true` is required for the same reason a `confirm` button needs it: undoing this
/// takes a person. Restoring a deleted profile means finding the archive on that PC, renaming
/// it back, and calling `/admin/reload`.
pub async fn admin_delete_profile(
    State(state): State<SharedState>,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Response, ApiError> {
    editing_allowed(&state)?;
    let Some(name) = q_get(&q, "profile") else {
        return Err(ApiError::bad_request("which profile? pass ?profile=NAME")
            .with_detail(json!({"known_profiles": state.profile_names()})));
    };
    if !q_bool(&q, "confirm")?.unwrap_or(false) {
        return Err(ApiError::forbidden(format!(
            "deleting '{name}' needs confirm=true — this is not undoable from here"
        ))
        .with_detail(json!({
            "hint": format!("DELETE /admin/profile?profile={name}&confirm=true"),
            "what_survives": "the file is moved aside as deescreen.<name>.json.deleted-<timestamp> \
                              on the target PC, so a person can put it back",
        })));
    }
    let prof = pick(&state, Some(&name), Params::Query)?;
    not_the_configured_default(&state, &prof.name)?;

    let path = prof.path.clone();
    let archived = tokio::task::spawn_blocking(move || crate::targets::archive(&path))
        .await
        .map_err(|e| ApiError::internal(format!("delete task failed: {e}")))?
        .map_err(ApiError::internal)?;

    let t = prof.targets();
    let mut map = (**state.profiles.load()).clone();
    map.remove(&prof.name);
    let remaining: Vec<String> = map.keys().cloned().collect();
    state.profiles.store(std::sync::Arc::new(map));

    // Logged at WARN — this is the moment everything that could be pressed disappears.
    log::warn!(
        "profile '{}' DELETED over HTTP — {} buttons, {} regions, {} keys; kept at {}",
        prof.name,
        t.buttons.len(),
        t.regions.len(),
        t.keys.len(),
        archived.display()
    );
    Ok(json_ok(json!({
        "deleted": true,
        "profile": prof.name,
        "was": {"buttons": t.buttons.len(), "regions": t.regions.len(), "keys": t.keys.len()},
        "archived": archived.to_string_lossy(),
        "note": "the archive name carries a timestamp, so deleting the same name twice never \
                 overwrites the earlier copy. Nothing here deletes archives — a person does.",
        "profiles": remaining,
    })))
}

// ────────────────────────── the window's menu bar ──────────────────────────

fn menu_item_json(m: &crate::win::menu::MenuItem, guarded: &[String]) -> Value {
    let mut v = json!({
        "path": m.path,
        "label": m.label,
        "depth": m.depth,
        "enabled": m.enabled,
        "checked": m.checked,
        "submenu": m.submenu,
        "id": m.id,
    });
    if let Some(g) = crate::targets::menu_needs_confirm(&m.path, guarded) {
        v["confirm"] = json!(true);
        v["confirm_by"] = json!(g);
    }
    v
}

/// Advice shared by both menu endpoints when the bar is empty, so the two cannot drift apart.
const NO_MENU: &str = "this window has no menu bar that Windows can see. Either it has none, \
                       or it draws its own (a ribbon, a WPF/WinUI menu, a custom title bar) - \
                       and a drawn menu is pixels, so it is reached by clicking like anything \
                       else: GET /controls or a capture to find it, then a saved button.";

/// **The window's menu bar** — paths, command IDs and state. Presses nothing.
///
/// An empty list is an answer. Not every application has a menu Windows can see, and the reply
/// says which case this is rather than looking like a failure.
pub async fn menus(
    State(state): State<SharedState>,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Response, ApiError> {
    let prof = pick(&state, q_get(&q, "profile").as_deref(), Params::Query)?;
    tokio::task::spawn_blocking(move || {
        let t = prof.targets();
        let info = find_window(&t)?;
        let items = crate::win::menu::read(info.handle);
        let invocable = items.iter().filter(|m| !m.submenu && m.enabled).count();
        let mut out = json!({
            "profile": prof.name,
            "window": window_json(&info),
            "menus": items.iter().map(|m| menu_item_json(m, &t.confirm_menus)).collect::<Vec<_>>(),
            "count": items.len(),
            "invocable": invocable,
            "allow_menus": state.config.allow_menus,
        });
        if items.is_empty() {
            out["note"] = json!(NO_MENU);
        } else {
            out["note"] = json!(
                "'path' is what POST /menu takes. A 'submenu' entry is a place, not an action - \
                 its children are the actions. 'enabled' is the state the menu carries right \
                 now; an application that greys items out only as the menu opens will report \
                 everything enabled here, because nothing opens it."
            );
        }
        if !t.confirm_menus.is_empty() {
            out["confirm_menus"] = json!(t.confirm_menus);
        }
        Ok(json_ok(out))
    })
    .await
    .map_err(|e| ApiError::internal(format!("menu task failed: {e}")))?
}

/// What `POST /menu` was asked for.
#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct MenuReq {
    #[serde(default)]
    pub profile: Option<String>,
    /// `"Tool/Set Machine Parameters"`, as `GET /menus` prints it.
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub confirm: bool,
    #[serde(default)]
    pub capture: Option<String>,
    #[serde(default)]
    pub pad: Option<i32>,
    #[serde(default)]
    pub ignore: Option<Rect>,
    #[serde(default)]
    pub settle_ms: Option<u64>,
    #[serde(default)]
    pub scale: Option<f64>,
    #[serde(default)]
    pub max_width: Option<u32>,
}

/// **Pick one item from the window's menu bar.**
///
/// `POST /menu {"path": "Tool/Set Machine Parameters"}`
pub async fn menu(State(state): State<SharedState>, body: Bytes) -> Result<Response, ApiError> {
    if !state.config.allow_menus {
        return Err(ApiError::forbidden(
            "invoking the window's menu is disabled — set \"allow_menus\": true in config.json",
        )
        .with_detail(json!({
            "why": "a menu is read off the window rather than written in the profile, so this \
                    endpoint reaches whatever the application's menu reaches — a wider surface \
                    than the named buttons, which is why it has its own switch.",
            "hint": "GET /menus still works and presses nothing, so what is there can be read \
                     either way.",
        })));
    }
    let req: MenuReq = parse_body(&body)?;
    let path = req.path.clone().filter(|p| !p.trim().is_empty()).ok_or_else(|| {
        ApiError::bad_request("'path' is required — GET /menus lists them")
    })?;
    let prof = pick(&state, req.profile.as_deref(), Params::Body)?;

    // One input at a time, like a click: a menu command that opens a dialog changes what the
    // next press would land on.
    let _guard = state.input_lock.lock().await;
    let st = state.clone();
    tokio::task::spawn_blocking(move || menu_now(&st, &prof, req, path))
        .await
        .map_err(|e| ApiError::internal(format!("menu task failed: {e}")))?
}

fn menu_now(
    st: &SharedState,
    prof: &crate::state::Profile,
    req: MenuReq,
    path: String,
) -> Result<Response, ApiError> {
    let t = prof.targets();
    let info = find_window(&t)?;
    let items = crate::win::menu::read(info.handle);
    if items.is_empty() {
        return Err(ApiError::conflict("this window has no menu bar")
            .with_detail(json!({"note": NO_MENU})));
    }

    let wanted = path.trim().trim_matches('/');
    let Some(item) = items.iter().find(|m| m.path.eq_ignore_ascii_case(wanted)) else {
        return Err(ApiError::not_found(format!("no menu item at '{path}'")).with_detail(json!({
            "menu": near_names(wanted, items.iter().map(|m| m.path.clone()), "GET /menus"),
            "note": "the path is the full one, separated by '/', exactly as GET /menus prints \
                     it — the '&' that marks the underlined letter and the accelerator column \
                     are already stripped there, so do not type them.",
        })));
    };
    if item.submenu {
        return Err(ApiError::bad_request(format!("'{path}' is a submenu, not a command"))
            .with_detail(json!({
                "children": items
                    .iter()
                    .filter(|m| m.path.starts_with(&format!("{}/", item.path)))
                    .map(|m| m.path.clone())
                    .collect::<Vec<_>>(),
                "note": "a submenu is a place. Name one of the items inside it.",
            })));
    }
    if !item.enabled {
        // Posting the command anyway might work, and that is the problem: an application that
        // decides validity when the menu opens never gets to decide here. Refusing is the only
        // answer that cannot act on something the application had said no to.
        return Err(ApiError::conflict(format!("the menu item '{path}' is disabled"))
            .with_detail(json!({
                "path": item.path,
                "note": "the menu itself reports it greyed out. Posting the command anyway may \
                         still be acted on, because the application never sees the menu open - \
                         which is exactly why this is refused rather than attempted. Put the \
                         application into the mode where the item is available and ask again.",
            })));
    }
    let Some(id) = item.id else {
        return Err(ApiError::conflict(format!("the menu item '{path}' carries no command id")));
    };

    if let Some(g) = crate::targets::menu_needs_confirm(&item.path, &t.confirm_menus)
        && !req.confirm
    {
        return Err(ApiError::forbidden(format!(
            "'{}' is covered by confirm_menus ('{g}') — resend with \"confirm\": true if you \
             meant it",
            item.path
        ))
        .with_detail(json!({"path": item.path, "confirm_by": g})));
    }

    let pad = req.pad.unwrap_or(0).max(0);
    let offsets = anchor_offsets(&t, &info)?;
    let crop = match &req.capture {
        None => None,
        Some(r) => Some(Targets::pad_rect(
            t.region(r, info.client_size, &offsets).map_err(|e| {
                ApiError::bad_request(e).with_detail(json!({"known_regions": t.region_names()}))
            })?,
            pad,
        )),
    };
    let before = crop.and_then(|rect| shoot(&info, rect, req.scale, req.max_width).ok().map(|(f, _, _)| f));

    window::focus(info.handle).map_err(ApiError::conflict)?;
    log::info!(
        "MENU profile={} path={:?} id={id} window={:?} pid={}",
        prof.name, item.path, info.title, info.pid
    );
    crate::win::menu::invoke(info.handle, id).map_err(ApiError::internal)?;

    let settle = st.config.clamp_settle(req.settle_ms);
    std::thread::sleep(std::time::Duration::from_millis(settle));

    let info = window::describe(info.handle)
        .ok_or_else(|| ApiError::conflict("the target window disappeared"))?;
    let mut result = json!({
        "profile": prof.name,
        "menu": item.path,
        "id": id,
        "settle_ms": settle,
        "window": window_json(&info),
        "note": "the command was POSTED, not sent — a menu item that opens a modal dialog would \
                 otherwise hold this request open for as long as the dialog is on screen. So \
                 this says the application received it, not that it did anything. Capture to \
                 see. A dialog that opened is a NEW window, and this profile points at the old \
                 one: GET /windows to find it.",
    });
    if let (Some(rect), Some(region)) = (crop, req.capture.clone()) {
        let (frame, method, black) = shoot(&info, rect, req.scale, req.max_width)?;
        let (_png, mut meta) =
            deliver(st, &frame, method, black, &format!("{}_{}", prof.name, safe_label(&region)), true)?;
        if let Some(b) = before {
            apply_change(&mut meta, &b, &frame, req.ignore, None);
        }
        result["capture"] = meta;
    }
    Ok(json_ok(result))
}

// ────────────────────────── refit ──────────────────────────

/// What a refit was asked to do.
#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct RefitReq {
    /// Where an anchor is **now**, by anchor name — for the anchors this cannot work out for
    /// itself. See `find_anchor_now`.
    #[serde(default)]
    pub anchors: std::collections::BTreeMap<String, Rect>,
    /// Write the result. Absent or false, nothing is saved and the reply is the proposal.
    #[serde(default)]
    pub apply: Option<bool>,
    /// Save although the check found buttons that land on nothing.
    #[serde(default)]
    pub force: Option<bool>,
}

/// Where an anchor's control is on the window right now.
///
/// The ordinary match is text **and** size, and size is what identifies one control among
/// same-named ones. A refit exists precisely because the size changed, so that identifier is
/// the one thing not available here — and "the one nearest where it used to be" is the
/// reasoning anchors were designed to avoid, because it uses the possibly-stale rectangle to
/// find the thing that would prove it stale.
///
/// So: the exact match is preferred when it still exists (that anchor did not change), a
/// single control carrying the text is accepted, and anything else is **refused with the
/// candidates listed** so the caller can name the rectangle in `anchors`. Guessing is done
/// only where there is one answer to guess.
fn find_anchor_now(
    name: &str,
    a: &AnchorDef,
    controls: &[crate::win::window::ControlInfo],
    told: Option<&Rect>,
) -> Result<(Rect, &'static str), ApiError> {
    if let Some(r) = told {
        return Ok((*r, "named in the request"));
    }
    let same_text: Vec<&crate::win::window::ControlInfo> =
        controls.iter().filter(|c| c.text.trim() == a.text.trim()).collect();

    if let Some(c) = same_text.iter().find(|c| (c.rect[2], c.rect[3]) == (a.rect[2], a.rect[3])) {
        return Ok((c.rect, "text and size still match — this anchor did not change"));
    }
    match same_text.as_slice() {
        [c] => Ok((c.rect, "the only control carrying that text")),
        [] => Err(ApiError::not_found(format!(
            "anchor '{name}': no control on this window carries the text {:?}",
            a.text
        ))
        .with_detail(json!({
            "anchor": name,
            "text": a.text,
            "note": "the anchor cannot be re-seated because it is not there at all. This is a \
                     different application or a different build, not a resized one. GET \
                     /controls lists what is present.",
        }))),
        more => Err(ApiError::conflict(format!(
            "anchor '{name}': {} controls carry the text {:?} and none is still {}x{}, so which \
             one this anchor means cannot be worked out",
            more.len(),
            a.text,
            a.rect[2],
            a.rect[3]
        ))
        .with_detail(json!({
            "anchor": name,
            "was": a.rect,
            "candidates": more.iter().map(|c| c.rect).collect::<Vec<_>>(),
            "hint": format!(
                "name the right one: POST a body of {{\"anchors\": {{\"{name}\": [x,y,w,h]}}}}. \
                 Picking the nearest one here would use the rectangle that may be stale to \
                 decide which control proves it stale, and would move every coordinate \
                 belonging to this anchor if it guessed wrong."
            ),
        }))),
    }
}

/// **Re-seat a profile onto the window as it is now**, when a container changed size.
///
/// `POST /admin/profile/refit?profile=NAME` — a proposal by default, saved only with
/// `apply=true`, and refused even then if the check found buttons that land on nothing.
pub async fn admin_refit_profile(
    State(state): State<SharedState>,
    Query(q): Query<HashMap<String, String>>,
    body: Bytes,
) -> Result<Response, ApiError> {
    editing_allowed(&state)?;
    let req: RefitReq = parse_body(&body)?;
    let apply = req.apply.unwrap_or(false) || q_bool(&q, "apply")?.unwrap_or(false);
    let force = req.force.unwrap_or(false) || q_bool(&q, "force")?.unwrap_or(false);
    let prof = pick(&state, q_get(&q, "profile").as_deref(), Params::Query)?;

    tokio::task::spawn_blocking(move || refit_now(&prof, req, apply, force))
        .await
        .map_err(|e| ApiError::internal(format!("refit task failed: {e}")))?
}

fn refit_now(
    prof: &crate::state::Profile,
    req: RefitReq,
    apply: bool,
    force: bool,
) -> Result<Response, ApiError> {
    let t = prof.targets();
    if t.anchors.is_empty() {
        return Err(ApiError::bad_request(
            "this profile declares no anchors, so there is nothing to re-seat it against",
        )
        .with_detail(json!({
            "note": "a refit moves each element with the container it was measured inside. \
                     Without anchors there is no container, and the whole-window equivalent is \
                     already there: POST /window/fit restores the client area, or set \
                     reference_client to the size the coordinates were drawn at.",
        })));
    }
    // An anchor named in the request that is not in the profile is a typo, and a typo that
    // silently does nothing here would look exactly like a refit that decided to ignore it.
    for name in req.anchors.keys() {
        if !t.anchors.contains_key(name) {
            return Err(ApiError::not_found(format!("no anchor named '{name}' in this profile"))
                .with_detail(near_names(
                    name,
                    t.anchors.keys().cloned(),
                    "GET /admin/profile (anchors)",
                )));
        }
    }

    let (info, coord_scale, size_warning) = bind_window_view(&t)?;
    let list = window::enumerate_controls(info.handle);
    if list.items.is_empty() {
        return Err(ApiError::conflict(
            "this application has no child windows, so its containers cannot be found",
        )
        .with_detail(json!({
            "note": "GET /controls is empty: the application paints its own controls. An anchor \
                     is a real Win32 control, so neither finding one nor checking a button \
                     against one is possible here. Re-measure in /editor, and use GET \
                     /sheet.png to check the result button by button.",
        })));
    }

    // ── where each anchor is now ──
    let mut anchors_json = serde_json::Map::new();
    let mut moves: std::collections::HashMap<String, (Rect, Rect)> = Default::default();
    for (name, a) in &t.anchors {
        // The saved rect is in the profile's own coordinates; the control's is in live pixels.
        let was = Targets::scale_rect(a.rect, coord_scale);
        let (now, how) = find_anchor_now(name, a, &list.items, req.anchors.get(name))?;
        anchors_json.insert(
            name.clone(),
            json!({
                "text": a.text,
                "was": was,
                "now": now,
                "moved": [now[0] - was[0], now[1] - was[1]],
                "resized": [now[2] - was[2], now[3] - was[3]],
                "matched_by": how,
            }),
        );
        moves.insert(name.clone(), (was, now));
    }

    // ── move everything that belongs to one ──
    let mut fresh = (*t).clone();
    let mut fixed = Vec::new();
    let mut moved_regions = 0usize;
    let mut moved_buttons = 0usize;

    for (name, r) in &mut fresh.regions {
        let live = Targets::scale_rect(r.rect, coord_scale);
        match moves.get(&r.anchor) {
            Some((was, now)) => {
                r.rect = Targets::refit_rect(live, *was, *now);
                moved_regions += 1;
            }
            // `@fixed` is a statement somebody made — that this does not move with the rest —
            // so it is rescaled into the new reference and otherwise left exactly alone.
            None => {
                r.rect = live;
                fixed.push(format!("region:{name}"));
            }
        }
    }
    for (name, b) in &mut fresh.buttons {
        let live = Targets::scale_rect(b.rect, coord_scale);
        let live_point = b.point.map(|p| {
            [(p[0] as f64 * coord_scale.0).round() as i32, (p[1] as f64 * coord_scale.1).round() as i32]
        });
        match moves.get(&b.anchor) {
            Some((was, now)) => {
                b.rect = Targets::refit_rect(live, *was, *now);
                b.point = live_point.map(|p| Targets::refit_point(p, *was, *now));
                moved_buttons += 1;
            }
            None => {
                b.rect = live;
                b.point = live_point;
                fixed.push(format!("button:{name}"));
            }
        }
    }
    for (name, a) in &mut fresh.anchors {
        if let Some((_, now)) = moves.get(name) {
            a.rect = *now;
        }
    }
    // Everything above is now in live pixels, so this is the size they were measured at. Left
    // stale, on_size_mismatch would refuse every click against coordinates that are correct.
    let was_reference = fresh.reference_client;
    fresh.reference_client = Some([info.client_size.0, info.client_size.1]);

    // ── check it against the window ──
    let mut landed = 0usize;
    let mut failed: Vec<Value> = Vec::new();
    let mut worst: Option<(String, i32, Value)> = None;
    for (name, b) in &fresh.buttons {
        if !moves.contains_key(&b.anchor) {
            continue; // not moved, so not this call's claim to check
        }
        let (px, py) = Targets::click_point(b, (1.0, 1.0), (0, 0));
        match window::control_at(info.handle, px, py) {
            Some(h) if !h.is_window_itself => {
                landed += 1;
                // How far the new rectangle sits from the control it landed on. A perfect
                // refit puts them on top of each other; half a key's width means the point is
                // inside the NEIGHBOUR, which still "lands" and is still wrong.
                let c = |r: [i32; 4]| (r[0] + r[2] / 2, r[1] + r[3] / 2);
                let (bx, by) = c(b.rect);
                let (hx, hy) = c(h.rect);
                let d = (bx - hx).abs().max((by - hy).abs());
                if worst.as_ref().is_none_or(|(_, w, _)| d > *w) {
                    worst = Some((
                        name.clone(),
                        d,
                        json!({"button": name, "off_by": d, "rect": b.rect, "control": h.rect}),
                    ));
                }
            }
            other => failed.push(json!({
                "button": name,
                "point": [px, py],
                "reason": match other {
                    Some(_) => "the point landed on the window itself — no control sits there",
                    None => "nothing at all is at that point",
                },
                "rect": b.rect,
            })),
        }
    }

    let checked = landed + failed.len();
    let mut out = json!({
        "profile": prof.name,
        "applied": false,
        "client": [info.client_size.0, info.client_size.1],
        "anchors": Value::Object(anchors_json),
        "moved": {"buttons": moved_buttons, "regions": moved_regions},
        "reference_client": {"was": was_reference, "now": fresh.reference_client},
        "verify": {
            "checked": checked,
            "landed": landed,
            "failed": failed,
            "worst_offset": worst.as_ref().map(|(_, _, v)| v.clone()),
            "note": "each moved button's new click point was looked up on the live window. \
                     'landed' only means a control is there — 'worst_offset' is the one to \
                     read, because a point half a key off still lands, on the NEIGHBOUR. \
                     Anything more than a few pixels means the layout did not merely scale, \
                     and the refit is not the answer for this application.",
        },
    });
    if !fixed.is_empty() {
        out["untouched"] = json!({
            "elements": fixed,
            "note": "these are @fixed — somebody stated they do not move with a container, so \
                     they were rescaled into the new reference_client and otherwise left alone. \
                     If the window itself changed size, check them by hand: GET /sheet.png.",
        });
    }
    if let Some(w) = size_warning {
        out["size_mismatch"] = json!(w);
    }

    if !apply {
        out["hint"] = json!(
            "nothing was written. Read 'verify', then repeat with ?apply=true to save. The \
             previous file is kept as a backup either way."
        );
        return Ok(json_ok(out));
    }
    if !failed.is_empty() && !force {
        return Err(ApiError::conflict(format!(
            "{} of {checked} moved buttons land on nothing, so the refit was NOT saved",
            failed.len()
        ))
        .with_detail(json!({
            "verify": out["verify"],
            "anchors": out["anchors"],
            "hint": "the proportional move is an assumption about the application, and this is \
                     the assumption failing. Look at the named buttons first. If they are keys \
                     the application genuinely does not make controls for, and the rest are \
                     right, repeat with force=true.",
            "note": "nothing was written — the profile on disk is unchanged",
        })));
    }

    fresh.validate().map_err(|e| {
        ApiError::new(StatusCode::UNPROCESSABLE_ENTITY, format!("the refitted profile is invalid: {e}"))
    })?;
    fresh.save(&prof.path).map_err(ApiError::internal)?;
    prof.targets.store(std::sync::Arc::new(fresh));
    log::info!(
        "REFIT profile={} anchors={} buttons={moved_buttons} regions={moved_regions} failed={}",
        prof.name,
        t.anchors.len(),
        failed.len()
    );
    out["applied"] = json!(true);
    out["forced"] = json!(!failed.is_empty());
    Ok(json_ok(out))
}

/// **Rename** a profile.
///
/// One might ask why not just save under the new name — because that **copies**. The old one
/// stays, two profiles point at one window, and omitting `profile`, which only works with
/// exactly one, breaks that moment. Renaming has to be atomic for none of that to happen.
pub async fn admin_rename_profile(
    State(state): State<SharedState>,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Response, ApiError> {
    editing_allowed(&state)?;
    let Some(to) = q_get(&q, "to") else {
        return Err(ApiError::bad_request("rename to what? pass &to=NEWNAME"));
    };
    if !crate::config::is_safe_profile_name(&to) {
        return Err(ApiError::bad_request(format!(
            "'{to}' is not a valid profile name — letters, digits, '-' and '_' only"
        )));
    }
    let prof = pick(&state, q_get(&q, "profile").as_deref(), Params::Query)?;
    if prof.name == to {
        return Err(ApiError::bad_request(format!("'{to}' is already its name")));
    }
    not_the_configured_default(&state, &prof.name)?;

    let dest = crate::config::profile_path(&to);
    // Refuse if the file exists even when it is not in the list — overwriting would make
    // that file quietly disappear.
    if state.profiles.load().contains_key(&to) || dest.exists() {
        return Err(ApiError::conflict(format!("'{to}' already exists")).with_detail(json!({
            "path": dest.to_string_lossy(),
            "hint": "pick another name, or delete that profile first",
        })));
    }

    let from = prof.path.clone();
    let d = dest.clone();
    tokio::task::spawn_blocking(move || crate::targets::move_profile_files(&from, &d))
        .await
        .map_err(|e| ApiError::internal(format!("rename task failed: {e}")))?
        .map_err(ApiError::internal)?;

    let moved = std::sync::Arc::new(crate::state::Profile {
        name: to.clone(),
        path: dest.clone(),
        targets: arc_swap::ArcSwap::from(prof.targets()),
    });
    let mut map = (**state.profiles.load()).clone();
    map.remove(&prof.name);
    map.insert(to.clone(), moved);
    let names: Vec<String> = map.keys().cloned().collect();
    state.profiles.store(std::sync::Arc::new(map));

    log::warn!("profile '{}' RENAMED to '{to}' ({})", prof.name, dest.display());
    Ok(json_ok(json!({
        "renamed": true,
        "from": prof.name,
        "to": to,
        "path": dest.to_string_lossy(),
        "note": "callers using the old name now get 404 with the known list — nothing is \
                 silently redirected",
        "profiles": names,
    })))
}

/// Re-read the profiles, so coordinates can be corrected without a restart.
/// **A bad file is not applied** — if parsing or validation fails, the old definition lives on.
pub async fn admin_reload(
    State(state): State<SharedState>,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Response, ApiError> {
    // With no `?profile=` this **rescans the disk** — drop in a new
    // profiles/deescreen.<name>.json, call this, and the profile exists without a restart.
    // Naming one re-reads only that one.
    let Some(name) = q_get(&q, "profile") else {
        let st = state.clone();
        let (fresh, notes) =
            tokio::task::spawn_blocking(move || crate::state::build_profiles(&st.config))
                .await
                .map_err(|e| ApiError::internal(format!("rescan task failed: {e}")))?;

        // A rescan that would lose the default profile the config **named** is refused —
        // accepting it would quietly send every request that omits `profile` to a different
        // window. With none named, the omit rule is "only when there is exactly one", so there
        // is nothing to protect.
        if !state.default_profile.is_empty() && !fresh.contains_key(&state.default_profile) {
            return Err(ApiError::conflict(format!(
                "after rescanning, the default profile '{}' would be gone",
                state.default_profile
            ))
            .with_detail(json!({
                "found": fresh.keys().cloned().collect::<Vec<_>>(),
                "note": "the previous profiles are still in effect",
            })));
        }
        let names: Vec<String> = fresh.keys().cloned().collect();
        state.profiles.store(std::sync::Arc::new(fresh));
        log::info!("profiles rescanned: {}", names.join(", "));
        return Ok(json_ok(json!({
            "rescanned": true,
            "profiles": names,
            "default_profile": state.effective_default(),
            "notes": notes,
        })));
    };

    let prof = pick(&state, Some(&name), Params::Query)?;
    let path = prof.path.clone();
    let fresh = tokio::task::spawn_blocking(move || Targets::load(&path))
        .await
        .map_err(|e| ApiError::internal(format!("reload task failed: {e}")))?
        .map_err(|e| {
            ApiError::new(StatusCode::UNPROCESSABLE_ENTITY, e)
                .with_detail(json!({"note": "the previous targets are still in effect"}))
        })?;

    let summary = json!({
        "reloaded": true,
        "profile": prof.name,
        "path": prof.path.to_string_lossy(),
        "window": fresh.window,
        "buttons": fresh.buttons.len(),
        "regions": fresh.regions.len(),
        "keys": fresh.keys.len(),
    });
    prof.targets.store(std::sync::Arc::new(fresh));
    log::info!("profile '{}' reloaded from {}", prof.name, prof.path.display());
    Ok(json_ok(summary))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lbl(x: i32, y: i32, w: i32, h: i32, t: &str) -> (i32, i32, i32, i32, String, draw::Color) {
        (x, y, w, h, t.to_string(), draw::TARGET)
    }

    /// Placed labels **must not overlap each other.** Overlapping, two names read as one
    /// (measured: `"MDI_CASE_TMDI_Z"`), and from the picture there is no telling whether that
    /// is two run together or a name in its own right. So this is not an aesthetic problem but
    /// a **wrong information** problem.
    #[test]
    fn labels_do_not_overlap_each_other() {
        let mut img = image::RgbaImage::new(400, 300);
        // Names far wider than their rectangles, 3px apart — an arrangement that cannot
        // avoid overlapping
        let labels: Vec<_> = (0..12)
            .map(|i| lbl(10 + i * 3, 100, 4, 4, &format!("MDI_CASE_TOGGLE_{i}")))
            .collect();

        let out = place_labels(&mut img, &labels);

        // **What gets placed never overlaps** — the placer's one and only promise.
        for (i, a) in out.placed.iter().enumerate() {
            for b in &out.placed[i + 1..] {
                assert!(!draw::boxes_overlap(*a, *b), "{a:?} and {b:?} overlap");
            }
        }
        // Out of room, it falls back to numbers — it never quietly draws them on top of each
        // other.
        assert!(out.collided > 0, "this arrangement cannot fit every name");
        assert!(out.collided < labels.len(), "all of them failing means the placer did nothing");
    }

    /// Given room, it staggers above and below to **keep the names**.
    #[test]
    fn crowded_but_solvable_layouts_stagger_instead_of_giving_up() {
        let mut img = image::RgbaImage::new(600, 300);
        // Touching horizontally, but few enough to spread across the four rows
        let labels: Vec<_> =
            (0..4).map(|i| lbl(10 + i * 20, 100, 18, 30, &format!("KEY_SWITCH_{i}"))).collect();
        let out = place_labels(&mut img, &labels);
        assert_eq!(out.collided, 0, "four spots exist, so every one should keep its name");
        assert_eq!(out.placed.len(), 4);
    }

    /// With space to spare every name stays — falling back to a number is a last resort.
    #[test]
    fn roomy_layouts_keep_every_name() {
        let mut img = image::RgbaImage::new(400, 400);
        let labels: Vec<_> = (0..4).map(|i| lbl(10, 10 + i * 90, 200, 40, "CYCLE_START")).collect();
        assert_eq!(place_labels(&mut img, &labels).collided, 0);
    }

    /// Draw the overlay over a synthetic capture and check, in pixels, **what each mode draws
    /// and what it does not**.
    fn overlay_probe(mode: &str) -> (image::RgbaImage, Value) {
        let mut t = crate::targets::Targets {
            description: String::new(),
            window: crate::win::window::WindowSpec {
                title: "x".into(),
                title_exact: false,
                class: String::new(),
            },
            reference_client: None,
            on_size_mismatch: crate::targets::SizeMismatch::Ignore,
            anchors: std::collections::BTreeMap::new(),
            regions: std::collections::BTreeMap::new(),
            buttons: std::collections::BTreeMap::new(),
            confirm_menus: Vec::new(),
            shift: None,
            keys: std::collections::BTreeMap::new(),
        };
        t.buttons.insert(
            "CYCLE_START".to_string(),
            crate::targets::ButtonDef {
                rect: [20, 20, 60, 60],
                point: None,
                click_button: "left".into(),
                double: false,
                confirm: false,
                anchor: String::new(),
                hold_ms: None,
                types: String::new(),
                shift_types: String::new(),
                settle_ms: None,
                note: String::new(),
            },
        );
        let mut frame = crate::captures::Frame {
            image: image::RgbaImage::from_pixel(200, 200, image::Rgba([9, 9, 9, 255])),
            source_rect: [0, 0, 200, 200],
            scale: 1.0,
        };
        let mut q = HashMap::new();
        q.insert("buttons".to_string(), mode.to_string());
        let ov = Overlay::from_query(&q).expect("overlay");
        let drawn = apply_overlay(&mut frame, &ov, Some(&t), (1.0, 1.0), (200, 200), &AnchorOffsets::new());
        (frame.image, drawn)
    }

    /// The crosshair sits on the click point = usually the middle of the key = **on top of the
    /// key's legend**. So it has to be switchable off, and off has to really mean not drawn.
    #[test]
    fn box_mode_draws_the_outline_without_the_crosshair() {
        // The click point is the rectangle's centre (50,50). The crosshair paints around the
        // centre pixel and leaves that one alone, so this samples a few px to the side. That is
        // **inside** the rectangle, where the translucent fill is already down, so the modes are
        // compared against each other rather than against the original background.
        let at = |m: &str| overlay_probe(m).0.get_pixel(50, 46).0;

        assert_ne!(at("1"), at("box"), "only buttons=1 should draw the crosshair");
        assert_eq!(at("box"), at("num"), "box and num should both be without it");

        // All three still draw the rectangle — the crosshair is the only thing switched off.
        let bg = [9u8, 9, 9, 255];
        for m in ["1", "box", "num"] {
            assert_ne!(overlay_probe(m).0.get_pixel(20, 20).0, bg, "outline for buttons={m}");
        }
    }

    #[test]
    fn overlay_reports_which_mode_it_drew() {
        for (q, want) in [("1", "full"), ("box", "box"), ("num", "num")] {
            assert_eq!(overlay_probe(q).1["buttons_mode"], json!(want));
        }
        // The numbered mode has to say for itself that no legend table is needed — otherwise
        // the caller goes looking for a way to turn a number back into a name and stops.
        assert!(overlay_probe("num").1["numbering"].is_string());
        assert!(overlay_probe("box").1["numbering"].is_null());
    }

    /// Coordinates must not get past confirm.
    ///
    /// A rectangle computed from a capture landing on the emergency stop happens by accident.
    /// If a person's "think twice about this one" quietly is not there in that moment, the mark
    /// is decoration that only applies when you call it by name.
    #[test]
    fn raw_coordinates_do_not_slip_past_a_confirm_button() {
        let mut t = crate::targets::Targets {
            description: String::new(),
            window: crate::win::window::WindowSpec {
                title: "x".into(),
                title_exact: false,
                class: String::new(),
            },
            reference_client: None,
            on_size_mismatch: crate::targets::SizeMismatch::Ignore,
            anchors: std::collections::BTreeMap::new(),
            regions: std::collections::BTreeMap::new(),
            buttons: std::collections::BTreeMap::new(),
            confirm_menus: Vec::new(),
            shift: None,
            keys: std::collections::BTreeMap::new(),
        };
        let def = |rect, confirm| crate::targets::ButtonDef {
            rect,
            point: None,
            click_button: "left".into(),
            double: false,
                anchor: String::new(),
                hold_ms: None,
                types: String::new(),
                shift_types: String::new(),
            confirm,
            settle_ms: None,
            note: String::new(),
        };
        t.buttons.insert("estop".into(), def([100, 100, 60, 60], true));
        t.buttons.insert("jog".into(), def([200, 100, 60, 60], false));

        let one = (1.0, 1.0);
        assert_eq!(confirm_button_at(&t, (130, 130), one).map(|(n, _)| n.as_str()), Some("estop"));
        assert_eq!(confirm_button_at(&t, (100, 100), one).map(|(n, _)| n.as_str()), Some("estop"));
        // the edge is exclusive — 159 is inside, 160 is outside
        assert!(confirm_button_at(&t, (159, 159), one).is_some());
        assert!(confirm_button_at(&t, (160, 160), one).is_none());
        // a button without confirm is not blocked
        assert!(confirm_button_at(&t, (230, 130), one).is_none());
        // nor is empty space
        assert!(confirm_button_at(&t, (10, 10), one).is_none());

        // With a factor applied because the window size differs, it still has to guard the same
        // place — applying the factor on one side only makes the test wrong on a scaled screen.
        let two = (2.0, 2.0);
        assert!(confirm_button_at(&t, (260, 260), two).is_some(), "inside, after scaling");
        assert!(confirm_button_at(&t, (130, 130), two).is_none(), "outside, after scaling");
    }

    /// Translation strings interpolate with {0}, and both languages agree on how many.
    ///
    /// `t()` substitutes {0}, {1}, {2}; it has never looked at anything else. Sixteen strings
    /// written with $1 printed it literally - the anchor message ended with a visible "$1"
    /// where the reason should have been, so the explanation was fetched, formatted into the
    /// sentence, and then left out of it. Nothing failed; it just said less than it meant to.
    ///
    /// The second half catches the subtler one: if English takes two values and Korean takes
    /// one, the Korean reader loses a number and nobody notices until they switch language.
    #[test]
    fn translations_interpolate_the_way_the_page_substitutes() {
        let html = include_str!("editor.html");

        let mut wrong_form = Vec::new();
        let mut slots: std::collections::BTreeMap<String, Vec<std::collections::BTreeSet<u32>>> =
            std::collections::BTreeMap::new();

        for line in html.lines() {
            let t = line.trim_end();
            let Some(rest) = t.strip_prefix("    '") else { continue };
            let Some((key, after)) = rest.split_once('\'') else { continue };
            if !after.starts_with(':') || key.is_empty() {
                continue;
            }
            if !key.chars().all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '_') {
                continue;
            }
            if after.contains('$') && after.chars().any(|c| c.is_ascii_digit()) {
                // Only flag a digit immediately after the sigil - a lone $ is just text.
                let bytes: Vec<char> = after.chars().collect();
                for w in bytes.windows(2) {
                    if w[0] == '$' && w[1].is_ascii_digit() {
                        wrong_form.push(format!("{key}: {}", &after[..after.len().min(60)]));
                        break;
                    }
                }
            }
            let mut found = std::collections::BTreeSet::new();
            let chars: Vec<char> = after.chars().collect();
            for (i, c) in chars.iter().enumerate() {
                if *c == '{' && i + 2 < chars.len() && chars[i + 1].is_ascii_digit() && chars[i + 2] == '}' {
                    found.insert(chars[i + 1].to_digit(10).unwrap_or(0));
                }
            }
            slots.entry(key.to_string()).or_default().push(found);
        }

        assert!(
            wrong_form.is_empty(),
            "t() substitutes {{0}}, not the other kind. These print the placeholder:\n{}",
            wrong_form.join("\n")
        );

        let disagree: Vec<String> = slots
            .iter()
            .filter(|(_, uses)| uses.len() == 2 && uses[0] != uses[1])
            .map(|(k, uses)| format!("{k}: en{:?} ko{:?}", uses[0], uses[1]))
            .collect();
        assert!(
            disagree.is_empty(),
            "one language would drop a value the other shows:\n{}",
            disagree.join("\n")
        );
    }

    /// The editor is served as bytes, so nothing that runs on this side ever parses it.
    ///
    /// A build, clippy and eighty tests all passed while its script carried a syntax error
    /// and the page did nothing whatsoever — the cause was a translation string written
    /// across two lines, which a single-quoted JS string cannot be. That failure is silent
    /// twice over: the script dies before it can draw its own error, so the page sits on
    /// "loading…" looking exactly like a server that never answered.
    ///
    /// This checks the shape those tables are actually written in — one entry, one line —
    /// rather than trying to be a JavaScript parser. For the real thing during development:
    /// pull the <script> out and run `node --check` on it.
    #[test]
    fn every_translation_entry_is_one_line() {
        let html = include_str!("editor.html");
        let mut bad = Vec::new();
        for (n, line) in html.lines().enumerate() {
            let t = line.trim_end();
            let is_entry = t.starts_with("    '")
                && t.strip_prefix("    '")
                    .and_then(|r| r.split_once("'"))
                    .is_some_and(|(k, rest)| {
                        rest.starts_with(':')
                            && !k.is_empty()
                            && k.chars().all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '_')
                    });
            if is_entry && !(t.ends_with(',') || t.ends_with('}')) {
                bad.push(format!("editor.html:{}: {}", n + 1, &t[..t.len().min(72)]));
            }
        }
        assert!(
            bad.is_empty(),
            "a translation entry runs past its line, which breaks the whole script:\n{}",
            bad.join("\n")
        );
    }

    /// Messages must not carry the indentation of the source they were written in.
    ///
    /// A `\` at the end of a line inside a Rust string literal swallows the newline and the
    /// next line's leading spaces. Lose that one character — an editor reflowing, a script
    /// rewriting the file — and the literal silently becomes one long line with a run of
    /// spaces wedged into the middle of a sentence. It compiles. It ships. It is visible only
    /// in the JSON somebody else receives, which is why it had gone unnoticed in four
    /// `changed_hint` strings until this test was written.
    ///
    /// The manual is a raw string whose columns line up on purpose, so it is skipped.
    #[test]
    fn no_message_carries_its_own_indentation() {
        // Every file that writes messages a caller reads. This keeps happening — a `\` at the
        // end of a line is invisible in a diff and survives compilation either way, so the
        // only thing that catches it is looking at every string literal.
        let files: [(&str, &str); 4] = [
            ("api.rs", include_str!("api.rs")),
            ("targets.rs", include_str!("targets.rs")),
            ("config.rs", include_str!("config.rs")),
            ("sheet.rs", include_str!("sheet.rs")),
        ];
        let mut bad = Vec::new();
        for (file, src) in files {
            let mut in_manual = false;
            for (n, line) in src.lines().enumerate() {
                if line.contains("r#\"deescreen v{version}") {
                    in_manual = true;
                } else if in_manual && line.trim_start().starts_with("\"#") {
                    in_manual = false;
                }
                if in_manual {
                    continue;
                }
                // Odd-numbered pieces of a split on `"` are the insides of string literals.
                for body in line.split('"').skip(1).step_by(2) {
                    let squashed = body.trim();
                    if squashed.contains("    ") && squashed.split("    ").count() > 1 {
                        let joined_words = squashed
                            .split("    ")
                            .filter(|p| !p.is_empty())
                            .count()
                            > 1;
                        if joined_words && !squashed.starts_with('-') && !squashed.contains("{:?}") {
                            bad.push(format!("{file}:{}: {}", n + 1, &squashed[..squashed.len().min(70)]));
                        }
                    }
                }
            }
        }
        assert!(bad.is_empty(), "a lost line-continuation left indentation inside a message:\n{}", bad.join("\n"));
    }

    /// The layout-drift check: same size elsewhere is drift, a different size says nothing.
    ///
    /// Measured on NC Trainer2 plus, which moves its whole panel 16px sideways between
    /// relaunches without changing its window size. Nothing else in this server notices — the
    /// size check compares the window, `hit` only asks whether a control is there — so this
    /// comparison is the only thing standing between a caller and a neighbouring key.
    ///
    /// The quiet half matters as much. A rectangle drawn by hand in the editor sits around a
    /// control rather than on it, so its size differs and no conclusion is available. Reporting
    /// drift there would be a false alarm on every press of every hand-made profile.
    #[test]
    fn a_control_of_the_same_size_somewhere_else_is_drift() {
        use crate::win::window::ControlHit;
        let hit = |rect: Rect| ControlHit {
            rect,
            class: "Button".into(),
            text: String::new(),
            enabled: true,
            visible: true,
            is_window_itself: false,
            hwnd: 1,
            id: 0,
            depth: 3,
        };
        let saved = [58, 882, 32, 18];

        // Exactly where the file says.
        let v = aim_json(Some(saved), Some(&hit(saved))).expect("a claim to check");
        assert_eq!(v["matches"], json!(true));

        // The 16px shift this exists for.
        let v = aim_json(Some(saved), Some(&hit([74, 882, 32, 18]))).expect("a claim to check");
        assert_eq!(v["matches"], json!(false));
        assert_eq!(v["delta"], json!([16, 0]));
        assert_eq!(v["saved"], json!(saved));
        assert_eq!(v["found"], json!([74, 882, 32, 18]));

        // A different size is a hand-drawn rectangle, not a moved control: say nothing.
        assert!(aim_json(Some(saved), Some(&hit([58, 882, 40, 24]))).is_none());
        assert!(aim_json(Some(saved), Some(&hit([74, 882, 40, 24]))).is_none());

        // Nothing to compare: coordinates the request supplied, or background.
        assert!(aim_json(None, Some(&hit(saved))).is_none());
        assert!(aim_json(Some(saved), None).is_none());
        let mut bg = hit(saved);
        bg.is_window_itself = true;
        assert!(aim_json(Some(saved), Some(&bg)).is_none(), "the window itself is not a control");
    }

    /// `rect` in the reading form is a rectangle, and stays one however the stored form grows.
    ///
    /// A region used to BE a rectangle, so this endpoint wrote the stored value straight under
    /// the key `rect`. When regions gained an anchor and became a struct, that line kept
    /// working and started returning a rect containing a rect. Nothing failed on this side —
    /// the break was in every consumer that did `const [x,y,w,h] = r.rect`, which is what the
    /// editor does, and it took a person opening the page to find out.
    ///
    /// So this asserts the shape, not the code path: four numbers, at the top, with the other
    /// fields beside them rather than wrapped around them.
    #[test]
    fn the_reading_form_keeps_rect_a_rectangle() {
        let t: Targets = serde_json::from_str(
            r#"{"window":{"title":"x"},
                "anchors":{"screen":{"text":"PANEL","rect":[0,0,10,10]}},
                "regions":{"bar":{"rect":[0,940,1280,60],"anchor":"screen","note":"n"}},
                "buttons":{"A":{"rect":[1,2,3,4],"anchor":"@fixed"}}}"#,
        )
        .expect("parses");
        let v = defs_view(&t);

        let region = &v["regions"][0];
        assert_eq!(region["rect"], json!([0, 940, 1280, 60]), "four numbers, not an object");
        assert!(region["rect"].is_array(), "a consumer destructures this: {}", region["rect"]);
        assert_eq!(region["anchor"], json!("screen"), "beside the rect, not wrapped around it");
        assert_eq!(region["note"], json!("n"));

        let button = &v["buttons"][0];
        assert_eq!(button["rect"], json!([1, 2, 3, 4]));
        assert!(button["rect"].is_array());
        assert_eq!(button["anchor"], json!("@fixed"), "the flat form shows the anchor too");
    }

    /// The reply to a wrong name has to be the near ones, and only the near ones.
    ///
    /// Listing all 140 was 1.4 kB per typo and put the answer somewhere inside a wall. The
    /// trap in replacing it is a suggestion list that is itself noise: containment alone made
    /// `Y` a candidate for `CYCLE_STAR`, because `cYcle` contains a y.
    #[test]
    fn a_wrong_name_is_answered_with_the_near_ones() {
        let names = [
            "CYCLE_START", "CYCLE_STOP", "EMERGENCY", "MDI_PERIOD", "MDI_9", "MDI_0",
            "SOFTKEY_01", "SOFTKEY_10", "X", "Y", "Z",
        ];
        let all = || names.iter().map(|s| s.to_string());
        let of = |v: &Value, k: &str| -> Vec<String> {
            v.get(k)
                .and_then(Value::as_array)
                .map(|a| a.iter().filter_map(|x| x.as_str().map(str::to_string)).collect())
                .unwrap_or_default()
        };

        // Wrong case is the cheapest miss, so the right name comes first.
        let v = near_names("mdi_period", all(), "GET /buttons");
        assert_eq!(of(&v, "did_you_mean").first().map(String::as_str), Some("MDI_PERIOD"));

        // A dropped letter.
        let v = near_names("CYCLE_STAR", all(), "GET /buttons");
        let near = of(&v, "did_you_mean");
        assert_eq!(near.first().map(String::as_str), Some("CYCLE_START"));
        assert!(!near.iter().any(|n| n == "Y"), "a one-letter name is not a suggestion: {near:?}");

        // Every reply carries the size of the list and where to read it, so a caller who
        // really does want all of them knows the number and the call.
        assert_eq!(v["known"], json!(names.len()));
        assert_eq!(v["all"], json!("GET /buttons"));

        // The right family, the wrong word for the thing — no distance finds DOT -> PERIOD,
        // so the prefix count is what turns it into one filtered look.
        let v = near_names("MDI_DOT", all(), "GET /buttons");
        assert_eq!(v["same_prefix"]["prefix"], json!("MDI_"));
        assert_eq!(v["same_prefix"]["count"], json!(3));

        // A name that exists needs no prefix hint, and nothing close means no guesses at all
        // rather than five arbitrary ones.
        let v = near_names("qqqqqqqq", all(), "GET /buttons");
        assert!(v.get("did_you_mean").is_none(), "no near names: {v}");
        assert!(v.get("same_prefix").is_none(), "no underscore, no family: {v}");
    }

    /// The suggested settle is the number that ends up in a profile, so it errs long. Measured
    /// once on a quiet machine it has to hold on a busy one, and the two ways of being wrong
    /// are not symmetric: too long costs a wait, too short returns the screen from before the
    /// press and calls it the result.
    #[test]
    fn the_suggested_settle_leaves_room_and_is_never_finer_than_the_measurement() {
        // 850ms measured -> a quarter more, rounded up to 50.
        assert_eq!(suggest_settle(850, 60), 1100);
        // Rounding is always upwards, never to the nearest.
        assert_eq!(suggest_settle(800, 60), 1000);
        assert_eq!(suggest_settle(4, 60), 150);

        // A screen that never moved still gets a floor. Suggesting 0 would read as "no wait
        // needed", which is a claim about the machine that one quiet measurement cannot make.
        assert!(suggest_settle(0, 60) >= 120, "{}", suggest_settle(0, 60));
        for last in [0u64, 1, 250, 900, 5000] {
            assert!(suggest_settle(last, 60) >= last, "{last} came back shorter than measured");
        }
    }

    /// `quiet_ms` at or above the ceiling can never be reached, so the measurement would time
    /// out every single time while looking like it had been configured. Clamped instead, and
    /// clamped to half so there is room for an answer rather than just for the wait.
    #[test]
    fn a_stillness_longer_than_the_wait_is_clamped_rather_than_guaranteed_to_fail() {
        let cfg = Config { max_settle_ms: 2000, ..Config::starter() };

        let (quiet, ceiling) = watch_bounds(&cfg, None);
        assert_eq!(ceiling, 2000);
        assert_eq!(quiet, DEFAULT_QUIET_MS);

        // Asked for longer than the whole wait.
        let (quiet, ceiling) = watch_bounds(&cfg, Some(9_000));
        assert!(quiet <= ceiling / 2, "quiet {quiet} of ceiling {ceiling}");

        // And below one sample, which would call every gap between two shots "still".
        let (quiet, _) = watch_bounds(&cfg, Some(1));
        assert_eq!(quiet, WATCH_POLL_MS);
    }

    /// A flat frame at 1:1, for comparing against another one.
    fn frame_of(w: u32, h: u32, grey: u8) -> Frame {
        Frame {
            image: image::RgbaImage::from_pixel(w, h, image::Rgba([grey, grey, grey, 255])),
            source_rect: [0, 0, w as i32, h as i32],
            scale: 1.0,
        }
    }

    /// One press's record says what moved and where, and nothing about whether it worked.
    /// A toggle already in that state, a key with no legend to repaint and a key that never
    /// arrived are the same picture; claiming otherwise would put a guess in the field a
    /// caller reads to find the press that failed.
    #[test]
    fn a_press_record_reports_the_change_without_judging_it() {
        let a = frame_of(20, 10, 200);
        let same = frame_of(20, 10, 200);
        let mut moved = frame_of(20, 10, 200);
        moved.image.put_pixel(4, 3, image::Rgba([0, 0, 0, 255]));

        let quiet = press_change(&a, &same, None);
        assert_eq!(quiet["changed"], json!(false));
        assert_eq!(quiet["pixels"], json!(0));
        // No bbox for a change that did not happen — an all-zero rectangle would read as a
        // change at the origin.
        assert_eq!(quiet["bbox"], Value::Null);
        assert!(quiet.get("failed").is_none(), "no verdict: {quiet}");
        assert!(quiet.get("ok").is_none(), "no verdict: {quiet}");

        let busy = press_change(&a, &moved, None);
        assert_eq!(busy["changed"], json!(true));
        assert_eq!(busy["pixels"], json!(1));
        assert_eq!(busy["bbox"], json!([4, 3, 1, 1]));

        // `ignore` drops a rectangle from the comparison, which is how a blinking cursor
        // stops making every press look like it did something.
        let blind = press_change(&a, &moved, Some([4, 3, 1, 1]));
        assert_eq!(blind["changed"], json!(false));
    }

    /// A window resized mid-sequence makes the two shots incomparable. That is reported as a
    /// change with the reason, not as "nothing happened" — the one reading that would send a
    /// caller looking at the key instead of at the window.
    #[test]
    fn a_resize_between_presses_is_not_reported_as_stillness() {
        let a = frame_of(20, 10, 200);
        let b = frame_of(30, 10, 200);
        let v = press_change(&a, &b, None);
        assert_eq!(v["changed"], json!(true));
        assert!(v["note"].as_str().unwrap_or_default().contains("resized"), "{v}");
    }

    /// The sentence a refusal carries has to name the place the caller is actually standing in.
    /// `/click` reads a body and `/click.png` reads a query string, and neither reads both, so
    /// the one message that mentioned both was wrong half the time - and wrong precisely for
    /// the caller who had done the right thing and still been refused.
    #[test]
    fn a_refusal_names_the_place_that_endpoint_reads() {
        let q = Params::Query.profile_hint();
        let b = Params::Body.profile_hint();

        // Each names its own place and not the other. Said plainly because "mentions the word
        // query" is the whole content of the bug: the old sentence mentioned both.
        assert!(q.contains("?profile=NAME"), "{q}");
        assert!(!q.contains("body") || q.contains("does not read a body"), "{q}");
        assert!(b.contains(r#""profile""#), "{b}");
        assert!(!b.contains("?profile="), "{b}");
        assert_ne!(q, b);

        // A message split across source lines must not carry the indentation with it.
        for h in [q, b] {
            assert!(!h.contains("  "), "doubled spaces in: {h}");
        }

        // The wiring. A query-built request says query; a body-parsed one says body - which is
        // the default, and is why the query constructors have to set it explicitly.
        let empty = std::collections::HashMap::new();
        assert_eq!(ClickReq::from_query(&empty).expect("empty query").params, Params::Query);
        assert_eq!(CaptureReq::from_query(&empty).expect("empty query").params, Params::Query);

        let from_body: ClickReq = parse_body(&Bytes::from_static(b"{}")).expect("empty body");
        assert_eq!(from_body.params, Params::Body);
        let from_body: CaptureReq = parse_body(&Bytes::from_static(b"{}")).expect("empty body");
        assert_eq!(from_body.params, Params::Body);

        // And `params` is ours, not the caller's: a body naming it is refused like any other
        // unknown field rather than talking the server into the wrong advice.
        assert!(parse_body::<ClickReq>(&Bytes::from_static(br#"{"params":"Query"}"#)).is_err());
    }

    /// Losing a confirm flag is detected whether the flag was cleared or the whole button
    /// went away. Both end the same way — that name, and any raw coordinate inside it, stop
    /// being protected — so an edit endpoint that noticed only one of them would leave the
    /// other as the way around it.
    #[test]
    fn taking_a_confirm_flag_away_is_noticed_either_way() {
        let doc = r#"{"window":{"title":"x"},"buttons":{
            "E":{"rect":[1,2,30,40],"confirm":true},
            "S":{"rect":[5,6,7,8],"confirm":true},
            "P":{"rect":[9,9,9,9]}}}"#;
        let before: Targets = serde_json::from_str(doc).expect("parses");

        let flag_off: Targets = serde_json::from_str(
            r#"{"window":{"title":"x"},"buttons":{
                "E":{"rect":[1,2,30,40],"confirm":false},
                "S":{"rect":[5,6,7,8],"confirm":true},
                "P":{"rect":[9,9,9,9]}}}"#,
        )
        .expect("parses");
        assert_eq!(confirm_flags_lost(&before, &flag_off), vec!["E".to_string()]);

        let button_gone: Targets = serde_json::from_str(
            r#"{"window":{"title":"x"},"buttons":{
                "S":{"rect":[5,6,7,8],"confirm":true},
                "P":{"rect":[9,9,9,9]}}}"#,
        )
        .expect("parses");
        assert_eq!(confirm_flags_lost(&before, &button_gone), vec!["E".to_string()]);

        // Adding one, moving one, and dropping an unprotected button are all free.
        let harmless: Targets = serde_json::from_str(
            r#"{"window":{"title":"x"},"buttons":{
                "E":{"rect":[0,0,50,50],"confirm":true},
                "S":{"rect":[5,6,7,8],"confirm":true},
                "N":{"rect":[1,1,1,1],"confirm":true}}}"#,
        )
        .expect("parses");
        assert!(confirm_flags_lost(&before, &harmless).is_empty(), "no flag was lost");
    }

    /// Merge patch, the three rules that matter: a null removes, an object merges into what is
    /// already there rather than replacing it, and anything else replaces outright. The middle
    /// one is the whole point — patching one button must not disturb the other 139.
    #[test]
    fn a_patch_touches_only_what_it_names() {
        let base = json!({
            "description": "d",
            "buttons": {
                "A": {"rect": [1, 2, 3, 4], "confirm": true},
                "B": {"rect": [5, 6, 7, 8]}
            }
        });

        let mut doc = base.clone();
        merge_patch(&mut doc, &json!({"buttons": {"C": {"rect": [9, 9, 9, 9]}, "B": Value::Null}}));
        assert_eq!(doc["buttons"]["A"], base["buttons"]["A"], "A is untouched");
        assert!(doc["buttons"].get("B").is_none(), "null removed B");
        assert_eq!(doc["buttons"]["C"]["rect"][0], 9, "C was added");
        assert_eq!(doc["description"], "d", "an unnamed sibling is left alone");

        // Merging INTO a button keeps its other fields. Replacing would silently drop the
        // confirm flag, which is the one field that must never go missing by accident.
        let mut doc = base.clone();
        merge_patch(&mut doc, &json!({"buttons": {"A": {"rect": [0, 0, 1, 1]}}}));
        assert_eq!(doc["buttons"]["A"]["rect"][2], 1, "rect was replaced");
        assert_eq!(doc["buttons"]["A"]["confirm"], true, "confirm survived the patch");

        // A scalar replaces, and an explicit null on a leaf removes the key entirely.
        let mut doc = base.clone();
        merge_patch(&mut doc, &json!({"description": "new"}));
        assert_eq!(doc["description"], "new");
        let mut doc = base.clone();
        merge_patch(&mut doc, &json!({"description": Value::Null}));
        assert!(doc.get("description").is_none());
    }

    /// A field name this struct does not know must be refused, not dropped. Serde's default is
    /// to ignore it, and then `{"keys": "ctrl+alt+f1"}` parses into a request carrying no key at
    /// all — a caller who mistyped gets "one of 'key', 'chord' or 'text' is required" and no
    /// idea which part was wrong. `deny_unknown_fields` turns that into an answer.
    #[test]
    fn a_misspelled_field_is_refused_rather_than_ignored() {
        let r: KeyReq = serde_json::from_str(r#"{"chord":"ctrl+alt+f1"}"#).expect("chord body");
        assert_eq!(r.chord.as_deref(), Some("ctrl+alt+f1"));
        assert!(r.key.is_none() && r.text.is_none());

        // KeyReq has no Debug, so unwrap the error by hand rather than derive one for a test.
        let Err(e) = serde_json::from_str::<KeyReq>(r#"{"keys":"ctrl+alt+f1"}"#) else {
            panic!("an unknown field must be refused, not dropped");
        };
        let msg = e.to_string();
        assert!(msg.contains("keys"), "names what was wrong: {msg}");
        assert!(msg.contains("chord"), "names what to use instead: {msg}");
    }

    /// A sequence and a single press are mutually exclusive, and the body shape parses.
    #[test]
    fn a_click_request_takes_either_one_press_or_a_sequence() {
        let r: ClickReq = serde_json::from_str(
            r#"{"buttons":["MDI_G","MDI_9"],"gap_ms":150,"capture":"hmi","pad":25}"#,
        )
        .expect("sequence body");
        assert_eq!(r.buttons.as_deref(), Some(&["MDI_G".to_string(), "MDI_9".to_string()][..]));
        assert_eq!(r.gap_ms, Some(150));
        assert_eq!(r.pad, Some(25));
        assert!(r.button.is_none());

        // The .png variant has no body, so a sequence has to survive a query string too.
        let mut q = HashMap::new();
        q.insert("buttons".to_string(), "MDI_G, MDI_9 ,MDI_1".to_string());
        let r = ClickReq::from_query(&q).expect("sequence query");
        assert_eq!(r.buttons.expect("parsed").len(), 3, "whitespace around names is trimmed");

        // Empty entries are dropped rather than becoming a press of "".
        let mut q = HashMap::new();
        q.insert("buttons".to_string(), "A,,B,".to_string());
        assert_eq!(ClickReq::from_query(&q).unwrap().buttons.unwrap(), vec!["A", "B"]);
    }

    /// The gap default is deliberately slow. A panel that drops input when pressed too fast
    /// fails silently — a half-typed block and no error — which is the thing this endpoint
    /// exists to prevent.
    #[test]
    fn the_default_gap_is_unhurried() {
        assert_eq!(DEFAULT_GAP_MS, 500);
    }

    /// Capture file names are built from the region name, and `button:NAME` has a colon in it.
    #[test]
    fn capture_labels_never_carry_a_colon() {
        assert_eq!(safe_label("button:CYCLE_START"), "button_CYCLE_START");
        assert_eq!(safe_label("status_bar"), "status_bar");
        assert_eq!(safe_label("a/b\\c"), "a_b_c");
    }

    #[test]
    fn buttons_query_accepts_flags_and_modes() {
        let q = |v: &str| {
            let mut m = HashMap::new();
            m.insert("buttons".to_string(), v.to_string());
            q_buttons(&m).map(|o| o.and_then(ButtonsOpt::mode))
        };
        // what 1 means does not change — older calls have to keep working
        assert_eq!(q("1").unwrap(), Some(ButtonMode::Full));
        assert_eq!(q("true").unwrap(), Some(ButtonMode::Full));
        assert_eq!(q("0").unwrap(), None);
        assert_eq!(q("box").unwrap(), Some(ButtonMode::Box));
        assert_eq!(q("NUM").unwrap(), Some(ButtonMode::Num));
        assert!(q("boks").is_err());
    }

    /// The JSON body has to take a boolean and a mode name alike.
    #[test]
    fn buttons_body_accepts_flags_and_modes() {
        let m = |j: &str| {
            serde_json::from_str::<ButtonsOpt>(j).map(ButtonsOpt::mode).expect("parse")
        };
        assert_eq!(m("true"), Some(ButtonMode::Full));
        assert_eq!(m("false"), None);
        assert_eq!(m("\"box\""), Some(ButtonMode::Box));
        assert_eq!(m("\"num\""), Some(ButtonMode::Num));
    }
}
