//! The text view: turns a document into rows on screen and back (painting, hit testing, caret movement,
//! scrolling), plus the text-editing operations.
//!
//! Display model: the document is cut into *segments* — a whole line, or for lines longer than a few KB, pieces
//! cut at fixed 8 KiB grid points (`segment_at`). Each segment gets one cached DirectWrite layout, which may wrap
//! into several *rows*. Nothing ever lays out more than what's on screen, so a single 800 MB line scrolls as
//! smoothly as a small file. The scroll position `top` is the byte offset where the first visible row starts;
//! the scrollbar maps bytes, not rows, so it never needs the whole file laid out.

use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::rc::Rc;

use windows::Win32::Foundation::BOOL;
use windows::Win32::Graphics::DirectWrite::{
    DWRITE_CLUSTER_METRICS, DWRITE_FONT_STYLE_ITALIC, DWRITE_FONT_WEIGHT_BOLD, DWRITE_HIT_TEST_METRICS, DWRITE_LINE_METRICS, DWRITE_TEXT_METRICS, DWRITE_TEXT_RANGE, IDWriteTextFormat,
    IDWriteTextLayout,
};

use crate::core::document::{Document, Sel};
use crate::core::text;

pub use crate::edit::*;

use super::gfx::{Gfx, Ink, Rect};
use super::highlight::{self, CommentStyle, Lang, Span, State as HlState, Tok};
use super::theme::{Theme, metrics};

const WINDOW_EXTRA: u64 = 64 * 1024;
const PREFIX_MAX: u64 = 4096;
/// Layouts kept of segments shown recently: at most this many...
const CACHE_MAX: usize = 4000;
/// ...and this much of their text: a colored 8 KiB segment's layout takes half a MB, so the count alone let a long line
/// scrolled through without word wrap fill gigabytes.
const CACHE_BYTES: usize = 1 << 20;

/// A DirectWrite layout of a segment's text from UTF-16 position `at` on, whose first row is the segment's row
/// `row0`, drawn `x` to the right of the text's left edge.
pub struct Part {
    pub layout: IDWriteTextLayout,
    pub at: u32,
    pub x: f32,
    pub row0: usize,
}

pub struct SegLayout {
    /// The text in one layout; or, wrapped with the line indented, its first row and then the rows after it, laid out
    /// narrower to hang under the line's indentation (as VS Code does).
    pub parts: Vec<Part>,
    /// Byte offset (from the segment start) of each UTF-16 unit, plus one for the end.
    pub map: Vec<u32>,
    /// Rows as UTF-16 ranges `[start, end)`; a row's end is the next row's start.
    pub rows: Vec<(u32, u32)>,
    pub width: f32,
    /// No character that may show as a color glyph (emoji), which only Windows 11 draws a run at a time
    /// (`Gfx::draw_layout_in`).
    pub plain: bool,
    /// Where the colors start (UTF-16 position, color), when they're given at drawing time rather than set on the
    /// layout (which costs a lot more when the text is packed with them; but a layout drawn whole needs them on it).
    pub colors: Vec<(u32, u32)>,
}

/// A UTF-16 unit of a character that may show as a color glyph: emoji (all outside the BMP, the symbols and dingbats
/// of U+2190…U+2BFF, the emoji variation selector) and the few others with an emoji form. (Not the stand-ins for
/// control characters, U+2400…U+243F.)
fn maybe_color(u: u16) -> bool {
    matches!(u, 0xD800..=0xDFFF | 0x2190..=0x23FF | 0x2460..=0x2BFF | 0xFE0F | 0x00A9 | 0x00AE | 0x203C | 0x2049)
        || matches!(u, 0x2122 | 0x2139 | 0x3030 | 0x303D | 0x3297 | 0x3299)
}

impl Part {
    /// x of the end of the character at `u` (counted from the part's start), from the part's left edge.
    fn end_x(&self, u: u32) -> f32 {
        let (mut x, mut y) = (0f32, 0f32);
        let mut m = DWRITE_HIT_TEST_METRICS::default();
        unsafe {
            let _ = self.layout.HitTestTextPosition(u, BOOL(1), &mut x, &mut y, &mut m);
        }
        x
    }
}

/// The rows of a layout of the segment's text from `at` to `end`, as UTF-16 ranges of the segment.
fn layout_rows_of(layout: &IDWriteTextLayout, at: u32, end: u32) -> Vec<(u32, u32)> {
    let mut count = 0u32;
    unsafe {
        let _ = layout.GetLineMetrics(None, &mut count);
    }
    let mut lm = vec![DWRITE_LINE_METRICS::default(); count.max(1) as usize];
    unsafe {
        let _ = layout.GetLineMetrics(Some(&mut lm), &mut count);
    }
    lm.truncate(count.max(1) as usize);
    let mut rows = Vec::with_capacity(lm.len());
    let mut pos = at;
    for m in &lm {
        let e = (pos + m.length).min(end);
        rows.push((pos, e));
        pos = e;
    }
    if let Some(last) = rows.last_mut() {
        last.1 = end;
    }
    rows
}

