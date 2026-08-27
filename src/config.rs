//! `config.json` — server settings. Read once at startup and never again.
//!
//! **Strict JSON, no comments.** This once stripped `//` so the reasoning could sit beside each
//! value, and then a profile file — which the server does rewrite — lost every comment the
//! first time it was saved. Documentation that a save can delete is worse than none. So the
//! files are plain JSON that any tool can read, and the explanations live in the README and in
//! `/help`, where nothing overwrites them.
//!
//! (The part you actually edit often is the profile files, and those hot-reload with
//! `POST /admin/reload`. The bind address and the whitelists change only on a restart —
//! making the access boundary swappable while the server is running would itself be the hole.)

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Deserialize, Serialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// Bind address. `127.0.0.1` = this PC only, `0.0.0.0` = every NIC.
    #[serde(default = "default_host")]
    pub host: String,
    #[serde(default = "default_port")]
    pub port: u16,

    /// IPs allowed on the read endpoints (`/health` `/windows` `/capture` …).
    pub allowed_ips_read: Vec<String>,
    /// IPs allowed on the **control** endpoints (`/click` `/key` `/window/*`).
    /// Independent of the read list — read access does not confer control access.
    /// An empty array blocks control entirely (observation-only mode).
    pub allowed_ips_write: Vec<String>,

    /// The code for `/admin/*`. An empty string disables it (the whitelist alone).
    /// Clients send it in the `X-Admin-Code` header.
    #[serde(default)]
    pub admin_code: String,

    /// **Profiles** — name → definition file. How one instance drives several windows.
    ///
    /// A different application is an entirely different coordinate universe (NC Trainer's
    /// buttons are not where NC Guide's are), so swapping only the window is not a coherent
    /// operation. The window, buttons, regions, keys and reference size have to change as one
    /// bundle, and that bundle is a profile.
    ///
    /// Why separate files: saving one profile in the editor never touches another, each keeps
    /// its own `.bak`, and file permissions can differ per profile.
    ///
    /// ```json
    /// "profiles": { "ncguide": "targets.ncguide.json", "nctrainer": "targets.nctrainer.json" },
    /// "default_profile": "ncguide"
    /// ```
    #[serde(default, deserialize_with = "no_duplicate_keys")]
    pub profiles: BTreeMap<String, String>,

    /// Which profile a request gets when it omits `profile`.
    #[serde(default)]
    pub default_profile: String,

    #[serde(default)]
    pub captures: CaptureConfig,

    /// Whether **unnamed coordinates** may be clicked. Defaults to `false`.
    ///
    /// This flag is the safety boundary. At `false`, only the names written in the profile
    /// file can be pressed, which is what makes **whoever can edit that file the one who holds
    /// control authority**. Turn it on while measuring new coordinates and turn it back off.
    #[serde(default)]
    pub allow_raw_clicks: bool,
    /// Whether unnamed **key input** (`keys`/`text` on `POST /key`) is allowed. Defaults to
    /// `false`. On an application that maps panel keys to the PC keyboard, a key is as powerful
    /// as a click, so it sits at the same level.
    #[serde(default)]
    pub allow_raw_keys: bool,

    /// Whether `/editor` may **write** `profiles/deescreen.<name>.json` directly.
    /// Defaults to `false`.
    ///
    /// Know exactly what this means before turning it on. While off, the only way to change a
    /// profile is editing the file on that PC, so **the permission boundary is file
    /// permissions**. On, the boundary moves to **HTTP reachability** — anyone on the control
    /// whitelist can create new buttons for themselves.
    ///
    /// On a fresh install with no profiles, this has to be on for the editor to create the
    /// first one. Turn it off afterwards and the definitions freeze.
    ///
    /// With it on, any client on the control IP list can save. Setting `admin_code` adds one
    /// more layer, but it is **not required** — it started out required and was removed once
    /// real use showed it produced only friction (2026-08-26).
    #[serde(default)]
    pub allow_profile_editing: bool,

    /// Default wait between an input and the re-capture (ms).
    #[serde(default = "default_settle_ms")]
    pub default_settle_ms: u64,
    /// Ceiling on the wait a request may ask for (ms), so an HTTP connection is not held open.
    #[serde(default = "default_max_settle_ms")]
    pub max_settle_ms: u64,

    /// How long a press stays down (ms).
    ///
    /// Zero is not a press at all: down and up in one batch close and open the contact inside
    /// a single input tick. A Win32 button does not mind, because it latches on the down. A
    /// simulated machine key does — something scans that contact on a cycle, and a press that
    /// exists for no measurable time is one no scan ever sees, so nothing happens and nothing
    /// says why. The default is roughly how long a person leans on a key. Where a panel needs
    /// longer, this is the place: the scan rate belongs to the application, not to each caller.
    #[serde(default = "default_hold_ms")]
    pub default_hold_ms: u64,
    /// Ceiling on the hold a request may ask for (ms). The input lock is held for the whole
    /// press, so an unbounded hold would stop everything else on that window for that long.
    #[serde(default = "default_max_hold_ms")]
    pub max_hold_ms: u64,
}

