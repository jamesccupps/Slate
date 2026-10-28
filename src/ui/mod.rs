//! The Windows user interface: the main window, its message loop, single-instance handling and startup.

pub mod actions;
pub mod app;
pub mod commands;
pub mod crash;
pub mod editor;
pub mod findbar;
pub mod gfx;
/// (shared with the other platforms; `super::highlight` keeps working here)
pub use crate::highlight;
pub use crate::settings;
pub use crate::theme;
pub mod install;
pub mod jumplist;
pub mod preview;
pub mod print;
pub mod prompt;
pub mod session;
pub mod structure;
pub mod testmode;
pub mod uia;
pub mod update;
pub mod win;

use std::cell::RefCell;
use std::ffi::OsString;
use std::os::windows::ffi::{OsStrExt, OsStringExt};
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
use windows::Win32::System::Threading::{CreateMutexW, GetCurrentProcessId};
use windows::Win32::UI::Input::Ime::{
    CFS_POINT, COMPOSITIONFORM, ImmGetContext, ImmReleaseContext, ImmSetCompositionFontW, ImmSetCompositionWindow,
};
use windows::Win32::UI::Shell::{DragAcceptFiles, DragFinish, DragQueryFileW, HDROP};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{PCWSTR, w};

const WM_MOUSELEAVE: u32 = 0x02A3;

use actions::{TIMER_DISK, WM_APP_DISK, WM_APP_GPU, WM_APP_JOB};
use app::{App, Cell, Deferred, Hit};
use commands::Cmd;

pub const CLASS: PCWSTR = w!("SlateMainWindow");

/// The main window's class. A Slate running as administrator uses its own, so a normal Slate never finds (and tries
/// to hand files to) a window Windows won't let it talk to; so do a Slate that opened on its own (`settings::guest`)
/// and one in test mode, which a Slate starting later mustn't take for the running one.
pub fn class() -> PCWSTR {
    if win::SCRIPTED.with(|s| s.borrow().is_some()) {
        w!("SlateTestWindow")
    } else if settings::guest() {
        w!("SlateGuestWindow")
    } else if win::elevated() {
        w!("SlateMainWindow.Admin")
    } else {
        CLASS
    }
}
pub(crate) const COPYDATA_OPEN: usize = 0x51A7E;
/// Files with what the command line asked for besides them (`app::Opts`): lines starting with `?` (no file name
/// can) say it — `?read-only`, `?wait=<event>`, `?text=<file>` (standard input, written to a file Slate deletes once
/// read). A Slate from before 0.13.0 doesn't take these: the one starting then opens a window of its own.
pub(crate) const COPYDATA_OPEN2: usize = 0x51A7F;

thread_local! {
    static APP: RefCell<Option<Cell>> = const { RefCell::new(None) };
    /// Where the keyboard focus was when the window was deactivated (put back when it's active again).
    static SAVED_FOCUS: std::cell::Cell<isize> = const { std::cell::Cell::new(0) };
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
    let t = unsafe { windows::Win32::System::SystemInformation::GetLocalTime() };
    let line = format!(
        "[{:04}-{:02}-{:02} {:02}:{:02}:{:02}] Slate {}: {what}\n",
        t.wYear,
        t.wMonth,
        t.wDay,
        t.wHour,
        t.wMinute,
        t.wSecond,
        crash::version()
    );
    settings::append_crash_log(&line);
}

