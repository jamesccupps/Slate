//! Prompts in Slate's own colors: a small dialog with a message and buttons, in place of Windows' task dialogs and
//! message boxes, which stay light when Slate is dark. It's a real dialog made of Windows' own controls (static text,
//! push buttons), so the keyboard (Tab, Enter, Esc, a button's underlined letter), screen readers and high contrast
//! work as in any dialog; only the colors are Slate's (dark buttons through the `DarkMode_Explorer` theme, as for the
//! menus). Ctrl+C copies what it says, like Windows' own dialogs.

use std::cell::Cell;

use windows::Win32::Foundation::{COLORREF, HINSTANCE, HWND, LPARAM, LRESULT, RECT, SIZE, WPARAM};
use windows::Win32::Graphics::Gdi::{
    CreateFontIndirectW, CreateSolidBrush, DT_CALCRECT, DT_EDITCONTROL, DT_LEFT, DT_NOPREFIX, DT_WORDBREAK,
    DeleteObject, DrawTextW, FillRect, GetDC, GetMonitorInfoW, HBRUSH, HDC, HFONT, InvalidateRect, MONITOR_DEFAULTTONEAREST,
    MONITORINFO, MonitorFromRect, MonitorFromWindow, ReleaseDC, SelectObject, SetBkColor, SetTextColor,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::SystemServices::{SS_EDITCONTROL, SS_LEFT, SS_NOPREFIX};
use windows::Win32::UI::Controls::SetWindowTheme;
use windows::Win32::UI::HiDpi::{AdjustWindowRectExForDpi, SystemParametersInfoForDpi};
use windows::Win32::UI::Input::KeyboardAndMouse::{GetKeyState, VK_CONTROL};
use windows::Win32::UI::Shell::{DefSubclassProc, RemoveWindowSubclass, SetWindowSubclass};
use windows::Win32::UI::WindowsAndMessaging::{
    BS_DEFPUSHBUTTON, BS_PUSHBUTTON, CreateDialogIndirectParamW, CreateWindowExW, DLGC_WANTCHARS, DLGTEMPLATE, DM_SETDEFID,
    DS_MODALFRAME, DWLP_MSGRESULT, DestroyWindow, DialogBoxIndirectParamW, EndDialog, GW_OWNER, GWL_EXSTYLE,
    GWL_STYLE, GWLP_USERDATA, GetClientRect, GetDlgItem, GetParent, GetWindow, GetWindowLongPtrW, GetWindowLongW,
    GetWindowRect, HMENU, IDCANCEL, IsIconic, IsWindowVisible, MoveWindow, NONCLIENTMETRICSW, SPI_GETNONCLIENTMETRICS,
    SWP_NOACTIVATE, SWP_NOZORDER, SendMessageW, SetWindowLongPtrW, SetWindowPos, WINDOW_EX_STYLE, WINDOW_LONG_PTR_INDEX,
    WINDOW_STYLE,
    WM_COMMAND, WM_CTLCOLORBTN, WM_CTLCOLORDLG, WM_CTLCOLORSTATIC, WM_DPICHANGED, WM_ERASEBKGND, WM_GETDPISCALEDSIZE,
    WM_INITDIALOG,
    MSG, WM_CHAR, WM_GETDLGCODE, WM_KEYDOWN, WM_NCDESTROY, WM_NEXTDLGCTL, WM_SETFONT, WS_CAPTION, WS_CHILD, WS_GROUP, WS_POPUP, WS_SYSMENU, WS_TABSTOP,
    WS_VISIBLE,
};
use windows::core::{HSTRING, PCWSTR, w};

use super::win::colorref;

/// The colors prompts are drawn in (0xFFRRGGBB), from the window's theme.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Colors {
    pub dark: bool,
    /// Behind the message.
    pub surface: u32,
    /// Behind the buttons (and the title bar).
    pub frame: u32,
    pub text: u32,
    /// The line between the message and the buttons.
    pub border: u32,
}

