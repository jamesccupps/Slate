//! `Slate.exe --test <commands…>` (or `--test script.txt`, one command per line): drives the real app in a hidden
//! window and renders frames offscreen into PNG files, so the UI can be checked without touching the desktop.
//! Settings and the session are never written in this mode.
//!
//! With `SLATE_TEST_VISIBLE=1` the window is shown instead (on top, without taking the keyboard focus), it draws
//! through the real swap chain, and `shot` captures what is actually on screen, native edit boxes included.
//!
//! Commands: `size:1200x800`, `theme:dark|light`, `open:<path>`, `type:<text>` (`\n`, `\r` and `\t` allowed),
//! `key:<combo>` (e.g. `ctrl+shift+k`, `enter`, `pagedown`, `apps`), `cmd:<Name>` (a menu command, e.g. `JsonFormat`),
//! `find:<text>`, `replace:<text>`, `goto:<line>`, `saveas:<path>`, `click:<x>,<y>`, `dblclick:<x>,<y>`,
//! `wheel:<rows>`, `wait:<ms>`, `jobs` (wait for background work), `checkdisk` (look for files changed on disk,
//! as the window does every 2 s; then `jobs`), `shot:<file.png>`, `print:<what>`
//! (`text`, `sel`, `status`, `lines`, `title`, `top`, `find`, `tabs`, `dirty`, `asked`, `clipboard`, `window`,
//! `saving`), `expect:<what>=<value>`, `answer:save,dont,cancel` (answers for the next prompts, which are never
//! shown in this mode; `asked` lists the prompts so far), `set:restore_session=true`.
//!
//! Lower level: `down:<x>,<y>` / `move:<x>,<y>` / `up:<x>,<y>` (left button, for drags; also where drag scrolling
//! sees the pointer), `wheelraw:<delta>` or `wheelraw:ctrl,<delta>` (one WM_MOUSEWHEEL; touchpads send small
//! deltas), `char:<hex>[,<hex>…]` (WM_CHAR through the window procedure, e.g. `char:d83d,de00`), `altkey` (Alt
//! pressed and released alone: WM_SYSCOMMAND SC_KEYMENU), `altgr:on|off` (pretend Ctrl+Alt+letter types a
//! character, like AltGr on a Polish keyboard), `activate` / `deactivate` (WM_ACTIVATE), `cancelmode`
//! (WM_CANCELMODE: something took the mouse capture), `timer:<id>` (run a timer's tick now; 3 = disk check, 4 = drag
//! scrolling). More `print:` values: `focus` (main, find, replace, goto), `armed` (menu bar title with the
//! keyboard), `opened` (menus that would have opened: native menus are never shown in this mode), `keys0`…`keys4`
//! (each menu item's access key), `scrollx`, `zoom`, `topline`, `drag`, `wintitle`, `tabnames`, `indent`.
//!
//! Also: `args:<path>` (open it as if named on the command line: a missing file becomes a new one), `lang:<name>`
//! (pick the language), `hit:<x>,<y>` / `hover:<x>,<y>` (what's at a point / move the mouse
//! there), `scrollto:<0..1>`, `endsession` (what a Windows shutdown asks), `t:<label>` (a timing mark), `temp:<dir>`,
//! `persist` (write settings and the session; only with `SLATE_DATA_DIR` set, never into the real data folder),
//! `session:save|soon|restore` (write the session now / on another thread as the timer does / restore it), `guest`
//! (as if another Slate was running but didn't answer: nothing is kept for next time), `crash` (a native crash, to
//! try the minidump; the run ends there).

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use windows::Win32::Foundation::HWND;
use windows::Win32::Graphics::Direct2D::{
    D2D1_FEATURE_LEVEL_DEFAULT, D2D1_RENDER_TARGET_PROPERTIES, D2D1_RENDER_TARGET_TYPE_DEFAULT,
    D2D1_RENDER_TARGET_USAGE_NONE,
};
use windows::Win32::Graphics::Imaging::{
    CLSID_WICImagingFactory, GUID_WICPixelFormat32bppPBGRA, IWICImagingFactory, WICBitmapCacheOnLoad,
};
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance};
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::WindowsAndMessaging::{
    DestroyWindow, DispatchMessageW, MSG, PM_REMOVE, PeekMessageW, SWP_NOMOVE, SWP_NOZORDER, SetWindowPos,
    TranslateMessage,
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

fn busy(cell: &Cell) -> bool {
    let a = cell.borrow();
    a.tabs.iter().any(|t| {
        t.load_job.is_some()
            || t.index_job.is_some()
            || t.save.is_some()
            || t.task.is_some()
            || t.search.job.is_some()
            || t.find_job.is_some()
            || t.structure.busy()
    }) || a.disk_job.is_some()
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
            fn labels(items: &[super::commands::Item]) -> String {
                items
                    .iter()
                    .map(|it| match it {
                        super::commands::Item::Cmd { label, enabled, .. } => format!("{}{}", label.replace('&', ""), if *enabled { "" } else { " (off)" }),
                        super::commands::Item::Sep => "-".into(),
                        super::commands::Item::ColBreak => "||".into(),
                        super::commands::Item::Sub { label, items } => format!("{} [{}]", label.replace('&', ""), labels(items)),
                    })
                    .collect::<Vec<_>>()
                    .join(" | ")
            }
            let items = a.menu_items(m[4..].parse().unwrap_or(0));
            labels(&items)
        }
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
        "notice" => a.tab().notice.as_ref().map(|n| n.text.clone()).unwrap_or_default(),
        "focus" => {
            let f = unsafe { GetFocus() };
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
        _ => format!("(unknown: {what})"),
    }
}

