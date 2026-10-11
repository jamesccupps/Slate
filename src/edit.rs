//! Editing a document, the same on every platform: the operations (typing over a selection, Backspace, Enter with
//! the indentation, comments, indenting, moving and duplicating lines, brackets, counts) work on the document and
//! return the new selection; the caller reveals the caret. Also the display segments a view cuts the text into
//! (`segment_in`: a whole line, or pieces of a long one cut on an 8 KiB grid) and the lexer states at checkpoints
//! that color what spans lines (`HlIndex`).

use std::collections::BTreeMap;
use std::fs::File;
use std::io::{self, BufWriter, Write};
use std::path::PathBuf;
use std::sync::Arc;

use crate::core::buffer::Snapshot;
use crate::core::document::{Document, EditKind, Sel};
use crate::core::io::MEM_LIMIT;
use crate::core::job::Ctx;
use crate::core::lines::LineOp;
use crate::core::source::{IndexBuilder, Source, create_temp_file};
use crate::core::text::{self, is_continuation, utf8_len};
use crate::highlight::{self, CommentStyle, Lang, State as HlState};

pub const SEG: u64 = 8192;
pub const HALF: u64 = SEG / 2;
/// How far before a grid point a long line may be cut at a space or comma instead.
pub const CUT_BACK: u64 = 256;
/// Documents up to this size get exact coloring of what spans lines (block comments, multi-line strings, tags...).
pub const HL_EXACT_MAX: u64 = 32 << 20;
/// How often lexer states are kept along the document.
pub const HL_CHECK: u64 = 16 << 10;
/// Line-based operations (indent, move lines...) refuse selections covering more lines than this.
pub const MAX_LINE_OPS: u64 = 200_000;
/// Duplicating and moving lines copy them: they refuse more text than this (a 300 MB line would otherwise be
/// copied into memory, and running out of it ends the program).
pub const COPY_MAX: u64 = 16 << 20;
/// Indenting rewrites the lines as one replacement up to this much text (bigger: line by line, in place).
pub const INDENT_AT_ONCE_MAX: u64 = 64 << 20;
pub const TOO_MANY_LINES: &str = "Select fewer lines for that (up to 200,000).";
const _: () = assert!(MAX_LINE_OPS == 200_000);
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

/// Lexer states at checkpoints through the document (and at recent segment starts, always worked out from the
/// checkpoint before them), so coloring knows what a segment starts inside. An edit drops what comes after it; it
/// is worked out again when needed.
#[derive(Default)]
pub struct HlIndex {
    lang: Option<Lang>,
    pub len: u64,
    /// How many of the document's pending changes are already applied.
    pub seen: usize,
    checkpoints: Vec<(u64, HlState)>,
    memo: BTreeMap<u64, HlState>,
    buf: Vec<u8>,
}

impl HlIndex {
    pub fn reset(&mut self) {
        self.lang = None;
        self.seen = 0;
        self.checkpoints.clear();
        self.memo.clear();
    }

    /// The text changed at `at`: states after it aren't known any more.
    pub fn edited(&mut self, at: u64) {
        let k = self.checkpoints.partition_point(|c| c.0 <= at);
        self.checkpoints.truncate(k);
        let _ = self.memo.split_off(&(at + 1));
    }

