//! Finding windows, the client coordinate system, focus and fitting.

use std::ffi::c_void;

use windows_sys::Win32::Foundation::{HWND, LPARAM, POINT, RECT};
use windows_sys::Win32::Graphics::Gdi::{ClientToScreen, ScreenToClient};
use windows_sys::Win32::UI::HiDpi::{AdjustWindowRectExForDpi, GetDpiForWindow};
// IsWindowEnabled lives here rather than in WindowsAndMessaging (same place as SetFocus).
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{IsWindowEnabled, SetFocus};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    BringWindowToTop, CWP_SKIPINVISIBLE, ChildWindowFromPointEx,
    EnumChildWindows, EnumWindows, GA_ROOT, GWL_EXSTYLE, GWL_STYLE, GetAncestor, GetClassNameW,
    GetClientRect, GetDlgCtrlID, GetForegroundWindow, GetParent, GetWindowLongW, GetWindowRect,
    GetWindowTextW, GetWindowThreadProcessId, IsIconic, IsWindow, IsWindowVisible,
    SW_RESTORE, SWP_NOMOVE, SWP_NOOWNERZORDER, SWP_NOZORDER, SetForegroundWindow, SetWindowPos,
    ShowWindow, WINDOW_EX_STYLE, WINDOW_STYLE,
};
use windows_sys::Win32::System::Threading::{AttachThreadInput, GetCurrentThreadId};

use super::hwnd;

/// A snapshot of one window. The HWND is carried as an `isize` so it can cross thread
/// boundaries.
#[derive(Clone, Debug)]
pub struct WindowInfo {
    pub handle: isize,
    pub title: String,
    pub class: String,
    pub pid: u32,
    /// The whole window including its frame, in screen coordinates — diagnostic only.
    /// Nothing to do with button coordinates.
    pub window_rect: (i32, i32, i32, i32),
    /// The **screen** coordinate of the client area's top-left. The origin used to lift a
    /// button coordinate into screen space.
    pub client_origin: (i32, i32),
    /// Client area size in physical pixels — the canvas the button coordinates live on.
    pub client_size: (i32, i32),
    pub dpi: u32,
    pub minimized: bool,
}

/// How to find the window. Like the coordinates, **the config file decides this and a
/// request cannot change it**.
#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub struct WindowSpec {
    /// The window title. A case-insensitive substring match by default — plenty of
    /// applications append the open file name.
    #[serde(default)]
    pub title: String,
    /// `true` requires the whole title to match.
    #[serde(default)]
    pub title_exact: bool,
    /// The window class name (optional). For narrowing down when several share a title.
    #[serde(default)]
    pub class: String,
    /// **Controls the window must contain** — for telling apart windows that title and class
    /// cannot.
    ///
    /// One program that opens different projects shows the same title and class for all of
    /// them: NCGuide's 0i lathe, 0i mill and 30i are all `FANUC NCGuide`. What differs is the
    /// panel layout inside. Without this, a profile measured on one project binds whichever is
    /// running and presses its coordinates onto it — checked live, with no warning.
    ///
    /// Anchors read the same thing, but declaring one makes every button say which anchor it
    /// belongs to. This is only the identity half: checked while the window is being chosen,
    /// and nothing else. A window lacking any of these is not this profile's window.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub has: Vec<ControlMark>,
}

/// One control a window has to contain — see [`WindowSpec::has`].
///
/// Text **and** size, like an anchor, and for the same reason: the text is usually shared.
/// Every NCGuide project has a control called `Main Panel`; they differ in how big it is. A
/// mark with only a text would match every one of them and look like it was doing its job.
/// So the size is required, and copied from `GET /controls` like the text.
#[derive(Clone, Debug, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub struct ControlMark {
    /// The control's window text, as `GET /controls` reports it.
    pub text: String,
    /// Its `[w, h]` — the last two numbers of that control's `rect`.
    pub size: [i32; 2],
}

