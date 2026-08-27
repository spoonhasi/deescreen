//! Mouse and keyboard injection (`SendInput`).
//!
//! ## The UIPI warning
//!
//! If the target application runs as administrator and we do not, `SendInput` **does nothing
//! and returns success.** MSDN states outright that neither the return value nor
//! `GetLastError` reports UIPI blocking, so detecting it after the fact is impossible. This
//! module therefore cannot detect it, and two things stand in instead:
//! - `/health` compares both elevation states up front and warns (`win::process_elevation`)
//! - `/click` compares captures before and after and reports `changed: false`

use std::time::Duration;

use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    INPUT, INPUT_0, INPUT_KEYBOARD, INPUT_MOUSE, KEYBDINPUT, KEYEVENTF_EXTENDEDKEY,
    KEYEVENTF_KEYUP, KEYEVENTF_UNICODE, MAPVK_VK_TO_VSC, MOUSEEVENTF_ABSOLUTE,
    MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP, MOUSEEVENTF_MIDDLEDOWN, MOUSEEVENTF_MIDDLEUP,
    MOUSEEVENTF_MOVE, MOUSEEVENTF_RIGHTDOWN, MOUSEEVENTF_RIGHTUP, MOUSEEVENTF_VIRTUALDESK,
    MOUSEINPUT, MapVirtualKeyW, SendInput,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    GetSystemMetrics, SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Button {
    Left,
    Right,
    Middle,
}

impl Button {
    pub fn parse(s: &str) -> Result<Button, String> {
        match s.trim().to_lowercase().as_str() {
            "" | "left" | "l" => Ok(Button::Left),
            "right" | "r" => Ok(Button::Right),
            "middle" | "m" => Ok(Button::Middle),
            other => Err(format!("unknown mouse button: {other} (left|right|middle)")),
        }
    }

    fn down_up(self) -> (u32, u32) {
        match self {
            Button::Left => (MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP),
            Button::Right => (MOUSEEVENTF_RIGHTDOWN, MOUSEEVENTF_RIGHTUP),
            Button::Middle => (MOUSEEVENTF_MIDDLEDOWN, MOUSEEVENTF_MIDDLEUP),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Button::Left => "left",
            Button::Right => "right",
            Button::Middle => "middle",
        }
    }
}

/// Absolute screen coordinates into the 0..65535 normalised form `SendInput` wants.
///
/// Computed against **the virtual desktop** (`MOUSEEVENTF_VIRTUALDESK`) — using the primary
/// monitor as the reference makes windows on a secondary monitor unclickable.
fn normalize(sx: i32, sy: i32) -> (i32, i32) {
    unsafe {
        let vx = GetSystemMetrics(SM_XVIRTUALSCREEN);
        let vy = GetSystemMetrics(SM_YVIRTUALSCREEN);
        let vw = GetSystemMetrics(SM_CXVIRTUALSCREEN).max(1);
        let vh = GetSystemMetrics(SM_CYVIRTUALSCREEN).max(1);
        let nx = ((sx - vx) as i64 * 65535 / (vw - 1).max(1) as i64) as i32;
        let ny = ((sy - vy) as i64 * 65535 / (vh - 1).max(1) as i64) as i32;
        (nx.clamp(0, 65535), ny.clamp(0, 65535))
    }
}

