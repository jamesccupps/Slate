//! Every user action as a `Cmd`, the keyboard shortcuts that trigger them, and the menus that list them.

use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreatePopupMenu, HMENU, MENU_ITEM_FLAGS, MF_CHECKED, MF_GRAYED, MF_MENUBARBREAK, MF_POPUP, MF_SEPARATOR,
    MF_STRING,
};
use windows::core::HSTRING;

use crate::core::lines::{CaseOp, LineOp};
use crate::core::text::{Encoding, Eol};

use super::highlight::Lang;
use super::settings::ThemeMode;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cmd {
    NewTab,
    Open,
    OpenRecent(usize),
    ClearRecent,
    Save,
    SaveAs,
    SaveAll,
    Reload,
    RevealFile,
    CopyPath,
    CloseTab,
    CloseTabAt(usize),
    CloseOthers,
    CloseRight,
    CloseAll,
    Exit,
    Undo,
    Redo,
    Cut,
    Copy,
    Paste,
    Delete,
    SelectAll,
    Find,
    FindNext,
    FindPrev,
    Replace,
    GoToLine,
    DuplicateLine,
    DeleteLine,
    MoveLineUp,
    MoveLineDown,
    InsertDateTime,
    Indent,
    Outdent,
    ToggleWrap,
    ToggleLineNumbers,
    ToggleStructure,
    TogglePathBar,
    CopyJsonPath,
    ZoomIn,
    ZoomOut,
    ZoomReset,
    Font(u16),
    FontSize(u8),
    Theme(ThemeMode),
    /// Format / minify / check the document (JSON or XML).
    Format,
    Minify,
    Validate,
    ToggleComment,
    Lines(LineOp),
    Case(CaseOp),
    SetEol(Eol),
    SaveEncoding(Encoding),
    ReopenEncoding(Encoding),
    SetLang(Lang),
    IndentSpaces(bool),
    TabSize(u32),
    NextTab,
    PrevTab,
    ActivateTab(usize),
    About,
    Shortcuts,
    MakeDefault,
    OpenDataFolder,
    CheckUpdates,
    Update,
    ToggleAutoUpdate,
}

pub struct Mods {
    pub ctrl: bool,
    pub shift: bool,
    pub alt: bool,
}

