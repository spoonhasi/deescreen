//! The system tray icon — for interactive runs only.
//!
//! The tray is created on a dedicated OS thread that runs a Win32 message loop (the tray only
//! receives menu and click messages if that thread pumps them). Failing to create it is not
//! fatal — it warns and the server carries on.
//!
//! The icon is drawn in code rather than loaded from a file. This is a tool you use by
//! copying one exe, so having nothing alongside it is worth a few lines of drawing.

use std::sync::Arc;
use std::sync::mpsc;
use std::thread::JoinHandle;

use tray_icon::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tray_icon::{Icon, TrayIconBuilder, TrayIconEvent};
use windows_sys::Win32::System::Threading::GetCurrentThreadId;
use windows_sys::Win32::UI::Shell::ShellExecuteW;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    DispatchMessageW, GetMessageW, MSG, PostThreadMessageW, SW_SHOWNORMAL, TranslateMessage, WM_QUIT,
};

pub struct TrayHandle {
    thread_id: u32,
    join: Option<JoinHandle<()>>,
}

impl TrayHandle {
    /// Post `WM_QUIT` to the message loop to end the thread (→ `TrayIcon` drop → icon removed).
    pub fn shutdown(mut self) {
        if self.thread_id != 0 {
            unsafe { PostThreadMessageW(self.thread_id, WM_QUIT, 0, 0) };
        }
        if let Some(j) = self.join.take() {
            let _ = j.join();
        }
    }
}

pub fn spawn(base_url: String, quit: Arc<tokio::sync::Notify>) -> TrayHandle {
    if !crate::win::is_interactive_session() {
        log::info!("non-interactive session — system tray icon skipped");
        return TrayHandle { thread_id: 0, join: None };
    }
    let (tx, rx) = mpsc::channel::<u32>();
    let join = match std::thread::Builder::new().name("tray".into()).spawn(move || {
        let _ = tx.send(unsafe { GetCurrentThreadId() });
        if let Err(e) = run(base_url, quit) {
            log::warn!("system tray icon disabled: {e}");
        }
    }) {
        Ok(j) => j,
        Err(e) => {
            log::warn!("failed to spawn tray thread: {e}");
            return TrayHandle { thread_id: 0, join: None };
        }
    };
    let thread_id = rx.recv().unwrap_or(0);
    TrayHandle { thread_id, join: Some(join) }
}

fn run(base_url: String, quit: Arc<tokio::sync::Notify>) -> Result<(), Box<dyn std::error::Error>> {
    let icon = Icon::from_rgba(icon_rgba(), 32, 32)?;

    let menu = Menu::new();
    let version = MenuItem::new(format!("deescreen v{}", env!("CARGO_PKG_VERSION")), false, None);
    let url_label = MenuItem::new(base_url.clone(), false, None);
    // The editor is the practical way in — it is where buttons get drawn and checked, so it
    // goes at the top.
    let open_editor = MenuItem::new("Open button editor", true, None);
    let open_health = MenuItem::new("Status (/health)", true, None);
    let open_folder = MenuItem::new("Open settings folder", true, None);
    let quit_item = MenuItem::new("Quit", true, None);
    menu.append(&version)?;
    menu.append(&url_label)?;
    menu.append(&PredefinedMenuItem::separator())?;
    menu.append(&open_editor)?;
    menu.append(&open_health)?;
    menu.append(&open_folder)?;
    menu.append(&PredefinedMenuItem::separator())?;
    menu.append(&quit_item)?;

    let _tray = TrayIconBuilder::new()
        .with_tooltip(format!("deescreen v{} · {base_url}", env!("CARGO_PKG_VERSION")))
        .with_menu(Box::new(menu))
        .with_icon(icon)
        .build()?;

    let menu_rx = MenuEvent::receiver();
    let tray_rx = TrayIconEvent::receiver();
    let editor = format!("{base_url}/editor");
    let health = format!("{base_url}/health");
    // Not the exe's folder — with `cargo install` those are two different places, and the
    // one a person wants is where config.json actually is.
    let folder = crate::config::home().dir.to_string_lossy().to_string();

    let mut msg: MSG = unsafe { std::mem::zeroed() };
    'pump: loop {
        let r = unsafe { GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) };
        if r <= 0 {
            break; // 0 = WM_QUIT, -1 = error
        }
        unsafe {
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
        while let Ok(ev) = menu_rx.try_recv() {
            if ev.id == *open_editor.id() {
                open_url(&editor);
            } else if ev.id == *open_health.id() {
                open_url(&health);
            } else if ev.id == *open_folder.id() {
                // Where config.json, profiles/ and logs/ live. On a deployment with editor
                // saving turned off, this is the only way to edit anything.
                open_url(&folder);
            } else if ev.id == *quit_item.id() {
                // notify_one — the permit persists even with nobody waiting yet, so the
                // server loop wakes immediately whenever it does wait (notify_waiters would
                // be lost).
                quit.notify_one();
                break 'pump;
            }
        }
        while let Ok(ev) = tray_rx.try_recv() {
            if let TrayIconEvent::DoubleClick { .. } = ev {
                open_url(&editor);
            }
        }
    }
    Ok(())
}

fn open_url(url: &str) {
    let file: Vec<u16> = url.encode_utf16().chain(std::iter::once(0)).collect();
    let verb: Vec<u16> = "open".encode_utf16().chain(std::iter::once(0)).collect();
    unsafe {
        ShellExecuteW(
            std::ptr::null_mut(),
            verb.as_ptr(),
            file.as_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            SW_SHOWNORMAL,
        );
    }
}

/// Draw the 32x32 RGBA icon — a mouse cursor over a dark screen.
///
/// Public because the web side serves the same drawing as the page's favicon: the tray and the
/// browser tab should not be two different pictures of the same tool.
pub fn icon_rgba() -> Vec<u8> {
    const N: usize = 32;
    let mut px = vec![0u8; N * N * 4];
    let mut set = |x: usize, y: usize, c: [u8; 4]| {
        if x < N && y < N {
            let i = (y * N + x) * 4;
            px[i..i + 4].copy_from_slice(&c);
        }
    };

    let frame = [0x1e, 0x29, 0x3b, 0xff]; // dark slate — the monitor body
    let screen = [0x38, 0xbd, 0xf8, 0xff]; // sky blue — the screen
    let cursor = [0xff, 0xff, 0xff, 0xff];

    // the body, with the corners cut
    for y in 3..25 {
        for x in 2..30 {
            let corner = !(4..=27).contains(&x) && !(5..=22).contains(&y);
            if !corner {
                set(x, y, frame);
            }
        }
    }
    // the screen
    for y in 6..21 {
        for x in 5..27 {
            set(x, y, screen);
        }
    }
    // the stand
    for y in 25..28 {
        for x in 13..19 {
            set(x, y, frame);
        }
    }
    for x in 8..24 {
        set(x, 28, frame);
        set(x, 29, frame);
    }
    // the cursor arrow, over the screen
    for (i, y) in (11..22).enumerate() {
        let w = (i / 2 + 1).min(7);
        for x in 15..15 + w {
            set(x, y, cursor);
        }
    }

    px
}

#[cfg(test)]
mod tests {
    #[test]
    fn icon_is_exactly_32x32_rgba() {
        // Icon::from_rgba checks the length — get it wrong and the tray quietly disappears
        assert_eq!(super::icon_rgba().len(), 32 * 32 * 4);
    }
}
