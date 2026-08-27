//! The profile document — **this file is the ceiling on what this tool can do.**
//!
//! A request sends only a name (`{"button": "cycle_start"}`). The only coordinates used are
//! the ones written here. Letting a request specify raw pixels means quietly pressing the
//! wrong thing, and with no error it takes a long time to notice. So the side that
//! computes coordinates (the AI, the client) and the side that decides them (this file) are
//! kept apart.
//!
//! Put the other way round: **whoever can edit this file holds the control authority.**

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::win::window::WindowSpec;

/// `[x, y, w, h]` — physical pixels in the client area.
pub type Rect = [i32; 4];

/// What to do when the window size differs from the one the coordinates were measured at.
#[derive(Deserialize, Serialize, Clone, Copy, PartialEq, Eq, Debug, Default)]
#[serde(rename_all = "lowercase")]
pub enum SizeMismatch {
    /// Refuse (the default). Not pressing beats pressing with shifted coordinates.
    #[default]
    Reject,
    /// Scale proportionally. Only safe on a UI whose panel stretches with the window.
    Scale,
    /// Carry on unchanged. For a UI where the window grows but the panel stays pinned to
    /// the top-left.
    Ignore,
}

#[derive(Deserialize, Serialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct ButtonDef {
    /// The rectangle to press. The click point defaults to its centre.
    pub rect: Rect,
    /// A point to press instead of the centre (client coordinates), for asymmetric
    /// switches and the like.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub point: Option<[i32; 2]>,
    /// Which mouse button to press with (`left` | `right` | `middle`).
    ///
    /// It is `click_button` because in this file "button" means the **thing being pressed**;
    /// calling the mouse button `button` too would make one word mean two things inside a
    /// single object.
    #[serde(default = "default_click_button")]
    pub click_button: String,
    #[serde(default)]
    pub double: bool,
    /// At `true`, refuse unless the request also carries `"confirm": true`.
    /// Put it on anything that needs a person at the machine to undo, like an emergency stop.
    #[serde(default)]
    pub confirm: bool,
    /// A settle time for this button alone (ms). Absent, the config default applies.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub settle_ms: Option<u64>,
    /// How long to hold this key down (ms). Absent, the default applies.
    ///
    /// A key that some ladder or poll reads on a cycle has to stay closed long enough to be
    /// scanned at least once. Where a panel needs longer than the default, the number belongs
    /// here — measured once, on that key, rather than remembered by every caller.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hold_ms: Option<u64>,
    /// A note for people, carried verbatim into the API responses. The AI calling this has to
    /// know what it is pressing, so do not leave it blank.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub note: String,
}

fn default_click_button() -> String {
    "left".to_string()
}

#[derive(Deserialize, Serialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct Targets {
    /// **What this profile is**, in plain language. Carried verbatim into the API responses.
    ///
    /// The profile name is a slug (`ncguide`) and the window title is technical
    /// (`FANUC NCGuide`), so neither matches what a person calls it. Write something like
    /// "the FANUC simulator that mirrors the real machine's screen" and an agent has something
    /// to connect a person's words to.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
    /// The window to drive. If you do not know the title, find it with `GET /windows` first.
    pub window: WindowSpec,
    /// **The client size the coordinates below were measured at.** A change of window size
    /// shifts every one of them at once, and this is the reference that detects it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reference_client: Option<[i32; 2]>,
    #[serde(default)]
    pub on_size_mismatch: SizeMismatch,
    /// Named capture regions. Capturing the whole screen every time is expensive, and what is
    /// usually needed is one line of the status bar.
    #[serde(default, deserialize_with = "crate::config::no_duplicate_keys")]
    pub regions: BTreeMap<String, Rect>,
    /// Everything that can be pressed. **A coordinate that is not here cannot be pressed.**
    #[serde(default, deserialize_with = "crate::config::no_duplicate_keys")]
    pub buttons: BTreeMap<String, ButtonDef>,
    /// Named key input. On an application that maps panel keys to the PC keyboard, a key is
    /// often more reliable than a click. Values look like `"f5"` or `"ctrl+alt+r"`.
    #[serde(default, deserialize_with = "crate::config::no_duplicate_keys")]
    pub keys: BTreeMap<String, String>,
}

