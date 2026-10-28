//! File → Print preview… on Windows: a window of its own with the pages as they'll print, one at a time, scaled to
//! fit it (whole page, or the page's width with the rest a scroll away). The pages are the printer's own: laid out
//! for the printer chosen last (or the default one) and drawn into metafiles made for it (`print::Pages`), which the
//! window plays back at its size. A bar at the top: the arrows and the page count, Page width, the options Print
//! has (colors, line numbers, header and footer: the settings, so they're what Print does too), Page setup
//! (Windows' dialog: paper, orientation, margins), Print (on to the print dialog) and Close. Keys: arrows and
//! Page Up / Page Down from page to page, Home / End, Ctrl+P, Esc. It's modal, like a dialog: the main window
//! waits, its timers and jobs going on. Drawn with GDI (metafiles play back there), in the theme's colors.

use std::cell::RefCell;

use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, POINT, RECT, SIZE, WPARAM};
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{HSTRING, PCWSTR, w};

use crate::core::buffer::Snapshot;
use crate::core::job::{Failure, Job, Notify};
use crate::edit::group;
use crate::highlight::Lang;
use crate::print::{self, Row, TooMany};
use crate::theme::Theme;

use super::print::{Geometry, Last, Pages, Printer, Style};

/// What to preview: `[from, to)` of a text, the number of the line `from` is on, its language.
pub struct Source {
    pub snap: Snapshot,
    pub from: u64,
    pub to: u64,
    pub line: u64,
    pub line_start: u64,
    pub lang: Lang,
}

/// How the preview ended.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Outcome {
    Closed,
    /// Print… was clicked: the print dialog comes next.
    Print,
}

/// A layout on another thread.
enum Laid {
    Rows(Vec<Row>),
    TooMany,
    Stopped,
    Failed(String),
}

