//! The profile document — **this file is the ceiling on what this tool can do.**
//!
//! A request sends only a name (`{"button": "cycle_start"}`). The only coordinates used are
//! the ones written here. Letting a request specify raw pixels means quietly pressing the
//! wrong thing, and with no error it takes a long time to notice. So the side that
//! computes coordinates (the AI, the client) and the side that decides them (this file) are
//! kept apart.
//!
//! Put the other way round: **whoever can edit this file holds the control authority.**
//!
//! ## The one thing outside it
//!
//! `POST /menu` presses a menu item, and a menu is read off the window rather than written
//! here — a menu item has no stable rectangle to write down. So that endpoint reaches whatever
//! the application's menu reaches, which is why it is off unless `allow_menus` is set, and why
//! `confirm_menus` below is this file's only say in it. Everything else obeys the paragraph
//! above without exception.

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

/// Move a rectangle by an offset. Free function so `region` can use it before `impl Targets`.
fn shift(r: Rect, by: (i32, i32)) -> Rect {
    [r[0] + by.0, r[1] + by.1, r[2], r[3]]
}

/// How much one anchor box grew, per axis. A zero-width `from` would divide by zero and is a
/// profile that could never have matched anything, so it scales by 1 rather than producing
/// infinities that look like coordinates.
fn refit_factor(from: Rect, to: Rect) -> (f64, f64) {
    (
        if from[2] > 0 { to[2] as f64 / from[2] as f64 } else { 1.0 },
        if from[3] > 0 { to[3] as f64 / from[3] as f64 } else { 1.0 },
    )
}

/// A rectangle to capture, and which anchor its coordinates are measured from.
///
/// Accepts either shape on the way in:
///
/// ```json
/// "status_bar": [0, 940, 1280, 60]                        // before anchors existed
/// "status_bar": {"rect": [0, 940, 1280, 60], "anchor": "@fixed"}
/// ```
///
/// The bare array keeps an existing profile loading. Saving always writes the object, so a
/// profile that has been through one save can no longer be missing the answer.
#[derive(Serialize, Clone, Debug)]
pub struct RegionDef {
    pub rect: Rect,
    /// An anchor name, or `"@fixed"`. Empty only in a profile that declares no anchors.
    #[serde(default)]
    pub anchor: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub note: String,
}

impl<'de> Deserialize<'de> for RegionDef {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Full {
            rect: Rect,
            #[serde(default)]
            anchor: String,
            #[serde(default)]
            note: String,
        }
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Shape {
            Bare(Rect),
            Full(Full),
        }
        // The untagged enum only ever sees two shapes here — an array or an object — and serde
        // reports the object's own field errors when it is one, so the message stays specific.
        Ok(match Shape::deserialize(d)? {
            Shape::Bare(rect) => RegionDef { rect, anchor: String::new(), note: String::new() },
            Shape::Full(f) => RegionDef { rect: f.rect, anchor: f.anchor, note: f.note },
        })
    }
}

/// A control the coordinates around it are measured from.
///
/// Some applications move their whole layout between runs without changing their window size.
/// NC Trainer2 plus shifts every container 16px sideways depending on how it was started; the
/// window is the same size, the controls are the same size, and only the origin differs. No
/// check based on the window can see that, and a 44px key still takes the press while a 32px
/// one hands it to a neighbour — so it mostly works, and occasionally does the wrong thing
/// without saying so.
///
/// An anchor is the fix: find this control now, compare where it is with where it was when the
/// coordinates were written, and shift everything that belongs to it by the difference.
///
/// Matched on **text and size together**, and on neither alone. The class is useless here — an
/// MFC window carries its module's load address in it, so it differs every run
/// (`Afx:00D90000:3:…` then `Afx:00F20000:8:…`). Text alone is not enough either: this panel
/// has two containers called `OPERATION PANEL`. Their sizes differ (328x238 and 708x238), and
/// size is exactly what a translation leaves alone, so the pair identifies one control.
///
/// Deliberately not "the control nearest to where it used to be": that reasoning uses the
/// possibly-stale rectangle to find the thing that would tell you it is stale, and fails
/// exactly when the drift is largest.
#[derive(Deserialize, Serialize, Clone, Debug)]
#[serde(deny_unknown_fields)]
pub struct AnchorDef {
    /// The control's window text, as `GET /controls` reports it.
    pub text: String,
    /// Where that control was when everything else here was measured — `[x, y, w, h]`. The size
    /// identifies it among same-named controls; the origin gives the offset to apply.
    pub rect: Rect,
    /// A note for people, like a button's.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub note: String,
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
    /// Which anchor this button's coordinates are measured from — an anchor name, or
    /// `"@fixed"` for something that does not move with the rest.
    ///
    /// Required once the profile declares any anchor, and empty otherwise. There is no third
    /// state: a button that has not answered cannot be saved, because the answer that gets
    /// silently skipped is the one nobody checks afterwards. `"@fixed"` is a statement someone
    /// made, not an absence.
    #[serde(default)]
    pub anchor: String,
    /// How long to hold this key down (ms). Absent, the default applies.
    ///
    /// A key that some ladder or poll reads on a cycle has to stay closed long enough to be
    /// scanned at least once. Where a panel needs longer than the default, the number belongs
    /// here — measured once, on that key, rather than remembered by every caller.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hold_ms: Option<u64>,
    /// **What this key enters**, as printed on it — `"G"`, `"7"`, `";"` for END OF BLOCK.
    ///
    /// Empty for a key that enters nothing (MONITOR, CYCLE START, a mode selector). Only keys
    /// that put characters into an input line have this.
    ///
    /// More than one character is allowed, for a key that enters several at once. Spelling
    /// tries the longest legend first, so a key legended `"G0"` is used for `G0` and the plain
    /// `G` key for anything else.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub types: String,
    /// **What it enters after the shift key** — the second legend, the small one printed above.
    ///
    /// This is the fact that used to live in an English sentence in `note`, where nothing could
    /// read it and nobody could check it. Setting it requires the profile to declare `shift`,
    /// because which key reaches it and how that key behaves is not derivable from here.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub shift_types: String,
    /// A note for people, carried verbatim into the API responses. The AI calling this has to
    /// know what it is pressing, so do not leave it blank.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub note: String,
}

/// How the keypad's shift key behaves — **declared, never assumed**.
///
/// The two are not interchangeable and guessing wrong types a different string than the one
/// asked for, without erroring: on a one-shot panel a latch model presses shift once for `EE`
/// and gets `Ef`, and on a latching panel a one-shot model leaves it on.
#[derive(Deserialize, Serialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "lowercase")]
pub enum ShiftMode {
    /// Reaches the second legend for **one** key, then falls back on its own.
    Oneshot,
    /// Stays on until pressed again. Spelling turns it off before it finishes, so a sequence
    /// never leaves the panel in a state the next caller did not ask for.
    Toggle,
}