#[derive(Deserialize, Serialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct CaptureConfig {
    /// Where capture PNGs accumulate. A relative path is resolved inside the home directory;
    /// an absolute one is taken as written, which is how this can point at another disk.
    #[serde(default = "default_capture_dir")]
    pub dir: String,
    /// How many recent files to keep; the oldest beyond this are deleted.
    #[serde(default = "default_keep")]
    pub keep: usize,
    /// Delete files older than this many minutes regardless of count. `0` = no age limit.
    #[serde(default = "default_max_age_minutes")]
    pub max_age_minutes: u64,
}

impl Default for CaptureConfig {
    fn default() -> Self {
        CaptureConfig {
            dir: default_capture_dir(),
            keep: default_keep(),
            max_age_minutes: default_max_age_minutes(),
        }
    }
}

fn default_host() -> String {
    "127.0.0.1".to_string()
}
fn default_port() -> u16 {
    8090
}
fn default_capture_dir() -> String {
    "captures".to_string()
}
fn default_keep() -> usize {
    200
}
fn default_max_age_minutes() -> u64 {
    1440 // one day
}
fn default_settle_ms() -> u64 {
    // Raised from 300 after driving a real panel. A capture taken before the screen has caught
    // up is not an error — it is the previous screen handed back as the result, which reads
    // exactly like the operation having failed. Being late costs a fraction of a second;
    // being early costs a wrong answer that looks right.
    500
}
fn default_max_settle_ms() -> u64 {
    10_000
}
fn default_hold_ms() -> u64 {
    80
}
fn default_max_hold_ms() -> u64 {
    2_000
}

/// A map that **refuses at parse time** when a key appears twice.
///
/// JSON does not forbid duplicate keys and the default parser lets the later one win. But
/// buttons in this tool are a map keyed by name, so a collision means **one of them is already
/// gone by the time the server sees the document.** You send 128 and 127 are saved, and the
/// response says success.
///
/// This collision really happens: the MDI keypad's letter keys X/Y/Z and the operator panel's
/// axis-select X/Y/Z naturally end up with the same names.
///
/// Making clients send a count to cross-check (`expect_buttons=128`) would work too, but that
/// is a discipline every client has to remember, and some client will always forget. Blocking
/// it in the parser makes **every path safe at once** — file or HTTP.
pub(crate) fn no_duplicate_keys<'de, D, V>(d: D) -> Result<BTreeMap<String, V>, D::Error>
where
    D: serde::Deserializer<'de>,
    V: serde::Deserialize<'de>,
{
    use serde::de::{Error, MapAccess, Visitor};

    struct NoDup<V>(std::marker::PhantomData<V>);

    impl<'de, V: serde::Deserialize<'de>> Visitor<'de> for NoDup<V> {
        type Value = BTreeMap<String, V>;

        fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
            f.write_str("an object with unique keys")
        }

        fn visit_map<A: MapAccess<'de>>(self, mut m: A) -> Result<Self::Value, A::Error> {
            let mut out = BTreeMap::new();
            while let Some((k, v)) = m.next_entry::<String, V>()? {
                if out.contains_key(&k) {
                    return Err(A::Error::custom(format!(
                        "duplicate name '{k}' — the later one would silently replace the \
                         earlier one and you would save fewer entries than you sent"
                    )));
                }
                out.insert(k, v);
            }
            Ok(out)
        }
    }

    d.deserialize_map(NoDup(std::marker::PhantomData))
}

