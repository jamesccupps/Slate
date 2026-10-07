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

/// Shortcuts that work everywhere (also while typing in the find box).
pub fn global_key(vk: u16, m: &Mods) -> Option<Cmd> {
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
Files
  Ctrl+N / Ctrl+T    New tab
  Ctrl+O             Open
  Ctrl+S             Save
  Ctrl+Shift+S       Save as
  Ctrl+Alt+S         Save all
  Ctrl+W             Close tab
  Ctrl+Tab           Next tab
  Ctrl+1 … 9         Go to tab

Editing
  Ctrl+Z / Ctrl+Y    Undo / Redo
  Ctrl+X / C / V     Cut / Copy / Paste (no selection: the whole line)
  Ctrl+D             Duplicate line
  Ctrl+Shift+K       Delete line
  Alt+Up / Down      Move line up / down
  Tab / Shift+Tab    Indent / outdent selected lines
  Ctrl+/             Comment / uncomment lines
  Ctrl+Shift+U       UPPERCASE (Ctrl+U: lowercase)
  Ctrl+Backspace     Delete previous word
  F5                 Insert time and date

Find
  Ctrl+F             Find
  Ctrl+H             Replace
  F3 / Shift+F3      Next / previous match
  Ctrl+G             Go to line

View
  Alt+Z              Word wrap
  Ctrl+Plus / Minus  Zoom (or Ctrl+mouse wheel)
  Ctrl+0             Reset zoom
  Shift+Alt+F        Format JSON or XML";