impl Targets {
    pub fn load(path: &Path) -> Result<Targets, String> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| format!("failed to read {}: {e}", path.display()))?;
        let t: Targets = serde_json::from_str(&text)
            .map_err(|e| format!("failed to parse {}: {e}", path.display()))?;
        t.validate()?;
        Ok(t)
    }

    /// Whether a parsed definition is actually usable. **Loading and saving run the same
    /// function** — a looser check on the editor's path would make that path the way around it.
    pub(crate) fn validate(&self) -> Result<(), String> {
        for (kind, names) in [
            ("button", self.buttons.keys().collect::<Vec<_>>()),
            ("region", self.regions.keys().collect::<Vec<_>>()),
            ("key", self.keys.keys().collect::<Vec<_>>()),
        ] {
            for n in names {
                check_name(kind, n)?;
            }
        }
        if self.window.title.trim().is_empty() && self.window.class.trim().is_empty() {
            return Err("window.title (or window.class) must be set — \
                        call GET /windows to discover the target window"
                .into());
        }
        for (name, t) in &self.buttons {
            let [_, _, w, h] = t.rect;
            if w <= 0 || h <= 0 {
                return Err(format!("button '{name}': rect must have positive width/height"));
            }
            crate::win::input::Button::parse(&t.click_button)
                .map_err(|e| format!("button '{name}': {e}"))?;
        }
        for (name, spec) in &self.keys {
            crate::win::input::parse_chord(spec)
                .map_err(|e| format!("key '{name}': {e}"))?;
        }
        for (name, r) in &self.regions {
            if r[2] <= 0 || r[3] <= 0 {
                return Err(format!("region '{name}': must have positive width/height"));
            }
            // `client` (the whole client area) is reserved. A region with that name becomes a
            // ghost that can never be selected — left alone it turns into "I definitely made
            // it and it never appears", so it is blocked where the name is made.
            if name == "client" {
                return Err(format!(
                    "region name '{name}' is reserved ('client' means the whole client area). Pick another name."
                ));
            }
        }
        Ok(())
    }

    /// Write a validated definition to disk.
    ///
    /// The existing file is kept as `.bak` first, then the new content goes to a temporary file
    /// and is swapped in. This file is the tool's **permission boundary**, so dying mid-write
    /// and leaving half-written JSON would block the next startup entirely. There is always one
    /// copy to go back to.
    pub fn save(&self, path: &Path) -> Result<(), String> {
        self.validate()?;
        // On a new profile, profiles/ may not exist yet.
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("cannot create {}: {e}", parent.display()))?;
        }
        let text = serde_json::to_string_pretty(self)
            .map_err(|e| format!("failed to serialize targets: {e}"))?;
        if path.exists() {
            let bak = path.with_extension("json.bak");
            std::fs::copy(path, &bak)
                .map_err(|e| format!("failed to back up {} to {}: {e}", path.display(), bak.display()))?;
        }
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, text.as_bytes())
            .map_err(|e| format!("failed to write {}: {e}", tmp.display()))?;
        std::fs::rename(&tmp, path)
            .map_err(|e| format!("failed to replace {}: {e}", path.display()))?;
        Ok(())
    }

    /// Look up a region by name. `"client"` is built in and means the whole client area.
    pub fn region(&self, name: &str, client: (i32, i32)) -> Result<Rect, String> {
        if name.is_empty() || name == "client" {
            return Ok([0, 0, client.0, client.1]);
        }
        // `button:NAME` captures a saved button's own rectangle.
        //
        // A toggle's state usually is not inside its button — on this kind of operator panel
        // the lamp sits just above the key. Without this, every caller reads the rect from
        // GET /buttons, adds a margin by hand, and sends the result back as `rect=`. That is
        // arithmetic done in three places against a number the server already holds, and
        // arithmetic is where things go quietly wrong. Pair it with `pad` for the margin.
        if let Some(b) = name.strip_prefix("button:") {
            return self
                .buttons
                .get(b)
                .map(|t| t.rect)
                .ok_or_else(|| format!("unknown button '{b}' in capture region 'button:{b}' (known: {})",
                                       self.button_names()));
        }
        self.regions
            .get(name)
            .copied()
            .ok_or_else(|| format!(
                "unknown capture region '{name}' (known: client, {}; or button:NAME for a button's own rect)",
                self.region_names()
            ))
    }

    /// Grow a rectangle by `pad` on every side. Coordinates stay in the profile's own space,
    /// so `pad` is in the same units as the numbers in the file — which is what a caller has
    /// in hand after reading GET /buttons. Anything that runs off the window is clamped when
    /// the capture is cropped, so a pad larger than the margin available is not an error.
    pub fn pad_rect(rect: Rect, pad: i32) -> Rect {
        if pad == 0 {
            return rect;
        }
        [rect[0] - pad, rect[1] - pad, rect[2] + pad * 2, rect[3] + pad * 2]
    }

    pub fn region_names(&self) -> String {
        self.regions.keys().cloned().collect::<Vec<_>>().join(", ")
    }

    pub fn button_names(&self) -> String {
        self.buttons.keys().cloned().collect::<Vec<_>>().join(", ")
    }

    /// Judge whether the current client size matches the one the coordinates were measured at,
    /// and give the factor to multiply them by.
    ///
    /// - no reference recorded → nothing to check → factor 1
    /// - sizes equal → factor 1
    /// - different → per policy: `reject` errors, `scale` gives the ratio, `ignore` gives 1
    pub fn coordinate_scale(&self, client: (i32, i32)) -> Result<(f64, f64), String> {
        let Some([rw, rh]) = self.reference_client else {
            return Ok((1.0, 1.0));
        };
        if (rw, rh) == client {
            return Ok((1.0, 1.0));
        }
        match self.on_size_mismatch {
            SizeMismatch::Reject => Err(format!(
                "client area is {}x{} but the targets were measured at {rw}x{rh}. Every coordinate would be off. Two ways out, and which one is right depends on which number is stale: if the window drifted, POST /window/fit to put it back to {rw}x{rh}; if the targets were drawn against the window as it is now, update reference_client to {}x{} instead (the editor has a button for that). Or set on_size_mismatch to scale/ignore if the mismatch is expected.",
                client.0, client.1, client.0, client.1
            )),
            SizeMismatch::Scale => Ok((
                client.0 as f64 / rw.max(1) as f64,
                client.1 as f64 / rh.max(1) as f64,
            )),
            SizeMismatch::Ignore => Ok((1.0, 1.0)),
        }
    }

    /// Where a button gets pressed (client coordinates), at the factor `coordinate_scale` gave.
    pub fn click_point(t: &ButtonDef, scale: (f64, f64)) -> (i32, i32) {
        let (x, y) = match t.point {
            Some([px, py]) => (px as f64, py as f64),
            None => {
                let [rx, ry, rw, rh] = t.rect;
                (rx as f64 + rw as f64 / 2.0, ry as f64 + rh as f64 / 2.0)
            }
        };
        ((x * scale.0).round() as i32, (y * scale.1).round() as i32)
    }

    /// Apply the factor to a rectangle.
    pub fn scale_rect(r: Rect, scale: (f64, f64)) -> Rect {
        [
            (r[0] as f64 * scale.0).round() as i32,
            (r[1] as f64 * scale.1).round() as i32,
            (r[2] as f64 * scale.0).round() as i32,
            (r[3] as f64 * scale.1).round() as i32,
        ]
    }

}