thread_local! {
    /// Test mode: pretend these modifier keys (ctrl, shift, alt) are held.
    pub static FORCED_MODS: std::cell::Cell<Option<(bool, bool, bool)>> = const { std::cell::Cell::new(None) };
    /// Test mode: pretend the keyboard layout types a character for every letter with AltGr (Ctrl+Alt), like Polish.
    pub static FORCED_ALTGR: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

pub fn mods() -> Mods {
    if let Some((ctrl, shift, alt)) = FORCED_MODS.with(|m| m.get()) {
        return Mods { ctrl, shift, alt };
    }
    unsafe {
        Mods {
            ctrl: GetKeyState(VK_CONTROL.0 as i32) < 0,
            shift: GetKeyState(VK_SHIFT.0 as i32) < 0,
            alt: GetKeyState(VK_MENU.0 as i32) < 0,
        }
    }
}

/// Whether `vk` types a character with these modifiers in the current keyboard layout. Windows reports AltGr as
/// Ctrl+Alt, so on many layouts Ctrl+Alt+letter is a letter (Polish AltGr+S is "ś"), not a shortcut.
pub fn makes_char(vk: u16, m: &Mods) -> bool {
    if FORCED_ALTGR.with(|f| f.get()) {
        return m.ctrl && m.alt && (VK_A.0..=VK_Z.0).contains(&vk);
    }
    unsafe {
        let mut state = [0u8; 256];
        let mut hold = |k: VIRTUAL_KEY| state[k.0 as usize] = 0x80;
        if m.ctrl {
            hold(VK_CONTROL);
            hold(VK_LCONTROL);
        }
        if m.alt {
            hold(VK_MENU);
            hold(VK_RMENU);
        }
        if m.shift {
            hold(VK_SHIFT);
            hold(VK_LSHIFT);
        }
        state[VK_CAPITAL.0 as usize] = (GetKeyState(VK_CAPITAL.0 as i32) & 1) as u8;
        let layout = GetKeyboardLayout(0);
        let scan = MapVirtualKeyExW(vk as u32, MAPVK_VK_TO_VSC, layout);
        let mut buf = [0u16; 8];
        // Flag 4: leave the keyboard state alone, so a pending dead key still combines with the next key.
        let n = ToUnicodeEx(vk as u32, scan, &state, &mut buf, 4, layout);
        // A dead key (n < 0) types a character too; control characters (Ctrl+Enter...) don't count.
        n < 0 || buf[..n.max(0) as usize].iter().any(|&c| c >= 0x20)
    }
}

/// Shortcuts that work everywhere (also while typing in the find box).
pub fn global_key(vk: u16, m: &Mods) -> Option<Cmd> {
    if m.ctrl && m.alt && makes_char(vk, m) {
        // AltGr+key types a character.
        return None;
    }
    let k = VIRTUAL_KEY(vk);
    let c = m.ctrl && !m.alt;
    Some(match (k, c, m.shift) {
        (VK_N, true, false) => Cmd::NewTab,
        (VK_T, true, false) => Cmd::NewTab,
        (VK_O, true, false) => Cmd::Open,
        (VK_S, true, false) => Cmd::Save,
        (VK_S, true, true) => Cmd::SaveAs,
        (VK_W, true, false) | (VK_F4, true, false) => Cmd::CloseTab,
        (VK_F, true, false) => Cmd::Find,
        (VK_H, true, false) => Cmd::Replace,
        (VK_G, true, false) => Cmd::GoToLine,
        (VK_O, true, true) => Cmd::ToggleStructure,
        (VK_F3, false, false) => Cmd::FindNext,
        (VK_F3, false, true) => Cmd::FindPrev,
        (VK_TAB, true, false) | (VK_NEXT, true, false) => Cmd::NextTab,
        (VK_TAB, true, true) | (VK_PRIOR, true, false) => Cmd::PrevTab,
        (VK_OEM_PLUS, true, _) | (VK_ADD, true, _) => Cmd::ZoomIn,
        (VK_OEM_MINUS, true, _) | (VK_SUBTRACT, true, _) => Cmd::ZoomOut,
        (VK_0, true, false) | (VK_NUMPAD0, true, false) => Cmd::ZoomReset,
        _ => {
            if m.ctrl && m.alt && k == VK_S {
                return Some(Cmd::SaveAll);
            }
            if m.alt && !m.ctrl && k == VK_Z {
                return Some(Cmd::ToggleWrap);
            }
            if m.alt && m.shift && !m.ctrl && k == VK_F {
                return Some(Cmd::Format);
            }
            if c && !m.shift && (VK_1.0..=VK_9.0).contains(&k.0) {
                return Some(Cmd::ActivateTab((k.0 - VK_1.0) as usize));
            }
            return None;
        }
    })
}

/// Shortcuts for the text area.
pub fn editor_key(vk: u16, m: &Mods) -> Option<Cmd> {
    let k = VIRTUAL_KEY(vk);
    if m.alt && !m.ctrl {
        return match k {
            VK_UP => Some(Cmd::MoveLineUp),
            VK_DOWN => Some(Cmd::MoveLineDown),
            _ => None,
        };
    }
    if m.ctrl && !m.alt {
        return Some(match (k, m.shift) {
            (VK_Z, false) => Cmd::Undo,
            (VK_Y, false) | (VK_Z, true) => Cmd::Redo,
            (VK_X, false) => Cmd::Cut,
            (VK_C, false) | (VK_INSERT, false) => Cmd::Copy,
            (VK_V, false) => Cmd::Paste,
            (VK_A, false) => Cmd::SelectAll,
            (VK_D, false) => Cmd::DuplicateLine,
            (VK_K, true) => Cmd::DeleteLine,
            (VK_OEM_2, false) | (VK_DIVIDE, false) => Cmd::ToggleComment,
            (VK_U, false) => Cmd::Case(CaseOp::Lower),
            (VK_U, true) => Cmd::Case(CaseOp::Upper),
            _ => return None,
        });
    }
    match (k, m.shift) {
        (VK_F5, false) => Some(Cmd::InsertDateTime),
        (VK_INSERT, true) => Some(Cmd::Paste),
        (VK_DELETE, true) => Some(Cmd::Cut),
        _ => None,
    }
}

pub enum Item {
    Cmd { cmd: Cmd, label: String, keys: &'static str, checked: bool, enabled: bool },
    Sep,
    Sub { label: String, items: Vec<Item> },
    /// The next items go in a new column.
    ColBreak,
}

pub fn item(cmd: Cmd, label: &str, keys: &'static str) -> Item {
    Item::Cmd { cmd, label: label.into(), keys, checked: false, enabled: true }
}

pub fn check(cmd: Cmd, label: &str, keys: &'static str, checked: bool) -> Item {
    Item::Cmd { cmd, label: label.into(), keys, checked, enabled: true }
}

pub fn enabled(cmd: Cmd, label: &str, keys: &'static str, on: bool) -> Item {
    Item::Cmd { cmd, label: label.into(), keys, checked: false, enabled: on }
}

pub fn sub(label: &str, items: Vec<Item>) -> Item {
    Item::Sub { label: label.into(), items }
}

/// Builds a native popup menu; menu ids index into `ids`.
pub fn build_menu(items: &[Item], ids: &mut Vec<Cmd>) -> HMENU {
    unsafe {
        let m = CreatePopupMenu().expect("menu");
        let mut col_break = false;
        for it in items {
            match it {
                Item::ColBreak => col_break = true,
                Item::Sep => {
                    let _ = AppendMenuW(m, MF_SEPARATOR, 0, None);
                }
                Item::Cmd { cmd, label, keys, checked, enabled } => {
                    ids.push(*cmd);
                    let text = if keys.is_empty() { label.clone() } else { format!("{label}\t{keys}") };
                    let mut flags = MF_STRING;
                    if std::mem::take(&mut col_break) {
                        flags |= MF_MENUBARBREAK;
                    }
                    if *checked {
                        flags |= MF_CHECKED;
                    }
                    if !*enabled {
                        flags |= MF_GRAYED;
                    }
                    let _ = AppendMenuW(m, flags, ids.len(), &HSTRING::from(text));
                }
                Item::Sub { label, items } => {
                    let s = build_menu(items, ids);
                    let _ = AppendMenuW(m, MENU_ITEM_FLAGS(MF_POPUP.0 | MF_STRING.0), s.0 as usize, &HSTRING::from(label.as_str()));
                }
            }
        }
        m
    }
}

pub const MENU_TITLES: [&str; 5] = ["File", "Edit", "View", "Format", "Help"];
/// The letter of each title that opens it with Alt (F, E, V, O, H), underlined while the menu bar has the keyboard.
pub const MENU_KEYS: [u32; 5] = [0, 0, 0, 1, 0];

/// Menu bar shortcut letters (Alt+F etc.).
pub fn menu_for_letter(vk: u16) -> Option<usize> {
    match VIRTUAL_KEY(vk) {
        VK_F => Some(0),
        VK_E => Some(1),
        VK_V => Some(2),
        VK_O => Some(3),
        VK_H => Some(4),
        _ => None,
    }
}

pub const SHORTCUTS: &str = "\
Keyboard shortcuts

Files
  Ctrl+N / Ctrl+T               New tab
  Ctrl+O                        Open
  Ctrl+S                        Save
  Ctrl+Shift+S                  Save as
  Ctrl+Alt+S                    Save all
  Ctrl+W / Ctrl+F4              Close tab
  Ctrl+Tab / Ctrl+PgDn          Next tab
  Ctrl+Shift+Tab / Ctrl+PgUp    Previous tab
  Ctrl+1 … Ctrl+8               Go to that tab
  Ctrl+9                        Go to the last tab
  Alt+F4                        Exit (unsaved work comes back next time)

Editing
  Ctrl+Z / Ctrl+Y               Undo / Redo (Ctrl+Shift+Z redoes too)
  Ctrl+X / Ctrl+C / Ctrl+V      Cut / Copy / Paste (nothing selected: the whole line)
  Shift+Del / Shift+Ins         Cut / Paste
  Ctrl+Ins                      Copy
  Ctrl+A                        Select all
  Ctrl+D                        Duplicate the line (or the selection)
  Ctrl+Shift+K                  Delete the line
  Alt+Up / Alt+Down             Move the line up / down
  Tab / Shift+Tab               Indent / outdent the selected lines
  Ctrl+/                        Comment / uncomment the lines
  Ctrl+U / Ctrl+Shift+U         lowercase / UPPERCASE
  Ctrl+Backspace / Ctrl+Del     Delete the word before / after the caret
  F5                            Insert the time and date
  Shift+Alt+F                   Format JSON or XML

Moving around
  Ctrl+Left / Ctrl+Right        Previous / next word (with Shift: select)
  Home / End                    Start / end of the line (Home: first the text, then the edge)
  Ctrl+Home / Ctrl+End          Start / end of the document
  PgUp / PgDn                   Page up / down
  Ctrl+Up / Ctrl+Down           Scroll without moving the caret
  Ctrl+G                        Go to line (line:column works too)

Find and replace
  Ctrl+F / Ctrl+H               Find / Replace
  F3 / Shift+F3                 Next / previous match
  Enter / Shift+Enter           Next / previous match (in the find box)
  Alt+C / Alt+W / Alt+R         Match case / Whole word / Regular expression
  Enter                         Replace this match (in the replace box)
  Ctrl+Alt+Enter                Replace all (in the replace box)
  Tab                           Switch between the find and replace boxes
  Esc                           Close the find bar

View
  Alt+Z                         Word wrap
  Ctrl+Plus / Ctrl+Minus        Zoom in / out (or Ctrl+mouse wheel)
  Ctrl+0                        Reset zoom
  Ctrl+Shift+O                  JSON and XML structure panel

Menus
  Alt, then a letter            Open a menu (or Alt+F, Alt+E, Alt+V, Alt+O, Alt+H)
  F10                           File menu
  Shift+F10 / Menu key          Context menu

Mouse
  Double-click / triple-click   Select a word / a line
  Click a line number           Select the line (drag for more)
  Shift+wheel                   Scroll sideways (when word wrap is off)";