fn tok_color(t: &Theme, tok: Tok) -> u32 {
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

/// Colors a layout of the segment's text from UTF-16 position `at` to `end` with the spans (byte ranges of the
/// segment) that fall in it: bold and italic, and (`colors`) the colors.
fn color_layout(cx: &Ctx, layout: &IDWriteTextLayout, spans: &[Span], map: &[u32], at: u32, end: u32, colors: bool) {
    let t = cx.theme;
    for &(s, e, tok) in spans {
        // (an error bold too: it mustn't pass for a string, PPCL's values, in any theme; not in a simulated bold,
        // which would widen it)
        let bold = matches!(tok, Tok::Bold | Tok::Heading) || (tok == Tok::Error && cx.style.real_bold);
        let italic = matches!(tok, Tok::Italic);
        if !colors && !bold && !italic {
            continue;
        }
        let us = (map.partition_point(|&m| m < s) as u32).max(at);
        let ue = (map.partition_point(|&m| m < e) as u32).min(end);
        if ue <= us {
            continue;
        }
        let range = DWRITE_TEXT_RANGE { startPosition: us - at, length: ue - us };
        unsafe {
            // (High contrast: all text in the one text color, which selected text is drawn over in its own.)
            if colors && !t.hc {
                let _ = layout.SetDrawingEffect(&cx.g.brush(tok_color(t, tok)), range);
            }
            if bold {
                let _ = layout.SetFontWeight(DWRITE_FONT_WEIGHT_BOLD, range);
            }
            if italic {
                let _ = layout.SetFontStyle(DWRITE_FONT_STYLE_ITALIC, range);
            }
        }
    }
}

/// Where the colors of the spans start in a text of `total` UTF-16 units (a later span wins where they overlap, as on
/// a layout); none for text without spans.
fn colors_of(t: &Theme, spans: &[Span], map: &[u32], total: u32) -> Vec<(u32, u32)> {
    if spans.is_empty() || t.hc {
        return Vec::new();
    }
    let mut each = vec![t.text; total as usize];
    for &(s, e, tok) in spans {
        let us = map.partition_point(|&m| m < s).min(each.len());
        let ue = map.partition_point(|&m| m < e).min(each.len());
        each[us..ue.max(us)].fill(tok_color(t, tok));
    }
    let mut out: Vec<(u32, u32)> = Vec::new();
    for (i, &c) in each.iter().enumerate() {
        if out.last().is_none_or(|&(_, k)| k != c) {
            out.push((i as u32, c));
        }
    }
    out
}

impl SegLayout {
    pub fn u16_len(&self) -> u32 {
        (self.map.len() - 1) as u32
    }
    pub fn u16_of(&self, rel: u64) -> u32 {
        self.map.partition_point(|&m| (m as u64) < rel) as u32
    }
    pub fn rel_of(&self, u: u32) -> u64 {
        self.map[(u as usize).min(self.map.len() - 1)] as u64
    }
    pub fn row_of_u16(&self, u: u32) -> usize {
        self.rows.partition_point(|r| r.0 <= u).saturating_sub(1)
    }
    pub fn row_of_rel(&self, rel: u64) -> usize {
        self.row_of_u16(self.u16_of(rel))
    }
    /// Byte range of row `i`, relative to the segment start.
    pub fn row_bytes(&self, i: usize) -> (u64, u64) {
        let (a, b) = self.rows[i];
        (self.rel_of(a), self.rel_of(b))
    }
    /// The part with the text at UTF-16 position `u` (the last one for the end).
    pub fn part_at(&self, u: u32) -> &Part {
        &self.parts[self.parts.partition_point(|p| p.at <= u).max(1) - 1]
    }
    /// The part with row `i`.
    pub fn part_of_row(&self, i: usize) -> &Part {
        &self.parts[self.parts.partition_point(|p| p.row0 <= i).max(1) - 1]
    }
    fn x_of(&self, u: u32, trailing: bool) -> f32 {
        let p = self.part_at(u);
        let (mut x, mut y) = (0f32, 0f32);
        let mut m = DWRITE_HIT_TEST_METRICS::default();
        unsafe {
            let _ = p.layout.HitTestTextPosition(u - p.at, BOOL(trailing as i32), &mut x, &mut y, &mut m);
        }
        p.x + x
    }

    /// Where the text `[us, ue)` of row `i` is: (x, width), from the text's left edge.
    fn range_x(&self, i: usize, us: u32, ue: u32) -> Vec<(f32, f32)> {
        let p = self.part_of_row(i);
        let hit = |m: &mut Vec<DWRITE_HIT_TEST_METRICS>, n: &mut u32| unsafe {
            p.layout.HitTestTextRange(us - p.at, ue - us, 0.0, 0.0, Some(&mut m[..]), n).is_ok()
        };
        let (mut m, mut count) = (vec![DWRITE_HIT_TEST_METRICS::default(); 8], 0u32);
        // (one rectangle for each run of one direction: more than 8 only with mixed right-to-left text)
        let mut ok = hit(&mut m, &mut count);
        if !ok && count as usize > m.len() {
            m.resize(count as usize, DWRITE_HIT_TEST_METRICS::default());
            ok = hit(&mut m, &mut count);
        }
        m.truncate(if ok { (count as usize).min(m.len()) } else { 0 });
        m.iter().map(|m| (p.x + m.left, m.width)).collect()
    }

    /// Each cluster's text position, x from the text's left edge, and width: one call for each layout, where a hit
    /// test per position gets dearer the further into a long line it is.
    fn clusters(&self) -> Vec<(u32, f32, f32)> {
        let mut out = Vec::new();
        for p in &self.parts {
            let mut n = 0u32;
            // (the first call says how many there are)
            unsafe {
                let _ = p.layout.GetClusterMetrics(None, &mut n);
            }
            let mut m = vec![DWRITE_CLUSTER_METRICS::default(); n as usize];
            if n == 0 || unsafe { p.layout.GetClusterMetrics(Some(&mut m), &mut n) }.is_err() {
                continue;
            }
            out.reserve(m.len());
            let (mut pos, mut x, mut row) = (p.at, p.x, p.row0);
            for c in &m[..(n as usize).min(m.len())] {
                // (rows start at the part's x: the text is left-aligned, and only left to right)
                while row + 1 < self.rows.len() && pos >= self.rows[row + 1].0 {
                    row += 1;
                    x = p.x;
                }
                out.push((pos, x, c.width));
                pos += c.length as u32;
                x += c.width;
            }
        }
        out
    }
}

/// Things the view needs that depend on settings (font, zoom, wrapping), shared by all tabs.
pub struct Style {
    pub format_wrap: IDWriteTextFormat,
    pub format_nowrap: IDWriteTextFormat,
    pub row_h: f32,
    /// The font has a bold face of its own (one Windows simulates is wider, so errors aren't drawn in it).
    pub real_bold: bool,
    pub char_w: f32,
    pub digit_w: f32,
    pub wrap: bool,
    pub tab_size: u32,
    pub use_spaces: bool,
    pub line_numbers: bool,
    /// Dots for spaces, arrows for tabs and a mark for each line break (View → Show whitespace).
    pub show_whitespace: bool,
    /// Changes whenever anything that affects layouts changes (font, zoom, theme, render target).
    pub generation: u64,
}

/// Where things are on screen for one paint.
#[derive(Clone, Copy, Debug, Default)]
pub struct Geom {
    pub rect: Rect,
    pub gutter_w: f32,
    pub text_x: f32,
    pub text_w: f32,
}

impl Geom {
    pub fn text_rect(&self) -> Rect {
        Rect::new(self.text_x, self.rect.y, (self.rect.right() - self.text_x).max(0.0), self.rect.h)
    }
    pub fn vbar(&self) -> Rect {
        Rect::new(self.rect.right() - metrics::SCROLLBAR_W, self.rect.y, metrics::SCROLLBAR_W, self.rect.h)
    }
    pub fn hbar(&self) -> Rect {
        Rect::new(
            self.text_x,
            self.rect.bottom() - metrics::SCROLLBAR_W,
            (self.rect.right() - metrics::SCROLLBAR_W - self.text_x).max(0.0),
            metrics::SCROLLBAR_W,
        )
    }
}

pub struct Ctx<'a> {
    pub doc: &'a Document,
    pub g: &'a Gfx,
    pub style: &'a Style,
    pub theme: &'a Theme,
    pub lang: Lang,
    pub geom: Geom,
}

impl Ctx<'_> {
    fn visible_rows(&self) -> usize {
        ((self.geom.rect.h / self.style.row_h).floor() as usize).max(1)
    }
}

#[derive(Clone)]
pub struct VisRow {
    pub seg: Seg,
    pub lay: Rc<SegLayout>,
    pub row: usize,
    pub y: f32,
    pub start: u64,
    pub end: u64,
    /// 1-based line number, on the first row of a line (when known).
    pub line_no: Option<u64>,
}

struct CacheEntry {
    bytes: Vec<u8>,
    layout: Rc<SegLayout>,
    used: u64,
}