thread_local! {
    static COLORS: Cell<Option<Colors>> = const { Cell::new(None) };
}

/// The colors the next prompts use; None for Windows' own (high contrast).
pub fn set_colors(c: Option<Colors>) {
    COLORS.with(|x| x.set(c));
}

const MAIN: i32 = 10;
const DETAIL: i32 = 11;
/// The first button's id; the others follow.
const BUTTON: i32 = 100;

struct Prompt<'a> {
    title: &'a str,
    main: &'a str,
    detail: &'a str,
    buttons: &'a [&'a str],
    colors: Option<Colors>,
    font: HFONT,
    big: HFONT,
    surface: HBRUSH,
    frame: HBRUSH,
    line: HBRUSH,
    /// Where the row of buttons starts (client pixels).
    footer: i32,
}

impl Prompt<'_> {
    /// What Ctrl+C copies, laid out as Windows' task dialogs do it.
    fn text(&self) -> String {
        let mut s = format!("[Window Title]\r\n{}\r\n\r\n[Main Instruction]\r\n{}\r\n", self.title, self.main);
        if !self.detail.is_empty() {
            s.push_str(&format!("\r\n[Content]\r\n{}\r\n", self.detail.replace('\n', "\r\n")));
        }
        let buttons: Vec<String> = self.buttons.iter().map(|b| format!("[{}]", b.replace('&', ""))).collect();
        s.push_str(&format!("\r\n{}\r\n", buttons.join(" ")));
        s
    }

    fn free(&mut self) {
        unsafe {
            for h in [self.font, self.big] {
                if !h.is_invalid() {
                    let _ = DeleteObject(h);
                }
            }
            for h in [self.surface, self.frame, self.line] {
                if !h.is_invalid() {
                    let _ = DeleteObject(h);
                }
            }
            (self.font, self.big) = (HFONT::default(), HFONT::default());
            (self.surface, self.frame, self.line) = (HBRUSH::default(), HBRUSH::default(), HBRUSH::default());
        }
    }

    /// Makes the controls (WM_INITDIALOG). (This and `layout` reach the prompt through its pointer, as the messages
    /// they cause, sent while they run, do too.)
    fn init(p: *mut Prompt, hwnd: HWND) {
        unsafe {
            let hinst = instance();
            for (id, text) in [(MAIN, (*p).main), (DETAIL, (*p).detail)] {
                if text.is_empty() {
                    continue;
                }
                let style = WS_CHILD | WS_VISIBLE | WINDOW_STYLE(SS_LEFT.0 | SS_NOPREFIX.0 | SS_EDITCONTROL.0);
                let _ = CreateWindowExW(
                    WINDOW_EX_STYLE(0),
                    w!("STATIC"),
                    &HSTRING::from(text),
                    style,
                    0,
                    0,
                    0,
                    0,
                    hwnd,
                    HMENU(id as usize as _),
                    hinst,
                    None,
                );
            }
            let dark = (*p).colors.is_some_and(|c| c.dark);
            for (i, label) in (*p).buttons.iter().enumerate() {
                let kind = if i == 0 { BS_DEFPUSHBUTTON } else { BS_PUSHBUTTON };
                let mut style = WS_CHILD | WS_VISIBLE | WS_TABSTOP | WINDOW_STYLE(kind as u32);
                if i == 0 {
                    style |= WS_GROUP;
                }
                let id = BUTTON + i as i32;
                let Ok(b) = CreateWindowExW(
                    WINDOW_EX_STYLE(0),
                    w!("BUTTON"),
                    &HSTRING::from(*label),
                    style,
                    0,
                    0,
                    0,
                    0,
                    hwnd,
                    HMENU(id as usize as _),
                    hinst,
                    None,
                ) else {
                    continue;
                };
                if dark {
                    super::win::allow_dark(b);
                    let _ = SetWindowTheme(b, w!("DarkMode_Explorer"), PCWSTR::null());
                }
                let _ = SetWindowSubclass(b, Some(copy_keys), 1, 0);
            }
            match (*p).colors {
                Some(c) => {
                    super::win::style_title_bar(hwnd, c.dark, c.frame, c.text);
                    (*p).surface = CreateSolidBrush(COLORREF(colorref(c.surface)));
                    (*p).frame = CreateSolidBrush(COLORREF(colorref(c.frame)));
                    (*p).line = CreateSolidBrush(COLORREF(colorref(c.border)));
                }
                None => super::win::system_title_bar(hwnd),
            }
            SendMessageW(hwnd, DM_SETDEFID, WPARAM(BUTTON as usize), LPARAM(0));
            Prompt::layout(p, hwnd, super::win::dpi_of(hwnd), None);
            if let Ok(b) = GetDlgItem(hwnd, BUTTON) {
                SendMessageW(hwnd, WM_NEXTDLGCTL, WPARAM(b.0 as usize), LPARAM(1));
            }
        }
    }

    /// Where the parts go at `dpi` (client pixels), measured in `font` and `big`, for a client area at most `max_h`
    /// tall: as wide as the longest line, within limits, then as tall as the text wraps to.
    fn geometry(&self, hdc: HDC, font: HFONT, big: HFONT, dpi: u32, max_h: i32) -> Geometry {
        let px = |v: i32| v * dpi as i32 / 96;
        let (margin, gap) = (px(16), px(10));
        let natural = measure(hdc, big, self.main, None).0.max(measure(hdc, font, self.detail, None).0);
        let labels: Vec<i32> = self.buttons.iter().map(|b| measure_label(hdc, font, b)).collect();
        let line_h = measure(hdc, font, "Ag", None).1;
        let (bh, bgap, bpad) = (px(24).max(line_h + px(8)), px(8), px(12));
        let widths: Vec<i32> = labels.iter().map(|&w| (w + px(24)).max(px(80))).collect();
        let gaps = bgap * (widths.len() as i32 - 1).max(0);
        let text_w = natural.clamp(px(280), px(460)).max(widths.iter().sum::<i32>() + gaps + 2 * bpad - 2 * margin);
        // (a gap between the main line and the detail when there are both)
        let between = if self.main.is_empty() || self.detail.is_empty() { 0 } else { gap };
        let fixed = px(16) + between + px(20) + bh + 2 * px(11);
        let (main_h, detail_h) = fit(
            (measure(hdc, big, self.main, Some(text_w)).1, measure(hdc, big, "Ag", None).1),
            (measure(hdc, font, self.detail, Some(text_w)).1, line_h),
            fixed,
            max_h,
        );
        let main = (px(16), main_h);
        let detail = (main.0 + main_h + between, detail_h);
        let footer = detail.0 + detail_h + px(20);
        let width = text_w + 2 * margin;
        let mut x = width - bpad - widths.iter().sum::<i32>() - gaps;
        let mut buttons = Vec::new();
        for &w in &widths {
            buttons.push((x, w));
            x += w + bgap;
        }
        let height = footer + bh + 2 * px(11);
        Geometry { margin, text_w, main, detail, footer, buttons, button_y: footer + px(11), bh, width, height }
    }

    /// How much bigger than its client area the dialog's window is at `dpi`.
    fn frame(hwnd: HWND, dpi: u32) -> (i32, i32) {
        unsafe {
            let mut r = RECT::default();
            let style = WINDOW_STYLE(GetWindowLongW(hwnd, GWL_STYLE) as u32);
            let ex = WINDOW_EX_STYLE(GetWindowLongW(hwnd, GWL_EXSTYLE) as u32);
            let _ = AdjustWindowRectExForDpi(&mut r, style, false, ex, dpi);
            (r.right - r.left, r.bottom - r.top)
        }
    }

    /// The work area of the screen `monitor`.
    fn work_area(monitor: windows::Win32::Graphics::Gdi::HMONITOR) -> RECT {
        let mut mi = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
        unsafe {
            let _ = GetMonitorInfoW(monitor, &mut mi);
        }
        mi.rcWork
    }

    /// Sizes everything for `dpi` and places the dialog: over the window it belongs to (or at `at`, where Windows
    /// moved it to another screen), within the screen. Text too tall for the screen is cut short, so the buttons stay
    /// on it (Ctrl+C still copies all of it).
    fn layout(p: *mut Prompt, hwnd: HWND, dpi: u32, at: Option<RECT>) {
        unsafe {
            // (the old fonts go once the controls have the new ones)
            let old = [(*p).font, (*p).big];
            ((*p).font, (*p).big) = fonts(dpi);
            let owner = GetWindow(hwnd, GW_OWNER).unwrap_or_default();
            let work = Prompt::work_area(match &at {
                Some(a) => MonitorFromRect(a, MONITOR_DEFAULTTONEAREST),
                None => MonitorFromWindow(if owner.is_invalid() { hwnd } else { owner }, MONITOR_DEFAULTTONEAREST),
            });
            let (fw, fh) = Prompt::frame(hwnd, dpi);
            let hdc = GetDC(hwnd);
            let g = (*p).geometry(hdc, (*p).font, (*p).big, dpi, work.bottom - work.top - fh);
            ReleaseDC(hwnd, hdc);
            for (id, (y, h), font) in [(MAIN, g.main, (*p).big), (DETAIL, g.detail, (*p).font)] {
                if let Ok(c) = GetDlgItem(hwnd, id) {
                    let _ = MoveWindow(c, g.margin, y, g.text_w, h, true);
                    SendMessageW(c, WM_SETFONT, WPARAM(font.0 as usize), LPARAM(1));
                }
            }
            (*p).footer = g.footer;
            for (i, &(x, w)) in g.buttons.iter().enumerate() {
                if let Ok(b) = GetDlgItem(hwnd, BUTTON + i as i32) {
                    let _ = MoveWindow(b, x, g.button_y, w, g.bh, true);
                    SendMessageW(b, WM_SETFONT, WPARAM((*p).font.0 as usize), LPARAM(1));
                }
            }
            for h in old {
                if !h.is_invalid() {
                    let _ = DeleteObject(h);
                }
            }

            // the window around that client area, over its owner and on its screen
            let (w, h) = (g.width + fw, g.height + fh);
            let (mut x, mut y) = match at {
                Some(a) => (a.left, a.top),
                None => {
                    let mut o = work;
                    if !owner.is_invalid() && IsWindowVisible(owner).as_bool() && !IsIconic(owner).as_bool() {
                        let _ = GetWindowRect(owner, &mut o);
                    }
                    (o.left + (o.right - o.left - w) / 2, o.top + (o.bottom - o.top - h) / 3)
                }
            };
            x = x.min(work.right - w).max(work.left);
            y = y.min(work.bottom - h).max(work.top);
            let _ = SetWindowPos(hwnd, None, x, y, w, h.min(work.bottom - work.top), SWP_NOZORDER | SWP_NOACTIVATE);
            let _ = InvalidateRect(hwnd, None, true);
        }
    }
}

