//! deescreen — capture and click a GUI window on a remote PC, over HTTP.
//!
//! ⚠️ **Do not put this on a real equipment HMI.** Driving a simulator and driving a machine
//! are not the same activity. The first section of the README has the rest.

// A release build starts with no console window — on the target PC this lives in the tray;
// it is not a program that keeps a command window open. A debug build keeps the console
// (during development, seeing the log immediately is better).
//
// With no console, a failed startup looks like **nothing happening at all**, so `fatal()`
// puts the reason in a message box. Without that, double-clicking the exe and getting no
// response is all the user would ever see.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

// Win32 only — it captures with `PrintWindow` and presses with `SendInput`. Porting means
// rewriting all of `src/win/`, so the build refuses rather than failing quietly later.
#[cfg(not(windows))]
compile_error!("deescreen is Windows-only (Win32 PrintWindow + SendInput).");

mod acl;
mod api;
mod captures;
mod config;
mod draw;
mod logging;
mod router;
mod sheet;
mod state;
mod targets;
/// The 32x32 icon, as raw RGBA.
///
/// Its own file because two things read it: this program, which hands it to the tray and
/// serves it as the page's favicon, and `build.rs`, which turns it into the icon on the exe.
/// A build script cannot call into the crate it is building, so it `include!`s the file —
/// unusual, and the reason there is one drawing rather than a copy that drifts. That is also
/// why the file carries no `//!` doc comment and depends on nothing but `std`.
mod icon;
mod tray;
mod web;
mod win;

use std::net::SocketAddr;
use std::sync::Arc;

use arc_swap::ArcSwap;
use log::{error, info, warn};

use crate::config::{Config, in_home};
use crate::state::AppState;

