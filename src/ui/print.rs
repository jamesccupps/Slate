//! File → Print on Windows: the system's print dialog (PrintDlgEx), the print job started on the UI thread (a
//! printer's own dialogs, like Print to PDF's Save as, come up there), then the pages laid out (`crate::print`) and
//! drawn with GDI on a job's thread, in the printer's own pixels. Print preview (`super::preview`) draws the same
//! pages into metafiles made for the printer (`Pages`), which it plays back scaled to its window.

use std::path::Path;

use windows::Win32::Foundation::{COLORREF, HGLOBAL, HWND, RECT, SIZE};
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::Storage::Xps::{AbortDoc, DOCINFOW, EndDoc, EndPage, StartDocW, StartPage};
use windows::Win32::System::Memory::{GlobalLock, GlobalUnlock};
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

/// The printer and its settings chosen last (in the print dialog or Page setup), for the next Print and the preview
/// (as long as Slate runs).
#[derive(Default)]
pub struct Last {
    mode: HGLOBAL,
    names: HGLOBAL,
}

impl Last {
    /// A device context for the printer chosen last, with its settings (paper, orientation), to lay pages out for
    /// (the preview); the default printer if none was chosen yet. None: no printer at all.
    pub fn printer(&mut self, owner: HWND) -> Option<Printer> {
        if self.names.is_invalid() {
            // (the default printer, and its settings, without a dialog)
            let mut pd = PRINTDLGEXW {
                lStructSize: std::mem::size_of::<PRINTDLGEXW>() as u32,
                hwndOwner: owner,
                // (without page ranges to fill in: else "the parameter is incorrect")
                Flags: PD_RETURNDEFAULT | PD_NOPAGENUMS,
                nStartPage: START_PAGE_GENERAL,
                ..Default::default()
            };
            if unsafe { PrintDlgExW(&mut pd) }.is_err() || pd.hDevNames.is_invalid() {
                return None;
            }
            self.mode = pd.hDevMode;
            self.names = pd.hDevNames;
        }
        unsafe {
            let names = GlobalLock(self.names) as *const DEVNAMES;
            if names.is_null() {
                return None;
            }
            let base = names as *const u16;
            let at = |off: u16| PCWSTR(base.add(off as usize));
            let mode = if self.mode.is_invalid() { std::ptr::null_mut() } else { GlobalLock(self.mode) as *mut DEVMODEW };
            let dc = CreateDCW(at((*names).wDriverOffset), at((*names).wDeviceOffset), PCWSTR::null(), (!mode.is_null()).then_some(mode as *const DEVMODEW));
            if !mode.is_null() {
                let _ = GlobalUnlock(self.mode);
            }
            let _ = GlobalUnlock(self.names);
            (!dc.is_invalid()).then_some(Printer(dc))
        }
    }

    /// Windows' Page setup dialog (paper, orientation, margins) for the printer chosen last. True when it was OK'd:
    /// `margins` (hundredths of a millimetre: left, top, right, bottom) are the new ones, and so are the paper and
    /// orientation for the next Print.
    pub fn page_setup(&mut self, owner: HWND, margins: &mut [u32; 4]) -> bool {
        let mut ps = PAGESETUPDLGW {
            lStructSize: std::mem::size_of::<PAGESETUPDLGW>() as u32,
            hwndOwner: owner,
            hDevMode: self.mode,
            hDevNames: self.names,
            Flags: PSD_MARGINS | PSD_INHUNDREDTHSOFMILLIMETERS,
            rtMargin: RECT { left: margins[0] as i32, top: margins[1] as i32, right: margins[2] as i32, bottom: margins[3] as i32 },
            ..Default::default()
        };
        let ok = unsafe { PageSetupDlgW(&mut ps) }.as_bool();
        self.mode = ps.hDevMode;
        self.names = ps.hDevNames;
        if ok {
            let m = ps.rtMargin;
            *margins = [m.left, m.top, m.right, m.bottom].map(|v| v.max(0) as u32);
        }
        ok
    }
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
#[derive(Clone)]
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
    /// The text in its colors (else in black; bold and italic stay).
    pub colors: bool,
    /// The file's name at the top of each page, and its number at the bottom.
    pub header: bool,
    /// From the paper's edges, in hundredths of a millimetre: left, top, right, bottom (the printer's own margin where
    /// it's wider).
    pub margins: [u32; 4],
}

