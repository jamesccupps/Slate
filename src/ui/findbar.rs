//! The find / replace / go-to-line bar docked above the text. Typing happens in native EDIT controls (so IME,
//! clipboard and undo just work); everything around them is drawn with Direct2D.

use std::sync::Arc;

use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::Graphics::Gdi::{
    CLEARTYPE_QUALITY, CLIP_DEFAULT_PRECIS, CreateFontW, CreateSolidBrush, DEFAULT_CHARSET, DeleteObject, FF_DONTCARE,
    FW_NORMAL, HBRUSH, HFONT, OUT_DEFAULT_PRECIS,
};
use windows::Win32::UI::Input::KeyboardAndMouse::SetFocus;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, ES_AUTOHSCROLL, GetWindowTextLengthW, GetWindowTextW, HMENU, SW_HIDE, SW_SHOW, SWP_NOZORDER,
    SendMessageW, SetWindowPos, SetWindowTextW, ShowWindow, WINDOW_EX_STYLE, WINDOW_STYLE, WM_SETFONT, WS_CHILD,
    WS_TABSTOP,
};
use windows::core::{HSTRING, w};

use crate::core::search::{Matcher, Query};

use super::gfx::{Align, Gfx, Rect};
use super::theme::{Theme, metrics};
use super::win::colorref;

