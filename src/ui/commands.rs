//! Every user action as a `Cmd`, the keyboard shortcuts that trigger them, and the menus that list them.

use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreatePopupMenu, HMENU, MENU_ITEM_FLAGS, MF_CHECKED, MF_GRAYED, MF_MENUBARBREAK, MF_POPUP, MF_SEPARATOR,
    MF_STRING,
};
use windows::core::HSTRING;

use crate::convert::Conv;
use crate::shortcuts::{Combo, Key, Keymap};

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
    /// File → Print… (the system's print dialog).
    Print,
    /// File → Print preview… (`preview`).
    PrintPreview,
    /// File → Page setup… (Windows' dialog: paper, orientation, margins).
    PageSetup,
    Reload,
    RevealFile,
    CopyPath,
    CloseTab,
    CloseTabAt(usize),
    CloseOthers,
    CloseRight,
    CloseAll,
    /// Close the tabs without unsaved changes.
    CloseSaved,
    /// Open the tab closed last again (Ctrl+Shift+T).
    ReopenClosed,
    /// Show the tab with this id (from the list of all tabs).
    ShowTab(u64),
    /// Keep the tabs and unsaved text for next time (settings.json's `restore_session`).
    ToggleRestoreSession,
    /// File → Read only: the tab's text can't be changed.
    ToggleReadOnly,
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
    /// Edit → Go to matching bracket (Ctrl+M) and Select inside brackets (Ctrl+Shift+M): `edit::bracket_jump`,
    /// `edit::bracket_select`.
    GoToBracket,
    SelectBrackets,
    /// View → Show only matching lines (false) / lines not matching (true): the find bar's search, the lines in a tab
    /// of their own.
    FilterLines(bool),
    /// View → Clear highlights: no search highlighted any more (the find box's Highlight).
    ClearHighlights,
    /// Bookmarks: the caret's line marked or not (Ctrl+F2), the next and previous (F2, Shift+F2), none.
    ToggleBookmark,
    NextBookmark,
    PrevBookmark,
    ClearBookmarks,
    DuplicateLine,
    DeleteLine,
    MoveLineUp,
    MoveLineDown,
    InsertDateTime,
    Indent,
    Outdent,
    ToggleWrap,
    ToggleLineNumbers,
    ToggleWhitespace,
    ToggleSpellCheck,
    ToggleTitleBarTabs,
    /// View → Close Slate with the last tab (`Settings::close_with_last_tab`).
    ToggleCloseWithLastTab,
    /// View → Find bar: Close when you click in the text (`Settings::find_close_on_leave`), One for each tab
    /// (`Settings::find_per_tab`).
    ToggleFindCloseOnLeave,
    ToggleFindPerTab,
    /// Edit → Multiple cursors (`Settings::multi_cursor`; `crate::edit::Carets`): the setting, and adding and taking
    /// away cursors.
    ToggleMultiCursor,
    AddCaretAbove,
    AddCaretBelow,
    AddNextMatch,
    SkipMatch,
    AllMatches,
    LineStartCarets,
    LineEndCarets,
    RemoveLastCaret,
    /// The spell check menu's suggestion (`App::spell_word`).
    Suggestion(u8),
    AddWord,
    IgnoreWord,
    /// Typing replaces the character after the caret (Insert).
    ToggleOvertype,
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
    /// How many spaces formatting JSON or XML indents by (settings.json's `json_indent`).
    JsonIndent(u32),
    ToggleComment,
    Lines(LineOp),
    /// CSV and TSV: the rows sorted by the caret's column (Z to A: true).
    SortByColumn(bool),
    /// View → Character at the caret: its code point and bytes in the status bar.
    CharInfo,
    Case(CaseOp),
    /// Format → Convert: Base64, URL encoding, JSON escapes (on the selection, or the whole text), SHA-256.
    Convert(crate::convert::Conv),
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
    /// Help → Change keyboard shortcuts…: the user's file (`crate::shortcuts`), made the first time.
    ChangeShortcuts,
    MakeDefault,
    StopDefault,
    OpenDataFolder,
    ReportProblem,
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
        (VK_T, true, true) => Cmd::ReopenClosed,
        (VK_O, true, false) => Cmd::Open,
        (VK_S, true, false) => Cmd::Save,
        (VK_S, true, true) => Cmd::SaveAs,
        (VK_P, true, false) => Cmd::Print,
        (VK_W, true, false) | (VK_F4, true, false) => Cmd::CloseTab,
        (VK_F, true, false) => Cmd::Find,
        (VK_H, true, false) => Cmd::Replace,
        (VK_G, true, false) => Cmd::GoToLine,
        (VK_O, true, true) => Cmd::ToggleStructure,
        (VK_F3, false, false) => Cmd::FindNext,
        (VK_F3, false, true) => Cmd::FindPrev,
        (VK_F2, true, false) => Cmd::ToggleBookmark,
        (VK_F2, false, false) => Cmd::NextBookmark,
        (VK_F2, false, true) => Cmd::PrevBookmark,
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
            if m.alt && !m.ctrl && !m.shift && k == VK_L {
                return Some(Cmd::FilterLines(false));
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
    // (Shift+Alt+arrows: a column selection, `App::block_key`)
    if m.alt && !m.ctrl {
        return match k {
            VK_UP if !m.shift => Some(Cmd::MoveLineUp),
            VK_DOWN if !m.shift => Some(Cmd::MoveLineDown),
            VK_OEM_4 if !m.shift => Some(Cmd::LineStartCarets),
            VK_OEM_6 if !m.shift => Some(Cmd::LineEndCarets),
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
            (VK_M, false) => Cmd::GoToBracket,
            (VK_M, true) => Cmd::SelectBrackets,
            (VK_OEM_2, false) | (VK_DIVIDE, false) => Cmd::ToggleComment,
            (VK_U, false) => Cmd::Case(CaseOp::Lower),
            (VK_U, true) => Cmd::Case(CaseOp::Upper),
            (VK_N, true) => Cmd::AddNextMatch,
            (VK_A, true) => Cmd::AllMatches,
            (VK_UP, true) => Cmd::AddCaretAbove,
            (VK_DOWN, true) => Cmd::AddCaretBelow,
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
    Cmd { cmd: Cmd, label: String, keys: String, checked: bool, enabled: bool },
    Sep,
    Sub { label: String, items: Vec<Item> },
    /// The next items go in a new column.
    ColBreak,
}

pub fn item(cmd: Cmd, label: &str, keys: &'static str) -> Item {
    Item::Cmd { cmd, label: label.into(), keys: keys.into(), checked: false, enabled: true }
}

pub fn check(cmd: Cmd, label: &str, keys: &'static str, checked: bool) -> Item {
    Item::Cmd { cmd, label: label.into(), keys: keys.into(), checked, enabled: true }
}

pub fn enabled(cmd: Cmd, label: &str, keys: &'static str, on: bool) -> Item {
    Item::Cmd { cmd, label: label.into(), keys: keys.into(), checked: false, enabled: on }
}

pub fn sub(label: &str, items: Vec<Item>) -> Item {
    Item::Sub { label: label.into(), items }
}

/// The changeable commands (`crate::shortcuts`) by their names there.
const IDS: &[(&str, Cmd)] = &[
    ("new-tab", Cmd::NewTab),
    ("open", Cmd::Open),
    ("reopen-closed", Cmd::ReopenClosed),
    ("save", Cmd::Save),
    ("save-as", Cmd::SaveAs),
    ("save-all", Cmd::SaveAll),
    ("reload", Cmd::Reload),
    ("print", Cmd::Print),
    ("print-preview", Cmd::PrintPreview),
    ("page-setup", Cmd::PageSetup),
    ("close-tab", Cmd::CloseTab),
    ("close-others", Cmd::CloseOthers),
    ("close-right", Cmd::CloseRight),
    ("close-saved", Cmd::CloseSaved),
    ("close-all", Cmd::CloseAll),
    ("copy-path", Cmd::CopyPath),
    ("reveal-file", Cmd::RevealFile),
    ("read-only", Cmd::ToggleReadOnly),
    ("restore-session", Cmd::ToggleRestoreSession),
    ("clear-recent", Cmd::ClearRecent),
    ("quit", Cmd::Exit),
    ("undo", Cmd::Undo),
    ("redo", Cmd::Redo),
    ("cut", Cmd::Cut),
    ("copy", Cmd::Copy),
    ("paste", Cmd::Paste),
    ("select-all", Cmd::SelectAll),
    ("find", Cmd::Find),
    ("replace", Cmd::Replace),
    ("find-next", Cmd::FindNext),
    ("find-prev", Cmd::FindPrev),
    ("goto-line", Cmd::GoToLine),
    ("goto-bracket", Cmd::GoToBracket),
    ("select-brackets", Cmd::SelectBrackets),
    ("toggle-bookmark", Cmd::ToggleBookmark),
    ("next-bookmark", Cmd::NextBookmark),
    ("prev-bookmark", Cmd::PrevBookmark),
    ("clear-bookmarks", Cmd::ClearBookmarks),
    ("toggle-comment", Cmd::ToggleComment),
    ("duplicate-line", Cmd::DuplicateLine),
    ("delete-line", Cmd::DeleteLine),
    ("move-line-up", Cmd::MoveLineUp),
    ("move-line-down", Cmd::MoveLineDown),
    ("sort-asc", Cmd::Lines(LineOp::SortAsc)),
    ("sort-desc", Cmd::Lines(LineOp::SortDesc)),
    ("dedupe", Cmd::Lines(LineOp::Dedupe)),
    ("remove-blank", Cmd::Lines(LineOp::RemoveBlank)),
    ("trim", Cmd::Lines(LineOp::TrimTrailing)),
    ("upper", Cmd::Case(CaseOp::Upper)),
    ("lower", Cmd::Case(CaseOp::Lower)),
    ("title-case", Cmd::Case(CaseOp::Title)),
    ("date-time", Cmd::InsertDateTime),
    ("overtype", Cmd::ToggleOvertype),
    ("multi-cursor", Cmd::ToggleMultiCursor),
    ("add-cursor-above", Cmd::AddCaretAbove),
    ("add-cursor-below", Cmd::AddCaretBelow),
    ("add-next-match", Cmd::AddNextMatch),
    ("skip-match", Cmd::SkipMatch),
    ("all-matches", Cmd::AllMatches),
    ("cursors-at-line-starts", Cmd::LineStartCarets),
    ("cursors-at-line-ends", Cmd::LineEndCarets),
    ("remove-last-cursor", Cmd::RemoveLastCaret),
    ("wrap", Cmd::ToggleWrap),
    ("line-numbers", Cmd::ToggleLineNumbers),
    ("show-whitespace", Cmd::ToggleWhitespace),
    ("spell-check", Cmd::ToggleSpellCheck),
    ("title-bar-tabs", Cmd::ToggleTitleBarTabs),
    ("close-with-last-tab", Cmd::ToggleCloseWithLastTab),
    ("find-close-on-leave", Cmd::ToggleFindCloseOnLeave),
    ("find-per-tab", Cmd::ToggleFindPerTab),
    ("char-info", Cmd::CharInfo),
    ("filter-lines", Cmd::FilterLines(false)),
    ("filter-lines-not", Cmd::FilterLines(true)),
    ("clear-highlights", Cmd::ClearHighlights),
    ("structure-panel", Cmd::ToggleStructure),
    ("path-bar", Cmd::TogglePathBar),
    ("zoom-in", Cmd::ZoomIn),
    ("zoom-out", Cmd::ZoomOut),
    ("zoom-reset", Cmd::ZoomReset),
    ("theme-system", Cmd::Theme(ThemeMode::System)),
    ("theme-light", Cmd::Theme(ThemeMode::Light)),
    ("theme-dark", Cmd::Theme(ThemeMode::Dark)),
    ("next-tab", Cmd::NextTab),
    ("prev-tab", Cmd::PrevTab),
    ("format", Cmd::Format),
    ("minify", Cmd::Minify),
    ("check", Cmd::Validate),
    ("csv-sort", Cmd::SortByColumn(false)),
    ("csv-sort-desc", Cmd::SortByColumn(true)),
    ("copy-json-path", Cmd::CopyJsonPath),
    ("eol-lf", Cmd::SetEol(Eol::Lf)),
    ("eol-crlf", Cmd::SetEol(Eol::Crlf)),
    ("convert-base64-encode", Cmd::Convert(Conv::Base64Encode)),
    ("convert-base64-decode", Cmd::Convert(Conv::Base64Decode)),
    ("convert-url-encode", Cmd::Convert(Conv::UrlEncode)),
    ("convert-url-decode", Cmd::Convert(Conv::UrlDecode)),
    ("convert-json-escape", Cmd::Convert(Conv::JsonEscape)),
    ("convert-json-unescape", Cmd::Convert(Conv::JsonUnescape)),
    ("convert-sha256", Cmd::Convert(Conv::Sha256)),
    ("shortcuts", Cmd::Shortcuts),
    ("change-shortcuts", Cmd::ChangeShortcuts),
    ("open-data-folder", Cmd::OpenDataFolder),
    ("make-default", Cmd::MakeDefault),
    ("stop-default", Cmd::StopDefault),
    ("check-updates", Cmd::CheckUpdates),
    ("report-problem", Cmd::ReportProblem),
    ("about", Cmd::About),
];

pub fn cmd_of(id: &str) -> Option<Cmd> {
    IDS.iter().find(|(i, _)| *i == id).map(|(_, c)| *c)
}

pub fn id_of(cmd: Cmd) -> Option<&'static str> {
    IDS.iter().find(|(_, c)| *c == cmd).map(|(i, _)| *i)
}

/// A key pressed as `crate::shortcuts` writes it (None: one it doesn't name, or AltGr typing a character).
pub fn combo(vk: u16, m: &Mods) -> Option<Combo> {
    if m.ctrl && m.alt && makes_char(vk, m) {
        return None;
    }
    let k = VIRTUAL_KEY(vk);
    let key = match k {
        _ if (VK_A.0..=VK_Z.0).contains(&vk) || (VK_0.0..=VK_9.0).contains(&vk) => Key::Char(vk as u8),
        _ if (VK_F1.0..=VK_F24.0).contains(&vk) => Key::F((vk - VK_F1.0 + 1) as u8),
        _ if (VK_NUMPAD0.0..=VK_NUMPAD9.0).contains(&vk) => Key::Num((vk - VK_NUMPAD0.0) as u8),
        VK_UP => Key::Up,
        VK_DOWN => Key::Down,
        VK_LEFT => Key::Left,
        VK_RIGHT => Key::Right,
        VK_HOME => Key::Home,
        VK_END => Key::End,
        VK_PRIOR => Key::PageUp,
        VK_NEXT => Key::PageDown,
        VK_INSERT => Key::Insert,
        VK_DELETE => Key::Delete,
        VK_BACK => Key::Backspace,
        VK_RETURN => Key::Enter,
        VK_TAB => Key::Tab,
        VK_ESCAPE => Key::Esc,
        VK_SPACE => Key::Space,
        VK_ADD => Key::NumPlus,
        VK_SUBTRACT => Key::NumMinus,
        VK_DIVIDE => Key::NumSlash,
        VK_OEM_2 => Key::Char(b'/'),
        VK_OEM_5 => Key::Char(b'\\'),
        VK_OEM_4 => Key::Char(b'['),
        VK_OEM_6 => Key::Char(b']'),
        VK_OEM_1 => Key::Char(b';'),
        VK_OEM_7 => Key::Char(b'\''),
        VK_OEM_COMMA => Key::Char(b','),
        VK_OEM_PERIOD => Key::Char(b'.'),
        VK_OEM_MINUS => Key::Char(b'-'),
        VK_OEM_PLUS => Key::Char(b'='),
        VK_OEM_3 => Key::Char(b'`'),
        _ => return None,
    };
    Some(Combo { ctrl: m.ctrl, alt: m.alt, shift: m.shift, key })
}

/// A menu with the user's keys shown for the commands that have them (`crate::shortcuts`).
pub fn keyed(items: Vec<Item>, km: &Keymap) -> Vec<Item> {
    if km.is_empty() {
        return items;
    }
    items
        .into_iter()
        .map(|it| match it {
            Item::Cmd { cmd, label, keys, checked, enabled } => {
                let keys = id_of(cmd).and_then(|id| km.menu_text(id)).unwrap_or(keys);
                Item::Cmd { cmd, label, keys, checked, enabled }
            }
            Item::Sub { label, items } => Item::Sub { label, items: keyed(items, km) },
            other => other,
        })
        .collect()
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
Keyboard shortcuts (Help → Change keyboard shortcuts… gives commands other keys)

Files
  Ctrl+N / Ctrl+T               New tab
  Ctrl+O                        Open
  Ctrl+S                        Save
  Ctrl+Shift+S                  Save as
  Ctrl+Alt+S                    Save all
  Ctrl+P                        Print
  Ctrl+W / Ctrl+F4              Close tab
  Ctrl+Shift+T                  Reopen the tab closed last
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
  Alt+drag, Shift+Alt+arrows    Column selection (type, paste, delete on every line)
  Ctrl+click                    Add or take away a cursor (with Edit → Multiple cursors on)
  Ctrl+Shift+Up / Ctrl+Shift+Down  A cursor above / below (multiple cursors)
  Ctrl+Shift+N / Ctrl+Shift+A   The next match / every match too (multiple cursors)
  Alt+[ / Alt+]                 A cursor at the start / end of every selected line (multiple cursors)
  Esc                           Back to one cursor
  Tab / Shift+Tab               Indent / outdent the selected lines
  Ctrl+/                        Toggle comment
  Ctrl+U / Ctrl+Shift+U         lowercase / UPPERCASE
  Ctrl+Backspace / Ctrl+Del     Delete the word before / after the caret
  Insert                        Overtype on / off (typing replaces the next character)
  F5                            Insert the time and date
  Shift+Alt+F                   Format JSON or XML

Moving around
  Ctrl+Left / Ctrl+Right        Previous / next word (with Shift: select)
  Home / End                    Start / end of the line (Home: first the text, then the edge)
  Ctrl+Home / Ctrl+End          Start / end of the document
  PgUp / PgDn                   Page up / down
  Ctrl+Up / Ctrl+Down           Scroll without moving the caret
  Ctrl+G                        Go to line (line:column works too)
  Ctrl+M                        Go to the matching bracket (again: back)
  Ctrl+Shift+M                  Select inside brackets (again: the brackets too, then the
                                pair around them)
  Ctrl+F2                       Bookmark the line (or take the bookmark off)
  F2 / Shift+F2                 Next / previous bookmark

Find and replace
  Ctrl+F / Ctrl+H               Find / Replace
  F3 / Shift+F3                 Next / previous match
  Enter / Shift+Enter           Next / previous match (in the find box)
  Alt+C / Alt+W / Alt+R         Match case / Whole word / Regular expression
  Alt+S                         Find in selection (what was selected when the bar opened)
  Alt+A                         Next / previous match through all open tabs (on or off)
  Alt+H                         Highlight the search in every tab, in a color of its own
  Up / Down                     The searches (or replacements) before, in the find box
  \\n  \\t                      A line break, a tab, in a regular expression's replacement
  Enter                         Replace this match (in the replace box)
  Ctrl+Alt+Enter                Replace all (in the replace box)
  Tab                           Switch between the find and replace boxes
  Alt+L                         Show only the lines with a match, in a tab of their own
                                (Enter or a double-click there goes to a line in the file)
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

#[cfg(test)]
mod tests {
    use super::*;

    /// The key a `Combo` is pressed with here (the inverse of `combo`).
    fn vk_of(k: Key) -> u16 {
        match k {
            Key::Char(c) if c.is_ascii_alphanumeric() => c as u16,
            Key::Char(c) => match c {
                b'/' => VK_OEM_2.0,
                b'\\' => VK_OEM_5.0,
                b'[' => VK_OEM_4.0,
                b']' => VK_OEM_6.0,
                b';' => VK_OEM_1.0,
                b'\'' => VK_OEM_7.0,
                b',' => VK_OEM_COMMA.0,
                b'.' => VK_OEM_PERIOD.0,
                b'-' => VK_OEM_MINUS.0,
                b'=' => VK_OEM_PLUS.0,
                _ => VK_OEM_3.0,
            },
            Key::F(n) => VK_F1.0 + n as u16 - 1,
            Key::Num(n) => VK_NUMPAD0.0 + n as u16,
            Key::Up => VK_UP.0,
            Key::Down => VK_DOWN.0,
            Key::Left => VK_LEFT.0,
            Key::Right => VK_RIGHT.0,
            Key::Home => VK_HOME.0,
            Key::End => VK_END.0,
            Key::PageUp => VK_PRIOR.0,
            Key::PageDown => VK_NEXT.0,
            Key::Insert => VK_INSERT.0,
            Key::Delete => VK_DELETE.0,
            Key::Backspace => VK_BACK.0,
            Key::Enter => VK_RETURN.0,
            Key::Tab => VK_TAB.0,
            Key::Esc => VK_ESCAPE.0,
            Key::Space => VK_SPACE.0,
            Key::NumPlus => VK_ADD.0,
            Key::NumMinus => VK_SUBTRACT.0,
            Key::NumSlash => VK_DIVIDE.0,
        }
    }

    #[test]
    fn the_listed_keys_are_the_keys_slate_has() {
        for c in crate::shortcuts::commands() {
            let cmd = cmd_of(c.id).unwrap_or_else(|| panic!("no Cmd for {}", c.id));
            assert_eq!(id_of(cmd), Some(c.id));
            for k in crate::shortcuts::default_keys(c) {
                let vk = vk_of(k.key);
                let m = Mods { ctrl: k.ctrl, shift: k.shift, alt: k.alt };
                assert_eq!(combo(vk, &m), Some(k), "{}", k.text());
                let got = if c.anywhere { global_key(vk, &m) } else { editor_key(vk, &m) };
                assert_eq!(got, Some(cmd), "{} for {}", k.text(), c.id);
            }
        }
    }
}