/// Where a prompt's parts go at one DPI (client pixels).
struct Geometry {
    margin: i32,
    text_w: i32,
    /// The main line's and the detail's top and height.
    main: (i32, i32),
    detail: (i32, i32),
    /// Where the row of buttons starts, and each button's left edge and width.
    footer: i32,
    buttons: Vec<(i32, i32)>,
    button_y: i32,
    bh: i32,
    width: i32,
    height: i32,
}

/// Windows' message font at `dpi`, and a bigger one for the main line, as task dialogs have.
fn fonts(dpi: u32) -> (HFONT, HFONT) {
    unsafe {
        let mut m = NONCLIENTMETRICSW { cbSize: std::mem::size_of::<NONCLIENTMETRICSW>() as u32, ..Default::default() };
        let _ = SystemParametersInfoForDpi(SPI_GETNONCLIENTMETRICS.0, m.cbSize, Some(&mut m as *mut _ as _), 0, dpi);
        let mut big = m.lfMessageFont;
        big.lfHeight = big.lfHeight * 4 / 3;
        (CreateFontIndirectW(&m.lfMessageFont), CreateFontIndirectW(&big))
    }
}

/// The heights the main line and the detail get, each given as (its height, a line's height), when the client area
/// can be `max_h` tall and everything else in it takes `fixed`: what doesn't fit comes off the detail first, then
/// the main line, in whole lines, so the buttons stay on the screen.
fn fit((main_h, main_line): (i32, i32), (detail_h, detail_line): (i32, i32), fixed: i32, max_h: i32) -> (i32, i32) {
    let whole = |h: i32, line: i32| if line > 0 { h / line * line } else { h };
    let over = fixed + main_h + detail_h - max_h;
    if over <= 0 {
        return (main_h, detail_h);
    }
    let detail = whole((detail_h - over).max(0), detail_line);
    let over = over - (detail_h - detail);
    let main = if over > 0 { whole((main_h - over).max(0), main_line) } else { main_h };
    (main, detail)
}

