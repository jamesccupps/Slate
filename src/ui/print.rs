//! File → Print on Windows: the system's print dialog (PrintDlgEx), the print job started on the UI thread (a
//! printer's own dialogs, like Print to PDF's Save as, come up there), then the pages laid out (`crate::print`) and
//! drawn with GDI on a job's thread, in the printer's own pixels.

use std::path::Path;

use windows::Win32::Foundation::{COLORREF, HGLOBAL, HWND, SIZE};
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::Storage::Xps::{AbortDoc, DOCINFOW, EndDoc, EndPage, StartDocW, StartPage};
use windows::Win32::UI::Controls::Dialogs::*;
use windows::core::{HSTRING, PCWSTR, w};

use crate::edit::{group, plural};
use crate::print::{self, Row, Shape, Text, TooMany};
use crate::theme::{Theme, tok_color};

/// A printer's device context, made on the UI thread and handed to the thread that draws the pages.
pub struct Printer(pub HDC);

// (one thread at a time: the UI thread starts the document, then only the job's thread uses it)
unsafe impl Send for Printer {}

impl Printer {
    /// The "Microsoft Print to PDF" printer, to print into a file without any dialog (tests).
    pub fn pdf() -> Option<Printer> {
        let dc = unsafe { CreateDCW(w!("WINSPOOL"), w!("Microsoft Print to PDF"), PCWSTR::null(), None) };
        (!dc.is_invalid()).then_some(Printer(dc))
    }

    /// Starts the document (on the UI thread); `output`: a file to print into. False when it didn't start (Print to
    /// PDF's Save as cancelled, a printer that's gone), and the printer is let go.
    pub fn start(self, title: &str, output: Option<&Path>) -> Option<Printer> {
        let name = HSTRING::from(title);
        let out = output.map(|p| HSTRING::from(p.as_os_str()));
        let di = DOCINFOW {
            cbSize: std::mem::size_of::<DOCINFOW>() as i32,
            lpszDocName: PCWSTR(name.as_ptr()),
            lpszOutput: out.as_ref().map_or(PCWSTR::null(), |o| PCWSTR(o.as_ptr())),
            lpszDatatype: PCWSTR::null(),
            fwType: 0,
        };
        if unsafe { StartDocW(self.0, &di) } > 0 {
            Some(self)
        } else {
            unsafe {
                let _ = DeleteDC(self.0);
            }
            None
        }
    }

    /// Lets a printer go that's never started a document.
    pub fn discard(self) {
        unsafe {
            let _ = DeleteDC(self.0);
        }
    }
}

/// The printer and its settings chosen last, for the next Print (as long as Slate runs).
#[derive(Default)]
pub struct Last {
    mode: HGLOBAL,
    names: HGLOBAL,
}

/// What the print dialog said: the printer, the selection or all, which pages (None: all of them).
pub struct Choice {
    pub printer: Printer,
    pub selection: bool,
    pub pages: Option<Vec<(u32, u32)>>,
}

/// The system's print dialog. None when it was cancelled (or couldn't show).
pub fn dialog(hwnd: HWND, has_selection: bool, last: &mut Last) -> Option<Choice> {
    let mut ranges = vec![PRINTPAGERANGE { nFromPage: 0, nToPage: 0 }; 8];
    let mut flags = PD_RETURNDC | PD_USEDEVMODECOPIESANDCOLLATE | PD_NOCURRENTPAGE;
    if !has_selection {
        flags |= PD_NOSELECTION;
    }
    let mut pd = PRINTDLGEXW {
        lStructSize: std::mem::size_of::<PRINTDLGEXW>() as u32,
        hwndOwner: hwnd,
        hDevMode: last.mode,
        hDevNames: last.names,
        Flags: flags,
        nMaxPageRanges: ranges.len() as u32,
        lpPageRanges: ranges.as_mut_ptr(),
        nMinPage: 1,
        nMaxPage: print::PAGES_MAX as u32,
        nCopies: 1,
        nStartPage: START_PAGE_GENERAL,
        ..Default::default()
    };
    let ok = unsafe { PrintDlgExW(&mut pd) }.is_ok();
    // (kept for next time, printed or not)
    last.mode = pd.hDevMode;
    last.names = pd.hDevNames;
    if !ok || pd.dwResultAction != PD_RESULT_PRINT || pd.hDC.is_invalid() {
        if !pd.hDC.is_invalid() {
            Printer(pd.hDC).discard();
        }
        return None;
    }
    let pages = (pd.Flags & PD_PAGENUMS).0 != 0;
    let pages = pages.then(|| ranges[..pd.nPageRanges as usize].iter().map(|r| (r.nFromPage, r.nToPage)).collect());
    Some(Choice { printer: Printer(pd.hDC), selection: (pd.Flags & PD_SELECTION).0 != 0, pages })
}

