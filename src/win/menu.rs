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
//! ## State is only true once the application has been asked
//!
//! `WM_COMMAND` is what an application receives **after** it has decided a menu item is
//! enabled and which item was picked. Many applications decide that only as a menu opens: MFC
//! sets every check mark and every greyed item in `WM_INITMENUPOPUP`, and leaves whatever the
//! resource file said until then. Read cold, NC Trainer's menu had both of two exclusive view
//! modes checked and a submenu greyed whose items all worked.
//!
//! So reading sends the same messages a person opening each menu would cause —
//! `WM_INITMENU` for the bar, `WM_INITMENUPOPUP` before each submenu is read and
//! `WM_UNINITMENUPOPUP` after — without opening anything. A menu the application builds on
//! demand gets built by the same message. Each is sent with a timeout, since a sent message
//! waits for the application, and one that does not answer ends the asking for the rest of
//! the read: `Menu::refreshed` then says the states are as the menu held them.
//!
//! A disabled item's ID posted anyway may still be acted on, so invoking one is refused
//! rather than attempted - and so is one inside a disabled submenu, which no person could
//! open to reach it (`MenuItem::blocked_by`).
//!
//! Posted, never sent: a menu item that opens a modal dialog does not return until the dialog
//! is closed, and `SendMessage` would hold this thread — and with it the input lock — for as
//! long as the dialog is on screen.

use windows_sys::Win32::Foundation::HWND;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    GetMenu, GetMenuItemCount, GetMenuItemInfoW, MENUITEMINFOW, MF_BYPOSITION, MFS_CHECKED,
    MFS_DISABLED, MFS_GRAYED, MFT_SEPARATOR, MIIM_FTYPE, MIIM_ID, MIIM_STATE, MIIM_STRING,
    MIIM_SUBMENU, PostMessageW, SMTO_ABORTIFHUNG, SendMessageTimeoutW, WM_COMMAND, WM_INITMENU,
    WM_INITMENUPOPUP, WM_UNINITMENUPOPUP,
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
    /// The outermost submenu above this one that is disabled, if any.
    ///
    /// A disabled submenu does not open, so nothing under it can be reached by a person
    /// however its own state reads - NCGuide greys "Cycle Time Estimate Function" and leaves
    /// "Start Estimation" inside it enabled. The command id is still there to post, which is
    /// why this is recorded rather than inferred from `enabled`.
    pub blocked_by: Option<String>,
    /// A submenu. Not invocable; its children are.
    pub submenu: bool,
    /// A submenu that came back with nothing in it.
    ///
    /// A menu the application fills at the moment it is opened — a recent-files list, or a
    /// whole File menu built on demand — and did not fill when asked (see the module notes):
    /// one that waits until it is really on screen. Its items do not exist from here and cannot
    /// be invoked by path. Flagged so that asking for one gets that explanation instead of a
    /// list of names that look similar.
    pub empty: bool,
}

/// Strip what belongs to the menu's presentation rather than to its name.
///
/// Three things are presentation, not name:
///
/// - the accelerator column, everything from a tab on (`Save\tCtrl+S`) - a second way to reach
///   the item;
/// - the mnemonic marker. Western menus put `&` before the underlined letter (`&File`);
///   Japanese and Chinese ones append the letter as a group (`ファイル(&F)`, `Import(&I)`),
///   and removing only the `&` there left `Import(I)` in the path;
/// - a trailing ellipsis (`Import...`), which says a dialog follows.
///
/// All three would otherwise have to be typed exactly into a path. Only a group that is
/// exactly `(&` + one character + `)` is removed, in ASCII or full-width parentheses, so a
/// caption like `中文(简体)(&S)` keeps its `(简体)`. `&&` is a literal ampersand.
pub fn clean_label(raw: &str) -> String {
    let head: Vec<char> = raw.split('\t').next().unwrap_or(raw).chars().collect();
    let mut out = String::with_capacity(head.len());
    let mut i = 0;
    while i < head.len() {
        let c = head[i];
        // (&X) or （&X）: the mnemonic group, and any space in front of it.
        if (c == '(' || c == '（')
            && head.get(i + 1) == Some(&'&')
            && head.get(i + 2).is_some_and(|x| *x != '&')
            && matches!(head.get(i + 3), Some(')') | Some('）'))
        {
            let kept = out.trim_end().len();
            out.truncate(kept);
            i += 4;
            continue;
        }
        if c == '&' {
            if head.get(i + 1) == Some(&'&') {
                out.push('&');
                i += 2;
            } else {
                i += 1;
            }
            continue;
        }
        out.push(c);
        i += 1;
    }
    let trimmed = out.trim();
    let trimmed = trimmed.strip_suffix("...").or_else(|| trimmed.strip_suffix('…')).unwrap_or(trimmed);
    trimmed.trim_end().to_string()
}