/// The key that reaches the second legend, and how it behaves.
#[derive(Deserialize, Serialize, Clone, Debug)]
#[serde(deny_unknown_fields)]
pub struct ShiftDef {
    /// The name of the button in this profile that acts as shift.
    pub button: String,
    /// Required. There is no default because the wrong one is silently wrong — see [`ShiftMode`].
    pub mode: ShiftMode,
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
    /// **Which application this profile is one version of.**
    ///
    /// A program's profiles are **alternatives**: the same application with different projects
    /// loaded, only one of which is open at a time — NCGuide's 0i lathe, 0i mill and 30i. Naming
    /// the program instead of the profile lets the server pick whichever of them is open, and
    /// only when it can prove which one that is.
    ///
    /// A dialog or a second window of the same application is **not** an alternative: it is
    /// open at the same time as the main one, so putting it in the program makes the program
    /// ambiguous whenever it shows. Give it no program, or one of its own.
    ///
    /// Empty is fine: the profile is then addressed by its own name only.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub program: String,
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
    /// Controls that the coordinates in this profile are measured from, by name.
    ///
    /// Empty is the ordinary case: the application keeps its layout still, every coordinate
    /// means what it says, and nothing here applies. Declaring even one turns the whole
    /// profile strict — every button and region must then say which anchor it belongs to, or
    /// `"@fixed"`. Half-answered is the state that would be quietly wrong, so it is refused.
    #[serde(default, deserialize_with = "crate::config::no_duplicate_keys")]
    pub anchors: BTreeMap<String, AnchorDef>,
    #[serde(default, deserialize_with = "crate::config::no_duplicate_keys")]
    pub regions: BTreeMap<String, RegionDef>,
    /// Everything that can be pressed. **A coordinate that is not here cannot be pressed.**
    #[serde(default, deserialize_with = "crate::config::no_duplicate_keys")]
    pub buttons: BTreeMap<String, ButtonDef>,
    /// Menu paths that need `"confirm": true`, the way a button's `confirm` flag does.
    ///
    /// Matched as a **prefix on whole path segments**, so `"File"` protects the entire File
    /// menu and `"Tool/Set Machine Parameters"` protects one item. A menu is not a whitelist -
    /// it is read off the window, so anything in it can be reached - and this is where a
    /// person says which parts of it need a second look.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub confirm_menus: Vec<String>,
    /// The keypad's shift key, for profiles whose keys carry a second legend.
    ///
    /// Absent is the ordinary case. It is required as soon as any button sets `shift_types`,
    /// because a shifted legend that nothing can reach is a fact recorded and then not usable,
    /// which is the same as not recording it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shift: Option<ShiftDef>,
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
        if !self.program.is_empty() && !crate::config::is_safe_profile_name(&self.program) {
            return Err(format!(
                "program {:?} is not a valid name - letters, digits, '-' and '_' only, because it \
                 travels in ?program= the way a profile name travels in ?profile=",
                self.program
            ));
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
        self.validate_legends()?;
        // Anchors, and who belongs to which.
        //
        // A profile with no anchors is the ordinary case and nothing here applies. One anchor
        // makes the whole profile strict: an element that has not said where its coordinates
        // come from would be left behind when everything else moves, pressing the place its
        // neighbours used to be. Half-corrected is worse than uncorrected, because the half
        // that works hides the half that does not.
        if !self.anchors.is_empty() {
            for (name, a) in &self.anchors {
                check_name("anchor", name)?;
                if a.text.trim().is_empty() {
                    return Err(format!(
                        "anchor '{name}' has no text. An anchor is found by the control's text \
                         and size together; the class cannot be used because an MFC window puts \
                         its module load address in it and that changes every run."
                    ));
                }
                if a.rect[2] <= 0 || a.rect[3] <= 0 {
                    return Err(format!("anchor '{name}': rect must have positive width/height"));
                }
            }

            let known = |who: &str, name: &str, anchor: &str| -> Result<(), String> {
                if anchor.is_empty() {
                    return Err(format!(
                        "{who} '{name}' does not say which anchor it belongs to. This profile \
                         declares anchors ({}), so every button and region must name one or say \
                         \"@fixed\" for something that does not move. There is no third answer: \
                         an element left unanswered is one that silently stays put while the \
                         rest of the panel moves.",
                        self.anchors.keys().cloned().collect::<Vec<_>>().join(", ")
                    ));
                }
                if anchor != "@fixed" && !self.anchors.contains_key(anchor) {
                    return Err(format!(
                        "{who} '{name}' names anchor '{anchor}', which this profile does not \
                         define. Known anchors: {}. Or \"@fixed\" if it does not move.",
                        self.anchors.keys().cloned().collect::<Vec<_>>().join(", ")
                    ));
                }
                Ok(())
            };
            // Report the count before the first name: on a 140-button panel the useful fact is
            // how much is left to do, not which one happened to sort first.
            let missing: Vec<&String> = self
                .buttons
                .iter()
                .filter(|(_, b)| b.anchor.is_empty())
                .map(|(n, _)| n)
                .chain(self.regions.iter().filter(|(_, r)| r.anchor.is_empty()).map(|(n, _)| n))
                .collect();
            if missing.len() > 1 {
                let shown: Vec<&str> = missing.iter().take(5).map(|s| s.as_str()).collect();
                return Err(format!(
                    "{} buttons and regions do not say which anchor they belong to, starting \
                     with: {}. This profile declares anchors ({}), so every one of them must \
                     name an anchor or say \"@fixed\".",
                    missing.len(),
                    shown.join(", "),
                    self.anchors.keys().cloned().collect::<Vec<_>>().join(", ")
                ));
            }
            for (name, b) in &self.buttons {
                known("button", name, &b.anchor)?;
            }
            for (name, r) in &self.regions {
                known("region", name, &r.anchor)?;
            }
        } else {
            // No anchors declared, so an anchor NAME here refers to nothing, and offset_for
            // treats a name it cannot find as no shift at all. The way this happens is a
            // whole-document save that lost the anchors map while every element kept its
            // anchor - and passing it would switch the correction off without anyone having
            // decided to. `@fixed` is still fine: it says "no shift", which is what happens.
            let dangling: Vec<String> = self
                .buttons
                .iter()
                .filter(|(_, b)| !b.anchor.is_empty() && b.anchor != "@fixed")
                .map(|(n, b)| format!("{n} -> {}", b.anchor))
                .chain(
                    self.regions
                        .iter()
                        .filter(|(_, r)| !r.anchor.is_empty() && r.anchor != "@fixed")
                        .map(|(n, r)| format!("{n} -> {}", r.anchor)),
                )
                .collect();
            if !dangling.is_empty() {
                let shown: Vec<&str> = dangling.iter().take(5).map(String::as_str).collect();
                return Err(format!(
                    "{} buttons and regions name an anchor, but this profile declares none \
                     (starting with: {}). Nothing would correct them. If the anchors were \
                     removed on purpose, clear those names as well; if not, the anchors map \
                     was lost on the way here - send it back with the rest of the document.",
                    dangling.len(),
                    shown.join(", ")
                ));
            }
        }

        for (name, r) in &self.regions {
            if r.rect[2] <= 0 || r.rect[3] <= 0 {
                return Err(format!("region '{name}': must have positive width/height"));
            }
            // A region called `client` was the old spelling of the whole client area, and it
            // was reserved with nothing to show for it — an ordinary word that happened to be
            // taken. It is `@client` now, so this name is free; a region using it would only
            // ever be a ghost under the old rule.
            if name == "client" {
                return Err(format!(
                    "region name '{name}' was the old spelling of the whole client area, which is \
                     now '@client'. The bare word is free to use as a name, but a profile written \
                     against the old spelling almost certainly means the whole client area - so \
                     this is refused once, deliberately, rather than silently becoming an \
                     ordinary region. Rename it, or use '@client' where you meant the whole area."
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

    /// Match each declared anchor against the controls on screen and give back how far each
    /// has moved.
    ///
    /// `Err` for an anchor that cannot be resolved. Falling back to the saved coordinates
    /// would be the worst answer available: the profile said these numbers need correcting,
    /// and using them uncorrected is precisely the silent mistake the anchor was added to
    /// prevent. Only the elements belonging to that anchor are affected — an unrelated part of
    /// the panel keeps working.
    pub fn anchor_offsets(
        &self,
        controls: &[crate::win::window::ControlInfo],
    ) -> Result<std::collections::HashMap<String, (i32, i32)>, String> {
        let mut out = std::collections::HashMap::new();
        for (name, a) in &self.anchors {
            let want = (a.rect[2], a.rect[3]);
            let found: Vec<&crate::win::window::ControlInfo> = controls
                .iter()
                .filter(|c| c.text.trim() == a.text.trim() && (c.rect[2], c.rect[3]) == want)
                .collect();
            match found.as_slice() {
                [c] => {
                    out.insert(name.clone(), (c.rect[0] - a.rect[0], c.rect[1] - a.rect[1]));
                }
                [] => {
                    let same_text = controls.iter().filter(|c| c.text.trim() == a.text.trim()).count();
                    return Err(format!(
                        "anchor '{name}' was not found: no control with text {:?} and size {}x{}. \
                         {} control(s) carry that text. Either the application changed, or the \
                         anchor's own rect is stale — it records the size as well as the place, \
                         and the size is what identifies it.",
                        a.text, want.0, want.1, same_text
                    ));
                }
                more => {
                    // Picking one would be a guess, and a wrong guess moves every coordinate
                    // that belongs to this anchor. Refuse and say what would have to change.
                    return Err(format!(
                        "anchor '{name}' is ambiguous: {} controls have text {:?} AND size {}x{}. \
                         Text and size together are supposed to identify one control. Anchor a \
                         different container that is unique, or give this one a size that is.",
                        more.len(), a.text, want.0, want.1
                    ));
                }
            }
        }
        Ok(out)
    }

    /// The offset an element with this anchor value should be shifted by.
    ///
    /// `"@fixed"` and an empty value both mean no shift — the first because somebody said so,
    /// the second because this profile has no anchors at all. Validation is what keeps those
    /// two from being confused; by the time a profile is in use, an empty value can only mean
    /// the second.
    pub fn offset_for(offsets: &std::collections::HashMap<String, (i32, i32)>, anchor: &str) -> (i32, i32) {
        if anchor.is_empty() || anchor == "@fixed" {
            return (0, 0);
        }
        offsets.get(anchor).copied().unwrap_or((0, 0))
    }

    /// The keypad legends: reachable, and each one belonging to exactly one key.
    ///
    /// Both checks exist because their failures are silent. A shifted legend with no shift key
    /// declared is a fact recorded and then unusable; two keys claiming `7` means a string
    /// containing `7` is spelled with whichever key sorted first, which is not a decision
    /// anybody made.
    fn validate_legends(&self) -> Result<(), String> {
        let shifted: Vec<&str> =
            self.buttons.iter().filter(|(_, b)| !b.shift_types.is_empty()).map(|(n, _)| n.as_str()).collect();
        match &self.shift {
            None if !shifted.is_empty() => {
                return Err(format!(
                    "{} button(s) carry a shifted legend ({}) but this profile does not declare \
                     'shift'. Which key reaches the second legend, and whether it is one-shot or \
                     a latch, cannot be worked out from the buttons - and guessing wrong types a \
                     different string with no error. Add \"shift\": {{\"button\": \"NAME\", \
                     \"mode\": \"oneshot\" | \"toggle\"}}.",
                    shifted.len(),
                    shifted.join(", ")
                ));
            }
            Some(s) => {
                if !self.buttons.contains_key(&s.button) {
                    return Err(format!(
                        "shift.button '{}' is not a button in this profile. It has to be a key \
                         that can actually be pressed, because spelling presses it.",
                        s.button
                    ));
                }
                if let Some(b) = self.buttons.get(&s.button)
                    && !b.shift_types.is_empty()
                {
                    return Err(format!(
                        "shift.button '{}' carries a shifted legend of its own. Reaching it would \
                         mean pressing shift to press shift, so spelling could never produce it.",
                        s.button
                    ));
                }
            }
            None => {}
        }

        // One legend, one key. Checked at load, so an ambiguous keypad is refused where it is
        // written rather than surfacing later as a string spelled with the wrong key.
        for field in ["types", "shift_types"] {
            let mut seen: BTreeMap<&str, &str> = BTreeMap::new();
            for (name, b) in &self.buttons {
                let v = if field == "types" { &b.types } else { &b.shift_types };
                if v.is_empty() {
                    continue;
                }
                if let Some(first) = seen.insert(v.as_str(), name.as_str()) {
                    return Err(format!(
                        "buttons '{first}' and '{name}' both say {field} {v:?}. A legend has to \
                         identify one key, or a string containing it would be spelled with \
                         whichever key happened to sort first."
                    ));
                }
            }
        }
        Ok(())
    }

    /// Look up a region by name, already shifted by its anchor. `"@client"` is built in and
    /// means the whole client area, which by definition does not move with anything.
    pub fn region(
        &self,
        name: &str,
        client: (i32, i32),
        offsets: &std::collections::HashMap<String, (i32, i32)>,
    ) -> Result<Rect, String> {
        if name.is_empty() || name == "@client" {
            return Ok([0, 0, client.0, client.1]);
        }
        // The old spelling. It was a bare word that happened to be taken, which is exactly what
        // the `@` prefix exists to stop; a caller still sending it means the whole client area,
        // so say that rather than "unknown region" and let them fix it in one edit.
        if name == "client" {
            return Err(
                "region 'client' is now '@client'. Reserved values all start with '@' so that a \
                 name can never collide with one - the bare word is an ordinary name now, and \
                 this profile has no region called that."
                    .to_string(),
            );
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
                .map(|t| shift(t.rect, Self::offset_for(offsets, &t.anchor)))
                .ok_or_else(|| format!("unknown button '{b}' in capture region 'button:{b}' (known: {})",
                                       self.button_names()));
        }
        self.regions
            .get(name)
            .map(|r| shift(r.rect, Self::offset_for(offsets, &r.anchor)))
            .ok_or_else(|| format!(
                "unknown capture region '{name}' (known: @client, {}; or button:NAME for a button's own rect)",
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

    /// Where a button gets pressed (client coordinates), at the factor `coordinate_scale`
    /// gave and shifted by its anchor.
    ///
    /// The offset is applied after the scale, in whole pixels, because it is measured on the
    /// live window — it is already in the coordinates being pressed, not in the saved ones.
    pub fn click_point(t: &ButtonDef, scale: (f64, f64), by: (i32, i32)) -> (i32, i32) {
        let (x, y) = match t.point {
            Some([px, py]) => (px as f64, py as f64),
            None => {
                let [rx, ry, rw, rh] = t.rect;
                (rx as f64 + rw as f64 / 2.0, ry as f64 + rh as f64 / 2.0)
            }
        };
        ((x * scale.0).round() as i32 + by.0, (y * scale.1).round() as i32 + by.1)
    }

    /// Move a rectangle by an anchor's offset. Sizes never change: the whole reason an
    /// anchor can be trusted is that what it corrects is a translation.
    pub fn shift_rect(r: Rect, by: (i32, i32)) -> Rect {
        [r[0] + by.0, r[1] + by.1, r[2], r[3]]
    }

    /// Move a rectangle from one anchor box to another, keeping its place **within** the box.
    ///
    /// This is the one thing an anchor cannot do. An anchor produces a translation, which is
    /// exactly right while the container keeps its size — and a container that grew from
    /// 708x238 to 746x251 makes every rectangle inside it wrong by an amount that depends on
    /// how far it sits from the container's own origin. No single offset can express that,
    /// which is why the profile had to be rewritten by hand instead.
    ///
    /// **The arithmetic is proportional, and that is an assumption, not a measurement.** It
    /// says the layout was scaled; it does not hold for a panel that re-flowed its keys onto
    /// different rows. So nothing computed here is saved until each button has been checked
    /// against a real control on the live window.
    pub fn refit_rect(r: Rect, from: Rect, to: Rect) -> Rect {
        let (sx, sy) = refit_factor(from, to);
        [
            to[0] + ((r[0] - from[0]) as f64 * sx).round() as i32,
            to[1] + ((r[1] - from[1]) as f64 * sy).round() as i32,
            ((r[2] as f64 * sx).round() as i32).max(1),
            ((r[3] as f64 * sy).round() as i32).max(1),
        ]
    }

    /// The same move, for a lone point. A button's explicit `point` is a place inside the
    /// container, not a size, so only the origin and the factor apply.
    pub fn refit_point(p: [i32; 2], from: Rect, to: Rect) -> [i32; 2] {
        let (sx, sy) = refit_factor(from, to);
        [
            to[0] + ((p[0] - from[0]) as f64 * sx).round() as i32,
            to[1] + ((p[1] - from[1]) as f64 * sy).round() as i32,
        ]
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


/// One step of a spelled string.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Spelled {
    /// The button to press.
    pub button: String,
    /// The characters this press enters. Empty for a shift press, which enters nothing.
    pub enters: String,
    /// True when this press is the shift key rather than a legend.
    pub is_shift: bool,
    /// The legend was matched ignoring case — `"g"` spelled with the `G` key. Reported so a
    /// caller can see that the string it gets is not character-for-character what it sent.
    pub case_folded: bool,
}

/// What could not be spelled, and why.
#[derive(Debug)]
pub struct Unspellable {
    /// Where in the string, counted in characters.
    pub at: usize,
    pub character: String,
}

impl Targets {
    /// Every legend this profile can enter, longest first.
    ///
    /// Longest first is what lets one key be legended `";"` and another `"G0"` without the
    /// shorter one shadowing the longer: at each position the longest legend that fits is the
    /// one taken. Ties cannot happen — `validate_legends` refuses two keys with one legend.
    fn legends(&self) -> Vec<(&str, &str, bool)> {
        let mut v: Vec<(&str, &str, bool)> = Vec::new();
        for (name, b) in &self.buttons {
            if !b.types.is_empty() {
                v.push((b.types.as_str(), name.as_str(), false));
            }
            if !b.shift_types.is_empty() {
                v.push((b.shift_types.as_str(), name.as_str(), true));
            }
        }
        v.sort_by(|a, b| b.0.chars().count().cmp(&a.0.chars().count()).then(a.0.cmp(b.0)));
        v
    }

    /// **Spell a string as key presses.**
    ///
    /// Returns the whole sequence, or every character that has no key — every one of them,
    /// not the first, so a caller fixes the string once rather than a character per round trip.
    ///
    /// Shift is inserted according to the profile's declared mode, and a `toggle` panel is
    /// always left off: a sequence that ends with shift latched would change what the *next*
    /// caller's presses mean, which is the kind of state nobody thinks to check.
    pub fn spell(&self, text: &str) -> Result<Vec<Spelled>, Vec<Unspellable>> {
        let legends = self.legends();
        let chars: Vec<char> = text.chars().collect();
        let mut out: Vec<Spelled> = Vec::new();
        let mut bad: Vec<Unspellable> = Vec::new();
        let mut shift_on = false;
        let mut i = 0usize;

        while i < chars.len() {
            let rest: String = chars[i..].iter().collect();
            // Exact before case-folded, so a keypad that really does carry both cases is
            // spelled with the key that says so.
            let hit = legends
                .iter()
                .find(|(lit, _, _)| rest.starts_with(lit))
                .map(|m| (m, false))
                .or_else(|| {
                    let lower = rest.to_lowercase();
                    legends
                        .iter()
                        .find(|(lit, _, _)| lower.starts_with(&lit.to_lowercase()))
                        .map(|m| (m, true))
                });

            let Some(((lit, name, needs_shift), folded)) = hit else {
                bad.push(Unspellable { at: i, character: chars[i].to_string() });
                i += 1;
                continue;
            };
            let n = lit.chars().count();

            if let Some(s) = &self.shift {
                let want = *needs_shift;
                match s.mode {
                    // Falls back by itself, so it is pressed before each shifted key and never
                    // pressed to turn off.
                    ShiftMode::Oneshot if want => out.push(shift_press(&s.button)),
                    ShiftMode::Oneshot => {}
                    // A latch is only touched when the state has to change.
                    ShiftMode::Toggle if want != shift_on => {
                        out.push(shift_press(&s.button));
                        shift_on = want;
                    }
                    ShiftMode::Toggle => {}
                }
            }
            out.push(Spelled {
                button: (*name).to_string(),
                enters: chars[i..i + n].iter().collect(),
                is_shift: false,
                case_folded: folded,
            });
            i += n;
        }

        if !bad.is_empty() {
            return Err(bad);
        }
        // Leave the panel as it was found.
        if shift_on && let Some(s) = &self.shift {
            out.push(shift_press(&s.button));
        }
        Ok(out)
    }
}

fn shift_press(button: &str) -> Spelled {
    Spelled { button: button.to_string(), enters: String::new(), is_shift: true, case_folded: false }
}

/// Whether a menu path is covered by a `confirm_menus` entry.
///
/// Prefix, but only on **whole segments**: `"File"` covers `"File/Save"` and does not cover
/// `"Filename Options/..."`. A plain `starts_with` would protect the first and quietly also
/// the second, and a protection that covers more than it says is as confusing as one that
/// covers less.
pub fn menu_needs_confirm(path: &str, guarded: &[String]) -> Option<String> {
    let norm = |s: &str| s.trim().trim_matches('/').to_ascii_lowercase();
    let p = norm(path);
    guarded.iter().find(|g| {
        let g = norm(g);
        !g.is_empty() && (p == g || p.starts_with(&format!("{g}/")))
    })
    .cloned()
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
/// Values that mean something to the server rather than naming something in the profile.
/// They all begin with `@`, and no name may, so the two can never be confused.
pub const RESERVED_PREFIX: char = '@';

fn check_name(kind: &str, name: &str) -> Result<(), String> {
    if name.trim().is_empty() {
        return Err(format!("a {kind} name must not be empty or whitespace"));
    }
    if name.starts_with(RESERVED_PREFIX) {
        return Err(format!(
            "{kind} name '{name}' starts with '{RESERVED_PREFIX}', which is reserved for values \
             the server defines - '@client' is the whole client area, '@fixed' is an element \
             that does not move. Reserving the whole prefix rather than individual words means \
             a name can never collide with one, now or later. Pick a name without it."
        ));
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
            program: String::new(),
            window: WindowSpec {
                title: "Sample".into(),
                title_exact: false,
                class: String::new(),
                has: Vec::new(),
            },
            reference_client: Some([1280, 1000]),
            on_size_mismatch: SizeMismatch::Reject,
            anchors: BTreeMap::new(),
            regions: BTreeMap::new(),
            buttons: BTreeMap::new(),
            confirm_menus: Vec::new(),
            shift: None,
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
                anchor: String::new(),
                hold_ms: None,
                types: String::new(),
                shift_types: String::new(),
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
                anchor: "@fixed".into(),
                hold_ms: Some(250),
                types: String::new(),
                shift_types: String::new(),
                settle_ms: Some(1500),
                note: "undoing this needs a person at the machine".into(),
            },
        );
        t.regions.insert("status_bar".into(), RegionDef { rect: [0, 940, 1280, 60], anchor: String::new(), note: String::new() });
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

    /// The whole client area is placed without consulting an anchor; a named region is not.
    ///
    /// This is what lets a capture of the screen through when a profile's anchors cannot be
    /// found on the window in front of it. Refusing that too meant you could not look at the
    /// screen to discover that a different build of the application was running - which is the
    /// one thing that would have told you immediately.
    #[test]
    fn the_client_area_needs_no_anchor_but_a_named_region_does() {
        let t: Targets = serde_json::from_str(
            r#"{"window":{"title":"x"},
                "anchors":{"screen":{"text":"CNC","rect":[0,0,100,100]}},
                "regions":{"bar":{"rect":[10,20,30,40],"anchor":"screen"}},
                "buttons":{}}"#,
        )
        .expect("parses");

        // Nothing resolved: the state when an anchor could not be found at all.
        let none = std::collections::HashMap::new();
        assert_eq!(
            t.region("@client", (1920, 997), &none).expect("the window is the window"),
            [0, 0, 1920, 997]
        );
        // ...and it does not move when an anchor *is* resolved either, since it is not placed
        // by one.
        let mut resolved = std::collections::HashMap::new();
        resolved.insert("screen".to_string(), (16, 0));
        assert_eq!(t.region("@client", (1920, 997), &resolved).expect("still the window"), [0, 0, 1920, 997]);

        // A named region is placed by its anchor, so the offset is the whole point.
        assert_eq!(t.region("bar", (1920, 997), &none).expect("no offset"), [10, 20, 30, 40]);
        assert_eq!(t.region("bar", (1920, 997), &resolved).expect("shifted"), [26, 20, 30, 40]);
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

    /// The case that made this necessary: a container that grew. The whole point is that
    /// elements move by *different* amounts depending on where they sit inside it, which is
    /// the one thing an anchor's single offset cannot say.
    #[test]
    fn a_grown_container_moves_its_far_side_further_than_its_near_side() {
        // Measured on NC Trainer2 plus: the operator panel between two builds.
        let was = [1158, 384, 708, 238];
        let now = [1142, 384, 746, 251];

        // A key at the container's own origin moves exactly as the container did.
        let near = Targets::refit_rect([1158, 384, 44, 44], was, now);
        assert_eq!([near[0], near[1]], [1142, 384]);

        // One at the far corner moves further, because the container grew underneath it.
        let far = Targets::refit_rect([1158 + 708 - 44, 384 + 238 - 44, 44, 44], was, now);
        assert_eq!(far[0], 1142 + 746 - 46, "the far edge tracks the new width");

        // Which is the whole claim: one offset cannot serve both.
        assert_ne!(near[0] - 1158, far[0] - (1158 + 708 - 44));

        // A key keeps its proportion of the container, so it grows with it.
        assert!(far[2] > 44 && far[3] > 44, "{far:?}");

        // And the corners stay inside: a refit must not put a rectangle outside the container
        // it was measured inside.
        for r in [near, far] {
            assert!(r[0] >= now[0] && r[1] >= now[1], "{r:?} starts outside {now:?}");
            assert!(r[0] + r[2] <= now[0] + now[2] + 1, "{r:?} runs past {now:?}");
            assert!(r[1] + r[3] <= now[1] + now[3] + 1, "{r:?} runs past {now:?}");
        }
    }

    /// A container that only moved is the case anchors already handle, and the refit must
    /// agree with them exactly — otherwise re-seating a profile that was fine would nudge
    /// every coordinate by a rounding error.
    #[test]
    fn a_container_that_only_moved_is_a_plain_translation() {
        let was = [1158, 384, 708, 238];
        let now = [1142, 384, 708, 238];
        let r = [1200, 400, 44, 44];
        assert_eq!(Targets::refit_rect(r, was, now), Targets::shift_rect(r, (-16, 0)));
        assert_eq!(Targets::refit_point([1222, 422], was, now), [1206, 422]);
    }

    /// An explicit `point` is a place inside the container, not a size — it takes the origin
    /// and the factor and nothing else. A button with an asymmetric switch would otherwise
    /// keep a press point that no longer sits on the switch.
    #[test]
    fn an_explicit_point_is_carried_across_with_its_rectangle() {
        let was = [0, 0, 100, 100];
        let now = [10, 20, 200, 300];
        assert_eq!(Targets::refit_point([50, 50], was, now), [110, 170]);
        // The rectangle it belongs to lands consistently with it.
        let r = Targets::refit_rect([40, 40, 20, 20], was, now);
        assert_eq!(r, [90, 140, 40, 60]);
    }

    /// A zero-width container could never have matched a control, and dividing by it would
    /// produce infinities that serialize as coordinates.
    #[test]
    fn a_degenerate_container_scales_by_one_rather_than_by_infinity() {
        let r = Targets::refit_rect([5, 5, 10, 10], [0, 0, 0, 0], [7, 9, 0, 0]);
        assert_eq!(r, [12, 14, 10, 10]);
    }

    fn keypad(shift_mode: &str) -> Targets {
        let shift = if shift_mode.is_empty() {
            String::new()
        } else {
            format!(r#","shift":{{"button":"SHIFT","mode":"{shift_mode}"}}"#)
        };
        let doc = format!(
            r#"{{"window":{{"title":"x"}},"buttons":{{
                "MDI_G":{{"rect":[0,0,10,10],"types":"G"}},
                "MDI_9":{{"rect":[10,0,10,10],"types":"9"}},
                "MDI_1":{{"rect":[20,0,10,10],"types":"1"}},
                "MDI_F":{{"rect":[30,0,10,10],"types":"F","shift_types":"E"}},
                "MDI_X":{{"rect":[40,0,10,10],"types":"X","shift_types":"U"}},
                "MDI_EOB":{{"rect":[50,0,10,10],"types":";"}},
                "SHIFT":{{"rect":[60,0,10,10]}},
                "CYCLE_START":{{"rect":[70,0,10,10]}}
            }}{shift}}}"#
        );
        serde_json::from_str(&doc).expect("parses")
    }

    fn spelt(t: &Targets, text: &str) -> Vec<String> {
        t.spell(text).expect("spellable").into_iter().map(|s| s.button).collect()
    }

    /// The fact that used to live in an English sentence: E is on the F key, behind shift.
    #[test]
    fn a_shifted_legend_is_reached_through_the_declared_shift_key() {
        let t = keypad("oneshot");
        assert_eq!(spelt(&t, "G91"), ["MDI_G", "MDI_9", "MDI_1"]);
        assert_eq!(spelt(&t, "E"), ["SHIFT", "MDI_F"]);
        // One-shot falls back by itself, so each shifted key gets its own press.
        assert_eq!(spelt(&t, "EE"), ["SHIFT", "MDI_F", "SHIFT", "MDI_F"]);
        assert_eq!(spelt(&t, "EF"), ["SHIFT", "MDI_F", "MDI_F"]);
    }

    /// A latch is pressed only when the state has to change — and is always turned off at the
    /// end. Left on, it would change what the NEXT caller's presses mean, which is exactly the
    /// kind of state nobody thinks to check.
    #[test]
    fn a_latching_shift_is_toggled_only_when_needed_and_never_left_on() {
        let t = keypad("toggle");
        assert_eq!(spelt(&t, "EE"), ["SHIFT", "MDI_F", "MDI_F", "SHIFT"]);
        assert_eq!(spelt(&t, "EU"), ["SHIFT", "MDI_F", "MDI_X", "SHIFT"]);
        assert_eq!(spelt(&t, "EFE"), ["SHIFT", "MDI_F", "SHIFT", "MDI_F", "SHIFT", "MDI_F", "SHIFT"]);
        // Nothing shifted, nothing pressed.
        assert_eq!(spelt(&t, "G91"), ["MDI_G", "MDI_9", "MDI_1"]);

        // A string that ENDS on a shifted character is where leaving it on would happen.
        let seq = t.spell("FE").expect("spellable");
        assert!(seq.last().expect("nonempty").is_shift, "the latch has to come back off");
        assert_eq!(spelt(&t, "FE"), ["MDI_F", "SHIFT", "MDI_F", "SHIFT"]);
    }

    /// Every character that cannot be entered comes back, not the first one. A caller fixing a
    /// string one character per round trip is a caller pressing keys in between.
    #[test]
    fn an_unspellable_string_names_every_character_that_failed() {
        let t = keypad("oneshot");
        let bad = t.spell("G@1#").expect_err("has no key for @ or #");
        let got: Vec<&str> = bad.iter().map(|u| u.character.as_str()).collect();
        assert_eq!(got, ["@", "#"]);
        assert_eq!(bad[0].at, 1);
        assert_eq!(bad[1].at, 3);
    }

    /// Case is folded rather than refused, because a keypad is upper case and a caller writing
    /// `g91` means `G91` — but the fold is reported, so nobody has to assume the string that
    /// went in is character-for-character the string they sent.
    #[test]
    fn lower_case_is_spelled_on_the_upper_case_key_and_said_so() {
        let t = keypad("oneshot");
        let seq = t.spell("g9").expect("spellable");
        assert_eq!(seq[0].button, "MDI_G");
        assert!(seq[0].case_folded, "the fold has to be visible");
        assert!(!seq[1].case_folded, "a digit was not folded");
    }

    /// A shifted legend nothing can reach is a fact written down and then unusable, which is
    /// the same as not writing it down. Refused where it is written.
    #[test]
    fn a_shifted_legend_without_a_declared_shift_key_does_not_load() {
        let doc = r#"{"window":{"title":"x"},"buttons":{
            "MDI_F":{"rect":[0,0,10,10],"types":"F","shift_types":"E"}}}"#;
        let e = serde_json::from_str::<Targets>(doc)
            .expect("parses")
            .validate()
            .expect_err("no shift declared");
        assert!(e.contains("shift"), "{e}");

        // And the shift key has to be a button that exists, since spelling presses it.
        let doc = r#"{"window":{"title":"x"},"buttons":{
            "MDI_F":{"rect":[0,0,10,10],"types":"F","shift_types":"E"}},
            "shift":{"button":"NOPE","mode":"oneshot"}}"#;
        let e = serde_json::from_str::<Targets>(doc)
            .expect("parses")
            .validate()
            .expect_err("no such button");
        assert!(e.contains("NOPE"), "{e}");
    }

    /// Two keys claiming one legend would spell a string with whichever sorted first, which is
    /// not a decision anybody made.
    #[test]
    fn two_keys_cannot_claim_the_same_legend() {
        let doc = r#"{"window":{"title":"x"},"buttons":{
            "A":{"rect":[0,0,10,10],"types":"7"},
            "B":{"rect":[10,0,10,10],"types":"7"}}}"#;
        let e = serde_json::from_str::<Targets>(doc)
            .expect("parses")
            .validate()
            .expect_err("ambiguous legend");
        assert!(e.contains("'A'") && e.contains("'B'"), "{e}");
    }