    pub fn state_at(&mut self, doc: &Document, lang: Lang, off: u64) -> HlState {
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

/// Output for a background transform: memory for small results, a self-deleting temp file for big ones. A result
/// that turns out bigger than it was thought to be (Format can make much more text than it's given) moves from
/// memory to a temp file once it passes `MEM_LIMIT`; one bigger than its limit (`limited`) is refused.
pub struct Sink {
    out: SinkOut,
    written: u64,
    max: u64,
}

enum SinkOut {
    Mem(Vec<u8>),
    File(BufWriter<File>, PathBuf),
}

/// What a limited `Sink` says when the result would be bigger than its limit.
pub const TOO_BIG: &str = "the result would be far bigger than the file (absurdly deep nesting?)";

/// The most a transform of `len` bytes may make (Format makes a few times as much; 64 times is absurd nesting, and
/// the disk would fill up next).
pub fn transform_max(len: u64) -> u64 {
    len.saturating_mul(64).saturating_add(64 << 20)
}

impl Sink {
    pub fn new(len_hint: u64) -> io::Result<Sink> {
        Sink::limited(len_hint, u64::MAX)
    }

    /// A sink that refuses more than `max` bytes (an error saying `TOO_BIG`).
    pub fn limited(len_hint: u64, max: u64) -> io::Result<Sink> {
        let out = if len_hint <= MEM_LIMIT {
            SinkOut::Mem(Vec::with_capacity(len_hint as usize))
        } else {
            let (f, p) = create_temp_file()?;
            SinkOut::File(BufWriter::with_capacity(1 << 20, f), p)
        };
        Ok(Sink { out, written: 0, max })
    }

    pub fn finish(self, idx: IndexBuilder) -> io::Result<(Arc<Source>, u64)> {
        let nl = idx.newlines();
        match self.out {
            SinkOut::Mem(v) => Ok((Arc::new(Source::from_vec(v)), nl)),
            SinkOut::File(w, p) => {
                let f = w.into_inner().map_err(|e| e.into_error())?;
                let len = f.metadata()?.len();
                Ok((Arc::new(Source::from_file(f, len, p, true, Some(idx.finish()))), nl))
            }
        }
    }
}

impl Write for Sink {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.write_all(buf)?;
        Ok(buf.len())
    }
    fn write_all(&mut self, buf: &[u8]) -> io::Result<()> {
        self.written += buf.len() as u64;
        if self.written > self.max {
            return Err(io::Error::other(TOO_BIG));
        }
        if let SinkOut::Mem(v) = &mut self.out {
            if (v.len() + buf.len()) as u64 > MEM_LIMIT {
                // (bigger than thought: on to a temp file, with what's there so far)
                let (f, p) = create_temp_file()?;
                let mut w = BufWriter::with_capacity(1 << 20, f);
                w.write_all(v)?;
                self.out = SinkOut::File(w, p);
            }
        }
        match &mut self.out {
            SinkOut::Mem(v) => {
                v.extend_from_slice(buf);
                Ok(())
            }
            SinkOut::File(w, _) => w.write_all(buf),
        }
    }
    fn flush(&mut self) -> io::Result<()> {
        match &mut self.out {
            SinkOut::Mem(_) => Ok(()),
            SinkOut::File(w, _) => w.flush(),
        }
    }
}

/// Text to insert for a newline in `doc`.
pub fn eol(doc: &Document) -> &'static [u8] {
    doc.eol.as_bytes()
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
    let lines = selected_lines(doc, sel).ok_or(TOO_MANY_LINES)?;
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
pub fn text_start(line: &[u8], numbered: bool) -> usize {
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

// ---- the same on both windows: rules that depend on the file, and what commands say ----

/// What Toggle comment uses at the line holding `at`: the language's comments, but `;` in a .ini, .inf or .reg file,
/// and `//` in Inno Setup's [Code] section (Pascal, where a `;` would only be an empty statement).
pub fn comment_style(doc: &Document, lang: Lang, at: u64) -> Option<CommentStyle> {
    let mut style = lang.comment()?;
    let ext = doc.path.as_ref().and_then(|p| p.extension()).map(|e| e.to_ascii_lowercase());
    if lang == Lang::Ini && ext.as_ref().is_some_and(|e| e == "ini" || e == "inf" || e == "reg") {
        style = CommentStyle::Line(";");
    }
    if lang == Lang::InnoSetup {
        let a = doc.line_start_of(at);
        let before = doc.read(a.saturating_sub(4 << 20), a);
        if highlight::inno_code_line(&before, &doc.read(a, doc.line_end_of(a))) {
            style = CommentStyle::Line("//");
        }
    }
    Some(style)
}

/// Changes every line break in `snap` to CRLF (`to_crlf`) or LF, writing the text to `w` and its line breaks to
/// `idx`. Returns how many changed.
pub fn convert_eol(snap: &Snapshot, to_crlf: bool, w: &mut dyn Write, idx: &mut IndexBuilder, ctx: &Ctx) -> io::Result<u64> {
    let mut out = Vec::with_capacity(1 << 20);
    let mut changed = 0u64;
    let mut prev_cr = false;
    let mut pending_cr = false;
    let mut pos = 0u64;
    let mut err = None;
    while pos < snap.len() {
        if ctx.cancelled() {
            return Err(io::Error::new(io::ErrorKind::Interrupted, "cancelled"));
        }
        let end = (pos + (8 << 20)).min(snap.len());
        snap.chunks(pos, end, &mut |c| {
            for &b in c {
                if to_crlf {
                    if b == b'\n' && !prev_cr {
                        out.push(b'\r');
                        changed += 1;
                    }
                    out.push(b);
                    prev_cr = b == b'\r';
                } else {
                    if pending_cr {
                        pending_cr = false;
                        if b == b'\n' {
                            changed += 1;
                        } else {
                            out.push(b'\r');
                        }
                    }
                    if b == b'\r' {
                        pending_cr = true;
                    } else {
                        out.push(b);
                    }
                }
            }
            if out.len() >= 1 << 20 {
                idx.push(&out);
                if let Err(e) = w.write_all(&out) {
                    err = Some(e);
                    return false;
                }
                out.clear();
            }
            true
        });
        if let Some(e) = err.take() {
            return Err(e);
        }
        pos = end;
        ctx.set(pos);
    }
    if pending_cr {
        out.push(b'\r');
    }
    idx.push(&out);
    w.write_all(&out)?;
    Ok(changed)
}

/// `n` with thousands separators: 4,851,721.
pub fn group(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// "1 line", "4,851,721 lines".
pub fn plural(n: u64, one: &str, many: &str) -> String {
    if n == 1 { format!("1 {one}") } else { format!("{} {many}", group(n)) }
}

/// What a line tool did, for the status bar.
pub fn lines_done(op: LineOp, n: u64) -> String {
    match op {
        LineOp::SortAsc | LineOp::SortDesc => format!("Sorted {}", plural(n, "line", "lines")),
        LineOp::Dedupe => format!("Removed {}", plural(n, "duplicate line", "duplicate lines")),
        LineOp::RemoveBlank => format!("Removed {}", plural(n, "blank line", "blank lines")),
        LineOp::TrimTrailing => format!("Trimmed spaces from {}", plural(n, "line", "lines")),
    }
}

/// What a line tool says when it finds nothing to do.
pub fn nothing_to_clean(op: LineOp) -> &'static str {
    match op {
        LineOp::SortAsc | LineOp::SortDesc => "The lines are already in that order",
        LineOp::Dedupe => "No duplicate lines",
        LineOp::RemoveBlank => "No blank lines",
        LineOp::TrimTrailing => "No spaces at line ends",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_result_bigger_than_thought_goes_to_disk_and_an_absurd_one_is_refused() {
        let mut s = Sink::new(10).unwrap();
        let chunk = vec![b'x'; 1 << 20];
        for _ in 0..(MEM_LIMIT >> 20) + 2 {
            s.write_all(&chunk).unwrap();
        }
        assert!(matches!(s.out, SinkOut::File(..)));
        let (src, _) = s.finish(IndexBuilder::new()).unwrap();
        assert_eq!(src.len(), MEM_LIMIT + (2 << 20));
        let mut s = Sink::limited(10, 100).unwrap();
        s.write_all(&[0; 100]).unwrap();
        assert!(s.write_all(&[0]).is_err_and(|e| e.to_string() == TOO_BIG));
        assert!(transform_max(1 << 20) > 32 << 20);
    }

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

    /// A document whose new line breaks are CRLF (the tests' expectations are written for them).
    fn crlf(text: &[u8]) -> Document {
        let mut d = Document::from_text(text);
        d.eol = crate::core::text::Eol::Crlf;
        d
    }

    #[test]
    fn edits() {
        let mut d = crlf(b"{}\n  [1, 2]\n");
        let s = newline(&mut d, Sel::at(1), b"  ");
        assert_eq!(d.read(0, d.len()), b"{\r\n  \r\n}\n  [1, 2]\n");
        assert_eq!(s, Sel::at(5));
        let mut d = crlf(b"a\nb\nc");
        let s = move_lines(&mut d, Sel::at(0), true).unwrap();
        assert_eq!(d.read(0, d.len()), b"b\r\na\nc");
        assert_eq!(s, Sel::at(3));
        let mut d = crlf(b"a\nb\nc");
        delete_lines(&mut d, Sel::at(2)).unwrap();
        assert_eq!(d.read(0, d.len()), b"a\nc");
        let mut d = crlf(b"a\nb\nc");
        delete_lines(&mut d, Sel::at(4)).unwrap();
        assert_eq!(d.read(0, d.len()), b"a\nb");
        let mut d = crlf(b"x\ny");
        let s = indent_lines(&mut d, Sel::new(0, 3), Indent::Tabs, 4, false).unwrap();
        assert_eq!(d.read(0, d.len()), b"\tx\n\ty");
        assert_eq!(s, Sel::new(0, 5));
        indent_lines(&mut d, s, Indent::Tabs, 4, true).unwrap();
        assert_eq!(d.read(0, d.len()), b"x\ny");
        assert_eq!(normalize_eols(b"a\nb\r\nc\rd", b"\r\n"), b"a\r\nb\r\nc\r\nd");
        let mut d = crlf(b"ab");
        let s = duplicate(&mut d, Sel::at(1)).unwrap();
        assert_eq!(d.read(0, d.len()), b"ab\r\nab");
        assert_eq!(s, Sel::at(5));
        // Enter on a line of nothing but indentation takes the indentation along instead of leaving it behind.
        let mut d = crlf(b"  x\n    \ny");
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
        let mut d = crlf(&big);
        assert_eq!(duplicate(&mut d, Sel::at(5)), Err(TOO_MUCH_TEXT));
        assert_eq!(move_lines(&mut d, Sel::at(5), true), Err(TOO_MUCH_TEXT));
        let end = d.len();
        assert_eq!(move_lines(&mut d, Sel::at(end), false), Err(TOO_MUCH_TEXT));
        assert_eq!(d.len(), big.len() as u64);
        let s = duplicate(&mut d, Sel::at(end)).unwrap();
        assert!(d.read(0, d.len()).ends_with(b"\nb\r\nb"));
        assert_eq!(s, Sel::at(d.len()));
    }
}