/// Startup steps and when they were done (FILETIME units), for `print:startup` in test mode.
static MARKS: std::sync::Mutex<Vec<(&'static str, u64)>> = std::sync::Mutex::new(Vec::new());

pub fn mark(what: &'static str) {
    let t = unsafe { windows::Win32::System::SystemInformation::GetSystemTimePreciseAsFileTime() };
    MARKS.lock().unwrap().push((what, ((t.dwHighDateTime as u64) << 32) | t.dwLowDateTime as u64));
}

/// The startup marks so far, as milliseconds since the process was created.
pub fn marks() -> Vec<(&'static str, f64)> {
    use windows::Win32::Foundation::FILETIME;
    let (mut created, mut x) = (FILETIME::default(), FILETIME::default());
    let (mut y, mut z) = (FILETIME::default(), FILETIME::default());
    let process = unsafe { windows::Win32::System::Threading::GetCurrentProcess() };
    let _ = unsafe { windows::Win32::System::Threading::GetProcessTimes(process, &mut created, &mut x, &mut y, &mut z) };
    let created = ((created.dwHighDateTime as u64) << 32) | created.dwLowDateTime as u64;
    MARKS.lock().unwrap().iter().map(|&(w, t)| (w, t.saturating_sub(created) as f64 / 10_000.0)).collect()
}

/// Runs queued actions (menus, dialogs, commands) once the App isn't borrowed.
pub fn drain_pending(cell: &Cell) {
    thread_local! {
        static DRAINING: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    }
    // Not again from inside a dialog or menu that one of these shows (its message loop comes through here too):
    // the rest waits until it's closed. An Exit run then would destroy the window under the dialog.
    if DRAINING.with(|d| d.replace(true)) {
        return;
    }
    struct Done;
    impl Drop for Done {
        fn drop(&mut self) {
            DRAINING.with(|d| d.set(false));
        }
    }
    let _done = Done;
    loop {
        let next = match cell.try_borrow_mut() {
            Ok(mut a) => {
                // Files first: an Exit queued meanwhile would close the window before they're in it. (In a tab,
                // they're kept in the session like the rest.)
                let late = LATE_FILES.with(|l| std::mem::take(&mut *l.borrow_mut()));
                for (paths, args, opts) in late {
                    if args { a.open_request(&paths, &opts) } else { a.open_paths(&paths) }
                }
                (!a.pending.is_empty()).then(|| a.pending.remove(0))
            }
            _ => None,
        };
        match next {
            Some(d) => actions::run(cell, d),
            None => break,
        }
    }
}

thread_local! {
    /// Files dropped on the window or sent by another Slate while the app was busy; opened right after.
    /// (and whether they're from a command line: another Slate's, with what it asked for besides them)
    static LATE_FILES: RefCell<Vec<(Vec<PathBuf>, bool, app::Opts)>> = const { RefCell::new(Vec::new()) };
    /// The window is being closed (`Closing`).
    static CLOSING: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Closing the window (`actions::close_window`, while it asks about unsaved changes): another Slate's files aren't
/// taken meanwhile (see WM_COPYDATA). Until it's dropped.
pub struct Closing(bool);

impl Closing {
    pub fn now() -> Closing {
        Closing(CLOSING.with(|c| c.replace(true)))
    }
}

impl Drop for Closing {
    fn drop(&mut self) {
        CLOSING.with(|c| c.set(self.0));
    }
}

/// Opens `paths` now, or as soon as the app isn't busy.
fn open_soon(cell: &Cell, hwnd: HWND, paths: Vec<PathBuf>) {
    match cell.try_borrow_mut() {
        Ok(mut a) => a.open_paths(&paths),
        Err(_) => open_later(hwnd, paths, false, app::Opts::default()),
    }
}

/// Opens `paths` with the next message.
fn open_later(hwnd: HWND, paths: Vec<PathBuf>, args: bool, opts: app::Opts) {
    if paths.is_empty() && opts.text.is_none() && opts.wait.is_none() {
        return;
    }
    LATE_FILES.with(|l| l.borrow_mut().push((paths, args, opts)));
    unsafe {
        let _ = PostMessageW(hwnd, WM_APP_JOB, WPARAM(0), LPARAM(0));
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
        // (Not for files another Slate sent: it waits for the answer, so they're opened just after.)
        if msg != WM_COPYDATA {
            drain_pending(&cell);
        }
        // Screen readers hear what changed (only while they listen).
        uia::after(&cell);
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
        WM_GETOBJECT => uia::on_getobject(hwnd, wp, lp),
        WM_SIZE => {
            if let Ok(mut a) = cell.try_borrow_mut() {
                a.layout();
                a.invalidate();
            }
            Some(LRESULT(0))
        }
        WM_GETMINMAXINFO => {
            let mm = unsafe { &mut *(lp.0 as *mut MINMAXINFO) };
            let dpi = win::dpi_of(hwnd) as i32;
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
            // Light/dark mode or the accent color changed, or high contrast went on or off.
            let colors = lp.0 != 0 && unsafe { PCWSTR(lp.0 as *const u16).to_string() }.unwrap_or_default() == "ImmersiveColorSet";
            if colors || wp.0 as u32 == SPI_SETHIGHCONTRAST.0 {
                if let Ok(mut a) = cell.try_borrow_mut() {
                    a.apply_theme();
                }
            }
            None
        }
        WM_SYSCOLORCHANGE | WM_THEMECHANGED => {
            // (Another contrast theme: new high-contrast colors.)
            if let Ok(mut a) = cell.try_borrow_mut() {
                a.apply_theme();
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
            match cell.try_borrow_mut() {
                Ok(mut a) => {
                    a.focused = true;
                    a.restart_caret();
                    a.invalidate();
                }
                // (Focus moved by code that has the App borrowed: at least start blinking.)
                Err(_) => unsafe {
                    actions::caret_moved();
                    let blink = GetCaretBlinkTime();
                    if blink != 0 && blink != u32::MAX {
                        SetTimer(hwnd, actions::TIMER_CARET, blink.clamp(200, 2000), None);
                    }
                },
            }
            Some(LRESULT(0))
        }
        WM_KILLFOCUS => {
            // No blinking (and repainting) while the keyboard is elsewhere, and the system caret goes with it.
            unsafe {
                let _ = KillTimer(hwnd, actions::TIMER_CARET);
            }
            win::drop_caret();
            if let Ok(mut a) = cell.try_borrow_mut() {
                a.focused = false;
                a.caret_on = true;
                a.invalidate();
            }
            Some(LRESULT(0))
        }
        WM_PARENTNOTIFY | WM_NCLBUTTONDOWN | WM_NCRBUTTONDOWN => {
            // A click in the find box (its edit boxes tell the window) or on the title bar ends the menu bar's
            // keyboard mode, as a click in the text does; otherwise the next letters would open menus.
            let button = matches!((wp.0 & 0xFFFF) as u32, WM_LBUTTONDOWN | WM_RBUTTONDOWN | WM_MBUTTONDOWN);
            if msg != WM_PARENTNOTIFY || button {
                if let Ok(mut a) = cell.try_borrow_mut() {
                    a.disarm_menu_bar();
                }
            }
            None
        }
        WM_CAPTURECHANGED | WM_CANCELMODE => {
            // The mouse capture went elsewhere (Alt+Tab, a menu, a dialog) before the button came up: end any drag,
            // or moving the mouse would go on selecting (and scrolling) with no button held.
            if let Ok(mut a) = cell.try_borrow_mut() {
                a.cancel_drags();
            }
            // (DefWindowProc releases the capture for WM_CANCELMODE.)
            if msg == WM_CANCELMODE { None } else { Some(LRESULT(0)) }
        }
        WM_SYSCOMMAND if (wp.0 & 0xFFF0) as u32 == SC_KEYMENU => {
            // Alt on its own (lParam 0) gives Slate's menu bar the keyboard, rather than the window's hidden system
            // menu; Alt+F and the other menu letters typed in a find box open that menu. Alt+Space (the window menu)
            // and other letters keep Windows' handling.
            let ch = lp.0 as u32;
            let menu = char::from_u32(ch).filter(char::is_ascii_alphabetic).and_then(|c| commands::menu_for_letter(c.to_ascii_uppercase() as u16));
            if ch != 0 && menu.is_none() {
                return None;
            }
            if let Ok(mut a) = cell.try_borrow_mut() {
                match menu {
                    Some(i) => {
                        a.disarm_menu_bar();
                        a.pending.push(Deferred::Menu(i));
                    }
                    None if std::mem::take(&mut a.alt_dragged) => {}
                    None => a.toggle_menu_bar(),
                }
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
                } else if code == EN_CHANGE && id == findbar::ID_REPLACE {
                    a.find.history_at = None;
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
                paths.push(PathBuf::from(OsString::from_wide(&buf[..len])));
            }
            unsafe { DragFinish(drop) };
            open_soon(cell, hwnd, paths);
            Some(LRESULT(0))
        }
        WM_COPYDATA => {
            let cds = unsafe { &*(lp.0 as *const COPYDATASTRUCT) };
            if cds.dwData != COPYDATA_OPEN && cds.dwData != COPYDATA_OPEN2 {
                return None;
            }
            // Closing (asking about unsaved changes, or saving for it): not taken, as the window may be gone in a
            // moment. The other Slate tries again: once this one has ended, it's the Slate (with the session); if
            // the close is called off, this one takes them then. (After 10 s of asking it opens a window of its
            // own, as for a Slate that's busy.)
            if CLOSING.with(|c| c.get()) || cell.try_borrow().is_ok_and(|a| a.closing) {
                return Some(LRESULT(0));
            }
            // (from another process: nothing to read is nothing to open)
            if cds.lpData.is_null() || cds.cbData < 2 {
                return Some(LRESULT(1));
            }
            let units = unsafe { std::slice::from_raw_parts(cds.lpData as *const u16, cds.cbData as usize / 2) };
            let (paths, opts) = if cds.dwData == COPYDATA_OPEN2 { handed_over2(units) } else { (handed_over(units), app::Opts::default()) };
            // Answered at once; opening them (which can take a while on a slow share) comes right after.
            open_later(hwnd, paths, true, opts);
            // (A test's hidden window stays out of the user's way.)
            if !win::testing() {
                unsafe {
                    if IsIconic(hwnd).as_bool() {
                        let _ = ShowWindow(hwnd, SW_RESTORE);
                    }
                    let _ = SetForegroundWindow(hwnd);
                }
            }
            Some(LRESULT(1))
        }
        WM_APP_DISK => {
            if let Ok(mut a) = cell.try_borrow_mut() {
                a.poll_disk();
            }
            Some(LRESULT(0))
        }
        WM_APP_JOB => {
            if let Ok(mut a) = cell.try_borrow_mut() {
                a.poll_jobs();
            }
            Some(LRESULT(0))
        }
        WM_APP_GPU => {
            // (the paint switches: `Gfx::switch_to_gpu`)
            unsafe {
                let _ = windows::Win32::Graphics::Gdi::InvalidateRect(hwnd, None, false);
            }
            Some(LRESULT(0))
        }
        WM_ACTIVATE => {
            if loword(wp.0) as u32 == WA_INACTIVE {
                // Remember where the keyboard was (the find box or the text) for when the window comes back.
                let f = win::focus();
                SAVED_FOCUS.with(|s| s.set(f.0 as isize));
                if let Ok(mut a) = cell.try_borrow_mut() {
                    a.disarm_menu_bar();
                    a.hide_tip();
                }
                return None;
            }
            if let Ok(mut a) = cell.try_borrow_mut() {
                a.check_disk();
            }
            if hiword(wp.0) != 0 {
                return None; // minimized
            }
            // Put the focus back where it was; DefWindowProc would move it to the main window, out of the find box
            // (and a Ctrl+V meant for the find box would paste into the text).
            let saved = HWND(SAVED_FOCUS.with(|s| s.get()) as *mut _);
            // (the box's own visibility: the bar may have switched to another box meanwhile)
            let shown = |h: HWND| unsafe { GetWindowLongPtrW(h, GWL_STYLE) } as u32 & WS_VISIBLE.0 != 0;
            let back = match cell.try_borrow() {
                Ok(a) => a.find.open && a.find.is_edit(saved) && shown(saved),
                Err(_) => false,
            };
            win::set_focus(if back { saved } else { hwnd });
            Some(LRESULT(0))
        }
        WM_IME_STARTCOMPOSITION => {
            if let Ok(mut a) = cell.try_borrow_mut() {
                a.wake_caret();
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
        WM_IME_COMPOSITION | WM_IME_ENDCOMPOSITION => {
            // (typing in the IME: the caret blinks again, as for any key)
            if let Ok(mut a) = cell.try_borrow_mut() {
                a.wake_caret();
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
                    if !a.g.offscreen {
                        session::restored();
                    }
                }
                // Windows may end the process once this returns.
                update::wait_for_swap();
            } else {
                // The shutdown was called off.
                unsafe {
                    let _ = windows::Win32::System::Shutdown::ShutdownBlockReasonDestroy(hwnd);
                }
            }
            Some(LRESULT(0))
        }
        WM_DESTROY => {
            uia::on_destroy(hwnd);
            unsafe { PostQuitMessage(0) };
            Some(LRESULT(0))
        }
        _ => None,
    }
}

/// The single-instance lock, held from start until `release_instance_lock`.
static INSTANCE_LOCK: std::sync::atomic::AtomicIsize = std::sync::atomic::AtomicIsize::new(0);

/// Lets a Slate starting now be the running one (the update restart: this one is closing).
pub fn release_instance_lock() {
    let h = INSTANCE_LOCK.swap(0, std::sync::atomic::Ordering::Relaxed);
    if h != 0 {
        unsafe {
            let _ = windows::Win32::Foundation::CloseHandle(windows::Win32::Foundation::HANDLE(h as *mut _));
        }
    }
}

/// Whether another Slate runs, and took the files.
enum Running {
    /// No: this one is the Slate.
    No,
    /// Yes, and it opened the files (its process id).
    Took(u32),
    /// Yes, but it didn't answer (busy or hung).
    Busy,
}

/// If Slate is already running, hands it the files (and what else the command line asks for: `extra`, the lines of
/// `COPYDATA_OPEN2`). A Slate running as administrator is a separate one, with its own lock, window class and session.
fn forward_to_running(paths: &[PathBuf], extra: &[String], tries: u32) -> Running {
    let lock = instance_lock();
    // (as Windows has them: a name needn't be valid Unicode)
    let mut text: Vec<u16> = Vec::new();
    for line in extra {
        text.extend(line.encode_utf16());
        text.push(b'\n' as u16);
    }
    for (k, p) in paths.iter().enumerate() {
        if k > 0 {
            text.push(b'\n' as u16);
        }
        text.extend(p.as_os_str().encode_wide());
    }
    let kind = if extra.is_empty() { COPYDATA_OPEN } else { COPYDATA_OPEN2 };
    hand_over_as(lock, class(), &text, kind, tries)
}

fn instance_lock() -> PCWSTR {
    if win::elevated() { w!("Local\\Slate.SingleInstance.Admin") } else { w!("Local\\Slate.SingleInstance") }
}

/// `COPYDATA_OPEN2`: the files, and what else was asked for (`?` lines). Standard input's text is read from the file
/// named (one Slate wrote in the temp folder: nothing else is read or deleted).
fn handed_over2(units: &[u16]) -> (Vec<PathBuf>, app::Opts) {
    let mut opts = app::Opts::default();
    let mut paths = Vec::new();
    for line in units.split(|&u| u == b'\n' as u16).filter(|s| !s.is_empty()) {
        if line.first() != Some(&(b'?' as u16)) {
            paths.push(PathBuf::from(OsString::from_wide(line)));
            continue;
        }
        let s = String::from_utf16_lossy(&line[1..]);
        if s == "read-only" {
            opts.read_only = true;
        } else if let Some(name) = s.strip_prefix("wait=") {
            opts.wait = Some(name.to_string());
        } else if let Some(file) = s.strip_prefix("text=") {
            let p = PathBuf::from(file);
            let ours = p.parent() == Some(settings::temp_dir().as_path())
                && p.file_name().is_some_and(|n| n.to_string_lossy().starts_with("slate-stdin-"));
            if ours {
                opts.text = Some(std::fs::read(&p).unwrap_or_default());
                let _ = std::fs::remove_file(&p);
            }
        }
    }
    (paths, opts)
}

/// `slate --wait`: waits until the Slate that took the files sets `event` (their tabs are all closed), or ends.
fn wait_for_tabs(event: windows::Win32::Foundation::HANDLE, pid: u32) {
    use windows::Win32::System::Threading::{INFINITE, OpenProcess, PROCESS_SYNCHRONIZE, WaitForMultipleObjects};
    unsafe {
        match OpenProcess(PROCESS_SYNCHRONIZE, false, pid) {
            Ok(p) => {
                let _ = WaitForMultipleObjects(&[event, p], false, INFINITE);
                let _ = windows::Win32::Foundation::CloseHandle(p);
            }
            Err(_) => {
                let _ = windows::Win32::System::Threading::WaitForSingleObject(event, INFINITE);
            }
        }
    }
}

/// The files another Slate handed over (`forward_to_running`): one per line.
fn handed_over(units: &[u16]) -> Vec<PathBuf> {
    units.split(|&u| u == b'\n' as u16).filter(|s| !s.is_empty()).map(|s| PathBuf::from(OsString::from_wide(s))).collect()
}

/// `forward_to_running` with the lock and window class given (the tests use their own), trying `tries` times
/// 100 ms apart: the running Slate may be starting (its window not there yet, or not ready for files) or closing
/// (its window gone, its lock not yet), and then this one becomes the Slate.
#[cfg(test)]
fn hand_over(lock: PCWSTR, class: PCWSTR, text: &[u16], tries: u32) -> Running {
    hand_over_as(lock, class, text, COPYDATA_OPEN, tries)
}

/// `hand_over` with the kind of message (`COPYDATA_OPEN` or `COPYDATA_OPEN2`).
fn hand_over_as(lock: PCWSTR, class: PCWSTR, text: &[u16], kind: usize, tries: u32) -> Running {
    for _ in 0..tries {
        unsafe {
            match CreateMutexW(None, false, lock) {
                // We're first (or the one before has ended); keep the lock while we run.
                Ok(h) if GetLastError() != ERROR_ALREADY_EXISTS => {
                    INSTANCE_LOCK.store(h.0 as isize, std::sync::atomic::Ordering::Relaxed);
                    return Running::No;
                }
                // (not kept: it would keep the lock there after the running one has ended)
                Ok(h) => {
                    let _ = windows::Win32::Foundation::CloseHandle(h);
                }
                // (it can't be opened at all: look for the window anyway)
                Err(_) => {}
            }
            if let Some(h) = slate_window(class) {
                let cds = COPYDATASTRUCT {
                    dwData: kind,
                    cbData: (text.len() * 2) as u32,
                    lpData: text.as_ptr() as *mut _,
                };
                let mut pid = 0u32;
                GetWindowThreadProcessId(h, Some(&mut pid));
                let _ = AllowSetForegroundWindow(pid);
                // A Slate that hangs doesn't hold this one up.
                let mut res = 0usize;
                let sent = SendMessageTimeoutW(
                    h,
                    WM_COPYDATA,
                    WPARAM(0),
                    LPARAM(&cds as *const _ as isize),
                    SMTO_ABORTIFHUNG | SMTO_BLOCK,
                    10_000,
                    Some(&mut res),
                );
                if sent.0 != 0 && res != 0 {
                    return Running::Took(pid);
                }
                // Busy or hung, its window still there: a window of its own. The window gone meanwhile, or not
                // ready for files yet: look again.
                if sent.0 == 0 && IsWindow(h).as_bool() {
                    return Running::Busy;
                }
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    Running::Busy
}

/// The running Slate's window: one of that class whose process runs as this user with the same rights (another
/// program can make a window of that class too; files go only to a Slate's).
fn slate_window(class: PCWSTR) -> Option<HWND> {
    let me = win::process_identity(unsafe { GetCurrentProcessId() })?;
    let mut after = HWND::default();
    loop {
        let h = unsafe { FindWindowExW(None, after, class, None) }.ok().filter(|h| !h.is_invalid())?;
        let mut pid = 0u32;
        unsafe { GetWindowThreadProcessId(h, Some(&mut pid)) };
        if win::process_identity(pid).as_ref() == Some(&me) {
            return Some(h);
        }
        after = h;
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
            lpszClassName: class(),
            ..Default::default()
        };
        RegisterClassExW(&wc);
        hinst
    }
}

/// Creates the main window (hidden) and its App, where it was last time.
pub fn create_window(hinst: HINSTANCE, s: &settings::Settings) -> HWND {
    // The saved place is in workspace coordinates (GetWindowPlacement's: they leave out a taskbar at the top or
    // left of the monitor), which only SetWindowPlacement takes; creating the window right there first means it
    // starts on that monitor, with that monitor's DPI. (A place on no monitor any more: Windows' default.)
    let saved = s
        .window
        .filter(|p| p.w > 200 && p.h > 150)
        .map(|p| RECT { left: p.x, top: p.y, right: p.x + p.w, bottom: p.y + p.h })
        .filter(|&r| unsafe { !MonitorFromRect(&workspace_to_screen(r), MONITOR_DEFAULTTONULL).is_invalid() });
    let (x, y, w, h) = match saved.map(workspace_to_screen) {
        Some(r) => (r.left, r.top, r.right - r.left, r.bottom - r.top),
        None => (CW_USEDEFAULT, CW_USEDEFAULT, CW_USEDEFAULT, CW_USEDEFAULT),
    };
    unsafe {
        let hwnd = CreateWindowExW(
            WS_EX_ACCEPTFILES,
            class(),
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
        .expect("create window");
        if let Some(r) = saved {
            let wp = WINDOWPLACEMENT {
                length: std::mem::size_of::<WINDOWPLACEMENT>() as u32,
                showCmd: SW_HIDE.0 as u32,
                ptMinPosition: POINT { x: -1, y: -1 },
                ptMaxPosition: POINT { x: -1, y: -1 },
                rcNormalPosition: r,
                ..Default::default()
            };
            let _ = SetWindowPlacement(hwnd, &wp);
        }
        hwnd
    }
}

/// Workspace coordinates (relative to the work area of the monitor they're on) to screen coordinates.
fn workspace_to_screen(r: RECT) -> RECT {
    use windows::Win32::Graphics::Gdi::{GetMonitorInfoW, MONITOR_DEFAULTTONEAREST, MONITORINFO};
    let mut mi = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
    if unsafe { !GetMonitorInfoW(MonitorFromRect(&r, MONITOR_DEFAULTTONEAREST), &mut mi).as_bool() } {
        return r;
    }
    let (dx, dy) = (mi.rcWork.left - mi.rcMonitor.left, mi.rcWork.top - mi.rcMonitor.top);
    RECT { left: r.left + dx, top: r.top + dy, right: r.right + dx, bottom: r.bottom + dy }
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
pub fn restore(app: &mut App, paths: &[PathBuf], opts: &app::Opts) {
    let mut active_id = None;
    // (not in a test: its windows draw offscreen)
    let watch = !app.g.offscreen && settings::persist();
    if app.settings.restore_session && !settings::guest() && watch && session::mark_restoring() {
        // The last start stopped while putting the tabs back, in a way no panic told: it would again. Set aside.
        let kept = session::put_aside();
        app.flash(
            format!(
                "Slate stopped while reopening last time's tabs. Their unsaved text is kept in {} for {} days.",
                kept.display(),
                session::DAMAGED_DAYS
            ),
            true,
        );
    } else if app.settings.restore_session && !settings::guest() {
        match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| restore_session(app))) {
            Ok(id) => active_id = id,
            Err(_) => {
                // It would fail the same way at every start: put it all aside and start without it.
                app.tabs.clear();
                app.active = 0;
                let kept = session::put_aside();
                app.flash(
                    format!(
                        "Slate couldn't reopen last time's tabs (details in crash.log). Their unsaved text is kept in {} \
                         for 30 days.",
                        kept.display()
                    ),
                    true,
                );
            }
        }
    }
    if !paths.is_empty() || opts.text.is_some() {
        app.open_request(paths, opts);
    } else if let Some(id) = active_id {
        if let Some(i) = app.tabs.iter().position(|t| t.id == id) {
            // (what restoring had to say stays: it isn't about the tab shown before)
            let said = app.flash.take();
            app.activate(i);
            keep_message(app, said);
        }
    }
    if app.tabs.is_empty() {
        app.new_untitled();
    }
}

/// Puts back the message `said`, taken before something that changes the tab shown (which clears messages about
/// the tab before), together with what was said meanwhile.
pub fn keep_message(app: &mut App, said: Option<(String, std::time::Instant, bool)>) {
    match (said, app.flash.take()) {
        (Some((m, _, bad)), Some((now, _, now_bad))) => app.flash(format!("{m} {now}"), bad || now_bad),
        (said, now) => app.flash = now.or(said),
    }
}

/// The session's tabs, then unsaved text no tab refers to (see session.rs) as new tabs. Returns the tab to show.
fn restore_session(app: &mut App) -> Option<u64> {
    use crate::core::document::Document;
    let mut active_id = None;
    let (mut missing, mut unreachable) = (Vec::new(), Vec::new());
    let (mut damaged, mut emptied) = (Vec::new(), Vec::new());
    let mut big = Vec::new();
    // Files of the session's tabs being read: waited for once, together.
    let mut reading = Vec::new();
    session::prune_damaged();
    let mut loaded = session::load();
    // (big documents written anew when Slate stopped: the list that has all the newest text)
    session::finish_rewrites(loaded.as_mut().map_or(&mut [][..], |s| &mut s.tabs[..]));
    if let Some(s) = loaded {
        let there = exist_all(&s.tabs);
        for (k, st) in s.tabs.iter().enumerate() {
            let before = app.tabs.len();
            let name = || st.path.as_ref().and_then(|p| p.file_name()).map(|n| n.to_string_lossy().into_owned());
            big.extend(st.pieces.clone());
            let (back, lost) = restore_tab(app, st, there[k].clone(), &mut reading);
            match back {
                Restored::Yes => {}
                Restored::Missing => missing.extend(name()),
                Restored::Unreachable => unreachable.extend(name()),
            }
            let what = || name().unwrap_or_else(|| "an untitled tab".into());
            match lost {
                Some(Lost::Damaged) => damaged.push(what()),
                Some(Lost::Empty) => emptied.push(what()),
                None => {}
            }
            if k == s.active && app.tabs.len() > before {
                active_id = Some(app.tabs[app.tabs.len() - 1].id);
            }
        }
    }
    app.settle(&reading);
    // (what reading them had to say, if a file couldn't be read: kept for the message below, as the tabs added next
    // would clear it)
    let said = app.flash.take();
    // Big documents whose lists of pieces session.json doesn't refer to (Slate stopped in between).
    for st in session::big_orphans(&big) {
        restore_tab(app, &st, (Some(true), None), &mut Vec::new());
    }
    // Added text of a big document too big for a copy, with no list at all (Slate stopped before the first one).
    for (st, list) in session::data_orphans() {
        app.add_restoring(st, Some(list));
    }
    let claimed: Vec<String> = app.tabs.iter().filter_map(|t| t.backup_name.clone()).collect();
    let found = session::orphans(&claimed);
    for (name, bytes) in &found {
        let mut doc = Document::from_text(bytes);
        doc.eol = crate::core::text::detect_eol(&bytes[..bytes.len().min(64 << 10)]);
        doc.mark_dirty();
        let k = app.add_tab(doc);
        app.tabs[k].backup_name = Some(name.clone());
        app.tabs[k].backup_version = app.tabs[k].doc.version;
    }
    let mut msg = Vec::new();
    match found.len() {
        0 => {}
        1 => msg.push("Unsaved text from last time is back in a new tab.".to_string()),
        n => msg.push(format!("Unsaved text from last time is back in {n} new tabs.")),
    }
    if !missing.is_empty() {
        msg.push(format!("Not found any more: {}.", missing.join(", ")));
    }
    if !unreachable.is_empty() {
        msg.push(if unreachable.len() == 1 {
            format!("Not answering yet (a network drive?), so it opens as soon as it does: {}.", unreachable[0])
        } else {
            format!("Not answering yet (a network drive?), so they open as soon as they do: {}.", unreachable.join(", "))
        });
    }
    if !damaged.is_empty() {
        msg.push(format!(
            "Unsaved changes from last time couldn't be read back for: {} (kept in {}).",
            damaged.join(", "),
            session::dir().join("damaged").display()
        ));
    }
    if !emptied.is_empty() {
        msg.push(format!(
            "The unsaved changes kept for {} came back empty (the power went off before they reached the disk).",
            emptied.join(", ")
        ));
    }
    if !msg.is_empty() {
        let bad = !missing.is_empty() || !unreachable.is_empty() || !damaged.is_empty() || !emptied.is_empty();
        app.flash(msg.join(" "), bad);
    }
    keep_message(app, said);
    active_id
}

/// How a tab of the session came back: its file.
enum Restored {
    Yes,
    /// Its file isn't there any more.
    Missing,
    /// Its file (on a network share) didn't answer in time: a tab that waits for it.
    Unreachable,
}

/// A tab's unsaved changes that couldn't be put back (the file, if any, is opened as it is).
enum Lost {
    /// A big document's pieces can't be read back: set aside in `damaged\`.
    Damaged,
    /// Their copy came back as zeros: written just before a power cut, it never reached the disk.
    Empty,
}

/// Whether each tab's file exists, all looked at together (`session::probe`), and its canonical path; None for one
/// that doesn't answer within 2 s (a network share that's gone: Windows can take half a minute to give up, and this
/// runs before the window shows) or answers with something other than "not there".
fn exist_all(tabs: &[session::SessionTab]) -> Vec<(Option<bool>, Option<PathBuf>)> {
    let (tx, rx) = std::sync::mpsc::channel();
    let mut out = vec![(None, None); tabs.len()];
    let mut asked = 0;
    for (i, t) in tabs.iter().enumerate() {
        if let Some(p) = t.path.clone() {
            let tx = tx.clone();
            asked += 1;
            std::thread::spawn(move || {
                let there = session::probe(&p);
                let canon = if there == Some(true) { std::fs::canonicalize(&p).ok() } else { None };
                let _ = tx.send((i, (there, canon)));
            });
        }
    }
    let until = std::time::Instant::now() + std::time::Duration::from_secs(2);
    while asked > 0 {
        let Ok((i, there)) = rx.recv_timeout(until.saturating_duration_since(std::time::Instant::now())) else { break };
        out[i] = there;
        asked -= 1;
    }
    out
}

/// Puts back one tab of the session. `there`: whether its file is there (see `exist_all`), and its canonical path.
/// A tab whose file is being read goes into `reading` (see `App::settle`). Returns how its file came back, and its
/// unsaved changes if they couldn't.
fn restore_tab(
    app: &mut App,
    st: &session::SessionTab,
    (there, canon): (Option<bool>, Option<PathBuf>),
    reading: &mut Vec<u64>,
) -> (Restored, Option<Lost>) {
    use crate::core::document::Document;
    // A big document: put back from its pieces on another thread (it reads its files again); its tab waits.
    let mut lost = None;
    if let Some(name) = &st.pieces {
        match session::read_big(name) {
            Ok(list) => {
                app.add_restoring(st.clone(), Some(list));
                return (Restored::Yes, None);
            }
            // (another program has it just now: read when it's put back, which waits for it)
            Err(session::ListErr::Busy(_)) => {
                app.add_restoring(st.clone(), None);
                return (Restored::Yes, None);
            }
            Err(session::ListErr::Damaged(_)) => {
                // Kept for a look; the file as it is, if any.
                session::set_aside_big(name);
                lost = Some(Lost::Damaged);
            }
        }
    }
    let mut i = None;
    if let Some(name) = &st.backup {
        if let Some(bytes) = session::read_backup(name) {
            if session::is_damaged(&bytes) {
                // Written just before a power cut: the file on disk, if any, is what's left (and the user is told).
                session::set_aside(name);
                lost = Some(Lost::Empty);
            } else {
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
                if let Some(p) = &st.path {
                    app.tabs[k].canon = Some((p.clone(), canon));
                }
                i = Some(k);
            }
        }
    }
    if i.is_none() && st.path.is_some() {
        match there {
            Some(true) => {}
            Some(false) => return (Restored::Missing, lost),
            None => {
                // A tab that waits for its file, opening it as soon as it answers.
                app.add_restoring(session::SessionTab { backup: None, pieces: None, ..st.clone() }, None);
                return (Restored::Unreachable, lost);
            }
        }
        // (back where it was once it's read)
        reading.extend(app.open_from_session(st));
        return (Restored::Yes, lost);
    }
    let Some(i) = i else { return (Restored::Yes, lost) };
    let tab = &mut app.tabs[i];
    if st.untitled > 0 && tab.doc.path.is_none() {
        tab.untitled = st.untitled;
        app.untitled_counter = app.untitled_counter.max(st.untitled);
    }
    place_tab(&mut app.tabs[i], st);
    (Restored::Yes, lost)
}

/// Gives a tab back what the session says of it: the language if the user picked it, and the selection and scroll
/// position.
pub fn place_tab(tab: &mut app::Tab, st: &session::SessionTab) {
    use crate::core::document::Sel;
    // Picked by the user: keep it. Otherwise it was worked out from the file (again now, so a newer Slate's
    // detection applies to tabs from an older one).
    if st.lang_picked {
        tab.lang = st.lang;
        tab.lang_picked = true;
    }
    // The text may not be what the session describes (a backup written after it): keep positions on characters.
    let doc = &tab.doc;
    let mut top = editor::char_start(doc, st.top);
    if top != st.top.min(doc.len()) {
        let ls = doc.line_start_of(top);
        if top - ls <= 64 << 10 {
            top = ls;
        }
    }
    tab.view.sel = Sel::new(editor::char_start(doc, st.anchor), editor::char_start(doc, st.caret));
    tab.view.top = top;
    // (read only on disk counts too)
    tab.read_only |= st.read_only;
    let mut marks = st.bookmarks.clone();
    crate::edit::tidy_marks(&mut marks, doc);
    tab.view.bookmarks = marks;
}

pub fn run(raw: Vec<std::ffi::OsString>) -> i32 {
    // (switches as text; the files named kept exactly: a Windows name needn't be valid Unicode)
    let args: Vec<String> = raw.iter().map(|a| a.to_string_lossy().into_owned()).collect();
    mark("run");
    std::panic::set_hook(Box::new(|info| log_crash(&info.to_string())));
    crash::install();
    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE);
    }
    mark("init");
    if args.first().map(String::as_str) == Some("--uninstall") {
        install::uninstall(args.iter().any(|a| a == "--quiet"));
        return 0;
    }
    if args.first().map(String::as_str) == Some("--install") {
        return if install::install_quiet() { 0 } else { 1 };
    }
    if args.first().map(String::as_str) == Some("--unassociate") {
        install::unassociate();
        return 0;
    }
    if args.first().map(String::as_str) == Some("--test") {
        return testmode::run(&args[1..]);
    }
    // Started by an update: `--updated` (the version before watches this one start, ready to go back to itself),
    // or by 0.2.0's updater `--wait-for <pid>` (it ends first). `--update-failed <version>`: that version didn't
    // start, and this one is back.
    let (mut args, mut raw) = (args, raw);
    // (each switch taken out of both lists)
    let mut take = |args: &mut Vec<String>, i: usize, n: usize| {
        args.drain(i..(i + n).min(args.len()));
        raw.drain(i..(i + n).min(raw.len()));
    };
    let mut updated = false;
    if let Some(i) = args.iter().position(|a| a == "--wait-for") {
        if let Some(pid) = args.get(i + 1).and_then(|p| p.parse().ok()) {
            update::wait_for(pid);
        }
        take(&mut args, i, 2);
        updated = true;
    }
    if let Some(i) = args.iter().position(|a| a == "--updated") {
        take(&mut args, i, 1);
        updated = true;
    }
    let mut update_failed = None;
    if let Some(i) = args.iter().position(|a| a == "--update-failed") {
        update_failed = Some(args.get(i + 1).cloned().unwrap_or_default());
        take(&mut args, i, 2);
    }
    // `--read-only`, `--wait` and `-` (standard input): `app::Opts`.
    let mut opts = app::Opts::default();
    if let Some(i) = args.iter().position(|a| a == "--read-only") {
        take(&mut args, i, 1);
        opts.read_only = true;
    }
    let wait = args.iter().position(|a| a == "--wait");
    if let Some(i) = wait {
        take(&mut args, i, 1);
    }
    if let Some(i) = args.iter().position(|a| a == "-") {
        take(&mut args, i, 1);
        let mut text = Vec::new();
        let _ = std::io::Read::read_to_end(&mut std::io::stdin(), &mut text);
        opts.text = Some(text);
    }
    let paths: Vec<PathBuf> = raw
        .iter()
        .zip(&args)
        .filter(|(_, a)| !a.starts_with("--"))
        .map(|(r, _)| std::path::absolute(r).unwrap_or_else(|_| PathBuf::from(r)))
        .collect();
    match hand_over_request(&paths, &opts, wait.is_some()) {
        Running::Took(_) => return 0,
        Running::No => {}
        Running::Busy => settings::GUEST.store(true, std::sync::atomic::Ordering::Relaxed),
    }
    let _device = gfx::make_device_early();
    if !updated && update_failed.is_none() {
        // (Right after an update the copy before it stays: it goes back in place if the new one fails to start.)
        // On another thread: a big Downloads folder can take a while to list.
        std::thread::spawn(install::clean_old_copies);
    }
    install::refresh_version();
    crate::core::source::set_temp_dir(settings::temp_dir());
    let hinst = register_class();
    let s = settings::Settings::load();
    let hwnd = create_window(hinst, &s);
    gfx::early_window(hwnd);
    let cell = make_app(hwnd);
    {
        let mut a = cell.borrow_mut();
        restore(&mut a, &paths, &opts);
        // (Ahead of what restoring had to say, which stays.)
        let said = a.flash.take();
        let mut first = None;
        if updated {
            first = Some((format!("Slate is updated to {}.", update::Version::current()), false));
        }
        if let Some(v) = update_failed {
            first = Some((format!("Slate {v} didn't start on this PC, so this version is back (with your tabs)."), true));
            a.settings.failed_update = v;
            a.settings.save();
        }
        if settings::guest() {
            first = Some(("Another Slate is busy, so this window opened on its own: it doesn't keep its tabs for next time.".into(), false));
        }
        match (first, said) {
            (Some((m, bad)), Some((s, _, sbad))) => a.flash(format!("{m} {s}"), bad || sbad),
            (Some((m, bad)), None) => a.flash(m, bad),
            (None, Some((s, _, sbad))) => a.flash(s, sbad),
            (None, None) => {}
        }
        a.layout();
        a.update_title();
        a.timer(TIMER_DISK, 2000);
        a.timer(actions::TIMER_UPDATE, 8000);
        // (the taskbar's list of recent files, as they are now: one taken off it elsewhere, say)
        jumplist::update(a.settings.recent.clone());
        // (off the screen until its first frame: `win::cloak`; shown by then in any case)
        a.timer(actions::TIMER_UNCLOAK, 1000);
        a.restart_caret();
    }
    unsafe {
        DragAcceptFiles(hwnd, true);
        let maximized = s.window.is_some_and(|p| p.maximized);
        win::cloak(hwnd);
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
    update::wait_for_swap();
    let restart = cell.borrow().restart_on_exit;
    drop(cell);
    release_app();
    if restart {
        update::restart();
    }
    code
}

/// Hands the files and what the command line asked for besides them to a running Slate (`forward_to_running`).
/// Standard input's text goes in a file in the temp folder, which that Slate reads and deletes. `--wait`: a Slate is
/// started first when none runs (so closing the tabs, not the whole window, ends the wait), and this one returns once
/// that Slate says their tabs are closed, or has ended. Took: there's nothing more for this one to do.
fn hand_over_request(paths: &[PathBuf], opts: &app::Opts, wait: bool) -> Running {
    use windows::Win32::System::Threading::CreateEventW;
    if !wait && opts.text.is_none() && !opts.read_only {
        return forward_to_running(paths, &[], 100);
    }
    let mut extra = Vec::new();
    if opts.read_only {
        extra.push("?read-only".to_string());
    }
    let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos());
    let pid = unsafe { GetCurrentProcessId() };
    let mut text_file = None;
    if let Some(text) = &opts.text {
        let p = settings::temp_dir().join(format!("slate-stdin-{pid}-{stamp}.txt"));
        if std::fs::write(&p, text).is_ok() {
            extra.push(format!("?text={}", p.display()));
            text_file = Some(p);
        }
    }
    let mut event = None;
    if wait {
        let name = format!("Local\\Slate.Wait.{pid}.{stamp}");
        if let Ok(h) = unsafe { CreateEventW(None, true, false, &windows::core::HSTRING::from(name.as_str())) } {
            extra.push(format!("?wait={name}"));
            event = Some(h);
        }
    }
    let mut r = forward_to_running(paths, &extra, 100);
    if matches!(r, Running::No) && event.is_some() {
        // No Slate runs: one is started (without these files), and they go to it once it's there.
        release_instance_lock();
        let started = std::env::current_exe().ok().is_some_and(|exe| {
            use std::os::windows::process::CommandExt;
            const DETACHED_PROCESS: u32 = 0x8;
            std::process::Command::new(exe)
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .creation_flags(DETACHED_PROCESS)
                .spawn()
                .is_ok()
        });
        if started {
            // Once its window is there (it takes the lock before making it: asking for the lock sooner, this one
            // would get it again), for up to 20 s.
            let until = std::time::Instant::now() + std::time::Duration::from_secs(20);
            while slate_window(class()).is_none() && std::time::Instant::now() < until {
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
            r = forward_to_running(paths, &extra, 100);
            if let Running::No = r {
                // (it didn't come up after all: this one is the Slate)
                return r;
            }
        } else {
            r = forward_to_running(paths, &[], 100);
        }
    }
    match (&r, event) {
        (Running::Took(pid), Some(h)) => {
            wait_for_tabs(h, *pid);
            unsafe {
                let _ = windows::Win32::Foundation::CloseHandle(h);
            }
        }
        (Running::Took(_), None) => {}
        (_, h) => {
            // (not taken: this one opens them itself; nothing to wait for but its own window)
            if let Some(h) = h {
                unsafe {
                    let _ = windows::Win32::Foundation::CloseHandle(h);
                }
            }
            if let Some(p) = text_file {
                let _ = std::fs::remove_file(p);
            }
        }
    }
    r
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    use std::time::{Duration, Instant};
    use windows::Win32::Foundation::CloseHandle;
    use windows::core::HSTRING;

    /// What the window of `another_slate` answers to the files sent to it, in turn (the last one from then on), and
    /// what it took.
    static ANSWERS: Mutex<Vec<isize>> = Mutex::new(Vec::new());
    static TOOK: Mutex<Vec<String>> = Mutex::new(Vec::new());

    extern "system" fn another_wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
        if msg == WM_COPYDATA {
            let cds = unsafe { &*(lp.0 as *const COPYDATASTRUCT) };
            let units = unsafe { std::slice::from_raw_parts(cds.lpData as *const u16, cds.cbData as usize / 2) };
            let mut answers = ANSWERS.lock().unwrap();
            let answer = if answers.len() > 1 { answers.remove(0) } else { answers[0] };
            if answer != 0 {
                TOOK.lock().unwrap().push(String::from_utf16_lossy(units));
            }
            return LRESULT(answer);
        }
        unsafe { DefWindowProcW(hwnd, msg, wp, lp) }
    }

    /// Another Slate's window (hidden, of class `class`) on a thread of its own, until `quit`.
    fn another_slate(class: &HSTRING, quit: std::sync::mpsc::Receiver<()>) -> std::thread::JoinHandle<()> {
        let class = class.clone();
        let (ready, is_ready) = std::sync::mpsc::channel();
        let t = std::thread::spawn(move || unsafe {
            let wc = WNDCLASSEXW {
                cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
                lpfnWndProc: Some(another_wndproc),
                lpszClassName: PCWSTR(class.as_ptr()),
                ..Default::default()
            };
            RegisterClassExW(&wc);
            let hwnd = CreateWindowExW(WINDOW_EX_STYLE(0), &class, &class, WS_OVERLAPPED, 0, 0, 10, 10, None, None, None, None)
                .unwrap();
            ready.send(()).unwrap();
            let mut msg = MSG::default();
            while quit.try_recv().is_err() {
                while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                    DispatchMessageW(&msg);
                }
                std::thread::sleep(Duration::from_millis(5));
            }
            let _ = DestroyWindow(hwnd);
        });
        is_ready.recv().unwrap();
        t
    }

    #[test]
    fn a_process_is_known_by_its_user_and_rights() {
        let me = win::process_identity(unsafe { GetCurrentProcessId() });
        assert!(me.as_ref().is_some_and(|(sid, _)| !sid.is_empty()));
        assert_eq!(win::process_identity(unsafe { GetCurrentProcessId() }), me);
        // (the System Idle Process can't be opened)
        assert_eq!(win::process_identity(0), None);
    }

    #[test]
    fn files_go_to_the_running_slate_once_it_can_take_them_or_this_one_becomes_it() {
        // (names of this test's own: never a real Slate's)
        let lock = HSTRING::from(format!("Local\\Slate.Test.{}", std::process::id()));
        let class = HSTRING::from(format!("SlateTest.{}", std::process::id()));
        let (lock_w, class_w) = (PCWSTR(lock.as_ptr()), PCWSTR(class.as_ptr()));
        // none running: this one is the Slate, and holds the lock
        let wide = |s: &str| s.encode_utf16().collect::<Vec<u16>>();
        // (names as Windows has them, also one that isn't valid Unicode)
        let odd = [b'C' as u16, b':' as u16, 0xD800, b'\n' as u16, b'd' as u16];
        assert_eq!(handed_over(&odd), [PathBuf::from(OsString::from_wide(&odd[..3])), PathBuf::from("d")]);
        assert!(matches!(hand_over(lock_w, class_w, &wide("a.txt"), 20), Running::No));
        release_instance_lock();
        // one running, which isn't ready for files at first (its window is there before the rest of it)
        let held = unsafe { CreateMutexW(None, false, lock_w) }.unwrap();
        *ANSWERS.lock().unwrap() = vec![0, 0, 1];
        let (stop, quit) = std::sync::mpsc::channel();
        let other = another_slate(&class, quit);
        assert!(matches!(hand_over(lock_w, class_w, &wide("b.txt\nc.txt"), 20), Running::Took(_)));
        assert_eq!(*TOOK.lock().unwrap(), ["b.txt\nc.txt"]);
        // one that's closing: its window is gone, its lock still there for a moment; then this one is the Slate
        stop.send(()).unwrap();
        other.join().unwrap();
        let held = held.0 as isize;
        let closing = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(300));
            unsafe {
                let _ = CloseHandle(windows::Win32::Foundation::HANDLE(held as *mut _));
            }
        });
        let start = Instant::now();
        assert!(matches!(hand_over(lock_w, class_w, &wide("d.txt"), 20), Running::No));
        assert!(start.elapsed() >= Duration::from_millis(250));
        closing.join().unwrap();
        release_instance_lock();
        assert_eq!(TOOK.lock().unwrap().len(), 1);
    }
}