    /// A key legended with several characters is taken before the single-character keys that
    /// would otherwise shadow it.
    #[test]
    fn the_longest_legend_wins_at_each_position() {
        let doc = r#"{"window":{"title":"x"},"buttons":{
            "G":{"rect":[0,0,10,10],"types":"G"},
            "ZERO":{"rect":[10,0,10,10],"types":"0"},
            "G0":{"rect":[20,0,10,10],"types":"G0"}}}"#;
        let t: Targets = serde_json::from_str(doc).expect("parses");
        t.validate().expect("valid");
        assert_eq!(spelt(&t, "G0"), ["G0"]);
        assert_eq!(spelt(&t, "0G"), ["ZERO", "G"]);
    }

    /// A confirm entry covers whole segments, not characters. Plain `starts_with` would let
    /// "File" also protect "Filename Options", and a guard that covers more than it says is as
    /// confusing as one that covers less - either way nobody can tell from the profile which
    /// items need a second look.
    #[test]
    fn a_confirm_menu_entry_covers_whole_segments_only() {
        let g = vec!["File".to_string(), "Tool/Set Machine Parameters".to_string()];

        assert_eq!(menu_needs_confirm("File", &g).as_deref(), Some("File"));
        assert_eq!(menu_needs_confirm("File/Save", &g).as_deref(), Some("File"));
        assert_eq!(menu_needs_confirm("File/Recent/1", &g).as_deref(), Some("File"));
        assert_eq!(
            menu_needs_confirm("Tool/Set Machine Parameters", &g).as_deref(),
            Some("Tool/Set Machine Parameters")
        );

        // The near misses.
        assert_eq!(menu_needs_confirm("Filename Options/Save", &g), None);
        assert_eq!(menu_needs_confirm("Tool", &g), None);
        assert_eq!(menu_needs_confirm("Tool/Options", &g), None);
        assert_eq!(menu_needs_confirm("Help/About", &g), None);

        // Case and stray slashes are the menu's presentation, not the caller's mistake.
        assert_eq!(menu_needs_confirm("file/save", &g).as_deref(), Some("File"));
        assert_eq!(menu_needs_confirm("/File/Save", &g).as_deref(), Some("File"));

        // An empty entry would otherwise match every path and guard the whole menu by accident.
        assert_eq!(menu_needs_confirm("File/Save", &["".to_string()]), None);
        assert_eq!(menu_needs_confirm("File/Save", &[]), None);
    }

    /// The editor used to rebuild a profile without its anchors map and save it. Every
    /// button kept its anchor name, validation only looked at names when anchors existed, and
    /// the correction for a moving layout switched itself off. A name that points at nothing
    /// is refused now.
    #[test]
    fn an_anchor_name_with_no_anchors_behind_it_is_refused() {
        let lost = r#"{"window":{"title":"x"},"buttons":{
            "A":{"rect":[0,0,10,10],"anchor":"main"},
            "B":{"rect":[20,0,10,10],"anchor":"@fixed"}}}"#;
        let e = serde_json::from_str::<Targets>(lost)
            .expect("parses")
            .validate()
            .expect_err("'main' refers to nothing");
        assert!(e.contains("A -> main"), "{e}");
        assert!(!e.contains("B ->"), "@fixed means no shift, which is what happens: {e}");

        // Only @fixed and empty names, with no anchors: nothing to correct, nothing lost.
        let fine = r#"{"window":{"title":"x"},"buttons":{
            "A":{"rect":[0,0,10,10]},
            "B":{"rect":[20,0,10,10],"anchor":"@fixed"}}}"#;
        serde_json::from_str::<Targets>(fine).expect("parses").validate().expect("valid");
    }

    #[test]
    fn scale_policy_scales_coordinates() {
        let mut t = sample();
        t.on_size_mismatch = SizeMismatch::Scale;
        let s = t.coordinate_scale((2560, 2000)).expect("scale");
        assert_eq!(s, (2.0, 2.0));
        let target = &t.buttons["cycle_start"];
        // the centre of rect [820,640,60,40] is (850, 660), doubled
        assert_eq!(Targets::click_point(target, s, (0, 0)), (1700, 1320));
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
                anchor: String::new(),
                hold_ms: None,
                types: String::new(),
                shift_types: String::new(),
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
        assert_eq!(Targets::click_point(&t.buttons["cycle_start"], (1.0, 1.0), (0, 0)), (850, 660));
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

    /// Anchors: what identifies one, and what happens when nothing does.
    ///
    /// Built from the real thing. NC Trainer2 plus has two containers called
    /// `OPERATION PANEL`, and the whole layout moves 16px sideways between runs while every
    /// size stays the same. Text alone cannot tell the two apart; size can, and size is
    /// exactly what a translation leaves untouched.
    #[test]
    fn an_anchor_is_found_by_text_and_size_together() {
        let ctl = |text: &str, rect: Rect| crate::win::window::ControlInfo {
            class: "Afx:00F20000:3:00000000:0B100968:0".into(),
            text: text.into(),
            id: 0,
            rect,
            depth: 2,
            visible: true,
        };
        // State B, 16px left of where the profile was measured.
        let live = vec![
            ctl("NC DISPLAY", [38, 92, 1104, 818]),
            ctl("OPERATION PANEL", [1142, 384, 328, 238]),
            ctl("OPERATION PANEL", [1142, 622, 708, 238]),
            ctl("NC KEYBOARD", [1470, 92, 316, 530]),
        ];

        let mut t = sample();
        t.anchors.insert(
            "screen".into(),
            AnchorDef { text: "NC DISPLAY".into(), rect: [54, 92, 1104, 818], note: String::new() },
        );
        // The narrower of the two same-named panels, told apart by its size alone.
        t.anchors.insert(
            "sub".into(),
            AnchorDef {
                text: "OPERATION PANEL".into(),
                rect: [1158, 384, 328, 238],
                note: String::new(),
            },
        );

        let off = t.anchor_offsets(&live).expect("both resolve");
        assert_eq!(off["screen"], (-16, 0));
        assert_eq!(off["sub"], (-16, 0), "the 328-wide panel, not the 708-wide one");

        // "@fixed" and an unanchored profile both mean stay put.
        assert_eq!(Targets::offset_for(&off, "@fixed"), (0, 0));
        assert_eq!(Targets::offset_for(&off, ""), (0, 0));
        assert_eq!(Targets::offset_for(&off, "screen"), (-16, 0));

        // Gone: the size no longer matches anything with that text.
        let mut stale = t.clone();
        stale.anchors.get_mut("screen").expect("there").rect = [54, 92, 999, 818];
        let e = stale.anchor_offsets(&live).expect_err("not found");
        assert!(e.contains("not found"), "{e}");
        assert!(e.contains("1 control"), "says how many carry the text: {e}");

        // Ambiguous: two controls match text AND size, so refuse rather than pick one.
        let twins = vec![
            ctl("OPERATION PANEL", [1142, 384, 328, 238]),
            ctl("OPERATION PANEL", [1142, 900, 328, 238]),
        ];
        let mut only_sub = sample();
        only_sub.anchors.insert(
            "sub".into(),
            AnchorDef {
                text: "OPERATION PANEL".into(),
                rect: [1158, 384, 328, 238],
                note: String::new(),
            },
        );
        let e = only_sub.anchor_offsets(&twins).expect_err("ambiguous");
        assert!(e.contains("ambiguous"), "{e}");
    }

    /// Declaring one anchor makes the whole profile answer for itself.
    ///
    /// The half-answered profile is the one worth refusing: the elements that named an anchor
    /// move with the panel, the ones that said nothing stay behind, and the ones that still
    /// work hide the ones that do not.
    #[test]
    fn an_anchored_profile_leaves_nothing_unanswered() {
        let mut t = sample();
        assert!(t.validate().is_ok(), "no anchors, nothing to answer");

        t.anchors.insert(
            "screen".into(),
            AnchorDef { text: "NC DISPLAY".into(), rect: [54, 92, 1104, 818], note: String::new() },
        );
        let e = t.validate().expect_err("now everything must say");
        assert!(e.contains("anchor"), "{e}");

        // Answer everything and it passes; "@fixed" is as valid an answer as a name.
        for b in t.buttons.values_mut() {
            b.anchor = "screen".into();
        }
        for r in t.regions.values_mut() {
            r.anchor = "@fixed".into();
        }
        t.validate().expect("fully answered");

        // A name that is not defined is refused, and the message lists what is.
        t.buttons.values_mut().next().expect("one").anchor = "nope".into();
        let e = t.validate().expect_err("unknown anchor");
        assert!(e.contains("nope") && e.contains("screen"), "{e}");
    }

    /// A reserved word as a region name makes that region unselectable forever — blocked
    /// where the name is made.
    #[test]
    fn reserved_region_names_are_rejected() {
        // Anything starting with '@' is the server's to define, so a name can never be one.
        for name in ["@client", "@fixed", "@anything"] {
            let mut t = sample();
            t.regions.insert(name.to_string(), RegionDef { rect: [0, 0, 10, 10], anchor: String::new(), note: String::new() });
            let err = t.validate().expect_err("a name starting with @ must be rejected");
            assert!(err.contains("reserved"), "{err}");
        }
        // Buttons and keys live in the same namespace rule.
        let mut t = sample();
        t.buttons.insert("@fixed".to_string(), t.buttons.values().next().expect("one").clone());
        assert!(t.validate().is_err(), "a button may not start with @ either");

        // The old bare word is refused once rather than quietly becoming an ordinary region:
        // a profile using it almost certainly meant the whole client area.
        let mut t = sample();
        t.regions.insert("client".to_string(), RegionDef { rect: [0, 0, 10, 10], anchor: String::new(), note: String::new() });
        let err = t.validate().expect_err("the old spelling is refused");
        assert!(err.contains("@client"), "and says what to use instead: {err}");
    }

    /// `button:NAME` captures a button's own rectangle, and `pad` grows it.
    ///
    /// Without these, every caller reads the rect from GET /buttons, adds a margin by hand and
    /// sends it back as `rect=` — arithmetic in the caller against a number the server already
    /// holds, which is exactly where things go quietly wrong.
    #[test]
    fn a_button_can_be_captured_by_name_with_a_margin() {
        let t = sample();
        assert_eq!(t.region("button:cycle_start", (1280, 1000), &Default::default()).expect("by name"), [820, 640, 60, 40]);

        // pad grows every side, so the width and height gain twice the padding.
        assert_eq!(
            Targets::pad_rect(t.region("button:cycle_start", (1280, 1000), &Default::default()).unwrap(), 25),
            [795, 615, 110, 90]
        );
        assert_eq!(Targets::pad_rect([10, 10, 5, 5], 0), [10, 10, 5, 5]);

        // Padding past the window edge is not an error — cropping clamps it.
        assert_eq!(Targets::pad_rect([2, 2, 10, 10], 5), [-3, -3, 20, 20]);

        // An unknown button says so, and says it is a button it could not find.
        let e = t.region("button:nope", (1280, 1000), &Default::default()).unwrap_err();
        assert!(e.contains("unknown button 'nope'"), "{e}");
        // A plain unknown region points at both ways of naming one.
        let e = t.region("nope", (1280, 1000), &Default::default()).unwrap_err();
        assert!(e.contains("button:NAME"), "{e}");
    }

    #[test]
    fn client_region_is_built_in() {
        let t = sample();
        assert_eq!(t.region("@client", (800, 600), &Default::default()).expect("@client"), [0, 0, 800, 600]);
        assert_eq!(t.region("", (800, 600), &Default::default()).expect("default"), [0, 0, 800, 600]);
        assert!(t.region("nope", (800, 600), &Default::default()).is_err());

        // The old spelling does not resolve, and the error names the new one rather than
        // saying "unknown region" to somebody who is asking for exactly the right thing.
        let err = t.region("client", (800, 600), &Default::default()).expect_err("the old spelling is gone");
        assert!(err.contains("@client"), "{err}");
    }
}