#[derive(Debug)]
pub enum FindError {
    /// Nothing matched.
    NotFound,
    /// Several matched — **nothing is picked.** Clicking while it is unclear which window is
    /// being driven is exactly the accident that naming buttons exists to prevent.
    Ambiguous(Vec<String>),
    /// Windows with that title and class are open, and none contains the controls `has`
    /// names. Kept apart from `NotFound` because the fix is the opposite one: the window is
    /// right there, it is a different project — or the same one at a different size.
    Unmarked(Vec<String>),
}

/// Enumerate every visible top-level window. The source of the `/windows` endpoint — this is
/// how you find the target application's title when you do not know it.
pub fn enumerate() -> Vec<WindowInfo> {
    let mut out: Vec<WindowInfo> = Vec::new();
    unsafe {
        EnumWindows(Some(enum_proc), (&raw mut out) as LPARAM);
    }
    out
}

unsafe extern "system" fn enum_proc(h: HWND, lparam: LPARAM) -> i32 {
    let out = unsafe { &mut *(lparam as *mut Vec<WindowInfo>) };
    if unsafe { IsWindowVisible(h) } == 0 {
        return 1;
    }
    let title = unsafe { text_of(h, GetWindowTextW) };
    if title.is_empty() {
        return 1; // untitled windows are tool or hidden windows — listing them helps nobody
    }
    if let Some(info) = describe(h as isize) {
        out.push(info);
    }
    1
}

/// Inspect one window into a `WindowInfo`. `None` if the handle is already gone.
pub fn describe(handle: isize) -> Option<WindowInfo> {
    let h = hwnd(handle);
    unsafe {
        if IsWindow(h) == 0 {
            return None;
        }
        let title = text_of(h, GetWindowTextW);
        let class = text_of(h, GetClassNameW);

        let mut wr = RECT { left: 0, top: 0, right: 0, bottom: 0 };
        GetWindowRect(h, &mut wr);

        let mut cr = RECT { left: 0, top: 0, right: 0, bottom: 0 };
        GetClientRect(h, &mut cr);

        let mut origin = POINT { x: 0, y: 0 };
        ClientToScreen(h, &mut origin);

        let mut pid = 0u32;
        GetWindowThreadProcessId(h, &mut pid);

        Some(WindowInfo {
            handle,
            title,
            class,
            pid,
            window_rect: (wr.left, wr.top, wr.right - wr.left, wr.bottom - wr.top),
            client_origin: (origin.x, origin.y),
            client_size: (cr.right - cr.left, cr.bottom - cr.top),
            dpi: GetDpiForWindow(h),
            minimized: IsIconic(h) != 0,
        })
    }
}

/// Which of `marks` are absent from `controls`. Text trimmed and compared exactly, size
/// compared exactly — the same rule an anchor uses to find its control.
pub fn marks_missing_from(controls: &[ControlInfo], marks: &[ControlMark]) -> Vec<ControlMark> {
    marks
        .iter()
        .filter(|m| {
            !controls
                .iter()
                .any(|c| c.text.trim() == m.text.trim() && [c.rect[2], c.rect[3]] == m.size)
        })
        .cloned()
        .collect()
}

/// Find the window the configured spec describes.
pub fn find(spec: &WindowSpec) -> Result<WindowInfo, FindError> {
    let needle = spec.title.trim().to_lowercase();
    let class_needle = spec.class.trim().to_lowercase();

    let mut hits: Vec<WindowInfo> = enumerate()
        .into_iter()
        .filter(|w| {
            let t = w.title.to_lowercase();
            let title_ok = if needle.is_empty() {
                true
            } else if spec.title_exact {
                t == needle
            } else {
                t.contains(&needle)
            };
            let class_ok = class_needle.is_empty() || w.class.to_lowercase() == class_needle;
            title_ok && class_ok
        })
        .collect();

    if !spec.has.is_empty() && !hits.is_empty() {
        let mut others: Vec<String> = Vec::new();
        hits.retain(|w| {
            let missing = marks_missing_from(&enumerate_controls_all(w.handle).items, &spec.has);
            if !missing.is_empty() {
                let what: Vec<String> =
                    missing.iter().map(|m| format!("{:?} {}x{}", m.text, m.size[0], m.size[1])).collect();
                others.push(format!("{} [{}] pid={} lacks {}", w.title, w.class, w.pid, what.join(", ")));
            }
            missing.is_empty()
        });
        if hits.is_empty() {
            return Err(FindError::Unmarked(others));
        }
    }

    match hits.len() {
        0 => Err(FindError::NotFound),
        1 => Ok(hits.remove(0)),
        _ => Err(FindError::Ambiguous(
            hits.into_iter()
                .map(|w| format!("{} [{}] pid={}", w.title, w.class, w.pid))
                .collect(),
        )),
    }
}

