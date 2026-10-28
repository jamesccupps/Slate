//! `Slate.exe --test <commands…>` (or `--test script.txt`, one command per line): drives the real app in a hidden
//! window and renders frames offscreen into PNG files, so the UI can be checked without touching the desktop.
//! Settings and the session are never written in this mode.
//!
//! With `SLATE_TEST_VISIBLE=1` the window is shown instead (on top, without taking the keyboard focus), it draws
//! through the real swap chain, and `shot` captures what is actually on screen, native edit boxes included. With
//! `SLATE_TEST_GPU=1` the hidden window draws through its own swap chain on the GPU, as the real one does (for timings
//! with `t:` marks; `shot` then goes offscreen for good).
//!
//! Commands: `size:1200x800`, `dpi:144` (as if the window moved to a monitor at that DPI: WM_DPICHANGED),
//! `theme:dark|light`, `open:<path>`, `type:<text>` (`\n`, `\r` and `\t` allowed),
//! `key:<combo>` (e.g. `ctrl+shift+k`, `enter`, `pagedown`, `apps`), `cmd:<Name>` (a menu command, e.g. `JsonFormat`),
//! `find:<text>`, `replace:<text>`, `goto:<line>`, `saveas:<path>`, `click:<x>,<y>`, `dblclick:<x>,<y>`,
//! `wheel:<rows>`, `wait:<ms>`, `jobs` (wait for background work), `checkdisk` (look for files changed on disk,
//! as the window does every 2 s; then `jobs`), `shot:<file.png>`, `print:<what>`
//! (`text`, `sel`, `status`, `lines`, `title`, `top`, `find`, `tabs`, `dirty`, `asked`, `clipboard`, `window`,
//! `saving`, `findtext`: the find bar's boxes and Find in selection's range, `busy`: what `jobs` waits for, `mem`: private bytes and working set), `expect:<what>=<value>`, `answer:save,dont,cancel` (answers for the next prompts, which are never
//! shown in this mode; `asked` lists the prompts so far), `set:restore_session=true` (also `font=<family>`).
//!
//! Lower level: `down:<x>,<y>` / `move:<x>,<y>` / `up:<x>,<y>` (left button, for drags; also where drag scrolling
//! sees the pointer; `down:<x>,<y>,right` or `,middle` for the others), `leave` (WM_MOUSELEAVE), `wheelraw:<delta>`
//! or `wheelraw:ctrl,<delta>` (one WM_MOUSEWHEEL over the text, or with `@<x>,<y>` over that point; touchpads send
//! small deltas), `char:<hex>[,<hex>…]` (WM_CHAR
//! through the window procedure, e.g. `char:d83d,de00`), `ime` (WM_IME_COMPOSITION, as typing in an IME), `imemsg:`
//! (an input method's messages through the window procedure, with what it would have: `start`, `comp|<text>|<cursor>|
//! <attributes, a digit per UTF-16 unit>|<clause starts, comma between>`, `result|<text>`, `end`; `print:composing`),
//! `clickraw:<x>,<y>` (a left click's messages through the window procedure, in client pixels; `click:` calls the
//! App directly), `altkey`
//! (Alt pressed and released alone: WM_SYSCOMMAND SC_KEYMENU), `altgr:on|off` (pretend Ctrl+Alt+letter types a
//! character, like AltGr on a Polish keyboard), `activate` / `deactivate` (WM_ACTIVATE), `cancelmode`
//! (WM_CANCELMODE: something took the mouse capture), `timer:<id>` (run a timer's tick now; 3 = disk check, 4 = drag
//! scrolling). More `print:` values: `focus` (main, find, replace, goto), `armed` (menu bar title with the
//! keyboard), `opened` (menus that would have opened: native menus are never shown in this mode), `keys0`…`keys4`
//! (each menu item's access key), `scrollx`, `zoom`, `topline`, `drag`, `wintitle`, `tabnames`, `indent`, `theme`
//! (light, dark or high contrast), `syscaret` (whether the hidden system caret is where the caret is: `follows`),
//! `statusbar` (all its texts), `closed` (tabs Reopen closed tab would bring back), `tablist` (the list of all
//! tabs, or `hidden` while they all fit), `bracket` (the bracket pair at the caret), `overtype`, `tip` (the tooltip
//! showing: `hover:` a part, then `timer:10`), `caret` (blinked on or off; `timer:1` blinks it), `pressed` (the part
//! drawn pressed), `invalidated` (repaints asked for since the last time), `realbold` (the font has a bold face of
//! its own).
//! `contrast:on|off|system` pretends Windows' high contrast is on or off (and tells the window it changed).
//! Screen readers: `uia:<op>` works on a text range through the UI Automation providers, as a client would
//! (`uia::command`), and `print:uia`, `uia-line`, `uia-word`, `uia-char`, `uia-para`, `uia-sel`, `uia-range`,
//! `uia-rects`, `uia-events` show what a client gets (`uia::describe`).
//!
//! The JSON and XML structure panel: `print:rows` (its rows, `+` open, `*` selected), `row:<n>` (a click on row `n`),
//! `rowopen:<n>` (on its arrow).
//!
//! Also: `args:<path>` (open it as if named on the command line: a missing file becomes a new one), `lang:<name>`
//! (pick the language), `hit:<x>,<y>` / `hover:<x>,<y>` (what's at a point / move the mouse
//! there), `scrollto:<0..1>`, `endsession` (what a Windows shutdown asks), `t:<label>` (a timing mark), `temp:<dir>`,
//! `persist` (write settings and the session; only with `SLATE_DATA_DIR` set, never into the real data folder),
//! `session:save|soon|restore` (write the session now / on another thread as the timer does / restore it), `guest`
//! (as if another Slate was running but didn't answer: nothing is kept for next time), `crash` (a native crash, to
//! try the minidump; the run ends there), `prompt:<file.png>|save|update|info|tall` (draws that prompt into a PNG, in
//! the theme's colors, without showing it; `tall` has more text than a screen holds).
//!
//! Measuring: `print:startup` (the start's steps in ms since the process was created), `gfx:window` anywhere in the
//! script (frames go through the hidden window's own swap chain, from the paint after the first command on, as in a
//! real start; `gfx:late` too: its Direct3D device is made then, not from the start), `idle:<ms>` (the real message
//! loop for that long, with the 2 s disk check: what woke it), `copydata:<path>` (a file handed over the way a second
//! Slate does it, and whether it was taken), `printpdf:<file>[|selection]` (the tab printed into a PDF file through
//! Microsoft Print to PDF, no dialog); `session:…` says how long it took, and needs `SLATE_DATA_DIR` (as
//! `persist` does); `print:datadir` is `empty` while that folder has nothing in it.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::Graphics::Direct2D::{
    D2D1_FEATURE_LEVEL_DEFAULT, D2D1_RENDER_TARGET_PROPERTIES, D2D1_RENDER_TARGET_TYPE_DEFAULT,
    D2D1_RENDER_TARGET_USAGE_NONE,
};
use windows::Win32::Graphics::Imaging::{
    CLSID_WICImagingFactory, GUID_WICPixelFormat32bppPBGRA, IWICImagingFactory, WICBitmapCacheOnLoad,
};
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance};
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, DestroyWindow, DispatchMessageW, HCBT_ACTIVATE, MSG, PM_REMOVE, PeekMessageW, SWP_NOACTIVATE,
    SWP_NOMOVE, SWP_NOZORDER, SetWindowPos, SetWindowsHookExW, TranslateMessage, UnhookWindowsHookEx, WH_CBT,
};

use super::app::{Cell, Hit};

thread_local! {
    /// `waitevent:`: the event a `--wait` request sets.
    static WAIT_EVENT: std::cell::Cell<isize> = const { std::cell::Cell::new(0) };
}
use super::commands::Cmd;
use super::findbar::FindBar;
use super::gfx::offscreen_pixel_format;
use super::settings::{Settings, ThemeMode};
use super::{create_window, drain_pending, make_app, register_class};

