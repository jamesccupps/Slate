//! The text view on Linux: the document cut into display segments (`edit::segment_in`: a whole line, or pieces of
//! a long one), each laid out once by Pango and cached, drawn row by row with cairo; the caret, the selection,
//! scrolling and hit testing. Rows have one height (the font's), so a row is found by its y alone. The scroll
//! position is the segment at the top and the row in it, and the scrollbar maps bytes, not rows: nothing ever lays
//! out more than what's on screen, so an 800 MB line scrolls like a small file.

use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::rc::Rc;

use gtk4::cairo;
use gtk4::pango;

use crate::core::document::{Document, Sel};
use crate::core::text;
use crate::edit::{self, HlIndex, Seg, SEG};
use crate::highlight::{self, Lang, Span, State as HlState, Tok};
use crate::theme::Theme;

/// Bytes read around a segment, so its neighbours come from the same read.
const WINDOW_EXTRA: u64 = 64 * 1024;
/// Documents up to this size get exact coloring of what spans lines (block comments, multi-line strings...).
const HL_EXACT_MAX: u64 = 32 << 20;
/// Layouts kept: at most this many, and this much text.
const CACHE_MAX: usize = 3000;
const CACHE_BYTES: usize = 2 << 20;
/// A line indented more than this part of the width doesn't hang its wrapped rows under the indentation.
const HANG_MAX: f64 = 0.5;

/// The font and what text is laid out with.
#[derive(Clone)]
pub struct Style {
    pub font: pango::FontDescription,
    /// Width of a digit (and of most characters of a monospaced font), height of a row, and the baseline in it.
    pub char_w: f64,
    pub row_h: f64,
    pub baseline: f64,
    pub tab_size: u32,
    pub wrap: bool,
    pub line_numbers: bool,
}

impl Style {
    pub fn new(ctx: &pango::Context, family: &str, points: f64, tab_size: u32, wrap: bool, line_numbers: bool) -> Style {
        let mut font = pango::FontDescription::from_string(family);
        font.set_size((points * pango::SCALE as f64) as i32);
        let m = ctx.metrics(Some(&font), None);
        let px = |v: i32| v as f64 / pango::SCALE as f64;
        let (asc, desc) = (px(m.ascent()), px(m.descent()));
        // (a monospaced font's "M" is its cell; the digit width can be a little narrower in some)
        let probe = pango::Layout::new(ctx);
        probe.set_font_description(Some(&font));
        probe.set_text("MMMMMMMMMM");
        let char_w = (probe.pixel_size().0 as f64 / 10.0).max(1.0);
        let row_h = (asc + desc).ceil().max(px(m.height()).ceil()) + 2.0;
        Style { font, char_w, row_h, baseline: ((row_h - asc - desc) / 2.0 + asc).round(), tab_size, wrap, line_numbers }
    }
}

/// The text area: what lays text out there, and its size (without the gutter), in pixels.
pub struct Geom {
    pub pango: pango::Context,
    pub width: f64,
    pub height: f64,
}

/// What the view needs to lay text out and draw it.
pub struct Ctx<'a> {
    pub doc: &'a Document,
    pub lang: Lang,
    pub style: &'a Style,
    pub theme: &'a Theme,
    pub pango: &'a pango::Context,
    /// The text area's size (without the gutter), in pixels.
    pub width: f64,
    pub height: f64,
}

impl Ctx<'_> {
    pub fn visible_rows(&self) -> usize {
        ((self.height / self.style.row_h).floor() as usize).max(1)
    }
}

/// A laid out segment: the layout, which display byte is which byte of the segment (`map`, one more at the end:
/// the segment's length), and its lines' y and baseline in the layout (Pango units).
pub struct Lay {
    pub layout: pango::Layout,
    pub map: Vec<u32>,
    pub line_y: Vec<i32>,
    bytes: usize,
}

impl Lay {
    pub fn lines(&self) -> usize {
        self.line_y.len().max(1)
    }

    /// The display index of byte `rel` of the segment (the first that's at or after it).
    pub fn disp(&self, rel: u64) -> i32 {
        self.map.partition_point(|&m| (m as u64) < rel) as i32
    }

    /// The byte of the segment display index `i` is (or the segment's end).
    pub fn rel(&self, i: i32) -> u64 {
        *self.map.get(i.max(0) as usize).unwrap_or(self.map.last().unwrap_or(&0)) as u64
    }

