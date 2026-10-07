//! Small Windows helpers: dark title bar and menus, clipboard, file dialogs, prompts, Explorer.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::path::{Path, PathBuf};

use windows::Win32::Foundation::{BOOL, HANDLE, HGLOBAL, HWND, LPARAM, WPARAM};
use windows::Win32::Graphics::Dwm::{DWMWINDOWATTRIBUTE, DwmSetWindowAttribute};
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance, CoTaskMemFree};
use windows::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, GetClipboardData, IsClipboardFormatAvailable, OpenClipboard, SetClipboardData,
};
use windows::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};
use windows::Win32::System::Memory::{GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalSize, GlobalUnlock};
use windows::Win32::System::Ole::CF_UNICODETEXT;
use windows::Win32::UI::Controls::{
    TASKDIALOG_BUTTON, TASKDIALOG_COMMON_BUTTON_FLAGS, TASKDIALOG_FLAGS, TASKDIALOGCONFIG, TDF_ALLOW_DIALOG_CANCELLATION,
    TDF_POSITION_RELATIVE_TO_WINDOW, TDF_SIZE_TO_CONTENT, TaskDialogIndirect,
};
use windows::Win32::UI::Shell::Common::COMDLG_FILTERSPEC;
use windows::Win32::UI::Shell::{
    FOS_ALLOWMULTISELECT, FOS_FILEMUSTEXIST, FOS_FORCEFILESYSTEM, FOS_OVERWRITEPROMPT, FOS_PATHMUSTEXIST,
    FileOpenDialog, FileSaveDialog, IFileOpenDialog, IFileSaveDialog, ILCreateFromPathW, ILFree, IShellItem,
    SHCreateItemFromParsingName, SHOpenFolderAndSelectItems, SIGDN_FILESYSPATH,
};
use windows::Win32::UI::WindowsAndMessaging::{
    IDCANCEL, MB_ICONINFORMATION, MB_OK, MESSAGEBOX_STYLE, MessageBoxW, SendMessageW, WM_SETICON,
};
use windows::core::{HSTRING, PCSTR, PCWSTR, PWSTR, w};

// ---- test mode ----

/// Test mode: prompts are answered from the script instead of shown, and the clipboard lives here instead of on
/// the real one, so tests never pop up a dialog or touch the user's clipboard.
#[derive(Default)]
pub struct Scripted {
    /// Button index for each coming prompt (None = cancel; also when the queue is empty).
    pub answers: VecDeque<Option<usize>>,
    /// The prompts that were "shown".
    pub asked: Vec<String>,
    pub clipboard: Option<Vec<u8>>,
}

thread_local! {
    pub static SCRIPTED: RefCell<Option<Scripted>> = const { RefCell::new(None) };
}

fn scripted<R>(f: impl FnOnce(&mut Scripted) -> R) -> Option<R> {
    SCRIPTED.with(|s| s.borrow_mut().as_mut().map(f))
}

// ---- dark mode ----

const DWMWA_USE_IMMERSIVE_DARK_MODE: DWMWINDOWATTRIBUTE = DWMWINDOWATTRIBUTE(20);
const DWMWA_BORDER_COLOR: DWMWINDOWATTRIBUTE = DWMWINDOWATTRIBUTE(34);
const DWMWA_CAPTION_COLOR: DWMWINDOWATTRIBUTE = DWMWINDOWATTRIBUTE(35);
const DWMWA_TEXT_COLOR: DWMWINDOWATTRIBUTE = DWMWINDOWATTRIBUTE(36);

/// 0xFFRRGGBB → COLORREF (0x00BBGGRR).
pub fn colorref(argb: u32) -> u32 {
    ((argb & 0xFF) << 16) | (argb & 0xFF00) | ((argb >> 16) & 0xFF)
}