fn instance() -> HINSTANCE {
    unsafe { GetModuleHandleW(None) }.map(|h| h.into()).unwrap_or_default()
}

/// How big `text` is in `font`: each line on its own (`width` None), or wrapped to `width`.
fn measure(hdc: HDC, font: HFONT, text: &str, width: Option<i32>) -> (i32, i32) {
    unsafe {
        let mut t: Vec<u16> = text.encode_utf16().collect();
        if t.is_empty() {
            return (0, 0);
        }
        let old = SelectObject(hdc, font);
        let mut r = RECT { left: 0, top: 0, right: width.unwrap_or(0), bottom: 0 };
        let mut flags = DT_CALCRECT | DT_LEFT | DT_NOPREFIX;
        if width.is_some() {
            flags |= DT_WORDBREAK | DT_EDITCONTROL;
        }
        DrawTextW(hdc, &mut t, &mut r, flags);
        SelectObject(hdc, old);
        (r.right - r.left, r.bottom - r.top)
    }
}

/// The width of a button's label (its `&` marks the letter that presses it, which takes no room).
fn measure_label(hdc: HDC, font: HFONT, label: &str) -> i32 {
    measure(hdc, font, &label.replacen('&', "", 1), None).0
}

/// The dialog template: an empty dialog (its controls are made in WM_INITDIALOG) with a caption and a close button.
fn template(title: &str) -> Vec<u32> {
    let style = (WS_POPUP | WS_CAPTION | WS_SYSMENU).0 | DS_MODALFRAME as u32;
    // style, extended style, no controls, x, y, width, height (set later), no menu, the standard class, the title
    let mut t: Vec<u16> = vec![style as u16, (style >> 16) as u16, 0, 0, 0, 0, 0, 0, 0, 0, 0];
    t.extend(title.encode_utf16());
    t.push(0);
    // (DWORD-aligned)
    let mut buf = vec![0u32; t.len().div_ceil(2)];
    unsafe { std::ptr::copy_nonoverlapping(t.as_ptr(), buf.as_mut_ptr() as *mut u16, t.len()) };
    buf
}