    /// The layout line display index `i` is on (`trailing`: an index at a wrap belongs to the line before).
    pub fn line_of(&self, i: i32, trailing: bool) -> usize {
        let (line, _) = self.layout.index_to_line_x(i, trailing);
        (line.max(0) as usize).min(self.lines() - 1)
    }

    /// x (pixels, in the layout) of display index `i`.
    pub fn x_of(&self, i: i32) -> f64 {
        self.layout.index_to_pos(i).x() as f64 / pango::SCALE as f64
    }

    /// The display range `[a, b)` of layout line `line`.
    pub fn line_range(&self, line: usize) -> (i32, i32) {
        match self.layout.line_readonly(line as i32) {
            Some(l) => (l.start_index(), l.start_index() + l.length()),
            None => (0, 0),
        }
    }

    /// The display index nearest x (pixels, in the layout) on layout line `line`.
    pub fn index_at(&self, line: usize, x: f64) -> i32 {
        let y = self.line_y.get(line).copied().unwrap_or(0) + 1;
        let (_, idx, trailing) = self.layout.xy_to_index((x * pango::SCALE as f64) as i32, y);
        // (trailing: past the middle of a cluster; the index after it, which can't leave the line)
        let (a, b) = self.line_range(line);
        let mut i = idx;
        if trailing > 0 {
            i = self.next_index(idx);
        }
        i.clamp(a, b)
    }

    /// The display index after the character at `i`.
    fn next_index(&self, i: i32) -> i32 {
        let text = self.layout.text();
        let s = text.as_str();
        let i = (i.max(0) as usize).min(s.len());
        match s[i..].chars().next() {
            Some(c) => (i + c.len_utf8()) as i32,
            None => i as i32,
        }
    }
}

/// A row on screen: layout line `line` of segment `seg`, at `y` (pixels, in the text area).
#[derive(Clone)]
pub struct VisRow {
    pub seg: Seg,
    pub lay: Rc<Lay>,
    pub line: usize,
    pub y: f64,
    /// 1-based line number on the first row of a line (when it's known).
    pub line_no: Option<u64>,
}

struct CacheEntry {
    lay: Rc<Lay>,
    used: u64,
}

#[derive(Default)]
struct TextWindow {
    start: u64,
    data: Vec<u8>,
    version: u64,
    len: u64,
    valid: bool,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Drag {
    Chars,
    Words(u64, u64),
    Lines(u64, u64),
}

pub struct View {
    pub sel: Sel,
    /// The x the caret keeps while it moves up and down (pixels, in the text).
    pub want_x: Option<f64>,
    /// The segment at the top, and the row in it.
    pub top: u64,
    pub top_row: usize,
    pub scroll_x: f64,
    /// Widest row seen lately (horizontal scrolling without word wrap).
    pub max_w: f64,
    pub rows: Vec<VisRow>,
    pub drag: Option<Drag>,
    cache: HashMap<u64, CacheEntry>,
    cache_bytes: usize,
    tick: u64,
    win: TextWindow,
    hl: HlIndex,
    /// The layout settings the cache is for (width, wrap, font...).
    shape: u64,
    spans: Vec<Span>,
}

impl Default for View {
    fn default() -> Self {
        View::new()
    }
}

/// A color 0xAARRGGBB for cairo.
pub fn set_color(cr: &cairo::Context, argb: u32) {
    let f = |s: u32| ((argb >> s) & 0xFF) as f64 / 255.0;
    cr.set_source_rgba(f(16), f(8), f(0), f(24));
}

fn pango_rgb(argb: u32) -> (u16, u16, u16) {
    let f = |s: u32| (((argb >> s) & 0xFF) * 257) as u16;
    (f(16), f(8), f(0))
}

pub fn tok_color(t: &Theme, tok: Tok) -> u32 {
    match tok {
        Tok::Key => t.syn_key,
        Tok::Str => t.syn_string,
        Tok::Num => t.syn_number,
        Tok::Lit => t.syn_literal,
        Tok::Punct => t.syn_punct,
        Tok::Comment => t.syn_comment,
        Tok::Section => t.syn_section,
        Tok::Error => t.syn_error,
        Tok::Warn => t.syn_warn,
        Tok::Info => t.syn_info,
        Tok::Dim => t.syn_dim,
        Tok::Keyword => t.syn_keyword,
        Tok::Control => t.syn_control,
        Tok::Type => t.syn_type,
        Tok::Func => t.syn_func,
        Tok::Tag => t.syn_tag,
        Tok::Attr => t.syn_attr,
        Tok::Var => t.syn_var,
        Tok::Heading => t.syn_heading,
        Tok::Link => t.syn_link,
        Tok::Added => t.syn_added,
        Tok::Removed => t.syn_removed,
        Tok::Col(k) => t.syn_cols[k as usize % 8],
        Tok::Bold | Tok::Italic => t.text,
    }
}

/// The segment's bytes as text Pango can lay out: control characters as their symbols (␀, ␍...), bytes that
/// aren't UTF-8 as U+FFFD; with which byte of the segment each display byte is.
pub fn display_text(bytes: &[u8]) -> (String, Vec<u32>) {
    let mut out = String::with_capacity(bytes.len());
    let mut map = Vec::with_capacity(bytes.len() + 1);
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        let (c, n) = if b < 0x80 {
            let c = match b {
                b'\t' => '\t',
                0x00..=0x1F => char::from_u32(0x2400 + b as u32).unwrap(),
                0x7F => '\u{2421}',
                _ => b as char,
            };
            (c, 1)
        } else {
            let n = text::char_len_at(&bytes[i..]).max(1);
            match std::str::from_utf8(&bytes[i..i + n]).ok().and_then(|s| s.chars().next()) {
                Some(c) => (c, n),
                None => ('\u{FFFD}', 1),
            }
        };
        let start = out.len();
        out.push(c);
        map.extend(std::iter::repeat_n(i as u32, out.len() - start));
        i += n;
    }
    map.push(bytes.len() as u32);
    (out, map)
}