#[derive(Default)]
struct TextWindow {
    start: u64,
    data: Vec<u8>,
    version: u64,
    doc_len: u64,
    valid: bool,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum DragMode {
    Chars,
    Words(u64, u64),
    Lines(u64, u64),
    VScroll { grab: i32 },
    HScroll { grab: i32 },
}

pub struct View {
    pub sel: Sel,
    /// The caret sits at a wrap point but belongs at the end of the row above (after End or a click there).
    pub upstream: bool,
    pub want_x: Option<f32>,
    pub top: u64,
    pub scroll_x: f32,
    pub rows: Vec<VisRow>,
    pub drag: Option<DragMode>,
    /// Widest row seen recently (for horizontal scrolling without wrapping).
    pub content_w: f32,
    win: TextWindow,
    cache: HashMap<u64, CacheEntry>,
    /// The text the cached layouts hold, in bytes.
    cache_bytes: usize,
    tick: u64,
    /// `tick` when the rows on screen were last laid out (what they use is never dropped from the cache).
    frame_tick: u64,
    max_top: Option<((u64, u64, u32, usize), u64)>,
    spans: Vec<(u32, u32, Tok)>,
    hl: HlIndex,
    /// Lay out with guessed rather than exact coloring states (only measuring, far from what's on screen).
    hl_guess: bool,
    /// The bracket pair at the caret, for (document version, caret).
    bracket: Option<((u64, u64), Option<(u64, u64)>)>,
    /// The clusters (`SegLayout::clusters`) of the layouts last shown with whitespace marks.
    ws_clusters: Vec<(Rc<SegLayout>, Rc<Vec<(u32, f32, f32)>>)>,
}

impl Default for View {
    fn default() -> Self {
        Self::new()
    }
}

impl View {
    pub fn new() -> View {
        View {
            sel: Sel::default(),
            upstream: false,
            want_x: None,
            top: 0,
            scroll_x: 0.0,
            rows: Vec::new(),
            drag: None,
            content_w: 0.0,
            win: TextWindow::default(),
            cache: HashMap::new(),
            cache_bytes: 0,
            tick: 0,
            frame_tick: 0,
            max_top: None,
            spans: Vec::new(),
            hl: HlIndex::default(),
            hl_guess: false,
            bracket: None,
            ws_clusters: Vec::new(),
        }
    }

    pub fn geometry(doc: &Document, style: &Style, rect: Rect) -> Geom {
        let gutter_w = if style.line_numbers {
            let lines = doc.line_count().unwrap_or_else(|| (doc.len() / 40).max(1));
            let digits = (lines.max(1) as f64).log10().floor() as u32 + 1;
            (digits.max(3) as f32) * style.digit_w + style.digit_w * 2.5
        } else {
            style.digit_w * 0.5
        };
        let text_x = rect.x + gutter_w + 4.0;
        let text_w = (rect.right() - text_x - metrics::SCROLLBAR_W - 4.0).max(style.char_w * 8.0);
        Geom { rect, gutter_w, text_x, text_w }
    }

    /// Forgets cached layouts (font, theme or wrap changed).
    pub fn clear_cache(&mut self) {
        self.cache.clear();
        self.cache_bytes = 0;
        self.ws_clusters.clear();
        self.max_top = None;
    }

    /// Forgets everything worked out from the text (the document was replaced).
    pub fn forget_text(&mut self) {
        self.clear_cache();
        self.hl.reset();
    }

    /// Applies the document's change log to the scroll position.
    pub fn sync(&mut self, doc: &mut Document) {
        for (k, c) in doc.take_changes().iter().enumerate() {
            self.top = c.map(self.top);
            if k >= self.hl.seen {
                self.hl.edited(c.at);
            }
        }
        self.hl.seen = 0;
        self.hl.len = doc.len();
        self.top = self.top.min(doc.len());
        self.sel.anchor = self.sel.anchor.min(doc.len());
        self.sel.caret = self.sel.caret.min(doc.len());
    }

    // ---- segments ----

    fn window(&mut self, doc: &Document, a: u64, b: u64) -> &[u8] {
        let w = &mut self.win;
        let len = doc.len();
        let b = b.min(len);
        let ok = w.valid
            && w.version == doc.version
            && w.doc_len == len
            && a >= w.start
            && b <= w.start + w.data.len() as u64;
        if !ok {
            w.data.clear();
            let end = (b + WINDOW_EXTRA).min(len);
            doc.read_into(a, end, &mut w.data);
            w.start = a;
            w.version = doc.version;
            w.doc_len = len;
            w.valid = true;
        }
        let s = (a - w.start) as usize;
        let e = (b - w.start) as usize;
        &w.data[s..e]
    }

    /// The segment containing `off` (or ending at it).
    pub fn segment_at(&mut self, doc: &Document, off: u64) -> Seg {
        let len = doc.len();
        let off = off.min(len);
        let w0 = off.saturating_sub(2 * SEG);
        let w1 = (off + 2 * SEG + 4).min(len);
        let buf = self.window(doc, w0, w1);
        segment_in(buf, w0, len, off)
    }

    // ---- layouts ----

    /// The lexer state `seg` starts in.
    fn hl_state(&mut self, cx: &Ctx, seg: &Seg) -> HlState {
        if cx.lang == Lang::Plain {
            return HlState::START;
        }
        if cx.doc.is_ready() && cx.doc.len() <= HL_EXACT_MAX && !self.hl_guess {
            return self.hl.state_at(cx.doc, cx.lang, seg.start);
        }
        // Huge documents: each line on its own; a piece of a long line from a few KB before it.
        if seg.line_start {
            return HlState::START;
        }
        let from = seg.start.saturating_sub(PREFIX_MAX);
        let prefix = self.window(cx.doc, from, seg.start).to_vec();
        match memchr::memrchr(b'\n', &prefix) {
            Some(p) => highlight::guess(cx.lang, &prefix[p + 1..], true),
            None => highlight::guess(cx.lang, &prefix, from == 0),
        }
    }

    fn layout_of(&mut self, cx: &Ctx, seg: &Seg) -> Rc<SegLayout> {
        let st = self.hl_state(cx, seg);
        let bytes = self.window(cx.doc, seg.start, seg.end).to_vec();
        let width = if cx.style.wrap { (cx.geom.text_w * 2.0).round() as u64 } else { 0 };
        // (Only a line that is one segment hangs its wrapped rows under its indentation.)
        let whole_line = seg.line_start && seg.line_end;
        let mut h = std::collections::hash_map::DefaultHasher::new();
        bytes.hash(&mut h);
        (width, cx.style.generation, cx.lang as u8, st, whole_line).hash(&mut h);
        let key = h.finish();
        self.tick += 1;
        if let Some(e) = self.cache.get_mut(&key) {
            if e.bytes == bytes {
                e.used = self.tick;
                return e.layout.clone();
            }
        }
        let lay = Rc::new(self.build_layout(cx, &bytes, st, whole_line));
        if !cx.g.has_target() {
            // No render target yet (colors need its brushes): use this layout once, don't keep it.
            return lay;
        }
        if !self.cache.is_empty() && (self.cache.len() >= CACHE_MAX || self.cache_bytes > CACHE_BYTES) {
            // the older half, but nothing the rows on screen use
            let mut ages: Vec<u64> = self.cache.values().map(|e| e.used).collect();
            ages.sort_unstable();
            let cut = ages[ages.len() / 2].min(self.frame_tick);
            self.cache.retain(|_, e| e.used > cut);
            self.cache_bytes = self.cache.values().map(|e| e.bytes.len()).sum();
        }
        self.cache_bytes += bytes.len();
        if let Some(old) = self.cache.insert(key, CacheEntry { bytes, layout: lay.clone(), used: self.tick }) {
            self.cache_bytes -= old.bytes.len();
        }
        lay
    }