/// One child control inside a window.
#[derive(Clone, Debug)]
pub struct ControlInfo {
    pub class: String,
    /// The caption. On a button this usually holds the label.
    pub text: String,
    /// The dialog control ID (0 when there is none).
    pub id: i32,
    /// `[x, y, w, h]` in **the root window's client coordinates** — writable straight into
    /// the profile file.
    pub rect: [i32; 4],
    pub depth: u32,
    pub visible: bool,
}

/// Enumerate child controls — where it works, nothing has to be measured by eye.
///
/// **It only works on certain applications.** Standard Win32, MFC and WinForms give each
/// button its own window, so they all appear here. WPF and WinUI have a single window and
/// produce nothing, as do industrial HMI mock-ups that paint the panel as one bitmap and
/// hit-test it in code. **An empty list is not a failure but an answer** — it means that
/// application has to be measured by hand.
/// Ceiling on how many controls one response carries. Applications where tree and list
/// interiors are windows too produce thousands.
pub const CONTROL_LIMIT: usize = 500;

/// The enumeration, **including the fact that it was truncated**.
///
/// This used to cut at 500 and hand back only a `Vec`. On a window with 600, a hundred
/// vanished silently and whoever built a profile from that list never knew any were missing.
/// That is the worst kind of failure in this tool — incomplete without an error — so the
/// count travels with it and the caller decides.
pub struct ControlList {
    pub items: Vec<ControlInfo>,
    /// The real count, before truncation.
    pub total: usize,
}

pub fn enumerate_controls(root: isize) -> ControlList {
    enumerate_up_to(root, CONTROL_LIMIT)
}

/// Every control, with no ceiling — for **identifying** a window rather than describing it.
///
/// The ceiling exists to bound a response. Deciding whether a control is present against a
/// list that was cut off reports it missing when it is merely late in the enumeration, and a
/// container is visited only after every descendant of the containers before it — so on an
/// application with thousands of child windows, exactly the controls an anchor or a `has`
/// mark names are the ones most likely to fall past the cut.
pub fn enumerate_controls_all(root: isize) -> ControlList {
    enumerate_up_to(root, usize::MAX)
}

fn enumerate_up_to(root: isize, limit: usize) -> ControlList {
    let Some(info) = describe(root) else {
        return ControlList { items: Vec::new(), total: 0 };
    };
    let mut handles: Vec<isize> = Vec::new();
    unsafe {
        // EnumChildWindows returns every descendant, not just direct children.
        EnumChildWindows(hwnd(root), Some(child_proc), (&raw mut handles) as LPARAM);
    }

    let (ox, oy) = info.client_origin;
    let total = handles.len();
    let items = handles
        .into_iter()
        .take(limit)
        .filter_map(|h| {
            let hw = hwnd(h);
            unsafe {
                let mut r = RECT { left: 0, top: 0, right: 0, bottom: 0 };
                if GetWindowRect(hw, &mut r) == 0 {
                    return None;
                }
                Some(ControlInfo {
                    class: text_of(hw, GetClassNameW),
                    text: text_of(hw, GetWindowTextW),
                    id: GetDlgCtrlID(hw),
                    rect: [r.left - ox, r.top - oy, r.right - r.left, r.bottom - r.top],
                    depth: depth_from(h, root),
                    visible: IsWindowVisible(hw) != 0,
                })
            }
        })
        .collect();
    ControlList { items, total }
}

unsafe extern "system" fn child_proc(h: HWND, lparam: LPARAM) -> i32 {
    let out = unsafe { &mut *(lparam as *mut Vec<isize>) };
    out.push(h as isize);
    1
}