impl Config {
    /// The safe defaults — what the first run writes into `config.json`.
    ///
    /// It starts **localhost only**, so copying the exe onto someone else's PC and running it
    /// never opens a controllable port on the LAN without a decision. Using it from a
    /// development PC means writing that IP in by hand — and that one line is the permission.
    pub fn starter() -> Config {
        Config {
            host: default_host(),
            port: default_port(),
            allowed_ips_read: vec!["127.0.0.1".into()],
            allowed_ips_write: vec!["127.0.0.1".into()],
            admin_code: String::new(),
            profiles: BTreeMap::new(),
            default_profile: String::new(),
            captures: CaptureConfig::default(),
            allow_raw_clicks: false,
            allow_raw_keys: false,
            allow_profile_editing: false,
            default_settle_ms: default_settle_ms(),
            max_settle_ms: default_max_settle_ms(),
            default_hold_ms: default_hold_ms(),
            max_hold_ms: default_max_hold_ms(),
        }
    }

    pub fn load(path: &Path) -> Result<Config, String> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| format!("failed to read {}: {e}", path.display()))?;
        let cfg: Config = serde_json::from_str(&text)
            .map_err(|e| format!("failed to parse {}: {e}", path.display()))?;
        cfg.validate()?;
        Ok(cfg)
    }

    fn validate(&self) -> Result<(), String> {
        if self.port == 0 {
            return Err("port must not be 0".into());
        }
        if self.allowed_ips_read.is_empty() {
            return Err("allowed_ips_read is empty — nobody could reach this server".into());
        }
        if self.max_settle_ms < self.default_settle_ms {
            return Err("max_settle_ms must be >= default_settle_ms".into());
        }
        if self.max_hold_ms < self.default_hold_ms {
            return Err("max_hold_ms must be >= default_hold_ms".into());
        }
        // The whitelist is a string comparison against the caller's **numeric address**.
        // Writing "localhost" or a hostname never matches, and the symptom — "I configured it
        // and still get 403" — does not even look like a typo. Catch it at startup.
        for (which, list) in [
            ("allowed_ips_read", &self.allowed_ips_read),
            ("allowed_ips_write", &self.allowed_ips_write),
        ] {
            for entry in list {
                if entry != "*" && entry.parse::<std::net::IpAddr>().is_err() {
                    return Err(format!(
                        "{which} contains '{entry}', which is not an IP address. The whitelist is \
                         compared against the caller's numeric address, so names never match — \
                         write 127.0.0.1 instead of localhost."
                    ));
                }
            }
        }
        for name in self.profiles.keys() {
            if !is_safe_profile_name(name) {
                return Err(format!(
                    "profile name '{name}' is invalid — use letters, digits, '-' and '_' only \
                     (the name travels in URLs and capture filenames)"
                ));
            }
        }
        if !self.default_profile.is_empty()
            && !self.profiles.is_empty()
            && !self.profiles.contains_key(&self.default_profile)
        {
            return Err(format!(
                "default_profile '{}' is not one of the defined profiles ({})",
                self.default_profile,
                self.profiles.keys().cloned().collect::<Vec<_>>().join(", ")
            ));
        }
        Ok(())
    }

    /// The effective profile list = **what was found on disk, plus what the config names**.
    ///
    /// Each `profiles/deescreen.<name>.json` becomes the profile of that name. Dropping in a
    /// file is all it takes — requiring a config edit and a restart per application would break
    /// the premise that this tool is not specific to any one program.
    ///
    /// The `profiles` map in `config.json` layers on top (on a name collision, the config
    /// wins). It is needed for files that break the naming rule or live in another folder.
    pub fn effective_profiles(&self) -> BTreeMap<String, String> {
        let mut found = discover_profiles();
        for (name, path) in &self.profiles {
            found.insert(name.clone(), path.clone());
        }
        // Finding none **leaves none.** This used to create an empty placeholder, which was
        // dangerous because it looked exactly like a configured profile (you would press
        // invented coordinates), and profiles are made in the editor anyway. Saying there are
        // none is the honest answer.
        found
    }


    /// Clamp a requested wait into the configured ceiling.
    pub fn clamp_settle(&self, requested: Option<u64>) -> u64 {
        requested.unwrap_or(self.default_settle_ms).min(self.max_settle_ms)
    }

    /// Clamp a requested press length into the configured ceiling.
    pub fn clamp_hold(&self, requested: Option<u64>) -> u64 {
        requested.unwrap_or(self.default_hold_ms).min(self.max_hold_ms)
    }
}