/// How the text looks on paper.
pub struct Style {
    pub font: String,
    /// Points.
    pub size: f32,
    pub numbers: bool,
    pub tab: usize,
    /// At the top of each page: the file's name.
    pub title: String,
    /// The number of the last line printed (for the width of the line numbers).
    pub last_line: u64,
    pub theme: Theme,
}

/// Ends the document when dropped: abandons it unless `done` (a cancel, an error, a panic) and lets the printer go.
struct Job {
    dc: HDC,
    done: bool,
    fonts: Vec<HFONT>,
}

impl Drop for Job {
    fn drop(&mut self) {
        unsafe {
            if !self.done {
                AbortDoc(self.dc);
            }
            for f in self.fonts.drain(..) {
                let _ = DeleteObject(f);
            }
            let _ = DeleteDC(self.dc);
        }
    }
}

fn colorref(argb: u32) -> COLORREF {
    COLORREF(((argb >> 16) & 0xFF) | (argb & 0xFF00) | ((argb & 0xFF) << 16))
}

/// Lays out `text` for the printer's paper and prints the pages wanted (all of them, or those of `pages`), on a
/// started printer (`Printer::start`). `going` is told how many bytes are laid out and says whether to go on.
/// Returns how many pages were printed, None when stopped.
pub fn print(p: Printer, text: &Text, style: &Style, pages: Option<&[(u32, u32)]>, going: &mut dyn FnMut(u64) -> bool) -> Result<Option<u64>, String> {
    let dc = p.0;
    let mut job = Job { dc, done: false, fonts: Vec::new() };
    let cap = |i| unsafe { GetDeviceCaps(dc, i) };
    let (dpi_x, dpi_y) = (cap(LOGPIXELSX).max(72), cap(LOGPIXELSY).max(72));
    // the printable area's pixels; the paper's edges are `PHYSICALOFFSET` before it
    let (w, h) = (cap(HORZRES), cap(VERTRES));
    let (paper_w, paper_h) = (cap(PHYSICALWIDTH).max(w), cap(PHYSICALHEIGHT).max(h));
    let (off_x, off_y) = (cap(PHYSICALOFFSETX), cap(PHYSICALOFFSETY));
    // three quarters of an inch from the paper's edges, or the printer's own margin if it's wider
    let (mx, my) = (dpi_x * 3 / 4, dpi_y * 3 / 4);
    let (left, right) = ((mx - off_x).max(0), (paper_w - off_x - mx).min(w));
    let (top, bottom) = ((my - off_y).max(0), (paper_h - off_y - my).min(h));
    let face = HSTRING::from(style.font.as_str());
    let px = |pt: f32| -((pt * dpi_y as f32 / 72.0).round() as i32).max(1);
    let mut font = |height: i32, bold: bool, italic: bool| {
        let weight = if bold { FW_BOLD } else { FW_NORMAL };
        let f = unsafe {
            CreateFontW(
                height,
                0,
                0,
                0,
                weight.0 as i32,
                italic as u32,
                0,
                0,
                DEFAULT_CHARSET.0 as u32,
                OUT_DEFAULT_PRECIS.0 as u32,
                CLIP_DEFAULT_PRECIS.0 as u32,
                DEFAULT_QUALITY.0 as u32,
                (FIXED_PITCH.0 | FF_MODERN.0) as u32,
                &face,
            )
        };
        job.fonts.push(f);
        f
    };
    let size = style.size.clamp(4.0, 72.0);
    let fonts = [font(px(size), false, false), font(px(size), true, false), font(px(size), false, true), font(px(size), true, true)];
    let small = font(px(size * 0.85), false, false);
    let measure = |f: HFONT| unsafe {
        SelectObject(dc, f);
        let mut tm = TEXTMETRICW::default();
        let _ = GetTextMetricsW(dc, &mut tm);
        let mut s = SIZE::default();
        let _ = GetTextExtentPoint32W(dc, &[b'0' as u16], &mut s);
        ((tm.tmHeight + tm.tmExternalLeading).max(1), s.cx.max(1))
    };
    let (line_h, char_w) = measure(fonts[0]);
    let (small_h, _) = measure(small);
    // the file's name, a blank line, the text, a blank line, the page's number
    let body_top = top + 2 * small_h;
    let body_bottom = bottom - 2 * small_h;
    let rows = ((body_bottom - body_top) / line_h).max(1) as usize;
    let digits = if style.numbers { style.last_line.max(1).to_string().len() } else { 0 };
    let gutter = if style.numbers { digits + 2 } else { 0 };
    let cols = (((right - left) / char_w) as usize).saturating_sub(gutter).max(1);
    let all = match print::layout(text, Shape { cols, rows, tab: style.tab }, going) {
        Ok(Some(all)) => all,
        Ok(None) => return Ok(None),
        Err(TooMany) => return Err(format!("That's more than {} pages: select the part to print.", group(print::PAGES_MAX))),
    };
    let total = all.len().div_ceil(rows) as u64;
    let wanted = |n: u64| pages.is_none_or(|r| r.iter().any(|&(a, b)| (a as u64..=b as u64).contains(&n)));
    let theme = &style.theme;
    let mut printed = 0;
    unsafe {
        SetBkMode(dc, TRANSPARENT);
        for (k, page) in all.chunks(rows).enumerate() {
            let n = k as u64 + 1;
            if !wanted(n) {
                continue;
            }
            if !going(text.to - text.from) {
                return Ok(None);
            }
            if StartPage(dc) <= 0 {
                return Err("The printer didn't take the page.".into());
            }
            SelectObject(dc, small);
            SetTextColor(dc, colorref(theme.text_dim));
            let _ = TextOutW(dc, left, top, &style.title.encode_utf16().collect::<Vec<u16>>());
            let foot: Vec<u16> = format!("Page {} of {}", group(n), group(total)).encode_utf16().collect();
            let mut s = SIZE::default();
            let _ = GetTextExtentPoint32W(dc, &foot, &mut s);
            let _ = TextOutW(dc, left + (right - left - s.cx) / 2, bottom - small_h, &foot);
            for (r, row) in page.iter().enumerate() {
                draw_row(dc, row, left, body_top + r as i32 * line_h, char_w, digits, gutter, &fonts, theme);
            }
            if EndPage(dc) <= 0 {
                return Err("The printer didn't take the page.".into());
            }
            printed += 1;
        }
        if EndDoc(dc) <= 0 {
            return Err("The printer didn't take the document.".into());
        }
    }
    job.done = true;
    Ok(Some(printed))
}