/// Move a profile's files to a new name. The save backup follows — left behind it is an orphan
/// with no way to tell which profile it belonged to.
pub fn move_profile_files(from: &Path, to: &Path) -> Result<(), String> {
    if let Some(parent) = to.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("cannot create {}: {e}", parent.display()))?;
    }
    std::fs::rename(from, to)
        .map_err(|e| format!("failed to move {} to {}: {e}", from.display(), to.display()))?;
    let bak = from.with_extension("json.bak");
    if bak.exists() {
        let _ = std::fs::rename(&bak, to.with_extension("json.bak"));
    }
    Ok(())
}

/// A name **travels in the query string** as `?button=NAME`, so characters that get quietly
/// cut there are blocked at save time. In a client that forgot to encode, a name containing
/// `&` or `=` is not an error — it becomes **a different name**, and presses the wrong button
/// or 404s.
///
/// Why not a whitelist (alphanumerics plus `_-.`): people name things in their own language,
/// and a name in their own script is natural. Non-Latin letters are not dangerous — forget to
/// encode those and **the URL itself breaks, loudly**, rather than quietly becoming something
/// else. So what is blocked is exactly "characters that silently mean something else", and the
/// list below is that reason, one entry at a time.
fn check_name(kind: &str, name: &str) -> Result<(), String> {
    if name.trim().is_empty() {
        return Err(format!("a {kind} name must not be empty or whitespace"));
    }
    if name.chars().count() > 64 {
        return Err(format!("{kind} name '{name}' is longer than 64 characters"));
    }
    // What each character becomes in a query string is the reason for blocking it.
    for c in name.chars() {
        let why = match c {
            '&' => "separates query parameters — the name would be cut here",
            '=' => "separates name from value",
            '#' => "starts the fragment — everything after it never reaches the server",
            '?' => "starts the query string",
            '%' => "starts a percent-escape",
            '+' => "decodes to a space in a query string",
            '/' | '\\' => "a path separator; it also ends up in capture file names",
            c if c.is_whitespace() => "whitespace is encoded inconsistently by clients",
            c if c.is_control() => "a control character",
            _ => continue,
        };
        return Err(format!(
            "{kind} name '{name}' contains '{c}': {why}. Names travel in ?{kind}=NAME, so this \
             one would fail silently rather than loudly. Letters, digits, '_', '-' and '.' are \
             always safe; non-ASCII letters are fine too."
        ));
    }
    Ok(())
}