    fn build_layout(&mut self, cx: &Ctx, bytes: &[u8], st: HlState, whole_line: bool) -> SegLayout {
        let mut u = Vec::with_capacity(bytes.len());
        let mut map = Vec::with_capacity(bytes.len() + 1);
        text::decode_display(bytes, &mut u, &mut map);
        let (fmt, max_w) = if cx.style.wrap {
            (&cx.style.format_wrap, cx.geom.text_w.max(cx.style.char_w * 8.0))
        } else {
            (&cx.style.format_nowrap, 1.0e7)
        };
        let mut spans = std::mem::take(&mut self.spans);
        spans.clear();
        if cx.lang != Lang::Plain && !bytes.is_empty() && cx.g.has_target() {
            highlight::lex(cx.lang, bytes, st, Some(&mut spans));
        }
        let total = u.len() as u32;
        let layout = cx.g.layout(&u, fmt, max_w, 1.0e7);
        // (Text that may have emoji can end up drawn whole, which needs its colors on the layout.)
        let plain = !u.iter().any(|&c| maybe_color(c));
        color_layout(cx, &layout, &spans, &map, 0, total, !plain);
        let mut rows = layout_rows_of(&layout, 0, total);
        let mut tm = DWRITE_TEXT_METRICS::default();
        unsafe {
            let _ = layout.GetMetrics(&mut tm);
        }
        let mut parts = vec![Part { layout, at: 0, x: 0.0, row0: 0 }];
        // Word wrap: the rows after the first line up under the line's indentation (after a PPCL line number), unless
        // that's more than half the width.
        if cx.style.wrap && whole_line && rows.len() > 1 {
            let numbered = matches!(cx.lang.comment(), Some(CommentStyle::AfterNumber(_)));
            let k = map.partition_point(|&m| (m as usize) < text_start(bytes, numbered)) as u32;
            let r1 = rows[0].1;
            // (where the indentation ends: the start of the text after it is its right end if it's right to left)
            let ind = if k > 0 && k < r1 { parts[0].end_x(k - 1) } else { 0.0 };
            if ind >= 1.0 && ind <= max_w / 2.0 {
                let first = cx.g.layout(&u[..r1 as usize], fmt, max_w, 1.0e7);
                let rest = cx.g.layout(&u[r1 as usize..], fmt, max_w - ind, 1.0e7);
                let first_rows = layout_rows_of(&first, 0, r1);
                if first_rows.len() == 1 {
                    color_layout(cx, &first, &spans, &map, 0, r1, !plain);
                    color_layout(cx, &rest, &spans, &map, r1, total, !plain);
                    rows = first_rows;
                    rows.extend(layout_rows_of(&rest, r1, total));
                    parts = vec![
                        Part { layout: first, at: 0, x: 0.0, row0: 0 },
                        Part { layout: rest, at: r1, x: ind, row0: 1 },
                    ];
                }
            }
        }
        let colors = if plain { colors_of(cx.theme, &spans, &map, total) } else { Vec::new() };
        self.spans = spans;
        SegLayout { parts, map, rows, width: tm.widthIncludingTrailingWhitespace, plain, colors }
    }

    // ---- rows ----

    /// Lays out the rows that fill the view (into `self.rows`).
    pub fn layout_rows(&mut self, cx: &Ctx) {
        self.hl_guess = false;
        self.frame_tick = self.tick;
        self.rows.clear();
        let len = cx.doc.len();
        let row_h = cx.style.row_h;
        let mut seg = self.segment_at(cx.doc, self.top);
        let mut lay = self.layout_of(cx, &seg);
        let mut ri = lay.row_of_rel(self.top - seg.start);
        let mut line = cx.doc.line_of(seg.start);
        let mut y = cx.geom.rect.y;
        let bottom = cx.geom.rect.bottom();
        let mut widest = 0f32;
        loop {
            let (a, b) = lay.row_bytes(ri);
            let line_no = if ri == 0 && seg.line_start { line.map(|l| l + 1) } else { None };
            self.rows.push(VisRow {
                seg,
                lay: lay.clone(),
                row: ri,
                y,
                start: seg.start + a,
                end: seg.start + b,
                line_no,
            });
            widest = widest.max(lay.width);
            y += row_h;
            if y >= bottom {
                break;
            }
            ri += 1;
            if ri >= lay.rows.len() {
                if seg.eol == 0 && seg.end >= len {
                    break;
                }
                if seg.eol > 0 {
                    line = line.map(|l| l + 1);
                }
                seg = self.segment_at(cx.doc, seg.next_start());
                lay = self.layout_of(cx, &seg);
                ri = 0;
            }
        }
        if !cx.style.wrap {
            self.content_w = self.content_w.max(widest);
        }
    }

    /// Start of the visual row containing `off`.
    pub fn row_start(&mut self, cx: &Ctx, off: u64) -> u64 {
        let seg = self.segment_at(cx.doc, off);
        let lay = self.layout_of(cx, &seg);
        let i = lay.row_of_rel(off - seg.start);
        seg.start + lay.row_bytes(i).0
    }

    pub fn next_row(&mut self, cx: &Ctx, row_start: u64) -> Option<u64> {
        let seg = self.segment_at(cx.doc, row_start);
        let lay = self.layout_of(cx, &seg);
        let i = lay.row_of_rel(row_start - seg.start);
        if i + 1 < lay.rows.len() {
            return Some(seg.start + lay.row_bytes(i + 1).0);
        }
        if seg.eol == 0 && seg.end >= cx.doc.len() {
            return None;
        }
        Some(seg.next_start())
    }

    pub fn prev_row(&mut self, cx: &Ctx, row_start: u64) -> Option<u64> {
        let seg = self.segment_at(cx.doc, row_start);
        let lay = self.layout_of(cx, &seg);
        let i = lay.row_of_rel(row_start - seg.start);
        if i > 0 {
            return Some(seg.start + lay.row_bytes(i - 1).0);
        }
        if seg.start == 0 {
            return None;
        }
        let p = self.segment_at(cx.doc, seg.start - 1);
        let pl = self.layout_of(cx, &p);
        Some(p.start + pl.row_bytes(pl.rows.len() - 1).0)
    }

    /// The highest `top` allowed: the last row sits at the bottom of the view.
    pub fn max_top(&mut self, cx: &Ctx) -> u64 {
        let key = (cx.doc.version ^ cx.doc.len().rotate_left(32), cx.style.generation, cx.geom.text_w as u32, cx.visible_rows());
        if let Some((k, v)) = self.max_top {
            if k == key {
                return v;
            }
        }
        let guess = std::mem::replace(&mut self.hl_guess, true);
        let mut r = self.row_start(cx, cx.doc.len());
        for _ in 1..cx.visible_rows() {
            match self.prev_row(cx, r) {
                Some(p) => r = p,
                None => break,
            }
        }
        self.hl_guess = guess;
        self.max_top = Some((key, r));
        r
    }

