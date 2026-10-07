//! The Windows user interface: the main window, its message loop, single-instance handling and startup.

pub mod actions;
pub mod app;
pub mod commands;
pub mod editor;
pub mod findbar;
pub mod gfx;
pub mod highlight;
pub mod install;
pub mod session;
pub mod settings;
pub mod structure;
pub mod testmode;
pub mod theme;
pub mod win;

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;

use windows::Win32::Foundation::{
    COLORREF, ERROR_ALREADY_EXISTS, GetLastError, HINSTANCE, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM,
};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, EndPaint, HDC, MONITOR_DEFAULTTONULL, MonitorFromRect, PAINTSTRUCT, ScreenToClient, SetBkColor,
    SetTextColor,
};
use windows::Win32::System::Com::{COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE, CoInitializeEx};
use windows::Win32::System::DataExchange::COPYDATASTRUCT;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::CreateMutexW;
use windows::Win32::UI::Input::Ime::{
    CFS_POINT, COMPOSITIONFORM, ImmGetContext, ImmReleaseContext, ImmSetCompositionFontW, ImmSetCompositionWindow,
};
use windows::Win32::UI::Shell::{DragAcceptFiles, DragFinish, DragQueryFileW, HDROP};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{PCWSTR, w};

const WM_MOUSELEAVE: u32 = 0x02A3;

use actions::{TIMER_DISK, WM_APP_JOB};
use app::{App, Cell, Deferred, Hit};
use commands::Cmd;

pub const CLASS: PCWSTR = w!("SlateMainWindow");
const COPYDATA_OPEN: usize = 0x51A7E;

thread_local! {
    static APP: RefCell<Option<Cell>> = const { RefCell::new(None) };
}

pub fn app_cell() -> Option<Cell> {
    APP.with(|a| a.borrow().clone())
}

/// Drops the app while COM and the graphics DLLs are still loaded (dropping them during process exit crashes).
pub fn release_app() {
    let cell = APP.with(|a| a.borrow_mut().take());
    drop(cell);
}

fn loword(v: usize) -> u16 {
    (v & 0xFFFF) as u16
}
fn hiword(v: usize) -> u16 {
    ((v >> 16) & 0xFFFF) as u16
}
fn xy(lp: LPARAM) -> (i32, i32) {
    ((lp.0 & 0xFFFF) as i16 as i32, ((lp.0 >> 16) & 0xFFFF) as i16 as i32)
}

fn log_crash(what: &str) {
    let dir = settings::data_dir();
    let _ = std::fs::create_dir_all(&dir);
    let line = format!("[{:?}] {what}\n", std::time::SystemTime::now());
    use std::io::Write;
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(dir.join("crash.log")) {
        let _ = f.write_all(line.as_bytes());
    }
}

/// Runs queued actions (menus, dialogs, commands) once the App isn't borrowed.
pub fn drain_pending(cell: &Cell) {
    loop {
        let next = match cell.try_borrow_mut() {
            Ok(mut a) if !a.pending.is_empty() => Some(a.pending.remove(0)),
            _ => None,
        };
        match next {
            Some(d) => actions::run(cell, d),
            None => break,
        }
    }
}

fn panic_text(e: &(dyn std::any::Any + Send)) -> String {
    e.downcast_ref::<String>().cloned().or_else(|| e.downcast_ref::<&str>().map(|s| s.to_string())).unwrap_or_else(|| "unknown error".into())
}

/// Runs `f`; if it panics, logs it, keeps the user's work and carries on (None).
fn guarded<R>(cell: &Cell, what: impl FnOnce() -> String, f: impl FnOnce() -> R) -> Option<R> {
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)) {
        Ok(r) => Some(r),
        Err(e) => {
            log_crash(&format!("panic {}: {}", what(), panic_text(&*e)));
            if let Ok(mut a) = cell.try_borrow_mut() {
                let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| a.save_session()));
                a.pending.clear();
                a.flash("Something went wrong (details in crash.log in the settings folder). Your work is kept.", true);
            }
            None
        }
    }
}

extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    let Some(cell) = app_cell() else {
        return unsafe { DefWindowProcW(hwnd, msg, wp, lp) };
    };
    let r = guarded(&cell, || format!("handling message {msg:#x}"), || {
        let r = handle(&cell, hwnd, msg, wp, lp);
        drain_pending(&cell);
        r
    });
    match r {
        Some(Some(v)) => v,
        Some(None) => unsafe { DefWindowProcW(hwnd, msg, wp, lp) },
        None => LRESULT(0),
    }
}

