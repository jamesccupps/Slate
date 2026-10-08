//! A document: the piece table plus undo/redo, the file it belongs to, its encoding and line endings.
//!
//! Edits are grouped into undo steps between `begin` and `end`. Consecutive typing (or backspacing) at the caret
//! joins the previous step, so undo removes a run of typing at once; `seal` ends such a run.
//!
//! A big file is usable before its newline index is finished ("pending"): text, scrolling and search work right
//! away; line numbers come in as the index grows, and editing waits until it is complete (about a second for
//! hundreds of MB).

use std::path::PathBuf;
use std::sync::Arc;
use std::time::SystemTime;

use super::buffer::{Buffer, Piece, Snapshot};
use super::source::Source;
use super::text::{self, CharClass, Encoding, Eol, char_class};

/// A selection: `caret` is where the cursor is, `anchor` the other end (equal when nothing is selected).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Sel {
    pub anchor: u64,
    pub caret: u64,
}

impl Sel {
    pub fn at(pos: u64) -> Sel {
        Sel { anchor: pos, caret: pos }
    }
    pub fn new(anchor: u64, caret: u64) -> Sel {
        Sel { anchor, caret }
    }
    pub fn start(&self) -> u64 {
        self.anchor.min(self.caret)
    }
    pub fn end(&self) -> u64 {
        self.anchor.max(self.caret)
    }
    pub fn is_empty(&self) -> bool {
        self.anchor == self.caret
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum EditKind {
    Typing,
    Backspace,
    DeleteForward,
    Other,
}

#[derive(Clone, Debug)]
enum Op {
    Insert { at: u64, pieces: Vec<Piece> },
    Delete { at: u64, pieces: Vec<Piece> },
}

fn plen(p: &[Piece]) -> u64 {
    p.iter().map(|x| x.len).sum()
}

fn append_pieces(dst: &mut Vec<Piece>, src: Vec<Piece>) {
    for p in src {
        if let Some(last) = dst.last_mut() {
            if last.src == p.src && last.start + last.len == p.start {
                last.len += p.len;
                last.nl += p.nl;
                continue;
            }
        }
        dst.push(p);
    }
}

struct Step {
    ops: Vec<Op>,
    before: Sel,
    after: Sel,
    kind: EditKind,
    /// State id after this step, and before it.
    state: u64,
    prev: u64,
}

/// One change to the text, for views that keep positions (scroll position, search results) up to date.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Change {
    pub at: u64,
    pub del: u64,
    pub ins: u64,
}

impl Change {
    /// Where `pos` ends up after this change (positions inside deleted text move to its start).
    pub fn map(&self, pos: u64) -> u64 {
        if pos <= self.at {
            pos
        } else if pos >= self.at + self.del {
            pos - self.del + self.ins
        } else {
            self.at
        }
    }
}

static VERSIONS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

fn next_version() -> u64 {
    VERSIONS.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}

/// What the file looked like on disk when we last read or wrote it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DiskInfo {
    pub len: u64,
    pub modified: SystemTime,
}

pub struct Document {
    buf: Buffer,
    /// While the main source is still being indexed: that source and where the document starts in it (after a BOM).
    pending: Option<(Arc<Source>, u64)>,
    undo: Vec<Step>,
    redo: Vec<Step>,
    cur: Option<Step>,
    cur_reopened: bool,
    sealed: bool,
    state: u64,
    next_state: u64,
    saved: Option<u64>,
    /// Changes on every edit. Unique across all documents of the process, so a cache keyed on it can't confuse
    /// a reloaded document with the old one.
    pub version: u64,
    changes: Vec<Change>,
    pub path: Option<PathBuf>,
    pub encoding: Encoding,
    /// The file has a byte order mark (UTF-16 files may or may not).
    pub bom: bool,
    pub eol: Eol,
    pub disk: Option<DiskInfo>,
}

impl Default for Document {
    fn default() -> Self {
        Self::new()
    }
}

impl Document {
    /// An empty, unsaved document.
    pub fn new() -> Document {
        Document::from_buffer(Buffer::new())
    }