/// Columns of a line's indentation (spaces and tabs at its start).
fn indent_cols(bytes: &[u8], tab: u32) -> u32 {
    let mut col = 0u32;
    for &b in bytes {
        match b {
            b' ' => col += 1,
            b'\t' => col = (col / tab.max(1) + 1) * tab.max(1),
            _ => break,
        }
    }
    col
}

impl View {
    pub fn new() -> View {
        View {
            sel: Sel::at(0),
            want_x: None,
            top: 0,
            top_row: 0,
            scroll_x: 0.0,
            max_w: 0.0,
            rows: Vec::new(),
            drag: None,
            cache: HashMap::new(),
            cache_bytes: 0,
            tick: 0,
            win: TextWindow::default(),
            hl: HlIndex::default(),
            shape: 0,
            spans: Vec::new(),
        }
    }

    /// Takes the document's changes (after edits, undo, a reload): coloring states after them are worked out again.
    pub fn sync(&mut self, doc: &mut Document) {
        // (the coloring index applies pending changes itself as it's asked; those it hasn't seen yet, here)
        for (k, c) in doc.take_changes().iter().enumerate() {
            if k >= self.hl.seen {
                self.hl.edited(c.at);
            }
        }
        self.hl.seen = 0;
        self.hl.len = doc.len();
        self.win.valid = false;
        let len = doc.len();
        self.sel.anchor = self.sel.anchor.min(len);
        self.sel.caret = self.sel.caret.min(len);
        self.top = self.top.min(len);
    }

    /// Forgets the laid out text (another font, theme or language).
    pub fn clear_cache(&mut self) {
        self.cache.clear();
        self.cache_bytes = 0;
        self.hl.reset();
        self.rows.clear();
    }

    // ---- segments ----

    fn window(&mut self, doc: &Document, a: u64, b: u64) -> &[u8] {
        let w = &mut self.win;
        let len = doc.len();
        let b = b.min(len);
        let ok = w.valid && w.version == doc.version && w.len == len && a >= w.start && b <= w.start + w.data.len() as u64;
        if !ok {
            w.data.clear();
            doc.read_into(a, (b + WINDOW_EXTRA).min(len), &mut w.data);
            w.start = a;
            w.version = doc.version;
            w.len = len;
            w.valid = true;
        }
        &w.data[(a - w.start) as usize..(b - w.start) as usize]
    }

    /// The segment containing `off` (or ending at it).
    pub fn segment_at(&mut self, doc: &Document, off: u64) -> Seg {
        let len = doc.len();
        let off = off.min(len);
        let w0 = off.saturating_sub(2 * SEG);
        let w1 = (off + 2 * SEG + 4).min(len);
        let buf = self.window(doc, w0, w1);
        edit::segment_in(buf, w0, len, off)
    }

    /// The segment before `seg` (None at the start).
    pub fn prev_segment(&mut self, doc: &Document, seg: &Seg) -> Option<Seg> {
        (seg.start > 0).then(|| self.segment_at(doc, seg.start - 1))
    }