    pub fn scroll_rows(&mut self, cx: &Ctx, n: i64) {
        let max = self.max_top(cx);
        let mut t = self.row_start(cx, self.top);
        if n > 0 {
            for _ in 0..n {
                if t >= max {
                    break;
                }
                match self.next_row(cx, t) {
                    Some(x) => t = x.min(max),
                    None => break,
                }
            }
        } else {
            for _ in 0..(-n) {
                match self.prev_row(cx, t) {
                    Some(x) => t = x,
                    None => break,
                }
            }
        }
        self.top = t;
    }

    /// Scrolls so the row containing `off` is at the top (clamped).
    pub fn scroll_to_offset(&mut self, cx: &Ctx, off: u64) {
        let max = self.max_top(cx);
        let r = self.row_start(cx, off.min(cx.doc.len()));
        self.top = r.min(max);
    }

    /// Scroll position as a fraction 0..=1 and the visible share of the document (for the scrollbar).
    pub fn scroll_fraction(&mut self, cx: &Ctx) -> (f32, f32) {
        let len = cx.doc.len().max(1);
        let max = self.max_top(cx).max(1);
        let shown = match (self.rows.first(), self.rows.last()) {
            (Some(a), Some(b)) => b.end.saturating_sub(a.start).max(1),
            _ => len,
        };
        ((self.top as f64 / max as f64).min(1.0) as f32, (shown as f64 / len as f64).min(1.0) as f32)
    }

    pub fn set_scroll_fraction(&mut self, cx: &Ctx, f: f32) {
        let max = self.max_top(cx);
        let target = (max as f64 * f.clamp(0.0, 1.0) as f64) as u64;
        let r = self.row_start(cx, target);
        self.top = r.min(max);
    }

    // ---- caret geometry ----

    /// Segment, layout, row and x of a caret position.
    fn caret_place(&mut self, cx: &Ctx, pos: u64, upstream: bool) -> (Seg, Rc<SegLayout>, usize, f32) {
        let seg = self.segment_at(cx.doc, pos);
        let lay = self.layout_of(cx, &seg);
        let u = lay.u16_of(pos - seg.start);
        let row = lay.row_of_u16(u);
        if upstream && u > 0 && lay.rows[row].0 == u {
            if row > 0 {
                return (seg, lay.clone(), row - 1, lay.x_of(u - 1, true));
            }
        }
        if upstream && u == 0 && !seg.line_start && seg.start > 0 {
            let p = self.segment_at(cx.doc, seg.start - 1);
            let pl = self.layout_of(cx, &p);
            let last = pl.rows.len() - 1;
            let x = if pl.u16_len() > 0 { pl.x_of(pl.u16_len() - 1, true) } else { 0.0 };
            return (p, pl, last, x);
        }
        let x = lay.x_of(u, false);
        (seg, lay, row, x)
    }

    /// Caret position in client DIPs (top-left of the caret), if it's on screen.
    pub fn caret_point(&mut self, cx: &Ctx) -> Option<(f32, f32)> {
        let (seg, lay, row, x) = self.caret_place(cx, self.sel.caret, self.upstream);
        let rs = seg.start + lay.row_bytes(row).0;
        let vr = self.rows.iter().find(|r| r.start == rs && r.seg.start == seg.start)?;
        Some((cx.geom.text_x - self.scroll_x + x, vr.y))
    }

    /// The caret's place in client DIPs, if it's on screen: a thin bar, or (`overtype`) as wide as the character it
    /// replaces.
    pub fn caret_rect(&mut self, cx: &Ctx, overtype: bool) -> Option<Rect> {
        let (x, y) = self.caret_point(cx)?;
        let w = if overtype { self.cell_width(cx, self.sel.caret) } else { 2.0 };
        Some(Rect::new(x, y, w, cx.style.row_h))
    }

    /// How wide the character at `pos` shows (a space's width at the end of a line).
    fn cell_width(&mut self, cx: &Ctx, pos: u64) -> f32 {
        let seg = self.segment_at(cx.doc, pos);
        let next = next_cluster(cx.doc, pos);
        if pos < seg.end && next <= seg.end {
            let lay = self.layout_of(cx, &seg);
            let (u0, u1) = (lay.u16_of(pos - seg.start), lay.u16_of(next - seg.start));
            let w = lay.range_x(lay.row_of_u16(u0), u0, u1.max(u0)).first().map_or(0.0, |r| r.1);
            if w > 0.5 {
                return w;
            }
        }
        cx.style.char_w
    }

    /// The bracket pair at the caret (`matching_bracket`), worked out once per caret place and text.
    fn brackets_at(&mut self, doc: &Document, caret: u64) -> Option<(u64, u64)> {
        let key = (doc.version, caret);
        if let Some((k, v)) = self.bracket {
            if k == key {
                return v;
            }
        }
        let v = matching_bracket(doc, caret);
        self.bracket = Some((key, v));
        v
    }

    /// Position under a point (client DIPs); also whether it belongs to the end of the row above.
    pub fn pos_at(&mut self, cx: &Ctx, x: f32, y: f32) -> (u64, bool) {
        if self.rows.is_empty() {
            self.layout_rows(cx);
        }
        let row_h = cx.style.row_h;
        let vr = match self.rows.iter().position(|r| y < r.y + row_h) {
            Some(i) => self.rows[i].clone(),
            None => self.rows.last().cloned().unwrap(),
        };
        if y < cx.geom.rect.y && vr.start == self.top {
            if let Some(p) = self.prev_row(cx, vr.start) {
                return self.pos_in_row(cx, p, x - cx.geom.text_x + self.scroll_x);
            }
        }
        self.pos_in_row(cx, vr.start, x - cx.geom.text_x + self.scroll_x)
    }

    /// Position in the row starting at `row_start` closest to layout x.
    fn pos_in_row(&mut self, cx: &Ctx, row_start: u64, lx: f32) -> (u64, bool) {
        let seg = self.segment_at(cx.doc, row_start);
        let lay = self.layout_of(cx, &seg);
        let ri = lay.row_of_rel(row_start - seg.start);
        let (ra, rb) = lay.rows[ri];
        let p = lay.part_of_row(ri);
        let ly = (ri - p.row0) as f32 * cx.style.row_h + cx.style.row_h / 2.0;
        let mut trailing = BOOL(0);
        let mut inside = BOOL(0);
        let mut m = DWRITE_HIT_TEST_METRICS::default();
        unsafe {
            let _ = p.layout.HitTestPoint((lx - p.x).max(0.0), ly, &mut trailing, &mut inside, &mut m);
        }
        let mut u = p.at + m.textPosition + if trailing.as_bool() { m.length } else { 0 };
        u = u.max(ra).min(rb.max(ra));
        let last_row = ri + 1 == lay.rows.len();
        // At the end of a wrapped row the position is the next row's start; keep the caret on this row.
        let upstream = u == rb && rb > ra && (!last_row || !seg.line_end);
        (seg.start + lay.rel_of(u), upstream)
    }