/// Dark or light title bar, colored like the tab strip under it (Windows 11; ignored on Windows 10).
pub fn style_title_bar(hwnd: HWND, dark: bool, caption: u32, text: u32) {
    unsafe {
        let d = BOOL(dark as i32);
        let _ = DwmSetWindowAttribute(hwnd, DWMWA_USE_IMMERSIVE_DARK_MODE, &d as *const _ as _, 4);
        let c = colorref(caption);
        let _ = DwmSetWindowAttribute(hwnd, DWMWA_CAPTION_COLOR, &c as *const _ as _, 4);
        let _ = DwmSetWindowAttribute(hwnd, DWMWA_BORDER_COLOR, &c as *const _ as _, 4);
        let t = colorref(text);
        let _ = DwmSetWindowAttribute(hwnd, DWMWA_TEXT_COLOR, &t as *const _ as _, 4);
    }
}

/// Dark popup menus. Uses uxtheme's unnamed exports (ordinals 135 and 136), which Windows has kept stable since
/// 1903 and which many apps (Notepad++ among them) rely on; silently does nothing if they're missing.
pub fn set_menu_dark(dark: bool) {
    unsafe {
        let Ok(lib) = LoadLibraryW(w!("uxtheme.dll")) else { return };
        // SetPreferredAppMode(mode): 0 default, 1 allow dark, 2 force dark, 3 force light
        if let Some(f) = GetProcAddress(lib, PCSTR(135usize as *const u8)) {
            let f: extern "system" fn(i32) -> i32 = std::mem::transmute(f);
            f(if dark { 2 } else { 3 });
        }
        if let Some(f) = GetProcAddress(lib, PCSTR(136usize as *const u8)) {
            let f: extern "system" fn() = std::mem::transmute(f);
            f();
        }
    }
}

// ---- clipboard ----

fn open_clipboard(hwnd: HWND) -> bool {
    for _ in 0..10 {
        if unsafe { OpenClipboard(hwnd) }.is_ok() {
            return true;
        }
        std::thread::sleep(std::time::Duration::from_millis(15));
    }
    false
}

pub fn clipboard_has_text() -> bool {
    if let Some(has) = scripted(|s| s.clipboard.is_some()) {
        return has;
    }
    unsafe { IsClipboardFormatAvailable(CF_UNICODETEXT.0 as u32) }.is_ok()
}

/// Clipboard text as UTF-8.
pub fn get_clipboard(hwnd: HWND) -> Option<Vec<u8>> {
    if let Some(text) = scripted(|s| s.clipboard.clone()) {
        return text;
    }
    if !open_clipboard(hwnd) {
        return None;
    }
    let result = unsafe {
        (|| {
            let h = GetClipboardData(CF_UNICODETEXT.0 as u32).ok()?;
            let g = HGLOBAL(h.0);
            let p = GlobalLock(g) as *const u16;
            if p.is_null() {
                return None;
            }
            let max = GlobalSize(g) / 2;
            let s = std::slice::from_raw_parts(p, max);
            let n = s.iter().position(|&c| c == 0).unwrap_or(max);
            let text = String::from_utf16_lossy(&s[..n]).into_bytes();
            let _ = GlobalUnlock(g);
            Some(text)
        })()
    };
    unsafe {
        let _ = CloseClipboard();
    }
    result
}

/// Puts UTF-8 text on the clipboard.
pub fn set_clipboard(hwnd: HWND, text: &[u8]) -> bool {
    if scripted(|s| s.clipboard = Some(text.to_vec())).is_some() {
        return true;
    }
    let wide: Vec<u16> = String::from_utf8_lossy(text).encode_utf16().chain(std::iter::once(0)).collect();
    if !open_clipboard(hwnd) {
        return false;
    }
    let ok = unsafe {
        (|| {
            EmptyClipboard().ok()?;
            let g = GlobalAlloc(GMEM_MOVEABLE, wide.len() * 2).ok()?;
            let p = GlobalLock(g) as *mut u16;
            if p.is_null() {
                return None;
            }
            std::ptr::copy_nonoverlapping(wide.as_ptr(), p, wide.len());
            let _ = GlobalUnlock(g);
            SetClipboardData(CF_UNICODETEXT.0 as u32, HANDLE(g.0)).ok()?;
            Some(())
        })()
        .is_some()
    };
    unsafe {
        let _ = CloseClipboard();
    }
    ok
}

