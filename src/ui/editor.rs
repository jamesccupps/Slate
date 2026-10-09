//! The text view: turns a document into rows on screen and back (painting, hit testing, caret movement,
//! scrolling), plus the text-editing operations.
//!
//! Display model: the document is cut into *segments* — a whole line, or for lines longer than a few KB, pieces
//! cut at fixed 8 KiB grid points (`segment_at`). Each segment gets one cached DirectWrite layout, which may wrap
//! into several *rows*. Nothing ever lays out more than what's on screen, so a single 800 MB line scrolls as
//! smoothly as a small file. The scroll position `top` is the byte offset where the first visible row starts;
//! the scrollbar maps bytes, not rows, so it never needs the whole file laid out.

use std::collections::{BTreeMap, HashMap};
use std::hash::{Hash, Hasher};
use std::rc::Rc;

use windows::Win32::Foundation::BOOL;
use windows::Win32::Graphics::DirectWrite::{
    DWRITE_CLUSTER_METRICS, DWRITE_FONT_STYLE_ITALIC, DWRITE_FONT_WEIGHT_BOLD, DWRITE_HIT_TEST_METRICS, DWRITE_LINE_METRICS, DWRITE_TEXT_METRICS, DWRITE_TEXT_RANGE, IDWriteTextFormat,
    IDWriteTextLayout,
};

use crate::core::document::{Document, EditKind, Sel};
use crate::core::text::{self, is_continuation, utf8_len};

use super::gfx::{Gfx, Ink, Rect};
use super::highlight::{self, CommentStyle, Lang, Span, State as HlState, Tok};
use super::theme::{Theme, metrics};

pub const SEG: u64 = 8192;
const HALF: u64 = SEG / 2;
const WINDOW_EXTRA: u64 = 64 * 1024;
const PREFIX_MAX: u64 = 4096;
/// How far before a grid point a long line may be cut at a space or comma instead.
const CUT_BACK: u64 = 256;
/// Layouts kept of segments shown recently: at most this many...
const CACHE_MAX: usize = 4000;
/// ...and this much of their text: a colored 8 KiB segment's layout takes half a MB, so the count alone let a long line
/// scrolled through without word wrap fill gigabytes.
const CACHE_BYTES: usize = 1 << 20;
/// Documents up to this size get exact coloring of what spans lines (block comments, multi-line strings, tags...).
const HL_EXACT_MAX: u64 = 32 << 20;
/// How often lexer states are kept along the document.
const HL_CHECK: u64 = 16 << 10;
/// Line-based operations (indent, move lines...) refuse selections covering more lines than this.
pub const MAX_LINE_OPS: u64 = 200_000;
/// Duplicating and moving lines copy them: they refuse more text than this (a 300 MB line would otherwise be
/// copied into memory, and running out of it ends the program).
pub const COPY_MAX: u64 = 16 << 20;
/// Indenting rewrites the lines as one replacement up to this much text (bigger: line by line, in place).
const INDENT_AT_ONCE_MAX: u64 = 64 << 20;
pub const TOO_MANY_LINES: &str = "Too many lines selected for that.";
pub const TOO_MUCH_TEXT: &str = "That's too much text for this (more than 16 MB).";

/// What Tab and the automatic indentation insert: a tab character, or this many spaces per level.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Indent {
    Tabs,
    Spaces(u32),
}

impl Indent {
    /// The text of one level.
    pub fn unit(self) -> Vec<u8> {
        match self {
            Indent::Tabs => b"\t".to_vec(),
            Indent::Spaces(n) => vec![b' '; n.max(1) as usize],
        }
    }
}

/// How a text is indented, judged from (up to) its first 10,000 lines: by tabs, or by spaces in steps of how many
/// columns (the most common step between neighbouring lines). None when it has no indented lines, or as many of
/// each kind.
pub fn detect_indent(text: &[u8]) -> Option<Indent> {
    let (mut tabs, mut spaces) = (0u32, 0u32);
    let mut steps = [0u32; 9];
    // (The start of the text counts as a line at column 0.)
    let mut prev: Option<usize> = Some(0);
    for line in text.split(|&b| b == b'\n').take(10_000) {
        let line = line.strip_suffix(b"\r").unwrap_or(line);
        let ws = line.iter().take_while(|&&b| b == b' ' || b == b'\t').count();
        if ws == line.len() {
            continue; // blank
        }
        if line[0] == b'\t' {
            tabs += 1;
            prev = None;
            continue;
        }
        if line[..ws].contains(&b'\t') {
            prev = None;
            continue;
        }
        if ws > 1 {
            spaces += 1;
        }
        if let Some(p) = prev {
            // (A step of one is alignment, like the " * " of a block comment, not a level.)
            let d = ws.abs_diff(p);
            if (2..=8).contains(&d) {
                steps[d] += 1;
            }
        }
        prev = Some(ws);
    }
    if tabs > spaces {
        return Some(Indent::Tabs);
    }
    if spaces > tabs {
        // the most common step; on a tie the smaller one
        let (w, n) = (2..=8).map(|d| (d, steps[d])).fold((0, 0), |best, x| if x.1 > best.1 { x } else { best });
        return (n > 0).then_some(Indent::Spaces(w as u32));
    }
    None
}

/// A display segment: `[start, end)` is text without the line break; `eol` is the length of the line break after
/// `end` (0 when the segment continues the line, or at the end of the document).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Seg {
    pub start: u64,
    pub end: u64,
    pub line_start: bool,
    pub line_end: bool,
    pub eol: u8,
}