/// The fonts pages are drawn with (deleted when dropped; never left selected into a device context).
struct Fonts {
    text: [HFONT; 4],
    small: HFONT,
}

impl Drop for Fonts {
    fn drop(&mut self) {
        unsafe {
            for f in self.text.iter().chain([&self.small]) {
                let _ = DeleteObject(*f);
            }
        }
    }
}

/// Where everything goes on a printer's page, in its pixels from the corner of its printable area (`Geometry::new`).
pub struct Geometry {
    /// The paper: its corner (outside the printable area, so negative or 0) and its size.
    pub paper: (i32, i32, i32, i32),
    pub dpi: (i32, i32),
    left: i32,
    right: i32,
    top: i32,
    bottom: i32,
    body_top: i32,
    line_h: i32,
    char_w: i32,
    small_h: i32,
    pub rows: usize,
    pub cols: usize,
    digits: usize,
    gutter: usize,
    fonts: Fonts,
}

impl Geometry {
    /// `style`'s font measured on the printer `dc`, and the page worked out: the margins, the file's name, a blank
    /// line, the text, a blank line, the page's number.
    pub fn new(dc: HDC, style: &Style) -> Geometry {
        let cap = |i| unsafe { GetDeviceCaps(dc, i) };
        let (dpi_x, dpi_y) = (cap(LOGPIXELSX).max(72), cap(LOGPIXELSY).max(72));
        // the printable area's pixels; the paper's edges are `PHYSICALOFFSET` before it
        let (w, h) = (cap(HORZRES).max(1), cap(VERTRES).max(1));
        let (paper_w, paper_h) = (cap(PHYSICALWIDTH).max(w), cap(PHYSICALHEIGHT).max(h));
        let (off_x, off_y) = (cap(PHYSICALOFFSETX), cap(PHYSICALOFFSETY));
        let px = |mm100: u32, dpi: i32| (mm100 as i64 * dpi as i64 / 2540) as i32;
        let [ml, mt, mr, mb] = style.margins;
        let (left, right) = ((px(ml, dpi_x) - off_x).max(0), (paper_w - off_x - px(mr, dpi_x)).min(w));
        let (top, bottom) = ((px(mt, dpi_y) - off_y).max(0), (paper_h - off_y - px(mb, dpi_y)).min(h));
        let (right, bottom) = (right.max(left + 1), bottom.max(top + 1));
        let face = HSTRING::from(style.font.as_str());
        let font = |height: i32, bold: bool, italic: bool| unsafe {
            let weight = if bold { FW_BOLD } else { FW_NORMAL };
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
        let size = style.size.clamp(4.0, 72.0);
        let pt = |pt: f32| -((pt * dpi_y as f32 / 72.0).round() as i32).max(1);
        let fonts = Fonts {
            text: [font(pt(size), false, false), font(pt(size), true, false), font(pt(size), false, true), font(pt(size), true, true)],
            small: font(pt(size * 0.85), false, false),
        };
        let measure = |f: HFONT| unsafe {
            let old = SelectObject(dc, f);
            let mut tm = TEXTMETRICW::default();
            let _ = GetTextMetricsW(dc, &mut tm);
            let mut s = SIZE::default();
            let _ = GetTextExtentPoint32W(dc, &[b'0' as u16], &mut s);
            SelectObject(dc, old);
            ((tm.tmHeight + tm.tmExternalLeading).max(1), s.cx.max(1))
        };
        let (line_h, char_w) = measure(fonts.text[0]);
        let (small_h, _) = measure(fonts.small);
        let (body_top, body_bottom) = if style.header { (top + 2 * small_h, bottom - 2 * small_h) } else { (top, bottom) };
        let rows = ((body_bottom - body_top) / line_h).max(1) as usize;
        let digits = if style.numbers { style.last_line.max(1).to_string().len() } else { 0 };
        let gutter = if style.numbers { digits + 2 } else { 0 };
        let cols = (((right - left) / char_w) as usize).saturating_sub(gutter).max(1);
        Geometry {
            paper: (-off_x, -off_y, paper_w, paper_h),
            dpi: (dpi_x, dpi_y),
            left,
            right,
            top,
            bottom,
            body_top,
            line_h,
            char_w,
            small_h,
            rows,
            cols,
            digits,
            gutter,
            fonts,
        }
    }

    pub fn shape(&self, style: &Style) -> Shape {
        Shape { cols: self.cols, rows: self.rows, tab: style.tab }
    }

    /// Draws page `n` of `total` (its `rows`) on `dc`, the printable area's corner at (`x0`, `y0`).
    fn draw_page(&self, dc: HDC, style: &Style, rows: &[Row], n: u64, total: u64, x0: i32, y0: i32) {
        let theme = &style.theme;
        unsafe {
            SetBkMode(dc, TRANSPARENT);
            let old = SelectObject(dc, self.fonts.small);
            if style.header {
                SetTextColor(dc, colorref(if style.colors { theme.text_dim } else { 0xFF00_0000 }));
                let _ = TextOutW(dc, x0 + self.left, y0 + self.top, &style.title.encode_utf16().collect::<Vec<u16>>());
                let foot: Vec<u16> = format!("Page {} of {}", group(n), group(total)).encode_utf16().collect();
                let mut s = SIZE::default();
                let _ = GetTextExtentPoint32W(dc, &foot, &mut s);
                let _ = TextOutW(dc, x0 + self.left + (self.right - self.left - s.cx) / 2, y0 + self.bottom - self.small_h, &foot);
            }
            for (r, row) in rows.iter().enumerate() {
                self.draw_row(dc, style, row, x0 + self.left, y0 + self.body_top + r as i32 * self.line_h);
            }
            SelectObject(dc, old);
        }
    }

    /// One row: its line's number (right-aligned in the gutter, dimmed), then its runs, each at its column.
    unsafe fn draw_row(&self, dc: HDC, style: &Style, row: &Row, left: i32, y: i32) {
        let (theme, char_w) = (&style.theme, self.char_w);
        let black = 0xFF00_0000;
        unsafe {
            if let (Some(l), true) = (row.line, self.digits > 0) {
                let s = l.to_string();
                SelectObject(dc, self.fonts.text[0]);
                SetTextColor(dc, colorref(if style.colors { theme.text_dim } else { black }));
                let x = left + (self.digits.saturating_sub(s.len())) as i32 * char_w;
                let _ = TextOutW(dc, x, y, &s.encode_utf16().collect::<Vec<u16>>());
            }
            for run in &row.runs {
                let (bold, italic) = run.tok.map_or((false, false), |t| (print::bold(t), print::italic(t)));
                SelectObject(dc, self.fonts.text[bold as usize + 2 * italic as usize]);
                let color = match run.tok {
                    Some(t) if style.colors => tok_color(theme, t),
                    _ if style.colors => theme.text,
                    _ => black,
                };
                SetTextColor(dc, colorref(color));
                let x = left + (self.gutter + run.col) as i32 * char_w;
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
}

/// Ends the document when dropped: abandons it unless `done` (a cancel, an error, a panic) and lets the printer go.
struct Job {
    dc: HDC,
    done: bool,
}

impl Drop for Job {
    fn drop(&mut self) {
        unsafe {
            if !self.done {
                AbortDoc(self.dc);
            }
            let _ = DeleteDC(self.dc);
        }
    }
}

fn colorref(argb: u32) -> COLORREF {
    COLORREF(((argb >> 16) & 0xFF) | (argb & 0xFF00) | ((argb & 0xFF) << 16))
}

/// The error message when the layout would be more than `PAGES_MAX` pages.
pub fn too_many() -> String {
    format!("That's more than {} pages: select the part to print.", group(print::PAGES_MAX))
}

/// Lays out `text` for the printer's paper and prints the pages wanted (all of them, or those of `pages`), on a
/// started printer (`Printer::start`). `going` is told how many bytes are laid out and says whether to go on.
/// Returns how many pages were printed, None when stopped.
pub fn print(p: Printer, text: &Text, style: &Style, pages: Option<&[(u32, u32)]>, going: &mut dyn FnMut(u64) -> bool) -> Result<Option<u64>, String> {
    let dc = p.0;
    let mut job = Job { dc, done: false };
    let geo = Geometry::new(dc, style);
    let all = match print::layout(text, geo.shape(style), going) {
        Ok(Some(all)) => all,
        Ok(None) => return Ok(None),
        Err(TooMany) => return Err(too_many()),
    };
    let total = all.len().div_ceil(geo.rows) as u64;
    let wanted = |n: u64| pages.is_none_or(|r| r.iter().any(|&(a, b)| (a as u64..=b as u64).contains(&n)));
    let mut printed = 0;
    unsafe {
        for (k, page) in all.chunks(geo.rows).enumerate() {
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
            geo.draw_page(dc, style, page, n, total, 0, 0);
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
    drop(job);
    Ok(Some(printed))
}

/// A text laid out on a printer's paper, for the preview: its pages drawn into metafiles made for that printer when
/// first shown (the same drawing as printing, so what shows is what prints), played back at any size.
pub struct Pages {
    printer: HDC,
    pub geo: Geometry,
    pub style: Style,
    rows: Vec<Row>,
    /// Pages drawn already (a few: the ones shown last).
    drawn: Vec<(usize, HENHMETAFILE)>,
}

impl Pages {
    /// The page count, at least 1 (an empty text prints one page).
    pub fn count(&self) -> usize {
        self.rows.len().div_ceil(self.geo.rows).max(1)
    }

    /// The pages of `rows` (laid out for `geo`) on `printer`, which they keep.
    pub fn new(printer: Printer, geo: Geometry, style: Style, rows: Vec<Row>) -> Pages {
        Pages { printer: printer.0, geo, style, rows, drawn: Vec::new() }
    }

    /// Page `k` (from 0) as a metafile the size of the paper.
    pub fn page(&mut self, k: usize) -> HENHMETAFILE {
        if let Some(&(_, m)) = self.drawn.iter().find(|(n, _)| *n == k) {
            return m;
        }
        let (px, py, pw, ph) = self.geo.paper;
        let (dx, dy) = self.geo.dpi;
        // (the frame in hundredths of a millimetre)
        let frame = RECT { left: 0, top: 0, right: (pw as i64 * 2540 / dx as i64) as i32, bottom: (ph as i64 * 2540 / dy as i64) as i32 };
        let total = self.count();
        let rows = self.geo.rows;
        let page = self.rows.chunks(rows).nth(k).unwrap_or(&[]);
        let m = unsafe {
            let mdc = CreateEnhMetaFileW(self.printer, PCWSTR::null(), Some(&frame as *const RECT), PCWSTR::null());
            if !mdc.is_invalid() {
                // the paper's white, then the page as the printer gets it (its printable area from the paper's corner)
                let paper = RECT { left: 0, top: 0, right: pw, bottom: ph };
                let white = CreateSolidBrush(COLORREF(0xFFFFFF));
                FillRect(mdc, &paper, white);
                let _ = DeleteObject(white);
                self.geo.draw_page(mdc, &self.style, page, k as u64 + 1, total as u64, -px, -py);
            }
            CloseEnhMetaFile(mdc)
        };
        if self.drawn.len() >= 8 {
            let (_, old) = self.drawn.remove(0);
            unsafe {
                let _ = DeleteEnhMetaFile(old);
            }
        }
        self.drawn.push((k, m));
        m
    }

    /// The paper's width over its height.
    pub fn aspect(&self) -> f32 {
        let (_, _, w, h) = self.geo.paper;
        let (dx, dy) = self.geo.dpi;
        (w as f32 / dx as f32) / (h as f32 / dy as f32).max(0.01)
    }
}

impl Drop for Pages {
    fn drop(&mut self) {
        unsafe {
            for (_, m) in self.drawn.drain(..) {
                let _ = DeleteEnhMetaFile(m);
            }
            let _ = DeleteDC(self.printer);
        }
    }
}

/// What the status bar says after printing.
pub fn done_message(pages: u64) -> String {
    format!("Sent {} to the printer", plural(pages, "page", "pages"))
}