    fn is_last(seg: &Seg, len: u64) -> bool {
        seg.next_start() >= len && (seg.eol == 0 || seg.next_start() > len)
    }

    // ---- layouts ----

    fn hl_state(&mut self, cx: &Ctx, seg: &Seg) -> HlState {
        if cx.lang == Lang::Plain {
            return HlState::START;
        }
        if cx.doc.is_ready() && cx.doc.len() <= HL_EXACT_MAX {
            return self.hl.state_at(cx.doc, cx.lang, seg.start);
        }
        // Too big to know: a line starts fresh; a piece of a long line from what's just before it.
        if seg.line_start {
            return HlState::START;
        }
        let a = seg.start.saturating_sub(4096);
        let prefix = cx.doc.read(a, seg.start);
        let from = memchr::memrchr(b'\n', &prefix).map_or(0, |p| p + 1);
        highlight::lex(cx.lang, &prefix[from..], HlState::START, None)
    }

    /// What the layouts depend on besides their text: the font, the width, word wrap, the theme's colors.
    fn shape_of(cx: &Ctx) -> u64 {
        let mut h = std::collections::hash_map::DefaultHasher::new();
        cx.style.font.to_string().hash(&mut h);
        cx.style.tab_size.hash(&mut h);
        cx.style.wrap.hash(&mut h);
        if cx.style.wrap {
            (cx.width as i64).hash(&mut h);
        }
        cx.theme.syn_key.hash(&mut h);
        cx.theme.text.hash(&mut h);
        cx.theme.dark.hash(&mut h);
        h.finish()
    }

    pub fn layout_of(&mut self, cx: &Ctx, seg: &Seg) -> Rc<Lay> {
        let shape = Self::shape_of(cx);
        if shape != self.shape {
            self.cache.clear();
            self.cache_bytes = 0;
            self.shape = shape;
        }
        let st = self.hl_state(cx, seg);
        let bytes = cx.doc.read(seg.start, seg.end);
        let mut h = std::collections::hash_map::DefaultHasher::new();
        bytes.hash(&mut h);
        st.hash(&mut h);
        seg.line_start.hash(&mut h);
        cx.lang.hash(&mut h);
        let key = h.finish();
        self.tick += 1;
        if let Some(e) = self.cache.get_mut(&key) {
            e.used = self.tick;
            return e.lay.clone();
        }
        let lay = Rc::new(self.build(cx, &bytes, st, seg.line_start));
        self.cache_bytes += lay.bytes;
        if self.cache.len() >= CACHE_MAX || self.cache_bytes > CACHE_BYTES {
            // the oldest half goes (never what's on screen: those were used since)
            let mut ages: Vec<u64> = self.cache.values().map(|e| e.used).collect();
            ages.sort_unstable();
            let cut = ages[ages.len() / 2];
            let on_screen: Vec<*const Lay> = self.rows.iter().map(|r| Rc::as_ptr(&r.lay)).collect();
            self.cache.retain(|_, e| e.used > cut || on_screen.contains(&Rc::as_ptr(&e.lay)));
            self.cache_bytes = self.cache.values().map(|e| e.lay.bytes).sum();
        }
        self.cache.insert(key, CacheEntry { lay: lay.clone(), used: self.tick });
        lay
    }