fn mouse_event(flags: u32, nx: i32, ny: i32) -> INPUT {
    INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 {
            mi: MOUSEINPUT {
                dx: nx,
                dy: ny,
                mouseData: 0,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    }
}

fn send(events: &[INPUT]) -> Result<(), String> {
    let sent = unsafe { SendInput(events.len() as u32, events.as_ptr(), size_of::<INPUT>() as i32) };
    if sent as usize != events.len() {
        return Err(format!("SendInput injected {sent}/{} events", events.len()));
    }
    Ok(())
}

/// Click at absolute screen coordinates. The target window has to be **brought to the front first**.
pub fn click(sx: i32, sy: i32, button: Button, double: bool, hold_ms: u64) -> Result<(), String> {
    let (nx, ny) = normalize(sx, sy);
    let (down, up) = button.down_up();

    // Send the move on its own first, so a button that changes appearance on hover (common
    // in operator-panel UI) has a moment to update itself before the press arrives.
    send(&[mouse_event(MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK, nx, ny)])?;
    std::thread::sleep(Duration::from_millis(20));

    // MOUSEEVENTF_MOVE rides on the press as well, so the press itself pins the cursor to
    // (nx, ny).
    //
    // Without it, the coordinates on a button-down are inert — Windows only moves the pointer
    // when MOVE is set — and the press lands wherever the cursor happens to be. That is fine
    // right up until a person nudges the physical mouse during the hover pause above, at which
    // point the click goes somewhere nobody chose, with no error. A batch passed to SendInput
    // in one call is inserted serially and is never interspersed with the user's own input, so
    // placing and pressing inside one batch closes that window.
    let flags = MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK | MOUSEEVENTF_MOVE;

    // Down and up are sent as SEPARATE calls with a wait between them, so the contact is
    // closed for a measurable length of time.
    //
    // They used to travel in one batch, which the system inserts back to back: the button was
    // pressed and released inside the same tick. A Win32 button does not mind — it latches on
    // the down and fires on the up. A simulated machine key does: something scans that contact
    // on a cycle, and a press that exists for no measurable time is a press no scan ever sees.
    // On NC Trainer2 plus that made CYCLE START do nothing whatsoever, while the very same
    // coordinate showed the application reacting to the pointer arriving.
    //
    // The move above still shares the down's batch, so a person nudging the physical mouse
    // during the hover pause cannot drag the press off target. Only the release is separated,
    // and a release lands wherever the button already went down.
    let press = |events: &[_]| send(events);
    press(&[mouse_event(down | flags, nx, ny)])?;
    std::thread::sleep(Duration::from_millis(hold_ms));
    press(&[mouse_event(up | flags, nx, ny)])?;
    if double {
        std::thread::sleep(Duration::from_millis(hold_ms.min(60)));
        press(&[mouse_event(down | flags, nx, ny)])?;
        std::thread::sleep(Duration::from_millis(hold_ms));
        press(&[mouse_event(up | flags, nx, ny)])?;
    }
    Ok(())
}

/// Modifiers plus one key — the result of parsing a string like `"ctrl+alt+f1"`.
#[derive(Clone, Debug)]
pub struct Chord {
    pub mods: Vec<u16>,
    pub vk: u16,
}

fn key_event(vk: u16, up: bool) -> INPUT {
    let mut flags = if up { KEYEVENTF_KEYUP } else { 0 };
    if is_extended(vk) {
        flags |= KEYEVENTF_EXTENDEDKEY;
    }
    // Send the scan code as well. Some applications read the scan code rather than the VK
    // (game engines, some industrial HMIs), and filling in both works either way.
    let scan = unsafe { MapVirtualKeyW(vk as u32, MAPVK_VK_TO_VSC) } as u16;
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: vk,
                wScan: scan,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    }
}

/// Extended keys — without this flag the arrow and editing keys are mistaken for numpad keys.
fn is_extended(vk: u16) -> bool {
    matches!(
        vk,
        0x21..=0x28 // PageUp PageDown End Home Left Up Right Down
            | 0x2D | 0x2E // Insert Delete
            | 0x90 // NumLock
            | 0x6F // Divide
            | 0x5C // RWin
            | 0xA3 | 0xA5 // RControl RMenu
            | 0x2C // PrintScreen
    )
}

/// Press and release one chord. Modifiers nest properly: pressed in order, released in reverse.
pub fn send_chord(chord: &Chord) -> Result<(), String> {
    let mut events: Vec<INPUT> = Vec::with_capacity(chord.mods.len() * 2 + 2);
    for m in &chord.mods {
        events.push(key_event(*m, false));
    }
    events.push(key_event(chord.vk, false));
    events.push(key_event(chord.vk, true));
    for m in chord.mods.iter().rev() {
        events.push(key_event(*m, true));
    }
    send(&events)
}

/// Type a Unicode string verbatim, regardless of keyboard layout.
/// `KEYEVENTF_UNICODE` skips VK mapping, so non-Latin text and symbols arrive intact.
///
/// **One `SendInput` call per character, with a gap between them.** Batched into a single
/// array, characters go missing silently — measured 2026-08-12 on Windows 11 Notepad: 60
/// characters sent at once arrived as 17, and even split into three batches of ten, four
/// vanished from the last batch. There is no error. `SendInput` returns the count it queued;
/// what loses them is the target application's input handling (particularly WinUI/UWP, which
/// processes input asynchronously).
///
/// So this trades speed for certainty — about two seconds at the 512-character limit.
const TYPE_INTERVAL: Duration = Duration::from_millis(4);

pub fn send_text(text: &str) -> Result<(), String> {
    for unit in text.encode_utf16() {
        let mk = |up: bool| INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: INPUT_0 {
                ki: KEYBDINPUT {
                    wVk: 0,
                    wScan: unit,
                    dwFlags: KEYEVENTF_UNICODE | if up { KEYEVENTF_KEYUP } else { 0 },
                    time: 0,
                    dwExtraInfo: 0,
                },
            },
        };
        send(&[mk(false), mk(true)])?;
        std::thread::sleep(TYPE_INTERVAL);
    }
    Ok(())
}