    /// Makes `pos` visible, scrolling as little as possible (or centering it).
    pub fn reveal(&mut self, cx: &Ctx, pos: u64, center: bool) {
        let (seg, lay, row, x) = self.caret_place(cx, pos, self.upstream);
        let rs = seg.start + lay.row_bytes(row).0;
        let n = cx.visible_rows();
        let mut visible = false;
        // (Before the top it can't be visible: no need to lay out the old view, which may be far away.)
        if rs >= self.top {
            let mut r = self.row_start(cx, self.top);
            for _ in 0..n {
                if r == rs {
                    visible = true;
                    break;
                }
                match self.next_row(cx, r) {
                    Some(x) => r = x,
                    None => break,
                }
            }
        }
        if !visible {
            let back = if center { n / 2 } else if rs < self.top { 0 } else { n.saturating_sub(1) };
            let mut t = rs;
            for _ in 0..back {
                match self.prev_row(cx, t) {
                    Some(p) => t = p,
                    None => break,
                }
            }
            let max = self.max_top(cx);
            self.top = t.min(max);
        }
        if !cx.style.wrap {
            let margin = cx.style.char_w * 4.0;
            let w = cx.geom.text_w;
            if x < self.scroll_x + margin {
                self.scroll_x = (x - margin * 2.0).max(0.0);
            } else if x > self.scroll_x + w - margin {
                self.scroll_x = x - w + margin * 2.0;
            }
        } else {
            self.scroll_x = 0.0;
        }
    }

    // ---- caret movement ----

    pub fn set_caret(&mut self, pos: u64, extend: bool) {
        self.sel.caret = pos;
        if !extend {
            self.sel.anchor = pos;
        }
        self.upstream = false;
    }

    /// Up/down by `n` rows (negative = up), keeping the column.
    pub fn move_rows(&mut self, cx: &Ctx, n: i64, extend: bool) {
        let (seg, lay, row, x) = self.caret_place(cx, self.sel.caret, self.upstream);
        let want = self.want_x.unwrap_or(x);
        let mut rs = seg.start + lay.row_bytes(row).0;
        let mut hit_edge = false;
        for _ in 0..n.unsigned_abs() {
            let next = if n > 0 { self.next_row(cx, rs) } else { self.prev_row(cx, rs) };
            match next {
                Some(r) => rs = r,
                None => {
                    hit_edge = true;
                    break;
                }
            }
        }
        if hit_edge && n.unsigned_abs() == 1 {
            let pos = if n > 0 { cx.doc.len() } else { 0 };
            self.set_caret(pos, extend);
            self.want_x = None;
            return;
        }
        let (pos, up) = self.pos_in_row(cx, rs, want);
        self.set_caret(pos, extend);
        self.upstream = up;
        self.want_x = Some(want);
    }

    pub fn page(&mut self, cx: &Ctx, down: bool, extend: bool) {
        let n = cx.visible_rows().saturating_sub(1).max(1) as i64;
        let n = if down { n } else { -n };
        self.scroll_rows(cx, n);
        self.move_rows(cx, n, extend);
    }

    pub fn home(&mut self, cx: &Ctx, extend: bool) {
        let caret = self.sel.caret;
        let (seg, lay, row, _) = self.caret_place(cx, caret, self.upstream);
        let rs = seg.start + lay.row_bytes(row).0;
        // (A row the line wrapped into starts after the line: no need to look for where that is, which in a huge line
        // whose lines are still being counted means reading back to it.)
        let first_row = row == 0 && seg.line_start;
        if !first_row && caret != rs {
            self.set_caret(rs, extend);
        } else {
            let ls = if first_row { rs } else { cx.doc.line_start_of(caret) };
            // (the indentation ends at the first other character, a line break too)
            let head = cx.doc.read(ls, (ls + 4096).min(cx.doc.len()));
            let ind = head.iter().take_while(|&&b| b == b' ' || b == b'\t').count() as u64;
            let first = ls + ind;
            self.set_caret(if caret == first { ls } else { first }, extend);
        }
        self.want_x = None;
    }

    pub fn end(&mut self, cx: &Ctx, extend: bool) {
        let caret = self.sel.caret;
        let (seg, lay, row, _) = self.caret_place(cx, caret, self.upstream);
        let (_, rb) = lay.row_bytes(row);
        let re = seg.start + rb;
        let last_of_line = row + 1 == lay.rows.len() && seg.line_end;
        if !last_of_line && !(caret == re && self.upstream) {
            self.set_caret(re, extend);
            self.upstream = true;
        } else {
            self.set_caret(cx.doc.line_end_of(caret), extend);
        }
        self.want_x = None;
    }

    // ---- painting ----