/// Move a deleted profile aside under **a name nothing overwrites**.
///
/// The `.bak` a save leaves is one generation deep and the next save overwrites it. Use that
/// for deletion and this happens: delete a profile → recreate it under the same name → save
/// once, and **the backup of the first one is gone.** Even if it was a 128-button definition.
/// So a delete archive carries the time in its name and they never overwrite each other.
///
/// If that name already exists (deleted twice within one second) `_2`, `_3` are appended —
/// Windows' `rename` overwrites an existing file silently.
pub fn archive(path: &Path) -> Result<PathBuf, String> {
    let now = crate::logging::local_now();
    let stamp = format!(
        "{:04}-{:02}-{:02}_{:02}{:02}{:02}",
        now.year(), now.month() as u8, now.day(), now.hour(), now.minute(), now.second()
    );
    let base = path
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| format!("{} has no file name", path.display()))?
        .to_string();

    let mut dest = path.with_file_name(format!("{base}.deleted-{stamp}"));
    let mut n = 2;
    while dest.exists() {
        dest = path.with_file_name(format!("{base}.deleted-{stamp}_{n}"));
        n += 1;
    }
    std::fs::rename(path, &dest)
        .map_err(|e| format!("failed to move {} to {}: {e}", path.display(), dest.display()))?;

    // The save backup is moved aside with it. Left in place, the moment a new profile takes
    // that name the next save overwrites it — a file that looks like a backup and is not.
    let bak = path.with_extension("json.bak");
    if bak.exists() {
        let _ = std::fs::rename(&bak, path.with_file_name(format!("{base}.bak.deleted-{stamp}")));
    }
    Ok(dest)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fixture for exercising the coordinate maths, built here rather than borrowed from
    /// anywhere, so changing a default elsewhere cannot quietly change what these tests mean.
    fn sample() -> Targets {
        let mut t = Targets {
            description: String::new(),
            window: WindowSpec { title: "Sample".into(), title_exact: false, class: String::new() },
            reference_client: Some([1280, 1000]),
            on_size_mismatch: SizeMismatch::Reject,
            regions: BTreeMap::new(),
            buttons: BTreeMap::new(),
            keys: BTreeMap::new(),
        };
        t.buttons.insert(
            "cycle_start".into(),
            ButtonDef {
                rect: [820, 640, 60, 40],
                point: None,
                click_button: default_click_button(),
                double: false,
                confirm: false,
                hold_ms: None,
                settle_ms: None,
                note: String::new(),
            },
        );
        // One entry carrying **every** non-default field, so a field that quietly drops out of
        // a round trip is caught here.
        t.buttons.insert(
            "estop".into(),
            ButtonDef {
                rect: [10, 10, 40, 40],
                point: Some([30, 30]),
                click_button: "right".into(),
                double: true,
                confirm: true,
                hold_ms: Some(250),
                settle_ms: Some(1500),
                note: "undoing this needs a person at the machine".into(),
            },
        );
        t.regions.insert("status_bar".into(), [0, 940, 1280, 60]);
        t.keys.insert("reset".into(), "escape".into());
        t
    }


    /// Every optional thing in a profile must be clearable by a merge patch, which can only
    /// ever REMOVE a key — it has no way to write a literal null. That works here only because
    /// nothing is stored as null in the first place: an unset option is an absent key. Add a
    /// field that is `Option<T>` without `#[serde(default)]`, or one that serialises its empty
    /// state as `null`, and `"field": null` in a patch stops meaning "clear it" — silently for
    /// the second case, since the document would still round-trip.
    #[test]
    fn removing_a_key_is_how_a_patch_clears_a_value() {
        let full = r#"{"window":{"title":"x"},"reference_client":[1280,1000],"on_size_mismatch":"scale",
                       "buttons":{"A":{"rect":[1,2,30,40],"point":[5,6],"settle_ms":900,"confirm":true}}}"#;
        let t: Targets = serde_json::from_str(full).expect("parses");
        assert!(t.reference_client.is_some() && t.buttons["A"].point.is_some());

        // What a merge patch leaves behind after `null` on each of them: the key, gone.
        let cleared = r#"{"window":{"title":"x"},"buttons":{"A":{"rect":[1,2,30,40],"confirm":true}}}"#;
        let t: Targets = serde_json::from_str(cleared).expect("parses without the optional keys");
        assert!(t.reference_client.is_none(), "reference_client cleared");
        assert!(t.buttons["A"].point.is_none(), "point cleared");
        assert!(t.buttons["A"].settle_ms.is_none(), "settle_ms cleared");
        assert_eq!(t.on_size_mismatch, SizeMismatch::Reject, "back to the default");
        assert!(t.buttons["A"].confirm, "and the flag that was not named survived");

        // The other half of the claim: none of those write a null back out, so a cleared
        // document and a never-set one are the same bytes.
        let text = serde_json::to_string(&t).expect("serialises");
        assert!(!text.contains("null"), "an unset option must be absent, not null: {text}");
    }

    /// A field name this build does not know must stop the document, not be dropped from it.
    /// `POST /admin/profile` replaces the whole file, so a dropped field is a *saved* loss —
    /// and the field most worth mistyping is `confirm`. Typing `confrim` on the emergency stop
    /// would otherwise store that button with no confirmation required and answer "saved".
    #[test]
    fn a_misspelled_field_stops_the_document() {
        let good = r#"{"window":{"title":"x"},"buttons":{"E":{"rect":[0,0,9,9],"confirm":true}}}"#;
        let t: Targets = serde_json::from_str(good).expect("the spelled version parses");
        assert!(t.buttons["E"].confirm);

        for (doc, bad) in [
            (r#"{"window":{"title":"x"},"buttons":{"E":{"rect":[0,0,9,9],"confrim":true}}}"#, "confrim"),
            (r#"{"window":{"title":"x","title_exakt":true}}"#, "title_exakt"),
            (r#"{"window":{"title":"x"},"button":{}}"#, "button"),
        ] {
            let Err(e) = serde_json::from_str::<Targets>(doc) else {
                panic!("'{bad}' must be refused, not dropped");
            };
            assert!(e.to_string().contains(bad), "the error names it: {e}");
        }
    }

    /// Whether `profile.example.json` really parses into a `Targets` and passes validation.
    /// Ship a broken example and the first user copies it verbatim and gets stuck.
    #[test]
    fn profile_example_parses() {
        let t: Targets =
            serde_json::from_str(include_str!("../profile.example.json")).expect("parse example");
        t.validate().expect("example must be valid");
        // The emergency-stop example must carry confirm — people copy this example verbatim,
        // so the shape they start from has to be the safe one
        assert!(t.buttons["emergency_stop"].confirm);
    }

    #[test]
    fn size_mismatch_rejects_by_default() {
        let t = sample();
        assert_eq!(t.coordinate_scale((1280, 1000)).expect("same size"), (1.0, 1.0));
        let err = t.coordinate_scale((1024, 800)).expect_err("must reject");
        // both sizes have to appear for an operator to know what to restore
        assert!(err.contains("1024x800"), "{err}");
        assert!(err.contains("1280x1000"), "{err}");
    }

    #[test]
    fn scale_policy_scales_coordinates() {
        let mut t = sample();
        t.on_size_mismatch = SizeMismatch::Scale;
        let s = t.coordinate_scale((2560, 2000)).expect("scale");
        assert_eq!(s, (2.0, 2.0));
        let target = &t.buttons["cycle_start"];
        // the centre of rect [820,640,60,40] is (850, 660), doubled
        assert_eq!(Targets::click_point(target, s), (1700, 1320));
    }

    /// Two identical names have to die **at parse time**. Left alone, the later replaces the
    /// earlier and one is already gone by the time the server sees the document: you send 128,
    /// 127 are saved, and the response says success. This collision really happens — the MDI
    /// letter keys X/Y/Z and the panel's axis-select X/Y/Z.
    #[test]
    fn duplicate_names_are_refused_instead_of_silently_replacing() {
        let doc = r#"{
            "window": {"title": "x"},
            "buttons": {
                "AXIS_X": {"rect": [0, 0, 10, 10]},
                "AXIS_X": {"rect": [50, 50, 10, 10]}
            }
        }"#;
        let e = serde_json::from_str::<Targets>(doc).err().expect("must be refused").to_string();
        assert!(e.contains("duplicate name 'AXIS_X'"), "{e}");

        // regions and keys need the same rule — blocking one place leaks through the rest
        for field in ["regions", "keys"] {
            let v = if field == "regions" { "[0,0,4,4]" } else { "\"f1\"" };
            let doc = format!(
                r#"{{"window": {{"title": "x"}}, "{field}": {{"DUP": {v}, "DUP": {v}}}}}"#
            );
            let e = serde_json::from_str::<Targets>(&doc).err().expect("must be refused").to_string();
            assert!(e.contains("duplicate name 'DUP'"), "{field}: {e}");
        }

        // different names pass, of course
        let ok = r#"{"window": {"title": "x"},
                     "buttons": {"A": {"rect": [0,0,10,10]}, "B": {"rect": [1,1,10,10]}}}"#;
        assert_eq!(serde_json::from_str::<Targets>(ok).expect("ok").buttons.len(), 2);
    }

    /// A name travels in the query string as `?button=NAME`. Only the characters that
    /// **silently** become something else there are blocked — characters that merely need
    /// encoding, such as non-Latin letters, are not.
    #[test]
    fn names_that_would_be_cut_in_a_query_string_are_refused() {
        let with_name = |n: &str| {
            let mut t = sample();
            t.buttons.clear();
            t.buttons.insert(n.to_string(), ButtonDef {
                rect: [0, 0, 10, 10],
                point: None,
                click_button: "left".into(),
                double: false,
                confirm: false,
                hold_ms: None,
                settle_ms: None,
                note: String::new(),
            });
            t.validate()
        };

        for bad in ["a&b", "a=b", "a#b", "a?b", "a%b", "a+b", "a/b", "a b", "  ", ""] {
            assert!(with_name(bad).is_err(), "'{bad}' should be refused");
        }
        // The safe ones, non-Latin letters included: forget to encode those and the URL itself
        // breaks **loudly**, rather than quietly becoming a different name.
        for good in ["CYCLE_START", "mdi.x", "axis-z", "ctrl_0001", "\u{c8fc}\u{cd95}_\u{ae30}\u{b3d9}"] {
            with_name(good).unwrap_or_else(|e| panic!("'{good}' should pass: {e}"));
        }
        // the length ceiling
        assert!(with_name(&"a".repeat(65)).is_err());
    }

    /// Serialising to the saved shape and reading it back has to give **the same document**.
    /// The `GET /admin/profile` → edit → `POST /admin/profile` round trip rests on this.
    #[test]
    fn a_profile_survives_a_serialize_parse_round_trip() {
        let before = sample();
        let text = serde_json::to_string(&before).expect("serialize");
        let after = serde_json::from_str::<Targets>(&text).expect("parse back");

        assert_eq!(after.buttons.len(), before.buttons.len());
        assert_eq!(after.regions.len(), before.regions.len());
        assert_eq!(after.keys.len(), before.keys.len());
        assert_eq!(after.reference_client, before.reference_client);
        assert_eq!(after.window.title, before.window.title);
        // whether non-default fields survive the round trip — this is where they vanish
        let b = after.buttons.get("estop").expect("estop survived");
        assert!(b.confirm, "confirm");
        assert!(b.double, "double");
        assert_eq!(b.point, Some([30, 30]), "point");
        assert_eq!(b.settle_ms, Some(1500), "settle_ms");
        assert_eq!(b.click_button, "right", "click_button");
        assert_eq!(b.note, before.buttons["estop"].note, "note");
    }

    /// Deleting the same name twice has to **leave the first archive alive.**
    ///
    /// The `.bak` a save leaves is one generation deep and the next save overwrites it. Use
    /// that scheme for deletion and "delete → recreate under the same name → delete again"
    /// loses the first one — which might have been a 128-button definition. So the time goes
    /// into the name, and a number is appended within the same second.
    #[test]
    fn deleting_the_same_name_twice_keeps_both_copies() {
        let dir = std::env::temp_dir().join(format!("deescreen-archive-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join("deescreen.fanuc.json");

        std::fs::write(&path, b"first").expect("write");
        let a = archive(&path).expect("first archive");

        // recreate under the same name, leave a save backup too, then delete again
        std::fs::write(&path, b"second").expect("write");
        std::fs::write(path.with_extension("json.bak"), b"second-previous").expect("write");
        let b = archive(&path).expect("second archive");

        assert_ne!(a, b, "same archive name would mean one overwrote the other");
        assert_eq!(std::fs::read(&a).expect("first survives"), b"first");
        assert_eq!(std::fs::read(&b).expect("second survives"), b"second");
        // The save backup has to come along — left behind, the next save overwrites it and it
        // becomes a file that looks like a backup and is not.
        assert!(!path.with_extension("json.bak").exists(), "an orphaned .bak was left behind");
        assert!(!path.exists(), "the original was left behind");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A rename brings the save backup with it — left behind, there is no telling which
    /// profile it belonged to.
    #[test]
    fn renaming_takes_the_backup_along() {
        let dir = std::env::temp_dir().join(format!("deescreen-rename-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp dir");
        let (from, to) = (dir.join("deescreen.old.json"), dir.join("deescreen.new.json"));

        std::fs::write(&from, b"doc").expect("write");
        std::fs::write(from.with_extension("json.bak"), b"previous").expect("write");
        move_profile_files(&from, &to).expect("rename");

        assert!(!from.exists() && to.exists());
        assert_eq!(std::fs::read(to.with_extension("json.bak")).expect("bak moved"), b"previous");
        assert!(!from.with_extension("json.bak").exists());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn click_point_defaults_to_rect_center() {
        let t = sample();
        assert_eq!(Targets::click_point(&t.buttons["cycle_start"], (1.0, 1.0)), (850, 660));
    }

    #[test]
    fn bad_key_spec_is_rejected_at_load() {
        let mut t = sample();
        t.keys.insert("bogus".into(), "ctrl+nosuchkey".into());
        assert!(t.validate().is_err());
    }

    /// A regression guard — a recovery must not be blocked by the state it recovers from.
    /// `/window/focus` and `/window/fit` are what **fix** being minimised and being the wrong
    /// size, so they have to bypass those checks. This once produced a loop where focus replied
    /// "the window is minimized — call /window/focus" (2026-08-12).
    #[test]
    fn recovery_endpoints_must_not_be_blocked_by_what_they_fix() {
        let src = include_str!("api.rs");
        let focus = src.split("pub async fn window_focus").nth(1).expect("window_focus");
        let focus_body = focus.split("pub async fn").next().expect("body");
        assert!(
            !focus_body.contains("find_window(&t)") && !focus_body.contains("bind_window(&t)"),
            "window_focus must not use the strict lookup — it rejects minimized windows"
        );
        let fit = src.split("pub async fn window_fit").nth(1).expect("window_fit");
        let fit_body = fit.split("pub async fn").next().expect("body");
        assert!(
            !fit_body.contains("find_window(&t)") && !fit_body.contains("bind_window(&t)"),
            "window_fit must not use the strict lookup — it rejects mismatched sizes"
        );
    }

    /// A reserved word as a region name makes that region unselectable forever — blocked
    /// where the name is made.
    #[test]
    fn reserved_region_names_are_rejected() {
        let mut t = sample();
        t.regions.insert("client".to_string(), [0, 0, 10, 10]);
        let err = t.validate().expect_err("reserved name must be rejected");
        assert!(err.contains("reserved"), "{err}");
    }

    /// `button:NAME` captures a button's own rectangle, and `pad` grows it.
    ///
    /// Without these, every caller reads the rect from GET /buttons, adds a margin by hand and
    /// sends it back as `rect=` — arithmetic in the caller against a number the server already
    /// holds, which is exactly where things go quietly wrong.
    #[test]
    fn a_button_can_be_captured_by_name_with_a_margin() {
        let t = sample();
        assert_eq!(t.region("button:cycle_start", (1280, 1000)).expect("by name"), [820, 640, 60, 40]);

        // pad grows every side, so the width and height gain twice the padding.
        assert_eq!(
            Targets::pad_rect(t.region("button:cycle_start", (1280, 1000)).unwrap(), 25),
            [795, 615, 110, 90]
        );
        assert_eq!(Targets::pad_rect([10, 10, 5, 5], 0), [10, 10, 5, 5]);

        // Padding past the window edge is not an error — cropping clamps it.
        assert_eq!(Targets::pad_rect([2, 2, 10, 10], 5), [-3, -3, 20, 20]);

        // An unknown button says so, and says it is a button it could not find.
        let e = t.region("button:nope", (1280, 1000)).unwrap_err();
        assert!(e.contains("unknown button 'nope'"), "{e}");
        // A plain unknown region points at both ways of naming one.
        let e = t.region("nope", (1280, 1000)).unwrap_err();
        assert!(e.contains("button:NAME"), "{e}");
    }

    #[test]
    fn client_region_is_built_in() {
        let t = sample();
        assert_eq!(t.region("client", (800, 600)).expect("client"), [0, 0, 800, 600]);
        assert_eq!(t.region("", (800, 600)).expect("default"), [0, 0, 800, 600]);
        assert!(t.region("nope", (800, 600)).is_err());
    }
}