pub fn run(args: &[String]) -> i32 {
    super::settings::NO_PERSIST.store(true, std::sync::atomic::Ordering::Relaxed);
    super::win::SCRIPTED.with(|s| *s.borrow_mut() = Some(Default::default()));
    let lines: Vec<String> = if args.len() == 1 && Path::new(&args[0]).is_file() {
        std::fs::read_to_string(&args[0]).unwrap_or_default().lines().map(String::from).collect()
    } else {
        args.to_vec()
    };
    let hinst = register_class();
    let s = Settings { restore_session: false, ..Default::default() };
    let hwnd = create_window(hinst, &s);
    let cell = make_app(hwnd);
    {
        let mut a = cell.borrow_mut();
        a.settings = s;
        a.apply_theme();
        a.new_untitled();
    }
    let visible = std::env::var_os("SLATE_TEST_VISIBLE").is_some();
    let set_size = |hwnd: HWND, w: i32, h: i32| unsafe {
        use windows::Win32::UI::WindowsAndMessaging::{HWND_TOPMOST, SWP_NOACTIVATE, SWP_SHOWWINDOW};
        if visible {
            let _ = SetWindowPos(hwnd, HWND_TOPMOST, 40, 40, w, h, SWP_NOACTIVATE | SWP_SHOWWINDOW);
        } else {
            let _ = SetWindowPos(hwnd, None, 0, 0, w, h, SWP_NOMOVE | SWP_NOZORDER);
        }
    };
    set_size(hwnd, 1200, 800);
    if !visible {
        let _ = offscreen(&cell);
    }
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
                    if !visible {
                        let _ = offscreen(&cell);
                    }
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
                        let f = unsafe { GetFocus() };
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
            "crash" => unsafe { std::ptr::null_mut::<u8>().write_volatile(1) },
            // As if another Slate was running but didn't answer: this window keeps nothing for next time.
            "guest" => super::settings::GUEST.store(true, std::sync::atomic::Ordering::Relaxed),
            "session" => {
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
            }
            "down" | "move" | "up" => {
                let (x, y) = arg.split_once(',').map(|(x, y)| (x.parse().unwrap_or(0.0), y.parse().unwrap_or(0.0))).unwrap_or((0.0, 0.0));
                super::actions::TEST_POINTER.with(|p| p.set(Some((x, y))));
                {
                    let mut a = cell.borrow_mut();
                    match op {
                        "down" => {
                            a.on_mouse_move(x, y);
                            a.on_mouse_down(x, y, 0);
                        }
                        "move" => a.on_mouse_move(x, y),
                        _ => a.on_mouse_up(x, y, 0),
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
            "altkey" => {
                use windows::Win32::Foundation::{LPARAM, WPARAM};
                use windows::Win32::UI::WindowsAndMessaging::{SC_KEYMENU, SendMessageW, WM_SYSCOMMAND};
                unsafe { SendMessageW(hwnd, WM_SYSCOMMAND, WPARAM(SC_KEYMENU as usize), LPARAM(0)) };
            }
            "altgr" => super::commands::FORCED_ALTGR.with(|f| f.set(arg == "on")),
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
    }
    drop(cell);
    super::release_app();
    if failures > 0 { 1 } else { 0 }
}