#[tokio::main]
async fn main() {
    // ── before anything else ──
    // A single Win32 call ahead of this makes the declaration ineffective. Ineffective means
    // that at 125% or 150% display scale, capture pixels and click coordinates end up in
    // different systems — and that mismatch surfaces only as "it pressed the wrong place",
    // never as an error.
    let dpi_aware = win::init_dpi_awareness();

    // Everything below lives in here, so make it before anything tries to write. This used
    // to happen by accident — logging created <home>/logs, which created <home> on the way —
    // and a side effect of an unrelated call is not something the config file should need.
    let home = config::home();
    if let Err(e) = std::fs::create_dir_all(&home.dir) {
        fatal(&format!(
            "could not create the settings folder {} ({}): {e}",
            home.dir.display(),
            home.kind.as_str()
        ));
    }

    let logs_dir = in_home("logs");
    logging::init(logs_dir.clone());

    // ── config ──
    let config_path = in_home("config.json");
    if !config_path.exists() {
        match serde_json::to_string_pretty(&Config::starter())
            .map_err(|e| e.to_string())
            .and_then(|s| std::fs::write(&config_path, s).map_err(|e| e.to_string()))
        {
            // WARN rather than INFO: this line appearing means **there was no config at
            // that path**, and if one existed before, it is a sign the exe was started from a
            // different folder. That confusion really happened, and a quiet INFO does not
            // catch the eye.
            Ok(()) => warn!(
                "no config.json at {} — created a starter one. Access is LOCALHOST ONLY until you edit allowed_ips_* and restart. If you expected an existing config, this run picked that folder because: {}. Landing somewhere different from last time means one of those inputs changed — DEESCREEN_HOME, or where the exe is being run from.",
                config_path.display(),
                home.kind.as_str()
            ),
            Err(e) => fatal(&format!(
                "no config.json and could not create one at {}: {e}",
                config_path.display()
            )),
        }
    }
    let config = match Config::load(&config_path) {
        Ok(c) => {
            // An install upgraded by copying the exe over has a config file older than the
            // build. Put the settings it is missing into it, at the values already in force,
            // so the file describes what the program does rather than a subset of it.
            match config::fill_missing_keys(&config_path, &c) {
                Ok(added) if !added.is_empty() => info!(
                    "config.json was missing {} setting(s) this build knows about — added at their current values: {}",
                    added.len(),
                    added.join(", ")
                ),
                Ok(_) => {}
                // Not fatal. The settings are in force either way; only the file is behind.
                Err(e) => warn!("could not bring config.json up to date ({e}) — running with the defaults for anything it does not mention"),
            }
            Arc::new(c)
        }
        Err(e) => fatal(&e),
    };

    // The single-instance guard is **per port**. Driving two applications on one PC (say NC
    // Guide on 8090 and NC Trainer on 8091) needs two instances, and locking on one global
    // name would stop the second from starting at all — a problem actually hit in practice.
    // The real collision is two instances claiming one port, and the name below blocks that.
    if !claim_single_instance(config.port) {
        fatal(&format!(
            "another deescreen instance is already using port {}.\n\nTo drive a second application on this PC, copy the exe into its own folder and give it a different port in config.json.",
            config.port
        ));
    }

    // ── profiles ──
    // Create the folder first, even empty. Where files go should be evident from the folder
    // existing (make people read documentation to create it and nobody creates it).
    let pdir = config::profiles_dir();
    if let Err(e) = std::fs::create_dir_all(&pdir) {
        warn!("could not create {}: {e}", pdir.display());
    }

    // Each profiles/deescreen.<name>.json in the home directory simply is that profile. The
    // config's `profiles` map layers on top. Drop in a file and you have one more application.
    let (profiles, notes) = state::build_profiles(&config);
    for n in &notes {
        warn!("{n}");
    }

    // Starting with no profiles at all is fine. Having none is not a fault but the normal
    // state of "not made yet", and where you make one is the /editor this very server serves.
    // Stopping here would mean you could not get in to make one.
    //
    // But if the config **named** a default profile, it does not start without it — the name
    // disappearing means the file broke or was renamed, and starting anyway would send every
    // request that omits the profile to the wrong one. With none named, the omit rule is
    // decided by `AppState::effective_default` (omitting works only with exactly one profile).
    if !config.default_profile.is_empty() && !profiles.contains_key(&config.default_profile) {
        fatal(&format!(
            "default_profile '{}' did not load. Available: {}",
            config.default_profile,
            if profiles.is_empty() {
                "(none)".to_string()
            } else {
                profiles.keys().cloned().collect::<Vec<_>>().join(", ")
            }
        ));
    }

    // ── state ──
    let captures_dir = in_home(&config.captures.dir);
    if let Err(e) = std::fs::create_dir_all(&captures_dir) {
        warn!("could not create capture dir {}: {e}", captures_dir.display());
    }
    let quit = Arc::new(tokio::sync::Notify::new());
    let state = Arc::new(AppState {
        config: config.clone(),
        profiles: ArcSwap::from(Arc::new(profiles)),
        default_profile: config.default_profile.clone(),
        captures_dir: captures_dir.clone(),
        input_lock: Arc::new(tokio::sync::Mutex::new(())),
        dpi_aware,
        load_notes: ArcSwap::from(Arc::new(notes.clone())),
    });

    // ── startup diagnostics ──
    // Warning loudly here matters. Every one of the conditions below is the "works wrongly
    // without an error" kind, and left unsaid at startup they cost a long search later.
    if !dpi_aware {
        warn!(
            "per-monitor DPI awareness could not be set. If the display scale is not 100%, \
             capture pixels and click coordinates may disagree — verify with GET /health."
        );
    }
    if !win::is_interactive_session() {
        warn!(
            "this process is not in an interactive desktop session. Capture will be black and \
             input will be ignored. Run deescreen as the logged-in user, not as a service."
        );
    }
    if win::own_elevation() != win::Elevation::Elevated {
        info!(
            "running unelevated — if the target application runs as administrator, Windows UIPI \
             will silently discard our input. GET /health reports this as input.uipi_risk."
        );
    }
    if config.allowed_ips_read.iter().any(|ip| ip == "*") || config.allowed_ips_write.iter().any(|ip| ip == "*") {
        warn!(
            "an IP whitelist contains \"*\" — anyone who can reach this port can drive the \
             target window. Narrow allowed_ips_* in config.json."
        );
    }
    // The tray menu opens this server at `127.0.0.1` on this PC. Drop localhost from the
    // whitelist and those links return 403, and all the browser shows is an "access denied"
    // JSON, which does not point anywhere useful. Say it at startup instead.
    if !config.allowed_ips_read.iter().any(|ip| ip == "127.0.0.1" || ip == "*") {
        warn!(
            "127.0.0.1 is not in allowed_ips_read — the tray menu (editor / health) will get \
             403 on this PC. Loopback is NOT allowed automatically; it has to be listed. \
             Add it if you want to use the tray links locally."
        );
    }
    // The editor opens via the read list, but **saving goes through the control list**
    // (/admin/profile). When the two disagree, the editor comes up fine, drawing works, and
    // only [Save] returns 403 — the hardest combination to guess at, so it is said up front.
    if config.allow_profile_editing
        && !config.allowed_ips_write.iter().any(|ip| ip == "127.0.0.1" || ip == "*")
    {
        warn!(
            "profile editing is on, but 127.0.0.1 is not in allowed_ips_write — the editor will \
             open on this PC and refuse to SAVE (saving goes through the control whitelist)."
        );
    }
    if config.allow_raw_clicks || config.allow_raw_keys {
        warn!(
            "raw input is enabled (allow_raw_clicks={}, allow_raw_keys={}) — callers can act \
             outside the named targets. Turn it back off once coordinates are measured.",
            config.allow_raw_clicks, config.allow_raw_keys
        );
    }

    // ── listener ──
    let addr: SocketAddr = match format!("{}:{}", config.host, config.port).parse() {
        Ok(a) => a,
        Err(e) => fatal(&format!("invalid host/port {}:{} — {e}", config.host, config.port)),
    };
    let listener = match tokio::net::TcpListener::bind(addr).await {
        Ok(l) => l,
        Err(e) => fatal(&format!(
            "failed to bind {addr}: {e}\n\nAnother program may already be using port {}. \
             Change \"port\" in config.json.",
            config.port
        )),
    };
    let base_url = format!(
        "http://{}:{}",
        if config.host == "0.0.0.0" { "127.0.0.1" } else { &config.host },
        config.port
    );
    info!("deescreen v{} listening on {addr}", env!("CARGO_PKG_VERSION"));
    info!("  home     {}  ({})", config::home().dir.display(), config::home().kind.as_str());
    info!("  config   {}", config_path.display());
    info!("  log      {}", logging::today_path(&logs_dir).display());
    if state.profiles.load().is_empty() {
        info!("  profile  (none yet — create one at {base_url}/editor)");
    }
    for (name, prof) in state.profiles.load().iter() {
        let mark = if Some(name) == state.effective_default().as_ref() { " (default)" } else { "" };
        info!("  profile  {name}{mark}  {}", prof.path.display());
    }
    info!("  captures {}", captures_dir.display());
    info!("  read     {:?}", config.allowed_ips_read);
    info!("  control  {:?}", config.allowed_ips_write);

    let tray = tray::spawn(base_url, quit.clone());

    let app = router::build(state);
    let serve = axum::serve(listener, app.into_make_service_with_connect_info::<SocketAddr>())
        .with_graceful_shutdown(async move {
            tokio::select! {
                _ = tokio::signal::ctrl_c() => info!("Ctrl+C — shutting down"),
                _ = quit.notified() => info!("tray quit — shutting down"),
            }
        });
    if let Err(e) = serve.await {
        error!("server error: {e}");
    }
    tray.shutdown();
    info!("stopped");
}