impl Seg {
    pub fn next_start(&self) -> u64 {
        self.end + self.eol as u64
    }
}

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
    /// x of the start of the character at `u` (counted from the part's start), from the part's left edge.
    fn layout_x(&self, u: u32) -> f32 {
        let (mut x, mut y) = (0f32, 0f32);
        let mut m = DWRITE_HIT_TEST_METRICS::default();
        unsafe {
            let _ = self.layout.HitTestTextPosition(u, BOOL(0), &mut x, &mut y, &mut m);
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
        // (an error bold too: it mustn't pass for a string, PPCL's values, in any theme)
        let bold = matches!(tok, Tok::Bold | Tok::Heading | Tok::Error);
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

/// Lexer states at checkpoints through the document (and at recent segment starts, always worked out from the
/// checkpoint before them), so coloring knows what a segment starts inside. An edit drops what comes after it; it
/// is worked out again when needed.
#[derive(Default)]
struct HlIndex {
    lang: Option<Lang>,
    len: u64,
    /// How many of the document's pending changes are already applied.
    seen: usize,
    checkpoints: Vec<(u64, HlState)>,
    memo: BTreeMap<u64, HlState>,
    buf: Vec<u8>,
}

impl HlIndex {
    fn reset(&mut self) {
        self.lang = None;
        self.seen = 0;
        self.checkpoints.clear();
        self.memo.clear();
    }

    /// The text changed at `at`: states after it aren't known any more.
    fn edited(&mut self, at: u64) {
        let k = self.checkpoints.partition_point(|c| c.0 <= at);
        self.checkpoints.truncate(k);
        let _ = self.memo.split_off(&(at + 1));
    }

    fn state_at(&mut self, doc: &Document, lang: Lang, off: u64) -> HlState {
        let pending = doc.pending_changes();
        if pending.len() > self.seen {
            for c in &pending[self.seen..] {
                self.edited(c.at);
            }
            self.seen = pending.len();
            self.len = doc.len();
        }
        if self.lang != Some(lang) || self.len != doc.len() || self.checkpoints.is_empty() {
            self.reset();
            self.lang = Some(lang);
            self.len = doc.len();
            self.checkpoints.push((0, HlState::START));
        }
        if let Some(s) = self.memo.get(&off) {
            return *s;
        }
        // Start from the checkpoint before `off`, never from a remembered segment start: a long line's segments are
        // cut wherever the grid says, and a lexer that looks ahead (to a line's end, say) can be off at such a cut;
        // starting from it would carry that into everything after it.
        let k = self.checkpoints.partition_point(|c| c.0 <= off) - 1;
        let (mut pos, mut st) = self.checkpoints[k];
        if off - pos > HL_CHECK && k + 1 == self.checkpoints.len() {
            // Far from anything known: keep checkpoints on the way, at line starts where possible.
            let (mut cp, mut cs) = self.checkpoints[k];
            while off - cp > HL_CHECK {
                self.buf.clear();
                doc.read_into(cp, cp + HL_CHECK, &mut self.buf);
                // after a line break, or (in a very long line) after a space, comma, ';' or '>' near the end, where
                // no token like "*/" or "-->" can be cut in two
                let cut = match memchr::memrchr(b'\n', &self.buf) {
                    Some(p) => p + 1,
                    None => self.buf.iter().rev().take(512).position(|&b| matches!(b, b' ' | b',' | b';' | b'>')).map_or(self.buf.len(), |p| self.buf.len() - p),
                };
                cs = highlight::lex(lang, &self.buf[..cut], cs, None);
                cp += cut as u64;
                self.checkpoints.push((cp, cs));
            }
            (pos, st) = (cp, cs);
        }
        if off > pos {
            self.buf.clear();
            doc.read_into(pos, off, &mut self.buf);
            st = highlight::lex(lang, &self.buf, st, None);
        }
        if self.memo.len() > 4096 {
            self.memo.clear();
        }
        self.memo.insert(off, st);
        st
    }
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

/// Text to insert for a newline in `doc`.
fn eol(doc: &Document) -> &'static [u8] {
    doc.eol.as_bytes()
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
            let ind = if k > 0 && k < r1 { parts[0].layout_x(k) } else { 0.0 };
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

/// The segment containing `off`, given `buf` = document bytes `[w0, w0 + buf.len())` covering at least
/// `[off - 2*SEG, off + 2*SEG + 4)` (clipped to the document).
pub fn segment_in(buf: &[u8], w0: u64, len: u64, off: u64) -> Seg {
    let w1 = w0 + buf.len() as u64;
    let at = |p: u64| buf[(p - w0) as usize];
    // A grid point is a boundary if its line started at least HALF bytes earlier and doesn't end right there.
    let valid = |g: u64| -> bool {
        if g == 0 || g >= len || g < w0 + HALF {
            return false;
        }
        let a = (g - HALF - w0) as usize;
        let b = ((g + 2).min(w1) - w0) as usize;
        memchr::memchr(b'\n', &buf[a..b]).is_none()
    };
    let snap = |g: u64| -> u64 {
        if g >= w1 || !is_continuation(at(g)) {
            return g;
        }
        for k in 1..=3u64 {
            if g < w0 + k {
                break;
            }
            let b = at(g - k);
            if !is_continuation(b) {
                return if utf8_len(b) as u64 > k { g - k } else { g };
            }
        }
        g
    };
    // Where a grid point actually cuts: just after the last space, comma or `}` shortly before it, so the cut looks
    // like an ordinary wrap; otherwise at the nearest character boundary. (Not after `]`: it would split the `]]>`
    // ending XML's CDATA, or Lua's `]]`.)
    let cut = |g: u64| -> u64 {
        let lo = g.saturating_sub(CUT_BACK).max(w0);
        let (a, b) = ((lo - w0) as usize, (g.min(w1) - w0) as usize);
        match buf[a..b].iter().rposition(|&c| matches!(c, b' ' | b'\t' | b',' | b'}')) {
            Some(i) => lo + i as u64 + 1,
            None => snap(g),
        }
    };
    let rel = (off - w0) as usize;
    let mut start = match memchr::memrchr(b'\n', &buf[..rel]) {
        Some(i) => Some(w0 + i as u64 + 1),
        None if w0 == 0 => Some(0),
        None => None,
    };
    let g1 = off / SEG * SEG;
    // The next grid point counts too when its cut lands at or before `off`.
    for g in [g1 + SEG, g1, g1.wrapping_sub(SEG)] {
        if g > off + CUT_BACK + 3 || g == 0 || !valid(g) {
            continue;
        }
        let b = cut(g);
        if b <= off {
            if start.is_none_or(|s| b > s) {
                start = Some(b);
            }
            break;
        }
    }
    let start = start.unwrap_or(w0);
    let line_start = start == 0 || (start > w0 && at(start - 1) == b'\n');
    // End: the line break, or the next grid boundary.
    let from = (start - w0) as usize;
    let nl = memchr::memchr(b'\n', &buf[from..]).map(|i| start + i as u64);
    let mut gend = None;
    let mut g = (start / SEG + 1) * SEG;
    while g < w1 && g <= start + 2 * SEG {
        if valid(g) {
            let b = cut(g);
            if b > start {
                gend = Some(b);
                break;
            }
        }
        g += SEG;
    }
    let content_end = nl.map(|p| if p > start && at(p - 1) == b'\r' { p - 1 } else { p });
    match (content_end, gend) {
        (Some(ce), Some(ge)) if ge < ce => Seg { start, end: ge, line_start, line_end: false, eol: 0 },
        (Some(ce), _) => {
            let p = nl.unwrap();
            Seg { start, end: ce, line_start, line_end: true, eol: (p + 1 - ce) as u8 }
        }
        (None, Some(ge)) => Seg { start, end: ge, line_start, line_end: false, eol: 0 },
        (None, None) => Seg { start, end: w1.min(len), line_start, line_end: w1 >= len, eol: 0 },
    }
}

// ---------------------------------------------------------------------------------------------------------------
// Editing operations. They work on the document and return the new selection; the caller reveals the caret.

/// Replaces the selection with `text`.
pub fn replace_selection(doc: &mut Document, sel: Sel, text: &[u8], kind: EditKind) -> Sel {
    doc.begin(kind, sel);
    let a = sel.start();
    if !sel.is_empty() {
        doc.delete(a, sel.end());
    }
    doc.insert(a, text);
    let new = Sel::at(a + text.len() as u64);
    doc.end(new);
    new
}

/// Converts pasted text's line breaks to the document's.
pub fn normalize_eols(text: &[u8], eol: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(text.len() + text.len() / 32);
    let mut i = 0;
    while i < text.len() {
        match text[i] {
            b'\r' => {
                out.extend_from_slice(eol);
                if text.get(i + 1) == Some(&b'\n') {
                    i += 1;
                }
            }
            b'\n' => out.extend_from_slice(eol),
            b => out.push(b),
        }
        i += 1;
    }
    out
}

/// The position after the character people see at `pos`: a letter with its accents, an emoji sequence, a flag
/// (Left/Right and Delete step over it whole). `\r\n` counts as one.
pub fn next_cluster(doc: &Document, pos: u64) -> u64 {
    let len = doc.len();
    if pos >= len {
        return len;
    }
    let b = doc.read(pos, (pos + 256).min(len));
    if matches!(b.first(), Some(b'\r' | b'\n')) {
        return doc.next_char(pos);
    }
    pos + text::cluster_len_at(&b).max(1) as u64
}

/// The text from shortly before `pos` (from its line's start when that's near) up to `pos`.
fn before(doc: &Document, pos: u64) -> Vec<u8> {
    let b = doc.read(pos.saturating_sub(256), pos);
    match memchr::memrchr(b'\n', &b) {
        Some(i) => b[i + 1..].to_vec(),
        None => b,
    }
}

/// The position before the character people see that ends at `pos`.
pub fn prev_cluster(doc: &Document, pos: u64) -> u64 {
    let b = before(doc, pos);
    if b.is_empty() || b.ends_with(b"\r") {
        return doc.prev_char(pos);
    }
    pos - text::cluster_len_before(&b).max(1) as u64
}

pub fn backspace(doc: &mut Document, sel: Sel, word: bool) -> Sel {
    if !sel.is_empty() {
        return replace_selection(doc, sel, b"", EditKind::Other);
    }
    if sel.caret == 0 {
        return sel;
    }
    // (an emoji or a flag goes as a whole; an accented letter loses its last accent)
    let back = || {
        let b = before(doc, sel.caret);
        if b.is_empty() || b.ends_with(b"\r") { doc.prev_char(sel.caret) } else { sel.caret - text::backspace_len(&b).max(1) as u64 }
    };
    let a = if word { doc.word_left(sel.caret) } else { back() };
    doc.begin(if word { EditKind::Other } else { EditKind::Backspace }, sel);
    doc.delete(a, sel.caret);
    let new = Sel::at(a);
    doc.end(new);
    new
}

pub fn delete_forward(doc: &mut Document, sel: Sel, word: bool) -> Sel {
    if !sel.is_empty() {
        return replace_selection(doc, sel, b"", EditKind::Other);
    }
    if sel.caret >= doc.len() {
        return sel;
    }
    let b = if word { doc.word_right(sel.caret) } else { next_cluster(doc, sel.caret) };
    doc.begin(if word { EditKind::Other } else { EditKind::DeleteForward }, sel);
    doc.delete(sel.caret, b);
    doc.end(sel);
    Sel::at(sel.caret)
}

/// Enter: a line break plus the current line's indentation (one level more after `{` or `[`, and the closing
/// bracket moves to its own line). On a line holding nothing but indentation, that indentation moves to the new
/// line instead of staying behind as trailing spaces.
pub fn newline(doc: &mut Document, sel: Sel, indent_unit: &[u8]) -> Sel {
    let a = sel.start();
    let ls = doc.line_start_of(a);
    let head = doc.read(ls, a.min(ls + 4096));
    let indent: Vec<u8> = head.iter().copied().take_while(|&b| b == b' ' || b == b'\t').collect();
    if !indent.is_empty() && indent.len() as u64 == a - ls && doc.line_end_of(sel.end()) == sel.end() {
        let mut text = eol(doc).to_vec();
        text.extend_from_slice(&indent);
        let new = Sel::at(ls + text.len() as u64);
        doc.begin(EditKind::Other, sel);
        doc.delete(ls, sel.end());
        doc.insert(ls, &text);
        doc.end(new);
        return new;
    }
    let before = head.iter().rev().find(|&&b| b != b' ' && b != b'\t').copied();
    let after = doc.byte_at(sel.end());
    let mut text = eol(doc).to_vec();
    text.extend_from_slice(&indent);
    let mut caret_back = 0u64;
    if matches!(before, Some(b'{') | Some(b'[')) {
        text.extend_from_slice(indent_unit);
        let closes = matches!((before, after), (Some(b'{'), Some(b'}')) | (Some(b'['), Some(b']')));
        if closes {
            let tail_len = eol(doc).len() + indent.len();
            text.extend_from_slice(eol(doc));
            text.extend_from_slice(&indent);
            caret_back = tail_len as u64;
        }
    }
    let new = replace_selection(doc, sel, &text, EditKind::Other);
    Sel::at(new.caret - caret_back)
}

/// Typing `}` or `]` with only indentation before it on the line (where Enter after `{` put one level more): the
/// line gets the indentation of the line with the matching `{` or `[`, or one level less when that isn't found
/// nearby. Done with the bracket as one step; None when there's nothing to change.
pub fn close_bracket(doc: &mut Document, sel: Sel, close: u8) -> Option<Sel> {
    let pos = sel.caret;
    let ls = doc.line_start_of(pos);
    if !sel.is_empty() || pos == ls || pos - ls > 4096 {
        return None;
    }
    let head = doc.read(ls, pos);
    if !head.iter().all(|&b| b == b' ' || b == b'\t') {
        return None;
    }
    let open = if close == b'}' { b'{' } else { b'[' };
    // (Without the bracket it closes, nothing tells what the indentation should be: typed as usual.)
    let o = matching_open(doc, ls, open, close)?;
    let ols = doc.line_start_of(o);
    let indent: Vec<u8> = doc.read(ols, o.min(ols + 4096)).into_iter().take_while(|&b| b == b' ' || b == b'\t').collect();
    if indent == head {
        return None;
    }
    let mut text = indent;
    text.push(close);
    let new = Sel::at(ls + text.len() as u64);
    doc.begin(EditKind::Other, sel);
    doc.delete(ls, pos);
    doc.insert(ls, &text);
    doc.end(new);
    Some(new)
}

/// The `open` bracket that a `close` typed at `before` would match (looking back up to 256 KB; brackets inside
/// strings or comments count too, which is good enough for indenting).
fn matching_open(doc: &Document, before: u64, open: u8, close: u8) -> Option<u64> {
    let from = before.saturating_sub(256 << 10);
    let text = doc.read(from, before);
    let mut depth = 0usize;
    for (i, &b) in text.iter().enumerate().rev() {
        if b == close {
            depth += 1;
        } else if b == open {
            if depth == 0 {
                return Some(from + i as u64);
            }
            depth -= 1;
        }
    }
    None
}

/// Typing `text` with overtype on (Insert): it replaces as many characters after the caret as it has, but not a
/// line break (at the end of a line it's added; a lone CR isn't one). A selection is replaced as usual. Characters
/// typed one after the other are one undo step, as with ordinary typing.
pub fn overtype(doc: &mut Document, sel: Sel, text: &[u8]) -> Sel {
    if !sel.is_empty() {
        return replace_selection(doc, sel, text, EditKind::Typing);
    }
    let pos = sel.caret;
    let mut end = pos;
    for _ in String::from_utf8_lossy(text).chars() {
        match doc.byte_at(end) {
            None | Some(b'\n') => break,
            Some(b'\r') if doc.byte_at(end + 1) == Some(b'\n') => break,
            _ => end = next_cluster(doc, end),
        }
    }
    doc.begin(EditKind::Typing, sel);
    doc.delete(pos, end);
    doc.insert(pos, text);
    let new = Sel::at(pos + text.len() as u64);
    doc.end(new);
    new
}

/// How far `matching_bracket` looks for the other bracket.
const BRACKET_REACH: u64 = 1 << 20;

/// The bracket just before `pos` (else the one just after it) and the bracket it pairs with: `(`, `[` and `{`
/// with their closing ones, up to 1 MB away. Brackets in strings and comments count too.
pub fn matching_bracket(doc: &Document, pos: u64) -> Option<(u64, u64)> {
    for p in [pos.checked_sub(1), Some(pos)].into_iter().flatten() {
        let Some(b) = doc.byte_at(p) else { continue };
        let (open, close, forward) = match b {
            b'(' => (b'(', b')', true),
            b'[' => (b'[', b']', true),
            b'{' => (b'{', b'}', true),
            b')' => (b'(', b')', false),
            b']' => (b'[', b']', false),
            b'}' => (b'{', b'}', false),
            _ => continue,
        };
        if let Some(q) = bracket_partner(doc, p, open, close, forward) {
            return Some((p, q));
        }
    }
    None
}

fn bracket_partner(doc: &Document, at: u64, open: u8, close: u8, forward: bool) -> Option<u64> {
    const CHUNK: u64 = 64 << 10;
    let mut depth = 0u64;
    if forward {
        let end = (at + 1 + BRACKET_REACH).min(doc.len());
        let mut a = at + 1;
        while a < end {
            let b = (a + CHUNK).min(end);
            for (i, &c) in doc.read(a, b).iter().enumerate() {
                if c == open {
                    depth += 1;
                } else if c == close {
                    if depth == 0 {
                        return Some(a + i as u64);
                    }
                    depth -= 1;
                }
            }
            a = b;
        }
    } else {
        let start = at.saturating_sub(BRACKET_REACH);
        let mut b = at;
        while b > start {
            let a = b.saturating_sub(CHUNK).max(start);
            for (i, &c) in doc.read(a, b).iter().enumerate().rev() {
                if c == close {
                    depth += 1;
                } else if c == open {
                    if depth == 0 {
                        return Some(a + i as u64);
                    }
                    depth -= 1;
                }
            }
            b = a;
        }
    }
    None
}

/// `pos`, moved back to the start of the character it falls in (and to the `\r` of a `\r\n`): a place from another
/// version of the text (a reopened tab, the session) never splits a character or a line break.
pub fn char_start(doc: &Document, pos: u64) -> u64 {
    let pos = pos.min(doc.len());
    let mut p = pos;
    while p > 0 && pos - p < 3 && doc.byte_at(p).is_some_and(is_continuation) {
        p -= 1;
    }
    if p > 0 && doc.byte_at(p) == Some(b'\n') && doc.byte_at(p - 1) == Some(b'\r') {
        p -= 1;
    }
    p
}

/// Characters and words in a text (the status bar's counts).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Counts {
    pub chars: u64,
    pub words: u64,
}

/// Counts characters and words of a text fed in pieces, which may cut a character in two. A word is a run of
/// characters that aren't spaces (as `wc -w` counts); characters are counted as the status bar's selection count
/// does (`\r\n` is two).
#[derive(Default)]
pub struct Counter {
    counts: Counts,
    in_word: bool,
    /// The start of a character the last piece ended in.
    partial: Vec<u8>,
}

impl Counter {
    pub fn feed(&mut self, data: &[u8]) {
        let mut i = 0;
        if !self.partial.is_empty() {
            let need = utf8_len(self.partial[0]);
            while self.partial.len() < need && i < data.len() && is_continuation(data[i]) {
                self.partial.push(data[i]);
                i += 1;
            }
            if self.partial.len() < need && i == data.len() {
                return;
            }
            let c = std::mem::take(&mut self.partial);
            self.char(&c);
        }
        while i < data.len() {
            let b = data[i];
            if b < 0x80 {
                self.counts.chars += 1;
                self.step(matches!(b, b' ' | b'\t' | b'\n' | b'\r' | 0x0B | 0x0C));
                i += 1;
            } else if is_continuation(b) {
                // (a stray continuation byte: part of the word it's in, not a character of its own)
                self.step(false);
                i += 1;
            } else {
                let need = utf8_len(b);
                let mut j = i + 1;
                while j < data.len() && j < i + need && is_continuation(data[j]) {
                    j += 1;
                }
                if j == data.len() && j < i + need {
                    self.partial.extend_from_slice(&data[i..j]);
                    return;
                }
                self.char(&data[i..j]);
                i = j;
            }
        }
    }

    pub fn finish(mut self) -> Counts {
        if !self.partial.is_empty() {
            let c = std::mem::take(&mut self.partial);
            self.char(&c);
        }
        self.counts
    }

    fn char(&mut self, c: &[u8]) {
        self.counts.chars += 1;
        // the other Unicode spaces: U+0085, U+00A0, U+1680, U+2000..U+200A, U+2028, U+2029, U+202F, U+205F, U+3000
        let space = matches!(
            c,
            [0xC2, 0x85 | 0xA0]
                | [0xE1, 0x9A, 0x80]
                | [0xE2, 0x80, 0x80..=0x8A | 0xA8 | 0xA9 | 0xAF]
                | [0xE2, 0x81, 0x9F]
                | [0xE3, 0x80, 0x80]
        );
        self.step(space);
    }

    fn step(&mut self, space: bool) {
        if space {
            self.in_word = false;
        } else if !self.in_word {
            self.in_word = true;
            self.counts.words += 1;
        }
    }
}

/// Characters and words in `text`.
pub fn count(text: &[u8]) -> Counts {
    let mut c = Counter::default();
    c.feed(text);
    c.finish()
}

/// Line starts of the lines touched by the selection (a selection ending at a line start doesn't include that
/// line). None if there are too many.
pub fn selected_lines(doc: &Document, sel: Sel) -> Option<Vec<u64>> {
    let first = doc.line_start_of(sel.start());
    let mut last_pos = sel.end();
    if !sel.is_empty() && last_pos > first && doc.line_start_of(last_pos) == last_pos {
        last_pos -= 1;
    }
    let (Some(l0), Some(l1)) = (doc.line_of(first), doc.line_of(last_pos)) else { return None };
    if l1 - l0 > MAX_LINE_OPS {
        return None;
    }
    let mut v = Vec::with_capacity((l1 - l0 + 1) as usize);
    let mut p = first;
    loop {
        v.push(p);
        if v.len() as u64 > l1 - l0 {
            break;
        }
        match doc.next_newline(p) {
            Some(n) if n < last_pos => p = n + 1,
            _ => break,
        }
    }
    Some(v)
}

/// Replaces `[a, b)` with `text` as one undo step (from selection `before`) and returns the selection of the new
/// text.
pub fn replace_range(doc: &mut Document, before: Sel, a: u64, b: u64, text: &[u8]) -> Sel {
    doc.seal();
    doc.begin(EditKind::Other, before);
    doc.delete(a, b);
    doc.insert(a, text);
    let new = Sel::new(a, a + text.len() as u64);
    doc.end(new);
    doc.seal();
    new
}

/// Comments out the selected lines (or the caret's), or uncomments them when they all are. Says why not when it
/// can't (too many lines; a block comment can't go around text that has a comment end in it).
pub fn toggle_comment(doc: &mut Document, sel: Sel, style: CommentStyle) -> Result<Sel, String> {
    toggle_comment_with(doc, sel, style, INDENT_AT_ONCE_MAX)
}

/// `toggle_comment`, rewriting up to `at_once_max` bytes of lines as one replacement (more: line by line).
fn toggle_comment_with(doc: &mut Document, sel: Sel, style: CommentStyle, at_once_max: u64) -> Result<Sel, String> {
    let lines = selected_lines(doc, sel).ok_or("Too many lines selected for that.")?;
    let (mut anchor, mut caret) = (sel.anchor, sel.caret);
    let start = sel.start();
    let ranged = !sel.is_empty();
    // Where an insertion moves a position: the selection's start stays before what's inserted at it.
    let ins = |p: &mut u64, at: u64, n: u64| {
        if *p > at || (*p == at && !(ranged && *p == start)) {
            *p += n;
        }
    };
    let del = |p: &mut u64, at: u64, n: u64| {
        if *p > at {
            *p = (*p).saturating_sub(n).max(at);
        }
    };
    match style {
        CommentStyle::Line(tok) | CommentStyle::AfterNumber(tok) => {
            let core = tok.trim_end().as_bytes();
            let numbered = matches!(style, CommentStyle::AfterNumber(_));
            let a = lines[0];
            let b = doc.line_end_of(*lines.last().unwrap_or(&a)).max(a);
            if b - a <= at_once_max {
                // one replacement: quick even for 200,000 lines (and so is its undo)
                return Ok(line_comment_at_once(doc, sel, a, b, core, numbered));
            }
            let wordy = core.last().is_some_and(|c| c.is_ascii_alphanumeric());
            // (line start, indentation, already commented) for each line that isn't blank
            let mut info = Vec::with_capacity(lines.len());
            for &ls in &lines {
                let le = doc.line_end_of(ls);
                let head = doc.read(ls, le.min(ls + 4096));
                let ind = text_start(&head, numbered);
                if ind == head.len() {
                    continue;
                }
                let rest = &head[ind..];
                let commented = rest.len() >= core.len()
                    && rest[..core.len()].eq_ignore_ascii_case(core)
                    && !(wordy && rest.get(core.len()).is_some_and(|c| !c.is_ascii_whitespace()));
                info.push((ls, ind as u64, commented));
            }
            if info.is_empty() {
                return Ok(sel);
            }
            let remove = info.iter().all(|x| x.2);
            let col = info.iter().map(|x| x.1).min().unwrap_or(0);
            doc.begin(EditKind::Other, sel);
            for &(ls, ind, _) in info.iter().rev() {
                if remove {
                    let at = ls + ind;
                    let mut n = core.len() as u64;
                    if doc.byte_at(at + n) == Some(b' ') {
                        n += 1;
                    }
                    doc.delete(at, at + n);
                    del(&mut anchor, at, n);
                    del(&mut caret, at, n);
                } else {
                    let at = ls + if numbered { ind } else { col };
                    let text = [core, b" "].concat();
                    doc.insert(at, &text);
                    ins(&mut anchor, at, text.len() as u64);
                    ins(&mut caret, at, text.len() as u64);
                }
            }
        }
        CommentStyle::Block(open, close) => {
            let (open, close) = (open.as_bytes(), close.as_bytes());
            let a = lines[0];
            let b = doc.line_end_of(*lines.last().unwrap_or(&a));
            if b - a > 16 << 20 {
                return Err("Too much text selected for that.".into());
            }
            let text = doc.read(a, b);
            let s = text.iter().position(|c| !c.is_ascii_whitespace());
            let Some(s) = s else { return Ok(sel) };
            let e = text.len() - text.iter().rev().position(|c| !c.is_ascii_whitespace()).unwrap_or(0);
            let body = &text[s..e];
            let commented = body.len() >= open.len() + close.len() && body.starts_with(open) && body.ends_with(close);
            let inner = if commented { &body[open.len()..body.len() - close.len()] } else { body };
            if memchr::memmem::find(inner, close).is_some() {
                // `<!-- a --> x <!-- b -->`: these comments don't nest, so neither removing nor adding one works
                let close = String::from_utf8_lossy(close);
                return Err(format!("These lines already have a comment end ({close}) in them, so they can't be commented as one block."));
            }
            doc.begin(EditKind::Other, sel);
            if commented {
                // remove the markers (and the space just inside each)
                let lead = inner.first() == Some(&b' ');
                let trail = inner.len() > lead as usize && inner.last() == Some(&b' ');
                let ce = a + e as u64;
                let cn = close.len() as u64 + trail as u64;
                doc.delete(ce - cn, ce);
                del(&mut anchor, ce - cn, cn);
                del(&mut caret, ce - cn, cn);
                let os = a + s as u64;
                let on = open.len() as u64 + lead as u64;
                doc.delete(os, os + on);
                del(&mut anchor, os, on);
                del(&mut caret, os, on);
            } else {
                let ce = a + e as u64;
                let end = [b" ", close].concat();
                doc.insert(ce, &end);
                if anchor >= ce && !(ranged && anchor == start) {
                    anchor += end.len() as u64;
                }
                if caret >= ce && !(ranged && caret == start) {
                    caret += end.len() as u64;
                }
                let os = a + s as u64;
                let begin = [open, b" "].concat();
                doc.insert(os, &begin);
                ins(&mut anchor, os, begin.len() as u64);
                ins(&mut caret, os, begin.len() as u64);
            }
        }
    }
    let new = Sel::new(anchor, caret);
    doc.end(new);
    Ok(new)
}

/// Where a line's text starts: after its indentation, and when `numbered` also after the line number before it and
/// the spaces after that (PPCL's `00010     SET(…)`, and `# 00010     SET(…)` for a line that's turned off).
fn text_start(line: &[u8], numbered: bool) -> usize {
    let ws = |i: usize| i + line[i..].iter().take_while(|&&c| c == b' ' || c == b'\t').count();
    let s = ws(0);
    let n = if line.get(s) == Some(&b'#') { ws(s + 1) } else { s };
    let d = n + line[n..].iter().take_while(|c| c.is_ascii_digit()).count();
    if numbered && d > n && matches!(line.get(d), None | Some(b' ' | b'\t')) { ws(d) } else { s }
}

/// Line comments added to (or taken from) the whole lines in `[a, b)`, as one replacement of their text (see
/// `toggle_comment`).
fn line_comment_at_once(doc: &mut Document, sel: Sel, a: u64, b: u64, core: &[u8], numbered: bool) -> Sel {
    let text = doc.read(a, b);
    let wordy = core.last().is_some_and(|c| c.is_ascii_alphanumeric());
    // (offset in `text`, indentation, already commented) for each line that isn't blank
    let mut info = Vec::new();
    let mut off = 0usize;
    for line in text.split(|&c| c == b'\n') {
        let content = line.strip_suffix(b"\r").unwrap_or(line);
        let head = &content[..content.len().min(4096)];
        let ind = text_start(head, numbered);
        if ind < head.len() {
            let rest = &head[ind..];
            let commented = rest.len() >= core.len()
                && rest[..core.len()].eq_ignore_ascii_case(core)
                && !(wordy && rest.get(core.len()).is_some_and(|c| !c.is_ascii_whitespace()));
            info.push((off, ind, commented));
        }
        off += line.len() + 1;
    }
    if info.is_empty() {
        return sel;
    }
    let remove = info.iter().all(|x| x.2);
    let col = info.iter().map(|x| x.1).min().unwrap_or(0);
    // the new text, and where in the document it grows or shrinks by how much
    let mut out = Vec::with_capacity(text.len() + if remove { 0 } else { info.len() * (core.len() + 1) });
    let mut edits: Vec<(u64, i64)> = Vec::with_capacity(info.len());
    let mut copied = 0usize;
    for &(off, ind, _) in &info {
        if remove {
            let at = off + ind;
            let n = core.len() + (text.get(at + core.len()) == Some(&b' ')) as usize;
            out.extend_from_slice(&text[copied..at]);
            copied = at + n;
            edits.push((a + at as u64, -(n as i64)));
        } else {
            let at = off + if numbered { ind } else { col };
            out.extend_from_slice(&text[copied..at]);
            out.extend_from_slice(core);
            out.push(b' ');
            copied = at;
            edits.push((a + at as u64, core.len() as i64 + 1));
        }
    }
    out.extend_from_slice(&text[copied..]);
    let keep = (!sel.is_empty()).then_some(sel.start());
    let new = Sel::new(map_through(&edits, sel.anchor, keep), map_through(&edits, sel.caret, keep));
    doc.begin(EditKind::Other, sel);
    doc.delete(a, b);
    doc.insert(a, &out);
    doc.end(new);
    new
}

/// Where position `p` ends up after `edits` ((where, size change), in order, not overlapping): inside removed
/// text it moves to where that was; at an insertion it moves past it, unless it's `keep` (where a selection
/// starts, which stays before what's inserted there).
fn map_through(edits: &[(u64, i64)], p: u64, keep: Option<u64>) -> u64 {
    let k = edits.partition_point(|e| e.0 < p);
    let mut shift: i64 = edits[..k].iter().map(|e| e.1).sum();
    if let Some(&(at, d)) = k.checked_sub(1).map(|j| &edits[j]) {
        if d < 0 && p < at + d.unsigned_abs() {
            return (at as i64 + shift - d) as u64;
        }
    }
    if let Some(&(at, d)) = edits.get(k) {
        if at == p && d > 0 && keep != Some(p) {
            shift += d;
        }
    }
    (p as i64 + shift) as u64
}

/// Indents (or outdents) the selected lines by one level of `ind` (outdent removes a tab, or up to that many
/// spaces). The lines are rewritten as one replacement, so even 200,000 lines take one quick step (and one quick
/// undo).
pub fn indent_lines(doc: &mut Document, sel: Sel, ind: Indent, tab_size: u32, outdent: bool) -> Result<Sel, &'static str> {
    let lines = selected_lines(doc, sel).ok_or(TOO_MANY_LINES)?;
    let unit = ind.unit();
    let width = match ind {
        Indent::Spaces(n) => n,
        Indent::Tabs => tab_size,
    }
    .max(1) as usize;
    let a = lines[0];
    let b = doc.line_end_of(*lines.last().unwrap()).max(a);
    if b - a > INDENT_AT_ONCE_MAX {
        // (Huge lines: change each one where it is instead of copying them all.)
        return Ok(indent_each(doc, sel, &lines, &unit, width, outdent));
    }
    let text = doc.read(a, b);
    let mut out = Vec::with_capacity(text.len() + if outdent { 0 } else { lines.len() * unit.len() });
    // Each line's start in the document and how much the line grows (or shrinks) there.
    let mut starts = Vec::with_capacity(lines.len());
    let mut deltas: Vec<i64> = Vec::with_capacity(lines.len());
    let mut off = a;
    for line in text.split(|&c| c == b'\n') {
        starts.push(off);
        off += line.len() as u64 + 1;
        let delta = if outdent {
            let n = if line.first() == Some(&b'\t') { 1 } else { line.iter().take(width).take_while(|&&c| c == b' ').count() };
            out.extend_from_slice(&line[n..]);
            -(n as i64)
        } else if lines.len() > 1 && matches!(line, [] | [b'\r']) {
            // empty lines stay empty
            out.extend_from_slice(line);
            0
        } else {
            out.extend_from_slice(&unit);
            out.extend_from_slice(line);
            unit.len() as i64
        };
        deltas.push(delta);
        out.push(b'\n');
    }
    out.pop();
    if deltas.iter().all(|&d| d == 0) {
        return Ok(sel);
    }
    // Where a position moves: along with the changes at the line starts before it, and at its own line's start
    // (outdenting stops at the line start; indenting moves it along, unless it's where a selection starts, which
    // stays before the new indentation).
    let mut before = vec![0i64; deltas.len()];
    for k in 1..deltas.len() {
        before[k] = before[k - 1] + deltas[k - 1];
    }
    let map = |p: u64| -> u64 {
        let k = starts.partition_point(|&s| s <= p).max(1) - 1;
        let (s, d) = (starts[k] as i64, deltas[k]);
        let p = p as i64;
        let own = if d < 0 {
            (p + d).max(s) - p
        } else if p > s || sel.is_empty() || p != sel.start() as i64 {
            d
        } else {
            0
        };
        (p + before[k] + own) as u64
    };
    let new = Sel::new(map(sel.anchor), map(sel.caret));
    doc.begin(EditKind::Other, sel);
    doc.delete(a, b);
    doc.insert(a, &out);
    doc.end(new);
    Ok(new)
}

/// `indent_lines` one line at a time (for selections with lines too long to copy).
fn indent_each(doc: &mut Document, sel: Sel, lines: &[u64], unit: &[u8], width: usize, outdent: bool) -> Sel {
    doc.begin(EditKind::Other, sel);
    let mut anchor = sel.anchor;
    let mut caret = sel.caret;
    let adjust = |p: &mut u64, at: u64, delta: i64| {
        if *p >= at {
            *p = (*p as i64 + delta).max(at as i64) as u64;
        }
    };
    // Work from the last line up so earlier offsets stay valid.
    for &ls in lines.iter().rev() {
        if outdent {
            let head = doc.read(ls, ls + width as u64);
            let n = if head.first() == Some(&b'\t') {
                1
            } else {
                head.iter().take_while(|&&b| b == b' ').count()
            };
            if n > 0 {
                doc.delete(ls, ls + n as u64);
                adjust(&mut anchor, ls, -(n as i64));
                adjust(&mut caret, ls, -(n as i64));
            }
        } else {
            let line_empty = doc.line_end_of(ls) == ls;
            if line_empty && lines.len() > 1 {
                continue;
            }
            doc.insert(ls, unit);
            if anchor > ls || (anchor == ls && !sel.is_empty() && anchor != sel.start()) {
                anchor += unit.len() as u64;
            } else if anchor == ls && sel.is_empty() {
                anchor += unit.len() as u64;
            }
            if caret > ls || (caret == ls && (sel.is_empty() || caret != sel.start())) {
                caret += unit.len() as u64;
            }
        }
    }
    let new = Sel::new(anchor, caret);
    doc.end(new);
    new
}

/// The indentation to insert for Tab at `pos`: a tab, or spaces up to the next multiple of the indent width (tabs
/// before `pos` count as `tab_size` columns).
pub fn tab_text(doc: &Document, pos: u64, ind: Indent, tab_size: u32) -> Vec<u8> {
    let Indent::Spaces(width) = ind else { return b"\t".to_vec() };
    let (width, tab_size) = (width.max(1), tab_size.max(1));
    let ls = doc.line_start_of(pos);
    let head = doc.read(pos.saturating_sub(1024).max(ls), pos);
    let mut col = 0u32;
    for c in String::from_utf8_lossy(&head).chars() {
        col = if c == '\t' { (col / tab_size + 1) * tab_size } else { col + 1 };
    }
    vec![b' '; (width - col % width) as usize]
}

/// Duplicates the selection, or the current line when nothing is selected.
pub fn duplicate(doc: &mut Document, sel: Sel) -> Result<Sel, &'static str> {
    let (a, b) = if sel.is_empty() { (doc.line_start_of(sel.caret), doc.line_end_of(sel.caret)) } else { (sel.start(), sel.end()) };
    if b - a > COPY_MAX {
        return Err(TOO_MUCH_TEXT);
    }
    doc.begin(EditKind::Other, sel);
    let new = if sel.is_empty() {
        let ls = doc.line_start_of(sel.caret);
        let le = doc.line_end_of(sel.caret);
        let mut text = eol(doc).to_vec();
        text.extend_from_slice(&doc.read(ls, le));
        doc.insert(le, &text);
        Sel::at(sel.caret + text.len() as u64)
    } else {
        let text = doc.read(sel.start(), sel.end());
        doc.insert(sel.end(), &text);
        let n = text.len() as u64;
        Sel::new(sel.anchor + n, sel.caret + n)
    };
    doc.end(new);
    Ok(new)
}