/// The menu bar as read.
pub struct Menu {
    pub items: Vec<MenuItem>,
    /// Whether the application was asked to bring every state up to date, and answered each
    /// time. `false` when asking was turned off, or a message went unanswered — then `enabled`
    /// and `checked` are what the menu held, which may be the resource file's defaults.
    pub refreshed: bool,
}

/// How long one menu message may wait for the application. Opening a menu is instant for a
/// responsive program; longer than this and it is busy, and a read should not wait it out.
const ASK_TIMEOUT_MS: u32 = 500;

/// Sends the opening messages, until one goes unanswered.
struct Asker {
    window: HWND,
    /// Still asking. Cleared by the first failure, so a busy application costs one timeout per
    /// read rather than one per submenu.
    on: bool,
}

impl Asker {
    fn send(&mut self, msg: u32, wparam: usize, lparam: isize) {
        if !self.on {
            return;
        }
        let mut result = 0usize;
        let ok = unsafe {
            SendMessageTimeoutW(self.window, msg, wparam, lparam, SMTO_ABORTIFHUNG, ASK_TIMEOUT_MS, &mut result)
        };
        if ok == 0 {
            self.on = false;
        }
    }
}

/// Everything in the window's menu bar, depth-first, in the order it is drawn.
///
/// With `refresh`, the application is first asked to update each menu's state, as it would be
/// when a person opens it — see the module notes.
///
/// An empty list is an answer, not a failure: plenty of applications have no menu bar at all,
/// and some put it inside their own drawing where no API can see it.
pub fn read(window: isize, refresh: bool) -> Menu {
    let h = hwnd(window);
    let bar = unsafe { GetMenu(h) };
    if bar.is_null() {
        return Menu { items: Vec::new(), refreshed: false };
    }
    let mut ask = Asker { window: h, on: refresh };
    ask.send(WM_INITMENU, bar as usize, 0);
    let mut items = Vec::new();
    // Bounded so a corrupt or hostile menu cannot walk forever. Real menu bars are three deep.
    walk(bar, "", 0, None, &mut items, &mut ask);
    Menu { items, refreshed: ask.on }
}

fn walk(
    menu: *mut core::ffi::c_void,
    prefix: &str,
    depth: u32,
    blocked_by: Option<&str>,
    out: &mut Vec<MenuItem>,
    ask: &mut Asker,
) {
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
        // MFS_DISABLED and MFS_GRAYED are the same bit; both are checked so the intent survives
        // a rename in the windows crate.
        let enabled = info.fState & (MFS_DISABLED | MFS_GRAYED) == 0;
        let at = out.len();
        out.push(MenuItem {
            path: path.clone(),
            label,
            id: if submenu { None } else { Some(info.wID) },
            depth,
            enabled,
            checked: info.fState & MFS_CHECKED != 0,
            blocked_by: blocked_by.map(str::to_string),
            submenu,
            empty: false,
        });
        if submenu {
            // The outermost one is named: it is the one a person would have to get past first.
            let below = blocked_by.or((!enabled).then_some(path.as_str()));
            // lParam: the position in the parent, and FALSE for "not the window menu".
            ask.send(WM_INITMENUPOPUP, info.hSubMenu as usize, (i as u16) as isize);
            walk(info.hSubMenu, &path, depth + 1, below, out, ask);
            ask.send(WM_UNINITMENUPOPUP, info.hSubMenu as usize, 0);
            // Nothing was added under it — see `MenuItem::empty`.
            out[at].empty = out.len() == at + 1;
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
        assert_eq!(clean_label("  &Open...  "), "Open");
        // A doubled ampersand is a literal one — "AT&&T" is a menu item called AT&T.
        assert_eq!(clean_label("AT&&T"), "AT&T");
        assert_eq!(clean_label(""), "");
    }

    /// NC Trainer's menus, as they came back: the group-style mnemonic kept its letter and
    /// the dots stayed, so the path read "Project(P)/Import(I).../Project(P)".
    #[test]
    fn a_group_style_mnemonic_and_a_trailing_ellipsis_are_not_part_of_the_name() {
        assert_eq!(clean_label("Project(&P)"), "Project");
        assert_eq!(clean_label("Import(&I)..."), "Import");
        assert_eq!(clean_label("New Project(&N)...\tCtrl+N"), "New Project");
        assert_eq!(clean_label("Exit(&X)\tAlt+F4"), "Exit");
        assert_eq!(clean_label("100%(&H)"), "100%");
        assert_eq!(clean_label("ファイル（&F）"), "ファイル");
        assert_eq!(clean_label("Project (&P)"), "Project");
        assert_eq!(clean_label("Open…"), "Open");
        // A parenthesised part of the name stays; only the mnemonic group goes.
        assert_eq!(clean_label("中文(简体)(&S)"), "中文(简体)");
        assert_eq!(clean_label("Size (A4)"), "Size (A4)");
        // Not a mnemonic: a literal ampersand in parentheses.
        assert_eq!(clean_label("R(&&D)"), "R(&D)");
    }
}