/// Shows a prompt and waits for the answer: the index of the button pressed, or None (Esc, the close button).
/// Err when the dialog couldn't be made, for the caller to ask another way.
pub(super) fn ask(owner: HWND, title: &str, main: &str, detail: &str, buttons: &[&str]) -> Result<Option<usize>, ()> {
    let colors = COLORS.with(|c| c.get());
    let mut p = Prompt {
        title,
        main,
        detail,
        buttons,
        colors,
        font: HFONT::default(),
        big: HFONT::default(),
        surface: HBRUSH::default(),
        frame: HBRUSH::default(),
        line: HBRUSH::default(),
        footer: 0,
    };
    let t = template(title);
    let r = unsafe {
        let r = DialogBoxIndirectParamW(
            instance(),
            t.as_ptr() as *const DLGTEMPLATE,
            owner,
            Some(dialog_proc),
            LPARAM(&mut p as *mut Prompt as isize),
        );
        p.free();
        r
    };
    match r {
        r if r == IDCANCEL.0 as isize => Ok(None),
        r if r >= BUTTON as isize && ((r - BUTTON as isize) as usize) < buttons.len() => Ok(Some((r - BUTTON as isize) as usize)),
        _ => Err(()),
    }
}

/// Test mode: makes the prompt without showing it and hands its window to `f` (to draw it into a picture).
pub fn render(owner: HWND, title: &str, main: &str, detail: &str, buttons: &[&str], f: impl FnOnce(HWND, i32)) {
    let mut p = Prompt {
        title,
        main,
        detail,
        buttons,
        colors: COLORS.with(|c| c.get()),
        font: HFONT::default(),
        big: HFONT::default(),
        surface: HBRUSH::default(),
        frame: HBRUSH::default(),
        line: HBRUSH::default(),
        footer: 0,
    };
    let t = template(title);
    unsafe {
        if let Ok(hwnd) = CreateDialogIndirectParamW(
            instance(),
            t.as_ptr() as *const DLGTEMPLATE,
            owner,
            Some(dialog_proc),
            LPARAM(&mut p as *mut Prompt as isize),
        ) {
            f(hwnd, p.footer);
            let _ = DestroyWindow(hwnd);
        }
        p.free();
    }
}

