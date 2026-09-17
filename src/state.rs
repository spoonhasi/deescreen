//! Shared state.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use arc_swap::ArcSwap;

use crate::config::Config;
use crate::targets::Targets;

/// One profile — **one window and its entire coordinate universe**.
///
/// A different application puts the same-named button somewhere completely different, so
/// swapping only the window is not a coherent operation. The window spec, buttons, regions,
/// keys and reference size have to move as one bundle, and this is that bundle. Each profile
/// gets its own file too, so saving one never touches another and each keeps its own backup.
pub struct Profile {
    pub name: String,
    pub path: PathBuf,
    /// Replaced wholesale by `POST /admin/reload` and `/admin/profile`.
    /// Reads dominate and writes are rare, which is where `ArcSwap` beats an `RwLock`.
    pub targets: ArcSwap<Targets>,
}

impl Profile {
    pub fn targets(&self) -> Arc<Targets> {
        self.targets.load_full()
    }
}

pub struct AppState {
    pub config: Arc<Config>,
    /// Name → profile. `POST /admin/reload` with no profile named rescans the disk and
    /// replaces this wholesale — drop in a new `deescreen.<name>.json` and it is picked up
    /// without a restart.
    ///
    /// A request that already holds a profile holds an `Arc<Profile>`, so the swap does not
    /// reach it. Which window a request is driving never changes half-way through.
    pub profiles: arc_swap::ArcSwap<BTreeMap<String, Arc<Profile>>>,
    /// The default profile `config.json` **named explicitly**. May be empty, in which case
    /// omitting the profile only works while there is exactly one (see `profile()`).
    pub default_profile: String,
    pub captures_dir: PathBuf,
    /// Serialises input injection — **one lock across every profile**. There is one mouse,
    /// one keyboard and one foreground on a PC, so driving two profiles at once would have
    /// them stealing focus from each other and the wrong window taking the click.
    pub input_lock: Arc<tokio::sync::Mutex<()>>,
    /// Whether `SetProcessDpiAwarenessContext` succeeded — the DPI diagnostic.
    pub dpi_aware: bool,
    /// What the last startup or rescan could not load, in words.
    ///
    /// Written to the log as well, but an agent cannot read the log. Without this, a file
    /// that failed to load simply was not in /health - one profile fewer, no problem listed,
    /// which is the quietest failure there is.
    pub load_notes: arc_swap::ArcSwap<Vec<String>>,
}

/// `AppState` is cloned per handler, so it is wrapped in an `Arc` (`ArcSwap` is not Clone).
pub type SharedState = Arc<AppState>;

impl AppState {
    /// Find a profile by name. `None` means the omit-default (`effective_default`).
    /// An unknown name fails with the list of known names — **nothing is picked for you**.
    pub fn profile(&self, name: Option<&str>) -> Result<Arc<Profile>, String> {
        let snapshot = self.profiles.load();
        if snapshot.is_empty() {
            return Err(
                "no profiles exist yet — a profile is one window plus its buttons and regions. \
                 Open /editor and use [＋ Profile] to create one."
                    .to_string(),
            );
        }
        let known = || snapshot.keys().cloned().collect::<Vec<_>>().join(", ");
        let wanted = match name.map(str::trim).filter(|n| !n.is_empty()) {
            Some(n) => n.to_string(),
            None => match implicit_default(&self.default_profile, &snapshot) {
                Some(n) => n,
                // Picking one here would mean **pressing another application's window.**
                // The same button name sits somewhere different in every application, so
                // when it is ambiguous this asks instead of choosing.
                None => {
                    return Err(format!(
                        "no profile given and no default is set, but {} profiles exist ({}). \
                         Pass profile=<name>, or set \"default_profile\" in config.json to pick \
                         one implicitly.",
                        snapshot.len(),
                        known()
                    ));
                }
            },
        };
        snapshot
            .get(&wanted)
            .cloned()
            .ok_or_else(|| format!("unknown profile '{wanted}' (known: {})", known()))
    }

    /// The name a request omitting `profile` **actually** gets. May be none.
    ///
    /// `/health` and the editor report this one, because the name written in the config and
    /// the name actually in use can differ — when that profile's file failed to load.
    pub fn effective_default(&self) -> Option<String> {
        implicit_default(&self.default_profile, &self.profiles.load())
    }

    pub fn profile_names(&self) -> Vec<String> {
        self.profiles.load().keys().cloned().collect()
    }
}

/// Read the profiles the config points at from disk and build the map.
///
/// Startup and the `/admin/reload` rescan **run the same function** — different rules on one
/// side would produce differences like "it works after a restart but not after a reload".
///
/// **Finding none is a success.** Profiles are made in the editor, and having none is not a
/// fault but the normal state of "not made yet". A broken file is skipped loudly and the rest
/// live — if one bad file out of three took the other two down, you could not get in to fix
/// it.
pub fn build_profiles(config: &Config) -> (BTreeMap<String, Arc<Profile>>, Vec<String>) {
    let mut out: BTreeMap<String, Arc<Profile>> = BTreeMap::new();
    let mut notes: Vec<String> = Vec::new();

    for (name, rel) in config.effective_profiles() {
        let path = crate::config::in_home(&rel);
        if !path.exists() {
            notes.push(format!("profile '{name}': {} is missing — skipped", path.display()));
            continue;
        }
        let targets = match Targets::load(&path) {
            Ok(t) => t,
            Err(e) => {
                notes.push(format!("profile '{name}': SKIPPED — {e}"));
                continue;
            }
        };
        out.insert(
            name.clone(),
            Arc::new(Profile {
                name,
                path,
                targets: ArcSwap::from(Arc::new(targets)),
            }),
        );
    }
    (out, notes)
}

/// The name the config chose if it is loaded; otherwise the only profile, **and only if there
/// is exactly one**.
///
/// Two or more with nothing named in the config gives `None`. This used to take the first in
/// the list, which meant that adding one profile could quietly re-aim every request that
/// omitted the name. A structure where alphabetical order decides what gets driven is exactly
/// what naming a button exists to prevent.
fn implicit_default(configured: &str, snapshot: &BTreeMap<String, Arc<Profile>>) -> Option<String> {
    if !configured.is_empty() && snapshot.contains_key(configured) {
        return Some(configured.to_string());
    }
    if configured.is_empty() && snapshot.len() == 1 {
        return snapshot.keys().next().cloned();
    }
    None
}