/// Where profile files live — `profiles/` inside the home directory.
pub fn profiles_dir() -> PathBuf {
    in_home("profiles")
}

/// The path of one profile file — `profiles/deescreen.<name>.json`.
pub fn profile_path(name: &str) -> PathBuf {
    profiles_dir().join(format!("deescreen.{name}.json"))
}

/// Discover profiles on disk.
///
/// **`profiles/deescreen.<name>.json` is the proper place.** Drop in a file and a profile
/// exists — requiring a config edit and a restart per application would break the premise that
/// this tool is not specific to any one program.
///
/// None of this widens access. The whitelist is unchanged; this only adds one more list of
/// "what can be pressed", and its contents are decided by whoever can write that file.
///
/// `.bak` and `.tmp` are filtered out naturally, since their extension is not `.json`.
fn discover_profiles() -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();

    // profiles/deescreen.<name>.json
    let dir = profiles_dir();
    if let Ok(entries) = std::fs::read_dir(&dir) {
        for entry in entries.flatten() {
            let Ok(name) = entry.file_name().into_string() else {
                continue;
            };
            if let Some(mid) = name.strip_prefix("deescreen.").and_then(|r| r.strip_suffix(".json"))
                && is_safe_profile_name(mid)
            {
                out.insert(mid.to_string(), format!("profiles/{name}"));
            }
        }
    }
    out
}

/// A profile name travels verbatim in URL queries and capture file names — a path separator or
/// a space breaks each of those in its own way. Narrow it where the name is created.
pub fn is_safe_profile_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 40
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// Settings this build knows about that the file does not mention, filled in with the values
/// already in force.
///
/// `serde(default)` means a missing setting still works — and that is the problem. It works
/// invisibly: the operator opens config.json, does not see `default_hold_ms`, and has no reason
/// to think there is a hold to tune. Every release that adds a setting widens that gap for
/// everyone who upgraded by copying the exe over.
///
/// Nothing is overwritten. Only absent keys are added, and only with what the running config
/// already resolved to, so the file after the write describes exactly the behaviour before it.
///
/// This is only safe because `Config` refuses unknown fields: a file that parsed is fully
/// represented by the struct, so serialising it back cannot lose anything that was in it. Were
/// that not true, this would quietly delete the settings it did not understand.
pub fn fill_missing_keys(path: &Path, cfg: &Config) -> Result<Vec<String>, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("re-read {}: {e}", path.display()))?;
    let on_disk: serde_json::Value =
        serde_json::from_str(&text).map_err(|e| format!("re-parse {}: {e}", path.display()))?;
    let full = serde_json::to_value(cfg).map_err(|e| format!("serialize config: {e}"))?;

    let mut added = Vec::new();
    missing_keys(&on_disk, &full, "", &mut added);
    if added.is_empty() {
        return Ok(added);
    }

    let pretty = serde_json::to_string_pretty(&full).map_err(|e| format!("serialize config: {e}"))?;
    std::fs::write(path, pretty + "\n").map_err(|e| format!("write {}: {e}", path.display()))?;
    Ok(added)
}