extern "system" fn dialog_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> isize {
    unsafe {
        if msg == WM_INITDIALOG {
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, lp.0);
        }
        let p = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut Prompt;
        if p.is_null() {
            return 0;
        }
        match msg {
            WM_INITDIALOG => {
                Prompt::init(p, hwnd);
                // (the focus is on the first button already)
                0
            }
            // (in high contrast, Windows' own colors)
            WM_CTLCOLORDLG | WM_CTLCOLORSTATIC | WM_CTLCOLORBTN => match (*p).colors {
                Some(c) => {
                    let hdc = HDC(wp.0 as _);
                    let buttons = msg == WM_CTLCOLORBTN;
                    SetTextColor(hdc, COLORREF(colorref(c.text)));
                    SetBkColor(hdc, COLORREF(colorref(if buttons { c.frame } else { c.surface })));
                    (if buttons { (*p).frame } else { (*p).surface }).0 as isize
                }
                None => 0,
            },
            WM_ERASEBKGND if (*p).colors.is_some() => {
                let hdc = HDC(wp.0 as _);
                let mut r = RECT::default();
                let _ = GetClientRect(hwnd, &mut r);
                FillRect(hdc, &RECT { bottom: (*p).footer, ..r }, (*p).surface);
                FillRect(hdc, &RECT { top: (*p).footer, ..r }, (*p).frame);
                FillRect(hdc, &RECT { top: (*p).footer, bottom: (*p).footer + 1, ..r }, (*p).line);
                SetWindowLongPtrW(hwnd, WINDOW_LONG_PTR_INDEX(DWLP_MSGRESULT as i32), 1);
                1
            }
            WM_COMMAND => {
                let id = (wp.0 & 0xFFFF) as i32;
                if id >= BUTTON || id == IDCANCEL.0 {
                    let _ = EndDialog(hwnd, id as isize);
                }
                1
            }
            WM_GETDPISCALEDSIZE => {
                // The size it gets at the new DPI (its text doesn't grow exactly in step), so Windows moves it to the
                // other screen in one go.
                let dpi = wp.0 as u32;
                let (font, big) = fonts(dpi);
                let work = Prompt::work_area(MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST));
                let (fw, fh) = Prompt::frame(hwnd, dpi);
                let hdc = GetDC(hwnd);
                let g = (*p).geometry(hdc, font, big, dpi, work.bottom - work.top - fh);
                ReleaseDC(hwnd, hdc);
                let _ = DeleteObject(font);
                let _ = DeleteObject(big);
                *(lp.0 as *mut SIZE) = SIZE { cx: g.width + fw, cy: (g.height + fh).min(work.bottom - work.top) };
                SetWindowLongPtrW(hwnd, WINDOW_LONG_PTR_INDEX(DWLP_MSGRESULT as i32), 1);
                1
            }
            WM_DPICHANGED => {
                let at = *(lp.0 as *const RECT);
                Prompt::layout(p, hwnd, (wp.0 & 0xFFFF) as u32, Some(at));
                1
            }
            _ => 0,
        }
    }
}