pub const ID_FIND: usize = 1001;
pub const ID_REPLACE: usize = 1002;
pub const ID_GOTO: usize = 1003;
const EM_SETCUEBANNER: u32 = 0x1501;
const EM_SETSEL: u32 = 0x00B1;
const EM_SETMARGINS: u32 = 0x00D3;
const EC_LEFTMARGIN: usize = 1;
const EC_RIGHTMARGIN: usize = 2;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mode {
    Find,
    Replace,
    GoTo,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Part {
    Expand,
    Case,
    Word,
    Regex,
    Prev,
    Next,
    Close,
    ReplaceOne,
    ReplaceAll,
    Go,
}

pub struct FindBar {
    pub open: bool,
    pub mode: Mode,
    pub find_edit: HWND,
    pub replace_edit: HWND,
    pub goto_edit: HWND,
    pub query: Query,
    pub replace_text: String,
    pub matcher: Option<Arc<Matcher>>,
    pub error: Option<String>,
    /// Where the caret was when the search text last started changing (live search starts here).
    pub origin: Option<u64>,
    pub status: String,
    pub status_bad: bool,
    pub goto_hint: String,
    pub parts: Vec<(Part, Rect)>,
    pub inputs: Vec<(HWND, Rect)>,
    /// Room for the match count ("3 of 120") or the go-to hint (it shrinks first in a narrow window).
    status_w: f32,
    font: HFONT,
    pub brush: HBRUSH,
    font_dpi: u32,
}

fn make_edit(parent: HWND, id: usize, cue: &str) -> HWND {
    unsafe {
        let h = CreateWindowExW(
            WINDOW_EX_STYLE(0),
            w!("EDIT"),
            w!(""),
            WINDOW_STYLE(WS_CHILD.0 | WS_TABSTOP.0 | ES_AUTOHSCROLL as u32),
            0,
            0,
            10,
            10,
            parent,
            HMENU(id as *mut _),
            None,
            None,
        )
        .expect("edit control");
        let cue = HSTRING::from(cue);
        SendMessageW(h, EM_SETCUEBANNER, WPARAM(1), LPARAM(cue.as_ptr() as isize));
        h
    }
}

impl FindBar {
    pub fn new(parent: HWND) -> FindBar {
        FindBar {
            open: false,
            mode: Mode::Find,
            find_edit: make_edit(parent, ID_FIND, "Find"),
            replace_edit: make_edit(parent, ID_REPLACE, "Replace with"),
            goto_edit: make_edit(parent, ID_GOTO, "Line number, or line:column"),
            query: Query::default(),
            replace_text: String::new(),
            matcher: None,
            error: None,
            origin: None,
            status: String::new(),
            status_bad: false,
            goto_hint: String::new(),
            parts: Vec::new(),
            inputs: Vec::new(),
            status_w: 0.0,
            font: HFONT::default(),
            brush: HBRUSH::default(),
            font_dpi: 0,
        }
    }

    pub fn height(&self) -> f32 {
        if !self.open {
            0.0
        } else if self.mode == Mode::Replace {
            metrics::FIND_ROW_H * 2.0 - 6.0
        } else {
            metrics::FIND_ROW_H
        }
    }

    pub fn is_edit(&self, h: HWND) -> bool {
        h == self.find_edit || h == self.replace_edit || h == self.goto_edit
    }

    pub fn text_of(h: HWND) -> String {
        unsafe {
            let n = GetWindowTextLengthW(h);
            let mut buf = vec![0u16; n as usize + 1];
            let got = GetWindowTextW(h, &mut buf);
            String::from_utf16_lossy(&buf[..got.max(0) as usize])
        }
    }

    pub fn set_text(h: HWND, s: &str) {
        unsafe {
            let _ = SetWindowTextW(h, &HSTRING::from(s));
        }
    }

    pub fn select_all(h: HWND) {
        unsafe {
            SendMessageW(h, EM_SETSEL, WPARAM(0), LPARAM(-1));
        }
    }

    pub fn focus(h: HWND) {
        unsafe {
            let _ = SetFocus(h);
        }
    }

    /// Rebuilds the matcher from the current query.
    pub fn compile(&mut self) {
        self.matcher = None;
        self.error = None;
        if self.query.text.is_empty() {
            return;
        }
        match Matcher::new(&self.query) {
            Ok(m) => self.matcher = Some(Arc::new(m)),
            Err(e) => self.error = Some(e),
        }
    }

    /// Fonts and colors for the edit boxes (DPI and theme dependent).
    pub fn style_edits(&mut self, dpi: u32, theme: &Theme) {
        unsafe {
            if self.font_dpi != dpi {
                if !self.font.is_invalid() {
                    let _ = DeleteObject(self.font);
                }
                let px = -((13.0 * dpi as f32 / 96.0).round() as i32);
                self.font = CreateFontW(
                    px,
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
                    FF_DONTCARE.0 as u32,
                    w!("Segoe UI"),
                );
                self.font_dpi = dpi;
                let m = (4.0 * dpi as f32 / 96.0) as isize;
                for h in [self.find_edit, self.replace_edit, self.goto_edit] {
                    SendMessageW(h, WM_SETFONT, WPARAM(self.font.0 as usize), LPARAM(1));
                    SendMessageW(h, EM_SETMARGINS, WPARAM(EC_LEFTMARGIN | EC_RIGHTMARGIN), LPARAM(m | (m << 16)));
                }
            }
            if !self.brush.is_invalid() {
                let _ = DeleteObject(self.brush);
            }
            self.brush = CreateSolidBrush(windows::Win32::Foundation::COLORREF(colorref(theme.input_bg)));
            // The boxes ask for their colors only when they paint: repaint them in the new ones now.
            for h in [self.find_edit, self.replace_edit, self.goto_edit] {
                let _ = windows::Win32::Graphics::Gdi::InvalidateRect(h, None, true);
            }
        }
    }

    /// Positions the bar's pieces inside `r` (DIPs) and the edit controls (pixels). Hides edits when closed.
    pub fn layout(&mut self, r: Rect, gfx: &Gfx, ui: &windows::Win32::Graphics::DirectWrite::IDWriteTextFormat) {
        self.parts.clear();
        self.inputs.clear();
        let show = |h: HWND, on: bool| unsafe {
            let _ = ShowWindow(h, if on { SW_SHOW } else { SW_HIDE });
        };
        if !self.open {
            for h in [self.find_edit, self.replace_edit, self.goto_edit] {
                show(h, false);
            }
            return;
        }
        let row_h = metrics::FIND_ROW_H;
        let pad = 8.0;
        let btn = 28.0;
        let box_h = 28.0;
        let y = r.y + (row_h - box_h) / 2.0;
        let mut x = r.x + pad;
        let dpi = gfx.dpi;
        let place = |h: HWND, b: Rect| unsafe {
            let k = dpi / 96.0;
            let eh = (20.0 * k).round();
            let left = (b.x * k).round() as i32;
            let top = ((b.y * k) + (b.h * k - eh) / 2.0).round() as i32;
            let _ = SetWindowPos(
                h,
                None,
                left + 1,
                top,
                (b.w * k).round() as i32 - 2,
                eh as i32,
                SWP_NOZORDER,
            );
        };
        // The close button keeps the right edge; in a narrow window the hint or match count gets less room first,
        // then the box, so nothing overlaps.
        let close = Rect::new(r.right() - pad - btn, y, btn, box_h);
        if self.mode == Mode::GoTo {
            let (lw, _) = gfx.measure("Go to line", ui);
            x += lw + 12.0;
            let box_w = 260f32.min(close.x - 8.0 - 52.0 - 8.0 - x).max(80.0);
            let b = Rect::new(x, y, box_w, box_h);
            self.inputs.push((self.goto_edit, b));
            place(self.goto_edit, Rect::new(b.x + 4.0, b.y, b.w - 8.0, b.h));
            show(self.goto_edit, true);
            show(self.find_edit, false);
            show(self.replace_edit, false);
            x = b.right() + 8.0;
            self.parts.push((Part::Go, Rect::new(x, y, 52.0, box_h)));
            self.status_w = (close.x - 8.0 - (x + 60.0)).clamp(0.0, 300.0);
            self.parts.push((Part::Close, close));
            return;
        }
        self.parts.push((Part::Expand, Rect::new(x, y, 22.0, box_h)));
        x += 26.0;
        // what the box and the match count after it can have, before the previous / next buttons
        let room = (close.x - 8.0 - (2.0 * btn + 2.0) - 14.0 - x).max(0.0);
        self.status_w = 124f32.min(room - 140.0).max(0.0);
        let box_w = (r.w * 0.42).clamp(220.0, 460.0).min(room - self.status_w).max(120.0);
        let b = Rect::new(x, y, box_w, box_h);
        self.inputs.push((self.find_edit, b));
        // Toggles sit inside the box on the right.
        let tw = 26.0;
        let tx = b.right() - 3.0 * tw - 2.0;
        self.parts.push((Part::Case, Rect::new(tx, y + 2.0, tw, box_h - 4.0)));
        self.parts.push((Part::Word, Rect::new(tx + tw, y + 2.0, tw, box_h - 4.0)));
        self.parts.push((Part::Regex, Rect::new(tx + 2.0 * tw, y + 2.0, tw, box_h - 4.0)));
        place(self.find_edit, Rect::new(b.x + 4.0, b.y, tx - b.x - 6.0, b.h));
        show(self.find_edit, true);
        show(self.goto_edit, false);
        x = b.right() + 10.0 + self.status_w + 4.0;
        self.parts.push((Part::Prev, Rect::new(x, y, btn, box_h)));
        self.parts.push((Part::Next, Rect::new(x + btn + 2.0, y, btn, box_h)));
        self.parts.push((Part::Close, close));
        if self.mode == Mode::Replace {
            let y2 = y + row_h - 6.0;
            let b2 = Rect::new(b.x, y2, box_w, box_h);
            self.inputs.push((self.replace_edit, b2));
            place(self.replace_edit, Rect::new(b2.x + 4.0, b2.y, b2.w - 8.0, b2.h));
            show(self.replace_edit, true);
            let (w1, _) = gfx.measure("Replace", ui);
            let (w2, _) = gfx.measure("Replace all", ui);
            self.parts.push((Part::ReplaceOne, Rect::new(b2.right() + 8.0, y2, w1 + 20.0, box_h)));
            self.parts.push((Part::ReplaceAll, Rect::new(b2.right() + 8.0 + w1 + 24.0, y2, w2 + 20.0, box_h)));
        } else {
            show(self.replace_edit, false);
        }
    }

    pub fn hit(&self, x: f32, y: f32) -> Option<Part> {
        self.parts.iter().find(|(_, r)| r.contains(x, y)).map(|(p, _)| *p)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn paint(
        &self,
        g: &Gfx,
        t: &Theme,
        r: Rect,
        ui: &windows::Win32::Graphics::DirectWrite::IDWriteTextFormat,
        icons: &windows::Win32::Graphics::DirectWrite::IDWriteTextFormat,
        hover: Option<Part>,
        focused_edit: Option<HWND>,
    ) {
        if !self.open {
            return;
        }
        g.fill(r, t.surface);
        g.line(r.x, r.bottom() - 0.5, r.right(), r.bottom() - 0.5, t.border, 1.0);
        for (h, b) in &self.inputs {
            g.fill_round(*b, 4.0, t.input_bg);
            let focused = focused_edit == Some(*h);
            g.stroke_round(*b, 4.0, if focused { t.accent } else { t.border }, if focused { 1.5 } else { 1.0 });
        }
        if self.mode == Mode::GoTo {
            let (lw, _) = g.measure("Go to line", ui);
            g.text("Go to line", ui, Rect::new(r.x + 8.0, r.y, lw + 4.0, metrics::FIND_ROW_H), t.text, Align::Left);
            if let Some((_, b)) = self.inputs.first() {
                let hint_x = b.right() + 68.0;
                g.text(&self.goto_hint, ui, Rect::new(hint_x, r.y, self.status_w, metrics::FIND_ROW_H), t.text_faint, Align::Left);
            }
        }
        for (p, b) in &self.parts {
            let on = match p {
                Part::Case => self.query.match_case,
                Part::Word => self.query.whole_word,
                Part::Regex => self.query.regex,
                _ => false,
            };
            if on {
                g.fill_round(*b, 4.0, t.pressed);
                g.stroke_round(*b, 4.0, t.accent, 1.0);
            } else if hover == Some(*p) {
                g.fill_round(*b, 4.0, t.hover);
            }
            let color = if on { t.text } else { t.text_dim };
            match p {
                Part::Expand => {
                    let glyph = if self.mode == Mode::Replace { "\u{E70D}" } else { "\u{E76C}" };
                    g.text(glyph, icons, *b, t.text_dim, Align::Center);
                }
                Part::Case => g.text("Aa", ui, *b, color, Align::Center),
                Part::Word => {
                    g.text("ab", ui, *b, color, Align::Center);
                    let cx = b.x + b.w / 2.0;
                    g.line(cx - 7.0, b.bottom() - 6.0, cx + 7.0, b.bottom() - 6.0, color, 1.0);
                }
                Part::Regex => g.text(".*", ui, *b, color, Align::Center),
                Part::Prev => g.text("\u{E70E}", icons, *b, t.text_dim, Align::Center),
                Part::Next => g.text("\u{E70D}", icons, *b, t.text_dim, Align::Center),
                Part::Close => g.text("\u{E711}", icons, *b, t.text_dim, Align::Center),
                Part::ReplaceOne => {
                    g.stroke_round(*b, 4.0, t.border, 1.0);
                    g.text("Replace", ui, *b, t.text, Align::Center);
                }
                Part::ReplaceAll => {
                    g.stroke_round(*b, 4.0, t.border, 1.0);
                    g.text("Replace all", ui, *b, t.text, Align::Center);
                }
                Part::Go => {
                    g.fill_round(*b, 4.0, t.accent);
                    g.text("Go", ui, *b, super::gfx::rgb(0xFFFFFF), Align::Center);
                }
            }
        }
        if self.mode != Mode::GoTo {
            if let Some((_, b)) = self.inputs.first() {
                let msg = self.error.as_deref().unwrap_or(&self.status);
                let bad = self.error.is_some() || self.status_bad;
                g.text(
                    msg,
                    ui,
                    Rect::new(b.right() + 10.0, b.y, self.status_w, b.h),
                    if bad { t.error } else { t.text_dim },
                    Align::Left,
                );
            }
        }
    }
}