/// How many times the parent chain has to be walked to reach `root`. Bounded, in case of a
/// cycle or something unexpected.
fn depth_from(child: isize, root: isize) -> u32 {
    let mut cur = child;
    for d in 1..=16u32 {
        let parent = unsafe { GetParent(hwnd(cur)) } as isize;
        if parent == root || parent == 0 {
            return d;
        }
        cur = parent;
    }
    16
}

/// Bring the window to the front.
///
/// **This has to succeed before a click.** `SendInput` puts input at a screen coordinate and
/// knows nothing about windows, so if the target is not in front, **whatever window covers
/// that coordinate** receives the click. That is the accident where aiming at an operator
/// panel presses a button on the window behind it.
///
/// `SetForegroundWindow` often fails because of the foreground lock policy, so this attaches
/// to the input queue (`AttachThreadInput`) and tries once more.
pub fn focus(handle: isize) -> Result<(), String> {
    let h = hwnd(handle);
    unsafe {
        if IsWindow(h) == 0 {
            return Err("window is gone".into());
        }
        if IsIconic(h) != 0 {
            ShowWindow(h, SW_RESTORE);
        }
        SetForegroundWindow(h);
        BringWindowToTop(h);
        if is_foreground(handle) {
            return Ok(());
        }

        // Second attempt — attaching to the current foreground window's input thread
        // releases the lock.
        let fg = GetForegroundWindow();
        let fg_thread = GetWindowThreadProcessId(fg, std::ptr::null_mut());
        let our_thread = GetCurrentThreadId();
        if fg_thread != 0 && fg_thread != our_thread {
            AttachThreadInput(our_thread, fg_thread, 1);
            SetForegroundWindow(h);
            BringWindowToTop(h);
            SetFocus(h);
            AttachThreadInput(our_thread, fg_thread, 0);
        }
    }

    // Give the window manager a moment to reflect the switch (up to ~600ms).
    for _ in 0..12 {
        if is_foreground(handle) {
            return Ok(());
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    Err("failed to bring the target window to the foreground \
         (another window may be topmost, or the session is locked)"
        .into())
}

/// Whether the target window, or its root ancestor, is the foreground right now.
pub fn is_foreground(handle: isize) -> bool {
    unsafe {
        let fg = GetForegroundWindow();
        if fg.is_null() {
            return false;
        }
        let h = hwnd(handle);
        fg == h || GetAncestor(fg, GA_ROOT) == GetAncestor(h, GA_ROOT)
    }
}

/// Resize the window so its client area is exactly `w x h` physical pixels, leaving position
/// and Z order alone.
///
/// A change of window size shifts every button coordinate at once, so this is **the one
/// move that puts it back** when the size has drifted. The frame thickness is computed from
/// that window's own style and DPI.
pub fn fit_client(handle: isize, w: i32, h: i32) -> Result<(i32, i32), String> {
    if w <= 0 || h <= 0 {
        return Err("target client size must be positive".into());
    }
    let hw = hwnd(handle);
    unsafe {
        if IsWindow(hw) == 0 {
            return Err("window is gone".into());
        }
        if IsIconic(hw) != 0 {
            ShowWindow(hw, SW_RESTORE);
        }
        let style = GetWindowLongW(hw, GWL_STYLE) as WINDOW_STYLE;
        let ex_style = GetWindowLongW(hw, GWL_EXSTYLE) as WINDOW_EX_STYLE;
        let dpi = GetDpiForWindow(hw);
        let mut rc = RECT { left: 0, top: 0, right: w, bottom: h };
        // Whether there is a menu cannot be known here, so this passes false. A window with
        // a menu is off by one row, which the correction below measures and fixes.
        AdjustWindowRectExForDpi(&mut rc, style, 0, ex_style, if dpi == 0 { 96 } else { dpi });
        let fw = rc.right - rc.left;
        let fh = rc.bottom - rc.top;
        SetWindowPos(hw, std::ptr::null_mut(), 0, 0, fw, fh, SWP_NOMOVE | SWP_NOZORDER | SWP_NOOWNERZORDER);

        // Correction — add whatever difference is left to the frame size.
        let mut cr = RECT { left: 0, top: 0, right: 0, bottom: 0 };
        GetClientRect(hw, &mut cr);
        let (cw, ch) = (cr.right - cr.left, cr.bottom - cr.top);
        if (cw, ch) != (w, h) {
            SetWindowPos(
                hw,
                std::ptr::null_mut(),
                0,
                0,
                fw + (w - cw),
                fh + (h - ch),
                SWP_NOMOVE | SWP_NOZORDER | SWP_NOOWNERZORDER,
            );
            GetClientRect(hw, &mut cr);
        }
        Ok((cr.right - cr.left, cr.bottom - cr.top))
    }
}

/// A common wrapper for the "fill a buffer with UTF-16 and return the length" shape that
/// `GetWindowTextW` and `GetClassNameW` share.
unsafe fn text_of(h: HWND, f: unsafe extern "system" fn(HWND, *mut u16, i32) -> i32) -> String {
    let mut buf = [0u16; 512];
    let n = unsafe { f(h, buf.as_mut_ptr(), buf.len() as i32) };
    if n <= 0 {
        return String::new();
    }
    String::from_utf16_lossy(&buf[..n as usize])
}

/// Lift a button's client coordinate into screen space. This conversion happens **exactly
/// once, immediately before a click**.
pub fn client_to_screen(info: &WindowInfo, x: i32, y: i32) -> (i32, i32) {
    (info.client_origin.0 + x, info.client_origin.1 + y)
}

/// Silences the unused-import warning — `c_void` is used by the unsafe cast above.
const _: Option<*const c_void> = None;

/// The control **directly under** a coordinate.
///
/// ## Why this is needed
///
/// "did the click land" and "did the screen change" are **two independent questions**, and
/// pixel comparison (`changed`) mashes them into one boolean. All four combinations occur:
///
/// | | screen changed | screen identical |
/// |---|---|---|
/// | **hit a control** | it worked | blank key · toggle already in that state · ignored in this mode |
/// | **hit nothing** | a clock or animation moved on its own | the coordinate landed on background |
///
/// Measured on a FANUC NCGuide operator panel: 21 of 128 keys are blank keys with no legend.
/// Nothing happening when you press them is **correct**, so pixels cannot settle it. This
/// function answers the left-hand column — did it land — independently of pixels.
///
/// **It only works on certain applications**, on the same condition as `enumerate_controls`.
/// Where controls are not separate windows (WPF, a single-bitmap HMI) the window itself is
/// always what comes back, and `is_window_itself` says so.
pub fn control_at(root: isize, cx: i32, cy: i32) -> Option<ControlHit> {
    let info = describe(root)?;
    let (ox, oy) = info.client_origin;
    let (sx, sy) = (ox + cx, oy + cy);

    // ChildWindowFromPointEx looks at **direct children only**. Reaching a grandchild means
    // repeating one generation at a time, converting the point into each generation's client
    // coordinates as it goes.
    let mut cur = hwnd(root);
    let mut depth = 0u32;
    for _ in 0..32 {
        let mut p = POINT { x: sx, y: sy };
        unsafe {
            if ScreenToClient(cur, &mut p) == 0 {
                break;
            }
            // CWP_SKIPTRANSPARENT is **deliberately not used.** It also skips disabled
            // (WS_DISABLED) controls, and a disabled control is precisely what has to be
            // reported here — "the click arrived and the control ignored it" is a correct
            // no-change. With that flag on, points known to have a child returned the window
            // itself (measured 2026-08-26), which then reads as "you pressed background".
            let child = ChildWindowFromPointEx(cur, p, CWP_SKIPINVISIBLE);
            if child.is_null() || child == cur {
                break;
            }
            cur = child;
        }
        depth += 1;
    }

    unsafe {
        let mut r = RECT { left: 0, top: 0, right: 0, bottom: 0 };
        if GetWindowRect(cur, &mut r) == 0 {
            return None;
        }
        Some(ControlHit {
            hwnd: cur as isize,
            class: text_of(cur, GetClassNameW),
            text: text_of(cur, GetWindowTextW),
            id: GetDlgCtrlID(cur),
            rect: [r.left - ox, r.top - oy, r.right - r.left, r.bottom - r.top],
            visible: IsWindowVisible(cur) != 0,
            enabled: IsWindowEnabled(cur) != 0,
            depth,
            is_window_itself: depth == 0,
        })
    }
}

/// The result of `control_at`. Kept separate from `ControlInfo` because this is **a judgement
/// about one point**, where `enabled` (will a press do anything) and `is_window_itself` (is
/// there no child at all) are the whole value — and neither means anything in a listing.
#[derive(Clone, Debug)]
pub struct ControlHit {
    /// The window handle, for telling across calls whether it is the same control.
    pub hwnd: isize,
    pub class: String,
    pub text: String,
    pub id: i32,
    /// `[x, y, w, h]` in **the root window's client coordinates** — the same system the
    /// button definitions use.
    pub rect: [i32; 4],
    pub visible: bool,
    /// A disabled control takes the click and does nothing — also a correct "no change".
    pub enabled: bool,
    /// How many generations down from the root. 0 means there was no child.
    pub depth: u32,
    /// The target window itself was hit = no child control sits there (either background was
    /// pressed, or this application does not split controls into windows).
    pub is_window_itself: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn control(text: &str, rect: [i32; 4]) -> ControlInfo {
        ControlInfo { class: String::new(), text: text.into(), id: 0, rect, depth: 2, visible: true }
    }

    /// The live case. The running NCGuide had its `Main Panel` at 705x411, which is the 0i
    /// lathe; the 0i mill's is 666x439. Same text, so only the size can tell them apart —
    /// which is why a mark carries both.
    #[test]
    fn a_mark_is_matched_on_text_and_size_together() {
        let running = [
            control("Main Panel", [670, 530, 705, 411]),
            control("MDI", [2, 531, 667, 409]),
            control("CNC", [2, 2, 666, 529]),
        ];
        let lathe = ControlMark { text: "Main Panel".into(), size: [705, 411] };
        let mill = ControlMark { text: "Main Panel".into(), size: [666, 439] };

        assert!(marks_missing_from(&running, std::slice::from_ref(&lathe)).is_empty());
        assert_eq!(marks_missing_from(&running, std::slice::from_ref(&mill)), std::slice::from_ref(&mill));

        // Every mark has to be there, and each missing one is reported rather than the first.
        let screen = ControlMark { text: "CNC".into(), size: [666, 529] };
        let gone = ControlMark { text: "Sub Panel".into(), size: [1, 1] };
        assert_eq!(
            marks_missing_from(&running, &[lathe, screen, mill.clone(), gone.clone()]),
            [mill, gone]
        );

        // Whitespace around a caption is not part of its name, as for anchors.
        let padded = [control("  Main Panel ", [0, 0, 705, 411])];
        assert!(marks_missing_from(&padded, &[ControlMark { text: "Main Panel".into(), size: [705, 411] }]).is_empty());
    }

    /// A mark with only a text would match every project that has a panel of that name, and
    /// look as if it were working. So the size is not optional.
    #[test]
    fn a_mark_without_a_size_does_not_load() {
        let no_size = r#"{"title": "FANUC NCGuide", "has": [{"text": "Main Panel"}]}"#;
        assert!(serde_json::from_str::<WindowSpec>(no_size).is_err());

        let ok = r#"{"title": "FANUC NCGuide", "has": [{"text": "Main Panel", "size": [705, 411]}]}"#;
        let spec: WindowSpec = serde_json::from_str(ok).expect("parses");
        assert_eq!(spec.has.len(), 1);

        // Absent is the ordinary case, and it stays absent when written back.
        let plain: WindowSpec = serde_json::from_str(r#"{"title": "x"}"#).expect("parses");
        assert!(plain.has.is_empty());
        let out = serde_json::to_value(&plain).expect("serializes");
        assert!(out.get("has").is_none(), "{out}");
    }

    /// Check that `control_at` really picks the control at a point, against **windows that
    /// are already open**.
    ///
    /// Two coordinate systems meet here: `enumerate_controls` reports a child's rectangle in
    /// the root's client coordinates, and `control_at` takes the same coordinates, lifts them
    /// to screen space and lowers them again at every generation. Get the origin wrong at
    /// either end and you get **the wrong control, with no error**.
    ///
    /// Read-only — it creates no window, moves no focus, captures nothing and presses nothing.
    /// On a PC with no child-window application open (only WPF, say) there is nothing to check
    /// against, and it passes quietly: reporting a missing environment as a failure would be a
    /// failure that carries no information.
    #[test]
    fn control_at_lands_on_the_control_that_owns_that_point() {
        let mut checked = 0;
        let mut reached = 0;
        'sweep: for w in enumerate() {
            // Direct children only (depth 1). `enumerate_controls` returns every descendant,
            // and a grandchild can sit at a point its own parent does not cover — clipped or
            // positioned outside it. Descending one generation at a time cannot reach those,
            // and correctly so, which would make this assertion wrong rather than the code.
            let kids: Vec<ControlInfo> = enumerate_controls(w.handle)
                .items
                .into_iter()
                .filter(|c| c.depth == 1 && c.visible && c.rect[2] > 8 && c.rect[3] > 8)
                .collect();
            for k in kids.into_iter().take(3) {
                let (cx, cy) = (k.rect[0] + k.rect[2] / 2, k.rect[1] + k.rect[3] / 2);
                // Skip children hanging outside the client area (a scrolled list, say).
                if cx < 0 || cy < 0 || cx >= w.client_size.0 || cy >= w.client_size.1 {
                    continue;
                }
                let Some(hit) = control_at(w.handle, cx, cy) else { continue };

                // Whatever came back has to CONTAIN the point. This is the strict part, and
                // it is what a coordinate-system mistake breaks: get the origin wrong at
                // either end and the returned rectangle stops covering the point asked about.
                let r = hit.rect;
                assert!(
                    cx >= r[0] && cx < r[0] + r[2] && cy >= r[1] && cy < r[1] + r[3],
                    "control_at({cx},{cy}) returned {:?} whose rect {r:?} does not contain the point",
                    hit.class
                );

                // Reaching a child is NOT asserted per sample. On a live desktop a window can
                // move between the enumeration and the query, and a control can be covered by
                // something that is not its sibling, so an individual miss says nothing. What
                // does have to hold is that descent works AT ALL — with a wrong flag
                // (CWP_SKIPTRANSPARENT, once) every point reported the window itself, and a
                // sweep that never reaches one child is exactly that failure.
                if !hit.is_window_itself {
                    reached += 1;
                }
                checked += 1;
                if checked >= 12 {
                    break 'sweep;
                }
            }
        }
        // Nothing to check against — not a failure. On a PC with only WPF windows open there
        // is no child-window application to test with.
        if checked > 0 {
            assert!(reached > 0, "{checked} points checked and not one reached a child control");
        }
    }

    /// A point with no child under it has to answer with **the window itself** — that is the
    /// signal for "you pressed background".
    #[test]
    fn a_point_outside_every_child_reports_the_window_itself() {
        for w in enumerate() {
            if w.client_size.0 < 4 || w.client_size.1 < 4 {
                continue;
            }
            let kids = enumerate_controls(w.handle).items;
            // Find a point no child covers — the bottom-right corner of the client area.
            let (cx, cy) = (w.client_size.0 - 1, w.client_size.1 - 1);
            if kids.iter().any(|k| {
                cx >= k.rect[0] && cx < k.rect[0] + k.rect[2]
                    && cy >= k.rect[1] && cy < k.rect[1] + k.rect[3]
            }) {
                continue;
            }
            let Some(hit) = control_at(w.handle, cx, cy) else { continue };
            assert!(hit.is_window_itself, "expected the window itself at ({cx},{cy}), got {:?}", hit.class);
            assert_eq!(hit.depth, 0);
            return;
        }
    }
}