/// Ctrl+C on a button copies the prompt's text.
extern "system" fn copy_keys(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM, _id: usize, _data: usize) -> LRESULT {
    unsafe {
        if msg == WM_KEYDOWN && wp.0 == b'C' as usize && GetKeyState(VK_CONTROL.0 as i32) < 0 {
            if let Ok(dlg) = GetParent(hwnd) {
                let p = GetWindowLongPtrW(dlg, GWLP_USERDATA) as *const Prompt;
                if !p.is_null() {
                    super::win::set_clipboard(dlg, (*p).text().as_bytes());
                }
            }
            return LRESULT(0);
        }
        if msg == WM_GETDLGCODE && lp.0 != 0 {
            let m = &*(lp.0 as *const MSG);
            if m.message == WM_CHAR && m.wParam.0 == 3 {
                return LRESULT(DefSubclassProc(hwnd, msg, wp, lp).0 | DLGC_WANTCHARS as isize);
            }
        }
        if msg == WM_NCDESTROY {
            let _ = RemoveWindowSubclass(hwnd, Some(copy_keys), 1);
        }
        DefSubclassProc(hwnd, msg, wp, lp)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ctrl_c_copies_the_prompt_as_task_dialogs_do() {
        let p = Prompt {
            title: "Slate",
            main: "Do you want to save changes to notes.txt?",
            detail: "It changed.\nTwice.",
            buttons: &["&Save", "Do&n't save", "Cancel"],
            colors: None,
            font: HFONT::default(),
            big: HFONT::default(),
            surface: HBRUSH::default(),
            frame: HBRUSH::default(),
            line: HBRUSH::default(),
            footer: 0,
        };
        assert_eq!(
            p.text(),
            "[Window Title]\r\nSlate\r\n\r\n[Main Instruction]\r\nDo you want to save changes to notes.txt?\r\n\r\n[Content]\r\nIt changed.\r\nTwice.\r\n\r\n[Save] [Don't save] [Cancel]\r\n"
        );
        // the template: style, no extended style, no controls, zero size, no menu, the standard class, the title
        let t = template("Hi");
        let words: Vec<u16> = t.iter().flat_map(|d| [*d as u16, (*d >> 16) as u16]).collect();
        assert_eq!(&words[2..11], &[0; 9]);
        assert_eq!(&words[11..14], &[b'H' as u16, b'i' as u16, 0]);
    }

    #[test]
    fn text_taller_than_the_screen_leaves_the_buttons_on_it() {
        // it fits: as it is
        assert_eq!(fit((30, 30), (200, 20), 100, 1000), (30, 200));
        // too tall: the detail gives way, in whole lines, and everything fits
        let (m, d) = fit((30, 30), (2000, 20), 100, 1000);
        assert_eq!(m, 30);
        assert!(100 + m + d <= 1000 && d % 20 == 0 && d > 1000 - 100 - 30 - 20, "{d}");
        // still too tall without the detail: the main line too
        let (m, d) = fit((900, 30), (300, 20), 100, 500);
        assert_eq!(d, 0);
        assert!(100 + m <= 500 && m % 30 == 0 && m > 0, "{m}");
    }
}