    pub fn from_buffer(buf: Buffer) -> Document {
        Document {
            buf,
            pending: None,
            undo: Vec::new(),
            redo: Vec::new(),
            cur: None,
            cur_reopened: false,
            sealed: false,
            state: 0,
            next_state: 1,
            saved: Some(0),
            version: next_version(),
            changes: Vec::new(),
            path: None,
            encoding: Encoding::Utf8,
            bom: false,
            eol: Eol::Crlf,
            disk: None,
        }
    }

    pub fn from_text(text: &[u8]) -> Document {
        let nl = bytecount::count(text, b'\n') as u64;
        Document::from_buffer(Buffer::from_source(Arc::new(Source::from_vec(text.to_vec())), nl))
    }

    /// A document over `[base, src.len())` of a file source whose index is still being built.
    pub fn new_pending(src: Arc<Source>, base: u64) -> Document {
        let mut buf = Buffer::new();
        let len = src.len().saturating_sub(base);
        let id = buf.push_source(src.clone());
        if len > 0 {
            buf.insert_pieces(0, &[Piece { src: id, start: base, len, nl: 0 }]);
        }
        let mut d = Document::from_buffer(buf);
        if src.index_complete() {
            d.buf.recount();
        } else {
            d.pending = Some((src, base));
        }
        d
    }

    pub fn buffer(&self) -> &Buffer {
        &self.buf
    }

    // ---- state ----

    pub fn is_ready(&self) -> bool {
        self.pending.is_none()
    }

    /// (bytes indexed, total) while the index is being built.
    pub fn index_progress(&self) -> Option<(u64, u64)> {
        self.pending.as_ref().map(|(s, _)| (s.indexed_bytes(), s.len()))
    }

    /// Call when the indexer reports progress; returns true when the document just became ready.
    pub fn poll_index(&mut self) -> bool {
        let done = matches!(&self.pending, Some((s, _)) if s.index_complete());
        if done {
            // The text is unchanged, so `version` stays (caches keyed on it remain valid).
            self.pending = None;
            self.buf.recount();
        }
        done
    }

    pub fn pending_source(&self) -> Option<Arc<Source>> {
        self.pending.as_ref().map(|(s, _)| s.clone())
    }

    /// Why the index of a pending document couldn't be finished (see `Source::build_index`). It then stays
    /// pending (readable, not editable) until it is opened again.
    pub fn index_error(&self) -> Option<String> {
        self.pending.as_ref().and_then(|(s, _)| s.index_error())
    }

    /// The files (not memory) the current text is read from: a big file, or the file it was last saved to.
    pub fn file_sources(&self) -> Vec<Arc<Source>> {
        self.buf.file_sources()
    }