/// Keys present in `full` and absent from `on_disk`, one level into nested objects so that
/// `captures.keep` is found rather than only `captures`.
fn missing_keys(on_disk: &serde_json::Value, full: &serde_json::Value, prefix: &str, out: &mut Vec<String>) {
    let (Some(have), Some(want)) = (on_disk.as_object(), full.as_object()) else { return };
    for (k, v) in want {
        match have.get(k) {
            None => out.push(format!("{prefix}{k}")),
            Some(mine) if v.is_object() => missing_keys(mine, v, &format!("{prefix}{k}."), out),
            Some(_) => {}
        }
    }
}

/// Why the runtime files ended up where they did.
///
/// Four answers, and the reason is carried alongside the path because there is now more than
/// one place they could be. A tool that quietly picks a different directory than last time is
/// worse than one that only ever had a single option.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum HomeKind {
    /// `DEESCREEN_HOME` was set. An explicit answer beats every rule below it.
    Env,
    /// A `config.json` already sits beside the exe, so this install is already portable and
    /// stays that way — moving somebody's existing files is not a decision an upgrade makes.
    Portable,
    /// The exe lives in a shared bin directory (`cargo install` puts it there), where writing
    /// config.json, profiles/, captures/ and logs/ would scatter them among other tools.
    UserData,
    /// The ordinary case: the exe was dropped in a folder, and that folder is the install.
    BesideExe,
}

impl HomeKind {
    pub fn as_str(self) -> &'static str {
        match self {
            HomeKind::Env => "DEESCREEN_HOME",
            HomeKind::Portable => "portable (config.json beside the exe)",
            HomeKind::UserData => "user data (the exe is in a shared bin directory)",
            HomeKind::BesideExe => "beside the exe",
        }
    }
}

/// Where `config.json`, `profiles/`, `captures/` and `logs/` live.
#[derive(Clone, Debug)]
pub struct Home {
    pub dir: PathBuf,
    pub kind: HomeKind,
}

/// The decision, with the environment passed in rather than read, so every branch is testable
/// without a filesystem or a real `cargo install`.
pub fn decide_home(
    env_home: Option<&str>,
    exe_dir: Option<&Path>,
    config_beside_exe: bool,
    cargo_home: Option<&str>,
    local_appdata: Option<&str>,
) -> Home {
    if let Some(h) = env_home.map(str::trim).filter(|h| !h.is_empty()) {
        return Home { dir: PathBuf::from(h), kind: HomeKind::Env };
    }
    let Some(exe_dir) = exe_dir else {
        // No exe path is a strange enough state that guessing a data directory would only hide
        // it. The working directory at least fails visibly.
        return Home { dir: PathBuf::from("."), kind: HomeKind::BesideExe };
    };
    if config_beside_exe {
        return Home { dir: exe_dir.to_path_buf(), kind: HomeKind::Portable };
    }
    if is_shared_bin(exe_dir, cargo_home)
        && let Some(base) = local_appdata.map(str::trim).filter(|b| !b.is_empty())
    {
        return Home { dir: Path::new(base).join("deescreen"), kind: HomeKind::UserData };
    }
    Home { dir: exe_dir.to_path_buf(), kind: HomeKind::BesideExe }
}

/// Whether this directory is somewhere a package manager drops binaries, rather than an install
/// of its own. Deliberately narrow: `cargo install` is the case this exists for, and a wrong
/// guess here moves a person's config file.
fn is_shared_bin(exe_dir: &Path, cargo_home: Option<&str>) -> bool {
    if let Some(ch) = cargo_home.map(str::trim).filter(|c| !c.is_empty())
        && exe_dir == Path::new(ch).join("bin")
    {
        return true;
    }
    let mut parts = exe_dir.components().rev().filter_map(|c| c.as_os_str().to_str());
    matches!((parts.next(), parts.next()), (Some("bin"), Some(".cargo")))
}