// ---- file dialogs ----

fn item_path(item: &IShellItem) -> Option<PathBuf> {
    unsafe {
        let p: PWSTR = item.GetDisplayName(SIGDN_FILESYSPATH).ok()?;
        let s = p.to_string().ok();
        CoTaskMemFree(Some(p.0 as _));
        s.map(PathBuf::from)
    }
}

fn filters() -> Vec<(HSTRING, HSTRING)> {
    vec![
        ("All files (*.*)".into(), "*.*".into()),
        ("Text files (*.txt)".into(), "*.txt".into()),
        ("Data files (JSON, XML, YAML, CSV)".into(), "*.json;*.jsonl;*.ndjson;*.geojson;*.xml;*.yaml;*.yml;*.csv;*.tsv".into()),
        ("Log files (*.log)".into(), "*.log".into()),
        ("Config files".into(), "*.ini;*.cfg;*.conf;*.config;*.toml;*.properties;*.env;*.reg;*.inf".into()),
        ("Scripts".into(), "*.ps1;*.psm1;*.bat;*.cmd;*.sh;*.py;*.rb;*.lua;*.sql".into()),
        (
            "Web and code".into(),
            "*.html;*.htm;*.css;*.js;*.ts;*.tsx;*.jsx;*.php;*.c;*.h;*.cpp;*.hpp;*.cs;*.java;*.kt;*.go;*.rs;*.swift;*.md".into(),
        ),
    ]
}

/// The Open dialog (several files allowed).
pub fn open_dialog(hwnd: HWND, folder: Option<&Path>) -> Vec<PathBuf> {
    unsafe {
        let Ok(dlg) = CoCreateInstance::<_, IFileOpenDialog>(&FileOpenDialog, None, CLSCTX_INPROC_SERVER) else {
            return Vec::new();
        };
        let f = filters();
        let specs: Vec<COMDLG_FILTERSPEC> =
            f.iter().map(|(n, s)| COMDLG_FILTERSPEC { pszName: PCWSTR(n.as_ptr()), pszSpec: PCWSTR(s.as_ptr()) }).collect();
        let _ = dlg.SetFileTypes(&specs);
        let opts = dlg.GetOptions().unwrap_or_default();
        let _ = dlg.SetOptions(opts | FOS_ALLOWMULTISELECT | FOS_FILEMUSTEXIST | FOS_FORCEFILESYSTEM);
        if let Some(dir) = folder {
            if let Ok(item) = SHCreateItemFromParsingName::<_, _, IShellItem>(&HSTRING::from(dir.as_os_str()), None) {
                let _ = dlg.SetFolder(&item);
            }
        }
        if dlg.Show(hwnd).is_err() {
            return Vec::new();
        }
        let Ok(items) = dlg.GetResults() else { return Vec::new() };
        let n = items.GetCount().unwrap_or(0);
        (0..n).filter_map(|i| items.GetItemAt(i).ok()).filter_map(|it| item_path(&it)).collect()
    }
}

/// The Save As dialog.
pub fn save_dialog(hwnd: HWND, name: &str, folder: Option<&Path>) -> Option<PathBuf> {
    if scripted(|s| s.asked.push(format!("save as {name}"))).is_some() {
        return None;
    }
    unsafe {
        let dlg = CoCreateInstance::<_, IFileSaveDialog>(&FileSaveDialog, None, CLSCTX_INPROC_SERVER).ok()?;
        let f = filters();
        let specs: Vec<COMDLG_FILTERSPEC> =
            f.iter().map(|(n, s)| COMDLG_FILTERSPEC { pszName: PCWSTR(n.as_ptr()), pszSpec: PCWSTR(s.as_ptr()) }).collect();
        let _ = dlg.SetFileTypes(&specs);
        let opts = dlg.GetOptions().unwrap_or_default();
        let _ = dlg.SetOptions(opts | FOS_OVERWRITEPROMPT | FOS_PATHMUSTEXIST | FOS_FORCEFILESYSTEM);
        let _ = dlg.SetFileName(&HSTRING::from(name));
        if let Some(dir) = folder {
            if let Ok(item) = SHCreateItemFromParsingName::<_, _, IShellItem>(&HSTRING::from(dir.as_os_str()), None) {
                let _ = dlg.SetFolder(&item);
            }
        }
        dlg.Show(hwnd).ok()?;
        let item = dlg.GetResult().ok()?;
        item_path(&item)
    }
}