    /// Paints the rows from the last `layout_rows` (call it first). `overtype`: the caret marks the character that
    /// typing replaces.
    pub fn paint(&mut self, cx: &Ctx, focused: bool, caret_on: bool, matches: &[(u64, u64)], overtype: bool) {
        let g = cx.g;
        let t = cx.theme;
        let geom = cx.geom;
        let row_h = cx.style.row_h;
        g.fill(geom.rect, t.surface);
        if self.rows.is_empty() {
            self.layout_rows(cx);
        }
        let sel = self.sel;
        let caret_line_no = if sel.is_empty() { cx.doc.line_of(sel.caret) } else { None };
        let caret_place = self.caret_place(cx, sel.caret, self.upstream);
        let caret_row_start = caret_place.0.start + caret_place.1.row_bytes(caret_place.2).0;

        // Line numbers.
        if cx.style.line_numbers {
            let gutter = Rect::new(geom.rect.x, geom.rect.y, geom.gutter_w, geom.rect.h);
            g.push_clip(gutter);
            let fmt = &cx.style.format_nowrap;
            let mut current_line: Option<u64> = None;
            for r in &self.rows {
                if let Some(n) = r.line_no {
                    current_line = Some(n);
                    let active = caret_line_no.is_some_and(|c| c + 1 == n);
                    let s = n.to_string();
                    let w = s.len() as f32 * cx.style.digit_w;
                    let x = geom.rect.x + geom.gutter_w - cx.style.digit_w * 1.25 - w;
                    let l = g.layout(&super::gfx::wide(&s), fmt, 1000.0, row_h);
                    g.draw_layout(&l, x, r.y, if active { t.gutter_active } else { t.gutter });
                }
            }
            let _ = current_line;
            g.pop_clip();
        }

        let text = geom.text_rect();
        let clip = Rect::new(text.x - 2.0, text.y, text.w + 2.0, text.h);
        g.push_clip(clip);
        let ox = geom.text_x - self.scroll_x;

        // Current row highlight.
        if sel.is_empty() && focused {
            if let Some(r) = self.rows.iter().find(|r| r.start == caret_row_start) {
                g.fill(Rect::new(clip.x, r.y, clip.w, row_h), t.current_line);
            }
        }

        // Search matches, then the selection on top (unless the selection is the current match).
        let mut sel_is_match = false;
        for &(ms, me) in matches {
            let current = ms == sel.start() && me == sel.end();
            sel_is_match |= current;
            self.fill_range(cx, ms, me, ox, if current { t.match_current } else { t.match_bg }, false);
        }
        if !sel.is_empty() && !sel_is_match {
            let c = if focused { t.selection } else { t.selection_inactive };
            self.fill_range(cx, sel.start(), sel.end(), ox, c, true);
        }

        // The bracket next to the caret and the one it pairs with.
        if sel.is_empty() {
            if let Some((a, b)) = self.brackets_at(cx.doc, sel.caret) {
                for p in [a, b] {
                    for (rc, _) in self.range_rects(cx, p, p + 1, ox, false) {
                        g.fill(rc, t.bracket_bg);
                        g.stroke_round(rc, 2.0, t.bracket_border, 1.0);
                    }
                }
            }
        }

        // Text: each segment's layouts once, positioned by its first visible row (and of a long one only what's in view).
        let mut i = 0;
        while i < self.rows.len() {
            let r = &self.rows[i];
            let y0 = r.y - r.row as f32 * row_h;
            for p in &r.lay.parts {
                let ink = Ink { color_glyphs: !r.lay.plain, colors: &r.lay.colors, at: p.at };
                g.draw_layout_in(&p.layout, ox + p.x, y0 + p.row0 as f32 * row_h, t.text, clip, ink);
            }
            let s = r.seg.start;
            while i < self.rows.len() && self.rows[i].seg.start == s {
                i += 1;
            }
        }

        // High contrast: selected text in the highlight's own text color (where the selection has the highlight
        // color), drawn again over the rest.
        if t.hc && !sel.is_empty() && (focused || sel_is_match) {
            for (rc, k) in self.range_rects(cx, sel.start(), sel.end(), ox, false) {
                let r = &self.rows[k];
                let p = r.lay.part_of_row(r.row);
                let (x, y) = (ox + p.x, r.y - (r.row - p.row0) as f32 * row_h);
                g.push_clip(rc);
                let ink = Ink { color_glyphs: !r.lay.plain, ..Default::default() };
                g.draw_layout_in(&p.layout, x, y, t.selection_text, rc, ink);
                g.pop_clip();
            }
        }

        if cx.style.show_whitespace {
            self.paint_whitespace(cx, ox);
        } else if !self.ws_clusters.is_empty() {
            self.ws_clusters.clear();
        }

        // Caret: a thin bar, or a bar under the character typing replaces.
        if focused && caret_on {
            if let Some(r) = self.rows.iter().find(|r| r.start == caret_row_start && r.seg.start == caret_place.0.start) {
                let x = ox + caret_place.3;
                let y = r.y;
                if overtype {
                    let w = self.cell_width(cx, sel.caret);
                    let h = (row_h * 0.14).max(2.0);
                    g.fill(Rect::new(x, y + row_h - h, w, h), t.caret);
                } else {
                    let w = (1.5f32 * g.dpi / 96.0).round() / (g.dpi / 96.0);
                    g.fill(Rect::new(x.round() - 0.5, y, w.max(1.0), row_h), t.caret);
                }
            }
        }
        g.pop_clip();
    }

    /// Fills the area of `[a, b)` on the visible rows (selection, matches). `eol` also marks selected line breaks.
    fn fill_range(&self, cx: &Ctx, a: u64, b: u64, ox: f32, color: u32, eol: bool) {
        for (rc, _) in self.range_rects(cx, a, b, ox, eol) {
            cx.g.fill(rc, color);
        }
    }

    /// Where `[a, b)` is on the visible rows (client DIPs), with the index of the row of each part. `eol` adds a
    /// small block after a row whose line break is in the range.
    fn range_rects(&self, cx: &Ctx, a: u64, b: u64, ox: f32, eol: bool) -> Vec<(Rect, usize)> {
        let row_h = cx.style.row_h;
        let mut out = Vec::new();
        for (k, r) in self.rows.iter().enumerate() {
            if b < r.start || a > r.end {
                continue;
            }
            let s = a.max(r.start);
            let e = b.min(r.end);
            if s < e {
                let us = r.lay.u16_of(s - r.seg.start);
                let ue = r.lay.u16_of(e - r.seg.start);
                for (x, w) in r.lay.range_x(r.row, us, ue) {
                    out.push((Rect::new(ox + x, r.y, w.max(1.0), row_h), k));
                }
            }
            // A line break in the range shows as a small block after the row.
            let last_row_of_line = r.row + 1 == r.lay.rows.len() && r.seg.line_end && r.seg.eol > 0;
            if eol && last_row_of_line && a <= r.end && b > r.end {
                let x = ox + r.lay.x_of(r.lay.rows[r.row].1, false);
                out.push((Rect::new(x, r.y, cx.style.char_w * 0.6, row_h), k));
            }
        }
        out
    }