    fn build(&mut self, cx: &Ctx, bytes: &[u8], st: HlState, line_start: bool) -> Lay {
        let (text, map) = display_text(bytes);
        let layout = pango::Layout::new(cx.pango);
        layout.set_font_description(Some(&cx.style.font));
        let tab_px = (cx.style.tab_size.max(1) as f64 * cx.style.char_w).round() as i32;
        let mut tabs = pango::TabArray::new(1, true);
        tabs.set_tab(0, pango::TabAlign::Left, tab_px);
        layout.set_tabs(Some(&tabs));
        layout.set_single_paragraph_mode(true);
        if cx.style.wrap {
            layout.set_width((cx.width.max(cx.style.char_w * 4.0) * pango::SCALE as f64) as i32);
            layout.set_wrap(pango::WrapMode::WordChar);
            // wrapped rows hang under the line's indentation (unless it's most of the width, or a long line's piece)
            if line_start && bytes.len() < 4096 {
                let hang = indent_cols(bytes, cx.style.tab_size) as f64 * cx.style.char_w;
                if hang > 0.0 && hang < cx.width * HANG_MAX {
                    layout.set_indent(-(hang * pango::SCALE as f64) as i32);
                }
            }
        }
        layout.set_text(&text);
        // colors
        if cx.lang != Lang::Plain && !cx.theme.hc {
            self.spans.clear();
            highlight::lex(cx.lang, bytes, st, Some(&mut self.spans));
            let attrs = pango::AttrList::new();
            let disp = |rel: u32| map.partition_point(|&m| m < rel) as u32;
            for &(s, e, tok) in &self.spans {
                let (a, b) = (disp(s), disp(e));
                if b <= a {
                    continue;
                }
                let (r, g, bl) = pango_rgb(tok_color(cx.theme, tok));
                let mut c = pango::AttrColor::new_foreground(r, g, bl);
                c.set_start_index(a);
                c.set_end_index(b);
                attrs.insert(c);
                if matches!(tok, Tok::Bold | Tok::Heading | Tok::Error) {
                    let mut w = pango::AttrInt::new_weight(pango::Weight::Bold);
                    w.set_start_index(a);
                    w.set_end_index(b);
                    attrs.insert(w);
                } else if tok == Tok::Italic {
                    let mut i = pango::AttrInt::new_style(pango::Style::Italic);
                    i.set_start_index(a);
                    i.set_end_index(b);
                    attrs.insert(i);
                }
            }
            layout.set_attributes(Some(&attrs));
        }
        let mut line_y = Vec::with_capacity(layout.line_count().max(1) as usize);
        let mut it = layout.iter();
        loop {
            line_y.push(it.line_yrange().0);
            if !it.next_line() {
                break;
            }
        }
        let bytes_kept = text.len() * 6 + map.len() * 4 + 256;
        Lay { layout, map, line_y, bytes: bytes_kept }
    }

    // ---- rows ----