// ---- prompts ----

/// A task dialog with custom buttons; returns the index of the chosen button, or None if dismissed.
pub fn ask(hwnd: HWND, title: &str, main: &str, detail: &str, buttons: &[&str]) -> Option<usize> {
    if let Some(a) = scripted(|s| {
        s.asked.push(main.to_string());
        s.answers.pop_front().flatten()
    }) {
        return a;
    }
    let title_w = HSTRING::from(title);
    let main_w = HSTRING::from(main);
    let detail_w = HSTRING::from(detail);
    let labels: Vec<HSTRING> = buttons.iter().map(|b| HSTRING::from(*b)).collect();
    let btns: Vec<TASKDIALOG_BUTTON> = labels
        .iter()
        .enumerate()
        .map(|(i, l)| TASKDIALOG_BUTTON { nButtonID: 100 + i as i32, pszButtonText: PCWSTR(l.as_ptr()) })
        .collect();
    let cfg = TASKDIALOGCONFIG {
        cbSize: std::mem::size_of::<TASKDIALOGCONFIG>() as u32,
        hwndParent: hwnd,
        dwFlags: TASKDIALOG_FLAGS(
            TDF_ALLOW_DIALOG_CANCELLATION.0 | TDF_POSITION_RELATIVE_TO_WINDOW.0 | TDF_SIZE_TO_CONTENT.0,
        ),
        dwCommonButtons: TASKDIALOG_COMMON_BUTTON_FLAGS(0),
        pszWindowTitle: PCWSTR(title_w.as_ptr()),
        pszMainInstruction: PCWSTR(main_w.as_ptr()),
        pszContent: if detail.is_empty() { PCWSTR::null() } else { PCWSTR(detail_w.as_ptr()) },
        cButtons: btns.len() as u32,
        pButtons: btns.as_ptr(),
        nDefaultButton: 100,
        ..Default::default()
    };
    let mut pressed = 0i32;
    unsafe { TaskDialogIndirect(&cfg, Some(&mut pressed), None, None) }.ok()?;
    if pressed == IDCANCEL.0 || pressed < 100 {
        return None;
    }
    Some((pressed - 100) as usize)
}

pub fn info(hwnd: HWND, title: &str, text: &str) {
    if scripted(|s| s.asked.push(title.to_string())).is_some() {
        return;
    }
    unsafe {
        MessageBoxW(
            hwnd,
            &HSTRING::from(text),
            &HSTRING::from(title),
            MESSAGEBOX_STYLE(MB_OK.0 | MB_ICONINFORMATION.0),
        );
    }
}

// ---- Explorer ----

/// Opens Explorer with the file selected.
pub fn reveal_in_explorer(path: &Path) {
    unsafe {
        let p = HSTRING::from(path.as_os_str());
        let pidl = ILCreateFromPathW(PCWSTR(p.as_ptr()));
        if !pidl.is_null() {
            let _ = SHOpenFolderAndSelectItems(pidl, None, 0);
            ILFree(Some(pidl));
        }
    }
}

pub fn set_window_icons(hwnd: HWND, big: isize, small: isize) {
    unsafe {
        SendMessageW(hwnd, WM_SETICON, WPARAM(1), LPARAM(big));
        SendMessageW(hwnd, WM_SETICON, WPARAM(0), LPARAM(small));
    }
}
