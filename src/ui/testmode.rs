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
//! `saving`, `busy`: what `jobs` waits for, `mem`: private bytes and working set), `expect:<what>=<value>`, `answer:save,dont,cancel` (answers for the next prompts, which are never
//! shown in this mode; `asked` lists the prompts so far), `set:restore_session=true` (also `font=<family>`).
//!
//! Lower level: `down:<x>,<y>` / `move:<x>,<y>` / `up:<x>,<y>` (left button, for drags; also where drag scrolling
//! sees the pointer; `down:<x>,<y>,right` or `,middle` for the others), `leave` (WM_MOUSELEAVE), `wheelraw:<delta>`
//! or `wheelraw:ctrl,<delta>` (one WM_MOUSEWHEEL; touchpads send small deltas), `char:<hex>[,<hex>…]` (WM_CHAR
//! through the window procedure, e.g. `char:d83d,de00`), `ime` (WM_IME_COMPOSITION, as typing in an IME), `altkey`
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
//! Slate does it, and whether it was taken); `session:…` says how long it took, and needs `SLATE_DATA_DIR` (as
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
            || t.structure.busy()
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
        "SortLinesDesc" => Cmd::Lines(crate::core::lines::LineOp::SortDesc),
        "RemoveDuplicates" => Cmd::Lines(crate::core::lines::LineOp::Dedupe),
        "RemoveBlank" => Cmd::Lines(crate::core::lines::LineOp::RemoveBlank),
        "TrimTrailing" => Cmd::Lines(crate::core::lines::LineOp::TrimTrailing),
        "Upper" => Cmd::Case(crate::core::lines::CaseOp::Upper),
        "Lower" => Cmd::Case(crate::core::lines::CaseOp::Lower),
        "Title" => Cmd::Case(crate::core::lines::CaseOp::Title),
        "EolLf" => Cmd::SetEol(crate::core::text::Eol::Lf),
        "EolCrlf" => Cmd::SetEol(crate::core::text::Eol::Crlf),
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
    let mut a = cell.borrow_mut();
    let (w, h) = a.client_size();
    unsafe {
        a.update_find_status();
        a.paint();
        let stride = w * 4;
        let mut px = vec![0u8; (stride * h) as usize];
        bmp.CopyPixels(std::ptr::null(), stride, &mut px).map_err(|e| e.to_string())?;
        write_png(path, w, h, &px).map_err(|e| e.to_string())
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
            "print" => out.push_str(&format!("{arg}: {}\n", describe(&cell, arg))),
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
                        super::restore(&mut a, &[]);
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
                drain_pending(&cell);
            }
            "wheelraw" => {
                let (ctrl, delta) = match arg.split_once(',') {
                    Some((m, d)) => (m == "ctrl", d),
                    None => (false, arg),
                };
                super::commands::FORCED_MODS.with(|m| m.set(Some((ctrl, false, false))));
                {
                    let mut a = cell.borrow_mut();
                    let (x, y) = (a.r_edit.x + 100.0, a.r_edit.y + 100.0);
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