/// Deletes the lines touched by the selection.
pub fn delete_lines(doc: &mut Document, sel: Sel) -> Option<Sel> {
    let lines = selected_lines(doc, sel)?;
    let a = lines[0];
    let last = *lines.last().unwrap();
    let b = match doc.next_newline(last) {
        Some(n) => n + 1,
        None => doc.len(),
    };
    // Deleting the last line also takes the line break before it.
    let a2 = if b == doc.len() && a > 0 && doc.next_newline(last).is_none() {
        doc.prev_char(a)
    } else {
        a
    };
    doc.begin(EditKind::Other, sel);
    doc.delete(a2, b);
    let new = Sel::at(a2.min(doc.len()));
    doc.end(new);
    Some(new)
}

/// Moves the selected lines up or down by one.
pub fn move_lines(doc: &mut Document, sel: Sel, down: bool) -> Result<Sel, &'static str> {
    let lines = selected_lines(doc, sel).ok_or(TOO_MANY_LINES)?;
    let a = lines[0];
    let last = *lines.last().unwrap();
    let block_end = doc.line_end_of(last);
    let e = eol(doc).to_vec();
    if down {
        let Some(nl) = doc.next_newline(block_end) else { return Ok(sel) };
        let next_start = nl + 1;
        let next_end = doc.line_end_of(next_start);
        if next_end - a > COPY_MAX {
            return Err(TOO_MUCH_TEXT);
        }
        let block = doc.read(a, block_end);
        let next = doc.read(next_start, next_end);
        doc.begin(EditKind::Other, sel);
        doc.delete(a, next_end);
        let mut t = next.clone();
        t.extend_from_slice(&e);
        t.extend_from_slice(&block);
        doc.insert(a, &t);
        let shift = next.len() as u64 + e.len() as u64;
        let new = Sel::new(sel.anchor + shift, sel.caret + shift);
        doc.end(new);
        Ok(new)
    } else {
        if a == 0 {
            return Ok(sel);
        }
        let prev_start = doc.line_start_of(a - 1);
        let prev_end = doc.line_end_of(prev_start);
        if block_end - prev_start > COPY_MAX {
            return Err(TOO_MUCH_TEXT);
        }
        let block = doc.read(a, block_end);
        let prev = doc.read(prev_start, prev_end);
        doc.begin(EditKind::Other, sel);
        doc.delete(prev_start, block_end);
        let mut t = block.clone();
        t.extend_from_slice(&e);
        t.extend_from_slice(&prev);
        doc.insert(prev_start, &t);
        let shift = a - prev_start;
        let new = Sel::new(sel.anchor - shift, sel.caret - shift);
        doc.end(new);
        Ok(new)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn places_from_another_text_never_split_a_character_or_a_line_break() {
        let d = Document::from_text("abcd\r\nefgh é\r\n".as_bytes());
        assert_eq!(char_start(&d, 5), 4);
        assert_eq!(char_start(&d, 4), 4);
        assert_eq!(char_start(&d, 6), 6);
        assert_eq!(char_start(&d, 12), 11);
        assert_eq!(char_start(&d, 99), d.len());
    }

    fn segs(text: &[u8]) -> Vec<Seg> {
        let len = text.len() as u64;
        let mut out = Vec::new();
        let mut off = 0u64;
        loop {
            let s = segment_in(text, 0, len, off);
            out.push(s);
            if s.eol == 0 && s.end >= len {
                break;
            }
            off = s.next_start();
        }
        out
    }

    #[test]
    fn short_lines_are_one_segment_each() {
        let s = segs(b"one\r\ntwo\n\nthree");
        assert_eq!(s.len(), 4);
        assert_eq!((s[0].start, s[0].end, s[0].eol), (0, 3, 2));
        assert_eq!((s[1].start, s[1].end, s[1].eol), (5, 8, 1));
        assert_eq!((s[2].start, s[2].end, s[2].eol), (9, 9, 1));
        assert_eq!((s[3].start, s[3].end, s[3].eol), (10, 15, 0));
        assert!(s.iter().all(|x| x.line_start));
        let s = segs(b"abc\n");
        assert_eq!(s.len(), 2);
        assert_eq!((s[1].start, s[1].end), (4, 4));
    }

    #[test]
    fn long_lines_split_on_the_grid_and_agree_from_any_offset() {
        let mut text = Vec::new();
        text.extend_from_slice(b"short\n");
        text.extend(std::iter::repeat_n(b'x', 57_337));
        text.extend_from_slice("é".as_bytes()); // straddles the grid point at 57344: the cut moves before it
        text.extend(std::iter::repeat_n(b'y', 20_000));
        text.push(b'\n');
        text.extend(std::iter::repeat_n(b'z', 4000)); // shorter than HALF: never split
        let len = text.len() as u64;
        let all = segs(&text);
        // every segment is at most ~1.5 SEG and the long line got split
        assert!(all.iter().all(|s| s.end - s.start <= SEG + HALF + 4));
        assert!(all.len() > 8);
        // segment_at from any offset inside a segment gives the same segment
        for s in &all {
            for off in [s.start, (s.start + s.end) / 2, s.end.saturating_sub(1).max(s.start)] {
                let w0 = off.saturating_sub(2 * SEG);
                let w1 = (off + 2 * SEG + 4).min(len);
                let got = segment_in(&text[w0 as usize..w1 as usize], w0, len, off);
                assert_eq!(got, *s, "from {off}");
            }
        }
        // boundaries fall on char boundaries
        for s in &all {
            assert!(std::str::from_utf8(&text[s.start as usize..s.end as usize]).is_ok());
        }
        assert!(all.iter().any(|s| s.start == 57_343), "cut before the é");
        // Long lines of words are cut right after a space, like an ordinary wrap.
        let words: Vec<u8> = (0..30_000).flat_map(|i| format!("word{} ", i % 97).into_bytes()).collect();
        let s = segs(&words);
        assert!(s.len() > 10);
        for x in &s[1..] {
            assert_eq!(words[x.start as usize - 1], b' ', "cut at {}", x.start);
            let w0 = x.start.saturating_sub(2 * SEG);
            let w1 = (x.start + 2 * SEG + 4).min(words.len() as u64);
            assert_eq!(segment_in(&words[w0 as usize..w1 as usize], w0, words.len() as u64, x.start + 5), *x);
        }
        let last = all.last().unwrap();
        assert_eq!(last.end - last.start, 4000);
        assert!(last.line_start);
    }

    #[test]
    fn edits() {
        let mut d = Document::from_text(b"{}\n  [1, 2]\n");
        let s = newline(&mut d, Sel::at(1), b"  ");
        assert_eq!(d.read(0, d.len()), b"{\r\n  \r\n}\n  [1, 2]\n");
        assert_eq!(s, Sel::at(5));
        let mut d = Document::from_text(b"a\nb\nc");
        let s = move_lines(&mut d, Sel::at(0), true).unwrap();
        assert_eq!(d.read(0, d.len()), b"b\r\na\nc");
        assert_eq!(s, Sel::at(3));
        let mut d = Document::from_text(b"a\nb\nc");
        delete_lines(&mut d, Sel::at(2)).unwrap();
        assert_eq!(d.read(0, d.len()), b"a\nc");
        let mut d = Document::from_text(b"a\nb\nc");
        delete_lines(&mut d, Sel::at(4)).unwrap();
        assert_eq!(d.read(0, d.len()), b"a\nb");
        let mut d = Document::from_text(b"x\ny");
        let s = indent_lines(&mut d, Sel::new(0, 3), Indent::Tabs, 4, false).unwrap();
        assert_eq!(d.read(0, d.len()), b"\tx\n\ty");
        assert_eq!(s, Sel::new(0, 5));
        indent_lines(&mut d, s, Indent::Tabs, 4, true).unwrap();
        assert_eq!(d.read(0, d.len()), b"x\ny");
        assert_eq!(normalize_eols(b"a\nb\r\nc\rd", b"\r\n"), b"a\r\nb\r\nc\r\nd");
        let mut d = Document::from_text(b"ab");
        let s = duplicate(&mut d, Sel::at(1)).unwrap();
        assert_eq!(d.read(0, d.len()), b"ab\r\nab");
        assert_eq!(s, Sel::at(5));
        // Enter on a line of nothing but indentation takes the indentation along instead of leaving it behind.
        let mut d = Document::from_text(b"  x\n    \ny");
        let s = newline(&mut d, Sel::at(8), b"  ");
        assert_eq!(d.read(0, d.len()), b"  x\n\r\n    \ny");
        assert_eq!(s, Sel::at(10));
    }

    #[test]
    fn indenting_at_once_matches_line_by_line() {
        let text = b"a\n  b\n\n\tc\r\n     d\r\n\r\ne  \n    \nf";
        let len = text.len() as u64;
        for ind in [Indent::Tabs, Indent::Spaces(2), Indent::Spaces(4)] {
            for outdent in [false, true] {
                for anchor in 0..=len {
                    for caret in [0, 1, 3, 7, 12, 20, len - 1, len] {
                        let sel = Sel::new(anchor, caret.min(len));
                        let mut one = Document::from_text(text);
                        let mut each = Document::from_text(text);
                        let a = indent_lines(&mut one, sel, ind, 4, outdent).unwrap();
                        let lines = selected_lines(&each, sel).unwrap();
                        let width = match ind {
                            Indent::Spaces(n) => n as usize,
                            Indent::Tabs => 4,
                        };
                        let b = indent_each(&mut each, sel, &lines, &ind.unit(), width, outdent);
                        assert_eq!(one.read(0, one.len()), each.read(0, each.len()), "{ind:?} {outdent} {sel:?}");
                        assert_eq!(a, b, "{ind:?} {outdent} {sel:?}");
                        // one undo step brings it all back
                        if one.can_undo() {
                            one.undo();
                            assert_eq!(one.read(0, one.len()), text);
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn commenting_at_once_matches_line_by_line() {
        let text: &[u8] = b"a\n  b // x\n\n\t// c\r\n     d\r\n  \r\n//e  \n    \n// f";
        let ppcl: &[u8] = b"00010     SET(1)\n00020     C x\n\n  30\tGOTO 10\r\n00040 \r\nC top\n9 C\n# 00045 ON(\"A\")\n00050     c lower";
        for (text, style) in
            [(text, CommentStyle::Line("// ")), (text, CommentStyle::Line("REM ")), (ppcl, CommentStyle::AfterNumber("C "))]
        {
            let len = text.len() as u64;
            for anchor in 0..=len {
                for caret in [0, 1, 3, 7, 12, 20, len - 1, len] {
                    let sel = Sel::new(anchor, caret.min(len));
                    let mut one = Document::from_text(text);
                    let mut each = Document::from_text(text);
                    let a = toggle_comment_with(&mut one, sel, style, u64::MAX).unwrap();
                    let b = toggle_comment_with(&mut each, sel, style, 0).unwrap();
                    assert_eq!(one.read(0, one.len()), each.read(0, each.len()), "{sel:?}");
                    assert_eq!(a, b, "{sel:?}");
                    // and back
                    let a2 = toggle_comment_with(&mut one, a, style, u64::MAX).unwrap();
                    let b2 = toggle_comment_with(&mut each, b, style, 0).unwrap();
                    assert_eq!(one.read(0, one.len()), each.read(0, each.len()), "{sel:?} again");
                    assert_eq!(a2, b2, "{sel:?} again");
                }
            }
        }
    }

    #[test]
    fn ppcl_comments_go_after_each_line_number() {
        let style = CommentStyle::AfterNumber("C ");
        let text: &[u8] = b"00010     SET(1,\"A\")\r\n100 ON(\"B\")\r\n00030     \r\nGOTO 10\r\n# 00040\tOFF(\"B\")\r\n";
        let commented: &[u8] = b"00010     C SET(1,\"A\")\r\n100 C ON(\"B\")\r\n00030     \r\nC GOTO 10\r\n# 00040\tC OFF(\"B\")\r\n";
        let mut d = Document::from_text(text);
        let all = Sel::new(0, d.len());
        let s = toggle_comment(&mut d, all, style).unwrap();
        assert_eq!(d.read(0, d.len()), commented);
        toggle_comment(&mut d, s, style).unwrap();
        assert_eq!(d.read(0, d.len()), text);
        // a comment line's `C` is taken off (a tab after it stays)
        let mut d = Document::from_text(b"01096     C\tNOTE\n01098     C");
        let all = Sel::new(0, d.len());
        toggle_comment(&mut d, all, style).unwrap();
        assert_eq!(d.read(0, d.len()), b"01096     \tNOTE\n01098     ");
    }

    #[test]
    fn closing_brackets_line_up_with_their_opening_line() {
        let mut d = Document::from_text(b"  if (x) {\n      ");
        let s = close_bracket(&mut d, Sel::at(17), b'}').unwrap();
        assert_eq!(d.read(0, d.len()), b"  if (x) {\n  }");
        assert_eq!(s, Sel::at(14));
        // no opening bracket around: typed as usual (a `}` in a TSV row or a makefile recipe keeps its tab)
        let mut d = Document::from_text(b"a\n      ");
        assert!(close_bracket(&mut d, Sel::at(8), b']').is_none());
        let mut d = Document::from_text(b"a\n\t\t");
        assert!(close_bracket(&mut d, Sel::at(4), b'}').is_none());
        // nested: the line of the bracket it closes
        let mut d = Document::from_text(b"[\n  [1,\n   2],\n  {\n    ");
        let end = d.len();
        close_bracket(&mut d, Sel::at(end), b'}').unwrap();
        assert!(d.read(0, d.len()).ends_with(b"  {\n  }"));
        // text before the caret, or already in place: typed as usual
        let mut d = Document::from_text(b"{\n  x ");
        assert!(close_bracket(&mut d, Sel::at(6), b'}').is_none());
        let mut d = Document::from_text(b"{\n");
        assert!(close_bracket(&mut d, Sel::at(2), b'}').is_none());
    }

    #[test]
    fn indentation_is_detected() {
        assert_eq!(detect_indent(b"a\n\tb\n\t\tc\n\td\n"), Some(Indent::Tabs));
        assert_eq!(detect_indent(b"a:\n  b:\n    c: 1\n  d: 2\n"), Some(Indent::Spaces(2)));
        assert_eq!(detect_indent(b"def f():\n    if x:\n        y()\n    return 1\n"), Some(Indent::Spaces(4)));
        // block comments in a tab-indented file
        assert_eq!(detect_indent(b"/*\n * x\n * y\n */\nint f() {\n\treturn 1;\n\tx;\n\ty;\n}\n"), Some(Indent::Tabs));
        assert_eq!(detect_indent(b"no\nindentation\nhere\n"), None);
        assert_eq!(detect_indent(b""), None);
        assert_eq!(detect_indent(b"    starts indented\nx\n"), Some(Indent::Spaces(4)));
    }

    #[test]
    fn overtype_replaces_characters_but_not_line_breaks() {
        let mut d = Document::from_text("abc\r\néx".as_bytes());
        let s = overtype(&mut d, Sel::at(1), b"X");
        let s = overtype(&mut d, s, b"Y");
        assert_eq!(d.read(0, d.len()), b"aXY\r\n\xC3\xA9x");
        // at the end of the line it adds instead
        let s = overtype(&mut d, s, b"Z");
        assert_eq!(d.read(0, d.len()), b"aXYZ\r\n\xC3\xA9x");
        assert_eq!(s, Sel::at(4));
        // a whole character goes, and a selection is replaced as usual
        let s = overtype(&mut d, Sel::at(6), b"e");
        assert_eq!(d.read(0, d.len()), b"aXYZ\r\nex");
        assert_eq!(s, Sel::at(7));
        overtype(&mut d, Sel::new(0, 2), b"Q");
        assert_eq!(d.read(0, d.len()), b"QYZ\r\nex");
        // a lone CR is a character like any other; CRLF is a line break
        let mut d = Document::from_text(b"a\rb\r\nc");
        let s = overtype(&mut d, Sel::at(1), b"X");
        assert_eq!(d.read(0, d.len()), b"aXb\r\nc");
        overtype(&mut d, Sel::at(s.caret + 1), b"Y");
        assert_eq!(d.read(0, d.len()), b"aXbY\r\nc");
        // typed one after another: one undo step
        let mut d = Document::from_text(b"hello");
        let mut s = Sel::at(0);
        for c in [b"j", b"e", b"l"] {
            s = overtype(&mut d, s, c);
        }
        assert_eq!(d.read(0, d.len()), b"jello");
        d.undo();
        assert_eq!(d.read(0, d.len()), b"hello");
    }

    #[test]
    fn brackets_pair_up() {
        let d = Document::from_text(b"f(a[1], {b: (2)}) x");
        // before the caret first, then after it
        assert_eq!(matching_bracket(&d, 2), Some((1, 16)));
        assert_eq!(matching_bracket(&d, 1), Some((1, 16)));
        assert_eq!(matching_bracket(&d, 17), Some((16, 1)));
        assert_eq!(matching_bracket(&d, 16), Some((15, 8)));
        assert_eq!(matching_bracket(&d, 9), Some((8, 15)));
        assert_eq!(matching_bracket(&d, 18), None);
        // unmatched: nothing
        let d = Document::from_text(b"((x)");
        assert_eq!(matching_bracket(&d, 0), None);
        assert_eq!(matching_bracket(&d, 1), Some((1, 3)));
        assert_eq!(matching_bracket(&d, 2), Some((1, 3)));
    }

    #[test]
    fn words_and_characters_are_counted_in_pieces() {
        let text = "Hello, wörld!\r\n  two\u{a0}words\u{3000}三 ".as_bytes();
        let all = count(text);
        assert_eq!(all, Counts { chars: bytecount::num_chars(text) as u64, words: 5 });
        // cut anywhere, even inside a character: the same
        for cut in 0..=text.len() {
            let mut c = Counter::default();
            c.feed(&text[..cut]);
            c.feed(&text[cut..]);
            assert_eq!(c.finish(), all, "cut at {cut}");
        }
        assert_eq!(count(b""), Counts::default());
        assert_eq!(count(b"   \n\t"), Counts { chars: 5, words: 0 });
        // broken UTF-8 counts like the selection count does
        let bad = b"a\xC3 b\x80c";
        assert_eq!(count(bad), Counts { chars: bytecount::num_chars(bad) as u64, words: 2 });
    }

    #[test]
    fn huge_lines_are_not_copied() {
        let mut big = vec![b'a'; (COPY_MAX + 10) as usize];
        big.extend_from_slice(b"\nb");
        let mut d = Document::from_text(&big);
        assert_eq!(duplicate(&mut d, Sel::at(5)), Err(TOO_MUCH_TEXT));
        assert_eq!(move_lines(&mut d, Sel::at(5), true), Err(TOO_MUCH_TEXT));
        let end = d.len();
        assert_eq!(move_lines(&mut d, Sel::at(end), false), Err(TOO_MUCH_TEXT));
        assert_eq!(d.len(), big.len() as u64);
        let s = duplicate(&mut d, Sel::at(end)).unwrap();
        assert!(d.read(0, d.len()).ends_with(b"\nb\r\nb"));
        assert_eq!(s, Sel::at(d.len()));
    }

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