    /// Works out the rows on screen from the top down.
    pub fn layout_rows(&mut self, cx: &Ctx) {
        let len = cx.doc.len();
        let n = cx.visible_rows() + 1;
        let mut rows = Vec::with_capacity(n);
        let mut seg = self.segment_at(cx.doc, self.top);
        if seg.start != self.top {
            self.top = seg.start;
            self.top_row = 0;
        }
        let mut row = self.top_row;
        let mut y = 0.0;
        'outer: loop {
            let lay = self.layout_of(cx, &seg);
            if row >= lay.lines() {
                row = lay.lines() - 1;
                self.top_row = self.top_row.min(row);
            }
            let line_no = if seg.line_start { cx.doc.line_of(seg.start).map(|l| l + 1) } else { None };
            for line in row..lay.lines() {
                rows.push(VisRow { seg, lay: lay.clone(), line, y, line_no: if line == 0 { line_no } else { None } });
                y += cx.style.row_h;
                if rows.len() >= n {
                    break 'outer;
                }
            }
            if Self::is_last(&seg, len) {
                break;
            }
            seg = self.segment_at(cx.doc, seg.next_start());
            row = 0;
        }
        if !cx.style.wrap {
            for r in &rows {
                let w = r.lay.layout.pixel_size().0 as f64;
                if w > self.max_w {
                    self.max_w = w;
                }
            }
        }
        self.rows = rows;
    }

    /// The row after (segment, row) (None at the end).
    fn next_row(&mut self, cx: &Ctx, seg: Seg, row: usize) -> Option<(Seg, usize)> {
        let lay = self.layout_of(cx, &seg);
        if row + 1 < lay.lines() {
            return Some((seg, row + 1));
        }
        if Self::is_last(&seg, cx.doc.len()) {
            return None;
        }
        Some((self.segment_at(cx.doc, seg.next_start()), 0))
    }

    /// The row before (segment, row) (None at the start).
    fn prev_row(&mut self, cx: &Ctx, seg: Seg, row: usize) -> Option<(Seg, usize)> {
        if row > 0 {
            return Some((seg, row - 1));
        }
        let prev = self.prev_segment(cx.doc, &seg)?;
        let lay = self.layout_of(cx, &prev);
        Some((prev, lay.lines() - 1))
    }

    /// The top as far down as it goes: the last page's first row.
    fn max_top(&mut self, cx: &Ctx) -> (u64, usize) {
        let len = cx.doc.len();
        let mut seg = self.segment_at(cx.doc, len);
        let lay = self.layout_of(cx, &seg);
        let mut row = lay.lines() - 1;
        for _ in 1..cx.visible_rows() {
            match self.prev_row(cx, seg, row) {
                Some((s, r)) => (seg, row) = (s, r),
                None => break,
            }
        }
        (seg.start, row)
    }

    fn clamp_top(&mut self, cx: &Ctx) {
        let (mt, mr) = self.max_top(cx);
        if (self.top, self.top_row) > (mt, mr) {
            (self.top, self.top_row) = (mt, mr);
        }
    }

    /// Scrolls by `n` rows (down when positive).
    pub fn scroll_rows(&mut self, cx: &Ctx, n: i64) {
        let mut seg = self.segment_at(cx.doc, self.top);
        let mut row = self.top_row;
        for _ in 0..n.unsigned_abs() {
            let next = if n > 0 { self.next_row(cx, seg, row) } else { self.prev_row(cx, seg, row) };
            match next {
                Some((s, r)) => (seg, row) = (s, r),
                None => break,
            }
        }
        self.top = seg.start;
        self.top_row = row;
        if n > 0 {
            self.clamp_top(cx);
        }
    }

    /// Puts the top at the segment holding `off` (the scrollbar was dragged there).
    pub fn scroll_to_offset(&mut self, cx: &Ctx, off: u64) {
        let seg = self.segment_at(cx.doc, off);
        self.top = seg.start;
        self.top_row = 0;
        self.clamp_top(cx);
    }

    /// Where the caret is: its segment, layout, line and x in the layout.
    pub fn caret_place(&mut self, cx: &Ctx, pos: u64) -> (Seg, Rc<Lay>, usize, f64) {
        let seg = self.segment_at(cx.doc, pos);
        let lay = self.layout_of(cx, &seg);
        let i = lay.disp(pos.saturating_sub(seg.start));
        let line = lay.line_of(i, false);
        let x = lay.x_of(i);
        (seg, lay, line, x)
    }

    /// The caret's rectangle in the text area (None when it isn't on screen).
    pub fn caret_rect(&mut self, cx: &Ctx) -> Option<(f64, f64, f64, f64)> {
        let pos = self.sel.caret;
        let (seg, _, line, x) = self.caret_place(cx, pos);
        let r = self.rows.iter().find(|r| r.seg.start == seg.start && r.line == line)?;
        Some((x - self.scroll_x, r.y, 2.0, cx.style.row_h))
    }

    /// The document offset at (x, y) of the text area.
    pub fn pos_at(&mut self, cx: &Ctx, x: f64, y: f64) -> u64 {
        if self.rows.is_empty() {
            self.layout_rows(cx);
        }
        let Some(last) = self.rows.last().cloned() else { return 0 };
        let row = if y < 0.0 {
            self.rows[0].clone()
        } else {
            let k = (y / cx.style.row_h) as usize;
            self.rows.get(k).cloned().unwrap_or(last)
        };
        let i = row.lay.index_at(row.line, x + self.scroll_x);
        row.seg.start + row.lay.rel(i).min(row.seg.end - row.seg.start)
    }

    /// Scrolls so the caret is on screen (`center`: in the middle, when it's far).
    pub fn reveal(&mut self, cx: &Ctx, center: bool) {
        let pos = self.sel.caret;
        let (seg, lay, line, x) = self.caret_place(cx, pos);
        let caret = (seg.start, line);
        let above = caret < (self.top, self.top_row);
        let visible = cx.visible_rows();
        // where the last full row is
        let mut bottom = (self.top, self.top_row);
        let mut s = self.segment_at(cx.doc, self.top);
        let mut r = self.top_row;
        let mut far = true;
        for _ in 1..visible {
            match self.next_row(cx, s, r) {
                Some((ns, nr)) => {
                    (s, r) = (ns, nr);
                    bottom = (s.start, r);
                }
                None => break,
            }
            if (s.start, r) == caret {
                far = false;
            }
        }
        if (self.top, self.top_row) == caret {
            far = false;
        }
        if above || caret > bottom {
            // back from the caret: one row (it goes at the top), a page minus one (at the bottom), or half
            let back = if center && far { visible / 2 } else if above { 0 } else { visible - 1 };
            let (mut s, mut r) = (seg, line);
            for _ in 0..back {
                match self.prev_row(cx, s, r) {
                    Some((ps, pr)) => (s, r) = (ps, pr),
                    None => break,
                }
            }
            self.top = s.start;
            self.top_row = r;
            self.clamp_top(cx);
        }
        // sideways (no word wrap)
        if !cx.style.wrap {
            let margin = cx.style.char_w * 4.0;
            if x < self.scroll_x + margin {
                self.scroll_x = (x - margin).max(0.0);
            } else if x > self.scroll_x + cx.width - margin {
                self.scroll_x = x - cx.width + margin;
            }
        } else {
            self.scroll_x = 0.0;
        }
        let _ = lay;
    }

    // ---- caret movement ----

    pub fn set_caret(&mut self, pos: u64, extend: bool) {
        if extend {
            self.sel.caret = pos;
        } else {
            self.sel = Sel::at(pos);
        }
    }

    /// Moves the caret `n` rows down (up when negative), keeping its x.
    pub fn move_rows(&mut self, cx: &Ctx, n: i64, extend: bool) {
        let (seg, lay, line, x) = self.caret_place(cx, self.sel.caret);
        let want = *self.want_x.get_or_insert(x);
        let (mut s, mut r) = (seg, line);
        let mut moved = 0;
        for _ in 0..n.unsigned_abs() {
            let next = if n > 0 { self.next_row(cx, s, r) } else { self.prev_row(cx, s, r) };
            match next {
                Some((ns, nr)) => {
                    (s, r) = (ns, nr);
                    moved += 1;
                }
                None => break,
            }
        }
        let pos = if moved == 0 {
            // at the first or last row: to the start or the end
            if n < 0 { 0 } else { cx.doc.len() }
        } else {
            let l = self.layout_of(cx, &s);
            let i = l.index_at(r, want);
            s.start + l.rel(i).min(s.end - s.start)
        };
        let _ = lay;
        self.set_caret(pos, extend);
    }

    /// Page Up / Page Down: the caret and the view move by a page.
    pub fn page(&mut self, cx: &Ctx, down: bool, extend: bool) {
        let n = cx.visible_rows().saturating_sub(1).max(1) as i64;
        self.scroll_rows(cx, if down { n } else { -n });
        self.move_rows(cx, if down { n } else { -n }, extend);
    }

    /// Home: to the first character that isn't a space, then to the start of the line (of the row, with wrap).
    pub fn home(&mut self, cx: &Ctx, extend: bool) {
        let pos = self.sel.caret;
        let (seg, lay, line, _) = self.caret_place(cx, pos);
        let row_start = seg.start + lay.rel(lay.line_range(line).0);
        let line_start = cx.doc.line_start_of(pos);
        let head = cx.doc.read(line_start, (line_start + 4096).min(cx.doc.len()));
        let ws = head.iter().take_while(|&&b| b == b' ' || b == b'\t').count() as u64;
        let text_start = line_start + ws;
        let target = if row_start > line_start && pos != row_start {
            row_start
        } else if pos != text_start && text_start < cx.doc.line_end_of(pos).max(line_start) + 1 {
            text_start
        } else {
            line_start
        };
        self.set_caret(target, extend);
    }

    /// End: to the end of the row (with wrap), then of the line.
    pub fn end(&mut self, cx: &Ctx, extend: bool) {
        let pos = self.sel.caret;
        let (seg, lay, line, _) = self.caret_place(cx, pos);
        let (_, b) = lay.line_range(line);
        let mut row_end = seg.start + lay.rel(b);
        // (a wrapped row ends at a space, which belongs to it: stop before it)
        if line + 1 < lay.lines() && row_end > seg.start {
            row_end -= 1;
        }
        let line_end = cx.doc.line_end_of(pos);
        let target = if row_end < line_end && pos != row_end { row_end } else { line_end };
        self.set_caret(target, extend);
    }

    // ---- painting ----

    /// Draws the rows: the current line, the selection, find matches, the text, the caret. `ox`: where the text
    /// starts (after the gutter).
    #[allow(clippy::too_many_arguments)]
    pub fn paint(&mut self, cx: &Ctx, cr: &cairo::Context, ox: f64, focused: bool, caret_on: bool, matches: &[(u64, u64)]) {
        self.layout_rows(cx);
        let t = cx.theme;
        let row_h = cx.style.row_h;
        let caret = self.sel.caret;
        let (sa, sb) = (self.sel.start(), self.sel.end());
        cr.save().ok();
        cr.rectangle(ox, 0.0, cx.width, cx.height);
        cr.clip();
        let rows = std::mem::take(&mut self.rows);
        let x0 = ox - self.scroll_x;
        // the line the caret is on
        if self.sel.is_empty() {
            for r in rows.iter() {
                let (a, b) = (r.seg.start, r.seg.end + r.seg.eol as u64);
                if caret >= a && (caret < b || (caret == b && r.seg.eol == 0)) {
                    set_color(cr, t.current_line);
                    cr.rectangle(ox, r.y, cx.width, row_h);
                    let _ = cr.fill();
                }
            }
        }
        let fill = |cr: &cairo::Context, r: &VisRow, a: u64, b: u64, color: u32, eol: bool| {
            let (la, lb) = r.lay.line_range(r.line);
            let sa = r.seg.start + r.lay.rel(la);
            let sb = r.seg.start + r.lay.rel(lb);
            let (a, b) = (a.max(sa), b.min(sb));
            if b < a || (b == a && !eol) {
                return;
            }
            let (ia, ib) = (r.lay.disp(a - r.seg.start), r.lay.disp(b - r.seg.start));
            set_color(cr, color);
            if let Some(line) = r.lay.layout.line_readonly(r.line as i32) {
                let ranges = line.x_ranges(ia, ib);
                for p in ranges.chunks(2) {
                    let (xa, xb) = (p[0] as f64 / pango::SCALE as f64, p[1] as f64 / pango::SCALE as f64);
                    cr.rectangle(x0 + xa, r.y, (xb - xa).max(1.0), row_h);
                }
            }
            if eol {
                // the line break, selected: a little more than a space
                let x = r.lay.x_of(ib);
                cr.rectangle(x0 + x, r.y, cx.style.char_w * 0.6, row_h);
            }
            let _ = cr.fill();
        };
        for (a, b) in matches {
            for r in rows.iter() {
                fill(cr, r, *a, *b, t.match_bg, false);
            }
        }
        if sb > sa {
            let color = if focused { t.selection } else { t.selection_inactive };
            for r in rows.iter() {
                let last_row = r.line + 1 == r.lay.lines();
                let line_end = r.seg.end;
                let eol = last_row && r.seg.eol > 0 && sa <= line_end && sb > line_end;
                fill(cr, r, sa, sb, color, eol);
            }
        }
        // the text
        set_color(cr, t.text);
        for r in rows.iter() {
            let Some(line) = r.lay.layout.line_readonly(r.line as i32) else { continue };
            // (the line's own x in the layout: an indent, a wrapped row hanging under the indentation)
            let lx = r.lay.x_of(line.start_index()) - line_x_from_start(&r.lay, r.line);
            cr.move_to(x0 + lx, r.y + cx.style.baseline);
            pangocairo::functions::show_layout_line(cr, &line);
        }
        // the caret
        if focused && caret_on {
            for r in rows.iter() {
                let (la, lb) = r.lay.line_range(r.line);
                let (a, b) = (r.seg.start + r.lay.rel(la), r.seg.start + r.lay.rel(lb));
                let last = r.line + 1 == r.lay.lines();
                if caret >= a && (caret < b || (caret == b && last)) {
                    let x = r.lay.x_of(r.lay.disp(caret - r.seg.start));
                    set_color(cr, t.caret);
                    cr.rectangle((x0 + x).round(), r.y, 2.0, row_h);
                    let _ = cr.fill();
                    break;
                }
            }
        }
        cr.restore().ok();
        self.rows = rows;
    }
}