/// The resolved home, worked out once. Every runtime path hangs off this.
pub fn home() -> &'static Home {
    static HOME: std::sync::OnceLock<Home> = std::sync::OnceLock::new();
    HOME.get_or_init(|| {
        let exe_dir = std::env::current_exe().ok().and_then(|e| e.parent().map(Path::to_path_buf));
        let beside = exe_dir.as_ref().map(|d| d.join("config.json").exists()).unwrap_or(false);
        decide_home(
            std::env::var("DEESCREEN_HOME").ok().as_deref(),
            exe_dir.as_deref(),
            beside,
            std::env::var("CARGO_HOME").ok().as_deref(),
            std::env::var("LOCALAPPDATA").ok().as_deref(),
        )
    })
}

/// Build a path inside the home directory. An absolute setting overrides it outright, which is
/// how `captures.dir` can point at another disk.
pub fn in_home(rel: &str) -> PathBuf {
    let p = Path::new(rel);
    if p.is_absolute() {
        return p.to_path_buf();
    }
    home().dir.join(p)
}



#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn starter_config_round_trips() {
        let text = serde_json::to_string_pretty(&Config::starter()).expect("serialize");
        let back: Config = serde_json::from_str(&text).expect("parse");
        back.validate().expect("starter must be valid");
        assert_eq!(back.allowed_ips_read, vec!["127.0.0.1".to_string()]);
        // named buttons only by default — flip this and the safety boundary is gone
        assert!(!back.allow_raw_clicks);
        assert!(!back.allow_raw_keys);
    }

    /// These files are strict JSON. A `//` comment is a parse error, and it says so.
    ///
    /// This used to strip them, which read nicely in the example file — right up until a save
    /// rewrote a profile through a serialiser and every comment a person had written was gone.
    /// Documentation a save can delete is worse than none, so the format is plain JSON and the
    /// explanations live where nothing overwrites them.
    #[test]
    fn comments_are_not_a_thing() {
        let with_comment = r#"{
            // why this IP
            "allowed_ips_read": ["127.0.0.1"], "allowed_ips_write": []
        }"#;
        assert!(serde_json::from_str::<Config>(with_comment).is_err());

        // And the same text without it is fine — the comment is the only reason it failed.
        let plain = r#"{"allowed_ips_read": ["127.0.0.1"], "allowed_ips_write": []}"#;
        serde_json::from_str::<Config>(plain).expect("plain JSON parses");
    }

    /// Both examples have to be readable by any JSON parser, not just this program's.
    #[test]
    fn the_examples_are_strict_json() {
        for text in [include_str!("../config.example.json"), include_str!("../profile.example.json")] {
            serde_json::from_str::<serde_json::Value>(text).expect("strict JSON");
            assert!(!text.contains("//"), "an example still carries a comment");
        }
    }

    #[test]
    fn hostnames_in_the_whitelist_are_rejected() {
        // "localhost" never matches the caller's numeric address. A quiet 403 does not even
        // look like a typo to whoever wrote the config, so startup itself is blocked.
        let mut c = Config::starter();
        c.allowed_ips_read = vec!["localhost".into()];
        let e = c.validate().expect_err("hostname must be rejected");
        assert!(e.contains("localhost"), "{e}");

        // numeric addresses and the wildcard pass. The IPv6 loopback is an address too.
        c.allowed_ips_read = vec!["127.0.0.1".into(), "::1".into(), "*".into()];
        c.validate().expect("numeric addresses are fine");
    }


    #[test]
    fn empty_read_whitelist_is_rejected() {
        let mut c = Config::starter();
        c.allowed_ips_read.clear();
        assert!(c.validate().is_err());
    }

    /// Upgrading over an old install adds the settings the file does not mention, keeps
    /// everything it does, and touches nothing once the file is complete.
    ///
    /// The keeping half is the one worth a test. This rewrites a file the operator owns, and
    /// it is only safe because `deny_unknown_fields` guarantees a parsed config holds
    /// everything the file held — remove that and this silently deletes whatever this build
    /// does not recognise.
    #[test]
    fn an_old_config_gains_the_settings_it_is_missing() {
        let dir = std::env::temp_dir().join(format!("deescreen-cfg-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join("config.json");

        // A file from before default_hold_ms and captures.max_age_minutes existed, with
        // non-default values that must survive.
        std::fs::write(
            &path,
            r#"{"host":"0.0.0.0","port":9001,"allowed_ips_read":["10.0.0.5"],
                "allowed_ips_write":["10.0.0.5"],"allow_raw_clicks":true,
                "captures":{"dir":"shots","keep":7}}"#,
        )
        .expect("write");

        let cfg = Config::load(&path).expect("an old file still loads");
        let added = fill_missing_keys(&path, &cfg).expect("fills");
        assert!(added.contains(&"default_hold_ms".to_string()), "added: {added:?}");
        assert!(added.contains(&"max_hold_ms".to_string()), "added: {added:?}");
        assert!(
            added.contains(&"captures.max_age_minutes".to_string()),
            "one level in, so a nested setting is found too: {added:?}"
        );

        let after = Config::load(&path).expect("and reloads");
        assert_eq!(after.port, 9001, "an existing value is not overwritten");
        assert_eq!(after.allowed_ips_read, vec!["10.0.0.5".to_string()]);
        assert!(after.allow_raw_clicks);
        assert_eq!(after.captures.dir, "shots");
        assert_eq!(after.captures.keep, 7);
        assert_eq!(after.default_hold_ms, cfg.default_hold_ms);

        // Complete already: nothing to add, and nothing written.
        let before = std::fs::read_to_string(&path).expect("read");
        assert!(fill_missing_keys(&path, &after).expect("second pass").is_empty());
        assert_eq!(std::fs::read_to_string(&path).expect("read"), before, "file untouched");

        std::fs::remove_dir_all(&dir).ok();
    }

    /// Every branch of the home decision, without a filesystem or a real `cargo install`.
    ///
    /// The rules exist in a fixed order and the order is the whole design: an explicit answer
    /// wins, then an install that is already portable stays portable, and only then does the
    /// shared-bin guess get a turn. Reordering any two of those moves somebody's config file.
    #[test]
    fn the_home_rules_apply_in_order() {
        let exe = Path::new("C:/tools/deescreen");
        let cargo_bin = Path::new("C:/Users/x/.cargo/bin");

        // 1. DEESCREEN_HOME wins over everything, including a portable install.
        let h = decide_home(Some("D:/dee"), Some(exe), true, None, Some("C:/AppData"));
        assert_eq!((h.kind, h.dir), (HomeKind::Env, PathBuf::from("D:/dee")));
        // ...and over the shared-bin rule.
        let h = decide_home(Some("D:/dee"), Some(cargo_bin), false, None, Some("C:/AppData"));
        assert_eq!(h.kind, HomeKind::Env);
        // An empty variable is not an answer.
        assert_eq!(decide_home(Some("  "), Some(exe), false, None, None).kind, HomeKind::BesideExe);

        // 2. A config.json already beside the exe keeps that install where it is - even in a
        // bin directory, where somebody clearly put it on purpose.
        let h = decide_home(None, Some(cargo_bin), true, None, Some("C:/AppData"));
        assert_eq!((h.kind, h.dir), (HomeKind::Portable, cargo_bin.to_path_buf()));

        // 3. `cargo install` drops the exe in a shared bin, so the files go to user data.
        let h = decide_home(None, Some(cargo_bin), false, None, Some("C:/AppData"));
        assert_eq!((h.kind, h.dir), (HomeKind::UserData, PathBuf::from("C:/AppData/deescreen")));
        // CARGO_HOME finds a bin directory that is not named .cargo.
        let odd = Path::new("D:/rust/bin");
        let h = decide_home(None, Some(odd), false, Some("D:/rust"), Some("C:/AppData"));
        assert_eq!(h.kind, HomeKind::UserData);
        // With nowhere to put user data, beside the exe beats inventing a path.
        assert_eq!(decide_home(None, Some(cargo_bin), false, None, None).kind, HomeKind::BesideExe);

        // 4. The ordinary case, unchanged: the folder the exe was dropped into.
        let h = decide_home(None, Some(exe), false, None, Some("C:/AppData"));
        assert_eq!((h.kind, h.dir), (HomeKind::BesideExe, exe.to_path_buf()));
        // A folder that merely ends in "bin" is not a package manager's.
        let mine = Path::new("C:/work/bin");
        assert_eq!(decide_home(None, Some(mine), false, None, Some("C:/A")).kind, HomeKind::BesideExe);
    }

    /// Whether `config.example.json` really parses into a `Config`. Edit the example without
    /// the struct following and this fails — schema drift, blocked.
    #[test]
    fn config_example_parses() {
        let cfg: Config = serde_json::from_str(include_str!("../config.example.json")).expect("parse example");
        cfg.validate().expect("example must be valid");
        // also pin that the example has not flipped the safe defaults
        assert!(!cfg.allow_raw_clicks);
        assert!(!cfg.allow_raw_keys);
    }

    #[test]
    fn explicit_profiles_win_over_discovery() {
        // what the config names overrides what discovery found — needed for files that break
        // the naming rule, or that live in another folder
        let mut c = Config::starter();
        c.profiles.insert("ncguide".into(), "custom/place.json".into());
        let ps = c.effective_profiles();
        assert_eq!(ps.get("ncguide").map(String::as_str), Some("custom/place.json"));
    }

    #[test]
    fn default_profile_must_name_a_real_profile() {
        let mut c = Config::starter();
        c.profiles.insert("a".into(), "targets.a.json".into());
        c.default_profile = "nope".into();
        let err = c.validate().expect_err("dangling default must be rejected");
        assert!(err.contains("nope"), "{err}");
    }

    #[test]
    fn profile_names_are_restricted() {
        assert!(is_safe_profile_name("ncguide"));
        assert!(is_safe_profile_name("nc-trainer_2"));
        // these travel in URLs and file names, so they are blocked where the name is made
        assert!(!is_safe_profile_name("nc guide"));
        assert!(!is_safe_profile_name("../etc"));
        assert!(!is_safe_profile_name(""));
    }

    #[test]
    fn settle_is_clamped_to_max() {
        let c = Config::starter();
        assert_eq!(c.clamp_settle(None), c.default_settle_ms);
        assert_eq!(c.clamp_settle(Some(999_999)), c.max_settle_ms);
        assert_eq!(c.clamp_settle(Some(50)), 50);
    }

    /// The hold has the same shape as the settle, and one thing the settle does not: it must
    /// never come back as zero from the default path. Zero is not a short press, it is the
    /// press-and-release-in-one-tick that a scanned machine key cannot see at all.
    #[test]
    fn hold_is_clamped_and_never_defaults_to_nothing() {
        let c = Config::starter();
        assert_eq!(c.clamp_hold(None), c.default_hold_ms);
        assert!(c.default_hold_ms > 0, "a zero default is the bug this exists to prevent");
        assert_eq!(c.clamp_hold(Some(999_999)), c.max_hold_ms);
        assert_eq!(c.clamp_hold(Some(250)), 250);

        // A config that asks for a ceiling below its own default is refused rather than
        // silently clamping every press to the lower number.
        let mut bad = Config::starter();
        bad.max_hold_ms = 10;
        assert!(bad.validate().is_err());
    }
}