/// `"ctrl+shift+f5"` → `Chord`. The modifiers are `ctrl` `alt` `shift` `win`.
pub fn parse_chord(s: &str) -> Result<Chord, String> {
    let src = s.trim();
    if src.is_empty() {
        return Err("empty key spec".into());
    }
    let parts: Vec<&str> = src.split('+').map(|p| p.trim()).filter(|p| !p.is_empty()).collect();
    // One `let else` rather than an emptiness check and a later unwrap: the guard and the use
    // were four lines apart, and that gap is where a refactor turns a check into a panic.
    let Some((key, mods_src)) = parts.split_last() else {
        return Err(format!("cannot parse key spec: {src}"));
    };
    let mut mods = Vec::new();
    for m in mods_src {
        mods.push(match m.to_lowercase().as_str() {
            "ctrl" | "control" => 0x11u16,
            "alt" | "menu" => 0x12,
            "shift" => 0x10,
            "win" | "super" | "meta" => 0x5B,
            other => return Err(format!("unknown modifier: {other} (ctrl|alt|shift|win)")),
        });
    }
    let vk = vk_of(key).ok_or_else(|| format!("unknown key name: {key}"))?;
    Ok(Chord { mods, vk })
}

/// Key name → Win32 virtual key code. The values are the VK_* constants as documented.
fn vk_of(name: &str) -> Option<u16> {
    let n = name.to_lowercase();
    let b = n.as_bytes();

    // a single letter or digit
    if b.len() == 1 {
        let c = b[0];
        if c.is_ascii_lowercase() {
            return Some(0x41 + (c - b'a') as u16);
        }
        if c.is_ascii_digit() {
            return Some(0x30 + (c - b'0') as u16);
        }
    }
    // function keys f1..f24
    if let Some(rest) = n.strip_prefix('f')
        && let Ok(idx) = rest.parse::<u16>()
        && (1..=24).contains(&idx)
    {
        return Some(0x6F + idx);
    }
    // numpad0..numpad9
    if let Some(rest) = n.strip_prefix("numpad")
        && let Ok(d) = rest.parse::<u16>()
        && d <= 9
    {
        return Some(0x60 + d);
    }

    Some(match n.as_str() {
        "enter" | "return" => 0x0D,
        "esc" | "escape" => 0x1B,
        "space" | "spacebar" => 0x20,
        "tab" => 0x09,
        "backspace" | "bksp" => 0x08,
        "delete" | "del" => 0x2E,
        "insert" | "ins" => 0x2D,
        "home" => 0x24,
        "end" => 0x23,
        "pageup" | "pgup" => 0x21,
        "pagedown" | "pgdn" => 0x22,
        "up" => 0x26,
        "down" => 0x28,
        "left" => 0x25,
        "right" => 0x27,
        "capslock" => 0x14,
        "numlock" => 0x90,
        "scrolllock" => 0x91,
        "printscreen" | "prtsc" => 0x2C,
        "pause" => 0x13,
        "apps" | "contextmenu" => 0x5D,
        "multiply" => 0x6A,
        "add" | "plus" => 0x6B,
        "subtract" | "minus" => 0x6D,
        "decimal" => 0x6E,
        "divide" => 0x6F,
        "semicolon" => 0xBA,
        "equals" => 0xBB,
        "comma" => 0xBC,
        "period" | "dot" => 0xBE,
        "slash" => 0xBF,
        "backtick" | "grave" => 0xC0,
        "lbracket" => 0xDB,
        "backslash" => 0xDC,
        "rbracket" => 0xDD,
        "quote" => 0xDE,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_plain_and_modified_keys() {
        let c = parse_chord("f5").expect("f5");
        assert_eq!(c.vk, 0x74);
        assert!(c.mods.is_empty());

        let c = parse_chord("Ctrl+Shift+A").expect("chord");
        assert_eq!(c.vk, 0x41);
        assert_eq!(c.mods, vec![0x11, 0x10]);
    }

    #[test]
    fn function_key_range_is_bounded() {
        assert_eq!(parse_chord("f24").expect("f24").vk, 0x87);
        // there is no f25 — it must not slide quietly into some other code
        assert!(parse_chord("f25").is_err());
        assert!(parse_chord("f0").is_err());
    }

    #[test]
    fn unknown_names_are_rejected() {
        assert!(parse_chord("cycle_start").is_err());
        assert!(parse_chord("ctrl+").is_err());
        assert!(parse_chord("hyper+a").is_err());
    }

    #[test]
    fn arrow_keys_are_extended() {
        // without the extended flag the arrows are mistaken for numpad — regression guard
        for k in ["up", "down", "left", "right", "home", "end", "delete"] {
            let vk = parse_chord(k).expect(k).vk;
            assert!(is_extended(vk), "{k} should be an extended key");
        }
        assert!(!is_extended(parse_chord("a").expect("a").vk));
    }
}
