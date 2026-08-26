//! The Win32 layer — finding windows, capturing the screen, injecting input.
//!
//! **No HWND leaves this layer as a pointer.** `HWND` is a `*mut c_void`, which is not `Send`
//! and so cannot cross a tokio task boundary. Handles travel as `isize` and are turned back
//! into pointers only in here (the `hwnd()` helper).
//!
//! ## Coordinates — there is exactly one system
//!
//! **Every coordinate is a physical pixel inside the target window's client area.**
//! - client-relative: moving the window does not break a button
//! - physical pixels: the process is per-monitor DPI aware, so `GetClientRect` already speaks
//!   pixels and they match a captured PNG one to one
//!
//! An absolute screen coordinate is produced exactly once, immediately before a click
//! (`client_origin + the button's coordinate`), and is never stored anywhere.

pub mod capture;
pub mod input;
pub mod window;

use std::ffi::c_void;

use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, HWND};
use windows_sys::Win32::Security::{GetTokenInformation, TOKEN_ELEVATION, TOKEN_QUERY, TokenElevation};
use windows_sys::Win32::System::StationsAndDesktops::{
    GetProcessWindowStation, GetUserObjectInformationW, UOI_FLAGS, USEROBJECTFLAGS,
};
use windows_sys::Win32::System::Threading::{
    GetCurrentProcess, OpenProcess, OpenProcessToken, PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows_sys::Win32::UI::HiDpi::{
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext,
};

/// Turn a window handle carried as an `isize` back into a Win32 pointer.
#[inline]
pub(crate) fn hwnd(h: isize) -> HWND {
    h as HWND
}

/// **Call this on the first line of main(),** before any other Win32 call.
///
/// Without it, at a display scale of 125% or 150% the OS quietly converts coordinates —
/// `GetClientRect` hands back logical pixels while the captured image is physical pixels, so
/// **the capture's pixels and the click's coordinates end up in different systems**. No error
/// is raised; it simply presses the wrong place. This is the trap the README calls
/// the first thing to get right.
///
/// A failure is warned about but not fatal — being already declared aware by a manifest or by
/// system policy also fails (`ERROR_ACCESS_DENIED`), so the failure alone means nothing. Which
/// system it is actually running in is reported by `dpi` in `/health`.
pub fn init_dpi_awareness() -> bool {
    let ok = unsafe { SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };
    ok != 0
}

/// Elevation state — for judging UIPI.
///
/// UIPI: input sent from a lower-integrity process into a higher-integrity window is
/// **silently discarded**. `SendInput` returns success and `GetLastError` says nothing about
/// why (MSDN states this outright). Detecting it after the fact is therefore impossible, and
/// comparing the two elevation states up front is the only warning available.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Elevation {
    Normal,
    Elevated,
    /// The query itself was refused. Typical when we are unelevated and the target is not.
    Unknown,
}

impl Elevation {
    pub fn as_str(self) -> &'static str {
        match self {
            Elevation::Normal => "normal",
            Elevation::Elevated => "elevated",
            Elevation::Unknown => "unknown",
        }
    }
}

/// Whether this process is elevated (running as administrator).
pub fn own_elevation() -> Elevation {
    unsafe { elevation_of_process(GetCurrentProcess(), false) }
}

/// Whether the process with this pid is elevated. `Unknown` if the handle cannot be opened.
pub fn process_elevation(pid: u32) -> Elevation {
    unsafe {
        let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if h.is_null() {
            return Elevation::Unknown;
        }
        elevation_of_process(h, true)
    }
}

unsafe fn elevation_of_process(proc: HANDLE, close_proc: bool) -> Elevation {
    let mut token: HANDLE = std::ptr::null_mut();
    let opened = unsafe { OpenProcessToken(proc, TOKEN_QUERY, &mut token) } != 0;
    let result = if opened {
        let mut elev = TOKEN_ELEVATION { TokenIsElevated: 0 };
        let mut ret_len = 0u32;
        let ok = unsafe {
            GetTokenInformation(
                token,
                TokenElevation,
                (&raw mut elev) as *mut c_void,
                size_of::<TOKEN_ELEVATION>() as u32,
                &mut ret_len,
            )
        } != 0;
        unsafe { CloseHandle(token) };
        if ok {
            if elev.TokenIsElevated != 0 { Elevation::Elevated } else { Elevation::Normal }
        } else {
            Elevation::Unknown
        }
    } else {
        Elevation::Unknown
    };
    if close_proc {
        unsafe { CloseHandle(proc) };
    }
    result
}

/// Whether this runs in an interactive session (a visible desktop).
///
/// Started as a service (Session 0) this returns `false`, and in that environment **captures
/// are black and input does nothing**. The tray icon does not appear either. This tool has to
/// run in a logged-in console session.
pub fn is_interactive_session() -> bool {
    const WSF_VISIBLE: u32 = 0x0001;
    unsafe {
        let hwinsta = GetProcessWindowStation();
        if hwinsta.is_null() {
            return true; // cannot tell — carry on as normal
        }
        let mut flags = USEROBJECTFLAGS {
            fInherit: 0,
            fReserved: 0,
            dwFlags: 0,
        };
        let mut needed = 0u32;
        let ok = GetUserObjectInformationW(
            hwinsta as HANDLE,
            UOI_FLAGS,
            (&raw mut flags) as *mut c_void,
            size_of::<USEROBJECTFLAGS>() as u32,
            &mut needed,
        );
        if ok == 0 {
            return true;
        }
        flags.dwFlags & WSF_VISIBLE != 0
    }
}
