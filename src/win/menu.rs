//! The window's own menu bar — reading it, and invoking one item.
//!
//! ## Why this is not a click
//!
//! A menu item has no stable rectangle. It only has one while the menu is open, it moves with
//! the length of the items above it, and opening a menu to click inside it puts the application
//! in a state that a failed click then leaves it in. So the whole feature is done through the
//! menu's own identity instead: the item carries a command ID, and posting that ID is what the
//! application receives when a person picks it.
//!
//! ## What that is not
//!
//! `WM_COMMAND` is what an application receives **after** it has decided a menu item is
//! enabled and which item was picked. An application that changes item state in
//! `WM_INITMENUPOPUP` — greying out what is not currently valid — never runs that code here,
//! because the menu is never opened. So a disabled item's ID posted anyway may be acted on. The
//! state read from the menu is reported for exactly that reason, and invoking a disabled item
//! is refused rather than attempted.
//!
//! Posted, never sent: a menu item that opens a modal dialog does not return until the dialog
//! is closed, and `SendMessage` would hold this thread — and with it the input lock — for as
//! long as the dialog is on screen.

use windows_sys::Win32::Foundation::HWND;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    GetMenu, GetMenuItemCount, GetMenuItemInfoW, MENUITEMINFOW, MF_BYPOSITION, MFS_CHECKED,
    MFS_DISABLED, MFS_GRAYED, MFT_SEPARATOR, MIIM_FTYPE, MIIM_ID, MIIM_STATE, MIIM_STRING,
    MIIM_SUBMENU, PostMessageW, WM_COMMAND,
};

use super::hwnd;

/// One entry in the menu tree.
#[derive(Clone, Debug)]
pub struct MenuItem {
    /// The full path to it, `"Tool/Set Machine Parameters"` — what `POST /menu` takes.
    pub path: String,
    /// The caption as the application wrote it, `&` and the accelerator column and all.
    pub label: String,
    /// The command ID. `None` for a submenu, which is a place rather than an action.
    pub id: Option<u32>,
    /// How deep, from 0 for the menu bar itself.
    pub depth: u32,
    pub enabled: bool,
    pub checked: bool,
    /// A submenu. Not invocable; its children are.
    pub submenu: bool,
}

/// Strip what belongs to the menu's presentation rather than to its name.
///
/// `&` marks the underlined letter and is not part of what anyone calls the item; everything
/// from a tab on is the accelerator column (`Ctrl+S`), which is a second way to reach the same
/// item rather than part of its name. Both would otherwise have to be typed exactly into a
/// path, including the position of an ampersand nobody can see.
pub fn clean_label(raw: &str) -> String {
    let head = raw.split('\t').next().unwrap_or(raw);
    let mut out = String::with_capacity(head.len());
    let mut chars = head.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '&' {
            // `&&` is a literal ampersand in a caption.
            if chars.peek() == Some(&'&') {
                chars.next();
                out.push('&');
            }
            continue;
        }
        out.push(c);
    }
    out.trim().to_string()
}

/// Everything in the window's menu bar, depth-first, in the order it is drawn.
///
/// An empty list is an answer, not a failure: plenty of applications have no menu bar at all,
/// and some put it inside their own drawing where no API can see it.
pub fn read(window: isize) -> Vec<MenuItem> {
    let h = hwnd(window);
    let bar = unsafe { GetMenu(h) };
    if bar.is_null() {
        return Vec::new();
    }
    let mut out = Vec::new();
    // Bounded so a corrupt or hostile menu cannot walk forever. Real menu bars are three deep.
    walk(bar, "", 0, &mut out);
    out
}

fn walk(menu: *mut core::ffi::c_void, prefix: &str, depth: u32, out: &mut Vec<MenuItem>) {
    if depth > 8 {
        return;
    }
    let count = unsafe { GetMenuItemCount(menu) };
    if count <= 0 {
        return;
    }
    for i in 0..count {
        // Two calls: the first asks how long the caption is, the second reads it. `cch` is
        // both the buffer size going in and the length coming out, so it has to be reset.
        let mut info: MENUITEMINFOW = unsafe { std::mem::zeroed() };
        info.cbSize = std::mem::size_of::<MENUITEMINFOW>() as u32;
        info.fMask = MIIM_STRING | MIIM_SUBMENU | MIIM_ID | MIIM_STATE | MIIM_FTYPE;
        if unsafe { GetMenuItemInfoW(menu, i as u32, MF_BYPOSITION as i32, &mut info) } == 0 {
            continue;
        }
        if info.fType & MFT_SEPARATOR != 0 {
            continue;
        }
        let mut buf = vec![0u16; info.cch as usize + 1];
        info.dwTypeData = buf.as_mut_ptr();
        info.cch += 1;
        let label = if unsafe { GetMenuItemInfoW(menu, i as u32, MF_BYPOSITION as i32, &mut info) } != 0
        {
            String::from_utf16_lossy(&buf[..info.cch as usize])
        } else {
            String::new()
        };

        let name = clean_label(&label);
        // An item with no caption is one this cannot be asked for by name. Skipping it would
        // hide it; it is listed with its position so it can at least be seen to exist.
        let name = if name.is_empty() { format!("#{i}") } else { name };
        let path = if prefix.is_empty() { name.clone() } else { format!("{prefix}/{name}") };

        let submenu = !info.hSubMenu.is_null();
        out.push(MenuItem {
            path: path.clone(),
            label,
            id: if submenu { None } else { Some(info.wID) },
            depth,
            // MFS_DISABLED and MFS_GRAYED are the same bit; both are checked so the intent
            // survives a rename in the windows crate.
            enabled: info.fState & (MFS_DISABLED | MFS_GRAYED) == 0,
            checked: info.fState & MFS_CHECKED != 0,
            submenu,
        });
        if submenu {
            walk(info.hSubMenu, &path, depth + 1, out);
        }
    }
}

/// Post a menu command to the window.
///
/// `PostMessageW`, not `SendMessage`: an item that opens a modal dialog would not return until
/// the dialog closed, holding this thread and the input lock with it. The cost is that the
/// return value says the message was queued, not that the application did anything — which is
/// why the caller captures the screen afterwards like it does for a click.
pub fn invoke(window: isize, id: u32) -> Result<(), String> {
    let ok = unsafe { PostMessageW(hwnd(window), WM_COMMAND, id as usize, 0) };
    if ok == 0 {
        return Err(format!(
            "the menu command could not be posted to the window (id {id}). If deescreen runs at \
             a lower integrity level than the application, Windows blocks this silently — check \
             input.uipi_risk in GET /health."
        ));
    }
    Ok(())
}

/// Silences the unused-import warning on non-Windows analysis passes.
const _: Option<HWND> = None;

#[cfg(test)]
mod tests {
    use super::*;

    /// A path has to be typeable. The ampersand marks an underlined letter and is invisible on
    /// screen, and everything past a tab is the accelerator column — requiring either in a path
    /// would mean asking for a name nobody can read off the menu.
    #[test]
    fn a_label_loses_its_underline_marker_and_its_accelerator() {
        assert_eq!(clean_label("&File"), "File");
        assert_eq!(clean_label("Set &Machine Parameters"), "Set Machine Parameters");
        assert_eq!(clean_label("&Save\tCtrl+S"), "Save");
        assert_eq!(clean_label("  &Open...  "), "Open...");
        // A doubled ampersand is a literal one — "AT&&T" is a menu item called AT&T.
        assert_eq!(clean_label("AT&&T"), "AT&T");
        assert_eq!(clean_label(""), "");
    }
}