    /// Faint dots for spaces, arrows for tabs, and after each line a mark for its line break (↵ for CRLF, ↓ for
    /// LF), on what's on screen. Without word wrap a row is a whole segment (up to 8 KiB, mostly off to the side): only
    /// the part in view is looked at, with x positions from each layout's clusters (kept while it stays on screen).
    fn paint_whitespace(&mut self, cx: &Ctx, ox: f32) {
        let g = cx.g;
        let color = cx.theme.whitespace;
        let row_h = cx.style.row_h;
        let dot = (cx.style.char_w * 0.16).clamp(1.5, 3.0);
        let fmt = &cx.style.format_nowrap;
        let crlf = g.layout(&super::gfx::wide("\u{21B5}"), fmt, 200.0, row_h);
        let lf = g.layout(&super::gfx::wide("\u{2193}"), fmt, 200.0, row_h);
        // in layout x
        let pad = cx.style.char_w * 2.0;
        let (vx0, vx1) = (self.scroll_x - pad, self.scroll_x + cx.geom.text_w + pad);
        let before = std::mem::take(&mut self.ws_clusters);
        let mut shown: Vec<(Rc<SegLayout>, Rc<Vec<(u32, f32, f32)>>)> = Vec::new();
        for k in 0..self.rows.len() {
            let r = self.rows[k].clone();
            let found = shown.iter().rev().chain(before.iter()).find(|(l, _)| Rc::ptr_eq(l, &r.lay)).map(|(_, c)| c.clone());
            let all = match found {
                Some(c) => c,
                None => Rc::new(r.lay.clusters()),
            };
            if !shown.iter().any(|(l, _)| Rc::ptr_eq(l, &r.lay)) {
                shown.push((r.lay.clone(), all.clone()));
            }
            // this row's clusters, then those in view
            let (ra, rb) = r.lay.rows[r.row];
            let row = &all[all.partition_point(|c| c.0 < ra)..all.partition_point(|c| c.0 < rb)];
            let seen = &row[row.partition_point(|c| c.1 + c.2 < vx0)..row.partition_point(|c| c.1 <= vx1)];
            let mid = r.y + row_h / 2.0;
            if let (Some(first), Some(last)) = (seen.first(), seen.last()) {
                let a = r.seg.start + r.lay.rel_of(first.0);
                let b = r.seg.start + r.lay.rel_of(last.0) + 1;
                let bytes = self.window(cx.doc, a, b).to_vec();
                for &(pos, x, w) in seen {
                    let x0 = ox + x;
                    match bytes.get((r.seg.start + r.lay.rel_of(pos) - a) as usize) {
                        Some(b' ') => {
                            let x = x0 + w / 2.0;
                            g.fill_round(Rect::new(x - dot / 2.0, mid - dot / 2.0, dot, dot), dot / 2.0, color);
                        }
                        Some(b'\t') if w > 4.0 => {
                            let (a, b) = (x0 + 2.0, x0 + w - 2.0);
                            let head = (row_h * 0.18).min((b - a) / 2.0);
                            g.line(a, mid, b, mid, color, 1.0);
                            g.line(b - head, mid - head, b, mid, color, 1.0);
                            g.line(b - head, mid + head, b, mid, color, 1.0);
                        }
                        _ => {}
                    }
                }
            }
            if r.row + 1 == r.lay.rows.len() && r.seg.line_end && r.seg.eol > 0 {
                let x = row.last().map_or(0.0, |c| c.1 + c.2);
                if x >= vx0 && x <= vx1 {
                    g.draw_layout(if r.seg.eol == 2 { &crlf } else { &lf }, ox + x + 1.0, r.y, color);
                }
            }
        }
        self.ws_clusters = shown;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A view on `text` 300 DIPs wide (Consolas when the default font isn't there), with word wrap.
    fn view_on(text: &[u8], lang: Lang, f: impl FnOnce(&mut View, &Ctx)) {
        view_wrapped(text, lang, true, f)
    }

    fn view_wrapped(text: &[u8], lang: Lang, wrap: bool, f: impl FnOnce(&mut View, &Ctx)) {
        let mut g = Gfx::new().unwrap();
        // (layouts are kept only with a render target)
        let _bitmap = crate::ui::gfx::tests::bitmap_target(&mut g, 8, 8);
        let settings = crate::ui::settings::Settings { wrap, ..Default::default() };
        let style = crate::ui::app::make_style(&g, &settings, 1);
        let theme = Theme::light(0xFF0078D4);
        let doc = Document::from_text(text);
        let geom = View::geometry(&doc, &style, Rect::new(0.0, 0.0, 300.0, 400.0));
        let cx = Ctx { doc: &doc, g: &g, style: &style, theme: &theme, lang, geom };
        f(&mut View::new(), &cx);
    }

    #[test]
    fn wrapped_rows_hang_under_the_indentation() {
        let line = b"    let x = aaaa(bbbb, cccc, dddd, eeee, ffff, gggg, hhhh, iiii, jjjj, kkkk, llll, mmmm, nnnn);";
        let mut text = line.to_vec();
        text.extend_from_slice(b"\n\tx = 1\n00100     SET(1, \"A LONG POINT NAME\", \"ANOTHER POINT\", \"AND ANOTHER ONE\")\n");
        view_on(&text, Lang::Plain, |v, cx| {
            let seg = v.segment_at(cx.doc, 0);
            let lay = v.layout_of(cx, &seg);
            assert!(lay.rows.len() >= 3, "it wraps: {:?}", lay.rows);
            assert_eq!(lay.parts.len(), 2);
            // every row after the first starts where the text of the first does, after its 4 spaces
            let ind = lay.x_of(4, false);
            assert!(ind > 10.0);
            for r in &lay.rows[1..] {
                assert!((lay.x_of(r.0, false) - ind).abs() < 0.01, "row {r:?}");
            }
            // every position, from where it is on its row, hit-tests back to itself
            for pos in 0..=line.len() as u64 {
                for up in [false, true] {
                    let (seg, lay, row, x) = v.caret_place(cx, pos, up);
                    let rs = seg.start + lay.row_bytes(row).0;
                    let (back, back_up) = v.pos_in_row(cx, rs, x + 0.25);
                    assert_eq!(back, pos, "{pos} {up}");
                    if back_up {
                        assert_eq!(v.caret_place(cx, back, true).2, row);
                    }
                }
            }
            // a short line doesn't wrap: one layout
            let seg = v.segment_at(cx.doc, line.len() as u64 + 1);
            assert_eq!(v.layout_of(cx, &seg).parts.len(), 1);
        });
        // a line whose text starts right to left: under its start all the same (not under that word's right end)
        let hebrew = "    \u{5E9}\u{5DC}\u{5D5}\u{5DD} \u{5E2}\u{5D5}\u{5DC}\u{5DD} and then more words that go on to wrap the line";
        view_on(hebrew.as_bytes(), Lang::Plain, |v, cx| {
            let seg = v.segment_at(cx.doc, 0);
            let lay = v.layout_of(cx, &seg);
            assert_eq!(lay.parts.len(), 2);
            assert!((lay.parts[1].x - lay.parts[0].end_x(3)).abs() < 0.01 && lay.parts[1].x < cx.style.char_w * 6.0);
        });
        // PPCL: under the statement, after the line number
        view_on(&text, Lang::Ppcl, |v, cx| {
            let at = text.windows(5).position(|w| w == b"00100").unwrap() as u64;
            let seg = v.segment_at(cx.doc, at);
            let lay = v.layout_of(cx, &seg);
            assert!(lay.rows.len() >= 2);
            let set = lay.x_of(10, false);
            assert!((lay.x_of(lay.rows[1].0, false) - set).abs() < 0.01);
        });
    }

    #[test]
    fn every_caret_place_hit_tests_back_to_itself() {
        // tabs, kanji (another font), emoji, a letter with its accent, and a line long enough to be cut in segments
        let mut text = "\tab\tc 日本語の text 👍🏽 and e\u{301}x, \t \u{1F468}\u{200D}\u{1F469} end\n".repeat(3).into_bytes();
        text.extend("word 日本 ".repeat(1200).as_bytes());
        text.push(b'\n');
        for wrap in [true, false] {
            view_wrapped(&text, Lang::Plain, wrap, |v, cx| {
                let mut pos = 0;
                while pos < text.len() as u64 {
                    for up in [false, true] {
                        let (seg, lay, row, x) = v.caret_place(cx, pos, up);
                        let rs = seg.start + lay.row_bytes(row).0;
                        let (back, _) = v.pos_in_row(cx, rs, x + 0.25);
                        assert_eq!(back, pos, "{pos} {up} {wrap}");
                    }
                    pos = next_cluster(cx.doc, pos);
                }
            });
        }
    }

    #[test]
    fn home_goes_to_the_row_then_the_text_then_the_line() {
        let line = b"    let x = aaaa(bbbb, cccc, dddd, eeee, ffff, gggg, hhhh, iiii, jjjj, kkkk, llll, mmmm, nnnn);";
        let mut text = b"\n".to_vec();
        text.extend_from_slice(line);
        view_on(&text, Lang::Plain, |v, cx| {
            let end = text.len() as u64;
            let row = v.row_start(cx, end);
            assert!(row > 10, "it wraps");
            v.set_caret(end - 2, false);
            let mut stops = Vec::new();
            for _ in 0..4 {
                v.home(cx, false);
                stops.push(v.sel.caret);
            }
            assert_eq!(stops, [row, 5, 1, 5]);
        });
    }

    #[test]
    fn emoji_are_drawn_whole() {
        let plain = |s: &str| !s.encode_utf16().any(maybe_color);
        assert!(plain("let x = \"日本語\"; // naïve café ␍"));
        assert!(!plain("ok 👍"));
        assert!(!plain("❤️"));
        assert!(!plain("☀"));
    }
}