    pub fn is_dirty(&self) -> bool {
        self.saved != Some(self.state)
    }
    pub fn mark_saved(&mut self) {
        self.saved = Some(self.state);
        self.sealed = true;
    }
    /// Marks the state as saved as of `state` (from `state_id`), e.g. when a save finishes after more edits.
    pub fn mark_saved_at(&mut self, state: u64) {
        self.saved = Some(state);
        self.sealed = true;
    }
    pub fn mark_dirty(&mut self) {
        self.saved = None;
    }
    pub fn state_id(&self) -> u64 {
        self.state
    }
    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }
    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    pub fn take_changes(&mut self) -> Vec<Change> {
        std::mem::take(&mut self.changes)
    }

    /// Changes not taken yet (in order).
    pub fn pending_changes(&self) -> &[Change] {
        &self.changes
    }

    // ---- reading ----

    pub fn len(&self) -> u64 {
        self.buf.len()
    }
    pub fn is_empty(&self) -> bool {
        self.buf.is_empty()
    }
    pub fn read(&self, a: u64, b: u64) -> Vec<u8> {
        self.buf.read(a, b)
    }
    pub fn read_into(&self, a: u64, b: u64, out: &mut Vec<u8>) {
        self.buf.read_into(a, b, out)
    }
    pub fn chunks(&self, a: u64, b: u64, f: &mut dyn FnMut(&[u8]) -> bool) -> bool {
        self.buf.chunks(a, b, f)
    }
    pub fn byte_at(&self, pos: u64) -> Option<u8> {
        self.buf.byte_at(pos)
    }
    pub fn snapshot(&mut self) -> Snapshot {
        self.buf.snapshot()
    }
    pub fn read_errors(&self) -> u64 {
        self.buf.read_errors()
    }

    // ---- lines (work while pending, with less information) ----

    pub fn next_newline(&self, off: u64) -> Option<u64> {
        match &self.pending {
            Some((s, base)) => s.find_nl_fwd(off + base, s.len()).map(|p| p - base),
            None => self.buf.next_newline(off),
        }
    }

    pub fn prev_newline(&self, off: u64) -> Option<u64> {
        match &self.pending {
            Some((s, base)) => s.find_nl_back(*base, off + base).map(|p| p - base),
            None => self.buf.prev_newline(off),
        }
    }

    /// 0-based line of `off`; None while that part of a big file isn't indexed yet.
    pub fn line_of(&self, off: u64) -> Option<u64> {
        match &self.pending {
            Some((s, base)) => {
                if off + base <= s.indexed_bytes() {
                    Some(s.count_nl(*base, off + base))
                } else {
                    None
                }
            }
            None => Some(self.buf.line_of(off)),
        }
    }

    /// Number of lines; None while pending.
    pub fn line_count(&self) -> Option<u64> {
        match &self.pending {
            Some(_) => None,
            None => Some(self.buf.line_count()),
        }
    }

    /// Start offset of a 0-based line; None while pending.
    pub fn line_start(&self, line: u64) -> Option<u64> {
        match &self.pending {
            Some(_) => None,
            None => Some(self.buf.line_start(line)),
        }
    }

    pub fn line_start_of(&self, off: u64) -> u64 {
        self.prev_newline(off).map_or(0, |p| p + 1)
    }

    /// End of the text of the line containing `off` (before `\r\n` / `\n`).
    pub fn line_end_of(&self, off: u64) -> u64 {
        match self.next_newline(off) {
            Some(p) if p > 0 && self.byte_at(p - 1) == Some(b'\r') => p - 1,
            Some(p) => p,
            None => self.len(),
        }
    }

    // ---- characters and words ----

    /// The position after the character at `pos` (`\r\n` counts as one).
    pub fn next_char(&self, pos: u64) -> u64 {
        if pos >= self.len() {
            return self.len();
        }
        let b = self.read(pos, pos + 4);
        if b.starts_with(b"\r\n") {
            return pos + 2;
        }
        pos + text::char_len_at(&b).max(1) as u64
    }

    /// The position before the character that ends at `pos`.
    pub fn prev_char(&self, pos: u64) -> u64 {
        if pos == 0 {
            return 0;
        }
        let pos = pos.min(self.len());
        let s = pos.saturating_sub(4);
        let b = self.read(s, pos);
        if b.ends_with(b"\r\n") {
            return pos - 2;
        }
        pos - text::decode_char_before(&b).1.max(1) as u64
    }

    fn char_at(&self, pos: u64) -> Option<char> {
        if pos >= self.len() {
            return None;
        }
        let b = self.read(pos, pos + 4);
        Some(text::decode_char(&b).0)
    }

    fn char_before(&self, pos: u64) -> Option<char> {
        if pos == 0 {
            return None;
        }
        let b = self.read(pos.saturating_sub(4), pos);
        Some(text::decode_char_before(&b).0)
    }

    /// Ctrl+Right: to the end of the next word (or to the next line from the end of a line).
    pub fn word_right(&self, pos: u64) -> u64 {
        const LIMIT: u64 = 1 << 16;
        let start = pos;
        let mut pos = pos;
        if matches!(self.char_at(pos), Some('\r' | '\n')) {
            return self.next_char(pos);
        }
        while let Some(c) = self.char_at(pos) {
            if char_class(c) != CharClass::Space || pos - start > LIMIT {
                break;
            }
            pos = self.next_char(pos);
        }
        let Some(c) = self.char_at(pos) else { return pos };
        let class = char_class(c);
        if class == CharClass::Newline {
            return pos;
        }
        while let Some(c) = self.char_at(pos) {
            if char_class(c) != class || pos - start > LIMIT {
                break;
            }
            pos = self.next_char(pos);
        }
        pos
    }

    /// Ctrl+Left: to the start of the previous word (or to the previous line from the start of a line).
    pub fn word_left(&self, pos: u64) -> u64 {
        const LIMIT: u64 = 1 << 16;
        let start = pos;
        let mut pos = pos;
        if matches!(self.char_before(pos), Some('\r' | '\n')) {
            return self.prev_char(pos);
        }
        while let Some(c) = self.char_before(pos) {
            if char_class(c) != CharClass::Space || start - pos > LIMIT {
                break;
            }
            pos = self.prev_char(pos);
        }
        let Some(c) = self.char_before(pos) else { return pos };
        let class = char_class(c);
        if class == CharClass::Newline {
            return pos;
        }
        while let Some(c) = self.char_before(pos) {
            if char_class(c) != class || start - pos > LIMIT {
                break;
            }
            pos = self.prev_char(pos);
        }
        pos
    }

    /// The word (or run of spaces / punctuation) at `pos`, for double-click.
    pub fn word_at(&self, pos: u64) -> (u64, u64) {
        const LIMIT: u64 = 1 << 16;
        let class = match self.char_at(pos) {
            Some(c) if char_class(c) != CharClass::Newline => char_class(c),
            _ => match self.char_before(pos) {
                Some(c) if char_class(c) != CharClass::Newline => char_class(c),
                _ => return (pos, pos),
            },
        };
        let mut a = pos;
        while let Some(c) = self.char_before(a) {
            if char_class(c) != class || pos - a > LIMIT {
                break;
            }
            a = self.prev_char(a);
        }
        let mut b = pos;
        while let Some(c) = self.char_at(b) {
            if char_class(c) != class || b - pos > LIMIT {
                break;
            }
            b = self.next_char(b);
        }
        (a, b)
    }

    // ---- editing ----

    /// Ends the current typing run, so the next edit starts a new undo step.
    pub fn seal(&mut self) {
        self.sealed = true;
    }

    /// Starts an edit. Typing / backspacing right where the previous step of the same kind left the caret
    /// continues that step.
    pub fn begin(&mut self, kind: EditKind, before: Sel) {
        if let Some(open) = &self.cur {
            // An edit was interrupted (a panic between begin and end): keep what it did as its own step.
            let after = open.after;
            self.end(after);
        }
        if kind != EditKind::Other && !self.sealed && before.is_empty() && self.redo.is_empty() {
            if let Some(top) = self.undo.last() {
                if top.kind == kind && top.after == before && top.state == self.state {
                    self.cur = self.undo.pop();
                    self.cur_reopened = true;
                    return;
                }
            }
        }
        self.cur = Some(Step { ops: Vec::new(), before, after: before, kind, state: 0, prev: self.state });
        self.cur_reopened = false;
    }

    pub fn insert(&mut self, at: u64, text: &[u8]) {
        if text.is_empty() {
            return;
        }
        let pieces = self.buf.insert(at, text);
        self.changes.push(Change { at, del: 0, ins: text.len() as u64 });
        self.record(Op::Insert { at, pieces });
    }

    pub fn delete(&mut self, a: u64, b: u64) {
        let b = b.min(self.len());
        if a >= b {
            return;
        }
        let pieces = self.buf.delete(a, b);
        self.changes.push(Change { at: a, del: b - a, ins: 0 });
        self.record(Op::Delete { at: a, pieces });
    }

    /// Replaces the whole text with `src` (big transforms: formatting, replace all, conversions).
    pub fn replace_all_with(&mut self, src: Arc<Source>, nl: u64) {
        let len = src.len();
        let id = self.buf.push_source(src);
        let new = if len > 0 { vec![Piece { src: id, start: 0, len, nl }] } else { Vec::new() };
        let old_len = self.len();
        let old = self.buf.replace_all(&new);
        self.changes.push(Change { at: 0, del: old_len, ins: len });
        self.record(Op::Delete { at: 0, pieces: old });
        self.record(Op::Insert { at: 0, pieces: new });
    }

    fn record(&mut self, op: Op) {
        let step = self.cur.as_mut().expect("edit outside begin/end");
        match (step.ops.last_mut(), op) {
            (Some(Op::Insert { at, pieces }), Op::Insert { at: a2, pieces: p2 }) if *at + plen(pieces) == a2 => {
                append_pieces(pieces, p2);
            }
            (Some(Op::Delete { at, pieces }), Op::Delete { at: a2, pieces: p2 })
                if a2 + plen(&p2) == *at && step.kind == EditKind::Backspace =>
            {
                let mut joined = p2;
                append_pieces(&mut joined, std::mem::take(pieces));
                *pieces = joined;
                *at = a2;
            }
            (Some(Op::Delete { at, pieces }), Op::Delete { at: a2, pieces: p2 })
                if a2 == *at && step.kind == EditKind::DeleteForward =>
            {
                append_pieces(pieces, p2);
            }
            (_, op) => step.ops.push(op),
        }
    }

    pub fn end(&mut self, after: Sel) {
        let mut step = self.cur.take().expect("end() without begin()");
        if step.ops.is_empty() {
            if self.cur_reopened {
                self.undo.push(step);
            }
            return;
        }
        step.after = after;
        step.state = self.next_state;
        self.next_state += 1;
        self.state = step.state;
        self.undo.push(step);
        self.redo.clear();
        self.sealed = false;
        self.version = next_version();
    }

    /// Undoes the last step; returns the selection to restore.
    pub fn undo(&mut self) -> Option<Sel> {
        let step = self.undo.pop()?;
        for op in step.ops.iter().rev() {
            match op {
                Op::Insert { at, pieces } => {
                    let n = plen(pieces);
                    self.buf.delete(*at, at + n);
                    self.changes.push(Change { at: *at, del: n, ins: 0 });
                }
                Op::Delete { at, pieces } => {
                    self.buf.insert_pieces(*at, pieces);
                    self.changes.push(Change { at: *at, del: 0, ins: plen(pieces) });
                }
            }
        }
        self.state = step.prev;
        let sel = step.before;
        self.redo.push(step);
        self.sealed = true;
        self.version = next_version();
        Some(sel)
    }

    pub fn redo(&mut self) -> Option<Sel> {
        let step = self.redo.pop()?;
        for op in &step.ops {
            match op {
                Op::Insert { at, pieces } => {
                    self.buf.insert_pieces(*at, pieces);
                    self.changes.push(Change { at: *at, del: 0, ins: plen(pieces) });
                }
                Op::Delete { at, pieces } => {
                    let n = plen(pieces);
                    self.buf.delete(*at, at + n);
                    self.changes.push(Change { at: *at, del: n, ins: 0 });
                }
            }
        }
        self.state = step.state;
        let sel = step.after;
        self.undo.push(step);
        self.sealed = true;
        self.version = next_version();
        Some(sel)
    }

    /// Swaps the current content for `src` without an undo step (after saving, the file on disk holds exactly
    /// the current text). Undo history stays valid: it keeps its own references.
    pub fn rebase_on(&mut self, src: Arc<Source>, start: u64, nl: u64) {
        let len = src.len() - start;
        debug_assert_eq!(len, self.len());
        let id = self.buf.push_source(src);
        let new = if len > 0 { vec![Piece { src: id, start, len, nl }] } else { Vec::new() };
        self.buf.replace_all(&new);
        // Same text, same offsets: `version` stays so caches remain valid.
        self.release_unused();
    }

    /// Lets go of sources only the old text used (e.g. the previous version of a big file after a save).
    pub fn release_unused(&mut self) {
        let mut ids = Vec::new();
        for step in self.undo.iter().chain(self.redo.iter()) {
            for op in &step.ops {
                let (Op::Insert { pieces, .. } | Op::Delete { pieces, .. }) = op;
                ids.extend(pieces.iter().map(|p| p.src));
            }
        }
        if let Some(step) = &self.cur {
            for op in &step.ops {
                let (Op::Insert { pieces, .. } | Op::Delete { pieces, .. }) = op;
                ids.extend(pieces.iter().map(|p| p.src));
            }
        }
        ids.sort_unstable();
        ids.dedup();
        self.buf.release_unused(&ids);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(d: &Document) -> String {
        String::from_utf8(d.read(0, d.len())).unwrap()
    }

    fn typ(d: &mut Document, sel: &mut Sel, s: &str) {
        for ch in s.chars() {
            let mut b = [0u8; 4];
            let t = ch.encode_utf8(&mut b).as_bytes().to_vec();
            if ch == ' ' {
                d.seal();
            }
            d.begin(EditKind::Typing, *sel);
            d.insert(sel.caret, &t);
            *sel = Sel::at(sel.caret + t.len() as u64);
            d.end(*sel);
        }
    }

    #[test]
    fn typing_groups_by_word_and_undoes() {
        let mut d = Document::from_text(b"start\n");
        let mut sel = Sel::at(6);
        typ(&mut d, &mut sel, "hello world");
        assert_eq!(text(&d), "start\nhello world");
        assert!(d.is_dirty());
        assert_eq!(d.undo(), Some(Sel::at(11)));
        assert_eq!(text(&d), "start\nhello");
        assert_eq!(d.undo(), Some(Sel::at(6)));
        assert_eq!(text(&d), "start\n");
        assert!(!d.is_dirty());
        d.redo();
        d.redo();
        assert_eq!(text(&d), "start\nhello world");
        d.mark_saved();
        assert!(!d.is_dirty());
        d.undo();
        assert!(d.is_dirty());
        d.redo();
        assert!(!d.is_dirty());
    }

    #[test]
    fn backspace_and_delete_runs_merge() {
        let mut d = Document::from_text(b"abcdefgh");
        let mut sel = Sel::at(6);
        for _ in 0..3 {
            d.begin(EditKind::Backspace, sel);
            d.delete(sel.caret - 1, sel.caret);
            sel = Sel::at(sel.caret - 1);
            d.end(sel);
        }
        assert_eq!(text(&d), "abcgh");
        d.begin(EditKind::DeleteForward, sel);
        d.delete(3, 4);
        d.end(sel);
        d.begin(EditKind::DeleteForward, sel);
        d.delete(3, 4);
        d.end(sel);
        assert_eq!(text(&d), "abc");
        d.undo();
        assert_eq!(text(&d), "abcgh");
        d.undo();
        assert_eq!(text(&d), "abcdefgh");
        assert!(!d.can_undo());
    }

    #[test]
    fn moving_the_caret_starts_a_new_step() {
        let mut d = Document::from_text(b"");
        let mut sel = Sel::at(0);
        typ(&mut d, &mut sel, "ab");
        sel = Sel::at(0);
        typ(&mut d, &mut sel, "X");
        assert_eq!(text(&d), "Xab");
        d.undo();
        assert_eq!(text(&d), "ab");
    }

    #[test]
    fn replace_all_is_one_step() {
        let mut d = Document::from_text(b"one two");
        d.begin(EditKind::Other, Sel::at(0));
        d.replace_all_with(Arc::new(Source::from_vec(b"ONE\nTWO".to_vec())), 1);
        d.end(Sel::at(0));
        assert_eq!(text(&d), "ONE\nTWO");
        assert_eq!(d.line_count(), Some(2));
        d.undo();
        assert_eq!(text(&d), "one two");
        d.redo();
        assert_eq!(text(&d), "ONE\nTWO");
    }

    #[test]
    fn words_and_chars() {
        let d = Document::from_text("foo.bar  baz\r\nnext é".as_bytes());
        assert_eq!(d.word_right(0), 3);
        assert_eq!(d.word_right(3), 4);
        assert_eq!(d.word_right(4), 7);
        assert_eq!(d.word_right(7), 12);
        assert_eq!(d.word_right(12), 14);
        assert_eq!(d.word_left(14), 12);
        assert_eq!(d.word_left(12), 9);
        assert_eq!(d.word_at(5), (4, 7));
        assert_eq!(d.next_char(12), 14);
        assert_eq!(d.prev_char(14), 12);
        let end = d.len();
        assert_eq!(d.prev_char(end), end - 2);
        assert_eq!(d.line_end_of(0), 12);
    }

    #[test]
    fn change_mapping() {
        let c = Change { at: 10, del: 5, ins: 2 };
        assert_eq!(c.map(3), 3);
        assert_eq!(c.map(10), 10);
        assert_eq!(c.map(12), 10);
        assert_eq!(c.map(15), 12);
        assert_eq!(c.map(20), 17);
    }
}