/// Paints, always ending the paint (or Windows sends WM_PAINT forever). A panic while painting is logged and the
/// target thrown away, but not shown as a message: showing it would paint again, and fail again.
fn on_paint(cell: &Cell, hwnd: HWND) {
    use std::sync::atomic::{AtomicU32, Ordering};
    static FAILS: AtomicU32 = AtomicU32::new(0);
    let mut ps = PAINTSTRUCT::default();
    unsafe { BeginPaint(hwnd, &mut ps) };
    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        if let Ok(mut a) = cell.try_borrow_mut() {
            a.update_find_status();
            a.paint();
        }
    }));
    unsafe {
        let _ = EndPaint(hwnd, &ps);
    }
    if let Err(e) = r {
        if FAILS.fetch_add(1, Ordering::Relaxed) < 3 {
            log_crash(&format!("panic while painting: {}", panic_text(&*e)));
        }
        if let Ok(mut a) = cell.try_borrow_mut() {
            // The frame was left half-drawn: start the next one on a fresh target.
            a.g.discard_target();
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| a.save_session()));
        }
    }
}

fn handle(cell: &Cell, hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> Option<LRESULT> {
    match msg {
        WM_PAINT => {
            on_paint(cell, hwnd);
            Some(LRESULT(0))
        }
        WM_ERASEBKGND => Some(LRESULT(1)),
        WM_SIZE => {
            if let Ok(mut a) = cell.try_borrow_mut() {
                a.layout();
                a.invalidate();
            }
            Some(LRESULT(0))
        }
        WM_GETMINMAXINFO => {
            let mm = unsafe { &mut *(lp.0 as *mut MINMAXINFO) };
            let dpi = unsafe { windows::Win32::UI::HiDpi::GetDpiForWindow(hwnd) }.max(96) as i32;
            mm.ptMinTrackSize = POINT { x: 420 * dpi / 96, y: 260 * dpi / 96 };
            Some(LRESULT(0))
        }
        WM_DPICHANGED => {
            let r = unsafe { &*(lp.0 as *const RECT) };
            unsafe {
                let _ = SetWindowPos(hwnd, None, r.left, r.top, r.right - r.left, r.bottom - r.top, SWP_NOZORDER | SWP_NOACTIVATE);
            }
            if let Ok(mut a) = cell.try_borrow_mut() {
                a.update_dpi();
                a.layout();
            }
            Some(LRESULT(0))
        }
        WM_SETTINGCHANGE => {
            if lp.0 != 0 {
                let s = unsafe { PCWSTR(lp.0 as *const u16).to_string() }.unwrap_or_default();
                if s == "ImmersiveColorSet" {
                    if let Ok(mut a) = cell.try_borrow_mut() {
                        a.apply_theme();
                    }
                }
            }
            None
        }
        WM_LBUTTONDOWN | WM_RBUTTONDOWN | WM_MBUTTONDOWN => {
            let (x, y) = xy(lp);
            let b = match msg {
                WM_LBUTTONDOWN => 0,
                WM_RBUTTONDOWN => 1,
                _ => 2,
            };
            if let Ok(mut a) = cell.try_borrow_mut() {
                let (dx, dy) = (a.px_to_dip(x), a.px_to_dip(y));
                a.on_mouse_down(dx, dy, b);
            }
            Some(LRESULT(0))
        }
        WM_LBUTTONUP | WM_RBUTTONUP | WM_MBUTTONUP => {
            let (x, y) = xy(lp);
            let b = match msg {
                WM_LBUTTONUP => 0,
                WM_RBUTTONUP => 1,
                _ => 2,
            };
            if let Ok(mut a) = cell.try_borrow_mut() {
                let (dx, dy) = (a.px_to_dip(x), a.px_to_dip(y));
                a.on_mouse_up(dx, dy, b);
            }
            Some(LRESULT(0))
        }
        WM_MOUSEMOVE => {
            let (x, y) = xy(lp);
            if let Ok(mut a) = cell.try_borrow_mut() {
                let (dx, dy) = (a.px_to_dip(x), a.px_to_dip(y));
                a.on_mouse_move(dx, dy);
            }
            Some(LRESULT(0))
        }
        WM_MOUSELEAVE => {
            if let Ok(mut a) = cell.try_borrow_mut() {
                a.on_mouse_leave();
            }
            Some(LRESULT(0))
        }
        WM_MOUSEWHEEL | WM_MOUSEHWHEEL => {
            let delta = hiword(wp.0) as i16 as i32;
            let (sx, sy) = xy(lp);
            let mut p = POINT { x: sx, y: sy };
            unsafe {
                let _ = ScreenToClient(hwnd, &mut p);
            }
            if let Ok(mut a) = cell.try_borrow_mut() {
                let (dx, dy) = (a.px_to_dip(p.x), a.px_to_dip(p.y));
                a.on_wheel(delta, msg == WM_MOUSEHWHEEL, dx, dy);
            }
            Some(LRESULT(0))
        }
        WM_SETCURSOR => {
            if loword(lp.0 as usize) as u32 != HTCLIENT {
                return None;
            }
            let id = match cell.try_borrow() {
                Ok(a) => match a.hover {
                    Hit::Text => IDC_IBEAM,
                    Hit::StructSplitter => IDC_SIZEWE,
                    _ if a.split_drag.is_some() => IDC_SIZEWE,
                    _ => IDC_ARROW,
                },
                Err(_) => IDC_ARROW,
            };
            unsafe {
                if let Ok(c) = LoadCursorW(None, id) {
                    SetCursor(c);
                }
            }
            Some(LRESULT(1))
        }
        WM_KEYDOWN | WM_SYSKEYDOWN => {
            let used = match cell.try_borrow_mut() {
                Ok(mut a) => a.on_key(wp.0 as u16),
                Err(_) => false,
            };
            if used { Some(LRESULT(0)) } else { None }
        }
        WM_CHAR => {
            if let Ok(mut a) = cell.try_borrow_mut() {
                a.on_char(wp.0 as u16);
            }
            Some(LRESULT(0))
        }
        WM_SYSCHAR => {
            // Alt+letter shortcuts are handled in WM_SYSKEYDOWN; don't beep for them.
            let c = (wp.0 as u8).to_ascii_uppercase();
            if b"FEVOHZ".contains(&c) {
                return Some(LRESULT(0));
            }
            None
        }
        WM_SETFOCUS => {
            if let Ok(mut a) = cell.try_borrow_mut() {
                a.focused = true;
                a.restart_caret();
                a.invalidate();
            }
            Some(LRESULT(0))
        }
        WM_KILLFOCUS => {
            if let Ok(mut a) = cell.try_borrow_mut() {
                a.focused = false;
                a.invalidate();
            }
            Some(LRESULT(0))
        }
        WM_TIMER => {
            if let Ok(mut a) = cell.try_borrow_mut() {
                a.on_timer(wp.0);
            }
            Some(LRESULT(0))
        }
        WM_COMMAND => {
            let code = hiword(wp.0) as u32;
            let id = loword(wp.0) as usize;
            if let Ok(mut a) = cell.try_borrow_mut() {
                if code == EN_CHANGE && id == findbar::ID_FIND {
                    a.on_find_changed();
                } else if code == EN_SETFOCUS || code == EN_KILLFOCUS {
                    a.invalidate();
                }
            }
            Some(LRESULT(0))
        }
        WM_CTLCOLOREDIT => {
            let hdc = HDC(wp.0 as *mut _);
            if let Ok(a) = cell.try_borrow() {
                unsafe {
                    SetTextColor(hdc, COLORREF(win::colorref(a.theme.text)));
                    SetBkColor(hdc, COLORREF(win::colorref(a.theme.input_bg)));
                }
                return Some(LRESULT(a.find.brush.0 as isize));
            }
            None
        }
        WM_DROPFILES => {
            let drop = HDROP(wp.0 as *mut _);
            let n = unsafe { DragQueryFileW(drop, 0xFFFF_FFFF, None) };
            let mut paths = Vec::new();
            for i in 0..n {
                let len = unsafe { DragQueryFileW(drop, i, None) } as usize;
                let mut buf = vec![0u16; len + 1];
                unsafe { DragQueryFileW(drop, i, Some(&mut buf)) };
                paths.push(PathBuf::from(String::from_utf16_lossy(&buf[..len])));
            }
            unsafe { DragFinish(drop) };
            if let Ok(mut a) = cell.try_borrow_mut() {
                a.open_paths(&paths);
            }
            Some(LRESULT(0))
        }
        WM_COPYDATA => {
            let cds = unsafe { &*(lp.0 as *const COPYDATASTRUCT) };
            if cds.dwData != COPYDATA_OPEN {
                return None;
            }
            let units = unsafe { std::slice::from_raw_parts(cds.lpData as *const u16, cds.cbData as usize / 2) };
            let text = String::from_utf16_lossy(units);
            let paths: Vec<PathBuf> = text.split('\n').filter(|s| !s.is_empty()).map(PathBuf::from).collect();
            if let Ok(mut a) = cell.try_borrow_mut() {
                if !paths.is_empty() {
                    a.open_paths(&paths);
                }
            }
            unsafe {
                if IsIconic(hwnd).as_bool() {
                    let _ = ShowWindow(hwnd, SW_RESTORE);
                }
                let _ = SetForegroundWindow(hwnd);
            }
            Some(LRESULT(1))
        }
        WM_APP_JOB => {
            if let Ok(mut a) = cell.try_borrow_mut() {
                a.poll_jobs();
            }
            Some(LRESULT(0))
        }
        WM_ACTIVATE => {
            if loword(wp.0) != 0 {
                if let Ok(mut a) = cell.try_borrow_mut() {
                    a.check_disk();
                    if a.find.open {
                        // keep keyboard focus where it was
                    }
                }
            }
            None
        }
        WM_IME_STARTCOMPOSITION => {
            if let Ok(mut a) = cell.try_borrow_mut() {
                let p = a.with_view(|v, cx| v.caret_point(cx));
                if let Some((x, y)) = p {
                    let (px, py) = (a.dip_to_px(x), a.dip_to_px(y));
                    let font_px = a.dip_to_px(a.style.row_h * 0.8);
                    unsafe {
                        let himc = ImmGetContext(hwnd);
                        let cf = COMPOSITIONFORM { dwStyle: CFS_POINT, ptCurrentPos: POINT { x: px, y: py }, rcArea: RECT::default() };
                        let _ = ImmSetCompositionWindow(himc, &cf);
                        let mut lf = windows::Win32::Graphics::Gdi::LOGFONTW { lfHeight: -font_px, ..Default::default() };
                        for (i, c) in a.settings.font.encode_utf16().take(31).enumerate() {
                            lf.lfFaceName[i] = c;
                        }
                        let _ = ImmSetCompositionFontW(himc, &lf);
                        let _ = ImmReleaseContext(hwnd, himc);
                    }
                }
            }
            None
        }
        WM_MENUSELECT => {
            let top = actions::TOP_MENU.with(|t| t.get());
            actions::on_menu_select(hiword(wp.0) as u32, lp.0, top);
            None
        }
        WM_CLOSE => {
            if let Ok(mut a) = cell.try_borrow_mut() {
                a.pending.push(Deferred::Cmd(Cmd::Exit));
            }
            Some(LRESULT(0))
        }
        WM_QUERYENDSESSION => {
            // Windows is shutting down or signing out. Keep what can be kept; if some unsaved work can't be,
            // say so: Windows then shows "Slate is preventing shutdown" with this reason and lets the user decide.
            let mut ok = true;
            if let Ok(mut a) = cell.try_borrow_mut() {
                a.save_window_placement();
                let session_ok = a.save_session();
                a.settings.save();
                let names: Vec<String> = a
                    .unkept_tabs(session_ok)
                    .iter()
                    .filter_map(|id| a.tabs.iter().find(|t| t.id == *id))
                    .map(|t| t.title())
                    .chain(a.tabs.iter().filter(|t| t.save.is_some()).map(|t| format!("{} (saving)", t.title())))
                    .collect();
                if !names.is_empty() {
                    ok = false;
                    let mut reason = format!("Unsaved changes: {}", names.join(", "));
                    if reason.chars().count() > 200 {
                        // Windows takes at most 256 characters here.
                        reason = reason.chars().take(199).collect::<String>() + "…";
                    }
                    unsafe {
                        let _ = windows::Win32::System::Shutdown::ShutdownBlockReasonCreate(hwnd, &windows::core::HSTRING::from(reason));
                    }
                }
            }
            Some(LRESULT(ok as isize))
        }
        WM_ENDSESSION => {
            if wp.0 != 0 {
                if let Ok(mut a) = cell.try_borrow_mut() {
                    a.save_session();
                    a.settings.save();
                }
            } else {
                // The shutdown was called off.
                unsafe {
                    let _ = windows::Win32::System::Shutdown::ShutdownBlockReasonDestroy(hwnd);
                }
            }
            Some(LRESULT(0))
        }
        WM_DESTROY => {
            unsafe { PostQuitMessage(0) };
            Some(LRESULT(0))
        }
        _ => None,
    }
}

/// If Slate is already running, hands it the files and returns true.
fn forward_to_running(paths: &[PathBuf]) -> bool {
    unsafe {
        let m = CreateMutexW(None, false, w!("Local\\Slate.SingleInstance"));
        if GetLastError() != ERROR_ALREADY_EXISTS {
            // We're first; keep the mutex for the life of the process.
            std::mem::forget(m);
            return false;
        }
        for _ in 0..50 {
            if let Ok(h) = FindWindowW(CLASS, None) {
                if !h.is_invalid() {
                    let text: Vec<u16> =
                        paths.iter().map(|p| p.display().to_string()).collect::<Vec<_>>().join("\n").encode_utf16().collect();
                    let cds = COPYDATASTRUCT {
                        dwData: COPYDATA_OPEN,
                        cbData: (text.len() * 2) as u32,
                        lpData: text.as_ptr() as *mut _,
                    };
                    let mut pid = 0u32;
                    GetWindowThreadProcessId(h, Some(&mut pid));
                    let _ = AllowSetForegroundWindow(pid);
                    SendMessageW(h, WM_COPYDATA, WPARAM(0), LPARAM(&cds as *const _ as isize));
                    return true;
                }
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        false
    }
}

pub fn register_class() -> HINSTANCE {
    unsafe {
        let hinst: HINSTANCE = GetModuleHandleW(None).map(|h| h.into()).unwrap_or_default();
        let icon = LoadImageW(hinst, PCWSTR(1 as *const u16), IMAGE_ICON, 0, 0, LR_DEFAULTSIZE | LR_SHARED)
            .map(|h| HICON(h.0))
            .unwrap_or_default();
        let wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            style: CS_HREDRAW | CS_VREDRAW,
            lpfnWndProc: Some(wndproc),
            hInstance: hinst,
            hIcon: icon,
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
            lpszClassName: CLASS,
            ..Default::default()
        };
        RegisterClassExW(&wc);
        hinst
    }
}

/// Creates the main window (hidden) and its App.
pub fn create_window(hinst: HINSTANCE, s: &settings::Settings) -> HWND {
    let (mut x, mut y, mut w, mut h) = (CW_USEDEFAULT, CW_USEDEFAULT, CW_USEDEFAULT, CW_USEDEFAULT);
    if let Some(p) = s.window {
        let r = RECT { left: p.x, top: p.y, right: p.x + p.w, bottom: p.y + p.h };
        if unsafe { !MonitorFromRect(&r, MONITOR_DEFAULTTONULL).is_invalid() } && p.w > 200 && p.h > 150 {
            (x, y, w, h) = (p.x, p.y, p.w, p.h);
        }
    }
    unsafe {
        CreateWindowExW(
            WS_EX_ACCEPTFILES,
            CLASS,
            w!("Slate"),
            WS_OVERLAPPEDWINDOW | WS_CLIPCHILDREN,
            x,
            y,
            w,
            h,
            None,
            None,
            hinst,
            None,
        )
        .expect("create window")
    }
}

pub fn make_app(hwnd: HWND) -> Cell {
    let raw = hwnd.0 as isize;
    let notify: crate::core::job::Notify = Arc::new(move || unsafe {
        let _ = PostMessageW(HWND(raw as *mut _), WM_APP_JOB, WPARAM(0), LPARAM(0));
    });
    let cell: Cell = Rc::new(RefCell::new(App::new(hwnd, notify)));
    APP.with(|a| *a.borrow_mut() = Some(cell.clone()));
    cell
}

/// Puts back the tabs from last time, then opens `paths`.
pub fn restore(app: &mut App, paths: &[PathBuf]) {
    let mut active_id = None;
    if app.settings.restore_session {
        if let Some(s) = session::load() {
            for (k, st) in s.tabs.iter().enumerate() {
                let before = app.tabs.len();
                restore_tab(app, st);
                if k == s.active && app.tabs.len() > before {
                    active_id = Some(app.tabs[app.tabs.len() - 1].id);
                }
            }
        }
    }
    if !paths.is_empty() {
        app.open_paths(paths);
    } else if let Some(id) = active_id {
        if let Some(i) = app.tabs.iter().position(|t| t.id == id) {
            app.activate(i);
        }
    }
    if app.tabs.is_empty() {
        app.new_untitled();
    }
}

fn restore_tab(app: &mut App, st: &session::SessionTab) {
    use crate::core::document::{Document, Sel};
    let mut i = None;
    if let Some(name) = &st.backup {
        if let Some(bytes) = session::read_backup(name) {
            let mut doc = Document::from_text(&bytes);
            doc.path = st.path.clone();
            doc.encoding = st.encoding;
            doc.bom = st.bom;
            doc.eol = st.eol;
            doc.disk = session::disk_from(st);
            doc.mark_dirty();
            let k = app.add_tab(doc);
            // This tab's text is exactly what that backup file holds.
            app.tabs[k].backup_name = Some(name.clone());
            app.tabs[k].backup_version = app.tabs[k].doc.version;
            i = Some(k);
        }
    }
    if i.is_none() {
        if let Some(p) = &st.path {
            if p.exists() {
                let before = app.tabs.len();
                app.open_paths(std::slice::from_ref(p));
                if app.tabs.len() > before {
                    i = Some(app.tabs.len() - 1);
                }
            }
        }
    }
    let Some(i) = i else { return };
    let tab = &mut app.tabs[i];
    if st.untitled > 0 && tab.doc.path.is_none() {
        tab.untitled = st.untitled;
        app.untitled_counter = app.untitled_counter.max(st.untitled);
    }
    tab.lang = st.lang;
    let len = tab.doc.len();
    tab.view.sel = Sel::new(st.anchor.min(len), st.caret.min(len));
    tab.view.top = st.top.min(len);
}

pub fn run(args: Vec<String>) -> i32 {
    std::panic::set_hook(Box::new(|info| log_crash(&info.to_string())));
    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE);
    }
    if args.first().map(String::as_str) == Some("--uninstall") {
        install::uninstall();
        return 0;
    }
    if args.first().map(String::as_str) == Some("--test") {
        return testmode::run(&args[1..]);
    }
    let paths: Vec<PathBuf> =
        args.iter().filter(|a| !a.starts_with("--")).map(|a| std::path::absolute(a).unwrap_or_else(|_| PathBuf::from(a))).collect();
    if forward_to_running(&paths) {
        return 0;
    }
    install::clean_old_copies();
    crate::core::source::set_temp_dir(settings::temp_dir());
    let hinst = register_class();
    let s = settings::Settings::load();
    let hwnd = create_window(hinst, &s);
    let cell = make_app(hwnd);
    {
        let mut a = cell.borrow_mut();
        restore(&mut a, &paths);
        a.layout();
        a.update_title();
        a.timer(TIMER_DISK, 2000);
        a.restart_caret();
    }
    unsafe {
        DragAcceptFiles(hwnd, true);
        let maximized = s.window.is_some_and(|p| p.maximized);
        let _ = ShowWindow(hwnd, if maximized { SW_SHOWMAXIMIZED } else { SW_SHOWNORMAL });
        // Take the keyboard focus (SetFocus also activates the window), unless whoever started Slate asked for it
        // not to be activated.
        let mut si = windows::Win32::System::Threading::STARTUPINFOW::default();
        windows::Win32::System::Threading::GetStartupInfoW(&mut si);
        let shown = si.wShowWindow as i32;
        let no_activate = (si.dwFlags & windows::Win32::System::Threading::STARTF_USESHOWWINDOW).0 != 0
            && [SW_SHOWNOACTIVATE.0, SW_SHOWNA.0, SW_SHOWMINNOACTIVE.0].contains(&shown);
        if !no_activate {
            let _ = windows::Win32::UI::Input::KeyboardAndMouse::SetFocus(hwnd);
        }
    }
    let code = message_loop(&cell, hwnd);
    drop(cell);
    release_app();
    code
}

pub fn message_loop(cell: &Cell, hwnd: HWND) -> i32 {
    let mut msg = MSG::default();
    unsafe {
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            if msg.hwnd != hwnd && !msg.hwnd.is_invalid() {
                // Keys typed in the find bar's edit boxes.
                if msg.message == WM_KEYDOWN || msg.message == WM_SYSKEYDOWN {
                    let used = guarded(cell, || "handling a find bar key".into(), || {
                        let used = match cell.try_borrow_mut() {
                            Ok(mut a) if a.find.is_edit(msg.hwnd) => a.bar_key(msg.hwnd, msg.wParam.0 as u16),
                            _ => false,
                        };
                        drain_pending(cell);
                        used
                    })
                    .unwrap_or(true);
                    if used {
                        continue;
                    }
                }
                if msg.message == WM_CHAR && matches!(msg.wParam.0, 0x0D | 0x1B | 0x09 | 0x7F) {
                    if let Ok(a) = cell.try_borrow() {
                        if a.find.is_edit(msg.hwnd) {
                            continue;
                        }
                    }
                }
            }
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
    msg.wParam.0 as i32
}