impl Failure for Laid {
    fn failure(msg: &str) -> Laid {
        Laid::Failed(msg.to_string())
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Item {
    Prev,
    Count,
    Next,
    Width,
    Colors,
    Numbers,
    Header,
    Setup,
    Print,
    Close,
}

const TIMER_LAYOUT: usize = 1;
const TIMER_SCRIPT: usize = 2;
const WM_MOUSELEAVE: u32 = 0x02A3;
const BAR_H: i32 = 44;

struct State {
    hwnd: HWND,
    owner: HWND,
    last: Last,
    style: Style,
    source: Source,
    notify: Notify,
    pages: Option<Pages>,
    /// The layout being worked out, and the printer and page it's for.
    job: Option<(Job<Laid>, Printer, Geometry)>,
    error: Option<String>,
    page: usize,
    width: bool,
    scroll: i32,
    items: Vec<(Item, RECT)>,
    hover: Option<Item>,
    pressed: Option<Item>,
    theme: Theme,
    outcome: Outcome,
    done: bool,
    /// The options or margins were changed (the caller keeps them in the settings).
    changed: bool,
}

thread_local! {
    static STATE: RefCell<Option<State>> = const { RefCell::new(None) };
    /// Test mode's steps for the window (`previewrun:`), while it runs one.
    pub static SCRIPT: RefCell<Option<Script>> = const { RefCell::new(None) };
}

/// Test mode: what to do in the preview window instead of a person (`previewrun:`). The window runs as it does for
/// real (the printer looked up, the layout's job and timer, the bar's clicks, keys), only never shown: each step
/// once the layout is done, after a frame drawn as the screen would show it. Steps: `click:<item>` (`next`,
/// `colors`…; `setup` only notes it, as the dialog is never shown), `key:<left|right|home|end|up|down|esc>`,
/// `wheel:<delta>`, `size:<w>x<h>` (the window's, as dragging its edge: not under its minimum), `shot:<png>`,
/// `state`. What they find goes in `out`.
pub struct Script {
    pub steps: std::collections::VecDeque<String>,
    pub out: String,
    pub shot: fn(&std::path::Path, i32, i32, &[u8]) -> Result<(), String>,
}

fn note(s: String) {
    SCRIPT.with(|c| {
        if let Some(c) = c.borrow_mut().as_mut() {
            c.out.push_str(&s);
            c.out.push('\n');
        }
    });
}

fn with<R>(f: impl FnOnce(&mut State) -> R) -> Option<R> {
    STATE.with(|s| s.try_borrow_mut().ok().and_then(|mut s| s.as_mut().map(f)))
}

fn colorref(argb: u32) -> COLORREF {
    COLORREF(((argb >> 16) & 0xFF) | (argb & 0xFF00) | ((argb & 0xFF) << 16))
}

/// The theme's see-through colors (hover, pressed: drawn over things with Direct2D) laid over `under`, for GDI.
fn over(c: u32, under: u32) -> u32 {
    let a = c >> 24;
    let mix = |s: u32| (((c >> s) & 0xFF) * a + ((under >> s) & 0xFF) * (255 - a)) / 255;
    0xFF00_0000 | (mix(16) << 16) | (mix(8) << 8) | mix(0)
}

impl State {
    /// Lays the text out again (for the printer chosen last, with the style as it is now), on another thread.
    fn relayout(&mut self) {
        self.pages = None;
        self.drop_job();
        self.error = None;
        let Some(printer) = self.last.printer(self.owner) else {
            self.error = Some(NO_PRINTER.into());
            return;
        };
        let geo = Geometry::new(printer.0, &self.style);
        let shape = geo.shape(&self.style);
        let s = &self.source;
        let (snap, from, to, line, line_start, lang) = (s.snap.clone(), s.from, s.to, s.line, s.line_start, s.lang);
        let job = Job::spawn(to - from, self.notify.clone(), move |ctx| {
            let state = print::state_at(&snap, lang, line_start, from);
            let text = print::Text { snap: &snap, from, to, line, lang, state };
            let mut going = |done: u64| {
                ctx.set(done);
                !ctx.cancelled()
            };
            match print::layout(&text, shape, &mut going) {
                Ok(Some(rows)) => Laid::Rows(rows),
                Ok(None) => Laid::Stopped,
                Err(TooMany) => Laid::TooMany,
            }
        });
        self.job = Some((job, printer, geo));
        unsafe {
            SetTimer(self.hwnd, TIMER_LAYOUT, 30, None);
        }
    }

    /// Gives up a layout being worked out (the job only uses the shape, never the printer's DC).
    fn drop_job(&mut self) {
        if let Some((job, printer, _)) = self.job.take() {
            drop(job);
            printer.discard();
        }
    }

    /// Takes a finished layout.
    fn poll(&mut self) {
        let Some((job, ..)) = self.job.as_mut() else { return };
        let Some(laid) = job.take() else { return };
        let (_, printer, geo) = self.job.take().unwrap();
        unsafe {
            let _ = KillTimer(self.hwnd, TIMER_LAYOUT);
        }
        match laid {
            Laid::Rows(rows) => {
                let pages = Pages::new(printer, geo, self.style.clone(), rows);
                self.page = self.page.min(pages.count() - 1);
                self.pages = Some(pages);
            }
            Laid::TooMany => {
                printer.discard();
                self.error = Some(super::print::too_many());
            }
            Laid::Stopped => printer.discard(),
            Laid::Failed(m) => {
                printer.discard();
                self.error = Some(m);
            }
        }
    }

    fn count(&self) -> usize {
        self.pages.as_ref().map_or(0, |p| p.count())
    }

    fn go(&mut self, page: isize) {
        let n = self.count();
        if n > 0 {
            self.page = page.clamp(0, n as isize - 1) as usize;
            self.scroll = 0;
        }
    }

    fn act(&mut self, item: Item) {
        match item {
            Item::Prev => self.go(self.page as isize - 1),
            Item::Next => self.go(self.page as isize + 1),
            Item::Count => {}
            Item::Width => {
                self.width = !self.width;
                self.scroll = 0;
            }
            Item::Colors => self.option(|s| s.colors = !s.colors),
            Item::Numbers => self.option(|s| s.numbers = !s.numbers),
            Item::Header => self.option(|s| s.header = !s.header),
            // (shows a dialog: done by `act`, outside the state's borrow)
            Item::Setup => {}
            Item::Print => {
                self.outcome = Outcome::Print;
                self.done = true;
            }
            Item::Close => self.done = true,
        }
    }

    fn option(&mut self, f: impl FnOnce(&mut Style)) {
        f(&mut self.style);
        self.changed = true;
        self.relayout();
    }

    fn on(&self, item: Item) -> Option<bool> {
        match item {
            Item::Width => Some(self.width),
            Item::Colors => Some(self.style.colors),
            Item::Numbers => Some(self.style.numbers),
            Item::Header => Some(self.style.header),
            _ => None,
        }
    }

    fn enabled(&self, item: Item) -> bool {
        match item {
            Item::Prev => self.page > 0,
            Item::Next => self.page + 1 < self.count(),
            Item::Print => self.pages.is_some(),
            _ => true,
        }
    }

    fn label(&self, item: Item) -> String {
        match item {
            // (chevrons of the icon font)
            Item::Prev => "\u{E76B}".into(),
            Item::Next => "\u{E76C}".into(),
            Item::Count => match self.count() {
                0 if self.job.is_some() => "Laying out the pages…".into(),
                0 => String::new(),
                n => format!("Page {} of {}", group(self.page as u64 + 1), group(n as u64)),
            },
            Item::Width => "Page width".into(),
            Item::Colors => "Colors".into(),
            Item::Numbers => "Line numbers".into(),
            Item::Header => "Header and footer".into(),
            Item::Setup => "Page setup…".into(),
            Item::Print => "Print…".into(),
            Item::Close => "Close".into(),
        }
    }
}

const NO_PRINTER: &str = "There's no printer to lay the pages out for. Add one in Windows' Settings.";

/// The window's scale (its DPI over 96).
fn scale(hwnd: HWND) -> f32 {
    let dpi = unsafe { GetDpiForWindow(hwnd) };
    if dpi == 0 { 1.0 } else { dpi as f32 / 96.0 }
}

fn ui_font(k: f32) -> HFONT {
    unsafe {
        CreateFontW(
            -((9.0 * 96.0 / 72.0 * k).round() as i32),
            0,
            0,
            0,
            FW_NORMAL.0 as i32,
            0,
            0,
            0,
            DEFAULT_CHARSET.0 as u32,
            OUT_DEFAULT_PRECIS.0 as u32,
            CLIP_DEFAULT_PRECIS.0 as u32,
            CLEARTYPE_QUALITY.0 as u32,
            (VARIABLE_PITCH.0 | FF_SWISS.0) as u32,
            w!("Segoe UI"),
        )
    }
}

fn icon_font(k: f32) -> HFONT {
    unsafe {
        CreateFontW(
            -((12.0 * k).round() as i32),
            0,
            0,
            0,
            FW_NORMAL.0 as i32,
            0,
            0,
            0,
            DEFAULT_CHARSET.0 as u32,
            OUT_DEFAULT_PRECIS.0 as u32,
            CLIP_DEFAULT_PRECIS.0 as u32,
            CLEARTYPE_QUALITY.0 as u32,
            0,
            w!("Segoe MDL2 Assets"),
        )
    }
}

fn text_w(dc: HDC, s: &str) -> i32 {
    if s.is_empty() {
        return 0;
    }
    let u: Vec<u16> = s.encode_utf16().collect();
    let mut sz = SIZE::default();
    unsafe {
        let _ = GetTextExtentPoint32W(dc, &u, &mut sz);
    }
    sz.cx
}

fn fill(dc: HDC, r: RECT, argb: u32) {
    unsafe {
        let b = CreateSolidBrush(colorref(argb));
        FillRect(dc, &r, b);
        let _ = DeleteObject(b);
    }
}

fn round(dc: HDC, r: RECT, argb: u32, k: f32) {
    unsafe {
        let b = CreateSolidBrush(colorref(argb));
        let p = CreatePen(PS_SOLID, 1, colorref(argb));
        let (ob, op) = (SelectObject(dc, b), SelectObject(dc, p));
        let c = (6.0 * k).round() as i32;
        let _ = RoundRect(dc, r.left, r.top, r.right, r.bottom, c, c);
        SelectObject(dc, ob);
        SelectObject(dc, op);
        let _ = DeleteObject(b);
        let _ = DeleteObject(p);
    }
}

fn text_in(dc: HDC, s: &str, r: RECT, argb: u32, flags: DRAW_TEXT_FORMAT) {
    // (an empty slice's pointer is a placeholder, and DrawTextW reads it even for no characters: an access violation)
    if s.is_empty() {
        return;
    }
    let mut u: Vec<u16> = s.encode_utf16().collect();
    let mut r = r;
    unsafe {
        SetTextColor(dc, colorref(argb));
        let _ = DrawTextW(dc, &mut u, &mut r, flags | DT_SINGLELINE | DT_VCENTER | DT_NOPREFIX);
    }
}

/// Lays out the bar's items for a window `w` wide (the measuring with the bar's font selected into `dc`).
fn place_items(st: &State, dc: HDC, w: i32, k: f32) -> Vec<(Item, RECT)> {
    let (h, pad, gap) = ((28.0 * k) as i32, (12.0 * k) as i32, (6.0 * k) as i32);
    let top = ((BAR_H as f32 * k) as i32 - h) / 2;
    let mut out = Vec::new();
    let mut x = (10.0 * k) as i32;
    let widths = |item: Item| -> i32 {
        let base = text_w(dc, &st.label(item)) + 2 * pad;
        match item {
            Item::Prev | Item::Next => h,
            // (room for its box)
            Item::Colors | Item::Numbers | Item::Header => base + (18.0 * k) as i32,
            // (as wide as its longest text, so the bar doesn't move from page to page)
            Item::Count => base.max(text_w(dc, &format!("Page {0} of {0}", group(st.count().max(1) as u64))) + 2 * pad),
            _ => base,
        }
    };
    for item in [Item::Prev, Item::Count, Item::Next, Item::Width, Item::Colors, Item::Numbers, Item::Header, Item::Setup] {
        let iw = widths(item);
        if matches!(item, Item::Width | Item::Colors) {
            x += 2 * gap;
        }
        out.push((item, RECT { left: x, top, right: x + iw, bottom: top + h }));
        x += iw + gap;
    }
    let mut r = w - (10.0 * k) as i32;
    for item in [Item::Close, Item::Print] {
        let iw = widths(item);
        out.push((item, RECT { left: r - iw, top, right: r, bottom: top + h }));
        r -= iw + gap;
    }
    out
}

/// Where the page goes in a client area of `w` by `h` (the bar's height `bar`): whole, or its width filling the
/// window (then `scroll` pixels down it). Also how far it can scroll.
fn page_rect(st: &State, w: i32, h: i32, bar: i32, k: f32) -> (RECT, i32) {
    let aspect = st.pages.as_ref().map_or(0.77, |p| p.aspect());
    let m = (24.0 * k) as i32;
    let (aw, ah) = ((w - 2 * m).max(1), (h - bar - 2 * m).max(1));
    if st.width {
        let pw = aw.min((900.0 * k * 1.6) as i32);
        let ph = (pw as f32 / aspect) as i32;
        let max = (ph - ah).max(0);
        let y = bar + m - st.scroll.clamp(0, max);
        let x = (w - pw) / 2;
        (RECT { left: x, top: y, right: x + pw, bottom: y + ph }, max)
    } else {
        let (mut pw, mut ph) = (aw, (aw as f32 / aspect) as i32);
        if ph > ah {
            ph = ah;
            pw = (ah as f32 * aspect) as i32;
        }
        let (x, y) = ((w - pw) / 2, bar + m + (ah - ph) / 2);
        (RECT { left: x, top: y, right: x + pw, bottom: y + ph }, 0)
    }
}

/// Draws the whole client area (`w` by `h`) into `dc`.
fn paint(st: &mut State, dc: HDC, w: i32, h: i32, k: f32) {
    let t = st.theme.clone();
    let (hover, pressed) = (over(t.hover, t.surface), over(t.pressed, t.surface));
    let bar = (BAR_H as f32 * k) as i32;
    let (font, icons) = (ui_font(k), icon_font(k));
    unsafe {
        let old = SelectObject(dc, font);
        SetBkMode(dc, TRANSPARENT);
        // around the page: a shade darker than the window
        fill(dc, RECT { left: 0, top: 0, right: w, bottom: h }, if t.dark { 0xFF1A1A1A } else { 0xFFE4E4E4 });
        let (pr, max) = page_rect(st, w, h, bar, k);
        st.scroll = st.scroll.clamp(0, max);
        let page = st.page;
        match (&mut st.pages, &st.error) {
            (Some(pages), _) => {
                let shadow = (3.0 * k) as i32;
                fill(dc, RECT { left: pr.left + shadow, top: pr.top + shadow, right: pr.right + shadow, bottom: pr.bottom + shadow }, if t.dark { 0xFF0C0C0C } else { 0xFFC4C4C4 });
                fill(dc, pr, 0xFFFFFFFF);
                let m = pages.page(page);
                let _ = PlayEnhMetaFile(dc, m, &pr);
            }
            (None, Some(e)) => text_in(dc, e, RECT { left: 0, top: bar, right: w, bottom: h }, t.text, DT_CENTER),
            (None, None) => {
                let p = st.job.as_ref().map_or(0.0, |(j, ..)| j.fraction());
                let msg = format!("Laying out the pages… {:.0}%", p * 100.0);
                text_in(dc, &msg, RECT { left: 0, top: bar, right: w, bottom: h }, t.text_dim, DT_CENTER);
            }
        }
        // the bar
        fill(dc, RECT { left: 0, top: 0, right: w, bottom: bar }, t.surface);
        fill(dc, RECT { left: 0, top: bar - 1, right: w, bottom: bar }, t.border);
        st.items = place_items(st, dc, w, k);
        for &(item, r) in &st.items {
            let enabled = st.enabled(item);
            let on = st.on(item);
            let primary = item == Item::Print;
            if primary && enabled {
                round(dc, r, if st.pressed == Some(item) { over(t.pressed, t.accent) } else { t.accent }, k);
            } else if enabled && item != Item::Count && (st.hover == Some(item) || on == Some(true) && item == Item::Width) {
                round(dc, r, if st.pressed == Some(item) || on == Some(true) { pressed } else { hover }, k);
            }
            let color = if !enabled { t.text_faint } else if primary { t.on_accent } else { t.text };
            let label = st.label(item);
            match (item, on) {
                (Item::Colors | Item::Numbers | Item::Header, Some(on)) => {
                    // a check box, then the label
                    let s = (14.0 * k) as i32;
                    let bx = r.left + (8.0 * k) as i32;
                    let by = (r.top + r.bottom - s) / 2;
                    let b = RECT { left: bx, top: by, right: bx + s, bottom: by + s };
                    if on {
                        round(dc, b, t.accent, k * 0.6);
                        let pen = CreatePen(PS_SOLID, (1.6 * k).max(1.0) as i32, colorref(t.on_accent));
                        let op = SelectObject(dc, pen);
                        let p = |fx: f32, fy: f32| (bx + (fx * s as f32) as i32, by + (fy * s as f32) as i32);
                        let (a, m2, c) = (p(0.22, 0.52), p(0.42, 0.72), p(0.78, 0.30));
                        let _ = MoveToEx(dc, a.0, a.1, None);
                        let _ = LineTo(dc, m2.0, m2.1);
                        let _ = LineTo(dc, c.0, c.1);
                        SelectObject(dc, op);
                        let _ = DeleteObject(pen);
                    } else {
                        round(dc, b, t.text_dim, k * 0.6);
                        round(dc, RECT { left: b.left + 1, top: b.top + 1, right: b.right - 1, bottom: b.bottom - 1 }, t.surface, k * 0.5);
                    }
                    text_in(dc, &label, RECT { left: b.right + (6.0 * k) as i32, ..r }, color, DT_LEFT);
                }
                (Item::Prev | Item::Next, _) => {
                    SelectObject(dc, icons);
                    text_in(dc, &label, r, color, DT_CENTER);
                    SelectObject(dc, font);
                }
                _ => text_in(dc, &label, r, color, DT_CENTER),
            }
        }
        SelectObject(dc, old);
        let _ = DeleteObject(font);
        let _ = DeleteObject(icons);
    }
}

fn client(hwnd: HWND) -> (i32, i32) {
    let mut rc = RECT::default();
    unsafe {
        let _ = GetClientRect(hwnd, &mut rc);
    }
    (rc.right.max(1), rc.bottom.max(1))
}

fn invalidate(hwnd: HWND) {
    unsafe {
        let _ = InvalidateRect(hwnd, None, false);
    }
}

fn hit(st: &State, x: i32, y: i32) -> Option<Item> {
    st.items.iter().find(|(_, r)| x >= r.left && x < r.right && y >= r.top && y < r.bottom).map(|(i, _)| *i)
}

extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    let xy = || ((lp.0 & 0xFFFF) as i16 as i32, ((lp.0 >> 16) & 0xFFFF) as i16 as i32);
    match msg {
        WM_PAINT => {
            let mut ps = PAINTSTRUCT::default();
            unsafe {
                let dc = BeginPaint(hwnd, &mut ps);
                let (w, h) = client(hwnd);
                let mem = CreateCompatibleDC(dc);
                let bmp = CreateCompatibleBitmap(dc, w, h);
                let old = SelectObject(mem, bmp);
                let k = scale(hwnd);
                with(|st| paint(st, mem, w, h, k));
                let _ = BitBlt(dc, 0, 0, w, h, mem, 0, 0, SRCCOPY);
                SelectObject(mem, old);
                let _ = DeleteObject(bmp);
                let _ = DeleteDC(mem);
                let _ = EndPaint(hwnd, &ps);
            }
            LRESULT(0)
        }
        WM_ERASEBKGND => LRESULT(1),
        WM_TIMER if wp.0 == TIMER_LAYOUT => {
            with(|st| st.poll());
            invalidate(hwnd);
            LRESULT(0)
        }
        WM_TIMER if wp.0 == TIMER_SCRIPT => {
            script_step(hwnd);
            LRESULT(0)
        }
        WM_SIZE => {
            invalidate(hwnd);
            LRESULT(0)
        }
        WM_MOUSEMOVE => {
            let (x, y) = xy();
            let changed = with(|st| {
                let h = hit(st, x, y).filter(|i| st.enabled(*i));
                std::mem::replace(&mut st.hover, h) != h
            });
            if changed == Some(true) {
                let mut tme = TRACKMOUSEEVENT { cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32, dwFlags: TME_LEAVE, hwndTrack: hwnd, dwHoverTime: 0 };
                unsafe {
                    let _ = TrackMouseEvent(&mut tme);
                }
                invalidate(hwnd);
            }
            LRESULT(0)
        }
        WM_MOUSELEAVE => {
            with(|st| st.hover = None);
            invalidate(hwnd);
            LRESULT(0)
        }
        WM_LBUTTONDOWN => {
            let (x, y) = xy();
            with(|st| st.pressed = hit(st, x, y).filter(|i| st.enabled(*i)));
            unsafe {
                SetCapture(hwnd);
            }
            invalidate(hwnd);
            LRESULT(0)
        }
        WM_LBUTTONUP => {
            let (x, y) = xy();
            unsafe {
                let _ = ReleaseCapture();
            }
            // (the dialog Page setup shows mustn't run while the state is borrowed)
            let item = with(|st| {
                let p = st.pressed.take();
                p.filter(|p| hit(st, x, y) == Some(*p))
            })
            .flatten();
            if let Some(item) = item {
                act(item);
            }
            invalidate(hwnd);
            LRESULT(0)
        }
        WM_MOUSEWHEEL => {
            let delta = (wp.0 >> 16) as i16 as i32;
            with(|st| {
                if st.width {
                    st.scroll -= delta * (120.0 * scale(hwnd)) as i32 / 120;
                } else {
                    st.go(st.page as isize - delta.signum() as isize);
                }
            });
            invalidate(hwnd);
            LRESULT(0)
        }
        WM_KEYDOWN => {
            let ctrl = unsafe { GetKeyState(VK_CONTROL.0 as i32) } < 0;
            let key = VIRTUAL_KEY(wp.0 as u16);
            let item = match key {
                VK_ESCAPE => Some(Item::Close),
                VK_P if ctrl => Some(Item::Print),
                _ => None,
            };
            if let Some(i) = item {
                act(i);
                return LRESULT(0);
            }
            with(|st| {
                let step = (60.0 * scale(hwnd)) as i32;
                match key {
                    VK_LEFT | VK_PRIOR => st.go(st.page as isize - 1),
                    VK_RIGHT | VK_NEXT => st.go(st.page as isize + 1),
                    VK_HOME => st.go(0),
                    VK_END => st.go(isize::MAX),
                    VK_UP => st.scroll -= step,
                    VK_DOWN => st.scroll += step,
                    _ => {}
                }
            });
            invalidate(hwnd);
            LRESULT(0)
        }
        WM_GETMINMAXINFO => {
            let mmi = lp.0 as *mut MINMAXINFO;
            let k = scale(hwnd);
            unsafe {
                (*mmi).ptMinTrackSize = POINT { x: min_width(hwnd, k), y: (400.0 * k) as i32 };
            }
            LRESULT(0)
        }
        WM_DPICHANGED => {
            let r = unsafe { &*(lp.0 as *const RECT) };
            unsafe {
                let _ = SetWindowPos(hwnd, None, r.left, r.top, r.right - r.left, r.bottom - r.top, SWP_NOZORDER | SWP_NOACTIVATE);
            }
            invalidate(hwnd);
            LRESULT(0)
        }
        WM_CLOSE => {
            with(|st| st.done = true);
            LRESULT(0)
        }
        _ => unsafe { DefWindowProcW(hwnd, msg, wp, lp) },
    }
}

/// How wide the window must be for the bar's items to fit side by side (narrower, Page setup went under Print and
/// Close, and a click on Close opened it).
fn min_width(hwnd: HWND, k: f32) -> i32 {
    let bar = with(|st| unsafe {
        let dc = GetDC(hwnd);
        let font = ui_font(k);
        let old = SelectObject(dc, font);
        // (laid out for a window 0 wide: Print and Close then reach left of 0 by the room they take)
        let items = place_items(st, dc, 0, k);
        SelectObject(dc, old);
        let _ = DeleteObject(font);
        ReleaseDC(hwnd, dc);
        let right = |i: &Item| matches!(i, Item::Print | Item::Close);
        let left_end = items.iter().filter(|(i, _)| !right(i)).map(|(_, r)| r.right).max().unwrap_or(0);
        let right_room = items.iter().filter(|(i, _)| right(i)).map(|(_, r)| -r.left).max().unwrap_or(0);
        left_end + right_room + (12.0 * k) as i32
    });
    let (mut wr, mut cr) = (RECT::default(), RECT::default());
    unsafe {
        let _ = GetWindowRect(hwnd, &mut wr);
        let _ = GetClientRect(hwnd, &mut cr);
    }
    let frame = (wr.right - wr.left) - (cr.right - cr.left);
    (bar.unwrap_or(0) + frame).max((720.0 * k) as i32)
}

/// Does what `item` is for (outside the state's borrow: Page setup shows a dialog).
fn act(item: Item) {
    if item == Item::Setup {
        // (test mode never shows a dialog)
        if super::win::SCRIPTED.with(|s| s.borrow_mut().as_mut().map(|s| s.asked.push("Page setup".into())).is_some()) {
            note("Page setup: its dialog isn't shown in test mode".into());
            return;
        }
        let Some((hwnd, mut last, mut m)) = with(|st| (st.hwnd, std::mem::take(&mut st.last), st.style.margins)) else { return };
        let ok = last.page_setup(hwnd, &mut m);
        with(|st| {
            st.last = last;
            if ok {
                st.style.margins = m;
                st.changed = true;
                st.relayout();
            }
        });
    } else {
        with(|st| st.act(item));
    }
    if let Some(hwnd) = with(|st| st.hwnd) {
        invalidate(hwnd);
    }
}

/// The preview of `source` in `style`, for the printer `last` says, over the window `owner` (modal). Returns how it
/// ended, the printer choice (Page setup can change it), and the style if its options or margins were changed.
pub fn run(owner: HWND, title: &str, source: Source, style: Style, last: Last, theme: Theme, notify: Notify) -> (Outcome, Last, Option<Style>) {
    let class = w!("SlatePreview");
    unsafe {
        let hinst: windows::Win32::Foundation::HINSTANCE = windows::Win32::System::LibraryLoader::GetModuleHandleW(None).unwrap_or_default().into();
        let wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(wndproc),
            hInstance: hinst,
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
            lpszClassName: class,
            ..Default::default()
        };
        RegisterClassExW(&wc);
        // most of the screen the main window is on, a page's shape
        let mon = MonitorFromWindow(owner, MONITOR_DEFAULTTONEAREST);
        let mut mi = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
        let _ = GetMonitorInfoW(mon, &mut mi);
        let wa = mi.rcWork;
        let h = ((wa.bottom - wa.top) as f32 * 0.9) as i32;
        let w = ((h as f32 * 0.85) as i32).max(((wa.right - wa.left) as f32 * 0.5) as i32).min(wa.right - wa.left);
        let (x, y) = (wa.left + (wa.right - wa.left - w) / 2, wa.top + (wa.bottom - wa.top - h) / 2);
        let name = HSTRING::from(format!("Print preview — {title}"));
        let Ok(hwnd) = CreateWindowExW(
            WINDOW_EX_STYLE(0),
            class,
            PCWSTR(name.as_ptr()),
            WS_OVERLAPPEDWINDOW,
            x,
            y,
            w,
            h,
            owner,
            None,
            hinst,
            None,
        ) else {
            return (Outcome::Closed, last, None);
        };
        if theme.hc {
            super::win::system_title_bar(hwnd);
        } else {
            super::win::style_title_bar(hwnd, theme.dark, theme.surface, theme.text);
        }
        STATE.with(|s| {
            *s.borrow_mut() = Some(State {
                hwnd,
                owner,
                last,
                style,
                source,
                notify,
                pages: None,
                job: None,
                error: None,
                page: 0,
                width: false,
                scroll: 0,
                items: Vec::new(),
                hover: None,
                pressed: None,
                theme,
                outcome: Outcome::Closed,
                done: false,
                changed: false,
            })
        });
        with(|st| st.relayout());
        // (wide enough for the bar, on screens whose scaling makes it wider than the window's usual share)
        let need = min_width(hwnd, scale(hwnd)).min(wa.right - wa.left);
        if need > w {
            let x = wa.left + (wa.right - wa.left - need) / 2;
            let _ = SetWindowPos(hwnd, None, x, y, need, h, SWP_NOZORDER | SWP_NOACTIVATE);
        }
        let _ = EnableWindow(owner, false);
        let scripted = SCRIPT.with(|s| s.borrow().is_some());
        if scripted {
            SetTimer(hwnd, TIMER_SCRIPT, 10, None);
        } else {
            let _ = ShowWindow(hwnd, SW_SHOW);
        }
        let mut msg = MSG::default();
        loop {
            if with(|st| st.done).unwrap_or(true) {
                break;
            }
            if !GetMessageW(&mut msg, None, 0, 0).as_bool() {
                // (Windows is ending Slate: passed on to the main loop)
                PostQuitMessage(msg.wParam.0 as i32);
                break;
            }
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
        // (the main window gets the keyboard back before this one goes, so it doesn't drop behind other windows)
        let _ = EnableWindow(owner, true);
        if !scripted {
            let _ = SetForegroundWindow(owner);
        }
        let _ = DestroyWindow(hwnd);
        let st = STATE.with(|s| s.borrow_mut().take());
        match st {
            Some(mut st) => {
                st.drop_job();
                let changed = st.changed.then(|| st.style.clone());
                (st.outcome, st.last, changed)
            }
            None => (Outcome::Closed, Last::default(), None),
        }
    }
}

/// Test mode: the preview of `source` laid out for `printer` (Microsoft Print to PDF; None: as the window shows
/// without one), page `page` (from 1) drawn into a `w` by `h` picture (BGRA, top row first), with Page width on or
/// off. Also the page count.
pub fn picture(printer: Option<Printer>, source: &Source, style: Style, theme: Theme, page: usize, width: bool, w: i32, h: i32) -> Result<(Vec<u8>, usize), String> {
    let pages = match printer {
        Some(printer) => {
            let geo = Geometry::new(printer.0, &style);
            let state = print::state_at(&source.snap, source.lang, source.line_start, source.from);
            let text = print::Text { snap: &source.snap, from: source.from, to: source.to, line: source.line, lang: source.lang, state };
            let rows = match print::layout(&text, geo.shape(&style), &mut |_| true) {
                Ok(Some(rows)) => rows,
                Ok(None) => return Err("stopped".into()),
                Err(TooMany) => return Err(super::print::too_many()),
            };
            Some(Pages::new(printer, geo, style.clone(), rows))
        }
        None => None,
    };
    let count = pages.as_ref().map_or(0, |p| p.count());
    let error = pages.is_none().then(|| NO_PRINTER.to_string());
    let mut st = State {
        hwnd: HWND::default(),
        owner: HWND::default(),
        last: Last::default(),
        style,
        source: Source { snap: source.snap.clone(), ..*source },
        notify: std::sync::Arc::new(|| {}),
        pages,
        job: None,
        error,
        page: page.saturating_sub(1).min(count.saturating_sub(1)),
        width,
        scroll: 0,
        items: Vec::new(),
        hover: None,
        pressed: None,
        theme,
        outcome: Outcome::Closed,
        done: false,
        changed: false,
    };
    Ok((frame(&mut st, w, h, 1.0)?, count))
}

/// The window's client area as `st` draws it, `w` by `h` at scale `k`, into a picture (BGRA, top row first).
fn frame(st: &mut State, w: i32, h: i32, k: f32) -> Result<Vec<u8>, String> {
    unsafe {
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
        paint(st, mem, w, h, k);
        let _ = GdiFlush();
        let out = std::slice::from_raw_parts(bits as *const u8, (w * h * 4) as usize).to_vec();
        SelectObject(mem, old);
        let _ = DeleteObject(dib);
        let _ = DeleteDC(mem);
        ReleaseDC(None, screen);
        Ok(out)
    }
}

/// Test mode: the script's next step (`Script`), once the layout is done.
fn script_step(hwnd: HWND) {
    let (w, h) = client(hwnd);
    let k = scale(hwnd);
    // (a frame first, as the screen shows one before anything's clicked: it places the bar's items)
    let Some(Some(pic)) = with(|st| if st.job.is_some() { None } else { Some(frame(st, w, h, k)) }) else { return };
    let Some(step) = SCRIPT.with(|s| s.borrow_mut().as_mut().and_then(|s| s.steps.pop_front())) else {
        note("preview still open at the script's end: closed".into());
        with(|st| st.done = true);
        return;
    };
    let (op, arg) = step.split_once(':').unwrap_or((&step, ""));
    let send = |msg: u32, wp: usize, lp: isize| unsafe { SendMessageW(hwnd, msg, WPARAM(wp), LPARAM(lp)) };
    match op {
        "click" => {
            let at = with(|st| st.items.iter().find(|(i, _)| format!("{i:?}").eq_ignore_ascii_case(arg)).map(|(_, r)| *r)).flatten();
            match at {
                Some(r) => {
                    let lp = ((((r.top + r.bottom) / 2) << 16) | (((r.left + r.right) / 2) & 0xFFFF)) as isize;
                    send(WM_MOUSEMOVE, 0, lp);
                    send(WM_LBUTTONDOWN, 1, lp);
                    send(WM_LBUTTONUP, 0, lp);
                }
                None => note(format!("click: no item {arg}")),
            }
        }
        "key" => {
            let vk = match arg {
                "left" => VK_LEFT,
                "right" => VK_RIGHT,
                "home" => VK_HOME,
                "end" => VK_END,
                "up" => VK_UP,
                "down" => VK_DOWN,
                "pgdn" => VK_NEXT,
                "pgup" => VK_PRIOR,
                "esc" => VK_ESCAPE,
                _ => {
                    note(format!("key: no key {arg}"));
                    return;
                }
            };
            send(WM_KEYDOWN, vk.0 as usize, 0);
        }
        "wheel" => {
            let d: i16 = arg.parse().unwrap_or(-120);
            send(WM_MOUSEWHEEL, (d as u16 as usize) << 16, 0);
        }
        "size" => {
            let (sw, sh) = arg.split_once('x').map_or((800, 600), |(a, b)| (a.parse().unwrap_or(800), b.parse().unwrap_or(600)));
            // (as far as dragging an edge goes: SetWindowPos itself doesn't keep to WM_GETMINMAXINFO's minimum)
            let (sw, sh) = (sw.max(min_width(hwnd, k)), sh.max((400.0 * k) as i32));
            unsafe {
                let _ = SetWindowPos(hwnd, None, 0, 0, sw, sh, SWP_NOMOVE | SWP_NOZORDER | SWP_NOACTIVATE);
            }
        }
        "shot" => {
            let f = SCRIPT.with(|s| s.borrow().as_ref().map(|s| s.shot));
            let done = match (&pic, f) {
                (Ok(pic), Some(f)) => f(std::path::Path::new(arg), w, h, pic),
                (Err(e), _) => Err(e.clone()),
                (_, None) => Ok(()),
            };
            if let Err(e) = done {
                note(format!("shot {arg}: {e}"));
            }
        }
        "state" => {
            let s = with(|st| {
                let items: Vec<String> = st.items.iter().filter(|(i, _)| !st.enabled(*i)).map(|(i, _)| format!("{i:?}")).collect();
                format!(
                    "{} | width {} colors {} numbers {} header {} | off: {} | error: {}",
                    st.label(Item::Count),
                    st.width,
                    st.style.colors,
                    st.style.numbers,
                    st.style.header,
                    if items.is_empty() { "none".into() } else { items.join(" ") },
                    st.error.as_deref().unwrap_or("none")
                )
            });
            note(format!("state: {}", s.unwrap_or_default()));
        }
        _ => note(format!("unknown step {step}")),
    }
}