/// One row: its line's number (right-aligned in the gutter, dimmed), then its runs, each at its column.
#[allow(clippy::too_many_arguments)]
unsafe fn draw_row(dc: HDC, row: &Row, left: i32, y: i32, char_w: i32, digits: usize, gutter: usize, fonts: &[HFONT; 4], theme: &Theme) {
    unsafe {
        if let (Some(l), true) = (row.line, digits > 0) {
            let s = l.to_string();
            SelectObject(dc, fonts[0]);
            SetTextColor(dc, colorref(theme.text_dim));
            let x = left + (digits.saturating_sub(s.len())) as i32 * char_w;
            let _ = TextOutW(dc, x, y, &s.encode_utf16().collect::<Vec<u16>>());
        }
        for run in &row.runs {
            let (bold, italic) = run.tok.map_or((false, false), |t| (print::bold(t), print::italic(t)));
            SelectObject(dc, fonts[bold as usize + 2 * italic as usize]);
            SetTextColor(dc, colorref(run.tok.map_or(theme.text, |t| tok_color(theme, t))));
            let x = left + (gutter + run.col) as i32 * char_w;
            // each character on the columns' grid (a wide one in two, whatever its glyph's width in the font)
            let (mut text, mut dx) = (Vec::new(), Vec::new());
            for c in row.text[run.start..run.end].chars() {
                let mut units = [0u16; 2];
                let units = c.encode_utf16(&mut units);
                text.extend_from_slice(units);
                dx.push(print::width(c) as i32 * char_w);
                if units.len() == 2 {
                    dx.push(0);
                }
            }
            let _ = ExtTextOutW(dc, x, y, ETO_OPTIONS(0), None, PCWSTR(text.as_ptr()), text.len() as u32, Some(dx.as_ptr()));
        }
    }
}

/// What the status bar says after printing.
pub fn done_message(pages: u64) -> String {
    format!("Sent {} to the printer", plural(pages, "page", "pages"))
}