/// How far into its line a layout line's first character is drawn (its x in the layout is what `index_to_pos`
/// gives; the line itself is drawn from its start).
fn line_x_from_start(lay: &Lay, line: usize) -> f64 {
    let Some(l) = lay.layout.line_readonly(line as i32) else { return 0.0 };
    // x of the line's first index within the line
    l.index_to_x(l.start_index(), false) as f64 / pango::SCALE as f64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn control_characters_show_as_symbols_and_bad_bytes_as_replacements() {
        let (text, map) = display_text(b"a\tb\x01c\xFFd\xC3\xA9");
        assert_eq!(text, "a\tb\u{2401}c\u{FFFD}d\u{E9}");
        // every display byte knows its byte, and one more for the end
        assert_eq!(map.len(), text.len() + 1);
        assert_eq!(*map.last().unwrap(), 9);
        let i = text.find('d').unwrap();
        assert_eq!(map[i], 6);
    }

    #[test]
    fn indentation_columns_count_tabs_to_their_stops() {
        assert_eq!(indent_cols(b"    x", 4), 4);
        assert_eq!(indent_cols(b"\t  x", 4), 6);
        assert_eq!(indent_cols(b"  \tx", 4), 4);
        assert_eq!(indent_cols(b"x", 4), 0);
    }
}