/// A failure with nowhere to go — log it, show a message box when interactive, and exit.
///
/// A release build has no console, so `eprintln!` goes nowhere. One typo in a config file
/// leaving the exe in a "click it and nothing happens" state gives no way to find the cause,
/// so at least startup failures reach the screen. (Not under a service or Session 0, where a
/// modal would appear somewhere nobody can see and hold the process open.)
fn fatal(msg: &str) -> ! {
    use windows_sys::Win32::UI::WindowsAndMessaging::{MB_ICONERROR, MB_OK, MessageBoxW};

    error!("{msg}");
    if win::is_interactive_session() {
        let logs = crate::config::in_home("logs");
        let text: Vec<u16> = format!("{msg}\n\nSee today's file in {} for details.", logs.display())
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect();
        let caption: Vec<u16> = "deescreen".encode_utf16().chain(std::iter::once(0)).collect();
        unsafe {
            MessageBoxW(std::ptr::null_mut(), text.as_ptr(), caption.as_ptr(), MB_OK | MB_ICONERROR);
        }
    }
    std::process::exit(1);
}

/// The single-instance guard — **per port**. Two instances claiming one port would fail at
/// bind time anyway, but that error ("address already in use") explains very little.
///
/// The port is in the name because driving different applications on one PC needs several
/// instances (each application is its own coordinate universe, with its own profile file).
/// Locking on one global name would prevent exactly that.
fn claim_single_instance(port: u16) -> bool {
    use windows_sys::Win32::Foundation::{ERROR_ALREADY_EXISTS, GetLastError};
    use windows_sys::Win32::System::Threading::CreateMutexW;

    let name: Vec<u16> = format!("deescreen-singleton-{port}\0").encode_utf16().collect();
    let handle = unsafe { CreateMutexW(std::ptr::null(), 1, name.as_ptr()) };
    if handle.is_null() {
        return true; // cannot tell — do not block
    }
    // The handle is deliberately not closed — the mutex has to stay held for as long as this
    // process lives, and the OS releases it when the process ends.
    let last = unsafe { GetLastError() };
    last != ERROR_ALREADY_EXISTS
}