fn pump(cell: &Cell, ms: u64) {
    let end = Instant::now() + Duration::from_millis(ms);
    loop {
        unsafe {
            let mut msg = MSG::default();
            while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
        if let Ok(mut a) = cell.try_borrow_mut() {
            a.poll_jobs();
        }
        drain_pending(cell);
        if Instant::now() >= end {
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// Runs the message loop for `ms` the way the real one does (GetMessage), counting what woke it.
fn idle(ms: u32) -> String {
    use windows::Win32::UI::WindowsAndMessaging::{GetMessageW, KillTimer, SetTimer, WM_TIMER};
    let mut seen = std::collections::BTreeMap::<String, u32>::new();
    unsafe {
        let stop = SetTimer(None, 0, ms, None);
        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            if msg.hwnd.is_invalid() && msg.message == WM_TIMER && msg.wParam.0 == stop {
                break;
            }
            let what = match msg.message {
                WM_TIMER => format!("timer {}", msg.wParam.0),
                super::actions::WM_APP_JOB => "job".into(),
                super::actions::WM_APP_DISK => "disk".into(),
                super::actions::WM_APP_GPU => "gpu".into(),
                m => format!("{m:#06x}"),
            };
            *seen.entry(what).or_default() += 1;
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
        let _ = KillTimer(None, stop);
    }
    seen.iter().map(|(k, n)| format!("{k} x{n}")).collect::<Vec<_>>().join(", ")
}

fn busy(cell: &Cell) -> bool {
    let a = cell.borrow();
    a.tabs.iter().any(|t| {
        t.load_job.is_some()
            // (a tab waiting for its file isn't busy between looks)
            || t.restore.as_ref().is_some_and(|r| r.running())
            || t.index_job.is_some()
            || t.save.is_some()
            || t.task.is_some()
            || t.search.job.is_some()
            || t.find_job.is_some()
            || t.find_all.is_some()
            || t.structure.busy()
            || t.structure.pausing()
    }) || a.disk_job.is_some()
        || a.count_job.is_some()
        || matches!(a.update, super::app::UpdateState::Checking { .. } | super::app::UpdateState::Downloading { .. })
        || a.session_job.is_some()
}

fn vk_of(name: &str) -> Option<u16> {
    let k = match name {
        "enter" | "return" => VK_RETURN,
        "esc" | "escape" => VK_ESCAPE,
        "tab" => VK_TAB,
        "backspace" => VK_BACK,
        "delete" | "del" => VK_DELETE,
        "left" => VK_LEFT,
        "right" => VK_RIGHT,
        "up" => VK_UP,
        "down" => VK_DOWN,
        "home" => VK_HOME,
        "end" => VK_END,
        "pageup" => VK_PRIOR,
        "pagedown" => VK_NEXT,
        "insert" => VK_INSERT,
        "plus" => VK_OEM_PLUS,
        "minus" => VK_OEM_MINUS,
        "space" => VK_SPACE,
        "apps" => VK_APPS,
        "alt" => VK_MENU,
        f if f.starts_with('f') && f.len() > 1 && f[1..].parse::<u16>().is_ok() => {
            return Some(VK_F1.0 + f[1..].parse::<u16>().unwrap() - 1);
        }
        c if c.len() == 1 => return Some(c.to_ascii_uppercase().as_bytes()[0] as u16),
        _ => return None,
    };
    Some(k.0)
}

fn cmd_of(name: &str) -> Option<Cmd> {
    Some(match name {
        "NewTab" => Cmd::NewTab,
        "Save" => Cmd::Save,
        "SaveAll" => Cmd::SaveAll,
        "CloseTab" => Cmd::CloseTab,
        "Exit" => Cmd::Exit,
        "PrintPreview" => Cmd::PrintPreview,
        "PageSetup" => Cmd::PageSetup,
        "ClearHighlights" => Cmd::ClearHighlights,
        "CheckUpdates" => Cmd::CheckUpdates,
        "Undo" => Cmd::Undo,
        "Redo" => Cmd::Redo,
        "Cut" => Cmd::Cut,
        "Copy" => Cmd::Copy,
        "Paste" => Cmd::Paste,
        "SelectAll" => Cmd::SelectAll,
        "Find" => Cmd::Find,
        "FindNext" => Cmd::FindNext,
        "FindPrev" => Cmd::FindPrev,
        "Replace" => Cmd::Replace,
        "GoToLine" => Cmd::GoToLine,
        "DuplicateLine" => Cmd::DuplicateLine,
        "DeleteLine" => Cmd::DeleteLine,
        "MoveLineUp" => Cmd::MoveLineUp,
        "MoveLineDown" => Cmd::MoveLineDown,
        "ToggleWrap" => Cmd::ToggleWrap,
        "ToggleSpellCheck" => Cmd::ToggleSpellCheck,
        "ToggleTitleBarTabs" => Cmd::ToggleTitleBarTabs,
        "AddWord" => Cmd::AddWord,
        "IgnoreWord" => Cmd::IgnoreWord,
        "ToggleLineNumbers" => Cmd::ToggleLineNumbers,
        "ToggleStructure" => Cmd::ToggleStructure,
        "TogglePathBar" => Cmd::TogglePathBar,
        "CopyJsonPath" => Cmd::CopyJsonPath,
        "ZoomIn" => Cmd::ZoomIn,
        "ZoomOut" => Cmd::ZoomOut,
        "ZoomReset" => Cmd::ZoomReset,
        "JsonFormat" | "Format" => Cmd::Format,
        "JsonMinify" | "Minify" => Cmd::Minify,
        "JsonValidate" | "Validate" => Cmd::Validate,
        "ToggleComment" => Cmd::ToggleComment,
        "SortLines" => Cmd::Lines(crate::core::lines::LineOp::SortAsc),
        "SortByColumn" => Cmd::SortByColumn(false),
        "CharInfo" => Cmd::CharInfo,
        "SortByColumnDesc" => Cmd::SortByColumn(true),
        "SortLinesDesc" => Cmd::Lines(crate::core::lines::LineOp::SortDesc),
        "RemoveDuplicates" => Cmd::Lines(crate::core::lines::LineOp::Dedupe),
        "RemoveBlank" => Cmd::Lines(crate::core::lines::LineOp::RemoveBlank),
        "TrimTrailing" => Cmd::Lines(crate::core::lines::LineOp::TrimTrailing),
        "Upper" => Cmd::Case(crate::core::lines::CaseOp::Upper),
        "Lower" => Cmd::Case(crate::core::lines::CaseOp::Lower),
        "Title" => Cmd::Case(crate::core::lines::CaseOp::Title),
        "EolLf" => Cmd::SetEol(crate::core::text::Eol::Lf),
        "EolCrlf" => Cmd::SetEol(crate::core::text::Eol::Crlf),
        "ToggleReadOnly" => Cmd::ToggleReadOnly,
        "FilterLines" => Cmd::FilterLines(false),
        "FilterLinesNot" => Cmd::FilterLines(true),
        "ToggleBookmark" => Cmd::ToggleBookmark,
        "NextBookmark" => Cmd::NextBookmark,
        "PrevBookmark" => Cmd::PrevBookmark,
        "ClearBookmarks" => Cmd::ClearBookmarks,
        "SaveUtf8" => Cmd::SaveEncoding(crate::core::text::Encoding::Utf8),
        "SaveUtf16Le" => Cmd::SaveEncoding(crate::core::text::Encoding::Utf16Le),
        "SaveAnsi" => Cmd::SaveEncoding(crate::core::text::Encoding::Ansi),
        "NextTab" => Cmd::NextTab,
        "PrevTab" => Cmd::PrevTab,
        "Reload" => Cmd::Reload,
        "IndentSpaces" => Cmd::IndentSpaces(true),
        "IndentTabs" => Cmd::IndentSpaces(false),
        "Shortcuts" => Cmd::Shortcuts,
        "SaveAs" => Cmd::SaveAs,
        "ToggleWhitespace" => Cmd::ToggleWhitespace,
        "ToggleOvertype" => Cmd::ToggleOvertype,
        "ReopenClosed" => Cmd::ReopenClosed,
        "CloseOthers" => Cmd::CloseOthers,
        "CloseSaved" => Cmd::CloseSaved,
        "CloseAll" => Cmd::CloseAll,
        "ToggleRestoreSession" => Cmd::ToggleRestoreSession,
        n if n.starts_with("JsonIndent") => Cmd::JsonIndent(n["JsonIndent".len()..].parse().ok()?),
        "ReplaceAll" => return None,
        _ => return None,
    })
}

/// Points the app at a fresh offscreen bitmap of the window's size.
fn offscreen(cell: &Cell) -> Result<windows::Win32::Graphics::Imaging::IWICBitmap, String> {
    let mut a = cell.borrow_mut();
    let (w, h) = a.client_size();
    unsafe {
        let wic: IWICImagingFactory =
            CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER).map_err(|e| e.to_string())?;
        let bmp = wic.CreateBitmap(w, h, &GUID_WICPixelFormat32bppPBGRA, WICBitmapCacheOnLoad).map_err(|e| e.to_string())?;
        let props = D2D1_RENDER_TARGET_PROPERTIES {
            r#type: D2D1_RENDER_TARGET_TYPE_DEFAULT,
            pixelFormat: offscreen_pixel_format(),
            dpiX: 96.0,
            dpiY: 96.0,
            usage: D2D1_RENDER_TARGET_USAGE_NONE,
            minLevel: D2D1_FEATURE_LEVEL_DEFAULT,
        };
        let rt = a.g.d2d.CreateWicBitmapRenderTarget(&bmp, &props).map_err(|e| e.to_string())?;
        let dpi = a.dpi as f32;
        a.g.use_target(rt, dpi);
        a.g.offscreen = true;
        a.rebuild_style();
        Ok(bmp)
    }
}

/// Captures the window's client area from the screen (visible mode).
fn screen_shot(cell: &Cell, path: &Path) -> Result<(), String> {
    use windows::Win32::Foundation::{POINT, RECT};
    use windows::Win32::Graphics::Gdi::*;
    let hwnd = {
        let mut a = cell.borrow_mut();
        a.update_find_status();
        a.paint();
        a.hwnd
    };
    unsafe {
        let _ = windows::Win32::Graphics::Dwm::DwmFlush();
    }
    pump(cell, 150);
    unsafe {
        let _ = windows::Win32::Graphics::Dwm::DwmFlush();
        let mut rc = RECT::default();
        let _ = windows::Win32::UI::WindowsAndMessaging::GetClientRect(hwnd, &mut rc);
        let mut pt = POINT { x: 0, y: 0 };
        let _ = ClientToScreen(hwnd, &mut pt);
        let (w, h) = (rc.right.max(1), rc.bottom.max(1));
        let screen = GetDC(None);
        let mem = CreateCompatibleDC(screen);
        let bmi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: w,
                biHeight: -h,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut bits = std::ptr::null_mut();
        let dib = CreateDIBSection(mem, &bmi, DIB_RGB_COLORS, &mut bits, None, 0).map_err(|e| e.to_string())?;
        let old = SelectObject(mem, dib);
        let r = BitBlt(mem, 0, 0, w, h, screen, pt.x, pt.y, SRCCOPY);
        let px = std::slice::from_raw_parts(bits as *const u8, (w * h * 4) as usize).to_vec();
        SelectObject(mem, old);
        let _ = DeleteObject(dib);
        let _ = DeleteDC(mem);
        ReleaseDC(None, screen);
        r.map_err(|e| e.to_string())?;
        write_png(path, w as u32, h as u32, &px).map_err(|e| e.to_string())
    }
}

/// Draws a prompt (`save`, `update` or `info`, as Slate would show it) into a PNG, without showing it: the dialog's
/// background, then each of its controls where it is.
fn prompt_shot(owner: HWND, path: &Path, kind: &str) -> Result<(), String> {
    use windows::Win32::Foundation::{LPARAM, POINT, RECT, WPARAM};
    use windows::Win32::Graphics::Gdi::*;
    use windows::Win32::UI::WindowsAndMessaging::{
        GW_CHILD, GW_HWNDNEXT, GetClientRect, GetWindow, GetWindowRect, PRF_CLIENT, PRF_ERASEBKGND, PRF_NONCLIENT,
        SendMessageW, WM_PRINT,
    };
    let tall: String = (1..=120).map(|i| format!("Line {i} of a message that doesn't fit on the screen.\n")).collect();
    let (title, main, detail, buttons): (&str, &str, &str, &[&str]) = match kind {
        "info" => ("About Slate", "", "Slate 0.5.0\n\nA fast, simple text editor that opens files of any size. MIT license.", &["OK"]),
        "update" => (
            "Slate",
            "Slate 0.5.0 is available",
            "You have 0.4.0. Slate downloads the new version from GitHub and restarts; your tabs and unsaved changes come back.",
            &["Update and restart", "What's new", "Not now"],
        ),
        // more text than a screen holds
        "tall" => ("Slate", "A very long message", &tall, &["&Save", "Do&n't save", "Cancel"]),
        _ => ("Slate", "Do you want to save changes to notes.txt?", "", &["&Save", "Do&n't save", "Cancel"]),
    };
    let mut result = Err("the prompt wasn't made".to_string());
    super::prompt::render(owner, title, main, detail, buttons, |dlg, _| unsafe {
        let mut rc = RECT::default();
        let _ = GetClientRect(dlg, &mut rc);
        let (w, h) = (rc.right.max(1), rc.bottom.max(1));
        let mut origin = POINT { x: 0, y: 0 };
        let _ = ClientToScreen(dlg, &mut origin);
        let screen = GetDC(None);
        let mem = CreateCompatibleDC(screen);
        let bmi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: w,
                biHeight: -h,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut bits = std::ptr::null_mut();
        result = (|| {
            let dib = CreateDIBSection(mem, &bmi, DIB_RGB_COLORS, &mut bits, None, 0).map_err(|e| e.to_string())?;
            let old = SelectObject(mem, dib);
            SendMessageW(dlg, WM_PRINT, WPARAM(mem.0 as usize), LPARAM((PRF_CLIENT | PRF_ERASEBKGND) as isize));
            let mut child = GetWindow(dlg, GW_CHILD).ok();
            while let Some(c) = child {
                let mut r = RECT::default();
                let _ = GetWindowRect(c, &mut r);
                let _ = SetViewportOrgEx(mem, r.left - origin.x, r.top - origin.y, None);
                SendMessageW(c, WM_PRINT, WPARAM(mem.0 as usize), LPARAM((PRF_CLIENT | PRF_ERASEBKGND | PRF_NONCLIENT) as isize));
                child = GetWindow(c, GW_HWNDNEXT).ok();
            }
            let _ = SetViewportOrgEx(mem, 0, 0, None);
            let px = std::slice::from_raw_parts(bits as *const u8, (w * h * 4) as usize).to_vec();
            SelectObject(mem, old);
            let _ = DeleteObject(dib);
            write_png(path, w as u32, h as u32, &px).map_err(|e| e.to_string())
        })();
        let _ = DeleteDC(mem);
        ReleaseDC(None, screen);
    });
    result
}

/// Renders the window offscreen and writes a PNG.
fn shot(cell: &Cell, path: &Path) -> Result<(), String> {
    let bmp = offscreen(cell)?;
    let (w, h, mut px, hwnd) = {
        let mut a = cell.borrow_mut();
        let (w, h) = a.client_size();
        a.update_find_status();
        a.paint();
        let stride = w * 4;
        let mut px = vec![0u8; (stride * h) as usize];
        unsafe { bmp.CopyPixels(std::ptr::null(), stride, &mut px) }.map_err(|e| e.to_string())?;
        (w, h, px, a.hwnd)
    };
    // (outside the App's borrow: the boxes ask the window for their colors)
    print_children(hwnd, w, h, &mut px);
    write_png(path, w, h, &px).map_err(|e| e.to_string())
}

/// Draws the window's own child windows (the find bar's edit boxes, which Direct2D doesn't draw) into a frame.
fn print_children(hwnd: HWND, w: u32, h: u32, px: &mut [u8]) {
    use windows::Win32::Foundation::{POINT, RECT};
    use windows::Win32::Graphics::Gdi::*;
    use windows::Win32::UI::WindowsAndMessaging::{
        GW_CHILD, GW_HWNDNEXT, GWL_STYLE, GetWindow, GetWindowLongW, GetWindowRect, PRF_CHILDREN, PRF_CLIENT,
        PRF_ERASEBKGND, PRF_NONCLIENT, SendMessageW, WM_PRINT, WS_VISIBLE,
    };
    unsafe {
        let mut child = GetWindow(hwnd, GW_CHILD).ok();
        while let Some(c) = child {
            child = GetWindow(c, GW_HWNDNEXT).ok();
            if GetWindowLongW(c, GWL_STYLE) as u32 & WS_VISIBLE.0 == 0 {
                continue;
            }
            let mut r = RECT::default();
            let _ = GetWindowRect(c, &mut r);
            let mut at = POINT { x: r.left, y: r.top };
            let _ = ScreenToClient(hwnd, &mut at);
            let (cw, ch) = (r.right - r.left, r.bottom - r.top);
            if cw <= 0 || ch <= 0 {
                continue;
            }
            let screen = GetDC(None);
            let mem = CreateCompatibleDC(screen);
            let bmi = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER {
                    biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                    biWidth: cw,
                    biHeight: -ch,
                    biPlanes: 1,
                    biBitCount: 32,
                    biCompression: BI_RGB.0,
                    ..Default::default()
                },
                ..Default::default()
            };
            let mut bits = std::ptr::null_mut();
            if let Ok(dib) = CreateDIBSection(mem, &bmi, DIB_RGB_COLORS, &mut bits, None, 0) {
                let old = SelectObject(mem, dib);
                let flags = PRF_CLIENT | PRF_ERASEBKGND | PRF_NONCLIENT | PRF_CHILDREN;
                SendMessageW(c, WM_PRINT, WPARAM(mem.0 as usize), LPARAM(flags as isize));
                let src = std::slice::from_raw_parts(bits as *const u8, (cw * ch * 4) as usize);
                for y in 0..ch {
                    for x in 0..cw {
                        let (dx, dy) = (at.x + x, at.y + y);
                        if dx < 0 || dy < 0 || dx >= w as i32 || dy >= h as i32 {
                            continue;
                        }
                        let s = ((y * cw + x) * 4) as usize;
                        let d = ((dy as u32 * w + dx as u32) * 4) as usize;
                        px[d..d + 3].copy_from_slice(&src[s..s + 3]);
                        px[d + 3] = 255;
                    }
                }
                SelectObject(mem, old);
                let _ = DeleteObject(dib);
            }
            let _ = DeleteDC(mem);
            ReleaseDC(None, screen);
        }
    }
}

fn crc32(data: &[u8], mut c: u32) -> u32 {
    for &b in data {
        c ^= b as u32;
        for _ in 0..8 {
            c = if c & 1 != 0 { 0xEDB8_8320 ^ (c >> 1) } else { c >> 1 };
        }
    }
    c
}

fn preview_png(path: &Path, w: i32, h: i32, bgra: &[u8]) -> Result<(), String> {
    write_png(path, w as u32, h as u32, bgra).map_err(|e| e.to_string())
}

/// A minimal PNG writer (RGB, uncompressed deflate blocks).
fn write_png(path: &Path, w: u32, h: u32, bgra: &[u8]) -> std::io::Result<()> {
    let mut raw = Vec::with_capacity(((w * 3 + 1) * h) as usize);
    for y in 0..h as usize {
        raw.push(0);
        for x in 0..w as usize {
            let p = &bgra[(y * w as usize + x) * 4..][..4];
            raw.extend_from_slice(&[p[2], p[1], p[0]]);
        }
    }
    let mut z = vec![0x78, 0x01];
    let chunks: Vec<&[u8]> = raw.chunks(65535).collect();
    for (i, c) in chunks.iter().enumerate() {
        z.push((i + 1 == chunks.len()) as u8);
        z.extend_from_slice(&(c.len() as u16).to_le_bytes());
        z.extend_from_slice(&(!(c.len() as u16)).to_le_bytes());
        z.extend_from_slice(c);
    }
    let (mut s1, mut s2) = (1u32, 0u32);
    for &b in &raw {
        s1 = (s1 + b as u32) % 65521;
        s2 = (s2 + s1) % 65521;
    }
    z.extend_from_slice(&((s2 << 16) | s1).to_be_bytes());
    let mut out = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
    let mut chunk = |kind: &[u8], data: &[u8]| {
        out.extend_from_slice(&(data.len() as u32).to_be_bytes());
        out.extend_from_slice(kind);
        out.extend_from_slice(data);
        let c = crc32(data, crc32(kind, 0xFFFF_FFFF)) ^ 0xFFFF_FFFF;
        out.extend_from_slice(&c.to_be_bytes());
    };
    let mut ihdr = Vec::new();
    ihdr.extend_from_slice(&w.to_be_bytes());
    ihdr.extend_from_slice(&h.to_be_bytes());
    ihdr.extend_from_slice(&[8, 2, 0, 0, 0]);
    chunk(b"IHDR", &ihdr);
    chunk(b"IDAT", &z);
    chunk(b"IEND", &[]);
    std::fs::write(path, out)
}

fn describe(cell: &Cell, what: &str) -> String {
    // (before the App is borrowed: the providers borrow it themselves)
    if let Some(v) = super::uia::describe(cell, what) {
        return v;
    }
    let mut a = cell.borrow_mut();
    match what {
        "text" => {
            let d = &a.tab().doc;
            String::from_utf8_lossy(&d.read(0, d.len().min(4000))).into_owned()
        }
        "sel" => {
            let s = a.tab().view.sel;
            format!("{}..{}", s.anchor, s.caret)
        }
        "status" => a.status_message().map(|m| m.0).unwrap_or_default(),
        "lines" => a.tab().doc.line_count().map(|n| n.to_string()).unwrap_or_else(|| "?".into()),
        "len" => a.tab().doc.len().to_string(),
        "title" => a.tab().title(),
        "lang" => a.tab().lang.label().to_string(),
        "update" => match &a.update {
            super::app::UpdateState::Idle => "idle".into(),
            super::app::UpdateState::Checking { .. } => "checking".into(),
            super::app::UpdateState::Available(r) => format!("available {}", r.version),
            super::app::UpdateState::Downloading { .. } => "downloading".into(),
        },
        "restart" => a.restart_on_exit.to_string(),
        "top" => a.tab().view.top.to_string(),
        "dirty" => a.tab().doc.is_dirty().to_string(),
        "tabs" => a.tabs.len().to_string(),
        m if m.starts_with("menu") => {
            // menu0 … menu4: the menu bar's menus, as their labels (submenus in brackets)
            let items = a.menu_items(m[4..].parse().unwrap_or(0));
            labels(&items)
        }
        "tablist" if a.tablist_rect.w <= 0.0 => "hidden".into(),
        "tablist" => labels(&a.tab_list_items()),
        "theme" => (if a.theme.hc { "high contrast" } else if a.theme.dark { "dark" } else { "light" }).into(),
        "syscaret" => {
            let (k, ov) = (a.dpi as f32 / 96.0, a.overtype);
            let ours = a.with_view(|v, cx| v.caret_rect(cx, ov)).map(|c| ((c.x * k).round() as i32, (c.y * k).round() as i32));
            match (super::win::caret_pos(), ours) {
                (None, _) => "none".into(),
                (Some(p), Some(o)) if p == o => "follows".into(),
                (Some(p), o) => format!("at {p:?}, caret at {o:?}"),
            }
        }
        "statusbar" => {
            let s = a.status_items();
            let mut parts = vec![s.pos];
            parts.extend(s.items.into_iter().map(|(_, l)| l));
            parts.extend(s.counts);
            parts.push(s.size);
            parts.join(" | ")
        }
        "closed" => a
            .closed_tabs
            .iter()
            .map(|c| format!("{}@{}", c.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(), c.caret))
            .collect::<Vec<_>>()
            .join(" | "),
        "bracket" => {
            let t = a.tab();
            match super::editor::matching_bracket(&t.doc, t.view.sel.caret) {
                Some((p, q)) => format!("{p},{q}"),
                None => "none".into(),
            }
        }
        "overtype" => a.overtype.to_string(),
        "realbold" => a.style.real_bold.to_string(),
        "tip" => a.tip.text.clone().unwrap_or_default(),
        "pressed" if a.down != Hit::None && a.down == a.hover => format!("{:?}", a.down),
        "pressed" => "none".into(),
        "invalidated" => super::app::INVALIDATED.with(|n| n.replace(0)).to_string(),
        "caret" => (if a.caret_on { "on" } else { "off" }).into(),
        k if k.starts_with("keys") => {
            // keys0 … keys4: each item's access key (the letter after '&'; '?' for none), submenus in brackets
            fn keys(items: &[super::commands::Item]) -> String {
                let key = |l: &str| l.split_once('&').and_then(|(_, r)| r.chars().next()).map(|c| c.to_ascii_uppercase()).unwrap_or('?');
                items
                    .iter()
                    .filter_map(|it| match it {
                        super::commands::Item::Cmd { label, .. } => Some(key(label).to_string()),
                        super::commands::Item::Sub { label, items } => Some(format!("{}[{}]", key(label), keys(items))),
                        _ => None,
                    })
                    .collect()
            }
            let items = a.menu_items(k[4..].parse().unwrap_or(0));
            keys(&items)
        }
        "asked" => super::win::SCRIPTED.with(|s| s.borrow().as_ref().map(|s| s.asked.join(" | ")).unwrap_or_default()),
        "clipboard" => super::win::SCRIPTED
            .with(|s| s.borrow().as_ref().and_then(|s| s.clipboard.clone()))
            .map(|c| String::from_utf8_lossy(&c).into_owned())
            .unwrap_or_default(),
        "window" => (unsafe { windows::Win32::UI::WindowsAndMessaging::IsWindow(a.hwnd) }.as_bool()).to_string(),
        "saving" => a.tabs.iter().filter(|t| t.save.is_some()).count().to_string(),
        // what an input method is composing: `<text>|<cursor, bytes>|<clauses: start-end, * converting>`, or `none`
        "composing" => match &a.tab().view.composing {
            Some(c) => {
                let cl: Vec<String> = c.clauses.iter().map(|&(s, e, t)| format!("{s}-{e}{}", if t { "*" } else { "" })).collect();
                format!("{}|{}|{}", c.text, c.cursor, cl.join(","))
            }
            None => "none".into(),
        },
        // the printer the preview lays its pages out for while none was chosen: Windows' default one
        "printer" => match super::print::Last::default().printer(a.hwnd) {
            Some(p) => {
                p.discard();
                "default".into()
            }
            None => "none".into(),
        },
        "busy" => {
            // what `jobs` waits for
            let mut v = Vec::new();
            for t in &a.tabs {
                let parts = [
                    ("load", t.load_job.is_some()),
                    ("restore", t.restore.as_ref().is_some_and(|r| r.running())),
                    ("index", t.index_job.is_some()),
                    ("save", t.save.is_some()),
                    ("task", t.task.is_some()),
                    ("count", t.search.job.is_some()),
                    ("find", t.find_job.is_some()),
                    ("find all tabs", t.find_all.is_some()),
                    ("structure", t.structure.busy()),
                ];
                v.extend(parts.iter().filter(|p| p.1).map(|p| p.0));
            }
            let parts = [("disk", a.disk_job.is_some()), ("words", a.count_job.is_some()), ("session", a.session_job.is_some())];
            v.extend(parts.iter().filter(|p| p.1).map(|p| p.0));
            v.join(" ")
        }
        "mem" => {
            // this process's private bytes and working set (PROCESS_MEMORY_COUNTERS_EX)
            #[repr(C)]
            #[derive(Default)]
            struct Counters {
                cb: u32,
                faults: u32,
                peak_ws: usize,
                ws: usize,
                pools: [usize; 4],
                pagefile: usize,
                peak_pagefile: usize,
                private: usize,
            }
            #[link(name = "kernel32")]
            unsafe extern "system" {
                fn K32GetProcessMemoryInfo(process: isize, counters: *mut Counters, cb: u32) -> i32;
            }
            let mut c = Counters { cb: std::mem::size_of::<Counters>() as u32, ..Default::default() };
            // (-1: this process)
            unsafe { K32GetProcessMemoryInfo(-1, &mut c, c.cb) };
            format!("private {} MB, working set {} MB, peak {} MB", c.private >> 20, c.ws >> 20, c.peak_ws >> 20)
        }
        "notice" => a.tab().notice.as_ref().map(|n| n.text.clone()).unwrap_or_default(),
        "focus" => {
            let f = super::win::focus();
            let names = [(a.hwnd, "main"), (a.find.find_edit, "find"), (a.find.replace_edit, "replace"), (a.find.goto_edit, "goto")];
            names.iter().find(|(h, _)| *h == f).map(|(_, n)| n.to_string()).unwrap_or_else(|| "none".into())
        }
        "armed" => a.menu_armed.map(|i| super::commands::MENU_TITLES[i].to_string()).unwrap_or_default(),
        "opened" => super::win::SCRIPTED.with(|s| s.borrow().as_ref().map(|s| s.menus.join(" | ")).unwrap_or_default()),
        // the items of the last popup menu that would have opened (`rmenu:`)
        "popupitems" => super::win::SCRIPTED.with(|s| s.borrow().as_ref().map(|s| s.menu_items.join(" | ")).unwrap_or_default()),
        // the words spelled wrong on screen
        "wrong" => {
            let t = a.tab();
            a.spell_shown.iter().map(|&(s, e)| String::from_utf8_lossy(&t.doc.read(s, e)).into_owned()).collect::<Vec<_>>().join(" ")
        }
        // the window's frame: `title bar tabs` or `standard`, and how far the client area starts below the window's top
        "frame" => {
            use windows::Win32::Foundation::{POINT, RECT};
            use windows::Win32::Graphics::Gdi::ClientToScreen;
            use windows::Win32::UI::WindowsAndMessaging::GetWindowRect;
            let (mut r, mut p) = (RECT::default(), POINT::default());
            unsafe {
                let _ = GetWindowRect(a.hwnd, &mut r);
                let _ = ClientToScreen(a.hwnd, &mut p);
            }
            let kind = if a.custom_frame { "title bar tabs" } else { "standard" };
            format!("{kind}, client {} px below the top{}", p.y - r.top, if super::CUSTOM_FRAME.with(|c| c.get()) == a.custom_frame { "" } else { " (MISMATCH)" })
        }
        "speller" => match &a.speller {
            super::app::Spelling::Ready(s) => s.lang.clone(),
            super::app::Spelling::Unavailable => "none".into(),
            _ => "not yet".into(),
        },
        "scrollx" => format!("{:.0}", a.tab().view.scroll_x),
        "zoom" => format!("{:.0}%", a.settings.zoom * 100.0),
        "topline" => {
            let top = a.tab().view.top;
            a.tab().doc.line_of(top).map(|l| (l + 1).to_string()).unwrap_or_default()
        }
        "drag" => format!("{:?}", a.tab().view.drag),
        "wintitle" => {
            let mut buf = [0u16; 1024];
            let n = unsafe { windows::Win32::UI::WindowsAndMessaging::GetWindowTextW(a.hwnd, &mut buf) };
            String::from_utf16_lossy(&buf[..n.max(0) as usize])
        }
        "tabnames" => (0..a.tabs.len()).map(|i| a.tab_label(i)).collect::<Vec<_>>().join(" | "),
        "indent" => match a.indent_now() {
            super::app::Indent::Tabs => "tabs".into(),
            super::app::Indent::Spaces(n) => format!("spaces {n}"),
        },
        "find" => {
            a.update_find_status();
            a.find.status.clone()
        }
        // `--wait`s noted (their tabs), and whether the event `waitevent:` made is set
        "waits" => {
            let set = WAIT_EVENT.with(|w| w.get()) != 0 && unsafe {
                windows::Win32::System::Threading::WaitForSingleObject(windows::Win32::Foundation::HANDLE(WAIT_EVENT.with(|w| w.get()) as *mut _), 0)
            } == windows::Win32::Foundation::WAIT_OBJECT_0;
            format!("{:?}, event {}", a.waits.iter().map(|(_, ids)| ids.len()).collect::<Vec<_>>(), if set { "set" } else { "not set" })
        }
        // (the find bar's boxes, and Find in selection's range)
        "findtext" => format!(
            "{:?} / {:?} / scope {:?}",
            FindBar::text_of(a.find.find_edit),
            FindBar::text_of(a.find.replace_edit),
            a.tab().view.scope
        ),
        "readonly" => (a.tab().read_only || a.tab().filter.is_some()).to_string(),
        // (the lines bookmarked, 1-based)
        "bookmarks" => {
            let tab = a.tab();
            let lines: Vec<String> = tab.view.bookmarks.iter().filter_map(|&b| tab.doc.line_of(b)).map(|l| (l + 1).to_string()).collect();
            lines.join(",")
        }
        // (a tab of matching lines: the line in its file of each of its lines, 1-based)
        "filter" => match &a.tab().filter {
            Some(f) => (0..f.map.lines.min(50)).filter_map(|l| f.map.source(l)).map(|l| (l + 1).to_string()).collect::<Vec<_>>().join(","),
            None => "none".into(),
        },
        // (the structure panel's rows as last worked out: `+` open, `*` selected; `(old)` before an edit)
        "rows" => {
            let tab = a.tab();
            let s = &tab.structure;
            let rows: Vec<String> = s
                .rows
                .iter()
                .map(|r| format!("{}{}{}", r.label, if r.expanded { "+" } else { "" }, if s.selected == Some(r.key) { "*" } else { "" }))
                .collect();
            format!("{}{}", if s.rows_current(&tab.doc) { "" } else { "(old) " }, rows.join(" "))
        }
        // (the JSON or XML path bar, as last worked out)
        "path" => {
            let s = &a.tab().structure;
            s.path.as_ref().map_or("none".into(), |p| s.path_text(p))
        }
        // (where the scrollbar's match marks are, in pixels from its top, as last drawn)
        "marks" => a.tab().search.marks.as_ref().map_or("none".into(), |m| format!("{:?}", m.1.iter().map(|y| y.round() as i32).collect::<Vec<_>>())),
        // (milliseconds since the process was created: the start's steps, then this command)
        "startup" => {
            super::mark("now");
            super::marks().iter().map(|(w, ms)| format!("{w} {ms:.1}")).collect::<Vec<_>>().join(" | ")
        }
        // (what's in the data folder SLATE_DATA_DIR names: a test that keeps a session there needs it empty)
        "datadir" => match std::env::var_os("SLATE_DATA_DIR") {
            None => "not set".into(),
            Some(d) => match std::fs::read_dir(&d).map(|r| r.count()) {
                Ok(0) | Err(_) => "empty".into(),
                Ok(n) => format!("not empty ({n} files or folders in {})", Path::new(&d).display()),
            },
        },
        _ => format!("(unknown: {what})"),
    }
}

/// A menu as its labels (submenus in brackets).
fn labels(items: &[super::commands::Item]) -> String {
    items
        .iter()
        .map(|it| match it {
            super::commands::Item::Cmd { label, enabled, checked, .. } => {
                let label = label.replace("&&", "\u{1}").replace('&', "").replace('\u{1}', "&");
                format!("{label}{}{}", if *checked { " (on)" } else { "" }, if *enabled { "" } else { " (off)" })
            }
            super::commands::Item::Sep => "-".into(),
            super::commands::Item::ColBreak => "||".into(),
            super::commands::Item::Sub { label, items } => format!("{} [{}]", label.replace('&', ""), labels(items)),
        })
        .collect::<Vec<_>>()
        .join(" | ")
}

/// Keeps every window of the test's thread from being activated (CBT hook): one that was would take the keyboard from
/// the user's own windows. (The focus is only noted: `win::set_focus`.)
unsafe extern "system" fn never_activate(code: i32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    if code == HCBT_ACTIVATE as i32 {
        return LRESULT(1);
    }
    unsafe { CallNextHookEx(None, code, wp, lp) }
}

pub fn run(args: &[String]) -> i32 {
    super::settings::NO_PERSIST.store(true, std::sync::atomic::Ordering::Relaxed);
    super::win::SCRIPTED.with(|s| *s.borrow_mut() = Some(Default::default()));
    let hook = unsafe { SetWindowsHookExW(WH_CBT, Some(never_activate), None, GetCurrentThreadId()) };
    let lines: Vec<String> = if args.len() == 1 && Path::new(&args[0]).is_file() {
        std::fs::read_to_string(&args[0]).unwrap_or_default().lines().map(String::from).collect()
    } else {
        args.to_vec()
    };
    // A script that draws through the window's own target starts the way Slate does: the device made early (unless
    // `gfx:late`: then at the first paint, to compare), and the first frame after the first command.
    let has = |what: &str| lines.iter().any(|l| l.trim() == what);
    let real = has("gfx:window");
    let _device = (real && !has("gfx:late")).then(super::gfx::make_device_early);
    let hinst = register_class();
    let s = Settings { restore_session: false, ..Default::default() };
    let hwnd = create_window(hinst, &s);
    super::gfx::early_window(hwnd);
    // (as Slate takes it when it starts)
    super::win::set_focus(hwnd);
    super::mark("window");
    let cell = make_app(hwnd);
    super::mark("app");
    {
        let mut a = cell.borrow_mut();
        a.settings = s;
        a.apply_theme();
        a.new_untitled();
    }
    let visible = std::env::var_os("SLATE_TEST_VISIBLE").is_some();
    // (For timings: draw through the hidden window's own swap chain on the GPU, as the real window does; so does a
    // script with `gfx:window`.)
    let gpu = real || std::env::var_os("SLATE_TEST_GPU").is_some();
    let set_size = |hwnd: HWND, w: i32, h: i32| unsafe {
        use windows::Win32::UI::WindowsAndMessaging::{HWND_TOPMOST, SWP_SHOWWINDOW};
        if visible {
            let _ = SetWindowPos(hwnd, HWND_TOPMOST, 40, 40, w, h, SWP_NOACTIVATE | SWP_SHOWWINDOW);
        } else {
            let _ = SetWindowPos(hwnd, None, 0, 0, w, h, SWP_NOMOVE | SWP_NOZORDER | SWP_NOACTIVATE);
        }
    };
    set_size(hwnd, 1200, 800);
    if !visible && !gpu {
        let _ = offscreen(&cell);
    }
    super::mark("ready");
    let started = Instant::now();
    let mut mark = started;
    let mut failures = 0;
    let mut out = String::new();
    for line in lines.iter().map(|l| l.trim()).filter(|l| !l.is_empty() && !l.starts_with('#')) {
        let (op, arg) = line.split_once(':').unwrap_or((line, ""));
        let unescape = |s: &str| s.replace("\\r", "\r").replace("\\n", "\n").replace("\\t", "\t");
        match op {
            "size" => {
                if let Some((w, h)) = arg.split_once('x') {
                    set_size(hwnd, w.parse().unwrap_or(1200), h.parse().unwrap_or(800));
                    if !visible && !gpu {
                        let _ = offscreen(&cell);
                    }
                }
            }
            "dpi" => {
                // As if the window moved to a monitor at this DPI: WM_DPICHANGED with the rect Windows suggests (the
                // same size in DIPs).
                use windows::Win32::Foundation::{LPARAM, RECT, WPARAM};
                use windows::Win32::UI::WindowsAndMessaging::{GetWindowRect, SendMessageW, WM_DPICHANGED};
                let new: u32 = arg.parse().unwrap_or(96).clamp(96, 480);
                let old = cell.borrow().dpi.max(1);
                super::win::FORCED_DPI.with(|d| d.set(Some(new)));
                let mut r = RECT::default();
                unsafe {
                    let _ = GetWindowRect(hwnd, &mut r);
                }
                let scale = |v: i32| (v as i64 * new as i64 / old as i64) as i32;
                let r = RECT { right: r.left + scale(r.right - r.left), bottom: r.top + scale(r.bottom - r.top), ..r };
                let (wp, lp) = (WPARAM((new | new << 16) as usize), LPARAM(&r as *const RECT as isize));
                unsafe { SendMessageW(hwnd, WM_DPICHANGED, wp, lp) };
                if !visible && !gpu {
                    let _ = offscreen(&cell);
                }
            }
            "theme" => {
                let mut a = cell.borrow_mut();
                a.settings.theme = if arg == "dark" { ThemeMode::Dark } else { ThemeMode::Light };
                a.apply_theme();
            }
            "set" => {
                let mut a = cell.borrow_mut();
                let (k, v) = arg.split_once('=').unwrap_or((arg, ""));
                match k {
                    "wrap" => a.settings.wrap = v == "true",
                    "line_numbers" => a.settings.line_numbers = v == "true",
                    "font_size" => a.settings.font_size = v.parse().unwrap_or(11.0),
                    "restore_session" => a.settings.restore_session = v == "true",
                    "show_whitespace" => a.settings.show_whitespace = v == "true",
                    "font" => a.settings.font = v.to_string(),
                    "print_colors" => a.settings.print_colors = v == "true",
                    "print_line_numbers" => a.settings.print_line_numbers = v == "true",
                    "print_header" => a.settings.print_header = v == "true",
                    // (hundredths of a millimetre: left,top,right,bottom)
                    "print_margins" => {
                        if let [l, t, r, b] = v.split(',').filter_map(|x| x.trim().parse().ok()).collect::<Vec<u32>>()[..] {
                            a.settings.print_margins = [l, t, r, b];
                        }
                    }
                    _ => {}
                }
                a.rebuild_style();
            }
            "open" => cell.borrow_mut().open_paths(&[PathBuf::from(arg)]),
            // as if named on Slate's command line (a file that isn't there yet becomes a new one)
            "args" => cell.borrow_mut().open_command_line(&[PathBuf::from(arg)]),
            "lang" => match super::highlight::Lang::ALL.iter().find(|l| l.label().eq_ignore_ascii_case(arg)) {
                Some(&l) => cell.borrow_mut().exec(Cmd::SetLang(l)),
                None => {
                    out.push_str(&format!("unknown language {arg}\n"));
                    failures += 1;
                }
            },
            "type" => cell.borrow_mut().type_text(&unescape(arg)),
            "key" => {
                let parts: Vec<String> = arg.to_lowercase().split('+').map(String::from).collect();
                let ctrl = parts.iter().any(|p| p == "ctrl");
                let shift = parts.iter().any(|p| p == "shift");
                let alt = parts.iter().any(|p| p == "alt");
                if let Some(vk) = parts.last().and_then(|k| vk_of(k)) {
                    super::commands::FORCED_MODS.with(|m| m.set(Some((ctrl, shift, alt))));
                    let bar_edit = {
                        let f = super::win::focus();
                        let a = cell.borrow();
                        if a.find.is_edit(f) { Some(f) } else { None }
                    };
                    let used = match bar_edit {
                        Some(e) => cell.borrow_mut().bar_key(e, vk),
                        None => false,
                    };
                    if !used {
                        cell.borrow_mut().on_key(vk);
                    }
                    drain_pending(&cell);
                    super::commands::FORCED_MODS.with(|m| m.set(None));
                }
            }
            "cmd" => match cmd_of(arg) {
                Some(c) => {
                    super::actions::run_cmd(&cell, c);
                    drain_pending(&cell);
                }
                None if arg == "ReplaceAll" => cell.borrow_mut().start_task(super::app::TaskKind::ReplaceAll),
                None => {
                    out.push_str(&format!("unknown command {arg}\n"));
                    failures += 1;
                }
            },
            "find" => {
                let mut a = cell.borrow_mut();
                if !a.find.open {
                    a.open_find(super::findbar::Mode::Find);
                }
                FindBar::set_text(a.find.find_edit, &unescape(arg));
                a.on_find_changed();
                a.start_count();
            }
            "replace" => {
                let mut a = cell.borrow_mut();
                a.open_find(super::findbar::Mode::Replace);
                FindBar::set_text(a.find.replace_edit, &unescape(arg));
            }
            "goto" => {
                let mut a = cell.borrow_mut();
                a.open_find(super::findbar::Mode::GoTo);
                FindBar::set_text(a.find.goto_edit, arg);
                a.go_to_line_from_bar();
            }
            "saveas" => {
                let mut a = cell.borrow_mut();
                let i = a.active;
                let enc = a.tab().doc.encoding;
                a.start_save(i, PathBuf::from(arg), enc, false);
            }
            // the text's menu at a point (client DIPs), as a right click there opens it, and an item chosen in it by
            // its label: `rmenu:<x>,<y>[|<label>]` (`print:popupitems` lists them)
            "rmenu" => {
                let (at, pick) = arg.split_once('|').map_or((arg, None), |(p, l)| (p, Some(l.to_string())));
                let (x, y) = at.split_once(',').map(|(x, y)| (x.trim().parse().unwrap_or(0.0), y.trim().parse().unwrap_or(0.0))).unwrap_or((0.0, 0.0));
                super::win::SCRIPTED.with(|s| {
                    if let Some(s) = s.borrow_mut().as_mut() {
                        s.pick = pick;
                    }
                });
                cell.borrow_mut().pending.push(super::app::Deferred::ContextMenu(x, y));
                drain_pending(&cell);
            }
            "click" | "dblclick" | "rclick" => {
                let (x, y) = arg.split_once(',').map(|(x, y)| (x.parse().unwrap_or(0.0), y.parse().unwrap_or(0.0))).unwrap_or((0.0, 0.0));
                let n = if op == "dblclick" { 2 } else { 1 };
                let b = if op == "rclick" { 1 } else { 0 };
                for _ in 0..n {
                    let mut a = cell.borrow_mut();
                    a.on_mouse_move(x, y);
                    a.on_mouse_down(x, y, b);
                    a.on_mouse_up(x, y, b);
                }
                cell.borrow_mut().pending.retain(|d| !matches!(d, super::app::Deferred::ContextMenu(..) | super::app::Deferred::Menu(_)));
                drain_pending(&cell);
            }
            "hit" => {
                let (x, y) = arg.split_once(',').map(|(x, y)| (x.parse().unwrap_or(0.0), y.parse().unwrap_or(0.0))).unwrap_or((0.0, 0.0));
                let a = cell.borrow();
                out.push_str(&format!(
                    "hit {x},{y}: {:?} (client {:?} dip, dpi {}, menu {:?}, toggle {:?})\n",
                    a.hit(x, y),
                    a.size,
                    a.dpi,
                    a.r_menu,
                    a.theme_rect
                ));
            }
            "hover" => {
                let (x, y) = arg.split_once(',').map(|(x, y)| (x.parse().unwrap_or(0.0), y.parse().unwrap_or(0.0))).unwrap_or((0.0, 0.0));
                cell.borrow_mut().on_mouse_move(x, y);
            }
            "wheel" => {
                let rows: i32 = arg.parse().unwrap_or(3);
                let mut a = cell.borrow_mut();
                let (x, y) = (a.r_edit.x + 100.0, a.r_edit.y + 100.0);
                a.on_wheel(-rows * 40, false, x, y);
                a.hover = Hit::None;
            }
            "wait" => pump(&cell, arg.parse().unwrap_or(100)),
            // What the timer does every 2 s in the real window (follow with `jobs`).
            "checkdisk" => cell.borrow_mut().check_disk(),
            "endsession" => {
                // What a shutdown asks: may the session end? Then "it isn't ending after all".
                use windows::Win32::Foundation::{LPARAM, WPARAM};
                use windows::Win32::UI::WindowsAndMessaging::{SendMessageW, WM_ENDSESSION, WM_QUERYENDSESSION};
                let r = unsafe { SendMessageW(hwnd, WM_QUERYENDSESSION, WPARAM(0), LPARAM(0)) };
                out.push_str(&format!("end session: {}\n", if r.0 != 0 { "allowed" } else { "blocked" }));
                unsafe { SendMessageW(hwnd, WM_ENDSESSION, WPARAM(0), LPARAM(0)) };
            }
            "answer" => super::win::SCRIPTED.with(|s| {
                if let Some(s) = s.borrow_mut().as_mut() {
                    for a in arg.split(',').map(str::trim) {
                        s.answers.push_back(match a {
                            "save" => Some(0),
                            "dont" => Some(1),
                            "cancel" => None,
                            n => n.parse().ok(),
                        });
                    }
                }
            }),
            "jobs" => {
                let start = Instant::now();
                pump(&cell, 20);
                while busy(&cell) && start.elapsed() < Duration::from_secs(300) {
                    pump(&cell, 20);
                }
                pump(&cell, 20);
                out.push_str(&format!("jobs done in {} ms\n", start.elapsed().as_millis()));
            }
            "shot" => {
                let r = if visible { screen_shot(&cell, Path::new(arg)) } else { shot(&cell, Path::new(arg)) };
                if let Err(e) = r {
                    out.push_str(&format!("shot failed: {e}\n"));
                    failures += 1;
                }
            }
            "prompt" => {
                let (file, kind) = arg.split_once('|').unwrap_or((arg, "save"));
                let hwnd = cell.borrow().hwnd;
                if let Err(e) = prompt_shot(hwnd, Path::new(file), kind) {
                    out.push_str(&format!("prompt failed: {e}\n"));
                    failures += 1;
                }
            }
            "print" => {
                // (the words spelled wrong are worked out when a frame is drawn: one first)
                if arg == "wrong" {
                    if let Ok(_frame) = offscreen(&cell) {
                        cell.borrow_mut().paint();
                    }
                }
                out.push_str(&format!("{arg}: {}\n", describe(&cell, arg)));
            }
            "expect" => {
                let (what, want) = arg.split_once('=').unwrap_or((arg, ""));
                let got = describe(&cell, what);
                let want = unescape(want);
                if got != want {
                    out.push_str(&format!("FAIL {what}: expected {want:?}, got {got:?}\n"));
                    failures += 1;
                } else {
                    out.push_str(&format!("ok {what}\n"));
                }
            }
            "time" => out.push_str(&format!("{arg}\n")),
            "t" => {
                let now = Instant::now();
                out.push_str(&format!(
                    "[{:>7.1} ms | +{:>7.1} ms] {arg}\n",
                    now.duration_since(started).as_secs_f64() * 1000.0,
                    now.duration_since(mark).as_secs_f64() * 1000.0
                ));
                mark = now;
            }
            "temp" => crate::core::source::set_temp_dir(PathBuf::from(arg)),
            "persist" => {
                // Only into a data folder of the test's own: never the real settings and session.
                if std::env::var_os("SLATE_DATA_DIR").is_some() {
                    super::settings::NO_PERSIST.store(false, std::sync::atomic::Ordering::Relaxed);
                } else {
                    out.push_str("persist needs SLATE_DATA_DIR\n");
                    failures += 1;
                }
            }
            // A native crash (to try the minidump): the process ends here.
            // (only with a data folder of the test's own: the crash log and the dump go there)
            "crash" if std::env::var_os("SLATE_DATA_DIR").is_none() => {
                out.push_str("crash needs SLATE_DATA_DIR\n");
                failures += 1;
            }
            "crash" => unsafe { std::ptr::null_mut::<u8>().write_volatile(1) },
            // As if another Slate was running but didn't answer: this window keeps nothing for next time.
            "guest" => super::settings::GUEST.store(true, std::sync::atomic::Ordering::Relaxed),
            // (only with a data folder of the test's own: restoring tidies up the session's folder, and reads it)
            "session" if std::env::var_os("SLATE_DATA_DIR").is_none() => {
                out.push_str(&format!("session:{arg} needs SLATE_DATA_DIR\n"));
                failures += 1;
            }
            "session" => {
                let t = Instant::now();
                let mut a = cell.borrow_mut();
                match arg {
                    "save" => {
                        a.settings.restore_session = true;
                        a.save_session();
                    }
                    "soon" => {
                        a.settings.restore_session = true;
                        a.save_session_soon();
                    }
                    _ => {
                        a.settings.restore_session = true;
                        a.tabs.clear();
                        a.active = 0;
                        super::restore(&mut a, &[], &Default::default());
                    }
                }
                out.push_str(&format!("session {arg}: {:.1} ms\n", t.elapsed().as_secs_f64() * 1000.0));
            }
            "down" | "move" | "up" => {
                // (`down:<x>,<y>,right` or `,middle`: another button)
                let parts: Vec<&str> = arg.split(',').map(str::trim).collect();
                let num = |i: usize| parts.get(i).and_then(|s| s.parse().ok()).unwrap_or(0.0);
                let (x, y) = (num(0), num(1));
                let b = match parts.get(2) {
                    Some(&"right") => 1,
                    Some(&"middle") => 2,
                    _ => 0,
                };
                // (`,alt` or `,ctrl`: that key held)
                let held = match parts.get(2) {
                    Some(&"alt") => Some((false, false, true)),
                    Some(&"ctrl") => Some((true, false, false)),
                    _ => None,
                };
                super::commands::FORCED_MODS.with(|m| m.set(held));
                super::actions::TEST_POINTER.with(|p| p.set(Some((x, y))));
                {
                    let mut a = cell.borrow_mut();
                    match op {
                        "down" => {
                            a.on_mouse_move(x, y);
                            a.on_mouse_down(x, y, b);
                        }
                        "move" => a.on_mouse_move(x, y),
                        _ => a.on_mouse_up(x, y, b),
                    }
                }
                super::commands::FORCED_MODS.with(|m| m.set(None));
                drain_pending(&cell);
            }
            "wheelraw" => {
                // (`@x,y`: over that point, the tab strip say; else over the text)
                let (arg, at) = match arg.split_once('@') {
                    Some((a, p)) => (a, p.split_once(',').and_then(|(x, y)| Some((x.parse::<f32>().ok()?, y.parse::<f32>().ok()?)))),
                    None => (arg, None),
                };
                let (ctrl, delta) = match arg.split_once(',') {
                    Some((m, d)) => (m == "ctrl", d),
                    None => (false, arg),
                };
                super::commands::FORCED_MODS.with(|m| m.set(Some((ctrl, false, false))));
                {
                    let mut a = cell.borrow_mut();
                    let (x, y) = at.unwrap_or((a.r_edit.x + 100.0, a.r_edit.y + 100.0));
                    a.on_wheel(delta.parse().unwrap_or(120), false, x, y);
                    a.hover = Hit::None;
                }
                super::commands::FORCED_MODS.with(|m| m.set(None));
            }
            "char" => {
                use windows::Win32::Foundation::{LPARAM, WPARAM};
                use windows::Win32::UI::WindowsAndMessaging::{SendMessageW, WM_CHAR};
                for c in arg.split(',') {
                    let v = u16::from_str_radix(c.trim(), 16).unwrap_or(0);
                    unsafe { SendMessageW(hwnd, WM_CHAR, WPARAM(v as usize), LPARAM(0)) };
                }
            }
            "ime" => {
                use windows::Win32::UI::WindowsAndMessaging::{SendMessageW, WM_IME_COMPOSITION};
                unsafe { SendMessageW(hwnd, WM_IME_COMPOSITION, WPARAM(0), LPARAM(0)) };
            }
            "altkey" => {
                use windows::Win32::Foundation::{LPARAM, WPARAM};
                use windows::Win32::UI::WindowsAndMessaging::{SC_KEYMENU, SendMessageW, WM_SYSCOMMAND};
                unsafe { SendMessageW(hwnd, WM_SYSCOMMAND, WPARAM(SC_KEYMENU as usize), LPARAM(0)) };
            }
            "altgr" => super::commands::FORCED_ALTGR.with(|f| f.set(arg == "on")),
            // the window's frame, as Windows asks it (`<x>,<y>` client DIPs): `nchit:` what's there (WM_NCHITTEST),
            // `ncmove:` the mouse there, `ncclick:` a left click there (the non-client messages, when it's the frame's),
            // `ncleave` (WM_NCMOUSELEAVE)
            // the window active or not, as Windows tells its frame (WM_NCACTIVATE: `ncactivate:0|1`)
            "ncactivate" => {
                use windows::Win32::Foundation::{LPARAM, WPARAM};
                use windows::Win32::UI::WindowsAndMessaging::{SendMessageW, WM_NCACTIVATE};
                unsafe { SendMessageW(hwnd, WM_NCACTIVATE, WPARAM((arg == "1") as usize), LPARAM(0)) };
            }
            "nchit" | "ncmove" | "ncclick" | "ncleave" => {
                use windows::Win32::Foundation::{LPARAM, POINT, WPARAM};
                use windows::Win32::Graphics::Gdi::ClientToScreen;
                use windows::Win32::UI::WindowsAndMessaging::*;
                let (x, y) = arg.split_once(',').map(|(x, y)| (x.trim().parse().unwrap_or(0.0), y.trim().parse().unwrap_or(0.0))).unwrap_or((0.0, 0.0));
                let mut p = {
                    let a = cell.borrow();
                    POINT { x: a.dip_to_px(x), y: a.dip_to_px(y) }
                };
                unsafe {
                    let _ = ClientToScreen(hwnd, &mut p);
                }
                let lp = LPARAM((((p.y as u32 & 0xFFFF) << 16) | (p.x as u32 & 0xFFFF)) as isize);
                let code = unsafe { SendMessageW(hwnd, WM_NCHITTEST, WPARAM(0), lp) }.0 as u32;
                let name = match code {
                    HTCLIENT => "client",
                    HTCAPTION => "caption",
                    HTMINBUTTON => "minimize",
                    HTMAXBUTTON => "maximize",
                    HTCLOSE => "close",
                    HTTOP => "top",
                    HTTOPLEFT => "topleft",
                    HTTOPRIGHT => "topright",
                    HTLEFT => "left",
                    HTRIGHT => "right",
                    HTBOTTOM => "bottom",
                    HTSYSMENU => "sysmenu",
                    _ => "other",
                };
                match op {
                    "nchit" => out.push_str(&format!("nchit {arg}: {name}\n")),
                    "ncmove" => unsafe {
                        SendMessageW(hwnd, WM_NCMOUSEMOVE, WPARAM(code as usize), lp);
                    },
                    "ncleave" => unsafe {
                        SendMessageW(hwnd, WM_NCMOUSELEAVE, WPARAM(0), LPARAM(0));
                    },
                    // (only the window's buttons: a press on the caption or an edge would start Windows' own moving
                    // or sizing, which follows the real mouse)
                    _ if !matches!(code, HTMINBUTTON | HTMAXBUTTON | HTCLOSE) => out.push_str(&format!("ncclick {arg}: {name}, not clicked\n")),
                    _ => unsafe {
                        SendMessageW(hwnd, WM_NCLBUTTONDOWN, WPARAM(code as usize), lp);
                        SendMessageW(hwnd, WM_NCLBUTTONUP, WPARAM(code as usize), lp);
                    },
                }
                drain_pending(&cell);
            }
            // a left click as the window gets one: its messages through the window procedure (`<x>,<y>` client pixels)
            "clickraw" => {
                use windows::Win32::Foundation::{LPARAM, WPARAM};
                use windows::Win32::UI::WindowsAndMessaging::{SendMessageW, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MOUSEMOVE};
                let (x, y) = arg.split_once(',').map(|(x, y)| (x.trim().parse().unwrap_or(0), y.trim().parse().unwrap_or(0))).unwrap_or((0, 0));
                let lp = LPARAM((((y as u32 & 0xFFFF) << 16) | (x as u32 & 0xFFFF)) as isize);
                for (m, w) in [(WM_MOUSEMOVE, 0), (WM_LBUTTONDOWN, 1), (WM_LBUTTONUP, 0)] {
                    unsafe { SendMessageW(hwnd, m, WPARAM(w), lp) };
                }
                drain_pending(&cell);
            }
            "imemsg" => {
                use windows::Win32::Foundation::{LPARAM, WPARAM};
                use windows::Win32::UI::Input::Ime::{GCS_COMPATTR, GCS_COMPCLAUSE, GCS_COMPSTR, GCS_CURSORPOS, GCS_RESULTSTR};
                use windows::Win32::UI::WindowsAndMessaging::{SendMessageW, WM_IME_COMPOSITION, WM_IME_ENDCOMPOSITION, WM_IME_STARTCOMPOSITION};
                let mut p = arg.split('|');
                let (msg, lp) = match p.next().unwrap_or_default() {
                    "start" => (WM_IME_STARTCOMPOSITION, 0),
                    "comp" => {
                        let comp: Vec<u16> = p.next().unwrap_or_default().encode_utf16().collect();
                        let cursor = p.next().and_then(|c| c.parse().ok()).unwrap_or(comp.len() as u32);
                        let attrs = p.next().unwrap_or_default().bytes().map(|b| b.wrapping_sub(b'0')).collect();
                        let clauses = p.next().unwrap_or_default().split(',').filter_map(|c| c.parse().ok()).collect();
                        let raw = super::ime::Raw { comp, attrs, clauses, cursor, result: Vec::new() };
                        super::ime::FAKE.with(|f| *f.borrow_mut() = Some(raw));
                        (WM_IME_COMPOSITION, (GCS_COMPSTR.0 | GCS_COMPATTR.0 | GCS_COMPCLAUSE.0 | GCS_CURSORPOS.0) as isize)
                    }
                    "result" => {
                        let result = p.next().unwrap_or_default().encode_utf16().collect();
                        super::ime::FAKE.with(|f| *f.borrow_mut() = Some(super::ime::Raw { result, ..Default::default() }));
                        (WM_IME_COMPOSITION, GCS_RESULTSTR.0 as isize)
                    }
                    _ => (WM_IME_ENDCOMPOSITION, 0),
                };
                unsafe { SendMessageW(hwnd, msg, WPARAM(0), LPARAM(lp)) };
            }
            "contrast" => {
                use windows::Win32::Foundation::{LPARAM, WPARAM};
                use windows::Win32::UI::WindowsAndMessaging::{SPI_SETHIGHCONTRAST, SendMessageW, WM_SETTINGCHANGE};
                let forced = match arg {
                    "on" => Some(true),
                    "off" => Some(false),
                    _ => None,
                };
                super::theme::FORCED_HIGH_CONTRAST.with(|f| f.set(forced));
                // what Windows sends when high contrast goes on or off
                unsafe { SendMessageW(hwnd, WM_SETTINGCHANGE, WPARAM(SPI_SETHIGHCONTRAST.0 as usize), LPARAM(0)) };
            }
            // the mouse left the window (WM_MOUSELEAVE)
            "leave" => cell.borrow_mut().on_mouse_leave(),
            "activate" | "deactivate" | "cancelmode" => {
                use windows::Win32::Foundation::{LPARAM, WPARAM};
                use windows::Win32::UI::WindowsAndMessaging::{SendMessageW, WA_ACTIVE, WA_INACTIVE, WM_ACTIVATE, WM_CANCELMODE};
                let (m, w) = match op {
                    "activate" => (WM_ACTIVATE, WA_ACTIVE as usize),
                    "deactivate" => (WM_ACTIVATE, WA_INACTIVE as usize),
                    _ => (WM_CANCELMODE, 0),
                };
                unsafe { SendMessageW(hwnd, m, WPARAM(w), LPARAM(0)) };
            }
            // File → Print preview run for real, its window hidden, doing `steps` (| between; `preview::Script`)
            "previewrun" => {
                let steps = arg.split('|').filter(|s| !s.is_empty()).map(String::from).collect();
                super::preview::SCRIPT.with(|s| *s.borrow_mut() = Some(super::preview::Script { steps, out: String::new(), shot: preview_png }));
                super::actions::run_cmd(&cell, super::commands::Cmd::PrintPreview);
                drain_pending(&cell);
                let script = super::preview::SCRIPT.with(|s| s.borrow_mut().take());
                if let Some(s) = script {
                    out.push_str(&s.out);
                    if !s.steps.is_empty() {
                        out.push_str(&format!("previewrun: {} steps not done (the preview closed or never opened)\n", s.steps.len()));
                        failures += 1;
                    }
                }
            }
            // the print preview of the tab, page `n` (from 1), into a picture: `previewshot:<png>|<n>[|width|noprinter]` (laid
            // out for Microsoft Print to PDF)
            "previewshot" => {
                let mut parts = arg.split('|');
                let file = parts.next().unwrap_or_default();
                let page: usize = parts.next().and_then(|n| n.parse().ok()).unwrap_or(1);
                let opt = parts.next();
                let width = opt == Some("width");
                let made = {
                    let mut a = cell.borrow_mut();
                    let i = a.active;
                    let snap = a.tabs[i].doc.snapshot();
                    let to = snap.len();
                    let source = super::preview::Source { snap, from: 0, to, line: 1, line_start: 0, lang: a.tabs[i].lang };
                    let style = super::actions::print_style(&a.settings, &a.tabs[i], to);
                    let theme = a.theme.clone();
                    // (`noprinter`: as the window shows when there's none)
                    match (opt == Some("noprinter")).then_some(None).or_else(|| super::print::Printer::pdf().map(Some)) {
                        Some(p) => super::preview::picture(p, &source, style, theme, page, width, 900, 1100),
                        None => Err("no Microsoft Print to PDF printer".into()),
                    }
                };
                match made {
                    Ok((bgra, pages)) => {
                        out.push_str(&format!("preview: {pages} pages\n"));
                        if let Err(e) = write_png(Path::new(file), 900, 1100, &bgra) {
                            out.push_str(&format!("previewshot {file}: {e}\n"));
                            failures += 1;
                        }
                    }
                    Err(e) => {
                        out.push_str(&format!("previewshot failed: {e}\n"));
                        failures += 1;
                    }
                }
            }
            // a click on the structure panel's row (its arrow: `rowopen:`)
            "row" | "rowopen" => {
                cell.borrow_mut().struct_click(arg.parse().unwrap_or(0), op == "rowopen", false);
                drain_pending(&cell);
            }
            "timer" => {
                let id: usize = arg.parse().unwrap_or(0);
                cell.borrow_mut().on_timer(id);
                drain_pending(&cell);
            }
            "scrollto" => {
                // fraction of the scrollbar, 0..1
                let f: f32 = arg.parse().unwrap_or(0.5);
                let mut a = cell.borrow_mut();
                a.with_view(|v, cx| v.set_scroll_fraction(cx, f));
            }
            // (see `real` above) Back to the window's own target after a `shot`.
            "gfx" => {
                let mut a = cell.borrow_mut();
                if a.g.offscreen {
                    a.g.offscreen = false;
                    a.g.discard_target();
                }
            }
            // The real message loop for <ms>, as an idle window runs it (with its 2 s disk check): what woke it.
            "idle" => {
                cell.borrow().timer(super::actions::TIMER_DISK, 2000);
                out.push_str(&format!("idle: {}\n", idle(arg.parse().unwrap_or(10_000))));
            }
            // What another Slate sends to hand its files over.
            "printpdf" => {
                // printpdf:<file>[|selection]: the tab printed into a PDF file through Microsoft Print to PDF
                let (file, selection) = match arg.split_once('|') {
                    Some((f, s)) => (f, s == "selection"),
                    None => (arg, false),
                };
                let path = std::path::absolute(file).unwrap_or_else(|_| std::path::PathBuf::from(file));
                match super::print::Printer::pdf() {
                    None => out.push_str("printpdf: no Microsoft Print to PDF printer\n"),
                    Some(p) => {
                        let title = cell.borrow().tab().title();
                        match p.start(&title, Some(&path)) {
                            Some(p) => cell.borrow_mut().start_print(p, selection, None),
                            None => out.push_str("printpdf: the document didn't start\n"),
                        }
                    }
                }
            }
            // what another Slate hands over with `--read-only`, `--wait`, `-`: the lines of `COPYDATA_OPEN2`, `|` between
            // them (`?read-only|?wait=Local\Slate.Wait.x|C:\a.txt`); `waitevent:<name>` makes the event a `--wait` sets
            "request" => {
                use windows::Win32::Foundation::{LPARAM, WPARAM};
                use windows::Win32::System::DataExchange::COPYDATASTRUCT;
                use windows::Win32::UI::WindowsAndMessaging::{SendMessageW, WM_COPYDATA};
                let text: Vec<u16> = arg.replace('|', "
").encode_utf16().collect();
                let cds = COPYDATASTRUCT { dwData: super::COPYDATA_OPEN2, cbData: (text.len() * 2) as u32, lpData: text.as_ptr() as *mut _ };
                let took = unsafe { SendMessageW(hwnd, WM_COPYDATA, WPARAM(0), LPARAM(&cds as *const _ as isize)) };
                out.push_str(&format!("request: {}
", if took.0 != 0 { "taken" } else { "not taken" }));
                pump(&cell, 0);
            }
            "waitevent" => unsafe {
                let h = windows::Win32::System::Threading::CreateEventW(None, true, false, &windows::core::HSTRING::from(arg));
                WAIT_EVENT.with(|w| w.set(h.map_or(0, |h| h.0 as isize)));
            },
            "copydata" => {
                use windows::Win32::Foundation::{LPARAM, WPARAM};
                use windows::Win32::System::DataExchange::COPYDATASTRUCT;
                use windows::Win32::UI::WindowsAndMessaging::{SendMessageW, WM_COPYDATA};
                let text: Vec<u16> = arg.encode_utf16().collect();
                let cds = COPYDATASTRUCT { dwData: super::COPYDATA_OPEN, cbData: (text.len() * 2) as u32, lpData: text.as_ptr() as *mut _ };
                let took = unsafe { SendMessageW(hwnd, WM_COPYDATA, WPARAM(0), LPARAM(&cds as *const _ as isize)) };
                out.push_str(&format!("copydata: {}\n", if took.0 != 0 { "taken" } else { "not taken" }));
                pump(&cell, 0);
            }
            "uia" => {
                let r = super::uia::command(&cell, arg);
                if r.starts_with("uia failed") {
                    failures += 1;
                }
                if !r.is_empty() {
                    out.push_str(&format!("uia:{arg}: {r}\n"));
                }
            }
            _ => {
                out.push_str(&format!("unknown op {op}\n"));
                failures += 1;
            }
        }
        // Paint after every step, like the real window does after input (unless a step closed the window).
        if !unsafe { windows::Win32::UI::WindowsAndMessaging::IsWindow(hwnd) }.as_bool() {
            continue;
        }
        if let Ok(mut a) = cell.try_borrow_mut() {
            a.update_find_status();
            a.paint();
        }
        // (and screen readers hear what changed, as after a message)
        super::uia::after(&cell);
    }
    let log = std::env::var("SLATE_TEST_LOG").unwrap_or_else(|_| "slate-test.log".into());
    let _ = std::fs::write(&log, &out);
    print!("{out}");
    unsafe {
        let _ = DestroyWindow(hwnd);
        if let Ok(h) = hook {
            let _ = UnhookWindowsHookEx(h);
        }
    }
    drop(cell);
    super::release_app();
    if failures > 0 { 1 } else { 0 }
}
